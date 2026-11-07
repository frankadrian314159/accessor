//! One client connection: startup, authentication, then the simple and extended query protocols.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use accessor_core::{Connection, Error, QueryResult, Row, Value};

use crate::auth::{self, AuthMethod, Scram};
use crate::sql;
use crate::types;
use crate::wire::{self, Body, Msg};
use crate::Shared;

/// A prepared statement: the text, the parameter types the client declared (0 for any), and how many parameters it uses.
struct Statement {
    sql: String,
    declared: Vec<u32>,
    count: usize,
}

/// A statement with its parameters filled in. It runs when its rows are first needed, and the result is kept.
struct Portal {
    sql: String,
    result_formats: Vec<i16>,
    result: Option<QueryResult>,
    sent: usize,
}

struct Session<'a> {
    conn: Connection,
    out: BufWriter<TcpStream>,
    shared: &'a Shared,
    cancel: Arc<AtomicBool>,
    statements: HashMap<String, Statement>,
    portals: HashMap<String, Portal>,
}

/// Why a message could not be handled: a protocol or connection failure ends the session; an engine error is reported
/// to the client and the session goes on.
enum Failure {
    Io(io::Error),
    Sql(Error),
    Message(String, &'static str),
}

impl From<io::Error> for Failure {
    fn from(e: io::Error) -> Failure {
        Failure::Io(e)
    }
}
impl From<Error> for Failure {
    fn from(e: Error) -> Failure {
        Failure::Sql(e)
    }
}

type Done = Result<(), Failure>;

fn fatal(out: &mut impl Write, code: &str, message: &str) -> io::Result<()> {
    wire::error_fields(b'E', "FATAL", code, message).send(out)?;
    out.flush()
}

pub fn serve(stream: TcpStream, shared: &Shared, pid: i32) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = BufWriter::new(stream);

    // startup: SSL and GSS requests are declined (the client then goes on in the clear), a cancel request ends here
    let params = loop {
        let Some((code, body)) = wire::read_startup(&mut reader)? else { return Ok(()) };
        match code {
            wire::SSL_REQUEST | wire::GSS_REQUEST => {
                out.write_all(b"N")?;
                out.flush()?;
            }
            wire::CANCEL_REQUEST => {
                if body.len() >= 8 {
                    let key = (i32::from_be_bytes([body[0], body[1], body[2], body[3]]), i32::from_be_bytes([body[4], body[5], body[6], body[7]]));
                    if let Some(flag) = shared.cancels.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
                        flag.store(true, Ordering::SeqCst);
                    }
                }
                return Ok(());
            }
            wire::PROTOCOL_3 => {
                let mut b = Body::new(&body);
                let mut map = HashMap::new();
                loop {
                    let k = b.cstr()?;
                    if k.is_empty() {
                        break;
                    }
                    map.insert(k, b.cstr()?);
                }
                break map;
            }
            other => {
                return fatal(&mut out, "0A000", &format!("unsupported frontend protocol {}.{}", other >> 16, other & 0xFFFF));
            }
        }
    };
    // a client may choose its dialect when it connects: a `dialect` parameter, or `options=-c dialect=mysql`
    let requested = params.get("dialect").cloned().or_else(|| dialect_in_options(params.get("options").map(String::as_str).unwrap_or("")));
    let dialect = match requested.as_deref().map(|d| (d, accessor_core::dialect::Dialect::parse(d))) {
        Some((d, None)) => return fatal(&mut out, "22023", &format!("invalid value for parameter \"dialect\": \"{d}\" (asql, postgresql, mysql or tsql)")),
        Some((_, Some(d))) => Some(d),
        None => None,
    };
    let user = params.get("user").cloned().unwrap_or_default();
    if user.is_empty() {
        return fatal(&mut out, "28000", "no PostgreSQL user name specified in startup packet");
    }

    if shared.active.load(Ordering::SeqCst) > shared.config.max_connections {
        return fatal(&mut out, "53300", "sorry, too many clients already");
    }
    if !authenticate(&mut reader, &mut out, shared, &user)? {
        return Ok(());
    }

    Msg::new(b'R').i32(0).send(&mut out)?;
    for (k, v) in [
        ("server_version", shared.config.server_version.as_str()),
        ("server_encoding", "UTF8"),
        ("client_encoding", "UTF8"),
        ("DateStyle", "ISO, MDY"),
        ("integer_datetimes", "on"),
        ("standard_conforming_strings", "on"),
        ("TimeZone", "UTC"),
        ("IntervalStyle", "postgres"),
        ("is_superuser", "on"),
        ("session_authorization", user.as_str()),
    ] {
        Msg::new(b'S').cstr(k).cstr(v).send(&mut out)?;
    }
    let secret = i32::from_be_bytes(auth::random_bytes::<4>());
    let cancel = Arc::new(AtomicBool::new(false));
    shared.cancels.lock().unwrap_or_else(|e| e.into_inner()).insert((pid, secret), cancel.clone());
    Msg::new(b'K').i32(pid).i32(secret).send(&mut out)?;

    let mut session = Session { conn: shared.db.connect(), out, shared, cancel, statements: HashMap::new(), portals: HashMap::new() };
    if let Some(d) = dialect {
        if let Err(e) = session.conn.execute(&format!("SET dialect = '{}'", d.name())) {
            return fatal(&mut session.out, "XX000", &e.to_string());
        }
    }
    session.ready()?;
    let result = session.run(&mut reader);
    shared.cancels.lock().unwrap_or_else(|e| e.into_inner()).remove(&(pid, secret));
    result
}

