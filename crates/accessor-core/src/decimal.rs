//! Arbitrary-precision decimal numbers.
//!
//! A [`Decimal`] is `mantissa × 10^-scale` with an unbounded integer mantissa.
//! Addition, subtraction and multiplication are exact. Division is exact when
//! the quotient terminates, and otherwise rounded (half-even) to
//! [`DIV_DIGITS`] significant digits.
//!
//! Values are kept in canonical form (no trailing zeros after the point), so
//! equal numbers have identical representations and derive `Eq`/`Hash`.

use std::cmp::Ordering;
use std::fmt;

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Significant digits kept when a division does not terminate.
pub const DIV_DIGITS: usize = 50;

/// Largest power of ten accepted when parsing an exponent or raising to a
/// power; keeps a typo such as `1e999999999` from exhausting memory.
const MAX_EXPONENT: i64 = 100_000;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Decimal {
    mant: BigInt,
    scale: u32,
}

fn pow10(n: u32) -> BigInt {
    BigInt::from(10u32).pow(n)
}

impl Decimal {
    /// Builds `mant × 10^-scale`, in canonical form. A negative scale means
    /// a multiple of a power of ten.
    pub fn from_parts(mut mant: BigInt, scale: i64) -> Decimal {
        let mut scale = scale;
        if scale < 0 {
            mant *= pow10(u32::try_from(-scale).unwrap_or(u32::MAX));
            scale = 0;
        }
        let mut scale = scale as u32;
        if mant.is_zero() {
            return Decimal { mant, scale: 0 };
        }
        while scale > 0 {
            let (q, r) = mant.div_rem(&BigInt::from(10u32));
            if !r.is_zero() {
                break;
            }
            mant = q;
            scale -= 1;
        }
        Decimal { mant, scale }
    }

    pub fn from_i64(i: i64) -> Decimal {
        Decimal { mant: BigInt::from(i), scale: 0 }
    }

    /// The exact decimal for a floating-point value's shortest round-trip
    /// text; `None` for NaN and infinities.
    pub fn from_f64(f: f64) -> Option<Decimal> {
        if !f.is_finite() {
            return None;
        }
        Decimal::parse(&format!("{f}"))
    }

    /// Parses `-12`, `3.14`, `.5`, `1e-3` and the like. Surrounding
    /// whitespace is not allowed.
    pub fn parse(s: &str) -> Option<Decimal> {
        let (neg, rest) = match s.as_bytes().first()? {
            b'-' => (true, &s[1..]),
            b'+' => (false, &s[1..]),
            _ => (false, s),
        };
        let (number, exp) = match rest.find(['e', 'E']) {
            Some(i) => (&rest[..i], rest[i + 1..].parse::<i64>().ok()?),
            None => (rest, 0),
        };
        if exp.abs() > MAX_EXPONENT {
            return None;
        }
        let (int_part, frac_part) = match number.find('.') {
            Some(i) => (&number[..i], &number[i + 1..]),
            None => (number, ""),
        };
        if int_part.is_empty() && frac_part.is_empty() {
            return None;
        }
        if !int_part.bytes().chain(frac_part.bytes()).all(|b| b.is_ascii_digit()) {
            return None;
        }
        let digits = format!("{int_part}{frac_part}");
        let mut mant = BigInt::parse_bytes(digits.as_bytes(), 10)?;
        if neg {
            mant = -mant;
        }
        Some(Decimal::from_parts(mant, frac_part.len() as i64 - exp))
    }

    /// The mantissa and the number of digits after the point.
    pub fn parts(&self) -> (BigInt, u32) {
        (self.mant.clone(), self.scale)
    }

    /// The nearest `f64`.
    pub fn to_f64(&self) -> f64 {
        self.to_string().parse().unwrap_or(f64::NAN)
    }

    /// The value as an `i64` if it is a whole number that fits.
    pub fn to_i64(&self) -> Option<i64> {
        if self.scale == 0 {
            self.mant.to_i64()
        } else {
            None
        }
    }

