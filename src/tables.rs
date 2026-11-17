//! Table detection.
//!
//! Ruled tables: thin horizontal and vertical strokes that touch each other
//! form a component. Their distinct x and y positions define the cell grid;
//! glyphs are assigned to cells by their centre, so cell text never leaks
//! across a rule even when two cells sit close together.
//!
//! Borderless tables: consecutive text rows that each hold several segments
//! and share at least `MIN_COLS` whitespace-separated column bands. Columns
//! come from a projection of all segment extents in the block, which handles
//! left-, right- and centre-aligned columns alike.

use serde::Serialize;

use crate::lines::build_segments;
use crate::model::{BBox, Glyph, PageContent, Segment};

const RULE_TOL: f32 = 2.0;
const MIN_COLS: usize = 3;
const MIN_ROWS: usize = 3;
/// Borderless cells are short; long cells mean multi-column prose instead.
const MAX_MEAN_CELL_CHARS: f32 = 30.0;
/// Ruled grids with fewer filled cells than this share are chart gridlines.
const MIN_FILLED_SHARE: f32 = 0.3;
/// A merged cell with at least this many text lines spanning most of a ruled
/// grid marks the grid as page layout rather than a table.
const LAYOUT_PANEL_LINES: usize = 5;
/// Horizontal rules must be at least this long to frame a table.
const MIN_HRULE_LEN: f32 = 40.0;
/// A table inside an OCR-read image must cover this share of the image.
const MIN_IMAGE_TABLE_SHARE: f32 = 0.5;

#[derive(Clone, Debug, Serialize)]
pub struct Table {
    pub bbox: BBox,
    pub rows: Vec<Vec<String>>,
    #[serde(skip)]
    pub row_boxes: Vec<BBox>,
}

pub fn detect(page: &PageContent, segs: &[Segment]) -> Vec<Table> {
    if std::env::var_os("PAGELENS_DEBUG_RULES").is_some() {
        for r in &page.rules {
            eprintln!("rule {:.1} {:.1} {:.1} {:.1}", r.x0, r.y0, r.x1, r.y1);
        }
    }
    // Inside an image read by OCR, only a ruled grid filling most of the
    // image is a table (a picture of a table). Chart axes and diagram
    // labels make small grids and aligned rows.
    let image_of = |b: &BBox| {
        let (cx, cy) = ((b.x0 + b.x1) / 2.0, b.cy());
        page.ocr_images.iter().find(|i| cx >= i.x0 && cx <= i.x1 && cy >= i.y0 && cy <= i.y1).copied()
    };
    let mut tables: Vec<Table> = ruled_tables(page)
        .into_iter()
        .filter(|t| image_of(&t.bbox).is_none_or(|i| t.bbox.area() >= MIN_IMAGE_TABLE_SHARE * i.area()))
        .collect();
    let free = |tables: &[Table]| -> Vec<&Segment> {
        segs.iter().filter(|s| !tables.iter().any(|t| t.bbox.contains(&s.bbox, RULE_TOL))).collect()
    };
    let hr: Vec<Table> = hrule_tables(page, &free(&tables), &tables).into_iter().filter(|t| image_of(&t.bbox).is_none()).collect();
    tables.extend(hr);
    let bl: Vec<Table> = borderless_tables(&free(&tables)).into_iter().filter(|t| image_of(&t.bbox).is_none()).collect();
    tables.extend(bl);
    tables
}

// ------------------------------------------------------------------ ruled

