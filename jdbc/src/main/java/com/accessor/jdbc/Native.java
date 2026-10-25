package com.accessor.jdbc;

import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.SymbolLookup;
import java.lang.invoke.MethodHandle;
import java.nio.charset.StandardCharsets;
import java.sql.SQLException;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_BYTE;
import static java.lang.foreign.ValueLayout.JAVA_DOUBLE;
import static java.lang.foreign.ValueLayout.JAVA_INT;
import static java.lang.foreign.ValueLayout.JAVA_LONG;

/**
 * The C interface of Accessor ({@code accessor.h}), bound with the Java foreign-function API.
 * Everything that touches native memory is in this class; the rest of the driver sees Java objects.
 *
 * <p>Sizes ({@code size_t}) are taken to be 64 bits, which is the only case the library supports.
 */
final class Native {
    private Native() {}

    // Error codes (ACC_ERR_*).
    static final int OK = 0;
    static final int ERR_SYNTAX = 1;
    static final int ERR_CATALOG = 2;
    static final int ERR_CONSTRAINT = 3;
    static final int ERR_TYPE = 4;
    static final int ERR_RUNTIME = 5;
    static final int ERR_IO = 6;
    static final int ERR_CORRUPT = 7;
    static final int ERR_LOCKED = 8;
    static final int ERR_MISUSE = 9;
    static final int ERR_ENCODING = 10;
    static final int ERR_INTERNAL = 11;

    // Value type tags (ACC_TYPE_*).
    static final int T_NULL = 0;
    static final int T_INTEGER = 1;
    static final int T_DOUBLE = 2;
    static final int T_TEXT = 3;
    static final int T_BOOL = 4;
    static final int T_DECIMAL = 5;
    static final int T_BIGNUM = 6;
    static final int T_CURRENCY = 7;
    static final int T_RATIONAL = 8;
    static final int T_DATETIME = 9;
    static final int T_TIMESTAMPTZ = 10;
    static final int T_INTERVAL = 11;
    static final int T_JSON = 12;

    static final int DURABILITY_FSYNC = 0;
    static final int DURABILITY_OS_BUFFERED = 1;

    static final int REPORT_TEXT = 0;
    static final int REPORT_HTML = 1;
    static final int REPORT_MARKDOWN = 2;
    static final int REPORT_CSV = 3;
    static final int REPORT_PDF = 4;

    /** The ABI version this driver was written for. */
    static final int EXPECTED_ABI = 1;

    private static final Linker LINKER = Linker.nativeLinker();
    private static final SymbolLookup LIB = NativeLoader.lookup();

    private static MethodHandle fn(String name, FunctionDescriptor d) {
        MemorySegment sym = LIB.find(name).orElseThrow(() -> new UnsatisfiedLinkError("the Accessor library has no function " + name));
        return LINKER.downcallHandle(sym, d);
    }

