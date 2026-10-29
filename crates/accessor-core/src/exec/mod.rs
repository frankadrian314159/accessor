//! Statement execution.

pub use prepared::Prepared;
pub mod agg;
pub mod bind;
mod budget;
mod multi;
mod mysqlshow;
mod catalog;
pub(crate) mod pushdown;
mod inherit;
mod check;
mod ddl;
mod merge;
mod seq;
mod settings;
mod routine;
mod reports;
mod infoschema;
mod returning;
mod prepared;
mod decorrelate;
mod viewdml;
pub(crate) mod optimizer;
mod proc;
mod dml;
mod lock;
pub(crate) mod select;
pub(crate) mod window;

use crate::database::Database;
use crate::error::{Error, Result};
use crate::sql::ast::Statement;
use crate::value::Row;

/// The outcome of a statement: a result set for SELECT, a row count for DML.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    pub affected: usize,
}

impl QueryResult {
    pub(crate) fn affected(n: usize) -> Self {
        QueryResult { affected: n, ..Default::default() }
    }
}

impl Database {
    /// Runs every statement in `sql`, returning the result of the last one.
    pub fn execute(&mut self, sql: &str) -> Result<QueryResult> {
        let mut last = QueryResult::default();
        let _hint = budget::hinted_time(sql).map(budget::limit);
        for st in self.parse_cached(sql)?.iter() {
            last = self.execute_statement(st)?;
        }
        Ok(last)
    }

    /// `SET integer_division = off`: until the guard is dropped, `/` of whole numbers keeps the fraction.
    /// `SET round_half_even = off`: and `Round` sends a tie away from zero.
    /// `SET cast_truncates = off`: and `CAST(x AS INTEGER)` rounds, ties to even.
    /// `SET lenient_types = on`: and a definition read while the statement runs (a routine's) reads unknown types as text.
    fn division_guard(&self) -> (crate::numeric::DivisionGuard, crate::decimal::TiesGuard, crate::expr::func::CastGuard, crate::sql::usertypes::LenientGuard) {
        let off = |name: &str| self.settings.get(name).is_some_and(|v| v == "off");
        (
            crate::numeric::exact_whole_division(off("integer_division")),
            crate::decimal::ties_away_from_zero(off("round_half_even")),
            crate::expr::func::cast_truncates(!off("cast_truncates")),
            crate::sql::usertypes::lenient_guard(self.settings.get("lenient_types").is_some_and(|v| v == "on")),
        )
    }

    /// Runs a single read-only SELECT.
    pub fn query(&self, sql: &str) -> Result<QueryResult> {
        self.enter_session();
        let parsed = self.parse_cached(sql)?;
        self.run_read(&parsed)
    }

    /// Runs statements that [`is_pure_read`](Database::is_pure_read) has accepted (or any one SELECT).
    pub(crate) fn run_read(&self, parsed: &[Statement]) -> Result<QueryResult> {
        self.enter_session();
        let _division = self.division_guard();
        self.admit()?;
        match parsed {
            [] => Err(Error::syntax("empty statement")),
            [Statement::Select(s)] => self.run_select(s),
            [Statement::Explain(s)] => self.run_explain(s, false),
            [Statement::ExplainAnalyze(s)] => self.run_explain(s, true),
            [_] => Err(Error::runtime("query() only accepts SELECT statements")),
            _ => Err(Error::syntax("expected a single statement")),
        }
    }

    /// Runs a query and returns, instead of its rows, how it was planned.
    /// `EXPLAIN` runs the query to count the rows of each step. A plain `EXPLAIN` stops after a few seconds and
    /// shows the plan as far as it got; `EXPLAIN ANALYZE` runs to the end.
    fn run_explain(&self, sel: &crate::sql::ast::Select, analyze: bool) -> Result<QueryResult> {
        let _limit = (!analyze).then(|| budget::limit(std::time::Duration::from_secs(4)));
        let (result, mut lines) = self.collect_plan(|| self.run_select(sel));
        let result = match result {
            Err(e) if !analyze && e.to_string().contains(budget::EXPIRED) => {
                lines.push("(stopped after 4 seconds: this query is too large to run to the end for a plan; EXPLAIN ANALYZE runs it all)".to_string());
                return Ok(QueryResult { columns: vec!["plan".to_string()], rows: lines.iter().map(|l| vec![crate::value::Value::text(l)]).collect(), affected: 0 });
            }
            other => other?,
        };
        lines.push(format!("{} row(s) returned", result.rows.len()));
        Ok(QueryResult {
            columns: vec!["plan".to_string()],
            rows: lines.iter().map(|l| vec![crate::value::Value::text(l)]).collect(),
            affected: 0,
        })
    }

