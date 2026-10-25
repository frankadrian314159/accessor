# Session settings

A few settings belong to the open database, not to the file. They last until changed or until the database is closed, and are not undone by `ROLLBACK`.

## Time zone

```sql
SET TIME ZONE 'Europe/Paris';
SET TIME ZONE '+05:30';
SET TIMEZONE TO 'America/New_York';
SET TIME ZONE LOCAL;          -- back to UTC, the default
```

The session zone decides how a timestamp written without an offset is read, and on which wall clock a `TIMESTAMPTZ` is shown. The stored values do not change. See [types/timestamptz.md](../types/timestamptz.md). `SessionTimeZone()` returns the current setting.

From Rust: `db.set_time_zone("Europe/Paris")?` and `db.time_zone()`. In the shell: `.timezone` shows it and `.timezone Europe/Paris` sets it. An unknown zone name is an error and leaves the setting alone.

## Query optimizer

The [optimizer](../optimizer/README.md) is on by default. `db.set_optimizer(false)` in Rust, or `.optimizer off` in the shell, runs queries exactly as written. The answers are the same; use it to compare plans and timings.

## EXPLAIN

`EXPLAIN query` runs a query and returns its plan instead of its rows: see [../optimizer/explain.md](../optimizer/explain.md).
