//! Resume structure for the branded writer: the parsed document mapped onto
//! standard sections (summary, experience, projects, skills, education, ...).
//!
//! Sections are recognised by their heading ("Work History" and
//! "Professional Experience" are both Experience). Experience, projects and
//! education are split into entries with title, organisation and dates.
//! A section whose heading is not recognised is kept with its heading as is,
//! in `extras`. Contact and personal-details sections are dropped, and all
//! text has personal data removed (the name becomes the given label).

use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

use crate::markdown::OutBlock;
use crate::{pii, DocResult};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub enum Item {
    Paragraph(String),
    Bullet(String),
    Subheading(String),
    Row(Vec<String>),
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Entry {
    pub title: String,
    pub org: String,
    pub dates: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct SkillGroup {
    /// Category such as "DevOps & Engineering"; empty when the resume has none.
    pub name: String,
    pub items: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Extra {
    /// The section's own heading, as written in the resume.
    pub heading: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Profile {
    /// Current role or title line ("Senior Scrum Master & Agile Coach").
    pub headline: String,
    /// Years of experience as stated ("12", "6+"); empty when not stated.
    pub years: String,
    /// City, only when asked for (`with_location`).
    pub location: String,
    pub summary: Vec<Item>,
    pub experience: Vec<Entry>,
    pub projects: Vec<Entry>,
    pub achievements: Vec<Item>,
    pub education: Vec<Entry>,
    pub skills: Vec<SkillGroup>,
    pub certifications: Vec<String>,
    pub languages: Vec<String>,
    /// Sections with headings that are not recognised, in reading order.
    pub extras: Vec<Extra>,
}

impl Profile {
    /// Hide the current employer (the jobs dated "to present"): the
    /// organisation becomes `placeholder` and every mention of it elsewhere
    /// in the profile is replaced too. Returns the names hidden.
    pub fn hide_current_employer(&mut self, placeholder: &str) -> Vec<String> {
        let current = |d: &str| {
            let d = d.to_lowercase();
            ["present", "current", "till date", "to date", "now", "ongoing", "today"].iter().any(|w| d.contains(w))
        };
        let mut orgs: Vec<String> = self
            .experience
            .iter()
            .filter(|e| current(&e.dates))
            .map(|e| e.org.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
            .filter(|o| o.chars().count() >= 2)
            .collect();
        orgs.sort_by_key(|o| std::cmp::Reverse(o.len()));
        orgs.dedup();
        if orgs.is_empty() {
            return orgs;
        }
        let scrub = |t: &mut String| {
            for o in &orgs {
                *t = replace_ci(t, o, placeholder);
            }
        };
        let scrub_items = |items: &mut Vec<Item>| {
            for it in items.iter_mut() {
                match it {
                    Item::Paragraph(t) | Item::Bullet(t) | Item::Subheading(t) => scrub(t),
                    Item::Row(cells) => cells.iter_mut().for_each(scrub),
                }
            }
        };
        scrub(&mut self.headline);
        scrub_items(&mut self.summary);
        scrub_items(&mut self.achievements);
        for e in self.experience.iter_mut().chain(self.projects.iter_mut()).chain(self.education.iter_mut()) {
            scrub(&mut e.title);
            scrub(&mut e.org);
            scrub_items(&mut e.items);
        }
        self.certifications.iter_mut().for_each(scrub);
        for x in &mut self.extras {
            scrub(&mut x.heading);
            scrub_items(&mut x.items);
        }
        orgs
    }
}

/// Replace every whole-word, case-insensitive occurrence of `what`.
fn replace_ci(text: &str, what: &str, with: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let needle = what.to_ascii_lowercase();
    let mut out = String::new();
    let mut last = 0;
    for (pos, _) in lower.match_indices(&needle) {
        let end = pos + needle.len();
        let edge = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric());
        if pos < last || edge(text[..pos].chars().next_back()) || edge(text[end..].chars().next()) {
            continue;
        }
        out.push_str(&text[last..pos]);
        out.push_str(with);
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cat {
    Summary,
    Experience,
    Projects,
    Achievements,
    Education,
    Skills,
    Certifications,
    Languages,
    Drop,
}

/// Heading phrases per section, checked in this order (whole words).
const CATEGORIES: &[(Cat, &[&str])] = &[
    (Cat::Drop, &["contact", "contacts", "personal details", "personal information", "personal data", "personal info",
        "declaration", "references", "reference", "personal particulars"]),
    (Cat::Summary, &["summary", "profile", "objective", "about me", "about", "overview", "introduction"]),
    (Cat::Projects, &["projects", "project", "assignments"]),
    (Cat::Education, &["education", "educational", "academic", "academics", "qualification", "qualifications"]),
    (Cat::Certifications, &["certifications", "certification", "certificates", "certificate", "licenses", "licences",
        "courses", "trainings", "training"]),
    (Cat::Experience, &["experience", "employment", "work history", "career history", "professional background"]),
    (Cat::Skills, &["skills", "skill", "competencies", "competency", "expertise", "technologies", "tools",
        "proficiency", "proficiencies", "tech stack", "programming"]),
    (Cat::Achievements, &["achievements", "awards", "honors", "honours", "accomplishments", "recognition"]),
    (Cat::Languages, &["languages", "language"]),
];

/// Headings that belong inside an entry ("Key Responsibilities").
const SUBHEADINGS: &[&str] = &["responsibilities", "responsibility", "roles", "role", "key deliverables", "deliverables"];

fn norm(text: &str) -> String {
    let t: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == '&' { c } else { ' ' }).collect();
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn has_phrase(norm: &str, phrase: &str) -> bool {
    format!(" {norm} ").contains(&format!(" {phrase} "))
}

/// The standard section a heading names, if any.
fn classify(heading: &str) -> Option<Cat> {
    let t = heading.trim();
    // "Project: Caterpillar" names an entry, not a section.
    if let Some((_, after)) = t.split_once(':') {
        if !after.trim().is_empty() {
            return None;
        }
    }
    let n = norm(t);
    if n.is_empty() || n.split_whitespace().count() > 5 || SUBHEADINGS.iter().any(|p| has_phrase(&n, p)) {
        return None;
    }
    CATEGORIES.iter().find(|(_, ps)| ps.iter().any(|p| has_phrase(&n, p))).map(|(c, _)| *c)
}

fn is_subheading(text: &str) -> bool {
    let n = norm(text);
    n.split_whitespace().count() <= 5 && SUBHEADINGS.iter().any(|p| has_phrase(&n, p))
}

fn date_range() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let m = r"(?:jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?";
        let date = format!(r"(?:(?:\d{{1,2}}(?:st|nd|rd|th)?\s+)?{m}\s*(?:\d{{1,2}},?\s+)?['’]?\d{{2,4}}|\d{{1,2}}[/.-](?:19|20)?\d{{2}}|(?:19|20)\d{{2}})");
        let end = format!(r"(?:{date}|present|current|now|till\s+date|to\s+date|date|ongoing)");
        Regex::new(&format!(r"(?i)\b{date}\s*(?:-|–|—|to|till|until)\s*{end}\b")).unwrap()
    })
}

fn single_year() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b(?:19|20)\d{2}\b").unwrap())
}

fn years_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(\d{1,2}(?:\.\d)?\s*(?:\+|plus)?)\s*(?:years|yrs)\b").unwrap())
}

/// "Label: value" with a short label.
fn field(text: &str) -> Option<(String, String)> {
    let (label, value) = text.split_once(':')?;
    let n = norm(label);
    let words = n.split_whitespace().count();
    (words >= 1 && words <= 4 && label.len() <= 32).then(|| (n, value.trim().to_string()))
}

fn split_list(text: &str) -> Vec<String> {
    text.split([',', ';', '|', '•', '·', '\n'])
        .map(|s| s.trim().trim_end_matches('.').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split "Role | Company | Jan 2020 – Present" into its parts.
fn parse_header(text: &str) -> (String, Vec<String>) {
    static SEP: OnceLock<Regex> = OnceLock::new();
    let sep = SEP.get_or_init(|| Regex::new(r"\s*(?:\||·|•|–|—|\s-\s|,|\s@\s|\sat\s)\s*").unwrap());
    let dates = date_range().find(text).map(|m| m.as_str().to_string()).unwrap_or_default();
    let rest = if dates.is_empty() { text.to_string() } else { text.replacen(&dates, " ", 1) };
    let rest = rest.replace(['(', ')'], " ");
    let parts: Vec<String> = sep
        .split(&rest)
        .map(|p| p.trim().trim_matches(|c: char| c == ':' || c == '-').trim().to_string())
        .filter(|p| p.chars().any(|c| c.is_alphanumeric()))
        .collect();
    (dates, parts)
}

struct Cleaner<'l> {
    label: &'l str,
}

impl Cleaner<'_> {
    /// Personal data removed, the name replaced by the label, icons dropped.
    fn text(&self, b: &OutBlock) -> String {
        self.finish(pii::redact(&b.text, &b.pii))
    }

    fn cell(&self, c: &str, b: &OutBlock) -> String {
        self.finish(crate::markdown::redact_cell(c, &b.pii))
    }

    fn finish(&self, r: String) -> String {
        let r = crate::render::drop_placeholders(&r).replace("[PERSON_NAME]", self.label);
        let r = crate::render::normalise_symbols(&r);
        // Leftover leading markers and spacing from removed values or icons.
        let r = r.trim_start_matches(|c: char| c == '-' || c == '|' || c == ':' || c.is_whitespace()).trim();
        // Only symbols left (an icon's text, "#"): nothing to keep.
        if r.chars().any(|c| c.is_alphanumeric()) { r.to_string() } else { String::new() }
    }
}

enum Place {
    Intro,
    Cat(Cat, Option<u8>),
    Extra(usize, Option<u8>),
}

/// Map a parsed resume onto the standard sections.
pub fn build(doc: &DocResult, label: &str, with_location: bool) -> Profile {
    let clean = Cleaner { label };
    let mut p = Profile::default();
    if with_location {
        p.location = doc
            .pii
            .iter()
            .filter(|f| f.kind == "location")
            .chain(doc.pii.iter().filter(|f| f.kind == "address"))
            .map(|f| f.text.trim().to_string())
            .find(|t| t.split_whitespace().count() <= 3 && t.len() <= 30)
            .unwrap_or_default();
    }
    let mut place = Place::Intro;
    let mut intro: Vec<Item> = Vec::new();
    for b in doc.pages.iter().flat_map(|pg| pg.blocks.iter()) {
        if b.kind == "table" {
            let rows: Vec<Vec<String>> = b
                .rows
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|r| r.iter().map(|c| clean.cell(c, b)).collect::<Vec<_>>())
                .filter(|r| r.iter().any(|c| !c.is_empty()))
                .collect();
            add_table(&mut p, &mut intro, &place, rows);
            continue;
        }
        let text = clean.text(b);
        if text.is_empty() {
            continue;
        }
        let short_caps = b.kind == "paragraph"
            && text.split_whitespace().count() <= 4
            && (text.ends_with(':') || text.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase()));
        if b.kind == "heading" || short_caps {
            if let Some(cat) = classify(&text) {
                place = Place::Cat(cat, b.level);
                continue;
            }
        }
        if b.kind == "heading" {
            let item_level = b.level;
            match &place {
                Place::Intro if text == label => {}
                Place::Intro => {
                    if p.headline.is_empty() && text.split_whitespace().count() <= 12 {
                        p.headline = text;
                    } else {
                        intro.push(Item::Subheading(text));
                    }
                }
                Place::Cat(cat @ (Cat::Experience | Cat::Projects | Cat::Education), _) => {
                    if is_subheading(&text) {
                        entries(&mut p, *cat).last_mut().map(|e| e.items.push(Item::Subheading(text.clone())));
                    } else if let Some((_, v)) = field(&text).filter(|(l, _)| classify(l).is_some()) {
                        // "Project: Caterpillar" becomes "Caterpillar".
                        entries(&mut p, *cat).push(Entry { title: v, ..Default::default() });
                    } else if !starts_capital(&text) {
                        // A bold word that starts in lower case continues a line.
                        entry_line(entries(&mut p, *cat), text.clone(), Item::Paragraph(text));
                    } else if *cat == Cat::Education {
                        education_line(entries(&mut p, *cat), text.clone(), Item::Paragraph(text));
                    } else {
                        // A heading is a job header, or part of the one above
                        // (dates or role set in bold on their own line).
                        header(entries(&mut p, *cat), &text, true);
                    }
                }
                // "Project: Caterpillar" or "Company: Acme" with no section
                // heading above starts an entry of that kind.
                _ if entry_heading(&text).is_some() => {
                    let (cat, title) = entry_heading(&text).unwrap();
                    entries(&mut p, cat).push(Entry { title, ..Default::default() });
                    place = Place::Cat(cat, b.level);
                }
                Place::Cat(Cat::Drop, _) => {}
                Place::Cat(_, lvl) | Place::Extra(_, lvl) if deeper(item_level, *lvl) || is_subheading(&text) => {
                    push_item(&mut p, &mut intro, &place, Item::Subheading(text));
                }
                _ => {
                    p.extras.push(Extra { heading: text, items: Vec::new() });
                    place = Place::Extra(p.extras.len() - 1, b.level);
                }
            }
            continue;
        }
        let item = if b.kind == "list_item" { Item::Bullet(text.clone()) } else { Item::Paragraph(text.clone()) };
        match &place {
            Place::Intro if text == label => {}
            Place::Intro => intro_line(&mut p, &mut intro, text, item),
            Place::Cat(Cat::Drop, _) => {}
            Place::Cat(Cat::Skills, _) => skill_line(&mut p, &text, b.kind == "list_item"),
            Place::Cat(Cat::Certifications, _) => p.certifications.push(text),
            Place::Cat(Cat::Languages, _) => p.languages.extend(split_list(&text)),
            Place::Cat(cat @ (Cat::Experience | Cat::Projects), _) => entry_line(entries(&mut p, *cat), text, item),
            Place::Cat(Cat::Education, _) => education_line(&mut p.education, text, item),
            _ => push_item(&mut p, &mut intro, &place, item),
        }
    }
    // Lines before the first section that are not the headline are the
    // summary when the resume has no summary section of its own.
    if p.summary.is_empty() {
        p.summary = intro;
    } else {
        intro.extend(std::mem::take(&mut p.summary));
        p.summary = intro;
    }
    if p.years.is_empty() {
        let text: Vec<&str> = std::iter::once(p.headline.as_str())
            .chain(p.summary.iter().filter_map(|i| match i {
                Item::Paragraph(t) | Item::Bullet(t) => Some(t.as_str()),
                _ => None,
            }))
            .collect();
        if let Some(c) = text.iter().find_map(|t| years_re().captures(t)) {
            p.years = years_text(&c[1]);
        }
    }
    p.skills.retain(|g| !g.items.is_empty());
    // An "education" entry described by sentences is a job that the reading
    // order placed after the education heading (a sidebar beside the main
    // column): it moves to experience, after the jobs already there.
    // Once one such job is found, the dated entries after it are jobs too.
    let mut seen_job = false;
    for e in std::mem::take(&mut p.education) {
        let sentences = e.items.iter().filter(|i| matches!(i, Item::Paragraph(t) | Item::Bullet(t) if t.split_whitespace().count() > 6)).count();
        seen_job |= sentences >= 2 && !e.dates.is_empty();
        if seen_job && !e.dates.is_empty() {
            p.experience.push(e);
        } else {
            p.education.push(e);
        }
    }
    for list in [&mut p.experience, &mut p.projects] {
        tidy_entries(list);
    }
    join_broken(&mut p.summary);
    join_broken(&mut p.achievements);
    for e in p.experience.iter_mut().chain(p.projects.iter_mut()).chain(p.education.iter_mut()) {
        join_broken(&mut e.items);
    }
    for x in &mut p.extras {
        join_broken(&mut x.items);
    }
    p.extras.retain(|x| !x.items.is_empty());
    p
}

