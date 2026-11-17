//! PowerPoint decks (.pptx): one page per slide in presentation order.
//! Title placeholders become headings, body placeholder paragraphs become
//! list items, other text boxes paragraphs, and tables keep their cells.
//! Shapes are read top to bottom, then left to right.

use std::collections::HashMap;
use std::path::Path;

use super::xml::{self, Node};
use super::{tidy, zip_text, Block};

pub fn read(path: &Path) -> Result<Vec<Vec<Block>>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| format!("not a PPTX file: {e}"))?;
    let mut pages = Vec::new();
    for slide in slide_paths(&mut archive)? {
        let Some(text) = zip_text(&mut archive, &slide) else { continue };
        let root = xml::parse(&text)?;
        let Some(tree) = root.find("spTree") else {
            pages.push(Vec::new());
            continue;
        };
        let mut shapes: Vec<Shape> = Vec::new();
        collect_shapes(tree, (0, 0), &mut shapes);
        shapes.sort_by_key(|s| (s.y / ROW_TOLERANCE_EMU, s.x));
        pages.push(shapes.into_iter().flat_map(|s| s.blocks).collect());
    }
    Ok(pages)
}

/// Shapes whose tops are this close (EMU; 914400 per inch) count as one row.
const ROW_TOLERANCE_EMU: i64 = 914_400 / 4;

struct Shape {
    x: i64,
    y: i64,
    blocks: Vec<Block>,
}

/// Slide part names in presentation order.
fn slide_paths<R: std::io::Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>) -> Result<Vec<String>, String> {
    let pres = zip_text(archive, "ppt/presentation.xml").ok_or("no ppt/presentation.xml")?;
    let rels = zip_text(archive, "ppt/_rels/presentation.xml.rels").unwrap_or_default();
    let rels = xml::parse(&rels)?;
    let mut targets: HashMap<String, String> = HashMap::new();
    let mut nodes = Vec::new();
    rels.find_all("Relationship", &mut nodes);
    for r in nodes {
        if let (Some(id), Some(t)) = (r.attr("Id"), r.attr("Target")) {
            let t = t.trim_start_matches('/');
            let path = if t.starts_with("ppt/") { t.to_string() } else { format!("ppt/{t}") };
            targets.insert(id.to_string(), path);
        }
    }
    let pres = xml::parse(&pres)?;
    let mut ids = Vec::new();
    pres.find_all("sldId", &mut ids);
    let mut out: Vec<String> = ids.iter().filter_map(|s| s.attr("r:id")).filter_map(|id| targets.get(id).cloned()).collect();
    if out.is_empty() {
        // No usable order: fall back to slide numbers.
        let mut names: Vec<String> = archive
            .file_names()
            .filter_map(|n| n.ok().map(|n| n.to_string()))
            .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
            .collect();
        names.sort_by_key(|n| n.trim_start_matches("ppt/slides/slide").trim_end_matches(".xml").parse::<u32>().unwrap_or(u32::MAX));
        out = names;
    }
    Ok(out)
}

fn offset(node: &Node) -> (i64, i64) {
    node.find("xfrm")
        .and_then(|x| x.child("off"))
        .map(|o| {
            let v = |k: &str| o.attr(k).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            (v("x"), v("y"))
        })
        .unwrap_or((0, 0))
}

fn collect_shapes(tree: &Node, origin: (i64, i64), out: &mut Vec<Shape>) {
    for n in tree.elements() {
        match n.name.as_str() {
            "sp" => {
                let (x, y) = offset(n);
                let placeholder = n.find("ph").map(|p| p.attr("type").unwrap_or("body").to_string());
                let blocks = n.child("txBody").map(|b| text_body(b, placeholder.as_deref())).unwrap_or_default();
                if !blocks.is_empty() {
                    out.push(Shape { x: origin.0 + x, y: origin.1 + y, blocks });
                }
            }
            "graphicFrame" => {
                let (x, y) = offset(n);
                if let Some(tbl) = n.find("tbl") {
                    out.push(Shape { x: origin.0 + x, y: origin.1 + y, blocks: vec![Block::Table(table(tbl))] });
                }
            }
            "grpSp" => {
                let (x, y) = offset(n);
                collect_shapes(n, (origin.0 + x, origin.1 + y), out);
            }
            _ => {}
        }
    }
}

fn text_body(body: &Node, placeholder: Option<&str>) -> Vec<Block> {
    let mut out = Vec::new();
    for p in body.elements().filter(|n| n.name == "p") {
        let text = tidy(&paragraph_text(p));
        if text.is_empty() {
            continue;
        }
        let bulleted = p.child("pPr").is_some_and(|pr| pr.child("buChar").is_some() || pr.child("buAutoNum").is_some());
        out.push(match placeholder {
            Some("title" | "ctrTitle") => Block::Heading(1, text),
            Some("subTitle") => Block::Heading(2, text),
            Some("body" | "obj") => Block::ListItem(text),
            _ if bulleted => Block::ListItem(text),
            _ => Block::Paragraph(text),
        });
    }
    // A multi-line title is one heading.
    if matches!(placeholder, Some("title" | "ctrTitle")) && out.len() > 1 {
        let joined = out.iter().filter_map(|b| if let Block::Heading(_, t) = b { Some(t.as_str()) } else { None }).collect::<Vec<_>>().join(" ");
        return vec![Block::Heading(1, joined)];
    }
    out
}

fn paragraph_text(p: &Node) -> String {
    let mut s = String::new();
    for n in p.elements() {
        match n.name.as_str() {
            "r" | "fld" => {
                if let Some(t) = n.child("t") {
                    s.push_str(&t.text());
                }
            }
            "br" => s.push(' '),
            _ => {}
        }
    }
    s
}

fn table(tbl: &Node) -> Vec<Vec<String>> {
    tbl.elements()
        .filter(|n| n.name == "tr")
        .map(|tr| {
            tr.elements()
                .filter(|n| n.name == "tc")
                .map(|tc| {
                    // Cells covered by a merge are empty placeholders.
                    if tc.attr("hMerge") == Some("1") || tc.attr("vMerge") == Some("1") {
                        return String::new();
                    }
                    let mut paras = Vec::new();
                    if let Some(body) = tc.child("txBody") {
                        for p in body.elements().filter(|n| n.name == "p") {
                            let t = tidy(&paragraph_text(p));
                            if !t.is_empty() {
                                paras.push(t);
                            }
                        }
                    }
                    paras.join(" ")
                })
                .collect()
        })
        .collect()
}
