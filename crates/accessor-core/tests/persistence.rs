use std::fs::OpenOptions;
use std::io::Write;

use accessor_core::{Database, Durability, Value};

fn wal_path(p: &std::path::Path) -> std::path::PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push("-wal");
    s.into()
}

fn count(db: &Database, table: &str) -> i64 {
    match db.query(&format!("SELECT Count(*) FROM {table}")).unwrap().rows[0][0] {
        Value::Int(n) => n,
        _ => panic!("count is not an integer"),
    }
}

#[test]
fn reopen_after_clean_close() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.adb");
    {
        let mut db = Database::open(&path).unwrap();
        db.execute("CREATE TABLE t (id COUNTER PRIMARY KEY, name TEXT, amt CURRENCY)").unwrap();
        db.execute("INSERT INTO t (name, amt) VALUES ('a', 1.5), ('b', 2.5), ('c', 3)").unwrap();
        db.execute("DELETE FROM t WHERE name = 'b'").unwrap();
        db.execute("CREATE VIEW v AS SELECT name FROM t").unwrap();
    }
    let mut db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "t"), 2);
    assert_eq!(db.query("SELECT name FROM v ORDER BY name").unwrap().rows.len(), 2);
    // AutoNumber must not reuse the deleted id's successor space.
    db.execute("INSERT INTO t (name) VALUES ('d')").unwrap();
    assert_eq!(db.query("SELECT id FROM t WHERE name = 'd'").unwrap().rows[0][0], Value::Int(4));
}

#[test]
fn recovers_from_log_without_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.adb");
    let mut db = Database::open(&path).unwrap();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    db.execute("INSERT INTO t VALUES (1, 'one'), (2, 'two')").unwrap();
    db.execute("UPDATE t SET v = 'TWO' WHERE id = 2").unwrap();
    // Simulate a crash: leak the handle so Drop never checkpoints.
    std::mem::forget(db);

    let db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "t"), 2);
    assert_eq!(db.query("SELECT v FROM t WHERE id = 2").unwrap().rows[0][0], Value::text("TWO"));
}

#[test]
fn torn_log_tail_is_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.adb");
    let mut db = Database::open(&path).unwrap();
    db.set_durability(Durability::OsBuffered);
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();
    db.execute("INSERT INTO t VALUES (1)").unwrap();
    db.execute("INSERT INTO t VALUES (2)").unwrap();
    std::mem::forget(db);

    // Chop the final frame in half, as a power loss mid-write would.
    let wal = wal_path(&path);
    let len = std::fs::metadata(&wal).unwrap().len();
    OpenOptions::new().write(true).open(&wal).unwrap().set_len(len - 3).unwrap();

    let mut db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "t"), 1);
    // The log is usable again after recovery.
    db.execute("INSERT INTO t VALUES (3)").unwrap();
    drop(db);
    let db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "t"), 2);
}

#[test]
fn corrupt_frame_stops_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.adb");
    let mut db = Database::open(&path).unwrap();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();
    db.execute("INSERT INTO t VALUES (1)").unwrap();
    std::mem::forget(db);
    let wal = wal_path(&path);
    let mut f = OpenOptions::new().append(true).open(&wal).unwrap();
    f.write_all(&[9, 0, 0, 0, 1, 2, 3, 4, 0xde, 0xad, 0xbe, 0xef, 0, 0, 0, 0, 0]).unwrap();
    drop(f);
    let db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "t"), 1);
}

#[test]
fn uncommitted_transaction_is_not_durable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.adb");
    let mut db = Database::open(&path).unwrap();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();
    db.execute("BEGIN").unwrap();
    db.execute("INSERT INTO t VALUES (1)").unwrap();
    std::mem::forget(db);
    let db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "t"), 0);
}

