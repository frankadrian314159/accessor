//! More of PostgreSQL's built-in functions: string helpers (`strpos`, `to_hex`, `btrim`, `chr`, `starts_with`,
//! `quote_nullable`), regular-expression arrays (`regexp_match`, `regexp_split_to_array`), the JSON builders and
//! readers (`json_build_object`, `json_extract_path`, `jsonb_exists`, `jsonb_pretty`, ...), array reordering
//! (`array_reverse`, `array_sort`), size formatting and a few numeric functions.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::sync::Arc;

use num_bigint::BigInt;
use serde_json::Value as J;

use crate::error::{Error, Result};
use crate::json;
use crate::value::{DataType, Value};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MoreFn {
    Strpos,
    ToHex,
    ToBin,
    ToOct,
    Btrim,
    Chr,
    StartsWith,
    QuoteNullable,
    QuoteLiteral,
    Normalize,
    Casefold,
    Unistr,
    NumNulls,
    NumNonnulls,
    RegexpMatch,
    RegexpSplitToArray,
    Factorial,
    Lgamma,
    SizePretty,
    SizeBytes,
    JsonBuildObject,
    JsonBuildArray,
    JsonExtractPath,
    JsonExtractPathText,
    JsonbExists,
    JsonbExistsAny,
    JsonbExistsAll,
    JsonbPretty,
    JsonbContains,
    JsonbContained,
    JsonbDelete,
    JsonbDeletePath,
    JsonScalar,
    JsonSerialize,
    JsonExists,
    ArrayToJson,
    JsonbObject,
    ArrayReverse,
    ArraySort,
    ArrayShuffle,
    ArraySample,
    UuidVersion,
    UuidTimestamp,
    UuidV7,
    UuidV4,
    JustifyDays,
    JustifyHours,
    JustifyInterval,
    HashText,
    HashTextExtended,
    HashInt,
    HashIntExtended,
    SatisfiesHashPartition,
    ToRegcollation,
    EnumRange,
    EnumFirst,
    EnumLast,
    Populate,
    LoCreate,
    LoCreat,
    LoFromBytea,
    ByteaIn,
    JsonpathIn,
    LoGet,
    LoPut,
    LoUnlink,
    LoImport,
    LoExport,
    LoOpen,
    LoClose,
    LoRead,
    LoWrite,
    LoLseek,
    LoTell,
    LoTruncate,
    Xpath,
    XmlValue,
    ConvertFrom,
    ConvertTo,
    XmlNotNull,
    XmlElement,
    XmlForest,
    XmlPi,
    XmlParse,
    XmlSerialize,
    XmlRoot,
    XmlConcat,
    XmlComment,
    XmlWellFormed,
    XmlWellFormedDocument,
    XmlWellFormedContent,
    XpathExists,
    RestoreStats,
    ClearStats,
    Null,
}

