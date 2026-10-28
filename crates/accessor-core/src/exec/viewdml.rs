//! Changing data through a view, and reading simple views without materialising them.
//!
//! A saved query is *simple* when it reads exactly one table (or another simple view),
//! shows plain columns of it (renamed if you like), and has no `DISTINCT`, grouping,
//! aggregate, limit, window or set operator. Such a view is a window onto its table, so
//! `INSERT`, `UPDATE` and `DELETE` on it are carried out on that table, with the view's
//! `WHERE` condition added to the statement. With `WITH CHECK OPTION` a change that would
//! leave a row outside the view is refused.
//!
//! The same analysis lets the optimizer *merge* a view that shows every column of its table:
//! the outer query reads the table directly, with the view's condition applied during the
//! scan, so indexes and pushdown work through the view instead of the view being built first.

use super::bind::{Binder, Scope, ScopeCol};
use super::select::{Source, SourceData};
use super::QueryResult;
use crate::database::Database;
use crate::error::{Error, Result};
use crate::sql::ast::*;
use crate::sql::walk::walk_expr;
use crate::sql::{parse_query_def, QueryDef};
use crate::storage::Op;

/// Column names starting with this cannot be written in a query: the columns of a merged view or
/// derived table that it does not show.
pub(crate) const HIDDEN_COLUMN: char = '\u{1}';

/// What a simple view stands for.
pub(crate) struct ViewTarget {
    /// The table (or view) it reads.
    pub base: String,
    /// Its `WHERE` condition, written for the base table.
    pub filter: Option<Expr>,
    /// For each column of the view: its name, and the base column it shows.
    pub columns: Vec<(String, String)>,
    pub check: bool,
}

fn and(a: Option<Expr>, b: Option<Expr>) -> Option<Expr> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Expr::Binary { op: BinOp::And, left: Box::new(a), right: Box::new(b) }),
        (a, None) => a,
        (None, b) => b,
    }
}

impl Database {
    /// Analyses view `name`; the error says why it cannot take changes.
    pub(crate) fn view_target(&self, name: &str) -> Result<ViewTarget> {
        let sql = self
            .view_sql(name)
            .ok_or_else(|| Error::catalog(format!("the table or query '{name}' does not exist")))?;
        let cannot = |why: &str| Error::runtime(format!("the query '{name}' cannot be changed: {why}"));
        let check = sql.trim_start().starts_with("/*CHECK OPTION*/");
        let (params, select) = match parse_query_def(sql)? {
            QueryDef::Select { params, select } => (params, select),
            QueryDef::Exec { .. } => return Err(cannot("it calls another query")),
        };
        if !params.is_empty() {
            return Err(cannot("it has parameters"));
        }
        // A column-name list is stored as a wrapper around the real query.
        let (inner, renames) = match (select.items.as_slice(), &select.from) {
            ([SelectItem::Wildcard], Some(From { first: TableFactor::Derived { query, alias, columns, .. }, joins }))
                if alias == "__v" && joins.is_empty() && select.filter.is_none() =>
            {
                ((**query).clone(), columns.clone())
            }
            _ => (select.clone(), Vec::new()),
        };
        self.simple_target(&inner, &renames, check, name)
    }

