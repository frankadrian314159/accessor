//! Arithmetic across the numeric types.
//!
//! ## How types combine
//!
//! The type of a result depends only on the types of the operands, never on
//! their values.
//!
//! * **Floats are contagious.** If either operand is a `DOUBLE` the result is a
//!   `DOUBLE`; otherwise if either is a `FLOAT` it is a `FLOAT` (the other
//!   operand is converted to the float type first). Float results must be
//!   finite: overflow, division by zero and the like are errors, never
//!   infinities or NaNs.
//! * **Among exact types**, `+`, `-` and `*` give the first that applies:
//!   `RATIONAL` if either operand is one, else `CURRENCY`, else `DECIMAL`, else
//!   `BIGNUM`, else `INTEGER`. An `INTEGER` result that does not fit in 64 bits
//!   becomes a `BIGNUM` instead of overflowing.
//! * **`/`** is exact: `RATIONAL` if either operand is, `CURRENCY` for money
//!   divided by a plain number, `DECIMAL` otherwise (including money divided by
//!   money, and integers divided by integers: `7 / 2` is the decimal `3.5`, and
//!   `1 / 3` is a decimal rounded at 50 digits; use `RATIONAL` for exact thirds).
//! * **`\` and `Mod`** round their operands to whole numbers and give an
//!   `INTEGER`, or a `BIGNUM` if an operand is one or the result does not fit.
//! * **`^`** with a whole exponent is exact in the base's own type (a negative
//!   exponent turns integers into decimals); any other exponent is computed in
//!   floating point and gives a `DOUBLE`.

use std::borrow::Cow;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

use crate::decimal::Decimal;
use crate::error::{Error, Result};
use crate::rational::Rational;
use crate::sql::ast::BinOp;
use crate::value::{Kind, Value};

fn overflow() -> Error {
    Error::runtime("Overflow")
}

fn division_by_zero() -> Error {
    Error::runtime("Division by zero")
}

fn double(f: f64) -> Result<Value> {
    Value::from_f64(f).ok_or_else(overflow)
}

fn float(f: f32) -> Result<Value> {
    Value::from_f32(f).ok_or_else(overflow)
}

/// An integer result: `INTEGER` if it fits, else `BIGNUM`.
fn int_or_big(b: BigInt) -> Value {
    b.to_i64().map_or_else(|| Value::Big(b), Value::Int)
}

/// The type of `x op y` for `+`, `-`, `*`.
fn join(a: Kind, b: Kind) -> Kind {
    use Kind::*;
    match (a, b) {
        (Double, _) | (_, Double) => Double,
        (Float, _) | (_, Float) => Float,
        (Rat, _) | (_, Rat) => Rat,
        (Cur, _) | (_, Cur) => Cur,
        (Dec, _) | (_, Dec) => Dec,
        (Big, _) | (_, Big) => Big,
        (Int, Int) => Int,
    }
}

/// The type of `x / y`.
fn divide_kind(a: Kind, b: Kind) -> Kind {
    use Kind::*;
    match (a, b) {
        (Double, _) | (_, Double) => Double,
        (Float, _) | (_, Float) => Float,
        (Rat, _) | (_, Rat) => Rat,
        (Cur, Cur) => Dec,
        (Cur, _) => Cur,
        _ => Dec,
    }
}

fn require_f64(v: &Value) -> Result<f64> {
    v.as_f64().ok_or_else(overflow)
}

fn require_f32(v: &Value) -> Result<f32> {
    v.as_f32().ok_or_else(overflow)
}

fn require_decimal(v: &Value) -> Result<Decimal> {
    v.as_decimal().ok_or_else(overflow)
}

fn require_rational(v: &Value) -> Result<Rational> {
    v.exact().ok_or_else(overflow)
}

fn require_big(v: &Value) -> Result<BigInt> {
    match v {
        Value::Int(i) => Ok(BigInt::from(*i)),
        Value::Big(b) => Ok(b.clone()),
        _ => Err(overflow()),
    }
}

