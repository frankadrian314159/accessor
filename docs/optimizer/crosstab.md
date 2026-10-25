# Crosstab queries and the optimizer

A crosstab (`TRANSFORM … PIVOT`, see [select.md](../sql/select.md#crosstab-queries-transform--pivot)) turns the distinct values of one column into columns of the result. It is planned in two stages, and the optimizer chooses between two ways of doing the second.

## The two ways to compute a crosstab

Take

```sql
TRANSFORM Sum(Amount)
SELECT Region
FROM Sales
GROUP BY Region
PIVOT Quarter;
```

### The plain rewrite

The query becomes an ordinary grouped query with one extra aggregate per pivot value, each restricted to the rows having that value:

```sql
SELECT Region,
       Sum(IIf(Quarter = 1, Amount, NULL)) AS [1],
       Sum(IIf(Quarter = 2, Amount, NULL)) AS [2],
       ...
FROM Sales GROUP BY Region;
```

The values are discovered first, by a separate `SELECT DISTINCT Quarter FROM Sales` over the same tables and conditions (unless you gave an `IN (...)` list). This is simple and always correct, but every input row is tested against **every column's** condition, so the cost grows with *rows × columns*. With a hundred pivot values the work is a hundred times that of a plain `GROUP BY`.

### One grouped pass (the optimized way)

The optimizer groups **once** by the row headings *and* the pivot value, so each cell is produced directly:

```sql
SELECT Region, Quarter, Sum(Amount) FROM Sales GROUP BY Region, Quarter;
```

The pivot values are read from that result (no separate discovery query), the cells are laid out in a table in memory, and the row headings, with any other aggregates in the `SELECT` list, are computed by the ordinary query and joined to their cells by the heading values. The cost is two passes over the data, whatever the number of columns.

| 100,000 rows, 100 pivot values | Time |
| --- | --- |
| plain rewrite (optimizer off) | about 1.26 s |
| one grouped pass (optimizer on) | about 0.52 s |

With few columns the difference is small (ten columns over 100,000 headings: 2.1 s against 2.5 s); the benefit grows with the number of columns.

Both passes are ordinary queries, so everything else in the optimizer applies to them: conditions are pushed down, joins are reordered, indexes are used. A crosstab over a three-table join is as well planned as the join alone.

## When the one-pass method is used

It is used whenever the optimizer is on, except when the query has:

* `SELECT DISTINCT`;
* `ROLLUP`, `CUBE` or `GROUPING SETS`;
* a `TRANSFORM` aggregate with more than one argument (such as `STRING_AGG(x, ', ')`).

Those use the plain rewrite. The two methods give **identical results**, including the columns, their order, and the contents of empty cells (`0` for a `Count`, NULL for the others). That is tested by running crosstabs of several shapes (joins, `HAVING`, `WHERE`, expressions as the pivot, `IN` lists, `Count(DISTINCT …)`, `Avg`) both ways.

## What EXPLAIN shows

```
crosstab: one grouped pass computed 6 cell(s) for 5 pivot column(s) (no separate query per column)
```

followed by the plans of the two queries it ran, indented. With the optimizer off the line is

```
crosstab: 5 pivot column(s)
```

and the plan that follows is that of the rewritten query. See [explain.md](explain.md).

## Tips

* The row headings must be in the `GROUP BY`, as for any grouped query.
* Index the pivot column if the crosstab has a `WHERE` on it, or the headings if there are many; the passes are ordinary scans and joins and use indexes the usual way.
* If you know the columns, list them with `PIVOT col IN (…)`: the output is fixed in shape, and rows with other values are left out of the columns (they still count in any other aggregate you select).
* A crosstab with thousands of pivot values makes a very wide result. Group the values first (`PIVOT Year(d)` rather than `PIVOT d`).
