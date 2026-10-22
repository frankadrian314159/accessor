//! Window functions: `func(...) OVER (PARTITION BY ... ORDER BY ... frame)`.
//!
//! A window function does not change the number of rows. Each one is computed
//! after grouping, from the rows as they stand, and appended to every row as a
//! hidden column that the select list then reads.

use std::cmp::Ordering;

use super::agg::{AggFn, AggSpec, AggState};
use crate::error::{Error, Result};
use crate::expr::BExpr;
use crate::sql::ast::{Frame, FrameBound, FrameUnits};
use crate::value::{Row, Value};

/// Column positions at or above this stand for "the n-th window column", and
/// are rewritten once the width of the row they extend is known.
pub const WIN_BASE: usize = 1 << 40;
/// Stand-in for the GROUPING mask column of grouped rows.
pub const MASK_COL: usize = WIN_BASE - 1;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WinFn {
    RowNumber,
    Rank,
    DenseRank,
    PercentRank,
    CumeDist,
    Ntile,
    Lag,
    Lead,
    FirstValue,
    LastValue,
    NthValue,
    Agg(AggFn),
}

impl WinFn {
    pub fn from_name(name: &str) -> Option<WinFn> {
        Some(match name.to_ascii_uppercase().as_str() {
            "ROW_NUMBER" => WinFn::RowNumber,
            "RANK" => WinFn::Rank,
            "DENSE_RANK" => WinFn::DenseRank,
            "PERCENT_RANK" => WinFn::PercentRank,
            "CUME_DIST" => WinFn::CumeDist,
            "NTILE" => WinFn::Ntile,
            "LAG" => WinFn::Lag,
            "LEAD" => WinFn::Lead,
            "FIRST_VALUE" => WinFn::FirstValue,
            "LAST_VALUE" => WinFn::LastValue,
            "NTH_VALUE" => WinFn::NthValue,
            other => WinFn::Agg(AggFn::from_name(other)?),
        })
    }

    /// The `(min, max)` number of arguments.
    pub fn arity(self) -> (usize, usize) {
        match self {
            WinFn::RowNumber | WinFn::Rank | WinFn::DenseRank | WinFn::PercentRank | WinFn::CumeDist => (0, 0),
            WinFn::Ntile | WinFn::FirstValue | WinFn::LastValue => (1, 1),
            WinFn::Lag | WinFn::Lead => (1, 3),
            WinFn::NthValue => (2, 2),
            WinFn::Agg(AggFn::StringAgg) => (1, 2),
            WinFn::Agg(_) => (0, 1),
        }
    }
}

/// A bound window function call.
pub struct WinSpecB {
    pub func: WinFn,
    pub args: Vec<BExpr>,
    pub star: bool,
    pub distinct: bool,
    pub sep: Option<String>,
    pub partition: Vec<BExpr>,
    pub order: Vec<(BExpr, bool)>,
    pub frame: Option<Frame>,
}

impl WinSpecB {
    pub fn map_cols(&mut self, f: &dyn Fn(usize) -> usize) {
        let m = |e: &BExpr| e.map_cols(f);
        self.args = self.args.iter().map(m).collect();
        self.partition = self.partition.iter().map(m).collect();
        self.order = self.order.iter().map(|(e, d)| (e.map_cols(f), *d)).collect();
    }
}

/// Appends one column per window function to every row.
pub fn compute(mut rows: Vec<Row>, wins: &[WinSpecB]) -> Result<Vec<Row>> {
    for w in wins {
        let col = compute_one(&rows, w)?;
        for (r, v) in rows.iter_mut().zip(col) {
            r.push(v);
        }
    }
    Ok(rows)
}

fn eval_all(rows: &[Row], exprs: &[BExpr]) -> Result<Vec<Vec<Value>>> {
    rows.iter()
        .map(|r| exprs.iter().map(|e| e.eval(r).map(|v| v.into_owned())).collect::<Result<Vec<_>>>())
        .collect()
}

