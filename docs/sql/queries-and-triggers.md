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

A query is checked when it is created: one that names a missing table or column is refused then, not when first used. (A query with parameters cannot be tried without values and is checked when it runs.)

### Column names

A parenthesised list of names after the view's name chooses the names of its columns:

```sql
CREATE VIEW Cheap (code, label, cost) AS SELECT id, name, price FROM Product WHERE price < 1;
SELECT label FROM Cheap;
```

The list must have as many names as the query has columns. (A list that starts with a `@parameter` declares parameters instead; see below.) `CREATE OR REPLACE VIEW` changes a view's definition ([ddl.md](ddl.md#create-or-replace)).

### Changing data through a view

`INSERT`, `UPDATE` and `DELETE` work on a **simple view**: one that reads a single table (or another simple view) and shows plain columns of it, with no `DISTINCT`, `GROUP BY`, aggregate, limit, window function, join, sub-query in `FROM`, set operator or parameters. The statement is carried out on the table, with the view's `WHERE` condition added, so only rows the view shows can be updated or deleted:

```sql
CREATE VIEW Live (code, label) AS SELECT id, name FROM Product WHERE active;
UPDATE Live SET label = 'renamed' WHERE code = 3;     -- only if product 3 is active
DELETE FROM Live WHERE code = 4;
INSERT INTO Live (code, label) VALUES (10, 'new');    -- columns the view does not show take their defaults
```

Columns are named as the view names them. A view column that is computed (`price * 2 AS twice`) cannot be set, and a view that is not simple refuses a change with the reason ("it joins tables", "it summarises, limits or combines rows", "a column is computed by an expression", ...).

### WITH CHECK OPTION

```sql
CREATE VIEW Pricey AS SELECT id, name, price FROM Product WHERE price > 5 WITH CHECK OPTION;
UPDATE Pricey SET price = 1 WHERE id = 4;    -- refused: the row would leave the view
```

With `WITH CHECK OPTION`, an `INSERT` or `UPDATE` through the view that would produce a row not satisfying the view's condition is refused and changes nothing. (The check is made when the view reads a table directly; for a view built on another view, that view's own option applies.)

### Reading a view

A view that shows every column of one table is *merged* into the query that uses it, so the table's indexes still work: see [../optimizer/views.md](../optimizer/views.md).

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

## Prepared statements

```sql
PREPARE name AS statement;
EXEC name [ ( [@p =] value [, ...] ) ];
DEALLOCATE [PREPARE] { name | ALL };
```

`PREPARE` remembers one statement, which may use `@parameters`, for the rest of the session. `EXEC name (v1, v2)` (or `EXECUTE`, or `CALL`) runs it, the values filling the parameters in the order of their first appearance in the statement; `EXEC name (@city = 'Rome')` names them. It is the same `EXEC` as for saved queries: a saved query of that name is used if there is one, and a prepared statement cannot take the name of a saved query. Prepared statements are not stored in the file. From Rust, `Database::prepare`, `execute_prepared` and `query_prepared` do the same without SQL text; see [the optimizer notes on prepared statements](../optimizer/prepared.md).

## Triggers

A trigger runs statements automatically when rows are inserted, updated or deleted.

```sql
CREATE [OR REPLACE] TRIGGER name
    {BEFORE | AFTER | INSTEAD OF} event [OR event ...] ON table_or_query
    [FOR EACH {ROW | STATEMENT}]
    [WHEN condition]
BEGIN
    statement;
    ...
END;

event ::= INSERT | DELETE | UPDATE [OF column [, ...]]
```

The trigger fires once for each row (the default), or with `FOR EACH STATEMENT` once for the whole statement. `WHEN` is tested first, and the body runs only when it is true. In the condition and the body of a row trigger, `NEW.column` is the row being written (insert and update) and `OLD.column` is the row as it was (update and delete).

The body may contain `INSERT`, `UPDATE`, `DELETE`, `SELECT` and `RAISE` statements. `RAISE expression` stops the whole statement with the message the expression gives; everything the statement did, including earlier trigger effects, is undone. A `SELECT` is run for what it does, usually calling a function such as `NEXTVAL`; its rows are not used.

