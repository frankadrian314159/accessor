# Accessor

A small embedded database in the spirit of Microsoft Access, written in Rust.
It has a SQL engine you can use as a library, a command-line shell, and a
desktop app with datasheet, form, design and query views.

Accessor is its own thing: it does not read or write `.mdb` / `.accdb` files.
What it borrows from Access is the feel: `[bracketed names]`, `*` and `?` in
`LIKE`, `&` for joining text, AutoNumber fields, saved queries, and forms and
datasheets for working with records directly.

## What it can do

- **SQL** (called ASQL): all the join types including `FULL`, `USING` and
  `NATURAL` and Access-style nested joins; grouping, `ROLLUP` / `CUBE`;
  subqueries including correlated ones; `UNION` / `INTERSECT` / `EXCEPT`;
  `WITH` and `WITH RECURSIVE`; window functions; `MERGE`; Access crosstabs
  (`TRANSFORM … PIVOT`); `CHECK` constraints;
  row value comparisons, `LATERAL` and table functions, `PIVOT` / `UNPIVOT`, named windows,
  `NULLS FIRST/LAST`, simple updatable views, `INTERVAL` and time-zone-aware timestamps;
  `CAST`, `Format` and about a hundred functions; `TOP` / `LIMIT` / `FETCH FIRST`;
  `CREATE` / `ALTER` / `DROP` for tables, indexes and queries; and CSV import and
  export. ASQL is a large subset of standard SQL, not all of the latest standard;
  see [docs/sql/conformance.md](docs/sql/conformance.md).
- **A query optimizer**: conditions are applied to each table before joining,
  inner joins are put in the cheapest order, `ORDER BY ... LIMIT` reads an index
  in order instead of sorting, and `EXPLAIN` shows the plan with estimated and
  actual row counts (see [docs/optimizer](docs/optimizer/README.md)).
- **Transactions**: `BEGIN`, `COMMIT`, `ROLLBACK` and savepoints, covering schema
  changes as well as rows. Everything in a transaction becomes permanent together
  or not at all, including after a crash (see below).
- **Relationships**: foreign keys with `RESTRICT`, `CASCADE` or `SET NULL` on
  delete and on key change, checked as a whole statement so rows can arrive in
  any order.
- **Calculated fields**: a field whose value is an expression over the other
  fields of its record, recomputed on every write. Any ordinary field can be
  turned into one.
- **Saved queries with parameters**: `CREATE QUERY name (@a, @b) AS ...`, run
  with `EXEC`. A saved query can also be a call of another one.
- **Triggers**: row-level `BEFORE` / `AFTER` `INSERT` / `UPDATE` / `DELETE`
  triggers with `NEW` and `OLD`, an optional `WHEN`, and `RAISE` to refuse a change.
- **A numeric tower** with exact arithmetic across the types (see below).
- **Durability**: a checksummed write-ahead log plus atomic snapshots. Committed
  work survives a crash; a torn write at the end of the log is discarded.
  Files written by every earlier version still open.

## Documentation

The [docs](docs/README.md) folder has an overview of Accessor, a reference for the [SQL language](docs/sql/README.md) with its BNF grammar, the
[functions](docs/functions/README.md), the [column types](docs/types/README.md), and the
[query optimizer](docs/optimizer/README.md).

## Building and running

You need a recent stable Rust (1.85 or later).

```text
cargo run --release -p accessor-gui              # the desktop app
cargo run --release -p accessor-cli -- my.adb    # the shell, on a database file
cargo test --workspace                           # everything
```

In the desktop app, **File > Load sample data** gives you something to explore.
Right-click a table for its datasheet, form and design views. Saved queries,
triggers and tables are listed in the left pane.

Database files use the extension `.adb`. Beside each one there is a `-wal`
file while it is open; both belong together.

### The shell

```text
accessor [FILE.adb] [-c "SQL"] [-f script.sql]
```

Statements end with `;`. Dot-commands: `.tables`, `.queries`, `.schema [TABLE]`,
`.indexes TABLE`, `.import FILE TABLE`, `.import --new FILE TABLE`,
`.export FILE SQL`, `.read FILE`, `.sync on|off`, `.timer on|off`,
`.checkpoint`, `.compact`, `.help`, `.quit`.

The prompt shows `accessor*>` while a transaction is open. A transaction still open
when the shell ends is rolled back, with a warning; a `-c` or `-f` script that ends
that way exits with a failure status.

### As a library

```rust
use accessor_core::{Database, Value};

let mut db = Database::in_memory();            // or Database::open("my.adb")?
db.execute("CREATE TABLE Customers (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL)")?;
db.execute("INSERT INTO Customers (Name) VALUES ('Ada'), ('Grace')")?;
let r = db.query("SELECT Name FROM Customers WHERE Name LIKE 'g*'")?;
assert_eq!(r.rows, vec![vec![Value::text("Grace")]]);
```

