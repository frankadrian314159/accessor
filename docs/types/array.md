# ARRAY

A column or value can hold a list of values: `INTEGER[]`, `TEXT[]`, `DATETIME[]` and so on (or `type ARRAY`, or plain `ARRAY` for elements of any type). An array holds NULLs, and an array of arrays is a further dimension (`{{1,2},{3,4}}`). Arrays are stored with the row, compared element by element (a shorter array that starts a longer one sorts first), can be grouped, sorted and indexed, and are shown in PostgreSQL's text form:

```
{1,2,3}        {"a b",NULL,c}        {{1,2},{3,4}}
```

Arrays are the same in the `asql` and `postgresql` dialects. MySQL has no array type; with `--mysql` the arrays are JSON arrays, see [For MySQL](#for-mysql) below.

```sql
CREATE TABLE post (id INTEGER PRIMARY KEY, tags TEXT[]);
INSERT INTO post VALUES (1, ARRAY['sql', 'rust']), (2, '{rust,db}');
SELECT id FROM post WHERE 'rust' = ANY (tags);
SELECT id FROM post WHERE tags @> ARRAY['db'];
SELECT tags[1], array_length(tags, 1) FROM post;
SELECT t FROM post, unnest(tags) AS u(t);
```

## Writing arrays

| Form | Meaning |
| --- | --- |
| `ARRAY[1, 2, 3]` | the constructor; nested `ARRAY[[1,2],[3,4]]` is two-dimensional |
| `'{1,2,3}'::integer[]`, `CAST('{1,2}' AS INTEGER[])` | from the text form; the elements are converted to the element type |
| `ARRAY(SELECT x FROM t ORDER BY x)` | the rows of a query with one column |
| `a[i]` | element `i`, counting from 1; NULL outside the array |
| `a[lo:hi]`, `a[:hi]`, `a[lo:]` | a slice, ends included |
| `a \|\| b`, `a \|\| x`, `x \|\| a` | join two arrays, or add an element at either end |
| `x = ANY (a)`, `x < ALL (a)` | compare `x` with each element (`SOME` is `ANY`); NULL elements make an unclear answer NULL |
| `a @> b`, `a <@ b`, `a && b` | `a` contains all of `b`; `a` is inside `b`; they share an element (NULL elements never match). In `asql`, `&&` is not read (it is a slip for `&`, the text join), so write `array_overlaps(a, b)`; `postgresql` has both |

In `asql` a `[` that follows a name or `)` is a subscript or a type suffix (`tags[1]`, `integer[]`), and inside `ARRAY[ ]` it is always one; elsewhere `[name]` is still an Access identifier. A column whose name needs brackets takes a subscript after a space-free name or a double-quoted one: `"my col"[1]`.

## Functions

| Function | Result |
| --- | --- |
| `array_length(a, dim)`, `array_upper(a, dim)` | the length along dimension `dim` (NULL for an empty array) |
| `array_lower(a, dim)` | 1 (NULL for an empty array) |
| `cardinality(a)` | the number of elements in all dimensions |
| `array_ndims(a)`, `array_dims(a)` | the number of dimensions, and `[1:3][1:2]` |
| `array_append(a, x)`, `array_prepend(x, a)`, `array_cat(a, b)` | add an element at the end or start, or join two arrays (a NULL array counts as empty) |
| `array_remove(a, x)`, `array_replace(a, from, to)` | take out, or change, every element equal to `x` (NULL matches NULL) |
| `array_position(a, x [, start])`, `array_positions(a, x)` | the first position of `x` (NULL if none), or the array of all positions |
| `array_to_string(a, sep [, null_text])` | the elements joined with `sep`; NULL elements are left out unless `null_text` is given |
| `string_to_array(s, sep [, null_text])` | the pieces of `s`; an empty `s` gives `{}`, a NULL `sep` splits into characters |
| `array_fill(x, ARRAY[n, ...])` | an array of the given dimensions filled with `x` |
| `trim_array(a, n)` | `a` without its last `n` elements |
| `array_agg(x [ORDER BY ...])` | an aggregate: all the values, NULLs included, as an array (NULL if there are no rows); `DISTINCT` works |
| `unnest(a [, b ...])` | a [table function](../functions/table-functions.md#unnest): one row per element (all dimensions), several arrays side by side |

An array argument may also be given as text in the `{...}` form, or as a JSON array.

## For MySQL

With `--mysql` there is no `ARRAY` syntax. MySQL keeps lists in `JSON` columns, and these work on them (see also [json](../functions/json.md)): `JSON_ARRAY(...)`, `JSON_ARRAYAGG(x)`, `JSON_LENGTH(doc)`, `JSON_EXTRACT(doc, '$[1]')` and `doc->'$[1]'`, `JSON_CONTAINS(doc, val)`, and

| Function | Result |
| --- | --- |
| `JSON_ARRAY_APPEND(doc, path, val [, path, val ...])` | adds `val` to the array at `path` (a value that is not an array becomes a two-element array) |
| `JSON_ARRAY_INSERT(doc, path, val ...)` | inserts at the position the path ends with (`'$[1]'`); past the end it appends |
| `JSON_INSERT(doc, path, val ...)`, `JSON_REPLACE(doc, path, val ...)` | set a path only if it is missing, or only if it exists |
| `JSON_OVERLAPS(a, b)` | 1 if two arrays share an element, two objects a key and value, or two scalars are equal |
| `x MEMBER OF (json_array)` | 1 if `x` is an element |

The operators `&&`, `@>` and `<@` (and `ARRAY[ ]`) are not read in `--mysql` (where `&&` is AND), and `--mysql` has no `ARRAY[ ]` values to apply the array functions to.
