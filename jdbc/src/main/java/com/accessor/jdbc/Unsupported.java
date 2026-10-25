package com.accessor.jdbc;

import java.sql.SQLFeatureNotSupportedException;

/** The exception for JDBC features the driver does not provide. */
final class Unsupported {
    private Unsupported() {}

    static SQLFeatureNotSupportedException feature(String what) {
        return new SQLFeatureNotSupportedException(what + " is not supported by the Accessor driver", "0A000");
    }
}
