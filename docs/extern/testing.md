# How the C interface is checked

The C interface is the one place where the engine's memory safety rests on a caller's pointers, so it is checked harder than the rest.

## The C tests

`crates/accessor-capi/tests/c/test_accessor.c` is a C program of 1,256 checks that exercises every function: all value types, prepared statements with every kind of bind, the error code of every class of failure, text that is empty, has a NUL, is a megabyte, is not UTF-8, NULL handles passed to every function, a closed connection, a database file locked by another **process** (a second copy of the program is started to try), a file that is not a database, reports as PDF, eight threads sharing one connection, a hundred thousand rows, and fifty connections opened and closed.

```
crates/accessor-capi/tests/c/run-c-tests.sh [plain] [valgrind] [asan]
```

builds the library, builds the test with `-Wall -Wextra -Werror -pedantic -std=c11`, and runs it in each mode (all three by default):

* **plain**.
* **valgrind**: `--leak-check=full`, failing on any invalid access, use of uninitialised memory, or definite or possible leak.
* **asan**: the *library* is built with `-Zsanitizer=address` (nightly Rust), so that every unsafe access in the Rust code that handles C pointers is checked as well as the C test's own, and the test is built with AddressSanitizer and UndefinedBehaviorSanitizer, failing on the first report.

Each checker is first run on `canary.c`, a program with a deliberate heap overflow, and the run stops if the checker does *not* object. A clean run of the real tests therefore means the checker was watching.

Both checkers found nothing in the current code. Valgrind did show one thing worth recording: the Rust runtime registers the calling thread the first time it blocks on a channel, which leaves a 48-byte allocation behind on a thread owned by the C program and reads as a leak. The library therefore waits for the database thread with a mutex and a condition variable, which do not.

## Fuzzing

`crates/accessor-capi/fuzz` has two libFuzzer targets run under AddressSanitizer (`cargo +nightly fuzz`):

| Target | Feeds the interface |
| --- | --- |
| `strings` | arbitrary bytes as SQL (with a length and NUL-terminated, cut at arbitrary bytes), report names, parameter names and bound text of every kind (`text`, `decimal`, `bignum`), and paths to `acc_open` |
| `binds` | arbitrary sequences of `acc_bind_*`, clear, run and read calls on prepared statements, with indexes in range, one past, negative and huge, and values of every kind |

After every call the targets require that the return code is a documented one and **never `ACC_ERR_INTERNAL`** (which would mean a panic was caught: the interface contained it, but it is a bug in the engine or the library), that text coming back is valid UTF-8 and NUL-terminated, that positions outside a result give defaults, that bound text comes back byte for byte, and that the connection still works. A keyword dictionary and about 130 valid statements seed the SQL, so the fuzzer starts inside the parser. SQL using the functions that can ask for unbounded memory (`GENERATE_SERIES`, `REPEAT`, `LPAD`, `WITH RECURSIVE`, ...) is skipped: the engine has no resource limits, which is a known property, not something fuzzing can tell it.

```
crates/accessor-capi/fuzz/run-fuzz.sh [seconds per target] [target ...]
```

**Results so far.** No crash, sanitizer report, caught panic, timeout or out-of-memory in any run: `binds` for 240 seconds (95,453 inputs) and `strings` for 120 seconds (8,077 inputs, after fixing the harness so that it never creates files from fuzz data; an earlier run had let some fuzzed paths through to `acc_open`). That is a short fuzzing session, not a proof; run it for hours before a release, and again after changing `crates/accessor-capi/src/lib.rs`.

## Isolation: Hermitage

