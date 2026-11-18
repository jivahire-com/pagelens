//! Recognised lines become glyphs at their place on the page, so the
//! parser's own analysis (columns, sections, tables, personal data,
//! redaction) runs on a scan as it does on a digital PDF.

use pagelens::model::{BBox, Glyph};

use crate::ppocr::Line;

/// Glyphs for `lines` read from an image placed at `origin` (points) with
/// `px_per_pt` pixels per point.
pub fn glyphs(lines: &[Line], px_per_pt: f32, origin: (f32, f32)) -> Vec<Glyph> {
    let mut out = Vec::new();
    for line in lines {
        let to_x = |px: f32| origin.0 + px / px_per_pt;
        let (lx0, lx1) = (to_x(line.x0), to_x(line.x1));
        let (y0, y1) = (origin.1 + line.y0 / px_per_pt, origin.1 + line.y1 / px_per_pt);
        // The detector's box spans ascenders to descenders: about the font size.
        let size = (y1 - y0).max(1.0);

        let ink: Vec<(char, f32)> = line.chars.iter().map(|&(c, x)| (c, to_x(x))).collect();
        let mut steps: Vec<f32> = ink.windows(2).filter(|w| w[0].0 != ' ' && w[1].0 != ' ').map(|w| w[1].1 - w[0].1).collect();
        steps.sort_by(f32::total_cmp);
        let pitch = steps.get(steps.len() / 2).copied().filter(|p| *p > 0.0).unwrap_or(size * 0.5);

        // Words: split at the model's spaces. Splitting at wide gaps as
        // well broke words apart (narrow letters and dots sit far from
        // their neighbours' centres) and lowered word accuracy.
        let mut words: Vec<Vec<(char, f32)>> = vec![Vec::new()];
        for &(c, x) in &ink {
            if c == ' ' {
                words.push(Vec::new());
            } else {
                words.last_mut().unwrap().push((c, x));
            }
        }
        words.retain(|w| !w.is_empty());

        let mut prev_end: Option<f32> = None;
        for word in &words {
            // Letters of a word touch: each spans from the midpoint with the
            // letter before to the midpoint with the one after.
            let first = (word[0].1 - pitch / 2.0).max(lx0).max(prev_end.unwrap_or(f32::MIN));
            let last = (word[word.len() - 1].1 + pitch / 2.0).min(lx1.max(word[word.len() - 1].1));
            if let Some(end) = prev_end {
                out.push(Glyph { ch: ' ', bbox: BBox { x0: end, y0, x1: first.max(end), y1 }, size, space: true, bold: false });
            }
            for (i, &(ch, x)) in word.iter().enumerate() {
                let x0 = if i == 0 { first } else { (word[i - 1].1 + x) / 2.0 };
                let x1 = if i + 1 == word.len() { last.max(x0 + 0.1) } else { (x + word[i + 1].1) / 2.0 };
                out.push(Glyph { ch, bbox: BBox { x0, y0, x1, y1 }, size, space: false, bold: false });
            }
            prev_end = Some(last);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, step: f32) -> Line {
        let chars = text.chars().enumerate().map(|(i, c)| (c, 10.0 + step * i as f32)).collect();
        Line { x0: 0.0, y0: 0.0, x1: 10.0 + step * text.len() as f32, y1: 20.0, score: 1.0, chars }
    }

    fn text(glyphs: &[Glyph]) -> String {
        glyphs.iter().map(|g| g.ch).collect()
    }

    #[test]
    fn the_models_spaces_become_space_glyphs() {
        let g = glyphs(&[line("ab cd", 10.0)], 1.0, (0.0, 0.0));
        assert_eq!(text(&g), "ab cd");
        assert!(g[2].space);
        // A wide gap alone does not split a word.
        let mut l = line("abcd", 10.0);
        l.chars[2].1 += 10.0;
        l.chars[3].1 += 10.0;
        assert_eq!(text(&glyphs(&[l], 1.0, (0.0, 0.0))), "abcd");
    }

    #[test]
    fn letters_of_a_word_touch_and_keep_order() {
        let g = glyphs(&[line("word", 8.0)], 2.0, (100.0, 50.0));
        assert_eq!(text(&g), "word");
        for w in g.windows(2) {
            assert!((w[0].bbox.x1 - w[1].bbox.x0).abs() < 1e-3);
        }
        assert!(g[0].bbox.x0 >= 100.0 && (g[0].bbox.y0 - 50.0).abs() < 1e-3);
    }
}
