//! SELECT planning and execution: index selection, joins (index-nested-loop,
//! hash, nested-loop), grouping, DISTINCT, ORDER BY with top-k, TOP/LIMIT.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::Bound;
use std::sync::Arc;

use super::agg::{AggFn, AggSpec, AggState};
use super::bind::{contains_agg, Binder, CorrKind, CorrSpec, GroupCtx, Scope, ScopeCol};
use super::optimizer::{choose_order, estimate, nullable, source_mask, Pred, SourceMask, MAX_SOURCES, bit};
use super::window::{self, GCORR_BASE, MASK_COL, WIN_BASE};
use super::QueryResult;
use crate::database::Database;
use crate::error::{Error, Result};
use crate::expr::BExpr;
use crate::schema::IndexDef;
use crate::sql::ast::*;
use crate::storage::{RowId, Table};
use crate::value::{DataType, Row, Value};

const MAX_DEPTH: u32 = 16;

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

pub(crate) enum SourceData<'a> {
    Table(&'a Table),
    Rows(Vec<Row>),
    /// A LATERAL sub-query or table function that reads earlier tables: rerun for each of their rows.
    Lateral(Box<Lateral>),
}

pub(crate) struct Lateral {
    kind: LateralKind,
    /// Parameter names, with the position (in the rows joined so far) of the column each stands for.
    outer: Vec<(String, usize)>,
}

enum LateralKind {
    Query(Select),
    Func(String, Vec<Expr>),
}

pub(crate) struct Source<'a> {
    pub(crate) data: SourceData<'a>,
    /// Conditions that belong to the source itself (a merged view's `WHERE`), written for its columns.
    pub(crate) intrinsic: Vec<BExpr>,
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
            SourceData::Lateral(_) => 0,
        }
    }
    fn get(&self, slot: usize) -> Option<&Row> {
        match &self.data {
            SourceData::Table(t) => t.get(slot as RowId),
            SourceData::Rows(r) => r.get(slot),
            SourceData::Lateral(_) => None,
        }
    }
}

