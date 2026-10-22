//! Row-level triggers.
//!
//! A trigger fires around each individual row operation, whether it came
//! from the user's statement, a foreign-key cascade, or another trigger. Its
//! body statements are planned against the current state with `NEW`/`OLD`
//! replaced by the row's values, and the resulting operations join the same
//! atomic batch: if anything fails, everything the trigger did is undone and
//! the write-ahead log replays the full effect without re-running triggers.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::database::Database;
use crate::error::{Error, Result};
use crate::exec::bind::{Binder, Scope};
use crate::fk::{Applied, RefMap};
use crate::sql::ast::{Statement, TriggerDef, TriggerEvent, TriggerTiming};
use crate::sql::walk::RowContext;
use crate::storage::Op;
use crate::value::{Row, Value};

/// How deeply triggers may cause other triggers before the statement fails.
pub(crate) const MAX_DEPTH: usize = 16;

/// The row operation a trigger is reacting to.
pub(crate) struct Fired {
    table: String,
    event: TriggerEvent,
    old: Option<Row>,
    new: Option<Row>,
}

impl Database {
    /// Describes `op` for trigger matching, or `None` when no trigger is
    /// attached to its table (the common case, kept cheap).
    pub(crate) fn trigger_context(&self, op: &Op) -> Option<Fired> {
        if self.triggers.is_empty() {
            return None;
        }
        let (table, event, id, new) = match op {
            Op::Insert { table, row, .. } => (table, TriggerEvent::Insert, None, Some(row.clone())),
            Op::Update { table, id, row } => (table, TriggerEvent::Update, Some(*id), Some(row.clone())),
            Op::Delete { table, id } => (table, TriggerEvent::Delete, Some(*id), None),
            _ => return None,
        };
        if !self.triggers.values().any(|t| t.event == event && t.table.eq_ignore_ascii_case(table)) {
            return None;
        }
        let old = id.and_then(|id| self.table(table)?.get(id).cloned());
        Some(Fired { table: table.clone(), event, old, new })
    }

    fn eval_condition(&self, e: &crate::sql::ast::Expr) -> Result<bool> {
        let scope = Scope::default();
        let v = Binder::new(self, &scope, 0).bind(e)?.eval(&[])?.into_owned();
        Ok(matches!(v, Value::Bool(true)))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fire_triggers(
        &mut self,
        timing: TriggerTiming,
        fired: &Fired,
        ops: &mut Vec<Op>,
        undo: &mut Vec<Op>,
        queue: &mut VecDeque<Applied>,
        refs: &RefMap,
        depth: usize,
    ) -> Result<()> {
        let matching: Vec<Arc<TriggerDef>> = self
            .triggers
            .values()
            .filter(|t| t.timing == timing && t.event == fired.event && t.table.eq_ignore_ascii_case(&fired.table))
            .cloned()
            .collect();
        if matching.is_empty() {
            return Ok(());
        }
        let schema = self.table_checked(&fired.table)?.schema().clone();
        let ctx = RowContext { schema: &schema, old: fired.old.as_ref(), new: fired.new.as_ref() };
        for trigger in matching {
            if let Some(when) = &trigger.when {
                let mut when = when.clone();
                ctx.bind_expr(&mut when)?;
                if !self.eval_condition(&when)? {
                    continue;
                }
            }
            for stmt in &trigger.body {
                let mut stmt = stmt.clone();
                ctx.bind_statement(&mut stmt)?;
                let planned = match &stmt {
                    Statement::Insert { table, columns, source } => self.plan_insert(table, columns, source)?,
                    Statement::Update { table, assignments, filter } => {
                        self.plan_update(table, assignments, filter.as_ref())?
                    }
                    Statement::Delete { table, filter } => self.plan_delete(table, filter.as_ref())?,
                    Statement::Raise(message) => {
                        let scope = Scope::default();
                        let text = Binder::new(self, &scope, 0).bind(message)?.eval(&[])?.into_owned();
                        return Err(Error::constraint(text.to_string()));
                    }
                    _ => unreachable!("the parser only admits DML and RAISE in trigger bodies"),
                };
                for op in planned {
                    self.apply_tracked(op, ops, undo, queue, refs, depth + 1)?;
                }
            }
        }
        Ok(())
    }
}
