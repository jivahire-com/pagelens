//! Branded resume writer: every resume in one agency format.
//!
//! The agency's theme (logo, colour, name, footer) is set once. Each resume
//! is mapped onto standard sections (`profile`) and laid out in one column,
//! always in the same order: a header band with the candidate label, role and
//! quick facts; a career timeline (when there are at least two dated jobs);
//! summary, skills, experience, projects, achievements, education,
//! certifications and languages; then any section with an unrecognised
//! heading under its own heading, and recruiter notes last. One column reads
//! in order for people and for every parser (an ATS, an LLM) alike.
//! Personal data is removed, not labelled; the name becomes the label.

use std::path::{Path, PathBuf};

use pdfium_render::prelude::*;
use serde::Deserialize;

use crate::profile::{Entry, Item, Profile};
use crate::render::{load_fonts, normalise_symbols, script_runs, text_width, wrap, Fonts};

const W: f32 = 595.0;
const H: f32 = 842.0;
const MARGIN: f32 = 44.0;
const CONTENT_W: f32 = W - 2.0 * MARGIN;
const HEADER_STRIP: f32 = 62.0;
const BAND: f32 = 92.0;
const NEXT_TOP: f32 = 40.0;
const BOTTOM: f32 = H - 50.0;
const BODY: f32 = 9.5;
const SMALL: f32 = 8.5;
const LEADING: f32 = 1.38;
const LOGO_MAX_W: f32 = 150.0;
const LOGO_MAX_H: f32 = 36.0;
/// Timeline: bar height, label line, axis line.
const BAR_H: f32 = 12.0;

/// One agency's look, loaded from a JSON file:
/// `{"agency": "Acme Talent", "logo": "logo.png", "color": "#1F4E79",
///   "footer": "Confidential", "show_location": false}`
#[derive(Debug, Deserialize)]
pub struct Theme {
    pub agency: String,
    /// Path to a PNG or JPEG, relative to the theme file.
    #[serde(default)]
    pub logo: Option<PathBuf>,
    /// Main colour, "#RRGGBB".
    pub color: String,
    /// Optional second colour for organisation names and highlights.
    #[serde(default)]
    pub accent: Option<String>,
    /// Footer text; defaults to "Confidential".
    #[serde(default)]
    pub footer: Option<String>,
    /// Show the candidate's city in the header.
    #[serde(default)]
    pub show_location: bool,
    /// Replace the candidate's current employer (jobs dated "to present")
    /// with "Confidential", everywhere in the profile.
    #[serde(default)]
    pub hide_current_employer: bool,
}

impl Theme {
    pub fn load(path: &Path) -> Result<Theme, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("theme {}: {e}", path.display()))?;
        let mut t: Theme = serde_json::from_str(&text).map_err(|e| format!("theme {}: {e}", path.display()))?;
        if let Some(logo) = &t.logo {
            if logo.is_relative() {
                t.logo = Some(path.parent().unwrap_or(Path::new(".")).join(logo));
            }
        }
        parse_color(&t.color).ok_or_else(|| format!("theme colour {:?} is not #RRGGBB", t.color))?;
        if let Some(a) = &t.accent {
            parse_color(a).ok_or_else(|| format!("theme accent {a:?} is not #RRGGBB"))?;
        }
        Ok(t)
    }
}

/// Recruiter notes, in order, from JSON: either `{"Notice period": "30 days"}`
/// or `[{"title": "Why this candidate", "text": "..."}]`.
pub fn load_notes(path: &Path) -> Result<Vec<(String, String)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("notes {}: {e}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("notes {}: {e}", path.display()))?;
    let as_text = |v: &serde_json::Value| match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match v {
        serde_json::Value::Object(m) => Ok(m.iter().map(|(k, v)| (k.clone(), as_text(v))).collect()),
        serde_json::Value::Array(a) => Ok(a
            .iter()
            .map(|n| (n.get("title").map(as_text).unwrap_or_default(), n.get("text").map(as_text).unwrap_or_default()))
            .filter(|(t, x)| !t.is_empty() || !x.is_empty())
            .collect()),
        _ => Err("notes must be a JSON object or array".into()),
    }
}

fn parse_color(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some((p(0)?, p(2)?, p(4)?))
}

