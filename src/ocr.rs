//! OCR fallback for pages without a usable text layer.
//!
//! Detection is cheap and always on. Recognition runs only on pages that
//! need it, so born-digital pages stay fast. Engines:
//!   * builtin: ocrs (pure Rust, MIT/Apache-2.0) compiled into this binary
//!     with its models embedded; loaded once per process. Only in builds
//!     with the `ocr` feature, which the default build leaves out.
//!   * tesseract: the external `tesseract` program, if installed (more
//!     languages, often better on small digits).
//!   * ocrs: the external `ocrs` program.
//! Select with PAGELENS_OCR = auto (default: builtin if built in, else
//! none) | builtin | tesseract | ocrs | off. With no engine, pages and
//! images that need reading are listed in the result's `needs_review`.
//! Recognised words become glyphs, so OCR pages go through the same layout
//! and table pipeline as born-digital pages.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::model::{BBox, Glyph, PageContent};

/// Pages with fewer real characters than this may be scans.
pub const MIN_TEXT_CHARS: usize = 30;
/// A near-textless page with at least this share of dark pixels has
/// visible content that only OCR can read.
pub const MIN_INK_SHARE: f32 = 0.01;
/// ... if images cover at least this share of the page,
const SCAN_IMAGE_SHARE: f32 = 0.25;
/// ... or the page is drawn with at least this many vector paths
/// (text converted to outlines).
const OUTLINED_TEXT_PATHS: usize = 150;
/// Share of undecodable characters above which the text layer is unusable.
const MAX_BAD_SHARE: f32 = 0.3;
/// Render resolution for OCR, capped by `MAX_RENDER_PX` on the long side.
const OCR_DPI: f32 = 250.0;
const MAX_RENDER_PX: f32 = 3500.0;
/// Single-character OCR words shorter than this share of the median word
/// height are dropped as noise.
const NOISE_HEIGHT_SHARE: f32 = 0.75;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct OcrStatus {
    /// Why the page needs OCR.
    pub reason: String,
    /// Engine that read the page, or None if no OCR engine was available.
    pub engine: Option<String>,
    pub words: usize,
}

/// Number of visible (non-space) characters in the text layer.
pub fn text_chars(page: &PageContent) -> usize {
    page.glyphs.iter().filter(|g| !g.space).count()
}

/// Why this page needs OCR, if it does.
pub fn needs_ocr(page: &PageContent) -> Option<&'static str> {
    let visible: Vec<&Glyph> = page.glyphs.iter().filter(|g| !g.space).collect();
    let page_area = (page.width * page.height).max(1.0);
    let image_share = page.images.iter().map(|b| b.area()).sum::<f32>() / page_area;
    if visible.len() < MIN_TEXT_CHARS {
        if image_share >= SCAN_IMAGE_SHARE {
            return Some("scanned page (image without text layer)");
        }
        if page.path_count >= OUTLINED_TEXT_PATHS {
            return Some("text drawn as vector outlines");
        }
        return None;
    }
    let bad = visible.iter().filter(|g| is_undecodable(g.ch)).count();
    if bad as f32 > MAX_BAD_SHARE * visible.len() as f32 {
        return Some("text layer has no usable character mapping");
    }
    None
}

fn is_undecodable(c: char) -> bool {
    // U+FFFD replacement, C0/C1 controls, and private-use code points other
    // than the common Word symbol-font bullets.
    let pua = ('\u{E000}'..='\u{F8FF}').contains(&c) && !matches!(c, '\u{F0B7}' | '\u{F0A7}' | '\u{F0D8}' | '\u{F0FC}');
    c == '\u{FFFD}' || c.is_control() || pua
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Engine {
    Builtin,
    Tesseract,
    Ocrs,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Engine::Builtin => "builtin-ocrs",
            Engine::Tesseract => "tesseract",
            Engine::Ocrs => "ocrs",
        }
    }
}

/// Whether large images on digital pages are read for text
/// (PAGELENS_OCR_IMAGES=on|off, default on).
pub fn image_ocr_enabled() -> bool {
    !matches!(std::env::var("PAGELENS_OCR_IMAGES").as_deref(), Ok("off") | Ok("0"))
}

/// OCR engine chosen by PAGELENS_OCR, or None when OCR is off/unavailable.
pub fn select_engine() -> Option<Engine> {
    let choice = std::env::var("PAGELENS_OCR").unwrap_or_else(|_| "auto".into());
    match choice.as_str() {
        "off" | "none" => None,
        "tesseract" => on_path("tesseract").then_some(Engine::Tesseract),
        "ocrs" => on_path("ocrs").then_some(Engine::Ocrs),
        // "auto" never picks up whatever OCR program happens to be
        // installed: without the built-in engine (off in the default
        // build), scans and text images are listed in `needs_review` for
        // the caller to read, for example with a vision model.
        _ if cfg!(feature = "ocr") => Some(Engine::Builtin),
        _ => None,
    }
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            let exe = format!("{bin}{}", std::env::consts::EXE_SUFFIX);
            std::env::split_paths(&paths).any(|d| d.join(&exe).is_file())
        })
        .unwrap_or(false)
}

