//! Sequences: named counters that hand out numbers (`CREATE SEQUENCE`, `NEXTVAL`).
//!
//! The definition of a sequence is part of the schema and follows transactions; the
//! *value* is not. A number handed out is never handed out again, even if the
//! transaction that asked for it is rolled back (so sequences can have gaps).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A sequence and the last number it handed out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceDef {
    pub name: String,
    pub start: i64,
    pub increment: i64,
    pub min: i64,
    pub max: i64,
    pub cycle: bool,
    /// The last number handed out (`None`: none yet).
    pub last: Option<i64>,
}

/// The options of `CREATE SEQUENCE` / `ALTER SEQUENCE`; `None` is "not given".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SequenceOptions {
    pub start: Option<i64>,
    pub increment: Option<i64>,
    /// `Some(None)` is `NO MINVALUE`.
    pub min: Option<Option<i64>>,
    pub max: Option<Option<i64>>,
    pub cycle: Option<bool>,
    /// `RESTART` (`Some(None)`: at the start value) or `RESTART WITH n`.
    pub restart: Option<Option<i64>>,
}

impl SequenceDef {
    /// A new sequence from `CREATE SEQUENCE` options.
    pub fn create(name: &str, o: &SequenceOptions) -> Result<SequenceDef> {
        let mut d = SequenceDef { name: name.to_string(), start: 0, increment: 1, min: 1, max: i64::MAX, cycle: false, last: None };
        d.increment = o.increment.unwrap_or(1);
        if d.increment == 0 {
            return Err(Error::runtime("a sequence cannot increase by 0"));
        }
        let up = d.increment > 0;
        d.min = o.min.flatten().unwrap_or(if up { 1 } else { i64::MIN });
        d.max = o.max.flatten().unwrap_or(if up { i64::MAX } else { -1 });
        d.start = o.start.unwrap_or(if up { d.min } else { d.max });
        d.cycle = o.cycle.unwrap_or(false);
        d.check()?;
        Ok(d)
    }

    /// Applies the options of `ALTER SEQUENCE`.
    pub fn alter(&mut self, o: &SequenceOptions) -> Result<()> {
        if let Some(i) = o.increment {
            if i == 0 {
                return Err(Error::runtime("a sequence cannot increase by 0"));
            }
            self.increment = i;
        }
        if let Some(m) = o.min {
            self.min = m.unwrap_or(if self.increment > 0 { 1 } else { i64::MIN });
        }
        if let Some(m) = o.max {
            self.max = m.unwrap_or(if self.increment > 0 { i64::MAX } else { -1 });
        }
        if let Some(s) = o.start {
            self.start = s;
        }
        if let Some(c) = o.cycle {
            self.cycle = c;
        }
        if let Some(r) = o.restart {
            self.last = None;
            if let Some(n) = r {
                self.start = n;
            }
        }
        self.check()
    }

    fn check(&self) -> Result<()> {
        if self.min > self.max {
            return Err(Error::runtime(format!("sequence '{}': the minimum is above the maximum", self.name)));
        }
        if self.start < self.min || self.start > self.max {
            return Err(Error::runtime(format!("sequence '{}': the start value is outside {} to {}", self.name, self.min, self.max)));
        }
        Ok(())
    }

    /// The next number.
    pub fn advance(&mut self) -> Result<i64> {
        let v = match self.last {
            None => self.start,
            Some(l) => match l.checked_add(self.increment) {
                Some(n) if n >= self.min && n <= self.max => n,
                _ if self.cycle => {
                    if self.increment > 0 {
                        self.min
                    } else {
                        self.max
                    }
                }
                _ => return Err(Error::runtime(format!("the sequence '{}' has reached its limit", self.name))),
            },
        };
        self.last = Some(v);
        Ok(v)
    }
}

/// The sequences of a database, shared with the expression evaluator.
#[derive(Default)]
pub struct SeqStore {
    pub(crate) defs: BTreeMap<String, SequenceDef>,
    /// Sequences handed out numbers since their value was last logged.
    pub(crate) dirty: Vec<String>,
    /// The number most recently handed out by any sequence (`LASTVAL`).
    pub(crate) last_any: Option<i64>,
}

