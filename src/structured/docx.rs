//! Word documents (.docx): body paragraphs, headings (from paragraph
//! styles or outline levels), list items (numbering), tables and text boxes,
//! in document order.

use std::collections::HashMap;
use std::path::Path;

use super::xml::{self, Node};
use super::{tidy, zip_text, Block};

pub fn read(path: &Path) -> Result<Vec<Block>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| format!("not a DOCX file: {e}"))?;
    let document = zip_text(&mut archive, "word/document.xml").ok_or("no word/document.xml")?;
    let styles = zip_text(&mut archive, "word/styles.xml").map(|s| heading_styles(&s)).unwrap_or_default();
    let list_styles = zip_text(&mut archive, "word/styles.xml").map(|s| list_styles(&s)).unwrap_or_default();
    let root = xml::parse(&document)?;
    let body = root.find("body").ok_or("no document body")?;
    let ctx = Ctx { headings: styles, lists: list_styles };
    let mut out = Vec::new();
    ctx.container(body, &mut out);
    Ok(out)
}

struct Ctx {
    /// Paragraph style id -> heading level.
    headings: HashMap<String, u8>,
    /// Paragraph style ids that are list styles.
    lists: Vec<String>,
}

impl Ctx {
    /// Block-level content of the body, a cell, a text box or a content
    /// control.
    fn container(&self, node: &Node, out: &mut Vec<Block>) {
        for n in node.elements() {
            match n.name.as_str() {
                "p" => self.paragraph(n, out),
                "tbl" => out.push(Block::Table(self.table(n))),
                "sdt" => {
                    if let Some(c) = n.child("sdtContent") {
                        self.container(c, out);
                    }
                }
                "customXml" | "ins" => self.container(n, out),
                _ => {}
            }
        }
    }

    fn paragraph(&self, p: &Node, out: &mut Vec<Block>) {
        let mut text = String::new();
        let mut boxes: Vec<&Node> = Vec::new();
        run_text(p, &mut text, &mut boxes);
        let text = tidy(&text);
        if !text.is_empty() {
            let ppr = p.child("pPr");
            let style = ppr.and_then(|x| x.child("pStyle")).and_then(|s| s.attr("w:val")).unwrap_or("");
            let outline = ppr
                .and_then(|x| x.child("outlineLvl"))
                .and_then(|o| o.attr("w:val"))
                .and_then(|v| v.parse::<u8>().ok())
                .filter(|&l| l < 9)
                .map(|l| l + 1);
            let numbered = ppr.and_then(|x| x.child("numPr")).is_some_and(|n| {
                // numId 0 means "numbering removed".
                n.child("numId").and_then(|i| i.attr("w:val")) != Some("0")
            });
            if let Some(level) = self.headings.get(style).copied().or(outline) {
                out.push(Block::Heading(level, text));
            } else if numbered || self.lists.iter().any(|l| l == style) {
                out.push(Block::ListItem(text));
            } else {
                out.push(Block::Paragraph(text));
            }
        }
        // Text boxes anchored in this paragraph follow it.
        for b in boxes {
            self.container(b, out);
        }
    }

    fn table(&self, tbl: &Node) -> Vec<Vec<String>> {
        let mut rows = Vec::new();
        for tr in tbl.elements().filter(|n| n.name == "tr") {
            let mut row = Vec::new();
            for tc in tr.elements().filter(|n| n.name == "tc") {
                let tcpr = tc.child("tcPr");
                // A cell continuing a vertical merge repeats nothing.
                let continued = tcpr
                    .and_then(|p| p.child("vMerge"))
                    .is_some_and(|v| v.attr("w:val").is_none_or(|val| val == "continue"));
                let mut blocks = Vec::new();
                if !continued {
                    self.container(tc, &mut blocks);
                }
                row.push(blocks.iter().map(block_text).collect::<Vec<_>>().join(" "));
                // A cell spanning several grid columns fills them.
                let span = tcpr
                    .and_then(|p| p.child("gridSpan"))
                    .and_then(|g| g.attr("w:val"))
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(1);
                for _ in 1..span.min(64) {
                    row.push(String::new());
                }
            }
            rows.push(row);
        }
        rows
    }
}

fn block_text(b: &Block) -> String {
    match b {
        Block::Heading(_, t) | Block::Paragraph(t) | Block::ListItem(t) => t.clone(),
        Block::Table(rows) => rows.iter().map(|r| r.join(" ")).collect::<Vec<_>>().join(" "),
    }
}

/// Visible text of a paragraph's runs; text boxes are collected separately.
fn run_text<'a>(node: &'a Node, out: &mut String, boxes: &mut Vec<&'a Node>) {
    for n in node.elements() {
        match n.name.as_str() {
            "t" => out.push_str(&n.text()),
            "tab" | "ptab" => out.push(' '),
            "br" | "cr" => out.push(' '),
            "noBreakHyphen" => out.push('-'),
            "sym" => {
                if let Some(c) = n.attr("w:char").and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32) {
                    // Symbol-font code points (F0xx) are bullets and icons.
                    if !(0xF000..=0xF0FF).contains(&(c as u32)) {
                        out.push(c);
                    }
                }
            }
            "txbxContent" => boxes.push(n),
            // Deleted text, field instructions and paragraph properties.
            "del" | "delText" | "instrText" | "pPr" | "rPr" => {}
            _ => run_text(n, out, boxes),
        }
    }
}

/// Paragraph style ids that are headings, with their level: by name
/// ("heading 1".."heading 9", "title", "subtitle") or by outline level.
fn heading_styles(xml_text: &str) -> HashMap<String, u8> {
    let mut map = HashMap::new();
    let Ok(root) = xml::parse(xml_text) else { return map };
    let mut styles = Vec::new();
    root.find_all("style", &mut styles);
    for s in styles {
        if s.attr("w:type") != Some("paragraph") {
            continue;
        }
        let Some(id) = s.attr("w:styleId") else { continue };
        let name = s.child("name").and_then(|n| n.attr("w:val")).unwrap_or("").to_ascii_lowercase();
        let level = if name == "title" {
            Some(1)
        } else if name == "subtitle" {
            Some(2)
        } else if let Some(n) = name.strip_prefix("heading ") {
            n.trim().parse::<u8>().ok().filter(|l| (1..=9).contains(l))
        } else {
            s.child("pPr")
                .and_then(|p| p.child("outlineLvl"))
                .and_then(|o| o.attr("w:val"))
                .and_then(|v| v.parse::<u8>().ok())
                .filter(|&l| l < 9)
                .map(|l| l + 1)
        };
        if let Some(l) = level {
            map.insert(id.to_string(), l.min(6));
        }
    }
    map
}

/// Paragraph style ids that are list styles (by name or numbering).
fn list_styles(xml_text: &str) -> Vec<String> {
    let Ok(root) = xml::parse(xml_text) else { return Vec::new() };
    let mut styles = Vec::new();
    root.find_all("style", &mut styles);
    styles
        .into_iter()
        .filter(|s| s.attr("w:type") == Some("paragraph"))
        .filter(|s| {
            let name = s.child("name").and_then(|n| n.attr("w:val")).unwrap_or("").to_ascii_lowercase();
            name.starts_with("list bullet") || name.starts_with("list number") || s.find("numPr").is_some()
        })
        .filter_map(|s| s.attr("w:styleId").map(String::from))
        .collect()
}
