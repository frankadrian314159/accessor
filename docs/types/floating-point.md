# FLOAT and DOUBLE

The two binary floating-point types. They are fast and cover an enormous range, but most decimal fractions (0.1, 0.01) cannot be stored in them exactly. For money, prices and anything that must add up, use [`DECIMAL` or `CURRENCY`](decimal-currency.md) instead.

| | `FLOAT` | `DOUBLE` |
| --- | --- | --- |
| Size | 32 bits (IEEE 754 single) | 64 bits (IEEE 754 double) |
| Aliases | `REAL`, `SINGLE` | `DOUBLE PRECISION` |
| Precision | about 7 significant digits | about 15–16 significant digits |
| Largest value | about 3.4 × 10³⁸ | about 1.8 × 10³⁰⁸ |
| Smallest positive | about 1.2 × 10⁻³⁸ | about 2.2 × 10⁻³⁰⁸ |

## Literals

A number written with an exponent is a `DOUBLE`: `1e3`, `2.5E-4`. A number with a point and no exponent (`2.5`) is an exact `DECIMAL`, not a float. Use `CDbl(2.5)` or `CFloat(2.5)` to convert.

## Never NaN, never infinite

ASQL does not store `NaN` or infinity. A calculation that would produce one is an error instead (`Overflow`, or `Asin: the result is not a number`), and a value too big for a `FLOAT` is refused when stored (`... is out of range for FLOAT`). So such values can never end up in a table or in an index.

## Exact comparison

Floats are compared with other numbers by their exact binary value. The `DOUBLE` nearest to 0.1 is slightly more than one tenth, so:

```sql
SELECT CDbl(0.1) = 0.1;                         -- No
SELECT CDbl(0.5) = 0.5;                         -- Yes (0.5 is exactly representable)
SELECT CDbl(0.1) + CDbl(0.2) = CDbl(0.3);       -- No
SELECT 0.1 + 0.2 = 0.3;                         -- Yes (DECIMAL arithmetic is exact)
```

If you want to compare a float with a decimal as a person would, convert the float with `CDec`, which uses the float's shortest decimal text: `CDec(CDbl(0.1)) = 0.1` is true.

## Display

A float is shown with the shortest text that reads back as the same number: `0.1`, `1.4142135623730951`, `0.33333334` for a `FLOAT` one third. Exponents are not used for ordinary magnitudes: `1e15` shows as `1000000000000000`, `1e-7` as `0.0000001`.

## Arithmetic

* Any operation with a `DOUBLE` gives a `DOUBLE`; with a `FLOAT` and no `DOUBLE`, a `FLOAT`. See [arithmetic.md](arithmetic.md).
* Dividing by zero is an error, as are the other undefined operations.
* `Sqr`, `Exp`, `Log`, `Log10`, the trigonometric functions, and powers with a fractional exponent are computed in floating point and give a `DOUBLE` (a `FLOAT` for a `FLOAT` argument).
* `Round` rounds a float by its shortest decimal text, so `Round(2.675, 2)` is 2.68 for a float as it is for a decimal.

## Conversion

| Value stored | Result |
| --- | --- |
| a number | the nearest float; a `FLOAT` loses digits beyond about 7 (`CFloat(16777217)` is 16777216) |
| text that reads as a number | the nearest float |
| other text | an error |
| `TRUE` / `FALSE` | −1 / 0 |
| too big for the type | an error |

`CSng(x)` / `CFloat(x)` / `CAST(x AS FLOAT)` and `CDbl(x)` / `CAST(x AS DOUBLE)` convert in an expression.
