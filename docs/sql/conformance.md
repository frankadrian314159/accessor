# ASQL and the SQL standard

**ASQL does not implement the whole of the latest SQL standard (ISO/IEC 9075:2023).** No small embedded engine does; the standard is some twenty-thousand pages in nine parts, and most of it concerns client/server systems, procedural languages, XML and JSON, and administration. ASQL implements a large, useful subset of the core query and data-definition language (the part most people mean by "SQL"), together with the Access conventions it grew from. This page lists what is in, what is different, and what is missing, so that you know what to expect.

The names of the standard's features here are descriptive; they are not the feature codes of the standard, and this is a statement of what ASQL does, not a certified conformance claim.

## Implemented from the standard

| Area | Implemented |
| --- | --- |
| Queries | `SELECT` with `DISTINCT`/`ALL`, `WHERE`, `GROUP BY`, `HAVING`, `ORDER BY`; derived tables; `VALUES` as a table; `WITH` and `WITH RECURSIVE`; `UNION`, `INTERSECT`, `EXCEPT` with `ALL`/`DISTINCT`; `OFFSET ... ROWS` and `FETCH FIRST/NEXT ... ROWS ONLY` |
| Joins | inner, left, right, full outer and cross joins; `ON`, `USING`, `NATURAL`; parenthesised joins |
| Sub-queries | scalar, `IN`, `EXISTS`, `ANY`/`SOME`/`ALL`, derived tables, correlated sub-queries (in `SELECT`, `WHERE` and `ORDER BY` of ungrouped queries, and in `UPDATE`/`DELETE`) |
| Grouping | aggregates with `DISTINCT` and `FILTER`; `ROLLUP`, `CUBE`, `GROUPING SETS`, `GROUPING()` |
| Aggregates | `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `EVERY`, `ANY_VALUE`, `STDDEV_POP`, `STDDEV_SAMP`, `VAR_POP`, `VAR_SAMP`, `STRING_AGG`/`LISTAGG` (without `WITHIN GROUP`) |
| Window functions | `ROW_NUMBER`, `RANK`, `DENSE_RANK`, `PERCENT_RANK`, `CUME_DIST`, `NTILE`, `LAG`, `LEAD`, `FIRST_VALUE`, `LAST_VALUE`, `NTH_VALUE`, aggregates `OVER`, `PARTITION BY`, `ORDER BY`, `ROWS` frames and `RANGE` with `UNBOUNDED`/`CURRENT ROW` |
| Predicates | comparisons, `BETWEEN` (`SYMMETRIC`/`ASYMMETRIC`), `IN`, `LIKE ... ESCAPE`, `IS [NOT] NULL`, `IS [NOT] TRUE/FALSE/UNKNOWN`, `IS [NOT] DISTINCT FROM`, `EXISTS`, quantified comparisons |
| Expressions | searched and simple `CASE`, `CAST`, `COALESCE`, `NULLIF`, `EXTRACT`, `POSITION`, `SUBSTRING ... FROM ... FOR`, `TRIM` with `LEADING`/`TRAILING`/`BOTH`, `CHAR_LENGTH`, `OCTET_LENGTH`, `UPPER`, `LOWER`, `ABS`, `MOD`, `POWER`, `SQRT`, `EXP`, `LN`, `LOG10`, `FLOOR`, `CEIL`, trigonometric functions, `CURRENT_DATE`, `CURRENT_TIMESTAMP`, `LOCALTIMESTAMP`, typed date literals |
| Data changes | `INSERT` (values, query, `DEFAULT VALUES`), `UPDATE`, `DELETE`, `MERGE` (matched update/delete, not-matched insert), `TRUNCATE TABLE` |
| Definition | `CREATE`/`ALTER`/`DROP TABLE`; `PRIMARY KEY`, `UNIQUE`, `NOT NULL`, `DEFAULT`, `CHECK`, `FOREIGN KEY` with `ON DELETE`/`ON UPDATE` `RESTRICT`/`CASCADE`/`SET NULL`; identity columns (`GENERATED ... AS IDENTITY`); generated columns (`GENERATED ALWAYS AS (expr)`); `IF [NOT] EXISTS`; `CREATE`/`DROP INDEX`; `CREATE`/`DROP VIEW`; `CREATE`/`DROP TRIGGER` |
| Transactions | `BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `ROLLBACK TO SAVEPOINT`, `RELEASE SAVEPOINT`; transactional DDL |
| Types | exact and approximate numerics, character strings, booleans, date-times, with the standard's spellings (`VARCHAR`, `NUMERIC`, `DOUBLE PRECISION`, `BOOLEAN`, `TIMESTAMP`, ...) |