impl Profile {
    /// True when nothing resume-like was found (no experience, projects,
    /// education or skills): probably not a resume.
    pub fn looks_empty(&self) -> bool {
        self.experience.is_empty() && self.projects.is_empty() && self.education.is_empty() && self.skills.is_empty()
    }
}

/// A bare short line taken for an entry title (a stray fragment, no
/// organisation, dates or content) is kept as a line of the entry above.
fn tidy_entries(list: &mut Vec<Entry>) {
    let mut out: Vec<Entry> = Vec::with_capacity(list.len());
    for e in list.drain(..) {
        let bare = e.org.is_empty() && e.dates.is_empty() && e.items.is_empty();
        // "Designed" / "developed and deployed ...": a sentence split at a
        // bold first word, not a job.
        let sentence = e.dates.is_empty() && !e.org.is_empty() && !starts_capital(&e.org);
        match out.last_mut() {
            Some(prev) if bare => prev.items.push(Item::Paragraph(e.title)),
            Some(prev) if sentence => {
                prev.items.push(Item::Paragraph(format!("{} {}", e.title, e.org)));
                prev.items.extend(e.items);
            }
            _ => out.push(e),
        }
    }
    *list = out;
}

fn entry_heading(text: &str) -> Option<(Cat, String)> {
    let (label, value) = field(text)?;
    if value.is_empty() {
        return None;
    }
    match label.as_str() {
        "project" | "project name" | "project title" | "client" => Some((Cat::Projects, value)),
        "company" | "employer" | "organisation" | "organization" => Some((Cat::Experience, value)),
        _ => None,
    }
}

