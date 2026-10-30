//! Stored functions and procedures.
//!
//! A **function** is one expression with named parameters. Calling it puts the arguments into the
//! expression (each parameter converted to its declared type), so the optimizer sees the real
//! expression: a call with constant arguments is folded to a constant and can use an index, and a
//! call on a column is evaluated like any other expression.
//!
//! A **procedure** is a block of statements with variables and control flow, run by `CALL` or
//! `EXEC`. It runs as one unit: if any statement fails, everything it did is undone.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::QueryResult;
use crate::database::{key, Database};
use crate::error::{Error, Result};
use crate::expr::ScalarFn;
use crate::sql::ast::*;
use crate::sql::parser::cast_function;
use crate::sql::walk::{bind_params, bind_params_exprs, walk_expr};
use crate::value::{DataType, Value};

/// Calls of functions inside functions may go this deep.
pub(crate) const MAX_INLINE: u32 = 16;
/// Procedures calling procedures may go this deep.
const MAX_CALLS: u32 = 16;
/// Loop iterations one `CALL` may make in all.
const MAX_ITERATIONS: u64 = 1_000_000;

fn cast_to(e: Expr, ty: DataType) -> Expr {
    match cast_function(ty) {
        Some(name) => Expr::Function { name: name.into(), args: vec![e], distinct: false, star: false },
        None => e,
    }
}

fn volatile(e: &Expr) -> bool {
    let mut found = false;
    let _ = walk_expr(&mut e.clone(), &mut |x| {
        if let Expr::Function { name, .. } = x {
            found |= ScalarFn::lookup(name).is_some_and(|(f, ..)| f.is_volatile());
        }
        Ok(())
    });
    found
}

/// The number of times parameter `name` appears in `body`.
fn uses(body: &Expr, name: &str) -> usize {
    let mut n = 0;
    let _ = walk_expr(&mut body.clone(), &mut |x| {
        if matches!(x, Expr::Param(p) if p.eq_ignore_ascii_case(name)) {
            n += 1;
        }
        Ok(())
    });
    n
}

/// A function's body with its parameters replaced by the arguments of a call.
pub(crate) fn inline_call(def: &RoutineDef, args: &[Expr]) -> Result<Expr> {
    let RoutineBody::Expr(body) = &def.body else {
        return Err(Error::catalog(if def.function {
            format!("the function '{}' has statements for a body, so it can only be called with arguments that are known before the query runs (not columns), and not by query()", def.name)
        } else {
            format!("'{}' is a procedure; run it with CALL", def.name)
        }));
    };
    if args.len() > def.params.len() {
        return Err(Error::runtime(format!("{} takes {} argument(s), not {}", def.name, def.params.len(), args.len())));
    }
    let mut map: HashMap<String, Expr> = HashMap::new();
    for (i, p) in def.params.iter().enumerate() {
        let arg = args
            .get(i)
            .cloned()
            .or_else(|| p.default.clone())
            .ok_or_else(|| Error::runtime(format!("{} needs a value for @{}", def.name, p.name)))?;
        if uses(body, &p.name) > 1 && volatile(&arg) {
            return Err(Error::runtime(format!(
                "{}: the argument for @{} calls a function that gives a new value each time (NEXTVAL, ...) and the body uses it more than once; compute it first",
                def.name, p.name
            )));
        }
        let arg = match p.data_type {
            Some(t) => cast_to(arg, t),
            None => arg,
        };
        map.insert(p.name.to_ascii_lowercase(), arg);
    }
    // First mark the body's parameters, so that parameters in the arguments (a caller's own) are not
    // mistaken for them when the arguments go in.
    let mut out = body.clone();
    let mut unknown: Option<String> = None;
    walk_expr(&mut out, &mut |x| {
        if let Expr::Param(n) = x {
            if map.contains_key(&n.to_ascii_lowercase()) {
                *x = Expr::Param(format!("\u{1}{}", n.to_ascii_lowercase()));
            } else {
                unknown.get_or_insert_with(|| n.clone());
            }
        }
        Ok(())
    })?;
    if let Some(n) = unknown {
        return Err(Error::catalog(format!("the function '{}' uses @{n}, which is not one of its parameters", def.name)));
    }
    walk_expr(&mut out, &mut |x| {
        if let Expr::Param(n) = x {
            if let Some(rest) = n.strip_prefix('\u{1}') {
                *x = map[rest].clone();
            }
        }
        Ok(())
    })?;
    Ok(match def.returns {
        Some(t) => cast_to(out, t),
        None => out,
    })
}

/// Where a block of procedure statements ended.
enum Flow {
    Normal,
    Leave,
    Return,
}

