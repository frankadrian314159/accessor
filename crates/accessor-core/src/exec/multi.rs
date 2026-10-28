//! MySQL's multi-table `DELETE` and `UPDATE`.
//!
//! ```sql
//! DELETE a FROM t1 AS a, t2 AS b WHERE a.k = b.k;
//! UPDATE t1, t2 SET t1.x = t2.y WHERE t1.k = t2.k;
//! ```
//!
//! Both are rewritten into the single-table statement they amount to. The rows of the target that the join reaches are
//! those for which a row of the join, in which the target table is read as a copy of itself (`__m`), is the same row
//! (all its columns equal, NULLs equal): `DELETE FROM t1 WHERE EXISTS (SELECT 1 FROM t1 AS __m, t2 AS b WHERE ... AND
//! t1.c IS NOT DISTINCT FROM __m.c ...)`. An `UPDATE` takes each new value from a sub-query over the same join. Two rows of
//! the target that are alike in every column are not told apart, so they are treated alike.

use super::QueryResult;
use crate::database::Database;
use crate::error::{Error, Result};
use crate::sql::ast::{BinOp, Expr, From as FromClause, Select, SelectItem, Statement, TableFactor};
use crate::sql::walk::walk_expr;
use crate::value::Value;

const COPY: &str = "__m";

/// Replaces the table qualifier `from` by `COPY` in `e`.
fn requalify(e: &mut Expr, from: &str) {
    let _ = walk_expr(e, &mut |x| {
        if let Expr::Column { table: Some(t), .. } = x {
            if t.eq_ignore_ascii_case(from) {
                *t = COPY.to_string();
            }
        }
        Ok(())
    });
}

fn factor_names(f: &TableFactor) -> Option<(String, String)> {
    match f {
        TableFactor::Named { name, alias, .. } => Some((name.clone(), alias.clone().unwrap_or_else(|| name.clone()))),
        _ => None,
    }
}

/// Renames the table called `target` in `from` to `__m`, rewriting what refers to it; gives the real table's name.
fn rename_target(from: &mut FromClause, target: &str, exprs: &mut [&mut Expr]) -> Result<String> {
    let mut found: Option<String> = None;
    let mut visit = |f: &mut TableFactor| {
        if let TableFactor::Named { name, alias, .. } = f {
            let shown = alias.clone().unwrap_or_else(|| name.clone());
            if found.is_none() && shown.eq_ignore_ascii_case(target) {
                found = Some(name.clone());
                *alias = Some(COPY.to_string());
            }
        }
    };
    visit(&mut from.first);
    for j in &mut from.joins {
        visit(&mut j.factor);
    }
    let Some(base) = found else {
        return Err(Error::catalog(format!("the table '{target}' is not in the list of tables")));
    };
    for e in exprs.iter_mut() {
        requalify(e, target);
    }
    for j in &mut from.joins {
        if let Some(on) = &mut j.on {
            requalify(on, target);
        }
    }
    Ok(base)
}

fn and(a: Option<Expr>, b: Expr) -> Expr {
    match a {
        Some(a) => Expr::Binary { op: BinOp::And, left: Box::new(a), right: Box::new(b) },
        None => b,
    }
}

/// `base.c IS NOT DISTINCT FROM __m.c` for every column of `base`.
fn same_row(db: &Database, base: &str) -> Result<Expr> {
    same_row_as(db, base, COPY)
}

/// `base.c IS NOT DISTINCT FROM other.c` for every column of `base`.
fn same_row_as(db: &Database, base: &str, other: &str) -> Result<Expr> {
    let schema = db.table_checked(base)?.schema().clone();
    let mut out: Option<Expr> = None;
    for c in &schema.columns {
        let a = Expr::Column { table: Some(schema.name.clone()), name: c.name.clone() };
        let b = Expr::Column { table: Some(other.to_string()), name: c.name.clone() };
        let eq = Expr::Binary { op: BinOp::Eq, left: Box::new(a.clone()), right: Box::new(b.clone()) };
        let both_null = Expr::Binary {
            op: BinOp::And,
            left: Box::new(Expr::IsNull { expr: Box::new(a), negated: false }),
            right: Box::new(Expr::IsNull { expr: Box::new(b), negated: false }),
        };
        let same = Expr::Binary { op: BinOp::Or, left: Box::new(eq), right: Box::new(both_null) };
        out = Some(and(out, same));
    }
    out.ok_or_else(|| Error::catalog("the table has no columns"))
}

