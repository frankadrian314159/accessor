//! Column statistics for the optimizer: how many distinct values a column has, which are the
//! most common, and how the rest are spread (an equi-depth histogram).
//!
//! Nothing here is stored. The statistics of a column are worked out from the rows the first time
//! a query needs them and kept until the table next changes, so they can never be out of date and
//! the same data always gives the same plan. A big table is read through a sample of evenly spaced
//! rows.

use std::collections::HashMap;
use std::ops::Bound;
use std::sync::{Arc, Mutex};

use crate::value::Value;

/// Rows read to work out a column's statistics; more than this and every n-th row is used.
const SAMPLE: usize = 20_000;
/// Boundaries of the histogram (so `BUCKETS` buckets, each holding the same number of rows).
const BUCKETS: usize = 32;
/// Values remembered with their own frequency.
const MOST_COMMON: usize = 10;

#[derive(Debug)]
pub struct ColumnStats {
    /// Rows looked at, NULLs included.
    pub rows: usize,
    pub nulls: usize,
    /// How many different values the column holds (estimated when sampled).
    pub distinct: f64,
    /// The commonest values and the fraction of all rows each makes up.
    pub most_common: Vec<(Value, f64)>,
    /// `BUCKETS + 1` values from the smallest to the largest; between two neighbours lies an equal share of the rows.
    pub bounds: Vec<Value>,
    /// Whether a sample was used rather than every row.
    pub sampled: bool,
}

/// A cheap, fixed scramble of a row position (splitmix64).
fn mix(i: u64) -> u64 {
    let mut z = i.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Values the histogram can order against each other without surprises.
fn same_kind(a: &Value, b: &Value) -> bool {
    use Value::*;
    let numeric = |v: &Value| v.is_numeric();
    (numeric(a) && numeric(b))
        || matches!((a, b), (Text(_), Text(_)) | (Bool(_), Bool(_)) | (Interval(_), Interval(_)) | (Json(_), Json(_)))
        || matches!((a, b), (DateTime(_), DateTime(_)) | (TimestampTz(_), TimestampTz(_)))
}

impl ColumnStats {
    /// Works out the statistics of the values of one column.
    pub fn compute<'a>(values: impl Iterator<Item = &'a Value>, total_rows: usize) -> ColumnStats {
        let stride = total_rows.div_ceil(SAMPLE).max(1);
        let mut nulls = 0;
        let mut seen = 0;
        let mut data: Vec<&Value> = Vec::new();
        for (i, v) in values.enumerate() {
            // Every stride-th row on average, picked by a fixed hash of the position so that data
            // which repeats with some period is not sampled in step with it.
            if stride > 1 && mix(i as u64) % stride as u64 != 0 {
                continue;
            }
            seen += 1;
            if v.is_null() {
                nulls += 1;
            } else {
                data.push(v);
            }
        }
        data.sort();
        // Runs of equal values.
        let mut runs: Vec<(usize, usize)> = Vec::new(); // (first index, length)
        for i in 0..data.len() {
            match runs.last_mut() {
                Some((start, len)) if data[*start] == data[i] => *len += 1,
                _ => runs.push((i, 1)),
            }
        }
        let sampled = stride > 1;
        let mut distinct = runs.len() as f64;
        if sampled && !data.is_empty() {
            // Values seen once in the sample stand for many unseen ones (the GEE estimator).
            let singles = runs.iter().filter(|(_, l)| *l == 1).count() as f64;
            distinct = (total_rows as f64 / data.len() as f64).sqrt() * singles + (distinct - singles);
        }
        let mut common: Vec<&(usize, usize)> = runs.iter().filter(|(_, l)| *l >= 2).collect();
        common.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        common.truncate(MOST_COMMON);
        let most_common = common.iter().map(|(s, l)| (data[*s].clone(), *l as f64 / seen.max(1) as f64)).collect();
        let bounds = if data.is_empty() {
            Vec::new()
        } else {
            (0..=BUCKETS).map(|i| data[i * (data.len() - 1) / BUCKETS].clone()).collect()
        };
        ColumnStats { rows: seen, nulls, distinct: distinct.max(1.0), most_common, bounds, sampled }
    }

    pub fn null_fraction(&self) -> f64 {
        if self.rows == 0 {
            0.0
        } else {
            self.nulls as f64 / self.rows as f64
        }
    }

    /// The fraction of rows equal to `v`.
    pub fn eq(&self, v: &Value) -> Option<f64> {
        if v.is_null() {
            return Some(0.0);
        }
        if self.bounds.is_empty() {
            return Some(0.0);
        }
        if !same_kind(v, &self.bounds[0]) {
            return None;
        }
        if let Some((_, f)) = self.most_common.iter().find(|(m, _)| m == v) {
            return Some(*f);
        }
        let in_common: f64 = self.most_common.iter().map(|(_, f)| f).sum();
        let others = (self.distinct - self.most_common.len() as f64).max(1.0);
        let left = (1.0 - self.null_fraction() - in_common).max(0.0);
        // A value outside the range seen is rare; one inside is the average of the rest.
        let outside = v < &self.bounds[0] || v > &self.bounds[self.bounds.len() - 1];
        Some(if outside { (left / others) * 0.1 } else { left / others })
    }

    /// The fraction of the non-NULL rows that are below `v` (or at most `v`).
    fn below(&self, v: &Value, inclusive: bool) -> Option<f64> {
        let b = &self.bounds;
        if b.is_empty() || !same_kind(v, &b[0]) {
            return None;
        }
        let k = (b.len() - 1) as f64;
        let i = b.partition_point(|x| if inclusive { x <= v } else { x < v });
        if i == 0 {
            return Some(0.0);
        }
        if i == b.len() {
            return Some(1.0);
        }
        // Between bound i-1 and bound i: how far along, when both can be measured.
        let along = match (b[i - 1].to_f64(), b[i].to_f64(), v.to_f64()) {
            (Some(lo), Some(hi), Some(x)) if hi > lo => ((x - lo) / (hi - lo)).clamp(0.0, 1.0),
            _ => 0.5,
        };
        Some((((i - 1) as f64) + along) / k)
    }

    /// The fraction of all rows with a value in the range.
    pub fn range(&self, lo: &Bound<Value>, hi: &Bound<Value>) -> Option<f64> {
        let low = match lo {
            Bound::Unbounded => 0.0,
            Bound::Included(v) => self.below(v, false)?,
            Bound::Excluded(v) => self.below(v, true)?,
        };
        let high = match hi {
            Bound::Unbounded => 1.0,
            Bound::Included(v) => self.below(v, true)?,
            Bound::Excluded(v) => self.below(v, false)?,
        };
        Some(((high - low).max(0.0) * (1.0 - self.null_fraction())).clamp(0.0, 1.0))
    }
}

