use accessor_core::{Database, Value};

fn db() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Customers (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL, City TEXT, Credit CURRENCY DEFAULT 0);
         CREATE TABLE Orders (OrderID COUNTER PRIMARY KEY, CustID INTEGER, Total CURRENCY, Placed DATETIME);
         CREATE INDEX idx_orders_cust ON Orders (CustID);
         INSERT INTO Customers (Name, City) VALUES ('Ada', 'London'), ('Grace', 'NYC'), ('Linus', 'Helsinki'), ('Zed', NULL);
         INSERT INTO Orders (CustID, Total, Placed) VALUES
            (1, 10.50, #2024-01-05#), (1, 20, #2024-02-10#), (2, 5, #2024-02-11#), (2, 7.25, #2024-03-01#), (3, 100, #2024-03-15#);",
    )
    .unwrap();
    db
}

fn col(r: &accessor_core::QueryResult, c: usize) -> Vec<String> {
    r.rows.iter().map(|row| row[c].to_string()).collect()
}

#[test]
fn basic_select_and_where() {
    let db = db();
    let r = db.query("SELECT Name FROM Customers WHERE City = 'london'").unwrap();
    assert_eq!(col(&r, 0), vec!["Ada"]);
    let r = db.query("SELECT Name FROM Customers WHERE City IS NULL").unwrap();
    assert_eq!(col(&r, 0), vec!["Zed"]);
    let r = db.query("SELECT Name FROM Customers WHERE Name LIKE '*a*' ORDER BY Name").unwrap();
    assert_eq!(col(&r, 0), vec!["Ada", "Grace"]);
    let r = db.query("SELECT [Name], ID FROM Customers WHERE ID BETWEEN 2 AND 3 ORDER BY ID DESC").unwrap();
    assert_eq!(col(&r, 1), vec!["3", "2"]);
    assert_eq!(r.columns, vec!["Name", "ID"]);
}

#[test]
fn null_semantics() {
    let db = db();
    // City <> 'x' is unknown for the NULL city, so Zed is excluded.
    let r = db.query("SELECT Name FROM Customers WHERE City <> 'x'").unwrap();
    assert_eq!(r.rows.len(), 3);
    let r = db.query("SELECT Name FROM Customers WHERE NOT (City = 'NYC') ORDER BY Name").unwrap();
    assert_eq!(col(&r, 0), vec!["Ada", "Linus"]);
    let r = db.query("SELECT Nz(City, 'n/a') FROM Customers WHERE ID = 4").unwrap();
    assert_eq!(col(&r, 0), vec!["n/a"]);
}

#[test]
fn joins() {
    let db = db();
    let r = db
        .query(
            "SELECT c.Name, o.Total FROM Customers c INNER JOIN Orders o ON c.ID = o.CustID
             WHERE o.Total > 6 ORDER BY o.Total",
        )
        .unwrap();
    assert_eq!(col(&r, 0), vec!["Grace", "Ada", "Ada", "Linus"]);

    let r = db
        .query("SELECT c.Name, o.OrderID FROM Customers c LEFT JOIN Orders o ON c.ID = o.CustID WHERE c.ID = 4")
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert!(r.rows[0][1].is_null());

    let r = db
        .query("SELECT c.Name, o.OrderID FROM Orders o RIGHT JOIN Customers c ON c.ID = o.CustID")
        .unwrap();
    assert_eq!(r.rows.len(), 6); // 5 orders + Zed

    // Access-style parenthesised joins and non-equi conditions.
    let r = db
        .query(
            "SELECT a.Name, b.Name FROM (Customers a INNER JOIN Customers b ON a.ID < b.ID) WHERE a.ID = 1 ORDER BY b.ID",
        )
        .unwrap();
    assert_eq!(col(&r, 1), vec!["Grace", "Linus", "Zed"]);
}

#[test]
fn grouping_and_aggregates() {
    let db = db();
    let r = db
        .query(
            "SELECT CustID, Count(*) AS N, Sum(Total) AS S, Max(Total), Avg(Total)
             FROM Orders GROUP BY CustID HAVING Count(*) > 1 ORDER BY CustID",
        )
        .unwrap();
    assert_eq!(col(&r, 0), vec!["1", "2"]);
    assert_eq!(col(&r, 1), vec!["2", "2"]);
    assert_eq!(col(&r, 2), vec!["$30.50", "$12.25"]);
    assert_eq!(r.columns[1], "N");

    // Global aggregate over empty input yields one row.
    let r = db.query("SELECT Count(*), Sum(Total) FROM Orders WHERE Total > 1000").unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0][0], Value::Int(0));
    assert!(r.rows[0][1].is_null());

    let r = db.query("SELECT Count(DISTINCT CustID) FROM Orders").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(3));

    // Expression over aggregates and group keys.
    let r = db.query("SELECT CustID + 100, Sum(Total) * 2 FROM Orders GROUP BY CustID ORDER BY 1").unwrap();
    assert_eq!(col(&r, 0), vec!["101", "102", "103"]);

    assert!(db.query("SELECT City, Count(*) FROM Customers").is_err());
}

#[test]
fn distinct_top_limit_order() {
    let db = db();
    let r = db.query("SELECT DISTINCT CustID FROM Orders ORDER BY CustID DESC").unwrap();
    assert_eq!(col(&r, 0), vec!["3", "2", "1"]);
    let r = db.query("SELECT TOP 2 Total FROM Orders ORDER BY Total DESC").unwrap();
    assert_eq!(col(&r, 0), vec!["$100.00", "$20.00"]);
    let r = db.query("SELECT OrderID FROM Orders ORDER BY OrderID LIMIT 2 OFFSET 1").unwrap();
    assert_eq!(col(&r, 0), vec!["2", "3"]);
    // Top-k path (k small relative to input) keeps stable tie order.
    let r = db.query("SELECT OrderID FROM Orders ORDER BY CustID LIMIT 2").unwrap();
    assert_eq!(col(&r, 0), vec!["1", "2"]);
}

#[test]
fn subqueries_and_views() {
    let mut db = db();
    let r = db
        .query("SELECT Name FROM Customers WHERE ID IN (SELECT CustID FROM Orders WHERE Total >= 20) ORDER BY Name")
        .unwrap();
    assert_eq!(col(&r, 0), vec!["Ada", "Linus"]);
    let r = db.query("SELECT Name FROM Customers WHERE ID NOT IN (SELECT CustID FROM Orders)").unwrap();
    assert_eq!(col(&r, 0), vec!["Zed"]);
    let r = db.query("SELECT (SELECT Max(Total) FROM Orders)").unwrap();
    assert_eq!(col(&r, 0), vec!["$100.00"]);

    db.execute("CREATE VIEW BigSpenders AS SELECT CustID, Sum(Total) AS Spent FROM Orders GROUP BY CustID HAVING Sum(Total) > 20")
        .unwrap();
    let r = db
        .query("SELECT c.Name, b.Spent FROM BigSpenders b INNER JOIN Customers c ON c.ID = b.CustID ORDER BY c.Name")
        .unwrap();
    assert_eq!(col(&r, 0), vec!["Ada", "Linus"]);
    let r = db.query("SELECT x.N FROM (SELECT Count(*) AS N FROM Orders) AS x").unwrap();
    assert_eq!(col(&r, 0), vec!["5"]);
}

#[test]
fn functions_and_operators() {
    let db = db();
    let q = |sql: &str| db.query(sql).unwrap().rows[0][0].to_string();
    assert_eq!(q("SELECT UCase('abc') & '-' & Len('hello')"), "ABC-5");
    assert_eq!(q("SELECT Left('abcdef', 2) & Right('abcdef', 2) & Mid('abcdef', 3, 2)"), "abefcd");
    assert_eq!(q("SELECT IIf(1 < 2, 'yes', 'no')"), "yes");
    assert_eq!(q("SELECT 7 \\ 2"), "3");
    assert_eq!(q("SELECT 7 Mod 4"), "3");
    assert_eq!(q("SELECT 2 ^ 10"), "1024");
    assert_eq!(q("SELECT Round(2.5)"), "2");
    assert_eq!(q("SELECT Round(3.14159, 2)"), "3.14");
    assert_eq!(q("SELECT InStr('Hello World', 'wor')"), "7");
    assert_eq!(q("SELECT Year(#2024-02-29#) * 100 + Month(#2024-02-29#)"), "202402");
    assert_eq!(q("SELECT DateAdd('m', 1, #2024-01-31#)"), "2024-02-29");
    assert_eq!(q("SELECT DateDiff('d', #2024-01-01#, #2024-03-01#)"), "60");
    assert_eq!(q("SELECT #2024-03-01# - #2024-01-01#"), "60");
    assert_eq!(q("SELECT CASE WHEN 1 > 2 THEN 'a' WHEN 2 > 1 THEN 'b' END"), "b");
    assert_eq!(q("SELECT 5 IN (1, 2, 5)"), "Yes");
    assert_eq!(q("SELECT 'abc' LIKE 'a?c'"), "Yes");
    assert_eq!(q("SELECT Val('12.5abc')"), "12.5");
    assert!(db.query("SELECT 1 / 0").is_err());
}

#[test]
fn dml_roundtrip() {
    let mut db = db();
    let r = db.execute("UPDATE Customers SET City = 'Paris', Credit = Credit + 5 WHERE Name = 'Zed'").unwrap();
    assert_eq!(r.affected, 1);
    let r = db.query("SELECT City, Credit FROM Customers WHERE Name = 'zed'").unwrap();
    assert_eq!(r.rows[0][0], Value::text("Paris"));
    assert_eq!(r.rows[0][1].to_string(), "$5.00");

    let r = db.execute("DELETE FROM Orders WHERE Total < 8").unwrap();
    assert_eq!(r.affected, 2);
    assert_eq!(db.table("Orders").unwrap().len(), 3);

    db.execute("INSERT INTO Customers (Name) SELECT Name & '2' FROM Customers WHERE ID <= 2").unwrap();
    let r = db.query("SELECT Count(*) FROM Customers").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(6));
    // AutoNumber continues after explicit values.
    db.execute("INSERT INTO Customers (ID, Name) VALUES (100, 'Big')").unwrap();
    db.execute("INSERT INTO Customers (Name) VALUES ('After')").unwrap();
    let r = db.query("SELECT ID FROM Customers WHERE Name = 'After'").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(101));
}

#[test]
fn constraints_are_atomic() {
    let mut db = db();
    assert!(db.execute("INSERT INTO Customers (ID, Name) VALUES (1, 'dup')").is_err());
    assert!(db.execute("INSERT INTO Customers (Name) VALUES (NULL)").is_err());
    // Multi-row insert with a failing last row inserts nothing.
    let before = db.table("Customers").unwrap().len();
    assert!(db.execute("INSERT INTO Customers (ID, Name) VALUES (50, 'ok'), (1, 'dup')").is_err());
    assert_eq!(db.table("Customers").unwrap().len(), before);
    // Type errors.
    assert!(db.execute("INSERT INTO Orders (CustID) VALUES ('abc')").is_err());
    // Unique index.
    db.execute("CREATE UNIQUE INDEX u_name ON Customers (Name)").unwrap();
    assert!(db.execute("INSERT INTO Customers (Name) VALUES ('ada')").is_err()); // case-insensitive
    assert!(db.execute("CREATE UNIQUE INDEX u_cust ON Orders (CustID)").is_err());
}

#[test]
fn transactions() {
    let mut db = db();
    db.execute("BEGIN").unwrap();
    db.execute("DELETE FROM Orders").unwrap();
    db.execute("UPDATE Customers SET Name = 'X' WHERE ID = 1").unwrap();
    assert_eq!(db.table("Orders").unwrap().len(), 0);
    db.execute("ROLLBACK").unwrap();
    assert_eq!(db.table("Orders").unwrap().len(), 5);
    let r = db.query("SELECT Name FROM Customers WHERE ID = 1").unwrap();
    assert_eq!(r.rows[0][0], Value::text("Ada"));

    db.execute("BEGIN; INSERT INTO Customers (Name) VALUES ('T'); COMMIT").unwrap();
    assert_eq!(db.table("Customers").unwrap().len(), 5);
    // Schema changes take part too.
    db.execute("BEGIN").unwrap();
    db.execute("CREATE TABLE z (a INTEGER)").unwrap();
    db.execute("ROLLBACK").unwrap();
    assert!(db.table("z").is_none());
}

#[test]
fn index_paths_agree_with_scans() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE n (id INTEGER PRIMARY KEY, grp INTEGER, name TEXT)").unwrap();
    db.execute("BEGIN").unwrap();
    for i in 0..2000 {
        db.execute(&format!("INSERT INTO n VALUES ({i}, {}, 'name{}')", i % 17, i % 50)).unwrap();
    }
    db.execute("COMMIT").unwrap();
    db.execute("CREATE INDEX g ON n (grp, id)").unwrap();
    db.execute("CREATE TABLE m (id INTEGER, grp INTEGER, name TEXT)").unwrap(); // same data, no indexes
    db.execute("INSERT INTO m SELECT * FROM n").unwrap();

    for pred in [
        "id = 1234",
        "id > 1990",
        "id >= 1990 AND id < 1995",
        "id BETWEEN 10 AND 14",
        "grp = 3 AND id > 1500",
        "grp = 3",
        "10 > id",
        "id < 5 OR id > 1995",
        "id = 7.0",
        "name = 'NAME7'",
    ] {
        let a = db.query(&format!("SELECT id FROM n WHERE {pred} ORDER BY id")).unwrap();
        let b = db.query(&format!("SELECT id FROM m WHERE {pred} ORDER BY id")).unwrap();
        assert_eq!(a.rows, b.rows, "predicate: {pred}");
    }
    // Index nested loop join vs hash join must agree.
    let a = db.query("SELECT Count(*) FROM n a INNER JOIN n b ON a.id = b.grp").unwrap();
    let b = db.query("SELECT Count(*) FROM m a INNER JOIN m b ON a.id = b.grp").unwrap();
    assert_eq!(a.rows, b.rows);
    // Updating an indexed column keeps the index consistent.
    db.execute("UPDATE n SET id = id + 10000 WHERE id < 10").unwrap();
    let r = db.query("SELECT Count(*) FROM n WHERE id >= 10000").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(10));
    let r = db.query("SELECT Count(*) FROM n WHERE id = 10003").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(1));
}

#[test]
fn errors_are_helpful() {
    let db = db();
    let msg = |sql: &str| db.query(sql).unwrap_err().to_string();
    assert!(msg("SELECT Nope FROM Customers").contains("Nope"));
    assert!(msg("SELECT * FROM Missing").contains("Missing"));
    assert!(msg("SELECT ID FROM Customers a, Customers b").contains("ambiguous"));
    assert!(msg("SELECT Bogus(1)").to_lowercase().contains("bogus"));
    assert!(msg("SELEKT 1").to_lowercase().contains("syntax"));
}

#[test]
fn alter_table_columns() {
    let mut db = db();
    db.execute("ALTER TABLE Customers ADD COLUMN Tier TEXT DEFAULT 'std'").unwrap();
    let r = db.query("SELECT Tier FROM Customers ORDER BY ID").unwrap();
    assert!(r.rows.iter().all(|row| row[0] == Value::text("std")));
    // Required column with no default cannot be added to a populated table.
    assert!(db.execute("ALTER TABLE Customers ADD COLUMN Req TEXT NOT NULL").is_err());
    assert!(db.execute("ALTER TABLE Customers ADD COLUMN tier TEXT").is_err());

    db.execute("ALTER TABLE Customers RENAME COLUMN City TO Town").unwrap();
    assert_eq!(col(&db.query("SELECT Town FROM Customers WHERE ID = 1").unwrap(), 0), vec!["London"]);
    assert!(db.query("SELECT City FROM Customers").is_err());

    // Dropping an indexed column is refused; dropping one before an indexed column shifts positions.
    assert!(db.execute("ALTER TABLE Orders DROP COLUMN CustID").is_err());
    db.execute("ALTER TABLE Orders ADD COLUMN Note TEXT").unwrap();
    db.execute("ALTER TABLE Orders DROP COLUMN OrderID").unwrap_err(); // primary key
    db.execute("DROP INDEX idx_orders_cust ON Orders").unwrap();
    db.execute("ALTER TABLE Orders DROP COLUMN CustID").unwrap();
    db.execute("INSERT INTO Orders (Total) VALUES (1)").unwrap();
    assert_eq!(col(&db.query("SELECT Count(*) FROM Orders WHERE OrderID = 6").unwrap(), 0), vec!["1"]);
}

#[test]
fn alter_column_type_is_atomic() {
    let mut db = db();
    db.execute("ALTER TABLE Customers ALTER COLUMN Credit TEXT").unwrap();
    assert_eq!(col(&db.query("SELECT Credit FROM Customers WHERE ID = 1").unwrap(), 0).len(), 1);
    // Names are not numbers: conversion fails and leaves data intact.
    assert!(db.execute("ALTER TABLE Customers ALTER COLUMN Name INTEGER").is_err());
    assert_eq!(col(&db.query("SELECT Name FROM Customers WHERE ID = 1").unwrap(), 0), vec!["Ada"]);
    assert!(db.execute("ALTER TABLE Customers ALTER COLUMN ID TEXT").is_err());
}

fn fk_db() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Customers (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL);
         CREATE TABLE Orders (
            OrderID COUNTER PRIMARY KEY,
            CustID INTEGER REFERENCES Customers (ID) ON DELETE CASCADE ON UPDATE CASCADE,
            Total CURRENCY
         );
         CREATE TABLE Notes (
            NoteID COUNTER PRIMARY KEY,
            CustID INTEGER,
            Body TEXT,
            CONSTRAINT fk_notes FOREIGN KEY (CustID) REFERENCES Customers ON DELETE SET NULL ON UPDATE CASCADE
         );
         INSERT INTO Customers (Name) VALUES ('Ada'), ('Grace');
         INSERT INTO Orders (CustID, Total) VALUES (1, 10), (1, 20), (2, 5), (NULL, 1);
         INSERT INTO Notes (CustID, Body) VALUES (1, 'a'), (2, 'b');",
    )
    .unwrap();
    db
}

