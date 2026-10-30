//! Array functions: the constructor `ARRAY[...]`, subscripts and slices, `array_length` and its kin, `array_append`,
//! `array_cat`, `array_remove`, `array_position`, `array_to_string`, `string_to_array`, containment (`@>`, `<@`, `&&`)
//! and the comparisons against a whole array (`x = ANY (arr)`).
//!
//! An array is a `Value::Array` of values; a nested array is a further dimension. Subscripts count from 1, as in
//! PostgreSQL, and an index outside the array gives NULL.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::json;
use crate::value::{ArrayOf, DataType, Value};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ArrFn {
    /// `ARRAY[a, b, ...]`
    Ctor,
    /// `a[i]`
    Subscript,
    /// `a[lo:hi]`, either bound NULL for open
    Slice,
    Length,
    Cardinality,
    Upper,
    Lower,
    Ndims,
    Dims,
    Append,
    Prepend,
    Cat,
    Remove,
    Replace,
    Position,
    Positions,
    ToString,
    FromString,
    Fill,
    Trim,
    Contains,
    ContainedBy,
    Overlap,
    /// `x op ANY (arr)` (false) and `x op ALL (arr)` (true), the operator as a third argument
    Quant(bool),
    /// `CAST(x AS type[])`
    Cast(ArrayOf),
    // MySQL has no array type; its arrays are JSON arrays, and these are the functions that work on them
    JsonArrayAppend,
    JsonArrayInsert,
    JsonInsert,
    JsonReplace,
    JsonOverlaps,
    /// `x MEMBER OF (json_array)`
    MemberOf,
}

pub fn lookup(name: &str) -> Option<(ArrFn, usize, usize)> {
    use ArrFn::*;
    Some(match name {
        "__ARRAY" => (Ctor, 0, usize::MAX),
        "__SUBSCRIPT" => (Subscript, 2, 2),
        "__SLICE" => (Slice, 3, 3),
        "ARRAY_LENGTH" => (Length, 2, 2),
        "CARDINALITY" => (Cardinality, 1, 1),
        "ARRAY_UPPER" => (Upper, 2, 2),
        "ARRAY_LOWER" => (Lower, 2, 2),
        "ARRAY_NDIMS" => (Ndims, 1, 1),
        "ARRAY_DIMS" => (Dims, 1, 1),
        "ARRAY_APPEND" => (Append, 2, 2),
        "ARRAY_PREPEND" => (Prepend, 2, 2),
        "ARRAY_CAT" => (Cat, 2, 2),
        "ARRAY_REMOVE" => (Remove, 2, 2),
        "ARRAY_REPLACE" => (Replace, 3, 3),
        "ARRAY_POSITION" => (Position, 2, 3),
        "ARRAY_POSITIONS" => (Positions, 2, 2),
        "ARRAY_TO_STRING" => (ToString, 2, 3),
        "STRING_TO_ARRAY" => (FromString, 2, 3),
        "ARRAY_FILL" => (Fill, 2, 3),
        "TRIM_ARRAY" => (Trim, 2, 2),
        "__ARRAY_CONTAINS" => (Contains, 2, 2),
        "__ARRAY_CONTAINED" => (ContainedBy, 2, 2),
        "__ARRAY_OVERLAP" => (Overlap, 2, 2),
        "__ANY" => (Quant(false), 3, 3),
        "__ALL" => (Quant(true), 3, 3),
        "JSON_ARRAY_APPEND" => (JsonArrayAppend, 3, usize::MAX),
        "JSON_ARRAY_INSERT" => (JsonArrayInsert, 3, usize::MAX),
        "JSON_INSERT" => (JsonInsert, 3, usize::MAX),
        "JSON_REPLACE" => (JsonReplace, 3, usize::MAX),
        "JSON_OVERLAPS" => (JsonOverlaps, 2, 2),
        "__MEMBER_OF" => (MemberOf, 2, 2),
        "CARRAY" => (Cast(ArrayOf::Any), 1, 1),
        "CARRAY_INTEGER" => (Cast(ArrayOf::Integer), 1, 1),
        "CARRAY_BIGNUM" => (Cast(ArrayOf::Bignum), 1, 1),
        "CARRAY_FLOAT" => (Cast(ArrayOf::Float), 1, 1),
        "CARRAY_DOUBLE" => (Cast(ArrayOf::Double), 1, 1),
        "CARRAY_DECIMAL" => (Cast(ArrayOf::Decimal), 1, 1),
        "CARRAY_CURRENCY" => (Cast(ArrayOf::Currency), 1, 1),
        "CARRAY_RATIONAL" => (Cast(ArrayOf::Rational), 1, 1),
        "CARRAY_TEXT" => (Cast(ArrayOf::Text), 1, 1),
        "CARRAY_YESNO" => (Cast(ArrayOf::YesNo), 1, 1),
        "CARRAY_DATETIME" => (Cast(ArrayOf::DateTime), 1, 1),
        "CARRAY_TIMESTAMPTZ" => (Cast(ArrayOf::TimestampTz), 1, 1),
        "CARRAY_INTERVAL" => (Cast(ArrayOf::Interval), 1, 1),
        "CARRAY_JSON" => (Cast(ArrayOf::Json), 1, 1),
        _ => return None,
    })
}

