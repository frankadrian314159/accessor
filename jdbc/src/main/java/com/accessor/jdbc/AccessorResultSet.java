package com.accessor.jdbc;

import java.math.BigDecimal;
import java.math.BigInteger;
import java.math.MathContext;
import java.sql.Date;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLDataException;
import java.sql.SQLException;
import java.sql.SQLWarning;
import java.sql.Statement;
import java.sql.Time;
import java.sql.Timestamp;
import java.time.Instant;
import java.time.LocalDate;
import java.time.LocalDateTime;
import java.time.LocalTime;
import java.time.OffsetDateTime;
import java.time.ZoneId;
import java.time.ZoneOffset;
import java.util.Calendar;
import java.util.Locale;

/**
 * The rows of a query. The rows were copied out of the engine when the query ran, so a result set
 * can be scrolled in any direction and is not affected by later changes to the database; it is
 * read-only. Close it (or its statement) to release the copy.
 */
final class AccessorResultSet extends UnsupportedResultSet {
    private final AccessorStatement owner;
    private final Native.Result data;
    private final int type;
    private final long rows;
    private long row = -1; // 0-based position; -1 before the first row, rows after the last
    private boolean wasNull;
    private boolean closed;
    private int[] columnTypes; // lazily: the type tag of the first non-NULL value of each column
    private int fetchSize;

    AccessorResultSet(AccessorStatement owner, Native.Result data, int type, int maxRows) {
        this.owner = owner;
        this.data = data;
        this.type = type;
        this.rows = maxRows > 0 ? Math.min(data.rows, maxRows) : data.rows;
    }

    // ----- state ------------------------------------------------------------------------------

    private void checkOpen() throws SQLException {
        if (closed) {
            throw new SQLException("the result set is closed", "HY010");
        }
    }

    private long cell(int column) throws SQLException {
        checkOpen();
        if (row < 0 || row >= rows) {
            throw new SQLException("the cursor is not on a row (call next() first)", "24000");
        }
        if (column < 1 || column > data.columns) {
            throw new SQLException("column " + column + " does not exist (the result has " + data.columns + ")", "07009");
        }
        return column - 1;
    }

    /** Type tag of the value, noting whether it was NULL. */
    private int tag(int column) throws SQLException {
        long c = cell(column);
        int t = data.type(row, c);
        wasNull = t == Native.T_NULL;
        return t;
    }

    int columnCount() {
        return (int) data.columns;
    }

    String columnName(int column) {
        return data.names[column - 1];
    }

    /** The tag of the first non-NULL value of the column (NULL if there is none). */
    int columnTag(int column) {
        if (columnTypes == null) {
            columnTypes = new int[(int) data.columns];
            java.util.Arrays.fill(columnTypes, -1);
        }
        if (columnTypes[column - 1] < 0) {
            int found = Native.T_NULL;
            for (long r = 0; r < rows; r++) {
                int t = data.type(r, column - 1);
                if (t != Native.T_NULL) {
                    found = t;
                    break;
                }
            }
            columnTypes[column - 1] = found;
        }
        return columnTypes[column - 1];
    }

    @Override
    public boolean wasNull() throws SQLException {
        checkOpen();
        return wasNull;
    }

    // ----- moving -----------------------------------------------------------------------------

    @Override
    public boolean next() throws SQLException {
        checkOpen();
        if (row < rows) {
            row++;
        }
        return row < rows;
    }

    private void needScroll() throws SQLException {
        checkOpen();
        if (type == ResultSet.TYPE_FORWARD_ONLY) {
            throw new SQLException("the result set is forward-only (create the statement with TYPE_SCROLL_INSENSITIVE)", "24000");
        }
    }

    @Override
    public boolean previous() throws SQLException {
        needScroll();
        if (row >= 0) {
            row--;
        }
        return row >= 0;
    }

    @Override
    public boolean first() throws SQLException {
        needScroll();
        row = rows > 0 ? 0 : -1;
        return rows > 0;
    }

    @Override
    public boolean last() throws SQLException {
        needScroll();
        row = rows > 0 ? rows - 1 : -1;
        return rows > 0;
    }

    @Override
    public void beforeFirst() throws SQLException {
        needScroll();
        row = -1;
    }

    @Override
    public void afterLast() throws SQLException {
        needScroll();
        row = rows;
    }

