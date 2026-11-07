//! The functions of Transact-SQL that differ from, or are missing in, the other dialects. They are found first when the
//! dialect is `tsql`, so `ISNULL(a, b)` is a two-argument function there (in ASQL it asks whether one value is Null) and
//! `DATEADD(day, 1, d)` takes SQL Server's names for the parts of a date.

use std::borrow::Cow;

use crate::datetime::{self, MS_PER_DAY};
use crate::error::{Error, Result};
use crate::value::{DataType, Value};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TsqlFn {
    IsNull,
    GetDate,
    GetUtcDate,
    SysDateTimeOffset,
    NewId,
    CharIndex,
    Stuff,
    Space,
    Str,
    Reverse,
    DateAdd,
    DateDiff,
    DatePart,
    DateName,
    EoMonth,
    DateFromParts,
    DateTimeFromParts,
    TimeFromParts,
    IsNumeric,
    IsDate,
    Choose,
    Char,
    Ascii,
    QuoteName,
    Square,
    Pi,
    Rand,
    Concat,
    DataLength,
    DbName,
    HostName,
    UserName,
    ErrorMessage,
    ErrorNumber,
    ErrorSeverity,
    ErrorState,
    ErrorLine,
    ErrorProcedure,
    FormatMessage,
    CanCast,
    Style,
    IdentityOf,
    ObjectId,
    ObjectName,
    ObjectIdOf,
    Format,
    Version,
}

pub fn lookup(name: &str) -> Option<(TsqlFn, usize, usize)> {
    use TsqlFn::*;
    if crate::dialect::current() != crate::dialect::Dialect::Tsql {
        return None;
    }
    Some(match name {
        "ISNULL" => (IsNull, 2, 2),
        "GETDATE" | "SYSDATETIME" | "CURRENT_TIMESTAMP" => (GetDate, 0, 0),
        "GETUTCDATE" | "SYSUTCDATETIME" => (GetUtcDate, 0, 0),
        "SYSDATETIMEOFFSET" => (SysDateTimeOffset, 0, 0),
        "NEWID" | "NEWSEQUENTIALID" => (NewId, 0, 0),
        "CHARINDEX" => (CharIndex, 2, 3),
        "STUFF" => (Stuff, 4, 4),
        "SPACE" => (Space, 1, 1),
        "STR" => (Str, 1, 3),
        "REVERSE" => (Reverse, 1, 1),
        "DATEADD" => (DateAdd, 3, 3),
        "DATEDIFF" | "DATEDIFF_BIG" => (DateDiff, 3, 3),
        "DATEPART" => (DatePart, 2, 2),
        "DATENAME" => (DateName, 2, 2),
        "EOMONTH" => (EoMonth, 1, 2),
        "DATEFROMPARTS" => (DateFromParts, 3, 3),
        "DATETIMEFROMPARTS" | "DATETIME2FROMPARTS" | "SMALLDATETIMEFROMPARTS" => (DateTimeFromParts, 5, 8),
        "TIMEFROMPARTS" => (TimeFromParts, 5, 5),
        "ISNUMERIC" => (IsNumeric, 1, 1),
        "ISDATE" => (IsDate, 1, 1),
        "CHOOSE" => (Choose, 2, usize::MAX),
        "CHAR" | "NCHAR" => (Char, 1, 1),
        "ASCII" | "UNICODE" => (Ascii, 1, 1),
        "QUOTENAME" => (QuoteName, 1, 2),
        "SQUARE" => (Square, 1, 1),
        "PI" => (Pi, 0, 0),
        "RAND" => (Rand, 0, 1),
        "CONCAT" => (Concat, 2, usize::MAX),
        "DATALENGTH" => (DataLength, 1, 1),
        "DB_NAME" => (DbName, 0, 1),
        "HOST_NAME" | "SUSER_SNAME" | "SUSER_NAME" | "ORIGINAL_LOGIN" => (HostName, 0, 1),
        "USER_NAME" | "SESSION_USER" | "SYSTEM_USER" => (UserName, 0, 1),
        "ERROR_MESSAGE" => (ErrorMessage, 0, 0),
        "ERROR_NUMBER" => (ErrorNumber, 0, 0),
        "ERROR_SEVERITY" => (ErrorSeverity, 0, 0),
        "ERROR_STATE" => (ErrorState, 0, 0),
        "ERROR_LINE" => (ErrorLine, 0, 0),
        "ERROR_PROCEDURE" => (ErrorProcedure, 0, 0),
        "__FORMATMESSAGE" => (FormatMessage, 1, usize::MAX),
        "__CAN_CAST" => (CanCast, 2, 2),
        "__TSQL_STYLE" => (Style, 2, 2),
        "SCOPE_IDENTITY" | "IDENT_CURRENT" => (IdentityOf, 0, 1),
        "OBJECT_ID" => (ObjectId, 1, 2),
        "__TSQL_OBJNAME" => (ObjectName, 1, 1),
        "__TSQL_OBJID" => (ObjectIdOf, 1, 1),
        "FORMAT" => (Format, 2, 3),
        "__TSQL_VERSION" => (Version, 0, 0),
        _ => return None,
    })
}

