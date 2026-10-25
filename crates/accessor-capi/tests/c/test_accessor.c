/*
 * Tests of the C interface, written in C so that they check the header as well as the library.
 *
 * Build and run (see run-c-tests.sh):   cc -std=c11 -Wall -Wextra -Werror -pedantic test_accessor.c ...
 * The tests are meant to be run under Valgrind and AddressSanitizer: they free everything they allocate.
 */
#define _GNU_SOURCE
#include <inttypes.h>
#include <math.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include "accessor.h"

static int checks = 0;
static int failures = 0;

#define CHECK(cond)                                                                      \
    do {                                                                                 \
        checks++;                                                                        \
        if (!(cond)) {                                                                   \
            failures++;                                                                  \
            fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #cond);     \
        }                                                                                \
    } while (0)

#define CHECK_CODE(expr, want)                                                           \
    do {                                                                                 \
        int got_ = (expr);                                                               \
        checks++;                                                                        \
        if (got_ != (want)) {                                                            \
            failures++;                                                                  \
            fprintf(stderr, "%s:%d: %s returned %d, expected %s (%d)\n", __FILE__,       \
                    __LINE__, #expr, got_, #want, (int)(want));                          \
        }                                                                                \
    } while (0)

/* ---- helpers ------------------------------------------------------------------------- */

static acc_db *open_memory(void) {
    acc_db *db = NULL;
    CHECK_CODE(acc_open(NULL, &db), ACC_OK);
    CHECK(db != NULL);
    return db;
}

static void run(acc_db *db, const char *sql) {
    int code = acc_exec(db, sql, ACC_NTS, NULL);
    if (code != ACC_OK) {
        fprintf(stderr, "  [%s] failed: %s\n", sql, acc_errmsg(db));
    }
    CHECK_CODE(code, ACC_OK);
}

static acc_result *query(acc_db *db, const char *sql) {
    acc_result *r = NULL;
    int code = acc_exec(db, sql, ACC_NTS, &r);
    if (code != ACC_OK) {
        fprintf(stderr, "  [%s] failed: %s\n", sql, acc_errmsg(db));
    }
    CHECK_CODE(code, ACC_OK);
    CHECK(r != NULL);
    return r;
}

static int text_is(const acc_result *r, size_t row, size_t col, const char *want) {
    size_t len = 0;
    const char *t = acc_value_text(r, row, col, &len);
    return t != NULL && len == strlen(want) && memcmp(t, want, len) == 0 && t[len] == '\0';
}

static int64_t scalar_int(acc_db *db, const char *sql) {
    acc_result *r = query(db, sql);
    int64_t v = acc_value_int64(r, 0, 0);
    acc_result_free(r);
    return v;
}

/* ---- tests --------------------------------------------------------------------------- */

static void test_version(void) {
    const char *v = acc_version();
    CHECK(v != NULL && strlen(v) >= 5);
    CHECK(strchr(v, '.') != NULL);
    int n = acc_version_number();
    CHECK(n > 0);
    /* major.minor.patch encoded as major*10000 + minor*100 + patch. */
    int major = 0, minor = 0, patch = 0;
    CHECK(sscanf(v, "%d.%d.%d", &major, &minor, &patch) == 3);
    CHECK(n == major * 10000 + minor * 100 + patch);
    CHECK(acc_abi_version() == 1);
    CHECK(acc_file_format_version() >= 5);
}

static void test_basics_and_types(void) {
    acc_db *db = open_memory();
    run(db,
        "CREATE TABLE t (i INTEGER, d DOUBLE, s TEXT, b YESNO, dec DECIMAL, big BIGNUM,"
        " cur CURRENCY, rat RATIONAL, dt DATETIME, tz TIMESTAMPTZ, iv INTERVAL, js JSON)");
    run(db,
        "INSERT INTO t VALUES (42, 2.5, 'hello', TRUE, 12.50, 123456789012345678901234567890,"
        " 19.99, Rational(1, 3), #2024-03-05 14:30:00#, TIMESTAMP WITH TIME ZONE '2024-03-05 14:30:00+00:00',"
        " INTERVAL '3 days', '{\"b\": [1,2], \"a\": null}')");
    run(db, "INSERT INTO t (i) VALUES (NULL)");
    acc_result *r = query(db, "SELECT * FROM t ORDER BY i DESC");
    CHECK(acc_result_row_count(r) == 2);
    CHECK(acc_result_column_count(r) == 12);
    CHECK(strcmp(acc_result_column_name(r, 0), "i") == 0);
    CHECK(strcmp(acc_result_column_name(r, 11), "js") == 0);
    CHECK(acc_result_column_name(r, 12) == NULL);

    CHECK(acc_value_type(r, 0, 0) == ACC_TYPE_INTEGER);
    CHECK(acc_value_int64(r, 0, 0) == 42);
    CHECK(acc_value_type(r, 0, 1) == ACC_TYPE_DOUBLE);
    CHECK(fabs(acc_value_double(r, 0, 1) - 2.5) < 1e-12);
    CHECK(acc_value_type(r, 0, 2) == ACC_TYPE_TEXT);
    CHECK(text_is(r, 0, 2, "hello"));
    CHECK(acc_value_type(r, 0, 3) == ACC_TYPE_BOOL);
    CHECK(acc_value_int64(r, 0, 3) == 1);
    CHECK(text_is(r, 0, 3, "true"));
    CHECK(acc_value_type(r, 0, 4) == ACC_TYPE_DECIMAL);
    CHECK(text_is(r, 0, 4, "12.5"));
    CHECK(acc_value_type(r, 0, 5) == ACC_TYPE_BIGNUM);
    CHECK(text_is(r, 0, 5, "123456789012345678901234567890"));
    CHECK(acc_value_type(r, 0, 6) == ACC_TYPE_CURRENCY);
    CHECK(text_is(r, 0, 6, "19.99")); /* digits, without a currency sign */
    CHECK(acc_value_type(r, 0, 7) == ACC_TYPE_RATIONAL);
    CHECK(text_is(r, 0, 7, "1/3"));
    CHECK(acc_value_type(r, 0, 8) == ACC_TYPE_DATETIME);
    CHECK(acc_value_int64(r, 0, 8) == 1709649000); /* 2024-03-05 14:30:00 */
    CHECK(text_is(r, 0, 8, "2024-03-05 14:30:00"));
    CHECK(acc_value_type(r, 0, 9) == ACC_TYPE_TIMESTAMPTZ);
    CHECK(acc_value_int64(r, 0, 9) == 1709649000);
    CHECK(acc_value_type(r, 0, 10) == ACC_TYPE_INTERVAL);
    CHECK(acc_value_text(r, 0, 10, NULL) != NULL);
    CHECK(acc_value_type(r, 0, 11) == ACC_TYPE_JSON);
    CHECK(text_is(r, 0, 11, "{\"a\":null,\"b\":[1,2]}")); /* canonical form */

    /* The NULL row. */
    for (size_t c = 0; c < 12; c++) {
        CHECK(acc_value_type(r, 1, c) == ACC_TYPE_NULL);
    }
    CHECK(acc_value_text(r, 1, 2, NULL) == NULL);

    /* Out-of-range positions give defaults, never a crash. */
    CHECK(acc_value_type(r, 2, 0) == ACC_TYPE_NULL);
    CHECK(acc_value_type(r, 0, 99) == ACC_TYPE_NULL);
    CHECK(acc_value_int64(r, 99, 99) == 0);
    CHECK(acc_value_double(r, 99, 99) == 0.0);
    size_t len = 123;
    CHECK(acc_value_text(r, 99, 99, &len) == NULL && len == 0);
    acc_result_free(r);

    /* A statement that changes rows reports how many. */
    r = query(db, "UPDATE t SET i = 7 WHERE i = 42");
    CHECK(acc_result_affected(r) == 1);
    CHECK(acc_result_column_count(r) == 0);
    acc_result_free(r);
    acc_close(db);
}

static void test_errors(void) {
    acc_db *db = open_memory();
    CHECK_CODE(acc_exec(db, "SELEKT 1", ACC_NTS, NULL), ACC_ERR_SYNTAX);
    CHECK(strstr(acc_errmsg(db), "Syntax") != NULL);
    CHECK(acc_errcode(db) == ACC_ERR_SYNTAX);
    CHECK_CODE(acc_exec(db, "SELECT * FROM nothing", ACC_NTS, NULL), ACC_ERR_CATALOG);
    run(db, "CREATE TABLE u (k INTEGER PRIMARY KEY, v TEXT NOT NULL)");
    run(db, "INSERT INTO u VALUES (1, 'a')");
    CHECK_CODE(acc_exec(db, "INSERT INTO u VALUES (1, 'b')", ACC_NTS, NULL), ACC_ERR_CONSTRAINT);
    CHECK_CODE(acc_exec(db, "INSERT INTO u VALUES (2, NULL)", ACC_NTS, NULL), ACC_ERR_CONSTRAINT);
    CHECK_CODE(acc_exec(db, "INSERT INTO u VALUES ('x', 'b')", ACC_NTS, NULL), ACC_ERR_TYPE);
    CHECK_CODE(acc_exec(db, "SELECT 1 / 0", ACC_NTS, NULL), ACC_ERR_RUNTIME);
    /* A failed statement changed nothing, and the next call succeeds and clears the error. */
    CHECK(scalar_int(db, "SELECT Count(*) FROM u") == 1);
    CHECK(acc_errcode(db) == ACC_OK);
    CHECK(strlen(acc_errmsg(db)) == 0);

    /* Wrong use is reported, not crashed on. */
    CHECK_CODE(acc_exec(db, NULL, 0, NULL), ACC_ERR_MISUSE);
    CHECK_CODE(acc_exec(NULL, "SELECT 1", ACC_NTS, NULL), ACC_ERR_MISUSE);
    CHECK_CODE(acc_exec(db, "\xff\xfe", 2, NULL), ACC_ERR_ENCODING);
    CHECK_CODE(acc_exec(db, "SELECT '\xc3'", ACC_NTS, NULL), ACC_ERR_ENCODING); /* truncated sequence */
    CHECK_CODE(acc_open(NULL, NULL), ACC_ERR_MISUSE);
    CHECK_CODE(acc_checkpoint(NULL), ACC_ERR_MISUSE);
    CHECK_CODE(acc_set_optimizer(NULL, 1), ACC_ERR_MISUSE);
    CHECK_CODE(acc_set_durability(db, 99), ACC_ERR_MISUSE);
    CHECK(acc_in_transaction(NULL) < 0);
    CHECK(acc_errcode(NULL) == ACC_ERR_MISUSE);
    CHECK(acc_errmsg(NULL) != NULL);
    acc_close(db);
}

static void test_null_handles_never_crash(void) {
    acc_close(NULL);
    acc_stmt_free(NULL);
    acc_result_free(NULL);
    acc_buffer_free(NULL, 0);
    CHECK(acc_result_row_count(NULL) == 0);
    CHECK(acc_result_column_count(NULL) == 0);
    CHECK(acc_result_affected(NULL) == 0);
    CHECK(acc_result_column_name(NULL, 0) == NULL);
    CHECK(acc_value_type(NULL, 0, 0) == ACC_TYPE_NULL);
    CHECK(acc_value_int64(NULL, 0, 0) == 0);
    CHECK(acc_value_double(NULL, 0, 0) == 0.0);
    CHECK(acc_value_text(NULL, 0, 0, NULL) == NULL);
    CHECK(acc_stmt_param_count(NULL) < 0);
    CHECK(acc_stmt_param_name(NULL, 0) == NULL);
    CHECK(acc_stmt_param_index(NULL, "a", ACC_NTS) == -1);
    CHECK_CODE(acc_bind_null(NULL, 0), ACC_ERR_MISUSE);
    CHECK_CODE(acc_bind_int64(NULL, 0, 1), ACC_ERR_MISUSE);
    CHECK_CODE(acc_bind_double(NULL, 0, 1.0), ACC_ERR_MISUSE);
    CHECK_CODE(acc_bind_bool(NULL, 0, 1), ACC_ERR_MISUSE);
    CHECK_CODE(acc_bind_text(NULL, 0, "x", 1), ACC_ERR_MISUSE);
    CHECK_CODE(acc_bind_decimal(NULL, 0, "1", 1), ACC_ERR_MISUSE);
    CHECK_CODE(acc_bind_bignum(NULL, 0, "1", 1), ACC_ERR_MISUSE);
    CHECK_CODE(acc_bind_datetime(NULL, 0, 0), ACC_ERR_MISUSE);
    CHECK_CODE(acc_bind_timestamptz(NULL, 0, 0), ACC_ERR_MISUSE);
    CHECK_CODE(acc_stmt_clear_bindings(NULL), ACC_ERR_MISUSE);
    CHECK_CODE(acc_stmt_run(NULL, NULL), ACC_ERR_MISUSE);
    acc_stmt *s = NULL;
    CHECK_CODE(acc_prepare(NULL, "SELECT 1", ACC_NTS, &s), ACC_ERR_MISUSE);
    CHECK(s == NULL);
    uint8_t *buf = NULL;
    size_t blen = 0;
    CHECK_CODE(acc_render_report(NULL, "r", ACC_NTS, ACC_REPORT_TEXT, &buf, &blen), ACC_ERR_MISUSE);
}

static void test_prepared_statements(void) {
    acc_db *db = open_memory();
    run(db, "CREATE TABLE p (id INTEGER PRIMARY KEY, name TEXT, score DOUBLE, ok YESNO, amt DECIMAL)");
    acc_stmt *ins = NULL;
    CHECK_CODE(acc_prepare(db, "INSERT INTO p VALUES (@id, @name, @score, @ok, @amt)", ACC_NTS, &ins), ACC_OK);
    CHECK(acc_stmt_param_count(ins) == 5);
    CHECK(strcmp(acc_stmt_param_name(ins, 0), "id") == 0);
    CHECK(strcmp(acc_stmt_param_name(ins, 4), "amt") == 0);
    CHECK(acc_stmt_param_name(ins, 5) == NULL);
    CHECK(acc_stmt_param_name(ins, -1) == NULL);
    CHECK(acc_stmt_param_index(ins, "score", ACC_NTS) == 2);
    CHECK(acc_stmt_param_index(ins, "@SCORE", ACC_NTS) == 2);
    CHECK(acc_stmt_param_index(ins, "nosuch", ACC_NTS) == -1);

    /* Not every parameter is bound yet. */
    CHECK_CODE(acc_bind_int64(ins, 0, 1), ACC_OK);
    CHECK_CODE(acc_stmt_run(ins, NULL), ACC_ERR_MISUSE);
    CHECK(strstr(acc_errmsg(db), "@name") != NULL);
    CHECK_CODE(acc_bind_int64(ins, 5, 1), ACC_ERR_MISUSE);
    CHECK_CODE(acc_bind_int64(ins, -1, 1), ACC_ERR_MISUSE);

    for (int i = 0; i < 100; i++) {
        char name[32];
        int n = snprintf(name, sizeof name, "row %d", i);
        CHECK_CODE(acc_bind_int64(ins, 0, i), ACC_OK);
        CHECK_CODE(acc_bind_text(ins, 1, name, (size_t)n), ACC_OK);
        CHECK_CODE(acc_bind_double(ins, 2, i / 4.0), ACC_OK);
        CHECK_CODE(acc_bind_bool(ins, 3, i % 2), ACC_OK);
        CHECK_CODE(acc_bind_decimal(ins, 4, "10.05", ACC_NTS), ACC_OK);
        CHECK_CODE(acc_stmt_run(ins, NULL), ACC_OK);
    }
    CHECK(scalar_int(db, "SELECT Count(*) FROM p") == 100);
    /* A duplicate key is a constraint error, and the statement can be run again afterwards. */
    CHECK_CODE(acc_bind_int64(ins, 0, 5), ACC_OK);
    CHECK_CODE(acc_stmt_run(ins, NULL), ACC_ERR_CONSTRAINT);
    CHECK_CODE(acc_bind_int64(ins, 0, 1000), ACC_OK);
    CHECK_CODE(acc_stmt_run(ins, NULL), ACC_OK);

    /* NULL is a value; clearing the bindings makes them missing again. */
    CHECK_CODE(acc_bind_int64(ins, 0, 2000), ACC_OK);
    CHECK_CODE(acc_bind_null(ins, 1), ACC_OK);
    CHECK_CODE(acc_bind_null(ins, 2), ACC_OK);
    CHECK_CODE(acc_bind_null(ins, 3), ACC_OK);
    CHECK_CODE(acc_bind_null(ins, 4), ACC_OK);
    CHECK_CODE(acc_stmt_run(ins, NULL), ACC_OK);
    CHECK_CODE(acc_stmt_clear_bindings(ins), ACC_OK);
    CHECK_CODE(acc_stmt_run(ins, NULL), ACC_ERR_MISUSE);

    /* Bad values are refused with a type or encoding error and leave the binding alone. */
    CHECK_CODE(acc_bind_double(ins, 2, NAN), ACC_ERR_TYPE);
    CHECK_CODE(acc_bind_double(ins, 2, INFINITY), ACC_ERR_TYPE);
    CHECK_CODE(acc_bind_decimal(ins, 4, "twelve", ACC_NTS), ACC_ERR_TYPE);
    CHECK_CODE(acc_bind_bignum(ins, 4, "1.5", ACC_NTS), ACC_ERR_TYPE);
    CHECK_CODE(acc_bind_text(ins, 1, "\xff", 1), ACC_ERR_ENCODING);
    CHECK_CODE(acc_bind_text(ins, 1, NULL, 0), ACC_ERR_MISUSE);
    acc_stmt_free(ins);

    /* A query with parameters, used over and over. */
    acc_stmt *sel = NULL;
    CHECK_CODE(acc_prepare(db, "SELECT name, amt FROM p WHERE id = @id AND ok = @ok", ACC_NTS, &sel), ACC_OK);
    for (int i = 0; i < 100; i += 7) {
        CHECK_CODE(acc_bind_int64(sel, 0, i), ACC_OK);
        CHECK_CODE(acc_bind_bool(sel, 1, i % 2), ACC_OK);
        acc_result *r = NULL;
        CHECK_CODE(acc_stmt_run(sel, &r), ACC_OK);
        CHECK(acc_result_row_count(r) == 1);
        char want[32];
        snprintf(want, sizeof want, "row %d", i);
        CHECK(text_is(r, 0, 0, want));
        CHECK(text_is(r, 0, 1, "10.05"));
        acc_result_free(r);
    }
    acc_stmt_free(sel);

    /* Exact numbers, big numbers and times go in as themselves. */
    run(db, "CREATE TABLE q (big BIGNUM, dec DECIMAL, cur CURRENCY, dt DATETIME, tz TIMESTAMPTZ)");
    acc_stmt *ex = NULL;
    CHECK_CODE(acc_prepare(db, "INSERT INTO q VALUES (@a, @b, @c, @d, @e) RETURNING big, dec, cur, dt, tz", ACC_NTS, &ex), ACC_OK);
    CHECK_CODE(acc_bind_bignum(ex, 0, "-123456789012345678901234567890", ACC_NTS), ACC_OK);
    CHECK_CODE(acc_bind_decimal(ex, 1, "0.1", ACC_NTS), ACC_OK);
    CHECK_CODE(acc_bind_decimal(ex, 2, "1234.50", ACC_NTS), ACC_OK);
    CHECK_CODE(acc_bind_datetime(ex, 3, 1709649000), ACC_OK);
    CHECK_CODE(acc_bind_timestamptz(ex, 4, 1709649000), ACC_OK);
    acc_result *r = NULL;
    CHECK_CODE(acc_stmt_run(ex, &r), ACC_OK);
    CHECK(acc_result_affected(r) == 1);
    CHECK(text_is(r, 0, 0, "-123456789012345678901234567890"));
    CHECK(text_is(r, 0, 1, "0.1"));
    CHECK(acc_value_type(r, 0, 2) == ACC_TYPE_CURRENCY && text_is(r, 0, 2, "1234.5"));
    CHECK(acc_value_int64(r, 0, 3) == 1709649000);
    CHECK(acc_value_int64(r, 0, 4) == 1709649000);
    acc_result_free(r);
    acc_stmt_free(ex);

    /* A script with several statements: the result is the last one's. */
    acc_stmt *script = NULL;
    CHECK_CODE(acc_prepare(db, "DELETE FROM q WHERE big < @x; SELECT Count(*) FROM q", ACC_NTS, &script), ACC_OK);
    CHECK_CODE(acc_bind_int64(script, 0, 0), ACC_OK);
    CHECK_CODE(acc_stmt_run(script, &r), ACC_OK);
    CHECK(acc_value_int64(r, 0, 0) == 0);
    acc_result_free(r);
    acc_stmt_free(script);

    /* Bad SQL does not make a statement. */
    acc_stmt *bad = (acc_stmt *)0x1;
    CHECK_CODE(acc_prepare(db, "SELEKT", ACC_NTS, &bad), ACC_ERR_SYNTAX);
    CHECK(bad == NULL);
    CHECK_CODE(acc_prepare(db, "", ACC_NTS, &bad), ACC_ERR_SYNTAX);
    acc_close(db);
}

static void test_text_edge_cases(void) {
    acc_db *db = open_memory();
    run(db, "CREATE TABLE s (id INTEGER, t TEXT)");
    acc_stmt *ins = NULL;
    CHECK_CODE(acc_prepare(db, "INSERT INTO s VALUES (@id, @t)", ACC_NTS, &ins), ACC_OK);

    /* Unicode, an empty string (which is not NULL) and an embedded NUL byte survive the round trip. */
    const char uni[] = "h\xc3\xa9llo \xe4\xb8\x96\xe7\x95\x8c \xf0\x9f\x98\x80";
    CHECK_CODE(acc_bind_int64(ins, 0, 1), ACC_OK);
    CHECK_CODE(acc_bind_text(ins, 1, uni, sizeof uni - 1), ACC_OK);
    CHECK_CODE(acc_stmt_run(ins, NULL), ACC_OK);
    CHECK_CODE(acc_bind_int64(ins, 0, 2), ACC_OK);
    CHECK_CODE(acc_bind_text(ins, 1, "", 0), ACC_OK);
    CHECK_CODE(acc_stmt_run(ins, NULL), ACC_OK);
    const char nul[] = {'a', '\0', 'b'};
    CHECK_CODE(acc_bind_int64(ins, 0, 3), ACC_OK);
    CHECK_CODE(acc_bind_text(ins, 1, nul, 3), ACC_OK);
    CHECK_CODE(acc_stmt_run(ins, NULL), ACC_OK);
    /* NUL-terminated mode stops at the NUL. */
    CHECK_CODE(acc_bind_int64(ins, 0, 4), ACC_OK);
    CHECK_CODE(acc_bind_text(ins, 1, nul, ACC_NTS), ACC_OK);
    CHECK_CODE(acc_stmt_run(ins, NULL), ACC_OK);
    /* A megabyte. */
    size_t big = 1 << 20;
    char *mb = malloc(big);
    CHECK(mb != NULL);
    memset(mb, 'x', big);
    CHECK_CODE(acc_bind_int64(ins, 0, 5), ACC_OK);
    CHECK_CODE(acc_bind_text(ins, 1, mb, big), ACC_OK);
    CHECK_CODE(acc_stmt_run(ins, NULL), ACC_OK);
    free(mb);
    acc_stmt_free(ins);

    acc_result *r = query(db, "SELECT t FROM s ORDER BY id");
    CHECK(acc_result_row_count(r) == 5);
    size_t len = 0;
    const char *t = acc_value_text(r, 0, 0, &len);
    CHECK(t != NULL && len == sizeof uni - 1 && memcmp(t, uni, len) == 0);
    t = acc_value_text(r, 1, 0, &len);
    CHECK(t != NULL && len == 0 && t[0] == '\0'); /* empty, not NULL */
    CHECK(acc_value_type(r, 1, 0) == ACC_TYPE_TEXT);
    t = acc_value_text(r, 2, 0, &len);
    CHECK(t != NULL && len == 3 && memcmp(t, nul, 3) == 0);
    t = acc_value_text(r, 3, 0, &len);
    CHECK(t != NULL && len == 1 && t[0] == 'a');
    acc_value_text(r, 4, 0, &len);
    CHECK(len == big);
    acc_result_free(r);

    /* Quotes and special characters in SQL text, and a statement with a length rather than a NUL. */
    const char sql[] = "SELECT 'it''s' AS q; SELECT 1; garbage after the length";
    acc_result *r2 = NULL;
    CHECK_CODE(acc_exec(db, sql, strlen("SELECT 'it''s' AS q"), &r2), ACC_OK);
    CHECK(text_is(r2, 0, 0, "it's"));
    acc_result_free(r2);
    CHECK_CODE(acc_exec(db, sql, sizeof sql - 1, NULL), ACC_ERR_SYNTAX);
    acc_close(db);
}

static void test_transactions_and_options(void) {
    acc_db *db = open_memory();
    run(db, "CREATE TABLE x (n INTEGER)");
    CHECK(acc_in_transaction(db) == 0);
    run(db, "BEGIN");
    CHECK(acc_in_transaction(db) == 1);
    run(db, "INSERT INTO x VALUES (1)");
    run(db, "ROLLBACK");
    CHECK(acc_in_transaction(db) == 0);
    CHECK(scalar_int(db, "SELECT Count(*) FROM x") == 0);
    run(db, "BEGIN; INSERT INTO x VALUES (1); INSERT INTO x VALUES (2); COMMIT");
    CHECK(scalar_int(db, "SELECT Count(*) FROM x") == 2);
    CHECK_CODE(acc_exec(db, "COMMIT", ACC_NTS, NULL), ACC_ERR_RUNTIME);
    CHECK_CODE(acc_set_optimizer(db, 0), ACC_OK);
    CHECK(scalar_int(db, "SELECT Count(*) FROM x WHERE n > 1") == 1);
    CHECK_CODE(acc_set_optimizer(db, 1), ACC_OK);
    CHECK_CODE(acc_set_durability(db, ACC_DURABILITY_OS_BUFFERED), ACC_OK);
    CHECK_CODE(acc_set_durability(db, ACC_DURABILITY_FSYNC), ACC_OK);
    CHECK_CODE(acc_checkpoint(db), ACC_OK); /* in memory: nothing to do */
    acc_close(db);
}

static char tmpdir[256];
static char self_path[4096];

static void make_tmpdir(void) {
    snprintf(tmpdir, sizeof tmpdir, "/tmp/accessor-ctest-XXXXXX");
    CHECK(mkdtemp(tmpdir) != NULL);
}

static void remove_tmpdir(void) {
    char cmd[400];
    snprintf(cmd, sizeof cmd, "rm -rf '%s'", tmpdir);
    if (system(cmd) != 0) {
        fprintf(stderr, "could not remove %s\n", tmpdir);
    }
}

static void test_files_and_locking(void) {
    char path[300];
    snprintf(path, sizeof path, "%s/shop.adb", tmpdir);
    acc_db *db = NULL;
    CHECK_CODE(acc_open(path, &db), ACC_OK);
    run(db, "CREATE TABLE c (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO c VALUES (1, 'Ada'), (2, 'Grace')");

    /* A second connection to the same file, in this process, is refused with a handle that explains. */
    acc_db *second = NULL;
    CHECK_CODE(acc_open(path, &second), ACC_ERR_LOCKED);
    CHECK(second != NULL);
    CHECK(acc_errcode(second) == ACC_ERR_LOCKED);
    CHECK(strstr(acc_errmsg(second), "in use") != NULL);
    CHECK_CODE(acc_exec(second, "SELECT 1", ACC_NTS, NULL), ACC_ERR_MISUSE); /* a failed open runs nothing */
    acc_close(second);

    /* ...and so is another process. */
    fflush(NULL);
    pid_t pid = fork();
    CHECK(pid >= 0);
    if (pid == 0) {
        /* A fresh copy of this program, not the forked image: the parent's database thread does not
           exist in a forked child, and what it owns would look like a leak. */
        execl(self_path, self_path, "--try-open", path, (char *)NULL);
        _exit(4);
    }
    int status = 0;
    CHECK(waitpid(pid, &status, 0) == pid);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);

    /* The refused attempts changed nothing. */
    run(db, "INSERT INTO c VALUES (3, 'Linus')");
    CHECK_CODE(acc_checkpoint(db), ACC_OK);
    acc_close(db);

    /* Closed: the lock is gone and the data is there. */
    CHECK_CODE(acc_open(path, &db), ACC_OK);
    CHECK(scalar_int(db, "SELECT Count(*) FROM c") == 3);
    acc_result *r = query(db, "SELECT name FROM c WHERE id = 2");
    CHECK(text_is(r, 0, 0, "Grace"));
    acc_result_free(r);

    /* Statements outlive neither the connection nor their own failure. */
    acc_stmt *s = NULL;
    CHECK_CODE(acc_prepare(db, "SELECT name FROM c WHERE id = @id", ACC_NTS, &s), ACC_OK);
    CHECK_CODE(acc_bind_int64(s, 0, 1), ACC_OK);
    acc_close(db);
    CHECK_CODE(acc_stmt_run(s, NULL), ACC_ERR_MISUSE);
    acc_stmt_free(s); /* allowed after the connection is closed */

    /* A file that is not a database. */
    char junk[300];
    snprintf(junk, sizeof junk, "%s/junk.adb", tmpdir);
    FILE *f = fopen(junk, "wb");
    CHECK(f != NULL);
    fputs("this is not an Accessor database, only text that is long enough to have a header", f);
    fclose(f);
    CHECK_CODE(acc_open(junk, &db), ACC_ERR_CORRUPT);
    CHECK(db != NULL && strlen(acc_errmsg(db)) > 0);
    acc_close(db);

    /* A path that cannot be a file. */
    char bad[300];
    snprintf(bad, sizeof bad, "%s/no/such/dir/x.adb", tmpdir);
    CHECK_CODE(acc_open(bad, &db), ACC_ERR_IO);
    acc_close(db);
    CHECK_CODE(acc_open("\xff\xfe", &db), ACC_ERR_ENCODING);
    acc_close(db);
}

