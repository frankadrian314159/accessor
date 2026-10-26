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

use std::ops::Bound;
use std::sync::Mutex;

use super::select::{choose_access, Access, Source, SourceData};
use crate::database::Database;
use crate::expr::BExpr;
use crate::sql::ast::{BinOp, JoinKind};
use crate::storage::Table;
use crate::value::Value;

/// More sources than this are joined in the order written, without pushdown.
/// Which of a query's sources (tables, subqueries, ...) a condition reads: one bit each.
pub(crate) type SourceMask = u128;

/// The bit for source `s` (none for a source beyond the mask: a query that long is not optimized, and the
/// bits are not used).
pub(crate) fn bit(s: usize) -> SourceMask {
    u32::try_from(s).ok().and_then(|s| (1 as SourceMask).checked_shl(s)).unwrap_or(0)
}

/// The most sources in one `FROM` clause that the optimizer will order; more are joined as written.
pub(crate) const MAX_SOURCES: usize = SourceMask::BITS as usize;

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

    /// Runs `f` without adding to the plan, for internal probes such as learning the columns
    /// of a LATERAL item.
    pub(crate) fn unlogged<T>(&self, f: impl FnOnce() -> T) -> T {
        let saved = self.plan_log.0.lock().unwrap_or_else(|e| e.into_inner()).take();
        let out = f();
        *self.plan_log.0.lock().unwrap_or_else(|e| e.into_inner()) = saved;
        out
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
    pub mask: SourceMask,
    /// Whether it came from `WHERE` (as opposed to a join's `ON`).
    pub from_where: bool,
    pub used: bool,
}

/// The sources an expression reads, given where each source's columns start.
pub(crate) fn source_mask(e: &BExpr, offs: &[usize]) -> SourceMask {
    let mut m: SourceMask = 0;
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

/// A column of the table compared with a constant, whichever way round it is written.
fn col_const<'a>(a: &'a BExpr, b: &'a BExpr) -> Option<(usize, &'a Value, bool)> {
    match (a, b) {
        (BExpr::Col(c), BExpr::Lit(v)) => Some((*c, v, false)),
        (BExpr::Lit(v), BExpr::Col(c)) => Some((*c, v, true)),
        _ => None,
    }
}

