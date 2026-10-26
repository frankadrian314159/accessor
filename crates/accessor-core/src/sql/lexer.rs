//! Tokenizer. Understands Access conventions: `[bracketed names]`,
//! `#date#` literals, and double-quoted strings.

use crate::datetime;
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
    Date(i64),
    Sym(&'static str),
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

const SYMBOLS: [&str; 30] = [
    "!~*", "~*", "!~", "~", "@", "->>", "->", "::", ":=", "..", "<>", "<=", ">=", "!=", "(", ")", ",", ".", ";", "*", "+", "-", "/", "\\", "^", "&", "=", "<", ">", "%",
];

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
            if hex.len() % 2 != 0 {
                return Err(Error::syntax("an X'...' literal needs an even number of hexadecimal digits"));
            }
            let bytes: Vec<u8> = (0..hex.len()).step_by(2).map(|k| u8::from_str_radix(&hex[k..k + 2], 16).expect("hex digits")).collect();
            let text = String::from_utf8(bytes)
                .map_err(|_| Error::syntax("an X'...' literal is read as text, and these bytes are not valid UTF-8 (there is no BLOB type)"))?;
            out.push(Token { tok: Tok::Str(text), pos });
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
        } else if is_ident_start(c) {
            let start = i;
            while at(i).is_some_and(is_ident_part) {
                i += 1;
            }
            let end = chars.get(i).map_or(src.len(), |&(p, _)| p);
            let _ = start;
            out.push(Token { tok: Tok::Ident(src[pos..end].to_string()), pos });
        } else if c == '@' && at(i + 1).is_some_and(is_ident_start) {
            i += 1;
            let start = i;
            while at(i).is_some_and(is_ident_part) {
                i += 1;
            }
            let end = chars.get(i).map_or(src.len(), |&(p, _)| p);
            out.push(Token { tok: Tok::Param(src[chars[start].0..end].to_string()), pos });
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
                    Some(ch) => {
                        s.push(ch);
                        i += 1;
                    }
                }
            }
            i += 1;
            out.push(Token { tok: Tok::Str(s), pos });
        } else if c == '#' {
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
            // `||` (standard SQL) is accepted as a spelling of the concatenation operator &.
            if rest.starts_with("||") {
                i += 2;
                out.push(Token { tok: Tok::Sym("&"), pos });
                continue;
            }
            let sym = SYMBOLS
                .iter()
                .find(|s| rest.starts_with(**s))
                .ok_or_else(|| Error::syntax(format!("unexpected character '{c}'")))?;
            i += sym.chars().count();
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
