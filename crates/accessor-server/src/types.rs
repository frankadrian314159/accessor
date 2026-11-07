//! Value types on the wire: the type OID a column is announced with, and the text and binary encodings of values.
//!
//! Accessor's values carry their own type, so a column's OID is taken from the first non-null value in it (text when
//! there is none). Values are announced and sent in the encodings a PostgreSQL client expects for that OID.

use accessor_core::{Row, Value};

pub const BOOL: u32 = 16;
pub const BYTEA: u32 = 17;
pub const INT8: u32 = 20;
pub const INT2: u32 = 21;
pub const INT4: u32 = 23;
pub const TEXT: u32 = 25;
pub const JSON: u32 = 114;
pub const FLOAT4: u32 = 700;
pub const FLOAT8: u32 = 701;
pub const MONEY: u32 = 790;
pub const VARCHAR: u32 = 1043;
pub const DATE: u32 = 1082;
pub const TIMESTAMP: u32 = 1114;
pub const TIMESTAMPTZ: u32 = 1184;
pub const INTERVAL: u32 = 1186;
pub const NUMERIC: u32 = 1700;
pub const JSONB: u32 = 3802;

/// The OID a value is announced with. A boolean is a `boolean` only in PostgreSQL's dialect; in MySQL's and T-SQL's it is
/// the number 1 or 0 (`bit`, `tinyint(1)`), and in ASQL it is shown as Yes/No text.
pub fn oid_of(v: &Value) -> Option<u32> {
    use accessor_core::dialect::{current, Dialect};
    Some(match v {
        Value::Null => return None,
        Value::Bool(_) => match current() {
            Dialect::Postgresql => BOOL,
            Dialect::Mysql | Dialect::Tsql => INT2,
            Dialect::Asql => TEXT,
        },
        Value::Int(_) => INT8,
        Value::Big(_) | Value::Dec(_) => NUMERIC,
        Value::Currency(_) => MONEY,
        Value::Float(_) => FLOAT4,
        Value::Double(_) => FLOAT8,
        Value::DateTime(_) => TIMESTAMP,
        Value::Date(_) => DATE,
        Value::TimestampTz(_) => TIMESTAMPTZ,
        Value::Interval(_) => INTERVAL,
        Value::Json(_) => JSONB,
        Value::Bytes(_) => BYTEA,
        _ => TEXT,
    })
}

/// One OID for each column of a result.
pub fn column_oids(columns: usize, rows: &[Row]) -> Vec<u32> {
    (0..columns).map(|c| rows.iter().find_map(|r| r.get(c).and_then(oid_of)).unwrap_or(TEXT)).collect()
}

/// The size announced for a type (-1 for variable length).
pub fn type_size(oid: u32) -> i16 {
    match oid {
        BOOL => 1,
        INT2 => 2,
        INT4 | FLOAT4 | DATE => 4,
        INT8 | FLOAT8 | TIMESTAMP | TIMESTAMPTZ | MONEY => 8,
        INTERVAL => 16,
        _ => -1,
    }
}

/// A value in PostgreSQL's text format.
pub fn text_of(v: &Value) -> String {
    use accessor_core::dialect::{current, Dialect};
    match v {
        Value::Bool(b) => match current() {
            Dialect::Postgresql => (if *b { "t" } else { "f" }).to_string(),
            Dialect::Mysql | Dialect::Tsql => (if *b { "1" } else { "0" }).to_string(),
            Dialect::Asql => v.to_string(),
        },
        Value::Text(s) => s.to_string(),
        other => other.to_string(),
    }
}

const POSTGRES_EPOCH_DAYS: i64 = 10_957; // 1970-01-01 to 2000-01-01
const POSTGRES_EPOCH_MS: i64 = POSTGRES_EPOCH_DAYS * 86_400_000;
const MS_PER_DAY: i64 = 86_400_000;

fn is_infinite(ms: i64) -> bool {
    accessor_core::datetime::is_infinite(ms)
}

