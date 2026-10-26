//! Dynamically typed cell values with Access-flavoured semantics:
//! case-insensitive text, `Null` that sorts first, and a tower of numeric
//! types: `INTEGER` (64-bit), `BIGNUM` (unbounded integer), `FLOAT` (32-bit),
//! `DOUBLE` (64-bit), `DECIMAL` (unbounded decimal), `CURRENCY` (a decimal
//! that is kept distinct) and `RATIONAL` (exact fractions).
//!
//! Arithmetic between the numeric types is in [`crate::numeric`]. Comparison,
//! sorting, hashing and keys are *exact* across every numeric type: two
//! numbers are equal exactly when they have the same mathematical value, so
//! `1 = 1.0 = 2/2` and a `DOUBLE` 0.1 is not the `DECIMAL` 0.1.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use num_bigint::BigInt;
use num_traits::{FromPrimitive, ToPrimitive, Zero};
use serde::{Deserialize, Serialize, Serializer};

use crate::datetime;
use crate::interval::Interval;
use crate::tz;
use crate::decimal::Decimal;
use crate::error::{Error, Result};
use crate::rational::Rational;

pub type Row = Vec<Value>;

/// A cell value.
///
/// Numbers keep the type they were created with: the result type of every
/// operation depends only on the types of its operands, never on their values
/// (`6 / 3` is a `DECIMAL` 2, not an `INTEGER`). Floats are always finite.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(from = "Wire")]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    /// `INTEGER`: 64-bit signed.
    Int(i64),
    /// `BIGNUM`: an integer of any size.
    Big(BigInt),
    /// `FLOAT`: 32-bit floating point.
    Float(f32),
    /// `DOUBLE`: 64-bit floating point.
    Double(f64),
    /// `DECIMAL`: a decimal number of any size and precision.
    Dec(Decimal),
    /// `CURRENCY`: represented exactly like `DECIMAL`, but a type of its own.
    Currency(Decimal),
    /// `RATIONAL`: an exact fraction of two `BIGNUM`s.
    Rational(Rational),
    /// A wall-clock reading in whole seconds since 1970-01-01 00:00:00, with no zone.
    DateTime(i64),
    Text(Arc<str>),
    /// `TIMESTAMPTZ`: an instant, as seconds since the Unix epoch in UTC.
    TimestampTz(i64),
    /// `INTERVAL`: months, days and seconds.
    Interval(Interval),
    /// `JSON`: a JSON document, kept as canonical text.
    Json(Arc<str>),
}

// ----- on-disk form ---------------------------------------------------------
//
// The variant order of these enums is the file format and must never change.
// `Double` (3) and the old fixed-point `CurrencyOld` (4) come from the first
// versions; the rest were added later. A damaged non-finite float reads as Null.

#[derive(Deserialize)]
enum Wire {
    Null,
    Bool(bool),
    Int(i64),
    Double(f64),
    /// Units of 1/10 000, as originally stored.
    CurrencyOld(i64),
    DateTime(i64),
    Text(Arc<str>),
    Dec(Decimal),
    Big(BigInt),
    Float(f32),
    Rational(Rational),
    Currency(Decimal),
    TimestampTz(i64),
    Interval(Interval),
    Json(Arc<str>),
}