    @Override
    public boolean absolute(int position) throws SQLException {
        needScroll();
        long target = position > 0 ? position - 1 : rows + position;
        row = Math.max(-1, Math.min(target, rows));
        return row >= 0 && row < rows;
    }

    @Override
    public boolean relative(int offset) throws SQLException {
        needScroll();
        row = Math.max(-1, Math.min(row + offset, rows));
        return row >= 0 && row < rows;
    }

    @Override
    public int getRow() throws SQLException {
        checkOpen();
        return row >= 0 && row < rows ? (int) row + 1 : 0;
    }

    @Override
    public boolean isBeforeFirst() throws SQLException {
        checkOpen();
        return rows > 0 && row < 0;
    }

    @Override
    public boolean isAfterLast() throws SQLException {
        checkOpen();
        return rows > 0 && row >= rows;
    }

    @Override
    public boolean isFirst() throws SQLException {
        checkOpen();
        return row == 0 && rows > 0;
    }

    @Override
    public boolean isLast() throws SQLException {
        checkOpen();
        return rows > 0 && row == rows - 1;
    }

    // ----- reading values ---------------------------------------------------------------------

    private SQLDataException cannot(String what, int column, int tag) {
        return new SQLDataException("cannot read column " + column + " (" + data.names[column - 1] + ") as " + what + ": it holds " + tagName(tag), "22018");
    }

    static String tagName(int tag) {
        return switch (tag) {
            case Native.T_NULL -> "NULL";
            case Native.T_INTEGER -> "an INTEGER";
            case Native.T_DOUBLE -> "a floating-point number";
            case Native.T_TEXT -> "TEXT";
            case Native.T_BOOL -> "a YESNO";
            case Native.T_DECIMAL -> "a DECIMAL";
            case Native.T_BIGNUM -> "a BIGNUM";
            case Native.T_CURRENCY -> "CURRENCY";
            case Native.T_RATIONAL -> "a RATIONAL";
            case Native.T_DATETIME -> "a DATETIME";
            case Native.T_TIMESTAMPTZ -> "a TIMESTAMPTZ";
            case Native.T_INTERVAL -> "an INTERVAL";
            case Native.T_JSON -> "JSON";
            default -> "a value of unknown type";
        };
    }

    @Override
    public String getString(int column) throws SQLException {
        int t = tag(column);
        return switch (t) {
            case Native.T_NULL -> null;
            case Native.T_INTEGER -> Long.toString(data.int64(row, column - 1));
            case Native.T_DOUBLE -> {
                double d = data.dbl(row, column - 1);
                yield d == Math.rint(d) && Math.abs(d) < 1e15 ? Long.toString((long) d) + ".0" : Double.toString(d);
            }
            default -> data.text(row, column - 1);
        };
    }

    @Override
    public boolean getBoolean(int column) throws SQLException {
        int t = tag(column);
        return switch (t) {
            case Native.T_NULL -> false;
            case Native.T_BOOL, Native.T_INTEGER -> data.int64(row, column - 1) != 0;
            case Native.T_DOUBLE -> data.dbl(row, column - 1) != 0;
            case Native.T_TEXT -> {
                String s = data.text(row, column - 1).trim().toLowerCase(Locale.ROOT);
                yield switch (s) {
                    case "true", "yes", "y", "t", "1" -> true;
                    case "false", "no", "n", "f", "0", "" -> false;
                    default -> throw cannot("a boolean", column, t);
                };
            }
            case Native.T_DECIMAL, Native.T_BIGNUM, Native.T_CURRENCY -> new BigDecimal(data.text(row, column - 1)).signum() != 0;
            default -> throw cannot("a boolean", column, t);
        };
    }

    private BigDecimal decimal(int column, int t) throws SQLException {
        try {
            return switch (t) {
                case Native.T_INTEGER, Native.T_BOOL -> BigDecimal.valueOf(data.int64(row, column - 1));
                case Native.T_DOUBLE -> BigDecimal.valueOf(data.dbl(row, column - 1));
                case Native.T_DECIMAL, Native.T_BIGNUM, Native.T_CURRENCY, Native.T_TEXT -> new BigDecimal(data.text(row, column - 1).trim());
                case Native.T_RATIONAL -> {
                    String[] parts = data.text(row, column - 1).split("/");
                    BigDecimal num = new BigDecimal(parts[0]);
                    yield parts.length == 1 ? num : num.divide(new BigDecimal(parts[1]), MathContext.DECIMAL128);
                }
                case Native.T_DATETIME, Native.T_TIMESTAMPTZ -> BigDecimal.valueOf(data.int64(row, column - 1));
                default -> throw cannot("a number", column, t);
            };
        } catch (NumberFormatException e) {
            throw cannot("a number", column, t);
        }
    }

