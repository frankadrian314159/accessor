//! Runs PostgreSQL's regression tests (`src/test/regress`) against Accessor.
//!
//!   accessor-pgreg --pg PATH-TO/src/test/regress [--report FILE] [--stall SECONDS] [--timeout SECONDS] [--memory-mb N] [--only a,b,c]
//!
//! PostgreSQL's tests compare the whole text `psql` prints, which no other engine can reproduce, so this does
//! the next best thing: every SQL statement of every test is run, in the order of `parallel_schedule` and in
//! one database (as the real run has it), and what Accessor did is compared with what PostgreSQL did, which
//! the expected output (`expected/NAME.out`) records:
//!
//! * `ok`      both accepted the statement (and, for a query whose result the expected output shows, the rows
//!             agree: compared as sets, after the usual spelling differences such as `t`/`f`);
//! * `error`   both refused it;
//! * `missing` PostgreSQL accepted it and Accessor refused it: a missing feature, or a difference;
//! * `lenient` PostgreSQL refused it and Accessor accepted it;
//! * `value`   both accepted it, with different rows.
//!
//! `psql` meta-commands (`\d`, `\set`...) are not statements and are skipped, except that `\set`, `\getenv` and
//! `:'name'` are understood well enough to load the test data with `COPY`.
//!
//! A statement that exhausts memory or never ends is found and skipped: the tests run in a child process with a
//! hard memory limit (a Windows job object) and a time limit, restarted without the statement.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use accessor_core::{Database, Value};

// ----- reading the SQL files ---------------------------------------------------------------------------

enum Item {
    Sql { text: String, line: usize },
    /// A `\command` of psql (the text after the backslash).
    Meta { text: String, line: usize },
    /// The data lines of `COPY ... FROM stdin`.
    CopyData { rows: Vec<String> },
}

