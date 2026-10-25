# The C interface

`crates/accessor-capi` builds the library `accessor_capi` and the header `accessor.h`:

```
cargo build --release -p accessor-capi
  target/release/accessor_capi.dll     (Windows)      libaccessor_capi.so   (Linux)      libaccessor_capi.dylib   (macOS)
  target/release/libaccessor_capi.a / accessor_capi.lib   (a static library, to link into your program)
  crates/accessor-capi/include/accessor.h
```

The header is **generated** from the Rust source each time the crate is built (with cbindgen), so it cannot drift from the library. Compile with `-Icrates/accessor-capi/include` and link the library; the static library needs `-lpthread -ldl -lm` on Linux.

```c
#include <stdio.h>
#include "accessor.h"

int main(void) {
    acc_db *db;
    if (acc_open("shop.adb", &db) != ACC_OK) {          /* NULL or ":memory:" for a database in memory */
        fprintf(stderr, "cannot open: %s\n", acc_errmsg(db));
        acc_close(db);                                   /* a handle is returned even when opening fails */
        return 1;
    }
    acc_exec(db, "CREATE TABLE IF NOT EXISTS c (id COUNTER PRIMARY KEY, name TEXT)", ACC_NTS, NULL);

    acc_stmt *ins;
    acc_prepare(db, "INSERT INTO c (name) VALUES (@name) RETURNING id", ACC_NTS, &ins);
    acc_bind_text(ins, 0, "Ada", ACC_NTS);
    acc_result *r;
    if (acc_stmt_run(ins, &r) == ACC_OK) {
        printf("new id %lld\n", (long long)acc_value_int64(r, 0, 0));
        acc_result_free(r);
    }
    acc_stmt_free(ins);

    if (acc_exec(db, "SELECT id, name FROM c ORDER BY id", ACC_NTS, &r) == ACC_OK) {
        for (size_t row = 0; row < acc_result_row_count(r); row++)
            printf("%lld  %s\n", (long long)acc_value_int64(r, row, 0), acc_value_text(r, row, 1, NULL));
        acc_result_free(r);
    }
    acc_close(db);
    return 0;
}
```

## Rules

* **Return codes.** Every function that can fail returns an `int`: `ACC_OK` (0) or an error code. `acc_errcode(db)` and `acc_errmsg(db)` give the code and message of the last call on a connection (an empty message after a success); the message string is valid until the next call on that connection.
* **Text** is UTF-8. A function that takes text takes `(const char *text, size_t len)`: pass the length in bytes, or `ACC_NTS` (`(size_t)-1`) if the text is NUL-terminated. With a length, the text may contain NUL bytes. Invalid UTF-8 is refused with `ACC_ERR_ENCODING`; NULL with `ACC_ERR_MISUSE`.
* **Ownership.** `acc_open`, `acc_prepare`, `acc_exec`/`acc_stmt_run` (with a result pointer) and `acc_render_report` give you something to release: `acc_close`, `acc_stmt_free`, `acc_result_free`, `acc_buffer_free`. The close and free functions accept NULL. Strings returned by `acc_result_column_name`, `acc_value_text` and `acc_stmt_param_name` belong to the result or statement and are valid until it is freed.
* **Threads.** A connection may be used from any thread, and from several at once: each database runs on a thread of its own inside the library, and calls on one connection are carried out one at a time, in the order they arrive. (This is because the engine is single-threaded; it is also why there is no connection pool inside.) Statements and results are not thread-safe: use each from one thread at a time. Closing a connection while other threads are using it is a mistake, as with any handle.
* **Errors do not escape.** Every function catches panics; one that does reports `ACC_ERR_INTERNAL` ("a bug in the library") and the connection stays usable.
* **A connection is one database file.** See [locking.md](locking.md). Closing a connection (`acc_close`) checkpoints the database and releases the lock before it returns; statements made from it fail with `ACC_ERR_MISUSE` afterwards but must still be freed.

## Error codes

| Code | Name | Meaning |
| --- | --- | --- |
| 0 | `ACC_OK` | success |
| 1 | `ACC_ERR_SYNTAX` | the SQL could not be read |
| 2 | `ACC_ERR_CATALOG` | a table, column, index, ... is missing, duplicated or in the way |
| 3 | `ACC_ERR_CONSTRAINT` | not null, unique, foreign key, check or a trigger's `RAISE` refused the change |
| 4 | `ACC_ERR_TYPE` | a value has the wrong type or cannot be converted |
| 5 | `ACC_ERR_RUNTIME` | any other error while running a statement (division by zero, ...) |
| 6 | `ACC_ERR_IO` | the operating system refused a file operation |
| 7 | `ACC_ERR_CORRUPT` | the database file is damaged or is not an Accessor database |
| 8 | `ACC_ERR_LOCKED` | another connection or process has the database open |
| 9 | `ACC_ERR_MISUSE` | wrong use: NULL or closed handle, index out of range, a parameter with no value |
| 10 | `ACC_ERR_ENCODING` | text is not valid UTF-8 |
| 11 | `ACC_ERR_INTERNAL` | a bug in the library (a panic was caught) |
| 12 | `ACC_ERR_CONFLICT` | two transactions changed the same row and this one must be run again (the C interface has one connection per database, so it cannot happen yet; it is reserved for when connections can overlap, see [concurrency](../concurrency.md)) |