/// The fraction of a table's rows that satisfy `c`, from the statistics of its columns where it
/// compares a column with constants, and from fixed guesses where it does not.
pub(crate) fn cond_selectivity(table: Option<&Table>, c: &BExpr) -> f64 {
    let stats_for = |col: usize| table.filter(|t| col < t.schema().columns.len()).map(|t| t.column_stats(col));
    let guess = selectivity(c);
    let from_stats = || -> Option<f64> {
        match c {
            BExpr::Bin(BinOp::And, a, b) => Some(cond_selectivity(table, a) * cond_selectivity(table, b)),
            BExpr::Bin(BinOp::Or, a, b) => {
                let (x, y) = (cond_selectivity(table, a), cond_selectivity(table, b));
                Some((x + y - x * y).clamp(0.0, 1.0))
            }
            BExpr::Not(e) => Some(1.0 - cond_selectivity(table, e)),
            BExpr::Bin(op, a, b) => {
                let (col, v, flipped) = col_const(a, b)?;
                let s = stats_for(col)?;
                let nulls = s.null_fraction();
                let op = if flipped {
                    match op {
                        BinOp::Lt => BinOp::Gt,
                        BinOp::Le => BinOp::Ge,
                        BinOp::Gt => BinOp::Lt,
                        BinOp::Ge => BinOp::Le,
                        o => *o,
                    }
                } else {
                    *op
                };
                match op {
                    BinOp::Eq => s.eq(v),
                    BinOp::Ne => s.eq(v).map(|e| (1.0 - nulls - e).max(0.0)),
                    BinOp::Lt => s.range(&Bound::Unbounded, &Bound::Excluded(v.clone())),
                    BinOp::Le => s.range(&Bound::Unbounded, &Bound::Included(v.clone())),
                    BinOp::Gt => s.range(&Bound::Excluded(v.clone()), &Bound::Unbounded),
                    BinOp::Ge => s.range(&Bound::Included(v.clone()), &Bound::Unbounded),
                    _ => None,
                }
            }
            BExpr::Between { expr, low, high, negated } => {
                let (BExpr::Col(col), BExpr::Lit(lo), BExpr::Lit(hi)) = (&**expr, &**low, &**high) else { return None };
                let s = stats_for(*col)?;
                let inside = s.range(&Bound::Included(lo.clone()), &Bound::Included(hi.clone()))?;
                Some(if *negated { (1.0 - s.null_fraction() - inside).max(0.0) } else { inside })
            }
            BExpr::IsNull(e, negated) => {
                let BExpr::Col(col) = &**e else { return None };
                let n = stats_for(*col)?.null_fraction();
                Some(if *negated { 1.0 - n } else { n })
            }
            BExpr::InList { expr, list, negated } => {
                let BExpr::Col(col) = &**expr else { return None };
                let s = stats_for(*col)?;
                let mut total = 0.0;
                for item in list {
                    let BExpr::Lit(v) = item else { return None };
                    total += s.eq(v)?;
                }
                let total = total.min(1.0 - s.null_fraction());
                Some(if *negated { (1.0 - s.null_fraction() - total).max(0.0) } else { total })
            }
            BExpr::InSet { expr, set, negated, .. } => {
                let BExpr::Col(col) = &**expr else { return None };
                let s = stats_for(*col)?;
                let mut total = 0.0;
                for v in set {
                    total += s.eq(v)?;
                }
                let total = total.min(1.0 - s.null_fraction());
                Some(if *negated { (1.0 - s.null_fraction() - total).max(0.0) } else { total })
            }
            // A pattern with a fixed beginning picks out a range of the text values.
            BExpr::Like { expr, pattern, negated: false, .. } => {
                let (BExpr::Col(col), BExpr::Lit(Value::Text(p))) = (&**expr, &**pattern) else { return None };
                let prefix: String = p.chars().take_while(|ch| !matches!(ch, '*' | '?' | '#' | '[' | '%' | '_')).collect();
                if prefix.is_empty() {
                    return None;
                }
                let s = stats_for(*col)?;
                let top = format!("{prefix}\u{10FFFF}");
                s.range(&Bound::Included(Value::text(&prefix)), &Bound::Included(Value::text(&top)))
            }
            _ => None,
        }
    };
    from_stats().map_or(guess, |s| s.clamp(0.0, 1.0))
}