```sql
CREATE TRIGGER stock_check BEFORE INSERT ON OrderLine
WHEN NEW.Qty > (SELECT Stock FROM Product WHERE ID = NEW.ProductID)
BEGIN
    RAISE 'Not enough stock';
END;

CREATE TRIGGER audit AFTER UPDATE OF Price ON Product
BEGIN
    INSERT INTO PriceLog (ProductID, Old, New, At)
    VALUES (OLD.ID, OLD.Price, NEW.Price, Now());
END;
```

### Several events, and UPDATE OF

`AFTER INSERT OR UPDATE OR DELETE ON t` is one trigger for all three. `NEW` and `OLD` must be valid for every event the trigger names, so a trigger on `INSERT OR DELETE` can use neither; use separate triggers if they differ. Each event may be named once.

`UPDATE OF a, b` fires only for updates that *change* one of the columns listed (an `UPDATE` that sets a column to the value it already has does not count). It may be combined with other events: `AFTER INSERT OR UPDATE OF Price`.

### Statement triggers

```sql
CREATE TRIGGER touched AFTER INSERT OR UPDATE OR DELETE ON Product
FOR EACH STATEMENT
BEGIN
    UPDATE Meta SET LastChange = Now();
END;
```

A statement trigger runs once, however many rows the statement changed: `BEFORE` ones before the first row is written, `AFTER` ones after the last row and after the relationships have been checked. It has no `NEW` or `OLD`. It runs only when the statement changes at least one row (a statement that matches nothing fires nothing), and not for the rows changed by cascades or other triggers. When rows and statements both have triggers, the order is: statement `BEFORE`, then for each row `BEFORE`, the change, `AFTER`, then statement `AFTER`.

### INSTEAD OF triggers on queries

An `INSTEAD OF` trigger replaces an `INSERT`, `UPDATE` or `DELETE` on a *saved query* (a view). It makes any query changeable: one that joins tables, groups, or computes columns.

```sql
CREATE VIEW Directory AS
    SELECT P.ID, P.Name, F.Number FROM Person P JOIN Phone F ON F.PersonID = P.ID;

CREATE TRIGGER dir_insert INSTEAD OF INSERT ON Directory
BEGIN
    INSERT INTO Person VALUES (NEW.ID, NEW.Name);
    INSERT INTO Phone VALUES (NEW.ID, NEW.Number);
END;

CREATE TRIGGER dir_delete INSTEAD OF DELETE ON Directory
BEGIN
    DELETE FROM Person WHERE ID = OLD.ID;
END;

INSERT INTO Directory VALUES (1, 'Ada', '555-1');
DELETE FROM Directory WHERE Name = 'Ada';
```

The trigger runs once for each row the statement would have affected, with `OLD` and `NEW` holding the query's columns. For an `INSERT`, `NEW` is the row given (columns not named are NULL); for a `DELETE`, `OLD` is each row the `WHERE` selects; for an `UPDATE`, `OLD` is each row selected and `NEW` is that row with the `SET` assignments applied. The statement reports the number of rows it ran for. If the query has an `INSTEAD OF` trigger for an event, it is used even when the query could have been changed directly. An `INSTEAD OF` trigger is only for queries (not tables), cannot be `FOR EACH STATEMENT`, and goes when its query is dropped. Everything a statement does through such a trigger is atomic.

### Rules

* `BEFORE` triggers see the row before it is written; `AFTER` triggers see the result.
* Triggers fire in the alphabetical order of their names.
* Triggers also fire for changes made by foreign-key cascades and by other triggers, up to 16 levels deep; deeper is an error.
* A trigger cannot insert into its own table.
* The whole statement, with every trigger it caused, succeeds or fails as one.
* `DROP TRIGGER name` removes a trigger. Dropping a table drops its triggers; dropping a query drops its `INSTEAD OF` triggers.
* `CHECK` constraints are implemented as triggers; see [ddl.md](ddl.md#check-constraints).

### The optimizer and triggers

Triggers are not part of query planning, but the work they do is: the statements in a trigger body are planned like any others, so conditions in a trigger's `UPDATE ... WHERE id = NEW.id` use the table's indexes (index the columns your triggers look rows up by). A row trigger runs its body once per row, so a trigger that scans a large table makes a large statement quadratic; prefer a `FOR EACH STATEMENT` trigger when one pass will do.
