//! Runs SQLite's *sqllogictest* files against Accessor.
//!
//!   accessor-slt [--jobs N] [--timeout SECONDS] [--memory-mb N] [--show N] [--quiet] [--no-optimizer] PATH...
//!
//! A PATH is a `.test` file or a folder (searched for `.test` files). Each file gets a fresh in-memory
//! database. The format and the rules for printing values are those of the original runner: see
//! <https://www.sqlite.org/sqllogictest/doc/trunk/about.wiki>. Accessor counts as a database called
//! `accessor` for the `skipif` and `onlyif` lines.
//!
//! Each file runs in a child process of its own, under a hard limit on memory (a Windows job object; on
//! other systems use `ulimit -v`) and on time, so that a query that tries to use all the memory, or never
//! ends, cannot take the machine with it. A file whose child dies or is stopped is reported with the record it was
//! on (a KILLER), and is run again without that record, so that every such record is found.
//!
//! With `--jobs N`, N files run at once, each in a child of its own: the memory in use is at most N times
//! the limit. Prints one line per file, some of the failures, the reasons records failed, and totals. The exit code is 0
//! only if every record passed.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use accessor_core::{Database, Value};

const ENGINE: &str = "accessor";

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    No,
    Row,
    Value,
}

enum Expected {
    Values(Vec<String>),
    Hash(usize, String),
}

enum Record {
    Statement { ok: bool, sql: String, line: usize },
    Query { types: String, sort: Sort, sql: String, expected: Expected, line: usize },
    Halt,
}

fn parse(text: &str) -> Vec<Record> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut skip = false;
    while i < lines.len() {
        let line = lines[i].trim_end();
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.first().copied() {
            None => {
                skip = false;
                i += 1;
            }
            Some(w) if w.starts_with('#') => i += 1,
            Some("skipif") => {
                skip |= words.get(1) == Some(&ENGINE);
                i += 1;
            }
            Some("onlyif") => {
                skip |= words.get(1) != Some(&ENGINE);
                i += 1;
            }
            Some("halt") => {
                if !skip {
                    out.push(Record::Halt);
                }
                i += 1;
            }
            Some("hash-threshold") => i += 1,
            Some("statement") => {
                let start = i + 1;
                let mut j = start;
                while j < lines.len() && !lines[j].trim().is_empty() {
                    j += 1;
                }
                if !skip {
                    out.push(Record::Statement { ok: words.get(1) != Some(&"error"), sql: lines[start..j].join("\n"), line: i + 1 });
                }
                skip = false;
                i = j;
            }
            Some("query") => {
                let types = words.get(1).copied().unwrap_or("").to_string();
                let sort = match words.get(2).copied() {
                    Some("rowsort") => Sort::Row,
                    Some("valuesort") => Sort::Value,
                    _ => Sort::No,
                };
                let start = i + 1;
                let mut j = start;
                while j < lines.len() && lines[j].trim_end() != "----" {
                    j += 1;
                }
                let sql = lines[start..j.min(lines.len())].join("\n");
                let mut k = j + 1;
                let mut values = Vec::new();
                while k < lines.len() && !lines[k].trim().is_empty() {
                    values.push(lines[k].trim_end().to_string());
                    k += 1;
                }
                let expected = match values.as_slice() {
                    [one] if one.contains(" values hashing to ") => {
                        let (n, h) = one.split_once(" values hashing to ").expect("checked");
                        Expected::Hash(n.parse().unwrap_or(0), h.trim().to_string())
                    }
                    _ => Expected::Values(values),
                };
                if !skip {
                    out.push(Record::Query { types, sort, sql, expected, line: i + 1 });
                }
                skip = false;
                i = k;
            }
            Some(_) => i += 1,
        }
    }
    out
}

