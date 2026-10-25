//! Overlapping transactions: snapshots, conflicts, constraints, cleaning up, recovery.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use accessor_core::{Connection, Error, SharedDatabase, Value};

fn db_with(sql: &str) -> SharedDatabase {
    let db = SharedDatabase::in_memory();
    db.connect().execute(sql).unwrap();
    db
}

fn counters() -> SharedDatabase {
    db_with("CREATE TABLE c (id INTEGER PRIMARY KEY, n INTEGER); INSERT INTO c VALUES (1, 0), (2, 0)")
}

fn int(c: &Connection, sql: &str) -> i64 {
    match c.execute(sql).unwrap().rows[0][0] {
        Value::Int(i) => i,
        ref v => panic!("{sql}: not an integer: {v:?}"),
    }
}

fn n(c: &Connection, id: i64) -> i64 {
    int(c, &format!("SELECT n FROM c WHERE id = {id}"))
}

fn quick(db: &SharedDatabase) {
    db.set_busy_timeout(Some(Duration::from_millis(80)));
}

fn is_conflict<T: std::fmt::Debug>(r: Result<T, Error>) -> bool {
    matches!(r, Err(Error::Conflict(_)))
}

// ----- what a transaction sees ------------------------------------------------------------------

#[test]
fn uncommitted_changes_are_seen_by_nobody_else_and_nobody_waits() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    a.execute("UPDATE c SET n = 5 WHERE id = 1").unwrap();
    a.execute("INSERT INTO c VALUES (3, 7)").unwrap();
    a.execute("DELETE FROM c WHERE id = 2").unwrap();
    assert_eq!((n(&a, 1), int(&a, "SELECT COUNT(*) FROM c")), (5, 2), "the writer sees its own changes");
    // Another connection reads at once, and sees none of it, whatever it asks.
    assert_eq!(n(&b, 1), 0);
    assert_eq!(int(&b, "SELECT COUNT(*) FROM c"), 2);
    assert_eq!(int(&b, "SELECT COUNT(*) FROM c WHERE id = 3"), 0);
    assert_eq!(n(&b, 2), 0);
    // And it can write other rows meanwhile.
    b.execute("INSERT INTO c VALUES (4, 1)").unwrap();
    a.execute("COMMIT").unwrap();
    assert_eq!((n(&b, 1), int(&b, "SELECT COUNT(*) FROM c")), (5, 3));
}

#[test]
fn a_transaction_keeps_seeing_the_database_as_it_began() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    assert_eq!(int(&a, "SELECT COUNT(*) FROM c"), 2);
    b.execute("INSERT INTO c VALUES (3, 0)").unwrap();
    b.execute("UPDATE c SET n = 9 WHERE id = 1").unwrap();
    b.execute("DELETE FROM c WHERE id = 2").unwrap();
    assert_eq!(int(&a, "SELECT COUNT(*) FROM c"), 2, "no new rows appear");
    assert_eq!(n(&a, 1), 0, "no row changes under it");
    assert_eq!(n(&a, 2), 0, "no row disappears");
    assert_eq!(int(&a, "SELECT SUM(n) FROM c"), 0);
    a.execute("COMMIT").unwrap();
    assert_eq!((int(&a, "SELECT COUNT(*) FROM c"), n(&a, 1)), (2, 9));
}

#[test]
fn indexes_find_the_version_the_reader_can_see() {
    let db = db_with("CREATE TABLE p (id INTEGER PRIMARY KEY, name TEXT); CREATE UNIQUE INDEX p_name ON p (name); INSERT INTO p VALUES (1, 'old'), (2, 'other')");
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    a.execute("UPDATE p SET name = 'new' WHERE id = 1").unwrap();
    // A reader asking by the old key finds the row; by the new key, nothing yet.
    assert_eq!(int(&b, "SELECT COUNT(*) FROM p WHERE name = 'old'"), 1);
    assert_eq!(int(&b, "SELECT COUNT(*) FROM p WHERE name = 'new'"), 0);
    assert_eq!(int(&a, "SELECT COUNT(*) FROM p WHERE name = 'old'"), 0);
    assert_eq!(int(&a, "SELECT id FROM p WHERE name = 'new'"), 1);
    a.execute("COMMIT").unwrap();
    assert_eq!(int(&b, "SELECT COUNT(*) FROM p WHERE name = 'old'"), 0);
    assert_eq!(int(&b, "SELECT id FROM p WHERE name = 'new'"), 1);
    // The same through a rollback.
    a.execute("BEGIN").unwrap();
    a.execute("UPDATE p SET name = 'again' WHERE id = 1").unwrap();
    a.execute("ROLLBACK").unwrap();
    assert_eq!(int(&b, "SELECT id FROM p WHERE name = 'new'"), 1);
    assert_eq!(int(&b, "SELECT COUNT(*) FROM p WHERE name = 'again'"), 0);
    // A range and an ordered read see one version of each row, once.
    assert_eq!(int(&b, "SELECT COUNT(*) FROM p WHERE name >= 'a'"), 2);
    assert_eq!(b.execute("SELECT name FROM p ORDER BY name").unwrap().rows.len(), 2);
}