fn wrap_decimal(kind: Kind, d: Decimal) -> Value {
    if kind == Kind::Cur {
        Value::Currency(d)
    } else {
        Value::Dec(d)
    }
}

pub fn negate(v: &Value) -> Result<Value> {
    Ok(match v {
        Value::Null => Value::Null,
        Value::Int(i) => i.checked_neg().map_or_else(|| Value::Big(-BigInt::from(*i)), Value::Int),
        Value::Big(b) => Value::Big(-b),
        Value::Float(f) => Value::Float(-f),
        Value::Double(f) => Value::Double(-f),
        Value::Dec(d) => Value::Dec(d.neg()),
        Value::Currency(d) => Value::Currency(d.neg()),
        Value::Rational(r) => Value::Rational(r.neg()),
        Value::Bool(b) => Value::Bool(!b),
        Value::Interval(i) => Value::Interval(i.neg().ok_or_else(overflow)?),
        other => match other.num_lenient() {
            Some(n) => negate(&n)?,
            None => return Err(Error::type_err(format!("cannot negate '{other}'"))),
        },
    })
}

/// `+`, `-` and `*` on two numeric values.
fn add_sub_mul(op: BinOp, x: &Value, y: &Value) -> Result<Value> {
    let (kx, ky) = (x.kind().ok_or_else(overflow)?, y.kind().ok_or_else(overflow)?);
    Ok(match join(kx, ky) {
        Kind::Double => {
            let (p, q) = (require_f64(x)?, require_f64(y)?);
            double(match op {
                BinOp::Add => p + q,
                BinOp::Sub => p - q,
                _ => p * q,
            })?
        }
        Kind::Float => {
            let (p, q) = (require_f32(x)?, require_f32(y)?);
            float(match op {
                BinOp::Add => p + q,
                BinOp::Sub => p - q,
                _ => p * q,
            })?
        }
        Kind::Rat => {
            let (p, q) = (require_rational(x)?, require_rational(y)?);
            Value::Rational(match op {
                BinOp::Add => p.add(&q),
                BinOp::Sub => p.sub(&q),
                _ => p.mul(&q),
            })
        }
        kind @ (Kind::Cur | Kind::Dec) => {
            let (p, q) = (require_decimal(x)?, require_decimal(y)?);
            wrap_decimal(
                kind,
                match op {
                    BinOp::Add => p.add(&q),
                    BinOp::Sub => p.sub(&q),
                    _ => p.mul(&q),
                },
            )
        }
        Kind::Big => {
            let (p, q) = (require_big(x)?, require_big(y)?);
            Value::Big(match op {
                BinOp::Add => p + q,
                BinOp::Sub => p - q,
                _ => p * q,
            })
        }
        Kind::Int => {
            let (Value::Int(p), Value::Int(q)) = (x, y) else { return Err(overflow()) };
            let r = match op {
                BinOp::Add => p.checked_add(*q),
                BinOp::Sub => p.checked_sub(*q),
                _ => p.checked_mul(*q),
            };
            match r {
                Some(v) => Value::Int(v),
                // Too big for 64 bits: carry on exactly as a BIGNUM.
                None => {
                    let (p, q) = (BigInt::from(*p), BigInt::from(*q));
                    Value::Big(match op {
                        BinOp::Add => p + q,
                        BinOp::Sub => p - q,
                        _ => p * q,
                    })
                }
            }
        }
    })
}