/// The name of the conversion function `CAST(x AS type[])` stands for.
pub fn cast_name(of: ArrayOf) -> &'static str {
    match of {
        ArrayOf::Any => "CARRAY",
        ArrayOf::Integer => "CARRAY_INTEGER",
        ArrayOf::Bignum => "CARRAY_BIGNUM",
        ArrayOf::Float => "CARRAY_FLOAT",
        ArrayOf::Double => "CARRAY_DOUBLE",
        ArrayOf::Decimal => "CARRAY_DECIMAL",
        ArrayOf::Currency => "CARRAY_CURRENCY",
        ArrayOf::Rational => "CARRAY_RATIONAL",
        ArrayOf::Text => "CARRAY_TEXT",
        ArrayOf::YesNo => "CARRAY_YESNO",
        ArrayOf::DateTime => "CARRAY_DATETIME",
        ArrayOf::TimestampTz => "CARRAY_TIMESTAMPTZ",
        ArrayOf::Interval => "CARRAY_INTERVAL",
        ArrayOf::Json => "CARRAY_JSON",
    }
}

impl ArrFn {
    /// Functions that get their NULL arguments instead of answering NULL themselves.
    pub fn handles_null(self) -> bool {
        use ArrFn::*;
        matches!(self, Ctor | Append | Prepend | Cat | Remove | Replace | Position | Positions | Fill | ToString | Quant(_) | JsonArrayAppend | JsonArrayInsert | JsonInsert | JsonReplace)
    }
}

/// The elements of an array argument; text in the `{1,2}` form and a JSON array are read as arrays.
fn items(v: &Value) -> Result<Cow<'_, [Value]>> {
    match v {
        Value::Array(a) => Ok(Cow::Borrowed(a.as_slice())),
        Value::Text(s) => match Value::parse_array(s, ArrayOf::Any) {
            Some(Value::Array(a)) => Ok(Cow::Owned(a.as_ref().clone())),
            _ => Err(Error::type_err(format!("malformed array literal: \"{s}\""))),
        },
        Value::Json(_) => match v.coerce_to(DataType::Array(ArrayOf::Any))? {
            Value::Array(a) => Ok(Cow::Owned(a.as_ref().clone())),
            _ => unreachable!("an array was asked for"),
        },
        other => Err(Error::type_err(format!("'{other}' is not an array"))),
    }
}

fn array(v: Vec<Value>) -> Value {
    Value::Array(Arc::new(v))
}

fn int(v: &Value) -> Result<i64> {
    match v.coerce_to(DataType::Integer)? {
        Value::Int(i) => Ok(i),
        _ => Err(Error::type_err("a whole number was expected")),
    }
}

/// Equality as the array functions see it: two NULLs are the same element.
fn same(a: &Value, b: &Value) -> bool {
    match (a.is_null(), b.is_null()) {
        (true, true) => true,
        (false, false) => a.sql_cmp(b) == Some(Ordering::Equal),
        _ => false,
    }
}

