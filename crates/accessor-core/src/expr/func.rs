//! Built-in scalar functions (the commonly used subset of Access's library).

use std::borrow::Cow;

use super::BExpr;
use crate::datetime;
use crate::json;
use super::extra;
use crate::tz;
use crate::error::{Error, Result};
use crate::decimal::Decimal;
use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive};

use crate::rational::Rational;
use crate::value::{num_cmp, DataType, Value};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScalarFn {
    /// The functions of `pgfuncs.rs`.
    Pg(super::pgfuncs::PgFn),
    Iif,
    Nz,
    IsNull,
    Coalesce,
    UCase,
    LCase,
    Len,
    Left,
    Right,
    Mid,
    Trim,
    LTrim,
    RTrim,
    InStr,
    Replace,
    Abs,
    Round,
    Int,
    Fix,
    Sqr,
    Sgn,
    Exp,
    Log,
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
    Weekday,
    Now,
    Date,
    DateAdd,
    DateDiff,
    DateSerial,
    Val,
    CStr,
    CInt,
    /// `CAST(x AS INTEGER)`: like `CInt`, but a number with a fraction is truncated toward zero, not rounded.
    CastInt,
    CastBig,
    CSng,
    CDbl,
    CBig,
    CDec,
    CCur,
    CRat,
    Rational,
    Numerator,
    Denominator,
    TypeName,
    CBool,
    CDate,
    Format,
    DatePart,
    TrimOf,
    NullIf,
    Greatest,
    Least,
    Concat,
    Ceil,
    Power,
    Mod,
    Log10,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    OctetLength,
    /// `(mask, i1, i2, ...)`: bits of a GROUPING mask, as a number.
    GroupingBit,
    NowTz,
    LocalNow,
    LocalDate,
    SessionTimeZone,
    AtTimeZone,
    TimeZone,
    CTimestampTz,
    CInterval,
    MakeInterval,
    Age,
    Nextval,
    Currval,
    Lastval,
    Setval,
    Lpad,
    Rpad,
    Repeat,
    Reverse,
    Overlay,
    Translate,
    Initcap,
    RegexpLike,
    RegexpReplace,
    RegexpSubstr,
    RegexpCount,
    SimilarTo,
    Atan2,
    Degrees,
    Radians,
    Pi,
    Random,
    Trunc,
    JsonValid,
    JsonExtract,
    JsonValue,
    JsonGet,
    JsonGetText,
    JsonArray,
    JsonObject,
    JsonLength,
    JsonType,
    JsonKeys,
    JsonSet,
    JsonRemove,
    JsonMergePatch,
    JsonContains,
    ToJson,
    JsonParse,
}

impl ScalarFn {
    /// Functions whose value changes from call to call, so they are never folded into a constant.
    pub fn is_volatile(self) -> bool {
        matches!(
            self,
            ScalarFn::Now
                | ScalarFn::Date
                | ScalarFn::NowTz
                | ScalarFn::LocalNow
                | ScalarFn::LocalDate
                | ScalarFn::Nextval
                | ScalarFn::Currval
                | ScalarFn::Lastval
                | ScalarFn::Setval
                | ScalarFn::Random
        )
    }

