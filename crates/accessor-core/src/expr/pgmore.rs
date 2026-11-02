//! More of PostgreSQL's built-in functions: string helpers (`strpos`, `to_hex`, `btrim`, `chr`, `starts_with`,
//! `quote_nullable`), regular-expression arrays (`regexp_match`, `regexp_split_to_array`), the JSON builders and
//! readers (`json_build_object`, `json_extract_path`, `jsonb_exists`, `jsonb_pretty`, ...), array reordering
//! (`array_reverse`, `array_sort`), size formatting and a few numeric functions.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::sync::Arc;

use num_bigint::BigInt;
use serde_json::Value as J;

use crate::error::{Error, Result};
use crate::json;
use crate::value::{DataType, Value};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MoreFn {
    Strpos,
    ToHex,
    ToBin,
    ToOct,
    Btrim,
    Chr,
    StartsWith,
    QuoteNullable,
    QuoteLiteral,
    Normalize,
    Casefold,
    Unistr,
    NumNulls,
    NumNonnulls,
    RegexpMatch,
    RegexpSplitToArray,
    Factorial,
    Lgamma,
    SizePretty,
    SizeBytes,
    JsonBuildObject,
    JsonBuildArray,
    JsonExtractPath,
    JsonExtractPathText,
    JsonbExists,
    JsonbExistsAny,
    JsonbExistsAll,
    JsonbPretty,
    JsonbContains,
    JsonbContained,
    JsonbDelete,
    JsonbDeletePath,
    JsonScalar,
    JsonSerialize,
    JsonExists,
    ArrayToJson,
    JsonbObject,
    ArrayReverse,
    ArraySort,
    ArrayShuffle,
    ArraySample,
    UuidVersion,
    Null,
}

pub fn lookup(name: &str) -> Option<(MoreFn, usize, usize)> {
    use MoreFn::*;
    Some(match name {
        "STRPOS" => (Strpos, 2, 2),
        "TO_HEX" => (ToHex, 1, 1),
        "TO_BIN" => (ToBin, 1, 1),
        "TO_OCT" => (ToOct, 1, 1),
        "BTRIM" => (Btrim, 1, 2),
        "CHR" => (Chr, 1, 1),
        "STARTS_WITH" => (StartsWith, 2, 2),
        "QUOTE_NULLABLE" => (QuoteNullable, 1, 1),
        "QUOTE_LITERAL" => (QuoteLiteral, 1, 1),
        "NORMALIZE" => (Normalize, 1, 2),
        "CASEFOLD" => (Casefold, 1, 1),
        "UNISTR" => (Unistr, 1, 1),
        "NUM_NULLS" => (NumNulls, 1, usize::MAX),
        "NUM_NONNULLS" => (NumNonnulls, 1, usize::MAX),
        "REGEXP_MATCH" => (RegexpMatch, 2, 3),
        "REGEXP_SPLIT_TO_ARRAY" => (RegexpSplitToArray, 2, 3),
        "FACTORIAL" => (Factorial, 1, 1),
        "LGAMMA" => (Lgamma, 1, 1),
        "PG_SIZE_PRETTY" => (SizePretty, 1, 1),
        "PG_SIZE_BYTES" => (SizeBytes, 1, 1),
        "JSON_BUILD_OBJECT" | "JSONB_BUILD_OBJECT" => (JsonBuildObject, 0, usize::MAX),
        "JSON_BUILD_ARRAY" | "JSONB_BUILD_ARRAY" => (JsonBuildArray, 0, usize::MAX),
        "JSON_EXTRACT_PATH" | "JSONB_EXTRACT_PATH" => (JsonExtractPath, 1, usize::MAX),
        "JSON_EXTRACT_PATH_TEXT" | "JSONB_EXTRACT_PATH_TEXT" => (JsonExtractPathText, 1, usize::MAX),
        "JSONB_EXISTS" => (JsonbExists, 2, 2),
        "JSONB_EXISTS_ANY" => (JsonbExistsAny, 2, 2),
        "JSONB_EXISTS_ALL" => (JsonbExistsAll, 2, 2),
        "JSONB_PRETTY" => (JsonbPretty, 1, 1),
        "JSONB_CONTAINS" => (JsonbContains, 2, 2),
        "JSONB_CONTAINED" => (JsonbContained, 2, 2),
        "JSONB_DELETE" => (JsonbDelete, 2, 2),
        "JSONB_DELETE_PATH" => (JsonbDeletePath, 2, 2),
        "JSON_SCALAR" => (JsonScalar, 1, 1),
        "JSON_SERIALIZE" => (JsonSerialize, 1, 1),
        "JSON_EXISTS" => (JsonExists, 2, 2),
        "ARRAY_TO_JSON" => (ArrayToJson, 1, 2),
        "JSONB_OBJECT" | "JSON_OBJECT_AGG_PAIRS" => (JsonbObject, 1, 2),
        "ARRAY_REVERSE" => (ArrayReverse, 1, 1),
        "ARRAY_SORT" => (ArraySort, 1, 3),
        "ARRAY_SHUFFLE" => (ArrayShuffle, 1, 1),
        "ARRAY_SAMPLE" => (ArraySample, 2, 2),
        "UUID_EXTRACT_VERSION" => (UuidVersion, 1, 1),
        "PG_COLUMN_COMPRESSION" => (Null, 1, 1),
        _ => return None,
    })
}

