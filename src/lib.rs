//! pagelens: deterministic, geometry-based PDF layout extraction.

pub mod backend;
pub mod layout;
pub mod lines;
pub mod markdown;
pub mod model;
#[cfg(feature = "ner")]
pub mod ner;
pub mod ocr;
pub mod pii;
pub mod render;
pub mod structured;
pub mod check;
pub mod profile;
pub mod brand;
pub mod tables;

use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

use model::{BBox, PageContent, Segment};
use tables::Table;

/// Images covering less than this share of the page count as figures.
/// Larger images are usually page backgrounds, whose text is body text.
const MAX_FIGURE_SHARE: f32 = 0.6;
/// Images smaller than this share of the page are icons or logos.
const MIN_FIGURE_SHARE: f32 = 0.01;
/// An image carrying at least this many text lines, averaging at least this
/// many characters, is a background panel rather than a figure.
const MIN_BACKGROUND_LINES: usize = 3;
const MIN_BACKGROUND_LINE_CHARS: f32 = 15.0;
/// Text this much larger than the body text over an image is a banner.
const BANNER_SIZE_RATIO: f32 = 1.4;

/// A raster figure and the text drawn on top of it (labels, axes).
#[derive(Clone, Debug, Serialize)]
pub struct Figure {
    pub bbox: BBox,
    pub text: String,
}

#[derive(Serialize)]
pub struct PageResult {
    pub width: f32,
    pub height: f32,
    /// Segments in reading order. Each table row appears as one segment with
    /// cells separated by " | ".
    pub segments: Vec<Segment>,
    pub tables: Vec<Table>,
    pub figures: Vec<Figure>,
    /// The page as Markdown: headings, lists, paragraphs and tables.
    pub markdown: String,
    /// The page as typed blocks (heading, paragraph, list_item, table) with
    /// positions and PII labels: the structured form of `markdown`.
    pub blocks: Vec<markdown::OutBlock>,
    /// Text in the file that a reader cannot see (hidden keywords, white on
    /// white). Kept out of `segments` and `markdown`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hidden_text: String,
    /// Present when the page had no usable text layer: why, and whether an
    /// OCR engine read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr: Option<ocr::OcrStatus>,
    /// Images that look like text but were not read (see `needs_review`).
    #[serde(skip)]
    pub unread_images: Vec<BBox>,
}

/// The candidate's main details, each with how sure the parser is.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Person {
    pub name: Option<Detail>,
    pub email: Option<Detail>,
    pub phone: Option<Detail>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Detail {
    pub text: String,
    /// 0..1. Names: 0.95 when the email address or a "Name:" label backs the
    /// name, 0.75 when it is the top line and nothing contradicts it, 0.5
    /// when the email address points elsewhere.
    pub confidence: f32,
    /// What the value is based on, in words.
    pub basis: String,
}

/// Something the parser could not read or is not sure of, and what the
/// caller should do about it. The parser's job ends here: reading a scan or
/// an image, or confirming a doubtful name, is for an LLM or a person.
#[derive(Clone, Debug, Serialize)]
pub struct ReviewItem {
    /// scanned_page | image | name | email | phone
    pub kind: &'static str,
    /// 1-based page, for pages and images.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    /// Where on the page (points, origin top left), for images.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<BBox>,
    pub reason: String,
    /// read_with_llm: send the page or the image region to a vision model.
    /// find_with_llm / confirm_with_llm: ask a model (or a person) for the
    /// value, or to confirm the one given in `person`.
    pub action: &'static str,
}

#[derive(Serialize)]
pub struct DocResult {
    pub pages: Vec<PageResult>,
    /// Plain text in reading order, one segment per line.
    pub text: String,
    /// Every table in the document, as rows of cells.
    pub tables: Vec<Vec<Vec<String>>>,
    /// The document as Markdown.
    pub markdown: String,
    /// The document as Markdown with personal data replaced by placeholders.
    pub markdown_redacted: String,
    /// Content grouped under its headings, in reading order.
    pub sections: Vec<Section>,
    /// Every piece of personal data found, with where it was found.
    pub pii: Vec<PiiFinding>,
    /// All text that is in the file but not visible (see PageResult).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hidden_text: String,
    /// Name, email and phone with confidence.
    pub person: Person,
    /// What still needs an LLM or a person; empty when the parser is sure of
    /// everything.
    pub needs_review: Vec<ReviewItem>,
}

/// Placeholder written where an image or a scanned page was not read, so
/// the caller can see where its text belongs.
const UNREAD_IMAGE_MARK: &str = "<!-- image not read: see needs_review -->";
const UNREAD_PAGE_MARK: &str = "<!-- scanned page not read: see needs_review -->";

pub fn analyse_page(page: &PageContent) -> PageResult {
    let segs = lines::build_segments(&page.glyphs);

    // Text drawn on top of a figure (chart labels, diagram callouts) is kept
    // with the figure, not mixed into the body text.
    let page_area = (page.width * page.height).max(1.0);
    let mut figures: Vec<Figure> = page
        .images
        .iter()
        .filter(|b| (MIN_FIGURE_SHARE..MAX_FIGURE_SHARE).contains(&(b.area() / page_area)))
        .map(|b| Figure { bbox: *b, text: String::new() })
        .collect();
    // An image with lines of normal text on it is a background (design
    // panel, coloured sidebar), not a figure: its text stays body text.
    // Figure text is short, scattered labels.
    let centre_in = |b: &BBox, s: &Segment| {
        let (cx, cy) = ((s.bbox.x0 + s.bbox.x1) / 2.0, s.bbox.cy());
        cx >= b.x0 && cx <= b.x1 && cy >= b.y0 && cy <= b.y1
    };
    let body_size = {
        let mut v: Vec<f32> = segs.iter().map(|s| s.size).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        v.get(v.len() / 2).copied().unwrap_or(10.0)
    };
    figures.retain(|f| {
        let inside: Vec<&Segment> = segs.iter().filter(|s| centre_in(&f.bbox, s)).collect();
        let chars: usize = inside.iter().map(|s| s.text.chars().count()).sum();
        let mean = chars as f32 / inside.len().max(1) as f32;
        let prose = inside.len() >= MIN_BACKGROUND_LINES && mean >= MIN_BACKGROUND_LINE_CHARS;
        // A lone line or large type over an image is a banner (a name on a
        // header graphic), not chart labels.
        let banner = inside.len() == 1 || inside.iter().any(|s| s.size >= BANNER_SIZE_RATIO * body_size);
        !(prose || banner)
    });
    let mut body = Vec::with_capacity(segs.len());
    for s in segs {
        let (cx, cy) = ((s.bbox.x0 + s.bbox.x1) / 2.0, s.bbox.cy());
        match figures.iter_mut().find(|f| cx >= f.bbox.x0 && cx <= f.bbox.x1 && cy >= f.bbox.y0 && cy <= f.bbox.y1) {
            Some(f) => {
                if !f.text.is_empty() {
                    f.text.push('\n');
                }
                f.text.push_str(&s.text);
            }
            None => body.push(s),
        }
    }
    let mut segs = body;
    let mut tables = tables::detect(page, &segs);
    let icon_links = link_targets(page, &mut segs, &mut tables, body_size);
    let tables = split_at_section_rows(tables);

    // Replace table text with one pseudo-segment per row, so reading order
    // treats each row as a unit instead of reading the table column by column.
    // A line inside a table's box is replaced by the table only when the
    // table's cells hold its words. Grid layouts (skill boxes, two-column
    // job panels) can be taken for a table whose rows miss some of the text
    // inside its box; without this check that text vanished from the output
    // (on one real resume, a fifth of its words). Text is never dropped for
    // being inside a table.
    let cell_words: Vec<std::collections::HashSet<String>> = tables
        .iter()
        .map(|t| t.rows.iter().flatten().flat_map(|c| c.split_whitespace().map(str::to_lowercase)).collect())
        .collect();
    let mut items: Vec<Segment> = segs
        .into_iter()
        .filter(|s| {
            !tables.iter().zip(&cell_words).any(|(t, words)| {
                t.bbox.contains(&s.bbox, 2.0) && s.text.split_whitespace().all(|w| words.contains(&w.to_lowercase()))
            })
        })
        .collect();
    for (ti, t) in tables.iter().enumerate() {
        for (row, bbox) in t.rows.iter().zip(&t.row_boxes) {
            let size = bbox.height().max(1.0);
            items.push(Segment { text: row.join(" | "), bbox: *bbox, size, bold: false, table: Some(ti) });
        }
    }

    items.extend(icon_links);
    for b in &page.unread_images {
        items.push(Segment { text: UNREAD_IMAGE_MARK.to_string(), bbox: *b, size: body_size, bold: false, table: None });
    }
    if page.ocr.as_ref().is_some_and(|o| o.engine.is_none()) {
        let top = BBox { x0: 0.0, y0: 0.0, x1: page.width, y1: 1.0 };
        items.push(Segment { text: UNREAD_PAGE_MARK.to_string(), bbox: top, size: body_size, bold: false, table: None });
    }
    mark_graphic_bullets(&mut items, page);
    let order = layout::reading_order(&items, &page.fills);
    let segments: Vec<Segment> = order.into_iter().map(|i| items[i].clone()).collect();
    let segments = running_heads_first(segments, page.height);
    let segments = title_first(segments, page.height, body_size);
    let blocks = markdown::blocks(&segments, &tables, body_size);
    let markdown = markdown::to_markdown(&blocks, false);
    let hidden_text = {
        let mut h = lines::build_segments(&page.hidden_glyphs);
        h.sort_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0).then(a.bbox.x0.total_cmp(&b.bbox.x0)));
        h.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n")
    };
    PageResult {
        width: page.width,
        height: page.height,
        segments,
        tables,
        figures,
        ocr: page.ocr.clone(),
        markdown,
        blocks,
        hidden_text,
        unread_images: page.unread_images.clone(),
    }
}

