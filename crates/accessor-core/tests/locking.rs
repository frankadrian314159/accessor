//! Locking reads: `SELECT ... FOR UPDATE | SHARE [OF t] [NOWAIT | SKIP LOCKED]` on a shared database.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use accessor_core::{Connection, Error, SharedDatabase, Value};

fn counters() -> SharedDatabase {
    let db = SharedDatabase::in_memory();
    db.connect().execute("CREATE TABLE c (id INTEGER PRIMARY KEY, n INTEGER); INSERT INTO c VALUES (1, 0), (2, 0)").unwrap();
    db
}

fn int(c: &Connection, sql: &str) -> i64 {
    match c.execute(sql).unwrap().rows[0][0] {
        Value::Int(i) => i,
        ref v => panic!("{sql}: not an integer: {v:?}"),
    }
}

fn ids(c: &Connection, sql: &str) -> Vec<i64> {
    c.execute(sql)
        .unwrap()
        .rows
        .iter()
        .map(|r| match r[0] {
            Value::Int(i) => i,
            ref v => panic!("{sql}: not an integer: {v:?}"),
        })
        .collect()
}

#[test]
fn a_locking_read_waits_for_the_holder_and_then_sees_its_changes() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    assert_eq!(int(&a, "SELECT n FROM c WHERE id = 1 FOR UPDATE"), 0);
    b.execute("BEGIN").unwrap();
    assert_eq!(int(&b, "SELECT n FROM c WHERE id = 2"), 0); // b's snapshot is older than a's commit
    thread::scope(|s| {
        let waiter = s.spawn(|| {
            // waits for a, then goes by the newest values: no conflict, and the value a left
            let seen = int(&b, "SELECT n FROM c WHERE id = 1 FOR UPDATE");
            b.execute("UPDATE c SET n = n + 10 WHERE id = 1").unwrap();
            b.execute("COMMIT").unwrap();
            seen
        });
        thread::sleep(Duration::from_millis(150));
        assert!(!waiter.is_finished(), "the second transaction should be waiting for the first");
        a.execute("UPDATE c SET n = n + 1 WHERE id = 1").unwrap();
        a.execute("COMMIT").unwrap();
        assert_eq!(waiter.join().unwrap(), 1);
    });
    assert_eq!(int(&a, "SELECT n FROM c WHERE id = 1"), 11);
}

#[test]
fn without_the_lock_the_second_writer_is_told() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    b.execute("BEGIN").unwrap();
    assert_eq!(int(&b, "SELECT n FROM c WHERE id = 1"), 0);
    a.execute("UPDATE c SET n = 1 WHERE id = 1").unwrap();
    assert!(matches!(b.execute("UPDATE c SET n = n + 10 WHERE id = 1"), Err(Error::Conflict(_))));
}

#[test]
fn read_modify_write_with_a_lock_loses_nothing_and_fails_nowhere() {
    let db = counters();
    let conflicts = AtomicUsize::new(0);
    thread::scope(|s| {
        for _ in 0..8 {
            let c = db.connect();
            let conflicts = &conflicts;
            s.spawn(move || {
                for _ in 0..40 {
                    c.execute("BEGIN").unwrap();
                    let n = int(&c, "SELECT n FROM c WHERE id = 1 FOR UPDATE");
                    c.execute(&format!("UPDATE c SET n = {} WHERE id = 1", n + 1)).unwrap_or_else(|e| {
                        conflicts.fetch_add(1, Ordering::Relaxed);
                        panic!("{e}")
                    });
                    c.execute("COMMIT").unwrap();
                }
            });
        }
    });
    assert_eq!(conflicts.load(Ordering::Relaxed), 0);
    assert_eq!(int(&db.connect(), "SELECT n FROM c WHERE id = 1"), 320);
}

#[test]
fn nowait_fails_at_once_and_skip_locked_leaves_the_row_out() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    assert_eq!(ids(&a, "SELECT id FROM c WHERE id = 1 FOR UPDATE"), vec![1]);
    b.execute("BEGIN").unwrap();
    assert!(matches!(b.execute("SELECT id FROM c FOR UPDATE NOWAIT"), Err(Error::Conflict(_))));
    // the failed statement claimed nothing: row 2 can still be had by a third
    let c = db.connect();
    c.execute("BEGIN").unwrap();
    assert_eq!(ids(&c, "SELECT id FROM c WHERE id = 2 FOR UPDATE NOWAIT"), vec![2]);
    c.execute("ROLLBACK").unwrap();
    assert_eq!(ids(&b, "SELECT id FROM c ORDER BY id FOR UPDATE SKIP LOCKED"), vec![2]);
    assert_eq!(int(&b, "SELECT COUNT(*) FROM c FOR UPDATE SKIP LOCKED"), 1, "row 1 is a's; row 2 is b's own");
}

#[test]
fn a_queue_is_taken_one_row_each_with_skip_locked() {
    let db = SharedDatabase::in_memory();
    db.connect().execute("CREATE TABLE jobs (id INTEGER PRIMARY KEY, done INTEGER); INSERT INTO jobs VALUES (1, 0), (2, 0), (3, 0)").unwrap();
    let (a, b, c) = (db.connect(), db.connect(), db.connect());
    for x in [&a, &b, &c] {
        x.execute("BEGIN").unwrap();
    }
    let pop = "SELECT id FROM jobs WHERE done = 0 ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED";
    assert_eq!(ids(&a, pop), vec![1]);
    assert_eq!(ids(&b, pop), vec![2]);
    assert_eq!(ids(&c, pop), vec![3]);
    let d = db.connect();
    d.execute("BEGIN").unwrap();
    assert!(ids(&d, pop).is_empty());
    b.execute("ROLLBACK").unwrap();
    assert_eq!(ids(&d, pop), vec![2], "a row comes back when its holder lets go");
}