/// The first letter or digit is a capital or a digit (icons are skipped).
fn starts_capital(text: &str) -> bool {
    text.chars().find(|c| c.is_alphanumeric()).is_some_and(|c| c.is_uppercase() || c.is_ascii_digit())
}

/// "10 plus" and "6 +" become "10+" and "6+".
fn years_text(raw: &str) -> String {
    raw.to_lowercase().replace("plus", "+").replace(' ', "")
}

/// Lines of one sentence broken across paragraphs (narrow columns, PDF
/// line breaks) are joined: a paragraph continues the one above when that
/// one has no closing punctuation and this one starts in lower case.
fn join_broken(items: &mut Vec<Item>) {
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    for it in items.drain(..) {
        if let (Some(Item::Paragraph(prev) | Item::Bullet(prev)), Item::Paragraph(t)) = (out.last_mut(), &it) {
            let open = !prev.trim_end().ends_with(['.', '!', '?', ':', ';']);
            if open && t.chars().next().is_some_and(|c| c.is_lowercase()) {
                prev.push(' ');
                prev.push_str(t.trim());
                continue;
            }
        }
        out.push(it);
    }
    *items = out;
}

fn deeper(item: Option<u8>, section: Option<u8>) -> bool {
    matches!((item, section), (Some(i), Some(s)) if i > s)
}

