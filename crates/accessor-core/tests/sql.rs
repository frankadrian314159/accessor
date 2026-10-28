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

    // A column that is neither grouped nor aggregated takes its value from a row of the group (the first),
    // as in SQLite and MySQL.
    let r = db.query("SELECT City, Count(*) FROM Customers").unwrap();
    assert_eq!((r.rows.len(), r.rows[0][1].clone()), (1, Value::Int(4)));
    assert_eq!(r.rows[0][0], Value::text("London"));
    let r = db.query("SELECT CustID, Total, Count(*) FROM Orders GROUP BY CustID ORDER BY CustID").unwrap();
    assert_eq!(col(&r, 2), vec!["2", "2", "1"]);
    assert_eq!(col(&r, 1), vec!["$10.50", "$5.00", "$100.00"], "the first order of each customer");
}

#[test]
fn cast_to_an_integer_drops_the_fraction() {
    let db = Database::in_memory();
    let one = |sql: &str| db.query(sql).unwrap().rows[0][0].to_string();
    for (sql, want) in [
        ("SELECT CAST(2.7 AS INTEGER)", "2"), ("SELECT CAST(-2.7 AS INTEGER)", "-2"), ("SELECT CAST(2.5 AS INTEGER)", "2"),
        ("SELECT CAST(3.5 AS INTEGER)", "3"), ("SELECT CAST(2.9e0 AS INTEGER)", "2"), ("SELECT CAST(-0.9e0 AS INTEGER)", "0"),
        ("SELECT CAST(CSng(2.9) AS INTEGER)", "2"), ("SELECT CAST(CCur(2.99) AS INTEGER)", "2"), ("SELECT CAST(CRat(7) DIV 2 AS INTEGER)", "3"),
        ("SELECT CAST(7 / 2.0 AS INTEGER)", "3"), ("SELECT CAST(3.99e0 AS BIGNUM)", "3"), ("SELECT CAST('12' AS INTEGER)", "12"),
        ("SELECT CAST(5 AS INTEGER)", "5"),
        // the Access function still rounds, halves to the even digit
        ("SELECT CInt(2.7)", "3"), ("SELECT CInt(2.5)", "2"), ("SELECT CInt(3.5)", "4"),
    ] {
        assert_eq!(one(sql), want, "{sql}");
    }
    assert_eq!(db.query("SELECT CAST(NULL AS INTEGER)").unwrap().rows[0][0], Value::Null);
    assert!(db.query("SELECT CAST(1e30 AS INTEGER)").is_err());
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
    assert!(db.query("SELECT 1 / 0").unwrap().rows[0][0].is_null());
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
    bad(&mut db, "CREATE TRIGGER t AFTER INSERT ON Album BEGIN DROP TABLE Log; END");
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
    // Dividing by zero gives NULL, so the write goes through and the calculated value is NULL.
    db.execute("ALTER TABLE Line ADD COLUMN Each CALCULATED AS (Total DIV Qty)").unwrap();
    db.execute("UPDATE Line SET Qty = 0 WHERE Item = 'pen'").unwrap();
    assert!(db.query("SELECT Each FROM Line WHERE Item = 'pen'").unwrap().rows[0][0].is_null());
    db.execute("UPDATE Line SET Qty = 3 WHERE Item = 'pen'").unwrap();
    assert_eq!(one(&db, "SELECT Each FROM Line WHERE Item = 'pen'"), "$2.00");
    // A value that cannot be computed is an error and leaves nothing behind...
    assert!(db.execute("UPDATE Line SET Price = 1e300 * 1e300 WHERE Item = 'pen'").is_err());
    assert_eq!(one(&db, "SELECT Qty FROM Line WHERE Item = 'pen'"), "3");
    // ...and a bad expression change is rolled back as a whole.
    assert!(db.execute("ALTER TABLE Line ALTER COLUMN Each CALCULATED AS (Total * CDbl(1e300) * CDbl(1e300))").is_err());
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
    assert_eq!(q(&db, "SELECT 7 DIV 2"), "3.5");
    assert_eq!(q(&db, "SELECT 6 DIV 3"), "2");
    assert_eq!(q(&db, "SELECT 1 DIV 4"), "0.25");
    assert_eq!(q(&db, "SELECT 1 DIV 3"), format!("0.{}", "3".repeat(50)));
    assert_eq!(q(&db, "SELECT 7 \\ 2"), "3");
    assert_eq!(q(&db, "SELECT 7 MOD 4"), "3");
    assert_eq!(q(&db, "SELECT -7.5 + 0.5"), "-7");
    assert_eq!(q(&db, "SELECT 2 ^ -2"), "0.25");
    assert_eq!(q(&db, "SELECT 1.5 ^ 2"), "2.25");
    assert!(db.query("SELECT 1 / 0").unwrap().rows[0][0].is_null());
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
    // A value that cannot be computed for some record: no record may be touched, and Age keeps its values and type.
    assert!(db.execute("ALTER TABLE P ALTER COLUMN Age CALCULATED AS (Born * CDbl(1e300) * CDbl(1e300))").is_err());
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
    assert_eq!(typed(&db, "SELECT i DIV 4 FROM Z"), "DECIMAL 1.5");
    assert_eq!(typed(&db, "SELECT b DIV 4 FROM Z"), "DECIMAL 1.5");
    assert_eq!(typed(&db, "SELECT m DIV 4 FROM Z"), "CURRENCY $1.50");
    assert_eq!(typed(&db, "SELECT m DIV m FROM Z"), "DECIMAL 1");
    assert_eq!(typed(&db, "SELECT r DIV 4 FROM Z"), "RATIONAL 3/2");
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
    assert_eq!(typed(&db, "SELECT Round(m DIV 7, 2) FROM Z"), "CURRENCY $0.86");
    assert_eq!(typed(&db, "SELECT Round(r DIV 7, 2) FROM Z"), "RATIONAL 43/50");
    assert_eq!(typed(&db, "SELECT Int(-2.5)"), "DECIMAL -3");
    assert_eq!(typed(&db, "SELECT Fix(CRat(-5) DIV 2)"), "BIGNUM -2");
    assert_eq!(typed(&db, "SELECT Sqr(f) FROM Z").split(' ').next(), Some("FLOAT"));
    assert_eq!(typed(&db, "SELECT Sgn(r) FROM Z"), "INTEGER 1");
    // Errors rather than infinities, NaNs or silent wrap-around.
    for bad in ["SELECT d * 1e300 * 1e300 FROM Z", "SELECT f * CSng(1e30) * CSng(1e30) FROM Z", ] {
        assert!(db.query(bad).is_err(), "{bad}");
    }
    // Dividing by zero, in any type, is NULL.
    for null in ["SELECT 1 / (i - 6) FROM Z", "SELECT r / (r - 6) FROM Z", "SELECT d / (d - 6) FROM Z", "SELECT f / (f - 6) FROM Z", "SELECT m DIV (i - 6) FROM Z", "SELECT 0 ^ -1"] {
        assert!(db.query(null).unwrap().rows[0][0].is_null(), "{null}");
    }
}

#[test]
fn rationals_and_conversions() {
    let db = Database::in_memory();
    assert_eq!(typed(&db, "SELECT CRat(1) DIV 3"), "RATIONAL 1/3");
    assert_eq!(typed(&db, "SELECT CRat(1) DIV 3 + CRat(1) DIV 6"), "RATIONAL 1/2");
    assert_eq!(typed(&db, "SELECT Rational(6, 8)"), "RATIONAL 3/4");
    assert_eq!(typed(&db, "SELECT Rational(1, -2)"), "RATIONAL -1/2");
    assert_eq!(typed(&db, "SELECT CRat('7/2')"), "RATIONAL 7/2");
    assert_eq!(typed(&db, "SELECT CRat(0.75)"), "RATIONAL 3/4");
    assert_eq!(typed(&db, "SELECT CRat(CDbl(0.1))"), "RATIONAL 1/10");
    assert_eq!(typed(&db, "SELECT Numerator(CRat(6) DIV 4)"), "BIGNUM 3");
    assert_eq!(typed(&db, "SELECT Denominator(CRat(6) DIV 4)"), "BIGNUM 2");
    assert_eq!(typed(&db, "SELECT CDec(CRat(1) DIV 4)"), "DECIMAL 0.25");
    assert_eq!(typed(&db, "SELECT CDbl(CRat(1) DIV 4)"), "DOUBLE 0.25");
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
    assert_eq!(q(&db, "SELECT CRat(2) DIV 4 = 0.5"), "Yes");
    assert_eq!(q(&db, "SELECT CDbl(0.5) = 0.5"), "Yes");
    assert_eq!(q(&db, "SELECT CDbl(0.1) = 0.1"), "No");
    assert_eq!(q(&db, "SELECT CDec(CDbl(0.1)) = 0.1"), "Yes");
    assert_eq!(q(&db, "SELECT CSng(0.1) = CDbl(0.1)"), "No");
    assert_eq!(q(&db, "SELECT CRat(1) / 3 = 1 / 3"), "No"); // the decimal is rounded at 50 digits
    assert_eq!(q(&db, "SELECT CRat(1) DIV 3 < 1 DIV 3"), "No");
    assert_eq!(q(&db, "SELECT CRat(1) DIV 3 > 0.33333333333333333333333333333333333333333333333333"), "Yes");
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
         INSERT INTO D VALUES (2 DIV 2, 'd1'), (7 DIV 2, 'd2'), (6 DIV 2, 'd3');
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
        "CREATE TABLE T (a INTEGER, b CURRENCY, c RATIONAL, total CALCULATED AS (a * b), share CALCULATED AS (c DIV a));
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
    db.execute("ALTER TABLE Orders ADD COLUMN Tax CALCULATED AS (Total DIV 10)").unwrap();
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
    // Only grouping columns can be read in a grouped query, with a clear message.
    let e = db
        .query("SELECT City, Count(*) FROM Customers c GROUP BY City HAVING EXISTS (SELECT 1 FROM Orders o WHERE o.CustID = c.ID)")
        .unwrap_err();
    assert!(e.to_string().contains("GROUP BY"), "{e}");
    // (A sub-query in a grouped query may read the grouping columns: see correlated_subqueries_in_grouped_queries.)
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
        .execute("MERGE INTO T USING (VALUES (9, 'n'), (8, 'm')) AS s(k, v) ON T.K = s.k WHEN NOT MATCHED THEN INSERT (K, V) VALUES (s.k, s.k * CDbl(1e300) * CDbl(1e300))")
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

// ----- INTERVAL and time zones ---------------------------------------------------------------

#[test]
fn intervals() {
    let db = Database::in_memory();
    let one = |sql: &str| q(&db, sql);
    assert_eq!(one("SELECT INTERVAL '1 year 2 months 3 days 04:05:06'"), "1 year 2 months 3 days 04:05:06");
    assert_eq!(one("SELECT INTERVAL '5' DAY"), "5 days");
    assert_eq!(one("SELECT INTERVAL '1-6' YEAR TO MONTH"), "1 year 6 months");
    assert_eq!(one("SELECT INTERVAL '2 03:04:05' DAY TO SECOND"), "2 days 03:04:05");
    assert_eq!(one("SELECT INTERVAL '90' MINUTE"), "01:30:00");
    assert_eq!(one("SELECT INTERVAL 'P1Y2M3DT4H5M6S'"), "1 year 2 months 3 days 04:05:06");
    assert_eq!(one("SELECT TypeName(INTERVAL '1 day')"), "INTERVAL");
    // Arithmetic.
    assert_eq!(one("SELECT INTERVAL '1 day' * 3"), "3 days");
    assert_eq!(one("SELECT 2 * INTERVAL '90 minutes'"), "03:00:00");
    assert_eq!(one("SELECT INTERVAL '10 hours' / 4"), "02:30:00");
    assert_eq!(one("SELECT INTERVAL '1 month' * 0.5"), "15 days");
    assert_eq!(one("SELECT INTERVAL '1 day' + INTERVAL '2 hours' - INTERVAL '30 minutes'"), "1 day 01:30:00");
    assert_eq!(one("SELECT -INTERVAL '1 day'"), "-1 day");
    // Dates: months clamp to the end of a month, a day is a calendar day.
    assert_eq!(one("SELECT #2024-01-31# + INTERVAL '1 month'"), "2024-02-29");
    assert_eq!(one("SELECT #2024-03-05 10:00# - INTERVAL '2 hours 30 minutes'"), "2024-03-05 07:30:00");
    assert_eq!(one("SELECT INTERVAL '1 year' + #2023-02-28#"), "2024-02-28");
    // Comparison treats a month as 30 days and a day as 24 hours.
    assert_eq!(one("SELECT INTERVAL '1 month' = INTERVAL '30 days'"), "Yes");
    assert_eq!(one("SELECT INTERVAL '25 hours' > INTERVAL '1 day'"), "Yes");
    assert_eq!(one("SELECT INTERVAL '1 day' IN (INTERVAL '24 hours', INTERVAL '2 days')"), "Yes");
    // Parts, construction, age and text.
    assert_eq!(one("SELECT EXTRACT(MONTH FROM INTERVAL '14 months 3 days')"), "2");
    assert_eq!(one("SELECT EXTRACT(YEAR FROM INTERVAL '14 months')"), "1");
    assert_eq!(one("SELECT EXTRACT(EPOCH FROM INTERVAL '1 hour')"), "3600");
    assert_eq!(one("SELECT MakeInterval(1, 2, 3, 4, 5, 6)"), "1 year 2 months 3 days 04:05:06");
    assert_eq!(one("SELECT AGE(#2024-03-05#, #2021-01-10#)"), "3 years 1 month 24 days");
    assert_eq!(one("SELECT CAST('2 days' AS INTERVAL) + CAST('3 hours' AS INTERVAL)"), "2 days 03:00:00");
    assert!(db.query("SELECT INTERVAL 'soon'").is_err());
    assert!(db.query("SELECT INTERVAL '1 day' / 0").unwrap().rows[0][0].is_null());

    // A column, aggregates and series.
    let mut db = db;
    db.execute("CREATE TABLE J (id INT, dur INTERVAL); INSERT INTO J VALUES (1, '2 hours'), (2, '90 minutes'), (3, NULL)").unwrap();
    assert_eq!(rows(&db, "SELECT Sum(dur), Avg(dur), Max(dur), Min(dur) FROM J"), [["03:30:00", "01:45:00", "02:00:00", "01:30:00"]]);
    assert_eq!(rows(&db, "SELECT id FROM J WHERE dur > INTERVAL '100 minutes'"), [["1"]]);
    assert_eq!(rows(&db, "SELECT id FROM J ORDER BY dur DESC NULLS LAST LIMIT 2"), [["1"], ["2"]]);
    let r = rows(&db, "SELECT * FROM GENERATE_SERIES(#2024-01-31#, #2024-04-30#, INTERVAL '1 month')");
    assert_eq!(r, [["2024-01-31"], ["2024-02-29"], ["2024-03-29"], ["2024-04-29"]]);
    assert_eq!(rows(&db, "SELECT * FROM GENERATE_SERIES(#2024-03-05 00:00#, #2024-03-05 06:00#, INTERVAL '3 hours')").len(), 3);
}

#[test]
fn time_zones() {
    let mut db = Database::in_memory();
    // In UTC, the default session zone.
    assert_eq!(q(&db, "SELECT SessionTimeZone()"), "UTC");
    assert_eq!(q(&db, "SELECT TIMESTAMP WITH TIME ZONE '2024-03-05 14:30:00+01:00'"), "2024-03-05 13:30:00+00:00");
    assert_eq!(q(&db, "SELECT TypeName(TIMESTAMP WITH TIME ZONE '2024-03-05 14:30:00Z')"), "TIMESTAMPTZ");
    // The session zone decides how an offset-less reading is taken and how an instant is shown.
    db.execute("SET TIME ZONE 'America/New_York'").unwrap();
    assert_eq!(db.time_zone(), "America/New_York");
    assert_eq!(q(&db, "SELECT TIMESTAMP WITH TIME ZONE '2024-07-15 12:00:00'"), "2024-07-15 12:00:00-04:00");
    assert_eq!(q(&db, "SELECT TIMESTAMP WITH TIME ZONE '2024-01-15 12:00:00'"), "2024-01-15 12:00:00-05:00");
    assert_eq!(q(&db, "SELECT TIMESTAMP WITH TIME ZONE '2024-07-15 12:00:00+00:00'"), "2024-07-15 08:00:00-04:00");
    // AT TIME ZONE goes both ways.
    assert_eq!(q(&db, "SELECT TIMESTAMP WITH TIME ZONE '2024-07-15 12:00:00+00:00' AT TIME ZONE 'Asia/Tokyo'"), "2024-07-15 21:00:00");
    assert_eq!(q(&db, "SELECT #2024-07-15 12:00:00# AT TIME ZONE 'Europe/Paris'"), "2024-07-15 06:00:00-04:00");
    assert_eq!(q(&db, "SELECT TIMEZONE('+05:30', TIMESTAMP WITH TIME ZONE '2024-07-15 12:00:00+00:00')"), "2024-07-15 17:30:00");
    assert_eq!(q(&db, "SELECT #2024-07-15 12:00:00# AT TIME ZONE INTERVAL '2 hours'"), "2024-07-15 06:00:00-04:00");
    // A day is a calendar day across the clock change; 24 hours are not.
    assert_eq!(q(&db, "SELECT TIMESTAMP WITH TIME ZONE '2024-03-09 12:00:00' + INTERVAL '1 day'"), "2024-03-10 12:00:00-04:00");
    assert_eq!(q(&db, "SELECT TIMESTAMP WITH TIME ZONE '2024-03-09 12:00:00' + INTERVAL '24 hours'"), "2024-03-10 13:00:00-04:00");
    assert_eq!(q(&db, "SELECT TIMESTAMP WITH TIME ZONE '2024-03-10 12:00:00' - TIMESTAMP WITH TIME ZONE '2024-03-09 12:00:00'"), "23:00:00");
    // Parts are those of the session wall clock.
    assert_eq!(q(&db, "SELECT EXTRACT(HOUR FROM TIMESTAMP WITH TIME ZONE '2024-07-15 12:00:00+00:00')"), "8");
    assert_eq!(q(&db, "SELECT EXTRACT(TIMEZONE_HOUR FROM TIMESTAMP WITH TIME ZONE '2024-07-15 12:00:00+00:00')"), "-4");
    assert_eq!(q(&db, "SELECT Format(TIMESTAMP WITH TIME ZONE '2024-07-15 12:00:00+00:00', 'yyyy-mm-dd hh:nn')"), "2024-07-15 08:00");
    // Converting between the two timestamp types.
    assert_eq!(q(&db, "SELECT CAST(#2024-07-15 12:00:00# AS TIMESTAMPTZ)"), "2024-07-15 12:00:00-04:00");
    assert_eq!(q(&db, "SELECT CAST(TIMESTAMP WITH TIME ZONE '2024-07-15 12:00:00+00:00' AS DATETIME)"), "2024-07-15 08:00:00");
    assert!(db.execute("SET TIME ZONE 'Nowhere/Land'").is_err());
    assert!(db.query("SELECT #2024-07-15# AT TIME ZONE 'Nowhere/Land'").is_err());

    // Columns: stored as instants, so a later change of session zone moves the display, not the value.
    db.execute("CREATE TABLE Ev (id INT, at TIMESTAMPTZ); CREATE INDEX evat ON Ev (at)").unwrap();
    db.execute("INSERT INTO Ev VALUES (1, '2024-03-05 10:00:00+01:00'), (2, '2024-03-05 09:30:00+00:00'), (3, '2024-03-05 12:00:00')").unwrap();
    assert_eq!(rows(&db, "SELECT id FROM Ev ORDER BY at"), [["1"], ["2"], ["3"]]);
    assert_eq!(rows(&db, "SELECT id FROM Ev WHERE at = TIMESTAMP WITH TIME ZONE '2024-03-05 09:00:00+00:00'"), [["1"]]);
    db.execute("SET TIME ZONE 'UTC'").unwrap();
    assert_eq!(rows(&db, "SELECT at FROM Ev WHERE id = 1"), [["2024-03-05 09:00:00+00:00"]]);
    // The datetime and the zoned type compare as UTC readings.
    assert_eq!(q(&db, "SELECT TIMESTAMP WITH TIME ZONE '2024-03-05 09:00:00+00:00' = #2024-03-05 09:00:00#"), "Yes");

    // The optimizer reads a range of instants through the index and sorts nothing.
    db.set_optimizer(true);
    let plan = db
        .query("EXPLAIN SELECT id FROM Ev WHERE at >= TIMESTAMP WITH TIME ZONE '2024-03-05 00:00:00+00:00' AND at < TIMESTAMP WITH TIME ZONE '2024-03-05 00:00:00+00:00' + INTERVAL '1 day'")
        .unwrap()
        .rows
        .iter()
        .map(|r| r[0].to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(plan.contains("index range scan evat"), "{plan}");
    let plan = db.query("EXPLAIN SELECT id FROM Ev ORDER BY at DESC LIMIT 1").unwrap().rows.iter().map(|r| r[0].to_string()).collect::<Vec<_>>().join("\n");
    assert!(plan.contains("no sort"), "{plan}");
}

// ----- CREATE OR REPLACE, RENAME TO, SET/DROP DEFAULT and NOT NULL --------------------------------

#[test]
fn replace_rename_and_column_settings() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Parent (id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE Child (id INTEGER PRIMARY KEY, pid INTEGER REFERENCES Parent (id), amt INTEGER CHECK (amt >= 0));
         CREATE TABLE Log (msg TEXT);
         CREATE TRIGGER logp AFTER INSERT ON Parent BEGIN INSERT INTO Log VALUES ('added ' & NEW.name); END;
         CREATE VIEW PV AS SELECT name FROM Parent WHERE id > 0;
         INSERT INTO Parent VALUES (1, 'a');
         INSERT INTO Child VALUES (1, 1, 5);",
    )
    .unwrap();

    // OR REPLACE for views and triggers; not for tables.
    db.execute("CREATE OR REPLACE VIEW PV AS SELECT name, id FROM Parent").unwrap();
    assert_eq!(db.query("SELECT * FROM PV").unwrap().columns, ["name", "id"]);
    db.execute("CREATE OR REPLACE VIEW Brand AS SELECT 1 AS x").unwrap(); // creates when absent
    assert!(db.execute("CREATE OR REPLACE TABLE Parent (x INT)").is_err());
    assert!(db.execute("CREATE OR REPLACE VIEW Parent AS SELECT 1").is_err()); // a table is in the way
    db.execute("CREATE OR REPLACE TRIGGER logp AFTER INSERT ON Parent BEGIN INSERT INTO Log VALUES ('new ' & NEW.name); END").unwrap();
    db.execute("INSERT INTO Parent VALUES (2, 'b')").unwrap();
    assert_eq!(rows(&db, "SELECT msg FROM Log"), [["added a"], ["new b"]]);
    // A failing replacement leaves the old definition in place.
    assert!(db.execute("CREATE OR REPLACE VIEW PV AS SELECT nosuchcolumn FROM Parent").is_err());
    assert_eq!(db.query("SELECT * FROM PV").unwrap().columns, ["name", "id"]);

    // Renaming a table follows its relationships, triggers, checks and saved queries.
    db.execute("ALTER TABLE Parent RENAME TO Guardian").unwrap();
    assert!(db.table("Parent").is_none() && db.table("Guardian").is_some());
    assert_eq!(rows(&db, "SELECT name FROM PV ORDER BY id"), [["a"], ["b"]]);
    db.execute("INSERT INTO Guardian VALUES (3, 'c')").unwrap();
    assert_eq!(rows(&db, "SELECT msg FROM Log ORDER BY msg"), [["added a"], ["new b"], ["new c"]]);
    assert!(db.execute("INSERT INTO Child VALUES (2, 99, 1)").is_err()); // the relationship still holds
    db.execute("INSERT INTO Child VALUES (2, 3, 1)").unwrap();
    assert!(db.execute("INSERT INTO Child VALUES (3, 1, -1)").is_err()); // and so does the CHECK
    assert!(db.execute("DROP TABLE Guardian").is_err()); // Child still refers to it
    db.execute("ALTER TABLE Child RENAME TO Kid").unwrap();
    assert_eq!(rows(&db, "SELECT Count(*) FROM Kid"), [["2"]]);
    assert!(db.execute("INSERT INTO Kid VALUES (9, 1, -5)").is_err());
    assert!(db.execute("ALTER TABLE Kid RENAME TO Guardian").is_err()); // taken
    assert!(db.execute("ALTER TABLE Kid RENAME TO PV").is_err()); // taken by a query
    assert!(db.execute("ALTER TABLE Nothing RENAME TO X").is_err());
    // It can be rolled back with the rest of a transaction.
    db.execute("BEGIN; ALTER TABLE Kid RENAME TO Gone; ROLLBACK").unwrap();
    assert!(db.table("Kid").is_some() && db.table("Gone").is_none());
    db.execute("INSERT INTO Kid VALUES (4, 1, 1)").unwrap();

    // Defaults and requiredness.
    db.execute("CREATE TABLE T (a INTEGER, b TEXT, c INTEGER PRIMARY KEY)").unwrap();
    db.execute("ALTER TABLE T ALTER COLUMN a SET DEFAULT 7").unwrap();
    db.execute("ALTER TABLE T ALTER COLUMN b SET DEFAULT 'x'").unwrap();
    db.execute("INSERT INTO T (c) VALUES (1)").unwrap();
    assert_eq!(rows(&db, "SELECT a, b FROM T"), [["7", "x"]]);
    db.execute("ALTER TABLE T ALTER COLUMN a DROP DEFAULT").unwrap();
    db.execute("INSERT INTO T (c) VALUES (2)").unwrap();
    assert_eq!(rows(&db, "SELECT a FROM T WHERE c = 2"), [[""]]);
    assert!(db.execute("ALTER TABLE T ALTER COLUMN a SET DEFAULT 'not a number'").is_err());
    // SET NOT NULL is refused while records lack a value, and then works.
    let e = db.execute("ALTER TABLE T ALTER COLUMN a SET NOT NULL").unwrap_err();
    assert!(e.to_string().contains("1 record(s)"), "{e}");
    db.execute("UPDATE T SET a = 0 WHERE a IS NULL").unwrap();
    db.execute("ALTER TABLE T ALTER COLUMN a SET NOT NULL").unwrap();
    assert!(db.execute("INSERT INTO T (c, a) VALUES (3, NULL)").is_err());
    db.execute("ALTER TABLE T ALTER COLUMN a DROP NOT NULL").unwrap();
    db.execute("INSERT INTO T (c, a) VALUES (3, NULL)").unwrap();
    assert!(db.execute("ALTER TABLE T ALTER COLUMN c DROP NOT NULL").is_err()); // primary key
}

// ----- views with column lists, and changing data through simple views -------------------------

fn plan_text(db: &Database, sql: &str) -> String {
    db.query(&format!("EXPLAIN {sql}")).unwrap().rows.iter().map(|r| r[0].to_string()).collect::<Vec<_>>().join("\n")
}

#[test]
fn views_with_column_lists_and_updatable_views() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Prod (id INTEGER PRIMARY KEY, name TEXT, price CURRENCY, active YESNO DEFAULT TRUE, stock INTEGER DEFAULT 0);
         CREATE INDEX pname ON Prod (name);
         INSERT INTO Prod VALUES (1,'nut',0.10,TRUE,100),(2,'bolt',0.25,TRUE,50),(3,'cog',3,FALSE,0),(4,'gear',9,TRUE,5);",
    )
    .unwrap();

    // Names for the columns.
    db.execute("CREATE VIEW Cheap (code, label, cost) AS SELECT id, name, price FROM Prod WHERE price < 1").unwrap();
    assert_eq!(db.query("SELECT * FROM Cheap").unwrap().columns, ["code", "label", "cost"]);
    assert_eq!(rows(&db, "SELECT label FROM Cheap ORDER BY code"), [["nut"], ["bolt"]]);
    assert!(db.execute("CREATE VIEW Bad (a) AS SELECT id, name FROM Prod").is_err()); // wrong number of names
    // Parameters still go in the same place.
    db.execute("CREATE QUERY Over (@p CURRENCY) AS SELECT name FROM Prod WHERE price > @p").unwrap();
    assert_eq!(db.execute("EXEC Over 5").unwrap().rows[0][0].to_string(), "gear");

    // DELETE, UPDATE and INSERT through a view act on its table, within the view's condition.
    db.execute("UPDATE Cheap SET cost = cost * 2 WHERE label = 'nut'").unwrap();
    assert_eq!(rows(&db, "SELECT price FROM Prod WHERE id = 1"), [["$0.20"]]);
    assert_eq!(db.execute("UPDATE Cheap SET cost = 0.5").unwrap().affected, 2); // not the dearer ones
    assert_eq!(rows(&db, "SELECT price FROM Prod WHERE id = 4"), [["$9.00"]]);
    db.execute("INSERT INTO Cheap (code, label, cost) VALUES (10, 'pin', 0.05)").unwrap();
    assert_eq!(rows(&db, "SELECT name, active, stock FROM Prod WHERE id = 10"), [["pin", "Yes", "0"]]); // base defaults
    assert_eq!(db.execute("DELETE FROM Cheap WHERE label = 'pin'").unwrap().affected, 1);
    assert_eq!(db.execute("DELETE FROM Cheap WHERE code = 4").unwrap().affected, 0); // gear is not in the view

    // WITH CHECK OPTION refuses a row that would leave the view.
    db.execute("CREATE VIEW Active AS SELECT id, name, price FROM Prod WHERE active WITH CHECK OPTION").unwrap();
    assert!(db.execute("INSERT INTO Active (id, name, price) VALUES (20, 'cam', 1)").is_ok()); // active defaults to TRUE
    db.execute("CREATE VIEW Pricey AS SELECT id, name, price FROM Prod WHERE price > 5 WITH CHECK OPTION").unwrap();
    let e = db.execute("UPDATE Pricey SET price = 1 WHERE id = 4").unwrap_err();
    assert!(e.to_string().contains("CHECK OPTION"), "{e}");
    assert!(db.execute("INSERT INTO Pricey (id, name, price) VALUES (30, 'x', 2)").is_err());
    db.execute("UPDATE Pricey SET price = 20 WHERE id = 4").unwrap();
    // A view over a view.
    db.execute("CREATE VIEW Pricey2 AS SELECT id, name FROM Pricey").unwrap();
    db.execute("UPDATE Pricey2 SET name = 'big gear' WHERE id = 4").unwrap();
    assert_eq!(rows(&db, "SELECT name FROM Prod WHERE id = 4"), [["big gear"]]);

    // Views that cannot take changes say why.
    db.execute("CREATE VIEW Sum1 AS SELECT Count(*) AS n FROM Prod").unwrap();
    db.execute("CREATE VIEW Calc AS SELECT id, price * 2 AS twice FROM Prod").unwrap();
    db.execute("CREATE VIEW Joined AS SELECT P.id, Q.name FROM Prod P JOIN Prod Q ON Q.id = P.id").unwrap();
    for v in ["Sum1", "Calc", "Joined"] {
        let e = db.execute(&format!("DELETE FROM {v}")).unwrap_err();
        assert!(e.to_string().contains("cannot be changed"), "{v}: {e}");
    }
    assert!(db.execute("UPDATE Calc SET twice = 1").is_err());
    assert!(db.execute("INSERT INTO Cheap (nosuch) VALUES (1)").is_err());

    // Reading a view that shows all its table's columns goes through the table's indexes.
    db.execute("CREATE VIEW Live AS SELECT * FROM Prod WHERE active").unwrap();
    let plan = plan_text(&db, "SELECT id FROM Live WHERE name = 'nut'");
    assert!(plan.contains("scan Live") && plan.contains("index pname on name"), "{plan}");
    for sql in [
        "SELECT id, name FROM Live WHERE name = 'bolt' OR stock > 90 ORDER BY id",
        "SELECT Count(*) FROM Live",
        "SELECT L.id, P.name FROM Live L LEFT JOIN Prod P ON P.id = L.id ORDER BY 1",
        "SELECT P.id, L.name FROM Prod P LEFT JOIN Live L ON L.id = P.id ORDER BY 1",
        "SELECT P.id, L.name FROM Prod P RIGHT JOIN Live L ON L.id = P.id ORDER BY 1",
        "SELECT L.id, P.name FROM Live L FULL JOIN Prod P ON P.id = L.id ORDER BY 1, 2",
        "SELECT id FROM Live ORDER BY price DESC LIMIT 2",
    ] {
        db.set_optimizer(true);
        let on = rows(&db, sql);
        db.set_optimizer(false);
        assert_eq!(on, rows(&db, sql), "{sql}");
    }
}

