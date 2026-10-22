//! Name resolution: turns parsed [`Expr`]s into position-resolved [`BExpr`]s.

use std::collections::HashSet;

use super::agg::{AggFn, AggSpec};
use crate::database::Database;
use crate::error::{Error, Result};
use crate::expr::{BExpr, LikePattern, ScalarFn};
use crate::sql::ast::{Expr, Select};
use crate::value::Value;

#[derive(Clone, Debug)]
pub struct ScopeCol {
    /// Table alias (or name) the column came from.
    pub table: String,
    pub name: String,
}

/// The columns visible to an expression, in row order.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    pub cols: Vec<ScopeCol>,
}

impl Scope {
    pub fn resolve(&self, table: Option<&str>, name: &str) -> Result<usize> {
        let mut found = None;
        for (i, c) in self.cols.iter().enumerate() {
            if c.name.eq_ignore_ascii_case(name) && table.is_none_or(|t| c.table.eq_ignore_ascii_case(t)) {
                if found.is_some() {
                    return Err(Error::catalog(format!("column '{name}' is ambiguous; qualify it with a table name")));
                }
                found = Some(i);
            }
        }
        found.ok_or_else(|| match table {
            Some(t) => Error::catalog(format!("column '{t}.{name}' was not found")),
            None => Error::catalog(format!("column '{name}' was not found")),
        })
    }

    pub fn prefix(&self, n: usize) -> Scope {
        Scope { cols: self.cols[..n].to_vec() }
    }
}

pub struct GroupCtx {
    pub exprs: Vec<BExpr>,
    pub aggs: Vec<AggSpec>,
}

pub struct Binder<'a> {
    pub db: &'a Database,
    pub scope: &'a Scope,
    pub group: Option<GroupCtx>,
    pub depth: u32,
}

pub fn contains_agg(e: &Expr) -> bool {
    match e {
        Expr::Function { name, args, .. } => AggFn::from_name(name).is_some() || args.iter().any(contains_agg),
        Expr::Literal(_) | Expr::Column { .. } | Expr::Param(_) | Expr::Subquery(_) | Expr::Exists { .. } => false,
        Expr::Neg(a) | Expr::Not(a) => contains_agg(a),
        Expr::Binary { left, right, .. } => contains_agg(left) || contains_agg(right),
        Expr::IsNull { expr, .. } => contains_agg(expr),
        Expr::Like { expr, pattern, .. } => contains_agg(expr) || contains_agg(pattern),
        Expr::Between { expr, low, high, .. } => contains_agg(expr) || contains_agg(low) || contains_agg(high),
        Expr::InList { expr, list, .. } => contains_agg(expr) || list.iter().any(contains_agg),
        Expr::InSelect { expr, .. } => contains_agg(expr),
        Expr::Case { whens, else_expr } => {
            whens.iter().any(|(c, r)| contains_agg(c) || contains_agg(r))
                || else_expr.as_ref().is_some_and(|e| contains_agg(e))
        }
    }
}

impl<'a> Binder<'a> {
    pub fn new(db: &'a Database, scope: &'a Scope, depth: u32) -> Self {
        Binder { db, scope, group: None, depth }
    }

    pub fn bind(&mut self, e: &Expr) -> Result<BExpr> {
        let b = self.bind_inner(e)?;
        // Fold constant sub-trees (errors such as 1/0 are deferred to run time).
        if !matches!(b, BExpr::Lit(_)) && b.is_constant() {
            if let Ok(v) = b.eval(&[]) {
                return Ok(BExpr::Lit(v.into_owned()));
            }
        }
        Ok(b)
    }

    fn boxed(&mut self, e: &Expr) -> Result<Box<BExpr>> {
        Ok(Box::new(self.bind(e)?))
    }

    fn sub_select(&self, q: &Select) -> Result<crate::exec::QueryResult> {
        self.db.run_select_depth(q, self.depth + 1)
    }