fn ruled_tables(page: &PageContent) -> Vec<Table> {
    let rules = &page.rules;
    if rules.len() < 4 {
        return vec![];
    }
    // Connected components of touching rules (union-find).
    let mut parent: Vec<usize> = (0..rules.len()).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut i = i;
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..rules.len() {
        for j in i + 1..rules.len() {
            if touches(&rules[i], &rules[j]) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    let mut comps: std::collections::BTreeMap<usize, Vec<BBox>> = Default::default();
    for i in 0..rules.len() {
        let root = find(&mut parent, i);
        comps.entry(root).or_default().push(rules[i]);
    }
    let comps = join_striped(comps.into_values().collect(), page);

    let mut out = Vec::new();
    for comp in comps.iter() {
        let xs = distinct(comp.iter().filter(|r| r.height() > r.width()).map(|r| (r.x0 + r.x1) / 2.0));
        let ys = distinct(comp.iter().filter(|r| r.width() >= r.height()).map(|r| (r.y0 + r.y1) / 2.0));
        if xs.len() < 2 || ys.len() < 2 {
            continue;
        }
        let bbox = BBox { x0: xs[0], y0: ys[0], x1: *xs.last().unwrap(), y1: *ys.last().unwrap() };
        let (nr, nc) = (ys.len() - 1, xs.len() - 1);
        // Merged cells: neighbouring grid cells with no drawn line between
        // them in that row (or column) are one cell. Its text goes to the
        // top-left grid cell; the cells it covers stay empty.
        let vrules: Vec<&BBox> = comp.iter().filter(|r| r.height() > r.width()).collect();
        let hrules: Vec<&BBox> = comp.iter().filter(|r| r.width() >= r.height()).collect();
        let mut parent: Vec<usize> = (0..nr * nc).collect();
        for r in 0..nr {
            for c in 0..nc {
                if c + 1 < nc && !covered(&vrules, xs[c + 1], ys[r], ys[r + 1], true) {
                    union(&mut parent, r * nc + c, r * nc + c + 1);
                }
                if r + 1 < nr && !covered(&hrules, ys[r + 1], xs[c], xs[c + 1], false) {
                    union(&mut parent, r * nc + c, (r + 1) * nc + c);
                }
            }
        }
        let mut cells: Vec<Vec<Vec<Glyph>>> = vec![vec![Vec::new(); nc]; nr];
        for g in &page.glyphs {
            let (cx, cy) = ((g.bbox.x0 + g.bbox.x1) / 2.0, g.bbox.cy());
            let (Some(c), Some(r)) = (slot(&xs, cx), slot(&ys, cy)) else { continue };
            // Top-left cell of the merged region (union-find roots are not
            // ordered, so take the smallest index in the group).
            let root = root_of(&mut parent, r * nc + c);
            let first = (0..nr * nc).find(|&i| root_of(&mut parent, i) == root).unwrap_or(r * nc + c);
            cells[first / nc][first % nc].push(g.clone());
        }
        // A merged cell spanning most of the grid's height with several lines
        // of text is a page-layout panel (a sidebar drawn with borders), not
        // a table cell: leave the whole grid to the layout engine.
        let layout_panel = (0..nr * nc).any(|i| {
            let root = root_of(&mut parent, i);
            if root != i {
                return false;
            }
            let rows_spanned: std::collections::BTreeSet<usize> =
                (0..nr * nc).filter(|&j| root_of(&mut parent, j) == root).map(|j| j / nc).collect();
            let (top, left) = {
                let first = (0..nr * nc).find(|&j| root_of(&mut parent, j) == root).unwrap();
                (first / nc, first % nc)
            };
            let lines = build_segments(&cells[top][left]).len();
            rows_spanned.len() >= 2 && rows_spanned.len() * 2 > nr && lines >= LAYOUT_PANEL_LINES
        });
        if layout_panel {
            continue;
        }
        let mut rows = Vec::new();
        let mut row_boxes = Vec::new();
        for (r, row) in cells.into_iter().enumerate() {
            let texts: Vec<String> = row.iter().map(|gl| cell_text(gl)).collect();
            if texts.iter().all(|t| t.is_empty()) {
                continue;
            }
            rows.push(texts);
            row_boxes.push(BBox { x0: bbox.x0, y0: ys[r], x1: bbox.x1, y1: ys[r + 1] });
        }
        // A one-row box is a caption or label frame, and a grid whose cells
        // are mostly empty is chart gridlines, not a table.
        let cells_total = rows.len() * nc;
        let filled = rows.iter().flatten().filter(|t| !t.is_empty()).count();
        // A single column of boxed entries is a list table when every box
        // holds one short line.
        let list_table = nc == 1
            && rows.len() >= 4
            && rows.iter().all(|r| !r[0].is_empty() && r[0].chars().count() <= 80)
            && row_boxes.iter().all(|b| b.height() < 30.0);
        if rows.len() >= 2 && (nc >= 2 || list_table) && filled as f32 >= MIN_FILLED_SHARE * cells_total as f32 {
            out.push(Table { bbox, rows, row_boxes });
        }
    }
    out
}

/// Striped tables shade every other row (or group of rows) and draw no
/// lines elsewhere, so each shaded band is a separate grid. Bands with the
/// same column lines that sit close together are one table: join them and
/// give the plain text rows between them, and right above and below them,
/// their own row and column lines.
fn join_striped(mut comps: Vec<Vec<BBox>>, page: &PageContent) -> Vec<Vec<BBox>> {
    let cols = |c: &[BBox]| distinct(c.iter().filter(|r| r.height() > r.width()).map(|r| (r.x0 + r.x1) / 2.0));
    let span = |c: &[BBox]| BBox::union_all(c.iter()).unwrap();
    let same_cols = |a: &[f32], b: &[f32]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 3.0);
    // Text lines (centre y, height) inside an x range, from the glyphs.
    let lines_in = |x0: f32, x1: f32, y0: f32, y1: f32| -> Vec<(f32, f32, f32, f32)> {
        let gl: Vec<Glyph> = page
            .glyphs
            .iter()
            .filter(|g| {
                let (cx, cy) = ((g.bbox.x0 + g.bbox.x1) / 2.0, g.bbox.cy());
                cx > x0 && cx < x1 && cy > y0 && cy < y1
            })
            .cloned()
            .collect();
        let mut segs = build_segments(&gl);
        segs.sort_by(|a, b| a.bbox.cy().total_cmp(&b.bbox.cy()));
        let mut rows: Vec<(f32, f32, f32, f32)> = Vec::new(); // (top, bottom, x0, x1)
        for s in segs {
            match rows.last_mut() {
                Some(r) if s.bbox.cy() < r.1 => {
                    r.1 = r.1.max(s.bbox.y1);
                    r.2 = r.2.min(s.bbox.x0);
                    r.3 = r.3.max(s.bbox.x1);
                }
                _ => rows.push((s.bbox.y0, s.bbox.y1, s.bbox.x0, s.bbox.x1)),
            }
        }
        rows
    };

    comps.sort_by(|a, b| span(a).y0.total_cmp(&span(b).y0));
    let mut out: Vec<Vec<BBox>> = Vec::new();
    let mut i = 0;
    while i < comps.len() {
        let xs = cols(&comps[i]);
        if xs.len() < 3 {
            out.push(comps[i].clone());
            i += 1;
            continue;
        }
        let (left, right) = (xs[0], *xs.last().unwrap());
        let mut group = comps[i].clone();
        let mut j = i + 1;
        while j < comps.len() && same_cols(&xs, &cols(&comps[j])) {
            let (above, below) = (span(&group), span(&comps[j]));
            let gap = lines_in(left, right, above.y1, below.y0);
            // At most a few plain rows of normal height between bands.
            let row_h = gap.iter().map(|r| r.1 - r.0).fold(0.0f32, f32::max).max(8.0);
            if below.y0 - above.y1 > 4.0 * row_h + 4.0 {
                break;
            }
            group.extend(comps[j].iter().copied());
            j += 1;
        }
        if j - i < 2 {
            out.push(comps[i].clone());
            i += 1;
            continue;
        }
        // Plain rows: between bands, and continuing past the first and last
        // band while rows keep the same spacing and stay inside the columns.
        let bb = span(&group);
        let mut rows: Vec<(f32, f32)> = lines_in(left, right, bb.y0, bb.y1).iter().map(|r| (r.0, r.1)).collect();
        let pitch = {
            let mut p: Vec<f32> = rows.windows(2).map(|w| w[1].0 - w[0].0).filter(|d| *d > 2.0).collect();
            p.sort_by(f32::total_cmp);
            p.get(p.len() / 2).copied().unwrap_or(12.0)
        };
        let fits = |r: &(f32, f32, f32, f32)| r.2 >= left - 2.0 && r.3 <= right + 2.0;
        let mut bottom = bb.y1;
        for r in lines_in(left, right, bb.y1, bb.y1 + 6.0 * pitch) {
            if r.0 - bottom > 0.8 * pitch || !fits(&r) || multi_cell(&r, page, &xs) < 2 {
                break;
            }
            rows.push((r.0, r.1));
            bottom = r.1;
        }
        let mut top = bb.y0;
        for r in lines_in(left, right, bb.y0 - 6.0 * pitch, bb.y0).into_iter().rev() {
            if top - r.1 > 0.8 * pitch || !fits(&r) || multi_cell(&r, page, &xs) < 2 {
                break;
            }
            rows.push((r.0, r.1));
            top = r.0;
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Virtual lines: a row line between every pair of rows not already
        // separated by a band edge, the outer edges, and full-height columns.
        let t = 0.5;
        let mut virt = Vec::new();
        for w in rows.windows(2) {
            let y = (w[0].1 + w[1].0) / 2.0;
            virt.push(BBox { x0: left, y0: y - t, x1: right, y1: y + t });
        }
        let (y_top, y_bot) = (top.min(bb.y0) - 1.0, bottom.max(bb.y1) + 1.0);
        virt.push(BBox { x0: left, y0: y_top - t, x1: right, y1: y_top + t });
        virt.push(BBox { x0: left, y0: y_bot - t, x1: right, y1: y_bot + t });
        // Keep only the virtual row lines that do not duplicate a band edge.
        let edges: Vec<f32> = group.iter().filter(|r| r.width() >= r.height()).map(|r| (r.y0 + r.y1) / 2.0).collect();
        virt.retain(|v| {
            let y = (v.y0 + v.y1) / 2.0;
            !edges.iter().any(|e| (e - y).abs() < 3.0)
        });
        // Drop row lines that cut through a band's interior (bands may hold
        // several text lines of one row).
        let band_spans: Vec<(f32, f32)> = comps[i..j].iter().map(|c| (span(c).y0, span(c).y1)).collect();
        virt.retain(|v| {
            let y = (v.y0 + v.y1) / 2.0;
            !band_spans.iter().any(|(a, b)| y > a + 1.0 && y < b - 1.0)
        });
        // Column lines only through the plain rows: bands keep their own
        // (possibly merged) cells.
        let mut bounds: Vec<f32> = edges.clone();
        bounds.extend(virt.iter().map(|v| (v.y0 + v.y1) / 2.0));
        let bounds = distinct(bounds.into_iter());
        for w in bounds.windows(2) {
            let mid = (w[0] + w[1]) / 2.0;
            if band_spans.iter().any(|(a, b)| mid > *a && mid < *b) {
                continue;
            }
            for &x in &xs {
                virt.push(BBox { x0: x - t, y0: w[0], x1: x + t, y1: w[1] });
            }
        }
        group.extend(virt);
        out.push(group);
        i = j;
    }
    out
}

/// How many column slots a text line fills, or 0 when any of its text runs
/// crosses an inner column line (a caption or note, not a table row).
fn multi_cell(r: &(f32, f32, f32, f32), page: &PageContent, xs: &[f32]) -> usize {
    let gl: Vec<Glyph> = page
        .glyphs
        .iter()
        .filter(|g| {
            let cy = g.bbox.cy();
            cy > r.0 && cy < r.1 && (g.bbox.x0 + g.bbox.x1) / 2.0 > xs[0] && (g.bbox.x0 + g.bbox.x1) / 2.0 < xs[xs.len() - 1]
        })
        .cloned()
        .collect();
    let inner = &xs[1..xs.len() - 1];
    let mut slots = std::collections::BTreeSet::new();
    for s in build_segments(&gl) {
        if inner.iter().any(|&x| s.bbox.x0 < x - 1.0 && s.bbox.x1 > x + 1.0) {
            return 0;
        }
        if let Some(k) = slot(xs, (s.bbox.x0 + s.bbox.x1) / 2.0) {
            slots.insert(k);
        }
    }
    slots.len()
}

fn root_of(p: &mut [usize], mut i: usize) -> usize {
    while p[i] != i {
        p[i] = p[p[i]];
        i = p[i];
    }
    i
}

fn union(p: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (root_of(p, a), root_of(p, b));
    p[ra] = rb;
}

/// Whether drawn rules cover at least half of the boundary at `at` between
/// `from` and `to` (a vertical boundary when `vertical`, else horizontal).
fn covered(rules: &[&BBox], at: f32, from: f32, to: f32, vertical: bool) -> bool {
    let len = (to - from).max(0.1);
    let total: f32 = rules
        .iter()
        .filter(|r| {
            let c = if vertical { (r.x0 + r.x1) / 2.0 } else { (r.y0 + r.y1) / 2.0 };
            (c - at).abs() <= RULE_TOL
        })
        .map(|r| {
            let (a, b) = if vertical { (r.y0, r.y1) } else { (r.x0, r.x1) };
            (b.min(to) - a.max(from)).max(0.0)
        })
        .sum();
    total >= 0.5 * len
}

fn touches(a: &BBox, b: &BBox) -> bool {
    a.x0 <= b.x1 + RULE_TOL && b.x0 <= a.x1 + RULE_TOL && a.y0 <= b.y1 + RULE_TOL && b.y0 <= a.y1 + RULE_TOL
}

fn distinct(vals: impl Iterator<Item = f32>) -> Vec<f32> {
    let mut v: Vec<f32> = vals.collect();
    v.sort_by(|a, b| a.total_cmp(b));
    let mut out: Vec<f32> = Vec::new();
    for x in v {
        if out.last().is_none_or(|&l| x - l > RULE_TOL) {
            out.push(x);
        }
    }
    out
}

fn slot(edges: &[f32], v: f32) -> Option<usize> {
    edges.windows(2).position(|w| v >= w[0] && v < w[1])
}

fn cell_text(glyphs: &[Glyph]) -> String {
    let mut segs = build_segments(glyphs);
    segs.sort_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0).then(a.bbox.x0.total_cmp(&b.bbox.x0)));
    segs.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ")
}