## Functions

**Library**: `acc_version`, `acc_version_number`, `acc_abi_version`, `acc_file_format_version`.

**Connections**: `acc_open(path, &db)`, `acc_close(db)`, `acc_errcode(db)`, `acc_errmsg(db)`, `acc_exec(db, sql, len, &result)` (one or more statements; the result is that of the last), `acc_checkpoint(db)`, `acc_set_optimizer(db, on)`, `acc_set_durability(db, mode)` (`ACC_DURABILITY_FSYNC` or `ACC_DURABILITY_OS_BUFFERED`), `acc_in_transaction(db)`, `acc_render_report(db, name, len, format, &bytes, &size)` with `acc_buffer_free` (formats `ACC_REPORT_TEXT`, `_HTML`, `_MARKDOWN`, `_CSV`, `_PDF`).

Transactions are SQL: `acc_exec(db, "BEGIN", ...)`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`.

**Prepared statements**: `acc_prepare(db, sql, len, &stmt)` reads the SQL once; parameters are written `@name` and are numbered from 0 in order of first appearance (`acc_stmt_param_count`, `acc_stmt_param_name`, `acc_stmt_param_index`). Set them with `acc_bind_null`, `_int64`, `_double`, `_bool`, `_text`, `_decimal` (digits of an exact decimal number), `_bignum` (digits of a whole number of any size), `_datetime` (seconds since 1970-01-01 00:00:00, no time zone) and `_timestamptz` (seconds since the Unix epoch, UTC). Every parameter must have a value (NULL is a value) before `acc_stmt_run`; values stay bound for the next run until `acc_stmt_clear_bindings`. `acc_stmt_free`.

**Results**: `acc_result_row_count`, `_column_count`, `_affected` (rows changed by an `INSERT`, `UPDATE`, `DELETE`), `_column_name`; and for a value at (row, column), both counted from 0: `acc_value_type`, `acc_value_int64`, `acc_value_double`, `acc_value_text(res, row, col, &len)`. A position outside the result gives `ACC_TYPE_NULL`, 0 and NULL; it never reads out of bounds.

## Value types

| Tag | Type | Read with |
| --- | --- | --- |
| `ACC_TYPE_NULL` | NULL | |
| `ACC_TYPE_INTEGER` | `INTEGER`, `COUNTER` | `acc_value_int64` |
| `ACC_TYPE_DOUBLE` | `FLOAT`, `DOUBLE` | `acc_value_double` |
| `ACC_TYPE_TEXT` | `TEXT` | `acc_value_text` |
| `ACC_TYPE_BOOL` | `YESNO` | `acc_value_int64` (0 or 1), text `true`/`false` |
| `ACC_TYPE_DECIMAL`, `_BIGNUM`, `_CURRENCY`, `_RATIONAL` | exact numbers | `acc_value_text`: the digits (`"12.50"`, `"-123456789012345678901234567890"`, `"1/3"`), with no currency sign. These are text on purpose, so that no precision is lost on the way to a C type |
| `ACC_TYPE_DATETIME` | `DATETIME` | `acc_value_int64`: seconds since 1970-01-01 00:00:00 with no time zone; text `YYYY-MM-DD HH:MM:SS` |
| `ACC_TYPE_TIMESTAMPTZ` | `TIMESTAMPTZ` | `acc_value_int64`: seconds since the Unix epoch, UTC; text in the session time zone |
| `ACC_TYPE_INTERVAL` | `INTERVAL` | `acc_value_text` |
| `ACC_TYPE_JSON` | `JSON` | `acc_value_text`: the canonical document |

## Notes

* A result is **copied out** of the engine when the statement runs: it can be read at leisure, is not affected by later changes, and holds its memory until freed. There is no row-at-a-time cursor, since the database is in memory in any case.
* `acc_exec` with `RETURNING` (or a prepared statement with it) gives the generated keys in a result, which is how to get an AutoNumber back.
* Plans are not shared between runs of a prepared statement; the SQL is read once. See [the optimizer notes](../optimizer/prepared.md).
