//! Rewriting passes over the syntax tree.
//!
//! Query parameters (`@name`) and trigger row references (`NEW.col`,
//! `OLD.col`) are replaced with literals before a statement is planned, so
//! the executor never needs to know about either.

use std::collections::HashMap;

use super::ast::*;
use crate::error::{Error, Result};
use crate::schema::TableSchema;
use crate::value::{Row, Value};

type Visitor<'a> = dyn FnMut(&mut Expr) -> Result<()> + 'a;

/// Calls `f` on `e` and then on every expression nested inside it
/// (including inside sub-queries). `f` may replace the node.
pub fn walk_expr(e: &mut Expr, f: &mut Visitor) -> Result<()> {
    f(e)?;
    match e {
        Expr::Literal(_) | Expr::Column { .. } | Expr::Param(_) => {}
        Expr::Neg(a) | Expr::Not(a) => walk_expr(a, f)?,
        Expr::Binary { left, right, .. } => {
            walk_expr(left, f)?;
            walk_expr(right, f)?;
        }
        Expr::IsNull { expr, .. } => walk_expr(expr, f)?,
        Expr::Like { expr, pattern, .. } => {
            walk_expr(expr, f)?;
            walk_expr(pattern, f)?;
        }
        Expr::Between { expr, low, high, .. } => {
            walk_expr(expr, f)?;
            walk_expr(low, f)?;
            walk_expr(high, f)?;
        }
        Expr::InList { expr, list, .. } => {
            walk_expr(expr, f)?;
            list.iter_mut().try_for_each(|x| walk_expr(x, f))?;
        }
        Expr::InSelect { expr, query, .. } => {
            walk_expr(expr, f)?;
            walk_select(query, f)?;
        }
        Expr::Exists { query, .. } | Expr::Subquery(query) => walk_select(query, f)?,
        Expr::Function { args, .. } => args.iter_mut().try_for_each(|x| walk_expr(x, f))?,
        Expr::Case { whens, else_expr } => {
            for (c, r) in whens {
                walk_expr(c, f)?;
                walk_expr(r, f)?;
            }
            if let Some(x) = else_expr {
                walk_expr(x, f)?;
            }
        }
    }
    Ok(())
}

fn walk_factor(t: &mut TableFactor, f: &mut Visitor) -> Result<()> {
    match t {
        TableFactor::Named { .. } => Ok(()),
        TableFactor::Derived { query, .. } => walk_select(query, f),
    }
}

pub fn walk_select(s: &mut Select, f: &mut Visitor) -> Result<()> {
    for item in &mut s.items {
        if let SelectItem::Expr { expr, .. } = item {
            walk_expr(expr, f)?;
        }
    }
    if let Some(from) = &mut s.from {
        walk_factor(&mut from.first, f)?;
        for j in &mut from.joins {
            walk_factor(&mut j.factor, f)?;
            if let Some(on) = &mut j.on {
                walk_expr(on, f)?;
            }
        }
    }
    for e in s.filter.iter_mut().chain(s.having.iter_mut()) {
        walk_expr(e, f)?;
    }
    s.group_by.iter_mut().try_for_each(|e| walk_expr(e, f))?;
    s.order_by.iter_mut().try_for_each(|o| walk_expr(&mut o.expr, f))
}

pub fn walk_statement(st: &mut Statement, f: &mut Visitor) -> Result<()> {
    match st {
        Statement::Select(s) => walk_select(s, f),
        Statement::Insert { source, .. } => match source {
            InsertSource::Values(rows) => rows.iter_mut().flatten().try_for_each(|e| walk_expr(e, f)),
            InsertSource::Select(s) => walk_select(s, f),
        },
        Statement::Update { assignments, filter, .. } => {
            assignments.iter_mut().try_for_each(|(_, e)| walk_expr(e, f))?;
            filter.iter_mut().try_for_each(|e| walk_expr(e, f))
        }
        Statement::Delete { filter, .. } => filter.iter_mut().try_for_each(|e| walk_expr(e, f)),
        Statement::Exec { args, .. } => args.iter_mut().try_for_each(|(_, e)| walk_expr(e, f)),
        Statement::Raise(e) => walk_expr(e, f),
        _ => Ok(()),
    }
}

/// Replaces every `@name` using `values` (keys are lowercase).
pub fn bind_params(st: &mut Statement, values: &HashMap<String, Value>) -> Result<()> {
    walk_statement(st, &mut |e| bind_param(e, values))
}

pub fn bind_params_select(s: &mut Select, values: &HashMap<String, Value>) -> Result<()> {
    walk_select(s, &mut |e| bind_param(e, values))
}

fn bind_param(e: &mut Expr, values: &HashMap<String, Value>) -> Result<()> {
    if let Expr::Param(name) = e {
        let v = values.get(&name.to_ascii_lowercase()).ok_or_else(|| {
            Error::runtime(format!("the parameter @{name} has no value; run the query with EXEC and supply one"))
        })?;
        *e = Expr::Literal(v.clone());
    }
    Ok(())
}

/// The rows a trigger body can see as `OLD` and `NEW`.
pub struct RowContext<'a> {
    pub schema: &'a TableSchema,
    pub old: Option<&'a Row>,
    pub new: Option<&'a Row>,
}

impl RowContext<'_> {
    fn resolve(&self, e: &mut Expr) -> Result<()> {
        let Expr::Column { table: Some(t), name } = e else { return Ok(()) };
        let (which, row) = if t.eq_ignore_ascii_case("new") {
            ("NEW", self.new)
        } else if t.eq_ignore_ascii_case("old") {
            ("OLD", self.old)
        } else {
            return Ok(());
        };
        let row = row.ok_or_else(|| Error::runtime(format!("{which} is not available in this kind of trigger")))?;
        let pos = self.schema.column_index(name).ok_or_else(|| {
            Error::catalog(format!("the field '{name}' does not exist in table '{}'", self.schema.name))
        })?;
        *e = Expr::Literal(row[pos].clone());
        Ok(())
    }

    pub fn bind_statement(&self, st: &mut Statement) -> Result<()> {
        walk_statement(st, &mut |e| self.resolve(e))
    }

    pub fn bind_expr(&self, e: &mut Expr) -> Result<()> {
        walk_expr(e, &mut |x| self.resolve(x))
    }
}