#[derive(Clone, Copy)]
struct Rgb(u8, u8, u8);

impl Rgb {
    /// Blend towards white by `t` (0 = colour, 1 = white).
    fn tint(self, t: f32) -> Rgb {
        let f = |c: u8| (c as f32 + (255.0 - c as f32) * t).round() as u8;
        Rgb(f(self.0), f(self.1), f(self.2))
    }
    fn light(self) -> bool {
        (0.2126 * self.0 as f32 + 0.7152 * self.1 as f32 + 0.0722 * self.2 as f32) / 255.0 > 0.6
    }
    fn pdf(self) -> PdfColor {
        PdfColor::new(self.0, self.1, self.2, 255)
    }
}

const INK: Rgb = Rgb(0x22, 0x26, 0x2B);
const MUTED: Rgb = Rgb(0x6B, 0x72, 0x80);
const WHITE: Rgb = Rgb(255, 255, 255);

enum Op {
    Rect { page: usize, x: f32, top: f32, w: f32, h: f32, color: Rgb },
    Text { page: usize, x: f32, top: f32, text: String, size: f32, bold: bool, color: Rgb, right: bool },
}

/// The single text column; continues on the next page when full.
struct Flow {
    page: usize,
    y: f32,
}

impl Flow {
    fn top(page: usize) -> f32 {
        if page == 0 { first_top() } else { NEXT_TOP }
    }
    fn ensure(&mut self, h: f32) {
        if self.y + h > BOTTOM {
            self.page += 1;
            self.y = Flow::top(self.page);
        }
    }
}

struct Layout {
    ops: Vec<Op>,
    f: Flow,
    color: Rgb,
    accent: Rgb,
}

impl Layout {
    fn text_at(&mut self, x: f32, text: String, size: f32, bold: bool, color: Rgb) {
        self.ops.push(Op::Text { page: self.f.page, x, top: self.f.y, text, size, bold, color, right: false });
    }

    fn text(&mut self, indent: f32, text: &str, size: f32, bold: bool, color: Rgb) {
        for line in wrap(&normalise_symbols(text), size, bold, CONTENT_W - indent) {
            self.f.ensure(size * LEADING);
            self.text_at(MARGIN + indent, line, size, bold, color);
            self.f.y += size * LEADING;
        }
    }

    fn bullet(&mut self, text: &str, size: f32) {
        self.f.ensure(size * LEADING);
        let c = self.color;
        self.text_at(MARGIN + 2.0, "•".into(), size, false, c);
        self.text(12.0, text, size, false, INK);
        self.f.y += 1.5;
    }

    fn section(&mut self, title: &str) {
        self.f.y += 10.0;
        // Keep the title with at least two lines of what follows.
        self.f.ensure(11.0 * LEADING + 8.0 + 2.0 * BODY * LEADING);
        let (page, top) = (self.f.page, self.f.y);
        self.ops.push(Op::Rect { page, x: MARGIN, top: top + 2.0, w: 4.0, h: 11.0, color: self.color });
        let c = self.color;
        self.text_at(MARGIN + 10.0, title.to_uppercase(), 10.5, true, c);
        self.f.y += 11.0 * LEADING + 1.0;
        self.ops.push(Op::Rect { page: self.f.page, x: MARGIN, top: self.f.y, w: CONTENT_W, h: 0.7, color: self.color.tint(0.75) });
        self.f.y += 7.0;
    }

    fn items(&mut self, items: &[Item], size: f32) {
        for it in items {
            match it {
                Item::Paragraph(t) => {
                    self.text(0.0, t, size, false, INK);
                    self.f.y += 3.0;
                }
                Item::Bullet(t) => self.bullet(t, size),
                Item::Subheading(t) => {
                    self.f.y += 2.0;
                    self.text(0.0, t, size + 0.5, true, INK);
                    self.f.y += 1.0;
                }
                Item::Row(cells) => {
                    let cells: Vec<&str> = cells.iter().map(String::as_str).filter(|c| !c.is_empty()).collect();
                    self.text(0.0, &cells.join("  ·  "), size, false, INK);
                    self.f.y += 2.0;
                }
            }
        }
    }

