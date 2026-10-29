# Pushdown

Joining is the expensive part of most queries, and its cost depends on how many rows go in. *Pushdown* applies each condition as early as possible, so that the join sees only the rows that can matter.

## Into derived tables

A condition of the outer `WHERE` that compares columns of a derived table (a sub-query in `FROM`) with constants is also applied inside it, so the inner query builds only the rows that can survive:

```sql
SELECT x, count(*) FROM (SELECT p.id AS x, q.id AS y FROM big p, big q) s WHERE x = 5 GROUP BY x;
-- the inner cross join is run for p.id = 5 only; EXPLAIN says "conditions on s moved into its query"
```

It is done only when that cannot change the answer: the inner query has no `DISTINCT`, `GROUP BY`, `HAVING`, `LIMIT`, `OFFSET`, `TOP`, set operator, `WITH` or window, and the columns concerned are computed without aggregates or sub-queries; the condition is a comparison, `BETWEEN` or `IN` (not `IS NULL`, `OR` or a function) between such columns and constants. The outer condition stays where it is.

## Conditions implied by an OR

`ON (a.x = 1 AND b.y = 2) OR (a.x = 3 AND b.z = 4)` cannot be split into per-table conditions, but it says that `a` must have `x = 1` or `x = 3`, and `b` must have `y = 2` or `z = 4`. Those are used to filter each table before the join (and can use an index); the whole `OR` is still checked on the pairs that remain. A table that some branch of the `OR` says nothing about is not filtered.

## Conditions every branch of an OR shares

`WHERE (a.k = b.k AND a.x = 1) OR (a.k = b.k AND a.x = 2)` repeats the join condition `a.k = b.k` in each branch. The optimizer moves what every branch has in common out of the `OR`: `a.k = b.k AND (a.x = 1 OR a.x = 2)`. The two are equal, including when a value is NULL, and the shared condition is now a condition of the query itself, so it can be used to join `a` and `b` and to filter them. Without this, a query such as TPC-DS 13, which writes its join conditions inside every branch of an `OR`, saw no condition linking its tables and built every combination of their rows. It is done for the `WHERE` condition, at the top level and inside `AND`.

## LATERAL

The rows a `LATERAL` item produces for one value of the outer columns are remembered, and when the join condition has equalities between the outer columns and the item's columns, they are looked up through a hash instead of being compared one by one.

## The idea

```sql
SELECT *
FROM Lines L JOIN Orders O ON O.ID = L.OrderID
WHERE L.Qty > 4 AND O.Total > 100;
```

Read literally, this joins every line to its order and then keeps the pairs where both conditions hold. But `L.Qty > 4` can be decided looking at a line alone, and `O.Total > 100` at an order alone. The optimizer applies each while reading its own table, and joins what is left.

## How conditions are classified

The `WHERE` clause and the `ON` conditions of the joins are split at `AND` into *conjuncts*. Each conjunct is labelled with the set of tables whose columns it reads:

| A conjunct that reads... | Is applied... |
| --- | --- |
| no table (`1 = 0`, `5 > 3`) | once, at the start; if it is false the result is empty |
| exactly one table | **while that table is scanned or probed**, before any join |
| two or more tables | at the first join step where all those tables are present |

A conjunct on one table may also choose that table's access path: `WHERE C.ID = 7` becomes a primary-key lookup, `WHERE Total BETWEEN 10 AND 20` an index range scan (see [access-paths.md](access-paths.md#choosing-an-index)). Conditions the index cannot answer are tested row by row during the scan.

A condition is split only at `AND`. An `OR` stays whole and goes wherever all the tables it mentions are available:

```sql
WHERE A.k = 1 OR B.w = 2      -- reads two tables: applied at the A-B join, not before
WHERE A.k = 1 OR A.k = 2      -- reads one table: pushed to A
```

## Outer joins: where pushdown stops

Pushing a condition below an outer join can change the answer, so the optimizer is careful. Take:

```sql
SELECT P.n FROM P LEFT JOIN Q ON Q.p_id = P.id WHERE Q.flag = 1;
```

Applying `Q.flag = 1` to `Q` first would happen to give the same answer here. But take the way to find the unmatched rows:

```sql
SELECT P.n FROM P LEFT JOIN Q ON Q.p_id = P.id WHERE Q.id IS NULL;
```

Pushed into `Q` before the join, `Q.id IS NULL` would leave `Q` with no rows at all (an id is never NULL), so *every* `P` row would look unmatched and the query would return all of `P`: the wrong answer. The condition is about the padding that the join adds, so it can only be tested after the join. So the rule is simple and strict:

> **A `WHERE` condition is never applied to a table on the nullable side of an outer join before the join.** It is applied to the joined rows, as written.

The nullable side is the right table of a `LEFT JOIN`, every table to the left of a `RIGHT JOIN`, and both sides of a `FULL JOIN`.

The preserved side is safe: in `P LEFT JOIN Q ... WHERE P.n = 'a'`, the condition on `P` is applied to `P` first, because a `P` row that fails it would be removed after the join anyway.

### ON conditions of a LEFT JOIN

A condition in the `ON` of a `LEFT JOIN` that reads only the right table decides which right rows *can match*. A right row that fails it matches nothing, which is the same as being absent, so it can be filtered out first:

```sql
FROM P LEFT JOIN Q ON Q.p_id = P.id AND Q.flag = 1
```

`Q.flag = 1` filters `Q` before the join; every `P` row still appears, padded with NULLs when no `Q` row matches. This is exactly the difference between putting a condition in `ON` and in `WHERE` on the nullable side, and the optimizer preserves it:

```sql
-- ON: every P row appears
SELECT P.n, Q.id FROM P LEFT JOIN Q ON Q.p_id = P.id AND Q.flag = 1;
-- WHERE: only P rows with a flagged Q row
SELECT P.n, Q.id FROM P LEFT JOIN Q ON Q.p_id = P.id WHERE Q.flag = 1;
```

For `RIGHT` and `FULL` joins no `ON` condition is pushed, because the right (or both) sides are preserved.

## Inner joins: ON and WHERE are the same

For inner and cross joins, a condition means the same thing in `ON` and in `WHERE`. The optimizer pools all of them, so `FROM A JOIN B ON B.a = A.id WHERE B.w > 3` and `FROM A JOIN B ON B.a = A.id AND B.w > 3` are planned identically, and so is `FROM A, B WHERE B.a = A.id`. Write the join conditions where they read best.

## Seeing it

`EXPLAIN` counts the conditions applied to each scan:

```
scan C [50 rows]: full scan (1 condition(s) pushed down)
  -> est 5 rows, actual 1
```
