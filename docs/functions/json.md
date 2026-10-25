# JSON functions

These work on [JSON](../types/json.md) values. Wherever a function takes a document you may give a `JSON` value or text that is JSON; text that is not valid JSON is an error (except in `JSON_VALID`).

## Paths

A path says where in a document to look.

| Path | Meaning |
| --- | --- |
| `$` | the whole document |
| `$.name` | the member `name` of an object |
| `$.a.b` | a member of a member |
| `$.items[0]` | the first element of an array (positions count from 0) |
| `$["odd key"]` | a member whose name has spaces or punctuation |

A path that leads nowhere gives NULL; it is not an error. A path that is not written in this form is an error.

## Reading

| Function | Result |
| --- | --- |
| `JSON_VALID(text)` | true if the text is a JSON document |
| `JSON_EXTRACT(doc, path)` (also `JSON_QUERY`) | the part of the document at the path, as `JSON` (a string keeps its quotes); NULL if absent |
| `JSON_VALUE(doc, path)` | the scalar at the path as an ordinary SQL value: text, a number (`INTEGER` or `DECIMAL`, exactly as written), or a yes/no; NULL if the path leads to an object, an array, a JSON `null`, or nothing |
| `JSON_GET(doc, key)` | the member `key` (text) or element (number), as `JSON`. This is `doc -> key` |
| `JSON_GET_TEXT(doc, key)` | the same as text, without the quotes of a string. This is `doc ->> key` |
| `JSON_LENGTH(doc [, path])` (also `JSON_ARRAY_LENGTH`) | the number of elements of an array or members of an object; 1 for a scalar |
| `JSON_TYPE(doc [, path])` | `object`, `array`, `string`, `number`, `boolean` or `null` |
| `JSON_KEYS(doc [, path])` | the member names of an object, as a `JSON` array |
| `JSON_CONTAINS(doc, candidate)` | true if `doc` contains `candidate`: an object contains an object having some of its members (recursively), an array contains an array whose elements all appear in it, and a scalar must be equal |

```sql
SELECT JSON_VALUE(body, '$.address.city')            FROM Docs;
SELECT body -> 'tags' -> 0, body ->> 'name'           FROM Docs;
SELECT ID FROM Docs WHERE JSON_CONTAINS(body, '{"status": "open"}');
```

`->` and `->>` bind tighter than the arithmetic operators and chain from left to right: `body -> 'a' -> 'b' ->> 'c'`.

## Building

| Function | Result |
| --- | --- |
| `JSON_ARRAY(v, ...)` | an array of the values; a NULL becomes JSON `null` |
| `JSON_OBJECT(key, value, ...)` | an object from pairs; keys are text and may not be NULL |
| `TO_JSON(value)` | any SQL value as JSON: text becomes a string, numbers keep their digits, a yes/no becomes `true`/`false`, dates become strings |
| `JSON(text)` (also `JSON_PARSE`, and `CAST(text AS JSON)`) | the text as a `JSON` value |
| `JSON_ARRAYAGG(expr)` (aggregate; also `JSON_AGG`) | the non-NULL values of a group as a JSON array; NULL for an empty group |

## Changing

These return a new document; they never change a stored one in place (use them in an `UPDATE`).

| Function | Result |
| --- | --- |
| `JSON_SET(doc, path, value)` | the document with the member or element at the path set. An object member is added if missing; an array position one past the end appends; any other missing step is an error |
| `JSON_REMOVE(doc, path)` | the document without the member or element |
| `JSON_MERGE_PATCH(doc, patch)` (also `JSON_MERGE`) | the document with the patch merged in as in RFC 7396: objects merge member by member, a `null` in the patch removes the member, anything else replaces |

```sql
UPDATE Docs SET body = JSON_SET(body, '$.status', 'closed') WHERE ID = 7;
UPDATE Docs SET body = JSON_MERGE_PATCH(body, '{"tags": null, "reviewed": true}');
```

## JSON_EACH

`JSON_EACH(doc)` is a [table function](table-functions.md#json_each): it returns one row for each member of an object or element of an array.

## Notes

* NULL in, NULL out, as for other functions; `JSON_ARRAY`, `JSON_OBJECT` and `TO_JSON` take NULL as a value (JSON `null`).
* The functions are deterministic, so the optimizer folds calls with constant arguments. A call on a column is evaluated for each row read; see [the optimizer notes on JSON](../optimizer/tuning.md#json-columns).