#[test]
fn committed_transaction_is_durable_and_compaction_works() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.adb");
    let mut db = Database::open(&path).unwrap();
    db.set_durability(Durability::OsBuffered);
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT)").unwrap();
    db.execute("BEGIN").unwrap();
    for i in 0..500 {
        db.execute(&format!("INSERT INTO t VALUES ({i}, 'row {i}')")).unwrap();
    }
    db.execute("COMMIT").unwrap();
    db.execute("DELETE FROM t WHERE id % 2 = 0").unwrap();
    std::mem::forget(db);

    let mut db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "t"), 250);
    assert_eq!(db.table("t").unwrap().tombstones(), 0, "open compacts replayed tombstones");
    db.execute("DELETE FROM t WHERE id < 100").unwrap();
    db.compact().unwrap();
    assert_eq!(db.table("t").unwrap().slot_count(), 200);
    assert_eq!(db.query("SELECT s FROM t WHERE id = 301").unwrap().rows[0][0], Value::text("row 301"));
}

#[test]
fn rejects_foreign_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("junk.adb");
    std::fs::write(&path, b"this is not a database at all, definitely not").unwrap();
    assert!(Database::open(&path).is_err());
}

#[test]
fn alter_table_survives_log_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alter.adb");
    {
        let mut db = Database::open(&path).unwrap();
        db.execute("CREATE TABLE t (id COUNTER PRIMARY KEY, a TEXT, b INTEGER)").unwrap();
        db.execute("INSERT INTO t (a, b) VALUES ('x', 1), ('y', 2)").unwrap();
        db.checkpoint().unwrap();
        db.execute("ALTER TABLE t ADD COLUMN c TEXT DEFAULT 'z'").unwrap();
        db.execute("ALTER TABLE t RENAME COLUMN a TO name").unwrap();
        db.execute("ALTER TABLE t DROP COLUMN b").unwrap();
        db.set_durability(Durability::default());
        std::mem::forget(db); // simulate a crash: only the log holds the changes
    }
    let db = Database::open(&path).unwrap();
    let r = db.query("SELECT name, c FROM t ORDER BY id").unwrap();
    assert_eq!(r.rows[1], vec![Value::text("y"), Value::text("z")]);
}

#[test]
fn foreign_keys_survive_reopen_and_log_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fk.adb");
    {
        let mut db = Database::open(&path).unwrap();
        db.execute(
            "CREATE TABLE P (ID INTEGER PRIMARY KEY);
             CREATE TABLE C (ID INTEGER PRIMARY KEY, P INTEGER REFERENCES P ON DELETE CASCADE);
             INSERT INTO P VALUES (1), (2); INSERT INTO C VALUES (1, 1), (2, 1), (3, 2);",
        )
        .unwrap();
        db.checkpoint().unwrap();
        db.execute("DELETE FROM P WHERE ID = 1").unwrap(); // the cascade lands in the log too
        std::mem::forget(db); // crash
    }
    let mut db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "C"), 1);
    assert!(db.execute("INSERT INTO C VALUES (9, 42)").is_err());
}

#[test]
fn triggers_and_parameter_queries_persist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("proc.adb");
    {
        let mut db = Database::open(&path).unwrap();
        db.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE log (msg TEXT);
             CREATE QUERY by_id (@id Integer) AS SELECT name FROM t WHERE id = @id;
             CREATE TRIGGER t_added AFTER INSERT ON t BEGIN INSERT INTO log VALUES ('added ' & NEW.name); END",
        )
        .unwrap();
        db.checkpoint().unwrap();
        db.execute("INSERT INTO t VALUES (1, 'a')").unwrap();
        std::mem::forget(db); // crash: the trigger's effect is only in the log
    }
    let mut db = Database::open(&path).unwrap();
    // Replay applied the logged trigger output exactly once, without re-firing.
    assert_eq!(count(&db, "log"), 1);
    assert_eq!(db.trigger_names(), vec!["t_added"]);
    assert_eq!(db.query_parameters("by_id").unwrap(), vec!["id"]);
    db.execute("INSERT INTO t VALUES (2, 'b')").unwrap();
    assert_eq!(count(&db, "log"), 2);
    drop(db);

    // And again after a clean close (snapshot round trip).
    let mut db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "log"), 2);
    assert_eq!(db.execute("EXEC by_id 2").unwrap().rows[0][0], Value::text("b"));
    db.execute("DROP TRIGGER t_added").unwrap();
    drop(db);
    assert!(Database::open(&path).unwrap().trigger_names().is_empty());
}

