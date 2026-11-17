//! Named-entity recognition for PII in running text (feature `ner`).
//!
//! Model: DistilBERT-NER (dslim/distilbert-NER, Apache-2.0), quantised to
//! int8 (about 63MB) and embedded in the binary with its tokenizer; loaded
//! once per process and run on CPU with rten (pure Rust).
//!
//! Only people and places are reported (organisations are not personal
//! data). To keep skill and tool names ("Jenkins", "Ansible") from being
//! flagged, callers run this on prose blocks, people need at least two
//! words, and places need high confidence.

use rten::{Model, NodeId};
use rten_tensor::prelude::*;
use rten_tensor::NdTensor;
use rten_text::Tokenizer;

static MODEL: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/models/ner/model.int8.onnx"));
static TOKENIZER: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/models/ner/tokenizer.json"));

/// Model label ids: O, B-PER, I-PER, B-ORG, I-ORG, B-LOC, I-LOC, B-MISC, I-MISC.
const PER: usize = 1;
const LOC: usize = 5;
/// Longest text piece sent to the model at once (characters); the model
/// takes at most 512 tokens.
const MAX_PIECE_CHARS: usize = 1200;
const MIN_PERSON_SCORE: f32 = 0.80;
const MIN_PLACE_SCORE: f32 = 0.90;

#[derive(Clone)]
pub struct Entity {
    /// "person_name" or "location".
    pub kind: &'static str,
    pub start: usize,
    pub end: usize,
    pub score: f32,
}

struct Ner {
    model: Model,
    tokenizer: Tokenizer,
    input_ids: NodeId,
    attention_mask: NodeId,
    logits: NodeId,
}

thread_local! {
    // The tokenizer is not Sync, so each thread keeps its own instance
    // (loaded once, on first use). Workers are single-threaded per document.
    static NER: Option<Ner> = load().map_err(|e| eprintln!("NER model unavailable: {e}")).ok();
}

fn load() -> Result<Ner, String> {
    let model = Model::load_static_slice(MODEL).map_err(|e| e.to_string())?;
    let tokenizer = Tokenizer::from_json(TOKENIZER).map_err(|e| e.to_string())?;
    let input_ids = model.node_id("input_ids").map_err(|e| e.to_string())?;
    let attention_mask = model.node_id("attention_mask").map_err(|e| e.to_string())?;
    let logits = model.node_id("logits").map_err(|e| e.to_string())?;
    Ok(Ner { model, tokenizer, input_ids, attention_mask, logits })
}

/// People and places in `text` (byte offsets into `text`).
/// Whether NER should run: PAGELENS_NER=on|off, or set by the CLI for
/// redaction and rendering. Off by default: pattern detectors still run.
pub fn enabled() -> bool {
    use std::sync::atomic::Ordering;
    match std::env::var("PAGELENS_NER").as_deref() {
        Ok("on") | Ok("1") => true,
        Ok("off") | Ok("0") => false,
        _ => FORCED.load(Ordering::Relaxed),
    }
}

static FORCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turn NER on for this process (used by --redact, --ner and render).
pub fn enable() {
    FORCED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Cheap pre-check: a person's name needs two adjacent capitalised words
/// (Title Case, not ALL CAPS acronyms). Most bullets and skills lines have
/// none, so the model is skipped for them.
fn may_contain_name(text: &str) -> bool {
    let words: Vec<&str> = text.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).collect();
    let title = |w: &str| {
        let mut cs = w.chars();
        cs.next().is_some_and(|c| c.is_uppercase()) && cs.clone().count() >= 1 && cs.all(|c| c.is_lowercase())
    };
    words.windows(2).any(|p| title(p[0]) && title(p[1]))
}

pub fn entities(text: &str) -> Vec<Entity> {
    if !enabled() || !may_contain_name(text) {
        return Vec::new();
    }
    // The model sees an ASCII-safe copy (one byte per character) so token
    // offsets always land on character boundaries and the tokenizer's
    // normaliser cannot fail on unusual characters; `map` gives the
    // original byte offset of every byte of the copy.
    let (safe, map) = ascii_safe(text);
    let to_original = |start: usize, end: usize| -> (usize, usize) {
        let s = map[start.min(map.len() - 1)];
        let last = map[(end.max(1) - 1).min(map.len() - 1)];
        let e = last + text[last..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        (s, e)
    };
    NER.with(|ner| {
        let Some(ner) = ner else { return Vec::new() };
        let mut out = Vec::new();
        for (offset, piece) in pieces(&safe) {
            // A tokenizer or model failure skips this piece; it never takes
            // the whole document down.
            let found = quiet_catch(|| run(ner, piece));
            if let Ok(Ok(found)) = found {
                for e in found {
                    let (start, end) = to_original(e.start + offset, e.end + offset);
                    out.push(Entity { start, end, ..e });
                }
            }
        }
        out
    })
}

/// Run `f`, turning a panic into an error without printing it.
fn quiet_catch<T>(f: impl FnOnce() -> T) -> std::thread::Result<T> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(hook);
    r
}

