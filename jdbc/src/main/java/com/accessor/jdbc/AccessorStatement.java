package com.accessor.jdbc;

import java.sql.Connection;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.SQLWarning;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/** A statement run with SQL text. {@link AccessorPreparedStatement} adds parameters. */
class AccessorStatement extends UnsupportedStatement {
    private static final Pattern INSERT_TARGET =
            Pattern.compile("^\\s*(?:insert\\s+(?:or\\s+\\w+\\s+)?into|replace\\s+into)\\s+(\\[[^\\]]+\\]|\\w+)", Pattern.CASE_INSENSITIVE);
    private static final Pattern TRANSACTION_WORD =
            Pattern.compile("^\\s*(begin|commit|rollback|savepoint|release|end)\\b", Pattern.CASE_INSENSITIVE);

    final AccessorConnection conn;
    private final int resultSetType;
    private boolean closed;
    private boolean closeOnCompletion;
    private int maxRows;
    private int fetchSize;
    private int queryTimeout;
    private AccessorResultSet current;
    private AccessorResultSet generatedKeys;
    private long updateCount = -1;
    private final List<String> batch = new ArrayList<>();

    AccessorStatement(AccessorConnection conn, int resultSetType) {
        this.conn = conn;
        this.resultSetType = resultSetType;
        conn.register(this);
    }

    final void checkOpen() throws SQLException {
        if (closed) {
            throw new SQLException("the statement is closed", "HY010");
        }
        conn.checkOpen();
    }

    // ----- running ----------------------------------------------------------------------------

    /** Forgets the previous results of this statement. */
    final void clearResults() {
        if (current != null) {
            current.closeQuietly();
            current = null;
        }
        if (generatedKeys != null) {
            generatedKeys.closeQuietly();
            generatedKeys = null;
        }
        updateCount = -1;
    }

    /** Takes the outcome of a native call as this statement's result. Returns true for a result set. */
    final boolean take(Native.Result r) {
        if (r.columns > 0) {
            current = new AccessorResultSet(this, r, resultSetType, maxRows);
            updateCount = -1;
            return true;
        }
        updateCount = r.affected;
        r.close();
        return false;
    }

    /** Runs SQL text. With {@code keys}, an insert reports the AutoNumber (or sequence) value it made. */
    final boolean runText(String sql, boolean keys) throws SQLException {
        checkOpen();
        clearResults();
        synchronized (conn) {
            if (!TRANSACTION_WORD.matcher(sql).find()) {
                conn.beginIfNeeded();
            }
            if (keys) {
                String withKey = withGeneratedKeys(sql);
                if (withKey != null) {
                    Native.Result r = conn.db().exec(withKey);
                    updateCount = r.affected;
                    generatedKeys = new AccessorResultSet(this, r, ResultSet.TYPE_FORWARD_ONLY, 0);
                    return false;
                }
            }
            boolean rows = take(conn.db().exec(sql));
            if (keys) {
                generatedKeys = emptyKeys();
            }
            return rows;
        }
    }

    final void setUpdateCount(long n) {
        updateCount = n;
    }

    final void setGeneratedKeys(AccessorResultSet rs) {
        generatedKeys = rs;
    }

    final AccessorResultSet emptyKeys() throws SQLException {
        return new AccessorResultSet(this, conn.db().exec("SELECT 1 AS GENERATED_KEY WHERE 1 = 0"), ResultSet.TYPE_FORWARD_ONLY, 0);
    }

    /** The statement with {@code RETURNING key} added, if it is an insert into a table with an AutoNumber. */
    final String withGeneratedKeys(String sql) throws SQLException {
        Matcher m = INSERT_TARGET.matcher(sql);
        if (!m.find() || sql.toLowerCase(Locale.ROOT).contains("returning")) {
            return null;
        }
        String table = m.group(1);
        table = table.startsWith("[") ? table.substring(1, table.length() - 1) : table;
        try (Native.Result r = conn.db().exec(
                "SELECT column_name FROM information_schema.columns WHERE table_name = '" + table.replace("'", "''")
                        + "' AND (is_autonumber OR column_default LIKE 'NEXT VALUE FOR*') ORDER BY ordinal_position")) {
            if (r.rows == 0) {
                return null;
            }
            String column = r.text(0, 0);
            String trimmed = sql.stripTrailing();
            if (trimmed.endsWith(";")) {
                trimmed = trimmed.substring(0, trimmed.length() - 1);
            }
            return trimmed + " RETURNING [" + column + "]";
        }
    }