    private long toLong(int column, long min, long max, String what) throws SQLException {
        int t = tag(column);
        if (t == Native.T_NULL) {
            return 0;
        }
        long v;
        if (t == Native.T_INTEGER || t == Native.T_BOOL || t == Native.T_DATETIME || t == Native.T_TIMESTAMPTZ) {
            v = data.int64(row, column - 1);
        } else {
            BigDecimal d = decimal(column, t);
            try {
                v = d.setScale(0, java.math.RoundingMode.DOWN).longValueExact();
            } catch (ArithmeticException e) {
                throw new SQLDataException("the value " + d.toPlainString() + " in column " + column + " does not fit in " + what, "22003");
            }
        }
        if (v < min || v > max) {
            throw new SQLDataException("the value " + v + " in column " + column + " does not fit in " + what, "22003");
        }
        return v;
    }

    @Override
    public byte getByte(int column) throws SQLException {
        return (byte) toLong(column, Byte.MIN_VALUE, Byte.MAX_VALUE, "a byte");
    }

    @Override
    public short getShort(int column) throws SQLException {
        return (short) toLong(column, Short.MIN_VALUE, Short.MAX_VALUE, "a short");
    }

    @Override
    public int getInt(int column) throws SQLException {
        return (int) toLong(column, Integer.MIN_VALUE, Integer.MAX_VALUE, "an int");
    }

    @Override
    public long getLong(int column) throws SQLException {
        return toLong(column, Long.MIN_VALUE, Long.MAX_VALUE, "a long");
    }

    @Override
    public double getDouble(int column) throws SQLException {
        int t = tag(column);
        return switch (t) {
            case Native.T_NULL -> 0;
            case Native.T_DOUBLE, Native.T_INTEGER -> data.dbl(row, column - 1);
            default -> decimal(column, t).doubleValue();
        };
    }

    @Override
    public float getFloat(int column) throws SQLException {
        return (float) getDouble(column);
    }

    @Override
    public BigDecimal getBigDecimal(int column) throws SQLException {
        int t = tag(column);
        return t == Native.T_NULL ? null : decimal(column, t);
    }

    @Override
    @SuppressWarnings("deprecation")
    public BigDecimal getBigDecimal(int column, int scale) throws SQLException {
        BigDecimal d = getBigDecimal(column);
        return d == null ? null : d.setScale(scale, java.math.RoundingMode.HALF_EVEN);
    }

    private BigInteger bigInteger(int column) throws SQLException {
        int t = tag(column);
        if (t == Native.T_NULL) {
            return null;
        }
        if (t == Native.T_BIGNUM) {
            return new BigInteger(data.text(row, column - 1));
        }
        return decimal(column, t).toBigInteger();
    }

    // Dates and times: a DATETIME has no time zone (it reads the same wherever the program runs); a
    // TIMESTAMPTZ is an instant, which these methods show in the zone of the JVM.

    private LocalDateTime localDateTime(int column) throws SQLException {
        int t = tag(column);
        switch (t) {
            case Native.T_NULL:
                return null;
            case Native.T_DATETIME:
                return LocalDateTime.ofEpochSecond(data.int64(row, column - 1), 0, ZoneOffset.UTC);
            case Native.T_TIMESTAMPTZ:
                return LocalDateTime.ofInstant(Instant.ofEpochSecond(data.int64(row, column - 1)), ZoneId.systemDefault());
            case Native.T_TEXT:
                try {
                    String s = data.text(row, column - 1).trim().replace(' ', 'T');
                    return s.length() <= 10 ? LocalDate.parse(s).atStartOfDay() : LocalDateTime.parse(s);
                } catch (java.time.DateTimeException e) {
                    throw cannot("a date", column, t);
                }
            default:
                throw cannot("a date or time", column, t);
        }
    }

