//! Reading order: container zones + band-aware recursive XY-cut.
//!
//! 1. Segments inside a filled container (sidebar, callout box) are grouped
//!    into a zone and ordered on their own, so they never interleave with the
//!    body text next to them.
//! 2. The page is ordered with a recursive XY-cut that prefers vertical cuts
//!    (column gutters). Where no gutter spans the whole region (for example
//!    because a title or note runs across both columns), the region is split
//!    into horizontal rows, and consecutive rows are grouped into bands only
//!    while they keep a shared gutter. Each band is then cut into columns.

use crate::model::{BBox, Fill, Segment};

/// Minimum gutter width, as a multiple of the median font size. Items are
/// whole lines, so a single column never has internal gaps; real gutters
/// can be as narrow as ~0.8 em.
const GUTTER_EM: f32 = 0.5;
/// A container at least this share of the text width is a band (header),
/// not a column.
const BAND_SHARE: f32 = 0.6;
/// Slack (points) when testing whether a segment sits inside a container.
const CONTAIN_TOL: f32 = 2.0;

#[derive(Debug)]
enum Node {
    Seg(usize),
    Zone(Vec<Node>, BBox),
}

impl Node {
    fn bbox(&self, segs: &[Segment]) -> BBox {
        match self {
            Node::Seg(i) => segs[*i].bbox,
            Node::Zone(_, b) => *b,
        }
    }

    fn flatten(self, out: &mut Vec<usize>) {
        match self {
            Node::Seg(i) => out.push(i),
            Node::Zone(children, _) => children.into_iter().for_each(|c| c.flatten(out)),
        }
    }
}

pub fn reading_order(segs: &[Segment], fills: &[Fill]) -> Vec<usize> {
    if segs.is_empty() {
        return vec![];
    }
    let gutter = GUTTER_EM * median(segs.iter().map(|s| s.size).collect());
    let page_w = BBox::union_all(segs.iter().map(|s| &s.bbox)).map(|b| b.width()).unwrap_or(0.0);

    // Assign each segment to the largest container that holds it: a small
    // box nested in a sidebar belongs to the sidebar for reading order.
    let mut zone_of: Vec<Option<usize>> = vec![None; segs.len()];
    for (si, s) in segs.iter().enumerate() {
        zone_of[si] = fills
            .iter()
            .enumerate()
            .filter(|(_, f)| f.bbox.contains(&s.bbox, CONTAIN_TOL))
            .max_by(|(_, a), (_, b)| a.bbox.area().total_cmp(&b.bbox.area()))
            .map(|(fi, _)| fi);
    }

    let mut nodes: Vec<Node> = Vec::new();
    for (fi, f) in fills.iter().enumerate() {
        let members: Vec<Node> = (0..segs.len()).filter(|&si| zone_of[si] == Some(fi)).map(Node::Seg).collect();
        match members.len() {
            0 => {}
            1 => nodes.extend(members), // a single label in a box is ordinary text
            _ => {
                // Lay the zone out by its text, not its background box: the
                // box often runs right up to the neighbouring column and
                // would hide the gutter between them.
                // Wide bands (page headers) keep their full box so they still
                // span the columns below them.
                let content = BBox::union_all(members.iter().map(|n| n.bbox(segs)).collect::<Vec<_>>().iter())
                    .unwrap_or(f.bbox);
                let content = if f.bbox.width() >= BAND_SHARE * page_w { f.bbox } else { content };
                let ordered = xy_cut(members, segs, gutter);
                nodes.push(Node::Zone(ordered, content));
            }
        }
    }
    nodes.extend((0..segs.len()).filter(|&si| zone_of[si].is_none()).map(Node::Seg));

    let mut out = Vec::with_capacity(segs.len());
    xy_cut(nodes, segs, gutter).into_iter().for_each(|n| n.flatten(&mut out));
    out
}

