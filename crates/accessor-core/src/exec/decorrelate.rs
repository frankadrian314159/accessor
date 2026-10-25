//! Semi-joins: running a correlated sub-query once for the whole outer query.
//!
//! A correlated sub-query such as
//!
//! ```sql
//! EXISTS (SELECT 1 FROM Orders o WHERE o.CustID = c.ID AND o.Total > 100)
//! ```
//!
//! is normally run once for each distinct value of `c.ID`. When all the correlation is through
//! equalities (`inner expression = outer column`), the sub-query can instead be run *once* without
//! them, selecting the inner expressions as well, and its rows kept in a hash table by those keys.
//! Each outer row then finds its answer by a lookup. This turns "one query per customer" into "one
//! pass over the orders", which is what a database calls a semi-join (or, for `NOT EXISTS`, an
//! anti-join). It also handles `IN (...)` and scalar sub-queries, including the common
//! `(SELECT MAX(x) FROM t WHERE t.k = outer.k)`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::agg::AggFn;
use super::bind::{contains_agg, CorrKind, CorrSpec};
use crate::database::Database;
use crate::error::Result;
use crate::sql::ast::*;
use crate::sql::walk::{walk_expr, walk_select};
use crate::value::{Row, Value};

/// A correlated sub-query rewritten to run once.
pub(crate) struct Decorrelated {
    /// The sub-query without its correlation: it selects one column for each key, then (for a
    /// value or `IN`) the value.
    pub query: Select,
    /// The outer row position each key column is compared with.
    pub outer: Vec<usize>,
    /// The inner expression of each key, to find an index for it.
    pub keys: Vec<Expr>,
    /// A scalar `COUNT`: a key with no rows counts 0, where other aggregates give NULL.
    pub counts: bool,
}

fn split_and(e: Expr, out: &mut Vec<Expr>) {
    match e {
        Expr::Binary { op: BinOp::And, left, right } => {
            split_and(*left, out);
            split_and(*right, out);
        }
        other => out.push(other),
    }
}

fn param_count(e: &Expr) -> usize {
    let mut n = 0;
    let _ = walk_expr(&mut e.clone(), &mut |x| {
        if matches!(x, Expr::Param(_)) {
            n += 1;
        }
        Ok(())
    });
    n
}

/// Rewrites the correlated sub-query of `spec`, or `None` if its correlation is not purely a set
/// of equalities against inner expressions, or its shape would give different answers.
pub(crate) fn decorrelate(spec: &CorrSpec) -> Option<Decorrelated> {
    let q = &spec.query;
    if q.distinct
        || q.top.is_some()
        || q.offset.is_some()
        || !q.group_by.is_empty()
        || q.having.is_some()
        || !q.group_sets.is_empty()
        || !q.unions.is_empty()
        || q.with.is_some()
        || q.crosstab.is_some()
        || !q.windows.is_empty()
        || q.from.is_none()
    {
        return None;
    }
    // The limit of 1 that EXISTS gets does not change its answer; any other limit might.
    match spec.kind {
        CorrKind::Exists { .. } if q.limit.is_some_and(|l| l != 1) => return None,
        CorrKind::Scalar | CorrKind::In { .. } if q.limit.is_some() => return None,
        _ => {}
    }
    // The value the sub-query yields.
    let mut counts = false;
    let mut aggregate = false;
    let value = match &spec.kind {
        CorrKind::Exists { .. } => None,
        kind => {
            let [SelectItem::Expr { expr, .. }] = q.items.as_slice() else { return None };
            if contains_agg(expr) {
                // Only a plain aggregate call, over a scalar sub-query.
                let Expr::Function { name, args, .. } = expr else { return None };
                let func = AggFn::from_name(name)?;
                if !matches!(kind, CorrKind::Scalar) || args.iter().any(contains_agg) {
                    return None;
                }
                counts = func == AggFn::Count;
                aggregate = true;
            }
            Some(expr.clone())
        }
    };
    // The equalities to take out of the WHERE clause.
    let mut conjuncts = Vec::new();
    split_and(q.filter.clone()?, &mut conjuncts);
    let (mut keys, mut params, mut rest) = (Vec::new(), Vec::new(), Vec::new());
    for c in conjuncts {
        let split = match &c {
            Expr::Binary { op: BinOp::Eq, left, right } => match (&**left, &**right) {
                (inner, Expr::Param(p)) | (Expr::Param(p), inner) if param_count(inner) == 0 => Some((inner.clone(), p.clone())),
                _ => None,
            },
            _ => None,
        };
        match split {
            Some((inner, p)) => {
                keys.push(inner);
                params.push(p);
            }
            None => rest.push(c),
        }
    }
    if keys.is_empty() {
        return None;
    }
    // Every use of an outer value must have been one of those equalities.
    let mut all = q.clone();
    let mut total = 0;
    let _ = walk_select(&mut all, &mut |x| {
        if matches!(x, Expr::Param(_)) {
            total += 1;
        }
        Ok(())
    });
    if total != keys.len() {
        return None;
    }
    let outer = params
        .iter()
        .map(|p| spec.outer.iter().find(|(n, _)| n == p).map(|(_, pos)| *pos))
        .collect::<Option<Vec<_>>>()?;

    let mut nq = q.clone();
    nq.filter = rest.into_iter().reduce(|a, b| Expr::Binary { op: BinOp::And, left: Box::new(a), right: Box::new(b) });
    nq.limit = None;
    nq.order_by.clear();
    nq.items = keys
        .iter()
        .enumerate()
        .map(|(i, k)| SelectItem::Expr { expr: k.clone(), alias: Some(format!("__k{i}")) })
        .chain(value.map(|v| SelectItem::Expr { expr: v, alias: Some("__v".into()) }))
        .collect();
    if aggregate {
        nq.group_by = keys.clone();
    }
    Some(Decorrelated { query: nq, outer, keys, counts })
}

