//! Aggregate functions and their running state.

use std::collections::HashSet;

use crate::error::{Error, Result};
use crate::expr::{arith, tri, BExpr};
use crate::sql::ast::BinOp;
use crate::value::Value;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AggFn {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    First,
    Last,
    StringAgg,
    StddevPop,
    StddevSamp,
    VarPop,
    VarSamp,
    Every,
    AnyTrue,
    JsonArrayAgg,
    BitAnd,
    BitOr,
    BitXor,
}

impl AggFn {
    pub fn from_name(name: &str) -> Option<AggFn> {
        Some(match name.to_ascii_uppercase().as_str() {
            "COUNT" => AggFn::Count,
            "SUM" => AggFn::Sum,
            "AVG" => AggFn::Avg,
            "MIN" => AggFn::Min,
            "MAX" => AggFn::Max,
            "FIRST" => AggFn::First,
            "LAST" => AggFn::Last,
            "ANY_VALUE" => AggFn::First,
            "STRING_AGG" | "LISTAGG" | "GROUP_CONCAT" => AggFn::StringAgg,
            "STDDEV_POP" | "STD" => AggFn::StddevPop,
            "STDDEV" | "STDDEV_SAMP" => AggFn::StddevSamp,
            "VAR_POP" => AggFn::VarPop,
            "VARIANCE" | "VAR_SAMP" => AggFn::VarSamp,
            "EVERY" | "BOOL_AND" => AggFn::Every,
            "BOOL_OR" => AggFn::AnyTrue,
            "JSON_ARRAYAGG" | "JSON_AGG" => AggFn::JsonArrayAgg,
            "BIT_AND" => AggFn::BitAnd,
            "BIT_OR" => AggFn::BitOr,
            "BIT_XOR" => AggFn::BitXor,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            AggFn::Count => "Count",
            AggFn::Sum => "Sum",
            AggFn::Avg => "Avg",
            AggFn::Min => "Min",
            AggFn::Max => "Max",
            AggFn::First => "First",
            AggFn::Last => "Last",
            AggFn::StringAgg => "StringAgg",
            AggFn::StddevPop => "StddevPop",
            AggFn::StddevSamp => "StddevSamp",
            AggFn::VarPop => "VarPop",
            AggFn::VarSamp => "VarSamp",
            AggFn::Every => "Every",
            AggFn::AnyTrue => "AnyTrue",
            AggFn::JsonArrayAgg => "JsonArrayAgg",
            AggFn::BitAnd => "BitAnd",
            AggFn::BitOr => "BitOr",
            AggFn::BitXor => "BitXor",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AggSpec {
    pub func: AggFn,
    /// `None` means `COUNT(*)`.
    pub arg: Option<BExpr>,
    pub distinct: bool,
    /// The separator of `STRING_AGG`.
    pub sep: Option<String>,
    /// `ORDER BY` inside the call (`GROUP_CONCAT(x ORDER BY y DESC)`): the key and whether it is descending.
    pub order: Vec<(BExpr, bool)>,
}

#[derive(Clone)]
pub struct AggState {
    func: AggFn,
    seen: Option<HashSet<Value>>,
    count: i64,
    acc: Option<Value>,
    parts: Vec<String>,
    /// The sort keys of each part, when the call has an `ORDER BY`.
    keys: Vec<Vec<Value>>,
    desc: Vec<bool>,
    sep: String,
    mean: f64,
    m2: f64,
    /// The bits of `BIT_AND` / `BIT_OR` / `BIT_XOR`: all ones to start with for the first, zero for the others.
    bits: u64,
}

impl AggState {
    pub fn new(spec: &AggSpec) -> Self {
        AggState {
            func: spec.func,
            seen: spec.distinct.then(HashSet::new),
            count: 0,
            acc: None,
            parts: Vec::new(),
            keys: Vec::new(),
            desc: spec.order.iter().map(|o| o.1).collect(),
            sep: spec.sep.clone().unwrap_or_else(|| ",".into()),
            mean: 0.0,
            m2: 0.0,
            bits: if spec.func == AggFn::BitAnd { u64::MAX } else { 0 },
        }
    }

    /// Feeds one input value with the sort keys of its row (an aggregate with an `ORDER BY`).
    pub fn update_ordered(&mut self, v: Option<&Value>, keys: Vec<Value>) -> Result<()> {
        let before = self.parts.len();
        self.update(v)?;
        if self.parts.len() > before {
            self.keys.push(keys);
        }
        Ok(())
    }

    /// Feeds one input value (`None` for `COUNT(*)`).
    pub fn update(&mut self, v: Option<&Value>) -> Result<()> {
        let Some(v) = v else {
            self.count += 1;
            return Ok(());
        };
        if v.is_null() {
            return Ok(());
        }
        if let Some(seen) = &mut self.seen {
            if !seen.insert(v.clone()) {
                return Ok(());
            }
        }
        match self.func {
            AggFn::Count => self.count += 1,
            AggFn::Sum => {
                let v = match v {
                    Value::Text(_) | Value::DateTime(_) => v
                        .num_lenient()
                        .ok_or_else(|| Error::type_err(format!("cannot sum '{v}'")))?
                        .into_owned(),
                    other => other.clone(),
                };
                self.acc = Some(match self.acc.take() {
                    None => v,
                    Some(a) => arith(BinOp::Add, &a, &v)?,
                });
            }
            AggFn::Avg => {
                // Summed exactly; the division happens once, in `finish`.
                let n = match v {
                    Value::Interval(_) => v.clone(),
                    _ => v
                        .num_lenient()
                        .ok_or_else(|| Error::type_err(format!("cannot average '{v}'")))?
                        .into_owned(),
                };
                self.acc = Some(match self.acc.take() {
                    None => n,
                    Some(a) => arith(BinOp::Add, &a, &n)?,
                });
                self.count += 1;
            }
            AggFn::Min => {
                if self.acc.as_ref().is_none_or(|a| v < a) {
                    self.acc = Some(v.clone());
                }
            }
            AggFn::Max => {
                if self.acc.as_ref().is_none_or(|a| v > a) {
                    self.acc = Some(v.clone());
                }
            }
            AggFn::First => {
                if self.acc.is_none() {
                    self.acc = Some(v.clone());
                }
            }
            AggFn::Last => self.acc = Some(v.clone()),
            AggFn::StringAgg => self.parts.push(v.to_string()),
            AggFn::JsonArrayAgg => self.parts.push(crate::json::to_text(&crate::json::from_value(v))),
            AggFn::StddevPop | AggFn::StddevSamp | AggFn::VarPop | AggFn::VarSamp => {
                let x = v
                    .num_lenient()
                    .and_then(|n| n.to_f64())
                    .ok_or_else(|| Error::type_err(format!("cannot compute a variance over '{v}'")))?;
                self.count += 1;
                let delta = x - self.mean;
                self.mean += delta / self.count as f64;
                self.m2 += delta * (x - self.mean);
            }
            AggFn::BitAnd | AggFn::BitOr | AggFn::BitXor => {
                let n = v
                    .num_lenient()
                    .and_then(|n| n.rounded_int())
                    .ok_or_else(|| Error::type_err(format!("cannot take the bits of '{v}'")))?;
                use num_traits::ToPrimitive;
                let b = n.to_i64().map(|i| i as u64).or_else(|| n.to_u64()).ok_or_else(|| Error::type_err(format!("'{v}' is out of range for a bit operation")))?;
                match self.func {
                    AggFn::BitAnd => self.bits &= b,
                    AggFn::BitOr => self.bits |= b,
                    _ => self.bits ^= b,
                }
            }
            AggFn::Every | AggFn::AnyTrue => {
                let t = tri(v)?.unwrap_or(false);
                self.acc = Some(Value::Bool(match (&self.acc, self.func) {
                    (None, _) => t,
                    (Some(Value::Bool(a)), AggFn::Every) => *a && t,
                    (Some(Value::Bool(a)), _) => *a || t,
                    _ => t,
                }));
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Value {
        match self.func {
            AggFn::Count => Value::Int(self.count),
            AggFn::Avg => {
                match self.acc {
                    Some(sum) if self.count > 0 => arith(BinOp::ExactDiv, &sum, &Value::Int(self.count)).unwrap_or(Value::Null),
                    _ => Value::Null,
                }
            }
            AggFn::StringAgg | AggFn::JsonArrayAgg if !self.keys.is_empty() => {
                let mut at: Vec<usize> = (0..self.parts.len()).collect();
                let (keys, desc) = (&self.keys, &self.desc);
                at.sort_by(|&a, &b| {
                    for (i, d) in desc.iter().enumerate() {
                        let o = keys[a][i].cmp(&keys[b][i]);
                        let o = if *d { o.reverse() } else { o };
                        if o != std::cmp::Ordering::Equal {
                            return o;
                        }
                    }
                    std::cmp::Ordering::Equal
                });
                let parts: Vec<&String> = at.iter().map(|&i| &self.parts[i]).collect();
                if self.func == AggFn::StringAgg {
                    Value::text(&parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(&self.sep))
                } else {
                    Value::Json(format!("[{}]", parts.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(",")).as_str().into())
                }
            }
            AggFn::StringAgg => {
                if self.parts.is_empty() {
                    Value::Null
                } else {
                    Value::text(&self.parts.join(&self.sep))
                }
            }
            AggFn::JsonArrayAgg => {
                if self.parts.is_empty() {
                    Value::Null
                } else {
                    Value::Json(format!("[{}]", self.parts.join(",")).as_str().into())
                }
            }
            AggFn::StddevPop | AggFn::StddevSamp | AggFn::VarPop | AggFn::VarSamp => {
                let n = self.count as f64;
                let (var, enough) = match self.func {
                    AggFn::VarPop | AggFn::StddevPop => (self.m2 / n, self.count >= 1),
                    _ => (self.m2 / (n - 1.0), self.count >= 2),
                };
                if !enough {
                    return Value::Null;
                }
                let r = if matches!(self.func, AggFn::StddevPop | AggFn::StddevSamp) { var.sqrt() } else { var };
                Value::from_f64(r).unwrap_or(Value::Null)
            }
            AggFn::BitAnd | AggFn::BitOr | AggFn::BitXor => {
                // unsigned 64 bits, as MySQL's are
                if self.bits <= i64::MAX as u64 {
                    Value::Int(self.bits as i64)
                } else {
                    Value::Big(num_bigint::BigInt::from(self.bits))
                }
            }
            _ => self.acc.unwrap_or(Value::Null),
        }
    }
}
