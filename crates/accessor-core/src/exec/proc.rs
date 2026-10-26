//! Parameterised saved queries (`EXEC`) and trigger definitions.

use std::collections::HashMap;
use std::sync::Arc;

use super::bind::{Binder, Scope};
use super::QueryResult;
use crate::database::{key, Database};
use crate::error::{Error, Result};
use crate::sql::ast::{Expr, ParamDecl, Select, Statement, TriggerDef, TriggerEvent, TriggerTiming};
use crate::sql::walk::{bind_params_select, walk_select, RowContext};
use crate::sql::{parse_query_def, QueryDef};
use crate::storage::Op;
use crate::value::{Row, Value};

/// Pairs supplied values with declared parameters: positional values fill
/// the declarations in order, named ones (`@p = v`) go where they say.
/// Declared types, when present, convert the values.
pub(crate) fn bind_arguments(decls: &[ParamDecl], args: Vec<(Option<String>, Value)>) -> Result<HashMap<String, Value>> {
    let mut out: HashMap<String, Value> = HashMap::with_capacity(decls.len());
    let mut next = 0;
    for (name, value) in args {
        let decl = match name {
            Some(n) => decls
                .iter()
                .find(|d| d.name.eq_ignore_ascii_case(&n))
                .ok_or_else(|| Error::runtime(format!("the query has no parameter @{n}")))?,
            None => {
                while decls.get(next).is_some_and(|d| out.contains_key(&d.name.to_ascii_lowercase())) {
                    next += 1;
                }
                next += 1;
                decls.get(next - 1).ok_or_else(|| {
                    Error::runtime(format!("too many arguments: the query takes {} parameter(s)", decls.len()))
                })?
            }
        };
        let value = match decl.data_type {
            Some(t) if !value.is_null() => value
                .coerce_to(t)
                .map_err(|e| Error::type_err(format!("parameter @{}: {e}", decl.name)))?,
            _ => value,
        };
        if out.insert(decl.name.to_ascii_lowercase(), value).is_some() {
            return Err(Error::runtime(format!("the parameter @{} was supplied twice", decl.name)));
        }
    }
    if let Some(d) = decls.iter().find(|d| !out.contains_key(&d.name.to_ascii_lowercase())) {
        return Err(Error::runtime(format!("no value was supplied for the parameter @{}", d.name)));
    }
    Ok(out)
}

/// Names of the parameters a SELECT uses.
pub(crate) fn used_params(select: &mut Select) -> Vec<String> {
    let mut names = Vec::new();
    let _ = walk_select(select, &mut |e| {
        if let Expr::Param(n) = e {
            names.push(n.clone());
        }
        Ok(())
    });
    names
}

impl Database {
    pub(crate) fn eval_const(&self, e: &Expr) -> Result<Value> {
        let scope = Scope::default();
        Ok(Binder::new(self, &scope, 0).bind(e)?.eval(&[])?.into_owned())
    }

    /// The SELECT a saved query stands for, with its declared parameters. A
    /// saved call of another query is resolved by binding that query now.
    pub(crate) fn resolve_query(&self, name: &str, depth: usize) -> Result<(Vec<ParamDecl>, Select)> {
        if depth > 8 {
            return Err(Error::runtime("saved queries call each other in a cycle"));
        }
        let sql = self
            .view_sql(name)
            .ok_or_else(|| Error::catalog(format!("the query '{name}' does not exist")))?;
        self.resolve_def(parse_query_def(sql)?, depth)
    }

    pub(crate) fn resolve_def(&self, def: QueryDef, depth: usize) -> Result<(Vec<ParamDecl>, Select)> {
        match def {
            QueryDef::Select { params, select } => Ok((params, select)),
            QueryDef::Exec { name, args } => Ok((Vec::new(), self.bind_call(&name, &args, depth + 1)?)),
        }
    }

