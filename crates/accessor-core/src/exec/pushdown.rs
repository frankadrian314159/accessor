//! Moving a condition of the outer query into a derived table, and deriving per-table conditions from an `OR`.
//!
//! `SELECT ... FROM (SELECT a AS x, b FROM big1, big2) t WHERE x = 1` is run as if the inner query had `WHERE a = 1`:
//! the condition only keeps rows the outer `WHERE` would throw away, so the inner query builds far fewer.

use std::collections::HashMap;

use crate::sql::ast::{BinOp, Expr, Select, SelectItem};
use crate::sql::walk::walk_expr;

fn conjuncts(e: &Expr, out: &mut Vec<Expr>) {
    match e {
        Expr::Binary { op: BinOp::And, left, right } => {
            conjuncts(left, out);
            conjuncts(right, out);
        }
        other => out.push(other.clone()),
    }
}

/// Is `e` free of aggregates, windows and sub-queries, so that it can be moved under a `WHERE`?
fn plain(e: &Expr) -> bool {
    let mut ok = true;
    let _ = walk_expr(&mut e.clone(), &mut |x| {
        match x {
            Expr::Window { .. } | Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSelect { .. } | Expr::Param(_) => ok = false,
            Expr::Function { name, .. } if crate::exec::agg::AggFn::from_name(name).is_some() => ok = false,
            _ => {}
        }
        Ok(())
    });
    ok
}

/// `c` with each column of the derived table `alias` replaced by what the inner query computes for it; `None` when
/// `c` is not a condition that rejects NULL (comparisons, `BETWEEN`, `IN`) built from such columns and constants.
fn substitute(c: &Expr, alias: &str, map: &HashMap<String, Expr>) -> Option<Expr> {
    let side = |e: &Expr| -> Option<Expr> {
        match e {
            Expr::Literal(_) => Some(e.clone()),
            Expr::Column { table, name } if table.as_ref().is_none_or(|t| t.eq_ignore_ascii_case(alias)) => map.get(&name.to_ascii_lowercase()).cloned(),
            _ => None,
        }
    };
    match c {
        Expr::Binary { op: op @ (BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge), left, right } => {
            let (l, r) = (side(left)?, side(right)?);
            if matches!((&l, &r), (Expr::Literal(_), Expr::Literal(_))) {
                return None;
            }
            Some(Expr::Binary { op: *op, left: Box::new(l), right: Box::new(r) })
        }
        Expr::Between { expr, low, high, negated: false } => {
            Some(Expr::Between { expr: Box::new(side(expr)?), low: Box::new(side(low)?), high: Box::new(side(high)?), negated: false })
        }
        Expr::InList { expr, list, negated: false } => {
            let list: Option<Vec<Expr>> = list.iter().map(|x| side(x)).collect();
            Some(Expr::InList { expr: Box::new(side(expr)?), list: list?, negated: false })
        }
        _ => None,
    }
}

/// The derived table's query with the conditions of `outer_filter` that can move into it, or `None` if none can.
pub(crate) fn push_into_derived(inner: &Select, alias: &str, outer_filter: &Expr) -> Option<Select> {
    if inner.distinct
        || inner.top.is_some()
        || inner.limit.is_some()
        || inner.offset.is_some()
        || !inner.group_by.is_empty()
        || inner.having.is_some()
        || !inner.unions.is_empty()
        || inner.with.is_some()
        || inner.crosstab.is_some()
        || !inner.windows.is_empty()
        || inner.from.is_none()
    {
        return None;
    }
    let mut map: HashMap<String, Expr> = HashMap::new();
    for item in &inner.items {
        let SelectItem::Expr { expr, alias: a } = item else { return None };
        if !plain(expr) {
            return None;
        }
        let name = match (a, expr) {
            (Some(a), _) => a.clone(),
            (None, Expr::Column { name, .. }) => name.clone(),
            _ => continue,
        };
        map.insert(name.to_ascii_lowercase(), expr.clone());
    }
    let mut conj = Vec::new();
    conjuncts(outer_filter, &mut conj);
    let moved: Vec<Expr> = conj.iter().filter_map(|c| substitute(c, alias, &map)).collect();
    if moved.is_empty() {
        return None;
    }
    let mut out = inner.clone();
    for m in moved {
        out.filter = Some(match out.filter.take() {
            Some(f) => Expr::Binary { op: BinOp::And, left: Box::new(f), right: Box::new(m) },
            None => m,
        });
    }
    Some(out)
}