impl TsqlFn {
    /// Functions that see their NULL arguments rather than answering NULL.
    pub fn handles_null(self) -> bool {
        matches!(self, TsqlFn::IsNull | TsqlFn::Choose | TsqlFn::Concat | TsqlFn::CanCast | TsqlFn::FormatMessage | TsqlFn::ObjectId | TsqlFn::Style)
    }
}

fn text(v: &Value) -> Cow<'_, str> {
    match v {
        Value::Text(s) => Cow::Borrowed(&**s),
        other => Cow::Owned(other.to_string()),
    }
}

fn int(v: &Value) -> Result<i64> {
    match v.coerce_to(DataType::Integer)? {
        Value::Int(i) => Ok(i),
        _ => Err(Error::type_err("a whole number was expected")),
    }
}

/// The part of a date a name stands for.
fn unit_of(name: &str) -> Result<&'static str> {
    Ok(match name.trim().to_ascii_lowercase().as_str() {
        "year" | "yy" | "yyyy" => "year",
        "quarter" | "qq" | "q" => "quarter",
        "month" | "mm" | "m" => "month",
        "dayofyear" | "dy" | "y" => "dayofyear",
        "day" | "dd" | "d" => "day",
        "week" | "wk" | "ww" => "week",
        "weekday" | "dw" | "w" => "weekday",
        "hour" | "hh" => "hour",
        "minute" | "mi" | "n" => "minute",
        "second" | "ss" | "s" => "second",
        "millisecond" | "ms" => "millisecond",
        "microsecond" | "mcs" => "microsecond",
        "nanosecond" | "ns" => "nanosecond",
        "iso_week" | "isowk" | "isoww" => "iso_week",
        other => return Err(Error::runtime(format!("'{other}' is not a recognized date part"))),
    })
}

/// A date and time value as milliseconds, and the kind it was (so the result can be of the same kind).
#[derive(Copy, Clone, PartialEq)]
enum Kind {
    Date,
    Time,
    DateTime,
    Offset,
}

fn instant(v: &Value) -> Result<(i64, Kind)> {
    Ok(match v {
        Value::Date(t) => (*t, Kind::Date),
        Value::Time(t) => (*t, Kind::Time),
        Value::DateTime(t) => (*t, Kind::DateTime),
        Value::TimestampTz(t) => (crate::tz::session().to_local(*t), Kind::Offset),
        other => match other.coerce_to(DataType::DateTime)? {
            Value::DateTime(t) => (t, Kind::DateTime),
            _ => return Err(Error::type_err("a date was expected")),
        },
    })
}

fn rebuild(ms: i64, kind: Kind) -> Value {
    match kind {
        Kind::Date => Value::Date(datetime::trunc_day(ms)),
        Kind::Time => Value::Time(ms.rem_euclid(MS_PER_DAY)),
        Kind::DateTime => Value::DateTime(ms),
        Kind::Offset => Value::TimestampTz(crate::tz::session().to_utc(ms)),
    }
}

