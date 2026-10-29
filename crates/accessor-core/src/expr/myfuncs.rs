//! Functions that MySQL has and the engine did not: `HEX`, `UNHEX`, `ASCII`, `LOCATE`, `FIELD`, `ELT`,
//! `FIND_IN_SET`, `STR_TO_DATE`, `DATE_FORMAT`, `UNIX_TIMESTAMP`, `FROM_UNIXTIME`, `UUID`, `MD5`, `SHA2`, the
//! `INET_*` and `IS_IPV*` functions, `SOUNDEX`, and the ones that have nothing to do on one embedded connection
//! (`SLEEP`, `GET_LOCK`, `CONNECTION_ID`, `CHARSET`, `COLLATION`).

use std::borrow::Cow;

use chrono::{Datelike, Timelike};

use crate::error::{Error, Result};
use crate::value::Value;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MyFn {
    Hex,
    Unhex,
    Ascii,
    Space,
    Locate,
    Field,
    Elt,
    FindInSet,
    StrToDate,
    DateFormat,
    UnixTimestamp,
    FromUnixtime,
    SecToTime,
    Uuid,
    Sha2,
    Md5,
    Charset,
    Collation,
    Coercibility,
    Sleep,
    Lock,
    ConnectionId,
    IsIpv4,
    IsIpv6,
    InetAton,
    InetNtoa,
    Soundex,
    JsonContainsPath,
    SubstringIndex,
    DayName,
    MonthName,
    LastDay,
    ToDays,
    Char,
    Bin,
    Oct,
    BitShl,
    BitShr,
    BitOr,
    BitNot,
    TimeOf,
    Conv,
    Strcmp,
    MakeTime,
    AddTime,
    SubTime,
    TimeDiff,
    TimestampDiff,
    ConvertTz,
    BitCount,
    LoadFile,
    ToBase64,
    FromBase64,
}

pub fn lookup(name: &str) -> Option<(MyFn, usize, usize)> {
    use MyFn::*;
    Some(match name {
        "HEX" => (Hex, 1, 1),
        "UNHEX" => (Unhex, 1, 1),
        "ASCII" | "ORD" => (Ascii, 1, 1),
        "SPACE" => (Space, 1, 1),
        "LOCATE" | "POSITION_IN" => (Locate, 2, 3),
        "FIELD" => (Field, 2, usize::MAX),
        "ELT" => (Elt, 2, usize::MAX),
        "FIND_IN_SET" => (FindInSet, 2, 2),
        "STR_TO_DATE" => (StrToDate, 2, 2),
        "DATE_FORMAT" => (DateFormat, 2, 2),
        "UNIX_TIMESTAMP" => (UnixTimestamp, 0, 1),
        "FROM_UNIXTIME" => (FromUnixtime, 1, 2),
        "SEC_TO_TIME" => (SecToTime, 1, 1),
        "UUID" => (Uuid, 0, 0),
        "SHA2" => (Sha2, 2, 2),
        "MD5" => (Md5, 1, 1),
        "CHARSET" => (Charset, 1, 1),
        "COLLATION" => (Collation, 1, 1),
        "COERCIBILITY" => (Coercibility, 1, 1),
        "SLEEP" | "BENCHMARK" => (Sleep, 1, 2),
        "GET_LOCK" | "RELEASE_LOCK" | "IS_FREE_LOCK" | "IS_USED_LOCK" | "RELEASE_ALL_LOCKS" => (Lock, 0, 2),
        "CONNECTION_ID" => (ConnectionId, 0, 0),
        "IS_IPV4" => (IsIpv4, 1, 1),
        "IS_IPV6" => (IsIpv6, 1, 1),
        "INET_ATON" => (InetAton, 1, 1),
        "INET_NTOA" => (InetNtoa, 1, 1),
        "SOUNDEX" => (Soundex, 1, 1),
        "JSON_CONTAINS_PATH" => (JsonContainsPath, 3, usize::MAX),
        "SUBSTRING_INDEX" => (SubstringIndex, 3, 3),
        "DAYNAME" => (DayName, 1, 1),
        "MONTHNAME" => (MonthName, 1, 1),
        "LAST_DAY" => (LastDay, 1, 1),
        "TO_DAYS" => (ToDays, 1, 1),
        "CHAR" => (Char, 1, usize::MAX),
        "BIN" => (Bin, 1, 1),
        "BIT_SHL" => (BitShl, 2, 2),
        "BIT_SHR" => (BitShr, 2, 2),
        "__BITOR" => (BitOr, 2, 2),
        "__BITNOT" => (BitNot, 1, 1),
        "__TIMEOF" => (TimeOf, 1, 1),
        "CONV" => (Conv, 3, 3),
        "OCT" => (Oct, 1, 1),
        "STRCMP" => (Strcmp, 2, 2),
        "MAKETIME" => (MakeTime, 3, 3),
        "ADDTIME" => (AddTime, 2, 2),
        "SUBTIME" => (SubTime, 2, 2),
        "TIMEDIFF" => (TimeDiff, 2, 2),
        "TIMESTAMPDIFF" => (TimestampDiff, 3, 3),
        "CONVERT_TZ" => (ConvertTz, 3, 3),
        "BIT_COUNT" => (BitCount, 1, 1),
        "LOAD_FILE" => (LoadFile, 1, 1),
        "TO_BASE64" => (ToBase64, 1, 1),
        "FROM_BASE64" => (FromBase64, 1, 1),
        _ => return None,
    })
}

