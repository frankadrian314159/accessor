//! TPC-DS as the benchmark's own tools produce it: the data written by `dsdgen`, the 99 qualification queries written by
//! `dsqgen`, and the answer sets the kit ships for scale factor 1 (see `scripts/tpcds-kit.sh`, which builds the tools from the
//! `third_party/tpcds-kit` submodule). Each query must run, and its result must equal the answer set.
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Instant;

use accessor_core::{csv::import_csv, Database};

use crate::{run, INDEXES};

/// Lines of a `.dat` file imported at a time.
const CHUNK: usize = 50_000;

/// Loads a `dsdgen` file (fields split by `|`, an empty field is NULL) into the table.
fn load_dat(db: &mut Database, table: &str, path: &Path) -> Result<usize, String> {
    let mut reader = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let (mut csv, mut lines, mut total) = (String::new(), 0, 0);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf).map_err(|e| e.to_string())?;
        if n > 0 {
            // the files are Latin-1 where a name has an accent
            let line = match std::str::from_utf8(&buf) {
                Ok(s) => s.to_string(),
                Err(_) => buf.iter().map(|&b| b as char).collect(),
            };
            for (i, field) in line.trim_end_matches(['\n', '\r']).split('|').enumerate() {
                if i > 0 {
                    csv.push(',');
                }
                if !field.is_empty() {
                    csv.push('"');
                    for c in field.chars() {
                        if c == '"' {
                            csv.push('"');
                        }
                        csv.push(c);
                    }
                    csv.push('"');
                }
            }
            csv.push('\n');
            lines += 1;
        }
        if lines == CHUNK || (n == 0 && lines > 0) {
            total += import_csv(db, table, &csv, false).map_err(|e| format!("loading {table}: {e}"))?;
            csv.clear();
            lines = 0;
        }
        if n == 0 {
            return Ok(total);
        }
    }
}

/// The queries of a `dsqgen` stream file, by template number; a template can hold several statements.
fn read_queries(path: &Path) -> Vec<(u32, Vec<String>)> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out: Vec<(u32, Vec<String>)> = Vec::new();
    let mut current: Option<(u32, String)> = None;
    let finish = |current: Option<(u32, String)>, out: &mut Vec<(u32, Vec<String>)>| {
        if let Some((n, body)) = current {
            let mut statements = Vec::new();
            let mut stmt = String::new();
            for line in body.lines() {
                stmt.push_str(line);
                stmt.push('\n');
                if line.trim_end().ends_with(';') {
                    statements.push(std::mem::take(&mut stmt));
                }
            }
            if !stmt.trim().is_empty() {
                statements.push(stmt);
            }
            out.push((n, statements));
        }
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("-- start query") {
            finish(current.take(), &mut out);
            let n = rest.split("using template query").nth(1).and_then(|t| t.trim().trim_end_matches(".tpl").parse().ok()).unwrap_or(0);
            current = Some((n, String::new()));
        } else if line.starts_with("-- end query") {
            finish(current.take(), &mut out);
        } else if let Some((_, body)) = &mut current {
            body.push_str(line);
            body.push('\n');
        }
    }
    finish(current.take(), &mut out);
    out.sort_by_key(|(n, _)| *n);
    out
}

/// One value of a row or an answer, as it is compared: numbers to two decimals, text as is.
fn token(t: &str) -> String {
    match t.parse::<f64>() {
        Ok(x) if t.chars().any(|c| c.is_ascii_digit()) && !t.contains(['e', 'E']) => {
            let s = format!("{x:.2}");
            if s == "-0.00" { "0.00".into() } else { s }
        }
        _ => t.to_string(),
    }
}

/// Splits a row into whitespace-separated tokens (the answer sets are fixed-width text, so values with spaces cannot be told apart
/// from neighbours; both sides are compared as token lists).
fn row_key(fields: &[&str]) -> String {
    // `last_name || ', ' || first_name` of a CHAR(30) column has its padding in the answer files: `Carter , Rodney`
    fields.iter().flat_map(|f| f.split_whitespace()).filter(|t| *t != "NULL").map(token).collect::<Vec<_>>().join(" ").replace(" ,", ",")
}

/// A row as its values, whitespace separated, without the NULLs and with the padding before a comma gone, but with the numbers as
/// written: the answer files print them to as many digits as the column is wide.
fn raw_row(fields: &[&str]) -> String {
    fields.iter().flat_map(|f| f.split_whitespace()).filter(|t| *t != "NULL").collect::<Vec<_>>().join(" ").replace(" ,", ",")
}

