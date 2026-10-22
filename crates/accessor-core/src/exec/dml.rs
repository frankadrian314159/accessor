//! INSERT / UPDATE / DELETE, plus the cell-level API used by the GUI.
//!
//! Each statement is planned against an immutable view of the database into
//! a list of [`Op`]s, which are then applied atomically.

use super::bind::{Binder, Scope, ScopeCol};
use super::select::{matching_ids, split_and};
use super::QueryResult;
use crate::database::Database;
use crate::error::{Error, Result};
use crate::expr::BExpr;
use crate::schema::TableSchema;
use crate::sql::ast::{Expr, InsertSource};
use crate::storage::{Op, RowId};
use crate::value::{Row, Value};

/// Builds a full row from explicitly supplied values: defaults first, then
/// the supplied values, then type coercion and AutoNumber assignment.
pub(crate) fn build_row(schema: &TableSchema, positions: &[usize], vals: Vec<Value>, next_auto: &mut i64) -> Result<Row> {
    let mut row: Row = schema
        .columns
        .iter()
        .map(|c| if c.is_calculated() { Value::Null } else { c.default.clone().unwrap_or(Value::Null) })
        .collect();
    for (&p, v) in positions.iter().zip(vals) {
        if schema.columns[p].is_calculated() {
            return Err(Database::calculated_write_error(&schema.name, &schema.columns[p].name));
        }
        row[p] = v;
    }
    let mut row = schema.coerce_row(row)?;
    if let Some(a) = schema.autonumber_column() {
        match row[a] {
            Value::Null => {
                row[a] = Value::Int(*next_auto);
                *next_auto += 1;
            }
            Value::Int(n) => *next_auto = (*next_auto).max(n.saturating_add(1)),
            _ => {}
        }
    }
    Ok(row)
}

fn table_scope(schema: &TableSchema) -> Scope {
    Scope {
        cols: schema
            .columns
            .iter()
            .map(|c| ScopeCol { table: schema.name.clone(), name: c.name.clone() })
            .collect(),
    }
}

fn column_pos(schema: &TableSchema, name: &str) -> Result<usize> {
    schema
        .column_index(name)
        .ok_or_else(|| Error::catalog(format!("the field '{name}' does not exist in table '{}'", schema.name)))
}

impl Database {
    pub(crate) fn exec_insert(
        &mut self,
        table: &str,
        columns: &Option<Vec<String>>,
        source: &InsertSource,
    ) -> Result<QueryResult> {
        let ops = self.plan_insert(table, columns, source)?;
        let n = ops.len();
        self.apply_all(ops)?;
        Ok(QueryResult::affected(n))
    }

    pub(crate) fn exec_update(
        &mut self,
        table: &str,
        assignments: &[(String, Expr)],
        filter: Option<&Expr>,
    ) -> Result<QueryResult> {
        let ops = self.plan_update(table, assignments, filter)?;
        let n = ops.len();
        self.apply_all(ops)?;
        Ok(QueryResult::affected(n))
    }

    pub(crate) fn exec_delete(&mut self, table: &str, filter: Option<&Expr>) -> Result<QueryResult> {
        let ops = self.plan_delete(table, filter)?;
        let n = ops.len();
        self.apply_all(ops)?;
        Ok(QueryResult::affected(n))
    }

    pub(crate) fn plan_insert(
        &self,
        table: &str,
        columns: &Option<Vec<String>>,
        source: &InsertSource,
    ) -> Result<Vec<Op>> {
        let ops = {
            let t = self.table_checked(table)?;
            let schema = t.schema();
            let positions: Vec<usize> = match columns {
                Some(cols) => cols.iter().map(|c| column_pos(schema, c)).collect::<Result<_>>()?,
                // Without a field list, the values are for the fields you can set.
                None => (0..schema.columns.len()).filter(|&i| !schema.columns[i].is_calculated()).collect(),
            };
            let value_rows: Vec<Vec<Value>> = match source {
                InsertSource::Values(rows) => {
                    let scope = Scope::default();
                    let mut binder = Binder::new(self, &scope, 0);
                    let mut out = Vec::with_capacity(rows.len());
                    for r in rows {
                        if r.len() != positions.len() {
                            return Err(Error::runtime(format!(
                                "INSERT supplies {} values for {} columns",
                                r.len(),
                                positions.len()
                            )));
                        }
                        let mut vals = Vec::with_capacity(r.len());
                        for e in r {
                            vals.push(binder.bind(e)?.eval(&[])?.into_owned());
                        }
                        out.push(vals);
                    }
                    out
                }
                InsertSource::Select(sel) => {
                    let res = self.run_select(sel)?;
                    if res.columns.len() != positions.len() {
                        return Err(Error::runtime(format!(
                            "INSERT ... SELECT returns {} columns for {} targets",
                            res.columns.len(),
                            positions.len()
                        )));
                    }
                    res.rows
                }
            };
            let mut next_auto = t.next_auto();
            let base = t.slot_count();
            let mut ops = Vec::with_capacity(value_rows.len());
            for (k, vals) in value_rows.into_iter().enumerate() {
                let row = build_row(schema, &positions, vals, &mut next_auto)?;
                ops.push(Op::Insert { table: schema.name.clone(), id: (base + k) as RowId, row });
            }
            ops
        };
        Ok(ops)
    }

