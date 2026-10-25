package com.accessor.jdbc;

import java.sql.ParameterMetaData;
import java.sql.SQLException;
import java.sql.Types;

/** The parameters of a prepared statement. Their types are not known until values are given. */
final class AccessorParameterMetaData extends UnsupportedParameterMetaData {
    private final int count;

    AccessorParameterMetaData(int count) {
        this.count = count;
    }

    private void check(int index) throws SQLException {
        if (index < 1 || index > count) {
            throw new SQLException("parameter " + index + " does not exist (the statement has " + count + ")", "07009");
        }
    }

    @Override
    public int getParameterCount() {
        return count;
    }

    @Override
    public int isNullable(int param) throws SQLException {
        check(param);
        return ParameterMetaData.parameterNullableUnknown;
    }

    @Override
    public boolean isSigned(int param) throws SQLException {
        check(param);
        return true;
    }

    @Override
    public int getPrecision(int param) throws SQLException {
        check(param);
        return 0;
    }

    @Override
    public int getScale(int param) throws SQLException {
        check(param);
        return 0;
    }

    @Override
    public int getParameterType(int param) throws SQLException {
        check(param);
        return Types.OTHER;
    }

    @Override
    public String getParameterTypeName(int param) throws SQLException {
        check(param);
        return "ANY";
    }

    @Override
    public String getParameterClassName(int param) throws SQLException {
        check(param);
        return Object.class.getName();
    }

    @Override
    public int getParameterMode(int param) throws SQLException {
        check(param);
        return ParameterMetaData.parameterModeIn;
    }
}