pub fn lookup(name: &str) -> Option<(MoreFn, usize, usize)> {
    use MoreFn::*;
    Some(match name {
        "STRPOS" => (Strpos, 2, 2),
        "TO_HEX" => (ToHex, 1, 1),
        "TO_BIN" => (ToBin, 1, 1),
        "TO_OCT" => (ToOct, 1, 1),
        "BTRIM" => (Btrim, 1, 2),
        "CHR" => (Chr, 1, 1),
        "STARTS_WITH" => (StartsWith, 2, 2),
        "QUOTE_NULLABLE" => (QuoteNullable, 1, 1),
        "QUOTE_LITERAL" => (QuoteLiteral, 1, 1),
        "NORMALIZE" => (Normalize, 1, 2),
        "CASEFOLD" => (Casefold, 1, 1),
        "UNISTR" => (Unistr, 1, 1),
        "NUM_NULLS" => (NumNulls, 1, usize::MAX),
        "NUM_NONNULLS" => (NumNonnulls, 1, usize::MAX),
        "REGEXP_MATCH" => (RegexpMatch, 2, 3),
        "REGEXP_SPLIT_TO_ARRAY" => (RegexpSplitToArray, 2, 3),
        "FACTORIAL" => (Factorial, 1, 1),
        "LGAMMA" => (Lgamma, 1, 1),
        "PG_SIZE_PRETTY" => (SizePretty, 1, 1),
        "PG_SIZE_BYTES" => (SizeBytes, 1, 1),
        "JSON_BUILD_OBJECT" | "JSONB_BUILD_OBJECT" => (JsonBuildObject, 0, usize::MAX),
        "JSON_BUILD_ARRAY" | "JSONB_BUILD_ARRAY" => (JsonBuildArray, 0, usize::MAX),
        "JSON_EXTRACT_PATH" | "JSONB_EXTRACT_PATH" => (JsonExtractPath, 1, usize::MAX),
        "JSON_EXTRACT_PATH_TEXT" | "JSONB_EXTRACT_PATH_TEXT" => (JsonExtractPathText, 1, usize::MAX),
        "JSONB_EXISTS" => (JsonbExists, 2, 2),
        "JSONB_EXISTS_ANY" => (JsonbExistsAny, 2, 2),
        "JSONB_EXISTS_ALL" => (JsonbExistsAll, 2, 2),
        "JSONB_PRETTY" => (JsonbPretty, 1, 1),
        "JSONB_CONTAINS" => (JsonbContains, 2, 2),
        "JSONB_CONTAINED" => (JsonbContained, 2, 2),
        "JSONB_DELETE" => (JsonbDelete, 2, 2),
        "JSONB_DELETE_PATH" => (JsonbDeletePath, 2, 2),
        "JSON_SCALAR" => (JsonScalar, 1, 1),
        "JSON_SERIALIZE" => (JsonSerialize, 1, 1),
        "JSON_EXISTS" => (JsonExists, 2, 2),
        "ARRAY_TO_JSON" => (ArrayToJson, 1, 2),
        "JSONB_OBJECT" | "JSON_OBJECT_AGG_PAIRS" => (JsonbObject, 1, 2),
        "ARRAY_REVERSE" => (ArrayReverse, 1, 1),
        "ARRAY_SORT" => (ArraySort, 1, 3),
        "ARRAY_SHUFFLE" => (ArrayShuffle, 1, 1),
        "ARRAY_SAMPLE" => (ArraySample, 2, 2),
        "UUID_EXTRACT_VERSION" => (UuidVersion, 1, 1),
        "UUID_EXTRACT_TIMESTAMP" => (UuidTimestamp, 1, 1),
        "UUIDV7" => (UuidV7, 0, 1),
        "UUIDV4" | "GEN_RANDOM_UUID" => (UuidV4, 0, 0),
        "JUSTIFY_DAYS" => (JustifyDays, 1, 1),
        "JUSTIFY_HOURS" => (JustifyHours, 1, 1),
        "JUSTIFY_INTERVAL" => (JustifyInterval, 1, 1),
        "HASHTEXT" | "HASHNAME" | "HASHBPCHAR" => (HashText, 1, 1),
        "HASHTEXTEXTENDED" | "HASHNAMEEXTENDED" | "HASHBPCHAREXTENDED" => (HashTextExtended, 2, 2),
        "HASHINT2" | "HASHINT4" | "HASHINT8" | "HASHCHAR" => (HashInt, 1, 1),
        "HASHINT2EXTENDED" | "HASHINT4EXTENDED" | "HASHINT8EXTENDED" | "HASHCHAREXTENDED" => (HashIntExtended, 2, 2),
        "SATISFIES_HASH_PARTITION" => (SatisfiesHashPartition, 3, usize::MAX),
        "TO_REGCOLLATION" => (ToRegcollation, 1, 1),
        "__ENUM_RANGE" => (EnumRange, 2, 3),
        "__ENUM_FIRST" => (EnumFirst, 2, 2),
        "__ENUM_LAST" => (EnumLast, 2, 2),
        "__POPULATE" => (Populate, 3, 3),
        "LO_CREATE" => (LoCreate, 1, 1),
        "LO_CREAT" => (LoCreat, 1, 1),
        "LO_FROM_BYTEA" => (LoFromBytea, 2, 2),
        "BYTEA_IN" => (ByteaIn, 1, 1),
        "JSONPATH_IN" => (JsonpathIn, 1, 1),
        "LO_GET" => (LoGet, 1, 3),
        "LO_PUT" => (LoPut, 3, 3),
        "LO_UNLINK" => (LoUnlink, 1, 1),
        "LO_IMPORT" => (LoImport, 1, 2),
        "LO_EXPORT" => (LoExport, 2, 2),
        "LO_OPEN" => (LoOpen, 2, 2),
        "LO_CLOSE" => (LoClose, 1, 1),
        "LOREAD" => (LoRead, 2, 2),
        "LOWRITE" => (LoWrite, 2, 2),
        "LO_LSEEK" | "LO_LSEEK64" => (LoLseek, 3, 3),
        "LO_TELL" | "LO_TELL64" => (LoTell, 1, 1),
        "LO_TRUNCATE" | "LO_TRUNCATE64" => (LoTruncate, 2, 2),
        "XPATH" => (Xpath, 2, 3),
        "__XML_VALUE" => (XmlValue, 2, 2),
        "CONVERT_FROM" => (ConvertFrom, 2, 2),
        "CONVERT_TO" => (ConvertTo, 2, 2),
        "__XML_NOTNULL" => (XmlNotNull, 2, 2),
        "__XMLELEMENT" => (XmlElement, 2, 255),
        "__XMLFOREST" => (XmlForest, 0, 255),
        "__XMLPI" => (XmlPi, 1, 2),
        "__XMLPARSE" => (XmlParse, 2, 2),
        "__XMLSERIALIZE" => (XmlSerialize, 2, 2),
        "__XMLROOT" => (XmlRoot, 3, 3),
        "XMLCONCAT" => (XmlConcat, 1, 255),
        "XMLCOMMENT" => (XmlComment, 1, 1),
        "XML_IS_WELL_FORMED" => (XmlWellFormed, 1, 1),
        "XML_IS_WELL_FORMED_DOCUMENT" => (XmlWellFormedDocument, 1, 1),
        "XML_IS_WELL_FORMED_CONTENT" => (XmlWellFormedContent, 1, 1),
        "XPATH_EXISTS" => (XpathExists, 2, 3),
        "PG_RESTORE_RELATION_STATS" | "PG_RESTORE_ATTRIBUTE_STATS" => (RestoreStats, 0, usize::MAX),
        "PG_CLEAR_RELATION_STATS" | "PG_CLEAR_ATTRIBUTE_STATS" => (ClearStats, 2, 3),
        "PG_COLUMN_COMPRESSION" => (Null, 1, 1),
        _ => return None,
    })
}

impl MoreFn {
    pub fn handles_null(self) -> bool {
        matches!(
            self,
            MoreFn::NumNulls
                | MoreFn::NumNonnulls
                | MoreFn::QuoteNullable
                | MoreFn::JsonBuildObject
                | MoreFn::JsonBuildArray
                | MoreFn::SatisfiesHashPartition
                | MoreFn::EnumRange
                | MoreFn::EnumFirst
                | MoreFn::EnumLast
                | MoreFn::Populate
                | MoreFn::RestoreStats
                | MoreFn::XmlNotNull
                | MoreFn::XmlElement
                | MoreFn::XmlForest
                | MoreFn::XmlPi
                | MoreFn::XmlRoot
                | MoreFn::XmlConcat
        )
    }
}

fn text(v: &Value) -> Cow<'_, str> {
    match v {
        Value::Text(s) => Cow::Borrowed(&**s),
        Value::Json(s) => Cow::Borrowed(&**s),
        other => Cow::Owned(other.to_string()),
    }
}

fn int(v: &Value) -> Result<i64> {
    match v.coerce_to(DataType::Integer)? {
        Value::Int(i) => Ok(i),
        _ => Err(Error::type_err("a whole number was expected")),
    }
}

fn items(v: &Value) -> Result<Vec<Value>> {
    match v {
        Value::Array(a) => Ok(a.as_ref().clone()),
        Value::Text(s) => match Value::parse_array(s, crate::value::ArrayOf::Any) {
            Some(Value::Array(a)) => Ok(a.as_ref().clone()),
            _ => Err(Error::type_err(format!("malformed array literal: \"{s}\""))),
        },
        other => Err(Error::type_err(format!("'{other}' is not an array"))),
    }
}

fn array(v: Vec<Value>) -> Value {
    Value::Array(Arc::new(v))
}

const DAY: i64 = 86_400;

fn justify_hours(mut i: crate::interval::Interval) -> crate::interval::Interval {
    i.days += i.secs / DAY;
    i.secs %= DAY;
    if i.days > 0 && i.secs < 0 {
        i.days -= 1;
        i.secs += DAY;
    } else if i.days < 0 && i.secs > 0 {
        i.days += 1;
        i.secs -= DAY;
    }
    i
}