    /// Title on the left, dates right-aligned on its first line, then the
    /// organisation and the entry's lines.
    fn entry(&mut self, e: &Entry, compact: bool) {
        let size = if compact { BODY } else { BODY + 1.0 };
        self.f.ensure(2.0 * size * LEADING + 4.0);
        let dates = normalise_symbols(&e.dates);
        let (title, org) = if e.title.is_empty() { (e.org.as_str(), "") } else { (e.title.as_str(), e.org.as_str()) };
        if title.is_empty() && !dates.is_empty() {
            // Dates alone: show them as the entry's first line.
            self.text(0.0, &dates, SMALL, false, MUTED);
            self.items(&e.items, BODY);
            self.f.y += 4.0;
            return;
        }
        let dates_w = if dates.is_empty() { 0.0 } else { text_width(&dates, SMALL, false) + 12.0 };
        if !dates.is_empty() {
            self.ops.push(Op::Text { page: self.f.page, x: W - MARGIN, top: self.f.y + 1.0, text: dates, size: SMALL, bold: false, color: MUTED, right: true });
        }
        for line in wrap(&normalise_symbols(title), size, true, CONTENT_W - dates_w) {
            self.f.ensure(size * LEADING);
            self.text_at(MARGIN, line, size, true, INK);
            self.f.y += size * LEADING;
        }
        if !org.is_empty() {
            let a = self.accent;
            self.text(0.0, org, BODY, !compact, a);
        }
        self.f.y += 2.0;
        self.items(&e.items, BODY);
        self.f.y += if compact { 4.0 } else { 7.0 };
    }

    /// Small tags that wrap across the column; very long items as lines.
    fn tags(&mut self, items: &[String], indent: f32) {
        let (pad, h, gap) = (5.0, 14.0, 4.0);
        let width = CONTENT_W - indent;
        let mut x = 0.0;
        let mut started = false;
        for it in items {
            let it = normalise_symbols(it);
            if it.is_empty() {
                continue;
            }
            let w = text_width(&it, SMALL - 0.5, false) + 2.0 * pad;
            if w > width {
                if started {
                    self.f.y += h + gap;
                    x = 0.0;
                    started = false;
                }
                self.text(indent, &it, SMALL, false, INK);
                continue;
            }
            if started && x + w > width {
                self.f.y += h + gap;
                x = 0.0;
            }
            if x == 0.0 {
                self.f.ensure(h + gap);
            }
            let (page, top) = (self.f.page, self.f.y);
            self.ops.push(Op::Rect { page, x: MARGIN + indent + x, top, w, h, color: self.color.tint(0.88) });
            self.ops.push(Op::Text { page, x: MARGIN + indent + x + pad, top: top + 3.2, text: it, size: SMALL - 0.5, bold: false, color: INK, right: false });
            x += w + gap;
            started = true;
        }
        if started {
            self.f.y += h + gap;
        }
    }

    /// Roles as bars on a shared time axis, one row per role, newest on top.
    fn timeline(&mut self, roles: &[(f32, f32, String)]) {
        let start = roles.iter().map(|r| r.0).fold(f32::MAX, f32::min).floor();
        let end = roles.iter().map(|r| r.1).fold(f32::MIN, f32::max).ceil().max(start + 1.0);
        let label_w = 150.0;
        let axis_x = MARGIN + label_w;
        let axis_w = CONTENT_W - label_w;
        let at = |t: f32| axis_x + (t - start) / (end - start) * axis_w;
        let row = BAR_H + 5.0;
        self.f.ensure(roles.len() as f32 * row + 20.0);
        for (i, (s, e, title)) in roles.iter().enumerate() {
            let top = self.f.y;
            let label = fit(title, SMALL, label_w - 10.0);
            self.text_at(MARGIN, label, SMALL, false, INK);
            let color = if i % 2 == 0 { self.color } else { self.color.tint(0.35) };
            let (x0, x1) = (at(*s), at(*e).max(at(*s) + 3.0));
            self.ops.push(Op::Rect { page: self.f.page, x: x0, top: top - 1.0, w: x1 - x0, h: BAR_H - 2.0, color });
            self.f.y += row;
        }
        // Year ticks: every year, or every few years on long careers.
        let span = (end - start) as i32;
        let step = ((span + 7) / 8).max(1);
        self.ops.push(Op::Rect { page: self.f.page, x: axis_x, top: self.f.y - 2.0, w: axis_w, h: 0.6, color: self.color.tint(0.6) });
        let mut y = start as i32;
        while y <= end as i32 {
            let x = at(y as f32);
            self.ops.push(Op::Text { page: self.f.page, x: x + 6.0, top: self.f.y + 1.0, text: y.to_string(), size: 7.0, bold: false, color: MUTED, right: true });
            y += step;
        }
        self.f.y += 14.0;
    }
}

