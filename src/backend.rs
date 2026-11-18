//! PDFium backend: decodes glyphs and vector shapes into the neutral model.
//! PDFium only supplies characters and geometry; all layout logic is ours.

use std::path::Path;

use pdfium_render::prelude::*;

use crate::model::{BBox, Fill, Glyph, PageContent};

/// Minimum side length (points) for a filled shape to count as a container.
const MIN_CONTAINER_SIDE: f32 = 20.0;
/// Maximum thickness (points) for a shape to count as a table rule.
const MAX_RULE_THICKNESS: f32 = 2.0;
/// Text smaller than this (points) is not readable; treated as hidden.
const MIN_VISIBLE_SIZE: f32 = 1.0;

pub fn bind(lib_dir: &Path) -> Result<Pdfium, PdfiumError> {
    let bindings = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(lib_dir))?;
    Ok(Pdfium::new(bindings))
}

/// PDFium compiled into this binary (feature `embed-pdfium`). On first use
/// it is written to a per-version cache folder and loaded from there, so the
/// engine ships as a single file.
#[cfg(feature = "embed-pdfium")]
static EMBEDDED_PDFIUM: &[u8] = include_bytes!(env!("PAGELENS_PDFIUM_LIB"));

/// Image files read through OCR (no PDF involved).
pub fn is_image_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp" | "webp")
    )
}

/// Assumed resolution of an image file when converting pixels to points.
#[cfg(feature = "ocr")]
const IMAGE_DPI: f32 = 200.0;

