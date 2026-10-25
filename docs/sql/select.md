# SELECT and other queries

A query reads data and returns a result set. Its general shape is:

```sql
[ WITH [RECURSIVE] cte [, ...] ]
SELECT [DISTINCT | ALL] [TOP n] select-list
[ FROM from-clause ]
[ WHERE condition ]
[ GROUP BY group-elements ]
[ HAVING condition ]
[ { UNION | INTERSECT | EXCEPT } [ALL] select ... ]
[ ORDER BY sort-list ]
[ LIMIT n ] [ OFFSET m [ROWS] ] [ FETCH {FIRST | NEXT} [n] {ROW | ROWS} ONLY ]
```

The full grammar is in [grammar.md](grammar.md).

## The select list

```sql
SELECT *                      FROM Customers;      -- every column
SELECT C.*                    FROM Customers C;    -- every column of one table
SELECT Name, Price * 1.2 AS WithTax FROM Product;  -- expressions, with an alias
```

An alias follows `AS` (which may be left out). A column with no alias takes the column's own name; an aggregate gets a name such as `SumOfSal`; any other expression is named `Expr1000`, `Expr1001`, and so on.

`SELECT DISTINCT` removes duplicate rows. `SELECT ALL` (the default) keeps them. Two rows are duplicates when every column is equal; NULLs count as equal to each other here.

