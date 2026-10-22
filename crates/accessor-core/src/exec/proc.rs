//! Parameterised saved queries (`EXEC`) and trigger definitions.

use std::collections::HashMap;
use std::sync::Arc;

use super::bind::{Binder, Scope};
use super::QueryResult;
use crate::database::{key, Database};
use crate::error::{Error, Result};
use crate::sql::ast::{Expr, ParamDecl, Select, Statement, TriggerDef, TriggerEvent};
use crate::sql::walk::{bind_params, bind_params_select, walk_select, RowContext};
use crate::sql::{parse_query_def, parse_script, QueryDef};
use crate::storage::Op;
use crate::value::{Row, Value};

/// Pairs supplied values with declared parameters: positional values fill
/// the declarations in order, named ones (`@p = v`) go where they say.
/// Declared types, when present, convert the values.
fn bind_arguments(decls: &[ParamDecl], args: Vec<(Option<String>, Value)>) -> Result<HashMap<String, Value>> {
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
    fn eval_const(&self, e: &Expr) -> Result<Value> {
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
        let select = self.bind_call(name, args, 0)?;
        self.run_select(&select)
    }

    /// Runs a script in which `@name` parameters are replaced by `params`.
    /// A leading `PARAMETERS` line declares (and type-converts) them.
    pub fn execute_with_params(&mut self, sql: &str, params: &[(&str, Value)]) -> Result<QueryResult> {
        let mut values: HashMap<String, Value> =
            params.iter().map(|(n, v)| (n.trim_start_matches('@').to_ascii_lowercase(), v.clone())).collect();
        let mut last = QueryResult::default();
        for mut st in parse_script(sql)? {
            if let Statement::Parameters(decls) = &st {
                let given = decls
                    .iter()
                    .map(|d| {
                        let v = values.get(&d.name.to_ascii_lowercase()).cloned().ok_or_else(|| {
                            Error::runtime(format!("no value was supplied for the parameter @{}", d.name))
                        })?;
                        Ok((Some(d.name.clone()), v))
                    })
                    .collect::<Result<Vec<_>>>()?;
                values.extend(bind_arguments(decls, given)?);
                continue;
            }
            bind_params(&mut st, &values)?;
            last = self.execute_statement(&st)?;
        }
        Ok(last)
    }

    // ----- triggers ---------------------------------------------------------------

    pub(crate) fn exec_create_trigger(&mut self, def: &TriggerDef) -> Result<QueryResult> {
        let table = self.table_checked(&def.table)?;
        if self.triggers.contains_key(&key(&def.name)) {
            return Err(Error::catalog(format!("the trigger '{}' already exists", def.name)));
        }
        // Dry run: every OLD/NEW/column reference must resolve for this event.
        let blank: Row = vec![Value::Null; table.schema().columns.len()];
        let ctx = RowContext {
            schema: table.schema(),
            old: (def.event != TriggerEvent::Insert).then_some(&blank),
            new: (def.event != TriggerEvent::Delete).then_some(&blank),
        };
        if let Some(w) = &def.when {
            ctx.bind_expr(&mut w.clone())?;
        }
        for st in &def.body {
            let mut st = st.clone();
            ctx.bind_statement(&mut st)?;
            if let Statement::Insert { table: target, .. } = &st {
                if target.eq_ignore_ascii_case(table.name()) {
                    return Err(Error::catalog("a trigger cannot insert into the table it is attached to"));
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
