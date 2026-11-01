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

pub(crate) fn cast_to(e: Expr, ty: DataType) -> Expr {
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
    /// `LEAVE label` (MySQL): out to the block or loop with that label.
    LeaveTo(String),
    /// `ITERATE label`: on with the next round of the loop with that label.
    IterateTo(String),
    Return,
}

fn handler_matches(h: &Handler, e: &Error) -> bool {
    let state = e.sqlstate();
    let class = state.get(..2).unwrap_or("");
    h.conditions.iter().any(|c| match c {
        HandlerCond::SqlException => !matches!(class, "00" | "01" | "02"),
        HandlerCond::SqlWarning => class == "01",
        HandlerCond::NotFound => class == "02",
        HandlerCond::SqlState(s) => *s == state,
        HandlerCond::Code(n) => *n == e.errno(),
    })
}

/// A function that returns a set of rows with `RETURN NEXT` / `RETURN QUERY` statements, rather than as a query of its own
/// (a `LANGUAGE sql` body ends with the query whose rows it gives).
fn returns_rows_by_statements(d: &RoutineDef) -> bool {
    d.function && !d.set_columns.is_empty() && matches!(&d.body, RoutineBody::Block(b) if !matches!(b.last(), Some(ProcStmt::Sql(Statement::Select(_)))))
}

struct Frame {
    vars: HashMap<String, Value>,
    types: HashMap<String, DataType>,
    last: Option<QueryResult>,
    returned: Option<Value>,
    iterations: u64,
    /// A function that returns a set: the names of its output columns, and the rows `RETURN NEXT` / `RETURN QUERY` made.
    set_names: Vec<String>,
    rows: Vec<Vec<Value>>,
    /// The columns of each record variable, in order.
    rec_cols: HashMap<String, Vec<String>>,
    /// The cursors declared, with the rows of each that is open and how many were fetched.
    cursors: HashMap<String, (crate::sql::ast::Select, Option<(Vec<Vec<Value>>, usize)>)>,
    /// The label of the loop about to run.
    loop_label: Option<String>,
    /// The condition a handler is dealing with (what `GET DIAGNOSTICS` and `RESIGNAL` read).
    diag: Option<crate::error::Diag>,
    /// The handlers of the blocks being run, innermost last.
    handlers: Vec<Handler>,
}

impl Frame {
    fn new(vars: HashMap<String, Value>) -> Frame {
        Frame { vars, types: HashMap::new(), last: None, returned: None, iterations: 0, set_names: Vec::new(), rows: Vec::new(), rec_cols: HashMap::new(), cursors: HashMap::new(), loop_label: None, diag: None, handlers: Vec::new() }
    }
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
        let mut frame = Frame::new(vars);
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
        if self.routines.values().any(|r| returns_rows_by_statements(r)) {
            match &mut copy {
                Statement::Select(s) => any |= self.materialise_set_calls(s, 0, false)?,
                Statement::Insert { source: crate::sql::ast::InsertSource::Select(s), .. } => any |= self.materialise_set_calls(s, 0, false)?,
                _ => {}
            }
        }
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

