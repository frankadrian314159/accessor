//! Many readers or one writer: `SharedDatabase` and `Connection`.
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Barrier, Mutex};
use std::thread;
use std::time::Duration;

use accessor_core::{Connection, Database, Error, SharedDatabase, Value};

fn counter_db() -> SharedDatabase {
    let db = SharedDatabase::in_memory();
    let c = db.connect();
    c.execute("CREATE TABLE c (id INTEGER PRIMARY KEY, n INTEGER)").unwrap();
    c.execute("INSERT INTO c VALUES (1, 0)").unwrap();
    db
}

fn n(c: &Connection) -> i64 {
    match c.execute("SELECT n FROM c WHERE id = 1").unwrap().rows[0][0] {
        Value::Int(i) => i,
        ref v => panic!("not an integer: {v:?}"),
    }
}

#[test]
fn readers_run_at_the_same_time() {
    let db = counter_db();
    // Every reader waits at the barrier for all the others while still reading: if reads took turns,
    // this could never get past it.
    let barrier = Barrier::new(6);
    thread::scope(|s| {
        for _ in 0..6 {
            let c = db.connect();
            let barrier = &barrier;
            s.spawn(move || {
                c.read(|d| {
                    barrier.wait();
                    assert_eq!(d.query("SELECT COUNT(*) FROM c").unwrap().rows[0][0], Value::Int(1));
                    Ok(())
                })
                .unwrap();
            });
        }
    });
}

