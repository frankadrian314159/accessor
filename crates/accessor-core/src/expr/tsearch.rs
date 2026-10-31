//! Full-text search, as PostgreSQL has it: `tsvector` and `tsquery`.
//!
//! Both are kept as text, in the form PostgreSQL prints them (`'cat':3 'fat':2`, `'fat' & 'cat'`), so a column of either
//! type is a text column; the functions here read that text, do the search and write it back. `to_tsvector` splits text
//! into words, lower-cases them, drops the stop words of the `english` configuration and stems the rest with the Snowball
//! English (Porter2) stemmer; the `simple` configuration (and any other) keeps the words as they are. Phrase searches
//! (`<->`) use the positions. Ranks follow PostgreSQL's formulas for `ts_rank`; `ts_rank_cd` gives the same numbers.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::value::Value;

pub const NAMES: &[(&str, usize, usize)] = &[
    ("TO_TSVECTOR", 1, 2),
    ("TO_TSQUERY", 1, 2),
    ("PLAINTO_TSQUERY", 1, 2),
    ("PHRASETO_TSQUERY", 1, 2),
    ("WEBSEARCH_TO_TSQUERY", 1, 2),
    ("SETWEIGHT", 2, 3),
    ("STRIP", 1, 1),
    ("TS_RANK", 2, 3),
    ("TS_RANK_CD", 2, 3),
    ("TS_HEADLINE", 2, 4),
    ("NUMNODE", 1, 1),
    ("QUERYTREE", 1, 1),
    ("TSVECTOR_TO_ARRAY", 1, 1),
    ("ARRAY_TO_TSVECTOR", 1, 1),
    ("TS_DELETE", 2, 2),
    ("TS_FILTER", 2, 2),
    ("TSVECTOR_IN", 1, 1),
    ("TSQUERY_IN", 1, 1),
    ("__TS_MATCH", 2, 2),
    ("JSON_TO_TSVECTOR", 2, 3),
    ("JSONB_TO_TSVECTOR", 2, 3),
    ("TSVECTOR_CONCAT", 2, 2),
    ("TSQUERY_AND", 2, 2),
    ("TSQUERY_OR", 2, 2),
    ("TSQUERY_PHRASE", 2, 3),
    ("TSQUERY_NOT", 1, 1),
];

/// The registered name of a function of this module.
pub fn lookup(name: &str) -> Option<(&'static str, usize, usize)> {
    if crate::dialect::current() != crate::dialect::Dialect::Postgresql {
        return None;
    }
    NAMES.iter().find(|(n, _, _)| *n == name).copied()
}

// ----- the English stemmer (Snowball, "Porter2") -----

const STOP: &[&str] = &[
    "i", "me", "my", "myself", "we", "our", "ours", "ourselves", "you", "your", "yours", "yourself", "yourselves", "he", "him", "his", "himself", "she", "her",
    "hers", "herself", "it", "its", "itself", "they", "them", "their", "theirs", "themselves", "what", "which", "who", "whom", "this", "that", "these", "those",
    "am", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had", "having", "do", "does", "did", "doing", "a", "an", "the", "and", "but", "if",
    "or", "because", "as", "until", "while", "of", "at", "by", "for", "with", "about", "against", "between", "into", "through", "during", "before", "after",
    "above", "below", "to", "from", "up", "down", "in", "out", "on", "off", "over", "under", "again", "further", "then", "once", "here", "there", "when",
    "where", "why", "how", "all", "any", "both", "each", "few", "more", "most", "other", "some", "such", "no", "nor", "not", "only", "own", "same", "so",
    "than", "too", "very", "s", "t", "can", "will", "just", "don", "should", "now",
];

fn vowel(c: u8) -> bool {
    matches!(c, b'a' | b'e' | b'i' | b'o' | b'u' | b'y')
}

/// The start of R1 or R2: after the first non-vowel that follows a vowel, from `from`.
fn region(w: &[u8], from: usize) -> usize {
    let mut i = from;
    while i + 1 < w.len() {
        if vowel(w[i]) && !vowel(w[i + 1]) {
            return i + 2;
        }
        i += 1;
    }
    w.len()
}

fn short_syllable_end(w: &[u8]) -> bool {
    // a vowel followed by a non-vowel other than w, x, Y, preceded by a non-vowel; or a vowel at the start followed by a non-vowel
    let n = w.len();
    if n == 2 {
        return vowel(w[0]) && !vowel(w[1]);
    }
    if n >= 3 {
        return !vowel(w[n - 3]) && vowel(w[n - 2]) && !vowel(w[n - 1]) && !matches!(w[n - 1], b'w' | b'x' | b'Y');
    }
    false
}

fn is_short(w: &[u8], r1: usize) -> bool {
    r1 >= w.len() && short_syllable_end(w)
}

