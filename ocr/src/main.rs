//! pagelens-ocr: pagelens, plus local OCR for what the parser cannot read.
//!
//! The parser lists scanned pages and images that contain text in
//! `needs_review`. This program reads exactly those with PP-OCRv6 tiny
//! (PaddleOCR, run in pure Rust on the CPU; nothing leaves the machine),
//! puts each line back at its place on the page and runs the parser's
//! normal analysis. Columns, sections, tables, personal data, `person` and
//! `--redact` then cover scanned text as they do a digital PDF. Documents
//! with nothing to read come out exactly as `pagelens` gives them.
//!
//! An email or phone number on a line read with low confidence is listed in
//! `needs_review` to confirm.
//!
//! usage: pagelens-ocr [--redact] [--known details.json] <file> [more ...]

mod place;
mod ppocr;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pagelens::model::{BBox, PageContent};
use pagelens::ocr::OcrStatus;
use pagelens::ReviewItem;
use pdfium_render::prelude::*;

const USAGE: &str = "usage: pagelens-ocr [--redact] [--known details.json] <file.pdf|image|.docx|.pptx|.html> [more ...]";

/// Scanned pages are rendered at this resolution (capped by the engine's
/// 2,000 px), images inside digital pages at the same.
const RENDER_DPI: f32 = 200.0;
/// Below this line confidence, an email or phone on the line is listed to confirm.
const CONFIRM_BELOW: f32 = 0.9;

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("pagelens-ocr {} ({})", env!("CARGO_PKG_VERSION"), ppocr::ENGINE_NAME);
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}\n\nOutput: one JSON object per input file, one per line, as from pagelens.");
        return ExitCode::SUCCESS;
    }
    if let Some(i) = args.iter().position(|a| a == "--known") {
        let Some(path) = args.get(i + 1).cloned() else {
            eprintln!("--known needs a JSON file\n{USAGE}");
            return ExitCode::from(2);
        };
        let known = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<pagelens::pii::Known>(&t).map_err(|e| e.to_string()));
        match known {
            Ok(k) => pagelens::pii::set_known(Some(k)),
            Err(e) => {
                eprintln!("known details {path}: {e}");
                return ExitCode::from(2);
            }
        }
        args.drain(i..i + 2);
    }
    let redact = args.iter().any(|a| a == "--redact");
    args.retain(|a| a != "--redact");
    if let Some(bad) = args.iter().find(|a| a.starts_with("--")) {
        eprintln!("unknown option {bad}\n{USAGE}");
        return ExitCode::from(2);
    }
    if args.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    if redact {
        pagelens::backend::enable_visibility_check();
    }
    let pdfium = match pagelens::backend::bind_default(std::env::var_os("PDFIUM_LIB_DIR").map(PathBuf::from).as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut failed = false;
    for path in &args {
        match process(&pdfium, Path::new(path), redact) {
            Ok(doc) => {
                serde_json::to_writer(&mut out, &doc).expect("serialisable");
                writeln!(out).ok();
            }
            Err(e) => {
                eprintln!("cannot read {path}: {e}");
                failed = true;
                if args.len() > 1 {
                    writeln!(out, "{{\"error\":{}}}", serde_json::to_string(&e).unwrap()).ok();
                }
            }
        }
        out.flush().ok();
    }
    if failed { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn process(pdfium: &Pdfium, path: &Path, redact: bool) -> Result<pagelens::DocResult, String> {
    if pagelens::structured::is_structured_file(path) {
        let doc = pagelens::structured::analyse_blocks(pagelens::structured::load(path)?);
        return Ok(if redact { pagelens::redact_doc(doc) } else { doc });
    }
    let image_input = pagelens::backend::is_image_file(path);
    // Plain parse (no OCR): what is there to read?
    let mut pages = if image_input {
        image_page(path)?
    } else {
        pagelens::backend::load_pages_with_ocr(pdfium, path, None).map_err(|e| e.to_string())?
    };
    let todo: Vec<(usize, Vec<BBox>)> = pages
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let whole = p.ocr.as_ref().is_some_and(|s| s.engine.is_none());
            let regions = if whole { vec![BBox { x0: 0.0, y0: 0.0, x1: p.width, y1: p.height }] } else { p.unread_images.clone() };
            (!regions.is_empty()).then_some((i, regions))
        })
        .collect();
    if todo.is_empty() {
        let doc = pagelens::analyse(&pages);
        return Ok(if redact { pagelens::redact_doc(doc) } else { doc });
    }

    let rendered = if image_input { None } else { Some(pdfium.load_pdf_from_file(path, None).map_err(|e| e.to_string())?) };
    let source_image = if image_input { Some(image::open(path).map_err(|e| e.to_string())?.to_rgb8()) } else { None };
    let mut doubtful: Vec<(&'static str, usize)> = Vec::new();
    for (index, regions) in &todo {
        let page = &mut pages[*index];
        let whole = page.ocr.is_some();
        if whole {
            // The scan's own image goes; its text comes back as glyphs.
            page.glyphs.clear();
            page.images.clear();
        }
        let mut words = 0;
        for region in regions {
            let (img, px_per_pt) = match (&rendered, &source_image) {
                (Some(doc), _) => render_region(doc, *index, page, region)?,
                (None, Some(img)) => (img.clone(), 1.0),
                _ => unreachable!(),
            };
            let started = std::time::Instant::now();
            let lines = ppocr::read(&img)?;
            eprintln!("page {}: read {} lines in {:.1} s", index + 1, lines.len(), started.elapsed().as_secs_f32());
            for l in &lines {
                if l.score < CONFIRM_BELOW {
                    let t = l.text();
                    if t.contains('@') {
                        doubtful.push(("email", index + 1));
                    } else if t.chars().filter(char::is_ascii_digit).count() >= 10 {
                        doubtful.push(("phone", index + 1));
                    }
                }
            }
            words += lines.iter().map(|l| l.text().split_whitespace().count()).sum::<usize>();
            page.glyphs.extend(place::glyphs(&lines, px_per_pt, (region.x0, region.y0)));
            if !whole {
                page.ocr_images.push(*region);
            }
        }
        if whole {
            page.ocr = Some(OcrStatus {
                reason: page.ocr.as_ref().map(|s| s.reason.clone()).unwrap_or_default(),
                engine: Some(ppocr::ENGINE_NAME.to_string()),
                words,
            });
        }
        page.unread_images.clear();
    }
    let doc = pagelens::analyse(&pages);
    let mut doc = if redact { pagelens::redact_doc(doc) } else { doc };
    doubtful.dedup();
    for (kind, page) in doubtful {
        doc.needs_review.push(ReviewItem {
            kind,
            page: Some(page),
            bbox: None,
            reason: format!("the {kind} on this page was read with low confidence; confirm it from the page"),
            action: "confirm_with_llm",
        });
    }
    Ok(doc)
}

/// An image file as one page to read, in points at 72 dpi (one pixel each).
fn image_page(path: &Path) -> Result<Vec<PageContent>, String> {
    let (w, h) = image::image_dimensions(path).map_err(|e| e.to_string())?;
    Ok(vec![PageContent {
        width: w as f32,
        height: h as f32,
        ocr: Some(OcrStatus { reason: "image file".into(), engine: None, words: 0 }),
        ..Default::default()
    }])
}

/// One region of a page as pixels, and the pixels per point it was drawn at.
fn render_region(doc: &PdfDocument, index: usize, page: &PageContent, region: &BBox) -> Result<(image::RgbImage, f32), String> {
    let scale = RENDER_DPI / 72.0;
    let pdf_page = doc.pages().iter().nth(index).ok_or("page out of range")?;
    let img = pdf_page
        .render_with_config(&PdfRenderConfig::new().scale_page_by_factor(scale))
        .map_err(|e| e.to_string())?
        .as_image()
        .map_err(|e| e.to_string())?
        .to_rgb8();
    let (sx, sy) = (img.width() as f32 / page.width.max(1.0), img.height() as f32 / page.height.max(1.0));
    let x = (region.x0 * sx).max(0.0) as u32;
    let y = (region.y0 * sy).max(0.0) as u32;
    let w = ((region.width() * sx) as u32).min(img.width().saturating_sub(x)).max(1);
    let h = ((region.height() * sy) as u32).min(img.height().saturating_sub(y)).max(1);
    Ok((image::imageops::crop_imm(&img, x, y, w, h).to_image(), sx))
}