static void test_reports(void) {
    acc_db *db = open_memory();
    run(db, "CREATE TABLE sales (region TEXT, amount CURRENCY)");
    run(db, "INSERT INTO sales VALUES ('N', 10.50), ('N', 4.25), ('S', 100)");
    run(db,
        "CREATE REPORT by_region TITLE 'Sales' BREAK BY region COLUMN amount FORMAT '#,##0.00' SUM"
        " GRAND TOTAL AS SELECT region, amount FROM sales");
    uint8_t *buf = NULL;
    size_t len = 0;
    CHECK_CODE(acc_render_report(db, "by_region", ACC_NTS, ACC_REPORT_TEXT, &buf, &len), ACC_OK);
    CHECK(buf != NULL && len > 20);
    CHECK(memmem(buf, len, "Sales", 5) != NULL);
    CHECK(memmem(buf, len, "114.75", 6) != NULL);
    acc_buffer_free(buf, len);
    CHECK_CODE(acc_render_report(db, "by_region", ACC_NTS, ACC_REPORT_PDF, &buf, &len), ACC_OK);
    CHECK(len > 100 && memcmp(buf, "%PDF-1.4", 8) == 0);
    acc_buffer_free(buf, len);
    CHECK_CODE(acc_render_report(db, "by_region", ACC_NTS, ACC_REPORT_HTML, &buf, &len), ACC_OK);
    CHECK(memcmp(buf, "<!DOCTYPE html>", 15) == 0);
    acc_buffer_free(buf, len);
    CHECK_CODE(acc_render_report(db, "by_region", ACC_NTS, ACC_REPORT_CSV, &buf, &len), ACC_OK);
    acc_buffer_free(buf, len);
    CHECK_CODE(acc_render_report(db, "by_region", ACC_NTS, ACC_REPORT_MARKDOWN, &buf, &len), ACC_OK);
    acc_buffer_free(buf, len);
    CHECK_CODE(acc_render_report(db, "nosuch", ACC_NTS, ACC_REPORT_TEXT, &buf, &len), ACC_ERR_CATALOG);
    CHECK(buf == NULL && len == 0);
    CHECK_CODE(acc_render_report(db, "by_region", ACC_NTS, 99, &buf, &len), ACC_ERR_MISUSE);
    CHECK_CODE(acc_render_report(db, "by_region", ACC_NTS, ACC_REPORT_TEXT, NULL, &len), ACC_ERR_MISUSE);
    acc_close(db);
}