// ----- named windows and RANGE offsets ---------------------------------------------------------

#[test]
fn named_windows_and_range_offsets() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE S (d DATETIME, v INTEGER, g TEXT);
         INSERT INTO S VALUES (#2024-01-01#, 10, 'a'), (#2024-01-02#, 20, 'a'), (#2024-01-04#, 30, 'a'),
                              (#2024-01-05#, 40, 'a'), (#2024-01-10#, 50, 'a'), (#2024-01-01#, 1, 'b'), (#2024-01-03#, 2, 'b');",
    )
    .unwrap();
    // A window named once and used several times.
    let r = rows(
        &db,
        "SELECT v, SUM(v) OVER w, ROW_NUMBER() OVER w, RANK() OVER (w) FROM S WHERE g = 'a'
         WINDOW w AS (ORDER BY d) ORDER BY d",
    );
    assert_eq!(r[2], ["30", "60", "3", "3"]);
    // A window based on another may add what it lacks.
    let r = rows(
        &db,
        "SELECT g, v, SUM(v) OVER (byg ORDER BY d ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM S
         WINDOW byg AS (PARTITION BY g) ORDER BY g, d",
    );
    assert_eq!(r[1], ["a", "20", "30"]);
    assert_eq!(r[6], ["b", "2", "3"]);
    // Two named windows, one built on the other.
    let r = rows(&db, "SELECT v, SUM(v) OVER w2 FROM S WINDOW w1 AS (PARTITION BY g), w2 AS (w1 ORDER BY d) ORDER BY g, d");
    assert_eq!(r[4], ["50", "150"]);
    // Mistakes are reported.
    assert!(db.query("SELECT SUM(v) OVER nosuch FROM S").is_err());
    assert!(db.query("SELECT SUM(v) OVER (w ORDER BY d) FROM S WINDOW w AS (ORDER BY v)").is_err()); // cannot re-order
    assert!(db.query("SELECT 1 FROM S WINDOW w AS (ORDER BY d), w AS (ORDER BY v)").is_err());

    // RANGE with an offset: the rows within 2 days before (dates take an interval or days).
    let r = rows(
        &db,
        "SELECT v, SUM(v) OVER (ORDER BY d RANGE BETWEEN INTERVAL '2 days' PRECEDING AND CURRENT ROW) FROM S WHERE g = 'a' ORDER BY d",
    );
    assert_eq!(r.iter().map(|x| x[1].clone()).collect::<Vec<_>>(), ["10", "30", "50", "70", "50"]);
    // Numbers: a window of values 15 either side of each.
    let r = rows(&db, "SELECT v, Count(*) OVER (ORDER BY v RANGE BETWEEN 15 PRECEDING AND 15 FOLLOWING) FROM S WHERE g = 'a' ORDER BY v");
    assert_eq!(r.iter().map(|x| x[1].clone()).collect::<Vec<_>>(), ["2", "3", "3", "3", "2"]);
    // Descending order flips what preceding means.
    let r = rows(&db, "SELECT v, Sum(v) OVER (ORDER BY v DESC RANGE BETWEEN 10 PRECEDING AND CURRENT ROW) FROM S WHERE g = 'a' ORDER BY v DESC");
    assert_eq!(r[0], ["50", "50"]);
    assert_eq!(r[1], ["40", "90"]);
    // NULL sort values frame only their peers.
    db.execute("INSERT INTO S VALUES (NULL, 7, 'c'), (NULL, 8, 'c'), (#2024-02-01#, 9, 'c')").unwrap();
    let r = rows(&db, "SELECT v, SUM(v) OVER (ORDER BY d RANGE BETWEEN INTERVAL '1 day' PRECEDING AND CURRENT ROW) FROM S WHERE g = 'c' ORDER BY d, v");
    assert_eq!(r, [["7", "15"], ["8", "15"], ["9", "9"]]);
    // A frame with an offset needs one ORDER BY column and a matching kind of offset.
    assert!(db.query("SELECT SUM(v) OVER (ORDER BY d, v RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM S").is_err());
    assert!(db.query("SELECT SUM(v) OVER (ORDER BY d RANGE BETWEEN 'x' PRECEDING AND CURRENT ROW) FROM S").is_err());
    assert!(db.query("SELECT SUM(v) OVER (ORDER BY v ROWS BETWEEN 1.5 PRECEDING AND CURRENT ROW) FROM S").is_err());
}

// ----- PIVOT and UNPIVOT in FROM -----------------------------------------------------------------

#[test]
fn pivot_and_unpivot() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Sales (region TEXT, q INTEGER, amount INTEGER, rep TEXT);
         INSERT INTO Sales VALUES ('East',1,10,'a'),('East',2,20,'a'),('East',2,5,'b'),('West',1,7,'a'),('West',4,3,'c');
         CREATE TABLE Wide (id INTEGER, q1 INTEGER, q2 INTEGER, q3 INTEGER, tag TEXT);
         INSERT INTO Wide VALUES (1, 10, 20, NULL, 'x'), (2, 5, NULL, 7, 'y');",
    )
    .unwrap();
    // PIVOT: the columns not used by the aggregate or the pivot column head the rows.
    let r = db
        .query("SELECT * FROM Sales PIVOT (SUM(amount) FOR q IN (1, 2, 4)) AS p ORDER BY region, rep")
        .unwrap();
    assert_eq!(r.columns, ["region", "rep", "1", "2", "4"]);
    let rows_: Vec<Vec<String>> = r.rows.iter().map(|x| x.iter().map(|v| v.to_string()).collect()).collect();
    assert_eq!(rows_[0], ["East", "a", "10", "20", ""]);
    assert_eq!(rows_[1], ["East", "b", "", "5", ""]);
    assert_eq!(rows_.len(), 4);
    // Labels, a derived source to choose the heading columns, and a count that shows 0.
    let r = db
        .query("SELECT * FROM (SELECT region, q FROM Sales) AS s PIVOT (COUNT(*) FOR q IN (1 AS first, 2 AS second)) AS p ORDER BY region")
        .unwrap();
    assert_eq!(r.columns, ["region", "first", "second"]);
    let rows_: Vec<Vec<String>> = r.rows.iter().map(|x| x.iter().map(|v| v.to_string()).collect()).collect();
    assert_eq!(rows_, [["East", "1", "2"], ["West", "1", "0"]]);
    // The result is an ordinary table: filter it, join it, aggregate it.
    assert_eq!(
        rows(&db, "SELECT region FROM Sales PIVOT (SUM(amount) FOR q IN (1, 2)) AS p WHERE [1] IS NOT NULL AND [2] IS NULL"),
        [["West"]]
    );

    // UNPIVOT: columns become (name, value) rows; NULLs are left out unless asked for.
    let r = db.query("SELECT * FROM Wide UNPIVOT (amount FOR quarter IN (q1, q2, q3)) AS u ORDER BY id, quarter").unwrap();
    assert_eq!(r.columns, ["id", "tag", "quarter", "amount"]);
    let rows_: Vec<Vec<String>> = r.rows.iter().map(|x| x.iter().map(|v| v.to_string()).collect()).collect();
    assert_eq!(rows_, [["1", "x", "q1", "10"], ["1", "x", "q2", "20"], ["2", "y", "q1", "5"], ["2", "y", "q3", "7"]]);
    assert_eq!(rows(&db, "SELECT Count(*) FROM Wide UNPIVOT INCLUDE NULLS (v FOR n IN (q1, q2, q3)) AS u"), [["6"]]);
    assert_eq!(
        rows(&db, "SELECT n FROM Wide UNPIVOT (v FOR n IN (q1 AS 'Q one', q2 AS 'Q two')) AS u WHERE id = 1 ORDER BY n"),
        [["Q one"], ["Q two"]]
    );
    // Round trip: unpivot the pivot.
    let r = rows(
        &db,
        "SELECT region, q, v FROM (SELECT region, SUM(amount) AS [1], SUM(amount) AS [2] FROM Sales GROUP BY region) AS t UNPIVOT (v FOR q IN ([1], [2])) AS u ORDER BY region, q",
    );
    assert_eq!(r.len(), 4);
    assert!(db.query("SELECT * FROM Wide UNPIVOT (v FOR n IN (nosuch)) AS u").is_err());
    assert!(db.query("SELECT * FROM Sales PIVOT (amount FOR q IN (1)) AS p").is_err()); // not an aggregate
}

#[test]
fn keyset_paging_with_row_values_uses_the_index() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE Log (Day INTEGER, ID INTEGER, Note TEXT); CREATE INDEX byday ON Log (Day, ID)").unwrap();
    let mut script = String::new();
    for i in 0..2000 {
        script += &format!("INSERT INTO Log VALUES ({}, {i}, 'n');\n", i / 10);
    }
    db.execute(&script).unwrap();
    let sql = "SELECT Day, ID FROM Log WHERE (Day, ID) > (150, 1503) ORDER BY Day, ID LIMIT 3";
    assert_eq!(rows(&db, sql), [["150", "1504"], ["150", "1505"], ["150", "1506"]]);
    let plan = plan_text(&db, sql);
    assert!(plan.contains("index range scan byday") || plan.contains("order by satisfied by index byday"), "{plan}");
    // The index narrows the read to the rows from the bookmark on.
    let plan = plan_text(&db, "SELECT Day, ID FROM Log WHERE (Day, ID) > (150, 1503)");
    assert!(plan.contains("index range scan byday"), "{plan}");
    // Same answers with the optimizer off.
    db.set_optimizer(false);
    assert_eq!(rows(&db, sql), [["150", "1504"], ["150", "1505"], ["150", "1506"]]);
}

#[test]
fn sequences_count_and_cycle() {
    let mut db = Database::in_memory();
    db.execute("CREATE SEQUENCE s START WITH 10 INCREMENT BY 5").unwrap();
    assert_eq!(rows(&db, "SELECT NEXTVAL('s'), NEXT VALUE FOR s, CURRVAL('s'), LASTVAL()"), [["10", "15", "15", "15"]]);
    // Each row of a query gets its own number.
    db.execute("CREATE TABLE t (a INTEGER)").unwrap();
    db.execute("INSERT INTO t VALUES (1), (2), (3)").unwrap();
    assert_eq!(rows(&db, "SELECT a, NEXTVAL('s') FROM t ORDER BY a"), [["1", "20"], ["2", "25"], ["3", "30"]]);
    assert_eq!(rows(&db, "SELECT SETVAL('s', 100), NEXTVAL('s')"), [["100", "105"]]);
    assert_eq!(rows(&db, "SELECT SETVAL('s', 7, FALSE), NEXTVAL('s')"), [["7", "7"]]);

    db.execute("CREATE SEQUENCE c MINVALUE 1 MAXVALUE 3 CYCLE; CREATE SEQUENCE d INCREMENT BY -1 START WITH 2 MINVALUE 1 MAXVALUE 5 NO CYCLE").unwrap();
    assert_eq!(rows(&db, "SELECT NEXTVAL('c'), NEXTVAL('c'), NEXTVAL('c'), NEXTVAL('c')"), [["1", "2", "3", "1"]]);
    assert_eq!(rows(&db, "SELECT NEXTVAL('d'), NEXTVAL('d')"), [["2", "1"]]);
    assert!(db.query("SELECT NEXTVAL('d')").unwrap_err().to_string().contains("limit"));
    assert!(db.query("SELECT CURRVAL('nosuch')").is_err());
    db.execute("CREATE SEQUENCE fresh").unwrap();
    assert!(db.query("SELECT CURRVAL('fresh')").is_err()); // nothing handed out yet

    db.execute("ALTER SEQUENCE d RESTART WITH 2 MAXVALUE 5; ALTER SEQUENCE c INCREMENT BY 2").unwrap();
    assert_eq!(rows(&db, "SELECT NEXTVAL('d'), NEXTVAL('d')"), [["2", "1"]]);
    assert!(db.execute("CREATE SEQUENCE s").is_err());
    db.execute("CREATE SEQUENCE IF NOT EXISTS s; DROP SEQUENCE IF EXISTS nosuch").unwrap();
    assert!(db.execute("CREATE SEQUENCE bad INCREMENT BY 0").is_err());
    assert!(db.execute("CREATE SEQUENCE bad START WITH 9 MAXVALUE 5").is_err());
    db.execute("DROP SEQUENCE s").unwrap();
    assert!(db.query("SELECT NEXTVAL('s')").is_err());
    assert_eq!(db.sequences().len(), 3);
}

#[test]
fn sequences_fill_columns_and_survive_rollback() {
    let mut db = Database::in_memory();
    db.execute("CREATE SEQUENCE ids START WITH 100").unwrap();
    db.execute("CREATE TABLE t (id INTEGER DEFAULT NEXT VALUE FOR ids PRIMARY KEY, name TEXT)").unwrap();
    db.execute("INSERT INTO t (name) VALUES ('a'), ('b')").unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (7, 'explicit')").unwrap(); // no number used up
    db.execute("INSERT INTO t (name) VALUES ('c')").unwrap();
    assert_eq!(rows(&db, "SELECT id, name FROM t ORDER BY id"), [["7", "explicit"], ["100", "a"], ["101", "b"], ["102", "c"]]);
    // A rolled-back insert still used its number.
    db.execute("BEGIN").unwrap();
    db.execute("INSERT INTO t (name) VALUES ('gone')").unwrap();
    db.execute("ROLLBACK").unwrap();
    db.execute("INSERT INTO t (name) VALUES ('d')").unwrap();
    assert_eq!(rows(&db, "SELECT id FROM t WHERE name = 'd'"), [["104"]]);
    // A sequence made in a rolled-back transaction is gone.
    db.execute("BEGIN; CREATE SEQUENCE temp; ROLLBACK").unwrap();
    assert!(db.query("SELECT NEXTVAL('temp')").is_err());
    // The default must name a sequence that exists.
    assert!(db.execute("CREATE TABLE u (id INTEGER DEFAULT NEXTVAL('nosuch'))").is_err());
    // Also settable later.
    db.execute("CREATE TABLE v (id INTEGER, n TEXT)").unwrap();
    db.execute("ALTER TABLE v ALTER COLUMN id SET DEFAULT NEXT VALUE FOR ids").unwrap();
    db.execute("INSERT INTO v (n) VALUES ('x')").unwrap();
    assert_eq!(rows(&db, "SELECT id FROM v"), [["105"]]);
}

#[test]
fn nextval_is_not_folded_by_the_optimizer() {
    let mut db = Database::in_memory();
    db.execute("CREATE SEQUENCE s; CREATE TABLE t (a INTEGER); INSERT INTO t VALUES (1), (2), (3)").unwrap();
    for on in [true, false] {
        db.set_optimizer(on);
        db.execute("ALTER SEQUENCE s RESTART").unwrap();
        // Evaluated for every row, not once.
        assert_eq!(rows(&db, "SELECT NEXTVAL('s') FROM t"), [["1"], ["2"], ["3"]], "optimizer {on}");
    }
}

#[test]
fn deferrable_relationships_wait_for_the_commit() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE p (id INTEGER PRIMARY KEY);
         CREATE TABLE c (id INTEGER PRIMARY KEY, pid INTEGER, CONSTRAINT fk_c FOREIGN KEY (pid) REFERENCES p (id) DEFERRABLE INITIALLY DEFERRED);
         CREATE TABLE lazy (id INTEGER PRIMARY KEY, pid INTEGER REFERENCES p (id) DEFERRABLE);
         CREATE TABLE strict (id INTEGER PRIMARY KEY, pid INTEGER REFERENCES p (id))",
    )
    .unwrap();
    // Outside a transaction each statement is checked as it ends.
    assert!(db.execute("INSERT INTO c VALUES (1, 5)").is_err());
    // Inside one, the child may come before its parent.
    db.execute("BEGIN; INSERT INTO c VALUES (1, 5); INSERT INTO p VALUES (5); COMMIT").unwrap();
    assert_eq!(rows(&db, "SELECT id, pid FROM c"), [["1", "5"]]);
    // A violation that is still there at COMMIT undoes the whole transaction.
    let err = db.execute("BEGIN; INSERT INTO p VALUES (6); INSERT INTO c VALUES (2, 9); COMMIT").unwrap_err();
    assert!(err.to_string().contains("cannot commit"), "{err}");
    assert!(!db.in_transaction());
    assert_eq!(rows(&db, "SELECT Count(*) FROM c"), [["1"]]);
    assert_eq!(rows(&db, "SELECT Count(*) FROM p"), [["1"]]);
    // Fixing it before COMMIT is fine; deleting a parent that still has children is caught late as well.
    db.execute("BEGIN; DELETE FROM p WHERE id = 5; INSERT INTO p VALUES (5); COMMIT").unwrap();
    assert!(db.execute("BEGIN; DELETE FROM p WHERE id = 5; COMMIT").is_err());
    // DEFERRABLE alone starts immediate; SET CONSTRAINTS defers it for the transaction.
    assert!(db.execute("BEGIN; INSERT INTO lazy VALUES (1, 7)").is_err());
    db.execute("ROLLBACK").unwrap();
    db.execute("BEGIN; SET CONSTRAINTS ALL DEFERRED; INSERT INTO lazy VALUES (1, 7); INSERT INTO p VALUES (7); COMMIT").unwrap();
    // IMMEDIATE checks at once what was waiting, and the transaction stays open if it fails.
    db.execute("BEGIN; SET CONSTRAINTS fk_c DEFERRED; INSERT INTO c VALUES (3, 99)").unwrap();
    assert!(db.execute("SET CONSTRAINTS fk_c IMMEDIATE").is_err());
    assert!(db.in_transaction());
    db.execute("DELETE FROM c WHERE id = 3; COMMIT").unwrap();
    // Relationships that are not deferrable cannot be named.
    assert!(db.execute("SET CONSTRAINTS nosuch DEFERRED").is_err());
    assert!(db.execute("BEGIN; SET CONSTRAINTS fk_strict_pid DEFERRED").is_err());
    db.execute("ROLLBACK").unwrap();
    assert!(db.execute("BEGIN; INSERT INTO strict VALUES (1, 100)").is_err());
    db.execute("ROLLBACK").unwrap();
}

#[test]
fn set_default_and_no_action() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE p (id INTEGER PRIMARY KEY, name TEXT);
         INSERT INTO p VALUES (0, 'nobody'), (1, 'a'), (2, 'b');
         CREATE TABLE c (id INTEGER PRIMARY KEY, pid INTEGER DEFAULT 0 REFERENCES p (id) ON DELETE SET DEFAULT ON UPDATE CASCADE);
         INSERT INTO c VALUES (1, 1), (2, 2), (3, 1);",
    )
    .unwrap();
    db.execute("DELETE FROM p WHERE id = 1").unwrap();
    assert_eq!(rows(&db, "SELECT id, pid FROM c ORDER BY id"), [["1", "0"], ["2", "2"], ["3", "0"]]);
    db.execute("UPDATE p SET id = 20 WHERE id = 2").unwrap();
    assert_eq!(rows(&db, "SELECT pid FROM c WHERE id = 2"), [["20"]]);
    // A default that points nowhere is refused.
    db.execute("DELETE FROM c; DELETE FROM p WHERE id = 0").unwrap();
    db.execute("INSERT INTO c VALUES (9, 20)").unwrap();
    assert!(db.execute("DELETE FROM p WHERE id = 20").is_err());
    assert_eq!(rows(&db, "SELECT Count(*) FROM p"), [["1"]]);

    db.execute("CREATE TABLE d (id INTEGER PRIMARY KEY, pid INTEGER REFERENCES p (id) ON DELETE NO ACTION ON UPDATE NO ACTION)").unwrap();
    db.execute("INSERT INTO d VALUES (1, 20)").unwrap();
    assert!(db.execute("DELETE FROM p WHERE id = 20").is_err());
    assert!(db.execute("UPDATE p SET id = 21 WHERE id = 20").is_err());
}

#[test]
fn statement_triggers_fire_once_per_statement() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER);
         CREATE TABLE log (seq COUNTER PRIMARY KEY, msg TEXT);
         CREATE TRIGGER before_ins BEFORE INSERT ON t FOR EACH STATEMENT BEGIN INSERT INTO log (msg) VALUES ('before insert'); END;
         CREATE TRIGGER after_ins AFTER INSERT ON t FOR EACH STATEMENT BEGIN INSERT INTO log (msg) VALUES ('after insert'); END;
         CREATE TRIGGER after_any AFTER UPDATE OR DELETE ON t FOR EACH STATEMENT BEGIN INSERT INTO log (msg) VALUES ('changed'); END;
         CREATE TRIGGER each_row AFTER INSERT ON t FOR EACH ROW BEGIN INSERT INTO log (msg) VALUES ('row ' & NEW.id); END",
    )
    .unwrap();
    db.execute("INSERT INTO t VALUES (1, 10), (2, 20), (3, 30)").unwrap();
    // The statement triggers wrap the row ones, and run once however many rows there are.
    assert_eq!(
        rows(&db, "SELECT msg FROM log ORDER BY seq"),
        [["before insert"], ["row 1"], ["row 2"], ["row 3"], ["after insert"]]
    );
    db.execute("DELETE FROM log").unwrap();
    db.execute("UPDATE t SET n = n + 1").unwrap();
    db.execute("DELETE FROM t WHERE id > 1").unwrap();
    assert_eq!(rows(&db, "SELECT msg FROM log ORDER BY seq"), [["changed"], ["changed"]]);
    // Statement triggers have no row to look at.
    assert!(db.execute("CREATE TRIGGER bad AFTER INSERT ON t FOR EACH STATEMENT BEGIN INSERT INTO log (msg) VALUES (NEW.id); END").is_err());
}

#[test]
fn triggers_with_several_events_and_update_of() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, price INTEGER);
         CREATE TABLE log (seq COUNTER PRIMARY KEY, msg TEXT);
         CREATE TRIGGER seen AFTER INSERT OR UPDATE ON items BEGIN INSERT INTO log (msg) VALUES ('saved ' & NEW.name); END;
         CREATE TRIGGER gone AFTER DELETE ON items BEGIN INSERT INTO log (msg) VALUES ('deleted ' & OLD.name); END;
         CREATE TRIGGER repriced AFTER UPDATE OF price ON items BEGIN INSERT INTO log (msg) VALUES (OLD.price & ' -> ' & NEW.price); END",
    )
    .unwrap();
    db.execute("INSERT INTO items VALUES (1, 'pen', 5)").unwrap();
    db.execute("UPDATE items SET name = 'biro' WHERE id = 1").unwrap(); // not the price
    db.execute("UPDATE items SET price = 6 WHERE id = 1").unwrap();
    db.execute("UPDATE items SET price = 6 WHERE id = 1").unwrap(); // unchanged: no repricing
    db.execute("DELETE FROM items").unwrap();
    assert_eq!(
        rows(&db, "SELECT msg FROM log ORDER BY seq"),
        [["saved pen"], ["saved biro"], ["5 -> 6"], ["saved biro"], ["saved biro"], ["deleted biro"]]
    );
    // OLD does not exist for an INSERT.
    assert!(db.execute("CREATE TRIGGER bad AFTER INSERT OR DELETE ON items BEGIN INSERT INTO log (msg) VALUES (OLD.name); END").is_err());
    assert!(db.execute("CREATE TRIGGER bad AFTER UPDATE OF nosuch ON items BEGIN INSERT INTO log (msg) VALUES ('x'); END").is_err());
    assert!(db.execute("CREATE TRIGGER bad AFTER INSERT OR INSERT ON items BEGIN INSERT INTO log (msg) VALUES ('x'); END").is_err());
}

#[test]
fn trigger_bodies_can_select() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE SEQUENCE audit_no;
         CREATE TABLE t (id INTEGER PRIMARY KEY);
         CREATE TABLE audit (num INTEGER, id INTEGER);
         CREATE TRIGGER stamp AFTER INSERT ON t BEGIN SELECT NEXTVAL('audit_no'); INSERT INTO audit VALUES (CURRVAL('audit_no'), NEW.id); END",
    )
    .unwrap();
    db.execute("INSERT INTO t VALUES (7), (8)").unwrap();
    assert_eq!(rows(&db, "SELECT num, id FROM audit ORDER BY num"), [["1", "7"], ["2", "8"]]);
}

#[test]
fn instead_of_triggers_make_any_query_changeable() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE person (id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE phone (person INTEGER REFERENCES person (id) ON DELETE CASCADE, number TEXT);
         CREATE VIEW directory AS SELECT p.id, p.name, f.number FROM person p JOIN phone f ON f.person = p.id;
         CREATE TRIGGER dir_in INSTEAD OF INSERT ON directory BEGIN
             INSERT INTO person VALUES (NEW.id, NEW.name);
             INSERT INTO phone VALUES (NEW.id, NEW.number);
         END;
         CREATE TRIGGER dir_up INSTEAD OF UPDATE ON directory BEGIN
             UPDATE person SET name = NEW.name WHERE id = OLD.id;
             UPDATE phone SET number = NEW.number WHERE person = OLD.id AND number = OLD.number;
         END;
         CREATE TRIGGER dir_del INSTEAD OF DELETE ON directory BEGIN
             DELETE FROM person WHERE id = OLD.id;
         END",
    )
    .unwrap();
    // The view joins two tables, so without triggers it cannot be changed.
    assert!(db.execute("CREATE VIEW plain AS SELECT p.id FROM person p JOIN phone f ON f.person = p.id; INSERT INTO plain VALUES (1)").is_err());
    let r = db.execute("INSERT INTO directory VALUES (1, 'Ada', '555-1'), (2, 'Grace', '555-2')").unwrap();
    assert_eq!(r.affected, 2);
    assert_eq!(rows(&db, "SELECT name, number FROM directory ORDER BY id"), [["Ada", "555-1"], ["Grace", "555-2"]]);
    let r = db.execute("UPDATE directory SET number = '555-9', name = name & '!' WHERE id = 2").unwrap();
    assert_eq!(r.affected, 1);
    assert_eq!(rows(&db, "SELECT name, number FROM directory ORDER BY id"), [["Ada", "555-1"], ["Grace!", "555-9"]]);
    db.execute("DELETE FROM directory WHERE name LIKE 'A*'").unwrap();
    assert_eq!(rows(&db, "SELECT name FROM directory"), [["Grace!"]]);
    // A failing trigger undoes the whole statement.
    db.execute("CREATE TRIGGER dir_bad INSTEAD OF INSERT ON directory WHEN NEW.id > 100 BEGIN RAISE 'too big'; END").unwrap();
    assert!(db.execute("INSERT INTO directory VALUES (3, 'Linus', '555-3'), (101, 'X', '1')").is_err());
    assert_eq!(rows(&db, "SELECT Count(*) FROM person"), [["1"]]);
    // Only queries can have INSTEAD OF triggers, and they go away with the query.
    assert!(db.execute("CREATE TRIGGER t1 INSTEAD OF INSERT ON person BEGIN RAISE 'x'; END").is_err());
    assert!(db.execute("CREATE TRIGGER t2 AFTER INSERT ON directory BEGIN RAISE 'x'; END").is_err());
    db.execute("DROP VIEW directory").unwrap();
    assert!(db.trigger_names().is_empty());
}

