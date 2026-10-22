//! `CHECK` constraints.
//!
//! A check is stored as a pair of `BEFORE INSERT` and `BEFORE UPDATE` triggers
//! (`name_ins` and `name_upd`) that refuse the row when the condition is false.
//! That reuses the trigger machinery, so a check is enforced for every write:
//! statements, cascades, the datasheet, and transactions that are rolled back.
//! As in SQL, a condition that is unknown (NULL) passes.

use super::bind::{Binder, Scope, ScopeCol};
use crate::database::Database;
use crate::error::{Error, Result};
use crate::sql::ast::Expr;
use crate::sql::parser::qualify_columns;
use crate::sql::walk::walk_expr;
use crate::sql::parse_expression;

impl Database {
    /// Adds a `CHECK (expr)` to `table`, after checking that existing records satisfy it.
    pub(crate) fn add_check(&mut self, table: &str, name: Option<&str>, expr: &str) -> Result<()> {
        let t = self.table_checked(table)?;
        let real = t.name().to_string();
        let schema = t.schema().clone();

        let mut parsed = parse_expression(expr)?;
        let mut unsupported = None;
        walk_expr(&mut parsed, &mut |e| {
            match e {
                Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSelect { .. } => {
                    unsupported = Some("a CHECK condition cannot contain a query")
                }
                Expr::Param(_) => unsupported = Some("a CHECK condition cannot contain a parameter"),
                Expr::Window { .. } => unsupported = Some("a CHECK condition cannot contain a window function"),
                _ => {}
            }
            Ok(())
        })?;
        if let Some(msg) = unsupported {
            return Err(Error::syntax(msg));
        }

        // Every existing record must pass.
        let scope = Scope {
            cols: schema
                .columns
                .iter()
                .map(|c| ScopeCol { table: real.clone(), name: c.name.clone(), hidden: false })
                .collect(),
        };
        let bound = Binder::new(self, &scope, 0).bind(&parsed)?;
        let mut failing = 0usize;
        for (_, row) in self.table_checked(&real)?.iter() {
            if bound.eval_tri(row)? == Some(false) {
                failing += 1;
            }
        }
        if failing > 0 {
            return Err(Error::constraint(format!(
                "cannot add the CHECK constraint: {failing} record(s) in '{real}' do not satisfy it"
            )));
        }

        let base = match name {
            Some(n) => n.to_string(),
            None => (1..)
                .map(|k| format!("ck_{real}_{k}"))
                .find(|n| self.trigger_sql(&format!("{n}_ins")).is_none())
                .expect("an unused name exists"),
        };
        let rewritten = qualify_columns(expr, "NEW", &|c| schema.column_index(c).is_some())?;
        let message = format!("CHECK constraint failed on '{real}': {expr}").replace('\'', "''");
        let make = |suffix: &str, event: &str| {
            format!("CREATE TRIGGER [{base}_{suffix}] BEFORE {event} ON [{real}] WHEN NOT ({rewritten}) BEGIN RAISE '{message}'; END")
        };
        self.execute(&make("ins", "INSERT"))?;
        if let Err(e) = self.execute(&make("upd", "UPDATE")) {
            let _ = self.execute(&format!("DROP TRIGGER [{base}_ins]"));
            return Err(e);
        }
        Ok(())
    }
}