#[derive(Serialize)]
#[allow(dead_code)]
enum WireRef<'a> {
    Null,
    Bool(bool),
    Int(i64),
    Double(f64),
    CurrencyOld(i64),
    DateTime(i64),
    Text(&'a str),
    Dec(&'a Decimal),
    Big(&'a BigInt),
    Float(f32),
    Rational(&'a Rational),
    Currency(&'a Decimal),
    TimestampTz(i64),
    Interval(&'a Interval),
    Json(&'a str),
}

impl From<Wire> for Value {
    fn from(w: Wire) -> Value {
        match w {
            Wire::Null => Value::Null,
            Wire::Bool(b) => Value::Bool(b),
            Wire::Int(i) => Value::Int(i),
            Wire::Double(f) if f.is_finite() => Value::Double(f),
            Wire::Float(f) if f.is_finite() => Value::Float(f),
            Wire::Double(_) | Wire::Float(_) => Value::Null,
            Wire::CurrencyOld(c) => Value::Currency(Decimal::from_parts(BigInt::from(c), 4)),
            Wire::DateTime(t) => Value::DateTime(t),
            Wire::Text(s) => Value::Text(s),
            Wire::Dec(d) => Value::Dec(d),
            Wire::Big(b) => Value::Big(b),
            Wire::Rational(r) => Value::Rational(r),
            Wire::Currency(d) => Value::Currency(d),
            Wire::TimestampTz(t) => Value::TimestampTz(t),
            Wire::Interval(i) => Value::Interval(i),
            Wire::Json(s) => Value::Json(s),
        }
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Value::Null => WireRef::Null,
            Value::Bool(b) => WireRef::Bool(*b),
            Value::Int(i) => WireRef::Int(*i),
            Value::Big(b) => WireRef::Big(b),
            Value::Float(f) => WireRef::Float(*f),
            Value::Double(f) => WireRef::Double(*f),
            Value::Dec(d) => WireRef::Dec(d),
            Value::Currency(d) => WireRef::Currency(d),
            Value::Rational(r) => WireRef::Rational(r),
            Value::DateTime(t) => WireRef::DateTime(*t),
            Value::Text(t) => WireRef::Text(t),
            Value::TimestampTz(t) => WireRef::TimestampTz(*t),
            Value::Interval(i) => WireRef::Interval(i),
            Value::Json(s) => WireRef::Json(s),
        }
        .serialize(s)
    }
}

// ----- data types -------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "TypeWire", into = "TypeWire")]
pub enum DataType {
    /// 64-bit signed integer.
    Integer,
    /// Integer of unbounded size.
    Bignum,
    /// 32-bit floating point.
    Float,
    /// 64-bit floating point.
    Double,
    /// Decimal number of unbounded size and precision.
    Decimal,
    /// Represented like `Decimal`; a separate type for money.
    Currency,
    /// Exact fraction of two `Bignum`s.
    Rational,
    Text,
    YesNo,
    DateTime,
    /// An instant, read on a wall clock through a time zone.
    TimestampTz,
    /// A length of time in months, days and seconds.
    Interval,
    /// A JSON document.
    Json,
    /// A field whose value is an expression over the other fields of the same
    /// record. The expression is kept in the column's `default`; see
    /// [`crate::schema::Column::expression`].
    Calculated,
}

/// File format of [`DataType`]; as for [`Wire`], the order must never change.
/// `Number` was a short-lived single numeric type; it is read as `Decimal`.
#[derive(Serialize, Deserialize)]
enum TypeWire {
    Integer,
    Double,
    Currency,
    Text,
    YesNo,
    DateTime,
    Calculated,
    Number,
    Bignum,
    Float,
    Decimal,
    Rational,
    TimestampTz,
    Interval,
    Json,
}

impl From<TypeWire> for DataType {
    fn from(w: TypeWire) -> DataType {
        match w {
            TypeWire::Integer => DataType::Integer,
            TypeWire::Double => DataType::Double,
            TypeWire::Currency => DataType::Currency,
            TypeWire::Text => DataType::Text,
            TypeWire::YesNo => DataType::YesNo,
            TypeWire::DateTime => DataType::DateTime,
            TypeWire::Calculated => DataType::Calculated,
            TypeWire::Number | TypeWire::Decimal => DataType::Decimal,
            TypeWire::Bignum => DataType::Bignum,
            TypeWire::Float => DataType::Float,
            TypeWire::Rational => DataType::Rational,
            TypeWire::TimestampTz => DataType::TimestampTz,
            TypeWire::Interval => DataType::Interval,
            TypeWire::Json => DataType::Json,
        }
    }
}

impl From<DataType> for TypeWire {
    fn from(t: DataType) -> TypeWire {
        match t {
            DataType::Integer => TypeWire::Integer,
            DataType::Bignum => TypeWire::Bignum,
            DataType::Float => TypeWire::Float,
            DataType::Double => TypeWire::Double,
            DataType::Decimal => TypeWire::Decimal,
            DataType::Currency => TypeWire::Currency,
            DataType::Rational => TypeWire::Rational,
            DataType::Text => TypeWire::Text,
            DataType::YesNo => TypeWire::YesNo,
            DataType::DateTime => TypeWire::DateTime,
            DataType::Calculated => TypeWire::Calculated,
            DataType::TimestampTz => TypeWire::TimestampTz,
            DataType::Interval => TypeWire::Interval,
            DataType::Json => TypeWire::Json,
        }
    }
}

impl DataType {
    pub fn sql_name(self) -> &'static str {
        match self {
            DataType::Integer => "INTEGER",
            DataType::Bignum => "BIGNUM",
            DataType::Float => "FLOAT",
            DataType::Double => "DOUBLE",
            DataType::Decimal => "DECIMAL",
            DataType::Currency => "CURRENCY",
            DataType::Rational => "RATIONAL",
            DataType::Text => "TEXT",
            DataType::YesNo => "YESNO",
            DataType::DateTime => "DATETIME",
            DataType::TimestampTz => "TIMESTAMPTZ",
            DataType::Interval => "INTERVAL",
            DataType::Json => "JSON",
            DataType::Calculated => "CALCULATED",
        }
    }

    /// Parses a (case-insensitive) SQL type name. Returns the type and
    /// whether the name implies an AutoNumber column.
    pub fn from_sql_name(name: &str) -> Option<(DataType, bool)> {
        let n = name.to_ascii_uppercase();
        Some(match n.as_str() {
            "INTEGER" | "INT" | "LONG" | "SHORT" | "SMALLINT" | "BIGINT" | "BYTE" | "TINYINT" | "INT2" | "INT4" | "INT8" | "OID" => {
                (DataType::Integer, false)
            }
            "COUNTER" | "AUTOINCREMENT" | "AUTONUMBER" | "SERIAL" | "SERIAL2" | "SERIAL4" | "SERIAL8" | "SMALLSERIAL" | "BIGSERIAL" => (DataType::Integer, true),
            "BIGNUM" => (DataType::Bignum, false),
            "FLOAT" | "REAL" | "SINGLE" | "FLOAT4" => (DataType::Float, false),
            "DOUBLE" | "FLOAT8" => (DataType::Double, false),
            // NUMBER is the name of the short-lived single numeric type that
            // DECIMAL replaced; it is still read so older scripts and saved
            // queries keep working.
            "DECIMAL" | "NUMERIC" | "NUMBER" => (DataType::Decimal, false),
            "CURRENCY" | "MONEY" => (DataType::Currency, false),
            "RATIONAL" => (DataType::Rational, false),
            "TEXT" | "VARCHAR" | "CHAR" | "STRING" | "MEMO" | "LONGTEXT" | "NVARCHAR" | "NCHAR" | "BPCHAR" | "NAME" | "CHARACTER" => {
                (DataType::Text, false)
            }
            "YESNO" | "BIT" | "BOOLEAN" | "BOOL" | "LOGICAL" => (DataType::YesNo, false),
            "DATETIME" | "DATE" | "TIME" | "TIMESTAMP" => (DataType::DateTime, false),
            "TIMESTAMPTZ" => (DataType::TimestampTz, false),
            "INTERVAL" => (DataType::Interval, false),
            "JSON" | "JSONB" => (DataType::Json, false),
            "CALCULATED" => (DataType::Calculated, false),
            _ => return None,
        })
    }

    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            DataType::Integer
                | DataType::Bignum
                | DataType::Float
                | DataType::Double
                | DataType::Decimal
                | DataType::Currency
                | DataType::Rational
        )
    }

    /// Numeric types that hold exact values (everything but the floats).
    pub fn is_exact(self) -> bool {
        self.is_numeric() && !matches!(self, DataType::Float | DataType::Double)
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.sql_name())
    }
}

// ----- numeric kinds --------------------------------------------------------------

/// The numeric type of a value, used to pick the type of a result.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Int,
    Big,
    Rat,
    Dec,
    Cur,
    Float,
    Double,
}

