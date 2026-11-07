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
    // object ids are numbers, and psql sends them back as quoted numbers
    assert!(oid.parse::<i64>().is_ok_and(|n| n >= 16384), "{oid}");
    assert_eq!(show(&mut b, &format!("SELECT c.relname FROM pg_catalog.pg_class c WHERE c.oid = '{oid}'")), "emp");
    assert_eq!(show(&mut b, &format!("SELECT '{oid}'::regclass")), "emp");
    assert_eq!(show(&mut b, &format!("SELECT count(*) FROM pg_catalog.pg_attribute a WHERE a.attrelid = '{oid}' AND a.attnum > 0")), "3");
    assert_eq!(show(&mut b, &format!("SELECT a.attname || ' ' || pg_catalog.format_type(a.atttypid, a.atttypmod) FROM pg_catalog.pg_attribute a WHERE a.attrelid = '{oid}' AND a.attname = 'salary'")), "salary numeric(10,2)");
    let ix = show(&mut b, &format!("SELECT i.indexrelid FROM pg_catalog.pg_index i WHERE i.indrelid = '{oid}' AND NOT i.indisprimary"));
    assert_eq!(show(&mut b, &format!("SELECT pg_catalog.pg_get_indexdef('{ix}'::oid, 0, true)")), "CREATE INDEX emp_name ON emp USING btree (name)");
    let def = show(&mut b, "SELECT pg_catalog.pg_get_indexdef(i.indexrelid, 0, true) FROM pg_catalog.pg_index i WHERE i.indexrelid = 'emp_name'");
    assert_eq!(def, "CREATE INDEX emp_name ON emp USING btree (name)");
    let fk = show(&mut b, "SELECT pg_catalog.pg_get_constraintdef(r.oid, true) FROM pg_catalog.pg_constraint r WHERE r.conrelid = 'dept' AND r.contype = 'f'");
    assert_eq!(fk, "FOREIGN KEY (emp_id) REFERENCES emp(id)");
    assert_eq!(show(&mut b, "SELECT pg_catalog.pg_get_expr(adbin, adrelid, true) FROM pg_catalog.pg_attrdef WHERE adrelid = 'emp'"), "0");

    drop((a, b));
    stop.stop();
    handle.join().unwrap();
}

#[test]
fn a_client_chooses_its_dialect() {
    let (conn, stop, handle) = start(Config::default());
    let value = |c: &mut Client, sql: &str| -> String {
        c.simple_query(sql)
            .unwrap()
            .iter()
            .find_map(|m| if let SimpleQueryMessage::Row(r) = m { r.get(0).map(str::to_string) } else { None })
            .unwrap_or_default()
    };
    let mut pg = Client::connect(&conn, NoTls).unwrap();
    let mut my = Client::connect(&format!("{conn} options='-c dialect=mysql'"), NoTls).unwrap();
    let mut ms = Client::connect(&format!("{conn} options='-c dialect=tsql'"), NoTls).unwrap();
    // a boolean is t / f in PostgreSQL's dialect, and the number 1 / 0 in MySQL's and T-SQL's
    assert_eq!(value(&mut pg, "SELECT 1 = 1"), "t");
    assert_eq!(value(&mut my, "SELECT 1 = 1"), "1");
    assert_eq!(value(&mut ms, "SELECT 1 = 1"), "1");
    // the dialect is the connection's own: the others are unchanged
    assert_eq!(value(&mut pg, "SELECT 1 = 1"), "t");
    // a connection can change its own later
    my.batch_execute("SET dialect = 'postgresql'").unwrap();
    assert_eq!(value(&mut my, "SELECT 1 = 1"), "t");
    // an unknown dialect is refused
    assert!(Client::connect(&format!("{conn} options='-c dialect=oracle'"), NoTls).is_err());
    drop((pg, my, ms));
    stop.stop();
    handle.join().unwrap();
}

