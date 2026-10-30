# SQL dialects

A session speaks one of three dialects. `asql` is the default and is what the rest of this documentation describes; `postgresql` and `mysql` change how the text is read and how values are shown, so that scripts written for those servers behave as they expect.

| | `asql` | `postgresql` | `mysql` |
| --- | --- | --- | --- |
| `# ...` | a comment, unless it opens a `#date#` literal | not a comment | a comment to the end of the line |
| `#2024-01-02#` | a date literal | not read | not read |
| a boolean as text | `Yes` / `No` | `true` / `false` (`t` / `f` in the shell's result tables) | `1` / `0` |
| a table with no columns | refused | `CREATE TABLE t ()` is allowed | refused |
| `tableoid` | not a column | the name of the table a row was read from (also through a parent or a partitioned table) | not a column |
| `NOT NULL` on a generated column | refused | allowed (a `DEFAULT` stands for it in `INSERT ... VALUES`) | refused |
| arrays | `ARRAY[...]`, `int[]`, `a[i]`, `ANY (array)`, `@>`, `<@`, `&&` | the same | not read: MySQL's arrays are JSON (`MEMBER OF`, `JSON_ARRAY_APPEND`, `JSON_OVERLAPS`) |
| arrays | `ARRAY[...]`, `int[]`, `a[i]`, `ANY (array)`, `@>`, `<@`, `&&` | the same | not read: MySQL's arrays are JSON (`MEMBER OF`, `JSON_ARRAY_APPEND`, `JSON_OVERLAPS`) |
| comparing booleans | Access's: Yes is -1, so `TRUE > FALSE` is No | `FALSE < TRUE` | `FALSE < TRUE` |

## Choosing one

```
accessor --postgresql mydb.adb
accessor --mysql -f script.sql
```

`--asql`, `--postgresql` and `--mysql` are options of the shell (the last one given counts). From SQL, `SET dialect = 'postgresql'` (`asql`, `postgresql` or `mysql`; `postgres`, `pg` and `access` are accepted too), `SHOW dialect` and `RESET dialect`; from Rust, `Database::set_dialect(Dialect::Mysql)` and `Database::dialect()`. `RESET ALL` leaves the dialect as it is.

A script is read as a whole before it runs, so `SET dialect` changes how the statements of the *next* call are read, not the rest of the same script: set the dialect first (with a flag, or a call of its own).

The dialect is kept for the session and is not stored in the file. The PostgreSQL regression runner (`accessor-pgreg`) and the MySQL test runner (`accessor-mtr`) run in their dialects; see [../extern/testing.md](../extern/testing.md).

Many MySQL and PostgreSQL forms (functions, `SHOW`, `ON DUPLICATE KEY UPDATE`, `::` casts...) are read in every dialect, because they cannot be mistaken for anything in Access SQL; the dialect only decides what the spellings that differ mean. See [mysql.md](mysql.md) and [conformance.md](conformance.md).