/// Reads a number from text: optional sign, `$` and thousands separators are
/// tolerated, and so is an exponent (`1.5e3`). Whole numbers become `INTEGER`
/// (or `BIGNUM` when too large), everything else `DECIMAL`.
pub(crate) fn parse_num(s: &str) -> Option<Value> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let cleaned: String;
    let t = if t.contains(['$', ',']) {
        cleaned = t.replace(['$', ','], "");
        cleaned.as_str()
    } else {
        t
    };
    if let Ok(i) = t.parse::<i64>() {
        return Some(Value::Int(i));
    }
    let d = Decimal::parse(t)?;
    Some(match d.to_i64() {
        Some(i) => Value::Int(i),
        None if d.is_integer() => Value::Big(d.parts().0),
        None => Value::Dec(d),
    })
}

fn ci_cmp(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// A date as a number of days (the OLE convention), for mixing dates with numbers.
fn ole_value(t: i64) -> Value {
    Decimal::from_f64(datetime::ole_days(t)).map_or(Value::Int(0), Value::Dec)
}

const TWO_63: f64 = 9_223_372_036_854_775_808.0; // 2^63, exactly representable

/// Compares an integer with a finite float exactly, without allocating.
fn cmp_int_f64(i: i64, f: f64) -> Ordering {
    if f >= TWO_63 {
        return Ordering::Less;
    }
    if f < -TWO_63 {
        return Ordering::Greater;
    }
    let t = f.trunc();
    match i.cmp(&(t as i64)) {
        Ordering::Equal => {
            let frac = f - t;
            if frac > 0.0 {
                Ordering::Less
            } else if frac < 0.0 {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        other => other,
    }
}

fn f64_cmp(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or_else(|| a.total_cmp(&b))
}

/// Exact numeric comparison of two numeric values (see the module docs).
pub(crate) fn num_cmp(a: &Value, b: &Value) -> Ordering {
    use Value::*;
    match (a, b) {
        (Int(x), Int(y)) => x.cmp(y),
        (Big(x), Big(y)) => x.cmp(y),
        (Int(x), Big(y)) => BigInt::from(*x).cmp(y),
        (Big(x), Int(y)) => x.cmp(&BigInt::from(*y)),
        (Dec(x) | Currency(x), Dec(y) | Currency(y)) => x.cmp(y),
        (Rational(x), Rational(y)) => x.cmp(y),
        (Double(x), Double(y)) => f64_cmp(*x, *y),
        (Float(x), Float(y)) => f64_cmp(*x as f64, *y as f64),
        (Float(x), Double(y)) => f64_cmp(*x as f64, *y),
        (Double(x), Float(y)) => f64_cmp(*x, *y as f64),
        (Int(x), Double(y)) => cmp_int_f64(*x, *y),
        (Double(x), Int(y)) => cmp_int_f64(*y, *x).reverse(),
        (Int(x), Float(y)) => cmp_int_f64(*x, *y as f64),
        (Float(x), Int(y)) => cmp_int_f64(*y, *x as f64).reverse(),
        (Int(x), Dec(y) | Currency(y)) => Decimal::from_i64(*x).cmp(y),
        (Dec(x) | Currency(x), Int(y)) => x.cmp(&Decimal::from_i64(*y)),
        (x, y) => match (x.exact(), y.exact()) {
            (Some(p), Some(q)) => p.cmp(&q),
            _ => Ordering::Equal,
        },
    }
}

impl Value {
    pub fn text(s: &str) -> Value {
        Value::Text(Arc::from(s))
    }

    /// An `INTEGER` if the number is whole and fits, else a `DECIMAL` (or
    /// `BIGNUM` for a whole number that is too large). Used for results whose
    /// type is "whatever the number needs", such as parsed text.
    pub fn from_decimal(d: Decimal) -> Value {
        match d.to_i64() {
            Some(i) => Value::Int(i),
            None if d.is_integer() => Value::Big(d.parts().0),
            None => Value::Dec(d),
        }
    }

    /// A `DOUBLE` from a float; `None` for NaN or infinity.
    pub fn from_f64(f: f64) -> Option<Value> {
        f.is_finite().then_some(Value::Double(f))
    }

    /// A `FLOAT` from a float; `None` for NaN or infinity (including a double too large for 32 bits).
    pub fn from_f32(f: f32) -> Option<Value> {
        f.is_finite().then_some(Value::Float(f))
    }

    /// How a number compares with zero (`None` for a value that is not a number).
    pub(crate) fn sign(&self) -> Option<Ordering> {
        if let Value::Interval(i) = self {
            return Some(i.total_secs().cmp(&0));
        }
        let n = self.num()?;
        Some(num_cmp(&n, &Value::Int(0)))
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn is_numeric(&self) -> bool {
        self.kind().is_some()
    }

    /// Reads a number from text, the way a number typed by a user is read: a whole
    /// number becomes an `INTEGER` (a `BIGNUM` if too large), any other a `DECIMAL`.
    pub fn parse_number(s: &str) -> Option<Value> {
        parse_num(s)
    }

    pub(crate) fn kind(&self) -> Option<Kind> {
        Some(match self {
            Value::Int(_) => Kind::Int,
            Value::Big(_) => Kind::Big,
            Value::Rational(_) => Kind::Rat,
            Value::Dec(_) => Kind::Dec,
            Value::Currency(_) => Kind::Cur,
            Value::Float(_) => Kind::Float,
            Value::Double(_) => Kind::Double,
            _ => return None,
        })
    }

    pub fn data_type(&self) -> Option<DataType> {
        Some(match self {
            Value::Null => return None,
            Value::Bool(_) => DataType::YesNo,
            Value::Int(_) => DataType::Integer,
            Value::Big(_) => DataType::Bignum,
            Value::Float(_) => DataType::Float,
            Value::Double(_) => DataType::Double,
            Value::Dec(_) => DataType::Decimal,
            Value::Currency(_) => DataType::Currency,
            Value::Rational(_) => DataType::Rational,
            Value::DateTime(_) => DataType::DateTime,
            Value::TimestampTz(_) => DataType::TimestampTz,
            Value::Interval(_) => DataType::Interval,
            Value::Json(_) => DataType::Json,
            Value::Text(_) => DataType::Text,
        })
    }

    pub fn type_label(&self) -> &'static str {
        self.data_type().map_or("Null", DataType::sql_name)
    }

    fn rank(&self) -> u8 {
        match self {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::DateTime(_) | Value::TimestampTz(_) => 3,
            Value::Text(_) => 4,
            Value::Interval(_) => 5,
            Value::Json(_) => 6,
            _ => 2,
        }
    }

    /// Strict numeric view: only genuinely numeric (and Yes/No) values.
    pub(crate) fn num(&self) -> Option<Cow<'_, Value>> {
        match self {
            Value::Bool(b) => Some(Cow::Owned(Value::Int(if *b { -1 } else { 0 }))),
            v if v.is_numeric() => Some(Cow::Borrowed(v)),
            _ => None,
        }
    }

    /// Lenient numeric view: also parses text and treats dates as OLE days.
    pub(crate) fn num_lenient(&self) -> Option<Cow<'_, Value>> {
        match self {
            Value::Text(s) => parse_num(s).map(Cow::Owned),
            Value::DateTime(t) | Value::TimestampTz(t) => Some(Cow::Owned(ole_value(*t))),
            other => other.num(),
        }
    }

    pub fn to_f64(&self) -> Option<f64> {
        self.num_lenient().and_then(|n| n.as_f64())
    }

    // ----- conversions of numeric values (the receiver must be numeric) ------------

    /// The nearest `f64`; `None` if the value is not numeric or too large.
    pub(crate) fn as_f64(&self) -> Option<f64> {
        let f = match self {
            Value::Int(i) => *i as f64,
            Value::Big(b) => b.to_f64()?,
            Value::Float(f) => *f as f64,
            Value::Double(f) => *f,
            Value::Dec(d) | Value::Currency(d) => d.to_f64(),
            Value::Rational(r) => r.to_f64(),
            _ => return None,
        };
        f.is_finite().then_some(f)
    }

    /// The nearest `f32`; `None` if not numeric or out of range.
    pub(crate) fn as_f32(&self) -> Option<f32> {
        match self {
            Value::Float(f) => Some(*f),
            other => {
                let f = other.as_f64()? as f32;
                f.is_finite().then_some(f)
            }
        }
    }

    /// The exact value as a fraction. Floats contribute their exact binary value.
    pub(crate) fn exact(&self) -> Option<Rational> {
        Some(match self {
            Value::Int(i) => Rational::from_i64(*i),
            Value::Big(b) => Rational::from_int(b.clone()),
            Value::Dec(d) | Value::Currency(d) => Rational::from_decimal(d),
            Value::Rational(r) => r.clone(),
            Value::Float(f) => Rational::from_f64_exact(*f as f64)?,
            Value::Double(f) => Rational::from_f64_exact(*f)?,
            _ => return None,
        })
    }

    /// The value as a decimal: exact for integers and decimals, rounded to
    /// 50 digits for a fraction that does not terminate, and the shortest
    /// round-trip decimal for a float (so a `FLOAT` 0.1 becomes `0.1`).
    pub(crate) fn as_decimal(&self) -> Option<Decimal> {
        Some(match self {
            Value::Int(i) => Decimal::from_i64(*i),
            Value::Big(b) => Decimal::from_parts(b.clone(), 0),
            Value::Dec(d) | Value::Currency(d) => d.clone(),
            Value::Rational(r) => r.to_decimal(),
            Value::Float(f) => Decimal::parse(&format!("{f}"))?,
            Value::Double(f) => Decimal::from_f64(*f)?,
            _ => return None,
        })
    }

    /// The value as a fraction taking floats at their shortest decimal value
    /// (`0.1` is 1/10), which is what a conversion to `RATIONAL` should mean.
    fn as_rational_decimal(&self) -> Option<Rational> {
        match self {
            Value::Float(_) | Value::Double(_) => self.as_decimal().map(|d| Rational::from_decimal(&d)),
            other => other.exact(),
        }
    }

    /// The value rounded to a whole number (ties to even).
    pub(crate) fn rounded_int(&self) -> Option<BigInt> {
        Some(match self {
            Value::Int(i) => BigInt::from(*i),
            Value::Big(b) => b.clone(),
            Value::Dec(d) | Value::Currency(d) => d.round(0).parts().0,
            Value::Rational(r) => r.round(),
            Value::Float(f) => BigInt::from_f64((*f as f64).round_ties_even())?,
            Value::Double(f) => BigInt::from_f64(f.round_ties_even())?,
            _ => return None,
        })
    }

    /// True for a numeric value equal to zero.
    pub(crate) fn is_zero(&self) -> bool {
        match self {
            Value::Int(i) => *i == 0,
            Value::Big(b) => b.is_zero(),
            Value::Float(f) => *f == 0.0,
            Value::Double(f) => *f == 0.0,
            Value::Dec(d) | Value::Currency(d) => d.is_zero(),
            Value::Rational(r) => r.is_zero(),
            _ => false,
        }
    }

    /// SQL comparison: `None` when either side is `Null`. Mixed text/number
    /// and text/date operands are coerced like Access does.
    pub fn sql_cmp(&self, other: &Value) -> Option<Ordering> {
        if self.is_null() || other.is_null() {
            return None;
        }
        Some(match (self, other) {
            (Value::Text(a), Value::Text(b)) => ci_cmp(a, b),
            (Value::Text(t), Value::DateTime(d)) => match datetime::parse(t) {
                Some(p) => p.cmp(d),
                None => Ordering::Greater,
            },
            (Value::DateTime(d), Value::Text(t)) => match datetime::parse(t) {
                Some(p) => d.cmp(&p),
                None => Ordering::Less,
            },
            (Value::Text(t), Value::TimestampTz(d)) => match tz::parse_timestamptz(t, tz::session()) {
                Some(p) => p.cmp(d),
                None => Ordering::Greater,
            },
            (Value::TimestampTz(d), Value::Text(t)) => match tz::parse_timestamptz(t, tz::session()) {
                Some(p) => d.cmp(&p),
                None => Ordering::Less,
            },
            (Value::Text(t), Value::Interval(i)) => match Interval::parse(t, None) {
                Ok(p) => p.cmp(i),
                Err(_) => Ordering::Greater,
            },
            (Value::Interval(i), Value::Text(t)) => match Interval::parse(t, None) {
                Ok(p) => i.cmp(&p),
                Err(_) => Ordering::Less,
            },
            (Value::Interval(a), Value::Interval(b)) => a.cmp(b),
            (Value::Json(a), Value::Json(b)) => a.cmp(b),
            (Value::Json(a), Value::Text(t)) => match crate::json::canonical(t) {
                Some(c) => (**a).cmp(c.as_str()),
                None => (**a).cmp(&**t),
            },
            (Value::Text(t), Value::Json(b)) => match crate::json::canonical(t) {
                Some(c) => c.as_str().cmp(&**b),
                None => (**t).cmp(&**b),
            },
            (Value::Text(t), n) => match (parse_num(t), n.num()) {
                (Some(x), Some(y)) => num_cmp(&x, &y),
                _ => Ordering::Greater,
            },
            (n, Value::Text(t)) => match (n.num(), parse_num(t)) {
                (Some(x), Some(y)) => num_cmp(&x, &y),
                _ => Ordering::Less,
            },
            (Value::DateTime(a) | Value::TimestampTz(a), Value::DateTime(b) | Value::TimestampTz(b)) => a.cmp(b),
            (Value::DateTime(a) | Value::TimestampTz(a), n) => match n.num() {
                Some(y) => num_cmp(&ole_value(*a), &y),
                None => self.cmp(other),
            },
            (n, Value::DateTime(b) | Value::TimestampTz(b)) => match n.num() {
                Some(x) => num_cmp(&x, &ole_value(*b)),
                None => self.cmp(other),
            },
            (a, b) => match (a.num(), b.num()) {
                (Some(x), Some(y)) => num_cmp(&x, &y),
                _ => a.cmp(b),
            },
        })
    }

    /// Converts the value for storage in a column of type `ty`.
    ///
    /// Conversions between numeric types keep the value where they can and
    /// otherwise round: to `INTEGER`/`BIGNUM` to the nearest whole number
    /// (ties to even), to `FLOAT`/`DOUBLE` to the nearest float, to
    /// `DECIMAL`/`CURRENCY` exactly (a fraction that does not terminate is cut
    /// at 50 digits), to `RATIONAL` exactly (a float by its shortest decimal).
    pub fn coerce_to(&self, ty: DataType) -> Result<Value> {
        use Value::*;
        if self.is_null() {
            return Ok(Null);
        }
        // The stand-in for a sequence default is not a value to convert.
        if matches!(self, Text(s) if (**s).starts_with(crate::schema::SEQUENCE_DEFAULT)) {
            return Ok(self.clone());
        }
        let bad = || Error::type_err(format!("cannot convert {} value '{}' to {}", self.type_label(), self, ty));
        let range = || Error::type_err(format!("'{}' is out of range for {}", self, ty));
        match ty {
            // A calculated field holds whatever its expression produced.
            DataType::Calculated => Ok(self.clone()),
            DataType::Text => Ok(match self {
                Text(_) => self.clone(),
                v => Value::text(&v.to_string()),
            }),
            DataType::YesNo => match self {
                Bool(_) => Ok(self.clone()),
                Text(s) => {
                    // As PostgreSQL reads them: true, yes, on, 1 and false, no, off, 0, or any start of true,
                    // false, yes, no and off that is not ambiguous (t, tr, f, fa, y, ye, n, of ...); also -1.
                    let t = s.trim().to_ascii_lowercase();
                    let starts = |word: &str| !t.is_empty() && word.starts_with(t.as_str());
                    if t == "-1" || t == "1" || t == "on" || starts("true") || starts("yes") {
                        Ok(Bool(true))
                    } else if t == "0" || (t.len() >= 2 && "off".starts_with(t.as_str())) || starts("false") || starts("no") {
                        Ok(Bool(false))
                    } else {
                        Err(bad())
                    }
                }
                v => match v.num() {
                    Some(n) => Ok(Bool(!n.is_zero())),
                    None => Err(bad()),
                },
            },
            DataType::TimestampTz => match self {
                TimestampTz(_) => Ok(self.clone()),
                DateTime(local) => Ok(TimestampTz(tz::session().to_utc(*local))),
                Text(s) => Ok(TimestampTz(tz::parse_timestamptz(s, tz::session()).ok_or_else(bad)?)),
                v => match v.num().and_then(|n| n.as_f64()) {
                    Some(f) => Ok(TimestampTz(tz::session().to_utc(datetime::from_ole_days(f)))),
                    None => Err(bad()),
                },
            },
            DataType::Json => match self {
                Json(_) => Ok(self.clone()),
                Text(s) => crate::json::canonical(s).map(|c| Json(c.as_str().into())).ok_or_else(bad),
                Bool(_) | Int(_) | Big(_) | Dec(_) | Currency(_) | Float(_) | Double(_) | Rational(_) => {
                    Ok(Json(crate::json::to_text(&crate::json::from_value(self)).as_str().into()))
                }
                _ => Err(bad()),
            },
            DataType::Interval => match self {
                Interval(_) => Ok(self.clone()),
                Text(s) => crate::interval::Interval::parse(s, None).map(Interval).map_err(Error::type_err),
                v => match v.num().and_then(|n| n.as_decimal()) {
                    // A plain number is a number of days, as in date arithmetic.
                    Some(d) => crate::interval::Interval::from_days(&d).map(Interval).ok_or_else(range),
                    None => Err(bad()),
                },
            },
            DataType::DateTime => match self {
                DateTime(_) => Ok(self.clone()),
                TimestampTz(t) => Ok(DateTime(tz::session().to_local(*t))),
                Text(s) => Ok(DateTime(datetime::parse(s).ok_or_else(bad)?)),
                v => match v.num().and_then(|n| n.as_f64()) {
                    Some(f) => Ok(DateTime(datetime::from_ole_days(f))),
                    None => Err(bad()),
                },
            },
            numeric => {
                // Text such as "7/2" is a ratio, which only a RATIONAL reads.
                if let (DataType::Rational, Text(s)) = (numeric, self) {
                    return crate::rational::Rational::parse(s).map(Value::Rational).ok_or_else(bad);
                }
                let n = self.num_lenient().ok_or_else(bad)?;
                let n: &Value = &n;
                match numeric {
                    DataType::Integer => match n {
                        Int(_) => Ok(n.clone()),
                        other => other.rounded_int().and_then(|b| b.to_i64()).map(Int).ok_or_else(range),
                    },
                    DataType::Bignum => match n {
                        Big(_) => Ok(n.clone()),
                        other => other.rounded_int().map(Big).ok_or_else(range),
                    },
                    DataType::Float => match n {
                        Float(_) => Ok(n.clone()),
                        other => other.as_f32().map(Float).ok_or_else(range),
                    },
                    DataType::Double => match n {
                        Double(_) => Ok(n.clone()),
                        other => other.as_f64().map(Double).ok_or_else(range),
                    },
                    DataType::Decimal => match n {
                        Dec(_) => Ok(n.clone()),
                        other => other.as_decimal().map(Dec).ok_or_else(range),
                    },
                    DataType::Currency => match n {
                        Currency(_) => Ok(n.clone()),
                        other => other.as_decimal().map(Currency).ok_or_else(range),
                    },
                    DataType::Rational => match n {
                        Rational(_) => Ok(n.clone()),
                        other => other.as_rational_decimal().map(Rational).ok_or_else(range),
                    },
                    _ => unreachable!("non-numeric types are handled above"),
                }
            }
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Total order used by indexes, sorting, grouping and DISTINCT:
/// `Null` < Yes/No < numbers < dates < text (case-insensitive). All numeric
/// types are ordered together by exact value.
impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        let (ra, rb) = (self.rank(), other.rank());
        if ra != rb {
            return ra.cmp(&rb);
        }
        match (self, other) {
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::DateTime(a) | Value::TimestampTz(a), Value::DateTime(b) | Value::TimestampTz(b)) => a.cmp(b),
            (Value::Interval(a), Value::Interval(b)) => a.cmp(b),
            (Value::Json(a), Value::Json(b)) => a.cmp(b),
            (Value::Text(a), Value::Text(b)) => ci_cmp(a, b),
            (Value::Null, Value::Null) => Ordering::Equal,
            (a, b) if a.is_numeric() && b.is_numeric() => num_cmp(a, b),
            _ => Ordering::Equal,
        }
    }
}

/// Hashes the canonical exact value, so numbers equal under [`num_cmp`] hash
/// alike whatever their type: whole numbers by value, other numbers by their
/// fraction in lowest terms.
fn hash_number<H: Hasher>(v: &Value, state: &mut H) {
    fn whole<H: Hasher>(n: &BigInt, state: &mut H) {
        match n.to_i128() {
            Some(i) => {
                0u8.hash(state);
                i.hash(state);
            }
            None => {
                1u8.hash(state);
                n.hash(state);
            }
        }
    }
    match v {
        Value::Int(i) => {
            0u8.hash(state);
            (*i as i128).hash(state);
        }
        Value::Big(b) => whole(b, state),
        // An integral float hashes as its integer; the rest take the slow path.
        Value::Double(f) if f.fract() == 0.0 && f.abs() < 1e30 => {
            0u8.hash(state);
            (*f as i128).hash(state);
        }
        Value::Float(f) if f.fract() == 0.0 && f.abs() < 1e30 => {
            0u8.hash(state);
            (*f as i128).hash(state);
        }
        other => match other.exact() {
            Some(r) if r.is_integer() => whole(r.numer(), state),
            Some(r) => {
                2u8.hash(state);
                r.numer().hash(state);
                r.denom().hash(state);
            }
            None => 3u8.hash(state),
        },
    }
}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Value::Null => 0u8.hash(state),
            Value::Bool(b) => {
                1u8.hash(state);
                b.hash(state);
            }
            Value::DateTime(t) | Value::TimestampTz(t) => {
                3u8.hash(state);
                t.hash(state);
            }
            Value::Interval(i) => {
                5u8.hash(state);
                i.hash(state);
            }
            Value::Json(s) => {
                6u8.hash(state);
                s.hash(state);
            }
            Value::Text(s) => {
                4u8.hash(state);
                for b in s.bytes() {
                    b.to_ascii_lowercase().hash(state);
                }
            }
            numeric => {
                2u8.hash(state);
                hash_number(numeric, state);
            }
        }
    }
}

