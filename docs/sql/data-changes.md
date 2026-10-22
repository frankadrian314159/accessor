# INSERT, UPDATE, DELETE, MERGE and TRUNCATE

Every data statement is atomic: if it fails part-way, none of its changes remain. This holds with or without an explicit [transaction](transactions.md). Constraints (types, `NOT NULL`, keys, unique indexes, `CHECK`) are applied as rows are written; [foreign keys](ddl.md#foreign-keys) are checked once at the end of the statement against its final state, so rows may be inserted in any order within one statement. [Triggers](triggers.md) fire for each row.

A data statement returns the number of rows it changed.

## INSERT

```sql
INSERT INTO table [(column, ...)] VALUES (expr, ...) [, (expr, ...)] ...;
INSERT INTO table [(column, ...)] query;
INSERT INTO table DEFAULT VALUES;
```

With a column list, the other columns take their defaults (or NULL, or the next AutoNumber). Without one, the values are for all the columns you can set, in table order: calculated columns are skipped. The number of values must match.

```sql
INSERT INTO Product (Name, Price) VALUES ('Nut', 0.10), ('Bolt', 0.25);
INSERT INTO Archive SELECT * FROM Orders WHERE Placed < #2020-01-01#;
INSERT INTO Log DEFAULT VALUES;           -- every column takes its default
```

Values are converted to the column's type (`'12'` into an integer column becomes `12`). A value that cannot be converted fails the statement.

## UPDATE

```sql
UPDATE table [[AS] alias]
SET column = expr [, column = expr ...]
[WHERE condition];
```

All the right-hand sides see the row as it was before the update, so `SET a = b, b = a` swaps. A calculated column cannot be set. Sub-queries may be [correlated](select.md#correlated-sub-queries) and may read the table being updated (they see its state before the statement).

```sql
UPDATE Product P SET Price = Price * 1.05 WHERE Price < 10;
UPDATE Emp SET Rank = (SELECT COUNT(*) FROM Emp E2 WHERE E2.Sal > Emp.Sal) + 1;
```

## DELETE

```sql
DELETE FROM table [[AS] alias] [WHERE condition];
```

`DELETE * FROM t` is accepted as in Access. Foreign keys with `ON DELETE CASCADE` or `SET NULL` act as part of the statement; with `RESTRICT` (the default) a delete that would orphan a record is refused.

```sql
DELETE FROM Orders O
WHERE NOT EXISTS (SELECT 1 FROM Lines L WHERE L.OrderID = O.ID);
```

## TRUNCATE

```sql
TRUNCATE [TABLE] table;
```

Deletes every record. It is the same as `DELETE FROM table`: foreign keys and triggers apply, and the AutoNumber counter is not reset.

## MERGE

```sql
MERGE INTO target [[AS] alias]
USING source [[AS] alias]
ON condition
WHEN MATCHED [AND condition] THEN UPDATE SET column = expr [, ...]
WHEN MATCHED [AND condition] THEN DELETE
WHEN NOT MATCHED [AND condition] THEN INSERT [(columns)] VALUES (expr, ...);
```

The source is a table, a saved query, or a parenthesised query with an alias (often `VALUES`). For each source row in turn:

1. The `ON` condition, with that row's values filled in, finds the matching target rows.
2. If there are matches, the first `WHEN MATCHED` clause whose extra `AND` condition holds is applied to each matching target row.
3. If there are none, the first `WHEN NOT MATCHED` clause whose condition holds inserts a row.

```sql
MERGE INTO Stock S
USING (VALUES (1, 5), (2, 0), (9, 7)) AS D (ID, Qty) ON S.ID = D.ID
WHEN MATCHED AND D.Qty = 0 THEN DELETE
WHEN MATCHED THEN UPDATE SET Qty = S.Qty + D.Qty
WHEN NOT MATCHED THEN INSERT (ID, Qty) VALUES (D.ID, D.Qty);
```

Expressions can use the source's columns (qualified by its alias, or unqualified when the target has no column of that name) and the target's. A target row matched by more than one source row is an error. The whole statement is atomic; an error leaves the target as it was. The statement returns the number of rows inserted, updated and deleted together.
