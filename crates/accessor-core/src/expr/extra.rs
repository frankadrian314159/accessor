//! Helpers for the regular-expression, string-padding and truncation functions.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use num_bigint::BigInt;
use regex::{Regex, RegexBuilder};

use crate::decimal::Decimal;
use crate::error::{Error, Result};

type PatternCache = RefCell<HashMap<(String, String), Rc<Regex>>>;

// Compiled patterns, so that a pattern used for every row is compiled once.
thread_local! {
    static PATTERNS: PatternCache = RefCell::new(HashMap::new());
}

/// A regular expression. Like `LIKE` and text comparison in ASQL, matching ignores case unless the
/// flags say otherwise: `c` (case-sensitive), `i` (ignore case), `n` (a dot also matches a line
/// break), `m` (`^` and `$` match at each line).
pub(crate) fn pattern(pattern: &str, flags: &str) -> Result<Rc<Regex>> {
    let key = (pattern.to_string(), flags.to_string());
    if let Some(r) = PATTERNS.with(|c| c.borrow().get(&key).cloned()) {
        return Ok(r);
    }
    let mut b = RegexBuilder::new(pattern);
    let mut ignore_case = true;
    for f in flags.chars() {
        match f {
            'c' => ignore_case = false,
            'i' => ignore_case = true,
            'n' | 's' => {
                b.dot_matches_new_line(true);
            }
            'm' => {
                b.multi_line(true);
            }
            other => return Err(Error::runtime(format!("'{other}' is not a regular-expression flag (use c, i, n or m)"))),
        }
    }
    b.case_insensitive(ignore_case);
    b.size_limit(10 * 1024 * 1024);
    let re = Rc::new(b.build().map_err(|e| Error::runtime(format!("not a valid regular expression: {e}")))?);
    PATTERNS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= 128 {
            c.clear();
        }
        c.insert(key, Rc::clone(&re));
    });
    Ok(re)
}

/// The regular expression a `SIMILAR TO` pattern stands for: `%` and `_` as in `LIKE`, with `|`, `*`,
/// `+`, `?`, `{m,n}`, `( )` and `[ ]` as in a regular expression, matching the whole text.
pub(crate) fn similar_to_regex(p: &str) -> String {
    let mut out = String::from("(?s)^(?:");
    let mut in_class = false;
    for c in p.chars() {
        match c {
            '[' => {
                in_class = true;
                out.push(c);
            }
            ']' => {
                in_class = false;
                out.push(c);
            }
            '%' if !in_class => out.push_str(".*"),
            '_' if !in_class => out.push('.'),
            '.' | '^' | '$' | '\\' if !in_class => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push_str(")$");
    out
}

/// `$1` and `${name}` are the groups of a match in a replacement; so are `\1` ... `\9`.
pub(crate) fn replacement(r: &str) -> String {
    let mut out = String::new();
    let mut chars = r.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek() {
                Some(d) if d.is_ascii_digit() => {
                    out.push_str("${");
                    out.push(chars.next().expect("peeked"));
                    out.push('}');
                }
                Some('\\') => {
                    chars.next();
                    out.push('\\');
                }
                _ => out.push('\\'),
            },
            '$' => match chars.peek() {
                Some(d) if d.is_ascii_digit() => {
                    out.push_str("${");
                    while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                        out.push(*d);
                        chars.next();
                    }
                    out.push('}');
                }
                Some('{') => out.push('$'),
                _ => out.push_str("$$"),
            },
            c => out.push(c),
        }
    }
    out
}

/// The longest text a function may build, in characters.
const MAX_TEXT: usize = 100_000_000;

pub(crate) fn pad(s: &str, width: i64, fill: &str, left: bool) -> Result<String> {
    if width <= 0 {
        return Ok(String::new());
    }
    let width = width as usize;
    if width > MAX_TEXT {
        return Err(Error::runtime("the padded text would be too long"));
    }
    let len = s.chars().count();
    if len >= width {
        return Ok(s.chars().take(width).collect());
    }
    if fill.is_empty() {
        return Ok(s.to_string());
    }
    let padding: String = fill.chars().cycle().take(width - len).collect();
    Ok(if left { format!("{padding}{s}") } else { format!("{s}{padding}") })
}