#[test]
fn json_values_and_functions() {
    let mut db = Database::in_memory();
    db.execute(
        r#"CREATE TABLE docs (id INTEGER PRIMARY KEY, body JSON);
           INSERT INTO docs VALUES (1, '{"name": "Ada", "tags": ["math", "code"], "age": 36, "address": {"city": "London"}}');
           INSERT INTO docs VALUES (2, '{"name": "Grace", "tags": [], "age": 85.5}');
           INSERT INTO docs VALUES (3, NULL)"#,
    )
    .unwrap();
    // The text is checked and stored in a canonical form.
    assert!(db.execute("INSERT INTO docs VALUES (4, '{broken')").is_err());
    assert_eq!(rows(&db, "SELECT body FROM docs WHERE id = 2"), [[r#"{"age":85.5,"name":"Grace","tags":[]}"#]]);
    assert_eq!(rows(&db, "SELECT JSON_VALID('[1,2]'), JSON_VALID('[1,'), JSON_VALID(body) FROM docs WHERE id = 1"), [["Yes", "No", "Yes"]]);

    // Paths and operators.
    assert_eq!(
        rows(&db, "SELECT JSON_VALUE(body, '$.name'), JSON_VALUE(body, '$.age'), JSON_VALUE(body, '$.address.city'), JSON_VALUE(body, '$.tags[1]') FROM docs WHERE id = 1"),
        [["Ada", "36", "London", "code"]]
    );
    assert_eq!(rows(&db, "SELECT JSON_EXTRACT(body, '$.tags'), JSON_EXTRACT(body, '$.nope') FROM docs WHERE id = 1"), [[r#"["math","code"]"#, ""]]);
    assert_eq!(rows(&db, "SELECT body -> 'name', body ->> 'name', body -> 'tags' -> 0, body -> 'tags' ->> 1 FROM docs WHERE id = 1"), [["\"Ada\"", "Ada", "\"math\"", "code"]]);
    assert_eq!(rows(&db, "SELECT id FROM docs WHERE body ->> 'name' = 'Grace'"), [["2"]]);
    // Numbers come back as numbers: arithmetic and comparison work.
    assert_eq!(rows(&db, "SELECT id FROM docs WHERE JSON_VALUE(body, '$.age') > 40"), [["2"]]);
    assert_eq!(rows(&db, "SELECT JSON_VALUE(body, '$.age') + 1 FROM docs WHERE id = 2"), [["86.5"]]);
    assert_eq!(rows(&db, "SELECT JSON_LENGTH(body, '$.tags'), JSON_TYPE(body, '$.age'), JSON_TYPE(body), JSON_KEYS(body) FROM docs WHERE id = 1"), [["2", "number", "object", r#"["address","age","name","tags"]"#]]);

    // Building and changing documents.
    assert_eq!(rows(&db, "SELECT JSON_OBJECT('a', 1, 'b', JSON_ARRAY(1, 'x', NULL, TRUE))"), [[r#"{"a":1,"b":[1,"x",null,true]}"#]]);
    assert_eq!(rows(&db, r#"SELECT JSON_SET('{"a":1}', '$.b', 2), JSON_SET('{"a":1}', '$.a', 'x'), JSON_REMOVE('{"a":1,"b":2}', '$.a')"#), [[r#"{"a":1,"b":2}"#, r#"{"a":"x"}"#, r#"{"b":2}"#]]);
    assert_eq!(rows(&db, r#"SELECT JSON_MERGE_PATCH('{"a":1,"b":{"c":2}}', '{"b":{"c":null,"d":3}}')"#), [[r#"{"a":1,"b":{"d":3}}"#]]);
    assert_eq!(rows(&db, r#"SELECT JSON_CONTAINS('{"a":[1,2,3]}', '{"a":[2]}'), JSON_CONTAINS('{"a":1}', '{"a":2}')"#), [["Yes", "No"]]);
    db.execute(r#"UPDATE docs SET body = JSON_SET(body, '$.age', 37) WHERE id = 1"#).unwrap();
    assert_eq!(rows(&db, "SELECT JSON_VALUE(body, '$.age') FROM docs WHERE id = 1"), [["37"]]);
    // NULL documents give NULL.
    assert_eq!(rows(&db, "SELECT JSON_VALUE(body, '$.name') FROM docs WHERE id = 3"), [[""]]);

    // Equal documents compare equal however they were written; CAST works both ways.
    assert_eq!(rows(&db, r#"SELECT CAST('{"b":1, "a":2}' AS JSON) = CAST('{"a":2,"b":1}' AS JSON), CAST(CAST('[1]' AS JSON) AS TEXT)"#), [["Yes", "[1]"]]);
    assert_eq!(rows(&db, "SELECT TO_JSON('hi'), TO_JSON(5), TO_JSON(TRUE)"), [["\"hi\"", "5", "true"]]);

    // Rows from a document, and a document from rows.
    assert_eq!(rows(&db, "SELECT key, value FROM JSON_EACH('{\"x\": 1, \"y\": [2]}') ORDER BY key"), [["x", "1"], ["y", "[2]"]]);
    assert_eq!(rows(&db, "SELECT value FROM JSON_EACH('[10, 20, 30]') WHERE key >= 1 ORDER BY key"), [["20"], ["30"]]);
    assert_eq!(rows(&db, "SELECT JSON_ARRAYAGG(id) FROM docs WHERE id < 3"), [["[1,2]"]]);
    assert!(db.query("SELECT JSON_VALUE('[1]', 'bad path')").is_err());
}

#[test]
fn json_results_are_the_same_with_the_optimizer_off() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE d (id INTEGER PRIMARY KEY, body JSON); CREATE INDEX byid ON d (id)").unwrap();
    for i in 0..50 {
        db.execute(&format!(r#"INSERT INTO d VALUES ({i}, '{{"n": {i}, "even": {}}}')"#, i % 2 == 0)).unwrap();
    }
    let sql = "SELECT id FROM d WHERE JSON_VALUE(body, '$.even') = TRUE AND id > 40 ORDER BY id";
    let on = rows(&db, sql);
    db.set_optimizer(false);
    assert_eq!(on, rows(&db, sql));
    assert_eq!(on.len(), 4);
}

#[test]
fn a_calculated_column_over_json_gives_the_same_answers_with_or_without_an_index() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE d (id INTEGER PRIMARY KEY, body JSON, status CALCULATED AS (JSON_VALUE(body, '$.status')))").unwrap();
    db.execute("CREATE INDEX bystatus ON d (status)").unwrap();
    for i in 0..200 {
        db.execute(&format!(r#"INSERT INTO d (id, body) VALUES ({i}, '{{"status": "{}"}}')"#, if i % 50 == 0 { "open" } else { "closed" })).unwrap();
    }
    assert_eq!(rows(&db, "SELECT Count(*) FROM d WHERE status = 'open'"), [["4"]]);
    // A calculated column has no fixed type, so an index probe could miss values that compare equal
    // (text '5' and the number 5); the optimizer leaves it to the scan.
    let plan = plan_text(&db, "SELECT id FROM d WHERE status = 'open'");
    assert!(plan.contains("full scan"), "{plan}");
}

#[test]
fn correlated_subqueries_in_grouped_queries() {
    let mut db = db();
    // The select list: a sub-query that reads the grouping column runs once per group.
    let sql = "SELECT O.CustID, (SELECT C.Name FROM Customers C WHERE C.ID = O.CustID) AS Who, SUM(O.Total) AS T \
               FROM Orders O GROUP BY O.CustID ORDER BY O.CustID";
    let expect = [["1", "Ada", "$30.50"], ["2", "Grace", "$12.25"], ["3", "Linus", "$100.00"]];
    for on in [true, false] {
        db.set_optimizer(on);
        assert_eq!(rows(&db, sql), expect, "optimizer {on}");
    }
    db.set_optimizer(true);
    let plan = plan_text(&db, sql);
    assert!(plan.contains("correlated sub-query: semi-join") && plan.contains("3 lookup(s) for 3 row(s)"), "{plan}");

    // HAVING, with EXISTS, IN and a scalar comparison.
    assert_eq!(
        rows(&db, "SELECT O.CustID FROM Orders O GROUP BY O.CustID HAVING EXISTS (SELECT 1 FROM Customers C WHERE C.ID = O.CustID AND C.City = 'London')"),
        [["1"]]
    );
    assert_eq!(
        rows(&db, "SELECT O.CustID FROM Orders O GROUP BY O.CustID HAVING O.CustID IN (SELECT C.ID FROM Customers C WHERE C.City LIKE '*s*') ORDER BY 1"),
        [["3"]]
    );
    assert_eq!(
        rows(&db, "SELECT O.CustID, COUNT(*) FROM Orders O GROUP BY O.CustID HAVING SUM(O.Total) > (SELECT C.Credit + 10 FROM Customers C WHERE C.ID = O.CustID) ORDER BY 1"),
        [["1", "2"], ["2", "2"], ["3", "1"]]
    );
    // ORDER BY and GROUP BY themselves.
    assert_eq!(
        rows(&db, "SELECT O.CustID FROM Orders O GROUP BY O.CustID ORDER BY (SELECT C.Name FROM Customers C WHERE C.ID = O.CustID) DESC"),
        [["3"], ["2"], ["1"]]
    );
    assert_eq!(
        rows(&db, "SELECT (SELECT C.City FROM Customers C WHERE C.ID = O.CustID) AS City, COUNT(*) FROM Orders O GROUP BY (SELECT C.City FROM Customers C WHERE C.ID = O.CustID) ORDER BY 1"),
        [["Helsinki", "1"], ["London", "2"], ["NYC", "2"]]
    );
    // Together with a window function.
    assert_eq!(
        rows(&db, "SELECT (SELECT C.Name FROM Customers C WHERE C.ID = O.CustID), RANK() OVER (ORDER BY SUM(O.Total) DESC) FROM Orders O GROUP BY O.CustID ORDER BY 2"),
        [["Linus", "1"], ["Ada", "2"], ["Grace", "3"]]
    );
    // Only grouping columns can be read.
    let err = db.query("SELECT O.CustID, (SELECT C.Name FROM Customers C WHERE C.Credit = O.Total) FROM Orders O GROUP BY O.CustID").unwrap_err();
    assert!(err.to_string().contains("GROUP BY"), "{err}");
}

fn big_customers() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Cust (ID INTEGER PRIMARY KEY, Name TEXT, City TEXT, Code TEXT);
         CREATE TABLE Ord (OID INTEGER PRIMARY KEY, CustID INTEGER, Total INTEGER);",
    )
    .unwrap();
    let mut script = String::new();
    for i in 0..300 {
        script += &format!("INSERT INTO Cust VALUES ({i}, 'c{i}', '{}', '{}');\n", ["Oslo", "Rome", "Kyiv"][i % 3], i);
    }
    // Customers 0..199 have orders (3 each), the rest have none; some orders have no customer.
    for o in 0..600 {
        script += &format!("INSERT INTO Ord VALUES ({o}, {}, {});\n", if o % 20 == 0 { "NULL".to_string() } else { (o % 200).to_string() }, o % 50);
    }
    db.execute(&script).unwrap();
    db
}

#[test]
fn correlated_subqueries_become_semi_joins() {
    let mut db = big_customers();
    let queries = [
        "SELECT ID FROM Cust c WHERE EXISTS (SELECT 1 FROM Ord o WHERE o.CustID = c.ID AND o.Total > 40) ORDER BY ID",
        "SELECT ID FROM Cust c WHERE NOT EXISTS (SELECT 1 FROM Ord o WHERE o.CustID = c.ID) ORDER BY ID",
        "SELECT ID FROM Cust c WHERE c.Name IN (SELECT 'c' & o.Total FROM Ord o WHERE o.CustID = c.ID) ORDER BY ID",
        "SELECT ID, (SELECT COUNT(*) FROM Ord o WHERE o.CustID = c.ID) FROM Cust c ORDER BY ID",
        "SELECT ID, (SELECT MAX(o.Total) FROM Ord o WHERE o.CustID = c.ID) FROM Cust c ORDER BY ID",
        "SELECT ID, (SELECT SUM(o.Total) FROM Ord o WHERE o.CustID = c.ID AND o.Total > 10) FROM Cust c ORDER BY ID",
        "SELECT OID, (SELECT c.Name FROM Cust c WHERE c.ID = o.CustID) FROM Ord o ORDER BY OID",
        "SELECT ID FROM Cust c WHERE c.ID = (SELECT MIN(o.CustID) FROM Ord o WHERE o.Total = c.ID) ORDER BY ID",
        // Not purely equalities, so these keep running per key.
        "SELECT ID FROM Cust c WHERE EXISTS (SELECT 1 FROM Ord o WHERE o.CustID = c.ID AND o.Total > c.ID % 50) ORDER BY ID",
        // Keys of different kinds (a number against text) compare by coercion.
        "SELECT ID FROM Cust c WHERE EXISTS (SELECT 1 FROM Ord o WHERE o.CustID = c.Code) ORDER BY ID",
        "SELECT ID FROM Cust c WHERE EXISTS (SELECT 1 FROM Ord o WHERE o.CustID = c.ID AND o.Total = c.ID % 50) ORDER BY ID",
    ];
    for sql in queries {
        db.set_optimizer(true);
        let on = rows(&db, sql);
        db.set_optimizer(false);
        let off = rows(&db, sql);
        assert_eq!(on, off, "{sql}");
    }
    db.set_optimizer(true);
    // The plain ones are hashed, and the answers are the ones expected.
    let plan = plan_text(&db, queries[0]);
    assert!(plan.contains("semi-join") && plan.contains(", 0 separate run(s)"), "{plan}");
    assert_eq!(rows(&db, "SELECT Count(*) FROM Cust c WHERE EXISTS (SELECT 1 FROM Ord o WHERE o.CustID = c.ID)"), [["190"]]);
    assert_eq!(rows(&db, "SELECT Count(*) FROM Cust c WHERE NOT EXISTS (SELECT 1 FROM Ord o WHERE o.CustID = c.ID)"), [["110"]]);
    // A count of nothing is 0, and the other aggregates give NULL.
    assert_eq!(rows(&db, "SELECT (SELECT COUNT(*) FROM Ord o WHERE o.CustID = c.ID), (SELECT MAX(o.Total) FROM Ord o WHERE o.CustID = c.ID) FROM Cust c WHERE ID = 250"), [["0", ""]]);
    // A scalar sub-query that finds two rows is still an error, where it is looked at.
    assert!(db.query("SELECT ID, (SELECT o.Total FROM Ord o WHERE o.CustID = c.ID) FROM Cust c").is_err());
    // Mixed kinds fall back to running the query, not to wrong answers.
    let plan = plan_text(&db, queries[9]);
    assert!(plan.contains("0 lookup(s)") && plan.contains("300 separate run(s)"), "{plan}");
    // With few outer rows and an index, running the query for each is cheaper and is kept.
    db.execute("CREATE INDEX byc ON Ord (CustID)").unwrap();
    let plan = plan_text(&db, "SELECT ID FROM Cust c WHERE ID < 5 AND EXISTS (SELECT 1 FROM Ord o WHERE o.CustID = c.ID)");
    assert!(plan.contains("ran 5 time(s)"), "{plan}");
}

#[test]
fn simple_derived_tables_are_read_as_their_table() {
    let mut db = big_customers();
    let queries = [
        "SELECT ID, Name FROM (SELECT ID, Name FROM Cust WHERE City = 'Rome') d WHERE d.ID > 250 ORDER BY ID",
        "SELECT * FROM (SELECT ID, City FROM Cust WHERE ID < 6) AS d ORDER BY ID",
        "SELECT d.* FROM (SELECT ID, Name FROM Cust) AS d WHERE d.ID = 7",
        "SELECT k, n FROM (SELECT ID, Name FROM Cust) AS d (k, n) WHERE k BETWEEN 3 AND 5 ORDER BY k",
        "SELECT d.Name, o.Total FROM (SELECT ID, Name FROM Cust WHERE ID < 5) d JOIN Ord o ON o.CustID = d.ID ORDER BY d.Name, o.Total",
        "SELECT c.ID, d.Total FROM Cust c LEFT JOIN (SELECT CustID, Total FROM Ord WHERE Total > 45) d ON d.CustID = c.ID WHERE c.ID < 10 ORDER BY c.ID, d.Total",
        "SELECT Count(*) FROM (SELECT ID FROM Cust WHERE Name LIKE 'c1*') d",
        // Not simple: computed, reordered, limited or grouped columns are computed first.
        "SELECT x FROM (SELECT ID + 1 AS x FROM Cust WHERE ID < 4) d ORDER BY x",
        "SELECT Name, ID FROM (SELECT Name, ID FROM Cust WHERE ID < 4) d ORDER BY ID",
        "SELECT * FROM (SELECT ID, Name FROM Cust ORDER BY ID DESC LIMIT 3) d ORDER BY ID",
    ];
    for sql in queries {
        db.set_optimizer(true);
        let on = rows(&db, sql);
        db.set_optimizer(false);
        let off = rows(&db, sql);
        assert_eq!(on, off, "{sql}");
    }
    db.set_optimizer(true);
    // A condition on the outside reaches the table's index through the derived table.
    let plan = plan_text(&db, "SELECT Name FROM (SELECT ID, Name FROM Cust WHERE City = 'Rome') d WHERE d.ID = 7");
    assert!(plan.contains("index lookup") || plan.contains("PrimaryKey"), "{plan}");
    // Columns the derived table does not show cannot be used.
    assert!(db.query("SELECT d.City FROM (SELECT ID FROM Cust) d").is_err());
    assert!(db.query("SELECT City FROM (SELECT ID FROM Cust) d").is_err());
    assert_eq!(rows(&db, "SELECT * FROM (SELECT ID FROM Cust WHERE ID = 3) d").len(), 1);
    // Views that show only some columns are merged too.
    db.execute("CREATE VIEW romans AS SELECT ID, Name FROM Cust WHERE City = 'Rome'").unwrap();
    let plan = plan_text(&db, "SELECT Name FROM romans WHERE ID = 7");
    assert!(plan.contains("index lookup") || plan.contains("PrimaryKey"), "{plan}");
    assert_eq!(rows(&db, "SELECT * FROM romans WHERE ID = 7"), [["7", "c7"]]);
}

/// The first "est N" in the plan of `sql`.
fn estimate_of(db: &Database, sql: &str) -> i64 {
    let plan = plan_text(db, sql);
    let at = plan.find("est ").unwrap_or_else(|| panic!("no estimate in\n{plan}")) + 4;
    plan[at..].split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap()
}

#[test]
fn statistics_sharpen_the_estimates_for_unindexed_columns() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, status TEXT, v INTEGER, note TEXT)").unwrap();
    let mut script = String::new();
    for i in 0..2000 {
        let status = if i < 1900 { "open" } else if i < 1990 { "closed" } else { "rare" };
        let note = if i % 4 == 0 { "NULL".to_string() } else { format!("'n{i}'") };
        script += &format!("INSERT INTO t VALUES ({i}, '{status}', {i}, {note});\n");
    }
    db.execute(&script).unwrap();
    let near = |got: i64, want: i64| (got - want).abs() <= want / 5 + 3;
    // Skew: 95% of the rows are open, which a fixed one in ten would badly underestimate.
    let open = estimate_of(&db, "SELECT * FROM t WHERE status = 'open'");
    assert!(near(open, 1900), "open {open}");
    let closed = estimate_of(&db, "SELECT * FROM t WHERE status = 'closed'");
    assert!(near(closed, 90), "closed {closed}");
    let rare = estimate_of(&db, "SELECT * FROM t WHERE status = 'rare'");
    assert!(near(rare, 10), "rare {rare}");
    // Ranges follow the histogram, not a fixed third.
    let high = estimate_of(&db, "SELECT * FROM t WHERE v > 1800");
    assert!(near(high, 199), "high {high}");
    let band = estimate_of(&db, "SELECT * FROM t WHERE v BETWEEN 100 AND 299");
    assert!(near(band, 200), "band {band}");
    // OR, IN, NOT, NULLs and prefixes.
    let either = estimate_of(&db, "SELECT * FROM t WHERE status = 'closed' OR status = 'rare'");
    assert!(near(either, 100), "either {either}");
    let list = estimate_of(&db, "SELECT * FROM t WHERE status IN ('closed', 'rare')");
    assert!(near(list, 100), "list {list}");
    let not_open = estimate_of(&db, "SELECT * FROM t WHERE status <> 'open'");
    assert!(near(not_open, 100), "not_open {not_open}");
    let nulls = estimate_of(&db, "SELECT * FROM t WHERE note IS NULL");
    assert!(near(nulls, 500), "nulls {nulls}");
    let prefix = estimate_of(&db, "SELECT * FROM t WHERE note LIKE 'n19*'");
    assert!(prefix < 300, "prefix {prefix}");
    // The estimates change with the data.
    db.execute("DELETE FROM t WHERE status = 'open'").unwrap();
    let after = estimate_of(&db, "SELECT * FROM t WHERE status = 'closed'");
    assert!(near(after, 90), "after {after}");
    assert_eq!(rows(&db, "SELECT Count(*) FROM t WHERE status = 'closed'"), [["90"]]);
}

#[test]
fn statistics_do_not_change_answers() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE a (k INTEGER, x INTEGER); CREATE TABLE b (k INTEGER, y INTEGER); CREATE TABLE c (id INTEGER PRIMARY KEY, k INTEGER)").unwrap();
    let mut script = String::new();
    for i in 0..300 {
        script += &format!("INSERT INTO a VALUES ({}, {i}); INSERT INTO b VALUES ({}, {i}); INSERT INTO c VALUES ({i}, {});\n", i % 10, i % 10, i % 300);
    }
    db.execute(&script).unwrap();
    let sql = "SELECT Count(*) FROM a JOIN b ON a.k = b.k JOIN c ON c.id = a.x WHERE c.k < 20";
    let on = rows(&db, sql);
    db.set_optimizer(false);
    assert_eq!(on, rows(&db, sql));
    assert_eq!(on, [["600"]]);
    // The many-to-many join of a and b (9,000 rows) is left until the selective one has thinned a.
    db.set_optimizer(true);
    let plan = plan_text(&db, sql);
    assert!(plan.contains("join order: c -> a -> b"), "{plan}");
}

#[test]
fn prepared_statements_run_many_times() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, score INTEGER)").unwrap();
    let insert = db.prepare("INSERT INTO t VALUES (@id, @name, @score)").unwrap();
    assert_eq!(insert.parameters(), ["id", "name", "score"]);
    for i in 0..50 {
        db.execute_prepared(&insert, &[("id", Value::Int(i)), ("name", Value::text(&format!("n{i}"))), ("score", Value::Int(i * 2))]).unwrap();
    }
    let find = db.prepare("SELECT name, score FROM t WHERE id = @id OR score > @min ORDER BY id").unwrap();
    let r = db.query_prepared(&find, &[("id", Value::Int(3)), ("min", Value::Int(94))]).unwrap();
    assert_eq!(col(&r, 0), ["n3", "n48", "n49"]);
    let r = db.query_prepared(&find, &[("@id", Value::Int(0)), ("min", Value::Int(1000))]).unwrap();
    assert_eq!(col(&r, 0), ["n0"]);
    // The same prepared statement sees changed data.
    db.execute("UPDATE t SET name = 'zero' WHERE id = 0").unwrap();
    assert_eq!(col(&db.query_prepared(&find, &[("id", Value::Int(0)), ("min", Value::Int(1000))]).unwrap(), 0), ["zero"]);
    // A missing parameter, a script, and a statement that changes things are all handled.
    assert!(db.query_prepared(&find, &[("id", Value::Int(0))]).is_err());
    assert!(db.query_prepared(&insert, &[("id", Value::Int(0)), ("name", Value::Null), ("score", Value::Null)]).is_err());
    let script = db.prepare("DELETE FROM t WHERE id >= @cut; SELECT Count(*) FROM t").unwrap();
    let r = db.execute_prepared(&script, &[("cut", Value::Int(10))]).unwrap();
    assert_eq!(col(&r, 0), ["10"]);
    // A schema change in between is seen by the next run (nothing about the table was kept).
    db.execute("ALTER TABLE t ADD COLUMN extra TEXT DEFAULT 'x'").unwrap();
    assert_eq!(col(&db.query_prepared(&db.prepare("SELECT extra FROM t WHERE id = @i").unwrap(), &[("i", Value::Int(1))]).unwrap(), 0), ["x"]);
    // ...and so is the new column: three values no longer fit four columns.
    assert!(db.execute_prepared(&insert, &[("id", Value::Int(99)), ("name", Value::text("a")), ("score", Value::Int(1))]).is_err());
}

#[test]
fn prepare_and_exec_in_sql() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')").unwrap();
    db.execute("PREPARE by_id AS SELECT name FROM t WHERE id = @id; PREPARE add_row AS INSERT INTO t VALUES (@id, @name)").unwrap();
    assert_eq!(rows_of(&mut db, "EXEC by_id (2)"), [["b"]]);
    assert_eq!(rows_of(&mut db, "EXEC by_id (@id = 3)"), [["c"]]);
    db.execute("EXEC add_row (4, 'd')").unwrap();
    db.execute("EXEC add_row (@name = 'e', @id = 5)").unwrap();
    assert_eq!(rows_of(&mut db, "EXECUTE by_id (5)"), [["e"]]);
    assert!(db.execute("EXEC by_id (1, 2)").is_err());
    assert!(db.execute("EXEC by_id").is_err());
    assert!(db.execute("EXEC nosuch (1)").is_err());
    // A saved query of the same name cannot be shadowed, and DEALLOCATE forgets.
    db.execute("CREATE QUERY saved AS SELECT 1").unwrap();
    assert!(db.execute("PREPARE saved AS SELECT 2").is_err());
    db.execute("DEALLOCATE by_id").unwrap();
    assert!(db.execute("EXEC by_id (1)").is_err());
    assert!(db.execute("DEALLOCATE by_id").is_err());
    db.execute("DEALLOCATE ALL").unwrap();
    assert!(db.execute("EXEC add_row (6, 'f')").is_err());
}

fn rows_of(db: &mut Database, sql: &str) -> Vec<Vec<String>> {
    db.execute(sql).unwrap().rows.iter().map(|r| r.iter().map(|v| v.to_string()).collect()).collect()
}

#[test]
fn repeated_text_is_read_once_and_still_correct() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE t (n INTEGER)").unwrap();
    for i in 0..5 {
        db.execute("INSERT INTO t VALUES (1)").unwrap();
        assert_eq!(rows(&db, "SELECT Count(*) FROM t"), [[(i + 1).to_string()]]);
    }
    // The cache holds the parsed text, never answers: a schema change is still seen.
    db.execute("DROP TABLE t").unwrap();
    assert!(db.query("SELECT Count(*) FROM t").is_err());
    db.execute("CREATE TABLE t (n INTEGER)").unwrap();
    assert_eq!(rows(&db, "SELECT Count(*) FROM t"), [["0"]]);
    assert!(db.query("SELECT 1; SELECT 2").is_err());
    assert!(db.query("").is_err());
    assert!(db.query("DELETE FROM t").is_err());
}

#[test]
fn stored_functions() {
    let mut db = db();
    db.execute(
        "CREATE FUNCTION tax(@amount CURRENCY, @rate DECIMAL = 0.2) RETURNS CURRENCY AS @amount * @rate;
         CREATE FUNCTION gross(@amount CURRENCY) AS @amount + tax(@amount);
         CREATE FUNCTION spent(@id INTEGER) RETURNS CURRENCY AS (SELECT COALESCE(SUM(Total), 0) FROM Orders WHERE CustID = @id);
         CREATE FUNCTION label(@n INTEGER) AS CASE WHEN @n > 1 THEN 'many' WHEN @n = 1 THEN 'one' ELSE 'none' END",
    )
    .unwrap();
    assert_eq!(rows(&db, "SELECT tax(100), tax(100, 0.1), gross(10)"), [["$20.00", "$10.00", "$12.00"]]);
    assert_eq!(rows(&db, "SELECT Name, spent(ID), label(ID - 2) FROM Customers ORDER BY ID"), [["Ada", "$30.50", "none"], ["Grace", "$12.25", "none"], ["Linus", "$100.00", "one"], ["Zed", "$0.00", "many"]]);
    // A function reads like any other expression: in WHERE, ORDER BY, GROUP BY, aggregates.
    assert_eq!(rows(&db, "SELECT Name FROM Customers WHERE spent(ID) > 20 ORDER BY spent(ID) DESC"), [["Linus"], ["Ada"]]);
    assert_eq!(rows(&db, "SELECT Sum(tax(Total)) FROM Orders"), [["$28.55"]]);
    // The parameter's type converts the argument.
    assert_eq!(rows(&db, "SELECT label('3')"), [["many"]]);
    // With constant arguments the call is a constant, so an index can answer it.
    let plan = plan_text(&db, "SELECT Name FROM Customers WHERE ID = gross(0) + 2");
    assert!(plan.contains("index") || plan.contains("PrimaryKey"), "{plan}");
    // Errors.
    assert!(db.query("SELECT tax()").is_err());
    assert!(db.query("SELECT tax(1, 2, 3)").is_err());
    assert!(db.query("SELECT nosuch(1)").is_err());
    assert!(db.execute("CREATE FUNCTION tax(@a INTEGER) AS @a").is_err()); // exists
    assert!(db.execute("CREATE FUNCTION upper(@a TEXT) AS @a").is_err()); // built in
    assert!(db.execute("CREATE FUNCTION bad(@a INTEGER) AS @b").is_err()); // unknown parameter
    assert!(db.execute("CREATE FUNCTION bad2(@a INTEGER) AS nothing_like_it(@a)").is_err());
    assert!(db.execute("CREATE FUNCTION bad3(@a INTEGER, @a INTEGER) AS @a").is_err());
    // A function that calls itself is stopped.
    db.execute("CREATE FUNCTION loop_(@a INTEGER) AS 1").unwrap();
    db.execute("CREATE OR REPLACE FUNCTION loop_(@a INTEGER) AS loop_(@a)").unwrap_err(); // refused when it is made
    // An argument that gives a new value each time cannot be used twice.
    db.execute("CREATE SEQUENCE s; CREATE FUNCTION twice(@a INTEGER) AS @a + @a; CREATE FUNCTION once(@a INTEGER) AS @a * 10").unwrap();
    assert!(db.query("SELECT twice(NEXTVAL('s'))").is_err());
    assert_eq!(rows(&db, "SELECT once(NEXTVAL('s'))"), [["10"]]);
    // Replace and drop.
    db.execute("CREATE OR REPLACE FUNCTION once(@a INTEGER) AS @a * 100").unwrap();
    assert_eq!(rows(&db, "SELECT once(2)"), [["200"]]);
    db.execute("DROP FUNCTION once").unwrap();
    assert!(db.query("SELECT once(2)").is_err());
    db.execute("DROP FUNCTION IF EXISTS once").unwrap();
    assert!(db.execute("DROP FUNCTION once").is_err());
    assert!(db.execute("DROP PROCEDURE tax").is_err());
    // Rolled back with a transaction.
    db.execute("BEGIN; CREATE FUNCTION temp_(@a INTEGER) AS @a; ROLLBACK").unwrap();
    assert!(db.query("SELECT temp_(1)").is_err());
    assert!(db.routine_names().iter().any(|(n, f)| *n == "tax" && *f));
    // The optimizer on or off gives the same answers.
    let sql = "SELECT Name, spent(ID) FROM Customers WHERE spent(ID) > 5 ORDER BY ID";
    let on = rows(&db, sql);
    db.set_optimizer(false);
    assert_eq!(on, rows(&db, sql));
}

#[test]
fn stored_procedures() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE item (id INTEGER PRIMARY KEY, stock INTEGER);
         CREATE TABLE log (msg TEXT);
         INSERT INTO item VALUES (1, 10), (2, 95);
         CREATE PROCEDURE restock(@id INTEGER, @qty INTEGER = 10) AS BEGIN
             DECLARE @cur INTEGER = (SELECT stock FROM item WHERE id = @id);
             IF @cur IS NULL THEN
                 RAISE 'no such item';
             ELSEIF @cur + @qty > 100 THEN
                 SET @qty = 100 - @cur;
             END IF;
             UPDATE item SET stock = stock + @qty WHERE id = @id;
             INSERT INTO log VALUES ('restocked ' & @id & ' by ' & @qty);
             RETURN @qty;
         END",
    )
    .unwrap();
    assert_eq!(rows_of(&mut db, "CALL restock(1, 5)"), [["5"]]);
    assert_eq!(rows_of(&mut db, "EXEC restock(2)"), [["5"]]); // capped: 95 + 10 > 100
    assert_eq!(rows_of(&mut db, "CALL restock(@qty = 1, @id = 1)"), [["1"]]);
    assert_eq!(rows(&db, "SELECT id, stock FROM item ORDER BY id"), [["1", "16"], ["2", "100"]]);
    assert_eq!(rows(&db, "SELECT msg FROM log ORDER BY msg"), [["restocked 1 by 1"], ["restocked 1 by 5"], ["restocked 2 by 5"]]);
    // A failure undoes everything the procedure did.
    let n = rows(&db, "SELECT Count(*) FROM log");
    assert!(db.execute("CALL restock(99)").is_err());
    assert_eq!(rows(&db, "SELECT Count(*) FROM log"), n);

    // Loops, LEAVE, variables, and the result of the last SELECT.
    db.execute(
        "CREATE PROCEDURE fill(@n INTEGER) AS BEGIN
             DECLARE @i INTEGER = 0;
             WHILE TRUE DO
                 SET @i = @i + 1;
                 IF @i > @n THEN LEAVE; END IF;
                 INSERT INTO item VALUES (1000 + @i, @i * @i);
             END WHILE;
             SELECT Count(*), Sum(stock) FROM item WHERE id > 1000;
         END",
    )
    .unwrap();
    assert_eq!(rows_of(&mut db, "CALL fill(4)"), [["4", "30"]]);
    // Procedures call procedures; a procedure that calls itself is stopped.
    db.execute("CREATE PROCEDURE outer_(@n INTEGER) AS BEGIN CALL fill(@n); END").unwrap();
    assert!(db.execute("CALL outer_(2)").is_err()); // ids 1001.. already exist: refused, whole call undone
    db.execute("CREATE PROCEDURE again() AS BEGIN CALL again(); END").unwrap();
    assert!(db.execute("CALL again()").unwrap_err().to_string().contains("nested too deeply"));
    db.execute("CREATE PROCEDURE forever() AS BEGIN DECLARE @x INTEGER = 0; WHILE TRUE DO SET @x = @x + 1; END WHILE; END").unwrap();
    assert!(db.execute("CALL forever()").unwrap_err().to_string().contains("million"));
    // Rules.
    assert!(db.execute("CALL restock()").is_err());
    assert!(db.execute("CALL restock(1, 2, 3)").is_err());
    assert!(db.execute("CALL restock(@nope = 1)").is_err());
    assert!(db.execute("CREATE PROCEDURE p() AS BEGIN COMMIT; END").is_err());
    assert!(db.execute("CREATE PROCEDURE p() AS BEGIN SET @undeclared = 1; END").is_ok());
    assert!(db.execute("CALL p()").is_err());
    assert!(db.execute("CREATE PROCEDURE q() RETURNS INTEGER AS BEGIN RETURN 1; END").is_err());
    assert!(db.query("SELECT restock(1)").is_err());
    db.execute("CREATE FUNCTION f(@a INTEGER) AS @a").unwrap();
    assert!(db.execute("CALL f(1)").is_err());
    db.execute("DROP PROCEDURE restock").unwrap();
    assert!(db.execute("CALL restock(1)").is_err());
}

#[test]
fn returning_gives_back_the_changed_rows() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE t (id COUNTER PRIMARY KEY, name TEXT, qty INTEGER DEFAULT 1, twice CALCULATED AS (qty * 2));
         CREATE TABLE log (msg TEXT);",
    )
    .unwrap();
    // Generated values come back: the AutoNumber, defaults and calculated fields.
    let r = db.execute("INSERT INTO t (name) VALUES ('a'), ('b') RETURNING id, name, qty, twice").unwrap();
    assert_eq!(r.columns, ["id", "name", "qty", "twice"]);
    assert_eq!(r.rows.iter().map(|r| r.iter().map(|v| v.to_string()).collect::<Vec<_>>()).collect::<Vec<_>>(), [["1", "a", "1", "2"], ["2", "b", "1", "2"]]);
    assert_eq!(r.affected, 2);
    // Expressions, aliases and *.
    let r = db.execute("INSERT INTO t (name, qty) VALUES ('c', 5) RETURNING id * 10 AS ten, Upper(name), *").unwrap();
    assert_eq!(r.columns, ["ten", "Expr1000", "id", "name", "qty", "twice"]); // an unnamed expression, as in SELECT
    assert_eq!(r.rows[0].iter().map(|v| v.to_string()).collect::<Vec<_>>(), ["30", "C", "3", "c", "5", "10"]);
    // INSERT ... SELECT and DEFAULT VALUES.
    let r = db.execute("INSERT INTO t (name, qty) SELECT name & '2', qty FROM t WHERE id < 3 RETURNING id").unwrap();
    assert_eq!(r.rows.len(), 2);
    assert_eq!(db.execute("INSERT INTO t DEFAULT VALUES RETURNING id, qty").unwrap().rows.len(), 1);

    // UPDATE returns the rows as they are now; DELETE the rows as they were.
    let r = db.execute("UPDATE t SET qty = qty + 100 WHERE id <= 2 RETURNING id, qty, twice").unwrap();
    assert_eq!(r.rows.iter().map(|r| r.iter().map(|v| v.to_string()).collect::<Vec<_>>()).collect::<Vec<_>>(), [["1", "101", "202"], ["2", "101", "202"]]);
    let r = db.execute("DELETE FROM t WHERE id >= 3 RETURNING id, name").unwrap();
    assert_eq!(r.rows.len(), 4);
    assert_eq!(r.rows[0][1].to_string(), "c");
    assert_eq!(rows(&db, "SELECT Count(*) FROM t"), [["2"]]);
    // Nothing matched: nothing returned.
    assert!(db.execute("DELETE FROM t WHERE id = 999 RETURNING id").unwrap().rows.is_empty());

    // What a trigger did to the row is what comes back.
    db.execute("CREATE TRIGGER bump AFTER INSERT ON t BEGIN UPDATE t SET qty = qty + 1000 WHERE id = NEW.id; END").unwrap();
    let r = db.execute("INSERT INTO t (name) VALUES ('d') RETURNING qty").unwrap();
    assert_eq!(r.rows[0][0].to_string(), "1001");

    // MERGE.
    db.execute("CREATE TABLE src (id INTEGER, name TEXT)").unwrap();
    db.execute("INSERT INTO src VALUES (1, 'one'), (77, 'new')").unwrap();
    let r = db
        .execute("MERGE INTO t USING src ON t.id = src.id WHEN MATCHED THEN UPDATE SET name = src.name WHEN NOT MATCHED THEN INSERT (name) VALUES (src.name) RETURNING id, name")
        .unwrap();
    let mut got: Vec<String> = r.rows.iter().map(|r| r[1].to_string()).collect();
    got.sort();
    assert_eq!(got, ["new", "one"]);

    // Checked before anything changes; only tables; with parameters; in a prepared statement.
    let before = rows(&db, "SELECT Count(*) FROM t");
    assert!(db.execute("INSERT INTO t (name) VALUES ('x') RETURNING nosuch").is_err());
    assert!(db.execute("INSERT INTO t (name) VALUES ('x') RETURNING Count(*)").is_err());
    assert_eq!(rows(&db, "SELECT Count(*) FROM t"), before);
    db.execute("CREATE VIEW v AS SELECT id, name FROM t").unwrap();
    assert!(db.execute("INSERT INTO v (name) VALUES ('x') RETURNING id").is_err());
    let r = db.execute_with_params("INSERT INTO log VALUES (@m) RETURNING msg", &[("m", Value::text("hello"))]).unwrap();
    assert_eq!(r.rows[0][0].to_string(), "hello");
    let p = db.prepare("DELETE FROM log WHERE msg = @m RETURNING msg").unwrap();
    assert_eq!(db.execute_prepared(&p, &[("m", Value::text("hello"))]).unwrap().rows.len(), 1);
    // Failure leaves nothing.
    assert!(db.execute("INSERT INTO t (id, name) VALUES (1, 'dup') RETURNING id").is_err());
}

fn sales_db() -> Database {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Sales (Region TEXT, City TEXT, Rep TEXT, Amount CURRENCY, Units INTEGER);
         INSERT INTO Sales VALUES
            ('North', 'Oslo', 'Ada', 1200.50, 3), ('North', 'Oslo', 'Bo', 300, 1), ('North', 'Bergen', 'Cy', 99.99, 2),
            ('South', 'Rome', 'Di', 4000, 10), ('South', 'Rome', 'Ed', 250.25, 5), ('South', 'Milan', 'Flo', 75, 1);
         CREATE REPORT by_region
            TITLE 'Sales by region' SUBTITLE 'All figures in dollars'
            BREAK BY Region HEADER 'Region {Region}' FOOTER 'Total {Region} ({count} sales)'
            BREAK BY City
            COLUMN Rep HEADING 'Representative'
            COLUMN Amount FORMAT '#,##0.00' SUM
            COLUMN Units SUM
            GRAND TOTAL 'All regions'
         AS SELECT Region, City, Rep, Amount, Units FROM Sales",
    )
    .unwrap();
    db
}

fn report(db: &Database, fmt: accessor_core::ReportFormat) -> String {
    String::from_utf8(db.render_report("by_region", fmt).unwrap()).unwrap()
}

#[test]
fn reports_group_total_and_render() {
    let db = sales_db();
    let text = report(&db, accessor_core::ReportFormat::Text);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "Sales by region");
    assert_eq!(lines[1], "All figures in dollars");
    assert!(text.contains("Representative"), "{text}");
    // Groups, in order, with their subtotals and the grand total.
    let pos = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("{needle} missing in\n{text}"));
    assert!(pos("Region North") < pos("City: Bergen"));
    assert!(pos("City: Bergen") < pos("City: Oslo"));
    assert!(pos("Total North (3 sales)") < pos("Region South"));
    assert!(pos("Total South (3 sales)") < pos("All regions"));
    // Totals are exact and use the column's pattern: 1,200.50 + 300 + 99.99 = 1,600.49 for the north.
    assert!(text.contains("1,600.49"), "{text}");
    assert!(text.contains("4,325.25"), "{text}"); // the south
    assert!(text.contains("5,925.74"), "{text}"); // everything
    assert!(text.contains("Page 1 of 1"));

    let html = report(&db, accessor_core::ReportFormat::Html);
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("<tr class=\"group g0\"><td colspan=\"3\">Region North</td></tr>"), "{html}");
    assert!(html.contains("<td class=\"r\">1,600.49</td>"), "{html}");
    assert!(html.contains("tr.grand"));
    let md = report(&db, accessor_core::ReportFormat::Markdown);
    assert!(md.starts_with("# Sales by region\n"));
    assert!(md.contains("| **Region South** |"), "{md}");
    let csv = report(&db, accessor_core::ReportFormat::Csv);
    assert_eq!(csv.lines().next().unwrap(), "Representative,Amount,Units");
    assert!(csv.contains("Ada,\"1,200.50\",3"), "{csv}");
    assert!(csv.lines().last().unwrap().starts_with("All regions"), "{csv}");
}

#[test]
fn reports_in_sql_and_as_documents() {
    let mut db = sales_db();
    let r = db.execute("RUN REPORT by_region AS markdown").unwrap();
    assert_eq!(r.columns, ["report"]);
    assert!(r.rows[0][0].to_string().starts_with("# Sales by region"));
    assert!(db.execute("RUN REPORT by_region AS pdf").is_err());
    assert!(db.execute("RUN REPORT by_region AS docx").is_err());
    assert!(db.execute("RUN REPORT nosuch").is_err());
    let pdf = db.render_report("by_region", accessor_core::ReportFormat::Pdf).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4"));
    assert!(String::from_utf8_lossy(&pdf).contains("Sales by region"));
    // Definition rules: the query must run and the layout must name its columns.
    assert!(db.execute("CREATE REPORT bad BREAK BY Nope AS SELECT Region FROM Sales").is_err());
    assert!(db.execute("CREATE REPORT bad COLUMN Nope SUM AS SELECT Region FROM Sales").is_err());
    assert!(db.execute("CREATE REPORT bad BREAK BY Region HEADER '{Nope}' AS SELECT Region FROM Sales").is_err());
    assert!(db.execute("CREATE REPORT bad AS SELECT Nope FROM Sales").is_err());
    assert!(db.execute("CREATE REPORT by_region AS SELECT 1").is_err());
    db.execute("CREATE OR REPLACE REPORT by_region TITLE 'Again' AS SELECT Region, Amount FROM Sales").unwrap();
    assert!(String::from_utf8(db.render_report("by_region", accessor_core::ReportFormat::Text).unwrap()).unwrap().starts_with("Again"));
    assert_eq!(db.report_names(), ["by_region"]);
    db.execute("DROP REPORT by_region").unwrap();
    assert!(db.render_report("by_region", accessor_core::ReportFormat::Text).is_err());
    db.execute("DROP REPORT IF EXISTS by_region").unwrap();
}

#[test]
fn long_reports_are_paged_and_new_page_breaks_work() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE r (grp INTEGER, n INTEGER)").unwrap();
    let mut script = String::new();
    for i in 0..100 {
        script += &format!("INSERT INTO r VALUES ({}, {i});\n", i / 50);
    }
    db.execute(&script).unwrap();
    db.execute("CREATE REPORT paged TITLE 'Long' BREAK BY grp NEW PAGE COLUMN n SUM PAGE SIZE 30 LINES AS SELECT grp, n FROM r").unwrap();
    let text = String::from_utf8(db.render_report("paged", accessor_core::ReportFormat::Text).unwrap()).unwrap();
    let pages: Vec<&str> = text.split("\n\u{c}\n").collect();
    assert!(pages.len() >= 4, "{} pages", pages.len());
    for (i, p) in pages.iter().enumerate() {
        assert!(p.lines().count() <= 30, "page {} has {} lines", i + 1, p.lines().count());
        assert!(p.starts_with("Long"), "every page has the title");
        assert!(p.contains(&format!("Page {} of {}", i + 1, pages.len())));
    }
    // The second group starts on a page of its own.
    let second = pages.iter().position(|p| p.contains("grp: 1")).unwrap();
    assert!(!pages[second].contains("grp: 0"));
    assert!(text.contains("1225") && text.contains("3725"), "sums of 0..49 and 50..99"); // 49*50/2, 99*100/2-1225
}

#[test]
fn regex_string_and_math_conveniences() {
    let db = Database::in_memory();
    let one = |sql: &str| rows(&db, sql)[0][0].clone();
    // Regular expressions ignore case unless told otherwise, like LIKE.
    assert_eq!(rows(&db, "SELECT REGEXP_LIKE('Hello World', 'WORLD$'), REGEXP_LIKE('Hello', 'h', 'c'), REGEXP_LIKE('ab12', '^[a-z]+[0-9]+$')"), [["Yes", "No", "Yes"]]);
    assert_eq!(one(r#"SELECT REGEXP_REPLACE('2024-03-05', '(\d+)-(\d+)-(\d+)', '\3/\2/\1')"#), "05/03/2024");
    assert_eq!(one("SELECT REGEXP_REPLACE('a1b22c333', '[0-9]+', '#')"), "a#b#c#");
    assert_eq!(one("SELECT REGEXP_SUBSTR('order 4711 shipped', '[0-9]+')"), "4711");
    assert_eq!(one(r#"SELECT REGEXP_SUBSTR('key=value', '(\w+)=(\w+)', 2)"#), "value");
    assert_eq!(one("SELECT REGEXP_SUBSTR('abc', '[0-9]+')"), "");
    assert_eq!(one("SELECT REGEXP_COUNT('banana', 'an')"), "2");
    assert!(db.query("SELECT REGEXP_LIKE('x', '(')").is_err());
    assert!(db.query("SELECT REGEXP_LIKE('x', 'x', 'q')").is_err());
    // SIMILAR TO matches the whole text.
    assert_eq!(rows(&db, "SELECT 'abc' SIMILAR TO 'a%', 'abc' SIMILAR TO 'b%', 'abc' NOT SIMILAR TO 'a_c', 'xyz' SIMILAR TO '(x|y)+z', 'a.c' SIMILAR TO 'a.c', 'abc' SIMILAR TO 'a.c'"), [["Yes", "No", "No", "Yes", "Yes", "No"]]);
    // Padding and friends.
    assert_eq!(rows(&db, "SELECT LPAD('7', 3, '0'), RPAD('ab', 5, '-'), LPAD('abcdef', 3), REPEAT('ab', 3), REVERSE('abc'), INITCAP('hELLO wORLD')"), [["007", "ab---", "abc", "ababab", "cba", "Hello World"]]);
    assert_eq!(rows(&db, "SELECT OVERLAY('Txxxxas' PLACING 'hom' FROM 2 FOR 4), OVERLAY('abc' PLACING 'ZZ' FROM 2), OVERLAY('abcdef', 'XY', 3, 1), TRANSLATE('12345', '143', 'ax')"), [["Thomas", "aZZ", "abXYdef", "a2x5"]]);
    assert_eq!(one("SELECT LPAD('x', 3, '')"), "x");
    assert!(db.query("SELECT REPEAT('x', 1000000000)").is_err());
    // Math.
    assert_eq!(rows(&db, "SELECT Round(PI(), 5), Round(DEGREES(PI()), 6), Round(RADIANS(180), 5), Round(ATAN2(1, 1), 5)"), [["3.14159", "180", "3.14159", "0.7854"]]);
    assert_eq!(rows(&db, "SELECT TRUNC(-123.4567, 2), TRUNC(123.999), TRUNC(1234, -2), TRUNC(CAST(2.789 AS DOUBLE), 1), TRUNC(CAST(9.99 AS CURRENCY), 1)"), [["-123.45", "123", "1200", "2.7", "$9.90"]]);
    let r = rows(&db, "SELECT RANDOM() FROM GENERATE_SERIES(1, 200)");
    let vals: Vec<f64> = r.iter().map(|x| x[0].parse().unwrap()).collect();
    assert!(vals.iter().all(|v| (0.0..1.0).contains(v)));
    assert!(vals.windows(2).any(|w| w[0] != w[1]), "a new value each row");
    // RANDOM is evaluated for every row, never folded to a constant, with or without the optimizer.
    let mut db2 = Database::in_memory();
    for on in [true, false] {
        db2.set_optimizer(on);
        let r = rows(&db2, "SELECT Count(*) FROM (SELECT RANDOM() AS r FROM GENERATE_SERIES(1, 50)) t WHERE r < 0.5 OR r >= 0.5");
        assert_eq!(r, [["50"]]);
    }
}

#[test]
fn upserts_deal_with_conflicting_rows() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE kv (k TEXT PRIMARY KEY, v INTEGER, hits INTEGER DEFAULT 1, tag TEXT);
         CREATE UNIQUE INDEX kv_tag ON kv (tag);",
    )
    .unwrap();
    let r = db.execute("INSERT INTO kv (k, v) VALUES ('a', 1), ('b', 2) ON CONFLICT (k) DO NOTHING").unwrap();
    assert_eq!(r.affected, 2);
    // Existing keys are left alone; new ones go in.
    let r = db.execute("INSERT INTO kv (k, v) VALUES ('a', 100), ('c', 3) ON CONFLICT (k) DO NOTHING").unwrap();
    assert_eq!(r.affected, 1);
    assert_eq!(rows(&db, "SELECT k, v FROM kv ORDER BY k"), [["a", "1"], ["b", "2"], ["c", "3"]]);
    // DO UPDATE reads the existing row and EXCLUDED, the offered one.
    db.execute("INSERT INTO kv (k, v) VALUES ('a', 10), ('d', 4) ON CONFLICT (k) DO UPDATE SET v = v + EXCLUDED.v, hits = hits + 1").unwrap();
    assert_eq!(rows(&db, "SELECT k, v, hits FROM kv ORDER BY k"), [["a", "11", "2"], ["b", "2", "1"], ["c", "3", "1"], ["d", "4", "1"]]);
    // The same key twice in one statement meets itself.
    db.execute("INSERT INTO kv (k, v) VALUES ('z', 1), ('z', 1), ('z', 1) ON CONFLICT (k) DO UPDATE SET v = v + EXCLUDED.v").unwrap();
    assert_eq!(rows(&db, "SELECT v FROM kv WHERE k = 'z'"), [["3"]]);
    // WHERE limits which conflicts update; the others are left as they are.
    let r = db.execute("INSERT INTO kv (k, v) VALUES ('a', 5), ('b', 50) ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v WHERE EXCLUDED.v > v").unwrap();
    assert_eq!(r.affected, 1);
    assert_eq!(rows(&db, "SELECT k, v FROM kv WHERE k IN ('a', 'b') ORDER BY k"), [["a", "11"], ["b", "50"]]);
    // A conflict on any unique key counts when no target is named.
    db.execute("UPDATE kv SET tag = 'red' WHERE k = 'a'").unwrap();
    let r = db.execute("INSERT INTO kv (k, v, tag) VALUES ('new', 0, 'red') ON CONFLICT DO NOTHING").unwrap();
    assert_eq!(r.affected, 0);
    // ...but NULL never conflicts.
    db.execute("INSERT INTO kv (k, v, tag) VALUES ('n1', 0, NULL), ('n2', 0, NULL) ON CONFLICT DO NOTHING").unwrap();
    assert_eq!(rows(&db, "SELECT Count(*) FROM kv WHERE tag IS NULL AND k LIKE 'n*'"), [["2"]]);
    // The target must be a unique key.
    assert!(db.execute("INSERT INTO kv (k, v) VALUES ('q', 1) ON CONFLICT (v) DO NOTHING").is_err());

    // REPLACE replaces the whole row (defaults and all).
    db.execute("REPLACE INTO kv (k, v) VALUES ('a', 7)").unwrap();
    assert_eq!(rows(&db, "SELECT k, v, hits, tag FROM kv WHERE k = 'a'"), [["a", "7", "1", ""]]);
    db.execute("INSERT OR REPLACE INTO kv (k, v) VALUES ('b', 8)").unwrap();
    db.execute("INSERT OR IGNORE INTO kv (k, v) VALUES ('b', 9), ('fresh', 1)").unwrap();
    assert_eq!(rows(&db, "SELECT k, v FROM kv WHERE k IN ('b', 'fresh') ORDER BY k"), [["b", "8"], ["fresh", "1"]]);

    // RETURNING shows the rows that were inserted or updated, not the ones that were skipped.
    let r = db.execute("INSERT INTO kv (k, v) VALUES ('a', 1), ('r1', 2) ON CONFLICT (k) DO UPDATE SET v = 99 RETURNING k, v").unwrap();
    assert_eq!(r.rows.iter().map(|x| format!("{}={}", x[0], x[1])).collect::<Vec<_>>(), ["a=99", "r1=2"]);
    // Parameters and prepared statements.
    let p = db.prepare("INSERT INTO kv (k, v) VALUES (@k, @v) ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v").unwrap();
    db.execute_prepared(&p, &[("k", Value::text("p")), ("v", Value::Int(1))]).unwrap();
    db.execute_prepared(&p, &[("k", Value::text("p")), ("v", Value::Int(2))]).unwrap();
    assert_eq!(rows(&db, "SELECT v FROM kv WHERE k = 'p'"), [["2"]]);
    // A failure part-way undoes the whole statement.
    let n = rows(&db, "SELECT Count(*) FROM kv");
    assert!(db.execute("INSERT INTO kv (k, v) VALUES ('x1', 1), ('x2', 'not a number') ON CONFLICT (k) DO NOTHING").is_err());
    assert_eq!(rows(&db, "SELECT Count(*) FROM kv"), n);
}

#[test]
fn information_schema_describes_the_catalog() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE SEQUENCE ids START WITH 10;
         CREATE TABLE Customers (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL, City TEXT DEFAULT 'Rome', Code TEXT);
         CREATE UNIQUE INDEX byCode ON Customers (Code);
         CREATE TABLE Orders (OID INTEGER DEFAULT NEXT VALUE FOR ids PRIMARY KEY, CustID INTEGER REFERENCES Customers (ID) ON DELETE CASCADE DEFERRABLE, Total CURRENCY, Tax CALCULATED AS (Total * 0.2));
         CREATE VIEW Big AS SELECT OID, Total FROM Orders WHERE Total > 100;
         CREATE VIEW Joined AS SELECT c.Name, o.Total FROM Customers c JOIN Orders o ON o.CustID = c.ID;
         CREATE FUNCTION dbl(@a INTEGER) RETURNS INTEGER AS @a * 2;
         CREATE PROCEDURE noop() AS BEGIN RETURN; END;
         CREATE TRIGGER t1 AFTER INSERT OR UPDATE ON Orders FOR EACH STATEMENT BEGIN SELECT 1; END;
         CREATE REPORT r1 AS SELECT Name FROM Customers;
         INSERT INTO Customers (Name) VALUES ('Ada'), ('Grace')",
    )
    .unwrap();
    assert_eq!(rows(&db, "SELECT table_name, table_type, row_count FROM information_schema.tables ORDER BY table_name"),
        [["Big", "VIEW", ""], ["Customers", "BASE TABLE", "2"], ["Joined", "VIEW", ""], ["Orders", "BASE TABLE", "0"]]);
    assert_eq!(
        rows(&db, "SELECT column_name, ordinal_position, data_type, is_nullable, column_default FROM INFORMATION_SCHEMA.COLUMNS WHERE table_name = 'Customers' ORDER BY ordinal_position"),
        [["ID", "1", "INTEGER", "NO", ""], ["Name", "2", "TEXT", "NO", ""], ["City", "3", "TEXT", "YES", "Rome"], ["Code", "4", "TEXT", "YES", ""]]
    );
    assert_eq!(rows(&db, "SELECT column_default FROM information_schema.columns WHERE column_name = 'OID' AND table_name = 'Orders'"), [["NEXT VALUE FOR ids"]]);
    assert_eq!(rows(&db, "SELECT column_name, calculated_expression FROM information_schema.columns WHERE table_name = 'Orders' AND calculated_expression IS NOT NULL"), [["Tax", "Total * 0.2"]]);
    // A query's columns are what it returns.
    assert_eq!(rows(&db, "SELECT column_name FROM information_schema.columns WHERE table_name = 'Joined' ORDER BY ordinal_position"), [["Name"], ["Total"]]);
    assert_eq!(rows(&db, "SELECT table_name, is_updatable FROM information_schema.views ORDER BY table_name"), [["Big", "YES"], ["Joined", "NO"]]);
    assert_eq!(
        rows(&db, "SELECT table_name, constraint_type FROM information_schema.table_constraints ORDER BY table_name, constraint_type"),
        [["Customers", "PRIMARY KEY"], ["Customers", "UNIQUE"], ["Orders", "FOREIGN KEY"], ["Orders", "PRIMARY KEY"]]
    );
    assert_eq!(
        rows(&db, "SELECT table_name, column_name, referenced_table, referenced_column FROM information_schema.key_column_usage WHERE referenced_table IS NOT NULL"),
        [["Orders", "CustID", "Customers", "ID"]]
    );
    assert_eq!(rows(&db, "SELECT delete_rule, is_deferrable FROM information_schema.referential_constraints"), [["CASCADE", "YES"]]);
    assert_eq!(rows(&db, "SELECT Count(*) FROM information_schema.indexes WHERE is_unique AND table_name = 'Customers'"), [["2"]]);
    assert_eq!(rows(&db, "SELECT routine_name, routine_type, data_type FROM information_schema.routines ORDER BY routine_name"), [["dbl", "FUNCTION", "INTEGER"], ["noop", "PROCEDURE", ""]]);
    assert_eq!(rows(&db, "SELECT parameter_name, data_type FROM information_schema.parameters"), [["a", "INTEGER"]]);
    assert_eq!(rows(&db, "SELECT sequence_name, start_value FROM information_schema.sequences"), [["ids", "10"]]);
    assert_eq!(rows(&db, "SELECT trigger_name, action_timing, event_manipulation, action_orientation FROM information_schema.triggers"), [["t1", "AFTER", "INSERT, UPDATE", "STATEMENT"]]);
    assert_eq!(rows(&db, "SELECT report_name FROM information_schema.reports"), [["r1"]]);
    // They are tables like any other: joins, aliases, aggregates.
    assert_eq!(
        rows(&db, "SELECT t.table_name, Count(*) FROM information_schema.tables t JOIN information_schema.columns c ON c.table_name = t.table_name WHERE t.table_type = 'BASE TABLE' GROUP BY t.table_name ORDER BY 1"),
        [["Customers", "4"], ["Orders", "4"]]
    );
    assert!(db.query("SELECT * FROM information_schema.nosuch").is_err());
    // Always current.
    db.execute("ALTER TABLE Customers ADD COLUMN Extra INTEGER").unwrap();
    assert_eq!(rows(&db, "SELECT Count(*) FROM information_schema.columns WHERE table_name = 'Customers'"), [["5"]]);
}

/// A chain of `n` one-row tables, each joined to the next, written in chain order with a condition on the
/// last: the best order starts from the last table and walks back, so the optimizer reorders it. The tables
/// have one row each so that the test cannot run away if the optimizer does not take the query: without
/// it the conditions are applied after all the joins, and more rows would mean a Cartesian product.
fn chain_query(db: &mut Database, n: usize) -> String {
    for i in 1..=n {
        if db.table(&format!("c{i}")).is_none() {
            db.execute(&format!("CREATE TABLE c{i} (a INTEGER PRIMARY KEY, b INTEGER)")).unwrap();
            db.execute(&format!("INSERT INTO c{i} VALUES (1, 1)")).unwrap();
        }
    }
    let from: Vec<String> = (1..=n).map(|i| format!("c{i}")).collect();
    let mut conds: Vec<String> = (1..n).map(|i| format!("c{i}.b = c{}.a", i + 1)).collect();
    conds.push(format!("c{n}.a = 1"));
    format!("FROM {} WHERE {}", from.join(", "), conds.join(" AND "))
}

fn plan_of(db: &Database, q: &str) -> String {
    let plan = db.query(&format!("EXPLAIN SELECT c1.a {q}")).unwrap();
    plan.rows.iter().map(|r| r[0].to_string()).collect::<Vec<_>>().join("
")
}

#[test]
fn the_optimizer_orders_joins_of_up_to_128_tables() {
    let mut db = Database::in_memory();
    for n in [2, 63, 64, 100, 128] {
        let q = chain_query(&mut db, n);
        let r = db.query(&format!("SELECT COUNT(*) {q}")).unwrap();
        assert_eq!(r.rows, vec![vec![Value::Int(1)]], "{n} tables: the answer");
        let plan = plan_of(&db, &q);
        assert!(plan.contains("index nested-loop"), "{n} tables should have been joined by the optimizer's methods:
{plan}");
    }
}

#[test]
fn a_join_of_more_than_128_tables_is_run_as_written_and_still_right() {
    let mut db = Database::in_memory();
    let q = chain_query(&mut db, 129);
    let r = db.query(&format!("SELECT COUNT(*) {q}")).unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(1)]]);
    let plan = plan_of(&db, &q);
    assert!(plan.contains("nested loop") && !plan.contains("index nested-loop"), "129 tables are beyond the optimizer:
{plan}");
}

// ----- ascending and descending index columns ---------------------------------------------------

fn ordered_db() -> Database {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE r (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, s TEXT)").unwrap();
    // a few hundred rows with plenty of ties and some NULLs
    let mut x: u64 = 12345;
    let mut rows = Vec::new();
    for id in 0..400 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let a = if (x >> 40) % 11 == 0 { "NULL".to_string() } else { ((x >> 33) % 7).to_string() };
        let b = if (x >> 45) % 13 == 0 { "NULL".to_string() } else { ((x >> 20) % 9).to_string() };
        rows.push(format!("({id}, {a}, {b}, 's{}')", (x >> 50) % 5));
    }
    db.execute(&format!("INSERT INTO r VALUES {}", rows.join(", "))).unwrap();
    db
}

fn ids(db: &Database, sql: &str) -> Vec<String> {
    db.query(sql).unwrap().rows.iter().map(|r| r[0].to_string()).collect()
}

#[test]
fn indexes_can_be_declared_ascending_and_descending() {
    let mut db = ordered_db();
    db.execute("CREATE INDEX r_ab ON r (a ASC, b DESC)").unwrap();
    db.execute("CREATE UNIQUE INDEX r_s_id ON r (s DESC, id DESC)").unwrap();
    let defs: Vec<(String, Vec<bool>)> = db.table("r").unwrap().index_defs().map(|d| (d.name.clone(), d.descending.clone())).collect();
    assert!(defs.contains(&("r_ab".to_string(), vec![false, true])), "{defs:?}");
    assert!(defs.contains(&("r_s_id".to_string(), vec![true, true])), "{defs:?}");
    // ASC is just the default spelled out, and the keyword is not a column name
    db.execute("CREATE INDEX r_b ON r (b ASC)").unwrap();
    assert!(db.table("r").unwrap().index_defs().any(|d| d.name == "r_b" && d.descending.is_empty()));
    assert!(db.execute("CREATE INDEX bad ON r (a SIDEWAYS)").is_err());
}

#[test]
fn every_order_by_gives_the_same_rows_with_and_without_an_index() {
    let mut db = ordered_db();
    db.execute("CREATE INDEX r_ab ON r (a ASC, b DESC)").unwrap();
    db.execute("CREATE INDEX r_ba ON r (b DESC, a DESC)").unwrap();
    db.execute("CREATE INDEX r_s ON r (s DESC)").unwrap();
    db.execute("CREATE INDEX r_plain ON r (a, b)").unwrap();
    let mut served = 0;
    for order in [
        "a", "a DESC", "a, b", "a, b DESC", "a DESC, b", "a DESC, b DESC", "b DESC, a DESC", "b, a", "b DESC", "b",
        "s DESC", "s", "a NULLS LAST", "a DESC NULLS FIRST", "a, b NULLS FIRST",
    ] {
        for filter in ["", "WHERE a = 3", "WHERE b > 2", "WHERE s = 's1'"] {
            for limit in ["", "LIMIT 7", "LIMIT 60 OFFSET 5"] {
                // ties: finish with the key so that the order is total and comparable
                let sql = format!("SELECT id FROM r {filter} ORDER BY {order}, id {limit}");
                db.set_optimizer(true);
                let with = ids(&db, &sql);
                db.set_optimizer(false);
                let without = ids(&db, &sql);
                assert_eq!(with, without, "{sql}");
                // the same order without the tie-break, only checked as a sequence of sort keys
                let key_sql = |opt: bool, db: &mut Database| {
                    db.set_optimizer(opt);
                    let cols = order.split(',').map(|c| c.split_whitespace().next().unwrap().to_string()).collect::<Vec<_>>().join(", ");
                    db.query(&format!("SELECT {cols} FROM r {filter} ORDER BY {order} {limit}")).unwrap().rows
                };
                assert_eq!(key_sql(true, &mut db), key_sql(false, &mut db), "keys of {order} {filter} {limit}");
            }
        }
        db.set_optimizer(true);
        let plan = db.query(&format!("EXPLAIN SELECT id FROM r ORDER BY {order} LIMIT 5")).unwrap();
        if plan.rows.iter().any(|r| r[0].to_string().contains("satisfied by index")) {
            served += 1;
        }
    }
    assert!(served >= 8, "indexes should serve most of these orders, served {served}");
}

#[test]
fn a_mixed_direction_order_is_served_by_an_index_of_the_same_directions_only() {
    let mut db = ordered_db();
    let plan = |db: &Database, order: &str| -> String {
        db.query(&format!("EXPLAIN SELECT id FROM r ORDER BY {order} LIMIT 5")).unwrap().rows.iter().map(|r| r[0].to_string()).collect::<Vec<_>>().join("\n")
    };
    db.execute("CREATE INDEX r_plain ON r (a, b)").unwrap();
    assert!(!plan(&db, "a, b DESC").contains("satisfied by index"), "an ascending index cannot give a mixed order");
    assert!(plan(&db, "a, b").contains("satisfied by index r_plain"));
    assert!(plan(&db, "a DESC, b DESC").contains("satisfied by index r_plain"), "walked backwards");
    db.execute("CREATE INDEX r_mixed ON r (a ASC, b DESC)").unwrap();
    let p = plan(&db, "a, b DESC");
    assert!(p.contains("satisfied by index r_mixed") && p.contains("mixed directions"), "{p}");
    let p = plan(&db, "a DESC, b");
    assert!(p.contains("satisfied by index r_mixed"), "the opposite of the index, walked backwards: {p}");
    assert!(!plan(&db, "a DESC, b DESC LIMIT 5").is_empty());
}

#[test]
fn lookups_ranges_and_unique_keys_work_through_a_descending_index() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE d (k INTEGER, t TEXT)").unwrap();
    db.execute("CREATE UNIQUE INDEX d_k ON d (k DESC)").unwrap();
    db.execute("INSERT INTO d VALUES (5, 'five'), (1, 'one'), (9, 'nine'), (3, 'three'), (7, 'seven'), (NULL, 'none')").unwrap();
    assert!(db.execute("INSERT INTO d VALUES (9, 'again')").is_err(), "the unique key holds");
    assert_eq!(ids(&db, "SELECT t FROM d WHERE k = 7"), vec!["seven"]);
    fn sorted(db: &Database, sql: &str) -> Vec<String> {
        let mut v = ids(db, sql);
        v.sort();
        v
    }
    assert_eq!(sorted(&db, "SELECT t FROM d WHERE k > 3"), vec!["five", "nine", "seven"]);
    assert_eq!(sorted(&db, "SELECT t FROM d WHERE k >= 3 AND k < 9"), vec!["five", "seven", "three"]);
    assert_eq!(sorted(&db, "SELECT t FROM d WHERE k BETWEEN 1 AND 5"), vec!["five", "one", "three"]);
    assert_eq!(sorted(&db, "SELECT t FROM d WHERE k <= 1"), vec!["one"]);
    assert_eq!(sorted(&db, "SELECT t FROM d WHERE k IS NULL"), vec!["none"]);
    // a change of key moves the entry
    db.execute("UPDATE d SET k = 4 WHERE k = 9").unwrap();
    assert_eq!(sorted(&db, "SELECT t FROM d WHERE k = 4"), vec!["nine"]);
    assert!(db.execute("UPDATE d SET k = 4 WHERE k = 7").is_err());
    db.execute("DELETE FROM d WHERE k = 4").unwrap();
    assert_eq!(ids(&db, "SELECT t FROM d WHERE k = 4").len(), 0);
    assert_eq!(ids(&db, "SELECT t FROM d ORDER BY k DESC LIMIT 2"), vec!["seven", "five"]);
    assert_eq!(ids(&db, "SELECT t FROM d ORDER BY k LIMIT 2"), vec!["none", "one"]);
}

#[test]
fn creating_a_unique_descending_index_checks_the_rows_that_are_there() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE u (a INTEGER, b INTEGER); INSERT INTO u VALUES (1, 1), (1, 1)").unwrap();
    assert!(db.execute("CREATE UNIQUE INDEX u_ab ON u (a DESC, b ASC)").is_err());
    db.execute("DELETE FROM u WHERE a = 1 AND b = 1 AND a IS NOT NULL").unwrap();
    db.execute("CREATE UNIQUE INDEX u_ab ON u (a DESC, b ASC)").unwrap();
}

// ----- / and DIV ---------------------------------------------------------------------------------

#[test]
fn slash_truncates_whole_numbers_and_divides_the_rest_in_floating_point() {
    let db = Database::in_memory();
    let t = |sql: &str| {
        let r = db.query(sql).unwrap();
        format!("{} {}", r.rows[0][0].type_label(), r.rows[0][0])
    };
    // whole numbers: the fraction is dropped, toward zero (the SQLite, PostgreSQL and SQL Server result)
    assert_eq!(t("SELECT 7 / 2"), "INTEGER 3");
    assert_eq!(t("SELECT -7 / 2"), "INTEGER -3");
    assert_eq!(t("SELECT 7 / -2"), "INTEGER -3");
    assert_eq!(t("SELECT 1 / 3"), "INTEGER 0");
    assert_eq!(t("SELECT 6 / 3"), "INTEGER 2");
    assert_eq!(t("SELECT CBig(100000000000000000000) / 7"), "BIGNUM 14285714285714285714");
    // anything else: floating point
    assert_eq!(t("SELECT 7 / 2.0"), "DOUBLE 3.5");
    assert_eq!(t("SELECT 1 / 3.0"), "DOUBLE 0.3333333333333333");
    assert_eq!(t("SELECT 7 / CDbl(2)"), "DOUBLE 3.5");
    assert_eq!(t("SELECT CSng(1) / 4"), "FLOAT 0.25");
    assert_eq!(t("SELECT CCur(10) / 4"), "DOUBLE 2.5");
    // division by zero is still an error
    // division by zero is NULL, not an error
    assert!(db.query("SELECT 1 / 0").unwrap().rows[0][0].is_null());
    assert!(db.query("SELECT 1 / 0.0").unwrap().rows[0][0].is_null());
}

#[test]
fn div_is_exact_division() {
    let db = Database::in_memory();
    let t = |sql: &str| {
        let r = db.query(sql).unwrap();
        format!("{} {}", r.rows[0][0].type_label(), r.rows[0][0])
    };
    assert_eq!(t("SELECT 7 DIV 2"), "DECIMAL 3.5");
    assert_eq!(t("SELECT 6 DIV 3"), "DECIMAL 2");
    assert_eq!(t("SELECT 1 DIV 3"), format!("DECIMAL 0.{}", "3".repeat(50)));
    assert_eq!(t("SELECT 0.3 DIV 0.1"), "DECIMAL 3");
    assert_eq!(t("SELECT CCur(10) DIV 4"), "CURRENCY $2.50");
    assert_eq!(t("SELECT CRat(1) DIV 3"), "RATIONAL 1/3");
    assert_eq!(t("SELECT 7 DIV 2.0"), "DECIMAL 3.5");
    assert_eq!(t("SELECT CDbl(7) DIV 2"), "DOUBLE 3.5");
    assert_eq!(t("SELECT INTERVAL '10 hours' DIV 4"), "INTERVAL 02:30:00");
    // same precedence as * / \ MOD, left to right
    assert_eq!(t("SELECT 2 + 9 DIV 2 * 2"), "DECIMAL 11");
    assert_eq!(t("SELECT 9 DIV 2 / 2"), "DOUBLE 2.25");
    assert!(db.query("SELECT 1 DIV 0").unwrap().rows[0][0].is_null());
    // the whole-number divisions side by side
    assert_eq!(t(r"SELECT 7 \ 2"), "INTEGER 3");
    assert_eq!(t(r"SELECT 7.6 \ 2"), "INTEGER 4");
    assert_eq!(t("SELECT 7 MOD 2"), "INTEGER 1");
}

#[test]
fn div_is_a_reserved_word_and_averages_stay_exact() {
    let mut db = Database::in_memory();
    assert!(db.execute("CREATE TABLE d (div INTEGER)").is_err(), "DIV cannot be a bare name");
    db.execute("CREATE TABLE d ([div] INTEGER, v DECIMAL)").unwrap();
    db.execute("INSERT INTO d VALUES (7, 0.1), (2, 0.2), (3, 0.3)").unwrap();
    assert_eq!(ids(&db, "SELECT [div] DIV 2 FROM d WHERE [div] = 7"), vec!["3.5"]);
    // Avg and Sum are exact whatever / does
    assert_eq!(ids(&db, "SELECT Avg(v) FROM d"), vec!["0.2"]);
    assert_eq!(ids(&db, "SELECT Avg(v) * 3 FROM d"), vec!["0.6"]);
    assert_eq!(ids(&db, "SELECT Avg([div]) FROM d"), vec!["4"]);
}

// ----- SQLite-compatible forms ---------------------------------------------------------------------

#[test]
fn dividing_by_zero_is_null() {
    let db = Database::in_memory();
    let null = |sql: &str| db.query(sql).unwrap().rows[0][0].is_null();
    for sql in [
        "SELECT 1 / 0", "SELECT 1 / 0.0", "SELECT 1 DIV 0", "SELECT Mod(7, 0)",
        "SELECT CCur(5) / 0", "SELECT CRat(1) DIV 0", "SELECT INTERVAL '1 day' / 0", "SELECT 1 / (2 - 2)",
    ] {
        assert!(null(sql), "{sql}");
    }
    // so a row's other columns still come out, and a condition on the quotient simply does not hold
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE q (a INTEGER, b INTEGER); INSERT INTO q VALUES (6, 3), (5, 0), (4, 2)").unwrap();
    assert_eq!(ids(&db, "SELECT a / b FROM q ORDER BY a"), vec!["2", "NULL", "2"].iter().map(|s| if *s == "NULL" { "".to_string() } else { s.to_string() }).collect::<Vec<_>>());
    assert_eq!(ids(&db, "SELECT a FROM q WHERE a / b > 0 ORDER BY a"), vec!["4", "6"]);
}

#[test]
fn all_is_allowed_in_an_aggregate() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE a (x INTEGER); INSERT INTO a VALUES (1), (2), (2), (NULL), (5)").unwrap();
    let one = |sql: &str| db.query(sql).unwrap().rows[0].iter().map(|v| v.to_string()).collect::<Vec<_>>();
    assert_eq!(one("SELECT COUNT(ALL x), SUM(ALL x), MIN(ALL x), MAX(ALL x) FROM a"), ["4", "10", "1", "5"]);
    assert_eq!(one("SELECT COUNT(ALL x), COUNT(DISTINCT x), COUNT(x), COUNT(*) FROM a"), ["4", "3", "4", "5"]);
    assert_eq!(one("SELECT AVG(ALL x) FROM a"), ["2.5"]);
    assert_eq!(one("SELECT SUM(ALL x + 1) FROM a"), ["14"]);
    assert_eq!(one("SELECT SELECT_ALL FROM (SELECT ALL MIN ( ALL x ) AS select_all FROM a) AS q"), ["1"]);
}

#[test]
fn in_with_an_empty_list_or_subquery_is_false_whatever_the_left_side() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE e (a INTEGER); CREATE TABLE f (a INTEGER); INSERT INTO f VALUES (1), (NULL)").unwrap();
    let one = |sql: &str| db.query(sql).unwrap().rows[0].iter().map(|v| v.to_string()).collect::<Vec<_>>();
    // an empty list (SQLite and MySQL allow it)
    assert_eq!(one("SELECT 1 IN (), 1 NOT IN (), NULL IN (), NULL NOT IN ()"), ["No", "Yes", "No", "Yes"]);
    // an empty subquery: the standard says x = ANY (nothing) is false, even for NULL
    assert_eq!(one("SELECT NULL IN (SELECT a FROM e), NULL NOT IN (SELECT a FROM e), 1 IN (SELECT a FROM e), 1 NOT IN (SELECT a FROM e)"), ["No", "Yes", "No", "Yes"]);
    // not empty: unknown for NULL
    // (NULL is shown as nothing)
    assert_eq!(one("SELECT NULL IN (SELECT a FROM f)"), [""]);
    assert_eq!(one("SELECT NULL NOT IN (SELECT a FROM f)"), [""]);
    assert_eq!(one("SELECT 2 IN (SELECT a FROM f)"), [""]); // the NULL in the set
    assert_eq!(one("SELECT 1 IN (SELECT a FROM f)"), ["Yes"]);
    // correlated: the inner query is empty for some outer rows
    db.execute("CREATE TABLE o (k INTEGER); INSERT INTO o VALUES (1), (2)").unwrap();
    let r = db.query("SELECT k, NULL IN (SELECT a FROM f WHERE f.a = o.k) AS x, NULL NOT IN (SELECT a FROM f WHERE f.a = o.k) AS y FROM o ORDER BY k").unwrap();
    let got: Vec<Vec<String>> = r.rows.iter().map(|row| row.iter().map(|v| v.to_string()).collect()).collect();
    assert_eq!(got, [["1", "", ""], ["2", "No", "Yes"]]);
}