    /// The SELECT of query `name` with `args` filled in.
    pub(crate) fn bind_call(&self, name: &str, args: &[(Option<String>, Expr)], depth: usize) -> Result<Select> {
        let (decls, mut select) = self.resolve_query(name, depth)?;
        let values = args
            .iter()
            .map(|(n, e)| Ok((n.clone(), self.eval_const(e)?)))
            .collect::<Result<Vec<_>>>()?;
        let bound = bind_arguments(&decls, values)?;
        bind_params_select(&mut select, &bound)?;
        Ok(select)
    }

    /// Runs a saved query, binding its parameters to `args`.
    pub(crate) fn exec_exec(&mut self, name: &str, args: &[(Option<String>, Expr)]) -> Result<QueryResult> {
        if self.view_sql(name).is_none() {
            if let Some(done) = self.exec_prepared_by_name(name, args)? {
                return Ok(done);
            }
            if let Some(done) = self.call_procedure(name, args)? {
                return Ok(done);
            }
        }
        let select = self.bind_call(name, args, 0)?;
        self.run_select(&select)
    }

    /// Runs a script in which `@name` parameters are replaced by `params`.
    /// A leading `PARAMETERS` line declares (and type-converts) them.
    pub fn execute_with_params(&mut self, sql: &str, params: &[(&str, Value)]) -> Result<QueryResult> {
        let values: HashMap<String, Value> =
            params.iter().map(|(n, v)| (n.trim_start_matches('@').to_ascii_lowercase(), v.clone())).collect();
        let statements = self.parse_cached(sql)?;
        self.run_with_values(&statements, values)
    }

    // ----- triggers ---------------------------------------------------------------

    pub(crate) fn exec_create_trigger(&mut self, def: &TriggerDef) -> Result<QueryResult> {
        let mut owned = def.clone();
        crate::trigger::rule_target(self, &mut owned);
        let def = &owned;
        if self.triggers.contains_key(&key(&def.name)) {
            return Err(Error::catalog(format!("the trigger '{}' already exists", def.name)));
        }
        // What the trigger is attached to: a table, or (for INSTEAD OF) a query.
        let schema = if def.timing == TriggerTiming::InsteadOf {
            if self.table(&def.table).is_some() || self.view_sql(&def.table).is_none() {
                return Err(Error::catalog(format!("an INSTEAD OF trigger belongs to a query, and '{}' is not one", def.table)));
            }
            crate::trigger::view_schema(&def.table, &self.resolve_factor(&crate::sql::ast::TableFactor::Named { name: def.table.clone(), alias: None, only: false }, 0)?.names)
        } else {
            self.table_checked(&def.table)?.schema().clone()
        };
        for c in &def.update_of {
            if schema.column_index(c).is_none() {
                return Err(Error::catalog(format!("the field '{c}' does not exist in '{}'", def.table)));
            }
        }
        // Dry run: every OLD/NEW/column reference must resolve for each event.
        let blank: Row = vec![Value::Null; schema.columns.len()];
        for event in &def.events {
            let ctx = if def.each_statement {
                RowContext { schema: &schema, old: None, new: None }
            } else {
                RowContext {
                    schema: &schema,
                    old: (*event != TriggerEvent::Insert).then_some(&blank),
                    new: (*event != TriggerEvent::Delete).then_some(&blank),
                }
            };
            if let Some(w) = &def.when {
                ctx.bind_expr(&mut w.clone())?;
            }
            for st in &def.body {
                let mut st = st.clone();
                ctx.bind_statement(&mut st)?;
                if let Statement::Insert { table: target, .. } = &st {
                    if target.eq_ignore_ascii_case(&def.table) {
                        return Err(Error::catalog("a trigger cannot insert into the table it is attached to"));
                    }
                }
            }
        }
        self.apply_all(vec![Op::CreateTrigger { name: def.name.clone(), sql: def.sql.clone() }])?;
        Ok(QueryResult::default())
    }

    pub(crate) fn exec_drop_trigger(&mut self, name: &str) -> Result<QueryResult> {
        let real = self
            .triggers
            .get(&key(name))
            .map(|t: &Arc<TriggerDef>| t.name.clone())
            .ok_or_else(|| Error::catalog(format!("the trigger '{name}' does not exist")))?;
        self.apply_all(vec![Op::DropTrigger(real)])?;
        Ok(QueryResult::default())
    }
}