// ------------------------------------------------------------- borderless

fn borderless_tables(segs: &[&Segment]) -> Vec<Table> {
    if segs.len() < MIN_COLS * MIN_ROWS {
        return vec![];
    }
    let rows = text_rows(segs);
    let med_h = median_height(segs);

    // Maximal runs of consecutive multi-segment rows with normal spacing.
    let mut blocks: Vec<Vec<Vec<&Segment>>> = Vec::new();
    let mut prev_bottom = f32::NEG_INFINITY;
    for row in rows {
        let b = BBox::union_all(row.iter().map(|s| &s.bbox)).unwrap();
        let continues = b.y0 - prev_bottom < 2.0 * med_h;
        if row.len() >= 2 {
            match blocks.last_mut() {
                Some(block) if continues => block.push(row),
                _ => blocks.push(vec![row]),
            }
        } else {
            blocks.push(vec![]); // a single-segment row ends any block
        }
        prev_bottom = b.y1;
    }

    blocks.into_iter().filter(|b| b.len() >= MIN_ROWS).filter_map(|b| grid(b, med_h, segs)).collect()
}

fn text_rows<'a>(segs: &[&'a Segment]) -> Vec<Vec<&'a Segment>> {
    let mut sorted: Vec<&Segment> = segs.to_vec();
    sorted.sort_by(|a, b| a.bbox.cy().total_cmp(&b.bbox.cy()));
    let mut rows: Vec<Vec<&Segment>> = Vec::new();
    for s in sorted {
        match rows.last_mut() {
            Some(r) if (r[0].bbox.cy() - s.bbox.cy()).abs() < 0.5 * s.bbox.height().min(r[0].bbox.height()) => {
                r.push(s)
            }
            _ => rows.push(vec![s]),
        }
    }
    for r in &mut rows {
        r.sort_by(|a, b| a.bbox.x0.total_cmp(&b.bbox.x0));
    }
    rows
}