    /// Analyses a plain `SELECT` (a view's query, or a derived table) that reads one table.
    pub(crate) fn simple_target(&self, inner: &Select, renames: &[String], check: bool, name: &str) -> Result<ViewTarget> {
        let cannot = |why: &str| Error::runtime(format!("the query '{name}' cannot be changed: {why}"));
        if inner.distinct
            || inner.top.is_some()
            || inner.limit.is_some()
            || inner.offset.is_some()
            || !inner.group_by.is_empty()
            || inner.having.is_some()
            || !inner.group_sets.is_empty()
            || !inner.unions.is_empty()
            || inner.with.is_some()
            || inner.crosstab.is_some()
        {
            return Err(cannot("it summarises, limits or combines rows"));
        }
        let Some(from) = &inner.from else { return Err(cannot("it reads no table")) };
        let TableFactor::Named { name: base, alias, .. } = &from.first else {
            return Err(cannot("it does not read exactly one table"));
        };
        if !from.joins.is_empty() {
            return Err(cannot("it joins tables"));
        }
        let base_names = self.resolve_factor(&from.first, 0)?.names;
        let mut columns: Vec<(String, String)> = Vec::new();
        for item in &inner.items {
            match item {
                SelectItem::Wildcard | SelectItem::QualifiedWildcard(_) => {
                    columns.extend(base_names.iter().map(|n| (n.clone(), n.clone())));
                }
                SelectItem::Expr { expr: Expr::Column { name: col, .. }, alias } => {
                    let real = base_names
                        .iter()
                        .find(|n| n.eq_ignore_ascii_case(col))
                        .ok_or_else(|| Error::catalog(format!("the column '{col}' was not found")))?;
                    columns.push((alias.clone().unwrap_or_else(|| real.clone()), real.clone()));
                }
                SelectItem::Expr { .. } => return Err(cannot("a column is computed by an expression")),
            }
        }
        if !renames.is_empty() {
            if renames.len() != columns.len() {
                return Err(cannot("its column list does not match its query"));
            }
            for (c, r) in columns.iter_mut().zip(renames) {
                c.0 = r.clone();
            }
        }
        let mut filter = inner.filter.clone();
        if let Some(f) = &mut filter {
            let table_alias = alias.clone().unwrap_or_else(|| base.clone());
            walk_expr(f, &mut |e| {
                if let Expr::Column { table: Some(q), .. } = e {
                    if q.eq_ignore_ascii_case(&table_alias) {
                        *q = base.clone();
                    }
                }
                Ok(())
            })?;
        }
        Ok(ViewTarget { base: base.clone(), filter, columns, check })
    }

    /// `INSERT`, `UPDATE` or `DELETE` whose target is a view.
    pub(crate) fn exec_view_dml(&mut self, st: &Statement) -> Result<QueryResult> {
        let (view, rewritten, filter_for_check) = match st {
            Statement::Delete { table, filter } => {
                let t = self.view_target(table)?;
                let f = map_filter(filter, &t, table)?;
                let combined = and(t.filter.clone(), f);
                (table, Statement::Delete { table: t.base.clone(), filter: combined }, None)
            }
            Statement::Update { table, assignments, filter } => {
                let t = self.view_target(table)?;
                let mut mapped = Vec::with_capacity(assignments.len());
                for (col, e) in assignments {
                    let mut e = e.clone();
                    map_columns(&mut e, &t, table)?;
                    mapped.push((base_column(&t, col, table)?, e));
                }
                let combined = and(t.filter.clone(), map_filter(filter, &t, table)?);
                let check = t.check.then(|| t.filter.clone()).flatten();
                (table, Statement::Update { table: t.base.clone(), assignments: mapped, filter: combined }, check)
            }
            Statement::Insert { table, columns, source } => {
                let t = self.view_target(table)?;
                let names: Vec<String> = match (columns, source) {
                    (_, InsertSource::DefaultValues) => Vec::new(),
                    (Some(cols), _) => cols.clone(),
                    (None, _) => t.columns.iter().map(|(v, _)| v.clone()).collect(),
                };
                let base_cols = names.iter().map(|c| base_column(&t, c, table)).collect::<Result<Vec<_>>>()?;
                let check = t.check.then(|| t.filter.clone()).flatten();
                (
                    table,
                    Statement::Insert { table: t.base.clone(), columns: Some(base_cols), source: source.clone() },
                    check,
                )
            }
            _ => unreachable!("only data changes are routed here"),
        };
        let _ = view;
        match filter_for_check {
            // WITH CHECK OPTION: plan the change, test every new row, then apply it.
            Some(cond) if self.table(match &rewritten {
                Statement::Update { table, .. } | Statement::Insert { table, .. } => table,
                _ => unreachable!(),
            })
            .is_some() =>
            {
                let ops = match &rewritten {
                    Statement::Update { table, assignments, filter } => self.plan_update(table, assignments, filter.as_ref())?,
                    Statement::Insert { table, columns, source } => self.plan_insert(table, columns, source)?,
                    _ => unreachable!(),
                };
                let base = match &rewritten {
                    Statement::Update { table, .. } | Statement::Insert { table, .. } => table.clone(),
                    _ => unreachable!(),
                };
                let schema = self.table_checked(&base)?.schema().clone();
                let scope = Scope {
                    cols: schema
                        .columns
                        .iter()
                        .map(|c| ScopeCol { table: schema.name.clone(), name: c.name.clone(), hidden: false })
                        .collect(),
                };
                let bound = Binder::new(self, &scope, 0).bind(&cond)?;
                for op in &ops {
                    if let Op::Insert { row, .. } | Op::Update { row, .. } = op {
                        if !bound.eval_bool(row)? {
                            return Err(Error::constraint(
                                "the change is refused: the new row does not satisfy the condition of the view (WITH CHECK OPTION)",
                            ));
                        }
                    }
                }
                let n = ops.len();
                self.apply_all(ops)?;
                Ok(QueryResult::affected(n))
            }
            _ => self.execute_statement(&rewritten),
        }
    }

