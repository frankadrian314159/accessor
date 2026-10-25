# Sub-queries, semi-joins and derived tables

Sub-queries are where a query can quietly turn into thousands of queries. The optimizer has three tools for them.

## Uncorrelated sub-queries run once

A sub-query that does not read the query around it is run once, before the outer query, and its answer is used as a constant (a scalar), a set (`IN`), or a yes/no (`EXISTS`). An `IN (SELECT ...)` becomes a hash-set membership test.

## Correlated sub-queries: one run per distinct key, or one run in all

A sub-query that reads columns of the outer query is *correlated*. The straightforward way to run it is once for every **distinct** combination of the outer values it reads, remembering the answers, so a thousand customers living in ten cities, correlated on city, give ten runs. When all the correlation is through equalities, the optimizer can do better.

### Semi-joins

```sql
SELECT Name FROM Customers C
WHERE EXISTS (SELECT 1 FROM Orders O WHERE O.CustID = C.ID AND O.Total > 100);
```

The correlation here is the single equality `O.CustID = C.ID`. Instead of asking the orders table "any big orders for customer 17?" once for each customer, the optimizer runs

```sql
SELECT O.CustID FROM Orders O WHERE O.Total > 100
```

**once**, keeps the customer ids it returns in a hash table, and gives each customer its answer by a lookup. This is what databases call a *semi-join*, and, for `NOT EXISTS`, an *anti-join*. It is used for

* `EXISTS` and `NOT EXISTS`;
* `x IN (SELECT ...)` and `NOT IN`, with the correlation in the `WHERE`;
* scalar sub-queries that return one value per key, such as `(SELECT Name FROM Customers WHERE ID = O.CustID)`;
* scalar sub-queries that are a single aggregate, such as `(SELECT MAX(Total) FROM Orders O WHERE O.CustID = C.ID)` or `(SELECT COUNT(*) ...)`. These are run once with a `GROUP BY` on the key. A key with no rows gives `0` for `COUNT` and NULL for the other aggregates, as the one-at-a-time run would.

The correlation may be several equalities (`O.A = C.A AND O.B = C.B`); the inner side of each may be any expression that reads only the sub-query's own tables. The sub-query may have other conditions, joins and sub-queries of its own.

It is **not** used (the one-run-per-key method is kept) when

* a correlated condition is not an equality (`O.Total > C.Credit`), or an outer column is used anywhere but in such an equality (in the select list, a join condition, a nested sub-query);
* the sub-query has `DISTINCT`, `GROUP BY`, `HAVING`, `LIMIT`, `TOP`, a window function, a set operator, or an aggregate mixed into a larger expression;
* an equality compares values of different kinds, such as a number with text. The two are compared by converting, which a hash lookup cannot do, so those rows use the one-at-a-time method;
* it would not pay.

### When it pays

Running once costs one pass over the inner rows that survive the sub-query's other conditions. Running per key costs, per distinct key, a lookup if the inner column has an index, or a scan of the whole inner table if it has not. So:

| Inner table | Hash once when |
| --- | --- |
| one table, **no index** on the correlated column | at least 2 distinct outer keys |
| one table, indexed | the number of distinct outer keys times 50 is at least the table's row count |
| joins or other sources | at least 32 distinct outer keys |

Measured with 5,000 customers and 30,000 orders (release build):

| Sub-query | No index on `Orders.CustID` | Index on `Orders.CustID` |
| --- | --- | --- |
| `EXISTS (... WHERE o.CustID = c.ID AND o.Total > 90)`, hashed | 8 ms | 9 ms |
| the same, one run per key | 11.8 s | 28 ms |
| `(SELECT MAX(o.Total) ... WHERE o.CustID = c.ID)`, hashed | 30 ms | 30 ms |
| the same, one run per key | 9.5 s | 32 ms |

Without an index, the difference is a factor of hundreds; with one, a small factor, or none. That is the other reason to index the columns that sub-queries are correlated on.

`EXPLAIN` shows which was used:

```
correlated sub-query: semi-join, the inner query ran once and kept 600 row(s) in a hash table; 300 lookup(s) for 300 row(s), 0 separate run(s)
correlated sub-query: ran 5 time(s) for 5 row(s) (once per distinct set of outer values)
```

With the optimizer off the one-run-per-key method is always used; the answers are identical.

## Correlated sub-queries in grouped queries

In a query with `GROUP BY`, a correlated sub-query in the select list, `HAVING` or `ORDER BY` reads the **group**: it may use the grouping columns, and it runs once per group (not once per row), after the aggregation:

```sql
SELECT O.CustID,
       (SELECT C.Name FROM Customers C WHERE C.ID = O.CustID) AS Who,
       SUM(O.Total)
FROM Orders O
GROUP BY O.CustID
HAVING EXISTS (SELECT 1 FROM Customers C WHERE C.ID = O.CustID AND C.City = 'London');
```

Here the sub-queries run for each customer's group, however many orders it has. A sub-query in `WHERE` or in a `GROUP BY` expression itself sees each row, as before. A sub-query that uses a column that is neither grouped nor aggregated is an error. The semi-join method applies to these as well.

## Derived tables and views read as their table

`FROM (SELECT ...) AS d` is normally computed in full and then filtered. A derived table (or a view) that is a plain selection from one table is *merged* instead: read as the table itself, with its own condition applied as part of the scan, so the outer query's conditions can use the table's indexes.

```sql
SELECT Name FROM (SELECT ID, Name FROM Customers WHERE City = 'Rome') d WHERE d.ID = 7;
-- scan d [..]: index PrimaryKey on ID = (7)
```

It must read exactly one table (not a join, a view or a nested derived table), show plain columns of it, **in the table's own order** (they may be renamed, also by a column list `AS d (k, n)`), and have no `DISTINCT`, grouping, aggregate, `LIMIT`, window function or set operator. The columns it does not show are unreachable from outside, as before. A derived table that computes a column, reorders columns, or limits rows is computed first. Merging is not done for the preserved side of a `RIGHT` or `FULL` join, nor for `LATERAL` items. See also [views.md](views.md).
