//! CREATE / DROP for tables, indexes, and saved queries.

use super::QueryResult;
use crate::database::{key, Database};
use crate::error::{Error, Result};
use crate::schema::{Column, ForeignKey, IndexDef, TableSchema};
use crate::sql::ast::{AlterAction, ColumnDef, ForeignKeyDef};
use crate::sql::{parse_query_def, QueryDef};
use crate::storage::Op;
use crate::value::{DataType, Value};

impl Database {
    pub(crate) fn exec_create_table(
        &mut self,
        name: &str,
        defs: &[ColumnDef],
        primary_key: &[String],
        uniques: &[Vec<String>],
        foreign_keys: &[ForeignKeyDef],
    ) -> Result<QueryResult> {
        let mut columns = Vec::with_capacity(defs.len());
        for d in defs {
            if d.data_type == DataType::Calculated {
                columns.push(calculated_column(d)?);
                continue;
            }
            let mut c = Column::new(d.name.clone(), d.data_type);
            c.primary_key = d.primary_key;
            c.not_null = d.not_null || d.primary_key;
            c.autonumber = d.autonumber;
            c.default = match &d.default {
                Some(v) => Some(
                    v.coerce_to(d.data_type)
                        .map_err(|e| Error::type_err(format!("default for '{}': {e}", d.name)))?,
                ),
                None => None,
            };
            columns.push(c);
        }
        let mut schema = TableSchema { name: name.to_string(), columns, indexes: Vec::new(), foreign_keys: Vec::new() };
        for i in 0..schema.columns.len() {
            if schema.columns[i].is_calculated() {
                self.validate_calculated(&schema, i)?;
            }
        }

        let position = |schema: &TableSchema, col: &str| {
            schema
                .column_index(col)
                .ok_or_else(|| Error::catalog(format!("the field '{col}' does not exist in table '{name}'")))
        };
        for col in primary_key {
            let p = position(&schema, col)?;
            schema.columns[p].primary_key = true;
            schema.columns[p].not_null = true;
        }
        let pk: Vec<usize> = (0..schema.columns.len()).filter(|&i| schema.columns[i].primary_key).collect();
        if !pk.is_empty() {
            schema.indexes.push(IndexDef { name: "PrimaryKey".into(), columns: pk, unique: true });
        }
        for (i, d) in defs.iter().enumerate() {
            if d.unique {
                schema.indexes.push(IndexDef {
                    name: format!("{name}_{}_unique", d.name),
                    columns: vec![i],
                    unique: true,
                });
            }
        }
        for cols in uniques {
            let positions = cols.iter().map(|c| position(&schema, c)).collect::<Result<Vec<_>>>()?;
            schema.indexes.push(IndexDef { name: format!("{name}_{}_unique", cols.join("_")), columns: positions, unique: true });
        }

        let fk_defs = defs.iter().filter_map(|d| d.references.as_ref()).chain(foreign_keys);
        for def in fk_defs {
            let (fk, index) = self.build_foreign_key(&schema, def)?;
            if schema.foreign_keys.iter().any(|f| f.name.eq_ignore_ascii_case(&fk.name)) {
                return Err(Error::catalog(format!("duplicate constraint name '{}'", fk.name)));
            }
            schema.foreign_keys.push(fk);
            schema.indexes.extend(index);
        }

        self.apply_all(vec![Op::CreateTable(schema)])?;
        Ok(QueryResult::default())
    }

    pub(crate) fn exec_drop_table(&mut self, name: &str) -> Result<QueryResult> {
        let real = self.table_checked(name)?.name().to_string();
        self.apply_all(vec![Op::DropTable(real)])?;
        Ok(QueryResult::default())
    }

