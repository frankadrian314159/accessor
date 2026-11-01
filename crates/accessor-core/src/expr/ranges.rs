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
    Some(match name {
        "ISEMPTY" => (IsEmpty, 1, 1),
        "LOWER_INC" => (LowerInc, 1, 1),
        "UPPER_INC" => (UpperInc, 1, 1),
        "LOWER_INF" => (LowerInf, 1, 1),
        "UPPER_INF" => (UpperInf, 1, 1),
        "RANGE_MERGE" => (Merge, 2, 2),
        "__RANGE_LEFT" => (Op(RangeOp::Left), 2, 2),
        "__RANGE_RIGHT" => (Op(RangeOp::Right), 2, 2),
        "__RANGE_NOTRIGHT" => (Op(RangeOp::NotRight), 2, 2),
        "__RANGE_NOTLEFT" => (Op(RangeOp::NotLeft), 2, 2),
        "__RANGE_ADJACENT" => (Op(RangeOp::Adjacent), 2, 2),
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
    s.is_empty() || s.chars().any(|c| matches!(c, ',' | '(' | ')' | '[' | ']' | '"' | '\\') || c.is_whitespace())
}

fn quoted(s: &str) -> String {
    if needs_quotes(s) {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
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
    // two bounds, separated by a comma outside quotes
    let mut parts: Vec<(String, bool)> = vec![(String::new(), false)];
    let mut quoted = false;
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                quoted = !quoted;
                parts.last_mut()?.1 = true;
            }
            '\\' if quoted => parts.last_mut()?.0.push(it.next()?),
            ',' if !quoted => parts.push((String::new(), false)),
            c => parts.last_mut()?.0.push(c),
        }
    }
    if quoted || parts.len() != 2 {
        return None;
    }
    let bound = |(text, was_quoted): &(String, bool)| -> Option<String> {
        let t = if *was_quoted { text.clone() } else { text.trim().to_string() };
        if t.is_empty() && !*was_quoted {
            None
        } else {
            Some(t)
        }
    };
    Some(Range { empty: false, lo: bound(&parts[0]), lo_inc, hi: bound(&parts[1]), hi_inc })
}

fn is_range_text(v: &Value) -> Option<Range> {
    let Value::Text(s) = v else { return None };
    let t = s.trim();
    if !(t.starts_with('[') || t.starts_with('(') || t.eq_ignore_ascii_case("empty")) {
        return None;
    }
    parse(t)
}

