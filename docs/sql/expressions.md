# Expressions, operators and predicates

## Literals

| Literal | Type | Examples |
| --- | --- | --- |
| whole number | `INTEGER` (or `BIGNUM` when too big for 64 bits) | `42`, `-7`, `123456789012345678901234567890` |
| number with a point | `DECIMAL` (exact) | `3.14`, `.5`, `10.00` |
| number with an exponent | `DOUBLE` | `1e3`, `2.5E-4` |
| text | `TEXT` | `'it''s'`, `"say ""hi"""` |
| text in hexadecimal | `TEXT` | `X'48656c6c6f'` is `Hello`. There is no `BLOB` type, so the bytes must be valid UTF-8 (`X'ff'` is an error) |
| interval | `INTERVAL` | `INTERVAL '3 days'`, `INTERVAL '1-6' YEAR TO MONTH`, `INTERVAL '90' MINUTE` |
| date and time | `DATETIME` | `#2024-03-05#`, `#2024-03-05 14:30:00#`, `#3/5/2024 2:30 PM#`, `DATE '2024-03-05'`, `TIMESTAMP '2024-03-05 14:30:00'` |
| date and time with a zone | `TIMESTAMPTZ` | `TIMESTAMP WITH TIME ZONE '2024-03-05 14:30:00+01:00'` |
| truth values | `YESNO` | `TRUE`, `FALSE`, `YES`, `NO` |
| nothing | | `NULL` |

Single quotes make text; a quote inside is doubled. Double quotes also make text (the Access habit), not identifiers: write `[Name]` for a name that needs quoting. A parameter is written `@name`.

## Identifiers

`Table.Column`, `Alias.Column` and plain `Column` are the forms of a column reference. Names are case-insensitive. A name containing a space or punctuation, or equal to a reserved word, goes in brackets: `[Order Details].[Unit Price]`. A plain name that matches columns in more than one source is an error ("ambiguous"); qualify it.

## Operators

From loosest to tightest binding:

| Level | Operators |
| --- | --- |
| 1 | `OR` |
| 2 | `AND` |
| 3 | `NOT` |
| 4 | comparisons and predicates (including row values): `= <> != < <= > >=`, `IS`, `LIKE`, `BETWEEN`, `IN` |
| 5 | `&`, `\|\|` (join text) |
| 6 | `+`, `-` |
| 7 | `*`, `/`, `DIV` (exact division), `\` (whole-number division), `%` and `MOD` (remainder) |
| 8 | unary `-`, `+` |
| 9 | `^` (power) |
| 10 | `AT TIME ZONE` |
| 11 | `->`, `->>` (JSON member or element) |

Arithmetic on the [numeric types](../../README.md#numbers) is exact: the result type depends only on the operand types. `/` is the exception: it drops the fraction between whole numbers (`7 / 2` is 3) and divides in floating point otherwise, while `DIV` is exact division (`7 DIV 2` is the `DECIMAL` 3.5, a 50-digit `DECIMAL` when the quotient does not end), and dividing by zero is NULL ([arithmetic.md](../types/arithmetic.md#division)); `RATIONAL` stays exact; a whole-number overflow becomes `BIGNUM`. Dividing by zero is an error. Date arithmetic: a date plus or minus a number is a date that many days away; the difference of two dates is a number of days.

`&` joins text and treats NULL as empty; `'a' & NULL & 'b'` is `'ab'`. Numbers and dates are converted to text. `||` is the same operator. `+` is only arithmetic.

## NULL and three-valued logic

NULL means "no value". Any comparison with NULL is *unknown* (neither true nor false), and arithmetic with NULL is NULL. `WHERE`, `HAVING`, `ON` and `WHEN` keep a row only when the condition is true. `AND`, `OR` and `NOT` follow the usual truth tables for unknown: `false AND unknown` is false, `true OR unknown` is true, and `NOT unknown` is unknown.

## Comparison and predicates

| Predicate | True when |
| --- | --- |
| `a = b`, `<>`, `<`, `<=`, `>`, `>=` | the comparison holds; numbers compare by value across all numeric types, text ignores case |
| `a IS NULL`, `a IS NOT NULL` | `a` is / is not NULL |
| `a IS [NOT] TRUE`, `IS [NOT] FALSE` | the test result is exactly that; never unknown |
| `a IS [NOT] UNKNOWN` | the same as `IS [NOT] NULL` |
| `a IS [NOT] DISTINCT FROM b` | like `<>` (`=`) but two NULLs count as equal, and the answer is never unknown |
| `a [NOT] BETWEEN x AND y` | `x <= a AND a <= y` |
| `a [NOT] BETWEEN SYMMETRIC x AND y` | between the two, whichever is smaller |
| `a [NOT] IN (x, y, ...)` | equal to one of the list. An empty list `()` is allowed (as in SQLite and MySQL): `a IN ()` is false and `a NOT IN ()` true, even when `a` is NULL |
| `a [NOT] IN (query)` | equal to one of the query's values. If the query returns no rows, `a IN` is false and `a NOT IN` true for every `a`, NULL included; if it returns rows, a NULL `a` gives NULL |
| `[NOT] EXISTS (query)` | the query returns a row |
| `a op ANY / SOME / ALL (query)` | see [quantified comparisons](select.md#quantified-comparisons) |
| `a [NOT] LIKE pattern [ESCAPE c]` | the text matches the pattern |
| `a [NOT] SIMILAR TO pattern` | the whole text matches the pattern, in which `%` `_` `\|` `*` `+` `?` `{ }` `( )` `[ ]` have their regular-expression meanings: see [regex.md](../functions/regex.md#similar-to) |

### LIKE

Both the Access and the standard wildcards work in the same pattern:

| Wildcard | Matches |
| --- | --- |
| `*` or `%` | any run of characters, including none |
| `?` or `_` | exactly one character |
| `#` | exactly one digit |
| `[abc]`, `[a-f]` | one character from the set or range |
| `[!abc]` | one character not in the set |

