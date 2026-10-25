//! The `INTERVAL` type: a length of time kept as months, days and seconds.
//!
//! Months and days are not fixed numbers of seconds (a month is 28 to 31 days, and a day
//! is 23 or 25 hours when the clocks change), so they are kept apart: adding "1 month" to
//! 31 January gives the end of February, and adding "1 day" to a zoned timestamp keeps the
//! wall-clock time across a daylight-saving change. Only the seconds are exact elapsed time.
//!
//! For comparison, sorting and equality a month counts as 30 days and a day as 24 hours,
//! so `INTERVAL '1 month' = INTERVAL '30 days'`.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::datetime;
use crate::decimal::Decimal;

#[derive(Copy, Clone, Debug, Default, Serialize, Deserialize)]
pub struct Interval {
    pub months: i64,
    pub days: i64,
    pub secs: i64,
}

/// A field of an interval literal such as `INTERVAL '5' DAY`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Unit {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

impl Unit {
    pub fn from_word(w: &str) -> Option<Unit> {
        Some(match w.to_ascii_lowercase().as_str() {
            "year" | "years" | "yr" | "yrs" | "y" => Unit::Year,
            "month" | "months" | "mon" | "mons" => Unit::Month,
            "day" | "days" | "d" => Unit::Day,
            "hour" | "hours" | "hr" | "hrs" | "h" => Unit::Hour,
            "minute" | "minutes" | "min" | "mins" => Unit::Minute,
            "second" | "seconds" | "sec" | "secs" | "s" => Unit::Second,
            _ => return None,
        })
    }

    const ORDER: [Unit; 6] = [Unit::Year, Unit::Month, Unit::Day, Unit::Hour, Unit::Minute, Unit::Second];
}

const DAY: i128 = 86_400;

impl Interval {
    pub const ZERO: Interval = Interval { months: 0, days: 0, secs: 0 };

    pub fn new(months: i64, days: i64, secs: i64) -> Interval {
        Interval { months, days, secs }
    }

    /// The length in seconds, taking a month as 30 days.
    pub fn total_secs(&self) -> i128 {
        self.months as i128 * 30 * DAY + self.days as i128 * DAY + self.secs as i128
    }

    pub fn is_zero(&self) -> bool {
        self.months == 0 && self.days == 0 && self.secs == 0
    }

    pub fn neg(&self) -> Option<Interval> {
        Some(Interval { months: self.months.checked_neg()?, days: self.days.checked_neg()?, secs: self.secs.checked_neg()? })
    }

    pub fn add(&self, o: &Interval) -> Option<Interval> {
        Some(Interval {
            months: self.months.checked_add(o.months)?,
            days: self.days.checked_add(o.days)?,
            secs: self.secs.checked_add(o.secs)?,
        })
    }

    pub fn sub(&self, o: &Interval) -> Option<Interval> {
        self.add(&o.neg()?)
    }

    /// Multiplies by a number. A fraction of a month carries into days (30 to the month),
    /// and a fraction of a day into seconds.
    pub fn scale(&self, k: &Decimal) -> Option<Interval> {
        let split = |d: Decimal| -> (Decimal, Decimal) {
            let whole = d.trunc();
            let frac = d.sub(&whole);
            (whole, frac)
        };
        let (months, frac_m) = split(Decimal::from_i64(self.months).mul(k));
        let (days, frac_d) = split(Decimal::from_i64(self.days).mul(k).add(&frac_m.mul(&Decimal::from_i64(30))));
        let secs = Decimal::from_i64(self.secs).mul(k).add(&frac_d.mul(&Decimal::from_i64(DAY as i64))).round(0);
        Some(Interval { months: months.to_i64()?, days: days.to_i64()?, secs: secs.to_i64()? })
    }

    /// An interval from a number of days, a fraction becoming seconds.
    pub fn from_days(d: &Decimal) -> Option<Interval> {
        Interval::new(0, 1, 0).scale(d)
    }

