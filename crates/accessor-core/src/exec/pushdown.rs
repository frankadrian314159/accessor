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

/// The `WHERE` of `sel` with the conditions it implies for other columns through the equalities of its joins: with
/// `WHERE a = 2000` and `... LEFT JOIN t ON t.y = a`, rows of `t` with another `y` can never be matched, so `t.y = 2000` is
/// a condition `t` can be read under, as a derived table or a partitioned table. Only equalities of inner and left joins
/// (and of the `WHERE`) are used, and only to carry constants.
pub(crate) fn with_implied(sel: &Select) -> Option<Expr> {
    use crate::sql::ast::JoinKind;
    let filter = sel.filter.clone()?;
    let from = sel.from.as_ref()?;
    let mut all = Vec::new();
    conjuncts(&filter, &mut all);
    let mut equalities: Vec<(Expr, Expr)> = Vec::new();
    let mut ons = Vec::new();
    for j in &from.joins {
        if let (Some(on), JoinKind::Inner | JoinKind::Left) = (&j.on, j.kind) {
            conjuncts(on, &mut ons);
        }
    }
    for c in all.iter().chain(ons.iter()) {
        if let Expr::Binary { op: BinOp::Eq, left, right } = c {
            if matches!(**left, Expr::Column { .. }) && matches!(**right, Expr::Column { .. }) && left != right {
                equalities.push(((**left).clone(), (**right).clone()));
            }
        }
    }
    if equalities.is_empty() {
        return Some(filter);
    }
    let mut known = all.clone();
    for _ in 0..4 {
        let mut found = Vec::new();
        for c in &known {
            let Expr::Binary { op: BinOp::Eq, left, right } = c else { continue };
            let (col, val) = match (&**left, &**right) {
                (col @ Expr::Column { .. }, val @ Expr::Literal(_)) | (val @ Expr::Literal(_), col @ Expr::Column { .. }) => (col, val),
                _ => continue,
            };
            for (a, b) in &equalities {
                let other = if a == col { b } else if b == col { a } else { continue };
                let implied = Expr::Binary { op: BinOp::Eq, left: Box::new(other.clone()), right: Box::new(val.clone()) };
                if !known.contains(&implied) && !found.contains(&implied) {
                    found.push(implied);
                }
            }
        }
        if found.is_empty() {
            break;
        }
        known.extend(found);
    }
    known.into_iter().reduce(|a, b| Expr::Binary { op: BinOp::And, left: Box::new(a), right: Box::new(b) })
}

/// The derived table's query with the conditions of `outer_filter` that can move into it, or `None` if none can.
pub(crate) fn push_into_derived(inner: &Select, alias: &str, outer_filter: &Expr) -> Option<Select> {
    // A grouped query takes conditions on its grouping columns: they keep or drop whole groups, so they can run before
    // the grouping, on the rows. (Not with ROLLUP, CUBE or GROUPING SETS, where the rows of a dropped key still count in the totals.)
    let grouped = !inner.group_by.is_empty();
    if inner.distinct
        || inner.top.is_some()
        || inner.limit.is_some()
        || inner.offset.is_some()
        || !inner.group_sets.is_empty()
        || (inner.having.is_some() && !grouped)
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
        if grouped {
            // only a grouping column can be conditioned: what an aggregate or another expression gives is known after the grouping
            if !plain(expr) || !inner.group_by.contains(expr) {
                continue;
            }
        } else if !plain(expr) {
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
