package com.accessor.jdbc;

import java.sql.Connection;
import java.sql.Driver;
import java.sql.DriverManager;
import java.sql.DriverPropertyInfo;
import java.sql.SQLException;
import java.sql.SQLFeatureNotSupportedException;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Properties;
import java.util.logging.Logger;

/**
 * The JDBC driver for Accessor.
 *
 * <pre>
 * jdbc:accessor:/path/to/shop.adb               a database file (created if it does not exist)
 * jdbc:accessor:C:/data/shop.adb?durability=os  options after a question mark, separated by &amp;
 * jdbc:accessor:mem:                            a private database in memory
 * </pre>
 *
 * <p>Options (in the URL or in the connection properties): {@code durability} ({@code fsync}, the
 * default, or {@code os}: commits survive a crash of the program but not of the machine),
 * {@code optimizer} ({@code on}/{@code off}) and {@code timezone} (an IANA zone such as
 * {@code Europe/Paris}, for showing {@code TIMESTAMPTZ} values).
 *
 * <p>A database file can be open in one connection at a time, in this process or any other; a second
 * attempt fails with SQL state 08001.
 */
public final class AccessorDriver implements Driver {
    public static final String PREFIX = "jdbc:accessor:";

    static {
        try {
            DriverManager.registerDriver(new AccessorDriver());
        } catch (SQLException e) {
            throw new ExceptionInInitializerError(e);
        }
    }

    @Override
    public boolean acceptsURL(String url) {
        return url != null && url.regionMatches(true, 0, PREFIX, 0, PREFIX.length());
    }

    @Override
    public Connection connect(String url, Properties info) throws SQLException {
        if (!acceptsURL(url)) {
            return null;
        }
        String rest = url.substring(PREFIX.length());
        Map<String, String> options = new LinkedHashMap<>();
        int q = rest.indexOf('?');
        if (q >= 0) {
            for (String pair : rest.substring(q + 1).split("&")) {
                if (!pair.isEmpty()) {
                    int eq = pair.indexOf('=');
                    options.put((eq < 0 ? pair : pair.substring(0, eq)).toLowerCase(), eq < 0 ? "" : pair.substring(eq + 1));
                }
            }
            rest = rest.substring(0, q);
        }
        if (info != null) {
            for (String key : info.stringPropertyNames()) {
                options.putIfAbsent(key.toLowerCase(), info.getProperty(key));
            }
        }
        String path = rest.startsWith("file:") ? rest.substring(5) : rest;
        boolean memory = path.isEmpty() || path.equalsIgnoreCase("mem:") || path.equalsIgnoreCase("mem") || path.equals(":memory:");
        return new AccessorConnection(url, memory ? null : path, options);
    }

    @Override
    public DriverPropertyInfo[] getPropertyInfo(String url, Properties info) {
        DriverPropertyInfo durability = new DriverPropertyInfo("durability", "fsync");
        durability.choices = new String[] {"fsync", "os"};
        durability.description = "fsync: commits survive power loss; os: they survive a crash of the program only";
        DriverPropertyInfo optimizer = new DriverPropertyInfo("optimizer", "on");
        optimizer.choices = new String[] {"on", "off"};
        optimizer.description = "the query optimizer; answers are the same either way";
        DriverPropertyInfo zone = new DriverPropertyInfo("timezone", null);
        zone.description = "the time zone TIMESTAMPTZ values are shown in, such as Europe/Paris";
        return new DriverPropertyInfo[] {durability, optimizer, zone};
    }

    @Override
    public int getMajorVersion() {
        return Native.versionNumber() / 10_000;
    }

    @Override
    public int getMinorVersion() {
        return Native.versionNumber() / 100 % 100;
    }

    @Override
    public boolean jdbcCompliant() {
        return false; // not every required feature is there (callable statements, blobs, ...)
    }

    @Override
    public Logger getParentLogger() throws SQLFeatureNotSupportedException {
        throw Unsupported.feature("Driver.getParentLogger");
    }
}
