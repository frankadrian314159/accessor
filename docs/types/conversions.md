# Converting between types

A value is converted to a column's type when it is stored, when it is given to `CAST` or a conversion function, and in some comparisons. If a value cannot be converted, the statement fails with a message naming the column and the value (`Type mismatch: column 'i': cannot convert TEXT value 'abc' to INTEGER`) and changes nothing.

## Converting on storing (`INSERT`, `UPDATE`, datasheet edits)

Rows: the value given. Columns: the type of the column it is stored in. "ok" means the value is converted exactly; other cells say what happens.

| Value \ Column | TEXT | INTEGER | BIGNUM | DECIMAL / CURRENCY | RATIONAL | FLOAT / DOUBLE | YESNO | DATETIME |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **whole number** | its digits | ok (error if out of range) | ok | ok | ok | nearest float | zero is false, else true | days from 1899-12-30 |
| **decimal** | its digits | rounded, halves to even | rounded | ok | exact fraction | nearest float | zero is false, else true | days from 1899-12-30 (fraction is time of day) |
| **float** | shortest text | rounded | rounded | its shortest decimal | fraction of its shortest decimal | ok (`FLOAT`: may lose digits) | zero is false, else true | as a decimal |
| **text** | ok | parsed, rounded | parsed, rounded | parsed | parsed (`'1/3'`, `'0.5'`) | parsed | `yes/true/on/1`, `no/false/off/0` | a date format |
| **yes/no** | `Yes`/`No` | −1 / 0 | −1 / 0 | −1 / 0 | −1 / 0 | −1 / 0 | ok | the day number −1 / 0 |
| **date** | `2024-03-05` | day number | day number | day number | day number | day number | error | ok |
| **NULL** | NULL | NULL | NULL | NULL | NULL | NULL | NULL | NULL |

"Parsed" means text that reads as a number is accepted, with surrounding spaces ignored, an exponent allowed (`'1e3'` is 1000), and commas treated as thousands separators (`'1,500'` is 1500); anything else is an error. The date formats are `YYYY-MM-DD`, `YYYY-MM-DD HH:MM[:SS]` (with a space or `T`) and `M/D/YYYY [h:mm[:ss] [AM|PM]]`.

### TIMESTAMPTZ and INTERVAL

| Value | To `TIMESTAMPTZ` | To `INTERVAL` |
| --- | --- | --- |
| text | a timestamp with or without an offset; read in the session zone if none | `3 days`, `1 year 2 months`, `04:05:06`, ISO 8601 |
| `DATETIME` | its wall clock read in the session zone | error |
| `TIMESTAMPTZ` | ok | error |
| a number | days since 1899-12-30, then as a `DATETIME` | that many days |
| `INTERVAL` | error | ok |

`TIMESTAMPTZ` converted to `DATETIME` gives the wall clock in the session zone. See [timestamptz.md](timestamptz.md) and [interval.md](interval.md).

## Converting in an expression

| Function | Same as | Notes |
| --- | --- | --- |
| `CInt(x)`, `CLng(x)` | `CAST(x AS INTEGER)` | `CInt` rounds, halves to even; **`CAST` drops the fraction** (toward zero) of a `FLOAT`, `DOUBLE`, `DECIMAL`, `CURRENCY` or `RATIONAL`: `CAST(2.7 AS INTEGER)` is 2, `CAST(-2.7 AS INTEGER)` is -2, `CInt(2.7)` is 3 |
| `CBig(x)` | `CAST(x AS BIGNUM)` | `CBig` rounds; `CAST` drops the fraction, as for `INTEGER` |
| `CSng(x)`, `CFloat(x)` | `CAST(x AS FLOAT)` | |
| `CDbl(x)` | `CAST(x AS DOUBLE)` | |
| `CDec(x)` | `CAST(x AS DECIMAL)` | |
| `CCur(x)` | `CAST(x AS CURRENCY)` | `CCur` rounds to four places |
| `CRat(x)` | `CAST(x AS RATIONAL)` | |
| `CStr(x)` | `CAST(x AS TEXT)` | |
| `CBool(x)` | `CAST(x AS YESNO)` | |
| `CDate(x)` | `CAST(x AS DATETIME)` | |
| `CTimestampTz(x)` | `CAST(x AS TIMESTAMPTZ)` | read in the session zone when there is no offset |
| `CInterval(x)` | `CAST(x AS INTERVAL)` | |
| `Val(text)` | | reads the number at the start of the text, 0 if none; never fails |

`CAST(x AS DECIMAL(p, s))` and `CAST(x AS NUMERIC(p, s))` also round to `s` places, halves to even. The other sizes and precisions are ignored. See the [conversion functions](../functions/conversion.md).

## Converting in comparisons and arithmetic

* Two numbers of any types compare by exact value; see [arithmetic.md](arithmetic.md).
* A number compared with text converts the text to a number if it reads as one: `5 = '5'` is true. Arithmetic does the same: `'10' + 1` is 11.
* A date compared with text converts the text to a date: `d = '2024-03-05'`.
* A date compared with a number reads the number as a day count from 1899-12-30.
* Two texts compare as text, ignoring case: `'10' < '9'` is true.

## Changing a column's type

```sql
ALTER TABLE t ALTER COLUMN c TYPE new_type;
```

Every stored value is converted by the rules above. If any value cannot be converted, the whole change is refused, nothing is altered, and the message names the column and the value ("... is out of range for INTEGER; no data was changed"). Changes that lose information (a `DOUBLE` to an `INTEGER`, a `TEXT` to a `YESNO`) are allowed when every value converts. A column cannot be converted to `COUNTER`; a column that a calculated column or a foreign key depends on cannot be retyped until that dependence is removed. See [calculated.md](calculated.md).
