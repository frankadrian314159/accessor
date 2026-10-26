//! Prepared statements and the statement cache.
//!
//! Reading SQL text (lexing and parsing it) is work that does not depend on the data, so it need
//! only be done once for a statement that is run again and again. Two things save it:
//!
//! * [`Database::prepare`] reads a statement once and gives back a [`Prepared`] that can be run
//!   many times with different parameter values (`@name`);
//! * every database keeps the parsed form of the SQL text it has recently been given, so
//!   `execute`, `query` and `execute_with_params` skip the parse for text they have seen.
//!
//! What is *not* kept is the plan. How a query is best run (which index, which join order, whether
//! to hash a sub-query) depends on the data, which changes between runs, and on the values of the
//! parameters, so it is decided afresh each time from the statistics of the moment.

use std::collections::HashMap;
use std::sync::Arc;

use super::QueryResult;
use crate::database::{key, Database};
use crate::error::{Error, Result};
use crate::sql::ast::{Expr, Statement};
use crate::sql::parse_script;
use crate::sql::walk::{bind_params, walk_statement};
use crate::value::Value;

/// How many different SQL texts the cache holds before it starts again.
const CACHE_ENTRIES: usize = 256;
/// Longer texts (bulk loads) are not worth remembering.
const CACHE_MAX_TEXT: usize = 8 * 1024;

/// A statement (or script) that has been read and can be run many times.
#[derive(Clone)]
pub struct Prepared {
    sql: String,
    statements: Arc<Vec<Statement>>,
    params: Vec<String>,
}

impl Prepared {
    /// The text it was prepared from.
    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// The parameters it uses (`@name`, written without the `@`), in order of first appearance.
    pub fn parameters(&self) -> &[String] {
        &self.params
    }

    pub(crate) fn statements(&self) -> &[Statement] {
        &self.statements
    }
}

impl Database {
    /// True if running `statements` cannot change the database in any way, so that they may run while
    /// other readers do: a single `SELECT` or `EXPLAIN` that hands out no sequence numbers. Anything
    /// that might (a call of `NEXTVAL` or `SETVAL`, of a stored function, or a saved query that
    /// mentions a sequence) is treated as a write.
    pub(crate) fn is_pure_read(&self, statements: &[Statement]) -> bool {
        let mut copy = match statements {
            [Statement::Select(s)] => s.clone(),
            [Statement::Explain(s) | Statement::ExplainAnalyze(s)] => (**s).clone(),
            _ => return false,
        };
        let sequences_in_views = self.views.values().any(|(_, sql)| {
            let up = sql.to_ascii_uppercase();
            up.contains("NEXTVAL") || up.contains("SETVAL") || up.contains("NEXT VALUE")
        });
        if sequences_in_views {
            return false;
        }
        let mut pure = true;
        let _ = crate::sql::walk::walk_select(&mut copy, &mut |e| {
            if let Expr::Function { name, .. } = e {
                if name.eq_ignore_ascii_case("NEXTVAL")
                    || name.eq_ignore_ascii_case("SETVAL")
                    || self.routines.contains_key(&key(name))
                {
                    pure = false;
                }
            }
            Ok(())
        });
        pure
    }
}

fn parameters_of(statements: &[Statement]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for st in statements {
        if let Statement::Parameters(decls) = st {
            for d in decls {
                if !names.iter().any(|n| n.eq_ignore_ascii_case(&d.name)) {
                    names.push(d.name.clone());
                }
            }
        }
        let mut copy = st.clone();
        let _ = walk_statement(&mut copy, &mut |e| {
            if let Expr::Param(n) = e {
                if !names.iter().any(|x| x.eq_ignore_ascii_case(n)) {
                    names.push(n.clone());
                }
            }
            Ok(())
        });
    }
    names
}

impl Database {
    /// The statements of `sql`, read now or taken from the cache.
    pub(crate) fn parse_cached(&self, sql: &str) -> Result<Arc<Vec<Statement>>> {
        if let Some(hit) = self.parse_cache.lock().unwrap_or_else(|e| e.into_inner()).get(sql) {
            return Ok(Arc::clone(hit));
        }
        crate::sql::usertypes::set_types(self.user_types());
        let parsed = Arc::new(parse_script(sql)?);
        if sql.len() <= CACHE_MAX_TEXT {
            let mut cache = self.parse_cache.lock().unwrap_or_else(|e| e.into_inner());
            if cache.len() >= CACHE_ENTRIES {
                cache.clear();
            }
            cache.insert(sql.to_string(), Arc::clone(&parsed));
        }
        Ok(parsed)
    }

    /// Reads `sql` once, to be run later with [`execute_prepared`](Database::execute_prepared)
    /// or [`query_prepared`](Database::query_prepared). Parameters are written `@name`.
    ///
    /// ```
    /// # use accessor_core::{Database, Value};
    /// let mut db = Database::in_memory();
    /// db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
    /// let insert = db.prepare("INSERT INTO t VALUES (@id, @name)").unwrap();
    /// for (i, n) in ["Ada", "Grace"].iter().enumerate() {
    ///     db.execute_prepared(&insert, &[("id", Value::Int(i as i64)), ("name", Value::text(n))]).unwrap();
    /// }
    /// let find = db.prepare("SELECT name FROM t WHERE id = @id").unwrap();
    /// let r = db.query_prepared(&find, &[("id", Value::Int(1))]).unwrap();
    /// assert_eq!(r.rows[0][0], Value::text("Grace"));
    /// ```
    pub fn prepare(&self, sql: &str) -> Result<Prepared> {
        let statements = self.parse_cached(sql)?;
        if statements.is_empty() {
            return Err(Error::syntax("empty statement"));
        }
        let params = parameters_of(&statements);
        Ok(Prepared { sql: sql.to_string(), statements, params })
    }