struct Frame {
    vars: HashMap<String, Value>,
    types: HashMap<String, DataType>,
    last: Option<QueryResult>,
    returned: Option<Value>,
    iterations: u64,
}

impl Database {
    pub(crate) fn exec_create_routine(&mut self, def: &RoutineDef) -> Result<QueryResult> {
        let k = key(&def.name);
        if self.routines.contains_key(&k) {
            return Err(Error::catalog(format!("the function or procedure '{}' already exists", def.name)));
        }
        if def.function && ScalarFn::lookup(&def.name).is_some() || crate::exec::agg::AggFn::from_name(&def.name).is_some() {
            return Err(Error::catalog(format!("'{}' is the name of a built-in function", def.name)));
        }
        if self.view_sql(&def.name).is_some() && !def.function {
            return Err(Error::catalog(format!("'{}' is already the name of a saved query", def.name)));
        }
        match &def.body {
            RoutineBody::Expr(_) => {
                // Dry run: the body must read, with NULL for every argument.
                let nulls: Vec<Expr> = def.params.iter().map(|_| Expr::Literal(Value::Null)).collect();
                let mut probe = RoutineDef { name: def.name.clone(), ..def.clone() };
                probe.params.iter_mut().for_each(|p| p.default = None);
                let body = inline_call(&probe, &nulls)?;
                let scope = super::bind::Scope::default();
                super::bind::Binder::new(self, &scope, 0).bind(&body)?;
            }
            RoutineBody::Block(_) => {}
        }
        self.apply_all(vec![crate::storage::Op::CreateRoutine { name: def.name.clone(), sql: def.sql.clone() }])?;
        Ok(QueryResult::default())
    }

    pub(crate) fn exec_drop_routine(&mut self, name: &str, function: bool) -> Result<QueryResult> {
        let found = self
            .routines
            .get(&key(name))
            .filter(|r| r.function == function)
            .map(|r| r.name.clone())
            .ok_or_else(|| Error::catalog(format!("the {} '{name}' does not exist", if function { "function" } else { "procedure" })))?;
        self.apply_all(vec![crate::storage::Op::DropRoutine(found)])?;
        Ok(QueryResult::default())
    }

    /// `CALL name (args)` for a stored procedure, or `None` if there is no such procedure.
    /// A stored function or procedure of any kind.
    pub(crate) fn routine_def(&self, name: &str) -> Option<Arc<RoutineDef>> {
        self.routines.get(&key(name)).cloned()
    }

    /// Runs a trigger function's block with `vars` as its variables; gives what it returned and the variables as they ended.
    pub(crate) fn run_trigger_block(&mut self, def: &RoutineDef, vars: HashMap<String, Value>) -> Result<(Option<Value>, HashMap<String, Value>)> {
        let RoutineBody::Block(body) = &def.body else {
            return Err(Error::catalog(format!("'{}' is not a trigger function", def.name)));
        };
        if self.call_depth.load(Ordering::Relaxed) >= MAX_CALLS {
            return Err(Error::runtime("trigger functions are nested too deeply"));
        }
        let mut frame = Frame { vars, types: HashMap::new(), last: None, returned: None, iterations: 0 };
        self.call_depth.fetch_add(1, Ordering::Relaxed);
        let outcome = self.run_block(body, &mut frame);
        self.call_depth.fetch_sub(1, Ordering::Relaxed);
        outcome?;
        Ok((frame.returned, frame.vars))
    }

    pub(crate) fn call_procedure(&mut self, name: &str, args: &[(Option<String>, Expr)]) -> Result<Option<QueryResult>> {
        self.call_routine(name, args, false)
    }

    /// Runs the calls of functions with a block for a body (PL/pgSQL, several SQL statements) that sit in `st` with
    /// arguments that are known now, and puts their results in their place. `None` if there is nothing to run.
    /// A call with a column for an argument is left, and fails when the statement is bound.
    pub(crate) fn run_block_functions(&mut self, st: &Statement) -> Result<Option<Statement>> {
        if !self.routines.values().any(|r| r.function && matches!(r.body, RoutineBody::Block(_))) {
            return Ok(None);
        }
        let mut copy = st.clone();
        let mut any = false;
        for _ in 0..8 {
            let mut changed = false;
            crate::sql::walk::walk_statement(&mut copy, &mut |e| {
                let Expr::Function { name, args, .. } = e else { return Ok(()) };
                let Some(def) = self.function_def(name) else { return Ok(()) };
                if !matches!(def.body, RoutineBody::Block(_)) {
                    return Ok(());
                }
                let mut given = Vec::with_capacity(args.len());
                for a in args.iter() {
                    match self.eval_const(a) {
                        Ok(v) => given.push((None, Expr::Literal(v))),
                        Err(_) => return Ok(()),
                    }
                }
                let result = self.call_routine(&def.name, &given, true)?.unwrap_or_default();
                let mut v = match result.columns.first().map(String::as_str) {
                    Some("return") => result.rows.into_iter().next().and_then(|r| r.into_iter().next()).unwrap_or(Value::Null),
                    _ => Value::Null,
                };
                if let (Some(t), false) = (def.returns, v.is_null()) {
                    v = v.coerce_to(t)?;
                }
                *e = Expr::Literal(v);
                changed = true;
                Ok(())
            })?;
            any |= changed;
            if !changed {
                break;
            }
        }
        Ok(any.then_some(copy))
    }

