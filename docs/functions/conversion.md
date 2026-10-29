# Conversion functions

## CAST

```sql
CAST(expression AS type)
```

Converts a value to a [data type](../sql/ddl.md#data-types): `CAST('12' AS INTEGER)`, `CAST(Total AS TEXT)`, `CAST(x AS DECIMAL(10, 2))`. Sizes are ignored, except that the scale of a `DECIMAL` or `CURRENCY` rounds the result (halves to the even digit). A NULL stays NULL. `CAST(x AS INTEGER)` and `CAST(x AS BIGNUM)` drop the fraction of a number toward zero (`CAST(2.7 AS INTEGER)` is 2; the function `CInt(2.7)` rounds to 3). `SET cast_truncates = off` makes them round instead, halves to the even digit, as PostgreSQL's casts do: `CAST(2.7 AS INTEGER)` is 3 and `CAST(-1.5 AS INTEGER)` is -2 (`on` is the default). A value that cannot be converted (`CAST('abc' AS INTEGER)`) is an error.

`CAST` is the same as the conversion function for the type, listed next.

## Conversion functions

| Function | Converts to | Notes |
| --- | --- | --- |
| `CInt(x)` (also `CLng`) | `INTEGER` | text and fractions are rounded to a whole number, halves to even; an out-of-range number is an error |
| `CBig(x)` | `BIGNUM` | rounded the same way; any size |
| `CSng(x)` (also `CFloat`) | `FLOAT` | 32-bit floating point |
| `CDbl(x)` | `DOUBLE` | 64-bit floating point |
| `CDec(x)` | `DECIMAL` | exact; a float gives its shortest decimal text, so `CDec(CDbl(0.1))` is 0.1 |
| `CCur(x)` | `CURRENCY` | rounded to four places, as Access does |
| `CRat(x)` | `RATIONAL` | an exact fraction; `CRat(0.75)` is 3/4 |
| `CStr(x)` | `TEXT` | the text form of any value |
| `CBool(x)` | `YESNO` | a number is true unless it is zero; text such as `'yes'`, `'true'`, `'no'`, `'false'` |
| `CTimestampTz(x)` | `TIMESTAMPTZ` | a moment, read in the session time zone if the text has no offset |
| `CInterval(x)` | `INTERVAL` | `3 days`, `1 year 2 months`, `04:05:06` |
| `CDate(x)` | `DATETIME` | text in the date formats of the [date page](datetime.md), or a day number |

These conversions are exact where they can be: `CDec(5)` is `5`, `CBig('123456789012345678901234567890')` keeps every digit. Converting an exact number to `FLOAT` or `DOUBLE` can lose precision; the other direction never invents digits.

## Val

`Val(text)` reads the number at the start of the text and ignores the rest, giving a `DECIMAL`; text that does not start with a number gives 0. It never fails.

```sql
SELECT Val('12.5 kg'), Val('abc'), Val('  -3x');    -- 12.5, 0, -3
```

## Rational, Numerator and Denominator

`Rational(n, d)` builds the exact fraction *n*/*d* in lowest terms; *d* must not be zero. `Numerator(x)` and `Denominator(x)` return the two parts of any exact number as `BIGNUM`s (the denominator of a whole number is 1).

```sql
SELECT Rational(6, 8);                       -- 3/4
SELECT Numerator(Rational(6, 8)), Denominator(0.75);   -- 3, 4
```

A `FLOAT` or `DOUBLE` is taken at its exact binary value: `Numerator(CDbl(0.5))` is 1, and `Denominator(CDbl(0.1))` is 36028797018963968 (2^55), because 0.1 has no exact binary form.

## TypeName

`TypeName(x)` returns the name of the type of a value as text: `INTEGER`, `BIGNUM`, `FLOAT`, `DOUBLE`, `DECIMAL`, `CURRENCY`, `RATIONAL`, `TEXT`, `YESNO` or `DATETIME`. It is useful for seeing how the numeric types combine.

```sql
SELECT TypeName(1 / 3), TypeName(2 ^ 100), TypeName(1 + 1.5);   -- DECIMAL, BIGNUM, DECIMAL
```