## A short tour

```sql
CREATE TABLE Artist (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL);

CREATE TABLE Album (
  ID        COUNTER PRIMARY KEY,
  Name      TEXT NOT NULL,
  Year      INTEGER,
  Artist_ID INTEGER NOT NULL REFERENCES Artist (ID) ON DELETE CASCADE,
  Title     CALCULATED AS (Name & ' (' & Year & ')')
);

INSERT INTO Artist (Name) VALUES ('Grand Funk Railroad'), ('Yes');
INSERT INTO Album (Name, Year, Artist_ID) VALUES
  ('On Time', 1969, 1), ('Closer to Home', 1970, 1), ('Close to the Edge', 1972, 2);

SELECT Title FROM Album ORDER BY Year;      -- On Time (1969), Closer to Home (1970), ...

CREATE QUERY AlbumsBy (@artist) AS
  SELECT AL.Title FROM Album AL JOIN Artist AR ON AR.ID = AL.Artist_ID
  WHERE AR.Name = @artist ORDER BY AL.Year;
EXEC AlbumsBy 'Grand Funk Railroad';

CREATE TABLE Log (Msg TEXT);
CREATE TRIGGER album_added AFTER INSERT ON Album
BEGIN
  INSERT INTO Log VALUES ('added ' & NEW.Name);
END;
```

Text is joined with a single `&` (or the standard `||`). `RAISE 'message'` inside
a trigger refuses the statement, and everything the trigger and any cascades did
is undone with it.

## The query optimizer

A query says *what* rows you want; the optimizer decides *how* to get them, and
never changes the answer. It is on by default, needs no statistics or hints, and
makes its choices from the tables as they are when the query runs.

- **Pushdown.** A `WHERE` or inner-join `ON` condition that reads one table is
  applied while that table is read, before any join. Conditions are never
  applied to the nullable side of an outer join, where that would change the answer.
- **Join order.** Inner and cross joins are done in the cheapest order, not the
  written one: start with the table that has the fewest rows after its own
  conditions, then add the table that keeps the intermediate result smallest,
  preferring index lookups. Outer joins keep the order written.
- **Join method.** Index nested-loop when the joined table has an index on the
  join columns, otherwise a hash join built on the smaller side.
- **Access paths.** Each table uses the best index for its conditions. An
  `ORDER BY ... LIMIT` that matches an index reads the index in order and stops
  early instead of sorting, and a bare `SELECT COUNT(*)` is the table's row count.
  A `TRANSFORM … PIVOT` crosstab is computed in one grouped pass rather than one
  conditional aggregate per column.

On a table of a million rows, a three-way join filtered to one customer takes
about 1.8 ms instead of 2.6 s, and `SELECT COUNT(*)` takes 0.02 ms instead of 260 ms.

`EXPLAIN` runs a query and shows its plan, with the optimizer's estimate next
to the real row count at each step:

```
accessor> EXPLAIN SELECT C.Name, O.Total, L.Item
     ...> FROM Line L JOIN Ord O ON O.ID = L.OrdID JOIN Cust C ON C.ID = O.CustID
     ...> WHERE C.Name = 'c7';
query over L, O, C
join order: C -> O -> L (reordered from the query's order)
scan C [50 rows]: full scan (1 condition(s) pushed down)
  -> est 5 rows, actual 1
join O: index nested-loop via ix_ord_cust (1 key column(s)); 1 rows in, 8 out (...)
join L: index nested-loop via ix_line_ord (1 key column(s)); 8 rows in, 40 out (...)
40 row(s) returned
```

`.optimizer off` in the shell (or `db.set_optimizer(false)`) runs queries exactly
as written, which is how the optimizer is tested: the same queries must give the
same answers, in the same order, with it on and off. When a query is slow, read
its plan first; the usual fix is an index on a filtered or joined column. The
estimates are simple (table sizes, exact counts for indexed conditions, fixed
fractions otherwise), outer joins are not reordered, and plans are not cached.
The full reference, with a tuning guide, is in [docs/optimizer](docs/optimizer/README.md).

## Transactions

```sql
BEGIN;
INSERT INTO Artist (Name) VALUES ('Rush');
SAVEPOINT before_albums;
INSERT INTO Album (Name, Year, Artist_ID) VALUES ('Moving Pictures', 1981, 3);
ALTER TABLE Album ADD COLUMN Label TEXT;
ROLLBACK TO before_albums;      -- the album and the new column are gone; Rush stays
COMMIT;                         -- ...and now Rush is permanent
```

- **All or nothing.** `BEGIN` starts a transaction. `COMMIT` makes every change since then
  permanent as a single record in the write-ahead log, so after a crash you find either
  all of it or none of it, never part. `ROLLBACK` takes everything back.
