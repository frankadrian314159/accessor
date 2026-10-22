# Column types

Every column has one type, chosen when the table is created (`CREATE TABLE t (col TYPE)`) and changeable later with `ALTER TABLE t ALTER COLUMN col TYPE new_type`. Every value in the column is converted to that type when it is stored, so a column never holds a mixture.

| Type | Page | Holds |
| --- | --- | --- |
| `TEXT` | [text.md](text.md) | Unicode text of any length |
| `INTEGER` | [integers.md](integers.md) | whole numbers from −2⁶³ to 2⁶³−1 |
| `BIGNUM` | [integers.md](integers.md) | whole numbers of any size |
| `FLOAT` | [floating-point.md](floating-point.md) | 32-bit binary floating-point numbers |
| `DOUBLE` | [floating-point.md](floating-point.md) | 64-bit binary floating-point numbers |
| `DECIMAL` | [decimal-currency.md](decimal-currency.md) | exact decimal numbers of any size |
| `CURRENCY` | [decimal-currency.md](decimal-currency.md) | exact decimal numbers, shown as money |
| `RATIONAL` | [rational.md](rational.md) | exact fractions |
| `YESNO` | [yesno.md](yesno.md) | true or false |
| `DATETIME` | [datetime.md](datetime.md) | a date with a time of day |
| `COUNTER` | [counter.md](counter.md) | an `INTEGER` that numbers itself |
| `CALCULATED` | [calculated.md](calculated.md) | a value worked out from other columns |

Two further pages apply to all the numeric types:

* [arithmetic.md](arithmetic.md): which type an operation gives when the operands differ in type.
* [conversions.md](conversions.md): how a value is converted when it is stored in a column, passed to `CAST`, or compared.

## Names and aliases

Each type may be written with its own name or with a name from the standard or from other systems.

| Type | Also accepted |
| --- | --- |
| `TEXT` | `VARCHAR`, `CHAR`, `CHARACTER`, `CHARACTER VARYING`, `NCHAR`, `NVARCHAR`, `NATIONAL CHARACTER [VARYING]`, `STRING`, `MEMO`, `LONGTEXT`, `CLOB` |
| `INTEGER` | `INT`, `SMALLINT`, `TINYINT`, `BIGINT`, `LONG`, `SHORT`, `BYTE` |
| `FLOAT` | `REAL`, `SINGLE` |
| `DOUBLE` | `DOUBLE PRECISION` |
| `DECIMAL` | `NUMERIC`, `DEC`, `NUMBER` |
| `CURRENCY` | `MONEY` |
| `YESNO` | `BOOLEAN`, `BOOL`, `BIT`, `LOGICAL` |
| `DATETIME` | `DATE`, `TIME`, `TIMESTAMP` (with or without `WITH TIME ZONE`) |
| `COUNTER` | `AUTOINCREMENT`, `AUTONUMBER`, `SERIAL` |

All of `SMALLINT`, `INT` and `BIGINT` are the same 64-bit `INTEGER`; `BIGINT` does *not* mean a `BIGNUM`. A length or precision after a name, as in `VARCHAR(50)` or `NUMERIC(10, 2)`, is accepted and ignored.

## NULL

Every type except `COUNTER` and `CALCULATED` can hold NULL, meaning "no value", unless the column is `NOT NULL` or part of the primary key. NULL is not zero and not the empty string: `''` is an ordinary text of length 0 and `'' IS NULL` is false. Arithmetic or comparison with NULL gives NULL (unknown); see [expressions](../sql/expressions.md#null-and-three-valued-logic). NULLs sort before all other values in ascending order.

## Choosing a type

| You are storing | Use |
| --- | --- |
| names, notes, codes | `TEXT` |
| a count, a quantity, a whole-number identifier | `INTEGER` |
| a surrogate key | `COUNTER` |
| money | `CURRENCY` |
| a measurement or rate that must be exact (a price per unit, a tax rate) | `DECIMAL` |
| a ratio that must be exact, like one third | `RATIONAL` |
| a value that came from a sensor or a scientific calculation | `DOUBLE` |
| a whole number that may exceed 9.2 × 10¹⁸ | `BIGNUM` |
| a yes/no flag | `YESNO` |
| a date or timestamp | `DATETIME` |
| a value that is always derived from other columns | `CALCULATED` |

When in doubt about a number, prefer the exact types; they cost a little speed but never surprise.
