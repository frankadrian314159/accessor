# Table functions

A table function returns rows and is used in the `FROM` clause like a table.

```sql
SELECT * FROM GENERATE_SERIES(1, 5);
SELECT value FROM SPLIT_STRING('a,b,c', ',') AS s;
```

Its arguments may be constants, or, written after `LATERAL` or following another table in the `FROM`, columns of the tables before it: then the function runs once for each of their rows ([LATERAL](../sql/select.md#lateral-joins)). Optional column names follow the alias: `GENERATE_SERIES(1, 3) AS g (n)`.

The optimizer treats a table function with constant arguments as a small table computed once, which it can join in any order. One that reads earlier tables is evaluated per distinct set of values, with the answers remembered.

## GENERATE_SERIES

`GENERATE_SERIES(start, stop [, step])` returns a column `generate_series` with `start`, `start + step`, ... while the value has not passed `stop`. The step defaults to 1 and may be negative; it must not be zero.

| Arguments | Rows |
| --- | --- |
| whole numbers, decimals or any numeric type | numbers of the type that results from adding (`1, 2.5, 4` for step 1.5 from 1 is `DECIMAL`) |
| `DATETIME` or `TIMESTAMPTZ` start and stop, with an `INTERVAL` step | dates and times: `GENERATE_SERIES(#2024-01-31#, #2024-04-30#, INTERVAL '1 month')` |
| any NULL argument | no rows |

A result of more than ten million rows is refused.

```sql
SELECT generate_series FROM GENERATE_SERIES(10, 1, -3);                    -- 10, 7, 4, 1
SELECT d FROM GENERATE_SERIES(#2024-03-01#, #2024-03-07#, INTERVAL '1 day') AS g (d);

-- every day of the month, with the day's sales (0 for none)
SELECT g.d, COALESCE(SUM(S.Amount), 0)
FROM GENERATE_SERIES(#2024-03-01#, #2024-03-31#, INTERVAL '1 day') AS g (d)
LEFT JOIN Sales S ON S.Day = g.d
GROUP BY g.d ORDER BY g.d;
```

Months are added one at a time to the previous value, so a series that starts on the 31st continues on the last day of each month it passes through (31 Jan, 29 Feb, 29 Mar, ...).

## JSON_EACH

`JSON_EACH(doc)` returns one row for each member of a JSON object, or element of a JSON array, with the columns `key` and `value`. For an array `key` is the position (from 0); for an object it is the member name. A scalar `value` is an ordinary SQL value (text, number, yes/no, or NULL for JSON `null`), and an object or array stays `JSON`. A NULL document gives no rows; a document that is not an object or array is an error.

```sql
SELECT key, value FROM JSON_EACH('{"a": 1, "b": [2, 3]}');           -- a | 1   and   b | [2,3]
SELECT D.ID, t.value FROM Docs D, LATERAL JSON_EACH(D.Body -> 'tags') AS t;    -- one row per tag
```

## SPLIT_STRING

`SPLIT_STRING(text, separator)` returns the pieces of `text` between occurrences of `separator`, with columns `value` (text) and `ordinal` (1, 2, 3, ...). An empty separator splits into characters; a NULL argument gives no rows.

A table function alone in a select list (`SELECT generate_series(1, 3)`, `SELECT jsonb_path_query(doc, path)`) is the same as selecting from it; with other items in the list, or a `FROM`, it is not available there (put it in `FROM`).

```sql
SELECT value, ordinal FROM SPLIT_STRING('red,green,blue', ',');

-- the tags of each article, one per row
SELECT A.Title, T.value AS tag
FROM Article A CROSS JOIN LATERAL SPLIT_STRING(A.Tags, ';') AS T;
```