#[test]
fn calculated_fields_persist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calc.adb");
    {
        let mut db = Database::open(&path).unwrap();
        db.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, sum CALCULATED AS (a + b));
             INSERT INTO t (id, a, b) VALUES (1, 1, 2)",
        )
        .unwrap();
        db.checkpoint().unwrap();
        db.execute("INSERT INTO t (id, a, b) VALUES (2, 10, 20)").unwrap();
        db.execute("UPDATE t SET a = 100 WHERE id = 1").unwrap();
        db.execute("ALTER TABLE t ADD COLUMN twice CALCULATED AS (sum * 2)").unwrap();
        std::mem::forget(db); // crash: everything after the checkpoint is only in the log
    }
    let mut db = Database::open(&path).unwrap();
    let rows = db.query("SELECT id, sum, twice FROM t ORDER BY id").unwrap().rows;
    assert_eq!(rows[0], vec![Value::Int(1), Value::Int(102), Value::Int(204)]);
    assert_eq!(rows[1], vec![Value::Int(2), Value::Int(30), Value::Int(60)]);
    // Still live after recovery.
    db.execute("UPDATE t SET b = 0 WHERE id = 2").unwrap();
    assert_eq!(count(&db, "t WHERE sum = 10"), 1);
    drop(db);

    let db = Database::open(&path).unwrap();
    let t = db.table("t").unwrap();
    assert_eq!(t.schema().columns[3].expression(), Some("a + b"));
    assert_eq!(db.query("SELECT sum FROM t WHERE id = 2").unwrap().rows[0][0], Value::Int(10));
}
// ----- files written by earlier versions ------------------------------------------
//
// These tests build the bytes an old version wrote, with the old numeric types
// (Integer, Double, Currency) and value tags, by serializing local mirrors of
// the old layouts.

use serde::Serialize;

#[derive(Serialize)]
#[allow(dead_code)]
enum OldType {
    Integer,
    Double,
    Currency,
    Text,
    YesNo,
    DateTime,
    Calculated,
}

#[derive(Serialize)]
#[allow(dead_code)]
enum OldValue {
    Null,
    Bool(bool),
    Int(i64),
    Double(f64),
    Currency(i64),
    DateTime(i64),
    Text(String),
}

#[derive(Serialize)]
struct OldColumn {
    name: String,
    data_type: OldType,
    not_null: bool,
    primary_key: bool,
    autonumber: bool,
    default: Option<OldValue>,
}

#[derive(Serialize)]
struct OldIndex {
    name: String,
    columns: Vec<usize>,
    unique: bool,
}

fn old_column(name: &str, data_type: OldType, primary_key: bool) -> OldColumn {
    OldColumn { name: name.into(), data_type, not_null: primary_key, primary_key, autonumber: false, default: None }
}

/// A table `t (id INTEGER PRIMARY KEY, name TEXT, price CURRENCY, ratio DOUBLE)` with one row.
fn old_columns() -> Vec<OldColumn> {
    vec![
        old_column("id", OldType::Integer, true),
        old_column("name", OldType::Text, false),
        old_column("price", OldType::Currency, false),
        old_column("ratio", OldType::Double, false),
    ]
}

fn old_row() -> Vec<Option<Vec<OldValue>>> {
    vec![Some(vec![
        OldValue::Int(1),
        OldValue::Text("a".into()),
        OldValue::Currency(12_345_000), // $1234.50
        OldValue::Double(0.1),
    ])]
}

fn old_index() -> Vec<OldIndex> {
    vec![OldIndex { name: "PrimaryKey".into(), columns: vec![0], unique: true }]
}

fn old_file(version: u32, epoch: u64, payload: &[u8]) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(b"ACDB");
    file.extend_from_slice(&version.to_le_bytes());
    file.extend_from_slice(&epoch.to_le_bytes());
    file.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    file.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    file.extend_from_slice(payload);
    file
}

