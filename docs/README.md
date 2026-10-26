# Accessor documentation

| Folder or file | Contents |
| --- | --- |
| [sql/](sql/README.md) | the ASQL language: statements, the BNF grammar, expressions, and how ASQL relates to standard SQL |
| [concurrency.md](concurrency.md) | using one database from several threads: overlapping transactions with snapshot isolation, conflicts, deadlocks |
| [extern/](extern/README.md) | using Accessor from C and from Java (JDBC), version numbers, using the file from several processes, and how the interface is tested |
| [functions/](functions/README.md) | every built-in function and table function, by group |
| [optimizer/](optimizer/README.md) | the query optimizer: pushdown, join order, access paths, estimates, `EXPLAIN` and tuning |
| [types/](types/README.md) | the column types, and how they convert and combine |

The rest of this page is an overview of Accessor: what it is, its features, its architecture and how to use it.

---

## Accessor: an overview

*Version 0.1 · MIT licensed*

A small embedded database written in Rust, in the spirit of Microsoft Access. It gives you a SQL engine you can link into a program, a command-line shell, and a desktop application with datasheets, forms, a table designer and a query editor.

- Rust workspace, five crates, a C interface and a JDBC driver
- Exact numbers of any size
- Transactions that include schema changes
- Crash-safe write-ahead log
- A query optimizer with EXPLAIN
- Relationships, triggers, calculated fields

### Contents of this overview

