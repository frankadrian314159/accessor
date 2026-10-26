import java.math.BigDecimal;
import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.DriverManager;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Statement;
import java.sql.Timestamp;
import java.util.ArrayList;
import java.util.List;

/**
 * A plain-JDBC test of Accessor's driver: nothing but java.sql is used. Run with run.sh / run.ps1, which put
 * accessor-jdbc.jar (the driver, with the native library inside it) on the class path.
 */
public class AccessorJdbcTest {
    static int checks = 0, failures = 0;

    static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            failures++;
            System.out.println("FAIL  " + what);
        }
    }

    static void eq(Object expected, Object actual, String what) {
        check(java.util.Objects.equals(expected, actual), what + " (expected " + expected + ", got " + actual + ")");
    }

    interface Body {
        void run() throws Exception;
    }

    static void test(String name, Body body) {
        int before = failures;
        try {
            body.run();
        } catch (Throwable t) {
            failures++;
            checks++;
            System.out.println("FAIL  " + name + ": " + t);
            t.printStackTrace(System.out);
        }
        System.out.println((failures == before ? "ok    " : "FAIL  ") + name);
    }

    static SQLException failure(Body body) {
        try {
            body.run();
        } catch (SQLException e) {
            return e;
        } catch (Exception e) {
            throw new RuntimeException(e);
        }
        return null;
    }

    static long count(Connection c, String table) throws SQLException {
        try (Statement s = c.createStatement(); ResultSet r = s.executeQuery("SELECT COUNT(*) FROM " + table)) {
            r.next();
            return r.getLong(1);
        }
    }

    public static void main(String[] args) throws Exception {
        test("the driver accepts its URLs and no others", () -> {
            check(DriverManager.getDriver("jdbc:accessor:mem:") != null, "driver found by URL");
            check(failure(() -> DriverManager.getDriver("jdbc:postgresql://x/y")) != null, "other URLs are not ours");
            try (Connection c = DriverManager.getConnection("jdbc:accessor:mem:")) {
                check(!c.isClosed(), "open");
                eq("Accessor", c.getMetaData().getDatabaseProductName(), "product name");
            }
        });

        test("create, insert with generated keys, select", () -> {
            try (Connection c = DriverManager.getConnection("jdbc:accessor:mem:")) {
                try (Statement s = c.createStatement()) {
                    s.execute("CREATE TABLE people (id COUNTER PRIMARY KEY, name TEXT NOT NULL, age INTEGER)");
                }
                List<Long> keys = new ArrayList<>();
                try (PreparedStatement ps = c.prepareStatement("INSERT INTO people (name, age) VALUES (?, ?)", Statement.RETURN_GENERATED_KEYS)) {
                    for (Object[] row : new Object[][] {{"Ada", 36}, {"Grace", 45}, {"Edsger", null}}) {
                        ps.setString(1, (String) row[0]);
                        if (row[1] == null) ps.setNull(2, java.sql.Types.INTEGER); else ps.setInt(2, (Integer) row[1]);
                        eq(1, ps.executeUpdate(), "one row inserted");
                        try (ResultSet k = ps.getGeneratedKeys()) {
                            check(k.next(), "a generated key");
                            keys.add(k.getLong(1));
                        }
                    }
                }
                eq(List.of(1L, 2L, 3L), keys, "AutoNumber keys");
                try (PreparedStatement ps = c.prepareStatement("SELECT name, age FROM people WHERE id >= ? ORDER BY id")) {
                    ps.setInt(1, 2);
                    try (ResultSet r = ps.executeQuery()) {
                        check(r.next(), "row 2");
                        eq("Grace", r.getString("name"), "name");
                        eq(45, r.getInt("age"), "age");
                        check(r.next(), "row 3");
                        eq("Edsger", r.getString(1), "name by position");
                        eq(0, r.getInt("age"), "NULL reads as 0");
                        check(r.wasNull(), "wasNull");
                        check(!r.next(), "no row 4");
                    }
                }
            }
        });

        test("types come back exactly", () -> {
            try (Connection c = DriverManager.getConnection("jdbc:accessor:mem:"); Statement s = c.createStatement()) {
                s.execute("CREATE TABLE t (d DECIMAL, big BIGNUM, f DOUBLE, flag YESNO, at DATETIME, txt TEXT)");
                try (PreparedStatement ps = c.prepareStatement("INSERT INTO t VALUES (?, ?, ?, ?, ?, ?)")) {
                    ps.setBigDecimal(1, new BigDecimal("0.1"));
                    ps.setObject(2, new java.math.BigInteger("123456789012345678901234567890"));
                    ps.setDouble(3, 2.5);
                    ps.setBoolean(4, true);
                    ps.setTimestamp(5, Timestamp.valueOf("2024-03-01 12:30:45"));
                    ps.setString(6, "héllo 世界 😀");
                    ps.executeUpdate();
                }
                try (ResultSet r = s.executeQuery("SELECT d + 0.2 AS sum, big, f, flag, at, txt FROM t")) {
                    check(r.next(), "a row");
                    eq(new BigDecimal("0.3"), r.getBigDecimal("sum"), "0.1 + 0.2 is exactly 0.3");
                    eq(new java.math.BigInteger("123456789012345678901234567890"), r.getObject("big"), "bignum");
                    eq(2.5, r.getDouble("f"), "double");
                    eq(true, r.getBoolean("flag"), "boolean");
                    eq(Timestamp.valueOf("2024-03-01 12:30:45"), r.getTimestamp("at"), "datetime");
                    eq("héllo 世界 😀", r.getString("txt"), "unicode text");
                }
            }
        });

        test("transactions: commit, rollback, and DDL that rolls back", () -> {
            try (Connection c = DriverManager.getConnection("jdbc:accessor:mem:"); Statement s = c.createStatement()) {
                s.execute("CREATE TABLE acct (id INTEGER PRIMARY KEY, bal INTEGER)");
                s.execute("INSERT INTO acct VALUES (1, 100), (2, 100)");
                c.setAutoCommit(false);
                s.executeUpdate("UPDATE acct SET bal = bal - 30 WHERE id = 1");
                s.executeUpdate("UPDATE acct SET bal = bal + 30 WHERE id = 2");
                c.commit();
                s.executeUpdate("UPDATE acct SET bal = 0");
                s.execute("CREATE TABLE scratch (x INTEGER)");
                c.rollback();
                try (ResultSet r = s.executeQuery("SELECT bal FROM acct ORDER BY id")) {
                    r.next(); eq(70, r.getInt(1), "committed transfer, first");
                    r.next(); eq(130, r.getInt(1), "committed transfer, second");
                }
                check(failure(() -> s.executeQuery("SELECT * FROM scratch")) != null, "the table made in the rolled-back transaction is gone");
                c.setAutoCommit(true);
            }
        });

        test("a failed batch changes nothing", () -> {
            try (Connection c = DriverManager.getConnection("jdbc:accessor:mem:"); Statement s = c.createStatement()) {
                s.execute("CREATE TABLE u (id INTEGER PRIMARY KEY)");
                try (PreparedStatement ps = c.prepareStatement("INSERT INTO u VALUES (?)")) {
                    for (int id : new int[] {1, 2, 2}) {
                        ps.setInt(1, id);
                        ps.addBatch();
                    }
                    check(failure(ps::executeBatch) != null, "the duplicate fails the batch");
                }
                eq(0L, count(c, "u"), "none of the rows went in");
            }
        });

        test("errors carry SQL states", () -> {
            try (Connection c = DriverManager.getConnection("jdbc:accessor:mem:"); Statement s = c.createStatement()) {
                s.execute("CREATE TABLE e (id INTEGER PRIMARY KEY)");
                eq("42000", failure(() -> s.execute("SELEC 1")).getSQLState(), "syntax error");
                eq("42S02", failure(() -> s.executeQuery("SELECT * FROM nope")).getSQLState(), "no such table");
                // Preparing bad SQL fails with the SQL exception itself, and leaves the connection usable.
                eq("42000", failure(() -> c.prepareStatement("SELEC ?")).getSQLState(), "syntax error when preparing");
                s.execute("INSERT INTO e VALUES (1)");
                eq("23000", failure(() -> s.execute("INSERT INTO e VALUES (1)")).getSQLState(), "duplicate key");
            }
        });

        test("metadata lists tables and columns", () -> {
            try (Connection c = DriverManager.getConnection("jdbc:accessor:mem:"); Statement s = c.createStatement()) {
                s.execute("CREATE TABLE Orders (ID COUNTER PRIMARY KEY, Customer TEXT, Total DECIMAL)");
                DatabaseMetaData md = c.getMetaData();
                List<String> tables = new ArrayList<>();
                try (ResultSet r = md.getTables(null, null, "%", new String[] {"TABLE"})) {
                    while (r.next()) tables.add(r.getString("TABLE_NAME"));
                }
                check(tables.stream().anyMatch(t -> t.equalsIgnoreCase("Orders")), "Orders is listed: " + tables);
                List<String> cols = new ArrayList<>();
                try (ResultSet r = md.getColumns(null, null, "Orders", "%")) {
                    while (r.next()) cols.add(r.getString("COLUMN_NAME").toLowerCase());
                }
                eq(List.of("id", "customer", "total"), cols, "columns, in order");
                try (ResultSet r = md.getPrimaryKeys(null, null, "Orders")) {
                    check(r.next(), "a primary key");
                    eq("id", r.getString("COLUMN_NAME").toLowerCase(), "key column");
                }
            }
        });

        test("a database file persists, and is locked while open", () -> {
            Path dir = Files.createTempDirectory("accessor-jdbc-test");
            String url = "jdbc:accessor:" + dir.resolve("test.adb").toString().replace('\\', '/');
            try {
                try (Connection c = DriverManager.getConnection(url); Statement s = c.createStatement()) {
                    s.execute("CREATE TABLE kept (n INTEGER)");
                    s.execute("INSERT INTO kept VALUES (42)");
                    SQLException again = failure(() -> DriverManager.getConnection(url));
                    check(again != null, "a second connection to the file is refused");
                    if (again != null) eq("08001", again.getSQLState(), "state of the refusal");
                }
                try (Connection c = DriverManager.getConnection(url); Statement s = c.createStatement(); ResultSet r = s.executeQuery("SELECT n FROM kept")) {
                    check(r.next(), "the row is still there");
                    eq(42, r.getInt(1), "its value");
                }
            } finally {
                try (var files = Files.list(dir)) {
                    files.forEach(f -> f.toFile().delete());
                }
                dir.toFile().delete();
            }
        });

        System.out.println();
        System.out.println(checks + " checks, " + failures + " failed");
        System.exit(failures == 0 ? 0 : 1);
    }
}