fn entries(p: &mut Profile, cat: Cat) -> &mut Vec<Entry> {
    match cat {
        Cat::Projects => &mut p.projects,
        Cat::Education => &mut p.education,
        _ => &mut p.experience,
    }
}

fn push_item(p: &mut Profile, intro: &mut Vec<Item>, place: &Place, item: Item) {
    match place {
        Place::Intro => intro.push(item),
        Place::Extra(i, _) => p.extras[*i].items.push(item),
        Place::Cat(Cat::Summary, _) => p.summary.push(item),
        Place::Cat(Cat::Achievements, _) => p.achievements.push(item),
        Place::Cat(Cat::Drop, _) => {}
        Place::Cat(cat, _) => match item {
            Item::Paragraph(t) | Item::Bullet(t) | Item::Subheading(t) => match cat {
                Cat::Skills => skill_line(p, &t, false),
                Cat::Certifications => p.certifications.push(t),
                Cat::Languages => p.languages.extend(split_list(&t)),
                _ => {
                    let e = entries(p, *cat);
                    if e.is_empty() {
                        e.push(Entry::default());
                    }
                    e.last_mut().unwrap().items.push(Item::Paragraph(t));
                }
            },
            Item::Row(r) => push_item(p, intro, &Place::Cat(*cat, None), Item::Paragraph(r.join(" · "))),
        },
    }
}

