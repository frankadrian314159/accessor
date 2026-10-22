//! Calculated fields.
//!
//! A `CALCULATED` column holds an expression over the other fields of the same
//! record. Like Access, the engine keeps the computed value in the row so reads,
//! sorting and indexes need no special handling; what makes it a calculated
//! field is that every write to the record recomputes it, whichever path the
//! write came from (a statement, the datasheet, a cascade, a trigger). The
//! write-ahead log records the computed values, so replay never evaluates
//! anything.
//!
//! Because the value is only refreshed when its record is written, an
//! expression must give the same answer every time it is evaluated: it cannot
//! use `Now()`/`Date()`, subqueries, parameters or aggregates, and it can only
//! refer to fields of its own record.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::database::{key, Database};
use crate::error::{Error, Result};
use crate::exec::bind::{Binder, Scope, ScopeCol};
use crate::expr::BExpr;
use crate::schema::{Column, TableSchema};
use crate::sql::ast::Expr;
use crate::sql::parse_expression;
use crate::sql::walk::walk_expr;
use crate::storage::{Op, RowId};
use crate::value::{DataType, Row, Value};

/// The calculated fields of one table, in column order, ready to evaluate.
pub(crate) type CalcPlan = Arc<Vec<(usize, BExpr)>>;

/// Compiled plans by lowercase table name; cleared by every schema change.
#[derive(Default)]
pub(crate) struct CalcCache(Mutex<HashMap<String, Option<CalcPlan>>>);

