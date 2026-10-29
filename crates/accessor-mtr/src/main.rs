//! Runs MySQL's test suite (`mysql-test/t` and `mysql-test/r`) against Accessor.
//!
//!   accessor-mtr --mysql PATH-TO/mysql-test [--report FILE] [--stall SECONDS] [--timeout SECONDS] [--memory-mb N] [--only a,b,c]
//!
//! A MySQL test is a script in the `mysqltest` language: SQL statements mixed with commands (`--error`, `let`, `while`,
//! `--source`, `--disable_query_log`, ...). Its expected output (`r/NAME.result`) records what the server printed, with
//! each statement echoed before its output. The server's own comparison is of the whole text, which another engine cannot
//! reproduce, so this runs every statement of every test (each test in a database of its own, the way `mtr` does) and
//! compares what Accessor did with what MySQL did:
//!
//! * `ok`      both accepted the statement (and, for a query whose result the expected output shows, the rows agree);
//! * `error`   both refused it;
//! * `missing` MySQL accepted it and Accessor refused it;
//! * `lenient` MySQL refused it and Accessor accepted it;
//! * `value`   both accepted it, with different rows;
//! * `unaligned` the statement's output could not be found in the expected text (the query log was off, or the output
//!             was rewritten by `--replace_result`), and Accessor refused it: nothing to compare with.
//!
//! The interpreter understands what the tests need to get at their statements: `let`, `inc`, `dec`, `eval`, `if`,
//! `while`, `--source` of the files under `include/`, `--error`, and the query-log switches. Everything about
//! servers, replication, connections and files is ignored.
//!
//! A statement that exhausts memory or never ends is found and skipped: the tests run in a child process with a hard
//! memory limit (a Windows job object) and a time limit, restarted without the statement.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use accessor_core::{Database, QueryResult, Value};

// ----- reading a test ------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Unit {
    Sql { text: String, line: usize },
    Cmd { name: String, rest: String, line: usize },
}

/// Marks a skip entry that means "stop the test at this statement".
const STOP: usize = 1 << 40;

const COMMANDS: &[&str] = &[
    "echo", "source", "let", "eval", "error", "inc", "dec", "disable_query_log", "enable_query_log", "disable_result_log", "enable_result_log",
    "disable_warnings", "enable_warnings", "disable_info", "enable_info", "disable_abort_on_error", "enable_abort_on_error", "disable_metadata",
    "enable_metadata", "disable_ps_protocol", "enable_ps_protocol", "disable_connect_log", "enable_connect_log", "disable_session_track_info",
    "enable_session_track_info", "disable_reconnect", "enable_reconnect", "connect", "connection", "disconnect", "send", "reap", "sleep", "real_sleep",
    "delimiter", "if", "while", "exit", "remove_file", "write_file", "append_file", "copy_file", "move_file", "perl", "exec", "execw", "system", "replace_regex",
    "replace_column", "replace_result", "replace_numeric_round", "sorted_result", "partially_sorted_result", "vertical_results", "horizontal_results",
    "query_vertical", "query_horizontal", "query", "die", "skip", "save_master_pos", "sync_with_master", "sync_slave_with_master", "wait_for_slave_to_stop",
    "result_format", "reset_connection", "mkdir", "rmdir", "list_files", "cat_file", "file_exists", "output", "lowercase_result", "character_set", "ping",
    "diff_files", "chmod", "change_user", "dirty_close", "shutdown_server", "start_timer", "end_timer", "send_eval", "force-rmdir", "force-cpdir", "cpdir",
    "remove_files_wildcard", "copy_files_wildcard", "move_file", "assert", "sync_slave_with_master", "save_master_pos", "enable_testcase", "disable_testcase",
    "enable_async_client", "disable_async_client", "require", "wait_for_slave_to_start", "emit_warnings", "enable_prepare_warnings", "disable_prepare_warnings",
    "begin_ps_protocol", "end_ps_protocol", "send_quit", "error_unquoted", "result", "disable_rpl_parse", "enable_rpl_parse", "text", "end",
];

