//! Functions that PostgreSQL has and Access does not: formatting (`to_char`, `to_date`, `to_timestamp`,
//! `to_number`), building dates (`make_date`, `make_timestamptz`), `date_trunc`, the numeric scale functions,
//! `pg_typeof`, `pg_input_is_valid`, `concat_ws`, `regexp_instr`, the hyperbolic arc functions, and the ones that
//! have nothing to do on a database with one user (`has_table_privilege` and its kin are always true).

use std::borrow::Cow;

use num_bigint::BigInt;

use crate::datetime;
use crate::decimal::Decimal;
use crate::error::{Error, Result};
use crate::tz;
use crate::value::{DataType, Value};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PgFn {
    InputIsValid,
    TypeOf,
    Scale,
    TrimScale,
    Asinh,
    Acosh,
    Atanh,
    IsFinite,
    Privilege,
    Sleep,
    Nothing,
    MakeDate,
    MakeTimestamp,
    MakeTimestamptz,
    DateTrunc,
    ToTimestamp,
    ToDate,
    ToChar,
    ToNumber,
    ConcatWs,
    RegexpInstr,
    WidthBucket,
    Encode,
    Decode,
    Zero,
    Same,
    JsonPathArray,
    JsonPathFirst,
    JsonPathExists,
    JsonPathMatch,
}

pub fn lookup(name: &str) -> Option<(PgFn, usize, usize)> {
    use PgFn::*;
    Some(match name {
        "PG_INPUT_IS_VALID" => (InputIsValid, 2, 2),
        "PG_TYPEOF" => (TypeOf, 1, 1),
        "SCALE" | "MIN_SCALE" => (Scale, 1, 1),
        "TRIM_SCALE" => (TrimScale, 1, 1),
        "ASINH" => (Asinh, 1, 1),
        "ACOSH" => (Acosh, 1, 1),
        "ATANH" => (Atanh, 1, 1),
        "ISFINITE" => (IsFinite, 1, 1),
        n if n.starts_with("HAS_") && n.ends_with("_PRIVILEGE") => (Privilege, 2, 4),
        "PG_SLEEP" => (Sleep, 1, 1),
        "PG_STAT_FORCE_NEXT_FLUSH" | "PG_ADVISORY_LOCK" | "PG_ADVISORY_UNLOCK" | "PG_ADVISORY_XACT_LOCK" | "PG_ADVISORY_UNLOCK_ALL" => (Nothing, 0, 2),
        "MAKE_DATE" => (MakeDate, 3, 3),
        "MAKE_TIMESTAMP" => (MakeTimestamp, 6, 6),
        "MAKE_TIMESTAMPTZ" => (MakeTimestamptz, 6, 7),
        "DATE_TRUNC" => (DateTrunc, 2, 3),
        "TO_TIMESTAMP" => (ToTimestamp, 1, 2),
        "TO_DATE" => (ToDate, 2, 2),
        "TO_CHAR" => (ToChar, 2, 2),
        "TO_NUMBER" => (ToNumber, 2, 2),
        "CONCAT_WS" => (ConcatWs, 2, usize::MAX),
        "REGEXP_INSTR" => (RegexpInstr, 2, 7),
        "WIDTH_BUCKET" => (WidthBucket, 4, 4),
        "JSONB_PATH_QUERY_ARRAY" | "JSONB_PATH_QUERY_ARRAY_TZ" => (JsonPathArray, 2, 4),
        "JSONB_PATH_QUERY_FIRST" | "JSONB_PATH_QUERY_FIRST_TZ" => (JsonPathFirst, 2, 4),
        "JSONB_PATH_EXISTS" | "JSONB_PATH_EXISTS_TZ" => (JsonPathExists, 2, 4),
        "JSONB_PATH_MATCH" | "JSONB_PATH_MATCH_TZ" => (JsonPathMatch, 2, 4),
        "ENCODE" => (Encode, 2, 2),
        "DECODE" => (Decode, 2, 2),
        "PG_RELATION_SIZE" | "PG_TOTAL_RELATION_SIZE" | "PG_TABLE_SIZE" | "PG_INDEXES_SIZE" | "PG_DATABASE_SIZE" => (Zero, 1, 2),
        "TO_REGCLASS" | "TO_REGTYPE" | "TO_REGPROC" | "TO_REGROLE" | "TO_REGNAMESPACE" | "TO_REGPROCEDURE" | "TO_REGOPER" | "TO_REGDATABASE" => (Same, 1, 1),
        _ => return None,
    })
}

impl PgFn {
    /// Functions that get their NULL arguments instead of answering NULL themselves.
    pub fn handles_null(self) -> bool {
        matches!(self, PgFn::ConcatWs | PgFn::TypeOf | PgFn::Nothing)
    }
}

