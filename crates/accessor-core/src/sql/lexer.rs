//! Tokenizer. Understands Access conventions: `[bracketed names]`,
//! `#date#` literals, and double-quoted strings.

use crate::datetime;
use crate::dialect::{self, Dialect};
use num_bigint::BigInt;

use crate::decimal::Decimal;
use crate::error::{Error, Result};

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    /// `[Bracketed Name]` — never treated as a keyword.
    Quoted(String),
    Int(i64),
    Dec(Decimal),
    /// An integer literal too large for an INTEGER.
    Big(BigInt),
    /// A literal with an exponent.
    Float(f64),
    Str(String),
    /// An `X'..'` literal whose bytes are not valid UTF-8.
    Bytes(Vec<u8>),
    Date(i64),
    Sym(&'static str),
    /// A user-defined operator (`CREATE OPERATOR <<< (...)`), as it is spelled.
    Op(String),
    /// `@name` — a query parameter.
    Param(String),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    /// Byte offset of the token's first character.
    pub pos: usize,
}

const SYMBOLS: [&str; 38] = [
    "#", "<=>", "@?", "@@", "<<", ">>", "!~*", "~*", "!~", "~", "@", "->>", "->", "::", ":=", ":", "..", "<>", "<=", ">=", "!=", "(", ")", ",", ".", ";", "*", "+", "-", "/", "\\", "^", "&", "|", "=", "<", ">", "%",
];

