# RATIONAL

An exact fraction: a `BIGNUM` numerator over a positive `BIGNUM` denominator, always in lowest terms. Use it when a value is a ratio that decimals cannot hold, such as one third, and the answer must stay exact through a calculation.

```sql
SELECT Rational(1, 3) + Rational(1, 6);       -- 1/2
SELECT Rational(1, 3) * 3;                    -- 1
SELECT CRat(0.75);                            -- 3/4
```

There is no rational literal. Build one with `Rational(numerator, denominator)` or convert with `CRat(x)` / `CAST(x AS RATIONAL)`.

## Display

A fraction is shown as `n/d` (`3/4`, `-1/3`); a whole number as just `n` (`Rational(4, 2)` shows `2`). Storing a decimal such as `0.333` gives `333/1000`, because that is exactly what 0.333 is.

## Arithmetic

`+`, `-`, `*` and `/` on rationals are exact and the result is a `RATIONAL`, reduced to lowest terms. Dividing by zero is an error. Mixing with other types:

| With | Result |
| --- | --- |
| `INTEGER`, `BIGNUM`, `DECIMAL` | `RATIONAL` (the other number is taken exactly) |
| `CURRENCY` | `RATIONAL` |
| `FLOAT`, `DOUBLE` | the float type: the exactness is lost |

`^` with a whole exponent is exact. `Numerator(x)` and `Denominator(x)` return the two parts.

## Comparison with decimals

Because the fraction is exact, it is equal to a decimal only if the decimal is exactly equal. `Rational(1, 3)` does not equal `CDec(1) / 3`, which is one third rounded to 50 digits:

```sql
SELECT Rational(1, 3) = CDec(1) / 3;          -- No
SELECT Rational(1, 4) = 0.25;                 -- Yes
```

## Conversion

| Value stored | Result |
| --- | --- |
| a whole number | `n/1` |
| a decimal | the exact fraction of its digits |
| a float | the fraction of its shortest decimal text (`0.1` becomes `1/10`) |
| text that reads as a number or as `n/d` | the fraction |
| other text | an error |

Converting a `RATIONAL` to a `DECIMAL` gives the exact decimal when the fraction terminates (`3/4` is 0.75) and a 50-digit rounding otherwise.