/// One ASCII byte per character: ASCII kept, other letters become a plain
/// letter of the same case, everything else a space.
fn ascii_safe(text: &str) -> (String, Vec<usize>) {
    let mut safe = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        let a = if c.is_ascii() {
            c
        } else if c.is_alphabetic() {
            if c.is_uppercase() { 'E' } else { 'e' }
        } else {
            ' '
        };
        safe.push(a);
        map.push(i);
    }
    (safe, map)
}

/// Split long text at line or sentence ends into model-sized pieces.
fn pieces(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + MAX_PIECE_CHARS).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end < text.len() {
            if let Some(cut) = text[start..end].rfind(['\n', '.', ';']) {
                if cut > 0 {
                    end = start + cut + 1;
                }
            }
        }
        out.push((start, &text[start..end]));
        start = end;
    }
    out
}

fn run(ner: &Ner, text: &str) -> Result<Vec<Entity>, String> {
    let encoded = ner.tokenizer.encode(text, None).map_err(|e| e.to_string())?;
    let ids: Vec<i32> = encoded.token_ids().iter().map(|&t| t as i32).collect();
    let offsets = encoded.token_offsets().to_vec();
    let n = ids.len();
    if n < 3 {
        return Ok(Vec::new());
    }
    let input = NdTensor::from_data([1, n], ids);
    let mask = NdTensor::from_data([1, n], vec![1i32; n]);
    let [logits] = ner
        .model
        .run_n(vec![(ner.input_ids, input.into()), (ner.attention_mask, mask.into())], [ner.logits], None)
        .map_err(|e| e.to_string())?;
    let logits: NdTensor<f32, 3> = logits.try_into().map_err(|e: rten::TryFromValueError| e.to_string())?;

    // Per token: best label and its probability (softmax).
    let labels: Vec<(usize, f32)> = (0..n)
        .map(|i| {
            let row: Vec<f32> = (0..logits.size(2)).map(|j| logits[[0, i, j]]).collect();
            let max = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let exp: Vec<f32> = row.iter().map(|v| (v - max).exp()).collect();
            let sum: f32 = exp.iter().sum();
            let (best, p) = exp.iter().enumerate().fold((0, 0.0), |a, (j, &e)| if e > a.1 { (j, e) } else { a });
            (best, p / sum)
        })
        .collect();

    // Byte span of token i: from its offset to the next token's offset,
    // without trailing whitespace. Skip [CLS] (0) and [SEP] (last).
    let offsets: Vec<usize> = offsets.into_iter().map(|o| o.min(text.len())).collect();
    let token_end = |i: usize| -> usize {
        let next = if i + 1 < n - 1 { offsets[i + 1] } else { text.len() };
        let end = next.min(text.len()).max(offsets[i]);
        offsets[i] + text[offsets[i]..end].trim_end().len()
    };
    let mut out = Vec::new();
    let mut i = 1;
    while i < n - 1 {
        let (label, p) = labels[i];
        let entity = match label {
            l if l == PER || l == PER + 1 => Some((PER, "person_name")),
            l if l == LOC || l == LOC + 1 => Some((LOC, "location")),
            _ => None,
        };
        let Some((base, kind)) = entity else {
            i += 1;
            continue;
        };
        // Extend over following tokens of the same entity (I- tags and
        // word pieces of the same word).
        let start = offsets[i];
        let mut scores = vec![p];
        let mut j = i + 1;
        while j < n - 1 && (labels[j].0 == base + 1 || (labels[j].0 == base && is_word_piece(text, offsets[j]))) {
            scores.push(labels[j].1);
            j += 1;
        }
        let end = token_end(j - 1);
        let score = scores.iter().sum::<f32>() / scores.len() as f32;
        out.push(Entity { kind, start, end, score });
        i = j;
    }
    // Trailing punctuation ("Trello,") is not part of the entity.
    for e in &mut out {
        let span = text.get(e.start..e.end).unwrap_or("");
        e.end = e.start + span.trim_end_matches(|c: char| !c.is_alphanumeric()).len();
    }
    let kept: Vec<Entity> = out
        .into_iter()
        .filter(|e| {
            let span = text.get(e.start..e.end).unwrap_or("");
            let words = span.split_whitespace().count();
            let capitalised = span.chars().next().is_some_and(|c| c.is_uppercase());
            // A span cut inside a word ("Practitioner fro[m]") is a model
            // misreading, never a name or place.
            let whole = !text[..e.start].chars().next_back().is_some_and(|c| c.is_alphanumeric())
                && !text[e.end..].chars().next().is_some_and(|c| c.is_alphanumeric());
            whole
                && capitalised
                && match e.kind {
                    "person_name" => e.score >= MIN_PERSON_SCORE && words >= 2 && name_shaped(span),
                    _ => e.score >= MIN_PLACE_SCORE,
                }
        })
        .collect();
    Ok(drop_list_places(text, kept))
}

