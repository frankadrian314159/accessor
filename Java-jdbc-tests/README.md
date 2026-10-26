# Java-jdbc-tests

A plain-JDBC test of Accessor's driver: only `java.sql` is used. `AccessorJdbcTest.java` runs as a single source file (Java 22 or later).

```
(cd ../jdbc && ./build.sh --jar)     # once: builds ../jdbc/build/accessor-jdbc.jar (driver + native library)
./run.sh                             # or  .\run.ps1  on Windows
```

It covers URLs, creating tables, prepared statements with generated keys, exact types (decimal, bignum, datetime, Unicode text), transactions including DDL that rolls back, all-or-nothing batches, SQL states, `DatabaseMetaData`, and a file database that persists and is locked while open. Exit code 0 means every check passed.