// ----- writers that collide ----------------------------------------------------------------------

#[test]
fn the_first_to_commit_wins_and_the_other_is_told() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    assert_eq!(n(&a, 1), 0);
    b.execute("UPDATE c SET n = 1 WHERE id = 1").unwrap(); // commits at once
    assert!(is_conflict(a.execute("UPDATE c SET n = 100 WHERE id = 1")), "a lost update must not happen");
    assert!(a.in_transaction(), "the transaction stays open; the statement is undone");
    a.execute("UPDATE c SET n = 5 WHERE id = 2").unwrap();
    a.execute("ROLLBACK").unwrap();
    assert_eq!((n(&b, 1), n(&b, 2)), (1, 0));
    // Deleting a row that changed is refused the same way.
    a.execute("BEGIN").unwrap();
    assert_eq!(n(&a, 2), 0);
    b.execute("UPDATE c SET n = 2 WHERE id = 2").unwrap();
    assert!(is_conflict(a.execute("DELETE FROM c WHERE id = 2")));
    a.execute("ROLLBACK").unwrap();
}

#[test]
fn a_writer_waits_for_an_unfinished_change_and_goes_on_if_it_is_taken_back() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    a.execute("UPDATE c SET n = 5 WHERE id = 1").unwrap();
    let after = thread::scope(|s| {
        let h = s.spawn(move || {
            b.execute("UPDATE c SET n = n + 10 WHERE id = 1").unwrap();
            n(&b, 1)
        });
        thread::sleep(Duration::from_millis(100));
        a.execute("ROLLBACK").unwrap();
        h.join().unwrap()
    });
    assert_eq!(after, 10, "b waited, then worked on the row as it had been");
}

#[test]
fn a_writer_waiting_for_a_change_that_commits_is_a_conflict_inside_a_transaction() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    b.execute("BEGIN").unwrap();
    assert_eq!(n(&b, 1), 0);
    a.execute("BEGIN").unwrap();
    a.execute("UPDATE c SET n = 5 WHERE id = 1").unwrap();
    let result = thread::scope(|s| {
        let h = s.spawn(move || {
            let r = b.execute("UPDATE c SET n = 7 WHERE id = 1");
            (r, b)
        });
        thread::sleep(Duration::from_millis(100));
        a.execute("COMMIT").unwrap();
        h.join().unwrap()
    });
    assert!(is_conflict(result.0), "b began before a committed");
    // A statement outside a transaction starts afresh and just works.
    let c = db.connect();
    c.execute("UPDATE c SET n = 8 WHERE id = 1").unwrap();
    assert_eq!(n(&c, 1), 8);
}

#[test]
fn waiting_does_not_last_for_ever() {
    let db = counters();
    quick(&db);
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    a.execute("UPDATE c SET n = 5 WHERE id = 1").unwrap();
    let started = std::time::Instant::now();
    assert!(is_conflict(b.execute("UPDATE c SET n = 6 WHERE id = 1")));
    assert!(started.elapsed() < Duration::from_secs(5));
    // Other rows were never in the way.
    b.execute("UPDATE c SET n = 6 WHERE id = 2").unwrap();
    a.execute("COMMIT").unwrap();
    assert_eq!((n(&b, 1), n(&b, 2)), (5, 6));
}