/// A link whose text does not show where it goes ("LinkedIn", "GitHub",
/// "Email me") becomes a Markdown link, `[LinkedIn](https://...)`, so the
/// address is kept: on 472 real resumes, 208 links in 64 of them pointed to a
/// profile or mailbox that was nowhere in the visible text (80 of them on an
/// icon with no text at all). A link whose text already is the address is
/// left as it is. Returns the addresses of links on icons, as new lines at
/// the icon's place.
fn link_targets(page: &PageContent, segs: &mut [Segment], tables: &mut [Table], body_size: f32) -> Vec<Segment> {
    let mut icons = Vec::new();
    for (rect, uri) in &page.links {
        let area = BBox { x0: rect.x0 - 1.0, y0: rect.y0 - 1.0, x1: rect.x1 + 1.0, y1: rect.y1 + 1.0 };
        let inside: Vec<model::Glyph> = page
            .glyphs
            .iter()
            .filter(|g| {
                let (cx, cy) = ((g.bbox.x0 + g.bbox.x1) / 2.0, g.bbox.cy());
                cx >= area.x0 && cx <= area.x1 && cy >= area.y0 && cy <= area.y1
            })
            .cloned()
            .collect();
        let parts = lines::build_segments(&inside);
        let label_of = |s: &Segment| s.text.trim().trim_start_matches(|c: char| lines::is_bullet_char(c) || c == ' ').to_string();
        let labels: Vec<String> = parts.iter().map(label_of).filter(|l| l.chars().any(char::is_alphanumeric)).collect();
        let safe_uri = uri.replace(' ', "%20").replace('(', "%28").replace(')', "%29");
        if labels.is_empty() {
            if !uri.to_lowercase().starts_with("tel:") && !icons.iter().any(|s: &Segment| s.text.contains(&safe_uri)) {
                icons.push(Segment { text: format!("<{safe_uri}>"), bbox: *rect, size: body_size, bold: false, table: None });
            }
            continue;
        }
        // A link over several lines (a wrapped address, "View my / portfolio")
        // is checked as a whole and linked on its first line.
        if shows_target(&labels.join(""), uri) {
            continue;
        }
        let label = labels[0].as_str();
        if label.chars().count() < 2 || label.contains(['[', ']']) {
            continue;
        }
        // Text that is an address, or a piece of one ("@gmail.com", ".com"
        // on the line a wrap left it on), is the address: a link inside it
        // would break it.
        if label.contains('@') || label.starts_with('.') {
            continue;
        }
        let overlaps = |b: &BBox| b.x0 < area.x1 && area.x0 < b.x1 && b.y0 < area.y1 && area.y0 < b.y1;
        for s in segs.iter_mut().filter(|s| overlaps(&s.bbox)) {
            put_link(&mut s.text, label, &safe_uri);
        }
        for t in tables.iter_mut().filter(|t| overlaps(&t.bbox)) {
            for cell in t.rows.iter_mut().flatten() {
                put_link(cell, label, &safe_uri);
            }
        }
    }
    // An icon next to the address written out, or next to a text link to the
    // same place, adds nothing.
    let page_text: String = segs.iter().map(|s| s.text.as_str()).chain(tables.iter().flat_map(|t| t.rows.iter().flatten().map(String::as_str))).collect();
    icons.retain(|s| {
        let uri = &s.text[1..s.text.len() - 1];
        !page_text.contains(uri) && !shows_target(&page_text, uri)
    });
    icons
}

/// A row holding nothing but a section name ("Experience", "Education") is
/// a heading between two tables, not a row: Word resumes laid out as one big
/// table put each section name in a merged row. The table is split there;
/// the name's own line is then outside every table and the Markdown step
/// makes it a heading (on 472 real resumes, 24 section names were left as
/// table rows).
fn split_at_section_rows(tables: Vec<Table>) -> Vec<Table> {
    let mut out = Vec::new();
    for t in tables {
        let (mut rows, mut boxes): (Vec<Vec<String>>, Vec<BBox>) = (Vec::new(), Vec::new());
        let flush = |rows: &mut Vec<Vec<String>>, boxes: &mut Vec<BBox>, out: &mut Vec<Table>| {
            if let Some(bbox) = BBox::union_all(boxes.iter()) {
                out.push(Table { bbox, rows: std::mem::take(rows), row_boxes: std::mem::take(boxes) });
            }
        };
        for (row, bbox) in t.rows.into_iter().zip(t.row_boxes) {
            let filled: Vec<&String> = row.iter().filter(|c| !c.trim().is_empty()).collect();
            if filled.len() == 1 && markdown::is_section_name(filled[0]) {
                flush(&mut rows, &mut boxes, &mut out);
                continue;
            }
            rows.push(row);
            boxes.push(bbox);
        }
        flush(&mut rows, &mut boxes, &mut out);
    }
    out
}

/// The link text already shows the address (or the phone number).
fn shows_target(label: &str, uri: &str) -> bool {
    let squash = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    let lower = uri.to_lowercase();
    if let Some(number) = lower.strip_prefix("tel:") {
        let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
        return digits(label).contains(&digits(number));
    }
    let core = ["mailto:", "https://", "http://"].iter().fold(lower.as_str(), |u, p| u.strip_prefix(p).unwrap_or(u));
    let core = core.strip_prefix("www.").unwrap_or(core).trim_end_matches('/');
    let core: String = squash(core).chars().take(15).collect();
    !core.is_empty() && squash(label).contains(&core)
}