thread_local! {
    /// The operators `CREATE OPERATOR` has named, so the later statements read them as one token.
    static CUSTOM_OPS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn is_op_char(c: char) -> bool {
    "+-*/<>=~!@#%^&|`?".contains(c)
}

/// The operator at the start of `rest`, as PostgreSQL reads it: the longest run of operator characters (not into a
/// comment), without a trailing `+` or `-` unless it holds one of `~ ! @ # % ^ & | ` ?`.
fn op_run(rest: &str) -> &str {
    let mut n = 0;
    for (k, c) in rest.char_indices() {
        if !is_op_char(c) || rest[k..].starts_with("--") || rest[k..].starts_with("/*") {
            break;
        }
        n = k + 1;
    }
    let mut run = &rest[..n];
    if run.len() > 1 && !run.chars().any(|c| "~!@#%^&|`?".contains(c)) {
        while run.len() > 1 && (run.ends_with('+') || run.ends_with('-')) {
            run = &run[..run.len() - 1];
        }
    }
    run
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}
fn is_ident_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// `$$` or `$tag$` at `i`: the opening delimiter as text.
fn dollar_tag(chars: &[(usize, char)], i: usize) -> Option<String> {
    let mut j = i + 1;
    let mut tag = String::from("$");
    while let Some(&(_, c)) = chars.get(j) {
        if c == '$' {
            tag.push('$');
            return Some(tag);
        }
        if !(c.is_alphanumeric() || c == '_') || (j == i + 1 && c.is_ascii_digit()) {
            return None;
        }
        tag.push(c);
        j += 1;
    }
    None
}

pub fn lex(src: &str) -> Result<Vec<Token>> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut out = Vec::new();
    // inside a CREATE / DROP / ALTER OPERATOR statement every run of operator characters is an operator
    let mut in_operator_statement = false;
    // how many array brackets are open: inside them `[` is never the start of an [identifier]
    let mut arrays = 0usize;
    let mut question = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let (pos, c) = chars[i];
        let at = |j: usize| chars.get(j).map(|&(_, c)| c);
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && at(i + 1) == Some('-') {
            while i < chars.len() && chars[i].1 != '\n' {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == Some('*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i].1 == '*' && chars[i + 1].1 == '/') {
                i += 1;
            }
            if i + 1 >= chars.len() {
                return Err(Error::syntax("unterminated /* comment"));
            }
            i += 2;
        } else if c == '0' && matches!(at(i + 1), Some('x')) && at(i + 2).is_some_and(|d| d.is_ascii_hexdigit()) {
            // 0x7FFF: a number written in hexadecimal
            i += 2;
            let start = i;
            while at(i).is_some_and(|d| d.is_ascii_hexdigit()) {
                i += 1;
            }
            let digits: String = chars[start..i].iter().map(|&(_, ch)| ch).collect();
            if dialect::current() == Dialect::Tsql {
                // 0x1F is binary data in T-SQL
                let padded = if digits.len() % 2 == 1 { format!("0{digits}") } else { digits.clone() };
                let bytes: Vec<u8> = (0..padded.len()).step_by(2).filter_map(|k| u8::from_str_radix(&padded[k..k + 2], 16).ok()).collect();
                out.push(Token { tok: Tok::Bytes(bytes), pos });
                continue;
            }
            let tok = match i64::from_str_radix(&digits, 16) {
                Ok(v) => Tok::Int(v),
                Err(_) => Tok::Big(BigInt::parse_bytes(digits.as_bytes(), 16).ok_or_else(|| Error::syntax("bad hexadecimal number"))?),
            };
            out.push(Token { tok, pos });
        } else if c.is_ascii_digit() || (c == '.' && at(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            let mut has_point = false;
            let mut has_exponent = false;
            while at(i).is_some_and(|d| d.is_ascii_digit()) {
                i += 1;
            }
            if at(i) == Some('.') && at(i + 1).is_some_and(|d| d.is_ascii_digit()) || (at(i) == Some('.') && i == start) {
                has_point = true;
                i += 1;
                while at(i).is_some_and(|d| d.is_ascii_digit()) {
                    i += 1;
                }
            }
            if matches!(at(i), Some('e' | 'E'))
                && (at(i + 1).is_some_and(|d| d.is_ascii_digit())
                    || (matches!(at(i + 1), Some('+' | '-')) && at(i + 2).is_some_and(|d| d.is_ascii_digit())))
            {
                has_exponent = true;
                i += 2;
                while at(i).is_some_and(|d| d.is_ascii_digit()) {
                    i += 1;
                }
            }
            let end = chars.get(i).map_or(src.len(), |&(p, _)| p);
            let text = &src[pos..end];
            let bad = || Error::syntax(format!("bad number '{text}'"));
            // Whole numbers are INTEGERs (BIGNUMs when too large), numbers with a point are
            // exact DECIMALs, and an exponent makes the literal a DOUBLE.
            let tok = if has_exponent {
                Tok::Float(text.parse::<f64>().ok().filter(|f| f.is_finite()).ok_or_else(bad)?)
            } else if has_point {
                Tok::Dec(Decimal::parse(text).ok_or_else(bad)?)
            } else {
                match text.parse::<i64>() {
                    Ok(v) => Tok::Int(v),
                    Err(_) => Tok::Big(text.parse::<BigInt>().map_err(|_| bad())?),
                }
            };
            out.push(Token { tok, pos });
        } else if (c == 'x' || c == 'X') && at(i + 1) == Some('\'') {
            // X'303132': a literal written in hexadecimal. There is no BLOB type: the bytes must be text.
            let mut hex = String::new();
            i += 2;
            loop {
                match at(i) {
                    None => return Err(Error::syntax("unterminated X'...' literal")),
                    Some('\'') => break,
                    Some(ch) if ch.is_ascii_hexdigit() => hex.push(ch),
                    Some(ch) => return Err(Error::syntax(format!("'{ch}' is not a hexadecimal digit in an X'...' literal"))),
                }
                i += 1;
            }
            i += 1;
            if hex.len() % 2 != 0 && crate::dialect::current() == crate::dialect::Dialect::Postgresql {
                // PostgreSQL: a bit string; x'20000' is used as the number it spells
                let v = i64::from_str_radix(&hex, 16).map_err(|_| Error::syntax("the X'...' literal is too large"))?;
                out.push(Token { tok: Tok::Int(v), pos });
                continue;
            }
            if hex.len() % 2 != 0 {
                return Err(Error::syntax("an X'...' literal needs an even number of hexadecimal digits"));
            }
            let bytes: Vec<u8> = (0..hex.len()).step_by(2).map(|k| u8::from_str_radix(&hex[k..k + 2], 16).expect("hex digits")).collect();
            match String::from_utf8(bytes) {
                Ok(text) => out.push(Token { tok: Tok::Str(text), pos }),
                Err(e) => out.push(Token { tok: Tok::Bytes(e.into_bytes()), pos }),
            }
        } else if c == '$' && at(i + 1).is_some_and(|d| d.is_ascii_digit()) {
            // `$1`: the first argument of a PostgreSQL-style function body
            i += 1;
            let start = i;
            while at(i).is_some_and(|d| d.is_ascii_digit()) {
                i += 1;
            }
            let end = chars.get(i).map_or(src.len(), |&(p, _)| p);
            out.push(Token { tok: Tok::Param(format!("${}", &src[chars[start].0..end])), pos });
        } else if c == '$' && dollar_tag(&chars, i).is_some() {
            // `$$ text $$` or `$tag$ text $tag$`: a string in which nothing needs escaping
            let tag = dollar_tag(&chars, i).expect("checked");
            let open_len = tag.chars().count();
            let body_start = i + open_len;
            let mut j = body_start;
            let tag_chars: Vec<char> = tag.chars().collect();
            let mut found = None;
            while j + open_len <= chars.len() {
                if (0..open_len).all(|k| chars[j + k].1 == tag_chars[k]) {
                    found = Some(j);
                    break;
                }
                j += 1;
            }
            let Some(close) = found else { return Err(Error::syntax(format!("unterminated {tag} string"))) };
            let from = chars[body_start].0.min(src.len());
            let to = chars[close].0;
            out.push(Token { tok: Tok::Str(src[if body_start >= chars.len() { src.len() } else { from }..to].to_string()), pos });
            i = close + open_len;
        } else if matches!(c, 'N' | 'n') && at(i + 1) == Some('\'') && dialect::current() == Dialect::Tsql && !(i > 0 && is_ident_part(chars[i - 1].1)) {
            // N'text' is text: the N says it is Unicode, which all text is here
            i += 1;
        } else if matches!(c, 'e' | 'E' | 'b' | 'B') && at(i + 1) == Some('\'') && dialect::current() != Dialect::Mysql && !(i > 0 && is_ident_part(chars[i - 1].1)) {
            // E'..' (backslash escapes) and B'0101' (a bit string, kept as its digits)
            let escapes = matches!(c, 'e' | 'E');
            let mut s = String::new();
            i += 2;
            loop {
                match at(i) {
                    None => return Err(Error::syntax("unterminated string literal")),
                    Some('\'') if at(i + 1) == Some('\'') => {
                        s.push('\'');
                        i += 2;
                    }
                    Some('\'') => break,
                    Some('\\') if escapes && at(i + 1).is_some() => {
                        let e = at(i + 1).expect("checked");
                        i += 2;
                        let digits = |max: usize, radix: u32, i: &mut usize| {
                            let mut v = 0u32;
                            let mut n = 0;
                            while n < max {
                                match at(*i).and_then(|d| d.to_digit(radix)) {
                                    Some(d) => {
                                        v = v * radix + d;
                                        *i += 1;
                                        n += 1;
                                    }
                                    None => break,
                                }
                            }
                            (v, n)
                        };
                        match e {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            'r' => s.push('\r'),
                            'b' => s.push('\u{8}'),
                            'f' => s.push('\u{c}'),
                            'x' => {
                                let (v, n) = digits(2, 16, &mut i);
                                if n == 0 {
                                    s.push('x');
                                } else if let Some(ch) = char::from_u32(v) {
                                    s.push(ch);
                                }
                            }
                            'u' | 'U' => {
                                let (v, n) = digits(if e == 'u' { 4 } else { 8 }, 16, &mut i);
                                match char::from_u32(v) {
                                    Some(ch) if n > 0 => s.push(ch),
                                    _ => return Err(Error::syntax("invalid Unicode escape")),
                                }
                            }
                            '0'..='7' => {
                                i -= 1;
                                let (v, _) = digits(3, 8, &mut i);
                                if let Some(ch) = char::from_u32(v) {
                                    s.push(ch);
                                }
                            }
                            other => s.push(other),
                        }
                    }
                    Some(ch) => {
                        s.push(ch);
                        i += 1;
                    }
                }
            }
            i += 1;
            out.push(Token { tok: Tok::Str(s), pos });
        } else if c == '#' && dialect::current() == Dialect::Tsql && at(i + 1).is_some_and(|d| is_ident_start(d) || d == '#') {
            // #temp and ##global tables
            while at(i) == Some('#') {
                i += 1;
            }
            while at(i).is_some_and(is_ident_part) {
                i += 1;
            }
            let end = chars.get(i).map_or(src.len(), |&(p, _)| p);
            out.push(Token { tok: Tok::Ident(src[pos..end].to_string()), pos });
        } else if is_ident_start(c) {
            let start = i;
            while at(i).is_some_and(is_ident_part) {
                i += 1;
            }
            let end = chars.get(i).map_or(src.len(), |&(p, _)| p);
            let _ = start;
            out.push(Token { tok: Tok::Ident(src[pos..end].to_string()), pos });
        } else if c == '@' && dialect::current() != Dialect::Postgresql && at(i + 1).is_some_and(|d| is_ident_start(d) || (d.is_ascii_digit() && crate::dialect::current() == crate::dialect::Dialect::Mysql)) {
            i += 1;
            let start = i;
            while at(i).is_some_and(is_ident_part) {
                i += 1;
            }
            let end = chars.get(i).map_or(src.len(), |&(p, _)| p);
            out.push(Token { tok: Tok::Param(src[chars[start].0..end].to_string()), pos });
        } else if c == '`' {
            // MySQL's quoted identifier; `` inside stands for one backtick
            let mut name = String::new();
            i += 1;
            loop {
                match at(i) {
                    None => return Err(Error::syntax("unterminated `identifier`")),
                    Some('`') if at(i + 1) == Some('`') => {
                        name.push('`');
                        i += 1;
                    }
                    Some('`') => break,
                    Some(ch) => name.push(ch),
                }
                i += 1;
            }
            i += 1;
            out.push(Token { tok: Tok::Quoted(name), pos });
        } else if c == '?' {
            // a placeholder of a prepared statement: the first is ?1, the next ?2, ...
            question += 1;
            i += 1;
            out.push(Token { tok: Tok::Param(format!("?{question}")), pos });
        } else if c == '@' && at(i + 1) == Some('@') && at(i + 2).is_some_and(is_ident_start) {
            // @@name, @@global.name, @@session.name: a system variable
            i += 2;
            let mut start = i;
            while at(i).is_some_and(is_ident_part) {
                i += 1;
            }
            let mut end = chars.get(i).map_or(src.len(), |&(p, _)| p);
            let word = src[chars[start].0..end].to_ascii_lowercase();
            if matches!(word.as_str(), "global" | "session" | "local" | "persist" | "persist_only") && at(i) == Some('.') {
                i += 1;
                start = i;
                while at(i).is_some_and(is_ident_part) {
                    i += 1;
                }
                end = chars.get(i).map_or(src.len(), |&(p, _)| p);
                // a component's variable: @@global.component.name
                while at(i) == Some('.') && at(i + 1).is_some_and(is_ident_part) {
                    i += 1;
                    while at(i).is_some_and(is_ident_part) {
                        i += 1;
                    }
                    end = chars.get(i).map_or(src.len(), |&(p, _)| p);
                }
            }
            out.push(Token { tok: Tok::Param(format!("@@{}", &src[chars[start].0..end])), pos });
        } else if c == '[' && (arrays > 0 || (i > 0 && at(i - 1).is_some_and(|p| is_ident_part(p) || matches!(p, ')' | ']' | '"')))) {
            // an array subscript or type suffix (`a[1]`, `int[]`); `[name]` is an identifier unless it follows a name
            i += 1;
            arrays += 1;
            out.push(Token { tok: Tok::Sym("["), pos });
        } else if c == ']' {
            i += 1;
            arrays = arrays.saturating_sub(1);
            out.push(Token { tok: Tok::Sym("]"), pos });
        } else if c == '[' {
            let mut name = String::new();
            i += 1;
            loop {
                match at(i) {
                    None => return Err(Error::syntax("unterminated [identifier]")),
                    Some(']') => break,
                    Some(ch) => name.push(ch),
                }
                i += 1;
            }
            i += 1;
            out.push(Token { tok: Tok::Quoted(name), pos });
        } else if c == '\'' || c == '"' {
            let quote = c;
            let mut s = String::new();
            i += 1;
            loop {
                match at(i) {
                    None => return Err(Error::syntax("unterminated string literal")),
                    Some(ch) if ch == quote => {
                        if at(i + 1) == Some(quote) {
                            s.push(quote);
                            i += 2;
                        } else {
                            break;
                        }
                    }
                    Some('\\') if dialect::current() == Dialect::Mysql && at(i + 1).is_some() => {
                        // MySQL: \n \t \r \0 \b \Z, and a backslash before any other character is that character
                        // (\% and \_ keep the backslash, for LIKE)
                        let e = at(i + 1).expect("checked");
                        match e {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            'r' => s.push('\r'),
                            '0' => s.push('\0'),
                            'b' => s.push('\u{8}'),
                            'Z' => s.push('\u{1a}'),
                            '%' | '_' => {
                                s.push('\\');
                                s.push(e);
                            }
                            other => s.push(other),
                        }
                        i += 2;
                    }
                    Some(ch) => {
                        s.push(ch);
                        i += 1;
                    }
                }
            }
            i += 1;
            out.push(Token { tok: Tok::Str(s), pos });
        } else if c == '#' && hash_comment(&at, i) {
            // MySQL: `# ...` is a comment to the end of the line (a #date# literal has its closing # on the same line)
            while at(i).is_some_and(|ch| ch != '\n') {
                i += 1;
            }
        } else if c == '#' && dialect::current() == Dialect::Asql {
            let mut s = String::new();
            i += 1;
            loop {
                match at(i) {
                    None => return Err(Error::syntax("unterminated #date# literal")),
                    Some('#') => break,
                    Some(ch) => s.push(ch),
                }
                i += 1;
            }
            i += 1;
            let t = datetime::parse(&s).ok_or_else(|| Error::syntax(format!("invalid date literal #{s}#")))?;
            out.push(Token { tok: Tok::Date(t), pos });
        } else {
            let rest = &src[pos..];
            if dialect::current() == Dialect::Postgresql && is_op_char(c) {
                let run = op_run(rest);
                let named = matches!(
                    out.as_slice(),
                    [.., Token { tok: Tok::Ident(b), .. }, Token { tok: Tok::Ident(a), .. }]
                        if a.eq_ignore_ascii_case("OPERATOR") && ["CREATE", "DROP", "ALTER"].iter().any(|w| b.eq_ignore_ascii_case(w))
                ) || matches!(
                    out.as_slice(),
                    [.., Token { tok: Tok::Ident(b), .. }, Token { tok: Tok::Ident(a), .. }, Token { tok: Tok::Ident(_) | Tok::Quoted(_), .. }, Token { tok: Tok::Sym("."), .. }]
                        if a.eq_ignore_ascii_case("OPERATOR") && ["CREATE", "DROP", "ALTER"].iter().any(|w| b.eq_ignore_ascii_case(w))
                ) || matches!(
                    out.as_slice(),
                    [.., Token { tok: Tok::Ident(a), .. }, Token { tok: Tok::Ident(b), .. }, Token { tok: Tok::Ident(c2), .. }]
                        if c2.eq_ignore_ascii_case("EXISTS") && b.eq_ignore_ascii_case("IF") && a.eq_ignore_ascii_case("OPERATOR")
                );
                let first = named && !in_operator_statement;
                if first {
                    in_operator_statement = true;
                    // an operator that already means something (`=`, `<=`, `@>`) is never taken over
                    let builtin = SYMBOLS.contains(&run) || ["@>", "<@", "&&", "||", "-|-", "&<", "&>", "=>", ":="].contains(&run);
                    CUSTOM_OPS.with(|c| {
                        let mut c = c.borrow_mut();
                        if !builtin && !c.iter().any(|o| o == run) {
                            c.push(run.to_string());
                        }
                    });
                }
                let custom = first || (in_operator_statement && run.len() >= 2) || CUSTOM_OPS.with(|c| c.borrow().iter().any(|o| o == run));
                if custom {
                    i += run.chars().count();
                    out.push(Token { tok: Tok::Op(run.to_string()), pos });
                    continue;
                }
            }
            // `||` (standard SQL) is accepted as a spelling of the concatenation operator &.
            if rest.starts_with("||") {
                i += 2;
                out.push(Token { tok: Tok::Sym("||"), pos });
                continue;
            }
            if dialect::current() == Dialect::Postgresql {
                if let Some(sym) = ["-|-", "&<", "&>"].into_iter().find(|s| rest.starts_with(*s)) {
                    i += sym.len();
                    out.push(Token { tok: Tok::Sym(sym), pos });
                    continue;
                }
            }
            let array_op = match dialect::current() {
                Dialect::Mysql => None,
                // `&&` is a slip for `&` in asql (the error says so), so it means overlap in postgresql only
                d => ["@>", "<@", "&&"].into_iter().find(|s| {
                    rest.starts_with(*s) && !(d == Dialect::Asql && (*s == "&&" || (*s == "<@" && rest[2..].starts_with(is_ident_start))))
                }),
            };
            if let Some(sym) = array_op {
                i += 2;
                out.push(Token { tok: Tok::Sym(sym), pos });
                continue;
            }
            let sym = SYMBOLS
                .iter()
                .find(|s| rest.starts_with(**s))
                .ok_or_else(|| Error::syntax(format!("unexpected character '{c}'")))?;
            i += sym.chars().count();
            if *sym == ";" {
                in_operator_statement = false;
            }
            out.push(Token { tok: Tok::Sym(sym), pos });
        }
    }
    out.push(Token { tok: Tok::Eof, pos: src.len() });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        lex(s).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn basics() {
        assert_eq!(
            toks("SELECT [Order ID], 1.5e2 <> 'it''s' -- c"),
            vec![
                Tok::Ident("SELECT".into()),
                Tok::Quoted("Order ID".into()),
                Tok::Sym(","),
                Tok::Float(150.0),
                Tok::Sym("<>"),
                Tok::Str("it's".into()),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn dates_and_dots() {
        let t = toks("a.b #2020-01-02#");
        assert_eq!(t[1], Tok::Sym("."));
        assert!(matches!(t[3], Tok::Date(_)));
        assert!(lex("'open").is_err());
    }
}

#[cfg(test)]
mod number_literals {
    use super::*;

    fn one(s: &str) -> Tok {
        lex(s).unwrap().remove(0).tok
    }

    #[test]
    fn literals_have_the_type_they_look_like() {
        assert_eq!(one("42"), Tok::Int(42));
        assert_eq!(one("9223372036854775807"), Tok::Int(i64::MAX));
        assert!(matches!(one("9223372036854775808"), Tok::Big(_)));
        assert!(matches!(one("123456789012345678901234567890"), Tok::Big(_)));
        assert!(matches!(one("2.5"), Tok::Dec(_)));
        assert!(matches!(one("5.0"), Tok::Dec(_)), "a point makes it a DECIMAL even when whole");
        assert!(matches!(one(".25"), Tok::Dec(_)));
        assert_eq!(one("1e3"), Tok::Float(1000.0));
        assert_eq!(one("2.5E-1"), Tok::Float(0.25));
        assert!(lex("1e999").is_err(), "an infinite literal is refused");
    }
}

/// Does the `#` at `i` start a comment? In `mysql` always; in `asql` unless it opens a `#date#` literal; in `postgresql`
/// never (`#` is an operator there, which this does not read).
fn hash_comment(at: &dyn Fn(usize) -> Option<char>, i: usize) -> bool {
    match dialect::current() {
        Dialect::Mysql => true,
        Dialect::Asql => !closes_as_date(at, i),
        Dialect::Postgresql | Dialect::Tsql => false,
    }
}

/// Does the `#` at `i` open a `#date#` literal: a closing `#` on the same line, around a date?
fn closes_as_date(at: &dyn Fn(usize) -> Option<char>, i: usize) -> bool {
    let mut s = String::new();
    let mut j = i + 1;
    loop {
        match at(j) {
            Some('#') => return datetime::parse(&s).is_some(),
            Some('\n') | None => return false,
            Some(ch) => s.push(ch),
        }
        j += 1;
    }
}
