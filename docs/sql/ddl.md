# Defining tables: CREATE, ALTER and DROP

## CREATE TABLE

```sql
CREATE TABLE [IF NOT EXISTS] name (
    column-definition | table-constraint [, ...]
);
```

### Column definitions

```sql
CREATE TABLE Product (
    ID      COUNTER PRIMARY KEY,
    Name    TEXT NOT NULL UNIQUE,
    Price   CURRENCY NOT NULL DEFAULT 0 CHECK (Price >= 0),
    Stock   INTEGER DEFAULT 0,
    Added   DATETIME,
    Active  YESNO DEFAULT TRUE,
    Cat     INTEGER REFERENCES Category (ID) ON DELETE SET NULL,
    Value   CALCULATED AS (Price * Stock)
);
```

A column definition is a name, a [data type](#data-types), and any of these in any order:

| Constraint | Meaning |
| --- | --- |
| `NOT NULL` / `NULL` | the column is required / may be empty (the default) |
| `PRIMARY KEY` | part of the primary key; implies `NOT NULL` and a unique index |
| `UNIQUE` | no two records may have the same value (NULLs are not compared) |
| `DEFAULT literal` | the value used when an `INSERT` does not give one |
| `CHECK (condition)` | see [CHECK constraints](#check-constraints) |
| `REFERENCES t [(c)]` | a [foreign key](#foreign-keys) to another table |
| `AUTOINCREMENT`, `IDENTITY`, `GENERATED {ALWAYS \| BY DEFAULT} AS IDENTITY` | the column is an AutoNumber |
| `GENERATED ALWAYS AS (expr) [STORED \| VIRTUAL]` | the column is [calculated](#calculated-columns) |
| `CONSTRAINT name` | in front of any of the above; the name is kept only for `CHECK` |

`DEFAULT` takes a literal (a number, text, date or `TRUE`/`FALSE`/`NULL`), not an expression.

### Table constraints

```sql
PRIMARY KEY (a, b)
UNIQUE (a, b)
[CONSTRAINT name] CHECK (condition)
[CONSTRAINT name] FOREIGN KEY (a, b) REFERENCES other (x, y) [ON DELETE action] [ON UPDATE action]
```

A table has at most one primary key. Constraints on several columns use the table form.

### Data types

Each type has a canonical name and aliases; the aliases are what the standard and other systems call the same thing.

| Type | Aliases | Holds |
| --- | --- | --- |
| `TEXT` | `VARCHAR`, `CHAR`, `CHARACTER [VARYING]`, `NCHAR`, `NVARCHAR`, `NATIONAL CHARACTER`, `STRING`, `MEMO`, `LONGTEXT`, `CLOB` | text of any length |
| `INTEGER` | `INT`, `SMALLINT`, `TINYINT`, `BIGINT`, `LONG`, `SHORT`, `BYTE` | 64-bit whole numbers |
| `BIGNUM` | | whole numbers of any size |
| `FLOAT` | `REAL`, `SINGLE` | 32-bit floating point |
| `DOUBLE` | `DOUBLE PRECISION` | 64-bit floating point |
| `DECIMAL` | `NUMERIC`, `DEC`, `NUMBER` | exact decimals of any size |
| `CURRENCY` | `MONEY` | exact decimals, shown as money |
| `RATIONAL` | | exact fractions |
| `YESNO` | `BOOLEAN`, `BOOL`, `BIT`, `LOGICAL` | true or false |
| `DATETIME` | `DATE`, `TIME`, `TIMESTAMP` | a date and time of day, to the second, with no time zone |
| `TIMESTAMPTZ` | `TIMESTAMP WITH TIME ZONE` | a moment in time, shown in a time zone ([types/timestamptz.md](../types/timestamptz.md)) |
| `INTERVAL` | | a length of time in months, days and seconds ([types/interval.md](../types/interval.md)) |
| `JSON` | `JSONB` | a JSON document ([types/json.md](../types/json.md)) |
| `COUNTER` | `AUTOINCREMENT`, `AUTONUMBER`, `SERIAL` | an `INTEGER` filled in automatically |
| `CALCULATED` | | a value computed from the other columns |

A length or precision after a type name (`VARCHAR(50)`, `NUMERIC(10, 2)`) is accepted and ignored (unless `SET strict_types = on` turns it into a `CHECK` when the table is made, [session.md](session.md)): text has no length limit, and decimals have no fixed scale. Use `CAST(x AS DECIMAL(10,2))` or `ROUND` to round a value. `TIMESTAMP WITH TIME ZONE` makes a `TIMESTAMPTZ`; `TIMESTAMP WITHOUT TIME ZONE` is the plain `DATETIME`, with or without a precision before the zone (`timestamp(2) without time zone`, `time(3) with time zone`, where a time of day with a zone is a `TIMESTAMPTZ` too). `BIT VARYING(n)` keeps its bits as text. `INTEGER`, `FLOAT` and the other numeric types behave as described in the README's Numbers section.

### AutoNumber

For a number that is not tied to one table, see [sequences](sequences.md): `DEFAULT NEXT VALUE FOR seq` fills a column from a sequence.


A `COUNTER` column (or any `INTEGER` column marked `IDENTITY` / `GENERATED ... AS IDENTITY`) gets the next number when an insert does not supply one: 1, 2, 3, and so on, never reused even after a delete. A supplied value is kept, and later numbers continue after it. A table has at most one AutoNumber column. `GENERATED ALWAYS` is accepted as a spelling of the same thing: values you supply are still accepted.

### Calculated columns

```sql
Total CALCULATED AS (Qty * Price)
Total NUMERIC GENERATED ALWAYS AS (Qty * Price) STORED
```

The value is worked out from the other columns of the same record whenever the record is written. It cannot be inserted or updated. The formula may not use `Now()`, `Date()`, sub-queries, parameters or aggregates. `ALTER TABLE ... ALTER COLUMN c CALCULATED AS (...)` changes a formula, or turns an ordinary column into a calculated one.

### CHECK constraints

```sql
Age INTEGER CHECK (Age >= 0)
CONSTRAINT adult CHECK (Age >= 18 OR Age IS NULL)
```

A record is refused when the condition is false. When it is unknown (NULL) the record is accepted, as in SQL. The condition may use the record's own columns, constants, operators and functions, but not sub-queries, parameters or window functions. Checks are enforced for every write: statements, cascades, merges and the datasheet.

A check is stored as a pair of `BEFORE INSERT` and `BEFORE UPDATE` triggers named `name_ins` and `name_upd`, where `name` is the `CONSTRAINT` name, or `ck_table_1`, `ck_table_2`, ... when none was given. They appear in the list of triggers and in the shell's `.schema` output, and `DROP CONSTRAINT name` removes the pair. A table's checks go when the table is dropped.

### Foreign keys

```sql
CONSTRAINT FK_Orders_Customer FOREIGN KEY (CustID) REFERENCES Customers (ID)
    ON DELETE CASCADE ON UPDATE RESTRICT
```

The referenced columns must be the primary key of the other table or be covered by a unique index; if they are left out, the primary key is used. The actions are:

| Action | When the referenced record is deleted or its key changes |
| --- | --- |
| `RESTRICT` (default) | the change is refused while records refer to it |
| `CASCADE` | the referring records are deleted, or their key follows the change |
| `SET NULL` | the referring columns become NULL |
| `SET DEFAULT` | the referring columns take their `DEFAULT` values (NULL if they have none); the result must itself refer to an existing record, or the change is refused |
| `NO ACTION` | like `RESTRICT`, but a [deferrable](#deferrable-relationships) relationship may wait until the end of the transaction |

A foreign key column that is NULL refers to nothing and is always allowed. An index on the referencing columns is created automatically. Foreign keys are checked at the end of each statement, unless they are deferred.

### Deferrable relationships

```sql
CONSTRAINT FK_Child FOREIGN KEY (ParentID) REFERENCES Parent (ID)
    DEFERRABLE INITIALLY DEFERRED
```

| Clause | Meaning |
| --- | --- |
| `NOT DEFERRABLE` (default) | always checked at the end of the statement |
| `DEFERRABLE` or `DEFERRABLE INITIALLY IMMEDIATE` | checked at the end of the statement, but a transaction may defer it with `SET CONSTRAINTS` |
| `DEFERRABLE INITIALLY DEFERRED` (or just `INITIALLY DEFERRED`) | inside a transaction, checked when it is committed |

Inside a transaction, a deferred relationship may be broken for a while, so rows can be inserted in any order, or two rows can swap keys; when the transaction commits, every deferred relationship is verified, and if one is still broken the whole transaction is rolled back and `COMMIT` fails. [`SET CONSTRAINTS`](transactions.md#set-constraints) changes the timing for the rest of a transaction. Outside a transaction every statement is its own, so nothing is ever deferred.

A deferrable relationship that gives no action behaves as `NO ACTION`; one without a `DEFERRABLE` clause behaves as `RESTRICT`. `RESTRICT` is always checked at once, and so are the actions that change rows (`CASCADE`, `SET NULL`, `SET DEFAULT`): only the check that a row has its parent, and the refusal of `NO ACTION`, wait. Only foreign keys can be deferred; `UNIQUE`, `PRIMARY KEY` and `CHECK` are always immediate.

The other options of a constraint that PostgreSQL accepts are read and change nothing here: `NO INHERIT`, `[NOT] ENFORCED` and `NOT VALID` on `CHECK` and `REFERENCES`, `MATCH FULL | SIMPLE | PARTIAL` (every foreign key is `MATCH SIMPLE`), `NULLS [NOT] DISTINCT` and `INCLUDE (columns)` on `UNIQUE` and `PRIMARY KEY`, `DEFERRABLE` and `INITIALLY ...` on those two (they stay immediate), and `COMPRESSION method` on a column. A default that is more than a literal (`DEFAULT 100 + (200 - 199) * 2`, `DEFAULT 'x'::text`) is read and not applied, as a default that calls a function is. `CREATE TABLE t (a, b) AS query` names the columns of the new table. `expr ISNULL` and `expr NOTNULL` are `IS NULL` and `IS NOT NULL`. `PARTITION BY` takes `COLLATE` and an operator class after a column; a key that is an expression (`lower(a)`) is still refused.

### CREATE TABLE AS

```sql
CREATE [TEMP | UNLOGGED] TABLE name AS query [WITH [NO] DATA];
```

Makes a table from the result of a query. The columns take the query's column names and, as their type, the type of the first non-null value in each (`TEXT` if there is none). Nothing but the columns and the rows is copied: no keys, indexes or defaults. `WITH NO DATA` makes the empty table. `TEMP`, `TEMPORARY` and `UNLOGGED` are accepted and make an ordinary table. `INSERT ... VALUES (1, DEFAULT)` uses a column's default for that item, and a table may have no columns of its own (`CREATE TABLE child () INHERITS (parent)`).

## CREATE INDEX

```sql
CREATE [UNIQUE] INDEX [IF NOT EXISTS] name ON table (column [ASC | DESC] [, ...]);
DROP INDEX [IF EXISTS] name ON table;
```

`DROP INDEX name` may leave out `ON table` when only one table has an index of that name (index names belong to a table; if several tables share the name the statement says so and asks for the table). `REINDEX` builds all indexes again from the rows, `REINDEX table` those of one table and `REINDEX index` the indexes of the table that index is on; it is never needed in normal use (the indexes are kept up to date by every change, and rebuilt when a file is opened) but is a harmless repair, and it refuses a unique index that finds duplicates.

Indexes speed up equality and range conditions and joins on their columns, and the optimiser uses them without being told. A unique index also enforces uniqueness. Index names are per table.

**Ascending and descending columns.** Each column of an index runs from low to high (`ASC`, the default) or from high to low (`DESC`). It makes no difference to lookups, ranges, joins or uniqueness. It matters to `ORDER BY ... LIMIT`: an index serves a sort when its columns are the sort columns, in order, and the directions are the index's own or all the opposite (the index is read backwards). So an index on `(Region, Total DESC)` serves `ORDER BY Region, Total DESC` and `ORDER BY Region DESC, Total`, and stops reading after the rows asked for, whereas an index on `(Region, Total)` can serve only `ORDER BY Region, Total` and `ORDER BY Region DESC, Total DESC`; the mixed order needs a sort. See [access paths](../optimizer/access-paths.md#order-by-with-a-limit). NULLs sort as low values: first in an ascending column and last in a descending one, which is where `ORDER BY` puts them too. The directions are shown by `.indexes` in the shell and by `INFORMATION_SCHEMA.INDEXES` (`is_descending`), and are kept in the file (format version 6).

## ALTER TABLE

```sql
ALTER TABLE t ADD [COLUMN] column-definition;
ALTER TABLE t ADD [CONSTRAINT name] FOREIGN KEY (...) REFERENCES ...;
ALTER TABLE t ADD [CONSTRAINT name] CHECK (condition);
ALTER TABLE t DROP [COLUMN] column;
ALTER TABLE t DROP CONSTRAINT name;
ALTER TABLE t RENAME [COLUMN] old TO new;
ALTER TABLE t ALTER [COLUMN] column [TYPE] data-type;
ALTER TABLE t ALTER [COLUMN] column CALCULATED AS (expr);
ALTER TABLE t ALTER [COLUMN] column SET DEFAULT literal;
ALTER TABLE t ALTER [COLUMN] column DROP DEFAULT;
ALTER TABLE t ALTER [COLUMN] column SET NOT NULL;
ALTER TABLE t ALTER [COLUMN] column DROP NOT NULL;
ALTER TABLE t RENAME TO new_name;      -- MySQL: RENAME TABLE a TO b, c TO d
```

* **ADD COLUMN** fills the new column with its default (or NULL) in existing records.
* **ADD FOREIGN KEY** and **ADD CHECK** first test the existing records and are refused, naming how many break the rule, if any do.
* **ALTER COLUMN ... TYPE** converts every stored value; if one cannot be converted the whole change is refused and nothing changes. A column cannot be converted to an AutoNumber.
* **DROP COLUMN**, **RENAME COLUMN** and **ALTER COLUMN** are refused when a foreign key, index, trigger or calculated column depends on the column, with a message that says which.
* **DROP CONSTRAINT** removes a foreign key or a check by name.
* **SET DEFAULT** / **DROP DEFAULT** change the value used when an insert leaves the column out. The literal is converted to the column's type at once, so a value that does not fit is refused. Existing records are not touched. Calculated and AutoNumber columns have no default.
* **SET NOT NULL** makes the column required. It is refused, with the number of offending records, while any record has no value for it. **DROP NOT NULL** makes it optional (but not a primary key column).
* **RENAME TO** renames the table. Relationships that refer to it, its triggers and checks, and saved queries that name it are rewritten to follow. In a saved query or trigger the name is recognised after `FROM`, `JOIN`, `INTO`, `UPDATE`, `ON` and `TABLE`, and as a prefix (`Old.Column`), so a column that happens to have the old table's name is left alone. The new name must not be a table or a query already.

Schema changes take part in [transactions](transactions.md) and can be rolled back.

## DROP TABLE

```sql
DROP TABLE [IF EXISTS] name [, name ...] [CASCADE | RESTRICT];
```

Removes the table with its data, indexes and triggers. It is refused while another table has a foreign key that refers to it, or while tables [inherit from it](inheritance-and-partitioning.md). `CASCADE` drops those along with it: the tables that inherit from it, the queries (views) that mention it, and the foreign keys that point at it. Several names are dropped in order; with `IF EXISTS` the ones that do not exist are skipped.

`CREATE INDEX` may leave out the name (`CREATE INDEX ON t (a)` is named `t_a_idx`), and reads `CONCURRENTLY`, `USING method`, operator classes, `COLLATE`, `NULLS FIRST/LAST`, `INCLUDE (...)`, `WITH (...)` and `TABLESPACE`, ignoring them; an index on an expression and a partial index (`WHERE`) are refused. `ALTER TABLE` takes several actions separated by commas (all or none), `ADD COLUMN` may carry a `CHECK`, which is added to the table with the column, and `DROP COLUMN` drops the `CHECK` constraints that mention the column, `IF EXISTS`, `ONLY`, and `ATTACH PARTITION` / `DETACH PARTITION`. `CREATE TABLE t (LIKE other)` copies the columns, with their `NOT NULL` and defaults but nothing else.

`CREATE TABLE` also takes `INHERITS (parent)`, `PARTITION BY ...` and `PARTITION OF parent FOR VALUES ...` ([inheritance-and-partitioning.md](inheritance-and-partitioning.md)), a schema prefix on the name ([schemas-and-types.md](schemas-and-types.md)), and the storage clauses `WITH (...)`, `TABLESPACE t`, `USING method` and `ON COMMIT ...`, which are read and ignored.

## CREATE OR REPLACE

```sql
CREATE OR REPLACE VIEW name AS query;
CREATE OR REPLACE QUERY name (@p) AS query;
CREATE OR REPLACE TRIGGER name ... ;
```

Replaces the saved query or trigger of that name, or creates it if there is none. It is atomic: if the new definition is refused (it names a missing column, say), the old one is still there. Tables cannot be replaced, and a name that belongs to a table cannot be used for a query.

## IF EXISTS and IF NOT EXISTS

`CREATE TABLE | INDEX | VIEW | QUERY IF NOT EXISTS` does nothing when the object already exists, and `DROP TABLE | VIEW | QUERY | TRIGGER | INDEX IF EXISTS` does nothing when it does not. Neither checks that an existing object has the same definition.