fn drop_list_places(text: &str, kept: Vec<Entity>) -> Vec<Entity> {
    // In a list such as "Jira, Rally, Trello, Miro" the model tends to call
    // product names places. The list is the run of short comma-separated
    // items around the entity (across line breaks, inside prose too). A place
    // in a run of four or more stands only when most of the run is places.
    let mut items: Vec<(usize, usize)> = Vec::new();
    let mut from = 0;
    for (i, c) in text.char_indices().filter(|(_, c)| matches!(c, ',' | '|' | ';' | '•' | '\n')) {
        items.push((from, i));
        from = i + c.len_utf8();
    }
    items.push((from, text.len()));
    items.retain(|&(a, b)| !text[a..b].trim().is_empty());
    let short = |&(a, b): &(usize, usize)| {
        let words = text[a..b].split_whitespace().count();
        (1..=4).contains(&words)
    };
    let in_list = |e: &Entity| -> bool {
        let Some(at) = items.iter().position(|&(a, b)| a <= e.start && e.end <= b) else { return false };
        if !short(&items[at]) {
            return false;
        }
        let mut lo = at;
        while lo > 0 && short(&items[lo - 1]) {
            lo -= 1;
        }
        let mut hi = at;
        while hi + 1 < items.len() && short(&items[hi + 1]) {
            hi += 1;
        }
        let run = hi - lo + 1;
        let (a, b) = (items[lo].0, items[hi].1);
        let places = kept.iter().filter(|o| o.kind == e.kind && o.start >= a && o.end <= b).count();
        run >= 4 && 2 * places < run
    };
    kept.iter().filter(|e| e.kind == "person_name" || !in_list(e)).cloned().collect()
}

/// Words a person's name is made of: each starts with a capital (or is a
/// lowercase particle such as "de" or "van"), with no digits, commas or
/// mid-word capitals beyond a prefix ("McDonald", but not "JiraAlign").
fn name_shaped(span: &str) -> bool {
    const PARTICLES: [&str; 16] =
        ["de", "da", "di", "du", "dos", "das", "del", "der", "van", "von", "bin", "binti", "al", "el", "la", "le"];
    !span.contains(',')
        && span.split_whitespace().all(|w| {
            let w = w.trim_end_matches('.');
            let chars: Vec<char> = w.chars().collect();
            let starts_upper = chars.first().is_some_and(|c| c.is_uppercase());
            // A capital inside a word only after a name prefix: "McDonald",
            // "MacKenzie", "DeSouza", "LaRue" are names; "GitHub", "DevOps" are not.
            let camel = chars.windows(2).enumerate().any(|(i, p)| {
                let prefix: String = chars[..=i].iter().collect::<String>().to_lowercase();
                p[0].is_lowercase()
                    && p[1].is_uppercase()
                    && !matches!(prefix.as_str(), "mc" | "mac" | "de" | "di" | "da" | "la" | "le" | "van" | "von" | "fitz")
            });
            (starts_upper || PARTICLES.contains(&w)) && !camel && chars.iter().all(|c| c.is_alphabetic() || matches!(c, '\'' | '-' | '.'))
        })
}

/// A token that continues a word (no whitespace before it).
fn is_word_piece(text: &str, offset: usize) -> bool {
    let offset = offset.min(text.len());
    offset > 0 && text.as_bytes()[offset - 1].is_ascii() && !(text.as_bytes()[offset - 1] as char).is_whitespace()
}

#[cfg(test)]
mod tests {
    #[test]
    fn product_lists_are_not_places() {
        let text = "Proficient in Jira, Confluence, Rally, Smartsheet,\nMiro and Teams";
        let e = |w: &str| {
            let start = text.find(w).unwrap();
            super::Entity { kind: "location", start, end: start + w.len(), score: 0.95 }
        };
        let kept = super::drop_list_places(text, vec![e("Rally"), e("Miro")]);
        assert!(kept.is_empty());
        let text = "Offices in Pune, Delhi, Chennai and Kochi";
        let e = |w: &str| {
            let start = text.find(w).unwrap();
            super::Entity { kind: "location", start, end: start + w.len(), score: 0.95 }
        };
        assert_eq!(super::drop_list_places(text, vec![e("Pune"), e("Delhi"), e("Chennai and Kochi")]).len(), 3);
    }

    #[test]
    fn name_shapes() {
        for ok in ["Asha Example", "Ludwig van Beethoven", "Mary-Jane O'Neil", "Ronald McDonald", "J. R. Smith"] {
            assert!(super::name_shaped(ok), "{ok}");
        }
        for bad in ["Kanban Practitioner from", "JiraAlign, Confluence", "Python3 Expert"] {
            assert!(!super::name_shaped(bad), "{bad}");
        }
    }
}