`cargo test -p accessor-core --test hermitage` plays the scenarios of [Hermitage](https://github.com/ept/hermitage) against overlapping transactions: eleven tests, one per anomaly. All of them behave as snapshot isolation should, with and without `ACCESSOR_FORCE_MVCC=1`; no engine fix was needed. See [concurrency](../concurrency.md).

## TPC-H

`cargo run --release -p accessor-tpch -- --sf 0.1 [--compare] [--only 1,5] [--no-indexes] [--explain] [--timeout SECONDS]` generates data in the shape of `dbgen` (the official tool is not in the repository, so values differ from the official answers), loads it with the keys and indexes, and runs the 22 queries with the parameters of the validation run. With `--compare` each answer is checked against the optimizer switched off and against the same data without indexes. At scale factors 0.01 to 0.5 (3 million line items) all 22 queries run and agree. The run found a join order that multiplied two small tables first (query 8, never finishing at 0.1; now 0.3 seconds, see [join order](../optimizer/join-order.md)) and a size limit under `statement_timeout` that stopped joins of 262,144 rows whatever their width (queries 9 and 13 at 0.2 and up; now 16 million values, rows times columns). Slowest at 0.5: query 1 (17 s, a scan of 3 million rows) and query 9 (28 s).

## TPC-DS

`cargo run --release -p accessor-tpcds -- --sf 0.1 [--compare] [--only 3,7] [--no-indexes] [--explain] [--sql QUERY] [--timeout SECONDS]` generates data in the shape of `dsdgen` (the official tool is not in the repository, so values differ from the official answers): the three sales channels with their returns, the inventory and the dimensions. It runs 40 of the 99 queries, chosen for the features the benchmark exists to exercise: common table expressions used several times, `ROLLUP` with `GROUPING` and `RANK` over it, window functions over aggregates, `INTERSECT` / `EXCEPT`, correlated and uncorrelated subqueries, `UNION ALL` of the channels, full outer joins and many-way star joins. With `--compare` each answer is checked against the same data without indexes (the optimizer-off check is too slow for these queries). All 40 run and agree at scale factors 0.01 and 0.1 (288,000 store sales lines; the slowest query takes 3 seconds). The run found two gaps: `((SELECT ..) EXCEPT (SELECT ..)) t`, a parenthesised set operation as a derived table (query 87), and an output alias inside an `ORDER BY` expression, `ORDER BY CASE WHEN lochierarchy = 0 THEN i_category END` (queries 36 and 70), which MySQL and SQLite accept and now Accessor does when no column has that name.

### TPC-DS with the official kit

`scripts/tpcds-kit.sh OUT_DIR [SCALE]` (in WSL on Windows: it needs gcc, make, flex, bison, python3 and `pdftotext`) builds `dsdgen` and `dsqgen` from the `third_party/tpcds-kit` submodule and writes the data, the schema, the 99 queries and the kit's answer sets for scale factor 1. `accessor-tpcds --kit OUT_DIR --db FILE.adb [--only 3,7] [--timeout SECONDS] [--explain]` loads the data once into a database file (about 20 million rows at scale 1, five minutes; opening the file afterwards takes 90 seconds, because the snapshot is read into memory and its indexes rebuilt), runs the queries and compares each result with the answer set. With `--repl` the database stays open and the runner reads commands from standard input, one a line (`run 39,72`, `run 73-99`, `explain 72`, `sql SELECT ...`, `timeout 300`, `quit`), printing `done` after each, so a series of runs pays the opening once.

Two details matter for the answers to agree. `dsqgen -qualify` must be an upper case `Y` (a lower case `y` is ignored, and the queries come out shuffled with random parameters), and it only orders the queries: the *values* of the qualification run are listed in Appendix B of the specification, so `scripts/tpcds_qualify.py` reads them from the PDF and writes a copy of the templates with them filled in. The answer files are fixed-width text from several servers, so the comparison reads both sides as lists of values (numbers to two decimals), ignores footers (`(100 rows)`, `Warning: Null value is eliminated ...`), a repeated header between the result sets of a query that has several, reads a lone `%` or `-` as NULL (queries 44 and 73), and ignores the padding of a `CHAR` column before a comma (query 84). The files print each number to as many digits as the column is wide (`132885062` or `20390161.4` for 132885061.65 or 20390161.37 in query 77), so rows that differ only in the digits the answer does not show are matched again with the numbers rounded to the answer's own precision; the verdict then says so.

The answers of queries 73, 78 and 83 were made by a server whose `/` never drops the fraction of whole numbers (`hd_dep_count / hd_vehicle_count > 1`, `ss_qty / (ws_qty + cs_qty)`). `--exact-division` runs the queries with `SET integer_division = off`, which does the same ([../types/arithmetic.md](../types/arithmetic.md)); without it those queries give different rows. Query 78 also rounds `ss_qty / (...)` to two places, and the answer rounds a tie away from zero (2.125 is 2.13) where `Round` goes to the even digit; `--round-half-away` is `SET round_half_even = off` ([../functions/numeric.md](../functions/numeric.md)).

Result at scale factor 1 with both flags (`--timeout 120`): **98 of the 99 queries give the rows of the answer sets**, with no failure and no timeout; the slowest are query 23 (34 s), 14 (29 s), 4 (25 s), 39 (19 s) and 95 (19 s), and the whole run takes about 35 minutes. Six of them match only to the digits the answer file prints (queries 14 and 30 among them). The one that does not is query 18: 99 of its 100 rows agree, and the grand total of its `ROLLUP` differs (`101.83` against `101.78` for the average list price). That row is not an error of the engine. The answer file prints it to six decimals, and the mean of the 1,317 averages of the finest groups, `(i_item_id, ca_country, ca_state, ca_county)`, gives all seven of its values to the last digit (`49.803721`, `101.779821`, `277.435551`, `51.316508`, `-238.901636`, `1957.485012`, `2.974943`), whereas the average of the 1,318 rows under it, which is what `AVG` over a `ROLLUP` means in SQL, gives `49.795144`, `101.832508`, and so on. So the server that made the answer set took the average of a rolled-up level as the unweighted mean of the averages of the groups under it. (The item-level rows cannot tell the two apart: the items with more than one group have one row in each, and the three levels in between are cut off by the `LIMIT`.) Accessor, PostgreSQL, MySQL and SQL Server give the average of the rows.

## The JDBC driver

`jdbc/src/test/java/com/accessor/jdbc/JdbcTest.java` runs 23 groups of tests against the real native library (`jdbc/build.sh`): types in and out, exactness of decimals, NULLs, scrolling, every bind type, placeholders in odd places, batches that fail, generated keys, error states, transactions and savepoints, closed objects, metadata, files and locking, reports, unsupported features, strange text, many connections, several threads, and a result of 50,000 rows. The jar is also run on its own, loading the library from inside itself.

## sqllogictest

`crates/accessor-slt` runs SQLite's [sqllogictest](https://www.sqlite.org/sqllogictest/doc/trunk/about.wiki) files (about 7.4 million records in `select1`–`select5`, `evidence/`, `index/` and `random/`; get them from `https://github.com/gregrahn/sqllogictest`) against the engine:

```
cargo build --release -p accessor-slt
target/release/accessor-slt --jobs 8 --memory-mb 1500 --timeout 600 --quiet  PATH-TO/sqllogictest/test/index
```

* Every file gets a fresh in-memory database. `skipif`/`onlyif` treat Accessor as a database called `accessor`, so the lines meant for SQLite are skipped and the others run.
* **Each file runs in a child process** with a hard memory limit (a Windows job object; elsewhere use `ulimit -v`) and a time limit, up to `--jobs` at a time, so that a query that tries to use all the memory cannot take the machine down. A record that kills its process is reported as a `KILLER` with its file and line, and the file is run again without it. (A 64-table join once did exactly this; see [the optimizer's limit](../optimizer/join-order.md#when-reordering-applies).)
* The runner checks itself: a file with one value changed fails, and a record that is made to run out of memory is found.
* The order values are printed in, the rounding of reals (three places) and the `rowsort`/`valuesort`/hash forms are those of the original runner.

Results on this build (the `random/` folder has not been run in full; of the two files tried, `random/select/slt_good_0` passed 98.3% and `random/aggregates/slt_good_0` 94.6%):

| Part | Records | Passed |
| --- | --- | --- |
| `select1`–`select5` | 10,706 | **100%** |
| `evidence/` and `index/` (226 files) | 2,058,631 | 2,056,343 (**99.89%**), 0 killed, 0 panics |

On a machine with 12 cores, `evidence/` and `index/` took 18.5 minutes at 8 files at a time; a file of 10,000 records takes 20–50 seconds, so the whole suite one file at a time would be about 4.5 hours. The failures, and what they say about ASQL, are in [the conformance page](../sql/conformance.md#differences-found-by-sqllogictest).

All 622 files of the suite (5.94 million records) were run in 48 minutes at 8 files at a time: 5,939,712 records pass, and the two that do not are the updatable-view difference described there.

## The PostgreSQL regression suite

`crates/accessor-pgreg` runs the SQL files of PostgreSQL's `src/test/regress` (239 tests, 53,459 statements) against the engine and compares, statement by statement, whether the statement is accepted and, where PostgreSQL's expected output has a result table, whether the rows agree. It is a measure of how much PostgreSQL SQL Accessor reads, not a conformance claim: the suite exercises PostgreSQL's own types, catalogs and error messages.

```
target/release/accessor-pgreg --pg PATH-TO/postgres/src/test/regress --report report.md --stall 20 --memory-mb 2048
```

The runner works in the `postgresql` dialect ([../sql/dialects.md](../sql/dialects.md)).

Each test runs in its own memory-capped child process; a statement that runs past `--stall` seconds, or kills the process, is skipped and listed. Latest run (142 s): agreement on acceptance **47.7%** (25,482 of 53,459 statements; it was 47.6% before the TPC-DS work, 46.4% before the MySQL work, 25.9% before the PostgreSQL function pack and 40.8% before the statement-form, optimizer and SQL/JSON-path work), no statement skipped as a killer (1 before), 1 engine panic; 4,412 of 7,170 compared result tables differ (many because a table they read was empty after an earlier refusal). The one still skipped is a `LATERAL` join over two 10,000-row tables with `ORDER BY` (the recursive queries now stop at 1,000 rounds, as MySQL's do).

## The MySQL test suite

`crates/accessor-mtr` runs the tests of MySQL's `mysql-test` folder (`t/NAME.test` with the expected output in `r/NAME.result`; get them with a sparse checkout of `github.com/mysql/mysql-server`, folders `mysql-test/t`, `r` and `include`). It reads the `mysqltest` language well enough to reach the SQL (`let`, `inc`, `dec`, `eval`, `if`, `while`, `--source` of files under `include/`, `--error`, the query-log switches; replication, connections, files and server commands are ignored), runs every statement in a database of its own per test, and compares what Accessor did with the server's echoed output: accepted or refused and, for a query, the rows.

```
target/release/accessor-mtr --mysql PATH-TO/mysql-test --report mysql-report.md --stall 20 --memory-mb 2048 [--only a,b,c]
```

The runner works in the `mysql` dialect.

A statement that runs past `--stall` seconds or exhausts memory is skipped and listed; a test that needs five such skips is given up; `--cap N` (default 3000) limits the statements run per test, and the tests of `mysqltest` itself are left out. Statements whose `$variables` a helper script would have set are not counted. Counting only the first 500 statements of each test, agreement on whether a statement is accepted went from **25.2%** (first run) to 62.8% after the MySQL syntax of [mysql.md](../sql/mysql.md) was added, and to **71.1%** after the functions, multi-table `DELETE`/`UPDATE`, stored routines and triggers, catalog views and accounts; over the first 3,000 statements of each of 1,487 tests (388,238 statements, 22 minutes) it is **73.3%**, and **73.8%** after `RENAME TABLE`, `UPDATE`/`DELETE ... LIMIT`, quoted aliases, `JOIN` without `ON`, `EXPLAIN t`, events and `|`/`~` (25 minutes). After the runner learned to read `let name=` with its value on the following lines, to keep the results of a test that kills the process, and to read `test.t1` as `t1`, and with index hints, `DROP VIEW` lists, MySQL partitioning clauses and non-strict `sql_mode`, it is **79.5%** over 372,288 statements, and **79.9%** over 378,233 after `LIMIT m, n`, `COLLATE`, `HANDLER` and a limit on nesting (see [mysql.md](../sql/mysql.md#limit-offset-collate-handler-and-nesting)), with 3 tests left that kill the process; and **80.9%** after locking reads, `STRAIGHT_JOIN`, `CHAR(.. USING ..)`, `MAX_EXECUTION_TIME`, a lower row limit for recursion, `BIGINT UNSIGNED` and `CAST(x AS TIME)`, with 2 tests left that kill the process (the number of statements run changed with the runner, so this is not a like-for-like step). 284 tests agree on more than 80% of their statements and 50 have no statement both engines accepted. After the TPC-DS work (1,546 s) agreement on acceptance is still **83.0%** (306,599 of 369,602 statements, 306,638 before): 13 more statements agree with their result, 221 more result tables are compared (219 of them equal), and 52 fewer refusals agree, because Accessor now accepts a derived table without an alias (`FROM (SELECT ...) WHERE ...`, as PostgreSQL 16 does) where MySQL says *Every derived table must have its own alias*. Four statements of the `explain_tree_hypergraph` and `subquery_sj_all_bka_nobnl` tests now count as killers because their process peaks at 2,054 MB, six megabytes over the `--memory-mb 2048` cap; with 2304 they run.

## Logic bugs: the lancer

`crates/accessor-lancer` looks for wrong answers the way [SQLancer](https://github.com/sqlancer/sqlancer) does. SQLancer itself needs a JDBC provider written for the database under test, so the same oracles are built into a Rust program that links the engine. It makes random databases (one to three tables of integers, text, Yes/No and floating-point columns, NULLs, single and two-column indexes) and random queries (arithmetic, `CASE`, `COALESCE`, `LIKE`, `BETWEEN`, `IN`, correlated `EXISTS` and `IN` sub-queries, equality and outer joins), and checks:

* **TLP (ternary logic partitioning)**: a query returns the rows of the same query with `WHERE p`, `WHERE NOT p` and `WHERE p IS NULL` together; likewise `COUNT`, `SUM`, `MIN`, `MAX`, `DISTINCT` and `GROUP BY ... HAVING`.
* **NoREC**: `COUNT(*) ... WHERE p` equals the number of rows for which `p` is true when it is evaluated in the select list with the optimizer off.
* **Optimizer on and off** give the same rows for plain, grouped, sorted-and-limited, `DISTINCT`, derived-table, `WITH`, `UNION ALL`, outer-join, window, `INTERSECT` and `EXCEPT` queries.
* **`DELETE` and `UPDATE`** with a predicate change as many rows as the predicate selects, and leave the rest (undone afterwards by `ROLLBACK`).

A statement that panics is a finding. Rows that differ only in the case of a text are the same row (text is compared without regard to case, so `DISTINCT` and `LIMIT` keep whichever spelling comes first).

```
cargo run --release -p accessor-lancer -- --seeds 10000 --queries 72      # 720,000 checks
cargo run --release -p accessor-lancer -- --start 3213 --seeds 1 --queries 70 --verbose    # one database again, with its script
```

Every database is made from its seed, so a finding is repeated with `--start SEED --seeds 1` (with the same `--queries`); `--verbose` prints the script that builds the tables. It found two bugs, both fixed: an index on a Yes/No column was used for `<`, `>=` and `BETWEEN`, where Yes compares as -1 but sorts after No in the index, so the indexed and unindexed answers differed; and a derived table or view whose condition held a sub-query reading the table itself failed with the optimizer on. It also showed that a correlated sub-query inside the argument of an aggregate was refused; that works now.