#[test]
fn rollback_lets_go_and_a_lock_outside_a_transaction_does_not_last() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    a.execute("SELECT * FROM c FOR UPDATE").unwrap();
    b.execute("BEGIN").unwrap();
    assert!(matches!(b.execute("SELECT * FROM c FOR UPDATE NOWAIT"), Err(Error::Conflict(_))));
    a.execute("ROLLBACK").unwrap();
    assert_eq!(ids(&b, "SELECT id FROM c ORDER BY id FOR UPDATE NOWAIT"), vec![1, 2]);
    b.execute("COMMIT").unwrap();
    // no transaction: the claim is made and released within the statement
    assert_eq!(ids(&a, "SELECT id FROM c ORDER BY id FOR SHARE"), vec![1, 2]);
    assert_eq!(ids(&b, "SELECT id FROM c ORDER BY id FOR UPDATE NOWAIT"), vec![1, 2]);
}

#[test]
fn rows_that_stop_matching_or_vanish_are_not_returned() {
    let db = counters();
    let (a, b) = (db.connect(), db.connect());
    b.execute("BEGIN").unwrap();
    assert_eq!(int(&b, "SELECT COUNT(*) FROM c"), 2);
    a.execute("UPDATE c SET n = 5 WHERE id = 1").unwrap();
    a.execute("DELETE FROM c WHERE id = 2").unwrap();
    // b's snapshot has both rows with n = 0; the newest state has one row with n = 5
    assert!(ids(&b, "SELECT id FROM c WHERE n = 0 FOR UPDATE").is_empty());
    assert_eq!(ids(&b, "SELECT id FROM c WHERE n = 5 FOR UPDATE"), vec![1]);
}

#[test]
fn only_a_select_of_one_table_is_locked_and_a_lock_survives_triggers_and_keys() {
    let db = SharedDatabase::in_memory();
    let a = db.connect();
    a.execute("CREATE TABLE p (id INTEGER PRIMARY KEY); CREATE TABLE ch (id INTEGER PRIMARY KEY, p INTEGER REFERENCES p); INSERT INTO p VALUES (1), (2); INSERT INTO ch VALUES (10, 1)").unwrap();
    let b = db.connect();
    a.execute("BEGIN").unwrap();
    assert_eq!(ids(&a, "SELECT p.id FROM p JOIN ch ON ch.p = p.id FOR UPDATE"), vec![1]); // a join is read, not locked
    b.execute("BEGIN").unwrap();
    assert_eq!(ids(&b, "SELECT id FROM p WHERE id = 1 FOR UPDATE NOWAIT"), vec![1]);
    b.execute("INSERT INTO ch VALUES (11, 1)").unwrap();
    b.execute("COMMIT").unwrap();
    a.execute("COMMIT").unwrap();
    assert_eq!(int(&a, "SELECT COUNT(*) FROM ch"), 2);
}

#[test]
fn a_deadlock_between_locking_reads_is_found() {
    let db = counters();
    db.set_busy_timeout(Some(Duration::from_secs(5)));
    let (a, b) = (db.connect(), db.connect());
    a.execute("BEGIN").unwrap();
    b.execute("BEGIN").unwrap();
    a.execute("SELECT * FROM c WHERE id = 1 FOR UPDATE").unwrap();
    b.execute("SELECT * FROM c WHERE id = 2 FOR UPDATE").unwrap();
    // each now wants the row the other holds; whichever notices second is the one told, and lets go for the other
    let ask = |c: &Connection, id: i64| {
        let r = c.execute(&format!("SELECT * FROM c WHERE id = {id} FOR UPDATE"));
        if r.is_err() {
            c.execute("ROLLBACK").unwrap();
        }
        r
    };
    let (ra, rb) = thread::scope(|s| {
        let first = s.spawn(|| ask(&a, 2));
        let second = s.spawn(|| ask(&b, 1));
        (first.join().unwrap(), second.join().unwrap())
    });
    let victims = [&ra, &rb].iter().filter(|r| matches!(r, Err(Error::Conflict(m)) if m.contains("deadlock"))).count();
    assert_eq!((victims, ra.is_ok() as usize + rb.is_ok() as usize), (1, 1), "{ra:?} {rb:?}");
}

#[test]
fn a_child_can_be_added_to_a_parent_another_transaction_has_locked() {
    let db = SharedDatabase::in_memory();
    let (a, b) = (db.connect(), db.connect());
    a.execute("CREATE TABLE p (id INTEGER PRIMARY KEY, n INTEGER); CREATE TABLE ch (id INTEGER PRIMARY KEY, p INTEGER REFERENCES p); INSERT INTO p VALUES (1, 0), (3, 0)").unwrap();
    db.set_busy_timeout(Some(Duration::from_millis(80)));
    a.execute("BEGIN").unwrap();
    a.execute("SELECT * FROM p WHERE id = 1 FOR UPDATE").unwrap();
    // the parent keeps its key whatever a does, so b need not wait for a
    b.execute("INSERT INTO ch VALUES (1, 1)").unwrap();
    // but if a changes the key of the parent, whether it is there depends on a
    a.execute("UPDATE p SET id = 4 WHERE id = 3").unwrap();
    assert!(matches!(b.execute("INSERT INTO ch VALUES (2, 3)"), Err(Error::Conflict(_))));
}