/// Old columns keep the type they had: Integer, Currency and Double. Their values are
/// read exactly: the old money (in 1/10 000ths) is a CURRENCY, the double is the same double.
fn check_old_numbers(db: &Database) {
    let r = db.query("SELECT id, price, ratio, price + ratio, price * 2 FROM t").unwrap();
    let shown: Vec<String> = r.rows[0].iter().map(ToString::to_string).collect();
    assert_eq!(shown, ["1", "$1234.50", "0.1", "1234.6", "$2469.00"]);
    let types: Vec<_> = db.table("t").unwrap().schema().columns.iter().map(|c| c.data_type).collect();
    assert_eq!(types[0], accessor_core::DataType::Integer);
    assert_eq!(types[2], accessor_core::DataType::Currency);
    assert_eq!(types[3], accessor_core::DataType::Double);
    // Money plus a double is a double, money times a whole number is money.
    let r = db.query("SELECT TypeName(price + ratio), TypeName(price * 2) FROM t").unwrap();
    assert_eq!((r.rows[0][0].to_string(), r.rows[0][1].to_string()), ("DOUBLE".into(), "CURRENCY".into()));
}

/// Version 1: no foreign keys in the schema encoding.
#[test]
fn opens_version_1_files() {
    let schema = ("t".to_string(), old_columns(), old_index());
    let snapshot = (vec![(schema, old_row(), 2i64)], Vec::<(String, String)>::new());
    let file = old_file(1, 5, &postcard::to_allocvec(&snapshot).unwrap());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.adb");
    std::fs::write(&path, file).unwrap();
    {
        let mut db = Database::open(&path).unwrap();
        check_old_numbers(&db);
        db.execute("CREATE TABLE u (k INTEGER REFERENCES t)").unwrap();
        db.execute("INSERT INTO u VALUES (1)").unwrap();
    }
    let db = Database::open(&path).unwrap(); // now rewritten in the current format
    check_old_numbers(&db);
    assert_eq!(count(&db, "u"), 1);
}

/// Version 2: foreign keys in the schema, no triggers section.
#[test]
fn opens_version_2_files() {
    let schema = ("t".to_string(), old_columns(), old_index(), Vec::<accessor_core::ForeignKey>::new());
    let snapshot = (vec![(schema, old_row(), 2i64)], vec![("v".to_string(), "SELECT name FROM t".to_string())]);
    let file = old_file(2, 7, &postcard::to_allocvec(&snapshot).unwrap());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v2.adb");
    std::fs::write(&path, file).unwrap();
    {
        let mut db = Database::open(&path).unwrap();
        check_old_numbers(&db);
        assert_eq!(db.query("SELECT name FROM v").unwrap().rows.len(), 1);
        db.execute("CREATE TRIGGER g AFTER INSERT ON t BEGIN RAISE 'no'; END").unwrap();
    }
    let db = Database::open(&path).unwrap(); // rewritten in the current format
    assert_eq!(db.trigger_names(), vec!["g"]);
    check_old_numbers(&db);
}

/// Version 3 files hold no new fields, only the old numeric types, so the
/// only difference from today is the data in them.
#[test]
fn opens_version_3_files() {
    let schema = ("t".to_string(), old_columns(), old_index(), Vec::<accessor_core::ForeignKey>::new());
    let snapshot = (
        vec![(schema, old_row(), 2i64)],
        Vec::<(String, String)>::new(),
        Vec::<(String, String)>::new(),
    );
    let file = old_file(3, 9, &postcard::to_allocvec(&snapshot).unwrap());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v3.adb");
    std::fs::write(&path, file).unwrap();
    let mut db = Database::open(&path).unwrap();
    check_old_numbers(&db);
    // The old columns convert what they are given to their own type.
    db.execute("INSERT INTO t VALUES (2, 'b', 0.0001, 99999999999999999999999)").unwrap();
    let r = db.query("SELECT price, ratio FROM t WHERE id = 2").unwrap();
    assert_eq!((r.rows[0][0].to_string(), r.rows[0][1].to_string()), ("$0.0001".into(), "100000000000000000000000".into()));
}