#[test]
fn hexadecimal_literals_are_text() {
    let db = Database::in_memory();
    let one = |sql: &str| db.query(sql).unwrap().rows[0][0].to_string();
    assert_eq!(one("SELECT x'48656c6c6f'"), "Hello");
    assert_eq!(one("SELECT X'303132'"), "012");
    assert_eq!(one("SELECT x''"), "");
    assert_eq!(one("SELECT x'c3a9'"), "\u{e9}");
    assert_eq!(one("SELECT LENGTH(x'303132')"), "3");
    assert!(db.query("SELECT x'3'").is_err(), "an odd number of digits");
    assert!(db.query("SELECT x'zz'").is_err(), "not hexadecimal");
    assert!(db.query("SELECT x'ff'").is_err(), "not text: there is no BLOB type");
    // x is still an ordinary name
    assert!(db.query("SELECT x FROM (SELECT 1 AS x) AS t").is_ok());
}

#[test]
fn a_trigger_without_a_timing_is_a_before_trigger() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE t (a INTEGER, b INTEGER)").unwrap();
    db.execute("CREATE TRIGGER no_negatives INSERT ON t FOR EACH ROW WHEN NEW.a < 0 BEGIN RAISE 'negative'; END").unwrap();
    assert!(db.execute("INSERT INTO t (a) VALUES (-1)").is_err());
    db.execute("INSERT INTO t (a) VALUES (4)").unwrap();
    assert_eq!(ids(&db, "SELECT COUNT(*) FROM t"), vec!["1"], "refused before the row was stored");
    assert_eq!(ids(&db, "SELECT action_timing FROM information_schema.triggers"), vec!["BEFORE"]);
    assert!(db.execute("CREATE TRIGGER nothing ON t BEGIN SELECT 1; END").is_err());
}

