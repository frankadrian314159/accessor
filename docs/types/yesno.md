# YESNO

A true-or-false value. Aliases: `BOOLEAN`, `BOOL`, `BIT`, `LOGICAL`.

## Literals

`TRUE`, `FALSE`, `YES` and `NO` (any case). They are displayed as `Yes` and `No`, and converted to text (`CStr`, `&`, storing in a `TEXT` column) as `Yes` and `No`. `Format(x)` without a pattern gives `True` and `False`; `Format(x, 'Yes/No')`, `'True/False'` and `'On/Off'` choose the words.

```sql
CREATE TABLE Task (ID COUNTER PRIMARY KEY, Title TEXT, Done YESNO DEFAULT FALSE);
UPDATE Task SET Done = YES WHERE ID = 1;
SELECT Title FROM Task WHERE NOT Done;
```

A `YESNO` can be used directly as a condition, without `= TRUE`.

## Three-valued logic

A `YESNO` column may be NULL, which as a condition means *unknown*. `WHERE` keeps only rows where the condition is true, so a row whose flag is NULL matches neither `WHERE Done` nor `WHERE NOT Done`. Use `Done IS TRUE`, `Done IS NOT TRUE`, `IS FALSE` or `Nz(Done, FALSE)` to treat NULL as false. See [expressions](../sql/expressions.md#null-and-three-valued-logic).

## Conversion on storing

| Value stored | Result |
| --- | --- |
| a number | false if zero, true otherwise |
| `'yes'`, `'true'`, `'on'`, `'1'` (any case) | true |
| `'no'`, `'false'`, `'off'`, `'0'` (any case) | false |
| other text | an error (`cannot convert TEXT value 'maybe' to YESNO`) |

In conditions, a number is also accepted wherever a yes/no is expected, with zero false and any other value true.

## As a number

In arithmetic and when stored in a numeric column, `TRUE` is −1 and `FALSE` is 0, as in Access: `SELECT TRUE + TRUE` is −2, and `1 + TRUE` is 0. Sorting puts false before true. Comparing: `TRUE = 1` is false, since true is −1. To count the true values use `Sum(IIf(Done, 1, 0))` or `Count(*) FILTER (WHERE Done)`.

The aggregates `EVERY` (also `BOOL_AND`) and `BOOL_OR` combine yes/no values; see [aggregate](../functions/aggregate.md#every-bool_and-and-bool_or).
