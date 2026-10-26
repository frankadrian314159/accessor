//! Recursive-descent parser for the SQL dialect.

use super::ast::*;
use super::usertypes::UserType;

mod pgfunc;
use super::lexer::{lex, Tok, Token};
use crate::error::{Error, Result};
use crate::schema::RefAction;
use crate::sequence::SequenceOptions;
use crate::value::{DataType, Value};

/// Stands for `DEFAULT` in a `VALUES` row until the insert is planned.
pub(crate) const DEFAULT_MARK: &str = "\u{1}default";

const RESERVED: &[&str] = &[
    "SELECT", "FROM", "WHERE", "GROUP", "BY", "HAVING", "ORDER", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "OUTER",
    "CROSS", "ON", "AS", "AND", "OR", "NOT", "IN", "IS", "NULL", "LIKE", "BETWEEN", "UNION", "LIMIT", "OFFSET", "SET",
    "VALUES", "INTO", "DISTINCT", "TOP", "ASC", "DESC", "CASE", "WHEN", "THEN", "ELSE", "END", "EXISTS", "INSERT",
    "UPDATE", "DELETE", "CREATE", "DROP", "TABLE", "INDEX", "VIEW", "MOD", "DIV", "ALL", "NATURAL", "USING", "INTERSECT",
    "EXCEPT", "FETCH", "PIVOT", "UNPIVOT",
];

/// Parses a script of one or more `;`-separated statements.
pub fn parse_script(sql: &str) -> Result<Vec<Statement>> {
    let mut p = Parser { src: sql, toks: lex(sql)?, pos: 0, into_vars: Vec::new() };
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

/// The conversion function that `CAST(x AS ty)` stands for.
pub(crate) fn cast_function(ty: DataType) -> Option<&'static str> {
    Some(match ty {
        DataType::Integer => "CINT",
        DataType::Bignum => "CBIG",
        DataType::Float => "CSNG",
        DataType::Double => "CDBL",
        DataType::Decimal => "CDEC",
        DataType::Currency => "CCUR",
        DataType::Rational => "CRAT",
        DataType::Text => "CSTR",
        DataType::YesNo => "CBOOL",
        DataType::DateTime => "CDATE",
        DataType::TimestampTz => "CTIMESTAMPTZ",
        DataType::Interval => "CINTERVAL",
        DataType::Json => "JSON_PARSE",
        DataType::Calculated => return None,
    })
}

