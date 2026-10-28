//! The Join Order Benchmark (Leis et al., "How good are query optimizers, really?") against Accessor: the 113 queries
//! over the IMDB snapshot of the benchmark, which this program does not contain. Download `imdb.tgz` (from
//! <https://event.cwi.nl/da/job/imdb.tgz>) and the queries (<https://github.com/gregrahn/join-order-benchmark>: `schema.sql`,
//! `fkindexes.sql` and `1a.sql` ... `33c.sql`), unpack both, and run
//!
//! `accessor-job --data DIR --queries DIR [--fraction 10] [--only 1a,6b] [--timeout 60] [--compare] [--no-fk-indexes] [--explain]`
//!
//! The whole snapshot (74 million rows, 3.6 GB of text) does not fit in memory with its indexes on most machines, so
//! `--fraction K` keeps one movie in `K` (chosen by a hash of its id) and every row that belongs to those movies, and the
//! people, characters, companies and keywords that those rows refer to. `--fraction 1` keeps everything. The answers
//! differ from the official ones, and the sizes of the intermediate results are smaller, so this is a check that the queries
//! run, and run quickly, not the benchmark's own figures. With `--compare` each answer is checked against the same query with
//! the optimizer off (queries that are too slow that way are reported, not failed).
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use accessor_core::{Database, DataType, Value};

/// One CSV record: `None` is an empty field that was not quoted (a NULL).
type Record = Vec<Option<String>>;

/// Reads PostgreSQL-style CSV with `ESCAPE '\'`, as the benchmark's files are written.
struct Records<R: Read> {
    input: BufReader<R>,
    peeked: Option<u8>,
}

impl<R: Read> Records<R> {
    fn new(r: R) -> Self {
        Records { input: BufReader::with_capacity(1 << 20, r), peeked: None }
    }

    fn byte(&mut self) -> Option<u8> {
        if let Some(b) = self.peeked.take() {
            return Some(b);
        }
        let mut b = [0u8; 1];
        match self.input.read(&mut b) {
            Ok(1) => Some(b[0]),
            _ => None,
        }
    }

    fn next_record(&mut self) -> Option<Record> {
        let mut rec: Record = Vec::new();
        let mut field: Vec<u8> = Vec::new();
        let (mut quoted, mut in_quote, mut any) = (false, false, false);
        loop {
            let Some(b) = self.byte() else {
                if !any && field.is_empty() && rec.is_empty() {
                    return None;
                }
                rec.push(finish(&mut field, quoted));
                return Some(rec);
            };
            any = true;
            if in_quote {
                match b {
                    b'\\' => match self.byte() {
                        Some(n @ (b'"' | b'\\')) => field.push(n),
                        Some(n) => {
                            field.push(b'\\');
                            field.push(n);
                        }
                        None => field.push(b'\\'),
                    },
                    b'"' => in_quote = false,
                    _ => field.push(b),
                }
            } else {
                match b {
                    b'"' => {
                        in_quote = true;
                        quoted = true;
                    }
                    b',' => {
                        rec.push(finish(&mut field, quoted));
                        quoted = false;
                    }
                    b'\n' => {
                        rec.push(finish(&mut field, quoted));
                        return Some(rec);
                    }
                    b'\r' => {}
                    _ => field.push(b),
                }
            }
        }
    }
}

fn finish(field: &mut Vec<u8>, quoted: bool) -> Option<String> {
    let text = String::from_utf8_lossy(field).into_owned();
    field.clear();
    if text.is_empty() && !quoted {
        None
    } else {
        Some(text)
    }
}

/// Which movie a row belongs to, and which ids of other tables its rows refer to (column positions).
struct Table {
    name: &'static str,
    /// The column holding the movie, for the tables that hang off `title`.
    movie: Option<usize>,
    /// `(column, set)`: ids this table's kept rows refer to, to be kept in the table that owns them.
    refers: &'static [(usize, &'static str)],
    /// For a table owned by another: the id column, and the set that says which rows are kept (`id` column 0 of the owner).
    owned_by: Option<(usize, &'static str)>,
}