fn count(db: &Database, sql: &str) -> String {
    db.query(sql).unwrap().rows[0][0].to_string()
}

#[test]
fn fk_rejects_orphans() {
    let mut db = fk_db();
    assert!(db.execute("INSERT INTO Orders (CustID, Total) VALUES (99, 1)").is_err());
    assert!(db.execute("UPDATE Orders SET CustID = 99 WHERE OrderID = 1").is_err());
    // A failed multi-row insert leaves nothing behind.
    assert!(db.execute("INSERT INTO Orders (CustID) VALUES (1), (99)").is_err());
    assert_eq!(count(&db, "SELECT Count(*) FROM Orders"), "4");
    // Null is always allowed.
    db.execute("INSERT INTO Orders (CustID) VALUES (NULL)").unwrap();
    db.execute("UPDATE Orders SET CustID = 2 WHERE OrderID = 1").unwrap();
}

#[test]
fn fk_cascade_and_set_null() {
    let mut db = fk_db();
    db.execute("DELETE FROM Customers WHERE ID = 1").unwrap();
    assert_eq!(count(&db, "SELECT Count(*) FROM Orders"), "2"); // cascade removed two
    assert_eq!(count(&db, "SELECT Count(*) FROM Notes WHERE CustID IS NULL"), "1"); // set null
    db.execute("UPDATE Customers SET ID = 50 WHERE ID = 2").unwrap();
    assert_eq!(count(&db, "SELECT CustID FROM Orders WHERE Total = 5"), "50");
}

#[test]
fn fk_restrict_is_default() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE P (ID INTEGER PRIMARY KEY);
         CREATE TABLE C (ID INTEGER PRIMARY KEY, P INTEGER REFERENCES P);
         INSERT INTO P VALUES (1), (2); INSERT INTO C VALUES (1, 1);",
    )
    .unwrap();
    assert!(db.execute("DELETE FROM P WHERE ID = 1").is_err());
    assert!(db.execute("UPDATE P SET ID = 9 WHERE ID = 1").is_err());
    db.execute("DELETE FROM P WHERE ID = 2").unwrap();
    db.execute("DELETE FROM C").unwrap();
    db.execute("DELETE FROM P").unwrap();
}

#[test]
fn fk_self_reference_and_transaction_rollback() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Emp (ID INTEGER PRIMARY KEY, Boss INTEGER REFERENCES Emp ON DELETE CASCADE);
         INSERT INTO Emp VALUES (1, NULL), (2, 1), (3, 2), (4, NULL);",
    )
    .unwrap();
    db.execute("BEGIN").unwrap();
    db.execute("DELETE FROM Emp WHERE ID = 1").unwrap();
    assert_eq!(count(&db, "SELECT Count(*) FROM Emp"), "1");
    db.execute("ROLLBACK").unwrap();
    assert_eq!(count(&db, "SELECT Count(*) FROM Emp"), "4");
    // Rows may reference each other within one statement.
    db.execute("INSERT INTO Emp VALUES (10, 11), (11, NULL)").unwrap();
}

#[test]
fn fk_definition_rules_and_guards() {
    let mut db = fk_db();
    // Referenced fields need a unique index; types must match.
    assert!(db.execute("CREATE TABLE X (a INTEGER REFERENCES Customers (Name))").is_err());
    assert!(db.execute("CREATE TABLE X (a TEXT REFERENCES Customers (ID))").is_err());
    assert!(db.execute("CREATE TABLE X (a INTEGER REFERENCES Nope)").is_err());
    // Relationship members cannot be dropped or retyped; neither can their table.
    assert!(db.execute("DROP TABLE Customers").is_err());
    assert!(db.execute("ALTER TABLE Orders DROP COLUMN CustID").is_err());
    assert!(db.execute("ALTER TABLE Customers ALTER COLUMN ID TEXT").is_err());
    assert!(db.execute("DROP INDEX PrimaryKey ON Customers").is_err());
    // Dropping a column before a related one keeps the relationship intact.
    db.execute("ALTER TABLE Orders DROP COLUMN Total").unwrap();
    db.execute("INSERT INTO Orders (CustID) VALUES (2)").unwrap();
    assert!(db.execute("INSERT INTO Orders (CustID) VALUES (77)").is_err());
    // Removing the constraint frees everything.
    db.execute("ALTER TABLE Orders DROP CONSTRAINT fk_Orders_CustID").unwrap();
    db.execute("INSERT INTO Orders (CustID) VALUES (77)").unwrap();
}

#[test]
fn fk_added_later_checks_existing_rows() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE P (ID INTEGER PRIMARY KEY);
         CREATE TABLE C (ID INTEGER PRIMARY KEY, P INTEGER);
         INSERT INTO P VALUES (1); INSERT INTO C VALUES (1, 1), (2, 5);",
    )
    .unwrap();
    assert!(db.execute("ALTER TABLE C ADD FOREIGN KEY (P) REFERENCES P").is_err());
    db.execute("UPDATE C SET P = 1").unwrap();
    db.execute("ALTER TABLE C ADD CONSTRAINT fk FOREIGN KEY (P) REFERENCES P (ID) ON DELETE CASCADE").unwrap();
    db.execute("DELETE FROM P").unwrap();
    assert_eq!(count(&db, "SELECT Count(*) FROM C"), "0");
    db.execute("ALTER TABLE C ADD COLUMN Q INTEGER REFERENCES P").unwrap();
}

// ----- parameterised queries ---------------------------------------------------

fn music() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Artist (ID INTEGER PRIMARY KEY, Name TEXT NOT NULL);
         CREATE TABLE Album (ID INTEGER PRIMARY KEY, Name TEXT NOT NULL, Artist_ID INTEGER NOT NULL REFERENCES Artist (ID), Year INTEGER);
         INSERT INTO Artist VALUES (1, 'Grand Funk Railroad'), (2, 'Yes');
         INSERT INTO Album VALUES (1, 'On Time', 1, 1969), (2, 'Close to the Edge', 2, 1972), (3, 'Closer to Home', 1, 1970);",
    )
    .unwrap();
    db
}

fn names(r: &accessor_core::QueryResult) -> Vec<String> {
    r.rows.iter().map(|row| row[0].to_string()).collect()
}

#[test]
fn parameterised_queries() {
    let mut db = music();
    db.execute(
        "CREATE QUERY AlbumsBy (@artist, @since Integer) AS
           SELECT AL.Name FROM Album AL INNER JOIN Artist AR ON AR.ID = AL.Artist_ID
           WHERE AR.Name = @artist AND AL.Year >= @since ORDER BY AL.Year",
    )
    .unwrap();
    assert_eq!(db.query_parameters("AlbumsBy").unwrap(), vec!["artist", "since"]);
    assert!(db.view_sql("AlbumsBy").unwrap().to_lowercase().starts_with("parameters @artist, @since integer;"));

    let r = db.execute("EXEC AlbumsBy 'Grand Funk Railroad', 1970").unwrap();
    assert_eq!(names(&r), vec!["Closer to Home"]);
    // Named arguments, in any order; the declared type converts text.
    let r = db.execute("EXEC AlbumsBy @since = '1969', @artist = 'grand funk railroad'").unwrap();
    assert_eq!(names(&r), vec!["On Time", "Closer to Home"]);
    let r = db.execute("EXEC AlbumsBy('Yes', 0)").unwrap();
    assert_eq!(names(&r), vec!["Close to the Edge"]);

    assert!(db.execute("EXEC AlbumsBy 'Yes'").is_err()); // too few
    assert!(db.execute("EXEC AlbumsBy 'Yes', 1, 2").is_err()); // too many
    assert!(db.execute("EXEC AlbumsBy @nope = 1, 'Yes'").is_err()); // unknown name
    assert!(db.execute("EXEC AlbumsBy 'Yes', 'abc'").is_err()); // not an Integer
    assert!(db.execute("EXEC Missing 1").is_err());
    // Running it as a plain query, or using it as a table, needs EXEC.
    assert!(db.query("SELECT * FROM AlbumsBy").is_err());
    assert!(db.query("SELECT Name FROM Artist WHERE Name = @x").is_err());
}

#[test]
fn query_definition_rules() {
    let mut db = music();
    // Undeclared parameter.
    assert!(db.execute("CREATE QUERY q (@a) AS SELECT Name FROM Artist WHERE ID = @b").is_err());
    // PARAMETERS header form, and a query with no parameters via EXEC.
    db.execute("CREATE QUERY q2 AS PARAMETERS @id Integer; SELECT Name FROM Artist WHERE ID = @id").unwrap();
    assert_eq!(names(&db.execute("EXEC q2 2").unwrap()), vec!["Yes"]);
    db.execute("CREATE VIEW plain AS SELECT Name FROM Artist ORDER BY Name").unwrap();
    assert_eq!(names(&db.execute("EXEC plain").unwrap()), vec!["Grand Funk Railroad", "Yes"]);
    assert!(db.execute("CREATE QUERY q3 (@a) AS PARAMETERS @b; SELECT 1").is_err());
}

#[test]
fn execute_with_params_binds_values() {
    let mut db = music();
    let sql = "PARAMETERS @id Integer; SELECT Name FROM Artist WHERE ID = @id";
    let r = db.execute_with_params(sql, &[("id", Value::text("2"))]).unwrap();
    assert_eq!(names(&r), vec!["Yes"]);
    assert!(db.execute_with_params(sql, &[]).is_err());
    let r = db.execute_with_params("SELECT Name FROM Artist WHERE ID = @id", &[("@ID", Value::Int(1))]).unwrap();
    assert_eq!(names(&r), vec!["Grand Funk Railroad"]);
}

// ----- triggers ---------------------------------------------------------------------

fn triggered() -> Database {
    let mut db = music();
    db.execute(
        "CREATE TABLE Log (ID COUNTER PRIMARY KEY, Msg TEXT);
         CREATE TRIGGER album_added AFTER INSERT ON Album FOR EACH ROW
         BEGIN
           INSERT INTO Log (Msg) VALUES ('added ' & NEW.Name);
         END;
         CREATE TRIGGER album_year BEFORE INSERT ON Album
         WHEN NEW.Year < 1900
         BEGIN
           RAISE 'the year must be 1900 or later';
         END",
    )
    .unwrap();
    db
}

#[test]
fn triggers_log_and_validate() {
    let mut db = triggered();
    db.execute("INSERT INTO Album VALUES (4, 'Wowie', 2, 1980)").unwrap();
    assert_eq!(names(&db.query("SELECT Msg FROM Log").unwrap()), vec!["added Wowie"]);

    let err = db.execute("INSERT INTO Album VALUES (5, 'Old', 1, 1850)").unwrap_err().to_string();
    assert!(err.contains("1900"), "{err}");
    // A failing row rolls back the whole statement, including trigger effects.
    assert!(db.execute("INSERT INTO Album VALUES (6, 'Fine', 1, 1990), (7, 'Bad', 1, 1800)").is_err());
    assert_eq!(count(&db, "SELECT Count(*) FROM Album"), "4");
    assert_eq!(count(&db, "SELECT Count(*) FROM Log"), "1");
    assert_eq!(db.trigger_names().len(), 2);
    assert!(db.trigger_sql("album_year").unwrap().starts_with("CREATE TRIGGER album_year"));
}

#[test]
fn triggers_see_old_and_new() {
    let mut db = music();
    db.execute(
        "CREATE TABLE Hist (ID COUNTER PRIMARY KEY, Note TEXT);
         CREATE TABLE Stats (Changes INTEGER);
         INSERT INTO Stats VALUES (0);
         CREATE TRIGGER year_changed AFTER UPDATE ON Album WHEN OLD.Year <> NEW.Year
         BEGIN
           INSERT INTO Hist (Note) VALUES (OLD.Name & ': ' & OLD.Year & ' -> ' & NEW.Year);
           UPDATE Stats SET Changes = Changes + 1;
         END;
         CREATE TRIGGER album_gone BEFORE DELETE ON Album
         BEGIN
           INSERT INTO Hist (Note) VALUES ('deleted ' & OLD.Name);
         END",
    )
    .unwrap();
    db.execute("UPDATE Album SET Year = Year + 1").unwrap();
    assert_eq!(count(&db, "SELECT Changes FROM Stats"), "3");
    db.execute("UPDATE Album SET Name = 'x' WHERE ID = 1").unwrap(); // year unchanged: WHEN is false
    assert_eq!(count(&db, "SELECT Changes FROM Stats"), "3");
    assert_eq!(names(&db.query("SELECT Note FROM Hist ORDER BY ID LIMIT 1").unwrap()), vec!["On Time: 1969 -> 1970"]);
    db.execute("DELETE FROM Album WHERE ID = 2").unwrap();
    assert_eq!(count(&db, "SELECT Count(*) FROM Hist WHERE Note = 'deleted Close to the Edge'"), "1");

    // Rolled-back transactions undo trigger effects too.
    db.execute("BEGIN").unwrap();
    db.execute("DELETE FROM Album").unwrap();
    db.execute("ROLLBACK").unwrap();
    assert_eq!(count(&db, "SELECT Count(*) FROM Hist WHERE Note LIKE 'deleted*'"), "1");
}

#[test]
fn triggers_fire_for_cascades() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE P (ID INTEGER PRIMARY KEY);
         CREATE TABLE C (ID INTEGER PRIMARY KEY, P INTEGER REFERENCES P ON DELETE CASCADE);
         CREATE TABLE Audit (Msg TEXT);
         CREATE TRIGGER c_gone AFTER DELETE ON C BEGIN INSERT INTO Audit VALUES ('c' & OLD.ID); END;
         INSERT INTO P VALUES (1); INSERT INTO C VALUES (1, 1), (2, 1);
         DELETE FROM P",
    )
    .unwrap();
    assert_eq!(names(&db.query("SELECT Msg FROM Audit ORDER BY Msg").unwrap()), vec!["c1", "c2"]);
}

#[test]
fn trigger_definition_rules() {
    let mut db = triggered();
    let bad = |db: &mut Database, sql: &str| assert!(db.execute(sql).is_err(), "{sql}");
    bad(&mut db, "CREATE TRIGGER album_added AFTER INSERT ON Album BEGIN RAISE 'x'; END"); // duplicate name
    bad(&mut db, "CREATE TRIGGER t AFTER INSERT ON Nope BEGIN RAISE 'x'; END");
    bad(&mut db, "CREATE TRIGGER t AFTER DELETE ON Album BEGIN INSERT INTO Log (Msg) VALUES (NEW.Name); END");
    bad(&mut db, "CREATE TRIGGER t AFTER INSERT ON Album BEGIN INSERT INTO Log (Msg) VALUES (OLD.Name); END");
    bad(&mut db, "CREATE TRIGGER t AFTER INSERT ON Album BEGIN INSERT INTO Log (Msg) VALUES (NEW.Nope); END");
    bad(&mut db, "CREATE TRIGGER t AFTER INSERT ON Album BEGIN INSERT INTO Album VALUES (9, 'x', 1, 2000); END");
    bad(&mut db, "CREATE TRIGGER t AFTER INSERT ON Album BEGIN SELECT 1; END");
    bad(&mut db, "CREATE TRIGGER t AFTER INSERT ON Album BEGIN END");
    bad(&mut db, "RAISE 'outside a trigger'");

    // Triggers can be dropped, and go away with their table.
    db.execute("DROP TRIGGER album_year").unwrap();
    db.execute("INSERT INTO Album VALUES (8, 'Ancient', 1, 1500)").unwrap();
    assert!(db.execute("DROP TRIGGER album_year").is_err());
    db.execute("DROP TABLE Album").unwrap();
    assert!(db.trigger_names().is_empty());
}

#[test]
fn runaway_triggers_are_stopped() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE A (ID INTEGER PRIMARY KEY); CREATE TABLE B (ID INTEGER PRIMARY KEY);
         CREATE TRIGGER a_to_b AFTER INSERT ON A BEGIN INSERT INTO B VALUES (NEW.ID); END;
         CREATE TRIGGER b_to_a AFTER INSERT ON B BEGIN INSERT INTO A VALUES (NEW.ID + 1); END",
    )
    .unwrap();
    let err = db.execute("INSERT INTO A VALUES (1)").unwrap_err().to_string();
    assert!(err.contains("nested too deeply"), "{err}");
    assert_eq!(count(&db, "SELECT Count(*) FROM A"), "0");
    assert_eq!(count(&db, "SELECT Count(*) FROM B"), "0");
}

#[test]
fn saved_queries_can_call_other_queries() {
    let mut db = music();
    db.execute(
        "CREATE QUERY AlbumsBy (@artist) AS
           SELECT AL.Name FROM Album AL JOIN Artist AR ON AR.ID = AL.Artist_ID WHERE AR.Name = @artist ORDER BY AL.Year;
         CREATE QUERY GrandFunk AS EXEC AlbumsBy 'Grand Funk Railroad';
         CREATE QUERY YesAlbums AS EXECUTE AlbumsBy @artist = 'Yes'",
    )
    .unwrap();
    assert_eq!(db.view_sql("GrandFunk").unwrap(), "EXEC AlbumsBy 'Grand Funk Railroad'");
    assert_eq!(names(&db.execute("EXEC GrandFunk").unwrap()), vec!["On Time", "Closer to Home"]);
    assert_eq!(names(&db.execute("EXEC YesAlbums").unwrap()), vec!["Close to the Edge"]);
    // Usable as a table, and it has no parameters left.
    assert_eq!(names(&db.query("SELECT Name FROM GrandFunk WHERE Name LIKE 'Close*'").unwrap()), vec!["Closer to Home"]);
    assert!(db.query_parameters("GrandFunk").unwrap().is_empty());
    // Calls chain.
    db.execute("CREATE QUERY Again AS EXEC GrandFunk").unwrap();
    assert_eq!(names(&db.execute("EXEC Again").unwrap()).len(), 2);

    // A bad call is rejected when saved, not when first run.
    assert!(db.execute("CREATE QUERY Bad AS EXEC Nope 1").is_err());
    assert!(db.execute("CREATE QUERY Bad AS EXEC AlbumsBy").is_err());
    assert!(db.execute("CREATE QUERY Bad (@x) AS EXEC AlbumsBy 'Yes'").is_err());
}

