//! Exact fractions of arbitrary-size integers.
//!
//! A [`Rational`] is `numerator / denominator` in lowest terms with a positive
//! denominator, so equal fractions are identical and derive `Eq`/`Hash`.

use std::cmp::Ordering;
use std::fmt;

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{One, Signed, ToPrimitive, Zero};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::decimal::Decimal;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rational {
    num: BigInt,
    den: BigInt,
}

impl Rational {
    /// `num / den` in lowest terms; `None` when `den` is zero.
    pub fn new(num: BigInt, den: BigInt) -> Option<Rational> {
        if den.is_zero() {
            return None;
        }
        let (mut num, mut den) = if den.sign() == Sign::Minus { (-num, -den) } else { (num, den) };
        let g = num.gcd(&den);
        if !g.is_one() && !g.is_zero() {
            num /= &g;
            den /= &g;
        }
        if num.is_zero() {
            den = BigInt::one();
        }
        Some(Rational { num, den })
    }

    pub fn from_int(n: BigInt) -> Rational {
        Rational { num: n, den: BigInt::one() }
    }

    pub fn from_i64(n: i64) -> Rational {
        Rational::from_int(BigInt::from(n))
    }

    /// The exact value of a decimal.
    pub fn from_decimal(d: &Decimal) -> Rational {
        let (mant, scale) = d.parts();
        Rational::new(mant, BigInt::from(10u32).pow(scale)).expect("a power of ten is not zero")
    }

    /// The exact binary value of a finite float. `None` for NaN and infinities.
    pub fn from_f64_exact(f: f64) -> Option<Rational> {
        if !f.is_finite() {
            return None;
        }
        let bits = f.to_bits();
        let sign = if bits >> 63 == 0 { 1 } else { -1 };
        let exp = ((bits >> 52) & 0x7ff) as i64;
        let frac = (bits & 0xf_ffff_ffff_ffff) as i64;
        let (mant, exp) = if exp == 0 { (frac, -1074) } else { (frac | (1 << 52), exp - 1075) };
        let mant = BigInt::from(sign * mant);
        Some(if exp >= 0 {
            Rational::from_int(mant << exp as usize)
        } else {
            Rational::new(mant, BigInt::one() << (-exp) as usize).expect("a power of two is not zero")
        })
    }

    pub fn numer(&self) -> &BigInt {
        &self.num
    }

    pub fn denom(&self) -> &BigInt {
        &self.den
    }

    pub fn is_integer(&self) -> bool {
        self.den.is_one()
    }

    pub fn is_zero(&self) -> bool {
        self.num.is_zero()
    }

    pub fn is_negative(&self) -> bool {
        self.num.sign() == Sign::Minus
    }

    pub fn neg(&self) -> Rational {
        Rational { num: -self.num.clone(), den: self.den.clone() }
    }

    pub fn abs(&self) -> Rational {
        Rational { num: self.num.abs(), den: self.den.clone() }
    }

    pub fn add(&self, o: &Rational) -> Rational {
        Rational::new(&self.num * &o.den + &o.num * &self.den, &self.den * &o.den).expect("denominators are not zero")
    }

    pub fn sub(&self, o: &Rational) -> Rational {
        Rational::new(&self.num * &o.den - &o.num * &self.den, &self.den * &o.den).expect("denominators are not zero")
    }

    pub fn mul(&self, o: &Rational) -> Rational {
        Rational::new(&self.num * &o.num, &self.den * &o.den).expect("denominators are not zero")
    }

    /// `None` when `o` is zero.
    pub fn div(&self, o: &Rational) -> Option<Rational> {
        Rational::new(&self.num * &o.den, &self.den * &o.num)
    }

    /// `self` raised to a whole power (negative powers invert). `None` for
    /// zero to a negative power, or a result that would be unreasonably large.
    pub fn powi(&self, e: i64) -> Option<Rational> {
        let n = u32::try_from(e.unsigned_abs()).ok()?;
        let bits = self.num.bits().max(self.den.bits()).max(1);
        if bits.saturating_mul(n as u64) > 4_000_000 {
            return None;
        }
        let p = Rational::new(self.num.pow(n), self.den.pow(n))?;
        if e < 0 {
            Rational::from_int(BigInt::one()).div(&p)
        } else {
            Some(p)
        }
    }

