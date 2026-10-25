//! `Database` must stay `Send` and `Sync`: a connection can move between threads, and several readers
//! can share a `&Database`. A stray `Rc`, `RefCell` or raw pointer anywhere in it fails this at compile time.
use accessor_core::{Database, Value};

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

#[test]
fn database_is_send_and_sync() {
    assert_send::<Database>();
    assert_sync::<Database>();
    assert_send::<Value>();
    assert_sync::<Value>();
}

#[test]
fn a_database_moves_between_threads_and_keeps_its_session() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE t (id COUNTER PRIMARY KEY, at TIMESTAMPTZ)").unwrap();
    db.execute("CREATE SEQUENCE s").unwrap();
    db.set_time_zone("Europe/Paris").unwrap();
    db.execute("INSERT INTO t (at) VALUES ('2024-07-01 12:00:00')").unwrap();
    // Other work on this thread changes this thread's session state.
    let mut other = Database::in_memory();
    other.set_time_zone("Asia/Tokyo").unwrap();

    let db = std::thread::spawn(move || {
        db.execute("INSERT INTO t (at) VALUES ('2024-07-01 12:00:00')").unwrap();
        assert_eq!(db.execute("SELECT NEXTVAL('s')").unwrap().rows[0][0], Value::Int(1));
        db
    })
    .join()
    .unwrap();
    let mut db = db;
    assert_eq!(db.execute("SELECT NEXTVAL('s')").unwrap().rows[0][0], Value::Int(2));
    let r = db.execute("SELECT COUNT(DISTINCT at) FROM t").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(1), "both rows were read in the Paris zone");
}

#[test]
fn readers_share_a_database_across_threads() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, g INTEGER)").unwrap();
    db.execute("INSERT INTO t SELECT n, n % 7 FROM GENERATE_SERIES(1, 2000) AS g (n)").unwrap();
    let db = &db;
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|i| {
                s.spawn(move || {
                    for _ in 0..50 {
                        let r = db.query(&format!("SELECT g, COUNT(*) FROM t WHERE id > {i} GROUP BY g ORDER BY g")).unwrap();
                        assert_eq!(r.rows.len(), 7);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    });
}
