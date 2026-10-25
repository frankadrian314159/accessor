//! `CREATE REPORT`, `RUN REPORT` and rendering a stored report.

use super::QueryResult;
use crate::database::{key, Database};
use crate::error::{Error, Result};
use crate::report::{self, ReportFormat};
use crate::sql::ast::{Expr, OrderItem, ReportDef};
use crate::value::Value;

impl Database {
    pub(crate) fn exec_create_report(&mut self, def: &ReportDef) -> Result<QueryResult> {
        if self.reports.contains_key(&key(&def.name)) {
            return Err(Error::catalog(format!("the report '{}' already exists", def.name)));
        }
        // The query must run, and the layout must fit what it returns.
        let mut probe = def.query.clone();
        probe.limit = Some(0);
        let columns = self.unlogged(|| self.run_select(&probe))?.columns;
        report::validate(&columns, &def.layout)?;
        self.apply_all(vec![crate::storage::Op::CreateReport { name: def.name.clone(), sql: def.sql.clone() }])?;
        Ok(QueryResult::default())
    }

    pub(crate) fn exec_drop_report(&mut self, name: &str) -> Result<QueryResult> {
        let real = self
            .reports
            .get(&key(name))
            .map(|r| r.name.clone())
            .ok_or_else(|| Error::catalog(format!("the report '{name}' does not exist")))?;
        self.apply_all(vec![crate::storage::Op::DropReport(real)])?;
        Ok(QueryResult::default())
    }

    /// Renders a stored report as a document of the given kind. The bytes are UTF-8 text except for
    /// [`ReportFormat::Pdf`].
    pub fn render_report(&self, name: &str, format: ReportFormat) -> Result<Vec<u8>> {
        self.enter_session();
        let def = self
            .reports
            .get(&key(name))
            .cloned()
            .ok_or_else(|| Error::catalog(format!("the report '{name}' does not exist")))?;
        let mut query = def.query.clone();
        // Groups are runs of equal values, so the rows must come in that order; asking the query to
        // sort lets it use an index for it.
        if query.order_by.is_empty() && query.unions.is_empty() && query.crosstab.is_none() {
            query.order_by = def
                .layout
                .breaks
                .iter()
                .map(|b| OrderItem { expr: Expr::Column { table: None, name: b.column.clone() }, desc: false, nulls_first: None })
                .collect();
        }
        let result = self.run_select(&query)?;
        report::render(&result, &def.name, &def.layout, format)
    }

    pub(crate) fn exec_run_report(&mut self, name: &str, format: &str) -> Result<QueryResult> {
        let fmt = ReportFormat::parse(format)
            .ok_or_else(|| Error::runtime(format!("'{format}' is not a report format (TEXT, HTML, MARKDOWN, CSV or PDF)")))?;
        if fmt == ReportFormat::Pdf {
            return Err(Error::runtime("a PDF is not text; use Database::render_report or the shell's .report command to write it to a file"));
        }
        let bytes = self.render_report(name, fmt)?;
        let text = String::from_utf8(bytes).map_err(|_| Error::runtime("the report is not text"))?;
        Ok(QueryResult { columns: vec!["report".into()], rows: vec![vec![Value::text(&text)]], affected: 0 })
    }
}