    pub fn is_integer(&self) -> bool {
        self.scale == 0
    }

    pub fn is_zero(&self) -> bool {
        self.mant.is_zero()
    }

    pub fn is_negative(&self) -> bool {
        self.mant.sign() == Sign::Minus
    }

    pub fn signum(&self) -> i64 {
        match self.mant.sign() {
            Sign::Minus => -1,
            Sign::NoSign => 0,
            Sign::Plus => 1,
        }
    }

    pub fn neg(&self) -> Decimal {
        Decimal { mant: -self.mant.clone(), scale: self.scale }
    }

    pub fn abs(&self) -> Decimal {
        Decimal { mant: self.mant.abs(), scale: self.scale }
    }

    /// The mantissa expressed with `scale` fractional digits (`scale` must not
    /// be smaller than the value's own).
    fn scaled_to(&self, scale: u32) -> BigInt {
        if scale == self.scale {
            self.mant.clone()
        } else {
            &self.mant * pow10(scale - self.scale)
        }
    }

    pub fn add(&self, o: &Decimal) -> Decimal {
        let s = self.scale.max(o.scale);
        Decimal::from_parts(self.scaled_to(s) + o.scaled_to(s), s as i64)
    }

    pub fn sub(&self, o: &Decimal) -> Decimal {
        let s = self.scale.max(o.scale);
        Decimal::from_parts(self.scaled_to(s) - o.scaled_to(s), s as i64)
    }

    pub fn mul(&self, o: &Decimal) -> Decimal {
        Decimal::from_parts(&self.mant * &o.mant, self.scale as i64 + o.scale as i64)
    }

    /// `self / o`, rounded half-even to [`DIV_DIGITS`] significant digits when
    /// it does not terminate. `None` when `o` is zero.
    pub fn div(&self, o: &Decimal) -> Option<Decimal> {
        if o.is_zero() {
            return None;
        }
        if self.is_zero() {
            return Some(Decimal::from_i64(0));
        }
        let digits = |x: &BigInt| x.magnitude().to_str_radix(10).len() as i64;
        // Scale the dividend so the quotient carries enough digits.
        let k = (DIV_DIGITS as i64 + digits(&o.mant) - digits(&self.mant)).max(0);
        let num = &self.mant * pow10(k as u32);
        let (mut q, r) = num.div_rem(&o.mant);
        if !r.is_zero() {
            // Round half-even on the discarded remainder.
            let twice = r.abs() * 2u32;
            let divisor = o.mant.abs();
            let away = match twice.cmp(&divisor) {
                Ordering::Greater => true,
                Ordering::Equal => q.is_odd(),
                Ordering::Less => false,
            };
            if away {
                q += if (self.mant.sign() == Sign::Minus) != (o.mant.sign() == Sign::Minus) { -1 } else { 1 };
            }
        }
        Some(Decimal::from_parts(q, k + self.scale as i64 - o.scale as i64))
    }

    /// The integer part of `self / o` (truncated toward zero).
    pub fn div_trunc(&self, o: &Decimal) -> Option<Decimal> {
        if o.is_zero() {
            return None;
        }
        let s = self.scale.max(o.scale);
        Some(Decimal::from_parts(self.scaled_to(s) / o.scaled_to(s), 0))
    }

    /// The remainder of `self / o`, with the sign of `self`.
    pub fn rem(&self, o: &Decimal) -> Option<Decimal> {
        if o.is_zero() {
            return None;
        }
        let s = self.scale.max(o.scale);
        Some(Decimal::from_parts(self.scaled_to(s) % o.scaled_to(s), s as i64))
    }

    /// Drops the fractional part.
    pub fn trunc(&self) -> Decimal {
        if self.scale == 0 {
            return self.clone();
        }
        Decimal { mant: &self.mant / pow10(self.scale), scale: 0 }
    }

    /// The largest whole number not above `self`.
    pub fn floor(&self) -> Decimal {
        if self.scale == 0 {
            return self.clone();
        }
        Decimal { mant: self.mant.div_floor(&pow10(self.scale)), scale: 0 }
    }