#[test]
fn drop_index_can_leave_out_the_table_and_reindex_rebuilds() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE a (x INTEGER); CREATE TABLE b (y INTEGER)").unwrap();
    db.execute("CREATE INDEX only_a ON a (x); CREATE INDEX both ON a (x); CREATE INDEX both ON b (y)").unwrap();
    db.execute("INSERT INTO a VALUES (3), (1), (2); INSERT INTO b VALUES (9)").unwrap();
    db.execute("DROP INDEX only_a").unwrap();
    assert!(db.table("a").unwrap().index_defs().all(|d| d.name != "only_a"));
    assert!(db.execute("DROP INDEX only_a").is_err(), "gone");
    db.execute("DROP INDEX IF EXISTS only_a").unwrap();
    let err = db.execute("DROP INDEX both").unwrap_err().to_string();
    assert!(err.contains("ON table"), "{err}");
    db.execute("DROP INDEX both ON b").unwrap();
    db.execute("DROP INDEX both").unwrap(); // now only one table has it
    assert_eq!(db.table("a").unwrap().index_defs().count(), 0);

    db.execute("CREATE UNIQUE INDEX ux ON a (x); CREATE INDEX iy ON b (y)").unwrap();
    for sql in ["REINDEX", "REINDEX a", "REINDEX ux", "REINDEX iy;"] {
        db.execute(sql).unwrap();
    }
    assert!(db.execute("REINDEX nothing").is_err());
    assert_eq!(ids(&db, "SELECT x FROM a WHERE x = 2"), vec!["2"]);
    assert_eq!(ids(&db, "SELECT x FROM a ORDER BY x LIMIT 3"), vec!["1", "2", "3"]);
    assert!(db.execute("INSERT INTO a VALUES (2)").is_err(), "the unique index still holds");
}

// ----- PostgreSQL type names and typed literals -----------------------------------------------------

#[test]
fn postgresql_type_names_are_accepted() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE pg (a int2, b int4, c int8, d float4, e float8, f name, g oid, h bpchar, i serial4, j bigint, k double precision, l character varying(10))").unwrap();
    let types: Vec<String> = db.table("pg").unwrap().schema().columns.iter().map(|c| c.data_type.to_string()).collect();
    assert_eq!(types, ["INTEGER", "INTEGER", "INTEGER", "FLOAT", "DOUBLE", "TEXT", "INTEGER", "TEXT", "INTEGER", "INTEGER", "DOUBLE", "TEXT"]);
    assert!(db.table("pg").unwrap().schema().columns[8].autonumber, "serial4 is an AutoNumber");
    db.execute("INSERT INTO pg (a, b, c, d, e, f, g, h, k, l) VALUES (1, 2, 3, 4.5, 5.5, 'n', 7, 'x', 1.25, 'v')").unwrap();
    let r = db.query("SELECT CAST(a AS int8), CAST(b AS float8), CAST(c AS int2) FROM pg").unwrap();
    assert_eq!(r.rows[0].iter().map(|v| v.to_string()).collect::<Vec<_>>(), ["1", "2", "3"]);
}

#[test]
fn a_type_name_before_a_string_casts_it() {
    let db = Database::in_memory();
    let t = |sql: &str| {
        let r = db.query(sql).unwrap();
        format!("{} {}", r.rows[0][0].type_label(), r.rows[0][0])
    };
    assert_eq!(t("SELECT bool 't'"), "YESNO Yes");
    assert_eq!(t("SELECT bool 'false'"), "YESNO No");
    assert_eq!(t("SELECT boolean 'yes'"), "YESNO Yes");
    assert_eq!(t("SELECT int4 '42'"), "INTEGER 42");
    assert_eq!(t("SELECT int8 '9000000000'"), "INTEGER 9000000000");
    assert_eq!(t("SELECT int2 ' 7 '"), "INTEGER 7");
    assert_eq!(t("SELECT float8 '1.5e2'"), "DOUBLE 150");
    assert_eq!(t("SELECT float4 '2.5'"), "FLOAT 2.5");
    assert_eq!(t("SELECT double precision '3.25'"), "DOUBLE 3.25");
    assert_eq!(t("SELECT numeric '1.50'"), "DECIMAL 1.5");
    assert_eq!(t("SELECT text 'abc'"), "TEXT abc");
    assert_eq!(t("SELECT name 'x'"), "TEXT x");
    assert_eq!(t("SELECT bigint '5'"), "INTEGER 5");
    // the older typed literals still work
    assert_eq!(t("SELECT date '2024-03-01'"), "DATETIME 2024-03-01");
    assert_eq!(t("SELECT interval '1 day'"), "INTERVAL 1 day");
    // they work inside expressions, inserts and conditions
    assert_eq!(t("SELECT int4 '40' + int4 '2'"), "INTEGER 42");
    assert!(db.query("SELECT int4 'abc'").is_err());
    assert!(db.query("SELECT bool 'maybe'").is_err());
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE b (f1 bool); INSERT INTO b VALUES (bool 't'), (bool 'f'), (bool 'yes'), (bool 'off')").unwrap();
    assert_eq!(ids(&db, "SELECT COUNT(*) FROM b WHERE f1 = bool 'true'"), vec!["2"]);
    // a column or function that happens to be called like a type is still a name
    db.execute("CREATE TABLE n ([name] TEXT); INSERT INTO n VALUES ('a')").unwrap();
    assert_eq!(ids(&db, "SELECT [name] FROM n"), vec!["a"]);
}

#[test]
fn booleans_are_read_the_way_postgresql_reads_them() {
    let db = Database::in_memory();
    let yes = |s: &str| db.query(&format!("SELECT CBool('{s}')")).unwrap().rows[0][0].to_string();
    for s in ["t", "T", "tr", "tru", "true", "TRUE", "y", "ye", "yes", "on", "1", " true "] {
        assert_eq!(yes(s), "Yes", "{s}");
    }
    for s in ["f", "fa", "fals", "false", "n", "no", "of", "off", "0", " FALSE "] {
        assert_eq!(yes(s), "No", "{s}");
    }
    for s in ["", "o", "tx", "maybe", "truee", "2"] {
        assert!(db.query(&format!("SELECT CBool('{s}')")).is_err(), "{s}");
    }
}