// ----- calculated fields ----------------------------------------------------------

fn lines() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Line (
            ID COUNTER PRIMARY KEY,
            Item TEXT NOT NULL,
            Qty INTEGER,
            Price CURRENCY,
            Total CALCULATED AS (Qty * Price),
            Label CALCULATED AS (UCase(Item) & ' x' & Qty)
         );
         INSERT INTO Line (Item, Qty, Price) VALUES ('pen', 3, 2), ('ink', 2, 5.5)",
    )
    .unwrap();
    db
}

fn one(db: &Database, sql: &str) -> String {
    db.query(sql).unwrap().rows[0][0].to_string()
}

#[test]
fn calculated_fields_follow_their_record() {
    let mut db = lines();
    assert_eq!(one(&db, "SELECT Total FROM Line WHERE Item = 'pen'"), "$6.00");
    assert_eq!(one(&db, "SELECT Label FROM Line WHERE Item = 'ink'"), "INK x2");
    // The field is part of SELECT * like any other.
    assert_eq!(db.query("SELECT * FROM Line").unwrap().columns, ["ID", "Item", "Qty", "Price", "Total", "Label"]);

    db.execute("UPDATE Line SET Qty = 10 WHERE Item = 'pen'").unwrap();
    assert_eq!(one(&db, "SELECT Total FROM Line WHERE Item = 'pen'"), "$20.00");
    assert_eq!(one(&db, "SELECT Label FROM Line WHERE Item = 'pen'"), "PEN x10");
    // Other records are untouched, and the value can be filtered and sorted on.
    assert_eq!(one(&db, "SELECT Total FROM Line WHERE Item = 'ink'"), "$11.00");
    assert_eq!(names(&db.query("SELECT Item FROM Line WHERE Total > 15").unwrap()), ["pen"]);
    assert_eq!(names(&db.query("SELECT Item FROM Line ORDER BY Total").unwrap()), ["ink", "pen"]);

    // A Null input gives a Null result rather than an error.
    db.execute("INSERT INTO Line (Item) VALUES ('blank')").unwrap();
    assert_eq!(one(&db, "SELECT Count(*) FROM Line WHERE Total IS NULL"), "1");
}

#[test]
fn calculated_fields_cannot_be_written() {
    let mut db = lines();
    assert!(db.execute("UPDATE Line SET Total = 1").unwrap_err().to_string().contains("calculated"));
    assert!(db.execute("INSERT INTO Line (Item, Total) VALUES ('x', 1)").is_err());
    // Without a field list, values are for the fields that can be set.
    db.execute("INSERT INTO Line VALUES (10, 'cap', 4, 1.5)").unwrap();
    assert_eq!(one(&db, "SELECT Total FROM Line WHERE ID = 10"), "$6.00");
    // Same for the cell-level API used by the datasheet.
    let total = db.table("Line").unwrap().schema().column_index("Total").unwrap();
    assert!(db.update_cell("Line", 0, total, Value::Int(1)).is_err());
    assert!(db.insert_row("Line", &[(total, Value::Int(1))]).is_err());
    let qty = db.table("Line").unwrap().schema().column_index("Qty").unwrap();
    db.update_cell("Line", 0, qty, Value::Int(100)).unwrap();
    assert_eq!(one(&db, "SELECT Total FROM Line WHERE ID = 1"), "$200.00");
}

#[test]
fn calculated_fields_can_be_added_and_changed() {
    let mut db = lines();
    // Added to a table that already has records: they are computed immediately.
    db.execute("ALTER TABLE Line ADD COLUMN Double CALCULATED AS (Total * 2)").unwrap();
    assert_eq!(one(&db, "SELECT Double FROM Line WHERE Item = 'pen'"), "$12.00");
    // Changing the expression recomputes every record.
    db.execute("ALTER TABLE Line ALTER COLUMN Double CALCULATED AS (Total + Qty)").unwrap();
    assert_eq!(one(&db, "SELECT Double FROM Line WHERE Item = 'pen'"), "$9.00");
    assert!(db.execute("ALTER TABLE Line ALTER COLUMN Nope CALCULATED AS (1)").is_err()); // no such field
    // A calculated field can use earlier ones, and an index on it stays correct.
    db.execute("CREATE INDEX by_total ON Line (Total)").unwrap();
    db.execute("UPDATE Line SET Qty = 1 WHERE Item = 'pen'").unwrap();
    assert_eq!(names(&db.query("SELECT Item FROM Line WHERE Total = 2").unwrap()), ["pen"]);
    assert_eq!(names(&db.query("SELECT Item FROM Line WHERE Total = 6").unwrap()).len(), 0);

    // Turning it into an ordinary field keeps the current values and drops the formula.
    db.execute("ALTER TABLE Line ALTER COLUMN Double INTEGER").unwrap();
    db.execute("UPDATE Line SET Qty = 50").unwrap();
    assert_eq!(one(&db, "SELECT Double FROM Line WHERE Item = 'pen'"), "3");
    assert!(db.table("Line").unwrap().schema().columns.iter().all(|c| c.name != "Double" || c.expression().is_none()));
}

#[test]
fn calculated_expressions_are_checked() {
    let mut db = lines();
    let bad = |db: &mut Database, sql: &str| assert!(db.execute(sql).is_err(), "{sql}");
    bad(&mut db, "ALTER TABLE Line ADD COLUMN c CALCULATED AS (Nope + 1)");
    bad(&mut db, "ALTER TABLE Line ADD COLUMN c CALCULATED AS (Now())");
    bad(&mut db, "ALTER TABLE Line ADD COLUMN c CALCULATED AS (Date())");
    bad(&mut db, "ALTER TABLE Line ADD COLUMN c CALCULATED AS ((SELECT 1))");
    bad(&mut db, "ALTER TABLE Line ADD COLUMN c CALCULATED AS (Sum(Qty))");
    bad(&mut db, "ALTER TABLE Line ADD COLUMN c CALCULATED AS (@p)");
    bad(&mut db, "ALTER TABLE Line ADD COLUMN c CALCULATED AS (c + 1)");
    bad(&mut db, "ALTER TABLE Line ADD COLUMN c CALCULATED AS (Other.Qty)");
    bad(&mut db, "ALTER TABLE Line ADD COLUMN c CALCULATED");
    bad(&mut db, "ALTER TABLE Line ADD COLUMN c CALCULATED AS (Qty) NOT NULL");
    bad(&mut db, "CREATE TABLE X (a INTEGER, b CALCULATED AS (c), c CALCULATED AS (a))"); // c comes later
    bad(&mut db, "CREATE TABLE X (a INTEGER, b CALCULATED AS (a), PRIMARY KEY (b))");
    // Division by zero at write time fails the write and leaves nothing behind.
    db.execute("ALTER TABLE Line ADD COLUMN Each CALCULATED AS (Total / Qty)").unwrap();
    assert!(db.execute("UPDATE Line SET Qty = 0 WHERE Item = 'pen'").is_err());
    assert_eq!(one(&db, "SELECT Qty FROM Line WHERE Item = 'pen'"), "3");
    // ...and a bad expression change is rolled back as a whole.
    assert!(db.execute("ALTER TABLE Line ALTER COLUMN Each CALCULATED AS (Total / (Qty - 3))").is_err());
    assert_eq!(one(&db, "SELECT Each FROM Line WHERE Item = 'pen'"), "$2.00");
}

#[test]
fn calculated_fields_protect_what_they_use() {
    let mut db = lines();
    assert!(db.execute("ALTER TABLE Line DROP COLUMN Qty").unwrap_err().to_string().contains("calculated field"));
    assert!(db.execute("ALTER TABLE Line RENAME COLUMN Price TO Cost").is_err());
    assert!(db.execute("ALTER TABLE Line ALTER COLUMN Qty TEXT").is_err());
    // Fields nothing depends on are free to change; so is the calculated field itself.
    db.execute("ALTER TABLE Line RENAME COLUMN Total TO Amount").unwrap();
    db.execute("ALTER TABLE Line DROP COLUMN Label").unwrap();
}

#[test]
fn calculated_fields_follow_cascades_and_triggers() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE P (ID INTEGER PRIMARY KEY);
         CREATE TABLE C (ID INTEGER PRIMARY KEY, P INTEGER REFERENCES P ON UPDATE CASCADE, Tag CALCULATED AS ('p' & P));
         CREATE TABLE Seen (Tag TEXT);
         CREATE TRIGGER c_seen AFTER INSERT ON C BEGIN INSERT INTO Seen VALUES (NEW.Tag); END;
         INSERT INTO P VALUES (1), (2);
         INSERT INTO C (ID, P) VALUES (1, 1)",
    )
    .unwrap();
    // The trigger saw the computed value.
    assert_eq!(one(&db, "SELECT Tag FROM Seen"), "p1");
    // A cascaded key change recomputes the dependent field.
    db.execute("UPDATE P SET ID = 7 WHERE ID = 1").unwrap();
    assert_eq!(one(&db, "SELECT Tag FROM C"), "p7");
}

#[test]
fn concatenation_is_a_single_ampersand() {
    let mut db = music();
    assert_eq!(one(&db, "SELECT Name & ' (' & Year & ')' FROM Album WHERE ID = 1"), "On Time (1969)");
    assert_eq!(one(&db, "SELECT Name || '!' FROM Album WHERE ID = 1"), "On Time!");
    assert!(db.query("SELECT Name && '!' FROM Album").is_err());

    // A bad formula is refused with a message that says what is wrong.
    let err = db
        .execute("ALTER TABLE Album ADD COLUMN Title CALCULATED AS (Name && ' (' && Year && ')')")
        .unwrap_err()
        .to_string();
    assert!(err.contains("Title") && err.contains("formula is not valid") && err.contains("single &"), "{err}");
    assert!(db.table("Album").unwrap().schema().column_index("Title").is_none());

    // The same check is available without changing anything.
    assert!(db.check_formula("Album", "Title", "Name & ' (' & Year & ')'").is_ok());
    let err = db.check_formula("Album", "Title", "Name && Year").unwrap_err().to_string();
    assert!(err.contains("formula is not valid"), "{err}");
    assert!(db.check_formula("Album", "Title", "Nope + 1").is_err());
    assert!(db.check_formula("Album", "Name", "1").is_err()); // a name already in use
    assert!(db.check_formula("Album", "", "Year + 1").is_ok());
}

// ----- the NUMBER type ----------------------------------------------------------------

fn q(db: &Database, sql: &str) -> String {
    db.query(sql).unwrap().rows[0][0].to_string()
}

#[test]
fn every_numeric_type_has_its_name() {
    use accessor_core::DataType::*;
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE T (a INTEGER, b DOUBLE, c CURRENCY, d INT, e FLOAT, f MONEY, g DECIMAL, h NUMERIC, i SMALLINT,
                         j BIGNUM, k RATIONAL, l REAL, m BIGINT, n NUMBER, o COUNTER PRIMARY KEY)",
    )
    .unwrap();
    let types: Vec<_> = db.table("T").unwrap().schema().columns.iter().map(|c| c.data_type).collect();
    assert_eq!(
        types,
        [Integer, Double, Currency, Integer, Float, Currency, Decimal, Decimal, Integer, Bignum, Rational, Float, Integer, Decimal, Integer]
    );
    assert!(db.table("T").unwrap().schema().columns[14].autonumber);
}

#[test]
fn numbers_are_unbounded_and_exact() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE N (id COUNTER PRIMARY KEY, x NUMBER)").unwrap();
    db.execute(
        "INSERT INTO N (x) VALUES (1), (2.5), (0.1), (123456789012345678901234567890), (-9223372036854775808),
                                  (9223372036854775807), (0.000000000000000000000000001), (1e3)",
    )
    .unwrap();
    // Nothing is lost or rounded.
    assert_eq!(q(&db, "SELECT x FROM N WHERE id = 4"), "123456789012345678901234567890");
    assert_eq!(q(&db, "SELECT x FROM N WHERE id = 7"), "0.000000000000000000000000001");
    assert_eq!(q(&db, "SELECT x FROM N WHERE id = 8"), "1000");
    // Whole numbers and fractions sort together by value, whatever their size.
    let ordered = db.query("SELECT x FROM N ORDER BY x").unwrap();
    let shown: Vec<String> = ordered.rows.iter().map(|r| r[0].to_string()).collect();
    assert_eq!(
        shown,
        [
            "-9223372036854775808",
            "0.000000000000000000000000001",
            "0.1",
            "1",
            "2.5",
            "1000",
            "9223372036854775807",
            "123456789012345678901234567890"
        ]
    );
    // Arithmetic grows past 64 bits instead of overflowing.
    assert_eq!(q(&db, "SELECT x * x FROM N WHERE id = 6"), "85070591730234615847396907784232501249");
    assert_eq!(q(&db, "SELECT 2 ^ 200"), "1606938044258990275541962092341162602522202993782792835301376");
    assert_eq!(q(&db, "SELECT 9223372036854775807 + 1"), "9223372036854775808");
}

#[test]
fn decimal_arithmetic_is_exact() {
    let db = Database::in_memory();
    assert_eq!(q(&db, "SELECT 0.1 + 0.2"), "0.3");
    assert_eq!(q(&db, "SELECT 0.1 + 0.2 = 0.3"), "Yes");
    assert_eq!(q(&db, "SELECT 1.1 * 1.1"), "1.21");
    assert_eq!(q(&db, "SELECT 7 / 2"), "3.5");
    assert_eq!(q(&db, "SELECT 6 / 3"), "2");
    assert_eq!(q(&db, "SELECT 1 / 4"), "0.25");
    assert_eq!(q(&db, "SELECT 1 / 3"), format!("0.{}", "3".repeat(50)));
    assert_eq!(q(&db, "SELECT 7 \\ 2"), "3");
    assert_eq!(q(&db, "SELECT 7 MOD 4"), "3");
    assert_eq!(q(&db, "SELECT -7.5 + 0.5"), "-7");
    assert_eq!(q(&db, "SELECT 2 ^ -2"), "0.25");
    assert_eq!(q(&db, "SELECT 1.5 ^ 2"), "2.25");
    assert!(db.query("SELECT 1 / 0").is_err());
    // A whole-valued result is an ordinary whole number again.
    assert_eq!(q(&db, "SELECT 0.5 * 4 = 2"), "Yes");
    assert_eq!(q(&db, "SELECT 1.0 = 1"), "Yes");
}

#[test]
fn number_functions() {
    let db = Database::in_memory();
    assert_eq!(q(&db, "SELECT Round(2.675, 2)"), "2.68");
    assert_eq!(q(&db, "SELECT Round(2.5)"), "2");
    assert_eq!(q(&db, "SELECT Round(3.5)"), "4");
    assert_eq!(q(&db, "SELECT Round(1234.5678, 2)"), "1234.57");
    assert_eq!(q(&db, "SELECT Int(-2.5)"), "-3");
    assert_eq!(q(&db, "SELECT Fix(-2.5)"), "-2");
    assert_eq!(q(&db, "SELECT Abs(-0.25)"), "0.25");
    assert_eq!(q(&db, "SELECT Abs(-99999999999999999999)"), "99999999999999999999");
    assert_eq!(q(&db, "SELECT Sgn(-3.2)"), "-1");
    assert_eq!(q(&db, "SELECT Sqr(16)"), "4");
    assert_eq!(q(&db, "SELECT CInt(2.5)"), "2");
    assert_eq!(q(&db, "SELECT CCur(1.23456)"), "$1.2346");
    assert_eq!(q(&db, "SELECT Val('3.14abc')"), "3.14");
    assert_eq!(q(&db, "SELECT Val('x')"), "0");
    assert_eq!(q(&db, "SELECT CStr(12.50)"), "12.5");
    assert_eq!(q(&db, "SELECT CDbl('1e-3')"), "0.001");
    assert!(db.query("SELECT Sqr(-1)").is_err());
}

#[test]
fn aggregates_are_exact() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE A (x NUMBER); INSERT INTO A VALUES (0.1), (0.1), (0.1), (0.1), (0.1), (0.1), (0.1), (0.1), (0.1), (0.1)")
        .unwrap();
    assert_eq!(q(&db, "SELECT Sum(x) FROM A"), "1");
    assert_eq!(q(&db, "SELECT Avg(x) FROM A"), "0.1");
    db.execute("CREATE TABLE B (x NUMBER); INSERT INTO B VALUES (1), (2), (2)").unwrap();
    assert_eq!(q(&db, "SELECT Sum(x) FROM B"), "5");
    assert_eq!(q(&db, "SELECT Avg(x) FROM B"), format!("1.{}7", "6".repeat(49)));
    assert_eq!(q(&db, "SELECT Min(x) FROM B"), "1");
    db.execute("CREATE TABLE C (x NUMBER); INSERT INTO C VALUES (9223372036854775807), (9223372036854775807)").unwrap();
    assert_eq!(q(&db, "SELECT Sum(x) FROM C"), "18446744073709551614");
    assert_eq!(q(&db, "SELECT Avg(x) FROM C"), "9223372036854775807");
}

