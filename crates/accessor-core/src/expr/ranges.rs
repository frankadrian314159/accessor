//! PostgreSQL's range types (`int4range`, `int8range`, `numrange`, `daterange`, `tsrange`, `tstzrange`).
//!
//! There is no data type for a range: a range is text in PostgreSQL's own form, `[1,5)`, `(,7]`, `empty`, and a column of a
//! range type is a text column. The functions below read that text, so they are the same whatever table the value came from.
//! The ranges of the discrete types (`int4range`, `int8range`, `daterange`) are put in canonical form (`[)`) when they are made
//! with a constructor or a cast; a range written into a column as a text literal is stored as written.

use std::borrow::Cow;
use std::cmp::Ordering;

use crate::error::{Error, Result};
use crate::value::{DataType, Value};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Int4,
    Int8,
    Num,
    Date,
    Ts,
    Tstz,
    /// A range made with `CREATE TYPE .. AS RANGE`: continuous, with no canonical form.
    Custom,
    /// The same over a floating-point subtype, whose bounds are written the way a float is.
    Float,
}

impl Kind {
    pub fn from_name(name: &str) -> Option<Kind> {
        Some(match name.to_ascii_lowercase().as_str() {
            "int4range" => Kind::Int4,
            "int8range" => Kind::Int8,
            "numrange" => Kind::Num,
            "daterange" => Kind::Date,
            "tsrange" => Kind::Ts,
            "tstzrange" => Kind::Tstz,
            _ => return None,
        })
    }