pub fn stem(word: &str) -> String {
    if word.len() <= 2 || !word.is_ascii() {
        return word.to_string();
    }
    match word {
        "skis" => return "ski".into(),
        "skies" => return "sky".into(),
        "dying" => return "die".into(),
        "lying" => return "lie".into(),
        "tying" => return "tie".into(),
        "idly" => return "idl".into(),
        "gently" => return "gentl".into(),
        "ugly" => return "ugli".into(),
        "early" => return "earli".into(),
        "only" => return "onli".into(),
        "singly" => return "singl".into(),
        "sky" | "news" | "howe" | "atlas" | "cosmos" | "bias" | "andes" => return word.into(),
        _ => {}
    }
    let mut w: Vec<u8> = word.bytes().collect();
    if w[0] == b'\'' {
        w.remove(0);
    }
    for suffix in [&b"'s'"[..], b"'s", b"'"] {
        if w.ends_with(suffix) {
            w.truncate(w.len() - suffix.len());
            break;
        }
    }
    if w.is_empty() {
        return String::new();
    }
    if w[0] == b'y' {
        w[0] = b'Y';
    }
    for i in 1..w.len() {
        if w[i] == b'y' && vowel(w[i - 1]) {
            w[i] = b'Y';
        }
    }
    let r1_from = if w.starts_with(b"gener") || w.starts_with(b"arsen") {
        5
    } else if w.starts_with(b"commun") {
        6
    } else {
        0
    };
    let mut r1 = if r1_from > 0 { r1_from } else { region(&w, 0) };
    let mut r2 = region(&w, r1);
    // step 1a
    if w.ends_with(b"sses") {
        w.truncate(w.len() - 2);
    } else if w.ends_with(b"ied") || w.ends_with(b"ies") {
        let keep = if w.len() > 4 { w.len() - 2 } else { w.len() - 1 };
        w.truncate(keep);
    } else if w.ends_with(b"us") || w.ends_with(b"ss") {
    } else if w.ends_with(b"s") && w.len() >= 2 && w[..w.len() - 2].iter().any(|&c| vowel(c)) {
        w.truncate(w.len() - 1);
    }
    if matches!(&w[..], b"inning" | b"outing" | b"canning" | b"herring" | b"earring" | b"proceed" | b"exceed" | b"succeed") {
        return String::from_utf8_lossy(&w).into_owned();
    }
    r1 = r1.min(w.len());
    r2 = r2.min(w.len());
    // step 1b
    let mut done = false;
    for suffix in [&b"eedly"[..], b"eed"] {
        if w.ends_with(suffix) {
            if w.len() - suffix.len() >= r1 {
                w.truncate(w.len() - suffix.len());
                w.extend_from_slice(b"ee");
            }
            done = true;
            break;
        }
    }
    if !done {
        for suffix in [&b"ingly"[..], b"edly", b"ing", b"ed"] {
            if w.ends_with(suffix) {
                let base = w.len() - suffix.len();
                if w[..base].iter().any(|&c| vowel(c)) {
                    w.truncate(base);
                    if w.ends_with(b"at") || w.ends_with(b"bl") || w.ends_with(b"iz") {
                        w.push(b'e');
                    } else if w.len() >= 2 && w[w.len() - 1] == w[w.len() - 2] && matches!(w[w.len() - 1], b'b' | b'd' | b'f' | b'g' | b'm' | b'n' | b'p' | b'r' | b't') {
                        w.pop();
                    } else if is_short(&w, r1) {
                        w.push(b'e');
                    }
                }
                break;
            }
        }
    }
    // step 1c
    if w.len() > 2 && matches!(w[w.len() - 1], b'y' | b'Y') && !vowel(w[w.len() - 2]) {
        let n = w.len();
        w[n - 1] = b'i';
    }
    // step 2
    const STEP2: &[(&str, &str)] = &[
        ("ization", "ize"), ("ational", "ate"), ("fulness", "ful"), ("ousness", "ous"), ("iveness", "ive"), ("tional", "tion"), ("biliti", "ble"),
        ("lessli", "less"), ("entli", "ent"), ("ation", "ate"), ("alism", "al"), ("aliti", "al"), ("ousli", "ous"), ("iviti", "ive"), ("fulli", "ful"),
        ("enci", "ence"), ("anci", "ance"), ("abli", "able"), ("izer", "ize"), ("ator", "ate"), ("alli", "al"), ("bli", "ble"),
    ];
    let mut changed = false;
    for (from, to) in STEP2 {
        if w.ends_with(from.as_bytes()) {
            if w.len() - from.len() >= r1 {
                w.truncate(w.len() - from.len());
                w.extend_from_slice(to.as_bytes());
            }
            changed = true;
            break;
        }
    }
    if !changed {
        if w.ends_with(b"ogi") {
            if w.len() - 3 >= r1 && w.len() >= 4 && w[w.len() - 4] == b'l' {
                w.truncate(w.len() - 1);
            }
        } else if w.ends_with(b"li") && w.len() - 2 >= r1 && w.len() >= 3 && matches!(w[w.len() - 3], b'c' | b'd' | b'e' | b'g' | b'h' | b'k' | b'm' | b'n' | b'r' | b't') {
            w.truncate(w.len() - 2);
        }
    }
    // step 3
    const STEP3: &[(&str, &str)] = &[("ational", "ate"), ("tional", "tion"), ("alize", "al"), ("icate", "ic"), ("iciti", "ic"), ("ical", "ic"), ("ful", ""), ("ness", "")];
    let mut changed = false;
    for (from, to) in STEP3 {
        if w.ends_with(from.as_bytes()) {
            if w.len() - from.len() >= r1 {
                w.truncate(w.len() - from.len());
                w.extend_from_slice(to.as_bytes());
            }
            changed = true;
            break;
        }
    }
    if !changed && w.ends_with(b"ative") && w.len() - 5 >= r2 {
        w.truncate(w.len() - 5);
    }
    // step 4
    const STEP4: &[&str] = &["ement", "ance", "ence", "able", "ible", "ment", "ant", "ent", "ism", "ate", "iti", "ous", "ive", "ize", "al", "er", "ic"];
    let mut changed = false;
    for from in STEP4 {
        if w.ends_with(from.as_bytes()) {
            if w.len() - from.len() >= r2 {
                w.truncate(w.len() - from.len());
            }
            changed = true;
            break;
        }
    }
    if !changed && w.ends_with(b"ion") && w.len() - 3 >= r2 && w.len() >= 4 && matches!(w[w.len() - 4], b's' | b't') {
        w.truncate(w.len() - 3);
    }
    // step 5
    if w.ends_with(b"e") {
        let before = &w[..w.len() - 1];
        if w.len() - 1 >= r2 || (w.len() - 1 >= r1 && !short_syllable_end(before)) {
            w.pop();
        }
    } else if w.ends_with(b"ll") && w.len() - 1 >= r2 {
        w.pop();
    }
    String::from_utf8_lossy(&w).replace('Y', "y")
}

