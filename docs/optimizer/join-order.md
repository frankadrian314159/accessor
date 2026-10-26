# Join order and join methods

## Why order matters

Joining three tables `A`, `B`, `C` can be done as `(A ⋈ B) ⋈ C`, `(B ⋈ C) ⋈ A`, or any other order, and the result is the same set of rows. But the work is not the same. If `A` has a million rows, `B` ten thousand and `C` one, starting with `A` produces a huge intermediate result that `C` then throws almost all of away, while starting with `C` keeps every intermediate result tiny.

For inner and cross joins any order is allowed, so the optimizer picks one.

## When reordering applies

| The `FROM` clause has... | Order |
| --- | --- |
| only `INNER JOIN`, `JOIN`, `CROSS JOIN`, comma joins, `USING` and `NATURAL` joins | chosen by the optimizer |
| any `LEFT`, `RIGHT` or `FULL` join | as written |
| more than 128 tables | as written (and without the optimizer's pushdown, so the conditions are applied after all the joins: keep such queries small) |
| the optimizer switched off | as written |

Outer joins are left alone because moving a table across an outer join can change which rows are padded with NULLs.

The order you write has no effect on a reorderable query, and neither does the choice between `ON` and `WHERE` for the join conditions ([pushdown.md](pushdown.md#inner-joins-on-and-where-are-the-same)). The columns of `SELECT *` and the order of qualified names are still those of the tables as you wrote them: the rows are built in the join order and then rearranged back to the written column layout before the rest of the query sees them.

## How the order is chosen

The optimizer works greedily, one table at a time:

1. **Estimate each table.** For every table it estimates how many rows remain after that table's own pushed-down conditions ([estimates.md](estimates.md)).
2. **Start with the smallest.** The table with the fewest estimated rows goes first. Ties go to the table written first.
3. **Add the cheapest next table.** For each table not yet placed, it asks how that table can be reached from the tables already joined:

   | Reach | Meaning | Estimated rows out | Work |
   | --- | --- | --- | --- |
   | **unique** | an equality links it to the joined tables and a unique index covers the linked columns, so each row matches at most one | the rows so far (no growth) | one lookup per row |
   | **index** | an equality links it and some index starts with a linked column | the equality-join estimate below | one lookup per row |
   | **hash** | an equality links it but no index helps | the equality-join estimate below | rows so far + the table's rows |
   | **cross** | no equality links it to the joined tables | the product of the two | the product |

   It picks the table with the **fewest rows out**, using the work as the tie-break and then the written order. A table with no join condition to the rest is therefore left until last, so that Cartesian products are done on the smallest possible inputs.

The equality-join estimate is rows so far × the table's rows ÷ the larger distinct count of the two join columns, from the [column statistics](estimates.md#the-estimate-for-a-join). For a foreign key joined to its primary key that is about as many rows as the many side has; for two columns with few distinct values it is a large multiple, which is why such a join is left until the other tables have thinned the inputs. Without statistics for both columns (a derived table, say) the estimate is the larger of the two inputs. It is a guess, not a measurement.

Each step applies the join conditions that have just become available ([pushdown.md](pushdown.md)).

## LATERAL and table functions

An item that reads earlier tables (`LATERAL (SELECT ...)`, or a table function whose arguments use them) depends on those tables, so a query containing one keeps the order written and is not reordered. Each such item is run for each row of the tables before it, but only once for every **distinct** set of the outer values it reads, and the answers are remembered: for 10 000 orders belonging to 200 customers, a lateral item that uses only the customer runs 200 times. A table function with constant arguments (`GENERATE_SERIES(1, 100)`) is computed once and is an ordinary, reorderable source. Conditions in the `WHERE` that read only the lateral item's columns are applied to its rows as they come back. `EXPLAIN` says `LATERAL, run N time(s) for M left row(s)`.

## Join methods

Each step joins the rows so far (the *left*) to the next table (the *right*), by one of three methods:

### Index nested-loop

For each left row, look the matching right rows up through an index on the right table whose leading columns are the join columns. Cost grows with the left rows, not the right table's size. It is used when such an index exists, except when the right table has at most 64 rows and the left has more than four times as many: then hashing the small table once is cheaper.

Primary keys, unique constraints, foreign keys (which create an index on the referencing columns automatically) and `CREATE INDEX` all provide indexes. A composite index serves as many of its leading columns as the join equates.

### Hash join

If there is an equality between expressions of the two sides but no usable index, one side is loaded into a hash table keyed on the join columns and the other probes it. Normally the right table is hashed; when the join is an inner join and the left side has fewer than half as many rows as the right table has slots, the **left** is hashed instead and the right table is streamed past it. Rows whose key contains NULL never match, as in SQL.

Any equality works, not only column = column: `ON LOWER(A.name) = LOWER(B.name)` is hashed.

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
