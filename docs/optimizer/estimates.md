# How row counts are estimated

The optimizer's choices (which table first, whether to take a lookup or a scan, whether to hash a sub-query) rest on guesses of how many rows each table will yield after its own conditions. The guesses come from the table as it is when the query runs, from two sources: counts read off the **indexes**, and **column statistics** worked out from the rows.

Nothing is stored in the file and nothing has to be run (there is no `ANALYZE`). Statistics are computed the first time a query needs them and kept in memory **only until the table next changes**, so they cannot be out of date after a bulk load, and the same data always gives the same plan.

## Column statistics

For a column, the statistics are:

| | |
| --- | --- |
| rows, NULLs | how many rows were looked at, and how many were NULL |
| distinct values | how many different values the column holds |
| most common values | up to ten values that occur more than once, each with the fraction of the rows it makes up |
| histogram | 33 boundary values, from the smallest to the largest, with an equal share of the rows between neighbours |

They are worked out by reading the column once and sorting it. A table of more than 20,000 rows is read through a **sample** of about 20,000 rows picked by a fixed hash of the row position (so data that repeats with some period is not sampled in step with it); the distinct count is then estimated from the sample (a value seen once in the sample stands for many unseen ones). Statistics are gathered for a column only when a query has a condition on it that no index answers, or when it is a join column.

In the shell, `.stats table` prints them: distinct values, NULL share, range and the commonest values for each column. From Rust: `Table::column_stats(column_position)`.

## The estimate for one table

Given a table and the conditions pushed down to it:

1. **No conditions:** the number of rows in the table, which is known exactly.
2. **Choose the access path** as described in [access-paths.md](access-paths.md#choosing-an-index), and estimate what it returns:

   | Access path | Estimate |
   | --- | --- |
   | a unique index with every column equated | exactly 1 |
   | an equality on an index prefix | the **exact number** of index entries with that prefix, counted up to 4,096; past that, the table's size times the share the statistics give those values |
   | a range on an index | the exact number of entries in the range, counted up to 4,096; past that, the table's size times the share the histogram gives the range |
   | none (full scan) | the whole table |

   Counting walks the index entries, never the rows, and stops at the cap, so it costs at most a few thousand steps whatever the table size.

3. **The remaining conditions** (those the index did not answer) each multiply the estimate by the fraction of rows they are expected to keep. For a column compared with constants, the fraction comes from the statistics:

   | Condition | Fraction kept |
   | --- | --- |
   | `col = v` | the value's own share if it is among the most common; otherwise what is left after the common values and NULLs, divided among the other distinct values |
   | `col <> v` | the rest, less the NULLs |
   | `col < v`, `<=`, `>`, `>=` | the share of the histogram below or above `v` (interpolated between boundaries for numbers) |
   | `col BETWEEN a AND b` | the share between `a` and `b` |
   | `col IN (...)` | the sum of the values' shares |
   | `col IS [NOT] NULL` | the NULL share, or the rest |
   | `col LIKE 'abc*'` | the share of the range of text starting `abc` (a pattern that begins with a wildcard has no such range) |
   | `a AND b` | the product (the columns are taken to be independent) |
   | `a OR b` | `a + b − a·b` |
   | `NOT a` | one minus `a` |

   Where there are no statistics for a condition (a function of a column, two columns compared with each other, a constant of a different kind from the column), a **fixed fraction** is used: 0.1 for an equality, 0.9 for `<>`, 0.33 for other comparisons, 0.25 for `BETWEEN` and `LIKE`, 0.1 per listed value of `IN` (at most 0.5), 0.05 for `IS NULL`, 0.5 for anything else.

4. The result is kept between 1 and the table's size.

For a derived table, view, or common table expression that is not merged, the count is the number of rows it produced, known by the time the join is planned, with the fixed fractions for its conditions (it has no statistics).

## The estimate for a join

When the optimizer weighs adding a table to the rows joined so far, it uses the textbook rule: an equality join returns about

> rows so far × rows of the table ÷ the larger of the two columns' distinct counts

(each count taken over the whole table: the rows that remain after a table's own conditions are a share of its key domain, and joining them keeps that share of the other side. Counting only the remaining keys would make a join with 365 days of a 73,049-day `date_dim` look as large as the join with all of them). An **index** join is estimated from the index instead: the rows one probe finds, which for a primary key `(date, item, warehouse)` equated on `date` and `item` is the number of warehouses, not the number of rows per item. A foreign key joined to its primary key therefore returns about as many rows as the many side has, as before, but a join of two columns with few distinct values, which multiplies rows, is seen as the explosion it is, and is put off until other conditions have thinned the inputs. Where either side has no statistics the older rule applies: about as many rows as the larger input. A unique index guarantees at most one match per row, so the rows do not grow; a join with no equality is a product. See [join-order.md](join-order.md#how-the-order-is-chosen).

## How good are the estimates?

Good where it matters, and now much better for unindexed columns:

* A lookup of one key on a unique index is exact.
* Equalities and ranges on indexed columns are counted exactly when they are selective, which is when a mistake would be costly.
* An unindexed column with skew is where the old fixed fractions were worst. A `status` column that is 95% `'open'`: `status = 'open'` was guessed at a tenth of the table, and is now guessed at 95%; `status = 'closed'`, really 4.5%, was guessed at a tenth and is now guessed at 4.5%.
* The independence assumption remains: `city = 'Rome' AND country = 'Italy'` is estimated as if the two were unrelated, which underestimates when they are correlated.
* A sample's distinct count is an estimate, and a column with a handful of rare values may have its rarest missed by the sample.

The consequence of a poor estimate is a plan that is worse than the best, not a wrong answer. `EXPLAIN` prints the estimate next to the real count for each scan, for single tables and joins alike:

```
scan t [2000 rows]: full scan (1 condition(s) pushed down)
  -> est 1900 rows, actual 1900
```

When the estimates still mislead the optimizer, the usual cure is an index on the filtered column: that turns a statistical estimate into an exact count. See [tuning.md](tuning.md).

## What it costs

Computing a column's statistics reads (a sample of) its rows once, about as much as one scan, and only for a column used in a condition the indexes do not answer, only after the table has changed since the last time. A table that is changed between every two queries, and queried on unindexed columns each time, pays that scan each time, a cost of the same order as the scan that query was about to do anyway. Statistics are copied with the table when a transaction takes a snapshot of it, and recomputed lazily.

## When transactions overlap

In a database shared between threads ([concurrency](../concurrency.md)) a table can hold, besides the current rows, older versions that an open transaction still needs, and rows not yet committed. The estimates use the number of current rows (`Table::estimated_len`) and count index entries without checking each against the reader's snapshot, so they can be a little high while such versions exist. This changes only which plan is chosen, never an answer: the rows a query returns are always those its transaction can see.
