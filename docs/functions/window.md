# Window functions

A window function is written with an `OVER` clause and returns a value for every row, using a *window* of related rows:

```sql
function(arguments) OVER (
    [PARTITION BY expression, ...]
    [ORDER BY expression [ASC | DESC], ...]
    [ROWS | RANGE frame]
)
```

* `PARTITION BY` splits the rows into independent groups. Without it, all the rows form one.
* `ORDER BY` orders the rows inside each partition. Rows that tie are *peers*.
* The *frame* says which rows around the current one an aggregate or value function looks at. See [Frames](#frames).

Window functions may be used in the select list and in `ORDER BY`. In a query with `GROUP BY` they run after the grouping and may use aggregates in their arguments and clauses. See [select.md](../sql/select.md#window-functions) for what is not supported (named windows, `GROUPS` frames, `EXCLUDE`, `RANGE` with an offset).

## ROW_NUMBER

`ROW_NUMBER()` numbers the rows of each partition 1, 2, 3, ... in the window order. Rows with equal sort keys are numbered in an arbitrary but consistent order; add a tie-breaker to `ORDER BY` for a reproducible result.

## RANK, DENSE_RANK and ROW_NUMBER

| Function | Peers get | After a tie |
| --- | --- | --- |
| `RANK()` | the same rank | the next rank skips: 1, 1, 3 |
| `DENSE_RANK()` | the same rank | the next rank follows on: 1, 1, 2 |
| `ROW_NUMBER()` | different numbers | 1, 2, 3 |

```sql
SELECT Name, Sal, RANK() OVER (ORDER BY Sal DESC), DENSE_RANK() OVER (ORDER BY Sal DESC)
FROM Emp;
```

## PERCENT_RANK and CUME_DIST

* `PERCENT_RANK()` is (`RANK` − 1) / (rows in the partition − 1): 0 for the first, 1 for the last; 0 for a one-row partition.
* `CUME_DIST()` is the number of rows up to and including the current row's peers, divided by the number of rows in the partition.

Both return a `DOUBLE`.

## NTILE

`NTILE(n)` divides the partition into `n` groups of as equal a size as possible (earlier groups get the extra row) and returns the group number, 1 to `n`. `n` must be a positive whole number.

## LAG and LEAD

`LAG(expr [, offset [, default]])` returns `expr` from the row `offset` rows before the current row in the partition (default offset 1); `LEAD` from `offset` rows after. When there is no such row the result is `default`, or NULL. The frame is not used.

```sql
SELECT Day, Price, Price - LAG(Price) OVER (ORDER BY Day) AS Change FROM Quotes;
```

## FIRST_VALUE, LAST_VALUE and NTH_VALUE

`FIRST_VALUE(expr)`, `LAST_VALUE(expr)` and `NTH_VALUE(expr, n)` return `expr` from the first, last or *n*th row of the **frame**. With the default frame, `LAST_VALUE` is the current row (or its last peer): to get the last row of the partition, give the frame `ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING`. An empty frame, or an `n` beyond it, gives NULL.

## Aggregates over a window

Any [aggregate](aggregate.md) can be used with `OVER`: `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `STRING_AGG`, the variances, `EVERY`, and so on. It is computed over the frame of each row, so you can have running totals, moving averages and per-group totals next to the detail rows.

```sql
SELECT Day, Amount,
       SUM(Amount) OVER (ORDER BY Day)                                   AS RunningTotal,
       AVG(Amount) OVER (ORDER BY Day ROWS BETWEEN 6 PRECEDING AND CURRENT ROW) AS WeekAvg,
       SUM(Amount) OVER ()                                               AS Grand
FROM Sales;
```

`DISTINCT` is allowed (`COUNT(DISTINCT x) OVER (...)`).

## Frames

```
ROWS | RANGE  start
ROWS | RANGE  BETWEEN start AND end

start, end :=  UNBOUNDED PRECEDING | n PRECEDING | CURRENT ROW | n FOLLOWING | UNBOUNDED FOLLOWING
```

* `ROWS` counts rows: `ROWS BETWEEN 2 PRECEDING AND CURRENT ROW` is the row and the two before it.
* `RANGE` works on peers: `CURRENT ROW` means the current row *and all its peers*. Only `UNBOUNDED` and `CURRENT ROW` bounds are supported.
* With one bound given, the other end is `CURRENT ROW`.

**Default frame:** with an `ORDER BY`, `RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW` (a running aggregate, peers included); without one, the whole partition.
