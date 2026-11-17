//! Markdown rendering of a page in reading order.
//!
//! Consecutive lines become blocks (paragraphs, list items, headings);
//! tables become Markdown tables. Headings are inferred from type: clearly
//! larger than the body text, or short bold or all-caps lines.

use serde::Serialize;

use crate::model::{BBox, Segment};
use crate::pii::PiiSpan;
use crate::tables::Table;

/// A line at least this much larger than the body text is a heading.
const HEADING_SIZE_RATIO: f32 = 1.15;
/// A largest heading style at least this much larger than body text is level 1.
const H1_RATIO: f32 = 1.6;
/// Headings are at most this long.
const MAX_HEADING_CHARS: usize = 100;
/// A style is a heading style when at least this share of its blocks look
/// like headings (short, capitalised, not sentence-like).
const HEADING_STYLE_SHARE: f32 = 0.6;

/// Words that name a section of a resume or a document. Resumes often set
/// their section headings ("Experience", "Key projects", "Education") in the
/// same type as the body text, so no style sets them apart and they read as
/// one-line paragraphs. A one-line block made only of these words (and at
/// least one noun from the first list) is a heading whatever its style.
const SECTION_NOUNS: &[&str] = &[
    "summary", "profile", "objective", "objectives", "experience", "experiences", "employment", "history",
    "education", "qualification", "qualifications", "skills", "skill", "competencies", "competences", "expertise",
    "projects", "project", "certifications", "certification", "certificates", "achievements", "accomplishments",
    "awards", "honours", "honors", "languages", "interests", "hobbies", "references", "publications", "training",
    "courses", "coursework", "activities", "volunteering", "internships", "internship", "responsibilities",
    "overview", "contact", "declaration", "strengths", "highlights", "details", "tools", "technologies",
    "abstract", "introduction", "background", "conclusion", "conclusions", "results", "discussion", "methods",
    "methodology", "appendix", "contents", "acknowledgements", "acknowledgments", "bibliography",
    // Project write-ups in resumes: "Project Description", "Roles and
    // Responsibilities", "Environment", "Scope".
    "description", "roles", "role", "tasks", "scope", "environment",
];
const SECTION_MODIFIERS: &[&str] = &[
    "professional", "work", "career", "technical", "core", "key", "academic", "personal", "relevant",
    "additional", "educational", "extracurricular", "extra", "curricular", "other", "selected", "soft",
    "employment", "and", "of", "the", "my", "&",
];

/// A section name on its own: one to four words, all from the section
/// vocabulary, with at least one section noun ("Technical skills:").
pub(crate) fn is_section_name(text: &str) -> bool {
    let t = text.trim().trim_end_matches(':').trim().to_lowercase();
    let words: Vec<&str> = t.split(|c: char| !(c.is_alphabetic() || c == '&')).filter(|w| !w.is_empty()).collect();
    (1..=4).contains(&words.len())
        && words.iter().any(|w| SECTION_NOUNS.contains(w))
        && words.iter().all(|w| SECTION_NOUNS.contains(w) || SECTION_MODIFIERS.contains(w))
}

/// "1. ", "2) ", "a. ": a numbered list marker followed by text.
fn number_marker(text: &str) -> Option<u32> {
    let t = text.trim_start();
    let mut chars = t.char_indices();
    let (mut end, mut n) = (0, None);
    for (i, c) in chars.by_ref() {
        if c.is_ascii_digit() && i < 2 {
            n = Some(n.unwrap_or(0) * 10 + c.to_digit(10).unwrap());
            end = i + 1;
        } else {
            break;
        }
    }
    if n.is_none() {
        let c = t.chars().next()?;
        if !c.is_ascii_lowercase() {
            return None;
        }
        n = Some(c as u32 - 'a' as u32 + 1);
        end = 1;
    }
    let rest = &t[end..];
    (rest.starts_with(". ") || rest.starts_with(") ")).then_some(n?).filter(|_| rest[2..].trim().chars().count() >= 2)
}