/// Link the first plain occurrence of `label` in `text` to `uri`. A link
/// whose box covers only part of a word ("il:" of "Email:") links the whole
/// word; a word that is an address ("jane.doe@mail.com") is left alone, a
/// link inside it would break it.
fn put_link(text: &mut String, label: &str, uri: &str) {
    let mut from = 0;
    while let Some(i) = text[from..].find(label).map(|i| i + from) {
        from = i + label.len();
        // Out to the ends of the words the label cuts into; punctuation
        // next to it ("LinkedIn,", "(GitHub)") is not part of the link.
        let s = if text[..i].ends_with(char::is_alphanumeric) {
            text[..i].rfind(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '|' | '(' | ')' | ']')).map_or(0, |k| k + 1)
        } else {
            i
        };
        let e = if text[from..].starts_with(char::is_alphanumeric) {
            text[from..].find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '|' | '(' | ')' | '[')).map_or(text.len(), |k| from + k)
        } else {
            from
        };
        let word = &text[s..e];
        let token_start = text[..s].rfind(char::is_whitespace).map_or(0, |k| k + 1);
        let token_end = text[e..].find(char::is_whitespace).map_or(text.len(), |k| e + k);
        if text[..s].ends_with('[') || word.contains(['[', ']']) || text[token_start..token_end].contains('@') {
            continue;
        }
        let link = format!("[{word}]({uri})");
        if text.contains(&link) {
            return;
        }
        text.replace_range(s..e, &link);
        return;
    }
}

/// A heading under which the following blocks sit.
#[derive(Clone, Debug, Serialize)]
pub struct Section {
    /// Heading text; empty for content before the first heading.
    pub title: String,
    pub level: u8,
    pub page: usize,
    pub blocks: Vec<markdown::OutBlock>,
}

/// One piece of personal data and where it was found.
#[derive(Clone, Debug, Serialize)]
pub struct PiiFinding {
    pub kind: &'static str,
    pub text: String,
    /// 1-based page number and index into that page's `blocks`.
    pub page: usize,
    pub block: usize,
    pub confidence: f32,
}

fn sections(pages: &[PageResult]) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for (pi, p) in pages.iter().enumerate() {
        for b in &p.blocks {
            if b.kind == "heading" {
                out.push(Section { title: b.text.clone(), level: b.level.unwrap_or(3), page: pi + 1, blocks: Vec::new() });
            } else {
                if out.is_empty() {
                    out.push(Section { title: String::new(), level: 0, page: pi + 1, blocks: Vec::new() });
                }
                out.last_mut().unwrap().blocks.push(b.clone());
            }
        }
    }
    out
}

/// Whether the document is about one person (a resume or profile): it has
/// an email or phone number, or at least two resume section headings.
fn about_a_person(pages: &[PageResult]) -> bool {
    let contact = pages.iter().flat_map(|p| &p.blocks).any(|b| {
        b.pii.iter().any(|s| matches!(s.kind, "email" | "phone"))
            || b.rows.iter().flatten().flatten().any(|c| pii::detect(c).iter().any(|s| matches!(s.kind, "email" | "phone")))
    });
    let headings = pages.iter().flat_map(|p| &p.blocks).filter(|b| b.kind == "heading" && is_section_heading(&b.text)).count();
    contact || headings >= 2
}

fn is_section_heading(text: &str) -> bool {
    const SECTIONS: &[&str] =
        &["experience", "education", "skills", "employment", "projects", "summary", "objective", "certifications", "profile"];
    let t = text.to_lowercase();
    t.split_whitespace().count() <= 4 && SECTIONS.iter().any(|w| t.split(|c: char| !c.is_alphabetic()).any(|x| x == *w))
}

/// The person's name at the top of a resume, as byte spans per block of the
/// first page. It is the first short line that looks like a name (in any
/// capitalisation), or the first part of a "Name | City | Phone" line, or a
/// name broken over consecutive one-word lines; single words that also occur
/// in the email address ("jdoe.smith@...") are name parts too.
fn find_document_name(page: &PageResult) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    let n = page.blocks.len().min(8);
    let one_word = |t: &str| {
        let w = t.trim().trim_start_matches('#').trim();
        w.chars().count() >= 2
            && w.chars().all(|c| c.is_alphabetic() || matches!(c, '.' | '\'' | '-'))
            && w.chars().next().is_some_and(|c| c.is_uppercase())
            && !pii::NOT_NAME_WORDS.contains(&w.to_lowercase().as_str())
    };
    let locals: Vec<String> = page
        .blocks
        .iter()
        .flat_map(|b| b.pii.iter().filter(|s| s.kind == "email"))
        .map(|s| s.text.split('@').next().unwrap_or("").to_ascii_lowercase())
        .collect();
    for i in 0..n.min(5) {
        let b = &page.blocks[i];
        if b.kind == "table" || b.text.chars().count() > 200 {
            continue;
        }
        if b.pii.iter().any(|s| s.kind == "person_name" && s.start < 60) {
            return out; // already found (NER or a "Name:" label)
        }
        // The name comes before the first section ("Summary", "Experience").
        if b.kind == "heading" && is_section_heading(&b.text) {
            break;
        }
        let t = b.text.as_str();
        let lead = t.len() - t.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
        let body = &t[lead..];
        if pii::looks_like_name(body) {
            out.push((i, lead, t.trim_end().len().max(lead)));
            break;
        }
        // "Name | City | Phone": the first field.
        if let Some(cut) = body.find(['|', '•', '·', '–', ',']) {
            let first = body[..cut].trim_end();
            if cut > 0 && pii::looks_like_name(first) && b.pii.iter().any(|s| s.start >= lead + cut) {
                out.push((i, lead, lead + first.len()));
                break;
            }
        }
        // A name broken over one-word lines ("JANE" / "DOE").
        if one_word(body) {
            let next = page.blocks.get(i + 1).map(|x| x.text.as_str()).unwrap_or("");
            let lw = body.trim().to_ascii_lowercase();
            if one_word(next) || locals.iter().any(|l| lw.len() >= 3 && l.contains(&lw)) {
                out.push((i, lead, t.trim_end().len()));
                if one_word(next) {
                    out.push((i + 1, 0, next.trim_end().len()));
                }
                break;
            }
        }
    }
    // Further one-word lines in the header that are part of the email
    // address are the rest of the name ("JANE / DOE / ... / SMITH").
    if !out.is_empty() {
        for i in 0..n {
            if out.iter().any(|o| o.0 == i) {
                continue;
            }
            let t = page.blocks[i].text.trim();
            let lw = t.to_ascii_lowercase();
            if one_word(t) && lw.len() >= 3 && locals.iter().any(|l| l.contains(&lw)) {
                out.push((i, page.blocks[i].text.find(t).unwrap_or(0), page.blocks[i].text.find(t).unwrap_or(0) + t.len()));
            }
        }
    }
    out
}

