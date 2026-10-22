//! Micro-benchmark: `cargo run --release -p accessor-core --example bench [rows]`

use std::time::Instant;

use accessor_core::{Database, Durability, Value};

fn timed<T>(label: &str, n: usize, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    let dt = t.elapsed();
    let per = dt.as_secs_f64() * 1e6 / n.max(1) as f64;
    println!("{label:<44} {:>9.1} ms   {per:>9.2} µs/op", dt.as_secs_f64() * 1e3);
    r
}

fn main() {
    let rows: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(200_000);
    let mut db = Database::in_memory();
    db.execute(
        "CREATE TABLE Customers (ID INTEGER PRIMARY KEY, Name TEXT, Region INTEGER);
         CREATE TABLE Orders (OrderID COUNTER PRIMARY KEY, CustID INTEGER, Total INTEGER, Note TEXT);
         CREATE INDEX ix_cust ON Orders (CustID);",
    )
    .unwrap();

    let customers = 1000.min(rows);
    timed("insert customers (SQL, autocommit)", customers, || {
        for i in 0..customers {
            db.execute(&format!("INSERT INTO Customers VALUES ({i}, 'customer {i}', {})", i % 20)).unwrap();
        }
    });
    timed("insert orders (API, one transaction)", rows, || {
        db.begin().unwrap();
        for i in 0..rows {
            db.insert_row(
                "Orders",
                &[
                    (1, Value::Int((i % customers) as i64)),
                    (2, Value::Int((i % 997) as i64)),
                    (3, Value::text(&format!("note number {i}"))),
                ],
            )
            .unwrap();
        }
        db.commit().unwrap();
    });

    timed("point lookup by primary key x10000", 10_000, || {
        for i in 0..10_000 {
            let r = db.query(&format!("SELECT Name FROM Customers WHERE ID = {}", i % customers)).unwrap();
            assert_eq!(r.rows.len(), 1);
        }
    });
    timed("indexed range scan (1%)", rows / 100, || {
        let r = db.query(&format!("SELECT OrderID FROM Orders WHERE OrderID BETWEEN 1000 AND {}", 1000 + rows / 100)).unwrap();
        assert!(r.rows.len() > 1);
    });
    timed("full scan + filter (Total > 990)", rows, || {
        db.query("SELECT Count(*) FROM Orders WHERE Total > 990").unwrap();
    });
    timed("LIKE '*99*' full scan", rows, || {
        db.query("SELECT Count(*) FROM Orders WHERE Note LIKE '*99*'").unwrap();
    });
    timed("GROUP BY CustID, Sum/Avg/Count", rows, || {
        let r = db.query("SELECT CustID, Sum(Total), Avg(Total), Count(*) FROM Orders GROUP BY CustID").unwrap();
        assert_eq!(r.rows.len(), customers);
    });
    timed("join (index NLJ) + group by region", rows, || {
        db.query(
            "SELECT c.Region, Sum(o.Total) FROM Orders o INNER JOIN Customers c ON o.CustID = c.ID GROUP BY c.Region",
        )
        .unwrap();
    });
    timed("join (hash, no index) + count", rows, || {
        db.query("SELECT Count(*) FROM Orders o INNER JOIN Customers c ON o.CustID + 0 = c.ID + 0").unwrap();
    });
    timed("ORDER BY Total DESC TOP 10 (top-k)", rows, || {
        db.query("SELECT TOP 10 OrderID, Total FROM Orders ORDER BY Total DESC").unwrap();
    });
    timed("ORDER BY Note (full sort)", rows, || {
        db.query("SELECT OrderID FROM Orders ORDER BY Note").unwrap();
    });
    timed("UPDATE 1% of rows by indexed column", rows / 100, || {
        db.execute("UPDATE Orders SET Total = Total + 1 WHERE CustID = 7").unwrap();
    });
    timed("DELETE by primary-key range", rows / 100, || {
        db.execute(&format!("DELETE FROM Orders WHERE OrderID < {}", rows / 100)).unwrap();
    });

    // Fractional numbers take the exact-decimal path, which allocates.
    db.execute("CREATE TABLE Prices (ID COUNTER PRIMARY KEY, P CURRENCY, Q DOUBLE, R RATIONAL, S BIGNUM)").unwrap();
    timed("insert 2-decimal prices (API)", rows, || {
        db.begin().unwrap();
        for i in 0..rows {
            let price = accessor_core::decimal::Decimal::parse(&format!("{}.{:02}", i % 997, i % 100)).unwrap();
            let q = (i % 997) as f64 + (i % 100) as f64 / 100.0;
            db.insert_row(
                "Prices",
                &[
                    (1, Value::from(price)),
                    (2, Value::Double(q)),
                    (3, Value::Rational(accessor_core::rational::Rational::new((i as i64 % 997).into(), ((i as i64 % 13) + 1).into()).unwrap())),
                    (4, Value::Int(i as i64)),
                ],
            )
            .unwrap();
        }
        db.commit().unwrap();
    });
    timed("Sum + Avg over a decimal column", rows, || {
        db.query("SELECT Sum(P), Avg(P) FROM Prices").unwrap();
    });
    timed("full scan + filter on a decimal column", rows, || {
        db.query("SELECT Count(*) FROM Prices WHERE P > 500.5").unwrap();
    });
    timed("Sum + Avg over a DOUBLE column", rows, || {
        db.query("SELECT Sum(Q), Avg(Q) FROM Prices").unwrap();
    });
    timed("Sum over a RATIONAL column (exact fractions)", rows, || {
        db.query("SELECT Sum(R) FROM Prices").unwrap();
    });
    timed("Sum over a BIGNUM column", rows, || {
        db.query("SELECT Sum(S) FROM Prices").unwrap();
    });
    timed("mixed arithmetic: P * Q + S over all rows", rows, || {
        db.query("SELECT Sum(Q * 2 + S) FROM Prices").unwrap();
    });
    timed("ORDER BY decimal DESC TOP 10 (top-k)", rows, || {
        db.query("SELECT TOP 10 ID, P FROM Prices ORDER BY P DESC").unwrap();
    });
    timed("P * 1.0825 for every row (exact multiply)", rows, || {
        db.query("SELECT Sum(P * 1.0825) FROM Prices").unwrap();
    });

    // Durability cost.
    let dir = std::env::temp_dir().join(format!("accessor-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bench.adb");
    {
        let mut disk = Database::open(&path).unwrap();
        disk.execute("CREATE TABLE t (id COUNTER PRIMARY KEY, v TEXT)").unwrap();
        timed("disk: 200 autocommit inserts (fsync)", 200, || {
            for i in 0..200 {
                disk.execute(&format!("INSERT INTO t (v) VALUES ('row {i}')")).unwrap();
            }
        });
        disk.set_durability(Durability::OsBuffered);
        timed("disk: 20000 autocommit inserts (os-buffered)", 20_000, || {
            for i in 0..20_000 {
                disk.execute(&format!("INSERT INTO t (v) VALUES ('row {i}')")).unwrap();
            }
        });
        timed("disk: checkpoint", 1, || disk.checkpoint().unwrap());
    }
    timed("disk: reopen (load snapshot)", 1, || {
        let d = Database::open(&path).unwrap();
        assert_eq!(d.table("t").unwrap().len(), 20_200);
    });
    let _ = std::fs::remove_dir_all(dir);
}
