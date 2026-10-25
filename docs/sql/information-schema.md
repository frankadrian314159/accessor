# INFORMATION_SCHEMA

The catalog of the database (its tables, columns, constraints and so on) is available as read-only tables in a schema called `INFORMATION_SCHEMA`, so that tools and scripts can find out what is in a database with ordinary queries.

```sql
SELECT column_name, data_type, is_nullable
FROM INFORMATION_SCHEMA.COLUMNS
WHERE table_name = 'Orders'
ORDER BY ordinal_position;
```

The rows are worked out from the catalog each time they are read, so they are always current, and nothing is stored. They can be used anywhere a table can: joined, filtered, aggregated, given an alias. Names are not case-sensitive (`information_schema.columns` works). Values in the *name* columns keep the case they were created with.

## The views

| View | One row for | Columns |
| --- | --- | --- |
| `TABLES` | each table and saved query | `table_name`, `table_type` (`BASE TABLE` or `VIEW`), `row_count` (tables only) |
| `COLUMNS` | each column of each table, and each column a saved query returns | `table_name`, `column_name`, `ordinal_position` (from 1), `data_type`, `is_nullable` (`YES`/`NO`), `column_default`, `is_primary_key`, `is_autonumber`, `calculated_expression`. For a saved query only the name and position are known. `column_default` shows a sequence default as `NEXT VALUE FOR name` |
| `VIEWS` | each saved query | `table_name`, `view_definition`, `is_updatable` (`YES` if it is a [simple view](queries-and-triggers.md#changing-data-through-a-view)), `check_option` |
| `TABLE_CONSTRAINTS` | each primary key, unique index and foreign key | `constraint_name`, `table_name`, `constraint_type` (`PRIMARY KEY`, `UNIQUE`, `FOREIGN KEY`) |
| `KEY_COLUMN_USAGE` | each column of each of those | `constraint_name`, `table_name`, `column_name`, `ordinal_position`, `referenced_table`, `referenced_column` (foreign keys) |
| `REFERENTIAL_CONSTRAINTS` | each foreign key | `constraint_name`, `table_name`, `referenced_table`, `update_rule`, `delete_rule`, `is_deferrable`, `initially_deferred` |
| `INDEXES` | each column of each index | `table_name`, `index_name`, `column_name`, `ordinal_position`, `is_unique` |
| `ROUTINES` | each stored function and procedure | `routine_name`, `routine_type` (`FUNCTION`/`PROCEDURE`), `data_type` (the `RETURNS` type), `routine_definition` |
| `PARAMETERS` | each parameter of each routine | `routine_name`, `parameter_name`, `ordinal_position`, `data_type`, `has_default` |
| `SEQUENCES` | each sequence | `sequence_name`, `start_value`, `increment`, `minimum_value`, `maximum_value`, `cycle_option`, `last_value` |
| `TRIGGERS` | each trigger | `trigger_name`, `event_object_table`, `action_timing` (`BEFORE`, `AFTER`, `INSTEAD OF`), `event_manipulation` (such as `INSERT, UPDATE`), `action_orientation` (`ROW`/`STATEMENT`), `action_statement` |
| `REPORTS` | each report | `report_name`, `report_definition` |

`CHECK` constraints are implemented as triggers and are not listed in `TABLE_CONSTRAINTS`. There are no `catalog` or `schema` columns, because a database has just one schema; `INFORMATION_SCHEMA` is the only schema name ASQL knows, and `INFORMATION_SCHEMA.x` is the only form of qualified name.

```sql
-- every column that refers to another table
SELECT k.table_name, k.column_name, k.referenced_table
FROM INFORMATION_SCHEMA.KEY_COLUMN_USAGE k
WHERE k.referenced_table IS NOT NULL;

-- tables that nothing is indexed on
SELECT t.table_name FROM INFORMATION_SCHEMA.TABLES t
WHERE t.table_type = 'BASE TABLE'
  AND NOT EXISTS (SELECT 1 FROM INFORMATION_SCHEMA.INDEXES i WHERE i.table_name = t.table_name);
```

## The optimizer

Each view is a small table computed when the query runs, and is treated like a derived table: its row count is known exactly, and it can be joined in any order. It has no indexes.
