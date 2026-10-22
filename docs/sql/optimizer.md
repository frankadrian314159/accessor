# The query optimizer and EXPLAIN

Accessor's optimizer decides *how* a query is run without changing *what* it returns. It is on by default and needs no set-up: there are no statistics to gather and no hints to write. This page describes what it does, how to see its decisions with `EXPLAIN`, and what it does not do.

## What it does

### 1. Conditions move to the table they are about

A condition in `WHERE` (or in the `ON` of an inner join) that reads only one table is applied while that table is being read, before any join, so that the join sees fewer rows. In

```sql
SELECT * FROM Lines L JOIN Orders O ON O.ID = L.OrderID WHERE L.Qty > 4 AND O.Total > 100;
```

each condition filters its own table first. Conditions that read two tables are applied as soon as both are available.

Conditions are **not** moved to the nullable side of an outer join, because that would change the answer: in `A LEFT JOIN B ON ... WHERE B.x = 1` the `WHERE` condition removes the NULL-padded rows after the join, and the optimizer leaves it there. A condition in the `ON` of a `LEFT JOIN` that reads only the right table may be applied to that table first, since it only decides which rows can match.

### 2. Inner joins are put in the best order

Inner and cross joins can be done in any order without changing the result, so the optimizer chooses one instead of using the order you wrote. It:

1. estimates how many rows each table will yield after its own conditions, using the table's size and, for conditions an index can answer, the number of rows the index would visit (counted exactly up to 4,096);
2. starts with the table that yields the fewest rows;
3. repeatedly adds the table that keeps the intermediate result smallest, preferring a table it can reach through an index on the join columns (a unique index best of all), then one it can hash, and putting tables with no join condition last.

The result columns come out in the order you wrote the tables, whatever order they were joined in. Joins that include `LEFT`, `RIGHT` or `FULL` keep the order written, because reordering them is not always safe.

### 3. Join methods

For each join step, in order of preference:

| Method | Used when |
| --- | --- |
| **index nested-loop** | the joined table has an index starting with the join columns: each left row looks up its matches |
| **hash join** | the join has an equality condition but no usable index: one side is hashed. For an inner join with a much smaller left side, the left is hashed and the right table is scanned |
| **nested loop** | there is no equality to hash |

### 4. ORDER BY with a limit uses an index

`ORDER BY ... LIMIT n` (or `TOP n`, `FETCH FIRST n`) on columns that an index holds in the same order is answered by reading the index in order and stopping after `n` qualifying rows, with no sort. The index may follow an equality on its leading columns (`WHERE a = 5 ORDER BY b` with an index on `(a, b)`). All the sort columns must be plain columns of one table, in the same direction. A very selective lookup is preferred over an ordered read, since a handful of rows sorts instantly.

### 5. COUNT(*) is free

`SELECT COUNT(*) FROM t` with no condition, grouping or limit is answered from the table's row count without reading any row.

### Other things that were already fast

An index is used for equality and range conditions on its leading columns, `LIKE` patterns are compiled, `ORDER BY ... LIMIT` without a usable index sorts only the winners (top-k), uncorrelated sub-queries run once, and correlated ones run once per distinct outer value.

## EXPLAIN

```sql
EXPLAIN query
EXPLAIN QUERY PLAN query
```

`EXPLAIN` **runs the query** (it is read-only) and returns, instead of its rows, one row of text per step of the plan, with the estimated and actual number of rows. The result has one column, `plan`.

```
accessor> EXPLAIN SELECT C.Name, O.Total, L.Item
      ...> FROM Line L JOIN Ord O ON O.ID = L.OrdID JOIN Cust C ON C.ID = O.CustID
      ...> WHERE C.Name = 'c7' AND L.Qty >= 0;
query over L, O, C
join order: C -> O -> L (reordered from the query's order)
scan C [50 rows]: full scan (1 condition(s) pushed down)
  -> est 5 rows, actual 1
join O: index nested-loop via ix_ord_cust (1 key column(s)); 1 rows in, 8 out (the table has about 400 rows after its own conditions)
join L: index nested-loop via ix_line_ord (1 key column(s)); 8 rows in, 40 out (the table has about 660 rows after its own conditions)
40 row(s) returned
```

Read it from the top:

* `query over ...` names the tables in the order you wrote them. Sub-queries, derived tables and common table expressions appear indented below the step that runs them.
* `join order` shows the order chosen, and says when it differs from the written one.
* `scan T [n rows]` shows the access path for a table: `full scan`, `index NAME on COLUMN = (value)` for an equality lookup, or `index range scan`. "Conditions pushed down" counts the conditions applied during the scan.
* `-> est E rows, actual A` compares the optimizer's guess with what was found. A large gap means the estimate was poor; the plan may then not be the best one.
* Each `join` line gives the method, the rows that went in from the left and came out, and the estimated size of the table being joined.
* `sort: top-k of N rows` or `sort: all N rows` shows a sort; `order by satisfied by index ...` shows that none was needed; `COUNT(*) answered from the table's row count` shows the shortcut.

`EXPLAIN` can be run from the shell, from the desktop app's query editor (the plan appears as the result grid), and from the library with `db.query("EXPLAIN ...")`.

## Turning the optimizer off

```
accessor> .optimizer off
```

or `db.set_optimizer(false)` from Rust. With the optimizer off, joins run in the order written, conditions apply after the joins, and no index is read for ordering. The answers are the same (that is tested), only slower; the setting exists so you can compare plans and timings, or rule the optimizer out when something looks wrong. Combine it with `.timer on` to see the difference.

On a database of a million rows in the largest table, a three-table join filtered by one customer's name took about 1.8 milliseconds with the optimizer and about 2.6 seconds without it; `SELECT COUNT(*)` went from 260 milliseconds to 0.02; a top-5 `ORDER BY` from 42 milliseconds to 19.

## Limits

* The estimates are simple: table sizes, exact counts for indexed conditions, and fixed fractions for the rest. They are good enough to order joins sensibly but are not a full cost model, and there is no `ANALYZE`.
* Outer joins are never reordered, and more than 63 tables in one `FROM` are joined as written.
* Conditions are not pushed into or out of sub-queries or views, which are planned separately; a derived table is computed in full before it is joined.
* Index use for conditions follows the rules in [select.md](select.md): comparisons of a column with a constant. Expressions on columns (`WHERE Year(d) = 2024`) cannot use an index, but `WHERE d >= #2024-01-01# AND d < #2025-01-01#` can.
* The plan is made again every time a statement runs; nothing is cached between runs.