fn xy_cut(mut nodes: Vec<Node>, segs: &[Segment], gutter: f32) -> Vec<Node> {
    if nodes.len() <= 1 {
        return nodes;
    }
    let boxes: Vec<BBox> = nodes.iter().map(|n| n.bbox(segs)).collect();

    // 1) Column gutter across the whole region: read left side, then right.
    if let Some(x) = vertical_gap(&boxes, gutter) {
        // Unless one side is only notes to the other's lines (dates or a
        // place set to the right of a job title): each note joins its line.
        if let Some(rows) = row_notes(&boxes, x, segs, &nodes) {
            let mut slots: Vec<Option<Node>> = nodes.into_iter().map(Some).collect();
            let mut merged = Vec::new();
            for mut row in rows {
                row.sort_by(|&a, &b| boxes[a].x0.total_cmp(&boxes[b].x0));
                let bbox = BBox::union_all(row.iter().map(|&i| &boxes[i])).unwrap();
                merged.push(Node::Zone(row.iter().map(|&i| slots[i].take().unwrap()).collect(), bbox));
            }
            merged.extend(slots.into_iter().flatten());
            return xy_cut(merged, segs, gutter);
        }
        let (left, right): (Vec<Node>, Vec<Node>) = nodes.into_iter().partition(|n| n.bbox(segs).x1 <= x);
        let mut out = xy_cut(left, segs, gutter);
        out.extend(xy_cut(right, segs, gutter));
        return out;
    }

    // 2) Split into rows at horizontal whitespace.
    nodes.sort_by(|a, b| a.bbox(segs).y0.total_cmp(&b.bbox(segs).y0));
    let mut rows: Vec<Vec<Node>> = Vec::new();
    let mut row_bottom = f32::NEG_INFINITY;
    for n in nodes {
        let b = n.bbox(segs);
        if rows.is_empty() || b.y0 >= row_bottom - 0.5 {
            rows.push(vec![n]);
            row_bottom = b.y1;
        } else {
            row_bottom = row_bottom.max(b.y1);
            rows.last_mut().unwrap().push(n);
        }
    }
    if rows.len() == 1 {
        // Overlapping boxes with no clean cut: fall back to top-left order.
        let mut row = rows.pop().unwrap();
        row.sort_by(|a, b| {
            let (a, b) = (a.bbox(segs), b.bbox(segs));
            a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0))
        });
        return row;
    }

    // 3) Group consecutive rows into bands that share a column gutter.
    let mut bands: Vec<Vec<Node>> = Vec::new();
    for row in rows {
        let row_boxes: Vec<BBox> = row.iter().map(|n| n.bbox(segs)).collect();
        let row_split = vertical_gap(&row_boxes, gutter).is_some();
        if let Some(band) = bands.last_mut() {
            let band_boxes: Vec<BBox> = band.iter().map(|n| n.bbox(segs)).collect();
            let band_split = vertical_gap(&band_boxes, gutter).is_some();
            let mut joined = band_boxes.clone();
            joined.extend(row_boxes.iter().copied());
            if band_split && row_split && vertical_gap(&joined, gutter).is_some() {
                band.extend(row);
                continue;
            }
            // A row that leaves the band's main gutter open (perhaps a little
            // narrower: one line runs slightly further than its column's
            // others) continues the band. A title across the columns closes
            // the gutter and starts a new band.
            if band_split {
                let main = gutters(&band_boxes, gutter).into_iter().max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)));
                if let Some((m0, m1)) = main {
                    let open = gutters(&joined, gutter).iter().any(|&(g0, g1)| g1.min(m1) - g0.max(m0) >= gutter);
                    if open {
                        band.extend(row);
                        continue;
                    }
                }
            }
            // A row that stays inside one column of the band above (a column
            // that runs on after its neighbours have ended) belongs to it.
            let gs = gutters(&band_boxes, gutter);
            let crosses = row_boxes.iter().any(|b| gs.iter().any(|&(g0, g1)| b.x0 < g1 && b.x1 > g0));
            if !gs.is_empty() && !crosses {
                band.extend(row);
                continue;
            }
        }
        bands.push(row);
    }
    if std::env::var_os("PAGELENS_DEBUG_LAYOUT").is_some() {
        for b in &bands {
            let bb: Vec<BBox> = b.iter().map(|n| n.bbox(segs)).collect();
            let u = BBox::union_all(bb.iter()).unwrap();
            eprintln!("band n={} y={:.0}-{:.0} x={:.0}-{:.0} gap={:?}", b.len(), u.y0, u.y1, u.x0, u.x1, vertical_gap(&bb, gutter));
        }
        eprintln!("--");
    }

    bands.into_iter().flat_map(|band| xy_cut(band, segs, gutter)).collect()
}