    fn discrete(self) -> bool {
        matches!(self, Kind::Int4 | Kind::Int8 | Kind::Date)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RangeOp {
    Contains,
    ContainedBy,
    Overlap,
    Left,
    Right,
    NotRight,
    NotLeft,
    Adjacent,
    Union,
    Intersect,
    Minus,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RangeFn {
    Ctor(Kind),
    Cast(Kind),
    /// `int4multirange(r, ..)`
    MultiCtor(Kind),
    /// `'{[1,3),[5,7)}'::int4multirange`
    MultiCast(Kind),
    /// `multirange(r)`
    AsMulti,
    IsEmpty,
    LowerInc,
    UpperInc,
    LowerInf,
    UpperInf,
    Merge,
    Op(RangeOp),
}

const KINDS: [(&str, Kind); 6] = [
    ("INT4RANGE", Kind::Int4),
    ("INT8RANGE", Kind::Int8),
    ("NUMRANGE", Kind::Num),
    ("DATERANGE", Kind::Date),
    ("TSRANGE", Kind::Ts),
    ("TSTZRANGE", Kind::Tstz),
];

const MULTI: [(&str, Kind); 6] = [
    ("INT4MULTIRANGE", Kind::Int4),
    ("INT8MULTIRANGE", Kind::Int8),
    ("NUMMULTIRANGE", Kind::Num),
    ("DATEMULTIRANGE", Kind::Date),
    ("TSMULTIRANGE", Kind::Ts),
    ("TSTZMULTIRANGE", Kind::Tstz),
];

pub fn lookup(name: &str) -> Option<(RangeFn, usize, usize)> {
    use RangeFn::*;
    for (n, k) in KINDS {
        if name == n {
            return Some((Ctor(k), 0, 3));
        }
        if name.strip_prefix("__RANGE_CAST_") == Some(n) {
            return Some((Cast(k), 1, 1));
        }
    }
    for (n, k) in MULTI {
        if name == n {
            return Some((MultiCtor(k), 0, usize::MAX));
        }
        if name.strip_prefix("__RANGE_CAST_") == Some(n) {
            return Some((MultiCast(k), 1, 1));
        }
    }
    // a range type of the user's
    let custom = |n: &str| crate::sql::usertypes::lookup(n).is_some_and(|t| t.range.is_some());
    let kind_of = |n: &str| {
        let sub = crate::sql::usertypes::lookup(n).and_then(|t| t.range).unwrap_or_default().to_ascii_lowercase();
        if matches!(sub.as_str(), "float4" | "float8" | "real" | "double" | "double precision" | "float") {
            Kind::Float
        } else {
            Kind::Custom
        }
    };
    if let Some(n) = name.strip_prefix("__RANGE_CAST_") {
        if custom(n) {
            return Some((Cast(kind_of(n)), 1, 1));
        }
        if crate::sql::usertypes::is_multirange_name(n) {
            return Some((MultiCast(Kind::Custom), 1, 1));
        }
    } else if custom(name) {
        return Some((Ctor(kind_of(name)), 0, 3));
    } else if crate::sql::usertypes::is_multirange_name(name) {
        return Some((MultiCtor(Kind::Custom), 0, usize::MAX));
    }
    if let Some(op) = mixed_name(name) {
        return Some((Op(op), 2, 2));
    }
    Some(match name {
        "ISEMPTY" => (IsEmpty, 1, 1),
        "LOWER_INC" => (LowerInc, 1, 1),
        "UPPER_INC" => (UpperInc, 1, 1),
        "LOWER_INF" => (LowerInf, 1, 1),
        "UPPER_INF" => (UpperInf, 1, 1),
        "RANGE_MERGE" => (Merge, 1, 2),
        "MULTIRANGE" => (AsMulti, 1, 1),
        "__RANGE_LEFT" | "RANGE_BEFORE" => (Op(RangeOp::Left), 2, 2),
        "__RANGE_RIGHT" | "RANGE_AFTER" => (Op(RangeOp::Right), 2, 2),
        "__RANGE_NOTRIGHT" | "RANGE_OVERLEFT" => (Op(RangeOp::NotRight), 2, 2),
        "__RANGE_NOTLEFT" | "RANGE_OVERRIGHT" => (Op(RangeOp::NotLeft), 2, 2),
        "__RANGE_ADJACENT" | "RANGE_ADJACENT" => (Op(RangeOp::Adjacent), 2, 2),
        "RANGE_CONTAINS" => (Op(RangeOp::Contains), 2, 2),
        "RANGE_CONTAINED_BY" => (Op(RangeOp::ContainedBy), 2, 2),
        "RANGE_OVERLAPS" => (Op(RangeOp::Overlap), 2, 2),
        "RANGE_UNION" => (Op(RangeOp::Union), 2, 2),
        "RANGE_INTERSECT" => (Op(RangeOp::Intersect), 2, 2),
        "RANGE_MINUS" => (Op(RangeOp::Minus), 2, 2),
        _ => return None,
    })
}

/// The operator behind `multirange_contains_elem`, `range_before_multirange`, `elem_contained_by_multirange` and the like.
fn mixed_name(name: &str) -> Option<RangeOp> {
    let parts: Vec<&str> = name.split('_').collect();
    if !name.contains("MULTIRANGE") || parts.len() < 3 {
        return None;
    }
    let kind = |w: &str| matches!(w, "RANGE" | "MULTIRANGE" | "ELEM");
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !kind(first) || !kind(last) {
        return None;
    }
    Some(match parts[1..parts.len() - 1].join("_").as_str() {
        "CONTAINS" => RangeOp::Contains,
        "CONTAINED_BY" => RangeOp::ContainedBy,
        "OVERLAPS" => RangeOp::Overlap,
        "BEFORE" => RangeOp::Left,
        "AFTER" => RangeOp::Right,
        "OVERLEFT" => RangeOp::NotRight,
        "OVERRIGHT" => RangeOp::NotLeft,
        "ADJACENT" => RangeOp::Adjacent,
        _ => return None,
    })
}

impl RangeFn {
    pub fn handles_null(self) -> bool {
        matches!(self, RangeFn::Ctor(_))
    }
}

/// A range: `lo` or `hi` is `None` for an infinite bound.
#[derive(Clone, Debug, PartialEq)]
struct Range {
    empty: bool,
    lo: Option<String>,
    lo_inc: bool,
    hi: Option<String>,
    hi_inc: bool,
}

impl Range {
    fn empty() -> Range {
        Range { empty: true, lo: None, lo_inc: false, hi: None, hi_inc: false }
    }

    fn point(v: String) -> Range {
        Range { empty: false, lo: Some(v.clone()), lo_inc: true, hi: Some(v), hi_inc: true }
    }
}

fn needs_quotes(s: &str) -> bool {
    s.is_empty() || s.eq_ignore_ascii_case("empty") || s.chars().any(|c| matches!(c, ',' | '(' | ')' | '[' | ']' | '"' | '\\') || c.is_whitespace())
}

fn quoted(s: &str) -> String {
    if needs_quotes(s) {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

impl std::fmt::Display for Range {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.empty {
            return f.write_str("empty");
        }
        write!(
            f,
            "{}{},{}{}",
            if self.lo_inc { '[' } else { '(' },
            self.lo.as_deref().map(quoted).unwrap_or_default(),
            self.hi.as_deref().map(quoted).unwrap_or_default(),
            if self.hi_inc { ']' } else { ')' }
        )
    }
}

/// Reads a range literal. Space around the whole literal is ignored, but space inside the brackets belongs to the bounds.
fn parse(s: &str) -> Option<Range> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("empty") {
        return Some(Range::empty());
    }
    let mut chars = s.chars();
    let lo_inc = match chars.next()? {
        '[' => true,
        '(' => false,
        _ => return None,
    };
    let hi_inc = match chars.next_back()? {
        ']' => true,
        ')' => false,
        _ => return None,
    };
    let inner = chars.as_str();
    // two bounds, separated by a comma outside quotes; a backslash takes the next character as it is, and a doubled quote
    // inside quotes is a quote
    let mut parts: Vec<(String, bool)> = vec![(String::new(), false)];
    let mut quoted = false;
    let mut it = inner.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' if quoted && it.peek() == Some(&'"') => {
                it.next();
                parts.last_mut()?.0.push('"');
            }
            '"' => {
                quoted = !quoted;
                parts.last_mut()?.1 = true;
            }
            '\\' => parts.last_mut()?.0.push(it.next()?),
            ',' if !quoted => parts.push((String::new(), false)),
            ')' | ']' if !quoted => return None,
            c => parts.last_mut()?.0.push(c),
        }
    }
    if quoted || parts.len() != 2 {
        return None;
    }
    let bound = |(text, was_quoted): &(String, bool)| -> Option<String> {
        if text.is_empty() && !*was_quoted {
            None
        } else {
            Some(text.clone())
        }
    };
    Some(Range { empty: false, lo: bound(&parts[0]), lo_inc, hi: bound(&parts[1]), hi_inc })
}