const TABLES: &[Table] = &[
    Table { name: "kind_type", movie: None, refers: &[], owned_by: None },
    Table { name: "info_type", movie: None, refers: &[], owned_by: None },
    Table { name: "role_type", movie: None, refers: &[], owned_by: None },
    Table { name: "link_type", movie: None, refers: &[], owned_by: None },
    Table { name: "comp_cast_type", movie: None, refers: &[], owned_by: None },
    Table { name: "company_type", movie: None, refers: &[], owned_by: None },
    Table { name: "title", movie: Some(0), refers: &[], owned_by: None },
    Table { name: "aka_title", movie: Some(1), refers: &[], owned_by: None },
    Table { name: "complete_cast", movie: Some(1), refers: &[], owned_by: None },
    Table { name: "movie_link", movie: Some(1), refers: &[], owned_by: None },
    Table { name: "movie_info", movie: Some(1), refers: &[], owned_by: None },
    Table { name: "movie_info_idx", movie: Some(1), refers: &[], owned_by: None },
    Table { name: "movie_companies", movie: Some(1), refers: &[(2, "company")], owned_by: None },
    Table { name: "movie_keyword", movie: Some(1), refers: &[(2, "keyword")], owned_by: None },
    Table { name: "cast_info", movie: Some(2), refers: &[(1, "person"), (3, "char")], owned_by: None },
    Table { name: "company_name", movie: None, refers: &[], owned_by: Some((0, "company")) },
    Table { name: "keyword", movie: None, refers: &[], owned_by: Some((0, "keyword")) },
    Table { name: "char_name", movie: None, refers: &[], owned_by: Some((0, "char")) },
    Table { name: "name", movie: None, refers: &[], owned_by: Some((0, "person")) },
    Table { name: "aka_name", movie: None, refers: &[], owned_by: Some((1, "person")) },
    Table { name: "person_info", movie: None, refers: &[], owned_by: Some((1, "person")) },
];

fn kept(id: i64, fraction: i64) -> bool {
    fraction <= 1 || ((id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 20) % fraction as u64 == 0
}

fn load(db: &mut Database, dir: &Path, fraction: i64) -> std::io::Result<()> {
    let mut sets: std::collections::HashMap<&str, HashSet<i64>> = ["company", "keyword", "person", "char"].into_iter().map(|k| (k, HashSet::new())).collect();
    let mut movies: HashSet<i64> = HashSet::new();
    for t in TABLES {
        let started = Instant::now();
        let types: Vec<DataType> = db.table(t.name).unwrap_or_else(|| panic!("no table {}", t.name)).schema().columns.iter().map(|c| c.data_type).collect();
        let mut reader = Records::new(File::open(dir.join(format!("{}.csv", t.name)))?);
        db.begin().expect("begin");
        let (mut n, mut skipped) = (0u64, 0u64);
        while let Some(rec) = reader.next_record() {
            let id = |c: usize| rec.get(c).and_then(|f| f.as_ref()).and_then(|s| s.parse::<i64>().ok());
            if t.name == "title" {
                match id(0) {
                    Some(i) if kept(i, fraction) => {
                        movies.insert(i);
                    }
                    _ => {
                        skipped += 1;
                        continue;
                    }
                }
            } else if let Some(c) = t.movie {
                if fraction > 1 && !id(c).is_some_and(|m| movies.contains(&m)) {
                    skipped += 1;
                    continue;
                }
            } else if let (Some((c, set)), true) = (t.owned_by, fraction > 1) {
                if !id(c).is_some_and(|i| sets[set].contains(&i)) {
                    skipped += 1;
                    continue;
                }
            }
            for &(c, set) in t.refers {
                if let Some(i) = id(c) {
                    sets.get_mut(set).expect("set").insert(i);
                }
            }
            let values: Vec<(usize, Value)> = rec
                .iter()
                .enumerate()
                .take(types.len())
                .map(|(i, f)| {
                    let v = match f {
                        None => Value::Null,
                        Some(s) if types[i] == DataType::Integer => s.parse::<i64>().map_or(Value::Null, Value::Int),
                        Some(s) => Value::text(s),
                    };
                    (i, v)
                })
                .collect();
            if let Err(e) = db.insert_row(t.name, &values) {
                panic!("{}: row {}: {e}", t.name, n + skipped + 1);
            }
            n += 1;
            if n % 500_000 == 0 {
                db.commit().expect("commit");
                db.begin().expect("begin");
            }
        }
        db.commit().expect("commit");
        println!("  {:<16}{:>10} rows ({} left out) in {:.1}s", t.name, n, skipped, started.elapsed().as_secs_f64());
    }
    Ok(())
}

fn run(db: &mut Database, sql: &str) -> Result<(Vec<String>, Duration), String> {
    let t = Instant::now();
    let r = db.execute(sql).map_err(|e| e.to_string())?;
    Ok((r.rows.iter().map(|row| row.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" | ")).collect(), t.elapsed()))
}