fn add_table(p: &mut Profile, intro: &mut Vec<Item>, place: &Place, rows: Vec<Vec<String>>) {
    match place {
        Place::Cat(Cat::Skills, _) => {
            // A header row ("Category | Tools / Technologies").
            let header = |r: &Vec<String>| {
                let n = norm(&r.join(" "));
                r.len() >= 2 && (has_phrase(&n, "category") || r.iter().all(|c| classify(c) == Some(Cat::Skills)))
            };
            let skip = rows.first().is_some_and(header) as usize;
            for r in rows.into_iter().skip(skip) {
                let (name, rest) = match r.as_slice() {
                    [name, rest @ ..] if !rest.is_empty() => (name.trim_end_matches(':').to_string(), rest.join(", ")),
                    _ => (String::new(), r.join(", ")),
                };
                p.skills.push(SkillGroup { name, items: split_list(&rest) });
            }
        }
        Place::Cat(Cat::Education, _) => {
            // A header row ("Degree | Institute | Year") has no digits.
            let header = rows.first().is_some_and(|r| !r.iter().any(|c| c.chars().any(|ch| ch.is_ascii_digit())));
            for r in rows.into_iter().skip(header as usize) {
                let cells: Vec<String> = r.into_iter().filter(|c| !c.is_empty()).collect();
                let dates = cells.iter().find(|c| single_year().is_match(c) && c.len() <= 20).cloned().unwrap_or_default();
                let rest: Vec<String> = cells.into_iter().filter(|c| *c != dates).collect();
                p.education.push(Entry {
                    title: rest.first().cloned().unwrap_or_default(),
                    org: rest.get(1..).map(|r| r.join(" · ")).unwrap_or_default(),
                    dates,
                    items: Vec::new(),
                });
            }
        }
        _ => {
            for r in rows {
                push_item(p, intro, place, Item::Row(r));
            }
        }
    }
}

