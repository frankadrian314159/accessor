//! Built-in scalar functions (the commonly used subset of Access's library).

use std::borrow::Cow;

use super::BExpr;
use crate::datetime;
use crate::error::{Error, Result};
use crate::decimal::Decimal;
use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive};

use crate::rational::Rational;
use crate::value::{num_cmp, DataType, Value};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScalarFn {
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
}

impl ScalarFn {
    /// Resolves a function name to `(function, min_args, max_args)`.
    pub fn lookup(name: &str) -> Option<(ScalarFn, usize, usize)> {
        use ScalarFn::*;
        let up = name.to_ascii_uppercase();
        Some(match up.as_str() {
            "IIF" => (Iif, 3, 3),
            "NZ" => (Nz, 1, 2),
            "ISNULL" => (IsNull, 1, 1),
            "COALESCE" => (Coalesce, 1, usize::MAX),
            "UCASE" | "UPPER" => (UCase, 1, 1),
            "LCASE" | "LOWER" => (LCase, 1, 1),
            "LEN" | "LENGTH" => (Len, 1, 1),
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
            _ => return None,
        })
    }

    /// Functions that tolerate (and may transform) `Null` arguments; all
    /// others yield `Null` as soon as any argument is `Null`.
    fn handles_null(self) -> bool {
        matches!(self, ScalarFn::Nz | ScalarFn::IsNull | ScalarFn::Coalesce | ScalarFn::Iif)
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
        _ => {}
    }

    let vals: Vec<Cow<'_, Value>> = args.iter().map(|a| a.eval(row)).collect::<Result<_>>()?;
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
            Value::DateTime(match unit.as_str() {
                "yyyy" | "y" => months(n * 12)?,
                "q" => months(n * 3)?,
                "m" => months(n)?,
                "d" | "w" => t + n * 86_400,
                "ww" => t + n * 7 * 86_400,
                "h" => t + n * 3_600,
                "n" => t + n * 60,
                "s" => t + n,
                u => return Err(Error::runtime(format!("DateAdd: unknown interval '{u}'"))),
            })
        }
        DateDiff => {
            let (unit, a, b) = (text_of(&vals[0]).to_ascii_lowercase(), date_of(&vals[1])?, date_of(&vals[2])?);
            let month_index = |t: i64| {
                let (y, m, ..) = datetime::components(t);
                y * 12 + m as i64
            };
            Value::Int(match unit.as_str() {
                "yyyy" | "y" => month_index(b).div_euclid(12) - month_index(a).div_euclid(12),
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
        CInt => v0.unwrap().coerce_to(DataType::Integer)?,
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
        Iif | Nz | IsNull | Coalesce | Now | Date => unreachable!("handled above"),
    })
}
