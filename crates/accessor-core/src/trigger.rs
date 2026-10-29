//! Triggers.
//!
//! A *row* trigger fires around each individual row operation, whether it came
//! from the user's statement, a foreign-key cascade, or another trigger. A
//! *statement* trigger fires once around the statement (with no `OLD` or `NEW`).
//! An `INSTEAD OF` trigger replaces a change made to a query (a view).
//!
//! The body statements are planned against the current state with `NEW`/`OLD`
//! replaced by the row's values, and the resulting operations join the same
//! atomic batch: if anything fails, everything the trigger did is undone and
//! the write-ahead log replays the full effect without re-running triggers.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::database::Database;
use crate::error::{Error, Result};
use crate::exec::bind::{Binder, Scope};
use crate::exec::QueryResult;
use crate::fk::{Applied, RefMap};
use crate::schema::{Column, TableSchema};
use crate::sql::ast::*;
use crate::sql::walk::RowContext;
use crate::storage::Op;
use crate::value::{DataType, Row, Value};

/// How deeply triggers may cause other triggers before the statement fails.
pub(crate) const MAX_DEPTH: usize = 16;

/// The row operation a trigger is reacting to.
/// What the BEFORE triggers of a row decided.
pub(crate) enum Fire {
    Go,
    /// A trigger function changed the row.
    Replace(Row),
    /// A trigger function returned NULL: the operation is not done.
    Skip,
}

/// A rule `DO INSTEAD` on a query is an INSTEAD OF trigger.
pub(crate) fn rule_target(db: &Database, def: &mut TriggerDef) {
    if def.function.as_deref() == Some(crate::sql::ast::INSTEAD_MARK) && db.table(&def.table).is_none() && db.view_sql(&def.table).is_some() {
        def.timing = TriggerTiming::InsteadOf;
        def.function = None;
    }
}

pub(crate) struct Fired {
    table: String,
    event: TriggerEvent,
    old: Option<Row>,
    new: Option<Row>,
}

/// A stand-in schema for a query, so `NEW.col` and `OLD.col` can be resolved by name.
pub(crate) fn view_schema(name: &str, columns: &[String]) -> TableSchema {
    TableSchema {
        name: name.to_string(),
        columns: columns.iter().map(|c| Column::new(c.clone(), DataType::Text)).collect(),
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    }
}

/// `UPDATE OF a, b` triggers fire only when one of those columns changed.
fn update_of_hit(t: &TriggerDef, schema: &TableSchema, old: Option<&Row>, new: Option<&Row>) -> bool {
    if t.update_of.is_empty() {
        return true;
    }
    let (Some(o), Some(n)) = (old, new) else { return true };
    t.update_of.iter().any(|c| schema.column_index(c).is_some_and(|p| o[p] != n[p]))
}