fn text<'a>(v: &'a Value) -> Cow<'a, str> {
    match v {
        Value::Text(s) => Cow::Borrowed(&**s),
        other => Cow::Owned(other.to_string()),
    }
}

fn int(v: &Value) -> Result<i64> {
    v.coerce_to(DataType::Integer).and_then(|x| match x {
        Value::Int(i) => Ok(i),
        _ => Err(Error::type_err("a whole number was expected")),
    })
}

fn double(v: &Value) -> Result<f64> {
    v.to_f64().ok_or_else(|| Error::type_err(format!("'{v}' is not a number")))
}

fn finite(f: f64) -> Result<Value> {
    if f.is_finite() {
        Ok(Value::Double(f))
    } else {
        Err(Error::runtime("Overflow"))
    }
}

fn type_label(v: &Value) -> &'static str {
    match v.type_label() {
        "INTEGER" => "integer",
        "BIGNUM" | "DECIMAL" => "numeric",
        "FLOAT" => "real",
        "DOUBLE" => "double precision",
        "CURRENCY" => "money",
        "TEXT" => "text",
        "YESNO" => "boolean",
        "DATETIME" => "timestamp without time zone",
        "TIMESTAMPTZ" => "timestamp with time zone",
        "INTERVAL" => "interval",
        "JSON" => "jsonb",
        "RATIONAL" => "rational",
        _ => "unknown",
    }
}

