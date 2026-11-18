//! Glyphs -> segments: horizontal runs of text on one baseline.
//!
//! Works purely from geometry, so it does not matter in which order the PDF
//! stores its text.
//!
//! 1. Runs: glyphs on one baseline are chained left to right; a run ends
//!    where the gap exceeds `SPLIT_EM` times the font size.
//! 2. Gutters: narrow column gutters can be smaller than `SPLIT_EM`. Gaps of
//!    at least `GUTTER_GAP_EM` that line up vertically over `GUTTER_LINES`
//!    or more nearby lines are treated as a gutter, and runs are split there.
//!    Ordinary word spaces almost never line up like that.

use crate::model::{BBox, Glyph, Segment};

/// Gap (in ems) above which a run is always split.
const SPLIT_EM: f32 = 1.2;
/// Font size ratio across a gap that ends a run.
const SIZE_JUMP: f32 = 1.3;
/// Gap (in ems) above which a space is inserted between glyphs.
const SPACE_EM: f32 = 0.15;
/// Max vertical centre offset (as a share of glyph height) on one baseline.
const SAME_LINE: f32 = 0.35;
/// Minimum gap (in ems) that can take part in a gutter.
const GUTTER_GAP_EM: f32 = 0.5;
/// Minimum shared width (in ems) of gaps stacked into one gutter.
const GUTTER_OVERLAP_EM: f32 = 0.2;
/// Minimum number of distinct lines a gutter must cross.
const GUTTER_LINES: usize = 4;

struct Run {
    glyphs: Vec<usize>,
    bbox: BBox,
    last_x0: f32,
    last_x1: f32,
    /// Font size of the most recent non-space glyph.
    last_size: f32,
    cy: f32,
    size: f32,
}

pub fn build_segments(glyphs: &[Glyph]) -> Vec<Segment> {
    let runs = build_runs(glyphs);
    let runs = split_on_gutters(runs, glyphs);
    let segs = runs.into_iter().filter_map(|r| to_segment(&r, glyphs)).collect();
    attach_bullets(segs)
}

/// Max gap (in ems) between a bullet and its text.
const BULLET_GAP_EM: f32 = 4.0;

/// A bullet segment, or a list number ("1.", "2)"), followed on the same
/// line by text becomes part of that text. Otherwise markers form a narrow
/// "column" of their own and reading order lists all markers first, then all
/// items. Numbers end up apart from their text when the file sets them as a
/// separate piece (a tab, or a zero-width space as Google Docs writes):
/// without this, numbered items printed as lone numbers.
fn attach_bullets(mut segs: Vec<Segment>) -> Vec<Segment> {
    let marker = |s: &Segment| is_bullet_token(s.text.trim()) || is_number_token(s.text.trim());
    let mut used = vec![false; segs.len()];
    for i in 0..segs.len() {
        if !marker(&segs[i]) {
            continue;
        }
        let b = segs[i].bbox;
        let target = (0..segs.len())
            .filter(|&j| j != i && !used[j] && !marker(&segs[j]))
            .filter(|&j| {
                let t = segs[j].bbox;
                let gap = t.x0 - b.x1;
                // Bullets often come from another font with its own baseline:
                // accept a vertical overlap, not only matching centres.
                let v_overlap = t.y1.min(b.y1) - t.y0.max(b.y0);
                gap >= -1.0
                    && gap <= BULLET_GAP_EM * segs[i].size.max(segs[j].size)
                    && ((t.cy() - b.cy()).abs() <= 0.6 * t.height().max(b.height())
                        || v_overlap >= 0.3 * t.height().min(b.height()))
            })
            .min_by(|&a, &c| segs[a].bbox.x0.total_cmp(&segs[c].bbox.x0));
        if let Some(j) = target {
            let mark = segs[i].text.trim();
            let mark = if is_number_token(mark) { mark.to_string() } else { normalise_bullet(mark) };
            segs[j].text = format!("{mark} {}", segs[j].text);
            segs[j].bbox = segs[j].bbox.union(&b);
            used[i] = true;
        }
    }
    segs.into_iter().zip(used).filter(|(_, u)| !u).map(|(s, _)| s).collect()
}