impl CalcCache {
    pub(crate) fn clear(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

fn scope_of(schema: &TableSchema) -> Scope {
    Scope {
        cols: schema
            .columns
            .iter()
            .map(|c| ScopeCol { table: schema.name.clone(), name: c.name.clone() })
            .collect(),
    }
}

impl Database {
    /// True if any table has a calculated field.
    pub(crate) fn any_calculated(&self) -> bool {
        self.tables.values().any(|t| t.schema().has_calculated())
    }

    /// Checks that the expression of column `pos` can be used, by parsing and
    /// binding it against `schema` (which may describe a table not yet created).
    pub(crate) fn validate_calculated(&self, schema: &TableSchema, pos: usize) -> Result<()> {
        let col = &schema.columns[pos];
        let text = col
            .expression()
            .ok_or_else(|| Error::catalog(format!("the calculated field '{}' has no expression", col.name)))?;
        let fail = |why: String| Error::catalog(format!("the calculated field '{}': {why}", col.name));
        let mut expr = parse_expression(text).map_err(|e| {
            let hint = if text.contains("&&") { " (to join text use a single &)" } else { "" };
            fail(format!("the formula is not valid: {e}{hint}"))
        })?;

        let mut problem: Option<String> = None;
        walk_expr(&mut expr, &mut |e| {
            let found = match e {
                Expr::Param(p) => Some(format!("@{p} is a parameter; expressions can only use fields")),
                Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSelect { .. } => {
                    Some("an expression cannot contain a query; it may only use fields of the same record".to_string())
                }
                Expr::Function { name, .. } if ["NOW", "DATE", "TODAY"].iter().any(|v| name.eq_ignore_ascii_case(v)) => {
                    Some(format!(
                        "{name}() changes over time, but a calculated value is only recomputed when its record is saved"
                    ))
                }
                Expr::Column { table, name } => {
                    if table.as_deref().is_some_and(|t| !t.eq_ignore_ascii_case(&schema.name)) {
                        Some(format!("'{}.{name}' is not a field of this record", table.as_deref().unwrap_or("")))
                    } else {
                        match schema.column_index(name) {
                            Some(i) if i == pos => Some("an expression cannot use its own field".to_string()),
                            Some(i) if i > pos && schema.columns[i].is_calculated() => Some(format!(
                                "it uses the calculated field '{name}', which comes later; define it after '{name}'"
                            )),
                            _ => None,
                        }
                    }
                }
                _ => None,
            };
            if problem.is_none() {
                problem = found;
            }
            Ok(())
        })?;
        if let Some(p) = problem {
            return Err(fail(p));
        }
        let scope = scope_of(schema);
        Binder::new(self, &scope, 0).bind(&expr).map_err(|e| fail(e.to_string()))?;
        Ok(())
    }

    /// Checks a formula for a calculated field `field` of `table` without changing
    /// anything: the same test that adding or changing the field applies. Meant for
    /// showing a precise message while the formula is being typed.
    pub fn check_formula(&self, table: &str, field: &str, formula: &str) -> Result<()> {
        self.prospective_schema(table, field, formula, false).map(|_| ())
    }

    /// Like [`check_formula`](Self::check_formula), for turning the existing
    /// ordinary field `field` into a calculated one (its values are replaced).
    pub fn check_conversion(&self, table: &str, field: &str, formula: &str) -> Result<()> {
        let exists = self.table_checked(table)?.schema().column_index(field).is_some();
        if !exists {
            return Err(Error::catalog(format!("the field '{field}' does not exist in table '{table}'")));
        }
        self.prospective_schema(table, field, formula, true).map(|_| ())
    }

    /// The table as it would be with calculated field `field`: new, or with a new
    /// formula, or (with `convert`) made out of an existing ordinary field.
    /// Fails unless every calculated field in the result is valid.
    pub(crate) fn prospective_schema(&self, table: &str, field: &str, formula: &str, convert: bool) -> Result<TableSchema> {
        let mut schema = self.table_checked(table)?.schema().clone();
        let field = if field.trim().is_empty() { "new field" } else { field.trim() };
        let mut col = Column::new(field, DataType::Calculated);
        col.default = Some(Value::text(formula));
        match schema.column_index(field) {
            Some(i) if schema.columns[i].is_calculated() => schema.columns[i] = col,
            Some(i) if convert => {
                let old = &schema.columns[i];
                if old.primary_key || old.autonumber {
                    return Err(Error::catalog(format!(
                        "the field '{field}' is a key or an AutoNumber and cannot become a calculated field"
                    )));
                }
                self.guard_column_change(table, field, "made calculated")?;
                schema.columns[i] = col;
            }
            Some(_) => return Err(Error::catalog(format!("the table already has a field named '{field}'"))),
            None => schema.columns.push(col),
        }
        // A new formula can break the order other calculated fields rely on.
        for i in 0..schema.columns.len() {
            if schema.columns[i].is_calculated() {
                self.validate_calculated(&schema, i)?;
            }
        }
        Ok(schema)
    }

    /// The compiled calculated fields of `table`, or `None` if it has none.
    pub(crate) fn calc_plan(&self, table: &str) -> Result<Option<CalcPlan>> {
        let k = key(table);
        if let Some(hit) = self.calc_cache.0.lock().unwrap_or_else(|e| e.into_inner()).get(&k) {
            return Ok(hit.clone());
        }
        let schema = self.table_checked(table)?.schema();
        let plan = if schema.has_calculated() {
            let scope = scope_of(schema);
            let mut steps = Vec::new();
            for (i, c) in schema.columns.iter().enumerate() {
                if let Some(text) = c.expression() {
                    let bound = Binder::new(self, &scope, 0).bind(&parse_expression(text)?)?;
                    steps.push((i, bound));
                }
            }
            Some(Arc::new(steps))
        } else {
            None
        };
        self.calc_cache.0.lock().unwrap_or_else(|e| e.into_inner()).insert(k, plan.clone());
        Ok(plan)
    }

    /// Fills in the calculated fields of `row`, which belongs to `table`.
    fn calculate_row(&self, plan: &CalcPlan, row: &mut Row) -> Result<()> {
        for (pos, expr) in plan.iter() {
            let v = expr.eval(row)?.into_owned();
            row[*pos] = v;
        }
        Ok(())
    }

    /// Recomputes the calculated fields of a row-writing operation, in place.
    pub(crate) fn finalize_op(&self, op: &mut Op) -> Result<()> {
        if let Op::Insert { table, row, .. } | Op::Update { table, row, .. } = op {
            if let Some(plan) = self.calc_plan(table)? {
                self.calculate_row(&plan, row).map_err(|e| {
                    Error::runtime(format!("cannot compute a calculated field of '{table}': {e}"))
                })?;
            }
        }
        Ok(())
    }

    /// Recomputes every stored row of `table`. Nothing changes if any row fails.
    pub(crate) fn recalculate_table(&mut self, table: &str) -> Result<()> {
        let Some(plan) = self.calc_plan(table)? else { return Ok(()) };
        let t = self.table_checked(table)?;
        let mut updates: Vec<(RowId, Row)> = Vec::with_capacity(t.len());
        for (id, row) in t.iter() {
            let mut row = row.clone();
            self.calculate_row(&plan, &mut row)
                .map_err(|e| Error::runtime(format!("cannot compute a calculated field of '{table}': {e}")))?;
            updates.push((id, row));
        }
        let t = self.table_mut(table)?;
        for (id, row) in updates {
            t.replace(id, row)?;
        }
        Ok(())
    }

    /// Refuses to delete, rename or retype a field that a calculated field uses.
    pub(crate) fn guard_calculated_dependency(&self, table: &str, column: &str, verb: &str) -> Result<()> {
        let t = self.table_checked(table)?;
        for c in &t.schema().columns {
            let Some(text) = c.expression() else { continue };
            if c.name.eq_ignore_ascii_case(column) {
                continue;
            }
            let mut uses = false;
            walk_expr(&mut parse_expression(text)?, &mut |e| {
                if let Expr::Column { name, .. } = e {
                    uses |= name.eq_ignore_ascii_case(column);
                }
                Ok(())
            })?;
            if uses {
                return Err(Error::catalog(format!(
                    "the field '{column}' cannot be {verb} because the calculated field '{}' uses it",
                    c.name
                )));
            }
        }
        Ok(())
    }

    /// Value used when a calculated field is written to directly.
    pub(crate) fn calculated_write_error(table: &str, column: &str) -> Error {
        Error::constraint(format!(
            "the field '{column}' of '{table}' is calculated; its value comes from its expression and cannot be set"
        ))
    }
}
