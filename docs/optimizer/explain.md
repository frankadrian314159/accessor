# EXPLAIN

```sql
EXPLAIN query
EXPLAIN QUERY PLAN query
```

`EXPLAIN` shows how a query was planned. The two forms are the same.

**It runs the query.** `EXPLAIN` executes the `SELECT` (it is read-only, so nothing changes) and returns, instead of its rows, a text description of the plan with the real row counts at each step. That makes the figures trustworthy, and it means an `EXPLAIN` of a slow query is as slow as the query. Only queries are run: `EXPLAIN` of an `INSERT`, `UPDATE`, `DELETE` or other statement says what kind of statement it is and that it was **not run**.

**Options.** PostgreSQL's forms are accepted: `EXPLAIN ANALYZE`, `EXPLAIN ANALYZE VERBOSE`, and `EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, SETTINGS, BUFFERS, WAL, TIMING, SUMMARY, FORMAT TEXT, ...) query`. They change nothing: the plan is always the plain text plan with the real row counts, there are no cost figures to leave out, and only `FORMAT TEXT` is available (`FORMAT JSON` is an error, like an option that does not exist).

The result has one column, called `plan`, with one row per line of text.

## Where to run it

* **Shell:** type `EXPLAIN SELECT ...;`. Plan lines are printed in full, not shortened like other result cells.
* **Desktop app:** run `EXPLAIN SELECT ...` in the query editor; the plan appears in the result grid.
* **Library:** `db.query("EXPLAIN SELECT ...")?` returns a `QueryResult` with the `plan` column.

## A complete example

```sql
CREATE TABLE Cust (ID INTEGER PRIMARY KEY, Name TEXT);
CREATE TABLE Ord  (ID INTEGER PRIMARY KEY, CustID INTEGER, Total INTEGER);
CREATE TABLE Line (ID INTEGER PRIMARY KEY, OrdID INTEGER, Item TEXT);
CREATE INDEX ix_ord_cust ON Ord (CustID);
CREATE INDEX ix_line_ord ON Line (OrdID);

EXPLAIN SELECT C.Name, O.Total, L.Item
FROM Line L JOIN Ord O ON O.ID = L.OrdID JOIN Cust C ON C.ID = O.CustID
WHERE C.Name = 'c7';
```

```
query over L, O, C
join order: C -> O -> L (reordered from the query's order)
scan C [50 rows]: full scan (1 condition(s) pushed down)
  -> est 5 rows, actual 1
join O: index nested-loop via ix_ord_cust (1 key column(s)); 1 rows in, 8 out (the table has about 400 rows after its own conditions)
join L: index nested-loop via ix_line_ord (1 key column(s)); 8 rows in, 40 out (the table has about 660 rows after its own conditions)
40 row(s) returned
```

## Reading the lines

### `query over A, B, C`

The start of a `SELECT`, naming its tables in the order they were written. A query with no tables says `no tables`.

### `join order: C -> O -> L`

The order in which the tables are joined. "(reordered from the query's order)" is added when it differs from the written one. Outer joins always show the written order.

### `scan T [n rows]: ...`

How table `T`, which has `n` rows, is read. After the colon is the access path:

| Text | Meaning |
| --- | --- |
| `full scan` | every row is read |
| `index NAME on COL = (value)` | a lookup of one key (or key prefix) through index `NAME` |
| `index range scan NAME on COL` | a range of keys through index `NAME` |
| `(computed rows)` | the source is a derived table, view or CTE: its rows were computed first |

Followed by `(k condition(s) pushed down)` when `k` conditions are applied during the scan ([pushdown.md](pushdown.md)).

### `-> est E rows, actual A`

Under a scan: the estimated rows ([estimates.md](estimates.md)) and the rows actually produced. A single-table query shows only `-> A rows`.

### `join T: METHOD; X rows in, Y out (...)`

One join step, adding table `T`. `METHOD` is one of:

* `index nested-loop via NAME (k key column(s))`
* `hash join, built on the right table (N slots)`
* `hash join, built on the N left rows`
* `nested loop`

`X rows in` is how many rows came from the tables joined so far, `Y out` how many the join produced. The parenthesis gives the optimizer's estimate for table `T` after its own conditions.

### `sort: ...` and `order by satisfied by index ...`

`sort: top-k of N rows` is a selection of the first `k` of `N` rows; `sort: all N rows` is a full sort. `order by satisfied by index NAME (ascending|descending), stopping after N rows: no sort` shows an [ordered read](access-paths.md#order-by-with-a-limit).

### `group N rows` and `COUNT(*) answered ...`

`group N rows` is the aggregation step, with ` into S grouping set(s)` for `ROLLUP`, `CUBE` and `GROUPING SETS`. `COUNT(*) answered from the table's row count` is the [shortcut](access-paths.md#count).

### `join T: LATERAL, run N time(s) for M left row(s) ...`

A [LATERAL item](join-order.md#lateral-and-table-functions) was run `N` times for `M` rows from the tables before it (once per distinct set of outer values). The plans of the queries it ran follow, indented.

### `crosstab: ...`

For a [crosstab](crosstab.md): `crosstab: one grouped pass computed C cell(s) for P pivot column(s)` when the one-pass method was used, or `crosstab: P pivot column(s)` for the plain rewrite. The plans of the queries it ran follow, indented.

### `N row(s) returned`

The last line: the number of rows the query itself produced.

## Sub-queries

A sub-query, derived table or CTE is planned when it runs, and its lines appear **indented** under the lines of the query that runs it. A correlated sub-query runs once for every distinct outer value it reads, so its lines can appear many times, unless it was run once as a [semi-join](subqueries.md#semi-joins). After the lines of the inner queries comes a summary:

```
correlated sub-query: semi-join, the inner query ran once and kept 600 row(s) in a hash table; 300 lookup(s) for 300 row(s), 0 separate run(s)
correlated sub-query: ran 5 time(s) for 5 row(s) (once per distinct set of outer values)
```

In a grouped query the rows are the groups.

## Comparing plans

Turn the optimizer off to see the plan the query would have had as written:

```
accessor> .optimizer off
optimizer off
accessor> EXPLAIN SELECT ...
join order: L -> O -> C
scan L [2000 rows]: full scan
...
```

Combine with `.timer on` to see the time as well. Plans differ; answers do not.
