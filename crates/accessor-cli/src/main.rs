//! `accessor` — an interactive SQL shell for Accessor databases.
//!
//! ```text
//! accessor [FILE.adb] [-c "SQL"] [-f script.sql]
//! ```

use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::process::ExitCode;
use std::time::Instant;

use accessor_core::sql::lexer::lex;
use accessor_core::{csv, Database, Durability, QueryResult, Value};

const HELP: &str = "\
Statements end with ';'. Transactions: BEGIN, COMMIT, ROLLBACK, SAVEPOINT name,
ROLLBACK TO name, RELEASE name; the prompt shows * while one is open.
Dot-commands:
  .tables                    list tables
  .queries                   list saved queries (views)
  .schema [TABLE]            show table definitions
  .indexes TABLE             show a table's indexes
  .import FILE TABLE         append CSV rows (header names matched to columns)
  .import --new FILE TABLE   create TABLE from CSV, inferring column types
  .export FILE SQL           write a query's result set as CSV
  .read FILE                 run a SQL script
  .sync on|off               fsync every commit (default on)
  .timezone [NAME]           show or set the session time zone (UTC, +05:30, Europe/Paris)
  .optimizer on|off          turn the query optimizer on or off (for comparing plans)
  .timer on|off              show statement timings
  .checkpoint                fold the log into the database file
  .compact                   compact and repair
  .help                      this text
  .quit                      leave";

struct Shell {
    db: Database,
    timer: bool,
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut path: Option<String> = None;
    let mut commands: Vec<String> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("usage: accessor [FILE.adb] [-c SQL]... [-f SCRIPT]...\n\n{HELP}");
                return ExitCode::SUCCESS;
            }
            "-c" => match args.next() {
                Some(c) => commands.push(c),
                None => return usage_error("-c needs an argument"),
            },
            "-f" => match args.next().map(fs::read_to_string) {
                Some(Ok(s)) => commands.push(s),
                Some(Err(e)) => return usage_error(&format!("cannot read script: {e}")),
                None => return usage_error("-f needs an argument"),
            },
            other if path.is_none() && !other.starts_with('-') => path = Some(other.to_string()),
            other => return usage_error(&format!("unknown argument '{other}'")),
        }
    }

    let db = match &path {
        Some(p) => match Database::open(p) {
            Ok(db) => db,
            Err(e) => {
                eprintln!("cannot open '{p}': {e}");
                return ExitCode::FAILURE;
            }
        },
        None => Database::in_memory(),
    };
    let mut sh = Shell { db, timer: false };

    if !commands.is_empty() {
        let mut ok = true;
        for c in &commands {
            ok &= sh.run_script(c);
        }
        // An unfinished transaction means the script's work was discarded: that is a failure.
        ok &= !sh.db.in_transaction();
        sh.warn_if_transaction_open();
        return if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE };
    }

    let interactive = io::stdin().is_terminal();
    if interactive {
        match &path {
            Some(p) => println!("Accessor shell — {p}   (.help for commands)"),
            None => println!("Accessor shell — transient in-memory database   (.help for commands)"),
        }
    }
    sh.repl(interactive);
    ExitCode::SUCCESS
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("{msg}\nusage: accessor [FILE.adb] [-c SQL]... [-f SCRIPT]...");
    ExitCode::from(2)
}

/// A buffer is a complete statement batch when it ends with `;` outside any
/// string literal or comment.
fn is_complete(buf: &str) -> bool {
    buf.trim_end().ends_with(';') && lex(buf).is_ok()
}

impl Shell {
    fn repl(&mut self, interactive: bool) {
        let stdin = io::stdin();
        let mut buf = String::new();
        loop {
            if interactive {
                let open = if self.db.in_transaction() { "*" } else { "" };
                print!("{}", if buf.is_empty() { format!("accessor{open}> ") } else { "     ...> ".to_string() });
                let _ = io::stdout().flush();
            }
            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let trimmed = line.trim();
            if buf.is_empty() && trimmed.starts_with('.') {
                if !self.dot_command(trimmed) {
                    break;
                }
                continue;
            }
            if buf.is_empty() && trimmed.is_empty() {
                continue;
            }
            buf.push_str(&line);
            if is_complete(&buf) {
                self.run_script(&buf);
                buf.clear();
            }
        }
        if !buf.trim().is_empty() {
            self.run_script(&buf);
        }
        self.warn_if_transaction_open();
    }

    /// A transaction still open when the shell ends is rolled back; say so.
    fn warn_if_transaction_open(&self) {
        if self.db.in_transaction() {
            eprintln!(
                "warning: a transaction was still open ({} change(s)); it was rolled back, not committed",
                self.db.pending_changes()
            );
        }
    }