    /// Rounds to `digits` places after the point, ties to even.
    pub fn round(&self, digits: u32) -> Decimal {
        if self.scale <= digits {
            return self.clone();
        }
        let unit = pow10(self.scale - digits);
        let (mut q, r) = self.mant.div_rem(&unit);
        let twice = r.abs() * 2u32;
        let away = match twice.cmp(&unit) {
            Ordering::Greater => true,
            Ordering::Equal => q.is_odd(),
            Ordering::Less => false,
        };
        if away {
            q += if self.is_negative() { -1 } else { 1 };
        }
        Decimal::from_parts(q, digits as i64)
    }

    /// `self` raised to a non-negative whole power, exactly. `None` if the
    /// result would be unreasonably large.
    pub fn powi(&self, e: u32) -> Option<Decimal> {
        let bits = self.mant.bits().max(1);
        if bits.saturating_mul(e as u64) > 4_000_000 {
            return None;
        }
        Some(Decimal::from_parts(self.mant.pow(e), self.scale as i64 * e as i64))
    }

    /// Number of digits after the decimal point.
    pub fn scale(&self) -> u32 {
        self.scale
    }
}

impl Ord for Decimal {
    fn cmp(&self, o: &Decimal) -> Ordering {
        if self.scale == o.scale {
            return self.mant.cmp(&o.mant);
        }
        let (a, b) = (self.mant.sign(), o.mant.sign());
        if a != b {
            return (a as i8).cmp(&(b as i8));
        }
        let s = self.scale.max(o.scale);
        self.scaled_to(s).cmp(&o.scaled_to(s))
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, o: &Decimal) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.mant.magnitude().to_str_radix(10);
        if self.is_negative() {
            f.write_str("-")?;
        }
        let scale = self.scale as usize;
        if scale == 0 {
            f.write_str(&digits)
        } else if digits.len() <= scale {
            write!(f, "0.{}{digits}", "0".repeat(scale - digits.len()))
        } else {
            let (int, frac) = digits.split_at(digits.len() - scale);
            write!(f, "{int}.{frac}")
        }
    }
}