pub fn call(f: PgFn, vals: &[Cow<'_, Value>]) -> Result<Value> {
    use PgFn::*;
    let v = |i: usize| -> &Value { &vals[i] };
    Ok(match f {
        InputIsValid => {
            let name = text(v(1)).to_ascii_lowercase();
            let name = name.split('(').next().unwrap_or("").trim().to_string();
            let (ty, _) = DataType::from_sql_name(&name).ok_or_else(|| Error::catalog(format!("type \"{name}\" does not exist")))?;
            Value::Bool(Value::text(&text(v(0))).coerce_to(ty).is_ok())
        }
        TypeOf => Value::text(type_label(v(0))),
        Scale => match v(0).num_lenient().as_deref() {
            Some(Value::Dec(d)) | Some(Value::Currency(d)) => Value::Int(i64::from(d.scale())),
            Some(Value::Int(_)) | Some(Value::Big(_)) => Value::Int(0),
            _ => return Err(Error::type_err("a number was expected")),
        },
        TrimScale => match v(0).num_lenient().as_deref() {
            Some(Value::Dec(d)) => Value::Dec(d.clone()),
            Some(other) => other.clone(),
            None => return Err(Error::type_err("a number was expected")),
        },
        Asinh => finite(double(v(0))?.asinh())?,
        Acosh => {
            let x = double(v(0))?;
            if x < 1.0 {
                return Err(Error::runtime("input is out of range"));
            }
            finite(x.acosh())?
        }
        Atanh => {
            let x = double(v(0))?;
            if x.abs() >= 1.0 {
                return Err(Error::runtime("input is out of range"));
            }
            finite(x.atanh())?
        }
        WidthBucket => {
            let (x, lo, hi) = (double(v(0))?, double(v(1))?, double(v(2))?);
            let n = match v(3) {
                Value::Int(n) if *n > 0 => *n,
                _ => return Err(Error::runtime("count must be greater than zero")),
            };
            if lo == hi {
                return Err(Error::runtime("lower bound cannot equal upper bound"));
            }
            let k = if lo < hi {
                if x < lo { 0 } else if x >= hi { n + 1 } else { ((x - lo) / (hi - lo) * n as f64).floor() as i64 + 1 }
            } else if x > lo {
                0
            } else if x <= hi {
                n + 1
            } else {
                ((lo - x) / (lo - hi) * n as f64).floor() as i64 + 1
            };
            Value::Int(k)
        }
        Encode => {
            let s = text(v(0)).into_owned();
            match text(v(1)).to_ascii_lowercase().as_str() {
                "hex" => Value::text(&s.bytes().map(|b| format!("{b:02x}")).collect::<String>()),
                "base64" => Value::text(&base64_encode(s.as_bytes())),
                "escape" => Value::text(&s),
                other => return Err(Error::runtime(format!("unrecognized encoding: \"{other}\""))),
            }
        }
        Decode => {
            let s = text(v(0)).into_owned();
            let bytes: Vec<u8> = match text(v(1)).to_ascii_lowercase().as_str() {
                "hex" => {
                    if s.len() % 2 != 0 {
                        return Err(Error::runtime("invalid hexadecimal data: odd number of digits"));
                    }
                    (0..s.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| Error::runtime("invalid hexadecimal digit")))
                        .collect::<Result<_>>()?
                }
                "base64" => base64_decode(&s)?,
                "escape" => s.into_bytes(),
                other => return Err(Error::runtime(format!("unrecognized encoding: \"{other}\""))),
            };
            Value::text(&String::from_utf8_lossy(&bytes))
        }
        JsonPathArray | JsonPathFirst | JsonPathExists | JsonPathMatch => {
            let (doc, path, vars, silent) = json_path_args(vals)?;
            let items = crate::jsonpath::query(&doc, &path, vars.as_ref(), silent)?;
            match f {
                JsonPathArray => json_value(&serde_json::Value::Array(items)),
                JsonPathFirst => items.first().map_or(Value::Null, json_value),
                JsonPathExists => Value::Bool(!items.is_empty()),
                _ => match items.as_slice() {
                    [serde_json::Value::Bool(b)] => Value::Bool(*b),
                    [serde_json::Value::Null] => Value::Null,
                    _ if silent => Value::Null,
                    _ => return Err(Error::runtime("single boolean result is expected")),
                },
            }
        }
        Zero => Value::Int(0),
        Same => v(0).clone(),
        IsFinite => Value::Bool(true),
        Privilege => Value::Bool(true),
        Sleep => {
            let s = double(v(0))?.clamp(0.0, 1.0);
            std::thread::sleep(std::time::Duration::from_secs_f64(s));
            Value::Null
        }
        Nothing => Value::Null,
        MakeDate => {
            let (y, m, d) = (int(v(0))?, int(v(1))?, int(v(2))?);
            let t = datetime::from_components(y, m as u32, d as u32, 0, 0, 0).ok_or_else(|| Error::runtime("date field value out of range"))?;
            Value::DateTime(t)
        }
        MakeTimestamp | MakeTimestamptz => {
            let (y, mo, d, h, mi) = (int(v(0))?, int(v(1))?, int(v(2))?, int(v(3))?, int(v(4))?);
            let s = double(v(5))?.floor() as i64;
            if !(0..=60).contains(&s) || h < 0 || mi < 0 {
                return Err(Error::runtime("time field value out of range"));
            }
            let local = datetime::from_components(y, mo as u32, d as u32, h as u32, mi as u32, s as u32)
                .ok_or_else(|| Error::runtime("date field value out of range"))?;
            if f == MakeTimestamp {
                Value::DateTime(local)
            } else {
                let zone = match vals.get(6) {
                    Some(z) => tz::Zone::parse(&text(z)).ok_or_else(|| Error::runtime(format!("time zone \"{}\" not recognized", text(z))))?,
                    None => tz::session(),
                };
                Value::TimestampTz(zone.to_utc(local))
            }
        }
        DateTrunc => {
            let field = text(v(0)).to_ascii_lowercase();
            let zone = match vals.get(2) {
                Some(z) => tz::Zone::parse(&text(z)).ok_or_else(|| Error::runtime(format!("time zone \"{}\" not recognized", text(z))))?,
                None => tz::session(),
            };
            let (local, aware) = match v(1) {
                Value::TimestampTz(t) => (zone.to_local(*t), true),
                other => match other.coerce_to(DataType::DateTime)? {
                    Value::DateTime(t) => (t, false),
                    _ => return Err(Error::type_err("a date or time was expected")),
                },
            };
            let t = trunc_date(&field, local)?;
            if aware {
                Value::TimestampTz(zone.to_utc(t))
            } else {
                Value::DateTime(t)
            }
        }
        ToTimestamp => {
            if vals.len() == 1 {
                let secs = double(v(0))?;
                Value::TimestampTz(secs.floor() as i64)
            } else {
                let local = parse_datetime(&text(v(0)), &text(v(1)))?;
                Value::TimestampTz(tz::session().to_utc(local))
            }
        }
        ToDate => {
            let local = parse_datetime(&text(v(0)), &text(v(1)))?;
            Value::DateTime(local.div_euclid(86_400) * 86_400)
        }
        ToChar => match v(0) {
            Value::DateTime(t) => Value::text(&format_datetime(*t, &text(v(1)))),
            Value::TimestampTz(t) => Value::text(&format_datetime(tz::session().to_local(*t), &text(v(1)))),
            other => match other.num_lenient().as_deref() {
                Some(n) => {
                    let d = n.coerce_to(DataType::Decimal)?;
                    let Value::Dec(d) = d else { return Err(Error::type_err("a number was expected")) };
                    Value::text(&format_number(&d, &text(v(1))))
                }
                None => Value::text(&text(other)),
            },
        },
        ToNumber => Value::Dec(parse_number(&text(v(0)), &text(v(1)))?),
        ConcatWs => {
            if vals[0].is_null() {
                return Ok(Value::Null);
            }
            let sep = text(v(0)).into_owned();
            Value::text(&vals[1..].iter().filter(|x| !x.is_null()).map(|x| text(x).into_owned()).collect::<Vec<_>>().join(&sep))
        }
        RegexpInstr => {
            let s = text(v(0)).into_owned();
            let flags = vals.get(5).map(|f| text(f).into_owned()).unwrap_or_default();
            let re = super::extra::pattern(&text(v(1)), &flags)?;
            let start = vals.get(2).map(|x| int(x)).transpose()?.unwrap_or(1).max(1) as usize;
            let nth = vals.get(3).map(|x| int(x)).transpose()?.unwrap_or(1).max(1) as usize;
            let end_option = vals.get(4).map(|x| int(x)).transpose()?.unwrap_or(0);
            let chars: Vec<(usize, char)> = s.char_indices().collect();
            let from = chars.get(start - 1).map_or(s.len(), |&(b, _)| b);
            let found = re.find_iter(&s[from..]).nth(nth - 1).map(|m| (m.start(), m.end()));
            match found {
                Some((a, b)) => {
                    let byte = from + if end_option == 1 { b } else { a };
                    let pos = s[..byte].chars().count() + 1;
                    Value::Int(pos as i64)
                }
                None => Value::Int(0),
            }
        }
    })
}

