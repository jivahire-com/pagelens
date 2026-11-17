//! Formats that already carry their structure (DOCX, PPTX, HTML): their
//! readers produce logical blocks (headings, paragraphs, list items,
//! tables), which skip the geometric layout analysis that PDFs need and go
//! through the same block, PII, Markdown and section steps. Positions are
//! not known for these formats, so every `bbox` is zero.

use std::path::Path;

use crate::markdown::{self, OutBlock};
use crate::model::{BBox, Segment};
use crate::tables::Table;
use crate::{DocResult, PageResult};

pub mod docx;
pub mod html;
pub mod pptx;
mod xml;

/// One logical block from a structured document.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading(u8, String),
    Paragraph(String),
    ListItem(String),
    Table(Vec<Vec<String>>),
}

/// Input formats handled here, by file extension.
pub fn is_structured_file(path: &Path) -> bool {
    matches!(ext(path).as_deref(), Some("docx" | "pptx" | "html" | "htm" | "xhtml"))
}

fn ext(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase())
}

/// Read a DOCX, PPTX or HTML file into pages of blocks (one page for DOCX
/// and HTML, one per slide for PPTX).
pub fn load(path: &Path) -> Result<Vec<Vec<Block>>, String> {
    let ctx = |e: String| format!("{}: {e}", path.display());
    match ext(path).as_deref() {
        Some("docx") => docx::read(path).map(|b| vec![b]).map_err(ctx),
        Some("pptx") => pptx::read(path).map_err(ctx),
        Some("html" | "htm" | "xhtml") => {
            let bytes = std::fs::read(path).map_err(|e| ctx(e.to_string()))?;
            Ok(vec![html::read(&String::from_utf8_lossy(&bytes))])
        }
        _ => Err(ctx("unsupported format".into())),
    }
}

/// Analyse pages of logical blocks into the same result as a PDF.
pub fn analyse_blocks(pages: Vec<Vec<Block>>) -> DocResult {
    crate::finish(pages.into_iter().map(page_result).collect())
}

fn page_result(blocks: Vec<Block>) -> PageResult {
    let zero = BBox { x0: 0.0, y0: 0.0, x1: 0.0, y1: 0.0 };
    let mut out_blocks = Vec::new();
    let mut segments = Vec::new();
    let mut tables = Vec::new();
    let seg = |text: String, table: Option<usize>| Segment { text, bbox: zero, size: 0.0, bold: false, table };
    for b in blocks {
        match b {
            Block::Heading(level, text) => {
                segments.push(seg(text.clone(), None));
                out_blocks.push(OutBlock::new("heading", Some(level.clamp(1, 6)), text, zero, None));
            }
            Block::Paragraph(text) => {
                segments.push(seg(text.clone(), None));
                out_blocks.push(OutBlock::new("paragraph", None, text, zero, None));
            }
            Block::ListItem(text) => {
                segments.push(seg(text.clone(), None));
                out_blocks.push(OutBlock::new("list_item", None, text, zero, None));
            }
            Block::Table(rows) => {
                let rows = square(rows);
                if rows.is_empty() {
                    continue;
                }
                for r in &rows {
                    segments.push(seg(r.join(" | "), Some(tables.len())));
                }
                let text = rows.iter().map(|r| r.join(" | ")).collect::<Vec<_>>().join("\n");
                out_blocks.push(OutBlock::new("table", None, text, zero, Some(rows.clone())));
                tables.push(Table { bbox: zero, rows, row_boxes: Vec::new() });
            }
        }
    }
    let markdown = markdown::to_markdown(&out_blocks, false);
    PageResult {
        width: 0.0,
        height: 0.0,
        segments,
        tables,
        figures: Vec::new(),
        markdown,
        blocks: out_blocks,
        hidden_text: String::new(),
        ocr: None,
        unread_images: Vec::new(),
    }
}

/// Pad rows to the same number of cells and drop fully empty rows.
fn square(rows: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    rows.into_iter()
        .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
        .map(|mut r| {
            r.resize(width, String::new());
            r
        })
        .collect()
}

/// Collapse runs of whitespace into single spaces and trim.
pub(crate) fn tidy(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Read one member of a zip container (DOCX, PPTX) as text.
pub(crate) fn zip_text<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<String> {
    use std::io::Read;
    let mut f = archive.by_name(name).ok()?;
    // Guard against zip bombs: Office parts are a few MB at most.
    if f.size() > 256 * 1024 * 1024 {
        return None;
    }
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}