fn build_runs(glyphs: &[Glyph]) -> Vec<Run> {
    let mut order: Vec<usize> = (0..glyphs.len()).collect();
    order.sort_by(|&a, &b| glyphs[a].bbox.x0.total_cmp(&glyphs[b].bbox.x0));

    let mut open: Vec<Run> = Vec::new();
    for gi in order {
        let g = &glyphs[gi];
        let h = g.bbox.height().max(1.0);
        let size = g.size.max(1.0);
        // Best continuation: same baseline, gap within limit, nearest on the left.
        let best = open
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                (s.cy - g.bbox.cy()).abs() <= SAME_LINE * h.max(s.bbox.height())
                    && g.bbox.x0 - s.last_x1 <= SPLIT_EM * size.max(s.size)
                    // Overlap is fine when this glyph starts at or after the
                    // previous one (PDFium splits ligatures into chars that
                    // share one box, and italics overlap).
                    && (g.bbox.x0 - s.last_x1 >= -0.5 * size || g.bbox.x0 >= s.last_x0 - 0.1)
                    // A sharp change of font size across a visible gap starts
                    // a different block (a sidebar heading level with a body
                    // line); superscripts touch their base text, so stay.
                    && !(size.max(s.last_size) > SIZE_JUMP * size.min(s.last_size)
                        && g.bbox.x0 - s.last_x1 > 0.5 * size.min(s.last_size))
            })
            .min_by(|(_, a), (_, b)| (g.bbox.x0 - a.last_x1).abs().total_cmp(&(g.bbox.x0 - b.last_x1).abs()))
            .map(|(i, _)| i);
        match best {
            Some(i) => {
                let s = &mut open[i];
                s.glyphs.push(gi);
                if !g.space {
                    s.bbox = s.bbox.union(&g.bbox);
                }
                s.last_x1 = s.last_x1.max(g.bbox.x1);
                s.last_x0 = g.bbox.x0;
                if !g.space {
                    s.last_size = size;
                }
            }
            None if g.space => {} // a stray space never starts a run
            None => open.push(Run {
                glyphs: vec![gi],
                bbox: g.bbox,
                last_x0: g.bbox.x0,
                last_x1: g.bbox.x1,
                last_size: size,
                cy: g.bbox.cy(),
                size,
            }),
        }
    }
    open
}

#[derive(Clone, Copy)]
struct Gap {
    /// Run containing the gap, or None for a gap between two runs.
    run: Option<usize>,
    /// Text line the gap sits on (runs sharing a baseline share a line).
    line: usize,
    /// Index in the run's glyph list of the first glyph after the gap.
    at: usize,
    x0: f32,
    x1: f32,
    cy: f32,
    h: f32,
    size: f32,
}