// ----- date_trunc ----------------------------------------------------------------------------------------

fn trunc_date(field: &str, t: i64) -> Result<i64> {
    let (y, mo, d, h, mi, _) = datetime::components(t);
    let make = |y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32| datetime::from_components(y, mo, d, h, mi, s).ok_or_else(|| Error::runtime("timestamp out of range"));
    match field {
        "microseconds" | "milliseconds" | "microsecond" | "millisecond" | "second" => Ok(t),
        "minute" => make(y, mo, d, h, mi, 0),
        "hour" => make(y, mo, d, h, 0, 0),
        "day" => make(y, mo, d, 0, 0, 0),
        "week" => {
            let days = t.div_euclid(86_400);
            let dow = (days + 3).rem_euclid(7); // Monday = 0
            Ok((days - dow) * 86_400)
        }
        "month" => make(y, mo, 1, 0, 0, 0),
        "quarter" => make(y, ((mo - 1) / 3) * 3 + 1, 1, 0, 0, 0),
        "year" => make(y, 1, 1, 0, 0, 0),
        "decade" => make(y.div_euclid(10) * 10, 1, 1, 0, 0, 0),
        "century" => make((y - 1).div_euclid(100) * 100 + 1, 1, 1, 0, 0, 0),
        "millennium" => make((y - 1).div_euclid(1000) * 1000 + 1, 1, 1, 0, 0, 0),
        other => Err(Error::runtime(format!("unit \"{other}\" not recognized for type timestamp"))),
    }
}

// ----- date and time patterns ---------------------------------------------------------------------------------

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// The pattern words, longest first.
const WORDS: [&str; 33] = [
    "HH24", "HH12", "SSSS", "YYYY", "MONTH", "A.M.", "P.M.", "HH", "MI", "SS", "MS", "US", "YYY", "YY", "MON", "MM", "DAY", "DDD", "DD", "DY", "AM", "PM", "WW", "TZ",
    "CC", "FM", "Y", "Q", "W", "J", "D", "RM", "IW",
];

enum Piece {
    Word(String), // as written in the pattern
    Literal(String),
}

fn pieces(fmt: &str) -> Vec<Piece> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' {
            let mut lit = String::new();
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                }
                lit.push(chars[i]);
                i += 1;
            }
            i += 1;
            out.push(Piece::Literal(lit));
            continue;
        }
        let rest: String = chars[i..].iter().collect::<String>();
        let up = rest.to_ascii_uppercase();
        if let Some(w) = WORDS.iter().find(|w| up.starts_with(**w)) {
            out.push(Piece::Word(rest[..w.len()].to_string()));
            i += w.len();
        } else {
            out.push(Piece::Literal(chars[i].to_string()));
            i += 1;
        }
    }
    out
}

/// The name in the case the pattern word was written in: `MONTH`, `Month` or `month`.
fn cased(name: &str, word: &str) -> String {
    let letters: String = word.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.chars().all(|c| c.is_uppercase()) {
        name.to_uppercase()
    } else if letters.chars().skip(1).all(|c| c.is_lowercase()) && letters.chars().next().is_some_and(char::is_uppercase) {
        name.to_string()
    } else {
        name.to_lowercase()
    }
}

