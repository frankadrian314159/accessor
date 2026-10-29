//! Bound expressions: column references are resolved to row positions and
//! function names to enum variants, so per-row evaluation does no lookups.

pub mod format;
pub(crate) mod extra;
pub mod func;
pub mod like;
pub mod myfuncs;
pub mod pgfuncs;

use std::borrow::Cow;
use std::collections::HashSet;

use crate::error::{Error, Result};
use crate::sql::ast::BinOp;
pub use crate::numeric::arith;
use crate::numeric::negate;
use crate::value::Value;
pub use func::ScalarFn;
pub use like::LikePattern;

#[derive(Clone, Debug, PartialEq)]
pub enum BExpr {
    Lit(Value),
    Col(usize),
    Neg(Box<BExpr>),
    Not(Box<BExpr>),
    Bin(BinOp, Box<BExpr>, Box<BExpr>),
    IsNull(Box<BExpr>, bool),
    Like { expr: Box<BExpr>, pattern: Box<BExpr>, compiled: Option<LikePattern>, negated: bool },
    Between { expr: Box<BExpr>, low: Box<BExpr>, high: Box<BExpr>, negated: bool },
    InList { expr: Box<BExpr>, list: Vec<BExpr>, negated: bool },
    InSet { expr: Box<BExpr>, set: HashSet<Value>, has_null: bool, negated: bool },
    Func(ScalarFn, Vec<BExpr>),
    Case { whens: Vec<(BExpr, BExpr)>, else_expr: Option<Box<BExpr>> },
}

/// Three-valued truth of a value: `None` is SQL unknown.
pub(crate) fn tri(v: &Value) -> Result<Option<bool>> {
    Ok(match v {
        Value::Null => None,
        Value::Bool(b) => Some(*b),
        n if n.is_numeric() => Some(!n.is_zero()),
        other => return Err(Error::type_err(format!("'{other}' cannot be used as a Yes/No value"))),
    })
}

fn from_tri(t: Option<bool>) -> Value {
    t.map_or(Value::Null, Value::Bool)
}

fn negate_tri(t: Option<bool>, negated: bool) -> Option<bool> {
    t.map(|b| b != negated)
}

fn cmp_holds(op: BinOp, ord: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::*;
    match op {
        BinOp::Eq => ord == Equal,
        BinOp::Ne => ord != Equal,
        BinOp::Lt => ord == Less,
        BinOp::Le => ord != Greater,
        BinOp::Gt => ord == Greater,
        BinOp::Ge => ord != Less,
        _ => unreachable!("not a comparison"),
    }
}

impl BExpr {
    pub fn lit(v: Value) -> BExpr {
        BExpr::Lit(v)
    }

