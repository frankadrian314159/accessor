# Saved queries, parameters, EXEC and triggers

## Saved queries (views)

```sql
CREATE VIEW ExpensiveProducts AS
    SELECT * FROM Product WHERE Price > 100;

CREATE QUERY OrdersOf (@customer INTEGER) AS
    SELECT * FROM Orders WHERE CustID = @customer;

DROP VIEW ExpensiveProducts;
```

`VIEW` and `QUERY` are interchangeable. A saved query is stored as text and appears next to the tables. It can be used in the `FROM` clause of another query, like a table. Saved queries can refer to one another; a query that ends up referring to itself is stopped with an error.

A saved query may be a single `SELECT` (including `WITH`, `VALUES` and set operators), a [crosstab](select.md#crosstab-queries-transform--pivot) (`TRANSFORM … PIVOT`), or a call of another saved query:

```sql
CREATE QUERY CustomerThree AS EXEC OrdersOf 3;
```

A view cannot have a column-name list: a parenthesised list after the name declares *parameters* (see below). The column names come from the select list; use aliases to choose them.

## Parameters

A parameterised query declares its parameters, with optional types, either in a list after the name or in a `PARAMETERS` header:

```sql
CREATE QUERY Between (@lo DATETIME, @hi DATETIME) AS
    SELECT * FROM Orders WHERE Placed BETWEEN @lo AND @hi;

CREATE QUERY Between2 AS
    PARAMETERS @lo DATETIME, @hi DATETIME;
    SELECT * FROM Orders WHERE Placed BETWEEN @lo AND @hi;
```

Both forms are stored the second way. A declared type converts the argument. `@name` stands for the value wherever a literal could go. A query with parameters cannot be used in a `FROM` clause; run it with `EXEC`.

## EXEC

```sql
EXEC name;
EXEC name value, value;
EXEC name @lo = #2024-01-01#, @hi = #2024-12-31#;
EXEC name (value, value);
```

`EXECUTE` and `CALL` are synonyms. Arguments are matched by position, or by name with `@name = value` (names are not case-sensitive). Every parameter needs a value. `EXEC` returns the result of the saved query.

A `PARAMETERS` statement on its own, at the top of a script, does nothing; it exists so that a saved query's text runs as a script too.

## Triggers

A trigger runs statements automatically when a row is inserted, updated or deleted.

```sql
CREATE TRIGGER name
    {BEFORE | AFTER} {INSERT | UPDATE | DELETE} ON table
    [FOR EACH ROW]
    [WHEN condition]
BEGIN
    statement;
    ...
END;
```

The trigger fires once for each row. `WHEN` is tested first, and the body runs only when it is true. In the condition and the body, `NEW.column` is the row being written (insert and update) and `OLD.column` is the row as it was (update and delete).

The body may contain `INSERT`, `UPDATE`, `DELETE` and `RAISE` statements. `RAISE expression` stops the whole statement with the message the expression gives; everything the statement did, including earlier trigger effects, is undone.

```sql
CREATE TRIGGER stock_check BEFORE INSERT ON OrderLine
WHEN NEW.Qty > (SELECT Stock FROM Product WHERE ID = NEW.ProductID)
BEGIN
    RAISE 'Not enough stock';
END;

CREATE TRIGGER audit AFTER UPDATE ON Product
WHEN OLD.Price <> NEW.Price
BEGIN
    INSERT INTO PriceLog (ProductID, Old, New, At)
    VALUES (OLD.ID, OLD.Price, NEW.Price, Now());
END;
```

Rules:

* `BEFORE` triggers see the row before it is written; `AFTER` triggers see the result.
* Triggers also fire for changes made by foreign-key cascades and by other triggers, up to 16 levels deep; deeper is an error.
* A trigger cannot insert into its own table.
* The whole statement, with every trigger it caused, succeeds or fails as one.
* `DROP TRIGGER name` removes a trigger. Dropping a table drops its triggers.
* `CHECK` constraints are implemented as triggers; see [ddl.md](ddl.md#check-constraints).