/// Splits a test into SQL statements and commands.
fn split_test(src: &str) -> Vec<Unit> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let mut delim: Vec<char> = vec![';'];
    let eol = |mut j: usize, b: &[char]| {
        while j < b.len() && b[j] != '\n' {
            j += 1;
        }
        j
    };
    let at_line_start = |i: usize, b: &[char]| {
        let mut j = i;
        while j > 0 && (b[j - 1] == ' ' || b[j - 1] == '\t') {
            j -= 1;
        }
        j == 0 || b[j - 1] == '\n'
    };
    while i < b.len() {
        let c = b[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '#' && at_line_start(i, &b) {
            i = eol(i, &b);
            continue;
        }
        if c == '-' && b.get(i + 1) == Some(&'-') && at_line_start(i, &b) {
            let j = eol(i, &b);
            let text: String = b[i + 2..j].iter().collect();
            let text = text.trim().to_string();
            let (name, rest) = match text.split_once(char::is_whitespace) {
                Some((n, r)) => (n.to_string(), r.trim().to_string()),
                None => (text.clone(), String::new()),
            };
            let lname = name.to_ascii_lowercase();
            if lname == "delimiter" {
                let d = rest.trim_end_matches(';').trim();
                delim = if d.is_empty() { vec![';'] } else { d.chars().collect() };
                i = j;
                continue;
            }
            if COMMANDS.contains(&lname.as_str()) {
                out.push(Unit::Cmd { name: lname.clone(), rest: rest.clone(), line });
                if matches!(lname.as_str(), "perl" | "write_file" | "append_file") {
                    i = skip_block(&b, j, &rest, &mut line);
                    continue;
                }
            }
            i = j;
            continue;
        }
        if c == '{' || c == '}' {
            out.push(Unit::Cmd { name: c.to_string(), rest: String::new(), line });
            i += 1;
            continue;
        }
        // the first word
        let mut j = i;
        while j < b.len() && (b[j].is_alphanumeric() || b[j] == '_' || b[j] == '-') {
            j += 1;
        }
        let word: String = b[i..j].iter().collect::<String>().to_ascii_lowercase();
        if (word == "while" || word == "if") && {
            let mut k = j;
            while k < b.len() && b[k] == ' ' {
                k += 1;
            }
            b.get(k) == Some(&'(')
        } {
            let mut k = j;
            while b[k] != '(' {
                k += 1;
            }
            let (mut depth, start) = (0, k);
            while k < b.len() {
                match b[k] {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    '\n' => line += 1,
                    _ => {}
                }
                k += 1;
            }
            out.push(Unit::Cmd { name: word, rest: b[start + 1..k.min(b.len())].iter().collect(), line });
            i = k + 1;
            continue;
        }
        if word == "delimiter" {
            let e = eol(j, &b);
            let d: String = b[j..e].iter().collect::<String>().trim().trim_end_matches(';').trim().to_string();
            delim = if d.is_empty() { vec![';'] } else { d.chars().collect() };
            i = e;
            continue;
        }
        // a statement or command up to the delimiter
        let start_line = line;
        let mut k = i;
        let command = COMMANDS.contains(&word.as_str()) && word != "end" && word != "text" && word != "result" && word != "query";
        let mut quote: Option<char> = None;
        let mut text = String::new();
        while k < b.len() {
            let ch = b[k];
            if ch == '\n' {
                line += 1;
                // a command with no delimiter ends at the line's end when it is one of the line commands
                if command && quote.is_none() && matches!(word.as_str(), "echo" | "source" | "inc" | "dec" | "connection" | "disconnect" | "error" | "sleep" | "exit" | "die" | "skip") && !text.trim_end().ends_with('`') && !(word == "let" && text.contains('`') && text.matches('`').count() % 2 == 1) && !(word == "let" && text.split_once('=').map_or(true, |(_, v)| v.trim().is_empty())) {
                    break;
                }
            }
            match quote {
                Some(q) => {
                    if ch == '\\' && q != '`' && k + 1 < b.len() {
                        text.push(ch);
                        text.push(b[k + 1]);
                        k += 2;
                        continue;
                    }
                    if ch == q {
                        quote = None;
                    }
                }
                None => {
                    if ch == '\'' || ch == '"' || ch == '`' {
                        quote = Some(ch);
                    } else if ch == '#' && at_line_start(k, &b) {
                        // a comment line inside a statement
                        while k < b.len() && b[k] != '\n' {
                            k += 1;
                        }
                        continue;
                    } else if ch == '/' && b.get(k + 1) == Some(&'*') {
                        // a comment inside a statement is kept
                        while k < b.len() && !(b[k] == '*' && b.get(k + 1) == Some(&'/')) {
                            if b[k] == '\n' {
                                line += 1;
                            }
                            text.push(b[k]);
                            k += 1;
                        }
                        text.push('*');
                        if k + 1 < b.len() {
                            text.push('/');
                        }
                        k += 2;
                        continue;
                    } else if b[k..].starts_with(&delim) {
                        break;
                    }
                }
            }
            text.push(ch);
            k += 1;
        }
        let consumed_delim = k < b.len() && b[k..].starts_with(&delim);
        let text = text.trim().to_string();
        if command {
            let rest = text[word.len().min(text.len())..].trim().to_string();
            out.push(Unit::Cmd { name: word.clone(), rest: rest.clone(), line: start_line });
            if matches!(word.as_str(), "perl" | "write_file" | "append_file") {
                i = skip_block(&b, k, &rest, &mut line);
                continue;
            }
        } else if !text.is_empty() {
            out.push(Unit::Sql { text, line: start_line });
        }
        i = if consumed_delim { k + delim.len() } else { k };
    }
    out
}

/// Skips the lines of a `perl;` or `write_file name;` block, which end with `EOF` (or the terminator given).
fn skip_block(b: &[char], from: usize, args: &str, line: &mut usize) -> usize {
    let term = args.split_whitespace().nth(1).unwrap_or("EOF").to_string();
    let mut i = from;
    while i < b.len() && b[i] != '\n' {
        i += 1;
    }
    loop {
        let start = i + 1;
        if start >= b.len() {
            return b.len();
        }
        let mut e = start;
        while e < b.len() && b[e] != '\n' {
            e += 1;
        }
        *line += 1;
        let l: String = b[start..e].iter().collect();
        i = e;
        if l.trim() == term {
            return e;
        }
    }
}