fn cmp_keys(a: &[Value], b: &[Value], desc: &[bool]) -> Ordering {
    for ((x, y), d) in a.iter().zip(b).zip(desc) {
        let o = x.cmp(y);
        let o = if *d { o.reverse() } else { o };
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

fn compute_one(rows: &[Row], w: &WinSpecB) -> Result<Vec<Value>> {
    let n = rows.len();
    let pkeys = eval_all(rows, &w.partition)?;
    let order_exprs: Vec<BExpr> = w.order.iter().map(|(e, _)| e.clone()).collect();
    let desc: Vec<bool> = w.order.iter().map(|(_, d)| *d).collect();
    let okeys = eval_all(rows, &order_exprs)?;
    let args = w.args.iter().map(|a| rows.iter().map(|r| a.eval(r).map(|v| v.into_owned())).collect::<Result<Vec<_>>>());
    let argv: Vec<Vec<Value>> = args.collect::<Result<_>>()?;

    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| pkeys[a].cmp(&pkeys[b]).then_with(|| cmp_keys(&okeys[a], &okeys[b], &desc)));

    let mut out = vec![Value::Null; n];
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && pkeys[idx[j]] == pkeys[idx[i]] {
            j += 1;
        }
        partition(w, &idx[i..j], &argv, &okeys, &desc, &mut out)?;
        i = j;
    }
    Ok(out)
}