    pub(crate) fn call_routine(&mut self, name: &str, args: &[(Option<String>, Expr)], allow_function: bool) -> Result<Option<QueryResult>> {
        let Some(def) = self.routines.get(&key(name)).cloned() else { return Ok(None) };
        if def.function && !allow_function {
            return Err(Error::catalog(format!("'{name}' is a function; use it in an expression")));
        }
        let RoutineBody::Block(body) = &def.body else {
            return Err(Error::catalog(format!("'{}' has one expression for a body, so it cannot be run with CALL; write its statements between BEGIN and END", def.name)));
        };
        if self.call_depth.load(Ordering::Relaxed) >= MAX_CALLS {
            return Err(Error::runtime("procedures are nested too deeply (does one call itself?)"));
        }
        // Arguments, positionally or by name, then defaults.
        let mut given: Vec<Option<Value>> = vec![None; def.params.len()];
        let mut next = 0;
        for (pname, e) in args {
            let v = self.eval_const(e)?;
            let at = match pname {
                Some(n) => def
                    .params
                    .iter()
                    .position(|p| p.name.eq_ignore_ascii_case(n))
                    .ok_or_else(|| Error::runtime(format!("{} has no parameter @{n}", def.name)))?,
                None => {
                    while given.get(next).is_some_and(Option::is_some) {
                        next += 1;
                    }
                    next += 1;
                    if next > def.params.len() {
                        return Err(Error::runtime(format!("too many arguments: {} takes {} parameter(s)", def.name, def.params.len())));
                    }
                    next - 1
                }
            };
            if given[at].replace(v).is_some() {
                return Err(Error::runtime(format!("the parameter @{} was supplied twice", def.params[at].name)));
            }
        }
        let mut frame = Frame { vars: HashMap::new(), types: HashMap::new(), last: None, returned: None, iterations: 0 };
        for (p, v) in def.params.iter().zip(given) {
            let v = match (v, &p.default) {
                (Some(v), _) => v,
                (None, Some(d)) => self.eval_const(d)?,
                (None, None) => return Err(Error::runtime(format!("{} needs a value for @{}", def.name, p.name))),
            };
            let k = p.name.to_ascii_lowercase();
            let v = match p.data_type {
                Some(t) if !v.is_null() => {
                    frame.types.insert(k.clone(), t);
                    v.coerce_to(t).map_err(|e| Error::type_err(format!("parameter @{}: {e}", p.name)))?
                }
                Some(t) => {
                    frame.types.insert(k.clone(), t);
                    v
                }
                None => v,
            };
            frame.vars.insert(k, v);
        }
        self.call_depth.fetch_add(1, Ordering::Relaxed);
        let outcome = self.atomically(|db| db.run_block(body, &mut frame).map(|_| ()));
        self.call_depth.fetch_sub(1, Ordering::Relaxed);
        outcome?;
        Ok(Some(match (frame.returned, frame.last) {
            (Some(v), _) => QueryResult { columns: vec!["return".into()], rows: vec![vec![v]], affected: 0 },
            (None, Some(r)) => r,
            (None, None) => QueryResult::default(),
        }))
    }

    fn eval_in(&self, e: &Expr, frame: &Frame) -> Result<Value> {
        let mut e = e.clone();
        bind_params_exprs(std::slice::from_mut(&mut e), &frame.vars)?;
        self.eval_const(&e)
    }

