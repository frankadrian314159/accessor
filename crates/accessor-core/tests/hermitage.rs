//! Hermitage (https://github.com/ept/hermitage): the isolation anomalies, one test each, in the order of Martin Kleppmann's
//! tables. Overlapping transactions of `SharedDatabase` give snapshot isolation, the level PostgreSQL calls REPEATABLE READ:
//! every anomaly up to and including G-single is prevented, G2-item and G2 (write skew) are possible.
//! A statement that has to wait for another transaction runs on a thread of its own.
use std::thread;
use std::time::Duration;

use accessor_core::{Connection, Error, SharedDatabase, Value};

/// `test (id, value)` with the two rows of Hermitage.
fn test_table() -> SharedDatabase {
    let db = SharedDatabase::in_memory();
    db.connect().execute("CREATE TABLE test (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO test VALUES (1, 10), (2, 20)").unwrap();
    db
}

fn value(c: &Connection, id: i64) -> Option<i64> {
    match c.execute(&format!("SELECT value FROM test WHERE id = {id}")).unwrap().rows.first() {
        Some(r) => match r[0] {
            Value::Int(i) => Some(i),
            ref v => panic!("not an integer: {v:?}"),
        },
        None => None,
    }
}

fn count(c: &Connection, sql: &str) -> i64 {
    match c.execute(sql).unwrap().rows[0][0] {
        Value::Int(i) => i,
        ref v => panic!("{sql}: not an integer: {v:?}"),
    }
}

fn begin(db: &SharedDatabase) -> Connection {
    let c = db.connect();
    c.execute("BEGIN").unwrap();
    c
}

fn is_conflict<T: std::fmt::Debug>(r: Result<T, Error>) -> bool {
    matches!(r, Err(Error::Conflict(_)))
}

/// Run `blocked` on `waiter` in a thread, let it start waiting, run `release`, and give back what `blocked` returned.
fn waiting(waiter: &Connection, blocked: &str, release: impl FnOnce()) -> Result<usize, Error> {
    thread::scope(|s| {
        let h = s.spawn(|| waiter.execute(blocked).map(|r| r.affected));
        thread::sleep(Duration::from_millis(150));
        release();
        h.join().unwrap()
    })
}

#[test]
fn g0_write_cycles_are_prevented() {
    let db = test_table();
    let (t1, t2) = (begin(&db), begin(&db));
    t1.execute("UPDATE test SET value = 11 WHERE id = 1").unwrap();
    // t2 waits for t1's change of row 1; t1 goes on and commits
    let r = waiting(&t2, "UPDATE test SET value = 12 WHERE id = 1", || {
        t1.execute("UPDATE test SET value = 21 WHERE id = 2").unwrap();
        t1.execute("COMMIT").unwrap();
    });
    assert!(is_conflict(r), "t2 began before t1 committed");
    t2.execute("ROLLBACK").unwrap();
    let t3 = db.connect();
    assert_eq!((value(&t3, 1), value(&t3, 2)), (Some(11), Some(21)), "t1's writes only");
}

#[test]
fn g1a_aborted_reads_are_prevented() {
    let db = test_table();
    let (t1, t2) = (begin(&db), begin(&db));
    t1.execute("UPDATE test SET value = 101 WHERE id = 1").unwrap();
    assert_eq!(value(&t2, 1), Some(10));
    t1.execute("ROLLBACK").unwrap();
    assert_eq!(value(&t2, 1), Some(10));
    t2.execute("COMMIT").unwrap();
}

#[test]
fn g1b_intermediate_reads_are_prevented() {
    let db = test_table();
    let (t1, t2) = (begin(&db), begin(&db));
    t1.execute("UPDATE test SET value = 101 WHERE id = 1").unwrap();
    assert_eq!(value(&t2, 1), Some(10));
    t1.execute("UPDATE test SET value = 11 WHERE id = 1").unwrap();
    t1.execute("COMMIT").unwrap();
    assert_eq!(value(&t2, 1), Some(10), "t2 keeps its snapshot");
    t2.execute("COMMIT").unwrap();
}

#[test]
fn g1c_circular_information_flow_is_prevented() {
    let db = test_table();
    let (t1, t2) = (begin(&db), begin(&db));
    t1.execute("UPDATE test SET value = 11 WHERE id = 1").unwrap();
    t2.execute("UPDATE test SET value = 22 WHERE id = 2").unwrap();
    assert_eq!(value(&t1, 2), Some(20));
    assert_eq!(value(&t2, 1), Some(10));
    t1.execute("COMMIT").unwrap();
    t2.execute("COMMIT").unwrap();
}

#[test]
fn otv_observed_transaction_vanishes_is_prevented() {
    let db = test_table();
    let (t1, t2, t3) = (begin(&db), begin(&db), begin(&db));
    t1.execute("UPDATE test SET value = 11 WHERE id = 1").unwrap();
    t1.execute("UPDATE test SET value = 19 WHERE id = 2").unwrap();
    let r = waiting(&t2, "UPDATE test SET value = 12 WHERE id = 1", || {
        t1.execute("COMMIT").unwrap();
        // t3 began before t1 committed: it sees neither of t1's changes, and so cannot see one without the other
        assert_eq!((value(&t3, 1), value(&t3, 2)), (Some(10), Some(20)));
    });
    assert!(is_conflict(r));
    t2.execute("ROLLBACK").unwrap();
    t3.execute("COMMIT").unwrap();
    assert_eq!((value(&db.connect(), 1), value(&db.connect(), 2)), (Some(11), Some(19)));
}