    @Override
    public ResultSet executeQuery(String sql) throws SQLException {
        if (!runText(sql, false)) {
            throw new SQLException("the statement does not return rows", "HY000");
        }
        return current;
    }

    @Override
    public int executeUpdate(String sql) throws SQLException {
        runText(sql, false);
        return (int) Math.max(updateCount, 0);
    }

    @Override
    public int executeUpdate(String sql, int autoGeneratedKeys) throws SQLException {
        runText(sql, autoGeneratedKeys == Statement.RETURN_GENERATED_KEYS);
        return (int) Math.max(updateCount, 0);
    }

    @Override
    public int executeUpdate(String sql, int[] columnIndexes) throws SQLException {
        return executeUpdate(sql, Statement.RETURN_GENERATED_KEYS);
    }

    @Override
    public int executeUpdate(String sql, String[] columnNames) throws SQLException {
        return executeUpdate(sql, Statement.RETURN_GENERATED_KEYS);
    }

    @Override
    public long executeLargeUpdate(String sql) throws SQLException {
        runText(sql, false);
        return Math.max(updateCount, 0);
    }

    @Override
    public boolean execute(String sql) throws SQLException {
        return runText(sql, false);
    }

    @Override
    public boolean execute(String sql, int autoGeneratedKeys) throws SQLException {
        return runText(sql, autoGeneratedKeys == Statement.RETURN_GENERATED_KEYS);
    }

    @Override
    public boolean execute(String sql, int[] columnIndexes) throws SQLException {
        return runText(sql, true);
    }

    @Override
    public boolean execute(String sql, String[] columnNames) throws SQLException {
        return runText(sql, true);
    }

    // ----- results ----------------------------------------------------------------------------

    @Override
    public ResultSet getResultSet() throws SQLException {
        checkOpen();
        return current;
    }

    @Override
    public int getUpdateCount() throws SQLException {
        checkOpen();
        return (int) updateCount;
    }

    @Override
    public long getLargeUpdateCount() throws SQLException {
        checkOpen();
        return updateCount;
    }

    @Override
    public boolean getMoreResults() throws SQLException {
        checkOpen();
        if (current != null) {
            current.closeQuietly();
            current = null;
        }
        updateCount = -1;
        return false;
    }

    @Override
    public boolean getMoreResults(int mode) throws SQLException {
        return getMoreResults();
    }

    @Override
    public ResultSet getGeneratedKeys() throws SQLException {
        checkOpen();
        if (generatedKeys == null) {
            generatedKeys = emptyKeys();
        }
        return generatedKeys;
    }

    // ----- batches ----------------------------------------------------------------------------

    @Override
    public void addBatch(String sql) throws SQLException {
        checkOpen();
        batch.add(sql);
    }

    @Override
    public void clearBatch() throws SQLException {
        checkOpen();
        batch.clear();
    }

    @Override
    public int[] executeBatch() throws SQLException {
        long[] counts = executeLargeBatch();
        int[] out = new int[counts.length];
        for (int i = 0; i < out.length; i++) {
            out[i] = (int) counts[i];
        }
        return out;
    }