    /// Runs SQL (possibly several statements); returns false if one failed.
    fn run_script(&mut self, sql: &str) -> bool {
        let stmts = match accessor_core::sql::parse_script(sql) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: {e}");
                return false;
            }
        };
        for st in &stmts {
            let t = Instant::now();
            match self.db.execute_statement(st) {
                Ok(res) => {
                    print_result(&res);
                    if self.timer {
                        println!("({:.3} ms)", t.elapsed().as_secs_f64() * 1e3);
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    return false;
                }
            }
        }
        true
    }

    /// Returns false when the shell should exit.
    fn dot_command(&mut self, line: &str) -> bool {
        let mut parts = line.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();
        let report = |r: Result<(), accessor_core::Error>| {
            if let Err(e) = r {
                eprintln!("Error: {e}");
            }
        };
        match cmd {
            ".quit" | ".exit" | ".q" => return false,
            ".help" => println!("{HELP}"),
            ".tables" => self.db.table_names().iter().for_each(|n| println!("{n}")),
            ".queries" => self.db.view_names().iter().for_each(|n| println!("{n}")),
            ".schema" => self.schema(rest),
            ".indexes" => match self.db.table(rest) {
                Some(t) => {
                    for ix in t.index_defs() {
                        let cols: Vec<_> = ix.columns.iter().map(|&c| t.schema().columns[c].name.as_str()).collect();
                        println!("{}{}  ({})", if ix.unique { "UNIQUE " } else { "" }, ix.name, cols.join(", "));
                    }
                }
                None => eprintln!("no such table '{rest}'"),
            },
            ".timezone" => {
                if !rest.is_empty() {
                    if let Err(e) = self.db.set_time_zone(rest) {
                        eprintln!("Error: {e}");
                        return true;
                    }
                }
                println!("time zone {}", self.db.time_zone());
            }
            ".optimizer" => {
                self.db.set_optimizer(!rest.eq_ignore_ascii_case("off"));
                println!("optimizer {}", if self.db.optimizer_enabled() { "on" } else { "off" });
            }
            ".timer" => self.timer = rest.eq_ignore_ascii_case("on"),
            ".sync" => self.db.set_durability(if rest.eq_ignore_ascii_case("off") {
                Durability::OsBuffered
            } else {
                Durability::Fsync
            }),
            ".checkpoint" => report(self.db.checkpoint()),
            ".compact" => report(self.db.compact()),
            ".read" => match fs::read_to_string(rest) {
                Ok(s) => {
                    self.run_script(&s);
                }
                Err(e) => eprintln!("cannot read '{rest}': {e}"),
            },
            ".import" => self.import(rest),
            ".export" => self.export(rest),
            other => eprintln!("unknown command '{other}' (try .help)"),
        }
        true
    }

    /// Prints table definitions as SQL that would recreate them (data excluded).
    fn schema(&self, only: &str) {
        let names: Vec<&str> = if only.is_empty() { self.db.table_names() } else { vec![only] };
        for n in names {
            let Some(t) = self.db.table(n) else {
                eprintln!("no such table '{n}'");
                continue;
            };
            let s = t.schema();
            let keys: Vec<&str> = s.columns.iter().filter(|c| c.primary_key).map(|c| c.name.as_str()).collect();
            let mut lines: Vec<String> = Vec::new();
            for c in &s.columns {
                let line = if let Some(formula) = c.expression() {
                    format!("  [{}] CALCULATED AS ({formula})", c.name)
                } else {
                    let mut line =
                        format!("  [{}] {}", c.name, if c.autonumber { "COUNTER" } else { c.data_type.sql_name() });
                    if c.primary_key && keys.len() == 1 {
                        line.push_str(" PRIMARY KEY");
                    } else if c.not_null && !c.primary_key {
                        line.push_str(" NOT NULL");
                    }
                    if let Some(d) = &c.default {
                        line.push_str(&format!(" DEFAULT {}", sql_literal(d)));
                    }
                    line
                };
                lines.push(line);
            }
            if keys.len() > 1 {
                let list: Vec<String> = keys.iter().map(|k| format!("[{k}]")).collect();
                lines.push(format!("  PRIMARY KEY ({})", list.join(", ")));
            }
            for fk in &s.foreign_keys {
                let own: Vec<String> = fk.columns.iter().map(|&c| format!("[{}]", s.columns[c].name)).collect();
                let parent = self.db.table(&fk.ref_table);
                let theirs: Vec<String> = fk
                    .ref_columns
                    .iter()
                    .map(|&c| parent.map_or(c.to_string(), |p| format!("[{}]", p.schema().columns[c].name)))
                    .collect();
                let mut line = format!(
                    "  CONSTRAINT [{}] FOREIGN KEY ({}) REFERENCES [{}] ({})",
                    fk.name,
                    own.join(", "),
                    fk.ref_table,
                    theirs.join(", ")
                );
                if fk.on_delete != accessor_core::RefAction::Restrict {
                    line.push_str(&format!(" ON DELETE {}", fk.on_delete.sql()));
                }
                if fk.on_update != accessor_core::RefAction::Restrict {
                    line.push_str(&format!(" ON UPDATE {}", fk.on_update.sql()));
                }
                if fk.deferrable {
                    line.push_str(if fk.initially_deferred { " DEFERRABLE INITIALLY DEFERRED" } else { " DEFERRABLE" });
                }
                lines.push(line);
            }
            println!("CREATE TABLE {} (\n{}\n);", t.name(), lines.join(",\n"));
        }
        if only.is_empty() {
            for v in self.db.view_names() {
                println!("CREATE VIEW {v} AS {};", self.db.view_sql(v).unwrap_or(""));
            }
            for t in self.db.trigger_names() {
                println!("{};", self.db.trigger_sql(t).unwrap_or(""));
            }
        }
    }

    fn import(&mut self, rest: &str) {
        let (new_table, rest) = match rest.strip_prefix("--new") {
            Some(r) => (true, r.trim()),
            None => (false, rest),
        };
        let mut it = rest.split_whitespace();
        let (Some(file), Some(table)) = (it.next(), it.next()) else {
            eprintln!("usage: .import [--new] FILE TABLE");
            return;
        };
        let text = match fs::read_to_string(file) {
            Ok(t) => t,
            Err(e) => return eprintln!("cannot read '{file}': {e}"),
        };
        let res = if new_table {
            csv::import_csv_as_table(&mut self.db, table, &text)
        } else {
            csv::import_csv(&mut self.db, table, &text, true)
        };
        match res {
            Ok(n) => println!("{n} rows imported"),
            Err(e) => eprintln!("Error: {e}"),
        }
    }

    fn export(&mut self, rest: &str) {
        let Some((file, sql)) = rest.split_once(char::is_whitespace) else {
            return eprintln!("usage: .export FILE SQL");
        };
        match self.db.query(sql.trim().trim_end_matches(';')) {
            Ok(res) => {
                let out = fs::File::create(file).and_then(|f| {
                    let mut w = io::BufWriter::new(f);
                    csv::write_csv(&mut w, &res.columns, &res.rows)?;
                    w.flush()
                });
                match out {
                    Ok(()) => println!("{} rows written to {file}", res.rows.len()),
                    Err(e) => eprintln!("cannot write '{file}': {e}"),
                }
            }
            Err(e) => eprintln!("Error: {e}"),
        }
    }
}