#[test]
fn indexes_and_keys_treat_equal_numbers_alike() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE K (id NUMBER PRIMARY KEY, label TEXT)").unwrap();
    db.execute("INSERT INTO K VALUES (1, 'a'), (2.5, 'b'), (99999999999999999999, 'c')").unwrap();
    // 1.0 is the same number as 1, so it is a duplicate key.
    assert!(db.execute("INSERT INTO K VALUES (1.0, 'dup')").is_err());
    assert!(db.execute("INSERT INTO K VALUES (2.50, 'dup')").is_err());
    assert!(db.execute("INSERT INTO K VALUES (99999999999999999999.0, 'dup')").is_err());
    // Lookups, ranges and joins find them whichever way they are written.
    assert_eq!(q(&db, "SELECT label FROM K WHERE id = 2.50"), "b");
    assert_eq!(q(&db, "SELECT label FROM K WHERE id = 99999999999999999999"), "c");
    assert_eq!(q(&db, "SELECT Count(*) FROM K WHERE id BETWEEN 1 AND 3"), "2");
    assert_eq!(q(&db, "SELECT Count(*) FROM K WHERE id > 100"), "1");
    db.execute("CREATE TABLE J (k NUMBER, v TEXT); INSERT INTO J VALUES (1.0, 'x'), (2.5, 'y')").unwrap();
    assert_eq!(q(&db, "SELECT Count(*) FROM K INNER JOIN J ON K.id = J.k"), "2");
    db.execute("INSERT INTO J VALUES (1, 'z'), (2.50, 'w')").unwrap();
    assert_eq!(q(&db, "SELECT Count(DISTINCT k) FROM J"), "2");
    assert_eq!(q(&db, "SELECT Count(*) FROM J GROUP BY k HAVING k = 1"), "2");
}

#[test]
fn autonumber_stays_whole() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE S (id COUNTER PRIMARY KEY, v TEXT)").unwrap();
    db.execute("INSERT INTO S (v) VALUES ('a'), ('b')").unwrap();
    assert_eq!(q(&db, "SELECT Max(id) FROM S"), "2");
    assert!(db.execute("INSERT INTO S (id, v) VALUES (2.5, 'x')").is_err());
    assert!(db.execute("UPDATE S SET id = 1.5 WHERE id = 1").is_err());
    db.execute("INSERT INTO S (id, v) VALUES (10, 'c')").unwrap();
    db.execute("INSERT INTO S (v) VALUES ('d')").unwrap();
    assert_eq!(q(&db, "SELECT id FROM S WHERE v = 'd'"), "11");
}

#[test]
fn text_and_numbers_still_mix() {
    let db = Database::in_memory();
    assert_eq!(q(&db, "SELECT '10' > 9"), "Yes");
    assert_eq!(q(&db, "SELECT '2.50' = 2.5"), "Yes");
    assert_eq!(q(&db, "SELECT '3' + 4"), "7");
    assert_eq!(q(&db, "SELECT 'abc' & 1.50"), "abc1.5");
    assert_eq!(q(&db, "SELECT Len(1234.5)"), "6");
}

// ----- turning an ordinary field into a calculated one -------------------------------

fn people() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE P (ID COUNTER PRIMARY KEY, First TEXT, Last TEXT, FullName TEXT, Born NUMBER, Age NUMBER);
         CREATE INDEX by_full ON P (FullName);
         INSERT INTO P (First, Last, FullName, Born, Age) VALUES ('Ada', 'Lovelace', 'typed by hand', 1815, 36),
                                                              ('Alan', 'Turing', NULL, 1912, 41)",
    )
    .unwrap();
    db
}

#[test]
fn an_ordinary_field_can_become_calculated() {
    let mut db = people();
    db.execute("ALTER TABLE P ALTER COLUMN FullName CALCULATED AS (First & ' ' & Last)").unwrap();
    // The old values are replaced by the formula's results, for every record.
    assert_eq!(names(&db.query("SELECT FullName FROM P ORDER BY ID").unwrap()), ["Ada Lovelace", "Alan Turing"]);
    let col = db.table("P").unwrap().schema().columns[3].clone();
    assert!(col.is_calculated());
    assert_eq!(col.expression(), Some("First & ' ' & Last"));
    // From now on it follows its record and cannot be written directly.
    db.execute("UPDATE P SET Last = 'King' WHERE First = 'Ada'").unwrap();
    assert_eq!(one(&db, "SELECT FullName FROM P WHERE ID = 1"), "Ada King");
    assert!(db.execute("UPDATE P SET FullName = 'x'").is_err());
    // An index on the field is kept in step.
    assert_eq!(one(&db, "SELECT Count(*) FROM P WHERE FullName = 'ada king'"), "1");
    // Its formula can then be changed like any other, and it can go back to an ordinary field.
    db.execute("ALTER TABLE P ALTER COLUMN FullName CALCULATED AS (Last)").unwrap();
    assert_eq!(one(&db, "SELECT FullName FROM P WHERE ID = 2"), "Turing");
    db.execute("ALTER TABLE P ALTER COLUMN FullName TEXT").unwrap();
    db.execute("UPDATE P SET FullName = 'free again' WHERE ID = 2").unwrap();
    assert_eq!(one(&db, "SELECT FullName FROM P WHERE ID = 2"), "free again");
}

#[test]
fn conversion_works_on_populated_numbers_and_chains() {
    let mut db = people();
    db.execute("ALTER TABLE P ALTER COLUMN Age CALCULATED AS (1851 - Born)").unwrap();
    assert_eq!(one(&db, "SELECT Age FROM P WHERE ID = 1"), "36");
    // Another calculated field may use the converted one, if it comes after it.
    db.execute("ALTER TABLE P ADD COLUMN Decades CALCULATED AS (Age / 10)").unwrap();
    assert_eq!(one(&db, "SELECT Decades FROM P WHERE ID = 1"), "3.6");
    // ...but not if it comes before: that would need Age before Age exists.
    db.execute("CREATE TABLE Q (a NUMBER, early CALCULATED AS (a + 1), b NUMBER)").unwrap();
    assert!(db.execute("ALTER TABLE Q ALTER COLUMN a CALCULATED AS (b * 2)").is_ok());
    assert!(db.execute("ALTER TABLE Q ALTER COLUMN b CALCULATED AS (a + 1)").is_err()); // early <- a <- b <- a
}

#[test]
fn conversion_is_refused_where_it_would_break_something() {
    let mut db = music();
    let err = |db: &mut Database, sql: &str| db.execute(sql).unwrap_err().to_string();
    assert!(err(&mut db, "ALTER TABLE Album ALTER COLUMN ID CALCULATED AS (1)").contains("key"));
    // A field in a relationship, on either side.
    assert!(err(&mut db, "ALTER TABLE Album ALTER COLUMN Artist_ID CALCULATED AS (1)").contains("relationship"));
    assert!(err(&mut db, "ALTER TABLE Artist ALTER COLUMN ID CALCULATED AS (1)").contains("key"));
    // Bad formulas leave the field alone.
    assert!(db.execute("ALTER TABLE Album ALTER COLUMN Year CALCULATED AS (Nope + 1)").is_err());
    assert!(db.execute("ALTER TABLE Album ALTER COLUMN Year CALCULATED AS (Year + 1)").is_err());
    assert!(!db.table("Album").unwrap().schema().columns[3].is_calculated());
    assert_eq!(one(&db, "SELECT Year FROM Album WHERE ID = 1"), "1969");
}

#[test]
fn a_failed_conversion_changes_nothing() {
    let mut db = people();
    db.execute("INSERT INTO P (First, Last, Born, Age) VALUES ('Zero', 'Born', 0, 7)").unwrap();
    // Division by zero for one record: no record may be touched, and Age keeps its values and type.
    assert!(db.execute("ALTER TABLE P ALTER COLUMN Age CALCULATED AS (1000 / Born)").is_err());
    assert!(!db.table("P").unwrap().schema().columns[5].is_calculated());
    assert_eq!(names(&db.query("SELECT Age FROM P ORDER BY ID").unwrap()), ["36", "41", "7"]);
    db.execute("UPDATE P SET Age = 50 WHERE ID = 3").unwrap(); // still an ordinary field
    // A unique index that the computed values would violate also rolls everything back.
    db.execute("CREATE UNIQUE INDEX uniq_last_initial ON P (Age)").unwrap();
    db.execute("UPDATE P SET Age = 41 WHERE ID = 3").unwrap_err(); // duplicate under the unique index
    assert!(db.execute("ALTER TABLE P ALTER COLUMN Age CALCULATED AS (1)").is_err());
    assert_eq!(names(&db.query("SELECT Age FROM P ORDER BY ID").unwrap()), ["36", "41", "50"]);
    assert!(!db.table("P").unwrap().schema().columns[5].is_calculated());
}

#[test]
fn a_required_field_loses_its_requirement_when_it_becomes_calculated() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE R (id COUNTER PRIMARY KEY, a NUMBER, b TEXT NOT NULL); INSERT INTO R (a, b) VALUES (1, 'x')")
        .unwrap();
    db.execute("ALTER TABLE R ALTER COLUMN b CALCULATED AS ('n' & a)").unwrap();
    assert_eq!(one(&db, "SELECT b FROM R"), "n1");
    db.execute("INSERT INTO R (a) VALUES (NULL)").unwrap(); // b is simply computed; no longer required input
}

// ----- the numeric tower ---------------------------------------------------------------

/// "TYPE value" of a one-cell query, e.g. "DECIMAL 3.5".
fn typed(db: &Database, sql: &str) -> String {
    let v = &db.query(sql).unwrap().rows[0][0];
    format!("{} {}", v.type_label(), v)
}

fn zoo() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Z (id COUNTER PRIMARY KEY,
            i INTEGER, b BIGNUM, f FLOAT, d DOUBLE, c DECIMAL, m CURRENCY, r RATIONAL);
         INSERT INTO Z (i, b, f, d, c, m, r) VALUES (6, 6, 6, 6, 6, 6, 6)",
    )
    .unwrap();
    db
}

#[test]
fn columns_store_values_in_their_own_type() {
    let mut db = zoo();
    db.execute("INSERT INTO Z (i, b, f, d, c, m, r) VALUES (2.5, 2.5, 0.1, 0.1, 0.1, 0.1, 0.1)").unwrap();
    db.execute("INSERT INTO Z (i, b, f, d, c, m, r) VALUES ('7', '8.5', '0.1', '0.1', '0.1', '0.1', '1/3')").unwrap();
    let row = |id: i64| -> Vec<String> {
        db.query(&format!("SELECT i, b, f, d, c, m, r FROM Z WHERE id = {id}"))
            .unwrap()
            .rows[0]
            .iter()
            .map(|v| format!("{}:{v}", v.type_label()))
            .collect()
    };
    assert_eq!(
        row(1),
        ["INTEGER:6", "BIGNUM:6", "FLOAT:6", "DOUBLE:6", "DECIMAL:6", "CURRENCY:$6.00", "RATIONAL:6"]
    );
    // Whole-number columns round (ties to even); the others keep what they can.
    assert_eq!(
        row(2),
        ["INTEGER:2", "BIGNUM:2", "FLOAT:0.1", "DOUBLE:0.1", "DECIMAL:0.1", "CURRENCY:$0.10", "RATIONAL:1/10"]
    );
    assert_eq!(
        row(3),
        ["INTEGER:7", "BIGNUM:8", "FLOAT:0.1", "DOUBLE:0.1", "DECIMAL:0.1", "CURRENCY:$0.10", "RATIONAL:1/3"]
    );
    // A value that does not fit the column's type is refused, and nothing is stored.
    assert!(db.execute("INSERT INTO Z (i) VALUES (9223372036854775808)").is_err());
    assert!(db.execute("INSERT INTO Z (f) VALUES (1e300)").is_err());
    assert!(db.execute("INSERT INTO Z (i) VALUES ('abc')").is_err());
    assert_eq!(q(&db, "SELECT Count(*) FROM Z"), "3");
    // BIGNUM, DECIMAL and RATIONAL hold what INTEGER and the floats cannot.
    db.execute("INSERT INTO Z (b, c, r) VALUES (123456789012345678901234567890, 0.000000000000000000000000001, 22/7)")
        .unwrap();
    assert_eq!(q(&db, "SELECT b FROM Z WHERE id = 4"), "123456789012345678901234567890");
    assert_eq!(q(&db, "SELECT c FROM Z WHERE id = 4"), "0.000000000000000000000000001");
}

#[test]
fn literals_have_types() {
    let db = Database::in_memory();
    assert_eq!(typed(&db, "SELECT 42"), "INTEGER 42");
    assert_eq!(typed(&db, "SELECT -9223372036854775808"), "INTEGER -9223372036854775808");
    assert_eq!(typed(&db, "SELECT 9223372036854775808"), "BIGNUM 9223372036854775808");
    assert_eq!(typed(&db, "SELECT 2.5"), "DECIMAL 2.5");
    assert_eq!(typed(&db, "SELECT 5.0"), "DECIMAL 5");
    assert_eq!(typed(&db, "SELECT 1e3"), "DOUBLE 1000");
    assert_eq!(typed(&db, "SELECT -2.5"), "DECIMAL -2.5");
    assert_eq!(typed(&db, "SELECT -1e-3"), "DOUBLE -0.001");
    assert_eq!(typed(&db, "SELECT 'x'"), "TEXT x");
}

#[test]
fn every_pair_of_types_adds_up() {
    let db = zoo();
    let cols = ["i", "b", "f", "d", "c", "m", "r"];
    // What a column of each type gives when combined with one of every other type.
    let expect = |a: &str, b: &str| -> &'static str {
        let has = |t: &str| a == t || b == t;
        if has("d") {
            "DOUBLE"
        } else if has("f") {
            "FLOAT"
        } else if has("r") {
            "RATIONAL"
        } else if has("m") {
            "CURRENCY"
        } else if has("c") {
            "DECIMAL"
        } else if has("b") {
            "BIGNUM"
        } else {
            "INTEGER"
        }
    };
    for a in cols {
        for b in cols {
            for (op, want) in [("+", 12), ("-", 0), ("*", 36)] {
                let r = &db.query(&format!("SELECT {a} {op} {b} FROM Z")).unwrap().rows[0][0];
                assert_eq!(r.type_label(), expect(a, b), "{a} {op} {b}");
                assert_eq!(*r, accessor_core::Value::Int(want), "{a} {op} {b}");
            }
            // Comparisons see through the types.
            assert_eq!(q(&db, &format!("SELECT {a} = {b} FROM Z")), "Yes", "{a} = {b}");
            assert_eq!(q(&db, &format!("SELECT {a} < {b} FROM Z")), "No", "{a} < {b}");
        }
    }
}

#[test]
fn division_power_and_functions_by_type() {
    let db = zoo();
    assert_eq!(typed(&db, "SELECT i / 4 FROM Z"), "DECIMAL 1.5");
    assert_eq!(typed(&db, "SELECT b / 4 FROM Z"), "DECIMAL 1.5");
    assert_eq!(typed(&db, "SELECT m / 4 FROM Z"), "CURRENCY $1.50");
    assert_eq!(typed(&db, "SELECT m / m FROM Z"), "DECIMAL 1");
    assert_eq!(typed(&db, "SELECT r / 4 FROM Z"), "RATIONAL 3/2");
    assert_eq!(typed(&db, "SELECT f / 4 FROM Z"), "FLOAT 1.5");
    assert_eq!(typed(&db, "SELECT d / 4 FROM Z"), "DOUBLE 1.5");
    assert_eq!(typed(&db, "SELECT i \\ 4 FROM Z"), "INTEGER 1");
    assert_eq!(typed(&db, "SELECT b MOD 4 FROM Z"), "BIGNUM 2");
    assert_eq!(typed(&db, "SELECT i ^ 2 FROM Z"), "INTEGER 36");
    assert_eq!(typed(&db, "SELECT i ^ 40 FROM Z"), "BIGNUM 13367494538843734067838845976576");
    assert_eq!(typed(&db, "SELECT r ^ -2 FROM Z"), "RATIONAL 1/36");
    assert_eq!(typed(&db, "SELECT c ^ 0.5 FROM Z").split(' ').next(), Some("DOUBLE"));
    assert_eq!(typed(&db, "SELECT -m FROM Z"), "CURRENCY -$6.00");
    assert_eq!(typed(&db, "SELECT Abs(-f) FROM Z"), "FLOAT 6");
    assert_eq!(typed(&db, "SELECT Round(m / 7, 2) FROM Z"), "CURRENCY $0.86");
    assert_eq!(typed(&db, "SELECT Round(r / 7, 2) FROM Z"), "RATIONAL 43/50");
    assert_eq!(typed(&db, "SELECT Int(-2.5)"), "DECIMAL -3");
    assert_eq!(typed(&db, "SELECT Fix(CRat(-5) / 2)"), "BIGNUM -2");
    assert_eq!(typed(&db, "SELECT Sqr(f) FROM Z").split(' ').next(), Some("FLOAT"));
    assert_eq!(typed(&db, "SELECT Sgn(r) FROM Z"), "INTEGER 1");
    // Errors rather than infinities, NaNs or silent wrap-around.
    for bad in ["SELECT d * 1e300 * 1e300 FROM Z", "SELECT f * CSng(1e30) * CSng(1e30) FROM Z", "SELECT 1 / (i - 6) FROM Z", "SELECT r / (r - 6) FROM Z", "SELECT d / (d - 6) FROM Z", "SELECT 0 ^ -1"] {
        assert!(db.query(bad).is_err(), "{bad}");
    }
}