    @Override
    public long[] executeLargeBatch() throws SQLException {
        checkOpen();
        List<String> work = new ArrayList<>(batch);
        batch.clear();
        long[] counts = new long[work.size()];
        // All or nothing: a batch that fails part-way leaves nothing behind.
        synchronized (conn) {
            boolean own = conn.getAutoCommit();
            if (own) {
                conn.setAutoCommit(false);
            }
            try {
                for (int i = 0; i < counts.length; i++) {
                    runText(work.get(i), false);
                    counts[i] = Math.max(updateCount, 0);
                }
                if (own) {
                    conn.commit();
                }
            } catch (SQLException e) {
                if (own) {
                    conn.rollback();
                }
                java.sql.BatchUpdateException b = new java.sql.BatchUpdateException(e.getMessage(), e.getSQLState(), e.getErrorCode(), new long[0], e);
                throw b;
            } finally {
                if (own) {
                    conn.setAutoCommit(true);
                }
            }
        }
        return counts;
    }

    // ----- settings ---------------------------------------------------------------------------

    @Override
    public void setMaxRows(int max) throws SQLException {
        checkOpen();
        if (max < 0) {
            throw new SQLException("max rows cannot be negative", "HY024");
        }
        maxRows = max;
    }

    @Override
    public int getMaxRows() throws SQLException {
        checkOpen();
        return maxRows;
    }

    @Override
    public void setLargeMaxRows(long max) throws SQLException {
        setMaxRows((int) Math.min(max, Integer.MAX_VALUE));
    }

    @Override
    public long getLargeMaxRows() throws SQLException {
        return getMaxRows();
    }

    @Override
    public void setFetchSize(int rows) throws SQLException {
        checkOpen();
        fetchSize = rows;
    }

    @Override
    public int getFetchSize() throws SQLException {
        checkOpen();
        return fetchSize;
    }

    @Override
    public void setFetchDirection(int direction) throws SQLException {
        checkOpen();
    }

    @Override
    public int getFetchDirection() throws SQLException {
        checkOpen();
        return ResultSet.FETCH_FORWARD;
    }

    @Override
    public void setQueryTimeout(int seconds) throws SQLException {
        checkOpen();
        queryTimeout = seconds; // queries run to completion; there is no cancelling
    }

    @Override
    public int getQueryTimeout() throws SQLException {
        checkOpen();
        return queryTimeout;
    }

    @Override
    public void setMaxFieldSize(int max) throws SQLException {
        checkOpen();
    }

    @Override
    public int getMaxFieldSize() throws SQLException {
        checkOpen();
        return 0;
    }

    @Override
    public void setEscapeProcessing(boolean enable) throws SQLException {
        checkOpen();
    }

    @Override
    public void cancel() throws SQLException {
        checkOpen();
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
    public Connection getConnection() throws SQLException {
        checkOpen();
        return conn;
    }

    @Override
    public int getResultSetType() throws SQLException {
        checkOpen();
        return resultSetType;
    }

    @Override
    public int getResultSetConcurrency() throws SQLException {
        checkOpen();
        return ResultSet.CONCUR_READ_ONLY;
    }

    @Override
    public int getResultSetHoldability() throws SQLException {
        checkOpen();
        return ResultSet.CLOSE_CURSORS_AT_COMMIT;
    }

    @Override
    public void setPoolable(boolean poolable) throws SQLException {
        checkOpen();
    }

    @Override
    public boolean isPoolable() throws SQLException {
        checkOpen();
        return false;
    }

    @Override
    public void closeOnCompletion() throws SQLException {
        checkOpen();
        closeOnCompletion = true;
    }

    @Override
    public boolean isCloseOnCompletion() throws SQLException {
        checkOpen();
        return closeOnCompletion;
    }

    /** Called by a result set when it is closed. */
    final void resultSetClosed(AccessorResultSet rs) {
        if (rs == current) {
            current = null;
        }
        if (closeOnCompletion && !closed) {
            closeQuietly();
        }
    }

    // ----- closing ----------------------------------------------------------------------------

    void releaseNative() {}

    final void closeQuietly() {
        if (closed) {
            return;
        }
        closed = true;
        clearResults();
        releaseNative();
        conn.unregister(this);
    }

    @Override
    public final void close() throws SQLException {
        closeQuietly();
    }

    @Override
    public final boolean isClosed() {
        return closed;
    }
}