// ----- the expected output ---------------------------------------------------------------------------------

/// `s` with each run of white space as one space, and for each byte of the result its byte offset in `s`.
fn collapse(s: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(s.len());
    let mut map = Vec::with_capacity(s.len() + 1);
    let mut prev_space = true;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                map.push(i);
            }
            prev_space = true;
        } else {
            out.push(c);
            for _ in 0..c.len_utf8() {
                map.push(i);
            }
            prev_space = false;
        }
    }
    map.push(s.len());
    (out, map)
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Default, Clone)]
struct Expected {
    error: Option<String>,
    table: Option<Vec<Vec<String>>>,
}

fn parse_expected(segment: &str, wants_table: bool) -> Expected {
    let mut e = Expected::default();
    let seg = segment.strip_prefix(';').unwrap_or(segment);
    let lines: Vec<&str> = seg.lines().skip_while(|l| l.is_empty()).collect();
    if let Some(first) = lines.first() {
        if let Some(m) = first.strip_prefix("ERROR ") {
            e.error = Some(m.trim().to_string());
            return e;
        }
    }
    if let Some(l) = lines.iter().find(|l| l.starts_with("ERROR ")) {
        // an error after some output: the statement did fail
        if lines.len() == 1 || !wants_table {
            e.error = Some(l.trim_start_matches("ERROR ").trim().to_string());
            return e;
        }
    }
    if wants_table && !lines.is_empty() {
        let mut rows = Vec::new();
        for l in lines.iter().skip(1) {
            if *l == "Warnings:" || l.starts_with("ERROR ") || l.starts_with("affected rows:") {
                break;
            }
            rows.push(l.split('\t').map(str::to_string).collect::<Vec<_>>());
        }
        e.table = Some(rows);
        // the header gives the column names; keep their number in the first row's place
        e.table.as_mut().map(|t| t.insert(0, lines[0].split('\t').map(str::to_string).collect()));
    }
    e
}

// ----- running ---------------------------------------------------------------------------------------------

fn cell(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
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
            (None, '\'' | '"' | '`') => {
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
        "CREATE" | "ALTER" | "DROP" | "SHOW" | "SET" | "LOCK" | "FLUSH" => {
            let mut rest: Vec<String> = words.take(3).collect();
            rest.retain(|w| !matches!(w.as_str(), "OR" | "REPLACE" | "TEMP" | "TEMPORARY" | "UNIQUE" | "UNLOGGED" | "IF" | "NOT" | "EXISTS" | "GLOBAL" | "LOCAL" | "SESSION" | "FULL" | "ONLINE" | "IGNORE" | "DEFINER"));
            format!("{first} {}", rest.first().cloned().unwrap_or_default())
        }
        _ => first,
    }
}

fn wants_table(sql: &str) -> bool {
    matches!(lead(sql).split(' ').next().unwrap_or(""), "SELECT" | "WITH" | "VALUES" | "TABLE" | "(SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN")
        && !sql.trim_start().to_ascii_uppercase().starts_with("EXPLAIN")
        && !sql.trim_start().to_ascii_uppercase().starts_with("SHOW")
        && !sql.trim_start().to_ascii_uppercase().starts_with("DESC")
}

enum Outcome {
    Rows(QueryResult),
    Err(String),
    Panic,
}

struct Pending {
    idx: usize,
    line: usize,
    sql: String,
    outcome: Outcome,
    /// Where in the expected text the echo of this statement ends.
    echo_end: usize,
    sorted: bool,
}

struct Runner<'a> {
    db: Database,
    root: &'a Path,
    name: String,
    vars: HashMap<String, String>,
    out_text: String,
    norm: String,
    map: Vec<usize>,
    cursor: usize,
    pending: Option<Pending>,
    results: String,
    idx: usize,
    query_log: bool,
    expect_error: bool,
    sorted_result: bool,
    progress: Option<&'a Path>,
    skip: &'a [(String, usize)],
    depth: usize,
    steps: usize,
    /// Statements run per test, at most.
    cap: usize,
}

