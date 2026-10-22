//! Aggregate functions and their running state.

use std::collections::HashSet;

use crate::error::{Error, Result};
use crate::expr::{arith, BExpr};
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
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AggSpec {
    pub func: AggFn,
    /// `None` means `COUNT(*)`.
    pub arg: Option<BExpr>,
    pub distinct: bool,
}

pub struct AggState {
    func: AggFn,
    seen: Option<HashSet<Value>>,
    count: i64,
    acc: Option<Value>,
}

impl AggState {
    pub fn new(spec: &AggSpec) -> Self {
        AggState {
            func: spec.func,
            seen: spec.distinct.then(HashSet::new),
            count: 0,
            acc: None,
        }
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
                let n = v
                    .num_lenient()
                    .ok_or_else(|| Error::type_err(format!("cannot average '{v}'")))?
                    .into_owned();
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
        }
        Ok(())
    }

    pub fn finish(self) -> Value {
        match self.func {
            AggFn::Count => Value::Int(self.count),
            AggFn::Avg => {
                match self.acc {
                    Some(sum) if self.count > 0 => arith(BinOp::Div, &sum, &Value::Int(self.count)).unwrap_or(Value::Null),
                    _ => Value::Null,
                }
            }
            _ => self.acc.unwrap_or(Value::Null),
        }
    }
}