    /// Resolves a function name to `(function, min_args, max_args)`.
    pub fn lookup(name: &str) -> Option<(ScalarFn, usize, usize)> {
        use ScalarFn::*;
        let up = name.to_ascii_uppercase();
        if let Some((f, min, max)) = super::pgfuncs::lookup(&up) {
            return Some((Pg(f), min, max));
        }
        Some(match up.as_str() {
            "IIF" => (Iif, 3, 3),
            "NZ" => (Nz, 1, 2),
            "ISNULL" => (IsNull, 1, 1),
            "COALESCE" => (Coalesce, 1, usize::MAX),
            "UCASE" | "UPPER" => (UCase, 1, 1),
            "LCASE" | "LOWER" => (LCase, 1, 1),
            "LEN" | "LENGTH" | "CHAR_LENGTH" | "CHARACTER_LENGTH" => (Len, 1, 1),
            "LEFT" => (Left, 2, 2),
            "RIGHT" => (Right, 2, 2),
            "MID" | "SUBSTR" | "SUBSTRING" => (Mid, 2, 3),
            "TRIM" => (Trim, 1, 1),
            "LTRIM" => (LTrim, 1, 1),
            "RTRIM" => (RTrim, 1, 1),
            "INSTR" => (InStr, 2, 3),
            "REPLACE" => (Replace, 3, 3),
            "ABS" => (Abs, 1, 1),
            "ROUND" => (Round, 1, 2),
            "INT" | "FLOOR" => (Int, 1, 1),
            "FIX" => (Fix, 1, 1),
            "SQR" | "SQRT" => (Sqr, 1, 1),
            "SGN" | "SIGN" => (Sgn, 1, 1),
            "EXP" => (Exp, 1, 1),
            "LOG" | "LN" => (Log, 1, 1),
            "YEAR" => (Year, 1, 1),
            "MONTH" => (Month, 1, 1),
            "DAY" => (Day, 1, 1),
            "HOUR" => (Hour, 1, 1),
            "MINUTE" => (Minute, 1, 1),
            "SECOND" => (Second, 1, 1),
            "WEEKDAY" => (Weekday, 1, 1),
            "NOW" => (Now, 0, 0),
            "DATE" | "TODAY" => (Date, 0, 0),
            "DATEADD" => (DateAdd, 3, 3),
            "DATEDIFF" => (DateDiff, 3, 3),
            "DATESERIAL" => (DateSerial, 3, 3),
            "VAL" => (Val, 1, 1),
            "CSTR" => (CStr, 1, 1),
            "CINT" | "CLNG" => (CInt, 1, 1),
            "CAST_INTEGER" => (CastInt, 1, 1),
            "CAST_BIGNUM" => (CastBig, 1, 1),
            "CSNG" | "CFLOAT" => (CSng, 1, 1),
            "CDBL" => (CDbl, 1, 1),
            "CBIG" => (CBig, 1, 1),
            "CDEC" => (CDec, 1, 1),
            "CCUR" => (CCur, 1, 1),
            "CRAT" => (CRat, 1, 1),
            "RATIONAL" => (Rational, 2, 2),
            "NUMERATOR" => (Numerator, 1, 1),
            "DENOMINATOR" => (Denominator, 1, 1),
            "TYPENAME" => (TypeName, 1, 1),
            "CBOOL" => (CBool, 1, 1),
            "CDATE" => (CDate, 1, 1),
            "FORMAT" => (Format, 1, usize::MAX),
            "DATEPART" | "DATE_PART" => (DatePart, 2, 2),
            "TRIMOF" => (TrimOf, 3, 3),
            "NULLIF" => (NullIf, 2, 2),
            "GREATEST" => (Greatest, 1, usize::MAX),
            "LEAST" => (Least, 1, usize::MAX),
            "CONCAT" => (Concat, 1, usize::MAX),
            "CEIL" | "CEILING" => (Ceil, 1, 1),
            "POWER" | "POW" => (Power, 2, 2),
            "MOD" => (Mod, 2, 2),
            "LOG10" => (Log10, 1, 1),
            "SIN" => (Sin, 1, 1),
            "COS" => (Cos, 1, 1),
            "TAN" => (Tan, 1, 1),
            "ASIN" => (Asin, 1, 1),
            "ACOS" => (Acos, 1, 1),
            "ATAN" => (Atan, 1, 1),
            "SINH" => (Sinh, 1, 1),
            "COSH" => (Cosh, 1, 1),
            "TANH" => (Tanh, 1, 1),
            "OCTET_LENGTH" => (OctetLength, 1, 1),
            "GROUPINGBIT" => (GroupingBit, 2, usize::MAX),
            "NOWTZ" => (NowTz, 0, 0),
            "LOCALNOW" => (LocalNow, 0, 0),
            "LOCALDATE" => (LocalDate, 0, 0),
            "SESSIONTIMEZONE" | "CURRENT_TIMEZONE" => (SessionTimeZone, 0, 0),
            "ATTIMEZONE" => (AtTimeZone, 2, 2),
            "TIMEZONE" => (TimeZone, 2, 2),
            "CTIMESTAMPTZ" | "CTZ" => (CTimestampTz, 1, 1),
            "CINTERVAL" => (CInterval, 1, 1),
            "MAKEINTERVAL" | "MAKE_INTERVAL" => (MakeInterval, 1, 6),
            "AGE" => (Age, 2, 2),
            "LPAD" => (Lpad, 2, 3),
            "RPAD" => (Rpad, 2, 3),
            "REPEAT" | "REPLICATE" => (Repeat, 2, 2),
            "REVERSE" | "STRREVERSE" => (Reverse, 1, 1),
            "OVERLAY" => (Overlay, 3, 4),
            "TRANSLATE" => (Translate, 3, 3),
            "INITCAP" | "PROPER" => (Initcap, 1, 1),
            "REGEXP_LIKE" => (RegexpLike, 2, 3),
            "REGEXP_REPLACE" => (RegexpReplace, 3, 4),
            "REGEXP_SUBSTR" => (RegexpSubstr, 2, 4),
            "REGEXP_COUNT" => (RegexpCount, 2, 3),
            "SIMILAR_TO" => (SimilarTo, 2, 2),
            "ATAN2" => (Atan2, 2, 2),
            "DEGREES" => (Degrees, 1, 1),
            "RADIANS" => (Radians, 1, 1),
            "PI" => (Pi, 0, 0),
            "RANDOM" | "RAND" | "RND" => (Random, 0, 0),
            "TRUNC" | "TRUNCATE" => (Trunc, 1, 2),
            "NEXTVAL" => (Nextval, 1, 1),
            "CURRVAL" => (Currval, 1, 1),
            "LASTVAL" => (Lastval, 0, 0),
            "SETVAL" => (Setval, 2, 3),
            "JSON_VALID" => (JsonValid, 1, 1),
            "JSON_EXTRACT" | "JSON_QUERY" => (JsonExtract, 2, 2),
            "JSON_VALUE" => (JsonValue, 2, 2),
            "JSON_GET" => (JsonGet, 2, 2),
            "JSON_GET_TEXT" => (JsonGetText, 2, 2),
            "JSON_ARRAY" => (JsonArray, 0, usize::MAX),
            "JSON_OBJECT" => (JsonObject, 0, usize::MAX),
            "JSON_LENGTH" | "JSON_ARRAY_LENGTH" => (JsonLength, 1, 2),
            "JSON_TYPE" | "JSON_TYPEOF" | "JSONB_TYPEOF" => (JsonType, 1, 2),
            "JSON_KEYS" => (JsonKeys, 1, 2),
            "JSON_SET" => (JsonSet, 3, 3),
            "JSON_REMOVE" => (JsonRemove, 2, 2),
            "JSON_MERGE_PATCH" | "JSON_MERGE" => (JsonMergePatch, 2, 2),
            "JSON_CONTAINS" => (JsonContains, 2, 2),
            "TO_JSON" | "TO_JSONB" => (ToJson, 1, 1),
            "JSON" | "JSON_PARSE" => (JsonParse, 1, 1),
            _ => return None,
        })
    }

    /// Functions that tolerate (and may transform) `Null` arguments; all
    /// others yield `Null` as soon as any argument is `Null`.
    fn handles_null(self) -> bool {
        if let ScalarFn::Pg(p) = self {
            return p.handles_null();
        }
        matches!(
            self,
            ScalarFn::Nz
                | ScalarFn::IsNull
                | ScalarFn::Coalesce
                | ScalarFn::Iif
                | ScalarFn::NullIf
                | ScalarFn::Concat
                | ScalarFn::JsonArray
                | ScalarFn::JsonObject
                | ScalarFn::JsonSet
                | ScalarFn::ToJson
        )
    }
}

