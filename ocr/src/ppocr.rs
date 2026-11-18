//! PP-OCRv6 tiny (PaddleOCR, Apache-2.0) on rten: an image in, its text
//! lines out, each with its box and where each character sits on it.
//!
//! Steps and settings follow RapidOCR's PP-OCRv6 pipeline: the image is
//! scaled to at most 2,000 px; the detector marks text pixels; each patch
//! of text becomes a box, grown by the DB "unclip" margin; each box is cut
//! out, scaled to 48 px high and read by the recogniser (CTC). Boxes are
//! upright rectangles: document text is horizontal, and the parser's own
//! layout analysis works on upright boxes.

use std::sync::OnceLock;

use image::imageops::FilterType;
use image::RgbImage;
use rten::Model;
use rten_tensor::prelude::*;
use rten_tensor::NdTensor;

static DETECTION: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../models/ppocr/PP-OCRv6_det_tiny.onnx"));
static RECOGNITION: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../models/ppocr/PP-OCRv6_rec_tiny.onnx"));

pub const ENGINE_NAME: &str = "pp-ocrv6-tiny";

/// Longest side the pipeline works at.
const MAX_SIDE: u32 = 2000;
/// The detector's shorter side is scaled up to at least this.
const DET_MIN_SIDE: u32 = 736;
/// Text-pixel threshold, box score threshold and box margin (DB).
const DET_THRESH: f32 = 0.3;
const BOX_THRESH: f32 = 0.5;
const UNCLIP_RATIO: f32 = 1.6;
/// Recogniser input height and the narrowest width it is given.
const REC_HEIGHT: u32 = 48;
const REC_MIN_WIDTH: u32 = 320;
/// Lines read with a lower mean confidence are dropped (RapidOCR's default).
const TEXT_SCORE: f32 = 0.5;
/// Crops recognised in one batch.
const REC_BATCH: usize = 8;

/// One recognised line, in pixels of the image it was read from.
pub struct Line {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// Mean confidence of its characters, 0..1.
    pub score: f32,
    /// Characters with the x of their centre; ' ' where the model put a space.
    pub chars: Vec<(char, f32)>,
}

impl Line {
    pub fn text(&self) -> String {
        self.chars.iter().map(|c| c.0).collect()
    }
}

struct Engine {
    det: Model,
    rec: Model,
    /// Index 0 is the CTC blank; the last is the space.
    keys: Vec<char>,
}