thread_local! {
    /// Height of the first page's colour band for the document being laid
    /// out (shorter when there is no role or facts line).
    static BAND_H: std::cell::Cell<f32> = const { std::cell::Cell::new(BAND) };
}

fn first_top() -> f32 {
    HEADER_STRIP + BAND_H.with(|b| b.get()) + 18.0
}

/// Shorten text to fit a width, with an ellipsis.
fn fit(text: &str, size: f32, width: f32) -> String {
    let text = normalise_symbols(text);
    if text_width(&text, size, false) <= width {
        return text;
    }
    let mut out = String::new();
    for c in text.chars() {
        if text_width(&format!("{out}{c}…"), size, false) > width {
            break;
        }
        out.push(c);
    }
    format!("{}…", out.trim_end())
}

/// Start and end of a role as fractional years ("Jul 2020 – Present").
fn span(dates: &str, now: f32) -> Option<(f32, f32)> {
    use std::sync::OnceLock;
    static POINT: OnceLock<regex::Regex> = OnceLock::new();
    let re = POINT.get_or_init(|| {
        regex::Regex::new(r"(?i)(?:\b(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?\s*(?:\d{1,2},?\s+)?|\b(\d{1,2})[/.-])?((?:19|20)\d{2})\b").unwrap()
    });
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let points: Vec<f32> = re
        .captures_iter(dates)
        .filter_map(|c| {
            let year: f32 = c[3].parse().ok()?;
            let month = c
                .get(1)
                .and_then(|m| MONTHS.iter().position(|x| m.as_str().to_lowercase().starts_with(x)))
                .or_else(|| c.get(2).and_then(|m| m.as_str().parse::<usize>().ok()).filter(|m| (1..=12).contains(m)).map(|m| m - 1))
                .unwrap_or(0);
            Some(year + month as f32 / 12.0)
        })
        .collect();
    let start = *points.first()?;
    let lower = dates.to_lowercase();
    let open = ["present", "current", "now", "till date", "to date", "ongoing"].iter().any(|w| lower.contains(w));
    let end = if open { now } else if points.len() >= 2 { points[points.len() - 1] + 1.0 / 12.0 } else { return None };
    (end > start && end - start < 60.0).then_some((start, end))
}

fn current_year() -> f32 {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    (1970.0 + secs / (365.2425 * 86400.0)) as f32
}

pub struct Branding<'t> {
    pub theme: &'t Theme,
    /// Replaces the candidate's name, e.g. "Candidate #1042".
    pub label: String,
    pub notes: Vec<(String, String)>,
}