    @Override
    public Timestamp getTimestamp(int column) throws SQLException {
        LocalDateTime t = localDateTime(column);
        return t == null ? null : Timestamp.valueOf(t);
    }

    @Override
    public Timestamp getTimestamp(int column, Calendar cal) throws SQLException {
        return getTimestamp(column);
    }

    @Override
    public Date getDate(int column) throws SQLException {
        LocalDateTime t = localDateTime(column);
        return t == null ? null : Date.valueOf(t.toLocalDate());
    }

    @Override
    public Date getDate(int column, Calendar cal) throws SQLException {
        return getDate(column);
    }

    @Override
    public Time getTime(int column) throws SQLException {
        LocalDateTime t = localDateTime(column);
        return t == null ? null : Time.valueOf(t.toLocalTime().withNano(0));
    }

    @Override
    public Time getTime(int column, Calendar cal) throws SQLException {
        return getTime(column);
    }

    private OffsetDateTime offsetDateTime(int column) throws SQLException {
        int t = tag(column);
        return switch (t) {
            case Native.T_NULL -> null;
            case Native.T_TIMESTAMPTZ -> OffsetDateTime.ofInstant(Instant.ofEpochSecond(data.int64(row, column - 1)), ZoneOffset.UTC);
            case Native.T_DATETIME -> LocalDateTime.ofEpochSecond(data.int64(row, column - 1), 0, ZoneOffset.UTC).atOffset(ZoneOffset.UTC);
            default -> throw cannot("a timestamp with a time zone", column, t);
        };
    }

    // ----- objects ----------------------------------------------------------------------------

    @Override
    public Object getObject(int column) throws SQLException {
        int t = tag(column);
        return switch (t) {
            case Native.T_NULL -> null;
            case Native.T_INTEGER -> data.int64(row, column - 1);
            case Native.T_DOUBLE -> data.dbl(row, column - 1);
            case Native.T_BOOL -> data.int64(row, column - 1) != 0;
            case Native.T_DECIMAL, Native.T_CURRENCY -> new BigDecimal(data.text(row, column - 1));
            case Native.T_BIGNUM -> new BigInteger(data.text(row, column - 1));
            case Native.T_DATETIME -> getTimestamp(column);
            case Native.T_TIMESTAMPTZ -> offsetDateTime(column);
            default -> data.text(row, column - 1); // TEXT, RATIONAL, INTERVAL, JSON
        };
    }

    @Override
    @SuppressWarnings("unchecked")
    public <T> T getObject(int column, Class<T> type) throws SQLException {
        Object v;
        if (type == String.class) {
            v = getString(column);
        } else if (type == Integer.class || type == int.class) {
            v = tag(column) == Native.T_NULL ? null : (Object) getInt(column);
        } else if (type == Long.class || type == long.class) {
            v = tag(column) == Native.T_NULL ? null : (Object) getLong(column);
        } else if (type == Short.class) {
            v = tag(column) == Native.T_NULL ? null : (Object) getShort(column);
        } else if (type == Byte.class) {
            v = tag(column) == Native.T_NULL ? null : (Object) getByte(column);
        } else if (type == Double.class || type == double.class) {
            v = tag(column) == Native.T_NULL ? null : (Object) getDouble(column);
        } else if (type == Float.class) {
            v = tag(column) == Native.T_NULL ? null : (Object) getFloat(column);
        } else if (type == Boolean.class || type == boolean.class) {
            v = tag(column) == Native.T_NULL ? null : (Object) getBoolean(column);
        } else if (type == BigDecimal.class) {
            v = getBigDecimal(column);
        } else if (type == BigInteger.class) {
            v = bigInteger(column);
        } else if (type == LocalDateTime.class) {
            v = localDateTime(column);
        } else if (type == LocalDate.class) {
            LocalDateTime t = localDateTime(column);
            v = t == null ? null : t.toLocalDate();
        } else if (type == LocalTime.class) {
            LocalDateTime t = localDateTime(column);
            v = t == null ? null : t.toLocalTime();
        } else if (type == OffsetDateTime.class) {
            v = offsetDateTime(column);
        } else if (type == Instant.class) {
            OffsetDateTime t = offsetDateTime(column);
            v = t == null ? null : t.toInstant();
        } else if (type == Timestamp.class) {
            v = getTimestamp(column);
        } else if (type == Date.class) {
            v = getDate(column);
        } else if (type == Time.class) {
            v = getTime(column);
        } else if (type == Object.class) {
            v = getObject(column);
        } else {
            throw new SQLDataException("cannot convert column " + column + " to " + type.getName(), "22018");
        }
        return type.isPrimitive() ? (T) v : type.cast(v);
    }

