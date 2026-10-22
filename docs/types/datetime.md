# DATETIME

A date with a time of day, to the second. Aliases: `DATE`, `TIME`, `TIMESTAMP` (`TIMESTAMP WITH TIME ZONE` is accepted, but no zone is stored). They are all the same type, so a `DATE` column can hold a time and a `TIME` column a date.

## Representation

A `DATETIME` is a whole number of **seconds since 1970-01-01 00:00:00**, in one fixed time zone. Consequences:

* There are **no time zones** and no daylight-saving adjustments; store UTC or local time consistently.
* There are **no fractions of a second**. `CDate(45000.5)` is half a day, 12:00:00; anything finer than a second is rounded.
* The range is far wider than any practical need, from year 1 backwards and forwards (the calendar is the proleptic Gregorian one).

## Literals

```sql
#2024-03-05#                       -- a date (midnight)
#2024-03-05 14:30#                 -- date and time
#2024-03-05 14:30:15#
#3/5/2024#                         -- US order: month/day/year
#3/5/2024 2:30 PM#
DATE '2024-03-05'                  -- standard forms
TIMESTAMP '2024-03-05 14:30:00'
```

Text is converted to a date when stored in a `DATETIME` column or compared with one. The accepted forms are `YYYY-MM-DD`, with `HH:MM` or `HH:MM:SS` after a space or a `T`, and `M/D/YYYY` with an optional time and `AM`/`PM`. Anything else is an error.

## Display

A value at midnight shows as the date alone, `2024-03-05`; any other value shows its time, `2024-03-05 14:30:00`. Use [`Format`](../functions/format.md#dates) for another layout.

## Arithmetic

| Expression | Result |
| --- | --- |
| `date + n`, `date - n` | a `DATETIME` n days later or earlier; n may have a fraction (`+ 0.5` is twelve hours) |
| `date1 - date2` | the difference in days, as a number (with a fraction) |
| `date` compared with a number | the number is read as a day count from 1899-12-30, as in Access |
| `date` stored in a numeric column | that day count: `#2024-01-01#` stored in an `INTEGER` column is 45292 |
| `date` compared with text | the text is converted to a date |

Month and year arithmetic is done with `DateAdd`, which clamps to the end of a shorter month: `DateAdd('m', 1, #2024-01-31#)` is 2024-02-29.

## Functions

`Now`, `Date`, `Year`, `Month`, `Day`, `Hour`, `Minute`, `Second`, `Weekday`, `DateAdd`, `DateDiff`, `DateSerial`, `DatePart`, `EXTRACT`, `CDate`: see [datetime functions](../functions/datetime.md).

## Conversion on storing

| Value stored | Result |
| --- | --- |
| text in one of the date formats | that date and time |
| a number | the date that many days after 1899-12-30 (45000 is 2023-03-15; a fraction is the time of day) |
| other text | an error |

`CDate(x)` or `CAST(x AS DATETIME)` do the same conversion in an expression.

## Sorting and grouping

Dates sort chronologically. To group by month, group by `Year(d), Month(d)` or by `Format(d, 'yyyy-mm')`, or by `DateSerial(Year(d), Month(d), 1)`, which stays a date.