/// Splits a `psql` script into statements and meta-commands.
fn split_sql(src: &str) -> Vec<Item> {
    let b: Vec<char> = src.chars().collect();
    let mut items = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let mut start: Option<(usize, usize)> = None; // (char index, line) of the first character of the statement
    let at_line_start = |i: usize| i == 0 || b[i - 1] == '\n';
    while i < b.len() {
        let c = b[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '-' && b.get(i + 1) == Some(&'-') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            let mut depth = 0;
            while i < b.len() {
                if b[i] == '/' && b.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == '*' && b.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if b[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
        } else if c == '\\' && (start.is_none() || at_line_start(i)) {
            // a psql command, to the end of the line; one after a statement (\g, \gset) ends it
            let mut j = i;
            while j < b.len() && b[j] != '\n' {
                j += 1;
            }
            let text: String = b[i + 1..j].iter().collect();
            if let Some((s, l)) = start.take() {
                let stmt: String = b[s..i].iter().collect::<String>().trim_end().to_string();
                items.push(Item::Sql { text: stmt, line: l });
            }
            items.push(Item::Meta { text, line });
            i = j;
        } else {
            if start.is_none() {
                start = Some((i, line));
            }
            match c {
                '\'' => {
                    // E'...' strings take backslash escapes
                    let escape = i > 0 && (b[i - 1] == 'E' || b[i - 1] == 'e') && (i < 2 || !(b[i - 2].is_alphanumeric() || b[i - 2] == '_'));
                    i += 1;
                    while i < b.len() {
                        if b[i] == '\n' {
                            line += 1;
                        }
                        if escape && b[i] == '\\' {
                            i += 2;
                            continue;
                        }
                        if b[i] == '\'' {
                            if b.get(i + 1) == Some(&'\'') {
                                i += 2;
                                continue;
                            }
                            break;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                '"' => {
                    i += 1;
                    while i < b.len() && !(b[i] == '"' && b.get(i + 1) != Some(&'"')) {
                        if b[i] == '"' {
                            i += 1;
                        }
                        if i < b.len() && b[i] == '\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                '$' => {
                    // $$...$$ or $tag$...$tag$
                    let mut j = i + 1;
                    while j < b.len() && (b[j].is_alphanumeric() || b[j] == '_') {
                        j += 1;
                    }
                    if b.get(j) == Some(&'$') && !(i > 0 && (b[i - 1].is_alphanumeric() || b[i - 1] == '_')) {
                        let tag: Vec<char> = b[i..=j].to_vec();
                        i = j + 1;
                        while i < b.len() && !b[i..].starts_with(&tag) {
                            if b[i] == '\n' {
                                line += 1;
                            }
                            i += 1;
                        }
                        i = (i + tag.len()).min(b.len());
                    } else {
                        i += 1;
                    }
                }
                ';' => {
                    let (s, l) = start.take().expect("a statement has started");
                    let text: String = b[s..=i].iter().collect();
                    i += 1;
                    let lower = text.to_ascii_lowercase();
                    let copy_stdin = lower.trim_start().starts_with("copy") && lower.contains("from stdin");
                    items.push(Item::Sql { text, line: l });
                    if copy_stdin {
                        // data lines up to `\.`
                        while i < b.len() && b[i] != '\n' {
                            i += 1;
                        }
                        let mut rows = Vec::new();
                        loop {
                            i += 1;
                            line += 1;
                            if i >= b.len() {
                                break;
                            }
                            let mut j = i;
                            while j < b.len() && b[j] != '\n' {
                                j += 1;
                            }
                            let l: String = b[i..j].iter().collect();
                            i = j;
                            if l.trim_end() == "\\." {
                                break;
                            }
                            rows.push(l);
                        }
                        items.push(Item::CopyData { rows });
                    }
                }
                _ => i += 1,
            }
        }
    }
    if let Some((s, l)) = start {
        let text: String = b[s..].iter().collect::<String>().trim_end().to_string();
        if !text.is_empty() {
            items.push(Item::Sql { text, line: l });
        }
    }
    items
}

// ----- reading the expected output ---------------------------------------------------------------------

/// What PostgreSQL printed after one statement.
#[derive(Default, Clone)]
struct Expected {
    aligned: bool,
    error: Option<String>,
    /// The rows of a result table, as the cells psql printed.
    table: Option<Vec<Vec<String>>>,
}

fn parse_expected(segment: &str) -> Expected {
    let mut e = Expected { aligned: true, ..Default::default() };
    let lines: Vec<&str> = segment.lines().collect();
    for l in &lines {
        if let Some(m) = l.strip_prefix("ERROR:") {
            e.error = Some(m.trim().to_string());
            break;
        }
    }
    let sep = lines.iter().position(|l| {
        let t = l.trim();
        t.len() >= 3 && t.contains('-') && t.chars().all(|c| c == '-' || c == '+')
    });
    if let (Some(s), None) = (sep, &e.error) {
        if s > 0 {
            let mut rows = Vec::new();
            for l in &lines[s + 1..] {
                let t = l.trim();
                if t.starts_with('(') && (t.ends_with(" rows)") || t.ends_with(" row)")) {
                    e.table = Some(rows);
                    break;
                }
                rows.push(l.split('|').map(|c| c.trim().to_string()).collect::<Vec<_>>());
            }
        }
    }
    e
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ----- running ---------------------------------------------------------------------------------------

fn cell(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => (if *b { "t" } else { "f" }).to_string(),
        Value::Text(s) => s.to_string(),
        other => other.to_string(),
    }
}

fn short(s: &str, n: usize) -> String {
    let one = normalize_ws(s);
    if one.chars().count() > n {
        format!("{}...", one.chars().take(n).collect::<String>())
    } else {
        one
    }
}

/// An error message with the particulars taken out.
fn class_of(msg: &str) -> String {
    let mut out = String::new();
    let mut in_quote = None;
    for c in msg.chars() {
        match (in_quote, c) {
            (None, '\'' | '"') => {
                in_quote = Some(c);
                out.push('\'');
            }
            (Some(q), c) if c == q => {
                in_quote = None;
                out.push('\'');
            }
            (Some(_), _) => {}
            (None, c) if c.is_ascii_digit() => {
                if !out.ends_with('#') {
                    out.push('#');
                }
            }
            (None, c) => out.push(c),
        }
    }
    short(&out, 100)
}

/// The first one or two words of a statement.
fn lead(sql: &str) -> String {
    let mut words = sql.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_ascii_uppercase()).filter(|w| !w.is_empty());
    let first = words.next().unwrap_or_default();
    match first.as_str() {
        "CREATE" | "ALTER" | "DROP" => {
            let mut rest: Vec<String> = words.take(3).collect();
            rest.retain(|w| !matches!(w.as_str(), "OR" | "REPLACE" | "TEMP" | "TEMPORARY" | "UNIQUE" | "UNLOGGED" | "IF" | "NOT" | "EXISTS" | "GLOBAL" | "LOCAL" | "RECURSIVE" | "MATERIALIZED"));
            format!("{first} {}", rest.first().cloned().unwrap_or_default())
        }
        _ => first,
    }
}

struct Ctx {
    db: Database,
    vars: HashMap<String, String>,
    pg: PathBuf,
    results: String,
    progress: Option<PathBuf>,
    skip: Vec<(String, usize)>,
}

fn interpolate(ctx: &Ctx, sql: &str) -> String {
    // :'name' (quoted literal) and :name
    let mut out = String::new();
    let b: Vec<char> = sql.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == ':' && i + 1 < b.len() && b.get(i.wrapping_sub(1)) != Some(&':') && b[i + 1] != ':' {
            let quoted = b[i + 1] == '\'';
            let s = if quoted { i + 2 } else { i + 1 };
            let mut e = s;
            while e < b.len() && (b[e].is_alphanumeric() || b[e] == '_') {
                e += 1;
            }
            let name: String = b[s..e].iter().collect();
            if !name.is_empty() && (!quoted || b.get(e) == Some(&'\'')) {
                if let Some(v) = ctx.vars.get(&name) {
                    if quoted {
                        let _ = write!(out, "'{}'", v.replace('\'', "''"));
                        i = e + 1;
                    } else {
                        out.push_str(v);
                        i = e;
                    }
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn unescape_copy(field: &str) -> Option<String> {
    if field == "\\N" {
        return None;
    }
    let mut out = String::new();
    let mut it = field.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// Loads the rows of a `COPY` into a table through ordinary inserts.
fn copy_rows(db: &mut Database, table: &str, columns: &str, rows: &[String]) -> Result<usize, String> {
    let mut done = 0;
    for chunk in rows.chunks(200) {
        let mut sql = format!("INSERT INTO {table}{columns} VALUES ");
        let mut first = true;
        for r in chunk {
            if r.is_empty() {
                continue;
            }
            if !first {
                sql.push_str(", ");
            }
            first = false;
            sql.push('(');
            for (k, f) in r.split('\t').enumerate() {
                if k > 0 {
                    sql.push_str(", ");
                }
                match unescape_copy(f) {
                    None => sql.push_str("NULL"),
                    Some(v) => {
                        let _ = write!(sql, "'{}'", v.replace('\'', "''"));
                    }
                }
            }
            sql.push(')');
        }
        if first {
            continue;
        }
        db.execute(&sql).map_err(|e| e.to_string())?;
        done += chunk.len();
    }
    Ok(done)
}

fn parse_copy(sql: &str) -> Option<(String, String, Option<String>)> {
    // COPY table [(cols)] FROM 'file' | stdin
    let t = sql.trim().trim_end_matches(';');
    let lower = t.to_ascii_lowercase();
    let rest = t.get(4..)?.trim_start();
    if !lower.starts_with("copy") {
        return None;
    }
    let from = lower.find(" from ")?;
    let head = t[4..from].trim();
    let (table, cols) = match head.find('(') {
        Some(p) => (head[..p].trim().to_string(), head[p..].trim().to_string()),
        None => (head.to_string(), String::new()),
    };
    let _ = rest;
    let source = t[from + 6..].trim();
    let file = source.strip_prefix('\'').and_then(|s| s.split('\'').next()).map(str::to_string);
    Some((table, if cols.is_empty() { cols } else { format!(" {cols}") }, file))
}

fn write_progress(ctx: &Ctx, test: &str, idx: usize) {
    if let Some(p) = &ctx.progress {
        let _ = std::fs::write(p, format!("{test}\t{idx}"));
    }
}

fn run_test(ctx: &mut Ctx, name: &str) {
    let sql_path = ctx.pg.join("sql").join(format!("{name}.sql"));
    let out_path = ctx.pg.join("expected").join(format!("{name}.out"));
    let (Ok(sql_bytes), Ok(out_bytes)) = (std::fs::read(&sql_path), std::fs::read(&out_path)) else { return };
    let sql_text = String::from_utf8_lossy(&sql_bytes).into_owned();
    let out_text = String::from_utf8_lossy(&out_bytes).into_owned().replace("\r\n", "\n");
    let items = split_sql(&sql_text.replace("\r\n", "\n"));

    // where each statement's output begins in the expected text
    let mut cursor = 0usize;
    let mut places: Vec<Option<(usize, usize)>> = Vec::new(); // (start, end) of the echoed statement
    for it in &items {
        match it {
            Item::Sql { text, .. } => {
                let t = text.trim();
                match out_text[cursor..].find(t) {
                    Some(p) => {
                        places.push(Some((cursor + p, cursor + p + t.len())));
                        cursor += p + t.len();
                    }
                    None => places.push(None),
                }
            }
            _ => places.push(None),
        }
    }
    // the output of statement k runs to the start of the next aligned statement
    let segment = |k: usize| -> Option<&str> {
        let (_, end) = places[k]?;
        let next = places[k + 1..].iter().flatten().next().map(|p| p.0).unwrap_or(out_text.len());
        Some(&out_text[end..next.max(end)])
    };

    let mut idx = 0usize;
    let mut pending_copy: Option<(String, String)> = None;
    for (k, it) in items.iter().enumerate() {
        match it {
            Item::Meta { text, line } => {
                let mut parts = text.split_whitespace();
                match parts.next() {
                    Some("set") => {
                        if let Some(var) = parts.next() {
                            // `\set a :b '/c'`: the words are joined, variables filled in, quotes dropped
                            let mut value = String::new();
                            for w in parts {
                                if w.starts_with(':') {
                                    value.push_str(&interpolate(ctx, w));
                                } else {
                                    value.push_str(w.trim_matches('\''));
                                }
                            }
                            let value = value.trim_matches('\'').to_string();
                            ctx.vars.insert(var.to_string(), value);
                        }
                    }
                    Some("getenv") => {
                        if let (Some(var), Some(env)) = (parts.next(), parts.next()) {
                            let v = match env {
                                "PG_ABS_SRCDIR" | "PG_ABS_BUILDDIR" => ctx.pg.display().to_string().replace('\\', "/"),
                                "PG_LIBDIR" => String::new(),
                                "PG_DLSUFFIX" => ".dll".into(),
                                _ => String::new(),
                            };
                            ctx.vars.insert(var.to_string(), v);
                        }
                    }
                    _ => {}
                }
                let _ = writeln!(ctx.results, "{name}\t{idx}\t{line}\tmeta\t\t\\{}", short(text, 60));
            }
            Item::CopyData { rows } => {
                if let Some((table, cols)) = pending_copy.take() {
                    let r = copy_rows(&mut ctx.db, &table, &cols, rows);
                    let _ = writeln!(ctx.results, "{name}\t{idx}\t0\t{}\t\tCOPY data into {table}: {}", if r.is_ok() { "ok" } else { "missing" }, r.err().map(|e| class_of(&e)).unwrap_or_default());
                }
            }
            Item::Sql { text, line } => {
                idx += 1;
                if ctx.skip.iter().any(|(t, i)| t == name && *i == idx) {
                    let _ = writeln!(ctx.results, "{name}\t{idx}\t{line}\tkiller\t\t{}", short(text, 160));
                    continue;
                }
                write_progress(ctx, name, idx);
                let sql = interpolate(ctx, text);
                let expected = segment(k).map(parse_expected).unwrap_or_default();
                let lead_word = lead(&sql);
                // COPY: loaded here, not by the engine
                if lead_word == "COPY" {
                    if let Some((table, cols, file)) = parse_copy(&sql) {
                        match file {
                            Some(f) => {
                                let data = std::fs::read(&f).map(|b| String::from_utf8_lossy(&b).into_owned());
                                let r = data.map_err(|e| e.to_string()).and_then(|d| {
                                    let rows: Vec<String> = d.lines().map(str::to_string).collect();
                                    copy_rows(&mut ctx.db, &table, &cols, &rows)
                                });
                                let _ = writeln!(ctx.results, "{name}\t{idx}\t{line}\t{}\tCOPY\t{}", if r.is_ok() { "ok" } else { "missing" }, r.err().map(|e| class_of(&e)).unwrap_or_default());
                            }
                            None => {
                                pending_copy = Some((table, cols));
                                let _ = writeln!(ctx.results, "{name}\t{idx}\t{line}\tok\tCOPY\tfrom stdin");
                            }
                        }
                        continue;
                    }
                }
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx.db.execute(&sql)));
                let (verdict, class, detail) = match (outcome, &expected.error) {
                    (Err(_), _) => ("panic".to_string(), "the engine panicked".to_string(), short(&sql, 200)),
                    (Ok(Ok(r)), None) => match &expected.table {
                        Some(want) => compare_rows(&sql, want, &r),
                        None => ("ok".into(), String::new(), String::new()),
                    },
                    (Ok(Ok(_)), Some(msg)) => ("lenient".into(), class_of(msg), short(&sql, 200)),
                    (Ok(Err(e)), None) => {
                        let m = e.to_string();
                        (if expected.aligned { "missing" } else { "unaligned" }.into(), class_of(&m), format!("{} -> {}", short(&sql, 200), short(&m, 160)))
                    }
                    (Ok(Err(_)), Some(_)) => ("error".into(), String::new(), String::new()),
                };
                let _ = writeln!(ctx.results, "{name}\t{idx}\t{line}\t{verdict}\t{lead_word}\t{class}\t{}", detail.replace('\t', " "));
            }
        }
    }
}

/// Settings the run starts with: PostgreSQL's casts of a float to an integer round, where Accessor's drop the fraction, and
/// its types that Accessor lacks (and arrays) are kept as text, so that the tables that use them can be made.
const POSTGRES_SETTINGS: &[&str] = &["SET cast_truncates = off", "SET lenient_types = on", "SET strict_types = on"];

fn compare_rows(sql: &str, want: &[Vec<String>], got: &accessor_core::QueryResult) -> (String, String, String) {
    let ordered = sql.to_ascii_lowercase().contains("order by");
    let mut g: Vec<Vec<String>> = got.rows.iter().map(|r| r.iter().map(cell).collect()).collect();
    let mut w: Vec<Vec<String>> = want.to_vec();
    if g.len() != w.len() {
        return ("value".into(), "different number of rows".into(), format!("{} -> got {} row(s), PostgreSQL {}", short(sql, 160), g.len(), w.len()));
    }
    if !ordered {
        g.sort();
        w.sort();
    }
    for (a, b) in g.iter().zip(&w) {
        if a.len() != b.len() {
            return ("value".into(), "different number of columns".into(), format!("{} -> got {} column(s), PostgreSQL {}", short(sql, 160), a.len(), b.len()));
        }
        if a != b {
            let k = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(0);
            return ("value".into(), "different values".into(), format!("{} -> got {:?}, PostgreSQL {:?}", short(sql, 160), a[k], b[k]));
        }
    }
    ("ok".into(), String::new(), "compared".into())
}

fn schedule(pg: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(pg.join("parallel_schedule")).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for l in text.lines() {
        if let Some(rest) = l.strip_prefix("test:") {
            for t in rest.split_whitespace() {
                if !out.iter().any(|x| x == t) {
                    out.push(t.to_string());
                }
            }
        }
    }
    out
}

fn child(args: &[String]) {
    let (mut pg, mut results, mut progress, mut skip, mut only) = (PathBuf::new(), PathBuf::new(), None, Vec::new(), Vec::<String>::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pg" => pg = it.next().map(PathBuf::from).unwrap_or_default(),
            "--results" => results = it.next().map(PathBuf::from).unwrap_or_default(),
            "--progress" => progress = it.next().map(PathBuf::from),
            "--skip" => {
                skip = it
                    .next()
                    .map(|v| v.split(',').filter_map(|p| p.split_once(':').and_then(|(t, i)| i.parse().ok().map(|i| (t.to_string(), i)))).collect())
                    .unwrap_or_default()
            }
            "--only" => only = it.next().map(|v| v.split(',').map(str::to_string).collect()).unwrap_or_default(),
            _ => {}
        }
    }
    std::panic::set_hook(Box::new(|_| {}));
    let mut db = Database::in_memory();
    db.set_dialect(accessor_core::dialect::Dialect::Postgresql);
    // PostgreSQL's reading of a few things Accessor does otherwise by default (docs/extern/testing.md)
    for st in POSTGRES_SETTINGS {
        let _ = db.execute(st);
    }
    let mut ctx = Ctx { db, vars: HashMap::new(), pg: pg.clone(), results: String::new(), progress, skip };
    for t in schedule(&pg) {
        if !only.is_empty() && !only.contains(&t) && t != "test_setup" {
            continue;
        }
        run_test(&mut ctx, &t);
        // keep what has been found if the process is killed later
        let _ = std::fs::write(&results, &ctx.results);
    }
    let _ = std::fs::write(&results, &ctx.results);
}

// ----- the parent ---------------------------------------------------------------------------------------

#[cfg(windows)]
fn limit_memory(child: &std::process::Child, megabytes: usize) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::*;
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        info.ProcessMemoryLimit = megabytes << 20;
        SetInformationJobObject(job, JobObjectExtendedLimitInformation, std::ptr::addr_of!(info).cast(), std::mem::size_of_val(&info) as u32);
        AssignProcessToJobObject(job, child.as_raw_handle().cast());
    }
}

#[cfg(not(windows))]
fn limit_memory(_child: &std::process::Child, _megabytes: usize) {}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--child") {
        child(&args[1..]);
        return;
    }
    let (mut pg, mut report, mut timeout, mut memory_mb, mut only, mut stall) = (PathBuf::new(), PathBuf::from("pg-report.md"), 1800u64, 2048usize, String::new(), 20u64);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pg" => pg = it.next().map(PathBuf::from).expect("--pg PATH"),
            "--report" => report = it.next().map(PathBuf::from).expect("--report FILE"),
            "--timeout" => timeout = it.next().and_then(|v| v.parse().ok()).expect("--timeout SECONDS"),
            "--memory-mb" => memory_mb = it.next().and_then(|v| v.parse().ok()).expect("--memory-mb N"),
            "--only" => only = it.next().cloned().expect("--only a,b"),
            "--stall" => stall = it.next().and_then(|v| v.parse().ok()).expect("--stall SECONDS"),
            _ => {}
        }
    }
    if !pg.join("parallel_schedule").exists() {
        eprintln!("usage: accessor-pgreg --pg PATH-TO/src/test/regress [--report FILE] [--stall SECONDS] [--timeout SECONDS] [--memory-mb N] [--only a,b,c]");
        std::process::exit(2);
    }
    let exe = std::env::current_exe().expect("own path");
    let scratch = std::env::temp_dir().join(format!("accessor-pgreg-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("temporary folder");
    let (results, progress) = (scratch.join("results"), scratch.join("progress"));
    let mut skip: Vec<(String, usize)> = Vec::new();
    let mut killers: Vec<(String, usize, String)> = Vec::new();
    let started = Instant::now();
    loop {
        let _ = std::fs::write(&progress, "");
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("--child").arg("--pg").arg(&pg).arg("--results").arg(&results).arg("--progress").arg(&progress);
        if !skip.is_empty() {
            cmd.arg("--skip").arg(skip.iter().map(|(t, i)| format!("{t}:{i}")).collect::<Vec<_>>().join(","));
        }
        if !only.is_empty() {
            cmd.arg("--only").arg(&only);
        }
        let mut child = cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().expect("child");
        limit_memory(&child, memory_mb);
        let deadline = Instant::now() + Duration::from_secs(timeout);
        // The progress file names the statement being run; if it stays the same for `stall` seconds, that
        // statement is stuck.
        let (mut last, mut changed) = (String::new(), Instant::now());
        let ok = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if Instant::now() > deadline || changed.elapsed() > Duration::from_secs(stall) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
                Ok(None) => {
                    std::thread::sleep(Duration::from_millis(50));
                    let now = std::fs::read_to_string(&progress).unwrap_or_default();
                    if now != last {
                        last = now;
                        changed = Instant::now();
                    }
                }
                Err(_) => break false,
            }
        };
        if ok {
            break;
        }
        let p = std::fs::read_to_string(&progress).unwrap_or_default();
        let Some((t, i)) = p.split_once('\t').and_then(|(t, i)| i.trim().parse::<usize>().ok().map(|i| (t.to_string(), i))) else {
            eprintln!("the child died before running anything");
            break;
        };
        eprintln!("KILLER {t} statement {i}: the process ended, ran past {timeout}s in all, or spent {stall}s on this statement (memory limit {memory_mb} MB)");
        killers.push((t.clone(), i, String::new()));
        if skip.contains(&(t.clone(), i)) || killers.len() > 60 {
            break;
        }
        skip.push((t, i));
    }
    let data = std::fs::read_to_string(&results).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&scratch);

    // ----- the report
    struct Row<'a> {
        test: &'a str,
        verdict: &'a str,
        lead: &'a str,
        class: &'a str,
        detail: &'a str,
    }
    let rows: Vec<Row> = data
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(7, '\t').collect();
            (f.len() >= 4).then(|| Row { test: f[0], verdict: f[3], lead: f.get(4).copied().unwrap_or(""), class: f.get(5).copied().unwrap_or(""), detail: f.get(6).copied().unwrap_or("") })
        })
        .collect();
    let mut verdicts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut per_test: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
    for r in &rows {
        *verdicts.entry(r.verdict).or_insert(0) += 1;
        *per_test.entry(r.test).or_default().entry(r.verdict).or_insert(0) += 1;
    }
    let statements: usize = rows.iter().filter(|r| r.verdict != "meta").count();
    let mut md = String::new();
    let _ = writeln!(md, "# PostgreSQL regression tests against Accessor\n");
    let _ = writeln!(md, "{} tests, {statements} statements (plus {} psql commands), run in {:.0}s.\n", per_test.len(), verdicts.get("meta").copied().unwrap_or(0), started.elapsed().as_secs_f64());
    let _ = writeln!(md, "| Outcome | Statements | Share |\n| --- | ---: | ---: |");
    for (v, label) in [("ok", "both accepted (rows agree where shown)"), ("error", "both refused"), ("missing", "PostgreSQL accepted, Accessor refused"), ("lenient", "PostgreSQL refused, Accessor accepted"), ("value", "both accepted, different rows"), ("unaligned", "could not match the expected output; Accessor refused"), ("panic", "the engine panicked"), ("killer", "killed the process (skipped)")] {
        if let Some(n) = verdicts.get(v) {
            let _ = writeln!(md, "| {label} | {n} | {:.1}% |", 100.0 * *n as f64 / statements.max(1) as f64);
        }
    }
    let compared = rows.iter().filter(|r| r.verdict == "ok" && r.detail == "compared").count();
    let wrong = verdicts.get("value").copied().unwrap_or(0);
    let cascade = rows.iter().filter(|r| r.verdict == "missing" && (r.class.contains("does not exist") || r.class.contains("already exists") || r.class.contains("was not found"))).count();
    let _ = writeln!(md, "
Result tables compared: {} ({} equal, {wrong} different). Of the refused statements, {cascade} were refused because an earlier statement that creates the object had failed (\"does not exist\", \"already exists\", \"was not found\").", compared + wrong, compared);
    let agree = verdicts.get("ok").copied().unwrap_or(0) + verdicts.get("error").copied().unwrap_or(0);
    let _ = writeln!(md, "\nAgreement on whether a statement is accepted: **{:.1}%** ({agree} of {statements}).\n", 100.0 * agree as f64 / statements.max(1) as f64);
    if !killers.is_empty() {
        let _ = writeln!(md, "## Statements that killed the process\n");
        for (t, i, _) in &killers {
            let _ = writeln!(md, "* `{t}` statement {i}");
        }
        let _ = writeln!(md);
    }
    // what Accessor refuses most
    let mut classes: HashMap<(&str, &str), (usize, &str)> = HashMap::new();
    for r in rows.iter().filter(|r| r.verdict == "missing") {
        let e = classes.entry((r.lead, r.class)).or_insert((0, r.detail));
        e.0 += 1;
    }
    let mut by_class: Vec<_> = classes.into_iter().collect();
    by_class.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let _ = writeln!(md, "## What Accessor refuses that PostgreSQL accepts: the 60 commonest errors\n");
    let _ = writeln!(md, "| Statements | Starting with | Error | Example |\n| ---: | --- | --- | --- |");
    for ((lead, class), (n, example)) in by_class.iter().take(60) {
        let _ = writeln!(md, "| {n} | {lead} | {} | `{}` |", class.replace('|', "\\|"), short(example, 150).replace('|', "\\|").replace('`', "'"));
    }
    let mut leads: HashMap<&str, usize> = HashMap::new();
    for r in rows.iter().filter(|r| r.verdict == "missing") {
        *leads.entry(r.lead).or_insert(0) += 1;
    }
    let mut leads: Vec<_> = leads.into_iter().collect();
    leads.sort_by(|a, b| b.1.cmp(&a.1));
    let _ = writeln!(md, "\n## Refused statements by kind\n");
    let _ = writeln!(md, "| Kind | Refused |\n| --- | ---: |");
    for (l, n) in leads.iter().take(30) {
        let _ = writeln!(md, "| {l} | {n} |");
    }
    let mut lenient: HashMap<&str, (usize, &str)> = HashMap::new();
    for r in rows.iter().filter(|r| r.verdict == "lenient") {
        let e = lenient.entry(r.class).or_insert((0, r.detail));
        e.0 += 1;
    }
    let mut lenient: Vec<_> = lenient.into_iter().collect();
    lenient.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let _ = writeln!(md, "\n## What Accessor accepts that PostgreSQL refuses (by PostgreSQL's message)\n");
    let _ = writeln!(md, "| Statements | PostgreSQL's error | Example |\n| ---: | --- | --- |");
    for (class, (n, example)) in lenient.iter().take(25) {
        let _ = writeln!(md, "| {n} | {} | `{}` |", class.replace('|', "\\|"), short(example, 140).replace('|', "\\|").replace('`', "'"));
    }
    let mut diffs: HashMap<&str, usize> = HashMap::new();
    for r in rows.iter().filter(|r| r.verdict == "value") {
        *diffs.entry(r.class).or_insert(0) += 1;
    }
    let _ = writeln!(md, "\n## Same statement, different rows\n");
    for (c, n) in &diffs {
        let _ = writeln!(md, "* {c}: {n}");
    }
    let mut examples: Vec<&Row> = rows.iter().filter(|r| r.verdict == "value").collect();
    examples.truncate(25);
    let _ = writeln!(md, "\nFirst examples:\n");
    for r in examples {
        let _ = writeln!(md, "* `{}`: {}", r.test, short(r.detail, 220).replace('`', "'"));
    }
    let _ = writeln!(md, "\n## By test\n");
    let _ = writeln!(md, "| Test | Statements | Both accepted | Both refused | Refused by Accessor | Accepted only by Accessor | Rows differ |\n| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
    for (t, v) in &per_test {
        let n: usize = v.iter().filter(|(k, _)| **k != "meta").map(|(_, n)| n).sum();
        if n == 0 {
            continue;
        }
        let g = |k: &str| v.get(k).copied().unwrap_or(0);
        let _ = writeln!(md, "| {t} | {n} | {} | {} | {} | {} | {} |", g("ok"), g("error"), g("missing") + g("unaligned"), g("lenient"), g("value"));
    }
    std::fs::write(&report, &md).expect("the report can be written");
    let _ = std::fs::write(report.with_extension("tsv"), &data);
    println!("{}", md.lines().take(24).collect::<Vec<_>>().join("\n"));
    println!("\n(full report: {})", report.display());
}
