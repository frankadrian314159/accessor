# The server

`accessor-server` serves one Accessor database to many clients over the network. It speaks PostgreSQL's wire protocol (version 3), so `psql`, JDBC, psycopg, node-postgres and the other PostgreSQL drivers connect to it unchanged. It is a separate program built on the same engine as the embedded library: the library, the command-line tool, the GUI and the C API are not touched by it, and a database file can be used embedded one day and served the next (only one process can have the file open at a time).

```
accessor-server [FILE.adb] [--listen ADDR:PORT] [--asql|--postgresql|--mysql]
                [--auth trust|password|md5|scram] [--user NAME:PASSWORD]... [--users-file FILE]
                [--max-connections N]
```

Defaults: no file (an in-memory database), `127.0.0.1:5432`, PostgreSQL mode, no password (`trust`); giving a user turns on `scram`. Stop it with Ctrl+C: connections are given a few seconds to finish and a file is checkpointed on the way out.

```
accessor-server shop.adb --listen 0.0.0.0:5432 --user app:s3cret
psql "host=localhost user=app dbname=shop"
```

## How it runs

Each connection gets a thread and its own `Connection` on one shared database, so clients overlap exactly as threads of an embedded program do: snapshot isolation, writers that meet on a row are told (`40001`), readers are never blocked by writers. See [the concurrency notes in `shared.rs`](../crates/accessor-core/src/shared.rs).

## What is there

* The handshake (SSL and GSS requests are declined), `ParameterStatus`, `BackendKeyData`.
* Password authentication: cleartext, MD5, SCRAM-SHA-256. A wrong or missing password is refused (`28P01`) and an unknown user is not told apart from a wrong password.
* **Simple queries**: several statements in one message, each answered with its own rows and tag (`SELECT 3`, `INSERT 0 2`, `CREATE TABLE`, `BEGIN`, ...); an empty query; a failing statement stops the rest.
* **Extended queries**: Parse, Bind, Describe, Execute, Close, Sync, Flush, named statements and portals, row limits on `Execute` (`PortalSuspended`), and the skip-to-Sync rule after an error. Parameters (`$1`...) are filled in as literals; text and binary parameters are read for the usual types.
* Results in **text or binary** format for booleans, integers, floats, `numeric`, `money`, text, `bytea`, dates, timestamps (with and without zone), intervals and JSON. The type OID of a column is taken from its first non-null value (text when there is none).
* Errors with SQLSTATE codes (`42P01` missing table, `42703` missing column, `42601` syntax, `23505` unique violation, `22012` division by zero, `40001` conflict, ...).
* **`psql`'s describe commands**: `\d`, `\d NAME` (columns, defaults, `not null`, indexes, primary and unique keys, foreign keys and what refers to the table), `\d+`, `\dt`, `\di`, `\dv`, `\ds`, `\dn`, `\du`, `\l`, `\dT`, `\df`, `\dx`, `\dp`. They work because the `pg_catalog` tables carry what psql reads (`pg_class`, `pg_index` and `pg_constraint` with PostgreSQL's names, `pg_attrdef`, `pg_am`, ...), the helpers it calls exist (`pg_table_is_visible`, `pg_get_userbyid`, `pg_get_indexdef`, `pg_get_constraintdef`, `pg_get_viewdef`, `pg_get_triggerdef`, `format_type`, `pg_get_expr`, `has_*_privilege`, ...), and the parser reads `OPERATOR(pg_catalog.~)`, `pg_catalog.` before a type or a table function, and `COLLATE pg_catalog.default`. A primary key index is `table_pkey`; the engine's own helper indexes for foreign keys are left out; a column shows its type as it was declared (`numeric(10,2)`, `character varying(40)`, `character(3)`), its default expression (`current_date`, `'n'::text`), and the `CHECK` constraints of the table are listed.
* **Settings belong to the connection.** The time zone, run-time parameters (`SET`, `set_config`), `@variables`, `PREPARE`d statements, cursors and `HANDLER`s are each connection's own, in the server and in the embedded `Connection` alike. A connection that has set nothing still reads alongside the others; one that has takes its own turn.
* **Column names as PostgreSQL gives them** (in PostgreSQL mode): `count`, `sum`, `max`, `upper`, `coalesce`, `case`, `exists`, `extract`, and `?column?` for an operator or a literal; a cast keeps the name of the column it casts.
* Cancel requests (taken between the statements of a message), a limit on connections (`53300`), and a graceful stop.

## What is not (yet)

* **TLS.** Connections are in the clear; run it on a trusted network or behind a tunnel or proxy.
* **`COPY`**, `LISTEN`/`NOTIFY`, and interrupting a statement while it runs.
* **Roles and permissions of the catalog** are not tied to logins: users come from `--user` / `--users-file`, and every logged-in user acts as the database owner.
* **Online backup and replication.** Durability is the engine's own (a checksummed write-ahead log plus snapshots, so committed work survives a crash of the server), but there is no way yet to copy a database while it is being served, and no standby. Copy the file while the server is stopped.
* **`\d` shows what the engine keeps.** A `CHECK` constraint is listed with the name the engine gave it (`ck_table_1`) when it was not named; `timestamp(3)`, `time(n)` and `bit(n)` modifiers are not kept; storage and statistics columns of `\d+` are the defaults.
* **Describing a statement before it is bound** runs it with `NULL` for every parameter when it only reads, to learn its columns; a client that prepares and describes a query in one step sees the types of that dry run.
