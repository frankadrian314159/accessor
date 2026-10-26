//! `MERGE INTO target USING source ON condition WHEN [NOT] MATCHED ...`.
//!
//! The source is read first. Then, for each source record in turn, the ON
//! condition (with that record's values filled in) finds the matching target
//! records; the first `WHEN MATCHED` clause whose extra condition holds updates
//! or deletes them, and with no match the first suitable `WHEN NOT MATCHED`
//! clause inserts. The whole statement is atomic.

use std::collections::HashSet;

use super::QueryResult;
use crate::database::Database;
use crate::error::{Error, Result};
use crate::sql::ast::*;
use crate::sql::walk::walk_expr;
use crate::storage::{Op, RowId};
use crate::value::Value;

impl Database {
    pub(crate) fn exec_merge(&mut self, m: &MergeDef) -> Result<QueryResult> {
        let own = !self.in_transaction();
        if own {
            self.begin()?;
        }
        const SAVEPOINT: &str = "__merge";
        let started = self.savepoint(SAVEPOINT);
        let outcome = started.and_then(|_| self.merge_rows(m));
        let result = match outcome {
            Ok(n) => {
                let released = self.release(SAVEPOINT);
                if own {
                    released.and_then(|_| self.commit()).map(|_| n)
                } else {
                    released.map(|_| n)
                }
            }
            Err(e) => {
                let _ = self.rollback_to(SAVEPOINT);
                let _ = self.release(SAVEPOINT);
                if own {
                    let _ = self.rollback();
                }
                Err(e)
            }
        };
        result.map(QueryResult::affected)
    }

    fn merge_rows(&mut self, m: &MergeDef) -> Result<usize> {
        let source = Select {
            items: vec![SelectItem::Wildcard],
            from: Some(From { first: m.source.clone(), joins: Vec::new() }),
            ..Select::default()
        };
        let src = self.run_select(&source)?;
        let src_alias = match &m.source {
            TableFactor::Named { name, alias, .. } => alias.clone().unwrap_or_else(|| name.clone()),
            TableFactor::Derived { alias, .. } | TableFactor::Materialized { alias, .. } | TableFactor::Function { alias, .. } | TableFactor::Pivot { alias, .. } | TableFactor::Unpivot { alias, .. } => alias.clone(),
        };
        let (target, target_cols) = {
            let t = self.table_checked(&m.target)?;
            (t.name().to_string(), t.schema().columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>())
        };
        let target_alias = m.target_alias.clone().unwrap_or_else(|| target.clone());

        let mut touched: HashSet<RowId> = HashSet::new();
        let mut affected = 0;
        for srow in &src.rows {
            // Fill in the source record's values, and name the target by its table.
            let fill = |e: &Expr| -> Result<Expr> {
                let mut e = e.clone();
                walk_expr(&mut e, &mut |x| {
                    let Expr::Column { table, name } = x else { return Ok(()) };
                    let from_source = match table {
                        Some(t) if t.eq_ignore_ascii_case(&src_alias) => true,
                        Some(_) => false,
                        None => {
                            src.columns.iter().any(|c| c.eq_ignore_ascii_case(name))
                                && !target_cols.iter().any(|c| c.eq_ignore_ascii_case(name))
                        }
                    };
                    if from_source {
                        let i = src
                            .columns
                            .iter()
                            .position(|c| c.eq_ignore_ascii_case(name))
                            .ok_or_else(|| Error::catalog(format!("the source has no column '{name}'")))?;
                        *x = Expr::Literal(srow[i].clone());
                    } else if table.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(&target_alias)) {
                        *table = Some(target.clone());
                    }
                    Ok(())
                })?;
                Ok(e)
            };

            let ids = self.ids_matching(&target, &fill(&m.on)?)?;
            if ids.is_empty() {
                for w in m.whens.iter().filter(|w| !w.matched) {
                    if let Some(c) = &w.condition {
                        if !self.constant_truth(&fill(c)?)? {
                            continue;
                        }
                    }
                    if let MergeAction::Insert { columns, values } = &w.action {
                        let vals = values.iter().map(&fill).collect::<Result<Vec<_>>>()?;
                        let ops = self.plan_insert(&target, columns, &InsertSource::Values(vec![vals]))?;
                        affected += ops.len();
                        self.apply_all(ops)?;
                    }
                    break;
                }
                continue;
            }
            for id in ids {
                if !touched.insert(id) {
                    return Err(Error::runtime(
                        "MERGE: a target record matches more than one source record",
                    ));
                }
                for w in m.whens.iter().filter(|w| w.matched) {
                    if let Some(c) = &w.condition {
                        if !self.row_condition(&target, id, &fill(c)?)? {
                            continue;
                        }
                    }
                    match &w.action {
                        MergeAction::Update(sets) => {
                            let sets = sets.iter().map(|(c, e)| Ok((c.clone(), fill(e)?))).collect::<Result<Vec<_>>>()?;
                            let ops = self.plan_update_ids(&target, &sets, &[id])?;
                            affected += ops.len();
                            self.apply_all(ops)?;
                        }
                        MergeAction::Delete => {
                            self.apply_all(vec![Op::Delete { table: target.clone(), id }])?;
                            affected += 1;
                        }
                        MergeAction::Insert { .. } => unreachable!("rejected by the parser"),
                    }
                    break;
                }
            }
        }
        Ok(affected)
    }

    fn constant_truth(&self, e: &Expr) -> Result<bool> {
        let scope = super::bind::Scope::default();
        let v = super::bind::Binder::new(self, &scope, 0).bind(e)?.eval(&[])?.into_owned();
        Ok(matches!(v, Value::Bool(true)))
    }
}