/// Whether `mine` is `answer` with each number rounded to the digits the answer prints: the files are fixed-width text, so
/// a 10-character column holds `132885062` or `20390161.4` for what is 132885061.65 or 20390161.37.
fn rounded_equal(answer: &str, mine: &str) -> bool {
    let (a, m): (Vec<&str>, Vec<&str>) = (answer.split_whitespace().collect(), mine.split_whitespace().collect());
    let number = |t: &str| t.parse::<f64>().ok().filter(|_| t.chars().any(|c| c.is_ascii_digit()) && !t.contains(['e', 'E']));
    a.len() == m.len()
        && a.iter().zip(&m).all(|(x, y)| match (number(x), number(y)) {
            (Some(p), Some(q)) => {
                let decimals = x.split_once('.').map_or(0, |(_, f)| f.len()) as i32;
                (p - q).abs() <= 0.5 * 10f64.powi(-decimals) + 1e-9
            }
            _ => x == y,
        })
}

/// The rows of an answer set: after the header and its dashed line. The files are fixed-width text, tab-padded text, or
/// split by `|`; a NULL is an empty field.
fn answer_rows(text: &str) -> Vec<(String, String)> {
    let dashes = |l: &str| l.contains('-') && l.chars().all(|c| matches!(c, '-' | ' ' | '\t' | '|'));
    let mut rows = Vec::new();
    let mut seen_header = false;
    let mut header: Option<String> = None;
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        // a second result set of the same query starts with a banner, then its own header and dashes
        if l.trim_start().starts_with("-----") && l.contains("OUTPUT") {
            seen_header = false;
            continue;
        }
        // the footers some servers print
        let t = l.trim();
        if (t.starts_with('(') && (t.ends_with(" rows)") || t.ends_with(" row)"))) || t.starts_with("Warning:") || t.ends_with("record(s) selected.") || t.ends_with("rows selected.") {
            continue;
        }
        if !seen_header {
            // the header is one line before the dashes, or (in the `|` files) the first line alone
            if !dashes(l) {
                header = Some(row_key(&[&l.replace('|', " ")]));
            }
            seen_header = dashes(l) || l.contains('|');
            continue;
        }
        if dashes(l) || l.trim_end().ends_with("rows selected.") {
            continue;
        }
        // query 44's answers print a NULL product name as a lone `%`, query 73's NULL salutation as a lone `-`
        let spaced = l.replace('|', " ");
        let line: Vec<&str> = spaced.split_whitespace().map(|t| if t == "%" || t == "-" { "NULL" } else { t }).collect();
        let key = row_key(&[&line.join(" ")]);
        // the header again, between the result sets of a query with several statements
        if header.as_ref() == Some(&key) {
            continue;
        }
        rows.push((key, raw_row(&[&line.join(" ")])));
    }
    rows
}

/// Whether the rows equal one of the answer files for the query (`N.ans`, or `N_NULLS_FIRST.ans` / `N_NULLS_LAST.ans`).
fn check_answer(dir: &Path, n: u32, rows: &[Vec<String>]) -> String {
    let mut files: Vec<String> = Vec::new();
    for name in [format!("{n}.ans"), format!("{n}_NULLS_FIRST.ans"), format!("{n}_NULLS_LAST.ans")] {
        if dir.join(&name).exists() {
            files.push(name);
        }
    }
    if files.is_empty() {
        return "no answer set".into();
    }
    let mut mine_rows: Vec<(String, String)> = rows
        .iter()
        .map(|r| {
            let fields: Vec<&str> = r.iter().map(String::as_str).collect();
            (row_key(&fields), raw_row(&fields))
        })
        .collect();
    mine_rows.sort();
    let mine: Vec<String> = mine_rows.iter().map(|(k, _)| k.clone()).collect();
    let mut best = String::new();
    for name in files {
        let text = String::from_utf8_lossy(&std::fs::read(dir.join(&name)).unwrap_or_default()).into_owned();
        let mut want_rows = answer_rows(&text);
        want_rows.sort();
        let want: Vec<String> = want_rows.iter().map(|(k, _)| k.clone()).collect();
        if want == mine {
            return "matches the answer set".into();
        }
        // the rows that differ, matched again with the numbers rounded to the digits the answer prints
        let mut unused: HashMap<&str, usize> = HashMap::new();
        for k in &mine {
            *unused.entry(k).or_default() += 1;
        }
        let want_left: Vec<&str> = want_rows
            .iter()
            .filter(|(k, _)| match unused.get_mut(k.as_str()) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    false
                }
                _ => true,
            })
            .map(|(_, raw)| raw.as_str())
            .collect();
        let mut mine_left: Vec<Option<&str>> = Vec::new();
        for (k, raw) in &mine_rows {
            match unused.get_mut(k.as_str()) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    mine_left.push(Some(raw.as_str()));
                }
                _ => {}
            }
        }
        if want_left.len() == mine_left.len() && want_left.len() <= 5000 {
            let all_found = want_left.iter().all(|w| match mine_left.iter().position(|m| m.is_some_and(|m| rounded_equal(w, m))) {
                Some(i) => {
                    mine_left[i] = None;
                    true
                }
                None => false,
            });
            if all_found {
                return "matches the answer set (numbers as rounded in the answer file)".into();
            }
        }
        if std::env::var_os("TPCDS_DEBUG").is_some() {
            for w in want.iter().filter(|w| !mine.contains(w)).take(3) {
                eprintln!("  want only: {w}");
            }
            for m in mine.iter().filter(|m| !want.contains(m)).take(3) {
                eprintln!("  mine only: {m}");
            }
        }
        let missing = want.iter().filter(|w| !mine.contains(w)).count();
        best = format!("DIFFERENT from the answer set: {} rows against {}, {missing} of the answers not found", rows.len(), want.len());
    }
    best
}