fn add_unit(ms: i64, unit: &str, n: i64) -> Result<i64> {
    let out_of_range = || Error::runtime("Adding a value to a 'datetime' column caused an overflow.");
    Ok(match unit {
        "year" => datetime::add_months(ms, n.checked_mul(12).ok_or_else(out_of_range)?).ok_or_else(out_of_range)?,
        "quarter" => datetime::add_months(ms, n.checked_mul(3).ok_or_else(out_of_range)?).ok_or_else(out_of_range)?,
        "month" => datetime::add_months(ms, n).ok_or_else(out_of_range)?,
        "dayofyear" | "day" | "weekday" => ms.checked_add(n.checked_mul(MS_PER_DAY).ok_or_else(out_of_range)?).ok_or_else(out_of_range)?,
        "week" | "iso_week" => ms.checked_add(n.checked_mul(7 * MS_PER_DAY).ok_or_else(out_of_range)?).ok_or_else(out_of_range)?,
        "hour" => ms.checked_add(n.checked_mul(3_600_000).ok_or_else(out_of_range)?).ok_or_else(out_of_range)?,
        "minute" => ms.checked_add(n.checked_mul(60_000).ok_or_else(out_of_range)?).ok_or_else(out_of_range)?,
        "second" => ms.checked_add(n.checked_mul(1000).ok_or_else(out_of_range)?).ok_or_else(out_of_range)?,
        "millisecond" => ms.checked_add(n).ok_or_else(out_of_range)?,
        "microsecond" => ms.checked_add(n / 1000).ok_or_else(out_of_range)?,
        _ => ms.checked_add(n / 1_000_000).ok_or_else(out_of_range)?,
    })
}

/// How many boundaries of `unit` lie between two instants (what `DATEDIFF` counts).
fn boundaries(unit: &str, a: i64, b: i64) -> i64 {
    let months = |t: i64| {
        let (y, m, ..) = datetime::components(t);
        y * 12 + i64::from(m) - 1
    };
    let days = |t: i64| t.div_euclid(MS_PER_DAY);
    match unit {
        "year" => months(b).div_euclid(12) - months(a).div_euclid(12),
        "quarter" => months(b).div_euclid(3) - months(a).div_euclid(3),
        "month" => months(b) - months(a),
        "dayofyear" | "day" | "weekday" => days(b) - days(a),
        // weeks start on Sunday (1970-01-01 was a Thursday)
        "week" | "iso_week" => (days(b) + 4).div_euclid(7) - (days(a) + 4).div_euclid(7),
        "hour" => b.div_euclid(3_600_000) - a.div_euclid(3_600_000),
        "minute" => b.div_euclid(60_000) - a.div_euclid(60_000),
        "second" => b.div_euclid(1000) - a.div_euclid(1000),
        "millisecond" => b - a,
        "microsecond" => (b - a).saturating_mul(1000),
        _ => (b - a).saturating_mul(1_000_000),
    }
}

fn part(unit: &str, ms: i64) -> i64 {
    let (y, mo, d, h, mi, s) = datetime::components(ms);
    let doy = datetime::days_from_civil(y, mo, d) - datetime::days_from_civil(y, 1, 1) + 1;
    match unit {
        "year" => y,
        "quarter" => i64::from((mo - 1) / 3 + 1),
        "month" => i64::from(mo),
        "dayofyear" => doy,
        "day" => i64::from(d),
        // the week of the year, Sunday first, the week with 1 January being week 1
        "week" => {
            let jan1 = datetime::weekday(datetime::days_from_civil(y, 1, 1) * MS_PER_DAY) - 1;
            (doy - 1 + jan1) / 7 + 1
        }
        "iso_week" => {
            // Monday-first weeks; week 1 holds the first Thursday
            let wd = (ms.div_euclid(MS_PER_DAY) + 3).rem_euclid(7); // Monday = 0
            let thursday = ms.div_euclid(MS_PER_DAY) - wd + 3;
            let (ty, ..) = datetime::civil_from_days(thursday);
            (thursday - datetime::days_from_civil(ty, 1, 1)) / 7 + 1
        }
        "weekday" => datetime::weekday(ms),
        "hour" => i64::from(h),
        "minute" => i64::from(mi),
        "second" => i64::from(s),
        "millisecond" => i64::from(datetime::millis(ms)),
        "microsecond" => i64::from(datetime::millis(ms)) * 1000,
        _ => i64::from(datetime::millis(ms)) * 1_000_000,
    }
}

