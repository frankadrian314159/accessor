# Roles and privileges

```sql
CREATE ROLE name [WITH] [SUPERUSER | NOSUPERUSER] [CREATEDB] [CREATEROLE] [LOGIN | NOLOGIN] [PASSWORD 'x'] [IN ROLE r, ...] ...;
CREATE USER name ...;               -- the same, with LOGIN
CREATE GROUP name ...;
ALTER ROLE name [WITH] options;
ALTER ROLE name RENAME TO new;
DROP ROLE [IF EXISTS] a, b;

GRANT SELECT, INSERT ON t TO alice, bob;
GRANT ALL ON ALL TABLES IN SCHEMA sales TO staff;
GRANT staff TO alice;                -- membership
REVOKE INSERT ON t FROM alice;

SET ROLE alice;                      -- act as alice
SET ROLE NONE;                       -- back to the unrestricted session   (also RESET ROLE)
```

## What is enforced

A database starts with **no restrictions**: nothing is checked until `SET ROLE name` makes a role current. From then on, every statement of the session needs the right on each **table** it uses:

| Statement | Needs |
| --- | --- |
| `SELECT` (and a table named in a sub-query, join or `WITH`) | `SELECT` on the table |
| `INSERT` | `INSERT` |
| `UPDATE` | `UPDATE` |
| `DELETE`, `TRUNCATE` | `DELETE` |

The refusal is "permission denied for table t". A role has the rights granted to it, to any role it is a **member** of (directly or through other groups), and to `PUBLIC`. A `SUPERUSER` role has all rights. `GRANT ALL` means all of the table privileges (`SELECT`, `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE`, `REFERENCES`, `TRIGGER`).

A **view** is checked by its own name only: a role that may select from a view may read the tables under it without any right on them, as in PostgreSQL where the view's owner's rights are used. A trigger or a stored function runs with the rights of the role that fired it.

## What is only recorded

Everything else is accepted and kept but does not change what a role may do:

* Rights on schemas, sequences, functions, domains, the database (`GRANT USAGE ON SCHEMA s`, `GRANT CREATE ON DATABASE d`): stored, not checked.
* Role attributes (`CREATEDB`, `LOGIN`, `PASSWORD`, `CONNECTION LIMIT`, `VALID UNTIL`): stored, not checked. There is no log-in; a connection is not a role.
* `GRANT ... WITH GRANT OPTION`, `GRANTED BY`, column lists (`SELECT (a, b)`): read, and the option or list is ignored (a column privilege is a whole-table privilege).
* Tables have **no owner**. A role that creates a table has no automatic rights on it, and there is no `ALTER TABLE ... OWNER TO` or `ALTER DEFAULT PRIVILEGES`. Row-level security (`CREATE POLICY`) is not available.

Roles and grants live in the database file and are transactional. `SET ROLE` belongs to the open database, not to a transaction: `ROLLBACK` does not undo it. `DROP ROLE` removes the role's grants and memberships and, if it was the current role, goes back to the unrestricted session.