fn split_on_gutters(runs: Vec<Run>, glyphs: &[Glyph]) -> Vec<Run> {
    // Group runs into text lines by baseline.
    let mut by_y: Vec<usize> = (0..runs.len()).collect();
    by_y.sort_by(|&a, &b| runs[a].cy.total_cmp(&runs[b].cy));
    let mut line_of = vec![0usize; runs.len()];
    let mut lines: Vec<Vec<usize>> = Vec::new();
    for ri in by_y {
        match lines.last_mut() {
            Some(l) if (runs[l[0]].cy - runs[ri].cy).abs() <= SAME_LINE * runs[ri].bbox.height().max(1.0) => l.push(ri),
            _ => lines.push(vec![ri]),
        }
        line_of[ri] = lines.len() - 1;
    }

    // Gaps between neighbouring runs on a line are gutter evidence too:
    // where most rows already split cleanly, the one row that did not is
    // still cut at the same place.
    let mut gaps: Vec<Gap> = Vec::new();
    for (li, l) in lines.iter().enumerate() {
        let mut row = l.clone();
        row.sort_by(|&a, &b| runs[a].bbox.x0.total_cmp(&runs[b].bbox.x0));
        for w in row.windows(2) {
            let (a, b) = (&runs[w[0]], &runs[w[1]]);
            let size = a.size.min(b.size);
            if b.bbox.x0 - a.bbox.x1 >= GUTTER_GAP_EM * size {
                gaps.push(Gap { run: None, line: li, at: 0, x0: a.bbox.x1, x1: b.bbox.x0, cy: a.cy,
                                h: a.bbox.height(), size });
            }
        }
    }

    // Wide gaps between consecutive visible glyphs inside each run.
    for (ri, r) in runs.iter().enumerate() {
        let mut prev: Option<usize> = None;
        for (k, &gi) in r.glyphs.iter().enumerate() {
            let g = &glyphs[gi];
            if g.space {
                continue;
            }
            if let Some(p) = prev {
                let px1 = glyphs[r.glyphs[p]].bbox.x1;
                if g.bbox.x0 - px1 >= GUTTER_GAP_EM * r.size {
                    gaps.push(Gap { run: Some(ri), line: line_of[ri], at: k, x0: px1, x1: g.bbox.x0, cy: r.cy,
                                    h: r.bbox.height(), size: r.size });
                }
            }
            prev = Some(k);
        }
    }
    if !gaps.iter().any(|g| g.run.is_some()) {
        return runs; // nothing to split
    }

    // Link gaps that overlap horizontally on nearby lines (union-find).
    let mut parent: Vec<usize> = (0..gaps.len()).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..gaps.len() {
        for j in i + 1..gaps.len() {
            let (a, b) = (&gaps[i], &gaps[j]);
            if a.line == b.line {
                continue;
            }
            let overlap = a.x1.min(b.x1) - a.x0.max(b.x0);
            let near = (a.cy - b.cy).abs() <= 2.5 * a.h.max(b.h);
            if near && overlap >= GUTTER_OVERLAP_EM * a.size.min(b.size) {
                let (ra, rb) = (find(&mut parent, i), find(&mut parent, j));
                parent[ra] = rb;
            }
        }
    }
    let mut lines_per_cluster: std::collections::HashMap<usize, std::collections::HashSet<usize>> =
        Default::default();
    for i in 0..gaps.len() {
        let root = find(&mut parent, i);
        lines_per_cluster.entry(root).or_default().insert(gaps[i].line);
    }
    let mut cuts: Vec<Vec<usize>> = vec![Vec::new(); runs.len()];
    for i in 0..gaps.len() {
        let root = find(&mut parent, i);
        if let Some(run) = gaps[i].run {
            if lines_per_cluster[&root].len() >= GUTTER_LINES {
                cuts[run].push(gaps[i].at);
            }
        }
    }

    // Split runs at the gutter positions.
    let mut out = Vec::with_capacity(runs.len());
    for (ri, r) in runs.into_iter().enumerate() {
        if cuts[ri].is_empty() {
            out.push(r);
            continue;
        }
        let mut c = cuts[ri].clone();
        c.sort_unstable();
        c.dedup();
        let mut start = 0;
        for &at in c.iter().chain(std::iter::once(&r.glyphs.len())) {
            let part: Vec<usize> = r.glyphs[start..at].to_vec();
            start = at;
            let visible: Vec<&Glyph> = part.iter().map(|&g| &glyphs[g]).filter(|g| !g.space).collect();
            let Some(bbox) = BBox::union_all(visible.iter().map(|g| &g.bbox)) else { continue };
            out.push(Run { glyphs: part, bbox, last_x0: bbox.x0, last_x1: bbox.x1, last_size: r.size, cy: r.cy, size: r.size });
        }
    }
    out
}

