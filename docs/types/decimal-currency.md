# DECIMAL and CURRENCY

Exact decimal numbers of any size: a whole-number digit string with a decimal point somewhere in it. Addition, subtraction and multiplication of decimals are exact; there is no rounding unless you ask for it. These are the right types for prices, rates, measurements and money.

`CURRENCY` is a `DECIMAL` that is shown as money (`$2.50`) and combines with other types in its own way. Both have the same representation and the same range, which is unlimited.

| | `DECIMAL` | `CURRENCY` |
| --- | --- | --- |
| Aliases | `NUMERIC`, `DEC`, `NUMBER` | `MONEY` |
| Display | `19.99` | `$19.99` |
| Stores | exactly what you give it | exactly what you give it (not rounded to 4 places) |

A scale or precision after the name (`NUMERIC(10, 2)`) is accepted and **ignored**: the number of decimal places is not fixed. To round, use `Round(x, 2)` or `CAST(x AS DECIMAL(10, 2))`. To limit the range, add a `CHECK`.

## Literals

A number with a decimal point and no exponent is a `DECIMAL`: `19.99`, `.5`, `10.00`. There is no currency literal; use `CCur(19.99)` or `CAST(19.99 AS CURRENCY)`.

## Stored without trailing zeros

A decimal is stored by value, not by the way it was written. `1.10` and `1.1` are the same number and both display as `1.1`; `10.00` displays as `10`. If you want two decimal places on output, use `Format(x, '0.00')` or `Format(x, 'Fixed')`.

## Arithmetic

* `+`, `-`, `*` are exact.
* `/` is exact when the quotient ends (`1 / 4` is 0.25) and otherwise rounded to **50 digits** (`1 / 3` is 0.333… with fifty threes).
* `^` with a whole exponent is exact: `1.1 ^ 3` is 1.331.
* `Round` rounds halves to the even digit: `Round(0.125, 2)` is 0.12. `CAST(x AS DECIMAL(p, s))` does the same.

Mixing with other types: a `DECIMAL` combined with an `INTEGER` or `BIGNUM` stays `DECIMAL`; with `CURRENCY` it becomes `CURRENCY`; with `RATIONAL`, `FLOAT` or `DOUBLE` it becomes that type. See [arithmetic.md](arithmetic.md).

### Money

`CURRENCY` follows the same rules, with these differences:

| Expression | Type |
| --- | --- |
| money `+ - *` number (`INTEGER`, `BIGNUM`, `DECIMAL`) | `CURRENCY` |
| money `/` number | `CURRENCY` |
| money `/` money | `DECIMAL` (a ratio is not money) |
| money with `RATIONAL` | `RATIONAL` |
| money with `FLOAT` or `DOUBLE` | that float type |

`CCur(x)` rounds to four decimal places, as Access's `CCur` does; storing a value into a `CURRENCY` column does not round.

```sql
SELECT CCur(19.99) * 3;                  -- $59.97
SELECT CCur(10) / 3;                     -- $3.33333… (50 digits)
SELECT CCur(10) / CCur(4);               -- 2.5       (a DECIMAL)
SELECT CCur(1.23456), Round(CCur(1.23456), 2);   -- $1.2346, $1.23
```

## Sums and averages

`Sum` of a column is exact and has the column's type. `Avg` is the exact sum divided once by the count, so it is exact when the result ends and rounded to 50 digits otherwise.

## Conversion

| Value stored | Result |
| --- | --- |
| a whole number | the same number |
| a float | its shortest decimal text: `CDbl(0.1)` becomes 0.1 |
| text that reads as a number (`'12.5'`, `'1e3'`, `'1,500'` is 1500) | the number |
| other text | an error |
| `TRUE` / `FALSE` | −1 / 0 |
| a `RATIONAL` | the exact decimal if the fraction ends, otherwise rounded to 50 digits |

`CDec(x)` / `CAST(x AS DECIMAL)` and `CCur(x)` / `CAST(x AS CURRENCY)` convert in an expression.
