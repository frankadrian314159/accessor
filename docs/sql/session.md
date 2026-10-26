# Session settings

A few settings belong to the open database, not to the file. They last until changed or until the database is closed, and are not undone by `ROLLBACK`.

## Run-time parameters

`SET name TO value` (or `SET name = value`), `RESET name` and `SHOW name` read and write named settings, as in PostgreSQL, so that scripts written for it run:

```sql
SET extra_float_digits TO 3;
SHOW extra_float_digits;        -- 3
SET datestyle TO ISO, DMY;      -- several values are kept as 'ISO, DMY'
RESET extra_float_digits;       -- back to the default, 1
SET enable_seqscan = off;       -- switches take on/off/true/false/yes/no/1/0, and show as on or off
SET my.setting TO 'abc';        -- a name with a dot may be made up
SET name TO DEFAULT;            -- the same as RESET name
SHOW ALL;                       -- name, setting and description of every parameter
RESET ALL;
```

About seventy of PostgreSQL's parameter names are known (the planner switches `enable_*`, `work_mem`, `search_path`, `statement_timeout`, `client_min_messages`, `standard_conforming_strings`, and so on), each with its default. Most **only remember** their value: nothing consults `work_mem` or `enable_seqscan`. Two have an effect: `timezone` (the same as `SET TIME ZONE`, below) and `optimizer` (`on` or `off`, the query optimizer; also `Database::set_optimizer`). A name that is not known and has no dot is an error ("unrecognized configuration parameter"), and so is a switch given something other than a truth value. `SET LOCAL` is the same as `SET` (there is no per-transaction state), and `SET TRANSACTION ...`, `SET SESSION CHARACTERISTICS ...` and `SET ROLE ...` are read and ignored: there is one user and one isolation level. A third has an effect too: **`statement_timeout`** (milliseconds, or a number with `ms`, `s`, `min` or `h`; `0` is no limit) ends a statement that runs longer with the error "canceling statement due to statement timeout" (the time is checked inside scans, joins and repeated sub-queries, so a statement can overrun by a little). Like the time zone, parameters belong to the open database and are not undone by `ROLLBACK`.

## Time zone

```sql
SET TIME ZONE 'Europe/Paris';
SET TIME ZONE '+05:30';
SET TIMEZONE TO 'America/New_York';
SET TIME ZONE LOCAL;          -- back to UTC, the default
SHOW TIME ZONE;               -- or SHOW timezone
```

The session zone decides how a timestamp written without an offset is read, and on which wall clock a `TIMESTAMPTZ` is shown. The stored values do not change. See [types/timestamptz.md](../types/timestamptz.md). `SessionTimeZone()` returns the current setting.

From Rust: `db.set_time_zone("Europe/Paris")?` and `db.time_zone()`. In the shell: `.timezone` shows it and `.timezone Europe/Paris` sets it. An unknown zone name is an error and leaves the setting alone.

## Query optimizer

The [optimizer](../optimizer/README.md) is on by default. `db.set_optimizer(false)` in Rust, or `.optimizer off` in the shell, runs queries exactly as written. The answers are the same; use it to compare plans and timings.

## EXPLAIN

`EXPLAIN query` runs a query and returns its plan instead of its rows: see [../optimizer/explain.md](../optimizer/explain.md).
