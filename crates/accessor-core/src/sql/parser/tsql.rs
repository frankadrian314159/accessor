//! Transact-SQL (Microsoft SQL Server) batches.
//!
//! A T-SQL script is a sequence of *batches* separated by `GO`. Statements need no terminator, variables (`DECLARE @n INT`)
//! live for the batch, and control flow (`IF`/`ELSE`, `WHILE`, `BEGIN ... END`, `BEGIN TRY ... END CATCH`) is written between
//! ordinary statements. A batch of plain SQL statements is returned as those statements; a batch that uses variables or
//! control flow becomes one [`Statement::TsqlBatch`], which the engine runs with the interpreter of stored procedures
//! (`ProcStmt`). The SQL inside is read by the ordinary parser, which knows the T-SQL spellings when the dialect is `tsql`.
//!
//! Covered: `DECLARE`, `SET @v = expr` (and `+=`, `-=`, `*=`, `/=`), `SET NOCOUNT ON`-style options (read and ignored),
//! `SELECT @v = expr ...`, `IF`, `WHILE`, `BREAK`, `CONTINUE`, `RETURN`, `BEGIN ... END`, `BEGIN TRY/CATCH`, `PRINT`, `THROW`,
//! `RAISERROR`, `EXEC`, `BEGIN/COMMIT/ROLLBACK/SAVE TRAN`, `USE`. Left out, with an error that says so: table variables
//! (`DECLARE @t TABLE`), cursors, `GOTO` and labels.

use std::cell::{Cell, RefCell};

use super::{Parser, Tok};
use crate::error::{Error, Result};
use crate::sql::ast::*;
use crate::value::Value;

thread_local! {
    /// The labels of the `WHILE` loops being read, innermost last (`CONTINUE` goes to the innermost).
    static LOOPS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static NEXT_LOOP: Cell<u32> = const { Cell::new(0) };
}

/// The words that begin a statement: the end of one without a terminator.
pub(super) const STARTERS: &[&str] = &[
    "DECLARE", "IF", "WHILE", "PRINT", "EXEC", "EXECUTE", "RETURN", "BREAK", "CONTINUE", "GO", "USE", "BEGIN", "THROW", "RAISERROR", "COMMIT",
    "ROLLBACK", "SAVE", "TRUNCATE", "ALTER", "MERGE", "GRANT", "REVOKE", "DENY", "WAITFOR",
];

/// `SET` options that SQL Server has and this engine has no use for: they are read and ignored.
const SET_OPTIONS: &[&str] = &[
    "NOCOUNT", "XACT_ABORT", "ANSI_NULLS", "QUOTED_IDENTIFIER", "ANSI_WARNINGS", "ARITHABORT", "ANSI_PADDING", "ANSI_NULL_DFLT_ON",
    "ANSI_NULL_DFLT_OFF", "NUMERIC_ROUNDABORT", "CONCAT_NULL_YIELDS_NULL", "IMPLICIT_TRANSACTIONS", "LOCK_TIMEOUT", "TRANSACTION", "IDENTITY_INSERT",
    "LANGUAGE", "DATEFORMAT", "DATEFIRST", "TEXTSIZE", "ROWCOUNT", "FMTONLY", "NOEXEC", "PARSEONLY", "STATISTICS", "SHOWPLAN_ALL", "SHOWPLAN_TEXT",
    "SHOWPLAN_XML", "DEADLOCK_PRIORITY", "QUERY_GOVERNOR_COST_LIMIT", "CONTEXT_INFO", "REMOTE_PROC_TRANSACTIONS", "NOCOUNT_",
];

fn empty() -> ProcStmt {
    ProcStmt::Block { label: None, body: Vec::new(), handlers: Vec::new() }
}