fn format_datetime(t: i64, fmt: &str) -> String {
    let (y, mo, d, h, mi, s) = datetime::components(t);
    let days = t.div_euclid(86_400);
    let dow = (days + 4).rem_euclid(7) as usize; // Sunday = 0
    let yday = days - datetime::days_from_civil(y, 1, 1) + 1;
    let mut out = String::new();
    let mut fill_mode = true;
    for p in pieces(fmt) {
        let w = match p {
            Piece::Literal(l) => {
                out.push_str(&l);
                continue;
            }
            Piece::Word(w) => w,
        };
        let up = w.to_ascii_uppercase();
        let num = |v: i64, width: usize, fm: bool| if fm { v.to_string() } else { format!("{v:0width$}") };
        let fm = !fill_mode;
        match up.as_str() {
            "FM" => {
                fill_mode = !fill_mode;
                continue;
            }
            "YYYY" => out.push_str(&num(y, 4, fm)),
            "YYY" => out.push_str(&num(y.rem_euclid(1000), 3, fm)),
            "YY" => out.push_str(&num(y.rem_euclid(100), 2, fm)),
            "Y" => out.push_str(&(y.rem_euclid(10)).to_string()),
            "MM" => out.push_str(&num(i64::from(mo), 2, fm)),
            "DD" => out.push_str(&num(i64::from(d), 2, fm)),
            "DDD" => out.push_str(&num(yday, 3, fm)),
            "HH24" => out.push_str(&num(i64::from(h), 2, fm)),
            "HH12" | "HH" => out.push_str(&num(i64::from(if h % 12 == 0 { 12 } else { h % 12 }), 2, fm)),
            "MI" => out.push_str(&num(i64::from(mi), 2, fm)),
            "SS" => out.push_str(&num(i64::from(s), 2, fm)),
            "SSSS" => out.push_str(&(h * 3600 + mi * 60 + s).to_string()),
            "MS" | "US" => out.push_str(if up == "MS" { "000" } else { "000000" }),
            "AM" | "PM" => out.push_str(&cased(if h < 12 { "AM" } else { "PM" }, &w)),
            "A.M." | "P.M." => out.push_str(&cased(if h < 12 { "A.M." } else { "P.M." }, &w)),
            "MONTH" => {
                let n = MONTHS[(mo - 1) as usize];
                out.push_str(&if fm { cased(n, &w) } else { format!("{:<9}", cased(n, &w)) });
            }
            "MON" => out.push_str(&cased(&MONTHS[(mo - 1) as usize][..3], &w)),
            "DAY" => {
                let n = DAYS[dow];
                out.push_str(&if fm { cased(n, &w) } else { format!("{:<9}", cased(n, &w)) });
            }
            "DY" => out.push_str(&cased(&DAYS[dow][..3], &w)),
            "D" => out.push_str(&(dow + 1).to_string()),
            "Q" => out.push_str(&(((mo - 1) / 3) + 1).to_string()),
            "WW" => out.push_str(&num((yday - 1) / 7 + 1, 2, fm)),
            "W" => out.push_str(&((i64::from(d) - 1) / 7 + 1).to_string()),
            "CC" => out.push_str(&num((y - 1).div_euclid(100) + 1, 2, fm)),
            "J" => out.push_str(&(days + 2_440_588).to_string()),
            "TZ" => out.push_str(&cased("UTC", &w)),
            "RM" => out.push_str(["I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X", "XI", "XII"][(mo - 1) as usize]),
            "IW" => out.push_str(&num((yday - 1) / 7 + 1, 2, fm)),
            _ => out.push_str(&w),
        }
        fill_mode = true;
    }
    out
}