/// A value as the original runner prints it, for a column of the given type letter.
fn format_value(v: &Value, ty: char) -> String {
    if v.is_null() {
        return "NULL".into();
    }
    match ty {
        'I' => match v {
            Value::Int(i) => i.to_string(),
            Value::Big(b) => b.to_string(),
            Value::Bool(b) => i64::from(*b).to_string(),
            Value::Text(s) => {
                let t = s.trim_start();
                let end = t.char_indices().find(|(i, c)| !(c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+')))).map_or(t.len(), |(i, _)| i);
                t[..end].parse::<i64>().unwrap_or(0).to_string()
            }
            other => other.to_f64().map_or_else(|| other.to_string(), |f| format!("{}", f.trunc() as i64)),
        },
        'R' => match v.to_f64() {
            Some(f) => format!("{f:.3}"),
            None => v.to_string(),
        },
        _ => {
            let s = match v {
                Value::Text(s) => s.to_string(),
                other => other.to_string(),
            };
            if s.is_empty() {
                "(empty)".into()
            } else {
                s.chars().map(|c| if (' '..='~').contains(&c) { c } else { '@' }).collect()
            }
        }
    }
}

enum Outcome {
    Pass,
    Fail(String, String), // the reason (a short class, for the table) and the detail
}

fn short(s: &str, n: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > n {
        format!("{}...", one.chars().take(n).collect::<String>())
    } else {
        one
    }
}

/// An error message with the particulars taken out, so that like errors are counted together.
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
    short(&out, 90)
}

fn run_record(db: &mut Database, rec: &Record) -> Outcome {
    match rec {
        Record::Halt => Outcome::Pass,
        Record::Statement { ok, sql, .. } => match (db.execute(sql), ok) {
            (Ok(_), true) | (Err(_), false) => Outcome::Pass,
            (Err(e), true) => Outcome::Fail(format!("statement error: {}", class_of(&e.to_string())), format!("{} -> {e}", short(sql, 160))),
            (Ok(_), false) => Outcome::Fail("statement should have failed".into(), short(sql, 160)),
        },
        Record::Query { types, sort, sql, expected, .. } => {
            let result = match db.execute(sql) {
                Ok(r) => r,
                Err(e) => return Outcome::Fail(format!("query error: {}", class_of(&e.to_string())), format!("{} -> {e}", short(sql, 200))),
            };
            let tys: Vec<char> = types.chars().collect();
            let width = result.columns.len();
            if tys.len() != width {
                return Outcome::Fail("wrong number of columns".into(), format!("{} -> got {width} column(s), expected {}", short(sql, 160), tys.len()));
            }
            let mut rows: Vec<Vec<String>> = result.rows.iter().map(|r| r.iter().zip(&tys).map(|(v, t)| format_value(v, *t)).collect()).collect();
            let values: Vec<String> = match sort {
                Sort::Row => {
                    rows.sort();
                    rows.into_iter().flatten().collect()
                }
                Sort::Value => {
                    let mut all: Vec<String> = rows.into_iter().flatten().collect();
                    all.sort();
                    all
                }
                Sort::No => rows.into_iter().flatten().collect(),
            };
            match expected {
                Expected::Values(want) => {
                    if &values == want {
                        Outcome::Pass
                    } else {
                        let first = values.iter().zip(want).position(|(a, b)| a != b);
                        let why = if values.len() != want.len() { "wrong number of values" } else { "wrong values" };
                        let at = first.map_or(String::new(), |p| format!(" first difference at value {p}: got {:?}, expected {:?}", values[p], want[p]));
                        Outcome::Fail(why.into(), format!("{} -> got {} value(s), expected {}.{at}", short(sql, 200), values.len(), want.len()))
                    }
                }
                Expected::Hash(n, hash) => {
                    let mut text = String::new();
                    for v in &values {
                        text.push_str(v);
                        text.push('\n');
                    }
                    let got = format!("{:x}", md5::compute(text.as_bytes()));
                    if values.len() == *n && &got == hash {
                        Outcome::Pass
                    } else {
                        let why = if values.len() != *n { "wrong number of values" } else { "wrong values" };
                        Outcome::Fail(why.into(), format!("{} -> got {} value(s) hashing to {got}, expected {n} hashing to {hash}", short(sql, 200), values.len()))
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct FileReport {
    total: usize,
    passed: usize,
    reasons: BTreeMap<String, usize>,
    examples: Vec<(usize, String, String)>,
    halted: bool,
    panics: usize,
    seconds: f64,
}

/// The line number of the record being run is written here before each one, so that whoever started
/// this process can say where it was when it died.
struct Progress(Option<PathBuf>);

impl Progress {
    fn at(&self, line: usize) {
        if let Some(p) = &self.0 {
            let _ = std::fs::write(p, line.to_string());
        }
    }
}

fn record_line(rec: &Record) -> usize {
    match rec {
        Record::Statement { line, .. } | Record::Query { line, .. } => *line,
        Record::Halt => 0,
    }
}

fn run_file(path: &Path, show: usize, skip: &[usize], progress: &Progress, optimizer: bool) -> FileReport {
    let started = Instant::now();
    let text = String::from_utf8_lossy(&std::fs::read(path).expect("readable file")).into_owned();
    let records = parse(&text);
    let mut db = Database::in_memory();
    db.set_optimizer(optimizer);
    let mut report = FileReport::default();
    let mut shown_per_reason: BTreeMap<String, usize> = BTreeMap::new();
    for rec in &records {
        if matches!(rec, Record::Halt) {
            report.halted = true;
            break;
        }
        let line = record_line(rec);
        if skip.contains(&line) {
            continue; // known to kill the process: reported by whoever skipped it
        }
        report.total += 1;
        progress.at(line);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_record(&mut db, rec))).unwrap_or_else(|_| {
            report.panics += 1;
            Outcome::Fail("PANIC".into(), "the engine panicked".into())
        });
        match outcome {
            Outcome::Pass => report.passed += 1,
            Outcome::Fail(reason, detail) => {
                *report.reasons.entry(reason.clone()).or_insert(0) += 1;
                let n = shown_per_reason.entry(reason.clone()).or_insert(0);
                if *n < show {
                    *n += 1;
                    report.examples.push((line, reason, detail));
                }
            }
        }
    }
    report.seconds = started.elapsed().as_secs_f64();
    report
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path).expect("readable folder").filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for e in entries {
            collect(&e, out);
        }
    } else if path.extension().is_some_and(|e| e == "test") {
        out.push(path.to_path_buf());
    }
}

fn clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// The child: runs one file and reports on standard output, one tab-separated line per fact.
fn child(args: &[String]) {
    let mut path = None;
    let (mut show, mut skip, mut progress, mut optimizer) = (2usize, Vec::new(), Progress(None), true);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--show" => show = it.next().and_then(|v| v.parse().ok()).unwrap_or(2),
            "--skip" => skip = it.next().map(|v| v.split(',').filter_map(|n| n.parse().ok()).collect()).unwrap_or_default(),
            "--progress" => progress = Progress(it.next().map(PathBuf::from)),
            "--no-optimizer" => optimizer = false,
            other => path = Some(PathBuf::from(other)),
        }
    }
    std::panic::set_hook(Box::new(|_| {}));
    let r = run_file(&path.expect("a file"), show, &skip, &progress, optimizer);
    println!("R\t{}\t{}\t{}\t{:.3}\t{}", r.total, r.passed, r.panics, r.seconds, u8::from(r.halted));
    for (reason, n) in &r.reasons {
        println!("W\t{n}\t{}", clean(reason));
    }
    for (line, reason, detail) in &r.examples {
        println!("E\t{line}\t{}\t{}", clean(reason), clean(detail));
    }
}

// ----- the parent ------------------------------------------------------------------------------------

/// Puts a process under a hard limit on the memory it may use; it is killed by the system, with the rest of
/// the machine untouched, if it asks for more.
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
        // (the job is left open: it ends with this process, taking the child with it)
    }
}

