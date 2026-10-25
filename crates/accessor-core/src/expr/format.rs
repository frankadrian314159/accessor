//! `Format(value, pattern)`: the Access formatting function.
//!
//! Numbers are formatted from their exact decimal value, so `Format(0.125, "0.00")`
//! is `0.12` (ties to even) whatever type the number has, and money never goes
//! through a float. Dates and text have their own pattern letters.

use crate::datetime;
use crate::decimal::Decimal;
use crate::error::{Error, Result};
use crate::value::Value;

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];
const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

pub fn format_value(v: &Value, pattern: &str) -> Result<String> {
    let named = pattern.trim().to_ascii_lowercase();
    match v {
        Value::DateTime(t) => return format_date(*t, &date_pattern(&named, pattern)),
        // A zoned instant is formatted on the wall clock of the session zone.
        Value::TimestampTz(t) => return format_date(crate::tz::session().to_local(*t), &date_pattern(&named, pattern)),
        Value::Interval(i) => return Ok(i.to_string()),
        Value::Bool(b) => {
            if let Some(s) = named_bool(&named, *b) {
                return Ok(s);
            }
            if pattern.is_empty() {
                return Ok(if *b { "True" } else { "False" }.to_string());
            }
            return format_number(&Decimal::from_i64(if *b { -1 } else { 0 }), pattern);
        }
        Value::Text(s) => {
            let numeric = !pattern.contains(['>', '<', '@', '&']);
            if numeric {
                if let Some(n) = Value::parse_number(s.trim()) {
                    return format_value(&n, pattern);
                }
            }
            return Ok(format_text(s, pattern));
        }
        _ => {}
    }
    let Some(d) = v.as_decimal() else {
        return Err(Error::type_err(format!("Format: cannot format '{v}'")));
    };
    if let Some(s) = named_bool(&named, !d.is_zero()) {
        return Ok(s);
    }
    format_number(&d, pattern)
}

fn named_bool(named: &str, truth: bool) -> Option<String> {
    let (yes, no) = match named {
        "yes/no" => ("Yes", "No"),
        "true/false" => ("True", "False"),
        "on/off" => ("On", "Off"),
        _ => return None,
    };
    Some(if truth { yes } else { no }.to_string())
}

// ---------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------

fn number_pattern(named: &str, original: &str) -> String {
    match named {
        "currency" => "$#,##0.00;($#,##0.00)".into(),
        "fixed" => "0.00".into(),
        "standard" => "#,##0.00".into(),
        "percent" => "0.00%".into(),
        "scientific" => "0.00E+00".into(),
        _ => original.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum NumTok {
    Lit(String),
    /// `0` (true) or `#` (false).
    Digit(bool),
    Point,
    Comma,
    Percent,
    /// `E+`/`e+` (true) or `E-`/`e-` (false) followed by the count of exponent digits.
    Exp(bool, usize),
}

/// Splits a pattern on `;`, ignoring semicolons inside quotes or after a backslash.
fn sections(pattern: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let (mut quoted, mut escaped) = (false, false);
    for c in pattern.chars() {
        let cur = out.last_mut().expect("never empty");
        if escaped {
            cur.push(c);
            escaped = false;
        } else if c == '\\' && !quoted {
            cur.push(c);
            escaped = true;
        } else if c == '"' {
            cur.push(c);
            quoted = !quoted;
        } else if c == ';' && !quoted {
            out.push(String::new());
        } else {
            cur.push(c);
        }
    }
    out
}

fn tokenize_number(sec: &str) -> Vec<NumTok> {
    let chars: Vec<char> = sec.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let push_lit = |out: &mut Vec<NumTok>, s: &str| match out.last_mut() {
        Some(NumTok::Lit(t)) => t.push_str(s),
        _ => out.push(NumTok::Lit(s.to_string())),
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '0' => out.push(NumTok::Digit(true)),
            '#' => out.push(NumTok::Digit(false)),
            '.' if !out.contains(&NumTok::Point) => out.push(NumTok::Point),
            ',' => out.push(NumTok::Comma),
            '%' => out.push(NumTok::Percent),
            'E' | 'e' if matches!(chars.get(i + 1), Some('+' | '-')) => {
                let plus = chars[i + 1] == '+';
                let mut n = 0;
                while chars.get(i + 2 + n).is_some_and(|c| *c == '0' || *c == '#') {
                    n += 1;
                }
                if n == 0 {
                    push_lit(&mut out, &c.to_string());
                } else {
                    out.push(NumTok::Exp(plus, n));
                    i += 1 + n;
                }
            }
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    s.push(chars[i]);
                    i += 1;
                }
                push_lit(&mut out, &s);
            }
            '\\' => {
                i += 1;
                if let Some(n) = chars.get(i) {
                    push_lit(&mut out, &n.to_string());
                }
            }
            other => push_lit(&mut out, &other.to_string()),
        }
        i += 1;
    }
    out
}

