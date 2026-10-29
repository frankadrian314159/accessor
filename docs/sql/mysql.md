# MySQL syntax

Scripts and tests written for MySQL run on the engine when they stay within what is described here. This page lists what is read, and what it does; it is not MySQL's dialect in full.

## Names and quoting

`` `name` `` is a quoted identifier (two backticks inside stand for one), like `[name]`. `?` is the placeholder of a prepared statement (see below).

## Variables

**`sql_mode`.** `SET sql_mode = ''` (any value without `STRICT`) makes a text that is not a number, stored in a numeric column, become the number it starts with, or 0, as MySQL does without a warning being shown; a mode with `STRICT` (and the start of a session) refuses it. Only this conversion changes; out-of-range numbers and bad dates are still refused.

```sql
SET @a = 5, @b := @a + 1;           -- user variables
SELECT @a * @b;                     -- 30
SELECT COUNT(*) INTO @n FROM t;     -- also SELECT a, b INTO @x, @y ...
SET sql_mode = 'ANSI';              -- system variables: SET [GLOBAL | SESSION | PERSIST] name = value
SET @@session.sort_buffer_size = 32 * 1024;
SELECT @@sql_mode, @@global.autocommit;
SET NAMES utf8mb4;                  -- read and ignored (text is UTF-8)
```

User variables belong to the open database and last until it is closed; `ROLLBACK` does not undo them. A user variable that was never set is an **error** here (MySQL gives NULL), because `@name` is also the parameter of a saved query. System variables are the run-time parameters of [session.md](session.md): about 50 MySQL names have defaults (`sql_mode`, `autocommit`, `version`, `character_set_*`, `collation_*`, `foreign_key_checks`, `sort_buffer_size`, `group_concat_max_len`, ...) and any name that starts like a MySQL variable (`innodb_`, `optimizer_`, `max_`, `sql_`, `default_`, ...) is accepted and only remembered; none of them changes how the engine works. `@@name` reads the value as a number when it is one.

## CREATE TABLE

```sql
CREATE TABLE `t1` (
  `id` int(11) unsigned NOT NULL AUTO_INCREMENT,
  name varchar(20) CHARACTER SET utf8 COLLATE utf8_bin DEFAULT NULL COMMENT 'x',
  kind enum('a','b') NOT NULL DEFAULT 'a',
  created timestamp NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  PRIMARY KEY (`id`),
  UNIQUE KEY uname (name(10)),
  KEY k1 (kind, id DESC),
  INDEX (created)
) ENGINE=InnoDB AUTO_INCREMENT=5 DEFAULT CHARSET=utf8mb4 COMMENT='t';
```

* **Types:** `MEDIUMINT` and `YEAR` are integers; `TINYTEXT`, `MEDIUMTEXT`, the `BLOB` and `BINARY` families and the geometry types are text; `DEC` and `FIXED` are decimals. Display widths (`int(11)`) and `UNSIGNED`, `SIGNED`, `ZEROFILL` are read and ignored: an unsigned column accepts negative numbers. `ENUM('a','b')` is text with a check that the value is a label (compared as text, not by position); `SET(...)` is plain text.
* **Column attributes read and ignored:** `CHARACTER SET`, `CHARSET`, `COLLATE`, `COMMENT`, `ON UPDATE ...`, `STORAGE`, `COLUMN_FORMAT`, `VISIBLE`, `INVISIBLE`. `AUTO_INCREMENT` and `KEY` (primary key) work. `DEFAULT CURRENT_TIMESTAMP` (also `NOW()`, `LOCALTIMESTAMP`) on a date-time column is the time of the insert; any other default that is not a constant (`DEFAULT (uuid())`) is read and **not applied**.
* **Keys in the definition:** `PRIMARY KEY (cols)`, `UNIQUE [KEY] [name] (cols)`, `KEY | INDEX [name] (cols)`, `FULLTEXT` and `SPATIAL` (an ordinary index), with prefix lengths (`name(10)`, ignored), `ASC`/`DESC`, `USING BTREE`, `COMMENT`, and `CONSTRAINT [name]` in front. A `KEY` is made as an index after the table; its name is kept, or `table_cols_idx`. Indexes on expressions are refused.
* **Table options after `)`:** `ENGINE`, `DEFAULT CHARSET`, `CHARACTER SET`, `COLLATE`, `AUTO_INCREMENT` (not applied), `COMMENT`, `ROW_FORMAT`, `KEY_BLOCK_SIZE`, `MAX_ROWS`, `MIN_ROWS`, `PACK_KEYS`, `CHECKSUM`, `DELAY_KEY_WRITE`, `STATS_*`, `INSERT_METHOD`, `UNION`, `CONNECTION`, `PASSWORD`, `COMPRESSION`, `ENCRYPTION`, `DATA DIRECTORY`, `INDEX DIRECTORY`: read and ignored. Every table is the same kind whatever the `ENGINE`.
* `CREATE TABLE t LIKE other` copies the columns; `CREATE TABLE t [(cols)] [AS] SELECT ...` fills the table from a query; `CREATE TEMPORARY TABLE` is an ordinary table; `CREATE DATABASE d` and `CREATE SCHEMA d` are the same (a [schema](schemas-and-types.md)), `USE d` is read and ignored, `DROP DATABASE d` drops the schema and what is in it.