static void test_a_large_result(void) {
    acc_db *db = open_memory();
    acc_result *r = query(db, "SELECT generate_series AS n, 'row ' & generate_series AS s FROM GENERATE_SERIES(1, 100000)");
    CHECK(acc_result_row_count(r) == 100000);
    int64_t sum = 0;
    for (size_t i = 0; i < acc_result_row_count(r); i++) {
        sum += acc_value_int64(r, i, 0);
    }
    CHECK(sum == (int64_t)100000 * 100001 / 2);
    CHECK(text_is(r, 99999, 1, "row 100000"));
    acc_result_free(r);
    acc_close(db);
}

typedef struct {
    acc_db *db;
    int id;
    int ok;
} worker_arg;

static void *worker(void *p) {
    worker_arg *a = p;
    a->ok = 1;
    acc_stmt *ins = NULL;
    if (acc_prepare(a->db, "INSERT INTO shared VALUES (@w, @i)", ACC_NTS, &ins) != ACC_OK) {
        a->ok = 0;
        return NULL;
    }
    for (int i = 0; i < 200; i++) {
        if (acc_bind_int64(ins, 0, a->id) != ACC_OK || acc_bind_int64(ins, 1, i) != ACC_OK || acc_stmt_run(ins, NULL) != ACC_OK) {
            a->ok = 0;
        }
        acc_result *r = NULL;
        if (acc_exec(a->db, "SELECT Count(*) FROM shared", ACC_NTS, &r) != ACC_OK) {
            a->ok = 0;
        }
        acc_result_free(r);
    }
    acc_stmt_free(ins);
    return NULL;
}

