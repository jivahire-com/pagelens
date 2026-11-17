//! A small element tree over quick-xml, enough for Office parts.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

#[derive(Debug, Default)]
pub struct Node {
    /// Local element name ("p" for "w:p").
    pub name: String,
    /// Attributes with their qualified names ("w:val", "r:id").
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Child>,
}

#[derive(Debug)]
pub enum Child {
    Element(Node),
    Text(String),
}

impl Node {
    /// Attribute by qualified name ("r:id") or, failing that, local name.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .or_else(|| self.attrs.iter().find(|(k, _)| k.rsplit(':').next() == Some(name)))
            .map(|(_, v)| v.as_str())
    }

    pub fn elements(&self) -> impl Iterator<Item = &Node> {
        self.children.iter().filter_map(|c| match c {
            Child::Element(n) => Some(n),
            Child::Text(_) => None,
        })
    }

    /// First child element with this local name.
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.elements().find(|n| n.name == name)
    }

    /// First descendant (depth first) with this local name.
    pub fn find(&self, name: &str) -> Option<&Node> {
        for n in self.elements() {
            if n.name == name {
                return Some(n);
            }
            if let Some(f) = n.find(name) {
                return Some(f);
            }
        }
        None
    }

    /// All descendants with this local name, not looking inside matches.
    pub fn find_all<'a>(&'a self, name: &str, out: &mut Vec<&'a Node>) {
        for n in self.elements() {
            if n.name == name {
                out.push(n);
            } else {
                n.find_all(name, out);
            }
        }
    }

    /// Concatenated text of all descendants.
    pub fn text(&self) -> String {
        let mut s = String::new();
        self.collect_text(&mut s);
        s
    }

    fn collect_text(&self, s: &mut String) {
        for c in &self.children {
            match c {
                Child::Text(t) => s.push_str(t),
                Child::Element(n) => n.collect_text(s),
            }
        }
    }
}

fn start(e: &BytesStart) -> Node {
    let name = e.local_name().as_ref().to_string();
    let attrs = e
        .attributes()
        .flatten()
        .map(|a| {
            let key = a.key.as_ref().to_string();
            let value = a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map(|v| v.into_owned()).unwrap_or_default();
            (key, value)
        })
        .collect();
    Node { name, attrs, children: Vec::new() }
}

/// Parse a document into a tree rooted at a synthetic node.
pub fn parse(xml: &str) -> Result<Node, String> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Node> = vec![Node::default()];
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => stack.push(start(&e)),
            Event::Empty(e) => {
                let n = start(&e);
                stack.last_mut().unwrap().children.push(Child::Element(n));
            }
            Event::End(_) => {
                if stack.len() > 1 {
                    let n = stack.pop().unwrap();
                    stack.last_mut().unwrap().children.push(Child::Element(n));
                }
            }
            Event::Text(t) => push_text(&mut stack, &t.xml10_content()),
            Event::CData(t) => push_text(&mut stack, &t.xml10_content()),
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => Some(c.to_string()),
                    _ => match r.xml10_content().as_ref() {
                        "amp" => Some("&".into()),
                        "lt" => Some("<".into()),
                        "gt" => Some(">".into()),
                        "quot" => Some("\"".into()),
                        "apos" => Some("'".into()),
                        _ => None,
                    },
                };
                if let Some(s) = resolved {
                    push_text(&mut stack, &s);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    // Unclosed elements (truncated files): fold them back up.
    while stack.len() > 1 {
        let n = stack.pop().unwrap();
        stack.last_mut().unwrap().children.push(Child::Element(n));
    }
    Ok(stack.pop().unwrap())
}

fn push_text(stack: &mut [Node], t: &str) {
    if t.is_empty() {
        return;
    }
    let node = stack.last_mut().unwrap();
    match node.children.last_mut() {
        Some(Child::Text(s)) => s.push_str(t),
        _ => node.children.push(Child::Text(t.to_string())),
    }
}