// ----- :: casts, run-time parameters, EXPLAIN options, GROUPING ------------------------------------

#[test]
fn double_colon_casts() {
    let db = Database::in_memory();
    let t = |sql: &str| {
        let r = db.query(sql).unwrap();
        format!("{} {}", r.rows[0][0].type_label(), r.rows[0][0])
    };
    assert_eq!(t("SELECT '42'::int4"), "INTEGER 42");
    assert_eq!(t("SELECT 'true'::boolean"), "YESNO Yes");
    assert_eq!(t("SELECT 1.9::int"), "INTEGER 1");
    assert_eq!(t("SELECT -5::int"), "INTEGER -5");
    assert_eq!(t("SELECT 7::text || 'x'"), "TEXT 7x");
    assert_eq!(t("SELECT '2024-03-01'::date"), "DATETIME 2024-03-01");
    assert_eq!(t("SELECT 3::float8 / 2"), "DOUBLE 1.5");
    assert_eq!(t("SELECT '1.55'::numeric(10, 1)"), "DECIMAL 1.6");
    assert_eq!(t("SELECT (1 + 2)::text"), "TEXT 3");
    assert_eq!(t("SELECT '1'::int::float8::text"), "TEXT 1");
    assert_eq!(t("SELECT double precision '2.5'::int"), "INTEGER 2");
    assert_eq!(t("SELECT '5'::int + '6'::int"), "INTEGER 11");
    assert!(db.query("SELECT 'abc'::int").is_err());
    assert!(db.query("SELECT 1::nosuchtype").is_err());
}

#[test]
fn run_time_parameters_can_be_set_reset_and_shown() {
    let mut db = Database::in_memory();
    let show = |db: &mut Database, name: &str| db.execute(&format!("SHOW {name}")).unwrap().rows[0][0].to_string();
    assert_eq!(show(&mut db, "extra_float_digits"), "1");
    db.execute("SET extra_float_digits TO 3").unwrap();
    assert_eq!(show(&mut db, "extra_float_digits"), "3");
    db.execute("SET SESSION extra_float_digits = 2; SET LOCAL work_mem = '64MB'").unwrap();
    assert_eq!((show(&mut db, "extra_float_digits"), show(&mut db, "work_mem")), ("2".to_string(), "64MB".to_string()));
    db.execute("RESET extra_float_digits").unwrap();
    assert_eq!(show(&mut db, "extra_float_digits"), "1");
    db.execute("SET datestyle TO ISO, DMY").unwrap();
    assert_eq!(show(&mut db, "DateStyle"), "ISO, DMY");
    db.execute("SET extra_float_digits TO DEFAULT").unwrap();
    // switches take on or off, in any of the usual spellings
    db.execute("SET enable_seqscan = false; SET enable_hashjoin TO 'OFF'").unwrap();
    assert_eq!((show(&mut db, "enable_seqscan"), show(&mut db, "enable_hashjoin")), ("off".to_string(), "off".to_string()));
    assert!(db.execute("SET enable_seqscan TO maybe").is_err());
    // names with a dot may be made up
    db.execute("SET my.setting TO 'abc'").unwrap();
    assert_eq!(show(&mut db, "my.setting"), "abc");
    assert!(db.execute("SHOW other.thing").is_err());
    // unknown names are refused
    assert!(db.execute("SET nosuch = 1").is_err());
    assert!(db.execute("SHOW nosuch").is_err());
    assert!(db.execute("RESET nosuch").is_err());
    // the time zone and the optimizer are real
    db.execute("SET TIME ZONE 'Europe/Paris'").unwrap();
    assert_eq!(show(&mut db, "timezone"), "Europe/Paris");
    db.execute("SET timezone TO 'UTC'").unwrap();
    assert_eq!(db.time_zone(), "UTC");
    db.execute("SET optimizer = off").unwrap();
    assert!(!db.optimizer_enabled());
    db.execute("RESET optimizer").unwrap();
    assert!(db.optimizer_enabled());
    // SHOW ALL lists them; RESET ALL puts everything back
    let all = db.execute("SHOW ALL").unwrap();
    assert_eq!(all.columns, ["name", "setting", "description"]);
    assert!(all.rows.len() > 50);
    db.execute("RESET ALL").unwrap();
    assert_eq!(show(&mut db, "enable_seqscan"), "on");
    // statements about transactions and roles are read and ignored
    db.execute("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE").unwrap();
    db.execute("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY").unwrap();
}

#[test]
fn explain_takes_options_and_describes_other_statements() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE t (a INTEGER PRIMARY KEY); INSERT INTO t VALUES (1), (2)").unwrap();
    let lines = |db: &mut Database, sql: &str| db.execute(sql).unwrap().rows.iter().map(|r| r[0].to_string()).collect::<Vec<_>>().join("\n");
    for sql in [
        "EXPLAIN SELECT * FROM t", "EXPLAIN (COSTS OFF) SELECT * FROM t", "EXPLAIN (ANALYZE, VERBOSE, BUFFERS OFF, TIMING false) SELECT * FROM t",
        "EXPLAIN ANALYZE SELECT * FROM t", "EXPLAIN ANALYZE VERBOSE SELECT * FROM t WHERE a = 1", "EXPLAIN (FORMAT TEXT, SUMMARY OFF) SELECT 1",
        "explain (costs off) WITH x AS (SELECT 1) SELECT * FROM x", "EXPLAIN (costs off) (SELECT a FROM t)",
    ] {
        assert!(lines(&mut db, sql).contains("row(s) returned"), "{sql}");
    }
    // a statement that is not a query is described, and not run
    let plan = lines(&mut db, "EXPLAIN (COSTS OFF) INSERT INTO t VALUES (3)");
    assert!(plan.contains("insert into t"), "{plan}");
    assert_eq!(db.execute("SELECT COUNT(*) FROM t").unwrap().rows[0][0], Value::Int(2));
    assert!(lines(&mut db, "EXPLAIN DELETE FROM t").contains("delete from t"));
    assert_eq!(db.execute("SELECT COUNT(*) FROM t").unwrap().rows[0][0], Value::Int(2));
    // options that cannot be honoured are refused
    assert!(db.execute("EXPLAIN (FORMAT JSON) SELECT 1").is_err());
    assert!(db.execute("EXPLAIN (NONSENSE) SELECT 1").is_err());
}

#[test]
fn grouping_works_without_grouping_sets_and_count_needs_a_star() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE g (ten INTEGER); INSERT INTO g VALUES (1), (1), (2)").unwrap();
    let r = db.query("SELECT ten, GROUPING(ten) FROM g GROUP BY (ten) HAVING GROUPING(ten) >= 0 ORDER BY 2, 1").unwrap();
    assert_eq!(r.rows.iter().map(|x| format!("{}/{}", x[0], x[1])).collect::<Vec<_>>(), ["1/0", "2/0"]);
    let r = db.query("SELECT GROUPING(ten) FROM g GROUP BY ten").unwrap();
    assert_eq!(r.rows.len(), 2);
    let e = db.query("SELECT COUNT() OVER () FROM g").unwrap_err().to_string();
    assert!(e.contains("count(*)"), "{e}");
}

// ----- PostgreSQL functions ---------------------------------------------------------------------------

#[test]
fn postgresql_functions() {
    let db = Database::in_memory();
    let one = |sql: &str| db.query(sql).unwrap().rows[0][0].to_string();
    assert_eq!(one("SELECT pg_input_is_valid('true', 'bool')"), "Yes");
    assert_eq!(one("SELECT pg_input_is_valid('maybe', 'boolean')"), "No");
    assert_eq!(one("SELECT pg_input_is_valid('12', 'int4')"), "Yes");
    assert_eq!(one("SELECT pg_input_is_valid('x12', 'int4')"), "No");
    assert!(db.query("SELECT pg_input_is_valid('1', 'nosuchtype')").is_err());
    assert_eq!(one("SELECT pg_typeof('x')"), "text");
    assert_eq!(one("SELECT pg_typeof(1.5)"), "numeric");
    assert_eq!(one("SELECT pg_typeof(1.5e0)"), "double precision");
    assert_eq!(one("SELECT scale(12.345)"), "3");
    assert_eq!(one("SELECT scale(7)"), "0");
    assert_eq!(one("SELECT trim_scale(1.500)"), "1.5");
    assert_eq!(one("SELECT round(asinh(1) * 1000)"), "881");
    assert_eq!(one("SELECT concat_ws('-', 'a', NULL, 'b')"), "a-b");
    assert_eq!(one("SELECT concat_ws(NULL, 'a', 'b') IS NULL"), "Yes");
    assert_eq!(one("SELECT regexp_instr('abcdefghi', 'd.f')"), "4");
    assert_eq!(one("SELECT regexp_instr('abcdefghi', 'd.f', 1, 1, 1)"), "7");
    assert_eq!(one("SELECT regexp_instr('abc', 'x')"), "0");
    assert_eq!(one("SELECT has_table_privilege('someone', 'atable', 'select')"), "Yes");
    assert_eq!(one("SELECT has_schema_privilege('someone', 'public', 'usage')"), "Yes");
    assert!(db.query("SELECT has_table_privilege(NULL, 'atable', 'select')").unwrap().rows[0][0].is_null());
    // a type name used as a function is a cast
    assert_eq!(one("SELECT float8(5) / 2"), "2.5");
    assert_eq!(one("SELECT int4(2.7)"), "2");
    assert_eq!(one("SELECT text(42) || 'x'"), "42x");
}

#[test]
fn dates_built_truncated_and_formatted() {
    let db = Database::in_memory();
    let one = |sql: &str| db.query(sql).unwrap().rows[0][0].to_string();
    assert_eq!(one("SELECT make_date(2024, 3, 15)"), "2024-03-15");
    assert_eq!(one("SELECT make_timestamp(2024, 3, 15, 10, 20, 30)"), "2024-03-15 10:20:30");
    assert!(db.query("SELECT make_date(2024, 13, 1)").is_err());
    assert_eq!(one("SELECT date_trunc('month', timestamp '2024-03-15 10:20:30')"), "2024-03-01");
    assert_eq!(one("SELECT date_trunc('hour', timestamp '2024-03-15 10:20:30')"), "2024-03-15 10:00:00");
    assert_eq!(one("SELECT date_trunc('week', timestamp '2024-03-15 10:20:30')"), "2024-03-11");
    assert_eq!(one("SELECT date_trunc('quarter', timestamp '2024-08-15 10:20:30')"), "2024-07-01");
    assert_eq!(one("SELECT date_trunc('decade', timestamp '2024-08-15 10:20:30')"), "2020-01-01");
    assert_eq!(one("SELECT date_trunc('century', timestamp '2024-08-15 10:20:30')"), "2001-01-01");
    assert!(db.query("SELECT date_trunc('fortnight', timestamp '2024-08-15 10:20:30')").is_err());
    assert_eq!(one("SELECT to_char(timestamp '2024-03-01 15:04:05', 'YYYY-MM-DD HH24:MI:SS')"), "2024-03-01 15:04:05");
    assert_eq!(one("SELECT to_char(timestamp '2024-03-01 15:04:05', 'Dy Mon DD, HH12:MI PM')"), "Fri Mar 01, 03:04 PM");
    assert_eq!(one("SELECT to_char(date '2024-03-01', 'FMMM/FMDD/YY')"), "3/1/24");
    assert_eq!(one("SELECT to_char(date '2024-03-01', 'DDD D Q')"), "061 6 1");
    assert_eq!(one("SELECT to_char(date '2024-03-01', 'Month')"), "March    ");
    assert_eq!(one("SELECT to_char(date '2024-03-01', 'FMMonth \"of\" YYYY')"), "March of 2024");
    assert_eq!(one("SELECT to_date('20100309', 'YYYYMMDD')"), "2010-03-09");
    assert_eq!(one("SELECT to_date('March 5, 2024', 'Month DD, YYYY')"), "2024-03-05");
    assert_eq!(one("SELECT to_timestamp('2024-03-01 10:30:00', 'YYYY-MM-DD HH24:MI:SS')"), "2024-03-01 10:30:00+00:00");
    assert_eq!(one("SELECT to_timestamp('03:04 PM', 'HH12:MI PM')"), "0001-01-01 15:04:00+00:00");
    assert_eq!(one("SELECT to_timestamp(86400)"), "1970-01-02 00:00:00+00:00");
    assert!(db.query("SELECT to_date('2024-13-45', 'YYYY-MM-DD')").is_err());
    assert!(db.query("SELECT to_date('abc', 'YYYY')").is_err());
}

#[test]
fn numbers_formatted_and_read_with_patterns() {
    let db = Database::in_memory();
    let one = |sql: &str| db.query(sql).unwrap().rows[0][0].to_string();
    assert_eq!(one("SELECT to_char(12345.678, '99999.99')"), " 12345.68");
    assert_eq!(one("SELECT to_char(5, '999')"), "   5");
    assert_eq!(one("SELECT to_char(-5, '999')"), "  -5");
    assert_eq!(one("SELECT to_char(7, '000')"), " 007");
    assert_eq!(one("SELECT to_char(1234567, '9,999,999')"), " 1,234,567");
    assert_eq!(one("SELECT to_char(0.5, 'FM0.99')"), "0.5");
    assert_eq!(one("SELECT to_char(-12.5, '999D9PR')"), "<12.5>");
    assert_eq!(one("SELECT to_char(2, '9V999999999')"), " 2000000000");
    assert_eq!(one("SELECT to_char(12345, '999')"), " ####");
    assert_eq!(one("SELECT to_char(1.5, '9.9') || '|'"), " 1.5|");
    assert_eq!(one("SELECT to_number('-34,338,492', '99G999G999')"), "-34338492");
    assert_eq!(one("SELECT to_number('12.5', '99.9')"), "12.5");
    assert!(db.query("SELECT to_number('abc', '999')").is_err());
}

#[test]
fn schemas_qualify_table_names() {
    let mut db = Database::in_memory();
    assert!(db.execute("CREATE TABLE s.t (a INT)").is_err());
    db.execute("CREATE SCHEMA s").unwrap();
    assert!(db.execute("CREATE SCHEMA s").is_err());
    db.execute("CREATE SCHEMA IF NOT EXISTS s").unwrap();
    db.execute("CREATE TABLE s.t (a INT); CREATE TABLE public.u (b INT)").unwrap();
    db.execute("INSERT INTO s.t VALUES (1), (2); INSERT INTO u VALUES (5)").unwrap();
    assert_eq!(db.query("SELECT sum(a) FROM s.t").unwrap().rows[0][0].to_string(), "3");
    assert_eq!(db.query("SELECT t.a FROM s.t ORDER BY a DESC").unwrap().rows[0][0].to_string(), "2");
    assert_eq!(db.query("SELECT b FROM public.u").unwrap().rows[0][0].to_string(), "5");
    assert!(db.execute("DROP SCHEMA s").is_err());
    db.execute("DROP SCHEMA s CASCADE").unwrap();
    assert!(db.query("SELECT * FROM s.t").is_err());
    db.execute("DROP SCHEMA IF EXISTS s").unwrap();
}

#[test]
fn domains_and_enums_expand_into_columns() {
    let mut db = Database::in_memory();
    db.execute("CREATE DOMAIN posint AS INTEGER NOT NULL DEFAULT 1 CHECK (VALUE > 0)").unwrap();
    db.execute("CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy')").unwrap();
    db.execute("CREATE TABLE p (id posint, m mood)").unwrap();
    db.execute("INSERT INTO p (m) VALUES ('ok')").unwrap();
    assert_eq!(db.query("SELECT id FROM p").unwrap().rows[0][0].to_string(), "1");
    assert!(db.execute("INSERT INTO p VALUES (0, 'ok')").is_err());
    assert!(db.execute("INSERT INTO p VALUES (NULL, 'ok')").is_err());
    assert!(db.execute("INSERT INTO p VALUES (3, 'angry')").is_err());
    db.execute("INSERT INTO p VALUES (3, 'happy')").unwrap();
    assert_eq!(db.query("SELECT '7'::posint + 1").unwrap().rows[0][0].to_string(), "8");
    assert!(db.execute("CREATE DOMAIN posint AS INTEGER").is_err());
    db.execute("DROP DOMAIN posint").unwrap();
    db.execute("DROP TYPE mood").unwrap();
    assert!(db.execute("CREATE TABLE q (id posint)").is_err());
    db.execute("DROP TYPE IF EXISTS mood").unwrap();
}

#[test]
fn types_survive_the_script_that_makes_them() {
    let mut db = Database::in_memory();
    db.execute("CREATE TYPE c AS ENUM ('r', 'g'); CREATE TABLE t (x c); INSERT INTO t VALUES ('r')").unwrap();
    assert!(db.execute("INSERT INTO t VALUES ('b')").is_err());
}

#[test]
fn extensions_are_recorded() {
    let mut db = Database::in_memory();
    db.execute("CREATE EXTENSION IF NOT EXISTS pgcrypto").unwrap();
    assert!(db.execute("CREATE EXTENSION pgcrypto").is_err());
    db.execute("DROP EXTENSION pgcrypto").unwrap();
    assert!(db.execute("DROP EXTENSION pgcrypto").is_err());
}

#[test]
fn roles_and_privileges() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE secret (a INT); INSERT INTO secret VALUES (1); CREATE VIEW v AS SELECT a FROM secret").unwrap();
    db.execute("CREATE ROLE alice LOGIN; CREATE USER bob PASSWORD 'x' SUPERUSER; CREATE ROLE staff").unwrap();
    assert!(db.execute("CREATE ROLE alice").is_err());
    db.execute("SET ROLE alice").unwrap();
    assert!(db.query("SELECT * FROM secret").is_err());
    assert!(db.execute("INSERT INTO secret VALUES (2)").is_err());
    db.execute("RESET ROLE").unwrap_or_default();
    db.execute("SET ROLE NONE").unwrap();
    db.execute("GRANT SELECT ON secret TO alice").unwrap();
    db.execute("SET ROLE alice").unwrap();
    assert_eq!(db.query("SELECT a FROM secret").unwrap().rows.len(), 1);
    assert!(db.execute("INSERT INTO secret VALUES (2)").is_err());
    db.execute("SET ROLE NONE").unwrap();
    db.execute("GRANT ALL ON secret TO staff; GRANT staff TO alice").unwrap();
    db.execute("SET ROLE alice; INSERT INTO secret VALUES (3); SET ROLE NONE").unwrap();
    db.execute("REVOKE ALL ON secret FROM staff; REVOKE SELECT ON secret FROM alice").unwrap();
    db.execute("SET ROLE alice").unwrap();
    assert!(db.query("SELECT * FROM secret").is_err());
    db.execute("SET ROLE NONE; GRANT SELECT ON v TO alice; SET ROLE alice").unwrap();
    assert_eq!(db.query("SELECT a FROM v").unwrap().rows.len(), 2);
    db.execute("SET ROLE bob").unwrap();
    assert_eq!(db.query("SELECT a FROM secret").unwrap().rows.len(), 2);
    db.execute("SET ROLE NONE").unwrap();
    assert!(db.execute("GRANT SELECT ON secret TO nobody").is_err());
    db.execute("ALTER ROLE alice NOLOGIN; DROP ROLE staff, alice").unwrap();
    assert!(db.execute("SET ROLE alice").is_err());
}

#[test]
fn postgres_sql_functions() {
    let mut db = Database::in_memory();
    db.execute("CREATE FUNCTION add(a int, b int) RETURNS int AS $$ SELECT a + b $$ LANGUAGE sql IMMUTABLE").unwrap();
    db.execute("CREATE FUNCTION twice(int) RETURNS int AS 'SELECT $1 * 2' LANGUAGE sql").unwrap();
    db.execute("CREATE TABLE t (id int, v int); INSERT INTO t VALUES (1, 10), (2, 20)").unwrap();
    db.execute("CREATE FUNCTION total(lim int) RETURNS int AS $f$ SELECT sum(v)::int FROM t WHERE t.id <= lim $f$ LANGUAGE sql").unwrap();
    let one = |db: &Database, sql: &str| db.query(sql).unwrap().rows[0][0].to_string();
    assert_eq!(one(&db, "SELECT add(2, 3)"), "5");
    assert_eq!(one(&db, "SELECT twice(21)"), "42");
    assert_eq!(one(&db, "SELECT add(id, v) FROM t ORDER BY id"), "11");
    assert_eq!(one(&db, "SELECT total(1)"), "10");
    assert_eq!(one(&db, "SELECT total(2)"), "30");
    db.execute("CREATE OR REPLACE FUNCTION add(a int, b int) RETURNS int AS $$ SELECT a + b + 1 $$ LANGUAGE sql").unwrap();
    assert_eq!(one(&db, "SELECT add(2, 3)"), "6");
    db.execute("DROP FUNCTION add(int, int)").unwrap();
    assert!(db.query("SELECT add(1, 2)").is_err());
    db.execute("DROP FUNCTION IF EXISTS add(int, int)").unwrap();
    assert!(db.execute("CREATE FUNCTION s() RETURNS SETOF int AS $$ SELECT 1 $$ LANGUAGE sql").is_err());
    assert!(db.execute("CREATE FUNCTION x() RETURNS int AS $$ SELECT 1 $$ LANGUAGE perl").is_err());
}

#[test]
fn plpgsql_functions_and_procedures() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE FUNCTION fact(n int) RETURNS int AS $$
         DECLARE r int := 1;
         BEGIN
           WHILE n > 1 LOOP r := r * n; n := n - 1; END LOOP;
           RETURN r;
         END $$ LANGUAGE plpgsql",
    )
    .unwrap();
    db.execute(
        "CREATE FUNCTION kind(n int) RETURNS text AS $$
         BEGIN
           IF n < 0 THEN RETURN 'negative';
           ELSIF n = 0 THEN RETURN 'zero';
           ELSE RETURN 'positive';
           END IF;
         END; $$ LANGUAGE plpgsql",
    )
    .unwrap();
    db.execute(
        "CREATE FUNCTION sum_to(n int) RETURNS int AS $$
         DECLARE s int := 0;
         BEGIN
           FOR i IN 1..n LOOP s := s + i; END LOOP;
           RETURN s;
         END $$ LANGUAGE plpgsql",
    )
    .unwrap();
    db.execute("CREATE FUNCTION inc(x int) RETURNS int AS $$ BEGIN RETURN x + 1; END $$ LANGUAGE plpgsql").unwrap();
    db.execute("CREATE TABLE log (msg text)").unwrap();
    db.execute(
        "CREATE PROCEDURE note(m text) AS $$ BEGIN INSERT INTO log VALUES (m); END $$ LANGUAGE plpgsql",
    )
    .unwrap();
    db.execute(
        "CREATE FUNCTION checked(n int) RETURNS int AS $$
         BEGIN
           IF n > 10 THEN RAISE EXCEPTION 'too big: %', n; END IF;
           RETURN n;
         END $$ LANGUAGE plpgsql",
    )
    .unwrap();
    db.execute("CREATE TABLE q (id int, v int); INSERT INTO q VALUES (1, 5), (2, 9)").unwrap();
    db.execute(
        "CREATE FUNCTION biggest() RETURNS int AS $$
         DECLARE m int;
         BEGIN
           SELECT max(v) INTO m FROM q;
           RETURN m;
         END $$ LANGUAGE plpgsql",
    )
    .unwrap();
    let one = |db: &mut Database, sql: &str| db.execute(sql).unwrap().rows[0][0].to_string();
    assert_eq!(one(&mut db, "SELECT fact(5)"), "120");
    assert_eq!(one(&mut db, "SELECT kind(-3)"), "negative");
    assert_eq!(one(&mut db, "SELECT kind(0)"), "zero");
    assert_eq!(one(&mut db, "SELECT kind(7)"), "positive");
    assert_eq!(one(&mut db, "SELECT sum_to(10)"), "55");
    assert_eq!(one(&mut db, "SELECT inc(41)"), "42");
    assert_eq!(one(&mut db, "SELECT inc(id) FROM q ORDER BY id"), "2");
    assert_eq!(one(&mut db, "SELECT checked(3)"), "3");
    let err = db.execute("SELECT checked(11)").unwrap_err().to_string();
    assert!(err.contains("too big: 11"), "{err}");
    assert_eq!(one(&mut db, "SELECT biggest()"), "9");
    db.execute("CALL note('hello')").unwrap();
    assert_eq!(one(&mut db, "SELECT msg FROM log"), "hello");
    assert!(db.execute("CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; EXCEPTION WHEN others THEN RETURN 2; END $$ LANGUAGE plpgsql").is_err());
    assert!(db.execute("CREATE FUNCTION g() RETURNS SETOF int AS $$ BEGIN RETURN 1; END $$ LANGUAGE plpgsql").is_err());
}

#[test]
fn inheritance_reads_children_with_the_parent() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE cities (name text, pop int); CREATE TABLE capitals (state text) INHERITS (cities)").unwrap();
    db.execute("INSERT INTO cities VALUES ('Oakland', 400); INSERT INTO capitals VALUES ('Sacramento', 500, 'CA')").unwrap();
    let n = |db: &Database, sql: &str| db.query(sql).unwrap().rows.len();
    assert_eq!(n(&db, "SELECT * FROM cities"), 2);
    assert_eq!(n(&db, "SELECT * FROM ONLY cities"), 1);
    assert_eq!(db.query("SELECT * FROM capitals").unwrap().columns, vec!["name", "pop", "state"]);
    assert_eq!(db.query("SELECT * FROM cities").unwrap().columns, vec!["name", "pop"]);
    db.execute("UPDATE cities SET pop = pop + 1").unwrap();
    assert_eq!(db.query("SELECT sum(pop) FROM cities").unwrap().rows[0][0].to_string(), "902");
    db.execute("UPDATE ONLY cities SET pop = 0").unwrap();
    assert_eq!(db.query("SELECT sum(pop) FROM cities").unwrap().rows[0][0].to_string(), "501");
    db.execute("DELETE FROM ONLY cities").unwrap();
    assert_eq!(n(&db, "SELECT * FROM cities"), 1);
    db.execute("DELETE FROM cities").unwrap();
    assert_eq!(n(&db, "SELECT * FROM capitals"), 0);
    assert!(db.execute("DROP TABLE cities").is_err());
    db.execute("DROP TABLE cities CASCADE").unwrap();
    assert!(db.query("SELECT * FROM capitals").is_err());
}

#[test]
fn range_list_and_hash_partitions_take_inserts() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE m (d int, v text) PARTITION BY RANGE (d);
         CREATE TABLE m1 PARTITION OF m FOR VALUES FROM (0) TO (10);
         CREATE TABLE m2 PARTITION OF m FOR VALUES FROM (10) TO (MAXVALUE);
         INSERT INTO m VALUES (5, 'a'), (10, 'b'), (99, 'c')",
    )
    .unwrap();
    let c = |db: &Database, t: &str| db.query(&format!("SELECT count(*) FROM ONLY {t}")).unwrap().rows[0][0].to_string();
    assert_eq!(c(&db, "m"), "0");
    assert_eq!(c(&db, "m1"), "1");
    assert_eq!(c(&db, "m2"), "2");
    assert_eq!(db.query("SELECT count(*) FROM m").unwrap().rows[0][0].to_string(), "3");
    assert!(db.execute("INSERT INTO m VALUES (-1, 'x')").is_err());
    db.execute("CREATE TABLE l (k text) PARTITION BY LIST (k); CREATE TABLE la PARTITION OF l FOR VALUES IN ('a', 'b'); CREATE TABLE ld PARTITION OF l DEFAULT").unwrap();
    db.execute("INSERT INTO l VALUES ('a'), ('z')").unwrap();
    assert_eq!(c(&db, "la"), "1");
    assert_eq!(c(&db, "ld"), "1");
    db.execute(
        "CREATE TABLE h (k int) PARTITION BY HASH (k);
         CREATE TABLE h0 PARTITION OF h FOR VALUES WITH (MODULUS 2, REMAINDER 0);
         CREATE TABLE h1 PARTITION OF h FOR VALUES WITH (MODULUS 2, REMAINDER 1);
         INSERT INTO h VALUES (1), (2), (3), (4), (5), (6)",
    )
    .unwrap();
    assert_eq!(db.query("SELECT count(*) FROM h").unwrap().rows[0][0].to_string(), "6");
    assert_eq!(c(&db, "h0").parse::<i32>().unwrap() + c(&db, "h1").parse::<i32>().unwrap(), 6);
    assert!(db.execute("CREATE TABLE z PARTITION OF m2 FOR VALUES FROM (1) TO (2)").is_err());
    db.execute("DROP TABLE m, l, h CASCADE").unwrap();
    assert!(db.query("SELECT * FROM m1").is_err());
}

#[test]
fn drop_table_lists_and_cascade() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE a (id int PRIMARY KEY); CREATE TABLE b (x int REFERENCES a(id)); CREATE VIEW vb AS SELECT * FROM a").unwrap();
    db.execute("DROP TABLE IF EXISTS nope, a CASCADE").unwrap();
    assert!(db.query("SELECT * FROM vb").is_err());
    db.execute("INSERT INTO b VALUES (1)").unwrap();
    db.execute("DROP TABLE b, nope").unwrap_err();
}

