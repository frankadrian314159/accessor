# Schemas, domains, enumerated types and extensions

These come from PostgreSQL. They are accepted so that scripts written for it run; each is simpler here than there, and this page says how.

All four are kept in the database file and are part of the schema: creating or dropping one is [transactional](transactions.md).

## Schemas

```sql
CREATE SCHEMA [IF NOT EXISTS] name;
CREATE SCHEMA AUTHORIZATION role;       -- the schema is named after the role
DROP SCHEMA [IF EXISTS] name [CASCADE];

CREATE TABLE sales.orders (id INTEGER PRIMARY KEY, total CURRENCY);
SELECT * FROM sales.orders;
```

A schema is a **prefix**. A table of schema `sales` is an ordinary table whose name is `sales.orders`, and it must be written that way (or with an alias: `FROM sales.orders o`). Several things follow:

* The schema must exist when a table is created in it ("schema "sales" does not exist").
* `public.t` and `pg_catalog.t` are the same as `t`: those two are the default and are left off.
* `DROP SCHEMA` is refused while the schema holds tables or queries; `CASCADE` drops them with it.
* A column is still `table.column`, not `schema.table.column`.
* There is no `search_path`: `SET search_path` is remembered (see [session](session.md)) but a bare `orders` never finds `sales.orders`.
* `CREATE SCHEMA name CREATE TABLE ...` (objects inside the statement) is not accepted; create them afterwards with the prefix.

## Domains

```sql
CREATE DOMAIN name [AS] type [DEFAULT literal] [NOT NULL] [CHECK (condition using VALUE)] ...;
DROP DOMAIN [IF EXISTS] name;

CREATE DOMAIN posint AS INTEGER NOT NULL DEFAULT 1 CHECK (VALUE > 0);
CREATE TABLE t (id posint, qty posint);
SELECT '7'::posint + 1;
```

A domain is a name for a built-in type with rules. A column declared with it gets the base type, the domain's default, `NOT NULL`, and each `CHECK` (with `VALUE` replaced by the column). The rules are **copied into the column when the table is created**: changing or dropping the domain later does not touch tables that already use it, and `CAST(x AS domain)` is a cast to the base type that does not check the rules. A domain may be made from another domain. `ALTER DOMAIN` is not available.

## Enumerated types

```sql
CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
DROP TYPE [IF EXISTS] mood;

CREATE TABLE person (name TEXT, current mood);
INSERT INTO person VALUES ('Moe', 'happy');     -- 'angry' would be refused
```

An enum column is a `TEXT` column with a `CHECK` that the value is one of the labels. What that leaves out: values are **compared as text**, so `'sad' < 'happy'` is alphabetical, not by the order the labels were listed; `ALTER TYPE ... ADD VALUE` is not available; and the check is copied into the column like a domain's.

`CREATE TYPE` for composite, range and base types is refused with a message saying so. Domains and enums share one namespace of type names.

## Extensions

```sql
CREATE EXTENSION [IF NOT EXISTS] name [WITH SCHEMA s] [VERSION 'v'] [CASCADE];
DROP EXTENSION [IF EXISTS] name [CASCADE];
```

The name is recorded (a second `CREATE EXTENSION` of it fails, a `DROP` of one never created fails) and nothing else happens: the functions an extension would bring are either built in already ([PostgreSQL functions](../functions/postgresql.md)) or absent.