    pub(crate) fn exec_alter_table(&mut self, table: &str, action: &AlterAction) -> Result<QueryResult> {
        let table = self.table_checked(table)?.name().to_string();
        let ops = match action {
            AlterAction::AddColumn(d) => {
                if d.data_type == DataType::Calculated {
                    let c = calculated_column(d)?;
                    let mut schema = self.table_checked(&table)?.schema().clone();
                    schema.columns.push(c.clone());
                    self.validate_calculated(&schema, schema.columns.len() - 1)?;
                    self.apply_all(vec![Op::AddColumn { table, column: c }])?;
                    return Ok(QueryResult::default());
                }
                if d.unique {
                    return Err(Error::catalog("add the column first, then CREATE UNIQUE INDEX"));
                }
                let mut c = Column::new(d.name.clone(), d.data_type);
                c.not_null = d.not_null;
                c.primary_key = d.primary_key;
                c.autonumber = d.autonumber;
                c.default = d.default.clone();
                let mut ops = Vec::with_capacity(3);
                if let Some(def) = &d.references {
                    // Plan against the schema as it will look once the column exists.
                    let mut schema = self.table_checked(&table)?.schema().clone();
                    schema.columns.push(c.clone());
                    let (fk, index) = self.build_foreign_key(&schema, def)?;
                    if schema.foreign_keys.iter().any(|f| f.name.eq_ignore_ascii_case(&fk.name)) {
                        return Err(Error::catalog(format!("constraint '{}' already exists on '{table}'", fk.name)));
                    }
                    if let Some(v) = c.default.as_ref().filter(|v| !v.is_null()) {
                        let v = v.coerce_to(c.data_type)?;
                        if self.table_checked(&fk.ref_table)?.lookup(&fk.ref_columns, &[v]).is_empty() {
                            return Err(Error::constraint(format!(
                                "the default for '{}' has no matching record in '{}'",
                                c.name, fk.ref_table
                            )));
                        }
                    }
                    ops.push(Op::AddColumn { table: table.clone(), column: c });
                    ops.extend(index.map(|def| Op::CreateIndex { table: table.clone(), def }));
                    ops.push(Op::AddForeignKey { table, fk });
                } else {
                    ops.push(Op::AddColumn { table, column: c });
                }
                ops
            }
            AlterAction::SetExpression { name, expr } => {
                // Changes a calculated field's formula, or turns an ordinary field into a calculated one.
                if self.table_checked(&table)?.schema().column_index(name).is_none() {
                    return Err(Error::catalog(format!("the field '{name}' does not exist in table '{table}'")));
                }
                self.prospective_schema(&table, name, expr, true)?;
                vec![Op::SetExpression { table, name: name.clone(), expr: expr.clone() }]
            }
            AlterAction::DropColumn(name) => vec![Op::DropColumn { table, name: name.clone() }],
            AlterAction::RenameColumn { from, to } => {
                vec![Op::RenameColumn { table, from: from.clone(), to: to.clone() }]
            }
            AlterAction::AlterColumn { name, data_type } => {
                vec![Op::AlterColumnType { table, name: name.clone(), data_type: *data_type }]
            }
            AlterAction::AddForeignKey(def) => {
                let t = self.table_checked(&table)?;
                let (fk, index) = self.build_foreign_key(t.schema(), def)?;
                if t.schema().foreign_keys.iter().any(|f| f.name.eq_ignore_ascii_case(&fk.name)) {
                    return Err(Error::catalog(format!("constraint '{}' already exists on '{table}'", fk.name)));
                }
                let parent = self.table_checked(&fk.ref_table)?;
                let orphans = t
                    .iter()
                    .filter(|(_, row)| {
                        let k: Vec<Value> = fk.columns.iter().map(|&c| row[c].clone()).collect();
                        !k.iter().any(Value::is_null) && parent.lookup(&fk.ref_columns, &k).is_empty()
                    })
                    .count();
                if orphans > 0 {
                    return Err(Error::constraint(format!(
                        "cannot create the relationship: {orphans} record(s) in '{table}' have no matching record in '{}'",
                        fk.ref_table
                    )));
                }
                let mut ops: Vec<Op> =
                    index.map(|def| Op::CreateIndex { table: table.clone(), def }).into_iter().collect();
                ops.push(Op::AddForeignKey { table, fk });
                ops
            }
            AlterAction::DropConstraint(name) => vec![Op::DropForeignKey { table, name: name.clone() }],
        };
        self.apply_all(ops)?;
        Ok(QueryResult::default())
    }