#[test]
fn a_tsql_connection_reads_batches_and_shows_tsql_values() {
    let (conn, stop, handle) = start(Config::default());
    let mut ms = Client::connect(&format!("{conn} options='-c dialect=tsql'"), NoTls).unwrap();
    ms.batch_execute(
        "CREATE TABLE dbo.Emp (Id INT IDENTITY(1,1) PRIMARY KEY CLUSTERED, Name NVARCHAR(50) NOT NULL, Active BIT NOT NULL DEFAULT 1, Seen DATETIME2 NULL)\nGO\n\
         INSERT INTO dbo.Emp (Name, Seen) VALUES (N'Ann', '2024-03-05 10:30:15.250')\nGO",
    )
    .unwrap();
    let rows = |c: &mut Client, sql: &str| -> Vec<Vec<String>> {
        c.simple_query(sql)
            .unwrap()
            .iter()
            .filter_map(|m| if let SimpleQueryMessage::Row(r) = m { Some((0..r.len()).map(|i| r.get(i).unwrap_or("NULL").to_string()).collect()) } else { None })
            .collect()
    };
    assert_eq!(rows(&mut ms, "SELECT Name, Active, Seen FROM dbo.Emp"), vec![vec!["Ann", "1", "2024-03-05 10:30:15.250"]]);
    // a batch with variables and control flow returns its last result set
    let r = rows(&mut ms, "DECLARE @n INT = 1\nWHILE @n < 5 SET @n += 2\nSELECT @n AS n, DATEADD(day, 7, CAST('2020-01-15' AS DATE)) AS d");
    assert_eq!(r, vec![vec!["5", "2020-01-22"]]);
    // errors are caught in the batch
    let r = rows(&mut ms, "BEGIN TRY\nDECLARE @x INT = 1 / 0\nEND TRY\nBEGIN CATCH\nSELECT ERROR_MESSAGE() AS m\nEND CATCH");
    assert_eq!(r, vec![vec!["Divide by zero error encountered."]]);
    // a batch that selects several times sends every result set, in order
    let msgs = ms.simple_query("SELECT 1 AS a
SELECT 2 AS b
SELECT 3 AS c").unwrap();
    let firsts: Vec<String> = msgs.iter().filter_map(|m| if let SimpleQueryMessage::Row(r) = m { r.get(0).map(str::to_string) } else { None }).collect();
    assert_eq!(firsts, vec!["1", "2", "3"]);
    drop(ms);
    stop.stop();
    handle.join().unwrap();
}

#[test]
fn catalog_queries_work_through_the_extended_protocol() {
    let (conn, stop, handle) = start(Config::default());
    // what DBeaver asks when it connects, in the dialect of the server and in another one
    let queries = [
        "SELECT db.oid,db.* FROM pg_catalog.pg_database db WHERE datname = $1",
        "SELECT n.oid,n.*,d.description FROM pg_catalog.pg_namespace n LEFT OUTER JOIN pg_catalog.pg_description d ON d.objoid=n.oid AND d.objsubid=0 ORDER BY nspname",
        "SELECT current_schema(),session_user",
        "SELECT current_database()",
        "SELECT version()",
        "SELECT * FROM pg_catalog.pg_tablespace",
        "SELECT a.oid,a.* FROM pg_catalog.pg_roles a",
        "SELECT * FROM pg_catalog.pg_extension",
        "SELECT t.oid,t.*,c.relkind,format_type(nullif(t.typbasetype, 0), t.typtypmod) as base_type_name, d.description FROM pg_catalog.pg_type t LEFT OUTER JOIN pg_catalog.pg_type et ON et.oid=t.typelem LEFT OUTER JOIN pg_catalog.pg_class c ON c.oid=t.typrelid LEFT OUTER JOIN pg_catalog.pg_description d ON t.oid=d.objoid WHERE t.typname IS NOT NULL AND (c.relkind IS NULL OR c.relkind = 'c') AND (et.typcategory IS NULL OR et.typcategory <> 'C')",
        "SELECT c.oid,c.*,d.description FROM pg_catalog.pg_class c LEFT OUTER JOIN pg_catalog.pg_description d ON c.oid=d.objoid AND d.objsubid=0 WHERE c.relkind not in ('i','I','c')",
        "SELECT pg_catalog.pg_encoding_to_char(encoding) FROM pg_catalog.pg_database",
        "SELECT has_database_privilege('postgres', 'CONNECT')",
        "SELECT current_catalog",
        "SELECT c.oid,c.*,d.description,pg_catalog.pg_get_expr(c.relpartbound, c.oid) as partition_expr, pg_catalog.pg_get_partkeydef(c.oid) as partition_key FROM pg_catalog.pg_class c LEFT OUTER JOIN pg_catalog.pg_description d ON d.objoid=c.oid AND d.objsubid=0 WHERE c.relkind='r'",
        "SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod) AS tname, a.attnotnull, pg_catalog.pg_get_expr(ad.adbin, ad.adrelid) AS def, pg_catalog.col_description(a.attrelid, a.attnum) FROM pg_catalog.pg_attribute a LEFT JOIN pg_catalog.pg_attrdef ad ON a.attrelid = ad.adrelid AND a.attnum = ad.adnum WHERE a.attnum > 0 AND NOT a.attisdropped",
        "SELECT pg_catalog.pg_get_userbyid(c.relowner), c.relacl, c.reloptions, pg_catalog.pg_relation_size(c.oid) FROM pg_catalog.pg_class c",
        "SELECT conname, contype, pg_catalog.pg_get_constraintdef(oid, true) FROM pg_catalog.pg_constraint",
        "SELECT pg_catalog.pg_get_indexdef(indexrelid, 0, true), indisprimary, indisunique FROM pg_catalog.pg_index",
        "SELECT tgname, pg_catalog.pg_get_triggerdef(oid) FROM pg_catalog.pg_trigger WHERE NOT tgisinternal",
        "SELECT schemaname, tablename, tableowner FROM pg_catalog.pg_tables",
        "SHOW TRANSACTION ISOLATION LEVEL",
    ];
    let mut failed = Vec::new();
    for options in ["", " options='-c dialect=mysql'", " options='-c dialect=tsql'"] {
        let mut c = Client::connect(&format!("{conn}{options}"), NoTls).unwrap();
        for sql in queries {
            let params: Vec<&(dyn postgres::types::ToSql + Sync)> = if sql.contains("$1") { vec![&"postgres"] } else { vec![] };
            if let Err(e) = c.query(sql, &params) {
                failed.push(format!("[{options}] {sql}: {}", e.as_db_error().map_or(e.to_string(), |d| d.message().to_string())));
            }
        }
    }
    // the navigator: the schema's number, then its tables, columns and indexes by that number
    let mut c = Client::connect(&conn, NoTls).unwrap();
    c.batch_execute("CREATE TABLE emp (id int PRIMARY KEY, name varchar(20) NOT NULL); CREATE INDEX emp_name ON emp(name)").unwrap();
    let schema: i64 = c.query_one("SELECT n.oid FROM pg_catalog.pg_namespace n WHERE n.nspname = 'public'", &[]).unwrap().get::<_, i64>(0);
    assert_eq!(schema, 2200);
    // JDBC declares its parameters (setLong is int8) when it prepares
    let int8 = [postgres::types::Type::INT8];
    let q = |c: &mut Client, sql: &str, v: i64| {
        let st = c.prepare_typed(sql, &int8).unwrap();
        c.query(&st, &[&v]).unwrap()
    };
    let tables = q(&mut c, "SELECT c.oid, c.relname, c.relkind FROM pg_catalog.pg_class c WHERE c.relnamespace = $1 AND c.relkind NOT IN ('i','I','c')", schema);
    assert_eq!(tables.len(), 1);
    let table: i64 = tables[0].get::<_, &str>(0).parse().unwrap();
    let cols = q(&mut c, "SELECT a.attname, a.atttypid, a.atttypmod, a.attnum FROM pg_catalog.pg_attribute a WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum", table);
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[1].get::<_, &str>(1), "1043");
    let types = q(&mut c, "SELECT t.oid, t.typname FROM pg_catalog.pg_type t WHERE t.oid = $1", 1043);
    assert_eq!(types.len(), 1);
    let ixs = q(&mut c, "SELECT i.indexrelid FROM pg_catalog.pg_index i WHERE i.indrelid = $1", table);
    assert_eq!(ixs.len(), 2);
    stop.stop();
    handle.join().unwrap();
    assert!(failed.is_empty(), "{}", failed.join("
"));
}