fn intro_line(p: &mut Profile, intro: &mut Vec<Item>, text: String, item: Item) {
    if let Some((label, value)) = field(&text) {
        let is = |names: &[&str]| names.iter().any(|n| label == *n);
        if is(&["role", "current role", "designation", "title", "position", "headline", "current designation", "profile"])
            && p.headline.is_empty()
        {
            p.headline = value;
            return;
        }
        if is(&["experience", "total experience", "work experience", "years of experience", "exp"]) {
            if let Some(c) = years_re().captures(&value) {
                p.years = years_text(&c[1]);
                return;
            }
        }
    }
    let words = text.split_whitespace().count();
    if p.headline.is_empty() && intro.is_empty() && words <= 16 && !text.ends_with('.') && !text.contains(':') {
        p.headline = text;
        return;
    }
    intro.push(item);
}

fn skill_line(p: &mut Profile, text: &str, bullet: bool) {
    if let Some((label, value)) = text.split_once(':').filter(|(l, _)| field(text).is_some() && !l.trim().is_empty()) {
        p.skills.push(SkillGroup { name: label.trim().to_string(), items: split_list(value) });
        return;
    }
    let items = split_list(text);
    // A wrapped line of the group above ("SAFe, Kanban, ...") continues it.
    let continues = !bullet || items.len() > 1;
    match p.skills.last_mut() {
        Some(g) if continues || g.name.is_empty() => g.items.extend(items),
        _ => p.skills.push(SkillGroup { name: String::new(), items }),
    }
}

