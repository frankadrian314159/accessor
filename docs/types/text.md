# TEXT

`TEXT` holds Unicode text of any length, from the empty string up to the memory available. There is no separate short and long text type, and a length given in the definition (`VARCHAR(50)`) is accepted and **not enforced**. Use a `CHECK (Len(col) <= 50)` constraint if you need a limit.

Aliases: `VARCHAR`, `CHAR`, `CHARACTER [VARYING]`, `NCHAR`, `NVARCHAR`, `NATIONAL CHARACTER`, `STRING`, `MEMO`, `LONGTEXT`, `CLOB`.

## Literals

```sql
'plain text'
'it''s'            -- a single quote is doubled
"also text"        -- double quotes make text too, as in Access
```

Double quotes are not identifiers; use `[brackets]` for a name that needs quoting.

## Comparison and sorting

Text is compared **without regard to case**: `'abc' = 'ABC'` is true, `'a' < 'B'` is true, and `ORDER BY` puts `apple` before `Banana`. This applies to `=`, `<`, `GROUP BY`, `DISTINCT`, `UNION`, joins, indexes and unique constraints: a unique column cannot hold both `Smith` and `SMITH`. The text keeps the case it was stored with. `Replace` is the exception, matching case-sensitively.

Comparison is by Unicode character, not by a locale's dictionary order.

## What is stored

Whatever you store is converted to text if it is not already:

| Stored | Becomes |
| --- | --- |
| a number | its text form: `12`, `3.5` (a `DECIMAL` never shows trailing zeros) |
| a yes/no | `True` or `False` |
| a date | `2024-03-05` or `2024-03-05 14:30:00` |

## Operators and functions

* `&` and `||` join text, treating NULL as empty. `+` is not text joining.
* `LIKE` matches patterns, with `*`/`%`, `?`/`_`, `#` and `[sets]`.
* The text functions are in [../functions/text.md](../functions/text.md); `Format` shapes text with a pattern.

## Converting text

* Text to a number: when stored in a numeric column, text that reads as a number is accepted (`' 12 '`, `'1e3'`, `'1,5'` is read as 15: commas are taken as thousands separators). Anything else is an error. `Val(text)` is lenient instead and reads the leading number, giving 0 when there is none.
* Text to a date: `'2024-03-05'`, `'2024-03-05 10:20'`, `'2024-03-05T10:20:00'` and `'3/5/2024 2:30 PM'` are understood.
* Text to yes/no: `yes`, `true`, `on`, `1` and their opposites; see [yesno.md](yesno.md).
