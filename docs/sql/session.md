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

About seventy of PostgreSQL's parameter names are known (the planner switches `enable_*`, `work_mem`, `search_path`, `statement_timeout`, `client_min_messages`, `standard_conforming_strings`, and so on), each with its default. Most **only remember** their value: nothing consults `work_mem` or `enable_seqscan`. Two have an effect: `timezone` (the same as `SET TIME ZONE`, below) and `optimizer` (`on` or `off`, the query optimizer; also `Database::set_optimizer`). A name that is not known and has no dot is an error ("unrecognized configuration parameter"), except in MySQL's spellings: `SET GLOBAL name = v`, `SET SESSION name = v`, `SET @@session.name := v` and the like keep the value of a server variable this engine has no use for (as `mysql.name`), and `@@global.component.name` reads it back, and so is a switch given something other than a truth value. `SET LOCAL` is the same as `SET` (there is no per-transaction state), and `SET TRANSACTION ...`, `SET SESSION CHARACTERISTICS ...` and `SET ROLE ...` are read and ignored: there is one user and one isolation level. A third has an effect too: **`statement_timeout`** (milliseconds, or a number with `ms`, `s`, `min` or `h`; `0` is no limit) ends a statement that runs longer with the error "canceling statement due to statement timeout" (the time is checked inside scans, joins and repeated sub-queries, so a statement can overrun by a little; under a limit, a join result of 16 million values (rows times columns) ends the statement at once, as the timeout would). Like the time zone, parameters belong to the open database and are not undone by `ROLLBACK`.

Five more parameters, not PostgreSQL's, switch a reading of SQL to what other servers do. All are `on` by default, except `lenient_types` and `strict_types`, which are `off`, and all can be `RESET`:

| Parameter | Default | When changed |
| --- | --- | --- |
| `integer_division` | `on` | `off`: `/` of two whole numbers is exact, as in Oracle and MySQL (`7 / 2` is 3.5), where `on` drops the fraction ([../types/arithmetic.md](../types/arithmetic.md)) |
| `round_half_even` | `on` | `off`: `Round` sends a tie away from zero (`Round(2.125, 2)` is 2.13), where `on` sends it to the even digit ([../functions/numeric.md](../functions/numeric.md)) |
| `cast_truncates` | `on` | `off`: `CAST(x AS INTEGER)` of a number with a fraction rounds, ties to even, as PostgreSQL's casts do, where `on` drops the fraction ([../functions/conversion.md](../functions/conversion.md)) |
| `strict_types` | `off` | `on`: a column made with a size gets a `CHECK` that keeps its values to it: `CHAR(n)`, `VARCHAR(n)` and `CHARACTER VARYING(n)` to `n` characters (spaces after them are allowed, as PostgreSQL cuts them off), `NUMERIC(p, s)` to values below 10^(p-s) (`s` may be negative, or larger than `p`), `SMALLINT`/`INT2` to 16 bits and `INT`/`INTEGER`/`INT4` to 32. The sizes are checked like any `CHECK`, on `INSERT` and `UPDATE`, and stay with the table when the setting is turned off. Tables made before keep their (absent) checks; `BIGINT` and `TEXT` have no size to keep, and a `NUMERIC` scale does not round the value |
| `lenient_types` | `off` | `on`: a type name Accessor does not know (`txid_snapshot`, `cstring`, `point`), and an array of any type (`int[]`), is kept as `TEXT` instead of being refused, in a column, a function parameter or a cast, so that a table or function written for PostgreSQL can be made and the statements after it can run. The values are text, with no array or point operators. A definition made this way is read the same way when the database is opened again only if the parameter is on then |

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