/// Numbered lines that are list items: their number is followed by the next
/// one within the next few lines (1., 2., 3. ...), so a lone "1. Introduction"
/// section number is not taken for a list.
fn numbered_items(segments: &[Segment]) -> Vec<bool> {
    let nums: Vec<Option<u32>> = segments.iter().map(|s| if s.table.is_some() { None } else { number_marker(&s.text) }).collect();
    let mut out = vec![false; segments.len()];
    for i in 0..segments.len() {
        let Some(n) = nums[i] else { continue };
        if let Some(j) = (i + 1..segments.len().min(i + 7)).find(|&j| nums[j] == Some(n + 1)) {
            out[i] = true;
            out[j] = true;
        }
    }
    out
}

enum Block<'a> {
    Lines(Vec<&'a Segment>),
    Table(usize),
}

/// Right edge of the text column a line belongs to: the widest line on the
/// page sharing its left edge.
fn column_right(segments: &[Segment], s: &Segment) -> f32 {
    segments
        .iter()
        .filter(|o| o.table.is_none() && (o.bbox.x0 - s.bbox.x0).abs() <= 4.0)
        .map(|o| o.bbox.x1)
        .fold(s.bbox.x1, f32::max)
}

/// A wrapped line runs (nearly) to its column's right edge; a short line
/// (contact field, skill category, heading, last line) ends its block.
fn is_full(segments: &[Segment], s: &Segment) -> bool {
    let right = column_right(segments, s);
    let slack = (2.5 * s.size).max(0.12 * (right - s.bbox.x0));
    s.bbox.x1 >= right - slack
}

/// Whether the line before `segments[i]` wrapped into it: the first word of
/// line `i` would not have fitted at the end of the previous line, measured
/// against the local text width (this line and the few lines that follow it
/// at the same indentation), not the whole page.
fn wraps_into(segments: &[Segment], i: usize) -> bool {
    if i == 0 {
        return false;
    }
    let (p, s) = (&segments[i - 1], &segments[i]);
    let mut right = p.bbox.x1.max(s.bbox.x1);
    let mut last = s;
    for o in segments[i + 1..].iter().take(3) {
        let gap = o.bbox.y0 - last.bbox.y1;
        if o.table.is_some() || (o.bbox.x0 - s.bbox.x0).abs() > 4.0 || gap > last.bbox.height() || gap < -1.0 {
            break;
        }
        right = right.max(o.bbox.x1);
        last = o;
    }
    let chars = s.text.chars().count().max(1) as f32;
    let char_w = s.bbox.width() / chars;
    let first_word = s.text.split_whitespace().next().map(|w| w.chars().count()).unwrap_or(0) as f32;
    p.bbox.x1 + (first_word + 1.0) * char_w > right - 1.0
}