- [What Accessor is](#what-accessor-is)
- [The main features](#the-main-features)
- [Getting started](#getting-started)
- [Tables, fields and keys](#tables-fields-and-keys)
- [Querying with SQL](#querying-with-sql)
- [Changing data and structure](#changing-data-and-structure)
- [Relationships](#relationships)
- [Calculated fields](#calculated-fields)
- [Saved and parameterised queries](#saved-and-parameterised-queries)
- [Triggers](#triggers)
- [Transactions](#transactions)
- [Numbers](#numbers)
- [Dates and times](#dates-and-times)
- [The desktop app](#the-desktop-app)
- [The shell](#the-shell)
- [Using the library](#using-the-library)
- [Architecture](#architecture)
- [The query optimizer](#the-query-optimizer)
- [Performance](#performance)
- [ASQL and standard SQL](#asql-and-standard-sql)
- [Limits and what is missing](#limits-and-what-is-missing)
- [Quick reference](#quick-reference)

### What Accessor is

Accessor is a relational database that lives inside your program or on your desktop. There is no server to install, no account to create and no network involved. A database is a single file with a companion log, and everything that reads or writes it is ordinary Rust code in a library you link or an application you run.

The inspiration is Microsoft Access, and in particular the part of Access that made it so useful to so many people: you could sit down, define a table, type some records into a grid, build a query, and have something working in an afternoon without being a database administrator. Accessor tries to keep that feel. Tables have AutoNumber keys. Names can be written in `[square brackets]`, so they may contain spaces. `LIKE` understands `*` and `?`. Text is joined with `&`. Dates are written between `#` marks. A saved query is a first-class object, listed next to the tables. And there is a graphical application where you can open a table as a datasheet, edit it as a form, redesign it, and run SQL against it.

Underneath that familiar surface the engine is deliberately modern and strict. It has real foreign keys with cascading rules, row triggers, fields computed from other fields, transactions that cover schema changes as well as rows, and a numeric system in which money never loses a cent and integers never silently overflow. It writes every commit to a checksummed write-ahead log so that a crash cannot leave a half-finished change behind.

#### Who it is for

Accessor suits three kinds of use. The first is **a desktop tool for small data**: a catalogue, a ledger, a membership list, a lab notebook, anything that fits comfortably in memory and benefits from forms and queries more than from a web application. The second is **an embedded store inside a Rust program** that wants SQL, relationships and transactions without a separate process. The third is **learning and experimentation**: the code base is compact, heavily commented and fully tested, and it is a readable example of how a relational engine fits together, from lexer to write-ahead log.

#### What it is not

It helps to be clear about the limits up front. Accessor does not read or write Access files (`.mdb` or `.accdb`); it has its own file format. It is not a client/server database, and nothing in it is designed for several processes to use one file at once. It keeps the whole database in memory, so the data must fit in RAM. It has no reports, no macros and no VBA. The SQL dialect is a useful subset, not the whole of the standard; the list of things that are missing is in [Limits and what is missing](#limits-and-what-is-missing).

### The main features

This is the whole feature set in one place. Each item has its own section later in this overview.

**A SQL engine with an Access flavour**

Joins (inner, left, right, full, cross, `USING`, `NATURAL`, and Access-style nested joins), grouping with `HAVING`, `ROLLUP`, `CUBE` and `GROUPING SETS`, aggregates, subqueries including correlated ones, derived tables, `WITH` and `WITH RECURSIVE`, `UNION`, `INTERSECT` and `EXCEPT`, window functions, `MERGE`, crosstab queries (`TRANSFORM … PIVOT`), `PIVOT` and `UNPIVOT`, `LATERAL` joins, table functions, row value comparisons, named windows, `NULLS FIRST/LAST`, simple updatable views, `CASE`, `CAST`, `TOP`, `LIMIT` and `FETCH FIRST`, and about a hundred built-in functions including `Format`. Indexes are used automatically for equality and range searches, and for joins. ASQL, as the dialect is called, implements a large part of the core of standard SQL, but not all of the latest standard; [see below](#asql-and-standard-sql).

**Tables, keys and indexes**

Typed fields, `NOT NULL`, `DEFAULT`, `CHECK`, single and composite primary keys, unique constraints, AutoNumber (`COUNTER`, or standard identity) fields, generated columns and secondary indexes. Tables can be altered after they are created: fields can be added, dropped, renamed and retyped.

**A query optimizer**

Conditions are applied to each table before joining, inner joins are put in the cheapest order, `ORDER BY ... LIMIT` reads an index in order instead of sorting, and `COUNT(*)` costs nothing. `EXPLAIN` shows the plan with estimated and actual row counts. On a million-row table the right plan is often a thousand times faster than the wrong one, and there is no statistics job to run. [The optimizer section](#the-query-optimizer) and the [optimizer reference](optimizer/README.md) explain how it decides.

**Relationships**

Foreign keys on one or several fields, with `RESTRICT`, `CASCADE` or `SET NULL` for deletes and for key changes. They are checked once per statement, after all of its rows are in place, so rows can arrive in any order.

**Calculated fields**

A field whose value is an expression over the other fields of its own record, kept up to date on every write. An ordinary field can be turned into one.

**Saved queries with parameters**

`CREATE QUERY AlbumsBy (@artist) AS ...` then `EXEC AlbumsBy 'Yes'`. A saved query can also be a call of another one.

**Triggers**

Row-level `BEFORE` and `AFTER` triggers for inserts, updates and deletes, with `NEW` and `OLD` values, an optional condition, and `RAISE` to refuse a change.

**Transactions**

`BEGIN`, `COMMIT`, `ROLLBACK` and savepoints. Schema changes take part, so you can create, alter and drop tables inside a transaction and take it all back.

**A numeric tower**

Seven numeric types: `INTEGER`, `BIGNUM`, `FLOAT`, `DOUBLE`, `DECIMAL`, `CURRENCY` and `RATIONAL`, which mix correctly in arithmetic and compare by exact value. Dates come as `DATETIME` (a wall-clock reading), `TIMESTAMPTZ` (a moment, shown in an IANA time zone) and `INTERVAL` (months, days and seconds).

**Dates and times**

A `DATETIME` type with date literals, calendar arithmetic and the familiar `DateAdd`, `DateDiff` and `DateSerial` functions.

**Durability**

A write-ahead log with checksums, atomic snapshots, automatic recovery on open, and the ability to read every earlier version of the file format.

**Three ways to use it**

A desktop application, an interactive shell with scripting, and a Rust library.

### Getting started

#### Building

Accessor is a Cargo workspace, so all you need is a recent stable Rust toolchain (version 1.85 or later). Clone the repository and build from its top folder:

```bash
cargo build --release
cargo test --workspace
```

The first build downloads the dependencies and takes a minute or two; the graphical application pulls in the most. The release profile enables full link-time optimisation, which makes the binaries fast but slows the compile. For everyday work, leave off `--release`.

#### The desktop app

```bash
cargo run --release -p accessor-gui
```

The window opens on an empty, in-memory database; the status bar says so. Nothing is saved until you create or open a file. Choose **File › New database…** to name a file ending in `.adb`, or **File › Load sample data** to fill the current database with a small shop (customers, products, orders, order details and two saved queries) so you have something to explore.

Double-click a table in the left pane to open its datasheet. Right-click it for the form view, the design view, or to delete it. Double-click a cell to edit it, press `Enter` to keep the change or `Esc` to cancel. The section on [the desktop app](#the-desktop-app) goes through each view.

#### The shell

```bash
cargo run --release -p accessor-cli -- shop.adb
```

The shell opens (or creates) `shop.adb` and reads statements from the keyboard. End each statement with a semicolon. Without a file name it works on a throw-away in-memory database. A first session might look like this:

```text
accessor> CREATE TABLE Customer (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL);
accessor> INSERT INTO Customer (Name) VALUES ('Ada'), ('Grace');
2 rows affected
accessor> SELECT * FROM Customer;
ID │ Name
────┼──────
 1 │ Ada
 2 │ Grace
(2 rows)
accessor> .quit
```

#### The library

To use the engine from your own program, depend on the `accessor-core` crate and open a database:

```rust
use accessor_core::{Database, Value};

let mut db = Database::open("shop.adb")?;          // or Database::in_memory()
db.execute("CREATE TABLE Customer (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL)")?;
db.execute("INSERT INTO Customer (Name) VALUES ('Ada'), ('Grace')")?;
let r = db.query("SELECT Name FROM Customer WHERE Name LIKE 'g*'")?;
assert_eq!(r.rows, vec![vec![Value::text("Grace")]]);
```

The section on [using the library](#using-the-library) covers parameters, transactions, cell-level editing, CSV and error handling. A statement that is run many times with different values can be read once with `db.prepare("... @id ...")` and run with `execute_prepared` or `query_prepared`; the engine also remembers the parsed form of recent SQL text. See [prepared statements](optimizer/prepared.md).

#### The files on disk

A database called `shop.adb` is a snapshot of the data as of the last checkpoint. While it is open there is a second file, `shop.adb-wal`, holding every committed change since then. Both belong together: copy or move them as a pair, or close the database first so that the log is folded into the snapshot and removed from the way. A third, short-lived file ending in `.tmp` appears briefly while a new snapshot is being written.

### Tables, fields and keys

A database is a set of named tables. A table is a list of typed fields (columns) and a collection of records (rows). Names are case-insensitive everywhere: `customer`, `Customer` and `CUSTOMER` are the same table. A name that contains spaces or punctuation, or that collides with a keyword, is written in square brackets: `[Order Details]`.

#### Creating a table

```sql
CREATE TABLE Product (
  ID      COUNTER PRIMARY KEY,
  Name    TEXT NOT NULL UNIQUE,
  Price   CURRENCY NOT NULL DEFAULT 0,
  Stock   INTEGER DEFAULT 0,
  Added   DATETIME,
  Active  YESNO DEFAULT TRUE
);
```

Each field has a name, a type and optional constraints. The constraints are `NOT NULL` (the field is required), `PRIMARY KEY`, `UNIQUE`, `DEFAULT` followed by a constant, and, for a calculated field, `CALCULATED AS (expression)`. A table-level `PRIMARY KEY (a, b)` clause declares a composite key, and `CONSTRAINT name FOREIGN KEY ...` clauses declare relationships, described [below](#relationships).

#### Field types

Every type has a canonical name and a number of aliases, so that SQL written for another system usually works unchanged.

| Type | Aliases | Holds |
| --- | --- | --- |
| TEXT | VARCHAR, CHAR, CHARACTER [VARYING], NCHAR, STRING, MEMO, CLOB | Unicode text of any length (a size such as `VARCHAR(50)` is accepted and ignored) |
| INTEGER | INT, SMALLINT, TINYINT, LONG | 64-bit whole numbers |
| BIGNUM | BIGINT | Whole numbers of any size |
| FLOAT | REAL, SINGLE | 32-bit floating point |
| DOUBLE | DOUBLE PRECISION | 64-bit floating point |
| DECIMAL | NUMERIC, DEC, NUMBER | Exact decimal numbers of any size (a scale such as `NUMERIC(10, 2)` is accepted and ignored) |
| CURRENCY | MONEY | Exact decimals, shown as money |
| RATIONAL |  | Exact fractions, numerator over denominator |
| YESNO | BOOLEAN, BOOL, BIT | True or false |
| DATETIME | DATE, TIME, TIMESTAMP | A date with a time of day |
| COUNTER | AUTONUMBER, SERIAL | An INTEGER that fills itself in |

Any field may also be `NULL`, meaning "no value", unless it is declared `NOT NULL` or is part of the primary key. The numeric types and dates have their own sections, [Numbers](#numbers) and [Dates and times](#dates-and-times).

#### Keys and AutoNumber

A primary key identifies one record. It may cover one field or several. Accessor refuses to store two records with the same key, and refuses a key that contains `NULL`. Behind the scenes the key is backed by an index, so looking a record up by its key does not scan the table.

A `COUNTER` field is an integer that the engine assigns when you leave it out of an insert. Numbers count up from one and are never reused, even if you delete the record that held them, which is what you want from an identifier. If you do supply a value, it is kept, and the counter moves on past it, so a later automatic value cannot collide with it.

```sql
INSERT INTO Product (Name, Price) VALUES ('Widget', 2.50);       -- ID becomes 1
INSERT INTO Product (ID, Name, Price) VALUES (40, 'Gadget', 9);  -- ID is 40
INSERT INTO Product (Name, Price) VALUES ('Gizmo', 4);           -- ID becomes 41
```

#### Defaults and required fields

A `DEFAULT` is applied when an insert does not mention the field. A required field with no default must be supplied, or the insert is refused with a message that names the table and the field: *field 'Customer.Name' cannot contain a Null value*. Because messages always name what was refused, a failure in a long script is easy to place.

#### Indexes

Besides the indexes that back keys and unique fields, you can create your own on any list of fields:

```sql
CREATE INDEX ByPrice ON Product (Price);
CREATE UNIQUE INDEX ByName ON Product (Name);
CREATE INDEX ByRegionTotal ON Orders (Region, Total DESC);   -- columns may be ASC (the default) or DESC
DROP INDEX ByPrice ON Product;
```

The engine consults indexes on its own: you never name one in a query. An index is used when a `WHERE` clause compares its leading fields to constants with `=` or with a range (`<`, `<=`, `>`, `>=`, `BETWEEN`), and when a join can look up matching records instead of scanning them. Indexes cost a little on every write and some memory, so add them where reads are frequent.

#### Dropping tables

`DROP TABLE Product` removes the table, its data and its indexes. It is refused while another table has a foreign key that points at it, because that would leave dangling references. Drop the child first, or remove the relationship.

### Querying with SQL

Accessor's SQL is a practical subset of the standard with the Access habits kept on top. It is parsed by a hand-written recursive-descent parser, so error messages can say exactly what was expected and where: *expected an identifier but found 'FROM' (at offset 7)*.

#### The basics

```sql
SELECT Name, Price * 1.2 AS WithTax
FROM Product
WHERE Active AND Price BETWEEN 1 AND 10
ORDER BY Price DESC, Name
LIMIT 20 OFFSET 40;
```

You can limit the result with Access's `SELECT TOP 10 ...` or with `LIMIT n [OFFSET m]`. `SELECT DISTINCT` removes duplicate rows. `ORDER BY` accepts expressions, aliases and column positions (`ORDER BY 2`). The wildcard `*` expands to every field, and `T.*` to every field of one table.

Operators are the usual ones: `+ - * /`, integer division `\`, `MOD`, exponent `^`, comparisons, `AND OR NOT`, `IS NULL` and `IS NOT NULL`, `IN (...)`, `BETWEEN`, `LIKE`. Text is joined with `&` (and, for portability, `||`). Comments run from `--` to the end of the line, or sit between `/*` and `*/`.

> **NULL and three-valued logic**

> As in every SQL, comparing anything with `NULL` gives "unknown" rather than true or false, and a `WHERE` clause keeps only the rows for which the condition is true. Use `IS NULL` to test for it, and `Nz(x, 0)` or `Coalesce(x, y)` to substitute a value.

#### Pattern matching

`LIKE` understands both dialects at once. In the Access style, `*` matches any run of characters, `?` exactly one, `#` exactly one digit, `[abc]` one character from a set, `[a-f]` one from a range and `[!abc]` one that is not in the set. In the standard style, `%` and `_` do the jobs of `*` and `?`. Matching is case-insensitive. Patterns are compiled once per statement, and common shapes (a literal prefix, a literal suffix, "contains") have fast paths that avoid the general matcher.

```sql
SELECT * FROM Customer WHERE Name LIKE 'A*';          -- starts with A
SELECT * FROM Product  WHERE Name LIKE '*widget*';    -- contains
SELECT * FROM Product  WHERE Code  LIKE 'AB-###';     -- AB- and three digits
```

#### Joins

All the usual joins are there: `INNER JOIN`, `LEFT JOIN`, `RIGHT JOIN`, `CROSS JOIN` and the comma form, which is a cross join filtered by `WHERE`. Joins can be nested with parentheses in the way Access requires, so a query pasted from Access usually runs unchanged:

```sql
SELECT C.Name, O.OrderDate, P.Name AS Product, D.Quantity
FROM ((Customer AS C
       INNER JOIN Orders AS O ON O.CustomerID = C.ID)
       INNER JOIN OrderDetail AS D ON D.OrderID = O.ID)
       INNER JOIN Product AS P ON P.ID = D.ProductID
WHERE C.Name LIKE 'A*'
ORDER BY O.OrderDate;
```

The planner chooses a method for each join. If the inner side has an index on the joined fields, it looks matches up one at a time (an index nested-loop join). Otherwise, for equality joins, it builds a hash table of one side and probes it with the other. Only for conditions that are not equalities does it fall back to comparing every pair. Chains of joins are ordered as written, so put the most selective table first.

#### Grouping and aggregates

```sql
SELECT C.Name, Count(*) AS Orders, Sum(D.Quantity * D.UnitPrice) AS Spent
FROM (Customer C INNER JOIN Orders O ON O.CustomerID = C.ID)
     INNER JOIN OrderDetail D ON D.OrderID = O.ID
GROUP BY C.Name
HAVING Sum(D.Quantity * D.UnitPrice) > 100
ORDER BY Spent DESC;
```

The aggregates are `Count`, `Sum`, `Avg`, `Min`, `Max`, `First` and `Last`. Each accepts `DISTINCT`. They skip `NULL` values, except `Count(*)`, which counts rows. Because the arithmetic is exact, a `Sum` of money columns is exactly right, with no accumulated rounding, and a `Sum` of integers cannot overflow: it quietly becomes a `BIGNUM` if it needs to.

#### Subqueries and derived tables

A subquery can stand in as a value, as a list after `IN`, or as a table in the `FROM` clause:

```sql
SELECT Name FROM Product WHERE Price > (SELECT Avg(Price) FROM Product);
SELECT Name FROM Customer WHERE ID IN (SELECT CustomerID FROM Orders);
SELECT T.Name, T.Total
FROM (SELECT C.Name, Count(*) AS Total FROM Customer C
      INNER JOIN Orders O ON O.CustomerID = C.ID GROUP BY C.Name) AS T
WHERE T.Total > 1;
```

##### Correlated subqueries

A subquery may also read columns of the query around it, and then it is evaluated for each outer row:

```sql
SELECT Name,
       (SELECT Sum(Total) FROM Orders O WHERE O.CustID = C.ID) AS Spent
FROM Customers C
WHERE EXISTS (SELECT 1 FROM Orders O WHERE O.CustID = C.ID AND O.Total > 100);
```

Names resolve to the subquery's own tables first, and to the outer query only when the subquery has no column of that name. They nest to any depth. A subquery runs once for each distinct combination of outer values it reads, and the answers are remembered, so a correlated subquery over a thousand customers who live in ten cities, correlated on city, runs ten times. They are allowed in the select list, `WHERE`, `GROUP BY`, `HAVING` and `ORDER BY` (in a grouped query they read the group, so only its grouping columns), and in the `WHERE` and `SET` of `UPDATE` and `DELETE`. When the correlation is through equalities, as in `EXISTS (SELECT 1 FROM Orders O WHERE O.CustID = C.ID)`, the optimizer runs the sub-query once and looks the answers up instead (a semi-join): over 5,000 customers and 30,000 orders with no index that is 8 ms instead of 12 s.

#### Upserts, regular expressions and the catalog

`INSERT ... ON CONFLICT (k) DO UPDATE SET n = n + EXCLUDED.n` (or `DO NOTHING`, or `REPLACE INTO`) deals with a row that collides with an existing one on a unique key. `REGEXP_LIKE`, `REGEXP_REPLACE`, `REGEXP_SUBSTR` and `SIMILAR TO` match patterns; `LPAD`, `RPAD`, `REPEAT`, `REVERSE`, `OVERLAY`, `TRANSLATE` and `INITCAP` shape text; `ATAN2`, `DEGREES`, `RADIANS`, `PI()`, `RANDOM()` and `TRUNC(x, n)` fill gaps in the maths. `SELECT * FROM INFORMATION_SCHEMA.COLUMNS WHERE table_name = 'Orders'` lists a table's fields, and the other views list constraints, indexes, routines, sequences, triggers and reports. See [data changes](sql/data-changes.md), [regular expressions](functions/regex.md) and the [catalog views](sql/information-schema.md).

#### Stored functions and procedures

`CREATE FUNCTION tax(@amount CURRENCY, @rate DECIMAL = 0.2) AS @amount * @rate` defines an expression you can use anywhere (it is expanded in place, so indexes and the optimizer see through it). `CREATE PROCEDURE` takes a block with `DECLARE`, `SET`, `IF`, `WHILE`, `RETURN` and ordinary SQL statements, run by `CALL` as one all-or-nothing unit. See [routines](sql/routines.md).

#### RETURNING

`INSERT INTO t (name) VALUES ('Ada') RETURNING id` hands back the AutoNumber (or any column, or an expression) of each row changed, for `INSERT`, `UPDATE`, `DELETE` and `MERGE`, as the row ended up. See [data changes](sql/data-changes.md#returning).

#### Reports

`CREATE REPORT` stores a layout and a query: `BREAK BY Region` groups the rows under headings, `COLUMN Amount FORMAT '#,##0.00' SUM` totals and formats a column, `GRAND TOTAL` closes the report, and `PAGE SIZE` pages it. `RUN REPORT name AS HTML` (or text, Markdown, CSV) returns the document; `db.render_report(name, ReportFormat::Pdf)` and the shell's `.report name file.pdf` write a PDF. See [reports](sql/reports.md).

#### Sequences

`CREATE SEQUENCE invoice_no START WITH 1000` makes a counter of its own; `NEXT VALUE FOR invoice_no` (or `NEXTVAL('invoice_no')`) takes the next number, and `DEFAULT NEXT VALUE FOR invoice_no` fills a column. Numbers taken are never given back, even by a rollback, and survive a restart. See [sequences](sql/sequences.md).

#### Views you can change

`CREATE VIEW v (a, b) AS SELECT ... [WITH CHECK OPTION]` names the columns, and `INSERT`, `UPDATE` and `DELETE` work on a view of one table, within the view's condition. `CREATE OR REPLACE VIEW` swaps a definition atomically, `ALTER TABLE t RENAME TO new` follows the references, and `ALTER COLUMN c SET DEFAULT / SET NOT NULL` adjust a column. A view that shows all of a table's columns is merged into the queries that read it, so the table's indexes apply. See [queries and triggers](sql/queries-and-triggers.md) and [data definition](sql/ddl.md).

#### Seeing the plan

`EXPLAIN` in front of a query runs it and returns the plan instead of the rows: the join order chosen, the index used for each table, the number of rows each step was expected to produce and did, and whether a sort was needed. See [the optimizer chapter](#the-query-optimizer) and the [EXPLAIN reference](optimizer/explain.md).

```sql
EXPLAIN SELECT C.Name, L.Item
FROM Line L JOIN Ord O ON O.ID = L.OrdID JOIN Cust C ON C.ID = O.CustID
WHERE C.Name = 'c77';
```

#### Crosstab queries

The Access crosstab turns the values of one column into columns:

```sql
TRANSFORM Sum(Amount)
SELECT Region, Sum(Amount) AS Total
FROM Sales
GROUP BY Region
PIVOT Quarter;
```

`TRANSFORM` gives the aggregate for each cell, the `SELECT` and `GROUP BY` give the row headings, and `PIVOT` the column whose values become new columns, in ascending order, with a column called `<>` for NULL. `PIVOT Quarter IN (1, 2, 3, 4)` fixes the columns. An empty cell is NULL, or 0 for `Count`. A crosstab can be saved with `CREATE QUERY` and selected from. See the [SQL reference](sql/select.md#crosstab-queries-transform--pivot) and [how the optimizer runs crosstabs](optimizer/crosstab.md).

#### Ordering, row values, LATERAL and pivots

`ORDER BY x NULLS LAST` places NULLs explicitly (an index can still serve it). A row value compares several columns at once: `WHERE (Day, ID) > (@lastDay, @lastID)` pages through results from an index. `LATERAL` runs a sub-query for each row of the tables before it, which makes "top 3 per customer" a one-liner, and `GENERATE_SERIES` and `SPLIT_STRING` supply rows from nothing. `source PIVOT (SUM(x) FOR col IN (...))` and `source UNPIVOT (v FOR name IN (c1, c2))` turn rows into columns and back. See the [SQL reference](sql/select.md).

#### JSON

A `JSON` column holds a document, checked when it is stored and kept in a canonical form so equal documents are equal. `body -> 'tags' -> 0` and `body ->> 'name'` reach inside; `JSON_VALUE(body, '$.age')` gives an ordinary number; `JSON_SET`, `JSON_REMOVE` and `JSON_MERGE_PATCH` build changed documents; `JSON_EACH` turns a document into rows and `JSON_ARRAYAGG` turns rows into a document. See [the JSON type](types/json.md) and [the JSON functions](functions/json.md).

#### Combining queries

`UNION`, `INTERSECT` and `EXCEPT` (also spelt `MINUS`) combine the rows of two or more queries with the same number of columns. Each takes `ALL` to keep duplicates. `INTERSECT` binds more tightly than the others, which go left to right; parentheses group. An `ORDER BY` after the last query sorts the combined result, by result column name or position.

```sql
SELECT City FROM Customers
UNION
SELECT City FROM Suppliers
ORDER BY 1;
```

#### Common table expressions

`WITH` names a query so that the rest of the statement can use it as a table. Each name is computed once, in order, and may use the ones before it.

```sql
WITH big AS (SELECT * FROM Orders WHERE Total > 100),
     per AS (SELECT CustID, Count(*) AS N FROM big GROUP BY CustID)
SELECT * FROM per WHERE N > 1;
```

`WITH RECURSIVE` lets a query refer to itself, which is how to walk a tree or generate a series. The parts that do not mention the name start it; the parts that do are run on the rows of the previous round until a round adds nothing.

```sql
WITH RECURSIVE n (i) AS (
    SELECT 1
    UNION ALL
    SELECT i + 1 FROM n WHERE i < 10
)
SELECT Sum(i) FROM n;          -- 55
```

A runaway recursion is stopped after 100,000 rounds or ten million rows. `VALUES (1, 'a'), (2, 'b')` is a query too, and can stand wherever one can: `FROM (VALUES ...) AS t (n, s)`.

#### Window functions

A window function computes a value for every row from a set of related rows without collapsing them, which makes rankings, running totals and moving averages one-liners:

```sql
SELECT Name, Dept, Sal,
       RANK() OVER (PARTITION BY Dept ORDER BY Sal DESC)        AS Rank,
       SUM(Sal) OVER (PARTITION BY Dept)                        AS DeptTotal,
       AVG(Sal) OVER (ORDER BY ID ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) AS Moving,
       Sal - LAG(Sal) OVER (ORDER BY ID)                        AS Change
FROM Emp;
```

The available functions are `ROW_NUMBER`, `RANK`, `DENSE_RANK`, `PERCENT_RANK`, `CUME_DIST`, `NTILE`, `LAG`, `LEAD`, `FIRST_VALUE`, `LAST_VALUE`, `NTH_VALUE`, and every aggregate. The frame is `ROWS` with any bounds, or `RANGE` with `UNBOUNDED` and `CURRENT ROW` bounds. In a grouped query they run after the grouping, so `RANK() OVER (ORDER BY Sum(Sal) DESC)` ranks groups.

#### Subtotals

`GROUP BY ROLLUP (a, b)` adds subtotal rows for `(a, b)`, `(a)` and the grand total; `CUBE (a, b)` adds every combination; `GROUPING SETS` lists exactly the groupings wanted. In the subtotal rows the rolled-up columns are NULL, and `GROUPING(a)` is 1 there so that you can tell them from real NULLs.

#### Conditional expressions

Besides the Access function `IIf(condition, a, b)`, the engine supports `CASE WHEN ... THEN ... ELSE ... END`. Both evaluate only the branch they choose, so `IIf(d = 0, 0, n / d)` is safe.

#### Built-in functions

| Group | Functions |
| --- | --- |
| Null handling | IsNull, Nz, Coalesce, NullIf, IIf |
| Text | UCase / Upper, LCase / Lower, Len / Char_Length, Left, Right, Mid / Substring, Trim (with LEADING / TRAILING / BOTH), LTrim, RTrim, InStr / Position, Replace, Concat, Octet_Length |
| Maths | Abs, Round, Int / Floor, Ceil, Fix, Sqr, Sgn, Exp, Log / Ln, Log10, Power, Mod, Sin, Cos, Tan, Asin, Acos, Atan, Sinh, Cosh, Tanh, Greatest, Least |
| Dates | Now, Date / Today, CURRENT_DATE, CURRENT_TIMESTAMP, Year, Month, Day, Hour, Minute, Second, Weekday, DateAdd, DateDiff, DateSerial, DatePart, EXTRACT |
| Conversion | CAST, Val, CStr, CInt, CBig, CSng / CFloat, CDbl, CDec, CCur, CRat, CBool, CDate |
| Rationals | Rational(n, d), Numerator, Denominator |
| Formatting | Format |
| Conditional | IIf, CASE, Coalesce, Nz, NullIf, Greatest, Least, IsNull |
| Inspection | TypeName |

`TypeName(x)` is a useful companion when you are learning how the numeric types behave: `SELECT TypeName(1 / 3)` says `DECIMAL`, and `SELECT TypeName(2 ^ 100)` says `BIGNUM`.

### Changing data and structure

#### Inserts, updates and deletes

```sql
INSERT INTO Product (Name, Price) VALUES ('Nut', 0.10), ('Bolt', 0.25);
INSERT INTO Archive SELECT * FROM Product WHERE Active = FALSE;
UPDATE Product SET Price = Price * 1.05 WHERE Price < 10;
DELETE FROM Product WHERE Stock = 0;
```

An insert can take several rows in one statement, or the result of a query. The result of every data statement is the number of records it changed. Each statement is atomic: if the fiftieth row of an update breaks a constraint, the first forty-nine are undone and nothing is written. This is true with or without an explicit transaction.

Constraints are checked as the changes are made, except foreign keys, which are checked at the end of the statement against the final state of the data. That is what lets you renumber a key in a single `UPDATE` that shifts every value, or insert a parent and its children in one multi-row statement, in any order.

#### Altering a table

```sql
ALTER TABLE Product ADD COLUMN Notes TEXT;
ALTER TABLE Product ADD COLUMN Margin CALCULATED AS (Price - Cost);
ALTER TABLE Product RENAME COLUMN Notes TO Remarks;
ALTER TABLE Product ALTER COLUMN Stock TYPE BIGNUM;
ALTER TABLE Product ADD CONSTRAINT nonneg CHECK (Stock >= 0);
ALTER TABLE Product DROP COLUMN Remarks;
```

Adding a field fills it with its default (or `NULL`) for the records already there. Changing a field's type converts each stored value; if a value cannot be converted, say text that is not a number, the whole change is refused and the table is untouched. Dropping or retyping a field that a foreign key relies on is refused with an explanation, as are changes that would break an index, a trigger or a calculated field that mentions it. Every one of these changes can be rolled back inside a transaction.

#### MERGE, TRUNCATE and the rest

`MERGE` reconciles a table with a source of new data in one statement, updating, deleting or inserting according to whether each source row matches:

```sql
MERGE INTO Stock S
USING (VALUES (1, 5), (2, 0), (9, 7)) AS D (ID, Qty) ON S.ID = D.ID
WHEN MATCHED AND D.Qty = 0 THEN DELETE
WHEN MATCHED THEN UPDATE SET Qty = S.Qty + D.Qty
WHEN NOT MATCHED THEN INSERT (ID, Qty) VALUES (D.ID, D.Qty);
```

It is atomic: an error part-way leaves the target as it was. `TRUNCATE TABLE t` deletes every record; `INSERT INTO t DEFAULT VALUES` inserts a record of defaults; `CREATE TABLE IF NOT EXISTS` and `DROP TABLE IF EXISTS` make scripts re-runnable. `UPDATE` and `DELETE` accept a table alias, which is useful in correlated subqueries.

#### CHECK constraints and generated columns

A `CHECK (condition)` on a column or a table refuses any record for which the condition is false (an unknown result passes, as in SQL). Checks are held as a pair of triggers, so they are enforced for every write, including cascades and the datasheet. A column can also be `GENERATED ALWAYS AS (expression)`, which is another way to spell a calculated field, or `GENERATED ... AS IDENTITY`, which is an AutoNumber.

#### CSV import and export

Comma-separated files are the lingua franca of small data, so they are built in. From the shell, `.import data.csv Table` loads a file into an existing table, matching the header row to the field names. With `--new` it creates the table first, inferring a type for each column by looking at the data: `INTEGER` for whole numbers that fit, `BIGNUM` for ones that do not, `DECIMAL` for numbers with a decimal point, `YESNO` for true/false columns, `DATETIME` for dates and `TEXT` for everything else. Type inference never chooses a floating-point type, so a column of prices imports exactly as written. `.export Table out.csv` writes a table back out, quoting where necessary.

The same operations are available from the desktop app's **File** menu and from the library.

### Relationships

A relationship says that a field (or several) in one table must match a key in another. Orders belong to customers; order lines belong to orders and to products. In Accessor these are foreign keys, and they are enforced by the engine, not by the application that happens to be talking to it.

#### Declaring a foreign key

```sql
CREATE TABLE Orders (
  ID          COUNTER PRIMARY KEY,
  CustomerID  INTEGER NOT NULL,
  OrderDate   DATETIME,
  CONSTRAINT FK_Orders_Customer
    FOREIGN KEY (CustomerID) REFERENCES Customer (ID)
    ON DELETE CASCADE
);
```

The referenced fields must be the primary key of the parent table, or covered by a unique constraint, and the types must be compatible. You can add a foreign key to an existing table with `ALTER TABLE ... ADD CONSTRAINT` and remove it with `DROP CONSTRAINT`; adding one checks the existing data first, and names the first offending record if it fails.

#### What the rules do

| Rule | When a parent record is deleted (or its key changes) |
| --- | --- |
| RESTRICT (default) | The change is refused while any child record refers to it. |
| CASCADE | Deleting the parent deletes its children; changing its key changes theirs. |
| SET NULL | The children's foreign key fields become `NULL`. The fields must allow it. |

Cascades follow chains: deleting a customer can delete its orders, which delete their order lines. The whole cascade is part of the original statement, so it is atomic with it and is undone with it. Inserting or updating a child whose parent does not exist is refused: *cannot save the record: 'Orders' (CustomerID) requires a matching record in 'Customer'*. A foreign key field that is `NULL` is allowed, and means "no parent".

The engine creates an index on the child's foreign key fields automatically if there isn't one, because without it every parent delete would have to scan the whole child table to look for dependants.

#### Lookups in the desktop app

The datasheet and the form know about relationships. A foreign key cell is edited through a drop-down that lists the parent table's records by their first text field, so you pick "Ada Lovelace" rather than typing the number 7. The design view has a **Relationships** panel in which you can add, edit and remove foreign keys without writing SQL.

### Calculated fields

A calculated field holds an expression instead of data. Its value is worked out from the other fields of the same record, and it is brought up to date every time the record is written.

```sql
CREATE TABLE OrderDetail (
  ID         COUNTER PRIMARY KEY,
  Quantity   INTEGER NOT NULL,
  UnitPrice  CURRENCY NOT NULL,
  Discount   DECIMAL DEFAULT 0,
  LineTotal  CALCULATED AS (Quantity * UnitPrice * (1 - Discount))
);

INSERT INTO OrderDetail (Quantity, UnitPrice, Discount) VALUES (3, 2.50, 0);
SELECT LineTotal FROM OrderDetail;     -- $7.50
```

You select from it like any other field, can filter and sort by it, and can index it. You cannot insert into it or update it; the engine will say so. Its type is whatever the expression produces, and in the example above it is `CURRENCY`, because currency times integer times decimal is currency.

#### What an expression may use

The formula may use the table's other fields, constants, operators and any of the built-in functions that give the same answer each time they are called. It may not use `Now()` or `Date()` (the result would go stale the moment it was stored), subqueries, parameters or aggregates, and it may not refer to another calculated field defined later in the table. If you write an expression that breaks one of these rules, or refers to a field that does not exist, or simply does not parse, you get an error message that says which, at the point you define it, rather than at some later write.

#### Stored or computed?

Conceptually only the expression is stored. In practice the engine keeps the latest result with each record so that reading the field is as cheap as reading any other, and so that an index on it can be maintained. Both facts are invisible to you, with one visible consequence: because the value is recomputed on each write rather than on each read, it always matches the formula as it stood when the record was last saved, and changing the formula recomputes every record immediately.

#### Turning a field into a calculated one

You can change an existing field to calculated, in the design view by choosing the **CALCULATED** type and typing a formula, or in SQL with `ALTER COLUMN ... CALCULATED AS (...)`. The old values are discarded and replaced by the results of the formula. The design view offers a **Calculate…** button that previews the formula against your data first.

### Saved and parameterised queries

A query you will want again can be saved under a name. It then appears beside the tables, in the navigation pane of the desktop app and in the shell's `.queries` list, and it can be used in the `FROM` clause of another query exactly as a table can.

```sql
CREATE VIEW BigOrders AS
    SELECT * FROM Orders WHERE Total > 100;

SELECT C.Name, B.Total
FROM Customers C JOIN BigOrders B ON B.CustID = C.ID;
```

`VIEW` and `QUERY` mean the same thing; Access calls them queries. Only the text is stored, so a saved query always sees the current data, and it can be changed by dropping and re-creating it. A saved query that ends up referring to itself, directly or through others, is stopped with an error rather than looping.

#### Parameters

This is Accessor's answer to the stored procedure. A saved query can declare parameters, whose names start with `@`, optionally with types. Wherever a literal could stand, a parameter can.

```sql
CREATE QUERY OrdersBetween (@lo DATETIME, @hi DATETIME) AS
    SELECT * FROM Orders WHERE Placed BETWEEN @lo AND @hi;

EXEC OrdersBetween #2024-01-01#, #2024-03-31#;
EXEC OrdersBetween @hi = #2024-03-31#, @lo = #2024-01-01#;
```

Arguments are matched by position, or by name with `@name = value` in any order. A declared type converts the argument, so passing text `'2024-01-01'` to a `DATETIME` parameter works. `EXECUTE` and `CALL` are synonyms for `EXEC`. A query with parameters cannot be used in a `FROM` clause, because there would be nowhere to give the values; run it with `EXEC`. The desktop app asks for the values in input boxes above the results.

A saved query can also be a call of another saved query with fixed arguments:

```sql
CREATE QUERY Q1 AS EXEC OrdersBetween #2024-01-01#, #2024-03-31#;
```

### Triggers

A trigger runs statements automatically when a record is inserted, updated or deleted. Triggers are how you keep an audit trail, maintain a summary, or refuse changes that no constraint can express.

```sql
CREATE TRIGGER audit_price AFTER UPDATE ON Product
WHEN OLD.Price <> NEW.Price
BEGIN
    INSERT INTO PriceLog (ProductID, OldPrice, NewPrice, At)
    VALUES (OLD.ID, OLD.Price, NEW.Price, Now());
END;

CREATE TRIGGER no_oversell BEFORE INSERT ON OrderLine
WHEN NEW.Qty > (SELECT Stock FROM Product WHERE ID = NEW.ProductID)
BEGIN
    RAISE 'Not enough stock';
END;
```

A trigger has a timing (`BEFORE` or `AFTER`), an event (`INSERT`, `UPDATE` or `DELETE`) and a table. It fires once for each record. The optional `WHEN` condition is tested first, and the body runs only if it is true. In the condition and the body `NEW.column` names the record being written and `OLD.column` the record as it was.

The body is a list of `INSERT`, `UPDATE`, `DELETE` and `RAISE` statements. `RAISE 'message'` stops the statement that caused the trigger and undoes everything it has done so far, which is what makes a `BEFORE` trigger a good place for validation.

Triggers also fire for the changes made by foreign-key cascades and by other triggers, to a depth of sixteen; after that the statement fails with a message. A trigger cannot insert into its own table. Everything a statement causes, triggers included, is one atomic unit. When the log is replayed after a crash, the finished effects are replayed without running the triggers a second time.

### Transactions

Without any ceremony, each statement is its own transaction: it takes effect completely, and is on disk when the call returns, or it has no effect at all. A transaction groups several statements so that they succeed or fail together.

```sql
BEGIN;
INSERT INTO Orders (CustID, Total) VALUES (1, 50);
SAVEPOINT after_order;
INSERT INTO Lines (OrderID, Item) VALUES (1, 'x');
ROLLBACK TO after_order;        -- undo just the line
COMMIT;                         -- keep the order
```

The rules are short. A statement that fails inside a transaction is undone by itself and the transaction stays open, so you can correct and continue. Schema changes take part: creating, altering and dropping tables, indexes, queries and triggers can all be rolled back. `COMMIT` writes the whole transaction to the log as one checksummed record, so after a crash it is wholly there or wholly absent. Transactions do not nest (use savepoints for that), checkpointing and compacting are refused while one is open, and closing a database with a transaction open rolls it back. There is one connection to a database at a time (the file is locked, see [locking](extern/locking.md)), so a transaction simply sees its own changes at once.

From Rust, `db.transaction(|db| { ... })` runs a closure inside a transaction, committing if it returns `Ok` and rolling back if it returns `Err`. The desktop app has a Transaction menu and shows a status-bar banner with **Commit** and **Roll back** buttons while one is open; closing the window or opening another database with one open asks what to do.

### Numbers

Most databases offer a handful of numeric types that behave differently in subtle ways, and money in a floating-point column is the classic way to lose a cent. Accessor has seven numeric types that mix predictably, and it makes the exact ones the default.

| Type | Representation | Use it for |
| --- | --- | --- |
| INTEGER | 64-bit whole number | counts, identifiers |
| BIGNUM | whole number of any size | factorials, big keys, exact products |
| DECIMAL | exact decimal of any size | measurements, rates |
| CURRENCY | exact decimal, shown with `$` | money |
| RATIONAL | a fraction of two BIGNUMs | exact thirds, ratios |
| FLOAT | 32-bit floating point | data that came in as floats |
| DOUBLE | 64-bit floating point | science, `Sqr`, `Exp` |

#### What type does an operation give?

The result type depends only on the types of the operands, never on their values, so a query cannot change type halfway down a column. When the operands differ, the one that is higher in this list wins:

**DOUBLE > FLOAT > RATIONAL > CURRENCY > DECIMAL > BIGNUM > INTEGER**

- Integer arithmetic never overflows: when an `INTEGER` result does not fit in 64 bits it becomes a `BIGNUM`. `SELECT 9223372036854775807 + 1` gives 9223372036854775808.
- `/` is exact division and gives a `DECIMAL`: `1 / 4` is 0.25. When the quotient does not terminate (`1 / 3`) it is rounded to 50 digits. Money divided by a number is money; money divided by money is a plain `DECIMAL`; `RATIONAL` division stays exact. Use `\` for whole-number division and `MOD` or `%` for the remainder.
- `^` is exact when the exponent is a whole number (`2 ^ 100` is a `BIGNUM`); otherwise it is computed in floating point.
- Literals follow the same plan: `5` is an `INTEGER`, `5.0` a `DECIMAL`, `5e0` a `DOUBLE`, and a whole number too large for 64 bits a `BIGNUM`.
- Floating-point results are never NaN or infinite: a calculation that would be one is an error instead, so such values cannot end up in a table.

#### Comparison is by exact value

Comparing, sorting, grouping, joining and indexing all compare numbers by their exact value across all seven types, so an `INTEGER` column can be joined to a `DECIMAL` one and an index on one can serve a probe by the other. The consequence for floats is honest, if surprising: a `DOUBLE` holding the nearest binary number to 0.1 is not equal to the decimal 0.1, because it is not exactly 0.1. `CDbl(0.1) = 0.1` is No. Convert with `CDec` when you want the shortest decimal.

```sql
SELECT 0.1 + 0.2 = 0.3;                      -- Yes   (DECIMAL)
SELECT CDbl(0.1) + CDbl(0.2) = CDbl(0.3);    -- No    (DOUBLE)
SELECT TypeName(2 ^ 100), 2 ^ 100;           -- BIGNUM, 1267650600228229401496703205376
SELECT Rational(1, 3) + Rational(1, 6);      -- 1/2
SELECT CCur(19.99) * 3;                      -- $59.97
```

### Dates and times

A `DATETIME` is a whole number of seconds since 1970-01-01 00:00:00, in a single time zone. There are no zones and no fractions of a second; the type is meant for the dates of an office, not for time-stamping an experiment. A value at midnight is shown as a plain date (`2024-03-05`), anything else with its time (`2024-03-05 14:30:00`).

Write a date between hash marks (`#2024-03-05#`, `#2024-03-05 14:30#`, or the US form `#3/5/2024 2:30 PM#`) or in the standard way, `DATE '2024-03-05'` and `TIMESTAMP '2024-03-05 14:30:00'`. Text is converted to a date wherever a date is needed.

Adding a number to a date moves it by that many days (a half is twelve hours), and subtracting two dates gives the difference in days. Compared with a plain number a date is its day count from 1899-12-30, as in Access, so spreadsheet dates import sensibly.

```sql
SELECT #2024-03-05# + 30;                         -- 2024-04-04
SELECT DateAdd('m', 1, #2024-01-31#);             -- 2024-02-29  (clamped to the month's end)
SELECT DateDiff('yyyy', #2020-12-31#, #2021-01-01#);   -- 1  (a year boundary was crossed)
SELECT DateSerial(2024, 3, 0);                    -- 2024-02-29  (the day before 1 March)
SELECT EXTRACT(QUARTER FROM DATE '2024-11-05');   -- 4
SELECT Format(#2024-03-05#, 'dddd, mmmm d, yyyy');   -- Tuesday, March 5, 2024
```

`DateDiff` counts boundaries crossed for years, quarters, months and days, which is what a report of "years between" usually wants, and elapsed whole units for weeks, hours, minutes and seconds. The functions are described in full in the [date function reference](functions/datetime.md).

### The desktop app

The desktop application (`accessor-gui`) is the part that most resembles Access. It is a single window with a navigation pane on the left and a set of tabs on the right. It is built with the `egui` toolkit, so it is one native executable with nothing to install and it looks the same on Windows, macOS and Linux.

#### The navigation pane

The pane lists the database's **Tables**, **Queries** and **Triggers**. Double-click a table to open its datasheet; right-click for *Open*, *Form view*, *Design view* and *Delete…*. A query opens in the query editor, where it can be edited and run, and a trigger can show its definition or be deleted. Deletions ask for confirmation. The **Create** menu makes a new table, query, parameter query or trigger.

#### The datasheet

The datasheet shows a table as a grid. It is virtualised, so only the visible rows are drawn and a table of a million records scrolls as smoothly as one of ten. Click a column heading to sort by it. The search box filters the grid. Double-click a cell to edit it, press `Enter` to store the change or `Esc` to abandon it. A value that does not fit the column's type or breaks a constraint is refused with a message, and the cell keeps its old value.

The last row of the grid is a blank *new record* row. Type into it: the half-typed record is kept in the grid as a draft, not lost, until every required field has a value, and then it is saved. AutoNumber fields are filled in by the database. Cells of [calculated fields](#calculated-fields) are read-only. A field that is a foreign key shows a drop-down of the records it may refer to, by their first text field, so you choose a customer by name.

#### Form view

The form shows one record at a time, with a labelled box for each field and buttons to move between records, to add a new one and to delete. It uses the same editing rules as the datasheet, including the drop-downs for relationships.

#### Design view

The design view is where a table's structure is changed, as in Access, without writing SQL. Each field is a row: rename it, change its type, mark it required or its default, delete it, or add a new one at the bottom. Choosing `CALCULATED` as the type asks for a formula; the **Calculate…** button previews it against your data. A **Relationships** panel adds, edits and removes foreign keys, with their delete and update rules, and an **Indexes** panel shows the table's indexes. Changes are applied together when you save them, as `ALTER TABLE` statements, and a change the data cannot support (a type the stored values do not convert to) is refused with an explanation, leaving the table as it was.

#### Queries

The query editor is a box for SQL and a grid for the result. `Ctrl`+`Enter` runs the text. A parameterised query shows an input box for each parameter. **Save as query** keeps the text under a name so that it appears in the navigation pane. Errors are shown in red under the editor with the position of the problem.

#### Transactions, files and CSV

The **Transaction** menu begins, commits and rolls back, and sets savepoints. While a transaction is open a banner in the status bar says how many changes it holds and has **Commit** and **Roll back** buttons; closing the window or opening another database with one open asks what to do. The **File** menu creates and opens databases, imports a CSV file as a new table, exports the current grid as CSV, loads the sample data, and runs *Compact and repair*, which rewrites the file without the gaps left by deleted records.

### The shell

The shell (`accessor`) is a line-oriented SQL client for the terminal and for scripts.

```text
accessor [FILE.adb] [-c SQL]... [-f SCRIPT]...
```

With no file name it works on a throw-away database in memory. With `-c` and `-f` it runs the given SQL text or script files in order and exits, with a non-zero status if anything failed (or if a transaction was left open, since its work was discarded). With neither, it reads statements interactively, or from a pipe. Statements end with a semicolon and may span lines; the prompt shows `...>` while a statement is unfinished, and a `*` while a transaction is open.

Results are printed as aligned tables, numbers right-aligned, NULL written out, with a row count. Lines beginning with a dot are commands to the shell itself:

| Command | What it does |
| --- | --- |
| `.tables`, `.queries` | list the tables, or the saved queries |
| `.schema [TABLE]` | print the SQL that would re-create a table (or, with no name, everything: tables, queries and triggers). The output can be fed back to the shell to copy a database |
| `.indexes TABLE` | show a table's indexes |
| `.import FILE TABLE` | append the rows of a CSV file to a table, matching the header to the field names |
| `.import --new FILE TABLE` | create the table from the CSV, guessing each column's type |
| `.export FILE SQL` | write a query's result as CSV |
| `.read FILE` | run a SQL script |
| `.sync on\|off` | flush to disk on every commit (the default) or leave it to the operating system |
| `.timer on\|off` | show how long each statement takes |
| `.optimizer on\|off` | turn the query optimizer off to compare plans and timings (the answers are the same) |
| `.checkpoint`, `.compact` | fold the log into the database file; rewrite the file compactly |
| `.help`, `.quit` | help; leave |

```text
$ accessor shop.adb -c ".import --new customers.csv Customers" -c "SELECT City, Count(*) FROM Customers GROUP BY City"
$ accessor shop.adb -f nightly.sql
$ accessor shop.adb -c ".schema" > backup.sql
$ accessor copy.adb -f backup.sql
```

### Using the library

Everything the application and the shell do goes through the `accessor-core` crate, which you can use directly. Add it to `Cargo.toml` as a path or git dependency and open a database:

```rust
use accessor_core::{Database, Value};

fn main() -> Result<(), accessor_core::Error> {
    let mut db = Database::open("shop.adb")?;      // created if it does not exist
    db.execute("CREATE TABLE IF NOT EXISTS Customer (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL)")?;
    db.execute("INSERT INTO Customer (Name) VALUES ('Ada'), ('Grace')")?;

    let r = db.query("SELECT ID, Name FROM Customer ORDER BY Name")?;
    println!("{:?}", r.columns);                      // ["ID", "Name"]
    for row in &r.rows {
        println!("{} {}", row[0], row[1]);
    }
    Ok(())
}
```

`Database::in_memory()` makes a database that lives only in the process. `execute` runs one or more statements and returns the result of the last; `query` runs a single `SELECT` and needs only a shared reference. Both return a `QueryResult` with `columns`, `rows` (a `Vec` of `Vec<Value>`) and `affected`, the number of rows a data statement changed.

#### Values and parameters

`Value` is an enum with one variant for each type: `Int`, `Big`, `Float`, `Double`, `Dec`, `Currency`, `Rational`, `Text`, `Bool`, `DateTime` and `Null`. It implements `Display` (the same text the shell prints), `Eq`, `Ord` and `Hash`, with the exact cross-type comparison of the SQL engine. To pass values into a statement without building SQL text, use a parameterised statement:

```rust
let r = db.execute_with_params(
    "PARAMETERS @id Integer; SELECT Name FROM Customer WHERE ID = @id",
    &[("id", Value::Int(2))],
)?;
```

#### Transactions

```rust
db.transaction(|db| {
    db.execute("INSERT INTO Orders (CustID, Total) VALUES (1, 10)")?;
    db.execute("INSERT INTO Orders (CustID, Total) VALUES (99, 10)")?;   // no customer 99
    Ok(())
})?;   // returns the error, and the first insert is rolled back too
```

The error carries the message you would see in the shell, such as *cannot save the record: 'Orders' (CustID) requires a matching record in 'Customer'*. `Error` has the variants `Syntax`, `Catalog` (unknown or duplicate names), `Constraint`, `Type`, `Runtime`, `Corrupt` and `Io`, so a program can tell a mistake in SQL from a refused record.

#### Records without SQL

A user interface that edits a grid does not want to build SQL strings, so there is a cell-level interface: `insert_row(table, &[(column_position, value)])` returns the new row's id, `update_cell(table, id, column, value)` changes one cell, and `delete_row(table, id)` removes a record. They apply the same conversions, constraints, foreign keys and triggers as the SQL statements do. The `csv` module reads and writes CSV: `csv::write_csv` and `csv::import_csv_as_table`, which guesses a column type from the data.

#### Durability and maintenance

`db.set_durability(Durability::OsBuffered)` trades safety against power loss for speed (see [Durability and recovery](#architecture)). `db.checkpoint()` folds the log into the snapshot and `db.compact()` rewrites the file without gaps; both are refused while a transaction is open. When a `Database` is dropped with a transaction open, the transaction is rolled back.

The library is synchronous. `Database` is `Send` and `Sync` (a test keeps it so): a connection can be moved to another thread, and several threads can share a `&Database` to run `query` and `query_prepared` at the same time. Mutating calls take `&mut self`, so the borrow checker allows one writer at a time and no readers while it writes; to share a writable database between threads, with a transaction for each, use `SharedDatabase` and `Connection`: transactions overlap, each seeing a snapshot of the database as it was when it began, readers never wait for writers, and two transactions that change the same row are told ([concurrency](concurrency.md)). Statements themselves are run one at a time. Sharing one file between processes is refused by a file lock (see [locking](extern/locking.md)).

### Architecture

Accessor is a Cargo workspace of five crates, about 35,600 lines of Rust in all (counting the tests that live beside the code), with 305 Rust tests alongside; the JDBC driver adds about 7,000 lines of Java.

| Crate | Lines | What it is |
| --- | --- | --- |
| `accessor-core` | 23,600 in `src`, 5,600 in `tests` | The engine: types, SQL, storage, durability, concurrency. A library with no user interface and few dependencies (`serde` and `num-bigint` are the notable ones). |
| `accessor-cli` | 500 | The shell, a thin loop around the library. |
| `accessor-gui` | 3,250 | The desktop application, on `egui`/`eframe`. It uses the library's public API only, as any other program would. |
| `accessor-capi` | 1,030, plus 430 of fuzz targets | The C interface (`accessor.h`, generated), with 670 lines of C tests. |
| `accessor-slt` | 600 | A runner for SQLite's *sqllogictest* files (see [extern/testing.md](extern/testing.md#sqllogictest)). |
| `jdbc/` (Java, not a crate) | 7,000 (2,800 of it generated stubs), plus 800 of tests | The JDBC driver, over the C interface. |

(Counted with `wc -l` over the source files, blank lines and comments included.)

```text
  Desktop app            Shell                Your Rust program
  (egui views)           (dot-commands)       (Database::open(...))
        \                   |                    /
         v                  v                   v
  +-----------------------------------------------------------+
  | Database: execute, query, transaction, cell API           |
  |  one writer; SharedDatabase: overlapping transactions     |
  +-----------------------------------------------------------+
        |                       |                        |
        v                       v                        v
  +-----------------+  +--------------------+  +--------------------+
  | SQL front end   |  | Planner/executor   |  | Rules              |
  | lexer, parser,  |  | binding, access    |  | foreign keys,      |
  | syntax tree,    |  | paths, joins,      |  | triggers, calc     |
  | parameter / NEW |  | grouping, windows, |  | fields, checks     |
  | / OLD rewriting |  | optimizer, DML ops |  |                    |
  +-----------------+  +--------------------+  +--------------------+
                                |
                                v
  +----------------------------------+  +------------------------------+
  | Tables and indexes (in memory)   |  | Snapshot + write-ahead log   |
  | slot vectors, ordered index sets,|  | name.adb and name.adb-wal,   |
  | numeric tower, dates             |  | checksums, atomic replace    |
  +----------------------------------+  +------------------------------+
```

*From the three front ends down to the files. Everything except the last box is in memory.*

#### The life of a statement

Take `UPDATE Product SET Price = Price * 1.05 WHERE Price < 10`.

1. **Lex and parse.** A hand-written lexer and recursive-descent parser turn the text into a syntax tree (`Statement`, `Select`, `Expr`). It is written by hand so that the error messages can say what was expected and where.
2. **Rewrite.** Parameters (`@name`) and trigger row references (`NEW.col`, `OLD.col`) are replaced with literals by a pass over the tree, so the rest of the engine never has to know about either.
3. **Bind.** Names are resolved to positions in the row (`Price` becomes "column 3"), function names to enum variants, and constant sub-expressions are folded. The result is a *bound expression* that can be evaluated against a row with no lookups.
4. **Plan.** For a `SELECT` the planner picks an access path per table (index prefix, index range, or full scan), a method per join, and works out grouping, windows, sorting and limits. For a data statement it finds the affected records, using an index when the `WHERE` allows, and produces a list of *operations*: insert this row, replace that one, delete another.
5. **Apply.** The list of operations is applied as one atomic batch. First calculated fields are finalised, then each operation runs through the rule checks: unique indexes, `NOT NULL`, foreign keys, and the triggers, whose own operations join the batch. If anything fails, everything done so far is undone.
6. **Log.** A successful batch is appended to the write-ahead log as a single checksummed record (or queued in the transaction, which writes it all at commit).

#### Storage in memory

Each table is a vector of *slots*, each holding a record or nothing. A record's position is its stable *row id*; deleting leaves a tombstone, so ids held by indexes and in-flight operations never move. Each index is an ordered set of `(key values, row id)` pairs. Because the set is ordered, one structure serves equality lookups, range scans and ordered output, and because the row id is in the key, non-unique indexes need no special treatment. The AutoNumber counter is kept with the table.

Values are a Rust enum with a variant for each type. Numbers carry the full numeric tower: `Int(i64)`, `Big(BigInt)`, `Dec(Decimal)` (a big integer mantissa and a scale), `Rational` (two big integers), and the two float types. Comparison, hashing and ordering are implemented once, across all of them, by exact value, and everything built on those (indexes, hash joins, `GROUP BY`, `DISTINCT`) inherits the same notion of equality.

#### Execution

Rows are plain vectors of values and the executor works on whole result sets, a simple choice that suits an in-memory engine and keeps the code short. A few techniques carry most of the performance:

- **Access paths.** Conjuncts of the `WHERE` that compare a column to a constant are matched to the best index: a unique equality beats a prefix equality beats a range. The full predicate is still applied afterwards, so the index only has to find a superset.
- **Joins.** An index on the inner table's join columns gives an index nested-loop join. Otherwise an equality join builds a hash table of the right-hand side. Outer joins keep a matched flag per row, which also gives `RIGHT` and `FULL` joins.
- **Top-k.** `ORDER BY ... LIMIT k` does not sort everything: it partitions the rows around the k-th and sorts only the winners.
- **Patterns.** A `LIKE` pattern is compiled once, and a literal prefix, suffix or "contains" shape takes a fast path that skips the general matcher.
- **Constant folding** at bind time turns `2 * 60 * 60` into `7200` once.

The newer SQL features are built from the same parts. A *correlated subquery* becomes a hidden column: the binder replaces it with a reference to an extra column, and the executor fills that column for each row by running the subquery with that row's values substituted in, remembering answers by the values it was given. A *window function* is the same idea at the level of the whole result: the rows are sorted by partition and order, the function is computed in one pass per partition (a growing frame feeds each row to the aggregate once), and the answers are appended as hidden columns for the select list to read. A *common table expression* is run once and substituted into the statement as already-computed rows; a recursive one loops, feeding each round's new rows back in. `ROLLUP` and `CUBE` run the grouping once per grouping set and add a mask column for `GROUPING()`. A `CHECK` constraint is a pair of triggers.

#### Durability and recovery

The database is two files. `name.adb` is a *snapshot*: a header (magic `ACDB`, format version, an epoch number, a CRC-32 and a length) followed by the whole database serialised. `name.adb-wal` is a *write-ahead log*: a header (`ACWL`, version, epoch) followed by frames of `length, CRC, payload`, each frame one committed batch of operations.

- **Commit** appends the frame and, by default, calls `fsync`, so a committed change survives power loss.
- **Open** reads the snapshot, then replays the log frames over it in order. A frame whose checksum fails, or that is cut off, is a crash in the middle of a write: it and everything after it are discarded and the file is trimmed. The result is exactly the state after the last complete commit.
- **Checkpoint** writes a new snapshot to a temporary file, syncs it, and renames it over the old one, which is atomic on every platform that matters, then starts a fresh log. The log carries the epoch of the snapshot it extends, so if the process dies between the rename and the log reset, the stale log is recognised and ignored rather than replayed twice. A checkpoint also happens automatically when the log reaches 64 MiB.
- **Compaction** is a checkpoint that also renumbers the records to close the gaps left by deletes.
- **Durability levels.** `Durability::Fsync` (the default) survives power loss; `Durability::OsBuffered` hands data to the operating system without waiting for the disk, which survives a crash of the process but not of the machine, and is nearly ninety times faster for single-row commits.

#### Transactions and the undo log

Changes are applied immediately inside a transaction, so reads see them, and for each change an undo entry is recorded: either the inverse row operation, or, for schema changes, a copy of the part of the catalog that was about to change. A savepoint is a position in that undo list; rolling back to it replays the entries after it, backwards. The operations themselves are held back and written to the log as one frame only at `COMMIT`, which is why a crash mid-transaction loses it cleanly. A failing statement uses the same machinery for statement atomicity.

#### File format and compatibility

The format is versioned (currently 6) and every earlier version is still readable: versions 1 and 2 are older layouts, and the short-lived single `NUMBER` type of an intermediate build is read as `DECIMAL`. Version 6 added descending index columns. Old files are upgraded by rewriting them on first open. The on-disk enumerations keep their original variant order, with the new types appended, so that a file never changes meaning.

#### Concurrency

A `Database` used directly has one writer at a time: the mutating API takes `&mut Database`, so Rust enforces it within a program. `SharedDatabase` lets several threads each run transactions at once, with any number of readers and several writers whose transactions overlap ([concurrency](concurrency.md)). Across connections and processes the database file is **locked**: opening a database that another connection (in this process or any other) has open fails with `Error::Locked`, and the lock is released when the connection closes or the process dies, however it dies. See [locking](extern/locking.md).

#### Testing

The 305 Rust tests run in a few seconds: 82 unit tests in the engine (the number types, the date calendar, the pattern matcher, the parser), 129 SQL tests covering each feature through the public API, 37 persistence tests that close, reopen, truncate and corrupt files, replay logs and test the file lock, 23 tests of overlapping transactions (snapshots, conflicts, deadlocks, recovery, two stress tests), 8 of connections shared between threads, 3 that keep `Database` `Send` and `Sync`, 3 documentation examples, and 18 that drive the desktop application headlessly, feeding it pointer events and reading back what it drew, so that transaction dialogs and the datasheet are exercised without a screen. Outside `cargo test` there are 1,256 checks in the C test suite (run plainly, under Valgrind and under AddressSanitizer), 23 groups in the JDBC driver's own suite, and tests of the driver from plain Java (43 checks), next.jdbc and HoneySQL (46 assertions) and Common Lisp (72 checks); see [extern/testing.md](extern/testing.md). The whole Rust suite is also run with every database in version mode (`ACCESSOR_FORCE_MVCC=1`).

### The query optimizer

A query says *what* rows you want; the optimizer decides *how* to get them. It chooses which table to read first, which index to use, where to apply each condition and whether a sort can be skipped, and it never changes the answer, only the time. It is on by default and needs no set-up: there are no statistics to gather and no hints to write, because it reads what it needs from the tables as they are when the query runs.

#### A worked example

Take three tables, a million rows in the biggest, and a query that mentions the big table first:

```sql
SELECT Count(*)
FROM Line L
JOIN Ord  O ON O.ID = L.OrdID
JOIN Cust C ON C.ID = O.CustID
WHERE C.Name = 'c77';
```

Run exactly as written, this scans every line, looks up each line's order, and only then asks whether the customer is the right one: about 2.6 seconds. The optimizer sees that `C.Name = 'c77'` leaves a single customer, so it starts there, finds that customer's orders through an index on `Ord.CustID`, and the orders' lines through an index on `Line.OrdID`. The answer takes 1.8 milliseconds. `EXPLAIN` in front of the query shows the choice:

```text
query over L, O, C
join order: C -> O -> L (reordered from the query's order)
scan C [50 rows]: full scan (1 condition(s) pushed down)
  -> est 5 rows, actual 1
join O: index nested-loop via ix_ord_cust (1 key column(s)); 1 rows in, 8 out ...
join L: index nested-loop via ix_line_ord (1 key column(s)); 8 rows in, 40 out ...
40 row(s) returned
```

#### What it does

1. **Pushdown.** The `WHERE` and inner-join `ON` conditions are split at `AND` and each piece is labelled with the tables it reads. A piece that reads one table is applied while that table is scanned, where it can also choose an index, so the join sees only rows that can matter. A piece that reads several is applied at the first join where all its tables are present. A condition is never pushed to the nullable side of an outer join: `WHERE Q.id IS NULL`, the way to find unmatched rows, tests the padding the join adds, and applying it to `Q` first would return every row.
2. **Join order.** Inner and cross joins can be done in any order, so the optimizer chooses. It estimates each table's rows after its own conditions, starts with the smallest, and adds, one at a time, the table that leaves the smallest intermediate result, ranking a unique-index lookup above an ordinary index, above a hash, above a cross product. Tables with no join condition go last. The rows are built in that order and then rearranged to the column order you wrote, so `SELECT *` is unaffected. Joins with a `LEFT`, `RIGHT` or `FULL` keep the order written.
3. **Join method.** An index on the joined table's join columns gives an index nested-loop join, one lookup per row. Otherwise an equality is hashed, building on the smaller side; a join with no equality is a nested loop.
4. **Access paths.** Each table is read through the best index for its conditions: a unique key with every column equated beats a prefix equality beats a range beats a scan. All the conditions are still tested on every row, so an index can only speed a query up.
5. **Ordered reads.** `ORDER BY ... LIMIT n` on one table, on plain columns an index holds in that order (perhaps after an equality on its leading columns), reads the index and stops after *n* qualifying rows, with no sort. Descending reads preserve the tie order of a stable sort, so the answer is identical.
6. **One-pass crosstabs.** `TRANSFORM … PIVOT` is computed by grouping once by the row headings and the pivot value, then laying the cells out, instead of testing every row against every column's condition. With 100 pivot columns over 100,000 rows that is 0.52 s instead of 1.26 s, and the gap grows with the columns. The two methods give identical results. See [optimizer/crosstab.md](optimizer/crosstab.md).
7. **Semi-joins.** A correlated `EXISTS`, `IN` or aggregate sub-query whose correlation is an equality is run once and looked up, instead of once per outer row's key; without an index on the inner column, 8 ms instead of 12 s. See [optimizer/subqueries.md](optimizer/subqueries.md).
8. **Merged derived tables.** `FROM (SELECT cols FROM t WHERE ...) AS d` and views like it are read as the table `t`, so the outer query can use `t`'s indexes.
9. **Free counts.** `SELECT COUNT(*) FROM t` with no condition is the table's row count: 0.02 ms over a million rows against 260 ms when scanned.

#### Estimates and statistics

The optimizer's guesses are made from the tables as they are, and nothing about them is stored. A table's size is known exactly. For a condition an index can answer, the estimate is the exact number of index entries it would visit, counted up to 4,096; a unique key is exactly one row. For a condition no index answers, it uses *column statistics*: the number of distinct values, the commonest values with their shares, and a histogram of the rest. They are worked out from the rows the first time a query needs them (from a sample, for a big table) and thrown away when the table changes, so a column that is 95% `'open'` is known to be, and a range such as `v > 1800` over values from 0 to 1999 is estimated at a tenth of the table, not a third. A join on an equality is estimated from the distinct counts of the two columns, so a many-to-many join is seen for the explosion it is. Nothing is stored in the file, there is nothing to run, and nothing goes stale after a bulk load. `.stats table` in the shell shows them, and `EXPLAIN` prints the estimate beside the real count so that a poor guess is easy to see.

#### EXPLAIN

`EXPLAIN` (or `EXPLAIN QUERY PLAN`) in front of a `SELECT` runs the query, read-only, and returns instead of its rows one line of text per step: the tables, the join order, each scan's access path and condition count, the estimated and actual rows, each join's method and the rows in and out, and any sort or shortcut. Sub-queries, derived tables and common table expressions appear indented. In the shell the lines are printed in full; in the desktop app they fill the result grid; from Rust, `db.query("EXPLAIN ...")` returns a one-column result.

#### Switching it off, and its limits

`.optimizer off` in the shell, or `db.set_optimizer(false)`, runs queries exactly as written: joins in the written order, conditions applied after the joins, no ordered index reads or count shortcut. The answers are the same, only slower, which is how it is tested: a test builds data with NULLs, ties and several index shapes, runs nearly forty query shapes with the optimizer on and off, and requires identical answers and identical row order.

It has limits. Outer joins are not reordered; more than 128 tables in a `FROM` are joined as written; sub-queries, views and common table expressions are planned on their own, so a condition on a derived table is not pushed inside it; the join search is greedy, not exhaustive; and plans are not cached (the parsed SQL is, and statements can be prepared). The usual cure for a slow query is an index on the filtered or joined column, then a look at the plan. The [tuning guide](optimizer/tuning.md) lists the symptoms and remedies, and the rest of the [optimizer reference](optimizer/README.md) goes into each decision in detail.

### Performance

These figures are from a release build on the author's machine with 200,000-row tables, as an indication of scale rather than a promise. They are per row, so a statement over a hundred thousand rows takes a hundred thousand times as long.

| Operation | Time per row |
| --- | --- |
| Insert (API), indexed table | 2.6 µs |
| Point lookup by primary key, including parsing the SQL | 4.3 µs |
| Indexed range scan | 0.5 µs |
| Full scan with a filter | 0.11 µs |
| `LIKE '*99*'` scan | 0.15 µs |
| `GROUP BY` with aggregates | 0.64 µs |
| Index nested-loop join plus grouping | 1.4 µs |
| Hash join | 1.0 µs |
| Top-k `ORDER BY ... LIMIT`; full sort | 0.6 µs; 0.75 µs |
| Update 1% of rows; delete a range | 0.27 µs; 2.0 µs |
| `DECIMAL` insert; `Sum`/`Avg` | 2.4 µs; 0.6 µs |
| `DOUBLE`, `RATIONAL`, `BIGNUM` sums | 0.4, 0.9, 0.4 µs |

The optimizer matters more than any of these micro-costs. On a database with 1,000,000 rows in the largest of three joined tables, `SELECT Count(*) FROM Line L JOIN Ord O ON O.ID = L.OrdID JOIN Cust C ON C.ID = O.CustID WHERE C.Name = 'c77'` takes about 1.8 ms with the optimizer and 2.6 s without it, because the customer is found first and the other tables are reached through their indexes instead of being scanned. A bare `SELECT Count(*)` over the million rows takes 0.02 ms against 260 ms, and a top-5 `ORDER BY` on an indexed column 19 ms against 42 ms.

Durability dominates writes. A committed single-row insert with `fsync` costs about 570 µs, almost all of it the disk; with `OsBuffered` it costs 6.4 µs. The way to make many inserts fast with full safety is to group them in one transaction, which pays the disk cost once. A checkpoint of a 200,000-row database takes a few milliseconds, and opening one (reading the snapshot) about 27 milliseconds.

### ASQL and standard SQL

The dialect is called ASQL. It is built to be familiar to someone who knows standard SQL or Access SQL, and it implements a large part of the core of the standard: the whole query language apart from a few corners, the usual data changes including `MERGE`, the common constraints, and transactions. **It does not implement the whole of the latest SQL standard (ISO/IEC 9075:2023), and it should not be described as doing so.** The standard has nine parts, and most of it is about things an embedded database does not have: users and privileges, stored procedures in a procedural language, client/server interfaces, XML and JSON data models, and so on.

The reference pages in [sql/conformance.md](sql/conformance.md) give the exact list of what is implemented, where ASQL deliberately differs (double quotes make text, text comparison ignores case, `/` is exact division), and what is missing: intervals and time zones, arrays and other collection types, `JSON` and `XML`, row-value comparisons, `LATERAL`, named windows, schemas and sequences, procedural routines, and privileges among them. A statement that uses an unsupported feature fails with a message; it is never quietly given a different meaning.

### Limits and what is missing

- **The optimizer is modest.** Its estimates are table sizes, exact counts for indexed conditions and histograms for the rest, with no knowledge of how columns are related; there is no `ANALYZE` (statistics come and go with the data) and no hints. Outer joins are not reordered, and plans are not cached.
- **The data must fit in memory.** The whole database is loaded on open. The 200,000-row benchmarks above use a few tens of megabytes.
- **One process at a time.** A database file is locked while open, so it is open once. Within that process, connections share it with overlapping transactions under snapshot isolation ([concurrency](concurrency.md)); statements run one at a time, write skew is possible, and a schema change needs every other transaction to be finished. There is no client/server mode and no shared read-only mode across processes.
- **No time zones, intervals or sub-second times.** A `DATETIME` is whole seconds in a single zone.
- **No fixed-width types.** `VARCHAR(n)` and `NUMERIC(p, s)` accept their sizes and ignore them; use `CHECK` constraints to limit values.
- **Correlated subqueries** work in the select list, `WHERE`, `GROUP BY`, `HAVING` and `ORDER BY` and in `UPDATE` and `DELETE`, but not in join conditions.
- **Windows:** no named windows, `GROUPS` frames, `EXCLUDE`, or `RANGE` offsets.
- **Views are read-only** and have no column list. A table cannot be renamed.
- **No stored procedures with control flow.** Parameterised queries and triggers cover the common uses.
- **No Access compatibility beyond the dialect.** The file format, forms, reports and macros are not Access's.

### Quick reference

The detailed reference lives beside this page, in the `docs` folder, as Markdown:

- [sql/](sql/README.md): every statement, the complete BNF grammar, expressions, and the comparison with standard SQL.
- [functions/](functions/README.md): every built-in function, with its arguments, result and examples.
- [types/](types/README.md): each column type, how types convert, and how they combine in arithmetic.
- [optimizer/](optimizer/README.md): pushdown, join order, access paths, row estimates, `EXPLAIN` and tuning.

#### Cheat sheet

```sql
-- tables
CREATE TABLE T (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL UNIQUE, Price CURRENCY CHECK (Price >= 0),
                Cat INTEGER REFERENCES Cat (ID) ON DELETE SET NULL, Tax CALCULATED AS (Price * 0.2));
ALTER TABLE T ADD COLUMN Notes TEXT;      CREATE INDEX ByName ON T (Name);     DROP TABLE IF EXISTS T;

-- queries
SELECT TOP 5 Cat, Count(*), Sum(Price) FROM T WHERE Name LIKE 'a*' GROUP BY Cat HAVING Count(*) > 1 ORDER BY 3 DESC;
SELECT a.Name, b.Name FROM T a JOIN T b ON a.Cat = b.Cat AND a.ID < b.ID;
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5) SELECT * FROM n;
SELECT Name, RANK() OVER (PARTITION BY Cat ORDER BY Price DESC) FROM T;

-- changes
INSERT INTO T (Name, Price) VALUES ('x', 1.50);   UPDATE T SET Price = Price * 2 WHERE Cat = 1;
DELETE FROM T WHERE NOT EXISTS (SELECT 1 FROM Cat WHERE Cat.ID = T.Cat);
MERGE INTO T USING (VALUES ('x', 2)) AS S (n, p) ON T.Name = S.n WHEN MATCHED THEN UPDATE SET Price = S.p;

-- programs
CREATE QUERY Cheap (@max CURRENCY) AS SELECT * FROM T WHERE Price <= @max;     EXEC Cheap 5;
CREATE TRIGGER t_log AFTER DELETE ON T BEGIN INSERT INTO Log (What) VALUES (OLD.Name); END;
BEGIN;  SAVEPOINT s;  ROLLBACK TO s;  COMMIT;
```