fn entry_line(list: &mut Vec<Entry>, text: String, item: Item) {
    let is_bullet = matches!(item, Item::Bullet(_));
    if !is_bullet {
        if let Some((label, value)) = field(&text) {
            let is = |names: &[&str]| names.iter().any(|n| label == *n);
            let last_has_items = list.last().is_some_and(|e| !e.items.is_empty());
            if is(&["company", "organisation", "organization", "employer", "client"]) {
                match list.last_mut() {
                    Some(e) if e.org.is_empty() => e.org = value,
                    // A second organisation line ("Client: ...") right
                    // under a header describes the same job.
                    Some(e) if !last_has_items => e.items.push(item),
                    _ => list.push(Entry { org: value, ..Default::default() }),
                }
                return;
            }
            if is(&["duration", "period", "dates", "date", "tenure"]) {
                match list.last_mut() {
                    Some(e) if e.dates.is_empty() => e.dates = value,
                    _ => list.push(Entry { dates: value, ..Default::default() }),
                }
                return;
            }
            if is(&["role", "designation", "title", "position"]) {
                match list.last_mut() {
                    Some(e) if e.title.is_empty() => e.title = value,
                    Some(e) => e.items.push(item),
                    None => list.push(Entry { title: value, ..Default::default() }),
                }
                return;
            }
            // Other fields ("Technology: ...") stay as lines of the entry.
            match list.last_mut() {
                Some(e) => e.items.push(item),
                None => list.push(Entry { items: vec![item], ..Default::default() }),
            }
            return;
        }
        let words = text.split_whitespace().count();
        let has_dates = date_range().is_match(&text);
        // A comma list ("Java, Spring, Kafka") is content, not "role, company".
        let commas = text.matches(',').count();
        // Headers start with a capital; a lower-case start continues the
        // line above. Without dates, a header is short (a role or company).
        let capital = starts_capital(&text);
        let header_like = capital && (has_dates || (words <= 7 && !text.ends_with('.') && commas <= 2));
        if header_like {
            header(list, &text, false);
            return;
        }
    }
    match list.last_mut() {
        Some(e) => e.items.push(item),
        None => list.push(Entry { items: vec![item], ..Default::default() }),
    }
}

/// A job header line: starts a new entry after the previous one has
/// content; otherwise fills in what the current entry lacks (dates, role,
/// organisation), so "Company | dates" then "Role" stay one job.
fn header(list: &mut Vec<Entry>, text: &str, heading: bool) {
    let has_dates = date_range().is_match(text);
    let (dates, parts) = parse_header(text);
    let starts_new = match list.last() {
        None => true,
        Some(e) => !e.items.is_empty() || (has_dates && !e.dates.is_empty()) || (heading && !parts.is_empty() && !e.title.is_empty() && !e.org.is_empty()),
    };
    if starts_new {
        list.push(Entry::default());
    }
    let e = list.last_mut().unwrap();
    if e.dates.is_empty() {
        e.dates = dates;
    }
    let mut rest: Vec<String> = Vec::new();
    for part in parts {
        if e.title.is_empty() {
            e.title = part;
        } else if e.org.is_empty() && rest.is_empty() {
            e.org = part;
        } else {
            rest.push(part);
        }
    }
    if !rest.is_empty() {
        e.items.push(Item::Subheading(rest.join(" · ")));
    }
}