const MONTH_NAMES: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAY_NAMES: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// `CONVERT(varchar, date, style)`: the styles of SQL Server that make text of a date.
fn styled(ms: i64, style: i64) -> Option<String> {
    let (y, mo, d, h, mi, s) = datetime::components(ms);
    let frac = datetime::millis(ms);
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    let ampm = if h < 12 { "AM" } else { "PM" };
    let mon = &MONTH_NAMES[(mo - 1) as usize][..3];
    let year = if style >= 100 { format!("{y:04}") } else { format!("{:02}", y % 100) };
    Some(match style {
        0 | 100 => format!("{mon} {d:2} {y} {h12:2}:{mi:02}{ampm}"),
        1 | 101 => format!("{mo:02}/{d:02}/{year}"),
        2 | 102 => format!("{year}.{mo:02}.{d:02}"),
        3 | 103 => format!("{d:02}/{mo:02}/{year}"),
        4 | 104 => format!("{d:02}.{mo:02}.{year}"),
        5 | 105 => format!("{d:02}-{mo:02}-{year}"),
        6 | 106 => format!("{d:02} {mon} {year}"),
        7 | 107 => format!("{mon} {d:02}, {year}"),
        8 | 108 => format!("{h:02}:{mi:02}:{s:02}"),
        10 | 110 => format!("{mo:02}-{d:02}-{year}"),
        11 | 111 => format!("{year}/{mo:02}/{d:02}"),
        12 | 112 => format!("{year}{mo:02}{d:02}"),
        20 | 120 => format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}"),
        21 | 121 => format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{frac:03}"),
        23 => format!("{y:04}-{mo:02}-{d:02}"),
        126 => format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{frac:03}"),
        127 => format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{frac:03}Z"),
        _ => return None,
    })
}

fn pad_left(s: String, width: usize) -> String {
    if s.len() >= width { s } else { format!("{}{s}", " ".repeat(width - s.len())) }
}