/// A page from an image file: the whole image is OCR'd with the built-in
/// engine and goes through the same layout pipeline as a PDF page.
#[cfg(feature = "ocr")]
pub fn load_image(path: &Path) -> Result<Vec<PageContent>, String> {
    let img = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // Very large images are scaled down to the OCR working size; positions
    // stay in points of the original image (assumed IMAGE_DPI).
    let orig_w = img.width() as f32;
    let long = img.width().max(img.height()) as f32;
    let img = if long > 3500.0 {
        let f = 3500.0 / long;
        img.resize((img.width() as f32 * f) as u32, (img.height() as f32 * f) as u32, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let px_per_pt = IMAGE_DPI / 72.0 * (img.width() as f32 / orig_w);
    let rgb = img.to_rgb8();
    let glyphs = crate::ocr::recognise_pixels(rgb.as_raw(), rgb.width(), rgb.height(), px_per_pt)?;
    let words = glyphs.len();
    Ok(vec![PageContent {
        width: rgb.width() as f32 / px_per_pt,
        height: rgb.height() as f32 / px_per_pt,
        glyphs,
        ocr: Some(crate::ocr::OcrStatus { reason: "image file".into(), engine: Some("builtin-ocrs".into()), words }),
        ..Default::default()
    }])
}

/// Load PDFium: an explicit directory if given, else the embedded copy.
pub fn bind_default(explicit_dir: Option<&Path>) -> Result<Pdfium, String> {
    if let Some(dir) = explicit_dir {
        return bind(dir).map_err(|e| format!("cannot load PDFium from {}: {e}", dir.display()));
    }
    #[cfg(feature = "embed-pdfium")]
    {
        let dir = embedded_pdfium_dir()?;
        return bind(&dir).map_err(|e| format!("cannot load embedded PDFium: {e}"));
    }
    #[allow(unreachable_code)]
    Err("no PDFium library: set PDFIUM_LIB_DIR or build with the embed-pdfium feature".into())
}

#[cfg(feature = "embed-pdfium")]
fn embedded_pdfium_dir() -> Result<std::path::PathBuf, String> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    EMBEDDED_PDFIUM.len().hash(&mut h);
    EMBEDDED_PDFIUM[..EMBEDDED_PDFIUM.len().min(4096)].hash(&mut h);
    // XDG cache on Linux, ~/.cache on macOS, %LOCALAPPDATA% on Windows;
    // the temp folder as a last resort.
    let base = std::env::var_os("PAGELENS_CACHE_DIR")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(std::path::PathBuf::from)
        .or_else(|| cfg!(windows).then(|| std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)).flatten())
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("pagelens").join(format!("pdfium-{:016x}", h.finish()));
    let file = Pdfium::pdfium_platform_library_name_at_path(&dir);
    let fresh = std::fs::metadata(&file).map(|m| m.len() as usize == EMBEDDED_PDFIUM.len()).unwrap_or(false);
    if !fresh {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // Write to a temporary name, then rename: safe with parallel workers.
        let tmp = dir.join(format!(".pdfium-{}.tmp", std::process::id()));
        std::fs::write(&tmp, EMBEDDED_PDFIUM).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &file).map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

pub fn load_pages(pdfium: &Pdfium, path: &Path) -> Result<Vec<PageContent>, PdfiumError> {
    load_pages_with_ocr(pdfium, path, crate::ocr::select_engine())
}

/// Read every page; pages without a usable text layer are sent to `ocr`.
pub fn load_pages_with_ocr(
    pdfium: &Pdfium,
    path: &Path,
    ocr: Option<crate::ocr::Engine>,
) -> Result<Vec<PageContent>, PdfiumError> {
    let doc = pdfium.load_pdf_from_file(path, None)?;
    let mut pages = Vec::new();
    for (index, page) in doc.pages().iter().enumerate() {
        let mut content = read_page(&page)?;
        // The pixel check runs when asked for, and on any page with white
        // text whose background is unknown (cheap: such pages are rare).
        if check_rendered_visibility() || content.white_suspects > 0 {
            hide_undrawn_glyphs(&page, &mut content);
        }
        let reason = crate::ocr::needs_ocr(&content).or_else(|| {
            // Almost no text: is anything visible? Text can hide in nested
            // drawing groups that object counts do not see.
            (crate::ocr::text_chars(&content) < crate::ocr::MIN_TEXT_CHARS
                && ink_share(&page).unwrap_or(0.0) >= crate::ocr::MIN_INK_SHARE)
                .then_some("visible content without a text layer")
        });
        if let Some(reason) = reason {
            let mut status = crate::ocr::OcrStatus { reason: reason.to_string(), engine: None, words: 0 };
            if let Some(engine) = ocr {
                match ocr_page(&page, &content, engine, index) {
                    Ok((glyphs, rules)) => {
                        status.engine = Some(engine.name().to_string());
                        status.words = glyphs.len();
                        content.glyphs = glyphs;
                        content.rules.extend(rules);
                        // The scan image is the page itself, not a figure.
                        content.images.clear();
                    }
                    Err(e) => eprintln!("OCR failed on page {}: {e}", index + 1),
                }
            }
            content.ocr = Some(status);
        } else if let Some(engine) = ocr.filter(|_| crate::ocr::image_ocr_enabled()) {
            ocr_text_images(&page, &mut content, engine);
        } else if ocr.is_none() {
            content.unread_images = unread_text_images(&page, &content);
        }
        pages.push(content);
    }
    Ok(pages)
}

/// Rendered-visibility check (finds clipped or covered text). On for
/// redaction and rendering, or with PAGELENS_VISIBILITY=on; plain parsing
/// runs it only on pages with white text of unknown background. Costs one
/// low-resolution render per checked page (about 10 ms).
fn check_rendered_visibility() -> bool {
    match std::env::var("PAGELENS_VISIBILITY").as_deref() {
        Ok("on") | Ok("1") => true,
        Ok("off") | Ok("0") => false,
        _ => VISIBILITY_FORCED.load(std::sync::atomic::Ordering::Relaxed),
    }
}

static VISIBILITY_FORCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turn the pixel visibility check on for every page of this process (used
/// by redaction and rendering, where hidden text must never slip through).
pub fn enable_visibility_check() {
    VISIBILITY_FORCED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Render resolution (dots per inch) for the visibility check.
const VISIBILITY_DPI: f32 = 100.0;
/// Minimum luminance range inside a glyph box for the glyph to count as drawn.
const MIN_GLYPH_CONTRAST: u8 = 12;

/// Moves glyphs that leave no mark on the rendered page (clipped away,
/// covered by a shape or image, or the same colour as their background) to
/// `hidden_glyphs`. One low-resolution render per page.
fn hide_undrawn_glyphs(page: &PdfPage, content: &mut PageContent) {
    if content.glyphs.is_empty() {
        return;
    }
    let scale = VISIBILITY_DPI / 72.0;
    let Ok(image) = page
        .render_with_config(&PdfRenderConfig::new().scale_page_by_factor(scale))
        .and_then(|b| b.as_image())
    else {
        return;
    };
    let gray = image.to_luma8();
    let (w, h) = (gray.width() as i64, gray.height() as i64);
    let drawn = |g: &Glyph| {
        // The glyph box plus one pixel of tolerance.
        let px = |v: f32| (v * scale).round() as i64;
        let (x0, x1) = (px(g.bbox.x0) - 1, px(g.bbox.x1) + 1);
        let (y0, y1) = (px(g.bbox.y0 + 0.15 * g.bbox.height()) - 1, px(g.bbox.y1) + 1);
        if x0 >= w || y0 >= h || x1 <= 0 || y1 <= 0 {
            return true; // off the rendered area: not our call
        }
        let (mut lo, mut hi) = (255u8, 0u8);
        for y in y0.max(0)..y1.min(h) {
            for x in x0.max(0)..x1.min(w) {
                let v = gray.get_pixel(x as u32, y as u32).0[0];
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        hi.saturating_sub(lo) >= MIN_GLYPH_CONTRAST
    };
    let marks: Vec<bool> = content.glyphs.iter().map(|g| g.space || !g.ch.is_alphanumeric() || drawn(g)).collect();
    // Decide per word: a glyph counts as hidden only when nothing drawn sits
    // next to it on its line (thin letters inside visible words stay).
    let visible: Vec<bool> = content
        .glyphs
        .iter()
        .enumerate()
        .map(|(i, g)| {
            marks[i]
                || content.glyphs.iter().enumerate().any(|(j, o)| {
                    j != i
                        && marks[j]
                        && !o.space
                        && o.ch.is_alphanumeric()
                        && (o.bbox.cy() - g.bbox.cy()).abs() < 0.4 * g.bbox.height().max(1.0)
                        && (o.bbox.x0 - g.bbox.x1).max(g.bbox.x0 - o.bbox.x1) < 0.6 * g.size.max(1.0)
                })
        })
        .collect();
    let mut keep = Vec::with_capacity(content.glyphs.len());
    let mut gone = Vec::new();
    for (g, v) in std::mem::take(&mut content.glyphs).into_iter().zip(visible) {
        if v { keep.push(g) } else { gone.push(g) }
    }
    // Spaces between hidden words would be orphaned; they are harmless.
    content.glyphs = keep;
    content.hidden_glyphs.extend(gone);
}

/// Share of non-white pixels in a low-resolution render of the page.
fn ink_share(page: &PdfPage) -> Result<f32, PdfiumError> {
    let image = page.render_with_config(&PdfRenderConfig::new().set_target_width(200))?.as_image()?;
    let gray = image.to_luma8();
    let total = (gray.width() * gray.height()).max(1) as f32;
    let dark = gray.pixels().filter(|p| p.0[0] < 200).count() as f32;
    Ok(dark / total)
}

/// Images smaller than this share of the page (logos, photos, icons) are
/// never read for text.
const MIN_TEXT_IMAGE_SHARE: f32 = 0.05;
/// An image must yield at least this many characters to count as text.
const MIN_IMAGE_CHARS: usize = 20;

/// Text that exists only as pixels inside an image on an otherwise digital
/// page: a pasted screenshot of a table, a scanned certificate, a chart
/// exported as a picture. Large images with no text over them are cropped
/// from one page render and read; their words join the page's glyphs.
fn ocr_text_images(page: &PdfPage, content: &mut PageContent, engine: crate::ocr::Engine) {
    let page_area = (content.width * content.height).max(1.0);
    let targets: Vec<BBox> = content
        .images
        .iter()
        .filter(|b| b.width() >= 60.0 && b.height() >= 60.0 && b.area() >= MIN_TEXT_IMAGE_SHARE * page_area)
        .filter(|b| {
            !content.glyphs.iter().any(|g| {
                let (cx, cy) = ((g.bbox.x0 + g.bbox.x1) / 2.0, g.bbox.cy());
                cx > b.x0 && cx < b.x1 && cy > b.y0 && cy < b.y1
            })
        })
        .map(|b| BBox { x0: b.x0.max(0.0), y0: b.y0.max(0.0), x1: b.x1.min(content.width), y1: b.y1.min(content.height) })
        .filter(|b| b.width() > 1.0 && b.height() > 1.0)
        .collect();
    if targets.is_empty() {
        return;
    }
    let scale = crate::ocr::render_scale(content.width, content.height);
    let Ok(image) = page.render_with_config(&PdfRenderConfig::new().scale_page_by_factor(scale)).and_then(|b| b.as_image())
    else {
        return;
    };
    let rgb = image.to_rgb8();
    let mut added = 0;
    for b in targets {
        let (x, y) = ((b.x0 * scale) as u32, (b.y0 * scale) as u32);
        let w = ((b.width() * scale) as u32).min(rgb.width().saturating_sub(x));
        let h = ((b.height() * scale) as u32).min(rgb.height().saturating_sub(y));
        if w < 32 || h < 32 {
            continue;
        }
        let crop = image::imageops::crop_imm(&rgb, x, y, w, h).to_image();
        let gray = image::DynamicImage::ImageRgb8(crop.clone()).to_luma8();
        if !looks_textual(&gray) {
            continue;
        }
        let words = match engine {
            #[cfg(feature = "ocr")]
            crate::ocr::Engine::Builtin => crate::ocr::recognise_pixels(crop.as_raw(), w, h, scale),
            _ => {
                let file = crate::ocr::temp_image_path(9999);
                if crop.save(&file).is_err() {
                    continue;
                }
                let r = crate::ocr::recognise(engine, &file, scale);
                let _ = std::fs::remove_file(&file);
                r
            }
        };
        let Ok(mut glyphs) = words else { continue };
        // Photos give a few stray characters at most.
        if glyphs.len() < MIN_IMAGE_CHARS {
            continue;
        }
        let (dx, dy) = (x as f32 / scale, y as f32 / scale);
        content.rules.extend(pixel_rules(&gray, scale, dx, dy));
        for g in &mut glyphs {
            g.bbox = BBox { x0: g.bbox.x0 + dx, y0: g.bbox.y0 + dy, x1: g.bbox.x1 + dx, y1: g.bbox.y1 + dy };
        }
        added += glyphs.len();
        content.ocr_images.push(b);
        content.glyphs.extend(glyphs);
        // Read as text now; no longer a figure.
        content.images.retain(|i| !i.contains(&b, 3.0));
    }
    if added > 0 {
        content.ocr = Some(crate::ocr::OcrStatus {
            reason: "text inside images".to_string(),
            engine: Some(engine.name().to_string()),
            words: added,
        });
    }
}

/// Without an OCR engine, images smaller than this (in points, either side)
/// are not listed as unread: icons, bullets and rating dots.
const MIN_UNREAD_IMAGE_SIDE: f32 = 30.0;
/// ... nor smaller than this share of the page.
const MIN_UNREAD_IMAGE_SHARE: f32 = 0.004;
/// A single line of text exported as a picture (a contact bar, a phone
/// number) is smaller than that: an image this short (points)...
const TEXT_STRIP_HEIGHT: std::ops::RangeInclusive<f32> = 5.0..=30.0;
/// ... at least this wide and this many times wider than high is checked
/// too, and listed only when its pixels look like a line of letters.
const TEXT_STRIP_MIN_WIDTH: f32 = 40.0;
const TEXT_STRIP_MIN_ASPECT: f32 = 4.0;
/// Separate ink strokes across the strip's middle that make it a line of
/// letters rather than a rating bar or a rule (a phone number has 10+).
const TEXT_STRIP_MIN_STROKES: usize = 6;
/// Resolution of the render used to tell text images from photos.
const UNREAD_CHECK_DPI: f32 = 100.0;

fn text_strip(b: &BBox) -> bool {
    TEXT_STRIP_HEIGHT.contains(&b.height()) && b.width() >= TEXT_STRIP_MIN_WIDTH && b.width() >= TEXT_STRIP_MIN_ASPECT * b.height()
}

/// Images on a digital page that look like printed text (a screenshot of a
/// table, a certificate, a skill badge, a contact line exported as a
/// picture) and have no text over them. With no OCR engine in the build
/// these are what the caller has to read some other way, for example with a
/// vision model. Photos and artwork are left out (their pixels are mostly
/// mid-tones), so a candidate's photo is never listed.
fn unread_text_images(page: &PdfPage, content: &PageContent) -> Vec<BBox> {
    let page_area = (content.width * content.height).max(1.0);
    let targets: Vec<BBox> = content
        .images
        .iter()
        .filter(|b| (b.width().min(b.height()) >= MIN_UNREAD_IMAGE_SIDE && b.area() >= MIN_UNREAD_IMAGE_SHARE * page_area) || text_strip(b))
        .filter(|b| {
            !content.glyphs.iter().any(|g| {
                let (cx, cy) = ((g.bbox.x0 + g.bbox.x1) / 2.0, g.bbox.cy());
                cx > b.x0 && cx < b.x1 && cy > b.y0 && cy < b.y1
            })
        })
        .map(|b| BBox { x0: b.x0.max(0.0), y0: b.y0.max(0.0), x1: b.x1.min(content.width), y1: b.y1.min(content.height) })
        .filter(|b| b.width() > 1.0 && b.height() > 1.0)
        .collect();
    if targets.is_empty() {
        return targets;
    }
    let scale = UNREAD_CHECK_DPI / 72.0;
    let Ok(image) = page.render_with_config(&PdfRenderConfig::new().scale_page_by_factor(scale)).and_then(|b| b.as_image())
    else {
        return Vec::new();
    };
    let gray = image.to_luma8();
    let mut out: Vec<BBox> = Vec::new();
    for b in targets {
        let (x, y) = ((b.x0 * scale) as u32, (b.y0 * scale) as u32);
        let w = ((b.width() * scale) as u32).min(gray.width().saturating_sub(x));
        let h = ((b.height() * scale) as u32).min(gray.height().saturating_sub(y));
        if w < 8 || h < 8 || out.iter().any(|o| o.contains(&b, 1.0)) {
            continue;
        }
        let crop = image::imageops::crop_imm(&gray, x, y, w, h).to_image();
        let textual = if text_strip(&b) {
            // Small letters are mostly anti-aliased edges, so fewer pixels
            // are clean light or dark; the strokes make up for it.
            let (clean, dark) = ink(&crop);
            clean >= 0.7 && (0.01..=0.35).contains(&dark) && strokes(&crop) >= TEXT_STRIP_MIN_STROKES
        } else {
            looks_textual(&crop)
        };
        if textual {
            out.push(b);
        }
    }
    out
}

/// Whether an image looks like printed text or a table: nearly every pixel
/// is either background-light or ink-dark, with some ink. Photos, artwork
/// and colourful charts have many mid-tones and are not read (OCR on them
/// costs seconds and returns noise).
fn looks_textual(gray: &image::GrayImage) -> bool {
    let (clean, dark) = ink(gray);
    clean >= 0.9 && (0.01..=0.35).contains(&dark)
}

/// Shares of pixels that are clean background or ink (light or dark), and
/// that are ink.
fn ink(gray: &image::GrayImage) -> (f32, f32) {
    let total = (gray.width() * gray.height()).max(1) as f32;
    let (mut light, mut dark) = (0u32, 0u32);
    // Every other pixel in each direction is plenty for a histogram.
    for y in (0..gray.height()).step_by(2) {
        for x in (0..gray.width()).step_by(2) {
            let v = gray.get_pixel(x, y).0[0];
            if v >= 215 {
                light += 1;
            } else if v < 110 {
                dark += 1;
            }
        }
    }
    let sampled = total / 4.0;
    let (light, dark) = (light as f32 / sampled, dark as f32 / sampled);
    (light + dark, dark)
}

/// Most separate runs of ink along any row in the middle of the image.
fn strokes(gray: &image::GrayImage) -> usize {
    let h = gray.height();
    (h / 5..(h * 4 / 5).max(h / 5 + 1).min(h))
        .map(|y| {
            let mut runs = 0;
            let mut ink = false;
            for x in 0..gray.width() {
                let d = gray.get_pixel(x, y).0[0] < 110;
                if d && !ink {
                    runs += 1;
                }
                ink = d;
            }
            runs
        })
        .max()
        .unwrap_or(0)
}

/// Shortest pixel line (points) taken as a table rule.
const MIN_PIXEL_RULE: f32 = 30.0;
/// Light pixels a pixel line may jump.
const MAX_PIXEL_GAP: usize = 2;
/// More pixel lines than this in one image is artwork (hatching, photos).
const MAX_PIXEL_RULES: usize = 100;

/// Table lines drawn as pixels (scans, pictures of tables): long thin runs
/// of dark pixels, horizontal and vertical, returned in page coordinates.
/// `scale` is pixels per point; (dx, dy) is the image's page position.
fn pixel_rules(gray: &image::GrayImage, scale: f32, dx: f32, dy: f32) -> Vec<BBox> {
    let (w, h) = (gray.width() as usize, gray.height() as usize);
    let dark = |x: usize, y: usize| gray.get_pixel(x as u32, y as u32).0[0] < 235;
    let min_len = (MIN_PIXEL_RULE * scale) as usize;
    let max_thick = (MAX_RULE_THICKNESS * 1.5 * scale).ceil() as usize;
    let mut out = Vec::new();
    for vertical in [false, true] {
        let (outer, inner) = if vertical { (w, h) } else { (h, w) };
        // Runs per scan line: (line index, start, end).
        let mut runs: Vec<(usize, usize, usize)> = Vec::new();
        for o in 0..outer {
            // A run may jump gaps of a couple of pixels (anti-aliasing,
            // crossings, faint scans).
            let (mut start, mut last) = (None::<usize>, 0usize);
            for i in 0..=inner {
                let d = i < inner && if vertical { dark(o, i) } else { dark(i, o) };
                if d {
                    start.get_or_insert(i);
                    last = i;
                } else if let Some(s) = start {
                    if i == inner || i - last > MAX_PIXEL_GAP {
                        if last + 1 - s >= min_len {
                            runs.push((o, s, last + 1));
                        }
                        start = None;
                    }
                }
            }
        }
        // Stack runs on neighbouring scan lines into one line.
        let mut lines: Vec<(usize, usize, usize, usize)> = Vec::new(); // first, last, start, end
        for (o, s, e) in runs {
            match lines.iter_mut().rev().find(|l| l.1 + 1 >= o && s < l.3 && e > l.2) {
                Some(l) => {
                    l.1 = o;
                    l.2 = l.2.min(s);
                    l.3 = l.3.max(e);
                }
                None => lines.push((o, o, s, e)),
            }
        }
        for (first, last, s, e) in lines {
            if last - first + 1 > max_thick {
                continue; // a filled block, not a line
            }
            let (a, b) = (first as f32 / scale, (last + 1) as f32 / scale);
            let (p, q) = (s as f32 / scale, e as f32 / scale);
            out.push(if vertical {
                BBox { x0: dx + a, y0: dy + p, x1: dx + b, y1: dy + q }
            } else {
                BBox { x0: dx + p, y0: dy + a, x1: dx + q, y1: dy + b }
            });
        }
    }
    if out.len() > MAX_PIXEL_RULES {
        return vec![];
    }
    out
}

/// OCR a whole page; also returns table lines found in its pixels.
fn ocr_page(
    page: &PdfPage,
    content: &PageContent,
    engine: crate::ocr::Engine,
    index: usize,
) -> Result<(Vec<Glyph>, Vec<BBox>), String> {
    let scale = crate::ocr::render_scale(content.width, content.height);
    let config = PdfRenderConfig::new().scale_page_by_factor(scale);
    let image = page.render_with_config(&config).and_then(|b| b.as_image()).map_err(|e| e.to_string())?;
    let rules = pixel_rules(&image.to_luma8(), scale, 0.0, 0.0);
    #[cfg(feature = "ocr")]
    if engine == crate::ocr::Engine::Builtin {
        // In memory: no temporary file, no extra process.
        let rgb = image.to_rgb8();
        return crate::ocr::recognise_pixels(rgb.as_raw(), rgb.width(), rgb.height(), scale).map(|g| (g, rules));
    }
    let file = crate::ocr::temp_image_path(index);
    image.save(&file).map_err(|e| e.to_string())?;
    let result = crate::ocr::recognise(engine, &file, scale);
    let _ = std::fs::remove_file(&file);
    result.map(|g| (g, rules))
}

/// Page space -> our coordinates: origin at the top-left of the visible
/// (crop) box, y downwards. `origin` is the crop box's (left, top) in page
/// space; most pages have (0, height), but cropped pages are offset.
fn to_bbox(r: &PdfRect, origin: (f32, f32)) -> BBox {
    BBox {
        x0: r.left().value - origin.0,
        y0: origin.1 - r.top().value,
        x1: r.right().value - origin.0,
        y1: origin.1 - r.bottom().value,
    }
}

/// (left, top) of the crop box in page space, falling back to the media box.
fn page_origin(page: &PdfPage) -> (f32, f32) {
    let b = page.boundaries();
    match b.crop().or_else(|_| b.media()) {
        Ok(bx) => (bx.bounds.left().value, bx.bounds.top().value),
        Err(_) => (0.0, page.height().value),
    }
}

fn read_page(page: &PdfPage) -> Result<PageContent, PdfiumError> {
    let h = page.height().value;
    let origin = page_origin(page);
    let mut out = PageContent { width: page.width().value, height: h, ..Default::default() };

    let text = page.text()?;
    // Indices of glyphs that a reader cannot see, and of white glyphs that are
    // hidden unless they sit on a dark background (decided once shapes and
    // images are known).
    let mut hidden: Vec<usize> = Vec::new();
    let mut white: Vec<usize> = Vec::new();
    let mut backgrounds: Vec<BBox> = Vec::new();
    for c in text.chars().iter() {
        let Some(s) = c.unicode_string().or_else(|| c.unicode_char().map(String::from)) else { continue };
        let s = expand_ligatures(&s);
        let mut chars: Vec<char> = s.chars().collect();
        if chars.is_empty() {
            continue;
        }
        if chars.len() == 1 {
            // PDFium reports a hyphen at a line break as U+FFFE (sometimes
            // U+0002 or a soft hyphen), often with an empty box.
            if matches!(chars[0], '\u{FFFE}' | '\u{0002}' | '\u{00AD}') {
                if let Some(prev) = out.glyphs.iter().rev().find(|g| !g.space).cloned() {
                    let w = 0.33 * prev.size;
                    let bbox = BBox { x0: prev.bbox.x1, x1: prev.bbox.x1 + w, ..prev.bbox };
                    out.glyphs.push(Glyph { ch: '-', bbox, size: prev.size, space: false, bold: prev.bold });
                }
                continue;
            }
            let ch = chars[0];
            if ch.is_control() || (ch.is_whitespace() && ch != ' ') {
                continue;
            }
            // PDFium also inserts spaces it infers from glyph positions. Files
            // that set words tightly store no space characters, so these are
            // the only word breaks. Place one inside the previous glyph's box,
            // past its middle: lines are put together in order of left edges,
            // and in some fonts the next word's box starts a little before
            // this one ends, which put the space after that word's first
            // letter ("toolsa nd").
            if ch == ' ' && c.is_generated().unwrap_or(true) {
                if let Some(prev) = out.glyphs.iter().rev().find(|g| !g.space).cloned() {
                    let x0 = prev.bbox.x0 + 0.6 * prev.bbox.width();
                    let bbox = BBox { x0, x1: x0 + 0.01, ..prev.bbox };
                    out.glyphs.push(Glyph { ch: ' ', bbox, size: prev.size, space: true, bold: prev.bold });
                }
                continue;
            }
        } else {
            chars.retain(|ch| !ch.is_control() && !ch.is_whitespace());
        }
        let Ok(r) = c.loose_bounds() else { continue };
        let mut bbox = to_bbox(&r, origin);
        // Vertical extent from the baseline and font size, not from the font's
        // declared ascent/descent: broken fonts report boxes several lines tall,
        // which would merge neighbouring lines. Rotated text keeps its box.
        let size = c.scaled_font_size().value;
        let upright = c.angle_degrees().map(|a| a.abs() < 1.0 || (a - 360.0).abs() < 1.0).unwrap_or(true);
        if upright && size > 0.5 {
            if let Ok(oy) = c.origin_y() {
                let base = origin.1 - oy.value;
                bbox.y0 = base - 0.8 * size;
                bbox.y1 = base + 0.25 * size;
            }
        }
        if bbox.width() <= 0.0 && bbox.height() <= 0.0 {
            continue;
        }
        // Text placed outside the visible page is never seen by a reader.
        let (cx, cy) = ((bbox.x0 + bbox.x1) / 2.0, bbox.cy());
        if cx < 0.0 || cy < 0.0 || cx > out.width || cy > h {
            continue;
        }
        let bold = is_bold(&c);
        let visibility = visibility(&c, size);
        // A ligature glyph becomes several characters sharing its width.
        let n = chars.len() as f32;
        let w = bbox.width() / n;
        for (k, ch) in chars.into_iter().enumerate() {
            let part = BBox { x0: bbox.x0 + w * k as f32, x1: bbox.x0 + w * (k as f32 + 1.0), ..bbox };
            match visibility {
                Visibility::Hidden => hidden.push(out.glyphs.len()),
                Visibility::White => white.push(out.glyphs.len()),
                Visibility::Visible => {}
            }
            out.glyphs.push(Glyph { ch, bbox: part, size: c.scaled_font_size().value, space: ch == ' ', bold });
        }
    }

    for link in page.links().iter() {
        let Some(PdfAction::Uri(action)) = link.action() else { continue };
        let (Ok(uri), Ok(rect)) = (action.uri(), link.rect()) else { continue };
        let uri = uri.trim();
        let lower = uri.to_lowercase();
        if ["http://", "https://", "www.", "mailto:", "tel:"].iter().any(|p| lower.starts_with(p)) {
            out.links.push((to_bbox(&rect, origin), uri.to_string()));
        }
    }

    for obj in page.objects().iter() {
        read_object(&obj, &Place::PAGE, origin, &mut out, &mut backgrounds, 0);
    }
    // White text with no known coloured shape or image under it is only a
    // suspect (shading patterns and soft masks are not listed here), so the
    // page gets the pixel check (in load_pages) instead.
    for i in white {
        let g = &out.glyphs[i];
        let (cx, cy) = ((g.bbox.x0 + g.bbox.x1) / 2.0, g.bbox.cy());
        let inside = |b: &BBox| cx >= b.x0 && cx <= b.x1 && cy >= b.y0 && cy <= b.y1;
        if !backgrounds.iter().any(inside) && !out.images.iter().any(inside) {
            out.white_suspects += 1;
        }
    }
    if !hidden.is_empty() {
        hidden.sort_unstable();
        hidden.dedup();
        let mut keep = Vec::with_capacity(out.glyphs.len());
        for (i, g) in std::mem::take(&mut out.glyphs).into_iter().enumerate() {
            if hidden.binary_search(&i).is_ok() {
                out.hidden_glyphs.push(g);
            } else {
                keep.push(g);
            }
        }
        out.glyphs = keep;
    }
    Ok(out)
}

/// Where an object lands on the page. Objects inside an embedded drawing
/// group (a form XObject) are reported in the group's own coordinates, so
/// the group's transform has to be applied to them; page-level objects need
/// none.
#[derive(Clone, Copy)]
struct Place {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Place {
    const PAGE: Place = Place { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    fn of(m: &PdfMatrix) -> Place {
        Place { a: m.a(), b: m.b(), c: m.c(), d: m.d(), e: m.e(), f: m.f() }
    }

    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }

    /// `self` applied first, then `outer`.
    fn then(&self, outer: &Place) -> Place {
        let (e, f) = outer.apply(self.e, self.f);
        Place {
            a: outer.a * self.a + outer.c * self.b,
            b: outer.b * self.a + outer.d * self.b,
            c: outer.a * self.c + outer.c * self.d,
            d: outer.b * self.c + outer.d * self.d,
            e,
            f,
        }
    }

    /// A box in the object's coordinates as a box on the page (our axes).
    fn bbox(&self, r: &PdfRect, origin: (f32, f32)) -> BBox {
        let corners = [(r.left().value, r.top().value), (r.right().value, r.top().value), (r.left().value, r.bottom().value), (r.right().value, r.bottom().value)];
        let pts: Vec<(f32, f32)> = corners.iter().map(|&(x, y)| self.apply(x, y)).collect();
        let (xs, ys): (Vec<f32>, Vec<f32>) = pts.into_iter().unzip();
        let min = |v: &[f32]| v.iter().copied().fold(f32::INFINITY, f32::min);
        let max = |v: &[f32]| v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        BBox { x0: min(&xs) - origin.0, y0: origin.1 - max(&ys), x1: max(&xs) - origin.0, y1: origin.1 - min(&ys) }
    }
}

/// Drawing groups nested deeper than this are not opened.
const MAX_FORM_DEPTH: usize = 4;

/// Shapes, images and rules of one page object. Drawing groups (form
/// XObjects) are opened and their contents read in place: Word and design
/// tools sometimes put a whole list, bullets included, inside one, and the
/// bullets were lost when only the page's top-level objects were read.
fn read_object(obj: &PdfPageObject, place: &Place, origin: (f32, f32), out: &mut PageContent, backgrounds: &mut Vec<BBox>, depth: usize) {
    if let Some(form) = obj.as_x_object_form_object() {
        if depth < MAX_FORM_DEPTH {
            let inner = form.matrix().map(|m| Place::of(&m).then(place)).unwrap_or(*place);
            for child in form.iter() {
                read_object(&child, &inner, origin, out, backgrounds, depth + 1);
            }
        }
        return;
    }
    if obj.object_type() == PdfPageObjectType::Path {
        out.path_count += 1;
    }
    if obj.object_type() == PdfPageObjectType::Image {
        if let Ok(q) = obj.bounds() {
            out.images.push(place.bbox(&q.to_rect(), origin));
        }
        return;
    }
    let Some(path) = obj.as_path_object() else { return };
    let Ok(q) = obj.bounds() else { return };
    let bbox = place.bbox(&q.to_rect(), origin);
    let (w, hh) = (bbox.width(), bbox.height());
    if w.min(hh) <= MAX_RULE_THICKNESS && w.max(hh) > 4.0 {
        out.rules.push(bbox);
        return;
    }
    // One path object often draws a whole table: a stroked grid of
    // lines, or filled cell rectangles, or a border drawn as an outer box
    // minus an inner box. Its bounding box alone hides those lines.
    out.rules.extend(path_lines(&path, place, origin));
    out.graphics.extend(path_marks(&path, place, origin, &bbox));
    out.graphics.push(bbox);
    let filled = !matches!(path.fill_mode(), Ok(PdfPathFillMode::None) | Err(_));
    if filled && is_visible_fill(obj) {
        // Any coloured shape can carry white text (header bars, badges).
        backgrounds.push(bbox);
        out.shaded.push(bbox);
        if w >= MIN_CONTAINER_SIDE && hh >= MIN_CONTAINER_SIDE {
            out.fills.push(Fill { bbox });
        }
    }
}

/// Paths with more segments than this are artwork or glyph outlines.
const MAX_PATH_SEGMENTS: usize = 4000;

/// Table lines inside one path object, in page coordinates.
///
/// Stroked paths: every horizontal or vertical straight segment is a line.
/// Filled paths made only of two or more axis-aligned rectangles (cell
/// tiles, or a frame drawn as an outer box minus an inner box): each
/// rectangle's edges are lines. Paths with curves are never tables.
/// One box per subpath of a path object that draws several small marks.
/// Some producers draw every bullet of a list (or every dot of a skill
/// rating) with a single path, whose bounding box then spans the whole list
/// and looks nothing like a bullet, so the list's items lose their markers.
/// Only small subpaths spread over a much larger area are returned.
fn path_marks(path: &PdfPagePathObject, place: &Place, origin: (f32, f32), whole: &BBox) -> Vec<BBox> {
    let segments = path.segments();
    let n = segments.len() as usize;
    if n < 8 || n > MAX_PATH_SEGMENTS {
        return vec![];
    }
    let Ok(matrix) = path.matrix() else { return vec![] };
    let mut boxes: Vec<BBox> = Vec::new();
    for seg in segments.transform(matrix).iter() {
        let (x, y) = seg.point();
        let (x, y) = place.apply(x.value, y.value);
        let (px, py) = (x - origin.0, origin.1 - y);
        match (seg.segment_type(), boxes.last_mut()) {
            (PdfPathSegmentType::MoveTo, _) | (_, None) => boxes.push(BBox { x0: px, y0: py, x1: px, y1: py }),
            (_, Some(b)) => {
                b.x0 = b.x0.min(px);
                b.y0 = b.y0.min(py);
                b.x1 = b.x1.max(px);
                b.y1 = b.y1.max(py);
            }
        }
    }
    let largest = boxes.iter().map(|b| b.width().max(b.height())).fold(0.0, f32::max);
    if boxes.len() < 2 || largest <= 0.0 || whole.width().max(whole.height()) < 3.0 * largest {
        return vec![];
    }
    boxes
}

fn path_lines(path: &PdfPagePathObject, place: &Place, origin: (f32, f32)) -> Vec<BBox> {
    let segments = path.segments();
    let n = segments.len() as usize;
    if n < 4 || n > MAX_PATH_SEGMENTS {
        return vec![];
    }
    let Ok(matrix) = path.matrix() else { return vec![] };
    // White or transparent strokes (gaps between chart segments) are not
    // lines a reader sees.
    let visible_stroke = path
        .stroke_color()
        .map(|c| c.alpha() > 0 && !(c.red() > 240 && c.green() > 240 && c.blue() > 240))
        .unwrap_or(false);
    let stroked = path.is_stroked().unwrap_or(false) && visible_stroke;
    if path.is_stroked().unwrap_or(false) && !visible_stroke {
        return vec![];
    }
    let thickness = path.stroke_width().map(|w| w.value).unwrap_or(1.0).clamp(0.3, MAX_RULE_THICKNESS);

    // Split into subpaths of page-space points.
    let mut subpaths: Vec<Vec<(f32, f32)>> = Vec::new();
    for seg in segments.transform(matrix).iter() {
        let (x, y) = seg.point();
        let (x, y) = place.apply(x.value, y.value);
        let p = (x - origin.0, origin.1 - y);
        match seg.segment_type() {
            PdfPathSegmentType::MoveTo => subpaths.push(vec![p]),
            PdfPathSegmentType::LineTo => match subpaths.last_mut() {
                // Back at the start after tracing a shape: what follows is a
                // new shape even without a MoveTo (frames drawn as outer box,
                // then a hop to the inner box).
                Some(sp) if sp.len() >= 4 && close(sp[0], *sp.last().unwrap()) => subpaths.push(vec![p]),
                Some(sp) => sp.push(p),
                None => subpaths.push(vec![p]),
            },
            _ => return vec![],
        }
    }

    let axis_line = |a: (f32, f32), b: (f32, f32)| -> Option<BBox> {
        let (dx, dy) = ((a.0 - b.0).abs(), (a.1 - b.1).abs());
        let t = thickness / 2.0;
        if dy < 0.5 && dx > 4.0 {
            let y = (a.1 + b.1) / 2.0;
            Some(BBox { x0: a.0.min(b.0), y0: y - t, x1: a.0.max(b.0), y1: y + t })
        } else if dx < 0.5 && dy > 4.0 {
            let x = (a.0 + b.0) / 2.0;
            Some(BBox { x0: x - t, y0: a.1.min(b.1), x1: x + t, y1: a.1.max(b.1) })
        } else {
            None
        }
    };

    if stroked {
        let mut out = Vec::new();
        for sp in &subpaths {
            let mut pts = sp.clone();
            if pts.len() >= 3 {
                pts.push(pts[0]); // closed subpaths draw their last edge too
            }
            out.extend(pts.windows(2).filter_map(|w| axis_line(w[0], w[1])));
        }
        // Only a grid (lines both ways) is a table. Parallel lines alone are
        // chart gridlines or decoration.
        let vertical = out.iter().filter(|r| r.height() > r.width()).count();
        if vertical < 2 || out.len() - vertical < 2 {
            return vec![];
        }
        return out;
    }

    // Filled: only when every subpath is an axis-aligned rectangle.
    let rects: Vec<BBox> = subpaths.iter().filter(|sp| sp.len() >= 4).filter_map(|sp| rect_of(sp)).collect();
    if rects.len() < 2 || rects.len() != subpaths.iter().filter(|sp| sp.len() >= 2).count() {
        return vec![];
    }
    let Some(all) = BBox::union_all(rects.iter()) else { return vec![] };
    // A border: one box inside another, a few points smaller on each side.
    let frame = rects.len() == 2 && {
        let (a, b) = if rects[0].area() >= rects[1].area() { (&rects[0], &rects[1]) } else { (&rects[1], &rects[0]) };
        let insets = [b.x0 - a.x0, b.y0 - a.y0, a.x1 - b.x1, a.y1 - b.y1];
        insets.iter().all(|&d| (-0.1..=3.0).contains(&d))
    };
    if frame {
        let r = all;
        return [((r.x0, r.y0), (r.x1, r.y0)), ((r.x0, r.y1), (r.x1, r.y1)), ((r.x0, r.y0), (r.x0, r.y1)), ((r.x1, r.y0), (r.x1, r.y1))]
            .into_iter()
            .filter_map(|(a, b)| axis_line(a, b))
            .collect();
    }
    // Cell tiles fill their joint box, or (shaded alternate rows) sit edge
    // to edge with their neighbours. Chart bars and legend swatches stand
    // apart with gaps between them.
    let covered: f32 = rects.iter().map(|r| r.area()).sum();
    let share = covered / all.area().max(1.0);
    let abutting = rects.iter().enumerate().filter(|(i, a)| rects.iter().enumerate().any(|(j, b)| *i != j && share_edge(a, b))).count();
    let tiled = (0.9..=1.1).contains(&share) || abutting as f32 >= 0.8 * rects.len() as f32;
    if rects.len() < 4 || !tiled {
        return vec![];
    }
    let mut out = Vec::new();
    for r in rects {
        if r.width().min(r.height()) <= MAX_RULE_THICKNESS {
            out.push(r);
            continue;
        }
        for (a, b) in [((r.x0, r.y0), (r.x1, r.y0)), ((r.x0, r.y1), (r.x1, r.y1)), ((r.x0, r.y0), (r.x0, r.y1)), ((r.x1, r.y0), (r.x1, r.y1))] {
            out.extend(axis_line(a, b));
        }
    }
    out
}

/// Two boxes side by side (or stacked) with a common edge of the same length.
fn share_edge(a: &BBox, b: &BBox) -> bool {
    let near = |x: f32, y: f32| (x - y).abs() < 1.0;
    let side = (near(a.x1, b.x0) || near(b.x1, a.x0)) && near(a.y0, b.y0) && near(a.y1, b.y1);
    let stacked = (near(a.y1, b.y0) || near(b.y1, a.y0)) && near(a.x0, b.x0) && near(a.x1, b.x1);
    side || stacked
}

fn close(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() < 0.5 && (a.1 - b.1).abs() < 0.5
}

/// The rectangle a closed subpath traces, if it traces one.
fn rect_of(pts: &[(f32, f32)]) -> Option<BBox> {
    let mut xs: Vec<f32> = pts.iter().map(|p| p.0).collect();
    let mut ys: Vec<f32> = pts.iter().map(|p| p.1).collect();
    xs.sort_by(f32::total_cmp);
    ys.sort_by(f32::total_cmp);
    let two = |v: &[f32]| v.iter().all(|&x| (x - v[0]).abs() < 0.5 || (x - v[v.len() - 1]).abs() < 0.5);
    if pts.len() > 6 || !two(&xs) || !two(&ys) {
        return None;
    }
    // Consecutive points must move along one axis only.
    let ok = pts.windows(2).all(|w| (w[0].0 - w[1].0).abs() < 0.5 || (w[0].1 - w[1].1).abs() < 0.5);
    ok.then(|| BBox { x0: xs[0], y0: ys[0], x1: xs[xs.len() - 1], y1: ys[ys.len() - 1] })
}

enum Visibility {
    Visible,
    /// White fill: visible only on a dark background.
    White,
    /// Invisible render mode, fully transparent, or microscopic.
    Hidden,
}

fn visibility(c: &PdfPageTextChar, size: f32) -> Visibility {
    if matches!(
        c.render_mode(),
        Ok(PdfPageTextRenderMode::Invisible | PdfPageTextRenderMode::InvisibleClipping)
    ) || (size > 0.0 && size < MIN_VISIBLE_SIZE)
    {
        return Visibility::Hidden;
    }
    match c.fill_color() {
        Ok(col) if col.alpha() < 16 => Visibility::Hidden,
        Ok(col) if col.red() >= 245 && col.green() >= 245 && col.blue() >= 245 => Visibility::White,
        _ => Visibility::Visible,
    }
}

/// Bold by declared weight, by "Bold"/"Black"/"Heavy"/"Semibold" in the
/// font name, or by PDFium's forced-bold flag.
fn is_bold(c: &PdfPageTextChar) -> bool {
    let heavy = matches!(
        c.font_weight(),
        Some(PdfFontWeight::Weight600 | PdfFontWeight::Weight700Bold | PdfFontWeight::Weight800 | PdfFontWeight::Weight900)
    ) || matches!(c.font_weight(), Some(PdfFontWeight::Custom(w)) if w >= 600);
    if heavy || c.font_is_bold_reenforced() {
        return true;
    }
    let name = c.font_name().to_ascii_lowercase();
    ["bold", "black", "heavy", "semibold", "demi"].iter().any(|k| name.contains(k))
}

/// Typographic ligatures (U+FB00..U+FB06) to their letters.
fn expand_ligatures(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
            _ => out.push(ch),
        }
    }
    out
}

/// A fill that is transparent or plain white does not separate content.
fn is_visible_fill(obj: &PdfPageObject) -> bool {
    match obj.fill_color() {
        Ok(c) => c.alpha() > 0 && !(c.red() > 250 && c.green() > 250 && c.blue() > 250),
        Err(_) => false,
    }
}