fn parse_datetime(input: &str, fmt: &str) -> Result<i64> {
    let s: Vec<char> = input.chars().collect();
    let mut i = 0;
    let (mut y, mut mo, mut d, mut h, mut mi, mut sec) = (1i64, 1i64, 1i64, 0i64, 0i64, 0i64);
    let mut pm: Option<bool> = None;
    let mut hour12 = false;
    let bad = |what: &str| Error::runtime(format!("invalid value for {what}"));
    for p in pieces(fmt) {
        match p {
            Piece::Literal(l) => {
                for c in l.chars() {
                    if i < s.len() && (s[i] == c || (!c.is_alphanumeric() && !s[i].is_alphanumeric())) {
                        i += 1;
                    } else if c.is_whitespace() {
                        continue;
                    } else {
                        return Err(bad(&format!("\"{c}\"")));
                    }
                }
            }
            Piece::Word(w) => {
                let up = w.to_ascii_uppercase();
                let digits = |max: usize, i: &mut usize| -> Result<i64> {
                    while *i < s.len() && s[*i] == ' ' {
                        *i += 1;
                    }
                    let start = *i;
                    while *i < s.len() && *i - start < max && s[*i].is_ascii_digit() {
                        *i += 1;
                    }
                    if start == *i {
                        return Err(Error::runtime(format!("invalid value \"{}\" for \"{w}\"", s[start.min(s.len())..].iter().collect::<String>())));
                    }
                    Ok(s[start..*i].iter().collect::<String>().parse().unwrap_or(0))
                };
                match up.as_str() {
                    "FM" => {}
                    "YYYY" => y = digits(4, &mut i)?,
                    "YYY" => y = 2000 + digits(3, &mut i)?,
                    "YY" => {
                        let v = digits(2, &mut i)?;
                        y = if v < 70 { 2000 + v } else { 1900 + v };
                    }
                    "Y" => y = 2000 + digits(1, &mut i)?,
                    "MM" => mo = digits(2, &mut i)?,
                    "DD" => d = digits(2, &mut i)?,
                    "DDD" => {
                        let n = digits(3, &mut i)?;
                        mo = 1;
                        d = n; // day of the year, resolved below
                    }
                    "HH24" => h = digits(2, &mut i)?,
                    "HH12" | "HH" => {
                        h = digits(2, &mut i)?;
                        hour12 = true;
                    }
                    "MI" => mi = digits(2, &mut i)?,
                    "SS" => sec = digits(2, &mut i)?,
                    "SSSS" => {
                        let v = digits(5, &mut i)?;
                        h = v / 3600;
                        mi = v % 3600 / 60;
                        sec = v % 60;
                    }
                    "MS" => {
                        digits(3, &mut i)?;
                    }
                    "US" => {
                        digits(6, &mut i)?;
                    }
                    "AM" | "PM" | "A.M." | "P.M." => {
                        let rest: String = s[i..].iter().collect::<String>().to_ascii_uppercase();
                        let len = if up.contains('.') { 4 } else { 2 };
                        pm = Some(rest.starts_with('P'));
                        if !(rest.starts_with("AM") || rest.starts_with("PM") || rest.starts_with("A.M.") || rest.starts_with("P.M.")) {
                            return Err(bad("\"AM\""));
                        }
                        i += len.min(s.len() - i);
                    }
                    "MONTH" | "MON" => {
                        let rest: String = s[i..].iter().collect::<String>().to_ascii_lowercase();
                        let found = MONTHS.iter().enumerate().find_map(|(k, m)| {
                            let full = m.to_ascii_lowercase();
                            if rest.starts_with(&full) {
                                Some((k, full.len()))
                            } else if rest.starts_with(&full[..3]) {
                                Some((k, 3))
                            } else {
                                None
                            }
                        });
                        let (k, len) = found.ok_or_else(|| bad("a month name"))?;
                        mo = k as i64 + 1;
                        i += len;
                    }
                    "DAY" | "DY" => {
                        let rest: String = s[i..].iter().collect::<String>().to_ascii_lowercase();
                        let found = DAYS.iter().find_map(|n| {
                            let full = n.to_ascii_lowercase();
                            if rest.starts_with(&full) {
                                Some(full.len())
                            } else if rest.starts_with(&full[..3]) {
                                Some(3)
                            } else {
                                None
                            }
                        });
                        i += found.ok_or_else(|| bad("a day name"))?;
                    }
                    _ => return Err(Error::runtime(format!("\"{w}\" is not supported in a date pattern for input"))),
                }
            }
        }
    }
    if hour12 {
        if !(1..=12).contains(&h) {
            return Err(Error::runtime(format!("hour \"{h}\" is invalid for the 12-hour clock")));
        }
        h = h % 12 + if pm == Some(true) { 12 } else { 0 };
    }
    // day of the year given as DDD
    if mo == 1 && d > 31 {
        let t = datetime::days_from_civil(y, 1, 1) + d - 1;
        let (yy, mm, dd) = datetime::civil_from_days(t);
        y = yy;
        mo = i64::from(mm);
        d = i64::from(dd);
    }
    datetime::from_components(y, mo as u32, d as u32, h as u32, mi as u32, sec as u32).ok_or_else(|| Error::runtime("date/time field value out of range"))
}

// ----- number patterns ---------------------------------------------------------------------------------------