fn one() -> SelectItem {
    SelectItem::Expr { expr: Expr::Literal(Value::Int(1)), alias: None }
}

const FIRST: &str = "__l";

/// Replaces the qualifier `from` by `to` in `e`.
fn requalify_to(e: &mut Expr, from: &str, to: &str) {
    let _ = walk_expr(e, &mut |x| {
        if let Expr::Column { table: Some(t), .. } = x {
            if t.eq_ignore_ascii_case(from) {
                *t = to.to_string();
            }
        }
        Ok(())
    });
}

impl Database {
    /// MySQL's `HANDLER`: a cursor over a table, kept by name until it is closed. Rows come in the order they are kept.
    pub(crate) fn exec_handler(&mut self, h: &crate::sql::ast::HandlerStmt) -> Result<QueryResult> {
        use crate::sql::ast::HandlerAction;
        let key = h.name.to_ascii_lowercase();
        match &h.action {
            HandlerAction::Open { alias } => {
                let table = self.table_checked(&h.name)?.name().to_string();
                let name = alias.clone().unwrap_or_else(|| h.name.clone()).to_ascii_lowercase();
                self.handlers.insert(name, (table, 0));
                Ok(QueryResult::default())
            }
            HandlerAction::Close => {
                self.handlers.remove(&key).ok_or_else(|| Error::catalog(format!("the handler '{}' is not open", h.name)))?;
                Ok(QueryResult::default())
            }
            HandlerAction::Read { direction, filter, limit } => {
                let (table, pos) = self.handlers.get(&key).cloned().ok_or_else(|| Error::catalog(format!("the handler '{}' is not open", h.name)))?;
                // every row, and with it whether the condition holds, so that the position counts the rows of the table
                let mut items = vec![SelectItem::Wildcard];
                if let Some(f) = filter {
                    items.push(SelectItem::Expr { expr: f.clone(), alias: Some("__ok".into()) });
                }
                let select = Select {
                    items,
                    from: Some(FromClause { first: TableFactor::Named { name: table.clone(), alias: None, only: false }, joins: Vec::new() }),
                    ..Select::default()
                };
                let mut all = self.execute_statement(&Statement::Select(select))?;
                let n = all.rows.len();
                let width = all.columns.len() - usize::from(filter.is_some());
                let ok = |i: usize| filter.is_none() || matches!(all.rows[i].last(), Some(Value::Bool(true)) | Some(Value::Int(1)));
                // `pos` is one past the row read last; the rows to read go forward from it, or backward
                let indexes: Vec<usize> = match *direction {
                    "first" => (0..n).filter(|&i| ok(i)).take(*limit).collect(),
                    "next" => (pos..n).filter(|&i| ok(i)).take(*limit).collect(),
                    "last" => (0..n).rev().filter(|&i| ok(i)).take(*limit).collect(),
                    _ => (0..pos.saturating_sub(1).min(n)).rev().filter(|&i| ok(i)).take(*limit).collect(),
                };
                let rows: Vec<_> = indexes.iter().map(|&i| all.rows[i][..width].to_vec()).collect();
                all.columns.truncate(width);
                if let Some(last) = indexes.last() {
                    if let Some(e) = self.handlers.get_mut(&key) {
                        e.1 = last + 1;
                    }
                }
                Ok(QueryResult { columns: all.columns, rows, affected: 0 })
            }
        }
    }

