# Access paths: indexes, ordered reads and COUNT(*)

An *access path* is the way rows of one table are found. The choices are a full scan, a lookup of one key, a scan of a range of keys, or a walk along an index in order.

## Choosing an index

For each table, the optimizer looks at the pushed-down conditions that compare a single column of that table with a constant:

| Condition | Usable as |
| --- | --- |
| `col = constant` | an equality on `col` |
| `col < c`, `<=`, `>`, `>=` | a range bound on `col` |
| `col BETWEEN a AND b` | a range on `col` |
| `constant op col` | the same, with the operator turned round |

(`NULL` constants, and constants of a type that cannot be compared with the column through the index, are not used. Conditions on expressions, such as `Year(d) = 2024`, `col + 1 = 5`, `LIKE`, `IN` and `OR`, are applied to rows as they are read but do not select an index.)

It then scores each index of the table and takes the best:

| Match | Score |
| --- | --- |
| a **unique** index with every column equated | best: at most one row |
| any index whose leading columns are equated (the more columns, the better) | next |
| an index whose first column has a range condition | next |
| none | full scan |

So with `PRIMARY KEY (a, b)` and an index on `(a, c)`, `WHERE a = 1 AND b = 2` uses the primary key as a single lookup; `WHERE a = 1` can use either, and `WHERE b = 2` uses neither, because `b` is not a leading column.

Whatever the index returns is only a superset: **all** the table's conditions are still tested on every row read, so the index can only make a query faster, never different.

Numbers of any type can probe a numeric index: `WHERE intcol = 5.0` uses an index on an `INTEGER` column, because the index orders all numbers by exact value.

## ORDER BY with a limit

Sorting is the other big cost. `ORDER BY x LIMIT 10` over a million rows, done naively, sorts a million rows to keep ten. Without an index the engine does what it can (it keeps only the top ten while scanning, a *top-k* selection, which is much cheaper than a full sort). But when an index already holds the rows in the wanted order, it can read the index from the start and stop after ten rows qualify.

The optimizer does this when **all** of these hold:

* the query reads one table, with no joins, no `GROUP BY` or aggregates, no `DISTINCT`, no window functions;
* it has a row limit: `LIMIT`, `TOP`, or `FETCH FIRST` (an `OFFSET` is added to the number of rows needed);
* every `ORDER BY` item is a plain column of the table (written directly, or as an alias or position that refers to one), and all go in the same direction, all ascending or all descending;
* some index starts with exactly those columns, in that order, optionally after columns the query fixes with equalities.

Then the index is read in order (backwards for `DESC`), each row is checked against the conditions, and the read stops as soon as enough rows have qualified.

```sql
CREATE INDEX byTotal ON Orders (Total);
SELECT * FROM Orders ORDER BY Total DESC LIMIT 5;          -- reads the top of byTotal

CREATE INDEX byCustDate ON Orders (CustID, Placed);
SELECT * FROM Orders WHERE CustID = 7 ORDER BY Placed LIMIT 3;   -- equality, then order
```

**The answer is the same as sorting.** Rows that tie on the sort columns come out in the order they would after a stable sort, that is, by when they were stored, in both directions. NULLs come first when ascending and last when descending, as in a sort.

### When it chooses not to

If another index gives a very selective lookup (the condition matches fewer than about two thousand rows), the optimizer uses that and sorts the few rows: that is cheaper than walking a big index and discarding most of what it finds. If the condition is a range on the sort column, it also uses the normal path.

In the plan:

```
order by satisfied by index ix_ord_total (descending), stopping after 5 rows: no sort
  -> 5 rows read in index order
```

compared with the sorting case:

```
sort: top-5 of 25 rows
```

## COUNT(*)

`SELECT COUNT(*) FROM t`, with no `WHERE`, `GROUP BY`, `HAVING`, `DISTINCT` or limit and nothing else in the select list but `COUNT(*)`, is answered from the number of rows the table already knows it has. No row is read:

| | 1,000,000 rows |
| --- | --- |
| optimizer on | 0.02 ms |
| optimizer off | 260 ms |

`COUNT(column)`, `COUNT(DISTINCT ...)` and counts with a condition read the rows as usual.

## Other single-table savings

These are always in effect, with or without the optimizer:

* A query with a limit and no `ORDER BY` stops scanning as soon as it has enough rows.
* `ORDER BY ... LIMIT` without a matching index selects the best *k* rows rather than sorting all of them.
* A `LIKE` pattern is compiled once, with fast paths for a fixed prefix, suffix, or "contains".
* Constant sub-expressions are evaluated once, before the scan.