impl MoreFn {
    pub fn handles_null(self) -> bool {
        matches!(self, MoreFn::NumNulls | MoreFn::NumNonnulls | MoreFn::QuoteNullable | MoreFn::JsonBuildObject | MoreFn::JsonBuildArray)
    }
}

fn text(v: &Value) -> Cow<'_, str> {
    match v {
        Value::Text(s) => Cow::Borrowed(&**s),
        Value::Json(s) => Cow::Borrowed(&**s),
        other => Cow::Owned(other.to_string()),
    }
}

fn int(v: &Value) -> Result<i64> {
    match v.coerce_to(DataType::Integer)? {
        Value::Int(i) => Ok(i),
        _ => Err(Error::type_err("a whole number was expected")),
    }
}

fn items(v: &Value) -> Result<Vec<Value>> {
    match v {
        Value::Array(a) => Ok(a.as_ref().clone()),
        Value::Text(s) => match Value::parse_array(s, crate::value::ArrayOf::Any) {
            Some(Value::Array(a)) => Ok(a.as_ref().clone()),
            _ => Err(Error::type_err(format!("malformed array literal: \"{s}\""))),
        },
        other => Err(Error::type_err(format!("'{other}' is not an array"))),
    }
}

fn array(v: Vec<Value>) -> Value {
    Value::Array(Arc::new(v))
}

