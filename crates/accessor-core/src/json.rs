//! JSON values: parsing, paths, and the operations behind the `JSON_*` functions.
//!
//! A `JSON` value is kept as canonical text: compact, with object keys in sorted order,
//! so two documents are equal exactly when they mean the same thing. Numbers keep the
//! digits they were written with.

use std::sync::Arc;

use serde_json::{Map, Number, Value as J};

use crate::decimal::Decimal;
use crate::error::{Error, Result};
use crate::value::Value;

/// Reads JSON text.
pub(crate) fn parse(text: &str) -> Result<J> {
    serde_json::from_str(text).map_err(|e| Error::type_err(format!("not valid JSON: {e}")))
}

pub(crate) fn to_text(j: &J) -> String {
    serde_json::to_string(j).expect("a JSON value always serialises")
}

/// The canonical text of `text`, or `None` if it is not JSON.
pub(crate) fn canonical(text: &str) -> Option<String> {
    serde_json::from_str::<J>(text).ok().map(|j| to_text(&j))
}

/// A JSON document as a SQL value.
pub(crate) fn json_value(j: &J) -> Value {
    Value::Json(Arc::from(to_text(j).as_str()))
}

/// The document held by a value: a JSON value as it is, text as JSON text.
pub(crate) fn document(v: &Value) -> Result<J> {
    match v {
        Value::Json(s) => parse(s),
        Value::Text(s) => parse(s),
        other => Err(Error::type_err(format!("'{other}' is not a JSON document"))),
    }
}

/// A SQL value as JSON (`Json` values are embedded as they are).
pub(crate) fn from_value(v: &Value) -> J {
    fn number(text: &str) -> J {
        serde_json::from_str::<Number>(text).map_or(J::Null, J::Number)
    }
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(i) => J::Number(Number::from(*i)),
        Value::Big(b) => number(&b.to_string()),
        Value::Dec(d) | Value::Currency(d) => number(&d.to_string()),
        Value::Float(_) | Value::Double(_) | Value::Rational(_) => {
            v.to_f64().and_then(Number::from_f64).map_or(J::Null, J::Number)
        }
        Value::Json(s) => parse(s).unwrap_or(J::Null),
        other => J::String(other.to_string()),
    }
}

/// A scalar of a document as a SQL value; objects and arrays stay JSON.
pub(crate) fn to_value(j: &J) -> Value {
    match j {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::String(s) => Value::text(s),
        J::Number(n) => {
            let t = n.to_string();
            if let Ok(i) = t.parse::<i64>() {
                Value::Int(i)
            } else if let Some(d) = Decimal::parse(&t) {
                Value::Dec(d)
            } else {
                t.parse::<f64>().ok().and_then(Value::from_f64).unwrap_or(Value::Null)
            }
        }
        other => json_value(other),
    }
}

pub(crate) fn type_name(j: &J) -> &'static str {
    match j {
        J::Null => "null",
        J::Bool(_) => "boolean",
        J::Number(_) => "number",
        J::String(_) => "string",
        J::Array(_) => "array",
        J::Object(_) => "object",
    }
}

/// One step of a path: an object key or an array position.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    Key(String),
    Index(usize),
}

/// Reads a path such as `$`, `$.a.b`, `$.items[0].name` or `$["odd key"]`.
pub(crate) fn path_steps(path: &str) -> Result<Vec<Step>> {
    let bad = || Error::runtime(format!("'{path}' is not a JSON path (try $.key[0].key)"));
    let p = path.trim();
    let mut rest = p.strip_prefix('$').ok_or_else(bad)?;
    let mut steps = Vec::new();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            let end = r.find(['.', '[']).unwrap_or(r.len());
            if end == 0 {
                return Err(bad());
            }
            steps.push(Step::Key(r[..end].to_string()));
            rest = &r[end..];
        } else if let Some(r) = rest.strip_prefix('[') {
            let close = r.find(']').ok_or_else(bad)?;
            let inner = r[..close].trim();
            if let Some(q) = inner.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                steps.push(Step::Key(q.to_string()));
            } else if let Some(q) = inner.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
                steps.push(Step::Key(q.to_string()));
            } else {
                steps.push(Step::Index(inner.parse().map_err(|_| bad())?));
            }
            rest = &r[close + 1..];
        } else {
            return Err(bad());
        }
    }
    Ok(steps)
}

