//! The server against a real PostgreSQL client (the `postgres` crate): handshake, authentication, simple and extended queries,
//! binary results, transactions, several clients at once, and a stop.

use accessor_core::dialect::Dialect;
use accessor_core::{Database, SharedDatabase};
use accessor_server::{AuthMethod, Config, Server};
use postgres::{Client, NoTls, SimpleQueryMessage};

fn start(config: Config) -> (String, accessor_server::StopHandle, std::thread::JoinHandle<()>) {
    let mut db = Database::in_memory();
    db.set_dialect(Dialect::Postgresql);
    let server = Server::bind("127.0.0.1:0", SharedDatabase::new(db), config).unwrap();
    let addr = server.local_addr().unwrap();
    let stop = server.stop_handle();
    let handle = std::thread::spawn(move || server.run().unwrap());
    (format!("host=127.0.0.1 port={} user=tester dbname=x", addr.port()), stop, handle)
}

#[test]
fn simple_and_extended_queries_with_text_and_binary_results() {
    let (conn, stop, handle) = start(Config::default());
    let mut c = Client::connect(&conn, NoTls).unwrap();

    // the simple protocol: several statements in one message, each answered
    let r = c.simple_query("CREATE TABLE t (id int, name text, price numeric(10,2), at timestamp, d date, ok boolean); INSERT INTO t VALUES (1, 'ann', 9.50, '2024-03-05 10:30:15.25', '2024-03-05', true), (2, NULL, 0.1, NULL, NULL, false); SELECT id, name FROM t ORDER BY id").unwrap();
    let rows: Vec<_> = r.iter().filter_map(|m| if let SimpleQueryMessage::Row(r) = m { Some((r.get(0).map(str::to_string), r.get(1).map(str::to_string))) } else { None }).collect();
    assert_eq!(rows, vec![(Some("1".into()), Some("ann".into())), (Some("2".into()), None)]);
    let tags: Vec<u64> = r.iter().filter_map(|m| if let SimpleQueryMessage::CommandComplete(n) = m { Some(*n) } else { None }).collect();
    assert_eq!(tags, vec![0, 2, 2]);

    // the extended protocol, results in binary format
    let rows = c.query("SELECT id, name, price::text, at, d, ok FROM t ORDER BY id", &[]).unwrap();
    assert_eq!(rows.len(), 2);
    let id: i64 = rows[0].get(0);
    let name: Option<String> = rows[0].get(1);
    assert_eq!((id, name.as_deref()), (1, Some("ann")));
    let ok: bool = rows[0].get(5);
    assert!(ok);
    let none: Option<String> = rows[1].get(1);
    assert_eq!(none, None);

    // parameters
    let rows = c.query("SELECT name FROM t WHERE id = $1", &[&"2"]).unwrap();
    assert_eq!(rows.len(), 1);
    let rows = c.query("SELECT ($1::int + 1)::text, $2", &[&"41", &"x'y"]).unwrap();
    let (sum, text): (String, String) = (rows[0].get(0), rows[0].get(1));
    assert_eq!((sum.as_str(), text.as_str()), ("42", "x'y"));

    // an error is reported with its SQLSTATE and the session goes on
    let e = c.simple_query("SELECT * FROM nope").unwrap_err();
    assert_eq!(e.code().map(|c| c.code()), Some("42P01"));
    let e = c.simple_query("SELEC 1").unwrap_err();
    assert_eq!(e.code().map(|c| c.code()), Some("42601"));
    assert_eq!(c.query("SELECT 1", &[]).unwrap().len(), 1);

    // the extended protocol: an error skips to the Sync, and the next statement works
    assert!(c.query("SELECT * FROM nope WHERE a = $1", &[&"1"]).is_err());
    assert_eq!(c.query("SELECT count(*) FROM t", &[]).unwrap()[0].get::<_, i64>(0), 2);

    c.close().unwrap();
    stop.stop();
    handle.join().unwrap();
}

