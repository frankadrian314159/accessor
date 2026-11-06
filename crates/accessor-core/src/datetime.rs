//! Minimal proleptic-Gregorian calendar helpers. Timestamps are milliseconds
//! since the Unix epoch (UTC); no external time crate is needed. Outside PostgreSQL mode
//! a fraction of a second in text is dropped, so only that dialect sees non-zero milliseconds.

use std::time::{SystemTime, UNIX_EPOCH};

pub const MS_PER_SEC: i64 = 1000;
pub const MS_PER_DAY: i64 = 86_400_000;
/// Days between the OLE automation epoch (1899-12-30) and the Unix epoch.
const OLE_OFFSET_DAYS: f64 = 25_569.0;

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
    }
}

/// Splits a timestamp into `(year, month, day, hour, minute, second)`; the milliseconds are [`millis`].
pub fn components(t: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = t.div_euclid(MS_PER_DAY);
    let rem = (t.rem_euclid(MS_PER_DAY) / MS_PER_SEC) as u32;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

/// The milliseconds past the whole second (0 to 999).
pub fn millis(t: i64) -> u32 {
    t.rem_euclid(MS_PER_SEC) as u32
}

/// Adds `n` months to a reading, clamping the day to the end of a shorter month
/// (31 January plus a month is 29 February in a leap year).
pub fn add_months(t: i64, n: i64) -> Option<i64> {
    let (y, m, d, h, mi, s) = components(t);
    let total = y.checked_mul(12)?.checked_add(m as i64 - 1)?.checked_add(n)?;
    let (ny, nm) = (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32);
    let day = d.min(days_in_month(ny, nm));
    from_components(ny, nm, day, h, mi, s)?.checked_add(millis(t) as i64)
}

pub fn from_components(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<i64> {
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    Some(days_from_civil(y, mo, d) * MS_PER_DAY + (h * 3600 + mi * 60 + s) as i64 * MS_PER_SEC)
}

thread_local! {
    static STATEMENT_START: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

/// Fixes `now()` for the statement that is about to run (PostgreSQL gives every `now()` of a statement the same time).
pub(crate) fn start_statement() {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    STATEMENT_START.with(|s| s.set(Some(ms)));
}

/// The time now; whole seconds outside PostgreSQL mode, and in it the time the statement started.
pub fn now() -> i64 {
    let ms = STATEMENT_START
        .with(std::cell::Cell::get)
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0));
    if crate::dialect::current() == crate::dialect::Dialect::Postgresql {
        ms
    } else {
        ms - ms % MS_PER_SEC
    }
}

pub fn ole_days(t: i64) -> f64 {
    t as f64 / MS_PER_DAY as f64 + OLE_OFFSET_DAYS
}

pub fn from_ole_days(days: f64) -> i64 {
    ((days - OLE_OFFSET_DAYS) * 86_400.0).round() as i64 * MS_PER_SEC
}

/// Weekday with Sunday = 1 … Saturday = 7 (Access `Weekday()` convention).
pub fn weekday(t: i64) -> i64 {
    // 1970-01-01 was a Thursday (5).
    (t.div_euclid(MS_PER_DAY) + 4).rem_euclid(7) + 1
}

/// PostgreSQL's `infinity` and `-infinity`: the largest and smallest readings, which compare with every other one.
pub const INFINITY: i64 = i64::MAX;
pub const NEG_INFINITY: i64 = -i64::MAX;

pub fn is_infinite(t: i64) -> bool {
    t >= INFINITY || t <= NEG_INFINITY
}

/// `SS` and, when there are milliseconds, `SS.mmm` with the trailing zeros dropped.
pub fn seconds_text(s: u32, ms: u32) -> String {
    if ms == 0 {
        format!("{s:02}")
    } else {
        format!("{s:02}.{}", format!("{ms:03}").trim_end_matches('0'))
    }
}

pub fn format(t: i64) -> String {
    if t >= INFINITY {
        return "infinity".into();
    }
    if t <= NEG_INFINITY {
        return "-infinity".into();
    }
    let (y, mo, d, h, mi, s) = components(t);
    let ms = millis(t);
    if h == 0 && mi == 0 && s == 0 && ms == 0 {
        format!("{y:04}-{mo:02}-{d:02}")
    } else {
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{}", seconds_text(s, ms))
    }
}

/// Reads the seconds of a clock (`07`, `07.5`, `07.123456`) as milliseconds. Outside PostgreSQL mode the fraction is dropped.
fn seconds_ms(text: &str) -> Option<i64> {
    let text = text.trim();
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let whole: i64 = whole.parse().ok()?;
    if !(0..=59).contains(&whole) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let ms = if crate::dialect::current() == crate::dialect::Dialect::Postgresql && !frac.is_empty() {
        // rounded to the millisecond; 59.9996 carries into the next minute as it does in PostgreSQL
        let digits: String = frac.chars().chain("0000".chars()).take(4).collect();
        (digits.parse::<i64>().ok()? + 5) / 10
    } else {
        0
    };
    Some(whole * MS_PER_SEC + ms)
}

/// Parses `YYYY-MM-DD[ T]HH:MM[:SS]` and US-style `M/D/YYYY [h:mm[:ss] [AM|PM]]`.
pub fn parse(input: &str) -> Option<i64> {
    parse_iso(input).or_else(|| if crate::dialect::current() == crate::dialect::Dialect::Postgresql { parse_loose(input) } else { None })
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

fn month_of(word: &str) -> Option<u32> {
    let w = word.trim_end_matches('.');
    if w.len() < 3 {
        return None;
    }
    let i = MONTHS.iter().position(|m| w.starts_with(m))?;
    let full = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"][i];
    (w == MONTHS[i] || w == full || (i == 8 && w == "sept")).then_some(i as u32 + 1)
}

fn is_weekday(word: &str) -> bool {
    let w = word.trim_end_matches('.');
    WEEKDAYS.iter().any(|d| w == *d || (w.len() > 3 && w.starts_with(d) && ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday", "tues", "wednes", "thur", "thurs"].contains(&w)))
}

/// PostgreSQL's looser timestamp input: `now`, `today`, `tomorrow`, `yesterday`, `epoch`, and dates with month names
/// (`Mon Feb 10 17:32:01 1997`, `February 10, 1997`, `10 Feb 1997`, `1997-Feb-10`) or run together (`19970210 173201`).
fn parse_loose(input: &str) -> Option<i64> {
    let lower = input.trim().to_ascii_lowercase();
    let zone = crate::tz::session();
    let n = now();
    let local_now = zone.to_local(n);
    let midnight = local_now.div_euclid(MS_PER_DAY) * MS_PER_DAY;
    match lower.as_str() {
        "now" => return Some(local_now),
        "today" | "midnight" | "allballs" => return Some(midnight),
        "tomorrow" => return Some(midnight + MS_PER_DAY),
        "yesterday" => return Some(midnight - MS_PER_DAY),
        "epoch" => return Some(0),
        _ => {}
    }
    let (mut year, mut month, mut day): (Option<i64>, Option<u32>, Option<u32>) = (None, None, None);
    let (mut h, mut mi, mut sec) = (0u32, 0u32, 0u32);
    let mut frac_ms = 0i64;
    let (mut pm, mut bc) = (None, false);
    let mut numbers: Vec<(i64, usize)> = Vec::new();
    // how many numbers came before the month's name
    let mut month_at: Option<usize> = None;
    for t in lower.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()) {
        if let Some(m) = month_of(t) {
            month = Some(m);
            month_at = Some(numbers.len());
        } else if is_weekday(t) || t == "ad" || t == "at" || t == "on" {
        } else if t == "bc" {
            bc = true;
        } else if t == "am" || t == "pm" {
            pm = Some(t == "pm");
        } else if t.contains(':') {
            let (clock, merid) = match t.strip_suffix("pm") {
                Some(c) => (c, Some(true)),
                None => match t.strip_suffix("am") {
                    Some(c) => (c, Some(false)),
                    None => (t, None),
                },
            };
            if merid.is_some() {
                pm = merid;
            }
            let mut it = clock.split(':');
            h = it.next()?.parse().ok()?;
            mi = it.next()?.parse().ok()?;
            if let Some(v) = it.next() {
                let total = seconds_ms(v)?;
                sec = (total / MS_PER_SEC) as u32;
                frac_ms = total % MS_PER_SEC;
            }
        } else if t.contains('-') && t.split('-').any(|p| month_of(p).is_some()) {
            // 1997-feb-10, 10-feb-1997, feb-10-1997
            for p in t.split('-') {
                if let Some(m) = month_of(p) {
                    month = Some(m);
                    month_at = Some(numbers.len());
                } else {
                    numbers.push((p.parse().ok()?, p.len()));
                }
            }
        } else if t.chars().all(|c| c.is_ascii_digit()) {
            let v: i64 = t.parse().ok()?;
            if t.len() == 8 && month.is_none() {
                year = Some(v / 10_000);
                month = Some((v / 100 % 100) as u32);
                day = Some((v % 100) as u32);
            } else if t.len() == 6 && year.is_some() {
                h = (v / 10_000) as u32;
                mi = (v / 100 % 100) as u32;
                sec = (v % 100) as u32;
            } else {
                numbers.push((v, t.len()));
            }
        } else {
            return None;
        }
    }
    match (month, numbers.as_slice()) {
        (Some(_), [(_, la), (_, lb)]) if la.max(lb) < &3 || month_at == Some(2) || (month_at == Some(2) && *la >= 3) || (*la >= 3 && month_at != Some(1)) => return None,
        (Some(_), [(a, la), (b, lb)]) => {
            let (y, d) = if *la >= 3 || *a > 31 { (*a, *b) } else if *lb >= 3 || *b > 31 { (*b, *a) } else { (*b, *a) };
            let y = if la.max(lb) < &3 && y < 100 { if y >= 70 { y + 1900 } else { y + 2000 } } else { y };
            year = Some(y);
            day = Some(u32::try_from(d).ok()?);
        }
        (Some(_), [(a, _)]) if year.is_none() => day = Some(u32::try_from(*a).ok()?),
        (None, []) if year.is_some() => {}
        _ if numbers.is_empty() => {}
        _ => return None,
    }
    if year.is_none() && day.is_some() {
        year = Some(components(local_now).0);
    }
    let (mut y, mo, d) = (year?, month?, day?);
    if bc && y < 100 {
        return None;
    }
    if bc {
        y = 1 - y;
    }
    match pm {
        Some(true) if h < 12 => h += 12,
        Some(false) if h == 12 => h = 0,
        _ => {}
    }
    from_components(y, mo, d, h, mi, sec)?.checked_add(frac_ms)
}

fn parse_iso(input: &str) -> Option<i64> {
    let s = input.trim();
    if crate::dialect::current() == crate::dialect::Dialect::Postgresql {
        if s.eq_ignore_ascii_case("infinity") || s.eq_ignore_ascii_case("+infinity") {
            return Some(INFINITY);
        }
        if s.eq_ignore_ascii_case("-infinity") {
            return Some(NEG_INFINITY);
        }
    }
    let (date_part, time_part) = match s.find(['T', ' ']) {
        Some(i) => (&s[..i], s[i + 1..].trim()),
        None => (s, ""),
    };
    let nums: Vec<&str> = date_part.split(['-', '/']).collect();
    if nums.len() != 3 {
        // a time of day alone ('12:30:00.5', MySQL's TIME) is on the first day of the epoch
        if s.contains(':') && !s.contains(['-', '/']) {
            return parse(&format!("1970-01-01 {s}"));
        }
        return None;
    }
    let a: i64 = nums[0].parse().ok()?;
    let b: u32 = nums[1].parse().ok()?;
    let c: i64 = nums[2].parse().ok()?;
    let (y, mo, d) = if nums[0].len() == 4 {
        (a, b, u32::try_from(c).ok()?)
    } else {
        (c, u32::try_from(a).ok()?, b)
    };
    let (mut h, mut mi, mut sec) = (0u32, 0u32, 0u32);
    let mut frac_ms = 0i64;
    if !time_part.is_empty() {
        let upper = time_part.to_ascii_uppercase();
        let (clock, meridiem) = match upper.strip_suffix("PM") {
            Some(c) => (c.trim().to_string(), Some(true)),
            None => match upper.strip_suffix("AM") {
                Some(c) => (c.trim().to_string(), Some(false)),
                None => (upper.clone(), None),
            },
        };
        let mut it = clock.split(':');
        h = it.next()?.trim().parse().ok()?;
        mi = it.next().map_or(Some(0), |v| v.trim().parse().ok())?;
        // a fraction of a second (MySQL's '00:00:00.567') is dropped outside PostgreSQL mode
        if let Some(v) = it.next() {
            let total = seconds_ms(v)?;
            sec = (total / MS_PER_SEC) as u32;
            frac_ms = total % MS_PER_SEC;
        }
        if it.next().is_some() {
            return None;
        }
        match meridiem {
            Some(true) if h < 12 => h += 12,
            Some(false) if h == 12 => h = 0,
            _ => {}
        }
    }
    // MySQL's zero date, 0000-00-00, is the earliest date here
    if y == 0 && mo == 0 && d == 0 {
        return from_components(1, 1, 1, h, mi, sec)?.checked_add(frac_ms);
    }
    from_components(y, mo, d, h, mi, sec)?.checked_add(frac_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_civil() {
        for days in [-800_000, -1, 0, 1, 11_016, 19_000, 2_000_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn parse_and_format() {
        let t = parse("2024-02-29 13:45:10").unwrap();
        assert_eq!(format(t), "2024-02-29 13:45:10");
        assert_eq!(parse("2/29/2024"), parse("2024-02-29"));
        assert_eq!(parse("2023-02-29"), None);
        assert_eq!(parse("1/2/2020 1:05 PM"), parse("2020-01-02 13:05"));
        assert_eq!(weekday(parse("2024-02-29").unwrap()), 5); // Thursday
        assert_eq!(parse("2024-02-29 01:02:03").unwrap() % 86_400_000, 3_723_000);
    }

    #[test]
    fn ole_epoch() {
        assert_eq!(ole_days(parse("1899-12-30").unwrap()), 0.0);
        assert_eq!(from_ole_days(0.0), parse("1899-12-30").unwrap());
    }
}