pub(crate) fn get<'a>(j: &'a J, steps: &[Step]) -> Option<&'a J> {
    let mut cur = j;
    for s in steps {
        cur = match (s, cur) {
            (Step::Key(k), J::Object(m)) => m.get(k)?,
            (Step::Index(i), J::Array(a)) => a.get(*i)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// Sets the value at `steps`, adding it where it is missing (an array position one past the
/// end appends). Returns an error where the path runs into something that is not a container.
pub(crate) fn set(j: &mut J, steps: &[Step], new: J) -> Result<()> {
    let Some((first, rest)) = steps.split_first() else {
        *j = new;
        return Ok(());
    };
    match (first, j) {
        (Step::Key(k), J::Object(m)) => {
            if rest.is_empty() {
                m.insert(k.clone(), new);
                Ok(())
            } else {
                let child = m.get_mut(k).ok_or_else(|| Error::runtime(format!("the path has no '{k}' to go through")))?;
                set(child, rest, new)
            }
        }
        (Step::Index(i), J::Array(a)) => {
            if *i == a.len() && rest.is_empty() {
                a.push(new);
                Ok(())
            } else {
                let child = a.get_mut(*i).ok_or_else(|| Error::runtime(format!("the array has no position {i}")))?;
                set(child, rest, new)
            }
        }
        _ => Err(Error::runtime("the path goes through a value that is not an object or array")),
    }
}

/// Removes the value at `steps` (nothing happens if it is not there).
pub(crate) fn remove(j: &mut J, steps: &[Step]) {
    let Some((last, init)) = steps.split_last() else { return };
    let mut cur = j;
    for s in init {
        cur = match (s, cur) {
            (Step::Key(k), J::Object(m)) => match m.get_mut(k) {
                Some(c) => c,
                None => return,
            },
            (Step::Index(i), J::Array(a)) => match a.get_mut(*i) {
                Some(c) => c,
                None => return,
            },
            _ => return,
        };
    }
    match (last, cur) {
        (Step::Key(k), J::Object(m)) => {
            m.remove(k);
        }
        (Step::Index(i), J::Array(a)) if *i < a.len() => {
            a.remove(*i);
        }
        _ => {}
    }
}

/// RFC 7396 merge patch: objects merge key by key, `null` removes a key, anything else replaces.
pub(crate) fn merge_patch(target: J, patch: J) -> J {
    match patch {
        J::Object(p) => {
            let mut base = match target {
                J::Object(m) => m,
                _ => Map::new(),
            };
            for (k, v) in p {
                if v.is_null() {
                    base.remove(&k);
                } else {
                    let old = base.remove(&k).unwrap_or(J::Null);
                    base.insert(k, merge_patch(old, v));
                }
            }
            J::Object(base)
        }
        other => other,
    }
}

/// Does `a` contain `b`? Objects contain objects with a subset of their keys (recursively),
/// arrays contain arrays whose elements each appear, and scalars must be equal.
pub(crate) fn contains(a: &J, b: &J) -> bool {
    match (a, b) {
        (J::Object(x), J::Object(y)) => y.iter().all(|(k, v)| x.get(k).is_some_and(|xv| contains(xv, v))),
        (J::Array(x), J::Array(y)) => y.iter().all(|v| x.iter().any(|xv| contains(xv, v))),
        (J::Array(x), scalar) if !matches!(scalar, J::Object(_) | J::Array(_)) => x.iter().any(|xv| xv == scalar),
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn j(s: &str) -> J {
        parse(s).unwrap()
    }

    #[test]
    fn canonical_form_sorts_keys_and_keeps_numbers() {
        assert_eq!(canonical(r#"{ "b": 1.50, "a": [1, 2] }"#).unwrap(), r#"{"a":[1,2],"b":1.50}"#);
        assert!(canonical("{oops").is_none());
    }

    #[test]
    fn paths() {
        let doc = j(r#"{"a":{"b":[10,20,{"c":"x"}]},"odd key":1}"#);
        let at = |p: &str| get(&doc, &path_steps(p).unwrap()).map(to_text);
        assert_eq!(at("$.a.b[1]").as_deref(), Some("20"));
        assert_eq!(at("$.a.b[2].c").as_deref(), Some("\"x\""));
        assert_eq!(at("$[\"odd key\"]").as_deref(), Some("1"));
        assert_eq!(at("$.a.nope"), None);
        assert!(path_steps("a.b").is_err());
        assert!(path_steps("$.a[x]").is_err());
    }

    #[test]
    fn set_and_remove() {
        let mut d = j(r#"{"a":[1],"b":2}"#);
        set(&mut d, &path_steps("$.a[1]").unwrap(), j("5")).unwrap(); // appends
        set(&mut d, &path_steps("$.c").unwrap(), j("true")).unwrap();
        remove(&mut d, &path_steps("$.b").unwrap());
        assert_eq!(to_text(&d), r#"{"a":[1,5],"c":true}"#);
        assert!(set(&mut d, &path_steps("$.a[7]").unwrap(), j("1")).is_err());
    }

    #[test]
    fn merge_and_contains() {
        let m = merge_patch(j(r#"{"a":1,"b":{"x":1,"y":2}}"#), j(r#"{"b":{"y":null,"z":3},"c":4}"#));
        assert_eq!(to_text(&m), r#"{"a":1,"b":{"x":1,"z":3},"c":4}"#);
        assert!(contains(&j(r#"{"a":[1,2,3],"b":"x"}"#), &j(r#"{"a":[3,1]}"#)));
        assert!(!contains(&j(r#"{"a":1}"#), &j(r#"{"a":2}"#)));
    }
}
