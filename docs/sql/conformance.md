# ASQL and the SQL standard

**ASQL does not implement the whole of the latest SQL standard (ISO/IEC 9075:2023).** No small embedded engine does; the standard is some twenty-thousand pages in nine parts, and most of it concerns client/server systems, procedural languages, XML and JSON, and administration. ASQL implements a large, useful subset of the core query and data-definition language (the part most people mean by "SQL"), together with the Access conventions it grew from. This page lists what is in, what is different, and what is missing, so that you know what to expect.

The names of the standard's features here are descriptive; they are not the feature codes of the standard, and this is a statement of what ASQL does, not a certified conformance claim.

## Implemented from the standard

| Area | Implemented |
| --- | --- |
| Queries | `SELECT` with `DISTINCT`/`ALL`, `WHERE`, `GROUP BY`, `HAVING`, `ORDER BY`; derived tables; `VALUES` as a table; `WITH` and `WITH RECURSIVE`; `UNION`, `INTERSECT`, `EXCEPT` with `ALL`/`DISTINCT`; `OFFSET ... ROWS` and `FETCH FIRST/NEXT ... ROWS ONLY`; `NULLS FIRST/LAST`; row value comparisons |
| Joins | inner, left, right, full outer and cross joins; `ON`, `USING`, `NATURAL`; parenthesised joins; `LATERAL`; table functions; `PIVOT` and `UNPIVOT` |
| Sub-queries | scalar, `IN`, `EXISTS`, `ANY`/`SOME`/`ALL`, derived tables, correlated sub-queries (in `SELECT`, `WHERE`, `GROUP BY`, `HAVING` and `ORDER BY`, and in `UPDATE`/`DELETE`) |
| Grouping | aggregates with `DISTINCT` and `FILTER`; `ROLLUP`, `CUBE`, `GROUPING SETS`, `GROUPING()` |
| Aggregates | `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `EVERY`, `ANY_VALUE`, `STDDEV_POP`, `STDDEV_SAMP`, `VAR_POP`, `VAR_SAMP`, `STRING_AGG`/`LISTAGG` (without `WITHIN GROUP`) |
| Window functions | `ROW_NUMBER`, `RANK`, `DENSE_RANK`, `PERCENT_RANK`, `CUME_DIST`, `NTILE`, `LAG`, `LEAD`, `FIRST_VALUE`, `LAST_VALUE`, `NTH_VALUE`, aggregates `OVER`, `PARTITION BY`, `ORDER BY`, named windows, `ROWS` frames and `RANGE` frames with or without an offset |
| Predicates | `SIMILAR TO`, comparisons, `BETWEEN` (`SYMMETRIC`/`ASYMMETRIC`), `IN`, `LIKE ... ESCAPE`, `IS [NOT] NULL`, `IS [NOT] TRUE/FALSE/UNKNOWN`, `IS [NOT] DISTINCT FROM`, `EXISTS`, quantified comparisons |
| Expressions | searched and simple `CASE`, `CAST`, `COALESCE`, `NULLIF`, `EXTRACT`, `POSITION`, `SUBSTRING ... FROM ... FOR`, `TRIM` with `LEADING`/`TRAILING`/`BOTH`, `CHAR_LENGTH`, `OCTET_LENGTH`, `UPPER`, `LOWER`, `ABS`, `MOD`, `POWER`, `SQRT`, `EXP`, `LN`, `LOG10`, `FLOOR`, `CEIL`, trigonometric functions, `CURRENT_DATE`, `CURRENT_TIMESTAMP`, `LOCALTIMESTAMP`, typed date literals |
| Data changes | `INSERT` (values, query, `DEFAULT VALUES`), `UPDATE`, `DELETE`, `MERGE` (matched update/delete, not-matched insert), `TRUNCATE TABLE` |
| Definition | `CREATE`/`ALTER`/`DROP TABLE` (including `RENAME TO`, `SET`/`DROP DEFAULT`, `SET`/`DROP NOT NULL`); `CREATE OR REPLACE VIEW`/`TRIGGER`; `PRIMARY KEY`, `UNIQUE`, `NOT NULL`, `DEFAULT`, `CHECK`, `FOREIGN KEY` with `ON DELETE`/`ON UPDATE` `RESTRICT`/`CASCADE`/`SET NULL`; identity columns (`GENERATED ... AS IDENTITY`); generated columns (`GENERATED ALWAYS AS (expr)`); `IF [NOT] EXISTS`; `CREATE`/`DROP INDEX`; `CREATE`/`DROP VIEW`; `CREATE`/`DROP TRIGGER`; `CREATE`/`ALTER`/`DROP SEQUENCE` with `NEXT VALUE FOR`; `DEFERRABLE` foreign keys with `SET CONSTRAINTS`; `SET DEFAULT` and `NO ACTION` referential actions |
| Triggers | `BEFORE`, `AFTER` and `INSTEAD OF`; `FOR EACH ROW` and `FOR EACH STATEMENT`; several events in one trigger; `UPDATE OF column`; `WHEN` |
| JSON | a `JSON` type; `->`, `->>`; value, query, set, remove, merge-patch, contains, object/array construction, `JSON_ARRAYAGG`, `JSON_EACH` (a simplified form of SQL/JSON, with a limited path language) |
| Routines | `CREATE FUNCTION` with an expression body, `CREATE PROCEDURE` with `DECLARE`, `SET`, `IF`, `WHILE`, `LEAVE`, `RETURN`; `CALL`; PostgreSQL-style `LANGUAGE sql` and PL/pgSQL functions |
| Transactions | `BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `ROLLBACK TO SAVEPOINT`, `RELEASE SAVEPOINT`; transactional DDL |
| Types | exact and approximate numerics, character strings, booleans, date-times, `TIMESTAMP WITH TIME ZONE`, `INTERVAL`, with the standard's spellings (`VARCHAR`, `NUMERIC`, `DOUBLE PRECISION`, `BOOLEAN`, `TIMESTAMP`, ...) |