    fn bind_inner(&mut self, e: &Expr) -> Result<BExpr> {
        if self.group.is_some() {
            if let Expr::Function { name, args, distinct, star } = e {
                if let Some(func) = AggFn::from_name(name) {
                    return self.bind_agg(func, args, *distinct, *star);
                }
            }
            // An expression identical to a GROUP BY expression is read from
            // the group key instead of being recomputed.
            if !matches!(e, Expr::Literal(_)) && !contains_agg(e) {
                let saved = self.group.take();
                let pre = self.bind(e);
                self.group = saved;
                if let Ok(pre) = pre {
                    if let Some(i) = self.group.as_ref().and_then(|g| g.exprs.iter().position(|x| *x == pre)) {
                        return Ok(BExpr::Col(i));
                    }
                }
            }
        } else if let Expr::Function { name, .. } = e {
            if AggFn::from_name(name).is_some() {
                return Err(Error::runtime(format!("aggregate function {name} is not allowed here")));
            }
        }

        Ok(match e {
            Expr::Literal(v) => BExpr::Lit(v.clone()),
            Expr::Param(name) => {
                return Err(Error::runtime(format!(
                    "the parameter @{name} has no value; run the query with EXEC and supply one"
                )))
            }
            Expr::Column { table, name } => {
                if self.group.is_some() {
                    return Err(Error::runtime(format!(
                        "'{name}' must appear in the GROUP BY clause or be used in an aggregate function"
                    )));
                }
                BExpr::Col(self.scope.resolve(table.as_deref(), name)?)
            }
            Expr::Neg(a) => BExpr::Neg(self.boxed(a)?),
            Expr::Not(a) => BExpr::Not(self.boxed(a)?),
            Expr::Binary { op, left, right } => BExpr::Bin(*op, self.boxed(left)?, self.boxed(right)?),
            Expr::IsNull { expr, negated } => BExpr::IsNull(self.boxed(expr)?, *negated),
            Expr::Like { expr, pattern, negated } => {
                let pattern = self.boxed(pattern)?;
                let compiled = match &*pattern {
                    BExpr::Lit(Value::Text(s)) => Some(LikePattern::new(s)),
                    _ => None,
                };
                BExpr::Like { expr: self.boxed(expr)?, pattern, compiled, negated: *negated }
            }
            Expr::Between { expr, low, high, negated } => BExpr::Between {
                expr: self.boxed(expr)?,
                low: self.boxed(low)?,
                high: self.boxed(high)?,
                negated: *negated,
            },
            Expr::InList { expr, list, negated } => {
                let bound = list.iter().map(|x| self.bind(x)).collect::<Result<Vec<_>>>()?;
                let expr = self.boxed(expr)?;
                if bound.len() >= 8 && bound.iter().all(|b| matches!(b, BExpr::Lit(_))) {
                    let mut set = HashSet::new();
                    let mut has_null = false;
                    for b in bound {
                        if let BExpr::Lit(v) = b {
                            if v.is_null() {
                                has_null = true;
                            } else {
                                set.insert(v);
                            }
                        }
                    }
                    BExpr::InSet { expr, set, has_null, negated: *negated }
                } else {
                    BExpr::InList { expr, list: bound, negated: *negated }
                }
            }
            Expr::InSelect { expr, query, negated } => {
                let res = self.sub_select(query)?;
                if res.columns.len() != 1 {
                    return Err(Error::runtime("subquery in IN must return exactly one column"));
                }
                let mut set = HashSet::new();
                let mut has_null = false;
                for mut r in res.rows {
                    let v = r.swap_remove(0);
                    if v.is_null() {
                        has_null = true;
                    } else {
                        set.insert(v);
                    }
                }
                BExpr::InSet { expr: self.boxed(expr)?, set, has_null, negated: *negated }
            }
            Expr::Exists { query, negated } => {
                let res = self.sub_select(query)?;
                BExpr::Lit(Value::Bool(!res.rows.is_empty() != *negated))
            }
            Expr::Subquery(query) => {
                let mut res = self.sub_select(query)?;
                if res.columns.len() != 1 {
                    return Err(Error::runtime("scalar subquery must return exactly one column"));
                }
                if res.rows.len() > 1 {
                    return Err(Error::runtime("scalar subquery returned more than one row"));
                }
                BExpr::Lit(res.rows.pop().map_or(Value::Null, |mut r| r.swap_remove(0)))
            }
            Expr::Function { name, args, distinct, star } => {
                if *distinct || *star {
                    return Err(Error::runtime(format!("{name} does not accept DISTINCT or *")));
                }
                let (f, min, max) =
                    ScalarFn::lookup(name).ok_or_else(|| Error::catalog(format!("unknown function '{name}'")))?;
                if args.len() < min || args.len() > max {
                    return Err(Error::runtime(format!("wrong number of arguments for {name}")));
                }
                BExpr::Func(f, args.iter().map(|a| self.bind(a)).collect::<Result<_>>()?)
            }
            Expr::Case { whens, else_expr } => BExpr::Case {
                whens: whens
                    .iter()
                    .map(|(c, r)| Ok((self.bind(c)?, self.bind(r)?)))
                    .collect::<Result<_>>()?,
                else_expr: match else_expr {
                    Some(e) => Some(self.boxed(e)?),
                    None => None,
                },
            },
        })
    }

    fn bind_agg(&mut self, func: AggFn, args: &[Expr], distinct: bool, star: bool) -> Result<BExpr> {
        let arg = if star {
            if func != AggFn::Count {
                return Err(Error::runtime("only COUNT accepts *"));
            }
            None
        } else {
            if args.len() != 1 {
                return Err(Error::runtime(format!("{} takes exactly one argument", func.label())));
            }
            // Arguments are evaluated per input row, outside the group layout.
            let saved = self.group.take();
            let r = self.bind(&args[0]);
            self.group = saved;
            Some(r?)
        };
        let spec = AggSpec { func, arg, distinct };
        let g = self.group.as_mut().expect("aggregate bound outside group context");
        let n = g.exprs.len();
        let slot = match g.aggs.iter().position(|a| *a == spec) {
            Some(i) => i,
            None => {
                g.aggs.push(spec);
                g.aggs.len() - 1
            }
        };
        Ok(BExpr::Col(n + slot))
    }
}