#[test]
fn rationals_and_conversions() {
    let db = Database::in_memory();
    assert_eq!(typed(&db, "SELECT CRat(1) / 3"), "RATIONAL 1/3");
    assert_eq!(typed(&db, "SELECT CRat(1) / 3 + CRat(1) / 6"), "RATIONAL 1/2");
    assert_eq!(typed(&db, "SELECT Rational(6, 8)"), "RATIONAL 3/4");
    assert_eq!(typed(&db, "SELECT Rational(1, -2)"), "RATIONAL -1/2");
    assert_eq!(typed(&db, "SELECT CRat('7/2')"), "RATIONAL 7/2");
    assert_eq!(typed(&db, "SELECT CRat(0.75)"), "RATIONAL 3/4");
    assert_eq!(typed(&db, "SELECT CRat(CDbl(0.1))"), "RATIONAL 1/10");
    assert_eq!(typed(&db, "SELECT Numerator(CRat(6) / 4)"), "BIGNUM 3");
    assert_eq!(typed(&db, "SELECT Denominator(CRat(6) / 4)"), "BIGNUM 2");
    assert_eq!(typed(&db, "SELECT CDec(CRat(1) / 4)"), "DECIMAL 0.25");
    assert_eq!(typed(&db, "SELECT CDbl(CRat(1) / 4)"), "DOUBLE 0.25");
    assert!(db.query("SELECT Rational(1, 0)").is_err());
    assert!(db.query("SELECT CRat('1/0')").is_err());
    // The other conversions.
    assert_eq!(typed(&db, "SELECT CInt(2.5)"), "INTEGER 2");
    assert_eq!(typed(&db, "SELECT CInt(3.5)"), "INTEGER 4");
    assert_eq!(typed(&db, "SELECT CBig(7)"), "BIGNUM 7");
    assert_eq!(typed(&db, "SELECT CSng(0.1)"), "FLOAT 0.1");
    assert_eq!(typed(&db, "SELECT CDbl(1)"), "DOUBLE 1");
    assert_eq!(typed(&db, "SELECT CDec(0.5e0)"), "DECIMAL 0.5");
    assert_eq!(typed(&db, "SELECT CCur(1.23456)"), "CURRENCY $1.2346");
    assert!(db.query("SELECT CInt(1e30)").is_err());
    assert_eq!(typed(&db, "SELECT TypeName(CSng(1) + CDbl(1))"), "TEXT DOUBLE");
}

/// Equality is by exact value, whatever the types: a binary 0.1 is not the decimal 0.1.
#[test]
fn comparison_is_exact_across_types() {
    let db = Database::in_memory();
    assert_eq!(q(&db, "SELECT 1 = 1.0"), "Yes");
    assert_eq!(q(&db, "SELECT CRat(2) / 4 = 0.5"), "Yes");
    assert_eq!(q(&db, "SELECT CDbl(0.5) = 0.5"), "Yes");
    assert_eq!(q(&db, "SELECT CDbl(0.1) = 0.1"), "No");
    assert_eq!(q(&db, "SELECT CDec(CDbl(0.1)) = 0.1"), "Yes");
    assert_eq!(q(&db, "SELECT CSng(0.1) = CDbl(0.1)"), "No");
    assert_eq!(q(&db, "SELECT CRat(1) / 3 = 1 / 3"), "No"); // the decimal is rounded at 50 digits
    assert_eq!(q(&db, "SELECT CRat(1) / 3 < 1 / 3"), "No");
    assert_eq!(q(&db, "SELECT CRat(1) / 3 > 0.33333333333333333333333333333333333333333333333333"), "Yes");
    assert_eq!(q(&db, "SELECT 9223372036854775807 < 9223372036854775808"), "Yes");
    assert_eq!(q(&db, "SELECT 9223372036854775807 < CDbl(9223372036854775807)"), "Yes"); // the double rounds up to 2^63
}

#[test]
fn indexes_sorts_and_joins_mix_types() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE A (k INTEGER PRIMARY KEY, label TEXT);
         CREATE TABLE B (k DECIMAL, tag TEXT);
         CREATE TABLE C (k DOUBLE, tag TEXT);
         CREATE TABLE D (k RATIONAL, tag TEXT);
         INSERT INTO A VALUES (1, 'one'), (2, 'two'), (3, 'three');
         INSERT INTO B VALUES (1.0, 'b1'), (2.50, 'b2'), (3, 'b3');
         INSERT INTO C VALUES (1, 'c1'), (2.5, 'c2'), (3.0, 'c3');
         INSERT INTO D VALUES (2/2, 'd1'), (7/2, 'd2'), (6/2, 'd3');
         CREATE INDEX ib ON B (k); CREATE INDEX ic ON C (k)",
    )
    .unwrap();
    // Joins on equal values across types: an index lookup and a hash join agree.
    for other in ["B", "C", "D"] {
        let r = db.query(&format!("SELECT label, tag FROM A INNER JOIN {other} ON A.k = {other}.k ORDER BY A.k")).unwrap();
        let pairs: Vec<String> = r.rows.iter().map(|x| format!("{}-{}", x[0], x[1])).collect();
        let c = other.to_lowercase();
        assert_eq!(pairs, [format!("one-{c}1"), format!("three-{c}3")], "{other}");
        let r = db.query(&format!("SELECT Count(*) FROM A INNER JOIN {other} ON A.k + 0 = {other}.k + 0")).unwrap();
        assert_eq!(r.rows[0][0].to_string(), "2", "hash join on {other}");
    }
    // Index probes and ranges on a column of another type than the literal.
    assert_eq!(q(&db, "SELECT tag FROM B WHERE k = 2.5"), "b2");
    assert_eq!(q(&db, "SELECT tag FROM B WHERE k = 3"), "b3");
    assert_eq!(q(&db, "SELECT tag FROM C WHERE k = 2.5"), "c2");
    assert_eq!(q(&db, "SELECT Count(*) FROM B WHERE k > 1"), "2");
    assert_eq!(q(&db, "SELECT Count(*) FROM C WHERE k BETWEEN 1 AND 2.5"), "2");
    assert_eq!(q(&db, "SELECT Count(*) FROM A WHERE k < 2.5"), "2");
    assert_eq!(q(&db, "SELECT Count(*) FROM D WHERE k > 3"), "1");
    // Sorting treats all types together by value.
    let r = db.query("SELECT tag FROM D ORDER BY k DESC").unwrap();
    assert_eq!(r.rows.iter().map(|x| x[0].to_string()).collect::<Vec<_>>(), ["d2", "d3", "d1"]);
    // DISTINCT and GROUP BY see equal numbers of different types as one.
    db.execute("CREATE TABLE M (x INTEGER, y DECIMAL, z DOUBLE); INSERT INTO M VALUES (1, 1.0, 1e0), (2, 2.5, 2.5), (1, 1, 1)")
        .unwrap();
    assert_eq!(q(&db, "SELECT Count(DISTINCT x) FROM M"), "2");
    assert_eq!(q(&db, "SELECT Count(*) FROM M GROUP BY y HAVING y = 1"), "2");
    assert_eq!(q(&db, "SELECT Count(*) FROM M WHERE x = y AND y = z"), "2");
}

#[test]
fn aggregates_follow_the_tower() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE G (i INTEGER, b BIGNUM, f FLOAT, d DOUBLE, c DECIMAL, m CURRENCY, r RATIONAL);
         INSERT INTO G VALUES (1, 1, 1, 1, 1, 1, 1), (2, 2, 2, 2, 2, 2, 2), (2, 2, 2, 2, 2, 2, 2)",
    )
    .unwrap();
    // Sum keeps the column's type; Avg divides exactly.
    let third = format!("DECIMAL 1.{}7", "6".repeat(49));
    let money_third = format!("CURRENCY $1.{}7", "6".repeat(49));
    for (col, sum, avg) in [
        ("i", "INTEGER 5", third.as_str()),
        ("b", "BIGNUM 5", third.as_str()),
        ("f", "FLOAT 5", "FLOAT 1.6666666"),
        ("d", "DOUBLE 5", "DOUBLE 1.6666666666666667"),
        ("c", "DECIMAL 5", third.as_str()),
        ("m", "CURRENCY $5.00", money_third.as_str()),
        ("r", "RATIONAL 5", "RATIONAL 5/3"),
    ] {
        assert_eq!(typed(&db, &format!("SELECT Sum({col}) FROM G")), sum, "Sum({col})");
        assert_eq!(typed(&db, &format!("SELECT Avg({col}) FROM G")), avg, "Avg({col})");
        assert_eq!(typed(&db, &format!("SELECT Max({col}) FROM G")).split(' ').next(), typed(&db, &format!("SELECT {col} FROM G")).split(' ').next());
    }
    // Sums of INTEGERs do not overflow: they become BIGNUMs.
    db.execute("CREATE TABLE H (i INTEGER); INSERT INTO H VALUES (9223372036854775807), (9223372036854775807)").unwrap();
    assert_eq!(typed(&db, "SELECT Sum(i) FROM H"), "BIGNUM 18446744073709551614");
    // A mixed column of values (via a calculated field) sums up too.
    db.execute("CREATE TABLE K (a INTEGER, b DECIMAL); INSERT INTO K VALUES (1, 0.5), (2, 0.25)").unwrap();
    assert_eq!(typed(&db, "SELECT Sum(a + b) FROM K"), "DECIMAL 3.75");
}

#[test]
fn foreign_keys_work_between_exact_number_types() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE P (id DECIMAL PRIMARY KEY);
         CREATE TABLE C1 (id INTEGER PRIMARY KEY, p INTEGER REFERENCES P (id));
         INSERT INTO P VALUES (1), (2.5)",
    )
    .unwrap();
    db.execute("INSERT INTO C1 VALUES (1, 1)").unwrap();
    assert!(db.execute("INSERT INTO C1 VALUES (2, 3)").is_err());
    assert!(db.execute("DELETE FROM P WHERE id = 1").is_err());
    // Floats do not take part with exact types.
    assert!(db.execute("CREATE TABLE C2 (id INTEGER PRIMARY KEY, p DOUBLE REFERENCES P (id))").is_err());
    // AutoNumbers stay 64-bit integers.
    assert!(db.execute("CREATE TABLE C3 (id COUNTER PRIMARY KEY, v BIGNUM)").is_ok());
    assert!(db.execute("INSERT INTO C3 (id) VALUES (2.5)").is_ok(), "an INTEGER column rounds what it is given");
    assert_eq!(q(&db, "SELECT id FROM C3"), "2");
}

#[test]
fn calculated_fields_have_typed_results() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE T (a INTEGER, b CURRENCY, c RATIONAL, total CALCULATED AS (a * b), share CALCULATED AS (c / a));
         INSERT INTO T (a, b, c) VALUES (3, 19.99, 1)",
    )
    .unwrap();
    assert_eq!(typed(&db, "SELECT total FROM T"), "CURRENCY $59.97");
    assert_eq!(typed(&db, "SELECT share FROM T"), "RATIONAL 1/3");
    db.execute("UPDATE T SET a = 6").unwrap();
    assert_eq!(typed(&db, "SELECT total FROM T"), "CURRENCY $119.94");
    assert_eq!(typed(&db, "SELECT share FROM T"), "RATIONAL 1/6");
}

// ----- transactions -----------------------------------------------------------------------

/// Everything observable about the database: every table's schema and rows, saved
/// queries and triggers. Two equal fingerprints mean nothing has changed.
fn fingerprint(db: &Database) -> String {
    let mut out = String::new();
    for name in db.table_names() {
        let t = db.table(name).unwrap();
        out.push_str(&format!("{:?}\n", t.schema()));
        let mut rows: Vec<String> = t.iter().map(|(id, r)| format!("{id}:{r:?}")).collect();
        rows.sort();
        out.push_str(&rows.join("\n"));
        out.push('\n');
    }
    for v in db.view_names() {
        out.push_str(&format!("view {v} = {}\n", db.view_sql(v).unwrap()));
    }
    for t in db.trigger_names() {
        out.push_str(&format!("trigger {t} = {}\n", db.trigger_sql(t).unwrap()));
    }
    out
}

fn shop() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Customer (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL);
         CREATE TABLE Orders (ID COUNTER PRIMARY KEY, CustID INTEGER REFERENCES Customer (ID) ON DELETE CASCADE,
                              Qty INTEGER, Price CURRENCY, Total CALCULATED AS (Qty * Price));
         CREATE TABLE Log (Msg TEXT);
         CREATE VIEW Big AS SELECT * FROM Orders WHERE Qty > 5;
         CREATE TRIGGER order_added AFTER INSERT ON Orders BEGIN INSERT INTO Log VALUES ('order ' & NEW.ID); END;
         INSERT INTO Customer (Name) VALUES ('Ada'), ('Grace');
         INSERT INTO Orders (CustID, Qty, Price) VALUES (1, 3, 2.5), (1, 9, 1), (2, 7, 4)",
    )
    .unwrap();
    db
}

#[test]
fn rollback_undoes_row_changes_of_every_kind() {
    let mut db = shop();
    let before = fingerprint(&db);
    db.begin().unwrap();
    assert!(db.in_transaction());
    db.execute("INSERT INTO Customer (Name) VALUES ('Linus')").unwrap(); // and the trigger's own insert
    db.execute("INSERT INTO Orders (CustID, Qty, Price) VALUES (3, 1, 1)").unwrap();
    db.execute("UPDATE Orders SET Qty = Qty + 1").unwrap(); // recomputes the calculated field
    db.execute("DELETE FROM Customer WHERE ID = 1").unwrap(); // cascades into Orders
    assert_eq!(count(&db, "SELECT Count(*) FROM Orders"), "2");
    assert!(db.pending_changes() > 0);
    db.rollback().unwrap();
    assert!(!db.in_transaction());
    assert_eq!(db.pending_changes(), 0);
    assert_eq!(fingerprint(&db), before);
}

#[test]
fn rollback_undoes_schema_changes_of_every_kind() {
    let mut db = shop();
    let before = fingerprint(&db);
    db.begin().unwrap();
    db.execute("CREATE TABLE Extra (a INTEGER PRIMARY KEY, b TEXT)").unwrap();
    db.execute("INSERT INTO Extra VALUES (1, 'x')").unwrap();
    db.execute("CREATE INDEX by_qty ON Orders (Qty)").unwrap();
    db.execute("ALTER TABLE Orders ADD COLUMN Note TEXT").unwrap();
    db.execute("ALTER TABLE Orders ADD COLUMN Tax CALCULATED AS (Total / 10)").unwrap();
    db.execute("ALTER TABLE Orders ALTER COLUMN Note CALCULATED AS ('n' & Qty)").unwrap();
    db.execute("ALTER TABLE Orders ALTER COLUMN Qty DECIMAL").unwrap_err(); // refused: Total uses it
    db.execute("ALTER TABLE Orders RENAME COLUMN Price TO Cost").unwrap_err(); // refused: Total uses it
    db.execute("ALTER TABLE Orders DROP COLUMN Tax").unwrap();
    db.execute("ALTER TABLE Orders DROP CONSTRAINT fk_Orders_CustID").unwrap();
    db.execute("ALTER TABLE Orders ADD FOREIGN KEY (Qty) REFERENCES Extra (a)").unwrap_err(); // 3, 9, 7 have no match
    db.execute("CREATE VIEW Small AS SELECT * FROM Orders WHERE Qty < 5").unwrap();
    db.execute("DROP VIEW Big").unwrap();
    db.execute("CREATE TRIGGER extra_added AFTER INSERT ON Extra BEGIN INSERT INTO Log VALUES ('extra'); END").unwrap();
    db.execute("DROP TRIGGER order_added").unwrap();
    db.execute("DROP TABLE Log").unwrap();
    db.execute("DROP TABLE Orders").unwrap();
    db.execute("CREATE TABLE Orders (just_this INTEGER)").unwrap();
    assert_ne!(fingerprint(&db), before);
    db.rollback().unwrap();
    assert_eq!(fingerprint(&db), before);
    // The restored database works: triggers fire, calculated fields follow, relationships hold.
    db.execute("INSERT INTO Orders (CustID, Qty, Price) VALUES (2, 2, 3)").unwrap();
    assert_eq!(count(&db, "SELECT Msg FROM Log ORDER BY Msg DESC LIMIT 1"), "order 4");
    assert_eq!(count(&db, "SELECT Total FROM Orders WHERE ID = 4"), "$6.00");
    assert!(db.execute("INSERT INTO Orders (CustID) VALUES (99)").is_err());
    assert_eq!(count(&db, "SELECT Count(*) FROM Big"), "2");
}

#[test]
fn rows_and_schema_interleave_correctly_on_rollback() {
    let mut db = shop();
    let before = fingerprint(&db);
    db.begin().unwrap();
    db.execute("INSERT INTO Orders (CustID, Qty, Price) VALUES (1, 4, 1)").unwrap();
    db.execute("ALTER TABLE Orders ADD COLUMN Flag YESNO").unwrap();
    db.execute("INSERT INTO Orders (CustID, Qty, Price, Flag) VALUES (1, 5, 1, Yes)").unwrap();
    db.execute("ALTER TABLE Orders DROP COLUMN Flag").unwrap();
    db.execute("DELETE FROM Orders WHERE Qty = 4").unwrap();
    db.rollback().unwrap();
    assert_eq!(fingerprint(&db), before);

    // And committed, the same sequence leaves the combined result.
    db.begin().unwrap();
    db.execute("INSERT INTO Orders (CustID, Qty, Price) VALUES (1, 4, 1)").unwrap();
    db.execute("ALTER TABLE Orders ADD COLUMN Flag YESNO DEFAULT Yes").unwrap();
    db.execute("INSERT INTO Orders (CustID, Qty, Price, Flag) VALUES (1, 5, 1, No)").unwrap();
    db.commit().unwrap();
    assert_eq!(count(&db, "SELECT Count(*) FROM Orders WHERE Flag"), "4"); // the 3 old rows and the one before the ALTER
    assert_eq!(count(&db, "SELECT Count(*) FROM Orders"), "5");
}