/// A value in PostgreSQL's binary format for `oid`. The OIDs [`oid_of`] gives are all handled.
pub fn binary_of(oid: u32, v: &Value) -> Vec<u8> {
    let as_i64 = |v: &Value| -> i64 {
        match v {
            Value::Int(i) => *i,
            other => other.to_f64().unwrap_or(0.0) as i64,
        }
    };
    let as_f64 = |v: &Value| v.to_f64().unwrap_or(f64::NAN);
    match oid {
        BOOL => vec![u8::from(matches!(v, Value::Bool(true)))],
        INT2 if matches!(v, Value::Bool(_)) => i16::from(matches!(v, Value::Bool(true))).to_be_bytes().to_vec(),
        INT8 => as_i64(v).to_be_bytes().to_vec(),
        INT4 => (as_i64(v) as i32).to_be_bytes().to_vec(),
        INT2 => (as_i64(v) as i16).to_be_bytes().to_vec(),
        FLOAT8 => as_f64(v).to_be_bytes().to_vec(),
        FLOAT4 => (as_f64(v) as f32).to_be_bytes().to_vec(),
        MONEY => {
            let cents = (v.to_f64().unwrap_or(0.0) * 100.0).round() as i64;
            cents.to_be_bytes().to_vec()
        }
        BYTEA => match v {
            Value::Bytes(b) => b.to_vec(),
            other => other.to_string().into_bytes(),
        },
        NUMERIC => numeric_binary(&v.to_string()),
        TIMESTAMP | TIMESTAMPTZ => {
            let ms = match v {
                Value::DateTime(t) | Value::TimestampTz(t) | Value::Date(t) => *t,
                _ => 0,
            };
            let micros = if ms >= i64::MAX {
                i64::MAX
            } else if ms <= -i64::MAX {
                i64::MIN
            } else {
                (ms - POSTGRES_EPOCH_MS).saturating_mul(1000)
            };
            micros.to_be_bytes().to_vec()
        }
        DATE => {
            let ms = match v {
                Value::Date(t) | Value::DateTime(t) | Value::TimestampTz(t) => *t,
                _ => 0,
            };
            let days: i32 = if is_infinite(ms) { if ms > 0 { i32::MAX } else { i32::MIN } } else { (ms.div_euclid(MS_PER_DAY) - POSTGRES_EPOCH_DAYS) as i32 };
            days.to_be_bytes().to_vec()
        }
        INTERVAL => match v {
            Value::Interval(i) => {
                let mut out = (i.ms.saturating_mul(1000)).to_be_bytes().to_vec();
                out.extend_from_slice(&(i.days as i32).to_be_bytes());
                out.extend_from_slice(&(i.months as i32).to_be_bytes());
                out
            }
            _ => vec![0; 16],
        },
        JSONB => {
            let mut out = vec![1u8];
            out.extend_from_slice(text_of(v).as_bytes());
            out
        }
        _ => text_of(v).into_bytes(),
    }
}

/// A decimal number in text as PostgreSQL's binary `numeric`: base-10000 digits, a weight, a sign and a display scale.
pub fn numeric_binary(text: &str) -> Vec<u8> {
    let t = text.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let header = |ndigits: i16, weight: i16, sign: u16, dscale: i16| -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(&ndigits.to_be_bytes());
        h.extend_from_slice(&weight.to_be_bytes());
        h.extend_from_slice(&sign.to_be_bytes());
        h.extend_from_slice(&dscale.to_be_bytes());
        h
    };
    if !t.chars().all(|c| c.is_ascii_digit() || c == '.') || t.is_empty() {
        return header(0, 0, 0xC000, 0); // NaN
    }
    let (int_part, frac_part) = t.split_once('.').unwrap_or((t, ""));
    let dscale = frac_part.len() as i16;
    let int_part = int_part.trim_start_matches('0');
    let mut int_s = int_part.to_string();
    while int_s.len() % 4 != 0 {
        int_s.insert(0, '0');
    }
    let mut frac_s = frac_part.to_string();
    while frac_s.len() % 4 != 0 {
        frac_s.push('0');
    }
    let mut groups: Vec<i16> = Vec::new();
    for chunk in int_s.as_bytes().chunks(4).chain(frac_s.as_bytes().chunks(4)) {
        groups.push(std::str::from_utf8(chunk).unwrap_or("0").parse().unwrap_or(0));
    }
    let mut weight = (int_s.len() / 4) as i16 - 1;
    // leading zero groups go (the weight drops with each), and so do trailing ones
    while groups.first() == Some(&0) {
        groups.remove(0);
        weight -= 1;
    }
    while groups.last() == Some(&0) {
        groups.pop();
    }
    if groups.is_empty() {
        return header(0, 0, 0, dscale);
    }
    let mut out = header(groups.len() as i16, weight, if neg { 0x4000 } else { 0 }, dscale);
    for g in groups {
        out.extend_from_slice(&g.to_be_bytes());
    }
    out
}

