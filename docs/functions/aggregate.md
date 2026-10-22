# Aggregate functions

An aggregate function reduces the rows of a group to one value. Used without `GROUP BY` it reduces the whole query result to a single row (even when there are no rows). Aggregates **skip NULL values**, except `COUNT(*)`; over no non-NULL values they return NULL (`COUNT` returns 0).

```sql
SELECT Dept, COUNT(*), SUM(Sal), AVG(Sal) FROM Emp GROUP BY Dept;
```

Three modifiers apply to all of them:

* `DISTINCT` considers each value once: `COUNT(DISTINCT City)`.
* `FILTER (WHERE condition)` makes the aggregate see only the rows where the condition is true: `COUNT(*) FILTER (WHERE Sal > 100)`.
* `OVER (...)` turns the aggregate into a [window function](window.md#aggregates-over-a-window).

Aggregates cannot be used in `WHERE`; use `HAVING`, or a sub-query.

## Count

* `COUNT(*)` counts rows.
* `COUNT(expr)` counts rows where `expr` is not NULL.
* `COUNT(DISTINCT expr)` counts distinct non-NULL values.

The result is an `INTEGER`.

## Sum

`Sum(expr)` adds the values. The result is exact, with the type of the numbers added: a sum of `CURRENCY` is `CURRENCY`, of `DECIMAL` is `DECIMAL`, and a sum of `INTEGER` that exceeds 64 bits becomes a `BIGNUM` instead of overflowing. Mixed types follow the usual [promotion rules](../sql/expressions.md#operators). Text that looks like a number is converted.

## Avg

`Avg(expr)` is the sum divided by the count of non-NULL values. The sum is exact and the single division is exact when it can be (a `DECIMAL` result), otherwise rounded to 50 digits.

## Min and Max

`Min(expr)` and `Max(expr)` return the smallest and largest value. They work on numbers, text (ignoring case) and dates, and return a value of the same type.

## First, Last and ANY_VALUE

`First(expr)` returns the first non-NULL value met, and `Last(expr)` the last, in the order the rows are read. Without `ORDER BY` that order is not guaranteed. `ANY_VALUE(expr)` is the same as `First`.

## STRING_AGG

`STRING_AGG(expr [, separator])` joins the non-NULL values into one text, in the order the rows are read. The separator defaults to a comma and must be a text constant. `LISTAGG` and `GROUP_CONCAT` are the same function. The result is NULL when there are no values. `WITHIN GROUP (ORDER BY ...)` is not supported.

```sql
SELECT Dept, STRING_AGG(Name, ', ') FROM Emp GROUP BY Dept;
```

## Variance and standard deviation

| Function | Meaning |
| --- | --- |
| `VAR_POP(x)` | population variance |
| `VAR_SAMP(x)` (also `VARIANCE`) | sample variance (divides by *n* − 1); NULL for fewer than 2 values |
| `STDDEV_POP(x)` | population standard deviation |
| `STDDEV_SAMP(x)` (also `STDDEV`) | sample standard deviation; NULL for fewer than 2 values |

They are computed in floating point and return a `DOUBLE`.

## EVERY, BOOL_AND and BOOL_OR

`EVERY(condition)` (also `BOOL_AND`) is true when the condition is true for every row; `BOOL_OR(condition)` is true when it is true for at least one. A NULL condition counts as false. Both are NULL over no rows.

## GROUPING

`GROUPING(expr [, ...])` is used with [`ROLLUP`, `CUBE` and `GROUPING SETS`](../sql/select.md#rollup-cube-and-grouping-sets). Each argument must be one of the `GROUP BY` expressions. For one argument it returns 1 in the rows where that expression was rolled up (shown as NULL), and 0 otherwise. For several it returns a number whose bits are those values, the first argument being the highest bit: `GROUPING(a, b)` is 0, 1, 2 or 3.

```sql
SELECT Region, City, SUM(Sales), GROUPING(Region, City) AS Level
FROM Sales GROUP BY ROLLUP (Region, City);
```
