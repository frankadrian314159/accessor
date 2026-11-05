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
        for (needed, wanted) in [("update", m.whens.iter().any(|w| matches!(w.action, MergeAction::Update(_)))), ("delete", m.whens.iter().any(|w| matches!(w.action, MergeAction::Delete))), ("insert", m.whens.iter().any(|w| matches!(w.action, MergeAction::Insert { .. })))] {
            if wanted {
                self.require_privilege(&m.target, needed)?;
            }
        }
        if self.table(&m.target).is_none() && self.view_sql(&m.target).is_some() {
            let through = self.merge_through_view(m)?;
            return self.exec_merge(&through);
        }
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

    /// A MERGE whose target is a simple view, as one on the table the view reads: the view's columns are renamed to
    /// the table's, and the view's condition joins the `ON` condition (a row outside the view matches nothing).
    fn merge_through_view(&self, m: &MergeDef) -> Result<MergeDef> {
        let t = self.view_target(&m.target)?;
        let alias = m.target_alias.clone().unwrap_or_else(|| m.target.clone());
        let rename = |e: &Expr| -> Result<Expr> {
            let mut e = e.clone();
            walk_expr(&mut e, &mut |x| {
                if let Expr::Column { table, name } = x {
                    if table.as_deref().is_none_or(|q| q.eq_ignore_ascii_case(&alias) || q.eq_ignore_ascii_case(&m.target)) {
                        if let Some((_, b)) = t.columns.iter().find(|(v, _)| v.eq_ignore_ascii_case(name)) {
                            *name = b.clone();
                        }
                    }
                }
                Ok(())
            })?;
            Ok(e)
        };
        let base_col = |c: &String| t.columns.iter().find(|(v, _)| v.eq_ignore_ascii_case(c)).map(|(_, b)| b.clone()).unwrap_or_else(|| c.clone());
        let mut on = rename(&m.on)?;
        if let Some(f) = &t.filter {
            let mut f = f.clone();
            walk_expr(&mut f, &mut |x| {
                if let Expr::Column { table: table @ None, .. } = x {
                    *table = Some(alias.clone());
                }
                Ok(())
            })?;
            on = Expr::Binary { op: BinOp::And, left: Box::new(on), right: Box::new(f) };
        }
        let mut whens = Vec::new();
        for w in &m.whens {
            let condition = w.condition.as_ref().map(&rename).transpose()?;
            let action = match &w.action {
                MergeAction::Update(sets) => MergeAction::Update(sets.iter().map(|(c, e)| Ok((base_col(c), rename(e)?))).collect::<Result<Vec<_>>>()?),
                MergeAction::Insert { columns, values } => MergeAction::Insert {
                    columns: Some(match columns {
                        Some(c) => c.iter().map(&base_col).collect(),
                        None => t.columns.iter().map(|(_, b)| b.clone()).collect(),
                    }),
                    values: values.iter().map(&rename).collect::<Result<Vec<_>>>()?,
                },
                other => other.clone(),
            };
            whens.push(MergeWhen { matched: w.matched, by_source: w.by_source, condition, action });
        }
        Ok(MergeDef { target: t.base.clone(), target_alias: Some(alias), source: m.source.clone(), on, whens })
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
            TableFactor::Derived { alias, .. } | TableFactor::Materialized { alias, .. } | TableFactor::Function { alias, .. } | TableFactor::Pivot { alias, .. } | TableFactor::Unpivot { alias, .. } | TableFactor::Nested { alias, .. } => alias.clone(),
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
                for w in m.whens.iter().filter(|w| !w.matched && !w.by_source) {
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
                        MergeAction::Nothing => {}
                        MergeAction::Insert { .. } => unreachable!("rejected by the parser"),
                    }
                    break;
                }
            }
        }
        // WHEN NOT MATCHED BY SOURCE: the target records no source record matched.
        if m.whens.iter().any(|w| w.by_source) {
            let all: Vec<RowId> = self.ids_matching(&target, &Expr::Literal(Value::Bool(true)))?;
            for id in all.into_iter().filter(|id| !touched.contains(id)) {
                let own = |e: &Expr| -> Result<Expr> {
                    let mut e = e.clone();
                    walk_expr(&mut e, &mut |x| {
                        if let Expr::Column { table, .. } = x {
                            if table.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(&target_alias)) {
                                *table = Some(target.clone());
                            }
                        }
                        Ok(())
                    })?;
                    Ok(e)
                };
                for w in m.whens.iter().filter(|w| w.by_source) {
                    if let Some(c) = &w.condition {
                        if !self.row_condition(&target, id, &own(c)?)? {
                            continue;
                        }
                    }
                    match &w.action {
                        MergeAction::Update(sets) => {
                            let sets = sets.iter().map(|(c, e)| Ok((c.clone(), own(e)?))).collect::<Result<Vec<_>>>()?;
                            let ops = self.plan_update_ids(&target, &sets, &[id])?;
                            affected += ops.len();
                            self.apply_all(ops)?;
                        }
                        MergeAction::Delete => {
                            self.apply_all(vec![Op::Delete { table: target.clone(), id }])?;
                            affected += 1;
                        }
                        _ => {}
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