#[test]
fn two_transactions_adding_the_same_key_cannot_both_succeed() {
    let db = counters();
    quick(&db);
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    a.execute("INSERT INTO c VALUES (9, 1)").unwrap();
    assert!(is_conflict(b.execute("INSERT INTO c VALUES (9, 2)")), "it depends on how a ends, so b waits and gives up");
    a.execute("COMMIT").unwrap();
    // Now it is plainly a duplicate.
    assert!(matches!(b.execute("INSERT INTO c VALUES (9, 2)"), Err(Error::Constraint(_))));
    // A key removed by a committed transaction is free again.
    b.execute("DELETE FROM c WHERE id = 9").unwrap();
    a.execute("INSERT INTO c VALUES (9, 3)").unwrap();
    // A key one transaction frees and another wants: wait, then take it.
    a.execute("BEGIN").unwrap();
    a.execute("DELETE FROM c WHERE id = 9").unwrap();
    assert!(is_conflict(b.execute("INSERT INTO c VALUES (9, 4)")));
    a.execute("COMMIT").unwrap();
    b.execute("INSERT INTO c VALUES (9, 4)").unwrap();
}

#[test]
fn a_child_and_its_parent_cannot_be_added_and_removed_at_once() {
    let db = db_with(
        "CREATE TABLE parent (id INTEGER PRIMARY KEY); CREATE TABLE child (id INTEGER PRIMARY KEY, p INTEGER REFERENCES parent (id));
         INSERT INTO parent VALUES (1), (2)",
    );
    quick(&db);
    let (a, b) = (db.connect(), db.connect());
    // A child is added; the parent cannot be deleted meanwhile, or afterwards.
    a.execute("BEGIN").unwrap();
    a.execute("INSERT INTO child VALUES (1, 1)").unwrap();
    assert!(is_conflict(b.execute("DELETE FROM parent WHERE id = 1")));
    a.execute("COMMIT").unwrap();
    assert!(matches!(b.execute("DELETE FROM parent WHERE id = 1"), Err(Error::Constraint(_))));
    // The parent is being deleted; a child cannot be added meanwhile, or afterwards.
    b.execute("BEGIN").unwrap();
    b.execute("DELETE FROM parent WHERE id = 2").unwrap();
    assert!(is_conflict(a.execute("INSERT INTO child VALUES (2, 2)")));
    b.execute("COMMIT").unwrap();
    assert!(matches!(a.execute("INSERT INTO child VALUES (2, 2)"), Err(Error::Constraint(_))));
    // A parent added by a transaction that has not committed cannot be relied on.
    b.execute("BEGIN").unwrap();
    b.execute("INSERT INTO parent VALUES (3)").unwrap();
    assert!(is_conflict(a.execute("INSERT INTO child VALUES (3, 3)")));
    b.execute("COMMIT").unwrap();
    a.execute("INSERT INTO child VALUES (3, 3)").unwrap();
}

#[test]
fn cascades_follow_the_newest_state() {
    let db = db_with(
        "CREATE TABLE parent (id INTEGER PRIMARY KEY); CREATE TABLE child (id INTEGER PRIMARY KEY, p INTEGER REFERENCES parent (id) ON DELETE CASCADE);
         INSERT INTO parent VALUES (1); INSERT INTO child VALUES (1, 1)",
    );
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    assert_eq!(int(&a, "SELECT COUNT(*) FROM child"), 1);
    b.execute("INSERT INTO child VALUES (2, 1)").unwrap(); // committed after a's snapshot
    // The cascade has to remove a child a cannot see: refused, not silently skipped.
    assert!(is_conflict(a.execute("DELETE FROM parent WHERE id = 1")));
    a.execute("ROLLBACK").unwrap();
    b.execute("DELETE FROM parent WHERE id = 1").unwrap();
    assert_eq!(int(&b, "SELECT COUNT(*) FROM child"), 0);
}

// ----- anomalies: which are prevented, which are not ----------------------------------------------