    private static final MethodHandle VERSION = fn("acc_version", FunctionDescriptor.of(ADDRESS));
    private static final MethodHandle VERSION_NUMBER = fn("acc_version_number", FunctionDescriptor.of(JAVA_INT));
    private static final MethodHandle ABI_VERSION = fn("acc_abi_version", FunctionDescriptor.of(JAVA_INT));
    private static final MethodHandle FILE_FORMAT = fn("acc_file_format_version", FunctionDescriptor.of(JAVA_INT));
    private static final MethodHandle OPEN = fn("acc_open", FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS));
    private static final MethodHandle CLOSE = fn("acc_close", FunctionDescriptor.ofVoid(ADDRESS));
    private static final MethodHandle ERRCODE = fn("acc_errcode", FunctionDescriptor.of(JAVA_INT, ADDRESS));
    private static final MethodHandle ERRMSG = fn("acc_errmsg", FunctionDescriptor.of(ADDRESS, ADDRESS));
    private static final MethodHandle EXEC = fn("acc_exec", FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_LONG, ADDRESS));
    private static final MethodHandle CHECKPOINT = fn("acc_checkpoint", FunctionDescriptor.of(JAVA_INT, ADDRESS));
    private static final MethodHandle SET_OPTIMIZER = fn("acc_set_optimizer", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT));
    private static final MethodHandle SET_DURABILITY = fn("acc_set_durability", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT));
    private static final MethodHandle IN_TRANSACTION = fn("acc_in_transaction", FunctionDescriptor.of(JAVA_INT, ADDRESS));
    private static final MethodHandle RENDER_REPORT =
            fn("acc_render_report", FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_LONG, JAVA_INT, ADDRESS, ADDRESS));
    private static final MethodHandle BUFFER_FREE = fn("acc_buffer_free", FunctionDescriptor.ofVoid(ADDRESS, JAVA_LONG));
    private static final MethodHandle PREPARE = fn("acc_prepare", FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_LONG, ADDRESS));
    private static final MethodHandle STMT_FREE = fn("acc_stmt_free", FunctionDescriptor.ofVoid(ADDRESS));
    private static final MethodHandle PARAM_COUNT = fn("acc_stmt_param_count", FunctionDescriptor.of(JAVA_INT, ADDRESS));
    private static final MethodHandle PARAM_NAME = fn("acc_stmt_param_name", FunctionDescriptor.of(ADDRESS, ADDRESS, JAVA_INT));
    private static final MethodHandle PARAM_INDEX = fn("acc_stmt_param_index", FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_LONG));
    private static final MethodHandle BIND_NULL = fn("acc_bind_null", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT));
    private static final MethodHandle BIND_INT64 = fn("acc_bind_int64", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, JAVA_LONG));
    private static final MethodHandle BIND_DOUBLE = fn("acc_bind_double", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, JAVA_DOUBLE));
    private static final MethodHandle BIND_BOOL = fn("acc_bind_bool", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, JAVA_INT));
    private static final MethodHandle BIND_TEXT = fn("acc_bind_text", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, ADDRESS, JAVA_LONG));
    private static final MethodHandle BIND_DECIMAL = fn("acc_bind_decimal", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, ADDRESS, JAVA_LONG));
    private static final MethodHandle BIND_BIGNUM = fn("acc_bind_bignum", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, ADDRESS, JAVA_LONG));
    private static final MethodHandle BIND_DATETIME = fn("acc_bind_datetime", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, JAVA_LONG));
    private static final MethodHandle BIND_TIMESTAMPTZ = fn("acc_bind_timestamptz", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, JAVA_LONG));
    private static final MethodHandle CLEAR_BINDINGS = fn("acc_stmt_clear_bindings", FunctionDescriptor.of(JAVA_INT, ADDRESS));
    private static final MethodHandle STMT_RUN = fn("acc_stmt_run", FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS));
    private static final MethodHandle RESULT_FREE = fn("acc_result_free", FunctionDescriptor.ofVoid(ADDRESS));
    private static final MethodHandle ROW_COUNT = fn("acc_result_row_count", FunctionDescriptor.of(JAVA_LONG, ADDRESS));
    private static final MethodHandle COLUMN_COUNT = fn("acc_result_column_count", FunctionDescriptor.of(JAVA_LONG, ADDRESS));
    private static final MethodHandle AFFECTED = fn("acc_result_affected", FunctionDescriptor.of(JAVA_LONG, ADDRESS));
    private static final MethodHandle COLUMN_NAME = fn("acc_result_column_name", FunctionDescriptor.of(ADDRESS, ADDRESS, JAVA_LONG));
    private static final MethodHandle VALUE_TYPE = fn("acc_value_type", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_LONG, JAVA_LONG));
    private static final MethodHandle VALUE_INT64 = fn("acc_value_int64", FunctionDescriptor.of(JAVA_LONG, ADDRESS, JAVA_LONG, JAVA_LONG));
    private static final MethodHandle VALUE_DOUBLE = fn("acc_value_double", FunctionDescriptor.of(JAVA_DOUBLE, ADDRESS, JAVA_LONG, JAVA_LONG));
    private static final MethodHandle VALUE_TEXT = fn("acc_value_text", FunctionDescriptor.of(ADDRESS, ADDRESS, JAVA_LONG, JAVA_LONG, ADDRESS));

    // ----- call helpers ------------------------------------------------------------------------

    @FunctionalInterface
    private interface Call<T> {
        T run() throws Throwable;
    }

    private static <T> T call(Call<T> c) {
        try {
            return c.run();
        } catch (RuntimeException | Error e) {
            throw e;
        } catch (Throwable t) {
            throw new IllegalStateException("native call failed", t);
        }
    }

    /** The text of a NUL-terminated native string. */
    static String cString(MemorySegment p) {
        if (p.equals(MemorySegment.NULL)) {
            return null;
        }
        return p.reinterpret(Long.MAX_VALUE).getString(0);
    }

    /** UTF-8 bytes in native memory owned by {@code arena}; never NULL, even for empty text. */
    static MemorySegment bytes(Arena arena, byte[] b) {
        MemorySegment seg = arena.allocate(Math.max(1, b.length));
        MemorySegment.copy(b, 0, seg, JAVA_BYTE, 0, b.length);
        return seg;
    }

    static MemorySegment utf8(Arena arena, String s) {
        return bytes(arena, s.getBytes(StandardCharsets.UTF_8));
    }

    // ----- library facts -----------------------------------------------------------------------

    static String version() {
        return cString(call(() -> (MemorySegment) VERSION.invokeExact()));
    }

    static int versionNumber() {
        return call(() -> (int) VERSION_NUMBER.invokeExact());
    }

    static int abiVersion() {
        return call(() -> (int) ABI_VERSION.invokeExact());
    }

    static int fileFormatVersion() {
        return call(() -> (int) FILE_FORMAT.invokeExact());
    }

    // ----- connections -------------------------------------------------------------------------

    /** An {@code acc_db *}. */
    static final class Db {
        private MemorySegment handle;

        private Db(MemorySegment handle) {
            this.handle = handle;
        }

        /** Opens {@code path} (null for a database in memory). */
        static Db open(String path) throws SQLException {
            try (Arena a = Arena.ofConfined()) {
                MemorySegment out = a.allocate(ADDRESS);
                MemorySegment p = path == null ? MemorySegment.NULL : a.allocateFrom(path);
                int code = call(() -> (int) OPEN.invokeExact(p, out));
                MemorySegment h = out.get(ADDRESS, 0);
                Db db = h.equals(MemorySegment.NULL) ? null : new Db(h);
                if (code != OK) {
                    String msg = db == null ? "cannot open the database (code " + code + ")" : db.message();
                    if (db != null) {
                        db.close();
                    }
                    throw error(code, msg);
                }
                return db;
            }
        }

        synchronized void close() {
            if (handle != null) {
                MemorySegment h = handle;
                handle = null;
                call(() -> {
                    CLOSE.invokeExact(h);
                    return null;
                });
            }
        }

        synchronized boolean isClosed() {
            return handle == null;
        }

        private MemorySegment h() throws SQLException {
            if (handle == null) {
                throw new SQLException("the connection is closed", "08003");
            }
            return handle;
        }

        String message() {
            return cString(call(() -> (MemorySegment) ERRMSG.invokeExact(handle)));
        }

        private void check(int code) throws SQLException {
            if (code != OK) {
                throw error(code, message());
            }
        }

        synchronized Result exec(String sql) throws SQLException {
            try (Arena a = Arena.ofConfined()) {
                byte[] b = sql.getBytes(StandardCharsets.UTF_8);
                MemorySegment text = bytes(a, b);
                MemorySegment out = a.allocate(ADDRESS);
                MemorySegment hh = h();
                int code = call(() -> (int) EXEC.invokeExact(hh, text, (long) b.length, out));
                check(code);
                return new Result(out.get(ADDRESS, 0));
            }
        }

        synchronized void run(String sql) throws SQLException {
            exec(sql).close();
        }

        synchronized Stmt prepare(String sql) throws SQLException {
            try (Arena a = Arena.ofConfined()) {
                byte[] b = sql.getBytes(StandardCharsets.UTF_8);
                MemorySegment text = bytes(a, b);
                MemorySegment out = a.allocate(ADDRESS);
                MemorySegment hh = h();
                int code = call(() -> (int) PREPARE.invokeExact(hh, text, (long) b.length, out));
                check(code);
                return new Stmt(this, out.get(ADDRESS, 0));
            }
        }

        synchronized boolean inTransaction() throws SQLException {
            MemorySegment hh = h();
            int r = call(() -> (int) IN_TRANSACTION.invokeExact(hh));
            if (r < 0) {
                throw error(-r, message());
            }
            return r == 1;
        }

        synchronized void checkpoint() throws SQLException {
            MemorySegment hh = h();
            check(call(() -> (int) CHECKPOINT.invokeExact(hh)));
        }

        synchronized void setOptimizer(boolean on) throws SQLException {
            MemorySegment hh = h();
            check(call(() -> (int) SET_OPTIMIZER.invokeExact(hh, on ? 1 : 0)));
        }

        synchronized void setDurability(int mode) throws SQLException {
            MemorySegment hh = h();
            check(call(() -> (int) SET_DURABILITY.invokeExact(hh, mode)));
        }

        synchronized byte[] renderReport(String name, int format) throws SQLException {
            try (Arena a = Arena.ofConfined()) {
                byte[] b = name.getBytes(StandardCharsets.UTF_8);
                MemorySegment text = bytes(a, b);
                MemorySegment out = a.allocate(ADDRESS);
                MemorySegment len = a.allocate(JAVA_LONG);
                MemorySegment hh = h();
                int code = call(() -> (int) RENDER_REPORT.invokeExact(hh, text, (long) b.length, format, out, len));
                check(code);
                MemorySegment buf = out.get(ADDRESS, 0);
                long n = len.get(JAVA_LONG, 0);
                try {
                    return buf.reinterpret(n).toArray(JAVA_BYTE);
                } finally {
                    call(() -> {
                        BUFFER_FREE.invokeExact(buf, n);
                        return null;
                    });
                }
            }
        }
    }

    // ----- prepared statements -----------------------------------------------------------------

    /** An {@code acc_stmt *}. */
    static final class Stmt {
        private final Db db;
        private MemorySegment handle;
        private final String[] names;

        private Stmt(Db db, MemorySegment handle) {
            this.db = db;
            this.handle = handle;
            int n = call(() -> (int) PARAM_COUNT.invokeExact(handle));
            names = new String[Math.max(n, 0)];
            for (int i = 0; i < names.length; i++) {
                int idx = i;
                names[i] = cString(call(() -> (MemorySegment) PARAM_NAME.invokeExact(handle, idx)));
            }
        }

        int paramCount() {
            return names.length;
        }

        /** The 0-based position of parameter {@code name}, or -1. */
        int indexOf(String name) {
            for (int i = 0; i < names.length; i++) {
                if (names[i].equalsIgnoreCase(name)) {
                    return i;
                }
            }
            return -1;
        }

        private MemorySegment h() throws SQLException {
            if (handle == null) {
                throw new SQLException("the statement is closed", "HY010");
            }
            return handle;
        }

        private void check(int code) throws SQLException {
            if (code != OK) {
                throw error(code, db.message());
            }
        }

        synchronized void close() {
            if (handle != null) {
                MemorySegment h = handle;
                handle = null;
                call(() -> {
                    STMT_FREE.invokeExact(h);
                    return null;
                });
            }
        }

        synchronized void bindNull(int i) throws SQLException {
            MemorySegment h = h();
            check(call(() -> (int) BIND_NULL.invokeExact(h, i)));
        }

        synchronized void bindLong(int i, long v) throws SQLException {
            MemorySegment h = h();
            check(call(() -> (int) BIND_INT64.invokeExact(h, i, v)));
        }

        synchronized void bindDouble(int i, double v) throws SQLException {
            MemorySegment h = h();
            check(call(() -> (int) BIND_DOUBLE.invokeExact(h, i, v)));
        }

        synchronized void bindBool(int i, boolean v) throws SQLException {
            MemorySegment h = h();
            check(call(() -> (int) BIND_BOOL.invokeExact(h, i, v ? 1 : 0)));
        }

        synchronized void bindText(int i, String s) throws SQLException {
            bindWithText(BIND_TEXT, i, s);
        }

        synchronized void bindDecimal(int i, String digits) throws SQLException {
            bindWithText(BIND_DECIMAL, i, digits);
        }

        synchronized void bindBignum(int i, String digits) throws SQLException {
            bindWithText(BIND_BIGNUM, i, digits);
        }

        private void bindWithText(MethodHandle fn, int i, String s) throws SQLException {
            try (Arena a = Arena.ofConfined()) {
                byte[] b = s.getBytes(StandardCharsets.UTF_8);
                MemorySegment text = bytes(a, b);
                MemorySegment h = h();
                check(call(() -> (int) fn.invokeExact(h, i, text, (long) b.length)));
            }
        }

        synchronized void bindDatetime(int i, long seconds) throws SQLException {
            MemorySegment h = h();
            check(call(() -> (int) BIND_DATETIME.invokeExact(h, i, seconds)));
        }

        synchronized void bindTimestamptz(int i, long seconds) throws SQLException {
            MemorySegment h = h();
            check(call(() -> (int) BIND_TIMESTAMPTZ.invokeExact(h, i, seconds)));
        }

        synchronized void clearBindings() throws SQLException {
            MemorySegment h = h();
            check(call(() -> (int) CLEAR_BINDINGS.invokeExact(h)));
        }

        synchronized Result run() throws SQLException {
            try (Arena a = Arena.ofConfined()) {
                MemorySegment out = a.allocate(ADDRESS);
                MemorySegment h = h();
                int code = call(() -> (int) STMT_RUN.invokeExact(h, out));
                check(code);
                return new Result(out.get(ADDRESS, 0));
            }
        }
    }

    // ----- results -----------------------------------------------------------------------------

    /** An {@code acc_result *}, copied out of the engine; read by position. */
    static final class Result implements AutoCloseable {
        private MemorySegment handle;
        private final Arena arena = Arena.ofShared();
        private final MemorySegment lenSlot = arena.allocate(JAVA_LONG);
        final long rows;
        final long columns;
        final long affected;
        final String[] names;

        private Result(MemorySegment handle) {
            this.handle = handle;
            MemorySegment h = handle;
            rows = call(() -> (long) ROW_COUNT.invokeExact(h));
            columns = call(() -> (long) COLUMN_COUNT.invokeExact(h));
            affected = call(() -> (long) AFFECTED.invokeExact(h));
            names = new String[(int) columns];
            for (int c = 0; c < names.length; c++) {
                long col = c;
                names[c] = cString(call(() -> (MemorySegment) COLUMN_NAME.invokeExact(h, col)));
            }
        }

        private MemorySegment h() {
            if (handle == null) {
                throw new IllegalStateException("the result is closed");
            }
            return handle;
        }

        int type(long row, long col) {
            MemorySegment h = h();
            return call(() -> (int) VALUE_TYPE.invokeExact(h, row, col));
        }

        long int64(long row, long col) {
            MemorySegment h = h();
            return call(() -> (long) VALUE_INT64.invokeExact(h, row, col));
        }

        double dbl(long row, long col) {
            MemorySegment h = h();
            return call(() -> (double) VALUE_DOUBLE.invokeExact(h, row, col));
        }

        /** The text of a value, or null if it has none. */
        String text(long row, long col) {
            MemorySegment h = h();
            MemorySegment p = call(() -> (MemorySegment) VALUE_TEXT.invokeExact(h, row, col, lenSlot));
            if (p.equals(MemorySegment.NULL)) {
                return null;
            }
            long n = lenSlot.get(JAVA_LONG, 0);
            return new String(p.reinterpret(n).toArray(JAVA_BYTE), StandardCharsets.UTF_8);
        }

        @Override
        public synchronized void close() {
            if (handle != null) {
                MemorySegment h = handle;
                handle = null;
                call(() -> {
                    RESULT_FREE.invokeExact(h);
                    return null;
                });
                arena.close();
            }
        }
    }

    // ----- errors ------------------------------------------------------------------------------

    static SQLException error(int code, String message) {
        String state = switch (code) {
            case ERR_SYNTAX -> "42000";
            case ERR_CATALOG -> message != null && message.contains("already exists") ? "42S01" : "42S02";
            case ERR_CONSTRAINT -> "23000";
            case ERR_TYPE -> "22018";
            case ERR_RUNTIME -> "HY000";
            case ERR_IO -> "58030";
            case ERR_CORRUPT -> "XX001";
            case ERR_LOCKED -> "08001";
            case ERR_MISUSE -> "HY010";
            case ERR_ENCODING -> "22021";
            default -> "XX000";
        };
        return new SQLException(message == null || message.isEmpty() ? "error " + code : message, state, code);
    }
}