pub(crate) type Shared = Arc<Mutex<SeqStore>>;

thread_local! {
    static CURRENT: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

/// Makes `store` the one `NEXTVAL` and friends talk to on this thread.
pub(crate) fn set_current(store: &Shared) {
    CURRENT.with(|c| *c.borrow_mut() = Some(store.clone()));
}

fn with_store<T>(f: impl FnOnce(&mut SeqStore) -> Result<T>) -> Result<T> {
    CURRENT.with(|c| {
        let c = c.borrow();
        let store = c.as_ref().ok_or_else(|| Error::runtime("there are no sequences here"))?;
        let mut guard = store.lock().map_err(|_| Error::runtime("the sequences are unavailable"))?;
        f(&mut guard)
    })
}

fn missing(name: &str) -> Error {
    Error::catalog(format!("the sequence '{name}' does not exist"))
}

fn lookup<'a>(s: &'a mut SeqStore, name: &str) -> Result<&'a mut SequenceDef> {
    s.defs.get_mut(&name.to_ascii_lowercase()).ok_or_else(|| missing(name))
}

/// `NEXTVAL('name')`.
pub(crate) fn next_value(name: &str) -> Result<i64> {
    with_store(|s| {
        let v = lookup(s, name)?.advance()?;
        s.last_any = Some(v);
        let k = name.to_ascii_lowercase();
        if !s.dirty.contains(&k) {
            s.dirty.push(k);
        }
        Ok(v)
    })
}

/// `CURRVAL('name')`: the number last handed out by the sequence.
pub(crate) fn current_value(name: &str) -> Result<i64> {
    with_store(|s| {
        lookup(s, name)?.last.ok_or_else(|| Error::runtime(format!("the sequence '{name}' has not handed out a number yet")))
    })
}

/// `LASTVAL()`: the number last handed out by any sequence.
pub(crate) fn last_value() -> Result<i64> {
    with_store(|s| s.last_any.ok_or_else(|| Error::runtime("no sequence has handed out a number yet")))
}

/// `SETVAL('name', n [, is_called])`.
pub(crate) fn set_value(name: &str, n: i64, called: bool) -> Result<i64> {
    with_store(|s| {
        let d = lookup(s, name)?;
        if n < d.min || n > d.max {
            return Err(Error::runtime(format!("{n} is outside the range of the sequence '{name}'")));
        }
        if called {
            d.last = Some(n);
        } else {
            // The next call hands out `n` itself.
            d.last = None;
            d.start = n;
        }
        let k = name.to_ascii_lowercase();
        if !s.dirty.contains(&k) {
            s.dirty.push(k);
        }
        Ok(n)
    })
}

/// True if a sequence of this name exists (used to check column defaults).
pub(crate) fn exists(store: &Shared, name: &str) -> bool {
    store.lock().map(|s| s.defs.contains_key(&name.to_ascii_lowercase())).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_cycles() {
        let o = SequenceOptions { max: Some(Some(3)), cycle: Some(true), ..Default::default() };
        let mut d = SequenceDef::create("s", &o).unwrap();
        let got: Vec<i64> = (0..5).map(|_| d.advance().unwrap()).collect();
        assert_eq!(got, vec![1, 2, 3, 1, 2]);
    }

    #[test]
    fn stops_at_the_limit() {
        let o = SequenceOptions { max: Some(Some(2)), ..Default::default() };
        let mut d = SequenceDef::create("s", &o).unwrap();
        d.advance().unwrap();
        d.advance().unwrap();
        assert!(d.advance().is_err());
    }

    #[test]
    fn descending_defaults() {
        let o = SequenceOptions { increment: Some(-2), ..Default::default() };
        let mut d = SequenceDef::create("s", &o).unwrap();
        assert_eq!((d.advance().unwrap(), d.advance().unwrap()), (-1, -3));
    }
}