#[test]
fn postgres_conveniences() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE s (a int, b text DEFAULT 'x'); INSERT INTO s VALUES (1, 'p'), (2, DEFAULT), (DEFAULT, 'q')").unwrap();
    let one = |db: &Database, sql: &str| db.query(sql).unwrap().rows[0][0].to_string();
    assert_eq!(one(&db, "SELECT b FROM s WHERE a = 2"), "x");
    assert_eq!(one(&db, "SELECT count(*) FROM s WHERE a IS NULL"), "1");
    db.execute("CREATE TABLE c AS SELECT a, b FROM s WHERE a IS NOT NULL").unwrap();
    assert_eq!(one(&db, "SELECT count(*) FROM c"), "2");
    db.execute("CREATE TEMP TABLE e AS SELECT * FROM s WITH NO DATA; CREATE UNLOGGED TABLE u (z int)").unwrap();
    assert_eq!(one(&db, "SELECT count(*) FROM e"), "0");
    db.execute("CREATE TABLE parent (id int); CREATE TABLE kid () INHERITS (parent)").unwrap();
    assert_eq!(db.query("SELECT * FROM kid").unwrap().columns, vec!["id"]);
    assert_eq!(one(&db, "SELECT 'abc' ~ 'b'"), "Yes");
    assert_eq!(one(&db, "SELECT 'abc' ~* 'B'"), "Yes");
    assert_eq!(one(&db, "SELECT 'abc' !~ 'z'"), "Yes");
    assert_eq!(one(&db, "SELECT @ -5"), "5");
    db.execute("VACUUM; VACUUM FULL s; ANALYZE s; COMMENT ON TABLE s IS 'x'; LOCK TABLE s IN ACCESS SHARE MODE").unwrap();
}

#[test]
fn trigger_functions() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE t (id int, name text, stamp text); CREATE TABLE audit (op text, id int)").unwrap();
    db.execute(
        "CREATE FUNCTION fill() RETURNS trigger AS $$
         BEGIN
           IF NEW.name IS NULL THEN NEW.name := 'anon'; END IF;
           NEW.stamp := TG_OP || ':' || TG_WHEN;
           RETURN NEW;
         END $$ LANGUAGE plpgsql",
    )
    .unwrap();
    db.execute("CREATE TRIGGER t_fill BEFORE INSERT OR UPDATE ON t FOR EACH ROW EXECUTE FUNCTION fill()").unwrap();
    db.execute("INSERT INTO t (id) VALUES (1)").unwrap();
    db.execute("INSERT INTO t VALUES (2, 'bob', NULL)").unwrap();
    let r = db.query("SELECT name, stamp FROM t ORDER BY id").unwrap();
    assert_eq!(r.rows[0][0].to_string(), "anon");
    assert_eq!(r.rows[0][1].to_string(), "INSERT:BEFORE");
    assert_eq!(r.rows[1][0].to_string(), "bob");
    db.execute("UPDATE t SET name = NULL WHERE id = 2").unwrap();
    let r = db.query("SELECT name, stamp FROM t WHERE id = 2").unwrap();
    assert_eq!((r.rows[0][0].to_string(), r.rows[0][1].to_string()), ("anon".into(), "UPDATE:BEFORE".into()));

    db.execute("CREATE FUNCTION log_it() RETURNS trigger AS $$ BEGIN INSERT INTO audit VALUES (TG_OP, OLD.id); RETURN NULL; END $$ LANGUAGE plpgsql").unwrap();
    db.execute("CREATE TRIGGER t_del AFTER DELETE ON t FOR EACH ROW EXECUTE PROCEDURE log_it()").unwrap();
    db.execute("DELETE FROM t WHERE id = 1").unwrap();
    assert_eq!(db.query("SELECT op, id FROM audit").unwrap().rows[0][0].to_string(), "DELETE");

    db.execute("CREATE FUNCTION no_odd() RETURNS trigger AS $$ BEGIN IF NEW.id % 2 = 1 THEN RETURN NULL; END IF; RETURN NEW; END $$ LANGUAGE plpgsql").unwrap();
    db.execute("CREATE TABLE ev (id int); CREATE TRIGGER ev_t BEFORE INSERT ON ev FOR EACH ROW EXECUTE FUNCTION no_odd()").unwrap();
    db.execute("INSERT INTO ev VALUES (1), (2), (3), (4)").unwrap();
    assert_eq!(db.query("SELECT count(*) FROM ev").unwrap().rows[0][0].to_string(), "2");

    db.execute("CREATE FUNCTION nope() RETURNS trigger AS $$ BEGIN RAISE EXCEPTION 'no way: %', NEW.id; END $$ LANGUAGE plpgsql").unwrap();
    db.execute("CREATE TABLE r (id int); CREATE TRIGGER r_t BEFORE INSERT ON r FOR EACH ROW EXECUTE FUNCTION nope()").unwrap();
    let e = db.execute("INSERT INTO r VALUES (7)").unwrap_err().to_string();
    assert!(e.contains("no way: 7"), "{e}");
}

#[test]
fn partitions_are_pruned_by_the_where_clause() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE m (d int, v int) PARTITION BY RANGE (d);
         CREATE TABLE m1 PARTITION OF m FOR VALUES FROM (0) TO (10);
         CREATE TABLE m2 PARTITION OF m FOR VALUES FROM (10) TO (20);
         CREATE TABLE m3 PARTITION OF m FOR VALUES FROM (20) TO (MAXVALUE);
         INSERT INTO m VALUES (1, 1), (12, 2), (25, 3), (30, 4);
         CREATE TABLE l (k text) PARTITION BY LIST (k);
         CREATE TABLE la PARTITION OF l FOR VALUES IN ('a', 'b');
         CREATE TABLE lc PARTITION OF l FOR VALUES IN ('c');
         INSERT INTO l VALUES ('a'), ('c')",
    )
    .unwrap();
    let plan = |db: &Database, sql: &str| -> String {
        db.query(&format!("EXPLAIN {sql}")).unwrap().rows.iter().map(|r| r[0].to_string()).collect::<Vec<_>>().join("\n")
    };
    assert!(plan(&db, "SELECT * FROM m WHERE d = 12").contains("2 of 4 tables"), "{}", plan(&db, "SELECT * FROM m WHERE d = 12"));
    assert!(plan(&db, "SELECT * FROM m WHERE d >= 20").contains("2 of 4 tables"));
    assert!(plan(&db, "SELECT * FROM m WHERE d < 10").contains("2 of 4 tables"));
    assert!(plan(&db, "SELECT * FROM m WHERE d BETWEEN 5 AND 12 AND v > 0").contains("3 of 4 tables"));
    assert!(plan(&db, "SELECT * FROM m x WHERE x.d IN (1, 25)").contains("3 of 4 tables"));
    assert!(!plan(&db, "SELECT * FROM m WHERE v = 2").contains("of 4 tables"));
    assert!(!plan(&db, "SELECT * FROM m WHERE d = 1 OR d = 25").contains("of 4 tables"));
    assert!(plan(&db, "SELECT * FROM l WHERE k = 'c'").contains("2 of 3 tables"));
    let n = |db: &Database, sql: &str| db.query(sql).unwrap().rows.len();
    assert_eq!(n(&db, "SELECT * FROM m WHERE d = 12"), 1);
    assert_eq!(n(&db, "SELECT * FROM m WHERE d >= 12"), 3);
    assert_eq!(n(&db, "SELECT * FROM m WHERE d IN (1, 30)"), 2);
    assert_eq!(n(&db, "SELECT * FROM l WHERE k = 'c'"), 1);
    db.execute("DELETE FROM m WHERE d = 25").unwrap();
    db.execute("UPDATE m SET v = 0 WHERE d < 10").unwrap();
    assert_eq!(n(&db, "SELECT * FROM m"), 3);
    db.execute("SET optimizer TO off").unwrap();
    assert!(!plan(&db, "SELECT * FROM m WHERE d = 12").contains("of 4 tables"));
    assert_eq!(n(&db, "SELECT * FROM m WHERE d = 12"), 1);
}

#[test]
fn postgres_statement_forms() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE t (a int, b text, c regclass, d inet, e int4range, f point)").unwrap();
    db.execute("CREATE INDEX ON t (a); CREATE INDEX CONCURRENTLY IF NOT EXISTS t_b ON t USING btree (b text_pattern_ops DESC NULLS LAST)").unwrap();
    assert!(db.execute("CREATE INDEX ON t (lower(b))").is_err());
    db.execute("CREATE TABLE u (LIKE t INCLUDING ALL, z int)").unwrap();
    assert_eq!(db.query("SELECT * FROM u").unwrap().columns.len(), 7);
    db.execute("START TRANSACTION ISOLATION LEVEL SERIALIZABLE, READ ONLY; INSERT INTO t (a) VALUES (1); COMMIT AND CHAIN").unwrap();
    db.execute("BEGIN; SELECT 1; ABORT; BEGIN TRANSACTION READ WRITE; END").unwrap();
    db.execute("RESET SESSION AUTHORIZATION; SELECT pg_catalog.lower('X'), public.upper('y')").unwrap();
    db.execute("ALTER TABLE t ENABLE ROW LEVEL SECURITY; ALTER TABLE ONLY t ADD COLUMN g int, ADD COLUMN h int; ALTER TABLE t OWNER TO nobody; ALTER TABLE t SET (fillfactor = 70)").unwrap();
    db.execute("ALTER TABLE t ALTER COLUMN g SET STATISTICS 100; ALTER TABLE t ALTER COLUMN g SET DATA TYPE bigint; ALTER TABLE IF EXISTS nope ADD COLUMN x int").unwrap();
    assert_eq!(db.query("SELECT * FROM t").unwrap().columns.len(), 8);
    db.execute("CREATE TABLESPACE ts LOCATION '/tmp'; CREATE PUBLICATION p FOR ALL TABLES; DROP PUBLICATION p; CREATE STATISTICS s ON a, b FROM t").unwrap();
    db.execute("CREATE MATERIALIZED VIEW mv AS SELECT a FROM t WHERE a IS NOT NULL").unwrap();
    assert_eq!(db.query("SELECT count(*) FROM mv").unwrap().rows[0][0].to_string(), "1");
    db.execute("DROP MATERIALIZED VIEW mv").unwrap();
    db.execute(
        "CREATE TABLE p (k int) PARTITION BY RANGE (k); CREATE TABLE p1 (k int); CREATE TABLE p2 (k int);
         ALTER TABLE p ATTACH PARTITION p1 FOR VALUES FROM (0) TO (10);
         ALTER TABLE p ATTACH PARTITION p2 FOR VALUES FROM (10) TO (20);
         INSERT INTO p VALUES (3), (15)",
    )
    .unwrap();
    assert_eq!(db.query("SELECT count(*) FROM ONLY p1").unwrap().rows[0][0].to_string(), "1");
    db.execute("ALTER TABLE p DETACH PARTITION p1").unwrap();
    assert_eq!(db.query("SELECT count(*) FROM p").unwrap().rows[0][0].to_string(), "1");
}

fn big(db: &mut Database, name: &str, n: i64) {
    db.execute(&format!("CREATE TABLE {name} (u1 int, u2 int, h int)")).unwrap();
    let rows: Vec<String> = (0..n).map(|i| format!("({i}, {}, {})", (i * 7) % n, i % 100)).collect();
    db.execute(&format!("INSERT INTO {name} VALUES {}", rows.join(","))).unwrap();
}

#[test]
fn time_limits_and_plans_that_would_not_finish() {
    let mut db = Database::in_memory();
    big(&mut db, "a", 3000);
    big(&mut db, "b", 3000);
    db.execute("SET statement_timeout = '50ms'").unwrap();
    let e = db.execute("SELECT count(*) FROM a x, b y WHERE x.u1 + y.u1 > 0 AND x.h + y.h > -1").unwrap_err().to_string();
    assert!(e.contains("statement timeout"), "{e}");
    db.execute("RESET statement_timeout").unwrap();
    // a plain EXPLAIN stops by itself
    let t = std::time::Instant::now();
    let plan = db.query("EXPLAIN SELECT count(*) FROM a x, b y, a z WHERE x.u1 + y.u1 + z.u1 > -1").unwrap();
    assert!(t.elapsed().as_secs() < 30);
    let text: Vec<String> = plan.rows.iter().map(|r| r[0].to_string()).collect();
    assert!(!text.is_empty());
}

#[test]
fn or_conditions_filter_each_table_before_the_join() {
    let mut db = Database::in_memory();
    big(&mut db, "a", 4000);
    big(&mut db, "b", 4000);
    let t = std::time::Instant::now();
    let r = db
        .query("SELECT count(*) FROM a x JOIN b y ON (x.u1 = 1 AND y.u1 = 2) OR (x.u2 = 7 AND y.h = 4)")
        .unwrap();
    assert!(t.elapsed().as_secs() < 5);
    // x.u1 = 1 with y.u1 = 2: one pair; x.u2 = 7 (u1 = 1) with y.h = 4: 40 rows of b
    assert_eq!(r.rows[0][0].to_string(), "41");
}

#[test]
fn conditions_move_into_derived_tables() {
    let mut db = Database::in_memory();
    big(&mut db, "a", 3000);
    let t = std::time::Instant::now();
    let r = db.query("SELECT x, count(*) FROM (SELECT p.u1 AS x, q.u1 AS y FROM a p, a q) s WHERE x = 5 GROUP BY x").unwrap();
    assert!(t.elapsed().as_secs() < 5);
    assert_eq!(r.rows[0][1].to_string(), "3000");
    let plan = db.query("EXPLAIN SELECT x, count(*) FROM (SELECT p.u1 AS x, q.u1 AS y FROM a p, a q) s WHERE x < 3 GROUP BY x").unwrap();
    assert!(plan.rows.iter().any(|r| r[0].to_string().contains("moved into its query")));
    // conditions that can match NULL stay outside
    db.execute("CREATE TABLE n (v int); INSERT INTO n VALUES (1), (NULL)").unwrap();
    assert_eq!(db.query("SELECT count(*) FROM (SELECT v AS w FROM n) s WHERE w IS NULL").unwrap().rows[0][0].to_string(), "1");
}

#[test]
fn lateral_results_are_looked_up_by_the_join_condition() {
    let mut db = Database::in_memory();
    big(&mut db, "a", 3000);
    let t = std::time::Instant::now();
    let r = db
        .query("SELECT count(*) FROM a t1 LEFT JOIN LATERAL (SELECT t1.h + t2.h AS x, t2.u2 AS k FROM a t2) ss ON t1.u1 = ss.k")
        .unwrap();
    assert!(t.elapsed().as_secs() < 20);
    assert_eq!(r.rows[0][0].to_string(), "3000");
}

#[test]
fn recursive_with_inside_a_recursive_part_is_refused() {
    let mut db = Database::in_memory();
    let e = db.execute("WITH RECURSIVE q AS (SELECT 1 AS n UNION ALL (WITH RECURSIVE x AS (SELECT n FROM q UNION ALL SELECT n FROM x) SELECT n FROM x)) SELECT * FROM q");
    assert!(e.is_err());
}

#[test]
fn more_postgres_functions() {
    let db = Database::in_memory();
    let one = |sql: &str| db.query(sql).unwrap().rows[0][0].to_string();
    assert_eq!(one("SELECT width_bucket(5.35, 0.024, 10.06, 5)"), "3");
    assert_eq!(one("SELECT width_bucket(-1, 0, 10, 5)"), "0");
    assert_eq!(one("SELECT width_bucket(11, 0, 10, 5)"), "6");
    assert_eq!(one("SELECT encode('abc', 'hex')"), "616263");
    assert_eq!(one("SELECT encode('abcd', 'base64')"), "YWJjZA==");
    assert_eq!(one("SELECT decode('YWJjZA==', 'base64')"), "abcd");
    assert_eq!(one("SELECT decode('616263', 'hex')"), "abc");
    assert_eq!(one("SELECT left('ahoj', -1) || '|' || right('ahoj', -1)"), "aho|hoj");
    assert_eq!(one("SELECT to_regclass('x')"), "x");
}

#[test]
fn where_on_an_outer_joined_table_filters_it_first_and_keeps_null_rows() {
    let mut db = Database::in_memory();
    big(&mut db, "a", 2000);
    db.execute("CREATE TABLE s (k int); INSERT INTO s VALUES (1), (2)").unwrap();
    // a join keeps its answer: null-extended rows are still dropped by the WHERE, IS NULL still finds them
    assert_eq!(db.query("SELECT count(*) FROM s LEFT JOIN a ON a.u1 = s.k WHERE a.u2 > 0").unwrap().rows[0][0].to_string(), "2");
    assert_eq!(db.query("SELECT count(*) FROM s LEFT JOIN a ON a.u1 = s.k + 100000 WHERE a.u2 IS NULL").unwrap().rows[0][0].to_string(), "2");
    let t = std::time::Instant::now();
    let r = db.query("SELECT count(*) FROM a t1 LEFT JOIN LATERAL (SELECT t1.h + 1 AS c1, t2.h AS c2 FROM a t2) s ON TRUE WHERE s.c1 = s.c2 AND t1.u1 < 100").unwrap();
    assert!(t.elapsed().as_secs() < 20);
    assert!(r.rows[0][0].to_string().parse::<i64>().unwrap() > 0);
}

#[test]
fn or_of_equalities_is_joined_through_lookups() {
    let mut db = Database::in_memory();
    big(&mut db, "a", 6000);
    big(&mut db, "b", 6000);
    let t = std::time::Instant::now();
    let r = db.query("SELECT count(*) FROM a x, b y WHERE y.u1 = x.u2 OR y.u1 = x.h OR y.u2 = x.u1").unwrap();
    assert!(t.elapsed().as_secs() < 10, "{:?}", t.elapsed());
    // the same by a plain nested loop on a small slice
    let slow = db.query("SELECT count(*) FROM (SELECT * FROM a WHERE u1 < 50) x, b y WHERE y.u1 = x.u2 OR y.u1 = x.h OR y.u2 = x.u1 + 0 * y.h").unwrap();
    let fast = db.query("SELECT count(*) FROM (SELECT * FROM a WHERE u1 < 50) x, b y WHERE y.u1 = x.u2 OR y.u1 = x.h OR y.u2 = x.u1").unwrap();
    assert_eq!(slow.rows, fast.rows);
    assert!(r.rows[0][0].to_string().parse::<i64>().unwrap() > 6000);
}

#[test]
fn a_lateral_item_is_joined_as_soon_as_its_tables_are_there() {
    let mut db = Database::in_memory();
    big(&mut db, "a", 3000);
    big(&mut db, "b", 3000);
    db.execute("CREATE INDEX b_u2 ON b (u2)").unwrap();
    let t = std::time::Instant::now();
    let r = db.query("SELECT count(*) FROM a, b JOIN LATERAL (VALUES (a.u1)) ss(x) ON b.u2 = ss.x").unwrap();
    assert!(t.elapsed().as_secs() < 10, "{:?}", t.elapsed());
    assert_eq!(r.rows[0][0].to_string(), "3000");
    let r = db.query("SELECT count(*) FROM a, b JOIN LATERAL (VALUES (a.u1), (-1)) ss(x) ON b.u2 = ss.x").unwrap();
    assert_eq!(r.rows[0][0].to_string(), "3000");
}

#[test]
fn json_path_queries() {
    let mut db = Database::in_memory();
    let all = |db: &mut Database, sql: &str| -> Vec<String> { db.execute(sql).unwrap().rows.iter().map(|r| r[0].to_string()).collect() };
    let d = "'{\"a\": [1, 2, 3, 4], \"b\": {\"c\": \"x\"}, \"n\": null}'::jsonb";
    assert_eq!(all(&mut db, &format!("SELECT jsonb_path_query({d}, '$.a[*] ? (@ > 2)')")), vec!["3", "4"]);
    assert_eq!(all(&mut db, &format!("SELECT * FROM jsonb_path_query({d}, '$.b.c')")), vec!["\"x\""]);
    assert_eq!(all(&mut db, &format!("SELECT jsonb_path_query_array({d}, '$.a[*] ? (@ >= 2 && @ < 4)')")), vec!["[2,3]"]);
    assert_eq!(all(&mut db, &format!("SELECT jsonb_path_query_first({d}, '$.a[last]')")), vec!["4"]);
    assert_eq!(all(&mut db, &format!("SELECT jsonb_path_exists({d}, '$.b.zz')")), vec!["No"]);
    assert_eq!(all(&mut db, &format!("SELECT {d} @? '$.a[1 to 2]'")), vec!["Yes"]);
    assert_eq!(all(&mut db, &format!("SELECT {d} @@ '$.a[0] == 1'")), vec!["Yes"]);
    assert_eq!(all(&mut db, &format!("SELECT jsonb_path_query({d}, '$.a.size()')")), vec!["4"]);
    assert_eq!(all(&mut db, &format!("SELECT jsonb_path_query({d}, '$.a[*] ? (@ > $x)', '{{\"x\": 3}}')")), vec!["4"]);
    assert_eq!(all(&mut db, &format!("SELECT jsonb_path_query({d}, '$.a[0] + 10')")), vec!["11"]);
    assert_eq!(all(&mut db, &format!("SELECT jsonb_path_query({d}, '$.b.c ? (@ like_regex \"^x\")')")), vec!["\"x\""]);
    assert!(db.execute(&format!("SELECT jsonb_path_query({d}, 'strict $.nope')")).is_err());
    assert_eq!(all(&mut db, &format!("SELECT jsonb_path_query({d}, 'strict $.nope', '{{}}', true)")).len(), 0);
    assert_eq!(all(&mut db, &format!("SELECT jsonb_path_query({d}, 'lax $.nope')")).len(), 0);
    assert!(db.execute("SELECT jsonb_path_query('{}', '$.a[')").is_err());
    assert_eq!(all(&mut db, "SELECT generate_series(1, 3)"), vec!["1", "2", "3"]);
}

#[test]
fn rules_are_row_triggers() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE base (id int, v text); CREATE TABLE log (id int, what text); CREATE TABLE archive (id int, v text)").unwrap();
    db.execute("CREATE RULE r_log AS ON INSERT TO base DO ALSO INSERT INTO log VALUES (new.id, 'ins')").unwrap();
    db.execute("CREATE RULE r_arch AS ON DELETE TO base DO (INSERT INTO archive VALUES (old.id, old.v); INSERT INTO log VALUES (old.id, 'del'))").unwrap();
    db.execute("INSERT INTO base VALUES (1, 'a'), (2, 'b')").unwrap();
    assert_eq!(db.query("SELECT count(*) FROM log").unwrap().rows[0][0].to_string(), "2");
    db.execute("DELETE FROM base WHERE id = 1").unwrap();
    assert_eq!(db.query("SELECT v FROM archive").unwrap().rows[0][0].to_string(), "a");
    // DO INSTEAD NOTHING with a condition
    db.execute("CREATE RULE no_neg AS ON INSERT TO base WHERE new.id < 0 DO INSTEAD NOTHING").unwrap();
    db.execute("INSERT INTO base VALUES (-5, 'x'), (5, 'y')").unwrap();
    assert_eq!(db.query("SELECT count(*) FROM base").unwrap().rows[0][0].to_string(), "2");
    // DO INSTEAD a different command
    db.execute("CREATE TABLE t2 (id int); CREATE RULE redirect AS ON INSERT TO t2 DO INSTEAD INSERT INTO log VALUES (new.id, 'redirected')").unwrap();
    db.execute("INSERT INTO t2 VALUES (9)").unwrap();
    assert_eq!(db.query("SELECT count(*) FROM t2").unwrap().rows[0][0].to_string(), "0");
    assert_eq!(db.query("SELECT count(*) FROM log WHERE what = 'redirected'").unwrap().rows[0][0].to_string(), "1");
    // on a view
    db.execute("CREATE VIEW vb AS SELECT id, v FROM base; CREATE RULE vb_ins AS ON INSERT TO vb DO INSTEAD INSERT INTO base VALUES (new.id + 100, new.v)").unwrap();
    db.execute("INSERT INTO vb VALUES (1, 'z')").unwrap();
    assert_eq!(db.query("SELECT count(*) FROM base WHERE id = 101").unwrap().rows[0][0].to_string(), "1");
    db.execute("DROP RULE r_log ON base").unwrap();
    db.execute("INSERT INTO base VALUES (7, 'q')").unwrap();
    assert_eq!(db.query("SELECT count(*) FROM log WHERE id = 7").unwrap().rows[0][0].to_string(), "0");
}

#[test]
fn row_level_security() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE docs (id int, owner text, body text);
         INSERT INTO docs VALUES (1, 'alice', 'a1'), (2, 'bob', 'b1'), (3, 'alice', 'a2');
         CREATE ROLE alice; CREATE ROLE bob; CREATE ROLE boss SUPERUSER;
         GRANT ALL ON docs TO alice, bob;
         ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
         CREATE POLICY mine ON docs FOR ALL TO PUBLIC USING (owner = current_user)",
    )
    .unwrap();
    let n = |db: &mut Database, sql: &str| db.execute(sql).unwrap().rows[0][0].to_string();
    // nobody chosen: no restriction
    assert_eq!(n(&mut db, "SELECT count(*) FROM docs"), "3");
    db.execute("SET ROLE alice").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM docs"), "2");
    assert!(db.execute("INSERT INTO docs VALUES (4, 'bob', 'sneaky')").is_err());
    db.execute("INSERT INTO docs VALUES (4, 'alice', 'fine')").unwrap();
    assert_eq!(db.execute("UPDATE docs SET body = 'x' WHERE id = 2").unwrap().affected, 0);
    assert!(db.execute("UPDATE docs SET owner = 'bob' WHERE id = 1").is_err());
    assert_eq!(db.execute("DELETE FROM docs").unwrap().affected, 3);
    db.execute("SET ROLE boss").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM docs"), "1");
    db.execute("SET ROLE NONE; CREATE ROLE carol; GRANT SELECT ON docs TO carol; CREATE POLICY open ON docs FOR SELECT TO carol USING (true); CREATE POLICY nobob ON docs AS RESTRICTIVE FOR SELECT USING (owner <> 'bob'); SET ROLE carol").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM docs"), "0");
    db.execute("SET ROLE NONE; ALTER TABLE docs DISABLE ROW LEVEL SECURITY; SET ROLE carol").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM docs"), "1");
    db.execute("SET ROLE NONE; DROP POLICY mine ON docs").unwrap();
    assert!(db.execute("DROP POLICY mine ON docs").is_err());
}

#[test]
fn policies_that_read_each_other_are_an_error_not_a_loop() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE s1 (a int); CREATE TABLE s2 (b int); INSERT INTO s1 VALUES (1); INSERT INTO s2 VALUES (1);
         CREATE ROLE u; GRANT ALL ON s1, s2 TO u;
         ALTER TABLE s1 ENABLE ROW LEVEL SECURITY; ALTER TABLE s2 ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p1 ON s1 USING (a IN (SELECT b FROM s2));
         CREATE POLICY p2 ON s2 USING (b IN (SELECT a FROM s1));
         SET ROLE u",
    )
    .unwrap();
    let e = db.execute("SELECT * FROM s1").unwrap_err().to_string();
    assert!(e.contains("infinite recursion"), "{e}");
}

