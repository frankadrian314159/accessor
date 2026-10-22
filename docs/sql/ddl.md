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
| `DATETIME` | `DATE`, `TIME`, `TIMESTAMP` | a date and time of day, to the second |
| `COUNTER` | `AUTOINCREMENT`, `AUTONUMBER`, `SERIAL` | an `INTEGER` filled in automatically |
| `CALCULATED` | | a value computed from the other columns |

A length or precision after a type name (`VARCHAR(50)`, `NUMERIC(10, 2)`) is accepted and ignored: text has no length limit, and decimals have no fixed scale. Use `CAST(x AS DECIMAL(10,2))` or `ROUND` to round a value. `TIMESTAMP WITH TIME ZONE` is accepted, and the zone is ignored: all times are stored as UTC-style seconds without a zone. `INTEGER`, `FLOAT` and the other numeric types behave as described in the README's Numbers section.

### AutoNumber

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

A foreign key column that is NULL refers to nothing and is always allowed. An index on the referencing columns is created automatically. Foreign keys are checked at the end of each statement.

## CREATE INDEX

```sql
CREATE [UNIQUE] INDEX [IF NOT EXISTS] name ON table (column [, ...]);
DROP INDEX [IF EXISTS] name ON table;
```

Indexes speed up equality and range conditions and joins on their columns, and the optimiser uses them without being told. A unique index also enforces uniqueness. Index names are per table.

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
```

* **ADD COLUMN** fills the new column with its default (or NULL) in existing records.
* **ADD FOREIGN KEY** and **ADD CHECK** first test the existing records and are refused, naming how many break the rule, if any do.
* **ALTER COLUMN ... TYPE** converts every stored value; if one cannot be converted the whole change is refused and nothing changes. A column cannot be converted to an AutoNumber.
* **DROP COLUMN**, **RENAME COLUMN** and **ALTER COLUMN** are refused when a foreign key, index, trigger or calculated column depends on the column, with a message that says which.
* **DROP CONSTRAINT** removes a foreign key or a check by name.

Schema changes take part in [transactions](transactions.md) and can be rolled back.

## DROP TABLE

```sql
DROP TABLE [IF EXISTS] name;
```

Removes the table with its data, indexes and triggers. It is refused while another table has a foreign key that refers to it.

## IF EXISTS and IF NOT EXISTS

`CREATE TABLE | INDEX | VIEW | QUERY IF NOT EXISTS` does nothing when the object already exists, and `DROP TABLE | VIEW | QUERY | TRIGGER | INDEX IF EXISTS` does nothing when it does not. Neither checks that an existing object has the same definition.