    /// `DELETE FROM t WHERE f ORDER BY o LIMIT n` and the `UPDATE` likewise: the rows to change are those that equal one of
    /// the first `n` rows (`SELECT * FROM t AS __m WHERE f ORDER BY o LIMIT n`) in every column.
    pub(crate) fn exec_limited(
        &mut self,
        table: &str,
        alias: Option<&str>,
        assignments: Option<&Vec<(String, Expr)>>,
        filter: Option<&Expr>,
        order_by: &[crate::sql::ast::OrderItem],
        limit: Option<usize>,
    ) -> Result<QueryResult> {
        let base = self.table_checked(table)?.name().to_string();
        let shown = alias.unwrap_or(table).to_string();
        let mut filter2 = filter.cloned();
        let mut order2 = order_by.to_vec();
        for e in filter2.iter_mut().chain(order2.iter_mut().map(|o| &mut o.expr)) {
            requalify_to(e, &shown, COPY);
            requalify_to(e, &base, COPY);
        }
        let inner = Select {
            items: vec![SelectItem::Wildcard],
            from: Some(FromClause { first: TableFactor::Named { name: base.clone(), alias: Some(COPY.to_string()), only: false }, joins: Vec::new() }),
            filter: filter2,
            order_by: order2,
            limit,
            ..Select::default()
        };
        let derived = FromClause {
            first: TableFactor::Derived { query: Box::new(inner), alias: FIRST.to_string(), columns: Vec::new(), lateral: false },
            joins: Vec::new(),
        };
        let pick = Select { items: vec![one()], from: Some(derived), filter: Some(same_row_as(self, &base, FIRST)?), ..Select::default() };
        let filter = Some(Expr::Exists { query: Box::new(pick), negated: false });
        let statement = match assignments {
            None => Statement::Delete { table: base, filter },
            Some(sets) => {
                let mut sets = sets.clone();
                for (_, e) in sets.iter_mut() {
                    requalify_to(e, &shown, &base);
                }
                Statement::Update { table: base, assignments: sets, filter }
            }
        };
        self.execute_statement(&statement)
    }

    pub(crate) fn exec_delete_multi(&mut self, target: &str, from: &FromClause, filter: Option<&Expr>) -> Result<QueryResult> {
        let mut from2 = from.clone();
        let mut filter2 = filter.cloned();
        let base = {
            let mut refs: Vec<&mut Expr> = filter2.iter_mut().collect();
            rename_target(&mut from2, target, &mut refs)?
        };
        let base = self.table_checked(&base)?.name().to_string();
        let cond = and(filter2, same_row(self, &base)?);
        let sub = Select { items: vec![one()], from: Some(from2), filter: Some(cond), ..Select::default() };
        let delete = Statement::Delete { table: base, filter: Some(Expr::Exists { query: Box::new(sub), negated: false }) };
        self.execute_statement(&delete)
    }

    pub(crate) fn exec_update_multi(&mut self, from: &FromClause, assignments: &[(Option<String>, String, Expr)], filter: Option<&Expr>) -> Result<QueryResult> {
        // the table that is changed: the one the assignments name
        let mut qualifiers: Vec<String> = assignments.iter().filter_map(|(q, _, _)| q.clone()).collect();
        qualifiers.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        let target = match qualifiers.as_slice() {
            [one] => one.clone(),
            [] => factor_names(&from.first).map(|(_, alias)| alias).ok_or_else(|| Error::runtime("UPDATE needs a table to change"))?,
            _ => return Err(Error::runtime("UPDATE of several tables at once is not supported")),
        };
        let mut from2 = from.clone();
        let mut filter2 = filter.cloned();
        let mut values: Vec<Expr> = assignments.iter().map(|(_, _, e)| e.clone()).collect();
        let base = {
            let mut refs: Vec<&mut Expr> = filter2.iter_mut().chain(values.iter_mut()).collect();
            rename_target(&mut from2, &target, &mut refs)?
        };
        let base = self.table_checked(&base)?.name().to_string();
        let corr = same_row(self, &base)?;
        let cond = and(filter2, corr);
        let sets: Vec<(String, Expr)> = assignments
            .iter()
            .zip(values)
            .map(|((_, column, _), value)| {
                let sub = Select {
                    items: vec![SelectItem::Expr { expr: value, alias: None }],
                    from: Some(from2.clone()),
                    filter: Some(cond.clone()),
                    limit: Some(1),
                    ..Select::default()
                };
                (column.clone(), Expr::Subquery(Box::new(sub)))
            })
            .collect();
        let exists = Select { items: vec![one()], from: Some(from2), filter: Some(cond), ..Select::default() };
        let update = Statement::Update { table: base, assignments: sets, filter: Some(Expr::Exists { query: Box::new(exists), negated: false }) };
        self.execute_statement(&update)
    }
}