    /// Runs the calls in FROM of functions that return a set with a block for a body (`RETURN NEXT`), when their arguments
    /// are known, and puts their rows in their place; the same inside views, derived tables and sub-queries.
    pub(crate) fn materialise_set_calls(&mut self, s: &mut crate::sql::ast::Select, depth: u32, dry: bool) -> Result<bool> {
        use crate::sql::ast::TableFactor;
        if depth > 8 {
            return Ok(false);
        }
        let mut any = false;
        if let Some(with) = &mut s.with {
            for cte in &mut with.ctes {
                any |= self.materialise_set_calls(&mut cte.query, depth + 1, dry)?;
            }
        }
        for u in &mut s.unions {
            any |= self.materialise_set_calls(&mut u.select, depth + 1, dry)?;
        }
        let mut subqueries: Vec<*mut crate::sql::ast::Select> = Vec::new();
        let mut collect = |e: &mut Expr| {
            match e {
                Expr::Subquery(q) | Expr::Exists { query: q, .. } | Expr::InSelect { query: q, .. } => subqueries.push(&mut **q as *mut _),
                _ => {}
            }
            Ok(())
        };
        if let Some(f) = &mut s.filter {
            crate::sql::walk::walk_expr(f, &mut collect)?;
        }
        for item in &mut s.items {
            if let crate::sql::ast::SelectItem::Expr { expr, .. } = item {
                crate::sql::walk::walk_expr(expr, &mut collect)?;
            }
        }
        for q in subqueries {
            // SAFETY: each pointer is to a sub-query boxed inside `s`, which is not touched while this runs
            any |= self.materialise_set_calls(unsafe { &mut *q }, depth + 1, dry)?;
        }
        let Some(from) = &mut s.from else { return Ok(any) };
        let factors = std::iter::once(&mut from.first).chain(from.joins.iter_mut().map(|j| &mut j.factor));
        for f in factors {
            match f {
                TableFactor::Function { name, args, alias, .. } => {
                    let Some(def) = self.routine_def(name).filter(|d| returns_rows_by_statements(d)) else { continue };
                    let mut vals = Vec::with_capacity(args.len());
                    let mut known = true;
                    for a in args.iter() {
                        match self.eval_const(a) {
                            Ok(v) => vals.push(v),
                            Err(_) => {
                                known = false;
                                break;
                            }
                        }
                    }
                    if !known {
                        continue;
                    }
                    let (columns, rows) = if dry { (self.set_columns_of(&def), Vec::new()) } else { self.run_set_block(&def, &vals)? };
                    let columns = if columns.len() == 1 && crate::dialect::current() == crate::dialect::Dialect::Postgresql { vec![alias.clone()] } else { columns };
                    *f = TableFactor::Materialized { rows: Arc::new(rows), columns, alias: alias.clone() };
                    any = true;
                }
                TableFactor::Derived { query, .. } => any |= self.materialise_set_calls(query, depth + 1, dry)?,
                TableFactor::Named { name, alias, .. } if self.table(name).is_none() => {
                    let Some(sql) = self.view_sql(name).map(str::to_string) else { continue };
                    let Ok(mut statements) = crate::sql::parse_script(&sql) else { continue };
                    let Some(Statement::Select(mut inner)) = statements.pop() else { continue };
                    if self.materialise_set_calls(&mut inner, depth + 1, dry)? {
                        *f = TableFactor::Derived { query: Box::new(inner), alias: alias.clone().unwrap_or_else(|| name.clone()), columns: Vec::new(), lateral: false };
                        any = true;
                    }
                }
                _ => {}
            }
        }
        Ok(any)
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
        let mut arg_exprs: Vec<Option<Expr>> = vec![None; def.params.len()];
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
            arg_exprs[at] = Some(e.clone());
        }
        let mut frame = Frame::new(HashMap::new());
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
        // the OUT and INOUT parameters go back to the variables the caller named
        for (p, arg) in def.params.iter().zip(&arg_exprs) {
            if let (true, Some(Expr::Param(var))) = (p.out, arg) {
                let v = frame.vars.get(&p.name.to_ascii_lowercase()).cloned().unwrap_or(Value::Null);
                self.user_vars.insert(var.to_ascii_lowercase(), v);
            }
        }
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

    /// A block with handlers: an error in one of its statements goes to the first handler that answers to it; a `CONTINUE`
    /// handler lets the block go on with the next statement, an `EXIT` handler ends the block.
    fn run_block_handled(&mut self, body: &[ProcStmt], handlers: &[Handler], frame: &mut Frame) -> Result<Flow> {
        let depth = frame.handlers.len();
        frame.handlers.extend(handlers.iter().cloned());
        let flow = self.run_block_handled_inner(body, handlers, frame);
        frame.handlers.truncate(depth);
        flow
    }

