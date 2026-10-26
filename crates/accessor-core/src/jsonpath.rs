//! SQL/JSON path expressions: `lax $.a[*] ? (@ > 1).b`, as used by `jsonb_path_query` and its relatives.
//!
//! Supported: `lax`/`strict`; `$`, `@`, `$name` (variables); `.key`, `."key"`, `.*`, `.**`; `[n]`, `[a to b]`, `[*]`,
//! `last`; filters `? (...)` with comparisons, `&&`, `||`, `!`, `exists(...)`, `like_regex "..." [flag "i"]`,
//! `starts with`, `is unknown`; arithmetic; the methods `type()`, `size()`, `abs()`, `floor()`, `ceiling()`, `double()`
//! and `keyvalue()`. Not supported: `datetime()` and the other date methods, `.**{levels}`, `.decimal()` and friends.

use serde_json::Value as Json;

use crate::error::{Error, Result};

#[derive(Clone, Debug)]
pub struct Path {
    strict: bool,
    expr: Expr,
}

#[derive(Clone, Debug)]
enum Root {
    Context,
    Current,
    Var(String),
    /// A parenthesised expression the steps are applied to.
    Sub(Box<Expr>),
}

#[derive(Clone, Debug)]
enum Idx {
    At(i64),
    Last(i64),
}

#[derive(Clone, Debug)]
enum Step {
    Key(String),
    Wild,
    Descend,
    All,
    Index(Vec<(Idx, Option<Idx>)>),
    Filter(Box<Expr>),
    Method(String),
}

#[derive(Clone, Debug)]
enum Expr {
    Lit(Json),
    Path(Root, Vec<Step>),
    Neg(Box<Expr>),
    Arith(char, Box<Expr>, Box<Expr>),
    Cmp(String, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Exists(Box<Expr>),
    Like(Box<Expr>, String, bool),
    Starts(Box<Expr>, Box<Expr>),
    Unknown(Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Dollar,
    At,
    Var(String),
    Word(String),
    Str(String),
    Num(String),
    Sym(&'static str),
}

fn syntax(path: &str) -> Error {
    Error::syntax(format!("syntax error in jsonpath \"{path}\""))
}

fn lex(src: &str) -> Result<Vec<Tok>> {
    let c: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    const SYMS: [&str; 22] = ["&&", "||", "==", "!=", "<>", "<=", ">=", "**", "<", ">", "!", "+", "-", "*", "/", "%", ".", "[", "]", "(", ")", ","];
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '$' {
            i += 1;
            let start = i;
            if i < c.len() && c[i] == '"' {
                i += 1;
                let mut name = String::new();
                while i < c.len() && c[i] != '"' {
                    name.push(c[i]);
                    i += 1;
                }
                i += 1;
                out.push(Tok::Var(name));
            } else {
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                    i += 1;
                }
                if i == start {
                    out.push(Tok::Dollar);
                } else {
                    out.push(Tok::Var(c[start..i].iter().collect()));
                }
            }
        } else if ch == '@' {
            out.push(Tok::At);
            i += 1;
        } else if ch == '?' {
            out.push(Tok::Sym("?"));
            i += 1;
        } else if ch == '"' {
            i += 1;
            let mut s = String::new();
            while i < c.len() && c[i] != '"' {
                if c[i] == '\\' && i + 1 < c.len() {
                    i += 1;
                    s.push(match c[i] {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                } else {
                    s.push(c[i]);
                }
                i += 1;
            }
            if i >= c.len() {
                return Err(syntax(src));
            }
            i += 1;
            out.push(Tok::Str(s));
        } else if ch.is_ascii_digit() || (ch == '.' && i + 1 < c.len() && c[i + 1].is_ascii_digit() && !matches!(out.last(), Some(Tok::Dollar | Tok::At | Tok::Sym("]" | ")")) | Some(Tok::Word(_)) | Some(Tok::Str(_)))) {
            let start = i;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.' || ((c[i] == 'e' || c[i] == 'E') && i + 1 < c.len()) || ((c[i] == '+' || c[i] == '-') && matches!(c[i - 1], 'e' | 'E'))) {
                i += 1;
            }
            out.push(Tok::Num(c[start..i].iter().collect()));
        } else if ch.is_alphabetic() || ch == '_' {
            let start = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok::Word(c[start..i].iter().collect()));
        } else {
            let rest: String = c[i..].iter().take(2).collect();
            let Some(sym) = SYMS.iter().find(|s| rest.starts_with(**s)) else { return Err(syntax(src)) };
            i += sym.chars().count();
            out.push(Tok::Sym(sym));
        }
    }
    Ok(out)
}

struct P<'a> {
    src: &'a str,
    t: Vec<Tok>,
    i: usize,
}