impl MyFn {
    /// Functions that get their NULL arguments instead of answering NULL themselves.
    pub fn handles_null(self) -> bool {
        matches!(self, MyFn::Field | MyFn::Char)
    }

    /// Functions that give a different answer each time.
    pub fn is_volatile(self) -> bool {
        matches!(self, MyFn::Uuid | MyFn::Sleep | MyFn::Lock | MyFn::UnixTimestamp)
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::Text(s) => s.to_string(),
        other => other.to_string(),
    }
}

fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Text(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        other => other.to_string().trim().parse::<f64>().ok().map(|f| f as i64),
    }
}

fn datetime_of(v: &Value) -> Option<chrono::NaiveDateTime> {
    let secs = match v {
        Value::DateTime(t) | Value::TimestampTz(t) => *t,
        Value::Text(s) => match Value::text(s).coerce_to(crate::value::DataType::DateTime).ok()? {
            Value::DateTime(t) => t,
            _ => return None,
        },
        _ => return None,
    };
    chrono::DateTime::from_timestamp(secs, 0).map(|d| d.naive_utc())
}

fn to_secs(d: chrono::NaiveDateTime) -> i64 {
    d.and_utc().timestamp()
}

const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// `DATE_FORMAT(d, '%Y-%m-%d %H:%i:%s')`.
fn format_date(d: chrono::NaiveDateTime, fmt: &str) -> String {
    let mut out = String::new();
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(s) = it.next() else { break };
        let h12 = if d.hour() % 12 == 0 { 12 } else { d.hour() % 12 };
        match s {
            'Y' => out.push_str(&format!("{:04}", d.year())),
            'y' => out.push_str(&format!("{:02}", d.year() % 100)),
            'm' => out.push_str(&format!("{:02}", d.month())),
            'c' => out.push_str(&d.month().to_string()),
            'd' => out.push_str(&format!("{:02}", d.day())),
            'e' => out.push_str(&d.day().to_string()),
            'H' => out.push_str(&format!("{:02}", d.hour())),
            'k' => out.push_str(&d.hour().to_string()),
            'h' | 'I' => out.push_str(&format!("{h12:02}")),
            'l' => out.push_str(&h12.to_string()),
            'i' => out.push_str(&format!("{:02}", d.minute())),
            's' | 'S' => out.push_str(&format!("{:02}", d.second())),
            'f' => out.push_str("000000"),
            'p' => out.push_str(if d.hour() < 12 { "AM" } else { "PM" }),
            'T' => out.push_str(&format!("{:02}:{:02}:{:02}", d.hour(), d.minute(), d.second())),
            'r' => out.push_str(&format!("{h12:02}:{:02}:{:02} {}", d.minute(), d.second(), if d.hour() < 12 { "AM" } else { "PM" })),
            'M' => out.push_str(MONTHS[d.month0() as usize]),
            'b' => out.push_str(&MONTHS[d.month0() as usize][..3]),
            'W' => out.push_str(DAYS[d.weekday().num_days_from_monday() as usize]),
            'a' => out.push_str(&DAYS[d.weekday().num_days_from_monday() as usize][..3]),
            'j' => out.push_str(&format!("{:03}", d.ordinal())),
            'w' => out.push_str(&d.weekday().num_days_from_sunday().to_string()),
            '%' => out.push('%'),
            other => out.push(other),
        }
    }
    out
}