/// Pixel scale for rendering a page of `w` x `h` points.
pub fn render_scale(w: f32, h: f32) -> f32 {
    let dpi = std::env::var("PAGELENS_OCR_DPI").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(OCR_DPI);
    let scale = dpi / 72.0;
    let long = w.max(h) * scale;
    if long > MAX_RENDER_PX { MAX_RENDER_PX / w.max(h) } else { scale }
}

/// A recognised word in pixel coordinates.
struct Word {
    text: String,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

/// Run an external OCR program on a rendered page image file.
pub fn recognise(engine: Engine, image: &Path, px_per_pt: f32) -> Result<Vec<Glyph>, String> {
    let words = match engine {
        Engine::Ocrs => run_ocrs(image)?,
        Engine::Tesseract => run_tesseract(image, px_per_pt * 72.0)?,
        Engine::Builtin => return Err("builtin engine takes pixels, not a file".into()),
    };
    Ok(words_to_glyphs(words, px_per_pt))
}

/// Share of pixels darker than the ink level auto-levels picks, and lighter
/// than its paper level.
#[cfg(any(feature = "ocr", test))]
const LEVELS_INK_SHARE: f32 = 0.01;
#[cfg(any(feature = "ocr", test))]
const LEVELS_PAPER_SHARE: f32 = 0.005;
/// Images already using at least this much of the 0-255 range are left as is.
#[cfg(any(feature = "ocr", test))]
const LEVELS_MIN_RANGE: u8 = 200;

/// Stretch the image so that its darkest ink is black and its paper white.
/// Screenshots and faint scans often have light grey text on a white page:
/// on 150-160 dpi resume scans of this kind the recogniser read about half
/// the words as non-words; stretching the contrast first raised the share
/// of real words from 0.47 to 0.59 and found 20% more words. A
/// well-exposed scan already spans the range and is not touched.
/// The same linear map is applied to all three channels.
#[cfg(any(feature = "ocr", test))]
fn auto_levels(rgb: &[u8]) -> Option<Vec<u8>> {
    let mut hist = [0usize; 256];
    for px in rgb.chunks_exact(3) {
        let l = (px[0] as u32 * 299 + px[1] as u32 * 587 + px[2] as u32 * 114) / 1000;
        hist[l as usize] += 1;
    }
    let total: usize = hist.iter().sum();
    if total == 0 {
        return None;
    }
    let level = |share: f32, from_dark: bool| -> u8 {
        let target = (share * total as f32) as usize;
        let mut acc = 0;
        let order: Box<dyn Iterator<Item = usize>> = if from_dark { Box::new(0..256) } else { Box::new((0..256).rev()) };
        for v in order {
            acc += hist[v];
            if acc > target {
                return v as u8;
            }
        }
        if from_dark { 0 } else { 255 }
    };
    let (ink, paper) = (level(LEVELS_INK_SHARE, true), level(LEVELS_PAPER_SHARE, false));
    if paper <= ink || paper - ink >= LEVELS_MIN_RANGE {
        return None;
    }
    let scale = 255.0 / (paper - ink) as f32;
    let lut: Vec<u8> = (0..256).map(|v| ((v as f32 - ink as f32) * scale).round().clamp(0.0, 255.0) as u8).collect();
    Some(rgb.iter().map(|&v| lut[v as usize]).collect())
}

/// Run the built-in engine on an RGB image held in memory.
#[cfg(feature = "ocr")]
pub fn recognise_pixels(rgb: &[u8], width: u32, height: u32, px_per_pt: f32) -> Result<Vec<Glyph>, String> {
    let levelled = auto_levels(rgb);
    let rgb = levelled.as_deref().unwrap_or(rgb);
    Ok(words_to_glyphs(builtin::recognise(rgb, width, height)?, px_per_pt))
}

#[cfg(feature = "ocr")]
mod builtin {
    use std::sync::OnceLock;

    use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};

    use super::Word;

    static DETECTION: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/models/ocrs/text-detection.onnx"));
    static RECOGNITION: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/models/ocrs/text-recognition.onnx"));