    /// Evaluates against `row`, borrowing from the row or the expression
    /// itself whenever no new value has to be computed.
    pub fn eval<'a>(&'a self, row: &'a [Value]) -> Result<Cow<'a, Value>> {
        Ok(match self {
            BExpr::Lit(v) => Cow::Borrowed(v),
            BExpr::Col(i) => Cow::Borrowed(&row[*i]),
            BExpr::Neg(e) => Cow::Owned(negate(&*e.eval(row)?)?),
            BExpr::Not(e) => Cow::Owned(from_tri(e.eval_tri(row)?.map(|b| !b))),
            BExpr::Bin(op, a, b) => match op {
                BinOp::And => {
                    let l = a.eval_tri(row)?;
                    if l == Some(false) {
                        Cow::Owned(Value::Bool(false))
                    } else {
                        let r = b.eval_tri(row)?;
                        Cow::Owned(match (l, r) {
                            (_, Some(false)) => Value::Bool(false),
                            (Some(true), Some(true)) => Value::Bool(true),
                            _ => Value::Null,
                        })
                    }
                }
                BinOp::Or => {
                    let l = a.eval_tri(row)?;
                    if l == Some(true) {
                        Cow::Owned(Value::Bool(true))
                    } else {
                        let r = b.eval_tri(row)?;
                        Cow::Owned(match (l, r) {
                            (_, Some(true)) => Value::Bool(true),
                            (Some(false), Some(false)) => Value::Bool(false),
                            _ => Value::Null,
                        })
                    }
                }
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    let (x, y) = (a.eval(row)?, b.eval(row)?);
                    Cow::Owned(from_tri(x.sql_cmp(&*y).map(|o| cmp_holds(*op, o))))
                }
                BinOp::Concat => {
                    let (x, y) = (a.eval(row)?, b.eval(row)?);
                    let mut s = String::new();
                    for v in [&x, &y] {
                        match &**v {
                            Value::Text(t) => s.push_str(t),
                            other => {
                                use std::fmt::Write;
                                let _ = write!(s, "{other}");
                            }
                        }
                    }
                    Cow::Owned(Value::text(&s))
                }
                _ => {
                    let (x, y) = (a.eval(row)?, b.eval(row)?);
                    Cow::Owned(arith(*op, &*x, &*y)?)
                }
            },
            BExpr::IsNull(e, negated) => Cow::Owned(Value::Bool(e.eval(row)?.is_null() != *negated)),
            BExpr::Like { expr, pattern, compiled, negated } => {
                let v = expr.eval(row)?;
                if v.is_null() {
                    return Ok(Cow::Owned(Value::Null));
                }
                let hit = match compiled {
                    Some(p) => match &*v {
                        Value::Text(s) => p.matches(s),
                        other => p.matches(&other.to_string()),
                    },
                    None => {
                        let pv = pattern.eval(row)?;
                        if pv.is_null() {
                            return Ok(Cow::Owned(Value::Null));
                        }
                        LikePattern::new(&pv.to_string()).matches(&v.to_string())
                    }
                };
                Cow::Owned(Value::Bool(hit != *negated))
            }
            BExpr::Between { expr, low, high, negated } => {
                let v = expr.eval(row)?;
                let lo = v.sql_cmp(&*low.eval(row)?).map(|o| o.is_ge());
                let hi = v.sql_cmp(&*high.eval(row)?).map(|o| o.is_le());
                let t = match (lo, hi) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                };
                Cow::Owned(from_tri(negate_tri(t, *negated)))
            }
            BExpr::InList { expr, list, negated } => {
                let v = expr.eval(row)?;
                if v.is_null() {
                    return Ok(Cow::Owned(Value::Null));
                }
                let mut unknown = false;
                let mut found = false;
                for item in list {
                    match v.sql_cmp(&*item.eval(row)?) {
                        Some(std::cmp::Ordering::Equal) => {
                            found = true;
                            break;
                        }
                        Some(_) => {}
                        None => unknown = true,
                    }
                }
                let t = if found {
                    Some(true)
                } else if unknown {
                    None
                } else {
                    Some(false)
                };
                Cow::Owned(from_tri(negate_tri(t, *negated)))
            }
            BExpr::InSet { expr, set, has_null, negated } => {
                let v = expr.eval(row)?;
                if v.is_null() {
                    // Among nothing, a NULL is not found: `x IN (empty)` is false for every x.
                    if set.is_empty() && !*has_null {
                        return Ok(Cow::Owned(from_tri(negate_tri(Some(false), *negated))));
                    }
                    return Ok(Cow::Owned(Value::Null));
                }
                let t = if set.contains(&*v) {
                    Some(true)
                } else if *has_null {
                    None
                } else {
                    Some(false)
                };
                Cow::Owned(from_tri(negate_tri(t, *negated)))
            }
            BExpr::Func(f, args) => Cow::Owned(func::call(*f, args, row)?),
            BExpr::Case { whens, else_expr } => {
                for (cond, result) in whens {
                    if cond.eval_tri(row)? == Some(true) {
                        return result.eval(row);
                    }
                }
                match else_expr {
                    Some(e) => e.eval(row)?,
                    None => Cow::Owned(Value::Null),
                }
            }
        })
    }

    /// Evaluates to a three-valued truth value.
    pub(crate) fn eval_tri(&self, row: &[Value]) -> Result<Option<bool>> {
        let v = self.eval(row)?;
        tri(&v)
    }

    /// Evaluates as a filter predicate: unknown counts as false.
    #[inline]
    pub fn eval_bool(&self, row: &[Value]) -> Result<bool> {
        Ok(self.eval_tri(row)?.unwrap_or(false))
    }

    /// Calls `f` with every column position the expression reads.
    pub fn visit_cols(&self, f: &mut dyn FnMut(usize)) {
        match self {
            BExpr::Lit(_) => {}
            BExpr::Col(i) => f(*i),
            BExpr::Neg(e) | BExpr::Not(e) | BExpr::IsNull(e, _) => e.visit_cols(f),
            BExpr::Bin(_, a, b) => {
                a.visit_cols(f);
                b.visit_cols(f);
            }
            BExpr::Like { expr, pattern, .. } => {
                expr.visit_cols(f);
                pattern.visit_cols(f);
            }
            BExpr::Between { expr, low, high, .. } => {
                expr.visit_cols(f);
                low.visit_cols(f);
                high.visit_cols(f);
            }
            BExpr::InList { expr, list, .. } => {
                expr.visit_cols(f);
                list.iter().for_each(|e| e.visit_cols(f));
            }
            BExpr::InSet { expr, .. } => expr.visit_cols(f),
            BExpr::Func(_, args) => args.iter().for_each(|e| e.visit_cols(f)),
            BExpr::Case { whens, else_expr } => {
                for (c, r) in whens {
                    c.visit_cols(f);
                    r.visit_cols(f);
                }
                if let Some(e) = else_expr {
                    e.visit_cols(f);
                }
            }
        }
    }

    /// Smallest and largest column position read, or `None` if constant.
    pub fn col_bounds(&self) -> Option<(usize, usize)> {
        let mut b: Option<(usize, usize)> = None;
        self.visit_cols(&mut |i| b = Some(b.map_or((i, i), |(lo, hi)| (lo.min(i), hi.max(i)))));
        b
    }

    /// A copy with every column position rewritten through `f`.
    pub fn map_cols(&self, f: &dyn Fn(usize) -> usize) -> BExpr {
        let bx = |e: &BExpr| Box::new(e.map_cols(f));
        match self {
            BExpr::Lit(v) => BExpr::Lit(v.clone()),
            BExpr::Col(i) => BExpr::Col(f(*i)),
            BExpr::Neg(e) => BExpr::Neg(bx(e)),
            BExpr::Not(e) => BExpr::Not(bx(e)),
            BExpr::IsNull(e, n) => BExpr::IsNull(bx(e), *n),
            BExpr::Bin(op, a, b) => BExpr::Bin(*op, bx(a), bx(b)),
            BExpr::Like { expr, pattern, compiled, negated } => {
                BExpr::Like { expr: bx(expr), pattern: bx(pattern), compiled: compiled.clone(), negated: *negated }
            }
            BExpr::Between { expr, low, high, negated } => {
                BExpr::Between { expr: bx(expr), low: bx(low), high: bx(high), negated: *negated }
            }
            BExpr::InList { expr, list, negated } => BExpr::InList {
                expr: bx(expr),
                list: list.iter().map(|e| e.map_cols(f)).collect(),
                negated: *negated,
            },
            BExpr::InSet { expr, set, has_null, negated } => {
                BExpr::InSet { expr: bx(expr), set: set.clone(), has_null: *has_null, negated: *negated }
            }
            BExpr::Func(func, args) => BExpr::Func(*func, args.iter().map(|e| e.map_cols(f)).collect()),
            BExpr::Case { whens, else_expr } => BExpr::Case {
                whens: whens.iter().map(|(c, r)| (c.map_cols(f), r.map_cols(f))).collect(),
                else_expr: else_expr.as_ref().map(|e| bx(e)),
            },
        }
    }

    /// True if the expression references no columns.
    pub fn is_constant(&self) -> bool {
        match self {
            BExpr::Lit(_) => true,
            BExpr::Col(_) => false,
            BExpr::Neg(e) | BExpr::Not(e) | BExpr::IsNull(e, _) => e.is_constant(),
            BExpr::Bin(_, a, b) => a.is_constant() && b.is_constant(),
            BExpr::Like { expr, pattern, .. } => expr.is_constant() && pattern.is_constant(),
            BExpr::Between { expr, low, high, .. } => expr.is_constant() && low.is_constant() && high.is_constant(),
            BExpr::InList { expr, list, .. } => expr.is_constant() && list.iter().all(BExpr::is_constant),
            BExpr::InSet { expr, .. } => expr.is_constant(),
            BExpr::Func(f, args) => {
                !f.is_volatile() && args.iter().all(BExpr::is_constant)
            }
            BExpr::Case { whens, else_expr } => {
                whens.iter().all(|(c, r)| c.is_constant() && r.is_constant())
                    && else_expr.as_ref().is_none_or(|e| e.is_constant())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bin(op: BinOp, a: Value, b: Value) -> Value {
        BExpr::Bin(op, Box::new(BExpr::Lit(a)), Box::new(BExpr::Lit(b))).eval(&[]).unwrap().into_owned()
    }

    #[test]
    fn evaluation_uses_the_numeric_tower() {
        assert_eq!(bin(BinOp::Add, Value::Int(1), Value::Int(2)), Value::Int(3));
        assert_eq!(bin(BinOp::Add, Value::Int(1), Value::Int(2)).type_label(), "INTEGER");
        assert_eq!(bin(BinOp::Div, Value::Int(1), Value::Int(2)).to_string(), "0");
        assert_eq!(bin(BinOp::ExactDiv, Value::Int(1), Value::Int(2)).to_string(), "0.5");
        assert_eq!(bin(BinOp::Div, Value::Int(1), Value::Double(2.0)).to_string(), "0.5");
        assert!(bin(BinOp::Add, Value::Null, Value::Int(2)).is_null());
        assert!(BExpr::Bin(BinOp::Div, Box::new(BExpr::Lit(1.into())), Box::new(BExpr::Lit(0.into())))
            .eval(&[])
            .unwrap()
            .is_null(), "dividing by zero is NULL");
        let neg = BExpr::Neg(Box::new(BExpr::Lit(Value::Float(2.5)))).eval(&[]).unwrap().into_owned();
        assert_eq!((neg.type_label(), neg.to_string()), ("FLOAT", "-2.5".to_string()));
    }

    #[test]
    fn three_valued_logic() {
        let t = Value::Bool(true);
        let f = Value::Bool(false);
        assert_eq!(bin(BinOp::And, Value::Null, f.clone()), f);
        assert!(bin(BinOp::And, Value::Null, t.clone()).is_null());
        assert_eq!(bin(BinOp::Or, Value::Null, t.clone()), t);
        assert!(bin(BinOp::Or, Value::Null, f).is_null());
    }

    #[test]
    fn concat_treats_null_as_empty() {
        assert_eq!(bin(BinOp::Concat, Value::Null, "x".into()), Value::text("x"));
    }

    #[test]
    fn numbers_are_truthy_when_nonzero() {
        for v in [Value::Int(2), Value::Float(0.5), Value::Double(-1.0), Value::Bool(true)] {
            assert_eq!(tri(&v).unwrap(), Some(true));
        }
        let zero = crate::decimal::Decimal::from_i64(0);
        for v in [Value::Int(0), Value::Float(0.0), Value::Dec(zero.clone()), Value::Currency(zero)] {
            assert_eq!(tri(&v).unwrap(), Some(false));
        }
    }
}

/// The parameter names of the built-in functions that PostgreSQL lets a call give by name (`name => value`).
pub(crate) fn named_params(func: &str) -> Option<&'static [&'static str]> {
    Some(match func.to_ascii_uppercase().as_str() {
        "PARSE_IDENT" => &["str", "strict"],
        "JSONB_PATH_QUERY_ARRAY" | "JSONB_PATH_QUERY_FIRST" | "JSONB_PATH_EXISTS" | "JSONB_PATH_MATCH" | "JSONB_PATH_QUERY_ARRAY_TZ" | "JSONB_PATH_QUERY_FIRST_TZ" | "JSONB_PATH_EXISTS_TZ" | "JSONB_PATH_MATCH_TZ" => {
            &["target", "path", "vars", "silent"]
        }
        "JSONB_SET" => &["target", "path", "new_value", "create_if_missing"],
        "JSONB_INSERT" => &["target", "path", "new_value", "insert_after"],
        "JSON_STRIP_NULLS" | "JSONB_STRIP_NULLS" => &["target", "strip_in_arrays"],
        "DATE_BIN" => &["stride", "source", "origin"],
        "SPLIT_PART" => &["string", "delimiter", "n"],
        "MAKE_DATE" => &["year", "month", "day"],
        _ => return None,
    })
}