    /// The interval as a number of days (a month as 30), with fractions.
    pub fn to_days(&self) -> Decimal {
        Decimal::from_i64(self.secs).div(&Decimal::from_i64(DAY as i64)).unwrap_or_else(|| Decimal::from_i64(0)).add(&Decimal::from_i64(self.days + self.months * 30))
    }

    /// Adds `sign` times this interval to a wall-clock reading: months first (clamping to the
    /// end of a shorter month), then days, then seconds.
    pub fn add_to_local(&self, local: i64, sign: i64) -> Option<i64> {
        let moved = if self.months == 0 { local } else { datetime::add_months(local, self.months.checked_mul(sign)?)? };
        moved.checked_add(self.days.checked_mul(sign)?.checked_mul(86_400)?)?.checked_add(self.secs.checked_mul(sign)?)
    }

    /// The `AGE` of `a` over `b`: whole years/months/days and a time of day, as people say it.
    pub fn age(a: i64, b: i64) -> Interval {
        if a < b {
            return Interval::age(b, a).neg().unwrap_or_default();
        }
        let (ya, ma, da, ..) = datetime::components(a);
        let (yb, mb, db, ..) = datetime::components(b);
        let mut months = (ya - yb) * 12 + ma as i64 - mb as i64;
        let mut days = da as i64 - db as i64;
        let mut tod = a.rem_euclid(86_400) - b.rem_euclid(86_400);
        if tod < 0 {
            tod += 86_400;
            days -= 1;
        }
        if days < 0 {
            months -= 1;
            // Borrow the length of the month before the later date's.
            let (py, pm) = if ma == 1 { (ya - 1, 12) } else { (ya, ma - 1) };
            days += datetime::days_in_month(py, pm) as i64;
        }
        Interval { months, days, secs: tod }
    }

    // ----- reading text ---------------------------------------------------------------

    /// Reads an interval from text: `3 days`, `1 year 2 months`, `-5 minutes`, `04:05:06`,
    /// `1 day 02:30`, `1-6` (years-months) or ISO 8601 (`P1Y2M3DT4H5M6S`). With `qualifier`
    /// (`INTERVAL '5' DAY`, `'1-6' YEAR TO MONTH`) bare numbers are read in those fields.
    pub fn parse(text: &str, qualifier: Option<(Unit, Option<Unit>)>) -> Result<Interval, String> {
        let bad = || format!("'{text}' is not a valid interval");
        let t = text.trim();
        if t.is_empty() {
            return Err(bad());
        }
        if let Some((from, to)) = qualifier {
            if let Some(iv) = parse_qualified(t, from, to) {
                return Ok(iv);
            }
        }
        if t.starts_with(['P', 'p']) && t.len() > 1 {
            return parse_iso(&t[1..]).ok_or_else(bad);
        }
        parse_words(t).ok_or_else(bad)
    }
}

impl PartialEq for Interval {
    fn eq(&self, other: &Self) -> bool {
        self.total_secs() == other.total_secs()
    }
}
impl Eq for Interval {}
impl PartialOrd for Interval {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Interval {
    fn cmp(&self, other: &Self) -> Ordering {
        self.total_secs().cmp(&other.total_secs())
    }
}
impl Hash for Interval {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.total_secs().hash(state);
    }
}

impl fmt::Display for Interval {
    /// `1 year 2 months 3 days 04:05:06`; each part carries its own sign.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = Vec::new();
        let plural = |n: i64, one: &str, many: &str| format!("{n} {}", if n.abs() == 1 { one } else { many });
        let (years, months) = (self.months / 12, self.months % 12);
        if years != 0 {
            parts.push(plural(years, "year", "years"));
        }
        if months != 0 {
            parts.push(plural(months, "month", "months"));
        }
        if self.days != 0 {
            parts.push(plural(self.days, "day", "days"));
        }
        if self.secs != 0 || parts.is_empty() {
            let a = self.secs.abs();
            let sign = if self.secs < 0 { "-" } else { "" };
            parts.push(format!("{sign}{:02}:{:02}:{:02}", a / 3600, a % 3600 / 60, a % 60));
        }
        f.write_str(&parts.join(" "))
    }
}