#[test]
fn transactions_and_several_clients() {
    let (conn, stop, handle) = start(Config::default());
    let mut a = Client::connect(&conn, NoTls).unwrap();
    let mut b = Client::connect(&conn, NoTls).unwrap();
    a.batch_execute("CREATE TABLE k (v int); INSERT INTO k VALUES (1)").unwrap();

    // a transaction is invisible to the other client until it commits, and a rollback leaves nothing
    a.batch_execute("BEGIN; INSERT INTO k VALUES (2)").unwrap();
    assert_eq!(b.query("SELECT count(*) FROM k", &[]).unwrap()[0].get::<_, i64>(0), 1);
    a.batch_execute("COMMIT").unwrap();
    assert_eq!(b.query("SELECT count(*) FROM k", &[]).unwrap()[0].get::<_, i64>(0), 2);
    a.batch_execute("BEGIN; INSERT INTO k VALUES (3); ROLLBACK").unwrap();
    assert_eq!(b.query("SELECT count(*) FROM k", &[]).unwrap()[0].get::<_, i64>(0), 2);

    // many clients at once
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let conn = conn.clone();
            std::thread::spawn(move || {
                let mut c = Client::connect(&conn, NoTls).unwrap();
                for j in 0..20 {
                    c.batch_execute(&format!("INSERT INTO k VALUES ({})", 100 + i * 20 + j)).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(a.query("SELECT count(*) FROM k", &[]).unwrap()[0].get::<_, i64>(0), 2 + 160);

    drop((a, b));
    stop.stop();
    handle.join().unwrap();
}

#[test]
fn passwords_are_checked() {
    for method in [AuthMethod::Cleartext, AuthMethod::Md5, AuthMethod::Scram] {
        let (conn, stop, handle) = start(Config { auth: method, users: vec![("tester".into(), "s3cret".into())], ..Config::default() });
        let mut ok = Client::connect(&format!("{conn} password=s3cret"), NoTls).unwrap_or_else(|e| panic!("{method:?}: {e}"));
        assert_eq!(ok.query("SELECT 1", &[]).unwrap().len(), 1);
        assert!(Client::connect(&format!("{conn} password=wrong"), NoTls).is_err(), "{method:?} let a wrong password in");
        assert!(Client::connect(&conn, NoTls).is_err(), "{method:?} let no password in");
        drop(ok);
        stop.stop();
        handle.join().unwrap();
    }
}

#[test]
fn settings_are_per_connection_and_psql_catalog_queries_work() {
    let (conn, stop, handle) = start(Config::default());
    let mut a = Client::connect(&conn, NoTls).unwrap();
    let mut b = Client::connect(&conn, NoTls).unwrap();
    let show = |c: &mut Client, sql: &str| -> String {
        c.simple_query(sql)
            .unwrap()
            .iter()
            .find_map(|m| if let SimpleQueryMessage::Row(r) = m { r.get(0).map(str::to_string) } else { None })
            .unwrap_or_default()
    };
    a.batch_execute("SET TIME ZONE 'Europe/Paris'").unwrap();
    assert_eq!(show(&mut a, "SHOW TIME ZONE"), "Europe/Paris");
    assert_eq!(show(&mut b, "SHOW TIME ZONE"), "UTC");

    // column names as PostgreSQL gives them
    a.batch_execute("CREATE TABLE emp (id int PRIMARY KEY, name text NOT NULL, salary numeric(10,2) DEFAULT 0)").unwrap();
    a.batch_execute("CREATE TABLE dept (id int PRIMARY KEY, emp_id int REFERENCES emp(id)); CREATE INDEX emp_name ON emp(name)").unwrap();
    let r = a.simple_query("SELECT count(*), max(id), id + 1, upper(name) FROM emp GROUP BY id, name").unwrap();
    let names: Vec<String> = r.iter().find_map(|m| if let SimpleQueryMessage::Row(r) = m { Some(r.columns().iter().map(|c| c.name().to_string()).collect()) } else { None }).unwrap_or_else(|| {
        // no rows: ask for the columns another way
        let s = a.prepare("SELECT count(*), max(id), id + 1, upper(name) FROM emp GROUP BY id, name").unwrap();
        s.columns().iter().map(|c| c.name().to_string()).collect()
    });
    assert_eq!(names, ["count", "max", "?column?", "upper"]);

    // the queries psql sends for \d emp
    let oid = show(&mut b, "SELECT c.oid FROM pg_catalog.pg_class c LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE c.relname OPERATOR(pg_catalog.~) '^(emp)$' COLLATE pg_catalog.default AND pg_catalog.pg_table_is_visible(c.oid) ORDER BY 2, 3");
    assert_eq!(oid, "emp");
    let def = show(&mut b, "SELECT pg_catalog.pg_get_indexdef(i.indexrelid, 0, true) FROM pg_catalog.pg_index i WHERE i.indexrelid = 'emp_name'");
    assert_eq!(def, "CREATE INDEX emp_name ON emp USING btree (name)");
    let fk = show(&mut b, "SELECT pg_catalog.pg_get_constraintdef(r.oid, true) FROM pg_catalog.pg_constraint r WHERE r.conrelid = 'dept' AND r.contype = 'f'");
    assert_eq!(fk, "FOREIGN KEY (emp_id) REFERENCES emp(id)");
    assert_eq!(show(&mut b, "SELECT pg_catalog.pg_get_expr(adbin, adrelid, true) FROM pg_catalog.pg_attrdef WHERE adrelid = 'emp'"), "0");

    drop((a, b));
    stop.stop();
    handle.join().unwrap();
}
