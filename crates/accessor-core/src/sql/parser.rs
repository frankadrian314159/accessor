//! Recursive-descent parser for the SQL dialect.

use super::ast::*;
use super::lexer::{lex, Tok, Token};
use crate::error::{Error, Result};
use crate::schema::RefAction;
use crate::value::{DataType, Value};

const RESERVED: &[&str] = &[
    "SELECT", "FROM", "WHERE", "GROUP", "BY", "HAVING", "ORDER", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "OUTER",
    "CROSS", "ON", "AS", "AND", "OR", "NOT", "IN", "IS", "NULL", "LIKE", "BETWEEN", "UNION", "LIMIT", "OFFSET", "SET",
    "VALUES", "INTO", "DISTINCT", "TOP", "ASC", "DESC", "CASE", "WHEN", "THEN", "ELSE", "END", "EXISTS", "INSERT",
    "UPDATE", "DELETE", "CREATE", "DROP", "TABLE", "INDEX", "VIEW", "MOD", "ALL",
];

/// Parses a script of one or more `;`-separated statements.
pub fn parse_script(sql: &str) -> Result<Vec<Statement>> {
    let mut p = Parser { src: sql, toks: lex(sql)?, pos: 0 };
    let mut out = Vec::new();
    loop {
        while p.eat_sym(";") {}
        if p.at_eof() {
            break;
        }
        out.push(p.statement()?);
        if !p.at_eof() && !p.peek_sym(";") {
            return Err(p.unexpected("end of statement"));
        }
    }
    Ok(out)
}

/// Parses a stand-alone expression (the stored form of a calculated field).
pub fn parse_expression(sql: &str) -> Result<Expr> {
    let mut p = Parser { src: sql, toks: lex(sql)?, pos: 0 };
    let e = p.expr()?;
    if !p.at_eof() {
        return Err(p.unexpected("the end of the expression"));
    }
    Ok(e)
}

/// Parses exactly one statement.
pub fn parse_statement(sql: &str) -> Result<Statement> {
    let mut v = parse_script(sql)?;
    match v.len() {
        1 => Ok(v.remove(0)),
        0 => Err(Error::syntax("empty statement")),
        _ => Err(Error::syntax("expected a single statement")),
    }
}

/// Parses a stand-alone SELECT (used for saved queries).
pub fn parse_select(sql: &str) -> Result<Select> {
    match parse_statement(sql)? {
        Statement::Select(s) => Ok(s),
        _ => Err(Error::syntax("expected a SELECT statement")),
    }
}

/// What a saved query holds: a SELECT (with optional parameters), or a call
/// of another saved query with fixed arguments.
pub enum QueryDef {
    Select { params: Vec<ParamDecl>, select: Select },
    Exec { name: String, args: Vec<(Option<String>, Expr)> },
}

/// Parses the text of a saved query.
pub fn parse_query_def(sql: &str) -> Result<QueryDef> {
    let mut stmts = parse_script(sql)?;
    if let [Statement::Exec { .. }] = stmts.as_slice() {
        if let Some(Statement::Exec { name, args }) = stmts.pop() {
            return Ok(QueryDef::Exec { name, args });
        }
    }
    let (params, select) = parse_query(sql)?;
    Ok(QueryDef::Select { params, select })
}

/// Parses a saved query: an optional `PARAMETERS` header, then one SELECT.
pub fn parse_query(sql: &str) -> Result<(Vec<ParamDecl>, Select)> {
    let mut stmts = parse_script(sql)?.into_iter();
    let first = stmts.next();
    let (params, select) = match first {
        Some(Statement::Parameters(p)) => (p, stmts.next()),
        other => (Vec::new(), other),
    };
    match (select, stmts.next()) {
        (Some(Statement::Select(s)), None) => Ok((params, s)),
        _ => Err(Error::syntax("expected a SELECT statement")),
    }
}

/// A negative number literal: the same type as the literal, and an `INTEGER` for
/// the one value (-2^63) whose positive form needs a `BIGNUM`.
fn negate_literal(v: &Value) -> Option<Value> {
    if !v.is_numeric() {
        return None;
    }
    Some(match crate::numeric::negate(v).ok()? {
        Value::Big(b) => match num_traits::ToPrimitive::to_i64(&b) {
            Some(i) => Value::Int(i),
            None => Value::Big(b),
        },
        other => other,
    })
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    pos: usize,
}

