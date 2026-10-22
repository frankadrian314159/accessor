//! Minimal proleptic-Gregorian calendar helpers. Timestamps are whole seconds
//! since the Unix epoch (UTC); no external time crate is needed.

use std::time::{SystemTime, UNIX_EPOCH};

const SECS_PER_DAY: i64 = 86_400;
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

fn days_in_month(y: i64, m: u32) -> u32 {
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

/// Splits a timestamp into `(year, month, day, hour, minute, second)`.
pub fn components(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(SECS_PER_DAY);
    let rem = secs.rem_euclid(SECS_PER_DAY) as u32;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn from_components(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<i64> {
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    Some(days_from_civil(y, mo, d) * SECS_PER_DAY + (h * 3600 + mi * 60 + s) as i64)
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn ole_days(secs: i64) -> f64 {
    secs as f64 / SECS_PER_DAY as f64 + OLE_OFFSET_DAYS
}

pub fn from_ole_days(days: f64) -> i64 {
    ((days - OLE_OFFSET_DAYS) * SECS_PER_DAY as f64).round() as i64
}

/// Weekday with Sunday = 1 … Saturday = 7 (Access `Weekday()` convention).
pub fn weekday(secs: i64) -> i64 {
    // 1970-01-01 was a Thursday (5).
    (secs.div_euclid(SECS_PER_DAY) + 4).rem_euclid(7) + 1
}

pub fn format(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = components(secs);
    if h == 0 && mi == 0 && s == 0 {
        format!("{y:04}-{mo:02}-{d:02}")
    } else {
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
    }
}

/// Parses `YYYY-MM-DD[ T]HH:MM[:SS]` and US-style `M/D/YYYY [h:mm[:ss] [AM|PM]]`.
pub fn parse(input: &str) -> Option<i64> {
    let s = input.trim();
    let (date_part, time_part) = match s.find(['T', ' ']) {
        Some(i) => (&s[..i], s[i + 1..].trim()),
        None => (s, ""),
    };
    let nums: Vec<&str> = date_part.split(['-', '/']).collect();
    if nums.len() != 3 {
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
        sec = it.next().map_or(Some(0), |v| v.trim().parse().ok())?;
        if it.next().is_some() {
            return None;
        }
        match meridiem {
            Some(true) if h < 12 => h += 12,
            Some(false) if h == 12 => h = 0,
            _ => {}
        }
    }
    from_components(y, mo, d, h, mi, sec)
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
    }

    #[test]
    fn ole_epoch() {
        assert_eq!(ole_days(parse("1899-12-30").unwrap()), 0.0);
        assert_eq!(from_ole_days(0.0), parse("1899-12-30").unwrap());
    }
}
