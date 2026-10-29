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
use crate::schema::IndexDef;
use crate::sql::ast::{BinOp, Expr, JoinKind};
use crate::storage::Table;
use crate::value::Value;

/// Factors the conditions every branch of an `OR` shares out of it: `(a AND x) OR (a AND y)` becomes `a AND (x OR y)`.
/// The two are equal in three-valued logic, and the shared conditions, join keys among them, are then conditions of the
/// query itself, so the joins can use them instead of building every combination of rows first (TPC-DS 13, 48).
pub(crate) fn factor_or(e: &Expr) -> Expr {
    fn split(e: &Expr, op: BinOp, out: &mut Vec<Expr>) {
        match e {
            Expr::Binary { op: o, left, right } if *o == op => {
                split(left, op, out);
                split(right, op, out);
            }
            other => out.push(other.clone()),
        }
    }
    fn join(mut parts: Vec<Expr>, op: BinOp) -> Option<Expr> {
        let last = parts.pop()?;
        Some(parts.into_iter().rev().fold(last, |acc, x| Expr::Binary { op, left: Box::new(x), right: Box::new(acc) }))
    }
    match e {
        Expr::Binary { op: BinOp::And, left, right } => {
            Expr::Binary { op: BinOp::And, left: Box::new(factor_or(left)), right: Box::new(factor_or(right)) }
        }
        Expr::Binary { op: BinOp::Or, .. } => {
            let mut branches = Vec::new();
            split(e, BinOp::Or, &mut branches);
            let mut parts: Vec<Vec<Expr>> = branches
                .iter()
                .map(|b| {
                    let mut v = Vec::new();
                    split(b, BinOp::And, &mut v);
                    v
                })
                .collect();
            let mut common: Vec<Expr> = Vec::new();
            for c in &parts[0] {
                if !common.contains(c) && parts[1..].iter().all(|p| p.contains(c)) {
                    common.push(c.clone());
                }
            }
            if common.is_empty() {
                return e.clone();
            }
            for p in &mut parts {
                p.retain(|c| !common.contains(c));
            }
            // a branch with nothing left is true, and so is the whole OR
            let rest = if parts.iter().any(Vec::is_empty) {
                None
            } else {
                join(parts.into_iter().filter_map(|p| join(p, BinOp::And)).collect(), BinOp::Or)
            };
            common.extend(rest);
            join(common, BinOp::And).expect("a shared condition")
        }
        other => other.clone(),
    }
}

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

/// How many rows an index probe finds for one left row when the join equates the first `k` columns of `def`: one
/// for a unique index that is fully equated, otherwise the table's rows over the distinct values of those columns.
pub(crate) fn probe_fanout(t: &Table, def: &IndexDef, k: usize) -> f64 {
    if def.unique && k >= def.columns.len() {
        return 1.0;
    }
    let n = t.estimated_len() as f64;
    let distinct: f64 = def.columns.iter().take(k).map(|&c| t.column_stats(c).distinct.max(1.0)).product();
    (n / distinct.min(n.max(1.0))).max(1.0)
}

/// Whether probing an index `left` times, finding `fanout` rows each time, reads fewer rows than reading the table
/// once to hash it. An index on a column with few values (the warehouse of every row) does not.
pub(crate) fn probe_pays(t: &Table, fanout: f64, left: f64) -> bool {
    left * fanout <= t.estimated_len() as f64 + left
}

/// `(this source's column, another source's column)` for each equality of two plain columns.
type JoinPairs = Vec<(usize, usize)>;

/// How a source is reached from the rows already joined, the pairs of columns it is joined on, and (when an index is
/// probed) the fewest rows one probe finds.
fn reach(source: &Source<'_>, s: usize, placed: SourceMask, preds: &[Pred], offs: &[usize], left: f64) -> (Reach, JoinPairs, Option<f64>) {
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
        return (Reach::Cross, pairs, None);
    }
    if let SourceData::Table(t) = &source.data {
        let mut best = Reach::Hash;
        let mut fan: Option<f64> = None;
        for def in t.index_defs() {
            let k = def.columns.iter().take_while(|c| cols.contains(c)).count();
            if k > 0 && probe_pays(t, probe_fanout(t, def, k), left) {
                best = best.max(Reach::Index);
                let f = probe_fanout(t, def, k);
                fan = Some(fan.map_or(f, |g: f64| g.min(f)));
            }
            if def.unique && def.columns.iter().all(|c| cols.contains(c)) {
                best = Reach::Unique;
            }
        }
        return (best, pairs, fan);
    }
    (Reach::Hash, pairs, None)
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
        // The distinct values of the whole table, not of the rows left after its conditions: the rows that remain are a
        // share of the key domain, and a join with them keeps that share of the other side (365 days of a 73,049-day
        // table keep 0.5% of an inventory), where counting only the remaining keys would keep all of it.
        let _ = (mine_rows, other_rows);
        let dm = mt.column_stats(mc).distinct;
        let dn = ot.column_stats(oc - offs[other]).distinct;
        let d = dm.max(dn).max(1.0);
        best = Some(best.map_or(d, |b: f64| b.max(d)));
    }
    best
}