    /// The largest integer not above `self`.
    pub fn floor(&self) -> BigInt {
        self.num.div_floor(&self.den)
    }

    /// The integer part (toward zero).
    pub fn trunc(&self) -> BigInt {
        &self.num / &self.den
    }

    /// The nearest integer, ties to even.
    pub fn round(&self) -> BigInt {
        let two = BigInt::from(2u32);
        let floor = self.floor();
        let frac = self.sub(&Rational::from_int(floor.clone())); // in [0, 1)
        match (&frac.num * &two).cmp(&frac.den) {
            Ordering::Less => floor,
            Ordering::Greater => floor + 1,
            Ordering::Equal => {
                if floor.is_even() {
                    floor
                } else {
                    floor + 1
                }
            }
        }
    }

    /// The nearest `f64` (ties to even); infinite when too large.
    pub fn to_f64(&self) -> f64 {
        if self.num.is_zero() {
            return 0.0;
        }
        let negative = self.is_negative();
        let num = self.num.abs();
        // Scale so the integer quotient carries at least 64 significant bits.
        let shift = 64 - (num.bits() as i64 - self.den.bits() as i64);
        let (scaled_num, scaled_den) = if shift >= 0 {
            (num << shift as usize, self.den.clone())
        } else {
            (num, &self.den << (-shift) as usize)
        };
        let (mut q, r) = scaled_num.div_rem(&scaled_den);
        if !r.is_zero() {
            // A sticky bit far below the rounding position keeps the final rounding honest.
            q |= BigInt::one();
        }
        let mut f = q.to_f64().unwrap_or(f64::INFINITY);
        let mut exp = -shift;
        while exp != 0 && f != 0.0 && f.is_finite() {
            let step = exp.clamp(-1000, 1000);
            f *= 2f64.powi(step as i32);
            exp -= step;
        }
        if negative {
            -f
        } else {
            f
        }
    }

    /// As a decimal: exact when the fraction terminates, otherwise rounded to
    /// [`crate::decimal::DIV_DIGITS`] significant digits.
    pub fn to_decimal(&self) -> Decimal {
        Decimal::from_parts(self.num.clone(), 0)
            .div(&Decimal::from_parts(self.den.clone(), 0))
            .expect("the denominator is not zero")
    }

    /// Parses `7/2`, `-3`, `1.25` or `2e3`. Whitespace around `/` is allowed.
    pub fn parse(s: &str) -> Option<Rational> {
        let s = s.trim();
        match s.split_once('/') {
            Some((n, d)) => {
                let (n, d) = (n.trim(), d.trim());
                let n = Decimal::parse(n).map(|x| Rational::from_decimal(&x))?;
                let d = Decimal::parse(d).map(|x| Rational::from_decimal(&x))?;
                n.div(&d)
            }
            None => Decimal::parse(s).map(|d| Rational::from_decimal(&d)),
        }
    }
}

impl Ord for Rational {
    fn cmp(&self, o: &Rational) -> Ordering {
        if self.den == o.den {
            return self.num.cmp(&o.num);
        }
        (&self.num * &o.den).cmp(&(&o.num * &self.den))
    }
}

