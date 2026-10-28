//! Locking reads: `SELECT ... FOR UPDATE | SHARE [OF t] [NOWAIT | SKIP LOCKED]`.
//!
//! A row is claimed by giving it a new version, with the contents of the newest committed one, as an
//! `UPDATE` that changes nothing would (see `Table::claim`). That is the whole lock: until the transaction
//! ends, no one else can change the row, and a transaction that wants it waits, as for any pending change.
//! Unlike a plain read, a locking read goes by the *newest* committed contents of the row rather than
//! the transaction's snapshot, so the one that waited and then got the row neither fails with a
//! conflict nor works from stale values, and may go on to change the row.
//!
//! Only a `SELECT` of one table is locked. Other forms (joins, derived tables, unions) are read as if they had
//! no locking clause.

use std::cmp::Ordering;
use std::collections::HashSet;

use super::bind::{Binder, Scope, ScopeCol};
use super::select::cmp_nulls;
use super::QueryResult;
use crate::database::Database;
use crate::error::{Error, Result};
use crate::expr::BExpr;
use crate::sql::ast::{Expr, LockWait, Select, SelectItem, TableFactor};
use crate::storage::mvcc;
use crate::storage::{Claim, RowId};
use crate::value::Value;

impl Database {
    /// Runs a `SELECT` that has a locking clause.
    pub(crate) fn exec_locking_select(&mut self, sel: &Select) -> Result<QueryResult> {
        let mut plain = sel.clone();
        let lock = plain.lock.take().expect("a locking select");
        let Some((table, alias)) = self.lockable(&plain, &lock.of) else { return self.run_select(&plain) };
        if !self.mvcc {
            return self.run_select(&plain); // one transaction at a time: nobody to lock against
        }
        let auto = self.tx.is_none();
        if auto {
            self.start_tx(true); // outside a transaction the claim lasts for the statement
        }
        let mark = self.undo_mark();
        let result = self.lock_and_read(&plain, &table, &alias, lock.wait);
        if result.is_err() {
            self.release_to(mark);
        }
        if auto {
            return match result {
                Ok(r) => self.commit().map(|()| r),
                Err(e) => {
                    let _ = self.rollback();
                    Err(e)
                }
            };
        }
        result
    }

    /// The table and the name it goes by in `sel`, if `sel` is one this can lock.
    fn lockable(&self, sel: &Select, of: &[String]) -> Option<(String, String)> {
        if sel.crosstab.is_some() || sel.with.is_some() || !sel.unions.is_empty() {
            return None;
        }
        let from = sel.from.as_ref()?;
        let TableFactor::Named { name, alias, .. } = &from.first else { return None };
        if !from.joins.is_empty() {
            return None;
        }
        let shown = alias.clone().unwrap_or_else(|| name.clone());
        let t = self.table(name)?;
        if self.has_children(name) || self.is_partitioned(name) || self.rls_expr(name, "select", false).ok()?.is_some() {
            return None;
        }
        if !of.is_empty() && !of.iter().any(|o| o.eq_ignore_ascii_case(&shown) || o.eq_ignore_ascii_case(name)) {
            return None;
        }
        Some((t.name().to_string(), shown))
    }

    fn lock_and_read(&mut self, plain: &Select, table: &str, alias: &str, wait: LockWait) -> Result<QueryResult> {
        let candidates = self.lock_candidates(plain, table, alias)?;
        let mut limit = plain.limit.map(|l| l.saturating_add(plain.offset.unwrap_or(0)));
        if let Some(top) = plain.top {
            limit = Some(limit.map_or(top, |l| l.min(top)));
        }
        if !plain.group_by.is_empty() || plain.distinct {
            limit = None; // the limit counts groups, not rows
        }
        let mut mine: HashSet<RowId> = HashSet::new();
        let mut left_out = false;
        for id in candidates {
            if limit.is_some_and(|l| mine.len() >= l) {
                break;
            }
            super::budget::tick()?;
            match self.claim_row(table, id) {
                Ok(Claim::Taken(_) | Claim::Mine) => {
                    mine.insert(id);
                }
                Ok(Claim::Gone) => left_out = true,
                Err(Error::Busy(m)) => match wait {
                    LockWait::Wait => return Err(Error::Busy(m)),
                    LockWait::NoWait => return Err(Error::conflict(format!("could not obtain the lock on a row of '{table}' (NOWAIT): {m}"))),
                    LockWait::SkipLocked => left_out = true,
                },
                Err(e) => return Err(e),
            }
        }
        if left_out {
            // Rows that were not claimed must not be returned either.
            return mvcc::only_rows(table, mine, || self.run_select(plain));
        }
        self.run_select(plain)
    }

    /// The rows of `table` that `sel` selects, in the order it selects them, as seen by the transaction's snapshot.
    fn lock_candidates(&self, sel: &Select, table: &str, alias: &str) -> Result<Vec<RowId>> {
        let t = self.table_checked(table)?;
        let schema = t.schema();
        let scope = Scope { cols: schema.columns.iter().map(|c| ScopeCol { table: alias.to_string(), name: c.name.clone(), hidden: false }).collect() };
        let mut binder = Binder::new(self, &scope, 0);
        binder.allow_corr = true;
        let filter = sel.filter.as_ref().map(|f| binder.bind(f)).transpose()?;
        let corr = std::mem::take(&mut binder.corr);
        let hits = self.matching_with_corr(t, filter, &corr, schema.columns.len())?;
        if sel.order_by.is_empty() {
            return Ok(hits.into_iter().map(|(id, _)| id).collect());
        }
        let mut keys: Vec<(BExpr, bool, Option<bool>)> = Vec::new();
        for o in &sel.order_by {
            let e = self.order_expr(sel, &o.expr).unwrap_or_else(|| o.expr.clone());
            let bound = Binder::new(self, &scope, 0)
                .bind(&e)
                .map_err(|_| Error::runtime("FOR UPDATE is supported with an ORDER BY of the columns of the table"))?;
            keys.push((bound, o.desc, o.nulls_first));
        }
        let mut rows: Vec<(RowId, Vec<Value>)> = Vec::with_capacity(hits.len());
        for (id, extended) in hits {
            let row = match &extended {
                Some(r) => r,
                None => t.get(id).expect("id from live scan"),
            };
            let k = keys.iter().map(|(e, _, _)| e.eval(row).map(|v| v.into_owned())).collect::<Result<Vec<_>>>()?;
            rows.push((id, k));
        }
        rows.sort_by(|a, b| {
            for (i, (_, desc, nulls_first)) in keys.iter().enumerate() {
                let o = cmp_nulls(&a.1[i], &b.1[i], *desc, *nulls_first);
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        });
        Ok(rows.into_iter().map(|(id, _)| id).collect())
    }

    /// An `ORDER BY` term written as a position or the alias of a select item, as the expression it stands for.
    fn order_expr(&self, sel: &Select, e: &Expr) -> Option<Expr> {
        let item = |i: usize| match sel.items.get(i) {
            Some(SelectItem::Expr { expr, .. }) => Some(expr.clone()),
            _ => None,
        };
        match e {
            Expr::Literal(Value::Int(n)) if *n >= 1 => item(*n as usize - 1),
            Expr::Column { table: None, name } => sel.items.iter().find_map(|i| match i {
                SelectItem::Expr { expr, alias: Some(a) } if a.eq_ignore_ascii_case(name) => Some(expr.clone()),
                _ => None,
            }),
            _ => None,
        }
    }
}