/// Label the person's name at the top of the document as PII and return the
/// name (all its parts), so that every other mention can be found.
///
/// How sure the parser is rests on the email address, the one other place a
/// resume usually spells the name: a name sharing a word with it is 0.95, a
/// top line with no email to compare is 0.75, and one the email does not
/// back is 0.5. When no top line reads as a name, a short line near the top
/// that shares a word with the email address is the name (on 472 real
/// resumes, 88 of the 161 resumes without a name had one), then the largest
/// text at the top. There is no name model: on the same resumes it added 64
/// names over these rules, of which the email address backed only 22.
fn mark_document_name(pages: &mut [PageResult]) -> (Vec<String>, Option<Detail>) {
    if pages.is_empty() || !about_a_person(pages) {
        return (Vec::new(), None);
    }
    let locals = email_locals(pages);
    let page = &mut pages[0];
    // A name labelled as one ("Name: ...") or found by the name model.
    if let Some(s) = page.blocks.iter().take(8).flat_map(|b| b.pii.iter().filter(|s| s.kind == "person_name" && s.start < 60)).next() {
        let text = s.text.clone();
        let matches = matches_email(&text, &locals);
        let detail = Detail {
            confidence: if matches { s.confidence.max(0.95) } else { s.confidence },
            basis: format!("labelled as the name{}", if matches { "; matches the email address" } else { "" }),
            text: text.clone(),
        };
        let mut names = vec![text];
        names.extend(find_document_name(page).into_iter().map(|(bi, s, e)| page.blocks[bi].text[s..e].to_string()));
        return (names, Some(detail));
    }
    let mut basis = "the first line at the top that reads as a name";
    let mut spans = find_document_name(page);
    let found = spans.iter().map(|&(bi, s, e)| page.blocks[bi].text.get(s..e).unwrap_or("")).collect::<Vec<_>>().join(" ");
    // A line the email address backs beats a name-shaped line it does not
    // (a heading, a place, a referee).
    if spans.is_empty() || !matches_email(&found, &locals) {
        if let Some(span) = line_matching_email(page, &locals) {
            basis = "a line near the top";
            spans = vec![span];
        }
    }
    if spans.is_empty() {
        if let Some(span) = first_name_line(page) {
            spans.push(span);
        } else if let Some(span) = largest_top_line(page) {
            basis = "the largest text at the top";
            spans.push(span);
        }
    }
    let spans: Vec<(usize, usize, usize)> = spans
        .into_iter()
        .filter(|&(bi, start, end)| end > start && page.blocks[bi].text.is_char_boundary(start) && page.blocks[bi].text.is_char_boundary(end))
        .collect();
    if spans.is_empty() {
        return (Vec::new(), None);
    }
    let names: Vec<String> = spans.iter().map(|&(bi, s, e)| page.blocks[bi].text[s..e].trim().to_string()).collect();
    let text = names.join(" ");
    let matches = matches_email(&text, &locals);
    let (confidence, why) = if matches {
        (0.95, "; matches the email address")
    } else if locals.is_empty() {
        (0.75, "; no email address to compare")
    } else {
        (0.5, "; the email address does not match")
    };
    for (bi, start, end) in spans {
        let b = &mut page.blocks[bi];
        if !b.pii.iter().any(|s| s.start < end && start < s.end) {
            b.pii.push(pii::PiiSpan { kind: "person_name", start, end, text: b.text[start..end].to_string(), confidence });
            b.pii.sort_by_key(|s| s.start);
            b.refresh();
        }
    }
    page.markdown = markdown::to_markdown(&page.blocks, false);
    (names, Some(Detail { text, confidence, basis: format!("{basis}{why}") }))
}

