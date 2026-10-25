package com.accessor.jdbc;

import java.sql.SQLException;
import java.sql.Savepoint;

final class AccessorSavepoint implements Savepoint {
    private final int id;
    private final String name;

    AccessorSavepoint(int id, String name) {
        this.id = id;
        this.name = name;
    }

    static AccessorSavepoint of(Savepoint s) throws SQLException {
        if (s instanceof AccessorSavepoint a) {
            return a;
        }
        throw new SQLException("not a savepoint of this driver", "3B001");
    }

    /** The name in SQL: names given by the program are bracketed, numbered ones are generated. */
    String sqlName() {
        return name == null ? "sp_" + id : "[" + name.replace("]", "") + "]";
    }

    @Override
    public int getSavepointId() throws SQLException {
        if (name != null) {
            throw new SQLException("this is a named savepoint", "HY000");
        }
        return id;
    }

    @Override
    public String getSavepointName() throws SQLException {
        if (name == null) {
            throw new SQLException("this is an unnamed savepoint", "HY000");
        }
        return name;
    }
}
