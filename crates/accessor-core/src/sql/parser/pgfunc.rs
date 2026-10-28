//! PostgreSQL-style `CREATE FUNCTION` and `CREATE PROCEDURE`.
//!
//! ```sql
//! CREATE FUNCTION add(a int, b int) RETURNS int AS $$ SELECT a + b $$ LANGUAGE sql;
//! CREATE FUNCTION fact(n int) RETURNS int AS $$
//! DECLARE r int := 1;
//! BEGIN
//!   WHILE n > 1 LOOP r := r * n; n := n - 1; END LOOP;
//!   RETURN r;
//! END $$ LANGUAGE plpgsql;
//! ```
//!
//! Both are read into the forms the engine already has: a `LANGUAGE sql` function whose body is one `SELECT` becomes
//! an expression (so it is inlined into the queries that call it); anything with several statements, and every
//! PL/pgSQL body that is more than `RETURN expr`, becomes a block of procedure statements (`ProcStmt`). Names in the
//! body that are parameters or variables are turned into the `@name` parameters those forms use.
//!
//! PL/pgSQL covered: `DECLARE`, `:=`/`=` assignment, `IF`/`ELSIF`/`ELSE`, `CASE`, `WHILE`, `LOOP`, `FOR i IN a..b`,
//! `EXIT [WHEN]`, `RETURN`, `RAISE`, `PERFORM`, `SELECT ... INTO`, and plain SQL statements. Left out, with an error
//! that says so: `EXCEPTION` handlers, cursors, `FOR ... IN query`, `RETURN NEXT/QUERY`, `EXECUTE`, arrays, records,
//! `CONTINUE`, set-returning functions and trigger functions.

use super::{Parser, Tok};
use crate::error::{Error, Result};
use crate::sql::ast::*;
use crate::sql::lexer::lex;
use crate::sql::usertypes;
use crate::sql::walk::{walk_expr, walk_statement};
use crate::value::{DataType, Value};

/// Names of the parameters and variables a body may use, lower-case, with the position of the parameters.
struct Names {
    all: Vec<String>,
    params: Vec<String>,
    /// In a trigger function, `NEW.x` and `OLD.x` are variables named `new.x` and `old.x`.
    trigger: bool,
}

impl Names {
    fn has(&self, n: &str) -> bool {
        self.all.iter().any(|v| v.eq_ignore_ascii_case(n))
    }

    /// Turns the names in `e` that are parameters or variables into `@name`, and `$n` into the n-th parameter.
    fn expr(&self, e: &mut Expr) -> Result<()> {
        walk_expr(e, &mut |x| {
            self.node(x);
            Ok(())
        })
    }

    fn statement(&self, st: &mut Statement) -> Result<()> {
        walk_statement(st, &mut |x| {
            self.node(x);
            Ok(())
        })
    }

    fn node(&self, x: &mut Expr) {
        match x {
            Expr::Column { table: Some(t), name } if self.trigger && (t.eq_ignore_ascii_case("new") || t.eq_ignore_ascii_case("old")) => {
                *x = Expr::Param(format!("{}.{}", t.to_ascii_lowercase(), name.to_ascii_lowercase()));
            }
            Expr::Column { table: None, name } if self.trigger && (name.eq_ignore_ascii_case("new") || name.eq_ignore_ascii_case("old")) => {
                *x = Expr::Param(name.to_ascii_lowercase());
            }
            Expr::Column { table: None, name } if self.has(name) => *x = Expr::Param(name.clone()),
            Expr::Param(n) if n.starts_with('$') => {
                if let Some(p) = n[1..].parse::<usize>().ok().and_then(|i| self.params.get(i.wrapping_sub(1))) {
                    *x = Expr::Param(p.clone());
                }
            }
            _ => {}
        }
    }
}

fn is_type_word(t: &Tok) -> bool {
    matches!(t, Tok::Ident(s) if DataType::from_sql_name(s).is_some() || usertypes::lookup(s).is_some())
}

fn lit_true() -> Expr {
    Expr::Literal(Value::Bool(true))
}

impl<'a> Parser<'a> {
    /// At the name after `CREATE FUNCTION`: is this the PostgreSQL form (types for parameters, a `LANGUAGE` clause)?
    pub(super) fn looks_like_pg_routine(&self) -> bool {
        let mut i = self.pos;
        while i < self.toks.len() && i < self.pos + 6 && !matches!(self.toks[i].tok, Tok::Sym("(")) {
            i += 1;
        }
        let Some(first) = self.toks.get(i + 1) else { return false };
        if matches!(first.tok, Tok::Param(_)) {
            return false;
        }
        if matches!(first.tok, Tok::Ident(_) | Tok::Quoted(_)) {
            return true;
        }
        // `f()`: PostgreSQL if a LANGUAGE clause comes before the end of the statement
        let mut j = i;
        while let Some(t) = self.toks.get(j) {
            match &t.tok {
                Tok::Sym(";") | Tok::Eof => return false,
                Tok::Ident(s) if s.eq_ignore_ascii_case("LANGUAGE") => return true,
                _ => j += 1,
            }
        }
        false
    }

    fn pg_type(&mut self) -> Result<DataType> {
        if self.peek_sym("[") {
            return Err(Error::syntax("array types are not supported"));
        }
        let ty = self.data_type_name()?;
        if self.peek_sym("[") {
            return Err(Error::syntax("array types are not supported"));
        }
        Ok(ty)
    }

