package com.accessor.jdbc;

import java.io.InputStream;
import java.io.Reader;
import java.math.BigDecimal;
import java.math.BigInteger;
import java.sql.Date;
import java.sql.ParameterMetaData;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.Time;
import java.sql.Timestamp;
import java.time.Instant;
import java.time.LocalDate;
import java.time.LocalDateTime;
import java.time.LocalTime;
import java.time.OffsetDateTime;
import java.time.ZoneOffset;
import java.time.ZonedDateTime;
import java.util.ArrayList;
import java.util.Calendar;
import java.util.List;
import java.util.UUID;

/**
 * A statement read once and run many times.
 *
 * <p>JDBC's {@code ?} placeholders become the engine's named parameters {@code @p1, @p2, ...}.
 * Values are kept as Java objects until the statement runs, so that a batch can hold many sets.
 */
final class AccessorPreparedStatement extends UnsupportedPreparedStatement {
    private static final Object NULL = new Object();

    private final String originalSql;
    private final Native.Stmt stmt;
    private final int count;
    private final boolean keys;
    private final boolean returnsKeys;
    private Object[] values;
    private final List<Object[]> batch = new ArrayList<>();

    AccessorPreparedStatement(AccessorConnection conn, String sql, int resultSetType, boolean generatedKeys) throws SQLException {
        super(conn, resultSetType);
        this.originalSql = sql;
        boolean ok = false;
        try {
            SqlTranslator.Result t = SqlTranslator.translate(sql);
            String text = t.sql();
            synchronized (conn) {
                String withKeys = generatedKeys ? withGeneratedKeys(text) : null;
                this.returnsKeys = withKeys != null;
                this.stmt = conn.db().prepare(withKeys != null ? withKeys : text);
            }
            this.keys = generatedKeys;
            this.count = t.parameters();
            this.values = new Object[count];
            ok = true;
        } finally {
            if (!ok) {
                closeQuietly();
            }
        }
    }

    @Override
    void releaseNative() {
        stmt.close();
    }

    // ----- parameters --------------------------------------------------------------------------

    private void set(int index, Object value) throws SQLException {
        checkOpen();
        if (index < 1 || index > count) {
            throw new SQLException("parameter index " + index + " is out of range (the statement has " + count + ")", "07009");
        }
        values[index - 1] = value == null ? NULL : value;
    }

    @Override
    public void clearParameters() throws SQLException {
        checkOpen();
        values = new Object[count];
    }

    @Override
    public void setNull(int i, int sqlType) throws SQLException {
        set(i, null);
    }

    @Override
    public void setNull(int i, int sqlType, String typeName) throws SQLException {
        set(i, null);
    }

    @Override
    public void setBoolean(int i, boolean v) throws SQLException {
        set(i, v);
    }

    @Override
    public void setByte(int i, byte v) throws SQLException {
        set(i, (long) v);
    }

    @Override
    public void setShort(int i, short v) throws SQLException {
        set(i, (long) v);
    }

    @Override
    public void setInt(int i, int v) throws SQLException {
        set(i, (long) v);
    }

    @Override
    public void setLong(int i, long v) throws SQLException {
        set(i, v);
    }

    @Override
    public void setFloat(int i, float v) throws SQLException {
        set(i, (double) v);
    }

    @Override
    public void setDouble(int i, double v) throws SQLException {
        set(i, v);
    }

    @Override
    public void setBigDecimal(int i, BigDecimal v) throws SQLException {
        set(i, v);
    }

    @Override
    public void setString(int i, String v) throws SQLException {
        set(i, v);
    }

    @Override
    public void setNString(int i, String v) throws SQLException {
        set(i, v);
    }

    @Override
    public void setDate(int i, Date v) throws SQLException {
        set(i, v);
    }

    @Override
    public void setDate(int i, Date v, Calendar cal) throws SQLException {
        set(i, v);
    }

    @Override
    public void setTime(int i, Time v) throws SQLException {
        set(i, v);
    }

    @Override
    public void setTime(int i, Time v, Calendar cal) throws SQLException {
        set(i, v);
    }

    @Override
    public void setTimestamp(int i, Timestamp v) throws SQLException {
        set(i, v);
    }

