package com.accessor.jdbc;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLClientInfoException;
import java.sql.SQLException;
import java.sql.SQLWarning;
import java.sql.Savepoint;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.Collections;
import java.util.IdentityHashMap;
import java.util.List;
import java.util.Map;
import java.util.Properties;
import java.util.Set;
import java.util.concurrent.Executor;

/**
 * A connection to an Accessor database.
 *
 * <p>Transactions: in auto-commit mode (the default) every statement is its own transaction. With
 * auto-commit off a transaction starts at the first statement and ends at {@link #commit()} or
 * {@link #rollback()}; schema changes (CREATE TABLE, ...) take part in it like row changes. There is a
 * single isolation level, serializable.
 */
public final class AccessorConnection extends UnsupportedConnection {
    private final String url;
    private final Native.Db db;
    private boolean autoCommit = true;
    private boolean readOnly;
    private boolean closed;
    private int holdability = ResultSet.CLOSE_CURSORS_AT_COMMIT;
    private int savepointCounter;
    private final Set<AccessorStatement> statements = Collections.newSetFromMap(new IdentityHashMap<>());

    AccessorConnection(String url, String path, Map<String, String> options) throws SQLException {
        this.url = url;
        this.db = Native.Db.open(path);
        try {
            String durability = options.get("durability");
            if (durability != null) {
                switch (durability.toLowerCase()) {
                    case "fsync" -> db.setDurability(Native.DURABILITY_FSYNC);
                    case "os" -> db.setDurability(Native.DURABILITY_OS_BUFFERED);
                    default -> throw new SQLException("durability must be fsync or os, not '" + durability + "'", "08001");
                }
            }
            String optimizer = options.get("optimizer");
            if (optimizer != null) {
                db.setOptimizer(!optimizer.equalsIgnoreCase("off") && !optimizer.equals("0") && !optimizer.equalsIgnoreCase("false"));
            }
            String zone = options.get("timezone");
            if (zone != null && !zone.isEmpty()) {
                db.run("SET TIME ZONE '" + zone.replace("'", "''") + "'");
            }
        } catch (SQLException | RuntimeException e) {
            db.close();
            throw e;
        }
    }

    String url() {
        return url;
    }

    Native.Db db() {
        return db;
    }

    void checkOpen() throws SQLException {
        if (closed) {
            throw new SQLException("the connection is closed", "08003");
        }
    }

    void register(AccessorStatement s) {
        synchronized (statements) {
            statements.add(s);
        }
    }

    void unregister(AccessorStatement s) {
        synchronized (statements) {
            statements.remove(s);
        }
    }

    /** Starts the transaction that a connection with auto-commit off is always in. */
    synchronized void beginIfNeeded() throws SQLException {
        if (!autoCommit && !db.inTransaction()) {
            db.run("BEGIN");
        }
    }

    // ----- Accessor extras (reach them with unwrap(AccessorConnection.class)) ------------------

    /** Renders the stored report {@code name} as {@code text}, {@code html}, {@code markdown}, {@code csv} or {@code pdf}. */
    public synchronized byte[] renderReport(String name, String format) throws SQLException {
        checkOpen();
        int f = switch (format.toLowerCase()) {
            case "text", "txt" -> Native.REPORT_TEXT;
            case "html" -> Native.REPORT_HTML;
            case "markdown", "md" -> Native.REPORT_MARKDOWN;
            case "csv" -> Native.REPORT_CSV;
            case "pdf" -> Native.REPORT_PDF;
            default -> throw new SQLException("'" + format + "' is not a report format (text, html, markdown, csv, pdf)", "HY000");
        };
        return db.renderReport(name, f);
    }

    /** Writes the log into the database file now. */
    public synchronized void checkpoint() throws SQLException {
        checkOpen();
        db.checkpoint();
    }

    // ----- statements --------------------------------------------------------------------------

    @Override
    public Statement createStatement() throws SQLException {
        return createStatement(ResultSet.TYPE_FORWARD_ONLY, ResultSet.CONCUR_READ_ONLY);
    }