/// `STR_TO_DATE('01,5,2013', '%d,%m,%Y')`; `None` when the text does not fit the format.
fn parse_date(s: &str, fmt: &str) -> Option<chrono::NaiveDateTime> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let (mut year, mut month, mut day, mut hour, mut min, mut sec) = (1970i32, 1u32, 1u32, 0u32, 0u32, 0u32);
    let mut pm: Option<bool> = None;
    let number = |i: &mut usize, max: usize| -> Option<u32> {
        let start = *i;
        while *i < b.len() && *i - start < max && b[*i].is_ascii_digit() {
            *i += 1;
        }
        (*i > start).then(|| b[start..*i].iter().collect::<String>().parse().ok()).flatten()
    };
    let mut f = fmt.chars();
    while let Some(c) = f.next() {
        if c != '%' {
            if c.is_whitespace() {
                while i < b.len() && b[i].is_whitespace() {
                    i += 1;
                }
            } else if b.get(i) == Some(&c) {
                i += 1;
            } else {
                return None;
            }
            continue;
        }
        match f.next()? {
            'Y' => year = number(&mut i, 4)? as i32,
            'y' => {
                let y = number(&mut i, 2)? as i32;
                year = if y < 70 { 2000 + y } else { 1900 + y };
            }
            'm' | 'c' => month = number(&mut i, 2)?,
            'd' | 'e' => day = number(&mut i, 2)?,
            'H' | 'k' | 'h' | 'I' | 'l' => hour = number(&mut i, 2)?,
            'i' => min = number(&mut i, 2)?,
            's' | 'S' => sec = number(&mut i, 2)?,
            'f' => {
                number(&mut i, 6)?;
            }
            'p' => {
                let w: String = b[i..].iter().take(2).collect::<String>().to_ascii_uppercase();
                pm = Some(match w.as_str() {
                    "AM" => false,
                    "PM" => true,
                    _ => return None,
                });
                i += 2;
            }
            'T' => {
                hour = number(&mut i, 2)?;
                if b.get(i) != Some(&':') {
                    return None;
                }
                i += 1;
                min = number(&mut i, 2)?;
                if b.get(i) != Some(&':') {
                    return None;
                }
                i += 1;
                sec = number(&mut i, 2)?;
            }
            'M' | 'b' => {
                let rest: String = b[i..].iter().collect::<String>().to_ascii_lowercase();
                let (k, len) = MONTHS.iter().enumerate().find_map(|(k, m)| {
                    let full = m.to_ascii_lowercase();
                    if rest.starts_with(&full) {
                        Some((k, full.len()))
                    } else if rest.starts_with(&full[..3]) {
                        Some((k, 3))
                    } else {
                        None
                    }
                })?;
                month = k as u32 + 1;
                i += len;
            }
            'W' | 'a' => {
                while i < b.len() && b[i].is_alphabetic() {
                    i += 1;
                }
            }
            '%' => {
                if b.get(i) != Some(&'%') {
                    return None;
                }
                i += 1;
            }
            _ => return None,
        }
    }
    if let Some(p) = pm {
        hour = hour % 12 + if p { 12 } else { 0 };
    }
    chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, min, sec)
}

fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f2.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, v) in [a0, b0, c0, d0].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let primes: Vec<u64> = (2u64..).filter(|n| (2..*n).take_while(|d| d * d <= *n).all(|d| n % d != 0)).take(64).collect();
    let frac = |x: f64| ((x - x.floor()) * 4294967296.0) as u32;
    let k: Vec<u32> = primes.iter().map(|&p| frac((p as f64).cbrt())).collect();
    let mut h: Vec<u32> = primes.iter().take(8).map(|&p| frac((p as f64).sqrt())).collect();
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, c) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h.clone();
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(k[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = vec![t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn soundex(s: &str) -> String {
    let code = |c: char| match c.to_ascii_uppercase() {
        'B' | 'F' | 'P' | 'V' => '1',
        'C' | 'G' | 'J' | 'K' | 'Q' | 'S' | 'X' | 'Z' => '2',
        'D' | 'T' => '3',
        'L' => '4',
        'M' | 'N' => '5',
        'R' => '6',
        _ => '0',
    };
    let letters: Vec<char> = s.chars().filter(|c| c.is_alphabetic()).collect();
    let Some(first) = letters.first() else { return String::new() };
    let mut out = String::from(first.to_ascii_uppercase());
    let mut last = code(*first);
    for c in &letters[1..] {
        let k = code(*c);
        if k != '0' && k != last {
            out.push(k);
        }
        if !matches!(c.to_ascii_uppercase(), 'H' | 'W') {
            last = k;
        }
    }
    while out.len() < 4 {
        out.push('0');
    }
    out
}

fn ipv4(s: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 || parts.iter().any(|p| p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let mut out = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p.parse().ok()?;
    }
    Some(out)
}

pub fn call(f: MyFn, vals: &[Cow<'_, Value>]) -> Result<Value> {
    use MyFn::*;
    let v = |i: usize| -> &Value { &vals[i] };
    Ok(match f {
        Hex => match v(0) {
            Value::Int(i) => Value::text(&format!("{:X}", *i as u64)),
            other => Value::text(&text(other).bytes().map(|b| format!("{b:02X}")).collect::<String>()),
        },
        Unhex => {
            let s = text(v(0));
            if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Ok(Value::Null);
            }
            let bytes: Vec<u8> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0)).collect();
            Value::text(&String::from_utf8_lossy(&bytes))
        }
        Ascii => Value::Int(text(v(0)).bytes().next().map_or(0, i64::from)),
        Space => Value::text(&" ".repeat(int(v(0)).unwrap_or(0).clamp(0, 1 << 20) as usize)),
        Locate => {
            let (needle, hay) = (text(v(0)).to_lowercase(), text(v(1)).to_lowercase());
            let start = vals.get(2).and_then(|p| int(p)).unwrap_or(1).max(1) as usize - 1;
            let skip: usize = hay.chars().take(start).map(char::len_utf8).sum();
            match hay.get(skip..).and_then(|h| h.find(&needle)) {
                Some(p) => Value::Int(hay[..skip + p].chars().count() as i64 + 1),
                None => Value::Int(0),
            }
        }
        Field => {
            if v(0).is_null() {
                return Ok(Value::Int(0));
            }
            let key = text(v(0)).to_lowercase();
            let hit = vals[1..].iter().position(|x| !x.is_null() && (text(x).to_lowercase() == key || v(0).sql_cmp(x) == Some(std::cmp::Ordering::Equal)));
            Value::Int(hit.map_or(0, |p| p as i64 + 1))
        }
        Elt => match int(v(0)) {
            Some(n) if n >= 1 && (n as usize) < vals.len() => vals[n as usize].as_ref().clone(),
            _ => Value::Null,
        },
        FindInSet => {
            let (needle, list) = (text(v(0)), text(v(1)));
            Value::Int(list.split(',').position(|x| x.eq_ignore_ascii_case(&needle)).map_or(0, |p| p as i64 + 1))
        }
        StrToDate => match parse_date(&text(v(0)), &text(v(1))) {
            Some(d) => Value::DateTime(to_secs(d)),
            None => Value::Null,
        },
        DateFormat => match datetime_of(v(0)) {
            Some(d) => Value::text(&format_date(d, &text(v(1)))),
            None => Value::Null,
        },
        UnixTimestamp => match vals.first() {
            None => Value::Int(crate::datetime::now()),
            Some(x) => datetime_of(x).map_or(Value::Null, |d| Value::Int(to_secs(d))),
        },
        FromUnixtime => {
            let Some(secs) = int(v(0)) else { return Ok(Value::Null) };
            match vals.get(1) {
                Some(fmt) => match chrono::DateTime::from_timestamp(secs, 0) {
                    Some(d) => Value::text(&format_date(d.naive_utc(), &text(fmt))),
                    None => Value::Null,
                },
                None => Value::DateTime(secs),
            }
        }
        SecToTime => {
            let n = int(v(0)).unwrap_or(0);
            let sign = if n < 0 { "-" } else { "" };
            let n = n.abs();
            Value::text(&format!("{sign}{:02}:{:02}:{:02}", n / 3600, n / 60 % 60, n % 60))
        }
        Uuid => {
            use std::sync::atomic::{AtomicU64, Ordering};
            static N: AtomicU64 = AtomicU64::new(1);
            let seed = format!("{:?}-{}", std::time::SystemTime::now(), N.fetch_add(1, Ordering::Relaxed));
            let h = hex(&md5(seed.as_bytes()));
            Value::text(&format!("{}-{}-4{}-a{}-{}", &h[0..8], &h[8..12], &h[13..16], &h[17..20], &h[20..32]))
        }
        Md5 => Value::text(&hex(&md5(text(v(0)).as_bytes()))),
        Sha2 => match int(v(1)) {
            Some(0) | Some(256) => Value::text(&hex(&sha256(text(v(0)).as_bytes()))),
            _ => Value::Null,
        },
        Charset => Value::text("utf8mb4"),
        Collation => Value::text("utf8mb4_0900_ai_ci"),
        Coercibility => Value::Int(2),
        Sleep => Value::Int(0),
        Lock => Value::Int(1),
        ConnectionId => Value::Int(1),
        IsIpv4 => Value::Int(i64::from(ipv4(&text(v(0))).is_some())),
        IsIpv6 => Value::Int(i64::from(text(v(0)).parse::<std::net::Ipv6Addr>().is_ok())),
        InetAton => match ipv4(&text(v(0))) {
            Some(o) => Value::Int(i64::from(u32::from_be_bytes(o))),
            None => Value::Null,
        },
        InetNtoa => match int(v(0)) {
            Some(n) if (0..=u32::MAX as i64).contains(&n) => Value::text(&std::net::Ipv4Addr::from(n as u32).to_string()),
            _ => Value::Null,
        },
        Soundex => Value::text(&soundex(&text(v(0)))),
        JsonContainsPath => {
            let doc = crate::json::document(v(0))?;
            let all = text(v(1)).eq_ignore_ascii_case("all");
            let mut found = Vec::new();
            for p in &vals[2..] {
                let path = crate::jsonpath::parse(&text(p))?;
                found.push(!crate::jsonpath::query(&doc, &path, None, true)?.is_empty());
            }
            Value::Int(i64::from(if all { found.iter().all(|x| *x) } else { found.iter().any(|x| *x) }))
        }
        SubstringIndex => {
            let (s, delim, n) = (text(v(0)), text(v(1)), int(v(2)).unwrap_or(0));
            if delim.is_empty() || n == 0 {
                return Ok(Value::text(""));
            }
            let parts: Vec<&str> = s.split(delim.as_str()).collect();
            let k = n.unsigned_abs() as usize;
            if k >= parts.len() {
                return Ok(Value::text(&s));
            }
            Value::text(&if n > 0 { parts[..k].join(&delim) } else { parts[parts.len() - k..].join(&delim) })
        }
        DayName => datetime_of(v(0)).map_or(Value::Null, |d| Value::text(DAYS[d.weekday().num_days_from_monday() as usize])),
        MonthName => datetime_of(v(0)).map_or(Value::Null, |d| Value::text(MONTHS[d.month0() as usize])),
        LastDay => match datetime_of(v(0)) {
            Some(d) => {
                let (y, m) = if d.month() == 12 { (d.year() + 1, 1) } else { (d.year(), d.month() + 1) };
                let first_next = chrono::NaiveDate::from_ymd_opt(y, m, 1).ok_or_else(|| Error::runtime("date out of range"))?;
                Value::DateTime(to_secs((first_next - chrono::Duration::days(1)).and_hms_opt(0, 0, 0).expect("midnight")))
            }
            None => Value::Null,
        },
        ToDays => match datetime_of(v(0)) {
            Some(d) => Value::Int(i64::from(d.date().num_days_from_ce()) + 365),
            None => Value::Null,
        },
        Char => {
            let s: String = vals.iter().filter_map(|x| int(x)).filter_map(|n| char::from_u32(n as u32)).collect();
            Value::text(&s)
        }
        BitShl => Value::Int(int(v(0)).unwrap_or(0).checked_shl(int(v(1)).unwrap_or(0).clamp(0, 64) as u32).unwrap_or(0)),
        BitShr => Value::Int(((int(v(0)).unwrap_or(0) as u64).checked_shr(int(v(1)).unwrap_or(0).clamp(0, 64) as u32).unwrap_or(0)) as i64),
        BitOr => Value::Int(int(v(0)).unwrap_or(0) | int(v(1)).unwrap_or(0)),
        BitNot => Value::Int(!int(v(0)).unwrap_or(0)),
        TimeOf => datetime_of(v(0)).map_or(Value::Null, |d| Value::text(&format_date(d, "%H:%i:%s"))),
        Conv => {
            let (from, to) = (int(v(1)).unwrap_or(10), int(v(2)).unwrap_or(10));
            let ok = |b: i64| (2..=36).contains(&b.abs());
            if !ok(from) || !ok(to) {
                return Ok(Value::Null);
            }
            let n = u64::from_str_radix(text(v(0)).trim(), from.unsigned_abs() as u32).unwrap_or(0);
            let digits = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
            let base = to.unsigned_abs();
            let mut x = n;
            let mut out = Vec::new();
            if x == 0 {
                out.push(b'0');
            }
            while x > 0 {
                out.push(digits[(x % base) as usize]);
                x /= base;
            }
            out.reverse();
            Value::text(&String::from_utf8_lossy(&out))
        }
        Strcmp => Value::Int(match text(v(0)).to_lowercase().cmp(&text(v(1)).to_lowercase()) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }),
        MakeTime => {
            let (h, m) = (int(v(0)).unwrap_or(0), int(v(1)).unwrap_or(0));
            let sec = text(v(2)).trim().parse::<f64>().unwrap_or(-1.0);
            if !(0..60).contains(&m) || !(0.0..60.0).contains(&sec) {
                return Ok(Value::Null);
            }
            let sign = if h < 0 { -1 } else { 1 };
            Value::text(&time_text(sign * (h.abs() * 3600 + m * 60 + sec as i64)))
        }
        AddTime | SubTime => {
            let (Some((a, a_is_time)), Some((b, true))) = (temporal(v(0)), temporal(v(1))) else { return Ok(Value::Null) };
            let secs = if f == AddTime { a + b } else { a - b };
            if a_is_time {
                Value::text(&time_text(secs))
            } else {
                Value::DateTime(secs)
            }
        }
        TimeDiff => match (temporal(v(0)), temporal(v(1))) {
            (Some((a, ka)), Some((b, kb))) if ka == kb => Value::text(&time_text(a - b)),
            _ => Value::Null,
        },
        TimestampDiff => {
            let (Some((a, _)), Some((b, _))) = (temporal(v(1)), temporal(v(2))) else { return Ok(Value::Null) };
            let d = b - a;
            let months = || -> i64 {
                let (ca, cb) = (crate::datetime::components(a), crate::datetime::components(b));
                let m = (cb.0 - ca.0) * 12 + (cb.1 as i64 - ca.1 as i64);
                // a whole month only when the day and time of the later one have come round
                let (ra, rb) = ((ca.2, ca.3, ca.4, ca.5), (cb.2, cb.3, cb.4, cb.5));
                if m > 0 && rb < ra {
                    m - 1
                } else if m < 0 && rb > ra {
                    m + 1
                } else {
                    m
                }
            };
            Value::Int(match text(v(0)).to_ascii_lowercase().as_str() {
                "second" => d,
                "minute" => d / 60,
                "hour" => d / 3600,
                "day" => d / 86_400,
                "week" => d / (7 * 86_400),
                "month" => months(),
                "quarter" => months() / 3,
                "year" => months() / 12,
                _ => return Ok(Value::Null),
            })
        }
        ConvertTz => {
            let Some((local, false)) = temporal(v(0)) else { return Ok(Value::Null) };
            let (Some(from), Some(to)) = (crate::tz::Zone::parse(&text(v(1))), crate::tz::Zone::parse(&text(v(2)))) else { return Ok(Value::Null) };
            Value::DateTime(to.to_local(from.to_utc(local)))
        }
        BitCount => Value::Int(int(v(0)).map_or(0, |n| (n as u64).count_ones() as i64)),
        ToBase64 => Value::text(&to_base64(text(v(0)).as_bytes())),
        FromBase64 => match from_base64(&text(v(0))) {
            Some(bytes) => Value::text(&String::from_utf8_lossy(&bytes)),
            None => Value::Null,
        },
        LoadFile => match std::fs::read(text(v(0))) {
            Ok(bytes) => Value::text(&String::from_utf8_lossy(&bytes)),
            Err(_) => Value::Null,
        },
        Bin => Value::text(&format!("{:b}", int(v(0)).unwrap_or(0) as u64)),
        Oct => Value::text(&format!("{:o}", int(v(0)).unwrap_or(0) as u64)),
    })
}