#[test]
fn big_and_fractional_numbers_persist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("numbers.adb");
    let biggest = "123456789012345678901234567890123456789";
    {
        let mut db = Database::open(&path).unwrap();
        db.execute("CREATE TABLE n (id COUNTER PRIMARY KEY, x NUMBER)").unwrap();
        db.execute(&format!("INSERT INTO n (x) VALUES ({biggest}), (0.1), (-2.50), (7)")).unwrap();
        db.checkpoint().unwrap();
        // Only in the log: another huge value, an update, and arithmetic past 64 bits.
        db.execute("INSERT INTO n (x) VALUES (0.000000000000000000000000000000000001)").unwrap();
        db.execute("UPDATE n SET x = x * x WHERE id = 4").unwrap();
        db.execute("UPDATE n SET x = x + 0.9 WHERE id = 2").unwrap();
        std::mem::forget(db); // crash
    }
    let check = |db: &Database| {
        let shown: Vec<String> = db
            .query("SELECT x FROM n ORDER BY id")
            .unwrap()
            .rows
            .iter()
            .map(|r| r[0].to_string())
            .collect();
        assert_eq!(shown, [biggest, "1", "-2.5", "49", "0.000000000000000000000000000000000001"]);
    };
    let db = Database::open(&path).unwrap();
    check(&db);
    drop(db);
    check(&Database::open(&path).unwrap()); // and again from the snapshot alone
}

/// A file from before NUMBER that contains a calculated field: the field must
/// still be calculated, and keep following its record.
#[test]
fn old_files_keep_their_calculated_fields() {
    let mut qty = old_column("qty", OldType::Integer, false);
    let mut total = old_column("total", OldType::Calculated, false);
    total.default = Some(OldValue::Text("qty * 2".into()));
    qty.not_null = false;
    let columns = vec![old_column("id", OldType::Integer, true), qty, total];
    let rows = vec![Some(vec![OldValue::Int(1), OldValue::Int(5), OldValue::Int(10)])];
    let schema = ("t".to_string(), columns, old_index(), Vec::<accessor_core::ForeignKey>::new());
    let snapshot = (
        vec![(schema, rows, 2i64)],
        Vec::<(String, String)>::new(),
        Vec::<(String, String)>::new(),
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calc-old.adb");
    std::fs::write(&path, old_file(3, 4, &postcard::to_allocvec(&snapshot).unwrap())).unwrap();

    let mut db = Database::open(&path).unwrap();
    let col = &db.table("t").unwrap().schema().columns[2];
    assert_eq!(col.data_type, accessor_core::DataType::Calculated);
    assert_eq!(col.expression(), Some("qty * 2"));
    // qty is an INTEGER column, so 7.5 is stored as the nearest whole number (ties to even): 8.
    db.execute("UPDATE t SET qty = 7.5").unwrap();
    assert_eq!(db.query("SELECT qty, total FROM t").unwrap().rows[0][1].to_string(), "16");
    db.execute("UPDATE t SET qty = 7").unwrap();
    assert_eq!(db.query("SELECT total FROM t").unwrap().rows[0][0].to_string(), "14");
    drop(db);
    let db = Database::open(&path).unwrap();
    assert_eq!(db.table("t").unwrap().schema().columns[2].expression(), Some("qty * 2"));
}

#[test]
fn converting_a_field_to_calculated_survives_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("convert.adb");
    {
        let mut db = Database::open(&path).unwrap();
        db.execute(
            "CREATE TABLE t (id COUNTER PRIMARY KEY, a NUMBER, b NUMBER);
             INSERT INTO t (a, b) VALUES (1, 100), (2, 200)",
        )
        .unwrap();
        db.checkpoint().unwrap();
        db.execute("ALTER TABLE t ALTER COLUMN b CALCULATED AS (a * 10)").unwrap();
        db.execute("INSERT INTO t (a) VALUES (3)").unwrap();
        std::mem::forget(db); // crash: the conversion exists only in the log
    }
    let mut db = Database::open(&path).unwrap();
    let b: Vec<String> = db.query("SELECT b FROM t ORDER BY id").unwrap().rows.iter().map(|r| r[0].to_string()).collect();
    assert_eq!(b, ["10", "20", "30"]);
    assert_eq!(db.table("t").unwrap().schema().columns[2].expression(), Some("a * 10"));
    db.execute("UPDATE t SET a = 5 WHERE id = 1").unwrap();
    assert_eq!(db.query("SELECT b FROM t WHERE id = 1").unwrap().rows[0][0].to_string(), "50");
}

