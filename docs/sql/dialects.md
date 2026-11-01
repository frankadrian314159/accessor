# SQL dialects

A session speaks one of three dialects. `asql` is the default and is what the rest of this documentation describes; `postgresql` and `mysql` change how the text is read and how values are shown, so that scripts written for those servers behave as they expect.

| | `asql` | `postgresql` | `mysql` |
| --- | --- | --- | --- |
| `# ...` | a comment, unless it opens a `#date#` literal | not a comment | a comment to the end of the line |
| `#2024-01-02#` | a date literal | not read | not read |
| a boolean as text | `Yes` / `No` | `true` / `false` (`t` / `f` in the shell's result tables) | `1` / `0` |
| a table with no columns | refused | `CREATE TABLE t ()` is allowed | refused |
| `tableoid` | not a column | the name of the table a row was read from (also through a parent or a partitioned table); also in a `CHECK` and in a trigger's `WHEN` (`NEW.tableoid`). A table's name alone in a query (`SELECT i FROM t i`, `LATERAL (VALUES (i))`) is its row written as text, `(1,2)` | not a column |
| `NOT NULL` on a generated column | refused | allowed (a `DEFAULT` stands for it in `INSERT ... VALUES`) | refused |
| arrays | `ARRAY[...]`, `int[]`, `a[i]`, `ANY (array)`, `@>`, `<@`, `ARRAY_OVERLAPS(a, b)` (`&&` is a slip for `&` here) | the same, with `a && b` for overlap | not read: MySQL's arrays are JSON (`MEMBER OF`, `JSON_ARRAY_APPEND`, `JSON_OVERLAPS`) |
| comparing booleans | Access's: Yes is -1, so `TRUE > FALSE` is No | `FALSE < TRUE` | `FALSE < TRUE` |
| `&`, `#`, `\|/`, `\|\|/`, `@` | `&` joins text | `&` and `#` are bitwise AND and XOR on integers; `\|/ x` and `\|\|/ x` are the square and cube root, `@ x` the absolute value; `\|\|` joins text | `&` is bitwise AND |
| `E'..'`, `B'..'` | not read | `E'a\nb'` reads backslash escapes (`\n \t \r \b \f \\ \' \xHH \ooo \uXXXX \UXXXXXXXX`); `B'0101'` is the text of its digits (there is no bit-string type) | `b'..'` is a binary literal |
| `UPDATE .. FROM`, `DELETE .. USING` | multi-table forms of MySQL only | `UPDATE t SET .. FROM u WHERE ..` and `DELETE FROM t [AS x] USING u WHERE ..` (the target is not in the list); no `RETURNING` on the first | the MySQL multi-table forms |
| `UPDATE .. SET` | `col = v` | also `arr[i] = v` (`arr[i][j]` too) and `(a, b) = (x, y)`, `= ROW(x, y)`, `= (SELECT x, y ..)` | `col = v` |
| `INSERT INTO t VALUES (..)` with fewer values than columns | refused | the last columns get their defaults | refused |
| `ALTER COLUMN c TYPE t USING expr` | not read | the values are rewritten with the expression, then the column gets the type | not read |
| `WITH x AS (INSERT \| UPDATE \| DELETE .. RETURNING ..) SELECT ..` | not read | the statement runs first and the query reads the rows it returned | not read |
| `COPY` | not read | `COPY table [(cols)] \| (query) TO 'file' \| STDOUT` and `COPY table FROM 'file'`, as `text`, `csv` (`HEADER`, `DELIMITER`, `NULL`, `QUOTE`, `ESCAPE`, `FORCE_QUOTE`) or `json` (`TO` only); to `STDOUT` the lines are the rows of the result; `FROM STDIN`, `PROGRAM` and `binary` are refused | not read |
| `ALTER TYPE t ADD VALUE 'x' [BEFORE \| AFTER 'y']`, `RENAME VALUE` | not read | changes the enum for tables made afterwards (a table made earlier keeps the labels it was made with) | not read |
| `REINDEX`, `ALTER INDEX` | not read | read and ignored | not read |
| `CREATE FUNCTION .. LANGUAGE internal AS 'int4eq'` | not read | the server's function of that name; the comparison, arithmetic and `in`/`out` functions of the number, text, boolean and date types are known | not read |
| `ON CONFLICT (lower(a), b)`, `ON CONFLICT ON CONSTRAINT c` | not read | an expression in the target (or a constraint name) makes any conflict count | not read |
| `CREATE TRIGGER .. REFERENCING OLD \| NEW TABLE AS t` | not read | read and ignored: a trigger that reads the transition table fails when it fires | not read |
| `ALTER TABLE p ATTACH PARTITION c` | the columns must match in order | a child with the same columns in another order is put in the parent's order | the same as `asql` |
| `CREATE AGGREGATE name (type, ..) (SFUNC = f, STYPE = t [, FINALFUNC = g] [, INITCOND = 'v'])` | not read | the state starts as `INITCOND` (cast to `STYPE`), `f(state, args..)` runs for each row (after `DISTINCT` and `ORDER BY` inside the call), and `g(state)` gives the result; `f` and `g` are functions with an expression body, or a function of the server such as `int4pl` or `int8inc`; the old spelling (`BASETYPE`, `SFUNC1`, `STYPE1`, `INITCOND1`) and `(*)` are read, the other options are ignored; ordered-set and `VARIADIC` aggregates are refused; `DROP AGGREGATE` | not read |
| range types: `int4range`, `int8range`, `numrange`, `daterange`, `tsrange`, `tstzrange` | not read | a range is text in PostgreSQL's form (`[1,5)`, `(,7]`, `empty`), so a column of a range type is a text column. `int4range(1, 5, '[]')` (flags `[)` `[]` `(]` `()`, `NULL` for no bound) and `'[1,5]'::int4range` put the discrete types in the canonical `[)` form; a text literal stored in a column is kept as written. Functions: `lower`, `upper`, `isempty`, `lower_inc`, `upper_inc`, `lower_inf`, `upper_inf`, `range_merge`. Operators: `@>` and `<@` (range or element), `&&`, `<<`, `>>`, `&<`, `&>`, `-\|-`, and `+` `-` `*` for union, difference and intersection (an error where the result would not be one range). Also `range_contains`, `range_contained_by`, `range_overlaps`, `range_adjacent`, `range_before`, `range_after`, `range_overleft`, `range_overright`, `range_union`, `range_intersect`, `range_minus`. Multiranges (`int4multirange(r, ..)`, `'{[1,3),[5,7)}'::int4multirange`, `multirange(r)`) are text such as `{[1,3),[5,7)}`, kept sorted and joined, and take the same functions and operators as ranges, with `unnest` for their ranges. `CREATE TYPE r AS RANGE (SUBTYPE = t)` makes a range type with the constructor `r(a, b [, flags])` and the cast `::r` (no canonical form; the other options are ignored). Aggregates: `range_agg` (the multirange) and `range_intersect_agg`. Ranges sort as text, and `range_minus_multi` is not available | not read |

## Choosing one

```
accessor --postgresql mydb.adb
accessor --mysql -f script.sql
```

`--asql`, `--postgresql` and `--mysql` are options of the shell (the last one given counts). From SQL, `SET dialect = 'postgresql'` (`asql`, `postgresql` or `mysql`; `postgres`, `pg` and `access` are accepted too), `SHOW dialect` and `RESET dialect`; from Rust, `Database::set_dialect(Dialect::Mysql)` and `Database::dialect()`. `RESET ALL` leaves the dialect as it is.

A script is read as a whole before it runs, so `SET dialect` changes how the statements of the *next* call are read, not the rest of the same script: set the dialect first (with a flag, or a call of its own).

The dialect is kept for the session and is not stored in the file. The PostgreSQL regression runner (`accessor-pgreg`) and the MySQL test runner (`accessor-mtr`) run in their dialects; see [../extern/testing.md](../extern/testing.md).

Many MySQL and PostgreSQL forms (functions, `SHOW`, `ON DUPLICATE KEY UPDATE`, `::` casts...) are read in every dialect, because they cannot be mistaken for anything in Access SQL; the dialect only decides what the spellings that differ mean. See [mysql.md](mysql.md) and [conformance.md](conformance.md).