fn format_number(value: &Decimal, fmt: &str) -> String {
    let chars: Vec<char> = fmt.chars().collect();
    let up: String = fmt.to_ascii_uppercase();
    let fm = up.starts_with("FM");
    let pr = up.contains("PR");
    let mi = up.contains("MI");
    let sign_word = up.contains('S') && !up.contains("SG") && !up.contains("SSSS");
    let currency = up.contains('L');
    // the picture: digits, group separators and the point
    let mut picture: Vec<char> = Vec::new(); // 9 0 , .
    let mut point: Option<usize> = None;
    let mut shift = 0usize;
    let mut after_v = false;
    let mut i = 0;
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect::<String>().to_ascii_uppercase();
        if rest.starts_with("FM") || rest.starts_with("PR") || rest.starts_with("MI") {
            i += 2;
            continue;
        }
        match chars[i] {
            '9' | '0' => {
                if after_v {
                    shift += 1;
                }
                picture.push(chars[i]);
            }
            ',' | 'G' | 'g' => picture.push(','),
            '.' | 'D' | 'd' => {
                point = Some(picture.len());
                picture.push('.');
            }
            'V' | 'v' => after_v = true,
            _ => {}
        }
        i += 1;
    }
    let int_slots: Vec<char> = picture[..point.unwrap_or(picture.len())].to_vec();
    let frac_slots = point.map_or(0, |p| picture[p + 1..].iter().filter(|c| **c == '9' || **c == '0').count());
    let frac_slots_shown = frac_slots - shift.min(frac_slots);
    let mut v = value.clone();
    if shift > 0 {
        v = v.mul(&Decimal::from_i64(10).powi(shift as u32).unwrap_or_else(|| Decimal::from_i64(1)));
    }
    let negative = v.is_negative();
    let abs = v.abs();
    // round half away from zero to the digits shown
    let scale = Decimal::from_i64(10).powi(frac_slots_shown as u32).unwrap_or_else(|| Decimal::from_i64(1));
    let half = Decimal::from_parts(BigInt::from(5), 1);
    let rounded = abs.mul(&scale).add(&half).trunc();
    let digits_all = rounded.to_string();
    let digits_all = digits_all.trim_start_matches('0').to_string();
    let mut padded = digits_all.clone();
    while padded.len() <= frac_slots_shown {
        padded.insert(0, '0');
    }
    let (int_digits, frac_digits) = padded.split_at(padded.len() - frac_slots_shown);
    let int_digits = int_digits.trim_start_matches('0').to_string();
    let slots = int_slots.iter().filter(|c| **c == '9' || **c == '0').count();
    if int_digits.len() > slots {
        return format!("{}{}", " ".repeat(usize::from(!fm)), "#".repeat(picture.len() + 1));
    }
    // fill the integer slots from the right
    let mut int_out: Vec<char> = Vec::new();
    let mut digit_iter = int_digits.chars().rev();
    let mut placed: Vec<Option<char>> = Vec::new();
    for slot in int_slots.iter().rev() {
        match slot {
            '9' | '0' => match digit_iter.next() {
                Some(d) => placed.push(Some(d)),
                None => placed.push(if *slot == '0' { Some('0') } else { None }),
            },
            _ => placed.push(Some(',')),
        }
    }
    placed.reverse();
    // a separator before the first digit is a blank
    let first_digit = placed.iter().position(|c| c.is_some_and(|c| c.is_ascii_digit()));
    for (k, c) in placed.iter().enumerate() {
        match (c, first_digit) {
            (Some(','), Some(f)) if k < f => int_out.push(' '),
            (Some(','), None) => int_out.push(' '),
            (Some(c), _) => int_out.push(*c),
            (None, _) => int_out.push(' '),
        }
    }
    let mut body: String = int_out.iter().collect();
    if point.is_some() {
        body.push('.');
        let mut f = frac_digits.to_string();
        while f.len() < frac_slots_shown {
            f.push('0');
        }
        body.push_str(&f);
    }
    if fm {
        body = body.trim_start().to_string();
        if body.contains('.') {
            body = body.trim_end_matches('0').trim_end_matches('.').to_string();
        }
    }
    let lead = if sign_word {
        if negative { "-" } else { "+" }
    } else if pr || mi {
        ""
    } else if negative {
        "-"
    } else if fm {
        ""
    } else {
        " "
    };
    let money = if currency { "$" } else { "" };
    let trimmed = body.trim_start();
    let pad = body.len() - trimmed.len();
    if pr {
        return if negative { format!("<{money}{trimmed}>") } else if fm { format!("{money}{trimmed}") } else { format!(" {money}{trimmed} ") };
    }
    let trailing = if mi { if negative { "-" } else if fm { "" } else { " " } } else { "" };
    if fm {
        return format!("{lead}{money}{trimmed}{trailing}");
    }
    // the sign slot sits just before the first character of the number
    format!("{}{lead}{money}{trimmed}{trailing}", " ".repeat(pad))
}

fn parse_number(input: &str, fmt: &str) -> Result<Decimal> {
    let up = fmt.to_ascii_uppercase();
    let negative = input.contains('-') || (input.contains('<') && input.contains('>')) || (up.contains("PR") && input.trim_start().starts_with('<'));
    let decimal_char = if up.contains('D') && !up.contains('.') { ',' } else { '.' };
    let mut s = String::new();
    for c in input.chars() {
        if c.is_ascii_digit() {
            s.push(c);
        } else if c == decimal_char && !s.contains('.') {
            s.push('.');
        }
    }
    if s.is_empty() || s == "." {
        return Err(Error::runtime("invalid input syntax for type numeric"));
    }
    let d = Decimal::parse(&s).ok_or_else(|| Error::runtime("invalid input syntax for type numeric"))?;
    Ok(if negative { d.neg() } else { d })
}