/// A binary `numeric` as text.
fn numeric_text(b: &[u8]) -> Option<String> {
    if b.len() < 8 {
        return None;
    }
    let rd = |i: usize| i16::from_be_bytes([b[i], b[i + 1]]);
    let (n, weight, sign, dscale) = (rd(0) as usize, rd(2) as i32, u16::from_be_bytes([b[4], b[5]]), rd(6).max(0) as usize);
    if sign == 0xC000 {
        return Some("NaN".into());
    }
    let digits: Vec<i16> = (0..n).map(|k| rd(8 + 2 * k)).collect();
    let mut int_s = String::new();
    let mut frac_s = String::new();
    for w in (0..=weight.max(0)).rev() {
        let idx = weight - w;
        let g = if idx >= 0 && (idx as usize) < n { digits[idx as usize] } else { 0 };
        int_s.push_str(&format!("{g:04}"));
    }
    let mut w = -1;
    while frac_s.len() < dscale {
        let idx = weight - w;
        let g = if idx >= 0 && (idx as usize) < n { digits[idx as usize] } else { 0 };
        frac_s.push_str(&format!("{g:04}"));
        w -= 1;
    }
    let int_s = int_s.trim_start_matches('0');
    let mut out = String::new();
    if sign == 0x4000 {
        out.push('-');
    }
    out.push_str(if int_s.is_empty() { "0" } else { int_s });
    if dscale > 0 {
        out.push('.');
        out.push_str(&frac_s[..dscale]);
    }
    Some(out)
}

/// A parameter sent in binary format, as SQL text for the type it was declared with. `None` when the type is not handled.
pub fn binary_param_text(oid: u32, b: &[u8]) -> Option<String> {
    let be = |n: usize| b.get(..n);
    Some(match oid {
        BOOL => (if *b.first()? != 0 { "true" } else { "false" }).to_string(),
        INT2 => i16::from_be_bytes(be(2)?.try_into().ok()?).to_string(),
        INT4 => i32::from_be_bytes(be(4)?.try_into().ok()?).to_string(),
        INT8 => i64::from_be_bytes(be(8)?.try_into().ok()?).to_string(),
        FLOAT4 => f32::from_be_bytes(be(4)?.try_into().ok()?).to_string(),
        FLOAT8 => f64::from_be_bytes(be(8)?.try_into().ok()?).to_string(),
        NUMERIC => numeric_text(b)?,
        0 | TEXT | VARCHAR | JSON => String::from_utf8_lossy(b).into_owned(),
        JSONB => String::from_utf8_lossy(b.get(1..)?).into_owned(),
        BYTEA => format!("\\x{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
        DATE => {
            let days = i64::from(i32::from_be_bytes(be(4)?.try_into().ok()?));
            accessor_core::datetime::format_date((days + POSTGRES_EPOCH_DAYS) * MS_PER_DAY)
        }
        TIMESTAMP | TIMESTAMPTZ => {
            let micros = i64::from_be_bytes(be(8)?.try_into().ok()?);
            let ms = micros.div_euclid(1000) + POSTGRES_EPOCH_MS;
            if oid == TIMESTAMP { accessor_core::datetime::format(ms) } else { accessor_core::tz::format_in(ms, accessor_core::tz::Zone::UTC) }
        }
        _ => return None,
    })
}

/// Is a parameter of this type written bare in SQL (a number or a boolean), rather than quoted?
pub fn is_bare(oid: u32) -> bool {
    matches!(oid, BOOL | INT2 | INT4 | INT8 | FLOAT4 | FLOAT8 | NUMERIC)
}
