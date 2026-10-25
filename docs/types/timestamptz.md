# TIMESTAMPTZ (timestamps with a time zone)

A moment in time, independent of any clock. It is stored as seconds since 1970-01-01 00:00:00 **UTC**, and shown on the wall clock of the *session time zone*. Aliases: `TIMESTAMP WITH TIME ZONE`, `TIME WITH TIME ZONE`.

Compare [`DATETIME`](datetime.md), which is a wall-clock reading with **no** zone: "2024-03-05 14:30" in nobody's particular place. Use `TIMESTAMPTZ` for events that happened at a moment (a login, a payment, a meeting) and `DATETIME` for dates and times that mean the same thing everywhere (a birthday, a shop's opening time).

```sql
CREATE TABLE Login (ID COUNTER PRIMARY KEY, Who TEXT, At TIMESTAMPTZ);
SET TIME ZONE 'Europe/Paris';
INSERT INTO Login (Who, At) VALUES ('ada', '2024-03-05 14:30:00');          -- read as Paris time
INSERT INTO Login (Who, At) VALUES ('bob', '2024-03-05 14:30:00-05:00');    -- an offset in the text
SELECT * FROM Login;                                                         -- shown as Paris time
SET TIME ZONE 'Asia/Tokyo';
SELECT * FROM Login;                                                         -- the same instants, in Tokyo time
```

## Time zones

A zone is one of:

| Form | Examples |
| --- | --- |
| `UTC`, `Z`, `GMT` | `UTC` |
| a fixed offset | `+05:30`, `-0800`, `+5` |
| a region of the IANA time zone database | `Europe/Paris`, `America/New_York`, `Asia/Kolkata`, `Australia/Sydney` |

Regions follow the real rules, **daylight saving time included**, from the IANA database built into Accessor. Names are not case-sensitive. When the clock is set back, the repeated hour is read as its first occurrence; when it jumps forward, a time inside the skipped hour is read with the offset from before the jump (02:30 on the spring-forward day becomes 03:30).

### The session time zone

`SET TIME ZONE 'Europe/Paris'` (also `SET TIMEZONE TO 'x'`, and `SET TIME ZONE LOCAL` for UTC) chooses the zone for the rest of the session:

* text **without an offset** is read in that zone when it is stored in a `TIMESTAMPTZ`, compared with one, or given to `CAST`;
* a `TIMESTAMPTZ` is **shown** on that zone's wall clock, with its offset: `2024-03-05 14:30:00+01:00`;
* `EXTRACT`, `Year`, `Hour`, `Format` and the other date functions applied to a `TIMESTAMPTZ` use that wall clock;
* a `DATETIME` converted to a `TIMESTAMPTZ`, or back, goes through it.

The default is `UTC`. The setting belongs to the open database (`Database::set_time_zone`, shell `.timezone NAME`), is **not** stored in the file and is not undone by `ROLLBACK`. The stored instants never change, whatever the setting.

## Reading timestamps

Accepted forms, with or without an offset or zone name:

```
2024-03-05 14:30:00            read in the session zone
2024-03-05T14:30:00Z           UTC
2024-03-05 14:30:00+01:00      offset
2024-03-05 14:30:00-0500
2024-03-05 14:30:00 Europe/Paris
3/5/2024 2:30 PM               US form, session zone
```

and the typed literal `TIMESTAMP WITH TIME ZONE '2024-03-05 14:30:00+01:00'`. Text that is not a timestamp is an error.

## AT TIME ZONE

```sql
timestamptz AT TIME ZONE zone      -- the wall clock in that zone: a DATETIME
datetime    AT TIME ZONE zone      -- the instant at which that zone's clock read this: a TIMESTAMPTZ
```

The zone is text (`'Asia/Tokyo'`, `'+05:30'`) or an interval used as a fixed offset (`INTERVAL '5 hours 30 minutes'`). `TIMEZONE(zone, value)` is the same with the arguments the other way round.

```sql
SELECT TIMESTAMP WITH TIME ZONE '2024-07-15 12:00:00+00:00' AT TIME ZONE 'Asia/Tokyo';   -- 2024-07-15 21:00:00
SELECT #2024-07-15 12:00:00# AT TIME ZONE 'Europe/Paris';       -- the instant 12:00 in Paris
```

## Arithmetic and comparison

* `timestamptz ± interval` moves by months and days on the wall clock of the session zone, and by seconds exactly; see [interval.md](interval.md).
* `timestamptz - timestamptz` is an `INTERVAL` of elapsed seconds. `timestamptz ± number` adds that many days of 86 400 seconds.
* Timestamps compare as instants. A `DATETIME` compared with a `TIMESTAMPTZ` is read **as UTC**; use `AT TIME ZONE` to say otherwise.

## Conversion

| From | To `TIMESTAMPTZ` |
| --- | --- |
| text | read as above |
| `DATETIME` | its wall clock read in the session zone |
| a number | days since 1899-12-30 as a `DATETIME`, then as above |
| `TIMESTAMPTZ` → `DATETIME` | the wall clock in the session zone |

`CTimestampTz(x)` and `CAST(x AS TIMESTAMPTZ)` convert in an expression.

## Current time

| | Type | Value |
| --- | --- | --- |
| `CURRENT_TIMESTAMP`, `NowTz()` | `TIMESTAMPTZ` | the current instant |
| `LOCALTIMESTAMP`, `CURRENT_TIME`, `LOCALTIME` | `DATETIME` | the wall clock of the session zone |
| `CURRENT_DATE` | `DATETIME` | today's date in the session zone, at midnight |
| `Now()`, `Date()` | `DATETIME` | the Access functions, in UTC |
| `SessionTimeZone()` | text | the name of the session zone |

## Indexes and the optimizer

An index on a `TIMESTAMPTZ` column orders instants, so ranges and `ORDER BY ... LIMIT` use it. A range such as

```sql
WHERE At >= TIMESTAMP WITH TIME ZONE '2024-03-05 00:00:00+00:00'
  AND At <  TIMESTAMP WITH TIME ZONE '2024-03-05 00:00:00+00:00' + INTERVAL '1 day'
```

has its bounds worked out once before the scan and is answered by an index range scan. Do not wrap the column in a function (`Year(At) = 2024` cannot use the index).