    /// Resolves and validates a relationship definition for `child`, which
    /// may be a table that does not exist yet. Also returns the lookup index
    /// on the referencing fields, unless an equivalent one already exists.
    fn build_foreign_key(&self, child: &TableSchema, def: &ForeignKeyDef) -> Result<(ForeignKey, Option<IndexDef>)> {
        let positions = |schema: &TableSchema, names: &[String]| {
            names
                .iter()
                .map(|n| {
                    schema.column_index(n).ok_or_else(|| {
                        Error::catalog(format!("the field '{n}' does not exist in table '{}'", schema.name))
                    })
                })
                .collect::<Result<Vec<_>>>()
        };
        let columns = positions(child, &def.columns)?;
        let parent = if def.ref_table.eq_ignore_ascii_case(&child.name) {
            child
        } else {
            self.table_checked(&def.ref_table)?.schema()
        };
        let ref_columns = if def.ref_columns.is_empty() {
            let pk: Vec<usize> = (0..parent.columns.len()).filter(|&i| parent.columns[i].primary_key).collect();
            if pk.is_empty() {
                return Err(Error::catalog(format!("'{}' has no primary key to reference", parent.name)));
            }
            pk
        } else {
            positions(parent, &def.ref_columns)?
        };
        if columns.len() != ref_columns.len() {
            return Err(Error::catalog("a relationship must list as many referenced fields as referencing fields"));
        }
        for (&c, &p) in columns.iter().zip(&ref_columns) {
            let (cc, pc) = (&child.columns[c], &parent.columns[p]);
            // Exact numbers compare by value whatever their type, so an INTEGER can refer to a
            // DECIMAL key; anything else must match, and so must the two floats.
            let comparable = cc.data_type == pc.data_type || (cc.data_type.is_exact() && pc.data_type.is_exact());
            if !comparable {
                return Err(Error::type_err(format!(
                    "'{}' ({}) and '{}.{}' ({}) must have the same data type",
                    cc.name, cc.data_type, parent.name, pc.name, pc.data_type
                )));
            }
            if cc.is_calculated() {
                return Err(Error::catalog(format!("the calculated field '{}' cannot refer to another table", cc.name)));
            }
            if cc.autonumber {
                return Err(Error::catalog(format!("the AutoNumber field '{}' cannot refer to another table", cc.name)));
            }
        }
        let same_set = |a: &[usize], b: &[usize]| a.len() == b.len() && a.iter().all(|c| b.contains(c));
        if !parent.indexes.iter().any(|ix| ix.unique && same_set(&ix.columns, &ref_columns)) {
            return Err(Error::catalog(format!(
                "the referenced fields of '{}' must be its primary key or have a unique index",
                parent.name
            )));
        }
        let name = def.name.clone().unwrap_or_else(|| format!("fk_{}_{}", child.name, def.columns.join("_")));
        let index = (!child.indexes.iter().any(|ix| same_set(&ix.columns, &columns)))
            .then(|| IndexDef { name: format!("{name}_index"), columns: columns.clone(), unique: false });
        let fk = ForeignKey {
            name,
            columns,
            ref_table: parent.name.clone(),
            ref_columns,
            on_delete: def.on_delete,
            on_update: def.on_update,
        };
        Ok((fk, index))
    }

    pub(crate) fn exec_create_index(&mut self, name: &str, table: &str, columns: &[String], unique: bool) -> Result<QueryResult> {
        let t = self.table_checked(table)?;
        let positions = columns
            .iter()
            .map(|c| {
                t.schema()
                    .column_index(c)
                    .ok_or_else(|| Error::catalog(format!("the field '{c}' does not exist in table '{}'", t.name())))
            })
            .collect::<Result<Vec<_>>>()?;
        let op = Op::CreateIndex {
            table: t.name().to_string(),
            def: IndexDef { name: name.to_string(), columns: positions, unique },
        };
        self.apply_all(vec![op])?;
        Ok(QueryResult::default())
    }

    pub(crate) fn exec_drop_index(&mut self, name: &str, table: &str) -> Result<QueryResult> {
        let real = self.table_checked(table)?.name().to_string();
        self.apply_all(vec![Op::DropIndex { table: real, name: name.to_string() }])?;
        Ok(QueryResult::default())
    }

    pub(crate) fn exec_create_view(&mut self, name: &str, sql: &str) -> Result<QueryResult> {
        // Fail early on queries that do not parse or that use undeclared parameters.
        let def = parse_query_def(sql)?;
        if let QueryDef::Exec { name: target, args } = &def {
            // The call must be valid now: the target exists and the arguments fit.
            self.bind_call(target, args, 0)?;
        }
        let (decls, mut select) = self.resolve_def(def, 0)?;
        if let Some(p) = super::proc::used_params(&mut select)
            .into_iter()
            .find(|p| !decls.iter().any(|d| d.name.eq_ignore_ascii_case(p)))
        {
            return Err(Error::syntax(format!("the parameter @{p} is not declared; add it to the query's parameter list")));
        }
        self.apply_all(vec![Op::CreateView { name: name.to_string(), sql: sql.to_string() }])?;
        Ok(QueryResult::default())
    }

    pub(crate) fn exec_drop_view(&mut self, name: &str) -> Result<QueryResult> {
        let real = self
            .views
            .get(&key(name))
            .map(|(n, _)| n.clone())
            .ok_or_else(|| Error::catalog(format!("the query '{name}' does not exist")))?;
        self.apply_all(vec![Op::DropView(real)])?;
        Ok(QueryResult::default())
    }
}

/// The column for a `CALCULATED AS (expr)` definition. The expression is kept
/// in `default`; options that make no sense for a computed value are refused.
fn calculated_column(d: &ColumnDef) -> Result<Column> {
    let expr = d
        .expression
        .as_ref()
        .ok_or_else(|| Error::syntax("a CALCULATED field needs an expression: CALCULATED AS (expression)"))?;
    if d.not_null || d.primary_key || d.unique || d.autonumber || d.default.is_some() || d.references.is_some() {
        return Err(Error::catalog(format!(
            "the calculated field '{}' cannot be required, a key, unique, AutoNumber, have a default, or refer to another table",
            d.name
        )));
    }
    let mut c = Column::new(d.name.clone(), DataType::Calculated);
    c.default = Some(Value::text(expr));
    Ok(c)
}