/// Letter-spaced ("tracked") text such as "P R O F I L E" leaves a gap
/// wider than a word space between every pair of letters. When most gaps in
/// a run are that wide, a word break is only a gap clearly wider than the
/// typical letter gap: returns that threshold, or None for normal text.
fn space_threshold(r: &Run, glyphs: &[Glyph]) -> Option<f32> {
    let visible: Vec<&Glyph> = r.glyphs.iter().map(|&g| &glyphs[g]).filter(|g| !g.space).collect();
    if visible.len() < 4 {
        return None;
    }
    let mut gaps: Vec<f32> = visible.windows(2).map(|w| w[1].bbox.x0 - w[0].bbox.x1).collect();
    let tracked = gaps.iter().filter(|&&g| g > SPACE_EM * r.size).count();
    if tracked * 10 < gaps.len() * 8 {
        return None;
    }
    gaps.sort_by(|a, b| a.total_cmp(b));
    let letter = gaps[gaps.len() / 3];
    Some(letter * 1.6 + 0.1 * r.size)
}

fn to_segment(r: &Run, glyphs: &[Glyph]) -> Option<Segment> {
    let mut text = String::new();
    let mut prev_x1: Option<f32> = None;
    let mut pending_space = false;
    let space_at = space_threshold(r, glyphs);
    for &gi in &r.glyphs {
        let g = &glyphs[gi];
        if g.space {
            pending_space = prev_x1.is_some() && space_at.is_none();
            continue;
        }
        if let Some(px) = prev_x1 {
            // Every space PDFium reports is kept, including the ones it only
            // inferred. Words that look split by an inferred space ("manag
            // ement") come from a space sorted after the next letter, which
            // the backend prevents; dropping inferred spaces at small gaps
            // glued phrases together ("toolsandframeworks"),
            // because in some fonts the boxes overlap across real word spaces.
            let gap = g.bbox.x0 - px;
            let space = match space_at {
                Some(t) => gap > t,
                None => pending_space || gap > SPACE_EM * r.size,
            };
            if space {
                text.push(' ');
            }
        }
        pending_space = false;
        text.push(g.ch);
        prev_x1 = Some(g.bbox.x1);
    }
    // A private-use symbol-font glyph leading the line is a bullet.
    if text.chars().next().is_some_and(is_private_use) {
        let first = text.chars().next().unwrap();
        text = format!("•{}", &text[first.len_utf8()..]);
    }
    let visible: Vec<&Glyph> = r.glyphs.iter().map(|&g| &glyphs[g]).filter(|g| !g.space).collect();
    let bold = visible.iter().filter(|g| g.bold).count() * 2 > visible.len();
    // The line's size is its typical character size, not the first glyph's
    // (a small bullet glyph would otherwise set it).
    let size = {
        let mut sizes: Vec<f32> = visible.iter().map(|g| g.size).filter(|s| *s > 0.0).collect();
        sizes.sort_by(|a, b| a.total_cmp(b));
        sizes.get(sizes.len() / 2).copied().unwrap_or(r.size)
    };
    (!text.is_empty()).then(|| Segment { text, bbox: r.bbox, size, bold, table: None })
}

/// Word exports bullets from symbol fonts as private-use code points; show
/// them as standard bullets.
pub fn normalise_bullet(marker: &str) -> String {
    match marker {
        "\u{F0A7}" | "\u{F0A8}" => "▪".to_string(),
        "\u{F0D8}" => "➢".to_string(),
        "\u{F0FC}" => "✓".to_string(),
        "o" => "◦".to_string(),
        m if m.chars().any(is_private_use) => "•".to_string(),
        other => other.to_string(),
    }
}

fn is_private_use(c: char) -> bool {
    ('\u{E000}'..='\u{F8FF}').contains(&c)
}

