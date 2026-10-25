# INTERVAL

A length of time, kept as **months**, **days** and **seconds**. Use it for "3 days", "1 month", "90 minutes", for adding to and subtracting from dates, and for the gap between two timestamps.

```sql
CREATE TABLE Task (ID COUNTER PRIMARY KEY, Name TEXT, Estimate INTERVAL);
INSERT INTO Task (Name, Estimate) VALUES ('write', '2 hours 30 minutes'), ('review', '1 day');

SELECT Name, Estimate, #2024-03-05 09:00# + Estimate AS Due FROM Task;
```

## Why three parts

Months and days are not fixed numbers of seconds. A month is 28 to 31 days; a day is 23 or 25 hours across a daylight-saving change. So an interval keeps the three apart:

| Part | Meaning when added to a date |
| --- | --- |
| months (and years: 12 months) | the same day of a later month, or the last day if that month is shorter: 31 January + 1 month is 29 February (2024) |
| days (and weeks: 7 days) | the same time of day on a later calendar day |
| seconds (hours, minutes, seconds) | exactly that much elapsed time |

Only the seconds part is exact elapsed time. On a zoned timestamp ([timestamptz.md](timestamptz.md)) a day across a clock change keeps the wall-clock time, while 24 hours do not:

```sql
SET TIME ZONE 'America/New_York';
SELECT TIMESTAMP WITH TIME ZONE '2024-03-09 12:00:00' + INTERVAL '1 day';      -- 2024-03-10 12:00:00-04:00
SELECT TIMESTAMP WITH TIME ZONE '2024-03-09 12:00:00' + INTERVAL '24 hours';   -- 2024-03-10 13:00:00-04:00
```

## Literals and text

```sql
INTERVAL '3 days'
INTERVAL '1 year 2 months 3 days 04:05:06'
INTERVAL '-5 minutes'
INTERVAL '1.5 hours'                      -- fractions of hours, minutes, seconds, days and weeks
INTERVAL '2 weeks'
INTERVAL 'P1Y2M3DT4H5M6S'                 -- ISO 8601
INTERVAL '5' DAY                          -- a bare number in the unit named after it
INTERVAL '90' MINUTE
INTERVAL '1-6' YEAR TO MONTH              -- 1 year 6 months
INTERVAL '2 03:04:05' DAY TO SECOND       -- 2 days 03:04:05
```

Units are `year(s)`, `month(s)` (`mon`), `week(s)`, `day(s)`, `hour(s)` (`hr`), `minute(s)` (`min`), `second(s)` (`sec`); the one-letter forms `y d h s w` also work after a number. Text in these forms is converted when stored in an `INTERVAL` column or given to `CAST(x AS INTERVAL)` / `CInterval(x)`. A plain number converted to an interval is a number of **days**. Text that is not an interval is an error.

An interval is shown as `1 year 2 months 3 days 04:05:06`: whole years and months, days, then the time of day (hours may exceed 24). An empty interval shows `00:00:00`.

## Arithmetic

| Expression | Result |
| --- | --- |
| `interval + interval`, `interval - interval` | `INTERVAL` |
| `-interval` | `INTERVAL` |
| `interval * number`, `number * interval` | `INTERVAL`; a fraction of a month becomes 30-day days, a fraction of a day becomes seconds |
| `interval / number` | `INTERVAL`; dividing by zero is an error |
| `datetime ± interval` | `DATETIME` |
| `timestamptz ± interval` | `TIMESTAMPTZ` |
| `timestamptz - timestamptz` | `INTERVAL` of elapsed seconds |

`datetime - datetime` is still a **number of days** (the Access convention), not an interval. Use `AGE(a, b)` for years, months and days between two dates, or subtract `TIMESTAMPTZ` values for elapsed time.

## Comparing

For comparison, sorting, equality and grouping, a month counts as **30 days** and a day as **24 hours**: `INTERVAL '1 month' = INTERVAL '30 days'`, and `INTERVAL '25 hours' > INTERVAL '1 day'`. NULL sorts as usual. An interval cannot be compared with a number.

## Functions

* `EXTRACT(field FROM interval)` / `DatePart`: `YEAR` (months ÷ 12), `MONTH` (months mod 12), `DAY`, `HOUR` (whole hours of the seconds part), `MINUTE`, `SECOND`, `EPOCH` (total seconds, a month as 30 days).
* `MakeInterval(years, months, days, hours, mins, secs)` builds one; missing parts are 0.
* `AGE(a, b)` is the calendar difference of two dates as an interval.
* `Sum`, `Avg`, `Min`, `Max` work on interval columns.
* `GENERATE_SERIES(start, stop, step)` takes an interval step to make a series of dates: see [table functions](../functions/table-functions.md).

## Range

Each part is a 64-bit whole number; arithmetic that overflows is an error ("interval out of range").
