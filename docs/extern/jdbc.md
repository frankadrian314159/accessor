# The JDBC driver

`jdbc/` holds a JDBC 4 driver for Accessor. It calls the C interface ([c.md](c.md)) through the Java **foreign-function API** (Panama), so it needs **Java 22 or later** and nothing else: no JNI code, no extra native glue.

## Using it

```
cd jdbc && ./build.sh --jar          # compiles, runs the tests, writes build/accessor-jdbc.jar
```

The jar contains the driver, the service file that registers it, and the native library for the platform it was built on (`/native/<os>-<arch>/`), which the driver extracts to a temporary file when it first needs it. Put the jar on the class path and run Java with native access enabled:

```
java --enable-native-access=ALL-UNNAMED -cp accessor-jdbc.jar:myapp.jar MyApp
```

```java
try (Connection c = DriverManager.getConnection("jdbc:accessor:/data/shop.adb");
     PreparedStatement ps = c.prepareStatement("SELECT Name, Total FROM Orders WHERE CustID = ?")) {
    ps.setInt(1, 7);
    try (ResultSet r = ps.executeQuery()) {
        while (r.next()) System.out.println(r.getString("Name") + " " + r.getBigDecimal("Total"));
    }
}
```

A different native library can be named with `-Daccessor.library=/path/to/accessor_capi.dll` or `ACCESSOR_LIBRARY`. Otherwise the driver looks in the jar, then on the operating system's library path.

## URLs

| URL | Opens |
| --- | --- |
| `jdbc:accessor:/path/shop.adb`, `jdbc:accessor:C:/data/shop.adb` | a database file, created if it does not exist |
| `jdbc:accessor:mem:` | a private database in memory |
| `jdbc:accessor:/path/shop.adb?durability=os&optimizer=off&timezone=Europe/Paris` | options, in the URL or in the connection properties |

| Option | Values |
| --- | --- |
| `durability` | `fsync` (default: commits survive power loss) or `os` (they survive a crash of the program only, and are faster) |
| `optimizer` | `on` (default) or `off`; answers are the same |
| `timezone` | an IANA zone, for showing `TIMESTAMPTZ` values |

**A file can be open in one process at a time** (and, through this driver, by one connection); a second `getConnection` fails with SQL state `08001` and a message saying the database is in use ([locking.md](locking.md)).

## Types

| Accessor | `getObject` gives | Notes |
| --- | --- | --- |
| `INTEGER`, `COUNTER` | `Long` | |
| `FLOAT`, `DOUBLE` | `Double` | |
| `TEXT` | `String` | any text, including NUL and emoji, of any length |
| `YESNO` | `Boolean` | |
| `DECIMAL`, `CURRENCY` | `BigDecimal` | exact: `0.1 + 0.2` is `0.3` |
| `BIGNUM` | `BigInteger` | any size |
| `RATIONAL`, `INTERVAL` | `String` | |
| `DATETIME` | `java.sql.Timestamp` | no time zone: reads the same wherever the program runs. `getObject(i, LocalDateTime.class)`, `LocalDate`, `LocalTime`, `Date`, `Time` also work |
| `TIMESTAMPTZ` | `OffsetDateTime` (UTC) | an instant; `getObject(i, Instant.class)` too |
| `JSON` | `String` | the canonical document |

Parameters accept `Boolean`, the integer types, `Float`, `Double`, `BigDecimal`, `BigInteger`, `String`, `Character`, `UUID`, enums (by name), `java.sql.Date`/`Time`/`Timestamp`, `LocalDate`/`LocalTime`/`LocalDateTime` (as `DATETIME`), and `OffsetDateTime`/`ZonedDateTime`/`Instant`/`java.util.Date` (as `TIMESTAMPTZ`). Setting a `byte[]`, a stream, or an object of another class throws `SQLFeatureNotSupportedException`.

The type reported by `ResultSetMetaData` is that of the first non-NULL value in the column: the engine's expressions can have a different type in each row.

## Statements and results