/// A character used as a list marker: private-use symbol-font glyphs and
/// the Unicode bullet, arrow, geometric-shape, dingbat and symbol blocks,
/// plus the ASCII markers "-", "*" and the middle dot.
pub fn is_bullet_char(c: char) -> bool {
    is_private_use(c)
        || matches!(c, '-' | '*' | '·' | '–' | '—' | '•' | '‣' | '⁃' | '∙' | '∗' | '>' | '~')
        || matches!(c as u32,
            0x2190..=0x21FF   // arrows
            | 0x2300..=0x23FF // miscellaneous technical
            | 0x25A0..=0x25FF // geometric shapes
            | 0x2600..=0x27BF // miscellaneous symbols and dingbats
            | 0x2900..=0x297F // supplemental arrows
            | 0x2B00..=0x2BFF // miscellaneous symbols and arrows
            | 0x1F300..=0x1FAFF) // pictographs and emoji
}

/// A whole segment that is only a list marker ("•", "", "o", "➢").
pub fn is_bullet_token(t: &str) -> bool {
    let n = t.chars().count();
    n >= 1 && n <= 2 && (t == "o" || t.chars().all(is_bullet_char))
}

/// A whole segment that is only a list number: one or two digits and a "."
/// or ")".
fn is_number_token(t: &str) -> bool {
    let digits = t.trim_end_matches(['.', ')']);
    digits.len() < t.len() && t.len() - digits.len() == 1 && (1..=2).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Letters set edge to edge from x, 6 pt wide at size 10, with an optional
    /// space glyph (`width` 0.01 = inferred by PDFium, 2.5 = stored in the file)
    /// before letter `at`, which starts `gap` pt after the previous letter.
    fn line(word: &str, at: usize, gap: f32, space_width: Option<f32>) -> Vec<Glyph> {
        let mut out = Vec::new();
        let mut x = 10.0;
        for (i, ch) in word.chars().enumerate() {
            if i == at {
                if let Some(w) = space_width {
                    // Inferred spaces (narrow) sit inside the previous glyph,
                    // as the backend places them.
                    let x0 = if w < 0.02 { x - 2.4 } else { x };
                    out.push(Glyph { ch: ' ', bbox: BBox { x0, y0: 0.0, x1: x0 + w, y1: 10.0 }, size: 10.0, space: true, bold: false });
                }
                x += gap;
            }
            out.push(Glyph { ch, bbox: BBox { x0: x, y0: 0.0, x1: x + 6.0, y1: 10.0 }, size: 10.0, space: false, bold: false });
            x += 6.0;
        }
        out
    }

    fn text(glyphs: &[Glyph]) -> String {
        build_segments(glyphs).iter().map(|s| s.text.clone()).collect::<Vec<_>>().join(" | ")
    }

    #[test]
    fn real_and_wide_spaces_are_kept() {
        // A space stored in the file is a word break even with no gap.
        assert_eq!(text(&line("toolsand", 5, 0.0, Some(2.5))), "tools and");
        // An inferred space at a real word gap stays.
        assert_eq!(text(&line("toolsand", 5, 3.0, Some(0.01))), "tools and");
        // Some fonts' boxes overlap across a real word space.
        assert_eq!(text(&line("toolsand", 5, -0.5, Some(0.01))), "tools and");
    }

    fn word_at(word: &str, x: f32) -> Vec<Glyph> {
        word.chars()
            .enumerate()
            .map(|(i, ch)| Glyph { ch, bbox: BBox { x0: x + 6.0 * i as f32, y0: 0.0, x1: x + 6.0 * (i as f32 + 1.0), y1: 10.0 }, size: 10.0, space: false, bold: false })
            .collect()
    }

    #[test]
    fn list_number_set_apart_joins_its_text() {
        let mut g = word_at("1.", 10.0);
        g.extend(word_at("Built", 60.0));
        assert_eq!(text(&g), "1. Built");
        // A year or a count is not a list number.
        let mut g = word_at("2019", 10.0);
        g.extend(word_at("Built", 80.0));
        assert_eq!(text(&g), "2019 | Built");
    }
}
