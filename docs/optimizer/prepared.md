# Prepared statements and the statement cache

Running SQL text means reading it (lexing and parsing), then planning and executing it. Reading depends only on the text; planning depends on the data. The engine saves the first and deliberately not the second.

## The statement cache

Every database remembers the parsed form of the SQL text it has recently been given (up to 256 different texts, none longer than 8 KB). `execute`, `query` and `execute_with_params` look the text up before parsing, so a statement that is run again, character for character, skips the parse. Nothing needs to be done to get this.

The cache holds *syntax*, never answers or plans: a changed table, a new index or new data is always seen by the next run.

## Prepared statements

When a statement is run again with different values, building new text each time defeats the cache and invites mistakes with quoting. `prepare` reads a statement once and gives back a handle to run with parameter values:

```rust
let find = db.prepare("SELECT name FROM Customer WHERE id = @id")?;
for id in ids {
    let r = db.query_prepared(&find, &[("id", Value::Int(id))])?;
    // ...
}
let add = db.prepare("INSERT INTO Log (at, msg) VALUES (@at, @msg)")?;
db.execute_prepared(&add, &[("at", Value::DateTime(now)), ("msg", Value::text("hi"))])?;
```

`Prepared::parameters()` lists the `@names` it uses. A value must be supplied for each; extra ones are ignored. A prepared script may hold several statements, and the result is that of the last. `query_prepared` runs a single `SELECT` with a shared borrow of the database, so it can be used where `query` can.

The same thing is available in SQL, for a session:

```sql
PREPARE by_city AS SELECT Name FROM Customers WHERE City = @city;
EXEC by_city ('Rome');                  -- positional: the parameters in order of first appearance
EXEC by_city (@city = 'Oslo');          -- or by name
DEALLOCATE by_city;                     -- DEALLOCATE ALL forgets every one
```

A prepared statement lives as long as the connection; it is not stored in the file. (A saved query, `CREATE QUERY`, is the stored kind, and takes the same `EXEC`; a name cannot be both.)

## Why plans are not kept

A query plan is a set of decisions: which table first, which index, whether to hash a sub-query. They rest on the sizes of the tables, the spread of values in the columns, and the values in the query itself. A plan made for `WHERE status = 'closed'` (a few rows, use the index) is wrong for `WHERE status = 'open'` (nearly all of them, scan); a plan made before a bulk load is wrong after it. Keeping plans would need a way to tell when each has gone stale; planning afresh costs microseconds and is always right for the data of the moment. What the engine does keep, because it is cheap to keep correctly, are the [column statistics](estimates.md), which are discarded the moment their table changes.

So a prepared statement saves **reading** the SQL. How much that matters depends on the statement: for a point lookup, reading is a large share of the time; for a query that scans a table, it is nothing.

## What it saves

A point lookup (`SELECT name, score FROM t WHERE id = ... AND score >= 0` over 10,000 rows, release build), per query:

| How it is run | Time |
| --- | --- |
| new text each time (`WHERE id = 17`, `WHERE id = 18`, ...) | 8.8 µs |
| the same text again (statement cache) | 3.3 µs |
| prepared, with a different `@id` each time | 4.3 µs |

and an insert of one row: 4.5 µs as new text, 2.4 µs prepared. So reading the SQL was more than half the cost of a point lookup, and the figures above for a prepared statement include copying it and putting the values in. For a query that does real work (scans, joins, sorts) the difference is lost in the noise.

A caveat for the cache: text that differs only in a constant is a different text. A loop that builds `WHERE id = 17` and `WHERE id = 18` fills the cache with one entry per value, which is useless and makes it churn; prepare once and pass the value.