    fn run_block(&mut self, body: &[ProcStmt], frame: &mut Frame) -> Result<Flow> {
        for st in body {
            match self.run_proc_stmt(st, frame)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn run_proc_stmt(&mut self, st: &ProcStmt, frame: &mut Frame) -> Result<Flow> {
        match st {
            ProcStmt::Declare { name, data_type, init } => {
                let k = name.to_ascii_lowercase();
                let v = match init {
                    Some(e) => self.eval_in(e, frame)?,
                    None => Value::Null,
                };
                let v = match data_type {
                    Some(t) => {
                        frame.types.insert(k.clone(), *t);
                        v.coerce_to(*t)?
                    }
                    None => v,
                };
                frame.vars.insert(k, v);
            }
            ProcStmt::Set { name, value } => {
                let k = name.to_ascii_lowercase();
                if !frame.vars.contains_key(&k) {
                    return Err(Error::runtime(format!("@{name} has not been declared (use DECLARE @{name})")));
                }
                let mut v = self.eval_in(value, frame)?;
                if let Some(t) = frame.types.get(&k) {
                    v = v.coerce_to(*t)?;
                }
                frame.vars.insert(k, v);
            }
            ProcStmt::If { branches, otherwise } => {
                for (cond, body) in branches {
                    if matches!(self.eval_in(cond, frame)?, Value::Bool(true)) {
                        return self.run_block(body, frame);
                    }
                }
                return self.run_block(otherwise, frame);
            }
            ProcStmt::While { cond, body } => {
                while matches!(self.eval_in(cond, frame)?, Value::Bool(true)) {
                    frame.iterations += 1;
                    if frame.iterations > MAX_ITERATIONS {
                        return Err(Error::runtime("a procedure made more than a million loop iterations (is a loop endless?)"));
                    }
                    match self.run_block(body, frame)? {
                        Flow::Normal => {}
                        Flow::Leave => break,
                        Flow::Return => return Ok(Flow::Return),
                    }
                }
            }
            ProcStmt::Leave => return Ok(Flow::Leave),
            ProcStmt::Return(value) => {
                if let Some(e) = value {
                    frame.returned = Some(self.eval_in(e, frame)?);
                }
                return Ok(Flow::Return);
            }
            ProcStmt::SelectRecord { name, select, strict, targets } => {
                let (columns, rows) = self.record_query(select, frame)?;
                if *strict && rows.len() != 1 {
                    return Err(Error::runtime(if rows.is_empty() { "query returned no rows" } else { "query returned more than one row" }));
                }
                let row = rows.into_iter().next();
                if targets.is_empty() {
                    self.set_record(frame, name, &columns, row);
                } else {
                    for (i, t) in targets.iter().enumerate() {
                        let mut v = row.as_ref().and_then(|r| r.get(i).cloned()).unwrap_or(Value::Null);
                        let k = t.to_ascii_lowercase();
                        if let Some(ty) = frame.types.get(&k) {
                            v = v.coerce_to(*ty)?;
                        }
                        frame.vars.insert(k, v);
                    }
                }
            }
            ProcStmt::ForRecord { name, select, body } => {
                let (columns, rows) = self.record_query(select, frame)?;
                for row in rows {
                    frame.iterations += 1;
                    if frame.iterations > MAX_ITERATIONS {
                        return Err(Error::runtime("a procedure made more than a million loop iterations (is a loop endless?)"));
                    }
                    self.set_record(frame, name, &columns, Some(row));
                    match self.run_block(body, frame)? {
                        Flow::Normal => {}
                        Flow::Leave => break,
                        Flow::Return => return Ok(Flow::Return),
                    }
                }
            }
            ProcStmt::Sql(Statement::Raise(message)) => {
                let text = self.eval_in(message, frame)?;
                return Err(Error::constraint(text.to_string()));
            }
            ProcStmt::Sql(stmt) => {
                let mut stmt = stmt.clone();
                bind_params(&mut stmt, &frame.vars)?;
                frame.last = Some(self.execute_statement(&stmt)?);
            }
        }
        Ok(Flow::Normal)
    }

    /// Runs the query of a record variable's `SELECT ... INTO` / `FOR ... IN`, with the variables filled in.
    fn record_query(&mut self, select: &crate::sql::ast::Select, frame: &Frame) -> Result<(Vec<String>, Vec<Vec<Value>>)> {
        let mut stmt = Statement::Select(select.clone());
        bind_params(&mut stmt, &frame.vars)?;
        let r = self.execute_statement(&stmt)?;
        Ok((r.columns, r.rows))
    }

    /// Sets the variables `name.column` from `row` (NULL for all of them when there is none).
    fn set_record(&self, frame: &mut Frame, name: &str, columns: &[String], row: Option<Vec<Value>>) {
        for (i, c) in columns.iter().enumerate() {
            let v = row.as_ref().and_then(|r| r.get(i).cloned()).unwrap_or(Value::Null);
            frame.vars.insert(format!("{}.{}", name.to_ascii_lowercase(), c.to_ascii_lowercase()), v);
        }
    }

    /// The routine called `name`, if it is a function.
    pub(crate) fn function_def(&self, name: &str) -> Option<Arc<RoutineDef>> {
        self.routines.get(&key(name)).filter(|r| r.function).cloned()
    }
}