#[cfg(not(windows))]
fn limit_memory(_child: &std::process::Child, _megabytes: usize) {
    // Run under `ulimit -v` instead.
}

enum Ended {
    Finished(String),
    Died(String), // how
    TimedOut,
}

fn run_child(exe: &Path, file: &Path, opts: &Opts, skip: &[usize], progress: &Path) -> Ended {
    use std::process::{Command, Stdio};
    let _ = std::fs::write(progress, "0");
    let mut cmd = Command::new(exe);
    cmd.arg("--child").arg(file).arg("--show").arg(opts.show.to_string()).arg("--progress").arg(progress);
    if !opts.optimizer {
        cmd.arg("--no-optimizer");
    }
    if !skip.is_empty() {
        cmd.arg("--skip").arg(skip.iter().map(ToString::to_string).collect::<Vec<_>>().join(","));
    }
    let out_path = progress.with_extension("out");
    let out = std::fs::File::create(&out_path).expect("temporary file");
    let mut child = cmd.stdout(Stdio::from(out)).stderr(Stdio::null()).spawn().expect("child process");
    limit_memory(&child, opts.memory_mb);
    let deadline = Instant::now() + Duration::from_secs(opts.timeout);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = std::fs::read_to_string(&out_path).unwrap_or_default();
                return if status.success() && text.lines().any(|l| l.starts_with("R\t")) {
                    Ended::Finished(text)
                } else {
                    Ended::Died(format!("{status}"))
                };
            }
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Ended::TimedOut;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Ended::Died(e.to_string()),
        }
    }
}

