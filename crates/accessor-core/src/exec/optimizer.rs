//! The query optimiser.
//!
//! A query is planned in three decisions, each made from cheap statistics read
//! off the tables as they are (row counts, and exact counts of the rows an
//! index range would visit, up to a cap), so there is nothing to keep up to
//! date and no `ANALYZE` to run:
//!
//! 1. **Pushdown.** A `WHERE` or inner-join `ON` condition that reads only one
//!    table is applied while that table is scanned or probed, before any join,
//!    so joins see fewer rows. Conditions are never pushed to the nullable side
//!    of an outer join, where doing so would change the answer.
//! 2. **Join order.** A chain of inner and cross joins can be done in any
//!    order, so the optimiser picks one: it starts from the source with the
//!    fewest rows after its own conditions, then repeatedly adds the source that
//!    keeps the intermediate result smallest, preferring sources it can reach
//!    through an index. Outer joins keep the order written.
//! 3. **Access paths.** Each scan uses the best index for its conditions, and
//!    an `ORDER BY ... LIMIT` that matches an index reads the index in order and
//!    stops early instead of sorting everything.
//!
//! `EXPLAIN` runs a query and reports these decisions with the estimated and
//! actual row counts.

use std::sync::Mutex;

use super::select::{choose_access, Access, Source, SourceData};
use crate::database::Database;
use crate::expr::BExpr;
use crate::sql::ast::{BinOp, JoinKind};
use crate::value::Value;

/// More sources than this are joined in the order written, without pushdown.
pub(crate) const MAX_SOURCES: usize = 63;

/// Where `EXPLAIN` collects its notes. `None` outside an `EXPLAIN`.
#[derive(Default)]
pub(crate) struct PlanLog(pub(crate) Mutex<Option<Vec<String>>>);

impl Database {
    /// Records one line of the plan, if an `EXPLAIN` is collecting. The message is
    /// only built when needed, so planning pays nothing otherwise.
    pub(crate) fn note(&self, depth: u32, msg: impl FnOnce() -> String) {
        let mut log = self.plan_log.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(lines) = log.as_mut() {
            lines.push(format!("{}{}", "  ".repeat(depth as usize), msg()));
        }
    }

    /// Runs `f` with plan collection on and returns what it collected.
    pub(crate) fn collect_plan<T>(&self, f: impl FnOnce() -> T) -> (T, Vec<String>) {
        *self.plan_log.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(Vec::new());
        let out = f();
        let lines = self.plan_log.0.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default();
        (out, lines)
    }
}

/// A conjunct of the query's conditions, with the sources it reads.
pub(crate) struct Pred {
    pub expr: BExpr,
    /// Bit `s` is set when the condition reads source `s`.
    pub mask: u64,
    /// Whether it came from `WHERE` (as opposed to a join's `ON`).
    pub from_where: bool,
    pub used: bool,
}

/// The sources an expression reads, given where each source's columns start.
pub(crate) fn source_mask(e: &BExpr, offs: &[usize]) -> u64 {
    let mut m = 0u64;
    e.visit_cols(&mut |c| {
        let s = offs.partition_point(|&o| o <= c).saturating_sub(1);
        m |= 1 << s;
    });
    m
}

/// Whether source `s` can be padded with NULLs by an outer join, which makes it
/// unsafe to filter before the join.
pub(crate) fn nullable(kinds: &[JoinKind], s: usize) -> bool {
    kinds.iter().enumerate().any(|(j, k)| match k {
        JoinKind::Left => s == j + 1,
        JoinKind::Right => s <= j,
        JoinKind::Full => true,
        _ => false,
    })
}

/// Fewer rows than this are counted exactly when estimating; above it the
/// estimate is a fraction of the table.
const COUNT_CAP: usize = 4096;

/// A rough selectivity for a condition no index can answer.
fn selectivity(c: &BExpr) -> f64 {
    match c {
        BExpr::Bin(BinOp::Eq, ..) => 0.1,
        BExpr::Bin(BinOp::Ne, ..) => 0.9,
        BExpr::Bin(..) => 0.33,
        BExpr::Between { .. } => 0.25,
        BExpr::Like { .. } => 0.25,
        BExpr::InList { list, .. } => (0.1 * list.len() as f64).min(0.5),
        BExpr::InSet { set, .. } => (0.1 * set.len() as f64).min(0.5),
        BExpr::IsNull(_, false) => 0.05,
        _ => 0.5,
    }
}

