//! SELECT planning and execution: index selection, joins (index-nested-loop,
//! hash, nested-loop), grouping, DISTINCT, ORDER BY with top-k, TOP/LIMIT.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::Bound;
use std::rc::Rc;

use super::agg::{AggFn, AggSpec, AggState};
use super::bind::{contains_agg, Binder, CorrKind, CorrSpec, GroupCtx, Scope, ScopeCol};
use super::window::{self, MASK_COL, WIN_BASE};
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

pub(crate) struct Source<'a> {
    data: SourceData<'a>,
    pub(crate) alias: String,
    pub(crate) names: Vec<String>,
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
    pub(crate) fn resolve_factor(&self, f: &TableFactor, depth: u32) -> Result<Source<'_>> {
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
            TableFactor::Derived { query, alias, columns } => {
                let res = self.run_select_depth(query, depth + 1)?;
                let names = rename_columns(res.columns, columns, alias)?;
                Ok(Source { data: SourceData::Rows(res.rows), alias: alias.clone(), names })
            }
            TableFactor::Materialized { rows, columns, alias } => {
                Ok(Source { data: SourceData::Rows(rows.as_ref().clone()), alias: alias.clone(), names: columns.clone() })
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
        if sel.with.is_some() {
            return self.run_with(sel, depth);
        }
        if !sel.unions.is_empty() {
            return self.run_union(sel, depth);
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
                    scope.cols.push(ScopeCol { table: s.alias.clone(), name: n.clone(), hidden: false });
                }
            }
        }
        let join_ons = match &sel.from {
            Some(from) => join_conditions(from, &sources, &mut scope)?,
            None => Vec::new(),
        };

        // 2. Expand the select list.
        let out_items = expand_items(sel, &scope)?;
        let n_vis = out_items.len();

        // 3. Bind everything.
        let is_agg = !sel.group_by.is_empty()
            || out_items.iter().any(|(_, e)| contains_agg(e))
            || sel.having.as_ref().is_some_and(contains_agg)
            || sel.order_by.iter().any(|o| contains_agg(&o.expr));

        let mut binder = Binder::new(self, &scope, depth);
        binder.allow_corr = true;
        let filter = sel.filter.as_ref().map(|f| binder.bind(f)).transpose()?;
        if is_agg {
            let exprs = sel.group_by.iter().map(|g| binder.bind(g)).collect::<Result<Vec<_>>>()?;
            binder.group = Some(GroupCtx { exprs, aggs: Vec::new() });
        }
        binder.allow_win = true;
        let out_b = out_items.iter().map(|(_, e)| binder.bind(e)).collect::<Result<Vec<_>>>()?;
        binder.allow_win = false;
        let having = sel.having.as_ref().map(|h| binder.bind(h)).transpose()?;
        binder.allow_win = true;

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
        let corr = std::mem::take(&mut binder.corr);
        let mut wins = std::mem::take(&mut binder.wins);

        // Hidden columns follow the row: the grouping mask (grouped queries),
        // sub-query values, then window function results.
        let mask_pos = group.as_ref().map_or(0, |g| g.exprs.len() + g.aggs.len());
        let has_mask = group.is_some() && !sel.group_sets.is_empty();
        let extra_base = match &group {
            Some(_) => mask_pos + usize::from(has_mask),
            None => scope.cols.len() + corr.len(),
        };
        let remap = move |i: usize| {
            if i == MASK_COL {
                mask_pos
            } else if i >= WIN_BASE {
                extra_base + (i - WIN_BASE)
            } else {
                i
            }
        };
        let out_b: Vec<BExpr> = out_b.iter().map(|e| e.map_cols(&remap)).collect();
        let having = having.map(|h| h.map_cols(&remap));
        let hidden: Vec<BExpr> = hidden.iter().map(|e| e.map_cols(&remap)).collect();
        wins.iter_mut().for_each(|w| w.map_cols(&remap));

        // 4. Produce source rows.
        let limit_total = match (sel.top, sel.limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let need = limit_total.map(|l| l.saturating_add(sel.offset.unwrap_or(0)));
        let scan_limit = if !is_agg && !sel.distinct && sel.order_by.is_empty() && corr.is_empty() && wins.is_empty() { need } else { None };
        // Conjuncts that read a correlated sub-query's value wait until it is computed.
        let (filter, late_filter) = if corr.is_empty() { (filter, None) } else { split_late(filter, scope.cols.len()) };

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
                self.produce_rows(&sources, from, &scope, &join_ons, filter.as_ref(), &conj, scan_limit, depth)?
            }
        };
        if !corr.is_empty() {
            rows = self.add_corr_columns(rows, &corr, depth)?;
            if let Some(f) = &late_filter {
                let mut kept = Vec::with_capacity(rows.len());
                for r in rows {
                    if f.eval_bool(&r)? {
                        kept.push(r);
                    }
                }
                rows = kept;
            }
        }

        // 5. Grouping.
        if let Some(g) = group {
            rows = aggregate(rows, &g.exprs, &g.aggs, &sel.group_sets)?;
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

        if !wins.is_empty() {
            rows = window::compute(rows, &wins)?;
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

    /// Appends, to every row, the value of each correlated sub-query for that
    /// row. A sub-query runs once per distinct set of outer values it reads.
    pub(crate) fn add_corr_columns(&self, mut rows: Vec<Row>, specs: &[CorrSpec], depth: u32) -> Result<Vec<Row>> {
        const CACHE_LIMIT: usize = 4096;
        let mut caches: Vec<HashMap<Vec<Value>, Rc<Vec<Row>>>> = specs.iter().map(|_| HashMap::new()).collect();
        for row in &mut rows {
            for (spec, cache) in specs.iter().zip(&mut caches) {
                let key: Vec<Value> = spec.outer.iter().map(|(_, c)| row[*c].clone()).collect();
                let result = match cache.get(&key) {
                    Some(r) => Rc::clone(r),
                    None => {
                        let values: HashMap<String, Value> =
                            spec.outer.iter().map(|(n, _)| n.clone()).zip(key.iter().cloned()).collect();
                        let mut q = spec.query.clone();
                        crate::sql::walk::bind_params_select(&mut q, &values)?;
                        let res = self.run_select_depth(&q, depth + 1)?;
                        if !matches!(spec.kind, CorrKind::Exists { .. }) && res.columns.len() != 1 {
                            return Err(Error::runtime("a sub-query used as a value must return exactly one column"));
                        }
                        if cache.len() >= CACHE_LIMIT {
                            cache.clear();
                        }
                        let rc = Rc::new(res.rows);
                        cache.insert(key, Rc::clone(&rc));
                        rc
                    }
                };
                let v = match &spec.kind {
                    CorrKind::Scalar => {
                        if result.len() > 1 {
                            return Err(Error::runtime("scalar subquery returned more than one row"));
                        }
                        result.first().map_or(Value::Null, |r| r[0].clone())
                    }
                    CorrKind::Exists { negated } => Value::Bool(result.is_empty() == *negated),
                    CorrKind::In { expr, negated } => {
                        let lhs = expr.eval(row)?;
                        if lhs.is_null() {
                            Value::Null
                        } else {
                            let found = result.iter().any(|r| lhs.sql_cmp(&r[0]) == Some(Ordering::Equal));
                            let t = if found {
                                Some(true)
                            } else if result.iter().any(|r| r[0].is_null()) {
                                None
                            } else {
                                Some(false)
                            };
                            t.map_or(Value::Null, |b| Value::Bool(b != *negated))
                        }
                    }
                };
                row.push(v);
            }
        }
        Ok(rows)
    }

    /// `SELECT ... UNION [ALL] SELECT ...`: the members run on their own, their
    /// rows are concatenated (removing duplicates at each plain `UNION`), and the
    /// trailing ORDER BY / LIMIT / OFFSET apply to the whole.
    fn run_union(&self, sel: &Select, depth: u32) -> Result<QueryResult> {
        let mut head = sel.clone();
        head.unions.clear();
        head.order_by.clear();
        head.limit = None;
        head.offset = None;
        let first = self.run_select_depth(&head, depth)?;
        let columns = first.columns;
        // INTERSECT binds tighter than UNION and EXCEPT, which go left to right.
        let mut terms: Vec<(SetOp, bool, Vec<Row>)> = vec![(SetOp::Union, true, first.rows)];
        for part in &sel.unions {
            let res = self.run_select_depth(&part.select, depth + 1)?;
            if res.columns.len() != columns.len() {
                return Err(Error::runtime(format!(
                    "every SELECT in a set operation must return the same number of columns ({} and {} differ)",
                    columns.len(),
                    res.columns.len()
                )));
            }
            if part.op == SetOp::Intersect {
                let last = terms.last_mut().expect("never empty");
                last.2 = combine(SetOp::Intersect, part.all, std::mem::take(&mut last.2), res.rows);
            } else {
                terms.push((part.op, part.all, res.rows));
            }
        }
        let mut terms = terms.into_iter();
        let mut rows = terms.next().expect("never empty").2;
        for (op, all, next) in terms {
            rows = combine(op, all, rows, next);
        }

        let mut keys: Vec<(usize, bool)> = Vec::new();
        for o in &sel.order_by {
            let col = match &o.expr {
                Expr::Literal(Value::Int(n)) if *n >= 1 && (*n as usize) <= columns.len() => *n as usize - 1,
                Expr::Column { name, .. } => columns.iter().position(|c| c.eq_ignore_ascii_case(name)).ok_or_else(|| {
                    Error::catalog(format!("ORDER BY '{name}' is not a column of the UNION result"))
                })?,
                _ => {
                    return Err(Error::runtime(
                        "ORDER BY after a UNION must name a result column or give its position",
                    ))
                }
            };
            keys.push((col, o.desc));
        }
        if !keys.is_empty() {
            rows.sort_by(|a, b| {
                for &(c, desc) in &keys {
                    let o = a[c].cmp(&b[c]);
                    let o = if desc { o.reverse() } else { o };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                Ordering::Equal
            });
        }
        if let Some(off) = sel.offset {
            rows.drain(..off.min(rows.len()));
        }
        if let Some(l) = sel.limit {
            rows.truncate(l);
        }
        Ok(QueryResult { columns, rows, affected: 0 })
    }

    #[allow(clippy::too_many_arguments)]
    fn produce_rows(
        &self,
        sources: &[Source<'_>],
        from: &From,
        scope: &Scope,
        ons: &[Option<Expr>],
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
            let on = match &ons[k] {
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
                for c in scope.cols.iter().filter(|c| !c.hidden) {
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

/// Splits a filter into the part that can run on source rows (reading only the
/// first `width` columns) and the rest, which needs computed hidden columns.
pub(crate) fn split_late(filter: Option<BExpr>, width: usize) -> (Option<BExpr>, Option<BExpr>) {
    let Some(f) = filter else { return (None, None) };
    let (mut early, mut late): (Vec<BExpr>, Vec<BExpr>) = (Vec::new(), Vec::new());
    for c in split_and(&f) {
        match c.col_bounds() {
            Some((_, hi)) if hi >= width => late.push(c.clone()),
            _ => early.push(c.clone()),
        }
    }
    let join = |v: Vec<BExpr>| v.into_iter().reduce(|a, b| BExpr::Bin(BinOp::And, Box::new(a), Box::new(b)));
    (join(early), join(late))
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
    let mut right_matched = matches!(kind, JoinKind::Right | JoinKind::Full).then(|| vec![false; right.slot_count()]);
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
        if !matched && matches!(kind, JoinKind::Left | JoinKind::Full) {
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

fn aggregate(rows: Vec<Row>, group_exprs: &[BExpr], aggs: &[AggSpec], sets: &[Vec<usize>]) -> Result<Vec<Row>> {
    if sets.is_empty() {
        return aggregate_set(&rows, group_exprs, aggs, None, None);
    }
    if group_exprs.len() > 62 {
        return Err(Error::runtime("too many GROUP BY expressions for grouping sets"));
    }
    let mut out = Vec::new();
    for set in sets {
        // Bit i of the mask is set when group expression i is rolled up in this set.
        let mask = (0..group_exprs.len()).filter(|i| !set.contains(i)).fold(0i64, |m, i| m | (1 << i));
        out.extend(aggregate_set(&rows, group_exprs, aggs, Some(set), Some(mask))?);
    }
    Ok(out)
}

/// One grouping. With `active`, only those group expressions form the key; the
/// others are NULL in the output, and `mask` is appended to every row.
fn aggregate_set(
    rows: &[Row],
    group_exprs: &[BExpr],
    aggs: &[AggSpec],
    active: Option<&[usize]>,
    mask: Option<i64>,
) -> Result<Vec<Row>> {
    let new_states = || aggs.iter().map(AggState::new).collect::<Vec<_>>();
    let mut index: HashMap<Vec<Value>, usize> = HashMap::new();
    let mut groups: Vec<(Vec<Value>, Vec<AggState>)> = Vec::new();
    let global = active.map_or(group_exprs.is_empty(), |s| s.is_empty());
    let null_key = || vec![Value::Null; group_exprs.len()];
    if global {
        // A global aggregate yields one row even over empty input.
        groups.push((null_key(), new_states()));
    }
    for row in rows {
        let gi = if global {
            0
        } else {
            let mut key = null_key();
            for (i, e) in group_exprs.iter().enumerate() {
                if active.is_none_or(|s| s.contains(&i)) {
                    key[i] = e.eval(row)?.into_owned();
                }
            }
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
            if let Some(m) = mask {
                key.push(Value::Int(m));
            }
            key
        })
        .collect())
}

/// `a UNION|INTERSECT|EXCEPT [ALL] b` over already computed rows.
fn combine(op: SetOp, all: bool, a: Vec<Row>, b: Vec<Row>) -> Vec<Row> {
    let dedupe = |rows: Vec<Row>| {
        let mut seen = HashSet::with_capacity(rows.len());
        rows.into_iter().filter(|r| seen.insert(r.clone())).collect::<Vec<_>>()
    };
    match op {
        SetOp::Union => {
            let mut rows = a;
            rows.extend(b);
            if all { rows } else { dedupe(rows) }
        }
        SetOp::Intersect | SetOp::Except => {
            let keep_matches = op == SetOp::Intersect;
            if all {
                let mut counts: HashMap<Row, usize> = HashMap::new();
                for r in b {
                    *counts.entry(r).or_default() += 1;
                }
                a.into_iter()
                    .filter(|r| match counts.get_mut(r) {
                        Some(n) if *n > 0 => {
                            *n -= 1;
                            keep_matches
                        }
                        _ => !keep_matches,
                    })
                    .collect()
            } else {
                let set: HashSet<Row> = b.into_iter().collect();
                dedupe(a.into_iter().filter(|r| set.contains(r) == keep_matches).collect())
            }
        }
    }
}

/// Applies a column alias list (`AS t(a, b)`) to a source's column names.
fn rename_columns(actual: Vec<String>, wanted: &[String], what: &str) -> Result<Vec<String>> {
    if wanted.is_empty() {
        return Ok(actual);
    }
    if wanted.len() != actual.len() {
        return Err(Error::runtime(format!(
            "'{what}' names {} columns but its query returns {}",
            wanted.len(),
            actual.len()
        )));
    }
    Ok(wanted.to_vec())
}

/// The ON condition of each join. `USING` and `NATURAL` become equalities of
/// the like-named columns, and hide the duplicate so the column shows once.
fn join_conditions(from: &From, sources: &[Source<'_>], scope: &mut Scope) -> Result<Vec<Option<Expr>>> {
    let mut out = Vec::with_capacity(from.joins.len());
    let mut left_width = sources[0].ncols();
    for (k, join) in from.joins.iter().enumerate() {
        let right = &sources[k + 1];
        if join.using.is_empty() && !join.natural {
            out.push(join.on.clone());
            left_width += right.ncols();
            continue;
        }
        if join.kind == JoinKind::Full {
            return Err(Error::runtime("FULL JOIN with USING or NATURAL is not supported; write the ON condition"));
        }
        let names: Vec<String> = if join.natural {
            let mut common: Vec<String> = Vec::new();
            for n in &right.names {
                let in_left = scope.cols[..left_width].iter().any(|c| !c.hidden && c.name.eq_ignore_ascii_case(n));
                if in_left && !common.iter().any(|c| c.eq_ignore_ascii_case(n)) {
                    common.push(n.clone());
                }
            }
            common
        } else {
            join.using.clone()
        };
        let mut cond: Option<Expr> = None;
        for name in &names {
            let lefts: Vec<usize> = (0..left_width)
                .filter(|&i| !scope.cols[i].hidden && scope.cols[i].name.eq_ignore_ascii_case(name))
                .collect();
            let rights: Vec<usize> =
                (0..right.ncols()).filter(|&i| right.names[i].eq_ignore_ascii_case(name)).collect();
            let (li, ri) = match (lefts.as_slice(), rights.as_slice()) {
                ([l], [r]) => (*l, *r),
                ([], _) => return Err(Error::catalog(format!("the column '{name}' of USING is not on the left side of the join"))),
                (_, []) => return Err(Error::catalog(format!("the column '{name}' of USING is not on the right side of the join"))),
                _ => return Err(Error::catalog(format!("the column '{name}' of USING is ambiguous"))),
            };
            let eq = Expr::Binary {
                op: BinOp::Eq,
                left: Box::new(Expr::Column { table: Some(scope.cols[li].table.clone()), name: scope.cols[li].name.clone() }),
                right: Box::new(Expr::Column { table: Some(right.alias.clone()), name: right.names[ri].clone() }),
            };
            cond = Some(match cond {
                None => eq,
                Some(c) => Expr::Binary { op: BinOp::And, left: Box::new(c), right: Box::new(eq) },
            });
            if join.kind == JoinKind::Right {
                scope.cols[li].hidden = true;
            } else {
                scope.cols[left_width + ri].hidden = true;
            }
        }
        out.push(cond);
        left_width += right.ncols();
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Common table expressions
// ---------------------------------------------------------------------------

/// Replaces references to the CTE `name` in `s` (sub-queries included) with its rows.
fn subst_cte(s: &mut Select, name: &str, rows: &Rc<Vec<Row>>, cols: &[String]) {
    fn factor(f: &mut TableFactor, name: &str, rows: &Rc<Vec<Row>>, cols: &[String]) {
        match f {
            TableFactor::Named { name: n, alias } if n.eq_ignore_ascii_case(name) => {
                let alias = alias.clone().unwrap_or_else(|| n.clone());
                *f = TableFactor::Materialized { rows: Rc::clone(rows), columns: cols.to_vec(), alias };
            }
            TableFactor::Derived { query, .. } => subst_cte(query, name, rows, cols),
            _ => {}
        }
    }
    if let Some(w) = &mut s.with {
        for c in &mut w.ctes {
            subst_cte(&mut c.query, name, rows, cols);
        }
    }
    if let Some(from) = &mut s.from {
        factor(&mut from.first, name, rows, cols);
        for j in &mut from.joins {
            factor(&mut j.factor, name, rows, cols);
        }
    }
    for u in &mut s.unions {
        subst_cte(&mut u.select, name, rows, cols);
    }
    let _ = crate::sql::walk::walk_select(s, &mut |e| {
        if let Expr::Subquery(q) | Expr::Exists { query: q, .. } | Expr::InSelect { query: q, .. } = e {
            subst_cte(q, name, rows, cols);
        }
        Ok(())
    });
}

fn mentions_table(s: &Select, name: &str) -> bool {
    let mut copy = s.clone();
    subst_cte(&mut copy, name, &Rc::new(Vec::new()), &[]);
    copy != *s
}

impl Database {
    /// `WITH name AS (...) query`: each CTE runs once, in order, and its rows
    /// stand in for the name wherever the rest of the statement uses it.
    fn run_with(&self, sel: &Select, depth: u32) -> Result<QueryResult> {
        let with = sel.with.as_ref().expect("checked by the caller");
        let mut body = sel.clone();
        body.with = None;
        let mut done: Vec<(String, Rc<Vec<Row>>, Vec<String>)> = Vec::new();
        for cte in &with.ctes {
            let mut q = cte.query.clone();
            for (n, rows, cols) in &done {
                subst_cte(&mut q, n, rows, cols);
            }
            let (cols, rows) = if with.recursive && mentions_table(&q, &cte.name) {
                self.run_recursive(cte, &q, depth)?
            } else {
                let res = self.run_select_depth(&q, depth + 1)?;
                (rename_columns(res.columns, &cte.columns, &cte.name)?, res.rows)
            };
            done.push((cte.name.clone(), Rc::new(rows), cols));
        }
        for (n, rows, cols) in &done {
            subst_cte(&mut body, n, rows, cols);
        }
        self.run_select_depth(&body, depth)
    }

    /// A recursive CTE: the non-recursive parts start it, then the recursive
    /// parts run again and again on the rows the previous round produced,
    /// until a round produces nothing.
    fn run_recursive(&self, cte: &Cte, q: &Select, depth: u32) -> Result<(Vec<String>, Vec<Row>)> {
        const MAX_ROUNDS: usize = 100_000;
        const MAX_ROWS: usize = 10_000_000;
        let name = &cte.name;
        if !q.order_by.is_empty() || q.limit.is_some() || q.offset.is_some() {
            return Err(Error::runtime(format!("the recursive query '{name}' cannot have ORDER BY or LIMIT")));
        }
        let mut head = q.clone();
        head.unions.clear();
        let mut members = vec![head];
        for u in &q.unions {
            if u.op != SetOp::Union {
                return Err(Error::runtime(format!("the recursive query '{name}' may only combine its parts with UNION")));
            }
            members.push(u.select.clone());
        }
        let distinct = q.unions.iter().any(|u| !u.all);
        let (recursive, anchors): (Vec<Select>, Vec<Select>) = members.into_iter().partition(|m| mentions_table(m, name));
        if anchors.is_empty() {
            return Err(Error::runtime(format!("the recursive query '{name}' has no starting part that avoids referring to itself")));
        }
        let mut names: Option<Vec<String>> = None;
        let mut rows: Vec<Row> = Vec::new();
        for m in &anchors {
            let res = self.run_select_depth(m, depth + 1)?;
            if names.as_ref().is_some_and(|n| n.len() != res.columns.len()) {
                return Err(Error::runtime(format!("the parts of the recursive query '{name}' have different numbers of columns")));
            }
            names.get_or_insert(res.columns);
            rows.extend(res.rows);
        }
        let names = rename_columns(names.expect("at least one anchor"), &cte.columns, name)?;
        let mut seen: HashSet<Row> = HashSet::new();
        if distinct {
            rows.retain(|r| seen.insert(r.clone()));
        }
        let mut all = rows.clone();
        let mut working = rows;
        let mut rounds = 0;
        while !working.is_empty() {
            rounds += 1;
            if rounds > MAX_ROUNDS {
                return Err(Error::runtime(format!("the recursive query '{name}' did not stop after {MAX_ROUNDS} rounds")));
            }
            let current = Rc::new(std::mem::take(&mut working));
            let mut next: Vec<Row> = Vec::new();
            for m in &recursive {
                let mut m = m.clone();
                subst_cte(&mut m, name, &current, &names);
                let res = self.run_select_depth(&m, depth + 1)?;
                if res.columns.len() != names.len() {
                    return Err(Error::runtime(format!("the parts of the recursive query '{name}' have different numbers of columns")));
                }
                next.extend(res.rows);
            }
            if distinct {
                next.retain(|r| seen.insert(r.clone()));
            }
            if all.len() + next.len() > MAX_ROWS {
                return Err(Error::runtime(format!("the recursive query '{name}' produced too many rows")));
            }
            all.extend(next.iter().cloned());
            working = next;
        }
        Ok((names, all))
    }
}