/// Longest note set apart beside a line, in characters.
const NOTE_MAX_CHARS: usize = 40;

/// Whether the side of a cut at `x` with fewer items holds only notes to
/// lines on the other side: single short lines, each level with a line
/// across the gap, and never two in a row (a sidebar's lines are; job dates
/// set to the right have the job's own lines between them). Returns each
/// note with every piece of its line ("Role at", "Company", "Apr 2020 -").
fn row_notes(boxes: &[BBox], x: f32, segs: &[Segment], nodes: &[Node]) -> Option<Vec<Vec<usize>>> {
    let (left, right): (Vec<usize>, Vec<usize>) = (0..boxes.len()).partition(|&i| boxes[i].x1 <= x);
    let (notes, lines) = if right.len() <= left.len() { (right, left) } else { (left, right) };
    if notes.is_empty() || notes.len() * 2 > lines.len() {
        return None;
    }
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut taken = vec![false; boxes.len()];
    let mut by_y = notes.clone();
    by_y.sort_by(|&a, &c| boxes[a].y0.total_cmp(&boxes[c].y0));
    for (k, &n) in by_y.iter().enumerate() {
        let Node::Seg(si) = nodes[n] else { return None };
        let b = boxes[n];
        if segs[si].text.chars().count() > NOTE_MAX_CHARS || b.height() > 1.6 * segs[si].size.max(1.0) {
            return None;
        }
        let level = |l: &usize| {
            let o = boxes[*l].y1.min(b.y1) - boxes[*l].y0.max(b.y0);
            o >= 0.5 * b.height().min(boxes[*l].height())
        };
        let row: Vec<usize> = lines.iter().copied().filter(level).collect();
        if row.is_empty() || row.iter().any(|&l| taken[l]) {
            return None;
        }
        // Between two notes there is a line on the other side that has none.
        if let Some(&prev) = k.checked_sub(1).map(|j| &by_y[j]) {
            let (top, bottom) = (boxes[prev], b);
            if !lines.iter().any(|&l| boxes[l].y0 >= top.y1 - 0.5 && boxes[l].y1 <= bottom.y0 + 0.5) {
                return None;
            }
        }
        row.iter().for_each(|&l| taken[l] = true);
        rows.push(row.into_iter().chain([n]).collect());
    }
    Some(rows)
}

/// Widest vertical whitespace strip (>= `min_gap`) that no box crosses.
/// Returns the x coordinate of its centre.
fn vertical_gap(boxes: &[BBox], min_gap: f32) -> Option<f32> {
    if boxes.len() < 2 {
        return None;
    }
    let mut spans: Vec<(f32, f32)> = boxes.iter().map(|b| (b.x0, b.x1)).collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut best: Option<(f32, f32)> = None; // (width, centre)
    let mut reach = spans[0].1;
    for &(x0, x1) in &spans[1..] {
        let gap = x0 - reach;
        if gap >= min_gap && best.is_none_or(|(w, _)| gap > w) {
            best = Some((gap, reach + gap * 0.5));
        }
        reach = reach.max(x1);
    }
    best.map(|(_, c)| c)
}

/// All vertical whitespace strips (>= `min_gap`) that no box crosses.
fn gutters(boxes: &[BBox], min_gap: f32) -> Vec<(f32, f32)> {
    let mut spans: Vec<(f32, f32)> = boxes.iter().map(|b| (b.x0, b.x1)).collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::new();
    let Some(&(_, first)) = spans.first() else { return out };
    let mut reach = first;
    for &(x0, x1) in &spans[1..] {
        if x0 - reach >= min_gap {
            out.push((reach, x0));
        }
        reach = reach.max(x1);
    }
    out
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 10.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}