/// The statistics of a table's columns, kept until the table changes.
#[derive(Default, Debug)]
pub(crate) struct StatsCache(Mutex<(u64, HashMap<usize, Arc<ColumnStats>>)>);

impl Clone for StatsCache {
    fn clone(&self) -> Self {
        StatsCache(Mutex::new(self.0.lock().map(|g| g.clone()).unwrap_or_default()))
    }
}

impl StatsCache {
    /// The statistics of column `col` for table state `generation`, computed by `make` if not cached.
    pub(crate) fn get(&self, generation: u64, col: usize, make: impl FnOnce() -> ColumnStats) -> Arc<ColumnStats> {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if g.0 != generation {
            *g = (generation, HashMap::new());
        }
        g.1.entry(col).or_insert_with(|| Arc::new(make())).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ints(v: &[i64]) -> Vec<Value> {
        v.iter().map(|i| Value::Int(*i)).collect()
    }

    #[test]
    fn counts_distinct_and_common_values() {
        let mut data = vec![Value::Text("open".into()); 90];
        data.extend(vec![Value::Text("closed".into()); 9]);
        data.push(Value::Text("odd".into()));
        data.push(Value::Null);
        let s = ColumnStats::compute(data.iter(), data.len());
        assert_eq!(s.distinct, 3.0);
        assert_eq!(s.nulls, 1);
        assert!((s.eq(&Value::Text("open".into())).unwrap() - 90.0 / 101.0).abs() < 1e-9);
        // A value not in the list shares what is left.
        let rare = s.eq(&Value::Text("odd".into())).unwrap();
        assert!(rare < 0.02, "{rare}");
    }

    #[test]
    fn ranges_follow_the_histogram() {
        let data = ints(&(0..1000).collect::<Vec<_>>());
        let s = ColumnStats::compute(data.iter(), data.len());
        let r = s.range(&Bound::Included(Value::Int(0)), &Bound::Excluded(Value::Int(250))).unwrap();
        assert!((r - 0.25).abs() < 0.03, "{r}");
        let r = s.range(&Bound::Excluded(Value::Int(900)), &Bound::Unbounded).unwrap();
        assert!((r - 0.1).abs() < 0.03, "{r}");
        // Skew: nine tenths of the rows are 1.
        let mut data = ints(&[1; 900]);
        data.extend(ints(&(100..200).collect::<Vec<_>>()));
        let s = ColumnStats::compute(data.iter(), data.len());
        let r = s.range(&Bound::Excluded(Value::Int(1)), &Bound::Unbounded).unwrap();
        assert!(r < 0.15, "{r}");
    }

    #[test]
    fn a_sample_estimates_the_distinct_count() {
        let data = ints(&(0..100_000).map(|i| i % 5000).collect::<Vec<_>>());
        let s = ColumnStats::compute(data.iter(), data.len());
        assert!(s.sampled);
        assert!(s.distinct > 2500.0 && s.distinct < 10_000.0, "{}", s.distinct);
    }
}