pub fn call(f: TsqlFn, vals: &[Cow<'_, Value>]) -> Result<Value> {
    use TsqlFn::*;
    let v = |i: usize| -> &Value { &vals[i] };
    Ok(match f {
        IsNull => {
            if v(0).is_null() {
                v(1).clone()
            } else {
                v(0).clone()
            }
        }
        GetDate => Value::DateTime(datetime::now()),
        GetUtcDate => Value::DateTime(crate::tz::session().to_utc(datetime::now())),
        SysDateTimeOffset => Value::TimestampTz(crate::tz::session().to_utc(datetime::now())),
        NewId => {
            let mut b = crate::expr::tsqlfuncs::random16();
            b[6] = (b[6] & 0x0f) | 0x40;
            b[8] = (b[8] & 0x3f) | 0x80;
            let h: String = b.iter().map(|x| format!("{x:02X}")).collect();
            Value::text(&format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
        }
        CharIndex => {
            let (needle, hay) = (text(v(0)).to_lowercase(), text(v(1)).to_lowercase());
            let start = if vals.len() > 2 { (int(v(2))?.max(1) - 1) as usize } else { 0 };
            let chars: Vec<char> = hay.chars().collect();
            if start > chars.len() {
                return Ok(Value::Int(0));
            }
            let tail: String = chars[start..].iter().collect();
            match tail.find(&needle) {
                Some(b) => Value::Int((start + tail[..b].chars().count() + 1) as i64),
                None => Value::Int(0),
            }
        }
        Stuff => {
            let s: Vec<char> = text(v(0)).chars().collect();
            let (start, len) = (int(v(1))?, int(v(2))?);
            if start < 1 || start as usize > s.len() + 1 || len < 0 {
                return Ok(Value::Null);
            }
            let (a, b) = ((start - 1) as usize, ((start - 1 + len) as usize).min(s.len()));
            let mut out: String = s[..a].iter().collect();
            out.push_str(&text(v(3)));
            out.extend(s[b..].iter());
            Value::text(&out)
        }
        Space => Value::text(&" ".repeat(int(v(0))?.clamp(0, 8_000_000) as usize)),
        Str => {
            let n = v(0).to_f64().ok_or_else(|| Error::type_err("a number was expected"))?;
            let width = if vals.len() > 1 { int(v(1))?.max(1) as usize } else { 10 };
            let decimals = if vals.len() > 2 { int(v(2))?.max(0) as usize } else { 0 };
            let s = format!("{n:.decimals$}");
            Value::text(&if s.len() > width { "*".repeat(width) } else { pad_left(s, width) })
        }
        Reverse => Value::text(&text(v(0)).chars().rev().collect::<String>()),
        DateAdd => {
            let unit = unit_of(&text(v(0)))?;
            let n = int(v(1))?;
            let (ms, kind) = instant(v(2))?;
            let time_unit = matches!(unit, "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond");
            let kind = if kind == Kind::Date && time_unit { Kind::DateTime } else { kind };
            rebuild(add_unit(ms, unit, n)?, kind)
        }
        DateDiff => {
            let unit = unit_of(&text(v(0)))?;
            let ((a, _), (b, _)) = (instant(v(1))?, instant(v(2))?);
            Value::Int(boundaries(unit, a, b))
        }
        DatePart => {
            let unit = unit_of(&text(v(0)))?;
            Value::Int(part(unit, instant(v(1))?.0))
        }
        DateName => {
            let unit = unit_of(&text(v(0)))?;
            let ms = instant(v(1))?.0;
            match unit {
                "month" => Value::text(MONTH_NAMES[(datetime::components(ms).1 - 1) as usize]),
                "weekday" => Value::text(DAY_NAMES[(part("weekday", ms) - 1) as usize]),
                other => Value::text(&part(other, ms).to_string()),
            }
        }
        EoMonth => {
            let (ms, _) = instant(v(0))?;
            let ms = if vals.len() > 1 { add_unit(ms, "month", int(v(1))?)? } else { ms };
            let (y, mo, ..) = datetime::components(ms);
            Value::Date(datetime::from_components(y, mo, datetime::days_in_month(y, mo), 0, 0, 0).ok_or_else(|| Error::runtime("date out of range"))?)
        }
        DateFromParts => {
            let t = datetime::from_components(int(v(0))?, int(v(1))? as u32, int(v(2))? as u32, 0, 0, 0)
                .ok_or_else(|| Error::runtime("Cannot construct data type date, some of the arguments have values which are not valid."))?;
            Value::Date(t)
        }
        DateTimeFromParts => {
            let g = |i: usize| if i < vals.len() { int(v(i)) } else { Ok(0) };
            let t = datetime::from_components(g(0)?, g(1)? as u32, g(2)? as u32, g(3)? as u32, g(4)? as u32, g(5)? as u32)
                .ok_or_else(|| Error::runtime("Cannot construct data type datetime, some of the arguments have values which are not valid."))?;
            Value::DateTime(t + g(6)?)
        }
        TimeFromParts => {
            let (h, mi, s) = (int(v(0))?, int(v(1))?, int(v(2))?);
            if !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..60).contains(&s) {
                return Err(Error::runtime("Cannot construct data type time, some of the arguments have values which are not valid."));
            }
            Value::Time(((h * 60 + mi) * 60 + s) * 1000)
        }
        IsNumeric => Value::Int(i64::from(!v(0).is_null() && v(0).to_f64().is_some() && !matches!(v(0), Value::Bool(_)))),
        IsDate => Value::Int(i64::from(!v(0).is_null() && matches!(v(0), Value::Text(t) if datetime::parse(t).is_some()) || matches!(v(0), Value::DateTime(_) | Value::Date(_)))),
        Choose => {
            let i = int(v(0))?;
            if i >= 1 && (i as usize) < vals.len() {
                v(i as usize).clone()
            } else {
                Value::Null
            }
        }
        Char => match int(v(0))? {
            n @ 0..=0x10FFFF => char::from_u32(n as u32).map_or(Value::Null, |c| Value::text(&c.to_string())),
            _ => Value::Null,
        },
        Ascii => Value::Int(text(v(0)).chars().next().map_or(0, |c| c as i64)),
        QuoteName => {
            let (open, close) = match vals.get(1).map(|q| text(q).to_string()).as_deref() {
                Some("\"") => ('"', '"'),
                Some("'") => ('\'', '\''),
                _ => ('[', ']'),
            };
            let s = text(v(0));
            Value::text(&format!("{open}{}{close}", s.replace(close, &format!("{close}{close}"))))
        }
        Square => {
            let x = v(0).to_f64().ok_or_else(|| Error::type_err("a number was expected"))?;
            Value::Double(x * x)
        }
        Pi => Value::Double(std::f64::consts::PI),
        Rand => Value::Double({
            let b = random16();
            u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f64 / u64::MAX as f64
        }),
        // CONCAT treats NULL as an empty string
        Concat => Value::text(&vals.iter().map(|x| if x.is_null() { String::new() } else { text(x).to_string() }).collect::<String>()),
        DataLength => match v(0) {
            Value::Null => Value::Null,
            Value::Bytes(b) => Value::Int(b.len() as i64),
            Value::Int(_) => Value::Int(8),
            other => Value::Int(text(other).len() as i64),
        },
        DbName => Value::text("master"),
        HostName => Value::text("accessor"),
        UserName => Value::text("dbo"),
        ErrorMessage => crate::error::handled().map_or(Value::Null, |d| Value::text(&d.message)),
        ErrorNumber => crate::error::handled().map_or(Value::Null, |d| Value::Int(if d.errno != 0 { d.errno } else { 50000 })),
        ErrorSeverity => crate::error::handled().map_or(Value::Null, |_| Value::Int(16)),
        ErrorState => crate::error::handled().map_or(Value::Null, |_| Value::Int(1)),
        ErrorLine => crate::error::handled().map_or(Value::Null, |_| Value::Int(1)),
        ErrorProcedure => Value::Null,
        FormatMessage => {
            let template = text(v(0)).to_string();
            let mut args = vals[1..].iter();
            let mut out = String::new();
            let mut chars = template.chars().peekable();
            while let Some(c) = chars.next() {
                if c != '%' {
                    out.push(c);
                    continue;
                }
                match chars.next() {
                    Some('%') => out.push('%'),
                    Some(k) if matches!(k, 's' | 'd' | 'i' | 'u' | 'x' | 'X' | 'o') => {
                        let a = args.next().map_or(String::new(), |a| if a.is_null() { "NULL".into() } else { text(a).to_string() });
                        out.push_str(&a);
                    }
                    Some(other) => {
                        out.push('%');
                        out.push(other);
                    }
                    None => out.push('%'),
                }
            }
            Value::text(&out)
        }
        CanCast => {
            if v(0).is_null() {
                return Ok(Value::Null);
            }
            let name = text(v(1)).to_string();
            Value::Bool(match DataType::from_sql_name(&name) {
                Some((t, _)) => v(0).coerce_to(t).is_ok(),
                None => false,
            })
        }
        Style => {
            let style = int(v(1))?;
            if v(0).is_null() {
                return Ok(Value::Null);
            }
            match v(0) {
                Value::DateTime(_) | Value::Date(_) | Value::TimestampTz(_) => match styled(instant(v(0))?.0, style) {
                    Some(s) => Value::text(&s),
                    None => Value::text(&v(0).to_string()),
                },
                other => Value::text(&other.to_string()),
            }
        }
        IdentityOf => match super::myfuncs::last_insert_id() {
            0 => Value::Null,
            n => Value::Int(n),
        },
        ObjectId => Value::Null,
        ObjectName => {
            // [dbo].[Emp], dbo.Emp, "Emp", tempdb..#t: the name of the object, with no schema or quotes
            let t = text(v(0)).to_string();
            let last = t.rsplit('.').next().unwrap_or(&t);
            Value::text(last.trim_matches(|c| c == '[' || c == ']' || c == '"'))
        }
        ObjectIdOf => {
            // a number for a table that does not change while it exists: a hash of its name
            let mut h: u64 = 0xcbf29ce484222325;
            for b in text(v(0)).to_lowercase().bytes() {
                h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
            }
            Value::Int(((h >> 33) as i64) + 100_000_000)
        }
        Format => match v(0) {
            Value::Null => Value::Null,
            val @ (Value::Date(_) | Value::DateTime(_) | Value::TimestampTz(_) | Value::Time(_)) => {
                let (ms, kind) = instant(val)?;
                Value::text(&format_date(ms, kind, &text(v(1)))?)
            }
            Value::Text(s) => Value::Text(s.clone()),
            val => match val.to_f64() {
                Some(x) => Value::text(&format_number(x, &text(v(1)))?),
                None => Value::text(&val.to_string()),
            },
        },
        Version => Value::text("Microsoft SQL Server 2022 (Accessor) - 16.0.1000.6"),
    })
}

// ----- FORMAT -------------------------------------------------------------------------------------------------------

fn group_thousands(digits: &str) -> String {
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `FORMAT(date, pattern)`: the standard letters and custom patterns of .NET, in the culture en-US.
fn format_date(ms: i64, kind: Kind, pattern: &str) -> Result<String> {
    let expanded = match pattern {
        "d" => "M/d/yyyy",
        "D" => "dddd, MMMM d, yyyy",
        "f" => "dddd, MMMM d, yyyy h:mm tt",
        "F" => "dddd, MMMM d, yyyy h:mm:ss tt",
        "g" => "M/d/yyyy h:mm tt",
        "G" => "M/d/yyyy h:mm:ss tt",
        "m" | "M" => "MMMM d",
        "y" | "Y" => "MMMM yyyy",
        "t" => "h:mm tt",
        "T" => "h:mm:ss tt",
        "s" => "yyyy'-'MM'-'dd'T'HH':'mm':'ss",
        "o" | "O" => "yyyy'-'MM'-'dd'T'HH':'mm':'ss'.'fffffff",
        "u" => "yyyy'-'MM'-'dd HH':'mm':'ss'Z'",
        "r" | "R" => "ddd, dd MMM yyyy HH':'mm':'ss 'GMT'",
        p => p,
    };
    let _ = kind;
    let (y, mo, d, h, mi, s) = datetime::components(ms);
    let frac = datetime::millis(ms);
    let weekday = (datetime::trunc_day(ms).div_euclid(MS_PER_DAY) + 4).rem_euclid(7) as usize;
    let chars: Vec<char> = expanded.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let run = chars[i..].iter().take_while(|&&x| x == c).count();
        match c {
            '\'' | '"' => {
                let end = chars[i + 1..].iter().position(|&x| x == c).map_or(chars.len(), |p| i + 1 + p);
                out.extend(&chars[i + 1..end]);
                i = end + 1;
                continue;
            }
            '\\' => {
                if let Some(&n) = chars.get(i + 1) {
                    out.push(n);
                }
                i += 2;
                continue;
            }
            'y' => match run {
                1 => out.push_str(&(y % 100).to_string()),
                2 => out.push_str(&format!("{:02}", y % 100)),
                n => out.push_str(&format!("{y:0width$}", width = n.max(4))),
            },
            'M' => match run {
                1 => out.push_str(&mo.to_string()),
                2 => out.push_str(&format!("{mo:02}")),
                3 => out.push_str(&MONTH_NAMES[(mo - 1) as usize][..3]),
                _ => out.push_str(MONTH_NAMES[(mo - 1) as usize]),
            },
            'd' => match run {
                1 => out.push_str(&d.to_string()),
                2 => out.push_str(&format!("{d:02}")),
                3 => out.push_str(&DAY_NAMES[weekday][..3]),
                _ => out.push_str(DAY_NAMES[weekday]),
            },
            'H' => out.push_str(&if run == 1 { h.to_string() } else { format!("{h:02}") }),
            'h' => {
                let h12 = if h % 12 == 0 { 12 } else { h % 12 };
                out.push_str(&if run == 1 { h12.to_string() } else { format!("{h12:02}") });
            }
            'm' => out.push_str(&if run == 1 { mi.to_string() } else { format!("{mi:02}") }),
            's' => out.push_str(&if run == 1 { s.to_string() } else { format!("{s:02}") }),
            'f' | 'F' => {
                let digits = format!("{frac:03}0000");
                let n = run.min(7);
                let mut part = digits[..n].to_string();
                if c == 'F' {
                    part = part.trim_end_matches('0').to_string();
                }
                out.push_str(&part);
            }
            't' => {
                let m = if h < 12 { "AM" } else { "PM" };
                out.push_str(if run == 1 { &m[..1] } else { m });
            }
            '/' => out.push('/'),
            other => out.extend(std::iter::repeat(other).take(run)),
        }
        i += run;
    }
    Ok(out)
}

/// `FORMAT(number, pattern)`: the standard letters (`N2`, `C`, `P1`, `D4`, `X`, `F3`, `E2`, `G`) and custom patterns
/// (`#,##0.00`, `000`, `0.0%`).
fn format_number(x: f64, pattern: &str) -> Result<String> {
    let mut letters = pattern.chars();
    let first = letters.next().unwrap_or('G');
    let rest: String = letters.collect();
    let precision = if rest.is_empty() { None } else { rest.parse::<usize>().ok() };
    if first.is_ascii_alphabetic() && (rest.is_empty() || precision.is_some()) {
        let neg = x < 0.0;
        let a = x.abs();
        let fixed = |n: usize| format!("{a:.n$}");
        let grouped = |n: usize| {
            let s = fixed(n);
            match s.split_once('.') {
                Some((i, f)) => format!("{}.{f}", group_thousands(i)),
                None => group_thousands(&s),
            }
        };
        let sign = if neg { "-" } else { "" };
        return Ok(match first.to_ascii_uppercase() {
            'N' => format!("{sign}{}", grouped(precision.unwrap_or(2))),
            'F' => format!("{sign}{}", fixed(precision.unwrap_or(2))),
            'C' => format!("{sign}${}", grouped(precision.unwrap_or(2))),
            'P' => format!("{sign}{}%", {
                let p = a * 100.0;
                let s = format!("{p:.*}", precision.unwrap_or(2));
                match s.split_once('.') {
                    Some((i, f)) => format!("{}.{f}", group_thousands(i)),
                    None => group_thousands(&s),
                }
            }),
            'D' => {
                let digits = format!("{}", a.trunc() as i64);
                format!("{sign}{digits:0>width$}", width = precision.unwrap_or(1))
            }
            'X' => {
                let s = format!("{:X}", x as i64);
                let s = if first == 'x' { s.to_lowercase() } else { s };
                format!("{s:0>width$}", width = precision.unwrap_or(1))
            }
            'E' => {
                let s = format!("{a:.*e}", precision.unwrap_or(6));
                let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
                let e: i32 = e.parse().unwrap_or(0);
                format!("{sign}{m}{}{}{:03}", if first == 'e' { 'e' } else { 'E' }, if e < 0 { '-' } else { '+' }, e.abs())
            }
            'G' | 'R' => {
                let s = if a == a.trunc() && a.abs() < 1e15 { format!("{}", a as i64) } else { format!("{a}") };
                format!("{sign}{s}")
            }
            _ => return Err(Error::runtime(format!("The format specifier '{first}' is not valid"))),
        });
    }
    // a custom pattern: 0 and # are digits, `,` groups, `.` is the decimal point, `%` multiplies by 100
    let percent = pattern.contains('%');
    let x = if percent { x * 100.0 } else { x };
    let (int_pat, frac_pat) = match pattern.split_once('.') {
        Some((i, f)) => (i, f),
        None => (pattern, ""),
    };
    let min_frac = frac_pat.chars().filter(|&c| c == '0').count();
    let max_frac = frac_pat.chars().filter(|&c| c == '0' || c == '#').count();
    let group = int_pat.contains(',');
    let min_int = int_pat.chars().filter(|&c| c == '0').count();
    let a = x.abs();
    let text = format!("{a:.max_frac$}");
    let (int_text, frac_text) = text.split_once('.').unwrap_or((&text, ""));
    let mut frac_text = frac_text.to_string();
    while frac_text.len() > min_frac && frac_text.ends_with('0') {
        frac_text.pop();
    }
    let mut int_digits = int_text.trim_start_matches('0').to_string();
    while int_digits.len() < min_int {
        int_digits.insert(0, '0');
    }
    if group {
        int_digits = group_thousands(&int_digits);
    }
    let nonzero = int_text.chars().any(|c| c != '0') || frac_text.chars().any(|c| c != '0');
    // the literal text around the digits
    let prefix: String = int_pat.chars().take_while(|c| !matches!(c, '0' | '#' | ',')).collect();
    let suffix_int: String = int_pat.chars().rev().take_while(|c| !matches!(c, '0' | '#' | ',')).collect::<Vec<_>>().into_iter().rev().collect();
    let suffix_frac: String = frac_pat.chars().rev().take_while(|c| !matches!(c, '0' | '#')).collect::<Vec<_>>().into_iter().rev().collect();
    let mut out = String::new();
    if x < 0.0 && nonzero {
        out.push('-');
    }
    out.push_str(&prefix.replace('%', ""));
    out.push_str(&int_digits);
    if !frac_text.is_empty() {
        out.push('.');
        out.push_str(&frac_text);
    }
    out.push_str(&suffix_frac.replace('%', ""));
    if frac_pat.is_empty() {
        out.push_str(&suffix_int.replace('%', ""));
    }
    if percent {
        out.push('%');
    }
    Ok(out)
}

/// Sixteen random bytes (for `NEWID` and `RAND`).
pub(crate) fn random16() -> [u8; 16] {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = [0u8; 16];
    for chunk in out.chunks_mut(8) {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(datetime::now() as u64);
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}