/// A batch becomes its statements when they are all plain SQL, and one block when it has variables or control flow.
fn flush(out: &mut Vec<Statement>, batch: Vec<ProcStmt>) {
    let plain = |s: &ProcStmt| matches!(s, ProcStmt::Sql(st) if !matches!(st, Statement::Raise(_) | Statement::Print(_)));
    if batch.is_empty() {
        return;
    }
    if batch.iter().all(plain) {
        out.extend(batch.into_iter().filter_map(|s| if let ProcStmt::Sql(st) = s { Some(st) } else { None }));
    } else if !batch.iter().all(|s| matches!(s, ProcStmt::Block { body, handlers, .. } if body.is_empty() && handlers.is_empty())) {
        out.push(Statement::TsqlBatch(batch));
    }
}

impl Parser<'_> {
    /// Does a new statement begin here (or is this the end of the text)?
    fn tsql_statement_start(&self) -> bool {
        match self.peek() {
            Tok::Eof | Tok::Sym(";") => true,
            Tok::Ident(w) => {
                let up = w.to_ascii_uppercase();
                STARTERS.contains(&up.as_str()) || matches!(up.as_str(), "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "CREATE" | "DROP" | "WITH" | "SET" | "ELSE")
            }
            _ => false,
        }
    }

    /// The whole text: batches split at `GO`.
    pub(super) fn tsql_script(&mut self) -> Result<Vec<Statement>> {
        let mut out = Vec::new();
        let mut batch: Vec<ProcStmt> = Vec::new();
        loop {
            while self.eat_sym(";") {}
            if self.at_eof() {
                break;
            }
            if self.peek_kw("GO") && !matches!(self.peek_at(1), Tok::Sym("(") | Tok::Sym(".")) {
                self.advance();
                if matches!(self.peek(), Tok::Int(_)) {
                    self.advance();
                }
                flush(&mut out, std::mem::take(&mut batch));
                continue;
            }
            batch.push(self.tsql_stmt()?);
        }
        flush(&mut out, batch);
        Ok(out)
    }

    /// Statements up to (not including) the keyword `stop`.
    fn tsql_until(&mut self, stop: &str) -> Result<Vec<ProcStmt>> {
        let mut body = Vec::new();
        loop {
            while self.eat_sym(";") {}
            if self.at_eof() {
                return Err(self.unexpected(stop));
            }
            if self.peek_kw(stop) {
                return Ok(body);
            }
            body.push(self.tsql_stmt()?);
        }
    }

    fn tsql_variable(&mut self) -> Result<String> {
        match self.advance() {
            Tok::Param(n) => Ok(n),
            _ => {
                self.pos -= 1;
                Err(self.unexpected("a variable such as @name"))
            }
        }
    }

    fn tsql_stmt(&mut self) -> Result<ProcStmt> {
        // DECLARE @a INT = 1, @b NVARCHAR(20)
        if self.eat_kw("DECLARE") {
            let mut body = Vec::new();
            loop {
                let name = self.tsql_variable()?;
                self.eat_kw("AS");
                if self.peek_kw("TABLE") || self.peek_kw("CURSOR") {
                    return Err(Error::syntax("table variables and cursors are not supported in a T-SQL batch"));
                }
                let data_type = Some(self.data_type_name()?);
                let init = if self.eat_sym("=") { Some(self.expr()?) } else { None };
                body.push(ProcStmt::Declare { name, data_type, init });
                if !self.eat_sym(",") {
                    break;
                }
            }
            return Ok(ProcStmt::Block { label: None, body, handlers: Vec::new() });
        }
        if self.peek_kw("SET") {
            if matches!(self.peek_at(1), Tok::Param(_)) {
                self.advance();
                let name = self.tsql_variable()?;
                // `SET @n += 1` lexes as `+` then `=`
                let op = match self.peek() {
                    Tok::Sym(s @ ("+" | "-" | "*" | "/" | "%")) => {
                        let s = *s;
                        self.advance();
                        Some(s)
                    }
                    _ => None,
                };
                self.expect_sym("=")?;
                let mut value = self.expr()?;
                if let Some(op) = op {
                    use crate::sql::ast::BinOp::*;
                    let op = match op {
                        "+" => Add,
                        "-" => Sub,
                        "*" => Mul,
                        "/" => Div,
                        _ => Mod,
                    };
                    value = Expr::Binary { op, left: Box::new(Expr::Param(name.clone())), right: Box::new(value) };
                }
                return Ok(ProcStmt::Set { name, value });
            }
            if matches!(self.peek_at(1), Tok::Ident(w) if SET_OPTIONS.contains(&w.to_ascii_uppercase().as_str())) {
                self.advance();
                self.advance();
                // ON | OFF, a value, or `ISOLATION LEVEL READ COMMITTED`: up to the next statement
                while !self.tsql_statement_start() {
                    self.advance();
                }
                return Ok(empty());
            }
        }
        if self.eat_kw("IF") {
            let cond = self.expr()?;
            let then = self.tsql_stmt()?;
            let otherwise = if self.eat_kw("ELSE") { vec![self.tsql_stmt()?] } else { Vec::new() };
            return Ok(ProcStmt::If { branches: vec![(cond, vec![then])], otherwise });
        }
        if self.eat_kw("WHILE") {
            let cond = self.expr()?;
            let label = format!("__while{}", NEXT_LOOP.with(|n| n.replace(n.get() + 1)));
            LOOPS.with(|l| l.borrow_mut().push(label.clone()));
            let body = self.tsql_stmt();
            LOOPS.with(|l| l.borrow_mut().pop());
            return Ok(ProcStmt::Labeled { label, stmt: Box::new(ProcStmt::While { cond, body: vec![body?] }) });
        }
        if self.eat_kw("BREAK") {
            return Ok(ProcStmt::Leave);
        }
        if self.eat_kw("CONTINUE") {
            let label = LOOPS.with(|l| l.borrow().last().cloned()).ok_or_else(|| Error::syntax("CONTINUE is used inside a WHILE loop"))?;
            return Ok(ProcStmt::Iterate(label));
        }
        if self.eat_kw("RETURN") {
            if self.tsql_statement_start() || self.peek_kw("END") {
                return Ok(ProcStmt::Return(None));
            }
            return Ok(ProcStmt::Return(Some(self.expr()?)));
        }
        if self.peek_kw("BEGIN") {
            if self.peek_kw_at(1, "TRY") {
                self.advance();
                self.advance();
                let body = self.tsql_until("END")?;
                self.expect_kw("END")?;
                self.expect_kw("TRY")?;
                self.expect_kw("BEGIN")?;
                self.expect_kw("CATCH")?;
                let catch = self.tsql_until("END")?;
                self.expect_kw("END")?;
                self.expect_kw("CATCH")?;
                let handler = Handler { exit: true, conditions: vec![HandlerCond::SqlException], body: catch };
                return Ok(ProcStmt::Block { label: None, body, handlers: vec![handler] });
            }
            if self.peek_kw_at(1, "TRAN") || self.peek_kw_at(1, "TRANSACTION") || self.peek_kw_at(1, "DISTRIBUTED") {
                self.advance();
                self.advance();
                self.eat_kw("TRANSACTION");
                if matches!(self.peek(), Tok::Ident(_) | Tok::Param(_)) && !self.tsql_statement_start() {
                    self.advance();
                }
                if self.eat_kw("WITH") {
                    self.eat_kw("MARK");
                    if matches!(self.peek(), Tok::Str(_)) {
                        self.advance();
                    }
                }
                return Ok(ProcStmt::Sql(Statement::Begin));
            }
            self.advance();
            let body = self.tsql_until("END")?;
            self.expect_kw("END")?;
            return Ok(ProcStmt::Block { label: None, body, handlers: Vec::new() });
        }
        if self.peek_kw("COMMIT") || self.peek_kw("ROLLBACK") || self.peek_kw("SAVE") {
            let word = self.advance();
            let commit = matches!(&word, Tok::Ident(w) if w.eq_ignore_ascii_case("COMMIT"));
            let save = matches!(&word, Tok::Ident(w) if w.eq_ignore_ascii_case("SAVE"));
            if !self.eat_kw("TRAN") && !self.eat_kw("TRANSACTION") {
                self.eat_kw("WORK");
            }
            let name = if matches!(self.peek(), Tok::Ident(_)) && !self.tsql_statement_start() { Some(self.ident()?) } else { None };
            return Ok(ProcStmt::Sql(match (commit, save, name) {
                (true, ..) => Statement::Commit,
                (_, true, Some(n)) => Statement::Savepoint(n),
                (_, _, Some(n)) => Statement::RollbackTo(n),
                _ => Statement::Rollback,
            }));
        }
        if self.eat_kw("PRINT") {
            return Ok(ProcStmt::Sql(Statement::Print(self.expr()?)));
        }
        if self.eat_kw("USE") {
            self.ident()?;
            return Ok(empty());
        }
        if self.eat_kw("THROW") {
            if self.tsql_statement_start() || self.peek_kw("END") {
                return Ok(ProcStmt::Sql(Statement::Raise(Expr::Function { name: "ERROR_MESSAGE".into(), args: Vec::new(), distinct: false, star: false })));
            }
            let _number = self.expr()?;
            self.expect_sym(",")?;
            let message = self.expr()?;
            self.expect_sym(",")?;
            self.expr()?;
            return Ok(ProcStmt::Sql(Statement::Raise(message)));
        }
        if self.eat_kw("RAISERROR") {
            let paren = self.eat_sym("(");
            let message = self.expr()?;
            self.expect_sym(",")?;
            let severity = self.expr()?;
            self.expect_sym(",")?;
            self.expr()?;
            let mut args = Vec::new();
            while self.eat_sym(",") {
                args.push(self.expr()?);
            }
            if paren {
                self.expect_sym(")")?;
            }
            if self.eat_kw("WITH") {
                while matches!(self.peek(), Tok::Ident(_)) && !self.tsql_statement_start() {
                    self.advance();
                    self.eat_sym(",");
                }
            }
            let message = if args.is_empty() {
                message
            } else {
                let mut all = vec![message];
                all.extend(args);
                Expr::Function { name: "__FORMATMESSAGE".into(), args: all, distinct: false, star: false }
            };
            // severity 10 and below is an informational message; above, an error
            let informational = matches!(severity, Expr::Literal(Value::Int(s)) if s <= 10);
            return Ok(ProcStmt::Sql(if informational { Statement::Print(message) } else { Statement::Raise(message) }));
        }
        if self.peek_kw("EXEC") || self.peek_kw("EXECUTE") {
            // EXEC('text'), EXEC (@sql), EXEC sp_executesql @sql, ...
            let dynamic = matches!(self.peek_at(1), Tok::Sym("(")) || self.peek_at_ident_is(1, "sp_executesql");
            if dynamic {
                self.advance();
                let sql = if self.eat_sym("(") {
                    let e = self.expr()?;
                    self.expect_sym(")")?;
                    e
                } else {
                    self.advance();
                    let e = self.expr()?;
                    // the parameter definitions and values are not used: the text runs with the variables of the batch
                    while self.eat_sym(",") {
                        self.expr()?;
                    }
                    e
                };
                return Ok(ProcStmt::Execute { sql, using: Vec::new(), targets: Vec::new(), record: false, strict: false });
            }
        }
        if self.peek_kw("SELECT") {
            let st = self.statement()?;
            return tsql_assignment(st);
        }
        Ok(ProcStmt::Sql(self.statement()?))
    }

    /// T-SQL's calls that are not written like the others: `DATEADD(day, 1, d)` (a bare part name first), and
    /// `CONVERT(type, x [, style])`, `TRY_CAST(x AS type)`, `TRY_CONVERT(type, x)`.
    pub(super) fn tsql_call(&mut self, name: &str) -> Result<Option<Expr>> {
        let up = name.to_ascii_uppercase();
        let call = |n: &str, args: Vec<Expr>| Expr::Function { name: n.into(), args, distinct: false, star: false };
        match up.as_str() {
            "DATEADD" | "DATEDIFF" | "DATEDIFF_BIG" | "DATEPART" | "DATENAME" => {
                let unit = match self.advance() {
                    Tok::Ident(w) | Tok::Quoted(w) => w,
                    Tok::Str(s) => s,
                    _ => {
                        self.pos -= 1;
                        return Err(self.unexpected("a date part such as day"));
                    }
                };
                self.expect_sym(",")?;
                let mut args = vec![Expr::Literal(Value::text(&unit))];
                args.push(self.expr()?);
                while self.eat_sym(",") {
                    args.push(self.expr()?);
                }
                self.expect_sym(")")?;
                Ok(Some(call(&up, args)))
            }
            "CONVERT" | "TRY_CONVERT" => {
                let (tn, hints) = self.tsql_type_with_size()?;
                self.expect_sym(",")?;
                let e = self.expr()?;
                let style = if self.eat_sym(",") { Some(self.expr()?) } else { None };
                self.expect_sym(")")?;
                Ok(Some(self.tsql_convert(&tn, hints, e, style, up == "TRY_CONVERT")?))
            }
            "CAST" | "TRY_CAST" => {
                let e = self.expr()?;
                self.expect_kw("AS")?;
                let (tn, hints) = self.tsql_type_with_size()?;
                self.expect_sym(")")?;
                Ok(Some(self.tsql_convert(&tn, hints, e, None, up == "TRY_CAST")?))
            }
            _ => Ok(None),
        }
    }

    /// A type name and its size: `VARCHAR(20)`, `DECIMAL(10, 2)`, `NVARCHAR(MAX)`.
    fn tsql_type_with_size(&mut self) -> Result<(String, Vec<i64>)> {
        let tn = self.type_name()?;
        let mut hints = Vec::new();
        if self.eat_sym("(") {
            while !self.peek_sym(")") && !self.at_eof() {
                if let Tok::Int(i) = self.advance() {
                    hints.push(i);
                }
            }
            self.expect_sym(")")?;
        }
        Ok((tn, hints))
    }

    fn tsql_convert(&mut self, tn: &str, hints: Vec<i64>, e: Expr, style: Option<Expr>, try_: bool) -> Result<Expr> {
        let call = |n: &str, args: Vec<Expr>| Expr::Function { name: n.into(), args, distinct: false, star: false };
        let (ty, _) = crate::sql::usertypes::resolve(tn).ok_or_else(|| Error::syntax(format!("unknown data type '{tn}'")))?;
        let value = match (&style, ty) {
            (Some(s), crate::value::DataType::Text) => call("__TSQL_STYLE", vec![e.clone(), s.clone()]),
            _ => e.clone(),
        };
        let cast = super::make_cast(value, ty, &hints);
        if !try_ {
            return Ok(cast);
        }
        // TRY_CAST: NULL when the value does not convert
        Ok(Expr::Case { whens: vec![(call("__CAN_CAST", vec![e, Expr::Literal(Value::text(tn))]), cast)], else_expr: None })
    }

    fn peek_at_ident_is(&self, n: usize, word: &str) -> bool {
        matches!(self.peek_at(n), Tok::Ident(w) if w.eq_ignore_ascii_case(word))
    }
}