fn divide(x: &Value, y: &Value) -> Result<Value> {
    if y.is_zero() {
        return Err(division_by_zero());
    }
    let (kx, ky) = (x.kind().ok_or_else(overflow)?, y.kind().ok_or_else(overflow)?);
    Ok(match divide_kind(kx, ky) {
        Kind::Double => double(require_f64(x)? / require_f64(y)?)?,
        Kind::Float => float(require_f32(x)? / require_f32(y)?)?,
        Kind::Rat => Value::Rational(
            require_rational(x)?.div(&require_rational(y)?).ok_or_else(division_by_zero)?,
        ),
        kind => {
            // Whole quotients of 64-bit integers skip the big-number machinery.
            if let (Value::Int(p), Value::Int(q)) = (x, y) {
                if *q != -1 && p % q == 0 {
                    return Ok(Value::Dec(Decimal::from_i64(p / q)));
                }
            }
            let quotient = require_decimal(x)?.div(&require_decimal(y)?).ok_or_else(division_by_zero)?;
            wrap_decimal(kind, quotient)
        }
    })
}

/// `\` and `Mod`: whole-number division after rounding both operands.
fn int_div_mod(op: BinOp, x: &Value, y: &Value) -> Result<Value> {
    let (p, q) = (x.rounded_int().ok_or_else(overflow)?, y.rounded_int().ok_or_else(overflow)?);
    if q.is_zero() {
        return Err(division_by_zero());
    }
    let r = if op == BinOp::IntDiv { &p / &q } else { &p % &q };
    let big_operand = matches!(x, Value::Big(_)) || matches!(y, Value::Big(_));
    Ok(if big_operand { Value::Big(r) } else { int_or_big(r) })
}

/// `x ^ y`.
fn power(x: &Value, y: &Value) -> Result<Value> {
    let (kx, ky) = (x.kind().ok_or_else(overflow)?, y.kind().ok_or_else(overflow)?);
    let inexact = |k: Kind| matches!(k, Kind::Float | Kind::Double);
    if inexact(kx) || inexact(ky) {
        return if kx == Kind::Double || ky == Kind::Double {
            double(require_f64(x)?.powf(require_f64(y)?))
        } else {
            float(require_f32(x)?.powf(require_f32(y)?))
        };
    }
    // An exact base and a whole exponent of reasonable size: exact result.
    let whole_exponent = y
        .exact()
        .filter(Rational::is_integer)
        .and_then(|r| r.numer().to_i64())
        .filter(|e| e.abs() <= 10_000);
    let Some(e) = whole_exponent else {
        return double(require_f64(x)?.powf(require_f64(y)?));
    };
    let cap = |v: Option<Value>| v.ok_or_else(overflow);
    match x {
        Value::Rational(r) => {
            if e < 0 && r.is_zero() {
                return Err(division_by_zero());
            }
            cap(r.powi(e).map(Value::Rational))
        }
        Value::Dec(_) | Value::Currency(_) | Value::Int(_) | Value::Big(_) if e >= 0 => {
            let n = u32::try_from(e).map_err(|_| overflow())?;
            match x {
                Value::Int(i) => {
                    if let Some(v) = i.checked_pow(n) {
                        return Ok(Value::Int(v));
                    }
                    let b = BigInt::from(*i);
                    cap((b.bits().saturating_mul(n as u64) <= 4_000_000).then(|| Value::Big(b.pow(n))))
                }
                Value::Big(b) => cap((b.bits().saturating_mul(n as u64) <= 4_000_000).then(|| Value::Big(b.pow(n)))),
                Value::Dec(d) => cap(d.powi(n).map(Value::Dec)),
                Value::Currency(d) => cap(d.powi(n).map(Value::Currency)),
                _ => unreachable!(),
            }
        }
        // A negative whole exponent divides one by the positive power: a decimal.
        Value::Dec(_) | Value::Currency(_) | Value::Int(_) | Value::Big(_) => {
            let positive = power(x, &Value::Int(-e))?;
            if positive.is_zero() {
                return Err(division_by_zero());
            }
            let one = Value::Dec(Decimal::from_i64(1));
            let quotient = divide(&one, &positive)?;
            Ok(match x {
                Value::Currency(_) => Value::Currency(require_decimal(&quotient)?),
                _ => quotient,
            })
        }
        _ => Err(overflow()),
    }
}