/// An estimate of the rows `source` yields under `conds` (bound to the source's own columns).
pub(crate) fn estimate(source: &Source<'_>, conds: &[BExpr]) -> f64 {
    let (total, mut rows) = match &source.data {
        SourceData::Table(t) => {
            let n = t.len() as f64;
            if conds.is_empty() {
                return n.max(1.0);
            }
            let refs: Vec<&BExpr> = conds.iter().collect();
            match choose_access(t, &refs) {
                Access::Full => (n, n),
                Access::Prefix { idx, key } => {
                    let unique = t.index_defs().nth(idx).is_some_and(|d| d.unique && d.columns.len() == key.len());
                    let seen = if unique { 1 } else { t.index_prefix(idx, &key).take(COUNT_CAP).count() };
                    let est = if seen >= COUNT_CAP { n * 0.1 } else { seen as f64 };
                    (n, est)
                }
                Access::Range { idx, lo, hi } => {
                    let seen = t.index_range(idx, lo, hi).take(COUNT_CAP).count();
                    let est = if seen >= COUNT_CAP { n * 0.33 } else { seen as f64 };
                    (n, est)
                }
            }
        }
        SourceData::Rows(r) => (r.len() as f64, r.len() as f64),
    };
    // Conditions the index did not answer thin the rows out further.
    let answered = match &source.data {
        SourceData::Table(t) => {
            let refs: Vec<&BExpr> = conds.iter().collect();
            match choose_access(t, &refs) {
                Access::Full => 0,
                Access::Prefix { key, .. } => key.len(),
                Access::Range { .. } => 1,
            }
        }
        SourceData::Rows(_) => 0,
    };
    for c in conds.iter().skip(answered.min(conds.len())) {
        rows *= selectivity(c);
    }
    rows.clamp(if total > 0.0 { 1.0 } else { 0.0 }, total.max(1.0))
}

/// How a source can be reached from rows already joined.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Reach {
    /// No equality links it to the joined rows: a cross product.
    Cross,
    /// An equality links them; the source would be hashed.
    Hash,
    /// An index on the source answers the equality.
    Index,
    /// A unique index does: at most one match per row.
    Unique,
}

fn reach(source: &Source<'_>, s: usize, placed: u64, preds: &[Pred], offs: &[usize]) -> Reach {
    let me = 1u64 << s;
    let mut cols: Vec<usize> = Vec::new();
    let mut linked = false;
    for p in preds {
        if p.mask & me == 0 || p.mask & !(placed | me) != 0 {
            continue;
        }
        let BExpr::Bin(BinOp::Eq, a, b) = &p.expr else { continue };
        let (ma, mb) = (source_mask(a, offs), source_mask(b, offs));
        let (mine, other) = if ma == me && mb != 0 && mb & me == 0 {
            (a, mb)
        } else if mb == me && ma != 0 && ma & me == 0 {
            (b, ma)
        } else {
            continue;
        };
        let _ = other;
        linked = true;
        if let BExpr::Col(c) = &**mine {
            cols.push(*c - offs[s]);
        }
    }
    if !linked {
        return Reach::Cross;
    }
    if let SourceData::Table(t) = &source.data {
        let mut best = Reach::Hash;
        for def in t.index_defs() {
            if def.columns.first().is_some_and(|c| cols.contains(c)) {
                best = best.max(Reach::Index);
            }
            if def.unique && def.columns.iter().all(|c| cols.contains(c)) {
                best = Reach::Unique;
            }
        }
        return best;
    }
    Reach::Hash
}

/// The order to join the sources in: start from the smallest filtered source,
/// then keep adding the one that leaves the smallest intermediate result.
pub(crate) fn choose_order(sources: &[Source<'_>], est: &[f64], preds: &[Pred], offs: &[usize]) -> Vec<usize> {
    let m = sources.len();
    let start = (0..m)
        .min_by(|&a, &b| est[a].partial_cmp(&est[b]).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b)))
        .expect("at least one source");
    let mut order = vec![start];
    let mut placed = 1u64 << start;
    let mut current = est[start];
    while order.len() < m {
        let mut best: Option<(usize, f64, f64)> = None; // (source, result rows, work)
        for s in 0..m {
            if placed & (1 << s) != 0 {
                continue;
            }
            let r = reach(&sources[s], s, placed, preds, offs);
            let (out, work) = match r {
                Reach::Unique => (current, current),
                Reach::Index => (current.max(est[s]), current),
                Reach::Hash => (current.max(est[s]), current + est[s]),
                Reach::Cross => (current * est[s], current * est[s]),
            };
            let better = match &best {
                None => true,
                Some((_, bo, bw)) => out < *bo || (out == *bo && work < *bw),
            };
            if better {
                best = Some((s, out, work));
            }
        }
        let (s, out, _) = best.expect("a source remains");
        order.push(s);
        placed |= 1 << s;
        current = out;
    }
    order
}

/// A one-line description of a literal key, for plan notes.
pub(crate) fn describe_key(key: &[Value]) -> String {
    key.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")
}