#[test]
fn write_skew_is_possible_under_snapshot_isolation() {
    // Two on call; each transaction checks that another is on call before going off. Both do.
    let db = db_with("CREATE TABLE doc (id INTEGER PRIMARY KEY, on_call INTEGER); INSERT INTO doc VALUES (1, 1), (2, 1)");
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    b.execute("BEGIN").unwrap();
    assert_eq!(int(&a, "SELECT SUM(on_call) FROM doc"), 2);
    assert_eq!(int(&b, "SELECT SUM(on_call) FROM doc"), 2);
    a.execute("UPDATE doc SET on_call = 0 WHERE id = 1").unwrap();
    b.execute("UPDATE doc SET on_call = 0 WHERE id = 2").unwrap();
    a.execute("COMMIT").unwrap();
    b.execute("COMMIT").unwrap();
    assert_eq!(int(&a, "SELECT SUM(on_call) FROM doc"), 0, "documented: snapshot isolation does not prevent this");
}

#[test]
fn a_transfer_between_accounts_is_never_seen_half_done() {
    let db = db_with("CREATE TABLE acct (id INTEGER PRIMARY KEY, bal INTEGER)");
    db.connect()
        .execute(&format!("INSERT INTO acct VALUES {}", (0..10).map(|i| format!("({i}, 100)")).collect::<Vec<_>>().join(", ")))
        .unwrap();
    let stop = AtomicBool::new(false);
    let reads = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    thread::scope(|s| {
        for t in 0..4u64 {
            let c = db.connect();
            let (done, stop) = (&done, &stop);
            s.spawn(move || {
                let mut x = t * 7919 + 1;
                while !stop.load(Ordering::Relaxed) {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    let (from, to) = ((x >> 33) % 10, (x >> 40) % 10);
                    if from == to {
                        continue;
                    }
                    let attempt = || -> Result<(), Error> {
                        c.execute("BEGIN")?;
                        let r = (|| {
                            c.execute(&format!("UPDATE acct SET bal = bal - 1 WHERE id = {from}"))?;
                            c.execute(&format!("UPDATE acct SET bal = bal + 1 WHERE id = {to}"))?;
                            c.execute("COMMIT").map(|_| ())
                        })();
                        if r.is_err() {
                            let _ = c.execute("ROLLBACK");
                        }
                        r
                    };
                    match attempt() {
                        Ok(()) => {
                            done.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(Error::Conflict(_)) => {}
                        Err(e) => panic!("{e}"),
                    }
                }
            });
        }
        for _ in 0..2 {
            let c = db.connect();
            let (stop, reads) = (&stop, &reads);
            s.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    // The total is the same in every snapshot, since each transfer commits whole.
                    assert_eq!(int(&c, "SELECT SUM(bal) FROM acct"), 1000);
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        thread::sleep(Duration::from_millis(700));
        stop.store(true, Ordering::Relaxed);
    });
    let c = db.connect();
    assert_eq!(int(&c, "SELECT SUM(bal) FROM acct"), 1000);
    assert!(done.load(Ordering::Relaxed) > 0 && reads.load(Ordering::Relaxed) > 0);
    // Whatever was left behind has been cleaned up: one version of each row.
    assert_eq!(db.inspect(|d| d.table("acct").unwrap().version_count()), 10);
}

// ----- schema changes ------------------------------------------------------------------------------

#[test]
fn a_schema_change_waits_for_the_others_and_holds_them_up_in_turn() {
    let db = counters();
    quick(&db);
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    a.execute("UPDATE c SET n = 1 WHERE id = 1").unwrap();
    assert!(is_conflict(b.execute("CREATE TABLE extra (x INTEGER)")), "another transaction is open");
    a.execute("COMMIT").unwrap();
    b.execute("CREATE TABLE extra (x INTEGER)").unwrap();

    // While one has changed the schema, nothing else runs, reads included.
    b.execute("BEGIN").unwrap();
    b.execute("CREATE TABLE more (x INTEGER)").unwrap();
    assert!(is_conflict(a.execute("SELECT COUNT(*) FROM c")));
    assert!(is_conflict(a.execute("INSERT INTO c VALUES (7, 7)")));
    b.execute("ROLLBACK").unwrap();
    assert_eq!(int(&a, "SELECT COUNT(*) FROM c"), 2);
    assert!(a.execute("SELECT * FROM more").is_err(), "the table went with the rollback");
}

#[test]
fn rows_changed_before_a_schema_change_in_the_same_transaction_commit_with_it() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    a.execute("INSERT INTO c VALUES (5, 5)").unwrap();
    a.execute("ALTER TABLE c ADD COLUMN extra INTEGER").unwrap();
    a.execute("UPDATE c SET extra = 1 WHERE id = 5").unwrap();
    a.execute("COMMIT").unwrap();
    assert_eq!(int(&b, "SELECT extra FROM c WHERE id = 5"), 1);
    assert_eq!(int(&b, "SELECT COUNT(*) FROM c"), 3);
    a.execute("BEGIN").unwrap();
    a.execute("DELETE FROM c WHERE id = 5").unwrap();
    a.execute("ALTER TABLE c DROP COLUMN extra").unwrap();
    a.execute("ROLLBACK").unwrap();
    assert_eq!(int(&b, "SELECT n FROM c WHERE id = 5"), 5);
}

// ----- cleaning up -----------------------------------------------------------------------------------

#[test]
fn old_versions_are_kept_only_while_someone_can_see_them() {
    let db = counters();
    let versions = || db.inspect(|d| d.table("c").unwrap().version_count());
    let w = db.connect();
    for i in 0..200 {
        w.execute(&format!("UPDATE c SET n = {i} WHERE id = 1")).unwrap();
    }
    assert_eq!(versions(), 2, "one version of each row when nobody is reading old data");

    let reader = db.connect();
    reader.execute("BEGIN").unwrap();
    assert_eq!(n(&reader, 1), 199);
    for i in 0..50 {
        w.execute(&format!("UPDATE c SET n = {} WHERE id = 1", 1000 + i)).unwrap();
    }
    assert_eq!(n(&reader, 1), 199, "the reader still sees its snapshot");
    assert!(versions() <= 4, "only the version the reader needs is kept, not all fifty: {}", versions());
    reader.execute("COMMIT").unwrap();
    w.execute("UPDATE c SET n = 0 WHERE id = 2").unwrap();
    assert_eq!(versions(), 2);

    // Deleted rows go too, and with them their index entries.
    w.execute("DELETE FROM c WHERE id = 1").unwrap();
    assert_eq!(versions(), 1);
    w.execute("INSERT INTO c VALUES (1, 1)").unwrap();
    assert_eq!(versions(), 2);
}

#[test]
fn an_abandoned_transaction_leaves_nothing_behind() {
    let db = counters();
    let c = db.connect();
    {
        let a = db.connect();
        a.execute("BEGIN").unwrap();
        a.execute("INSERT INTO c VALUES (8, 8)").unwrap();
        a.execute("UPDATE c SET n = 3").unwrap();
        a.execute("DELETE FROM c WHERE id = 1").unwrap();
    } // dropped with the transaction open
    assert_eq!(int(&c, "SELECT COUNT(*) FROM c"), 2);
    assert_eq!(int(&c, "SELECT SUM(n) FROM c"), 0);
    c.execute("INSERT INTO c VALUES (8, 8)").unwrap();
    assert_eq!(db.inspect(|d| d.table("c").unwrap().version_count()), 3);
}

#[test]
fn savepoints_work_among_overlapping_transactions() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    a.execute("UPDATE c SET n = 1 WHERE id = 1").unwrap();
    a.execute("SAVEPOINT s").unwrap();
    a.execute("UPDATE c SET n = 2 WHERE id = 1").unwrap();
    a.execute("INSERT INTO c VALUES (6, 6)").unwrap();
    b.execute("UPDATE c SET n = 4 WHERE id = 2").unwrap();
    a.execute("ROLLBACK TO s").unwrap();
    assert_eq!((n(&a, 1), int(&a, "SELECT COUNT(*) FROM c")), (1, 2));
    a.execute("COMMIT").unwrap();
    assert_eq!((n(&b, 1), n(&b, 2), int(&b, "SELECT COUNT(*) FROM c")), (1, 4, 2));
}