/// "Label: value" lines (Email:, Duration:, Tools:) are separate fields,
/// never the continuation of the line above.
fn is_field_line(text: &str) -> bool {
    let t = text.trim_start();
    match t.find(':') {
        Some(pos) if pos > 0 && pos <= 30 => {
            let label = &t[..pos];
            label.chars().next().is_some_and(|c| c.is_uppercase())
                && label.split_whitespace().count() <= 4
                && !label.chars().any(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

/// Gap between wrapped lines of one paragraph, as a share of line height.
const LINE_GAP: f32 = 0.6;
/// Lines of a page set with wide line spacing (1.5, double) are further
/// apart than LINE_GAP: up to this multiple of the page's typical gap
/// between wrapped lines still continues the block...
const TYPICAL_GAP_FACTOR: f32 = 1.25;
/// ...but never a full blank line or more.
const MAX_LINE_GAP: f32 = 1.0;

/// The typical vertical gap between consecutive lines of the same size that
/// share a column, as a share of line height: the page's line spacing.
fn typical_line_gap(segments: &[Segment]) -> f32 {
    let mut gaps: Vec<f32> = segments
        .windows(2)
        .filter(|w| w[0].table.is_none() && w[1].table.is_none() && (w[0].size - w[1].size).abs() < 1.0)
        .filter(|w| w[1].bbox.x1.min(w[0].bbox.x1) - w[1].bbox.x0.max(w[0].bbox.x0) > 0.0)
        .map(|w| (w[1].bbox.y0 - w[0].bbox.y1) / w[0].bbox.height().max(1.0))
        .filter(|g| (-0.5..1.5).contains(g))
        .collect();
    gaps.sort_by(|a, b| a.total_cmp(b));
    gaps.get(gaps.len() / 2).copied().unwrap_or(0.0)
}

pub fn blocks(segments: &[Segment], tables: &[Table], body_size: f32) -> Vec<OutBlock> {
    // Resumes are often set at 1.5 line spacing, where wrapped lines sit
    // further apart than LINE_GAP and every wrapped bullet broke in two.
    // The page's own line spacing sets how far a wrapped line may be; a
    // block still ends where the line above is short (see is_full).
    let max_gap = LINE_GAP.max(TYPICAL_GAP_FACTOR * typical_line_gap(segments)).min(MAX_LINE_GAP);
    // Group reading-order items into blocks.
    let mut blocks: Vec<Block> = Vec::new();
    let numbered = numbered_items(segments);
    // A table is written whole where its first row is read. Lines read
    // between its rows (cell text the table did not take, a heading split
    // out of it) must not start it again: every restart repeated all its rows.
    let mut tables_out = std::collections::HashSet::new();
    for (si, s) in segments.iter().enumerate() {
        if let Some(t) = s.table {
            if tables_out.insert(t) {
                blocks.push(Block::Table(t));
            }
            continue;
        }
        let starts_item = list_marker(&s.text).is_some() || numbered[si];
        // A placeholder ("<!-- image not read ... -->") stands alone.
        let mark = |t: &str| t.starts_with("<!--");
        let joins = match blocks.last() {
            Some(Block::Lines(lines)) if mark(&s.text) || mark(&lines.last().unwrap().text) => false,
            Some(Block::Lines(lines)) => {
                let p = lines.last().unwrap();
                let gap = s.bbox.y0 - p.bbox.y1;
                let h = p.bbox.height().max(1.0);
                let overlap = s.bbox.x1.min(p.bbox.x1) - s.bbox.x0.max(p.bbox.x0);
                // A list item continues only on lines indented past its
                // marker; a line back at the marker's edge starts a new entry.
                let first = lines[0];
                let in_list = list_marker(&first.text).is_some() || number_marker(&first.text).is_some();
                // Inside a list item's hanging indent, a line may differ in
                // weight from the one above (a bold phrase that wraps).
                let same_weight = s.bold == p.bold || (in_list && s.bbox.x0 > first.bbox.x0 + 1.0);
                // A line ending in an open range ("Apr 2020 –") goes on in
                // the next, wherever that starts: a date set to the right
                // whose end wrapped back to the left margin.
                let open_range = p.text.trim_end().ends_with(['–', '—', '-']) && s.text.starts_with(char::is_alphanumeric);
                !starts_item
                    && !(in_list && s.bbox.x0 <= first.bbox.x0 + 1.0)
                    && !is_field_line(&s.text)
                    && (is_full(segments, p) || wraps_into(segments, si) || open_range)
                    && gap > -0.5 * h
                    && gap < max_gap * h
                    && (overlap > 0.0 || open_range)
                    && (s.size - p.size).abs() < 1.0
                    && same_weight
            }
            _ => false,
        };
        match blocks.last_mut() {
            Some(Block::Lines(lines)) if joins => lines.push(s),
            _ => blocks.push(Block::Lines(vec![s])),
        }
    }

    // First pass: describe every text block.
    let mut infos: Vec<Option<BlockInfo>> = Vec::with_capacity(blocks.len());
    let mut prev_last: Option<&Segment> = None;
    let mut prev_heading_like = false;
    for b in &blocks {
        match b {
            Block::Table(_) => {
                prev_last = None;
                prev_heading_like = false;
                infos.push(None);
            }
            Block::Lines(lines) => {
                // A block that continues an unfinished sentence directly above
                // it is never a heading (a bold phrase inside a paragraph).
                // A bold line straight after regular text starts something new
                // (the next job's "Company, 2020 - 2023" after the last bullet
                // of the one above, which seldom ends with a full stop).
                let continues = prev_last.is_some_and(|p| {
                    let first = lines[0];
                    let gap = first.bbox.y0 - p.bbox.y1;
                    let overlap = first.bbox.x1.min(p.bbox.x1) - first.bbox.x0.max(p.bbox.x0);
                    overlap > 0.0
                        && gap < 0.8 * p.bbox.height().max(1.0)
                        && !p.text.trim_end().ends_with(['.', ':', ';', '!', '?'])
                        && !(first.bold && !p.bold)
                        // Nor does a bold line under a bold heading-like line:
                        // a header in several lines (company, dates, title) has
                        // no sentence to carry on.
                        && !(first.bold && p.bold && prev_heading_like)
                });
                prev_last = lines.last().copied();
                let numbered_item = segments.iter().position(|s| std::ptr::eq(s, lines[0])).is_some_and(|k| numbered[k]);
                let info = describe(lines, continues, numbered_item);
                prev_heading_like = info.heading_like && !info.list;
                infos.push(Some(info));
            }
        }
    }

    // Second pass: a style (size, bold, capitals) is a heading style when most
    // of its blocks look like headings. Every block in that style is then a
    // heading, at one level per style, so headings are consistent on the page.
    let mut styles: std::collections::BTreeMap<Style, (usize, usize)> = Default::default();
    for i in infos.iter().flatten() {
        if i.list {
            continue;
        }
        let e = styles.entry(i.style).or_default();
        e.0 += 1;
        e.1 += i.heading_like as usize;
    }
    let mut heading_styles: Vec<Style> = styles
        .iter()
        .filter(|(st, (n, h))| *h > 0 && (*h as f32) >= HEADING_STYLE_SHARE * *n as f32 && st.distinct(body_size))
        .map(|(st, _)| *st)
        .collect();
    // Larger first; at equal size, capitals and then bold rank higher.
    heading_styles.sort_by(|a, b| b.size_x2.cmp(&a.size_x2).then(b.caps.cmp(&a.caps)).then(b.bold.cmp(&a.bold)));
    let base = if heading_styles.first().is_some_and(|s| s.size() >= H1_RATIO * body_size) { 1 } else { 2 };

    let mut out = Vec::new();
    for (b, info) in blocks.iter().zip(&infos) {
        match (b, info) {
            (Block::Table(i), _) => {
                let t = &tables[*i];
                let text = t.rows.iter().map(|r| r.join(" | ")).collect::<Vec<_>>().join("\n");
                out.push(OutBlock::new("table", None, text, t.bbox, Some(t.rows.clone())));
            }
            (Block::Lines(lines), Some(i)) => {
                let bbox = BBox::union_all(lines.iter().map(|l| &l.bbox)).unwrap_or(lines[0].bbox);
                if i.list {
                    out.push(OutBlock::new("list_item", None, i.list_text.clone(), bbox, None));
                } else {
                    match heading_styles.iter().position(|s| *s == i.style) {
                        Some(rank) if i.heading_ok => {
                            let level = (base + rank).min(3) as u8;
                            out.push(OutBlock::new("heading", Some(level), i.text.clone(), bbox, None));
                        }
                        // A section name in body type is still a section heading.
                        _ if i.section_name => out.push(OutBlock::new("heading", Some(2), i.text.clone(), bbox, None)),
                        _ => out.push(OutBlock::new("paragraph", None, i.text.clone(), bbox, None)),
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// One structural block of a page, ready for JSON consumers.
#[derive(Clone, Debug, Serialize)]
pub struct OutBlock {
    /// heading | paragraph | list_item | table
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    pub text: String,
    pub bbox: BBox,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<Vec<String>>>,
    /// True when `text` (or a table cell) contains personal data.
    pub contains_pii: bool,
    /// Personal data found in `text`: type, byte range, matched text and
    /// confidence. Backends can mask by type or drop the block.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pii: Vec<PiiSpan>,
    /// `text` with every PII span replaced by a placeholder ("[EMAIL]"):
    /// safe to send to LLMs and other AI workloads.
    pub text_redacted: String,
}

impl OutBlock {
    pub fn new(kind: &'static str, level: Option<u8>, text: String, bbox: BBox, rows: Option<Vec<Vec<String>>>) -> Self {
        // NER runs on prose only: headings and table cells are mostly
        // titles, skills and tools, where it would add false positives.
        let pii = if matches!(kind, "paragraph" | "list_item") { crate::pii::detect_prose(&text) } else { crate::pii::detect(&text) };
        let mut b = OutBlock { kind, level, text, bbox, rows, contains_pii: false, pii, text_redacted: String::new() };
        b.refresh();
        b
    }

    /// Recompute the derived fields after `pii` changes.
    pub fn refresh(&mut self) {
        self.text_redacted = crate::pii::redact(&self.text, &self.pii);
        self.contains_pii = !self.pii.is_empty();
    }

    /// Strip all raw personal data: text becomes the redacted text, table
    /// cells are redacted, and PII spans keep only their type and position.
    pub fn redact_in_place(&mut self) {
        self.text = self.text_redacted.clone();
        let known = self.pii.clone();
        if let Some(rows) = &mut self.rows {
            for c in rows.iter_mut().flatten() {
                *c = redact_cell(c, &known);
            }
        }
        for s in &mut self.pii {
            s.text = format!("[{}]", s.kind.to_uppercase());
        }
    }
}

/// Redact one table cell: its own pattern matches plus every PII value
/// already known for the table (found by NER or elsewhere in the document).
pub fn redact_cell(cell: &str, known: &[PiiSpan]) -> String {
    let mut spans = crate::pii::detect(cell);
    let lower = cell.to_lowercase();
    if lower.len() == cell.len() {
        for k in known {
            let needle = k.text.trim().to_lowercase();
            if needle.chars().count() < 3 {
                continue;
            }
            let mut from = 0;
            while let Some(pos) = lower[from..].find(&needle) {
                let (start, end) = (from + pos, from + pos + needle.len());
                from = end;
                if cell.is_char_boundary(start) && cell.is_char_boundary(end) && !spans.iter().any(|s| s.start < end && start < s.end) {
                    spans.push(PiiSpan { kind: k.kind, start, end, text: cell[start..end].to_string(), confidence: k.confidence });
                }
            }
        }
    }
    spans.sort_by_key(|s| s.start);
    crate::pii::redact(cell, &spans)
}

/// Markdown for a list of blocks; with `redact`, PII is replaced by
/// placeholders such as "[EMAIL]".
pub fn to_markdown(blocks: &[OutBlock], redact: bool) -> String {
    let text = |b: &OutBlock, t: &str| if redact { crate::pii::redact(t, &b.pii) } else { t.to_string() };
    let mut out = String::new();
    for b in blocks {
        let md = match b.kind {
            "heading" => format!("{} {}", "#".repeat(b.level.unwrap_or(3) as usize), text(b, &b.text)),
            // A numbered item keeps its own number ("2. ..."); others get a dash.
            "list_item" if number_marker(&b.text).is_some() => text(b, &b.text),
            "list_item" => format!("- {}", text(b, &b.text)),
            "table" => match &b.rows {
                Some(rows) if redact => table_md(
                    &rows.iter().map(|r| r.iter().map(|c| redact_cell(c, &b.pii)).collect()).collect::<Vec<Vec<String>>>(),
                ),
                Some(rows) => table_md(rows),
                None => text(b, &b.text),
            },
            _ => text(b, &b.text),
        };
        if !md.is_empty() {
            out.push_str(&md);
            out.push_str("\n\n");
        }
    }
    out.trim_end().to_string()
}

/// Typographic style of a block, used to keep heading levels consistent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Style {
    /// Font size in half points.
    size_x2: i32,
    bold: bool,
    caps: bool,
}

impl Style {
    fn size(&self) -> f32 {
        self.size_x2 as f32 / 2.0
    }

    /// Set apart from body text by size, weight or capitals.
    fn distinct(&self, body_size: f32) -> bool {
        self.size() >= HEADING_SIZE_RATIO * body_size || self.bold || self.caps
    }
}

struct BlockInfo {
    text: String,
    style: Style,
    list: bool,
    list_text: String,
    /// Short, starts with a capital, does not continue a sentence.
    heading_ok: bool,
    /// heading_ok and also not ending like a sentence: a typical heading.
    heading_like: bool,
    /// One line naming a section ("Experience", "Key projects").
    section_name: bool,
}

fn describe(lines: &[&Segment], continues: bool, numbered_item: bool) -> BlockInfo {
    let text = join_lines(lines);
    let (list, list_text) = match list_marker(&text) {
        Some((_, rest)) => (true, rest.to_string()),
        None if numbered_item => (true, text.clone()),
        None => (false, String::new()),
    };
    let size = lines.iter().map(|l| l.size).fold(0.0, f32::max);
    let letters: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();
    let caps = letters.len() >= 3 && letters.iter().all(|c| !c.is_lowercase());
    let bold = lines.iter().all(|l| l.bold);
    let starts_upper = text.chars().find(|c| c.is_alphanumeric()).is_some_and(|c| !c.is_lowercase());
    let chars = text.chars().count();
    let heading_ok = !continues && starts_upper && letters.len() >= 2 && lines.len() <= 3 && chars <= MAX_HEADING_CHARS;
    let heading_like = heading_ok && lines.len() <= 2 && !text.trim_end().ends_with(['.', ',', ';']);
    // A capitalised section name starts a section even straight after an
    // unfinished line: the last bullet of the section above rarely ends with
    // a full stop, which would otherwise read as the sentence carrying on.
    let section_name = (!continues || starts_upper) && !list && lines.len() == 1 && is_section_name(&text);
    BlockInfo {
        style: Style { size_x2: (size * 2.0).round() as i32, bold, caps },
        text,
        list,
        list_text,
        heading_ok,
        heading_like,
        section_name,
    }
}

/// Join wrapped lines; a line ending in a hyphen joins without a space.
fn join_lines(lines: &[&Segment]) -> String {
    let mut out = String::new();
    for l in lines {
        let t = l.text.trim();
        if out.is_empty() {
            out.push_str(t);
        } else if out.ends_with('-') && !out.ends_with(" -") {
            // A word broken at a hyphen; "2020 -" is a range, not a word.
            out.push_str(t);
        } else {
            out.push(' ');
            out.push_str(t);
        }
    }
    out
}

/// A line starting with a list-marker symbol and a space ("• text").
fn list_marker(text: &str) -> Option<(&str, &str)> {
    let t = text.trim_start();
    let first = t.chars().next()?;
    if !crate::lines::is_bullet_char(first) {
        return None;
    }
    let (marker, rest) = t.split_at(first.len_utf8());
    (rest.starts_with(' ') && !rest.trim().is_empty()).then(|| (marker, rest.trim_start()))
}

fn table_md(rows: &[Vec<String>]) -> String {
    let Some(first) = rows.first() else { return String::new() };
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0).max(1);
    let cell = |c: &str| c.replace('|', "\\|").replace('\n', " ");
    let row = |r: &Vec<String>| {
        let mut cells: Vec<String> = r.iter().map(|c| cell(c)).collect();
        cells.resize(cols, String::new());
        format!("| {} |", cells.join(" | "))
    };
    let mut out = vec![row(first), format!("|{}|", vec![" --- "; cols].join("|"))];
    out.extend(rows.iter().skip(1).map(row));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str, y: f32, table: Option<usize>) -> Segment {
        let bbox = BBox { x0: 50.0, y0: y, x1: 400.0, y1: y + 10.0 };
        Segment { text: text.to_string(), bbox, size: 10.0, bold: false, table }
    }

    #[test]
    fn a_table_broken_by_other_lines_is_written_once() {
        let rows = vec![vec!["Cloud".to_string(), "AWS".to_string()], vec!["Data".to_string(), "Kafka".to_string()]];
        let row_boxes = vec![seg("", 100.0, None).bbox, seg("", 130.0, None).bbox];
        let table = Table { bbox: BBox { x0: 50.0, y0: 100.0, x1: 400.0, y1: 140.0 }, rows, row_boxes };
        let segments = [seg("Cloud | AWS", 100.0, Some(0)), seg("Storage", 115.0, None), seg("Data | Kafka", 130.0, Some(0))];
        let blocks = blocks(&segments, &[table], 10.0);
        assert_eq!(blocks.iter().filter(|b| b.kind == "table").count(), 1);
        assert!(blocks.iter().any(|b| b.text.contains("Storage")), "the line between rows is kept");
    }
}