/// `SELECT @a = x, @b = y FROM ...` assigns variables, and returns no rows.
fn tsql_assignment(st: Statement) -> Result<ProcStmt> {
    let Statement::Select(mut sel) = st else { return Ok(ProcStmt::Sql(st)) };
    let assigns = |i: &SelectItem| matches!(i, SelectItem::Expr { expr: Expr::Binary { op: BinOp::Eq, left, .. }, alias: None } if matches!(**left, Expr::Param(_)));
    let count = sel.items.iter().filter(|i| assigns(i)).count();
    if count == 0 || !sel.unions.is_empty() {
        return Ok(ProcStmt::Sql(Statement::Select(sel)));
    }
    if count != sel.items.len() {
        return Err(Error::syntax("a SELECT that assigns a variable cannot also return rows"));
    }
    let mut targets = Vec::new();
    for item in sel.items.iter_mut() {
        if let SelectItem::Expr { expr: Expr::Binary { left, right, .. }, .. } = item {
            if let Expr::Param(name) = &**left {
                targets.push(name.clone());
            }
            let value = std::mem::replace(&mut **right, Expr::Literal(Value::Null));
            *item = SelectItem::Expr { expr: value, alias: None };
        }
    }
    Ok(ProcStmt::SelectRecord { name: "__tsql".into(), select: Box::new(sel), strict: false, targets })
}