fn main() {
    let (mut data, mut queries, mut fraction, mut timeout) = (PathBuf::new(), PathBuf::new(), 10i64, 60u64);
    let (mut only, mut compare, mut fk_indexes, mut explain) = (Vec::<String>::new(), false, true, false);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data" => data = it.next().expect("--data DIR").into(),
            "--queries" => queries = it.next().expect("--queries DIR").into(),
            "--fraction" => fraction = it.next().and_then(|v| v.parse().ok()).expect("--fraction N"),
            "--timeout" => timeout = it.next().and_then(|v| v.parse().ok()).expect("--timeout SECONDS"),
            "--only" => only = it.next().expect("--only 1a,2b").split(',').map(str::to_string).collect(),
            "--compare" => compare = true,
            "--no-fk-indexes" => fk_indexes = false,
            "--explain" => explain = true,
            other => panic!("unknown argument {other}"),
        }
    }
    let started = Instant::now();
    let mut db = Database::in_memory();
    let schema = std::fs::read_to_string(queries.join("schema.sql")).expect("schema.sql");
    db.execute(&schema).expect("the schema");
    println!("loading {} (one movie in {fraction}):", data.display());
    load(&mut db, &data, fraction).expect("the data");
    if fk_indexes {
        let t = Instant::now();
        let ix = std::fs::read_to_string(queries.join("fkindexes.sql")).expect("fkindexes.sql");
        db.execute(&ix).expect("the indexes");
        println!("indexes built in {:.1}s", t.elapsed().as_secs_f64());
    }
    println!("loaded in {:.1}s\n", started.elapsed().as_secs_f64());
    let _ = db.execute(&format!("SET statement_timeout = '{timeout}s'"));

    let mut files: Vec<PathBuf> = std::fs::read_dir(&queries)
        .expect("queries directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "sql") && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.starts_with(|c: char| c.is_ascii_digit())))
        .collect();
    let key = |p: &PathBuf| {
        let s = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
        (digits.parse::<u32>().unwrap_or(0), s)
    };
    files.sort_by_key(key);
    println!("{:>4} {:>6} {:>9}  {}", "Q", "rows", "seconds", "result");
    let (mut bad, mut slow, mut total, mut ran) = (0, 0, Duration::ZERO, 0);
    for f in files {
        let name = f.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if !only.is_empty() && !only.contains(&name) {
            continue;
        }
        let sql = std::fs::read_to_string(&f).expect("a query");
        if explain {
            match run(&mut db, &format!("EXPLAIN {sql}")) {
                Ok((plan, _)) => println!("{name}:\n{}", plan.join("\n")),
                Err(e) => println!("{name}: EXPLAIN failed: {e}"),
            }
            continue;
        }
        match run(&mut db, &sql) {
            Err(e) if e.contains("timeout") || e.contains("time limit") => {
                slow += 1;
                println!("{name:>4} {:>6} {:>9}  too slow (over {timeout}s)", "", "");
            }
            Err(e) => {
                bad += 1;
                println!("{name:>4} {:>6} {:>9}  FAILED: {e}", "", "");
            }
            Ok((rows, took)) => {
                ran += 1;
                total += took;
                let mut verdict = String::from("ok");
                if compare {
                    db.set_optimizer(false);
                    let other = run(&mut db, &sql);
                    db.set_optimizer(true);
                    verdict = match other {
                        Ok((other, _)) if other == rows => "same with the optimizer off".into(),
                        Ok((other, _)) => {
                            bad += 1;
                            format!("DIFFERENT with the optimizer off: {other:?} against {rows:?}")
                        }
                        Err(e) => format!("optimizer off: {}", if e.contains("timeout") || e.contains("time limit") { "too slow".to_string() } else { e }),
                    };
                }
                println!("{name:>4} {:>6} {:>9.3}  {verdict}", rows.len(), took.as_secs_f64());
            }
        }
    }
    if !explain {
        println!("\n{ran} queries ran in {:.1}s; {slow} over {timeout}s; {bad} failed or differed", total.as_secs_f64());
    }
    if bad > 0 {
        std::process::exit(1);
    }
}