- **Schema changes take part.** Creating, altering and dropping tables, fields,
  indexes, relationships, saved queries and triggers can all be rolled back, along with
  the cascades, triggers and calculated fields that rows set off. `DROP TABLE`
  followed by `ROLLBACK` brings the table back with its data.
- **A failed statement undoes only itself.** An error inside a transaction (a constraint,
  a bad value) cancels that one statement; the transaction stays open and you can go on or
  roll back.
- **Savepoints.** `SAVEPOINT name`, `ROLLBACK TO [SAVEPOINT] name` (keeps the savepoint,
  discards later ones) and `RELEASE [SAVEPOINT] name` (keeps the changes, forgets the
  point). Reusing a name hides the earlier savepoint of that name.
- **No nesting**: `BEGIN` inside a transaction is an error (use savepoints). `.checkpoint`
  and "compact and repair" are refused while a transaction is open.
- **Nothing is lost by accident.** A database closed or dropped with a transaction open
  rolls it back. The desktop app shows an open transaction in its status bar (with
  *Commit* and *Roll back* buttons, and a *Transaction* menu), and asks before you close the
  window or open another database; the shell warns when it discards one.

From Rust:

```rust
// Committed if the closure returns Ok, rolled back if it returns an error.
db.transaction(|db| {
    db.execute("INSERT INTO Customers (Name) VALUES ('Ada')")?;
    db.execute("UPDATE Accounts SET Balance = Balance - 10 WHERE ID = 1")?;
    Ok(())
})?;

// Or by hand:
db.begin()?;
db.savepoint("before_risky_part")?;
// ...
db.rollback_to("before_risky_part")?;   // also: db.release(..), db.commit()?, db.rollback()?
db.commit()?;
```

## Numbers

Seven numeric types, which mix in arithmetic and compare by exact value:

| Type       | What it holds |
|------------|---------------|
| `INTEGER`  | 64-bit signed integer. Fractions are rounded to the nearest whole number (ties to even). Also the type of `COUNTER` (AutoNumber) fields. |
| `BIGNUM`   | An integer of any size. |
| `FLOAT`    | 32-bit floating point. |
| `DOUBLE`   | 64-bit floating point. |
| `DECIMAL`  | A decimal number of any size and precision. |
| `CURRENCY` | Represented exactly like `DECIMAL`, but a type of its own; prints as `$12.50`. |
| `RATIONAL` | An exact fraction of two `BIGNUM`s, such as `1/3`. |

The type of an arithmetic result depends only on the types of the operands:

- A `DOUBLE` operand makes the result a `DOUBLE`; otherwise a `FLOAT` operand makes it a `FLOAT`.
- Among the exact types, `+ - *` give `RATIONAL` if either side is one, else `CURRENCY`, else
  `DECIMAL`, else `BIGNUM`, else `INTEGER`. An `INTEGER` result too big for 64 bits becomes a `BIGNUM`.
- `/` is exact: `7 / 2` is the `DECIMAL` 3.5; `1 / 3` is a `DECIMAL` cut at 50 digits; use
  `CRat(1) / 3` for exactly one third. Money divided by a number is money.
- Floats are never infinite or NaN: overflow and division by zero are errors.

Comparison, sorting, indexes, `DISTINCT` and joins all use exact values, so
`1 = 1.0` and `CRat(2) / 4 = 0.5` are true, while a `DOUBLE` 0.1 is *not* the
`DECIMAL` 0.1. Use `CDec(CDbl(0.1)) = 0.1` to compare through the shortest decimal.

Literals: `5` is an `INTEGER`, `5.0` a `DECIMAL`, `5e0` a `DOUBLE`, and a whole
number too big for 64 bits a `BIGNUM`. Convert with `CInt`, `CBig`, `CSng`, `CDbl`,
`CDec`, `CCur`, `CRat` or `Rational(n, d)`; `TypeName(x)` tells you a value's type.
SQL names from other databases (`INT`, `BIGINT`, `FLOAT`, `REAL`, `NUMERIC`,
`MONEY` ...) are accepted as spellings of these types.

`Sqr`, `Exp`, `Log` and fractional powers are computed in floating point and are
only as precise as a `DOUBLE`; everything else is exact.

## Dates and times

A `DATETIME` field holds a moment as a whole number of seconds since 1970-01-01 00:00:00,
in UTC, so dates before 1970 work and there are no time zones or daylight-saving
rules to think about. Fractions of a second are not kept. `DATE`, `TIME` and `TIMESTAMP`
are accepted as other names for the type. The calendar is the proleptic Gregorian one.