fn grid(block: Vec<Vec<&Segment>>, med_h: f32, all: &[&Segment]) -> Option<Table> {
    let cols = column_bands(&block, med_h);
    if cols.len() < MIN_COLS {
        return None;
    }
    if columns_continue(&block, &cols, med_h, all) {
        return None;
    }
    let n_cells: usize = block.iter().map(|r| r.len()).sum();
    let chars: usize = block.iter().flatten().map(|s| s.text.chars().count()).sum();
    if chars as f32 / n_cells as f32 > MAX_MEAN_CELL_CHARS {
        return None;
    }

    let mut rows = Vec::new();
    let mut row_boxes = Vec::new();
    for r in &block {
        let mut cells = vec![String::new(); cols.len()];
        for s in r {
            if let Some(ci) = nearest_band(&cols, s) {
                if !cells[ci].is_empty() {
                    cells[ci].push(' ');
                }
                cells[ci].push_str(&s.text);
            }
        }
        rows.push(cells);
        row_boxes.push(BBox::union_all(r.iter().map(|s| &s.bbox)).unwrap());
    }
    if looks_like_chart_labels(&rows) {
        return None;
    }
    let bbox = BBox::union_all(row_boxes.iter())?;
    Some(Table { bbox, rows, row_boxes })
}

/// Chart labels line up like a table: axis ticks (one or two characters
/// each), value labels over bars (mostly empty cells), and axis groups that
/// repeat the same text in every column.
fn looks_like_chart_labels(rows: &[Vec<String>]) -> bool {
    let cells: Vec<&String> = rows.iter().flatten().collect();
    let filled: Vec<&&String> = cells.iter().filter(|c| !c.is_empty()).collect();
    if filled.is_empty() {
        return true;
    }
    let sparse = (filled.len() as f32) < 0.5 * cells.len() as f32;
    let mean = filled.iter().map(|c| c.chars().count()).sum::<usize>() as f32 / filled.len() as f32;
    let repeated = rows.iter().any(|r| r.len() >= 3 && !r[0].is_empty() && r.iter().all(|c| c == &r[0]));
    sparse || mean < 2.0 || repeated
}