fn text_of(v: &Value) -> Cow<'_, str> {
    match v {
        Value::Text(s) => Cow::Borrowed(&**s),
        other => Cow::Owned(other.to_string()),
    }
}

fn num_of(v: &Value) -> Result<Cow<'_, Value>> {
    v.num_lenient().ok_or_else(|| Error::type_err(format!("'{v}' is not a number")))
}

/// A whole-number argument; fractions are rounded, ties to even.
fn int_of(v: &Value) -> Result<i64> {
    match &*num_of(v)? {
        Value::Int(i) => Ok(*i),
        other => other.rounded_int().and_then(|b| b.to_i64()).ok_or_else(|| Error::type_err("number out of range")),
    }
}

fn big_of(v: &Value) -> Result<BigInt> {
    num_of(v)?.rounded_int().ok_or_else(|| Error::type_err("number out of range"))
}

fn f64_of(v: &Value) -> Result<f64> {
    v.to_f64()
        .ok_or_else(|| Error::type_err(format!("'{v}' is not a number")))
}

/// How a number compares with zero.
fn sign_of(v: &Value) -> Result<std::cmp::Ordering> {
    Ok(num_cmp(&*num_of(v)?, &Value::Int(0)))
}

/// A result computed in floating point (square roots, logarithms and the like),
/// which is only as precise as an `f64`. It is a `FLOAT` if the argument was one.
fn float_result(f: f64, like: &Value, what: &str) -> Result<Value> {
    let v = if matches!(like, Value::Float(_)) { Value::from_f32(f as f32) } else { Value::from_f64(f) };
    v.ok_or_else(|| Error::runtime(format!("{what}: the result is not a number")))
}

fn abs_of(v: &Value) -> Result<Value> {
    Ok(match &*num_of(v)? {
        Value::Int(i) => i.checked_abs().map_or_else(|| Value::Big(BigInt::from(*i).abs()), Value::Int),
        Value::Big(b) => Value::Big(b.abs()),
        Value::Float(f) => Value::Float(f.abs()),
        Value::Double(f) => Value::Double(f.abs()),
        Value::Dec(d) => Value::Dec(d.abs()),
        Value::Currency(d) => Value::Currency(d.abs()),
        Value::Rational(r) => Value::Rational(r.abs()),
        _ => unreachable!("num_lenient gives numbers"),
    })
}

/// Rounds to `digits` places, ties to even, keeping the type. A float is rounded
/// by its shortest decimal form, so `Round(2.675, 2)` is 2.68 whatever the type.
fn round_of(v: &Value, digits: u32) -> Result<Value> {
    let via_decimal = |d: Decimal| d.round(digits);
    Ok(match &*num_of(v)? {
        n @ (Value::Int(_) | Value::Big(_)) => n.clone(),
        Value::Dec(d) => Value::Dec(d.round(digits)),
        Value::Currency(d) => Value::Currency(d.round(digits)),
        Value::Rational(r) => {
            let scale = Rational::from_int(BigInt::from(10u32).pow(digits));
            Value::Rational(Rational::from_int(r.mul(&scale).round()).div(&scale).expect("scale is not zero"))
        }
        n @ (Value::Float(_) | Value::Double(_)) => {
            let rounded = via_decimal(n.as_decimal().ok_or_else(|| Error::runtime("Round: not a finite number"))?);
            if matches!(n, Value::Float(_)) {
                Value::Float(rounded.to_f64() as f32)
            } else {
                Value::Double(rounded.to_f64())
            }
        }
        _ => unreachable!("num_lenient gives numbers"),
    })
}

/// `Trunc(x, digits)`: cut off, not rounded, keeping the type.
fn trunc_of(v: &Value, digits: i64) -> Result<Value> {
    use num_traits::ToPrimitive;
    Ok(match &*num_of(v)? {
        Value::Int(i) => {
            if digits >= 0 {
                Value::Int(*i)
            } else {
                let (m, _) = extra::truncate(&Decimal::from_i64(*i), digits).parts();
                m.to_i64().map_or_else(|| Value::Big(m), Value::Int)
            }
        }
        Value::Big(b) => {
            if digits >= 0 {
                Value::Big(b.clone())
            } else {
                Value::Big(extra::truncate(&Decimal::from_parts(b.clone(), 0), digits).parts().0)
            }
        }
        Value::Dec(d) => Value::Dec(extra::truncate(d, digits)),
        Value::Currency(d) => Value::Currency(extra::truncate(d, digits)),
        Value::Rational(r) => {
            let unit = Rational::from_int(BigInt::from(10u32).pow(digits.unsigned_abs() as u32));
            if digits >= 0 {
                Value::Rational(Rational::from_int(r.mul(&unit).trunc()).div(&unit).expect("unit is not zero"))
            } else {
                Value::Rational(Rational::from_int(r.div(&unit).expect("unit is not zero").trunc()).mul(&unit))
            }
        }
        n @ (Value::Float(_) | Value::Double(_)) => {
            let cut = extra::truncate(&n.as_decimal().ok_or_else(|| Error::runtime("Trunc: not a finite number"))?, digits);
            if matches!(n, Value::Float(_)) {
                Value::Float(cut.to_f64() as f32)
            } else {
                Value::Double(cut.to_f64())
            }
        }
        _ => unreachable!("num_lenient gives numbers"),
    })
}

/// `Int` (floor) and `Fix` (toward zero) for any numeric type.
fn whole_part(v: &Value, floor: bool) -> Result<Value> {
    Ok(match &*num_of(v)? {
        n @ (Value::Int(_) | Value::Big(_)) => n.clone(),
        Value::Dec(d) => Value::Dec(if floor { d.floor() } else { d.trunc() }),
        Value::Currency(d) => Value::Currency(if floor { d.floor() } else { d.trunc() }),
        Value::Rational(r) => Value::Big(if floor { r.floor() } else { r.trunc() }),
        Value::Float(f) => Value::Float(if floor { f.floor() } else { f.trunc() }),
        Value::Double(f) => Value::Double(if floor { f.floor() } else { f.trunc() }),
        _ => unreachable!("num_lenient gives numbers"),
    })
}

