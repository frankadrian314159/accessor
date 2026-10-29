# Join order and join methods

## Why order matters

Joining three tables `A`, `B`, `C` can be done as `(A ⋈ B) ⋈ C`, `(B ⋈ C) ⋈ A`, or any other order, and the result is the same set of rows. But the work is not the same. If `A` has a million rows, `B` ten thousand and `C` one, starting with `A` produces a huge intermediate result that `C` then throws almost all of away, while starting with `C` keeps every intermediate result tiny.

For inner and cross joins any order is allowed, so the optimizer picks one.

## When reordering applies

| The `FROM` clause has... | Order |
| --- | --- |
| only `INNER JOIN`, `JOIN`, `CROSS JOIN`, comma joins, `USING` and `NATURAL` joins | chosen by the optimizer |
| `LEFT` joins whose `ON` condition reads only the table it joins and tables that no left join pads with NULLs, mixed with inner and cross joins | the other tables in the order chosen by the optimizer, then the left joins last, in the order written ([below](#left-joins-go-last)) |
| any `RIGHT` or `FULL` join, or a `LEFT` join whose `ON` reads another left-joined table | as written |
| more than 128 tables | as written (and without the optimizer's pushdown, so the conditions are applied after all the joins: keep such queries small) |
| the optimizer switched off | as written |

Outer joins are otherwise left alone because moving a table across an outer join can change which rows are padded with NULLs.

### Left joins go last

`FROM catalog_sales LEFT JOIN catalog_returns ON (...), warehouse, item, date_dim WHERE ...` keeps every `catalog_sales` row and adds the matching return, if any. A left join only adds columns to the rows it keeps, so it can be done after the tables that filter `catalog_sales`, with the same result. The optimizer does that: the inner and cross joins are ordered as usual, using the `WHERE` and their own `ON` conditions, and each left join follows, in the order written, probing or hashing the small set of rows that is left instead of the whole first table. This applies when the `ON` condition of every left join reads only the joined table and tables that no left join pads. A `WHERE` condition on a padded table is applied after the joins, as before. Without it, such a query joined all of its first table to the padded table before any filter ran: TPC-DS query 40 failed after 27 s, and query 49 took more than ten minutes; both now run in seconds.

The order you write has no effect on a reorderable query, and neither does the choice between `ON` and `WHERE` for the join conditions ([pushdown.md](pushdown.md#inner-joins-on-and-where-are-the-same)). The columns of `SELECT *` and the order of qualified names are still those of the tables as you wrote them: the rows are built in the join order and then rearranged back to the written column layout before the rest of the query sees them.

## How the order is chosen

The optimizer works greedily, one table at a time:

1. **Estimate each table.** For every table it estimates how many rows remain after that table's own pushed-down conditions ([estimates.md](estimates.md)).
2. **Try each table as the start.** The greedy steps below are run once from every table (for up to 16 tables; with more, only from the smallest). Each order is costed by the values it builds: the rows after each step times the width of those rows. The table with the fewest estimated rows is the usual start, and it is kept unless another start costs less than half as much. Starting from the smallest table is wrong when it links only to the largest one: a 5-row `warehouse` joined first to a 12-million-row `inventory` builds all 12 million rows before the date filter on a third table is applied, whereas starting from the filtered `date_dim` reads only the weeks in range. Left-joined tables that go last ([above](#left-joins-go-last)) are not part of the choice.
3. **Add the cheapest next table.** For each table not yet placed, it asks how that table can be reached from the tables already joined:

   | Reach | Meaning | Estimated rows out | Work |
   | --- | --- | --- | --- |
   | **unique** | an equality links it to the joined tables and a unique index covers the linked columns, so each row matches at most one | the rows so far, times the share of the table that its own conditions leave | one lookup per row |
   | **index** | an equality links it and some index starts with linked columns, and probing it is [worth it](#when-an-index-is-not-worth-probing) | the rows so far × the rows one probe finds (the table's rows over the distinct values of the linked index columns) × the share of the table that its own conditions leave | one lookup per row |
   | **hash** | an equality links it but no index helps | the equality-join estimate below | rows so far + the table's rows |
   | **cross** | no equality links it to the joined tables | the product of the two | the product |

   It picks the table with the **fewest rows out**, using the work as the tie-break and then the written order. A table with no join condition to the rest is therefore left until last, so that Cartesian products are done on the smallest possible inputs.

The equality-join estimate (for a **hash** reach) is rows so far × the table's rows ÷ the larger distinct count of the two join columns, from the [column statistics](estimates.md#the-estimate-for-a-join), counted over the whole table, not only the rows its conditions leave. For a foreign key joined to its primary key that is about as many rows as the many side has; for two columns with few distinct values it is a large multiple, which is why such a join is left until the other tables have thinned the inputs. Without statistics for both columns (a derived table, say) the estimate is the larger of the two inputs. It is a guess, not a measurement.

Each step applies the join conditions that have just become available ([pushdown.md](pushdown.md)).

## Narrow rows

The rows a join builds hold every column of every table in it. A self-join of a 34-column table (TPC-DS query 95 pairs the lines of each web order) therefore builds rows of 68 values, though the query reads three columns of each side: millions of such rows do not fit in memory, and under a `statement_timeout` they hit the limit on the size of a join result (rows times columns: 16 million values for a short time limit, growing by 4 million for each second allowed, up to 200 million), which cancels the statement with the timeout message. So a plain table in a query of two or more sources is read with **only the columns whose names the statement mentions** anywhere (the select list, `WHERE`, `ON`, `USING`, `GROUP BY`, `HAVING`, `ORDER BY`, window functions, sub-queries). The join keys, the conditions and the index probes still work on the table's own rows; only the rows handed to the rest of the query are cut down. Query 95 now builds rows of 6 values and runs in 27 s instead of being cancelled; query 39 went from 119 s to 30 s.

It is not done for `SELECT *` or `t.*`, `NATURAL` joins, `LATERAL` items, named windows, `FOR UPDATE`, tables with fewer than 8 columns, or views and derived tables (their rows are already what the inner query selected). A name counts as used wherever it appears, whichever table it belongs to, so a column is only left out when nothing could mean it.

## LATERAL and table functions

An item that reads earlier tables (`LATERAL (SELECT ...)`, or a table function whose arguments use them) depends on those tables: it is joined **after** all of them, and is taken to give about one row for each of their rows, so it is placed as soon as they are in. For `FROM a, b JOIN LATERAL (VALUES (a.x)) s(y) ON b.k = s.y` the order is `a`, the lateral item, then `b`, which is looked up through its index, instead of the 100-million-row product of `a` and `b` first. Queries with outer joins keep the order written. Each such item is run for each row of the tables before it, but only once for every **distinct** set of the outer values it reads, and the answers are remembered (up to a million rows at a time; beyond that the oldest are dropped and a repeated outer value runs again): for 10 000 orders belonging to 200 customers, a lateral item that uses only the customer runs 200 times. A table function with constant arguments (`GENERATE_SERIES(1, 100)`) is computed once and is an ordinary, reorderable source. Conditions in the `WHERE` that read only the lateral item's columns are applied to its rows as they come back. `EXPLAIN` says `LATERAL, run N time(s) for M left row(s)`.

## Join methods

Each step joins the rows so far (the *left*) to the next table (the *right*), by one of three methods:

### Index nested-loop

For each left row, look the matching right rows up through an index on the right table whose leading columns are the join columns. Cost grows with the left rows, not the right table's size. It is used when such an index exists, except when the right table has at most 64 rows and the left has more than four times as many (then hashing the small table once is cheaper), and except when the index [finds too much](#when-an-index-is-not-worth-probing).

Primary keys, unique constraints, foreign keys (which create an index on the referencing columns automatically) and `CREATE INDEX` all provide indexes. A composite index serves as many of its leading columns as the join equates.

#### When an index is not worth probing

An index finds, for one left row, every row with the same values in the columns the join equates. If those columns have few distinct values, that is a large part of the table: an index whose first column is the warehouse of every row, joined on that column alone, finds the whole table each time. The optimizer works out the rows a probe finds (the table's rows divided by the distinct values of the equated columns, from the [column statistics](estimates.md); one for a unique index that is fully equated), and probes only if left rows × that is no more than the table's rows plus the left rows, which is what hashing the table once costs. Otherwise the join is hashed, and the table's own conditions thin it out as it is read. `EXPLAIN` then says `hash join` where it would have said `index nested-loop`.

This matters most for foreign keys, which create an index on the referencing columns that a join on only the first of them can use. In TPC-C's Stock-Level query that index (`ol_w_id, ol_d_id, ol_o_id`, joined on `ol_w_id`) made the join run in 0.9 to 1.9 seconds instead of 11 ms whenever the statistics put `stock` first.

### Hash join

If there is an equality between expressions of the two sides but no usable index, one side is loaded into a hash table keyed on the join columns and the other probes it. Normally the right table is hashed; when the join is an inner join and the left side has fewer than half as many rows as the right table has slots, the **left** is hashed instead and the right table is streamed past it. Rows whose key contains NULL never match, as in SQL.

Any equality works, not only column = column: `ON LOWER(A.name) = LOWER(B.name)` is hashed.

### OR-ed equalities

When the condition has no plain equality but an `OR` whose every branch contains one (`t2.thousand = t1.a OR t2.thousand = t1.b OR t2.other = t1.c`), a hash table is built for each branch's right-hand side. For each left row the right rows found through every branch are taken together (a row that several branches find is taken once) and the whole condition is checked on them. `EXPLAIN` says `hash lookups for each of N OR-ed equalities`.

### Nested loop

With no equality to hash (`ON A.x < B.y`) every left row is compared with every right row. This is the slow case; add an equality condition if there is one, or accept it for small tables.

## What is checked at each step

Whatever the method, the rest of the join condition (anything that was not a plain equality the method used) is evaluated on each candidate pair, so correctness never depends on the method. The right table's own pushed-down conditions are applied to each candidate before pairing. For `LEFT` joins, left rows with no match are kept and padded; for `RIGHT` and `FULL` joins the unmatched right rows are added at the end.

## Seeing it

```
join order: C -> O -> L (reordered from the query's order)
...
join O: index nested-loop via ix_ord_cust (1 key column(s)); 1 rows in, 8 out ...
join L: hash join, built on the right table (2000 slots); 8 rows in, ...
```

The first line gives the order and says whether it differs from the written one; each `join` line gives the method, the rows in and out and the estimate. See [explain.md](explain.md).


## Cross products come last

At each step the source linked to the rows already joined (by an equality) is preferred to one that would make a cross product, even when the cross product is smaller right now. Two copies of a small table (TPC-H query 8 uses `nation` twice) would otherwise be multiplied first (5 × 25 rows) and every later step would carry that factor, when the big table that links each copy to the rest comes a step later. A cross product is chosen only when nothing left is linked.
