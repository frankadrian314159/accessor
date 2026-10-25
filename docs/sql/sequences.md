# Sequences

A *sequence* is a named counter that hands out numbers. Unlike a `COUNTER` column, which belongs to one table, a sequence is its own object: several tables, or several statements, can draw from the same one, and a number can be taken before the row that uses it exists.

```sql
CREATE SEQUENCE invoice_no START WITH 1000 INCREMENT BY 1;

SELECT NEXT VALUE FOR invoice_no;                 -- 1000
SELECT NEXTVAL('invoice_no');                     -- 1001  (the same thing, written as a function)
SELECT CURRVAL('invoice_no');                     -- 1001  (the last number handed out)

CREATE TABLE Invoice (
    No     INTEGER DEFAULT NEXT VALUE FOR invoice_no PRIMARY KEY,
    Client TEXT
);
INSERT INTO Invoice (Client) VALUES ('Ada'), ('Grace');     -- numbers 1002 and 1003
```

## CREATE SEQUENCE

```sql
CREATE SEQUENCE [IF NOT EXISTS] name
    [AS type]
    [START [WITH] n]
    [INCREMENT [BY] n]
    [MINVALUE n | NO MINVALUE]
    [MAXVALUE n | NO MAXVALUE]
    [CYCLE | NO CYCLE]
    [CACHE n];
```

| Option | Default |
| --- | --- |
| `INCREMENT BY` | 1; may be negative, never 0 |
| `MINVALUE` | 1 for an ascending sequence, the smallest 64-bit integer for a descending one |
| `MAXVALUE` | the largest 64-bit integer for an ascending sequence, −1 for a descending one |
| `START WITH` | `MINVALUE` for an ascending sequence, `MAXVALUE` for a descending one |
| `CYCLE` | off: when the limit is reached the next call is an error. With `CYCLE` the sequence starts over at the other end |

`AS type` and `CACHE` are accepted so scripts written for other systems run; sequences always count in 64-bit integers and there is nothing to cache.

## ALTER SEQUENCE and DROP SEQUENCE

```sql
ALTER SEQUENCE [IF EXISTS] name
    [INCREMENT BY n] [MINVALUE n | NO MINVALUE] [MAXVALUE n | NO MAXVALUE]
    [START WITH n] [CYCLE | NO CYCLE]
    [RESTART [WITH n]];
DROP SEQUENCE [IF EXISTS] name;
```

`RESTART` makes the next call return the start value again (or `n`). Changes to the settings keep the current position.

## Getting numbers

| Form | Result |
| --- | --- |
| `NEXT VALUE FOR name` | the next number, standard syntax |
| `NEXTVAL('name')` | the same, as a function |
| `CURRVAL('name')` or `PREVIOUS VALUE FOR name` | the number the sequence last handed out; an error if it has handed out none |
| `LASTVAL()` | the number last handed out by any sequence |
| `SETVAL('name', n [, is_called])` | makes `n` the current number (so the next call gives `n + increment`); with `is_called` false the next call gives `n` itself |

See [functions/sequence.md](../functions/sequence.md). A query that calls `NEXTVAL` for each row of its input gives each row its own number, in the order the rows are produced:

```sql
SELECT NEXTVAL('invoice_no'), Client FROM Clients ORDER BY Client;
```

## As a column default

`DEFAULT NEXT VALUE FOR name` (or `DEFAULT NEXTVAL('name')`) fills the column from the sequence when an `INSERT` gives no value for it. A value that is given is used as it is and takes no number. The sequence must exist when the table is created. It can also be set later with `ALTER TABLE t ALTER COLUMN c SET DEFAULT NEXT VALUE FOR name`; a column of that kind cannot be *added* to a table that already has rows.

## Numbers are never given back

A sequence is not part of a transaction. If a transaction takes a number and is rolled back, or the statement that took it fails, the number is still used; the next call gives the one after it. This is what lets many users draw from one sequence without waiting for each other, but it means a column filled from a sequence can have gaps. Use `COUNTER` when you need a number to follow the row; use a sequence when you need one that is never reused.

The *definition* of a sequence, on the other hand, follows transactions like any other schema change: `CREATE SEQUENCE` can be rolled back.

The position of every sequence is written to the log as it moves and is kept in the database file at each checkpoint, so numbers are not reused after a crash or a restart.

## The optimizer

`NEXTVAL` and its relatives are *volatile*: they give a different answer every time they are called. The optimizer therefore never folds them into a constant (a query that calls `NEXTVAL` in its select list calls it once per row, with the optimizer on or off) and, since the number of calls could change the answers, never moves them out of the place the query put them.