    pub fn execute_statement(&mut self, st: &Statement) -> Result<QueryResult> {
        let result = self.run_statement(st);
        // Numbers handed out are logged even if the statement failed or its transaction is rolled back.
        self.flush_sequences()?;
        result
    }

    fn run_statement(&mut self, st: &Statement) -> Result<QueryResult> {
        self.enter_session();
        let _timeout = self.settings.get("statement_timeout").and_then(|t| budget::parse_timeout(t)).map(budget::limit);
        let _division = self.division_guard();
        if !matches!(st, Statement::Begin | Statement::Commit | Statement::Rollback) {
            self.admit()?;
            if matches!(st, Statement::Select(_) | Statement::Insert { .. } | Statement::Update { .. } | Statement::Delete { .. } | Statement::Upsert { .. } | Statement::Returning { .. } | Statement::Exec { .. }) {
                if let Some(rewritten) = self.run_block_functions(st)? {
                    return self.run_statement(&rewritten);
                }
            }
        }
        match st {
            Statement::SetConstraints { names, deferred } => {
                self.set_constraints(names.as_deref(), *deferred).map(|_| QueryResult::default())
            }
            Statement::SetTimeZone(name) => self.set_time_zone(name).map(|_| QueryResult::default()),
            Statement::Select(s) if s.lock.is_some() => self.exec_locking_select(s),
            Statement::Select(s) => self.run_select(s),
            Statement::Insert { table, .. } | Statement::Update { table, .. } | Statement::Delete { table, .. }
                if self.table(table).is_none() && self.view_sql(table).is_some() =>
            {
                let event = match st {
                    Statement::Insert { .. } => crate::sql::ast::TriggerEvent::Insert,
                    Statement::Update { .. } => crate::sql::ast::TriggerEvent::Update,
                    _ => crate::sql::ast::TriggerEvent::Delete,
                };
                let instead = self.instead_of_triggers(table, event);
                if instead.is_empty() {
                    self.exec_view_dml(st)
                } else {
                    self.exec_instead_of(st, instead)
                }
            }
            Statement::Insert { table, columns, source } => self.exec_insert(table, columns, source),
            Statement::Update { table, assignments, filter } => self.exec_update(table, assignments, filter.as_ref()),
            Statement::Delete { table, filter } => self.exec_delete(table, filter.as_ref()),
            Statement::CreateTable { name, columns, primary_key, uniques, foreign_keys, checks } => {
                let done = self.exec_create_table(name, columns, primary_key, uniques, foreign_keys)?;
                for (cname, text) in checks {
                    if let Err(e) = self.add_check(name, cname.as_deref(), text) {
                        // The table and any checks already added go together.
                        let _ = self.exec_drop_table(name);
                        return Err(e);
                    }
                }
                Ok(done)
            }
            Statement::OrReplace(inner) => {
                let inner: Statement = (**inner).clone();
                match &inner {
                    Statement::CreateView { name, .. } => {
                        if self.table(name).is_some() {
                            return Err(Error::catalog(format!("'{name}' is a table and cannot be replaced by a query")));
                        }
                        let name = name.clone();
                        self.atomically(|db| {
                            if db.view_sql(&name).is_some() {
                                db.exec_drop_view(&name)?;
                            }
                            db.execute_statement(&inner)
                        })
                    }
                    Statement::CreateReport(def) => {
                        let name = def.name.clone();
                        self.atomically(|db| {
                            if db.reports.contains_key(&crate::database::key(&name)) {
                                db.exec_drop_report(&name)?;
                            }
                            db.execute_statement(&inner)
                        })
                    }
                    Statement::CreateRoutine(def) => {
                        let name = def.name.clone();
                        self.atomically(|db| {
                            if let Some(old) = db.routines.get(&crate::database::key(&name)).cloned() {
                                if old.function != def.function {
                                    return Err(Error::catalog(format!("'{name}' exists as a {}; drop it first", if old.function { "function" } else { "procedure" })));
                                }
                                db.exec_drop_routine(&name, old.function)?;
                            }
                            db.execute_statement(&inner)
                        })
                    }
                    Statement::CreateTrigger(def) => {
                        let name = def.name.clone();
                        self.atomically(|db| {
                            if db.trigger_sql(&name).is_some() {
                                db.exec_drop_trigger(&name)?;
                            }
                            db.execute_statement(&inner)
                        })
                    }
                    _ => Err(Error::runtime("only views, queries and triggers can be replaced")),
                }
            }
            Statement::Truncate(table) => self.exec_delete(table, None),
            Statement::Explain(sel) => self.run_explain(sel, false),
            Statement::ExplainAnalyze(sel) => self.run_explain(sel, true),
            Statement::ExplainOther(inner) => Ok(explain_other(inner)),
            Statement::Catalog(c) => {
                let result = self.exec_catalog(c)?;
                if c.kind == "role" {
                    if let (Some(value), Some(name)) = (&c.value, c.names.first()) {
                        self.add_role_memberships(name, value)?;
                    }
                }
                Ok(result)
            }
            Statement::AlterRole { name, rename, options } => self.exec_alter_role(name, rename.as_deref(), options),
            Statement::Grant(g) => self.exec_grant(g),
            Statement::Ignored(_) => Ok(QueryResult::default()),
            Statement::Handler(h) => self.exec_handler(h),
            Statement::DeleteMulti { target, from, filter } => self.exec_delete_multi(target, from, filter.as_ref()),
            Statement::UpdateMulti { from, assignments, filter } => self.exec_update_multi(from, assignments, filter.as_ref()),
            Statement::DeleteLimited { table, alias, filter, order_by, limit } => {
                self.exec_limited(table, alias.as_deref(), None, filter.as_ref(), order_by, *limit)
            }
            Statement::UpdateLimited { table, alias, assignments, filter, order_by, limit } => {
                self.exec_limited(table, alias.as_deref(), Some(assignments), filter.as_ref(), order_by, *limit)
            }
            Statement::SetVar { name, expr } => {
                let v = self.eval_const(expr)?;
                self.user_vars.insert(name.to_ascii_lowercase(), v);
                Ok(QueryResult::default())
            }
            Statement::SetParameterExpr { name, expr } => {
                let v = self.eval_const(expr)?;
                let text = if v.is_null() { String::new() } else { v.to_string() };
                self.set_parameter(name, Some(&text)).map(|()| QueryResult::default())
            }
            Statement::SelectInto { select, vars } => {
                let res = self.run_select(select)?;
                if res.rows.len() > 1 {
                    return Err(Error::runtime("Result consisted of more than one row"));
                }
                if let Some(row) = res.rows.into_iter().next() {
                    if row.len() != vars.len() {
                        return Err(Error::runtime("The used SELECT statements have a different number of columns"));
                    }
                    for (v, value) in vars.iter().zip(row) {
                        self.user_vars.insert(v.to_ascii_lowercase(), value);
                    }
                }
                Ok(QueryResult::default())
            }
            Statement::Show(show) => self.exec_show(show),
            Statement::PrepareText { name, source } => {
                let text = self.eval_const(source)?;
                let sql = text.to_string();
                let mut statements = crate::sql::parse_script(&sql)?;
                if statements.len() != 1 {
                    return Err(Error::runtime("PREPARE takes exactly one statement"));
                }
                let statement = statements.remove(0);
                self.exec_prepare(name, &statement)
            }
            Statement::SetRls { table, enable, force } => self.exec_set_rls(table, *enable, *force),
            Statement::Batch(list) => self.atomically(|db| {
                let mut last = QueryResult::default();
                for s in list {
                    last = db.execute_statement(s)?;
                }
                Ok(last)
            }),
            Statement::AttachPartition { parent, child, bound } => self.exec_attach_partition(parent, child, bound.as_deref()),
            Statement::CreateTableAs { name, query, with_data } => self.exec_create_table_as(name, query, *with_data),
            Statement::CreateTableExt(ext) => self.exec_create_table_ext(ext),
            Statement::DropTables { names, cascade, if_exists } => self.exec_drop_tables(names, *cascade, *if_exists),
            Statement::Only(inner) => {
                self.only_dml.store(true, std::sync::atomic::Ordering::Relaxed);
                let out = self.run_statement(inner);
                self.only_dml.store(false, std::sync::atomic::Ordering::Relaxed);
                out
            }
            Statement::SetRole(role) => self.exec_set_role(role.as_deref()),
            Statement::SetParameter { name, value, .. } => self.set_parameter(name, value.as_deref()).map(|()| QueryResult::default()),
            Statement::ResetParameter(name) => self.reset_parameter(name.as_deref()).map(|()| QueryResult::default()),
            Statement::ShowParameter(name) => self.show_parameter(name.as_deref()),
            Statement::Guarded { missing_ok, stmt } => match self.execute_statement(stmt) {
                Err(Error::Catalog(m))
                    if (*missing_ok && m.contains("does not exist")) || (!*missing_ok && m.contains("already exists")) =>
                {
                    Ok(QueryResult::default())
                }
                other => other,
            },
            Statement::Merge(def) => self.exec_merge(def),
            Statement::DropTable(name) => self.exec_drop_table(name),
            Statement::AlterTable { table, action } => self.exec_alter_table(table, action),
            Statement::CreateIndex { name, table, columns, descending, unique } => {
                self.exec_create_index(name, table, columns, descending, *unique)
            }
            Statement::DropIndex { name, table } => self.exec_drop_index(name, table.as_deref()),
            Statement::Reindex(name) => self.exec_reindex(name.as_deref()),
            Statement::CreateView { name, sql } => self.exec_create_view(name, sql),
            Statement::DropView(name) => self.exec_drop_view(name),
            Statement::Parameters(_) => Ok(QueryResult::default()),
            Statement::Exec { name, args } => self.exec_exec(name, args),
            Statement::Raise(_) => Err(Error::runtime("RAISE can only be used inside a trigger")),
            Statement::CreateTrigger(def) => self.exec_create_trigger(def),
            Statement::DropTrigger(name) => self.exec_drop_trigger(name),
            Statement::Upsert { insert, conflict } => self.exec_upsert(insert, conflict),
            Statement::CreateReport(def) => self.exec_create_report(def),
            Statement::DropReport(name) => self.exec_drop_report(name),
            Statement::RunReport { name, format } => self.exec_run_report(name, format),
            Statement::Returning { stmt, items } => self.exec_returning(stmt, items),
            Statement::CreateRoutine(def) => self.exec_create_routine(def),
            Statement::DropRoutine { name, function } => self.exec_drop_routine(name, *function),
            Statement::Prepare { name, statement } => self.exec_prepare(name, statement),
            Statement::Deallocate(name) => self.exec_deallocate(name),
            Statement::CreateSequence { name, options } => self.exec_create_sequence(name, options),
            Statement::AlterSequence { name, options } => self.exec_alter_sequence(name, options),
            Statement::DropSequence(name) => self.exec_drop_sequence(name),
            // as in MySQL and PostgreSQL, ending a transaction that is not there does nothing
            Statement::Commit | Statement::Rollback if !self.in_transaction() => Ok(QueryResult::default()),
            Statement::Begin => self.begin().map(|_| QueryResult::default()),
            Statement::Commit => self.commit().map(|_| QueryResult::default()),
            Statement::Rollback => self.rollback().map(|_| QueryResult::default()),
            Statement::Savepoint(name) => self.savepoint(name).map(|_| QueryResult::default()),
            Statement::ReleaseSavepoint(name) => self.release(name).map(|_| QueryResult::default()),
            Statement::RollbackTo(name) => self.rollback_to(name).map(|_| QueryResult::default()),
        }
    }
}

/// What `EXPLAIN` says of a statement that is not a query: which kind it is, and that it was not run.
fn explain_other(st: &Statement) -> QueryResult {
    let what = match st {
        Statement::Insert { table, .. } => format!("insert into {table}"),
        Statement::Update { table, .. } => format!("update {table}"),
        Statement::Delete { table, .. } => format!("delete from {table}"),
        Statement::Merge(_) => "merge".to_string(),
        Statement::Upsert { .. } => "insert with a conflict rule".to_string(),
        Statement::Returning { stmt, .. } => return explain_other(stmt),
        Statement::Guarded { stmt, .. } => return explain_other(stmt),
        Statement::Exec { name, .. } => format!("run {name}"),
        Statement::CreateTable { name, .. } => format!("create table {name}"),
        _ => "statement".to_string(),
    };
    QueryResult {
        columns: vec!["plan".to_string()],
        rows: vec![vec![crate::value::Value::text(&what)], vec![crate::value::Value::text("(not run: EXPLAIN describes only what a statement is, and runs queries)")]],
        affected: 0,
    }
}