`SELECT TOP n` is the Access spelling of a row limit; see [Limiting rows](#limiting-rows).

A `SELECT` without `FROM` returns one row: `SELECT 1 + 2`.

## FROM and joins

```sql
FROM Customers C
FROM Customers AS C
FROM (SELECT ... ) AS D                 -- a derived table
FROM (SELECT ...) AS D (a, b)           -- with names for its columns
FROM (VALUES (1, 'a'), (2, 'b')) AS V (n, s)
```

Each source can be a table, a saved query (view), or a parenthesised query. A derived table needs an alias.

### Join types

| Join | Rows returned |
| --- | --- |
| `a INNER JOIN b ON cond` (or just `JOIN`) | pairs that satisfy the condition |
| `a LEFT [OUTER] JOIN b ON cond` | every row of `a`, with NULLs where `b` has no match |
| `a RIGHT [OUTER] JOIN b ON cond` | every row of `b`, with NULLs where `a` has no match |
| `a FULL [OUTER] JOIN b ON cond` | every row of both, matched where possible |
| `a CROSS JOIN b` or `a, b` | every pair |

Joins can be nested with parentheses, which is how Access writes a join of more than two tables:

```sql
FROM ((Customers C INNER JOIN Orders O ON O.CustID = C.ID)
      INNER JOIN Lines L ON L.OrderID = O.ID)
```

### USING and NATURAL

```sql
FROM a JOIN b USING (Dept, Year)     -- equality on the listed columns
FROM a NATURAL JOIN b                -- equality on every column the two have in common
```

The joined column appears once in `SELECT *`, and can be written without a qualifier. For `LEFT` and `INNER` joins the left copy is the one shown; for `RIGHT` joins, the right copy. `FULL JOIN` cannot use `USING` or `NATURAL` (write the `ON` condition). A `NATURAL` join with no common columns is a cross join.

### How a join runs

The [optimizer](../optimizer/README.md) filters each table by its own conditions first, puts inner joins in the cheapest order, and picks a method per join: an index lookup in the joined table when the join columns are indexed, otherwise a hash join for equality conditions, otherwise a nested loop. Outer joins are done in the order written. Use `EXPLAIN` to see what was chosen.

## WHERE

The condition keeps the rows for which it is true. A row where it is false or NULL is dropped. See [expressions.md](expressions.md) for the operators and predicates.

Indexes are used for conditions that compare indexed columns with constants (`=`, `<`, `<=`, `>`, `>=`, `BETWEEN`).

## GROUP BY, HAVING and aggregates

```sql
SELECT Dept, COUNT(*) AS N, AVG(Sal) AS Mean
FROM Emp
WHERE Sal > 0
GROUP BY Dept
HAVING COUNT(*) > 1
ORDER BY Mean DESC;
```

Every column in the select list must be a `GROUP BY` expression or be inside an aggregate. An aggregate query with no `GROUP BY` forms one group, and returns exactly one row even over no input. The aggregate functions are listed in [../functions/aggregate.md](../functions/aggregate.md).

### ROLLUP, CUBE and GROUPING SETS

```sql
GROUP BY ROLLUP (Region, City)                -- (Region, City), (Region), ()
GROUP BY CUBE (Region, City)                  -- every subset
GROUP BY GROUPING SETS ((Region), (City), ()) -- exactly these
GROUP BY Year, ROLLUP (Region)                -- plain and special elements combine
```

In the rows for a coarser grouping, the columns that were rolled up are NULL. `GROUPING(column, ...)` tells those NULLs from real ones: it returns 1 for a rolled-up column and 0 otherwise (several arguments give a bit pattern, the first being the most significant).

```sql
SELECT Region, SUM(Sales), GROUPING(Region) FROM T GROUP BY ROLLUP (Region);
```

## Set operators

| Operator | Result |
| --- | --- |
| `a UNION b` | rows in either, duplicates removed |
| `a UNION ALL b` | rows in either, all kept |
| `a INTERSECT b` | rows in both, duplicates removed |
| `a INTERSECT ALL b` | rows in both, as many times as the smaller count |
| `a EXCEPT b` (or `MINUS`) | rows of `a` not in `b`, duplicates removed |
| `a EXCEPT ALL b` | rows of `a` after taking away `b`'s rows one for one |

All the queries must return the same number of columns. The result's column names come from the first query. Values are compared by value (`1`, `1.0` and `1e0` are the same). `INTERSECT` binds more tightly than `UNION` and `EXCEPT`; the others go left to right. Parentheses group:

```sql
(SELECT a FROM x UNION SELECT a FROM y) EXCEPT SELECT a FROM z
```

An `ORDER BY` after a set operator applies to the combined rows, and may name only result columns or give their positions: `ORDER BY 1 DESC`.

## Common table expressions (WITH)

```sql
WITH big AS (SELECT * FROM Orders WHERE Total > 100),
     per AS (SELECT CustID, COUNT(*) AS N FROM big GROUP BY CustID)
SELECT * FROM per WHERE N > 1;
```

Each CTE is evaluated once, in order, and may use the ones before it. The name can be used wherever a table can, including in sub-queries. A column list renames the CTE's columns: `WITH t (a, b) AS (...)`.

### Recursive CTEs

```sql
WITH RECURSIVE n (i) AS (
    SELECT 1                          -- the starting part
    UNION ALL
    SELECT i + 1 FROM n WHERE i < 10  -- the recursive part
)
SELECT SUM(i) FROM n;
```

The parts that do not mention the CTE start it; the parts that do are run again on the rows the previous round produced, until a round adds nothing. With `UNION` (not `ALL`) rows already produced are not added again, so a graph walk over a cyclic graph stops. The parts may be joined only with `UNION` / `UNION ALL`, and the CTE query cannot have its own `ORDER BY` or `LIMIT`. A recursion is stopped with an error after 100,000 rounds or 10,000,000 rows.

## VALUES

`VALUES (1, 'a'), (2, 'b')` is a query returning those rows, with columns named `column1`, `column2`, and so on. It can stand wherever a query can: as a derived table, in a CTE, after `INSERT`, or as the right-hand side of `IN`.

## Sub-queries

| Form | Meaning |
| --- | --- |
| `(SELECT ...)` as a value | a scalar sub-query: one column, at most one row (none gives NULL) |
| `x IN (SELECT ...)`, `x NOT IN (...)` | membership, with SQL's NULL rules |
| `EXISTS (SELECT ...)`, `NOT EXISTS (...)` | whether there are any rows |
| `x op ANY / SOME / ALL (SELECT ...)` | a quantified comparison |
| `FROM (SELECT ...) AS d` | a derived table |

### Correlated sub-queries

A sub-query may use columns of the query around it:

```sql
SELECT Name,
       (SELECT SUM(Total) FROM Orders O WHERE O.CustID = C.ID) AS Spent
FROM Customers C
WHERE EXISTS (SELECT 1 FROM Orders O WHERE O.CustID = C.ID AND O.Total > 100);
```

A name inside the sub-query refers to its own tables first, and to the outer query only if the sub-query has no column of that name. Correlated sub-queries can nest to any depth. Each one is run once for every distinct combination of outer values it reads, so the cost is that of the inner query times the number of distinct outer keys.

Correlated sub-queries are allowed in the select list, `WHERE` and `ORDER BY` of a query without `GROUP BY`, and in the `WHERE` and `SET` of `UPDATE` and `DELETE`. They are not allowed in `GROUP BY` queries, `HAVING`, join `ON` conditions, `INSERT ... VALUES` or calculated-field formulas; the error message says so.

### Quantified comparisons

`x > ALL (subquery)` is true when `x` is greater than every value, and true over an empty set. `x > ANY (subquery)` is true when it is greater than at least one, and false over an empty set. `= ANY` is the same as `IN`, and `<> ALL` the same as `NOT IN`. The other combinations are computed with the smallest and largest value of the sub-query, which ignores NULLs; SQL's rule that a NULL in the set makes the answer unknown is not applied. The sub-query must return one column.

## Window functions

A window function computes a value for each row from a set of related rows, without collapsing them:

```sql
SELECT Name, Dept, Sal,
       RANK()    OVER (PARTITION BY Dept ORDER BY Sal DESC) AS Rank,
       SUM(Sal)  OVER (PARTITION BY Dept)                    AS DeptTotal,
       AVG(Sal)  OVER (ORDER BY ID ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) AS Moving
FROM Emp;
```

`OVER (...)` has three optional parts: `PARTITION BY` splits the rows into independent groups; `ORDER BY` orders the rows within each group; and a frame (`ROWS` or `RANGE`) chooses which rows around the current one an aggregate uses. The functions are listed in [../functions/window.md](../functions/window.md).

Window functions may appear in the select list and in `ORDER BY`, not in `WHERE`, `GROUP BY`, `HAVING` or join conditions. In a grouped query they run after grouping, so they can use aggregates: `RANK() OVER (ORDER BY SUM(Sal) DESC)`.

Not supported: named windows (`WINDOW w AS (...)` and `OVER w`), `GROUPS` frames, `EXCLUDE` clauses, and `RANGE` with a numeric offset (use `ROWS`).

## Crosstab queries (TRANSFORM … PIVOT)

A crosstab turns the distinct values of one column into columns of the result, the Access way of laying out a summary. The form is

```sql
TRANSFORM aggregate
SELECT row-headings [, other aggregates]
FROM ...
[WHERE ...]
GROUP BY row-headings
[HAVING ...] [ORDER BY ...]
PIVOT pivot-column [IN (value, value, ...)];
```

`TRANSFORM` names the aggregate that goes in each cell. The `SELECT` and `GROUP BY` give the row headings, as for any grouped query. `PIVOT` names the column (or expression) whose values become the new columns.

```sql
CREATE TABLE Sales (Region TEXT, Quarter INTEGER, Amount CURRENCY);
-- ... rows ...

TRANSFORM Sum(Amount)
SELECT Region, Sum(Amount) AS Total
FROM Sales
GROUP BY Region
ORDER BY Region
PIVOT Quarter;
```

```
Region │ Total  │ <>    │ 1      │ 2      │ 3     │ 4
───────┼────────┼───────┼────────┼────────┼───────┼──────
East   │ $35.00 │ NULL  │ $10.00 │ $25.00 │ NULL  │ NULL
North  │ $10.00 │ $1.00 │ NULL   │ NULL   │ $9.00 │ NULL
West   │ $10.00 │ NULL  │ $7.00  │ NULL   │ NULL  │ $3.00
```

How the result is laid out:

* The columns are the `SELECT` list (row headings and any other aggregates, such as `Total` above) followed by one column per pivot value.
* Without an `IN` list the pivot columns are every distinct value that occurs in the rows selected by `FROM` and `WHERE`, in ascending order. A NULL pivot value gets a column called `<>`, first. A column's heading is the value as text.
* With `IN (…)` the columns are exactly the listed values, in the order given, even if no row has them; rows with other values are not in any pivot column (they still count in the other aggregates in the `SELECT` list).
* A cell where no row has that heading and value is NULL, except for `Count`, which gives 0.
* The aggregate can be any [aggregate function](../functions/aggregate.md), including `Count(*)`, `Count(DISTINCT x)`, `Avg`, `Min`, `Max`, with the usual NULL rules. `AS name` after the `TRANSFORM` aggregate is accepted and has no effect.
* `ORDER BY`, `TOP`, `LIMIT` and `HAVING` apply to the rows (the headings), not to the columns.

The pivot may be an expression: `PIVOT 'Q' & Quarter` makes columns called `Q1`, `Q2`, …, and `PIVOT Year(Placed)` one column per year.

A crosstab can be saved: `CREATE QUERY SalesByQuarter AS TRANSFORM … PIVOT …;`, then `SELECT * FROM SalesByQuarter` (note that the columns of a saved crosstab depend on the data at the time it runs). It can have parameters (`@year`), and it works inside `EXPLAIN`.

`PIVOT` is a reserved word. A crosstab cannot be used as a sub-query or as a derived table directly, but a saved crosstab can be selected from.

How crosstabs are executed, and how the optimizer speeds them up, is described in [../optimizer/crosstab.md](../optimizer/crosstab.md).

## ORDER BY

```sql
ORDER BY Dept, Sal DESC            -- columns or expressions
ORDER BY 2 DESC                    -- a position in the select list
ORDER BY Mean                      -- an alias from the select list
```

NULLs sort before every other value in ascending order, and after them in descending order. Numbers compare by value across all numeric types, text compares case-insensitively.

## Limiting rows

Four spellings do the same job; choose the one you like:

```sql
SELECT TOP 10 * FROM T ORDER BY X;                 -- Access
SELECT * FROM T ORDER BY X LIMIT 10 OFFSET 20;     -- common
SELECT * FROM T ORDER BY X OFFSET 20 ROWS FETCH FIRST 10 ROWS ONLY;   -- standard
SELECT * FROM T ORDER BY X FETCH NEXT ROW ONLY;    -- one row
```

`OFFSET` skips rows before the limit is applied. Without `ORDER BY` the rows are those the engine finds first, which is not a guaranteed order. `FETCH ... WITH TIES` is not supported.
