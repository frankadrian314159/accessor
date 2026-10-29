# ASQL reference

ASQL is the SQL dialect of Accessor. It is a subset of standard SQL with the habits of Microsoft Access added: square-bracketed names, `&` for joining text, `#date#` literals, `TOP n`, `LIKE` with `*` and `?`, and AutoNumber. It also has some things Access never had: exact arithmetic of any size, common table expressions, window functions, `MERGE`, and transactions that cover schema changes.

**ASQL does not implement the whole of the latest SQL standard.** [conformance.md](conformance.md) lists exactly what it implements from the standard, where it differs, and what is missing.

| Document | Contents |
| --- | --- |
| [grammar.md](grammar.md) | the complete grammar in BNF |
| [select.md](select.md) | queries: joins, grouping, set operators, `WITH`, window functions, sub-queries, crosstabs (`TRANSFORM … PIVOT`) |
| [data-changes.md](data-changes.md) | `INSERT` (with `ON CONFLICT`), `UPDATE`, `DELETE`, `MERGE`, `TRUNCATE`, `RETURNING` |
| [ddl.md](ddl.md) | `CREATE`/`ALTER`/`DROP TABLE`, data types, constraints, indexes |
| [queries-and-triggers.md](queries-and-triggers.md) | saved queries, parameters, `EXEC`, prepared statements, triggers (row, statement, `INSTEAD OF`) |
| [transactions.md](transactions.md) | `BEGIN`, `COMMIT`, `ROLLBACK`, savepoints, `SET CONSTRAINTS` |
| [information-schema.md](information-schema.md) | `INFORMATION_SCHEMA`: the catalog as tables |
| [sequences.md](sequences.md) | `CREATE SEQUENCE`, `NEXT VALUE FOR`, sequence defaults |
| [routines.md](routines.md) | `CREATE FUNCTION` (expression functions) and `CREATE PROCEDURE` (variables, `IF`, `WHILE`), `CALL`; PostgreSQL-style `LANGUAGE sql` and PL/pgSQL |
| [schemas-and-types.md](schemas-and-types.md) | `CREATE SCHEMA`, `CREATE DOMAIN`, `CREATE TYPE ... AS ENUM`, `CREATE EXTENSION` |
| [roles-and-privileges.md](roles-and-privileges.md) | `CREATE ROLE`, `GRANT`, `REVOKE`, `SET ROLE` |
| [mysql.md](mysql.md) | MySQL syntax read by the engine: backtick names, `@variables`, `CREATE TABLE` options and keys, `SHOW`, `PREPARE ... FROM`, `INSERT IGNORE`, `ON DUPLICATE KEY UPDATE` |
| [inheritance-and-partitioning.md](inheritance-and-partitioning.md) | `INHERITS`, `PARTITION BY`, `PARTITION OF`, `ONLY` |
| [reports.md](reports.md) | `CREATE REPORT`: grouped, totalled, paged output as text, HTML, Markdown, CSV or PDF |
| [../optimizer/](../optimizer/README.md) | the query optimizer and `EXPLAIN` |
| [dialects.md](dialects.md) | `asql` (default), `postgresql` and `mysql`: `--postgresql`/`--mysql` flags, `SET dialect`, what each changes |
| [session.md](session.md) | `SET TIME ZONE` and other session settings |
| [expressions.md](expressions.md) | literals, operators, `NULL` logic, predicates, `LIKE`, `CASE`, `CAST` |
| [conformance.md](conformance.md) | comparison with the SQL standard |
| [../functions/README.md](../functions/README.md) | every built-in function |

## Quick tour

```sql
CREATE TABLE Customers (
    ID    COUNTER PRIMARY KEY,
    Name  TEXT NOT NULL,
    City  TEXT
);
CREATE TABLE Orders (
    ID      COUNTER PRIMARY KEY,
    CustID  INTEGER NOT NULL REFERENCES Customers (ID) ON DELETE CASCADE,
    Placed  DATETIME DEFAULT NULL,
    Total   CURRENCY CHECK (Total >= 0)
);

INSERT INTO Customers (Name, City) VALUES ('Ada', 'London'), ('Grace', 'New York');
INSERT INTO Orders (CustID, Placed, Total) VALUES (1, #2024-03-05#, 19.99), (1, #2024-04-01#, 5);

SELECT C.Name,
       COUNT(*)                                          AS Orders,
       SUM(O.Total)                                      AS Spent,
       RANK() OVER (ORDER BY SUM(O.Total) DESC)          AS Rank
FROM Customers C JOIN Orders O ON O.CustID = C.ID
GROUP BY C.Name;
```

## Conventions

* Statements end with `;` in scripts; the last one may omit it.
* Names and keywords are not case-sensitive. Text values compare without regard to case.
* `--` starts a comment that runs to the end of the line; `/* ... */` is a comment that can span lines.
* Examples in these pages use the table names `Customers`, `Orders`, `Emp`, and so on; they are made-up.
