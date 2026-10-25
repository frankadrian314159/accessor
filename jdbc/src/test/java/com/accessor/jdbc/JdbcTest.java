package com.accessor.jdbc;

import java.io.IOException;
import java.math.BigDecimal;
import java.math.BigInteger;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.BatchUpdateException;
import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.Date;
import java.sql.Driver;
import java.sql.DriverManager;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.SQLFeatureNotSupportedException;
import java.sql.Savepoint;
import java.sql.Statement;
import java.sql.Timestamp;
import java.sql.Types;
import java.time.Instant;
import java.time.LocalDate;
import java.time.LocalDateTime;
import java.time.OffsetDateTime;
import java.time.ZoneOffset;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Tests of the JDBC driver against the real native library. A plain program rather than a test
 * framework's, so that it needs nothing but the JDK: run it with
 * {@code java --enable-native-access=ALL-UNNAMED -Daccessor.library=<path to the library> ...}; it
 * prints one line per test and exits with status 1 if any failed.
 */
public final class JdbcTest {
    private static int passed;
    private static int failed;
    private static final List<String> failures = new ArrayList<>();

    interface Body {
        void run() throws Exception;
    }

    static void test(String name, Body body) {
        try {
            body.run();
            passed++;
            System.out.println("ok   " + name);
        } catch (Throwable t) {
            failed++;
            failures.add(name);
            System.out.println("FAIL " + name + ": " + t);
            StackTraceElement[] st = t.getStackTrace();
            for (int i = 0; i < Math.min(6, st.length); i++) {
                System.out.println("       at " + st[i]);
            }
        }
    }

    static void check(boolean condition, String what) {
        if (!condition) {
            throw new AssertionError(what);
        }
    }

    static void eq(Object expected, Object actual, String what) {
        if (!java.util.Objects.equals(expected, actual)) {
            throw new AssertionError(what + ": expected <" + expected + "> but was <" + actual + ">");
        }
    }

    static void fails(String state, Body body, String what) {
        try {
            body.run();
        } catch (SQLException e) {
            if (state != null && !state.equals(e.getSQLState())) {
                throw new AssertionError(what + ": expected SQL state " + state + " but got " + e.getSQLState() + " (" + e.getMessage() + ")");
            }
            return;
        } catch (Exception e) {
            throw new AssertionError(what + ": expected an SQLException, got " + e);
        }
        throw new AssertionError(what + ": expected an SQLException, but none was thrown");
    }

    static Connection mem() throws SQLException {
        return DriverManager.getConnection("jdbc:accessor:mem:");
    }

    static void exec(Connection c, String sql) throws SQLException {
        try (Statement s = c.createStatement()) {
            s.execute(sql);
        }
    }

    static String one(Connection c, String sql) throws SQLException {
        try (Statement s = c.createStatement(); ResultSet r = s.executeQuery(sql)) {
            check(r.next(), "no row for " + sql);
            return r.getString(1);
        }
    }