fn flatten<'a>(a: &'a [Value], out: &mut Vec<&'a Value>) {
    for x in a {
        match x {
            Value::Array(inner) => flatten(inner, out),
            other => out.push(other),
        }
    }
}

fn depth(a: &[Value]) -> usize {
    1 + match a.first() {
        Some(Value::Array(inner)) => depth(inner),
        _ => 0,
    }
}

/// The length along dimension `dim` (1-based), if the array has it.
fn dimension(a: &[Value], dim: i64) -> Option<usize> {
    if dim < 1 {
        return None;
    }
    let mut cur = a;
    for _ in 1..dim {
        match cur.first() {
            Some(Value::Array(inner)) => cur = inner,
            _ => return None,
        }
    }
    (!cur.is_empty()).then_some(cur.len())
}

fn fill(value: &Value, dims: &[i64]) -> Value {
    match dims.split_first() {
        None => value.clone(),
        Some((n, rest)) => array((0..(*n).max(0)).map(|_| fill(value, rest)).collect()),
    }
}

/// The comparison `x op y` for the operator text of `ANY` / `ALL`; `None` when it is unknown (a NULL).
fn compare(op: &str, x: &Value, y: &Value) -> Result<Option<bool>> {
    let Some(o) = x.sql_cmp(y) else { return Ok(None) };
    Ok(Some(match op {
        "=" => o == Ordering::Equal,
        "<>" | "!=" => o != Ordering::Equal,
        "<" => o == Ordering::Less,
        "<=" => o != Ordering::Greater,
        ">" => o == Ordering::Greater,
        ">=" => o != Ordering::Less,
        other => return Err(Error::runtime(format!("the operator {other} cannot be used with ANY or ALL"))),
    }))
}

/// `a || b`: an array joined to an array, or to one more element at either end.
pub fn concat(a: &Value, b: &Value) -> Result<Value> {
    Ok(match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            let mut v = x.as_ref().clone();
            v.extend(y.iter().cloned());
            array(v)
        }
        (Value::Array(x), other) => {
            let mut v = x.as_ref().clone();
            v.push(other.clone());
            array(v)
        }
        (other, Value::Array(y)) => {
            let mut v = vec![other.clone()];
            v.extend(y.iter().cloned());
            array(v)
        }
        _ => return Err(Error::type_err("an array was expected")),
    })
}