/// `$` and at least two decimals, as money is written.
fn format_currency(d: &Decimal, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut digits = d.abs().to_string();
    match digits.find('.') {
        None => digits.push_str(".00"),
        Some(p) if digits.len() - p == 2 => digits.push('0'),
        Some(_) => {}
    }
    write!(f, "{}${digits}", if d.is_negative() { "-" } else { "" })
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => Ok(()),
            Value::Bool(true) => f.write_str("Yes"),
            Value::Bool(false) => f.write_str("No"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Big(b) => write!(f, "{b}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Double(x) => write!(f, "{x}"),
            Value::Dec(d) => write!(f, "{d}"),
            Value::Currency(d) => format_currency(d, f),
            Value::Rational(r) => write!(f, "{r}"),
            Value::DateTime(t) => f.write_str(&datetime::format(*t)),
            Value::TimestampTz(t) => f.write_str(&tz::format_timestamptz(*t)),
            Value::Interval(i) => write!(f, "{i}"),
            Value::Json(s) => f.write_str(s),
            Value::Text(s) => f.write_str(s),
        }
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}
impl From<f64> for Value {
    /// Non-finite values have no numeric form and become `Null`.
    fn from(v: f64) -> Self {
        Value::from_f64(v).unwrap_or(Value::Null)
    }
}
impl From<Decimal> for Value {
    fn from(v: Decimal) -> Self {
        Value::Dec(v)
    }
}
impl From<BigInt> for Value {
    fn from(v: BigInt) -> Self {
        Value::Big(v)
    }
}
impl From<Rational> for Value {
    fn from(v: Rational) -> Self {
        Value::Rational(v)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::text(v)
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(Arc::from(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;

    fn dec(s: &str) -> Value {
        Value::Dec(Decimal::parse(s).unwrap())
    }
    fn rat(n: i64, d: i64) -> Value {
        Value::Rational(Rational::new(BigInt::from(n), BigInt::from(d)).unwrap())
    }
    fn big(s: &str) -> Value {
        Value::Big(s.parse().unwrap())
    }
    fn hash_of(v: &Value) -> u64 {
        let mut h = DefaultHasher::new();
        v.hash(&mut h);
        h.finish()
    }

    #[test]
    fn text_is_case_insensitive() {
        assert_eq!(Value::text("Hello"), Value::text("hELLO"));
        assert!(Value::text("apple") < Value::text("Banana"));
    }

    /// Every numeric type holding the same mathematical value is one value:
    /// equal, and hashing alike.
    #[test]
    fn equal_numbers_are_equal_whatever_their_type() {
        let twos = [
            Value::Int(2),
            big("2"),
            Value::Float(2.0),
            Value::Double(2.0),
            dec("2"),
            dec("2.00"),
            Value::Currency(Decimal::parse("2").unwrap()),
            rat(2, 1),
            rat(4, 2),
        ];
        for a in &twos {
            for b in &twos {
                assert_eq!(a, b, "{a:?} vs {b:?}");
                assert_eq!(hash_of(a), hash_of(b), "{a:?} vs {b:?}");
            }
        }
        let halves = [Value::Float(0.5), Value::Double(0.5), dec("0.5"), rat(1, 2), rat(2, 4)];
        for a in &halves {
            for b in &halves {
                assert_eq!(a, b);
                assert_eq!(hash_of(a), hash_of(b));
            }
        }
        // Exact: a binary 0.1 is not the decimal 0.1, and a third is not 0.333...
        assert_ne!(Value::Double(0.1), dec("0.1"));
        assert_ne!(Value::Float(0.1), Value::Double(0.1));
        assert_ne!(rat(1, 3), dec("0.3333333333333333"));
        assert_eq!(Value::Double(0.1), Value::Double(0.1));
        assert_eq!(hash_of(&Value::Double(-0.0)), hash_of(&Value::Int(0)));
        assert_eq!(Value::Double(-0.0), Value::Int(0));
    }

    #[test]
    fn ordering_across_types() {
        let ascending = [
            Value::Null,
            Value::Bool(false),
            big("-100000000000000000000"),
            Value::Int(i64::MIN),
            rat(-7, 2),
            dec("-3.25"),
            Value::Double(-0.5),
            rat(-1, 3),
            Value::Int(0),
            Value::Float(0.1),
            dec("0.25"),
            rat(1, 3),
            Value::Currency(Decimal::parse("0.5").unwrap()),
            Value::Int(1),
            dec("1.5"),
            Value::Double(2.5),
            Value::Int(i64::MAX),
            big("9223372036854775808"),
            dec("1e30"),
            Value::DateTime(0),
            Value::text("a"),
        ];
        for (i, a) in ascending.iter().enumerate() {
            for (j, b) in ascending.iter().enumerate() {
                assert_eq!(a.cmp(b), i.cmp(&j), "{a:?} vs {b:?}");
            }
        }
        assert!(Value::Int(i64::MAX) < Value::Double(9.3e18));
        assert!(Value::Int(i64::MIN) > Value::Double(-9.3e18));
        assert!(Value::Int(3) > Value::Double(2.9999999999999996));
    }

    #[test]
    fn type_of_values_and_names() {
        assert_eq!(Value::Int(1).data_type(), Some(DataType::Integer));
        assert_eq!(big("1").data_type(), Some(DataType::Bignum));
        assert_eq!(Value::Float(1.0).data_type(), Some(DataType::Float));
        assert_eq!(Value::Double(1.0).data_type(), Some(DataType::Double));
        assert_eq!(dec("1").data_type(), Some(DataType::Decimal));
        assert_eq!(Value::Currency(Decimal::from_i64(1)).data_type(), Some(DataType::Currency));
        assert_eq!(rat(1, 2).data_type(), Some(DataType::Rational));
        for (name, ty, auto) in [
            ("integer", DataType::Integer, false),
            ("BIGINT", DataType::Integer, false),
            ("counter", DataType::Integer, true),
            ("bignum", DataType::Bignum, false),
            ("float", DataType::Float, false),
            ("REAL", DataType::Float, false),
            ("double", DataType::Double, false),
            ("decimal", DataType::Decimal, false),
            ("numeric", DataType::Decimal, false),
            ("currency", DataType::Currency, false),
            ("MONEY", DataType::Currency, false),
            ("rational", DataType::Rational, false),
        ] {
            assert_eq!(DataType::from_sql_name(name), Some((ty, auto)), "{name}");
        }
        assert!(DataType::Integer.is_exact() && DataType::Rational.is_exact());
        assert!(!DataType::Float.is_exact() && DataType::Double.is_numeric());
        assert!(!DataType::Text.is_numeric());
    }

    #[test]
    fn sql_cmp_null_and_coercion() {
        assert_eq!(Value::Null.sql_cmp(&Value::Int(1)), None);
        assert_eq!(Value::text("10").sql_cmp(&Value::Int(9)), Some(Ordering::Greater));
        assert_eq!(Value::text("2.50").sql_cmp(&dec("2.5")), Some(Ordering::Equal));
        assert_eq!(rat(1, 2).sql_cmp(&dec("0.5")), Some(Ordering::Equal));
        let d = Value::DateTime(datetime::parse("2020-01-01").unwrap());
        assert_eq!(Value::text("2020-01-01").sql_cmp(&d), Some(Ordering::Equal));
    }

    #[test]
    fn coercions_between_numeric_types() {
        let cv = |v: Value, ty| v.coerce_to(ty).unwrap();
        // To the integer types: round to nearest, ties to even.
        assert_eq!(cv(dec("2.5"), DataType::Integer), Value::Int(2));
        assert_eq!(cv(dec("3.5"), DataType::Integer), Value::Int(4));
        assert_eq!(cv(Value::Double(-2.5), DataType::Integer), Value::Int(-2));
        assert_eq!(cv(rat(7, 2), DataType::Integer), Value::Int(4));
        assert!(matches!(cv(dec("2.5"), DataType::Bignum), Value::Big(_)));
        assert_eq!(cv(Value::Int(5), DataType::Bignum).to_string(), "5");
        assert!(big("9223372036854775808").coerce_to(DataType::Integer).is_err());
        assert!(Value::Double(1e30).coerce_to(DataType::Integer).is_err());
        assert_eq!(cv(Value::Double(1e30), DataType::Bignum).to_string(), "1000000000000000019884624838656");
        // To the floats.
        assert!(matches!(cv(dec("0.1"), DataType::Float), Value::Float(f) if f == 0.1f32));
        assert!(matches!(cv(dec("0.1"), DataType::Double), Value::Double(f) if f == 0.1));
        assert!(Value::Double(1e300).coerce_to(DataType::Float).is_err());
        assert!(matches!(cv(rat(1, 3), DataType::Double), Value::Double(f) if f == 1.0 / 3.0));
        // To the exact decimal types: floats by their shortest decimal.
        assert_eq!(cv(Value::Double(0.1), DataType::Decimal), dec("0.1"));
        assert_eq!(cv(Value::Float(0.1), DataType::Decimal), dec("0.1"));
        assert_eq!(cv(Value::Int(3), DataType::Currency).to_string(), "$3.00");
        assert_eq!(cv(rat(1, 4), DataType::Decimal).to_string(), "0.25");
        // To rational: exact, floats by their shortest decimal.
        assert_eq!(cv(dec("0.75"), DataType::Rational).to_string(), "3/4");
        assert_eq!(cv(Value::Double(0.1), DataType::Rational).to_string(), "1/10");
        assert_eq!(cv(Value::Int(4), DataType::Rational).to_string(), "4");
        // From text and Yes/No.
        assert_eq!(cv(Value::text("$1,234.50"), DataType::Currency).to_string(), "$1234.50");
        assert_eq!(cv(Value::text(" 42 "), DataType::Integer), Value::Int(42));
        assert_eq!(cv(Value::text("1.5e3"), DataType::Integer), Value::Int(1500));
        assert_eq!(cv(Value::Bool(true), DataType::Integer), Value::Int(-1));
        assert!(Value::text("abc").coerce_to(DataType::Decimal).is_err());
        assert!(Value::text("inf").coerce_to(DataType::Double).is_err());
        assert_eq!(cv(Value::text("yes"), DataType::YesNo), Value::Bool(true));
        assert_eq!(cv(dec("0.5"), DataType::YesNo), Value::Bool(true));
        assert_eq!(cv(dec("12.50"), DataType::Text), Value::text("12.5"));
    }

    #[test]
    fn display() {
        assert_eq!(Value::Currency(Decimal::parse("1234.5").unwrap()).to_string(), "$1234.50");
        assert_eq!(Value::Currency(Decimal::parse("-0.0005").unwrap()).to_string(), "-$0.0005");
        assert_eq!(Value::Currency(Decimal::parse("7").unwrap()).to_string(), "$7.00");
        assert_eq!(rat(-7, 2).to_string(), "-7/2");
        assert_eq!(rat(6, 3).to_string(), "2");
        assert_eq!(Value::Float(0.1).to_string(), "0.1");
        assert_eq!(Value::Double(0.1 + 0.2).to_string(), "0.30000000000000004");
        assert_eq!(Value::Double(3.0).to_string(), "3");
        assert_eq!(big("123456789012345678901234567890").to_string(), "123456789012345678901234567890");
    }

    #[test]
    fn disk_format_reads_old_files() {
        // Values as the first versions wrote them (variant index, then payload).
        let old = |index: u8, payload: Vec<u8>| {
            let mut b = vec![index];
            b.extend(payload);
            b
        };
        let zigzag = |v: i64| postcard::to_allocvec(&v).unwrap();
        assert_eq!(postcard::from_bytes::<Value>(&old(2, zigzag(-7))).unwrap(), Value::Int(-7));
        let d = postcard::from_bytes::<Value>(&old(3, 2.5f64.to_le_bytes().to_vec())).unwrap();
        assert!(matches!(d, Value::Double(f) if f == 2.5));
        // The old fixed-point currency, in 1/10 000ths, is a CURRENCY now.
        let c = postcard::from_bytes::<Value>(&old(4, zigzag(12_345_000))).unwrap();
        assert!(matches!(c, Value::Currency(_)));
        assert_eq!(c.to_string(), "$1234.50");
        let text = postcard::to_allocvec("Ab").unwrap();
        assert_eq!(postcard::from_bytes::<Value>(&old(6, text)).unwrap(), Value::text("ab"));
        // Old type tags keep their meaning; NUMBER (7) is read as DECIMAL.
        for (tag, ty) in [
            (0u8, DataType::Integer),
            (1, DataType::Double),
            (2, DataType::Currency),
            (3, DataType::Text),
            (4, DataType::YesNo),
            (5, DataType::DateTime),
            (6, DataType::Calculated),
            (7, DataType::Decimal),
        ] {
            assert_eq!(postcard::from_bytes::<DataType>(&[tag]).unwrap(), ty, "tag {tag}");
        }
    }

    #[test]
    fn disk_format_round_trips() {
        for v in [
            Value::Null,
            Value::Bool(true),
            Value::Int(i64::MIN),
            big("-123456789012345678901234567890"),
            Value::Float(-0.5),
            Value::Double(1e-300),
            dec("-0.000123"),
            Value::Currency(Decimal::parse("19.99").unwrap()),
            rat(-22, 7),
            Value::DateTime(5),
            Value::text("x"),
        ] {
            let bytes = postcard::to_allocvec(&v).unwrap();
            let back: Value = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(back.data_type(), v.data_type());
            assert_eq!(back, v);
            assert_eq!(back.to_string(), v.to_string());
        }
        // Whole INTEGERs keep the byte layout the first versions used.
        assert_eq!(postcard::to_allocvec(&Value::Int(5)).unwrap()[0], 2);
        for ty in [
            DataType::Integer,
            DataType::Bignum,
            DataType::Float,
            DataType::Double,
            DataType::Decimal,
            DataType::Currency,
            DataType::Rational,
            DataType::Text,
            DataType::YesNo,
            DataType::DateTime,
            DataType::Calculated,
        ] {
            let bytes = postcard::to_allocvec(&ty).unwrap();
            assert_eq!(postcard::from_bytes::<DataType>(&bytes).unwrap(), ty);
        }
        // A damaged non-finite float reads as Null instead of breaking ordering.
        let nan = postcard::to_allocvec(&WireRefNan::D(f64::NAN)).unwrap();
        assert!(postcard::from_bytes::<Value>(&nan).unwrap().is_null());
    }

    /// A `Double` with a NaN payload, as `WireRef::Double`.
    #[derive(Serialize)]
    enum WireRefNan {
        #[allow(dead_code)]
        A,
        #[allow(dead_code)]
        B,
        #[allow(dead_code)]
        C,
        D(f64),
    }
}