fn print_result(res: &QueryResult) {
    if res.columns.is_empty() {
        if res.affected > 0 {
            println!("{} row{} affected", res.affected, if res.affected == 1 { "" } else { "s" });
        }
        return;
    }
    // A query plan is read line by line, not as a grid, and is never shortened.
    if res.columns.len() == 1 && res.columns[0] == "plan" {
        for r in &res.rows {
            println!("{}", r[0]);
        }
        return;
    }
    const MAX_WIDTH: usize = 60;
    let cell = |v: &Value| {
        let s = if v.is_null() { "NULL".to_string() } else { v.to_string().replace('\n', "⏎") };
        if s.chars().count() > MAX_WIDTH {
            format!("{}…", s.chars().take(MAX_WIDTH - 1).collect::<String>())
        } else {
            s
        }
    };
    let body: Vec<Vec<String>> = res.rows.iter().map(|r| r.iter().map(cell).collect()).collect();
    let mut widths: Vec<usize> = res.columns.iter().map(|c| c.chars().count()).collect();
    for row in &body {
        for (w, c) in widths.iter_mut().zip(row) {
            *w = (*w).max(c.chars().count());
        }
    }
    let numeric: Vec<bool> = (0..res.columns.len())
        .map(|i| {
            res.rows
                .iter()
                .all(|r| matches!(r[i], Value::Int(_) | Value::Dec(_) | Value::Null))
        })
        .collect();
    let line = |cells: &[String]| {
        let parts: Vec<String> = cells
            .iter()
            .zip(&widths)
            .zip(&numeric)
            .map(|((c, w), num)| {
                let pad = w - c.chars().count();
                if *num {
                    format!("{}{c}", " ".repeat(pad))
                } else {
                    format!("{c}{}", " ".repeat(pad))
                }
            })
            .collect();
        println!("{}", parts.join(" │ ").trim_end());
    };
    let header: Vec<String> = res.columns.clone();
    line(&header);
    println!("{}", widths.iter().map(|w| "─".repeat(*w)).collect::<Vec<_>>().join("─┼─"));
    for row in &body {
        line(row);
    }
    println!("({} row{})", body.len(), if body.len() == 1 { "" } else { "s" });
}

/// A value written the way SQL would read it back.
fn sql_literal(v: &Value) -> String {
    match v {
        Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        Value::DateTime(_) => format!("#{v}#"),
        // Money prints with a `$`, which SQL numbers do not have.
        Value::Currency(_) => v.to_string().replace('$', ""),
        other => other.to_string(),
    }
}
