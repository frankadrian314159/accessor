# Helping the optimizer, and its limits

The optimizer needs no help to be correct, and usually none to be fast. When a query is slower than it should be, the cause is almost always one of the things below. The first step is always the same: run `EXPLAIN` and read what it did ([explain.md](explain.md)).

## 1. Add the index the plan is missing

Look for these in the plan:

| In the plan | Meaning | Remedy |
| --- | --- | --- |
| `scan T [100000 rows]: full scan` with conditions pushed down | a selective condition on `T` has no index | `CREATE INDEX ix ON T (column)` on the filtered column |
| `join T: hash join ...` on a large table | the join columns have no index | index the join column of `T` (foreign keys get one automatically) |
| `sort: all N rows` or `sort: top-k of N rows` with a big `N` | `ORDER BY` has no matching index | `CREATE INDEX ix ON T (sort columns)` and use a limit |
| `est 5 rows, actual 40000` | the estimate was a fixed fraction because the column has no index | an index makes the estimate exact |

An index costs memory and a little time on every write, and helps only conditions on its **leading** columns. Put the equality columns first, then the range or sort column: `(CustID, Placed)` serves `WHERE CustID = 7 AND Placed > ...` and `WHERE CustID = 7 ORDER BY Placed`; `(Placed, CustID)` serves neither as well.

## 2. Write conditions an index can use

An index is chosen from conditions that compare a bare column with a constant. These cannot use one:

```sql
WHERE Year(Placed) = 2024                    -- function of the column
WHERE Total + 1 = 100                        -- arithmetic on the column
WHERE Name LIKE '*son'                       -- not a prefix
WHERE Dept = 'Eng' OR Dept = 'Ops'           -- OR
```

and these can:

```sql
WHERE Placed >= #2024-01-01# AND Placed < #2025-01-01#
WHERE Total = 99
WHERE Name = 'Ada'
```

Rewrite a function of a column as a range on the column when you can. `IN (a, b)` and `OR` of equalities are applied to rows as they are read but do not narrow the read.

## 3. Put join conditions in the join

Equalities between the tables are what let the optimizer order the joins and use indexes. A join with no equality (`ON A.x < B.y`) is a nested loop, and tables with no condition linking them are a cross product, which the optimizer leaves to the end but cannot avoid. Check that every table is connected:

```sql
-- B is not connected to A or C: a cross product
SELECT ... FROM A JOIN C ON C.a = A.id, B WHERE ...
```

## 4. Know which joins are reordered

Only inner and cross joins are. In a query with a `LEFT JOIN`, the tables are joined as written, so put the most selective table first and the left join after. If a `LEFT JOIN` is not really needed (the `WHERE` already requires a match on the right table), write it as an inner join and it becomes reorderable:

```sql
-- This LEFT JOIN is really an inner join: B.w = 3 fails for padded rows
SELECT ... FROM A LEFT JOIN B ON B.a_id = A.id WHERE B.w = 3;
-- so say so:
SELECT ... FROM A JOIN B ON B.a_id = A.id WHERE B.w = 3;
```

## 5. Use LIMIT with ORDER BY

An ordered read stops early only when there is a limit. `ORDER BY Total DESC` alone sorts everything; `ORDER BY Total DESC LIMIT 20` with an index on `Total` reads twenty rows.

## 6. Crosstabs: list the columns when you know them

A `TRANSFORM … PIVOT` query is as wide as the number of distinct pivot values. Pivot on a coarser expression (`PIVOT Year(d)`, not `PIVOT d`), or give an `IN (…)` list ([crosstab.md](crosstab.md)).

## 7. Page with a bookmark, not with OFFSET

`OFFSET 100000` reads and discards 100 000 rows. Remember the last row of a page and ask for the rows after it with a row comparison, `WHERE (Day, ID) > (@lastDay, @lastID) ORDER BY Day, ID LIMIT 50`, with an index on `(Day, ID)` ([access-paths.md](access-paths.md#row-values-and-keyset-paging)).

## 8. Views

A view of the form `SELECT * FROM table WHERE ...` costs nothing: it is merged into the query ([views.md](views.md)). A view that selects only some columns, joins or summarises is computed first, so put a condition you care about inside the view, or select from the table.

## 9. JSON columns

A condition on something *inside* a document, such as `WHERE JSON_VALUE(Body, '$.status') = 'open'`, reads and parses the document of every row it examines: an index on `Body` cannot help, because it holds whole documents. If you search a document member often, make it a column of its own: an ordinary column that you fill when you store the document, or a [calculated column](../types/calculated.md) (`Status CALCULATED AS (JSON_VALUE(Body, '$.status'))`) with an index on it. Conditions on that column use the index in the usual way.

## 10. Sequences and volatile functions

`NEXTVAL`, `Now()` and the other functions whose answer changes from call to call are never folded into a constant, so a condition that contains one is not used to pick an index. Take the value first (`SELECT NEXTVAL('s')` in its own statement, or a parameter) when you want to look it up by index.

## 11. Switch it off to check

If a result looks wrong, or you want to compare timings, turn the optimizer off:

```
accessor> .optimizer off
```

or `db.set_optimizer(false)` in Rust. Joins then run in the written order, with conditions applied after the joins, and no index-ordered reads or count shortcuts. The answers are identical by design, so if they differ, that is a bug; please report it with the query. The setting is per `Database`, not stored in the file, and applies until changed.

## Limits of the optimizer

* **Outer joins are not reordered** and conditions are not pushed to their nullable side.
* **More than 63 tables** in one `FROM` are joined as written.
* **Sub-queries, views and CTEs are planned separately.** A condition on a derived table is not pushed into it, and a derived table is computed in full before it is joined. A condition like `WHERE d.x = 5` over `(SELECT ... ) AS d` filters after `d` is built; put the condition inside the derived table when you can.
* **The estimates are simple** ([estimates.md](estimates.md)): good for indexed conditions, crude otherwise. There is no histogram, no correlation between columns, and no `ANALYZE`.
* **The join order search is greedy**: it picks the best next table at each step and does not try every order, so on a query with many tables it can miss the best order, though it rarely picks a bad one.
* **No plan cache.** Each run plans again. That is cheap compared with execution, and means a plan always reflects current data; a saved query is re-planned every time it runs.
* **Ordered reads need one table**, a limit, plain sort columns in one direction, and a matching index.
* **Correlated sub-queries** run once per distinct outer value, with no attempt to turn them into joins. Where a join would do, writing it as a join (or a derived table with `GROUP BY`) lets the optimizer help.
