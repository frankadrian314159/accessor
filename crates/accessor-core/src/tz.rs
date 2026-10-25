//! Time zones.
//!
//! A `TIMESTAMPTZ` is an instant, stored as seconds since the Unix epoch in UTC. A
//! zone says how that instant reads on a wall clock: its offset from UTC at that
//! moment, which for named regions (`Europe/Paris`) follows the IANA database,
//! daylight saving included. A plain `DATETIME` has no zone at all: it is a wall
//! clock reading.
//!
//! The *session zone* decides how text without an offset is read, how a `DATETIME` and
//! a `TIMESTAMPTZ` are converted into one another, and how a `TIMESTAMPTZ` is shown.
//! It is a per-database setting (`SET TIME ZONE`) kept in a thread-local while a
//! statement runs, since value formatting and the SQL functions have no database in
//! hand. The default is UTC.

use std::cell::Cell;

use chrono::{DateTime, Offset, TimeZone};

use crate::datetime;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Zone {
    /// A fixed offset from UTC, in seconds east.
    Fixed(i32),
    /// A region of the IANA time zone database.
    Named(chrono_tz::Tz),
}

impl Zone {
    pub const UTC: Zone = Zone::Fixed(0);

    /// Reads a zone name: `UTC`, `Z`, `GMT`, an offset (`+05:30`, `-0800`, `+5`) or a region
    /// of the IANA database (`Europe/Paris`, `America/New_York`; case does not matter).
    pub fn parse(text: &str) -> Option<Zone> {
        let t = text.trim();
        if t.eq_ignore_ascii_case("utc") || t.eq_ignore_ascii_case("z") || t.eq_ignore_ascii_case("gmt") {
            return Some(Zone::UTC);
        }
        if let Some(secs) = parse_offset(t) {
            return Some(Zone::Fixed(secs));
        }
        t.parse::<chrono_tz::Tz>().ok().map(Zone::Named).or_else(|| {
            // The database is case-sensitive; accept any case by trying the usual forms.
            chrono_tz::TZ_VARIANTS.iter().find(|z| z.name().eq_ignore_ascii_case(t)).map(|z| Zone::Named(*z))
        })
    }

    /// The offset from UTC (seconds east) in force at the instant `utc`.
    pub fn offset_at(self, utc: i64) -> i64 {
        match self {
            Zone::Fixed(o) => o as i64,
            Zone::Named(tz) => match DateTime::from_timestamp(utc, 0) {
                Some(dt) => tz.offset_from_utc_datetime(&dt.naive_utc()).fix().local_minus_utc() as i64,
                None => 0,
            },
        }
    }

    /// The wall-clock reading (as a `DATETIME` value) of the instant `utc`.
    pub fn to_local(self, utc: i64) -> i64 {
        utc + self.offset_at(utc)
    }

    /// The instant at which the wall clock of this zone read `local`. A reading that occurs
    /// twice (the clock went back) is taken at its first occurrence; one that never occurs
    /// (the clock jumped forward) is read with the offset from before the jump.
    pub fn to_utc(self, local: i64) -> i64 {
        match self {
            Zone::Fixed(o) => local - o as i64,
            Zone::Named(_) => {
                let before = self.offset_at(local - 86_400);
                let after = self.offset_at(local + 86_400);
                let valid: Vec<i64> = [before, after]
                    .into_iter()
                    .map(|off| local - off)
                    .filter(|utc| self.offset_at(*utc) == local - utc)
                    .collect();
                valid.into_iter().min().unwrap_or(local - before)
            }
        }
    }

    pub fn name(self) -> String {
        match self {
            Zone::Fixed(0) => "UTC".to_string(),
            Zone::Fixed(o) => format_offset(o as i64),
            Zone::Named(tz) => tz.name().to_string(),
        }
    }
}

/// `+05:30` for 19 800 seconds.
pub fn format_offset(secs: i64) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.abs();
    if a % 60 != 0 {
        format!("{sign}{:02}:{:02}:{:02}", a / 3600, a % 3600 / 60, a % 60)
    } else {
        format!("{sign}{:02}:{:02}", a / 3600, a % 3600 / 60)
    }
}

/// `+05:30`, `-0800`, `+5`, `+05` as seconds east of UTC.
fn parse_offset(t: &str) -> Option<i32> {
    let (sign, rest) = match t.chars().next()? {
        '+' => (1, &t[1..]),
        '-' => (-1, &t[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) || rest.len() > 8 {
        return None;
    }
    let (h, m, s) = match (rest.contains(':'), digits.len()) {
        (_, 1) | (_, 2) => (digits.parse::<i32>().ok()?, 0, 0),
        (_, 3) => (digits[..1].parse().ok()?, digits[1..].parse().ok()?, 0),
        (_, 4) => (digits[..2].parse().ok()?, digits[2..].parse().ok()?, 0),
        (_, 6) => (digits[..2].parse().ok()?, digits[2..4].parse().ok()?, digits[4..].parse().ok()?),
        _ => return None,
    };
    if h > 23 || m > 59 || s > 59 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60 + s))
}