    @Override
    public Statement createStatement(int type, int concurrency) throws SQLException {
        checkOpen();
        checkResultSetKind(type, concurrency);
        return new AccessorStatement(this, type);
    }

    @Override
    public Statement createStatement(int type, int concurrency, int holdability) throws SQLException {
        return createStatement(type, concurrency);
    }

    @Override
    public PreparedStatement prepareStatement(String sql) throws SQLException {
        return prepareStatement(sql, Statement.NO_GENERATED_KEYS);
    }

    @Override
    public PreparedStatement prepareStatement(String sql, int autoGeneratedKeys) throws SQLException {
        checkOpen();
        return new AccessorPreparedStatement(this, sql, ResultSet.TYPE_FORWARD_ONLY, autoGeneratedKeys == Statement.RETURN_GENERATED_KEYS);
    }

    @Override
    public PreparedStatement prepareStatement(String sql, int[] columnIndexes) throws SQLException {
        checkOpen();
        return new AccessorPreparedStatement(this, sql, ResultSet.TYPE_FORWARD_ONLY, columnIndexes != null && columnIndexes.length > 0);
    }

    @Override
    public PreparedStatement prepareStatement(String sql, String[] columnNames) throws SQLException {
        checkOpen();
        return new AccessorPreparedStatement(this, sql, ResultSet.TYPE_FORWARD_ONLY, columnNames != null && columnNames.length > 0);
    }

    @Override
    public PreparedStatement prepareStatement(String sql, int type, int concurrency) throws SQLException {
        checkOpen();
        checkResultSetKind(type, concurrency);
        return new AccessorPreparedStatement(this, sql, type, false);
    }

    @Override
    public PreparedStatement prepareStatement(String sql, int type, int concurrency, int holdability) throws SQLException {
        return prepareStatement(sql, type, concurrency);
    }

    private static void checkResultSetKind(int type, int concurrency) throws SQLException {
        if (concurrency != ResultSet.CONCUR_READ_ONLY) {
            throw Unsupported.feature("updatable result sets");
        }
        if (type != ResultSet.TYPE_FORWARD_ONLY && type != ResultSet.TYPE_SCROLL_INSENSITIVE && type != ResultSet.TYPE_SCROLL_SENSITIVE) {
            throw new SQLException("unknown result set type " + type, "HY000");
        }
    }

    @Override
    public String nativeSQL(String sql) {
        return sql;
    }

    // ----- transactions ------------------------------------------------------------------------

    @Override
    public synchronized void setAutoCommit(boolean autoCommit) throws SQLException {
        checkOpen();
        if (autoCommit && !this.autoCommit && db.inTransaction()) {
            db.run("COMMIT"); // JDBC: switching to auto-commit commits the open transaction
        }
        this.autoCommit = autoCommit;
    }

    @Override
    public boolean getAutoCommit() throws SQLException {
        checkOpen();
        return autoCommit;
    }

    @Override
    public synchronized void commit() throws SQLException {
        checkOpen();
        if (autoCommit) {
            throw new SQLException("cannot commit while auto-commit is on", "25000");
        }
        if (db.inTransaction()) {
            db.run("COMMIT");
        }
    }

    @Override
    public synchronized void rollback() throws SQLException {
        checkOpen();
        if (autoCommit) {
            throw new SQLException("cannot roll back while auto-commit is on", "25000");
        }
        if (db.inTransaction()) {
            db.run("ROLLBACK");
        }
    }

    @Override
    public synchronized Savepoint setSavepoint() throws SQLException {
        return setSavepoint(null);
    }

    @Override
    public synchronized Savepoint setSavepoint(String name) throws SQLException {
        checkOpen();
        if (autoCommit) {
            throw new SQLException("savepoints need a transaction: turn auto-commit off", "25000");
        }
        beginIfNeeded();
        AccessorSavepoint sp = new AccessorSavepoint(++savepointCounter, name);
        db.run("SAVEPOINT " + sp.sqlName());
        return sp;
    }