/// The `dialect=name` in a `-c dialect=name` / `--dialect=name` list of startup options.
fn dialect_in_options(options: &str) -> Option<String> {
    let mut words = options.split_whitespace().peekable();
    while let Some(w) = words.next() {
        let setting = match w {
            "-c" | "--set" => words.next()?,
            other => other.strip_prefix("-c").or_else(|| other.strip_prefix("--"))?,
        };
        let (name, value) = setting.split_once('=')?;
        if name.eq_ignore_ascii_case("dialect") || name.eq_ignore_ascii_case("accessor.dialect") {
            return Some(value.to_string());
        }
    }
    None
}

fn authenticate(reader: &mut impl Read, out: &mut impl Write, shared: &Shared, user: &str) -> io::Result<bool> {
    let method = shared.config.auth;
    if method == AuthMethod::Trust {
        return Ok(true);
    }
    let password = shared.config.users.iter().find(|(u, _)| u == user).map(|(_, p)| p.clone());
    let denied = |out: &mut dyn Write| -> io::Result<bool> {
        wire::error_fields(b'E', "FATAL", "28P01", &format!("password authentication failed for user \"{user}\"")).send(out)?;
        out.flush()?;
        Ok(false)
    };
    // the next 'p' message
    let mut password_message = |reader: &mut dyn Read| -> io::Result<Option<Vec<u8>>> {
        match wire::read_message(reader)? {
            Some((b'p', body)) => Ok(Some(body)),
            _ => Ok(None),
        }
    };
    match method {
        AuthMethod::Trust => Ok(true),
        AuthMethod::Cleartext => {
            Msg::new(b'R').i32(3).send(out)?;
            out.flush()?;
            let Some(body) = password_message(reader)? else { return Ok(false) };
            let given = Body::new(&body).cstr()?;
            if password.as_deref() == Some(given.as_str()) { Ok(true) } else { denied(out) }
        }
        AuthMethod::Md5 => {
            let salt = auth::random_bytes::<4>();
            Msg::new(b'R').i32(5).bytes(&salt).send(out)?;
            out.flush()?;
            let Some(body) = password_message(reader)? else { return Ok(false) };
            let given = Body::new(&body).cstr()?;
            match password {
                Some(p) if auth::md5_response(user, &p, &salt) == given => Ok(true),
                _ => denied(out),
            }
        }
        AuthMethod::Scram => {
            Msg::new(b'R').i32(10).cstr("SCRAM-SHA-256").byte(0).send(out)?;
            out.flush()?;
            let Some(body) = password_message(reader)? else { return Ok(false) };
            let mut b = Body::new(&body);
            let mechanism = b.cstr()?;
            let len = b.i32()?;
            let first = if len >= 0 { String::from_utf8_lossy(b.take(len as usize)?).into_owned() } else { String::new() };
            // an unknown user is run through the exchange with a throwaway password, so the answer does not tell users apart
            let known = password.is_some();
            let Some((scram, server_first)) = (mechanism == "SCRAM-SHA-256").then(|| Scram::start(password.as_deref().unwrap_or("\u{1}no such user"), &first)).flatten() else {
                return denied(out);
            };
            Msg::new(b'R').i32(11).bytes(server_first.as_bytes()).send(out)?;
            out.flush()?;
            let Some(body) = password_message(reader)? else { return Ok(false) };
            let client_final = String::from_utf8_lossy(&body).into_owned();
            match scram.finish(&client_final) {
                Some(server_final) if known => {
                    Msg::new(b'R').i32(12).bytes(server_final.as_bytes()).send(out)?;
                    Ok(true)
                }
                _ => denied(out),
            }
        }
    }
}