#[test]
fn selects_through_execute_do_not_wait_for_each_other() {
    let db = counter_db();
    let total = AtomicUsize::new(0);
    thread::scope(|s| {
        for _ in 0..8 {
            let c = db.connect();
            let total = &total;
            s.spawn(move || {
                for _ in 0..100 {
                    assert_eq!(n(&c), 0);
                    total.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    assert_eq!(total.load(Ordering::Relaxed), 800);
}

#[test]
fn writers_take_turns() {
    let db = counter_db();
    thread::scope(|s| {
        for _ in 0..8 {
            let c = db.connect();
            s.spawn(move || {
                for _ in 0..100 {
                    c.execute("UPDATE c SET n = n + 1 WHERE id = 1").unwrap();
                }
            });
        }
    });
    assert_eq!(n(&db.connect()), 800);
}

#[test]
fn transactions_do_not_interleave() {
    let db = counter_db();
    // Read, then write what was read plus one, inside a transaction: only correct if transactions are serial.
    thread::scope(|s| {
        for _ in 0..6 {
            let c = db.connect();
            s.spawn(move || {
                for _ in 0..40 {
                    c.execute("BEGIN").unwrap();
                    assert!(c.in_transaction());
                    let seen = n(&c);
                    c.execute(&format!("UPDATE c SET n = {} WHERE id = 1", seen + 1)).unwrap();
                    c.execute("COMMIT").unwrap();
                    assert!(!c.in_transaction());
                }
            });
        }
    });
    assert_eq!(n(&db.connect()), 240);
}

#[test]
fn an_open_transaction_holds_everyone_else_up_and_is_not_seen_until_commit() {
    let db = counter_db();
    db.set_busy_timeout(Some(Duration::from_millis(100)));
    let a = db.connect();
    let b = db.connect();
    a.execute("BEGIN").unwrap();
    a.execute("UPDATE c SET n = 5 WHERE id = 1").unwrap();
    assert_eq!(n(&a), 5, "the writer sees its own changes");
    // The others can neither read the uncommitted 5 nor write.
    assert!(matches!(b.execute("SELECT n FROM c"), Err(Error::Locked(_))));
    assert!(matches!(b.execute("UPDATE c SET n = 9"), Err(Error::Locked(_))));
    a.execute("COMMIT").unwrap();
    assert_eq!(n(&b), 5);
    b.execute("UPDATE c SET n = 6").unwrap();
    assert_eq!(n(&a), 6);
}

#[test]
fn a_waiting_connection_goes_ahead_when_the_transaction_ends() {
    let db = counter_db();
    let a = db.connect();
    a.execute("BEGIN").unwrap();
    a.execute("UPDATE c SET n = 7").unwrap();
    let b = db.connect();
    let seen = thread::scope(|s| {
        let h = s.spawn(move || n(&b));
        thread::sleep(Duration::from_millis(100));
        a.execute("COMMIT").unwrap();
        h.join().unwrap()
    });
    assert_eq!(seen, 7);
}

#[test]
fn rollback_and_dropping_a_connection_undo_the_transaction() {
    let db = counter_db();
    let a = db.connect();
    a.execute("BEGIN").unwrap();
    a.execute("UPDATE c SET n = 1").unwrap();
    a.execute("ROLLBACK").unwrap();
    assert_eq!(n(&db.connect()), 0);

    let a = db.connect();
    a.execute("BEGIN").unwrap();
    a.execute("CREATE TABLE gone (x INTEGER)").unwrap();
    a.execute("UPDATE c SET n = 2").unwrap();
    drop(a);
    let b = db.connect();
    assert_eq!(n(&b), 0);
    assert!(b.execute("SELECT * FROM gone").is_err());
    b.execute("UPDATE c SET n = 3").unwrap(); // the turn was given back
}

#[test]
fn a_failed_statement_in_a_transaction_leaves_it_open() {
    let db = counter_db();
    let a = db.connect();
    a.execute("BEGIN").unwrap();
    assert!(a.execute("INSERT INTO c VALUES (1, 0)").is_err());
    assert!(a.in_transaction());
    a.execute("COMMIT").unwrap();
    assert!(!a.in_transaction());
}

#[test]
fn a_busy_timeout_gives_up_instead_of_waiting_forever() {
    let db = counter_db();
    db.set_busy_timeout(Some(Duration::from_millis(50)));
    let a = db.connect();
    let b = db.connect();
    a.execute("BEGIN").unwrap();
    let started = std::time::Instant::now();
    let err = b.execute("INSERT INTO c VALUES (2, 0)").unwrap_err();
    assert!(matches!(err, Error::Locked(_)));
    assert!(started.elapsed() < Duration::from_secs(5));
    // The failed wait left nothing behind: once the transaction ends everyone proceeds.
    a.execute("ROLLBACK").unwrap();
    b.execute("INSERT INTO c VALUES (2, 0)").unwrap();
}

#[test]
fn sequences_hand_out_distinct_numbers_to_every_thread() {
    let db = SharedDatabase::in_memory();
    db.connect().execute("CREATE SEQUENCE s").unwrap();
    let seen = Mutex::new(Vec::new());
    thread::scope(|s| {
        for _ in 0..4 {
            let c = db.connect();
            let seen = &seen;
            s.spawn(move || {
                for _ in 0..50 {
                    if let Value::Int(i) = c.execute("SELECT NEXTVAL('s')").unwrap().rows[0][0] {
                        seen.lock().unwrap().push(i);
                    }
                }
            });
        }
    });
    let seen = seen.into_inner().unwrap();
    assert_eq!(seen.len(), 200);
    assert_eq!(seen.iter().collect::<HashSet<_>>().len(), 200, "a number was handed out twice");
}

#[test]
fn prepared_statements_read_and_write() {
    let db = counter_db();
    let c = db.connect();
    let get = c.prepare("SELECT n FROM c WHERE id = @id").unwrap();
    let set = c.prepare("UPDATE c SET n = @n WHERE id = @id").unwrap();
    thread::scope(|s| {
        for t in 0..4i64 {
            let c = db.connect();
            let (get, set) = (get.clone(), set.clone());
            s.spawn(move || {
                for i in 0..25 {
                    c.execute_prepared(&set, &[("n", Value::Int(t * 100 + i)), ("id", Value::Int(1))]).unwrap();
                    let r = c.execute_prepared(&get, &[("id", Value::Int(1))]).unwrap();
                    assert_eq!(r.rows.len(), 1);
                }
            });
        }
    });
    assert!(n(&c) >= 0);
}

#[test]
fn a_shared_file_is_locked_once_and_kept() {
    let dir = std::env::temp_dir().join(format!("accessor-shared-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("shared.adb");
    {
        let db = SharedDatabase::open(&path).unwrap();
        // Another open of the same file, from this process or any other, is refused;
        // sharing is done with connections.
        assert!(matches!(Database::open(&path), Err(Error::Locked(_))));
        assert!(matches!(SharedDatabase::open(&path), Err(Error::Locked(_))));
        let a = db.connect();
        a.execute("CREATE TABLE t (x INTEGER)").unwrap();
        thread::scope(|s| {
            for i in 0..4 {
                let c = db.connect();
                s.spawn(move || {
                    for j in 0..25 {
                        c.execute(&format!("INSERT INTO t VALUES ({})", i * 100 + j)).unwrap();
                    }
                });
            }
        });
        drop(a);
    }
    let db = SharedDatabase::open(&path).unwrap();
    assert_eq!(db.connect().execute("SELECT COUNT(*) FROM t").unwrap().rows[0][0], Value::Int(100));
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