## How ASQL differs from the standard

| Behaviour | ASQL | Standard |
| --- | --- | --- |
| Double quotes | make a *string* (an Access habit); use `[name]` for a delimited identifier | make a delimited identifier |
| Text comparison, `LIKE`, sorting | ignore case | depends on the collation, usually case-sensitive |
| `LIKE` wildcards | `*`, `?`, `#` and sets as well as `%` and `_` | `%` and `_` |
| Text concatenation | `&` and `\|\|`; NULL counts as empty | `\|\|`; NULL gives NULL |
| Booleans | also written `YES` and `NO`; a number is true when not zero | `TRUE`/`FALSE`/`UNKNOWN` only |
| Rounding | `ROUND` and `CAST ... AS DECIMAL(p, s)` round halves to the even digit; `CAST` to an integer drops the fraction, while `CInt` rounds | implementation-defined (usually half away from zero; `CAST` to an integer rounds in PostgreSQL, truncates in SQLite) |
| Columns in a grouped query | a column that is not grouped or aggregated takes its value from the first row of the group | an error |
| Division | `/` drops the fraction between whole numbers and divides in floating point otherwise; `DIV` is exact; division by zero is NULL | `/` on integers is implementation-defined; division by zero is an error (SQLSTATE 22012) |
| Type sizes | `VARCHAR(50)` and `NUMERIC(10, 2)` are accepted but sizes and scales are not enforced | enforced |
| `TIMESTAMP WITH TIME ZONE` | stored as an instant, shown in the session zone (set with `SET TIME ZONE`, not per column); zones come from the IANA database | per-session zone, `TIME WITH TIME ZONE` is a distinct type |
| `NULL` ordering | NULL sorts first ascending, last descending unless `NULLS FIRST`/`NULLS LAST` is given | implementation-defined, with `NULLS FIRST/LAST` |
| Quantified comparisons | computed from the minimum and maximum; NULLs in the set are ignored | NULLs make the result unknown |
| `CHECK` constraints | stored as a pair of `BEFORE` triggers | a constraint object |
| `TRUNCATE` | same as `DELETE` without a `WHERE` (triggers fire, counters are kept) | does not fire row triggers; `RESTART IDENTITY` option |
| `GENERATED ALWAYS AS IDENTITY` | the same as `BY DEFAULT`: a supplied value is accepted | a supplied value is refused |
| Views | simple views take `INSERT`/`UPDATE`/`DELETE` (with `WITH CHECK OPTION`); the rest are read-only; `CREATE VIEW v (cols) AS` names columns; parameters through `CREATE QUERY` | updatable views by a fuller set of rules; `WITH LOCAL/CASCADED CHECK OPTION` |
| Set operators | `MINUS` is accepted as `EXCEPT` | not standard |
| Concurrency | a database file is open once (a file lock, [locking](../extern/locking.md)); connections in that process overlap their transactions under snapshot isolation, with first-committer-wins conflicts ([concurrency](../concurrency.md)) | many sessions with isolation levels |

## Not implemented

These parts of the standard are not available. Using one gives a syntax error or a message saying it is not supported.

### Types and values