/// One row of every numeric type must come back with the same types and values,
/// whether it is read from the snapshot or replayed from the log.
#[test]
fn every_numeric_type_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tower.adb");
    let expect: Vec<String> = [
        "INTEGER:-9223372036854775808",
        "BIGNUM:123456789012345678901234567890",
        "FLOAT:0.1",
        "DOUBLE:0.30000000000000004",
        "DECIMAL:-0.000000000000000000000000001",
        "CURRENCY:$19.99",
        "RATIONAL:-22/7",
    ]
    .map(String::from)
    .to_vec();
    let describe = |db: &Database, id: i64| -> Vec<String> {
        db.query(&format!("SELECT i, b, f, d, c, m, r FROM z WHERE id = {id}"))
            .unwrap()
            .rows[0]
            .iter()
            .map(|v| format!("{}:{v}", v.type_label()))
            .collect()
    };
    {
        let mut db = Database::open(&path).unwrap();
        db.execute(
            "CREATE TABLE z (id COUNTER PRIMARY KEY, i INTEGER, b BIGNUM, f FLOAT, d DOUBLE, c DECIMAL, m CURRENCY, r RATIONAL);
             CREATE INDEX z_d ON z (d); CREATE INDEX z_r ON z (r)",
        )
        .unwrap();
        let row = "(-9223372036854775808, 123456789012345678901234567890, CSng(0.1), 0.1 + 0.2e0, \
                    -0.000000000000000000000000001, 19.99, CRat(-22) / 7)";
        db.execute(&format!("INSERT INTO z (i, b, f, d, c, m, r) VALUES {row}")).unwrap();
        db.checkpoint().unwrap();
        db.execute(&format!("INSERT INTO z (i, b, f, d, c, m, r) VALUES {row}")).unwrap(); // only in the log
        std::mem::forget(db); // crash
    }
    for round in 0..2 {
        let db = Database::open(&path).unwrap();
        assert_eq!(describe(&db, 1), expect, "snapshot, round {round}");
        assert_eq!(describe(&db, 2), expect, "log, round {round}");
        // The indexes were rebuilt in value order, whatever the types.
        let hits = db.query("SELECT Count(*) FROM z WHERE r = CRat(-22) / 7").unwrap();
        assert_eq!(hits.rows[0][0].to_string(), "2");
        let hits = db.query("SELECT Count(*) FROM z WHERE d > 0.30000000000000003").unwrap();
        assert_eq!(hits.rows[0][0].to_string(), "2");
    } // the second round reads what the first one checkpointed
}