/// True when the candidate's columns carry on as separate text right above
/// or below it: then the "table" is a slice of a multi-column page layout
/// (rows that happen to line up across page columns), not a table. A real
/// table is bounded by text that runs across its columns, or by space.
fn columns_continue(block: &[Vec<&Segment>], cols: &[(f32, f32)], med_h: f32, all: &[&Segment]) -> bool {
    let Some(bb) = BBox::union_all(block.iter().flatten().map(|s| &s.bbox)) else { return false };
    let in_block = |s: &Segment| block.iter().flatten().any(|b| std::ptr::eq(*b, s));
    let band_of = |s: &Segment| -> Option<usize> {
        let hits: Vec<usize> =
            cols.iter().enumerate().filter(|(_, c)| s.bbox.x0 < c.1 && s.bbox.x1 > c.0).map(|(i, _)| i).collect();
        (hits.len() == 1).then(|| hits[0])
    };
    let reach = 2.5 * med_h;
    let mut above = std::collections::HashSet::new();
    let mut below = std::collections::HashSet::new();
    for s in all.iter().copied().filter(|s| !in_block(s)) {
        if s.bbox.x1 < bb.x0 || s.bbox.x0 > bb.x1 {
            continue;
        }
        let Some(band) = band_of(s) else { continue };
        if s.bbox.y1 <= bb.y0 + 1.0 && bb.y0 - s.bbox.y1 < reach {
            above.insert(band);
        } else if s.bbox.y0 >= bb.y1 - 1.0 && s.bbox.y0 - bb.y1 < reach {
            below.insert(band);
        }
    }
    above.len() >= 2 || below.len() >= 2
}