/// The order to join the sources in. The greedy order from each possible first source is costed by the values (rows times
/// the width of the rows) it builds, and the cheapest wins: starting from the smallest source is not always best, when it
/// links only to the largest table (a 5-row warehouse and a 12-million-row inventory, with the date filter on a third table).
/// The usual start, the smallest filtered source, is kept unless another one costs less than half as much.
pub(crate) fn choose_order(sources: &[Source<'_>], est: &[f64], preds: &[Pred], offs: &[usize], deps: &[SourceMask], late: SourceMask) -> Vec<usize> {
    let m = sources.len();
    let mut starts: Vec<usize> = (0..m).filter(|&s| deps[s] == 0 && late & bit(s) == 0).collect();
    starts.sort_by(|&a, &b| est[a].partial_cmp(&est[b]).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b)));
    assert!(!starts.is_empty(), "at least one source");
    let tried = if m <= 16 { starts.len() } else { 1 };
    let mut best: Option<(Vec<usize>, f64)> = None;
    for &start in starts.iter().take(tried) {
        let (order, cost) = greedy_order(sources, est, preds, offs, deps, late, start);
        if std::env::var_os("ACCESSOR_ORDER_DEBUG").is_some() {
            let names: Vec<&str> = order.iter().map(|&s| sources[s].alias.as_str()).collect();
            eprintln!("order from {}: cost {cost:.0}: {}", sources[start].alias, names.join(" -> "));
        }
        if best.as_ref().is_none_or(|(_, c)| cost < c * 0.5) {
            best = Some((order, cost));
        }
    }
    best.expect("an order").0
}

/// Starting from `start`, keeps adding the source that leaves the smallest intermediate result, one that is linked
/// to the rows already joined before any that would be a cross product. Returns the order and its cost in values built.
fn greedy_order(sources: &[Source<'_>], est: &[f64], preds: &[Pred], offs: &[usize], deps: &[SourceMask], late: SourceMask, start: usize) -> (Vec<usize>, f64) {
    // the `late` sources (null-extended by a left join) are joined after all the others, in the order written: they are
    // not part of the choice, and their cost is the same for every order
    let m = sources.len() - late.count_ones() as usize;
    let mut order = vec![start];
    let mut placed: SourceMask = 1 << start;
    let mut current = est[start];
    let mut width = sources[start].ncols() as f64;
    let mut cost = current * width;
    while order.len() < m {
        let mut best: Option<(usize, f64, f64, bool)> = None; // (source, result rows, work, a cross product)
        for s in 0..sources.len() {
            if placed & (1 << s) != 0 || deps[s] & !placed != 0 || late & bit(s) != 0 {
                continue;
            }
            let (r, pairs, fan) = reach(&sources[s], s, placed, preds, offs, current);
            // the share of the table that its own conditions leave: a probe finds rows of the whole table, and that share of them stays
            let share = match &sources[s].data {
                SourceData::Table(t) => (est[s] / (t.estimated_len() as f64).max(1.0)).min(1.0),
                _ => 1.0,
            };
            // About |A|·|B| / (the larger number of distinct join values), or the larger input when
            // nothing is known (the foreign-key-to-key pattern).
            let joined = || match join_distinct(sources, s, &pairs, offs, est[s], current) {
                Some(d) => (current * est[s] / d).max(1.0),
                None => current.max(est[s]),
            };
            let (out, work) = match r {
                Reach::Unique => ((current * share).max(1.0), current),
                Reach::Index => (fan.map_or_else(joined, |f| (current * f * share).max(1.0)), current),
                Reach::Hash => (joined(), current + est[s]),
                Reach::Cross => (current * est[s], current * est[s]),
            };
            // a cross product only when nothing else is linked to the rows joined so far: a small one now (two
            // nations) multiplies every later step, and the source that links them may come later
            let cross = r == Reach::Cross;
            let better = match &best {
                None => true,
                Some((_, _, _, bc)) if *bc != cross => *bc,
                Some((_, bo, bw, _)) => out < *bo || (out == *bo && work < *bw),
            };
            if better {
                best = Some((s, out, work, cross));
            }
        }
        let (s, out, work, cross) = best.expect("a source remains");
        if std::env::var_os("ACCESSOR_ORDER_DEBUG2").is_some() && sources[start].alias.starts_with(['d', 'w']) {
            eprintln!("  from {}: add {} -> {out:.0} rows (work {work:.0}{}), table est {:.0}", sources[start].alias, sources[s].alias, if cross { ", CROSS" } else { "" }, est[s]);
        }
        order.push(s);
        placed |= 1 << s;
        current = out;
        width += sources[s].ncols() as f64;
        cost += current * width;
    }
    (order, cost)
}

/// A one-line description of a literal key, for plan notes.
pub(crate) fn describe_key(key: &[Value]) -> String {
    key.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")
}
