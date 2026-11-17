//! HTML pages: headings, paragraphs, list items and tables from the markup.
//! A small tolerant tokenizer (no dependencies): unclosed and misnested
//! tags are fine, script/style and hidden elements are skipped.

use super::{tidy, Block};

/// Elements whose content is never shown as text.
const SKIP: &[&str] = &["script", "style", "noscript", "template", "svg", "math", "head", "iframe", "object", "canvas", "select", "button"];
/// Elements without an end tag.
const VOID: &[&str] = &["br", "img", "input", "meta", "link", "hr", "wbr", "source", "area", "col", "embed", "param", "track", "base"];
/// Elements that start a new block of text.
const BLOCK: &[&str] = &[
    "p", "div", "section", "article", "header", "footer", "main", "aside", "nav", "blockquote", "pre", "ul", "ol", "dl", "dt",
    "dd", "li", "figure", "figcaption", "address", "h1", "h2", "h3", "h4", "h5", "h6", "table", "caption", "form", "fieldset",
    "legend", "details", "summary", "body", "html",
];

enum Tok {
    Open(String, bool), // name, hidden
    Close(String),
    Text(String),
}

pub fn read(html: &str) -> Vec<Block> {
    let mut b = Builder::default();
    let mut skip: Vec<String> = Vec::new(); // open elements being skipped
    for tok in tokenize(html) {
        match tok {
            Tok::Open(name, hidden) => {
                if !skip.is_empty() || SKIP.contains(&name.as_str()) || hidden {
                    if !VOID.contains(&name.as_str()) {
                        skip.push(name);
                    }
                    continue;
                }
                b.open(&name);
            }
            Tok::Close(name) => {
                if !skip.is_empty() {
                    if let Some(i) = skip.iter().rposition(|n| *n == name) {
                        skip.truncate(i);
                    }
                    continue;
                }
                b.close(&name);
            }
            Tok::Text(t) => {
                if skip.is_empty() {
                    b.text(&t);
                }
            }
        }
    }
    b.finish()
}

#[derive(Default)]
struct Builder {
    out: Vec<Block>,
    buf: String,
    /// Kind of the block being filled: heading level, list item, or text.
    heading: Option<u8>,
    list_item: bool,
    /// Table being built: depth of nesting, rows, current cell.
    table_depth: usize,
    rows: Vec<Vec<String>>,
    cell: Option<String>,
}

impl Builder {
    fn flush(&mut self) {
        let t = tidy(&self.buf);
        self.buf.clear();
        if t.is_empty() {
            return;
        }
        self.out.push(match (self.heading, self.list_item) {
            (Some(l), _) => Block::Heading(l, t),
            (None, true) => Block::ListItem(t),
            _ => Block::Paragraph(t),
        });
    }

    fn end_cell(&mut self) {
        if let Some(c) = self.cell.take() {
            match self.rows.last_mut() {
                Some(r) => r.push(tidy(&c)),
                None => self.rows.push(vec![tidy(&c)]),
            }
        }
    }

    fn open(&mut self, name: &str) {
        if self.table_depth > 0 {
            match name {
                "table" => {
                    self.table_depth += 1;
                    self.text(" ");
                }
                "tr" if self.table_depth == 1 => {
                    self.end_cell();
                    self.rows.push(Vec::new());
                }
                "td" | "th" if self.table_depth == 1 => {
                    self.end_cell();
                    self.cell = Some(String::new());
                }
                // Nested tables flatten into the outer cell.
                "tr" | "td" | "th" | "br" | "p" | "div" | "li" => self.text(" "),
                _ => {}
            }
            return;
        }
        if name == "br" {
            self.text(" ");
            return;
        }
        if name == "table" {
            self.flush();
            self.table_depth = 1;
            self.rows.clear();
            self.cell = None;
            return;
        }
        if BLOCK.contains(&name) {
            self.flush();
            if let Some(level) = heading_level(name) {
                self.heading = Some(level);
            } else if name == "li" {
                self.list_item = true;
                self.heading = None;
            }
        }
    }

    fn close(&mut self, name: &str) {
        if self.table_depth > 0 {
            match name {
                "table" => {
                    self.table_depth -= 1;
                    if self.table_depth == 0 {
                        self.end_cell();
                        let rows = std::mem::take(&mut self.rows);
                        self.out.push(Block::Table(rows));
                    }
                }
                "td" | "th" if self.table_depth == 1 => self.end_cell(),
                _ => {}
            }
            return;
        }
        if BLOCK.contains(&name) {
            self.flush();
            if heading_level(name).is_some() {
                self.heading = None;
            }
            if name == "li" {
                self.list_item = false;
            }
        }
    }