/// Returns the number of queries that failed or differ.
pub fn main(dir: &str, db_file: Option<&str>, only: &[u32], indexes: bool, timeout: u64, explain: bool, extra: &[String], repl: bool, settings: &[String]) -> usize {
    let dir = Path::new(dir);
    // with a database file the data is loaded once and kept there for the runs that follow
    let loaded = db_file.is_some_and(|f| Path::new(f).exists());
    let started = Instant::now();
    let mut db = match db_file {
        Some(f) => Database::open(f).expect("the database file"),
        None => Database::in_memory(),
    };
    if !loaded {
        let schema = std::fs::read_to_string(dir.join("tpcds.sql")).expect("tpcds.sql in the kit directory");
        let schema: String = schema.lines().filter(|l| !l.trim_start().starts_with("--")).collect::<Vec<_>>().join("
");
        db.execute(&schema).expect("the schema");
        let mut files: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();
        for entry in std::fs::read_dir(dir.join("data")).expect("the data directory").flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "dat") {
                files.insert(p.file_stem().unwrap().to_string_lossy().to_string(), p);
            }
        }
        let mut total = 0;
        for (table, path) in &files {
            match load_dat(&mut db, table, path) {
                Ok(n) => total += n,
                Err(e) => println!("{e}"),
            }
        }
        if indexes {
            db.execute(INDEXES).expect("the indexes");
        }
        db.checkpoint().expect("saving the database");
        println!("loaded {} tables, {total} rows, in {:.1}s{}", files.len(), started.elapsed().as_secs_f64(), if indexes { "" } else { " (no indexes)" });
    } else {
        println!("opened the database file in {:.1}s", started.elapsed().as_secs_f64());
    }
    let _ = db.execute(&format!("SET statement_timeout = '{timeout}s'"));
    // the answer sets were made by servers that divide whole numbers exactly (queries 73, 78, 83) and round a tie away from zero (78)
    for st in settings {
        db.execute(st).unwrap_or_else(|e| panic!("{st}: {e}"));
    }
    std::panic::set_hook(Box::new(|_| {}));
    // statements given with `--sql` run instead of the queries
    if !extra.is_empty() {
        for sql in extra {
            match run(&mut db, sql) {
                Ok((rows, took)) => {
                    println!("{:.3}s, {} rows", took.as_secs_f64(), rows.len());
                    rows.iter().take(30).for_each(|r| println!("  {r}"));
                }
                Err(e) => println!("FAILED: {e}"),
            }
        }
        return 0;
    }
    let queries = read_queries(&dir.join("queries").join("query_0.sql"));
    if repl {
        session(&mut db, dir, &queries);
        std::mem::forget(db);
        return 0;
    }
    let bad = run_queries(&mut db, dir, &queries, only, explain);
    // the queries changed nothing, and freeing 20 million rows one by one takes longer than running a query: the process
    // ends next, and the system takes the memory back
    std::mem::forget(db);
    bad
}

