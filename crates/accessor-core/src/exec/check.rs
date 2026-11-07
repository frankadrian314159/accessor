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

/// The start of the text that stands for an `EXCLUDE` constraint where a check is expected: the elements and their
/// operators (each `element \u{3} operator \u{2}`), then `\u{4}` and the `WHERE` condition if there is one.
pub(crate) const EXCLUDE_PREFIX: &str = "\u{1}exclude:";

impl Database {
    /// The `CHECK` constraints of a table, as `(name, condition)`: they are kept as the trigger pairs `add_check` makes,
    /// whose message holds the condition as it was written.
    pub(crate) fn check_constraints(&self, table: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for def in self.triggers.values() {
            let marker = format!("CHECK constraint failed on ''{}'': ", def.table);
            if !def.table.eq_ignore_ascii_case(table) || !def.name.ends_with("_ins") {
                continue;
            }
            let Some(at) = def.sql.find(&marker) else { continue };
            let rest = &def.sql[at + marker.len()..];
            let Some(end) = rest.rfind("'; END") else { continue };
            out.push((def.name[..def.name.len() - 4].to_string(), rest[..end].replace("''", "'")));
        }
        out.sort();
        out
    }

    /// An `EXCLUDE` constraint: no two rows may satisfy every `element operator element` at once. It is a pair of
    /// triggers like a check, which look for a conflicting row.
    fn add_exclusion(&mut self, table: &str, name: Option<&str>, spec: &str) -> Result<()> {
        // only PostgreSQL has the constraint (and the operators it is made of); elsewhere it is read and ignored
        if crate::dialect::current() != crate::dialect::Dialect::Postgresql {
            return Ok(());
        }
        let real = self.table_checked(table)?.name().to_string();
        let schema = self.table_checked(table)?.schema().clone();
        let (elements, filter) = match spec.split_once('\u{4}') {
            Some((e, w)) => (e, Some(w)),
            None => (spec, None),
        };
        let is_col = |c: &str| schema.column_index(c).is_some();
        let mut pairs = Vec::new();
        let mut first = String::new();
        for part in elements.split('\u{2}').filter(|p| !p.is_empty()) {
            let (element, op) = part.split_once('\u{3}').ok_or_else(|| Error::syntax("an EXCLUDE element needs an operator"))?;
            if first.is_empty() {
                first = element.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect();
            }
            // an operator other than `=` is only known for the range types; for a type the engine lacks (a box, a circle)
            // the constraint is read and ignored
            if op.trim() != "=" && crate::expr::ranges::column_type(&real, element.trim()).is_none() {
                return Ok(());
            }
            let (a, b) = (qualify_columns(element, "__o", &is_col)?, qualify_columns(element, "NEW", &is_col)?);
            pairs.push(format!("{a} {op} {b}"));
        }
        if pairs.is_empty() {
            return Err(Error::syntax("an EXCLUDE constraint needs an element"));
        }
        let mut outer = String::new();
        if let Some(w) = filter {
            pairs.push(qualify_columns(w, "__o", &is_col)?);
            outer = format!("({}) AND ", qualify_columns(w, "NEW", &is_col)?);
        }
        let base = match name {
            Some(n) => n.to_string(),
            None => format!("{real}_{first}_excl"),
        };
        let same_row = schema.columns.iter().map(|c| format!("__o.[{0}] IS NOT DISTINCT FROM OLD.[{0}]", c.name)).collect::<Vec<_>>().join(" AND ");
        let message = format!("conflicting key value violates exclusion constraint \"{base}\"").replace('\'', "''");
        let conflict = pairs.join(" AND ");
        let make = |suffix: &str, event: &str, other: &str| {
            format!(
                "CREATE TRIGGER [{base}_{suffix}] BEFORE {event} ON [{real}] WHEN {outer}EXISTS (SELECT 1 FROM [{real}] __o WHERE {conflict}{other}) BEGIN RAISE '{message}'; END"
            )
        };
        self.execute(&make("xins", "INSERT", ""))?;
        if let Err(e) = self.execute(&make("xupd", "UPDATE", &format!(" AND NOT ({same_row})"))) {
            let _ = self.execute(&format!("DROP TRIGGER [{base}_xins]"));
            return Err(e);
        }
        Ok(())
    }

    /// Adds a `CHECK (expr)` to `table`, after checking that existing records satisfy it.
    pub(crate) fn add_check(&mut self, table: &str, name: Option<&str>, expr: &str) -> Result<()> {
        if let Some(spec) = expr.strip_prefix(EXCLUDE_PREFIX) {
            return self.add_exclusion(table, name, spec);
        }
        let t = self.table_checked(table)?;
        let real = t.name().to_string();
        let schema = t.schema().clone();

        // MySQL: DEFAULT(col) is the column's default value
        let expr = &default_calls(expr, &schema);
        // PostgreSQL: tableoid is the table the row is in, which a condition on the table knows
        let expr = &if schema.column_index("tableoid").is_none() {
            let literal = format!("'{}'", real.replace('\'', "''"));
            regex::Regex::new(r"(?i)\btableoid\b").expect("a valid pattern").replace_all(expr, regex::NoExpand(&literal)).into_owned()
        } else {
            expr.to_string()
        };
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
            invisible: Vec::new(),
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