#[test]
fn a_failed_statement_leaves_the_transaction_open() {
    let mut db = shop();
    db.begin().unwrap();
    db.execute("INSERT INTO Customer (Name) VALUES ('Linus')").unwrap();
    assert!(db.execute("INSERT INTO Customer (Name) VALUES (NULL)").is_err()); // NOT NULL
    assert!(db.execute("INSERT INTO Customer (Name) VALUES ('a'), (NULL)").is_err()); // second row fails the first too
    assert!(db.execute("INSERT INTO Orders (CustID) VALUES (99)").is_err()); // no such customer
    assert!(db.execute("ALTER TABLE Customer ADD COLUMN x INTEGER NOT NULL").is_err());
    assert!(db.execute("CREATE TABLE Customer (a INTEGER)").is_err()); // exists
    assert!(db.in_transaction());
    assert_eq!(count(&db, "SELECT Count(*) FROM Customer"), "3"); // only the first insert is there
    db.commit().unwrap();
    assert_eq!(count(&db, "SELECT Count(*) FROM Customer"), "3");
    assert!(db.commit().is_err());
    assert!(db.rollback().is_err());
}

#[test]
fn transactions_do_not_nest_and_block_file_maintenance() {
    let mut db = shop();
    db.execute("BEGIN").unwrap();
    assert!(db.execute("BEGIN").is_err());
    assert!(db.checkpoint().is_err());
    assert!(db.compact().is_err());
    db.execute("COMMIT TRANSACTION").unwrap();
    db.execute("BEGIN TRANSACTION").unwrap();
    db.execute("ROLLBACK WORK").unwrap();
    assert!(db.execute("SAVEPOINT s").is_err(), "savepoints need a transaction");
    assert!(db.execute("ROLLBACK TO s").is_err());
    assert!(db.execute("RELEASE s").is_err());
}

#[test]
fn savepoints_return_to_a_point_inside_a_transaction() {
    let mut db = shop();
    let at_start = fingerprint(&db);
    db.execute("BEGIN").unwrap();
    db.execute("INSERT INTO Customer (Name) VALUES ('one')").unwrap();
    db.execute("SAVEPOINT a").unwrap();
    let at_a = fingerprint(&db);
    db.execute("INSERT INTO Customer (Name) VALUES ('two')").unwrap();
    db.execute("ALTER TABLE Customer ADD COLUMN Note TEXT").unwrap();
    db.execute("SAVEPOINT b").unwrap();
    db.execute("DELETE FROM Customer WHERE Name = 'one'").unwrap();
    db.execute("DROP VIEW Big").unwrap();

    db.execute("ROLLBACK TO b").unwrap();
    assert_eq!(count(&db, "SELECT Count(*) FROM Customer"), "4");
    assert_eq!(db.view_names().len(), 1);
    db.execute("ROLLBACK TO SAVEPOINT a").unwrap(); // b is gone now
    assert_eq!(fingerprint(&db), at_a);
    assert!(db.execute("ROLLBACK TO b").is_err());
    // The savepoint survives being returned to.
    db.execute("INSERT INTO Customer (Name) VALUES ('three')").unwrap();
    db.execute("ROLLBACK TO a").unwrap();
    assert_eq!(fingerprint(&db), at_a);

    // Releasing keeps the changes but forgets the point.
    db.execute("INSERT INTO Customer (Name) VALUES ('four')").unwrap();
    db.execute("RELEASE SAVEPOINT a").unwrap();
    assert!(db.execute("ROLLBACK TO a").is_err());
    assert_eq!(count(&db, "SELECT Count(*) FROM Customer"), "4");

    // A reused name refers to the newest one; the whole transaction can still be abandoned.
    db.execute("SAVEPOINT x").unwrap();
    db.execute("INSERT INTO Customer (Name) VALUES ('five')").unwrap();
    db.execute("SAVEPOINT x").unwrap();
    db.execute("INSERT INTO Customer (Name) VALUES ('six')").unwrap();
    db.execute("ROLLBACK TO x").unwrap();
    assert_eq!(count(&db, "SELECT Count(*) FROM Customer"), "5"); // 'five' stays
    db.execute("ROLLBACK").unwrap();
    assert_eq!(fingerprint(&db), at_start);
}

#[test]
fn the_transaction_helper_commits_or_rolls_back() {
    let mut db = shop();
    let before = fingerprint(&db);
    // Ok: committed, and the value comes back.
    let n = db
        .transaction(|db| {
            db.execute("INSERT INTO Customer (Name) VALUES ('Linus')")?;
            Ok(db.table("Customer").unwrap().len())
        })
        .unwrap();
    assert_eq!(n, 3);
    assert!(!db.in_transaction());
    assert_eq!(count(&db, "SELECT Count(*) FROM Customer"), "3");
    // Err: everything it did is undone, and the error is returned.
    let after_commit = fingerprint(&db);
    let err = db
        .transaction(|db| {
            db.execute("INSERT INTO Customer (Name) VALUES ('Tim')")?;
            db.execute("ALTER TABLE Customer ADD COLUMN x INTEGER")?;
            db.execute("INSERT INTO Customer (Name) VALUES (NULL)")?;
            Ok(())
        })
        .unwrap_err();
    assert!(err.to_string().contains("Null"), "{err}");
    assert!(!db.in_transaction());
    assert_eq!(fingerprint(&db), after_commit);
    assert_ne!(before, after_commit);
    // It cannot be started inside another transaction.
    db.begin().unwrap();
    assert!(db.transaction(|_| Ok(())).is_err());
    assert!(db.in_transaction(), "the outer transaction is untouched");
    db.rollback().unwrap();
}

// ----- dates -----------------------------------------------------------------------------

#[test]
fn datediff_counts_calendar_boundaries() {
    let db = Database::in_memory();
    let diff = |unit: &str, a: &str, b: &str| q(&db, &format!("SELECT DateDiff('{unit}', #{a}#, #{b}#)"));
    // Years and months count boundaries crossed, not elapsed time.
    assert_eq!(diff("yyyy", "2020-12-31", "2021-01-01"), "1");
    assert_eq!(diff("yyyy", "2020-01-01", "2020-12-31"), "0");
    assert_eq!(diff("yyyy", "2019-06-15", "2024-01-02"), "5");
    assert_eq!(diff("yyyy", "2021-01-01", "2020-12-31"), "-1");
    assert_eq!(diff("m", "2024-01-31", "2024-03-01"), "2");
    assert_eq!(diff("q", "2024-03-31", "2024-04-01"), "1");
    assert_eq!(diff("q", "2024-01-01", "2024-03-31"), "0");
    assert_eq!(diff("d", "2024-02-28", "2024-03-01"), "2");
    // Hours, minutes and seconds are whole elapsed units.
    assert_eq!(diff("h", "2024-01-01 10:30", "2024-01-01 12:29"), "1");
    assert_eq!(diff("n", "2024-01-01 10:00:30", "2024-01-01 10:02:29"), "1");
    assert_eq!(diff("s", "2024-01-01 10:00:00", "2024-01-01 10:00:07"), "7");
}

// ----- correlated sub-queries, UNION, FULL JOIN, Format ------------------------------------

#[test]
fn correlated_subqueries() {
    let db = db();
    let r = db
        .query("SELECT Name FROM Customers c WHERE EXISTS (SELECT 1 FROM Orders o WHERE o.CustID = c.ID) ORDER BY Name")
        .unwrap();
    assert_eq!(col(&r, 0), vec!["Ada", "Grace", "Linus"]);
    let r = db.query("SELECT Name FROM Customers c WHERE NOT EXISTS (SELECT 1 FROM Orders WHERE CustID = c.ID)").unwrap();
    assert_eq!(col(&r, 0), vec!["Zed"]);
    // A correlated scalar sub-query in the select list; no match gives NULL.
    let r = db
        .query("SELECT Name, (SELECT Sum(Total) FROM Orders WHERE CustID = Customers.ID) AS S FROM Customers ORDER BY ID")
        .unwrap();
    assert_eq!(col(&r, 1), vec!["$30.50", "$12.25", "$100.00", ""]);
    // Correlated IN, a correlated comparison with an aggregate, and a mix with an ordinary predicate.
    let r = db
        .query("SELECT OrderID FROM Orders o WHERE o.Total = (SELECT Max(Total) FROM Orders WHERE CustID = o.CustID) AND o.OrderID > 1 ORDER BY 1")
        .unwrap();
    assert_eq!(col(&r, 0), vec!["2", "4", "5"]);
    let r = db
        .query("SELECT Name FROM Customers c WHERE c.ID IN (SELECT CustID FROM Orders o WHERE o.Total > 6 AND o.CustID = c.ID) ORDER BY 1")
        .unwrap();
    assert_eq!(col(&r, 0), vec!["Ada", "Grace", "Linus"]);
    // Two levels deep: the innermost query reads the outermost row.
    let r = db
        .query("SELECT Name FROM Customers c WHERE EXISTS (SELECT 1 FROM Orders o WHERE o.CustID = c.ID AND EXISTS (SELECT 1 FROM Orders p WHERE p.CustID = c.ID AND p.Total > o.Total)) ORDER BY 1")
        .unwrap();
    assert_eq!(col(&r, 0), vec!["Ada", "Grace"]);
    // Not allowed with GROUP BY, with a clear message.
    let e = db
        .query("SELECT City, Count(*) FROM Customers c GROUP BY City HAVING EXISTS (SELECT 1 FROM Orders o WHERE o.CustID = c.ID)")
        .unwrap_err();
    assert!(e.to_string().contains("outer query"), "{e}");
}

#[test]
fn union_and_union_all() {
    let db = db();
    let r = db.query("SELECT City FROM Customers WHERE City IS NOT NULL UNION SELECT 'London' UNION SELECT 'Paris' ORDER BY 1").unwrap();
    assert_eq!(col(&r, 0), vec!["Helsinki", "London", "NYC", "Paris"]);
    let r = db.query("SELECT 1 AS n UNION ALL SELECT 1 UNION ALL SELECT 2").unwrap();
    assert_eq!(col(&r, 0), vec!["1", "1", "2"]);
    assert_eq!(r.columns, vec!["n"]);
    let r = db.query("SELECT CustID FROM Orders UNION SELECT ID FROM Customers ORDER BY CustID DESC LIMIT 2 OFFSET 1").unwrap();
    assert_eq!(col(&r, 0), vec!["3", "2"]);
    let e = db.query("SELECT 1, 2 UNION SELECT 3").unwrap_err();
    assert!(e.to_string().contains("same number of columns"), "{e}");
    // Inside a derived table and an IN list.
    let r = db.query("SELECT Count(*) FROM (SELECT ID FROM Customers UNION ALL SELECT CustID FROM Orders) AS u").unwrap();
    assert_eq!(col(&r, 0), vec!["9"]);
    let r = db.query("SELECT Name FROM Customers WHERE ID IN (SELECT 1 UNION SELECT 4) ORDER BY 1").unwrap();
    assert_eq!(col(&r, 0), vec!["Ada", "Zed"]);
}

#[test]
fn full_outer_join() {
    let mut db = db();
    db.execute("INSERT INTO Orders (CustID, Total) VALUES (99, 1)").unwrap();
    let r = db
        .query("SELECT c.Name, o.OrderID FROM Customers c FULL OUTER JOIN Orders o ON o.CustID = c.ID ORDER BY o.OrderID, c.Name")
        .unwrap();
    // Zed has no orders; order 6 has no customer; both survive.
    assert_eq!(r.rows.len(), 7);
    assert!(r.rows.iter().any(|row| row[0].to_string() == "Zed" && row[1].is_null()));
    assert!(r.rows.iter().any(|row| row[0].is_null() && row[1].to_string() == "6"));
    let r = db.query("SELECT Count(*) FROM Customers c FULL JOIN Orders o ON o.CustID = c.ID").unwrap();
    assert_eq!(col(&r, 0), vec!["7"]);
}

#[test]
fn format_function() {
    let db = Database::in_memory();
    let f = |v: &str, p: &str| q(&db, &format!("SELECT Format({v}, '{p}')"));
    assert_eq!(f("1234567.891", "#,##0.00"), "1,234,567.89");
    assert_eq!(f("0.125", "0.00"), "0.12"); // ties to even, exactly
    assert_eq!(f("5", "000"), "005");
    assert_eq!(f("0.256", "Percent"), "25.60%");
    assert_eq!(f("-3.5", "Currency"), "($3.50)");
    assert_eq!(f("1234.5", "Fixed"), "1234.50");
    assert_eq!(f("1234.5", "Standard"), "1,234.50");
    assert_eq!(f("1234.5", "0.00E+00"), "1.23E+03");
    assert_eq!(f("0.5", "#.##"), ".5");
    assert_eq!(f("-7", "0;(0)"), "(7)");
    assert_eq!(f("0", "0;(0);\"zero\""), "zero");
    assert_eq!(f("1", "Yes/No"), "Yes");
    assert_eq!(f("12345678901234567890123", "#,##0"), "12,345,678,901,234,567,890,123");
    assert_eq!(f("#2024-03-05 14:07:09#", "yyyy-mm-dd hh:nn:ss"), "2024-03-05 14:07:09");
    assert_eq!(f("#2024-03-05 14:07:09#", "dddd, mmmm d, yyyy"), "Tuesday, March 5, 2024");
    assert_eq!(f("#2024-03-05 14:07:09#", "h:mm AM/PM"), "2:07 PM");
    assert_eq!(f("#2024-03-05 14:07:09#", "Short Date"), "3/5/2024");
    assert_eq!(f("'abc'", ">"), "ABC");
    assert!(db.query("SELECT Format(NULL, '0')").unwrap().rows[0][0].is_null());
}

// ----- standard SQL additions -------------------------------------------------------------

fn emp() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Emp (ID INTEGER PRIMARY KEY, Name TEXT, Dept TEXT, Sal INTEGER);
         INSERT INTO Emp VALUES (1,'Ann','Eng',100),(2,'Bob','Eng',120),(3,'Cy','Eng',120),(4,'Di','Ops',90),(5,'Ed','Ops',70),(6,'Flo','HR',80);",
    )
    .unwrap();
    db
}

fn rows(db: &Database, sql: &str) -> Vec<Vec<String>> {
    db.query(sql).unwrap().rows.iter().map(|r| r.iter().map(|v| v.to_string()).collect()).collect()
}

#[test]
fn window_functions() {
    let db = emp();
    let r = rows(&db, "SELECT Name, RANK() OVER (PARTITION BY Dept ORDER BY Sal DESC), ROW_NUMBER() OVER (PARTITION BY Dept ORDER BY Sal DESC, Name) FROM Emp WHERE Dept = 'Eng' ORDER BY 3");
    assert_eq!(r, vec![vec!["Bob", "1", "1"], vec!["Cy", "1", "2"], vec!["Ann", "3", "3"]]);
    let r = rows(&db, "SELECT SUM(Sal) OVER (ORDER BY ID), SUM(Sal) OVER (PARTITION BY Dept) FROM Emp ORDER BY ID");
    assert_eq!(r[2], vec!["340", "340"]);
    assert_eq!(r[5], vec!["580", "80"]);
    let r = rows(&db, "SELECT LAG(Sal) OVER (ORDER BY ID), LEAD(Sal, 2, 0) OVER (ORDER BY ID) FROM Emp ORDER BY ID");
    assert_eq!(r[0], vec!["", "120"]);
    assert_eq!(r[5], vec!["70", "0"]);
    let r = rows(&db, "SELECT Sal, SUM(Sal) OVER (ORDER BY ID ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM Emp ORDER BY ID");
    assert_eq!(r[1][1], "220");
    assert_eq!(r[2][1], "240");
    // Over groups: rank the departments by their totals.
    let r = rows(&db, "SELECT Dept, RANK() OVER (ORDER BY SUM(Sal) DESC) FROM Emp GROUP BY Dept ORDER BY 2");
    assert_eq!(r, vec![vec!["Eng", "1"], vec!["Ops", "2"], vec!["HR", "3"]]);
    // Not allowed in WHERE.
    assert!(db.query("SELECT 1 FROM Emp WHERE ROW_NUMBER() OVER () = 1").is_err());
}

#[test]
fn grouping_sets_and_aggregates() {
    let db = emp();
    let r = rows(&db, "SELECT Dept, SUM(Sal), GROUPING(Dept) FROM Emp GROUP BY ROLLUP (Dept) ORDER BY 3, 1");
    assert_eq!(r.last().unwrap(), &vec!["", "580", "1"]);
    assert_eq!(r.len(), 4);
    let r = rows(&db, "SELECT Dept, Count(*) FROM Emp GROUP BY GROUPING SETS ((Dept), ()) ORDER BY 2 DESC LIMIT 1");
    assert_eq!(r, vec![vec!["", "6"]]);
    let r = rows(&db, "SELECT Dept, STRING_AGG(Name, '/'), Count(*) FILTER (WHERE Sal > 90) FROM Emp GROUP BY Dept ORDER BY Dept");
    assert_eq!(r[0], vec!["Eng", "Ann/Bob/Cy", "3"]);
    assert_eq!(r[2], vec!["Ops", "Di/Ed", "0"]);
    let r = rows(&db, "SELECT STDDEV_POP(Sal), VAR_SAMP(Sal), EVERY(Sal > 60) FROM Emp WHERE Dept = 'Ops'");
    assert_eq!(r, vec![vec!["10", "200", "Yes"]]);
}

#[test]
fn common_table_expressions() {
    let db = emp();
    let r = rows(&db, "WITH hi AS (SELECT * FROM Emp WHERE Sal >= 100), d AS (SELECT Dept, Count(*) c FROM hi GROUP BY Dept) SELECT * FROM d");
    assert_eq!(r, vec![vec!["Eng", "3"]]);
    let r = rows(&db, "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 10) SELECT SUM(i), Count(*) FROM n");
    assert_eq!(r, vec![vec!["55", "10"]]);
    let r = rows(&db, "WITH RECURSIVE fact(n, f) AS (VALUES (1, 1) UNION ALL SELECT n + 1, f * (n + 1) FROM fact WHERE n < 25) SELECT f FROM fact WHERE n = 25");
    assert_eq!(r, vec![vec!["15511210043330985984000000"]]);
}