#[test]
fn counters_are_never_handed_out_twice() {
    let db = db_with("CREATE TABLE t (id COUNTER PRIMARY KEY, who INTEGER)");
    thread::scope(|s| {
        for who in 0..4 {
            let c = db.connect();
            s.spawn(move || {
                for _ in 0..30 {
                    c.execute("BEGIN").unwrap();
                    c.execute(&format!("INSERT INTO t (who) VALUES ({who})")).unwrap();
                    c.execute("COMMIT").unwrap();
                }
            });
        }
    });
    let c = db.connect();
    assert_eq!(int(&c, "SELECT COUNT(*) FROM t"), 120);
    assert_eq!(int(&c, "SELECT COUNT(DISTINCT id) FROM t"), 120);
}

// ----- the file ---------------------------------------------------------------------------------------

fn temp_db(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("accessor-mvcc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("db.adb")
}

#[test]
fn only_what_was_committed_survives_a_crash_whatever_the_order() {
    let path = temp_db("crash");
    {
        let db = SharedDatabase::open(&path).unwrap();
        let (a, b, c) = (db.connect(), db.connect(), db.connect());
        a.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
        a.execute("INSERT INTO t VALUES (1, 'base')").unwrap();
        a.execute("BEGIN").unwrap();
        b.execute("BEGIN").unwrap();
        c.execute("BEGIN").unwrap();
        a.execute("INSERT INTO t VALUES (10, 'a')").unwrap();
        b.execute("INSERT INTO t VALUES (11, 'b')").unwrap();
        c.execute("INSERT INTO t VALUES (12, 'c')").unwrap();
        a.execute("UPDATE t SET v = 'a-changed' WHERE id = 1").unwrap();
        b.execute("COMMIT").unwrap(); // slot 11 is logged before slot 10 exists
        // A checkpoint in the middle leaves the unfinished transactions out of the file ...
        db.connect().checkpoint().unwrap();
        a.execute("COMMIT").unwrap(); // ... and a's commit comes after it, in the log
        c.execute("ROLLBACK").unwrap();
        drop((a, b, c));
        db.simulate_crash();
    }
    let db = SharedDatabase::open(&path).unwrap();
    let c = db.connect();
    let rows = c.execute("SELECT id, v FROM t ORDER BY id").unwrap().rows;
    let rows: Vec<(i64, String)> = rows
        .iter()
        .map(|r| match (&r[0], &r[1]) {
            (Value::Int(i), Value::Text(t)) => (*i, t.to_string()),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(rows, vec![(1, "a-changed".to_string()), (10, "a".to_string()), (11, "b".to_string())]);
    drop(c);
    drop(db);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_transaction_open_at_a_crash_is_gone() {
    let path = temp_db("open-at-crash");
    {
        let db = SharedDatabase::open(&path).unwrap();
        let (a, b) = (db.connect(), db.connect());
        a.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();
        a.execute("INSERT INTO t VALUES (1)").unwrap();
        b.execute("BEGIN").unwrap();
        b.execute("INSERT INTO t VALUES (2)").unwrap();
        a.execute("INSERT INTO t VALUES (3)").unwrap();
        b.execute("ROLLBACK").unwrap();
        b.execute("BEGIN").unwrap();
        b.execute("INSERT INTO t VALUES (4)").unwrap();
        drop((a, b));
        db.simulate_crash();
    }
    let db = SharedDatabase::open(&path).unwrap();
    assert_eq!(int(&db.connect(), "SELECT SUM(id) FROM t"), 4);
    drop(db);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn transactions_waiting_for_each_other_are_found_out_at_once() {
    let db = counters(); // the default busy timeout is ten seconds: this must not take that long
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    b.execute("BEGIN").unwrap();
    a.execute("UPDATE c SET n = 1 WHERE id = 1").unwrap();
    b.execute("UPDATE c SET n = 2 WHERE id = 2").unwrap();
    let started = std::time::Instant::now();
    // Each now wants the other's row. Whichever notices first is told, rolls back, and the other goes on.
    let go = |c: &Connection, sql: &str| {
        let r = c.execute(sql);
        if r.is_err() {
            c.execute("ROLLBACK").unwrap();
        }
        r.map(|_| ())
    };
    let (ra, rb) = thread::scope(|s| {
        let ha = s.spawn(|| go(&a, "UPDATE c SET n = 3 WHERE id = 2"));
        let hb = s.spawn(|| go(&b, "UPDATE c SET n = 4 WHERE id = 1"));
        (ha.join().unwrap(), hb.join().unwrap())
    });
    assert!(started.elapsed() < Duration::from_secs(5), "waited for the timeout instead of seeing the deadlock");
    for r in [&ra, &rb] {
        if let Err(e) = r {
            assert!(matches!(e, Error::Conflict(m) if m.contains("deadlock")), "{e}");
        }
    }
    assert!(ra.is_err() || rb.is_err(), "they cannot both have gone on");
    if ra.is_ok() {
        a.execute("COMMIT").unwrap();
    }
    if rb.is_ok() {
        b.execute("COMMIT").unwrap();
    }
    let c = db.connect();
    assert_eq!(int(&c, "SELECT COUNT(*) FROM c"), 2);
}

/// Many threads doing a mixed lot of work with keys, relationships and deletes, readers checking that every
/// snapshot adds up, and at the end the table and its indexes agreeing with each other.
#[test]
fn a_mixed_load_keeps_every_snapshot_consistent_and_the_structures_sound() {
    let db = db_with(
        "CREATE TABLE acct (id INTEGER PRIMARY KEY, code INTEGER, bal INTEGER);
         CREATE UNIQUE INDEX acct_code ON acct (code);
         CREATE TABLE note (id COUNTER PRIMARY KEY, acct INTEGER REFERENCES acct (id) ON DELETE CASCADE, txt TEXT);
         CREATE TABLE pool (id INTEGER PRIMARY KEY, bal INTEGER);
         INSERT INTO pool VALUES (1, 0)",
    );
    db.set_busy_timeout(Some(Duration::from_secs(5)));
    let seed = db.connect();
    for i in 0..20 {
        seed.execute(&format!("INSERT INTO acct VALUES ({i}, {i}, 50)")).unwrap();
    }
    let total = 20 * 50;
    let next_id = AtomicUsize::new(100);
    let stop = AtomicBool::new(false);
    let counts: [AtomicUsize; 4] = Default::default();

    thread::scope(|s| {
        for t in 0..6u64 {
            let c = db.connect();
            let (stop, next_id, counts) = (&stop, &next_id, &counts);
            s.spawn(move || {
                let mut x = t * 104729 + 17;
                let mut rnd = move |m: u64| {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    (x >> 33) % m
                };
                while !stop.load(Ordering::Relaxed) {
                    let (a, b) = (rnd(20), rnd(20));
                    let kind = rnd(10);
                    let r = (|| -> Result<(), Error> {
                        c.execute("BEGIN")?;
                        let body = (|| -> Result<(), Error> {
                            match kind {
                                0..=4 => {
                                    let out = c.execute(&format!("UPDATE acct SET bal = bal - 1 WHERE id = {a}"))?.affected;
                                    let into = c.execute(&format!("UPDATE acct SET bal = bal + 1 WHERE id = {b}"))?.affected;
                                    if out != 1 || into != 1 {
                                        return Err(Error::constraint("an account has been retired"));
                                    }
                                }
                                5 => {
                                    // retire an account: its balance goes to the pool
                                    let bal = c.execute(&format!("SELECT bal FROM acct WHERE id = {a}"))?.rows.first().map(|r| r[0].clone());
                                    if let Some(Value::Int(bal)) = bal {
                                        c.execute(&format!("DELETE FROM acct WHERE id = {a}"))?;
                                        c.execute(&format!("UPDATE pool SET bal = bal + {bal} WHERE id = 1"))?;
                                    }
                                }
                                6 => {
                                    // open a new account with 5 from the pool, if there is that much
                                    let id = next_id.fetch_add(1, Ordering::Relaxed);
                                    if int(&c, "SELECT bal FROM pool WHERE id = 1") >= 5 {
                                        c.execute("UPDATE pool SET bal = bal - 5 WHERE id = 1")?;
                                        c.execute(&format!("INSERT INTO acct VALUES ({id}, {id}, 5)"))?;
                                    }
                                }
                                7 => {
                                    // change a key; sometimes it collides
                                    c.execute(&format!("UPDATE acct SET code = {} WHERE id = {a}", 1000 + rnd(30)))?;
                                }
                                8 => {
                                    c.execute(&format!("INSERT INTO note (acct, txt) VALUES ({a}, 'n')"))?;
                                }
                                _ => {
                                    c.execute(&format!("DELETE FROM note WHERE acct = {a}"))?;
                                }
                            }
                            c.execute("COMMIT").map(|_| ())
                        })();
                        if body.is_err() {
                            let _ = c.execute("ROLLBACK");
                        }
                        body
                    })();
                    match r {
                        Ok(()) => counts[0].fetch_add(1, Ordering::Relaxed),
                        Err(Error::Conflict(_)) => counts[1].fetch_add(1, Ordering::Relaxed),
                        Err(Error::Constraint(_)) => counts[2].fetch_add(1, Ordering::Relaxed),
                        Err(e) => panic!("unexpected: {e}"),
                    };
                }
            });
        }
        for _ in 0..2 {
            let c = db.connect();
            let (stop, counts) = (&stop, &counts);
            s.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    c.execute("BEGIN").unwrap();
                    let snap = |c: &Connection| {
                        c.read(|d| Ok((d.query("SELECT id, bal FROM acct ORDER BY id")?.rows, d.query("SELECT bal FROM pool WHERE id = 1")?.rows)))
                            .unwrap()
                    };
                    let (first, pool1) = snap(&c);
                    thread::yield_now();
                    let (second, _) = snap(&c);
                    let sum = |rows: &Vec<Vec<Value>>| rows.iter().map(|r| if let Value::Int(i) = r[1] { i } else { 0 }).sum::<i64>();
                    let (accts, again) = (sum(&first), sum(&second));
                    let pool = if let Value::Int(i) = pool1[0][0] { i } else { 0 };
                    c.execute("COMMIT").unwrap();
                    assert_eq!(accts + pool, total, "a snapshot that does not add up");
                    assert_eq!(accts, again, "a snapshot that moved");
                    counts[3].fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        thread::sleep(Duration::from_millis(1500));
        stop.store(true, Ordering::Relaxed);
    });

    let c = db.connect();
    assert_eq!(int(&c, "SELECT COALESCE(SUM(bal), 0) FROM acct") + int(&c, "SELECT bal FROM pool WHERE id = 1"), total);
    let rows = int(&c, "SELECT COUNT(*) FROM acct");
    // Every row can be found through each of its indexes, and only once.
    let by_scan = c.execute("SELECT id, code FROM acct ORDER BY id").unwrap().rows;
    assert_eq!(by_scan.len() as i64, rows);
    for r in &by_scan {
        let (Value::Int(id), Value::Int(code)) = (&r[0], &r[1]) else { panic!("{r:?}") };
        assert_eq!(int(&c, &format!("SELECT COUNT(*) FROM acct WHERE id = {id}")), 1);
        assert_eq!(int(&c, &format!("SELECT id FROM acct WHERE code = {code}")), *id);
    }
    assert_eq!(c.execute("SELECT id FROM acct ORDER BY code").unwrap().rows.len() as i64, rows);
    assert_eq!(int(&c, "SELECT COUNT(*) FROM acct WHERE code >= 0"), rows);
    // Notes refer to accounts that exist.
    assert_eq!(int(&c, "SELECT COUNT(*) FROM note WHERE acct NOT IN (SELECT id FROM acct)"), 0);
    // Nothing is left behind by the transactions that gave up.
    let (live_acct, live_note) = (rows as usize, int(&c, "SELECT COUNT(*) FROM note") as usize);
    assert_eq!(db.inspect(|d| d.table("acct").unwrap().version_count()), live_acct);
    assert_eq!(db.inspect(|d| d.table("note").unwrap().version_count()), live_note);
    let done: Vec<usize> = counts.iter().map(|a| a.load(Ordering::Relaxed)).collect();
    println!("committed {}, conflicts {}, constraint refusals {}, snapshots checked {}", done[0], done[1], done[2], done[3]);
    assert!(done[0] > 50 && done[3] > 10, "{done:?}");
}

#[test]
fn a_row_deleted_since_the_snapshot_cannot_be_deleted_or_changed_again() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    assert_eq!(n(&a, 1), 0);
    b.execute("DELETE FROM c WHERE id = 1").unwrap();
    assert!(is_conflict(a.execute("DELETE FROM c WHERE id = 1")));
    assert!(is_conflict(a.execute("UPDATE c SET n = 5 WHERE id = 1")));
    a.execute("COMMIT").unwrap();
    assert_eq!(int(&a, "SELECT COUNT(*) FROM c"), 1);
}
