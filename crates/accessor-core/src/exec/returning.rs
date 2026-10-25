//! `RETURNING`: the rows a data change produced, without a second query.
//!
//! The statement runs as usual; every change it makes to its table is noted (see
//! [`Capture`](crate::database::Capture)), and the `RETURNING` expressions are evaluated on the
//! rows as they ended up: after defaults, AutoNumbers, calculated fields and `BEFORE`/`AFTER`
//! triggers for an insert or update, and as they were for a delete.

use std::collections::BTreeMap;

use super::bind::{Binder, Scope, ScopeCol};
use super::select::expand_items;
use super::QueryResult;
use crate::database::{key, Capture, Database};
use crate::error::{Error, Result};
use crate::sql::ast::*;

impl Database {
    pub(crate) fn exec_returning(&mut self, stmt: &Statement, items: &[SelectItem]) -> Result<QueryResult> {
        let target = match stmt {
            Statement::Insert { table, .. } | Statement::Update { table, .. } | Statement::Delete { table, .. } => table.clone(),
            Statement::Merge(m) => m.target.clone(),
            _ => return Err(Error::runtime("RETURNING follows INSERT, UPDATE, DELETE or MERGE")),
        };
        let Some(table) = self.table(&target) else {
            return Err(Error::catalog(format!("RETURNING works on tables, and '{target}' is not one (it may be a query)")));
        };
        let real = table.name().to_string();
        let scope = Scope {
            cols: table
                .schema()
                .columns
                .iter()
                .map(|c| ScopeCol { table: real.clone(), name: c.name.clone(), hidden: false })
                .collect(),
        };
        // Check the expressions before anything is changed.
        let pseudo = Select { items: items.to_vec(), ..Select::default() };
        let named = expand_items(&pseudo, &scope)?;
        let bound = {
            let mut binder = Binder::new(self, &scope, 0);
            named.iter().map(|(_, e)| binder.bind(e)).collect::<Result<Vec<_>>>()?
        };

        self.capture = Some(Capture { table: key(&real), changes: BTreeMap::new() });
        let done = self.execute_statement(stmt);
        let captured = self.capture.take();
        let done = done?;

        let mut rows = Vec::new();
        for (_, (old, new)) in captured.map(|c| c.changes).unwrap_or_default() {
            let Some(row) = new.or(old) else { continue };
            rows.push(bound.iter().map(|e| e.eval(&row).map(|v| v.into_owned())).collect::<Result<Vec<_>>>()?);
        }
        Ok(QueryResult { columns: named.into_iter().map(|(n, _)| n).collect(), affected: rows.len().max(done.affected), rows })
    }
}