#[test]
fn set_operations_and_values() {
    let db = emp();
    assert_eq!(rows(&db, "SELECT Dept FROM Emp INTERSECT SELECT 'Eng' UNION SELECT 'Zed' ORDER BY 1"), vec![vec!["Eng"], vec!["Zed"]]);
    assert_eq!(rows(&db, "SELECT Dept FROM Emp EXCEPT SELECT 'Eng' ORDER BY 1"), vec![vec!["HR"], vec!["Ops"]]);
    assert_eq!(rows(&db, "SELECT Dept FROM Emp EXCEPT ALL SELECT 'Eng' ORDER BY 1").len(), 5);
    assert_eq!(rows(&db, "SELECT Dept FROM Emp INTERSECT ALL SELECT Dept FROM Emp WHERE ID < 3").len(), 2);
    assert_eq!(rows(&db, "SELECT * FROM (VALUES (1,'a'),(2,'b')) AS v(n, s) ORDER BY n DESC"), vec![vec!["2", "b"], vec!["1", "a"]]);
    assert_eq!(rows(&db, "SELECT Name FROM Emp ORDER BY ID OFFSET 1 ROWS FETCH NEXT 2 ROWS ONLY"), vec![vec!["Bob"], vec!["Cy"]]);
}

#[test]
fn using_and_natural_joins() {
    let db = emp();
    let r = rows(&db, "SELECT e.Name, d.* FROM Emp e JOIN (VALUES ('Eng','X'),('Ops','Y')) AS d(Dept, Code) USING (Dept) ORDER BY e.ID");
    assert_eq!(r.len(), 5);
    assert_eq!(r[0], vec!["Ann", "Eng", "X"]);
    let r = db.query("SELECT * FROM Emp NATURAL JOIN (SELECT Dept, 1 AS flag FROM Emp WHERE ID = 1) x").unwrap();
    assert_eq!(r.columns, vec!["ID", "Name", "Dept", "Sal", "flag"]);
    assert_eq!(r.rows.len(), 3);
    // The merged column is usable unqualified.
    assert_eq!(rows(&db, "SELECT Dept FROM Emp JOIN (SELECT Dept FROM Emp WHERE ID = 4) o USING (Dept)").len(), 2);
}

#[test]
fn standard_expressions_and_functions() {
    let db = Database::in_memory();
    let one = |sql: &str| q(&db, sql);
    assert_eq!(one("SELECT CAST('12' AS INTEGER) + 1"), "13");
    assert_eq!(one("SELECT CAST(7 AS VARCHAR(5)) || 'x'"), "7x");
    assert_eq!(one("SELECT EXTRACT(MONTH FROM TIMESTAMP '2024-03-05 10:20:30')"), "3");
    assert_eq!(one("SELECT EXTRACT(QUARTER FROM DATE '2024-11-05')"), "4");
    assert_eq!(one("SELECT POSITION('lo' IN 'hello')"), "4");
    assert_eq!(one("SELECT SUBSTRING('hello' FROM 2 FOR 3)"), "ell");
    assert_eq!(one("SELECT TRIM(LEADING 'x' FROM 'xxhixx')"), "hixx");
    assert_eq!(one("SELECT CHAR_LENGTH('héllo')"), "5");
    assert_eq!(one("SELECT COALESCE(NULLIF(3, 3), 9)"), "9");
    assert_eq!(one("SELECT GREATEST(2, 9, 4) - LEAST(2, 9, 4)"), "7");
    assert_eq!(one("SELECT CEILING(2.1) + FLOOR(-2.1)"), "0");
    assert_eq!(one("SELECT POWER(2, 10) + MOD(7, 3)"), "1025");
    assert_eq!(one("SELECT CASE 2 WHEN 1 THEN 'a' WHEN 2 THEN 'b' END"), "b");
    assert_eq!(one("SELECT NULL IS NOT DISTINCT FROM NULL"), "Yes");
    assert_eq!(one("SELECT 1 IS DISTINCT FROM NULL"), "Yes");
    assert_eq!(one("SELECT (NULL = 1) IS NOT TRUE"), "Yes");
    assert_eq!(one("SELECT 5 BETWEEN SYMMETRIC 9 AND 1"), "Yes");
    assert_eq!(one("SELECT 'a_c' LIKE 'a!_c' ESCAPE '!'"), "Yes");
    assert_eq!(one("SELECT 'abc' LIKE 'a!_c' ESCAPE '!'"), "No");
    assert_eq!(one("SELECT 3 > ALL (SELECT 1 UNION SELECT 2)"), "Yes");
    assert_eq!(one("SELECT 3 > ALL (SELECT 1 UNION SELECT 5)"), "No");
    assert_eq!(one("SELECT 3 = ANY (SELECT 1 UNION SELECT 3)"), "Yes");
    assert_eq!(one("SELECT 1 < ALL (SELECT 5 WHERE 1 = 0)"), "Yes");
}

#[test]
fn ddl_dml_additions() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE IF NOT EXISTS P (ID INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, Name VARCHAR(20) NOT NULL,
            Age INT CHECK (Age >= 0), Dbl INT GENERATED ALWAYS AS (Age * 2) STORED, CONSTRAINT young CHECK (Age < 150 OR Age IS NULL));
         CREATE TABLE IF NOT EXISTS P (X INT);
         INSERT INTO P (Name, Age) VALUES ('a', 5), ('b', 10);",
    )
    .unwrap();
    assert_eq!(rows(&db, "SELECT ID, Dbl FROM P ORDER BY ID"), vec![vec!["1", "10"], vec!["2", "20"]]);
    let e = db.execute("INSERT INTO P (Name, Age) VALUES ('c', -1)").unwrap_err();
    assert!(e.to_string().contains("CHECK constraint failed"), "{e}");
    assert!(db.execute("UPDATE P SET Age = 500 WHERE Name = 'a'").is_err());
    assert!(db.execute("ALTER TABLE P ADD CONSTRAINT lim CHECK (Age < 8)").is_err()); // 10 violates it
    db.execute("ALTER TABLE P DROP CONSTRAINT young").unwrap();
    db.execute("UPDATE P SET Age = 500 WHERE Name = 'a'").unwrap();
    assert!(db.execute("INSERT INTO P DEFAULT VALUES").is_err()); // Name is required

    // Correlated sub-queries in UPDATE and DELETE.
    db.execute("DELETE FROM P x WHERE EXISTS (SELECT 1 FROM P y WHERE y.Age > x.Age)").unwrap();
    assert_eq!(rows(&db, "SELECT Name FROM P"), vec![vec!["a"]]);
    db.execute("INSERT INTO P (Name, Age) VALUES ('d', 3)").unwrap();
    db.execute("UPDATE P SET Age = (SELECT Count(*) FROM P q WHERE q.Age < P.Age) WHERE Name = 'a'").unwrap();
    assert_eq!(rows(&db, "SELECT Age FROM P WHERE Name = 'a'"), vec![vec!["1"]]);

    db.execute("CREATE TABLE T (K INT PRIMARY KEY, V TEXT); INSERT INTO T VALUES (1,'one'),(2,'two'),(3,'three');").unwrap();
    let n = db
        .execute(
            "MERGE INTO T USING (VALUES (2,'TWO'),(3,'x'),(4,'four')) AS s(k, v) ON T.K = s.k
             WHEN MATCHED AND s.v = 'x' THEN DELETE
             WHEN MATCHED THEN UPDATE SET V = s.v
             WHEN NOT MATCHED THEN INSERT (K, V) VALUES (s.k, s.v)",
        )
        .unwrap();
    assert_eq!(n.affected, 3);
    assert_eq!(rows(&db, "SELECT K, V FROM T ORDER BY K"), vec![vec!["1", "one"], vec!["2", "TWO"], vec!["4", "four"]]);
    // A failing MERGE changes nothing.
    assert!(db
        .execute("MERGE INTO T USING (VALUES (9, 'n'), (8, 'm')) AS s(k, v) ON T.K = s.k WHEN NOT MATCHED THEN INSERT (K, V) VALUES (s.k, s.k / 0)")
        .is_err());
    assert_eq!(rows(&db, "SELECT Count(*) FROM T"), vec![vec!["3"]]);

    db.execute("TRUNCATE TABLE T").unwrap();
    assert_eq!(rows(&db, "SELECT Count(*) FROM T"), vec![vec!["0"]]);
    db.execute("DROP TABLE IF EXISTS Nope; DROP TABLE IF EXISTS T").unwrap();
    assert!(db.table("T").is_none());
}

// ----- the query optimizer -----------------------------------------------------------------

/// A small deterministic generator, so the data is the same on every run.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

fn opt_db() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE A (id INTEGER PRIMARY KEY, k INTEGER, v TEXT);
         CREATE TABLE B (id INTEGER PRIMARY KEY, a_id INTEGER, w INTEGER);
         CREATE TABLE C (id INTEGER PRIMARY KEY, b_id INTEGER, z INTEGER);
         CREATE TABLE D (id INTEGER, tag TEXT);
         CREATE INDEX ib ON B (a_id);
         CREATE INDEX ic ON C (b_id);
         CREATE INDEX ak ON A (k);
         CREATE INDEX bw ON B (w, id);",
    )
    .unwrap();
    let mut r = Lcg(12345);
    let mut script = String::new();
    for i in 1..=60 {
        let k = if r.next(10) == 0 { "NULL".to_string() } else { r.next(8).to_string() };
        script += &format!("INSERT INTO A VALUES ({i}, {k}, 'v{}');\n", r.next(5));
    }
    for i in 1..=300 {
        let a = if r.next(12) == 0 { "NULL".to_string() } else { (r.next(70) + 1).to_string() };
        let w = if r.next(15) == 0 { "NULL".to_string() } else { r.next(20).to_string() };
        script += &format!("INSERT INTO B VALUES ({i}, {a}, {w});\n");
    }
    for i in 1..=900 {
        let b = if r.next(12) == 0 { "NULL".to_string() } else { (r.next(320) + 1).to_string() };
        script += &format!("INSERT INTO C VALUES ({i}, {b}, {});\n", r.next(10));
    }
    for i in 1..=6 {
        script += &format!("INSERT INTO D VALUES ({i}, 't{i}');\n");
    }
    db.execute(&script).unwrap();
    db
}

fn answers(db: &mut Database, sql: &str, on: bool) -> Vec<String> {
    db.set_optimizer(on);
    let r = db.query(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    r.rows.iter().map(|row| row.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("|")).collect()
}

#[test]
fn the_optimizer_never_changes_an_answer() {
    let mut db = opt_db();
    // Queries whose rows have no defined order: compare as multisets.
    let unordered = [
        "SELECT A.id, B.id, C.id FROM A JOIN B ON B.a_id = A.id JOIN C ON C.b_id = B.id",
        "SELECT A.id, C.id FROM C JOIN B ON B.id = C.b_id JOIN A ON A.id = B.a_id WHERE A.k = 3 AND C.z > 4",
        "SELECT A.id, B.id FROM A, B WHERE B.a_id = A.id AND A.v = 'v1' AND B.w < 7",
        "SELECT A.id, B.id, D.tag FROM A, B, D WHERE B.a_id = A.id AND D.id = B.w",
        "SELECT * FROM A CROSS JOIN D WHERE A.k = D.id",
        "SELECT A.id, B.id FROM A LEFT JOIN B ON B.a_id = A.id AND B.w > 10",
        "SELECT A.id, B.id FROM A LEFT JOIN B ON B.a_id = A.id WHERE B.id IS NULL",
        "SELECT A.id, B.id FROM A LEFT JOIN B ON B.a_id = A.id WHERE B.w > 10",
        "SELECT A.id, B.id FROM A LEFT JOIN B ON B.a_id = A.id WHERE A.k = 2",
        "SELECT A.id, B.id FROM A RIGHT JOIN B ON B.a_id = A.id WHERE B.w = 3",
        "SELECT A.id, B.id FROM A RIGHT JOIN B ON B.a_id = A.id WHERE A.k = 3",
        "SELECT A.id, B.id FROM A FULL JOIN B ON B.a_id = A.id WHERE B.w < 3 OR A.k = 1",
        "SELECT A.id, B.id, C.id FROM A JOIN B ON B.a_id = A.id LEFT JOIN C ON C.b_id = B.id WHERE A.k > 4",
        "SELECT A.id, B.id, C.id FROM A LEFT JOIN B ON B.a_id = A.id JOIN C ON C.b_id = B.id",
        "SELECT X.id, Y.id FROM A X JOIN A Y ON X.k = Y.k WHERE X.id < Y.id AND X.v = 'v2'",
        "SELECT B.id, C.id FROM B JOIN C ON C.z = B.w WHERE B.id < 20",
        "SELECT A.k, Count(*), Sum(B.w) FROM A JOIN B ON B.a_id = A.id WHERE B.w IS NOT NULL GROUP BY A.k",
        "SELECT A.id FROM A JOIN B ON B.a_id = A.id JOIN C ON C.b_id = B.id WHERE 1 = 0",
        "SELECT A.id, B.id FROM A JOIN B ON B.a_id = A.id AND B.w = A.k",
        "SELECT A.id FROM A WHERE A.id IN (SELECT a_id FROM B WHERE w = 5)",
        "SELECT A.id, (SELECT Count(*) FROM B WHERE B.a_id = A.id) FROM A",
        "SELECT Count(*) FROM B JOIN C ON C.b_id = B.id",
        "SELECT COUNT(*), COUNT(*) FROM C",
    ];
    for sql in unordered {
        let mut on = answers(&mut db, sql, true);
        let mut off = answers(&mut db, sql, false);
        on.sort();
        off.sort();
        assert_eq!(on, off, "different answers for {sql}");
    }
    // Queries with an ORDER BY: the sequence itself must match, ties included.
    let ordered = [
        "SELECT id, k FROM A ORDER BY k LIMIT 7",
        "SELECT id, k FROM A ORDER BY k DESC LIMIT 7",
        "SELECT id, k FROM A ORDER BY k DESC LIMIT 5 OFFSET 3",
        "SELECT id, k FROM A WHERE v = 'v1' ORDER BY k LIMIT 4",
        "SELECT id, w FROM B WHERE w IS NOT NULL ORDER BY w, id LIMIT 12",
        "SELECT id, w FROM B ORDER BY w DESC, id DESC LIMIT 12",
        "SELECT id, w FROM B ORDER BY w DESC LIMIT 12",
        "SELECT id, w FROM B WHERE w = 4 ORDER BY id LIMIT 3",
        "SELECT id, a_id FROM B WHERE a_id = 7 ORDER BY id DESC LIMIT 2",
        "SELECT id, k AS kk FROM A ORDER BY kk LIMIT 5",
        "SELECT id, k FROM A ORDER BY 2 DESC LIMIT 5",
        "SELECT id, k FROM A ORDER BY k, id DESC LIMIT 5",
        "SELECT TOP 6 id, k FROM A ORDER BY k",
        "SELECT id FROM A WHERE k > 2 ORDER BY k LIMIT 3",
        "SELECT A.id, B.id FROM A JOIN B ON B.a_id = A.id ORDER BY B.w, B.id LIMIT 10",
    ];
    for sql in ordered {
        assert_eq!(answers(&mut db, sql, true), answers(&mut db, sql, false), "different order for {sql}");
    }
}

#[test]
fn explain_reports_the_plan() {
    let mut db = opt_db();
    let plan = |db: &Database, sql: &str| -> String {
        db.query(&format!("EXPLAIN {sql}")).unwrap().rows.iter().map(|r| r[0].to_string()).collect::<Vec<_>>().join("\n")
    };
    // The selective table goes first and the others are reached through their indexes.
    let p = plan(&db, "SELECT C.id FROM C JOIN B ON B.id = C.b_id JOIN A ON A.id = B.a_id WHERE A.id = 3");
    assert!(p.contains("join order: A -> B -> C"), "{p}");
    assert!(p.contains("reordered"), "{p}");
    assert!(p.contains("index nested-loop via ib"), "{p}");
    assert!(p.contains("row(s) returned"), "{p}");

    // An ORDER BY answered by an index does not sort.
    let p = plan(&db, "SELECT id FROM A ORDER BY k LIMIT 3");
    assert!(p.contains("order by satisfied by index ak"), "{p}");
    assert!(!p.contains("sort:"), "{p}");
    let p = plan(&db, "SELECT id FROM A ORDER BY v LIMIT 3");
    assert!(p.contains("sort: top-3"), "{p}");

    // COUNT(*) is answered from the row count.
    assert!(plan(&db, "SELECT COUNT(*) FROM C").contains("row count"));

    // Outer joins keep the order written.
    let p = plan(&db, "SELECT A.id FROM A LEFT JOIN B ON B.a_id = A.id");
    assert!(p.contains("join order: A -> B"), "{p}");
    assert!(!p.contains("reordered"), "{p}");

    // The same query with the optimizer off reports no reordering.
    db.set_optimizer(false);
    let p = plan(&db, "SELECT C.id FROM C JOIN B ON B.id = C.b_id JOIN A ON A.id = B.a_id WHERE A.id = 3");
    assert!(p.contains("join order: C -> B -> A") && !p.contains("reordered"), "{p}");
    assert!(!plan(&db, "SELECT COUNT(*) FROM C").contains("row count"));
}

#[test]
fn pushdown_keeps_outer_join_semantics() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE P (id INTEGER PRIMARY KEY, n TEXT);
         CREATE TABLE Q (id INTEGER PRIMARY KEY, p_id INTEGER, flag INTEGER);
         INSERT INTO P VALUES (1, 'a'), (2, 'b'), (3, 'c');
         INSERT INTO Q VALUES (1, 1, 1), (2, 1, 0), (3, 2, 0);",
    )
    .unwrap();
    // A WHERE condition on the nullable side removes the padded rows, as written...
    assert_eq!(rows(&db, "SELECT P.n FROM P LEFT JOIN Q ON Q.p_id = P.id WHERE Q.flag = 1"), vec![vec!["a"]]);
    // ...but the same condition in ON keeps every P row.
    let r = rows(&db, "SELECT P.n, Q.id FROM P LEFT JOIN Q ON Q.p_id = P.id AND Q.flag = 1 ORDER BY P.n");
    assert_eq!(r, vec![vec!["a", "1"], vec!["b", ""], vec!["c", ""]]);
    // IS NULL on the nullable side finds the unmatched rows.
    assert_eq!(rows(&db, "SELECT P.n FROM P LEFT JOIN Q ON Q.p_id = P.id WHERE Q.id IS NULL"), vec![vec!["c"]]);
}