    /// A simple view that shows every column of its table, as a scan of that table with the
    /// view's condition attached, so that the rest of the query can use the table's indexes.
    pub(crate) fn merged_view(&self, name: &str, alias: &str) -> Result<Option<Source<'_>>> {
        if !self.optimizer_on {
            return Ok(None);
        }
        let Ok(t) = self.view_target(name) else { return Ok(None) };
        self.merged_target(t, alias)
    }

    /// A derived table of the same simple kind, read as its table in the same way.
    pub(crate) fn merged_derived(&self, query: &Select, alias: &str, columns: &[String]) -> Result<Option<Source<'_>>> {
        if !self.optimizer_on {
            return Ok(None);
        }
        let Ok(t) = self.simple_target(query, columns, false, alias) else { return Ok(None) };
        self.merged_target(t, alias)
    }

    /// The scan of the base table that a simple view or derived table stands for. It may show only
    /// some of the table's columns, as long as they keep the table's order; the others are kept in the
    /// row under a name nothing can refer to.
    fn merged_target(&self, t: ViewTarget, alias: &str) -> Result<Option<Source<'_>>> {
        let Some(base) = self.table(&t.base) else { return Ok(None) };
        let schema = base.schema();
        let mut positions = Vec::with_capacity(t.columns.len());
        for (_, b) in &t.columns {
            match schema.column_index(b) {
                Some(p) => positions.push(p),
                None => return Ok(None),
            }
        }
        if !positions.windows(2).all(|w| w[0] < w[1]) {
            return Ok(None);
        }
        let mut names: Vec<String> = (0..schema.columns.len()).map(|i| format!("{HIDDEN_COLUMN}{i}")).collect();
        for ((shown, _), &p) in t.columns.iter().zip(&positions) {
            names[p] = shown.clone();
        }
        let scope = Scope {
            cols: schema
                .columns
                .iter()
                .map(|c| ScopeCol { table: schema.name.clone(), name: c.name.clone(), hidden: false })
                .collect(),
        };
        // A condition that cannot be bound on its own (a sub-query that reads the table's columns, say) is left to the
        // ordinary way of running the view or derived table, which reports any real error itself.
        let intrinsic = match &t.filter {
            Some(f) => match Binder::new(self, &scope, 0).bind(f) {
                Ok(b) => vec![b],
                Err(_) => return Ok(None),
            },
            None => Vec::new(),
        };
        Ok(Some(Source {
            intrinsic,
            data: SourceData::Table(base),
            alias: alias.to_string(),
            names,
        }))
    }
}

/// The base column behind view column `col`.
fn base_column(t: &ViewTarget, col: &str, view: &str) -> Result<String> {
    t.columns
        .iter()
        .find(|(v, _)| v.eq_ignore_ascii_case(col))
        .map(|(_, b)| b.clone())
        .ok_or_else(|| Error::catalog(format!("the query '{view}' has no column '{col}'")))
}

/// Rewrites view column names in `e` to the base table's.
fn map_columns(e: &mut Expr, t: &ViewTarget, view: &str) -> Result<()> {
    walk_expr(e, &mut |x| {
        if let Expr::Column { table, name } = x {
            if table.as_deref().is_none_or(|q| q.eq_ignore_ascii_case(view)) {
                if let Some((_, b)) = t.columns.iter().find(|(v, _)| v.eq_ignore_ascii_case(name)) {
                    *x = Expr::Column { table: None, name: b.clone() };
                }
            }
        }
        Ok(())
    })
}

fn map_filter(filter: &Option<Expr>, t: &ViewTarget, view: &str) -> Result<Option<Expr>> {
    match filter {
        Some(f) => {
            let mut f = f.clone();
            map_columns(&mut f, t, view)?;
            Ok(Some(f))
        }
        None => Ok(None),
    }
}