// On disk a decimal is `(mantissa, scale)`; reading re-canonicalises so a
// damaged or foreign file can never introduce a non-canonical value.
impl Serialize for Decimal {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        (&self.mant, self.scale).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Decimal {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Decimal, D::Error> {
        let (mant, scale) = <(BigInt, u32)>::deserialize(d)?;
        Ok(Decimal::from_parts(mant, scale as i64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::parse(s).unwrap_or_else(|| panic!("bad literal {s}"))
    }

    #[test]
    fn parse_and_display_round_trip() {
        for (input, shown) in [
            ("0", "0"),
            ("-0.0", "0"),
            ("12", "12"),
            ("3.1400", "3.14"),
            (".5", "0.5"),
            ("-.05", "-0.05"),
            ("1e3", "1000"),
            ("1.5E-3", "0.0015"),
            ("123456789012345678901234567890.123456789", "123456789012345678901234567890.123456789"),
        ] {
            assert_eq!(d(input).to_string(), shown, "{input}");
        }
        for bad in ["", "-", ".", "1.2.3", "abc", "1e", "1e999999999", " 1"] {
            assert!(Decimal::parse(bad).is_none(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn equal_values_are_identical() {
        assert_eq!(d("2.50"), d("2.5"));
        assert_eq!(d("100"), d("1e2"));
        let h = |x: &Decimal| {
            use std::hash::{Hash, Hasher};
            let mut s = std::collections::hash_map::DefaultHasher::new();
            x.hash(&mut s);
            s.finish()
        };
        assert_eq!(h(&d("0.10")), h(&d("0.1")));
    }

    #[test]
    fn exact_arithmetic() {
        assert_eq!(d("0.1").add(&d("0.2")), d("0.3"));
        assert_eq!(d("1.10").mul(&d("1.10")).to_string(), "1.21");
        assert_eq!(d("5").sub(&d("7.25")).to_string(), "-2.25");
        // Far beyond 64 bits.
        let big = d("99999999999999999999999999999999");
        assert_eq!(big.add(&d("1")).to_string(), "100000000000000000000000000000000");
        assert_eq!(big.mul(&big).to_string(), "9999999999999999999999999999999800000000000000000000000000000001");
    }

    #[test]
    fn division() {
        assert_eq!(d("1").div(&d("4")).unwrap().to_string(), "0.25");
        assert_eq!(d("10").div(&d("4")).unwrap().to_string(), "2.5");
        assert_eq!(d("-7").div(&d("2")).unwrap().to_string(), "-3.5");
        assert!(d("1").div(&d("0")).is_none());
        let third = d("1").div(&d("3")).unwrap().to_string();
        assert_eq!(third, format!("0.{}", "3".repeat(DIV_DIGITS)));
        let two_thirds = d("2").div(&d("3")).unwrap().to_string();
        assert_eq!(two_thirds, format!("0.{}7", "6".repeat(DIV_DIGITS - 1)));
        assert_eq!(d("1").div(&d("3")).unwrap().mul(&d("3")).round(40), d("1"));
    }

    #[test]
    fn integer_division_and_remainder() {
        assert_eq!(d("7").div_trunc(&d("2")).unwrap(), d("3"));
        assert_eq!(d("-7").div_trunc(&d("2")).unwrap(), d("-3"));
        assert_eq!(d("7").rem(&d("4")).unwrap(), d("3"));
        assert_eq!(d("-7").rem(&d("4")).unwrap(), d("-3"));
        assert_eq!(d("7.5").rem(&d("2")).unwrap(), d("1.5"));
        assert!(d("1").rem(&d("0")).is_none());
    }

    #[test]
    fn rounding() {
        assert_eq!(d("2.5").round(0), d("2"));
        assert_eq!(d("3.5").round(0), d("4"));
        assert_eq!(d("-2.5").round(0), d("-2"));
        assert_eq!(d("1.005").round(2), d("1"));
        assert_eq!(d("1.015").round(2), d("1.02"));
        assert_eq!(d("2.567").round(2), d("2.57"));
        assert_eq!(d("-2.567").floor(), d("-3"));
        assert_eq!(d("-2.567").trunc(), d("-2"));
        assert_eq!(d("2.567").floor(), d("2"));
    }

    #[test]
    fn ordering() {
        assert!(d("-1") < d("0.5"));
        assert!(d("0.5") < d("0.51"));
        assert!(d("-0.51") < d("-0.5"));
        assert!(d("1e30") > d("999999999999999999999999999999"));
        assert_eq!(d("2.0").cmp(&d("2")), Ordering::Equal);
    }

    #[test]
    fn powers_and_conversions() {
        assert_eq!(d("2").powi(100).unwrap().to_string(), "1267650600228229401496703205376");
        assert_eq!(d("1.1").powi(2).unwrap(), d("1.21"));
        assert!(d("99").powi(10_000_000).is_none());
        assert_eq!(Decimal::from_f64(0.1).unwrap(), d("0.1"));
        assert_eq!(Decimal::from_f64(4.0).unwrap(), d("4"));
        assert!(Decimal::from_f64(f64::NAN).is_none());
        assert_eq!(d("2.5").to_f64(), 2.5);
        assert_eq!(d("42").to_i64(), Some(42));
        assert_eq!(d("4.2").to_i64(), None);
        assert_eq!(d("99999999999999999999").to_i64(), None);
    }

    #[test]
    fn serialization_recanonicalises() {
        let bytes = postcard::to_allocvec(&d("12.340")).unwrap();
        assert_eq!(postcard::from_bytes::<Decimal>(&bytes).unwrap(), d("12.34"));
        // A non-canonical encoding (trailing zeros) still reads back canonical.
        let raw = postcard::to_allocvec(&(BigInt::from(1200), 2u32)).unwrap();
        assert_eq!(postcard::from_bytes::<Decimal>(&raw).unwrap(), d("12"));
    }
}