/// A number of days as whole seconds.
fn day_seconds(n: &Value) -> Result<i64> {
    match n {
        Value::Float(_) | Value::Double(_) => {
            let secs = (require_f64(n)? * 86_400.0).round_ties_even();
            if secs.abs() < 9.2e18 {
                Ok(secs as i64)
            } else {
                Err(overflow())
            }
        }
        _ => {
            let r = require_rational(n)?.mul(&Rational::from_i64(86_400));
            r.round().to_i64().ok_or_else(overflow)
        }
    }
}

/// `a op b` where one side is an interval or a `TIMESTAMPTZ`; `None` for anything else.
fn interval_arith(op: BinOp, a: &Value, b: &Value) -> Result<Option<Value>> {
    use crate::interval::Interval;
    use crate::tz;
    let out_of_range = || Error::runtime("interval out of range");
    // A zoned instant moved by months and days on its wall clock, and by seconds exactly.
    let move_tz = |t: i64, iv: &Interval, sign: i64| -> Result<i64> {
        let zone = tz::session();
        let calendar = Interval::new(iv.months, iv.days, 0);
        let local = calendar.add_to_local(zone.to_local(t), sign).ok_or_else(out_of_range)?;
        zone.to_utc(local).checked_add(iv.secs.checked_mul(sign).ok_or_else(out_of_range)?).ok_or_else(out_of_range)
    };
    let scale = |iv: &Interval, k: &Decimal| iv.scale(k).map(Value::Interval).ok_or_else(out_of_range);
    Ok(Some(match (op, a, b) {
        (BinOp::Add, Value::Interval(x), Value::Interval(y)) => Value::Interval(x.add(y).ok_or_else(out_of_range)?),
        (BinOp::Sub, Value::Interval(x), Value::Interval(y)) => Value::Interval(x.sub(y).ok_or_else(out_of_range)?),
        (BinOp::Mul, Value::Interval(x), n) | (BinOp::Mul, n, Value::Interval(x)) if n.num().is_some() => {
            scale(x, &n.num().and_then(|n| n.as_decimal()).ok_or_else(overflow)?)?
        }
        (BinOp::Div, Value::Interval(x), n) if n.num().is_some() => {
            let d = n.num().and_then(|n| n.as_decimal()).ok_or_else(overflow)?;
            let inverse = Decimal::from_i64(1).div(&d).ok_or_else(division_by_zero)?;
            scale(x, &inverse)?
        }
        (BinOp::Add, Value::DateTime(t), Value::Interval(i)) | (BinOp::Add, Value::Interval(i), Value::DateTime(t)) => {
            Value::DateTime(i.add_to_local(*t, 1).ok_or_else(out_of_range)?)
        }
        (BinOp::Sub, Value::DateTime(t), Value::Interval(i)) => Value::DateTime(i.add_to_local(*t, -1).ok_or_else(out_of_range)?),
        (BinOp::Add, Value::TimestampTz(t), Value::Interval(i)) | (BinOp::Add, Value::Interval(i), Value::TimestampTz(t)) => {
            Value::TimestampTz(move_tz(*t, i, 1)?)
        }
        (BinOp::Sub, Value::TimestampTz(t), Value::Interval(i)) => Value::TimestampTz(move_tz(*t, i, -1)?),
        // The difference of two instants is an elapsed time.
        (BinOp::Sub, Value::TimestampTz(t), Value::TimestampTz(u))
        | (BinOp::Sub, Value::TimestampTz(t), Value::DateTime(u))
        | (BinOp::Sub, Value::DateTime(t), Value::TimestampTz(u)) => Value::Interval(Interval::new(0, 0, t.checked_sub(*u).ok_or_else(out_of_range)?)),
        (BinOp::Add, Value::TimestampTz(t), n) | (BinOp::Add, n, Value::TimestampTz(t)) if n.num().is_some() => {
            let days = n.num().ok_or_else(overflow)?.into_owned();
            Value::TimestampTz(t + day_seconds(&days)?)
        }
        (BinOp::Sub, Value::TimestampTz(t), n) if n.num().is_some() => {
            let days = n.num().ok_or_else(overflow)?.into_owned();
            Value::TimestampTz(t - day_seconds(&days)?)
        }
        _ => return Ok(None),
    }))
}