/// What kind of value this is, for deciding whether a hash lookup agrees with `=`.
fn family(v: &Value) -> u8 {
    match v {
        Value::Bool(_) => 1,
        Value::Text(_) => 4,
        Value::DateTime(_) => 3,
        Value::TimestampTz(_) => 7,
        Value::Interval(_) => 5,
        Value::Json(_) => 6,
        _ => 2,
    }
}

/// The answers of a decorrelated sub-query, by key.
pub(crate) struct Hashed {
    outer: Vec<usize>,
    map: HashMap<Vec<Value>, Arc<Vec<Row>>>,
    empty: Arc<Vec<Row>>,
    /// The kind of each key in the inner rows (`None`: no rows; `Some(0)`: mixed kinds).
    families: Vec<Option<u8>>,
    pub inner_rows: usize,
}

impl Hashed {
    pub(crate) fn key_of(&self, row: &Row) -> Vec<Value> {
        self.outer.iter().map(|&c| row[c].clone()).collect()
    }

    /// Whether looking `key` up gives the same answer as `=` would: both sides must be of the same kind.
    pub(crate) fn can_answer(&self, key: &[Value]) -> bool {
        key.iter().zip(&self.families).all(|(v, f)| v.is_null() || matches!(f, None) || *f == Some(family(v)))
    }

    pub(crate) fn lookup(&self, key: &[Value]) -> Arc<Vec<Row>> {
        if key.iter().any(Value::is_null) {
            return Arc::clone(&self.empty);
        }
        self.map.get(key).map_or_else(|| Arc::clone(&self.empty), Arc::clone)
    }
}

impl Database {
    /// Decides whether to run `spec` once (as a hash table) for these outer rows, and does so.
    pub(crate) fn hash_subquery(&self, spec: &CorrSpec, rows: &[Row], depth: u32) -> Result<Option<Hashed>> {
        if !self.optimizer_on {
            return Ok(None);
        }
        let Some(d) = decorrelate(spec) else { return Ok(None) };
        let distinct: HashSet<Vec<Value>> =
            rows.iter().map(|r| d.outer.iter().map(|&c| r[c].clone()).collect()).collect();
        let n = distinct.len();
        // One run per distinct key, each probing an index, against one scan of the inner rows.
        let from = d.query.from.as_ref().expect("checked by decorrelate");
        let worthwhile = match (&from.first, from.joins.is_empty()) {
            (TableFactor::Named { name, .. }, true) if self.table(name).is_some() => {
                let t = self.table(name).expect("checked");
                let indexed = d.keys.iter().all(|k| match k {
                    Expr::Column { name: c, .. } => t.schema().column_index(c).is_some_and(|p| t.index_defs().any(|ix| ix.columns.first() == Some(&p))),
                    _ => false,
                });
                n >= 2 && (!indexed || n.saturating_mul(50) >= t.len())
            }
            _ => n >= 32,
        };
        if !worthwhile {
            return Ok(None);
        }
        let res = self.run_select_depth(&d.query, depth + 1)?;
        let k = d.outer.len();
        let mut families: Vec<Option<u8>> = vec![None; k];
        let mut grouped: HashMap<Vec<Value>, Vec<Row>> = HashMap::new();
        for mut row in res.rows.iter().cloned() {
            if row[..k].iter().any(Value::is_null) {
                continue;
            }
            for (i, v) in row[..k].iter().enumerate() {
                let f = family(v);
                families[i] = match families[i] {
                    None => Some(f),
                    Some(g) if g == f => Some(g),
                    _ => Some(0),
                };
            }
            let tail = row.split_off(k);
            grouped.entry(row).or_default().push(tail);
        }
        let inner_rows = res.rows.len();
        let map = grouped.into_iter().map(|(key, rows)| (key, Arc::new(rows))).collect();
        let empty = Arc::new(if d.counts { vec![vec![Value::Int(0)]] } else { Vec::new() });
        Ok(Some(Hashed { outer: d.outer, map, empty, families, inner_rows }))
    }
}
