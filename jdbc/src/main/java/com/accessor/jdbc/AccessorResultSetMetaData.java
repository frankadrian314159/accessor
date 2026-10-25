package com.accessor.jdbc;

import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.Types;

/**
 * Describes the columns of a result. The engine does not report a column's type separately from its
 * values (an expression can give a different type in each row), so the type shown is that of the
 * first value that is not NULL.
 */
final class AccessorResultSetMetaData extends UnsupportedResultSetMetaData {
    private final AccessorResultSet rs;

    AccessorResultSetMetaData(AccessorResultSet rs) {
        this.rs = rs;
    }

    private int tag(int column) throws SQLException {
        if (column < 1 || column > rs.columnCount()) {
            throw new SQLException("column " + column + " does not exist (the result has " + rs.columnCount() + ")", "07009");
        }
        return rs.columnTag(column);
    }

    @Override
    public int getColumnCount() {
        return rs.columnCount();
    }

    @Override
    public String getColumnLabel(int column) throws SQLException {
        tag(column);
        return rs.columnName(column);
    }

    @Override
    public String getColumnName(int column) throws SQLException {
        return getColumnLabel(column);
    }

    @Override
    public int getColumnType(int column) throws SQLException {
        return switch (tag(column)) {
            case Native.T_INTEGER -> Types.BIGINT;
            case Native.T_DOUBLE -> Types.DOUBLE;
            case Native.T_BOOL -> Types.BOOLEAN;
            case Native.T_DECIMAL, Native.T_BIGNUM -> Types.NUMERIC;
            case Native.T_CURRENCY -> Types.DECIMAL;
            case Native.T_DATETIME -> Types.TIMESTAMP;
            case Native.T_TIMESTAMPTZ -> Types.TIMESTAMP_WITH_TIMEZONE;
            case Native.T_TEXT -> Types.VARCHAR;
            case Native.T_RATIONAL, Native.T_INTERVAL, Native.T_JSON -> Types.OTHER;
            default -> Types.NULL;
        };
    }

    @Override
    public String getColumnTypeName(int column) throws SQLException {
        return switch (tag(column)) {
            case Native.T_INTEGER -> "INTEGER";
            case Native.T_DOUBLE -> "DOUBLE";
            case Native.T_BOOL -> "YESNO";
            case Native.T_DECIMAL -> "DECIMAL";
            case Native.T_BIGNUM -> "BIGNUM";
            case Native.T_CURRENCY -> "CURRENCY";
            case Native.T_RATIONAL -> "RATIONAL";
            case Native.T_DATETIME -> "DATETIME";
            case Native.T_TIMESTAMPTZ -> "TIMESTAMPTZ";
            case Native.T_INTERVAL -> "INTERVAL";
            case Native.T_JSON -> "JSON";
            case Native.T_TEXT -> "TEXT";
            default -> "NULL";
        };
    }

    @Override
    public String getColumnClassName(int column) throws SQLException {
        return switch (tag(column)) {
            case Native.T_INTEGER -> Long.class.getName();
            case Native.T_DOUBLE -> Double.class.getName();
            case Native.T_BOOL -> Boolean.class.getName();
            case Native.T_DECIMAL, Native.T_CURRENCY -> java.math.BigDecimal.class.getName();
            case Native.T_BIGNUM -> java.math.BigInteger.class.getName();
            case Native.T_DATETIME -> java.sql.Timestamp.class.getName();
            case Native.T_TIMESTAMPTZ -> java.time.OffsetDateTime.class.getName();
            default -> String.class.getName();
        };
    }

    @Override
    public int isNullable(int column) throws SQLException {
        tag(column);
        return columnNullableUnknown;
    }

    private static final int columnNullableUnknown = ResultSetMetaData.columnNullableUnknown;

    @Override
    public boolean isSigned(int column) throws SQLException {
        int t = tag(column);
        return t == Native.T_INTEGER || t == Native.T_DOUBLE || t == Native.T_DECIMAL || t == Native.T_BIGNUM || t == Native.T_CURRENCY;
    }

    @Override
    public boolean isCaseSensitive(int column) throws SQLException {
        tag(column);
        return false; // text compares without regard to case
    }

    @Override
    public boolean isAutoIncrement(int column) throws SQLException {
        tag(column);
        return false;
    }

    @Override
    public boolean isCurrency(int column) throws SQLException {
        return tag(column) == Native.T_CURRENCY;
    }

    @Override
    public boolean isSearchable(int column) throws SQLException {
        tag(column);
        return true;
    }

    @Override
    public boolean isReadOnly(int column) throws SQLException {
        tag(column);
        return true;
    }

    @Override
    public boolean isWritable(int column) throws SQLException {
        tag(column);
        return false;
    }

    @Override
    public boolean isDefinitelyWritable(int column) throws SQLException {
        tag(column);
        return false;
    }

    @Override
    public int getColumnDisplaySize(int column) throws SQLException {
        tag(column);
        return 40;
    }

    @Override
    public int getPrecision(int column) throws SQLException {
        tag(column);
        return 0;
    }

    @Override
    public int getScale(int column) throws SQLException {
        tag(column);
        return 0;
    }

    @Override
    public String getTableName(int column) throws SQLException {
        tag(column);
        return "";
    }

    @Override
    public String getSchemaName(int column) throws SQLException {
        tag(column);
        return "";
    }

    @Override
    public String getCatalogName(int column) throws SQLException {
        tag(column);
        return "";
    }
}