impl Database {
    pub(crate) fn resolve_factor(&self, f: &TableFactor, depth: u32) -> Result<Source<'_>> {
        match f {
            TableFactor::Named { name, alias } => {
                let alias = alias.clone().unwrap_or_else(|| name.clone());
                if let Some(view) = name.strip_prefix("information_schema.") {
                    let (names, rows) = self.information_schema(view)?;
                    return Ok(Source { intrinsic: Vec::new(), data: SourceData::Rows(rows), alias, names });
                }
                if let Some(t) = self.table(name) {
                    let names = t.schema().columns.iter().map(|c| c.name.clone()).collect();
                    return Ok(Source { intrinsic: Vec::new(), data: SourceData::Table(t), alias, names });
                }
                if self.view_sql(name).is_some() {
                    let (params, q) = self.resolve_query(name, 0)?;
                    if !params.is_empty() {
                        return Err(Error::runtime(format!(
                            "the query '{name}' needs parameters; run it with EXEC [{name}] ..."
                        )));
                    }
                    let res = self.run_select_depth(&q, depth + 1)?;
                    return Ok(Source { intrinsic: Vec::new(), data: SourceData::Rows(res.rows), alias, names: res.columns });
                }
                Err(Error::catalog(format!("the table or query '{name}' does not exist")))
            }
            TableFactor::Pivot { source, agg, pivot, values, alias } => {
                let inner = self.resolve_factor(source, depth)?;
                // The columns the aggregate and the pivot use are consumed; the rest head the rows.
                let mut used: Vec<String> = Vec::new();
                for e in [agg, pivot] {
                    let mut copy = e.clone();
                    crate::sql::walk::walk_expr(&mut copy, &mut |x| {
                        if let Expr::Column { name, .. } = x {
                            used.push(name.to_ascii_lowercase());
                        }
                        Ok(())
                    })?;
                }
                let heads: Vec<String> = inner.names.iter().filter(|n| !used.contains(&n.to_ascii_lowercase())).cloned().collect();
                let from_factor = match inner.data {
                    SourceData::Rows(rows) => {
                        TableFactor::Materialized { rows: Arc::new(rows), columns: inner.names.clone(), alias: inner.alias.clone() }
                    }
                    _ => (**source).clone(),
                };
                let head_exprs: Vec<Expr> = heads.iter().map(|n| Expr::Column { table: None, name: n.clone() }).collect();
                let query = Select {
                    items: head_exprs.iter().map(|e| SelectItem::Expr { expr: e.clone(), alias: None }).collect(),
                    from: Some(From { first: from_factor, joins: Vec::new() }),
                    group_by: head_exprs,
                    crosstab: Some(Box::new(Crosstab {
                        agg: agg.clone(),
                        pivot: pivot.clone(),
                        values: Some(values.iter().map(|(v, _)| v.clone()).collect()),
                    })),
                    ..Select::default()
                };
                let mut res = self.run_select_depth(&query, depth + 1)?;
                for (i, (_, label)) in values.iter().enumerate() {
                    if let Some(l) = label {
                        res.columns[heads.len() + i] = l.clone();
                    }
                }
                Ok(Source { intrinsic: Vec::new(), data: SourceData::Rows(res.rows), alias: alias.clone(), names: res.columns })
            }
            TableFactor::Unpivot { source, value_col, name_col, columns, include_nulls, alias } => {
                let inner = self.resolve_factor(source, depth)?;
                let pos: Vec<usize> = columns
                    .iter()
                    .map(|(c, _)| {
                        inner
                            .names
                            .iter()
                            .position(|n| n.eq_ignore_ascii_case(c))
                            .ok_or_else(|| Error::catalog(format!("the column '{c}' of UNPIVOT was not found")))
                    })
                    .collect::<Result<_>>()?;
                let keep: Vec<usize> = (0..inner.names.len()).filter(|i| !pos.contains(i)).collect();
                let mut names: Vec<String> = keep.iter().map(|&i| inner.names[i].clone()).collect();
                names.push(name_col.clone());
                names.push(value_col.clone());
                let mut out: Vec<Row> = Vec::new();
                for slot in 0..inner.slot_count() {
                    let Some(row) = inner.get(slot) else { continue };
                    for (k, (col, label)) in columns.iter().enumerate() {
                        let v = &row[pos[k]];
                        if v.is_null() && !include_nulls {
                            continue;
                        }
                        let mut r: Row = keep.iter().map(|&i| row[i].clone()).collect();
                        r.push(Value::text(label.as_deref().unwrap_or(col)));
                        r.push(v.clone());
                        out.push(r);
                    }
                }
                Ok(Source { intrinsic: Vec::new(), data: SourceData::Rows(out), alias: alias.clone(), names })
            }
            TableFactor::Function { name, args, alias, columns } => {
                let scope = Scope::default();
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(Binder::new(self, &scope, depth).bind(a)?.eval(&[])?.into_owned());
                }
                let (cols, rows) = self.table_function(name, &vals)?;
                let names = rename_columns(cols, columns, alias)?;
                Ok(Source { intrinsic: Vec::new(), data: SourceData::Rows(rows), alias: alias.clone(), names })
            }
            TableFactor::Derived { query, alias, columns, .. } => {
                let res = self.run_select_depth(query, depth + 1)?;
                let names = rename_columns(res.columns, columns, alias)?;
                Ok(Source { intrinsic: Vec::new(), data: SourceData::Rows(res.rows), alias: alias.clone(), names })
            }
            TableFactor::Materialized { rows, columns, alias } => {
                Ok(Source { intrinsic: Vec::new(), data: SourceData::Rows(rows.as_ref().clone()), alias: alias.clone(), names: columns.clone() })
            }
        }
    }

    /// A FROM item resolved with the columns of the items before it in view, which a
    /// LATERAL sub-query or a table function may read.
    fn resolve_source(&self, f: &TableFactor, scope: &Scope, depth: u32, allow_merge: bool) -> Result<Source<'_>> {
        match f {
            TableFactor::Named { name, alias } if allow_merge && self.table(name).is_none() && self.view_sql(name).is_some() => {
                match self.merged_view(name, alias.as_deref().unwrap_or(name))? {
                    Some(src) => Ok(src),
                    None => self.resolve_factor(f, depth),
                }
            }
            TableFactor::Derived { query, alias, columns, lateral: false } if allow_merge => match self.merged_derived(query, alias, columns)? {
                Some(src) => Ok(src),
                None => self.resolve_factor(f, depth),
            },
            TableFactor::Derived { query, alias, columns, lateral: true } => {
                let mut q = (**query).clone();
                let outer = super::bind::correlate_select(self, scope, &mut q, depth)?;
                if outer.is_empty() {
                    return self.resolve_factor(f, depth);
                }
                // Its columns are learned by running it once with NULL for every outer value.
                let mut probe = q.clone();
                let nulls: HashMap<String, Value> = outer.iter().map(|(n, _)| (n.clone(), Value::Null)).collect();
                crate::sql::walk::bind_params_select(&mut probe, &nulls)?;
                probe.limit = Some(0);
                let probed = self.unlogged(|| self.run_select_depth(&probe, depth + 1))?;
                let names = rename_columns(probed.columns, columns, alias)?;
                Ok(Source { intrinsic: Vec::new(), data: SourceData::Lateral(Box::new(Lateral { kind: LateralKind::Query(q), outer })), alias: alias.clone(), names })
            }
            TableFactor::Function { name, args, alias, columns } => {
                let mut a = args.clone();
                let outer = super::bind::correlate_exprs(self, scope, &mut a, depth)?;
                if outer.is_empty() {
                    return self.resolve_factor(f, depth);
                }
                let cols = self.table_function_columns(name)?;
                let names = rename_columns(cols, columns, alias)?;
                Ok(Source {
                    intrinsic: Vec::new(),
                    data: SourceData::Lateral(Box::new(Lateral { kind: LateralKind::Func(name.clone(), a), outer })),
                    alias: alias.clone(),
                    names,
                })
            }
            _ => self.resolve_factor(f, depth),
        }
    }

    /// The column names a table function produces.
    pub(crate) fn table_function_columns(&self, name: &str) -> Result<Vec<String>> {
        Ok(match name.to_ascii_uppercase().as_str() {
            "GENERATE_SERIES" => vec!["generate_series".into()],
            "SPLIT_STRING" => vec!["value".into(), "ordinal".into()],
            "JSON_EACH" => vec!["key".into(), "value".into()],
            other => return Err(Error::catalog(format!("'{other}' is not a table function (GENERATE_SERIES, SPLIT_STRING and JSON_EACH are available)"))),
        })
    }

    /// Runs a table function on already evaluated arguments.
    pub(crate) fn table_function(&self, name: &str, args: &[Value]) -> Result<(Vec<String>, Vec<Row>)> {
        const MAX_ROWS: usize = 10_000_000;
        let cols = self.table_function_columns(name)?;
        let wrong = |n: &str| Error::runtime(format!("wrong number of arguments for {n}"));
        match name.to_ascii_uppercase().as_str() {
            "GENERATE_SERIES" => {
                if !(2..=3).contains(&args.len()) {
                    return Err(wrong("GENERATE_SERIES"));
                }
                if args.iter().any(Value::is_null) {
                    return Ok((cols, Vec::new()));
                }
                let step = args.get(2).cloned().unwrap_or(Value::Int(1));
                let forward = match step.sign() {
                    Some(Ordering::Greater) => true,
                    Some(Ordering::Less) => false,
                    _ => return Err(Error::runtime("GENERATE_SERIES: the step must not be zero")),
                };
                let mut rows = Vec::new();
                let mut cur = args[0].clone();
                loop {
                    match cur.sql_cmp(&args[1]) {
                        Some(o) if (forward && o != Ordering::Greater) || (!forward && o != Ordering::Less) => {}
                        _ => break,
                    }
                    if rows.len() >= MAX_ROWS {
                        return Err(Error::runtime("GENERATE_SERIES would produce too many rows"));
                    }
                    rows.push(vec![cur.clone()]);
                    cur = crate::numeric::arith(BinOp::Add, &cur, &step)?;
                }
                Ok((cols, rows))
            }
            "SPLIT_STRING" => {
                if args.len() != 2 {
                    return Err(wrong("SPLIT_STRING"));
                }
                if args.iter().any(Value::is_null) {
                    return Ok((cols, Vec::new()));
                }
                let (text, sep) = (args[0].to_string(), args[1].to_string());
                let parts: Vec<String> = if sep.is_empty() {
                    text.chars().map(String::from).collect()
                } else {
                    text.split(&sep).map(str::to_string).collect()
                };
                let rows = parts.into_iter().enumerate().map(|(i, p)| vec![Value::text(&p), Value::Int(i as i64 + 1)]).collect();
                Ok((cols, rows))
            }
            "JSON_EACH" => {
                if args.len() != 1 {
                    return Err(wrong("JSON_EACH"));
                }
                if args[0].is_null() {
                    return Ok((cols, Vec::new()));
                }
                let rows = match crate::json::document(&args[0])? {
                    serde_json::Value::Object(m) => m.iter().map(|(k, v)| vec![Value::text(k), crate::json::to_value(v)]).collect(),
                    serde_json::Value::Array(a) => a.iter().enumerate().map(|(i, v)| vec![Value::Int(i as i64), crate::json::to_value(v)]).collect(),
                    _ => return Err(Error::runtime("JSON_EACH needs an object or an array")),
                };
                Ok((cols, rows))
            }
            _ => unreachable!("checked by table_function_columns"),
        }
    }

    /// Joins `left` to a LATERAL item, running the item once for each distinct set of outer values.
    #[allow(clippy::too_many_arguments)]
    fn lateral_step(
        &self,
        left: Vec<Row>,
        right: &Source<'_>,
        lat: &Lateral,
        kind: JoinKind,
        on: Option<&BExpr>,
        right_pre: Option<&BExpr>,
        depth: u32,
    ) -> Result<(Vec<Row>, String)> {
        if matches!(kind, JoinKind::Right | JoinKind::Full) {
            return Err(Error::runtime("a LATERAL item cannot be the target of a RIGHT or FULL JOIN"));
        }
        let rw = right.ncols();
        let mut cache: HashMap<Vec<Value>, Arc<Vec<Row>>> = HashMap::new();
        let mut out: Vec<Row> = Vec::new();
        let mut runs = 0usize;
        for l in &left {
            let key: Vec<Value> = lat.outer.iter().map(|(_, c)| l[*c].clone()).collect();
            let produced = match cache.get(&key) {
                Some(r) => Arc::clone(r),
                None => {
                    let values: HashMap<String, Value> = lat.outer.iter().map(|(n, _)| n.clone()).zip(key.iter().cloned()).collect();
                    runs += 1;
                    let rows = match &lat.kind {
                        LateralKind::Query(q) => {
                            let mut q = q.clone();
                            crate::sql::walk::bind_params_select(&mut q, &values)?;
                            self.run_select_depth(&q, depth + 1)?.rows
                        }
                        LateralKind::Func(name, args) => {
                            let mut a = args.clone();
                            crate::sql::walk::bind_params_exprs(&mut a, &values)?;
                            let scope = Scope::default();
                            let mut vals = Vec::with_capacity(a.len());
                            for e in &a {
                                vals.push(Binder::new(self, &scope, depth).bind(e)?.eval(&[])?.into_owned());
                            }
                            self.table_function(name, &vals)?.1
                        }
                    };
                    let rc = Arc::new(rows);
                    cache.insert(key, Arc::clone(&rc));
                    rc
                }
            };
            let mut matched = false;
            for r in produced.iter() {
                if let Some(p) = right_pre {
                    if !p.eval_bool(r)? {
                        continue;
                    }
                }
                let mut row = Vec::with_capacity(l.len() + rw);
                row.extend_from_slice(l);
                row.extend_from_slice(r);
                if let Some(c) = on {
                    if !c.eval_bool(&row)? {
                        continue;
                    }
                }
                matched = true;
                out.push(row);
            }
            if !matched && kind == JoinKind::Left {
                let mut row = l.clone();
                row.resize(l.len() + rw, Value::Null);
                out.push(row);
            }
        }
        let how = format!("LATERAL, run {runs} time(s) for {} left row(s) (answers remembered per distinct outer value)", left.len());
        Ok((out, how))
    }

    pub fn run_select(&self, sel: &Select) -> Result<QueryResult> {
        self.enter_session();
        self.run_select_depth(sel, 0)
    }

    pub(crate) fn run_select_depth(&self, sel: &Select, depth: u32) -> Result<QueryResult> {
        if depth > MAX_DEPTH {
            return Err(Error::runtime("queries are nested too deeply (is a saved query referring to itself?)"));
        }
        if sel.crosstab.is_some() {
            return self.run_crosstab(sel, depth);
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
            let few = from.joins.len() < MAX_SOURCES;
            for (k, f) in std::iter::once(&from.first).chain(from.joins.iter().map(|j| &j.factor)).enumerate() {
                // A view read as its table cannot be the preserved side of a RIGHT or FULL join.
                let preserved = k > 0 && matches!(from.joins[k - 1].kind, JoinKind::Right | JoinKind::Full);
                let src = self.resolve_source(f, &scope, depth, few && !preserved)?;
                for n in &src.names {
                    scope.cols.push(ScopeCol { table: src.alias.clone(), name: n.clone(), hidden: n.starts_with(super::viewdml::HIDDEN_COLUMN) });
                }
                sources.push(src);
            }
        }
        let join_ons = match &sel.from {
            Some(from) => join_conditions(from, &sources, &mut scope)?,
            None => Vec::new(),
        };

        // 2. Expand the select list.
        let out_items = expand_items(sel, &scope)?;
        let n_vis = out_items.len();
        self.note(depth, || {
            format!("query over {}", if sources.is_empty() { "no tables".to_string() } else { sources.iter().map(|s| s.alias.as_str()).collect::<Vec<_>>().join(", ") })
        });

        // SELECT COUNT(*) FROM t needs no scan: the table knows its size.
        if let Some(res) = self.count_shortcut(sel, &sources, &out_items) {
            self.note(depth, || "COUNT(*) answered from the table's row count".to_string());
            return Ok(res);
        }

        // 3. Bind everything.
        let is_agg = !sel.group_by.is_empty()
            || out_items.iter().any(|(_, e)| contains_agg(e))
            || sel.having.as_ref().is_some_and(contains_agg)
            || sel.order_by.iter().any(|o| contains_agg(&o.expr));

        let mut binder = Binder::new(self, &scope, depth);
        binder.allow_corr = true;
        binder.windows = sel.windows.clone();
        let mut filter = sel.filter.as_ref().map(|f| binder.bind(f)).transpose()?;
        // A merged view's own condition joins the query's when it reads a single source.
        if sources.len() == 1 && !sources[0].intrinsic.is_empty() {
            for extra in std::mem::take(&mut sources[0].intrinsic) {
                filter = Some(match filter {
                    Some(f) => BExpr::Bin(BinOp::And, Box::new(f), Box::new(extra)),
                    None => extra,
                });
            }
        }
        if is_agg {
            let exprs = sel.group_by.iter().map(|g| binder.bind(g)).collect::<Result<Vec<_>>>()?;
            binder.group = Some(GroupCtx { exprs, asts: sel.group_by.clone(), aggs: Vec::new() });
        }
        binder.allow_win = true;
        let out_b = out_items.iter().map(|(_, e)| binder.bind(e)).collect::<Result<Vec<_>>>()?;
        binder.allow_win = false;
        let having = sel.having.as_ref().map(|h| binder.bind(h)).transpose()?;
        binder.allow_win = true;

        // ORDER BY: ordinal, output alias, or arbitrary expression (hidden column).
        let mut hidden: Vec<BExpr> = Vec::new();
        let mut sort_keys: Vec<(usize, bool, Option<bool>)> = Vec::new();
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
            sort_keys.push((col, o.desc, o.nulls_first));
        }
        let group = binder.group.take();
        let corr = std::mem::take(&mut binder.corr);
        let gcorr = std::mem::take(&mut binder.gcorr);
        let n_gcorr = gcorr.len();
        let mut wins = std::mem::take(&mut binder.wins);

        // Hidden columns follow the row: the grouping mask (grouped queries),
        // sub-query values, then window function results.
        let mask_pos = group.as_ref().map_or(0, |g| g.exprs.len() + g.aggs.len());
        let has_mask = group.is_some() && (!sel.group_sets.is_empty() || binder.grouping_used);
        let extra_base = match &group {
            Some(_) => mask_pos + usize::from(has_mask),
            None => scope.cols.len() + corr.len(),
        };
        let remap = move |i: usize| {
            if i == MASK_COL {
                mask_pos
            } else if i >= GCORR_BASE {
                extra_base + (i - GCORR_BASE)
            } else if i >= WIN_BASE {
                extra_base + n_gcorr + (i - WIN_BASE)
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

        let mut presorted = false;
        let ordered = if !is_agg && wins.is_empty() && corr.is_empty() && !sel.distinct && !sel.order_by.is_empty() {
            match (&sel.from, need) {
                (Some(from), Some(need)) if from.joins.is_empty() => {
                    self.index_ordered_rows(sel, &sources[0], &scope, &out_items, filter.as_ref(), need, depth)?
                }
                _ => None,
            }
        } else {
            None
        };
        let mut rows: Vec<Row> = match ordered {
            Some(r) => {
                presorted = true;
                r
            }
            None => match &sel.from {
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
        } };
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
            self.note(depth, || format!("group {} rows{}", rows.len(), if sel.group_sets.is_empty() { String::new() } else { format!(" into {} grouping set(s)", sel.group_sets.len()) }));
            rows = aggregate(rows, &g.exprs, &g.aggs, &sel.group_sets, has_mask)?;
            if !gcorr.is_empty() {
                rows = self.add_corr_columns(rows, &gcorr, depth)?;
            }
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
        if !sort_keys.is_empty() && !presorted {
            self.note(depth, || {
                match need {
                    Some(k) if k > 0 && k.saturating_mul(2) < out.len() => format!("sort: top-{k} of {} rows", out.len()),
                    _ => format!("sort: all {} rows", out.len()),
                }
            });
            let cmp = |a: &Row, b: &Row| -> Ordering {
                for &(c, desc, nulls_first) in &sort_keys {
                    let o = cmp_nulls(&a[c], &b[c], desc, nulls_first);
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
        let input_rows = rows.len();
        let runs = std::cell::Cell::new(0usize);
        let mut caches: Vec<HashMap<Vec<Value>, Arc<Vec<Row>>>> = specs.iter().map(|_| HashMap::new()).collect();
        // Sub-queries whose correlation is all equalities may be run once and looked up (a semi-join).
        let hashed: Vec<Option<super::decorrelate::Hashed>> =
            specs.iter().map(|s| self.hash_subquery(s, &rows, depth)).collect::<Result<_>>()?;
        let mut probes = 0usize;
        for row in &mut rows {
            for ((spec, cache), hash) in specs.iter().zip(&mut caches).zip(&hashed) {
                let key: Vec<Value> = spec.outer.iter().map(|(_, c)| row[*c].clone()).collect();
                let fast = hash.as_ref().and_then(|h| {
                    let k = h.key_of(row);
                    h.can_answer(&k).then(|| h.lookup(&k))
                });
                let result = match fast {
                    Some(r) => {
                        probes += 1;
                        r
                    }
                    None => match cache.get(&key) {
                        Some(r) => Arc::clone(r),
                        None => {
                            let values: HashMap<String, Value> =
                                spec.outer.iter().map(|(n, _)| n.clone()).zip(key.iter().cloned()).collect();
                            let mut q = spec.query.clone();
                            crate::sql::walk::bind_params_select(&mut q, &values)?;
                            runs.set(runs.get() + 1);
                            let res = self.run_select_depth(&q, depth + 1)?;
                            if !matches!(spec.kind, CorrKind::Exists { .. }) && res.columns.len() != 1 {
                                return Err(Error::runtime("a sub-query used as a value must return exactly one column"));
                            }
                            if cache.len() >= CACHE_LIMIT {
                                cache.clear();
                            }
                            let rc = Arc::new(res.rows);
                            cache.insert(key, Arc::clone(&rc));
                            rc
                        }
                    },
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
                            // (nothing to be among: false, so NOT IN is true)
                            if result.is_empty() { Value::Bool(*negated) } else { Value::Null }
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
        self.note(depth, || {
            let plural = if specs.len() == 1 { "y" } else { "ies" };
            let built: usize = hashed.iter().flatten().map(|h| h.inner_rows).sum();
            if hashed.iter().any(Option::is_some) {
                format!(
                    "correlated sub-quer{plural}: semi-join, the inner query ran once and kept {built} row(s) in a hash table; {probes} lookup(s) for {input_rows} row(s), {} separate run(s)",
                    runs.get()
                )
            } else {
                format!("correlated sub-quer{plural}: ran {} time(s) for {input_rows} row(s) (once per distinct set of outer values)", runs.get())
            }
        });
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

        let mut keys: Vec<(usize, bool, Option<bool>)> = Vec::new();
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
            keys.push((col, o.desc, o.nulls_first));
        }
        if !keys.is_empty() {
            rows.sort_by(|a, b| {
                for &(c, desc, nulls_first) in &keys {
                    let o = cmp_nulls(&a[c], &b[c], desc, nulls_first);
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
        let single = from.joins.is_empty();
        if single {
            let base = &sources[0];
            let rows = match &base.data {
                SourceData::Lateral(_) => {
                    return Err(Error::runtime("a LATERAL item needs another table before it in the FROM clause"))
                }
                SourceData::Table(t) => {
                    self.note(depth, || scan_note(base, t, conj));
                    scan_table(t, conj, filter, limit)?
                }
                SourceData::Rows(all) => {
                    let mut out = Vec::new();
                    for row in all {
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
                    self.note(depth, || format!("scan {} (computed rows)", base.alias));
                    out
                }
            };
            self.note(depth, || {
                if self.optimizer_on && matches!(base.data, SourceData::Table(_)) && !conj.is_empty() {
                    let conds: Vec<BExpr> = conj.iter().map(|c| (*c).clone()).collect();
                    format!("  -> est {:.0} rows, actual {}", super::optimizer::estimate(base, &conds), rows.len())
                } else {
                    format!("  -> {} rows", rows.len())
                }
            });
            return Ok(rows);
        }

        let m = sources.len();
        let optimize = m <= MAX_SOURCES && self.optimizer_on;
        let mut offs = Vec::with_capacity(m);
        let mut total = 0;
        for s in sources {
            offs.push(total);
            total += s.ncols();
        }
        let kinds: Vec<JoinKind> = from.joins.iter().map(|j| j.kind).collect();

        // Bind every ON condition against the columns it can see.
        let mut on_b: Vec<Option<BExpr>> = Vec::with_capacity(kinds.len());
        for (k, _) in kinds.iter().enumerate() {
            on_b.push(match &ons[k] {
                Some(on) => {
                    let prefix = scope.prefix(offs[k + 1] + sources[k + 1].ncols());
                    Some(Binder::new(self, &prefix, depth).bind(on)?)
                }
                None => None,
            });
        }

        // Inner and cross joins may be done in any order; outer joins keep theirs.
        let has_lateral = sources.iter().any(|s| matches!(s.data, SourceData::Lateral(_)));
        let reorder = optimize && !has_lateral && kinds.iter().all(|k| matches!(k, JoinKind::Inner | JoinKind::Cross));

        // The conditions, each with the sources it reads.
        let mut preds: Vec<Pred> = Vec::new();
        if optimize {
            for c in conj {
                preds.push(Pred { expr: (*c).clone(), mask: source_mask(c, &offs), from_where: true, used: false });
            }
            if reorder {
                for ob in on_b.iter().flatten() {
                    for c in split_and(ob) {
                        preds.push(Pred { expr: c.clone(), mask: source_mask(c, &offs), from_where: false, used: false });
                    }
                }
            } else {
                // A LEFT or INNER join's ON condition on the joined table alone can
                // filter that table before the join.
                for (k, ob) in on_b.iter().enumerate() {
                    if let (Some(ob), true) = (ob, matches!(kinds[k], JoinKind::Inner | JoinKind::Left)) {
                        for c in split_and(ob) {
                            if source_mask(c, &offs) == 1 << (k + 1) {
                                preds.push(Pred { expr: c.clone(), mask: 1 << (k + 1), from_where: false, used: false });
                            }
                        }
                    }
                }
            }
        }

        // Conditions on one source alone move into its scan (bound to its own columns).
        let mut pre: Vec<Vec<BExpr>> = sources.iter().map(|s| s.intrinsic.clone()).collect();
        for p in preds.iter_mut() {
            if p.mask.count_ones() == 1 {
                let s = p.mask.trailing_zeros() as usize;
                if !p.from_where || !nullable(&kinds, s) {
                    let off = offs[s];
                    pre[s].push(p.expr.map_cols(&move |i| i - off));
                    p.used = true;
                }
            }
        }

        let est: Vec<f64> = (0..m).map(|s| if optimize { estimate(&sources[s], &pre[s]) } else { 0.0 }).collect();
        let order: Vec<usize> = if reorder { choose_order(sources, &est, &preds, &offs) } else { (0..m).collect() };
        let reordered = order.iter().enumerate().any(|(i, &s)| i != s);
        self.note(depth, || {
            let names: Vec<&str> = order.iter().map(|&s| sources[s].alias.as_str()).collect();
            if reordered {
                format!("join order: {} (reordered from the query's order)", names.join(" -> "))
            } else {
                format!("join order: {}", names.join(" -> "))
            }
        });

        // Layout of the rows being built, which follows the join order.
        let mut eoff = vec![0usize; m];
        let mut pos = 0;
        for &s in &order {
            eoff[s] = pos;
            pos += sources[s].ncols();
        }
        let owner = |c: usize| offs.partition_point(|&o| o <= c).saturating_sub(1);
        let colmap = |c: usize| {
            let s = owner(c);
            eoff[s] + c - offs[s]
        };
        let and = |v: Vec<BExpr>| v.into_iter().reduce(|a, b| BExpr::Bin(BinOp::And, Box::new(a), Box::new(b)));

        // First source.
        let first = order[0];
        let mut rows = self.scan_source(&sources[first], &pre[first], depth, est[first])?;
        let mut placed: SourceMask = bit(first);
        let mut width = sources[first].ncols();
        if reorder {
            // Conditions that read no table at all.
            let consts: Vec<BExpr> = preds.iter_mut().filter(|p| p.mask == 0 && !p.used).map(|p| {
                p.used = true;
                p.expr.clone()
            }).collect();
            if let Some(c) = and(consts) {
                if !c.eval_bool(&[])? {
                    rows.clear();
                }
            }
        }

        for (step, &s) in order.iter().enumerate().skip(1) {
            let right = &sources[s];
            placed |= bit(s);
            let (kind, on) = if reorder {
                let ready: Vec<BExpr> = preds
                    .iter_mut()
                    .filter(|p| !p.used && p.mask != 0 && p.mask & !placed == 0)
                    .map(|p| {
                        p.used = true;
                        p.expr.map_cols(&colmap)
                    })
                    .collect();
                (if sources[s].ncols() == 0 { JoinKind::Cross } else { JoinKind::Inner }, and(ready))
            } else {
                (kinds[step - 1], on_b[step - 1].clone())
            };
            // Filtering the joined table first is safe unless it is preserved by the join.
            let right_pre = if matches!(kind, JoinKind::Right | JoinKind::Full) { None } else { and(pre[s].clone()) };
            let before = rows.len();
            let (joined, how) = match &right.data {
                SourceData::Lateral(lat) => self.lateral_step(rows, right, lat, kind, on.as_ref(), right_pre.as_ref(), depth)?,
                _ => join_step(rows, width, right, kind, on.as_ref(), right_pre.as_ref(), self.optimizer_on)?,
            };
            rows = joined;
            self.note(depth, || {
                format!(
                    "join {}: {how}; {before} rows in, {} out (the table has about {} rows after its own conditions)",
                    right.alias,
                    rows.len(),
                    if optimize { format!("{:.0}", est[s]) } else { "?".into() },
                )
            });
            width += right.ncols();
        }

        if reorder {
            if reordered {
                let mut out = Vec::with_capacity(rows.len());
                for r in &rows {
                    let mut row = Vec::with_capacity(total);
                    for s in 0..m {
                        row.extend_from_slice(&r[eoff[s]..eoff[s] + sources[s].ncols()]);
                    }
                    out.push(row);
                }
                rows = out;
            }
        } else if let Some(f) = filter {
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

    /// `TRANSFORM agg SELECT heads ... PIVOT col`: the values of `col` become columns.
    /// The query is rewritten into an ordinary grouped query with one extra aggregate
    /// per value, restricted to the rows having that value, and then run.
    fn run_crosstab(&self, sel: &Select, depth: u32) -> Result<QueryResult> {
        let ct = sel.crosstab.as_ref().expect("checked by the caller");
        match &ct.agg {
            Expr::Function { name, .. } if AggFn::from_name(name).is_some() => {}
            _ => return Err(Error::runtime("TRANSFORM must be followed by an aggregate function such as Sum(...) or Count(*)")),
        }
        let mut base = sel.clone();
        base.crosstab = None;
        if base.from.is_none() {
            return Err(Error::runtime("a crosstab query needs a FROM clause"));
        }
        if self.optimizer_on {
            if let Some(result) = self.crosstab_one_pass(sel, ct, depth)? {
                return Ok(result);
            }
        }

        // The columns: the listed values, or every value the rows have, in order.
        let values: Vec<Value> = match &ct.values {
            Some(list) => {
                let scope = Scope::default();
                let mut out = Vec::with_capacity(list.len());
                for e in list {
                    out.push(Binder::new(self, &scope, depth).bind(e)?.eval(&[])?.into_owned());
                }
                out
            }
            None => {
                let probe = Select {
                    distinct: true,
                    items: vec![SelectItem::Expr { expr: ct.pivot.clone(), alias: None }],
                    from: base.from.clone(),
                    filter: base.filter.clone(),
                    with: base.with.clone(),
                    ..Select::default()
                };
                let mut vals: Vec<Value> = self.run_select_depth(&probe, depth + 1)?.rows.into_iter().map(|mut r| r.swap_remove(0)).collect();
                vals.sort();
                vals.dedup();
                vals
            }
        };
        self.note(depth, || format!("crosstab: {} pivot column(s)", values.len()));

        for v in values {
            let cond = if v.is_null() {
                Expr::IsNull { expr: Box::new(ct.pivot.clone()), negated: false }
            } else {
                Expr::Binary { op: BinOp::Eq, left: Box::new(ct.pivot.clone()), right: Box::new(Expr::Literal(v.clone())) }
            };
            let guard = |inner: Expr| Expr::Function {
                name: "IIF".into(),
                args: vec![cond.clone(), inner, Expr::Literal(Value::Null)],
                distinct: false,
                star: false,
            };
            let mut cell = ct.agg.clone();
            if let Expr::Function { args, star, .. } = &mut cell {
                if *star {
                    *star = false;
                    args.push(guard(Expr::Literal(Value::Int(1))));
                } else if let Some(a) = args.first_mut() {
                    let inner = std::mem::replace(a, Expr::Literal(Value::Null));
                    *a = guard(inner);
                }
            }
            // A missing value is headed "<>" as in Access.
            let alias = if v.is_null() { "<>".to_string() } else { v.to_string() };
            base.items.push(SelectItem::Expr { expr: cell, alias: Some(alias) });
        }
        self.run_select_depth(&base, depth)
    }

    /// The optimised crosstab. Instead of one conditional aggregate per pivot column
    /// (which evaluates every column's condition for every row), group once by the row
    /// headings *and* the pivot value, so each cell is computed in a single pass, and
    /// then lay the cells out. The pivot values come out of that same pass, so no
    /// separate query is needed to discover them. `None` when the shape does not allow
    /// it, and the plain rewrite is used instead.
    fn crosstab_one_pass(&self, sel: &Select, ct: &Crosstab, depth: u32) -> Result<Option<QueryResult>> {
        let Expr::Function { name, args, star, .. } = &ct.agg else { return Ok(None) };
        let missing = if AggFn::from_name(name) == Some(AggFn::Count) { Value::Int(0) } else { Value::Null };
        if sel.distinct || !sel.group_sets.is_empty() || !sel.unions.is_empty() || args.len() > 1 || (*star && !args.is_empty()) {
            return Ok(None);
        }
        let keys = &sel.group_by;

        // Every cell: the aggregate for each (row heading, pivot value).
        let mut cells = Select {
            from: sel.from.clone(),
            filter: sel.filter.clone(),
            with: sel.with.clone(),
            ..Select::default()
        };
        for k in keys {
            cells.items.push(SelectItem::Expr { expr: k.clone(), alias: None });
            cells.group_by.push(k.clone());
        }
        cells.items.push(SelectItem::Expr { expr: ct.pivot.clone(), alias: None });
        cells.items.push(SelectItem::Expr { expr: ct.agg.clone(), alias: None });
        cells.group_by.push(ct.pivot.clone());
        let grid = self.run_select_depth(&cells, depth + 1)?;
        let nk = keys.len();

        let mut grid_map: HashMap<Vec<Value>, HashMap<Value, Value>> = HashMap::new();
        let mut seen: Vec<Value> = Vec::new();
        for mut row in grid.rows {
            let cell = row.pop().expect("aggregate column");
            let pivot = row.pop().expect("pivot column");
            seen.push(pivot.clone());
            grid_map.entry(row).or_default().insert(pivot, cell);
        }
        let values: Vec<Value> = match &ct.values {
            Some(list) => {
                let scope = Scope::default();
                let mut out = Vec::with_capacity(list.len());
                for e in list {
                    out.push(Binder::new(self, &scope, depth).bind(e)?.eval(&[])?.into_owned());
                }
                out
            }
            None => {
                seen.sort();
                seen.dedup();
                seen
            }
        };
        self.note(depth, || {
            format!(
                "crosstab: one grouped pass computed {} cell(s) for {} pivot column(s) (no separate query per column)",
                grid_map.values().map(HashMap::len).sum::<usize>(),
                values.len()
            )
        });

        // The row headings and any other aggregates of the SELECT, with the heading
        // values carried along so that each row can find its cells.
        let mut base = sel.clone();
        base.crosstab = None;
        for (i, k) in keys.iter().enumerate() {
            base.items.push(SelectItem::Expr { expr: k.clone(), alias: Some(format!("__ct{i}")) });
        }
        let mut result = self.run_select_depth(&base, depth)?;
        let visible = result.columns.len() - nk;
        result.columns.truncate(visible);
        result.columns.extend(values.iter().map(|v| if v.is_null() { "<>".to_string() } else { v.to_string() }));
        for row in &mut result.rows {
            let key: Vec<Value> = row.split_off(visible);
            let found = grid_map.get(&key);
            for v in &values {
                row.push(found.and_then(|m| m.get(v)).cloned().unwrap_or_else(|| missing.clone()));
            }
        }
        Ok(Some(result))
    }

    /// `SELECT COUNT(*), ... FROM t` with no condition, grouping or limit.
    fn count_shortcut(&self, sel: &Select, sources: &[Source<'_>], out_items: &[(String, Expr)]) -> Option<QueryResult> {
        if !self.optimizer_on {
            return None;
        }
        let from = sel.from.as_ref()?;
        if !from.joins.is_empty()
            || sel.filter.is_some()
            || !sel.group_by.is_empty()
            || sel.having.is_some()
            || sel.top.is_some()
            || sel.limit.is_some()
            || sel.offset.is_some()
            || sel.distinct
        {
            return None;
        }
        if !sources.first()?.intrinsic.is_empty() {
            return None;
        }
        let SourceData::Table(t) = &sources.first()?.data else { return None };
        let all_counts = !out_items.is_empty()
            && out_items.iter().all(|(_, e)| matches!(e, Expr::Function { name, star: true, distinct: false, .. } if name.eq_ignore_ascii_case("COUNT")));
        all_counts.then(|| QueryResult {
            columns: out_items.iter().map(|(n, _)| n.clone()).collect(),
            rows: vec![vec![Value::Int(t.len() as i64); out_items.len()]],
            affected: 0,
        })
    }

    /// `ORDER BY` on columns that an index delivers in order, with a row limit:
    /// read the index in order and stop as soon as enough rows qualify, instead
    /// of finding every row and sorting. `None` when the plan does not apply.
    #[allow(clippy::too_many_arguments)]
    fn index_ordered_rows(
        &self,
        sel: &Select,
        src: &Source<'_>,
        scope: &Scope,
        out_items: &[(String, Expr)],
        filter: Option<&BExpr>,
        need: usize,
        depth: u32,
    ) -> Result<Option<Vec<Row>>> {
        let SourceData::Table(t) = &src.data else { return Ok(None) };
        if !self.optimizer_on {
            return Ok(None);
        }
        // The columns being sorted on, which must be plain columns of the table.
        let mut cols = Vec::new();
        for o in &sel.order_by {
            let target = match &o.expr {
                Expr::Literal(Value::Int(n)) if *n >= 1 && (*n as usize) <= out_items.len() => &out_items[*n as usize - 1].1,
                Expr::Column { table: None, name } => out_items
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(name))
                    .map_or(&o.expr, |(_, e)| e),
                other => other,
            };
            let Expr::Column { table, name } = target else { return Ok(None) };
            match scope.resolve(table.as_deref(), name) {
                Ok(c) if c < t.schema().columns.len() => cols.push(c),
                _ => return Ok(None),
            }
        }
        if need == 0 {
            return Ok(None);
        }
        let k = cols.len();
        // The direction each sort column wants. An index serves them if they are its own directions or
        // all the opposite (it is walked backwards); an index of only ascending columns serves the
        // all-ascending and all-descending orders.
        let want: Vec<bool> = sel.order_by.iter().map(|o| o.desc).collect();
        let desc = want[0];
        let uniform = want.iter().all(|d| *d == desc);
        // NULLs sit where the direction puts them (first when ascending) unless the query says otherwise.
        let natural = sel.order_by.iter().all(|o| o.nulls_first.is_none_or(|n| n == !o.desc));
        let natural_first = !desc;
        let nulls_first = sel.order_by[0].nulls_first.unwrap_or(natural_first);
        if !natural && (!uniform || k > 1 || sel.order_by.iter().any(|o| o.nulls_first.unwrap_or(natural_first) != nulls_first)) {
            // (placing NULLs against the grain is handled for a single sort column of a plain index)
            return Ok(None);
        }
        let conj: Vec<&BExpr> = filter.map(split_and).unwrap_or_default();
        let defs: Vec<_> = t.index_defs().collect();
        let usable = |d: &IndexDef, start: usize| {
            d.columns.len() >= start + k && d.columns[start..start + k] == cols[..] && index_walk(d, start, &want).is_some() && (natural || !d.has_desc())
        };
        // Which index and key prefix give the order?
        let (idx, prefix): (usize, Vec<Value>) = match choose_access(t, &conj) {
            Access::Prefix { idx, key } if usable(defs[idx], key.len()) => (idx, key),
            Access::Prefix { idx, key } => {
                // A selective lookup followed by a small sort beats walking an index.
                if t.index_prefix(idx, &key).take(2048).count() < 2048 {
                    return Ok(None);
                }
                match defs.iter().position(|d| usable(d, 0)) {
                    Some(i) => (i, Vec::new()),
                    None => return Ok(None),
                }
            }
            Access::Range { .. } => return Ok(None),
            Access::Full => match defs.iter().position(|d| usable(d, 0)) {
                Some(i) => (i, Vec::new()),
                None => return Ok(None),
            },
        };
        // Walked backwards when the query asks for the opposite of what the index holds.
        let backwards = index_walk(defs[idx], prefix.len(), &want).unwrap_or(false);
        self.note(depth, || {
            format!(
                "order by satisfied by index {} ({}{}), stopping after {need} rows: no sort",
                defs[idx].name,
                if !uniform { "mixed directions" } else if desc { "descending" } else { "ascending" },
                if nulls_first == natural_first { "" } else if nulls_first { ", NULLs first" } else { ", NULLs last" }
            )
        });
        let mut out = Vec::new();
        let null_at = |id: RowId| t.get(id).is_some_and(|r| r[cols[0]].is_null());
        // Ids in the order wanted. The natural index order already puts NULLs where the
        // query wants them unless it asked for the other end.
        let ids: Box<dyn Iterator<Item = RowId> + '_> = if natural {
            t.index_ordered(idx, &prefix, prefix.len() + k, backwards)
        } else if !desc {
            // ascending, NULLs last: everything but the leading NULLs, then those
            let rest = t.index_ordered(idx, &prefix, prefix.len() + k, false).skip_while(move |&id| null_at(id));
            let nulls = t.index_ordered(idx, &prefix, prefix.len() + k, false).take_while(move |&id| null_at(id));
            Box::new(rest.chain(nulls))
        } else {
            // descending, NULLs first: the NULLs (in storage order), then the rest descending
            let nulls = t.index_ordered(idx, &prefix, prefix.len() + k, false).take_while(move |&id| null_at(id));
            let rest = t.index_ordered(idx, &prefix, prefix.len() + k, true).filter(move |&id| !null_at(id));
            Box::new(nulls.chain(rest))
        };
        for id in ids {
            let Some(row) = t.get(id) else { continue };
            if let Some(f) = filter {
                if !f.eval_bool(row)? {
                    continue;
                }
            }
            out.push(row.clone());
            if out.len() >= need {
                break;
            }
        }
        self.note(depth, || format!("  -> {} rows read in index order", out.len()));
        Ok(Some(out))
    }

    /// The rows of one source that satisfy `pre` (conditions bound to its own columns).
    fn scan_source(&self, src: &Source<'_>, pre: &[BExpr], depth: u32, est: f64) -> Result<Vec<Row>> {
        let filter = pre.iter().cloned().reduce(|a, b| BExpr::Bin(BinOp::And, Box::new(a), Box::new(b)));
        let rows = match &src.data {
            SourceData::Lateral(_) => {
                return Err(Error::runtime("a LATERAL item needs another table before it in the FROM clause"))
            }
            SourceData::Table(t) => {
                let refs: Vec<&BExpr> = pre.iter().collect();
                self.note(depth, || scan_note(src, t, &refs));
                scan_table(t, &refs, filter.as_ref(), None)?
            }
            SourceData::Rows(all) => {
                self.note(depth, || format!("scan {} (computed rows)", src.alias));
                let mut out = Vec::new();
                for row in all {
                    if let Some(f) = &filter {
                        if !f.eval_bool(row)? {
                            continue;
                        }
                    }
                    out.push(row.clone());
                }
                out
            }
        };
        self.note(depth, || format!("  -> est {est:.0} rows, actual {}", rows.len()));
        Ok(rows)
    }
}

/// A plan line for scanning `src` under the conditions `conj`.
fn scan_note(src: &Source<'_>, t: &Table, conj: &[&BExpr]) -> String {
    let how = match choose_access(t, conj) {
        Access::Full => "full scan".to_string(),
        Access::Prefix { idx, key } => {
            let def = t.index_defs().nth(idx);
            format!(
                "index {} on {} = ({})",
                def.map_or("?", |d| d.name.as_str()),
                def.map(|d| d.columns[..key.len()].iter().map(|&c| t.schema().columns[c].name.clone()).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default(),
                super::optimizer::describe_key(&key)
            )
        }
        Access::Range { idx, .. } => {
            let def = t.index_defs().nth(idx);
            format!(
                "index range scan {} on {}",
                def.map_or("?", |d| d.name.as_str()),
                def.map(|d| t.schema().columns[d.columns[0]].name.clone()).unwrap_or_default()
            )
        }
    };
    let filtered = if conj.is_empty() { String::new() } else { format!(" ({} condition(s) pushed down)", conj.len()) };
    format!("scan {} [{} rows]: {how}{filtered}", src.alias, t.estimated_len())
}

// ---------------------------------------------------------------------------
// Select list
// ---------------------------------------------------------------------------

pub(crate) fn expand_items(sel: &Select, scope: &Scope) -> Result<Vec<(String, Expr)>> {
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
                for c in scope.cols.iter().filter(|c| c.table.eq_ignore_ascii_case(t) && !c.name.starts_with(super::viewdml::HIDDEN_COLUMN)) {
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

pub(crate) enum Access {
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
            (DataType::Text, Value::Text(_))
                | (DataType::DateTime | DataType::TimestampTz, Value::DateTime(_) | Value::TimestampTz(_))
                | (DataType::Interval, Value::Interval(_))
                | (DataType::YesNo, Value::Bool(_))
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
pub(crate) fn choose_access(table: &Table, conj: &[&BExpr]) -> Access {
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

pub(crate) fn candidates<'t>(table: &'t Table, access: Access) -> Box<dyn Iterator<Item = (RowId, &'t Row)> + 't> {
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

/// How an index delivers the order `want` for its key columns from position `start` (`true`: descending):
/// `Some(false)` read forwards, `Some(true)` read backwards, `None` not at all.
fn index_walk(def: &IndexDef, start: usize, want: &[bool]) -> Option<bool> {
    if want.iter().enumerate().all(|(j, &w)| def.is_desc(start + j) == w) {
        Some(false)
    } else if want.iter().enumerate().all(|(j, &w)| def.is_desc(start + j) != w) {
        Some(true)
    } else {
        None
    }
}

pub(crate) fn scan_table(table: &Table, conj: &[&BExpr], filter: Option<&BExpr>, limit: Option<usize>) -> Result<Vec<Row>> {
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

fn join_step(
    left: Vec<Row>,
    lw: usize,
    right: &Source<'_>,
    kind: JoinKind,
    on: Option<&BExpr>,
    right_pre: Option<&BExpr>,
    allow_swap: bool,
) -> Result<(Vec<Row>, String)> {
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
                if left.len() <= t.estimated_len().saturating_mul(4) || t.estimated_len() > 64 {
                    break 'pick Strategy::Index { idx, left_cols };
                }
            }
        }
        Strategy::Hash {
            left: keyed.iter().map(|k| k.0.clone()).collect(),
            right: keyed.iter().map(|k| k.1.clone()).collect(),
        }
    };

    // A hash join builds on the smaller input: with an inner join and far fewer
    // rows on the left, hash the left and probe with the right.
    let swap = allow_swap && matches!(&strategy, Strategy::Hash { .. }) && kind == JoinKind::Inner && left.len().saturating_mul(2) < right.slot_count();

    // Hash table over the right side, built once.
    let mut hash: HashMap<Vec<Value>, Vec<usize>> = HashMap::new();
    if let (false, Strategy::Hash { right: right_keys, .. }) = (swap, &strategy) {
        for slot in 0..right.slot_count() {
            let Some(r) = right.get(slot) else { continue };
            if let Some(p) = right_pre {
                if !p.eval_bool(r)? {
                    continue;
                }
            }
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
    let how = match &strategy {
        Strategy::Scan => "nested loop".to_string(),
        Strategy::Hash { .. } if swap => format!("hash join, built on the {} left rows", left.len()),
        Strategy::Hash { .. } => format!("hash join, built on the right table ({} slots)", right.slot_count()),
        Strategy::Index { idx, left_cols } => match &right.data {
            SourceData::Table(t) => format!(
                "index nested-loop via {} ({} key column(s))",
                t.index_defs().nth(*idx).map_or("?", |d| d.name.as_str()),
                left_cols.len()
            ),
            _ => "index nested-loop".to_string(),
        },
    };
    if swap {
        let Strategy::Hash { left: left_keys, right: right_keys } = &strategy else { unreachable!("checked above") };
        let mut table: HashMap<Vec<Value>, Vec<usize>> = HashMap::new();
        for (i, l) in left.iter().enumerate() {
            let key = left_keys.iter().map(|e| e.eval(l).map(|v| v.into_owned())).collect::<Result<Vec<Value>>>()?;
            if !key.iter().any(Value::is_null) {
                table.entry(key).or_default().push(i);
            }
        }
        let mut out: Vec<Row> = Vec::new();
        let mut scratch: Row = Vec::with_capacity(lw + rw);
        for slot in 0..right.slot_count() {
            let Some(r) = right.get(slot) else { continue };
            if let Some(p) = right_pre {
                if !p.eval_bool(r)? {
                    continue;
                }
            }
            let key = right_keys.iter().map(|e| e.eval(r).map(|v| v.into_owned())).collect::<Result<Vec<Value>>>()?;
            if key.iter().any(Value::is_null) {
                continue;
            }
            if let Some(hits) = table.get(&key) {
                for &i in hits {
                    scratch.clear();
                    scratch.extend_from_slice(&left[i]);
                    scratch.extend_from_slice(r);
                    if let Some(p) = residual {
                        if !p.eval_bool(&scratch)? {
                            continue;
                        }
                    }
                    out.push(scratch.clone());
                }
            }
        }
        return Ok((out, how));
    }
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
            if let Some(p) = right_pre {
                if !p.eval_bool(r)? {
                    continue;
                }
            }
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
    Ok((out, how))
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

fn aggregate(rows: Vec<Row>, group_exprs: &[BExpr], aggs: &[AggSpec], sets: &[Vec<usize>], mask_wanted: bool) -> Result<Vec<Row>> {
    if sets.is_empty() {
        // (a plain GROUP BY has one set, with nothing rolled up: the mask is 0, if GROUPING() asks for it)
        return aggregate_set(&rows, group_exprs, aggs, None, mask_wanted.then_some(0));
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

/// Compares two sort values, with NULLs placed by `nulls_first` (by default first when
/// ascending and last when descending, which is where they sort naturally).
pub(crate) fn cmp_nulls(a: &Value, b: &Value, desc: bool, nulls_first: Option<bool>) -> Ordering {
    let first = nulls_first.unwrap_or(!desc);
    match (a.is_null(), b.is_null()) {
        (true, true) => Ordering::Equal,
        (true, false) => if first { Ordering::Less } else { Ordering::Greater },
        (false, true) => if first { Ordering::Greater } else { Ordering::Less },
        _ => {
            let o = a.cmp(b);
            if desc { o.reverse() } else { o }
        }
    }
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
fn subst_cte(s: &mut Select, name: &str, rows: &Arc<Vec<Row>>, cols: &[String]) {
    fn factor(f: &mut TableFactor, name: &str, rows: &Arc<Vec<Row>>, cols: &[String]) {
        match f {
            TableFactor::Named { name: n, alias } if n.eq_ignore_ascii_case(name) => {
                let alias = alias.clone().unwrap_or_else(|| n.clone());
                *f = TableFactor::Materialized { rows: Arc::clone(rows), columns: cols.to_vec(), alias };
            }
            TableFactor::Derived { query, .. } => subst_cte(query, name, rows, cols),
            TableFactor::Pivot { source, .. } | TableFactor::Unpivot { source, .. } => factor(source, name, rows, cols),
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
    subst_cte(&mut copy, name, &Arc::new(Vec::new()), &[]);
    copy != *s
}

impl Database {
    /// `WITH name AS (...) query`: each CTE runs once, in order, and its rows
    /// stand in for the name wherever the rest of the statement uses it.
    fn run_with(&self, sel: &Select, depth: u32) -> Result<QueryResult> {
        let with = sel.with.as_ref().expect("checked by the caller");
        let mut body = sel.clone();
        body.with = None;
        let mut done: Vec<(String, Arc<Vec<Row>>, Vec<String>)> = Vec::new();
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
            done.push((cte.name.clone(), Arc::new(rows), cols));
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
            let current = Arc::new(std::mem::take(&mut working));
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