    fn param_map(prepared: &Prepared, params: &[(&str, Value)]) -> Result<HashMap<String, Value>> {
        let mut values: HashMap<String, Value> =
            params.iter().map(|(n, v)| (n.trim_start_matches('@').to_ascii_lowercase(), v.clone())).collect();
        if let Some(name) = prepared.params.iter().find(|p| !values.contains_key(&p.to_ascii_lowercase())) {
            return Err(Error::runtime(format!("no value was supplied for the parameter @{name}")));
        }
        values.retain(|k, _| prepared.params.iter().any(|p| p.eq_ignore_ascii_case(k)));
        Ok(values)
    }

    /// Runs a prepared statement with values for its parameters; the result is that of the last statement.
    pub fn execute_prepared(&mut self, prepared: &Prepared, params: &[(&str, Value)]) -> Result<QueryResult> {
        let values = Self::param_map(prepared, params)?;
        self.run_with_values(&prepared.statements, values)
    }

    /// Runs a prepared `SELECT` (it cannot change anything, so the database is only borrowed).
    pub fn query_prepared(&self, prepared: &Prepared, params: &[(&str, Value)]) -> Result<QueryResult> {
        let values = Self::param_map(prepared, params)?;
        let [Statement::Select(sel)] = prepared.statements.as_slice() else {
            return Err(Error::runtime("query_prepared() only accepts a single SELECT statement"));
        };
        let mut sel = sel.clone();
        crate::sql::walk::bind_params_select(&mut sel, &values)?;
        self.run_select(&sel)
    }

    // ----- PREPARE / EXEC / DEALLOCATE ---------------------------------------------

    pub(crate) fn exec_prepare(&mut self, name: &str, statement: &Statement) -> Result<QueryResult> {
        if matches!(statement, Statement::Prepare { .. } | Statement::Deallocate(_)) {
            return Err(Error::runtime("PREPARE cannot prepare another PREPARE or DEALLOCATE"));
        }
        if self.view_sql(name).is_some() {
            return Err(Error::catalog(format!("'{name}' is already the name of a saved query")));
        }
        let statements = Arc::new(vec![statement.clone()]);
        let params = parameters_of(&statements);
        self.prepared.insert(key(name), Prepared { sql: String::new(), statements, params });
        Ok(QueryResult::default())
    }

    pub(crate) fn exec_deallocate(&mut self, name: &str) -> Result<QueryResult> {
        if name.eq_ignore_ascii_case("all") {
            self.prepared.clear();
        } else if self.prepared.remove(&key(name)).is_none() {
            return Err(Error::catalog(format!("there is no prepared statement named '{name}'")));
        }
        Ok(QueryResult::default())
    }

    /// `EXEC name (args)` when `name` is a prepared statement: positional arguments fill its
    /// parameters in order, named ones (`@p = v`) go where they say.
    pub(crate) fn exec_prepared_by_name(&mut self, name: &str, args: &[(Option<String>, Expr)]) -> Result<Option<QueryResult>> {
        let Some(p) = self.prepared.get(&key(name)).cloned() else { return Ok(None) };
        let mut values: Vec<(String, Value)> = Vec::new();
        let mut next = 0;
        for (pname, e) in args {
            let v = self.eval_const(e)?;
            let target = match pname {
                Some(n) => p
                    .params
                    .iter()
                    .find(|d| d.eq_ignore_ascii_case(n))
                    .ok_or_else(|| Error::runtime(format!("the prepared statement has no parameter @{n}")))?
                    .clone(),
                None => {
                    while p.params.get(next).is_some_and(|d| values.iter().any(|(n, _)| n.eq_ignore_ascii_case(d))) {
                        next += 1;
                    }
                    next += 1;
                    p.params
                        .get(next - 1)
                        .ok_or_else(|| Error::runtime(format!("too many arguments: the statement takes {} parameter(s)", p.params.len())))?
                        .clone()
                }
            };
            values.push((target, v));
        }
        let refs: Vec<(&str, Value)> = values.iter().map(|(n, v)| (n.as_str(), v.clone())).collect();
        self.execute_prepared(&p, &refs).map(Some)
    }

    /// Runs `statements`, in which each `@name` is replaced by its value.
    pub(crate) fn run_with_values(&mut self, statements: &[Statement], mut values: HashMap<String, Value>) -> Result<QueryResult> {
        let mut last = QueryResult::default();
        for st in statements {
            if let Statement::Parameters(decls) = st {
                let given = decls
                    .iter()
                    .map(|d| {
                        let v = values.get(&d.name.to_ascii_lowercase()).cloned().ok_or_else(|| {
                            Error::runtime(format!("no value was supplied for the parameter @{}", d.name))
                        })?;
                        Ok((Some(d.name.clone()), v))
                    })
                    .collect::<Result<Vec<_>>>()?;
                values.extend(super::proc::bind_arguments(decls, given)?);
                continue;
            }
            let mut st = st.clone();
            bind_params(&mut st, &values)?;
            last = self.execute_statement(&st)?;
        }
        Ok(last)
    }
}