static void test_threads_share_a_connection(void) {
    acc_db *db = open_memory();
    run(db, "CREATE TABLE shared (w INTEGER, i INTEGER)");
    enum { N = 8 };
    pthread_t th[N];
    worker_arg args[N];
    for (int k = 0; k < N; k++) {
        args[k] = (worker_arg){db, k, 0};
        CHECK(pthread_create(&th[k], NULL, worker, &args[k]) == 0);
    }
    for (int k = 0; k < N; k++) {
        pthread_join(th[k], NULL);
        CHECK(args[k].ok);
    }
    CHECK(scalar_int(db, "SELECT Count(*) FROM shared") == N * 200);
    CHECK(scalar_int(db, "SELECT Count(DISTINCT w) FROM shared") == N);
    acc_close(db);
}

static void test_open_and_close_many(void) {
    /* Connections come and go without leaking threads or memory. */
    for (int i = 0; i < 50; i++) {
        acc_db *db = open_memory();
        run(db, "CREATE TABLE t (n INTEGER); INSERT INTO t VALUES (1)");
        acc_stmt *s = NULL;
        CHECK_CODE(acc_prepare(db, "SELECT n FROM t", ACC_NTS, &s), ACC_OK);
        acc_result *r = NULL;
        CHECK_CODE(acc_stmt_run(s, &r), ACC_OK);
        acc_result_free(r);
        if (i % 2) {
            acc_stmt_free(s);
            acc_close(db);
        } else {
            acc_close(db);
            acc_stmt_free(s);
        }
    }
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "--try-open") == 0) {
        /* Helper for test_files_and_locking: try to open a database that another process has open. */
        acc_db *other = NULL;
        int code = acc_open(argv[2], &other);
        acc_close(other);
        return code == ACC_ERR_LOCKED ? 0 : 3;
    }
    if (realpath(argv[0], self_path) == NULL) {
        fprintf(stderr, "cannot find myself: %s\n", argv[0]);
        return 2;
    }
    make_tmpdir();
    test_version();
    test_basics_and_types();
    test_errors();
    test_null_handles_never_crash();
    test_prepared_statements();
    test_text_edge_cases();
    test_transactions_and_options();
    test_files_and_locking();
    test_reports();
    test_a_large_result();
    test_threads_share_a_connection();
    test_open_and_close_many();
    remove_tmpdir();
    printf("%d checks, %d failed\n", checks, failures);
    return failures == 0 ? 0 : 1;
}