fn cmp_bound(a: &str, b: &str) -> Ordering {
    // numbers may have space around them; other bounds (text) are compared as they are
    let (ta, tb) = (a.trim(), b.trim());
    if let (Ok(x), Ok(y)) = (ta.parse::<i128>(), tb.parse::<i128>()) {
        return x.cmp(&y);
    }
    if let (Ok(x), Ok(y)) = (ta.parse::<f64>(), tb.parse::<f64>()) {
        return x.partial_cmp(&y).unwrap_or(Ordering::Equal);
    }
    a.cmp(b)
}

/// Compares two lower bounds (an infinite one is the smallest; of equal values the inclusive one starts first).
fn lo_cmp(a: &Range, b: &Range) -> Ordering {
    match (&a.lo, &b.lo) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
        (Some(x), Some(y)) => cmp_bound(x, y).then_with(|| b.lo_inc.cmp(&a.lo_inc)),
    }
}

/// Compares two upper bounds (an infinite one is the largest; of equal values the inclusive one ends last).
fn hi_cmp(a: &Range, b: &Range) -> Ordering {
    match (&a.hi, &b.hi) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Greater,
        (_, None) => Ordering::Less,
        (Some(x), Some(y)) => cmp_bound(x, y).then_with(|| a.hi_inc.cmp(&b.hi_inc)),
    }
}

/// Does `a` end before `b` starts?
fn before(a: &Range, b: &Range) -> bool {
    match (&a.hi, &b.lo) {
        (Some(h), Some(l)) => match cmp_bound(h, l) {
            Ordering::Less => true,
            Ordering::Equal => !(a.hi_inc && b.lo_inc),
            Ordering::Greater => false,
        },
        _ => false,
    }
}

fn overlaps(a: &Range, b: &Range) -> bool {
    !a.empty && !b.empty && !before(a, b) && !before(b, a)
}

fn adjacent(a: &Range, b: &Range) -> bool {
    let touch = |x: &Range, y: &Range| match (&x.hi, &y.lo) {
        (Some(h), Some(l)) => cmp_bound(h, l) == Ordering::Equal && x.hi_inc != y.lo_inc,
        _ => false,
    };
    !a.empty && !b.empty && (touch(a, b) || touch(b, a))
}

fn contains(a: &Range, b: &Range) -> bool {
    if b.empty {
        return true;
    }
    !a.empty && lo_cmp(a, b) != Ordering::Greater && hi_cmp(a, b) != Ordering::Less
}

fn intersect(a: &Range, b: &Range) -> Range {
    if !overlaps(a, b) {
        return Range::empty();
    }
    let lo = if lo_cmp(a, b) != Ordering::Less { a } else { b };
    let hi = if hi_cmp(a, b) != Ordering::Greater { a } else { b };
    Range { empty: false, lo: lo.lo.clone(), lo_inc: lo.lo_inc, hi: hi.hi.clone(), hi_inc: hi.hi_inc }
}

// ----- construction ----------------------------------------------------------------------------

fn bound_text(kind: Kind, v: &Value) -> String {
    let s = v.to_string();
    if kind == Kind::Float {
        if let Ok(f) = s.trim().parse::<f64>() {
            return f.to_string();
        }
    }
    if kind == Kind::Date && s.len() > 10 && s.as_bytes().get(4) == Some(&b'-') {
        return s[..10].to_string();
    }
    s
}

/// Does the text read as a value of the subtype of `kind`?
fn check_bound(kind: Kind, b: &str) -> Result<()> {
    match kind {
        Kind::Int4 | Kind::Int8 => {
            let n: i128 = b.parse().map_err(|_| Error::type_err(format!("invalid input syntax for type {}: \"{b}\"", if kind == Kind::Int4 { "integer" } else { "bigint" })))?;
            if kind == Kind::Int4 && i32::try_from(n).is_err() {
                return Err(Error::runtime("value out of range for type integer"));
            }
            if kind == Kind::Int8 && i64::try_from(n).is_err() {
                return Err(Error::runtime("value out of range for type bigint"));
            }
        }
        Kind::Num | Kind::Float => {
            if b.parse::<f64>().is_err() && !b.eq_ignore_ascii_case("nan") {
                return Err(Error::type_err(format!("invalid input syntax for type numeric: \"{b}\"")));
            }
        }
        Kind::Date => {
            if b.eq_ignore_ascii_case("infinity") || b.eq_ignore_ascii_case("-infinity") {
                return Ok(());
            }
            let year: i64 = b.split('-').next().and_then(|y| y.parse().ok()).ok_or_else(|| Error::type_err(format!("invalid input syntax for type date: \"{b}\"")))?;
            if year > 5_874_897 {
                return Err(Error::runtime("date out of range"));
            }
        }
        _ => {}
    }
    Ok(())
}