fn cmp_bound(a: &str, b: &str) -> Ordering {
    if let (Ok(x), Ok(y)) = (a.parse::<i128>(), b.parse::<i128>()) {
        return x.cmp(&y);
    }
    if let (Ok(x), Ok(y)) = (a.parse::<f64>(), b.parse::<f64>()) {
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

fn union(a: &Range, b: &Range, merge: bool) -> Result<Range> {
    if a.empty {
        return Ok(b.clone());
    }
    if b.empty {
        return Ok(a.clone());
    }
    if !merge && !overlaps(a, b) && !adjacent(a, b) {
        return Err(Error::runtime("result of range union would not be contiguous"));
    }
    let lo = if lo_cmp(a, b) != Ordering::Greater { a } else { b };
    let hi = if hi_cmp(a, b) != Ordering::Less { a } else { b };
    Ok(Range { empty: false, lo: lo.lo.clone(), lo_inc: lo.lo_inc, hi: hi.hi.clone(), hi_inc: hi.hi_inc })
}

fn intersect(a: &Range, b: &Range) -> Range {
    if !overlaps(a, b) {
        return Range::empty();
    }
    let lo = if lo_cmp(a, b) != Ordering::Less { a } else { b };
    let hi = if hi_cmp(a, b) != Ordering::Greater { a } else { b };
    Range { empty: false, lo: lo.lo.clone(), lo_inc: lo.lo_inc, hi: hi.hi.clone(), hi_inc: hi.hi_inc }
}

fn minus(a: &Range, b: &Range) -> Result<Range> {
    if a.empty || b.empty || !overlaps(a, b) {
        return Ok(a.clone());
    }
    let starts_before = lo_cmp(a, b) == Ordering::Less;
    let ends_after = hi_cmp(a, b) == Ordering::Greater;
    if starts_before && ends_after {
        return Err(Error::runtime("result of range difference would not be contiguous"));
    }
    if !starts_before && !ends_after {
        return Ok(Range::empty());
    }
    if !starts_before {
        // the front of `a` is cut off
        Ok(Range { empty: false, lo: b.hi.clone(), lo_inc: !b.hi_inc, hi: a.hi.clone(), hi_inc: a.hi_inc })
    } else {
        Ok(Range { empty: false, lo: a.lo.clone(), lo_inc: a.lo_inc, hi: b.lo.clone(), hi_inc: !b.lo_inc })
    }
}

// ----- construction ----------------------------------------------------------------------------

fn bound_text(kind: Kind, v: &Value) -> String {
    let s = v.to_string();
    if kind == Kind::Date && s.len() > 10 && s.as_bytes().get(4) == Some(&b'-') {
        return s[..10].to_string();
    }
    s
}

fn next_bound(kind: Kind, s: &str) -> Result<String> {
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
    if let (Some(lo), Some(hi)) = (&r.lo, &r.hi) {
        if !r.empty && cmp_bound(lo, hi) == Ordering::Greater {
            return Err(Error::runtime("range lower bound must be less than or equal to range upper bound"));
        }
    }
    r.lo = r.lo.map(|b| bound_text(kind, &Value::text(&b)));
    r.hi = r.hi.map(|b| bound_text(kind, &Value::text(&b)));
    Ok(Value::text(&canonical(kind, r)?.to_string()))
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
fn element(v: &Value, like: &Range) -> Range {
    let mut s = v.to_string();
    let date_like = |b: &Option<String>| b.as_ref().is_some_and(|b| b.len() == 10 && b.as_bytes().get(4) == Some(&b'-'));
    if (date_like(&like.lo) || date_like(&like.hi)) && s.len() > 10 && s.as_bytes().get(4) == Some(&b'-') {
        s.truncate(10);
    }
    Range::point(s)
}

fn in_postgresql() -> bool {
    crate::dialect::current() == crate::dialect::Dialect::Postgresql
}

/// `a op b` when either side is a range written as text (PostgreSQL only), `None` when this is not a range operation.
pub(crate) fn try_op(op: RangeOp, a: &Value, b: &Value) -> Result<Option<Value>> {
    if !in_postgresql() {
        return Ok(None);
    }
    let (ra, rb) = (is_range_text(a), is_range_text(b));
    let (ra, rb) = match (op, ra, rb) {
        (RangeOp::Contains, Some(x), Some(y)) => (x, y),
        (RangeOp::Contains, Some(x), None) if !b.is_null() => {
            let e = element(b, &x);
            (x, e)
        }
        (RangeOp::ContainedBy, Some(x), Some(y)) => (x, y),
        (RangeOp::ContainedBy, None, Some(y)) if !a.is_null() => {
            let e = element(a, &y);
            (e, y)
        }
        (RangeOp::Contains | RangeOp::ContainedBy, _, _) => return Ok(None),
        (_, Some(x), Some(y)) => (x, y),
        _ => return Ok(None),
    };
    let t = |b: bool| Ok(Some(Value::Bool(b)));
    match op {
        RangeOp::Contains => t(contains(&ra, &rb)),
        RangeOp::ContainedBy => t(contains(&rb, &ra)),
        RangeOp::Overlap => t(overlaps(&ra, &rb)),
        RangeOp::Left => t(!ra.empty && !rb.empty && before(&ra, &rb)),
        RangeOp::Right => t(!ra.empty && !rb.empty && before(&rb, &ra)),
        RangeOp::NotRight => t(!ra.empty && !rb.empty && hi_cmp(&ra, &rb) != Ordering::Greater),
        RangeOp::NotLeft => t(!ra.empty && !rb.empty && lo_cmp(&ra, &rb) != Ordering::Less),
        RangeOp::Adjacent => t(adjacent(&ra, &rb)),
        RangeOp::Union => Ok(Some(Value::text(&union(&ra, &rb, false)?.to_string()))),
        RangeOp::Intersect => Ok(Some(Value::text(&intersect(&ra, &rb).to_string()))),
        RangeOp::Minus => Ok(Some(Value::text(&minus(&ra, &rb)?.to_string()))),
    }
}

/// `lower(r)` or `upper(r)` when the argument is a range (PostgreSQL only).
pub(crate) fn try_bound(v: &Value, upper: bool) -> Option<Value> {
    if !in_postgresql() {
        return None;
    }
    let t = v.to_string();
    let t = t.trim();
    if !(t.starts_with('[') || t.starts_with('(')) {
        return None;
    }
    let r = parse(t)?;
    let b = if upper { r.hi } else { r.lo };
    Some(b.map(|b| as_value(&b)).unwrap_or(Value::Null))
}

pub fn call(f: RangeFn, vals: &[Cow<'_, Value>]) -> Result<Value> {
    let range = |i: usize| -> Result<Range> {
        let s = vals[i].to_string();
        parse(&s).ok_or_else(|| Error::type_err(format!("malformed range literal: \"{s}\"")))
    };
    Ok(match f {
        RangeFn::Ctor(k) => return construct(k, vals),
        RangeFn::Cast(k) => {
            if vals[0].is_null() {
                return Ok(Value::Null);
            }
            return cast(k, &vals[0]);
        }
        RangeFn::IsEmpty => Value::Bool(range(0)?.empty),
        RangeFn::LowerInc => {
            let r = range(0)?;
            Value::Bool(!r.empty && r.lo_inc)
        }
        RangeFn::UpperInc => {
            let r = range(0)?;
            Value::Bool(!r.empty && r.hi_inc)
        }
        RangeFn::LowerInf => {
            let r = range(0)?;
            Value::Bool(!r.empty && r.lo.is_none())
        }
        RangeFn::UpperInf => {
            let r = range(0)?;
            Value::Bool(!r.empty && r.hi.is_none())
        }
        RangeFn::Merge => Value::text(&union(&range(0)?, &range(1)?, true)?.to_string()),
        RangeFn::Op(op) => {
            // these have no other meaning: both sides are ranges
            let (a, b) = (range(0)?, range(1)?);
            let t = |x: bool| Value::Bool(x);
            match op {
                RangeOp::Left => t(!a.empty && !b.empty && before(&a, &b)),
                RangeOp::Right => t(!a.empty && !b.empty && before(&b, &a)),
                RangeOp::NotRight => t(!a.empty && !b.empty && hi_cmp(&a, &b) != Ordering::Greater),
                RangeOp::NotLeft => t(!a.empty && !b.empty && lo_cmp(&a, &b) != Ordering::Less),
                RangeOp::Adjacent => t(adjacent(&a, &b)),
                _ => return Err(Error::runtime("not a range function")),
            }
        }
    })
}