pub fn write_branded(pdfium: &Pdfium, profile: &Profile, out: &Path, b: &Branding) -> Result<usize, String> {
    let color = Rgb::from(parse_color(&b.theme.color).unwrap_or((0x1F, 0x4E, 0x79)));
    let accent = b.theme.accent.as_deref().and_then(parse_color).map(Rgb::from).unwrap_or(color);
    let facts: Vec<String> = [
        (!profile.years.is_empty()).then(|| format!("{} years experience", profile.years)),
        (profile.experience.len() >= 2).then(|| format!("{} roles", profile.experience.len())),
        (!profile.location.is_empty()).then(|| profile.location.clone()),
    ]
    .into_iter()
    .flatten()
    .collect();
    let band_h: f32 = 50.0 + if profile.headline.is_empty() { 0.0 } else { 20.0 } + if facts.is_empty() { 0.0 } else { 20.0 };
    BAND_H.with(|b| b.set(band_h.max(58.0)));
    let mut l = Layout { ops: Vec::new(), f: Flow { page: 0, y: first_top() }, color, accent };

    // Career timeline, from the jobs whose dates can be read.
    let now = current_year();
    let roles: Vec<(f32, f32, String)> = profile
        .experience
        .iter()
        .filter_map(|e| span(&e.dates, now).map(|(s, en)| (s, en, if e.title.is_empty() { e.org.clone() } else { e.title.clone() })))
        .collect();
    if roles.len() >= 2 {
        l.section("Career timeline");
        let mut roles = roles.clone();
        roles.sort_by(|a, b| b.0.total_cmp(&a.0));
        l.timeline(&roles);
    }
    if !profile.summary.is_empty() {
        l.section("Summary");
        l.items(&profile.summary, BODY);
    }
    if !profile.skills.is_empty() {
        l.section("Skills");
        // Category names in a left gutter when the resume groups its skills.
        let named = profile.skills.iter().any(|g| !g.name.is_empty());
        let gutter = if named { 130.0 } else { 0.0 };
        for g in &profile.skills {
            if named {
                l.f.ensure(18.0);
                let name = fit(&g.name, SMALL, gutter - 10.0);
                let y0 = l.f.y + 3.0;
                l.ops.push(Op::Text { page: l.f.page, x: MARGIN, top: y0, text: name, size: SMALL, bold: true, color: INK, right: false });
            }
            l.tags(&g.items, gutter);
            l.f.y += 2.0;
        }
    }
    for (title, list, compact) in [("Experience", &profile.experience, false), ("Projects", &profile.projects, false)] {
        if !list.is_empty() {
            l.section(title);
            for e in list {
                l.entry(e, compact);
            }
        }
    }
    if !profile.achievements.is_empty() {
        l.section("Achievements");
        l.items(&profile.achievements, BODY);
    }
    if !profile.education.is_empty() {
        l.section("Education");
        for e in &profile.education {
            l.entry(e, true);
        }
    }
    if !profile.certifications.is_empty() {
        l.section("Certifications");
        for c in &profile.certifications {
            l.bullet(c, BODY);
        }
    }
    if !profile.languages.is_empty() {
        l.section("Languages");
        l.text(0.0, &profile.languages.join(", "), BODY, false, INK);
    }
    // Sections the resume has beyond the standard ones, as written.
    for x in &profile.extras {
        l.section(&x.heading);
        l.items(&x.items, BODY);
    }
    // Recruiter notes last, in a tinted panel.
    if !b.notes.is_empty() {
        l.section("Recruiter notes");
        for (title, text) in &b.notes {
            let lines = wrap(&normalise_symbols(text), BODY, false, CONTENT_W - 20.0);
            let h = 8.0 + if title.is_empty() { 0.0 } else { BODY * LEADING } + lines.len() as f32 * BODY * LEADING + 6.0;
            l.f.ensure(h.min(BOTTOM - NEXT_TOP));
            l.ops.push(Op::Rect { page: l.f.page, x: MARGIN, top: l.f.y, w: CONTENT_W, h, color: color.tint(0.92) });
            l.ops.push(Op::Rect { page: l.f.page, x: MARGIN, top: l.f.y, w: 3.0, h, color: accent });
            l.f.y += 8.0;
            if !title.is_empty() {
                l.text_at(MARGIN + 12.0, normalise_symbols(title), BODY, true, accent);
                l.f.y += BODY * LEADING;
            }
            for line in lines {
                l.text_at(MARGIN + 12.0, line, BODY, false, INK);
                l.f.y += BODY * LEADING;
            }
            l.f.y += 12.0;
        }
    }
    let pages = l.f.page + 1;

    // Header (first page only) and footers, drawn underneath the content.
    let mut bg: Vec<Op> = Vec::new();
    let on_band = if color.light() { INK } else { WHITE };
    bg.push(Op::Text { page: 0, x: W - MARGIN, top: 26.0, text: b.theme.agency.clone(), size: 9.0, bold: true, color, right: true });
    bg.push(Op::Rect { page: 0, x: 0.0, top: HEADER_STRIP, w: W, h: band_h.max(58.0), color });
    bg.push(Op::Text { page: 0, x: MARGIN, top: HEADER_STRIP + 16.0, text: b.label.clone(), size: 21.0, bold: true, color: on_band, right: false });
    let mut band_y = HEADER_STRIP + 46.0;
    if let Some(line) = wrap(&normalise_symbols(&profile.headline), 11.0, false, CONTENT_W).into_iter().next() {
        bg.push(Op::Text { page: 0, x: MARGIN, top: band_y, text: line, size: 11.0, bold: false, color: on_band, right: false });
        band_y += 18.0;
    }
    if !facts.is_empty() {
        bg.push(Op::Text { page: 0, x: MARGIN, top: band_y, text: facts.join("   ·   "), size: 9.0, bold: false, color: on_band.tint(0.15), right: false });
    }
    for p in 0..pages {
        bg.push(Op::Rect { page: p, x: MARGIN, top: H - 36.0, w: CONTENT_W, h: 0.6, color: color.tint(0.6) });
        let left = if p == 0 {
            format!("{}  ·  {}", b.theme.agency, b.theme.footer.as_deref().unwrap_or("Confidential"))
        } else {
            b.label.clone()
        };
        bg.push(Op::Text { page: p, x: MARGIN, top: H - 30.0, text: left, size: 7.5, bold: false, color: MUTED, right: false });
        bg.push(Op::Text { page: p, x: W - MARGIN, top: H - 30.0, text: format!("Page {} of {}", p + 1, pages), size: 7.5, bold: false, color: MUTED, right: true });
    }

    // Draw.
    let mut pdf = pdfium.create_new_pdf().map_err(|e| e.to_string())?;
    let fonts = load_fonts(&mut pdf)?;
    let mut doc_pages: Vec<PdfPage> = Vec::with_capacity(pages);
    for _ in 0..pages {
        doc_pages.push(pdf.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).map_err(|e| e.to_string())?);
    }
    if let Some(path) = &b.theme.logo {
        let img = image::open(path).map_err(|e| format!("logo {}: {e}", path.display()))?;
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        let scale = (LOGO_MAX_W / iw).min(LOGO_MAX_H / ih);
        let (lw, lh) = (iw * scale, ih * scale);
        let y = H - (HEADER_STRIP - lh) / 2.0 - lh;
        doc_pages[0]
            .objects_mut()
            .create_image_object(PdfPoints::new(MARGIN), PdfPoints::new(y), &img, Some(PdfPoints::new(lw)), Some(PdfPoints::new(lh)))
            .map_err(|e| e.to_string())?;
    }
    for op in bg.iter().chain(l.ops.iter()) {
        draw(&mut doc_pages, &fonts, op)?;
    }
    drop(doc_pages);
    pdf.save_to_file(out).map_err(|e| e.to_string())?;
    Ok(pages)
}