fn subst(s: &str, vars: &HashMap<String, String>) -> String {
    let b: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '$' && i + 1 < b.len() {
            let braced = b[i + 1] == '{';
            let s0 = if braced { i + 2 } else { i + 1 };
            let mut e = s0;
            while e < b.len() && (b[e].is_alphanumeric() || b[e] == '_') {
                e += 1;
            }
            let name: String = b[s0..e].iter().collect();
            if !name.is_empty() && (!braced || b.get(e) == Some(&'}')) {
                if let Some(v) = vars.get(&name) {
                    out.push_str(v);
                    i = if braced { e + 1 } else { e };
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Does `s` still contain a `$name` of mysqltest outside quotes?
fn has_unresolved(s: &str) -> bool {
    let b: Vec<char> = s.chars().collect();
    let mut quote: Option<char> = None;
    for (i, c) in b.iter().enumerate() {
        match quote {
            Some(q) if *c == q => quote = None,
            Some(_) => {}
            None if matches!(c, '\'' | '"' | '`') => quote = Some(*c),
            None if *c == '$' && b.get(i + 1).is_some_and(|n| n.is_alphabetic() || *n == '_' || *n == '{') => return true,
            None => {}
        }
    }
    false
}

fn skipped_include(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.starts_with("have_")
        || file.starts_with("not_")
        || file.starts_with("master-slave")
        || file.starts_with("check_")
        || file.starts_with("wait_")
        || file.starts_with("restart")
        || file.starts_with("kill_")
        || file.contains("binlog")
        || file.contains("slave")
        || file.contains("replica")
        || file.contains("mtr")
        || file.contains("rpl")
        || file.starts_with("force_")
        || file.starts_with("only_")
        || file.starts_with("big_test")
        || file.starts_with("no_valgrind")
        || file.starts_with("not_windows")
        || file.starts_with("has_")
        || file.starts_with("is_")
        || file.starts_with("setup_")
        || file.starts_with("cleanup")
        || file.starts_with("shutdown")
        || file.starts_with("start_")
        || file.starts_with("stop_")
}

impl Runner<'_> {
    fn write_progress(&self) {
        if let Some(p) = self.progress {
            let _ = std::fs::write(p, format!("{}\t{}", self.name, self.idx));
        }
    }

    fn record(&mut self, idx: usize, line: usize, verdict: &str, lead_word: &str, class: &str, detail: &str) {
        let _ = writeln!(self.results, "{}\t{idx}\t{line}\t{verdict}\t{lead_word}\t{class}\t{}", self.name, detail.replace(['\t', '\n'], " "));
    }

    /// Finds `text` (as the server echoes it) after the cursor: where its echo starts and ends in the expected text.
    fn find_echo(&self, text: &str) -> Option<(usize, usize)> {
        let (needle, _) = collapse(text);
        let needle = needle.trim().trim_end_matches(';').trim_end().to_string();
        if needle.is_empty() {
            return None;
        }
        let p = self.norm[self.cursor..].find(&needle)? + self.cursor;
        let (a, b) = (self.map[p], self.map[(p + needle.len()).min(self.map.len() - 1)]);
        Some((a, b)).map(|r| (r.0, r.1)).map(|r| {
            self.cursor_after(p + needle.len());
            r
        })
    }

    fn cursor_after(&self, _norm_pos: usize) {}

    /// Compares a statement's outcome with the expected text that followed its echo.
    fn resolve(&mut self, p: Pending, segment: Option<&str>) {
        let lead_word = lead(&p.sql);
        let wants = wants_table(&p.sql);
        let expected = segment.map(|s| parse_expected(s, wants));
        let (verdict, class, detail) = match (&p.outcome, expected.as_ref().and_then(|e| e.error.as_ref())) {
            (Outcome::Panic, _) => ("panic".to_string(), "the engine panicked".to_string(), short(&p.sql, 200)),
            (Outcome::Rows(r), None) => match expected.as_ref().and_then(|e| e.table.as_ref()) {
                Some(want) if wants => compare_rows(&p.sql, want, r, p.sorted),
                _ => ("ok".into(), String::new(), String::new()),
            },
            (Outcome::Rows(_), Some(msg)) => ("lenient".into(), class_of(msg), short(&p.sql, 200)),
            // a test statement with no `--error` before it is expected to work: mysqltest stops the test when one does not
            (Outcome::Err(m), None) => ("missing".into(), class_of(m), format!("{} -> {}", short(&p.sql, 200), short(m, 160))),
            (Outcome::Err(_), Some(_)) => ("error".into(), String::new(), String::new()),
        };
        self.record(p.idx, p.line, &verdict, &lead_word, &class, &detail);
    }

    /// An `--echo` line in the expected text ends the output of the statement before it.
    fn echo_line(&mut self, text: &str) {
        let Some((a, b)) = self.find_echo(text) else { return };
        if let Some(p) = self.pending.take() {
            let seg = self.out_text[p.echo_end.min(a)..a].to_string();
            self.resolve(p, Some(&seg));
        }
        self.cursor = self.cursor_from_orig(b);
    }

    fn cursor_from_orig(&self, orig: usize) -> usize {
        // the first normalized position whose original offset is at least `orig`
        self.map.partition_point(|&m| m < orig).min(self.norm.len())
    }

    fn sql(&mut self, text: &str, line: usize) {
        self.steps += 1;
        self.idx += 1;
        let idx = self.idx;
        let lead_word = lead(text);
        // a test given up at some statement: the ones after it are not run
        if self.skip.iter().any(|(t, i)| *t == self.name && *i & STOP != 0 && idx >= *i & !STOP) {
            self.expect_error = false;
            return;
        }
        if self.skip.iter().any(|(t, i)| *t == self.name && *i == idx) {
            self.record(idx, line, "killer", &lead_word, "", &short(text, 160));
            self.expect_error = false;
            return;
        }
        self.write_progress();
        let sorted = std::mem::take(&mut self.sorted_result);
        let expect_error = std::mem::take(&mut self.expect_error);
        let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.db.execute(&strip_test_schema(text)))) {
            Err(_) => Outcome::Panic,
            Ok(Ok(r)) => Outcome::Rows(r),
            Ok(Err(e)) => Outcome::Err(e.to_string()),
        };
        // where the server echoed it
        let echoed = if self.query_log { self.find_echo(text) } else { None };
        match echoed {
            Some((a, b)) => {
                if let Some(prev) = self.pending.take() {
                    let seg = self.out_text[prev.echo_end.min(a)..a].to_string();
                    self.resolve(prev, Some(&seg));
                }
                self.cursor = self.cursor_from_orig(b);
                self.pending = Some(Pending { idx, line, sql: text.to_string(), outcome, echo_end: b, sorted });
            }
            None => {
                // not echoed: only `--error` says what the server did
                let aligned = expect_error;
                let p = Pending { idx, line, sql: text.to_string(), outcome, echo_end: 0, sorted };
                if aligned {
                    let seg = "ERROR expected";
                    self.resolve(p, Some(seg));
                } else {
                    // no expectation: an acceptance is taken as agreement, a refusal cannot be judged
                    self.resolve(p, None);
                }
            }
        }
    }

    fn flush(&mut self) {
        if let Some(p) = self.pending.take() {
            let seg = self.out_text[p.echo_end.min(self.out_text.len())..].to_string();
            self.resolve(p, Some(&seg));
        }
    }

    fn truthy(&self, cond: &str) -> bool {
        let c = subst(cond, &self.vars);
        let c = c.trim();
        for op in ["<=", ">=", "!=", "==", "<", ">", "="] {
            if let Some((l, r)) = c.split_once(op) {
                let (l, r) = (l.trim().trim_matches('\''), r.trim().trim_matches('\''));
                return match (l.parse::<f64>(), r.parse::<f64>()) {
                    (Ok(a), Ok(b)) => match op {
                        "<=" => a <= b,
                        ">=" => a >= b,
                        "!=" => a != b,
                        "<" => a < b,
                        ">" => a > b,
                        _ => a == b,
                    },
                    _ => match op {
                        "!=" => l != r,
                        "=" | "==" => l == r,
                        _ => false,
                    },
                };
            }
        }
        if let Some(rest) = c.strip_prefix('!') {
            return !self.truthy(rest);
        }
        c.parse::<f64>().map(|n| n != 0.0).unwrap_or(!c.is_empty() && !c.starts_with('$') && !c.eq_ignore_ascii_case("null"))
    }

    fn run(&mut self, units: &[Unit]) {
        let mut i = 0;
        while i < units.len() {
            if self.steps >= self.cap {
                return;
            }
            match &units[i] {
                Unit::Sql { text, line } => self.sql(text, *line),
                Unit::Cmd { name, rest, line } => match name.as_str() {
                    "while" | "if" => {
                        // the block: from the next `{` to its `}`
                        let mut j = i + 1;
                        let mut depth = 0;
                        let mut start = None;
                        while j < units.len() {
                            if let Unit::Cmd { name: n, .. } = &units[j] {
                                if n == "{" {
                                    depth += 1;
                                    start.get_or_insert(j);
                                } else if n == "}" {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                            }
                            j += 1;
                        }
                        let (s, e) = (start.unwrap_or(i + 1), j.min(units.len()));
                        let body = &units[(s + 1).min(e)..e];
                        if name == "if" {
                            if self.truthy(rest) {
                                self.run(body);
                            }
                        } else {
                            let mut n = 0;
                            while self.truthy(rest) && n < 2000 {
                                self.run(body);
                                n += 1;
                            }
                        }
                        i = e + 1;
                        continue;
                    }
                    "let" => {
                        let r = rest.trim_start_matches(|c: char| c == ' ');
                        if let Some((l, v)) = r.split_once('=') {
                            let var = l.trim().trim_start_matches('$').to_string();
                            let v = v.trim().trim_end_matches(';').trim();
                            let value = if v.starts_with('`') && v.ends_with('`') && v.len() > 1 {
                                let q = subst(&v[1..v.len() - 1], &self.vars);
                                match self.db.execute(&q) {
                                    Ok(r) => r.rows.first().and_then(|r| r.first()).map(|v| if v.is_null() { "NULL".to_string() } else { cell(v) }).unwrap_or_default(),
                                    Err(_) => String::new(),
                                }
                            } else {
                                subst(v, &self.vars)
                            };
                            self.vars.insert(var, value);
                        }
                    }
                    "inc" | "dec" => {
                        let var = rest.trim().trim_start_matches('$').trim_end_matches(';').to_string();
                        let n: i64 = self.vars.get(&var).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                        self.vars.insert(var, (if name == "inc" { n + 1 } else { n - 1 }).to_string());
                    }
                    "eval" | "query" | "send_eval" => {
                        let t = subst(rest, &self.vars);
                        if has_unresolved(&t) {
                            // a variable that some include would have set (a helper script this runner does not run)
                            self.record(0, *line, "runner", "", "an unresolved $variable", &short(&t, 120));
                        } else {
                            self.sql(&t, *line);
                        }
                    }
                    "echo" => {
                        let t = subst(rest, &self.vars);
                        self.echo_line(t.trim_matches(|c| c == '"' || c == '\''));
                    }
                    "remove_file" => {
                        let path = subst(rest.trim().trim_end_matches(';'), &self.vars);
                        if !has_unresolved(&path) {
                            let _ = std::fs::remove_file(path.trim());
                        }
                    }
                    "error" => self.expect_error = !rest.trim().starts_with('0') || rest.contains(','),
                    "sorted_result" => self.sorted_result = true,
                    "disable_query_log" => self.query_log = false,
                    "enable_query_log" => self.query_log = true,
                    "source" => {
                        let path = subst(rest.trim().trim_end_matches(';'), &self.vars);
                        if !skipped_include(&path) && self.depth < 6 {
                            let candidates = [self.root.join(&path), self.root.join("include").join(&path), self.root.join("t").join(&path)];
                            if let Some(text) = candidates.iter().find_map(|p| std::fs::read(p).ok()) {
                                let units = split_test(&String::from_utf8_lossy(&text).replace("\r\n", "\n"));
                                self.depth += 1;
                                self.run(&units);
                                self.depth -= 1;
                            }
                        }
                    }
                    _ => {}
                },
            }
            i += 1;
        }
    }
}

fn compare_rows(sql: &str, want: &[Vec<String>], got: &QueryResult, sorted: bool) -> (String, String, String) {
    let ordered = sql.to_ascii_lowercase().contains("order by") && !sorted;
    let mut g: Vec<Vec<String>> = got.rows.iter().map(|r| r.iter().map(cell).collect()).collect();
    let header = &want[0];
    let mut w: Vec<Vec<String>> = want[1..].to_vec();
    if got.columns.len() != header.len() {
        return ("value".into(), "different number of columns".into(), format!("{} -> got {} column(s), MySQL {}", short(sql, 160), got.columns.len(), header.len()));
    }
    if g.len() != w.len() {
        return ("value".into(), "different number of rows".into(), format!("{} -> got {} row(s), MySQL {}", short(sql, 160), g.len(), w.len()));
    }
    if !ordered {
        g.sort();
        w.sort();
    }
    for (a, b) in g.iter().zip(&w) {
        if a != b {
            let k = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(0);
            return ("value".into(), "different values".into(), format!("{} -> got {:?}, MySQL {:?}", short(sql, 160), a.get(k), b.get(k)));
        }
    }
    ("ok".into(), String::new(), "compared".into())
}

fn run_test(root: &Path, name: &str, progress: Option<&Path>, skip: &[(String, usize)], cap: usize, results: &mut String) {
    let (Ok(t), Ok(r)) = (std::fs::read(root.join("t").join(format!("{name}.test"))), std::fs::read(root.join("r").join(format!("{name}.result")))) else { return };
    let test = String::from_utf8_lossy(&t).replace("\r\n", "\n");
    let out_text = String::from_utf8_lossy(&r).replace("\r\n", "\n");
    let (norm, map) = collapse(&out_text);
    let units = split_test(&test);
    let mut vars = HashMap::new();
    vars.insert("MYSQL_TEST_DIR".to_string(), root.display().to_string());
    vars.insert("MYSQLTEST_VARDIR".to_string(), std::env::temp_dir().display().to_string());
    let mut run = Runner {
        db: {
            accessor_core::schema::set_lenient(false);
            let mut db = Database::in_memory();
            db.set_dialect(accessor_core::dialect::Dialect::Mysql);
            db
        },
        root,
        name: name.to_string(),
        vars,
        out_text,
        norm,
        map,
        cursor: 0,
        pending: None,
        results: String::new(),
        idx: 0,
        query_log: true,
        expect_error: false,
        sorted_result: false,
        progress,
        skip,
        depth: 0,
        steps: 0,
        cap,
    };
    run.run(&units);
    run.flush();
    results.push_str(&run.results);
}

fn test_names(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join("t"))
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".test")).map(str::to_string))
                .filter(|n| root.join("r").join(format!("{n}.result")).exists())
                // the tests of mysqltest itself run shell commands and servers
                .filter(|n| !n.starts_with("mysqltest"))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn child(args: &[String]) {
    let mut cap = 3000usize;
    let (mut root, mut results, mut progress, mut skip, mut only, mut from) = (PathBuf::new(), PathBuf::new(), None, Vec::new(), Vec::<String>::new(), String::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--mysql" => root = it.next().map(PathBuf::from).unwrap_or_default(),
            "--results" => results = it.next().map(PathBuf::from).unwrap_or_default(),
            "--progress" => progress = it.next().map(PathBuf::from),
            "--from" => from = it.next().cloned().unwrap_or_default(),
            "--cap" => cap = it.next().and_then(|v| v.parse().ok()).unwrap_or(3000),
            "--skip" => {
                skip = it
                    .next()
                    .map(|v| v.split(',').filter_map(|p| p.rsplit_once(':').and_then(|(t, i)| i.parse().ok().map(|i| (t.to_string(), i)))).collect())
                    .unwrap_or_default()
            }
            "--only" => only = it.next().map(|v| v.split(',').map(str::to_string).collect()).unwrap_or_default(),
            _ => {}
        }
    }
    std::panic::set_hook(Box::new(|_| {}));
    let mut data = String::new();
    for t in test_names(&root) {
        if !from.is_empty() && t < from {
            continue;
        }
        if !only.is_empty() && !only.contains(&t) {
            continue;
        }
        if let Some(p) = &progress {
            let _ = std::fs::write(p, format!("{t}\t0"));
        }
        run_test(&root, &t, progress.as_deref(), &skip, cap, &mut data);
        let _ = std::fs::write(&results, &data);
    }
    let _ = std::fs::write(&results, &data);
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
    let (mut root, mut report, mut timeout, mut memory_mb, mut only, mut stall) = (PathBuf::new(), PathBuf::from("mysql-report.md"), 3600u64, 2048usize, String::new(), 20u64);
    let mut cap = 3000usize;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--mysql" => root = it.next().map(PathBuf::from).expect("--mysql PATH"),
            "--report" => report = it.next().map(PathBuf::from).expect("--report FILE"),
            "--timeout" => timeout = it.next().and_then(|v| v.parse().ok()).expect("--timeout SECONDS"),
            "--memory-mb" => memory_mb = it.next().and_then(|v| v.parse().ok()).expect("--memory-mb N"),
            "--only" => only = it.next().cloned().expect("--only a,b"),
            "--stall" => stall = it.next().and_then(|v| v.parse().ok()).expect("--stall SECONDS"),
            "--cap" => cap = it.next().and_then(|v| v.parse().ok()).expect("--cap N"),
            _ => {}
        }
    }
    if !root.join("t").exists() {
        eprintln!("usage: accessor-mtr --mysql PATH-TO/mysql-test [--report FILE] [--stall SECONDS] [--timeout SECONDS] [--memory-mb N] [--only a,b,c]");
        std::process::exit(2);
    }
    // the tests run in a folder of their own, so that the files they write (`INTO OUTFILE`) do not land in the one this was started in
    let root = std::path::absolute(&root).unwrap_or(root);
    let exe = std::env::current_exe().expect("own path");
    let scratch = std::env::temp_dir().join(format!("accessor-mtr-{}", std::process::id()));
    let work = scratch.join("work");
    std::fs::create_dir_all(&work).expect("temporary folder");
    let (results, progress) = (scratch.join("results"), scratch.join("progress"));
    let mut skip: Vec<(String, usize)> = Vec::new();
    let mut killers: Vec<(String, usize)> = Vec::new();
    let mut data = String::new();
    let mut from = String::new();
    let started = Instant::now();
    loop {
        let _ = std::fs::write(&progress, "");
        let _ = std::fs::write(&results, "");
        let mut cmd = std::process::Command::new(&exe);
        cmd.current_dir(&work);
        cmd.arg("--child").arg("--mysql").arg(&root).arg("--results").arg(&results).arg("--progress").arg(&progress);
        cmd.arg("--cap").arg(cap.to_string());
        if !from.is_empty() {
            cmd.arg("--from").arg(&from);
        }
        if !skip.is_empty() {
            cmd.arg("--skip").arg(skip.iter().map(|(t, i)| format!("{t}:{i}")).collect::<Vec<_>>().join(","));
        }
        if !only.is_empty() {
            cmd.arg("--only").arg(&only);
        }
        let mut child = cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().expect("child");
        limit_memory(&child, memory_mb);
        let deadline = Instant::now() + Duration::from_secs(timeout);
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
        // the tests the child finished
        let done = std::fs::read_to_string(&results).unwrap_or_default();
        data.push_str(&done);
        if ok {
            break;
        }
        let p = std::fs::read_to_string(&progress).unwrap_or_default();
        let Some((t, i)) = p.split_once('\t').and_then(|(t, i)| i.trim().parse::<usize>().ok().map(|i| (t.to_string(), i))) else {
            eprintln!("the child died before running anything");
            break;
        };
        eprintln!("KILLER {t} statement {i}: the process ended, or spent {stall}s on this statement (memory limit {memory_mb} MB)");
        // the killed test is run again from its start, without the statement; its earlier rows were not saved
        killers.push((t.clone(), i));
        if i == 0 || skip.contains(&(t.clone(), i)) || killers.len() > 400 {
            // a test that dies before its first statement is dropped
            from = format!("{t}\u{1}");
            if killers.len() > 400 {
                break;
            }
            continue;
        }
        if killers.iter().filter(|(kt, _)| *kt == t).count() >= 5 {
            // a test that keeps killing the process is given up: its other statements are not run
            // the statements before the first of them are kept; the rest of the test is not run
            let first = killers.iter().filter(|(kt, _)| *kt == t).map(|(_, ki)| *ki).min().unwrap_or(i);
            skip.push((t.clone(), first | STOP));
            from = t;
            continue;
        }
        skip.push((t.clone(), i));
        from = t;
    }
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
    let skipped_by_runner = rows.iter().filter(|r| r.verdict == "runner").count();
    let rows: Vec<Row> = rows.into_iter().filter(|r| r.verdict != "runner").collect();
    let mut verdicts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut per_test: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
    for r in &rows {
        *verdicts.entry(r.verdict).or_insert(0) += 1;
        *per_test.entry(r.test).or_default().entry(r.verdict).or_insert(0) += 1;
    }
    let statements = rows.len();
    let mut md = String::new();
    let _ = writeln!(md, "# MySQL test suite against Accessor\n");
    let _ = writeln!(md, "{} tests with statements, {statements} statements (at most {cap} per test; {skipped_by_runner} more left out because a variable of mysqltest could not be filled in), run in {:.0}s.\n", per_test.len(), started.elapsed().as_secs_f64());
    let _ = writeln!(md, "| Outcome | Statements | Share |\n| --- | ---: | ---: |");
    for (v, label) in [("ok", "both accepted (rows agree where shown)"), ("error", "both refused"), ("missing", "MySQL accepted, Accessor refused"), ("lenient", "MySQL refused, Accessor accepted"), ("value", "both accepted, different rows"), ("unaligned", "Accessor refused; MySQL's output not found to compare"), ("panic", "the engine panicked"), ("killer", "killed the process (skipped)")] {
        if let Some(n) = verdicts.get(v) {
            let _ = writeln!(md, "| {label} | {n} | {:.1}% |", 100.0 * *n as f64 / statements.max(1) as f64);
        }
    }
    let compared = rows.iter().filter(|r| r.verdict == "ok" && r.detail == "compared").count();
    let wrong = verdicts.get("value").copied().unwrap_or(0);
    let cascade = rows.iter().filter(|r| (r.verdict == "missing" || r.verdict == "unaligned") && (r.class.contains("does not exist") || r.class.contains("already exists") || r.class.contains("was not found"))).count();
    let _ = writeln!(md, "\nResult tables compared: {} ({} equal, {wrong} different). Of the refused statements, {cascade} were refused because an earlier statement that creates the object had failed.", compared + wrong, compared);
    let agree = verdicts.get("ok").copied().unwrap_or(0) + verdicts.get("error").copied().unwrap_or(0);
    let _ = writeln!(md, "\nAgreement on whether a statement is accepted: **{:.1}%** ({agree} of {statements}).\n", 100.0 * agree as f64 / statements.max(1) as f64);
    if !killers.is_empty() {
        let _ = writeln!(md, "## Statements that killed the process\n");
        for (t, i) in &killers {
            let _ = writeln!(md, "* `{t}` statement {i}");
        }
        let _ = writeln!(md);
    }
    let refused = |r: &&Row| r.verdict == "missing" || r.verdict == "unaligned";
    let mut classes: HashMap<(&str, &str), (usize, &str)> = HashMap::new();
    for r in rows.iter().filter(refused) {
        let e = classes.entry((r.lead, r.class)).or_insert((0, r.detail));
        e.0 += 1;
    }
    let mut by_class: Vec<_> = classes.into_iter().collect();
    by_class.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let _ = writeln!(md, "## What Accessor refuses that MySQL accepts: the 60 commonest errors\n");
    let _ = writeln!(md, "| Statements | Starting with | Error | Example |\n| ---: | --- | --- | --- |");
    for ((lead, class), (n, example)) in by_class.iter().take(60) {
        let _ = writeln!(md, "| {n} | {lead} | {} | `{}` |", class.replace('|', "\\|"), short(example, 150).replace('|', "\\|").replace('`', "'"));
    }
    let mut leads: HashMap<&str, usize> = HashMap::new();
    for r in rows.iter().filter(refused) {
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
    let _ = writeln!(md, "\n## What Accessor accepts that MySQL refuses (by MySQL's message)\n");
    let _ = writeln!(md, "| Statements | MySQL's error | Example |\n| ---: | --- | --- |");
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
    let _ = writeln!(md, "\n## By test (the 80 with most statements both engines accepted, then the whole table in the .tsv)\n");
    let _ = writeln!(md, "| Test | Statements | Both accepted | Both refused | Refused by Accessor | Accepted only by Accessor | Rows differ |\n| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
    let mut tests: Vec<_> = per_test.iter().collect();
    tests.sort_by(|a, b| b.1.get("ok").unwrap_or(&0).cmp(a.1.get("ok").unwrap_or(&0)));
    for (t, v) in tests.iter().take(80) {
        let n: usize = v.values().sum();
        let g = |k: &str| v.get(k).copied().unwrap_or(0);
        let _ = writeln!(md, "| {t} | {n} | {} | {} | {} | {} | {} |", g("ok"), g("error"), g("missing") + g("unaligned"), g("lenient"), g("value"));
    }
    std::fs::write(&report, &md).expect("the report can be written");
    let _ = std::fs::write(report.with_extension("tsv"), &data);
    println!("{}", md.lines().take(24).collect::<Vec<_>>().join("\n"));
    println!("\n(full report: {})", report.display());
}

/// The database of the tests is called `test`; Accessor has one schema, so `test.t1` is read as `t1` (outside quotes).
fn strip_test_schema(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => {
                if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                } else if (i == 0 || !(c[i - 1].is_alphanumeric() || c[i - 1] == '_' || c[i - 1] == '.'))
                    && c[i..].iter().take(5).collect::<String>().eq_ignore_ascii_case("test.")
                    && c.get(i + 5).is_some_and(|n| n.is_alphabetic() || *n == '_' || *n == '`')
                {
                    i += 5;
                    continue;
                }
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}