// ----- crosstab queries (TRANSFORM ... PIVOT) ------------------------------------------------

fn sales() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Sales (Region TEXT, Q INTEGER, Amount CURRENCY, Rep TEXT);
         INSERT INTO Sales VALUES ('East',1,10,'a'),('East',2,20,'b'),('East',2,5,'a'),('West',1,7,'a'),
                                  ('West',4,3,'c'),('North',3,9,NULL),('North',NULL,1,'a');",
    )
    .unwrap();
    db
}

#[test]
fn crosstab_basics() {
    let db = sales();
    let r = db
        .query("TRANSFORM Sum(Amount) SELECT Region, Sum(Amount) AS Total FROM Sales GROUP BY Region ORDER BY Region PIVOT Q")
        .unwrap();
    // The NULL pivot value heads a column called <>, then the values in order.
    assert_eq!(r.columns, vec!["Region", "Total", "<>", "1", "2", "3", "4"]);
    let rows: Vec<Vec<String>> = r.rows.iter().map(|row| row.iter().map(|v| v.to_string()).collect()).collect();
    assert_eq!(rows[0], vec!["East", "$35.00", "", "$10.00", "$25.00", "", ""]);
    assert_eq!(rows[1], vec!["North", "$10.00", "$1.00", "", "", "$9.00", ""]);
    assert_eq!(rows[2], vec!["West", "$10.00", "", "$7.00", "", "", "$3.00"]);

    // A fixed column list: other values are left out of the columns, and a count of nothing is 0.
    let r = db.query("TRANSFORM Count(*) SELECT Region FROM Sales GROUP BY Region ORDER BY Region PIVOT Q IN (2, 4, 9)").unwrap();
    assert_eq!(r.columns, vec!["Region", "2", "4", "9"]);
    let rows: Vec<Vec<String>> = r.rows.iter().map(|row| row.iter().map(|v| v.to_string()).collect()).collect();
    assert_eq!(rows, vec![vec!["East", "2", "0", "0"], vec!["North", "0", "0", "0"], vec!["West", "0", "1", "0"]]);

    // A condition, an expression for the pivot, and a saved crosstab.
    let r = db
        .query("TRANSFORM Max(Amount) SELECT Region FROM Sales WHERE Amount > 2 GROUP BY Region ORDER BY 1 PIVOT 'Q' & Q")
        .unwrap();
    assert_eq!(r.columns, vec!["Region", "Q1", "Q2", "Q3", "Q4"]); // the row with no Q is filtered out
    let mut db = db;
    db.execute("CREATE QUERY CT AS TRANSFORM Sum(Amount) SELECT Region FROM Sales GROUP BY Region PIVOT Rep").unwrap();
    assert_eq!(db.query("SELECT * FROM CT").unwrap().columns, vec!["Region", "<>", "a", "b", "c"]);

    // Errors say what is wrong.
    let e = db.query("TRANSFORM Amount SELECT Region FROM Sales GROUP BY Region PIVOT Q").unwrap_err();
    assert!(e.to_string().contains("aggregate function"), "{e}");
    assert!(db.query("TRANSFORM Sum(Amount) SELECT Region FROM Sales GROUP BY Region").is_err());
}

#[test]
fn the_crosstab_shortcut_gives_the_same_answers() {
    let mut db = opt_db();
    let queries = [
        "TRANSFORM Sum(w) SELECT A.k FROM A JOIN B ON B.a_id = A.id GROUP BY A.k ORDER BY A.k PIVOT B.w IN (1, 2, 3, 4)",
        "TRANSFORM Count(*) SELECT A.v, Count(*) AS n FROM A JOIN B ON B.a_id = A.id GROUP BY A.v ORDER BY A.v PIVOT A.k",
        "TRANSFORM Avg(C.z) SELECT B.w FROM B JOIN C ON C.b_id = B.id GROUP BY B.w ORDER BY B.w PIVOT C.z",
        "TRANSFORM Count(DISTINCT B.id) SELECT A.k FROM A JOIN B ON B.a_id = A.id WHERE B.w < 12 GROUP BY A.k HAVING Count(*) > 3 ORDER BY A.k PIVOT A.v",
        "TRANSFORM Max(id) SELECT v FROM A GROUP BY v ORDER BY v PIVOT k",
        "TRANSFORM Min(w) SELECT a_id FROM B WHERE a_id < 12 GROUP BY a_id ORDER BY a_id PIVOT w % 3",
        "TRANSFORM Count(*) SELECT Count(*) AS total FROM A PIVOT v",
        "TRANSFORM Sum(z) SELECT Year(#2024-01-01# + id) AS y FROM C GROUP BY Year(#2024-01-01# + id) ORDER BY 1 PIVOT z",
    ];
    for sql in queries {
        db.set_optimizer(true);
        let on = db.query(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        db.set_optimizer(false);
        let off = db.query(sql).unwrap();
        assert_eq!(on.columns, off.columns, "{sql}");
        assert_eq!(on.rows, off.rows, "different cells for {sql}");
    }
    // The optimized plan says what it did; the plain one runs one query per column.
    db.set_optimizer(true);
    let plan = db
        .query("EXPLAIN TRANSFORM Sum(w) SELECT a_id FROM B GROUP BY a_id PIVOT w")
        .unwrap()
        .rows
        .iter()
        .map(|r| r[0].to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(plan.contains("one grouped pass"), "{plan}");
}

// ----- NULLS FIRST / NULLS LAST --------------------------------------------------------------

#[test]
fn nulls_first_and_last() {
    let db = Database::in_memory();
    let mut db = db;
    db.execute(
        "CREATE TABLE N (id INTEGER PRIMARY KEY, x INTEGER, y TEXT);
         CREATE INDEX nx ON N (x);
         INSERT INTO N VALUES (1, 3, 'a'), (2, NULL, 'b'), (3, 1, 'c'), (4, NULL, 'd'), (5, 3, 'e'), (6, 2, NULL);",
    )
    .unwrap();
    let ids = |db: &Database, sql: &str| -> Vec<String> { rows(db, sql).into_iter().map(|r| r[0].clone()).collect() };
    // Defaults: NULLs first ascending, last descending.
    assert_eq!(ids(&db, "SELECT id FROM N ORDER BY x, id"), ["2", "4", "3", "6", "1", "5"]);
    assert_eq!(ids(&db, "SELECT id FROM N ORDER BY x DESC, id"), ["1", "5", "6", "3", "2", "4"]);
    // Explicit placement.
    assert_eq!(ids(&db, "SELECT id FROM N ORDER BY x NULLS LAST, id"), ["3", "6", "1", "5", "2", "4"]);
    assert_eq!(ids(&db, "SELECT id FROM N ORDER BY x DESC NULLS FIRST, id"), ["2", "4", "1", "5", "6", "3"]);
    assert_eq!(ids(&db, "SELECT id FROM N ORDER BY x ASC NULLS FIRST, id"), ["2", "4", "3", "6", "1", "5"]);
    assert_eq!(ids(&db, "SELECT id FROM N ORDER BY y NULLS FIRST, id"), ["6", "1", "2", "3", "4", "5"]);
    assert_eq!(ids(&db, "SELECT id, x FROM N ORDER BY 2 NULLS LAST, 1 DESC"), ["3", "6", "5", "1", "4", "2"]);
    // In a window's ORDER BY, and after a set operator.
    let r = rows(&db, "SELECT id, ROW_NUMBER() OVER (ORDER BY x NULLS LAST, id) FROM N ORDER BY 2");
    assert_eq!(r[0], ["3", "1"]);
    assert_eq!(r[5], ["4", "6"]);
    assert_eq!(ids(&db, "SELECT x FROM N WHERE id < 4 UNION ALL SELECT 9 ORDER BY 1 DESC NULLS FIRST").len(), 4);

    // The index serves every combination, with and without a limit, and gives the same rows.
    for sql in [
        "SELECT id, x FROM N ORDER BY x NULLS LAST LIMIT 4",
        "SELECT id, x FROM N ORDER BY x NULLS FIRST LIMIT 4",
        "SELECT id, x FROM N ORDER BY x DESC NULLS FIRST LIMIT 4",
        "SELECT id, x FROM N ORDER BY x DESC NULLS LAST LIMIT 4",
        "SELECT id, x FROM N ORDER BY x NULLS LAST LIMIT 3 OFFSET 2",
        "SELECT id, x FROM N WHERE id > 1 ORDER BY x DESC NULLS FIRST LIMIT 5",
        "SELECT id, x FROM N ORDER BY x NULLS LAST LIMIT 100",
    ] {
        db.set_optimizer(true);
        let on = rows(&db, sql);
        db.set_optimizer(false);
        assert_eq!(on, rows(&db, sql), "{sql}");
    }
    db.set_optimizer(true);
    let plan: String = db
        .query("EXPLAIN SELECT id FROM N ORDER BY x NULLS LAST LIMIT 2")
        .unwrap()
        .rows
        .iter()
        .map(|r| r[0].to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(plan.contains("NULLs last") && plan.contains("no sort"), "{plan}");
}

// ----- row values, LATERAL and table functions -------------------------------------------------

fn cust_ord() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Cust (ID INTEGER PRIMARY KEY, Name TEXT);
         CREATE TABLE Ord (ID INTEGER PRIMARY KEY, CustID INTEGER, Total INTEGER, D INTEGER);
         CREATE INDEX oc ON Ord (CustID, Total);
         INSERT INTO Cust VALUES (1,'Ada'),(2,'Bob'),(3,'Cy');
         INSERT INTO Ord VALUES (1,1,50,1),(2,1,70,2),(3,1,20,3),(4,2,90,1),(5,2,10,2);",
    )
    .unwrap();
    db
}

#[test]
fn row_value_comparisons() {
    let db = cust_ord();
    let ids = |sql: &str| -> Vec<String> { rows(&db, sql).into_iter().map(|r| r[0].clone()).collect() };
    assert_eq!(ids("SELECT ID FROM Ord WHERE (CustID, Total) = (1, 70)"), ["2"]);
    assert_eq!(ids("SELECT ID FROM Ord WHERE (CustID, Total) <> (1, 70) ORDER BY ID"), ["1", "3", "4", "5"]);
    // Lexicographic ordering: the first differing part decides.
    assert_eq!(ids("SELECT ID FROM Ord WHERE (CustID, Total) > (1, 50) ORDER BY ID"), ["2", "4", "5"]);
    assert_eq!(ids("SELECT ID FROM Ord WHERE (CustID, Total) >= (1, 50) ORDER BY ID"), ["1", "2", "4", "5"]);
    assert_eq!(ids("SELECT ID FROM Ord WHERE (CustID, Total) < (2, 10) ORDER BY ID"), ["1", "2", "3"]);
    assert_eq!(ids("SELECT ID FROM Ord WHERE (CustID, Total, D) <= (1, 50, 1) ORDER BY ID"), ["1", "3"]);
    assert_eq!(ids("SELECT ID FROM Ord WHERE ROW(CustID, Total) = ROW(2, 90)"), ["4"]);
    // IN and NOT IN with lists of rows and with a query.
    assert_eq!(ids("SELECT ID FROM Ord WHERE (CustID, Total) IN ((1, 20), (2, 10)) ORDER BY ID"), ["3", "5"]);
    assert_eq!(ids("SELECT ID FROM Ord WHERE (CustID, Total) NOT IN ((1, 20), (2, 10)) ORDER BY ID"), ["1", "2", "4"]);
    assert_eq!(ids("SELECT ID FROM Ord WHERE (CustID, D) IN (SELECT CustID, D FROM Ord WHERE Total >= 90) ORDER BY ID"), ["4"]);
    // Keyset paging: the page after (1, 2) in (CustID, ID) order.
    assert_eq!(ids("SELECT ID FROM Ord WHERE (CustID, ID) > (1, 2) ORDER BY CustID, ID LIMIT 2"), ["3", "4"]);
    // NULL parts make the answer unknown, not false.
    assert!(ids("SELECT 1 WHERE (1, NULL) = (1, 2)").is_empty());
    assert!(ids("SELECT 1 WHERE NOT ((1, NULL) = (2, 2))").len() == 1); // the first parts already differ
    // Misuse is reported.
    assert!(db.query("SELECT (1, 2)").is_err());
    assert!(db.query("SELECT 1 WHERE (1, 2) = (1, 2, 3)").is_err());
    assert!(db.query("SELECT 1 WHERE (1, 2) = 1").is_err());
}

#[test]
fn lateral_joins_and_table_functions() {
    let mut db = cust_ord();
    // Top 2 orders per customer.
    let r = rows(
        &db,
        "SELECT C.Name, T.ID, T.Total FROM Cust C, LATERAL (SELECT O.ID, O.Total FROM Ord O WHERE O.CustID = C.ID ORDER BY O.Total DESC LIMIT 2) AS T ORDER BY C.Name, T.Total DESC",
    );
    assert_eq!(r, [["Ada", "2", "70"], ["Ada", "1", "50"], ["Bob", "4", "90"], ["Bob", "5", "10"]]);
    // LEFT JOIN LATERAL keeps customers with no rows.
    let r = rows(&db, "SELECT C.Name, T.ID FROM Cust C LEFT JOIN LATERAL (SELECT O.ID FROM Ord O WHERE O.CustID = C.ID AND O.Total > 60) AS T ON TRUE ORDER BY C.Name, T.ID");
    assert_eq!(r, [["Ada", "2"], ["Bob", "4"], ["Cy", ""]]);
    // An aggregate per row, with the item's column names given.
    let r = rows(&db, "SELECT C.Name, S.n, S.total FROM Cust C CROSS JOIN LATERAL (SELECT Count(*), Sum(Total) FROM Ord WHERE CustID = C.ID) AS S (n, total) ORDER BY C.Name");
    assert_eq!(r, [["Ada", "3", "140"], ["Bob", "2", "100"], ["Cy", "0", ""]]);
    // A later item can use an earlier lateral one.
    let r = rows(&db, "SELECT C.ID, A.n, B.m FROM Cust C, LATERAL (SELECT C.ID * 10 AS n) AS A, LATERAL (SELECT A.n + 1 AS m) AS B ORDER BY 1");
    assert_eq!(r[1], ["2", "20", "21"]);
    // Without any outer reference LATERAL is just a derived table.
    assert_eq!(rows(&db, "SELECT * FROM LATERAL (SELECT 1 AS x) AS q"), [["1"]]);
    // RIGHT and FULL joins cannot be lateral.
    assert!(db.query("SELECT 1 FROM Cust C RIGHT JOIN LATERAL (SELECT 1 AS x WHERE C.ID > 0) AS T ON TRUE").is_err());

    // Table functions.
    assert_eq!(rows(&db, "SELECT * FROM GENERATE_SERIES(1, 10, 3)"), [["1"], ["4"], ["7"], ["10"]]);
    assert_eq!(rows(&db, "SELECT x FROM GENERATE_SERIES(5, 1, -2) AS g (x)"), [["5"], ["3"], ["1"]]);
    assert_eq!(rows(&db, "SELECT Sum(generate_series) FROM GENERATE_SERIES(1, 100)"), [["5050"]]);
    assert_eq!(rows(&db, "SELECT generate_series FROM GENERATE_SERIES(1, 0)").len(), 0);
    assert!(db.query("SELECT * FROM GENERATE_SERIES(1, 5, 0)").is_err());
    assert_eq!(rows(&db, "SELECT * FROM SPLIT_STRING('a,b,c', ',') AS p (piece, n)"), [["a", "1"], ["b", "2"], ["c", "3"]]);
    let r = rows(&db, "SELECT C.Name, S.generate_series FROM Cust C CROSS JOIN LATERAL GENERATE_SERIES(1, C.ID) AS S ORDER BY 1, 2");
    assert_eq!(r.len(), 6);
    assert!(db.query("SELECT * FROM NOSUCH(1)").is_err());

    // The optimizer changes nothing about the answers.
    for sql in [
        "SELECT C.Name, T.ID FROM Cust C, LATERAL (SELECT O.ID FROM Ord O WHERE O.CustID = C.ID) AS T",
        "SELECT C.Name, T.ID FROM Cust C JOIN Ord X ON X.CustID = C.ID, LATERAL (SELECT O.ID FROM Ord O WHERE O.CustID = X.CustID AND O.ID <> X.ID) AS T WHERE X.Total > 15",
        "SELECT C.Name, T.ID FROM Cust C LEFT JOIN LATERAL (SELECT O.ID FROM Ord O WHERE O.CustID = C.ID) AS T ON T.ID > 1 WHERE C.ID < 3",
    ] {
        db.set_optimizer(true);
        let mut on = rows(&db, sql);
        db.set_optimizer(false);
        let mut off = rows(&db, sql);
        on.sort();
        off.sort();
        assert_eq!(on, off, "{sql}");
    }
    // Repeated outer values reuse the answer: Ord has two customers, so two runs for five rows.
    db.set_optimizer(true);
    let plan: String = db
        .query("EXPLAIN SELECT X.ID, T.ID FROM Ord X, LATERAL (SELECT Count(*) AS ID FROM Ord O WHERE O.CustID = X.CustID) AS T")
        .unwrap()
        .rows
        .iter()
        .map(|r| r[0].to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(plan.contains("run 2 time(s) for 5 left row(s)"), "{plan}");
}