// ----- tsvector -----

#[derive(Clone, Debug, Default)]
struct Lex {
    word: String,
    /// position (1 to 16383) and weight (0 = D, 1 = C, 2 = B, 3 = A)
    pos: Vec<(u16, u8)>,
}

fn weight_letter(w: u8) -> &'static str {
    match w {
        3 => "A",
        2 => "B",
        1 => "C",
        _ => "",
    }
}

fn quote(w: &str) -> String {
    format!("'{}'", w.replace('\'', "''").replace('\\', "\\\\"))
}

fn write_vector(v: &[Lex]) -> String {
    v.iter()
        .map(|l| {
            if l.pos.is_empty() {
                quote(&l.word)
            } else {
                let ps: Vec<String> = l.pos.iter().map(|(p, w)| format!("{p}{}", weight_letter(*w))).collect();
                format!("{}:{}", quote(&l.word), ps.join(","))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn merge(mut lexes: Vec<Lex>) -> Vec<Lex> {
    let mut map: BTreeMap<String, Vec<(u16, u8)>> = BTreeMap::new();
    for l in lexes.drain(..) {
        map.entry(l.word).or_default().extend(l.pos);
    }
    map.into_iter()
        .map(|(word, mut pos)| {
            pos.sort();
            pos.dedup_by_key(|p| p.0);
            pos.truncate(256);
            Lex { word, pos }
        })
        .collect()
}

/// Reads `'cat':3 'fat':2A,4`; an error for anything that is not a tsvector.
fn parse_vector(s: &str) -> Result<Vec<Lex>> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    let bad = || Error::type_err(format!("syntax error in tsvector: \"{s}\""));
    while i < b.len() {
        while i < b.len() && b[i].is_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let mut word = String::new();
        if b[i] == '\'' {
            i += 1;
            loop {
                match b.get(i) {
                    None => return Err(bad()),
                    Some('\'') if b.get(i + 1) == Some(&'\'') => {
                        word.push('\'');
                        i += 2;
                    }
                    Some('\'') => {
                        i += 1;
                        break;
                    }
                    Some('\\') if i + 1 < b.len() => {
                        word.push(b[i + 1]);
                        i += 2;
                    }
                    Some(c) => {
                        word.push(*c);
                        i += 1;
                    }
                }
            }
        } else {
            while i < b.len() && !b[i].is_whitespace() && b[i] != ':' {
                if b[i] == '\\' && i + 1 < b.len() {
                    i += 1;
                }
                word.push(b[i]);
                i += 1;
            }
        }
        let mut pos = Vec::new();
        if b.get(i) == Some(&':') {
            i += 1;
            loop {
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i == start {
                    return Err(bad());
                }
                let n: u32 = b[start..i].iter().collect::<String>().parse().map_err(|_| bad())?;
                if n == 0 {
                    return Err(Error::type_err("wrong position info in tsvector"));
                }
                let mut w = 0u8;
                if let Some(c) = b.get(i) {
                    w = match c.to_ascii_uppercase() {
                        'A' => 3,
                        'B' => 2,
                        'C' => 1,
                        'D' => 0,
                        _ => 255,
                    };
                    if w != 255 {
                        i += 1;
                    } else {
                        w = 0;
                    }
                }
                pos.push((n.min(16383) as u16, w));
                if b.get(i) == Some(&',') {
                    i += 1;
                } else {
                    break;
                }
            }
        }
        if word.is_empty() {
            return Err(bad());
        }
        out.push(Lex { word, pos });
    }
    Ok(merge(out))
}

// ----- text to lexemes -----

#[derive(Clone, Copy, PartialEq)]
enum Config {
    English,
    Simple,
}

fn config_of(v: Option<&Value>) -> Config {
    match v {
        Some(v) => {
            let n = v.to_string().to_ascii_lowercase();
            let n = n.rsplit('.').next().unwrap_or("").trim_matches('"').to_string();
            if n == "english" || n.is_empty() {
                Config::English
            } else {
                Config::Simple
            }
        }
        None => Config::English,
    }
}

/// The words of `text`, as `(lexeme, position)`; a stop word has no lexeme but takes a position.
fn tokenize(text: &str, cfg: Config) -> Vec<(Option<String>, u32)> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut pos = 0u32;
    while i < chars.len() {
        if !chars[i].is_alphanumeric() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() {
            let c = chars[i];
            let joins = (c == '.' || c == '-') && i > start && chars[i - 1].is_ascii_digit() && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit());
            if c.is_alphanumeric() || joins {
                i += 1;
            } else {
                break;
            }
        }
        let word: String = chars[start..i].iter().collect::<String>().to_lowercase();
        pos += 1;
        let lexeme = match cfg {
            Config::Simple => Some(word),
            Config::English if STOP.contains(&word.as_str()) => None,
            Config::English => Some(stem(&word)),
        };
        out.push((lexeme, pos));
    }
    out
}

fn text_to_vector(text: &str, cfg: Config) -> Vec<Lex> {
    merge(
        tokenize(text, cfg)
            .into_iter()
            .filter_map(|(l, p)| l.map(|word| Lex { word, pos: vec![(p.min(16383) as u16, 0)] }))
            .collect(),
    )
}

// ----- tsquery -----

#[derive(Clone, Debug, PartialEq)]
enum Q {
    Term { word: String, prefix: bool, weights: u8 },
    Not(Box<Q>),
    And(Box<Q>, Box<Q>),
    Or(Box<Q>, Box<Q>),
    Phrase(Box<Q>, Box<Q>, u32),
}

fn write_query(q: &Q) -> String {
    fn go(q: &Q, parent: u8) -> String {
        // precedence: or 1, and 2, phrase 3, not 4
        match q {
            Q::Term { word, prefix, weights } => {
                let mut s = quote(word);
                if *prefix || *weights != 0 {
                    s.push(':');
                    if *prefix {
                        s.push('*');
                    }
                    for (bit, c) in [(8u8, 'A'), (4, 'B'), (2, 'C'), (1, 'D')] {
                        if weights & bit != 0 {
                            s.push(c);
                        }
                    }
                }
                s
            }
            Q::Not(x) => {
                let inner = go(x, 4);
                if matches!(**x, Q::Term { .. } | Q::Not(_)) { format!("!{inner}") } else { format!("!( {} )", go(x, 0)) }
            }
            Q::And(a, b) => {
                let s = format!("{} & {}", go(a, 2), go(b, 2));
                if parent > 2 { format!("( {s} )") } else { s }
            }
            Q::Or(a, b) => {
                let s = format!("{} | {}", go(a, 1), go(b, 1));
                if parent > 1 { format!("( {s} )") } else { s }
            }
            Q::Phrase(a, b, n) => {
                let op = if *n == 1 { "<->".to_string() } else { format!("<{n}>") };
                let s = format!("{} {op} {}", go(a, 3), go(b, 3));
                if parent > 3 { format!("( {s} )") } else { s }
            }
        }
    }
    go(q, 0)
}

struct QParser<'a> {
    b: Vec<char>,
    i: usize,
    src: &'a str,
}