## How ASQL differs from the standard

| Behaviour | ASQL | Standard |
| --- | --- | --- |
| Double quotes | make a *string* (an Access habit); use `[name]` for a delimited identifier | make a delimited identifier |
| Text comparison, `LIKE`, sorting | ignore case | depends on the collation, usually case-sensitive |
| `LIKE` wildcards | `*`, `?`, `#` and sets as well as `%` and `_` | `%` and `_` |
| Text concatenation | `&` and `\|\|`; NULL counts as empty | `\|\|`; NULL gives NULL |
| Booleans | also written `YES` and `NO`; a number is true when not zero | `TRUE`/`FALSE`/`UNKNOWN` only |
| Rounding | `ROUND` and `CAST ... AS DECIMAL(p, s)` round halves to the even digit | implementation-defined (usually half away from zero) |
| Integer division | `/` is exact division giving a `DECIMAL`; use `\` for whole-number division | `/` on integers is implementation-defined |
| Type sizes | `VARCHAR(50)` and `NUMERIC(10, 2)` are accepted but sizes and scales are not enforced | enforced |
| `TIMESTAMP WITH TIME ZONE` | accepted; no time zone is stored (whole seconds, one common zone) | zone-aware |
| `NULL` ordering | NULL sorts first ascending, last descending; no `NULLS FIRST/LAST` | implementation-defined, with `NULLS FIRST/LAST` |
| Quantified comparisons | computed from the minimum and maximum; NULLs in the set are ignored | NULLs make the result unknown |
| `CHECK` constraints | stored as a pair of `BEFORE` triggers | a constraint object |
| `TRUNCATE` | same as `DELETE` without a `WHERE` (triggers fire, counters are kept) | does not fire row triggers; `RESTART IDENTITY` option |
| `GENERATED ALWAYS AS IDENTITY` | the same as `BY DEFAULT`: a supplied value is accepted | a supplied value is refused |
| Views | read-only, with no column list; may take parameters through `CREATE QUERY` | updatable views, column lists |
| Set operators | `MINUS` is accepted as `EXCEPT` | not standard |
| Concurrency | one connection per database file | many sessions with isolation levels |

## Not implemented

These parts of the standard are not available. Using one gives a syntax error or a message saying it is not supported.

### Types and values

* `INTERVAL` types and interval arithmetic; `OVERLAPS`; `AT TIME ZONE`.
* `BLOB`, `BINARY`/`VARBINARY` and binary literals.
* Collection and structured types: `ARRAY`, `MULTISET`, `ROW`, `REF`, user-defined and distinct types (`CREATE TYPE`).
* `XML` and `JSON` types, the SQL/XML and SQL/JSON functions and path language.
* Row value constructors and row comparisons such as `(a, b) = (1, 2)` and `(a, b) IN (...)`; the `UNIQUE` and `MATCH` predicates.
* Regular-expression predicates: `SIMILAR TO` and `LIKE_REGEX`; `OVERLAY`, `TRANSLATE`, `NORMALIZE`.
* `CURRENT_USER`, `SESSION_USER` and the other user functions (there are no users).

### Queries

* Named windows (`WINDOW w AS (...)`), `GROUPS` frames, frame `EXCLUDE`, `RANGE` frames with a numeric offset, `IGNORE NULLS`.
* `FETCH ... WITH TIES`, `NULLS FIRST` / `NULLS LAST`.
* `LATERAL` joins and table-valued functions (`UNNEST`, `JSON_TABLE`); `TABLE name` as a query; `TABLESAMPLE`.
* Ordered-set and hypothetical-set aggregates (`WITHIN GROUP`: `PERCENTILE_CONT`, `LISTAGG ... WITHIN GROUP`); `ARRAY_AGG`; `GROUP BY DISTINCT`.
* `SEARCH` and `CYCLE` clauses of recursive queries.
* Row pattern recognition (`MATCH_RECOGNIZE`), polymorphic table functions, property graph queries (SQL/PGQ).
* Correlated sub-queries in `GROUP BY` queries, `HAVING`, join conditions and `INSERT ... VALUES`.
* Cursors (`DECLARE CURSOR`, `FETCH`, positioned update and delete), dynamic SQL (`PREPARE`, `EXECUTE IMMEDIATE`), embedded SQL.

### Definitions and data changes

* Schemas, catalogs and qualified names (`schema.table`); `CREATE SCHEMA`; `INFORMATION_SCHEMA`.
* `CREATE DOMAIN`, `CREATE SEQUENCE` and `NEXT VALUE FOR`, `CREATE ASSERTION`, collations and character sets (`COLLATE`).
* `CREATE OR REPLACE`, `CREATE TABLE ... AS`, `CREATE TABLE ... LIKE`, temporary tables, materialised views, recursive views, updatable views, `WITH CHECK OPTION`.
* `ALTER TABLE` actions `SET DEFAULT`, `DROP DEFAULT`, `SET NOT NULL`, `DROP NOT NULL`, `ADD PRIMARY KEY`, `ADD UNIQUE`, and renaming a table.
* Deferrable constraints (`DEFERRABLE`, `SET CONSTRAINTS`), `MATCH FULL/PARTIAL`, `NO ACTION` and `SET DEFAULT` referential actions.
* Temporal tables (`PERIOD FOR`, `WITH SYSTEM VERSIONING`).
* `INSERT ... OVERRIDING`.

### Routines, triggers and administration

* `CREATE FUNCTION`, `CREATE PROCEDURE` and the procedural language (SQL/PSM): variables, `IF`, loops, handlers. The substitutes are parameterised saved queries (`CREATE QUERY`, `EXEC`) and triggers.
* Triggers: statement-level triggers, `INSTEAD OF`, `REFERENCING OLD/NEW TABLE`, `UPDATE OF column`, one trigger for several events, trigger bodies other than `INSERT`, `UPDATE`, `DELETE` and `RAISE`.
* Privileges and roles (`GRANT`, `REVOKE`, `CREATE ROLE`), `SET TRANSACTION` isolation levels, `SET SESSION` statements.
* The other parts of the standard: client interface (SQL/CLI), foreign data (SQL/MED), Java routines (SQL/JRT), XML (SQL/XML), multidimensional arrays (SQL/MDA), property graphs (SQL/PGQ).

## Extensions to the standard

* Access-style syntax: `[bracketed names]`, `TOP n`, `&` concatenation, `#date#` literals, `IIf`, `Nz`, `\` and `MOD`, `First`/`Last`, `DateAdd`/`DateDiff`/`DateSerial`, `Format`, `DELETE *`, and crosstab queries (`TRANSFORM … PIVOT`; standard SQL has no pivot clause).
* A numeric tower with exact arithmetic: `BIGNUM`, `DECIMAL`, `CURRENCY` and `RATIONAL` ([see the README](../../README.md#numbers)).
* `CALCULATED` columns, parameterised saved queries with `EXEC`, `RAISE` in triggers.
* `EXPLAIN` and the [query optimizer](../optimizer/README.md).
