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
    db.execute("BEGIN").unwrap();
    assert!(db.execute("CREATE TABLE z (a INTEGER)").is_err());
    db.execute("ROLLBACK").unwrap();
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