fn date_of(v: &Value) -> Result<i64> {
    match v.coerce_to(DataType::DateTime)? {
        Value::DateTime(t) => Ok(t),
        _ => Err(Error::type_err("expected a date")),
    }
}

pub fn call(f: ScalarFn, args: &[BExpr], row: &[Value]) -> Result<Value> {
    use ScalarFn::*;
    // Lazily evaluated forms first.
    match f {
        Iif => {
            return Ok(if args[0].eval_tri(row)? == Some(true) {
                args[1].eval(row)?.into_owned()
            } else {
                args[2].eval(row)?.into_owned()
            });
        }
        Nz => {
            let v = args[0].eval(row)?;
            if !v.is_null() {
                return Ok(v.into_owned());
            }
            return match args.get(1) {
                Some(d) => Ok(d.eval(row)?.into_owned()),
                None => Ok(Value::Int(0)),
            };
        }
        Coalesce => {
            for a in args {
                let v = a.eval(row)?;
                if !v.is_null() {
                    return Ok(v.into_owned());
                }
            }
            return Ok(Value::Null);
        }
        IsNull => return Ok(Value::Bool(args[0].eval(row)?.is_null())),
        Now => return Ok(Value::DateTime(datetime::now())),
        Date => return Ok(Value::DateTime(datetime::now().div_euclid(86_400) * 86_400)),
        NowTz => return Ok(Value::TimestampTz(datetime::now())),
        LocalNow => return Ok(Value::DateTime(tz::session().to_local(datetime::now()))),
        LocalDate => return Ok(Value::DateTime(tz::session().to_local(datetime::now()).div_euclid(86_400) * 86_400)),
        SessionTimeZone => return Ok(Value::text(&tz::session().name())),
        _ => {}
    }

    let vals: Vec<Cow<'_, Value>> = args.iter().map(|a| a.eval(row)).collect::<Result<_>>()?;
    if f == Format {
        // PostgreSQL's format('Hello %s', x) takes the pattern first; Access's Format(x, 'pattern') takes the value first
        let owned: Vec<Value> = vals.iter().map(|v| (**v).clone()).collect();
        if super::pgfuncs::is_pg_format(&owned) {
            return super::pgfuncs::pg_format(&owned);
        }
        if owned.len() > 2 {
            return Err(Error::runtime("Format takes a value and a pattern"));
        }
    }
    if !f.handles_null() && vals.iter().any(|v| v.is_null()) {
        return Ok(Value::Null);
    }
    let v0 = vals.first().map(|v| &**v);

    Ok(match f {
        UCase => Value::text(&text_of(v0.unwrap()).to_uppercase()),
        LCase => Value::text(&text_of(v0.unwrap()).to_lowercase()),
        Len => Value::Int(text_of(v0.unwrap()).chars().count() as i64),
        Trim => Value::text(text_of(v0.unwrap()).trim()),
        LTrim => Value::text(text_of(v0.unwrap()).trim_start()),
        RTrim => Value::text(text_of(v0.unwrap()).trim_end()),
        Left => {
            let n = int_of(&vals[1])?.max(0) as usize;
            Value::text(&text_of(&vals[0]).chars().take(n).collect::<String>())
        }
        Right => {
            let n = int_of(&vals[1])?.max(0) as usize;
            let s = text_of(&vals[0]);
            let skip = s.chars().count().saturating_sub(n);
            Value::text(&s.chars().skip(skip).collect::<String>())
        }
        Mid => {
            let start = (int_of(&vals[1])? - 1).max(0) as usize;
            let s = text_of(&vals[0]);
            let it = s.chars().skip(start);
            match vals.get(2) {
                Some(n) => Value::text(&it.take(int_of(n)?.max(0) as usize).collect::<String>()),
                None => Value::text(&it.collect::<String>()),
            }
        }
        InStr => {
            // InStr([start,] haystack, needle) — 1-based, 0 when absent.
            let (start, hay, needle) = if vals.len() == 3 {
                (int_of(&vals[0])?.max(1) as usize, text_of(&vals[1]), text_of(&vals[2]))
            } else {
                (1, text_of(&vals[0]), text_of(&vals[1]))
            };
            let hay_l: Vec<char> = hay.to_lowercase().chars().collect();
            let nee_l: Vec<char> = needle.to_lowercase().chars().collect();
            let from = start - 1;
            let pos = if nee_l.is_empty() {
                Some(from)
            } else if hay_l.len() < nee_l.len() || from > hay_l.len() - nee_l.len() {
                None
            } else {
                (from..=hay_l.len() - nee_l.len()).find(|&i| hay_l[i..i + nee_l.len()] == nee_l[..])
            };
            Value::Int(pos.map_or(0, |p| p as i64 + 1))
        }
        Replace => {
            let (s, find, with) = (text_of(&vals[0]), text_of(&vals[1]), text_of(&vals[2]));
            if find.is_empty() {
                Value::text(&s)
            } else {
                Value::text(&s.replace(&*find, &with))
            }
        }
        Abs => abs_of(v0.unwrap())?,
        Round => {
            let digits = match vals.get(1) {
                Some(d) => int_of(d)?,
                None => 0,
            };
            if !(0..=10_000).contains(&digits) {
                return Err(Error::runtime("Round: digits must be between 0 and 10000"));
            }
            round_of(v0.unwrap(), digits as u32)?
        }
        Int => whole_part(v0.unwrap(), true)?,
        Fix => whole_part(v0.unwrap(), false)?,
        Sqr => {
            if sign_of(v0.unwrap())?.is_lt() {
                return Err(Error::runtime("Sqr: invalid argument"));
            }
            float_result(f64_of(v0.unwrap())?.sqrt(), v0.unwrap(), "Sqr")?
        }
        Sgn => Value::Int(match sign_of(v0.unwrap())? {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }),
        Exp => float_result(f64_of(v0.unwrap())?.exp(), v0.unwrap(), "Exp")?,
        Log => {
            if !sign_of(v0.unwrap())?.is_gt() {
                return Err(Error::runtime("Log: invalid argument"));
            }
            float_result(f64_of(v0.unwrap())?.ln(), v0.unwrap(), "Log")?
        }
        Year | Month | Day | Hour | Minute | Second => {
            let (y, mo, d, h, mi, s) = datetime::components(date_of(v0.unwrap())?);
            Value::Int(match f {
                Year => y,
                Month => mo as i64,
                Day => d as i64,
                Hour => h as i64,
                Minute => mi as i64,
                _ => s as i64,
            })
        }
        Weekday => Value::Int(datetime::weekday(date_of(v0.unwrap())?)),
        DateSerial => {
            let (y, m, d) = (int_of(&vals[0])?, int_of(&vals[1])?, int_of(&vals[2])?);
            // Normalise month overflow, then add days (Access semantics).
            let total = y * 12 + (m - 1);
            let (yy, mm) = (total.div_euclid(12), total.rem_euclid(12) + 1);
            Value::DateTime((datetime::days_from_civil(yy, mm as u32, 1) + d - 1) * 86_400)
        }
        DateAdd => {
            let (unit, n, t) = (text_of(&vals[0]).to_ascii_lowercase(), int_of(&vals[1])?, date_of(&vals[2])?);
            let months = |k: i64| -> Result<i64> {
                let (y, m, d, h, mi, s) = datetime::components(t);
                let total = y * 12 + (m as i64 - 1) + k;
                let (ny, nm) = (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32);
                // Clamp the day to the target month's length.
                let mut day = d;
                while datetime::from_components(ny, nm, day, h, mi, s).is_none() && day > 28 {
                    day -= 1;
                }
                datetime::from_components(ny, nm, day, h, mi, s).ok_or_else(|| Error::runtime("DateAdd: out of range"))
            };
            let shifted = match unit.as_str() {
                "yyyy" | "y" => months(n * 12)?,
                "q" => months(n * 3)?,
                "m" => months(n)?,
                "d" | "w" => t + n * 86_400,
                "ww" => t + n * 7 * 86_400,
                "h" => t + n * 3_600,
                "n" => t + n * 60,
                "s" => t + n,
                u => return Err(Error::runtime(format!("DateAdd: unknown interval '{u}'"))),
            };
            // A zoned timestamp stays zoned: the shift happened on its wall clock.
            if matches!(&*vals[2], Value::TimestampTz(_)) {
                Value::TimestampTz(tz::session().to_utc(shifted))
            } else {
                Value::DateTime(shifted)
            }
        }
        DateDiff => {
            let (unit, a, b) = (text_of(&vals[0]).to_ascii_lowercase(), date_of(&vals[1])?, date_of(&vals[2])?);
            let month_index = |t: i64| {
                let (y, m, ..) = datetime::components(t);
                y * 12 + m as i64
            };
            Value::Int(match unit.as_str() {
                "yyyy" | "y" => (month_index(b) - 1).div_euclid(12) - (month_index(a) - 1).div_euclid(12),
                "q" => (month_index(b) - 1).div_euclid(3) - (month_index(a) - 1).div_euclid(3),
                "m" => month_index(b) - month_index(a),
                "d" | "w" => b.div_euclid(86_400) - a.div_euclid(86_400),
                "ww" => (b.div_euclid(86_400) - a.div_euclid(86_400)) / 7,
                "h" => (b - a) / 3_600,
                "n" => (b - a) / 60,
                "s" => b - a,
                u => return Err(Error::runtime(format!("DateDiff: unknown interval '{u}'"))),
            })
        }
        Val => {
            let s = text_of(v0.unwrap());
            let t = s.trim_start();
            let mut end = 0;
            let mut seen_dot = false;
            for (i, c) in t.char_indices() {
                let ok = c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+')) || (c == '.' && !seen_dot);
                if !ok {
                    break;
                }
                seen_dot |= c == '.';
                end = i + c.len_utf8();
            }
            Value::from_decimal(Decimal::parse(&t[..end]).unwrap_or_else(|| Decimal::from_i64(0)))
        }
        CStr => Value::text(&v0.unwrap().to_string()),
        // Conversions to each numeric type.
        Pg(p) => super::pgfuncs::call(p, &vals)?,
        CInt => v0.unwrap().coerce_to(DataType::Integer)?,
        CastInt => truncated(v0.unwrap())?.coerce_to(DataType::Integer)?,
        CastBig => truncated(v0.unwrap())?.coerce_to(DataType::Bignum)?,
        CBig => v0.unwrap().coerce_to(DataType::Bignum)?,
        CSng => v0.unwrap().coerce_to(DataType::Float)?,
        CDbl => v0.unwrap().coerce_to(DataType::Double)?,
        CDec => v0.unwrap().coerce_to(DataType::Decimal)?,
        // The Access function rounds money to four places.
        CCur => match v0.unwrap().coerce_to(DataType::Currency)? {
            Value::Currency(d) => Value::Currency(d.round(4)),
            other => other,
        },
        CRat => v0.unwrap().coerce_to(DataType::Rational)?,
        Rational => {
            let (n, d) = (big_of(&vals[0])?, big_of(&vals[1])?);
            crate::rational::Rational::new(n, d)
                .map(Value::Rational)
                .ok_or_else(|| Error::runtime("Rational: the denominator is zero"))?
        }
        Numerator | Denominator => {
            let r = num_of(v0.unwrap())?
                .exact()
                .ok_or_else(|| Error::type_err("not an exact number"))?;
            Value::Big(if f == Numerator { r.numer().clone() } else { r.denom().clone() })
        }
        TypeName => Value::text(v0.unwrap().type_label()),
        CBool => v0.unwrap().coerce_to(DataType::YesNo)?,
        CDate => v0.unwrap().coerce_to(DataType::DateTime)?,
        NullIf => {
            if vals[0].is_null() || vals[1].is_null() {
                vals[0].clone().into_owned()
            } else if vals[0].sql_cmp(&vals[1]) == Some(std::cmp::Ordering::Equal) {
                Value::Null
            } else {
                vals[0].clone().into_owned()
            }
        }
        Greatest | Least => {
            let mut best = vals[0].clone().into_owned();
            for v in &vals[1..] {
                let ord = v.sql_cmp(&best).ok_or_else(|| Error::type_err("cannot compare these values"))?;
                if (f == Greatest && ord.is_gt()) || (f == Least && ord.is_lt()) {
                    best = v.clone().into_owned();
                }
            }
            best
        }
        Concat => {
            let mut s = String::new();
            for v in &vals {
                if !v.is_null() {
                    s.push_str(&text_of(v));
                }
            }
            Value::text(&s)
        }
        Ceil => {
            let negated = crate::numeric::negate(&*num_of(v0.unwrap())?)?;
            crate::numeric::negate(&whole_part(&negated, true)?)?
        }
        Power => crate::numeric::arith(crate::sql::ast::BinOp::Pow, &vals[0], &vals[1])?,
        Mod => crate::numeric::arith(crate::sql::ast::BinOp::Mod, &vals[0], &vals[1])?,
        Log10 => {
            if !sign_of(v0.unwrap())?.is_gt() {
                return Err(Error::runtime("Log10: invalid argument"));
            }
            float_result(f64_of(v0.unwrap())?.log10(), v0.unwrap(), "Log10")?
        }
        Sin => float_result(f64_of(v0.unwrap())?.sin(), v0.unwrap(), "Sin")?,
        Cos => float_result(f64_of(v0.unwrap())?.cos(), v0.unwrap(), "Cos")?,
        Tan => float_result(f64_of(v0.unwrap())?.tan(), v0.unwrap(), "Tan")?,
        Asin => float_result(f64_of(v0.unwrap())?.asin(), v0.unwrap(), "Asin")?,
        Acos => float_result(f64_of(v0.unwrap())?.acos(), v0.unwrap(), "Acos")?,
        Atan => float_result(f64_of(v0.unwrap())?.atan(), v0.unwrap(), "Atan")?,
        Sinh => float_result(f64_of(v0.unwrap())?.sinh(), v0.unwrap(), "Sinh")?,
        Cosh => float_result(f64_of(v0.unwrap())?.cosh(), v0.unwrap(), "Cosh")?,
        Tanh => float_result(f64_of(v0.unwrap())?.tanh(), v0.unwrap(), "Tanh")?,
        OctetLength => Value::Int(text_of(v0.unwrap()).len() as i64),
        GroupingBit => {
            let mask = int_of(&vals[0])?;
            let n = vals.len() - 1;
            let mut out = 0i64;
            for (k, v) in vals[1..].iter().enumerate() {
                let bit = (mask >> int_of(v)?) & 1;
                out |= bit << (n - 1 - k);
            }
            Value::Int(out)
        }
        TrimOf => {
            let (mode, chars, s) = (text_of(&vals[0]).to_string(), text_of(&vals[1]).to_string(), text_of(&vals[2]).to_string());
            let set: Vec<char> = chars.chars().collect();
            let hit = |c: char| set.contains(&c);
            Value::text(match mode.as_str() {
                "leading" => s.trim_start_matches(hit),
                "trailing" => s.trim_end_matches(hit),
                _ => s.trim_matches(hit),
            })
        }
        DatePart => {
            let unit = text_of(&vals[0]).to_ascii_lowercase();
            if let Value::Interval(iv) = &*vals[1] {
                // The parts of an interval as written: 14 months is 1 year 2 months.
                return Ok(Value::Int(match unit.as_str() {
                    "yyyy" | "year" | "yy" => iv.months / 12,
                    "m" | "month" | "mon" => iv.months % 12,
                    "d" | "day" => iv.days,
                    "h" | "hour" => iv.secs / 3600,
                    "n" | "mi" | "minute" => iv.secs % 3600 / 60,
                    "s" | "second" => iv.secs % 60,
                    "epoch" => i64::try_from(iv.total_secs()).map_err(|_| Error::runtime("interval out of range"))?,
                    u => return Err(Error::runtime(format!("DatePart: '{u}' is not a part of an interval"))),
                }));
            }
            if let Value::TimestampTz(utc) = &*vals[1] {
                let off = tz::session().offset_at(*utc);
                match unit.as_str() {
                    "timezone" => return Ok(Value::Int(off)),
                    "timezone_hour" => return Ok(Value::Int(off / 3600)),
                    "timezone_minute" => return Ok(Value::Int(off % 3600 / 60)),
                    _ => {}
                }
            }
            let t = date_of(&vals[1])?;
            let (y, mo, d, h, mi, s) = datetime::components(t);
            let doy = datetime::days_from_civil(y, mo, d) - datetime::days_from_civil(y, 1, 1) + 1;
            let jan1 = datetime::weekday(datetime::days_from_civil(y, 1, 1) * 86_400) - 1;
            Value::Int(match unit.as_str() {
                "yyyy" | "year" | "yy" => y,
                "q" | "quarter" => ((mo - 1) / 3 + 1) as i64,
                "m" | "month" => mo as i64,
                "y" | "dayofyear" | "doy" => doy,
                "d" | "day" => d as i64,
                "w" | "weekday" | "dow" => datetime::weekday(t) - 1 + i64::from(unit == "w" || unit == "weekday"),
                "ww" | "week" => (doy - 1 + jan1) / 7 + 1,
                "h" | "hour" => h as i64,
                "n" | "mi" | "minute" => mi as i64,
                "s" | "second" => s as i64,
                "epoch" => t,
                u => return Err(Error::runtime(format!("DatePart: unknown field '{u}'"))),
            })
        }
        AtTimeZone | TimeZone => {
            let (value, zone) = if f == AtTimeZone { (&vals[0], &vals[1]) } else { (&vals[1], &vals[0]) };
            let zone = match &**zone {
                Value::Interval(iv) if iv.months == 0 => tz::Zone::Fixed(i32::try_from(iv.days * 86_400 + iv.secs).map_err(|_| Error::runtime("time zone offset out of range"))?),
                other => {
                    let name = text_of(other);
                    tz::Zone::parse(&name).ok_or_else(|| Error::runtime(format!("unknown time zone '{name}'")))?
                }
            };
            match value.coerce_to(DataType::TimestampTz).ok().filter(|_| matches!(&**value, Value::TimestampTz(_))) {
                // An instant becomes the wall clock of the zone...
                Some(Value::TimestampTz(utc)) => Value::DateTime(zone.to_local(utc)),
                // ...and a wall clock is read as being in the zone.
                _ => match &**value {
                    Value::DateTime(local) => Value::TimestampTz(zone.to_utc(*local)),
                    Value::TimestampTz(utc) => Value::DateTime(zone.to_local(*utc)),
                    other => Value::TimestampTz(zone.to_utc(date_of(other)?)),
                },
            }
        }
        CTimestampTz => v0.unwrap().coerce_to(DataType::TimestampTz)?,
        CInterval => v0.unwrap().coerce_to(DataType::Interval)?,
        MakeInterval => {
            let part = |i: usize| -> Result<Decimal> {
                match vals.get(i) {
                    Some(v) => v.as_decimal().ok_or_else(|| Error::type_err(format!("'{v}' is not a number"))),
                    None => Ok(Decimal::from_i64(0)),
                }
            };
            let total = [(12, 0), (1, 0), (0, 1)];
            let _ = total;
            let (years, months, days, hours, mins, secs) = (part(0)?, part(1)?, part(2)?, part(3)?, part(4)?, part(5)?);
            let months_total = years.mul(&Decimal::from_i64(12)).add(&months);
            let hms = hours.mul(&Decimal::from_i64(3600)).add(&mins.mul(&Decimal::from_i64(60))).add(&secs);
            let iv = crate::interval::Interval::new(0, 1, 0)
                .scale(&days)
                .zip(crate::interval::Interval::new(1, 0, 0).scale(&months_total))
                .zip(crate::interval::Interval::new(0, 0, 1).scale(&hms))
                .and_then(|((d, m), s)| d.add(&m)?.add(&s))
                .ok_or_else(|| Error::runtime("interval out of range"))?;
            Value::Interval(iv)
        }
        JsonValid => Value::Bool(match &*vals[0] {
            Value::Json(_) => true,
            Value::Text(s) => json::canonical(s).is_some(),
            _ => false,
        }),
        JsonParse => vals[0].coerce_to(crate::value::DataType::Json)?,
        ToJson => json::json_value(&json::from_value(&vals[0])),
        JsonExtract | JsonValue => {
            let doc = json::document(&vals[0])?;
            let steps = json::path_steps(&text_of(&vals[1]))?;
            match json::get(&doc, &steps) {
                None => Value::Null,
                Some(found) if f == JsonValue => match found {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => Value::Null,
                    scalar => json::to_value(scalar),
                },
                Some(found) => json::json_value(found),
            }
        }
        JsonGet | JsonGetText => {
            let doc = json::document(&vals[0])?;
            let step = match &*vals[1] {
                Value::Text(k) => json::Step::Key(k.to_string()),
                other => json::Step::Index(usize::try_from(int_of(other)?).map_err(|_| Error::runtime("a JSON position cannot be negative"))?),
            };
            match json::get(&doc, &[step]) {
                None => Value::Null,
                Some(found) if f == JsonGet => json::json_value(found),
                Some(serde_json::Value::Null) => Value::Null,
                Some(serde_json::Value::String(s)) => Value::text(s),
                Some(other) => Value::text(&json::to_text(other)),
            }
        }
        JsonArray => json::json_value(&serde_json::Value::Array(vals.iter().map(|v| json::from_value(v)).collect())),
        JsonObject => {
            if vals.len() % 2 != 0 {
                return Err(Error::runtime("JSON_OBJECT takes pairs of a key and a value"));
            }
            let mut m = serde_json::Map::new();
            for pair in vals.chunks(2) {
                if pair[0].is_null() {
                    return Err(Error::runtime("a JSON object key cannot be NULL"));
                }
                m.insert(text_of(&pair[0]).into_owned(), json::from_value(&pair[1]));
            }
            json::json_value(&serde_json::Value::Object(m))
        }
        JsonLength | JsonType | JsonKeys => {
            let doc = json::document(&vals[0])?;
            let target = match vals.get(1) {
                Some(p) => json::get(&doc, &json::path_steps(&text_of(p))?),
                None => Some(&doc),
            };
            match (f, target) {
                (_, None) => Value::Null,
                (JsonLength, Some(serde_json::Value::Array(a))) => Value::Int(a.len() as i64),
                (JsonLength, Some(serde_json::Value::Object(m))) => Value::Int(m.len() as i64),
                (JsonLength, Some(_)) => Value::Int(1),
                (JsonType, Some(t)) => Value::text(json::type_name(t)),
                (JsonKeys, Some(serde_json::Value::Object(m))) => {
                    json::json_value(&serde_json::Value::Array(m.keys().map(|k| serde_json::Value::String(k.clone())).collect()))
                }
                _ => Value::Null,
            }
        }
        JsonSet => {
            if vals[0].is_null() || vals[1].is_null() {
                return Ok(Value::Null);
            }
            let mut doc = json::document(&vals[0])?;
            json::set(&mut doc, &json::path_steps(&text_of(&vals[1]))?, json::from_value(&vals[2]))?;
            json::json_value(&doc)
        }
        JsonRemove => {
            let mut doc = json::document(&vals[0])?;
            json::remove(&mut doc, &json::path_steps(&text_of(&vals[1]))?);
            json::json_value(&doc)
        }
        JsonMergePatch => json::json_value(&json::merge_patch(json::document(&vals[0])?, json::document(&vals[1])?)),
        JsonContains => {
            let candidate = match &*vals[1] {
                Value::Text(s) => json::canonical(s).map_or_else(|| serde_json::Value::String(s.to_string()), |_| json::document(&vals[1]).unwrap_or(serde_json::Value::Null)),
                other => json::from_value(other),
            };
            Value::Bool(json::contains(&json::document(&vals[0])?, &candidate))
        }
        Lpad | Rpad => {
            let fill = vals.get(2).map(|f| text_of(f).into_owned()).unwrap_or_else(|| " ".to_string());
            Value::text(&extra::pad(&text_of(&vals[0]), int_of(&vals[1])?, &fill, f == Lpad)?)
        }
        Repeat => Value::text(&extra::repeat(&text_of(&vals[0]), int_of(&vals[1])?)?),
        Reverse => Value::text(&text_of(&vals[0]).chars().rev().collect::<String>()),
        Overlay => {
            let len = match vals.get(3) {
                Some(l) => Some(int_of(l)?),
                None => None,
            };
            Value::text(&extra::overlay(&text_of(&vals[0]), &text_of(&vals[1]), int_of(&vals[2])?, len))
        }
        Translate => Value::text(&extra::translate(&text_of(&vals[0]), &text_of(&vals[1]), &text_of(&vals[2]))),
        Initcap => Value::text(&extra::initcap(&text_of(&vals[0]))),
        RegexpLike | RegexpCount => {
            let re = extra::pattern(&text_of(&vals[1]), &vals.get(2).map(|f| text_of(f).into_owned()).unwrap_or_default())?;
            let text = text_of(&vals[0]);
            if f == RegexpLike {
                Value::Bool(re.is_match(&text))
            } else {
                Value::Int(re.find_iter(&text).count() as i64)
            }
        }
        SimilarTo => {
            let re = extra::pattern(&extra::similar_to_regex(&text_of(&vals[1])), "")?;
            Value::Bool(re.is_match(&text_of(&vals[0])))
        }
        RegexpReplace => {
            let re = extra::pattern(&text_of(&vals[1]), &vals.get(3).map(|f| text_of(f).into_owned()).unwrap_or_default())?;
            let with = extra::replacement(&text_of(&vals[2]));
            Value::text(&re.replace_all(&text_of(&vals[0]), with.as_str()))
        }
        RegexpSubstr => {
            let re = extra::pattern(&text_of(&vals[1]), &vals.get(3).map(|f| text_of(f).into_owned()).unwrap_or_default())?;
            let group = match vals.get(2) {
                Some(g) => int_of(g)?.max(0) as usize,
                None => 0,
            };
            match re.captures(&text_of(&vals[0])).and_then(|c| c.get(group).map(|m| m.as_str().to_string())) {
                Some(m) => Value::text(&m),
                None => Value::Null,
            }
        }
        Atan2 => float_result(f64_of(&vals[0])?.atan2(f64_of(&vals[1])?), &vals[0], "Atan2")?,
        Degrees => float_result(f64_of(&vals[0])?.to_degrees(), &vals[0], "Degrees")?,
        Radians => float_result(f64_of(&vals[0])?.to_radians(), &vals[0], "Radians")?,
        Pi => Value::Double(std::f64::consts::PI),
        Random => Value::Double(extra::random()),
        Trunc => {
            let digits = match vals.get(1) {
                Some(d) => int_of(d)?,
                None => 0,
            };
            if !(-10_000..=10_000).contains(&digits) {
                return Err(Error::runtime("Trunc: digits must be between -10000 and 10000"));
            }
            trunc_of(&vals[0], digits)?
        }
        Nextval => Value::Int(crate::sequence::next_value(&text_of(&vals[0]))?),
        Currval => Value::Int(crate::sequence::current_value(&text_of(&vals[0]))?),
        Lastval => Value::Int(crate::sequence::last_value()?),
        Setval => {
            let called = match vals.get(2) {
                Some(v) => match &**v {
                    Value::Bool(b) => *b,
                    _ => return Err(Error::type_err("SETVAL: the third argument must be TRUE or FALSE")),
                },
                None => true,
            };
            Value::Int(crate::sequence::set_value(&text_of(&vals[0]), int_of(&vals[1])?, called)?)
        }
        Age => Value::Interval(crate::interval::Interval::age(date_of(&vals[0])?, date_of(&vals[1])?)),
        Format => {
            let pattern = vals.get(1).map(|p| text_of(p).into_owned()).unwrap_or_default();
            Value::text(&super::format::format_value(v0.unwrap(), &pattern)?)
        }
        Iif | Nz | IsNull | Coalesce | Now | Date | NowTz | LocalNow | LocalDate | SessionTimeZone => unreachable!("handled above"),
    })
}

/// A number with a fraction (a `FLOAT`, `DOUBLE`, `DECIMAL`, `CURRENCY` or `RATIONAL`) with the fraction
/// dropped, toward zero; anything else as it is.
fn truncated(v: &Value) -> Result<Value> {
    match v {
        Value::Float(f) if !f.is_finite() => Ok(v.clone()),
        Value::Double(f) if !f.is_finite() => Ok(v.clone()),
        Value::Float(_) | Value::Double(_) | Value::Dec(_) | Value::Currency(_) | Value::Rational(_) => whole_part(v, false),
        other => Ok(other.clone()),
    }
}
