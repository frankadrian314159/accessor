# Clojure-jdbc-tests

Clojure libraries running on Accessor's JDBC driver. Both need the Clojure CLI, Java 22 or later, and the driver jar:

```
(cd ../jdbc && ./build.sh --jar)     # once: builds ../jdbc/build/accessor-jdbc.jar (driver + native library)
```

| Folder | Library | Run |
| --- | --- | --- |
| `next/` | [next.jdbc](https://github.com/seancorfield/next-jdbc): `execute!`, `execute-one!` with `:return-keys`, `plan`, `next.jdbc.sql` helpers, `with-transaction`, `execute-batch!`, SQL states, metadata | `cd next && ./run.sh` (`.\run.ps1` on Windows) |
| `honey/` | [HoneySQL 2](https://github.com/seancorfield/honeysql) with next.jdbc: selects, joins, subqueries, CTEs, `UNION`, `RETURNING`, `ON CONFLICT`, transactions | `cd honey && ./run.sh` |

Each folder's `deps.edn` takes the driver as a local dependency and runs the tests with `--enable-native-access=ALL-UNNAMED` (the driver reaches the native library through the foreign-function API).

Notes for using these libraries with Accessor:

* A `jdbc:accessor:mem:` database belongs to the connection that opened it, so use one connection (as the tests do) or a file database (`jdbc:accessor:/path/db.adb`), not a datasource that opens a new connection per call.
* Accessor quotes names as `[name]`. With HoneySQL use `{:dialect :sqlserver :quoted true}`.
* Pass `{:builder-fn rs/as-unqualified-maps}` for plain keyword keys.