pub(crate) fn repeat(s: &str, n: i64) -> Result<String> {
    if n <= 0 || s.is_empty() {
        return Ok(String::new());
    }
    if (s.chars().count() as u128) * (n as u128) > MAX_TEXT as u128 {
        return Err(Error::runtime("the repeated text would be too long"));
    }
    Ok(s.repeat(n as usize))
}

pub(crate) fn overlay(s: &str, with: &str, start: i64, len: Option<i64>) -> String {
    let chars: Vec<char> = s.chars().collect();
    let at = ((start.max(1) - 1) as usize).min(chars.len());
    let cut = len.unwrap_or_else(|| with.chars().count() as i64).max(0) as usize;
    let end = at.saturating_add(cut).min(chars.len());
    let mut out: String = chars[..at].iter().collect();
    out.push_str(with);
    out.extend(chars[end..].iter());
    out
}

pub(crate) fn translate(s: &str, from: &str, to: &str) -> String {
    let to: Vec<char> = to.chars().collect();
    s.chars()
        .filter_map(|c| match from.chars().position(|f| f == c) {
            None => Some(c),
            Some(i) => to.get(i).copied(),
        })
        .collect()
}

pub(crate) fn initcap(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            start = false;
        } else {
            out.push(c);
            start = true;
        }
    }
    out
}

/// `d` cut off (not rounded) after `digits` places; a negative number of places cuts off whole
/// digits to the left of the point.
pub(crate) fn truncate(d: &Decimal, digits: i64) -> Decimal {
    let (mant, scale) = d.parts();
    if digits >= scale as i64 {
        return d.clone();
    }
    if digits >= 0 {
        let k = (scale as i64 - digits) as u32;
        return Decimal::from_parts(mant / BigInt::from(10u32).pow(k), digits);
    }
    // Whole digits: take the whole part, then zero its last -digits digits.
    let whole = mant / BigInt::from(10u32).pow(scale);
    let unit = BigInt::from(10u32).pow((-digits).min(10_000) as u32);
    Decimal::from_parts((whole / &unit) * unit, 0)
}

/// `RANDOM()`: a number in [0, 1).
pub(crate) fn random() -> f64 {
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut s = STATE.load(Ordering::Relaxed);
    if s == 0 {
        s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x9E37_79B9, |d| d.as_nanos() as u64) | 1;
    }
    // splitmix64
    s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    STATE.store(s, Ordering::Relaxed);
    let mut z = s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_and_friends() {
        assert_eq!(pad("ab", 5, "xy", true).unwrap(), "xyxab");
        assert_eq!(pad("ab", 5, " ", false).unwrap(), "ab   ");
        assert_eq!(pad("abcdef", 3, "x", true).unwrap(), "abc");
        assert_eq!(repeat("ab", 3).unwrap(), "ababab");
        assert_eq!(overlay("Txxxxas", "hom", 2, Some(4)), "Thomas");
        assert_eq!(overlay("abc", "ZZ", 2, None), "aZZ");
        assert_eq!(translate("12345", "143", "ax"), "a2x5");
        assert_eq!(initcap("hELLO wORLD-of o'neil"), "Hello World-Of O'Neil");
    }

    #[test]
    fn similar_to_translates() {
        let re = pattern(&similar_to_regex("ab%(c|d)_"), "").unwrap();
        assert!(re.is_match("abXXcZ"));
        assert!(re.is_match("ABD1"));
        assert!(!re.is_match("abc"));
        assert!(!re.is_match("xabcdz"));
        let re = pattern(&similar_to_regex("a.b"), "").unwrap();
        assert!(re.is_match("a.b") && !re.is_match("axb"));
    }

    #[test]
    fn replacements_and_truncation() {
        assert_eq!(replacement("\\2-\\1 $3 $$"), "${2}-${1} ${3} $$$$");
        let d = Decimal::parse("-123.4567").unwrap();
        assert_eq!(truncate(&d, 2).to_string(), "-123.45");
        assert_eq!(truncate(&d, 0).to_string(), "-123");
        assert_eq!(truncate(&d, -1).to_string(), "-120");
        assert_eq!(truncate(&d, 9).to_string(), "-123.4567");
    }

    #[test]
    fn random_is_in_range() {
        for _ in 0..1000 {
            let r = random();
            assert!((0.0..1.0).contains(&r));
        }
    }
}