impl P<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn err(&self) -> Error {
        syntax(self.src)
    }
    fn sym(&mut self, s: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Sym(x)) if *x == s) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn word(&mut self, w: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Word(x)) if x == w) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect_sym(&mut self, s: &str) -> Result<()> {
        if self.sym(s) { Ok(()) } else { Err(self.err()) }
    }

    fn or(&mut self) -> Result<Expr> {
        let mut l = self.and()?;
        while self.sym("||") {
            l = Expr::Or(Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr> {
        let mut l = self.not()?;
        while self.sym("&&") {
            l = Expr::And(Box::new(l), Box::new(self.not()?));
        }
        Ok(l)
    }
    fn not(&mut self) -> Result<Expr> {
        if self.sym("!") {
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<Expr> {
        let l = self.add()?;
        for op in ["==", "!=", "<>", "<=", ">=", "<", ">"] {
            if self.sym(op) {
                let r = self.add()?;
                let op = if op == "<>" { "!=" } else { op };
                return Ok(Expr::Cmp(op.to_string(), Box::new(l), Box::new(r)));
            }
        }
        if self.word("like_regex") {
            let Some(Tok::Str(pat)) = self.peek().cloned() else { return Err(self.err()) };
            self.i += 1;
            let mut ci = false;
            if self.word("flag") {
                let Some(Tok::Str(f)) = self.peek().cloned() else { return Err(self.err()) };
                self.i += 1;
                ci = f.contains('i');
            }
            return Ok(Expr::Like(Box::new(l), pat, ci));
        }
        if self.word("starts") {
            if !self.word("with") {
                return Err(self.err());
            }
            return Ok(Expr::Starts(Box::new(l), Box::new(self.add()?)));
        }
        if self.word("is") {
            if !self.word("unknown") {
                return Err(self.err());
            }
            return Ok(Expr::Unknown(Box::new(l)));
        }
        Ok(l)
    }
    fn add(&mut self) -> Result<Expr> {
        let mut l = self.mul()?;
        loop {
            let op = if self.sym("+") { '+' } else if self.sym("-") { '-' } else { break };
            l = Expr::Arith(op, Box::new(l), Box::new(self.mul()?));
        }
        Ok(l)
    }
    fn mul(&mut self) -> Result<Expr> {
        let mut l = self.unary()?;
        loop {
            let op = if self.sym("*") { '*' } else if self.sym("/") { '/' } else if self.sym("%") { '%' } else { break };
            l = Expr::Arith(op, Box::new(l), Box::new(self.unary()?));
        }
        Ok(l)
    }
    fn unary(&mut self) -> Result<Expr> {
        if self.sym("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.sym("+") {
            return self.unary();
        }
        self.primary()
    }
    fn primary(&mut self) -> Result<Expr> {
        let tok = self.peek().cloned().ok_or_else(|| self.err())?;
        self.i += 1;
        let root = match tok {
            Tok::Dollar => Root::Context,
            Tok::At => Root::Current,
            Tok::Var(n) => Root::Var(n),
            Tok::Num(n) => return Ok(Expr::Lit(number(&n).ok_or_else(|| self.err())?)),
            Tok::Str(s) => return Ok(Expr::Lit(Json::String(s))),
            Tok::Word(w) => match w.as_str() {
                "true" => return Ok(Expr::Lit(Json::Bool(true))),
                "false" => return Ok(Expr::Lit(Json::Bool(false))),
                "null" => return Ok(Expr::Lit(Json::Null)),
                "exists" => {
                    self.expect_sym("(")?;
                    let e = self.or()?;
                    self.expect_sym(")")?;
                    return Ok(Expr::Exists(Box::new(e)));
                }
                _ => return Err(self.err()),
            },
            Tok::Sym("(") => {
                let e = self.or()?;
                self.expect_sym(")")?;
                // a parenthesised expression may be followed by accessors: ($.a + 1).type()
                return self.steps_after(e);
            }
            _ => return Err(self.err()),
        };
        let mut steps = Vec::new();
        self.accessors(&mut steps)?;
        Ok(Expr::Path(root, steps))
    }
    fn steps_after(&mut self, e: Expr) -> Result<Expr> {
        let mut steps = Vec::new();
        self.accessors(&mut steps)?;
        if steps.is_empty() {
            return Ok(e);
        }
        // evaluate e, then apply the steps to each result: modelled as a path over a literal-producing sub-expression
        Ok(Expr::Path(Root::Sub(Box::new(e)), steps))
    }
    fn accessors(&mut self, steps: &mut Vec<Step>) -> Result<()> {
        loop {
            if self.sym(".") {
                match self.peek().cloned() {
                    Some(Tok::Word(w)) => {
                        self.i += 1;
                        if self.sym("(") {
                            self.expect_sym(")")?;
                            if !matches!(w.as_str(), "type" | "size" | "abs" | "floor" | "ceiling" | "double" | "keyvalue") {
                                return Err(Error::syntax(format!("the jsonpath method .{w}() is not supported")));
                            }
                            steps.push(Step::Method(w));
                        } else {
                            steps.push(Step::Key(w));
                        }
                    }
                    Some(Tok::Str(s)) => {
                        self.i += 1;
                        steps.push(Step::Key(s));
                    }
                    Some(Tok::Sym("*")) => {
                        self.i += 1;
                        steps.push(Step::Wild);
                    }
                    Some(Tok::Sym("**")) => {
                        self.i += 1;
                        steps.push(Step::Descend);
                    }
                    _ => return Err(self.err()),
                }
            } else if self.sym("[") {
                if self.sym("*") {
                    self.expect_sym("]")?;
                    steps.push(Step::All);
                    continue;
                }
                let mut ranges = Vec::new();
                loop {
                    let a = self.idx()?;
                    let b = if self.word("to") { Some(self.idx()?) } else { None };
                    ranges.push((a, b));
                    if !self.sym(",") {
                        break;
                    }
                }
                self.expect_sym("]")?;
                steps.push(Step::Index(ranges));
            } else if self.sym("?") {
                self.expect_sym("(")?;
                let e = self.or()?;
                self.expect_sym(")")?;
                steps.push(Step::Filter(Box::new(e)));
            } else {
                return Ok(());
            }
        }
    }
    fn idx(&mut self) -> Result<Idx> {
        if self.word("last") {
            let off = if self.sym("-") {
                let Some(Tok::Num(n)) = self.peek().cloned() else { return Err(self.err()) };
                self.i += 1;
                -n.parse::<i64>().map_err(|_| self.err())?
            } else if self.sym("+") {
                let Some(Tok::Num(n)) = self.peek().cloned() else { return Err(self.err()) };
                self.i += 1;
                n.parse::<i64>().map_err(|_| self.err())?
            } else {
                0
            };
            return Ok(Idx::Last(off));
        }
        let neg = self.sym("-");
        let Some(Tok::Num(n)) = self.peek().cloned() else { return Err(self.err()) };
        self.i += 1;
        let v: i64 = n.parse::<f64>().map_err(|_| self.err())? as i64;
        Ok(Idx::At(if neg { -v } else { v }))
    }
}

fn number(s: &str) -> Option<Json> {
    if let Ok(i) = s.parse::<i64>() {
        return Some(Json::from(i));
    }
    s.parse::<f64>().ok().and_then(|f| serde_json::Number::from_f64(f).map(Json::Number))
}

/// Reads a path.
pub fn parse(src: &str) -> Result<Path> {
    let mut t = lex(src)?;
    let mut strict = false;
    if matches!(t.first(), Some(Tok::Word(w)) if w == "strict" || w == "lax") {
        strict = matches!(t.remove(0), Tok::Word(w) if w == "strict");
    }
    let mut p = P { src, t, i: 0 };
    let expr = p.or()?;
    if p.i != p.t.len() {
        return Err(syntax(src));
    }
    Ok(Path { strict, expr })
}

struct Ctx<'a> {
    root: &'a Json,
    vars: &'a Json,
    strict: bool,
}

fn type_name(v: &Json) -> &'static str {
    match v {
        Json::Null => "null",
        Json::Bool(_) => "boolean",
        Json::Number(_) => "number",
        Json::String(_) => "string",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    }
}

fn as_f64(v: &Json) -> Option<f64> {
    v.as_f64()
}

fn from_f64(f: f64) -> Json {
    if f.fract() == 0.0 && f.abs() < 9e15 {
        Json::from(f as i64)
    } else {
        serde_json::Number::from_f64(f).map_or(Json::Null, Json::Number)
    }
}

impl Ctx<'_> {
    fn fail<T>(&self, msg: impl Into<String>) -> Result<T> {
        Err(Error::runtime(msg.into()))
    }

    /// The items an expression produces for the current item `cur`.
    fn items(&self, e: &Expr, cur: &Json) -> Result<Vec<Json>> {
        Ok(match e {
            Expr::Lit(v) => vec![v.clone()],
            Expr::Path(root, steps) => {
                let start = match root {
                    Root::Context => vec![self.root.clone()],
                    Root::Current => vec![cur.clone()],
                    Root::Sub(e) => self.items(e, cur)?,
                    Root::Var(n) => match self.vars.get(n) {
                        Some(v) => vec![v.clone()],
                        None => return self.fail(format!("could not find jsonpath variable \"{n}\"")),
                    },
                };
                let mut seq = start;
                for s in steps {
                    seq = self.step(s, seq)?;
                }
                seq
            }
            Expr::Neg(x) => {
                let mut out = Vec::new();
                for v in self.items(x, cur)? {
                    match as_f64(&v) {
                        Some(f) => out.push(from_f64(-f)),
                        None => return self.fail("operand of unary jsonpath operator - is not a numeric value"),
                    }
                }
                out
            }
            Expr::Arith(op, a, b) => {
                let (l, r) = (self.items(a, cur)?, self.items(b, cur)?);
                if l.len() != 1 || r.len() != 1 {
                    return self.fail(format!("left operand of jsonpath operator {op} is not a single numeric value"));
                }
                let (Some(x), Some(y)) = (as_f64(&l[0]), as_f64(&r[0])) else {
                    return self.fail(format!("operand of jsonpath operator {op} is not a numeric value"));
                };
                let f = match op {
                    '+' => x + y,
                    '-' => x - y,
                    '*' => x * y,
                    '/' => {
                        if y == 0.0 {
                            return self.fail("division by zero");
                        }
                        x / y
                    }
                    _ => {
                        if y == 0.0 {
                            return self.fail("division by zero");
                        }
                        x % y
                    }
                };
                vec![from_f64(f)]
            }
            predicate => match self.truth(predicate, cur)? {
                Some(b) => vec![Json::Bool(b)],
                None => vec![Json::Null],
            },
        })
    }

    fn step(&self, s: &Step, seq: Vec<Json>) -> Result<Vec<Json>> {
        let mut out = Vec::new();
        for v in seq {
            match s {
                Step::Key(k) => match &v {
                    Json::Object(m) => match m.get(k) {
                        Some(x) => out.push(x.clone()),
                        None if self.strict => return self.fail(format!("JSON object does not contain key \"{k}\"")),
                        None => {}
                    },
                    Json::Array(a) if !self.strict => {
                        for item in a {
                            if let Json::Object(m) = item {
                                if let Some(x) = m.get(k) {
                                    out.push(x.clone());
                                }
                            }
                        }
                    }
                    _ if self.strict => return self.fail("jsonpath member accessor can only be applied to an object"),
                    _ => {}
                },
                Step::Wild => match &v {
                    Json::Object(m) => out.extend(m.values().cloned()),
                    Json::Array(a) if !self.strict => {
                        for item in a {
                            if let Json::Object(m) = item {
                                out.extend(m.values().cloned());
                            }
                        }
                    }
                    _ if self.strict => return self.fail("jsonpath wildcard member accessor can only be applied to an object"),
                    _ => {}
                },
                Step::Descend => descend(&v, &mut out),
                Step::All => match &v {
                    Json::Array(a) => out.extend(a.iter().cloned()),
                    _ if self.strict => return self.fail("jsonpath wildcard array accessor can only be applied to an array"),
                    _ => out.push(v.clone()),
                },
                Step::Index(ranges) => {
                    let arr: Vec<Json> = match &v {
                        Json::Array(a) => a.clone(),
                        _ if self.strict => return self.fail("jsonpath array accessor can only be applied to an array"),
                        other => vec![other.clone()],
                    };
                    let n = arr.len() as i64;
                    let at = |i: &Idx| match i {
                        Idx::At(x) => *x,
                        Idx::Last(off) => n - 1 + off,
                    };
                    for (a, b) in ranges {
                        let (lo, hi) = (at(a), b.as_ref().map_or(at(a), at));
                        if self.strict && (lo < 0 || hi >= n || lo > hi) {
                            return self.fail("jsonpath array subscript is out of bounds");
                        }
                        for i in lo.max(0)..=hi.min(n - 1) {
                            out.push(arr[i as usize].clone());
                        }
                    }
                }
                Step::Filter(cond) => {
                    let items: Vec<Json> = match &v {
                        Json::Array(a) if !self.strict => a.clone(),
                        other => vec![other.clone()],
                    };
                    for item in items {
                        if self.truth(cond, &item)? == Some(true) {
                            out.push(item);
                        }
                    }
                }
                Step::Method(m) => match m.as_str() {
                    "type" => out.push(Json::String(type_name(&v).into())),
                    "size" => out.push(Json::from(match &v {
                        Json::Array(a) => a.len() as i64,
                        _ => 1,
                    })),
                    "keyvalue" => match &v {
                        Json::Object(map) => {
                            for (i, (k, x)) in map.iter().enumerate() {
                                out.push(serde_json::json!({"key": k, "value": x, "id": i}));
                            }
                        }
                        _ => return self.fail("jsonpath item method .keyvalue() can only be applied to an object"),
                    },
                    name => {
                        let items: Vec<Json> = match &v {
                            Json::Array(a) if !self.strict => a.clone(),
                            other => vec![other.clone()],
                        };
                        for item in items {
                            let f = match &item {
                                Json::String(s) if name == "double" => s.trim().parse::<f64>().ok(),
                                other => as_f64(other),
                            };
                            let Some(f) = f else {
                                return self.fail(format!("jsonpath item method .{name}() can only be applied to a numeric value"));
                            };
                            out.push(from_f64(match name {
                                "abs" => f.abs(),
                                "floor" => f.floor(),
                                "ceiling" => f.ceil(),
                                _ => f,
                            }));
                        }
                    }
                },
            }
        }
        Ok(out)
    }

    /// Three-valued truth of a predicate for the current item.
    fn truth(&self, e: &Expr, cur: &Json) -> Result<Option<bool>> {
        Ok(match e {
            Expr::And(a, b) => match (self.truth(a, cur)?, self.truth(b, cur)?) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            Expr::Or(a, b) => match (self.truth(a, cur)?, self.truth(b, cur)?) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
            Expr::Not(a) => self.truth(a, cur)?.map(|b| !b),
            Expr::Unknown(a) => Some(self.truth(a, cur)?.is_none()),
            Expr::Exists(a) => match self.items(a, cur) {
                Ok(v) => Some(!v.is_empty()),
                Err(_) => None,
            },
            Expr::Cmp(op, a, b) => {
                let (l, r) = match (self.items(a, cur), self.items(b, cur)) {
                    (Ok(l), Ok(r)) => (l, r),
                    _ => return Ok(None),
                };
                let mut unknown = false;
                let mut found = false;
                for x in &l {
                    for y in &r {
                        match compare(x, y) {
                            Some(o) => {
                                let hit = match op.as_str() {
                                    "==" => o == std::cmp::Ordering::Equal,
                                    "!=" => o != std::cmp::Ordering::Equal,
                                    "<" => o == std::cmp::Ordering::Less,
                                    "<=" => o != std::cmp::Ordering::Greater,
                                    ">" => o == std::cmp::Ordering::Greater,
                                    _ => o != std::cmp::Ordering::Less,
                                };
                                if hit {
                                    found = true;
                                }
                            }
                            None => unknown = true,
                        }
                    }
                }
                if found { Some(true) } else if unknown { None } else { Some(false) }
            }
            Expr::Like(a, pat, ci) => {
                let items = match self.items(a, cur) {
                    Ok(v) => v,
                    Err(_) => return Ok(None),
                };
                let re = regex::RegexBuilder::new(pat).case_insensitive(*ci).build().map_err(|e| Error::syntax(format!("invalid regular expression: {e}")))?;
                let mut unknown = false;
                let mut found = false;
                for x in &items {
                    match x {
                        Json::String(s) => found |= re.is_match(s),
                        _ => unknown = true,
                    }
                }
                if found { Some(true) } else if unknown { None } else { Some(false) }
            }
            Expr::Starts(a, b) => {
                let (l, r) = match (self.items(a, cur), self.items(b, cur)) {
                    (Ok(l), Ok(r)) => (l, r),
                    _ => return Ok(None),
                };
                let mut unknown = false;
                let mut found = false;
                for x in &l {
                    for y in &r {
                        match (x, y) {
                            (Json::String(s), Json::String(p)) => found |= s.starts_with(p.as_str()),
                            _ => unknown = true,
                        }
                    }
                }
                if found { Some(true) } else if unknown { None } else { Some(false) }
            }
            other => {
                // a value used as a condition: only a boolean is one
                match self.items(other, cur)?.as_slice() {
                    [Json::Bool(b)] => Some(*b),
                    _ => None,
                }
            }
        })
    }
}

fn descend(v: &Json, out: &mut Vec<Json>) {
    out.push(v.clone());
    match v {
        Json::Object(m) => m.values().for_each(|x| descend(x, out)),
        Json::Array(a) => a.iter().for_each(|x| descend(x, out)),
        _ => {}
    }
}

fn compare(a: &Json, b: &Json) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Json::Null, Json::Null) => Some(std::cmp::Ordering::Equal),
        (Json::Bool(x), Json::Bool(y)) => Some(x.cmp(y)),
        (Json::Number(_), Json::Number(_)) => as_f64(a)?.partial_cmp(&as_f64(b)?),
        (Json::String(x), Json::String(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

/// The items `path` selects in `doc`. `vars` is a JSON object of the path variables. With `silent`, errors give no
/// items.
pub fn query(doc: &Json, path: &Path, vars: Option<&Json>, silent: bool) -> Result<Vec<Json>> {
    let empty = Json::Object(serde_json::Map::new());
    let ctx = Ctx { root: doc, vars: vars.unwrap_or(&empty), strict: path.strict };
    match ctx.items(&path.expr, doc) {
        Ok(v) => Ok(v),
        Err(_) if silent => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}