    @Override
    public void setTimestamp(int i, Timestamp v, Calendar cal) throws SQLException {
        set(i, v);
    }

    @Override
    public void setObject(int i, Object v) throws SQLException {
        set(i, v);
    }

    @Override
    public void setObject(int i, Object v, int targetSqlType) throws SQLException {
        set(i, v);
    }

    @Override
    public void setObject(int i, Object v, int targetSqlType, int scaleOrLength) throws SQLException {
        set(i, v);
    }

    @Override
    public void setCharacterStream(int i, Reader reader, int length) throws SQLException {
        try {
            set(i, reader == null ? null : readAll(reader, length));
        } catch (java.io.IOException e) {
            throw new SQLException("cannot read the stream: " + e.getMessage(), "HY000", e);
        }
    }

    @Override
    public void setCharacterStream(int i, Reader reader) throws SQLException {
        setCharacterStream(i, reader, Integer.MAX_VALUE);
    }

    @Override
    public void setCharacterStream(int i, Reader reader, long length) throws SQLException {
        setCharacterStream(i, reader, (int) Math.min(length, Integer.MAX_VALUE));
    }

    private static String readAll(Reader r, int max) throws java.io.IOException {
        StringBuilder sb = new StringBuilder();
        char[] buf = new char[8192];
        int n;
        while (sb.length() < max && (n = r.read(buf, 0, Math.min(buf.length, max - sb.length()))) > 0) {
            sb.append(buf, 0, n);
        }
        return sb.toString();
    }

    @Override
    public void setAsciiStream(int i, InputStream in, int length) throws SQLException {
        throw Unsupported.feature("PreparedStatement.setAsciiStream");
    }

    // ----- running ----------------------------------------------------------------------------

    private void bindAll(Object[] vals) throws SQLException {
        for (int i = 0; i < count; i++) {
            Object v = vals[i];
            if (v == null) {
                throw new SQLException("parameter " + (i + 1) + " has no value; call a set method for it (setNull for NULL)", "07001");
            }
            int at = stmt.indexOf("p" + (i + 1));
            if (at < 0) {
                throw new SQLException("parameter " + (i + 1) + " is not used by the statement", "07009");
            }
            bind(at, v);
        }
    }

    private void bind(int at, Object v) throws SQLException {
        if (v == NULL) {
            stmt.bindNull(at);
        } else if (v instanceof Boolean b) {
            stmt.bindBool(at, b);
        } else if (v instanceof Long || v instanceof Integer || v instanceof Short || v instanceof Byte) {
            stmt.bindLong(at, ((Number) v).longValue());
        } else if (v instanceof Double || v instanceof Float) {
            stmt.bindDouble(at, ((Number) v).doubleValue());
        } else if (v instanceof BigDecimal d) {
            stmt.bindDecimal(at, d.toPlainString());
        } else if (v instanceof BigInteger n) {
            stmt.bindBignum(at, n.toString());
        } else if (v instanceof String || v instanceof Character || v instanceof UUID || v instanceof Enum<?>) {
            stmt.bindText(at, v instanceof Enum<?> e ? e.name() : v.toString());
        } else if (v instanceof Timestamp t) {
            stmt.bindDatetime(at, t.toLocalDateTime().toEpochSecond(ZoneOffset.UTC));
        } else if (v instanceof Date d) {
            stmt.bindDatetime(at, d.toLocalDate().atStartOfDay().toEpochSecond(ZoneOffset.UTC));
        } else if (v instanceof Time t) {
            stmt.bindDatetime(at, t.toLocalTime().atDate(LocalDate.EPOCH).toEpochSecond(ZoneOffset.UTC));
        } else if (v instanceof LocalDateTime t) {
            stmt.bindDatetime(at, t.toEpochSecond(ZoneOffset.UTC));
        } else if (v instanceof LocalDate d) {
            stmt.bindDatetime(at, d.atStartOfDay().toEpochSecond(ZoneOffset.UTC));
        } else if (v instanceof LocalTime t) {
            stmt.bindDatetime(at, t.atDate(LocalDate.EPOCH).toEpochSecond(ZoneOffset.UTC));
        } else if (v instanceof OffsetDateTime t) {
            stmt.bindTimestamptz(at, t.toEpochSecond());
        } else if (v instanceof ZonedDateTime t) {
            stmt.bindTimestamptz(at, t.toEpochSecond());
        } else if (v instanceof Instant t) {
            stmt.bindTimestamptz(at, t.getEpochSecond());
        } else if (v instanceof java.util.Date d) {
            stmt.bindTimestamptz(at, Math.floorDiv(d.getTime(), 1000L));
        } else {
            throw Unsupported.feature("a parameter of type " + v.getClass().getName());
        }
    }