/// Runs the queries (all of them, or `only`), printing a verdict for each; returns how many failed or differ.
fn run_queries(db: &mut Database, dir: &Path, queries: &[(u32, Vec<String>)], only: &[u32], explain: bool) -> usize {
    let (mut bad, mut good) = (0, 0);
    println!("{:>3} {:>7} {:>9}  {}", "Q", "rows", "seconds", "result");
    for (n, statements) in queries {
        let n = *n;
        if !only.is_empty() && !only.contains(&n) {
            continue;
        }
        let t = Instant::now();
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut failure = None;
        for sql in statements {
            if explain {
                match run(db, &format!("EXPLAIN {sql}")) {
                    Ok((plan, _)) => println!("{n}:
{}", plan.join("
")),
                    Err(e) => println!("{n}: EXPLAIN failed: {e}"),
                }
                continue;
            }
            match run(db, sql) {
                Err(e) => {
                    failure = Some(e);
                    break;
                }
                // a template with several statements has the answers of all of them, one after the other
                Ok((r, _)) => rows.extend(r.iter().map(|row| row.split('|').map(str::to_string).collect())),
            }
        }
        if explain {
            continue;
        }
        let took = t.elapsed().as_secs_f64();
        match failure {
            Some(e) => {
                bad += 1;
                println!("{n:>3} {:>7} {took:>9.3}  FAILED: {e}", "");
            }
            None => {
                let verdict = check_answer(&dir.join("answers"), n, &rows);
                if verdict.starts_with("DIFFERENT") {
                    bad += 1;
                } else if verdict.starts_with("matches") {
                    good += 1;
                }
                println!("{n:>3} {:>7} {took:>9.3}  {verdict}", rows.len());
            }
        }
    }
    println!("{good} queries match their answer sets, {bad} failed or differ");
    bad
}

/// `1,5,10-12` as the query numbers.
fn query_list(text: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => out.extend(a.trim().parse::<u32>().unwrap_or(0)..=b.trim().parse::<u32>().unwrap_or(0)),
            None => out.extend(part.parse::<u32>()),
        }
    }
    out
}

/// A session: the database stays open (and loaded) while commands arrive on standard input, one a line:
/// `run 39,72`, `explain 72`, `sql SELECT ...`, `timeout 300`, `quit`. Each command ends with a line `done`.
fn session(db: &mut Database, dir: &Path, queries: &[(u32, Vec<String>)]) {
    println!("ready");
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        match cmd {
            "" => continue,
            "quit" | "exit" => break,
            "timeout" => match arg.trim().parse::<u64>() {
                Ok(t) => {
                    let _ = db.execute(&format!("SET statement_timeout = '{t}s'"));
                    println!("statement timeout {t}s");
                }
                Err(_) => println!("timeout SECONDS"),
            },
            "run" | "explain" => {
                run_queries(db, dir, queries, &query_list(arg), cmd == "explain");
            }
            "sql" => match run(db, arg) {
                Ok((rows, took)) => {
                    println!("{:.3}s, {} rows", took.as_secs_f64(), rows.len());
                    rows.iter().take(30).for_each(|r| println!("  {r}"));
                }
                Err(e) => println!("FAILED: {e}"),
            },
            other => println!("unknown command {other}: run, explain, sql, timeout, quit"),
        }
        println!("done");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_match_to_the_digits_the_answer_prints() {
        assert!(rounded_equal("catalog channel 1 132885062 2050279.74", "catalog channel 1 132885061.65 2050279.74"));
        assert!(rounded_equal("20390161.4 -9133254.7", "20390161.37 -9133254.66"));
        assert!(!rounded_equal("132885062", "132885060.9"));
        assert!(!rounded_equal("2050279.74", "2050279.75"));
        assert!(!rounded_equal("a b", "a c"));
    }

    #[test]
    fn padding_before_a_comma_does_not_matter() {
        assert_eq!(row_key(&["Carter , Rodney"]), row_key(&["Carter, Rodney"]));
        assert_eq!(raw_row(&["Carter   , Rodney"]), "Carter, Rodney");
    }

    #[test]
    fn answer_files_print_nulls_as_dashes_and_percent_signs() {
        let text = "A B C\n- - -\nGreene - N\nx % y\n";
        let rows = answer_rows(text);
        assert_eq!(rows[0].0, row_key(&["Greene N"]));
        assert_eq!(rows[1].0, row_key(&["x y"]));
    }
}
