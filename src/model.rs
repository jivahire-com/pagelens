//! Backend-neutral page model. Coordinates are PDF points with the origin at
//! the top-left of the page and y growing downwards (reading direction).

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl BBox {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    pub fn cy(&self) -> f32 {
        (self.y0 + self.y1) * 0.5
    }

    pub fn area(&self) -> f32 {
        self.width().max(0.0) * self.height().max(0.0)
    }

    pub fn union(&self, o: &BBox) -> BBox {
        BBox {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    /// True if `o` lies inside `self`, allowing `tol` points of slack.
    pub fn contains(&self, o: &BBox, tol: f32) -> bool {
        o.x0 >= self.x0 - tol && o.x1 <= self.x1 + tol && o.y0 >= self.y0 - tol && o.y1 <= self.y1 + tol
    }

    pub fn union_all<'a>(it: impl IntoIterator<Item = &'a BBox>) -> Option<BBox> {
        it.into_iter().copied().reduce(|a, b| a.union(&b))
    }
}

/// One visible character as decoded by the backend.
#[derive(Clone, Debug)]
pub struct Glyph {
    pub ch: char,
    pub bbox: BBox,
    pub size: f32,
    /// A space character written in the PDF. Used as a word-break hint;
    /// never starts a segment and never widens a segment's box.
    pub space: bool,
    /// Set in a bold font (weight 600+ or a bold face name).
    pub bold: bool,
}

/// A filled shape large enough to act as a container (sidebar, callout).
#[derive(Clone, Debug)]
pub struct Fill {
    pub bbox: BBox,
}

/// Raw page content produced by a backend.
#[derive(Clone, Debug, Default)]
pub struct PageContent {
    pub width: f32,
    pub height: f32,
    pub glyphs: Vec<Glyph>,
    pub fills: Vec<Fill>,
    /// Thin horizontal or vertical strokes, used later for ruled tables.
    pub rules: Vec<BBox>,
    /// Raster image placements.
    pub images: Vec<BBox>,
    /// Every vector path (bars, lines, icons), for spotting charts.
    pub graphics: Vec<BBox>,
    /// Every visibly filled shape of any size (row shading, bars, badges).
    pub shaded: Vec<BBox>,
    /// White glyphs with no known background under them (to be checked
    /// against the rendered page).
    pub white_suspects: usize,
    /// Number of vector path objects (many paths and no text suggests text
    /// converted to outlines).
    pub path_count: usize,
    /// Set when the page had no usable text layer.
    pub ocr: Option<crate::ocr::OcrStatus>,
    /// Text present in the file but not visible on the page (invisible
    /// render mode, transparent, microscopic, or white on white).
    pub hidden_glyphs: Vec<Glyph>,
    /// Images on a digital page whose text was read by OCR. Only ruled
    /// tables are taken from them: chart and diagram labels align like
    /// borderless tables.
    pub ocr_images: Vec<BBox>,
    /// Clickable links: the area and where it goes (a web address, mailto:
    /// or tel:).
    pub links: Vec<(BBox, String)>,
    /// Images that look like printed text but were not read (no OCR engine):
    /// the caller can send them to a vision model.
    pub unread_images: Vec<BBox>,
}

/// A horizontal run of text on one baseline, without large internal gaps.
#[derive(Clone, Debug, Serialize)]
pub struct Segment {
    pub text: String,
    pub bbox: BBox,
    pub size: f32,
    /// Most of the segment's characters are bold.
    #[serde(default)]
    pub bold: bool,
    /// For a table-row item: index of its table on the page.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub table: Option<usize>,
}