    public static void main(String[] args) throws Exception {
        Path dir = Files.createTempDirectory("accessor-jdbc-test");

        test("the driver is registered and recognises its URLs", () -> {
            Driver d = DriverManager.getDriver("jdbc:accessor:mem:");
            check(d instanceof AccessorDriver, "driver class");
            check(d.acceptsURL("jdbc:accessor:/some/file.adb"), "accepts a file URL");
            check(!d.acceptsURL("jdbc:postgresql://localhost/x"), "refuses other URLs");
            check(d.getMajorVersion() >= 0, "version");
            eq(null, d.connect("jdbc:other:x", null), "connect to a foreign URL");
        });

        test("the library reports its version, ABI and file format", () -> {
            check(Native.version().matches("\\d+\\.\\d+\\.\\d+"), "version text " + Native.version());
            eq(Native.EXPECTED_ABI, Native.abiVersion(), "ABI version");
            check(Native.fileFormatVersion() >= 5, "file format");
            try (Connection c = mem()) {
                DatabaseMetaData m = c.getMetaData();
                eq("Accessor", m.getDatabaseProductName(), "product");
                eq(Native.version(), m.getDatabaseProductVersion(), "product version");
                eq("jdbc:accessor:mem:", m.getURL(), "url");
            }
        });

        test("create, insert and query", () -> {
            try (Connection c = mem(); Statement s = c.createStatement()) {
                eq(false, s.execute("CREATE TABLE t (id COUNTER PRIMARY KEY, name TEXT, qty INTEGER)"), "create returns no rows");
                eq(3, s.executeUpdate("INSERT INTO t (name, qty) VALUES ('a', 1), ('b', 2), ('c', 3)"), "inserted");
                try (ResultSet r = s.executeQuery("SELECT id, name, qty FROM t ORDER BY id")) {
                    int n = 0;
                    while (r.next()) {
                        n++;
                        eq(n, r.getInt("id"), "id");
                        eq(String.valueOf((char) ('a' + n - 1)), r.getString(2), "name");
                        eq((long) n, r.getLong("QTY"), "qty (case-insensitive label)");
                    }
                    eq(3, n, "rows");
                }
                eq(2, s.executeUpdate("UPDATE t SET qty = qty + 10 WHERE id > 1"), "updated");
                eq("26", one(c, "SELECT Sum(qty) FROM t"), "sum");
            }
        });

        test("every column type comes back as the right Java type", () -> {
            try (Connection c = mem(); Statement s = c.createStatement()) {
                s.execute("CREATE TABLE ty (i INTEGER, d DOUBLE, s TEXT, b YESNO, dec DECIMAL, big BIGNUM, cur CURRENCY, rat RATIONAL,"
                        + " dt DATETIME, tz TIMESTAMPTZ, iv INTERVAL, js JSON)");
                s.execute("INSERT INTO ty VALUES (42, 2.5, 'héllo 世界', TRUE, 0.1, 123456789012345678901234567890, 19.99, Rational(1, 3),"
                        + " #2024-03-05 14:30:00#, TIMESTAMP WITH TIME ZONE '2024-03-05 14:30:00+00:00', INTERVAL '3 days', '{\"b\":[1,2],\"a\":null}')");
                try (ResultSet r = s.executeQuery("SELECT * FROM ty")) {
                    check(r.next(), "row");
                    eq(Long.valueOf(42), r.getObject(1), "INTEGER -> Long");
                    eq(Double.valueOf(2.5), r.getObject(2), "DOUBLE -> Double");
                    eq("héllo 世界", r.getObject(3), "TEXT -> String (unicode)");
                    eq(Boolean.TRUE, r.getObject(4), "YESNO -> Boolean");
                    eq(new BigDecimal("0.1"), r.getObject(5), "DECIMAL -> BigDecimal");
                    eq(new BigInteger("123456789012345678901234567890"), r.getObject(6), "BIGNUM -> BigInteger");
                    eq(new BigDecimal("19.99"), r.getObject(7), "CURRENCY -> BigDecimal");
                    eq("1/3", r.getObject(8), "RATIONAL -> String");
                    eq(Timestamp.valueOf("2024-03-05 14:30:00"), r.getObject(9), "DATETIME -> Timestamp");
                    eq(OffsetDateTime.of(2024, 3, 5, 14, 30, 0, 0, ZoneOffset.UTC), r.getObject(10), "TIMESTAMPTZ -> OffsetDateTime");
                    eq("{\"a\":null,\"b\":[1,2]}", r.getObject(12), "JSON -> canonical String");
                    check(r.getString(11) != null && r.getString(11).contains("3"), "INTERVAL text " + r.getString(11));
                    // and through the typed getters
                    eq(42, r.getInt(1), "getInt");
                    eq(42.0, r.getDouble(1), "getDouble of an INTEGER");
                    eq("2.5", r.getString(2), "getString of a DOUBLE");
                    eq(true, r.getBoolean(4), "getBoolean");
                    eq(new BigDecimal("0.1"), r.getBigDecimal(5), "getBigDecimal");
                    eq(LocalDate.of(2024, 3, 5), r.getObject(9, LocalDate.class), "as LocalDate");
                    eq(LocalDateTime.of(2024, 3, 5, 14, 30), r.getObject(9, LocalDateTime.class), "as LocalDateTime");
                    eq(Date.valueOf("2024-03-05"), r.getDate(9), "getDate");
                    eq(Instant.parse("2024-03-05T14:30:00Z"), r.getObject(10, Instant.class), "as Instant");
                    eq(new BigInteger("123456789012345678901234567890"), r.getObject(6, BigInteger.class), "as BigInteger");
                    ResultSetMetaData m = r.getMetaData();
                    eq(12, m.getColumnCount(), "column count");
                    eq(Types.BIGINT, m.getColumnType(1), "type of INTEGER");
                    eq("CURRENCY", m.getColumnTypeName(7), "type name");
                    eq("dec", m.getColumnLabel(5), "label");
                    eq(Types.TIMESTAMP_WITH_TIMEZONE, m.getColumnType(10), "type of TIMESTAMPTZ");
                    eq("java.math.BigInteger", m.getColumnClassName(6), "class of BIGNUM");
                }
            }
        });

        test("decimal arithmetic stays exact", () -> {
            try (Connection c = mem()) {
                eq(new BigDecimal("0.3"), exactSum(c), "0.1 + 0.2");
            }
        });

        test("NULL handling and wasNull", () -> {
            try (Connection c = mem(); Statement s = c.createStatement()) {
                s.execute("CREATE TABLE n (a INTEGER, b TEXT, c DECIMAL, d DATETIME, e YESNO)");
                s.execute("INSERT INTO n VALUES (NULL, NULL, NULL, NULL, NULL), (1, 'x', 1.5, #2024-01-01#, TRUE)");
                try (ResultSet r = s.executeQuery("SELECT * FROM n ORDER BY a")) {
                    check(r.next(), "first");
                    eq(0, r.getInt(1), "NULL int is 0");
                    check(r.wasNull(), "wasNull after int");
                    eq(null, r.getString(2), "NULL string");
                    check(r.wasNull(), "wasNull after string");
                    eq(null, r.getBigDecimal(3), "NULL decimal");
                    eq(null, r.getTimestamp(4), "NULL timestamp");
                    eq(false, r.getBoolean(5), "NULL boolean is false");
                    eq(null, r.getObject(1), "NULL object");
                    check(r.next(), "second");
                    eq(1, r.getInt(1), "int");
                    check(!r.wasNull(), "wasNull cleared");
                    check(!r.next(), "end");
                }
            }
        });

        test("the cursor: next, bounds, scrolling", () -> {
            try (Connection c = mem(); Statement setup = c.createStatement()) {
                setup.execute("CREATE TABLE r (n INTEGER); INSERT INTO r VALUES (1), (2), (3), (4)");
                try (Statement s = c.createStatement(ResultSet.TYPE_SCROLL_INSENSITIVE, ResultSet.CONCUR_READ_ONLY);
                        ResultSet r = s.executeQuery("SELECT n FROM r ORDER BY n")) {
                    check(r.isBeforeFirst(), "before first");
                    fails("24000", () -> r.getInt(1), "reading before the first row");
                    check(r.last(), "last");
                    eq(4, r.getInt(1), "last row");
                    eq(4, r.getRow(), "row number");
                    check(r.isLast(), "isLast");
                    check(r.absolute(2) && r.getInt(1) == 2, "absolute(2)");
                    check(r.absolute(-1) && r.getInt(1) == 4, "absolute(-1)");
                    check(r.relative(-3) && r.getInt(1) == 1, "relative(-3)");
                    check(r.previous() == false && r.isBeforeFirst(), "previous off the start");
                    r.afterLast();
                    check(r.isAfterLast() && !r.next(), "after last");
                    check(r.first() && r.isFirst(), "first");
                    fails("07009", () -> r.getInt(2), "a column that does not exist");
                    fails("42S22", () -> r.findColumn("nope"), "a label that does not exist");
                }
                try (Statement s = c.createStatement(); ResultSet r = s.executeQuery("SELECT n FROM r")) {
                    fails("24000", r::last, "scrolling a forward-only result set");
                }
                try (Statement s = c.createStatement()) {
                    s.setMaxRows(2);
                    try (ResultSet r = s.executeQuery("SELECT n FROM r")) {
                        int n = 0;
                        while (r.next()) {
                            n++;
                        }
                        eq(2, n, "setMaxRows");
                    }
                }
            }
        });

        test("prepared statements bind every Java type", () -> {
            try (Connection c = mem()) {
                exec(c, "CREATE TABLE p (i INTEGER, d DOUBLE, s TEXT, b YESNO, dec DECIMAL, big BIGNUM, dt DATETIME, tz TIMESTAMPTZ)");
                try (PreparedStatement ps = c.prepareStatement("INSERT INTO p VALUES (?, ?, ?, ?, ?, ?, ?, ?)")) {
                    eq(8, ps.getParameterMetaData().getParameterCount(), "parameter count");
                    ps.setLong(1, 9_000_000_000L);
                    ps.setDouble(2, 1.25);
                    ps.setString(3, "it's \"quoted\" ? and [bracketed] --");
                    ps.setBoolean(4, true);
                    ps.setBigDecimal(5, new BigDecimal("12345.678901234567890123"));
                    ps.setObject(6, new BigInteger("-98765432109876543210"));
                    ps.setTimestamp(7, Timestamp.valueOf("2024-03-05 14:30:00"));
                    ps.setObject(8, OffsetDateTime.of(2024, 3, 5, 14, 30, 0, 0, ZoneOffset.ofHours(2)));
                    eq(1, ps.executeUpdate(), "inserted");
                    // the second row: everything else
                    ps.setInt(1, 7);
                    ps.setFloat(2, 0.5f);
                    ps.setNString(3, "");
                    ps.setBoolean(4, false);
                    ps.setBigDecimal(5, new BigDecimal("-0.5"));
                    ps.setObject(6, null);
                    ps.setObject(7, LocalDate.of(2020, 2, 29));
                    ps.setObject(8, Instant.parse("2020-02-29T00:00:00Z"));
                    eq(1, ps.executeUpdate(), "inserted again");
                }
                try (Statement s = c.createStatement(); ResultSet r = s.executeQuery("SELECT * FROM p ORDER BY i DESC")) {
                    check(r.next(), "row 1");
                    eq(9_000_000_000L, r.getLong(1), "long");
                    eq(1.25, r.getDouble(2), "double");
                    eq("it's \"quoted\" ? and [bracketed] --", r.getString(3), "text with special characters");
                    eq(true, r.getBoolean(4), "boolean");
                    eq(new BigDecimal("12345.678901234567890123"), r.getBigDecimal(5), "exact decimal");
                    eq(new BigInteger("-98765432109876543210"), r.getObject(6, BigInteger.class), "bignum");
                    eq(Timestamp.valueOf("2024-03-05 14:30:00"), r.getTimestamp(7), "timestamp");
                    eq(Instant.parse("2024-03-05T12:30:00Z"), r.getObject(8, Instant.class), "an offset time is the same instant");
                    check(r.next(), "row 2");
                    eq(0.5, r.getDouble(2), "float");
                    eq("", r.getString(3), "empty string is not NULL");
                    check(!r.wasNull(), "not null");
                    eq(null, r.getObject(6), "NULL bignum");
                    eq(LocalDate.of(2020, 2, 29), r.getObject(7, LocalDate.class), "date");
                    eq(Instant.parse("2020-02-29T00:00:00Z"), r.getObject(8, Instant.class), "instant");
                }
            }
        });

        test("prepared statements: placeholders are found only in SQL, and unset parameters are caught", () -> {
            eq("SELECT '?', [a?b], #1/2#, 1 -- ?\n, @p1 /* ? */, @p2", SqlTranslator.translate("SELECT '?', [a?b], #1/2#, 1 -- ?\n, ? /* ? */, ?").sql(),
                    "translation");
            eq(2, SqlTranslator.translate("SELECT '?', ?, 'it''s ?', ?").parameters(), "count");
            try (Connection c = mem()) {
                exec(c, "CREATE TABLE q (a INTEGER, b TEXT); INSERT INTO q VALUES (1, 'one?'), (2, 'two')");
                try (PreparedStatement ps = c.prepareStatement("SELECT b FROM q WHERE a = ? OR b = 'one?'")) {
                    fails("07001", ps::executeQuery, "a parameter with no value");
                    ps.setInt(1, 2);
                    try (ResultSet r = ps.executeQuery()) {
                        int n = 0;
                        while (r.next()) {
                            n++;
                        }
                        eq(2, n, "both rows");
                    }
                    fails("07009", () -> ps.setInt(2, 1), "a parameter index that does not exist");
                    fails("07009", () -> ps.setInt(0, 1), "parameter 0");
                    ps.clearParameters();
                    fails("07001", ps::executeQuery, "cleared parameters");
                    fails("HY010", () -> ps.executeQuery("SELECT 1"), "SQL text given to a prepared statement");
                }
                // names written with @ work as well
                try (PreparedStatement ps = c.prepareStatement("SELECT b FROM q WHERE a = 1")) {
                    check(ps.executeQuery().next(), "no parameters at all");
                }
            }
        });

        test("batches are all or nothing", () -> {
            try (Connection c = mem()) {
                exec(c, "CREATE TABLE b (k INTEGER PRIMARY KEY, v TEXT)");
                try (PreparedStatement ps = c.prepareStatement("INSERT INTO b VALUES (?, ?)")) {
                    for (int i = 1; i <= 5; i++) {
                        ps.setInt(1, i);
                        ps.setString(2, "v" + i);
                        ps.addBatch();
                    }
                    int[] counts = ps.executeBatch();
                    eq(5, counts.length, "batch size");
                    eq(1, counts[4], "count");
                    eq("5", one(c, "SELECT Count(*) FROM b"), "rows");
                    ps.setInt(1, 6);
                    ps.setString(2, "ok");
                    ps.addBatch();
                    ps.setInt(1, 3); // duplicate key
                    ps.setString(2, "dup");
                    ps.addBatch();
                    fails(null, ps::executeBatch, "a batch with a duplicate key");
                    eq("5", one(c, "SELECT Count(*) FROM b"), "the failed batch left nothing");
                }
                try (Statement s = c.createStatement()) {
                    s.addBatch("INSERT INTO b VALUES (10, 'x')");
                    s.addBatch("INSERT INTO b VALUES (11, 'y')");
                    eq(2, s.executeBatch().length, "statement batch");
                    s.addBatch("INSERT INTO b VALUES (12, 'z')");
                    s.addBatch("INSERT INTO nosuch VALUES (1)");
                    try {
                        s.executeBatch();
                        check(false, "the batch should fail");
                    } catch (BatchUpdateException expected) {
                        eq("42S02", expected.getSQLState(), "state of a failed batch");
                    }
                    eq("7", one(c, "SELECT Count(*) FROM b"), "rolled back");
                }
            }
        });

        test("generated keys: AutoNumbers, sequences and RETURNING", () -> {
            try (Connection c = mem()) {
                exec(c, "CREATE SEQUENCE ids START WITH 100; CREATE TABLE g (id COUNTER PRIMARY KEY, n TEXT);"
                        + " CREATE TABLE h (k INTEGER DEFAULT NEXT VALUE FOR ids PRIMARY KEY, n TEXT); CREATE TABLE plain (n TEXT)");
                try (PreparedStatement ps = c.prepareStatement("INSERT INTO g (n) VALUES (?)", Statement.RETURN_GENERATED_KEYS)) {
                    ps.setString(1, "a");
                    eq(1, ps.executeUpdate(), "insert");
                    try (ResultSet k = ps.getGeneratedKeys()) {
                        check(k.next(), "a key");
                        eq(1L, k.getLong(1), "first AutoNumber");
                        check(!k.next(), "one key");
                    }
                    ps.setString(1, "b");
                    ps.executeUpdate();
                    try (ResultSet k = ps.getGeneratedKeys()) {
                        check(k.next(), "second key");
                        eq(2L, k.getLong(1), "second AutoNumber");
                    }
                }
                try (Statement s = c.createStatement()) {
                    s.executeUpdate("INSERT INTO h (n) VALUES ('x'), ('y')", Statement.RETURN_GENERATED_KEYS);
                    try (ResultSet k = s.getGeneratedKeys()) {
                        check(k.next() && k.getLong(1) == 100, "first sequence key");
                        check(k.next() && k.getLong(1) == 101, "second sequence key");
                    }
                    s.executeUpdate("INSERT INTO plain VALUES ('z')", Statement.RETURN_GENERATED_KEYS);
                    try (ResultSet k = s.getGeneratedKeys()) {
                        check(!k.next(), "no keys for a table without one");
                    }
                    // RETURNING by hand is an ordinary query result
                    try (ResultSet r = s.executeQuery("INSERT INTO g (n) VALUES ('c') RETURNING id, n")) {
                        check(r.next() && r.getInt("id") == 3 && r.getString("n").equals("c"), "RETURNING");
                    }
                    check(s.execute("INSERT INTO g (n) VALUES ('d') RETURNING id"), "execute() reports the RETURNING rows");
                    eq(1, s.getResultSet().getMetaData().getColumnCount(), "returned column");
                }
            }
        });

        test("errors carry SQL states, vendor codes and messages", () -> {
            try (Connection c = mem(); Statement s = c.createStatement()) {
                s.execute("CREATE TABLE e (k INTEGER PRIMARY KEY, v TEXT NOT NULL)");
                s.execute("INSERT INTO e VALUES (1, 'a')");
                fails("42000", () -> s.execute("SELEKT 1"), "syntax");
                fails("42S02", () -> s.execute("SELECT * FROM nothing"), "missing table");
                fails("42S01", () -> s.execute("CREATE TABLE e (x INTEGER)"), "table exists");
                fails("23000", () -> s.execute("INSERT INTO e VALUES (1, 'b')"), "duplicate key");
                fails("23000", () -> s.execute("INSERT INTO e VALUES (2, NULL)"), "NOT NULL");
                fails("22018", () -> s.execute("INSERT INTO e VALUES ('x', 'b')"), "type");
                fails("HY000", () -> s.execute("SELECT 1 / 0"), "runtime");
                try {
                    s.execute("SELEKT 1");
                } catch (SQLException e) {
                    eq(Native.ERR_SYNTAX, e.getErrorCode(), "vendor code");
                    check(e.getMessage().contains("Syntax"), "message: " + e.getMessage());
                }
                eq("1", one(c, "SELECT Count(*) FROM e"), "the connection still works after errors");
            }
        });

        test("transactions: commit, rollback, auto-commit and DDL", () -> {
            try (Connection c = mem(); Statement s = c.createStatement()) {
                s.execute("CREATE TABLE tx (n INTEGER)");
                check(c.getAutoCommit(), "auto-commit is on by default");
                fails("25000", c::commit, "commit in auto-commit mode");
                c.setAutoCommit(false);
                s.execute("INSERT INTO tx VALUES (1)");
                s.execute("CREATE TABLE tx2 (x INTEGER)");
                c.rollback();
                eq("0", one(c, "SELECT Count(*) FROM tx"), "rolled back rows");
                fails("42S02", () -> s.execute("SELECT * FROM tx2"), "a table created in the rolled-back transaction");
                s.execute("INSERT INTO tx VALUES (2)");
                c.commit();
                eq("1", one(c, "SELECT Count(*) FROM tx"), "committed");
                s.execute("INSERT INTO tx VALUES (3)");
                c.setAutoCommit(true); // commits
                c.setAutoCommit(false);
                c.rollback();
                eq("2", one(c, "SELECT Count(*) FROM tx"), "switching to auto-commit committed");
                // user-written transaction statements work too
                c.setAutoCommit(true);
                s.execute("BEGIN");
                s.execute("INSERT INTO tx VALUES (4)");
                s.execute("ROLLBACK");
                eq("2", one(c, "SELECT Count(*) FROM tx"), "BEGIN/ROLLBACK as SQL");
                eq(Connection.TRANSACTION_SERIALIZABLE, c.getTransactionIsolation(), "isolation");
                c.setTransactionIsolation(Connection.TRANSACTION_READ_COMMITTED); // upgraded
                eq(Connection.TRANSACTION_SERIALIZABLE, c.getTransactionIsolation(), "still serializable");
            }
        });

        test("savepoints", () -> {
            try (Connection c = mem(); Statement s = c.createStatement()) {
                s.execute("CREATE TABLE sp (n INTEGER)");
                fails("25000", c::setSavepoint, "savepoint in auto-commit mode");
                c.setAutoCommit(false);
                s.execute("INSERT INTO sp VALUES (1)");
                Savepoint one = c.setSavepoint();
                s.execute("INSERT INTO sp VALUES (2)");
                Savepoint two = c.setSavepoint("second point");
                s.execute("INSERT INTO sp VALUES (3)");
                c.rollback(two);
                eq("2", one(c, "SELECT Count(*) FROM sp"), "back to the second savepoint");
                c.rollback(one);
                eq("1", one(c, "SELECT Count(*) FROM sp"), "back to the first");
                c.releaseSavepoint(one);
                c.commit();
                eq("second point", two.getSavepointName(), "name");
                fails(null, two::getSavepointId, "id of a named savepoint");
            }
        });

        test("a closed connection and a closed statement say so", () -> {
            Connection c = mem();
            Statement s = c.createStatement();
            ResultSet r = s.executeQuery("SELECT 1");
            PreparedStatement ps = c.prepareStatement("SELECT 1");
            check(!c.isClosed() && c.isValid(1), "valid");
            c.close();
            check(c.isClosed() && !c.isValid(1), "closed");
            check(s.isClosed() && r.isClosed() && ps.isClosed(), "closing the connection closes its statements and results");
            fails("08003", () -> c.createStatement(), "using a closed connection");
            fails("HY010", () -> s.executeQuery("SELECT 1"), "using a closed statement");
            fails("HY010", r::next, "using a closed result set");
            c.close(); // closing twice is allowed
            Connection d = mem();
            Statement t = d.createStatement();
            ResultSet u = t.executeQuery("SELECT 1");
            t.close();
            check(u.isClosed(), "closing a statement closes its result set");
            d.close();
        });

        test("DatabaseMetaData describes the catalog", () -> {
            try (Connection c = mem()) {
                exec(c, "CREATE TABLE Customers (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL, City TEXT DEFAULT 'Rome')");
                exec(c, "CREATE TABLE Orders (OID INTEGER PRIMARY KEY, CustID INTEGER REFERENCES Customers (ID) ON DELETE CASCADE, Total CURRENCY)");
                exec(c, "CREATE UNIQUE INDEX byName ON Customers (Name); CREATE VIEW Big AS SELECT OID FROM Orders WHERE Total > 100");
                exec(c, "CREATE FUNCTION dbl(@a INTEGER) RETURNS INTEGER AS @a * 2; CREATE PROCEDURE noop() AS BEGIN RETURN; END");
                DatabaseMetaData m = c.getMetaData();
                List<String> tables = new ArrayList<>();
                try (ResultSet r = m.getTables(null, null, "%", null)) {
                    while (r.next()) {
                        tables.add(r.getString("TABLE_NAME") + ":" + r.getString("TABLE_TYPE"));
                    }
                }
                eq(List.of("Customers:TABLE", "Orders:TABLE", "Big:VIEW").stream().sorted().toList(), tables.stream().sorted().toList(), "tables");
                try (ResultSet r = m.getTables(null, null, "Cust%", new String[] {"TABLE"})) {
                    check(r.next() && r.getString(3).equals("Customers") && !r.next(), "a name pattern");
                }
                try (ResultSet r = m.getColumns(null, null, "Customers", "%")) {
                    check(r.next(), "column 1");
                    eq("ID", r.getString("COLUMN_NAME"), "name");
                    eq(Types.BIGINT, r.getInt("DATA_TYPE"), "type code");
                    eq("YES", r.getString("IS_AUTOINCREMENT"), "AutoNumber");
                    eq(ResultSetMetaData.columnNoNulls, r.getInt("NULLABLE"), "not null");
                    check(r.next(), "column 2");
                    eq("Name", r.getString("COLUMN_NAME"), "name 2");
                    check(r.next(), "column 3");
                    eq("Rome", r.getString("COLUMN_DEF"), "default");
                    eq(3, r.getInt("ORDINAL_POSITION"), "position");
                }
                try (ResultSet r = m.getPrimaryKeys(null, null, "Orders")) {
                    check(r.next() && r.getString("COLUMN_NAME").equals("OID") && r.getInt("KEY_SEQ") == 1, "primary key");
                }
                try (ResultSet r = m.getImportedKeys(null, null, "Orders")) {
                    check(r.next(), "a foreign key");
                    eq("Customers", r.getString("PKTABLE_NAME"), "referenced table");
                    eq("ID", r.getString("PKCOLUMN_NAME"), "referenced column");
                    eq("CustID", r.getString("FKCOLUMN_NAME"), "referencing column");
                    eq(DatabaseMetaData.importedKeyCascade, r.getInt("DELETE_RULE"), "delete rule");
                    eq(DatabaseMetaData.importedKeyRestrict, r.getInt("UPDATE_RULE"), "update rule");
                }
                try (ResultSet r = m.getExportedKeys(null, null, "Customers")) {
                    check(r.next() && r.getString("FKTABLE_NAME").equals("Orders"), "exported keys");
                }
                try (ResultSet r = m.getIndexInfo(null, null, "Customers", true, false)) {
                    List<String> names = new ArrayList<>();
                    while (r.next()) {
                        names.add(r.getString("INDEX_NAME"));
                        check(!r.getBoolean("NON_UNIQUE"), "unique");
                    }
                    check(names.contains("byName") && names.contains("PrimaryKey"), "indexes " + names);
                }
                try (ResultSet r = m.getFunctions(null, null, "%")) {
                    check(r.next() && r.getString("FUNCTION_NAME").equals("dbl"), "functions");
                }
                try (ResultSet r = m.getProcedures(null, null, "%")) {
                    check(r.next() && r.getString("PROCEDURE_NAME").equals("noop"), "procedures");
                }
                try (ResultSet r = m.getTypeInfo()) {
                    int n = 0;
                    while (r.next()) {
                        n++;
                    }
                    check(n >= 12, "type info rows " + n);
                }
                try (ResultSet r = m.getTableTypes()) {
                    check(r.next() && r.next() && !r.next(), "table types");
                }
                try (ResultSet r = m.getSchemas()) {
                    check(!r.next(), "no schemas");
                }
                try (ResultSet r = m.getCatalogs()) {
                    check(!r.next(), "no catalogs");
                }
                check(m.supportsTransactions() && m.supportsSavepoints() && m.supportsBatchUpdates(), "capabilities");
                check(m.nullsAreSortedLow(), "NULLs sort low");
                eq(Connection.TRANSACTION_SERIALIZABLE, m.getDefaultTransactionIsolation(), "isolation");
            }
        });

        test("a database file: persistence, locking and URL options", () -> {
            Path file = dir.resolve("shop.adb");
            String url = "jdbc:accessor:" + file.toString().replace('\\', '/');
            try (Connection c = DriverManager.getConnection(url)) {
                exec(c, "CREATE TABLE s (n INTEGER, t TEXT); INSERT INTO s VALUES (1, 'one'), (2, 'two')");
                // a second connection to the same file is refused, with a message that says why
                try {
                    DriverManager.getConnection(url);
                    check(false, "the second connection must be refused");
                } catch (SQLException e) {
                    eq("08001", e.getSQLState(), "state of a locked database");
                    eq(Native.ERR_LOCKED, e.getErrorCode(), "vendor code");
                    check(e.getMessage().contains("in use"), e.getMessage());
                }
                exec(c, "INSERT INTO s VALUES (3, 'three')");
            }
            try (Connection c = DriverManager.getConnection(url + "?durability=os&optimizer=off&timezone=Europe/Paris")) {
                eq("3", one(c, "SELECT Count(*) FROM s"), "data survived closing");
                c.getMetaData();
                eq("Europe/Paris", one(c, "SELECT SessionTimeZone()"), "the timezone option");
                ((AccessorConnection) c).checkpoint();
            }
            fails("08001", () -> DriverManager.getConnection(url + "?durability=sometimes"), "a bad option");
            try (Connection c = DriverManager.getConnection(url)) {
                eq("3", one(c, "SELECT Count(*) FROM s"), "still there after the failed attempt released the lock");
            }
            fails("58030", () -> DriverManager.getConnection("jdbc:accessor:" + dir.toString().replace('\\', '/') + "/no/such/dir/x.adb"), "a path that cannot be a file");
            Path junk = dir.resolve("junk.adb");
            Files.writeString(junk, "this is not an Accessor database, only text that is long enough to look like a header");
            fails("XX001", () -> DriverManager.getConnection("jdbc:accessor:" + junk.toString().replace('\\', '/')), "a file that is not a database");
        });

        test("reports can be rendered through the driver", () -> {
            try (Connection c = mem()) {
                exec(c, "CREATE TABLE sales (region TEXT, amount CURRENCY); INSERT INTO sales VALUES ('N', 10.50), ('N', 4.25), ('S', 100)");
                exec(c, "CREATE REPORT by_region TITLE 'Sales' BREAK BY region COLUMN amount FORMAT '#,##0.00' SUM GRAND TOTAL AS SELECT region, amount FROM sales");
                AccessorConnection ac = c.unwrap(AccessorConnection.class);
                String text = new String(ac.renderReport("by_region", "text"), StandardCharsets.UTF_8);
                check(text.startsWith("Sales") && text.contains("114.75"), text);
                byte[] pdf = ac.renderReport("by_region", "pdf");
                check(new String(pdf, 0, 8, StandardCharsets.ISO_8859_1).equals("%PDF-1.4"), "a PDF");
                fails("42S02", () -> ac.renderReport("nosuch", "text"), "a report that does not exist");
                fails("HY000", () -> ac.renderReport("by_region", "docx"), "a format that does not exist");
                check(c.isWrapperFor(AccessorConnection.class) && !c.isWrapperFor(String.class), "isWrapperFor");
            }
        });

        test("features the driver does not have say so", () -> {
            try (Connection c = mem()) {
                try {
                    c.prepareCall("{call x()}");
                    check(false, "prepareCall");
                } catch (SQLFeatureNotSupportedException expected) {
                    eq("0A000", expected.getSQLState(), "state");
                }
                try {
                    c.createBlob();
                    check(false, "createBlob");
                } catch (SQLFeatureNotSupportedException expected) {
                    // as it should
                }
                try {
                    c.createStatement(ResultSet.TYPE_FORWARD_ONLY, ResultSet.CONCUR_UPDATABLE);
                    check(false, "updatable result sets");
                } catch (SQLFeatureNotSupportedException expected) {
                    // as it should
                }
                try (PreparedStatement ps = c.prepareStatement("SELECT ?")) {
                    try {
                        ps.setBytes(1, new byte[] {1});
                        check(false, "setBytes");
                    } catch (SQLFeatureNotSupportedException expected) {
                        // as it should
                    }
                    try {
                        ps.setObject(1, new Object());
                        ps.execute();
                        check(false, "a parameter of an unknown class");
                    } catch (SQLFeatureNotSupportedException expected) {
                        // as it should
                    }
                }
            }
        });

        test("text of any shape survives: NUL, emoji, a megabyte", () -> {
            try (Connection c = mem()) {
                exec(c, "CREATE TABLE tx (id INTEGER, t TEXT)");
                String big = "x".repeat(1 << 20);
                String[] texts = {"", "a\u0000b", "😀 emoji", "tab\tnewline\ncr\r", "'; DROP TABLE tx; --", "é世界", big};
                try (PreparedStatement ps = c.prepareStatement("INSERT INTO tx VALUES (?, ?)")) {
                    for (int i = 0; i < texts.length; i++) {
                        ps.setInt(1, i);
                        ps.setString(2, texts[i]);
                        ps.executeUpdate();
                    }
                }
                try (Statement s = c.createStatement(); ResultSet r = s.executeQuery("SELECT t FROM tx ORDER BY id")) {
                    for (String t : texts) {
                        check(r.next(), "row");
                        eq(t, r.getString(1), "text of length " + t.length());
                    }
                }
            }
        });

        test("many connections come and go without leaking threads", () -> {
            int before = Thread.getAllStackTraces().keySet().size();
            for (int i = 0; i < 100; i++) {
                try (Connection c = mem(); Statement s = c.createStatement()) {
                    s.execute("CREATE TABLE t (n INTEGER); INSERT INTO t VALUES (1)");
                    try (ResultSet r = s.executeQuery("SELECT n FROM t")) {
                        r.next();
                    }
                }
            }
            int after = Thread.getAllStackTraces().keySet().size();
            check(after <= before + 2, "threads before " + before + ", after " + after);
        });

        test("connections work from several threads at once", () -> {
            AtomicInteger errors = new AtomicInteger();
            List<Thread> threads = new ArrayList<>();
            for (int t = 0; t < 8; t++) {
                int id = t;
                Thread th = new Thread(() -> {
                    try (Connection c = mem(); Statement s = c.createStatement()) {
                        s.execute("CREATE TABLE t (n INTEGER)");
                        for (int i = 0; i < 200; i++) {
                            s.executeUpdate("INSERT INTO t VALUES (" + i + ")");
                        }
                        if (!one(c, "SELECT Sum(n) FROM t").equals("19900")) {
                            errors.incrementAndGet();
                        }
                    } catch (Throwable e) {
                        errors.incrementAndGet();
                        e.printStackTrace();
                    }
                }, "worker-" + id);
                threads.add(th);
                th.start();
            }
            for (Thread th : threads) {
                th.join();
            }
            eq(0, errors.get(), "errors in worker threads");
            // one connection shared by several threads: calls are carried out one at a time
            try (Connection c = mem()) {
                exec(c, "CREATE TABLE shared (w INTEGER, i INTEGER)");
                List<Thread> sharers = new ArrayList<>();
                for (int t = 0; t < 8; t++) {
                    int id = t;
                    Thread th = new Thread(() -> {
                        try (PreparedStatement ps = c.prepareStatement("INSERT INTO shared VALUES (?, ?)")) {
                            for (int i = 0; i < 100; i++) {
                                ps.setInt(1, id);
                                ps.setInt(2, i);
                                ps.executeUpdate();
                            }
                        } catch (Throwable e) {
                            errors.incrementAndGet();
                            e.printStackTrace();
                        }
                    });
                    sharers.add(th);
                    th.start();
                }
                for (Thread th : sharers) {
                    th.join();
                }
                eq(0, errors.get(), "errors in sharing threads");
                eq("800", one(c, "SELECT Count(*) FROM shared"), "rows inserted by sharing threads");
            }
        });

        test("a larger result is read correctly", () -> {
            try (Connection c = mem(); Statement s = c.createStatement();
                    ResultSet r = s.executeQuery("SELECT generate_series AS n, 'row ' & generate_series AS s FROM GENERATE_SERIES(1, 50000)")) {
                long sum = 0;
                int n = 0;
                while (r.next()) {
                    sum += r.getLong(1);
                    n++;
                    if (n == 50000) {
                        eq("row 50000", r.getString(2), "last text");
                    }
                }
                eq(50000, n, "rows");
                eq(50000L * 50001 / 2, sum, "sum");
            }
        });

        System.out.println();
        System.out.println(passed + " passed, " + failed + " failed");
        if (failed > 0) {
            System.out.println("failed: " + failures);
        }
        try {
            Files.walk(dir).sorted(java.util.Comparator.reverseOrder()).forEach(p -> p.toFile().delete());
        } catch (IOException ignored) {
            // the temporary files are removed by the operating system in time
        }
        System.exit(failed == 0 ? 0 : 1);
    }

    /** 0.1 + 0.2 as a DECIMAL: exactly 0.3 (as doubles it would be 0.30000000000000004). */
    private static BigDecimal exactSum(Connection c) throws SQLException {
        try (Statement s = c.createStatement(); ResultSet r = s.executeQuery("SELECT CAST(0.1 AS DECIMAL) + CAST(0.2 AS DECIMAL)")) {
            r.next();
            return r.getBigDecimal(1);
        }
    }
}