    pub(crate) fn plan_update(
        &self,
        table: &str,
        assignments: &[(String, Expr)],
        filter: Option<&Expr>,
    ) -> Result<Vec<Op>> {
        let ops = {
            let t = self.table_checked(table)?;
            let schema = t.schema();
            let scope = table_scope(schema);
            let mut binder = Binder::new(self, &scope, 0);
            let sets: Vec<(usize, BExpr)> = assignments
                .iter()
                .map(|(c, e)| Ok((column_pos(schema, c)?, binder.bind(e)?)))
                .collect::<Result<_>>()?;
            if let Some((p, _)) = sets.iter().find(|(p, _)| schema.columns[*p].is_calculated()) {
                return Err(Database::calculated_write_error(&schema.name, &schema.columns[*p].name));
            }
            let f = filter.map(|f| binder.bind(f)).transpose()?;
            let conj = f.as_ref().map(split_and).unwrap_or_default();
            let mut ops = Vec::new();
            for id in matching_ids(t, &conj, f.as_ref())? {
                let old = t.get(id).expect("id from live scan");
                let mut new = old.clone();
                for (c, e) in &sets {
                    new[*c] = e.eval(old)?.into_owned();
                }
                ops.push(Op::Update { table: schema.name.clone(), id, row: schema.coerce_row(new)? });
            }
            ops
        };
        Ok(ops)
    }

    pub(crate) fn plan_delete(&self, table: &str, filter: Option<&Expr>) -> Result<Vec<Op>> {
        let ops = {
            let t = self.table_checked(table)?;
            let schema = t.schema();
            let scope = table_scope(schema);
            let mut binder = Binder::new(self, &scope, 0);
            let f = filter.map(|f| binder.bind(f)).transpose()?;
            let conj = f.as_ref().map(split_and).unwrap_or_default();
            matching_ids(t, &conj, f.as_ref())?
                .into_iter()
                .map(|id| Op::Delete { table: schema.name.clone(), id })
                .collect::<Vec<_>>()
        };
        Ok(ops)
    }

    // ----- cell-level API (datasheet editing) -----------------------------------

    /// Inserts a row from `(column position, value)` pairs; other columns get
    /// their defaults. Returns the new row id.
    pub fn insert_row(&mut self, table: &str, values: &[(usize, Value)]) -> Result<RowId> {
        let op = {
            let t = self.table_checked(table)?;
            let schema = t.schema();
            if let Some(&(p, _)) = values.iter().find(|(p, _)| *p >= schema.columns.len()) {
                return Err(Error::runtime(format!("column position {p} is out of range")));
            }
            let positions: Vec<usize> = values.iter().map(|(p, _)| *p).collect();
            let vals: Vec<Value> = values.iter().map(|(_, v)| v.clone()).collect();
            let mut next_auto = t.next_auto();
            let id = t.slot_count() as RowId;
            Op::Insert { table: schema.name.clone(), id, row: build_row(schema, &positions, vals, &mut next_auto)? }
        };
        let id = match &op {
            Op::Insert { id, .. } => *id,
            _ => unreachable!(),
        };
        self.apply_all(vec![op])?;
        Ok(id)
    }

    /// Sets one cell, coercing the value to the column's type.
    pub fn update_cell(&mut self, table: &str, id: RowId, col: usize, value: Value) -> Result<()> {
        let op = {
            let t = self.table_checked(table)?;
            let schema = t.schema();
            if col >= schema.columns.len() {
                return Err(Error::runtime(format!("column position {col} is out of range")));
            }
            if schema.columns[col].is_calculated() {
                return Err(Database::calculated_write_error(&schema.name, &schema.columns[col].name));
            }
            let mut row = t
                .get(id)
                .ok_or_else(|| Error::runtime("that record no longer exists"))?
                .clone();
            row[col] = value;
            Op::Update { table: schema.name.clone(), id, row: schema.coerce_row(row)? }
        };
        self.apply_all(vec![op])
    }

    pub fn delete_row(&mut self, table: &str, id: RowId) -> Result<()> {
        let name = self.table_checked(table)?.name().to_string();
        self.apply_all(vec![Op::Delete { table: name, id }])
    }
}