* `INTERVAL` is a single type (months, days, seconds), not the standard's separate year-month and day-time classes with qualifiers such as `INTERVAL DAY(3) TO SECOND(2)`; `OVERLAPS`; `TIME WITH TIME ZONE` as a type of its own (it is read as `TIMESTAMPTZ`); sub-second precision.
* `BLOB`, `BINARY`/`VARBINARY` and binary literals.
* Collection and structured types: `ARRAY`, `MULTISET`, `ROW`, `REF`, composite and distinct types. `CREATE DOMAIN` and `CREATE TYPE ... AS ENUM` exist in a reduced form ([schemas-and-types.md](schemas-and-types.md)).
* The `XML` type and SQL/XML. SQL/JSON is only partly there: there is no `IS JSON` predicate (use `JSON_VALID`), no `JSON_TABLE`, no `JSON_EXISTS`, and the path language has only keys, positions and `$` (no filters, wildcards or recursive descent).
* Row values in `SELECT` lists and as sub-query results (`(a, b) = (SELECT x, y ...)`); row comparisons and `IN` work. The `UNIQUE` and `MATCH` predicates.
* `LIKE_REGEX` and `NORMALIZE` (`SIMILAR TO` is there, and the regular-expression functions `REGEXP_LIKE`, `REGEXP_REPLACE`, `REGEXP_SUBSTR` and `REGEXP_COUNT` that other systems have).
* `CURRENT_USER`, `SESSION_USER` and the other user functions (a role is chosen with `SET ROLE`, but there is no log-in).

### Queries

* `GROUPS` frames, frame `EXCLUDE`, `IGNORE NULLS`. (Named windows and `RANGE` offsets are supported.)
* `FETCH ... WITH TIES`.
* Table-valued functions other than `GENERATE_SERIES` and `SPLIT_STRING` (`UNNEST`, `JSON_TABLE`); `TABLE name` as a query; `TABLESAMPLE`.
* Ordered-set and hypothetical-set aggregates (`WITHIN GROUP`: `PERCENTILE_CONT`, `LISTAGG ... WITHIN GROUP`); `ARRAY_AGG`; `GROUP BY DISTINCT`.
* `SEARCH` and `CYCLE` clauses of recursive queries.
* Row pattern recognition (`MATCH_RECOGNIZE`), polymorphic table functions, property graph queries (SQL/PGQ).
* Correlated sub-queries in join conditions and `INSERT ... VALUES`; in a grouped query they may read only the grouping columns.
* Cursors (`DECLARE CURSOR`, `FETCH`, positioned update and delete), `EXECUTE IMMEDIATE`, embedded SQL. (`PREPARE`, `EXEC` and `DEALLOCATE` exist, with `@name` parameters rather than `?` or `$1`.)

### Definitions and data changes

* Catalogs, and schemas as namespaces: `CREATE SCHEMA` and `schema.table` exist, but a schema is a prefix of the table's name with no search path ([schemas-and-types.md](schemas-and-types.md)). `INFORMATION_SCHEMA` exists with the views listed in [information-schema.md](information-schema.md), which is the one schema, with no `catalog` or `schema` columns; the standard's views for privileges, domains, collations and so on are absent.
* `CREATE ASSERTION`, collations and character sets (`COLLATE`). `CREATE DOMAIN` is there, with its rules copied into the columns that use it.
* Table inheritance and declarative partitioning (`INHERITS`, `PARTITION BY`) exist as extensions, without partition pruning ([inheritance-and-partitioning.md](inheritance-and-partitioning.md)).
* `CREATE OR REPLACE TABLE`, `CREATE TABLE ... AS`, `CREATE TABLE ... LIKE`, temporary tables, materialised views, recursive views, views updatable by the standard's fuller rules (joins, aggregates), `WITH LOCAL/CASCADED CHECK OPTION`.
* `ALTER TABLE` actions `ADD PRIMARY KEY` and `ADD UNIQUE` (use `CREATE UNIQUE INDEX`).
* Deferrable `UNIQUE`, `PRIMARY KEY` and `CHECK` constraints (only foreign keys can be deferred); `MATCH FULL/PARTIAL`; sequence options `AS type` and `CACHE` are accepted and ignored; `OWNED BY`.
* Temporal tables (`PERIOD FOR`, `WITH SYSTEM VERSIONING`).
* `INSERT ... OVERRIDING`. `ON CONFLICT` (a PostgreSQL/SQLite spelling) is available instead of the standard `MERGE` for the common upsert. `RETURNING` (a common extension, not in the standard) is available; the standard's data-change delta tables (`SELECT ... FROM FINAL TABLE (INSERT ...)`) are not.
* Report writers are outside the standard; `CREATE REPORT` is an extension (see below).

### Routines, triggers and administration