/// An estimate of the rows `source` yields under `conds` (bound to the source's own columns).
pub(crate) fn estimate(source: &Source<'_>, conds: &[BExpr]) -> f64 {
    let (total, mut rows) = match &source.data {
        SourceData::Table(t) => {
            let n = t.estimated_len() as f64;
            if conds.is_empty() {
                return n.max(1.0);
            }
            let refs: Vec<&BExpr> = conds.iter().collect();
            match choose_access(t, &refs) {
                Access::Full => (n, n),
                Access::Prefix { idx, key } => {
                    let def = t.index_defs().nth(idx);
                    let unique = def.is_some_and(|d| d.unique && d.columns.len() == key.len());
                    let seen = if unique { 1 } else { t.index_prefix(idx, &key).take(COUNT_CAP).count() };
                    let est = if seen >= COUNT_CAP {
                        // Too many to count: use what the statistics say about these values.
                        let sel = def.map(|d| {
                            d.columns.iter().zip(&key).map(|(&c, v)| t.column_stats(c).eq(v).unwrap_or(0.1)).product::<f64>()
                        });
                        n * sel.unwrap_or(0.1)
                    } else {
                        seen as f64
                    };
                    (n, est)
                }
                Access::Range { idx, lo, hi } => {
                    let col = t.index_defs().nth(idx).and_then(|d| d.columns.first().copied());
                    let (lo_b, hi_b) = (lo.clone(), hi.clone());
                    let seen = t.index_range(idx, lo, hi).take(COUNT_CAP).count();
                    let est = if seen >= COUNT_CAP {
                        let sel = col.and_then(|c| t.column_stats(c).range(&lo_b, &hi_b));
                        n * sel.unwrap_or(0.33)
                    } else {
                        seen as f64
                    };
                    (n, est)
                }
            }
        }
        SourceData::Rows(r) => (r.len() as f64, r.len() as f64),
        // Evaluated once per left row; a handful of rows each is the usual shape.
        SourceData::Lateral(_) => (10.0, 10.0),
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
        SourceData::Rows(_) | SourceData::Lateral(_) => 0,
    };
    let table = match &source.data {
        SourceData::Table(t) => Some(*t),
        _ => None,
    };
    for c in conds.iter().skip(answered.min(conds.len())) {
        rows *= cond_selectivity(table, c);
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

/// `(this source's column, another source's column)` for each equality of two plain columns.
type JoinPairs = Vec<(usize, usize)>;

fn reach(source: &Source<'_>, s: usize, placed: SourceMask, preds: &[Pred], offs: &[usize]) -> (Reach, JoinPairs) {
    let me: SourceMask = 1 << s;
    let mut cols: Vec<usize> = Vec::new();
    let mut pairs: JoinPairs = Vec::new();
    let mut linked = false;
    for p in preds {
        if p.mask & me == 0 || p.mask & !(placed | me) != 0 {
            continue;
        }
        let BExpr::Bin(BinOp::Eq, a, b) = &p.expr else { continue };
        let (ma, mb) = (source_mask(a, offs), source_mask(b, offs));
        let (mine, theirs) = if ma == me && mb != 0 && mb & me == 0 {
            (a, b)
        } else if mb == me && ma != 0 && ma & me == 0 {
            (b, a)
        } else {
            continue;
        };
        linked = true;
        if let BExpr::Col(c) = &**mine {
            cols.push(*c - offs[s]);
            if let BExpr::Col(oc) = &**theirs {
                pairs.push((*c - offs[s], *oc));
            }
        }
    }
    if !linked {
        return (Reach::Cross, pairs);
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
        return (best, pairs);
    }
    (Reach::Hash, pairs)
}

/// How many different values the join columns of an equality join have, taking the most selective
/// pair; `None` when there are no statistics for both sides.
fn join_distinct(sources: &[Source<'_>], s: usize, pairs: &JoinPairs, offs: &[usize], mine_rows: f64, other_rows: f64) -> Option<f64> {
    let table = |src: usize| match &sources[src].data {
        SourceData::Table(t) => Some(*t),
        _ => None,
    };
    let mut best: Option<f64> = None;
    for &(mc, oc) in pairs {
        let other = offs.partition_point(|&o| o <= oc).saturating_sub(1);
        let (Some(mt), Some(ot)) = (table(s), table(other)) else { continue };
        let dm = mt.column_stats(mc).distinct.min(mine_rows.max(1.0));
        let dn = ot.column_stats(oc - offs[other]).distinct.min(other_rows.max(1.0));
        let d = dm.max(dn);
        best = Some(best.map_or(d, |b: f64| b.max(d)));
    }
    best
}

/// The order to join the sources in: start from the smallest filtered source,
/// then keep adding the one that leaves the smallest intermediate result.
pub(crate) fn choose_order(sources: &[Source<'_>], est: &[f64], preds: &[Pred], offs: &[usize], deps: &[SourceMask]) -> Vec<usize> {
    let m = sources.len();
    let start = (0..m)
        .filter(|&s| deps[s] == 0)
        .min_by(|&a, &b| est[a].partial_cmp(&est[b]).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b)))
        .expect("at least one source");
    let mut order = vec![start];
    let mut placed: SourceMask = 1 << start;
    let mut current = est[start];
    while order.len() < m {
        let mut best: Option<(usize, f64, f64)> = None; // (source, result rows, work)
        for s in 0..m {
            if placed & (1 << s) != 0 || deps[s] & !placed != 0 {
                continue;
            }
            let (r, pairs) = reach(&sources[s], s, placed, preds, offs);
            // About |A|·|B| / (the larger number of distinct join values), or the larger input when
            // nothing is known (the foreign-key-to-key pattern).
            let joined = || match join_distinct(sources, s, &pairs, offs, est[s], current) {
                Some(d) => (current * est[s] / d).max(1.0),
                None => current.max(est[s]),
            };
            let (out, work) = match r {
                Reach::Unique => (current, current),
                Reach::Index => (joined(), current),
                Reach::Hash => (joined(), current + est[s]),
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
