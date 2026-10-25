//! A rough timing of common operations; run with --release. Compare builds, or set ACCESSOR_FORCE_MVCC=1.
use std::time::Instant;

use accessor_core::Database;

fn time<R>(what: &str, f: impl FnOnce() -> R) -> R {
    let t = Instant::now();
    let r = f();
    println!("{what:<34} {:>8.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    r
}

fn main() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, g INTEGER, s TEXT)").unwrap();
    db.execute("CREATE INDEX t_g ON t (g)").unwrap();
    time("insert 200k rows (one statement)", || {
        db.execute("INSERT INTO t SELECT n, n % 100, 'row ' || n FROM GENERATE_SERIES(1, 200000) AS g (n)").unwrap()
    });
    time("20k autocommit inserts", || {
        for i in 0..20_000 {
            db.execute(&format!("INSERT INTO t VALUES ({}, 1, 'x')", 300_000 + i)).unwrap();
        }
    });
    time("20k point lookups (primary key)", || {
        for i in 0..20_000 {
            db.query(&format!("SELECT s FROM t WHERE id = {}", 1 + i * 9)).unwrap();
        }
    });
    time("500 lookups by indexed group", || {
        for i in 0..500 {
            db.query(&format!("SELECT COUNT(*) FROM t WHERE g = {}", i % 100)).unwrap();
        }
    });
    time("20 full-scan aggregates", || {
        for _ in 0..20 {
            db.query("SELECT SUM(g), COUNT(*) FROM t WHERE s LIKE '%7%'").unwrap();
        }
    });
    time("update every row", || db.execute("UPDATE t SET g = g + 1").unwrap());
    time("20k autocommit updates", || {
        for i in 0..20_000 {
            db.execute(&format!("UPDATE t SET g = 0 WHERE id = {}", 1 + i * 9)).unwrap();
        }
    });
    time("delete a tenth", || db.execute("DELETE FROM t WHERE id % 10 = 0").unwrap());
    time("1000 txns of 20 inserts", || {
        for k in 0..1000 {
            db.execute("BEGIN").unwrap();
            for j in 0..20 {
                db.execute(&format!("INSERT INTO t VALUES ({}, 2, 'y')", 1_000_000 + k * 20 + j)).unwrap();
            }
            db.execute("COMMIT").unwrap();
        }
    });
}