/// The value of the array function `f` on its arguments.
pub fn call(f: ArrFn, vals: &[Cow<'_, Value>]) -> Result<Value> {
    use ArrFn::*;
    let v = |i: usize| -> &Value { &vals[i] };
    Ok(match f {
        Ctor => array(vals.iter().map(|x| x.as_ref().clone()).collect()),
        Subscript => {
            let a = items(v(0))?;
            let i = int(v(1))?;
            usize::try_from(i - 1).ok().and_then(|k| a.get(k)).cloned().unwrap_or(Value::Null)
        }
        Slice => {
            let a = items(v(0))?;
            let lo = if v(1).is_null() { 1 } else { int(v(1))? }.max(1);
            let hi = if v(2).is_null() { a.len() as i64 } else { int(v(2))? }.min(a.len() as i64);
            if lo > hi {
                array(Vec::new())
            } else {
                array(a[(lo - 1) as usize..hi as usize].to_vec())
            }
        }
        Length => {
            let a = items(v(0))?;
            dimension(&a, int(v(1))?).map_or(Value::Null, |n| Value::Int(n as i64))
        }
        Upper => {
            let a = items(v(0))?;
            dimension(&a, int(v(1))?).map_or(Value::Null, |n| Value::Int(n as i64))
        }
        Lower => {
            let a = items(v(0))?;
            dimension(&a, int(v(1))?).map_or(Value::Null, |_| Value::Int(1))
        }
        Cardinality => {
            let a = items(v(0))?;
            let mut flat = Vec::new();
            flatten(&a, &mut flat);
            Value::Int(flat.len() as i64)
        }
        Ndims => {
            let a = items(v(0))?;
            if a.is_empty() { Value::Null } else { Value::Int(depth(&a) as i64) }
        }
        Dims => {
            let a = items(v(0))?;
            if a.is_empty() {
                Value::Null
            } else {
                let mut out = String::new();
                let mut cur: &[Value] = &a;
                loop {
                    out.push_str(&format!("[1:{}]", cur.len()));
                    match cur.first() {
                        Some(Value::Array(inner)) => cur = inner,
                        _ => break,
                    }
                }
                Value::text(&out)
            }
        }
        Append => {
            let mut a = if v(0).is_null() { Vec::new() } else { items(v(0))?.into_owned() };
            a.push(v(1).clone());
            array(a)
        }
        Prepend => {
            let mut a = vec![v(0).clone()];
            if !v(1).is_null() {
                a.extend(items(v(1))?.iter().cloned());
            }
            array(a)
        }
        Cat => match (v(0).is_null(), v(1).is_null()) {
            (true, true) => Value::Null,
            (true, false) => v(1).clone(),
            (false, true) => v(0).clone(),
            _ => {
                let mut a = items(v(0))?.into_owned();
                a.extend(items(v(1))?.iter().cloned());
                array(a)
            }
        },
        Remove => {
            if v(0).is_null() {
                return Ok(Value::Null);
            }
            array(items(v(0))?.iter().filter(|x| !same(x, v(1))).cloned().collect())
        }
        Replace => {
            if v(0).is_null() {
                return Ok(Value::Null);
            }
            array(items(v(0))?.iter().map(|x| if same(x, v(1)) { v(2).clone() } else { x.clone() }).collect())
        }
        Position | Positions => {
            if v(0).is_null() {
                return Ok(Value::Null);
            }
            let a = items(v(0))?;
            let start = if f == Position && vals.len() == 3 { int(v(2))?.max(1) as usize } else { 1 };
            let hits = a.iter().enumerate().filter(|(i, x)| i + 1 >= start && same(x, v(1))).map(|(i, _)| Value::Int(i as i64 + 1));
            if f == Position { hits.into_iter().next().unwrap_or(Value::Null) } else { array(hits.collect()) }
        }
        ToString => {
            if v(0).is_null() || v(1).is_null() {
                return Ok(Value::Null);
            }
            let a = items(v(0))?;
            let mut flat = Vec::new();
            flatten(&a, &mut flat);
            let sep = v(1).to_string();
            let null = vals.get(2).filter(|n| !n.is_null()).map(|n| n.to_string());
            let parts: Vec<String> = flat
                .into_iter()
                .filter_map(|x| if x.is_null() { null.clone() } else { Some(x.to_string()) })
                .collect();
            Value::text(&parts.join(&sep))
        }
        FromString => {
            if v(0).is_null() {
                return Ok(Value::Null);
            }
            let s = v(0).to_string();
            let null = vals.get(2).filter(|n| !n.is_null()).map(|n| n.to_string());
            let parts: Vec<String> = if s.is_empty() {
                Vec::new()
            } else if v(1).is_null() {
                s.chars().map(String::from).collect()
            } else {
                let sep = v(1).to_string();
                if sep.is_empty() { vec![s] } else { s.split(sep.as_str()).map(str::to_string).collect() }
            };
            array(parts.into_iter().map(|p| if null.as_deref() == Some(p.as_str()) { Value::Null } else { Value::text(&p) }).collect())
        }
        Fill => {
            if v(1).is_null() {
                return Err(Error::runtime("dimension array or low bound array cannot be null"));
            }
            let dims = items(v(1))?.iter().map(int).collect::<Result<Vec<_>>>()?;
            fill(v(0), &dims)
        }
        Trim => {
            let a = items(v(0))?;
            let n = int(v(1))?;
            if n < 0 || n as usize > a.len() {
                return Err(Error::runtime(format!("number of elements to trim must be between 0 and {}", a.len())));
            }
            array(a[..a.len() - n as usize].to_vec())
        }
        Contains | ContainedBy | Overlap => {
            // JSON documents use the same operators
            if let (Value::Json(_), Value::Json(_)) = (v(0), v(1)) {
                let (a, b) = (crate::json::document(v(0))?, crate::json::document(v(1))?);
                return Ok(Value::Bool(match f {
                    Contains => crate::json::contains(&a, &b),
                    ContainedBy => crate::json::contains(&b, &a),
                    _ => return Err(Error::type_err("&& is not defined for JSON")),
                }));
            }
            let (a, b) = (items(v(0))?, items(v(1))?);
            let has = |hay: &[Value], x: &Value| !x.is_null() && hay.iter().any(|y| same(x, y));
            Value::Bool(match f {
                Contains => b.iter().all(|x| has(&a, x)),
                ContainedBy => a.iter().all(|x| has(&b, x)),
                _ => a.iter().any(|x| has(&b, x)),
            })
        }
        Quant(all) => {
            if v(1).is_null() {
                return Ok(Value::Null);
            }
            let a = items(v(1))?;
            let op = v(2).to_string();
            let mut unknown = false;
            for y in a.iter() {
                match compare(&op, v(0), y)? {
                    Some(true) if !all => return Ok(Value::Bool(true)),
                    Some(false) if all => return Ok(Value::Bool(false)),
                    Some(_) => {}
                    None => unknown = true,
                }
            }
            if unknown { Value::Null } else { Value::Bool(all) }
        }
        Cast(of) => v(0).coerce_to(DataType::Array(of))?,
        JsonArrayAppend | JsonArrayInsert | JsonInsert | JsonReplace => {
            use crate::json::{self, Step};
            use serde_json::Value as J;
            if v(0).is_null() {
                return Ok(Value::Null);
            }
            if vals.len() % 2 == 0 {
                return Err(Error::runtime("the path and value arguments must come in pairs"));
            }
            let mut doc = json::document(v(0))?;
            for pair in vals[1..].chunks(2) {
                if pair[0].is_null() {
                    return Ok(Value::Null);
                }
                let steps = json::path_steps(&pair[0].to_string())?;
                let new = json::from_value(&pair[1]);
                match f {
                    JsonArrayAppend => {
                        let target = json::get(&doc, &steps).cloned();
                        let joined = match target {
                            Some(J::Array(mut a)) => {
                                a.push(new);
                                J::Array(a)
                            }
                            Some(other) => J::Array(vec![other, new]),
                            None => continue,
                        };
                        json::set(&mut doc, &steps, joined)?;
                    }
                    JsonArrayInsert => {
                        let Some((Step::Index(at), parent)) = steps.split_last() else {
                            return Err(Error::runtime("the path must end with an array position"));
                        };
                        if let Some(J::Array(a)) = json::get(&doc, parent).cloned() {
                            let mut a = a;
                            a.insert((*at).min(a.len()), new);
                            json::set(&mut doc, parent, J::Array(a))?;
                        }
                    }
                    JsonInsert => {
                        if json::get(&doc, &steps).is_none() {
                            json::set(&mut doc, &steps, new)?;
                        }
                    }
                    _ => {
                        if json::get(&doc, &steps).is_some() {
                            json::set(&mut doc, &steps, new)?;
                        }
                    }
                }
            }
            json::json_value(&doc)
        }
        JsonOverlaps => {
            use serde_json::Value as J;
            let (a, b) = (json::document(v(0))?, json::document(v(1))?);
            Value::Bool(match (&a, &b) {
                (J::Array(x), J::Array(y)) => x.iter().any(|p| y.contains(p)),
                (J::Array(x), other) | (other, J::Array(x)) => x.contains(other),
                (J::Object(x), J::Object(y)) => x.iter().any(|(k, p)| y.get(k) == Some(p)),
                _ => a == b,
            })
        }
        MemberOf => {
            let doc = json::document(v(1))?;
            let x = json::from_value(v(0));
            let eq = |a: &serde_json::Value| match (a.as_f64(), x.as_f64()) {
                (Some(p), Some(q)) => p == q,
                _ => *a == x,
            };
            Value::Bool(match &doc {
                serde_json::Value::Array(items) => items.iter().any(eq),
                other => eq(other),
            })
        }
    })
}
