# Date and time functions

A `DATETIME` is a number of whole seconds since 1970-01-01 00:00:00, in one time zone (there are no zones and no fractions of a second). A value at midnight is shown as a date alone: `2024-03-05`; otherwise as `2024-03-05 14:30:00`.

Dates are written as literals `#2024-03-05#`, `#2024-03-05 14:30:00#`, `#3/5/2024 2:30 PM#` (month first), or `DATE '2024-03-05'`, `TIMESTAMP '2024-03-05 14:30:00'`. Text is converted when a date is needed.

Arithmetic: `date + n` and `date - n` move by `n` days (fractions of a day work: `+ 0.5` is twelve hours). `date1 - date2` is the difference in days. Compared with a plain number, a date is its day number counted from 1899-12-30, as in Access.

## Now and Date

| Function | Returns |
| --- | --- |
| `Now()` | the current date and time |
| `Date()` (also `Today()`) | today's date, at midnight |
| `CURRENT_TIMESTAMP`, `LOCALTIMESTAMP`, `CURRENT_TIME`, `LOCALTIME` | the same as `Now()` |
| `CURRENT_DATE` | the same as `Date()` |

The standard forms are written without parentheses. These functions give a different answer each time, so they are not allowed in [calculated columns](../sql/ddl.md#calculated-columns).

## Year, Month, Day, Hour, Minute, Second

Each takes a date and returns that part as a whole number: `Year(#2024-03-05#)` is 2024, `Month` 3, `Day` 5. `Hour` is 0 to 23.

## Weekday

`Weekday(date)` returns 1 for Sunday through 7 for Saturday.

```sql
SELECT Weekday(#2024-03-05#);         -- 3 (a Tuesday)
```

## DateAdd

`DateAdd(interval, number, date)` adds `number` intervals to `date`. `number` may be negative.

| Interval | Unit |
| --- | --- |
| `'yyyy'` (or `'y'`) | years |
| `'q'` | quarters |
| `'m'` | months |
| `'d'` (or `'w'`) | days |
| `'ww'` | weeks |
| `'h'` | hours |
| `'n'` | minutes |
| `'s'` | seconds |

Adding months or years to a day that the target month lacks gives the last day of that month: `DateAdd('m', 1, #2024-01-31#)` is 2024-02-29.

## DateDiff

`DateDiff(interval, date1, date2)` returns how many intervals lie from `date1` to `date2` (negative if `date2` is earlier). The intervals are those of `DateAdd`.

For **years, quarters, months and days** it counts the *boundaries crossed*, not elapsed time: `DateDiff('yyyy', #2020-12-31#, #2021-01-01#)` is 1, though only a day apart. For **weeks, hours, minutes and seconds** it counts whole elapsed units, ignoring a remainder: from 10:30 to 12:29 is one hour.

```sql
SELECT DateDiff('m', #2024-01-31#, #2024-03-01#);    -- 2
SELECT DateDiff('d', #2024-02-28#, #2024-03-01#);    -- 2
```

## DateSerial

`DateSerial(year, month, day)` builds a date. Out-of-range months and days carry over, so `DateSerial(2024, 14, 1)` is 2025-02-01 and `DateSerial(2024, 3, 0)` is 2024-02-29, the last day of February. This is the usual way to get the first or last day of a month.

```sql
SELECT DateSerial(Year(Now()), Month(Now()) + 1, 0);   -- last day of this month
```

## DatePart

`DatePart(field, date)` (also `DATE_PART`) returns one part of a date as a whole number. The field name is not case-sensitive.

| Field | Returns |
| --- | --- |
| `'yyyy'`, `'year'` | the year |
| `'q'`, `'quarter'` | the quarter, 1 to 4 |
| `'m'`, `'month'` | the month, 1 to 12 |
| `'y'`, `'doy'`, `'dayofyear'` | the day of the year, 1 to 366 |
| `'d'`, `'day'` | the day of the month |
| `'w'`, `'weekday'` | the weekday, 1 (Sunday) to 7 |
| `'dow'` | the weekday, 0 (Sunday) to 6 |
| `'ww'`, `'week'` | the week of the year, weeks starting on Sunday, the first week containing 1 January |
| `'h'`, `'hour'` | the hour |
| `'n'`, `'mi'`, `'minute'` | the minute |
| `'s'`, `'second'` | the second |
| `'epoch'` | the seconds since 1970-01-01 |

## EXTRACT

`EXTRACT(field FROM date)` is the standard spelling of `DatePart`, with the field written as a word, not text: `EXTRACT(YEAR FROM d)`, and the same fields as above.

```sql
SELECT EXTRACT(QUARTER FROM DATE '2024-11-05');   -- 4
```

## Related

* [`Format`](format.md#dates) turns a date into text with a pattern.
* [`CDate`](conversion.md#conversion-functions) and `CAST(x AS DATETIME)` convert text and numbers to dates.
