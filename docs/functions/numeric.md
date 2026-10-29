# Numeric functions

These functions work on all the numeric types (`INTEGER`, `BIGNUM`, `FLOAT`, `DOUBLE`, `DECIMAL`, `CURRENCY`, `RATIONAL`). Where it makes sense the result has the type of the argument, so that exact numbers stay exact. Text and dates that look like numbers are converted first.

Functions computed in floating point (square roots, logarithms, powers with fractional exponents, trigonometry) give a `DOUBLE`, or a `FLOAT` when the argument was a `FLOAT`. They are as precise as an `f64`.

## Abs

`Abs(x)` returns the absolute value, in the type of `x`. The absolute value of the smallest `INTEGER` becomes a `BIGNUM` rather than overflowing.

```sql
SELECT Abs(-2.50);                    -- 2.5
```

## Round

`Round(x [, digits])` rounds to `digits` places after the point (default 0; `digits` can be 0 to 10000). **Halves round to the even digit** ("banker's rounding"), so `Round(2.5)` is 2 and `Round(3.5)` is 4. (`SET round_half_even = off` sends a half away from zero instead, as most servers do: `Round(2.125, 2)` is 2.13 and `Round(2.5)` is 3; the default is `on`.) The result keeps the type of `x`. A float is rounded by its shortest decimal form, so `Round(2.675, 2)` is 2.68 whatever the type.

```sql
SELECT Round(3.14159, 2);             -- 3.14
SELECT Round(2.5), Round(3.5);        -- 2, 4
SELECT Round(CCur(19.995), 2);         -- $20.00 (exactly, no float error)
```

## Int, Fix and Floor

* `Int(x)` (also `FLOOR`) returns the largest whole number not above `x`: `Int(-2.1)` is -3.
* `Fix(x)` removes the fraction, rounding toward zero: `Fix(-2.1)` is -2.

Whole numbers are returned unchanged; for fractions the result has the same type as the argument (a `DECIMAL` stays a `DECIMAL`), except that a `RATIONAL` gives a `BIGNUM`.

## Ceil

`Ceil(x)` (also `CEILING`) returns the smallest whole number not below `x`: `Ceil(2.1)` is 3, `Ceil(-2.1)` is -2.

## Sgn

`Sgn(x)` (also `SIGN`) returns -1, 0 or 1 as `x` is negative, zero or positive.

## Sqr

`Sqr(x)` (also `SQRT`) returns the square root as a floating-point number. It is an error for `x` to be negative. Even for a perfect square the result is a `DOUBLE` (`Sqr(16)` is 4 as a `DOUBLE`).

## Exp and Log

* `Exp(x)` is *e* raised to `x`.
* `Log(x)` (also `LN`) is the **natural** logarithm. It is an error for `x` to be zero or negative.
* `Log10(x)` is the base-10 logarithm, with the same restriction.

```sql
SELECT Log(Exp(1)), Log10(1000);      -- 1, 3
```

## Mod and Power

`Mod(a, b)` is the remainder of `a` divided by `b`, the same as `a MOD b` and `a % b`. The result has the sign of `a`. `b` must not be zero. `a \ b` is the matching whole-number division.

`Power(a, b)` (also `POW`) is `a ^ b`. When `b` is a whole number the result is exact (so `Power(2, 100)` is a `BIGNUM` and `Power(1.1, 3)` a `DECIMAL`); otherwise it is a `DOUBLE`.

```sql
SELECT Mod(17, 5), 17 MOD 5, 17 \ 5;       -- 2, 2, 3
SELECT Power(2, 100);                      -- 1267650600228229401496703205376
```

## Trigonometric and hyperbolic functions

`Sin`, `Cos`, `Tan`, `Asin`, `Acos`, `Atan`, `Sinh`, `Cosh`, `Tanh` take and return radians as floating-point numbers. `Asin` and `Acos` of a value outside −1 to 1 is an error.

`Atan2(y, x)` is the angle in radians of the point (x, y), in the right quadrant. `Degrees(x)` and `Radians(x)` convert between the two units, and `Pi()` is π (a `DOUBLE`).

## Trunc

`TRUNC(x [, digits])` (also `TRUNCATE`) cuts the number off after `digits` places **without rounding** (`Round` rounds), keeping its type. A negative `digits` cuts off digits to the left of the point. Exact types stay exact.

```sql
SELECT TRUNC(-123.4567, 2), TRUNC(123.999), TRUNC(1234, -2);      -- -123.45, 123, 1200
```

## Random

`RANDOM()` (also `RAND`, `RND`) is a `DOUBLE` from 0 up to, but not including, 1, a new one **for each row** of a query. It is not meant for cryptography. Because it changes from call to call, the optimizer never folds it into a constant. For a whole number from 1 to 6: `1 + Int(Random() * 6)`.

## Operators

The arithmetic operators `+ - * / \ % MOD ^` and unary minus are described in [expressions](../sql/expressions.md#operators).
