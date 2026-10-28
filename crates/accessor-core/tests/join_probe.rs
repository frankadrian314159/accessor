//! An index is not worth probing when it finds most of the table for every row: the join hashes instead.
use accessor_core::Database;

fn plan(db: &mut Database, sql: &str) -> String {
    db.execute(&format!("EXPLAIN {sql}")).unwrap().rows.iter().map(|r| r[0].to_string()).collect::<Vec<_>>().join("\n")
}

#[test]
fn an_index_on_a_column_with_one_value_is_not_probed_for_every_row() {
    let mut db = Database::in_memory();
    // `lines.w` has one value in all 6,000 rows, as the warehouse does in TPC-C; the join can only use the primary key up to it
    db.execute("CREATE TABLE stock (w INTEGER, i INTEGER, q INTEGER, PRIMARY KEY (w, i)); CREATE TABLE lines (w INTEGER, o INTEGER, n INTEGER, i INTEGER, PRIMARY KEY (w, o, n))").unwrap();
    let mut sql = String::new();
    for i in 0..100 {
        sql += &format!("INSERT INTO stock VALUES (1, {i}, {});", i % 20);
    }
    db.execute(&sql).unwrap();
    sql.clear();
    for o in 0..600 {
        for n in 0..10 {
            sql += &format!("INSERT INTO lines VALUES (1, {o}, {n}, {});", (o * 7 + n) % 100);
        }
    }
    db.execute(&sql).unwrap();
    let q = "SELECT COUNT(DISTINCT s.i) FROM lines l JOIN stock s ON s.w = l.w AND s.i = l.i WHERE l.w = 1 AND l.o >= 100 AND l.o < 140 AND s.q < 3";
    let text = plan(&mut db, q);
    assert!(!text.contains("index nested-loop via PrimaryKey (1 key"), "{text}");
    // whichever order is chosen, the answer is the same as with the optimizer's choices out of the way
    let fast = db.execute(q).unwrap().rows[0][0].to_string();
    let slow = db
        .execute("SELECT COUNT(DISTINCT s.i) FROM (SELECT * FROM lines WHERE o >= 100 AND o < 140) l, stock s WHERE s.w = l.w AND s.i = l.i AND s.q < 3")
        .unwrap()
        .rows[0][0]
        .to_string();
    assert_eq!(fast, slow);
    // a probe that finds one row (the primary key of `stock`) is still an index nested-loop
    let text = plan(&mut db, "SELECT COUNT(*) FROM lines l JOIN stock s ON s.w = l.w AND s.i = l.i WHERE l.o = 5");
    assert!(text.contains("index nested-loop"), "{text}");
}