/// What the short-lived single NUMBER type wrote: a NUMBER column held whole
/// numbers as integers, so on load they must become DECIMALs like their column.
#[test]
fn opens_files_from_the_number_build() {
    use serde::Serialize;

    // Type tag 7 was NUMBER; Value tag 7 was Dec(mantissa, scale).
    #[derive(Serialize)]
    enum NumberBuildType {
        Integer,
        Double,
        Currency,
        Text,
        YesNo,
        DateTime,
        Calculated,
        Number,
    }
    #[derive(Serialize)]
    enum NumberBuildValue {
        Null,
        Bool(bool),
        Int(i64),
        Double(f64),
        Currency(i64),
        DateTime(i64),
        Text(String),
        Dec((num_bigint::BigInt, u32)),
    }
    #[derive(Serialize)]
    struct Col {
        name: String,
        data_type: NumberBuildType,
        not_null: bool,
        primary_key: bool,
        autonumber: bool,
        default: Option<NumberBuildValue>,
    }
    let col = |name: &str, ty, pk| Col { name: name.into(), data_type: ty, not_null: pk, primary_key: pk, autonumber: false, default: None };
    let columns = vec![col("id", NumberBuildType::Integer, true), col("amount", NumberBuildType::Number, false)];
    let rows = vec![
        Some(vec![NumberBuildValue::Int(1), NumberBuildValue::Int(42)]), // a whole NUMBER
        Some(vec![NumberBuildValue::Int(2), NumberBuildValue::Dec((num_bigint::BigInt::from(25), 1))]), // 2.5
    ];
    let schema = ("t".to_string(), columns, old_index(), Vec::<accessor_core::ForeignKey>::new());
    let snapshot = (vec![(schema, rows, 3i64)], Vec::<(String, String)>::new(), Vec::<(String, String)>::new());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("number-build.adb");
    std::fs::write(&path, old_file(3, 2, &postcard::to_allocvec(&snapshot).unwrap())).unwrap();

    let db = Database::open(&path).unwrap();
    let types: Vec<String> = db
        .query("SELECT TypeName(amount) FROM t ORDER BY id")
        .unwrap()
        .rows
        .iter()
        .map(|r| r[0].to_string())
        .collect();
    assert_eq!(types, ["DECIMAL", "DECIMAL"]);
    assert_eq!(db.table("t").unwrap().schema().columns[1].data_type, accessor_core::DataType::Decimal);
    let amounts: Vec<String> = db.query("SELECT amount FROM t ORDER BY id").unwrap().rows.iter().map(|r| r[0].to_string()).collect();
    assert_eq!(amounts, ["42", "2.5"]);
}

// ----- transactions and durability --------------------------------------------------------

fn tx_schema(db: &mut Database) {
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    db.execute("INSERT INTO t VALUES (1, 'base')").unwrap();
    db.checkpoint().unwrap();
}

#[test]
fn a_committed_transaction_survives_a_crash_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tx.adb");
    {
        let mut db = Database::open(&path).unwrap();
        tx_schema(&mut db);
        db.begin().unwrap();
        db.execute("INSERT INTO t VALUES (2, 'in tx')").unwrap();
        db.execute("ALTER TABLE t ADD COLUMN extra INTEGER DEFAULT 7").unwrap();
        db.execute("CREATE TABLE made (a INTEGER)").unwrap();
        db.execute("INSERT INTO made VALUES (1)").unwrap();
        db.execute("DELETE FROM t WHERE id = 1").unwrap();
        db.commit().unwrap();
        std::mem::forget(db); // crash: the transaction exists only in the log
    }
    let db = Database::open(&path).unwrap();
    let rows: Vec<String> = db.query("SELECT id, v, extra FROM t").unwrap().rows[0].iter().map(ToString::to_string).collect();
    assert_eq!(rows, ["2", "in tx", "7"]);
    assert_eq!(count(&db, "t"), 1);
    assert_eq!(count(&db, "made"), 1);
}

#[test]
fn an_uncommitted_or_rolled_back_transaction_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tx.adb");
    {
        let mut db = Database::open(&path).unwrap();
        tx_schema(&mut db);
        db.begin().unwrap();
        db.execute("INSERT INTO t VALUES (2, 'rolled back')").unwrap();
        db.execute("CREATE TABLE gone (a INTEGER)").unwrap();
        db.rollback().unwrap();
        db.begin().unwrap();
        db.execute("INSERT INTO t VALUES (3, 'never committed')").unwrap();
        db.execute("DROP TABLE t").unwrap();
        std::mem::forget(db); // crash with the transaction still open
    }
    let db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "t"), 1);
    assert!(db.table("gone").is_none());
    assert_eq!(db.query("SELECT v FROM t").unwrap().rows[0][0].to_string(), "base");
}

#[test]
fn dropping_the_database_with_an_open_transaction_rolls_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tx.adb");
    {
        let mut db = Database::open(&path).unwrap();
        tx_schema(&mut db);
        db.begin().unwrap();
        db.execute("INSERT INTO t VALUES (2, 'abandoned')").unwrap();
        db.execute("ALTER TABLE t ADD COLUMN x INTEGER").unwrap();
        assert_eq!(db.pending_changes(), 2);
    } // dropped with the transaction open
    let db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "t"), 1);
    assert_eq!(db.table("t").unwrap().schema().columns.len(), 2);
}