#[test]
fn pmp_predicate_many_preceders_is_prevented() {
    let db = test_table();
    let (t1, t2) = (begin(&db), begin(&db));
    assert_eq!(count(&t1, "SELECT COUNT(*) FROM test WHERE value = 30"), 0);
    t2.execute("INSERT INTO test VALUES (3, 30)").unwrap();
    t2.execute("COMMIT").unwrap();
    assert_eq!(count(&t1, "SELECT COUNT(*) FROM test WHERE value = 30"), 0, "no phantom");
    t1.execute("COMMIT").unwrap();
    assert_eq!(count(&db.connect(), "SELECT COUNT(*) FROM test WHERE value = 30"), 1);
}

#[test]
fn pmp_write_is_prevented() {
    let db = test_table();
    let (t1, t2) = (begin(&db), begin(&db));
    t1.execute("UPDATE test SET value = value + 10").unwrap();
    let r = waiting(&t2, "DELETE FROM test WHERE value = 20", || {
        t1.execute("COMMIT").unwrap();
    });
    assert!(is_conflict(r), "the row t2 would delete is not the one it saw");
    t2.execute("ROLLBACK").unwrap();
    let c = db.connect();
    assert_eq!((value(&c, 1), value(&c, 2)), (Some(20), Some(30)));
}

#[test]
fn p4_lost_update_is_prevented() {
    let db = test_table();
    let (t1, t2) = (begin(&db), begin(&db));
    assert_eq!(value(&t1, 1), Some(10));
    assert_eq!(value(&t2, 1), Some(10));
    t1.execute("UPDATE test SET value = 11 WHERE id = 1").unwrap();
    let r = waiting(&t2, "UPDATE test SET value = 11 WHERE id = 1", || {
        t1.execute("COMMIT").unwrap();
    });
    assert!(is_conflict(r));
    t2.execute("ROLLBACK").unwrap();
    assert_eq!(value(&db.connect(), 1), Some(11));
}

#[test]
fn g_single_read_skew_is_prevented() {
    let db = test_table();
    // read skew: t1 reads one row before and the other after t2 changed both
    let (t1, t2) = (begin(&db), begin(&db));
    assert_eq!(value(&t1, 1), Some(10));
    t2.execute("UPDATE test SET value = 12 WHERE id = 1").unwrap();
    t2.execute("UPDATE test SET value = 18 WHERE id = 2").unwrap();
    t2.execute("COMMIT").unwrap();
    assert_eq!(value(&t1, 2), Some(20), "t1 does not see half of t2");
    t1.execute("COMMIT").unwrap();

    // the same, ending in a write that would build on the old value
    let db = test_table();
    let (t1, t2) = (begin(&db), begin(&db));
    assert_eq!(value(&t1, 1), Some(10));
    assert_eq!(count(&t1, "SELECT COUNT(*) FROM test WHERE value = 20"), 1);
    t2.execute("UPDATE test SET value = 12 WHERE id = 1").unwrap();
    t2.execute("UPDATE test SET value = 18 WHERE id = 2").unwrap();
    t2.execute("COMMIT").unwrap();
    assert!(is_conflict(t1.execute("DELETE FROM test WHERE value = 20")));
    t1.execute("ROLLBACK").unwrap();
    assert_eq!(count(&db.connect(), "SELECT COUNT(*) FROM test"), 2);
}

#[test]
fn g2_item_write_skew_is_possible() {
    let db = test_table();
    let (t1, t2) = (begin(&db), begin(&db));
    assert_eq!(count(&t1, "SELECT SUM(value) FROM test"), 30);
    assert_eq!(count(&t2, "SELECT SUM(value) FROM test"), 30);
    t1.execute("UPDATE test SET value = 11 WHERE id = 1").unwrap();
    t2.execute("UPDATE test SET value = 21 WHERE id = 2").unwrap();
    t1.execute("COMMIT").unwrap();
    t2.execute("COMMIT").unwrap();
    assert_eq!(count(&db.connect(), "SELECT SUM(value) FROM test"), 32);
}

#[test]
fn g2_anti_dependency_cycles_over_a_predicate_are_possible() {
    let db = test_table();
    let (t1, t2) = (begin(&db), begin(&db));
    assert_eq!(count(&t1, "SELECT COALESCE(SUM(value), 0) FROM test WHERE value % 3 = 0"), 0);
    assert_eq!(count(&t2, "SELECT COALESCE(SUM(value), 0) FROM test WHERE value % 3 = 0"), 0);
    t1.execute("INSERT INTO test VALUES (3, 30)").unwrap();
    t2.execute("INSERT INTO test VALUES (4, 42)").unwrap();
    t1.execute("COMMIT").unwrap();
    t2.execute("COMMIT").unwrap();
    assert_eq!(count(&db.connect(), "SELECT COALESCE(SUM(value), 0) FROM test WHERE value % 3 = 0"), 72);
}