    @Override
    public synchronized void rollback(Savepoint savepoint) throws SQLException {
        checkOpen();
        db.run("ROLLBACK TO SAVEPOINT " + AccessorSavepoint.of(savepoint).sqlName());
    }

    @Override
    public synchronized void releaseSavepoint(Savepoint savepoint) throws SQLException {
        checkOpen();
        db.run("RELEASE SAVEPOINT " + AccessorSavepoint.of(savepoint).sqlName());
    }

    @Override
    public void setTransactionIsolation(int level) throws SQLException {
        checkOpen();
        if (level == Connection.TRANSACTION_NONE) {
            throw new SQLException("transactions cannot be turned off", "HY024");
        }
        // Serializable is the only level; asking for a lower one gets this one, which JDBC allows.
    }

    @Override
    public int getTransactionIsolation() throws SQLException {
        checkOpen();
        return Connection.TRANSACTION_SERIALIZABLE;
    }

    // ----- properties --------------------------------------------------------------------------

    @Override
    public DatabaseMetaData getMetaData() throws SQLException {
        checkOpen();
        return new AccessorDatabaseMetaData(this);
    }

    @Override
    public void setReadOnly(boolean readOnly) throws SQLException {
        checkOpen();
        this.readOnly = readOnly; // a hint: the engine does not enforce it
    }

    @Override
    public boolean isReadOnly() throws SQLException {
        checkOpen();
        return readOnly;
    }

    @Override
    public void setCatalog(String catalog) throws SQLException {
        checkOpen();
    }

    @Override
    public String getCatalog() throws SQLException {
        checkOpen();
        return null;
    }

    @Override
    public void setSchema(String schema) throws SQLException {
        checkOpen();
    }

    @Override
    public String getSchema() throws SQLException {
        checkOpen();
        return null;
    }

    @Override
    public void setHoldability(int holdability) throws SQLException {
        checkOpen();
        this.holdability = holdability;
    }

    @Override
    public int getHoldability() throws SQLException {
        checkOpen();
        return holdability;
    }

    @Override
    public SQLWarning getWarnings() throws SQLException {
        checkOpen();
        return null;
    }

    @Override
    public void clearWarnings() throws SQLException {
        checkOpen();
    }

    @Override
    public Map<String, Class<?>> getTypeMap() throws SQLException {
        checkOpen();
        return Collections.emptyMap();
    }

    @Override
    public void setClientInfo(String name, String value) throws SQLClientInfoException {}

    @Override
    public void setClientInfo(Properties properties) throws SQLClientInfoException {}

    @Override
    public String getClientInfo(String name) throws SQLException {
        checkOpen();
        return null;
    }

    @Override
    public Properties getClientInfo() throws SQLException {
        checkOpen();
        return new Properties();
    }

    @Override
    public void setNetworkTimeout(Executor executor, int milliseconds) throws SQLException {
        checkOpen();
    }

    @Override
    public int getNetworkTimeout() throws SQLException {
        checkOpen();
        return 0;
    }

    @Override
    public boolean isValid(int timeout) {
        if (closed) {
            return false;
        }
        try {
            db.run("SELECT 1");
            return true;
        } catch (SQLException | RuntimeException e) {
            return false;
        }
    }

    @Override
    public void abort(Executor executor) throws SQLException {
        close();
    }

    // ----- closing -----------------------------------------------------------------------------

    @Override
    public void close() throws SQLException {
        List<AccessorStatement> open;
        synchronized (statements) {
            open = new ArrayList<>(statements);
        }
        synchronized (this) {
            if (closed) {
                return;
            }
            closed = true;
        }
        for (AccessorStatement s : open) {
            s.closeQuietly();
        }
        // An open transaction is rolled back by closing the database.
        db.close();
    }

    @Override
    public boolean isClosed() {
        return closed;
    }
}
