//! PDF writer: re-typesets a parsed document as a clean A4 PDF.
//!
//! The new file is built from the structured blocks, so nothing from the
//! original carries over except the visible text you choose to keep: no
//! photo, no hidden text, no metadata, no original fonts. With redaction on,
//! every PII span is replaced by a placeholder such as "[EMAIL]" (the name
//! can instead become a label such as "Candidate #1042"). An optional logo
//! (PNG or JPEG) is placed at the top of the first page.
//!
//! Fonts: Noto Sans (Latin, Greek, Cyrillic) and Noto Sans Devanagari are
//! embedded in this binary (SIL Open Font License) and in each output file.
//! PDFium places glyphs without complex-script shaping, so Devanagari
//! conjuncts may not join exactly as in a shaping engine.

use std::path::Path;

use pdfium_render::prelude::*;

use crate::markdown::OutBlock;
use crate::pii;
use crate::DocResult;

const PAGE_W: f32 = 595.0;
const PAGE_H: f32 = 842.0;
const MARGIN: f32 = 50.0;
const LOGO_MAX_W: f32 = 140.0;
const LOGO_MAX_H: f32 = 50.0;
const BODY: f32 = 10.0;
const LEADING: f32 = 1.35;

static NOTO_REGULAR: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/models/fonts/NotoSans-Regular.ttf"));
static NOTO_BOLD: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/models/fonts/NotoSans-Bold.ttf"));
static DEVANAGARI_REGULAR: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/models/fonts/NotoSansDevanagari-Regular.ttf"));
static DEVANAGARI_BOLD: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/models/fonts/NotoSansDevanagari-Bold.ttf"));

pub struct RenderOptions<'p> {
    pub logo: Option<&'p Path>,
    pub redact: bool,
    /// Replaces the person's name when redacting, e.g. "Candidate #1042".
    pub candidate_label: Option<String>,
    /// Optional line under the logo, e.g. "Candidate profile — prepared by Acme".
    pub header_note: Option<String>,
    /// Delete personal details outright instead of showing "[EMAIL]" and
    /// similar placeholders. The name is still replaced (by the label, or
    /// "[PERSON_NAME]"). Labels and separators left empty are tidied away.
    pub remove: bool,
}

pub fn write_pdf(pdfium: &Pdfium, doc: &DocResult, out: &Path, opts: &RenderOptions) -> Result<(), String> {
    let mut pdf = pdfium.create_new_pdf().map_err(|e| e.to_string())?;
    let fonts = load_fonts(&mut pdf)?;
    let mut w = Writer { page: None, y: 0.0, fonts, name_label: opts.candidate_label.clone(), remove: opts.redact && opts.remove };
    w.new_page(&mut pdf)?;

    if let Some(path) = opts.logo {
        let img = image::open(path).map_err(|e| format!("logo {}: {e}", path.display()))?;
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        let scale = (LOGO_MAX_W / iw).min(LOGO_MAX_H / ih);
        let (lw, lh) = (iw * scale, ih * scale);
        let page = w.page.as_mut().unwrap();
        page.objects_mut()
            .create_image_object(
                PdfPoints::new(MARGIN),
                PdfPoints::new(w.y - lh),
                &img,
                Some(PdfPoints::new(lw)),
                Some(PdfPoints::new(lh)),
            )
            .map_err(|e| e.to_string())?;
        w.y -= lh + 14.0;
    }
    if let Some(note) = &opts.header_note {
        w.lines(&mut pdf, note, 8.5, false, 0.0)?;
        w.y -= 8.0;
    }

    for page in &doc.pages {
        for b in &page.blocks {
            w.block(&mut pdf, b, opts.redact)?;
        }
    }
    pdf.save_to_file(out).map_err(|e| e.to_string())
}

#[derive(Clone, Copy)]
pub(crate) struct Fonts {
    regular: PdfFontToken,
    bold: PdfFontToken,
    deva_regular: PdfFontToken,
    deva_bold: PdfFontToken,
}

impl Fonts {
    pub(crate) fn pick(&self, devanagari: bool, bold: bool) -> PdfFontToken {
        match (devanagari, bold) {
            (true, true) => self.deva_bold,
            (true, false) => self.deva_regular,
            (false, true) => self.bold,
            (false, false) => self.regular,
        }
    }
}