    // ----- by name ----------------------------------------------------------------------------

    @Override
    public int findColumn(String label) throws SQLException {
        checkOpen();
        for (int i = 0; i < data.names.length; i++) {
            if (data.names[i].equalsIgnoreCase(label)) {
                return i + 1;
            }
        }
        throw new SQLException("the result has no column named '" + label + "'", "42S22");
    }

    @Override
    public String getString(String label) throws SQLException {
        return getString(findColumn(label));
    }

    @Override
    public boolean getBoolean(String label) throws SQLException {
        return getBoolean(findColumn(label));
    }

    @Override
    public byte getByte(String label) throws SQLException {
        return getByte(findColumn(label));
    }

    @Override
    public short getShort(String label) throws SQLException {
        return getShort(findColumn(label));
    }

    @Override
    public int getInt(String label) throws SQLException {
        return getInt(findColumn(label));
    }

    @Override
    public long getLong(String label) throws SQLException {
        return getLong(findColumn(label));
    }

    @Override
    public float getFloat(String label) throws SQLException {
        return getFloat(findColumn(label));
    }

    @Override
    public double getDouble(String label) throws SQLException {
        return getDouble(findColumn(label));
    }

    @Override
    public BigDecimal getBigDecimal(String label) throws SQLException {
        return getBigDecimal(findColumn(label));
    }

    @Override
    @SuppressWarnings("deprecation")
    public BigDecimal getBigDecimal(String label, int scale) throws SQLException {
        return getBigDecimal(findColumn(label), scale);
    }

    @Override
    public Date getDate(String label) throws SQLException {
        return getDate(findColumn(label));
    }

    @Override
    public Date getDate(String label, Calendar cal) throws SQLException {
        return getDate(findColumn(label));
    }

    @Override
    public Time getTime(String label) throws SQLException {
        return getTime(findColumn(label));
    }

    @Override
    public Time getTime(String label, Calendar cal) throws SQLException {
        return getTime(findColumn(label));
    }

    @Override
    public Timestamp getTimestamp(String label) throws SQLException {
        return getTimestamp(findColumn(label));
    }

    @Override
    public Timestamp getTimestamp(String label, Calendar cal) throws SQLException {
        return getTimestamp(findColumn(label));
    }

    @Override
    public Object getObject(String label) throws SQLException {
        return getObject(findColumn(label));
    }

    @Override
    public <T> T getObject(String label, Class<T> type) throws SQLException {
        return getObject(findColumn(label), type);
    }

    @Override
    public String getNString(int column) throws SQLException {
        return getString(column);
    }

    @Override
    public String getNString(String label) throws SQLException {
        return getString(label);
    }

    // ----- properties -------------------------------------------------------------------------

    @Override
    public ResultSetMetaData getMetaData() throws SQLException {
        checkOpen();
        return new AccessorResultSetMetaData(this);
    }

    @Override
    public Statement getStatement() throws SQLException {
        checkOpen();
        return owner;
    }

    @Override
    public int getType() throws SQLException {
        checkOpen();
        return type;
    }

    @Override
    public int getConcurrency() throws SQLException {
        checkOpen();
        return ResultSet.CONCUR_READ_ONLY;
    }

    @Override
    public int getHoldability() throws SQLException {
        checkOpen();
        return ResultSet.CLOSE_CURSORS_AT_COMMIT;
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
    public SQLWarning getWarnings() throws SQLException {
        checkOpen();
        return null;
    }

    @Override
    public void clearWarnings() throws SQLException {
        checkOpen();
    }

    // ----- closing ----------------------------------------------------------------------------

    void closeQuietly() {
        if (!closed) {
            closed = true;
            data.close();
        }
    }

    @Override
    public void close() throws SQLException {
        if (!closed) {
            closeQuietly();
            owner.resultSetClosed(this);
        }
    }

    @Override
    public boolean isClosed() {
        return closed;
    }
}
