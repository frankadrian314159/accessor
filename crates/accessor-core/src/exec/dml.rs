//! INSERT / UPDATE / DELETE, plus the cell-level API used by the GUI.
//!
//! Each statement is planned against an immutable view of the database into
//! a list of [`Op`]s, which are then applied atomically.

use super::bind::{Binder, CorrSpec, Scope, ScopeCol};
use super::select::{matching_ids, split_and, split_late};
use super::QueryResult;
use crate::database::Database;
use crate::error::{Error, Result};
use crate::expr::BExpr;
use crate::schema::TableSchema;
use crate::sql::ast::{Conflict, ConflictAction, Expr, InsertSource, Statement};
use crate::sql::walk::RowContext;
use crate::storage::{Op, RowId, Table};
use crate::value::{Row, Value};

/// Builds a full row from explicitly supplied values: defaults first, then
/// the supplied values, then type coercion and AutoNumber assignment.
pub(crate) fn build_row(schema: &TableSchema, positions: &[usize], vals: Vec<Value>, next_auto: &mut i64) -> Result<Row> {
    let mut row: Row = Vec::with_capacity(schema.columns.len());
    for (i, c) in schema.columns.iter().enumerate() {
        row.push(if c.is_calculated() {
            Value::Null
        } else if let Some(seq) = c.sequence_default() {
            // A column that was given a value does not use up a number.
            if positions.contains(&i) {
                Value::Null
            } else {
                Value::Int(crate::sequence::next_value(seq)?)
            }
        } else {
            c.default.clone().unwrap_or(Value::Null)
        });
    }
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

/// `a AND b`, where either may be missing.
fn combine_filters(a: Option<&Expr>, b: Option<Expr>) -> Option<Expr> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Expr::Binary { op: crate::sql::ast::BinOp::And, left: Box::new(a.clone()), right: Box::new(b) }),
        (Some(a), None) => Some(a.clone()),
        (None, b) => b,
    }
}

