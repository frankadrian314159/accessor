# JSON

A `JSON` column holds a JSON document: an object, an array, a string, a number, `true`, `false` or `null`. `JSONB` is accepted as another name for the same type.

```sql
CREATE TABLE Docs (ID COUNTER PRIMARY KEY, Body JSON);
INSERT INTO Docs (Body) VALUES ('{"name": "Ada", "tags": ["math", "code"], "age": 36}');

SELECT Body ->> 'name', JSON_VALUE(Body, '$.age') + 1 FROM Docs;
```

## What is stored

The text is checked when it is stored: text that is not valid JSON is refused. It is then kept in a *canonical* form: no extra spaces, object members in sorted order, string escapes in a standard form. The digits of numbers are kept exactly as written (`1.50` stays `1.50`, and a number too big for any machine type loses nothing). So:

* two documents that mean the same thing are *equal* and store the same text, however they were written: `'{"b":1, "a":2}'` equals `'{"a":2,"b":1}'`;
* the order of members is not kept (arrays keep their order);
* a document shows as its canonical text.

## Comparing and sorting

`=`, `<>`, `DISTINCT`, `GROUP BY` and unique indexes work on the whole document. Comparing a `JSON` value with text parses the text first, so `Body = '{"a": 1}'` is true when the document means `{"a":1}`. The order of documents under `<` and `ORDER BY` is that of their canonical text; it is stable but has no meaning. To compare or sort by something *inside* a document, extract it: `ORDER BY JSON_VALUE(Body, '$.age')`.

## Conversions

| From | To `JSON` |
| --- | --- |
| text | parsed; an error if it is not JSON (use `TO_JSON('text')` to make a JSON *string*) |
| number, yes/no | the JSON number or `true`/`false` |
| `JSON` | text: the canonical text |

Other types are not converted to `JSON` implicitly; `TO_JSON(x)` makes any value into JSON, dates and times becoming strings.

## Indexes

An index on a `JSON` column indexes whole documents, which helps only for equality of whole documents. To search *inside* documents quickly, keep the part you search on in an ordinary, typed column as well (filled when you store the document) and index that. A [calculated](calculated.md) column can show the part but is not used for index lookups. See [the optimizer notes](../optimizer/tuning.md#json-columns).

## Functions

See [functions/json.md](../functions/json.md): paths, `->` and `->>`, building, changing and `JSON_EACH`.