fn next_bound(kind: Kind, s: &str) -> Result<String> {
    if s.ends_with("infinity") {
        return Ok(s.to_string());
    }
    match kind {
        Kind::Int4 | Kind::Int8 => {
            let n: i128 = s.parse().map_err(|_| Error::type_err(format!("invalid input syntax for type integer: \"{s}\"")))?;
            if kind == Kind::Int4 && n + 1 > i32::MAX as i128 {
                return Err(Error::runtime("integer out of range"));
            }
            Ok((n + 1).to_string())
        }
        _ => {
            let bad = || Error::type_err(format!("invalid input syntax for type date: \"{s}\""));
            let mut p = s.get(..10).ok_or_else(bad)?.split('-');
            let (y, m, d) = (p.next(), p.next(), p.next());
            let (Some(y), Some(m), Some(d)) = (y, m, d) else { return Err(bad()) };
            let (y, m, d): (i64, u32, u32) = (y.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?, d.parse().map_err(|_| bad())?);
            let (y, m, d) = crate::datetime::civil_from_days(crate::datetime::days_from_civil(y, m, d) + 1);
            Ok(format!("{y:04}-{m:02}-{d:02}"))
        }
    }
}

/// Puts the range in canonical form for its kind, and turns one that holds no value into `empty`.
fn canonical(kind: Kind, mut r: Range) -> Result<Range> {
    if r.empty {
        return Ok(r);
    }
    if kind.discrete() {
        if let (Some(lo), false) = (&r.lo, r.lo_inc) {
            r.lo = Some(next_bound(kind, lo)?);
            r.lo_inc = true;
        }
        if let (Some(hi), true) = (&r.hi, r.hi_inc) {
            r.hi = Some(next_bound(kind, hi)?);
            r.hi_inc = false;
        }
    }
    if r.lo.is_none() {
        r.lo_inc = false;
    }
    if r.hi.is_none() {
        r.hi_inc = false;
    }
    if let (Some(lo), Some(hi)) = (&r.lo, &r.hi) {
        match cmp_bound(lo, hi) {
            Ordering::Greater => return Err(Error::runtime("range lower bound must be less than or equal to range upper bound")),
            Ordering::Equal if !(r.lo_inc && r.hi_inc) => return Ok(Range::empty()),
            _ => {}
        }
    }
    Ok(r)
}