// ------------------------------------------------------- shared helpers

/// Typical line height. OCR boxes hug the ink and are shorter than the
/// line, so the font size is used when it is larger.
fn median_height(segs: &[&Segment]) -> f32 {
    let mut h: Vec<f32> = segs.iter().map(|s| s.bbox.height().max(s.size)).collect();
    if h.is_empty() {
        return 10.0;
    }
    h.sort_by(|a, b| a.total_cmp(b));
    h[h.len() / 2]
}

/// Column bands from the rows with the most common number of cells (the
/// table body). Header cells centred over several columns would otherwise
/// bridge the gaps between those columns and merge them.
fn column_bands(block: &[Vec<&Segment>], med_h: f32) -> Vec<(f32, f32)> {
    let mut counts: std::collections::BTreeMap<usize, usize> = Default::default();
    for r in block {
        *counts.entry(r.len()).or_default() += 1;
    }
    let mode = counts.iter().max_by_key(|(len, n)| (**n, **len)).map(|(len, _)| *len).unwrap_or(0);
    let body: Vec<&Vec<&Segment>> = block.iter().filter(|r| r.len() == mode).collect();
    // One body row with several cells is enough (a header and a single row
    // of values).
    let source: Vec<&Segment> = if body.len() >= 2 || (mode >= MIN_COLS && !body.is_empty()) {
        body.into_iter().flatten().copied().collect()
    } else {
        block.iter().flatten().copied().collect()
    };
    let mut spans: Vec<(f32, f32)> = source.iter().map(|s| (s.bbox.x0, s.bbox.x1)).collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut cols: Vec<(f32, f32)> = Vec::new();
    for (x0, x1) in spans {
        match cols.last_mut() {
            Some(c) if x0 <= c.1 + 0.5 * med_h => c.1 = c.1.max(x1),
            _ => cols.push((x0, x1)),
        }
    }
    cols
}

