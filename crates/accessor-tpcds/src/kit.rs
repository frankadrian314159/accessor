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
            // a character the answer file lost (it prints U+FFFD for the O of CÔTE D'IVOIRE) matches whatever stands there
            _ if x.contains('\u{FFFD}') => x.chars().filter(char::is_ascii).eq(y.chars().filter(char::is_ascii)),
            _ => x == y,
        })
}

/// Whether `l` is the dashed line under a header (fixed-width and tab-padded files), or a line of `|` and dashes.
fn dashes(l: &str) -> bool {
    l.contains('-') && l.chars().all(|c| matches!(c, '-' | ' ' | '\t' | '|'))
}

/// A line that is no part of the answer: what a client prints around a result.
fn junk(l: &str) -> bool {
    let t = l.trim();
    (t.starts_with('(') && (t.ends_with(" rows)") || t.ends_with(" row)") || t.ends_with(" rows affected)")))
        || t.starts_with("Warning:")
        || (t.starts_with("-----") && t.contains("OUTPUT"))
        || t.starts_with("Elapsed:")
        || t.starts_with("SQL>")
        || t.contains("Oracle Database")
        || t.contains("Real Application Testing")
        || t.starts_with("With the Partitioning")
        || t.contains("Real Application Clusters")
        || t.ends_with("record(s) selected.")
        || t.ends_with("rows selected.")
        || t.ends_with("row selected.")
}

/// Whether a line of a `|` file is a header: every value is a name in capitals (`BRAND1`), none a number or lower case text.
fn pipe_header(l: &str) -> bool {
    let mut names = l.split(['|', ' ', '\t']).filter(|t| !t.is_empty()).peekable();
    names.peek().is_some() && l.contains('|') && names.all(|t| t.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !t.chars().any(char::is_lowercase))
}

/// The rows of an answer set: after the header and its dashed line. The files are fixed-width text, tab-padded text, or
/// split by `|`; a NULL is an empty field, a lone `%` (query 44), `-` (query 73) or `[NULL]` (query 64). A query with several
/// statements has a header (and dashes) for each result set, and a header too wide for the line is split in two blocks, each
/// followed by dashes, with every row split likewise (query 17): the lines of a row are joined.
fn answer_rows(text: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty() && !junk(l)).collect();
    let mut rows = Vec::new();
    let mut wrap = 1;
    let mut pending: Vec<&str> = Vec::new();
    let mut seen_header = false;
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        // a header: the lines before dashes (one block, or several when the header was wrapped)
        if i + 1 < lines.len() && !dashes(l) && dashes(lines[i + 1]) {
            let mut blocks = 0;
            let mut k = i;
            while k + 1 < lines.len() && !dashes(lines[k]) && dashes(lines[k + 1]) {
                blocks += 1;
                k += 2;
            }
            wrap = blocks;
            pending.clear();
            seen_header = true;
            i = k;
            continue;
        }
        if dashes(l) {
            i += 1;
            continue;
        }
        // the first line of a file of `|`-separated values is its header, and so is a later line that looks like one
        if !seen_header || pipe_header(l) {
            if l.contains('|') {
                seen_header = true;
                pending.clear();
                i += 1;
                continue;
            }
        }
        if !seen_header {
            i += 1;
            continue;
        }
        pending.push(l);
        i += 1;
        if pending.len() < wrap {
            continue;
        }
        let joined = pending.join(" ");
        pending.clear();
        let spaced = joined.replace('|', " ");
        let line: Vec<&str> = spaced.split_whitespace().map(|t| if matches!(t, "%" | "-" | "[NULL]") { "NULL" } else { t }).collect();
        rows.push((row_key(&[&line.join(" ")]), raw_row(&[&line.join(" ")])));
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
    let mut best_missing: Option<usize> = None;
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
            eprintln!("  {name}:");
            for w in want.iter().filter(|w| !mine.contains(w)).take(3) {
                eprintln!("  want only: {w}");
            }
            for m in mine.iter().filter(|m| !want.contains(m)).take(3) {
                eprintln!("  mine only: {m}");
            }
        }
        let missing = want.iter().filter(|w| !mine.contains(w)).count();
        if best_missing.is_none_or(|m| missing < m) {
            best_missing = Some(missing);
            best = format!("DIFFERENT from {name}: {} rows against {}, {missing} of the answers not found", rows.len(), want.len());
        }
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
    fn answer_files_with_several_result_sets_wrapped_headers_and_client_noise() {
        // two result sets, each with its header; Oracle's trailer; a NULL printed as [NULL]
        let text = "A B\n- -\nx 1\n\n1 rows selected.\nElapsed: 00:00:01.00\nC D E\n- - -\ny [NULL] 2\nSQL> Disconnected from Oracle Database 12c\n";
        let rows = answer_rows(text);
        assert_eq!(rows.iter().map(|r| r.1.as_str()).collect::<Vec<_>>(), ["x 1", "y 2"]);
        // the banner between the result sets of a query with several statements (query 24)
        let banner = "---------------------- OUTPUT Query 1\n\nA B\n- -\nx 1\n\n---------------------- OUTPUT Query 2\n\nC\n-\ny\n";
        assert_eq!(answer_rows(banner).iter().map(|r| r.1.as_str()).collect::<Vec<_>>(), ["x 1", "y"]);
        // a header split in two blocks: every row is two lines
        let wrapped = "H1 H2\n-- --\nH3\n--\nr1 1\n  2\nr2 3\n  4\n";
        let rows = answer_rows(wrapped);
        assert_eq!(rows.iter().map(|r| r.1.as_str()).collect::<Vec<_>>(), ["r1 1 2", "r2 3 4"]);
        // `|` files: the header is the first line, and a later one in capitals is a header too
        let piped = "CHANNEL|ID\ncatalog|1\nCHANN|ID2\nweb|2\n";
        let rows = answer_rows(piped);
        assert_eq!(rows.iter().map(|r| r.1.as_str()).collect::<Vec<_>>(), ["catalog 1", "web 2"]);
    }

    #[test]
    fn a_lost_character_in_an_answer_matches_the_real_one() {
        assert!(rounded_equal("C\u{FFFD}TE D'IVOIRE 5", "C\u{d4}TE D'IVOIRE 5"));
        assert!(!rounded_equal("C\u{FFFD}TE", "COTE X"));
    }

    #[test]
    fn answer_files_print_nulls_as_dashes_and_percent_signs() {
        let text = "A B C\n- - -\nGreene - N\nx % y\n";
        let rows = answer_rows(text);
        assert_eq!(rows[0].0, row_key(&["Greene N"]));
        assert_eq!(rows[1].0, row_key(&["x y"]));
    }
}
