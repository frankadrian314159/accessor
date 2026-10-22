//! Name resolution: turns parsed [`Expr`]s into position-resolved [`BExpr`]s.

use std::collections::HashSet;

use super::agg::{AggFn, AggSpec};
use super::window::{WinFn, WinSpecB, MASK_COL, WIN_BASE};
use crate::database::Database;
use crate::error::{Error, Result};
use crate::expr::{BExpr, LikePattern, ScalarFn};
use crate::sql::ast::{Expr, Select, SelectItem};
use crate::sql::walk::each_child;
use crate::value::Value;

#[derive(Clone, Debug)]
pub struct ScopeCol {
    /// Table alias (or name) the column came from.
    pub table: String,
    pub name: String,
    /// A column merged away by `USING`/`NATURAL JOIN`: reachable only by
    /// qualified name, and left out of `*`.
    pub hidden: bool,
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
            if c.hidden && table.is_none() {
                continue;
            }
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

/// How the rows of a correlated sub-query become a value.
pub enum CorrKind {
    Scalar,
    Exists { negated: bool },
    In { expr: BExpr, negated: bool },
}

/// A sub-query that reads columns of the query around it. It is run once per
/// distinct combination of those columns, and its value is appended to each
/// row as a hidden column, where the bound expression picks it up.
pub struct CorrSpec {
    /// The query with each outer column replaced by a parameter.
    pub query: Select,
    /// Parameter name and the outer row position it stands for.
    pub outer: Vec<(String, usize)>,
    pub kind: CorrKind,
}

pub struct Binder<'a> {
    pub db: &'a Database,
    pub scope: &'a Scope,
    pub group: Option<GroupCtx>,
    pub depth: u32,
    /// Whether correlated sub-queries may be bound here (only in the main
    /// expressions of a SELECT); they collect in `corr`.
    pub allow_corr: bool,
    pub corr: Vec<CorrSpec>,
    /// Whether window functions may be bound here (select list and ORDER BY).
    pub allow_win: bool,
    pub wins: Vec<WinSpecB>,
}

/// Finds the outer columns a sub-query uses.
struct Correlator<'a> {
    db: &'a Database,
    outer: &'a Scope,
    depth: u32,
    /// Per enclosing SELECT inside the sub-query: its sources aliases and columns.
    frames: Vec<Vec<(String, Vec<String>)>>,
    found: Vec<(String, usize)>,
}

impl Correlator<'_> {
    fn select(&mut self, s: &mut Select) -> Result<()> {
        let mut frame = Vec::new();
        if let Some(from) = &s.from {
            for f in std::iter::once(&from.first).chain(from.joins.iter().map(|j| &j.factor)) {
                let src = self.db.resolve_factor(f, self.depth)?;
                frame.push((src.alias, src.names));
            }
        }
        self.frames.push(frame);
        let aliases: Vec<String> = s
            .items
            .iter()
            .filter_map(|i| match i {
                SelectItem::Expr { alias: Some(a), .. } => Some(a.clone()),
                _ => None,
            })
            .collect();
        let r = self.select_exprs(s, &aliases);
        self.frames.pop();
        r?;
        for u in &mut s.unions {
            self.select(&mut u.select)?;
        }
        Ok(())
    }

    fn select_exprs(&mut self, s: &mut Select, aliases: &[String]) -> Result<()> {
        for item in &mut s.items {
            if let SelectItem::Expr { expr, .. } = item {
                self.expr(expr)?;
            }
        }
        if let Some(from) = &mut s.from {
            for j in &mut from.joins {
                if let Some(on) = &mut j.on {
                    self.expr(on)?;
                }
            }
        }
        for e in s.filter.iter_mut().chain(s.having.iter_mut()).chain(s.group_by.iter_mut()) {
            self.expr(e)?;
        }
        for o in &mut s.order_by {
            // An output alias is not a column.
            if matches!(&o.expr, Expr::Column { table: None, name } if aliases.iter().any(|a| a.eq_ignore_ascii_case(name))) {
                continue;
            }
            self.expr(&mut o.expr)?;
        }
        Ok(())
    }

    fn expr(&mut self, e: &mut Expr) -> Result<()> {
        match e {
            Expr::Column { table, name } => {
                let local = self.frames.iter().any(|fr| {
                    fr.iter().any(|(alias, cols)| {
                        table.as_deref().is_none_or(|t| alias.eq_ignore_ascii_case(t))
                            && cols.iter().any(|c| c.eq_ignore_ascii_case(name))
                    })
                });
                if !local {
                    if let Ok(pos) = self.outer.resolve(table.as_deref(), name) {
                        let param = match self.found.iter().find(|(_, p)| *p == pos) {
                            Some((n, _)) => n.clone(),
                            None => {
                                let n = format!("__corr{}", self.found.len());
                                self.found.push((n.clone(), pos));
                                n
                            }
                        };
                        *e = Expr::Param(param);
                    }
                }
                Ok(())
            }
            Expr::Subquery(q) | Expr::Exists { query: q, .. } => self.select(q),
            Expr::InSelect { expr, query, .. } => {
                self.expr(expr)?;
                self.select(query)
            }
            other => each_child(other, &mut |c| self.expr(c)),
        }
    }
}

