//! PostgreSQL's composite types (`CREATE TYPE t AS (a int, b text)`) and row values (`ROW(1, 'x')`).
//!
//! There is no data type for a row: a row value is text in PostgreSQL's record form, `(1,x)`, with a field that holds
//! NULL left empty and one that needs it in double quotes. A column of a composite type is a text column, and a
//! field is read by position (`(c).a`), which the binder works out from the type of the value.

use std::borrow::Cow;

use crate::error::{Error, Result};
use crate::value::{DataType, Value};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CompFn {
    /// `__ROWCAST(record, 'type')`: the record put in the form of the composite type.
    Cast,
    /// `__NTH(record, n, 'type of the field')`: the n-th field (from 1).
    Nth,
}

pub fn lookup(name: &str) -> Option<(CompFn, usize, usize)> {
    Some(match name {
        "__ROWCAST" => (CompFn::Cast, 2, 2),
        "__NTH" => (CompFn::Nth, 3, 3),
        _ => return None,
    })
}

impl CompFn {
    pub fn handles_null(self) -> bool {
        false
    }
}

/// The fields of the composite type `name` (name and type of each), if it is one.
pub(crate) fn fields_of(name: &str) -> Option<Vec<(String, String)>> {
    crate::sql::usertypes::lookup(name).map(|t| t.fields).filter(|f| !f.is_empty())
}

pub(crate) fn is_composite(name: &str) -> bool {
    fields_of(name).is_some()
}

/// Splits record text into its fields (`None` for NULL).
pub(crate) fn split(s: &str) -> Option<Vec<Option<String>>> {
    let t = s.trim();
    let inner = t.strip_prefix('(')?.strip_suffix(')')?;
    let mut out: Vec<Option<String>> = Vec::new();
    let (mut cur, mut quoted_any, mut quoted) = (String::new(), false, false);
    let mut it = inner.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' if quoted && it.peek() == Some(&'"') => {
                it.next();
                cur.push('"');
            }
            '"' => {
                quoted = !quoted;
                quoted_any = true;
            }
            '\\' => cur.push(it.next()?),
            ',' if !quoted => {
                out.push(if cur.is_empty() && !quoted_any { None } else { Some(std::mem::take(&mut cur)) });
                quoted_any = false;
            }
            c => cur.push(c),
        }
    }
    if quoted {
        return None;
    }
    out.push(if cur.is_empty() && !quoted_any { None } else { Some(cur) });
    Some(out)
}

/// Joins fields into record text.
pub(crate) fn join(fields: &[Option<String>]) -> String {
    let one = |f: &Option<String>| match f {
        None => String::new(),
        Some(s) if s.is_empty() => "\"\"".to_string(),
        Some(s) if s.chars().any(|c| matches!(c, '"' | '\\' | '(' | ')' | ',') || c.is_whitespace()) => {
            format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\"\""))
        }
        Some(s) => s.clone(),
    };
    format!("({})", fields.iter().map(one).collect::<Vec<_>>().join(","))
}

fn field_type(t: &str) -> Option<DataType> {
    DataType::from_sql_name(t).map(|x| x.0)
}

/// The record text put in the form of the composite type `type_name`: the right number of fields, each in the form of its type.
pub(crate) fn cast_text(type_name: &str, text: &str) -> Result<String> {
    let fields = fields_of(type_name).ok_or_else(|| Error::catalog(format!("type \"{type_name}\" does not exist")))?;
    let items = split(text).ok_or_else(|| Error::type_err(format!("malformed record literal: \"{text}\"")))?;
    if items.len() > fields.len() {
        return Err(Error::runtime("malformed record literal: Too many columns.".to_string()));
    }
    if items.len() < fields.len() {
        return Err(Error::runtime("malformed record literal: Too few columns.".to_string()));
    }
    let mut out = Vec::with_capacity(items.len());
    for (item, (_, ty)) in items.into_iter().zip(&fields) {
        out.push(match item {
            None => None,
            Some(s) => Some(match field_type(ty) {
                Some(dt) if !matches!(dt, DataType::Text) => Value::text(&s).coerce_to(dt)?.to_string(),
                _ => match super::ranges::cast_function_for(ty) {
                    Some(_) => super::ranges::normalize(ty, &Value::text(&s))?.to_string(),
                    None if is_composite(ty) => cast_text(ty, &s)?,
                    None => s,
                },
            }),
        });
    }
    Ok(join(&out))
}

pub fn call(f: CompFn, vals: &[Cow<'_, Value>]) -> Result<Value> {
    Ok(match f {
        CompFn::Cast => {
            if vals[0].is_null() {
                return Ok(Value::Null);
            }
            Value::text(&cast_text(&vals[1].to_string(), &vals[0].to_string())?)
        }
        CompFn::Nth => {
            if vals[0].is_null() {
                return Ok(Value::Null);
            }
            let n = vals[1].to_string().parse::<usize>().unwrap_or(0);
            let items = split(&vals[0].to_string()).ok_or_else(|| Error::type_err("malformed record"))?;
            match items.get(n.wrapping_sub(1)) {
                Some(Some(s)) => match field_type(&vals[2].to_string()) {
                    Some(dt) if !matches!(dt, DataType::Text) => Value::text(s).coerce_to(dt)?,
                    _ => Value::text(s),
                },
                _ => Value::Null,
            }
        }
    })
}