fn operand(v: &Value) -> Result<Cow<'_, Value>> {
    v.num_lenient().ok_or_else(|| Error::type_err(format!("'{v}' is not a number")))
}

/// Binary arithmetic. Operands may be numbers, numeric text, Yes/No or dates.
pub fn arith(op: BinOp, a: &Value, b: &Value) -> Result<Value> {
    if a.is_null() || b.is_null() {
        return Ok(Value::Null);
    }
    // Intervals and zoned timestamps.
    if let Some(v) = interval_arith(op, a, b)? {
        return Ok(v);
    }
    // Date arithmetic: dates are counted in days.
    match (op, a, b) {
        (BinOp::Sub, Value::DateTime(t), Value::DateTime(u)) => {
            let days = Decimal::from_i64(t - u).div(&Decimal::from_i64(86_400)).ok_or_else(division_by_zero)?;
            return Ok(Value::from_decimal(days));
        }
        (BinOp::Add, Value::DateTime(t), n) | (BinOp::Add, n, Value::DateTime(t)) => {
            if let Some(n) = n.num() {
                return Ok(Value::DateTime(t + day_seconds(&n)?));
            }
        }
        (BinOp::Sub, Value::DateTime(t), n) => {
            if let Some(n) = n.num() {
                return Ok(Value::DateTime(t - day_seconds(&n)?));
            }
        }
        (BinOp::Add, Value::Text(x), Value::Text(y)) => {
            let mut s = String::with_capacity(x.len() + y.len());
            s.push_str(x);
            s.push_str(y);
            return Ok(Value::text(&s));
        }
        _ => {}
    }
    let (x, y) = (operand(a)?, operand(b)?);
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul => add_sub_mul(op, &x, &y),
        BinOp::Div => divide(&x, &y),
        BinOp::IntDiv | BinOp::Mod => int_div_mod(op, &x, &y),
        BinOp::Pow => power(&x, &y),
        _ => unreachable!("not an arithmetic operator"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use BinOp::*;

    fn dec(s: &str) -> Value {
        Value::Dec(Decimal::parse(s).unwrap())
    }
    fn cur(s: &str) -> Value {
        Value::Currency(Decimal::parse(s).unwrap())
    }
    fn rat(n: i64, d: i64) -> Value {
        Value::Rational(Rational::new(BigInt::from(n), BigInt::from(d)).unwrap())
    }
    fn big(s: &str) -> Value {
        Value::Big(s.parse().unwrap())
    }
    fn op(o: BinOp, a: &Value, b: &Value) -> Value {
        arith(o, a, b).unwrap()
    }
    /// The type name and value of a result.
    fn is(v: Value, ty: &str, shown: &str) {
        assert_eq!((v.type_label(), v.to_string()), (ty, shown.to_string()));
    }

    /// One of every type, for the table-driven checks below.
    fn one_of_each() -> Vec<(&'static str, Value)> {
        vec![
            ("INTEGER", Value::Int(6)),
            ("BIGNUM", big("6")),
            ("FLOAT", Value::Float(6.0)),
            ("DOUBLE", Value::Double(6.0)),
            ("DECIMAL", dec("6")),
            ("CURRENCY", cur("6")),
            ("RATIONAL", rat(6, 1)),
        ]
    }

    #[test]
    fn result_types_of_add_sub_mul() {
        use std::collections::HashMap;
        // (left, right) -> result type, for every pair (the table is symmetric).
        let order = ["INTEGER", "BIGNUM", "FLOAT", "DOUBLE", "DECIMAL", "CURRENCY", "RATIONAL"];
        let expect = |a: &str, b: &str| -> &'static str {
            let has = |t: &str| a == t || b == t;
            if has("DOUBLE") {
                "DOUBLE"
            } else if has("FLOAT") {
                "FLOAT"
            } else if has("RATIONAL") {
                "RATIONAL"
            } else if has("CURRENCY") {
                "CURRENCY"
            } else if has("DECIMAL") {
                "DECIMAL"
            } else if has("BIGNUM") {
                "BIGNUM"
            } else {
                "INTEGER"
            }
        };
        let values: HashMap<_, _> = one_of_each().into_iter().collect();
        for a in order {
            for b in order {
                for o in [Add, Sub, Mul] {
                    let r = op(o, &values[a], &values[b]);
                    assert_eq!(r.type_label(), expect(a, b), "{a} {o:?} {b}");
                }
            }
        }
        // And the values are right whichever types are mixed (6 + 6, 6 - 6, 6 * 6).
        for (_, a) in one_of_each() {
            for (_, b) in one_of_each() {
                assert_eq!(op(Add, &a, &b), Value::Int(12));
                assert_eq!(op(Sub, &a, &b), Value::Int(0));
                assert_eq!(op(Mul, &a, &b), Value::Int(36));
            }
        }
    }

    #[test]
    fn integers_promote_instead_of_overflowing() {
        is(op(Add, &Value::Int(1), &Value::Int(2)), "INTEGER", "3");
        is(op(Add, &Value::Int(i64::MAX), &Value::Int(1)), "BIGNUM", "9223372036854775808");
        is(op(Sub, &Value::Int(i64::MIN), &Value::Int(1)), "BIGNUM", "-9223372036854775809");
        is(op(Mul, &Value::Int(i64::MAX), &Value::Int(i64::MAX)), "BIGNUM", "85070591730234615847396907784232501249");
        // Bignums stay bignums, even when the result is small again.
        let b = op(Add, &Value::Int(i64::MAX), &Value::Int(1));
        is(op(Sub, &b, &Value::Int(2)), "BIGNUM", "9223372036854775806");
        is(negate(&Value::Int(i64::MIN)).unwrap(), "BIGNUM", "9223372036854775808");
    }

    #[test]
    fn decimals_and_currency_are_exact() {
        is(op(Add, &dec("0.1"), &dec("0.2")), "DECIMAL", "0.3");
        is(op(Mul, &dec("1.1"), &dec("1.1")), "DECIMAL", "1.21");
        is(op(Mul, &cur("19.99"), &Value::Int(3)), "CURRENCY", "$59.97");
        is(op(Mul, &cur("100"), &dec("1.0825")), "CURRENCY", "$108.25");
        is(op(Add, &cur("0.1"), &cur("0.2")), "CURRENCY", "$0.30");
        is(op(Sub, &Value::Int(5), &cur("7.25")), "CURRENCY", "-$2.25");
        is(op(Add, &dec("1.5"), &big("99999999999999999999")), "DECIMAL", "100000000000000000000.5");
    }

    #[test]
    fn rationals_are_exact() {
        is(op(Add, &rat(1, 3), &rat(1, 6)), "RATIONAL", "1/2");
        is(op(Mul, &rat(2, 3), &Value::Int(3)), "RATIONAL", "2");
        is(op(Add, &rat(1, 3), &dec("0.5")), "RATIONAL", "5/6");
        is(op(Add, &rat(1, 3), &cur("0.5")), "RATIONAL", "5/6");
        is(op(Sub, &rat(1, 2), &big("1")), "RATIONAL", "-1/2");
        // Three thirds really are one.
        let third = rat(1, 3);
        is(op(Add, &op(Add, &third, &third), &third), "RATIONAL", "1");
    }

    #[test]
    fn floats_are_contagious() {
        is(op(Add, &Value::Float(0.5), &Value::Int(1)), "FLOAT", "1.5");
        is(op(Add, &Value::Float(0.5), &dec("1.25")), "FLOAT", "1.75");
        is(op(Mul, &Value::Float(0.5), &rat(1, 4)), "FLOAT", "0.125");
        is(op(Add, &Value::Float(0.5), &Value::Double(0.25)), "DOUBLE", "0.75");
        is(op(Add, &Value::Double(0.5), &Value::Int(1)), "DOUBLE", "1.5");
        is(op(Add, &Value::Double(0.5), &cur("1")), "DOUBLE", "1.5");
        // Float arithmetic is float arithmetic.
        is(op(Add, &Value::Double(0.1), &Value::Double(0.2)), "DOUBLE", "0.30000000000000004");
        is(op(Add, &Value::Float(0.1), &Value::Float(0.2)), "FLOAT", "0.3");
        let f = op(Add, &Value::Float(16_777_216.0), &Value::Float(1.0));
        assert_eq!(f.to_string(), "16777216"); // 2^24 + 1 is not representable in 32 bits
        // Never an infinity or NaN.
        assert!(arith(Mul, &Value::Double(1e200), &Value::Double(1e200)).is_err());
        assert!(arith(Mul, &Value::Float(1e30), &Value::Float(1e30)).is_err());
        assert!(arith(Add, &Value::Float(1.0), &big(&format!("1{}", "0".repeat(400)))).is_err());
    }

    #[test]
    fn division_types() {
        is(op(Div, &Value::Int(7), &Value::Int(2)), "DECIMAL", "3.5");
        is(op(Div, &Value::Int(6), &Value::Int(3)), "DECIMAL", "2");
        is(op(Div, &big("10"), &Value::Int(4)), "DECIMAL", "2.5");
        is(op(Div, &dec("1"), &Value::Int(4)), "DECIMAL", "0.25");
        is(op(Div, &cur("10"), &Value::Int(4)), "CURRENCY", "$2.50");
        is(op(Div, &cur("10"), &cur("4")), "DECIMAL", "2.5");
        is(op(Div, &Value::Int(10), &cur("4")), "DECIMAL", "2.5");
        is(op(Div, &Value::Int(1), &rat(3, 1)), "RATIONAL", "1/3");
        is(op(Div, &rat(1, 2), &dec("0.25")), "RATIONAL", "2");
        is(op(Div, &Value::Double(1.0), &Value::Int(4)), "DOUBLE", "0.25");
        is(op(Div, &Value::Float(1.0), &Value::Int(4)), "FLOAT", "0.25");
        is(op(Div, &Value::Int(i64::MIN), &Value::Int(-1)), "DECIMAL", "9223372036854775808");
        let third = op(Div, &Value::Int(1), &Value::Int(3)).to_string();
        assert_eq!(third, format!("0.{}", "3".repeat(crate::decimal::DIV_DIGITS)));
        for zero in [Value::Int(0), big("0"), Value::Float(0.0), Value::Double(0.0), dec("0"), cur("0"), rat(0, 1)] {
            for (_, x) in one_of_each() {
                assert!(arith(Div, &x, &zero).is_err(), "{x:?} / {zero:?}");
            }
        }
        // Zero divided by anything is zero, in the type the rules give.
        is(op(Div, &Value::Int(0), &Value::Int(5)), "DECIMAL", "0");
    }

    #[test]
    fn integer_division_and_remainder() {
        is(op(IntDiv, &Value::Int(7), &Value::Int(2)), "INTEGER", "3");
        is(op(Mod, &Value::Int(7), &Value::Int(4)), "INTEGER", "3");
        is(op(Mod, &Value::Int(-7), &Value::Int(4)), "INTEGER", "-3");
        is(op(IntDiv, &big("100000000000000000000"), &Value::Int(7)), "BIGNUM", "14285714285714285714");
        is(op(Mod, &big("100000000000000000000"), &Value::Int(7)), "BIGNUM", "2");
        is(op(IntDiv, &Value::Int(i64::MIN), &Value::Int(-1)), "BIGNUM", "9223372036854775808");
        // Operands are rounded to whole numbers first.
        is(op(IntDiv, &dec("7.5"), &dec("2")), "INTEGER", "4");
        is(op(Mod, &dec("7.5"), &Value::Int(2)), "INTEGER", "0");
        is(op(IntDiv, &Value::Double(9.0), &rat(7, 2)), "INTEGER", "2");
        assert!(arith(IntDiv, &Value::Int(1), &dec("0.2")).is_err());
        assert!(arith(Mod, &Value::Int(1), &Value::Int(0)).is_err());
    }

    #[test]
    fn powers() {
        is(op(Pow, &Value::Int(2), &Value::Int(10)), "INTEGER", "1024");
        is(op(Pow, &Value::Int(2), &Value::Int(100)), "BIGNUM", "1267650600228229401496703205376");
        is(op(Pow, &big("2"), &Value::Int(3)), "BIGNUM", "8");
        is(op(Pow, &dec("1.5"), &Value::Int(2)), "DECIMAL", "2.25");
        is(op(Pow, &cur("1.1"), &Value::Int(2)), "CURRENCY", "$1.21");
        is(op(Pow, &rat(2, 3), &Value::Int(3)), "RATIONAL", "8/27");
        is(op(Pow, &rat(2, 3), &Value::Int(-2)), "RATIONAL", "9/4");
        is(op(Pow, &Value::Int(2), &Value::Int(-2)), "DECIMAL", "0.25");
        is(op(Pow, &dec("0.5"), &Value::Int(-2)), "DECIMAL", "4");
        is(op(Pow, &Value::Int(7), &Value::Int(0)), "INTEGER", "1");
        is(op(Pow, &Value::Int(2), &dec("2"))
            , "INTEGER", "4"); // a whole decimal exponent still counts as whole
        is(op(Pow, &Value::Int(9), &dec("0.5")), "DOUBLE", "3");
        is(op(Pow, &Value::Double(2.0), &Value::Int(10)), "DOUBLE", "1024");
        is(op(Pow, &Value::Float(2.0), &Value::Int(10)), "FLOAT", "1024");
        assert!(arith(Pow, &Value::Int(0), &Value::Int(-1)).is_err());
        assert!(arith(Pow, &Value::Int(-8), &dec("0.5")).is_err()); // NaN is not a result
    }

    #[test]
    fn dates_and_text_in_arithmetic() {
        let d = Value::DateTime(crate::datetime::parse("2024-01-31").unwrap());
        assert_eq!(op(Add, &d, &Value::Int(1)).to_string(), "2024-02-01");
        assert_eq!(op(Add, &d, &dec("0.5")).to_string(), "2024-01-31 12:00:00");
        assert_eq!(op(Add, &d, &rat(1, 2)).to_string(), "2024-01-31 12:00:00");
        assert_eq!(op(Sub, &d, &Value::Double(1.0)).to_string(), "2024-01-30");
        let e = Value::DateTime(crate::datetime::parse("2024-01-29").unwrap());
        is(op(Sub, &d, &e), "INTEGER", "2");
        is(op(Add, &Value::text("3"), &Value::Int(4)), "INTEGER", "7");
        is(op(Add, &Value::text("2.5"), &Value::Int(1)), "DECIMAL", "3.5");
        is(op(Add, &Value::text("a"), &Value::text("b")), "TEXT", "ab");
        is(op(Add, &Value::Bool(true), &Value::Int(0)), "INTEGER", "-1");
        assert!(arith(Add, &Value::text("x"), &Value::Int(1)).is_err());
        assert!(arith(Add, &Value::Null, &Value::Int(1)).unwrap().is_null());
    }

    #[test]
    fn negation_keeps_the_type() {
        for (name, v) in one_of_each() {
            let n = negate(&v).unwrap();
            assert_eq!(n.type_label(), name);
            assert_eq!(n, Value::Int(-6));
        }
        assert!(negate(&Value::Null).unwrap().is_null());
    }
}