    private boolean runBound(Object[] vals) throws SQLException {
        checkOpen();
        clearResults();
        synchronized (conn) {
            conn.beginIfNeeded();
            bindAll(vals);
            Native.Result r = stmt.run();
            if (returnsKeys) {
                // The statement made rows of generated values (RETURNING ...): they are the keys.
                setUpdateCount(r.affected);
                setGeneratedKeys(new AccessorResultSet(this, r, ResultSet.TYPE_FORWARD_ONLY, 0));
                return false;
            }
            boolean rows = take(r);
            if (keys) {
                setGeneratedKeys(emptyKeys());
            }
            return rows;
        }
    }

    @Override
    public boolean execute() throws SQLException {
        return runBound(values.clone());
    }

    @Override
    public ResultSet executeQuery() throws SQLException {
        if (!execute()) {
            throw new SQLException("the statement does not return rows", "HY000");
        }
        return getResultSet();
    }

    @Override
    public int executeUpdate() throws SQLException {
        execute();
        return (int) Math.max(getLargeUpdateCount(), 0);
    }

    @Override
    public long executeLargeUpdate() throws SQLException {
        execute();
        return Math.max(getLargeUpdateCount(), 0);
    }

    // The Statement methods that take SQL text make no sense here.
    @Override
    public ResultSet executeQuery(String sql) throws SQLException {
        throw new SQLException("a PreparedStatement runs its own SQL; use executeQuery()", "HY010");
    }

    @Override
    public int executeUpdate(String sql) throws SQLException {
        throw new SQLException("a PreparedStatement runs its own SQL; use executeUpdate()", "HY010");
    }

    @Override
    public boolean execute(String sql) throws SQLException {
        throw new SQLException("a PreparedStatement runs its own SQL; use execute()", "HY010");
    }

    @Override
    public void addBatch(String sql) throws SQLException {
        throw new SQLException("a PreparedStatement batch holds sets of parameters; use addBatch()", "HY010");
    }

    // ----- batches ----------------------------------------------------------------------------

    @Override
    public void addBatch() throws SQLException {
        checkOpen();
        batch.add(values.clone());
    }

    @Override
    public void clearBatch() throws SQLException {
        checkOpen();
        batch.clear();
    }

    @Override
    public long[] executeLargeBatch() throws SQLException {
        checkOpen();
        List<Object[]> work = new ArrayList<>(batch);
        batch.clear();
        long[] counts = new long[work.size()];
        // All or nothing: a batch that fails part-way leaves nothing behind.
        synchronized (conn) {
            boolean own = conn.getAutoCommit();
            if (own) {
                conn.setAutoCommit(false);
            }
            int done = 0;
            try {
                for (Object[] set : work) {
                    runBound(set);
                    counts[done++] = Math.max(getLargeUpdateCount(), 0);
                }
                if (own) {
                    conn.commit();
                }
            } catch (SQLException e) {
                if (own) {
                    conn.rollback();
                }
                throw new java.sql.BatchUpdateException(e.getMessage(), e.getSQLState(), e.getErrorCode(), new long[0], e);
            } finally {
                if (own) {
                    conn.setAutoCommit(true);
                }
            }
        }
        return counts;
    }

    // ----- metadata ---------------------------------------------------------------------------

    @Override
    public ParameterMetaData getParameterMetaData() throws SQLException {
        checkOpen();
        return new AccessorParameterMetaData(count);
    }

    @Override
    public ResultSetMetaData getMetaData() throws SQLException {
        checkOpen();
        return null; // the columns are known only once the statement has run
    }

    @Override
    public String toString() {
        return "AccessorPreparedStatement[" + originalSql + "]";
    }
}