/// Band holding the segment's centre, or the nearest band to it.
fn nearest_band(cols: &[(f32, f32)], s: &Segment) -> Option<usize> {
    let cx = (s.bbox.x0 + s.bbox.x1) / 2.0;
    cols.iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| band_dist(a, cx).total_cmp(&band_dist(b, cx)))
        .map(|(i, _)| i)
}

fn band_dist(c: &(f32, f32), x: f32) -> f32 {
    if x < c.0 { c.0 - x } else if x > c.1 { x - c.1 } else { 0.0 }
}

fn build_grid(block: &[Vec<&Segment>], cols: &[(f32, f32)]) -> (Vec<Vec<String>>, Vec<BBox>) {
    let mut rows = Vec::new();
    let mut row_boxes = Vec::new();
    for r in block {
        let mut cells = vec![String::new(); cols.len()];
        for s in r {
            for (text, cx) in split_across(cols, s) {
                let ci = cols
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| band_dist(a, cx).total_cmp(&band_dist(b, cx)))
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                if !cells[ci].is_empty() {
                    cells[ci].push(' ');
                }
                cells[ci].push_str(&text);
            }
        }
        rows.push(cells);
        row_boxes.push(BBox::union_all(r.iter().map(|s| &s.bbox)).unwrap());
    }
    (rows, row_boxes)
}

/// A text run that spans several columns (headers set with single spaces
/// between column titles) split into words, each with its estimated centre.
/// Runs inside one column come back whole.
fn split_across(cols: &[(f32, f32)], s: &Segment) -> Vec<(String, f32)> {
    let cx = (s.bbox.x0 + s.bbox.x1) / 2.0;
    let spanned = cols.iter().filter(|c| s.bbox.x0 < c.1 - 1.0 && s.bbox.x1 > c.0 + 1.0).count();
    if spanned < 2 {
        return vec![(s.text.clone(), cx)];
    }
    let n = s.text.chars().count().max(1) as f32;
    let w = s.bbox.width();
    let mut out = Vec::new();
    let mut start = 0usize;
    for word in s.text.split(' ') {
        let len = word.chars().count();
        if !word.is_empty() {
            let mid = start as f32 + len as f32 / 2.0;
            out.push((word.to_string(), s.bbox.x0 + w * mid / n));
        }
        start += len + 1;
    }
    out
}

/// A vertical rule at `x` that meets the horizontal rule `h`.
fn has_side(rules: &[BBox], h: &BBox, x: f32) -> bool {
    rules.iter().any(|v| {
        v.height() > v.width() && v.height() > 4.0 && ((v.x0 + v.x1) / 2.0 - x).abs() <= RULE_TOL
            && v.y0 <= h.y1 + RULE_TOL && v.y1 >= h.y0 - RULE_TOL
    })
}

// --------------------------------------------- horizontal-rule tables

