# The query optimizer

A query says *what* rows you want. The optimizer decides *how* to get them: which table to read first, which index to use, where to apply each condition, and whether a sort can be skipped. It never changes the answer, only how long it takes to arrive.

The optimizer is on by default. There is nothing to configure: no statistics to gather, no hints to write, no plan to invalidate. It looks at the tables as they are when the query runs; the statistics it uses are worked out on demand and thrown away when the table changes.

| Page | Contents |
| --- | --- |
| [pushdown.md](pushdown.md) | applying each condition to its own table before joining, and why outer joins are different |
| [join-order.md](join-order.md) | how inner joins are reordered, and the methods used to join |
| [access-paths.md](access-paths.md) | index selection for a single table, ordered reads that skip the sort, and the `COUNT(*)` shortcut |
| [estimates.md](estimates.md) | how row counts are estimated, and how good the estimates are |
| [crosstab.md](crosstab.md) | how `TRANSFORM … PIVOT` queries are executed, and the one-pass method that makes them fast |
| [views.md](views.md) | how simple views are merged into the queries that use them, and changes through views |
| [subqueries.md](subqueries.md) | semi-joins, correlated sub-queries in grouped queries, and derived tables read as their table |
| [prepared.md](prepared.md) | the statement cache, prepared statements, and why plans are not kept |
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
7. **View and derived-table merging.** A view or derived table that is a plain selection of columns of one table is read as that table, with its condition applied during the scan. See [views.md](views.md) and [subqueries.md](subqueries.md#derived-tables-and-views-read-as-their-table).
8. **Semi-joins.** A correlated `EXISTS`, `IN` or aggregate sub-query whose correlation is an equality is run once and looked up in a hash table, instead of once per outer key. See [subqueries.md](subqueries.md).
9. **Statistics.** Distinct counts, common values and histograms of unindexed columns, worked out on demand, sharpen the estimates behind the choices above. See [estimates.md](estimates.md).
10. **Row counts for free.** A bare `SELECT COUNT(*) FROM t` is the table's row count. See [access-paths.md](access-paths.md#count).

And what it deliberately does not do, so that you are not surprised:

* It does not reorder outer joins (`LEFT`, `RIGHT`, `FULL`).
* It does not store statistics or need `ANALYZE`: they are worked out when needed and dropped when the table changes. It does not know that two columns are related (see [estimates.md](estimates.md)).
* Functions whose value changes between calls (`NEXTVAL`, `CURRVAL`, `Now`, ...) are never folded into constants and never used to choose an index; everything else, `JSON_*` functions included, is deterministic and folded when its arguments are constants.
* It does not look inside sub-queries, derived tables or views that join, group, compute columns or limit rows to merge them with the outer query; each is planned on its own. (A plain selection of columns from one table *is* merged: [views.md](views.md).)
* It does not cache plans: every statement is planned again each time it runs, which is cheap and always right for the data of the moment. The parsed SQL *is* cached, and statements can be prepared ([prepared.md](prepared.md)).

## Where it sits

```
SQL text → parse → bind names → OPTIMIZE → execute → rows
                                   │
                  pushdown · join order · access paths · EXPLAIN
```

The optimizer works on the bound query, after names have been resolved to column positions and before any row is read. Its code is `crates/accessor-core/src/exec/optimizer.rs` (statistics, join ordering, plan log) and `exec/select.rs` (the planner and executor that use its decisions).

## Is it correct?

The optimizer is tested by comparison. A test builds a database with NULLs, ties and a spread of index shapes, then runs nearly forty query shapes (three-way inner joins, comma joins, `LEFT`, `RIGHT` and `FULL` joins with conditions in `ON` and in `WHERE`, self-joins, group by, sub-queries, and `ORDER BY ... LIMIT` with ties, `OFFSET` and both directions) with the optimizer on and again with it off. The answers must be identical, and for ordered queries the rows must come out in exactly the same order. A second test checks that the `EXPLAIN` output reports the decisions described in these pages.
