//! SELECT planning and execution: index selection, joins (index-nested-loop,
//! hash, nested-loop), grouping, DISTINCT, ORDER BY with top-k, TOP/LIMIT.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::Bound;

use super::agg::{AggFn, AggSpec, AggState};
use super::bind::{contains_agg, Binder, GroupCtx, Scope, ScopeCol};
use super::QueryResult;
use crate::database::Database;
use crate::error::{Error, Result};
use crate::expr::BExpr;
use crate::sql::ast::*;
use crate::storage::{RowId, Table};
use crate::value::{DataType, Row, Value};

const MAX_DEPTH: u32 = 16;

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

enum SourceData<'a> {
    Table(&'a Table),
    Rows(Vec<Row>),
}

struct Source<'a> {
    data: SourceData<'a>,
    alias: String,
    names: Vec<String>,
}

impl Source<'_> {
    fn ncols(&self) -> usize {
        self.names.len()
    }
    fn slot_count(&self) -> usize {
        match &self.data {
            SourceData::Table(t) => t.slot_count(),
            SourceData::Rows(r) => r.len(),
        }
    }
    fn get(&self, slot: usize) -> Option<&Row> {
        match &self.data {
            SourceData::Table(t) => t.get(slot as RowId),
            SourceData::Rows(r) => r.get(slot),
        }
    }
}

