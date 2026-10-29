# Arithmetic across types

When an operator has operands of different types, the result type is decided **by the types alone, never by the values**. A query therefore cannot change the type of a column's results from one row to the next.

## Which type wins

For `+`, `-`, `*` and comparisons, the operand type that is higher in this list is the type of the result (and the other operand is converted to it):

```
DOUBLE  >  FLOAT  >  RATIONAL  >  CURRENCY  >  DECIMAL  >  BIGNUM  >  INTEGER
```

| Left \ Right | INTEGER | BIGNUM | DECIMAL | CURRENCY | RATIONAL | FLOAT | DOUBLE |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **INTEGER** | INTEGER¹ | BIGNUM | DECIMAL | CURRENCY | RATIONAL | FLOAT | DOUBLE |
| **BIGNUM** | BIGNUM | BIGNUM | DECIMAL | CURRENCY | RATIONAL | FLOAT | DOUBLE |
| **DECIMAL** | DECIMAL | DECIMAL | DECIMAL | CURRENCY | RATIONAL | FLOAT | DOUBLE |
| **CURRENCY** | CURRENCY | CURRENCY | CURRENCY | CURRENCY | RATIONAL | FLOAT | DOUBLE |
| **RATIONAL** | RATIONAL | RATIONAL | RATIONAL | RATIONAL | RATIONAL | FLOAT | DOUBLE |
| **FLOAT** | FLOAT | FLOAT | FLOAT | FLOAT | FLOAT | FLOAT | DOUBLE |
| **DOUBLE** | DOUBLE | DOUBLE | DOUBLE | DOUBLE | DOUBLE | DOUBLE | DOUBLE |

¹ An `INTEGER` result that does not fit in 64 bits becomes a `BIGNUM`.

You can see the type of any expression with `TypeName(expr)`:

```sql
SELECT TypeName(1 + 1.5), TypeName(CCur(1) + 1.5), TypeName(CRat(1) + CFloat(1)), TypeName(CFloat(1) + CDbl(1));
-- DECIMAL, CURRENCY, FLOAT, DOUBLE
```

Exact types stay exact until a float is involved. Mixing an exact number with a `FLOAT` or `DOUBLE` converts the exact one to a float and loses precision, so keep floats out of calculations on money.

## Division

There are three kinds of division, so that each of the results people expect is one operator away.

**`x / y`** drops the fraction when both operands are whole numbers, and divides in floating point otherwise:

| Expression | Result |
| --- | --- |
| `INTEGER` or `BIGNUM` `/` the same | a whole number, truncated toward zero: `7 / 2` is 3, `-7 / 2` is -3, `1 / 3` is 0. With `SET integer_division = off` the quotient is exact, as `DIV` gives it: `7 / 2` is the `DECIMAL` 3.5 (`RESET integer_division` or `SET integer_division = on` for the default). Queries written for Oracle, where `/` never drops the fraction, need it (`hd_dep_count / hd_vehicle_count > 1` in TPC-DS query 73) |
| anything with a `DECIMAL`, `CURRENCY` or `RATIONAL` operand | a `DOUBLE`: `7 / 2.0` is 3.5, `1 / 3.0` is 0.3333333333333333 |
| a `FLOAT` with a `FLOAT` or whole number | a `FLOAT` |
| a `DOUBLE` with anything | a `DOUBLE` |

**`x DIV y`** is exact division, in the exact type the operands call for:

| Expression | Result |
| --- | --- |
| whole number `DIV` whole number | `DECIMAL`, rounded to 50 digits if it does not end: `1 DIV 4` is 0.25, `1 DIV 3` is 0.333… (50 digits) |
| `DECIMAL DIV DECIMAL`, or any mix of the exact types | the same, a `DECIMAL` |
| money `DIV` number | `CURRENCY` |
| money `DIV` money | `DECIMAL` (a ratio is not an amount of money) |
| `RATIONAL DIV x` or `x DIV RATIONAL` | `RATIONAL`, exact |
| float with anything | the float type |

**`x \ y`** is whole-number division after rounding both operands to whole numbers, and `x MOD y` (or `x % y`) is the remainder; both give an `INTEGER` or `BIGNUM`.

**Dividing by zero is NULL**, for all of these (and for dividing an interval, and for `0 ^ -1`). It is not an error, so a column computed as `a / b` is NULL for the rows where `b` is 0, and a condition on it simply does not hold. This is what SQLite and MySQL do; the SQL standard says error. A `FLOAT` or `DOUBLE` result still never becomes infinite or NaN: overflow is an error.

Which one to use: `/` for counts and indexes (`n / 2`) and for approximate measurements; `DIV` for money, percentages and anything where a result such as 0.1 must be exactly 0.1. `Avg` and `Sum` of exact numbers stay exact whatever `/` does.

## Powers

`x ^ n`:

| Operands | Result |
| --- | --- |
| `n` a whole number, `x` any exact type | exact: `2 ^ 100` is a `BIGNUM`, `1.1 ^ 3` is 1.331, `2 ^ -1` is a `DECIMAL` 0.5, `Rational(2,3) ^ 2` is 4/9 |
| `n` a fraction (`x ^ 0.5`) | a `DOUBLE` |
| `x` or `n` a float | a float |

A very large exact power is refused with an error rather than exhausting memory.

## Comparison

Comparisons (`=`, `<`, `BETWEEN`, `IN`), sorting, `GROUP BY`, `DISTINCT` and joins use the **exact value** of each number, whatever its type: `1 = 1.0`, `1 = CDbl(1)` and `1 = CRat(1)` are all true, and an index on an `INTEGER` column can serve a search by a `DECIMAL`. The comparison never converts a decimal to a float: that is why `CDbl(0.1) = 0.1` is false (see [floating-point.md](floating-point.md)).

Text and numbers compare as numbers when the text reads as one: `5 = '5'` is true, `'10' + 1` is 11. Two texts compare as text: `'10' < '9'` is true.

## Aggregates

`Sum` uses the addition rules above (a sum of `INTEGER` can become a `BIGNUM`). `Avg` adds exactly and divides once. `Min` and `Max` return a value of the column's own type.

## Dates, intervals and time zones

| Expression | Result |
| --- | --- |
| `datetime ± number` | a `DATETIME` that many days away |
| `datetime - datetime` | the difference as a number of days |
| `datetime ± interval` | a `DATETIME`; months clamp to the end of a shorter month |
| `timestamptz ± interval` | a `TIMESTAMPTZ`; months and days follow the session zone's wall clock |
| `timestamptz - timestamptz` | an `INTERVAL` of elapsed seconds |
| `interval ± interval`, `interval * n`, `interval / n` | an `INTERVAL` |

See [interval.md](interval.md) and [timestamptz.md](timestamptz.md).

## Dates and yes/no

A `DATETIME` plus or minus a number is a `DATETIME`; two dates subtract to a number of days. `TRUE` is −1 and `FALSE` is 0 in arithmetic. See [datetime.md](datetime.md) and [yesno.md](yesno.md).