/// The letters of every email address's local part ("jane.doe85" ->
/// "janedoe"), when there are at least three.
fn email_locals(pages: &[PageResult]) -> Vec<String> {
    let mut out: Vec<String> = pages
        .iter()
        .flat_map(|p| &p.blocks)
        .flat_map(|b| b.pii.iter().filter(|s| s.kind == "email"))
        .map(|s| s.text.split('@').next().unwrap_or("").chars().filter(char::is_ascii_alphabetic).collect::<String>().to_ascii_lowercase())
        .filter(|l| l.len() >= 3)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A word of the name (three letters or more) is in an email address.
fn matches_email(name: &str, locals: &[String]) -> bool {
    name.split(|c: char| !c.is_alphabetic()).filter(|w| w.chars().count() >= 3).any(|w| {
        let w = w.to_lowercase();
        locals.iter().any(|l| l.contains(&w))
    })
}

/// A short line among the first lines of page 1 that shares a word with the
/// email address: the name, in whatever case or font. Lines, not blocks: the
/// name is often joined into one block with the title under it (all 82 such
/// names on 472 real resumes were).
fn line_matching_email(page: &PageResult, locals: &[String]) -> Option<(usize, usize, usize)> {
    if locals.is_empty() {
        return None;
    }
    for s in page.segments.iter().filter(|s| s.table.is_none()).take(15) {
        let t = s.text.as_str();
        if is_section_heading(t) {
            break;
        }
        let lead = t.len() - t.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
        let body = &t[lead..];
        let first = body.find(['|', '•', '·', '–', ',']).map(|cut| body[..cut].trim_end()).unwrap_or(body.trim_end());
        let words: Vec<&str> = first.split_whitespace().collect();
        let shaped = (1..=4).contains(&words.len())
            && first.chars().count() <= 40
            && words.iter().all(|w| w.chars().all(|c| c.is_alphabetic() || matches!(c, '.' | '\'' | '-')))
            && !words.iter().any(|w| pii::NOT_NAME_WORDS.contains(&w.to_lowercase().as_str()))
            // A single word must be long enough not to match by chance.
            && (words.len() > 1 || first.chars().count() >= 4);
        if shaped && matches_email(first, locals) {
            return block_span(page, first);
        }
    }
    None
}

/// The first of the top lines of page 1 that reads as a name, line by line:
/// a name joined into one block with the line above or below it is missed
/// when only whole blocks are looked at.
fn first_name_line(page: &PageResult) -> Option<(usize, usize, usize)> {
    for s in page.segments.iter().filter(|s| s.table.is_none()).take(8) {
        let t = s.text.trim();
        if is_section_heading(t) {
            break;
        }
        let body = t.trim_start_matches(|c: char| !c.is_alphanumeric());
        if pii::looks_like_name(body) {
            return block_span(page, body);
        }
    }
    None
}

/// The largest text in the top third of page 1, when it is clearly larger
/// than the body text and reads as a name.
fn largest_top_line(page: &PageResult) -> Option<(usize, usize, usize)> {
    let mut sizes: Vec<f32> = page.segments.iter().map(|s| s.size).filter(|s| *s > 0.0).collect();
    sizes.sort_by(|a, b| a.total_cmp(b));
    let body = *sizes.get(sizes.len() / 2)?;
    let top = page
        .segments
        .iter()
        .filter(|s| s.table.is_none() && s.bbox.y1 <= page.height * 0.35)
        .max_by(|a, b| a.size.total_cmp(&b.size))?;
    let text = top.text.trim();
    if top.size < 1.2 * body || !pii::looks_like_name(text) {
        return None;
    }
    block_span(page, text)
}

/// Where a line's text sits in the first blocks of the page.
fn block_span(page: &PageResult, text: &str) -> Option<(usize, usize, usize)> {
    page.blocks.iter().enumerate().take(15).find_map(|(i, b)| b.text.find(text).map(|s| (i, s, s + text.len())))
}

/// Mark every mention of the person and of known values: name parts in any
/// capitalisation where they read as a name (capitalised, or inside a link or
/// handle such as "jane-doe"), the name run together ("janedoe"), and the
/// known email and phone in any spacing.
fn mark_person(pages: &mut [PageResult], names: &[String], known: Option<&pii::Known>) {
    let mut all: Vec<String> = names.to_vec();
    if let Some(k) = known {
        all.extend(k.name.iter().cloned());
    }
    let mut tokens: Vec<String> = all
        .iter()
        .flat_map(|n| n.split(|c: char| !c.is_alphabetic()).map(str::to_string).collect::<Vec<_>>())
        .filter(|t| t.chars().count() >= 3 && !pii::NOT_NAME_WORDS.contains(&t.to_lowercase().as_str()))
        .map(|t| t.to_ascii_lowercase())
        .collect();
    tokens.sort();
    tokens.dedup();
    // The name run together, both ways round: "janedoe", "doejane".
    let mut joined: Vec<String> = Vec::new();
    for n in &all {
        let parts: Vec<String> =
            n.split(|c: char| !c.is_alphabetic()).filter(|t| t.chars().count() >= 2).map(|t| t.to_ascii_lowercase()).collect();
        if parts.len() >= 2 {
            joined.push(parts.concat());
            let mut r = parts.clone();
            r.reverse();
            joined.push(r.concat());
            if parts.len() > 2 {
                joined.push(format!("{}{}", parts[0], parts[parts.len() - 1]));
            }
        }
    }
    joined.retain(|j| j.len() >= 6);
    let mut literal: Vec<(String, &'static str)> = Vec::new();
    let mut phone: Option<regex::Regex> = None;
    if let Some(k) = known {
        if let Some(e) = &k.email {
            literal.push((e.trim().to_ascii_lowercase(), "email"));
            if let Some(local) = e.split('@').next().filter(|l| l.len() >= 4) {
                literal.push((local.to_ascii_lowercase(), "email"));
            }
        }
        for o in &k.other {
            if o.trim().chars().count() >= 3 {
                literal.push((o.trim().to_ascii_lowercase(), "personal_data"));
            }
        }
        if let Some(p) = &k.phone {
            let d: Vec<char> = p.chars().filter(|c| c.is_ascii_digit()).collect();
            if d.len() >= 7 {
                let last = &d[d.len().saturating_sub(10)..];
                let pat = last.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(r"[\s.\-()]*");
                phone = regex::Regex::new(&format!(r"(?:\+?\d{{1,3}}[\s.\-]*)?(?:\(0?\)[\s.\-]*)?0?{pat}")).ok();
            }
        }
    }
    if tokens.is_empty() && joined.is_empty() && literal.is_empty() && phone.is_none() {
        return;
    }
    for p in pages.iter_mut() {
        let mut changed = false;
        for b in &mut p.blocks {
            let lower = b.text.to_ascii_lowercase();
            let mut add: Vec<(usize, usize, &'static str)> = Vec::new();
            let joint = |c: Option<char>| c.is_some_and(|c| matches!(c, '-' | '.' | '_' | '/' | '@' | '+') || c.is_ascii_digit());
            for t in &tokens {
                for (pos, _) in lower.match_indices(t.as_str()) {
                    let end = pos + t.len();
                    let before = b.text[..pos].chars().next_back();
                    let after = b.text[end..].chars().next();
                    if before.is_some_and(|c| c.is_alphabetic()) || after.is_some_and(|c| c.is_alphabetic()) {
                        continue;
                    }
                    let capital = b.text[pos..].chars().next().is_some_and(|c| c.is_uppercase());
                    if capital || joint(before) || joint(after) {
                        add.push((pos, end, "person_name"));
                    }
                }
            }
            for j in &joined {
                for (pos, _) in lower.match_indices(j.as_str()) {
                    add.push((pos, pos + j.len(), "person_name"));
                }
            }
            for (v, kind) in &literal {
                for (pos, _) in lower.match_indices(v.as_str()) {
                    add.push((pos, pos + v.len(), kind));
                }
            }
            if let Some(re) = &phone {
                for m in re.find_iter(&b.text) {
                    add.push((m.start(), m.end(), "phone"));
                }
            }
            for (start, end, kind) in add {
                if !b.text.is_char_boundary(start) || !b.text.is_char_boundary(end) {
                    continue;
                }
                if b.pii.iter().any(|s| s.start <= start && end <= s.end) {
                    continue;
                }
                // A wider new span replaces the narrower ones inside it.
                b.pii.retain(|s| !(start <= s.start && s.end <= end));
                if b.pii.iter().any(|s| s.start < end && start < s.end) {
                    continue;
                }
                b.pii.push(pii::PiiSpan { kind, start, end, text: b.text[start..end].to_string(), confidence: 0.9 });
                changed = true;
            }
            if changed {
                b.pii.sort_by_key(|s| s.start);
                b.refresh();
            }
        }
        if changed {
            p.markdown = markdown::to_markdown(&p.blocks, false);
        }
    }
}

/// A phone number a narrow column wrapped onto the next line ("Mobile: +1
/// (555)" / "123 4567") is one number: the line ends in a phone label and
/// too few digits for a number, and the next block starts with the rest.
/// Both parts are tagged, so both are redacted; the first carries the whole
/// number.
fn join_wrapped_phones(pages: &mut [PageResult]) {
    static TAIL: OnceLock<Regex> = OnceLock::new();
    static HEAD: OnceLock<Regex> = OnceLock::new();
    let tail = TAIL.get_or_init(|| {
        Regex::new(r"(?i)\b(?:phone|mobile|mob|tel|cell|contact)(?:\s*(?:no|number))?\.?\s*[:\-]?\s*(\+?[\d()][\d\s().\-]*)$").unwrap()
    });
    let head = HEAD.get_or_init(|| Regex::new(r"^\s*(\d[\d\s().\-]*\d)").unwrap());
    let digits = |t: &str| t.chars().filter(char::is_ascii_digit).count();
    for p in pages.iter_mut() {
        let mut changed = false;
        for i in 1..p.blocks.len() {
            let (first, rest) = p.blocks.split_at_mut(i);
            let (a, b) = (&mut first[i - 1], &mut rest[0]);
            let (Some(t), Some(h)) = (tail.captures(a.text.trim_end()).and_then(|c| c.get(1)), head.captures(&b.text).and_then(|c| c.get(1)))
            else {
                continue;
            };
            let whole = format!("{} {}", t.as_str().trim(), h.as_str());
            if digits(t.as_str()) >= 8 || !(8..=15).contains(&digits(&whole)) || a.pii.iter().chain(&b.pii).any(|s| s.kind == "phone") {
                continue;
            }
            a.pii.push(pii::PiiSpan { kind: "phone", start: t.start(), end: t.start() + t.as_str().trim_end().len(), text: whole, confidence: 0.9 });
            b.pii.push(pii::PiiSpan { kind: "phone", start: h.start(), end: h.end(), text: h.as_str().to_string(), confidence: 0.9 });
            for blk in [a, b] {
                blk.pii.sort_by_key(|s| s.start);
                blk.refresh();
            }
            changed = true;
        }
        if changed {
            p.markdown = markdown::to_markdown(&p.blocks, false);
        }
    }
}

/// A value found as PII anywhere (a name in the title, an email in the
/// header) is PII everywhere: tag every other occurrence in the document,
/// case-insensitively, including in table cells' text.
/// Referees are other people. Under a "References" heading, the name that
/// starts an entry ("Anil Mehta, Manager, Infosys") is personal data.
fn mark_referees(pages: &mut [PageResult]) {
    let mut under = false;
    for p in pages.iter_mut() {
        let mut changed = false;
        for b in &mut p.blocks {
            let head = b.text.trim().trim_start_matches('#').trim().trim_end_matches(':').to_lowercase();
            if b.kind == "heading" || markdown::is_section_name(&b.text) {
                under = matches!(head.as_str(), "references" | "reference" | "referees" | "referee" | "professional references");
                continue;
            }
            if !under || b.kind == "table" {
                continue;
            }
            // Each entry: the text before its first ",", "|", "(" or " - ".
            let lead = b.text.len() - b.text.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
            let entry = &b.text[lead..];
            let cut = [",", "|", "(", " - ", " – ", "\n"].iter().filter_map(|d| entry.find(d)).min().unwrap_or(entry.len());
            let name = entry[..cut].trim_end();
            let bare = pii::strip_title(name);
            let (start, end) = (lead, lead + name.len());
            if pii::looks_like_name(bare) && !b.pii.iter().any(|s| s.start < end && start < s.end) {
                b.pii.push(pii::PiiSpan { kind: "person_name", start, end, text: name.to_string(), confidence: 0.85 });
                b.pii.sort_by_key(|s| s.start);
                b.refresh();
                changed = true;
            }
        }
        if changed {
            p.markdown = markdown::to_markdown(&p.blocks, false);
        }
    }
}

fn propagate_pii(pages: &mut [PageResult]) {
    // A place name is not spread: "Pune" in a job line is the job's city.
    let mut values: Vec<(String, &'static str)> = pages
        .iter()
        .flat_map(|p| p.blocks.iter().flat_map(|b| b.pii.iter().map(|s| (s.text.trim().to_string(), s.kind))))
        .filter(|(t, k)| t.chars().count() >= 4 && *k != "location")
        .collect();
    values.sort();
    values.dedup();
    // Longer values first, so "Jane Doe" wins over "Jane".
    values.sort_by_key(|(t, _)| std::cmp::Reverse(t.len()));
    let mut changed = false;
    for p in pages.iter_mut() {
        for b in &mut p.blocks {
            let lower = b.text.to_lowercase();
            for (v, kind) in &values {
                let needle = v.to_lowercase();
                let mut from = 0;
                while let Some(pos) = lower[from..].find(&needle) {
                    let (start, end) = (from + pos, from + pos + needle.len());
                    from = end;
                    // Byte offsets from the lower-cased copy are only valid
                    // when lower-casing kept the length (true for ASCII).
                    if lower.len() != b.text.len() || !b.text.is_char_boundary(start) || !b.text.is_char_boundary(end) {
                        break;
                    }
                    // Whole words only: "Rally" is not inside "Generally".
                    let inside_word = b.text[..start].chars().next_back().is_some_and(|c| c.is_alphanumeric())
                        || b.text[end..].chars().next().is_some_and(|c| c.is_alphanumeric());
                    if inside_word || b.pii.iter().any(|s| s.start < end && start < s.end) {
                        continue;
                    }
                    b.pii.push(pii::PiiSpan { kind, start, end, text: b.text[start..end].to_string(), confidence: 0.8 });
                    changed = true;
                }
            }
            b.pii.sort_by_key(|s| s.start);
            b.refresh();
        }
        if changed {
            p.markdown = markdown::to_markdown(&p.blocks, false);
        }
    }
}

/// A bullet drawn as a mark no larger than this share of the font size
/// (small bullet pictures on real resumes measure 0.76 to 0.79)...
const BULLET_MARK_MAX: f32 = 0.9;
/// ...and no smaller than this share (specks and rule ends are not bullets).
const BULLET_MARK_MIN: f32 = 0.12;
/// Most space between the mark and the line's first letter, in font sizes.
const BULLET_MARK_GAP: f32 = 2.5;

/// Bullets drawn as small shapes (a dot or square path, a tiny image)
/// rather than as a character leave list items with no marker in the text,
/// so the items run together into one paragraph. A small, roughly square
/// mark just left of a line's start, vertically centred on it, is that
/// line's bullet, and the line gets a "•" so it starts a list item. Two or
/// more such lines with their marks at the same x are needed, so a lone mark
/// (an icon before a contact line) is left alone; icons are also larger than
/// bullets, about the size of the text.
fn mark_graphic_bullets(items: &mut [Segment], page: &PageContent) {
    let marks: Vec<&BBox> = page.graphics.iter().chain(&page.shaded).chain(&page.images).collect();
    let mut hits: Vec<(usize, BBox)> = Vec::new();
    for (i, s) in items.iter().enumerate() {
        if s.table.is_some() || s.text.trim_start().chars().next().is_some_and(lines::is_bullet_char) {
            continue;
        }
        let h = s.size.max(1.0);
        let found = marks.iter().find(|m| {
            let (w, mh) = (m.width(), m.height());
            let side = w.max(mh);
            side <= BULLET_MARK_MAX * h
                && side >= BULLET_MARK_MIN * h
                && w <= 2.0 * mh.max(0.1)
                && mh <= 2.0 * w.max(0.1)
                && m.x1 <= s.bbox.x0 + 0.5
                && s.bbox.x0 - m.x1 <= BULLET_MARK_GAP * h
                && m.cy() >= s.bbox.y0
                && m.cy() <= s.bbox.y1
        });
        if let Some(m) = found {
            hits.push((i, **m));
        }
    }
    for &(i, m) in &hits {
        if hits.iter().any(|(j, m2)| *j != i && (m2.x0 - m.x0).abs() <= 2.0) {
            items[i].text = format!("• {}", items[i].text.trim_start());
            // The line now starts at its bullet, as a line with a bullet
            // character does; its wrapped lines, indented to the text, then
            // read as the same item instead of new ones.
            items[i].bbox = items[i].bbox.union(&m);
        }
    }
}

/// Text drawn over an image is kept with the image as figure text (chart
/// labels, diagram callouts), so it stays out of the body text of reports
/// and papers. In a resume or profile it is content: skill badges and
/// sidebar panels drawn as pictures, with the text set on top. Left out, a
/// resume lost those skills and panels entirely. So in a document about a
/// person, each figure's text becomes a paragraph where the figure sits:
/// before the first block below the figure's top that shares its columns.
fn figure_text_into_body(pages: &mut [PageResult]) {
    if !about_a_person(pages) {
        return;
    }
    for p in pages.iter_mut() {
        let figs: Vec<(BBox, String)> =
            p.figures.iter().filter(|f| !f.text.trim().is_empty()).map(|f| (f.bbox, f.text.trim().to_string())).collect();
        if figs.is_empty() {
            continue;
        }
        for (bbox, text) in figs {
            let at = p
                .blocks
                .iter()
                .position(|b| b.bbox.y0 >= bbox.y0 && b.bbox.x0 < bbox.x1 && b.bbox.x1 > bbox.x0)
                .unwrap_or(p.blocks.len());
            let text = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" · ");
            p.blocks.insert(at, markdown::OutBlock::new("paragraph", None, text, bbox, None));
        }
        p.markdown = markdown::to_markdown(&p.blocks, false);
    }
}

/// Lines in the top or bottom margin of a page, as a share of its height.
const MARGIN_ZONE: f32 = 0.08;

/// Text with digits replaced, so "Page 1 of 3" and "Page 2 of 3" match.
fn furniture_key(t: &str) -> String {
    let t: String = t.to_lowercase().chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect();
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "3", "Page 3", "Page 3 of 7", "3 / 7", "- 3 -".
fn is_page_number(key: &str) -> bool {
    key.contains('#')
        && key.chars().count() <= 20
        && key.replace("page", "").replace("of", "").chars().all(|c| matches!(c, '#' | ' ' | '/' | '-' | '–' | '|' | '.'))
}

/// Page furniture: a line repeated in the top or bottom margin of several
/// pages (a running head with the name and email, a footer) and page
/// numbers. Kept on every page it repeats between sections in the
/// document's Markdown and splits the text that runs over the page break,
/// so only its first occurrence stays, and page numbers go entirely. The raw
/// `text` keeps everything.
fn drop_page_furniture(pages: &mut [PageResult]) {
    if pages.len() < 2 {
        return;
    }
    let in_margin = |b: &markdown::OutBlock, h: f32| {
        b.kind != "table" && (b.bbox.y1 <= MARGIN_ZONE * h || b.bbox.y0 >= (1.0 - MARGIN_ZONE) * h)
    };
    let mut pages_with: std::collections::HashMap<String, std::collections::HashSet<usize>> = Default::default();
    for (pi, p) in pages.iter().enumerate() {
        for b in p.blocks.iter().filter(|b| in_margin(b, p.height)) {
            pages_with.entry(furniture_key(&b.text)).or_default().insert(pi);
        }
    }
    let mut seen = std::collections::HashSet::new();
    for p in pages.iter_mut() {
        let (h, before) = (p.height, p.blocks.len());
        p.blocks.retain(|b| {
            if !in_margin(b, h) {
                return true;
            }
            let k = furniture_key(&b.text);
            if is_page_number(&k) {
                return false;
            }
            pages_with.get(&k).map_or(0, |s| s.len()) < 2 || seen.insert(k)
        });
        if p.blocks.len() != before {
            p.markdown = markdown::to_markdown(&p.blocks, false);
        }
    }
}

/// A table that runs over a page break comes out as two tables, at the end
/// of one page and the start of the next, with the same columns and often
/// the header row repeated. It is one table: the rows are joined and a
/// repeated header row is dropped.
fn join_split_tables(pages: &mut [PageResult]) {
    for pi in 1..pages.len() {
        let (before, after) = pages.split_at_mut(pi);
        let (prev, next) = (&mut before[pi - 1], &mut after[0]);
        let (Some(a), Some(b)) = (prev.blocks.last(), next.blocks.first()) else { continue };
        let (Some(ra), Some(rb)) = (&a.rows, &b.rows) else { continue };
        let cols = |r: &Vec<Vec<String>>| r.iter().map(|x| x.len()).max().unwrap_or(0);
        if a.kind != "table" || b.kind != "table" || cols(ra) != cols(rb) || cols(ra) < 2 {
            continue;
        }
        let mut rows = ra.clone();
        let skip = (rb.first() == ra.first()) as usize;
        rows.extend(rb.iter().skip(skip).cloned());
        let text = rows.iter().map(|r| r.join(" | ")).collect::<Vec<_>>().join("\n");
        let bbox = a.bbox;
        *prev.blocks.last_mut().unwrap() = markdown::OutBlock::new("table", None, text, bbox, Some(rows));
        next.blocks.remove(0);
        prev.markdown = markdown::to_markdown(&prev.blocks, false);
        next.markdown = markdown::to_markdown(&next.blocks, false);
    }
}

/// Running heads and folios sit in the top and bottom margins. Reading order
/// would otherwise place them wherever their column is read (for example a
/// right-hand running head between the two columns), so they go first/last.
const RUNNING_HEAD_ZONE: f32 = 0.07;
const MAX_RUNNING_HEAD_CHARS: usize = 80;

fn running_heads_first(segments: Vec<Segment>, page_h: f32) -> Vec<Segment> {
    // A running head stands apart: no text directly below it (in its x
    // range) within 1.5 line heights. Otherwise it is the first line of a
    // column or paragraph that happens to start high on the page.
    let apart = |s: &Segment, below: bool| {
        let h = s.bbox.height().max(1.0);
        !segments.iter().any(|o| {
            !std::ptr::eq(o, s)
                && o.bbox.x0 < s.bbox.x1
                && o.bbox.x1 > s.bbox.x0
                && if below {
                    o.bbox.y0 >= s.bbox.y1 - 0.5 && o.bbox.y0 - s.bbox.y1 < 1.5 * h
                } else {
                    o.bbox.y1 <= s.bbox.y0 + 0.5 && s.bbox.y0 - o.bbox.y1 < 1.5 * h
                }
        })
    };
    let short = |s: &Segment| s.table.is_none() && s.text.chars().count() <= MAX_RUNNING_HEAD_CHARS;
    let is_head = |s: &Segment| short(s) && s.bbox.y1 <= RUNNING_HEAD_ZONE * page_h && apart(s, true);
    let is_foot = |s: &Segment| short(s) && s.bbox.y0 >= (1.0 - RUNNING_HEAD_ZONE) * page_h && apart(s, false);
    let kind: Vec<u8> = segments.iter().map(|s| if is_head(s) { 0 } else if is_foot(s) { 2 } else { 1 }).collect();
    let mut head = Vec::new();
    let mut body = Vec::new();
    let mut foot = Vec::new();
    for (s, k) in segments.into_iter().zip(kind) {
        match k {
            0 => head.push(s),
            2 => foot.push(s),
            _ => body.push(s),
        }
    }
    let by_position = |a: &Segment, b: &Segment| a.bbox.y0.total_cmp(&b.bbox.y0).then(a.bbox.x0.total_cmp(&b.bbox.x0));
    head.sort_by(by_position);
    foot.sort_by(by_position);
    head.extend(body);
    head.extend(foot);
    head
}

/// A page title (the largest type on the page, near the top) leads the
/// reading order with the lines directly beneath it, even when a sidebar
/// column starts higher and would otherwise be read first.
const TITLE_SIZE_RATIO: f32 = 1.5;
const TITLE_ZONE: f32 = 0.25;

fn title_first(mut segments: Vec<Segment>, page_h: f32, body_size: f32) -> Vec<Segment> {
    let Some(t) = segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.table.is_none())
        .max_by(|(_, a), (_, b)| a.size.total_cmp(&b.size))
        .map(|(i, _)| i)
    else {
        return segments;
    };
    let title = segments[t].clone();
    if t == 0 || title.size < TITLE_SIZE_RATIO * body_size || title.bbox.y0 > TITLE_ZONE * page_h {
        return segments;
    }
    // The title plus the lines that follow it directly below (subtitle,
    // contact line), in the order they are already read.
    let mut end = t + 1;
    let mut last = title.bbox;
    while end < segments.len() {
        let s = &segments[end];
        let gap = s.bbox.y0 - last.y1;
        let overlap = s.bbox.x1.min(title.bbox.x1) - s.bbox.x0.max(title.bbox.x0);
        if s.table.is_some() || overlap <= 0.0 || gap < -0.5 || gap > 1.5 * last.height().max(1.0) {
            break;
        }
        last = s.bbox;
        end += 1;
    }
    // Keep anything already placed above the title and over it (a running
    // head, a contact strip above the name) ahead of it.
    let lead = segments[..t]
        .iter()
        .take_while(|s| {
            s.bbox.y1 <= title.bbox.y0 + 0.5 && s.bbox.x1 > title.bbox.x0 && s.bbox.x0 < title.bbox.x1
        })
        .count();
    if lead >= t {
        return segments;
    }
    let group: Vec<Segment> = segments.drain(t..end).collect();
    for (k, s) in group.into_iter().enumerate() {
        segments.insert(lead + k, s);
    }
    segments
}

/// The same analysis with all raw personal data removed: block text, table
/// cells, sections, Markdown and the findings list carry placeholders only,
/// and the raw `text`, `segments`, `figures` and `hidden_text` are emptied.
/// Use this output for LLMs and other AI workloads.
pub fn analyse_redacted(pages: &[PageContent]) -> DocResult {
    redact_doc(analyse(pages))
}

/// Strip all raw personal data from an analysis (see `analyse_redacted`).
pub fn redact_doc(mut doc: DocResult) -> DocResult {
    for p in &mut doc.pages {
        for b in &mut p.blocks {
            b.redact_in_place();
        }
        p.markdown = markdown::to_markdown(&p.blocks, false);
        p.segments.clear();
        p.figures.clear();
        p.tables.clear();
        p.hidden_text.clear();
    }
    doc.markdown = doc.markdown_redacted.clone();
    doc.text = doc.markdown_redacted.clone();
    doc.sections = sections(&doc.pages);
    for s in &mut doc.sections {
        s.title = pii::redact(&s.title, &pii::detect(&s.title));
    }
    for f in &mut doc.pii {
        f.text = format!("[{}]", f.kind.to_uppercase());
    }
    for (d, label) in [(&mut doc.person.name, "[PERSON_NAME]"), (&mut doc.person.email, "[EMAIL]"), (&mut doc.person.phone, "[PHONE]")] {
        if let Some(d) = d {
            d.text = label.to_string();
        }
    }
    doc.tables.clear();
    doc.hidden_text.clear();
    doc
}

pub fn analyse(pages: &[PageContent]) -> DocResult {
    finish(pages.iter().map(analyse_page).collect())
}

/// Document-level steps shared by every input format: name and PII
/// propagation across pages, Markdown, sections and the findings list.
pub(crate) fn finish(pages: Vec<PageResult>) -> DocResult {
    let text = pages
        .iter()
        .map(|p| p.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let tables = pages.iter().flat_map(|p| p.tables.iter().map(|t| t.rows.clone())).collect();
    let mut pages = pages;
    drop_page_furniture(&mut pages);
    join_split_tables(&mut pages);
    figure_text_into_body(&mut pages);
    let (names, name) = mark_document_name(&mut pages);
    let known = pii::known();
    mark_person(&mut pages, &names, known.as_ref());
    join_wrapped_phones(&mut pages);
    mark_referees(&mut pages);
    propagate_pii(&mut pages);
    // Pages join with a blank line, not a "---" rule: a page break is where
    // the paper ended, not a break in the content, and a rule there cuts a
    // section (or a sentence) that runs on to the next page in two. Page
    // boundaries stay available in `pages`.
    let markdown = pages.iter().map(|p| p.markdown.as_str()).filter(|m| !m.is_empty()).collect::<Vec<_>>().join("\n\n");
    let markdown_redacted = pages
        .iter()
        .map(|p| markdown::to_markdown(&p.blocks, true))
        .filter(|m| !m.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let sections = sections(&pages);
    let pii = pages
        .iter()
        .enumerate()
        .flat_map(|(pi, p)| {
            p.blocks.iter().enumerate().flat_map(move |(bi, b)| {
                b.pii.iter().map(move |s| PiiFinding {
                    kind: s.kind,
                    text: s.text.clone(),
                    page: pi + 1,
                    block: bi,
                    confidence: s.confidence,
                })
            })
        })
        .collect::<Vec<PiiFinding>>();
    let hidden_text =
        pages.iter().map(|p| p.hidden_text.as_str()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n");
    let person = Person { name, email: contact(&pages, &pii, "email"), phone: contact(&pages, &pii, "phone") };
    let needs_review = review(&pages, &person);
    DocResult { pages, text, tables, markdown, markdown_redacted, sections, pii, hidden_text, person, needs_review }
}

/// The person's email or phone: the first one found, at the confidence of
/// the pattern that found it; lower when the resume gives several different
/// ones (whose is it?). One written out in the text comes before one that
/// is only a link's target (an icon's "mailto:"): the candidate shows the
/// first. The email comes back as an address (spaces a line wrap left inside
/// removed, lower case); the phone as written.
fn contact(pages: &[PageResult], pii: &[PiiFinding], kind: &str) -> Option<Detail> {
    let value = |t: &str| match kind {
        "email" => t.chars().filter(|c| !c.is_whitespace()).collect::<String>().trim_matches('.').to_lowercase(),
        _ => t.trim().to_string(),
    };
    // The same number with and without its country code ("+91 98765 43210",
    // "98765 43210") or a part of it (a number wrapped over two lines) is
    // one number.
    let key = |t: &str| match kind {
        "phone" => {
            let d: String = t.chars().filter(char::is_ascii_digit).collect();
            d[d.len().saturating_sub(10)..].to_string()
        }
        _ => value(t),
    };
    let link_only = |f: &PiiFinding| {
        let text = pages.get(f.page - 1).and_then(|p| p.blocks.get(f.block)).map_or("", |b| b.text.as_str());
        ["mailto:", "tel:"].iter().any(|scheme| text.contains(&format!("{scheme}{}", f.text)))
    };
    let mut found: Vec<&PiiFinding> = pii.iter().filter(|f| f.kind == kind).collect();
    found.sort_by_key(|f| link_only(f));
    let first = found.first()?;
    let mut distinct: Vec<String> = Vec::new();
    for k in found.iter().map(|f| key(&f.text)) {
        if !distinct.iter().any(|d| d.contains(&k) || k.contains(d.as_str())) {
            distinct.push(k);
        }
    }
    let (confidence, basis) = if distinct.len() > 1 {
        (first.confidence.min(0.7), format!("one of {} different ones, on page {}", distinct.len(), first.page))
    } else {
        (first.confidence, format!("found on page {}", first.page))
    };
    Some(Detail { text: value(&first.text), confidence, basis })
}

/// Below this confidence a name is listed for confirmation...
const CONFIRM_BELOW: f32 = 0.9;
/// ... and an email or phone: a single well-formed one is trusted (on 455
/// real resumes both agreed with the reference about 97% of the time); when
/// the resume gives several different ones, `contact` drops to 0.7.
const CONFIRM_CONTACT_BELOW: f32 = 0.8;

/// What the caller still has to do: read scans and text images (with a
/// vision model), and find or confirm details the parser is not sure of.
fn review(pages: &[PageResult], person: &Person) -> Vec<ReviewItem> {
    let mut out = Vec::new();
    for (pi, p) in pages.iter().enumerate() {
        if let Some(status) = p.ocr.as_ref().filter(|o| o.engine.is_none()) {
            out.push(ReviewItem { kind: "scanned_page", page: Some(pi + 1), bbox: None, reason: status.reason.clone(), action: "read_with_llm" });
        }
        for b in &p.unread_images {
            out.push(ReviewItem {
                kind: "image",
                page: Some(pi + 1),
                bbox: Some(*b),
                reason: "an image that looks like printed text".into(),
                action: "read_with_llm",
            });
        }
    }
    if about_a_person(pages) {
        match &person.name {
            None => out.push(ReviewItem { kind: "name", page: None, bbox: None, reason: "no name found at the top of the resume".into(), action: "find_with_llm" }),
            Some(d) if d.confidence < CONFIRM_BELOW => {
                out.push(ReviewItem { kind: "name", page: None, bbox: None, reason: d.basis.clone(), action: "confirm_with_llm" })
            }
            _ => {}
        }
        for (kind, d) in [("email", &person.email), ("phone", &person.phone)] {
            if let Some(d) = d.as_ref().filter(|d| d.confidence < CONFIRM_CONTACT_BELOW) {
                out.push(ReviewItem { kind, page: None, bbox: None, reason: d.basis.clone(), action: "confirm_with_llm" });
            }
        }
    }
    out
}
