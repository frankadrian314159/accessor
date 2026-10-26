# Table inheritance and partitioning

Both are the same thing underneath: a **child** table that is read together with its **parent**.

## Inheritance

```sql
CREATE TABLE cities (name TEXT, population INTEGER);
CREATE TABLE capitals (state TEXT) INHERITS (cities);

INSERT INTO cities   VALUES ('Oakland', 400000);
INSERT INTO capitals VALUES ('Sacramento', 525000, 'CA');

SELECT name FROM cities;          -- both rows, in cities' columns
SELECT name FROM ONLY cities;     -- just Oakland
SELECT * FROM capitals;           -- name, population, state
```

A child has the columns of its parent(s) first, then its own. A column named in both must have the same type and takes the child's definition (a `NOT NULL` of either stays). Defaults and `NOT NULL` are inherited; **primary keys, unique indexes, foreign keys and checks are not**.

* `SELECT FROM parent` reads the parent and every descendant, each projected on the parent's columns. `ONLY` leaves the descendants out.
* `UPDATE parent` and `DELETE FROM parent` reach the descendants too; `UPDATE ONLY` and `DELETE FROM ONLY` do not. `TRUNCATE` does the same as `DELETE FROM`.
* `INSERT INTO parent` inserts into the parent, and `INSERT INTO child` into the child.
* `DROP TABLE parent` is refused while it has children; `DROP TABLE parent CASCADE` drops them with it. Dropping a child just removes the link.
* A child may have several parents (`INHERITS (a, b)`).

## Partitioning

```sql
CREATE TABLE measurements (taken DATE, value INTEGER) PARTITION BY RANGE (taken);
CREATE TABLE m2024 PARTITION OF measurements FOR VALUES FROM ('2024-01-01') TO ('2025-01-01');
CREATE TABLE m2025 PARTITION OF measurements FOR VALUES FROM ('2025-01-01') TO (MAXVALUE);

CREATE TABLE regions (code TEXT) PARTITION BY LIST (code);
CREATE TABLE r_eu    PARTITION OF regions FOR VALUES IN ('de', 'fr');
CREATE TABLE r_other PARTITION OF regions DEFAULT;

CREATE TABLE h (k INTEGER) PARTITION BY HASH (k);
CREATE TABLE h0 PARTITION OF h FOR VALUES WITH (MODULUS 2, REMAINDER 0);
CREATE TABLE h1 PARTITION OF h FOR VALUES WITH (MODULUS 2, REMAINDER 1);
```

A partitioned table is a parent that is expected to hold no rows of its own. A partition has exactly the parent's columns, and `INSERT INTO parent` sends each row to the partition whose bound takes it (a partition may itself be partitioned, and the row goes down the levels). A row that no partition takes is refused: "no partition of relation "p" found for row". A `DEFAULT` partition takes what the others do not. Reads, `UPDATE`, `DELETE` and `DROP` are as for inheritance.

* A range bound is `FROM (a) TO (b)`, from inclusive to exclusive, with `MINVALUE` and `MAXVALUE` for the ends; a key of several columns compares column by column. A `NULL` key fits no range partition.
* The key is a list of **column names**; a key made of expressions is refused.
* A hash partition's number comes from this engine's own hash, so rows land in different partitions than in PostgreSQL.
* `CREATE TABLE ... (LIKE ...)`, `ATTACH PARTITION` and `DETACH PARTITION` are not available.

## What is not done

* **No partition pruning.** A query reads every partition and then filters, so a `WHERE taken = '2024-03-01'` costs the same as a full scan of all of them. The partitions are ordinary tables with ordinary [indexes](../optimizer/access-paths.md), but the plan does not yet choose among them by the bound.
* A row is not moved when an `UPDATE` changes its key to another partition's range; it stays where it is.
* A primary key or unique index applies within one table, not across the family.
* `tableoid` and `pg_inherits` do not exist.