fn format_number(d: &Decimal, pattern: &str) -> Result<String> {
    let named = pattern.trim().to_ascii_lowercase();
    if pattern.is_empty() || named == "general number" {
        return Ok(d.to_string());
    }
    let pat = number_pattern(&named, pattern);
    let secs = sections(&pat);
    // Choose the section by sign; a lone section shows a minus sign itself.
    let (sec, show_minus) = if d.is_negative() {
        match secs.get(1) {
            Some(s) if !s.is_empty() => (s.as_str(), false),
            _ => (secs[0].as_str(), true),
        }
    } else if d.is_zero() {
        match secs.get(2) {
            Some(s) if !s.is_empty() => (s.as_str(), false),
            _ => (secs[0].as_str(), false),
        }
    } else {
        (secs[0].as_str(), false)
    };
    let toks = tokenize_number(sec);
    if !toks.iter().any(|t| matches!(t, NumTok::Digit(_))) {
        // A section with no digit placeholders (such as "Zero") is just text.
        return Ok(toks
            .iter()
            .map(|t| match t {
                NumTok::Lit(s) => s.clone(),
                NumTok::Percent => "%".into(),
                _ => String::new(),
            })
            .collect());
    }

    let mut value = d.abs();
    let percents = toks.iter().filter(|t| matches!(t, NumTok::Percent)).count() as i64;
    if percents > 0 {
        let (m, s) = value.parts();
        value = Decimal::from_parts(m, s as i64 - 2 * percents);
    }
    let point_at = toks.iter().position(|t| *t == NumTok::Point);
    let is_int_ph = |i: usize| matches!(toks[i], NumTok::Digit(_)) && point_at.is_none_or(|p| i < p);
    let int_ph: Vec<usize> = (0..toks.len()).filter(|&i| is_int_ph(i)).collect();
    let frac_ph: Vec<usize> = (0..toks.len()).filter(|&i| matches!(toks[i], NumTok::Digit(_)) && !is_int_ph(i)).collect();
    // A comma between two integer placeholders groups thousands.
    let thousands = match (int_ph.first(), int_ph.last()) {
        (Some(&a), Some(&b)) => toks[a..b].contains(&NumTok::Comma),
        _ => false,
    };
    let exp_tok = toks.iter().find_map(|t| if let NumTok::Exp(plus, n) = t { Some((*plus, *n)) } else { None });

    // Scientific notation moves the point so that the integer placeholders are filled.
    let mut exponent: i64 = 0;
    if exp_tok.is_some() && !value.is_zero() {
        let place = |v: &Decimal| {
            let (m, s) = v.parts();
            m.magnitude().to_str_radix(10).len() as i64 - s as i64
        };
        let want = int_ph.len().max(1) as i64;
        exponent = place(&value) - want;
        let (m, s) = value.parts();
        let mut shifted = Decimal::from_parts(m, s as i64 + exponent).round(frac_ph.len() as u32);
        if place(&shifted) > want {
            exponent += 1;
            let (m, s) = value.parts();
            shifted = Decimal::from_parts(m, s as i64 + exponent).round(frac_ph.len() as u32);
        }
        value = shifted;
    }

    let rounded = value.round(frac_ph.len() as u32);
    let (mant, scale) = rounded.parts();
    let mut digits = mant.magnitude().to_str_radix(10);
    if (digits.len() as u32) <= scale {
        digits = format!("{}{digits}", "0".repeat(scale as usize + 1 - digits.len()));
    }
    let (int_digits, frac_digits) = digits.split_at(digits.len() - scale as usize);
    let mut int_part = if int_digits == "0" { String::new() } else { int_digits.to_string() };
    let min_int = int_ph.iter().filter(|&&i| toks[i] == NumTok::Digit(true)).count();
    if int_part.len() < min_int {
        int_part = format!("{}{int_part}", "0".repeat(min_int - int_part.len()));
    }
    let mut frac: Vec<char> = frac_digits.chars().collect();
    frac.resize(frac_ph.len(), '0');

    // Lay the digits out over the placeholders.
    let mut out = String::new();
    if show_minus && !rounded.is_zero() {
        out.push('-');
    }
    let int_chars: Vec<char> = int_part.chars().collect();
    let mut int_seen = 0;
    let mut frac_seen = 0;
    for (i, t) in toks.iter().enumerate() {
        match t {
            NumTok::Lit(s) => out.push_str(s),
            NumTok::Percent => out.push('%'),
            NumTok::Comma => {}
            NumTok::Point => {
                if !frac_ph.is_empty() {
                    out.push('.');
                }
            }
            NumTok::Digit(zero) if is_int_ph(i) => {
                if thousands {
                    if int_seen == 0 {
                        let n = int_chars.len();
                        for (k, c) in int_chars.iter().enumerate() {
                            if k > 0 && (n - k) % 3 == 0 {
                                out.push(',');
                            }
                            out.push(*c);
                        }
                    }
                } else {
                    // Digits fill from the right; extras land on the first placeholder.
                    let n = int_chars.len();
                    let from_right = int_ph.len() - 1 - int_seen;
                    if int_seen == 0 {
                        let head = n.saturating_sub(from_right);
                        if head > 0 {
                            out.extend(&int_chars[..head]);
                        } else if *zero {
                            out.push('0');
                        }
                    } else if n > from_right {
                        out.push(int_chars[n - from_right - 1]);
                    } else if *zero {
                        out.push('0');
                    }
                }
                int_seen += 1;
            }
            NumTok::Digit(zero) => {
                let c = frac[frac_seen];
                let rest_zero = frac[frac_seen..].iter().all(|c| *c == '0');
                if *zero || !rest_zero {
                    out.push(c);
                }
                frac_seen += 1;
            }
            NumTok::Exp(plus, n) => {
                out.push('E');
                if exponent < 0 {
                    out.push('-');
                } else if *plus {
                    out.push('+');
                }
                out.push_str(&format!("{:0width$}", exponent.unsigned_abs(), width = *n));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

fn date_pattern(named: &str, original: &str) -> String {
    match named {
        "general date" => return String::new(),
        "long date" => "dddd, mmmm d, yyyy",
        "medium date" => "dd-mmm-yy",
        "short date" => "m/d/yyyy",
        "long time" => "h:mm:ss AM/PM",
        "medium time" => "hh:mm AM/PM",
        "short time" => "hh:mm",
        _ => return original.to_string(),
    }
    .to_string()
}

#[derive(Debug, Clone, PartialEq)]
enum DateTok {
    Lit(String),
    /// A run of one pattern letter (`y`, `m`, `d`, `h`, `n`, `s`, `w`, `q`) and its length.
    Field(char, usize),
    /// `AM/PM`-style marker: the two strings to show and whether they are upper case.
    Meridiem(&'static str, &'static str, bool),
}

fn tokenize_date(pattern: &str) -> Vec<DateTok> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out: Vec<DateTok> = Vec::new();
    let mut i = 0;
    let push_lit = |out: &mut Vec<DateTok>, s: &str| match out.last_mut() {
        Some(DateTok::Lit(t)) => t.push_str(s),
        _ => out.push(DateTok::Lit(s.to_string())),
    };
    let starts = |i: usize, w: &str| {
        chars.len() >= i + w.len() && chars[i..i + w.len()].iter().collect::<String>().eq_ignore_ascii_case(w)
    };
    while i < chars.len() {
        let c = chars[i];
        let lower = c.to_ascii_lowercase();
        if starts(i, "am/pm") {
            let up = chars[i].is_ascii_uppercase();
            out.push(DateTok::Meridiem("AM", "PM", up));
            i += 5;
            continue;
        }
        if starts(i, "a/p") {
            let up = chars[i].is_ascii_uppercase();
            out.push(DateTok::Meridiem("A", "P", up));
            i += 3;
            continue;
        }
        match lower {
            'y' | 'm' | 'd' | 'h' | 'n' | 's' | 'w' | 'q' => {
                let mut n = 1;
                while chars.get(i + n).is_some_and(|x| x.to_ascii_lowercase() == lower) {
                    n += 1;
                }
                out.push(DateTok::Field(lower, n));
                i += n;
                continue;
            }
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    s.push(chars[i]);
                    i += 1;
                }
                push_lit(&mut out, &s);
            }
            '\\' => {
                i += 1;
                if let Some(n) = chars.get(i) {
                    push_lit(&mut out, &n.to_string());
                }
            }
            _ => push_lit(&mut out, &c.to_string()),
        }
        i += 1;
    }
    out
}

fn format_date(t: i64, pattern: &str) -> Result<String> {
    if pattern.is_empty() {
        return Ok(datetime::format(t));
    }
    let toks = tokenize_date(pattern);
    let (y, mo, d, h, mi, s) = datetime::components(t);
    let twelve = toks.iter().any(|t| matches!(t, DateTok::Meridiem(..)));
    let wd = datetime::weekday(t); // Sunday = 1
    let doy = datetime::days_from_civil(y, mo, d) - datetime::days_from_civil(y, 1, 1) + 1;
    let jan1_wd = datetime::weekday(datetime::days_from_civil(y, 1, 1) * 86_400) - 1;

    // "m" and "mm" mean minutes right after an hour or right before seconds.
    let significant = |from: usize, step: isize| -> Option<char> {
        let mut k = from as isize + step;
        while k >= 0 && (k as usize) < toks.len() {
            if let DateTok::Field(c, _) = &toks[k as usize] {
                return Some(*c);
            }
            k += step;
        }
        None
    };
    let mut out = String::new();
    for (i, tok) in toks.iter().enumerate() {
        match tok {
            DateTok::Lit(l) => out.push_str(l),
            DateTok::Meridiem(am, pm, upper) => {
                let w = if h < 12 { am } else { pm };
                out.push_str(&if *upper { w.to_string() } else { w.to_ascii_lowercase() });
            }
            DateTok::Field(c, n) => {
                let n = *n;
                let c = if *c == 'm' && (significant(i, -1) == Some('h') || significant(i, 1) == Some('s')) {
                    'n'
                } else {
                    *c
                };
                let hour = if twelve { if h % 12 == 0 { 12 } else { h % 12 } } else { h };
                match (c, n) {
                    ('y', 1) => out.push_str(&doy.to_string()),
                    ('y', 2) => out.push_str(&format!("{:02}", y.rem_euclid(100))),
                    ('y', _) => out.push_str(&format!("{y:04}")),
                    ('m', 1) => out.push_str(&mo.to_string()),
                    ('m', 2) => out.push_str(&format!("{mo:02}")),
                    ('m', 3) => out.push_str(&MONTHS[mo as usize - 1][..3]),
                    ('m', _) => out.push_str(MONTHS[mo as usize - 1]),
                    ('d', 1) => out.push_str(&d.to_string()),
                    ('d', 2) => out.push_str(&format!("{d:02}")),
                    ('d', 3) => out.push_str(&DAYS[wd as usize - 1][..3]),
                    ('d', 4) => out.push_str(DAYS[wd as usize - 1]),
                    ('d', 5) => out.push_str(&format_date(t, "m/d/yyyy")?),
                    ('d', _) => out.push_str(&format_date(t, "dddd, mmmm d, yyyy")?),
                    ('h', 1) => out.push_str(&hour.to_string()),
                    ('h', _) => out.push_str(&format!("{hour:02}")),
                    ('n', 1) => out.push_str(&mi.to_string()),
                    ('n', _) => out.push_str(&format!("{mi:02}")),
                    ('s', 1) => out.push_str(&s.to_string()),
                    ('s', _) => out.push_str(&format!("{s:02}")),
                    ('w', 1) => out.push_str(&wd.to_string()),
                    ('w', _) => out.push_str(&((doy - 1 + jan1_wd) / 7 + 1).to_string()),
                    ('q', _) => out.push_str(&((mo - 1) / 3 + 1).to_string()),
                    _ => {}
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

fn format_text(s: &str, pattern: &str) -> String {
    let sec = sections(pattern).swap_remove(0);
    let mut text = s.to_string();
    if sec.contains('>') {
        text = text.to_uppercase();
    } else if sec.contains('<') {
        text = text.to_lowercase();
    }
    let slots = sec.chars().filter(|c| matches!(c, '@' | '&')).count();
    if slots == 0 {
        return text;
    }
    // Characters fill the placeholders from the right; extras go to the first.
    let chars: Vec<char> = text.chars().collect();
    let pat: Vec<char> = sec.chars().collect();
    let mut out = Vec::new();
    let mut next = chars.len();
    let mut slot = slots;
    let mut i = pat.len();
    while i > 0 {
        i -= 1;
        match pat[i] {
            c @ ('@' | '&') => {
                slot -= 1;
                if slot == 0 {
                    out.extend(chars[..next].iter().rev());
                } else if next > 0 {
                    next -= 1;
                    out.push(chars[next]);
                } else if c == '@' {
                    out.push(' ');
                }
            }
            '>' | '<' => {}
            '!' => {}
            c => out.push(c),
        }
    }
    out.reverse();
    out.into_iter().collect()
}