/// A time of day as seconds, `[-][D ]hh:mm:ss[.ffffff]` (the hours may pass 24, as MySQL's TIME does); a fraction is dropped.
fn time_secs(s: &str) -> Option<i64> {
    let t = s.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    let (days, t) = match t.split_once(' ') {
        Some((d, rest)) if !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()) => (d.parse::<i64>().ok()?, rest),
        _ => (0, t),
    };
    let mut parts = t.split('.').next()?.split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next().map_or(Some(0), |x| x.parse().ok())?;
    let sec: i64 = parts.next().map_or(Some(0), |x| x.parse().ok())?;
    if parts.next().is_some() || !(0..60).contains(&m) || !(0..60).contains(&sec) {
        return None;
    }
    let total = (days * 24 + h) * 3600 + m * 60 + sec;
    Some(if neg { -total } else { total })
}

/// Seconds as `hh:mm:ss` (more than two digits of hours when needed), with a sign.
fn time_text(secs: i64) -> String {
    let a = secs.abs();
    format!("{}{:02}:{:02}:{:02}", if secs < 0 { "-" } else { "" }, a / 3600, a % 3600 / 60, a % 60)
}

/// The seconds a value stands for and whether it is a time of day (true) or a date and time (false).
fn temporal(v: &Value) -> Option<(i64, bool)> {
    match v {
        Value::DateTime(t) | Value::TimestampTz(t) => Some((*t, false)),
        Value::Text(s) => time_secs(s).map(|t| (t, true)).or_else(|| crate::datetime::parse(s).map(|t| (t, false))),
        _ => None,
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn to_base64(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The bytes of base64 text (white space is skipped); `None` when it is not valid base64.
fn from_base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for ch in s.bytes().filter(|b| !b.is_ascii_whitespace()).take_while(|&b| b != b'=') {
        let v = B64.iter().position(|&b| b == ch)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits & 0xff) as u8);
        }
    }
    Some(out)
}