/// Tables framed only by horizontal rules (top, header and bottom lines, as
/// in most academic and book tables). Rules of matching width that stack
/// vertically frame a region; the text inside forms the rows, and columns
/// come from the body rows' alignment.
fn hrule_tables(page: &PageContent, segs: &[&Segment], existing: &[Table]) -> Vec<Table> {
    let mut hr: Vec<BBox> = page
        .rules
        .iter()
        .filter(|r| r.width() >= MIN_HRULE_LEN && r.width() >= 3.0 * r.height())
        .filter(|r| !existing.iter().any(|t| t.bbox.contains(r, RULE_TOL)))
        // Edges of a closed box (a frame around a chart or panel) have
        // vertical sides at both ends; these tables have none.
        .filter(|r| !(has_side(&page.rules, r, r.x0) && has_side(&page.rules, r, r.x1)))
        .copied()
        .collect();
    hr.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    let med_h = median_height(segs);
    let page_h = page.height.max(1.0);

    let mut used = vec![false; hr.len()];
    let mut out = Vec::new();
    for i in 0..hr.len() {
        if used[i] {
            continue;
        }
        // Rules with the same left and right ends, stacked below rule i.
        let same = |r: &BBox| (r.x0 - hr[i].x0).abs() <= 3.0 && (r.x1 - hr[i].x1).abs() <= 3.0;
        let stacked: Vec<usize> = (i..hr.len()).filter(|&j| !used[j] && same(&hr[j])).collect();
        // Stop at the first gap holding running text (a caption between two
        // tables of the same width).
        let mut group: Vec<usize> = Vec::new();
        for &j in &stacked {
            if let Some(&prev) = group.last() {
                let (a, b) = (hr[prev].y1, hr[j].y0);
                let prose = segs.iter().any(|s| {
                    let cy = s.bbox.cy();
                    cy > a && cy < b && s.text.chars().count() as f32 > 2.0 * MAX_MEAN_CELL_CHARS
                });
                if prose {
                    break;
                }
            }
            group.push(j);
        }
        if group.len() < 2 {
            continue;
        }
        let (mut top, bottom) = (hr[group[0]].y0, hr[*group.last().unwrap()].y1);
        if bottom - top > 0.6 * page_h {
            continue; // page header and footer lines, not a table
        }
        // Many resumes and forms draw a rule *under* every row, the header
        // row included, so the header sits above the first rule. Left out,
        // the column titles come out as loose headings above the table, and
        // a table continued on the next page (header plus one row) has too
        // few rows to be found at all. A row of two or more runs right above
        // the first rule, within the table's width, is the header row.
        let above: Vec<&Segment> = segs
            .iter()
            .copied()
            .filter(|s| {
                let cx = (s.bbox.x0 + s.bbox.x1) / 2.0;
                cx >= hr[i].x0 - 2.0
                    && cx <= hr[i].x1 + 2.0
                    && s.bbox.y1 <= top + 1.0
                    && top - s.bbox.y1 < 0.8 * med_h.max(s.bbox.height())
            })
            .collect();
        if let Some(row) = text_rows(&above).last().filter(|r| r.len() >= 2) {
            top = row.iter().map(|s| s.bbox.y0).fold(top, f32::min) - 0.5;
        }
        let region = BBox { x0: hr[i].x0, y0: top, x1: hr[i].x1, y1: bottom };
        let inside: Vec<&Segment> = segs
            .iter()
            .copied()
            .filter(|s| {
                let (cx, cy) = ((s.bbox.x0 + s.bbox.x1) / 2.0, s.bbox.cy());
                cx >= region.x0 - 2.0 && cx <= region.x1 + 2.0 && cy > region.y0 && cy < region.y1
            })
            .collect();
        let rows = text_rows(&inside);
        if rows.len() < 2 {
            continue;
        }
        let chars: usize = inside.iter().map(|s| s.text.chars().count()).sum();
        if chars as f32 / inside.len() as f32 > 2.0 * MAX_MEAN_CELL_CHARS {
            continue; // a boxed paragraph or quote
        }
        let mut cols = column_bands(&rows, med_h);
        // Body rows with one value each under a header with several titles
        // (columns left blank to fill in): the header row sets the columns.
        if cols.len() == 1 {
            if let Some(widest) = rows.iter().max_by_key(|r| r.len()).filter(|r| r.len() >= 2) {
                cols = widest.iter().map(|s| (s.bbox.x0, s.bbox.x1)).collect();
            }
        }
        // A one-column list is a table only in the full book style: three
        // matching rules, one header line above the second rule, and
        // several short rows below it.
        let one_column = cols.len() == 1 && group.len() == 3 && rows.len() >= 4 && {
            let header_rule = hr[group[1]].cy();
            let header_lines = rows.iter().filter(|r| r[0].bbox.cy() < header_rule).count();
            let mean = chars as f32 / inside.len() as f32;
            header_lines == 1 && mean <= MAX_MEAN_CELL_CHARS + 10.0
        };
        if cols.len() < 2 && !one_column {
            continue;
        }
        let (grid_rows, row_boxes) = build_grid(&rows, &cols);
        for &j in &group {
            used[j] = true;
        }
        out.push(Table { bbox: region, rows: grid_rows, row_boxes });
    }
    out
}