/// Embed the Noto fonts in a new document.
pub(crate) fn load_fonts(pdf: &mut PdfDocument) -> Result<Fonts, String> {
    let mut load = |bytes: &[u8]| pdf.fonts_mut().load_true_type_from_bytes(bytes, true).map_err(|e| e.to_string());
    Ok(Fonts {
        regular: load(NOTO_REGULAR)?,
        bold: load(NOTO_BOLD)?,
        deva_regular: load(DEVANAGARI_REGULAR)?,
        deva_bold: load(DEVANAGARI_BOLD)?,
    })
}

struct Writer<'a> {
    page: Option<PdfPage<'a>>,
    y: f32,
    fonts: Fonts,
    name_label: Option<String>,
    remove: bool,
}

impl<'a> Writer<'a> {
    fn new_page(&mut self, pdf: &mut PdfDocument<'a>) -> Result<(), String> {
        let page = pdf.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).map_err(|e| e.to_string())?;
        self.page = Some(page);
        self.y = PAGE_H - MARGIN;
        Ok(())
    }

    fn block(&mut self, pdf: &mut PdfDocument<'a>, b: &OutBlock, redact: bool) -> Result<(), String> {
        let label = self.name_label.clone();
        let remove = self.remove;
        let finish = move |r: String| {
            let r = if remove { drop_placeholders(&r) } else { r };
            match &label {
                Some(l) => r.replace("[PERSON_NAME]", l),
                None => r,
            }
        };
        let clean = |t: &str, spans: &[pii::PiiSpan]| if redact { finish(pii::redact(t, spans)) } else { t.to_string() };
        let text = clean(&b.text, &b.pii);
        if b.kind != "table" && text.trim().is_empty() {
            return Ok(());
        }
        match b.kind {
            "heading" => {
                let size = match b.level.unwrap_or(3) {
                    1 => 17.0,
                    2 => 13.0,
                    _ => 11.0,
                };
                self.y -= size * 0.6;
                self.lines(pdf, &text, size, true, 0.0)?;
                self.y -= 2.0;
            }
            "list_item" => {
                self.ensure(pdf, BODY * LEADING)?;
                self.text_at(pdf, MARGIN + 4.0, "•", BODY, false)?;
                self.lines(pdf, &text, BODY, false, 14.0)?;
                self.y -= 2.0;
            }
            "table" => {
                for row in b.rows.as_deref().unwrap_or_default() {
                    let cells: Vec<String> =
                        row.iter().map(|c| if redact { finish(crate::markdown::redact_cell(c, &b.pii)) } else { c.clone() }).collect();
                    self.lines(pdf, &cells.join("   |   "), BODY - 0.5, false, 0.0)?;
                }
                self.y -= 4.0;
            }
            _ => {
                self.lines(pdf, &text, BODY, false, 0.0)?;
                self.y -= 4.0;
            }
        }
        Ok(())
    }

    /// Word-wrapped text at the current position.
    fn lines(&mut self, pdf: &mut PdfDocument<'a>, text: &str, size: f32, bold: bool, indent: f32) -> Result<(), String> {
        let width = PAGE_W - 2.0 * MARGIN - indent;
        for line in wrap(&normalise_symbols(text), size, bold, width) {
            self.ensure(pdf, size * LEADING)?;
            self.text_at(pdf, MARGIN + indent, &line, size, bold)?;
            self.y -= size * LEADING;
        }
        Ok(())
    }

    /// Draw one line, switching font per script run (Devanagari runs use the
    /// Devanagari font, everything else Noto Sans).
    fn text_at(&mut self, _pdf: &mut PdfDocument<'a>, x: f32, text: &str, size: f32, bold: bool) -> Result<(), String> {
        let baseline = self.y - size;
        let mut x = x;
        for (deva, run) in script_runs(text) {
            let font = self.fonts.pick(deva, bold);
            let obj = self
                .page
                .as_mut()
                .unwrap()
                .objects_mut()
                .create_text_object(PdfPoints::new(x), PdfPoints::new(baseline), &run, font, PdfPoints::new(size))
                .map_err(|e| e.to_string())?;
            // Advance by the width PDFium actually drew (exact for every
            // script), plus trailing spaces, which the ink box leaves out.
            let drawn = obj.bounds().map(|q| q.to_rect().width().value).unwrap_or_else(|_| text_width(&run, size, bold));
            let trailing = run.chars().rev().take_while(|c| *c == ' ').count() as f32;
            x += drawn + trailing * 0.26 * size;
        }
        Ok(())
    }

    fn ensure(&mut self, pdf: &mut PdfDocument<'a>, needed: f32) -> Result<(), String> {
        if self.y - needed < MARGIN {
            self.new_page(pdf)?;
        }
        Ok(())
    }
}