impl Database {
    fn resolve_factor(&self, f: &TableFactor, depth: u32) -> Result<Source<'_>> {
        match f {
            TableFactor::Named { name, alias } => {
                let alias = alias.clone().unwrap_or_else(|| name.clone());
                if let Some(t) = self.table(name) {
                    let names = t.schema().columns.iter().map(|c| c.name.clone()).collect();
                    return Ok(Source { data: SourceData::Table(t), alias, names });
                }
                if self.view_sql(name).is_some() {
                    let (params, q) = self.resolve_query(name, 0)?;
                    if !params.is_empty() {
                        return Err(Error::runtime(format!(
                            "the query '{name}' needs parameters; run it with EXEC [{name}] ..."
                        )));
                    }
                    let res = self.run_select_depth(&q, depth + 1)?;
                    return Ok(Source { data: SourceData::Rows(res.rows), alias, names: res.columns });
                }
                Err(Error::catalog(format!("the table or query '{name}' does not exist")))
            }
            TableFactor::Derived { query, alias } => {
                let res = self.run_select_depth(query, depth + 1)?;
                Ok(Source { data: SourceData::Rows(res.rows), alias: alias.clone(), names: res.columns })
            }
        }
    }

    pub fn run_select(&self, sel: &Select) -> Result<QueryResult> {
        self.run_select_depth(sel, 0)
    }

    pub(crate) fn run_select_depth(&self, sel: &Select, depth: u32) -> Result<QueryResult> {
        if depth > MAX_DEPTH {
            return Err(Error::runtime("queries are nested too deeply (is a saved query referring to itself?)"));
        }

        // 1. Resolve sources and build the name scope.
        let mut sources = Vec::new();
        let mut scope = Scope::default();
        if let Some(from) = &sel.from {
            sources.push(self.resolve_factor(&from.first, depth)?);
            for j in &from.joins {
                sources.push(self.resolve_factor(&j.factor, depth)?);
            }
            for s in &sources {
                for n in &s.names {
                    scope.cols.push(ScopeCol { table: s.alias.clone(), name: n.clone() });
                }
            }
        }

        // 2. Expand the select list.
        let out_items = expand_items(sel, &scope)?;
        let n_vis = out_items.len();

        // 3. Bind everything.
        let is_agg = !sel.group_by.is_empty()
            || out_items.iter().any(|(_, e)| contains_agg(e))
            || sel.having.as_ref().is_some_and(contains_agg)
            || sel.order_by.iter().any(|o| contains_agg(&o.expr));

        let mut binder = Binder::new(self, &scope, depth);
        let filter = sel.filter.as_ref().map(|f| binder.bind(f)).transpose()?;
        if is_agg {
            let exprs = sel.group_by.iter().map(|g| binder.bind(g)).collect::<Result<Vec<_>>>()?;
            binder.group = Some(GroupCtx { exprs, aggs: Vec::new() });
        }
        let out_b = out_items.iter().map(|(_, e)| binder.bind(e)).collect::<Result<Vec<_>>>()?;
        let having = sel.having.as_ref().map(|h| binder.bind(h)).transpose()?;

        // ORDER BY: ordinal, output alias, or arbitrary expression (hidden column).
        let mut hidden: Vec<BExpr> = Vec::new();
        let mut sort_keys: Vec<(usize, bool)> = Vec::new();
        for o in &sel.order_by {
            let col = match &o.expr {
                Expr::Literal(Value::Int(n)) if *n >= 1 && (*n as usize) <= n_vis => *n as usize - 1,
                Expr::Column { table: None, name } if out_items.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) => {
                    out_items.iter().position(|(n, _)| n.eq_ignore_ascii_case(name)).expect("checked")
                }
                other => {
                    hidden.push(binder.bind(other)?);
                    n_vis + hidden.len() - 1
                }
            };
            sort_keys.push((col, o.desc));
        }
        let group = binder.group.take();

        // 4. Produce source rows.
        let limit_total = match (sel.top, sel.limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let need = limit_total.map(|l| l.saturating_add(sel.offset.unwrap_or(0)));
        let scan_limit = if !is_agg && !sel.distinct && sel.order_by.is_empty() { need } else { None };

        let mut rows: Vec<Row> = match &sel.from {
            None => {
                let mut r = vec![Vec::new()];
                if let Some(f) = &filter {
                    if !f.eval_bool(&[])? {
                        r.clear();
                    }
                }
                r
            }
            Some(from) => {
                let conj: Vec<&BExpr> = filter.as_ref().map(split_and).unwrap_or_default();
                self.produce_rows(&sources, from, &scope, filter.as_ref(), &conj, scan_limit, depth)?
            }
        };

        // 5. Grouping.
        if let Some(g) = group {
            rows = aggregate(rows, &g.exprs, &g.aggs)?;
            if let Some(h) = &having {
                let mut kept = Vec::with_capacity(rows.len());
                for r in rows {
                    if h.eval_bool(&r)? {
                        kept.push(r);
                    }
                }
                rows = kept;
            }
        }

        // 6. Projection (plus hidden sort columns).
        let mut out: Vec<Row> = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut r = Vec::with_capacity(n_vis + hidden.len());
            for e in out_b.iter().chain(&hidden) {
                r.push(e.eval(row)?.into_owned());
            }
            out.push(r);
        }
        drop(rows);

        // 7. DISTINCT, ORDER BY, OFFSET / TOP / LIMIT.
        if sel.distinct {
            let mut seen = HashSet::with_capacity(out.len());
            out.retain(|r| seen.insert(r[..n_vis].to_vec()));
        }
        if !sort_keys.is_empty() {
            let cmp = |a: &Row, b: &Row| -> Ordering {
                for &(c, desc) in &sort_keys {
                    let o = a[c].cmp(&b[c]);
                    let o = if desc { o.reverse() } else { o };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                Ordering::Equal
            };
            match need {
                Some(k) if k > 0 && k.saturating_mul(2) < out.len() => {
                    // Top-k: partition, then sort only the winners. The original
                    // position breaks ties so the result matches a stable sort.
                    let mut tagged: Vec<(usize, Row)> = out.into_iter().enumerate().collect();
                    let c = |a: &(usize, Row), b: &(usize, Row)| cmp(&a.1, &b.1).then(a.0.cmp(&b.0));
                    tagged.select_nth_unstable_by(k - 1, c);
                    tagged.truncate(k);
                    tagged.sort_by(c);
                    out = tagged.into_iter().map(|t| t.1).collect();
                }
                _ => out.sort_by(cmp),
            }
        }
        if let Some(off) = sel.offset {
            out.drain(..off.min(out.len()));
        }
        if let Some(l) = limit_total {
            out.truncate(l);
        }
        if !hidden.is_empty() {
            for r in &mut out {
                r.truncate(n_vis);
            }
        }

        Ok(QueryResult {
            columns: out_items.into_iter().map(|(n, _)| n).collect(),
            rows: out,
            affected: 0,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn produce_rows(
        &self,
        sources: &[Source<'_>],
        from: &From,
        scope: &Scope,
        filter: Option<&BExpr>,
        conj: &[&BExpr],
        limit: Option<usize>,
        depth: u32,
    ) -> Result<Vec<Row>> {
        let base = &sources[0];
        let single = from.joins.is_empty();
        let mut rows = match &base.data {
            SourceData::Table(t) => scan_table(t, conj, if single { filter } else { None }, if single { limit } else { None })?,
            SourceData::Rows(all) => {
                let mut out = Vec::new();
                for row in all {
                    if single {
                        if let Some(f) = filter {
                            if !f.eval_bool(row)? {
                                continue;
                            }
                        }
                    }
                    out.push(row.clone());
                    if single && limit.is_some_and(|l| out.len() >= l) {
                        break;
                    }
                }
                out
            }
        };
        if single {
            return Ok(rows);
        }

        let mut width = base.ncols();
        for (k, join) in from.joins.iter().enumerate() {
            let right = &sources[k + 1];
            let on = match &join.on {
                Some(on) => {
                    let prefix = scope.prefix(width + right.ncols());
                    Some(Binder::new(self, &prefix, depth).bind(on)?)
                }
                None => None,
            };
            rows = join_step(rows, width, right, join.kind, on.as_ref())?;
            width += right.ncols();
        }
        if let Some(f) = filter {
            let mut kept = Vec::with_capacity(rows.len());
            for r in rows {
                if f.eval_bool(&r)? {
                    kept.push(r);
                }
            }
            rows = kept;
        }
        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// Select list
// ---------------------------------------------------------------------------

fn expand_items(sel: &Select, scope: &Scope) -> Result<Vec<(String, Expr)>> {
    let mut out = Vec::new();
    let mut anon = 1000;
    for item in &sel.items {
        match item {
            SelectItem::Wildcard => {
                if scope.cols.is_empty() {
                    return Err(Error::runtime("SELECT * requires a FROM clause"));
                }
                for c in &scope.cols {
                    out.push((c.name.clone(), Expr::Column { table: Some(c.table.clone()), name: c.name.clone() }));
                }
            }
            SelectItem::QualifiedWildcard(t) => {
                let before = out.len();
                for c in scope.cols.iter().filter(|c| c.table.eq_ignore_ascii_case(t)) {
                    out.push((c.name.clone(), Expr::Column { table: Some(c.table.clone()), name: c.name.clone() }));
                }
                if out.len() == before {
                    return Err(Error::catalog(format!("no table named '{t}' in this query")));
                }
            }
            SelectItem::Expr { expr, alias } => {
                let name = match (alias, expr) {
                    (Some(a), _) => a.clone(),
                    (None, Expr::Column { name, .. }) => name.clone(),
                    (None, Expr::Function { name, args, .. }) if AggFn::from_name(name).is_some() => {
                        match args.first() {
                            Some(Expr::Column { name: col, .. }) => {
                                format!("{}Of{col}", AggFn::from_name(name).expect("checked").label())
                            }
                            _ => {
                                anon += 1;
                                format!("Expr{}", anon - 1)
                            }
                        }
                    }
                    _ => {
                        anon += 1;
                        format!("Expr{}", anon - 1)
                    }
                };
                out.push((name, expr.clone()));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Access path selection
// ---------------------------------------------------------------------------

pub(crate) fn split_and(e: &BExpr) -> Vec<&BExpr> {
    let mut out = Vec::new();
    let mut stack = vec![e];
    while let Some(x) = stack.pop() {
        match x {
            BExpr::Bin(crate::sql::ast::BinOp::And, a, b) => {
                stack.push(b);
                stack.push(a);
            }
            other => out.push(other),
        }
    }
    out
}

enum Access {
    Full,
    Prefix { idx: usize, key: Vec<Value> },
    Range { idx: usize, lo: Bound<Value>, hi: Bound<Value> },
}

/// Whether `v` can be compared with the column's values through its index.
/// Every numeric type compares with every other by exact value, the same way
/// the index orders them, so any number can probe any numeric column.
fn compatible(dt: DataType, v: &Value) -> bool {
    (dt.is_numeric() && v.is_numeric())
        || matches!(
            (dt, v),
            (DataType::Text, Value::Text(_)) | (DataType::DateTime, Value::DateTime(_)) | (DataType::YesNo, Value::Bool(_))
        )
}

fn flip(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Le => BinOp::Ge,
        BinOp::Gt => BinOp::Lt,
        BinOp::Ge => BinOp::Le,
        other => other,
    }
}

/// Picks the cheapest index usable for the conjuncts that compare a column of
/// `table` (positions `0..ncols`) with a literal. The caller still applies the
/// full predicate, so the chosen access path only has to be a superset.
fn choose_access(table: &Table, conj: &[&BExpr]) -> Access {
    let cols = &table.schema().columns;
    let n = cols.len();
    let mut eq: Vec<Option<&Value>> = vec![None; n];
    let mut lo: Vec<Option<(&Value, bool)>> = vec![None; n];
    let mut hi: Vec<Option<(&Value, bool)>> = vec![None; n];

    for c in conj {
        match c {
            BExpr::Bin(op, a, b) => {
                let (col, lit, op) = match (&**a, &**b) {
                    (BExpr::Col(i), BExpr::Lit(v)) if *i < n => (*i, v, *op),
                    (BExpr::Lit(v), BExpr::Col(i)) if *i < n => (*i, v, flip(*op)),
                    _ => continue,
                };
                if lit.is_null() || !compatible(cols[col].data_type, lit) {
                    continue;
                }
                match op {
                    BinOp::Eq => eq[col] = eq[col].or(Some(lit)),
                    BinOp::Gt => lo[col] = Some((lit, false)),
                    BinOp::Ge => lo[col] = Some((lit, true)),
                    BinOp::Lt => hi[col] = Some((lit, false)),
                    BinOp::Le => hi[col] = Some((lit, true)),
                    _ => {}
                }
            }
            BExpr::Between { expr, low, high, negated: false } => {
                if let (BExpr::Col(i), BExpr::Lit(l), BExpr::Lit(h)) = (&**expr, &**low, &**high) {
                    if *i < n && !l.is_null() && !h.is_null() && compatible(cols[*i].data_type, l) && compatible(cols[*i].data_type, h)
                    {
                        lo[*i] = Some((l, true));
                        hi[*i] = Some((h, true));
                    }
                }
            }
            _ => {}
        }
    }

    let mut best: Option<(u32, Access)> = None;
    for (pos, def) in table.index_defs().enumerate() {
        let p = def.columns.iter().take_while(|&&c| eq[c].is_some()).count();
        let candidate = if p == def.columns.len() && def.unique {
            Some((1000, Access::Prefix { idx: pos, key: def.columns.iter().map(|&c| eq[c].expect("eq").clone()).collect() }))
        } else if p > 0 {
            Some((
                100 + p as u32,
                Access::Prefix { idx: pos, key: def.columns[..p].iter().map(|&c| eq[c].expect("eq").clone()).collect() },
            ))
        } else {
            let c = def.columns[0];
            if lo[c].is_some() || hi[c].is_some() {
                let bound = |b: Option<(&Value, bool)>| match b {
                    Some((v, true)) => Bound::Included(v.clone()),
                    Some((v, false)) => Bound::Excluded(v.clone()),
                    None => Bound::Unbounded,
                };
                Some((10, Access::Range { idx: pos, lo: bound(lo[c]), hi: bound(hi[c]) }))
            } else {
                None
            }
        };
        if let Some((score, access)) = candidate {
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, access));
            }
        }
    }
    best.map_or(Access::Full, |(_, a)| a)
}

fn candidates<'t>(table: &'t Table, access: Access) -> Box<dyn Iterator<Item = (RowId, &'t Row)> + 't> {
    match access {
        Access::Full => Box::new(table.iter()),
        Access::Prefix { idx, key } => {
            Box::new(table.index_prefix(idx, &key).filter_map(move |id| table.get(id).map(|r| (id, r))))
        }
        Access::Range { idx, lo, hi } => {
            Box::new(table.index_range(idx, lo, hi).filter_map(move |id| table.get(id).map(|r| (id, r))))
        }
    }
}

/// Row ids of `table` satisfying `filter`, using an index when one applies.
pub(crate) fn matching_ids(table: &Table, conj: &[&BExpr], filter: Option<&BExpr>) -> Result<Vec<RowId>> {
    let mut ids = Vec::new();
    for (id, row) in candidates(table, choose_access(table, conj)) {
        if let Some(f) = filter {
            if !f.eval_bool(row)? {
                continue;
            }
        }
        ids.push(id);
    }
    Ok(ids)
}

fn scan_table(table: &Table, conj: &[&BExpr], filter: Option<&BExpr>, limit: Option<usize>) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    if limit == Some(0) {
        return Ok(out);
    }
    for (_, row) in candidates(table, choose_access(table, conj)) {
        if let Some(f) = filter {
            if !f.eval_bool(row)? {
                continue;
            }
        }
        out.push(row.clone());
        if limit.is_some_and(|l| out.len() >= l) {
            break;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Joins
// ---------------------------------------------------------------------------

enum Strategy {
    /// Probe `index` with the values of `left_cols` of each left row.
    Index { idx: usize, left_cols: Vec<usize> },
    /// Hash the right side on `right` key expressions (evaluated against a
    /// right row alone), probe with `left` key expressions.
    Hash { left: Vec<BExpr>, right: Vec<BExpr> },
    Scan,
}

fn join_step(left: Vec<Row>, lw: usize, right: &Source<'_>, kind: JoinKind, on: Option<&BExpr>) -> Result<Vec<Row>> {
    let rw = right.ncols();
    let conj = on.map(split_and).unwrap_or_default();

    // Equality conjuncts whose sides each read only one input: plain column
    // pairs (usable with indexes) and general key expressions (hashable).
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut keyed: Vec<(BExpr, BExpr)> = Vec::new();
    let shift = move |i: usize| i - lw;
    for c in &conj {
        let BExpr::Bin(BinOp::Eq, a, b) = c else { continue };
        let side = |e: &BExpr| match e.col_bounds() {
            Some((_, hi)) if hi < lw => Some(false),
            Some((lo, hi)) if lo >= lw && hi < lw + rw => Some(true),
            _ => None,
        };
        let (l, r) = match (side(a), side(b)) {
            (Some(false), Some(true)) => (&**a, &**b),
            (Some(true), Some(false)) => (&**b, &**a),
            _ => continue,
        };
        if let (BExpr::Col(x), BExpr::Col(y)) = (l, r) {
            pairs.push((*x, *y - lw));
        }
        keyed.push((l.clone(), r.map_cols(&shift)));
    }
    let only_equi = !conj.is_empty() && keyed.len() == conj.len();

    let strategy = 'pick: {
        if keyed.is_empty() {
            break 'pick Strategy::Scan;
        }
        if let SourceData::Table(t) = &right.data {
            let mut best: Option<(usize, usize, Vec<usize>)> = None;
            for (pos, def) in t.index_defs().enumerate() {
                let mut left_cols = Vec::new();
                for &c in &def.columns {
                    match pairs.iter().find(|(_, r)| *r == c) {
                        Some((l, _)) => left_cols.push(*l),
                        None => break,
                    }
                }
                if left_cols.is_empty() {
                    continue;
                }
                let score = left_cols.len() * 2 + usize::from(def.unique && left_cols.len() == def.columns.len());
                if best.as_ref().is_none_or(|(s, ..)| score > *s) {
                    best = Some((score, pos, left_cols));
                }
            }
            // Index probing wins unless the right side is far smaller than the left.
            if let Some((_, idx, left_cols)) = best {
                if left.len() <= t.len().saturating_mul(4) || t.len() > 64 {
                    break 'pick Strategy::Index { idx, left_cols };
                }
            }
        }
        Strategy::Hash {
            left: keyed.iter().map(|k| k.0.clone()).collect(),
            right: keyed.iter().map(|k| k.1.clone()).collect(),
        }
    };

    // Hash table over the right side, built once.
    let mut hash: HashMap<Vec<Value>, Vec<usize>> = HashMap::new();
    if let Strategy::Hash { right: right_keys, .. } = &strategy {
        for slot in 0..right.slot_count() {
            let Some(r) = right.get(slot) else { continue };
            let key = right_keys
                .iter()
                .map(|e| e.eval(r).map(|v| v.into_owned()))
                .collect::<Result<Vec<Value>>>()?;
            if key.iter().any(Value::is_null) {
                continue;
            }
            hash.entry(key).or_default().push(slot);
        }
    }

    // The ON predicate can be skipped only when the strategy already enforces
    // every one of its (purely equi-join) conjuncts.
    let covers_all = match &strategy {
        Strategy::Hash { .. } => true,
        Strategy::Index { left_cols, .. } => left_cols.len() == keyed.len(),
        Strategy::Scan => false,
    };
    let residual = if only_equi && covers_all { None } else { on };
    let mut right_matched = (kind == JoinKind::Right).then(|| vec![false; right.slot_count()]);
    let mut out: Vec<Row> = Vec::new();
    let mut scratch: Row = Vec::with_capacity(lw + rw);
    let mut cands: Vec<usize> = Vec::new();

    for l in &left {
        cands.clear();
        match &strategy {
            Strategy::Scan => cands.extend(0..right.slot_count()),
            Strategy::Hash { left: left_keys, .. } => {
                let key = left_keys
                    .iter()
                    .map(|e| e.eval(l).map(|v| v.into_owned()))
                    .collect::<Result<Vec<Value>>>()?;
                if !key.iter().any(Value::is_null) {
                    if let Some(slots) = hash.get(&key) {
                        cands.extend_from_slice(slots);
                    }
                }
            }
            Strategy::Index { idx, left_cols } => {
                let key: Vec<Value> = left_cols.iter().map(|&c| l[c].clone()).collect();
                if !key.iter().any(Value::is_null) {
                    if let SourceData::Table(t) = &right.data {
                        cands.extend(t.index_prefix(*idx, &key).map(|id| id as usize));
                    }
                }
            }
        }

        let mut matched = false;
        for &slot in &cands {
            let Some(r) = right.get(slot) else { continue };
            scratch.clear();
            scratch.extend_from_slice(l);
            scratch.extend_from_slice(r);
            if let Some(p) = residual {
                if !p.eval_bool(&scratch)? {
                    continue;
                }
            }
            matched = true;
            if let Some(m) = right_matched.as_mut() {
                m[slot] = true;
            }
            out.push(scratch.clone());
        }
        if !matched && kind == JoinKind::Left {
            let mut row = l.clone();
            row.resize(lw + rw, Value::Null);
            out.push(row);
        }
    }

    if let Some(m) = right_matched {
        for (slot, was) in m.into_iter().enumerate() {
            if was {
                continue;
            }
            if let Some(r) = right.get(slot) {
                let mut row = vec![Value::Null; lw];
                row.extend_from_slice(r);
                out.push(row);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

fn aggregate(rows: Vec<Row>, group_exprs: &[BExpr], aggs: &[AggSpec]) -> Result<Vec<Row>> {
    let new_states = || aggs.iter().map(AggState::new).collect::<Vec<_>>();
    let mut index: HashMap<Vec<Value>, usize> = HashMap::new();
    let mut groups: Vec<(Vec<Value>, Vec<AggState>)> = Vec::new();
    if group_exprs.is_empty() {
        // A global aggregate yields one row even over empty input.
        groups.push((Vec::new(), new_states()));
    }
    for row in &rows {
        let gi = if group_exprs.is_empty() {
            0
        } else {
            let key = group_exprs
                .iter()
                .map(|e| e.eval(row).map(|v| v.into_owned()))
                .collect::<Result<Vec<_>>>()?;
            match index.get(&key) {
                Some(&i) => i,
                None => {
                    let i = groups.len();
                    index.insert(key.clone(), i);
                    groups.push((key, new_states()));
                    i
                }
            }
        };
        for (state, spec) in groups[gi].1.iter_mut().zip(aggs) {
            match &spec.arg {
                None => state.update(None)?,
                Some(e) => {
                    let v = e.eval(row)?;
                    state.update(Some(&v))?;
                }
            }
        }
    }
    Ok(groups
        .into_iter()
        .map(|(mut key, states)| {
            key.extend(states.into_iter().map(AggState::finish));
            key
        })
        .collect())
}
