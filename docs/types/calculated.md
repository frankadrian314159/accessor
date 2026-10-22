# CALCULATED

A `CALCULATED` column holds an expression instead of data. Its value is worked out from the other columns of the same record, and is brought up to date whenever the record is written. Only the expression is the definition; the type of the value is whatever the expression produces.

```sql
CREATE TABLE OrderLine (
    ID         COUNTER PRIMARY KEY,
    Qty        INTEGER NOT NULL,
    UnitPrice  CURRENCY NOT NULL,
    Discount   DECIMAL DEFAULT 0,
    LineTotal  CALCULATED AS (Qty * UnitPrice * (1 - Discount))
);
-- the standard spelling:
--   LineTotal CURRENCY GENERATED ALWAYS AS (Qty * UnitPrice * (1 - Discount)) STORED
```

`LineTotal` is `CURRENCY` here, because currency times integer times decimal is currency.

## Using it

A calculated column can be selected, filtered on, sorted by, grouped by, indexed, and used in other calculated columns' formulas if they come later in the table. It **cannot be inserted or updated**: `INSERT` without a column list skips it, and naming it is an error ("the field 'LineTotal' is calculated; its value comes from its expression and cannot be set"). In the datasheet its cells are read-only.

## What the formula may contain

* the table's other columns, by name;
* constants, operators, `CASE`, `IIf`, `CAST` and the built-in functions that always give the same answer for the same input.

It may **not** contain:

* `Now()`, `Date()` and `CURRENT_...`, which would be out of date as soon as the record was stored;
* sub-queries, parameters or aggregate functions;
* its own column.

A formula that breaks one of these rules, names a missing column, or does not parse, is refused when it is defined, with a message saying which. A common slip is `&&` for joining text; the message points out that the operator is a single `&`.

## When it is calculated

The result is recomputed each time a record is inserted or updated, and the latest result is kept with the record so that reading it costs the same as reading any column, and so that it can be indexed. Changing the formula recomputes every record at once, and the change can be rolled back inside a [transaction](../sql/transactions.md).

## Defining and changing

```sql
ALTER TABLE OrderLine ADD COLUMN Net CALCULATED AS (LineTotal * 0.8);
ALTER TABLE OrderLine ALTER COLUMN Net CALCULATED AS (LineTotal * 0.75);   -- new formula
ALTER TABLE OrderLine ALTER COLUMN Qty CALCULATED AS (1);    -- an ordinary column becomes calculated; its data is replaced
ALTER TABLE OrderLine ALTER COLUMN Net TYPE DECIMAL;         -- back to an ordinary column, keeping the values
```

A calculated column cannot be `NOT NULL`, a primary key or an AutoNumber. A column that a calculated column's formula uses cannot be dropped or renamed until the calculated column is changed. In the desktop app's design view, choose `CALCULATED` as the type and type the formula; the **Calculate…** button shows the result for your data before you commit to it.
