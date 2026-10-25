//! Statement execution.

pub mod agg;
pub mod bind;
mod check;
mod ddl;
mod merge;
pub(crate) mod optimizer;
mod proc;
mod dml;
pub(crate) mod select;
pub(crate) mod window;

use crate::database::Database;
use crate::error::{Error, Result};
use crate::sql::ast::Statement;
use crate::sql::{parse_script, parse_statement};
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
        for st in parse_script(sql)? {
            last = self.execute_statement(&st)?;
        }
        Ok(last)
    }

    /// Runs a single read-only SELECT.
    pub fn query(&self, sql: &str) -> Result<QueryResult> {
        self.enter_session();
        match parse_statement(sql)? {
            Statement::Select(s) => self.run_select(&s),
            Statement::Explain(s) => self.run_explain(&s),
            _ => Err(Error::runtime("query() only accepts SELECT statements")),
        }
    }

    /// Runs a query and returns, instead of its rows, how it was planned.
    fn run_explain(&self, sel: &crate::sql::ast::Select) -> Result<QueryResult> {
        let (result, mut lines) = self.collect_plan(|| self.run_select(sel));
        let result = result?;
        lines.push(format!("{} row(s) returned", result.rows.len()));
        Ok(QueryResult {
            columns: vec!["plan".to_string()],
            rows: lines.iter().map(|l| vec![crate::value::Value::text(l)]).collect(),
            affected: 0,
        })
    }

    pub fn execute_statement(&mut self, st: &Statement) -> Result<QueryResult> {
        self.enter_session();
        match st {
            Statement::SetTimeZone(name) => self.set_time_zone(name).map(|_| QueryResult::default()),
            Statement::Select(s) => self.run_select(s),
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
            Statement::Explain(sel) => self.run_explain(sel),
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
            Statement::CreateIndex { name, table, columns, unique } => {
                self.exec_create_index(name, table, columns, *unique)
            }
            Statement::DropIndex { name, table } => self.exec_drop_index(name, table),
            Statement::CreateView { name, sql } => self.exec_create_view(name, sql),
            Statement::DropView(name) => self.exec_drop_view(name),
            Statement::Parameters(_) => Ok(QueryResult::default()),
            Statement::Exec { name, args } => self.exec_exec(name, args),
            Statement::Raise(_) => Err(Error::runtime("RAISE can only be used inside a trigger")),
            Statement::CreateTrigger(def) => self.exec_create_trigger(def),
            Statement::DropTrigger(name) => self.exec_drop_trigger(name),
            Statement::Begin => self.begin().map(|_| QueryResult::default()),
            Statement::Commit => self.commit().map(|_| QueryResult::default()),
            Statement::Rollback => self.rollback().map(|_| QueryResult::default()),
            Statement::Savepoint(name) => self.savepoint(name).map(|_| QueryResult::default()),
            Statement::ReleaseSavepoint(name) => self.release(name).map(|_| QueryResult::default()),
            Statement::RollbackTo(name) => self.rollback_to(name).map(|_| QueryResult::default()),
        }
    }
}