fn partition(w: &WinSpecB, part: &[usize], argv: &[Vec<Value>], okeys: &[Vec<Value>], desc: &[bool], out: &mut [Value]) -> Result<()> {
    let m = part.len();
    // Peer groups: rows that tie on the ORDER BY keys (all rows, without one).
    let mut peer_start = vec![0usize; m];
    let mut peer_end = vec![0usize; m];
    let mut dense = vec![0usize; m];
    let mut start = 0;
    let mut group = 0;
    for p in 0..m {
        if p > 0 && cmp_keys(&okeys[part[p]], &okeys[part[p - 1]], desc) != Ordering::Equal {
            start = p;
            group += 1;
        }
        peer_start[p] = start;
        dense[p] = group;
    }
    let mut end = m;
    for p in (0..m).rev() {
        if p + 1 < m && peer_start[p + 1] != peer_start[p] {
            end = p + 1;
        }
        peer_end[p] = end - 1;
    }

    let frame = w.frame.unwrap_or(Frame {
        units: FrameUnits::Range,
        start: FrameBound::UnboundedPreceding,
        end: if w.order.is_empty() { FrameBound::UnboundedFollowing } else { FrameBound::CurrentRow },
    });
    let bounds = |p: usize| -> Result<Option<(usize, usize)>> {
        let (lo, hi): (i64, i64) = match frame.units {
            FrameUnits::Rows => {
                let lo = match frame.start {
                    FrameBound::UnboundedPreceding => 0,
                    FrameBound::Preceding(k) => p as i64 - k as i64,
                    FrameBound::CurrentRow => p as i64,
                    FrameBound::Following(k) => p as i64 + k as i64,
                    FrameBound::UnboundedFollowing => return Err(Error::runtime("a frame cannot start at UNBOUNDED FOLLOWING")),
                };
                let hi = match frame.end {
                    FrameBound::UnboundedFollowing => m as i64 - 1,
                    FrameBound::Following(k) => p as i64 + k as i64,
                    FrameBound::CurrentRow => p as i64,
                    FrameBound::Preceding(k) => p as i64 - k as i64,
                    FrameBound::UnboundedPreceding => return Err(Error::runtime("a frame cannot end at UNBOUNDED PRECEDING")),
                };
                (lo, hi)
            }
            FrameUnits::Range => {
                let lo = match frame.start {
                    FrameBound::UnboundedPreceding => 0,
                    FrameBound::CurrentRow => peer_start[p] as i64,
                    FrameBound::UnboundedFollowing => return Err(Error::runtime("a frame cannot start at UNBOUNDED FOLLOWING")),
                    _ => return Err(Error::runtime("RANGE frames with an offset are not supported; use ROWS")),
                };
                let hi = match frame.end {
                    FrameBound::UnboundedFollowing => m as i64 - 1,
                    FrameBound::CurrentRow => peer_end[p] as i64,
                    FrameBound::UnboundedPreceding => return Err(Error::runtime("a frame cannot end at UNBOUNDED PRECEDING")),
                    _ => return Err(Error::runtime("RANGE frames with an offset are not supported; use ROWS")),
                };
                (lo, hi)
            }
        };
        let (lo, hi) = (lo.max(0), hi.min(m as i64 - 1));
        Ok((lo <= hi).then_some((lo as usize, hi as usize)))
    };
    let arg_at = |k: usize, p: usize| -> &Value { &argv[k][part[p]] };

    match w.func {
        WinFn::RowNumber => (0..m).for_each(|p| out[part[p]] = Value::Int(p as i64 + 1)),
        WinFn::Rank => (0..m).for_each(|p| out[part[p]] = Value::Int(peer_start[p] as i64 + 1)),
        WinFn::DenseRank => (0..m).for_each(|p| out[part[p]] = Value::Int(dense[p] as i64 + 1)),
        WinFn::PercentRank => {
            for p in 0..m {
                let v = if m == 1 { 0.0 } else { peer_start[p] as f64 / (m - 1) as f64 };
                out[part[p]] = Value::from_f64(v).unwrap_or(Value::Null);
            }
        }
        WinFn::CumeDist => {
            for p in 0..m {
                out[part[p]] = Value::from_f64((peer_end[p] + 1) as f64 / m as f64).unwrap_or(Value::Null);
            }
        }
        WinFn::Ntile => {
            let k = match arg_at(0, 0) {
                Value::Int(k) if *k > 0 => *k as usize,
                _ => return Err(Error::runtime("NTILE needs a positive whole number of buckets")),
            };
            let (base, rem) = (m / k, m % k);
            for p in 0..m {
                let bucket = if p < rem * (base + 1) { p / (base + 1) + 1 } else { (p - rem * (base + 1)) / base + rem + 1 };
                out[part[p]] = Value::Int(bucket as i64);
            }
        }
        WinFn::Lag | WinFn::Lead => {
            for p in 0..m {
                let off = match argv.get(1) {
                    Some(_) => match arg_at(1, p) {
                        Value::Int(k) if *k >= 0 => *k as usize,
                        _ => return Err(Error::runtime("the offset of LAG/LEAD must be a non-negative whole number")),
                    },
                    None => 1,
                };
                let target = if w.func == WinFn::Lag { p.checked_sub(off) } else { Some(p + off).filter(|t| *t < m) };
                out[part[p]] = match target {
                    Some(t) => arg_at(0, t).clone(),
                    None => argv.get(2).map_or(Value::Null, |_| arg_at(2, p).clone()),
                };
            }
        }
        WinFn::FirstValue | WinFn::LastValue | WinFn::NthValue => {
            for p in 0..m {
                out[part[p]] = match bounds(p)? {
                    None => Value::Null,
                    Some((lo, hi)) => match w.func {
                        WinFn::FirstValue => arg_at(0, lo).clone(),
                        WinFn::LastValue => arg_at(0, hi).clone(),
                        _ => match arg_at(1, p) {
                            Value::Int(k) if *k >= 1 => {
                                let t = lo + *k as usize - 1;
                                if t <= hi { arg_at(0, t).clone() } else { Value::Null }
                            }
                            _ => return Err(Error::runtime("NTH_VALUE needs a positive whole number")),
                        },
                    },
                };
            }
        }
        WinFn::Agg(func) => {
            let spec = AggSpec { func, arg: None, distinct: w.distinct, sep: w.sep.clone() };
            let feed = |st: &mut AggState, p: usize| -> Result<()> {
                if w.star {
                    st.update(None)
                } else {
                    st.update(Some(arg_at(0, p)))
                }
            };
            let whole = matches!(
                (frame.start, frame.end),
                (FrameBound::UnboundedPreceding, FrameBound::UnboundedFollowing)
            );
            if whole {
                let mut st = AggState::new(&spec);
                for p in 0..m {
                    feed(&mut st, p)?;
                }
                let v = st.finish();
                (0..m).for_each(|p| out[part[p]] = v.clone());
            } else if frame.start == FrameBound::UnboundedPreceding {
                // A growing frame: feed each row once.
                let mut st = AggState::new(&spec);
                let mut fed = 0;
                for p in 0..m {
                    if let Some((_, hi)) = bounds(p)? {
                        while fed <= hi {
                            feed(&mut st, fed)?;
                            fed += 1;
                        }
                    }
                    out[part[p]] = st.clone().finish();
                }
            } else {
                for p in 0..m {
                    let mut st = AggState::new(&spec);
                    if let Some((lo, hi)) = bounds(p)? {
                        for q in lo..=hi {
                            feed(&mut st, q)?;
                        }
                    }
                    out[part[p]] = st.finish();
                }
            }
        }
    }
    Ok(())
}