impl<'a> Parser<'a> {
    // ----- token helpers -------------------------------------------------

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].tok
    }
    fn advance(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn at_eof(&self) -> bool {
        matches!(self.peek(), Tok::Eof)
    }
    fn peek_sym(&self, s: &str) -> bool {
        matches!(self.peek(), Tok::Sym(x) if *x == s)
    }
    fn eat_sym(&mut self, s: &str) -> bool {
        let hit = self.peek_sym(s);
        if hit {
            self.advance();
        }
        hit
    }
    fn expect_sym(&mut self, s: &str) -> Result<()> {
        if self.eat_sym(s) {
            Ok(())
        } else {
            Err(self.unexpected(&format!("'{s}'")))
        }
    }
    fn is_kw(tok: &Tok, kw: &str) -> bool {
        matches!(tok, Tok::Ident(s) if s.eq_ignore_ascii_case(kw))
    }
    fn peek_kw(&self, kw: &str) -> bool {
        Self::is_kw(self.peek(), kw)
    }
    fn peek_kw_at(&self, n: usize, kw: &str) -> bool {
        Self::is_kw(self.peek_at(n), kw)
    }
    fn eat_kw(&mut self, kw: &str) -> bool {
        let hit = self.peek_kw(kw);
        if hit {
            self.advance();
        }
        hit
    }
    fn expect_kw(&mut self, kw: &str) -> Result<()> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(self.unexpected(kw))
        }
    }
    fn unexpected(&self, wanted: &str) -> Error {
        let found = match self.peek() {
            Tok::Eof => "end of input".to_string(),
            Tok::Ident(s) | Tok::Quoted(s) => format!("'{s}'"),
            Tok::Str(s) => format!("'{s}'"),
            Tok::Int(i) => i.to_string(),
            Tok::Dec(d) => d.to_string(),
            Tok::Big(b) => b.to_string(),
            Tok::Float(f) => f.to_string(),
            Tok::Date(_) => "date literal".into(),
            Tok::Sym(s) => format!("'{s}'"),
            Tok::Param(p) => format!("'@{p}'"),
        };
        Error::syntax(format!("expected {wanted} but found {found} (at offset {})", self.toks[self.pos].pos))
    }

    /// Any identifier (reserved words allowed only when bracketed).
    fn ident(&mut self) -> Result<String> {
        match self.peek().clone() {
            Tok::Quoted(s) => {
                self.advance();
                Ok(s)
            }
            Tok::Ident(s) if !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&s)) => {
                self.advance();
                Ok(s)
            }
            _ => Err(self.unexpected("an identifier")),
        }
    }

    fn peek_is_alias(&self) -> bool {
        match self.peek() {
            Tok::Quoted(_) => true,
            Tok::Ident(s) => !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(s)),
            _ => false,
        }
    }

    fn usize_literal(&mut self) -> Result<usize> {
        match self.advance() {
            Tok::Int(i) if i >= 0 => Ok(i as usize),
            _ => {
                self.pos = self.pos.saturating_sub(1);
                Err(self.unexpected("a non-negative integer"))
            }
        }
    }

    // ----- statements ------------------------------------------------------

    fn statement(&mut self) -> Result<Statement> {
        if self.peek_kw("SELECT") {
            return Ok(Statement::Select(self.select()?));
        }
        if self.eat_kw("INSERT") {
            return self.insert();
        }
        if self.eat_kw("UPDATE") {
            return self.update();
        }
        if self.eat_kw("DELETE") {
            return self.delete();
        }
        if self.eat_kw("CREATE") {
            return self.create();
        }
        if self.eat_kw("DROP") {
            return self.drop_stmt();
        }
        if self.eat_kw("ALTER") {
            return self.alter();
        }
        if self.eat_kw("PARAMETERS") {
            return Ok(Statement::Parameters(self.param_list()?));
        }
        if self.eat_kw("EXEC") || self.eat_kw("EXECUTE") || self.eat_kw("CALL") {
            return self.exec();
        }
        if self.eat_kw("RAISE") {
            return Ok(Statement::Raise(self.expr()?));
        }
        if self.eat_kw("BEGIN") {
            if !self.eat_kw("TRANSACTION") {
                self.eat_kw("WORK");
            }
            return Ok(Statement::Begin);
        }
        if self.eat_kw("COMMIT") {
            if !self.eat_kw("TRANSACTION") {
                self.eat_kw("WORK");
            }
            return Ok(Statement::Commit);
        }
        if self.eat_kw("ROLLBACK") {
            if !self.eat_kw("TRANSACTION") {
                self.eat_kw("WORK");
            }
            if self.eat_kw("TO") {
                self.eat_kw("SAVEPOINT");
                return Ok(Statement::RollbackTo(self.ident()?));
            }
            return Ok(Statement::Rollback);
        }
        if self.eat_kw("SAVEPOINT") {
            return Ok(Statement::Savepoint(self.ident()?));
        }
        if self.eat_kw("RELEASE") {
            self.eat_kw("SAVEPOINT");
            return Ok(Statement::ReleaseSavepoint(self.ident()?));
        }
        Err(self.unexpected("a statement (SELECT, INSERT, UPDATE, DELETE, CREATE, DROP, ALTER, EXEC, BEGIN, COMMIT, ROLLBACK, SAVEPOINT, RELEASE)"))
    }

    fn insert(&mut self) -> Result<Statement> {
        self.expect_kw("INTO")?;
        let table = self.ident()?;
        let mut columns = None;
        if self.peek_sym("(") && !self.peek_kw_at(1, "SELECT") {
            self.advance();
            columns = Some(self.ident_list()?);
            self.expect_sym(")")?;
        }
        let source = if self.eat_kw("VALUES") {
            let mut rows = Vec::new();
            loop {
                self.expect_sym("(")?;
                let mut row = vec![self.expr()?];
                while self.eat_sym(",") {
                    row.push(self.expr()?);
                }
                self.expect_sym(")")?;
                rows.push(row);
                if !self.eat_sym(",") {
                    break;
                }
            }
            InsertSource::Values(rows)
        } else if self.peek_kw("SELECT") {
            InsertSource::Select(Box::new(self.select()?))
        } else {
            return Err(self.unexpected("VALUES or SELECT"));
        };
        Ok(Statement::Insert { table, columns, source })
    }

    fn update(&mut self) -> Result<Statement> {
        let table = self.ident()?;
        self.expect_kw("SET")?;
        let mut assignments = Vec::new();
        loop {
            let col = self.column_name()?;
            self.expect_sym("=")?;
            assignments.push((col, self.expr()?));
            if !self.eat_sym(",") {
                break;
            }
        }
        let filter = if self.eat_kw("WHERE") { Some(self.expr()?) } else { None };
        Ok(Statement::Update { table, assignments, filter })
    }

    /// A column name, optionally qualified (`t.col`); the qualifier is dropped.
    fn column_name(&mut self) -> Result<String> {
        let mut name = self.ident()?;
        if self.eat_sym(".") {
            name = self.ident()?;
        }
        Ok(name)
    }

    fn delete(&mut self) -> Result<Statement> {
        self.eat_sym("*");
        self.expect_kw("FROM")?;
        let table = self.ident()?;
        let filter = if self.eat_kw("WHERE") { Some(self.expr()?) } else { None };
        Ok(Statement::Delete { table, filter })
    }

    fn ident_list(&mut self) -> Result<Vec<String>> {
        let mut v = vec![self.ident()?];
        while self.eat_sym(",") {
            v.push(self.ident()?);
        }
        Ok(v)
    }

    /// `@a [type], @b [type]` — the declared parameters of a saved query.
    fn param_list(&mut self) -> Result<Vec<ParamDecl>> {
        let mut out = Vec::new();
        loop {
            let name = match self.peek().clone() {
                Tok::Param(n) => {
                    self.advance();
                    n
                }
                _ => return Err(self.unexpected("a parameter such as @name")),
            };
            let mut data_type = None;
            if let Tok::Ident(s) = self.peek().clone() {
                if let Some((t, _)) = DataType::from_sql_name(&s) {
                    self.advance();
                    data_type = Some(t);
                }
            }
            out.push(ParamDecl { name, data_type });
            if !self.eat_sym(",") {
                return Ok(out);
            }
        }
    }

    /// `EXEC name [(] [@p =] value, ... [)]`
    fn exec(&mut self) -> Result<Statement> {
        let name = self.ident()?;
        let paren = self.eat_sym("(");
        let mut args = Vec::new();
        if !(self.at_eof() || self.peek_sym(";") || (paren && self.peek_sym(")"))) {
            loop {
                let mut pname = None;
                if let Tok::Param(n) = self.peek().clone() {
                    if matches!(self.peek_at(1), Tok::Sym("=")) {
                        self.advance();
                        self.advance();
                        pname = Some(n);
                    }
                }
                args.push((pname, self.expr()?));
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        if paren {
            self.expect_sym(")")?;
        }
        Ok(Statement::Exec { name, args })
    }

    /// `name BEFORE|AFTER INSERT|UPDATE|DELETE ON table [FOR EACH ROW]
    /// [WHEN cond] BEGIN stmt; ... END`; `start` is where `CREATE` began.
    fn trigger(&mut self, start: usize) -> Result<Statement> {
        let name = self.ident()?;
        let timing = if self.eat_kw("BEFORE") {
            TriggerTiming::Before
        } else if self.eat_kw("AFTER") {
            TriggerTiming::After
        } else {
            return Err(self.unexpected("BEFORE or AFTER"));
        };
        let event = if self.eat_kw("INSERT") {
            TriggerEvent::Insert
        } else if self.eat_kw("UPDATE") {
            TriggerEvent::Update
        } else if self.eat_kw("DELETE") {
            TriggerEvent::Delete
        } else {
            return Err(self.unexpected("INSERT, UPDATE or DELETE"));
        };
        self.expect_kw("ON")?;
        let table = self.ident()?;
        if self.eat_kw("FOR") {
            self.expect_kw("EACH")?;
            self.expect_kw("ROW")?;
        }
        let when = if self.eat_kw("WHEN") { Some(self.expr()?) } else { None };
        self.expect_kw("BEGIN")?;
        let mut body = Vec::new();
        loop {
            while self.eat_sym(";") {}
            if self.eat_kw("END") {
                break;
            }
            if self.at_eof() {
                return Err(self.unexpected("END"));
            }
            let st = self.statement()?;
            if !matches!(st, Statement::Insert { .. } | Statement::Update { .. } | Statement::Delete { .. } | Statement::Raise(_)) {
                return Err(Error::syntax("a trigger body may contain only INSERT, UPDATE, DELETE and RAISE statements"));
            }
            body.push(st);
            if !self.peek_sym(";") && !self.peek_kw("END") {
                return Err(self.unexpected("';' or END"));
            }
        }
        if body.is_empty() {
            return Err(Error::syntax("a trigger needs at least one statement"));
        }
        let end = self.toks[self.pos - 1].pos + "END".len();
        let sql = self.src[start..end].to_string();
        Ok(Statement::CreateTrigger(Box::new(TriggerDef { name, timing, event, table, when, body, sql })))
    }

    fn create(&mut self) -> Result<Statement> {
        let create_pos = self.toks[self.pos - 1].pos;
        let unique = self.eat_kw("UNIQUE");
        if !unique && self.eat_kw("TRIGGER") {
            return self.trigger(create_pos);
        }
        if self.eat_kw("TABLE") {
            return self.create_table();
        }
        if self.eat_kw("INDEX") {
            let name = self.ident()?;
            self.expect_kw("ON")?;
            let table = self.ident()?;
            self.expect_sym("(")?;
            let columns = self.ident_list()?;
            self.expect_sym(")")?;
            return Ok(Statement::CreateIndex { name, table, columns, unique });
        }
        if !unique && (self.eat_kw("VIEW") || self.eat_kw("QUERY")) {
            let name = self.ident()?;
            // Parameters come either as `name(@a, @b) AS ...` or as a leading
            // `PARAMETERS @a, @b;`; both are stored in the latter form.
            let mut params = Vec::new();
            if self.eat_sym("(") {
                params = self.param_list()?;
                self.expect_sym(")")?;
            }
            self.expect_kw("AS")?;
            if self.peek_kw("EXEC") || self.peek_kw("EXECUTE") || self.peek_kw("CALL") {
                if !params.is_empty() {
                    return Err(Error::syntax("a query that calls another query cannot declare parameters"));
                }
                let start = self.toks[self.pos].pos;
                self.statement()?;
                let end = self.toks[self.pos].pos;
                return Ok(Statement::CreateView { name, sql: self.src[start..end].trim().to_string() });
            }
            if self.eat_kw("PARAMETERS") {
                if !params.is_empty() {
                    return Err(Error::syntax("declare parameters either after the name or with PARAMETERS, not both"));
                }
                params = self.param_list()?;
                self.expect_sym(";")?;
            }
            let start = self.toks[self.pos].pos;
            self.select()?;
            let end = self.toks[self.pos].pos;
            let body = self.src[start..end].trim();
            let sql = if params.is_empty() {
                body.to_string()
            } else {
                let decls: Vec<String> = params
                    .iter()
                    .map(|p| match p.data_type {
                        Some(t) => format!("@{} {}", p.name, t.sql_name()),
                        None => format!("@{}", p.name),
                    })
                    .collect();
                format!("PARAMETERS {}; {body}", decls.join(", "))
            };
            return Ok(Statement::CreateView { name, sql });
        }
        Err(self.unexpected("TABLE, INDEX, VIEW or TRIGGER"))
    }

    fn create_table(&mut self) -> Result<Statement> {
        let name = self.ident()?;
        self.expect_sym("(")?;
        let mut columns = Vec::new();
        let mut primary_key = Vec::new();
        let mut uniques = Vec::new();
        let mut foreign_keys = Vec::new();
        loop {
            if self.peek_kw("FOREIGN") || self.peek_kw("CONSTRAINT") {
                foreign_keys.push(self.table_foreign_key()?);
            } else if self.peek_kw("PRIMARY") {
                self.advance();
                self.expect_kw("KEY")?;
                self.expect_sym("(")?;
                primary_key = self.ident_list()?;
                self.expect_sym(")")?;
            } else if self.peek_kw("UNIQUE") && matches!(self.peek_at(1), Tok::Sym("(")) {
                self.advance();
                self.advance();
                uniques.push(self.ident_list()?);
                self.expect_sym(")")?;
            } else {
                columns.push(self.column_def()?);
            }
            if !self.eat_sym(",") {
                break;
            }
        }
        self.expect_sym(")")?;
        Ok(Statement::CreateTable { name, columns, primary_key, uniques, foreign_keys })
    }

    /// `[CONSTRAINT name] FOREIGN KEY (cols) REFERENCES ...`
    fn table_foreign_key(&mut self) -> Result<ForeignKeyDef> {
        let name = if self.eat_kw("CONSTRAINT") { Some(self.ident()?) } else { None };
        self.expect_kw("FOREIGN")?;
        self.expect_kw("KEY")?;
        self.expect_sym("(")?;
        let columns = self.ident_list()?;
        self.expect_sym(")")?;
        self.expect_kw("REFERENCES")?;
        self.references_clause(name, columns)
    }

    /// Everything after the `REFERENCES` keyword.
    fn references_clause(&mut self, name: Option<String>, columns: Vec<String>) -> Result<ForeignKeyDef> {
        let ref_table = self.ident()?;
        let mut ref_columns = Vec::new();
        if self.eat_sym("(") {
            ref_columns = self.ident_list()?;
            self.expect_sym(")")?;
        }
        let (mut on_delete, mut on_update) = (RefAction::Restrict, RefAction::Restrict);
        while self.eat_kw("ON") {
            let slot = if self.eat_kw("DELETE") {
                &mut on_delete
            } else if self.eat_kw("UPDATE") {
                &mut on_update
            } else {
                return Err(self.unexpected("DELETE or UPDATE"));
            };
            *slot = if self.eat_kw("CASCADE") {
                RefAction::Cascade
            } else if self.eat_kw("RESTRICT") {
                RefAction::Restrict
            } else if self.eat_kw("NO") {
                self.expect_kw("ACTION")?;
                RefAction::Restrict
            } else if self.eat_kw("SET") {
                self.expect_kw("NULL")?;
                RefAction::SetNull
            } else {
                return Err(self.unexpected("CASCADE, SET NULL, RESTRICT or NO ACTION"));
            };
        }
        Ok(ForeignKeyDef { name, columns, ref_table, ref_columns, on_delete, on_update })
    }

    /// `[AS] (expression)` after the type name CALCULATED; returns the expression text.
    fn calculated_expression(&mut self, field: &str) -> Result<String> {
        self.eat_kw("AS");
        self.expect_sym("(")?;
        let start = self.toks[self.pos].pos;
        if let Err(e) = self.expr() {
            // `&&` is a common slip; the parser stops on the second `&`.
            let doubled = matches!(self.peek(), Tok::Sym("&&") | Tok::Sym("&"))
                && self.pos > 0
                && matches!(self.toks[self.pos - 1].tok, Tok::Sym("&"));
            let hint = if doubled { " (to join text use a single &)" } else { "" };
            return Err(Error::syntax(format!(
                "the calculated field '{field}': the formula is not valid: {e}{hint}"
            )));
        }
        let end = self.toks[self.pos].pos;
        self.expect_sym(")")?;
        Ok(self.src[start..end].trim().to_string())
    }

    fn column_def(&mut self) -> Result<ColumnDef> {
        let name = self.ident()?;
        let type_name = match self.advance() {
            Tok::Ident(s) => s,
            _ => {
                self.pos = self.pos.saturating_sub(1);
                return Err(self.unexpected("a data type"));
            }
        };
        let (data_type, autonumber) = DataType::from_sql_name(&type_name)
            .ok_or_else(|| Error::syntax(format!("unknown data type '{type_name}'")))?;
        let mut expression = None;
        if data_type == DataType::Calculated {
            expression = Some(self.calculated_expression(&name)?);
        } else if self.eat_sym("(") {
            // Length / precision hints are accepted and ignored.
            while !self.peek_sym(")") && !self.at_eof() {
                self.advance();
            }
            self.expect_sym(")")?;
        }
        let mut def = ColumnDef {
            name,
            data_type,
            not_null: false,
            primary_key: false,
            unique: false,
            autonumber,
            default: None,
            references: None,
            expression,
        };
        loop {
            if self.eat_kw("NOT") {
                self.expect_kw("NULL")?;
                def.not_null = true;
            } else if self.eat_kw("NULL") {
            } else if self.eat_kw("PRIMARY") {
                self.expect_kw("KEY")?;
                def.primary_key = true;
            } else if self.eat_kw("UNIQUE") {
                def.unique = true;
            } else if self.eat_kw("REFERENCES") {
                def.references = Some(self.references_clause(None, vec![def.name.clone()])?);
            } else if self.eat_kw("AUTOINCREMENT") || self.eat_kw("IDENTITY") {
                def.autonumber = true;
            } else if self.eat_kw("DEFAULT") {
                let neg = self.eat_sym("-");
                let v = match self.primary()? {
                    Expr::Literal(v) => v,
                    _ => return Err(Error::syntax("DEFAULT must be a literal")),
                };
                def.default = Some(if neg {
                    match v {
                        n => negate_literal(&n).ok_or_else(|| Error::syntax("cannot negate DEFAULT value"))?,
                    }
                } else {
                    v
                });
            } else {
                break;
            }
        }
        Ok(def)
    }

    fn alter(&mut self) -> Result<Statement> {
        self.expect_kw("TABLE")?;
        let table = self.ident()?;
        let action = if self.eat_kw("ADD") {
            if self.peek_kw("FOREIGN") || self.peek_kw("CONSTRAINT") {
                AlterAction::AddForeignKey(self.table_foreign_key()?)
            } else {
                self.eat_kw("COLUMN");
                AlterAction::AddColumn(self.column_def()?)
            }
        } else if self.eat_kw("DROP") {
            if self.eat_kw("CONSTRAINT") {
                AlterAction::DropConstraint(self.ident()?)
            } else {
                self.eat_kw("COLUMN");
                AlterAction::DropColumn(self.ident()?)
            }
        } else if self.eat_kw("RENAME") {
            self.eat_kw("COLUMN");
            let from = self.ident()?;
            self.expect_kw("TO")?;
            AlterAction::RenameColumn { from, to: self.ident()? }
        } else if self.eat_kw("ALTER") {
            self.eat_kw("COLUMN");
            let name = self.ident()?;
            self.eat_kw("TYPE");
            let type_name = match self.advance() {
                Tok::Ident(s) => s,
                _ => {
                    self.pos = self.pos.saturating_sub(1);
                    return Err(self.unexpected("a data type"));
                }
            };
            let (data_type, autonumber) = DataType::from_sql_name(&type_name)
                .ok_or_else(|| Error::syntax(format!("unknown data type '{type_name}'")))?;
            if data_type == DataType::Calculated {
                let expr = self.calculated_expression(&name)?;
                return Ok(Statement::AlterTable { table, action: AlterAction::SetExpression { name, expr } });
            }
            if autonumber {
                return Err(Error::syntax("a column cannot be converted to AutoNumber"));
            }
            if self.eat_sym("(") {
                while !self.peek_sym(")") && !self.at_eof() {
                    self.advance();
                }
                self.expect_sym(")")?;
            }
            AlterAction::AlterColumn { name, data_type }
        } else {
            return Err(self.unexpected("ADD, DROP, RENAME or ALTER"));
        };
        Ok(Statement::AlterTable { table, action })
    }

    fn drop_stmt(&mut self) -> Result<Statement> {
        if self.eat_kw("TABLE") {
            Ok(Statement::DropTable(self.ident()?))
        } else if self.eat_kw("VIEW") || self.eat_kw("QUERY") {
            Ok(Statement::DropView(self.ident()?))
        } else if self.eat_kw("TRIGGER") {
            Ok(Statement::DropTrigger(self.ident()?))
        } else if self.eat_kw("INDEX") {
            let name = self.ident()?;
            self.expect_kw("ON")?;
            Ok(Statement::DropIndex { name, table: self.ident()? })
        } else {
            Err(self.unexpected("TABLE, VIEW, INDEX or TRIGGER"))
        }
    }

    // ----- SELECT -----------------------------------------------------------

    fn select(&mut self) -> Result<Select> {
        self.expect_kw("SELECT")?;
        let mut sel = Select::default();
        // DISTINCT / ALL and TOP n may come in either order.
        loop {
            if self.eat_kw("DISTINCT") {
                sel.distinct = true;
            } else if self.eat_kw("ALL") {
            } else if sel.top.is_none() && self.eat_kw("TOP") {
                sel.top = Some(self.usize_literal()?);
            } else {
                break;
            }
        }
        loop {
            sel.items.push(self.select_item()?);
            if !self.eat_sym(",") {
                break;
            }
        }
        if self.eat_kw("FROM") {
            sel.from = Some(self.from_clause()?);
        }
        if self.eat_kw("WHERE") {
            sel.filter = Some(self.expr()?);
        }
        if self.eat_kw("GROUP") {
            self.expect_kw("BY")?;
            sel.group_by.push(self.expr()?);
            while self.eat_sym(",") {
                sel.group_by.push(self.expr()?);
            }
        }
        if self.eat_kw("HAVING") {
            sel.having = Some(self.expr()?);
        }
        if self.eat_kw("ORDER") {
            self.expect_kw("BY")?;
            loop {
                let expr = self.expr()?;
                let desc = if self.eat_kw("DESC") {
                    true
                } else {
                    self.eat_kw("ASC");
                    false
                };
                sel.order_by.push(OrderItem { expr, desc });
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        if self.eat_kw("LIMIT") {
            sel.limit = Some(self.usize_literal()?);
            if self.eat_kw("OFFSET") {
                sel.offset = Some(self.usize_literal()?);
            }
        }
        Ok(sel)
    }

    fn select_item(&mut self) -> Result<SelectItem> {
        if self.eat_sym("*") {
            return Ok(SelectItem::Wildcard);
        }
        if matches!(self.peek(), Tok::Ident(_) | Tok::Quoted(_))
            && matches!(self.peek_at(1), Tok::Sym("."))
            && matches!(self.peek_at(2), Tok::Sym("*"))
        {
            let t = self.ident()?;
            self.advance();
            self.advance();
            return Ok(SelectItem::QualifiedWildcard(t));
        }
        let expr = self.expr()?;
        let alias = if self.eat_kw("AS") {
            Some(self.ident()?)
        } else if self.peek_is_alias() {
            Some(self.ident()?)
        } else {
            None
        };
        Ok(SelectItem::Expr { expr, alias })
    }

    fn from_clause(&mut self) -> Result<From> {
        let mut from = self.from_source()?;
        loop {
            let kind = if self.eat_kw("INNER") {
                self.expect_kw("JOIN")?;
                JoinKind::Inner
            } else if self.eat_kw("LEFT") {
                self.eat_kw("OUTER");
                self.expect_kw("JOIN")?;
                JoinKind::Left
            } else if self.eat_kw("RIGHT") {
                self.eat_kw("OUTER");
                self.expect_kw("JOIN")?;
                JoinKind::Right
            } else if self.eat_kw("CROSS") {
                self.expect_kw("JOIN")?;
                JoinKind::Cross
            } else if self.eat_kw("JOIN") {
                JoinKind::Inner
            } else if self.eat_sym(",") {
                JoinKind::Cross
            } else {
                break;
            };
            let factor = self.table_factor()?;
            let on = if kind != JoinKind::Cross {
                self.expect_kw("ON")?;
                Some(self.expr()?)
            } else {
                None
            };
            from.joins.push(Join { kind, factor, on });
        }
        Ok(from)
    }

    /// A table factor, or a parenthesised join list (Access nests joins).
    fn from_source(&mut self) -> Result<From> {
        if self.peek_sym("(") && !self.peek_kw_at(1, "SELECT") {
            self.advance();
            let inner = self.from_clause()?;
            self.expect_sym(")")?;
            Ok(inner)
        } else {
            Ok(From { first: self.table_factor()?, joins: Vec::new() })
        }
    }

    fn table_factor(&mut self) -> Result<TableFactor> {
        if self.peek_sym("(") && self.peek_kw_at(1, "SELECT") {
            self.advance();
            let query = Box::new(self.select()?);
            self.expect_sym(")")?;
            self.eat_kw("AS");
            let alias = self.ident()?;
            return Ok(TableFactor::Derived { query, alias });
        }
        let name = self.ident()?;
        let alias = if self.eat_kw("AS") || self.peek_is_alias() { Some(self.ident()?) } else { None };
        Ok(TableFactor::Named { name, alias })
    }

    // ----- expressions ------------------------------------------------------

    pub(crate) fn expr(&mut self) -> Result<Expr> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> Result<Expr> {
        let mut left = self.and_expr()?;
        while self.eat_kw("OR") {
            let right = self.and_expr()?;
            left = Expr::Binary { op: BinOp::Or, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Expr> {
        let mut left = self.not_expr()?;
        while self.eat_kw("AND") {
            let right = self.not_expr()?;
            left = Expr::Binary { op: BinOp::And, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn not_expr(&mut self) -> Result<Expr> {
        if self.peek_kw("NOT") && !self.peek_kw_at(1, "EXISTS") {
            self.advance();
            return Ok(Expr::Not(Box::new(self.not_expr()?)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Expr> {
        let mut left = self.concat()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("=") => Some(BinOp::Eq),
                Tok::Sym("<>") | Tok::Sym("!=") => Some(BinOp::Ne),
                Tok::Sym("<") => Some(BinOp::Lt),
                Tok::Sym("<=") => Some(BinOp::Le),
                Tok::Sym(">") => Some(BinOp::Gt),
                Tok::Sym(">=") => Some(BinOp::Ge),
                _ => None,
            };
            if let Some(op) = op {
                self.advance();
                let right = self.concat()?;
                left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
                continue;
            }
            if self.eat_kw("IS") {
                let negated = self.eat_kw("NOT");
                self.expect_kw("NULL")?;
                left = Expr::IsNull { expr: Box::new(left), negated };
                continue;
            }
            let negated = self.peek_kw("NOT")
                && (self.peek_kw_at(1, "LIKE") || self.peek_kw_at(1, "BETWEEN") || self.peek_kw_at(1, "IN"));
            if negated {
                self.advance();
            }
            if self.eat_kw("LIKE") {
                let pattern = self.concat()?;
                left = Expr::Like { expr: Box::new(left), pattern: Box::new(pattern), negated };
            } else if self.eat_kw("BETWEEN") {
                let low = self.concat()?;
                self.expect_kw("AND")?;
                let high = self.concat()?;
                left = Expr::Between { expr: Box::new(left), low: Box::new(low), high: Box::new(high), negated };
            } else if self.eat_kw("IN") {
                self.expect_sym("(")?;
                if self.peek_kw("SELECT") {
                    let q = self.select()?;
                    self.expect_sym(")")?;
                    left = Expr::InSelect { expr: Box::new(left), query: Box::new(q), negated };
                } else {
                    let mut list = vec![self.expr()?];
                    while self.eat_sym(",") {
                        list.push(self.expr()?);
                    }
                    self.expect_sym(")")?;
                    left = Expr::InList { expr: Box::new(left), list, negated };
                }
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn concat(&mut self) -> Result<Expr> {
        let mut left = self.additive()?;
        while self.eat_sym("&") {
            let right = self.additive()?;
            left = Expr::Binary { op: BinOp::Concat, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Expr> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("+") => BinOp::Add,
                Tok::Sym("-") => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.multiplicative()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<Expr> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("*") => BinOp::Mul,
                Tok::Sym("/") => BinOp::Div,
                Tok::Sym("\\") => BinOp::IntDiv,
                Tok::Sym("%") => BinOp::Mod,
                Tok::Ident(s) if s.eq_ignore_ascii_case("MOD") => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let right = self.unary()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr> {
        if self.eat_sym("-") {
            let inner = self.unary()?;
            return Ok(match inner {
                Expr::Literal(n) if n.is_numeric() => Expr::Literal(negate_literal(&n).expect("a number negates")),
                other => Expr::Neg(Box::new(other)),
            });
        }
        if self.eat_sym("+") {
            return self.unary();
        }
        self.power()
    }

    fn power(&mut self) -> Result<Expr> {
        let base = self.primary()?;
        if self.eat_sym("^") {
            let exp = self.unary()?;
            return Ok(Expr::Binary { op: BinOp::Pow, left: Box::new(base), right: Box::new(exp) });
        }
        Ok(base)
    }

    fn primary(&mut self) -> Result<Expr> {
        match self.peek().clone() {
            Tok::Int(i) => {
                self.advance();
                Ok(Expr::Literal(Value::Int(i)))
            }
            Tok::Dec(d) => {
                self.advance();
                Ok(Expr::Literal(Value::Dec(d)))
            }
            Tok::Big(b) => {
                self.advance();
                Ok(Expr::Literal(Value::Big(b)))
            }
            Tok::Float(f) => {
                self.advance();
                Ok(Expr::Literal(Value::Double(f)))
            }
            Tok::Str(s) => {
                self.advance();
                Ok(Expr::Literal(Value::text(&s)))
            }
            Tok::Date(d) => {
                self.advance();
                Ok(Expr::Literal(Value::DateTime(d)))
            }
            Tok::Sym("(") => {
                self.advance();
                if self.peek_kw("SELECT") {
                    let q = self.select()?;
                    self.expect_sym(")")?;
                    return Ok(Expr::Subquery(Box::new(q)));
                }
                let e = self.expr()?;
                self.expect_sym(")")?;
                Ok(e)
            }
            Tok::Param(p) => {
                self.advance();
                Ok(Expr::Param(p))
            }
            Tok::Quoted(_) => self.column_ref(),
            Tok::Ident(name) => {
                let up = name.to_ascii_uppercase();
                match up.as_str() {
                    "NULL" => {
                        self.advance();
                        return Ok(Expr::Literal(Value::Null));
                    }
                    "TRUE" | "YES" => {
                        self.advance();
                        return Ok(Expr::Literal(Value::Bool(true)));
                    }
                    "FALSE" | "NO" => {
                        self.advance();
                        return Ok(Expr::Literal(Value::Bool(false)));
                    }
                    "CASE" => return self.case_expr(),
                    "EXISTS" => {
                        self.advance();
                        return self.exists(false);
                    }
                    "NOT" if self.peek_kw_at(1, "EXISTS") => {
                        self.advance();
                        self.advance();
                        return self.exists(true);
                    }
                    _ => {}
                }
                // LEFT and RIGHT are join keywords but also string functions.
                let callable = !RESERVED.contains(&up.as_str()) || up == "LEFT" || up == "RIGHT";
                if matches!(self.peek_at(1), Tok::Sym("(")) && callable {
                    self.advance();
                    self.advance();
                    return self.function_call(name);
                }
                self.column_ref()
            }
            _ => Err(self.unexpected("an expression")),
        }
    }

    fn exists(&mut self, negated: bool) -> Result<Expr> {
        self.expect_sym("(")?;
        let q = self.select()?;
        self.expect_sym(")")?;
        Ok(Expr::Exists { query: Box::new(q), negated })
    }

    fn column_ref(&mut self) -> Result<Expr> {
        let first = self.ident()?;
        if self.eat_sym(".") {
            let name = self.ident()?;
            Ok(Expr::Column { table: Some(first), name })
        } else {
            Ok(Expr::Column { table: None, name: first })
        }
    }

    fn function_call(&mut self, name: String) -> Result<Expr> {
        let mut args = Vec::new();
        let mut distinct = false;
        let mut star = false;
        if self.eat_sym("*") {
            star = true;
        } else if !self.peek_sym(")") {
            distinct = self.eat_kw("DISTINCT");
            args.push(self.expr()?);
            while self.eat_sym(",") {
                args.push(self.expr()?);
            }
        }
        self.expect_sym(")")?;
        Ok(Expr::Function { name: name.to_ascii_uppercase(), args, distinct, star })
    }

    fn case_expr(&mut self) -> Result<Expr> {
        self.expect_kw("CASE")?;
        let mut whens = Vec::new();
        while self.eat_kw("WHEN") {
            let cond = self.expr()?;
            self.expect_kw("THEN")?;
            whens.push((cond, self.expr()?));
        }
        if whens.is_empty() {
            return Err(self.unexpected("WHEN"));
        }
        let else_expr = if self.eat_kw("ELSE") { Some(Box::new(self.expr()?)) } else { None };
        self.expect_kw("END")?;
        Ok(Expr::Case { whens, else_expr })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_shapes() {
        let s = parse_select(
            "SELECT TOP 5 DISTINCT c.Name, Sum(o.Total) AS T FROM (Customers c INNER JOIN Orders o ON c.ID = o.CustID) \
             WHERE o.Total > 10 AND c.Name LIKE 'A*' GROUP BY c.Name HAVING Sum(o.Total) > 5 ORDER BY 2 DESC",
        );
        let s = s.unwrap();
        assert_eq!(s.top, Some(5));
        assert!(s.distinct);
        assert_eq!(s.from.as_ref().unwrap().joins.len(), 1);
        assert_eq!(s.order_by.len(), 1);
    }

    #[test]
    fn precedence() {
        let Statement::Select(s) = parse_statement("SELECT 1 + 2 * 3 ^ 2").unwrap() else { panic!() };
        let SelectItem::Expr { expr, .. } = &s.items[0] else { panic!() };
        let Expr::Binary { op: BinOp::Add, right, .. } = expr else { panic!("{expr:?}") };
        assert!(matches!(**right, Expr::Binary { op: BinOp::Mul, .. }));
    }

    #[test]
    fn ddl_and_dml() {
        assert!(parse_script(
            "CREATE TABLE t (id COUNTER PRIMARY KEY, name TEXT(50) NOT NULL, price CURRENCY DEFAULT 0); \
             INSERT INTO t (name) VALUES ('a'), ('b'); UPDATE t SET name = 'z' WHERE id = 1; DELETE FROM t;"
        )
        .unwrap()
        .len()
            == 4);
    }

    #[test]
    fn view_text_is_captured() {
        let Statement::CreateView { sql, .. } = parse_statement("CREATE VIEW v AS SELECT a FROM t WHERE a > 1;").unwrap()
        else {
            panic!()
        };
        assert_eq!(sql, "SELECT a FROM t WHERE a > 1");
    }

    #[test]
    fn alter_table() {
        assert!(matches!(
            parse_statement("ALTER TABLE t ADD COLUMN x TEXT NOT NULL DEFAULT 'a'").unwrap(),
            Statement::AlterTable { action: AlterAction::AddColumn(_), .. }
        ));
        assert!(matches!(
            parse_statement("ALTER TABLE t RENAME COLUMN a TO b").unwrap(),
            Statement::AlterTable { action: AlterAction::RenameColumn { .. }, .. }
        ));
        assert!(parse_statement("ALTER TABLE t ALTER COLUMN a COUNTER").is_err());
        assert!(parse_statement("ALTER TABLE t FROB").is_err());
    }

    #[test]
    fn errors_are_reported() {
        assert!(parse_statement("SELECT FROM").is_err());
        assert!(parse_statement("SELECT 1 2").is_err());
    }
}