fn construct(kind: Kind, vals: &[Cow<'_, Value>]) -> Result<Value> {
    if vals.is_empty() {
        return Ok(Value::text("empty"));
    }
    if vals.len() == 1 {
        return Err(Error::runtime("a range takes no arguments or two or three"));
    }
    let flags = match vals.get(2) {
        None => "[)".to_string(),
        Some(v) if v.is_null() => return Err(Error::runtime("range constructor flags argument must not be null")),
        Some(v) => v.to_string(),
    };
    let (lo_inc, hi_inc) = match flags.as_str() {
        "[)" => (true, false),
        "[]" => (true, true),
        "(]" => (false, true),
        "()" => (false, false),
        _ => return Err(Error::runtime(format!("invalid range bound flags: \"{flags}\" (the flags must be [), [], (] or ())"))),
    };
    let bound = |v: &Value| if v.is_null() { None } else { Some(bound_text(kind, v)) };
    let r = Range { empty: false, lo: bound(&vals[0]), lo_inc, hi: bound(&vals[1]), hi_inc };
    Ok(Value::text(&canonical(kind, r)?.to_string()))
}

fn cast(kind: Kind, v: &Value) -> Result<Value> {
    let s = v.to_string();
    let mut r = parse(&s).ok_or_else(|| Error::type_err(format!("malformed range literal: \"{s}\"")))?;
    // the bounds of a built-in range are numbers or dates, which may have space around them
    let tidy = |b: String| if kind == Kind::Custom { b } else { bound_text(kind, &Value::text(b.trim())) };
    r.lo = r.lo.map(tidy);
    r.hi = r.hi.map(tidy);
    if let (Some(lo), Some(hi)) = (&r.lo, &r.hi) {
        if !r.empty && cmp_bound(lo, hi) == Ordering::Greater {
            return Err(Error::runtime("range lower bound must be less than or equal to range upper bound"));
        }
    }
    for b in r.lo.iter().chain(r.hi.iter()) {
        check_bound(kind, b)?;
    }
    Ok(Value::text(&canonical(kind, r)?.to_string()))
}

// ----- lists of ranges (a multirange is a sorted list of ranges that neither overlap nor touch) --------

/// Sorts the ranges, drops the empty ones and joins those that overlap or touch.
fn norm(mut v: Vec<Range>) -> Vec<Range> {
    v.retain(|r| !r.empty);
    v.sort_by(lo_cmp);
    let mut out: Vec<Range> = Vec::with_capacity(v.len());
    for r in v {
        match out.last_mut() {
            Some(last) if overlaps(last, &r) || adjacent(last, &r) => {
                if hi_cmp(&r, last) == Ordering::Greater {
                    last.hi = r.hi;
                    last.hi_inc = r.hi_inc;
                }
            }
            _ => out.push(r),
        }
    }
    out
}

fn subtract(p: &Range, y: &Range) -> Vec<Range> {
    if !overlaps(p, y) {
        return vec![p.clone()];
    }
    let mut out = Vec::new();
    if lo_cmp(p, y) == Ordering::Less {
        out.push(Range { empty: false, lo: p.lo.clone(), lo_inc: p.lo_inc, hi: y.lo.clone(), hi_inc: !y.lo_inc });
    }
    if hi_cmp(p, y) == Ordering::Greater {
        out.push(Range { empty: false, lo: y.hi.clone(), lo_inc: !y.hi_inc, hi: p.hi.clone(), hi_inc: p.hi_inc });
    }
    out
}

fn l_minus(a: &[Range], b: &[Range]) -> Vec<Range> {
    let mut out = Vec::new();
    for x in a {
        let mut pieces = vec![x.clone()];
        for y in b {
            pieces = pieces.iter().flat_map(|p| subtract(p, y)).collect();
        }
        out.extend(pieces);
    }
    norm(out)
}

fn l_intersect(a: &[Range], b: &[Range]) -> Vec<Range> {
    norm(a.iter().flat_map(|x| b.iter().map(move |y| intersect(x, y))).collect())
}

/// Reads a range or a multirange (`{[1,3),[5,7)}`); the flag says which it was.
fn read_ranges(s: &str) -> Option<(Vec<Range>, bool)> {
    let t = s.trim();
    if let Some(inner) = t.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
        let mut out = Vec::new();
        let (mut depth_quote, mut start) = (false, 0usize);
        let bytes: Vec<char> = inner.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                '"' => depth_quote = !depth_quote,
                '\\' if depth_quote => i += 1,
                ',' if !depth_quote && i > 0 && matches!(bytes[i - 1], ')' | ']') => {
                    out.push(bytes[start..i].iter().collect::<String>());
                    start = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        if !inner.trim().is_empty() {
            out.push(bytes[start..].iter().collect::<String>());
        }
        let ranges: Option<Vec<Range>> = out.iter().map(|p| parse(p)).collect();
        return Some((norm(ranges?), true));
    }
    if !(t.starts_with('[') || t.starts_with('(') || t.eq_ignore_ascii_case("empty")) {
        return None;
    }
    let r = parse(t)?;
    Some((if r.empty { Vec::new() } else { vec![r] }, false))
}

/// The elements of the text of an array of ranges (`"[1,2)","[3,4)"` or `[1,2),[3,4)`), without their quotes.
fn split_array_elements(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut cur, mut quoted, mut chars) = (String::new(), false, inner.chars().peekable());
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => cur.extend(chars.next()),
            ',' if !quoted && cur.chars().last().is_none_or(|l| matches!(l, ')' | ']')) && !cur.is_empty() => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// A text that is a multirange (`{[1,3),[5,7)}`, `{}`), in canonical form.
pub(crate) fn as_multirange(s: &str) -> Option<String> {
    let (list, multi) = read_ranges(s)?;
    multi.then(|| format!("{{{}}}", list.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(",")))
}

fn write_ranges(v: &[Range], multi: bool) -> Value {
    if multi {
        Value::Multirange(format!("{{{}}}", v.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(",")).as_str().into())
    } else {
        Value::text(&v.first().map_or("empty".to_string(), |r| r.to_string()))
    }
}

fn shape(v: &Value) -> Option<(Vec<Range>, bool)> {
    match v {
        Value::Text(s) | Value::Multirange(s) => read_ranges(s),
        _ => None,
    }
}

fn multi_cast(kind: Kind, v: &Value) -> Result<Value> {
    let s = v.to_string();
    // an array of ranges (`{"[1,2)","[5,6)"}`, from VARIADIC) has its ranges in quotes
    let unquoted = || {
        let inner = s.trim().strip_prefix('{')?.strip_suffix('}')?;
        if !(inner.starts_with('"') && inner.ends_with('"')) {
            return None;
        }
        let items: Option<Vec<Range>> = split_array_elements(inner).iter().map(|x| parse(x)).collect();
        Some((norm(items?), true))
    };
    let (list, _) = read_ranges(&s).or_else(unquoted).ok_or_else(|| Error::type_err(format!("malformed multirange literal: \"{s}\"")))?;
    let mut out = Vec::new();
    for r in list {
        let one = cast(kind, &Value::text(&r.to_string()))?;
        out.extend(read_ranges(&one.to_string()).map(|x| x.0).unwrap_or_default());
    }
    Ok(write_ranges(&norm(out), true))
}

// ----- operators -------------------------------------------------------------------------------

fn as_value(s: &str) -> Value {
    if let Ok(n) = s.parse::<i64>() {
        return Value::Int(n);
    }
    if s.parse::<f64>().is_ok() {
        if let Ok(d) = Value::text(s).coerce_to(DataType::Decimal) {
            return d;
        }
    }
    Value::text(s)
}

/// The element `v` as a one-point range, written like the bounds of `like`.
fn element(v: &Value, like: &[Range]) -> Vec<Range> {
    let mut s = v.to_string();
    let date_like = |b: &Option<String>| b.as_ref().is_some_and(|b| b.len() == 10 && b.as_bytes().get(4) == Some(&b'-'));
    if like.iter().any(|r| date_like(&r.lo) || date_like(&r.hi)) && s.len() > 10 && s.as_bytes().get(4) == Some(&b'-') {
        s.truncate(10);
    }
    vec![Range::point(s)]
}

/// Orders two texts that are ranges (or two multiranges) the way PostgreSQL does: an empty one first, then by the lower
/// bound and by the upper bound. `None` when they are not both ranges.
pub(crate) fn order(a: &str, b: &str) -> Option<Ordering> {
    let bracket = |s: &str| matches!(s.trim_start().as_bytes().first(), Some(b'[' | b'(' | b'{'));
    if !(bracket(a) || bracket(b)) || !in_postgresql() {
        return None;
    }
    let (la, ma) = read_ranges(a)?;
    let (lb, mb) = read_ranges(b)?;
    // a bare {} beside a plain array is not a multirange
    if (ma && la.is_empty() && !mb && lb.is_empty() && b.trim() != "empty") || (mb && lb.is_empty() && !ma && la.is_empty() && a.trim() != "empty") {
        return None;
    }
    for (x, y) in la.iter().zip(lb.iter()) {
        let o = lo_cmp(x, y).then_with(|| hi_cmp(x, y));
        if o != Ordering::Equal {
            return Some(o);
        }
    }
    Some(la.len().cmp(&lb.len()))
}

fn in_postgresql() -> bool {
    crate::dialect::current() == crate::dialect::Dialect::Postgresql
}

/// `a op b` for ranges and multiranges. A value on the other side of `@>` / `<@` may be an element. `None` when the
/// values are not ranges.
fn apply(op: RangeOp, a: &Value, b: &Value) -> Result<Option<Value>> {
    let (sa, sb) = (shape(a), shape(b));
    // `{}` alone is an empty array as much as an empty multirange: it is a multirange only beside one that has ranges
    let bare = |v: &Value| matches!(v, Value::Text(s) if s.trim() == "{}");
    let has_ranges = |s: &Option<(Vec<Range>, bool)>| s.as_ref().is_some_and(|x| !x.0.is_empty() || !x.1);
    // (the operators that arrays have too; beside a number or a date the `{}` is a multirange)
    let like_array = |v: &Value| matches!(v, Value::Text(_) | Value::Array(_));
    let arrays_have = matches!(op, RangeOp::Contains | RangeOp::ContainedBy | RangeOp::Overlap);
    if arrays_have && ((bare(a) && !has_ranges(&sb) && like_array(b)) || (bare(b) && !has_ranges(&sa) && like_array(a))) {
        return Ok(None);
    }
    let ((la, ma), (lb, mb)) = match (op, sa, sb) {
        (RangeOp::Contains, Some(x), Some(y)) => (x, y),
        (RangeOp::Contains, Some(x), None) if !b.is_null() => {
            let e = element(b, &x.0);
            (x, (e, false))
        }
        (RangeOp::ContainedBy, Some(x), Some(y)) => (x, y),
        (RangeOp::ContainedBy, None, Some(y)) if !a.is_null() => {
            let e = element(a, &y.0);
            ((e, false), y)
        }
        (RangeOp::Contains | RangeOp::ContainedBy, _, _) => return Ok(None),
        (_, Some(x), Some(y)) => (x, y),
        _ => return Ok(None),
    };
    let multi = ma || mb;
    let t = |x: bool| Ok(Some(Value::Bool(x)));
    let (fa, ea) = (la.first(), la.last());
    let (fb, eb) = (lb.first(), lb.last());
    let both = !la.is_empty() && !lb.is_empty();
    let list = |v: Vec<Range>, strict: bool| -> Result<Option<Value>> {
        if !multi && strict && v.len() > 1 {
            return Err(Error::runtime("result of range operation would not be contiguous"));
        }
        Ok(Some(write_ranges(&v, multi)))
    };
    match op {
        RangeOp::Contains => t(lb.iter().all(|y| la.iter().any(|x| contains(x, y)))),
        RangeOp::ContainedBy => t(la.iter().all(|y| lb.iter().any(|x| contains(x, y)))),
        RangeOp::Overlap => t(la.iter().any(|x| lb.iter().any(|y| overlaps(x, y)))),
        RangeOp::Left => t(both && before(ea.expect("not empty"), fb.expect("not empty"))),
        RangeOp::Right => t(both && before(eb.expect("not empty"), fa.expect("not empty"))),
        RangeOp::NotRight => t(both && hi_cmp(ea.expect("not empty"), eb.expect("not empty")) != Ordering::Greater),
        RangeOp::NotLeft => t(both && lo_cmp(fa.expect("not empty"), fb.expect("not empty")) != Ordering::Less),
        RangeOp::Adjacent => t(both && (adjacent(ea.expect("not empty"), fb.expect("not empty")) || adjacent(eb.expect("not empty"), fa.expect("not empty")))),
        RangeOp::Union => {
            let all: Vec<Range> = la.iter().chain(lb.iter()).cloned().collect();
            let joined = norm(all);
            if !multi && joined.len() > 1 {
                return Err(Error::runtime("result of range union would not be contiguous"));
            }
            list(joined, false)
        }
        RangeOp::Intersect => list(l_intersect(&la, &lb), false),
        RangeOp::Minus => {
            let out = l_minus(&la, &lb);
            if !multi && out.len() > 1 {
                return Err(Error::runtime("result of range difference would not be contiguous"));
            }
            list(out, false)
        }
    }
}

/// `a op b` when either side is a range written as text (PostgreSQL only), `None` when this is not a range operation.
pub(crate) fn try_op(op: RangeOp, a: &Value, b: &Value) -> Result<Option<Value>> {
    if !in_postgresql() {
        return Ok(None);
    }
    apply(op, a, b)
}

/// `lower(r)` or `upper(r)` when the argument is a range or multirange (PostgreSQL only).
pub(crate) fn try_bound(v: &Value, upper: bool) -> Option<Value> {
    if !in_postgresql() {
        return None;
    }
    let (list, _) = shape(v)?;
    let r = if upper { list.last() } else { list.first() };
    Some(r.and_then(|r| if upper { r.hi.as_ref() } else { r.lo.as_ref() }).map(|b| as_value(b)).unwrap_or(Value::Null))
}

/// The ranges of a multirange, for `unnest`.
pub(crate) fn multirange_items(v: &Value) -> Option<Vec<Value>> {
    match shape(v)? {
        (list, true) => Some(list.iter().map(|r| Value::text(&r.to_string())).collect()),
        _ => None,
    }
}

/// The ranges that are left of `a` when `b` is taken out, one row each (`range_minus_multi`).
pub(crate) fn minus_multi(a: &Value, b: &Value) -> Result<Vec<Value>> {
    if a.is_null() || b.is_null() {
        return Ok(Vec::new());
    }
    let (Some((la, _)), Some((lb, _))) = (shape(a), shape(b)) else { return Err(Error::type_err("this function takes ranges")) };
    Ok(l_minus(&la, &lb).iter().map(|r| Value::text(&r.to_string())).collect())
}

thread_local! {
    /// The range or multirange type of the columns made in this session: (table, column) to type name.
    static COLUMNS: std::cell::RefCell<std::collections::HashMap<(String, String), String>> = std::cell::RefCell::new(Default::default());
}

pub(crate) fn register_column(table: &str, column: &str, type_name: &str) {
    COLUMNS.with(|c| c.borrow_mut().insert((table.to_ascii_lowercase(), column.to_ascii_lowercase()), type_name.to_string()));
}

pub(crate) fn forget_table(table: &str) {
    let t = table.to_ascii_lowercase();
    COLUMNS.with(|c| c.borrow_mut().retain(|(tb, _), _| *tb != t));
}

/// The type of the columns called `column` in the tables of this session, when they all have the same one (for `NEW.col`
/// in a trigger, where the table is not named).
pub(crate) fn column_type_anywhere(column: &str) -> Option<String> {
    let col = column.to_ascii_lowercase();
    COLUMNS.with(|c| {
        let c = c.borrow();
        let mut types = c.iter().filter(|((_, n), _)| *n == col).map(|(_, t)| t);
        let first = types.next()?;
        types.all(|t| t == first).then(|| first.clone())
    })
}

pub(crate) fn column_type(table: &str, column: &str) -> Option<String> {
    COLUMNS.with(|c| {
        let c = c.borrow();
        if c.is_empty() {
            return None;
        }
        c.get(&(table.to_ascii_lowercase(), column.to_ascii_lowercase())).cloned()
    })
}

/// A text put in the form of the range or multirange type `tn`.
pub(crate) fn normalize(tn: &str, v: &Value) -> Result<Value> {
    if super::composite::is_composite(tn) {
        return Ok(Value::text(&super::composite::cast_text(tn, &v.to_string())?));
    }
    let Some(f) = cast_function_for(tn) else { return Ok(v.clone()) };
    let Some((func, _, _)) = lookup(&f) else { return Ok(v.clone()) };
    call(func, &[Cow::Borrowed(v)])
}

/// Does `text` read as a value of the range or multirange type `name`? `None` when `name` is not one.
pub(crate) fn input_check(name: &str, text: &str) -> Option<std::result::Result<(), String>> {
    let f = cast_function_for(name)?;
    let (func, _, _) = lookup(&f)?;
    let args = [Cow::Owned(Value::text(text))];
    Some(call(func, &args).map(|_| ()).map_err(|e| {
        let m = e.to_string();
        m.strip_prefix("Type mismatch: ").unwrap_or(&m).to_string()
    }))
}

/// The intersection of two ranges (the step of `range_intersect_agg`).
pub(crate) fn intersect_values(a: &Value, b: &Value) -> Result<Value> {
    apply(RangeOp::Intersect, a, b)?.ok_or_else(|| Error::type_err("range_intersect_agg needs ranges"))
}

/// The multirange of the ranges (the result of `range_agg`).
pub(crate) fn multirange_of(items: &[Value]) -> Result<Value> {
    let mut all = Vec::new();
    for v in items {
        if v.is_null() {
            continue;
        }
        let (list, _) = shape(v).ok_or_else(|| Error::type_err("range_agg needs ranges"))?;
        all.extend(list);
    }
    Ok(write_ranges(&norm(all), true))
}

pub fn call(f: RangeFn, vals: &[Cow<'_, Value>]) -> Result<Value> {
    let ranges = |i: usize| -> Result<(Vec<Range>, bool)> {
        let s = vals[i].to_string();
        read_ranges(&s).ok_or_else(|| Error::type_err(format!("malformed range literal: \"{s}\"")))
    };
    Ok(match f {
        RangeFn::Ctor(k) => return construct(k, vals),
        RangeFn::Cast(k) => {
            if vals[0].is_null() {
                return Ok(Value::Null);
            }
            return cast(k, &vals[0]);
        }
        RangeFn::MultiCtor(k) => {
            let mut all = Vec::new();
            // nummultirange(VARIADIC '{"[1,2)","[3,4)"}'::numrange[]): the ranges are the elements of an array
            let from_array: Vec<Value>;
            let vals: &[Cow<'_, Value>] = match vals {
                [one] if !one.is_null() && { let t = one.to_string(); let t = t.trim(); t == "{}" || t.starts_with("{\"") } => {
                    let s = one.to_string();
                    let inner = s.trim().trim_start_matches('{').trim_end_matches('}');
                    from_array = split_array_elements(inner).into_iter().map(|x| Value::text(&x)).collect();
                    &from_array.iter().map(Cow::Borrowed).collect::<Vec<_>>()
                }
                _ => vals,
            };
            for v in vals {
                if v.is_null() {
                    continue;
                }
                let one = cast(k, v)?;
                all.extend(read_ranges(&one.to_string()).map(|x| x.0).unwrap_or_default());
            }
            write_ranges(&norm(all), true)
        }
        RangeFn::MultiCast(k) => {
            if vals[0].is_null() {
                return Ok(Value::Null);
            }
            return multi_cast(k, &vals[0]);
        }
        RangeFn::AsMulti => write_ranges(&ranges(0)?.0, true),
        RangeFn::IsEmpty => Value::Bool(ranges(0)?.0.is_empty()),
        RangeFn::LowerInc => Value::Bool(ranges(0)?.0.first().is_some_and(|r| r.lo_inc)),
        RangeFn::UpperInc => Value::Bool(ranges(0)?.0.last().is_some_and(|r| r.hi_inc)),
        RangeFn::LowerInf => Value::Bool(ranges(0)?.0.first().is_some_and(|r| r.lo.is_none())),
        RangeFn::UpperInf => Value::Bool(ranges(0)?.0.last().is_some_and(|r| r.hi.is_none())),
        RangeFn::Merge => {
            let mut all = ranges(0)?.0;
            if vals.len() > 1 {
                all.extend(ranges(1)?.0);
            }
            match (all.iter().min_by(|a, b| lo_cmp(a, b)), all.iter().max_by(|a, b| hi_cmp(a, b))) {
                (Some(lo), Some(hi)) => write_ranges(&[Range { empty: false, lo: lo.lo.clone(), lo_inc: lo.lo_inc, hi: hi.hi.clone(), hi_inc: hi.hi_inc }], false),
                _ => Value::text("empty"),
            }
        }
        RangeFn::Op(op) => {
            if vals[0].is_null() || vals[1].is_null() {
                return Ok(Value::Null);
            }
            match apply(op, &vals[0], &vals[1])? {
                Some(v) => v,
                None => return Err(Error::type_err("this function takes ranges")),
            }
        }
    })
}

/// The function that casts to the range or multirange type called `tn`, if it is one.
pub(crate) fn cast_function_for(tn: &str) -> Option<String> {
    let up = tn.to_ascii_uppercase();
    let known = KINDS.iter().any(|(n, _)| *n == up) || MULTI.iter().any(|(n, _)| *n == up);
    let custom = crate::sql::usertypes::lookup(tn).is_some_and(|t| t.range.is_some()) || crate::sql::usertypes::is_multirange_name(tn);
    (known || custom).then(|| format!("__RANGE_CAST_{up}"))
}
