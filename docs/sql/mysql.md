# MySQL syntax

Scripts and tests written for MySQL run on the engine when they stay within what is described here. This page lists what is read, and what it does; it is not MySQL's dialect in full.

## Names and quoting

`` `name` `` is a quoted identifier (two backticks inside stand for one), like `[name]`. `?` is the placeholder of a prepared statement (see below).

## Variables

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

`CURRENT_USER()`, `USER()` and `SESSION_USER()` are `root@localhost`; `DATABASE()` and `SCHEMA()` are `test`; `VERSION()` is `8.0.40`. `FLUSH ...`, `OPTIMIZE`, `REPAIR`, `CHECK TABLE`, `PURGE`, `KILL`, `CHANGE`, `XA`, `LOAD INDEX`, `SIGNAL`, `RESIGNAL`, `GET DIAGNOSTICS`, `HELP`, `INSTALL`, `UNINSTALL`, `BINLOG`, `CACHE INDEX` and `CALL mtr.add_suppression(...)` are read to the end of the statement and ignored.

## Not available

Stored routines with MySQL's `DELIMITER` bodies and cursors, `HANDLER`, the effect of `LOCK TABLES`, `CREATE USER 'u'@'host'` and `GRANT` for such users, `performance_schema` and most of `information_schema`, multi-table `UPDATE` and `DELETE`, `LIMIT offset, count` in `UPDATE` and `DELETE`, `EXPLAIN FORMAT=`, optimizer hints, `GROUP BY ... WITH ROLLUP`, full-text search (`MATCH ... AGAINST`), spatial functions, and the MySQL date and string functions that are not in [the function reference](../functions/README.md).