* The rest of SQL/PSM: `REPEAT`, `ITERATE`, cursors, condition handlers (`DECLARE ... HANDLER`), `INOUT` parameters, table functions (`RETURNS TABLE`), overloading, `SIGNAL`/`RESIGNAL`, external routines. `RAISE` stops a procedure. Functions with statement bodies exist only in the PostgreSQL form (`LANGUAGE plpgsql`), which also has `LOOP`, `FOR i IN a..b` and `CASE` statements ([routines.md](routines.md#postgresql-style-functions-and-procedures)).
* Triggers: `REFERENCING OLD/NEW TABLE` transition tables, trigger functions on `INSTEAD OF`, `INSTEAD OF` on tables, statement triggers that fire when no row changed, `SET NEW.col = ...` in `BEFORE` triggers, procedural trigger bodies.
* Privileges are limited to the table rights `SELECT`, `INSERT`, `UPDATE` and `DELETE`, checked for the role chosen with `SET ROLE` ([roles-and-privileges.md](roles-and-privileges.md)); there are no owners, no `WITH GRANT OPTION`, no row-level security; `SET TRANSACTION` isolation levels are read and ignored.
* The other parts of the standard: client interface (SQL/CLI), foreign data (SQL/MED), Java routines (SQL/JRT), XML (SQL/XML), multidimensional arrays (SQL/MDA), property graphs (SQL/PGQ).

## Extensions to the standard

* `ON CONFLICT`, `REPLACE INTO`, `INSERT OR IGNORE`, `RETURNING`, `REGEXP_*`, `LPAD`/`RPAD`, `TRUNC`, `RANDOM`, `INITCAP`.
* Access-style syntax: `[bracketed names]`, `TOP n`, `&` concatenation, `#date#` literals, `IIf`, `Nz`, `\` and `MOD`, `First`/`Last`, `DateAdd`/`DateDiff`/`DateSerial`, `Format`, `DELETE *`, and crosstab queries (`TRANSFORM … PIVOT`; standard SQL has no pivot clause).
* A numeric tower with exact arithmetic: `BIGNUM`, `DECIMAL`, `CURRENCY` and `RATIONAL` ([see the README](../../README.md#numbers)).
* `CALCULATED` columns, parameterised saved queries with `EXEC`, `RAISE` in triggers, `SELECT` in trigger bodies, `LASTVAL`/`SETVAL`.
* `RETURNING` on `INSERT`, `UPDATE`, `DELETE` and `MERGE`, `PREPARE`/`EXEC`, [reports](reports.md) (`CREATE REPORT`), `LASTVAL`/`SETVAL`.
* `EXPLAIN` and the [query optimizer](../optimizer/README.md).

## Differences found by sqllogictest

Running SQLite's [sqllogictest](../extern/testing.md#sqllogictest) over every file of the suite (622 files, 5.94 million records) passes **all but two records**. Both are the same difference, and it is on purpose:

* **Simple views can be updated.** The suite expects `DELETE FROM view1` and `UPDATE view1 SET ...` to fail, as they do in SQLite, where views are read-only. ASQL accepts them for [simple views](data-changes.md).

The first runs found more, which were fixed rather than excused: `/` now divides like SQLite does (whole numbers truncate; `DIV` is the exact operator), division by zero gives NULL, `ALL` is accepted inside aggregates, `IN ()` and `X'..'` literals are read, `CREATE TRIGGER` without a timing means `BEFORE`, `DROP INDEX name` and `REINDEX` exist, a column that is not grouped may be selected in a grouped query, and `CAST(x AS INTEGER)` truncates. Those that remain different from the standard are in the table above.

Statements about objects that only a server has are read and **ignored**: `CREATE`/`ALTER`/`DROP` of `TABLESPACE`, `DATABASE`, `PUBLICATION`, `SUBSCRIPTION`, `SERVER`, `FOREIGN DATA WRAPPER`/`FOREIGN TABLE`, `USER MAPPING`, `EVENT TRIGGER`, `STATISTICS`, `COLLATION`, `CONVERSION`, `TRANSFORM`, `LANGUAGE`, `TEXT SEARCH ...`, `ACCESS METHOD`, `OPERATOR`, `CAST`, `OWNED BY` and `DEFAULT PRIVILEGES`; and the `ALTER TABLE` actions `ENABLE/DISABLE/FORCE ROW LEVEL SECURITY`, `OWNER TO`, `SET (...)`, `RESET (...)`, `REPLICA IDENTITY`, `CLUSTER ON`, `SET ACCESS METHOD/TABLESPACE/LOGGED/UNLOGGED`, `INHERIT`, `VALIDATE CONSTRAINT`, and `ALTER COLUMN ... SET STATISTICS/STORAGE/COMPRESSION`. Nothing is recorded, so a script that depends on what they do (an operator defined with `CREATE OPERATOR`, a cast with `CREATE CAST`) still fails where it uses the result. `CREATE MATERIALIZED VIEW v AS q` makes a table filled once from `q` (there is no `REFRESH`).

## Differences found by the PostgreSQL regression suite

See [testing](../extern/testing.md#the-postgresql-regression-suite).
