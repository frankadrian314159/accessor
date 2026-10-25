# Stored functions and procedures

A **function** is a named expression with parameters that you can use anywhere an expression goes. A **procedure** is a named block of statements with variables and control flow, run with `CALL`. Both are stored in the database file and can be changed with `CREATE OR REPLACE`.

## Functions

```sql
CREATE [OR REPLACE] FUNCTION name ( [@param type [= default] [, ...]] )
    [RETURNS type]
AS expression;

DROP FUNCTION [IF EXISTS] name;
```

```sql
CREATE FUNCTION tax(@amount CURRENCY, @rate DECIMAL = 0.2) RETURNS CURRENCY AS @amount * @rate;
CREATE FUNCTION gross(@amount CURRENCY) AS @amount + tax(@amount);
CREATE FUNCTION label(@n INTEGER) AS CASE WHEN @n > 1 THEN 'many' WHEN @n = 1 THEN 'one' ELSE 'none' END;
CREATE FUNCTION spent(@id INTEGER) RETURNS CURRENCY AS
    (SELECT COALESCE(SUM(Total), 0) FROM Orders WHERE CustID = @id);

SELECT Name, spent(ID), gross(spent(ID)) FROM Customers WHERE spent(ID) > 100 ORDER BY spent(ID) DESC;
SELECT tax(100), tax(100, 0.1);
```

* The body is **one expression**: arithmetic, `CASE`, other functions (built-in or stored), and sub-queries. A parameter is written `@name` in the body.
* A parameter with a type converts its argument to that type (`tax('100')` works); `RETURNS type` converts the result.
* A parameter with `= default` may be left out of the call (only at the end of the list). A call may not give more arguments than there are parameters.
* A function may call other functions. A function that calls itself, however indirectly, is refused when it nests more than 16 deep.
* An argument that calls a function giving a new value each time (`NEXTVAL`, ...) cannot be used for a parameter that the body uses more than once, since the call would take two values; compute it first.
* A function cannot be named like a built-in function, and a name is either a function or a procedure.
* A function body cannot contain an aggregate (`SUM(...)` over the rows of the calling query) or a window function; a sub-query with its own aggregate, as in `spent` above, is fine.

### How a call is evaluated

A call is **expanded in place**: the optimizer sees the body with the arguments put into it, as if you had written the expression out. So a call with constant arguments is a constant (`WHERE ID = gross(0) + 2` can use the primary-key index), a call on a column is evaluated like the expression it stands for, and a sub-query in the body whose parameters become outer columns is an ordinary correlated sub-query, which the optimizer may turn into a [semi-join](../optimizer/subqueries.md). There is no per-call overhead beyond the expression itself.

## Procedures

```sql
CREATE [OR REPLACE] PROCEDURE name ( [@param type [= default] [, ...]] )
AS BEGIN
    statement;
    ...
END;

CALL name (args);            -- or EXEC / EXECUTE
CALL name (@param = value);
DROP PROCEDURE [IF EXISTS] name;
```

```sql
CREATE PROCEDURE restock(@id INTEGER, @qty INTEGER = 10) AS BEGIN
    DECLARE @cur INTEGER = (SELECT stock FROM item WHERE id = @id);
    IF @cur IS NULL THEN
        RAISE 'no such item';
    ELSEIF @cur + @qty > 100 THEN
        SET @qty = 100 - @cur;
    END IF;
    UPDATE item SET stock = stock + @qty WHERE id = @id;
    INSERT INTO log VALUES ('restocked ' & @id & ' by ' & @qty);
    RETURN @qty;
END;

CALL restock(7);              -- gives 10
CALL restock(7, 500);         -- tops up to 100
```

### Statements in a body

| Statement | Meaning |
| --- | --- |
| `DECLARE @v [type] [= expr]` | a variable; with a type, every value assigned is converted to it |
| `SET @v = expr` | assign to a variable (or a parameter) that has been declared |
| `IF cond THEN ... [ELSEIF cond THEN ...] [ELSE ...] END IF` | choice; the condition must be true (not NULL) to take its branch |
| `WHILE cond DO ... END WHILE` | loop |
| `LEAVE` | leave the innermost `WHILE` |
| `RETURN [expr]` | stop; with a value, the call gives that value |
| `RAISE expr` | stop with an error; everything the call did is undone |
| any `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `SELECT`, `CALL`, `TRUNCATE`, ... | run with the current values of the variables put in for `@name` |

Statements end with `;`. A variable or parameter used in an SQL statement is replaced by its value when the statement runs, so it can be used wherever a literal can (not for a table or column name).

### What a call gives back and what it does

* The result is that of `RETURN expr` if there was one (one row, one column named `return`), otherwise the result of the **last statement** that ran, such as a `SELECT` or the row count of an `UPDATE`.
* A procedure runs as **one unit**: if any statement fails, or `RAISE` runs, everything it did is undone, and when called inside a transaction only the call is undone.
* A procedure cannot start or end a transaction (`BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`).
* Procedures may call procedures, up to 16 deep. One call may make at most a million loop iterations in all; an endless loop is stopped with an error.
* Arguments are given in order, or by name (`@qty = 5`) in any order; unspecified parameters take their default; one with none is an error.

## Where they are kept

A routine is stored as its `CREATE` text, in the same file as the tables, and is part of the schema: creating or dropping one is transactional. `Database::routine_names()` lists them; `.routines` does so in the shell.
