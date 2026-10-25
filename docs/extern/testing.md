# How the C interface is checked

The C interface is the one place where the engine's memory safety rests on a caller's pointers, so it is checked harder than the rest.

## The C tests

`crates/accessor-capi/tests/c/test_accessor.c` is a C program of about 1,300 checks that exercises every function: all value types, prepared statements with every kind of bind, the error code of every class of failure, text that is empty, has a NUL, is a megabyte, is not UTF-8, NULL handles passed to every function, a closed connection, a database file locked by another **process** (a second copy of the program is started to try), a file that is not a database, reports as PDF, eight threads sharing one connection, a hundred thousand rows, and fifty connections opened and closed.

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