    fn text(&mut self, t: &str) {
        if self.table_depth > 0 {
            // Text between cells (stray whitespace) is dropped.
            if let Some(c) = &mut self.cell {
                c.push_str(t);
            }
        } else {
            self.buf.push_str(t);
        }
    }

    fn finish(mut self) -> Vec<Block> {
        if self.table_depth > 0 {
            self.end_cell();
            let rows = std::mem::take(&mut self.rows);
            self.out.push(Block::Table(rows));
        }
        self.flush();
        self.out
    }
}

fn heading_level(name: &str) -> Option<u8> {
    match name.as_bytes() {
        [b'h', d @ b'1'..=b'6'] => Some(d - b'0'),
        _ => None,
    }
}

fn tokenize(html: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            out.push(Tok::Text(decode(rest)));
            break;
        };
        if lt > 0 {
            out.push(Tok::Text(decode(&rest[..lt])));
        }
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map(|i| &after[i + 3..]).unwrap_or("");
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = rest.find('>').map(|i| &rest[i + 1..]).unwrap_or("");
            continue;
        }
        let Some(end) = tag_end(rest) else {
            out.push(Tok::Text(decode(rest)));
            break;
        };
        let inner = &rest[1..end];
        rest = &rest[end + 1..];
        if let Some(name) = inner.strip_prefix('/') {
            out.push(Tok::Close(tag_name(name)));
            continue;
        }
        let name = tag_name(inner);
        if name.is_empty() {
            out.push(Tok::Text(decode(&format!("<{inner}>"))));
            continue;
        }
        let lower = inner.to_ascii_lowercase();
        let hidden = has_attr(&lower, "hidden")
            || lower.contains("aria-hidden=\"true\"")
            || lower.replace(' ', "").contains("display:none")
            || lower.replace(' ', "").contains("visibility:hidden");
        // Raw text elements: their content is not markup.
        if name == "script" || name == "style" {
            let close = format!("</{name}");
            let pos = rest.to_ascii_lowercase().find(&close).unwrap_or(rest.len());
            rest = &rest[pos..];
            rest = rest.find('>').map(|i| &rest[i + 1..]).unwrap_or("");
            continue;
        }
        let self_closing = inner.trim_end().ends_with('/');
        out.push(Tok::Open(name.clone(), hidden));
        if self_closing && !VOID.contains(&name.as_str()) {
            out.push(Tok::Close(name));
        }
    }
    out
}

/// Index of the '>' closing the tag at the start of `s`, skipping quoted
/// attribute values.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices().skip(1) {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(i),
            _ => {}
        }
    }
    None
}

fn tag_name(s: &str) -> String {
    s.trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == ':')
        .collect::<String>()
        .to_ascii_lowercase()
}

fn has_attr(lower_tag: &str, attr: &str) -> bool {
    lower_tag.split(|c: char| c.is_whitespace() || c == '/').skip(1).any(|a| a == attr || a.starts_with(&format!("{attr}=")))
}

/// Decode character references.
fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let semi = rest[..rest.len().min(12)].find(';');
        let decoded = semi.and_then(|i| entity(&rest[1..i]).map(|c| (c, i)));
        match decoded {
            Some((c, i)) => {
                out.push(c);
                rest = &rest[i + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let v = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(v);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bull" => '•',
        "middot" => '·',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "rupee" => '₹',
        "deg" => '°',
        "times" => '×',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structure_and_skipping() {
        let html = r#"<!doctype html><html><head><title>t</title><style>p{}</style></head><body>
            <h1>Jane&nbsp;Doe</h1><p>Email: jane@example.com<br>Pune</p>
            <script>var x = "<p>no</p>";</script>
            <div hidden>secret</div><span style="display: none">gone</span>
            <ul><li>Rust</li><li>Python &amp; SQL</li></ul>
            <table><tr><th>Year</th><th>Role</th></tr><tr><td>2024</td><td>Engineer<table><tr><td>inner</td></tr></table></td></tr></table>
            <p>Tail &#8212; done &#x41;</body></html>"#;
        let b = read(html);
        assert_eq!(b[0], Block::Heading(1, "Jane Doe".into()));
        assert_eq!(b[1], Block::Paragraph("Email: jane@example.com Pune".into()));
        assert_eq!(b[2], Block::ListItem("Rust".into()));
        assert_eq!(b[3], Block::ListItem("Python & SQL".into()));
        assert_eq!(b[4], Block::Table(vec![vec!["Year".into(), "Role".into()], vec!["2024".into(), "Engineer inner".into()]]));
        assert_eq!(b[5], Block::Paragraph("Tail — done A".into()));
        assert_eq!(b.len(), 6, "{b:?}");
    }
}