    fn run_block_handled_inner(&mut self, body: &[ProcStmt], handlers: &[Handler], frame: &mut Frame) -> Result<Flow> {
        for st in body {
            match self.run_proc_stmt(st, frame) {
                Ok(Flow::Normal) => {}
                Ok(other) => return Ok(other),
                Err(e) => {
                    let Some(h) = handlers.iter().find(|h| handler_matches(h, &e)) else { return Err(e) };
                    frame.diag = Some(e.diag());
                    match self.run_block(&h.body, frame)? {
                        Flow::Normal => {}
                        other => return Ok(other),
                    }
                    if h.exit {
                        return Ok(Flow::Normal);
                    }
                }
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
                let mine = frame.loop_label.take();
                while matches!(self.eval_in(cond, frame)?, Value::Bool(true)) {
                    frame.iterations += 1;
                    if frame.iterations > MAX_ITERATIONS {
                        return Err(Error::runtime("a procedure made more than a million loop iterations (is a loop endless?)"));
                    }
                    match self.run_block(body, frame)? {
                        Flow::Normal => {}
                        Flow::Leave => break,
                        Flow::IterateTo(l) if mine.as_deref() == Some(l.as_str()) => {}
                        other => return Ok(other),
                    }
                }
            }
            ProcStmt::Labeled { label, stmt } => {
                frame.loop_label = Some(label.clone());
                let flow = self.run_proc_stmt(stmt, frame)?;
                frame.loop_label = None;
                return Ok(match flow {
                    Flow::LeaveTo(l) if l.eq_ignore_ascii_case(label) => Flow::Normal,
                    other => other,
                });
            }
            ProcStmt::Block { label, body, handlers } => {
                let flow = self.run_block_handled(body, handlers, frame)?;
                return Ok(match flow {
                    Flow::LeaveTo(l) if label.as_deref().is_some_and(|b| b.eq_ignore_ascii_case(&l)) => Flow::Normal,
                    other => other,
                });
            }
            ProcStmt::Leave => return Ok(Flow::Leave),
            ProcStmt::LeaveTo(label) => return Ok(Flow::LeaveTo(label.clone())),
            ProcStmt::Iterate(label) => return Ok(Flow::IterateTo(label.clone())),
            ProcStmt::DeclareCursor { name, select } => {
                frame.cursors.insert(name.to_ascii_lowercase(), ((**select).clone(), None));
            }
            ProcStmt::OpenCursor(name) => {
                let k = name.to_ascii_lowercase();
                let select = frame.cursors.get(&k).map(|c| c.0.clone()).ok_or_else(|| Error::runtime(format!("the cursor '{name}' is not declared")))?;
                let (_, rows) = self.record_query(&select, frame)?;
                if let Some(c) = frame.cursors.get_mut(&k) {
                    c.1 = Some((rows, 0));
                }
            }
            ProcStmt::CloseCursor(name) => {
                let k = name.to_ascii_lowercase();
                match frame.cursors.get_mut(&k) {
                    Some(c) if c.1.is_some() => c.1 = None,
                    _ => return Err(Error::runtime(format!("the cursor '{name}' is not open"))),
                }
            }
            ProcStmt::Fetch { cursor, targets } => {
                let k = cursor.to_ascii_lowercase();
                let row = match frame.cursors.get_mut(&k) {
                    Some((_, Some((rows, pos)))) => {
                        let row = rows.get(*pos).cloned();
                        if row.is_some() {
                            *pos += 1;
                        }
                        row
                    }
                    _ => return Err(Error::runtime(format!("the cursor '{cursor}' is not open"))),
                };
                let Some(row) = row else { return Err(Error::runtime("No data to FETCH")) };
                for (i, t) in targets.iter().enumerate() {
                    let mut v = row.get(i).cloned().unwrap_or(Value::Null);
                    let tk = t.to_ascii_lowercase();
                    if let Some(ty) = frame.types.get(&tk) {
                        v = v.coerce_to(*ty)?;
                    }
                    frame.vars.insert(tk, v);
                }
            }
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
                        other => return Ok(other),
                    }
                }
            }
            ProcStmt::ReturnNext(value) => {
                let row = match value {
                    Some(e) => vec![self.eval_in(e, frame)?],
                    None => frame.set_names.iter().map(|n| frame.vars.get(&n.to_ascii_lowercase()).cloned().unwrap_or(Value::Null)).collect(),
                };
                self.add_set_row(frame, row)?;
            }
            ProcStmt::ReturnNextRecord(name) => {
                let cols = frame.rec_cols.get(name).cloned().unwrap_or_default();
                let row = cols.iter().map(|c| frame.vars.get(&format!("{name}.{c}")).cloned().unwrap_or(Value::Null)).collect();
                self.add_set_row(frame, row)?;
            }
            ProcStmt::ReturnQuery(select) => {
                let (_, rows) = self.record_query(select, frame)?;
                for row in rows {
                    self.add_set_row(frame, row)?;
                }
            }
            ProcStmt::Sql(Statement::Raise(message)) => {
                let text = self.eval_in(message, frame)?;
                return Err(Error::constraint(text.to_string()));
            }
            ProcStmt::Sql(stmt @ (Statement::Signal { .. } | Statement::GetDiagnostics { .. })) => {
                let mut stmt = stmt.clone();
                bind_params(&mut stmt, &frame.vars)?;
                match &stmt {
                    Statement::Signal { resignal, state, items } => {
                        let err = self.signal_error(*resignal, state, items, frame.diag.as_ref()).unwrap_or_else(|e| e);
                        // a warning (class 01) stops nothing unless a handler is there for it
                        if err.sqlstate().starts_with("01") && !frame.handlers.iter().any(|h| handler_matches(h, &err)) {
                            frame.diag = Some(err.diag());
                            return Ok(Flow::Normal);
                        }
                        return Err(err);
                    }
                    Statement::GetDiagnostics { stacked, condition, items } => {
                        let affected = frame.last.as_ref().map_or(0, |r| r.affected as i64);
                        let diag = frame.diag.clone();
                        for ((name, user), value) in self.diagnostics_values(*stacked, condition.as_ref(), items, diag.as_ref(), affected)? {
                            if user {
                                self.user_vars.insert(name.to_ascii_lowercase(), value);
                            } else {
                                let k = name.to_ascii_lowercase();
                                if !frame.vars.contains_key(&k) {
                                    return Err(Error::runtime(format!("@{name} has not been declared (use DECLARE @{name})")));
                                }
                                let value = match frame.types.get(&k) {
                                    Some(t) if !value.is_null() => value.coerce_to(*t)?,
                                    _ => value,
                                };
                                frame.vars.insert(k, value);
                            }
                        }
                    }
                    _ => {}
                }
            }
            ProcStmt::Sql(stmt) => {
                let mut stmt = stmt.clone();
                // CALL p(v): an OUT or INOUT argument that names a variable of this routine goes through a
                // temporary user variable, and comes back into the variable afterwards
                let mut outs: Vec<(String, String)> = Vec::new();
                if let Statement::Exec { name, args } = &mut stmt {
                    if let Some(def) = self.routines.get(&key(name)).cloned() {
                        let mut next = 0;
                        for (pname, e) in args.iter_mut() {
                            let at = match pname {
                                Some(pn) => def.params.iter().position(|p| p.name.eq_ignore_ascii_case(pn)),
                                None => {
                                    next += 1;
                                    Some(next - 1)
                                }
                            };
                            let is_out = at.and_then(|i| def.params.get(i)).is_some_and(|p| p.out);
                            if let (true, Expr::Param(var)) = (is_out, &*e) {
                                let k = var.to_ascii_lowercase();
                                if let Some(v) = frame.vars.get(&k).cloned() {
                                    let tmp = format!("__out{}_{k}", outs.len());
                                    self.user_vars.insert(tmp.clone(), v);
                                    *e = Expr::Param(tmp.clone());
                                    outs.push((k, tmp));
                                }
                            }
                        }
                    }
                }
                bind_params(&mut stmt, &frame.vars)?;
                let result = self.execute_statement(&stmt);
                for (k, tmp) in outs {
                    if let Some(v) = self.user_vars.remove(&tmp) {
                        let v = match frame.types.get(&k) {
                            Some(t) if !v.is_null() => v.coerce_to(*t).unwrap_or(v),
                            _ => v,
                        };
                        frame.vars.insert(k, v);
                    }
                }
                frame.last = Some(result?);
            }
        }
        Ok(Flow::Normal)
    }

    /// The error `SIGNAL` / `RESIGNAL` raises. `current` is the condition a `RESIGNAL` starts from.
    pub(crate) fn signal_error(&self, resignal: bool, state: &Option<String>, items: &[(String, Expr)], current: Option<&crate::error::Diag>) -> Result<Error> {
        let mut base = if resignal {
            match current {
                Some(d) => d.clone(),
                None => return Err(Error::signal("0K000", 1645, "RESIGNAL when handler not active".into(), Vec::new())),
            }
        } else {
            crate::error::Diag { state: String::new(), errno: 0, message: String::new(), items: Vec::new() }
        };
        if let Some(s) = state {
            let ok = s.len() == 5 && s.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_uppercase()) && !s.starts_with("00");
            if !ok {
                return Err(Error::signal("42000", 1407, format!("Bad SQLSTATE: '{s}'"), Vec::new()));
            }
            base.state = s.clone();
            if !resignal {
                base.errno = 0;
                base.message.clear();
            }
        }
        let mut explicit_errno = false;
        for (item, e) in items {
            let v = self.eval_const(e)?;
            let text = if v.is_null() { String::new() } else { v.to_string() };
            match item.as_str() {
                "MESSAGE_TEXT" => {
                    if v.is_null() {
                        return Err(Error::signal("42000", 1231, "Variable 'MESSAGE_TEXT' can't be set to the value of 'NULL'".into(), Vec::new()));
                    }
                    base.message = text;
                }
                "MYSQL_ERRNO" => {
                    let n = text.trim().parse::<i64>().ok().filter(|n| *n > 0).ok_or_else(|| Error::signal("42000", 1231, format!("Variable 'MYSQL_ERRNO' can't be set to the value of '{text}'"), Vec::new()))?;
                    base.errno = n;
                    explicit_errno = true;
                }
                "CLASS_ORIGIN" | "SUBCLASS_ORIGIN" | "CONSTRAINT_CATALOG" | "CONSTRAINT_SCHEMA" | "CONSTRAINT_NAME" | "CATALOG_NAME" | "SCHEMA_NAME" | "TABLE_NAME" | "COLUMN_NAME" | "CURSOR_NAME" => {
                    base.items.retain(|(k, _)| k != item);
                    base.items.push((item.clone(), text));
                }
                other => return Err(Error::signal("42000", 1064, format!("You have an error in your SQL syntax near '{other}'"), Vec::new())),
            }
        }
        let class = base.state.get(..2).unwrap_or("").to_string();
        if base.errno == 0 && !explicit_errno {
            base.errno = match class.as_str() {
                "01" => 1642,
                "02" => 1643,
                _ => 1644,
            };
        }
        if base.message.is_empty() {
            base.message = match class.as_str() {
                "01" => "Unhandled user-defined warning condition",
                "02" => "Unhandled user-defined not found condition",
                _ => "Unhandled user-defined exception condition",
            }
            .to_string();
        }
        Ok(Error::signal(&base.state, base.errno, base.message, base.items))
    }

    /// The values `GET DIAGNOSTICS` assigns: `(target, value)` for each item.
    pub(crate) fn diagnostics_values(
        &self,
        _stacked: bool,
        condition: Option<&Expr>,
        items: &[((String, bool), String)],
        diag: Option<&crate::error::Diag>,
        affected: i64,
    ) -> Result<Vec<((String, bool), Value)>> {
        let mut out = Vec::new();
        let n = match condition {
            Some(e) => Some(self.eval_const(e)?.to_f64().unwrap_or(0.0) as i64),
            None => None,
        };
        let mut condition_diag = diag;
        if let Some(n) = n {
            if n < 1 || n > i64::from(diag.is_some()) {
                condition_diag = None;
                if n < 1 || diag.is_some() || n > 1 {
                    return Err(Error::signal("35000", 1758, "Invalid condition number".into(), Vec::new()));
                }
            }
        }
        for (target, item) in items {
            let value = if condition.is_none() {
                match item.as_str() {
                    "NUMBER" => Value::Int(i64::from(diag.is_some())),
                    "ROW_COUNT" => Value::Int(affected),
                    other => return Err(Error::syntax(format!("'{other}' is not a statement information item"))),
                }
            } else {
                match (condition_diag, item.as_str()) {
                    (Some(d), "MYSQL_ERRNO") => Value::Int(d.errno),
                    (Some(d), i) => d.item(i).map_or(Value::Null, |t| Value::text(&t)),
                    (None, _) => Value::Null,
                }
            };
            out.push((target.clone(), value));
        }
        Ok(out)
    }

    /// Runs the query of a record variable's `SELECT ... INTO` / `FOR ... IN`, with the variables filled in.
    fn record_query(&mut self, select: &crate::sql::ast::Select, frame: &Frame) -> Result<(Vec<String>, Vec<Vec<Value>>)> {
        let mut stmt = Statement::Select(select.clone());
        bind_params(&mut stmt, &frame.vars)?;
        let r = self.execute_statement(&stmt)?;
        Ok((r.columns, r.rows))
    }

    /// Sets the variables `name.column` from `row` (NULL for all of them when there is none).
    fn add_set_row(&self, frame: &mut Frame, row: Vec<Value>) -> Result<()> {
        frame.iterations += 1;
        if frame.iterations > MAX_ITERATIONS {
            return Err(Error::runtime("a function returned more than a million rows"));
        }
        frame.rows.push(row);
        Ok(())
    }

    /// Runs a function that returns a set of rows (`RETURN NEXT` / `RETURN QUERY`) and gives the rows.
    pub(crate) fn run_set_block(&mut self, def: &RoutineDef, args: &[Value]) -> Result<(Vec<String>, Vec<Vec<Value>>)> {
        let RoutineBody::Block(body) = &def.body else { return Err(Error::catalog(format!("'{}' does not return rows", def.name))) };
        if args.len() > def.params.len() {
            return Err(Error::runtime(format!("{} takes {} argument(s), not {}", def.name, def.params.len(), args.len())));
        }
        let mut frame = Frame::new(HashMap::new());
        for (i, p) in def.params.iter().enumerate() {
            let v = match (args.get(i), &p.default) {
                (Some(v), _) => v.clone(),
                (None, Some(d)) => self.eval_const(d)?,
                (None, None) => return Err(Error::runtime(format!("{} needs a value for @{}", def.name, p.name))),
            };
            let k = p.name.to_ascii_lowercase();
            let v = match p.data_type {
                Some(t) => {
                    frame.types.insert(k.clone(), t);
                    if v.is_null() { v } else { v.coerce_to(t)? }
                }
                None => v,
            };
            frame.vars.insert(k, v);
        }
        let names = self.set_columns_of(def);
        for n in def.set_columns.iter().filter(|c| !c.starts_with('\u{1}')) {
            frame.vars.insert(n.to_ascii_lowercase(), Value::Null);
        }
        frame.set_names = names.clone();
        if self.call_depth.load(Ordering::Relaxed) >= MAX_CALLS {
            return Err(Error::runtime("functions are nested too deeply"));
        }
        self.call_depth.fetch_add(1, Ordering::Relaxed);
        let outcome = self.atomically(|db| db.run_block(body, &mut frame).map(|_| ()));
        self.call_depth.fetch_sub(1, Ordering::Relaxed);
        outcome?;
        Ok((names, frame.rows))
    }

    /// The output columns of a function that returns a set: those it names, or the columns of the table it returns.
    pub(crate) fn set_columns_of(&self, def: &RoutineDef) -> Vec<String> {
        match def.set_columns.as_slice() {
            [one] if one.starts_with('\u{1}') => {
                let table = &one["\u{1}setof:".len()..];
                self.table(table).map(|t| t.schema().columns.iter().map(|c| c.name.clone()).collect()).unwrap_or_default()
            }
            cols => cols.to_vec(),
        }
    }

    fn set_record(&self, frame: &mut Frame, name: &str, columns: &[String], row: Option<Vec<Value>>) {
        frame.rec_cols.insert(name.to_ascii_lowercase(), columns.iter().map(|c| c.to_ascii_lowercase()).collect());
        for (i, c) in columns.iter().enumerate() {
            let v = row.as_ref().and_then(|r| r.get(i).cloned()).unwrap_or(Value::Null);
            frame.vars.insert(format!("{}.{}", name.to_ascii_lowercase(), c.to_ascii_lowercase()), v);
        }
    }

    /// The aggregate made with `CREATE AGGREGATE` called `name`.
    pub(crate) fn aggregate_def(&self, name: &str) -> Option<Arc<RoutineDef>> {
        self.routines.get(&key(name)).filter(|r| r.aggregate.is_some()).cloned()
    }

    /// Tells the parser and binder (through a thread-local) which names are user-defined aggregates.
    pub(crate) fn sync_aggregates(&self) {
        let names = self.routines.iter().filter(|(_, r)| r.aggregate.is_some()).map(|(k, _)| k.to_ascii_lowercase()).collect();
        crate::exec::agg::set_user_aggregates(names);
    }

    /// The routine called `name`, if it is a function.
    pub(crate) fn function_def(&self, name: &str) -> Option<Arc<RoutineDef>> {
        self.routines.get(&key(name)).filter(|r| r.function && r.aggregate.is_none()).cloned()
    }
}
