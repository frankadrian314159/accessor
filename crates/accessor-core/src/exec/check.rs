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

        // MySQL: DEFAULT(col) is the column's default value
        let expr = &default_calls(expr, &schema);
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
            // constraint names belong to a table, trigger names to the database: a second table with a constraint of
            // the same name gets the table's name in front
            Some(n) if self.trigger_sql(&format!("{n}_ins")).is_some() => format!("{real}__{n}"),
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

    /// Dropping a column drops the CHECK constraints (the trigger pairs of `add_check`) that mention it, as PostgreSQL does.
    pub(crate) fn drop_checks_on_column(&mut self, table: &str, column: &str) -> Result<()> {
        let mut names: Vec<String> = Vec::new();
        for (key, def) in &self.triggers {
            if !def.table.eq_ignore_ascii_case(table) || !def.sql.contains("CHECK constraint failed on") {
                continue;
            }
            let mut mentions = false;
            if let Some(when) = &def.when {
                let mut w = when.clone();
                let _ = walk_expr(&mut w, &mut |e| {
                    if let Expr::Column { table: Some(t), name } = e {
                        mentions |= t.eq_ignore_ascii_case("NEW") && name.eq_ignore_ascii_case(column);
                    }
                    Ok(())
                });
            }
            if mentions {
                names.push(key.clone());
            }
        }
        for key in names {
            let _ = self.execute(&format!("DROP TRIGGER IF EXISTS [{key}]"));
        }
        Ok(())
    }
}

/// `expr` with each `DEFAULT(col)` replaced by the default value of the column, as a literal (NULL when it has none).
fn default_calls(expr: &str, schema: &crate::schema::TableSchema) -> String {
    use crate::value::Value;
    let re = regex::Regex::new(r#"(?i)\bdefault\s*\(\s*[\[`"]?(\w+)[\]`"]?\s*\)"#).expect("a valid pattern");
    re.replace_all(expr, |caps: &regex::Captures| {
        let Some(i) = schema.column_index(&caps[1]) else { return caps[0].to_string() };
        match &schema.columns[i].default {
            None | Some(Value::Null) => "NULL".to_string(),
            Some(Value::Text(t)) => format!("'{}'", t.replace('\'', "''")),
            Some(v @ (Value::Int(_) | Value::Big(_) | Value::Dec(_) | Value::Float(_))) => v.to_string(),
            Some(other) => format!("'{}'", other.to_string().replace('\'', "''")),
        }
    })
    .into_owned()
}