// ----- parsing helpers -----------------------------------------------------------------

/// `n` units as an interval; `None` if the amount is not a number or is out of range.
fn amount(unit: Unit, n: &Decimal) -> Option<Interval> {
    let one = match unit {
        Unit::Year => Interval::new(12, 0, 0),
        Unit::Month => Interval::new(1, 0, 0),
        Unit::Day => Interval::new(0, 1, 0),
        Unit::Hour => Interval::new(0, 0, 3600),
        Unit::Minute => Interval::new(0, 0, 60),
        Unit::Second => Interval::new(0, 0, 1),
    };
    one.scale(n)
}

fn num(s: &str) -> Option<Decimal> {
    Decimal::parse(s.strip_prefix('+').unwrap_or(s))
}

/// `[sign]hh:mm[:ss[.fff]]` as seconds.
fn clock(tok: &str) -> Option<i64> {
    let (neg, body) = match tok.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, tok.strip_prefix('+').unwrap_or(tok)),
    };
    let mut it = body.split(':');
    let h: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let s = match it.next() {
        Some(s) => num(s)?.round(0).to_i64()?,
        None => 0,
    };
    if it.next().is_some() || !(0..60).contains(&m) || !(0..60).contains(&s) {
        return None;
    }
    let total = h * 3600 + m * 60 + s;
    Some(if neg { -total } else { total })
}

fn parse_words(t: &str) -> Option<Interval> {
    let toks: Vec<&str> = t.split_whitespace().collect();
    let mut total = Interval::ZERO;
    let mut i = 0;
    let mut saw = false;
    while i < toks.len() {
        let tok = toks[i];
        if tok.contains(':') {
            total = total.add(&Interval::new(0, 0, clock(tok)?))?;
            i += 1;
        } else if let (Some((y, m)), true) = (tok.split_once('-').filter(|(a, _)| !a.is_empty()), tok.len() > 1) {
            // years-months
            let (neg, y) = match y.strip_prefix('+') {
                Some(r) => (false, r),
                None => (false, y),
            };
            let (y, m): (i64, i64) = (y.parse().ok()?, m.parse().ok()?);
            let v = Interval::new(y * 12 + m, 0, 0);
            total = total.add(&if neg { v.neg()? } else { v })?;
            i += 1;
        } else {
            // A number, with its unit attached ("3days") or in the next word.
            let split = tok.find(|c: char| c.is_ascii_alphabetic());
            let (digits, attached) = match split {
                Some(p) => (&tok[..p], Some(&tok[p..])),
                None => (tok, None),
            };
            let n = num(digits)?;
            let (word, used) = match attached {
                Some(w) => (w, 1),
                None => (*toks.get(i + 1)?, 2),
            };
            let unit_word = word.to_ascii_lowercase();
            let iv = match unit_word.as_str() {
                "week" | "weeks" | "w" => Interval::new(0, 7, 0).scale(&n)?,
                other => amount(Unit::from_word(other)?, &n)?,
            };
            total = total.add(&iv)?;
            i += used;
        }
        saw = true;
    }
    saw.then_some(total)
}

fn parse_iso(body: &str) -> Option<Interval> {
    let mut total = Interval::ZERO;
    let mut in_time = false;
    let mut digits = String::new();
    for c in body.chars() {
        match c {
            'T' | 't' => in_time = true,
            '0'..='9' | '.' | '-' | '+' => digits.push(c),
            letter => {
                let n = num(&digits)?;
                digits.clear();
                let iv = match (letter.to_ascii_uppercase(), in_time) {
                    ('Y', false) => amount(Unit::Year, &n)?,
                    ('M', false) => amount(Unit::Month, &n)?,
                    ('W', false) => Interval::new(0, 7, 0).scale(&n)?,
                    ('D', false) => amount(Unit::Day, &n)?,
                    ('H', true) => amount(Unit::Hour, &n)?,
                    ('M', true) => amount(Unit::Minute, &n)?,
                    ('S', true) => amount(Unit::Second, &n)?,
                    _ => return None,
                };
                total = total.add(&iv)?;
            }
        }
    }
    digits.is_empty().then_some(total)
}