fn engine() -> Result<&'static Engine, String> {
    static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let det = Model::load_static_slice(DETECTION).map_err(|e| format!("detection model: {e}"))?;
            let rec = Model::load_static_slice(RECOGNITION).map_err(|e| format!("recognition model: {e}"))?;
            let mut keys = vec!['\0'];
            keys.extend(character_list(RECOGNITION).ok_or("recognition model has no character list")?);
            keys.push(' ');
            Ok(Engine { det, rec, keys })
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// The recogniser's characters, one per line in the ONNX metadata entry
/// "character" (protobuf: key field 1, value field 2, length-prefixed).
fn character_list(onnx: &[u8]) -> Option<Vec<char>> {
    let key = b"\x0a\x09character\x12";
    let at = onnx.windows(key.len()).position(|w| w == key)? + key.len();
    let (mut len, mut shift, mut i) = (0usize, 0, at);
    loop {
        let b = *onnx.get(i)?;
        len |= ((b & 0x7f) as usize) << shift;
        i += 1;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    let text = std::str::from_utf8(onnx.get(i..i + len)?).ok()?;
    Some(text.lines().map(|l| l.chars().next().unwrap_or(' ')).collect())
}

/// Read every text line in `img`.
pub fn read(img: &RgbImage) -> Result<Vec<Line>, String> {
    let engine = engine()?;
    let (w, h) = img.dimensions();
    if w < 4 || h < 4 {
        return Ok(Vec::new());
    }
    // Work at no more than MAX_SIDE; results are mapped back at the end.
    let k = (MAX_SIDE as f32 / w.max(h) as f32).min(1.0);
    let work = if k < 1.0 {
        image::imageops::resize(img, ((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1), FilterType::Triangle)
    } else {
        img.clone()
    };
    let boxes = detect(engine, &work)?;
    let mut lines = recognise(engine, &work, &boxes)?;
    for l in &mut lines {
        l.x0 /= k;
        l.y0 /= k;
        l.x1 /= k;
        l.y1 /= k;
        for c in &mut l.chars {
            c.1 /= k;
        }
    }
    Ok(lines)
}

/// Pixel tensor in the models' layout: BGR planes, scaled to -1..1.
fn tensor(img: &RgbImage, width: usize) -> Vec<f32> {
    let h = img.height() as usize;
    let mut data = vec![0.0f32; 3 * h * width];
    for (x, y, p) in img.enumerate_pixels() {
        for (c, plane) in [2usize, 1, 0].into_iter().enumerate() {
            data[plane * h * width + y as usize * width + x as usize] = p[c] as f32 / 127.5 - 1.0;
        }
    }
    data
}

/// Text boxes in `img` pixels: (x0, y0, x1, y1).
fn detect(engine: &Engine, img: &RgbImage) -> Result<Vec<[f32; 4]>, String> {
    let (w, h) = img.dimensions();
    let ratio = if w.min(h) < DET_MIN_SIDE { DET_MIN_SIDE as f32 / w.min(h) as f32 } else { 1.0 };
    let round32 = |v: f32| (((v / 32.0).round() as u32) * 32).max(32);
    let (dw, dh) = (round32(w as f32 * ratio), round32(h as f32 * ratio));
    let input = image::imageops::resize(img, dw, dh, FilterType::Triangle);
    let tensor = NdTensor::from_data([1, 3, dh as usize, dw as usize], tensor(&input, dw as usize));
    let out = engine.det.run_one(tensor.view().into(), None).map_err(|e| e.to_string())?;
    let prob: rten_tensor::Tensor<f32> = out.try_into().map_err(|e| format!("{e:?}"))?;
    let shape = prob.shape().to_vec();
    let (mh, mw) = (shape[shape.len() - 2], shape[shape.len() - 1]);
    let prob = prob.to_vec();

    // Text pixels, dilated by one pixel right and down (2x2 kernel).
    let mut mask = vec![false; mh * mw];
    for y in 0..mh {
        for x in 0..mw {
            if prob[y * mw + x] > DET_THRESH {
                mask[y * mw + x] = true;
                if x + 1 < mw {
                    mask[y * mw + x + 1] = true;
                }
                if y + 1 < mh {
                    mask[(y + 1) * mw + x] = true;
                    if x + 1 < mw {
                        mask[(y + 1) * mw + x + 1] = true;
                    }
                }
            }
        }
    }
    let (sx, sy) = (w as f32 / mw as f32, h as f32 / mh as f32);
    let mut seen = vec![false; mh * mw];
    let mut boxes = Vec::new();
    let mut stack = Vec::new();
    for start in 0..mh * mw {
        if !mask[start] || seen[start] {
            continue;
        }
        // One patch of text (8-connected).
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        seen[start] = true;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % mw, i / mw);
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= mw as i32 || ny >= mh as i32 {
                        continue;
                    }
                    let j = ny as usize * mw + nx as usize;
                    if mask[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        let (bw, bh) = ((x1 - x0) as f32, (y1 - y0) as f32);
        if bw.min(bh) < 3.0 {
            continue;
        }
        let mut sum = 0.0;
        for y in y0..=y1 {
            sum += prob[y * mw + x0..=y * mw + x1].iter().sum::<f32>();
        }
        if sum / (((x1 - x0 + 1) * (y1 - y0 + 1)) as f32) < BOX_THRESH {
            continue;
        }
        // DB unclip: grow by area x ratio / perimeter.
        let d = bw * bh * UNCLIP_RATIO / (2.0 * (bw + bh));
        let (bx0, by0, bx1, by1) = (x0 as f32 - d, y0 as f32 - d, x1 as f32 + d, y1 as f32 + d);
        if (bx1 - bx0).min(by1 - by0) < 5.0 {
            continue;
        }
        let b = [
            (bx0 * sx).round().clamp(0.0, w as f32),
            (by0 * sy).round().clamp(0.0, h as f32),
            (bx1 * sx).round().clamp(0.0, w as f32),
            (by1 * sy).round().clamp(0.0, h as f32),
        ];
        if b[2] - b[0] > 3.0 && b[3] - b[1] > 3.0 {
            boxes.push(b);
        }
    }
    Ok(boxes)
}

fn recognise(engine: &Engine, img: &RgbImage, boxes: &[[f32; 4]]) -> Result<Vec<Line>, String> {
    let crops: Vec<RgbImage> = boxes
        .iter()
        .map(|b| image::imageops::crop_imm(img, b[0] as u32, b[1] as u32, (b[2] - b[0]) as u32, (b[3] - b[1]) as u32).to_image())
        .collect();
    // Similar widths together, so little of each batch is padding.
    let mut order: Vec<usize> = (0..crops.len()).collect();
    order.sort_by(|&a, &b| {
        let r = |i: usize| crops[i].width() as f32 / crops[i].height().max(1) as f32;
        r(a).total_cmp(&r(b))
    });
    // Batches run side by side: rten keeps few cores busy on one small
    // recogniser batch.
    let batches: Vec<&[usize]> = order.chunks(REC_BATCH).collect();
    let workers = threads().min(batches.len()).max(1);
    let results: Vec<Result<Vec<Line>, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|w| {
                let (batches, crops) = (&batches, &crops);
                scope.spawn(move || -> Result<Vec<Line>, String> {
                    let mut lines = Vec::new();
                    for batch in batches.iter().skip(w).step_by(workers) {
                        lines.extend(recognise_batch(engine, crops, boxes, batch)?);
                    }
                    Ok(lines)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("recogniser thread failed".into()))).collect()
    });
    let mut lines = Vec::new();
    for r in results {
        lines.extend(r?);
    }
    lines.sort_by(|a, b| a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0)));
    Ok(lines)
}