* `?` placeholders are rewritten to the engine's `@p1, @p2, ...` (question marks inside text, `[names]`, `#dates#` and comments are left alone). SQL written with `@name` parameters also works if you set them by the positions the engine gives them.
* `executeQuery` / `execute` / `executeUpdate`, `getUpdateCount`, `setMaxRows`, `closeOnCompletion`, `Statement.addBatch` / `executeBatch`. A **batch is all or nothing**: if one statement fails, none of them has any effect, and a `BatchUpdateException` is thrown.
* **Generated keys**: `prepareStatement(sql, RETURN_GENERATED_KEYS)` (or `executeUpdate(sql, RETURN_GENERATED_KEYS)`) on an `INSERT` returns the `COUNTER` column, or a column filled from a sequence, through `getGeneratedKeys()`. Behind it is `RETURNING`, which can also be written by hand and gives an ordinary result set.
* Result sets are **forward-only** by default and `TYPE_SCROLL_INSENSITIVE` on request (`first`, `last`, `absolute`, `relative`, `previous`, ...). They are read-only, and a copy: closing the statement or connection closes them.
* Multiple results from one statement (`getMoreResults`) are not supported: a script gives the result of its last statement.

## Transactions

Auto-commit is on by default: each statement is its own transaction. `setAutoCommit(false)` starts a transaction at the first statement; `commit()` and `rollback()` end it. **Schema changes take part**: `CREATE TABLE` in a transaction that is rolled back is undone. `setAutoCommit(true)` commits an open transaction, as JDBC says. Savepoints (`setSavepoint`, `rollback(sp)`, `releaseSavepoint`) work inside a transaction. There is one isolation level, `TRANSACTION_SERIALIZABLE`; asking for a lower one gets it, which JDBC allows. Closing a connection with a transaction open rolls it back.

## Errors

`SQLException.getSQLState()` and `getErrorCode()` (the `ACC_ERR_*` code of [c.md](c.md)):

| State | Cause |
| --- | --- |
| `42000` | syntax error |
| `42S02` / `42S01` | a table (or other object) does not exist / already exists |
| `23000` | a constraint refused the change |
| `22018`, `22021`, `22003` | wrong type, bad text encoding, number out of range |
| `08001` | the database could not be opened, such as when it is in use |
| `40001` | two transactions changed the same row and this one must be run again (reserved: one connection per database file cannot raise it today) |
| `08003` | the connection is closed |
| `07001`, `07009` | a parameter has no value / a parameter or column index is out of range |
| `25000`, `3B001` | a transaction or savepoint call in the wrong state |
| `58030`, `XX001` | an input/output error / the file is not a database or is damaged |
| `0A000` | a feature the driver does not have (`SQLFeatureNotSupportedException`) |
| `HY000`, `HY010` | any other error / wrong use (a closed statement) |

## Metadata

`DatabaseMetaData` answers from [`INFORMATION_SCHEMA`](../sql/information-schema.md): `getTables`, `getColumns`, `getPrimaryKeys`, `getImportedKeys` / `getExportedKeys`, `getIndexInfo`, `getProcedures`, `getFunctions`, `getTypeInfo`, `getTableTypes`, and the capability methods. A database has one schema and no catalogs, so `getSchemas()` and `getCatalogs()` are empty and the schema and catalog columns of other results are NULL. `getIdentifierQuoteString()` is a space, because names are quoted `[like this]`, which a single string cannot say.

## Accessor extras

`connection.unwrap(AccessorConnection.class)` gives `renderReport(name, "text" | "html" | "markdown" | "csv" | "pdf")`, which returns the bytes of a stored report ([reports](../sql/reports.md)), and `checkpoint()`.

## Not supported

Callable statements (`prepareCall`; use `CALL name(?)` through a prepared statement), blobs, clobs, arrays, `SQLXML`, `RowId`, updatable result sets, `ParameterMetaData` types (parameters have no declared type), query cancellation and timeouts (queries run to completion), `Savepoint`s outside a transaction, and `Connection.setReadOnly` (a hint only).

## The stubs

`java.sql.Connection`, `Statement`, `PreparedStatement`, `ResultSet` and `DatabaseMetaData` have hundreds of methods. The driver classes extend generated `Unsupported*` classes that implement all of them by throwing `SQLFeatureNotSupportedException`; `jdbc/tools/gen_stubs.py` writes them from the JDK's own interfaces, so a newer JDK's added methods are covered by running it again.