/// Is this call of `format` PostgreSQL's (`format('%s items', n)`, pattern first) and not Access's?
pub fn is_pg_format(vals: &[Value]) -> bool {
    match vals.first() {
        Some(Value::Text(t)) if vals.len() >= 3 => t.contains('%') || vals.len() >= 3,
        Some(Value::Text(t)) => {
            let b = t.as_bytes();
            b.iter().enumerate().any(|(i, &c)| c == b'%' && matches!(b.get(i + 1), Some(b's' | b'I' | b'L' | b'%' | b'0'..=b'9' | b'-')))
        }
        Some(Value::Null) => vals.len() >= 3,
        _ => vals.len() >= 3,
    }
}

/// PostgreSQL's `format(pattern, args...)`: `%s` text, `%I` identifier, `%L` literal, `%%`; `%2$s` picks an argument,
/// and a width (`%10s`, `%-10s`) pads.
pub fn pg_format(vals: &[Value]) -> Result<Value> {
    let Value::Text(pattern) = &vals[0] else {
        return Ok(Value::Null);
    };
    let args = &vals[1..];
    let mut out = String::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    let mut next = 0usize;
    while i < chars.len() {
        if chars[i] != '%' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        i += 1;
        // [position$]
        let mut j = i;
        let mut digits = String::new();
        while j < chars.len() && chars[j].is_ascii_digit() {
            digits.push(chars[j]);
            j += 1;
        }
        let mut position = None;
        if j < chars.len() && chars[j] == '$' && !digits.is_empty() {
            position = Some(digits.parse::<usize>().map_err(|_| Error::runtime("format: bad argument position"))?);
            i = j + 1;
        }
        let left = i < chars.len() && chars[i] == '-';
        if left {
            i += 1;
        }
        let mut width = String::new();
        while i < chars.len() && chars[i].is_ascii_digit() {
            width.push(chars[i]);
            i += 1;
        }
        let Some(&kind) = chars.get(i) else {
            return Err(Error::runtime("unterminated format() type specifier"));
        };
        i += 1;
        if kind == '%' {
            out.push('%');
            continue;
        }
        let at = match position {
            Some(p) => {
                if p == 0 {
                    return Err(Error::runtime("format specifies argument 0, but arguments are numbered from 1"));
                }
                p - 1
            }
            None => {
                next += 1;
                next - 1
            }
        };
        let arg = args.get(at).ok_or_else(|| Error::runtime("too few arguments for format()"))?;
        let rendered = match kind {
            's' => {
                if arg.is_null() {
                    String::new()
                } else {
                    text(arg).into_owned()
                }
            }
            'I' => {
                if arg.is_null() {
                    return Err(Error::runtime("null values cannot be formatted as an SQL identifier"));
                }
                let t = text(arg).into_owned();
                let plain = !t.is_empty()
                    && !t.starts_with(|c: char| c.is_ascii_digit())
                    && t.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
                if plain {
                    t
                } else {
                    format!("\"{}\"", t.replace('"', "\"\""))
                }
            }
            'L' => {
                if arg.is_null() {
                    "NULL".to_string()
                } else {
                    format!("'{}'", text(arg).replace('\'', "''"))
                }
            }
            other => return Err(Error::runtime(format!("unrecognized format() type specifier \"{other}\""))),
        };
        let w: usize = width.parse().unwrap_or(0);
        let len = rendered.chars().count();
        if len < w {
            let pad = " ".repeat(w - len);
            if left {
                out.push_str(&rendered);
                out.push_str(&pad);
            } else {
                out.push_str(&pad);
                out.push_str(&rendered);
            }
        } else {
            out.push_str(&rendered);
        }
    }
    Ok(Value::text(&out))
}

fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let d = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => return Err(Error::runtime(format!("invalid symbol \"{c}\" found while decoding base64 sequence"))),
        };
        acc = (acc << 6) | d;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// The target, path, variables and `silent` flag of the `jsonb_path_*` functions.
pub(crate) fn json_path_args(vals: &[Cow<'_, Value>]) -> Result<(serde_json::Value, crate::jsonpath::Path, Option<serde_json::Value>, bool)> {
    let doc = crate::json::document(&vals[0])?;
    let path = crate::jsonpath::parse(&text(&vals[1]))?;
    let vars = match vals.get(2) {
        Some(v) if !v.is_null() => Some(crate::json::document(v)?),
        _ => None,
    };
    let silent = matches!(vals.get(3).map(|v| &**v), Some(Value::Bool(true)));
    Ok((doc, path, vars, silent))
}

/// A JSON value as a `JSON` value (a string stays quoted).
pub(crate) fn json_value(j: &serde_json::Value) -> Value {
    Value::Json(j.to_string().into())
}