pub fn contains_agg(e: &Expr) -> bool {
    match e {
        Expr::Function { name, args, .. } => AggFn::from_name(name).is_some() || args.iter().any(contains_agg),
        Expr::Literal(_) | Expr::Column { .. } | Expr::Param(_) | Expr::Subquery(_) | Expr::Exists { .. } => false,
        // The window function itself is not an aggregate, but aggregates may sit inside its arguments.
        Expr::Window { func, spec } => {
            let args_agg = match &**func {
                Expr::Function { args, .. } => args.iter().any(contains_agg),
                _ => false,
            };
            args_agg
                || spec.partition_by.iter().any(contains_agg)
                || spec.order_by.iter().any(|o| contains_agg(&o.expr))
        }
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
        Binder { db, scope, group: None, depth, allow_corr: false, corr: Vec::new(), allow_win: false, wins: Vec::new() }
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

    /// If `q` reads columns of the enclosing query, returns it with those
    /// columns turned into parameters, and the outer positions they come from.
    fn correlate(&self, q: &Select) -> Result<Option<(Select, Vec<(String, usize)>)>> {
        let mut rewritten = q.clone();
        let mut c = Correlator { db: self.db, outer: self.scope, depth: self.depth + 1, frames: Vec::new(), found: Vec::new() };
        c.select(&mut rewritten)?;
        if c.found.is_empty() {
            return Ok(None);
        }
        if !self.allow_corr || self.group.is_some() {
            return Err(Error::runtime(
                "a sub-query that refers to the outer query is only allowed in the select list, WHERE clause or ORDER BY of a query without GROUP BY",
            ));
        }
        Ok(Some((rewritten, c.found)))
    }

    fn add_corr(&mut self, kind: CorrKind, query: Select, outer: Vec<(String, usize)>) -> BExpr {
        self.corr.push(CorrSpec { query, outer, kind });
        BExpr::Col(self.scope.cols.len() + self.corr.len() - 1)
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
                let (corr_ok, win_ok) = (self.allow_corr, self.allow_win);
                (self.allow_corr, self.allow_win) = (false, false);
                let pre = self.bind(e);
                (self.allow_corr, self.allow_win) = (corr_ok, win_ok);
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
                if let Some((q, outer)) = self.correlate(query)? {
                    let lhs = self.bind(expr)?;
                    return Ok(self.add_corr(CorrKind::In { expr: lhs, negated: *negated }, q, outer));
                }
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
                if let Some((mut q, outer)) = self.correlate(query)? {
                    if q.unions.is_empty() && q.top.is_none() && q.limit.is_none() && q.offset.is_none() {
                        q.limit = Some(1);
                    }
                    return Ok(self.add_corr(CorrKind::Exists { negated: *negated }, q, outer));
                }
                let res = self.sub_select(query)?;
                BExpr::Lit(Value::Bool(!res.rows.is_empty() != *negated))
            }
            Expr::Subquery(query) => {
                if let Some((q, outer)) = self.correlate(query)? {
                    return Ok(self.add_corr(CorrKind::Scalar, q, outer));
                }
                let mut res = self.sub_select(query)?;
                if res.columns.len() != 1 {
                    return Err(Error::runtime("scalar subquery must return exactly one column"));
                }
                if res.rows.len() > 1 {
                    return Err(Error::runtime("scalar subquery returned more than one row"));
                }
                BExpr::Lit(res.rows.pop().map_or(Value::Null, |mut r| r.swap_remove(0)))
            }
            Expr::Window { func, spec } => return self.bind_window(func, spec),
            Expr::Function { name, args, .. } if name.eq_ignore_ascii_case("GROUPING") && self.group.is_some() => {
                let saved = self.group.take();
                let bound: Result<Vec<BExpr>> = args.iter().map(|a| self.bind(a)).collect();
                self.group = saved;
                let g = self.group.as_ref().expect("group mode");
                let mut call = vec![BExpr::Col(MASK_COL)];
                for b in bound? {
                    let i = g
                        .exprs
                        .iter()
                        .position(|x| *x == b)
                        .ok_or_else(|| Error::runtime("the argument of GROUPING must be a GROUP BY expression"))?;
                    call.push(BExpr::Lit(Value::Int(i as i64)));
                }
                BExpr::Func(ScalarFn::GroupingBit, call)
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

    fn bind_window(&mut self, func: &Expr, spec: &crate::sql::ast::WindowSpec) -> Result<BExpr> {
        if !self.allow_win {
            return Err(Error::runtime("window functions are only allowed in the select list and ORDER BY"));
        }
        let Expr::Function { name, args, distinct, star } = func else {
            return Err(Error::runtime("OVER must follow a function call"));
        };
        let wf = WinFn::from_name(name)
            .ok_or_else(|| Error::catalog(format!("'{name}' cannot be used as a window function")))?;
        let (min, max) = wf.arity();
        let given = if *star { 0 } else { args.len() };
        if given < min || given > max || (*star && wf != WinFn::Agg(AggFn::Count)) {
            return Err(Error::runtime(format!("wrong number of arguments for window function {name}")));
        }
        // Arguments and keys are evaluated on the rows the windows run over.
        let saved = self.allow_win;
        self.allow_win = false;
        let result = (|| {
            let mut bound = args.iter().map(|a| self.bind(a)).collect::<Result<Vec<_>>>()?;
            let mut sep = None;
            if wf == WinFn::Agg(AggFn::StringAgg) && bound.len() == 2 {
                match bound.pop() {
                    Some(BExpr::Lit(Value::Text(s))) => sep = Some(s.to_string()),
                    _ => return Err(Error::runtime("the separator of STRING_AGG must be a text constant")),
                }
            }
            let partition = spec.partition_by.iter().map(|e| self.bind(e)).collect::<Result<Vec<_>>>()?;
            let order = spec
                .order_by
                .iter()
                .map(|o| Ok((self.bind(&o.expr)?, o.desc)))
                .collect::<Result<Vec<_>>>()?;
            Ok(WinSpecB {
                func: wf,
                args: bound,
                star: *star,
                distinct: *distinct,
                sep,
                partition,
                order,
                frame: spec.frame,
            })
        })();
        self.allow_win = saved;
        self.wins.push(result?);
        Ok(BExpr::Col(WIN_BASE + self.wins.len() - 1))
    }

    fn bind_agg(&mut self, func: AggFn, args: &[Expr], distinct: bool, star: bool) -> Result<BExpr> {
        let mut sep_text: Option<String> = None;
        let arg = if star {
            if func != AggFn::Count {
                return Err(Error::runtime("only COUNT accepts *"));
            }
            None
        } else {
            let ok = if func == AggFn::StringAgg { (1..=2).contains(&args.len()) } else { args.len() == 1 };
            if !ok {
                return Err(Error::runtime(format!("{} takes exactly one argument", func.label())));
            }
            // Arguments are evaluated per input row, outside the group layout.
            let saved = self.group.take();
            let (corr_ok, win_ok) = (self.allow_corr, self.allow_win);
            (self.allow_corr, self.allow_win) = (false, false);
            let r = self.bind(&args[0]);
            let sep = args.get(1).map(|s| self.bind(s));
            (self.allow_corr, self.allow_win) = (corr_ok, win_ok);
            self.group = saved;
            if let Some(sep) = sep {
                match sep? {
                    BExpr::Lit(Value::Text(s)) => sep_text = Some(s.to_string()),
                    _ => return Err(Error::runtime("the separator of STRING_AGG must be a text constant")),
                }
            }
            Some(r?)
        };
        let spec = AggSpec { func, arg, distinct, sep: sep_text };
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