## ALTER TABLE

Several actions separated by commas run together. Accepted: `ADD [COLUMN] def [FIRST | AFTER c]`, `ADD [COLUMN] (def, def)`, `ADD [UNIQUE | FULLTEXT | SPATIAL] KEY | INDEX [name] (cols)`, `ADD [CONSTRAINT] PRIMARY KEY (cols)` (a unique index named `table_pkey`, without making the columns `NOT NULL`), `DROP KEY | INDEX name`, `DROP PRIMARY KEY`, `DROP FOREIGN KEY name`, `MODIFY [COLUMN] def`, `CHANGE [COLUMN] old def` (rename and retype), `RENAME [TO | AS] new`. Read and ignored: `ENGINE`, `CONVERT TO CHARACTER SET`, `CHARACTER SET`, `COLLATE`, `AUTO_INCREMENT`, `COMMENT`, `ROW_FORMAT`, `ALGORITHM`, `LOCK`, `FORCE`, `ORDER BY`, `ANALYZE`/`OPTIMIZE`/`REBUILD PARTITION`, `WITH`/`WITHOUT VALIDATION` and the other table options. A position (`FIRST`, `AFTER c`) is ignored: a new column goes last. `MODIFY` changes the type and the `NOT NULL`; it does not change a default or other attributes.

## INSERT and DROP