#[test]
fn mysql_syntax() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE `t1` (
           `id` int(11) unsigned NOT NULL AUTO_INCREMENT,
           name varchar(20) CHARACTER SET utf8 COLLATE utf8_bin DEFAULT NULL COMMENT 'x',
           kind enum('a','b') NOT NULL DEFAULT 'a',
           created timestamp NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
           big bigint(20) unsigned zerofill,
           PRIMARY KEY (`id`),
           UNIQUE KEY uname (name(10)),
           KEY k1 (kind, big DESC),
           INDEX (created)
         ) ENGINE=InnoDB AUTO_INCREMENT=5 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci COMMENT='t'",
    )
    .unwrap();
    db.execute("INSERT INTO t1 (name, kind) VALUES ('x', 'b'), ('y', 'a')").unwrap();
    assert!(db.execute("INSERT INTO t1 (name, kind) VALUES ('z', 'c')").is_err());
    assert!(db.execute("INSERT INTO t1 (name) VALUES ('x')").is_err());
    let one = |db: &mut Database, sql: &str| db.execute(sql).unwrap().rows[0][0].to_string();
    assert_eq!(one(&mut db, "SELECT count(*) FROM `t1`"), "2");
    // user and system variables
    db.execute("SET @a = 5, @b := @a + 1").unwrap();
    assert_eq!(one(&mut db, "SELECT @a * @b"), "30");
    db.execute("SELECT count(*) INTO @n FROM t1").unwrap();
    assert_eq!(one(&mut db, "SELECT @n"), "2");
    db.execute("SET sql_mode = 'ANSI', SESSION sort_buffer_size = 32804; SET @@group_concat_max_len = 10 * 1024").unwrap();
    assert_eq!(one(&mut db, "SELECT @@sql_mode"), "ANSI");
    assert_eq!(one(&mut db, "SELECT @@session.group_concat_max_len"), "10240");
    db.execute("SET NAMES utf8mb4; SET GLOBAL foreign_key_checks = 0; USE test; FLUSH TABLES; CHECK TABLE t1").unwrap();
    // SHOW
    assert_eq!(one(&mut db, "SHOW TABLES"), "t1");
    assert_eq!(db.execute("SHOW TABLES LIKE 'x%'").unwrap().rows.len(), 0);
    assert_eq!(db.execute("SHOW COLUMNS FROM t1").unwrap().rows.len(), 5);
    assert_eq!(db.execute("DESCRIBE t1").unwrap().rows[0][0].to_string(), "id");
    assert!(db.execute("SHOW INDEX FROM t1").unwrap().rows.len() >= 4);
    assert!(one(&mut db, "SHOW CREATE TABLE t1").is_empty() == false);
    assert!(db.execute("SHOW VARIABLES LIKE 'sql_mode'").unwrap().rows.len() == 1);
    db.execute("SHOW WARNINGS; SHOW ENGINES; SHOW DATABASES; SHOW FULL TABLES; SHOW PROCESSLIST; SHOW STATUS LIKE 'Uptime'").unwrap();
    assert_eq!(db.execute("SHOW TABLES WHERE Tables_in_test = 't1'").unwrap().rows.len(), 1);
    // prepared statements
    db.execute("PREPARE s FROM 'SELECT count(*) FROM t1 WHERE name = ?'").unwrap();
    db.execute("SET @v = 'x'").unwrap();
    assert_eq!(one(&mut db, "EXECUTE s USING @v"), "1");
    db.execute("SET @q = 'SELECT 7'; PREPARE s2 FROM @q").unwrap();
    assert_eq!(one(&mut db, "EXECUTE s2"), "7");
    db.execute("DEALLOCATE PREPARE s2").unwrap();
    // INSERT forms
    db.execute("INSERT IGNORE INTO t1 (id, name) VALUES (1, 'dup')").unwrap();
    db.execute("INSERT INTO t1 SET name = 'set', kind = 'b'").unwrap();
    db.execute("INSERT INTO t1 (id, name) VALUES (1, 'q') ON DUPLICATE KEY UPDATE name = VALUES(name)").unwrap();
    assert_eq!(one(&mut db, "SELECT name FROM t1 WHERE id = 1"), "q");
    db.execute("REPLACE t1 (id, name) VALUES (2, 'r')").unwrap();
    assert_eq!(one(&mut db, "SELECT name FROM t1 WHERE id = 2"), "r");
    // ALTER and DROP
    db.execute("ALTER TABLE t1 ENGINE=MyISAM, ADD COLUMN extra int AFTER name, ADD KEY (extra), CONVERT TO CHARACTER SET utf8mb4").unwrap();
    db.execute("ALTER TABLE t1 ADD COLUMN (c1 int, c2 text), MODIFY extra bigint DEFAULT 0, CHANGE c1 c1x int, DROP KEY uname").unwrap();
    db.execute("CREATE TABLE t2 LIKE t1; CREATE TABLE t3 (a int) SELECT 1 AS b; DROP TABLES t2, t3").unwrap();
    db.execute("CREATE DATABASE IF NOT EXISTS d1; DROP DATABASE d1; SELECT CURRENT_USER(), DATABASE(), VERSION()").unwrap();
}

#[test]
fn mysql_functions_and_forms() {
    let mut db = Database::in_memory();
    let one = |db: &mut Database, sql: &str| db.execute(sql).unwrap().rows[0][0].to_string();
    assert_eq!(one(&mut db, "SELECT HEX('abc'), HEX(255)"), "616263");
    assert_eq!(one(&mut db, "SELECT UNHEX('616263')"), "abc");
    assert_eq!(one(&mut db, "SELECT ASCII('A') + LOCATE('b', 'abc') + FIELD('b', 'a', 'b') + FIND_IN_SET('c', 'a,b,c')"), "72");
    assert_eq!(one(&mut db, "SELECT CONCAT('[', SPACE(2), ']') & ELT(2, 'x', 'y')"), "[  ]y");
    assert_eq!(one(&mut db, "SELECT IFNULL(NULL, 5) + IF(1 > 0, 1, 2)"), "6");
    assert_eq!(one(&mut db, "SELECT DATE_FORMAT(STR_TO_DATE('03/07/2020 14:05', '%d/%m/%Y %H:%i'), '%Y-%m-%d %T %b %W')"), "2020-07-03 14:05:00 Jul Friday");
    assert_eq!(one(&mut db, "SELECT UNIX_TIMESTAMP('1970-01-02 00:00:00')"), "86400");
    assert_eq!(one(&mut db, "SELECT FROM_UNIXTIME(86400, '%Y-%m-%d')"), "1970-01-02");
    assert_eq!(one(&mut db, "SELECT MD5('abc')"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(one(&mut db, "SELECT SHA2('abc', 256)"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(one(&mut db, "SELECT SOUNDEX('Robert')"), "R163");
    assert_eq!(one(&mut db, "SELECT INET_ATON('10.0.5.9') + IS_IPV4('1.2.3.4') + LENGTH(UUID())"), "167773486");
    assert_eq!(one(&mut db, "SELECT SUBSTRING_INDEX('a.b.c', '.', 2)"), "a.b");
    assert_eq!(one(&mut db, "SELECT CONVERT('12', SIGNED) + CAST('3' AS UNSIGNED)"), "15");
    assert_eq!(one(&mut db, "SELECT CONVERT('x' USING utf8)"), "x");
    assert_eq!(one(&mut db, "SELECT DATE_ADD('2024-01-31', INTERVAL 1 MONTH)"), "2024-02-29");
    assert_eq!(one(&mut db, "SELECT '2024-03-05 10:00:00' - INTERVAL 2 HOUR"), "2024-03-05 08:00:00");
    assert_eq!(one(&mut db, "SELECT DATE_SUB('2024-03-05', INTERVAL 1 DAY)"), "2024-03-04");
    assert_eq!(one(&mut db, "SELECT MONTHNAME('2024-03-05') & DAYNAME('2024-03-05')"), "MarchTuesday");

    db.execute("CREATE TABLE t (id int PRIMARY KEY AUTO_INCREMENT, a int DEFAULT 7, b text)").unwrap();
    db.execute("INSERT INTO t VALUES (); INSERT INTO t () VALUES ()").unwrap_err();
    db.execute("INSERT INTO t VALUES ()").unwrap();
    assert_eq!(one(&mut db, "SELECT a FROM t"), "7");
    db.execute("LOCK TABLES t WRITE; UNLOCK TABLES").unwrap();
}

#[test]
fn mysql_multi_table_delete_and_update() {
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE t1 (k int, v int); CREATE TABLE t2 (k int, w int);
         INSERT INTO t1 VALUES (1, 10), (2, 20), (3, 30), (3, 31);
         INSERT INTO t2 VALUES (1, 100), (3, 300), (9, 900)",
    )
    .unwrap();
    let n = |db: &mut Database, sql: &str| db.execute(sql).unwrap().rows[0][0].to_string();
    db.execute("UPDATE t1, t2 SET t1.v = t1.v + t2.w WHERE t1.k = t2.k").unwrap();
    assert_eq!(n(&mut db, "SELECT sum(v) FROM t1"), "791");
    db.execute("UPDATE t1 JOIN t2 ON t1.k = t2.k SET t1.v = 0 WHERE t2.w > 200").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM t1 WHERE v = 0"), "2");
    db.execute("DELETE t1 FROM t1, t2 WHERE t1.k = t2.k AND t2.w = 100").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM t1"), "3");
    db.execute("DELETE FROM a USING t1 AS a, t2 AS b WHERE a.k = b.k").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM t1"), "1");
    db.execute("DELETE a FROM t2 AS a, t2 AS b WHERE a.k = b.k AND a.w < b.w").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM t2"), "3");
}

#[test]
fn mysql_rename_limits_aliases_events_and_bit_operators() {
    let mut db = Database::in_memory();
    let n = |db: &mut Database, sql: &str| db.execute(sql).unwrap().rows[0][0].to_string();
    db.execute("CREATE TABLE a (k int, v int); CREATE TABLE c (z int); INSERT INTO a VALUES (1,10),(2,20),(3,30),(4,40),(5,50)").unwrap();
    // RENAME TABLE, several pairs one after the other (a swap)
    db.execute("RENAME TABLE a TO b").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM b"), "5");
    db.execute("RENAME TABLE b TO tmp, c TO b, tmp TO c").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM c"), "5");
    assert_eq!(n(&mut db, "SELECT count(*) FROM b"), "0");
    // ORDER BY and LIMIT in UPDATE and DELETE
    db.execute("UPDATE c SET v = v + 1 WHERE k > 1 ORDER BY k DESC LIMIT 2").unwrap();
    assert_eq!(n(&mut db, "SELECT group_concat(v) FROM (SELECT v FROM c ORDER BY k) q"), "10,20,30,41,51");
    db.execute("DELETE FROM c ORDER BY k LIMIT 2").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM c"), "3");
    db.execute("DELETE FROM c LIMIT 1").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM c"), "2");
    // quoted aliases, JOIN without ON
    let r = db.execute("SELECT k AS 'the key', v 'val' FROM c").unwrap();
    assert_eq!(r.columns, vec!["the key".to_string(), "val".to_string()]);
    assert_eq!(n(&mut db, "SELECT count(*) FROM c JOIN c AS d"), "4");
    // EXPLAIN table, events
    assert_eq!(db.execute("EXPLAIN c").unwrap().rows.len(), 2);
    db.execute("CREATE EVENT ev ON SCHEDULE EVERY 1 SECOND DO INSERT INTO c VALUES (9, 9)").unwrap();
    db.execute("DROP EVENT IF EXISTS ev").unwrap();
    // bitwise
    assert_eq!(n(&mut db, "SELECT 5 | 2"), "7");
    assert_eq!(n(&mut db, "SELECT ~0"), "-1");
    assert_eq!(n(&mut db, "SELECT 1 | 2 << 1"), "5");
    // index hints, DROP VIEW lists, MySQL partitioning, lenient numbers
    db.execute("CREATE TABLE h (a int, b text, KEY (a))").unwrap();
    db.execute("INSERT INTO h VALUES (1, 'x')").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM h USE INDEX (a) WHERE a = 1"), "1");
    assert_eq!(n(&mut db, "SELECT count(*) FROM h AS q FORCE INDEX FOR ORDER BY (a) WHERE q.a = 1"), "1");
    assert_eq!(n(&mut db, "SELECT count(*) FROM h IGNORE INDEX (a)"), "1");
    db.execute("CREATE VIEW v1 AS SELECT a FROM h; CREATE VIEW v2 AS SELECT b FROM h").unwrap();
    db.execute("DROP VIEW v1, v2").unwrap();
    db.execute("CREATE TABLE p (id int, d int) ENGINE=InnoDB PARTITION BY LIST (d) (PARTITION p1 VALUES IN (1) ENGINE = InnoDB, PARTITION p2 VALUES IN (2))").unwrap();
    db.execute("CREATE TABLE p2 (id int) PARTITION BY KEY (id) PARTITIONS 4").unwrap();
    db.execute("CREATE TABLE p3 (id int, d date) PARTITION BY RANGE COLUMNS (d) (PARTITION a VALUES LESS THAN ('2000-01-01'))").unwrap();
    db.execute("ALTER TABLE p ADD PARTITION (PARTITION p3 VALUES IN (3))").unwrap();
    db.execute("ALTER TABLE p DROP PARTITION p3").unwrap();
    db.execute("INSERT INTO p VALUES (1, 1), (2, 5)").unwrap();
    assert_eq!(n(&mut db, "SELECT count(*) FROM p"), "2");
    db.execute("CREATE TABLE ln (x int, y decimal(5,2))").unwrap();
    assert!(db.execute("INSERT INTO ln VALUES ('abc', 'zz')").is_err());
    db.execute("SET sql_mode = ''").unwrap();
    db.execute("INSERT INTO ln VALUES ('12abc', 'zz')").unwrap();
    assert_eq!(n(&mut db, "SELECT x FROM ln"), "12");
    assert_eq!(n(&mut db, "SELECT y FROM ln"), "0");
    db.execute("SET sql_mode = 'STRICT_ALL_TABLES'").unwrap();
    assert!(db.execute("INSERT INTO ln VALUES ('abc', 1)").is_err());
    // LIMIT offset, count; COLLATE; HANDLER
    db.execute("CREATE TABLE lim (k int, v text); INSERT INTO lim VALUES (1,'a'),(2,'b'),(3,'c'),(4,'d'),(5,'e')").unwrap();
    assert_eq!(n(&mut db, "SELECT group_concat(v) FROM (SELECT v FROM lim ORDER BY k LIMIT 1, 2) q"), "b,c");
    assert_eq!(n(&mut db, "SELECT count(*) FROM lim WHERE v = 'b' COLLATE latin1_bin"), "1");
    assert_eq!(n(&mut db, "SELECT count(*) FROM lim WHERE v COLLATE utf8mb4_general_ci = 'B'"), "1");
    db.execute("HANDLER lim OPEN AS hh").unwrap();
    assert_eq!(db.execute("HANDLER hh READ FIRST").unwrap().rows[0][0].to_string(), "1");
    assert_eq!(db.execute("HANDLER hh READ NEXT").unwrap().rows[0][0].to_string(), "2");
    assert_eq!(db.execute("HANDLER hh READ NEXT LIMIT 2").unwrap().rows.len(), 2);
    assert_eq!(db.execute("HANDLER hh READ PREV").unwrap().rows[0][0].to_string(), "3");
    assert_eq!(db.execute("HANDLER hh READ LAST").unwrap().rows[0][0].to_string(), "5");
    assert_eq!(db.execute("HANDLER hh READ FIRST WHERE k > 2").unwrap().rows[0][0].to_string(), "3");
    assert_eq!(db.execute("HANDLER hh READ NEXT").unwrap().rows[0][0].to_string(), "4");
    db.execute("HANDLER hh CLOSE").unwrap();
    assert!(db.execute("HANDLER hh READ NEXT").is_err());
    // texts nested absurdly deep end in an error, not a crash
    let mut q = String::from("COUNT(k)");
    for _ in 0..70 {
        q = format!("(SELECT {q})");
    }
    assert!(db.execute(&format!("SELECT k FROM lim WHERE {q} > 0")).is_err());
    assert!(db.execute(&format!("SELECT {}1{}", "(".repeat(20000), ")".repeat(20000))).is_err());
    assert_eq!(n(&mut db, "SELECT ((((((((((((1))))))))))))"), "1");
}

#[test]
fn mysql_routines_and_triggers() {
    let mut db = Database::in_memory();
    db.execute("CREATE TABLE t2 (d int); CREATE TABLE log (m text)").unwrap();
    let n = |db: &mut Database, sql: &str| db.execute(sql).unwrap().rows[0][0].to_string();
    db.execute("CREATE PROCEDURE sp1() INSERT INTO t2(d) VALUES(10)").unwrap();
    db.execute("CALL sp1()").unwrap();
    assert_eq!(n(&mut db, "SELECT d FROM t2"), "10");
    db.execute(
        "CREATE PROCEDURE sp2(IN a INT, b INT) DETERMINISTIC BEGIN
           DECLARE i INT DEFAULT 0;
           DECLARE total INT DEFAULT 0;
           WHILE i < a DO
             SET i = i + 1;
             SET total = total + b;
           END WHILE;
           IF total > 100 THEN INSERT INTO log VALUES ('big'); ELSEIF total > 10 THEN INSERT INTO log VALUES ('mid'); ELSE INSERT INTO log VALUES ('small'); END IF;
           INSERT INTO t2 VALUES (total);
         END",
    )
    .unwrap();
    db.execute("CALL sp2(5, 3)").unwrap();
    assert_eq!(n(&mut db, "SELECT m FROM log"), "mid");
    assert_eq!(n(&mut db, "SELECT max(d) FROM t2"), "15");
    db.execute("CREATE FUNCTION f(a INTEGER) RETURNS INTEGER DETERMINISTIC RETURN IFNULL(a, 666)").unwrap();
    assert_eq!(n(&mut db, "SELECT f(NULL) + f(1)"), "667");
    db.execute(
        "CREATE FUNCTION fact(n INT) RETURNS INT NO SQL BEGIN
           DECLARE r INT DEFAULT 1;
           WHILE n > 1 DO SET r = r * n; SET n = n - 1; END WHILE;
           RETURN r;
         END",
    )
    .unwrap();
    assert_eq!(n(&mut db, "SELECT fact(5)"), "120");
    // triggers: a single statement, SET NEW.col, BEGIN ... END
    db.execute("CREATE TABLE t3 (id int, d int, e text)").unwrap();
    db.execute("CREATE TRIGGER trg1 BEFORE INSERT ON t3 FOR EACH ROW SET NEW.d = NEW.id * 2").unwrap();
    db.execute(
        "CREATE TRIGGER trg2 BEFORE UPDATE ON t3 FOR EACH ROW BEGIN
           IF NEW.d IS NULL THEN SET NEW.d = 0; END IF;
           SET NEW.e = CONCAT('u', NEW.id);
           INSERT INTO log VALUES (CONCAT('upd ', OLD.id));
         END",
    )
    .unwrap();
    db.execute("INSERT INTO t3 (id) VALUES (4), (5)").unwrap();
    assert_eq!(n(&mut db, "SELECT sum(d) FROM t3"), "18");
    db.execute("UPDATE t3 SET d = NULL WHERE id = 4").unwrap();
    assert_eq!(n(&mut db, "SELECT e FROM t3 WHERE id = 4"), "u4");
    assert_eq!(n(&mut db, "SELECT d FROM t3 WHERE id = 4"), "0");
    assert_eq!(n(&mut db, "SELECT count(*) FROM log WHERE m = 'upd 4'"), "1");
    db.execute("DROP TRIGGER trg1").unwrap();
    db.execute("DROP PROCEDURE sp1; DROP FUNCTION f").unwrap();
}

#[test]
fn mysql_information_schema_views() {
    let mut db = Database::in_memory();
    db.execute("CREATE SCHEMA d1; CREATE TABLE t (a int PRIMARY KEY, b text); CREATE INDEX tb ON t (b)").unwrap();
    let rows = |db: &mut Database, sql: &str| db.execute(sql).unwrap().rows.len();
    assert!(rows(&mut db, "SELECT schema_name FROM information_schema.schemata") >= 6);
    assert_eq!(rows(&mut db, "SELECT * FROM information_schema.statistics WHERE table_name = 't'"), 2);
    assert_eq!(rows(&mut db, "SELECT * FROM information_schema.tables WHERE table_schema = 'test' AND table_name = 't'"), 1);
    assert_eq!(rows(&mut db, "SELECT column_type FROM information_schema.columns WHERE table_name = 't' AND column_key = 'PRI'"), 1);
    assert_eq!(rows(&mut db, "SELECT * FROM information_schema.processlist"), 0);
    assert!(rows(&mut db, "SELECT * FROM information_schema.character_sets") >= 1);
}

#[test]
fn mysql_accounts_hex_numbers_and_leniency() {
    let mut db = Database::in_memory();
    let one = |db: &mut Database, sql: &str| db.execute(sql).unwrap().rows[0][0].to_string();
    db.execute("CREATE USER IF NOT EXISTS 'root'@'127.0.0.1' IDENTIFIED BY 'x'; CREATE USER u1; GRANT SELECT, INSERT ON *.* TO 'root'@'127.0.0.1' WITH GRANT OPTION").unwrap();
    db.execute("GRANT REPLICATION SLAVE ON *.* TO plug_user; GRANT ALL ON db1.* TO 'x'@'%'; REVOKE ALL PRIVILEGES ON *.* FROM 'x'@'%'; DROP USER 'x'@'%'; DROP USER u1").unwrap();
    db.execute("ALTER USER 'root'@'localhost' IDENTIFIED BY 'y'; ALTER SCHEMA test COLLATE utf8mb4_0900_ai_ci; RESET PERSIST; CHECK TABLES nothing").unwrap();
    assert_eq!(one(&mut db, "SELECT 1 << 3, CONV('ff', 16, 10)").as_str(), "8");
    assert_eq!(one(&mut db, "SELECT CAST(0x10 AS UNSIGNED) + 0xff"), "271");
    assert_eq!(one(&mut db, "SELECT _utf8mb4 'x' = 'x'"), "Yes");
    db.execute("COMMIT; ROLLBACK; BEGIN; COMMIT").unwrap();
    db.execute("CREATE TABLE t (a varchar(10) binary, b char(5) ASCII, FULLTEXT (a))").unwrap();
    assert_eq!(db.execute("EXPLAIN FORMAT=tree SELECT * FROM t").unwrap().columns, vec!["plan"]);
    db.execute("PREPARE p FROM 'SELECT 1'; DROP PREPARE p").unwrap();
    assert_eq!(one(&mut db, "SELECT CAST('2001-01-01 00:00:00.12' AS DATETIME)"), "2001-01-01");
    assert_eq!(one(&mut db, "SELECT CAST('0000-00-00' AS DATETIME)"), "0001-01-01");
}

#[test]
fn found_by_the_lancer_oracles() {
    let mut db = Database::in_memory();
    let count = |db: &mut Database, sql: &str| db.execute(sql).unwrap().rows.len();
    // an index on a Yes/No column keeps No before Yes, but Yes compares as -1: only equality may use it
    db.execute("CREATE TABLE b (c BOOLEAN, i INTEGER); INSERT INTO b VALUES (TRUE, 1), (FALSE, 2), (NULL, 3)").unwrap();
    let queries = [
        "SELECT i FROM b WHERE c >= TRUE",
        "SELECT i FROM b WHERE c < FALSE",
        "SELECT i FROM b WHERE c <= FALSE",
        "SELECT i FROM b WHERE c BETWEEN TRUE AND FALSE",
        "SELECT i FROM b WHERE c = TRUE",
    ];
    let before: Vec<usize> = queries.iter().map(|q| count(&mut db, q)).collect();
    db.execute("CREATE INDEX b_c ON b (c)").unwrap();
    let after: Vec<usize> = queries.iter().map(|q| count(&mut db, q)).collect();
    assert_eq!(before, after);
    assert_eq!(before, vec![2, 1, 2, 2, 1]);
    // a derived table or view whose condition holds a sub-query that reads its own table
    db.execute("CREATE TABLE t0 (c0 TEXT, c1 INTEGER); INSERT INTO t0 VALUES ('a', 1), ('b', 2), ('c', NULL)").unwrap();
    for opt in [true, false] {
        db.set_optimizer(opt);
        assert_eq!(count(&mut db, "SELECT * FROM (SELECT c0 AS v0 FROM t0 WHERE EXISTS (SELECT 1 FROM t0 AS s WHERE s.c1 = t0.c1)) AS d"), 2);
        assert_eq!(count(&mut db, "SELECT * FROM (SELECT c0 AS v0 FROM t0 WHERE EXISTS (SELECT 1 FROM t0 AS s WHERE s.c1 = t0.c1)) AS d WHERE d.v0 = 'a'"), 1);
    }
    db.execute("CREATE VIEW v AS SELECT c0 FROM t0 WHERE EXISTS (SELECT 1 FROM t0 AS s WHERE s.c1 = t0.c1)").unwrap();
    assert_eq!(count(&mut db, "SELECT * FROM v WHERE c0 = 'b'"), 1);
}

#[test]
fn mysql_locking_reads_straight_join_char_using_hints_and_runaway_recursion() {
    let mut db = Database::in_memory();
    let n = |db: &mut Database, sql: &str| db.execute(sql).unwrap().rows[0][0].to_string();
    db.execute("CREATE TABLE t1 (a int, b int); CREATE TABLE t2 (a int); INSERT INTO t1 VALUES (1,10),(2,20),(3,30); INSERT INTO t2 VALUES (2),(3),(4)").unwrap();
    // locking reads lock nothing in a database used by one transaction at a time (see tests/locking.rs for shared ones)
    assert_eq!(n(&mut db, "SELECT count(*) FROM (SELECT * FROM t1 FOR UPDATE) q"), "3");
    assert_eq!(db.execute("SELECT a FROM t1 WHERE a > 1 ORDER BY a LIMIT 1 FOR SHARE").unwrap().rows.len(), 1);
    assert_eq!(db.execute("SELECT a FROM t1 LOCK IN SHARE MODE").unwrap().rows.len(), 3);
    assert_eq!(db.execute("SELECT a FROM t1 FOR UPDATE OF t1 NOWAIT").unwrap().rows.len(), 3);
    assert_eq!(db.execute("SELECT a FROM t1 FOR UPDATE SKIP LOCKED").unwrap().rows.len(), 3);
    // STRAIGHT_JOIN and the other select modifiers
    assert_eq!(n(&mut db, "SELECT STRAIGHT_JOIN count(*) FROM t1 STRAIGHT_JOIN t2 ON t1.a = t2.a"), "2");
    assert_eq!(n(&mut db, "SELECT SQL_NO_CACHE SQL_CALC_FOUND_ROWS HIGH_PRIORITY count(*) FROM t1"), "3");
    // CHAR(.. USING cs)
    assert_eq!(n(&mut db, "SELECT CHAR(77, 121 USING utf8mb4)"), "My");
    // CAST(x AS TIME) is the time of day; BIGINT UNSIGNED holds what a signed 64-bit integer cannot
    assert_eq!(n(&mut db, "SELECT CAST('1:2:3' AS TIME)"), "01:02:03");
    assert_eq!(n(&mut db, "SELECT CAST('2024-05-06 07:08:09' AS TIME)"), "07:08:09");
    db.execute("CREATE TABLE u8t (u BIGINT UNSIGNED); INSERT INTO u8t VALUES (18446744073709551615), (0)").unwrap();
    assert_eq!(n(&mut db, "SELECT max(u) FROM u8t"), "18446744073709551615");
    // MAX_EXECUTION_TIME ends a long select
    db.execute("CREATE TABLE big (x int)").unwrap();
    db.execute("INSERT INTO big SELECT g FROM generate_series(1, 3000) AS s(g)").unwrap();
    let e = db.execute("SELECT /*+ MAX_EXECUTION_TIME(1) */ count(*) FROM big a, big b, big c").unwrap_err().to_string();
    assert!(e.contains("timeout"), "{e}");
    // a recursion that multiplies its rows ends with an error, soon
    let started = std::time::Instant::now();
    let e = db.execute("WITH RECURSIVE qn AS (SELECT 1 AS x FROM t1 UNION ALL SELECT 1 FROM t1 LEFT JOIN qn ON 1) SELECT count(*) FROM qn").unwrap_err().to_string();
    assert!(e.contains("too many rows"), "{e}");
    assert!(started.elapsed().as_secs() < 30);
    // a correlated sub-query in the argument of an aggregate
    db.execute("CREATE TABLE g (c0 text, c1 int, k int); INSERT INTO g VALUES ('a',1,1),('b',2,1),('c',NULL,2),('d',2,2)").unwrap();
    for opt in [true, false] {
        db.set_optimizer(opt);
        assert_eq!(n(&mut db, "SELECT SUM(CASE WHEN EXISTS (SELECT 1 FROM g s WHERE s.c1 = g.c1 AND s.c0 <> g.c0) THEN 1 ELSE 0 END) FROM g"), "2");
        assert_eq!(n(&mut db, "SELECT SUM((SELECT MAX(s.c1) FROM g s WHERE s.k = g.k)) FROM g"), "8");
    }
}

#[test]
fn mysql_databases_are_schemas_and_recursion_stops_after_a_thousand_rounds() {
    let mut db = Database::in_memory();
    db.execute("CREATE DATABASE mysqltest DEFAULT CHARACTER SET utf8mb4; CREATE TABLE mysqltest.t1 (a int); INSERT INTO mysqltest.t1 VALUES (7)").unwrap();
    assert_eq!(db.execute("SELECT a FROM mysqltest.t1").unwrap().rows[0][0].to_string(), "7");
    db.execute("CREATE DATABASE IF NOT EXISTS test; DROP DATABASE test; DROP TABLE mysqltest.t1; DROP DATABASE IF EXISTS mysqltest").unwrap();
    assert!(db.execute("SELECT 1 FROM mysqltest.t1").is_err());
    assert_eq!(db.execute("WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 900) SELECT count(*) FROM r").unwrap().rows[0][0].to_string(), "900");
    assert!(db.execute("WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) SELECT count(*) FROM r").is_err());
}

#[test]
fn a_small_table_is_not_cross_joined_before_the_table_that_links_it() {
    // TPC-H Q8 in miniature: two copies of a small table, each linked to the rest only through a big one
    let mut db = Database::in_memory();
    let mut sql = String::from("CREATE TABLE reg (id int PRIMARY KEY); CREATE TABLE nat (id int PRIMARY KEY, rid int); CREATE TABLE sup (id int PRIMARY KEY, nid int); CREATE TABLE cus (id int PRIMARY KEY, nid int, sid int);");
    sql.push_str("CREATE INDEX sup_nid ON sup (nid); CREATE INDEX cus_nid ON cus (nid);");
    sql.push_str("INSERT INTO reg SELECT g FROM generate_series(1, 5) AS s(g); INSERT INTO nat SELECT g, g % 5 + 1 FROM generate_series(1, 25) AS s(g);");
    sql.push_str("INSERT INTO sup SELECT g, g % 25 + 1 FROM generate_series(1, 1000) AS s(g); INSERT INTO cus SELECT g, g % 25 + 1, g % 1000 + 1 FROM generate_series(1, 3000) AS s(g); ANALYZE");
    db.execute(&sql).unwrap();
    let q = "SELECT count(*) FROM reg, nat n1, nat n2, sup, cus WHERE n1.rid = reg.id AND reg.id = 1 AND cus.nid = n1.id AND cus.sid = sup.id AND sup.nid = n2.id";
    let plan: Vec<String> = db.execute(&format!("EXPLAIN {q}")).unwrap().rows.iter().map(|r| r[0].to_string()).collect();
    let order = plan.iter().find(|l| l.contains("join order")).expect("a join order");
    let at = |name: &str| order.find(&format!("{name} ")).or_else(|| order.find(name)).unwrap();
    assert!(at("n2") > at("sup") && at("n2") > at("cus"), "{order}");
}