/// Reads the next message, waking up now and then to see whether the server is stopping.
fn next_message(reader: &mut BufReader<TcpStream>, stop: &AtomicBool) -> io::Result<Option<(u8, Vec<u8>)>> {
    reader.get_ref().set_read_timeout(Some(Duration::from_millis(250)))?;
    loop {
        if reader.buffer().is_empty() {
            match reader.fill_buf() {
                Ok([]) => return Ok(None),
                Ok(_) => {}
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    if stop.load(Ordering::SeqCst) {
                        return Ok(None);
                    }
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        break;
    }
    reader.get_ref().set_read_timeout(None)?;
    wire::read_message(reader)
}

impl Session<'_> {
    fn ready(&mut self) -> io::Result<()> {
        let status = if self.conn.in_transaction() { b'T' } else { b'I' };
        Msg::new(b'Z').byte(status).send(&mut self.out)?;
        self.out.flush()
    }

    /// What `PRINT` said, as notices.
    fn send_messages(&mut self) -> io::Result<()> {
        for m in accessor_core::exec::take_messages() {
            wire::error_fields(b'N', "NOTICE", "00000", &m).send(&mut self.out)?;
        }
        Ok(())
    }

    fn report(&mut self, f: Failure) -> io::Result<()> {
        match f {
            Failure::Io(e) => Err(e),
            Failure::Sql(e) => wire::error_fields(b'E', "ERROR", &sql::sqlstate(&e), &e.to_string()).send(&mut self.out),
            Failure::Message(m, code) => wire::error_fields(b'E', "ERROR", code, &m).send(&mut self.out),
        }
    }

    fn run(&mut self, reader: &mut BufReader<TcpStream>) -> io::Result<()> {
        // after an error in the extended protocol, messages are skipped up to the next Sync
        let mut skipping = false;
        loop {
            let Some((tag, body)) = next_message(reader, &self.shared.stop)? else { return Ok(()) };
            if tag == b'X' {
                return Ok(());
            }
            if skipping && tag != b'S' {
                continue;
            }
            let mut b = Body::new(&body);
            let outcome = match tag {
                b'Q' => {
                    let text = b.cstr()?;
                    let r = self.simple_query(&text);
                    if let Err(f) = r {
                        self.report(f)?;
                    }
                    self.ready()?;
                    continue;
                }
                b'S' => {
                    skipping = false;
                    self.ready()?;
                    continue;
                }
                b'H' => {
                    self.out.flush()?;
                    continue;
                }
                b'P' => self.parse(&mut b),
                b'B' => self.bind(&mut b),
                b'D' => self.describe(&mut b),
                b'E' => self.execute(&mut b),
                b'C' => self.close(&mut b),
                b'd' | b'c' | b'f' => Err(Failure::Message("COPY is not supported".into(), "0A000")),
                other => Err(Failure::Message(format!("invalid frontend message type {}", other as char), "08P01")),
            };
            if let Err(f) = outcome {
                self.report(f)?;
                skipping = true;
            }
        }
    }

    // ----- the simple protocol -------------------------------------------------------------------------------------

    fn simple_query(&mut self, text: &str) -> Done {
        // a T-SQL message is one batch, which has no `;` between its statements: the engine reads it as a whole
        let tsql = self.conn.dialect() == accessor_core::dialect::Dialect::Tsql;
        let batches: Vec<String> = if tsql { sql::split_batches(text) } else { Vec::new() };
        let statements: Vec<&str> = if tsql { batches.iter().map(String::as_str).collect() } else { sql::split_statements(text) };
        if statements.is_empty() {
            Msg::new(b'I').send(&mut self.out)?;
            return Ok(());
        }
        for stmt in statements {
            if self.cancel.swap(false, Ordering::SeqCst) {
                return Err(Failure::Message("canceling statement due to user request".into(), "57014"));
            }
            let result = self.conn.execute(stmt);
            self.send_messages()?;
            let result = result?;
            let formats: [i16; 0] = [];
            self.send_result(stmt, &result, &formats, true, 0, 0)?;
        }
        Ok(())
    }

    /// Sends a result: the row description (if asked), the rows `from..` (up to `max` of them, 0 for all) and the completion.
    /// Gives the number of the next row.
    fn send_result(&mut self, sql_text: &str, r: &QueryResult, formats: &[i16], describe: bool, from: usize, max: usize) -> Result<usize, Failure> {
        let has_columns = !r.columns.is_empty();
        let oids = types::column_oids(r.columns.len(), &r.rows);
        if has_columns && describe {
            self.row_description(r, &oids, formats)?;
        }
        let end = if max == 0 { r.rows.len() } else { (from + max).min(r.rows.len()) };
        for row in &r.rows[from.min(r.rows.len())..end] {
            self.data_row(row, &oids, formats)?;
        }
        if end < r.rows.len() {
            Msg::new(b's').send(&mut self.out)?;
        } else {
            Msg::new(b'C').cstr(&sql::command_tag(sql_text, r.rows.len(), r.affected, has_columns)).send(&mut self.out)?;
        }
        Ok(end)
    }

    fn format_of(formats: &[i16], column: usize) -> i16 {
        match formats.len() {
            0 => 0,
            1 => formats[0],
            _ => formats.get(column).copied().unwrap_or(0),
        }
    }

    fn row_description(&mut self, r: &QueryResult, oids: &[u32], formats: &[i16]) -> io::Result<()> {
        let mut m = Msg::new(b'T').i16(r.columns.len() as i16);
        for (i, name) in r.columns.iter().enumerate() {
            m = m.cstr(name).i32(0).i16(0).i32(oids[i] as i32).i16(types::type_size(oids[i])).i32(-1).i16(Self::format_of(formats, i));
        }
        m.send(&mut self.out)
    }

    fn data_row(&mut self, row: &Row, oids: &[u32], formats: &[i16]) -> io::Result<()> {
        let mut m = Msg::new(b'D').i16(oids.len() as i16);
        for (i, oid) in oids.iter().enumerate() {
            let v = row.get(i).unwrap_or(&Value::Null);
            if v.is_null() {
                m = m.value(None);
            } else if Self::format_of(formats, i) == 1 {
                m = m.value(Some(&types::binary_of(*oid, v)));
            } else {
                m = m.value(Some(types::text_of(v).as_bytes()));
            }
        }
        m.send(&mut self.out)
    }

    // ----- the extended protocol -----------------------------------------------------------------------------------

    fn parse(&mut self, b: &mut Body) -> Done {
        let name = b.cstr()?;
        let text = b.cstr()?;
        let n = b.i16()?.max(0) as usize;
        let mut declared = Vec::with_capacity(n);
        for _ in 0..n {
            declared.push(b.i32()? as u32);
        }
        let statements = sql::split_statements(&text);
        if statements.len() > 1 {
            return Err(Failure::Message("cannot insert multiple commands into a prepared statement".into(), "42601"));
        }
        let text = statements.first().map(|s| s.to_string()).unwrap_or_default();
        let count = sql::parameter_count(&text).max(declared.len());
        // a statement that does not parse is refused now, with parameters stood in for by NULL
        if !text.is_empty() {
            accessor_core::sql::parse_script(&sql::substitute_parameters(&text, |_| "NULL".into()))?;
        }
        self.statements.insert(name, Statement { sql: text, declared, count });
        Msg::new(b'1').send(&mut self.out)?;
        Ok(())
    }

    /// The parameter types announced: those declared, text for the rest.
    fn parameter_oids(s: &Statement) -> Vec<u32> {
        (0..s.count).map(|i| s.declared.get(i).copied().filter(|o| *o != 0).unwrap_or(types::TEXT)).collect()
    }

    fn bind(&mut self, b: &mut Body) -> Done {
        let portal = b.cstr()?;
        let statement = b.cstr()?;
        let stmt = self.statements.get(&statement).ok_or_else(|| Failure::Message(format!("prepared statement \"{statement}\" does not exist"), "26000"))?;
        let nformats = b.i16()?.max(0) as usize;
        let mut formats = Vec::with_capacity(nformats);
        for _ in 0..nformats {
            formats.push(b.i16()?);
        }
        let nparams = b.i16()?.max(0) as usize;
        if nparams != stmt.count {
            return Err(Failure::Message(format!("bind message supplies {nparams} parameters, but prepared statement \"{statement}\" requires {}", stmt.count), "08P01"));
        }
        let oids = Self::parameter_oids(stmt);
        let mut literals = Vec::with_capacity(nparams);
        for i in 0..nparams {
            let format = match formats.len() {
                0 => 0,
                1 => formats[0],
                _ => formats.get(i).copied().unwrap_or(0),
            };
            literals.push(match b.value()? {
                None => "NULL".to_string(),
                Some(bytes) => {
                    let (oid, text) = if format == 1 {
                        let t = types::binary_param_text(oids[i], bytes).ok_or_else(|| Failure::Message(format!("binary parameter of type {} is not supported", oids[i]), "22P03"))?;
                        (oids[i], t)
                    } else {
                        (oids[i], String::from_utf8_lossy(bytes).into_owned())
                    };
                    if types::is_bare(oid) && text.parse::<f64>().is_ok() || matches!(text.as_str(), "true" | "false") && oid == types::BOOL {
                        text
                    } else {
                        sql::quote(&text)
                    }
                }
            });
        }
        let nresults = b.i16()?.max(0) as usize;
        let mut result_formats = Vec::with_capacity(nresults);
        for _ in 0..nresults {
            result_formats.push(b.i16()?);
        }
        let text = sql::substitute_parameters(&stmt.sql, |n| literals.get(n.wrapping_sub(1)).cloned().unwrap_or_else(|| "NULL".into()));
        self.portals.insert(portal, Portal { sql: text, result_formats, result: None, sent: 0 });
        Msg::new(b'2').send(&mut self.out)?;
        Ok(())
    }

    /// Runs the portal's statement if it has not been run.
    fn run_portal(&mut self, name: &str) -> Result<(), Failure> {
        let p = self.portals.get(name).ok_or_else(|| Failure::Message(format!("portal \"{name}\" does not exist"), "34000"))?;
        if p.result.is_none() {
            let text = p.sql.clone();
            let r = if text.trim().is_empty() { Ok(QueryResult::default()) } else { self.conn.execute(&text) };
            self.send_messages()?;
            let r = r?;
            if let Some(p) = self.portals.get_mut(name) {
                p.result = Some(r);
            }
        }
        Ok(())
    }

    fn describe(&mut self, b: &mut Body) -> Done {
        let kind = b.byte()?;
        let name = b.cstr()?;
        if kind == b'S' {
            let s = self.statements.get(&name).ok_or_else(|| Failure::Message(format!("prepared statement \"{name}\" does not exist"), "26000"))?;
            let oids = Self::parameter_oids(s);
            let text = s.sql.clone();
            let mut m = Msg::new(b't').i16(oids.len() as i16);
            for o in &oids {
                m = m.i32(*o as i32);
            }
            m.send(&mut self.out)?;
            // the columns are those of the statement run with NULL for every parameter, when it only reads
            if sql::is_query(&text) {
                let dry = sql::substitute_parameters(&text, |_| "NULL".into());
                if let Ok(r) = self.conn.execute(&dry) {
                    if !r.columns.is_empty() {
                        let oids = types::column_oids(r.columns.len(), &r.rows);
                        self.row_description(&r, &oids, &[])?;
                        return Ok(());
                    }
                }
            }
            Msg::new(b'n').send(&mut self.out)?;
            return Ok(());
        }
        self.run_portal(&name)?;
        let p = &self.portals[&name];
        let (r, formats) = (p.result.clone().unwrap_or_default(), p.result_formats.clone());
        if r.columns.is_empty() {
            Msg::new(b'n').send(&mut self.out)?;
        } else {
            let oids = types::column_oids(r.columns.len(), &r.rows);
            self.row_description(&r, &oids, &formats)?;
        }
        Ok(())
    }

    fn execute(&mut self, b: &mut Body) -> Done {
        let name = b.cstr()?;
        let max = b.i32()?.max(0) as usize;
        if self.cancel.swap(false, Ordering::SeqCst) {
            return Err(Failure::Message("canceling statement due to user request".into(), "57014"));
        }
        if self.portals.get(&name).is_some_and(|p| p.sql.trim().is_empty()) {
            Msg::new(b'I').send(&mut self.out)?;
            return Ok(());
        }
        self.run_portal(&name)?;
        let p = self.portals.get_mut(&name).ok_or_else(|| Failure::Message(format!("portal \"{name}\" does not exist"), "34000"))?;
        let (r, formats, from, text) = (p.result.take().unwrap_or_default(), p.result_formats.clone(), p.sent, p.sql.clone());
        let sent = self.send_result(&text, &r, &formats, false, from, max);
        if let Some(p) = self.portals.get_mut(&name) {
            p.result = Some(r);
            if let Ok(n) = &sent {
                p.sent = *n;
            }
        }
        sent.map(|_| ())
    }

    fn close(&mut self, b: &mut Body) -> Done {
        let kind = b.byte()?;
        let name = b.cstr()?;
        if kind == b'S' {
            self.statements.remove(&name);
        } else {
            self.portals.remove(&name);
        }
        Msg::new(b'3').send(&mut self.out)?;
        Ok(())
    }
}