#[test]
fn savepoints_rolled_back_before_commit_never_reach_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tx.adb");
    {
        let mut db = Database::open(&path).unwrap();
        tx_schema(&mut db);
        db.begin().unwrap();
        db.execute("INSERT INTO t VALUES (2, 'kept')").unwrap();
        db.savepoint("sp").unwrap();
        db.execute("INSERT INTO t VALUES (3, 'undone')").unwrap();
        db.execute("CREATE TABLE undone (a INTEGER)").unwrap();
        db.execute("UPDATE t SET v = 'changed' WHERE id = 1").unwrap();
        db.rollback_to("sp").unwrap();
        db.execute("INSERT INTO t VALUES (4, 'after')").unwrap();
        db.release("sp").unwrap();
        db.commit().unwrap();
        std::mem::forget(db);
    }
    let db = Database::open(&path).unwrap();
    let rows: Vec<String> = db.query("SELECT v FROM t ORDER BY id").unwrap().rows.iter().map(|r| r[0].to_string()).collect();
    assert_eq!(rows, ["base", "kept", "after"]);
    assert!(db.table("undone").is_none());
}

#[test]
fn a_torn_commit_loses_the_whole_transaction_never_part_of_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tx.adb");
    {
        let mut db = Database::open(&path).unwrap();
        db.set_durability(Durability::OsBuffered);
        tx_schema(&mut db);
        db.execute("INSERT INTO t VALUES (2, 'before')").unwrap();
        db.begin().unwrap();
        for i in 10..20 {
            db.execute(&format!("INSERT INTO t VALUES ({i}, 'tx')")).unwrap();
        }
        db.execute("CREATE TABLE late (a INTEGER)").unwrap();
        db.commit().unwrap();
        std::mem::forget(db);
    }
    // Power fails while the commit record is being written: its tail is lost.
    let wal = wal_path(&path);
    let len = std::fs::metadata(&wal).unwrap().len();
    OpenOptions::new().write(true).open(&wal).unwrap().set_len(len - 5).unwrap();
    let db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "t"), 2, "none of the ten rows, and the work before the transaction is intact");
    assert!(db.table("late").is_none());
}

#[test]
fn a_transaction_can_span_a_large_amount_of_work() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tx.adb");
    let mut db = Database::open(&path).unwrap();
    db.execute("CREATE TABLE big (id INTEGER PRIMARY KEY, n INTEGER)").unwrap();
    db.transaction(|db| {
        for i in 0..5_000 {
            db.execute(&format!("INSERT INTO big VALUES ({i}, {})", i * 2))?;
        }
        Ok(())
    })
    .unwrap();
    std::mem::forget(db);
    let db = Database::open(&path).unwrap();
    assert_eq!(count(&db, "big"), 5_000);
    assert_eq!(db.query("SELECT Sum(n) FROM big").unwrap().rows[0][0].to_string(), "24995000");
}

#[test]
fn intervals_and_zoned_timestamps_survive_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tz.adb");
    {
        let mut db = Database::open(&path).unwrap();
        db.execute("CREATE TABLE e (at TIMESTAMPTZ, dur INTERVAL)").unwrap();
        db.execute("INSERT INTO e VALUES ('2024-03-05 10:00:00+01:00', '1 month 2 days 03:04:05')").unwrap();
    }
    let db = Database::open(&path).unwrap();
    let r = db.query("SELECT at, dur FROM e").unwrap();
    assert_eq!(r.rows[0][0].to_string(), "2024-03-05 09:00:00+00:00");
    assert_eq!(r.rows[0][1].to_string(), "1 month 2 days 03:04:05");
    assert_eq!(db.query("SELECT TypeName(at), TypeName(dur) FROM e").unwrap().rows[0][1].to_string(), "INTERVAL");
}
