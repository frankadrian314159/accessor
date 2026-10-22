# Accessor

A small embedded database in the spirit of Microsoft Access, written in Rust.
It has a SQL engine you can use as a library, a command-line shell, and a
desktop app with datasheet, form, design and query views.

Accessor is its own thing: it does not read or write `.mdb` / `.accdb` files.
What it borrows from Access is the feel: `[bracketed names]`, `*` and `?` in
`LIKE`, `&` for joining text, AutoNumber fields, saved queries, and forms and
datasheets for working with records directly.

## What it can do

- **SQL**: joins (including Access-style nested joins), grouping and `HAVING`,
  subqueries, `TOP` / `LIMIT`, transactions, `CREATE` / `ALTER` / `DROP`
  for tables, indexes and queries, and `CSV` import and export.
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
