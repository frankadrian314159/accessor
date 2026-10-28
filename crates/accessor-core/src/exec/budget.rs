//! A time limit for a statement: `SET statement_timeout`, and the budget a plain `EXPLAIN` gets.
//!
//! The hot loops of the executor (scans, joins, repeated sub-queries) call [`tick`], which looks at the clock once in a
//! thousand calls and fails with [`EXPIRED`] when the deadline of the running statement has passed.

use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// The message of the error a statement ends with when its time is up.
pub(crate) const EXPIRED: &str = "canceling statement due to statement timeout";

thread_local! {
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
    static TICKS: Cell<u32> = const { Cell::new(0) };
}

/// Puts the statement under a limit until the guard is dropped (the sooner of this and any limit already in force).
pub(crate) struct Guard(Option<Instant>);

pub(crate) fn limit(after: Duration) -> Guard {
    let new = Instant::now() + after;
    let old = DEADLINE.with(|d| d.get());
    DEADLINE.with(|d| d.set(Some(old.map_or(new, |o| o.min(new)))));
    Guard(old)
}

impl Drop for Guard {
    fn drop(&mut self) {
        let old = self.0;
        DEADLINE.with(|d| d.set(old));
    }
}

/// Called often; fails when the deadline has passed.
#[inline]
pub(crate) fn tick() -> Result<()> {
    let n = TICKS.with(|t| {
        let n = t.get().wrapping_add(1);
        t.set(n);
        n
    });
    if n & 0x3FF == 0 {
        if let Some(d) = DEADLINE.with(|d| d.get()) {
            if Instant::now() > d {
                return Err(Error::runtime(EXPIRED));
            }
        }
    }
    Ok(())
}

/// Under a time limit, a result with this many values (rows times columns) is not worth waiting for either, and may not fit
/// in memory: it ends like a timeout. Wide rows reach it sooner, so a join of a few million narrow rows, as in TPC-H at a scale
/// factor of 0.5, still runs.
pub(crate) fn rows(n: usize, width: usize) -> Result<()> {
    if n >= 262_144 && n & 0xFFFF == 0 && n.saturating_mul(width.max(1)) >= 16_000_000 && DEADLINE.with(|d| d.get()).is_some() {
        return Err(Error::runtime(EXPIRED));
    }
    Ok(())
}

/// `statement_timeout` as PostgreSQL writes it: milliseconds, or a number with `ms`, `s`, `min`, `h`; 0 is no limit.
pub(crate) fn parse_timeout(text: &str) -> Option<Duration> {
    let t = text.trim().to_ascii_lowercase();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let n: f64 = t[..split].parse().ok()?;
    let ms = match t[split..].trim() {
        "" | "ms" => n,
        "s" => n * 1000.0,
        "min" => n * 60_000.0,
        "h" => n * 3_600_000.0,
        "us" => n / 1000.0,
        _ => return None,
    };
    (ms > 0.0).then(|| Duration::from_micros((ms * 1000.0) as u64))
}

/// MySQL's optimizer hint `/*+ MAX_EXECUTION_TIME(n) */` in a `SELECT`: the milliseconds, if the text has one.
pub(crate) fn hinted_time(sql: &str) -> Option<Duration> {
    let head = sql.trim_start();
    if !head.get(..6).is_some_and(|h| h.eq_ignore_ascii_case("select")) {
        return None;
    }
    let start = sql.find("/*+")?;
    let end = sql[start..].find("*/")? + start;
    let hint = sql[start..end].to_ascii_uppercase();
    let at = hint.find("MAX_EXECUTION_TIME(")? + "MAX_EXECUTION_TIME(".len();
    let ms: u64 = hint[at..].split(')').next()?.trim().parse().ok()?;
    (ms > 0).then(|| Duration::from_millis(ms))
}