/// Parses a stand-alone expression (the stored form of a calculated field).
pub fn parse_expression(sql: &str) -> Result<Expr> {
    let mut p = Parser { src: sql, toks: lex(sql)?, pos: 0, into_vars: Vec::new() };
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

/// Prefixes every identifier of `text` for which `is_column` holds with `prefix`
/// (`age > 0` becomes `NEW.age > 0`), leaving function names and already
/// qualified names alone.
pub fn qualify_columns(text: &str, prefix: &str, is_column: &dyn Fn(&str) -> bool) -> Result<String> {
    let toks = lex(text)?;
    let mut out = String::new();
    let mut last = 0;
    for (i, t) in toks.iter().enumerate() {
        let name = match &t.tok {
            Tok::Ident(s) | Tok::Quoted(s) => s,
            _ => continue,
        };
        let after_dot = i > 0 && matches!(toks[i - 1].tok, Tok::Sym("."));
        let before_paren = matches!(toks.get(i + 1).map(|t| &t.tok), Some(Tok::Sym("(")));
        if after_dot || before_paren || !is_column(name) {
            continue;
        }
        out.push_str(&text[last..t.pos]);
        out.push_str(prefix);
        out.push('.');
        last = t.pos;
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// Rewrites references to table `old` in stored SQL (a trigger or saved query) after the table
/// is renamed to `new`: the name after `FROM`, `JOIN`, `INTO`, `UPDATE` and `ON`, and a name used
/// as a prefix (`old.column`).
pub fn rename_table_in_sql(text: &str, old: &str, new: &str) -> String {
    let Ok(toks) = lex(text) else { return text.to_string() };
    let mut out = String::new();
    let mut last = 0;
    for (i, t) in toks.iter().enumerate() {
        let (name, len) = match &t.tok {
            Tok::Ident(s) => (s, s.len()),
            Tok::Quoted(s) => (s, s.len() + 2),
            _ => continue,
        };
        if !name.eq_ignore_ascii_case(old) {
            continue;
        }
        let prev = i.checked_sub(1).map(|j| &toks[j].tok);
        let after_dot = matches!(prev, Some(Tok::Sym(".")));
        let after_keyword = matches!(prev, Some(Tok::Ident(k)) if ["FROM", "JOIN", "INTO", "UPDATE", "ON", "TABLE"].iter().any(|w| k.eq_ignore_ascii_case(w)));
        let qualifier = matches!(toks.get(i + 1).map(|t| &t.tok), Some(Tok::Sym(".")));
        if after_dot || !(after_keyword || qualifier) {
            continue;
        }
        out.push_str(&text[last..t.pos]);
        out.push_str(&format!("[{new}]"));
        last = t.pos + len;
    }
    out.push_str(&text[last..]);
    out
}

/// Rewrites `alias.col` to `table.col` in an UPDATE or DELETE that named its target with an alias.
fn requalify(st: &mut Statement, alias: &str, table: &str) -> Result<()> {
    crate::sql::walk::walk_statement(st, &mut |e| {
        if let Expr::Column { table: Some(t), .. } = e {
            if t.eq_ignore_ascii_case(alias) {
                *t = table.to_string();
            }
        }
        Ok(())
    })
}

/// The marker function that stands for a row value `(a, b)` until the comparison it is
/// part of has been parsed; it never reaches the executor.
const ROW_MARKER: &str = "__ROW";

fn row_value(items: Vec<Expr>) -> Expr {
    Expr::Function { name: ROW_MARKER.into(), args: items, distinct: false, star: false }
}

fn row_items(e: &Expr) -> Option<&[Expr]> {
    match e {
        Expr::Function { name, args, .. } if name == ROW_MARKER => Some(args),
        _ => None,
    }
}

/// `(a1, a2, ..) op (b1, b2, ..)`, written out as comparisons of the parts.
fn row_compare(op: BinOp, a: &[Expr], b: &[Expr]) -> Result<Expr> {
    if a.len() != b.len() {
        return Err(Error::syntax(format!("the row values have {} and {} parts; they must be the same size", a.len(), b.len())));
    }
    let bin = |op: BinOp, l: Expr, r: Expr| Expr::Binary { op, left: Box::new(l), right: Box::new(r) };
    let pair = |op: BinOp, i: usize| bin(op, a[i].clone(), b[i].clone());
    let chain = |join: BinOp, op: BinOp| -> Expr {
        (1..a.len()).fold(pair(op, 0), |acc, i| bin(join, acc, pair(op, i)))
    };
    Ok(match op {
        BinOp::Eq => chain(BinOp::And, BinOp::Eq),
        BinOp::Ne => chain(BinOp::Or, BinOp::Ne),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            // Lexicographic: the first differing part decides.
            let strict = if matches!(op, BinOp::Lt | BinOp::Le) { BinOp::Lt } else { BinOp::Gt };
            let mut result: Option<Expr> = None;
            for i in 0..a.len() {
                let last = if i + 1 == a.len() { op } else { strict };
                let mut term = pair(last, i);
                for j in (0..i).rev() {
                    term = bin(BinOp::And, pair(BinOp::Eq, j), term);
                }
                result = Some(match result {
                    None => term,
                    Some(prev) => bin(BinOp::Or, prev, term),
                });
            }
            // `(a, b) > (x, y)` implies `a >= x`: stating that lets an index on `a` narrow the read
            // to the rows from `x` on, which is what makes keyset paging cheap.
            let bound = if matches!(op, BinOp::Lt | BinOp::Le) { BinOp::Le } else { BinOp::Ge };
            bin(BinOp::And, pair(bound, 0), result.expect("at least one part"))
        }
        _ => return Err(Error::syntax("this operator cannot be used with row values")),
    })
}

/// `(a, b) [NOT] IN (query)` as an EXISTS over the query with its columns compared in turn.
fn row_in_query(items: &[Expr], q: Select, negated: bool) -> Expr {
    let columns: Vec<String> = (0..items.len()).map(|i| format!("__c{i}")).collect();
    let mut filter: Option<Expr> = None;
    for (i, item) in items.iter().enumerate() {
        let eq = Expr::Binary {
            op: BinOp::Eq,
            left: Box::new(item.clone()),
            right: Box::new(Expr::Column { table: Some("__r".into()), name: columns[i].clone() }),
        };
        filter = Some(match filter {
            None => eq,
            Some(prev) => Expr::Binary { op: BinOp::And, left: Box::new(prev), right: Box::new(eq) },
        });
    }
    Expr::Exists {
        query: Box::new(Select {
            items: vec![SelectItem::Expr { expr: Expr::Literal(Value::Int(1)), alias: None }],
            from: Some(From {
                first: TableFactor::Derived { query: Box::new(q), alias: "__r".into(), columns, lateral: false },
                joins: Vec::new(),
            }),
            filter,
            ..Select::default()
        }),
        negated,
    }
}

/// `a IS [NOT] DISTINCT FROM b`: like `<>`/`=`, but NULLs compare as equal to each other.
fn distinct_from(a: Expr, b: Expr, negated: bool) -> Expr {
    let null = |e: &Expr| Expr::IsNull { expr: Box::new(e.clone()), negated: false };
    let both_null = Expr::Binary { op: BinOp::And, left: Box::new(null(&a)), right: Box::new(null(&b)) };
    let one_null = Expr::Binary { op: BinOp::Or, left: Box::new(null(&a)), right: Box::new(null(&b)) };
    let differ = Expr::Binary { op: BinOp::Ne, left: Box::new(a), right: Box::new(b) };
    let lit = |b: bool| Expr::Literal(Value::Bool(b));
    Expr::Case {
        whens: vec![(both_null, lit(negated)), (one_null, lit(!negated))],
        else_expr: Some(Box::new(if negated { Expr::Not(Box::new(differ)) } else { differ })),
    }
}

/// `x IS [NOT] TRUE|FALSE`: never unknown.
fn truth_test(x: Expr, want: Option<bool>, negated: bool) -> Expr {
    let lit = |b: bool| Expr::Literal(Value::Bool(b));
    let is = match want {
        Some(true) => Expr::Case { whens: vec![(x, lit(true))], else_expr: Some(Box::new(lit(false))) },
        _ => Expr::Case {
            whens: vec![(Expr::Not(Box::new(x)), lit(true))],
            else_expr: Some(Box::new(lit(false))),
        },
    };
    if negated { Expr::Not(Box::new(is)) } else { is }
}

/// `LIKE pattern ESCAPE e`: each escaped character becomes a one-character set,
/// which matches that character literally.
fn apply_escape(pattern: Expr, esc: Expr) -> Result<Expr> {
    let (Expr::Literal(Value::Text(p)), Expr::Literal(Value::Text(e))) = (&pattern, &esc) else {
        return Err(Error::syntax("LIKE ... ESCAPE needs a literal pattern and a literal escape character"));
    };
    let mut e_chars = e.chars();
    let (Some(e), None) = (e_chars.next(), e_chars.next()) else {
        return Err(Error::syntax("the ESCAPE value must be a single character"));
    };
    let mut out = String::new();
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        if c == e {
            match chars.next() {
                Some(']') => out.push_str("]"),
                Some('[') => out.push_str("[[]"),
                Some(n) => {
                    out.push('[');
                    out.push(n);
                    out.push(']');
                }
                None => return Err(Error::syntax("the pattern ends with the ESCAPE character")),
            }
        } else {
            out.push(c);
        }
    }
    Ok(Expr::Literal(Value::text(&out)))
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    pos: usize,
    /// The variables of a `SELECT ... INTO @a, @b` just read.
    into_vars: Vec<String>,
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

    /// A table name, possibly qualified with a schema (`s.t`). `public` and `pg_catalog` are the default schemas and
    /// are dropped; any other schema stays part of the name (`s.t` is a table of its own, which `CREATE SCHEMA s`
    /// must have announced).
    fn table_ident(&mut self) -> Result<String> {
        let name = self.ident()?;
        if self.peek_sym(".") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Quoted(_)) {
            self.advance();
            let table = self.ident()?;
            if name.eq_ignore_ascii_case("public") || name.eq_ignore_ascii_case("pg_catalog") {
                return Ok(table);
            }
            return Ok(format!("{name}.{table}"));
        }
        Ok(name)
    }

    /// A role name: any word, even a reserved one such as `CURRENT_USER`.
    fn role_name(&mut self) -> Result<String> {
        match self.peek().clone() {
            Tok::Ident(s) | Tok::Quoted(s) | Tok::Str(s) => {
                self.advance();
                Ok(s)
            }
            _ => Err(self.unexpected("a role name")),
        }
    }

    fn role_list(&mut self) -> Result<Vec<String>> {
        let mut v = vec![self.role_name()?];
        while self.eat_sym(",") {
            v.push(self.role_name()?);
        }
        Ok(v)
    }

    /// The options of `CREATE ROLE` and `ALTER ROLE`, as `(name, value)` pairs.
    fn role_options(&mut self) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        self.eat_kw("WITH");
        loop {
            let Tok::Ident(word) = self.peek().clone() else { break };
            let up = word.to_ascii_uppercase();
            match up.as_str() {
                "SUPERUSER" | "CREATEDB" | "CREATEROLE" | "INHERIT" | "LOGIN" | "REPLICATION" | "BYPASSRLS" => {
                    self.advance();
                    out.push((up.to_ascii_lowercase(), "true".into()));
                }
                "NOSUPERUSER" | "NOCREATEDB" | "NOCREATEROLE" | "NOINHERIT" | "NOLOGIN" | "NOREPLICATION" | "NOBYPASSRLS" => {
                    self.advance();
                    out.push((up[2..].to_ascii_lowercase(), "false".into()));
                }
                "ENCRYPTED" | "UNENCRYPTED" => {
                    self.advance();
                }
                "PASSWORD" => {
                    self.advance();
                    let v = if self.eat_kw("NULL") {
                        String::new()
                    } else {
                        match self.advance() {
                            Tok::Str(s) => s,
                            _ => {
                                self.pos -= 1;
                                return Err(self.unexpected("a password in quotes"));
                            }
                        }
                    };
                    out.push(("password".into(), v));
                }
                "CONNECTION" => {
                    self.advance();
                    self.expect_kw("LIMIT")?;
                    let neg = self.eat_sym("-");
                    let n = match self.advance() {
                        Tok::Int(i) => i,
                        _ => {
                            self.pos -= 1;
                            return Err(self.unexpected("a number"));
                        }
                    };
                    out.push(("connection limit".into(), if neg { format!("-{n}") } else { n.to_string() }));
                }
                "VALID" => {
                    self.advance();
                    self.expect_kw("UNTIL")?;
                    match self.advance() {
                        Tok::Str(s) => out.push(("valid until".into(), s)),
                        _ => {
                            self.pos -= 1;
                            return Err(self.unexpected("a time in quotes"));
                        }
                    }
                }
                "IN" => {
                    self.advance();
                    if !self.eat_kw("ROLE") {
                        self.expect_kw("GROUP")?;
                    }
                    out.push(("in role".into(), self.role_list()?.join(",")));
                }
                "ROLE" | "USER" | "ADMIN" => {
                    self.advance();
                    out.push((up.to_ascii_lowercase(), self.role_list()?.join(",")));
                }
                "SYSID" => {
                    self.advance();
                    self.advance();
                }
                _ => break,
            }
        }
        Ok(out)
    }

    fn options_text(options: &[(String, String)]) -> String {
        let mut map = serde_json::Map::new();
        for (k, v) in options {
            map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        serde_json::Value::Object(map).to_string()
    }

    fn alter_role(&mut self) -> Result<Statement> {
        self.advance();
        let name = self.role_name()?;
        if self.eat_kw("RENAME") {
            self.expect_kw("TO")?;
            return Ok(Statement::AlterRole { name, rename: Some(self.role_name()?), options: Vec::new() });
        }
        if self.peek_kw("SET") || self.peek_kw("RESET") || self.peek_kw("IN") {
            // `ALTER ROLE r SET param TO value`: per-role settings are not kept
            while !self.at_eof() && !self.peek_sym(";") {
                self.advance();
            }
            return Ok(Statement::AlterRole { name, rename: None, options: Vec::new() });
        }
        let options = self.role_options()?;
        Ok(Statement::AlterRole { name, rename: None, options })
    }

    /// `GRANT` and `REVOKE`.
    fn grant(&mut self) -> Result<Statement> {
        let revoke = self.eat_kw("REVOKE");
        if !revoke {
            self.expect_kw("GRANT")?;
        }
        if revoke && (self.eat_kw("GRANT") || self.eat_kw("ADMIN")) {
            self.expect_kw("OPTION")?;
            self.expect_kw("FOR")?;
        }
        let mut privileges: Vec<String> = Vec::new();
        loop {
            let Tok::Ident(word) = self.peek().clone() else {
                return Err(self.unexpected("a privilege"));
            };
            self.advance();
            let w = word.to_ascii_lowercase();
            if w == "all" {
                self.eat_kw("PRIVILEGES");
            }
            // column lists: `SELECT (a, b)`
            if self.peek_sym("(") {
                while !self.peek_sym(")") && !self.at_eof() {
                    self.advance();
                }
                self.expect_sym(")")?;
            }
            privileges.push(w);
            if !self.eat_sym(",") {
                break;
            }
        }
        let (object_kind, objects) = if self.eat_kw("ON") {
            if self.eat_kw("ALL") {
                let kind = self.ident()?.to_ascii_lowercase();
                self.expect_kw("IN")?;
                self.expect_kw("SCHEMA")?;
                (format!("all_{kind}"), self.ident_list()?)
            } else {
                let mut kind = "table".to_string();
                if let Tok::Ident(w) = self.peek().clone() {
                    let up = w.to_ascii_uppercase();
                    if matches!(up.as_str(), "TABLE" | "SEQUENCE" | "SCHEMA" | "DATABASE" | "FUNCTION" | "PROCEDURE" | "ROUTINE" | "DOMAIN" | "TYPE" | "LANGUAGE" | "TABLESPACE") {
                        self.advance();
                        kind = up.to_ascii_lowercase();
                    } else if matches!(up.as_str(), "FOREIGN" | "LARGE") {
                        self.advance();
                        self.advance();
                        kind = "other".into();
                    }
                }
                let mut objects = vec![self.table_ident()?];
                while self.eat_sym(",") {
                    objects.push(self.table_ident()?);
                }
                // a function's argument types: `f(int, text)`
                if self.peek_sym("(") {
                    while !self.peek_sym(")") && !self.at_eof() {
                        self.advance();
                    }
                    self.expect_sym(")")?;
                }
                (kind, objects)
            }
        } else {
            (String::new(), Vec::new())
        };
        if !self.eat_kw("TO") {
            self.expect_kw("FROM")?;
        }
        let mut grantees = Vec::new();
        loop {
            self.eat_kw("GROUP");
            grantees.push(self.role_name()?);
            if !self.eat_sym(",") {
                break;
            }
        }
        let mut cascade = false;
        loop {
            if self.eat_kw("WITH") {
                // WITH GRANT OPTION / WITH ADMIN OPTION / WITH INHERIT ...
                while !self.at_eof() && !self.peek_sym(";") && !self.peek_kw("GRANTED") {
                    self.advance();
                }
            } else if self.eat_kw("GRANTED") {
                self.expect_kw("BY")?;
                self.role_name()?;
            } else if self.eat_kw("CASCADE") {
                cascade = true;
            } else if self.eat_kw("RESTRICT") {
            } else {
                break;
            }
        }
        let membership = object_kind.is_empty();
        Ok(Statement::Grant(Box::new(GrantDef {
            revoke,
            privileges: if membership { Vec::new() } else { privileges.clone() },
            object_kind,
            objects: if membership { privileges } else { objects },
            grantees,
            cascade,
        })))
    }

    /// `CREATE SCHEMA | DOMAIN | TYPE | EXTENSION | ROLE | USER | GROUP`, the objects kept in the catalog.
    fn create_catalog_object(&mut self, guarded: &mut bool) -> Result<Option<Statement>> {
        let change = |kind: &'static str, name: String, value: String| {
            Statement::Catalog(Box::new(CatalogChange { kind, names: vec![name], value: Some(value), replace: false, cascade: false }))
        };
        if self.eat_kw("SCHEMA") {
            self.if_exists_clause(true, guarded)?;
            let name = if self.peek_kw("AUTHORIZATION") { None } else { Some(self.ident()?) };
            let mut owner = None;
            if self.eat_kw("AUTHORIZATION") {
                owner = Some(self.role_name()?);
            }
            let name = name.or(owner).ok_or_else(|| self.unexpected("a schema name"))?;
            if self.peek_kw("CREATE") || self.peek_kw("GRANT") {
                return Err(Error::syntax("CREATE SCHEMA with objects inside is not supported; create them with the schema name in front"));
            }
            return Ok(Some(change("schema", name, String::new())));
        }
        if self.eat_kw("EXTENSION") {
            self.if_exists_clause(true, guarded)?;
            let name = self.ident()?;
            while !self.at_eof() && !self.peek_sym(";") {
                // WITH SCHEMA s VERSION 'x' CASCADE: accepted, ignored
                self.advance();
            }
            return Ok(Some(change("extension", name, String::new())));
        }
        if self.peek_kw("ROLE") || self.peek_kw("USER") || self.peek_kw("GROUP") {
            self.advance();
            self.if_exists_clause(true, guarded)?;
            let name = self.role_name()?;
            let options = self.role_options()?;
            return Ok(Some(change("role", name, Self::options_text(&options))));
        }
        if self.eat_kw("DOMAIN") {
            self.if_exists_clause(true, guarded)?;
            let name = self.ident()?;
            self.eat_kw("AS");
            let type_name = self.type_name()?;
            let mut t = match crate::sql::usertypes::lookup(&type_name) {
                Some(base) => UserType { domain: true, ..base },
                None => {
                    DataType::from_sql_name(&type_name).ok_or_else(|| Error::syntax(format!("unknown data type '{type_name}'")))?;
                    UserType { base: type_name, domain: true, ..Default::default() }
                }
            };
            if self.eat_sym("(") {
                while !self.peek_sym(")") && !self.at_eof() {
                    self.advance();
                }
                self.expect_sym(")")?;
            }
            loop {
                if self.eat_kw("CONSTRAINT") {
                    self.ident()?;
                } else if self.eat_kw("COLLATE") {
                    self.ident()?;
                } else if self.eat_kw("DEFAULT") {
                    let start = self.toks[self.pos].pos;
                    self.expr()?;
                    let end = self.toks[self.pos].pos;
                    t.default = Some(self.src[start..end].trim().to_string());
                } else if self.eat_kw("NOT") {
                    self.expect_kw("NULL")?;
                    t.not_null = true;
                } else if self.eat_kw("NULL") {
                } else if self.peek_kw("CHECK") {
                    t.checks.push(self.check_clause()?);
                } else {
                    break;
                }
            }
            crate::sql::usertypes::register(&name, t.clone());
            let value = serde_json::to_string(&t).map_err(|e| Error::syntax(e.to_string()))?;
            return Ok(Some(change("type", name, value)));
        }
        if self.eat_kw("TYPE") {
            self.if_exists_clause(true, guarded)?;
            let name = self.ident()?;
            if !self.eat_kw("AS") {
                return Err(Error::syntax("only CREATE TYPE name AS ENUM (...) is supported"));
            }
            if !self.eat_kw("ENUM") {
                return Err(Error::syntax("only enumerated types can be created with CREATE TYPE; composite, range and base types are not available"));
            }
            self.expect_sym("(")?;
            let mut labels = Vec::new();
            if !self.peek_sym(")") {
                loop {
                    match self.advance() {
                        Tok::Str(s) => labels.push(s),
                        _ => {
                            self.pos -= 1;
                            return Err(self.unexpected("a label in quotes"));
                        }
                    }
                    if !self.eat_sym(",") {
                        break;
                    }
                }
            }
            self.expect_sym(")")?;
            let t = UserType { base: "TEXT".into(), labels, ..Default::default() };
            crate::sql::usertypes::register(&name, t.clone());
            let value = serde_json::to_string(&t).map_err(|e| Error::syntax(e.to_string()))?;
            return Ok(Some(change("type", name, value)));
        }
        Ok(None)
    }

    fn drop_catalog_object(&mut self, guarded: &mut bool) -> Result<Option<Statement>> {
        let kind: &'static str = if self.eat_kw("SCHEMA") {
            "schema"
        } else if self.eat_kw("DOMAIN") || self.eat_kw("TYPE") {
            "type"
        } else if self.eat_kw("EXTENSION") {
            "extension"
        } else if self.peek_kw("ROLE") || self.peek_kw("USER") || self.peek_kw("GROUP") {
            self.advance();
            "role"
        } else {
            return Ok(None);
        };
        self.if_exists_clause(false, guarded)?;
        let names = if kind == "role" { self.role_list()? } else { self.ident_list()? };
        let cascade = self.eat_kw("CASCADE");
        self.eat_kw("RESTRICT");
        if kind == "type" {
            for n in &names {
                crate::sql::usertypes::unregister(n);
            }
        }
        Ok(Some(Statement::Catalog(Box::new(CatalogChange { kind, names, value: None, replace: false, cascade }))))
    }

    /// `AND [NO] CHAIN` after COMMIT or ROLLBACK: read and ignored.
    fn eat_chain(&mut self) {
        if self.peek_kw("AND") && (self.peek_kw_at(1, "CHAIN") || (self.peek_kw_at(1, "NO") && self.peek_kw_at(2, "CHAIN"))) {
            while !self.eat_kw("CHAIN") {
                self.advance();
            }
        }
    }

    /// Is the statement about a kind of object that only a server has (a tablespace, a publication, a text search
    /// configuration, ...)? Such statements are read and ignored.
    fn ignorable_object(&self) -> bool {
        const ALONE: [&str; 15] = [
            "TABLESPACE", "DATABASE", "PUBLICATION", "SUBSCRIPTION", "SERVER", "FOREIGN", "STATISTICS", "COLLATION", "CONVERSION", "TRANSFORM",
            "LANGUAGE", "OWNED", "PROCEDURAL", "OPERATOR", "CAST",
        ];
        ALONE.iter().any(|w| self.peek_kw(w))
            || (self.peek_kw("USER") && self.peek_kw_at(1, "MAPPING"))
            || (self.peek_kw("EVENT") && self.peek_kw_at(1, "TRIGGER"))
            || (self.peek_kw("TEXT") && self.peek_kw_at(1, "SEARCH"))
            || (self.peek_kw("ACCESS") && self.peek_kw_at(1, "METHOD"))
            || (self.peek_kw("DEFAULT") && self.peek_kw_at(1, "PRIVILEGES"))
            || (self.peek_kw("LARGE") && self.peek_kw_at(1, "OBJECT"))
    }

    fn ignore_rest(&mut self, what: &str) -> Statement {
        while !self.at_eof() && !self.peek_sym(";") {
            self.advance();
        }
        Statement::Ignored(what.to_string())
    }

    /// `FOR VALUES ...` or `DEFAULT` of a partition, as the JSON the catalog keeps.
    fn partition_bound_text(&mut self) -> Result<String> {
        let bound = if self.eat_kw("DEFAULT") {
            serde_json::json!({"kind": "default"})
        } else {
            self.expect_kw("FOR")?;
            self.expect_kw("VALUES")?;
            if self.eat_kw("IN") {
                serde_json::json!({"kind": "list", "values": self.bound_list()?})
            } else if self.eat_kw("FROM") {
                let from = self.bound_list()?;
                self.expect_kw("TO")?;
                let to = self.bound_list()?;
                serde_json::json!({"kind": "range", "from": from, "to": to})
            } else if self.eat_kw("WITH") {
                self.expect_sym("(")?;
                let (mut modulus, mut remainder) = (0i64, 0i64);
                loop {
                    let which = self.ident()?.to_ascii_uppercase();
                    let Tok::Int(n) = self.advance() else {
                        self.pos -= 1;
                        return Err(self.unexpected("a number"));
                    };
                    match which.as_str() {
                        "MODULUS" => modulus = n,
                        "REMAINDER" => remainder = n,
                        _ => return Err(Error::syntax("hash partitions take MODULUS and REMAINDER")),
                    }
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_sym(")")?;
                serde_json::json!({"kind": "hash", "modulus": modulus, "remainder": remainder})
            } else {
                return Err(self.unexpected("IN, FROM or WITH"));
            }
        };
        Ok(bound.to_string())
    }

    /// After `INTO` in a `SELECT`: `INTO @a, @b`.
    fn select_into_tail(&mut self) -> Result<()> {
        if self.peek_kw("INTO") && matches!(self.peek_at(1), Tok::Param(_)) {
            self.advance();
            loop {
                match self.advance() {
                    Tok::Param(n) => self.into_vars.push(n),
                    _ => {
                        self.pos -= 1;
                        return Err(self.unexpected("a variable such as @name"));
                    }
                }
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Does an assignment of `SET` start `n` tokens ahead (`name =`, `@name :=`, `GLOBAL name =`, `@@name =`)?
    fn starts_assignment(&self, n: usize) -> bool {
        let scope = |t: &Tok| matches!(t, Tok::Ident(s) if matches!(s.to_ascii_uppercase().as_str(), "GLOBAL" | "SESSION" | "LOCAL" | "PERSIST" | "PERSIST_ONLY"));
        let at = if scope(self.peek_at(n)) { n + 1 } else { n };
        let mut k = at;
        // name, or a dotted name
        match self.peek_at(k) {
            Tok::Param(_) | Tok::Ident(_) | Tok::Quoted(_) => k += 1,
            _ => return false,
        }
        while matches!(self.peek_at(k), Tok::Sym(".")) && matches!(self.peek_at(k + 1), Tok::Ident(_) | Tok::Quoted(_)) {
            k += 2;
        }
        matches!(self.peek_at(k), Tok::Sym("=") | Tok::Sym(":="))
    }

    /// Everything after `SET`: PostgreSQL and MySQL forms. Several assignments joined by commas are one batch.
    fn set_statement(&mut self) -> Result<Statement> {
        let mut out = Vec::new();
        loop {
            let mut local = false;
            let mut scoped = false;
            if self.eat_kw("LOCAL") {
                local = true;
                scoped = true;
            } else if self.peek_kw("SESSION") && !self.peek_kw_at(1, "AUTHORIZATION") && !self.peek_kw_at(1, "CHARACTERISTICS") {
                self.advance();
                scoped = true;
            } else if self.eat_kw("GLOBAL") || self.eat_kw("PERSIST") || self.eat_kw("PERSIST_ONLY") {
                scoped = true;
            }
            let _ = scoped;
            if out.is_empty() {
                if self.peek_kw("ROLE") || self.peek_kw("AUTHORIZATION") || (self.peek_kw("SESSION") && self.peek_kw_at(1, "AUTHORIZATION")) {
                    self.eat_kw("SESSION");
                    self.advance();
                    return Ok(Statement::SetRole(if self.eat_kw("NONE") || self.eat_kw("DEFAULT") { None } else { Some(self.role_name()?) }));
                }
                if self.peek_kw("TRANSACTION") || self.peek_kw("CHARACTERISTICS") || (self.peek_kw("SESSION") && self.peek_kw_at(1, "CHARACTERISTICS")) {
                    // `SET TRANSACTION ISOLATION LEVEL ...`, `SET SESSION CHARACTERISTICS AS ...`: there is one isolation level
                    self.eat_kw("SESSION");
                    let what = self.ident()?.to_ascii_lowercase();
                    while !matches!(self.peek(), Tok::Eof) && !self.peek_sym(";") {
                        self.advance();
                    }
                    return Ok(Statement::SetParameter { name: what, value: Some(String::new()), local });
                }
                if self.peek_kw("NAMES") || (self.peek_kw("CHARACTER") && self.peek_kw_at(1, "SET")) || self.peek_kw("CHARSET") {
                    // SET NAMES utf8 COLLATE x, SET CHARACTER SET x: text is always UTF-8 here
                    return Ok(self.ignore_rest("SET NAMES"));
                }
            }
            match self.peek().clone() {
                Tok::Param(p) if !p.starts_with("@@") => {
                    // a user variable
                    self.advance();
                    if !self.eat_sym(":=") {
                        self.expect_sym("=")?;
                    }
                    let expr = self.expr()?;
                    out.push(Statement::SetVar { name: p, expr });
                }
                _ => {
                    let name = match self.peek().clone() {
                        Tok::Param(p) => {
                            self.advance();
                            p[2..].to_ascii_lowercase()
                        }
                        _ => self.parameter_name()?,
                    };
                    if !self.eat_kw("TO") && !self.eat_sym(":=") {
                        self.expect_sym("=")?;
                    }
                    if self.eat_kw("DEFAULT") {
                        out.push(Statement::SetParameter { name, value: None, local });
                    } else {
                        // plain words and numbers are kept as they are; anything more is an expression to evaluate
                        let save = self.pos;
                        let mut parts: Vec<String> = Vec::new();
                        let mut simple = true;
                        loop {
                            let negative = self.eat_sym("-");
                            let part = match self.advance() {
                                Tok::Str(s) | Tok::Ident(s) | Tok::Quoted(s) => s,
                                Tok::Int(i) => i.to_string(),
                                Tok::Dec(d) => d.to_string(),
                                Tok::Float(f) => f.to_string(),
                                _ => {
                                    simple = false;
                                    break;
                                }
                            };
                            parts.push(if negative { format!("-{part}") } else { part });
                            if self.peek_sym(",") && !self.starts_assignment(1) {
                                self.advance();
                                continue;
                            }
                            break;
                        }
                        let ends = self.at_eof() || self.peek_sym(";") || self.peek_sym(",");
                        if simple && ends {
                            out.push(Statement::SetParameter { name, value: Some(parts.join(", ")), local });
                        } else {
                            self.pos = save;
                            let expr = self.expr()?;
                            out.push(Statement::SetParameterExpr { name, expr });
                        }
                    }
                }
            }
            if self.peek_sym(",") && self.starts_assignment(1) {
                self.advance();
                continue;
            }
            break;
        }
        Ok(if out.len() == 1 { out.remove(0) } else { Statement::Batch(out) })
    }

    /// MySQL statements that are not SQL of the standard: `SHOW`, `USE`, `DESCRIBE`, `PREPARE ... FROM`,
    /// `EXECUTE ... USING`, and the maintenance commands.
    fn mysql_statement(&mut self) -> Result<Option<Statement>> {
        if self.peek_kw("USE") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Quoted(_)) {
            return Ok(Some(self.ignore_rest("USE")));
        }
        for w in ["FLUSH", "OPTIMIZE", "REPAIR", "PURGE", "KILL", "CHANGE", "XA", "INSTALL", "UNINSTALL", "BINLOG", "CACHE", "LOAD", "SIGNAL", "RESIGNAL", "GET", "HELP"] {
            if self.peek_kw(w) && !(w == "LOAD" && !self.peek_kw_at(1, "INDEX") && !self.peek_kw_at(1, "DATA")) && !(w == "GET" && !self.peek_kw_at(1, "DIAGNOSTICS")) {
                return Ok(Some(self.ignore_rest(w)));
            }
        }
        if self.peek_kw("CHECK") && self.peek_kw_at(1, "TABLE") {
            return Ok(Some(self.ignore_rest("CHECK TABLE")));
        }
        if self.peek_kw("CALL") && matches!(self.peek_at(1), Tok::Ident(s) if s.eq_ignore_ascii_case("mtr")) {
            return Ok(Some(self.ignore_rest("CALL")));
        }
        if (self.peek_kw("DESCRIBE") || self.peek_kw("DESC")) && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Quoted(_)) {
            self.advance();
            let target = Some(self.table_ident()?);
            let mut show = ShowStmt { what: "columns".into(), target, ..ShowStmt::default() };
            if let Tok::Ident(_) | Tok::Quoted(_) | Tok::Str(_) = self.peek() {
                // DESCRIBE t col / 'pattern': a column to show
                if !self.at_eof() && !self.peek_sym(";") {
                    show.like = Some(match self.advance() {
                        Tok::Ident(s) | Tok::Quoted(s) | Tok::Str(s) => s,
                        _ => String::new(),
                    });
                }
            }
            return Ok(Some(Statement::Show(Box::new(show))));
        }
        if self.peek_kw("PREPARE") && self.peek_kw_at(2, "FROM") {
            self.advance();
            let name = self.ident()?;
            self.advance();
            let source = self.expr()?;
            return Ok(Some(Statement::PrepareText { name, source }));
        }
        if self.peek_kw("EXECUTE") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Quoted(_)) && self.peek_kw_at(2, "USING") {
            self.advance();
            let name = self.ident()?;
            self.advance();
            let mut args = Vec::new();
            loop {
                let e = self.expr()?;
                args.push((Some(format!("?{}", args.len() + 1)), e));
                if !self.eat_sym(",") {
                    break;
                }
            }
            return Ok(Some(Statement::Exec { name, args }));
        }
        if self.peek_kw("SHOW") {
            return self.show_statement();
        }
        Ok(None)
    }

    /// MySQL's `SHOW ...`; `None` (nothing consumed) for the PostgreSQL forms `SHOW name`, `SHOW ALL`.
    fn show_statement(&mut self) -> Result<Option<Statement>> {
        let save = self.pos;
        self.advance();
        // GLOBAL / SESSION before VARIABLES and STATUS
        if self.peek_kw("GLOBAL") || self.peek_kw("SESSION") || self.peek_kw("LOCAL") {
            if self.peek_kw_at(1, "VARIABLES") || self.peek_kw_at(1, "STATUS") {
                self.advance();
            }
        }
        let full = self.eat_kw("FULL");
        let mut show = ShowStmt { full, ..ShowStmt::default() };
        let word = match self.peek().clone() {
            Tok::Ident(w) => w.to_ascii_uppercase(),
            _ => {
                self.pos = save;
                return Ok(None);
            }
        };
        let simple = match word.as_str() {
            "TABLES" => "tables",
            "DATABASES" | "SCHEMAS" => "databases",
            "COLUMNS" | "FIELDS" => "columns",
            "INDEX" | "INDEXES" | "KEYS" => "index",
            "VARIABLES" => "variables",
            "STATUS" => "status",
            "WARNINGS" => "warnings",
            "ERRORS" => "errors",
            "ENGINES" => "engines",
            "COLLATION" => "collation",
            "CHARSET" => "character set",
            "PROCESSLIST" | "GRANTS" | "PRIVILEGES" | "PLUGINS" | "TRIGGERS" | "EVENTS" | "PROFILES" | "PROFILE" | "REPLICAS" | "REPLICA" | "SLAVE" | "MASTER" | "BINARY" | "BINLOG"
            | "RELAYLOG" | "OPEN" | "FUNCTION" | "PROCEDURE" | "ENGINE" | "STORAGE" | "COUNT" | "PARSE_TREE" => "other",
            "CHARACTER" if self.peek_kw_at(1, "SET") => "character set",
            "TABLE" if self.peek_kw_at(1, "STATUS") => "table status",
            "CREATE" => "create",
            _ => {
                self.pos = save;
                return Ok(None);
            }
        };
        show.what = simple.to_string();
        match simple {
            "other" => {
                let w = word.to_ascii_lowercase();
                while !self.at_eof() && !self.peek_sym(";") {
                    self.advance();
                }
                show.what = format!("other {w}");
            }
            "create" => {
                self.advance();
                let kind = self.role_name()?.to_ascii_lowercase();
                show.what = format!("create {kind}");
                show.target = Some(self.table_ident()?);
            }
            "character set" => {
                self.advance();
                if word == "CHARACTER" {
                    self.advance();
                }
                self.show_filters(&mut show)?;
            }
            "table status" => {
                self.advance();
                self.advance();
                self.show_from(&mut show)?;
                self.show_filters(&mut show)?;
            }
            "columns" | "index" => {
                self.advance();
                if !self.eat_kw("FROM") && !self.eat_kw("IN") {
                    return Err(self.unexpected("FROM"));
                }
                let t = self.table_ident()?;
                show.target = Some(t);
                self.show_from(&mut show)?;
                self.show_filters(&mut show)?;
            }
            "warnings" | "errors" => {
                self.advance();
                if self.eat_kw("LIMIT") {
                    self.usize_literal()?;
                    if self.eat_sym(",") {
                        self.usize_literal()?;
                    }
                }
            }
            _ => {
                self.advance();
                if simple == "tables" || simple == "databases" {
                    self.show_from(&mut show)?;
                }
                self.show_filters(&mut show)?;
            }
        }
        Ok(Some(Statement::Show(Box::new(show))))
    }

    /// `FROM db` / `IN db`: a schema, which qualifies the table asked about.
    fn show_from(&mut self, show: &mut ShowStmt) -> Result<()> {
        if self.eat_kw("FROM") || self.eat_kw("IN") {
            let db = self.ident()?;
            if !db.eq_ignore_ascii_case("test") && !db.eq_ignore_ascii_case("public") {
                match &mut show.target {
                    Some(t) if !t.contains('.') => *t = format!("{db}.{t}"),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn show_filters(&mut self, show: &mut ShowStmt) -> Result<()> {
        if self.eat_kw("LIKE") {
            match self.advance() {
                Tok::Str(s) => show.like = Some(s),
                _ => {
                    self.pos -= 1;
                    return Err(self.unexpected("a pattern in quotes"));
                }
            }
        } else if self.eat_kw("WHERE") {
            show.filter = Some(self.expr()?);
        }
        Ok(())
    }

    /// A default that is not a constant (`CURRENT_TIMESTAMP`, `NOW()`, `(expression)`) is read and not applied.
    fn skip_default_expr(&mut self) -> Result<()> {
        if self.peek_sym("(") {
            return self.skip_parens();
        }
        self.advance();
        if self.peek_sym("(") {
            self.skip_parens()?;
        }
        Ok(())
    }

    /// `(col [(length)] [ASC | DESC], ...)` of a key.
    fn key_columns(&mut self) -> Result<(Vec<String>, Vec<bool>)> {
        self.expect_sym("(")?;
        let (mut cols, mut desc) = (Vec::new(), Vec::new());
        loop {
            if self.peek_sym("(") {
                return Err(Error::syntax("an index on an expression is not supported; index the column"));
            }
            cols.push(self.ident()?);
            if self.peek_sym("(") {
                self.skip_parens()?; // a prefix length
            }
            desc.push(self.index_direction());
            if !self.eat_sym(",") {
                break;
            }
        }
        self.expect_sym(")")?;
        Ok((cols, desc))
    }

    /// Is the next table element a key (`KEY`, `INDEX`, `UNIQUE ...`, `PRIMARY KEY`, `FULLTEXT`, `CONSTRAINT n PRIMARY KEY`)?
    fn is_key_element(&self) -> bool {
        let mut k = 0;
        if self.peek_kw("CONSTRAINT") {
            k = 1;
            if !(self.peek_kw_at(1, "PRIMARY") || self.peek_kw_at(1, "UNIQUE") || self.peek_kw_at(1, "FOREIGN") || self.peek_kw_at(1, "CHECK")) {
                k = 2;
            }
        }
        (self.peek_kw_at(k, "PRIMARY") && self.peek_kw_at(k + 1, "KEY"))
            || self.peek_kw_at(k, "UNIQUE")
            || ((self.peek_kw_at(k, "KEY") || self.peek_kw_at(k, "INDEX")) && !matches!(self.peek_at(k + 1), Tok::Sym(",") | Tok::Sym(")")) && k == 0)
            || ((self.peek_kw_at(k, "FULLTEXT") || self.peek_kw_at(k, "SPATIAL")) && (self.peek_kw_at(k + 1, "KEY") || self.peek_kw_at(k + 1, "INDEX")))
    }

    fn key_element(&mut self, primary_key: &mut Vec<String>, uniques: &mut Vec<Vec<String>>, indexes: &mut Vec<IndexSpec>) -> Result<()> {
        if self.eat_kw("CONSTRAINT") && !(self.peek_kw("PRIMARY") || self.peek_kw("UNIQUE")) {
            self.ident()?;
        }
        let primary = self.eat_kw("PRIMARY");
        let unique = !primary && self.eat_kw("UNIQUE");
        if !primary && !unique {
            self.eat_kw("FULLTEXT");
            self.eat_kw("SPATIAL");
        }
        if !self.eat_kw("KEY") {
            self.eat_kw("INDEX");
        }
        let name = if matches!(self.peek(), Tok::Ident(_) | Tok::Quoted(_)) && !self.peek_kw("USING") { Some(self.ident()?) } else { None };
        if self.eat_kw("USING") {
            self.ident()?;
        }
        let (cols, desc) = self.key_columns()?;
        // USING BTREE, COMMENT 'x', KEY_BLOCK_SIZE = n, VISIBLE, WITH PARSER p
        while self.eat_kw("USING") || self.eat_kw("COMMENT") || self.eat_kw("KEY_BLOCK_SIZE") || self.eat_kw("VISIBLE") || self.eat_kw("INVISIBLE") || (self.peek_kw("WITH") && self.peek_kw_at(1, "PARSER") && self.eat_kw("WITH") && self.eat_kw("PARSER")) {
            self.eat_sym("=");
            if !matches!(self.peek(), Tok::Sym(",") | Tok::Sym(")")) {
                self.advance();
            }
        }
        if primary {
            *primary_key = cols;
        } else if unique {
            if name.is_some() || desc.iter().any(|d| *d) {
                indexes.push((name, cols, desc, true));
            } else {
                uniques.push(cols);
            }
        } else {
            indexes.push((name, cols, desc, false));
        }
        Ok(())
    }

    /// One MySQL table option after `)`: `ENGINE = InnoDB`, `DEFAULT CHARSET = utf8mb4`, `AUTO_INCREMENT = 5`, ...
    fn table_option(&mut self) -> Result<bool> {
        let Tok::Ident(w) = self.peek().clone() else { return Ok(false) };
        let up = w.to_ascii_uppercase();
        const OPTIONS: [&str; 33] = [
            "ENGINE", "CHARSET", "CHARACTER", "COLLATE", "AUTO_INCREMENT", "COMMENT", "ROW_FORMAT", "KEY_BLOCK_SIZE", "MAX_ROWS", "MIN_ROWS", "PACK_KEYS",
            "CHECKSUM", "TABLE_CHECKSUM", "DELAY_KEY_WRITE", "STATS_AUTO_RECALC", "STATS_PERSISTENT", "STATS_SAMPLE_PAGES", "INSERT_METHOD", "UNION",
            "CONNECTION", "PASSWORD", "COMPRESSION", "ENCRYPTION", "SECONDARY_ENGINE", "SECONDARY_ENGINE_ATTRIBUTE", "ENGINE_ATTRIBUTE", "STORAGE", "DEFAULT",
            "DATA", "INDEX", "AVG_ROW_LENGTH", "AUTOEXTEND_SIZE", "START",
        ];
        if !OPTIONS.contains(&up.as_str()) {
            return Ok(false);
        }
        if up == "DEFAULT" {
            self.advance();
            return Ok(true);
        }
        if (up == "DATA" || up == "INDEX") && !self.peek_kw_at(1, "DIRECTORY") {
            return Ok(false);
        }
        if up == "START" {
            return Ok(false);
        }
        self.advance();
        if up == "CHARACTER" {
            self.expect_kw("SET")?;
        }
        if up == "DATA" || up == "INDEX" {
            self.advance(); // DIRECTORY
        }
        self.eat_sym("=");
        if self.peek_sym("(") {
            self.skip_parens()?;
        } else {
            self.advance();
        }
        Ok(true)
    }

    /// The MySQL forms of `ALTER TABLE` actions; `None` (nothing consumed) when it is not one of them.
    fn mysql_alter_action(&mut self, table: &str) -> Result<Option<Statement>> {
        let table = table.to_string();
        // table options and maintenance: read and ignored
        const IGNORED: [&str; 31] = [
            "ENGINE", "CONVERT", "CHARACTER", "CHARSET", "COLLATE", "AUTO_INCREMENT", "COMMENT", "ROW_FORMAT", "ALGORITHM", "LOCK", "KEY_BLOCK_SIZE",
            "STATS_AUTO_RECALC", "STATS_PERSISTENT", "STATS_SAMPLE_PAGES", "PACK_KEYS", "CHECKSUM", "MAX_ROWS", "MIN_ROWS", "DELAY_KEY_WRITE", "ORDER",
            "WITH", "WITHOUT", "ANALYZE", "OPTIMIZE", "REBUILD", "REORGANIZE", "EXCHANGE", "DISCARD", "IMPORT", "COALESCE", "REMOVE",
        ];
        let first = match self.peek() {
            Tok::Ident(w) => w.to_ascii_uppercase(),
            _ => return Ok(None),
        };
        let ignored = IGNORED.contains(&first.as_str()) || (first == "DEFAULT" && (self.peek_kw_at(1, "CHARACTER") || self.peek_kw_at(1, "CHARSET") || self.peek_kw_at(1, "COLLATE")));
        if ignored {
            let mut depth = 0;
            while !self.at_eof() && !self.peek_sym(";") && !(depth == 0 && self.peek_sym(",")) {
                match self.advance() {
                    Tok::Sym("(") => depth += 1,
                    Tok::Sym(")") => depth -= 1,
                    _ => {}
                }
            }
            return Ok(Some(Statement::Ignored("ALTER TABLE".into())));
        }
        let position = |p: &mut Self| {
            // FIRST | AFTER column
            if p.eat_kw("FIRST") {
            } else if p.eat_kw("AFTER") {
                let _ = p.ident();
            }
        };
        if self.peek_kw("ADD") {
            // ADD [COLUMN] (def, def, ...)
            let k = if self.peek_kw_at(1, "COLUMN") { 2 } else { 1 };
            if matches!(self.peek_at(k), Tok::Sym("(")) {
                self.advance();
                if k == 2 {
                    self.advance();
                }
                self.advance();
                let mut all = Vec::new();
                loop {
                    all.push(Statement::AlterTable { table: table.clone(), action: AlterAction::AddColumn(self.column_def()?) });
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_sym(")")?;
                return Ok(Some(Statement::Batch(all)));
            }
            // ADD [CONSTRAINT [n]] PRIMARY KEY | UNIQUE [KEY] | [FULLTEXT | SPATIAL] KEY | INDEX
            let mut look = 1;
            if self.peek_kw_at(1, "CONSTRAINT") {
                look = if self.peek_kw_at(2, "PRIMARY") || self.peek_kw_at(2, "UNIQUE") { 2 } else { 3 };
            }
            let is_key = self.peek_kw_at(look, "PRIMARY") && self.peek_kw_at(look + 1, "KEY")
                || self.peek_kw_at(look, "UNIQUE")
                || self.peek_kw_at(look, "KEY")
                || self.peek_kw_at(look, "INDEX")
                || self.peek_kw_at(look, "FULLTEXT")
                || self.peek_kw_at(look, "SPATIAL");
            if is_key {
                self.advance();
                let (mut pk, mut uniques, mut indexes) = (Vec::new(), Vec::new(), Vec::new());
                let primary = self.peek_kw("PRIMARY") || (self.peek_kw("CONSTRAINT") && (self.peek_kw_at(1, "PRIMARY") || self.peek_kw_at(2, "PRIMARY")));
                let unique = self.peek_kw("UNIQUE") || (self.peek_kw("CONSTRAINT") && (self.peek_kw_at(1, "UNIQUE") || self.peek_kw_at(2, "UNIQUE")));
                let before = self.pos;
                self.key_element(&mut pk, &mut uniques, &mut indexes)?;
                let _ = before;
                let (name, columns, descending, is_unique) = if primary {
                    (Some(format!("{}_pkey", table.replace('.', "_"))), pk.clone(), Vec::new(), true)
                } else if let Some(u) = uniques.pop() {
                    (None, u, Vec::new(), true)
                } else if let Some((n, c, d, u)) = indexes.pop() {
                    (n, c, d, u)
                } else {
                    return Err(Error::syntax("expected a key definition"));
                };
                let _ = unique;
                let name = name.unwrap_or_else(|| format!("{}_{}_idx", table.replace('.', "_"), columns.join("_")));
                return Ok(Some(Statement::CreateIndex { name, table, columns, descending, unique: is_unique }));
            }
        }
        if self.peek_kw("DROP") && (self.peek_kw_at(1, "KEY") || self.peek_kw_at(1, "INDEX") || (self.peek_kw_at(1, "PRIMARY") && self.peek_kw_at(2, "KEY"))) {
            self.advance();
            if self.eat_kw("PRIMARY") {
                self.advance();
                return Ok(Some(Statement::DropIndex { name: format!("{}_pkey", table.replace('.', "_")), table: Some(table) }));
            }
            self.advance();
            let name = self.ident()?;
            return Ok(Some(Statement::DropIndex { name, table: Some(table) }));
        }
        if self.peek_kw("DROP") && self.peek_kw_at(1, "FOREIGN") && self.peek_kw_at(2, "KEY") {
            self.advance();
            self.advance();
            self.advance();
            return Ok(Some(Statement::AlterTable { table, action: AlterAction::DropConstraint(self.ident()?) }));
        }
        if self.peek_kw("MODIFY") || self.peek_kw("CHANGE") {
            let change = self.peek_kw("CHANGE");
            self.advance();
            self.eat_kw("COLUMN");
            let old = self.ident()?;
            let mut def = self.column_def_after_name(change, &old)?;
            position(self);
            let mut all = Vec::new();
            if change && !def.name.eq_ignore_ascii_case(&old) {
                all.push(Statement::AlterTable { table: table.clone(), action: AlterAction::RenameColumn { from: old.clone(), to: def.name.clone() } });
            }
            def.name = if change { def.name } else { old };
            all.push(Statement::AlterTable { table: table.clone(), action: AlterAction::AlterColumn { name: def.name.clone(), data_type: def.data_type } });
            all.push(Statement::AlterTable { table, action: AlterAction::SetNotNull { name: def.name.clone(), not_null: def.not_null } });
            return Ok(Some(Statement::Batch(all)));
        }
        if self.peek_kw("RENAME") && !self.peek_kw_at(1, "COLUMN") && !self.peek_kw_at(1, "KEY") && !self.peek_kw_at(1, "INDEX") && !self.peek_kw_at(1, "TO") {
            self.advance();
            self.eat_kw("AS");
            return Ok(Some(Statement::AlterTable { table, action: AlterAction::RenameTable(self.ident()?) }));
        }
        Ok(None)
    }

    /// The definition of a column after its name has been read (for `MODIFY`, and `CHANGE` which names it again).
    fn column_def_after_name(&mut self, change: bool, old: &str) -> Result<ColumnDef> {
        if change {
            return self.column_def();
        }
        // `MODIFY name type ...`: put the name back in front by reading as a definition of `old`
        let at = self.pos;
        self.toks.insert(at, Token { tok: Tok::Quoted(old.to_string()), pos: self.toks[at].pos });
        self.column_def()
    }

    fn peek_is_alias(&self) -> bool {
        match self.peek() {
            Tok::Quoted(_) => true,
            // `FROM t WINDOW w AS (...)`: WINDOW starts a clause here, not an alias.
            Tok::Ident(s) if s.eq_ignore_ascii_case("WINDOW") && matches!(self.peek_at(1), Tok::Ident(_)) && self.peek_kw_at(2, "AS") => false,
            // `CREATE TABLE t AS SELECT ... FROM s WITH NO DATA`
            Tok::Ident(s) if s.eq_ignore_ascii_case("WITH") && self.peek_kw_at(1, "NO") => false,
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
        let mut st = self.statement_inner()?;
        if matches!(st, Statement::Insert { .. }) && self.peek_kw("ON") && self.peek_kw_at(1, "CONFLICT") {
            self.advance();
            self.advance();
            let mut target = Vec::new();
            if self.eat_sym("(") {
                target = self.ident_list()?;
                self.expect_sym(")")?;
            }
            self.expect_kw("DO")?;
            let action = if self.eat_kw("NOTHING") {
                ConflictAction::Nothing
            } else {
                self.expect_kw("UPDATE")?;
                self.expect_kw("SET")?;
                let mut sets = Vec::new();
                loop {
                    let col = self.column_name()?;
                    self.expect_sym("=")?;
                    sets.push((col, self.expr()?));
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                let filter = if self.eat_kw("WHERE") { Some(self.expr()?) } else { None };
                ConflictAction::Update { sets, filter }
            };
            st = Statement::Upsert { insert: Box::new(st), conflict: Conflict { target, action } };
        }
        if matches!(st, Statement::Insert { .. }) && self.peek_kw("ON") && self.peek_kw_at(1, "DUPLICATE") {
            // ON DUPLICATE KEY UPDATE a = VALUES(a) + 1: ON CONFLICT DO UPDATE, VALUES(a) being the new row's a
            self.advance();
            self.advance();
            self.expect_kw("KEY")?;
            self.expect_kw("UPDATE")?;
            let mut sets = Vec::new();
            loop {
                let col = self.column_name()?;
                self.expect_sym("=")?;
                let mut e = self.expr()?;
                let _ = crate::sql::walk::walk_expr(&mut e, &mut |x| {
                    if let Expr::Function { name, args, .. } = x {
                        if name.eq_ignore_ascii_case("VALUES") {
                            if let [Expr::Column { name: c, .. }] = args.as_slice() {
                                *x = Expr::Column { table: Some("EXCLUDED".into()), name: c.clone() };
                            }
                        }
                    }
                    Ok(())
                });
                sets.push((col, e));
                if !self.eat_sym(",") {
                    break;
                }
            }
            st = Statement::Upsert { insert: Box::new(st), conflict: Conflict { target: Vec::new(), action: ConflictAction::Update { sets, filter: None } } };
        }
        if self.peek_kw("RETURNING")
            && matches!(st, Statement::Insert { .. } | Statement::Update { .. } | Statement::Delete { .. } | Statement::Merge(_) | Statement::Upsert { .. })
        {
            self.advance();
            let mut items = vec![self.select_item()?];
            while self.eat_sym(",") {
                items.push(self.select_item()?);
            }
            return Ok(Statement::Returning { stmt: Box::new(st), items });
        }
        Ok(st)
    }

    fn statement_inner(&mut self) -> Result<Statement> {
        for word in ["VACUUM", "ANALYZE", "ANALYSE", "LOCK", "COMMENT", "CHECKPOINT", "DISCARD", "CLUSTER", "LISTEN", "UNLISTEN", "NOTIFY", "SECURITY"] {
            if self.peek_kw(word) {
                // maintenance commands for a server: there is nothing to do, so they are read and ignored
                while !self.at_eof() && !self.peek_sym(";") {
                    self.advance();
                }
                return Ok(Statement::Ignored(word.to_string()));
            }
        }
        if self.peek_kw("SELECT") || self.peek_kw("WITH") || self.peek_kw("VALUES") || self.peek_kw("TRANSFORM") || self.peek_sym("(") {
            self.into_vars.clear();
            let select = self.select()?;
            self.select_into_tail()?;
            if !self.into_vars.is_empty() {
                return Ok(Statement::SelectInto { select: Box::new(select), vars: std::mem::take(&mut self.into_vars) });
            }
            return Ok(Statement::Select(select));
        }
        if self.eat_kw("INSERT") {
            // MySQL: INSERT [LOW_PRIORITY | DELAYED | HIGH_PRIORITY] [IGNORE] [INTO]
            let mut ignore = false;
            loop {
                if self.eat_kw("LOW_PRIORITY") || self.eat_kw("DELAYED") || self.eat_kw("HIGH_PRIORITY") {
                } else if self.eat_kw("IGNORE") {
                    ignore = true;
                } else {
                    break;
                }
            }
            // SQLite spellings: INSERT OR REPLACE / INSERT OR IGNORE.
            let action = if ignore {
                Some(ConflictAction::Nothing)
            } else if self.eat_kw("OR") {
                if self.eat_kw("REPLACE") {
                    Some(ConflictAction::Replace)
                } else {
                    self.expect_kw("IGNORE")?;
                    Some(ConflictAction::Nothing)
                }
            } else {
                None
            };
            let st = self.insert()?;
            return Ok(match action {
                Some(action) => Statement::Upsert { insert: Box::new(st), conflict: Conflict { target: Vec::new(), action } },
                None => st,
            });
        }
        if self.peek_kw("REPLACE") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Quoted(_)) {
            self.advance();
            let st = self.insert()?;
            return Ok(Statement::Upsert { insert: Box::new(st), conflict: Conflict { target: Vec::new(), action: ConflictAction::Replace } });
        }
        if self.eat_kw("MERGE") {
            return self.merge();
        }
        if self.eat_kw("EXPLAIN") {
            // `EXPLAIN QUERY PLAN` is accepted as a synonym.
            if self.eat_kw("QUERY") {
                self.expect_kw("PLAN")?;
            }
            // `EXPLAIN [ANALYZE] [VERBOSE] [(COSTS OFF, FORMAT TEXT, ...)] statement`. A query is run and its plan
            // shown with the rows it really produced, whatever the options say (they are accepted so that scripts
            // written for other systems run); a statement that is not a query is described and not run.
            let mut analyze = self.eat_kw("ANALYZE") || self.eat_kw("ANALYSE");
            self.eat_kw("VERBOSE");
            if self.peek_sym("(") {
                self.advance();
                loop {
                    let option = self.ident()?.to_ascii_uppercase();
                    let value = match self.peek().clone() {
                        Tok::Sym(",") | Tok::Sym(")") => None,
                        _ => {
                            let v = match self.advance() {
                                Tok::Ident(s) | Tok::Str(s) => s.to_ascii_uppercase(),
                                Tok::Int(i) => i.to_string(),
                                _ => return Err(Error::syntax("an EXPLAIN option takes a word or a number")),
                            };
                            Some(v)
                        }
                    };
                    const KNOWN: [&str; 14] = [
                        "ANALYZE", "ANALYSE", "VERBOSE", "COSTS", "SETTINGS", "GENERIC_PLAN", "BUFFERS", "SERIALIZE", "WAL", "TIMING", "SUMMARY",
                        "MEMORY", "FORMAT", "TRIGGERS",
                    ];
                    if !KNOWN.contains(&option.as_str()) {
                        return Err(Error::syntax(format!("unrecognized EXPLAIN option '{option}'")));
                    }
                    if (option == "ANALYZE" || option == "ANALYSE") && !matches!(value.as_deref(), Some("OFF" | "FALSE" | "0")) {
                        analyze = true;
                    }
                    if option == "FORMAT" && value.as_deref() != Some("TEXT") {
                        return Err(Error::syntax("only EXPLAIN (FORMAT TEXT) is available"));
                    }
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_sym(")")?;
            }
            const OTHER: [&str; 12] = ["INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE", "EXEC", "CALL", "CREATE", "DROP", "ALTER", "TRUNCATE", "EXECUTE"];
            if OTHER.iter().any(|k| self.peek_kw(k)) {
                return Ok(Statement::ExplainOther(Box::new(self.statement()?)));
            }
            let sel = Box::new(self.select()?);
            return Ok(if analyze { Statement::ExplainAnalyze(sel) } else { Statement::Explain(sel) });
        }
        if self.peek_kw("SET") && !self.peek_kw_at(1, "TIME") && !self.peek_kw_at(1, "TIMEZONE") && !self.peek_kw_at(1, "CONSTRAINTS") {
            self.advance();
            return self.set_statement();
        }
        if let Some(st) = self.mysql_statement()? {
            return Ok(st);
        }
        if self.eat_kw("RESET") {
            if self.eat_kw("SESSION") {
                self.expect_kw("AUTHORIZATION")?;
                return Ok(Statement::SetRole(None));
            }
            if self.eat_kw("ALL") {
                return Ok(Statement::ResetParameter(None));
            }
            let name = if self.eat_kw("TIME") { self.expect_kw("ZONE")?; "timezone".to_string() } else { self.parameter_name()? };
            return Ok(Statement::ResetParameter(Some(name)));
        }
        if self.eat_kw("SHOW") {
            if self.eat_kw("ALL") {
                return Ok(Statement::ShowParameter(None));
            }
            let name = if self.eat_kw("TIME") { self.expect_kw("ZONE")?; "timezone".to_string() } else { self.parameter_name()? };
            return Ok(Statement::ShowParameter(Some(name)));
        }
        if self.peek_kw("SET") && self.peek_kw_at(1, "CONSTRAINTS") {
            self.advance();
            self.advance();
            let names = if self.eat_kw("ALL") { None } else { Some(self.ident_list()?) };
            let deferred = if self.eat_kw("DEFERRED") {
                true
            } else {
                self.expect_kw("IMMEDIATE")?;
                false
            };
            return Ok(Statement::SetConstraints { names, deferred });
        }
        if self.eat_kw("SET") {
            // SET TIME ZONE 'x' | SET TIMEZONE TO 'x'
            if !self.eat_kw("TIMEZONE") {
                self.expect_kw("TIME")?;
                self.expect_kw("ZONE")?;
            }
            if !self.eat_kw("TO") {
                self.eat_sym("=");
            }
            let name = match self.advance() {
                Tok::Str(s) => s,
                Tok::Ident(s) => s,
                _ => {
                    self.pos = self.pos.saturating_sub(1);
                    return Err(self.unexpected("a time zone name in quotes, or LOCAL"));
                }
            };
            return Ok(Statement::SetTimeZone(name));
        }
        if self.eat_kw("REINDEX") {
            let name = if matches!(self.peek(), Tok::Eof) || self.peek_sym(";") { None } else { Some(self.ident()?) };
            return Ok(Statement::Reindex(name));
        }
        if self.eat_kw("TRUNCATE") {
            self.eat_kw("TABLE");
            return Ok(Statement::Truncate(self.table_ident()?));
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
        if self.peek_kw("GRANT") || self.peek_kw("REVOKE") {
            return self.grant();
        }
        if self.eat_kw("PARAMETERS") {
            return Ok(Statement::Parameters(self.param_list()?));
        }
        if self.eat_kw("EXEC") || self.eat_kw("EXECUTE") || self.eat_kw("CALL") {
            return self.exec();
        }
        if self.peek_kw("RUN") && self.peek_kw_at(1, "REPORT") {
            self.advance();
            self.advance();
            let name = self.ident()?;
            let format = if self.eat_kw("AS") { self.ident()? } else { "text".to_string() };
            return Ok(Statement::RunReport { name, format });
        }
        if self.eat_kw("PREPARE") {
            let name = self.ident()?;
            self.expect_kw("AS")?;
            let statement = Box::new(self.statement()?);
            return Ok(Statement::Prepare { name, statement });
        }
        if self.eat_kw("DEALLOCATE") {
            self.eat_kw("PREPARE");
            let name = if self.eat_kw("ALL") { "all".to_string() } else { self.ident()? };
            return Ok(Statement::Deallocate(name));
        }
        if self.eat_kw("RAISE") {
            return Ok(Statement::Raise(self.expr()?));
        }
        if self.eat_kw("BEGIN") || (self.peek_kw("START") && self.peek_kw_at(1, "TRANSACTION") && self.eat_kw("START")) {
            if !self.eat_kw("TRANSACTION") {
                self.eat_kw("WORK");
            }
            // ISOLATION LEVEL ..., READ ONLY / READ WRITE, [NOT] DEFERRABLE: there is one isolation level
            while matches!(self.peek(), Tok::Ident(w) if matches!(w.to_ascii_uppercase().as_str(),
                "ISOLATION" | "LEVEL" | "READ" | "WRITE" | "ONLY" | "NOT" | "DEFERRABLE" | "SERIALIZABLE" | "REPEATABLE" | "COMMITTED" | "UNCOMMITTED"))
                || self.peek_sym(",")
            {
                self.advance();
            }
            return Ok(Statement::Begin);
        }
        if self.eat_kw("COMMIT") || (self.peek_kw("END") && matches!(self.peek_at(1), Tok::Sym(";") | Tok::Eof | Tok::Ident(_)) && self.eat_kw("END")) {
            if !self.eat_kw("TRANSACTION") {
                self.eat_kw("WORK");
            }
            self.eat_chain();
            return Ok(Statement::Commit);
        }
        if self.eat_kw("ROLLBACK") || self.eat_kw("ABORT") {
            if !self.eat_kw("TRANSACTION") {
                self.eat_kw("WORK");
            }
            self.eat_chain();
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

    /// `CREATE RULE name AS ON event TO table [WHERE cond] DO [ALSO | INSTEAD] (NOTHING | command | (command; ...))`.
    /// A rule is a row trigger here: `ALSO` is an AFTER trigger running the commands (with `NEW` and `OLD`), `INSTEAD`
    /// a BEFORE trigger that runs them and drops the operation (on a query, an INSTEAD OF trigger).
    fn rule(&mut self, start: usize) -> Result<Statement> {
        let name = self.ident()?;
        self.expect_kw("AS")?;
        self.expect_kw("ON")?;
        let event = if self.eat_kw("INSERT") {
            TriggerEvent::Insert
        } else if self.eat_kw("UPDATE") {
            TriggerEvent::Update
        } else if self.eat_kw("DELETE") {
            TriggerEvent::Delete
        } else {
            return Err(Error::syntax("a rule on SELECT (the way views are made in PostgreSQL) is not supported; use CREATE VIEW"));
        };
        self.expect_kw("TO")?;
        let table = self.table_ident()?;
        let when = if self.eat_kw("WHERE") { Some(self.expr()?) } else { None };
        self.expect_kw("DO")?;
        let instead = if self.eat_kw("INSTEAD") {
            true
        } else {
            self.eat_kw("ALSO");
            false
        };
        let mut body = Vec::new();
        if self.eat_kw("NOTHING") {
        } else if self.eat_sym("(") {
            loop {
                while self.eat_sym(";") {}
                if self.eat_sym(")") {
                    break;
                }
                body.push(self.statement()?);
            }
        } else {
            body.push(self.statement()?);
        }
        // NOTIFY and the like do nothing here
        body.retain(|s| !matches!(s, Statement::Ignored(_)));
        for s in &body {
            if !matches!(s, Statement::Insert { .. } | Statement::Update { .. } | Statement::Delete { .. } | Statement::Select(_)) {
                return Err(Error::syntax("a rule can run INSERT, UPDATE, DELETE and SELECT commands"));
            }
        }
        let end = self.toks[self.pos].pos;
        let sql = self.src[start..end].to_string();
        Ok(Statement::CreateTrigger(Box::new(TriggerDef {
            name,
            timing: if instead { TriggerTiming::Before } else { TriggerTiming::After },
            events: vec![event],
            update_of: Vec::new(),
            each_statement: false,
            table,
            when,
            body,
            function: instead.then(|| INSTEAD_MARK.to_string()),
            sql,
        })))
    }

    /// The text of a parenthesised expression, `(expr)`.
    fn paren_expr_text(&mut self) -> Result<String> {
        self.expect_sym("(")?;
        let start = self.toks[self.pos].pos;
        self.expr()?;
        let end = self.toks[self.pos].pos;
        self.expect_sym(")")?;
        Ok(self.src[start..end].trim().to_string())
    }

    /// `CREATE POLICY name ON table [AS PERMISSIVE | RESTRICTIVE] [FOR cmd] [TO roles] [USING (e)] [WITH CHECK (e)]`
    fn policy(&mut self) -> Result<Statement> {
        let name = self.ident()?;
        self.expect_kw("ON")?;
        let table = self.table_ident()?;
        let (mut permissive, mut command, mut roles) = (true, "all".to_string(), Vec::<String>::new());
        let (mut using, mut check) = (None, None);
        loop {
            if self.eat_kw("AS") {
                permissive = !self.eat_kw("RESTRICTIVE");
                if permissive {
                    self.expect_kw("PERMISSIVE")?;
                }
            } else if self.eat_kw("FOR") {
                command = self.role_name()?.to_ascii_lowercase();
                if !matches!(command.as_str(), "all" | "select" | "insert" | "update" | "delete") {
                    return Err(Error::syntax("a policy is FOR ALL, SELECT, INSERT, UPDATE or DELETE"));
                }
            } else if self.eat_kw("TO") {
                roles = self.role_list()?.into_iter().map(|r| r.to_ascii_lowercase()).collect();
            } else if self.eat_kw("USING") {
                using = Some(self.paren_expr_text()?);
            } else if self.eat_kw("WITH") {
                self.expect_kw("CHECK")?;
                check = Some(self.paren_expr_text()?);
            } else {
                break;
            }
        }
        let value = serde_json::json!({"permissive": permissive, "command": command, "roles": roles, "using": using, "check": check}).to_string();
        Ok(Statement::Catalog(Box::new(CatalogChange { kind: "policy", names: vec![format!("{table}|{name}")], value: Some(value), replace: false, cascade: false })))
    }

    /// An item of a `VALUES` row: an expression, or `DEFAULT` (the column's default).
    fn values_item(&mut self) -> Result<Expr> {
        if self.peek_kw("DEFAULT") && matches!(self.peek_at(1), Tok::Sym(",") | Tok::Sym(")")) {
            self.advance();
            return Ok(Expr::Param(DEFAULT_MARK.to_string()));
        }
        self.expr()
    }

    fn insert(&mut self) -> Result<Statement> {
        self.eat_kw("INTO");
        let table = self.table_ident()?;
        if self.peek_kw("PARTITION") && matches!(self.peek_at(1), Tok::Sym("(")) {
            self.advance();
            self.skip_parens()?;
        }
        if self.eat_kw("SET") {
            // INSERT INTO t SET a = 1, b = 2
            let (mut cols, mut row) = (Vec::new(), Vec::new());
            loop {
                cols.push(self.column_name()?);
                self.expect_sym("=")?;
                row.push(self.values_item()?);
                if !self.eat_sym(",") {
                    break;
                }
            }
            return Ok(Statement::Insert { table, columns: Some(cols), source: InsertSource::Values(vec![row]) });
        }
        let mut columns = None;
        if self.peek_sym("(") && !self.peek_kw_at(1, "SELECT") && !self.peek_kw_at(1, "WITH") {
            self.advance();
            columns = Some(self.ident_list()?);
            self.expect_sym(")")?;
        }
        let source = if self.peek_kw("DEFAULT") && self.peek_kw_at(1, "VALUES") {
            self.advance();
            self.advance();
            InsertSource::DefaultValues
        } else if self.eat_kw("VALUES") {
            let mut rows = Vec::new();
            loop {
                self.expect_sym("(")?;
                let mut row = vec![self.values_item()?];
                while self.eat_sym(",") {
                    row.push(self.values_item()?);
                }
                self.expect_sym(")")?;
                rows.push(row);
                if !self.eat_sym(",") {
                    break;
                }
            }
            InsertSource::Values(rows)
        } else if self.peek_kw("SELECT") || self.peek_kw("WITH") || self.peek_sym("(") {
            InsertSource::Select(Box::new(self.select()?))
        } else {
            return Err(self.unexpected("VALUES or SELECT"));
        };
        Ok(Statement::Insert { table, columns, source })
    }

    fn update(&mut self) -> Result<Statement> {
        let only = self.peek_kw("ONLY") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Quoted(_)) && self.eat_kw("ONLY");
        let st = self.update_inner()?;
        Ok(if only { Statement::Only(Box::new(st)) } else { st })
    }

    fn update_inner(&mut self) -> Result<Statement> {
        let table = self.table_ident()?;
        let alias = self.target_alias()?;
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
        let mut st = Statement::Update { table: table.clone(), assignments, filter };
        if let Some(alias) = alias {
            requalify(&mut st, &alias, &table)?;
        }
        Ok(st)
    }

    /// An optional `[AS] alias` after the target table of UPDATE, DELETE and MERGE.
    fn target_alias(&mut self) -> Result<Option<String>> {
        if self.eat_kw("AS") || self.peek_is_alias() {
            return Ok(Some(self.ident()?));
        }
        Ok(None)
    }

    /// `MERGE INTO t [AS a] USING source ON cond WHEN [NOT] MATCHED [AND c] THEN action ...`.
    fn merge(&mut self) -> Result<Statement> {
        self.expect_kw("INTO")?;
        let target = self.table_ident()?;
        let target_alias = self.target_alias()?;
        self.expect_kw("USING")?;
        let source = self.table_factor()?;
        self.expect_kw("ON")?;
        let on = self.expr()?;
        let mut whens = Vec::new();
        while self.eat_kw("WHEN") {
            let matched = !self.eat_kw("NOT");
            self.expect_kw("MATCHED")?;
            let condition = if self.eat_kw("AND") { Some(self.expr()?) } else { None };
            self.expect_kw("THEN")?;
            let action = if self.eat_kw("UPDATE") {
                self.expect_kw("SET")?;
                let mut sets = Vec::new();
                loop {
                    let col = self.column_name()?;
                    self.expect_sym("=")?;
                    sets.push((col, self.expr()?));
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                MergeAction::Update(sets)
            } else if self.eat_kw("DELETE") {
                MergeAction::Delete
            } else if self.eat_kw("INSERT") {
                let columns = if self.eat_sym("(") {
                    let c = self.ident_list()?;
                    self.expect_sym(")")?;
                    Some(c)
                } else {
                    None
                };
                self.expect_kw("VALUES")?;
                self.expect_sym("(")?;
                let mut values = vec![self.expr()?];
                while self.eat_sym(",") {
                    values.push(self.expr()?);
                }
                self.expect_sym(")")?;
                MergeAction::Insert { columns, values }
            } else {
                return Err(self.unexpected("UPDATE, DELETE or INSERT"));
            };
            match (&action, matched) {
                (MergeAction::Insert { .. }, true) => {
                    return Err(Error::syntax("WHEN MATCHED cannot INSERT; use WHEN NOT MATCHED"))
                }
                (MergeAction::Update(_) | MergeAction::Delete, false) => {
                    return Err(Error::syntax("WHEN NOT MATCHED can only INSERT"))
                }
                _ => {}
            }
            whens.push(MergeWhen { matched, condition, action });
        }
        if whens.is_empty() {
            return Err(self.unexpected("WHEN"));
        }
        Ok(Statement::Merge(Box::new(MergeDef { target, target_alias, source, on, whens })))
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
        let only = self.peek_kw("ONLY") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Quoted(_)) && self.eat_kw("ONLY");
        let st = self.delete_inner()?;
        Ok(if only { Statement::Only(Box::new(st)) } else { st })
    }

    fn delete_inner(&mut self) -> Result<Statement> {
        let table = self.table_ident()?;
        let alias = self.target_alias()?;
        let filter = if self.eat_kw("WHERE") { Some(self.expr()?) } else { None };
        let mut st = Statement::Delete { table: table.clone(), filter };
        if let Some(alias) = alias {
            requalify(&mut st, &alias, &table)?;
        }
        Ok(st)
    }

    /// A run-time parameter's name: a word, or two joined by a dot (`my.setting`).
    fn parameter_name(&mut self) -> Result<String> {
        let mut name = self.ident()?;
        while self.eat_sym(".") {
            name.push('.');
            name.push_str(&self.ident()?);
        }
        Ok(name.to_ascii_lowercase())
    }

    /// An optional `ASC` or `DESC` after an index column: true for `DESC`.
    fn index_direction(&mut self) -> bool {
        if self.eat_kw("DESC") {
            true
        } else {
            self.eat_kw("ASC");
            false
        }
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

    /// After `CREATE REPORT`: `name [layout clauses] AS select`.
    fn report(&mut self, start: usize) -> Result<Statement> {
        let name = self.ident()?;
        let mut layout = ReportLayout::default();
        let text = |p: &mut Parser| -> Result<String> {
            match p.advance() {
                Tok::Str(s) => Ok(s),
                _ => {
                    p.pos -= 1;
                    Err(p.unexpected("text in quotes"))
                }
            }
        };
        loop {
            if self.eat_kw("TITLE") {
                layout.title = Some(text(self)?);
            } else if self.eat_kw("SUBTITLE") {
                layout.subtitle = Some(text(self)?);
            } else if self.eat_kw("BREAK") {
                self.eat_kw("BY");
                let column = self.ident()?;
                let mut b = ReportBreak { column, header: None, footer: None, new_page: false };
                loop {
                    if self.eat_kw("HEADER") {
                        b.header = Some(text(self)?);
                    } else if self.eat_kw("FOOTER") {
                        b.footer = Some(text(self)?);
                    } else if self.peek_kw("NEW") && self.peek_kw_at(1, "PAGE") {
                        self.advance();
                        self.advance();
                        b.new_page = true;
                    } else {
                        break;
                    }
                }
                layout.breaks.push(b);
            } else if self.eat_kw("COLUMN") {
                let mut c = ReportColumn { name: self.ident()?, ..Default::default() };
                loop {
                    if self.eat_kw("HEADING") {
                        c.heading = Some(text(self)?);
                    } else if self.eat_kw("FORMAT") {
                        c.format = Some(text(self)?);
                    } else if self.eat_kw("ALIGN") {
                        c.align = Some(if self.eat_kw("LEFT") {
                            ReportAlign::Left
                        } else if self.eat_kw("RIGHT") {
                            ReportAlign::Right
                        } else {
                            self.expect_kw("CENTER")?;
                            ReportAlign::Center
                        });
                    } else if self.eat_kw("WIDTH") {
                        c.width = Some(usize::try_from(self.signed_int()?).map_err(|_| Error::syntax("WIDTH must be positive"))?);
                    } else if self.eat_kw("SUM") {
                        c.total = Some(ReportTotal::Sum);
                    } else if self.eat_kw("AVG") {
                        c.total = Some(ReportTotal::Avg);
                    } else if self.eat_kw("COUNT") {
                        c.total = Some(ReportTotal::Count);
                    } else if self.eat_kw("MIN") {
                        c.total = Some(ReportTotal::Min);
                    } else if self.eat_kw("MAX") {
                        c.total = Some(ReportTotal::Max);
                    } else if self.eat_kw("HIDE") {
                        c.hidden = true;
                    } else {
                        break;
                    }
                }
                if layout.columns.iter().any(|x| x.name.eq_ignore_ascii_case(&c.name)) {
                    return Err(Error::syntax(format!("the column '{}' is described twice", c.name)));
                }
                layout.columns.push(c);
            } else if self.peek_kw("GRAND") {
                self.advance();
                self.expect_kw("TOTAL")?;
                layout.grand_total = Some(if matches!(self.peek(), Tok::Str(_)) { text(self)? } else { "Grand total".to_string() });
            } else if self.peek_kw("PAGE") {
                self.advance();
                self.expect_kw("SIZE")?;
                layout.page_lines = Some(usize::try_from(self.signed_int()?).map_err(|_| Error::syntax("PAGE SIZE must be positive"))?);
                self.eat_kw("LINES");
            } else {
                break;
            }
        }
        self.expect_kw("AS")?;
        let query = self.select()?;
        let end = self.toks[self.pos].pos;
        let sql = self.src[start..end].trim().to_string();
        Ok(Statement::CreateReport(Box::new(ReportDef { name, layout, query, sql })))
    }

    /// After `CREATE FUNCTION` or `CREATE PROCEDURE`: `name (@p type [= default], ...) [RETURNS type] AS body`.
    fn routine(&mut self, function: bool, start: usize) -> Result<Statement> {
        let name = self.ident()?;
        self.expect_sym("(")?;
        let mut params = Vec::new();
        if !self.peek_sym(")") {
            loop {
                let pname = match self.advance() {
                    Tok::Param(n) => n,
                    _ => {
                        self.pos -= 1;
                        return Err(self.unexpected("a parameter such as @name"));
                    }
                };
                if params.iter().any(|p: &RoutineParam| p.name.eq_ignore_ascii_case(&pname)) {
                    return Err(Error::syntax(format!("the parameter @{pname} is declared twice")));
                }
                let data_type = if matches!(self.peek(), Tok::Ident(_)) && !self.peek_kw("DEFAULT") { Some(self.data_type_name()?) } else { None };
                let default = if self.eat_sym("=") || self.eat_kw("DEFAULT") { Some(self.expr()?) } else { None };
                params.push(RoutineParam { name: pname, data_type, default });
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        self.expect_sym(")")?;
        let returns = if self.eat_kw("RETURNS") { Some(self.data_type_name()?) } else { None };
        if !function && returns.is_some() {
            return Err(Error::syntax("a procedure does not have RETURNS; use a function to return a value"));
        }
        self.expect_kw("AS")?;
        let (body, end) = if function {
            self.eat_kw("RETURN");
            let e = self.expr()?;
            (RoutineBody::Expr(e), self.toks[self.pos].pos)
        } else {
            self.expect_kw("BEGIN")?;
            let b = self.proc_block(&["END"])?;
            self.expect_kw("END")?;
            (RoutineBody::Block(b), self.toks[self.pos - 1].pos + "END".len())
        };
        let sql = self.src[start..end].trim().to_string();
        Ok(Statement::CreateRoutine(Box::new(RoutineDef { name, function, params, returns, body, trigger: false, sql })))
    }

    /// A data type written as a name (`INTEGER`, `VARCHAR(20)`, `DOUBLE PRECISION`, ...).
    fn data_type_name(&mut self) -> Result<DataType> {
        let tn = self.type_name()?;
        let tn = crate::sql::usertypes::lookup(&tn).map(|u| u.base).unwrap_or(tn);
        let (ty, auto) = DataType::from_sql_name(&tn).ok_or_else(|| Error::syntax(format!("unknown data type '{tn}'")))?;
        if auto || ty == DataType::Calculated {
            return Err(Error::syntax(format!("'{tn}' cannot be used as the type of a parameter")));
        }
        if self.eat_sym("(") {
            while !self.peek_sym(")") && !self.at_eof() {
                self.advance();
            }
            self.expect_sym(")")?;
        }
        Ok(ty)
    }

    /// Procedure statements up to (not including) one of the `stops` keywords.
    fn proc_block(&mut self, stops: &[&str]) -> Result<Vec<ProcStmt>> {
        let mut out = Vec::new();
        loop {
            while self.eat_sym(";") {}
            if self.at_eof() {
                return Err(self.unexpected(stops[0]));
            }
            if stops.iter().any(|s| self.peek_kw(s)) {
                return Ok(out);
            }
            out.push(self.proc_stmt()?);
            if !self.peek_sym(";") && !stops.iter().any(|s| self.peek_kw(s)) {
                return Err(self.unexpected("';'"));
            }
        }
    }

    fn proc_stmt(&mut self) -> Result<ProcStmt> {
        if self.eat_kw("DECLARE") {
            let name = match self.advance() {
                Tok::Param(n) => n,
                _ => {
                    self.pos -= 1;
                    return Err(self.unexpected("a variable such as @name"));
                }
            };
            let data_type = if matches!(self.peek(), Tok::Ident(_)) && !self.peek_kw("DEFAULT") { Some(self.data_type_name()?) } else { None };
            let init = if self.eat_sym("=") || self.eat_kw("DEFAULT") { Some(self.expr()?) } else { None };
            return Ok(ProcStmt::Declare { name, data_type, init });
        }
        if self.peek_kw("SET") && matches!(self.peek_at(1), Tok::Param(_)) {
            self.advance();
            let Tok::Param(name) = self.advance() else { unreachable!("checked") };
            self.expect_sym("=")?;
            return Ok(ProcStmt::Set { name, value: self.expr()? });
        }
        if self.eat_kw("IF") {
            let mut branches = Vec::new();
            let mut otherwise = Vec::new();
            let cond = self.expr()?;
            self.expect_kw("THEN")?;
            branches.push((cond, self.proc_block(&["ELSEIF", "ELSIF", "ELSE", "END"])?));
            loop {
                if self.eat_kw("ELSEIF") || self.eat_kw("ELSIF") {
                    let c = self.expr()?;
                    self.expect_kw("THEN")?;
                    branches.push((c, self.proc_block(&["ELSEIF", "ELSIF", "ELSE", "END"])?));
                } else if self.eat_kw("ELSE") {
                    otherwise = self.proc_block(&["END"])?;
                } else {
                    break;
                }
            }
            self.expect_kw("END")?;
            self.expect_kw("IF")?;
            return Ok(ProcStmt::If { branches, otherwise });
        }
        if self.eat_kw("WHILE") {
            let cond = self.expr()?;
            self.expect_kw("DO")?;
            let body = self.proc_block(&["END"])?;
            self.expect_kw("END")?;
            self.expect_kw("WHILE")?;
            return Ok(ProcStmt::While { cond, body });
        }
        if self.eat_kw("LEAVE") {
            return Ok(ProcStmt::Leave);
        }
        if self.eat_kw("RETURN") {
            if self.peek_sym(";") || self.peek_kw("END") || self.at_eof() {
                return Ok(ProcStmt::Return(None));
            }
            return Ok(ProcStmt::Return(Some(self.expr()?)));
        }
        let st = self.statement()?;
        if matches!(
            st,
            Statement::Begin
                | Statement::Commit
                | Statement::Rollback
                | Statement::Savepoint(_)
                | Statement::ReleaseSavepoint(_)
                | Statement::RollbackTo(_)
                | Statement::Parameters(_)
        ) {
            return Err(Error::syntax("a procedure runs as one unit and cannot start or end a transaction"));
        }
        Ok(ProcStmt::Sql(st))
    }

    /// `name BEFORE|AFTER INSERT|UPDATE|DELETE ON table [FOR EACH ROW]
    /// [WHEN cond] BEGIN stmt; ... END`; `start` is where `CREATE` began.
    fn trigger(&mut self, start: usize) -> Result<Statement> {
        let name = self.ident()?;
        let timing = if self.eat_kw("BEFORE") {
            TriggerTiming::Before
        } else if self.eat_kw("AFTER") {
            TriggerTiming::After
        } else if self.eat_kw("INSTEAD") {
            self.expect_kw("OF")?;
            TriggerTiming::InsteadOf
        } else if self.peek_kw("INSERT") || self.peek_kw("UPDATE") || self.peek_kw("DELETE") {
            TriggerTiming::Before // as in SQLite, when the timing is left out
        } else {
            return Err(self.unexpected("BEFORE, AFTER or INSTEAD OF"));
        };
        // One or more events: `INSERT OR UPDATE [OF a, b] OR DELETE`.
        let mut events = Vec::new();
        let mut update_of = Vec::new();
        loop {
            let event = if self.eat_kw("INSERT") {
                TriggerEvent::Insert
            } else if self.eat_kw("UPDATE") {
                if self.eat_kw("OF") {
                    update_of = self.ident_list()?;
                }
                TriggerEvent::Update
            } else if self.eat_kw("DELETE") {
                TriggerEvent::Delete
            } else {
                return Err(self.unexpected("INSERT, UPDATE or DELETE"));
            };
            if events.contains(&event) {
                return Err(Error::syntax("a trigger names each event only once"));
            }
            events.push(event);
            if !self.eat_kw("OR") {
                break;
            }
        }
        self.expect_kw("ON")?;
        let table = self.table_ident()?;
        if self.eat_kw("REFERENCING") {
            return Err(Error::syntax("REFERENCING (transition tables) is not supported"));
        }
        let mut each_statement = false;
        if self.eat_kw("FOR") {
            self.expect_kw("EACH")?;
            if self.eat_kw("STATEMENT") {
                each_statement = true;
            } else {
                self.expect_kw("ROW")?;
            }
        }
        if each_statement && timing == TriggerTiming::InsteadOf {
            return Err(Error::syntax("an INSTEAD OF trigger works on each row"));
        }
        let when = if self.eat_kw("WHEN") { Some(self.expr()?) } else { None };
        if self.eat_kw("EXECUTE") {
            // EXECUTE FUNCTION f(args): the body is a PL/pgSQL trigger function
            if !self.eat_kw("FUNCTION") {
                self.expect_kw("PROCEDURE")?;
            }
            let function = Some(self.table_ident()?);
            self.skip_parens()?;
            let end = self.toks[self.pos].pos;
            let sql = self.src[start..end].to_string();
            return Ok(Statement::CreateTrigger(Box::new(TriggerDef { name, timing, events, update_of, each_statement, table, when, body: Vec::new(), function, sql })));
        }
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
            if !matches!(
                st,
                Statement::Insert { .. } | Statement::Update { .. } | Statement::Delete { .. } | Statement::Raise(_) | Statement::Select(_)
            ) {
                return Err(Error::syntax("a trigger body may contain only INSERT, UPDATE, DELETE, SELECT and RAISE statements"));
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
        Ok(Statement::CreateTrigger(Box::new(TriggerDef { name, timing, events, update_of, each_statement, table, when, body, function: None, sql })))
    }

    fn create(&mut self) -> Result<Statement> {
        let create_pos = self.toks[self.pos - 1].pos;
        if self.peek_kw("OR") && self.peek_kw_at(1, "REPLACE") {
            self.advance();
            self.advance();
            let mut guarded = false;
            let mut inner = self.create_object(&mut guarded, create_pos)?;
            // The stored text is a plain CREATE, without the OR REPLACE.
            let strip = |sql: &mut String| {
                if let Some(at) = sql.to_ascii_uppercase().find("REPLACE") {
                    *sql = format!("CREATE {}", sql[at + "REPLACE".len()..].trim_start());
                }
            };
            match &mut inner {
                Statement::CreateTrigger(def) => strip(&mut def.sql),
                Statement::CreateRoutine(def) => strip(&mut def.sql),
                Statement::CreateReport(def) => strip(&mut def.sql),
                _ => {}
            }
            return match inner {
                Statement::CreateView { .. } | Statement::CreateTrigger(_) | Statement::CreateRoutine(_) | Statement::CreateReport(_) if !guarded => {
                    Ok(Statement::OrReplace(Box::new(inner)))
                }
                _ => Err(Error::syntax("CREATE OR REPLACE works for views, queries, triggers, functions and procedures; a table cannot be replaced")),
            };
        }
        let mut guarded = false;
        let st = self.create_object(&mut guarded, create_pos)?;
        Ok(if guarded { Statement::Guarded { missing_ok: false, stmt: Box::new(st) } } else { st })
    }

    /// `IF NOT EXISTS` / `IF EXISTS` after the object keyword.
    fn if_exists_clause(&mut self, negated: bool, guarded: &mut bool) -> Result<()> {
        if self.eat_kw("IF") {
            if negated {
                self.expect_kw("NOT")?;
            }
            self.expect_kw("EXISTS")?;
            *guarded = true;
        }
        Ok(())
    }

    fn create_object(&mut self, guarded: &mut bool, create_pos: usize) -> Result<Statement> {
        if self.ignorable_object() {
            return Ok(self.ignore_rest("CREATE"));
        }
        if self.peek_kw("MATERIALIZED") && self.peek_kw_at(1, "VIEW") {
            // a materialized view is a table filled once from its query
            self.advance();
            self.advance();
            self.if_exists_clause(true, guarded)?;
            let name = self.table_ident()?;
            self.expect_kw("AS")?;
            let query = Box::new(self.select()?);
            let mut with_data = true;
            if self.eat_kw("WITH") {
                with_data = !self.eat_kw("NO");
                self.expect_kw("DATA")?;
            }
            return Ok(Statement::CreateTableAs { name, query, with_data });
        }
        // temporary and unlogged tables are ordinary tables here
        while (self.peek_kw("TEMP") || self.peek_kw("TEMPORARY") || self.peek_kw("UNLOGGED") || self.peek_kw("GLOBAL") || self.peek_kw("LOCAL"))
            && (self.peek_kw_at(1, "TABLE") || self.peek_kw_at(1, "VIEW") || self.peek_kw_at(1, "TEMP") || self.peek_kw_at(1, "TEMPORARY") || self.peek_kw_at(1, "SEQUENCE"))
        {
            self.advance();
        }
        let unique = self.eat_kw("UNIQUE");
        if !unique && self.eat_kw("TRIGGER") {
            return self.trigger(create_pos);
        }
        if !unique && self.eat_kw("RULE") {
            return self.rule(create_pos);
        }
        if !unique && self.eat_kw("POLICY") {
            return self.policy();
        }
        if !unique {
            if let Some(st) = self.create_catalog_object(guarded)? {
                return Ok(st);
            }
        }
        if self.eat_kw("TABLE") {
            self.if_exists_clause(true, guarded)?;
            return self.create_table();
        }
        if !unique && self.eat_kw("REPORT") {
            self.if_exists_clause(true, guarded)?;
            return self.report(create_pos);
        }
        if !unique && (self.peek_kw("FUNCTION") || self.peek_kw("PROCEDURE")) {
            let function = self.peek_kw("FUNCTION");
            self.advance();
            self.if_exists_clause(true, guarded)?;
            if self.looks_like_pg_routine() {
                return self.pg_routine(function, create_pos);
            }
            return self.routine(function, create_pos);
        }
        if !unique && self.eat_kw("SEQUENCE") {
            self.if_exists_clause(true, guarded)?;
            let name = self.ident()?;
            let options = self.sequence_options()?;
            return Ok(Statement::CreateSequence { name, options });
        }
        if self.eat_kw("INDEX") {
            self.eat_kw("CONCURRENTLY");
            self.if_exists_clause(true, guarded)?;
            let name = if self.peek_kw("ON") { None } else { Some(self.ident()?) };
            self.expect_kw("ON")?;
            self.eat_kw("ONLY");
            let table = self.table_ident()?;
            if self.eat_kw("USING") {
                self.ident()?; // btree, hash, gist, ...: every index is the same kind here
            }
            self.expect_sym("(")?;
            let mut columns = Vec::new();
            let mut descending = Vec::new();
            loop {
                if self.peek_sym("(") || matches!(self.peek_at(1), Tok::Sym("(")) {
                    return Err(Error::syntax("an index on an expression is not supported; index the column"));
                }
                columns.push(self.ident()?);
                // an operator class or collation, then ASC/DESC and NULLS FIRST/LAST
                if self.eat_kw("COLLATE") {
                    self.ident()?;
                }
                while matches!(self.peek(), Tok::Ident(w) if w.to_ascii_lowercase().ends_with("_ops")) {
                    self.advance();
                }
                descending.push(self.index_direction());
                if self.eat_kw("NULLS") {
                    self.advance();
                }
                if !self.eat_sym(",") {
                    break;
                }
            }
            self.expect_sym(")")?;
            if self.eat_kw("INCLUDE") {
                self.skip_parens()?;
            }
            if self.eat_kw("WITH") {
                self.skip_parens()?;
            }
            if self.eat_kw("TABLESPACE") {
                self.ident()?;
            }
            if self.peek_kw("WHERE") {
                return Err(Error::syntax("a partial index (CREATE INDEX ... WHERE) is not supported"));
            }
            let name = name.unwrap_or_else(|| format!("{}_{}_idx", table.replace('.', "_"), columns.join("_")));
            return Ok(Statement::CreateIndex { name, table, columns, descending, unique });
        }
        if !unique && (self.eat_kw("VIEW") || self.eat_kw("QUERY")) {
            self.if_exists_clause(true, guarded)?;
            let name = self.ident()?;
            // Parameters come either as `name(@a, @b) AS ...` or as a leading
            // `PARAMETERS @a, @b;`; both are stored in the latter form.
            let mut params = Vec::new();
            let mut columns: Vec<String> = Vec::new();
            if self.eat_sym("(") {
                // `(@a, @b)` declares parameters; `(a, b)` names the columns of the result.
                if matches!(self.peek(), Tok::Param(_)) {
                    params = self.param_list()?;
                } else {
                    columns = self.ident_list()?;
                }
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
            let mut check = false;
            if self.peek_kw("WITH") && self.peek_kw_at(1, "CHECK") {
                self.advance();
                self.advance();
                self.expect_kw("OPTION")?;
                check = true;
            }
            let mut body = self.src[start..end].trim().to_string();
            if !columns.is_empty() {
                // Names for the columns: the query is wrapped so that they are applied.
                let names: Vec<String> = columns.iter().map(|c| format!("[{c}]")).collect();
                body = format!("SELECT * FROM ({body}) AS __v ({})", names.join(", "));
            }
            let body = body.as_str();
            let marker = if check { "/*CHECK OPTION*/ " } else { "" };
            let sql = if params.is_empty() {
                format!("{marker}{body}")
            } else {
                let decls: Vec<String> = params
                    .iter()
                    .map(|p| match p.data_type {
                        Some(t) => format!("@{} {}", p.name, t.sql_name()),
                        None => format!("@{}", p.name),
                    })
                    .collect();
                format!("{marker}PARAMETERS {}; {body}", decls.join(", "))
            };
            return Ok(Statement::CreateView { name, sql });
        }
        Err(self.unexpected("TABLE, INDEX, VIEW or TRIGGER"))
    }

    fn create_table(&mut self) -> Result<Statement> {
        let save = self.pos;
        let name = self.table_ident()?;
        if self.peek_kw("PARTITION") && self.peek_kw_at(1, "OF") {
            return self.create_partition(name);
        }
        if self.peek_kw("LIKE") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Quoted(_)) {
            // CREATE TABLE t LIKE other
            self.advance();
            let other = self.table_ident()?;
            let create = Statement::CreateTable { name, columns: Vec::new(), primary_key: Vec::new(), uniques: Vec::new(), foreign_keys: Vec::new(), checks: Vec::new() };
            return Ok(Statement::CreateTableExt(Box::new(TableExt { create, parents: Vec::new(), likes: vec![other], partition_by: None, partition_of: None })));
        }
        if self.peek_kw("AS") || self.peek_kw("SELECT") || ((self.peek_kw("IGNORE") || self.peek_kw("REPLACE")) && self.peek_kw_at(1, "SELECT")) {
            if !self.peek_kw("SELECT") && !self.peek_kw("IGNORE") && !self.peek_kw("REPLACE") {
                self.advance();
            } else if !self.peek_kw("SELECT") {
                self.advance();
            }
            let query = Box::new(self.select()?);
            let mut with_data = true;
            if self.eat_kw("WITH") {
                with_data = !self.eat_kw("NO");
                self.expect_kw("DATA")?;
            }
            return Ok(Statement::CreateTableAs { name, query, with_data });
        }
        self.pos = save;
        let (st, likes, indexes) = self.create_table_core()?;
        let mut trailing_select: Option<Box<Select>> = None;
        let mut parents = Vec::new();
        let mut partition_by = None;
        loop {
            if self.eat_kw("INHERITS") {
                self.expect_sym("(")?;
                loop {
                    parents.push(self.table_ident()?);
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_sym(")")?;
            } else if self.peek_kw("PARTITION") && self.peek_kw_at(1, "BY") {
                partition_by = Some(self.partition_by()?);
            } else if self.eat_kw("WITH") {
                self.skip_parens()?;
            } else if self.eat_kw("TABLESPACE") || self.eat_kw("USING") {
                self.ident()?;
            } else if self.table_option()? {
            } else if self.peek_kw("SELECT") || (self.peek_kw("AS") && self.peek_kw_at(1, "SELECT")) {
                // CREATE TABLE t (cols) SELECT ...: the rows of the query go in after the table is made
                self.eat_kw("AS");
                trailing_select = Some(Box::new(self.select()?));
                break;
            } else if self.eat_sym(",") {
            } else if self.peek_kw("ON") && self.peek_kw_at(1, "COMMIT") {
                while !self.at_eof() && !self.peek_sym(";") {
                    self.advance();
                }
            } else {
                break;
            }
        }
        let table_name = match &st {
            Statement::CreateTable { name, .. } => name.clone(),
            _ => String::new(),
        };
        let main = if parents.is_empty() && partition_by.is_none() && likes.is_empty() {
            st
        } else {
            Statement::CreateTableExt(Box::new(TableExt { create: st, parents, likes, partition_by, partition_of: None }))
        };
        if indexes.is_empty() && trailing_select.is_none() {
            return Ok(main);
        }
        // KEY / INDEX in the table definition: indexes made after the table
        let mut all = vec![main];
        if let Some(q) = trailing_select {
            all.push(Statement::Insert { table: table_name.clone(), columns: None, source: InsertSource::Select(q) });
        }
        for (n, cols, descending, unique) in indexes {
            let name = n.unwrap_or_else(|| format!("{}_{}_idx", table_name.replace('.', "_"), cols.join("_")));
            all.push(Statement::CreateIndex { name, table: table_name.clone(), columns: cols, descending, unique });
        }
        Ok(Statement::Batch(all))
    }

    fn skip_parens(&mut self) -> Result<()> {
        self.expect_sym("(")?;
        let mut depth = 1;
        while depth > 0 {
            match self.advance() {
                Tok::Sym("(") => depth += 1,
                Tok::Sym(")") => depth -= 1,
                Tok::Eof => return Err(self.unexpected("')'")),
                _ => {}
            }
        }
        Ok(())
    }

    /// `PARTITION BY RANGE | LIST | HASH (columns)`
    fn partition_by(&mut self) -> Result<(String, Vec<String>)> {
        self.expect_kw("PARTITION")?;
        self.expect_kw("BY")?;
        let method = self.ident()?.to_ascii_lowercase();
        if !matches!(method.as_str(), "range" | "list" | "hash") {
            return Err(Error::syntax("PARTITION BY takes RANGE, LIST or HASH"));
        }
        self.expect_sym("(")?;
        let columns = self.ident_list().map_err(|_| Error::syntax("a partition key must be made of column names"))?;
        self.expect_sym(")")?;
        Ok((method, columns))
    }

    fn expr_text(&mut self) -> Result<String> {
        let start = self.toks[self.pos].pos;
        self.expr()?;
        let end = self.toks[self.pos].pos;
        Ok(self.src[start..end].trim().to_string())
    }

    fn bound_list(&mut self) -> Result<Vec<String>> {
        self.expect_sym("(")?;
        let mut out = Vec::new();
        loop {
            if self.peek_kw("MINVALUE") || self.peek_kw("MAXVALUE") {
                let Tok::Ident(w) = self.advance() else { unreachable!("checked") };
                out.push(w.to_ascii_uppercase());
            } else {
                out.push(self.expr_text()?);
            }
            if !self.eat_sym(",") {
                break;
            }
        }
        self.expect_sym(")")?;
        Ok(out)
    }

    /// `CREATE TABLE name PARTITION OF parent FOR VALUES ... | DEFAULT`, after the name.
    fn create_partition(&mut self, name: String) -> Result<Statement> {
        self.expect_kw("PARTITION")?;
        self.expect_kw("OF")?;
        let parent = self.table_ident()?;
        if self.peek_sym("(") {
            self.skip_parens()?;
        }
        let bound = if self.eat_kw("DEFAULT") {
            serde_json::json!({"kind": "default"})
        } else {
            self.expect_kw("FOR")?;
            self.expect_kw("VALUES")?;
            if self.eat_kw("IN") {
                serde_json::json!({"kind": "list", "values": self.bound_list()?})
            } else if self.eat_kw("FROM") {
                let from = self.bound_list()?;
                self.expect_kw("TO")?;
                let to = self.bound_list()?;
                serde_json::json!({"kind": "range", "from": from, "to": to})
            } else if self.eat_kw("WITH") {
                self.expect_sym("(")?;
                let (mut modulus, mut remainder) = (0i64, 0i64);
                loop {
                    let which = self.ident()?.to_ascii_uppercase();
                    let Tok::Int(n) = self.advance() else {
                        self.pos -= 1;
                        return Err(self.unexpected("a number"));
                    };
                    match which.as_str() {
                        "MODULUS" => modulus = n,
                        "REMAINDER" => remainder = n,
                        _ => return Err(Error::syntax("hash partitions take MODULUS and REMAINDER")),
                    }
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_sym(")")?;
                serde_json::json!({"kind": "hash", "modulus": modulus, "remainder": remainder})
            } else {
                return Err(self.unexpected("IN, FROM or WITH"));
            }
        };
        let partition_by = if self.peek_kw("PARTITION") && self.peek_kw_at(1, "BY") { Some(self.partition_by()?) } else { None };
        let create = Statement::CreateTable { name, columns: Vec::new(), primary_key: Vec::new(), uniques: Vec::new(), foreign_keys: Vec::new(), checks: Vec::new() };
        Ok(Statement::CreateTableExt(Box::new(TableExt { create, parents: Vec::new(), likes: Vec::new(), partition_by, partition_of: Some((parent, bound.to_string())) })))
    }

    fn create_table_core(&mut self) -> Result<(Statement, Vec<String>, Vec<IndexSpec>)> {
        let name = self.table_ident()?;
        self.expect_sym("(")?;
        if self.eat_sym(")") {
            // a table with no columns of its own (all of them inherited)
            return Ok((Statement::CreateTable { name, columns: Vec::new(), primary_key: Vec::new(), uniques: Vec::new(), foreign_keys: Vec::new(), checks: Vec::new() }, Vec::new(), Vec::new()));
        }
        let mut indexes: Vec<IndexSpec> = Vec::new();
        let mut likes = Vec::new();
        let mut columns = Vec::new();
        let mut primary_key = Vec::new();
        let mut uniques = Vec::new();
        let mut foreign_keys = Vec::new();
        let mut checks: Vec<(Option<String>, String)> = Vec::new();
        loop {
            if self.peek_kw("CONSTRAINT") && self.peek_kw_at(2, "CHECK") {
                self.advance();
                let cname = self.ident()?;
                checks.push((Some(cname), self.check_clause()?));
            } else if self.peek_kw("CHECK") {
                checks.push((None, self.check_clause()?));
            } else if self.eat_kw("LIKE") {
                likes.push(self.table_ident()?);
                while !self.at_eof() && !self.peek_sym(",") && !self.peek_sym(")") {
                    self.advance(); // INCLUDING ALL, EXCLUDING DEFAULTS, ...
                }
            } else if self.is_key_element() {
                self.key_element(&mut primary_key, &mut uniques, &mut indexes)?;
            } else if self.peek_kw("FOREIGN") || self.peek_kw("CONSTRAINT") {
                foreign_keys.push(self.table_foreign_key()?);
            } else {
                columns.push(self.column_def()?);
            }
            if !self.eat_sym(",") {
                break;
            }
        }
        self.expect_sym(")")?;
        for c in &mut columns {
            if let Some(text) = c.check.take() {
                checks.push((None, text));
            }
        }
        Ok((Statement::CreateTable { name, columns, primary_key, uniques, foreign_keys, checks }, likes, indexes))
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
        let ref_table = self.table_ident()?;
        let mut ref_columns = Vec::new();
        if self.eat_sym("(") {
            ref_columns = self.ident_list()?;
            self.expect_sym(")")?;
        }
        let (mut del, mut upd) = self.references_actions()?;
        // `[NOT] DEFERRABLE [INITIALLY DEFERRED | INITIALLY IMMEDIATE]`, before or after the actions.
        let (mut deferrable, mut initially_deferred) = (false, false);
        loop {
            if self.eat_kw("NOT") {
                self.expect_kw("DEFERRABLE")?;
                deferrable = false;
            } else if self.eat_kw("DEFERRABLE") {
                deferrable = true;
            } else if self.eat_kw("INITIALLY") {
                if self.eat_kw("DEFERRED") {
                    initially_deferred = true;
                    deferrable = true;
                } else {
                    self.expect_kw("IMMEDIATE")?;
                    initially_deferred = false;
                }
            } else if self.peek_kw("ON") {
                // Actions given after the timing.
                let more = self.references_actions()?;
                del = more.0.or(del);
                upd = more.1.or(upd);
            } else {
                break;
            }
        }
        // Without an action, a relationship that can be deferred waits too (NO ACTION); others refuse at once.
        let default = if deferrable { RefAction::NoAction } else { RefAction::Restrict };
        let (on_delete, on_update) = (del.unwrap_or(default), upd.unwrap_or(default));
        Ok(ForeignKeyDef { name, columns, ref_table, ref_columns, on_delete, on_update, deferrable, initially_deferred })
    }

    /// `ON DELETE x` / `ON UPDATE y` clauses (either, in any order).
    fn references_actions(&mut self) -> Result<(Option<RefAction>, Option<RefAction>)> {
        let (mut del, mut upd) = (None, None);
        while self.eat_kw("ON") {
            let is_delete = if self.eat_kw("DELETE") {
                true
            } else if self.eat_kw("UPDATE") {
                false
            } else {
                return Err(self.unexpected("DELETE or UPDATE"));
            };
            let action = if self.eat_kw("CASCADE") {
                RefAction::Cascade
            } else if self.eat_kw("RESTRICT") {
                RefAction::Restrict
            } else if self.eat_kw("NO") {
                self.expect_kw("ACTION")?;
                RefAction::NoAction
            } else if self.eat_kw("SET") {
                if self.eat_kw("DEFAULT") {
                    RefAction::SetDefault
                } else {
                    self.expect_kw("NULL")?;
                    RefAction::SetNull
                }
            } else {
                return Err(self.unexpected("CASCADE, SET NULL, SET DEFAULT, RESTRICT or NO ACTION"));
            };
            if is_delete {
                del = Some(action);
            } else {
                upd = Some(action);
            }
        }
        Ok((del, upd))
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

    /// Consumes `SET` or `DROP`; true for `DROP`.
    fn advance_kw_drop(&mut self) -> bool {
        let drop = self.peek_kw("DROP");
        self.advance();
        drop
    }

    /// `NULLS FIRST` or `NULLS LAST` after an ORDER BY item.
    fn nulls_clause(&mut self) -> Result<Option<bool>> {
        if !self.eat_kw("NULLS") {
            return Ok(None);
        }
        if self.eat_kw("FIRST") {
            Ok(Some(true))
        } else {
            self.expect_kw("LAST")?;
            Ok(Some(false))
        }
    }

    /// `CHECK (expression)`; returns the expression text.
    fn check_clause(&mut self) -> Result<String> {
        self.expect_kw("CHECK")?;
        self.expect_sym("(")?;
        let start = self.toks[self.pos].pos;
        self.expr()?;
        let end = self.toks[self.pos].pos;
        self.expect_sym(")")?;
        Ok(self.src[start..end].trim().to_string())
    }

    fn column_def(&mut self) -> Result<ColumnDef> {
        let name = self.ident()?;
        let type_name = self.type_name()?;
        let user = crate::sql::usertypes::lookup(&type_name);
        // ENUM('a', 'b') and SET('a', 'b'): text that must be one of the labels
        let mut labels: Option<Vec<String>> = None;
        if (type_name.eq_ignore_ascii_case("ENUM") || type_name.eq_ignore_ascii_case("SET")) && self.peek_sym("(") {
            self.advance();
            let mut l = Vec::new();
            loop {
                match self.advance() {
                    Tok::Str(s) => l.push(s),
                    _ => {
                        self.pos -= 1;
                        return Err(self.unexpected("a label in quotes"));
                    }
                }
                if !self.eat_sym(",") {
                    break;
                }
            }
            self.expect_sym(")")?;
            labels = Some(l).filter(|_| type_name.eq_ignore_ascii_case("ENUM"));
        }
        let (data_type, autonumber) = match &user {
            Some(u) => DataType::from_sql_name(&u.base).ok_or_else(|| Error::syntax(format!("unknown data type '{}'", u.base)))?,
            None => DataType::from_sql_name(&type_name).ok_or_else(|| Error::syntax(format!("unknown data type '{type_name}'")))?,
        };
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
        while self.eat_kw("UNSIGNED") || self.eat_kw("SIGNED") || self.eat_kw("ZEROFILL") {}
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
            check: None,
        };
        if let Some(l) = labels {
            let list: Vec<String> = l.iter().map(|x| format!("'{}'", x.replace('\'', "''"))).collect();
            def.check = Some(format!("{} IN ({})", def.name, list.join(", ")));
        }
        if let Some(u) = user {
            // a domain or an enum: its base type, and what it requires of the column
            def.not_null = u.not_null;
            if let Some(d) = &u.default {
                if let Ok(Expr::Literal(v)) = parse_expression(d) {
                    def.default = Some(v);
                }
            }
            let checks = u.checks_for(&def.name);
            if !checks.is_empty() {
                def.check = Some(checks.iter().map(|c| format!("({c})")).collect::<Vec<_>>().join(" AND "));
            }
        }
        loop {
            if self.eat_kw("CONSTRAINT") {
                // A name for the constraint that follows; only CHECK uses it.
                self.ident()?;
            } else if self.peek_kw("CHECK") {
                def.check = Some(self.check_clause()?);
            } else if self.eat_kw("GENERATED") {
                if !self.eat_kw("ALWAYS") {
                    self.expect_kw("BY")?;
                    self.expect_kw("DEFAULT")?;
                }
                self.expect_kw("AS")?;
                if self.eat_kw("IDENTITY") {
                    def.autonumber = true;
                    if self.eat_sym("(") {
                        while !self.peek_sym(")") && !self.at_eof() {
                            self.advance();
                        }
                        self.expect_sym(")")?;
                    }
                } else {
                    // GENERATED ALWAYS AS (expression) [STORED|VIRTUAL]: a calculated field.
                    self.pos -= 1;
                    def.expression = Some(self.calculated_expression(&def.name)?);
                    def.data_type = DataType::Calculated;
                    if !self.eat_kw("STORED") {
                        self.eat_kw("VIRTUAL");
                    }
                }
            } else if self.eat_kw("NOT") {
                self.expect_kw("NULL")?;
                def.not_null = true;
            } else if self.eat_kw("NULL") {
            } else if self.eat_kw("PRIMARY") {
                self.expect_kw("KEY")?;
                def.primary_key = true;
            } else if self.eat_kw("UNIQUE") {
                self.eat_kw("KEY");
                def.unique = true;
            } else if self.peek_kw("KEY") {
                self.advance();
                def.primary_key = true;
            } else if self.eat_kw("UNSIGNED") || self.eat_kw("SIGNED") || self.eat_kw("ZEROFILL") || self.eat_kw("VISIBLE") || self.eat_kw("INVISIBLE") {
            } else if self.eat_kw("CHARACTER") {
                self.expect_kw("SET")?;
                self.advance();
            } else if self.eat_kw("CHARSET") || self.eat_kw("COLLATE") || self.eat_kw("STORAGE") || self.eat_kw("COLUMN_FORMAT") {
                self.advance();
            } else if self.eat_kw("COMMENT") {
                self.advance();
            } else if self.peek_kw("ON") && self.peek_kw_at(1, "UPDATE") {
                // ON UPDATE CURRENT_TIMESTAMP: not done by the engine
                self.advance();
                self.advance();
                self.skip_default_expr()?;
            } else if self.eat_kw("AUTO_INCREMENT") {
                def.autonumber = true;
            } else if self.eat_kw("REFERENCES") {
                def.references = Some(self.references_clause(None, vec![def.name.clone()])?);
            } else if self.eat_kw("AUTOINCREMENT") || self.eat_kw("IDENTITY") {
                def.autonumber = true;
            } else if self.eat_kw("DEFAULT") {
                let save = self.pos;
                match self.default_value() {
                    Ok(v) => def.default = Some(v),
                    Err(_) => {
                        // DEFAULT CURRENT_TIMESTAMP is the time of the insert; other expressions are read and not applied
                        self.pos = save;
                        let now = matches!(self.peek(), Tok::Ident(w) if matches!(w.to_ascii_uppercase().as_str(), "CURRENT_TIMESTAMP" | "NOW" | "LOCALTIMESTAMP" | "LOCALTIME" | "SYSDATE"))
                            && matches!(def.data_type, DataType::DateTime | DataType::TimestampTz);
                        self.skip_default_expr()?;
                        if now {
                            def.default = Some(Value::text(crate::schema::NOW_DEFAULT));
                        }
                    }
                }
            } else {
                break;
            }
        }
        Ok(def)
    }

    /// The value after `DEFAULT`: a literal, or `NEXT VALUE FOR seq` / `NEXTVAL('seq')`.
    fn default_value(&mut self) -> Result<Value> {
        let neg = self.eat_sym("-");
        self.default_value_after(neg)
    }

    fn default_value_after(&mut self, neg: bool) -> Result<Value> {
        match self.primary()? {
            Expr::Literal(v) if neg => negate_literal(&v).ok_or_else(|| Error::syntax("cannot negate DEFAULT value")),
            Expr::Literal(v) => Ok(v),
            Expr::Function { name, args, .. } if !neg && name.eq_ignore_ascii_case("NEXTVAL") => match args.as_slice() {
                [Expr::Literal(Value::Text(t))] => Ok(Value::text(&format!("{}{}", crate::schema::SEQUENCE_DEFAULT, &**t))),
                _ => Err(Error::syntax("NEXTVAL in a DEFAULT takes the name of a sequence")),
            },
            _ => Err(Error::syntax("DEFAULT must be a literal or NEXT VALUE FOR a sequence")),
        }
    }

    /// The options of `CREATE SEQUENCE` and `ALTER SEQUENCE`.
    fn sequence_options(&mut self) -> Result<SequenceOptions> {
        let mut o = SequenceOptions::default();
        loop {
            if self.eat_kw("AS") {
                self.ident()?; // the type: all sequences count in 64-bit integers
            } else if self.eat_kw("START") {
                self.eat_kw("WITH");
                o.start = Some(self.signed_int()?);
            } else if self.eat_kw("INCREMENT") {
                self.eat_kw("BY");
                o.increment = Some(self.signed_int()?);
            } else if self.eat_kw("MINVALUE") {
                o.min = Some(Some(self.signed_int()?));
            } else if self.eat_kw("MAXVALUE") {
                o.max = Some(Some(self.signed_int()?));
            } else if self.eat_kw("NO") {
                if self.eat_kw("MINVALUE") {
                    o.min = Some(None);
                } else if self.eat_kw("MAXVALUE") {
                    o.max = Some(None);
                } else if self.eat_kw("CYCLE") {
                    o.cycle = Some(false);
                } else {
                    return Err(self.unexpected("MINVALUE, MAXVALUE or CYCLE"));
                }
            } else if self.eat_kw("CYCLE") {
                o.cycle = Some(true);
            } else if self.eat_kw("CACHE") {
                self.signed_int()?; // accepted and ignored
            } else if self.eat_kw("RESTART") {
                o.restart = Some(if self.eat_kw("WITH") { Some(self.signed_int()?) } else { None });
            } else {
                return Ok(o);
            }
        }
    }

    fn signed_int(&mut self) -> Result<i64> {
        let neg = self.eat_sym("-");
        if !neg {
            self.eat_sym("+");
        }
        match self.advance() {
            Tok::Int(n) => Ok(if neg { -n } else { n }),
            _ => {
                self.pos -= 1;
                Err(self.unexpected("a whole number"))
            }
        }
    }

    fn alter(&mut self) -> Result<Statement> {
        if self.peek_kw("ROLE") || self.peek_kw("USER") || self.peek_kw("GROUP") {
            return self.alter_role();
        }
        if self.eat_kw("SEQUENCE") {
            let mut guarded = false;
            self.if_exists_clause(false, &mut guarded)?;
            let name = self.ident()?;
            let options = self.sequence_options()?;
            let st = Statement::AlterSequence { name, options };
            return Ok(if guarded { Statement::Guarded { missing_ok: true, stmt: Box::new(st) } } else { st });
        }
        if self.ignorable_object() {
            return Ok(self.ignore_rest("ALTER"));
        }
        self.expect_kw("TABLE")?;
        let mut if_exists = false;
        if self.eat_kw("IF") {
            self.expect_kw("EXISTS")?;
            if_exists = true;
        }
        self.eat_kw("ONLY");
        let table = self.table_ident()?;
        let mut actions = vec![self.alter_table_action(&table)?];
        while self.eat_sym(",") {
            actions.push(self.alter_table_action(&table)?);
        }
        let st = if actions.len() == 1 { actions.remove(0) } else { Statement::Batch(actions) };
        Ok(if if_exists { Statement::Guarded { missing_ok: true, stmt: Box::new(st) } } else { st })
    }

    /// One action of `ALTER TABLE name action [, action ...]`.
    fn alter_table_action(&mut self, table: &str) -> Result<Statement> {
        let table = table.to_string();
        {
            let no = self.peek_kw("NO") && self.peek_kw_at(1, "FORCE");
            let k = usize::from(no);
            if (self.peek_kw("ENABLE") || self.peek_kw("DISABLE") || self.peek_kw("FORCE") || no) && self.peek_kw_at(k + 1, "ROW") && self.peek_kw_at(k + 2, "LEVEL") {
                let word = self.ident()?.to_ascii_uppercase();
                if no {
                    self.advance();
                }
                for w in ["ROW", "LEVEL", "SECURITY"] {
                    self.expect_kw(w)?;
                }
                let (enable, force) = match (word.as_str(), no) {
                    ("ENABLE", _) => (Some(true), None),
                    ("DISABLE", _) => (Some(false), None),
                    ("NO", _) => (None, Some(false)),
                    _ => (None, Some(true)),
                };
                return Ok(Statement::SetRls { table, enable, force });
            }
        }
        if let Some(st) = self.mysql_alter_action(&table)? {
            return Ok(st);
        }
        // properties of a server table that mean nothing here
        if self.peek_kw("ENABLE") || self.peek_kw("DISABLE") || self.peek_kw("FORCE") || self.peek_kw("NO") && self.peek_kw_at(1, "FORCE")
            || self.peek_kw("OWNER") || self.peek_kw("REPLICA") || self.peek_kw("CLUSTER") || self.peek_kw("SET") && !self.peek_kw_at(1, "DATA") && !self.peek_kw_at(1, "DEFAULT")
            || self.peek_kw("RESET") || self.peek_kw("INHERIT") || self.peek_kw("NO") || self.peek_kw("OF") || self.peek_kw("VALIDATE")
        {
            let mut depth = 0;
            while !self.at_eof() && !self.peek_sym(";") && !(depth == 0 && self.peek_sym(",")) {
                match self.advance() {
                    Tok::Sym("(") => depth += 1,
                    Tok::Sym(")") => depth -= 1,
                    _ => {}
                }
            }
            return Ok(Statement::Ignored("ALTER TABLE".into()));
        }
        if self.eat_kw("ATTACH") {
            self.expect_kw("PARTITION")?;
            let child = self.table_ident()?;
            let bound = self.partition_bound_text()?;
            return Ok(Statement::AttachPartition { parent: table, child, bound: Some(bound) });
        }
        if self.eat_kw("DETACH") {
            self.expect_kw("PARTITION")?;
            let child = self.table_ident()?;
            self.eat_kw("CONCURRENTLY");
            self.eat_kw("FINALIZE");
            return Ok(Statement::AttachPartition { parent: table, child, bound: None });
        }
        let action = if self.eat_kw("ADD") {
            if self.peek_kw("CHECK") {
                AlterAction::AddCheck { name: None, expr: self.check_clause()? }
            } else if self.peek_kw("CONSTRAINT") && self.peek_kw_at(2, "CHECK") {
                self.advance();
                let name = self.ident()?;
                AlterAction::AddCheck { name: Some(name), expr: self.check_clause()? }
            } else if self.peek_kw("FOREIGN") || self.peek_kw("CONSTRAINT") {
                AlterAction::AddForeignKey(self.table_foreign_key()?)
            } else {
                self.eat_kw("COLUMN");
                let def = self.column_def()?;
                // MySQL: FIRST | AFTER column
                if self.eat_kw("FIRST") {
                } else if self.eat_kw("AFTER") {
                    self.ident()?;
                }
                AlterAction::AddColumn(def)
            }
        } else if self.eat_kw("DROP") {
            if self.eat_kw("CONSTRAINT") {
                AlterAction::DropConstraint(self.ident()?)
            } else {
                self.eat_kw("COLUMN");
                AlterAction::DropColumn(self.ident()?)
            }
        } else if self.eat_kw("RENAME") {
            if self.eat_kw("TO") {
                return Ok(Statement::AlterTable { table, action: AlterAction::RenameTable(self.ident()?) });
            }
            self.eat_kw("COLUMN");
            let from = self.ident()?;
            self.expect_kw("TO")?;
            AlterAction::RenameColumn { from, to: self.ident()? }
        } else if self.eat_kw("ALTER") {
            self.eat_kw("COLUMN");
            let name = self.ident()?;
            if self.peek_kw("SET") && self.peek_kw_at(1, "DATA") {
                self.advance();
                self.advance();
            } else if (self.peek_kw("SET") || self.peek_kw("DROP") || self.peek_kw("ADD") || self.peek_kw("RESET") || self.peek_kw("RESTART"))
                && !self.peek_kw_at(1, "DEFAULT")
                && !(self.peek_kw_at(1, "NOT") && self.peek_kw_at(2, "NULL"))
                && !(self.peek_kw("DROP") && self.peek_kw_at(1, "NOT"))
            {
                // SET STATISTICS / STORAGE / COMPRESSION / (options), ADD GENERATED, DROP IDENTITY, ...: storage details
                let mut depth = 0;
                while !self.at_eof() && !self.peek_sym(";") && !(depth == 0 && self.peek_sym(",")) {
                    match self.advance() {
                        Tok::Sym("(") => depth += 1,
                        Tok::Sym(")") => depth -= 1,
                        _ => {}
                    }
                }
                return Ok(Statement::Ignored("ALTER COLUMN".into()));
            }
            if self.peek_kw("SET") || self.peek_kw("DROP") {
                let drop = self.advance_kw_drop();
                let action = if self.eat_kw("DEFAULT") {
                    if drop {
                        AlterAction::SetDefault { name, value: None }
                    } else {
                        let neg = self.eat_sym("-");
                        let v = self.default_value_after(neg)?;
                        AlterAction::SetDefault { name, value: Some(v) }
                    }
                } else if self.eat_kw("NOT") {
                    self.expect_kw("NULL")?;
                    AlterAction::SetNotNull { name, not_null: !drop }
                } else {
                    return Err(self.unexpected("DEFAULT or NOT NULL"));
                };
                return Ok(Statement::AlterTable { table, action });
            }
            self.eat_kw("TYPE");
            let type_name = self.type_name()?;
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
        let mut guarded = false;
        let st = self.drop_object(&mut guarded)?;
        Ok(if guarded { Statement::Guarded { missing_ok: true, stmt: Box::new(st) } } else { st })
    }

    fn drop_object(&mut self, guarded: &mut bool) -> Result<Statement> {
        if self.eat_kw("POLICY") {
            self.if_exists_clause(false, guarded)?;
            let name = self.ident()?;
            self.expect_kw("ON")?;
            let table = self.table_ident()?;
            self.eat_kw("CASCADE");
            self.eat_kw("RESTRICT");
            return Ok(Statement::Catalog(Box::new(CatalogChange { kind: "policy", names: vec![format!("{table}|{name}")], value: None, replace: false, cascade: false })));
        }
        if self.eat_kw("RULE") {
            self.if_exists_clause(false, guarded)?;
            let name = self.ident()?;
            self.expect_kw("ON")?;
            self.table_ident()?;
            self.eat_kw("CASCADE");
            self.eat_kw("RESTRICT");
            return Ok(Statement::DropTrigger(name));
        }
        if self.ignorable_object() {
            return Ok(self.ignore_rest("DROP"));
        }
        if self.peek_kw("MATERIALIZED") && self.peek_kw_at(1, "VIEW") {
            self.advance();
            self.advance();
            self.if_exists_clause(false, guarded)?;
            return Ok(Statement::DropTable(self.table_ident()?));
        }
        if let Some(st) = self.drop_catalog_object(guarded)? {
            return Ok(st);
        }
        self.eat_kw("TEMPORARY");
        if self.eat_kw("TABLE") || self.eat_kw("TABLES") {
            self.if_exists_clause(false, guarded)?;
            let mut names = vec![self.table_ident()?];
            while self.eat_sym(",") {
                names.push(self.table_ident()?);
            }
            let cascade = self.eat_kw("CASCADE");
            self.eat_kw("RESTRICT");
            if names.len() == 1 && !cascade {
                return Ok(Statement::DropTable(names.remove(0)));
            }
            let if_exists = std::mem::take(guarded);
            Ok(Statement::DropTables { names, cascade, if_exists })
        } else if self.eat_kw("VIEW") || self.eat_kw("QUERY") {
            self.if_exists_clause(false, guarded)?;
            Ok(Statement::DropView(self.table_ident()?))
        } else if self.eat_kw("REPORT") {
            self.if_exists_clause(false, guarded)?;
            Ok(Statement::DropReport(self.ident()?))
        } else if self.peek_kw("FUNCTION") || self.peek_kw("PROCEDURE") {
            let function = self.peek_kw("FUNCTION");
            self.advance();
            self.if_exists_clause(false, guarded)?;
            let name = self.table_ident()?;
            self.skip_argument_types()?;
            self.eat_kw("CASCADE");
            self.eat_kw("RESTRICT");
            Ok(Statement::DropRoutine { name, function })
        } else if self.eat_kw("SEQUENCE") {
            self.if_exists_clause(false, guarded)?;
            Ok(Statement::DropSequence(self.ident()?))
        } else if self.eat_kw("TRIGGER") {
            self.if_exists_clause(false, guarded)?;
            Ok(Statement::DropTrigger(self.ident()?))
        } else if self.eat_kw("INDEX") {
            self.if_exists_clause(false, guarded)?;
            let name = self.ident()?;
            let table = if self.eat_kw("ON") { Some(self.table_ident()?) } else { None };
            Ok(Statement::DropIndex { name, table })
        } else {
            Err(self.unexpected("TABLE, VIEW, INDEX, SEQUENCE, FUNCTION, PROCEDURE, REPORT or TRIGGER"))
        }
    }

    // ----- SELECT -----------------------------------------------------------

    fn select(&mut self) -> Result<Select> {
        if self.peek_kw("TRANSFORM") {
            return self.crosstab();
        }
        let with = if self.peek_kw("WITH") { Some(Box::new(self.with_clause()?)) } else { None };
        let mut sel = self.select_core()?;
        // `SELECT generate_series(1, 3)` / `SELECT jsonb_path_query(doc, path)`: a function that returns rows, alone in the
        // select list, is the same as selecting from it
        if sel.from.is_none() && sel.items.len() == 1 {
            if let SelectItem::Expr { expr: Expr::Function { name, args, .. }, alias } = &sel.items[0] {
                let up = name.to_ascii_uppercase();
                if matches!(up.as_str(), "GENERATE_SERIES" | "JSONB_PATH_QUERY" | "JSONB_PATH_QUERY_TZ") {
                    let col = if up == "GENERATE_SERIES" { "generate_series" } else { "jsonb_path_query" };
                    let label = alias.clone().unwrap_or_else(|| name.to_ascii_lowercase());
                    sel.from = Some(From {
                        first: TableFactor::Function { name: up, args: args.clone(), alias: "srf".into(), columns: Vec::new() },
                        joins: Vec::new(),
                    });
                    sel.items = vec![SelectItem::Expr { expr: Expr::Column { table: Some("srf".into()), name: col.into() }, alias: Some(label) }];
                }
            }
        }
        // Set operators combine left to right.
        loop {
            let op = if self.eat_kw("UNION") {
                SetOp::Union
            } else if self.eat_kw("INTERSECT") {
                SetOp::Intersect
            } else if self.eat_kw("EXCEPT") || self.eat_kw("MINUS") {
                SetOp::Except
            } else {
                break;
            };
            let all = if self.eat_kw("ALL") {
                true
            } else {
                self.eat_kw("DISTINCT");
                false
            };
            let select = self.select_core()?;
            sel.unions.push(SetPart { op, all, select });
        }
        self.select_tail(&mut sel)?;
        sel.with = with;
        Ok(sel)
    }

    /// `TRANSFORM aggregate SELECT ... [ORDER BY ...] PIVOT column [IN (values)]`, the Access crosstab.
    fn crosstab(&mut self) -> Result<Select> {
        self.expect_kw("TRANSFORM")?;
        let agg = self.expr()?;
        // Access allows a name for the aggregate; the result does not use it.
        if self.eat_kw("AS") {
            self.ident()?;
        }
        if !self.peek_kw("SELECT") {
            return Err(self.unexpected("SELECT"));
        }
        let mut sel = self.select()?;
        self.expect_kw("PIVOT")?;
        let pivot = self.concat()?;
        let values = if self.eat_kw("IN") {
            self.expect_sym("(")?;
            let mut list = vec![self.expr()?];
            while self.eat_sym(",") {
                list.push(self.expr()?);
            }
            self.expect_sym(")")?;
            Some(list)
        } else {
            None
        };
        sel.crosstab = Some(Box::new(Crosstab { agg, pivot, values }));
        Ok(sel)
    }

    /// `WITH [RECURSIVE] name [(columns)] AS (query) [, ...]`.
    fn with_clause(&mut self) -> Result<With> {
        self.expect_kw("WITH")?;
        let recursive = self.eat_kw("RECURSIVE");
        let mut ctes = Vec::new();
        loop {
            let name = self.ident()?;
            let columns = if self.eat_sym("(") {
                let c = self.ident_list()?;
                self.expect_sym(")")?;
                c
            } else {
                Vec::new()
            };
            self.expect_kw("AS")?;
            self.expect_sym("(")?;
            let query = self.select()?;
            self.expect_sym(")")?;
            ctes.push(Cte { name, columns, query });
            if !self.eat_sym(",") {
                break;
            }
        }
        Ok(With { recursive, ctes })
    }

    /// `VALUES (a, b), (c, d)` as a query: the first row is the SELECT, the
    /// rest are `UNION ALL` members.
    fn values_select(&mut self) -> Result<Select> {
        self.expect_kw("VALUES")?;
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
        let width = rows[0].len();
        if rows.iter().any(|r| r.len() != width) {
            return Err(Error::syntax("every row of VALUES must have the same number of values"));
        }
        let mut rows = rows.into_iter();
        let first = rows.next().expect("at least one row");
        let items = first
            .into_iter()
            .enumerate()
            .map(|(i, expr)| SelectItem::Expr { expr, alias: Some(format!("column{}", i + 1)) })
            .collect();
        let mut sel = Select { items, ..Select::default() };
        for r in rows {
            let items = r.into_iter().map(|expr| SelectItem::Expr { expr, alias: None }).collect();
            sel.unions.push(SetPart { op: SetOp::Union, all: true, select: Select { items, ..Select::default() } });
        }
        Ok(sel)
    }

    /// `SELECT ... [FROM ...] [WHERE ...] [GROUP BY ...] [HAVING ...]`.
    fn select_core(&mut self) -> Result<Select> {
        if self.peek_kw("VALUES") {
            return self.values_select();
        }
        if self.peek_sym("(") {
            // A parenthesised query used as a set-operation operand.
            self.advance();
            let inner = self.select()?;
            self.expect_sym(")")?;
            return Ok(Select {
                items: vec![SelectItem::Wildcard],
                from: Some(From {
                    first: TableFactor::Derived { query: Box::new(inner), alias: "__paren".into(), columns: Vec::new(), lateral: false },
                    joins: Vec::new(),
                }),
                ..Select::default()
            });
        }
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
        self.select_into_tail()?;
        if self.eat_kw("FROM") {
            sel.from = Some(self.from_clause()?);
        }
        if self.eat_kw("WHERE") {
            sel.filter = Some(self.expr()?);
        }
        if self.eat_kw("GROUP") {
            self.expect_kw("BY")?;
            self.group_by(&mut sel)?;
        }
        if self.eat_kw("HAVING") {
            sel.having = Some(self.expr()?);
        }
        if self.peek_kw("WINDOW") && matches!(self.peek_at(1), Tok::Ident(_)) && self.peek_kw_at(2, "AS") {
            self.advance();
            loop {
                let name = self.ident()?;
                self.expect_kw("AS")?;
                let spec = self.window_spec()?;
                if sel.windows.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)) {
                    return Err(Error::syntax(format!("the window '{name}' is defined twice")));
                }
                sel.windows.push((name, spec));
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        Ok(sel)
    }

    /// `[ORDER BY ...] [LIMIT n [OFFSET m]]`, which close a whole query.
    fn select_tail(&mut self, sel: &mut Select) -> Result<()> {
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
                let nulls_first = self.nulls_clause()?;
                sel.order_by.push(OrderItem { expr, desc, nulls_first });
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        loop {
            if self.eat_kw("LIMIT") {
                sel.limit = Some(self.usize_literal()?);
            } else if self.eat_kw("OFFSET") {
                sel.offset = Some(self.usize_literal()?);
                if !self.eat_kw("ROWS") {
                    self.eat_kw("ROW");
                }
            } else if self.eat_kw("FETCH") {
                if !self.eat_kw("FIRST") {
                    self.expect_kw("NEXT")?;
                }
                let n = if matches!(self.peek(), Tok::Int(_)) { self.usize_literal()? } else { 1 };
                if !self.eat_kw("ROWS") {
                    self.expect_kw("ROW")?;
                }
                if self.eat_kw("WITH") {
                    return Err(Error::syntax("FETCH ... WITH TIES is not supported"));
                }
                self.expect_kw("ONLY")?;
                sel.limit = Some(n);
            } else {
                break;
            }
        }
        Ok(())
    }

    /// The elements of a GROUP BY: plain expressions, `ROLLUP (...)`,
    /// `CUBE (...)` and `GROUPING SETS (...)`, which combine by cross product.
    fn group_by(&mut self, sel: &mut Select) -> Result<()> {
        let index_of = |sel: &mut Select, e: Expr| -> usize {
            match sel.group_by.iter().position(|x| *x == e) {
                Some(i) => i,
                None => {
                    sel.group_by.push(e);
                    sel.group_by.len() - 1
                }
            }
        };
        let mut sets: Vec<Vec<usize>> = vec![Vec::new()];
        let mut special = false;
        loop {
            let alternatives: Vec<Vec<usize>> = if (self.peek_kw("ROLLUP") || self.peek_kw("CUBE"))
                && matches!(self.peek_at(1), Tok::Sym("("))
            {
                special = true;
                let cube = self.peek_kw("CUBE");
                self.advance();
                self.advance();
                let mut idx = Vec::new();
                loop {
                    let e = self.expr()?;
                    idx.push(index_of(sel, e));
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_sym(")")?;
                if cube {
                    (0..1usize << idx.len())
                        .rev()
                        .map(|mask| (0..idx.len()).filter(|b| mask >> (idx.len() - 1 - b) & 1 == 1).map(|b| idx[b]).collect())
                        .collect()
                } else {
                    (0..=idx.len()).rev().map(|n| idx[..n].to_vec()).collect()
                }
            } else if self.peek_kw("GROUPING") && self.peek_kw_at(1, "SETS") {
                special = true;
                self.advance();
                self.advance();
                self.expect_sym("(")?;
                let mut alts = Vec::new();
                loop {
                    let mut set = Vec::new();
                    if self.eat_sym("(") {
                        if !self.peek_sym(")") {
                            loop {
                                let e = self.expr()?;
                                set.push(index_of(sel, e));
                                if !self.eat_sym(",") {
                                    break;
                                }
                            }
                        }
                        self.expect_sym(")")?;
                    } else {
                        let e = self.expr()?;
                        set.push(index_of(sel, e));
                    }
                    alts.push(set);
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_sym(")")?;
                alts
            } else {
                let e = self.expr()?;
                vec![vec![index_of(sel, e)]]
            };
            let mut next = Vec::new();
            for base in &sets {
                for alt in &alternatives {
                    let mut s = base.clone();
                    for i in alt {
                        if !s.contains(i) {
                            s.push(*i);
                        }
                    }
                    next.push(s);
                }
            }
            sets = next;
            if !self.eat_sym(",") {
                break;
            }
        }
        if special {
            for s in &mut sets {
                s.sort_unstable();
            }
            sel.group_sets = sets;
        }
        Ok(())
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
            let natural = self.eat_kw("NATURAL");
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
            } else if self.eat_kw("FULL") {
                self.eat_kw("OUTER");
                self.expect_kw("JOIN")?;
                JoinKind::Full
            } else if self.eat_kw("CROSS") {
                self.expect_kw("JOIN")?;
                JoinKind::Cross
            } else if self.eat_kw("JOIN") {
                JoinKind::Inner
            } else if natural {
                return Err(self.unexpected("JOIN"));
            } else if self.eat_sym(",") {
                JoinKind::Cross
            } else {
                break;
            };
            let factor = self.table_factor()?;
            let mut on = None;
            let mut using = Vec::new();
            if kind != JoinKind::Cross && !natural {
                if self.eat_kw("USING") {
                    self.expect_sym("(")?;
                    using = self.ident_list()?;
                    self.expect_sym(")")?;
                } else {
                    self.expect_kw("ON")?;
                    on = Some(self.expr()?);
                }
            }
            from.joins.push(Join { kind, factor, on, using, natural });
        }
        Ok(from)
    }

    /// A table factor, or a parenthesised join list (Access nests joins).
    fn from_source(&mut self) -> Result<From> {
        if self.peek_sym("(") && !(self.peek_kw_at(1, "SELECT") || self.peek_kw_at(1, "VALUES") || self.peek_kw_at(1, "WITH")) {
            self.advance();
            let inner = self.from_clause()?;
            self.expect_sym(")")?;
            Ok(inner)
        } else {
            Ok(From { first: self.table_factor()?, joins: Vec::new() })
        }
    }

    /// A table factor followed by any number of `PIVOT (...)` / `UNPIVOT (...)` operators.
    fn table_factor(&mut self) -> Result<TableFactor> {
        let mut factor = self.table_factor_base()?;
        loop {
            if self.peek_kw("PIVOT") && self.pivot_clause_ahead() {
                self.advance();
                self.expect_sym("(")?;
                let agg = self.expr()?;
                self.expect_kw("FOR")?;
                let pivot = self.concat()?;
                self.expect_kw("IN")?;
                self.expect_sym("(")?;
                let mut values = Vec::new();
                loop {
                    let v = self.expr()?;
                    let label = if self.eat_kw("AS") {
                        Some(match self.advance() {
                            Tok::Str(s) => s,
                            Tok::Ident(s) | Tok::Quoted(s) => s,
                            _ => {
                                self.pos = self.pos.saturating_sub(1);
                                return Err(self.unexpected("a column name"));
                            }
                        })
                    } else {
                        None
                    };
                    values.push((v, label));
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_sym(")")?;
                self.expect_sym(")")?;
                let alias = if self.eat_kw("AS") || self.peek_is_alias() { self.ident()? } else { "pivot".to_string() };
                factor = TableFactor::Pivot { source: Box::new(factor), agg, pivot, values, alias };
            } else if self.eat_kw("UNPIVOT") {
                let mut include_nulls = false;
                if self.eat_kw("INCLUDE") {
                    self.expect_kw("NULLS")?;
                    include_nulls = true;
                } else if self.eat_kw("EXCLUDE") {
                    self.expect_kw("NULLS")?;
                }
                self.expect_sym("(")?;
                let value_col = self.ident()?;
                self.expect_kw("FOR")?;
                let name_col = self.ident()?;
                self.expect_kw("IN")?;
                self.expect_sym("(")?;
                let mut columns = Vec::new();
                loop {
                    let c = self.ident()?;
                    let label = if self.eat_kw("AS") {
                        Some(match self.advance() {
                            Tok::Str(s) => s,
                            Tok::Ident(s) | Tok::Quoted(s) => s,
                            _ => {
                                self.pos = self.pos.saturating_sub(1);
                                return Err(self.unexpected("a label"));
                            }
                        })
                    } else {
                        None
                    };
                    columns.push((c, label));
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_sym(")")?;
                self.expect_sym(")")?;
                let alias = if self.eat_kw("AS") || self.peek_is_alias() { self.ident()? } else { "unpivot".to_string() };
                factor = TableFactor::Unpivot { source: Box::new(factor), value_col, name_col, columns, include_nulls, alias };
            } else {
                return Ok(factor);
            }
        }
    }

    /// Whether the PIVOT ahead is a `PIVOT (agg FOR column IN (...))` clause on the table just read, as
    /// opposed to the `PIVOT column` that closes a `TRANSFORM` crosstab: a clause has FOR inside its parentheses.
    fn pivot_clause_ahead(&self) -> bool {
        if !matches!(self.peek_at(1), Tok::Sym("(")) {
            return false;
        }
        let mut depth = 0usize;
        for k in 1.. {
            match self.peek_at(k) {
                Tok::Sym("(") => depth += 1,
                Tok::Sym(")") => {
                    depth -= 1;
                    if depth == 0 {
                        return false;
                    }
                }
                Tok::Ident(w) if depth == 1 && w.eq_ignore_ascii_case("FOR") => return true,
                Tok::Eof => return false,
                _ => {}
            }
        }
        false
    }

    fn table_factor_base(&mut self) -> Result<TableFactor> {
        let lateral = self.eat_kw("LATERAL");
        if self.peek_sym("(")
            && (self.peek_kw_at(1, "SELECT") || self.peek_kw_at(1, "VALUES") || self.peek_kw_at(1, "WITH"))
        {
            self.advance();
            let query = Box::new(self.select()?);
            self.expect_sym(")")?;
            self.eat_kw("AS");
            let alias = self.ident()?;
            let columns = if self.eat_sym("(") {
                let c = self.ident_list()?;
                self.expect_sym(")")?;
                c
            } else {
                Vec::new()
            };
            return Ok(TableFactor::Derived { query, alias, columns, lateral });
        }
        // A table function: name(arguments) [AS] alias [(columns)].
        if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Sym("(")) {
            let name = self.ident()?.to_ascii_uppercase();
            self.expect_sym("(")?;
            let mut args = Vec::new();
            if !self.peek_sym(")") {
                args.push(self.expr()?);
                while self.eat_sym(",") {
                    args.push(self.expr()?);
                }
            }
            self.expect_sym(")")?;
            let alias = if self.eat_kw("AS") || self.peek_is_alias() { self.ident()? } else { name.to_ascii_lowercase() };
            let columns = if self.eat_sym("(") {
                let c = self.ident_list()?;
                self.expect_sym(")")?;
                c
            } else {
                Vec::new()
            };
            return Ok(TableFactor::Function { name, args, alias, columns });
        }
        if lateral {
            return Err(self.unexpected("a sub-query or function after LATERAL"));
        }
        let only = self.peek_kw("ONLY") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Quoted(_)) && self.eat_kw("ONLY");
        let mut name = self.ident()?;
        let mut default_alias = None;
        if name.eq_ignore_ascii_case("information_schema") && self.eat_sym(".") {
            // The catalog, as tables: INFORMATION_SCHEMA.COLUMNS and so on.
            let part = self.ident()?;
            name = format!("information_schema.{}", part.to_ascii_lowercase());
            default_alias = Some(part);
        } else if self.peek_sym(".") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Quoted(_)) {
            self.advance();
            let part = self.ident()?;
            default_alias = Some(part.clone());
            name = if name.eq_ignore_ascii_case("public") || name.eq_ignore_ascii_case("pg_catalog") { part } else { format!("{name}.{part}") };
        }
        let alias = if self.eat_kw("AS") || self.peek_is_alias() { Some(self.ident()?) } else { default_alias };
        Ok(TableFactor::Named { name, alias, only })
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
            if let Tok::Sym(s @ ("@?" | "@@")) = self.peek().clone() {
                // jsonb @? path (does the path select anything?), jsonb @@ path (is the predicate true?)
                self.advance();
                let right = self.concat()?;
                left = Expr::Function {
                    name: if s == "@?" { "JSONB_PATH_EXISTS" } else { "JSONB_PATH_MATCH" }.into(),
                    args: vec![left, right, Expr::Literal(Value::text("{}")), Expr::Literal(Value::Bool(true))],
                    distinct: false,
                    star: false,
                };
                continue;
            }
            if let Tok::Sym(s @ ("~" | "~*" | "!~" | "!~*")) = self.peek().clone() {
                // PostgreSQL's regular-expression match: ~ (case-sensitive), ~* (ignoring case), and their negations
                self.advance();
                let right = self.concat()?;
                let mut args = vec![left, right];
                if s.ends_with('*') {
                    args.push(Expr::Literal(Value::text("i")));
                }
                let m = Expr::Function { name: "REGEXP_LIKE".into(), args, distinct: false, star: false };
                left = if s.starts_with('!') { Expr::Not(Box::new(m)) } else { m };
                continue;
            }
            if let Some(op) = op {
                self.advance();
                if (self.peek_kw("ANY") || self.peek_kw("SOME") || self.peek_kw("ALL")) && matches!(self.peek_at(1), Tok::Sym("(")) {
                    left = self.quantified(left, op)?;
                    continue;
                }
                let right = self.concat()?;
                left = match (row_items(&left), row_items(&right)) {
                    (Some(a), Some(b)) => row_compare(op, a, b)?,
                    (None, None) => Expr::Binary { op, left: Box::new(left), right: Box::new(right) },
                    _ => return Err(Error::syntax("a row value (a, b) can only be compared with another row value of the same size")),
                };
                continue;
            }
            if self.eat_kw("IS") {
                let negated = self.eat_kw("NOT");
                left = if self.eat_kw("DISTINCT") {
                    self.expect_kw("FROM")?;
                    let right = self.concat()?;
                    distinct_from(left, right, negated)
                } else if self.eat_kw("TRUE") {
                    truth_test(left, Some(true), negated)
                } else if self.eat_kw("FALSE") {
                    truth_test(left, Some(false), negated)
                } else if self.eat_kw("UNKNOWN") {
                    Expr::IsNull { expr: Box::new(left), negated }
                } else {
                    self.expect_kw("NULL")?;
                    Expr::IsNull { expr: Box::new(left), negated }
                };
                continue;
            }
            let negated = self.peek_kw("NOT")
                && (self.peek_kw_at(1, "LIKE")
                    || self.peek_kw_at(1, "BETWEEN")
                    || self.peek_kw_at(1, "IN")
                    || (self.peek_kw_at(1, "SIMILAR") && self.peek_kw_at(2, "TO")));
            if negated {
                self.advance();
            }
            if self.peek_kw("SIMILAR") && self.peek_kw_at(1, "TO") {
                // `x SIMILAR TO pattern`: a regular-expression-like pattern matching the whole text.
                self.advance();
                self.advance();
                let pattern = self.concat()?;
                let call = Expr::Function { name: "SIMILAR_TO".into(), args: vec![left, pattern], distinct: false, star: false };
                left = if negated { Expr::Not(Box::new(call)) } else { call };
            } else if self.eat_kw("LIKE") {
                let mut pattern = self.concat()?;
                if self.eat_kw("ESCAPE") {
                    let esc = self.concat()?;
                    pattern = apply_escape(pattern, esc)?;
                }
                left = Expr::Like { expr: Box::new(left), pattern: Box::new(pattern), negated };
            } else if self.eat_kw("BETWEEN") {
                let symmetric = self.eat_kw("SYMMETRIC");
                if !symmetric {
                    self.eat_kw("ASYMMETRIC");
                }
                let low = self.concat()?;
                self.expect_kw("AND")?;
                let high = self.concat()?;
                left = if symmetric {
                    let fwd = Expr::Between {
                        expr: Box::new(left.clone()),
                        low: Box::new(low.clone()),
                        high: Box::new(high.clone()),
                        negated: false,
                    };
                    let rev = Expr::Between { expr: Box::new(left), low: Box::new(high), high: Box::new(low), negated: false };
                    let either = Expr::Binary { op: BinOp::Or, left: Box::new(fwd), right: Box::new(rev) };
                    if negated { Expr::Not(Box::new(either)) } else { either }
                } else {
                    Expr::Between { expr: Box::new(left), low: Box::new(low), high: Box::new(high), negated }
                };
            } else if self.eat_kw("IN") {
                self.expect_sym("(")?;
                if self.eat_sym(")") {
                    // `x IN ()`: nothing to be among, whatever x is (NULL included)
                    left = Expr::Literal(Value::Bool(negated));
                    continue;
                }
                if self.peek_kw("SELECT") || self.peek_kw("WITH") || self.peek_kw("VALUES") {
                    let q = self.select()?;
                    self.expect_sym(")")?;
                    left = match row_items(&left) {
                        Some(items) => row_in_query(items, q, negated),
                        None => Expr::InSelect { expr: Box::new(left), query: Box::new(q), negated },
                    };
                } else {
                    let mut list = vec![self.expr()?];
                    while self.eat_sym(",") {
                        list.push(self.expr()?);
                    }
                    self.expect_sym(")")?;
                    left = match row_items(&left) {
                        Some(items) => {
                            let mut any: Option<Expr> = None;
                            for item in &list {
                                let Some(b) = row_items(item) else {
                                    return Err(Error::syntax("a row value (a, b) must be compared with rows of the same size"));
                                };
                                let eq = row_compare(BinOp::Eq, items, b)?;
                                any = Some(match any {
                                    None => eq,
                                    Some(prev) => Expr::Binary { op: BinOp::Or, left: Box::new(prev), right: Box::new(eq) },
                                });
                            }
                            let any = any.expect("a non-empty list");
                            if negated { Expr::Not(Box::new(any)) } else { any }
                        }
                        None => Expr::InList { expr: Box::new(left), list, negated },
                    };
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
                Tok::Ident(s) if s.eq_ignore_ascii_case("DIV") => BinOp::ExactDiv,
                _ => break,
            };
            self.advance();
            let right = self.unary()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr> {
        if self.eat_sym("@") {
            // PostgreSQL's prefix absolute value: @ -5
            let inner = self.unary()?;
            return Ok(Expr::Function { name: "ABS".into(), args: vec![inner], distinct: false, star: false });
        }
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
        let mut base = self.primary()?;
        // `x::integer`, `'2024-01-01'::date`: PostgreSQL's cast, binding tighter than anything else
        while self.eat_sym("::") {
            base = self.type_cast(base)?;
        }
        // `doc -> 'key'` (JSON) and `doc ->> 'key'` (JSON as text).
        while matches!(self.peek(), Tok::Sym("->") | Tok::Sym("->>")) {
            let text = matches!(self.advance(), Tok::Sym("->>"));
            let key = self.primary()?;
            base = Expr::Function {
                name: if text { "JSON_GET_TEXT" } else { "JSON_GET" }.into(),
                args: vec![base, key],
                distinct: false,
                star: false,
            };
        }
        // `x AT TIME ZONE 'Europe/Paris'`
        while self.peek_kw("AT") && self.peek_kw_at(1, "TIME") && self.peek_kw_at(2, "ZONE") {
            for _ in 0..3 {
                self.advance();
            }
            let zone = self.primary()?;
            base = Expr::Function { name: "ATTIMEZONE".into(), args: vec![base, zone], distinct: false, star: false };
        }
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
                if self.peek_kw("SELECT") || self.peek_kw("WITH") || self.peek_kw("VALUES") {
                    let q = self.select()?;
                    self.expect_sym(")")?;
                    return Ok(Expr::Subquery(Box::new(q)));
                }
                let e = self.expr()?;
                if self.peek_sym(",") {
                    let mut items = vec![e];
                    while self.eat_sym(",") {
                        items.push(self.expr()?);
                    }
                    self.expect_sym(")")?;
                    return Ok(row_value(items));
                }
                self.expect_sym(")")?;
                Ok(e)
            }
            Tok::Param(p) => {
                self.advance();
                Ok(Expr::Param(p))
            }
            Tok::Quoted(_) => self.column_ref(),
            Tok::Ident(name) => {
                // VALUES(col) in ON DUPLICATE KEY UPDATE
                if name.eq_ignore_ascii_case("VALUES") && matches!(self.peek_at(1), Tok::Sym("(")) {
                    self.advance();
                    self.advance();
                    let mut args = vec![self.expr()?];
                    while self.eat_sym(",") {
                        args.push(self.expr()?);
                    }
                    self.expect_sym(")")?;
                    return Ok(Expr::Function { name: "VALUES".into(), args, distinct: false, star: false });
                }
                // pg_catalog.lower(x), public.f(x): the schema of a function is left off
                if matches!(self.peek_at(1), Tok::Sym(".")) && matches!(self.peek_at(2), Tok::Ident(_)) && matches!(self.peek_at(3), Tok::Sym("("))
                    && (name.eq_ignore_ascii_case("pg_catalog") || name.eq_ignore_ascii_case("public"))
                {
                    self.advance();
                    self.advance();
                    return self.primary();
                }
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
                    "ROW" if matches!(self.peek_at(1), Tok::Sym("(")) => {
                        self.advance();
                        self.advance();
                        let mut items = vec![self.expr()?];
                        while self.eat_sym(",") {
                            items.push(self.expr()?);
                        }
                        self.expect_sym(")")?;
                        return Ok(row_value(items));
                    }
                    // INTERVAL '3 days', INTERVAL '5' DAY, INTERVAL '1-6' YEAR TO MONTH.
                    "INTERVAL" if matches!(self.peek_at(1), Tok::Str(_)) => {
                        self.advance();
                        let Tok::Str(text) = self.advance() else { unreachable!("checked") };
                        let unit = |p: &Self| match p.peek() {
                            Tok::Ident(w) => crate::interval::Unit::from_word(w).filter(|_| !w.eq_ignore_ascii_case("d") && !w.eq_ignore_ascii_case("s") && !w.eq_ignore_ascii_case("h") && !w.eq_ignore_ascii_case("y")),
                            _ => None,
                        };
                        let qualifier = match unit(self) {
                            Some(from) => {
                                self.advance();
                                if self.eat_sym("(") {
                                    while !self.peek_sym(")") && !self.at_eof() {
                                        self.advance();
                                    }
                                    self.expect_sym(")")?;
                                }
                                let to = if self.eat_kw("TO") {
                                    let u = unit(self).ok_or_else(|| self.unexpected("YEAR, MONTH, DAY, HOUR, MINUTE or SECOND"))?;
                                    self.advance();
                                    Some(u)
                                } else {
                                    None
                                };
                                Some((from, to))
                            }
                            None => None,
                        };
                        let iv = crate::interval::Interval::parse(&text, qualifier).map_err(Error::syntax)?;
                        return Ok(Expr::Literal(Value::Interval(iv)));
                    }
                    // TIMESTAMP WITH TIME ZONE '2024-03-05 10:30:00+01:00', read in the session zone when no offset is given.
                    "TIMESTAMP" | "TIME"
                        if self.peek_kw_at(1, "WITH") && self.peek_kw_at(2, "TIME") && self.peek_kw_at(3, "ZONE") && matches!(self.peek_at(4), Tok::Str(_)) =>
                    {
                        for _ in 0..4 {
                            self.advance();
                        }
                        let Tok::Str(text) = self.advance() else { unreachable!("checked") };
                        return Ok(Expr::Function {
                            name: "CTIMESTAMPTZ".into(),
                            args: vec![Expr::Literal(Value::text(&text))],
                            distinct: false,
                            star: false,
                        });
                    }
                    // Typed literals: DATE '2024-03-05', TIMESTAMP '2024-03-05 10:30:00'.
                    "DATE" | "TIMESTAMP" | "TIME" if matches!(self.peek_at(1), Tok::Str(_)) => {
                        self.advance();
                        let Tok::Str(text) = self.advance() else { unreachable!("checked") };
                        let t = crate::datetime::parse(&text)
                            .ok_or_else(|| Error::syntax(format!("'{text}' is not a valid {up} literal")))?;
                        return Ok(Expr::Literal(Value::DateTime(t)));
                    }
                    // Any other type name before a string is a cast of that string: int4 '42', bool 't', float8 '1e3',
                    // numeric '1.50', text 'x', double precision '2.5'.
                    "DOUBLE" if self.peek_kw_at(1, "PRECISION") && matches!(self.peek_at(2), Tok::Str(_)) => {
                        self.advance();
                        self.advance();
                        let Tok::Str(text) = self.advance() else { unreachable!("checked") };
                        return Ok(Expr::Function { name: "CDBL".into(), args: vec![Expr::Literal(Value::text(&text))], distinct: false, star: false });
                    }
                    n if matches!(self.peek_at(1), Tok::Str(_))
                        && DataType::from_sql_name(n).is_some_and(|(t, auto)| !auto && t != DataType::Calculated) =>
                    {
                        let (ty, _) = DataType::from_sql_name(n).expect("checked");
                        self.advance();
                        let Tok::Str(text) = self.advance() else { unreachable!("checked") };
                        let name = cast_function(ty).expect("not CALCULATED");
                        return Ok(Expr::Function { name: name.into(), args: vec![Expr::Literal(Value::text(&text))], distinct: false, star: false });
                    }
                    "CURRENT_DATE" | "CURRENT_TIMESTAMP" | "CURRENT_TIME" | "LOCALTIMESTAMP" | "LOCALTIME"
                        if !matches!(self.peek_at(1), Tok::Sym("(")) =>
                    {
                        self.advance();
                        let f = match up.as_str() {
                            "CURRENT_DATE" => "LOCALDATE",
                            "CURRENT_TIMESTAMP" => "NOWTZ",
                            _ => "LOCALNOW",
                        };
                        return Ok(Expr::Function { name: f.into(), args: vec![], distinct: false, star: false });
                    }
                    "NEXT" | "PREVIOUS" if self.peek_kw_at(1, "VALUE") && self.peek_kw_at(2, "FOR") => {
                        self.advance();
                        self.advance();
                        self.advance();
                        let seq = self.ident()?;
                        let f = if up == "NEXT" { "NEXTVAL" } else { "CURRVAL" };
                        return Ok(Expr::Function { name: f.into(), args: vec![Expr::Literal(Value::text(&seq))], distinct: false, star: false });
                    }
                    "OVERLAY" if matches!(self.peek_at(1), Tok::Sym("(")) => {
                        self.advance();
                        self.advance();
                        let s = self.expr()?;
                        let mut args = vec![s];
                        if self.eat_kw("PLACING") {
                            args.push(self.expr()?);
                            self.expect_kw("FROM")?;
                            args.push(self.expr()?);
                            if self.eat_kw("FOR") {
                                args.push(self.expr()?);
                            }
                        } else {
                            while self.eat_sym(",") {
                                args.push(self.expr()?);
                            }
                        }
                        self.expect_sym(")")?;
                        return Ok(Expr::Function { name: "OVERLAY".into(), args, distinct: false, star: false });
                    }
                    "CAST" if matches!(self.peek_at(1), Tok::Sym("(")) => {
                        self.advance();
                        self.advance();
                        return self.cast_expr();
                    }
                    "EXTRACT" if matches!(self.peek_at(1), Tok::Sym("(")) => {
                        self.advance();
                        self.advance();
                        let field = self.ident_or_keyword()?;
                        self.expect_kw("FROM")?;
                        let e = self.expr()?;
                        self.expect_sym(")")?;
                        return Ok(Expr::Function {
                            name: "DATEPART".into(),
                            args: vec![Expr::Literal(Value::text(&field.to_ascii_lowercase())), e],
                            distinct: false,
                            star: false,
                        });
                    }
                    "POSITION" if matches!(self.peek_at(1), Tok::Sym("(")) => {
                        self.advance();
                        self.advance();
                        let needle = self.additive()?;
                        self.expect_kw("IN")?;
                        let hay = self.expr()?;
                        self.expect_sym(")")?;
                        return Ok(Expr::Function { name: "INSTR".into(), args: vec![hay, needle], distinct: false, star: false });
                    }
                    "SUBSTRING" if matches!(self.peek_at(1), Tok::Sym("(")) => {
                        self.advance();
                        self.advance();
                        let s = self.expr()?;
                        let mut args = vec![s];
                        if self.eat_kw("FROM") {
                            args.push(self.expr()?);
                            if self.eat_kw("FOR") {
                                args.push(self.expr()?);
                            }
                        } else {
                            while self.eat_sym(",") {
                                args.push(self.expr()?);
                            }
                        }
                        self.expect_sym(")")?;
                        return Ok(Expr::Function { name: "MID".into(), args, distinct: false, star: false });
                    }
                    "TRIM" if matches!(self.peek_at(1), Tok::Sym("(")) => {
                        self.advance();
                        self.advance();
                        return self.trim_expr();
                    }
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
                let callable = !RESERVED.contains(&up.as_str()) || matches!(up.as_str(), "LEFT" | "RIGHT" | "MOD");
                if matches!(self.peek_at(1), Tok::Sym("(")) && callable {
                    self.advance();
                    self.advance();
                    let call = self.function_call(name)?;
                    return self.call_suffixes(call);
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
            if !distinct {
                self.eat_kw("ALL"); // `Count(ALL x)`: the default, written out
            }
            args.push(self.expr()?);
            while self.eat_sym(",") {
                args.push(self.expr()?);
            }
        }
        self.expect_sym(")")?;
        Ok(Expr::Function { name: name.to_ascii_uppercase(), args, distinct, star })
    }

    /// A keyword or identifier used as a plain word (field names of EXTRACT).
    fn ident_or_keyword(&mut self) -> Result<String> {
        match self.advance() {
            Tok::Ident(s) => Ok(s),
            _ => {
                self.pos = self.pos.saturating_sub(1);
                Err(self.unexpected("a name"))
            }
        }
    }

    /// The name of a data type, including the multi-word standard spellings.
    fn type_name(&mut self) -> Result<String> {
        let first = match self.advance() {
            Tok::Ident(s) => s,
            _ => {
                self.pos = self.pos.saturating_sub(1);
                return Err(self.unexpected("a data type"));
            }
        };
        let up = first.to_ascii_uppercase();
        match up.as_str() {
            "DOUBLE" => {
                self.eat_kw("PRECISION");
            }
            "NATIONAL" => {
                if !self.eat_kw("CHARACTER") {
                    self.eat_kw("CHAR");
                }
                self.eat_kw("VARYING");
                return Ok("TEXT".into());
            }
            "CHARACTER" | "CHAR" | "VARCHAR" | "NCHAR" | "NVARCHAR" => {
                if self.eat_kw("VARYING") {
                    return Ok("TEXT".into());
                }
                if self.peek_kw("LARGE") {
                    self.advance();
                    self.expect_kw("OBJECT")?;
                }
            }
            "BINARY" => {
                self.eat_kw("LARGE");
                self.eat_kw("VARYING");
            }
            "TIMESTAMP" | "TIME" => {
                let with = self.eat_kw("WITH");
                if with || self.eat_kw("WITHOUT") {
                    self.expect_kw("TIME")?;
                    self.expect_kw("ZONE")?;
                    if with {
                        return Ok("TIMESTAMPTZ".into());
                    }
                }
            }
            _ => {}
        }
        Ok(first)
    }

    /// `CAST(expr AS type)` after the opening parenthesis: the conversion
    /// function for the type, and a ROUND for `DECIMAL(p, s)`.
    fn cast_expr(&mut self) -> Result<Expr> {
        let e = self.expr()?;
        self.expect_kw("AS")?;
        let tn = self.type_name()?;
        let (ty, _) = DataType::from_sql_name(&tn).ok_or_else(|| Error::syntax(format!("unknown data type '{tn}'")))?;
        let mut hints = Vec::new();
        if self.eat_sym("(") {
            while !self.peek_sym(")") && !self.at_eof() {
                if let Tok::Int(i) = self.advance() {
                    hints.push(i);
                }
            }
            self.expect_sym(")")?;
        }
        self.expect_sym(")")?;
        Ok(make_cast(e, ty, &hints))
    }

    /// The type after `::`, with an optional size, applied to `e`.
    fn type_cast(&mut self, e: Expr) -> Result<Expr> {
        let tn = self.type_name()?;
        let tn = crate::sql::usertypes::lookup(&tn).map(|u| u.base).unwrap_or(tn);
        let (ty, _) = DataType::from_sql_name(&tn).ok_or_else(|| Error::syntax(format!("unknown data type '{tn}'")))?;
        if ty == DataType::Calculated {
            return Err(Error::syntax("cannot cast to CALCULATED"));
        }
        let mut hints = Vec::new();
        if self.eat_sym("(") {
            while !self.peek_sym(")") && !self.at_eof() {
                if let Tok::Int(i) = self.advance() {
                    hints.push(i);
                }
            }
            self.expect_sym(")")?;
        }
        if self.peek_sym("[") {
            return Err(Error::syntax("array types are not supported"));
        }
        Ok(make_cast(e, ty, &hints))
    }

    /// `TRIM([LEADING|TRAILING|BOTH] [chars FROM] x)` after the parenthesis.
    fn trim_expr(&mut self) -> Result<Expr> {
        let mode = if self.eat_kw("LEADING") {
            Some("leading")
        } else if self.eat_kw("TRAILING") {
            Some("trailing")
        } else if self.eat_kw("BOTH") {
            Some("both")
        } else {
            None
        };
        let call = |args: Vec<Expr>, name: &str| Expr::Function { name: name.into(), args, distinct: false, star: false };
        let (chars, target) = if mode.is_some() && self.eat_kw("FROM") {
            (None, self.expr()?)
        } else {
            let first = self.expr()?;
            if self.eat_kw("FROM") {
                (Some(first), self.expr()?)
            } else if mode.is_none() {
                self.expect_sym(")")?;
                return Ok(call(vec![first], "TRIM"));
            } else {
                (None, first)
            }
        };
        self.expect_sym(")")?;
        Ok(call(
            vec![
                Expr::Literal(Value::text(mode.unwrap_or("both"))),
                chars.unwrap_or(Expr::Literal(Value::text(" "))),
                target,
            ],
            "TRIMOF",
        ))
    }

    /// `x op ANY|SOME|ALL (query)`, rewritten into IN, MIN/MAX comparisons or EXISTS.
    fn quantified(&mut self, x: Expr, op: BinOp) -> Result<Expr> {
        let all = self.peek_kw("ALL");
        self.advance();
        self.expect_sym("(")?;
        let mut q = self.select()?;
        self.expect_sym(")")?;
        if !all && op == BinOp::Eq {
            return Ok(Expr::InSelect { expr: Box::new(x), query: Box::new(q), negated: false });
        }
        if all && op == BinOp::Ne {
            return Ok(Expr::InSelect { expr: Box::new(x), query: Box::new(q), negated: true });
        }
        match q.items.as_mut_slice() {
            [SelectItem::Expr { alias, .. }] => *alias = Some("__q".into()),
            _ => return Err(Error::syntax("a quantified comparison needs a sub-query with exactly one column")),
        }
        let derived = |q: &Select| From {
            first: TableFactor::Derived { query: Box::new(q.clone()), alias: "__t".into(), columns: Vec::new(), lateral: false },
            joins: Vec::new(),
        };
        let col = Expr::Column { table: Some("__t".into()), name: "__q".into() };
        let agg = |name: &str| {
            Expr::Subquery(Box::new(Select {
                items: vec![SelectItem::Expr {
                    expr: Expr::Function { name: name.into(), args: vec![col.clone()], distinct: false, star: false },
                    alias: None,
                }],
                from: Some(derived(&q)),
                ..Select::default()
            }))
        };
        let (min, max) = (agg("MIN"), agg("MAX"));
        let cmp = |op: BinOp, r: Expr| Expr::Binary { op, left: Box::new(x.clone()), right: Box::new(r) };
        let both = |a: Expr, b: Expr, op: BinOp| Expr::Binary { op, left: Box::new(a), right: Box::new(b) };
        let body = match (all, op) {
            (false, BinOp::Gt | BinOp::Ge) | (true, BinOp::Lt | BinOp::Le) => cmp(op, min),
            (false, BinOp::Lt | BinOp::Le) | (true, BinOp::Gt | BinOp::Ge) => cmp(op, max),
            (false, BinOp::Ne) => both(cmp(BinOp::Ne, min), cmp(BinOp::Ne, max), BinOp::Or),
            (true, BinOp::Eq) => both(cmp(BinOp::Eq, min), cmp(BinOp::Eq, max), BinOp::And),
            _ => return Err(Error::syntax("unsupported quantified comparison")),
        };
        if !all {
            return Ok(body);
        }
        // ALL over an empty set is true.
        let empty = Expr::Exists {
            query: Box::new(Select {
                items: vec![SelectItem::Expr { expr: Expr::Literal(Value::Int(1)), alias: None }],
                from: Some(derived(&q)),
                ..Select::default()
            }),
            negated: true,
        };
        Ok(Expr::Case { whens: vec![(empty, Expr::Literal(Value::Bool(true)))], else_expr: Some(Box::new(body)) })
    }

    /// What may follow a function call: `FILTER (WHERE ...)` and `OVER (...)`.
    fn call_suffixes(&mut self, mut call: Expr) -> Result<Expr> {
        if self.peek_kw("FILTER") && matches!(self.peek_at(1), Tok::Sym("(")) {
            self.advance();
            self.advance();
            self.expect_kw("WHERE")?;
            let cond = self.expr()?;
            self.expect_sym(")")?;
            if let Expr::Function { args, star, .. } = &mut call {
                let guard = |v: Expr| Expr::Function {
                    name: "IIF".into(),
                    args: vec![cond.clone(), v, Expr::Literal(Value::Null)],
                    distinct: false,
                    star: false,
                };
                if *star {
                    *star = false;
                    args.push(guard(Expr::Literal(Value::Int(1))));
                } else if let Some(a) = args.first_mut() {
                    let inner = std::mem::replace(a, Expr::Literal(Value::Null));
                    *a = guard(inner);
                }
            }
        }
        if self.peek_kw("WITHIN") {
            return Err(Error::syntax("WITHIN GROUP is not supported"));
        }
        if self.eat_kw("OVER") {
            let spec = self.window_spec()?;
            return Ok(Expr::Window { func: Box::new(call), spec });
        }
        Ok(call)
    }

    fn window_spec(&mut self) -> Result<WindowSpec> {
        if !self.peek_sym("(") {
            // `OVER w` names a window defined in the WINDOW clause.
            let name = self.ident()?;
            return Ok(WindowSpec { base: Some(name), ..WindowSpec::default() });
        }
        self.advance();
        let mut spec = WindowSpec::default();
        // `OVER (w ORDER BY x)` starts from the window w.
        if let Tok::Ident(w) = self.peek().clone() {
            let clause = ["PARTITION", "ORDER", "ROWS", "RANGE", "GROUPS"].iter().any(|k| w.eq_ignore_ascii_case(k));
            if !clause {
                self.advance();
                spec.base = Some(w);
            }
        }
        if self.eat_kw("PARTITION") {
            self.expect_kw("BY")?;
            loop {
                spec.partition_by.push(self.expr()?);
                if !self.eat_sym(",") {
                    break;
                }
            }
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
                let nulls_first = self.nulls_clause()?;
                spec.order_by.push(OrderItem { expr, desc, nulls_first });
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        let units = if self.eat_kw("ROWS") {
            Some(FrameUnits::Rows)
        } else if self.eat_kw("RANGE") {
            Some(FrameUnits::Range)
        } else if self.peek_kw("GROUPS") {
            return Err(Error::syntax("GROUPS frames are not supported"));
        } else {
            None
        };
        if let Some(units) = units {
            let range = units == FrameUnits::Range;
            let (start, end) = if self.eat_kw("BETWEEN") {
                let s = self.frame_bound(range)?;
                self.expect_kw("AND")?;
                (s, self.frame_bound(range)?)
            } else {
                (self.frame_bound(range)?, FrameBound::CurrentRow)
            };
            if self.eat_kw("EXCLUDE") {
                return Err(Error::syntax("frame EXCLUDE is not supported"));
            }
            spec.frame = Some(Frame { units, start, end });
        }
        self.expect_sym(")")?;
        Ok(spec)
    }

    fn frame_bound(&mut self, range: bool) -> Result<FrameBound> {
        if self.eat_kw("UNBOUNDED") {
            return Ok(if self.eat_kw("PRECEDING") {
                FrameBound::UnboundedPreceding
            } else {
                self.expect_kw("FOLLOWING")?;
                FrameBound::UnboundedFollowing
            });
        }
        if self.eat_kw("CURRENT") {
            self.expect_kw("ROW")?;
            return Ok(FrameBound::CurrentRow);
        }
        if range {
            // RANGE offsets are values (numbers, or intervals for dates), not row counts.
            let off = match self.unary()? {
                Expr::Literal(v) if !v.is_null() => v,
                _ => return Err(Error::syntax("a RANGE offset must be a constant such as 5 or INTERVAL '1 day'")),
            };
            return Ok(if self.eat_kw("PRECEDING") {
                FrameBound::RangePreceding(off)
            } else {
                self.expect_kw("FOLLOWING")?;
                FrameBound::RangeFollowing(off)
            });
        }
        let n = self.usize_literal()?;
        Ok(if self.eat_kw("PRECEDING") {
            FrameBound::Preceding(n)
        } else {
            self.expect_kw("FOLLOWING")?;
            FrameBound::Following(n)
        })
    }

    fn case_expr(&mut self) -> Result<Expr> {
        self.expect_kw("CASE")?;
        // `CASE x WHEN a THEN ...` compares x with each WHEN value.
        let operand = if self.peek_kw("WHEN") { None } else { Some(self.expr()?) };
        let mut whens = Vec::new();
        while self.eat_kw("WHEN") {
            let mut cond = self.expr()?;
            if let Some(x) = &operand {
                cond = Expr::Binary { op: BinOp::Eq, left: Box::new(x.clone()), right: Box::new(cond) };
            }
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

/// `CAST(e AS ty)` / `e::ty` as the function that does the conversion; a decimal scale rounds.
fn make_cast(e: Expr, ty: DataType, hints: &[i64]) -> Expr {
    let name = match ty {
        // CAST drops the fraction of a float; the Access function CInt() rounds
        DataType::Integer => "CAST_INTEGER",
        DataType::Bignum => "CAST_BIGNUM",
        _ => cast_function(ty).expect("not CALCULATED"),
    };
    let call = Expr::Function { name: name.into(), args: vec![e], distinct: false, star: false };
    match (ty, hints.get(1)) {
        (DataType::Decimal | DataType::Currency, Some(scale)) => Expr::Function {
            name: "ROUND".into(),
            args: vec![call, Expr::Literal(Value::Int(*scale))],
            distinct: false,
            star: false,
        },
        _ => call,
    }
}