/// Regular-expression flags the way PostgreSQL reads them: case-sensitive unless `i` is given.
fn flags_of(vals: &[Cow<'_, Value>], at: usize) -> String {
    let f = vals.get(at).map(|f| text(f).into_owned()).unwrap_or_default();
    if f.contains('i') { f } else { format!("c{f}") }
}

fn quote_literal(s: &str) -> String {
    let body = s.replace('\'', "''");
    if s.contains('\\') {
        format!("E'{}'", body.replace('\\', "\\\\"))
    } else {
        format!("'{body}'")
    }
}

fn radix(v: &Value, base: u32) -> Result<String> {
    let i = int(v)?;
    // an int4 is shown as 32 bits when it is negative and fits in them, a bigint as 64
    let u = if i < 0 && i >= i64::from(i32::MIN) { u64::from(i as i32 as u32) } else { i as u64 };
    Ok(match base {
        16 => format!("{u:x}"),
        8 => format!("{u:o}"),
        _ => format!("{u:b}"),
    })
}

fn lgamma(x: f64) -> f64 {
    // Lanczos approximation (g = 7), with the reflection formula below 0.5
    const G: [f64; 9] = [
        0.999_999_999_999_809_9, 676.520_368_121_885_1, -1_259.139_216_722_402_8, 771.323_428_777_653_1,
        -176.615_029_162_140_6, 12.507_343_278_686_905, -0.138_571_095_265_720_12, 9.984_369_578_019_572e-6, 1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin().abs()).ln() - lgamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = G[0];
    let t = x + 7.5;
    for (i, g) in G.iter().enumerate().skip(1) {
        a += g / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

fn size_pretty(n: i64) -> String {
    let units = ["bytes", "kB", "MB", "GB", "TB", "PB"];
    let (mut v, mut i) = (n, 0);
    while v.abs() >= 10240 && i + 1 < units.len() {
        v = if v < 0 { -((-v + 512) / 1024) } else { (v + 512) / 1024 };
        i += 1;
    }
    format!("{v} {}", units[i])
}

fn size_bytes(s: &str) -> Result<i64> {
    let t = s.trim();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+')).unwrap_or(t.len());
    let (num, unit) = (&t[..split], t[split..].trim());
    let mult: f64 = match unit.to_ascii_lowercase().as_str() {
        "" | "b" | "bytes" => 1.0,
        "kb" => 1024.0,
        "mb" => 1024.0 * 1024.0,
        "gb" => 1024.0_f64.powi(3),
        "tb" => 1024.0_f64.powi(4),
        "pb" => 1024.0_f64.powi(5),
        _ => return Err(Error::runtime(format!("invalid size: \"{s}\""))),
    };
    let n: f64 = num.parse().map_err(|_| Error::runtime(format!("invalid size: \"{s}\"")))?;
    Ok((n * mult).round() as i64)
}

fn unistr(s: &str) -> Result<String> {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let bad = || Error::runtime("invalid Unicode escape");
        match it.peek().copied() {
            Some('\\') => {
                it.next();
                out.push('\\');
            }
            Some('+') => {
                it.next();
                let h: String = (0..6).filter_map(|_| it.next()).collect();
                out.push(u32::from_str_radix(&h, 16).ok().and_then(char::from_u32).ok_or_else(bad)?);
            }
            Some('u') => {
                it.next();
                let h: String = (0..4).filter_map(|_| it.next()).collect();
                out.push(u32::from_str_radix(&h, 16).ok().and_then(char::from_u32).ok_or_else(bad)?);
            }
            Some(d) if d.is_ascii_hexdigit() => {
                let h: String = (0..4).filter_map(|_| it.next()).collect();
                out.push(u32::from_str_radix(&h, 16).ok().and_then(char::from_u32).ok_or_else(bad)?);
            }
            _ => return Err(bad()),
        }
    }
    Ok(out)
}

/// Follows `keys` (object keys, or array positions written as numbers) down a document.
fn walk<'a>(mut cur: &'a J, keys: &[String]) -> Option<&'a J> {
    for k in keys {
        cur = match cur {
            J::Object(m) => m.get(k)?,
            J::Array(a) => {
                let i: i64 = k.parse().ok()?;
                let i = if i < 0 { a.len() as i64 + i } else { i };
                a.get(usize::try_from(i).ok()?)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

fn has_key(doc: &J, key: &str) -> bool {
    match doc {
        J::Object(m) => m.contains_key(key),
        J::Array(a) => a.iter().any(|x| matches!(x, J::String(s) if s == key)),
        J::String(s) => s == key,
        _ => false,
    }
}

fn pretty(j: &J) -> String {
    use serde::Serialize;
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    j.serialize(&mut ser).expect("a JSON value always serialises");
    String::from_utf8(buf).unwrap_or_default()
}

pub fn call(f: MoreFn, vals: &[Cow<'_, Value>]) -> Result<Value> {
    use MoreFn::*;
    let v = |i: usize| -> &Value { &vals[i] };
    Ok(match f {
        Strpos => {
            let (s, sub) = (text(v(0)), text(v(1)));
            Value::Int(s.find(&*sub).map_or(0, |b| s[..b].chars().count() as i64 + 1))
        }
        ToHex => Value::text(&radix(v(0), 16)?),
        ToBin => Value::text(&radix(v(0), 2)?),
        ToOct => Value::text(&radix(v(0), 8)?),
        Btrim => {
            let s = text(v(0));
            let set: Vec<char> = vals.get(1).map_or(vec![' '], |c| text(c).chars().collect());
            Value::text(s.trim_matches(|c| set.contains(&c)))
        }
        Chr => {
            let n = int(v(0))?;
            if n == 0 {
                return Err(Error::runtime("null character not permitted"));
            }
            match u32::try_from(n).ok().and_then(char::from_u32) {
                Some(c) => Value::text(&c.to_string()),
                None => return Err(Error::runtime(format!("requested character too large for encoding: {n}"))),
            }
        }
        StartsWith => Value::Bool(text(v(0)).starts_with(&*text(v(1)))),
        QuoteNullable => {
            if v(0).is_null() { Value::text("NULL") } else { Value::text(&quote_literal(&text(v(0)))) }
        }
        QuoteLiteral => Value::text(&quote_literal(&text(v(0)))),
        Normalize => Value::text(&text(v(0))),
        Casefold => Value::text(&text(v(0)).to_lowercase()),
        Unistr => Value::text(&unistr(&text(v(0)))?),
        NumNulls => Value::Int(vals.iter().filter(|x| x.is_null()).count() as i64),
        NumNonnulls => Value::Int(vals.iter().filter(|x| !x.is_null()).count() as i64),
        RegexpMatch => {
            let (s, p) = (text(v(0)).into_owned(), text(v(1)).into_owned());
            let re = super::extra::pattern(&p, &flags_of(vals, 2))?;
            match re.group(&s, 0) {
                None => Value::Null,
                Some(whole) => {
                    let n = re.group_count();
                    if n <= 1 {
                        array(vec![Value::text(&whole)])
                    } else {
                        array((1..n).map(|i| re.group(&s, i).map_or(Value::Null, |g| Value::text(&g))).collect())
                    }
                }
            }
        }
        RegexpSplitToArray => {
            let (s, p) = (text(v(0)).into_owned(), text(v(1)).into_owned());
            let re = super::extra::pattern(&p, &flags_of(vals, 2))?;
            let mut out = Vec::new();
            let mut last = 0;
            for (a, b) in re.find_all(&s) {
                if b == a {
                    continue;
                }
                out.push(Value::text(&s[last..a]));
                last = b;
            }
            out.push(Value::text(&s[last..]));
            array(out)
        }
        Factorial => {
            let n = int(v(0))?;
            if n < 0 {
                return Err(Error::runtime("factorial of a negative number is undefined"));
            }
            if n > 32177 {
                return Err(Error::runtime("value overflows numeric format"));
            }
            let mut r = BigInt::from(1);
            for i in 2..=n {
                r *= i;
            }
            Value::Big(r)
        }
        Lgamma => {
            let x = v(0).to_f64().ok_or_else(|| Error::type_err("a number was expected"))?;
            Value::from_f64(lgamma(x)).unwrap_or(Value::Null)
        }
        SizePretty => Value::text(&size_pretty(int(v(0))?)),
        SizeBytes => Value::Int(size_bytes(&text(v(0)))?),
        JsonBuildObject => {
            if vals.len() % 2 != 0 {
                return Err(Error::runtime("argument list must have even number of elements"));
            }
            let mut m = serde_json::Map::new();
            for pair in vals.chunks(2) {
                if pair[0].is_null() {
                    return Err(Error::runtime("null value not allowed for object key"));
                }
                m.insert(text(&pair[0]).into_owned(), json::from_value(&pair[1]));
            }
            json::json_value(&J::Object(m))
        }
        JsonBuildArray => json::json_value(&J::Array(vals.iter().map(|x| json::from_value(x)).collect())),
        JsonExtractPath | JsonExtractPathText => {
            let doc = json::document(v(0))?;
            let mut keys = Vec::new();
            for k in &vals[1..] {
                if k.is_null() {
                    return Ok(Value::Null);
                }
                match k.as_ref() {
                    Value::Array(a) => keys.extend(a.iter().map(|x| text(x).into_owned())),
                    other => keys.push(text(other).into_owned()),
                }
            }
            match walk(&doc, &keys) {
                None | Some(J::Null) if f == JsonExtractPathText => Value::Null,
                None => Value::Null,
                Some(J::String(s)) if f == JsonExtractPathText => Value::text(s),
                Some(j) if f == JsonExtractPathText => Value::text(&json::to_text(j)),
                Some(j) => json::json_value(j),
            }
        }
        JsonbExists => Value::Bool(has_key(&json::document(v(0))?, &text(v(1)))),
        JsonbExistsAny | JsonbExistsAll => {
            let doc = json::document(v(0))?;
            let keys = items(v(1))?;
            let hit = |k: &Value| !k.is_null() && has_key(&doc, &text(k));
            Value::Bool(if f == JsonbExistsAny { keys.iter().any(hit) } else { keys.iter().all(hit) })
        }
        JsonbPretty => Value::text(&pretty(&json::document(v(0))?)),
        JsonbContains => Value::Bool(json::contains(&json::document(v(0))?, &json::document(v(1))?)),
        JsonbContained => Value::Bool(json::contains(&json::document(v(1))?, &json::document(v(0))?)),
        JsonbDelete => {
            let mut doc = json::document(v(0))?;
            match (&mut doc, v(1)) {
                (J::Object(m), k) => {
                    m.remove(&*text(k));
                }
                (J::Array(a), Value::Int(i)) => {
                    let i = if *i < 0 { a.len() as i64 + i } else { *i };
                    if let Ok(i) = usize::try_from(i) {
                        if i < a.len() {
                            a.remove(i);
                        }
                    }
                }
                (J::Array(a), k) => {
                    let k = text(k).into_owned();
                    a.retain(|x| !matches!(x, J::String(s) if *s == k));
                }
                _ => return Err(Error::runtime("cannot delete from scalar")),
            }
            json::json_value(&doc)
        }
        JsonbDeletePath => {
            let mut doc = json::document(v(0))?;
            let steps: Vec<json::Step> = items(v(1))?
                .iter()
                .map(|k| {
                    let t = text(k).into_owned();
                    t.parse::<usize>().map_or(json::Step::Key(t), json::Step::Index)
                })
                .collect();
            json::remove(&mut doc, &steps);
            json::json_value(&doc)
        }
        JsonScalar => json::json_value(&json::from_value(v(0))),
        JsonSerialize => Value::text(&json::to_text(&json::document(v(0))?)),
        JsonExists => {
            let doc = json::document(v(0))?;
            Value::Bool(!crate::jsonpath::query(&doc, &crate::jsonpath::parse(&text(v(1)))?, None, true)?.is_empty())
        }
        ArrayToJson => json::json_value(&json::from_value(&array(items(v(0))?))),
        JsonbObject => {
            let mut m = serde_json::Map::new();
            let a = items(v(0))?;
            if let Some(b) = vals.get(1) {
                let b = items(b)?;
                if a.len() != b.len() {
                    return Err(Error::runtime("mismatched array dimensions"));
                }
                for (k, x) in a.iter().zip(&b) {
                    m.insert(text(k).into_owned(), if x.is_null() { J::Null } else { J::String(text(x).into_owned()) });
                }
            } else {
                if a.len() % 2 != 0 {
                    return Err(Error::runtime("array must have even number of elements"));
                }
                for p in a.chunks(2) {
                    m.insert(text(&p[0]).into_owned(), if p[1].is_null() { J::Null } else { J::String(text(&p[1]).into_owned()) });
                }
            }
            json::json_value(&J::Object(m))
        }
        ArrayReverse => {
            let mut a = items(v(0))?;
            a.reverse();
            array(a)
        }
        ArraySort => {
            let mut a = items(v(0))?;
            let desc = vals.get(1).is_some_and(|x| matches!(x.as_ref(), Value::Bool(true)));
            let nulls_first = vals.get(2).map_or(desc, |x| matches!(x.as_ref(), Value::Bool(true)));
            a.sort_by(|x, y| match (x.is_null(), y.is_null()) {
                (true, true) => Ordering::Equal,
                (true, false) => if nulls_first { Ordering::Less } else { Ordering::Greater },
                (false, true) => if nulls_first { Ordering::Greater } else { Ordering::Less },
                _ => if desc { y.cmp(x) } else { x.cmp(y) },
            });
            array(a)
        }
        ArrayShuffle | ArraySample => {
            let mut a = items(v(0))?;
            for i in (1..a.len()).rev() {
                a.swap(i, (super::extra::random() * (i + 1) as f64) as usize % (i + 1));
            }
            if f == ArraySample {
                let n = int(v(1))?;
                if n < 0 || n as usize > a.len() {
                    return Err(Error::runtime("sample size must be between 0 and the number of array elements"));
                }
                a.truncate(n as usize);
            }
            array(a)
        }
        UuidVersion => {
            let hex: Vec<char> = text(v(0)).chars().filter(|c| *c != '-').collect();
            match (hex.get(12).and_then(|c| c.to_digit(16)), hex.get(16).and_then(|c| c.to_digit(16))) {
                (Some(ver), Some(var)) if (8..=11).contains(&var) => Value::Int(i64::from(ver)),
                _ => Value::Null,
            }
        }
        Null => Value::Null,
    })
}
