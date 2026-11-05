//! MySQL's XML functions `EXTRACTVALUE` and `UPDATEXML`, with a small XML reader and the part of XPath 1.0 they are used
//! with: absolute and relative paths, `//`, `*`, `@attribute`, `text()`, `.`, `..`, the predicates `[n]`, `[last()]`,
//! `[@a]`, `[@a='v']`, `[child]`, `[child='v']`, `[text()='v']`, `[position() op n]`, and `count(path)`.

use crate::error::{Error, Result};

#[derive(Debug, Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Child>,
    parent: usize,
    /// byte range of the element in the source
    span: (usize, usize),
}

#[derive(Debug)]
enum Child {
    Elem(usize),
    Text(String),
}

struct Doc {
    nodes: Vec<Node>,
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn parse_xml(src: &str) -> Option<Doc> {
    let b = src.as_bytes();
    let mut doc = Doc { nodes: vec![Node::default()] };
    let mut stack: Vec<usize> = vec![0];
    let mut i = 0;
    let mut text = String::new();
    let flush = |doc: &mut Doc, stack: &[usize], text: &mut String| {
        if !text.trim().is_empty() {
            let t = unescape(text.trim());
            doc.nodes[*stack.last().expect("a node")].children.push(Child::Text(t));
        }
        text.clear();
    };
    while i < b.len() {
        if b[i] != b'<' {
            let start = i;
            while i < b.len() && b[i] != b'<' {
                i += 1;
            }
            text.push_str(&src[start..i]);
            continue;
        }
        if src[i..].starts_with("<!--") {
            i += src[i..].find("-->")? + 3;
            continue;
        }
        if src[i..].starts_with("<![CDATA[") {
            let end = src[i..].find("]]>")?;
            let t = src[i + 9..i + end].to_string();
            flush(&mut doc, &stack, &mut text);
            if !t.is_empty() {
                doc.nodes[*stack.last()?].children.push(Child::Text(t));
            }
            i += end + 3;
            continue;
        }
        if src[i..].starts_with("<?") || src[i..].starts_with("<!") {
            i += src[i..].find('>')? + 1;
            continue;
        }
        flush(&mut doc, &stack, &mut text);
        if b.get(i + 1) == Some(&b'/') {
            let end = i + src[i..].find('>')?;
            let name = src[i + 2..end].trim();
            let top = stack.pop()?;
            if stack.is_empty() || doc.nodes[top].name != name {
                return None;
            }
            doc.nodes[top].span.1 = end + 1;
            i = end + 1;
            continue;
        }
        // an opening tag
        let mut j = i + 1;
        while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' && b[j] != b'/' {
            j += 1;
        }
        let name = src[i + 1..j].to_string();
        if name.is_empty() {
            return None;
        }
        let mut attrs = Vec::new();
        loop {
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            match b.get(j)? {
                b'>' | b'/' => break,
                _ => {
                    let s = j;
                    while j < b.len() && b[j] != b'=' && !b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    let key = src[s..j].to_string();
                    while j < b.len() && b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if b.get(j) != Some(&b'=') {
                        return None;
                    }
                    j += 1;
                    while j < b.len() && b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    let q = *b.get(j)?;
                    if q != b'"' && q != b'\'' {
                        return None;
                    }
                    let e = j + 1 + src[j + 1..].find(q as char)?;
                    attrs.push((key, unescape(&src[j + 1..e])));
                    j = e + 1;
                }
            }
        }
        let self_closing = b[j] == b'/';
        let close = if self_closing { j + 1 } else { j };
        if b.get(close) != Some(&b'>') {
            return None;
        }
        let parent = *stack.last()?;
        let id = doc.nodes.len();
        doc.nodes.push(Node { name, attrs, children: Vec::new(), parent, span: (i, close + 1) });
        doc.nodes[parent].children.push(Child::Elem(id));
        if !self_closing {
            stack.push(id);
        }
        i = close + 1;
    }
    flush(&mut doc, &stack, &mut text);
    (stack.len() == 1).then_some(doc)
}