impl Database {
    /// Describes `op` for trigger matching, or `None` when no row trigger is
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
        if !self.triggers.values().any(|t| {
            !t.each_statement && t.timing != TriggerTiming::InsteadOf && t.events.contains(&event) && t.table.eq_ignore_ascii_case(table)
        }) {
            return None;
        }
        let old = id.and_then(|id| self.table(table)?.get(id).cloned());
        Some(Fired { table: table.clone(), event, old, new })
    }

    fn eval_condition(&self, e: &Expr) -> Result<bool> {
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
    ) -> Result<Fire> {
        let matching: Vec<Arc<TriggerDef>> = self
            .triggers
            .values()
            .filter(|t| {
                t.timing == timing && !t.each_statement && t.events.contains(&fired.event) && t.table.eq_ignore_ascii_case(&fired.table)
            })
            .cloned()
            .collect();
        if matching.is_empty() {
            return Ok(Fire::Go);
        }
        let schema = self.table_checked(&fired.table)?.schema().clone();
        let mut current = fired.new.clone();
        let mut changed = false;
        for trigger in matching {
            if fired.event == TriggerEvent::Update && !update_of_hit(&trigger, &schema, fired.old.as_ref(), current.as_ref()) {
                continue;
            }
            let ctx = RowContext { schema: &schema, old: fired.old.as_ref(), new: current.as_ref() };
            match self.run_trigger(&trigger, &ctx, ops, undo, queue, refs, depth, timing)? {
                Fire::Go => {}
                Fire::Skip => return Ok(Fire::Skip),
                Fire::Replace(row) => {
                    current = Some(row);
                    changed = true;
                }
            }
        }
        Ok(match (changed, current) {
            (true, Some(row)) => Fire::Replace(row),
            _ => Fire::Go,
        })
    }

    /// Runs one trigger's body (after its `WHEN` condition) against the rows in `ctx`.
    #[allow(clippy::too_many_arguments)]
    fn run_trigger(
        &mut self,
        trigger: &TriggerDef,
        ctx: &RowContext,
        ops: &mut Vec<Op>,
        undo: &mut Vec<Op>,
        queue: &mut VecDeque<Applied>,
        refs: &RefMap,
        depth: usize,
        timing: TriggerTiming,
    ) -> Result<Fire> {
        if let Some(when) = &trigger.when {
            let mut when = when.clone();
            ctx.bind_expr(&mut when)?;
            if !self.eval_condition(&when)? {
                return Ok(Fire::Go);
            }
        }
        if !trigger.block.is_empty() {
            let def = crate::sql::ast::RoutineDef {
                name: trigger.name.clone(),
                function: true,
                params: Vec::new(),
                returns: None,
                body: crate::sql::ast::RoutineBody::Block(trigger.block.clone()),
                trigger: true,
                set_columns: Vec::new(),
                sql: String::new(),
            };
            return self.run_trigger_def(trigger, &def, ctx, timing);
        }
        let instead = trigger.function.as_deref() == Some(crate::sql::ast::INSTEAD_MARK);
        if let (Some(function), false) = (&trigger.function, instead) {
            return self.run_trigger_function(trigger, function, ctx, timing);
        }
        for stmt in &trigger.body {
            let mut stmt = stmt.clone();
            ctx.bind_statement(&mut stmt)?;
            let planned = match &stmt {
                Statement::Insert { table, columns, source } => self.plan_insert(table, columns, source)?,
                Statement::Update { table, assignments, filter } => self.plan_update(table, assignments, filter.as_ref())?,
                Statement::Delete { table, filter } => self.plan_delete(table, filter.as_ref())?,
                // Run for what it does (`SELECT NEXTVAL('s')`); the rows are not used.
                Statement::Select(sel) => {
                    self.run_select(sel)?;
                    Vec::new()
                }
                Statement::Raise(message) => {
                    let scope = Scope::default();
                    let text = Binder::new(self, &scope, 0).bind(message)?.eval(&[])?.into_owned();
                    return Err(Error::constraint(text.to_string()));
                }
                _ => unreachable!("the parser only admits DML, SELECT and RAISE in trigger bodies"),
            };
            for op in planned {
                self.apply_tracked(op, ops, undo, queue, refs, depth + 1)?;
            }
        }
        Ok(if instead { Fire::Skip } else { Fire::Go })
    }

    /// Runs a PL/pgSQL trigger function for the rows in `ctx`. `NEW.col`/`OLD.col` are its variables; whatever it
    /// assigns to `NEW.col` goes into the row, and `RETURN NULL` in a BEFORE trigger drops the operation.
    fn run_trigger_function(&mut self, trigger: &TriggerDef, function: &str, ctx: &RowContext, timing: TriggerTiming) -> Result<Fire> {
        let def = self
            .function_def(function)
            .or_else(|| self.routine_def(function))
            .filter(|d| d.trigger)
            .ok_or_else(|| Error::catalog(format!("the trigger function '{function}' does not exist")))?;
        self.run_trigger_def(trigger, &def, ctx, timing)
    }

    /// Runs the body of a trigger function (or of a MySQL trigger) for the rows in `ctx`.
    fn run_trigger_def(&mut self, trigger: &TriggerDef, def: &crate::sql::ast::RoutineDef, ctx: &RowContext, timing: TriggerTiming) -> Result<Fire> {
        let mut vars: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
        let event = if ctx.new.is_some() && ctx.old.is_some() {
            "UPDATE"
        } else if ctx.new.is_some() {
            "INSERT"
        } else if ctx.old.is_some() {
            "DELETE"
        } else {
            trigger.events.first().map_or("", |e| match e {
                TriggerEvent::Insert => "INSERT",
                TriggerEvent::Update => "UPDATE",
                TriggerEvent::Delete => "DELETE",
            })
        };
        let text = |s: &str| Value::text(s);
        vars.insert("tg_op".into(), text(event));
        vars.insert("tg_name".into(), text(&trigger.name));
        vars.insert("tg_when".into(), text(match timing { TriggerTiming::Before => "BEFORE", TriggerTiming::After => "AFTER", TriggerTiming::InsteadOf => "INSTEAD OF" }));
        vars.insert("tg_level".into(), text(if trigger.each_statement { "STATEMENT" } else { "ROW" }));
        vars.insert("tg_table_name".into(), text(&ctx.schema.name));
        vars.insert("tg_relname".into(), text(&ctx.schema.name));
        vars.insert("tg_table_schema".into(), text("public"));
        vars.insert("tg_nargs".into(), Value::Int(0));
        vars.insert("new".into(), text("NEW"));
        vars.insert("old".into(), text("OLD"));
        for (i, c) in ctx.schema.columns.iter().enumerate() {
            let name = c.name.to_ascii_lowercase();
            vars.insert(format!("new.{name}"), ctx.new.map_or(Value::Null, |r| r[i].clone()));
            vars.insert(format!("old.{name}"), ctx.old.map_or(Value::Null, |r| r[i].clone()));
        }
        let (returned, after) = self.run_trigger_block(def, vars)?;
        if timing != TriggerTiming::Before || trigger.each_statement {
            return Ok(Fire::Go);
        }
        if matches!(returned, Some(Value::Null)) || (returned.is_none() && false) {
            return Ok(Fire::Skip);
        }
        let Some(new) = ctx.new else { return Ok(Fire::Go) };
        let mut row = new.clone();
        let mut changed = false;
        for (i, c) in ctx.schema.columns.iter().enumerate() {
            if let Some(v) = after.get(&format!("new.{}", c.name.to_ascii_lowercase())) {
                if *v != row[i] {
                    row[i] = v.clone();
                    changed = true;
                }
            }
        }
        Ok(if changed { Fire::Replace(row) } else { Fire::Go })
    }

    // ----- statement triggers ------------------------------------------------------

    pub(crate) fn has_statement_triggers(&self) -> bool {
        self.triggers.values().any(|t| t.each_statement)
    }

    /// The `(table, event)` pairs a statement's operations amount to, in order.
    pub(crate) fn statement_events(&self, ops: &[Op]) -> Vec<(String, TriggerEvent)> {
        let mut out: Vec<(String, TriggerEvent)> = Vec::new();
        for op in ops {
            let (table, event) = match op {
                Op::Insert { table, .. } => (table, TriggerEvent::Insert),
                Op::Update { table, .. } => (table, TriggerEvent::Update),
                Op::Delete { table, .. } => (table, TriggerEvent::Delete),
                _ => continue,
            };
            if !out.iter().any(|(t, e)| t.eq_ignore_ascii_case(table) && *e == event) {
                out.push((table.clone(), event));
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fire_statement_triggers(
        &mut self,
        timing: TriggerTiming,
        table: &str,
        event: TriggerEvent,
        ops: &mut Vec<Op>,
        undo: &mut Vec<Op>,
        queue: &mut VecDeque<Applied>,
        refs: &RefMap,
    ) -> Result<()> {
        let matching: Vec<Arc<TriggerDef>> = self
            .triggers
            .values()
            .filter(|t| t.each_statement && t.timing == timing && t.events.contains(&event) && t.table.eq_ignore_ascii_case(table))
            .cloned()
            .collect();
        if matching.is_empty() {
            return Ok(());
        }
        let schema = self.table_checked(table)?.schema().clone();
        let ctx = RowContext { schema: &schema, old: None, new: None };
        for trigger in matching {
            self.run_trigger(&trigger, &ctx, ops, undo, queue, refs, 0, timing)?;
        }
        Ok(())
    }

    // ----- INSTEAD OF ----------------------------------------------------------------

    pub(crate) fn instead_of_triggers(&self, view: &str, event: TriggerEvent) -> Vec<Arc<TriggerDef>> {
        self.triggers
            .values()
            .filter(|t| t.timing == TriggerTiming::InsteadOf && t.events.contains(&event) && t.table.eq_ignore_ascii_case(view))
            .cloned()
            .collect()
    }

    /// Carries out a change to a query by running its `INSTEAD OF` triggers once for each
    /// row the change would have affected.
    pub(crate) fn exec_instead_of(&mut self, st: &Statement, triggers: Vec<Arc<TriggerDef>>) -> Result<QueryResult> {
        let view = match st {
            Statement::Insert { table, .. } | Statement::Update { table, .. } | Statement::Delete { table, .. } => table.clone(),
            _ => unreachable!("only data changes are routed here"),
        };
        let named = TableFactor::Named { name: view.clone(), alias: None, only: false };
        let names = self.resolve_factor(&named, 0)?.names;
        let schema = view_schema(&view, &names);
        let position = |c: &str| {
            names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(c))
                .ok_or_else(|| Error::catalog(format!("the field '{c}' does not exist in the query '{view}'")))
        };
        let from = From { first: named, joins: Vec::new() };

        // The rows to work on, as (OLD, NEW).
        let mut work: Vec<(Option<Row>, Option<Row>)> = Vec::new();
        match st {
            Statement::Insert { columns, source, .. } => {
                let positions: Vec<usize> = match (columns, source) {
                    (_, InsertSource::DefaultValues) => Vec::new(),
                    (Some(cols), _) => cols.iter().map(|c| position(c)).collect::<Result<_>>()?,
                    (None, _) => (0..names.len()).collect(),
                };
                let value_rows: Vec<Vec<Value>> = match source {
                    InsertSource::Values(rows) => rows
                        .iter()
                        .map(|r| r.iter().map(|e| self.eval_const(e)).collect::<Result<Vec<_>>>())
                        .collect::<Result<_>>()?,
                    InsertSource::DefaultValues => vec![Vec::new()],
                    InsertSource::Select(sel) => self.run_select(sel)?.rows,
                };
                for vr in value_rows {
                    if vr.len() != positions.len() {
                        return Err(Error::runtime(format!("expected {} value(s) but found {}", positions.len(), vr.len())));
                    }
                    let mut row: Row = vec![Value::Null; names.len()];
                    for (&p, v) in positions.iter().zip(vr) {
                        row[p] = v;
                    }
                    work.push((None, Some(row)));
                }
            }
            Statement::Update { assignments, filter, .. } => {
                // One query gives each row and the new values of the assigned columns.
                let mut items = vec![SelectItem::Wildcard];
                items.extend(assignments.iter().map(|(_, e)| SelectItem::Expr { expr: e.clone(), alias: None }));
                let sel = Select { items, from: Some(from), filter: filter.clone(), ..Select::default() };
                let n = names.len();
                for r in self.run_select(&sel)?.rows {
                    let old: Row = r[..n].to_vec();
                    let mut new = old.clone();
                    for (k, (col, _)) in assignments.iter().enumerate() {
                        new[position(col)?] = r[n + k].clone();
                    }
                    work.push((Some(old), Some(new)));
                }
            }
            Statement::Delete { filter, .. } => {
                let sel = Select { items: vec![SelectItem::Wildcard], from: Some(from), filter: filter.clone(), ..Select::default() };
                work.extend(self.run_select(&sel)?.rows.into_iter().map(|r| (Some(r), None)));
            }
            _ => unreachable!("only data changes are routed here"),
        }

        let count = work.len();
        self.atomically(|db| {
            for (old, new) in &work {
                let ctx = RowContext { schema: &schema, old: old.as_ref(), new: new.as_ref() };
                for t in &triggers {
                    if !update_of_hit(t, &schema, old.as_ref(), new.as_ref()) {
                        continue;
                    }
                    if let Some(when) = &t.when {
                        let mut when = when.clone();
                        ctx.bind_expr(&mut when)?;
                        if !db.eval_condition(&when)? {
                            continue;
                        }
                    }
                    for stmt in &t.body {
                        let mut stmt = stmt.clone();
                        ctx.bind_statement(&mut stmt)?;
                        match &stmt {
                            Statement::Raise(message) => {
                                let scope = Scope::default();
                                let text = Binder::new(db, &scope, 0).bind(message)?.eval(&[])?.into_owned();
                                return Err(Error::constraint(text.to_string()));
                            }
                            other => {
                                db.execute_statement(other)?;
                            }
                        }
                    }
                }
            }
            Ok(())
        })?;
        Ok(QueryResult::affected(count))
    }
}
