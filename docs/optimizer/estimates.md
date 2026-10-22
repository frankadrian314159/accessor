# How row counts are estimated

The optimizer's choices (which table first, whether to take a lookup or a scan) rest on guesses of how many rows each table will yield after its own conditions. Accessor keeps **no stored statistics**: nothing to analyse, nothing that goes stale after a bulk load. Each estimate is made from the table as it is when the query runs.

## The estimate for one table

Given a table and the conditions pushed down to it:

1. **No conditions:** the number of rows in the table, which is known exactly.
2. **Choose the access path** as described in [access-paths.md](access-paths.md#choosing-an-index), and estimate what it returns:

   | Access path | Estimate |
   | --- | --- |
   | a unique index with every column equated | exactly 1 |
   | an equality on an index prefix | the **exact number** of index entries with that prefix, counted up to 4,096; if the count reaches 4,096, 10% of the table |
   | a range on an index | the exact number of entries in the range, counted up to 4,096; if it reaches 4,096, 33% of the table |
   | none (full scan) | the whole table |

   Counting walks the index entries, never the rows, and stops at the cap, so an estimate costs at most a few thousand steps whatever the table size.

3. **The remaining conditions** (those the index did not answer) each multiply the estimate by a fixed fraction:

   | Condition | Fraction kept |
   | --- | --- |
   | equality (`=`) | 0.1 |
   | not equal (`<>`) | 0.9 |
   | other comparison (`<`, `>`, ...) | 0.33 |
   | `BETWEEN` | 0.25 |
   | `LIKE` | 0.25 |
   | `IN (...)` | 0.1 per listed value, at most 0.5 |
   | `IS NULL` | 0.05 |
   | anything else (`OR`, functions, ...) | 0.5 |

4. The result is kept between 1 and the table's size.

For a derived table, view, or common table expression the count is the number of rows it produced, which is known by the time the join is planned (it is computed before being joined).

## The estimate for a join

When the optimizer weighs adding a table to the rows joined so far, it uses the rough rule that a join on an equality returns about **as many rows as the larger input** (the foreign-key-to-primary-key pattern) unless a unique index guarantees at most one match per row, in which case the rows do not grow. A join with no equality is a product. See [join-order.md](join-order.md#how-the-order-is-chosen).

## How good are the estimates?

Good where it matters:

* A lookup of one key on a unique index is exact.
* Equalities and ranges on indexed columns are counted exactly when they are selective, which is when a mistake would be costly.
* Unindexed conditions are crude. A table filtered by `Name = 'c77'` with no index on `Name` is estimated at one tenth of its rows, though the real figure may be one row or all of them.

The consequence of a poor estimate is a plan that is worse than the best, not a wrong answer. `EXPLAIN` prints the estimate next to the real count for each scan, so a bad guess is easy to spot:

```
scan C [50 rows]: full scan (1 condition(s) pushed down)
  -> est 5 rows, actual 1
```

When the estimates mislead the optimizer, the usual cure is an index on the filtered column: that turns a fixed fraction into an exact count. See [tuning.md](tuning.md).

## Why no statistics?

The engine keeps its data in memory, and counting entries in an ordered index is fast and always correct. A histogram would be more accurate for unindexed columns, but it would have to be stored, kept in step with every write and refreshed after bulk changes, and it would be one more thing that could be stale. This design keeps the optimizer stateless: the same data always gives the same plan.