```sql
CREATE TABLE Event (ID COUNTER PRIMARY KEY, Name TEXT, Starts DATETIME);
INSERT INTO Event (Name, Starts) VALUES
  ('launch', #2024-02-29 13:45:10#),    -- a date literal
  ('retro',  '3/1/2024 9:30 AM'),       -- text is read as a date when stored
  ('party',  '2024-03-15');
SELECT Name, Starts FROM Event ORDER BY Starts;
--  launch  2024-02-29 13:45:10
--  retro   2024-03-01 09:30:00
--  party   2024-03-15
```

**Writing dates.** A literal is written between `#` marks: `#2024-02-29#`,
`#2024-02-29 13:45:10#`, `#3/1/2024 9:30 AM#`. Text stored into a `DATETIME` field or
passed to `CDate` is read the same way. Two layouts are understood:

- `YYYY-MM-DD`, optionally followed by a space or `T` and `HH:MM[:SS]`;
- the US layout `M/D/YYYY`, optionally followed by `h:mm[:ss]` and `AM` or `PM`.

A date that does not exist (`2023-02-29`) is refused, with an error naming the value.
Years are taken literally, so write four digits.

**Showing dates.** A date at midnight shows as `2024-03-15`; any other time as
`2024-02-29 13:45:10`. `CStr` and CSV export use the same form.

**Taking dates apart and building them**

| Function | Result |
|----------|--------|
| `Year(d)`, `Month(d)`, `Day(d)` | the parts, as `INTEGER`s |
| `Hour(d)`, `Minute(d)`, `Second(d)` | the time of day |
| `Weekday(d)` | 1 for Sunday through 7 for Saturday |
| `DateSerial(y, m, d)` | a date; months and days out of range roll over, so `DateSerial(2024, 14, 1)` is 2025-02-01 and `DateSerial(2024, 3, 0)` is 2024-02-29 |
| `CDate(x)` | text or a number as a date |
| `Now()` | the current moment (UTC) |
| `Date()` (or `Today()`) | today's date at midnight (UTC) |

**Adding and counting**

`DateAdd(unit, n, d)` adds `n` units; `DateDiff(unit, a, b)` counts them from `a` to `b`
(negative if `b` is earlier). The units are `'yyyy'` (years), `'q'` (quarters), `'m'` (months),
`'d'` (days), `'ww'` (weeks), `'h'`, `'n'` (minutes) and `'s'`.

```sql
SELECT DateAdd('m', 1, #2024-01-31#);                 -- 2024-02-29 (the day is clamped to the month)
SELECT DateAdd('h', 5, #2024-01-31#);                 -- 2024-01-31 05:00:00
SELECT DateDiff('d', #2024-01-01#, #2024-03-01#);     -- 60
SELECT DateDiff('yyyy', #2020-12-31#, #2021-01-01#);  -- 1
```

For `yyyy`, `q`, `m` and `d`, `DateDiff` counts the calendar boundaries crossed, not elapsed
time, so the last example is 1 although it is one day. For `ww`, `h`, `n` and `s` it counts
whole elapsed units (a week is 7 days).

**Arithmetic.** Dates are counted in days, so a date plus a number is a later date and
a fraction is part of a day:

```sql
SELECT #2024-03-15# + 1;       -- 2024-03-16
SELECT #2024-03-15# + 0.5;     -- 2024-03-15 12:00:00
SELECT #2024-03-15# - 7;       -- 2024-03-08
SELECT #2024-03-15# - #2024-02-29#;   -- 15   (an INTEGER when it is a whole number of days, else a DECIMAL)
```

Any numeric type can be used for the number of days.

**Comparing and sorting.** Dates compare as moments. Text on the other side of a
comparison is read as a date, so `WHERE Starts >= '2024-03-01'` and `BETWEEN` work as
you would hope (text that is not a date compares as greater than any date). Compared with
a number, a date counts as the number of days since 1899-12-30, the convention
Access and spreadsheets use: `CDate(45000)` is 2023-03-15 and `CDbl(#1899-12-31#)` is 1.
In a sorted list, `Null` comes first, then Yes/No, numbers, dates, then text; `Min` and `Max`
work on dates. An index on a `DATETIME` field speeds up searches that compare it with a date (`#...#`);
comparing it with text works but reads the table.

**Elsewhere.** `Now()` and `Date()` cannot be used in a calculated field, because the value
would go stale (it is only recomputed when its record is saved). Importing CSV makes a column
`DATETIME` when every non-empty value contains `-` or `/` and reads as a date.

## Project layout

| Crate | |
|-------|---|
| `crates/accessor-core` | The engine: SQL parser and executor, storage, durability, the numeric types. |
| `crates/accessor-cli`  | The `accessor` shell. |
| `crates/accessor-gui`  | The `accessor-gui` desktop app, built with [egui](https://github.com/emilk/egui). |

Performance numbers for the engine come from
`cargo run --release -p accessor-core --example bench [rows]`.

## License

[MIT](LICENSE)
