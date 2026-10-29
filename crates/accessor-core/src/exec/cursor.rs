//! Cursors: `DECLARE name [SCROLL] CURSOR [WITH HOLD] FOR query`, `FETCH` / `MOVE` in each direction, and `CLOSE`.
//!
//! A cursor is the rows of its query as they were when it was declared (an insensitive cursor): later changes to the
//! tables are not seen through it. It stands before the first row, on a row, or after the last, and `FETCH` moves it.
//! A cursor not declared `WITH HOLD` is closed when the transaction ends.

use super::QueryResult;
use crate::database::Database;
use crate::error::{Error, Result};
use crate::sql::ast::{FetchDirection, Select};
use crate::value::Row;

pub(crate) struct Cursor {
    columns: Vec<String>,
    rows: Vec<Row>,
    /// 0: before the first row; 1..=rows.len(): on that row; rows.len() + 1: after the last.
    pos: usize,
    hold: bool,
}

impl Database {
    pub(crate) fn declare_cursor(&mut self, name: &str, query: &Select, hold: bool) -> Result<QueryResult> {
        let key = name.to_ascii_lowercase();
        if self.cursors.contains_key(&key) {
            return Err(Error::runtime(format!("cursor \"{name}\" already exists")));
        }
        let res = self.run_select(query)?;
        self.cursors.insert(key, Cursor { columns: res.columns, rows: res.rows, pos: 0, hold });
        Ok(QueryResult::default())
    }

    pub(crate) fn fetch_cursor(&mut self, name: &str, direction: FetchDirection, move_only: bool) -> Result<QueryResult> {
        let Some(cur) = self.cursors.get_mut(&name.to_ascii_lowercase()) else {
            return Err(Error::runtime(format!("cursor \"{name}\" does not exist")));
        };
        let len = cur.rows.len() as i64;
        let mut pos = cur.pos as i64;
        // the rows to give, as positions from 1
        let mut take: Vec<i64> = Vec::new();
        // go to one row: beyond either end the cursor stands outside the rows and gives none
        let single = |target: i64, pos: &mut i64, take: &mut Vec<i64>| {
            *pos = target.clamp(0, len + 1);
            if (1..=len).contains(pos) {
                take.push(*pos);
            }
        };
        match direction {
            FetchDirection::Next => single(pos + 1, &mut pos, &mut take),
            FetchDirection::Prior => single(pos - 1, &mut pos, &mut take),
            FetchDirection::First => single(1, &mut pos, &mut take),
            FetchDirection::Last => single(len, &mut pos, &mut take),
            FetchDirection::Absolute(n) if n > 0 => single(n, &mut pos, &mut take),
            FetchDirection::Absolute(n) if n < 0 => single(len + 1 + n, &mut pos, &mut take),
            FetchDirection::Absolute(_) => single(0, &mut pos, &mut take),
            FetchDirection::Relative(n) => single(pos + n, &mut pos, &mut take),
            FetchDirection::Forward(count) => {
                let n = count.unwrap_or(i64::MAX).max(0);
                if n == 0 {
                    single(pos, &mut pos, &mut take);
                } else {
                    let last = pos.saturating_add(n).min(len);
                    take.extend(pos + 1..=last);
                    pos = if pos.saturating_add(n) > len { len + 1 } else { pos + n };
                }
            }
            FetchDirection::Backward(count) => {
                let n = count.unwrap_or(i64::MAX).max(0);
                if n == 0 {
                    single(pos, &mut pos, &mut take);
                } else {
                    let first = pos.saturating_sub(n).max(1);
                    take.extend((first..pos.min(len + 1)).rev());
                    pos = if pos.saturating_sub(n) < 1 { 0 } else { pos - n };
                }
            }
        }
        cur.pos = pos as usize;
        if move_only {
            return Ok(QueryResult { columns: Vec::new(), rows: Vec::new(), affected: take.len() });
        }
        Ok(QueryResult { columns: cur.columns.clone(), rows: take.iter().map(|&i| cur.rows[(i - 1) as usize].clone()).collect(), affected: 0 })
    }

    pub(crate) fn close_cursor(&mut self, name: Option<&str>) -> Result<QueryResult> {
        match name {
            None => self.cursors.clear(),
            Some(n) => {
                if self.cursors.remove(&n.to_ascii_lowercase()).is_none() {
                    return Err(Error::runtime(format!("cursor \"{n}\" does not exist")));
                }
            }
        }
        Ok(QueryResult::default())
    }

    /// At the end of a transaction: the cursors that were not declared `WITH HOLD` close (all of them with `all`).
    pub(crate) fn drop_cursors(&mut self, all: bool) {
        self.cursors.retain(|_, c| c.hold && !all);
    }
}