`INSERT [LOW_PRIORITY | DELAYED | HIGH_PRIORITY] [IGNORE] [INTO] t ...` (`IGNORE` drops rows that collide with a unique key, like `ON CONFLICT DO NOTHING`; other errors still stop it); `INSERT INTO t SET a = 1, b = 2`; `INSERT ... ON DUPLICATE KEY UPDATE a = VALUES(a) + 1, b = b + 1` (the same as `ON CONFLICT DO UPDATE`, `VALUES(a)` being the new row's `a`); `REPLACE [INTO] t ...`; `DROP [TEMPORARY] TABLE[S] a, b`.

## SHOW and DESCRIBE

| Statement | Gives |
| --- | --- |
| `SHOW [FULL] TABLES [FROM db] [LIKE 'p' or WHERE cond]` | the tables and saved queries, column `Tables_in_test` (and `Table_type` with `FULL`) |
| `SHOW DATABASES` or `SCHEMAS [LIKE]` | `information_schema`, `mysql`, `performance_schema`, `sys`, `test` and the schemas made |
| `SHOW [FULL] COLUMNS` or `FIELDS FROM t`, `DESCRIBE t`, `DESC t` | `Field`, `Type`, `Null`, `Key`, `Default`, `Extra` (and `Collation`, `Privileges`, `Comment` with `FULL`); types are named as MySQL names them (`int`, `text`, `decimal(10,0)`, ...) without sizes |
| `SHOW INDEX`, `INDEXES` or `KEYS FROM t` | one row per column of each index |
| `SHOW CREATE TABLE t`, `SHOW CREATE VIEW v` | a statement that makes the table (columns, keys, `ENGINE=InnoDB DEFAULT CHARSET=utf8mb4`) |
| `SHOW [GLOBAL or SESSION] VARIABLES [LIKE 'p']` | the system variables |
| `SHOW TABLE STATUS`, `SHOW ENGINES`, `SHOW CHARACTER SET`, `SHOW COLLATION` | one row for each table; the one engine `InnoDB`; four character sets |
| `SHOW WARNINGS`, `SHOW ERRORS`, `SHOW STATUS` | no warnings (there are none); two status variables |
| `SHOW PROCESSLIST`, `GRANTS`, `PRIVILEGES`, `PLUGINS`, `TRIGGERS`, `EVENTS`, `MASTER STATUS`, `BINARY LOGS`, `SLAVE STATUS`, `ENGINE ... STATUS`, `FUNCTION STATUS`, `PROCEDURE STATUS`, `OPEN TABLES` | no rows |

`SHOW name` and `SHOW ALL` stay [PostgreSQL's](session.md#run-time-parameters).

## Prepared statements

```sql
PREPARE s FROM 'SELECT * FROM t WHERE a = ? AND b = ?';     -- or FROM @variable
SET @x = 1, @y = 'k';
EXECUTE s USING @x, @y;
DEALLOCATE PREPARE s;
```

The placeholders are numbered in the order they are written. (`PREPARE name AS statement`, with `@name` parameters, is the other form: [queries and triggers](queries-and-triggers.md#prepared-statements).)

## Functions and commands that do nothing

`CURRENT_USER()`, `USER()` and `SESSION_USER()` are `root@localhost`; `DATABASE()` and `SCHEMA()` are `test`; `VERSION()` is `8.0.40`; the other MySQL functions are in [mysql.md](../functions/mysql.md). `FLUSH ...`, `OPTIMIZE`, `REPAIR`, `CHECK TABLE`, `PURGE`, `KILL`, `CHANGE`, `XA`, `LOAD INDEX`, `SIGNAL`, `RESIGNAL`, `GET DIAGNOSTICS`, `HELP`, `INSTALL`, `UNINSTALL`, `BINLOG`, `CACHE INDEX` and `CALL mtr.add_suppression(...)` are read to the end of the statement and ignored.

## LIMIT offset, COLLATE, HANDLER and nesting

`LIMIT m, n` is `LIMIT n OFFSET m`. `expr COLLATE name` is read and the collation dropped: text is compared as everywhere else in Accessor (see [expressions](expressions.md)), whatever the collation named.

```sql
HANDLER t OPEN [AS h];
HANDLER h READ FIRST | NEXT | PREV | LAST [WHERE cond] [LIMIT n];
HANDLER h READ index FIRST | NEXT | PREV | LAST;   -- the index is ignored
HANDLER h CLOSE;
```

A handler is a cursor over a table: the rows come in the order they are kept, `WHERE` picks the rows to return from the position on, and `LIMIT` (default 1) says how many. Reading by the value of an index (`READ idx = (v)`, `>=`, ...) is refused.

`SELECT ... FOR UPDATE | SHARE [OF t] [NOWAIT | SKIP LOCKED]` and `LOCK IN SHARE MODE` lock the rows of a `SELECT` of one table when the database is shared between connections (see [locking reads](../concurrency.md#locking-reads-select--for-update)): the rows are claimed for the transaction, `NOWAIT` fails and `SKIP LOCKED` skips rows another transaction holds, and shared locks are exclusive; in a database used by one transaction at a time, and in forms with joins, derived tables or unions, they are read and ignored. The select modifiers `STRAIGHT_JOIN`, `HIGH_PRIORITY`, `SQL_NO_CACHE`, `SQL_CALC_FOUND_ROWS` and the rest; `a STRAIGHT_JOIN b ON ...` is an inner join. `CHAR(n, ... USING charset)` ignores the character set. `CAST(x AS TIME)` gives the time of day as `hh:mm:ss` text (there is no type for a time alone). A `BIGINT UNSIGNED` column is a `BIGNUM`, so it holds up to 18446744073709551615 (other `UNSIGNED` columns are still not limited to non-negative values).

`/*+ MAX_EXECUTION_TIME(n) */` in a `SELECT` ends it with a timeout error after `n` milliseconds. A recursive common table expression that has produced more than 2,000,000 rows stops with the error "produced too many rows".

Texts nested too deeply are refused with an error and never overflow the stack: more than 61 levels of sub-queries (MySQL's own limit, "too high level of nesting for select"), or expressions nested more than 150 levels (parentheses, functions, `CASE`). A text with more than 8 levels of parentheses is read on a thread with a large stack of its own; the executor stops a chain of sub-queries that is still too deep after that ("queries are nested too deeply").

## RENAME TABLE, ORDER BY and LIMIT in changes, aliases, events, hints, partitions

```sql
RENAME TABLE a TO b, c TO a;                                  -- the renames one after the other, so a swap needs a third name
UPDATE t SET v = v + 1 WHERE k > 1 ORDER BY k DESC LIMIT 2;
DELETE FROM t ORDER BY k LIMIT 10;
SELECT k AS 'the key', v 'value' FROM t JOIN u;              -- a string is a column alias; JOIN without ON is a cross join
EXPLAIN t;                                                    -- the same as DESCRIBE t
CREATE EVENT e ON SCHEDULE EVERY 1 DAY DO DELETE FROM log;   -- CREATE / ALTER / DROP EVENT are read and ignored
```

`USE INDEX`, `IGNORE INDEX` and `FORCE INDEX` (also `KEY`, and `FOR JOIN | ORDER BY | GROUP BY`) after a table in `FROM` are read and ignored; the optimizer chooses its own indexes. `DROP VIEW a, b, c` drops each in turn. MySQL's `PARTITION BY [LINEAR] HASH | KEY | RANGE | LIST [COLUMNS] (..) [PARTITIONS n] [SUBPARTITION BY ..] [(PARTITION p VALUES ..)]` in `CREATE TABLE`, and `ALTER TABLE` with `PARTITION BY`, `ADD PARTITION`, `DROP PARTITION`, `TRUNCATE PARTITION` and the other partition maintenance, are read and ignored: the table stays whole and any row goes in. (PostgreSQL's plain `PARTITION BY RANGE (col)` still makes a [partitioned table](inheritance-and-partitioning.md).)

With `ORDER BY` or `LIMIT`, the rows changed are those equal in every column to one of the first rows of `SELECT * FROM t WHERE ... ORDER BY ... LIMIT n`; two rows alike in every column are not told apart (the same rule as the [several-table forms](#multi-table-delete-and-update)). Events never run: there is no scheduler. `RENAME USER` is ignored.

## Multi-table DELETE and UPDATE

```sql
DELETE a FROM t1 AS a, t2 AS b WHERE a.k = b.k;       -- or DELETE FROM a USING t1 AS a, t2 AS b WHERE ...
UPDATE t1, t2 SET t1.v = t1.v + t2.w WHERE t1.k = t2.k;
UPDATE t1 JOIN t2 ON t1.k = t2.k SET t1.v = 0 WHERE t2.w > 200;
```

The rows of the named table that the join reaches are deleted or changed; the other tables are only read. They are run as the single-table statement they amount to (`DELETE ... WHERE EXISTS (join)`, each new value a sub-query over the join), so two rows of the target that are equal in every column are treated alike. One table may be changed at a time (`DELETE t1, t2 FROM ...` and `UPDATE ... SET t1.a = .., t2.b = ..` are refused). `ORDER BY` and `LIMIT` are not available in the several-table forms.

## Stored procedures, functions and triggers

```sql
CREATE PROCEDURE sp(IN a INT, b INT) DETERMINISTIC BEGIN
  DECLARE i INT DEFAULT 0;
  WHILE i < a DO SET i = i + 1; END WHILE;
  IF i > 3 THEN INSERT INTO log VALUES ('big'); ELSEIF i > 1 THEN INSERT INTO log VALUES ('mid'); END IF;
END;
CREATE PROCEDURE one() INSERT INTO t VALUES (1);
CREATE FUNCTION f(a INT) RETURNS INT DETERMINISTIC RETURN IFNULL(a, 666);
CREATE TRIGGER trg BEFORE INSERT ON t FOR EACH ROW SET NEW.d = NEW.id * 2;
CREATE TRIGGER trg2 BEFORE UPDATE ON t FOR EACH ROW BEGIN
  IF NEW.d IS NULL THEN SET NEW.d = 0; END IF;
  INSERT INTO log VALUES (CONCAT('upd ', OLD.id));
END;
```

The body has no `AS`. It is one statement, or `BEGIN ... END` with `DECLARE`, `SET`, `IF` / `ELSEIF`, `WHILE ... DO`, `REPEAT ... UNTIL`, `LOOP` and `LEAVE`, `CASE`, `RETURN`, `SELECT ... INTO var`, and ordinary SQL statements; the characteristics (`DETERMINISTIC`, `NO SQL`, `READS SQL DATA`, `SQL SECURITY ...`, `COMMENT`) and `DEFINER = ...` are read and ignored. A name in the body that is a parameter or variable means that variable even if a column has the same name. In a trigger, `SET NEW.col = ...` changes the row a `BEFORE` trigger lets through. Procedures are run with `CALL`; a function whose body has statements is run like the [PL/pgSQL functions](routines.md#postgresql-style-functions-and-procedures) (with arguments known before the statement runs). Not available: `OUT` and `INOUT` parameters, cursors, `DECLARE ... HANDLER`, `ITERATE`, `SIGNAL`, and a `FOLLOWS`/`PRECEDES` order of triggers (read and ignored).

## Dates and intervals

`DATE_ADD(d, INTERVAL n unit)`, `DATE_SUB`, `ADDDATE`, `SUBDATE` and `d + INTERVAL n unit` / `d - INTERVAL n unit` add to a date; the units are `MICROSECOND` (ignored), `SECOND`, `MINUTE`, `HOUR`, `DAY`, `WEEK`, `MONTH`, `QUARTER`, `YEAR`. A time with a fraction of a second (`'12:00:00.567'`) is read with the fraction dropped, and the zero date `0000-00-00` is read as `0001-01-01`. `CAST(x AS SIGNED | UNSIGNED)` and `CONVERT(x, type)` convert; `CONVERT(x USING charset)` is `x`.

## More forms that are read

`a <=> b` is the null-safe equality (`NULL <=> NULL` is true), and `a XOR b` the logical exclusive or (NULL if either is). `DO expr, ...` evaluates the expressions and gives no rows. `VALUES ROW(1, 2), ROW(3, 4)` is a table of values. `START TRANSACTION READ ONLY, WITH CONSISTENT SNAPSHOT` is read and ignored. A column may be `col type AS (expression) [VIRTUAL | STORED]`, as well as `GENERATED ALWAYS AS`; `GEOMETRY ... SRID n` and `VECTOR(n)` are read (a `GEOMETRY` and a `VECTOR` are text here); an `AUTO_INCREMENT` on `BIGINT UNSIGNED` counts in 64 bits. `CREATE TABLE t ENGINE=x ROW_FORMAT=y SELECT ...` reads its options before the query, and `CAST(x AS CHAR CHARSET binary)` its character set and collation, which change nothing.

**`SELECT ... INTO OUTFILE 'file'`** writes the rows of the query to a file that does not exist yet (MySQL's "File 'x' already exists" otherwise), as the server would: a tab between fields, a newline after each row, `\N` for NULL, with `FIELDS TERMINATED BY`, `[OPTIONALLY] ENCLOSED BY`, `ESCAPED BY`, `LINES STARTING BY` and `LINES TERMINATED BY` to change that. A character that would end the field or the line, the enclosing character and the escape character itself get the escape character in front. `INTO DUMPFILE` writes one row, as it is. The path is relative to the directory the program was started in. The statement returns the number of rows written.

`GROUP BY a, b WITH ROLLUP` is `GROUP BY ROLLUP (a, b)`. `INSERT INTO t () VALUES ()` inserts a row of defaults. An index may be written `KEY USING BTREE (c)`; `CAST(x AS UNSIGNED)` gives a number that can be as large as 18446744073709551615 (a `BIGNUM`).

`DEFAULT(col)` in a `CHECK` is the column's default value (written in when the check is made). A `#` that is not the start of a `#date#` literal on the same line begins a comment to the end of the line, as in MySQL. `CREATE TABLE t (a, b, c) SELECT 1 AS b, 2 AS c` puts the query's columns in the fields of their names.

## Catalog views

`INFORMATION_SCHEMA` has the standard columns beside the engine's own: `TABLES` and `COLUMNS` add `TABLE_CATALOG`, `TABLE_SCHEMA`, `ENGINE`, `COLUMN_TYPE`, `COLUMN_KEY`, `EXTRA`, `CHARACTER_SET_NAME` and `COLLATION_NAME`, and these views are added: `SCHEMATA`, `STATISTICS` (the indexes), `CHARACTER_SETS`, `COLLATIONS`, `ENGINES`, `PARTITIONS`, `CHECK_CONSTRAINTS`, and, with the right columns and no rows, `PROCESSLIST`, `COLUMN_STATISTICS`, `USER_PRIVILEGES`, `SCHEMA_PRIVILEGES`, `TABLE_PRIVILEGES`, `COLUMN_PRIVILEGES`, `EVENTS`, `OPTIMIZER_TRACE`, `RESOURCE_GROUPS`, `FILES`, `TABLESPACES`, `PLUGINS`, `VIEW_TABLE_USAGE`, `KEYWORDS` and a few more. See [information-schema.md](information-schema.md).

## Accounts and the rest that is read and ignored

`CREATE USER 'u'@'host' IDENTIFIED BY ...`, `ALTER USER`, `DROP USER`, `SET PASSWORD` and `GRANT` / `REVOKE` in MySQL's forms (`ON *.*`, `ON db.*`, a user with a host, privileges of two words such as `REPLICATION SLAVE`) are read and **ignored**: there are no accounts, and a connection is not a user. `CREATE USER name` with no host and only PostgreSQL's options is a [role](roles-and-privileges.md). `ALTER SCHEMA`/`ALTER DATABASE`, `RESET PERSIST`, `RESET MASTER`, `CHECK TABLE[S]`, `EXPLAIN FORMAT=...` (the plan is always the text plan) and `DROP PREPARE` are accepted. `COMMIT` and `ROLLBACK` with no transaction do nothing. `0x41FF` is a number, `_utf8mb4 'x'` is `'x'`, `a << b` and `a >> b` shift (`a | b` is bitwise OR and `~a` bitwise NOT; `&` joins text and `^` is a power, as in Access, so there is no bitwise AND or XOR operator), and `CONV(n, from, to)` converts between bases. A column may be `BINARY`, `ASCII` or `UNICODE` (ignored).

## Not available

`CREATE USER 'u'@'host'` and `GRANT` for such users, the effect of `LOCK TABLES` (it is read and ignored, like `UNLOCK TABLES`), `performance_schema`, optimizer hints other than as comments, full-text search (`MATCH ... AGAINST`), spatial functions (`ST_*`), `LAST_INSERT_ID()`, `FOUND_ROWS()`, and the MySQL functions that are not in [the MySQL functions](../functions/mysql.md) or the [function reference](../functions/README.md).