    pub(super) fn pg_routine(&mut self, function: bool, start: usize) -> Result<Statement> {
        let name = self.table_ident()?;
        self.expect_sym("(")?;
        let mut params: Vec<RoutineParam> = Vec::new();
        let mut outs: Vec<DataType> = Vec::new();
        if !self.peek_sym(")") {
            loop {
                let mut out = false;
                if self.eat_kw("IN") {
                } else if self.eat_kw("OUT") {
                    out = true;
                } else if self.peek_kw("INOUT") || self.peek_kw("VARIADIC") {
                    return Err(Error::syntax("INOUT and VARIADIC parameters are not supported"));
                }
                let named = matches!(self.peek(), Tok::Ident(_) | Tok::Quoted(_))
                    && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Quoted(_))
                    && !self.peek_kw_at(1, "DEFAULT")
                    && !(is_type_word(self.peek()) && !is_type_word(self.peek_at(1)));
                let pname = if named { Some(self.ident()?) } else { None };
                let ty = self.pg_type()?;
                if out {
                    outs.push(ty);
                } else {
                    let default = if self.eat_kw("DEFAULT") || self.eat_sym("=") { Some(self.expr()?) } else { None };
                    let pname = pname.unwrap_or_else(|| format!("${}", params.len() + 1));
                    if params.iter().any(|p| p.name.eq_ignore_ascii_case(&pname)) {
                        return Err(Error::syntax(format!("the parameter {pname} is declared twice")));
                    }
                    params.push(RoutineParam { name: pname, data_type: Some(ty), default });
                }
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        self.expect_sym(")")?;
        let mut returns: Option<DataType> = None;
        let mut is_trigger = false;
        if self.eat_kw("RETURNS") {
            if self.peek_kw("TABLE") || self.peek_kw("SETOF") {
                return Err(Error::syntax("functions that return a set of rows are not supported; return a value, or write a query"));
            } else if self.peek_kw("EVENT_TRIGGER") {
                return Err(Error::syntax("event triggers are not supported"));
            } else if self.eat_kw("TRIGGER") {
                is_trigger = true;
            } else if self.peek_kw("VOID") {
                self.advance();
            } else {
                returns = Some(self.pg_type()?);
            }
        }
        match outs.len() {
            0 => {}
            1 if returns.is_none() => returns = Some(outs[0]),
            _ => return Err(Error::syntax("OUT parameters that return several columns are not supported")),
        }
        let (mut language, mut body) = (None::<String>, None::<String>);
        loop {
            if self.eat_kw("LANGUAGE") {
                language = Some(match self.advance() {
                    Tok::Ident(s) | Tok::Str(s) | Tok::Quoted(s) => s.to_ascii_lowercase(),
                    _ => {
                        self.pos -= 1;
                        return Err(self.unexpected("a language name"));
                    }
                });
            } else if self.eat_kw("AS") {
                match self.advance() {
                    Tok::Str(s) => body = Some(s),
                    _ => {
                        self.pos -= 1;
                        return Err(self.unexpected("the function body in quotes ('...' or $$...$$)"));
                    }
                }
                if matches!(self.peek(), Tok::Sym(",")) {
                    return Err(Error::syntax("a function defined by an object file is not supported"));
                }
            } else if self.peek_kw("BEGIN") && self.peek_kw_at(1, "ATOMIC") {
                return Err(Error::syntax("BEGIN ATOMIC bodies are not supported; use AS $$ ... $$"));
            } else if self.eat_kw("IMMUTABLE") || self.eat_kw("STABLE") || self.eat_kw("VOLATILE") || self.eat_kw("STRICT") || self.eat_kw("WINDOW") {
            } else if self.eat_kw("LEAKPROOF") {
            } else if self.peek_kw("NOT") && self.peek_kw_at(1, "LEAKPROOF") {
                self.advance();
                self.advance();
            } else if self.eat_kw("PARALLEL") || self.eat_kw("COST") || self.eat_kw("ROWS") || self.eat_kw("SUPPORT") {
                self.advance();
            } else if self.eat_kw("EXTERNAL") {
            } else if self.eat_kw("SECURITY") {
                self.advance();
            } else if self.eat_kw("CALLED") {
                for w in ["ON", "NULL", "INPUT"] {
                    self.expect_kw(w)?;
                }
            } else if self.peek_kw("RETURNS") && self.peek_kw_at(1, "NULL") {
                for w in ["RETURNS", "NULL", "ON", "NULL", "INPUT"] {
                    self.expect_kw(w)?;
                }
            } else if self.eat_kw("SET") {
                let _ = self.parameter_name()?;
                if self.eat_kw("FROM") {
                    self.expect_kw("CURRENT")?;
                } else {
                    if !self.eat_kw("TO") {
                        self.expect_sym("=")?;
                    }
                    loop {
                        self.advance();
                        if !self.eat_sym(",") {
                            break;
                        }
                    }
                }
            } else {
                break;
            }
        }
        let end = self.toks[self.pos].pos;
        let sql = self.src[start..end].trim().to_string();
        let language = language.ok_or_else(|| Error::syntax("a function needs LANGUAGE sql or LANGUAGE plpgsql"))?;
        let body = body.ok_or_else(|| Error::syntax("a function needs its body: AS $$ ... $$"))?;
        let mut names = Names {
            all: params.iter().map(|p| p.name.clone()).collect(),
            params: params.iter().map(|p| p.name.clone()).collect(),
            trigger: is_trigger,
        };
        if is_trigger {
            if language != "plpgsql" {
                return Err(Error::syntax("a trigger function is written in plpgsql"));
            }
            for v in ["tg_op", "tg_name", "tg_when", "tg_level", "tg_table_name", "tg_relname", "tg_table_schema", "tg_nargs"] {
                names.all.push(v.to_string());
            }
        }
        let routine_body = match language.as_str() {
            "sql" => sql_body(&body, &names, returns.is_some())?,
            "plpgsql" => plpgsql_body(&body, names, returns.is_some())?,
            other => return Err(Error::syntax(format!("the language '{other}' is not available (use sql or plpgsql)"))),
        };
        Ok(Statement::CreateRoutine(Box::new(RoutineDef { name, function, params, returns, body: routine_body, trigger: is_trigger, sql })))
    }

    /// `DROP FUNCTION name (argument types)`: the types are read and ignored (functions are known by name).
    pub(super) fn skip_argument_types(&mut self) -> Result<()> {
        if self.eat_sym("(") {
            while !self.peek_sym(")") && !self.at_eof() {
                self.advance();
            }
            self.expect_sym(")")?;
        }
        Ok(())
    }

    // ---- PL/pgSQL ----

    fn skip_label(&mut self) {
        if self.peek_sym("<") && self.peek_sym_at(1, "<") {
            while !self.at_eof() && !(self.peek_sym(">") && self.peek_sym_at(1, ">")) {
                self.advance();
            }
            self.advance();
            self.advance();
        }
    }

    fn peek_sym_at(&self, n: usize, s: &str) -> bool {
        matches!(self.peek_at(n), Tok::Sym(x) if *x == s)
    }

    fn pl_expr(&mut self, names: &Names) -> Result<Expr> {
        let mut e = self.expr()?;
        names.expr(&mut e)?;
        Ok(e)
    }

    /// `[DECLARE ...] BEGIN ... END [;]`
    fn pl_block(&mut self, names: &mut Names) -> Result<Vec<ProcStmt>> {
        let mut out = Vec::new();
        self.skip_label();
        if self.eat_kw("DECLARE") {
            while !self.peek_kw("BEGIN") {
                if self.at_eof() {
                    return Err(self.unexpected("BEGIN"));
                }
                let name = self.ident()?;
                self.eat_kw("CONSTANT");
                if self.peek_sym("%") || self.peek_kw("RECORD") || self.peek_kw("CURSOR") {
                    return Err(Error::syntax("%TYPE, record and cursor variables are not supported"));
                }
                let ty = self.pg_type()?;
                if self.eat_kw("COLLATE") {
                    self.ident()?;
                }
                if self.eat_kw("NOT") {
                    self.expect_kw("NULL")?;
                }
                let init = if self.eat_sym(":=") || self.eat_sym("=") || self.eat_kw("DEFAULT") { Some(self.pl_expr(names)?) } else { None };
                self.expect_sym(";")?;
                names.all.push(name.clone());
                out.push(ProcStmt::Declare { name, data_type: Some(ty), init });
            }
        }
        self.expect_kw("BEGIN")?;
        out.extend(self.pl_stmts(names, &["END", "EXCEPTION"])?);
        if self.peek_kw("EXCEPTION") {
            return Err(Error::syntax("EXCEPTION handlers are not supported; let the error stop the function"));
        }
        self.expect_kw("END")?;
        if matches!(self.peek(), Tok::Ident(s) if !s.eq_ignore_ascii_case("IF") && !s.eq_ignore_ascii_case("LOOP")) {
            self.advance(); // END label
        }
        Ok(out)
    }

    fn pl_stmts(&mut self, names: &mut Names, stops: &[&str]) -> Result<Vec<ProcStmt>> {
        let mut out = Vec::new();
        loop {
            while self.eat_sym(";") {}
            if self.at_eof() {
                return Err(self.unexpected(stops[0]));
            }
            if stops.iter().any(|s| self.peek_kw(s)) {
                return Ok(out);
            }
            self.pl_stmt(names, &mut out)?;
        }
    }

    fn end_stmt(&mut self) -> Result<()> {
        if self.eat_sym(";") || self.peek_kw("END") || self.at_eof() {
            Ok(())
        } else {
            Err(self.unexpected("';'"))
        }
    }

    fn pl_stmt(&mut self, names: &mut Names, out: &mut Vec<ProcStmt>) -> Result<()> {
        self.skip_label();
        if self.peek_kw("DECLARE") || self.peek_kw("BEGIN") {
            let inner = self.pl_block(names)?;
            out.extend(inner);
            return self.end_stmt();
        }
        if self.eat_kw("NULL") {
            return self.end_stmt();
        }
        if self.eat_kw("RETURN") {
            if self.peek_kw("NEXT") || self.peek_kw("QUERY") {
                return Err(Error::syntax("RETURN NEXT and RETURN QUERY (set-returning functions) are not supported"));
            }
            let value = if self.peek_sym(";") || self.at_eof() { None } else { Some(self.pl_expr(names)?) };
            out.push(ProcStmt::Return(value));
            return self.end_stmt();
        }
        if self.eat_kw("IF") {
            let mut branches = Vec::new();
            let cond = self.pl_expr(names)?;
            self.expect_kw("THEN")?;
            branches.push((cond, self.pl_stmts(names, &["ELSIF", "ELSEIF", "ELSE", "END"])?));
            let mut otherwise = Vec::new();
            loop {
                if self.eat_kw("ELSIF") || self.eat_kw("ELSEIF") {
                    let cond = self.pl_expr(names)?;
                    self.expect_kw("THEN")?;
                    branches.push((cond, self.pl_stmts(names, &["ELSIF", "ELSEIF", "ELSE", "END"])?));
                } else if self.eat_kw("ELSE") {
                    otherwise = self.pl_stmts(names, &["END"])?;
                } else {
                    break;
                }
            }
            self.expect_kw("END")?;
            self.expect_kw("IF")?;
            out.push(ProcStmt::If { branches, otherwise });
            return self.end_stmt();
        }
        if self.eat_kw("CASE") {
            let subject = if self.peek_kw("WHEN") { None } else { Some(self.pl_expr(names)?) };
            let mut branches = Vec::new();
            while self.eat_kw("WHEN") {
                let mut conds: Vec<Expr> = Vec::new();
                loop {
                    let v = self.pl_expr(names)?;
                    conds.push(match &subject {
                        Some(s) => Expr::Binary { op: BinOp::Eq, left: Box::new(s.clone()), right: Box::new(v) },
                        None => v,
                    });
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_kw("THEN")?;
                let cond = conds
                    .into_iter()
                    .reduce(|a, b| Expr::Binary { op: BinOp::Or, left: Box::new(a), right: Box::new(b) })
                    .expect("one condition");
                branches.push((cond, self.pl_stmts(names, &["WHEN", "ELSE", "END"])?));
            }
            let otherwise = if self.eat_kw("ELSE") { self.pl_stmts(names, &["END"])? } else { Vec::new() };
            self.expect_kw("END")?;
            self.expect_kw("CASE")?;
            out.push(ProcStmt::If { branches, otherwise });
            return self.end_stmt();
        }
        if self.eat_kw("WHILE") {
            let cond = self.pl_expr(names)?;
            self.expect_kw("LOOP")?;
            let body = self.pl_stmts(names, &["END"])?;
            self.expect_kw("END")?;
            self.expect_kw("LOOP")?;
            out.push(ProcStmt::While { cond, body });
            return self.end_stmt();
        }
        if self.eat_kw("LOOP") {
            let body = self.pl_stmts(names, &["END"])?;
            self.expect_kw("END")?;
            self.expect_kw("LOOP")?;
            out.push(ProcStmt::While { cond: lit_true(), body });
            return self.end_stmt();
        }
        if self.eat_kw("FOR") {
            let var = self.ident()?;
            self.expect_kw("IN")?;
            let reverse = self.eat_kw("REVERSE");
            let first = self.pl_expr(names)?;
            if !self.eat_sym("..") {
                return Err(Error::syntax("FOR ... IN query LOOP is not supported; only FOR i IN a..b LOOP"));
            }
            let second = self.pl_expr(names)?;
            let step = if self.eat_kw("BY") { self.pl_expr(names)? } else { Expr::Literal(Value::Int(1)) };
            self.expect_kw("LOOP")?;
            names.all.push(var.clone());
            let mut body = self.pl_stmts(names, &["END"])?;
            self.expect_kw("END")?;
            self.expect_kw("LOOP")?;
            let (from, to) = if reverse { (second, first) } else { (first, second) };
            let v = || Box::new(Expr::Param(var.clone()));
            out.push(ProcStmt::Declare { name: var.clone(), data_type: Some(DataType::Integer), init: Some(from) });
            body.push(ProcStmt::Set {
                name: var.clone(),
                value: Expr::Binary { op: if reverse { BinOp::Sub } else { BinOp::Add }, left: v(), right: Box::new(step) },
            });
            out.push(ProcStmt::While {
                cond: Expr::Binary { op: if reverse { BinOp::Ge } else { BinOp::Le }, left: v(), right: Box::new(to) },
                body,
            });
            return self.end_stmt();
        }
        if self.eat_kw("EXIT") {
            if matches!(self.peek(), Tok::Ident(s) if !s.eq_ignore_ascii_case("WHEN")) {
                self.advance(); // a label: only the innermost loop can be left
            }
            if self.eat_kw("WHEN") {
                let cond = self.pl_expr(names)?;
                out.push(ProcStmt::If { branches: vec![(cond, vec![ProcStmt::Leave])], otherwise: Vec::new() });
            } else {
                out.push(ProcStmt::Leave);
            }
            return self.end_stmt();
        }
        if self.peek_kw("CONTINUE") {
            return Err(Error::syntax("CONTINUE is not supported"));
        }
        if self.eat_kw("RAISE") {
            return self.pl_raise(names, out);
        }
        if self.eat_kw("PERFORM") {
            // PERFORM q is SELECT q, its result thrown away
            let at = self.pos - 1;
            self.toks[at].tok = Tok::Ident("SELECT".to_string());
            self.pos = at;
            let mut st = self.statement()?;
            names.statement(&mut st)?;
            out.push(ProcStmt::Sql(st));
            return self.end_stmt();
        }
        if self.peek_kw("EXECUTE") || self.peek_kw("OPEN") || self.peek_kw("FETCH") || self.peek_kw("CLOSE") || self.peek_kw("GET") {
            return Err(Error::syntax("EXECUTE, cursors and GET DIAGNOSTICS are not supported"));
        }
        // NEW.col := expr (a trigger function changing the row)
        if names.trigger
            && (self.peek_kw("NEW") || self.peek_kw("OLD"))
            && self.peek_sym_at(1, ".")
            && matches!(self.peek_at(2), Tok::Ident(_) | Tok::Quoted(_))
            && (self.peek_sym_at(3, ":=") || self.peek_sym_at(3, "="))
        {
            let which = self.ident()?.to_ascii_lowercase();
            self.advance();
            let col = self.ident()?.to_ascii_lowercase();
            self.advance();
            let value = self.pl_expr(names)?;
            out.push(ProcStmt::Set { name: format!("{which}.{col}"), value });
            return self.end_stmt();
        }
        // an assignment: name := expr
        if matches!(self.peek(), Tok::Ident(_) | Tok::Quoted(_)) && (self.peek_sym_at(1, ":=") || self.peek_sym_at(1, "=")) {
            let name = self.ident()?;
            if !names.has(&name) {
                return Err(Error::syntax(format!("'{name}' is not a declared variable or parameter")));
            }
            self.advance();
            let value = self.pl_expr(names)?;
            out.push(ProcStmt::Set { name, value });
            return self.end_stmt();
        }
        // SELECT ... INTO v
        if self.peek_kw("SELECT") {
            if let Some((into_at, targets, after)) = self.find_into() {
                self.toks.drain(into_at..after);
                let mut sel = self.select()?;
                let mut e = Expr::Literal(Value::Null);
                for (k, target) in targets.iter().enumerate() {
                    if !names.has(target) {
                        return Err(Error::syntax(format!("'{target}' is not a declared variable or parameter")));
                    }
                    let mut one = sel.clone();
                    let item = sel.items.get(k).cloned().ok_or_else(|| Error::syntax("SELECT INTO has more variables than columns"))?;
                    one.items = vec![item];
                    let mut sub = Expr::Subquery(Box::new(one));
                    names.expr(&mut sub)?;
                    e = sub;
                    out.push(ProcStmt::Set { name: target.clone(), value: e.clone() });
                }
                let _ = e;
                sel.items.clear();
                return self.end_stmt();
            }
        }
        let mut st = self.statement()?;
        names.statement(&mut st)?;
        out.push(ProcStmt::Sql(st));
        self.end_stmt()
    }

    /// For a `SELECT` starting at the cursor: the position of `INTO`, the variables after it, and the position after
    /// them, if the statement has an `INTO` before its end.
    fn find_into(&self) -> Option<(usize, Vec<String>, usize)> {
        let mut depth = 0i32;
        let mut i = self.pos;
        while let Some(t) = self.toks.get(i) {
            match &t.tok {
                Tok::Sym("(") => depth += 1,
                Tok::Sym(")") => depth -= 1,
                Tok::Sym(";") | Tok::Eof => return None,
                Tok::Ident(s) if depth == 0 && s.eq_ignore_ascii_case("INTO") => {
                    let mut targets = Vec::new();
                    let mut j = i + 1;
                    if matches!(&self.toks.get(j)?.tok, Tok::Ident(s) if s.eq_ignore_ascii_case("STRICT")) {
                        j += 1;
                    }
                    loop {
                        match &self.toks.get(j)?.tok {
                            Tok::Ident(n) | Tok::Quoted(n) => targets.push(n.clone()),
                            _ => return None,
                        }
                        j += 1;
                        if matches!(self.toks.get(j)?.tok, Tok::Sym(",")) {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    return Some((i, targets, j));
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    fn pl_raise(&mut self, names: &mut Names, out: &mut Vec<ProcStmt>) -> Result<()> {
        let mut exception = true;
        for level in ["DEBUG", "LOG", "INFO", "NOTICE", "WARNING"] {
            if self.eat_kw(level) {
                exception = false;
                break;
            }
        }
        if !exception || self.peek_kw("EXCEPTION") {
            self.eat_kw("EXCEPTION");
        }
        let mut message: Option<Expr> = None;
        if let Tok::Str(fmt) = self.peek().clone() {
            self.advance();
            let mut converted = String::new();
            let mut chars = fmt.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '%' {
                    if chars.peek() == Some(&'%') {
                        chars.next();
                        converted.push_str("%%");
                    } else {
                        converted.push_str("%s");
                    }
                } else {
                    converted.push(c);
                }
            }
            let mut args = vec![Expr::Literal(Value::text(&converted))];
            while self.eat_sym(",") {
                args.push(self.pl_expr(names)?);
            }
            message = Some(Expr::Function { name: "format".into(), args, distinct: false, star: false });
        }
        if self.eat_kw("USING") {
            loop {
                let option = self.ident()?;
                if !self.eat_sym("=") {
                    self.expect_kw("TO")?;
                }
                let v = self.pl_expr(names)?;
                if option.eq_ignore_ascii_case("MESSAGE") && message.is_none() {
                    message = Some(v);
                }
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        if exception {
            let message = message.ok_or_else(|| Error::syntax("RAISE needs a message (re-raising an error is not supported)"))?;
            out.push(ProcStmt::Sql(Statement::Raise(message)));
        }
        self.end_stmt()
    }
}

/// The body of a `LANGUAGE sql` function.
fn sql_body(body: &str, names: &Names, value: bool) -> Result<RoutineBody> {
    let mut statements = crate::sql::parse_script(body)?;
    if statements.is_empty() {
        return Err(Error::syntax("the function body is empty"));
    }
    for st in &mut statements {
        names.statement(st)?;
    }
    if value {
        if let Some(Statement::Select(sel)) = statements.last().cloned() {
            let last = sel;
            let simple = last.from.is_none()
                && last.filter.is_none()
                && last.group_by.is_empty()
                && last.having.is_none()
                && last.order_by.is_empty()
                && last.unions.is_empty()
                && last.with.is_none()
                && last.limit.is_none()
                && last.items.len() == 1;
            let value_expr = match (&last.items[..], simple) {
                ([SelectItem::Expr { expr, .. }], true) => expr.clone(),
                _ => Expr::Subquery(Box::new(last)),
            };
            if statements.len() == 1 {
                return Ok(RoutineBody::Expr(value_expr));
            }
            let mut block: Vec<ProcStmt> = statements.drain(..statements.len() - 1).map(ProcStmt::Sql).collect();
            block.push(ProcStmt::Return(Some(value_expr)));
            return Ok(RoutineBody::Block(block));
        }
        return Err(Error::syntax("a function that returns a value must end with a SELECT"));
    }
    Ok(RoutineBody::Block(statements.into_iter().map(ProcStmt::Sql).collect()))
}

/// The body of a `LANGUAGE plpgsql` function.
fn plpgsql_body(body: &str, mut names: Names, value: bool) -> Result<RoutineBody> {
    let mut p = Parser { src: body, toks: lex(body)?, pos: 0, into_vars: Vec::new(), depth: 0, selects: 0 };
    let block = p.pl_block(&mut names)?;
    let value = value && !names.trigger;
    while p.eat_sym(";") {}
    if !p.at_eof() {
        return Err(p.unexpected("the end of the function body"));
    }
    // `BEGIN RETURN expr; END` is an expression, and is used as one
    if value {
        if let [ProcStmt::Return(Some(e))] = block.as_slice() {
            return Ok(RoutineBody::Expr(e.clone()));
        }
    }
    Ok(RoutineBody::Block(block))
}

// ---- MySQL stored routines and triggers ----

const BODY_START: [&str; 24] = [
    "BEGIN", "RETURN", "INSERT", "UPDATE", "DELETE", "SELECT", "SET", "CALL", "IF", "WHILE", "REPEAT", "LOOP", "CASE", "DECLARE", "REPLACE", "WITH", "TRUNCATE",
    "DROP", "CREATE", "ALTER", "SIGNAL", "EXPLAIN", "LOCK", "UNLOCK",
];

impl<'a> Parser<'a> {
    /// At the name after `CREATE FUNCTION | PROCEDURE`: MySQL's form has no `AS` between the parameters and the body.
    pub(super) fn looks_like_mysql_routine(&self) -> bool {
        let mut i = self.pos;
        while i < self.toks.len() && i < self.pos + 6 && !matches!(self.toks[i].tok, Tok::Sym("(")) {
            i += 1;
        }
        if matches!(self.toks.get(i + 1).map(|t| &t.tok), Some(Tok::Param(_))) || self.toks.get(i).is_none_or(|t| !matches!(t.tok, Tok::Sym("("))) {
            return false;
        }
        let mut depth = 0;
        let mut j = i;
        loop {
            match self.toks.get(j).map(|t| &t.tok) {
                Some(Tok::Sym("(")) => depth += 1,
                Some(Tok::Sym(")")) => {
                    depth -= 1;
                    if depth == 0 {
                        j += 1;
                        break;
                    }
                }
                Some(Tok::Eof) | None => return false,
                _ => {}
            }
            j += 1;
        }
        while let Some(t) = self.toks.get(j) {
            match &t.tok {
                Tok::Eof => return false,
                Tok::Ident(w) => {
                    let up = w.to_ascii_uppercase();
                    if up == "AS" || up == "LANGUAGE" && matches!(self.toks.get(j + 1).map(|t| &t.tok), Some(Tok::Ident(l)) if !l.eq_ignore_ascii_case("SQL")) {
                        return false;
                    }
                    if BODY_START.contains(&up.as_str()) {
                        return true;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        false
    }

    fn skip_charset(&mut self) {
        loop {
            if self.eat_kw("CHARSET") || self.eat_kw("COLLATE") {
                self.advance();
            } else if self.peek_kw("CHARACTER") && self.peek_kw_at(1, "SET") {
                self.advance();
                self.advance();
                self.advance();
            } else {
                break;
            }
        }
    }

    /// `CREATE PROCEDURE name ([IN] p type, ...) characteristics body` and `CREATE FUNCTION ... RETURNS type ... body`.
    pub(super) fn mysql_routine(&mut self, function: bool, start: usize) -> Result<Statement> {
        let name = self.table_ident()?;
        self.expect_sym("(")?;
        let mut params: Vec<RoutineParam> = Vec::new();
        if !self.peek_sym(")") {
            loop {
                self.eat_kw("IN");
                if self.peek_kw("OUT") || self.peek_kw("INOUT") {
                    return Err(Error::syntax("OUT and INOUT parameters are not supported"));
                }
                let pname = self.ident()?;
                let ty = self.pg_type()?;
                self.skip_charset();
                params.push(RoutineParam { name: pname, data_type: Some(ty), default: None });
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        self.expect_sym(")")?;
        let mut returns = None;
        if function {
            self.expect_kw("RETURNS")?;
            returns = Some(self.pg_type()?);
            self.skip_charset();
        }
        // characteristics
        loop {
            if self.eat_kw("DETERMINISTIC") || self.eat_kw("LANGUAGE") && self.eat_kw("SQL") {
            } else if self.peek_kw("NOT") && self.peek_kw_at(1, "DETERMINISTIC") {
                self.advance();
                self.advance();
            } else if (self.peek_kw("NO") || self.peek_kw("CONTAINS")) && self.peek_kw_at(1, "SQL") {
                self.advance();
                self.advance();
            } else if (self.peek_kw("READS") || self.peek_kw("MODIFIES")) && self.peek_kw_at(1, "SQL") {
                self.advance();
                self.advance();
                self.expect_kw("DATA")?;
            } else if self.peek_kw("SQL") && self.peek_kw_at(1, "SECURITY") {
                self.advance();
                self.advance();
                self.advance();
            } else if self.eat_kw("COMMENT") {
                self.advance();
            } else {
                break;
            }
        }
        let mut names = Names { all: params.iter().map(|p| p.name.clone()).collect(), params: params.iter().map(|p| p.name.clone()).collect(), trigger: false };
        let stmts = self.my_body(&mut names)?;
        let end = self.toks[self.pos].pos;
        let sql = self.src[start..end].trim().to_string();
        let body = match stmts.as_slice() {
            [ProcStmt::Return(Some(e))] if function => RoutineBody::Expr(e.clone()),
            _ => RoutineBody::Block(stmts),
        };
        Ok(Statement::CreateRoutine(Box::new(RoutineDef { name, function, params, returns, body, trigger: false, sql })))
    }

    /// The body of a MySQL trigger (a statement, or `BEGIN ... END` with `SET NEW.col = ...`): `NEW.col` and `OLD.col`
    /// are variables.
    pub(super) fn mysql_trigger_body(&mut self) -> Result<Vec<ProcStmt>> {
        let mut names = Names { all: Vec::new(), params: Vec::new(), trigger: true };
        let body = self.my_body(&mut names)?;
        fn check(list: &[ProcStmt]) -> Result<()> {
            for s in list {
                match s {
                    ProcStmt::Sql(Statement::Insert { .. } | Statement::Update { .. } | Statement::Delete { .. } | Statement::Select(_) | Statement::SelectInto { .. } | Statement::Raise(_) | Statement::SetVar { .. } | Statement::Upsert { .. } | Statement::Exec { .. } | Statement::Batch(_)) => {}
                    ProcStmt::Sql(_) => return Err(Error::syntax("a trigger body may contain only INSERT, UPDATE, DELETE, SELECT, SET and CALL statements")),
                    ProcStmt::If { branches, otherwise } => {
                        for (_, b) in branches {
                            check(b)?;
                        }
                        check(otherwise)?;
                    }
                    ProcStmt::While { body, .. } => check(body)?,
                    _ => {}
                }
            }
            Ok(())
        }
        check(&body)?;
        if body.is_empty() {
            return Err(Error::syntax("a trigger needs at least one statement"));
        }
        Ok(body)
    }

    fn my_body(&mut self, names: &mut Names) -> Result<Vec<ProcStmt>> {
        let mut out = Vec::new();
        if self.peek_kw("BEGIN") || (matches!(self.peek(), Tok::Ident(_)) && self.peek_sym_at(1, ":") && self.peek_kw_at(2, "BEGIN")) {
            self.my_block(names, &mut out)?;
        } else {
            self.my_stmt(names, &mut out)?;
        }
        Ok(out)
    }

    fn my_block(&mut self, names: &mut Names, out: &mut Vec<ProcStmt>) -> Result<()> {
        if matches!(self.peek(), Tok::Ident(_)) && self.peek_sym_at(1, ":") {
            self.advance();
            self.advance();
        }
        self.expect_kw("BEGIN")?;
        self.eat_kw("ATOMIC");
        let inner = self.my_stmts(names, &["END"])?;
        out.extend(inner);
        self.expect_kw("END")?;
        // an end label
        if matches!(self.peek(), Tok::Ident(w) if !matches!(w.to_ascii_uppercase().as_str(), "ELSE" | "ELSEIF" | "UNTIL" | "WHEN")) && !self.peek_sym(";") {
            self.advance();
        }
        Ok(())
    }

    fn my_stmts(&mut self, names: &mut Names, stops: &[&str]) -> Result<Vec<ProcStmt>> {
        let mut out = Vec::new();
        loop {
            while self.eat_sym(";") {}
            if self.at_eof() {
                return Err(self.unexpected(stops[0]));
            }
            if stops.iter().any(|s| self.peek_kw(s)) {
                return Ok(out);
            }
            self.my_stmt(names, &mut out)?;
        }
    }

    fn my_end(&mut self) -> Result<()> {
        if self.eat_sym(";") || self.at_eof() || ["END", "ELSE", "ELSEIF", "UNTIL", "WHEN"].iter().any(|w| self.peek_kw(w)) {
            Ok(())
        } else {
            Err(self.unexpected("';'"))
        }
    }

    fn my_stmt(&mut self, names: &mut Names, out: &mut Vec<ProcStmt>) -> Result<()> {
        // a label: `lbl: LOOP`
        if matches!(self.peek(), Tok::Ident(_)) && self.peek_sym_at(1, ":") && !self.peek_kw("BEGIN") {
            self.advance();
            self.advance();
        }
        if self.peek_kw("BEGIN") {
            return self.my_block(names, out).and_then(|()| self.my_end());
        }
        if self.eat_kw("DECLARE") {
            if self.peek_kw("CONTINUE") || self.peek_kw("EXIT") || self.peek_kw("UNDO") {
                return Err(Error::syntax("condition handlers (DECLARE ... HANDLER) are not supported"));
            }
            let mut vars = vec![self.ident()?];
            while self.eat_sym(",") {
                vars.push(self.ident()?);
            }
            if self.peek_kw("CURSOR") || self.peek_kw("CONDITION") {
                return Err(Error::syntax("cursors and named conditions are not supported"));
            }
            let ty = self.pg_type()?;
            self.skip_charset();
            let init = if self.eat_kw("DEFAULT") { Some(self.pl_expr(names)?) } else { None };
            for v in vars {
                names.all.push(v.clone());
                out.push(ProcStmt::Declare { name: v, data_type: Some(ty), init: init.clone() });
            }
            return self.my_end();
        }
        if self.peek_kw("SET") {
            // SET var = expr, NEW.col = expr; anything else (`SET @x`, `SET sql_mode`) is a statement
            let declared = |s: &Self, k: usize| -> bool {
                match s.peek_at(k) {
                    Tok::Ident(n) | Tok::Quoted(n) => {
                        let assigned = s.peek_sym_at(k + 1, "=") || s.peek_sym_at(k + 1, ":=");
                        let member = s.peek_sym_at(k + 1, ".") && matches!(s.peek_at(k + 2), Tok::Ident(_)) && (s.peek_sym_at(k + 3, "=") || s.peek_sym_at(k + 3, ":="));
                        (assigned && names.has(n)) || (member && names.trigger && (n.eq_ignore_ascii_case("new") || n.eq_ignore_ascii_case("old")))
                    }
                    _ => false,
                }
            };
            if declared(self, 1) {
                self.advance();
                loop {
                    let first = self.ident()?;
                    let target = if self.eat_sym(".") { format!("{}.{}", first.to_ascii_lowercase(), self.ident()?.to_ascii_lowercase()) } else { first };
                    if !self.eat_sym(":=") {
                        self.expect_sym("=")?;
                    }
                    let value = self.pl_expr(names)?;
                    out.push(ProcStmt::Set { name: target, value });
                    if self.peek_sym(",") && declared(self, 1) {
                        self.advance();
                        continue;
                    }
                    break;
                }
                return self.my_end();
            }
        }
        if self.eat_kw("IF") {
            let mut branches = Vec::new();
            let cond = self.pl_expr(names)?;
            self.expect_kw("THEN")?;
            branches.push((cond, self.my_stmts(names, &["ELSEIF", "ELSE", "END"])?));
            let mut otherwise = Vec::new();
            loop {
                if self.eat_kw("ELSEIF") {
                    let cond = self.pl_expr(names)?;
                    self.expect_kw("THEN")?;
                    branches.push((cond, self.my_stmts(names, &["ELSEIF", "ELSE", "END"])?));
                } else if self.eat_kw("ELSE") {
                    otherwise = self.my_stmts(names, &["END"])?;
                } else {
                    break;
                }
            }
            self.expect_kw("END")?;
            self.expect_kw("IF")?;
            out.push(ProcStmt::If { branches, otherwise });
            return self.my_end();
        }
        if self.eat_kw("WHILE") {
            let cond = self.pl_expr(names)?;
            self.expect_kw("DO")?;
            let body = self.my_stmts(names, &["END"])?;
            self.expect_kw("END")?;
            self.expect_kw("WHILE")?;
            if matches!(self.peek(), Tok::Ident(_)) && !self.peek_sym(";") {
                self.advance();
            }
            out.push(ProcStmt::While { cond, body });
            return self.my_end();
        }
        if self.eat_kw("REPEAT") {
            let mut body = self.my_stmts(names, &["UNTIL"])?;
            self.expect_kw("UNTIL")?;
            let cond = self.pl_expr(names)?;
            self.expect_kw("END")?;
            self.expect_kw("REPEAT")?;
            body.push(ProcStmt::If { branches: vec![(cond, vec![ProcStmt::Leave])], otherwise: Vec::new() });
            out.push(ProcStmt::While { cond: lit_true(), body });
            return self.my_end();
        }
        if self.eat_kw("LOOP") {
            let body = self.my_stmts(names, &["END"])?;
            self.expect_kw("END")?;
            self.expect_kw("LOOP")?;
            out.push(ProcStmt::While { cond: lit_true(), body });
            return self.my_end();
        }
        if self.eat_kw("LEAVE") {
            if matches!(self.peek(), Tok::Ident(_)) {
                self.advance();
            }
            out.push(ProcStmt::Leave);
            return self.my_end();
        }
        if self.peek_kw("ITERATE") || self.peek_kw("OPEN") || self.peek_kw("FETCH") || self.peek_kw("CLOSE") {
            return Err(Error::syntax("ITERATE and cursors are not supported"));
        }
        if self.eat_kw("RETURN") {
            let value = if self.peek_sym(";") || self.at_eof() || self.peek_kw("END") { None } else { Some(self.pl_expr(names)?) };
            out.push(ProcStmt::Return(value));
            return self.my_end();
        }
        if self.eat_kw("CASE") {
            let subject = if self.peek_kw("WHEN") { None } else { Some(self.pl_expr(names)?) };
            let mut branches = Vec::new();
            while self.eat_kw("WHEN") {
                let v = self.pl_expr(names)?;
                let cond = match &subject {
                    Some(s) => Expr::Binary { op: BinOp::Eq, left: Box::new(s.clone()), right: Box::new(v) },
                    None => v,
                };
                self.expect_kw("THEN")?;
                branches.push((cond, self.my_stmts(names, &["WHEN", "ELSE", "END"])?));
            }
            let otherwise = if self.eat_kw("ELSE") { self.my_stmts(names, &["END"])? } else { Vec::new() };
            self.expect_kw("END")?;
            self.expect_kw("CASE")?;
            out.push(ProcStmt::If { branches, otherwise });
            return self.my_end();
        }
        if self.peek_kw("SELECT") {
            if let Some((into_at, targets, after)) = self.find_into() {
                self.toks.drain(into_at..after);
                let sel = self.select()?;
                for (k, target) in targets.iter().enumerate() {
                    if !names.has(target) {
                        return Err(Error::syntax(format!("'{target}' is not a declared variable or parameter")));
                    }
                    let mut one = sel.clone();
                    let item = sel.items.get(k).cloned().ok_or_else(|| Error::syntax("SELECT INTO has more variables than columns"))?;
                    one.items = vec![item];
                    let mut sub = Expr::Subquery(Box::new(one));
                    names.expr(&mut sub)?;
                    out.push(ProcStmt::Set { name: target.clone(), value: sub });
                }
                return self.my_end();
            }
        }
        let mut st = self.statement()?;
        names.statement(&mut st)?;
        out.push(ProcStmt::Sql(st));
        self.my_end()
    }
}