/// Threads to use: RTEN_NUM_THREADS (the setting rten itself reads), else
/// one per CPU core.
fn threads() -> usize {
    std::env::var("RTEN_NUM_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4))
}

fn recognise_batch(engine: &Engine, crops: &[RgbImage], boxes: &[[f32; 4]], batch: &[usize]) -> Result<Vec<Line>, String> {
    let mut lines = Vec::new();
    {
        let widths: Vec<u32> = batch
            .iter()
            .map(|&i| ((REC_HEIGHT as f32 * crops[i].width() as f32 / crops[i].height().max(1) as f32).ceil() as u32).max(1))
            .collect();
        let width = widths.iter().copied().max().unwrap_or(1).max(REC_MIN_WIDTH) as usize;
        let plane = REC_HEIGHT as usize * width;
        let mut data = vec![0.0f32; batch.len() * 3 * plane];
        for (n, (&i, &rw)) in batch.iter().zip(&widths).enumerate() {
            let resized = image::imageops::resize(&crops[i], rw, REC_HEIGHT, FilterType::Triangle);
            data[n * 3 * plane..(n + 1) * 3 * plane].copy_from_slice(&tensor(&resized, width));
        }
        let input = NdTensor::from_data([batch.len(), 3, REC_HEIGHT as usize, width], data);
        let out = engine.rec.run_one(input.view().into(), None).map_err(|e| e.to_string())?;
        let probs: rten_tensor::Tensor<f32> = out.try_into().map_err(|e| format!("{e:?}"))?;
        let shape = probs.shape().to_vec();
        let (steps, classes) = (shape[1], shape[2]);
        let probs = probs.to_vec();
        for (n, (&i, &rw)) in batch.iter().zip(&widths).enumerate() {
            let b = boxes[i];
            // Recogniser pixels per time step, and crop pixels per recogniser pixel.
            let step_px = width as f32 / steps as f32;
            let back = (b[3] - b[1]) / REC_HEIGHT as f32;
            let mut chars = Vec::new();
            let mut scores = Vec::new();
            let mut prev = 0usize;
            for t in 0..steps {
                let row = &probs[(n * steps + t) * classes..(n * steps + t + 1) * classes];
                let (best, p) = row.iter().enumerate().fold((0, f32::MIN), |acc, (k, &v)| if v > acc.1 { (k, v) } else { acc });
                if best != 0 && best != prev {
                    let x = (t as f32 + 0.5) * step_px;
                    if x <= rw as f32 + step_px {
                        let ch = engine.keys.get(best).copied().unwrap_or(' ');
                        chars.push((ch, b[0] + x * back));
                        scores.push(p);
                    }
                }
                prev = best;
            }
            let score = if scores.is_empty() { 0.0 } else { scores.iter().sum::<f32>() / scores.len() as f32 };
            if score < TEXT_SCORE || chars.iter().all(|c| c.0 == ' ') {
                continue;
            }
            lines.push(Line { x0: b[0], y0: b[1], x1: b[2], y1: b[3], score, chars });
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_list_is_read_from_metadata() {
        let mut onnx = b"junk".to_vec();
        onnx.extend(b"\x0a\x09character\x12\x05a\nb\nc");
        assert_eq!(character_list(&onnx), Some(vec!['a', 'b', 'c']));
        assert!(character_list(b"nothing").is_none());
    }

    #[test]
    fn the_real_model_has_its_characters() {
        let keys = character_list(RECOGNITION).unwrap();
        // 6,904 characters; the model's 6,906 classes add the blank and space.
        assert_eq!(keys.len(), 6904);
        assert!(keys.contains(&'@') && keys.contains(&'Z') && keys.contains(&'7'));
    }
}
