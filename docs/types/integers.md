# INTEGER and BIGNUM

Both hold whole numbers. `INTEGER` is a fixed 64-bit signed number and is the type to use almost always. `BIGNUM` has no size limit.

| | `INTEGER` | `BIGNUM` |
| --- | --- | --- |
| Range | −9,223,372,036,854,775,808 to 9,223,372,036,854,775,807 | unlimited (memory only) |
| Aliases | `INT`, `SMALLINT`, `TINYINT`, `BIGINT`, `LONG`, `SHORT`, `BYTE` | none |
| Speed | fastest | slower, grows with the size |
| Use for | counts, quantities, identifiers | factorials, exact products, very large keys |

`SMALLINT`, `TINYINT`, `BYTE` and `BIGINT` are all the same 64-bit `INTEGER`; the width in the name is not enforced. Add a `CHECK` for a narrower range.

## Literals

A whole number with no point or exponent is an `INTEGER` if it fits in 64 bits and a `BIGNUM` if it does not:

```sql
SELECT 42, -7, 123456789012345678901234567890;
SELECT TypeName(42), TypeName(123456789012345678901234567890);   -- INTEGER, BIGNUM
```

## Arithmetic never overflows

An integer operation whose result does not fit in 64 bits is computed exactly and becomes a `BIGNUM`:

```sql
SELECT 9223372036854775807 + 1;       -- 9223372036854775808  (a BIGNUM)
SELECT 2 ^ 100;                       -- 1267650600228229401496703205376
```

The result is **not** converted back to an `INTEGER` when it would fit; the type depends only on the operand types. When such a value is stored in an `INTEGER` column it is converted if in range, and refused otherwise: `'9223372036854775808' is out of range for INTEGER`.

| Operator | Result for whole numbers |
| --- | --- |
| `+ - *` | `INTEGER`, or `BIGNUM` on overflow |
| `/` | exact `DECIMAL` (`7 / 2` is 3.5); see [arithmetic.md](arithmetic.md) |
| `\` | whole-number division, rounded toward zero (`7 \ 2` is 3, `-7 \ 2` is -3) |
| `MOD`, `%` | remainder with the sign of the left operand (`-7 MOD 3` is -1) |
| `^` | exact when the exponent is a whole number: `INTEGER` or `BIGNUM` for a non-negative one, an exact `DECIMAL` for a negative one (`2 ^ -1` is 0.5); a `DOUBLE` for a fractional exponent |

`\` and `MOD` first round both operands to whole numbers (halves to the even one), as Access does, so `7.5 MOD 2` rounds 7.5 to 8 and gives 0, and the result is always an `INTEGER` or `BIGNUM`. Dividing by zero is an error.

## Conversion on storing

| Value stored | Result |
| --- | --- |
| a fraction (`1.5`) | rounded to the nearest whole number, halves to the even one: 2; 2.5 gives 2 |
| text that reads as a number (`'12'`, `' 12 '`, `'1e3'`) | the number, rounded the same way |
| other text | an error |
| `TRUE` / `FALSE` | −1 / 0 (the Access convention) |
| a date | its day number, counted from 1899-12-30 |
| a number out of range (`INTEGER` only) | an error |

`CInt(x)` (or `CAST(x AS INTEGER)`) and `CBig(x)` (or `CAST(x AS BIGNUM)`) do the same conversion in an expression.

## Display and sorting

Whole numbers are shown with no separators or exponent. Numbers sort by value, together with every other numeric type: an `INTEGER` column and a `DECIMAL` column can be joined, and `1 = 1.0` is true.

## Changing between them

`ALTER TABLE t ALTER COLUMN c TYPE BIGNUM` is always possible. Changing a `BIGNUM` column to `INTEGER` fails, naming the problem, if any stored value is too large.
