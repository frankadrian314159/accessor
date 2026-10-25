# The query optimizer

A query says *what* rows you want. The optimizer decides *how* to get them: which table to read first, which index to use, where to apply each condition, and whether a sort can be skipped. It never changes the answer, only how long it takes to arrive.

The optimizer is on by default. There is nothing to configure: no statistics to gather, no hints to write, no plan to cache. It looks at the tables as they are when the query runs.

| Page | Contents |
| --- | --- |
| [pushdown.md](pushdown.md) | applying each condition to its own table before joining, and why outer joins are different |
| [join-order.md](join-order.md) | how inner joins are reordered, and the methods used to join |
| [access-paths.md](access-paths.md) | index selection for a single table, ordered reads that skip the sort, and the `COUNT(*)` shortcut |
| [estimates.md](estimates.md) | how row counts are estimated, and how good the estimates are |
| [crosstab.md](crosstab.md) | how `TRANSFORM … PIVOT` queries are executed, and the one-pass method that makes them fast |
| [views.md](views.md) | how simple views are merged into the queries that use them, and changes through views |
| [explain.md](explain.md) | reading the output of `EXPLAIN` |
| [tuning.md](tuning.md) | helping the optimizer: indexes, writing conditions, switching it off, limits |

## An example

Three tables, a million rows in the biggest, and a query that mentions the big table first:

```sql
SELECT Count(*)
FROM Line L
JOIN Ord  O ON O.ID = L.OrdID
JOIN Cust C ON C.ID = O.CustID
WHERE C.Name = 'c77';
```

Run exactly as written, this scans all of `Line`, looks each line's order up, and only at the end asks whether the customer is the one wanted. The optimizer sees that `C.Name = 'c77'` leaves a single customer, so it starts there, finds that customer's orders through an index on `Ord.CustID`, and finds those orders' lines through an index on `Line.OrdID`. The same query goes from about 2.6 seconds to 1.8 milliseconds. `EXPLAIN` shows the choice:

```
query over L, O, C
join order: C -> O -> L (reordered from the query's order)
scan C [50 rows]: full scan (1 condition(s) pushed down)
  -> est 5 rows, actual 1
join O: index nested-loop via ix_ord_cust (1 key column(s)); 1 rows in, 8 out ...
join L: index nested-loop via ix_line_ord (1 key column(s)); 8 rows in, 40 out ...
```

## What it does, in one list

1. **Pushdown.** A condition that reads one table is applied while that table is read, before joining. See [pushdown.md](pushdown.md).
2. **Join reordering.** When all the joins are inner (or cross) joins, they are done in the cheapest order rather than the written one. See [join-order.md](join-order.md).
3. **Join methods.** Index nested-loop when the joined table has a suitable index, a hash join otherwise, with the smaller side hashed. See [join-order.md](join-order.md#join-methods).
4. **Index selection.** Each table is read through the best index for its conditions. See [access-paths.md](access-paths.md#choosing-an-index).
5. **Ordered reads.** `ORDER BY ... LIMIT` that matches an index reads the index in order and stops early. See [access-paths.md](access-paths.md#order-by-with-a-limit).
6. **One-pass crosstabs.** A `TRANSFORM … PIVOT` query is computed by a single grouped pass instead of one conditional aggregate per column. See [crosstab.md](crosstab.md).
7. **View merging.** A view that is `SELECT * FROM table WHERE ...` is read as its table, with its condition applied during the scan. See [views.md](views.md).
8. **Row counts for free.** A bare `SELECT COUNT(*) FROM t` is the table's row count. See [access-paths.md](access-paths.md#count).

And what it deliberately does not do, so that you are not surprised:

* It does not reorder outer joins (`LEFT`, `RIGHT`, `FULL`).
* It does not keep statistics or need `ANALYZE`; its estimates are simple (see [estimates.md](estimates.md)).
* It does not look inside sub-queries, derived tables or views that join, group or project columns to merge them with the outer query; each is planned on its own. (A view of the form `SELECT * FROM table WHERE ...` *is* merged: [views.md](views.md).)
* It does not cache plans: every statement is planned again each time it runs.

## Where it sits

```
SQL text → parse → bind names → OPTIMIZE → execute → rows
                                   │
                  pushdown · join order · access paths · EXPLAIN
```

The optimizer works on the bound query, after names have been resolved to column positions and before any row is read. Its code is `crates/accessor-core/src/exec/optimizer.rs` (statistics, join ordering, plan log) and `exec/select.rs` (the planner and executor that use its decisions).

## Is it correct?

The optimizer is tested by comparison. A test builds a database with NULLs, ties and a spread of index shapes, then runs nearly forty query shapes (three-way inner joins, comma joins, `LEFT`, `RIGHT` and `FULL` joins with conditions in `ON` and in `WHERE`, self-joins, group by, sub-queries, and `ORDER BY ... LIMIT` with ties, `OFFSET` and both directions) with the optimizer on and again with it off. The answers must be identical, and for ordered queries the rows must come out in exactly the same order. A second test checks that the `EXPLAIN` output reports the decisions described in these pages.