Matching ignores case. `ESCAPE` names a character that takes away the special meaning of the next one; the pattern and the escape character must be literals: `Code LIKE 'A!_%' ESCAPE '!'`. A literal `[` is written `[[]`.

## Row values

`(a, b)` or `ROW(a, b)` compared with another row of the same size, or used with `IN`; see [select.md](select.md#row-values).

## Time zones and intervals

| Form | Meaning |
| --- | --- |
| `x AT TIME ZONE zone` | converts between a `TIMESTAMPTZ` and the wall clock of `zone`; it binds tighter than `^` |
| `date + INTERVAL '1 month'` | moves a date by a calendar interval |
| `INTERVAL '1 day' * 3` | scales an interval |

See [../types/timestamptz.md](../types/timestamptz.md) and [../types/interval.md](../types/interval.md).

## JSON access

`doc -> key` gives the member (text key) or element (whole-number position, from 0) of a JSON document as JSON; `doc ->> key` gives it as text or a number. They chain: `body -> 'items' -> 0 ->> 'name'`. See [functions/json.md](../functions/json.md).

## Sequence values

`NEXT VALUE FOR seq` takes the next number of a [sequence](sequences.md); `PREVIOUS VALUE FOR seq` is its current number.

## CASE

```sql
CASE WHEN Sal > 100 THEN 'high' WHEN Sal > 50 THEN 'mid' ELSE 'low' END
CASE Dept WHEN 'Eng' THEN 1 WHEN 'Ops' THEN 2 END
```

The first form tests conditions; the second compares one value with each `WHEN` value. Only the chosen branch is evaluated. Without a matching branch and without `ELSE` the result is NULL.

## CAST

`CAST(expr AS type)` converts a value to one of the [data types](ddl.md#data-types). Sizes are ignored except the scale of a decimal: `CAST(x AS DECIMAL(10, 2))` rounds to two places (halves go to the even digit). A value that cannot be converted is an error. A NULL stays NULL.

## Other standard forms

| Form | Same as |
| --- | --- |
| `EXTRACT(field FROM date)` | [`DatePart`](../functions/datetime.md#datepart) with the field name |
| `POSITION(needle IN text)` | `InStr(text, needle)` |
| `SUBSTRING(text FROM start [FOR length])` | `Mid(text, start, length)` |
| `OVERLAY(text PLACING r FROM n [FOR m])` | [`Overlay`](../functions/text.md#overlay) |
| `TRIM([LEADING \| TRAILING \| BOTH] [chars] FROM text)` | removes the characters (spaces by default) |
| `CURRENT_DATE` | `Date()` |
| `CURRENT_TIMESTAMP`, `CURRENT_TIME`, `LOCALTIMESTAMP`, `LOCALTIME` | `Now()` |

## Sub-queries as values

A parenthesised query used as a value is a scalar sub-query: it must return one column and at most one row; no row gives NULL. See [select.md](select.md#sub-queries).

## Functions

See the [function reference](../functions/README.md).