thread_local! {
    static SESSION: Cell<Zone> = const { Cell::new(Zone::UTC) };
}

/// The zone of the statement being run (UTC outside one).
pub fn session() -> Zone {
    SESSION.with(Cell::get)
}

pub fn set_session(zone: Zone) {
    SESSION.with(|c| c.set(zone));
}

/// Reads a timestamp with a zone. A trailing `Z`, offset (`+01:00`) or region name
/// (`Europe/Paris`) fixes the instant; without one the text is read in `default`.
pub fn parse_timestamptz(text: &str, default: Zone) -> Option<i64> {
    let s = text.trim();
    if let Some(local) = datetime::parse(s) {
        return Some(default.to_utc(local));
    }
    // A zone designator at the end: Z, an offset, or a name after a space.
    if let Some(body) = s.strip_suffix(['Z', 'z']) {
        return datetime::parse(body.trim_end()).map(|l| Zone::UTC.to_utc(l));
    }
    // An offset: the last '+' or '-' after the clock part.
    if let Some(colon) = s.find(':') {
        if let Some(pos) = s[colon..].rfind(['+', '-']).map(|p| p + colon) {
            if let (Some(zone), Some(local)) = (Zone::parse(&s[pos..]), datetime::parse(s[..pos].trim_end())) {
                return Some(zone.to_utc(local));
            }
        }
    }
    // A region or abbreviation after a space.
    let (body, name) = s.rsplit_once(' ')?;
    let zone = Zone::parse(name)?;
    datetime::parse(body.trim_end()).map(|l| zone.to_utc(l))
}

/// A `TIMESTAMPTZ` as text, in the session zone: `2024-03-05 14:30:00+01:00`.
pub fn format_timestamptz(utc: i64) -> String {
    format_in(utc, session())
}

pub fn format_in(utc: i64, zone: Zone) -> String {
    let off = zone.offset_at(utc);
    let (y, mo, d, h, mi, s) = datetime::components(utc + off);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}{}", format_offset(off))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_and_names() {
        assert_eq!(Zone::parse("+05:30"), Some(Zone::Fixed(19_800)));
        assert_eq!(Zone::parse("-0800"), Some(Zone::Fixed(-28_800)));
        assert_eq!(Zone::parse("utc"), Some(Zone::UTC));
        assert!(matches!(Zone::parse("europe/paris"), Some(Zone::Named(_))));
        assert_eq!(Zone::parse("Nowhere/Land"), None);
        assert_eq!(Zone::Fixed(19_800).name(), "+05:30");
    }

    #[test]
    fn daylight_saving_is_followed() {
        let ny = Zone::parse("America/New_York").unwrap();
        let winter = datetime::parse("2024-01-15 12:00:00").unwrap();
        let summer = datetime::parse("2024-07-15 12:00:00").unwrap();
        assert_eq!(ny.offset_at(winter), -5 * 3600);
        assert_eq!(ny.offset_at(summer), -4 * 3600);
        // Local noon in summer is 16:00 UTC.
        assert_eq!(ny.to_utc(summer), summer + 4 * 3600);
        // The hour that is skipped (2024-03-10 02:30) is read with the offset from before.
        let gap = datetime::parse("2024-03-10 02:30:00").unwrap();
        assert_eq!(ny.to_local(ny.to_utc(gap)), gap + 3600);
        // The hour that happens twice (2024-11-03 01:30) is the first one.
        let twice = datetime::parse("2024-11-03 01:30:00").unwrap();
        assert_eq!(ny.offset_at(ny.to_utc(twice)), -4 * 3600);
    }

    #[test]
    fn timestamps_with_zones() {
        let utc = |s: &str| datetime::parse(s).unwrap();
        assert_eq!(parse_timestamptz("2024-03-05 14:30:00+01:00", Zone::UTC), Some(utc("2024-03-05 13:30:00")));
        assert_eq!(parse_timestamptz("2024-03-05T14:30:00Z", Zone::UTC), Some(utc("2024-03-05 14:30:00")));
        assert_eq!(parse_timestamptz("2024-03-05 14:30:00 Europe/Paris", Zone::UTC), Some(utc("2024-03-05 13:30:00")));
        assert_eq!(parse_timestamptz("2024-03-05 14:30:00", Zone::Fixed(3600)), Some(utc("2024-03-05 13:30:00")));
        assert_eq!(parse_timestamptz("2024-03-05", Zone::UTC), Some(utc("2024-03-05")));
        assert_eq!(parse_timestamptz("nonsense", Zone::UTC), None);
    }
}