pub(crate) fn table_scope(schema: &TableSchema) -> Scope {
    Scope {
        cols: schema
            .columns
            .iter()
            .map(|c| ScopeCol { table: schema.name.clone(), name: c.name.clone(), hidden: false })
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
        let mut ops = self.plan_insert(table, columns, source)?;
        if self.is_partitioned(table) {
            ops = self.route_partition_inserts(table, ops)?;
        }
        let n = ops.len();
        self.apply_all(ops)?;
        Ok(QueryResult::affected(n))
    }

    /// An insert that deals with rows that collide with an existing one on a unique key. The rows
    /// go in one at a time, so a later row meets the earlier ones of the same statement.
    pub(crate) fn exec_upsert(&mut self, insert: &Statement, conflict: &Conflict) -> Result<QueryResult> {
        let Statement::Insert { table, columns, source } = insert else {
            return Err(Error::runtime("ON CONFLICT follows INSERT"));
        };
        let (name, schema) = {
            let t = self.table_checked(table)?;
            (t.name().to_string(), t.schema().clone())
        };
        let target: Vec<usize> = conflict.target.iter().map(|c| column_pos(&schema, c)).collect::<Result<_>>()?;
        let same = |a: &[usize], b: &[usize]| a.len() == b.len() && a.iter().all(|c| b.contains(c));
        if !target.is_empty() && !schema.indexes.iter().any(|ix| ix.unique && same(&ix.columns, &target)) {
            return Err(Error::catalog(format!(
                "ON CONFLICT ({}) needs a primary key or unique index on exactly those fields",
                conflict.target.join(", ")
            )));
        }
        let candidates = self.plan_insert(table, columns, source)?;
        self.atomically(|db| {
            let mut affected = 0;
            for op in candidates {
                let Op::Insert { row, .. } = op else { continue };
                // The existing rows this one collides with.
                let (hits, existing) = {
                    let t = db.table_checked(&name)?;
                    let mut hits: Vec<RowId> = Vec::new();
                    for ix in t.index_defs().filter(|ix| ix.unique && (target.is_empty() || same(&ix.columns, &target))) {
                        let key: Vec<Value> = ix.columns.iter().map(|&c| row[c].clone()).collect();
                        if key.iter().all(|v| !v.is_null()) {
                            hits.extend(t.lookup(&ix.columns, &key));
                        }
                    }
                    hits.sort_unstable();
                    hits.dedup();
                    let existing = hits.first().and_then(|&id| t.get(id).cloned());
                    (hits, existing)
                };
                let fresh = |db: &Database| Op::Insert { table: name.clone(), id: db.table(&name).map_or(0, Table::slot_count) as RowId, row: row.clone() };
                match (&conflict.action, existing) {
                    (_, None) => {
                        let op = fresh(db);
                        db.apply_all(vec![op])?;
                        affected += 1;
                    }
                    (ConflictAction::Nothing, Some(_)) => {}
                    (ConflictAction::Replace, Some(_)) => {
                        let mut ops: Vec<Op> = hits.iter().map(|&id| Op::Delete { table: name.clone(), id }).collect();
                        ops.push(fresh(db));
                        db.apply_all(ops)?;
                        affected += 1;
                    }
                    (ConflictAction::Update { sets, filter }, Some(old)) => {
                        // EXCLUDED.col is the row that was offered; the rest read the existing row.
                        let offered = RowContext { schema: &schema, old: None, new: Some(&row) };
                        let renamed = |e: &Expr| -> Result<Expr> {
                            let mut e = e.clone();
                            crate::sql::walk::walk_expr(&mut e, &mut |x| {
                                if let Expr::Column { table: Some(t), .. } = x {
                                    if t.eq_ignore_ascii_case("excluded") {
                                        *t = "new".into();
                                    }
                                }
                                Ok(())
                            })?;
                            offered.bind_expr(&mut e)?;
                            Ok(e)
                        };
                        if let Some(cond) = filter {
                            let cond = renamed(cond)?;
                            let scope = table_scope(&schema);
                            if !Binder::new(db, &scope, 0).bind(&cond)?.eval_bool(&old)? {
                                continue;
                            }
                        }
                        let sets: Vec<(String, Expr)> = sets.iter().map(|(c, e)| Ok((c.clone(), renamed(e)?))).collect::<Result<_>>()?;
                        let id = hits[0];
                        let ops = db.plan_update_ids(&name, &sets, &[id])?;
                        db.apply_all(ops)?;
                        affected += 1;
                    }
                }
            }
            Ok(QueryResult::affected(affected))
        })
    }

    pub(crate) fn exec_update(
        &mut self,
        table: &str,
        assignments: &[(String, Expr)],
        filter: Option<&Expr>,
    ) -> Result<QueryResult> {
        let mut ops = Vec::new();
        for member in self.dml_members(table, filter) {
            ops.extend(self.plan_update(&member, assignments, filter)?);
        }
        let n = ops.len();
        self.apply_all(ops)?;
        Ok(QueryResult::affected(n))
    }

    pub(crate) fn exec_delete(&mut self, table: &str, filter: Option<&Expr>) -> Result<QueryResult> {
        let mut ops = Vec::new();
        for member in self.dml_members(table, filter) {
            ops.extend(self.plan_delete(&member, filter)?);
        }
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
        self.require_privilege(table, "insert")?;
        let ops = {
            let t = self.table_checked(table)?;
            let schema = t.schema();
            let positions: Vec<usize> = match columns {
                _ if matches!(source, InsertSource::DefaultValues) => Vec::new(),
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
                        for (k, e) in r.iter().enumerate() {
                            if matches!(e, Expr::Param(p) if p == crate::sql::parser::DEFAULT_MARK) {
                                // DEFAULT: what the column would get if it were left out
                                let c = &schema.columns[positions[k]];
                                vals.push(match c.sequence_default() {
                                    Some(seq) => Value::Int(crate::sequence::next_value(seq)?),
                                    None => c.default.clone().unwrap_or(Value::Null),
                                });
                                continue;
                            }
                            vals.push(binder.bind(e)?.eval(&[])?.into_owned());
                        }
                        out.push(vals);
                    }
                    out
                }
                InsertSource::DefaultValues => vec![Vec::new()],
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
        self.rls_check(table, "insert", &ops)?;
        Ok(ops)
    }

    pub(crate) fn plan_update(
        &self,
        table: &str,
        assignments: &[(String, Expr)],
        filter: Option<&Expr>,
    ) -> Result<Vec<Op>> {
        self.require_privilege(table, "update")?;
        let rls = combine_filters(filter, self.rls_expr(table, "update", false)?);
        let filter = rls.as_ref();
        let t = self.table_checked(table)?;
        let schema = t.schema();
        let scope = table_scope(schema);
        let mut binder = Binder::new(self, &scope, 0);
        binder.allow_corr = true;
        let sets: Vec<(usize, BExpr)> = assignments
            .iter()
            .map(|(c, e)| Ok((column_pos(schema, c)?, binder.bind(e)?)))
            .collect::<Result<_>>()?;
        if let Some((p, _)) = sets.iter().find(|(p, _)| schema.columns[*p].is_calculated()) {
            return Err(Database::calculated_write_error(&schema.name, &schema.columns[*p].name));
        }
        let f = filter.map(|f| binder.bind(f)).transpose()?;
        let corr = std::mem::take(&mut binder.corr);
        let hits = self.matching_with_corr(t, f, &corr, schema.columns.len())?;
        let ops = update_ops(t, &sets, hits)?;
        self.rls_check(table, "update", &ops)?;
        Ok(ops)
    }

    /// The update of exactly the records `ids` (used by MERGE).
    pub(crate) fn plan_update_ids(&self, table: &str, assignments: &[(String, Expr)], ids: &[RowId]) -> Result<Vec<Op>> {
        self.require_privilege(table, "update")?;
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
        update_ops(t, &sets, ids.iter().map(|&id| (id, None)).collect())
    }

    pub(crate) fn plan_delete(&self, table: &str, filter: Option<&Expr>) -> Result<Vec<Op>> {
        self.require_privilege(table, "delete")?;
        let rls = combine_filters(filter, self.rls_expr(table, "delete", false)?);
        let filter = rls.as_ref();
        let t = self.table_checked(table)?;
        let schema = t.schema();
        let scope = table_scope(schema);
        let mut binder = Binder::new(self, &scope, 0);
        binder.allow_corr = true;
        let f = filter.map(|f| binder.bind(f)).transpose()?;
        let corr = std::mem::take(&mut binder.corr);
        Ok(self
            .matching_with_corr(t, f, &corr, schema.columns.len())?
            .into_iter()
            .map(|(id, _)| Op::Delete { table: schema.name.clone(), id })
            .collect())
    }

    /// The records of `t` passing `filter`. A filter that reads correlated
    /// sub-query values needs each record extended with them first; those
    /// extended records come back alongside the ids.
    fn matching_with_corr(
        &self,
        t: &Table,
        filter: Option<BExpr>,
        corr: &[CorrSpec],
        width: usize,
    ) -> Result<Vec<(RowId, Option<Row>)>> {
        if corr.is_empty() {
            let conj = filter.as_ref().map(split_and).unwrap_or_default();
            return Ok(matching_ids(t, &conj, filter.as_ref())?.into_iter().map(|id| (id, None)).collect());
        }
        let (early, late) = split_late(filter, width);
        let conj = early.as_ref().map(split_and).unwrap_or_default();
        let ids = matching_ids(t, &conj, early.as_ref())?;
        let rows: Vec<Row> = ids.iter().map(|&id| t.get(id).expect("id from live scan").clone()).collect();
        let rows = self.add_corr_columns(rows, corr, 0)?;
        let mut out = Vec::with_capacity(ids.len());
        for (id, row) in ids.into_iter().zip(rows) {
            if late.as_ref().is_none_or(|f| f.eval_bool(&row).unwrap_or(false)) {
                out.push((id, Some(row)));
            }
        }
        Ok(out)
    }

    /// Ids of the records of `table` for which `cond` holds.
    pub(crate) fn ids_matching(&self, table: &str, cond: &Expr) -> Result<Vec<RowId>> {
        let t = self.table_checked(table)?;
        let scope = table_scope(t.schema());
        let f = Binder::new(self, &scope, 0).bind(cond)?;
        let conj = split_and(&f);
        matching_ids(t, &conj, Some(&f))
    }

    /// Whether `cond` holds for one record of `table`.
    pub(crate) fn row_condition(&self, table: &str, id: RowId, cond: &Expr) -> Result<bool> {
        let t = self.table_checked(table)?;
        let scope = table_scope(t.schema());
        let f = Binder::new(self, &scope, 0).bind(cond)?;
        match t.get(id) {
            Some(row) => f.eval_bool(row),
            None => Ok(false),
        }
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

/// Builds the `Update` ops for `hits`, evaluating the assignments on each
/// record (extended with sub-query values where the statement needs them).
fn update_ops(t: &Table, sets: &[(usize, BExpr)], hits: Vec<(RowId, Option<Row>)>) -> Result<Vec<Op>> {
    let schema = t.schema();
    let width = schema.columns.len();
    let mut ops = Vec::with_capacity(hits.len());
    for (id, extended) in hits {
        let old: &Row = match &extended {
            Some(r) => r,
            None => t.get(id).expect("id from live scan"),
        };
        let mut new: Row = old[..width].to_vec();
        for (c, e) in sets {
            new[*c] = e.eval(old)?.into_owned();
        }
        ops.push(Op::Update { table: schema.name.clone(), id, row: schema.coerce_row(new)? });
    }
    Ok(ops)
}
