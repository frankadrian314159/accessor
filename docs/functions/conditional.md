# Conditional and NULL-handling functions

## CASE

`CASE` is an expression, described in [expressions](../sql/expressions.md#case):

```sql
CASE WHEN Sal > 100 THEN 'high' ELSE 'low' END
CASE Dept WHEN 'Eng' THEN 1 WHEN 'Ops' THEN 2 END
```

## IIf

`IIf(condition, if_true, if_false)` returns one of two values. Only the chosen one is evaluated, so `IIf(d = 0, 0, n / d)` does not divide by zero. When the condition is NULL (unknown) the third argument is returned.

## Coalesce

`Coalesce(a, b, c, ...)` returns the first argument that is not NULL, or NULL when all are. Later arguments are not evaluated once one is found.

## Nz

`Nz(value [, if_null])` returns `value`, or `if_null` when it is NULL. Without `if_null` it is 0. (`Nz(a, b)` is `Coalesce(a, b)`.)

## IsNull

`IsNull(value)` is true when the value is NULL. The predicate `value IS NULL` does the same.

## NullIf

`NullIf(a, b)` returns NULL when `a = b`, and otherwise `a`. It is the usual way to avoid dividing by zero: `x / NullIf(y, 0)`.

## Greatest and Least

`Greatest(a, b, ...)` and `Least(a, b, ...)` return the largest and smallest argument. All the arguments must be comparable with each other. **If any argument is NULL, the result is NULL.**

```sql
SELECT Greatest(2, 9, 4), Least('b', 'a', 'c');    -- 9, 'a'
```