fn justify_days(mut i: crate::interval::Interval) -> crate::interval::Interval {
    i.months += i.days / 30;
    i.days %= 30;
    if i.months > 0 && i.days < 0 {
        i.months -= 1;
        i.days += 30;
    } else if i.months < 0 && i.days > 0 {
        i.months += 1;
        i.days -= 30;
    }
    i
}

fn interval_of(v: &Value) -> Result<crate::interval::Interval> {
    match v {
        Value::Interval(i) => Ok(*i),
        Value::Text(t) => crate::interval::Interval::parse(t, None).map_err(Error::type_err),
        other => Err(Error::type_err(format!("'{other}' is not an interval"))),
    }
}

fn uuid_text(bytes: [u8; 16]) -> String {
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn random_bytes() -> [u8; 16] {
    let mut out = [0u8; 16];
    for b in &mut out {
        *b = (super::extra::random() * 256.0) as u8;
    }
    out
}

/// The labels of the enum type with this name, or of the one that has `probe` as a label.
fn enum_labels(type_name: &Value, probe: &[Cow<'_, Value>]) -> Result<Vec<String>> {
    if !type_name.is_null() {
        if let Some(t) = crate::sql::usertypes::lookup(&text(type_name)).filter(|t| !t.labels.is_empty()) {
            return Ok(t.labels);
        }
    }
    let wanted: Vec<String> = probe.iter().filter(|p| !p.is_null()).map(|p| text(p).into_owned()).collect();
    let mut found: Vec<Vec<String>> = crate::sql::usertypes::snapshot().into_values().map(|t| t.labels).filter(|l| !l.is_empty() && wanted.iter().all(|w| l.contains(w))).collect();
    match (found.pop(), wanted.is_empty()) {
        (Some(l), false) if found.is_empty() => Ok(l),
        _ => Err(Error::runtime("could not determine polymorphic type because input has type unknown")),
    }
}

thread_local! {
    static LARGE_OBJECTS: std::cell::RefCell<std::collections::BTreeMap<i64, Vec<u8>>> = const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
}

thread_local! {
    /// The large object descriptors that are open: the object, the position, and whether it may be written.
    static LO_DESCRIPTORS: std::cell::RefCell<Vec<Option<(i64, usize, bool)>>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn descriptor(fd: i64) -> Result<(i64, usize, bool)> {
    LO_DESCRIPTORS.with(|d| usize::try_from(fd).ok().and_then(|i| d.borrow().get(i).copied().flatten())).ok_or_else(|| Error::runtime(format!("invalid large-object descriptor: {fd}")))
}

fn set_position(fd: i64, at: usize) {
    LO_DESCRIPTORS.with(|d| {
        if let Some(Some(e)) = d.borrow_mut().get_mut(fd as usize) {
            e.1 = at;
        }
    });
}

fn lo_data(id: i64) -> Result<Vec<u8>> {
    LARGE_OBJECTS.with(|m| m.borrow().get(&id).cloned()).ok_or_else(|| Error::runtime(format!("large object {id} does not exist")))
}

/// A value as the text of an XML attribute or element.
fn xml_text(v: &Value) -> String {
    match v {
        Value::Bool(b) => b.to_string(),
        other => text(other).into_owned(),
    }
}

/// A value as XML element content: markup that is well formed (a value of type xml) stays as it is, any other text is escaped.
fn xml_content(v: &Value) -> String {
    let s = xml_text(v);
    if s.trim_start().starts_with('<') && super::xmlfuncs::well_formed(&s, true) {
        s
    } else {
        super::xmlfuncs::escape_content(&s)
    }
}

/// The bytes a bytea literal stands for: `\x` and hex digits, or the escape form (`\\` is a backslash, `\ooo` is a byte).
fn parse_bytea(s: &str) -> Result<Vec<u8>> {
    let bad = || Error::runtime("invalid input syntax for type bytea");
    if let Some(hex) = s.strip_prefix("\\x") {
        let digits: Vec<u8> = hex.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        if digits.len() % 2 != 0 {
            return Err(Error::runtime("invalid hexadecimal data: odd number of digits"));
        }
        return digits
            .chunks(2)
            .map(|p| {
                let h = |c: u8| (c as char).to_digit(16).ok_or_else(|| Error::runtime(format!("invalid hexadecimal digit: \"{}\"", c as char)));
                Ok((h(p[0])? * 16 + h(p[1])?) as u8)
            })
            .collect();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
        } else if b.get(i + 1) == Some(&b'\\') {
            out.push(b'\\');
            i += 2;
        } else if i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = b[i + 1..i + 4].iter().fold(0u32, |a, c| a * 8 + (*c - b'0') as u32);
            if v > 255 {
                return Err(bad());
            }
            out.push(v as u8);
            i += 4;
        } else {
            return Err(bad());
        }
    }
    Ok(out)
}

fn raw(v: &Value) -> Vec<u8> {
    match v {
        Value::Bytes(b) => b.as_ref().clone(),
        other => text(other).as_bytes().to_vec(),
    }
}

/// A new large object number: the one asked for, or the next free one.
/// The large objects, as `(oid, contents)`.
pub(crate) fn large_objects() -> Vec<(i64, Vec<u8>)> {
    LARGE_OBJECTS.with(|m| m.borrow().iter().map(|(k, v)| (*k, v.clone())).collect())
}

fn lo_new(wanted: i64, data: Vec<u8>) -> Result<i64> {
    LARGE_OBJECTS.with(|m| {
        let mut m = m.borrow_mut();
        let id = if wanted != 0 { wanted } else { m.keys().next_back().map_or(16384, |k| k + 1) };
        if m.contains_key(&id) {
            return Err(Error::runtime(format!("large object {id} already exists")));
        }
        m.insert(id, data);
        Ok(id)
    })
}

/// The fields of a composite type filled from a JSON object, over the fields of `base` (record text) where the object has no key.
pub(crate) fn populate_fields(type_name: &str, base: Option<&str>, doc: &J) -> Result<Vec<Option<String>>> {
    let fields = super::composite::fields_of(type_name).ok_or_else(|| Error::catalog(format!("type \"{type_name}\" does not exist")))?;
    let J::Object(map) = doc else { return Err(Error::runtime("cannot call populate_record on a non-object")) };
    let old = base.and_then(super::composite::split).unwrap_or_default();
    Ok(fields
        .iter()
        .enumerate()
        .map(|(i, (name, _))| match map.get(name) {
            Some(J::Null) => None,
            Some(J::String(s)) => Some(s.clone()),
            Some(j) => Some(json::to_text(j)),
            None => old.get(i).cloned().flatten(),
        })
        .collect())
}

/// Regular-expression flags the way PostgreSQL reads them: case-sensitive unless `i` is given.
fn flags_of(vals: &[Cow<'_, Value>], at: usize) -> String {
    let f = vals.get(at).map(|f| text(f).into_owned()).unwrap_or_default();
    if f.contains('i') { f } else { format!("c{f}") }
}

fn quote_literal(s: &str) -> String {
    let body = s.replace('\'', "''");
    if s.contains('\\') {
        format!("E'{}'", body.replace('\\', "\\\\"))
    } else {
        format!("'{body}'")
    }
}

fn radix(v: &Value, base: u32) -> Result<String> {
    let i = int(v)?;
    // an int4 is shown as 32 bits when it is negative and fits in them, a bigint as 64
    let u = if i < 0 && i >= i64::from(i32::MIN) { u64::from(i as i32 as u32) } else { i as u64 };
    Ok(match base {
        16 => format!("{u:x}"),
        8 => format!("{u:o}"),
        _ => format!("{u:b}"),
    })
}

fn lgamma(x: f64) -> f64 {
    // Lanczos approximation (g = 7), with the reflection formula below 0.5
    const G: [f64; 9] = [
        0.999_999_999_999_809_9, 676.520_368_121_885_1, -1_259.139_216_722_402_8, 771.323_428_777_653_1,
        -176.615_029_162_140_6, 12.507_343_278_686_905, -0.138_571_095_265_720_12, 9.984_369_578_019_572e-6, 1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin().abs()).ln() - lgamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = G[0];
    let t = x + 7.5;
    for (i, g) in G.iter().enumerate().skip(1) {
        a += g / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

fn size_pretty(n: i64) -> String {
    let units = ["bytes", "kB", "MB", "GB", "TB", "PB"];
    let (mut v, mut i) = (n, 0);
    while v.abs() >= 10240 && i + 1 < units.len() {
        v = if v < 0 { -((-v + 512) / 1024) } else { (v + 512) / 1024 };
        i += 1;
    }
    format!("{v} {}", units[i])
}

fn size_bytes(s: &str) -> Result<i64> {
    let t = s.trim();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+')).unwrap_or(t.len());
    let (num, unit) = (&t[..split], t[split..].trim());
    let mult: f64 = match unit.to_ascii_lowercase().as_str() {
        "" | "b" | "bytes" => 1.0,
        "kb" => 1024.0,
        "mb" => 1024.0 * 1024.0,
        "gb" => 1024.0_f64.powi(3),
        "tb" => 1024.0_f64.powi(4),
        "pb" => 1024.0_f64.powi(5),
        _ => return Err(Error::runtime(format!("invalid size: \"{s}\""))),
    };
    let n: f64 = num.parse().map_err(|_| Error::runtime(format!("invalid size: \"{s}\"")))?;
    Ok((n * mult).round() as i64)
}

fn unistr(s: &str) -> Result<String> {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let bad = || Error::runtime("invalid Unicode escape");
        match it.peek().copied() {
            Some('\\') => {
                it.next();
                out.push('\\');
            }
            Some('+') => {
                it.next();
                let h: String = (0..6).filter_map(|_| it.next()).collect();
                out.push(u32::from_str_radix(&h, 16).ok().and_then(char::from_u32).ok_or_else(bad)?);
            }
            Some('u') => {
                it.next();
                let h: String = (0..4).filter_map(|_| it.next()).collect();
                out.push(u32::from_str_radix(&h, 16).ok().and_then(char::from_u32).ok_or_else(bad)?);
            }
            Some(d) if d.is_ascii_hexdigit() => {
                let h: String = (0..4).filter_map(|_| it.next()).collect();
                out.push(u32::from_str_radix(&h, 16).ok().and_then(char::from_u32).ok_or_else(bad)?);
            }
            _ => return Err(bad()),
        }
    }
    Ok(out)
}

/// Follows `keys` (object keys, or array positions written as numbers) down a document.
fn walk<'a>(mut cur: &'a J, keys: &[String]) -> Option<&'a J> {
    for k in keys {
        cur = match cur {
            J::Object(m) => m.get(k)?,
            J::Array(a) => {
                let i: i64 = k.parse().ok()?;
                let i = if i < 0 { a.len() as i64 + i } else { i };
                a.get(usize::try_from(i).ok()?)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

fn has_key(doc: &J, key: &str) -> bool {
    match doc {
        J::Object(m) => m.contains_key(key),
        J::Array(a) => a.iter().any(|x| matches!(x, J::String(s) if s == key)),
        J::String(s) => s == key,
        _ => false,
    }
}

fn pretty(j: &J) -> String {
    use serde::Serialize;
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    j.serialize(&mut ser).expect("a JSON value always serialises");
    String::from_utf8(buf).unwrap_or_default()
}

pub fn call(f: MoreFn, vals: &[Cow<'_, Value>]) -> Result<Value> {
    use MoreFn::*;
    let v = |i: usize| -> &Value { &vals[i] };
    Ok(match f {
        Strpos => {
            let (s, sub) = (text(v(0)), text(v(1)));
            Value::Int(s.find(&*sub).map_or(0, |b| s[..b].chars().count() as i64 + 1))
        }
        ToHex => Value::text(&radix(v(0), 16)?),
        ToBin => Value::text(&radix(v(0), 2)?),
        ToOct => Value::text(&radix(v(0), 8)?),
        Btrim => {
            let s = text(v(0));
            let set: Vec<char> = vals.get(1).map_or(vec![' '], |c| text(c).chars().collect());
            Value::text(s.trim_matches(|c| set.contains(&c)))
        }
        Chr => {
            let n = int(v(0))?;
            if n == 0 {
                return Err(Error::runtime("null character not permitted"));
            }
            match u32::try_from(n).ok().and_then(char::from_u32) {
                Some(c) => Value::text(&c.to_string()),
                None => return Err(Error::runtime(format!("requested character too large for encoding: {n}"))),
            }
        }
        StartsWith => Value::Bool(text(v(0)).starts_with(&*text(v(1)))),
        QuoteNullable => {
            if v(0).is_null() { Value::text("NULL") } else { Value::text(&quote_literal(&text(v(0)))) }
        }
        QuoteLiteral => Value::text(&quote_literal(&text(v(0)))),
        Normalize => Value::text(&text(v(0))),
        Casefold => Value::text(&text(v(0)).to_lowercase()),
        Unistr => Value::text(&unistr(&text(v(0)))?),
        NumNulls => Value::Int(vals.iter().filter(|x| x.is_null()).count() as i64),
        NumNonnulls => Value::Int(vals.iter().filter(|x| !x.is_null()).count() as i64),
        RegexpMatch => {
            let (s, p) = (text(v(0)).into_owned(), text(v(1)).into_owned());
            let re = super::extra::pattern(&p, &flags_of(vals, 2))?;
            match re.group(&s, 0) {
                None => Value::Null,
                Some(whole) => {
                    let n = re.group_count();
                    if n <= 1 {
                        array(vec![Value::text(&whole)])
                    } else {
                        array((1..n).map(|i| re.group(&s, i).map_or(Value::Null, |g| Value::text(&g))).collect())
                    }
                }
            }
        }
        RegexpSplitToArray => {
            let (s, p) = (text(v(0)).into_owned(), text(v(1)).into_owned());
            let re = super::extra::pattern(&p, &flags_of(vals, 2))?;
            let mut out = Vec::new();
            let mut last = 0;
            for (a, b) in re.find_all(&s) {
                if b == a {
                    continue;
                }
                out.push(Value::text(&s[last..a]));
                last = b;
            }
            out.push(Value::text(&s[last..]));
            array(out)
        }
        Factorial => {
            let n = int(v(0))?;
            if n < 0 {
                return Err(Error::runtime("factorial of a negative number is undefined"));
            }
            if n > 32177 {
                return Err(Error::runtime("value overflows numeric format"));
            }
            let mut r = BigInt::from(1);
            for i in 2..=n {
                r *= i;
            }
            Value::Big(r)
        }
        Lgamma => {
            let x = v(0).to_f64().ok_or_else(|| Error::type_err("a number was expected"))?;
            Value::from_f64(lgamma(x)).unwrap_or(Value::Null)
        }
        SizePretty => Value::text(&size_pretty(int(v(0))?)),
        SizeBytes => Value::Int(size_bytes(&text(v(0)))?),
        JsonBuildObject => {
            if vals.len() % 2 != 0 {
                return Err(Error::runtime("argument list must have even number of elements"));
            }
            let mut m = serde_json::Map::new();
            for pair in vals.chunks(2) {
                if pair[0].is_null() {
                    return Err(Error::runtime("null value not allowed for object key"));
                }
                m.insert(text(&pair[0]).into_owned(), json::from_value(&pair[1]));
            }
            json::json_value(&J::Object(m))
        }
        JsonBuildArray => json::json_value(&J::Array(vals.iter().map(|x| json::from_value(x)).collect())),
        JsonExtractPath | JsonExtractPathText => {
            let doc = json::document(v(0))?;
            let mut keys = Vec::new();
            for k in &vals[1..] {
                if k.is_null() {
                    return Ok(Value::Null);
                }
                match k.as_ref() {
                    Value::Array(a) => keys.extend(a.iter().map(|x| text(x).into_owned())),
                    other => keys.push(text(other).into_owned()),
                }
            }
            match walk(&doc, &keys) {
                None | Some(J::Null) if f == JsonExtractPathText => Value::Null,
                None => Value::Null,
                Some(J::String(s)) if f == JsonExtractPathText => Value::text(s),
                Some(j) if f == JsonExtractPathText => Value::text(&json::to_text(j)),
                Some(j) => json::json_value(j),
            }
        }
        JsonbExists => Value::Bool(has_key(&json::document(v(0))?, &text(v(1)))),
        JsonbExistsAny | JsonbExistsAll => {
            let doc = json::document(v(0))?;
            let keys = items(v(1))?;
            let hit = |k: &Value| !k.is_null() && has_key(&doc, &text(k));
            Value::Bool(if f == JsonbExistsAny { keys.iter().any(hit) } else { keys.iter().all(hit) })
        }
        JsonbPretty => Value::text(&pretty(&json::document(v(0))?)),
        JsonbContains => Value::Bool(json::contains(&json::document(v(0))?, &json::document(v(1))?)),
        JsonbContained => Value::Bool(json::contains(&json::document(v(1))?, &json::document(v(0))?)),
        JsonbDelete => {
            let mut doc = json::document(v(0))?;
            match (&mut doc, v(1)) {
                (J::Object(m), k) => {
                    m.remove(&*text(k));
                }
                (J::Array(a), Value::Int(i)) => {
                    let i = if *i < 0 { a.len() as i64 + i } else { *i };
                    if let Ok(i) = usize::try_from(i) {
                        if i < a.len() {
                            a.remove(i);
                        }
                    }
                }
                (J::Array(a), k) => {
                    let k = text(k).into_owned();
                    a.retain(|x| !matches!(x, J::String(s) if *s == k));
                }
                _ => return Err(Error::runtime("cannot delete from scalar")),
            }
            json::json_value(&doc)
        }
        JsonbDeletePath => {
            let mut doc = json::document(v(0))?;
            let steps: Vec<json::Step> = items(v(1))?
                .iter()
                .map(|k| {
                    let t = text(k).into_owned();
                    t.parse::<usize>().map_or(json::Step::Key(t), json::Step::Index)
                })
                .collect();
            json::remove(&mut doc, &steps);
            json::json_value(&doc)
        }
        JsonScalar => json::json_value(&json::from_value(v(0))),
        JsonSerialize => Value::text(&json::to_text(&json::document(v(0))?)),
        JsonExists => {
            let doc = json::document(v(0))?;
            Value::Bool(!crate::jsonpath::query(&doc, &crate::jsonpath::parse(&text(v(1)))?, None, true)?.is_empty())
        }
        ArrayToJson => json::json_value(&json::from_value(&array(items(v(0))?))),
        JsonbObject => {
            let mut m = serde_json::Map::new();
            let a = items(v(0))?;
            if let Some(b) = vals.get(1) {
                let b = items(b)?;
                if a.len() != b.len() {
                    return Err(Error::runtime("mismatched array dimensions"));
                }
                for (k, x) in a.iter().zip(&b) {
                    m.insert(text(k).into_owned(), if x.is_null() { J::Null } else { J::String(text(x).into_owned()) });
                }
            } else {
                if a.len() % 2 != 0 {
                    return Err(Error::runtime("array must have even number of elements"));
                }
                for p in a.chunks(2) {
                    m.insert(text(&p[0]).into_owned(), if p[1].is_null() { J::Null } else { J::String(text(&p[1]).into_owned()) });
                }
            }
            json::json_value(&J::Object(m))
        }
        ArrayReverse => {
            let mut a = items(v(0))?;
            a.reverse();
            array(a)
        }
        ArraySort => {
            let mut a = items(v(0))?;
            let desc = vals.get(1).is_some_and(|x| matches!(x.as_ref(), Value::Bool(true)));
            let nulls_first = vals.get(2).map_or(desc, |x| matches!(x.as_ref(), Value::Bool(true)));
            a.sort_by(|x, y| match (x.is_null(), y.is_null()) {
                (true, true) => Ordering::Equal,
                (true, false) => if nulls_first { Ordering::Less } else { Ordering::Greater },
                (false, true) => if nulls_first { Ordering::Greater } else { Ordering::Less },
                _ => if desc { y.cmp(x) } else { x.cmp(y) },
            });
            array(a)
        }
        ArrayShuffle | ArraySample => {
            let mut a = items(v(0))?;
            for i in (1..a.len()).rev() {
                a.swap(i, (super::extra::random() * (i + 1) as f64) as usize % (i + 1));
            }
            if f == ArraySample {
                let n = int(v(1))?;
                if n < 0 || n as usize > a.len() {
                    return Err(Error::runtime("sample size must be between 0 and the number of array elements"));
                }
                a.truncate(n as usize);
            }
            array(a)
        }
        UuidVersion => {
            let hex: Vec<char> = text(v(0)).chars().filter(|c| *c != '-').collect();
            match (hex.get(12).and_then(|c| c.to_digit(16)), hex.get(16).and_then(|c| c.to_digit(16))) {
                (Some(ver), Some(var)) if (8..=11).contains(&var) => Value::Int(i64::from(ver)),
                _ => Value::Null,
            }
        }
        UuidTimestamp => {
            let b = match super::bytefuncs::uuid_bytes(&text(v(0))) {
                Some(b) => b,
                None => return Ok(Value::Null),
            };
            let var = b[8] >> 6;
            match (b[6] >> 4, var) {
                (7, 2) => {
                    let ms = b[..6].iter().fold(0u64, |a, x| (a << 8) | u64::from(*x));
                    Value::TimestampTz((ms / 1000) as i64)
                }
                (1, 2) => {
                    let low = u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
                    let mid = u64::from(u16::from_be_bytes([b[4], b[5]]));
                    let hi = u64::from(u16::from_be_bytes([b[6], b[7]]) & 0x0fff);
                    let ticks = (hi << 48) | (mid << 32) | low;
                    Value::TimestampTz((ticks / 10_000_000) as i64 - 12_219_292_800)
                }
                _ => Value::Null,
            }
        }
        UuidV7 | UuidV4 => {
            let mut b = random_bytes();
            if f == UuidV7 {
                let shift = match vals.first() {
                    Some(i) if !i.is_null() => interval_of(i)?.total_secs() as i64,
                    _ => 0,
                };
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64) + shift * 1000;
                let ms = now.max(0) as u64;
                b[..6].copy_from_slice(&ms.to_be_bytes()[2..]);
                b[6] = (b[6] & 0x0f) | 0x70;
            } else {
                b[6] = (b[6] & 0x0f) | 0x40;
            }
            b[8] = (b[8] & 0x3f) | 0x80;
            Value::text(&uuid_text(b))
        }
        JustifyDays => Value::Interval(justify_days(interval_of(v(0))?)),
        JustifyHours => Value::Interval(justify_hours(interval_of(v(0))?)),
        JustifyInterval => {
            let mut i = justify_days(justify_hours(interval_of(v(0))?));
            // a month and a day or time of opposite signs are brought to the same sign
            if i.months > 0 && (i.days < 0 || i.secs < 0) {
                i.months -= 1;
                i.days += 30;
            } else if i.months < 0 && (i.days > 0 || i.secs > 0) {
                i.months += 1;
                i.days -= 30;
            }
            Value::Interval(justify_hours(i))
        }
        HashText => Value::Int(i64::from(super::pghash::hash_bytes(text(v(0)).trim_end_matches(' ').as_bytes()) as i32)),
        HashTextExtended => Value::Int(super::pghash::hash_bytes_extended(text(v(0)).as_bytes(), int(v(1))? as u64) as i64),
        HashInt => Value::Int(i64::from(super::pghash::hash_i64(int(v(0))?) as i32)),
        HashIntExtended => Value::Int(super::pghash::hash_i64_extended(int(v(0))?, int(v(1))? as u64) as i64),
        SatisfiesHashPartition => {
            let (modulus, remainder) = (int(v(1))?, int(v(2))?);
            if modulus <= 0 {
                return Err(Error::runtime("modulus for hash partition must be an integer value greater than zero"));
            }
            if remainder < 0 || remainder >= modulus {
                return Err(Error::runtime("remainder for hash partition must be an integer value greater than or equal to zero and less than modulus"));
            }
            let keys: Vec<Value> = vals[3..].iter().map(|x| x.as_ref().clone()).collect();
            Value::Bool(super::pghash::partition_hash(&keys) % modulus as u64 == remainder as u64)
        }
        ToRegcollation => {
            let name = text(v(0));
            let bare = name.trim().trim_start_matches("pg_catalog.").trim_matches('"');
            if ["default", "C", "POSIX", "ucs_basic", "und-x-icu", "en-x-icu", "en_US", "en_US.utf8"].iter().any(|c| c.eq_ignore_ascii_case(bare)) {
                Value::text(bare)
            } else {
                Value::Null
            }
        }
        EnumRange => {
            let labels = enum_labels(v(0), &vals[1..])?;
            let at = |bound: Option<&Cow<'_, Value>>, default: usize| -> Result<usize> {
                match bound {
                    Some(b) if !b.is_null() => labels
                        .iter()
                        .position(|l| *l == *text(b))
                        .ok_or_else(|| Error::runtime(format!("invalid input value for enum: \"{}\"", text(b)))),
                    _ => Ok(default),
                }
            };
            let (lo, hi) = if vals.len() == 3 { (at(vals.get(1), 0)?, at(vals.get(2), labels.len().saturating_sub(1))?) } else { (0, labels.len().saturating_sub(1)) };
            array(if lo <= hi && !labels.is_empty() { labels[lo..=hi].iter().map(|l| Value::text(l)).collect() } else { Vec::new() })
        }
        EnumFirst | EnumLast => {
            let labels = enum_labels(v(0), &vals[1..])?;
            let l = if f == EnumFirst { labels.first() } else { labels.last() };
            l.map_or(Value::Null, |l| Value::text(l))
        }
        Populate => {
            if v(2).is_null() {
                return Ok(v(1).clone());
            }
            let doc = json::document(v(2))?;
            let base = (!v(1).is_null()).then(|| text(v(1)).into_owned());
            let fields = populate_fields(&text(v(0)), base.as_deref(), &doc)?;
            Value::text(&super::composite::cast_text(&text(v(0)), &super::composite::join(&fields))?)
        }
        LoCreate => Value::Int(lo_new(int(v(0))?, Vec::new())?),
        LoCreat => Value::Int(lo_new(0, Vec::new())?),
        LoFromBytea => Value::Int(lo_new(int(v(0))?, raw(v(1)))?),
        JsonpathIn => match v(0) {
            Value::Null => Value::Null,
            other => Value::text(&crate::jsonpath::parse(&text(other))?.to_text()),
        },
        ByteaIn => match v(0) {
            Value::Null => Value::Null,
            Value::Bytes(_) => v(0).clone(),
            other => Value::Bytes(std::sync::Arc::new(parse_bytea(&text(other))?)),
        },
        LoGet => {
            let id = int(v(0))?;
            let data = LARGE_OBJECTS.with(|m| m.borrow().get(&id).cloned()).ok_or_else(|| Error::runtime(format!("large object {id} does not exist")))?;
            let from = vals.get(1).map_or(Ok(0), |x| int(x))?;
            if from < 0 {
                return Err(Error::runtime("requested offset out of range"));
            }
            let from = (from as usize).min(data.len());
            let to = match vals.get(2) {
                Some(n) => {
                    let n = int(n)?;
                    if n < 0 {
                        return Err(Error::runtime("requested length cannot be negative"));
                    }
                    (from + n as usize).min(data.len())
                }
                None => data.len(),
            };
            Value::bytes(data[from..to].to_vec())
        }
        LoPut => {
            let id = int(v(0))?;
            let (at, bytes) = (int(v(1))?, raw(v(2)));
            if at < 0 {
                return Err(Error::runtime("requested offset out of range"));
            }
            LARGE_OBJECTS.with(|m| -> Result<()> {
                let mut m = m.borrow_mut();
                let data = m.get_mut(&id).ok_or_else(|| Error::runtime(format!("large object {id} does not exist")))?;
                let end = at as usize + bytes.len();
                if data.len() < end {
                    data.resize(end, 0);
                }
                data[at as usize..end].copy_from_slice(&bytes);
                Ok(())
            })?;
            Value::Null
        }
        XmlElement => {
            let name = super::xmlfuncs::element_name(&text(v(0)));
            let nattr = int(v(1))? as usize;
            let mut attrs = String::new();
            for i in 0..nattr {
                let (an, av) = (text(v(2 + 2 * i)), v(3 + 2 * i));
                if !av.is_null() {
                    attrs.push_str(&format!(" {}=\"{}\"", super::xmlfuncs::element_name(&an), super::xmlfuncs::escape_attribute(&xml_text(av))));
                }
            }
            let mut body = String::new();
            let mut any = false;
            for c in &vals[2 + 2 * nattr..] {
                if !c.is_null() {
                    any = true;
                    body.push_str(&xml_content(c));
                }
            }
            Value::text(&if any { format!("<{name}{attrs}>{body}</{name}>") } else { format!("<{name}{attrs}/>") })
        }
        XmlForest => {
            let mut out = String::new();
            for pair in vals.chunks(2) {
                if !pair[1].is_null() {
                    let n = super::xmlfuncs::element_name(&text(&pair[0]));
                    out.push_str(&format!("<{n}>{}</{n}>", xml_content(&pair[1])));
                }
            }
            if out.is_empty() { Value::Null } else { Value::text(&out) }
        }
        XmlPi => {
            let name = text(v(0)).into_owned();
            if name.eq_ignore_ascii_case("xml") {
                return Err(Error::runtime("invalid XML processing instruction"));
            }
            match vals.get(1).filter(|c| !c.is_null()) {
                None => Value::text(&format!("<?{name}?>")),
                Some(c) => {
                    let c = text(c).into_owned();
                    if c.contains("?>") {
                        return Err(Error::runtime("invalid XML processing instruction"));
                    }
                    Value::text(&format!("<?{name} {}?>", c.trim_start()))
                }
            }
        }
        XmlParse => {
            let content = text(v(0)).eq_ignore_ascii_case("content");
            let s = text(v(1)).into_owned();
            if !super::xmlfuncs::well_formed(&s, content) {
                return Err(Error::runtime(if content { "invalid XML content" } else { "invalid XML document" }));
            }
            Value::text(&s)
        }
        XmlSerialize => {
            let s = text(v(0)).into_owned();
            Value::text(&if matches!(v(1), Value::Bool(true)) { super::xmlfuncs::indent(&s) } else { s })
        }
        XmlRoot => {
            if v(0).is_null() {
                return Ok(Value::Null);
            }
            let s = text(v(0)).into_owned();
            let rest = match s.trim_start().strip_prefix("<?xml") {
                Some(r) if r.starts_with(char::is_whitespace) || r.starts_with("?>") => r.find("?>").map_or(r, |i| &r[i + 2..]).to_string(),
                _ => s.trim_start().to_string(),
            };
            let version = if v(1).is_null() { "1.0".to_string() } else { text(v(1)).into_owned() };
            let mut decl = format!("<?xml version=\"{version}\"");
            if !v(2).is_null() {
                decl.push_str(&format!(" standalone=\"{}\"", text(v(2)).to_ascii_lowercase()));
            }
            Value::text(&format!("{decl}?>{rest}"))
        }
        XmlConcat => {
            let parts: Vec<String> = vals.iter().filter(|x| !x.is_null()).map(|x| text(x).into_owned()).collect();
            if parts.is_empty() { Value::Null } else { Value::text(&parts.concat()) }
        }
        XmlComment => {
            let s = text(v(0)).into_owned();
            if s.contains("--") || s.ends_with('-') {
                return Err(Error::runtime("invalid XML comment"));
            }
            Value::text(&format!("<!--{s}-->"))
        }
        XmlWellFormed => {
            let s = text(v(0));
            Value::Bool(super::xmlfuncs::well_formed(&s, false) || super::xmlfuncs::well_formed(&s, true))
        }
        XmlWellFormedDocument => Value::Bool(super::xmlfuncs::well_formed(&text(v(0)), false)),
        XmlWellFormedContent => Value::Bool(super::xmlfuncs::well_formed(&text(v(0)), true)),
        XmlNotNull => {
            if v(0).is_null() {
                return Err(Error::runtime(format!("null is not allowed in column \"{}\"", text(v(1)))));
            }
            v(0).clone()
        }
        ConvertFrom => Value::text(&String::from_utf8_lossy(&raw(v(0)))),
        ConvertTo => Value::bytes(raw(v(0))),
        XmlValue => {
            // a column of XMLTABLE: the path is relative to the row element, which is the root of the node's text
            let node = text(v(0)).into_owned();
            let path = text(v(1)).into_owned();
            let root = node.trim_start().strip_prefix('<').map(|r| r.split(|c: char| c.is_whitespace() || c == '>' || c == '/').next().unwrap_or("").to_string()).unwrap_or_default();
            let full = if path.starts_with('/') || root.is_empty() { path } else if path == "." { format!("/{root}") } else { format!("/{root}/{path}") };
            if super::xmlfuncs::xpath_nodes(&node, &full)?.is_empty() {
                Value::Null
            } else {
                super::xmlfuncs::extract_value(&node, &full)?.map_or(Value::Null, |t| Value::text(&t))
            }
        }
        Xpath | XpathExists => {
            let nodes = super::xmlfuncs::xpath_nodes(&text(v(1)), &text(v(0)))?;
            if f == XpathExists {
                Value::Bool(!nodes.is_empty())
            } else {
                array(nodes.iter().map(|n| Value::text(n)).collect())
            }
        }
        RestoreStats => {
            if vals.len() % 2 != 0 {
                return Err(Error::runtime("variadic arguments must be name/value pairs"));
            }
            let mut names = Vec::new();
            for pair in vals.chunks(2) {
                if pair[0].is_null() {
                    return Err(Error::runtime("name at variadic position 1 is NULL"));
                }
                names.push(text(&pair[0]).to_ascii_lowercase());
            }
            if !names.iter().any(|n| n == "relname") {
                return Err(Error::runtime("\"relname\" must be specified"));
            }
            Value::Bool(true)
        }
        ClearStats => Value::Null,
        LoUnlink => {
            let id = int(v(0))?;
            if LARGE_OBJECTS.with(|m| m.borrow_mut().remove(&id)).is_none() {
                return Err(Error::runtime(format!("large object {id} does not exist")));
            }
            Value::Int(1)
        }
        LoImport => {
            let path = text(v(0)).into_owned();
            let data = std::fs::read(&path).map_err(|e| Error::runtime(format!("could not open server file \"{path}\": {e}")))?;
            Value::Int(lo_new(vals.get(1).map_or(Ok(0), |x| int(x))?, data)?)
        }
        LoExport => {
            let (id, path) = (int(v(0))?, text(v(1)).into_owned());
            std::fs::write(&path, lo_data(id)?).map_err(|e| Error::runtime(format!("could not create server file \"{path}\": {e}")))?;
            Value::Int(1)
        }
        LoOpen => {
            let (id, mode) = (int(v(0))?, int(v(1))?);
            lo_data(id)?;
            let fd = LO_DESCRIPTORS.with(|d| {
                let mut d = d.borrow_mut();
                let entry = Some((id, 0, mode & 0x20000 != 0));
                match d.iter().position(Option::is_none) {
                    Some(i) => {
                        d[i] = entry;
                        i
                    }
                    None => {
                        d.push(entry);
                        d.len() - 1
                    }
                }
            });
            Value::Int(fd as i64)
        }
        LoClose => {
            let fd = int(v(0))?;
            descriptor(fd)?;
            LO_DESCRIPTORS.with(|d| d.borrow_mut()[fd as usize] = None);
            Value::Int(0)
        }
        LoRead => {
            let (fd, n) = (int(v(0))?, int(v(1))?);
            let (id, at, _) = descriptor(fd)?;
            let data = lo_data(id)?;
            let from = at.min(data.len());
            let to = (from + n.max(0) as usize).min(data.len());
            set_position(fd, to);
            Value::bytes(data[from..to].to_vec())
        }
        LoWrite => {
            let fd = int(v(0))?;
            let (id, at, writable) = descriptor(fd)?;
            if !writable {
                return Err(Error::runtime("large object descriptor was not opened for writing"));
            }
            let bytes = raw(v(1));
            let end = at + bytes.len();
            LARGE_OBJECTS.with(|m| {
                if let Some(data) = m.borrow_mut().get_mut(&id) {
                    if data.len() < end {
                        data.resize(end, 0);
                    }
                    data[at..end].copy_from_slice(&bytes);
                }
            });
            set_position(fd, end);
            Value::Int(bytes.len() as i64)
        }
        LoLseek => {
            let (fd, offset, whence) = (int(v(0))?, int(v(1))?, int(v(2))?);
            let (id, at, _) = descriptor(fd)?;
            let base = match whence {
                0 => 0,
                1 => at as i64,
                2 => lo_data(id)?.len() as i64,
                _ => return Err(Error::runtime(format!("invalid whence setting: {whence}"))),
            };
            let to = base + offset;
            if to < 0 {
                return Err(Error::runtime(format!("invalid large object seek target: {to}")));
            }
            set_position(fd, to as usize);
            Value::Int(to)
        }
        LoTell => Value::Int(descriptor(int(v(0))?)?.1 as i64),
        LoTruncate => {
            let (fd, len) = (int(v(0))?, int(v(1))?);
            let (id, _, writable) = descriptor(fd)?;
            if !writable || len < 0 {
                return Err(Error::runtime("large object descriptor was not opened for writing"));
            }
            LARGE_OBJECTS.with(|m| {
                if let Some(data) = m.borrow_mut().get_mut(&id) {
                    data.resize(len as usize, 0);
                }
            });
            Value::Int(0)
        }
        Null => Value::Null,
    })
}