#[derive(Clone, Debug)]
enum Item {
    Elem(usize),
    Attr(String),
    Text(String),
}

#[derive(Debug)]
enum Test {
    Name(String),
    Any,
    Attr(Option<String>),
    Text,
    Node,
    This,
    Parent,
}

#[derive(Debug)]
enum Pred {
    Index(usize),
    Last,
    Position(String, f64),
    Attr(String, Option<String>),
    Child(String, Option<String>),
    TextIs(String),
}

#[derive(Debug)]
struct Step {
    deep: bool,
    test: Test,
    preds: Vec<Pred>,
}

fn parse_path(path: &str) -> Option<(bool, Vec<Step>)> {
    let b: Vec<char> = path.trim().chars().collect();
    let mut i = 0;
    let absolute = b.first() == Some(&'/');
    let mut steps = Vec::new();
    let mut first = true;
    while i < b.len() || first {
        let mut deep = false;
        if b.get(i) == Some(&'/') {
            i += 1;
            if b.get(i) == Some(&'/') {
                deep = true;
                i += 1;
            }
        } else if !first {
            return None;
        }
        first = false;
        if i >= b.len() {
            break;
        }
        let start = i;
        let test = if b[i] == '@' {
            i += 1;
            if b.get(i) == Some(&'*') {
                i += 1;
                Test::Attr(None)
            } else {
                let s = i;
                while i < b.len() && (b[i].is_alphanumeric() || matches!(b[i], '_' | '-' | ':' | '.')) {
                    i += 1;
                }
                Test::Attr(Some(b[s..i].iter().collect()))
            }
        } else if b[i] == '*' {
            i += 1;
            Test::Any
        } else if b[i] == '.' && b.get(i + 1) == Some(&'.') {
            i += 2;
            Test::Parent
        } else if b[i] == '.' {
            i += 1;
            Test::This
        } else {
            let s = i;
            while i < b.len() && (b[i].is_alphanumeric() || matches!(b[i], '_' | '-' | ':' | '.')) {
                i += 1;
            }
            let name: String = b[s..i].iter().collect();
            if name.is_empty() {
                return None;
            }
            if b.get(i) == Some(&'(') && b.get(i + 1) == Some(&')') {
                i += 2;
                match name.as_str() {
                    "text" => Test::Text,
                    "node" => Test::Node,
                    _ => return None,
                }
            } else {
                Test::Name(name)
            }
        };
        let _ = start;
        let mut preds = Vec::new();
        while b.get(i) == Some(&'[') {
            let mut depth = 0;
            let s = i + 1;
            while i < b.len() {
                match b[i] {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    '\'' | '"' => {
                        let q = b[i];
                        i += 1;
                        while i < b.len() && b[i] != q {
                            i += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            let body: String = b.get(s..i)?.iter().collect();
            i += 1;
            preds.push(parse_pred(body.trim())?);
        }
        steps.push(Step { deep, test, preds });
    }
    Some((absolute, steps))
}

fn literal(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() >= 2 && (s.starts_with('\'') && s.ends_with('\'') || s.starts_with('"') && s.ends_with('"')) {
        Some(s[1..s.len() - 1].to_string())
    } else {
        None
    }
}

fn parse_pred(body: &str) -> Option<Pred> {
    if let Ok(n) = body.parse::<usize>() {
        return Some(Pred::Index(n));
    }
    if body == "last()" {
        return Some(Pred::Last);
    }
    if let Some(rest) = body.strip_prefix("position()") {
        let rest = rest.trim();
        for op in ["<=", ">=", "!=", "<", ">", "="] {
            if let Some(n) = rest.strip_prefix(op) {
                return Some(Pred::Position(op.to_string(), n.trim().parse().ok()?));
            }
        }
        return None;
    }
    let (lhs, rhs) = match body.split_once('=') {
        Some((l, r)) => (l.trim(), Some(literal(r)?)),
        None => (body, None),
    };
    if let Some(a) = lhs.strip_prefix('@') {
        return Some(Pred::Attr(a.to_string(), rhs));
    }
    if lhs == "text()" || lhs == "." {
        return rhs.map(Pred::TextIs);
    }
    if lhs.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')) && !lhs.is_empty() {
        return Some(Pred::Child(lhs.to_string(), rhs));
    }
    None
}

impl Doc {
    fn text_of(&self, id: usize) -> String {
        self.nodes[id].children.iter().find_map(|c| if let Child::Text(t) = c { Some(t.clone()) } else { None }).unwrap_or_default()
    }

    fn descendants(&self, id: usize, out: &mut Vec<usize>) {
        for c in &self.nodes[id].children {
            if let Child::Elem(e) = c {
                out.push(*e);
                self.descendants(*e, out);
            }
        }
    }

    fn eval(&self, absolute: bool, steps: &[Step]) -> Vec<Item> {
        let _ = absolute;
        let mut current: Vec<Item> = vec![Item::Elem(0)];
        for step in steps {
            let mut next: Vec<Item> = Vec::new();
            for item in &current {
                let Item::Elem(id) = item else { continue };
                let mut pool: Vec<usize> = vec![*id];
                if step.deep {
                    self.descendants(*id, &mut pool);
                }
                for p in pool {
                    let mut found: Vec<Item> = match &step.test {
                        Test::Name(n) => self.nodes[p].children.iter().filter_map(|c| if let Child::Elem(e) = c { Some(*e) } else { None }).filter(|e| self.nodes[*e].name == *n).map(Item::Elem).collect(),
                        Test::Any => self.nodes[p].children.iter().filter_map(|c| if let Child::Elem(e) = c { Some(Item::Elem(*e)) } else { None }).collect(),
                        Test::Attr(Some(a)) => self.nodes[p].attrs.iter().filter(|(k, _)| k == a).map(|(_, v)| Item::Attr(v.clone())).collect(),
                        Test::Attr(None) => self.nodes[p].attrs.iter().map(|(_, v)| Item::Attr(v.clone())).collect(),
                        Test::Text => self.nodes[p].children.iter().filter_map(|c| if let Child::Text(t) = c { Some(Item::Text(t.clone())) } else { None }).collect(),
                        Test::Node => self.nodes[p].children.iter().map(|c| match c { Child::Elem(e) => Item::Elem(*e), Child::Text(t) => Item::Text(t.clone()) }).collect(),
                        Test::This => vec![Item::Elem(p)],
                        Test::Parent => vec![Item::Elem(self.nodes[p].parent)],
                    };
                    for pred in &step.preds {
                        let total = found.len();
                        let mut kept = Vec::new();
                        for (k, it) in found.into_iter().enumerate() {
                            let ok = match (pred, &it) {
                                (Pred::Index(n), _) => k + 1 == *n,
                                (Pred::Last, _) => k + 1 == total,
                                (Pred::Position(op, n), _) => {
                                    let pos = (k + 1) as f64;
                                    match op.as_str() {
                                        "<" => pos < *n,
                                        "<=" => pos <= *n,
                                        ">" => pos > *n,
                                        ">=" => pos >= *n,
                                        "!=" => pos != *n,
                                        _ => pos == *n,
                                    }
                                }
                                (Pred::Attr(a, v), Item::Elem(e)) => self.nodes[*e].attrs.iter().any(|(k, val)| k == a && v.as_ref().is_none_or(|v| v == val)),
                                (Pred::Child(c, v), Item::Elem(e)) => self.nodes[*e].children.iter().any(|ch| matches!(ch, Child::Elem(x) if self.nodes[*x].name == *c && v.as_ref().is_none_or(|v| *v == self.text_of(*x)))),
                                (Pred::TextIs(v), Item::Elem(e)) => self.text_of(*e) == *v,
                                (Pred::TextIs(v), Item::Text(t)) => t == v,
                                _ => false,
                            };
                            if ok {
                                kept.push(it);
                            }
                        }
                        found = kept;
                    }
                    next.extend(found);
                }
            }
            current = next;
        }
        current
    }
}

/// `EXTRACTVALUE(xml, xpath)`: the text of the nodes the path selects, joined by spaces; `None` for XML that is not well formed.
pub fn extract_value(xml: &str, path: &str) -> Result<Option<String>> {
    let Some(doc) = parse_xml(xml) else { return Ok(None) };
    let path = path.trim();
    if let Some(inner) = path.strip_prefix("count(").and_then(|p| p.strip_suffix(')')) {
        let (abs, steps) = parse_path(inner).ok_or_else(|| Error::runtime(format!("XPATH syntax error: '{path}'")))?;
        return Ok(Some(doc.eval(abs, &steps).len().to_string()));
    }
    let (abs, steps) = parse_path(path).ok_or_else(|| Error::runtime(format!("XPATH syntax error: '{path}'")))?;
    let parts: Vec<String> = doc
        .eval(abs, &steps)
        .into_iter()
        .filter_map(|it| match it {
            Item::Elem(e) => {
                let t = doc.text_of(e);
                (!t.is_empty()).then_some(t)
            }
            Item::Attr(v) => Some(v),
            Item::Text(t) => Some(t),
        })
        .collect();
    Ok(Some(parts.join(" ")))
}

/// PostgreSQL's `xpath(path, xml)`: the nodes the path selects, each as XML text (an element as it is written in the source).
pub fn xpath_nodes(xml: &str, path: &str) -> Result<Vec<String>> {
    let doc = parse_xml(xml).ok_or_else(|| Error::runtime("invalid XML document"))?;
    let (abs, steps) = parse_path(path.trim()).ok_or_else(|| Error::runtime(format!("invalid XPath expression: {path}")))?;
    Ok(doc
        .eval(abs, &steps)
        .into_iter()
        .map(|it| match it {
            Item::Elem(0) => xml.trim().to_string(),
            Item::Elem(e) => {
                let (s, t) = doc.nodes[e].span;
                xml[s..t].to_string()
            }
            Item::Attr(v) => v,
            Item::Text(t) => t,
        })
        .collect())
}

/// `UPDATEXML(xml, xpath, new)`: the XML with the one element the path selects replaced; unchanged when it selects none or
/// several.
pub fn update_xml(xml: &str, path: &str, new: &str) -> Result<Option<String>> {
    let Some(doc) = parse_xml(xml) else { return Ok(None) };
    let (abs, steps) = parse_path(path).ok_or_else(|| Error::runtime(format!("XPATH syntax error: '{path}'")))?;
    let hits: Vec<Item> = doc.eval(abs, &steps);
    match hits.as_slice() {
        [Item::Elem(e)] if *e != 0 => {
            let (s, t) = doc.nodes[*e].span;
            Ok(Some(format!("{}{}{}", &xml[..s], new, &xml[t..])))
        }
        _ => Ok(Some(xml.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extractvalue_follows_xpath() {
        let x = "<a><b>X</b><b>Y</b><c id=\"1\"><d>Z</d></c></a>";
        assert_eq!(extract_value(x, "/a/b").unwrap().unwrap(), "X Y");
        assert_eq!(extract_value(x, "/a/b[2]").unwrap().unwrap(), "Y");
        assert_eq!(extract_value(x, "//d").unwrap().unwrap(), "Z");
        assert_eq!(extract_value(x, "/a/c/@id").unwrap().unwrap(), "1");
        assert_eq!(extract_value(x, "/a/c[@id='1']/d").unwrap().unwrap(), "Z");
        assert_eq!(extract_value(x, "count(/a/b)").unwrap().unwrap(), "2");
        assert_eq!(update_xml(x, "/a/c", "<e/>").unwrap().unwrap(), "<a><b>X</b><b>Y</b><e/></a>");
        assert!(extract_value("<a><b></a>", "/a").unwrap().is_none());
    }
}