impl QParser<'_> {
    fn err(&self) -> Error {
        Error::type_err(format!("syntax error in tsquery: \"{}\"", self.src))
    }
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<char> {
        self.ws();
        self.b.get(self.i).copied()
    }
    fn or(&mut self) -> Result<Q> {
        let mut l = self.and()?;
        while self.peek() == Some('|') {
            self.i += 1;
            let r = self.and()?;
            l = Q::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Q> {
        let mut l = self.phrase()?;
        while self.peek() == Some('&') {
            self.i += 1;
            let r = self.phrase()?;
            l = Q::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn phrase(&mut self) -> Result<Q> {
        let mut l = self.unary()?;
        loop {
            self.ws();
            if self.b.get(self.i) == Some(&'<') {
                let mut j = self.i + 1;
                let mut n = String::new();
                while j < self.b.len() && self.b[j] != '>' {
                    n.push(self.b[j]);
                    j += 1;
                }
                if j >= self.b.len() {
                    return Err(self.err());
                }
                let dist = if n == "-" { 1 } else { n.parse::<u32>().map_err(|_| self.err())? };
                self.i = j + 1;
                let r = self.unary()?;
                l = Q::Phrase(Box::new(l), Box::new(r), dist);
            } else {
                return Ok(l);
            }
        }
    }
    fn unary(&mut self) -> Result<Q> {
        match self.peek() {
            Some('!') => {
                self.i += 1;
                Ok(Q::Not(Box::new(self.unary()?)))
            }
            Some('(') => {
                self.i += 1;
                let q = self.or()?;
                if self.peek() != Some(')') {
                    return Err(self.err());
                }
                self.i += 1;
                Ok(q)
            }
            Some(_) => self.term(),
            None => Err(Error::type_err(format!("syntax error in tsquery: \"{}\"", self.src))),
        }
    }
    fn term(&mut self) -> Result<Q> {
        let mut word = String::new();
        if self.b[self.i] == '\'' {
            self.i += 1;
            loop {
                match self.b.get(self.i) {
                    None => return Err(self.err()),
                    Some('\'') if self.b.get(self.i + 1) == Some(&'\'') => {
                        word.push('\'');
                        self.i += 2;
                    }
                    Some('\'') => {
                        self.i += 1;
                        break;
                    }
                    Some('\\') if self.i + 1 < self.b.len() => {
                        word.push(self.b[self.i + 1]);
                        self.i += 2;
                    }
                    Some(c) => {
                        word.push(*c);
                        self.i += 1;
                    }
                }
            }
        } else {
            while self.i < self.b.len() && !self.b[self.i].is_whitespace() && !matches!(self.b[self.i], '&' | '|' | '!' | '(' | ')' | ':' | '<') {
                if self.b[self.i] == '\\' && self.i + 1 < self.b.len() {
                    self.i += 1;
                }
                word.push(self.b[self.i]);
                self.i += 1;
            }
        }
        if word.is_empty() {
            return Err(self.err());
        }
        let (mut prefix, mut weights) = (false, 0u8);
        if self.b.get(self.i) == Some(&':') {
            self.i += 1;
            while let Some(c) = self.b.get(self.i) {
                match c.to_ascii_uppercase() {
                    '*' => prefix = true,
                    'A' => weights |= 8,
                    'B' => weights |= 4,
                    'C' => weights |= 2,
                    'D' => weights |= 1,
                    _ => break,
                }
                self.i += 1;
            }
        }
        Ok(Q::Term { word, prefix, weights })
    }
}

fn parse_query(s: &str) -> Result<Option<Q>> {
    let mut p = QParser { b: s.chars().collect(), i: 0, src: s };
    if p.peek().is_none() {
        return Ok(None);
    }
    let q = p.or()?;
    if p.peek().is_some() {
        return Err(p.err());
    }
    Ok(Some(q))
}

/// Applies the configuration to the words of a query: each is stemmed, and a stop word drops out of the query.
fn normalise(q: Q, cfg: Config) -> Option<Q> {
    match q {
        Q::Term { word, prefix, weights } => {
            // a word may split into several lexemes (`foo-bar`): they are joined by phrase search
            let toks: Vec<(Option<String>, u32)> = tokenize(&word, cfg);
            let words: Vec<String> = toks.into_iter().filter_map(|(l, _)| l).collect();
            let mut it = words.into_iter();
            let first = it.next()?;
            let mut out = Q::Term { word: first, prefix, weights };
            for w in it {
                out = Q::Phrase(Box::new(out), Box::new(Q::Term { word: w, prefix, weights }), 1);
            }
            Some(out)
        }
        Q::Not(x) => normalise(*x, cfg).map(|x| Q::Not(Box::new(x))),
        Q::And(a, b) => match (normalise(*a, cfg), normalise(*b, cfg)) {
            (Some(a), Some(b)) => Some(Q::And(Box::new(a), Box::new(b))),
            (x, y) => x.or(y),
        },
        Q::Or(a, b) => match (normalise(*a, cfg), normalise(*b, cfg)) {
            (Some(a), Some(b)) => Some(Q::Or(Box::new(a), Box::new(b))),
            (x, y) => x.or(y),
        },
        Q::Phrase(a, b, n) => match (normalise(*a, cfg), normalise(*b, cfg)) {
            (Some(a), Some(b)) => Some(Q::Phrase(Box::new(a), Box::new(b), n)),
            (x, y) => x.or(y),
        },
    }
}

fn query_text(q: Option<Q>) -> Value {
    Value::text(&q.map_or(String::new(), |q| write_query(&q)))
}

fn and_all(parts: Vec<Q>, phrase: bool) -> Option<Q> {
    let mut it = parts.into_iter();
    let first = it.next()?;
    Some(it.fold(first, |a, b| if phrase { Q::Phrase(Box::new(a), Box::new(b), 1) } else { Q::And(Box::new(a), Box::new(b)) }))
}

fn term(word: String) -> Q {
    Q::Term { word, prefix: false, weights: 0 }
}

/// `websearch_to_tsquery`: words are ANDed, `"a phrase"` is a phrase, `or` an alternative, `-word` a negation.
fn web_query(text: &str, cfg: Config) -> Option<Q> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut groups: Vec<Vec<Q>> = vec![Vec::new()];
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '"' {
            let start = i + 1;
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            let phrase: String = chars[start..i.min(chars.len())].iter().collect();
            i += 1;
            let toks = tokenize(&phrase, cfg);
            if let Some(q) = phrase_of(&toks) {
                groups.last_mut().expect("a group").push(q);
            }
        } else {
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            let raw: String = chars[start..i].iter().collect();
            if raw.eq_ignore_ascii_case("or") {
                groups.push(Vec::new());
                continue;
            }
            let (neg, body) = match raw.strip_prefix('-') {
                Some(rest) if !rest.is_empty() => (true, rest.to_string()),
                _ => (false, raw),
            };
            let toks = tokenize(&body, cfg);
            if let Some(q) = phrase_of(&toks) {
                groups.last_mut().expect("a group").push(if neg { Q::Not(Box::new(q)) } else { q });
            }
        }
    }
    let mut ors = groups.into_iter().filter_map(|g| and_all(g, false));
    let first = ors.next()?;
    Some(ors.fold(first, |a, b| Q::Or(Box::new(a), Box::new(b))))
}

/// The lexemes of `toks` joined by `<->`, or by `<n>` over the stop words between them.
fn phrase_of(toks: &[(Option<String>, u32)]) -> Option<Q> {
    let mut out: Option<(Q, u32)> = None;
    for (l, p) in toks {
        let Some(w) = l else { continue };
        out = Some(match out {
            None => (term(w.clone()), *p),
            Some((q, last)) => (Q::Phrase(Box::new(q), Box::new(term(w.clone())), p - last), *p),
        });
    }
    out.map(|(q, _)| q)
}

// ----- matching -----

/// The positions at which `q` holds in `doc`: `None` when it does not hold, `Some(empty)` when it holds without positions.
fn eval(q: &Q, doc: &[Lex]) -> Option<Vec<u32>> {
    match q {
        Q::Term { word, prefix, weights } => {
            let mut found = false;
            let mut pos = Vec::new();
            for l in doc {
                let hit = if *prefix { l.word.starts_with(word.as_str()) } else { l.word == *word };
                if !hit {
                    continue;
                }
                if *weights == 0 {
                    found = true;
                    pos.extend(l.pos.iter().map(|(p, _)| u32::from(*p)));
                } else {
                    for (p, w) in &l.pos {
                        let bit = match w {
                            3 => 8,
                            2 => 4,
                            1 => 2,
                            _ => 1,
                        };
                        if weights & bit != 0 {
                            found = true;
                            pos.push(u32::from(*p));
                        }
                    }
                }
            }
            found.then_some(pos)
        }
        Q::Not(x) => match eval(x, doc) {
            Some(_) => None,
            None => Some(Vec::new()),
        },
        Q::And(a, b) => match (eval(a, doc), eval(b, doc)) {
            (Some(mut x), Some(y)) => {
                x.extend(y);
                x.sort_unstable();
                x.dedup();
                Some(x)
            }
            _ => None,
        },
        Q::Or(a, b) => match (eval(a, doc), eval(b, doc)) {
            (None, None) => None,
            (x, y) => {
                let mut v = x.unwrap_or_default();
                v.extend(y.unwrap_or_default());
                v.sort_unstable();
                v.dedup();
                Some(v)
            }
        },
        Q::Phrase(a, b, n) => {
            let (x, y) = (eval(a, doc)?, eval(b, doc)?);
            let hits: Vec<u32> = y.iter().copied().filter(|p| x.iter().any(|q| q + n == *p)).collect();
            (!hits.is_empty()).then_some(hits)
        }
    }
}

fn operands(q: &Q, out: &mut Vec<Q>) {
    match q {
        Q::Term { .. } => out.push(q.clone()),
        Q::Not(_) => {}
        Q::And(a, b) | Q::Or(a, b) | Q::Phrase(a, b, _) => {
            operands(a, out);
            operands(b, out);
        }
    }
}

fn wpos(w: u8) -> f64 {
    [0.1, 0.2, 0.4, 1.0][w.min(3) as usize]
}

fn rank(doc: &[Lex], q: &Q, norm: i64) -> f64 {
    let mut items = Vec::new();
    operands(q, &mut items);
    let mut seen: Vec<String> = Vec::new();
    items.retain(|t| {
        let Q::Term { word, .. } = t else { return false };
        if seen.contains(word) {
            false
        } else {
            seen.push(word.clone());
            true
        }
    });
    let size = items.len();
    if size == 0 {
        return 0.0;
    }
    let entries: Vec<Vec<(u16, u8)>> = items
        .iter()
        .map(|t| {
            let Q::Term { word, prefix, .. } = t else { return Vec::new() };
            doc.iter().filter(|l| if *prefix { l.word.starts_with(word.as_str()) } else { l.word == *word }).flat_map(|l| l.pos.clone()).collect()
        })
        .collect();
    let all_and = fn_is_and(q);
    let mut res;
    if all_and && size >= 2 {
        res = -1.0;
        for i in 0..size {
            for k in 0..i {
                for (p1, w1) in &entries[i] {
                    for (p2, w2) in &entries[k] {
                        let dist = (i32::from(*p1) - i32::from(*p2)).unsigned_abs().max(1) as f64;
                        let wd = if dist > 100.0 { 1e-30 } else { 1.0 / (1.005 + 0.05 * (dist / 1.5 - 2.0).exp()) };
                        let curw = (wpos(*w1) * wpos(*w2) * wd).sqrt();
                        res = if res < 0.0 { curw } else { 1.0 - (1.0 - res) * (1.0 - curw) };
                    }
                }
            }
        }
        if res < 0.0 {
            res = 1e-20;
        }
    } else {
        res = 0.0;
        for e in &entries {
            if e.is_empty() {
                continue;
            }
            let (mut resj, mut wjm, mut jm) = (0.0f64, -1.0f64, 0usize);
            for (j, (_, w)) in e.iter().enumerate() {
                let wp = wpos(*w);
                resj += wp / ((j + 1) * (j + 1)) as f64;
                if wp > wjm {
                    wjm = wp;
                    jm = j;
                }
            }
            res += (wjm + resj - wjm / ((jm + 1) * (jm + 1)) as f64) / 1.644_934_066_85;
        }
        if size > 1 {
            res /= size as f64;
        }
    }
    let len: f64 = doc.iter().map(|l| l.pos.len().max(1) as f64).sum();
    if norm & 1 != 0 {
        res /= 1.0 + len.ln();
    }
    if norm & 2 != 0 && len > 0.0 {
        res /= len;
    }
    if norm & 8 != 0 {
        res /= doc.len().max(1) as f64;
    }
    if norm & 32 != 0 {
        res /= res + 1.0;
    }
    res
}

fn fn_is_and(q: &Q) -> bool {
    match q {
        Q::Term { .. } => true,
        Q::And(a, b) | Q::Phrase(a, b, _) => fn_is_and(a) && fn_is_and(b),
        _ => false,
    }
}

fn count_nodes(q: &Q) -> i64 {
    match q {
        Q::Term { .. } => 1,
        Q::Not(x) => 1 + count_nodes(x),
        Q::And(a, b) | Q::Or(a, b) | Q::Phrase(a, b, _) => 1 + count_nodes(a) + count_nodes(b),
    }
}

/// `ts_headline`: the document with the words of the query marked.
fn headline(doc: &str, q: &Q, options: &str, cfg: Config) -> String {
    let mut start_sel = "<b>".to_string();
    let mut stop_sel = "</b>".to_string();
    let mut max_words = 35usize;
    let mut min_words = 15usize;
    let mut frag_delim = " ... ".to_string();
    let mut highlight_all = false;
    for opt in options.split(',') {
        let Some((k, v)) = opt.split_once('=') else { continue };
        let v = v.trim().trim_matches('"').to_string();
        match k.trim().to_ascii_lowercase().as_str() {
            "startsel" => start_sel = v,
            "stopsel" => stop_sel = v,
            "maxwords" => max_words = v.parse().unwrap_or(35),
            "minwords" => min_words = v.parse().unwrap_or(15),
            "fragmentdelimiter" => frag_delim = v,
            "highlightall" => highlight_all = v.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    let mut items = Vec::new();
    operands(q, &mut items);
    // (start, end) of each word of the document, and whether it is wanted
    let chars: Vec<(usize, char)> = doc.char_indices().collect();
    let mut words: Vec<(usize, usize, bool)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].1.is_alphanumeric() {
            i += 1;
            continue;
        }
        let s = i;
        while i < chars.len() && chars[i].1.is_alphanumeric() {
            i += 1;
        }
        let (bs, be) = (chars[s].0, chars.get(i).map_or(doc.len(), |c| c.0));
        let w = doc[bs..be].to_lowercase();
        let lex = match cfg {
            Config::Simple => w,
            Config::English => stem(&w),
        };
        let hit = items.iter().any(|t| matches!(t, Q::Term { word, prefix, .. } if if *prefix { lex.starts_with(word.as_str()) } else { lex == *word }));
        words.push((bs, be, hit));
    }
    let (lo, hi) = if highlight_all || words.len() <= max_words {
        (0, words.len())
    } else {
        let first = words.iter().position(|w| w.2).unwrap_or(0);
        let lo = first.saturating_sub(min_words / 2);
        (lo, (lo + max_words.min(words.len() - lo)).min(words.len()))
    };
    if words.is_empty() {
        return doc.to_string();
    }
    let from = if lo == 0 { 0 } else { words[lo].0 };
    let to = if hi >= words.len() { doc.len() } else { words[hi - 1].1 };
    let mut out = String::new();
    if lo > 0 {
        out.push_str(frag_delim.trim_end_matches(' '));
        out.push(' ');
    }
    let mut at = from;
    for (s, e, hit) in &words[lo..hi] {
        out.push_str(&doc[at..*s]);
        if *hit {
            out.push_str(&start_sel);
            out.push_str(&doc[*s..*e]);
            out.push_str(&stop_sel);
        } else {
            out.push_str(&doc[*s..*e]);
        }
        at = *e;
    }
    let _ = to;
    if hi < words.len() {
        out.push_str(frag_delim.trim_start_matches(' '));
    }
    out
}

fn text_of(v: &Value) -> String {
    match v {
        Value::Text(s) => s.to_string(),
        other => other.to_string(),
    }
}

fn vector_of(v: &Value) -> Result<Vec<Lex>> {
    parse_vector(&text_of(v))
}

fn query_of(v: &Value) -> Result<Option<Q>> {
    parse_query(&text_of(v))
}

/// Strings of a JSON document, in order (`filter`: `"string"`, `"numeric"`, `"boolean"`, `"key"` or `"all"`).
fn json_text(v: &serde_json::Value, filter: &str, out: &mut Vec<String>) {
    let all = filter.contains("all");
    match v {
        serde_json::Value::String(s) if all || filter.contains("string") => out.push(s.clone()),
        serde_json::Value::Number(n) if all || filter.contains("numeric") => out.push(n.to_string()),
        serde_json::Value::Bool(b) if all || filter.contains("boolean") => out.push(b.to_string()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| json_text(x, filter, out)),
        serde_json::Value::Object(o) => {
            for (k, x) in o {
                if all || filter.contains("key") {
                    out.push(k.clone());
                }
                json_text(x, filter, out);
            }
        }
        _ => {}
    }
}

/// Evaluates the function `name` (as registered in [`NAMES`]).
pub fn call(name: &str, vals: &[Cow<'_, Value>]) -> Result<Value> {
    let v = |i: usize| -> &Value { vals[i].as_ref() };
    // the configuration is the first argument when there are two
    let (cfg, rest): (Config, &[Cow<'_, Value>]) = match name {
        "TO_TSVECTOR" | "TO_TSQUERY" | "PLAINTO_TSQUERY" | "PHRASETO_TSQUERY" | "WEBSEARCH_TO_TSQUERY" | "JSON_TO_TSVECTOR" | "JSONB_TO_TSVECTOR" if vals.len() == 2 && name != "JSON_TO_TSVECTOR" && name != "JSONB_TO_TSVECTOR" => {
            (config_of(Some(v(0))), &vals[1..])
        }
        "JSON_TO_TSVECTOR" | "JSONB_TO_TSVECTOR" if vals.len() == 3 => (config_of(Some(v(0))), &vals[1..]),
        _ => (Config::English, vals),
    };
    let r = |i: usize| -> &Value { rest[i].as_ref() };
    Ok(match name {
        "TO_TSVECTOR" => Value::text(&write_vector(&text_to_vector(&text_of(r(0)), cfg))),
        "TO_TSQUERY" => {
            let q = query_of(r(0))?;
            query_text(q.and_then(|q| normalise(q, cfg)))
        }
        "PLAINTO_TSQUERY" => query_text(and_all(text_to_vector_terms(&text_of(r(0)), cfg), false)),
        "PHRASETO_TSQUERY" => query_text(phrase_of(&tokenize(&text_of(r(0)), cfg))),
        "WEBSEARCH_TO_TSQUERY" => query_text(web_query(&text_of(r(0)), cfg)),
        "SETWEIGHT" => {
            let w = match text_of(v(1)).to_ascii_uppercase().as_str() {
                "A" => 3,
                "B" => 2,
                "C" => 1,
                "D" => 0,
                _ => return Err(Error::runtime("unrecognized weight: ".to_string() + &text_of(v(1)))),
            };
            let only: Option<Vec<String>> = vals.get(2).map(|a| match a.as_ref() {
                Value::Array(items) => items.iter().map(text_of).collect(),
                other => vec![text_of(other)],
            });
            let mut doc = vector_of(v(0))?;
            for l in &mut doc {
                if only.as_ref().is_none_or(|o| o.contains(&l.word)) {
                    l.pos.iter_mut().for_each(|p| p.1 = w);
                }
            }
            Value::text(&write_vector(&doc))
        }
        "STRIP" => {
            let mut doc = vector_of(v(0))?;
            doc.iter_mut().for_each(|l| l.pos.clear());
            Value::text(&write_vector(&doc))
        }
        "TSVECTOR_IN" => Value::text(&write_vector(&vector_of(v(0))?)),
        "TSQUERY_IN" => query_text(query_of(v(0))?),
        "__TS_MATCH" => {
            // `vector @@ query`; either order, and plain text on either side is read as a document or a query
            let (a, b) = (text_of(v(0)), text_of(v(1)));
            let as_query = |s: &str| parse_query(s);
            let (doc, q) = match (parse_vector(&a), as_query(&b)) {
                (Ok(d), Ok(q)) => (d, q),
                _ => match (parse_vector(&b), as_query(&a)) {
                    (Ok(d), Ok(q)) => (d, q),
                    _ => (text_to_vector(&a, Config::English), and_all(text_to_vector_terms(&b, Config::English), false)),
                },
            };
            match q {
                Some(q) => Value::Bool(eval(&q, &doc).is_some()),
                None => Value::Bool(false),
            }
        }
        "TS_RANK" | "TS_RANK_CD" => {
            let doc = vector_of(v(0))?;
            let norm = vals.get(2).and_then(|n| n.to_f64()).unwrap_or(0.0) as i64;
            match query_of(v(1))? {
                Some(q) => Value::Float(rank(&doc, &q, norm) as f32),
                None => Value::Float(0.0),
            }
        }
        "TS_HEADLINE" => {
            let (cfg, args): (Config, &[Cow<'_, Value>]) = match vals.len() {
                4 => (config_of(Some(v(0))), &vals[1..]),
                3 if text_of(v(2)).contains('=') => (Config::English, vals),
                3 => (config_of(Some(v(0))), &vals[1..]),
                _ => (Config::English, vals),
            };
            let q = query_of(&args[1])?;
            let options = args.get(2).map(|o| text_of(o)).unwrap_or_default();
            match q {
                Some(q) => Value::text(&headline(&text_of(&args[0]), &q, &options, cfg)),
                None => Value::text(&text_of(&args[0])),
            }
        }
        "NUMNODE" => Value::Int(query_of(v(0))?.map_or(0, |q| count_nodes(&q))),
        "QUERYTREE" => match query_of(v(0))? {
            Some(q) => Value::text(&write_query(&q)),
            None => Value::text("T"),
        },
        "TSVECTOR_TO_ARRAY" => Value::Array(std::sync::Arc::new(vector_of(v(0))?.into_iter().map(|l| Value::text(&l.word)).collect())),
        "ARRAY_TO_TSVECTOR" => match v(0) {
            Value::Array(items) => Value::text(&write_vector(&merge(items.iter().map(|i| Lex { word: text_of(i), pos: Vec::new() }).collect()))),
            other => return Err(Error::type_err(format!("'{other}' is not an array"))),
        },
        "TS_DELETE" => {
            let words: Vec<String> = match v(1) {
                Value::Array(items) => items.iter().map(text_of).collect(),
                other => vec![text_of(other)],
            };
            let doc: Vec<Lex> = vector_of(v(0))?.into_iter().filter(|l| !words.contains(&l.word)).collect();
            Value::text(&write_vector(&doc))
        }
        "TS_FILTER" => {
            let keep: Vec<u8> = match v(1) {
                Value::Array(items) => items.iter().map(|i| text_of(i).to_ascii_uppercase()).map(|w| match w.as_str() { "A" => 3, "B" => 2, "C" => 1, _ => 0 }).collect(),
                other => vec![match text_of(other).to_ascii_uppercase().as_str() { "A" => 3, "B" => 2, "C" => 1, _ => 0 }],
            };
            let doc: Vec<Lex> = vector_of(v(0))?
                .into_iter()
                .filter_map(|mut l| {
                    l.pos.retain(|p| keep.contains(&p.1));
                    (!l.pos.is_empty()).then_some(l)
                })
                .collect();
            Value::text(&write_vector(&doc))
        }
        "JSON_TO_TSVECTOR" | "JSONB_TO_TSVECTOR" => {
            let doc = crate::json::document(r(0))?;
            let filter = text_of(r(1)).to_ascii_lowercase();
            let mut parts = Vec::new();
            json_text(&doc, &filter, &mut parts);
            Value::text(&write_vector(&text_to_vector(&parts.join(" "), cfg)))
        }
        "TSVECTOR_CONCAT" => {
            let (a, b) = (vector_of(v(0))?, vector_of(v(1))?);
            let shift = a.iter().flat_map(|l| l.pos.iter().map(|p| p.0)).max().unwrap_or(0);
            let b: Vec<Lex> = b.into_iter().map(|mut l| {
                l.pos.iter_mut().for_each(|p| p.0 = (u32::from(p.0) + u32::from(shift)).min(16383) as u16);
                l
            }).collect();
            Value::text(&write_vector(&merge(a.into_iter().chain(b).collect())))
        }
        "TSQUERY_AND" | "TSQUERY_OR" => match (query_of(v(0))?, query_of(v(1))?) {
            (Some(a), Some(b)) => query_text(Some(if name == "TSQUERY_AND" { Q::And(Box::new(a), Box::new(b)) } else { Q::Or(Box::new(a), Box::new(b)) })),
            (a, b) => query_text(a.or(b)),
        },
        "TSQUERY_PHRASE" => {
            let n = vals.get(2).and_then(|n| n.to_f64()).unwrap_or(1.0) as u32;
            match (query_of(v(0))?, query_of(v(1))?) {
                (Some(a), Some(b)) => query_text(Some(Q::Phrase(Box::new(a), Box::new(b), n))),
                (a, b) => query_text(a.or(b)),
            }
        }
        "TSQUERY_NOT" => query_text(query_of(v(0))?.map(|q| Q::Not(Box::new(q)))),
        _ => return Err(Error::runtime(format!("unknown function '{name}'"))),
    })
}

/// The lexemes of `text` as terms (the stop words dropped), for `plainto_tsquery`.
fn text_to_vector_terms(text: &str, cfg: Config) -> Vec<Q> {
    tokenize(text, cfg).into_iter().filter_map(|(l, _)| l).map(term).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stemmer_follows_snowball() {
        for (w, s) in [("running", "run"), ("cats", "cat"), ("fatness", "fat"), ("generously", "generous"), ("happily", "happili"), ("rats", "rat"), ("jumped", "jump"), ("consign", "consign"), ("agreed", "agre"), ("relational", "relat"), ("hopping", "hop"), ("sky", "sky"), ("skies", "sky")] {
            assert_eq!(stem(w), s, "{w}");
        }
    }

    #[test]
    fn vectors_and_queries_read_and_write() {
        let v = text_to_vector("a fat cat sat on a mat and ate a fat rat", Config::English);
        assert_eq!(write_vector(&v), "'ate':9 'cat':3 'fat':2,11 'mat':7 'rat':12 'sat':4");
        let q = normalise(parse_query("fat & (rats | cat)").unwrap().unwrap(), Config::English).unwrap();
        assert_eq!(write_query(&q), "'fat' & ( 'rat' | 'cat' )");
        assert!(eval(&q, &v).is_some());
        let phrase = parse_query("'fat' <-> 'cat'").unwrap().unwrap();
        assert!(eval(&phrase, &v).is_some());
        let none = parse_query("'cat' <-> 'fat'").unwrap().unwrap();
        assert!(eval(&none, &v).is_none());
    }
}