impl PartialOrd for Rational {
    fn partial_cmp(&self, o: &Rational) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den.is_one() {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

// On disk a fraction is `(numerator, denominator)`; reading re-reduces it so a
// damaged file cannot introduce a non-canonical value (a zero denominator
// becomes an error).
impl Serialize for Rational {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        (&self.num, &self.den).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Rational {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Rational, D::Error> {
        let (num, den) = <(BigInt, BigInt)>::deserialize(d)?;
        Rational::new(num, den).ok_or_else(|| serde::de::Error::custom("rational with a zero denominator"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(n: i64, d: i64) -> Rational {
        Rational::new(BigInt::from(n), BigInt::from(d)).unwrap()
    }

    #[test]
    fn lowest_terms_and_sign() {
        assert_eq!(r(2, 4), r(1, 2));
        assert_eq!(r(1, -2), r(-1, 2));
        assert_eq!(r(0, 5), r(0, 1));
        assert_eq!(r(6, 3).to_string(), "2");
        assert_eq!(r(-7, 2).to_string(), "-7/2");
        assert!(Rational::new(BigInt::one(), BigInt::zero()).is_none());
    }

    #[test]
    fn exact_arithmetic() {
        assert_eq!(r(1, 3).add(&r(1, 6)), r(1, 2));
        assert_eq!(r(1, 2).sub(&r(1, 3)), r(1, 6));
        assert_eq!(r(2, 3).mul(&r(3, 4)), r(1, 2));
        assert_eq!(r(1, 2).div(&r(1, 4)).unwrap(), r(2, 1));
        assert!(r(1, 2).div(&r(0, 1)).is_none());
        // Thirds really do add up to one.
        assert_eq!(r(1, 3).add(&r(1, 3)).add(&r(1, 3)), r(1, 1));
        assert_eq!(r(2, 3).powi(3).unwrap(), r(8, 27));
        assert_eq!(r(2, 3).powi(-2).unwrap(), r(9, 4));
        assert!(r(0, 1).powi(-1).is_none());
    }

    #[test]
    fn ordering_and_rounding() {
        assert!(r(1, 3) < r(1, 2));
        assert!(r(-1, 2) < r(-1, 3));
        assert_eq!(r(7, 2).floor(), BigInt::from(3));
        assert_eq!(r(-7, 2).floor(), BigInt::from(-4));
        assert_eq!(r(-7, 2).trunc(), BigInt::from(-3));
        assert_eq!(r(5, 2).round(), BigInt::from(2)); // ties to even
        assert_eq!(r(7, 2).round(), BigInt::from(4));
        assert_eq!(r(-5, 2).round(), BigInt::from(-2));
        assert_eq!(r(-7, 3).round(), BigInt::from(-2));
        assert_eq!(r(8, 3).round(), BigInt::from(3));
    }

    #[test]
    fn conversions() {
        let d = Decimal::parse("0.125").unwrap();
        assert_eq!(Rational::from_decimal(&d), r(1, 8));
        assert_eq!(r(1, 8).to_decimal(), d);
        assert_eq!(r(1, 4).to_decimal().to_string(), "0.25");
        assert_eq!(r(1, 3).to_decimal().to_string().len(), 2 + crate::decimal::DIV_DIGITS);
        assert_eq!(Rational::from_f64_exact(0.5).unwrap(), r(1, 2));
        assert_eq!(Rational::from_f64_exact(-3.0).unwrap(), r(-3, 1));
        assert_eq!(Rational::from_f64_exact(0.1).unwrap().to_f64(), 0.1);
        assert!(Rational::from_f64_exact(f64::INFINITY).is_none());
        assert_eq!(r(1, 3).to_f64(), 1.0 / 3.0);
        assert_eq!(r(-22, 7).to_f64(), -22.0 / 7.0);
        assert_eq!(Rational::from_int(BigInt::from(10).pow(30)).to_f64(), 1e30);
        assert_eq!(Rational::new(BigInt::one(), BigInt::from(10).pow(30)).unwrap().to_f64(), 1e-30);
    }

    #[test]
    fn parsing() {
        assert_eq!(Rational::parse("7/2"), Some(r(7, 2)));
        assert_eq!(Rational::parse(" -3 / 6 "), Some(r(-1, 2)));
        assert_eq!(Rational::parse("1.25"), Some(r(5, 4)));
        assert_eq!(Rational::parse("2e3"), Some(r(2000, 1)));
        assert_eq!(Rational::parse("0.5/0.25"), Some(r(2, 1)));
        for bad in ["", "/", "1/0", "a/b", "1/2/3"] {
            assert!(Rational::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn serialization_recanonicalises() {
        let bytes = postcard::to_allocvec(&r(2, 4)).unwrap();
        assert_eq!(postcard::from_bytes::<Rational>(&bytes).unwrap(), r(1, 2));
        let raw = postcard::to_allocvec(&(BigInt::from(6), BigInt::from(-4))).unwrap();
        assert_eq!(postcard::from_bytes::<Rational>(&raw).unwrap(), r(-3, 2));
        let zero = postcard::to_allocvec(&(BigInt::from(1), BigInt::from(0))).unwrap();
        assert!(postcard::from_bytes::<Rational>(&zero).is_err());
    }
}
