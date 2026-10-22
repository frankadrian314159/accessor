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

| Expression | Result |
| --- | --- |
| whole number `/` whole number | exact `DECIMAL`, rounded to 50 digits if it does not end: `1 / 4` is 0.25, `1 / 3` is 0.333… (50 digits) |
| `DECIMAL / DECIMAL`, or any mix of the exact types | the same, a `DECIMAL` |
| money `/` number | `CURRENCY` |
| money `/` money | `DECIMAL` (a ratio is not an amount of money) |
| `RATIONAL / x` or `x / RATIONAL` | `RATIONAL`, exact |
| float with anything | the float type |
| anything `/ 0` | an error |

`x \ y` is whole-number division and `x MOD y` (or `x % y`) the remainder; both round their operands to whole numbers first and give an `INTEGER` or `BIGNUM`.

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

## Dates and yes/no

A `DATETIME` plus or minus a number is a `DATETIME`; two dates subtract to a number of days. `TRUE` is −1 and `FALSE` is 0 in arithmetic. See [datetime.md](datetime.md) and [yesno.md](yesno.md).