/// Text read in the fields named by a qualifier: `'5' DAY`, `'1-6' YEAR TO MONTH`,
/// `'2 03:04:05' DAY TO SECOND`.
fn parse_qualified(t: &str, from: Unit, to: Option<Unit>) -> Option<Interval> {
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b.trim_start()),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let first = Unit::ORDER.iter().position(|u| *u == from)?;
    let last = match to {
        Some(u) => Unit::ORDER.iter().position(|x| *x == u)?,
        None => first,
    };
    if last < first {
        return None;
    }
    let fields = &Unit::ORDER[first..=last];
    let tokens: Vec<&str> = body.split([' ', ':', '-']).filter(|s| !s.is_empty()).collect();
    if tokens.len() != fields.len() {
        return None;
    }
    let mut total = Interval::ZERO;
    for (u, tok) in fields.iter().zip(tokens) {
        total = total.add(&amount(*u, &num(tok)?)?)?;
    }
    if neg {
        total.neg()
    } else {
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Interval {
        Interval::parse(s, None).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn reading_and_showing() {
        assert_eq!(p("1 year 2 months 3 days 04:05:06").to_string(), "1 year 2 months 3 days 04:05:06");
        assert_eq!(p("3 days"), Interval::new(0, 3, 0));
        assert_eq!(p("2 weeks"), Interval::new(0, 14, 0));
        assert_eq!(p("90 minutes").to_string(), "01:30:00");
        assert_eq!(p("-5 hours"), Interval::new(0, 0, -18_000));
        assert_eq!(p("1.5 hours"), Interval::new(0, 0, 5400));
        assert_eq!(p("1-6"), Interval::new(18, 0, 0));
        assert_eq!(p("P1Y2M3DT4H5M6S"), Interval::new(14, 3, 14_706));
        assert_eq!(p("0.5 days"), Interval::new(0, 0, 43_200));
        assert_eq!(Interval::ZERO.to_string(), "00:00:00");
        assert!(Interval::parse("soon", None).is_err());
        assert_eq!(Interval::parse("5", Some((Unit::Day, None))).unwrap(), Interval::new(0, 5, 0));
        assert_eq!(Interval::parse("1-6", Some((Unit::Year, Some(Unit::Month)))).unwrap(), Interval::new(18, 0, 0));
        assert_eq!(Interval::parse("2 03:04:05", Some((Unit::Day, Some(Unit::Second)))).unwrap(), Interval::new(0, 2, 11_045));
    }

    #[test]
    fn comparison_and_arithmetic() {
        assert_eq!(p("1 month"), p("30 days"));
        assert!(p("25 hours") > p("1 day"));
        assert_eq!(p("1 day").scale(&Decimal::parse("2.5").unwrap()), Some(Interval::new(0, 2, 43_200)));
        assert_eq!(p("1 month").scale(&Decimal::parse("0.5").unwrap()), Some(Interval::new(0, 15, 0)));
    }

    #[test]
    fn age_in_calendar_terms() {
        let t = |s: &str| datetime::parse(s).unwrap();
        assert_eq!(Interval::age(t("2024-03-05"), t("2021-01-10")), Interval::new(37, 24, 0));
        assert_eq!(Interval::age(t("2024-03-05 10:00:00"), t("2024-03-04 12:00:00")), Interval::new(0, 0, 79_200));
        assert_eq!(Interval::age(t("2021-01-10"), t("2024-03-05")), Interval::new(-37, -24, 0));
    }
}
