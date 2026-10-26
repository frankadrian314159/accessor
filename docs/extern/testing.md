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

Each test runs in its own memory-capped child process; a statement that runs past `--stall` seconds, or kills the process, is skipped and listed. Latest run (619 s): agreement on acceptance **46.4%** (24,816 of 53,459 statements; it was 25.9% before the PostgreSQL function pack and 40.8% before the statement-form, optimizer and SQL/JSON-path work), 3 statements skipped as killers (27 before), 1 engine panic; 3,863 of 6,282 compared result tables differ (many because a table they read was empty after an earlier refusal). The three still skipped are a `LATERAL` join over two 10,000-row tables with `ORDER BY`, a recursive query over `varbit` (read as text here, so it never ends), and a recursive query with a nested `WITH RECURSIVE`.

## The MySQL test suite

`crates/accessor-mtr` runs the tests of MySQL's `mysql-test` folder (`t/NAME.test` with the expected output in `r/NAME.result`; get them with a sparse checkout of `github.com/mysql/mysql-server`, folders `mysql-test/t`, `r` and `include`). It reads the `mysqltest` language well enough to reach the SQL (`let`, `inc`, `dec`, `eval`, `if`, `while`, `--source` of files under `include/`, `--error`, the query-log switches; replication, connections, files and server commands are ignored), runs every statement in a database of its own per test, and compares what Accessor did with the server's echoed output: accepted or refused and, for a query, the rows.

```
target/release/accessor-mtr --mysql PATH-TO/mysql-test --report mysql-report.md --stall 20 --memory-mb 2048 [--only a,b,c]
```

A statement that runs past `--stall` seconds or exhausts memory is skipped and listed; a test that needs five such skips is given up. The first full run (1,489 tests, 661,039 statements, 2 h 15 min, most of it spent waiting on killers) agreed on whether a statement is accepted for **20.6%** of the statements, or **25.4%** counting only the first 500 statements of each test and leaving out the mechanical `$variable` and `$MYSQL_TEST` statements of the runner's own limits. Most of the rest is a few missing features of MySQL's dialect, reported in the run's `mysql-report.md`.