struct Opts {
    timeout: u64,
    memory_mb: usize,
    show: usize,
    quiet: bool,
    optimizer: bool,
    jobs: usize,
}

fn sql_at(file: &Path, line: usize) -> String {
    let text = String::from_utf8_lossy(&std::fs::read(file).unwrap_or_default()).into_owned();
    for rec in parse(&text) {
        match rec {
            Record::Statement { sql, line: l, .. } | Record::Query { sql, line: l, .. } if l == line => return short(&sql, 700),
            _ => {}
        }
    }
    "?".into()
}

/// What running one file came to.
#[derive(Default)]
struct Done {
    text: String,
    total: usize,
    passed: usize,
    panics: usize,
    clean: bool,
    reasons: BTreeMap<String, usize>,
    killers: Vec<(String, usize, String, String)>, // file, line, how, sql
}

/// Runs one file in child processes (more than one if records kill them), and reports on it.
fn process(file: &Path, exe: &Path, opts: &Opts, progress: &Path) -> Done {
    let name = file.display().to_string().replace('\\', "/");
    let name = name.split_once("/test/").map_or(name.clone(), |(_, rest)| rest.to_string());
    let mut done = Done::default();
    let mut skip: Vec<usize> = Vec::new();
    let mut file_killers = 0;
    loop {
        match run_child(exe, file, opts, &skip, progress) {
            Ended::Finished(text) => {
                let (mut r_total, mut r_passed, mut r_panics, mut secs, mut halted) = (0, 0, 0, 0.0, false);
                let mut examples = String::new();
                for l in text.lines() {
                    let f: Vec<&str> = l.splitn(4, '\t').collect();
                    match f[0] {
                        "R" => {
                            let g: Vec<&str> = l.split('\t').collect();
                            r_total = g[1].parse().unwrap_or(0);
                            r_passed = g[2].parse().unwrap_or(0);
                            r_panics = g[3].parse().unwrap_or(0);
                            secs = g[4].parse().unwrap_or(0.0);
                            halted = g[5] == "1";
                        }
                        "W" => *done.reasons.entry(f[2].to_string()).or_insert(0) += f[1].parse::<usize>().unwrap_or(0),
                        "E" => {
                            let _ = writeln!(examples, "      line {}: {}: {}", f[1], f[2], f.get(3).unwrap_or(&""));
                        }
                        _ => {}
                    }
                }
                done.total += r_total;
                done.passed += r_passed;
                done.panics += r_panics;
                done.clean = r_passed == r_total && file_killers == 0;
                if !(done.clean && opts.quiet) {
                    let _ = writeln!(
                        done.text,
                        "{} {name}: {r_passed}/{r_total} passed ({secs:.1}s){}{}",
                        if done.clean { "ok  " } else { "FAIL" },
                        if halted { " [halt]" } else { "" },
                        if file_killers > 0 { format!(" [{file_killers} record(s) killed the process and were skipped]") } else { String::new() }
                    );
                }
                if opts.show > 0 {
                    done.text.push_str(&examples);
                }
                return done;
            }
            dead => {
                let line: usize = std::fs::read_to_string(progress).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0);
                let how = match dead {
                    Ended::TimedOut => format!("ran for more than {}s", opts.timeout),
                    Ended::Died(h) => format!("the process ended abnormally ({h}); the memory limit is {} MB", opts.memory_mb),
                    Ended::Finished(_) => unreachable!(),
                };
                let sql = sql_at(file, line);
                let _ = writeln!(done.text, "KILLER {name} line {line}: {how}\n      {sql}");
                done.killers.push((name.clone(), line, how, sql));
                file_killers += 1;
                done.total += 1; // it counts as a failed record
                if line == 0 || skip.contains(&line) || file_killers > 25 {
                    let _ = writeln!(done.text, "      (giving up on this file)");
                    return done;
                }
                skip.push(line);
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--child") {
        child(&args[1..]);
        return;
    }
    let mut opts = Opts { timeout: 120, memory_mb: 2048, show: 2, quiet: false, optimizer: true, jobs: 1 };
    let mut paths = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--timeout" => opts.timeout = it.next().and_then(|v| v.parse().ok()).expect("--timeout SECONDS"),
            "--memory-mb" => opts.memory_mb = it.next().and_then(|v| v.parse().ok()).expect("--memory-mb N"),
            "--show" => opts.show = it.next().and_then(|v| v.parse().ok()).expect("--show N"),
            "--jobs" => opts.jobs = it.next().and_then(|v| v.parse().ok()).filter(|j| *j >= 1).expect("--jobs N"),
            "--quiet" => opts.quiet = true,
            "--no-optimizer" => opts.optimizer = false,
            _ => paths.push(PathBuf::from(a)),
        }
    }
    if paths.is_empty() {
        eprintln!("usage: accessor-slt [--jobs N] [--timeout SECONDS] [--memory-mb N] [--show N] [--quiet] [--no-optimizer] PATH...");
        std::process::exit(2);
    }
    let mut files = Vec::new();
    for p in &paths {
        collect(p, &mut files);
    }
    let exe = std::env::current_exe().expect("own path");
    let scratch = std::env::temp_dir().join(format!("accessor-slt-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("temporary folder");
    let jobs = opts.jobs.min(files.len().max(1));
    if jobs > 1 {
        println!("{} file(s), {jobs} at a time, each child limited to {} MB (at most {} MB in all)", files.len(), opts.memory_mb, jobs * opts.memory_mb);
    }

    let started = Instant::now();
    let next = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let results: Mutex<Vec<Done>> = Mutex::new(Vec::new());
    let print = Mutex::new(());
    std::thread::scope(|s| {
        for worker in 0..jobs {
            let (files, exe, opts, scratch, next, finished, results, print) = (&files, &exe, &opts, &scratch, &next, &finished, &results, &print);
            s.spawn(move || {
                let progress = scratch.join(format!("progress-{worker}"));
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(file) = files.get(i) else { break };
                    let done = process(file, exe, opts, &progress);
                    let n = finished.fetch_add(1, Ordering::SeqCst) + 1;
                    {
                        let _guard = print.lock().unwrap_or_else(|e| e.into_inner());
                        print!("{}", done.text);
                        if jobs > 1 && n % 25 == 0 {
                            println!("[{n}/{} files, {:.0}s]", files.len(), started.elapsed().as_secs_f64());
                        }
                    }
                    results.lock().unwrap_or_else(|e| e.into_inner()).push(done);
                }
            });
        }
    });
    let _ = std::fs::remove_dir_all(&scratch);

    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    let (mut total, mut passed, mut panics, mut files_clean) = (0usize, 0usize, 0usize, 0usize);
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut killers = 0;
    for d in &results {
        total += d.total;
        passed += d.passed;
        panics += d.panics;
        files_clean += usize::from(d.clean);
        killers += d.killers.len();
        for (k, v) in &d.reasons {
            *reasons.entry(k.clone()).or_insert(0) += v;
        }
    }
    println!();
    println!(
        "{} file(s), {} clean; {passed}/{total} records passed ({:.2}%), {killers} killed their process, {panics} panic(s), {:.0}s",
        files.len(),
        files_clean,
        100.0 * passed as f64 / total.max(1) as f64,
        started.elapsed().as_secs_f64()
    );
    if !reasons.is_empty() {
        println!("\nwhy records failed:");
        let mut sorted: Vec<_> = reasons.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        for (reason, n) in sorted.iter().take(40) {
            println!("{n:>9}  {reason}");
        }
    }
    std::process::exit(i32::from(passed != total));
}
