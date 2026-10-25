# ASQL reference

ASQL is the SQL dialect of Accessor. It is a subset of standard SQL with the habits of Microsoft Access added: square-bracketed names, `&` for joining text, `#date#` literals, `TOP n`, `LIKE` with `*` and `?`, and AutoNumber. It also has some things Access never had: exact arithmetic of any size, common table expressions, window functions, `MERGE`, and transactions that cover schema changes.

**ASQL does not implement the whole of the latest SQL standard.** [conformance.md](conformance.md) lists exactly what it implements from the standard, where it differs, and what is missing.

| Document | Contents |
| --- | --- |
| [grammar.md](grammar.md) | the complete grammar in BNF |
| [select.md](select.md) | queries: joins, grouping, set operators, `WITH`, window functions, sub-queries, crosstabs (`TRANSFORM … PIVOT`) |
| [data-changes.md](data-changes.md) | `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `TRUNCATE` |
| [ddl.md](ddl.md) | `CREATE`/`ALTER`/`DROP TABLE`, data types, constraints, indexes |
| [queries-and-triggers.md](queries-and-triggers.md) | saved queries, parameters, `EXEC`, triggers |
| [transactions.md](transactions.md) | `BEGIN`, `COMMIT`, `ROLLBACK`, savepoints |
| [../optimizer/](../optimizer/README.md) | the query optimizer and `EXPLAIN` |
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