    fn engine() -> Result<&'static OcrEngine, String> {
        static ENGINE: OnceLock<Result<OcrEngine, String>> = OnceLock::new();
        ENGINE
            .get_or_init(|| {
                let detection = rten::Model::load_static_slice(DETECTION).map_err(|e| e.to_string())?;
                let recognition = rten::Model::load_static_slice(RECOGNITION).map_err(|e| e.to_string())?;
                OcrEngine::new(OcrEngineParams {
                    detection_model: Some(detection),
                    recognition_model: Some(recognition),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|e| e.clone())
    }

    pub fn recognise(rgb: &[u8], width: u32, height: u32) -> Result<Vec<Word>, String> {
        let engine = engine()?;
        let source = ImageSource::from_bytes(rgb, (width, height)).map_err(|e| e.to_string())?;
        let input = engine.prepare_input(source).map_err(|e| e.to_string())?;
        let words = engine.detect_words(&input).map_err(|e| e.to_string())?;
        let lines = engine.find_text_lines(&input, &words);
        let recognised = engine.recognize_text(&input, &lines).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for line in recognised.into_iter().flatten() {
            for word in line.words() {
                let text: String = word.chars().iter().map(|c| c.char).collect();
                if text.trim().is_empty() {
                    continue;
                }
                let r = word.bounding_rect();
                out.push(Word {
                    text,
                    x0: r.left() as f32,
                    y0: r.top() as f32,
                    x1: r.right() as f32,
                    y1: r.bottom() as f32,
                });
            }
        }
        Ok(out)
    }
}

fn words_to_glyphs(words: Vec<Word>, px_per_pt: f32) -> Vec<Glyph> {
    // Median word height, to spot stray detections (specks, rule ends).
    let mut heights: Vec<f32> = words.iter().map(|w| w.y1 - w.y0).collect();
    heights.sort_by(|a, b| a.total_cmp(b));
    let median_h = heights.get(heights.len() / 2).copied().unwrap_or(0.0);

    let all = &words;
    let mut glyphs = Vec::new();
    for w in all {
        let chars: Vec<char> = w.text.chars().filter(|c| !c.is_whitespace()).collect();
        if chars.is_empty() {
            continue;
        }
        // A lone character much smaller than the text around it, or alone on
        // its line (and not a digit, which may be a page number), is noise;
        // left in, it splits table rows and paragraphs apart.
        if chars.len() == 1 {
            let small = (w.y1 - w.y0) < NOISE_HEIGHT_SHARE * median_h;
            let cy = (w.y0 + w.y1) / 2.0;
            let alone = !all.iter().any(|o| {
                !std::ptr::eq(o, w) && ((o.y0 + o.y1) / 2.0 - cy).abs() < 0.5 * (w.y1 - w.y0).max(o.y1 - o.y0)
            });
            if small || (alone && !chars[0].is_ascii_digit()) {
                continue;
            }
        }
        let (x0, y0, x1, y1) = (w.x0 / px_per_pt, w.y0 / px_per_pt, w.x1 / px_per_pt, w.y1 / px_per_pt);
        let step = (x1 - x0) / chars.len() as f32;
        // OCR word boxes hug the ink: about 0.72x the font size for mixed text.
        let size = ((y1 - y0) / 0.72).max(1.0);
        for (k, ch) in chars.into_iter().enumerate() {
            let bbox = BBox { x0: x0 + step * k as f32, y0, x1: x0 + step * (k as f32 + 1.0), y1 };
            glyphs.push(Glyph { ch, bbox, size, space: false, bold: false });
        }
    }
    glyphs
}

fn run_ocrs(image: &Path) -> Result<Vec<Word>, String> {
    let out = Command::new("ocrs").arg("--json").arg(image).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let mut words = Vec::new();
    for para in v["paragraphs"].as_array().into_iter().flatten() {
        for line in para["lines"].as_array().into_iter().flatten() {
            for w in line["words"].as_array().into_iter().flatten() {
                let (Some(text), Some(vs)) = (w["text"].as_str(), w["vertices"].as_array()) else { continue };
                let pts: Vec<(f32, f32)> = vs
                    .iter()
                    .filter_map(|p| Some((p[0].as_f64()? as f32, p[1].as_f64()? as f32)))
                    .collect();
                if pts.is_empty() {
                    continue;
                }
                let x0 = pts.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
                let x1 = pts.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
                let y0 = pts.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
                let y1 = pts.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
                words.push(Word { text: text.to_string(), x0, y0, x1, y1 });
            }
        }
    }
    Ok(words)
}

fn run_tesseract(image: &Path, dpi: f32) -> Result<Vec<Word>, String> {
    let out = Command::new("tesseract")
        .arg(image)
        .arg("stdout")
        .args(["--dpi", &format!("{}", dpi.round() as i32), "tsv"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    // level page_num block_num par_num line_num word_num left top width height conf text
    let mut words = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 12 || f[0] != "5" || f[11].trim().is_empty() {
            continue;
        }
        let num = |i: usize| f[i].parse::<f32>().unwrap_or(0.0);
        if num(10) < 0.0 {
            continue;
        }
        let (l, t, w, h) = (num(6), num(7), num(8), num(9));
        words.push(Word { text: f[11].to_string(), x0: l, y0: t, x1: l + w, y1: t + h });
    }
    Ok(words)
}

/// Unique temporary file path for a rendered page image.
pub fn temp_image_path(page_index: usize) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    std::env::temp_dir().join(format!("pagelens_{}_{}_{}.png", std::process::id(), page_index, nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_levels_stretches_faint_images_only() {
        // Light grey "ink" (170) on white: stretched so the ink becomes black.
        let mut faint = vec![255u8; 3 * 1000];
        faint[..3 * 20].fill(170);
        let out = auto_levels(&faint).expect("faint image is stretched");
        assert_eq!(out[0], 0);
        assert_eq!(out[3 * 999], 255);
        // Black ink on white already spans the range: left alone.
        let mut crisp = vec![255u8; 3 * 1000];
        crisp[..3 * 20].fill(10);
        assert!(auto_levels(&crisp).is_none());
    }
}