impl From<(u8, u8, u8)> for Rgb {
    fn from(c: (u8, u8, u8)) -> Rgb {
        Rgb(c.0, c.1, c.2)
    }
}

fn draw(pages: &mut [PdfPage], fonts: &Fonts, op: &Op) -> Result<(), String> {
    match op {
        Op::Rect { page, x, top, w, h, color } => {
            let rect = PdfRect::new_from_values(H - top - h, *x, H - top, x + w);
            pages[*page]
                .objects_mut()
                .create_path_object_rect(rect, None, None, Some(color.pdf()))
                .map_err(|e| e.to_string())?;
        }
        Op::Text { page, x, top, text, size, bold, color, right } => {
            if text.trim().is_empty() {
                return Ok(());
            }
            // `top` is the top of the line box; the baseline sits below it.
            let baseline = H - top - size * 0.95;
            let mut cx = if *right { x - text_width(text, *size, *bold) } else { *x };
            for (deva, run) in script_runs(text) {
                let font = fonts.pick(deva, *bold);
                let mut obj = pages[*page]
                    .objects_mut()
                    .create_text_object(PdfPoints::new(cx), PdfPoints::new(baseline), &run, font, PdfPoints::new(*size))
                    .map_err(|e| e.to_string())?;
                obj.set_fill_color(color.pdf()).map_err(|e| e.to_string())?;
                let drawn = obj.bounds().map(|q| q.to_rect().width().value).unwrap_or_else(|_| text_width(&run, *size, *bold));
                let trailing = run.chars().rev().take_while(|c| *c == ' ').count() as f32;
                cx += drawn + trailing * 0.26 * size;
            }
        }
    }
    Ok(())
}