fn education_line(list: &mut Vec<Entry>, text: String, item: Item) {
    // Grades and dates on their own line belong to the entry above.
    let attaches = field(&text).is_some_and(|(l, _)| {
        ["cgpa", "gpa", "percentage", "grade", "score", "marks", "year", "year of passing", "duration", "specialization",
            "specialisation", "major"]
            .contains(&l.as_str())
    }) || (date_range().find(&text).is_some_and(|m| m.as_str().len() + 4 >= text.len()));
    if attaches {
        let date_only = !text.contains(':');
        match list.last_mut() {
            Some(e) if date_only && e.dates.is_empty() => e.dates = text,
            Some(e) if !date_only => e.items.push(item),
            // Dates before the degree ("2012-2014" then "MBA ..."): the
            // dates open the next entry.
            _ => list.push(Entry { dates: text, ..Default::default() }),
        }
        return;
    }
    // Sentences and bullets describe the entry above; they are not degrees.
    let words = text.split_whitespace().count();
    let dated = date_range().is_match(&text) || single_year().is_match(&text);
    if !dated && (matches!(item, Item::Bullet(_)) || words > 10 || (text.ends_with('.') && words > 4)) {
        match list.last_mut() {
            Some(e) => e.items.push(item),
            None => list.push(Entry { items: vec![item], ..Default::default() }),
        }
        return;
    }
    // An entry opened by its dates takes the next line as its title.
    if !dated {
        if let Some(e) = list.last_mut().filter(|e| e.title.is_empty() && e.items.is_empty()) {
            let (_, parts) = parse_header(&text);
            e.title = parts.first().cloned().unwrap_or_default();
            e.org = parts.get(1..).map(|r| r.join(" · ")).unwrap_or_default();
            return;
        }
    }
    // "Bachelor of Engineering" then "SJB Institute of Technology": the
    // second line is the institution of the first.
    if !dated {
        if let Some(e) = list.last_mut().filter(|e| e.org.is_empty() && e.dates.is_empty() && e.items.is_empty()) {
            e.org = text;
            return;
        }
    }
    let (mut dates, parts) = parse_header(&text);
    let mut parts = parts;
    if dates.is_empty() {
        if let Some(i) = parts.iter().position(|p| single_year().is_match(p) && p.len() <= 12) {
            dates = parts.remove(i);
        }
    }
    list.push(Entry {
        title: parts.first().cloned().unwrap_or_default(),
        org: parts.get(1..).map(|r| r.join(" · ")).unwrap_or_default(),
        dates,
        items: Vec::new(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings() {
        assert_eq!(classify("PROFESSIONAL EXPERIENCE"), Some(Cat::Experience));
        assert_eq!(classify("Work History"), Some(Cat::Experience));
        assert_eq!(classify("Tools & Technologies"), Some(Cat::Skills));
        assert_eq!(classify("SUMMARY PROFESSIONAL:"), Some(Cat::Summary));
        assert_eq!(classify("Project Experience"), Some(Cat::Projects));
        assert_eq!(classify("Contact"), Some(Cat::Drop));
        assert_eq!(classify("Key Responsibilities & Achievements:"), None);
        assert_eq!(classify("Project: Caterpillar"), None);
        assert_eq!(classify("Volunteering"), None);
    }

    #[test]
    fn job_headers() {
        let (dates, parts) = parse_header("Senior Scrum Master / Agile Coach | Accenture | Jul 2020 – Present");
        assert_eq!(dates, "Jul 2020 – Present");
        assert_eq!(parts, vec!["Senior Scrum Master / Agile Coach", "Accenture"]);
        let (dates, _) = parse_header("Duration: Dec 15 2025 – Jan 16 2026");
        assert_eq!(dates, "Dec 15 2025 – Jan 16 2026");
    }

    #[test]
    fn skills_continue_across_lines() {
        let mut p = Profile::default();
        skill_line(&mut p, "Agile & Project Management: Scrum,", false);
        skill_line(&mut p, "SAFe, Kanban, Lean", false);
        skill_line(&mut p, "DevOps: Git, Jenkins", false);
        assert_eq!(p.skills.len(), 2);
        assert_eq!(p.skills[0].items, vec!["Scrum", "SAFe", "Kanban", "Lean"]);
    }
}
