//! Statement execution.

pub mod agg;
pub mod bind;
mod ddl;
mod proc;
mod dml;
pub(crate) mod select;

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
        match parse_statement(sql)? {
            Statement::Select(s) => self.run_select(&s),
            _ => Err(Error::runtime("query() only accepts SELECT statements")),
        }
    }

    pub fn execute_statement(&mut self, st: &Statement) -> Result<QueryResult> {
        match st {
            Statement::Select(s) => self.run_select(s),
            Statement::Insert { table, columns, source } => self.exec_insert(table, columns, source),
            Statement::Update { table, assignments, filter } => self.exec_update(table, assignments, filter.as_ref()),
            Statement::Delete { table, filter } => self.exec_delete(table, filter.as_ref()),
            Statement::CreateTable { name, columns, primary_key, uniques, foreign_keys } => {
                self.exec_create_table(name, columns, primary_key, uniques, foreign_keys)
            }
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
        }
    }
}