fn is_devanagari(c: char) -> bool {
    ('\u{0900}'..='\u{097F}').contains(&c) || ('\u{A8E0}'..='\u{A8FF}').contains(&c)
}

/// Split a line into runs of Devanagari and other text; spaces and
/// punctuation stay with the run they follow.
pub(crate) fn script_runs(text: &str) -> Vec<(bool, String)> {
    let mut runs: Vec<(bool, String)> = Vec::new();
    for c in text.chars() {
        let deva = if c.is_alphanumeric() { is_devanagari(c) } else { runs.last().map(|r| r.0).unwrap_or(false) };
        match runs.last_mut() {
            Some((d, s)) if *d == deva => s.push(c),
            _ => runs.push((deva, c.to_string())),
        }
    }
    runs
}

/// Greedy word wrap using Helvetica advance widths.
pub(crate) fn wrap(text: &str, size: f32, bold: bool, width: f32) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if text_width(&candidate, size, bold) <= width || line.is_empty() {
                line = candidate;
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

pub(crate) fn text_width(s: &str, size: f32, bold: bool) -> f32 {
    s.chars().map(|c| char_width(c, bold)).sum::<f32>() * size / 1000.0 * 1.04
}

/// Approximate advance widths (per 1000 em) for printable ASCII, from
/// Helvetica; Noto Sans is about 4% wider, which `text_width` adds. Others 600.
fn char_width(c: char, bold: bool) -> f32 {
    const REGULAR: [u16; 95] = [
        278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556,
        556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667,
        556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556,
        556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722,
        500, 500, 500, 334, 260, 334, 584,
    ];
    let w = match c as u32 {
        32..=126 => REGULAR[(c as u32 - 32) as usize] as f32,
        _ => 600.0,
    };
    if bold { w * 1.06 } else { w }
}

/// Removes every placeholder except the name's, then tidies what is left:
/// a part of a line ("Email: [EMAIL]", "[PHONE]") that held only a removed
/// value, with its label, goes together with its separator ("|", "•", ",").
pub(crate) fn drop_placeholders(text: &str) -> String {
    use std::sync::OnceLock;
    static TAG: OnceLock<regex::Regex> = OnceLock::new();
    static SEP: OnceLock<regex::Regex> = OnceLock::new();
    let tag = TAG.get_or_init(|| regex::Regex::new(r"\[(?:[A-Z][A-Z_]*)\]").unwrap());
    let sep = SEP.get_or_init(|| regex::Regex::new(r"\s*[|•·,;]\s*|\s{3,}").unwrap());
    let is_tag = |m: &str| m != "[PERSON_NAME]";
    if !tag.find_iter(text).any(|m| is_tag(m.as_str())) {
        return text.to_string();
    }
    let mut out = String::new();
    let mut pending_sep: Option<&str> = None;
    let mut last = 0;
    let mut pieces: Vec<(&str, &str)> = Vec::new(); // (part, separator after it)
    for m in sep.find_iter(text) {
        pieces.push((&text[last..m.start()], m.as_str()));
        last = m.end();
    }
    pieces.push((&text[last..], ""));
    for (part, after) in pieces {
        let had_tag = tag.find_iter(part).any(|m| is_tag(m.as_str()));
        let kept = if had_tag {
            let stripped = tag.replace_all(part, |c: &regex::Captures| {
                if is_tag(&c[0]) { String::new() } else { c[0].to_string() }
            });
            let rest = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
            let rest = rest.trim_matches(|c: char| c == '-' || c == '–' || c == ':' || c.is_whitespace());
            // What is left is only a label such as "Email" or "Mobile No".
            let label_only = part.contains(':') && rest.split_whitespace().count() <= 3 && !rest.contains(':');
            if rest.is_empty() || label_only { None } else { Some(rest.to_string()) }
        } else {
            Some(part.to_string())
        };
        match kept {
            Some(k) if !k.trim().is_empty() || out.is_empty() && pending_sep.is_none() => {
                if !out.is_empty() {
                    out.push_str(pending_sep.unwrap_or(" "));
                }
                out.push_str(&k);
                pending_sep = Some(after);
            }
            Some(_) => {}
            None => {}
        }
    }
    out.trim().trim_matches(|c: char| c == '|' || c == ',' || c == ';' || c == '•').trim().to_string()
}

/// Bullet-like symbols the embedded fonts may lack become a plain bullet.
/// Any other character neither embedded font can draw (emoji, icon-font and
/// private-use characters) is dropped rather than shown as an empty box.
pub(crate) fn normalise_symbols(s: &str) -> String {
    let out: String = s
        .chars()
        .filter_map(|c| match c {
            '▪' | '◦' | '➢' | '✓' | '✔' | '►' | '■' | '●' | '❖' | '◆' | '➤' => Some('•'),
            c if c.is_whitespace() || drawable(c) => Some(c),
            _ => None,
        })
        .collect();
    // An icon dropped from the start of a line leaves its spacing behind.
    if out.len() < s.len() { out.trim_start().to_string() } else { out }
}

fn drawable(c: char) -> bool {
    static COVERAGE: std::sync::OnceLock<Vec<(u32, u32)>> = std::sync::OnceLock::new();
    let ranges = COVERAGE.get_or_init(|| {
        let mut r = cmap_ranges(NOTO_REGULAR);
        r.extend(cmap_ranges(DEVANAGARI_REGULAR));
        r.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(r.len());
        for (lo, hi) in r {
            match merged.last_mut() {
                Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
                _ => merged.push((lo, hi)),
            }
        }
        merged
    });
    let c = c as u32;
    let i = ranges.partition_point(|&(lo, _)| lo <= c);
    i > 0 && c <= ranges[i - 1].1
}

/// Code-point ranges a TrueType font maps to real glyphs (cmap formats 4 and
/// 12). An unreadable table yields nothing, so nothing is claimed drawable.
fn cmap_ranges(font: &[u8]) -> Vec<(u32, u32)> {
    let u16_at = |o: usize| font.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize);
    let u32_at = |o: usize| font.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let mut out = Vec::new();
    let Some(tables) = u16_at(4) else { return out };
    let Some(cmap) = (0..tables)
        .map(|i| 12 + 16 * i)
        .find(|&rec| font.get(rec..rec + 4) == Some(b"cmap"))
        .and_then(|rec| u32_at(rec + 8))
        .map(|o| o as usize)
    else {
        return out;
    };
    let subtables = u16_at(cmap + 2).unwrap_or(0);
    for i in 0..subtables {
        let rec = cmap + 4 + 8 * i;
        let (Some(platform), Some(offset)) = (u16_at(rec), u32_at(rec + 4)) else { continue };
        if platform != 0 && platform != 3 {
            continue;
        }
        let sub = cmap + offset as usize;
        match u16_at(sub) {
            Some(4) => {
                let segs = u16_at(sub + 6).unwrap_or(0) / 2;
                let ends = sub + 14;
                let starts = ends + 2 * segs + 2;
                for s in 0..segs {
                    let (Some(end), Some(start)) = (u16_at(ends + 2 * s), u16_at(starts + 2 * s)) else { break };
                    if start != 0xFFFF {
                        out.push((start as u32, end as u32));
                    }
                }
            }
            Some(12) => {
                let groups = u32_at(sub + 12).unwrap_or(0) as usize;
                for g in 0..groups {
                    let at = sub + 16 + 12 * g;
                    let (Some(lo), Some(hi)) = (u32_at(at), u32_at(at + 4)) else { break };
                    out.push((lo, hi));
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn removes_placeholders_and_their_labels() {
        use super::drop_placeholders as d;
        assert_eq!(d("[EMAIL] | [PHONE]"), "");
        assert_eq!(d("Email: [EMAIL] | Phone: [PHONE] | Bengaluru"), "Bengaluru");
        assert_eq!(d("Bengaluru, [POSTAL_CODE]"), "Bengaluru");
        assert_eq!(d("[PERSON_NAME] | [EMAIL]"), "[PERSON_NAME]");
        assert_eq!(d("Reach me at [EMAIL] for details"), "Reach me at for details");
        assert_eq!(d("Senior Engineer, Platform"), "Senior Engineer, Platform");
    }

    #[test]
    fn drops_what_the_fonts_cannot_draw() {
        assert_eq!(super::normalise_symbols("\u{1F4CB} Role: Developer"), "Role: Developer");
        assert_eq!(super::normalise_symbols("\u{2709} a@b.c | \u{F0B7} x"), "a@b.c |  x");
        assert_eq!(super::normalise_symbols("Café – naïve ₹ नमस्ते"), "Café – naïve ₹ नमस्ते");
        assert_eq!(super::normalise_symbols("➢ item"), "• item");
    }
}
