# PostgreSQL functions

Functions that PostgreSQL has and Access does not. They follow PostgreSQL's names and patterns, and return NULL when an argument is NULL unless noted.

## Formatting with patterns

| Function | Result |
| --- | --- |
| `to_char(value, pattern)` | a number, date or timestamp as text |
| `to_date(text, pattern)` | a `DATETIME` (midnight) |
| `to_timestamp(text, pattern)` | a `TIMESTAMPTZ`, the text read in the session [time zone](../sql/session.md) |
| `to_timestamp(seconds)` | the `TIMESTAMPTZ` that many seconds after 1970-01-01 00:00:00 UTC |
| `to_number(text, pattern)` | a `DECIMAL`, reading digits, the decimal point and a minus sign |

**Date patterns** (case of a name pattern gives the case of the result: `MONTH`, `Month`, `month`): `YYYY` `YYY` `YY` `Y`, `MM`, `DD`, `DDD` (day of the year), `D` (day of the week, Sunday is 1), `HH24`, `HH12`/`HH`, `MI`, `SS`, `SSSS`, `AM`/`PM`/`A.M.`/`P.M.`, `MONTH`/`MON`, `DAY`/`DY`, `Q`, `WW`, `W`, `CC`, `J`, `RM`, `TZ`, and `"literal text"`. `FM` before a pattern suppresses padding (`FMMM` is 3, not 03; names are not padded to nine letters). Anything else is copied. `MS` and `US` print zeros: times have whole seconds. In `to_date` and `to_timestamp` a separator in the pattern matches any one separator in the text, and a missing year is 1, month 1, day 1.

```sql
SELECT to_char(TIMESTAMP '2024-03-01 15:04:05', 'Dy Mon DD, HH12:MI PM');   -- Fri Mar 01, 03:04 PM
SELECT to_date('March 5, 2024', 'Month DD, YYYY');                          -- 2024-03-05
```

**Number patterns**: `9` (a digit, or a blank before the first), `0` (a digit, zero filled), `.` or `D` (the decimal point), `,` or `G` (a group separator, blank before the first digit), `V` (multiply by ten for each digit after it), `S` (a sign), `MI` (trailing minus), `PR` (negative in angle brackets), `L` (a `$`), `FM` (no padding). The result has a place for a sign in front: `to_char(5, '999')` is `'   5'`. A number too wide for the pattern is shown as `#` signs.

## Building and cutting dates

| Function | Result |
| --- | --- |
| `make_date(year, month, day)` | a date |
| `make_timestamp(year, month, day, hour, minute, second)` | a `DATETIME` |
| `make_timestamptz(year, month, day, hour, minute, second [, zone])` | a `TIMESTAMPTZ`, read in the session zone or `zone` |
| `split_part(text, delimiter, n)` | the n-th piece (a negative n counts from the end; '' if there is none) |
| `date_bin(stride, source, origin)` | the start of the `stride`-wide bin, counted from `origin`, that contains `source` (the stride may have days and less, not months) |
| `jsonb_set(target, '{a,b}', value [, create_if_missing])`, `jsonb_insert(target, '{a,0}', value [, insert_after])` | the document with the value put at the path |
| `json_strip_nulls(doc [, strip_in_arrays])`, `jsonb_strip_nulls` | the document without the object members that are null |
| `date_trunc(unit, value [, zone])` | the start of the `microseconds`, `milliseconds`, `second`, `minute`, `hour`, `day`, `week` (Monday), `month`, `quarter`, `year`, `decade`, `century` or `millennium` that contains `value` |
| `isfinite(value)` | always true: there are no infinite dates |

## Types

| Function | Result |
| --- | --- |
| `pg_typeof(x)` | the type's PostgreSQL name as text: `integer`, `numeric`, `double precision`, `real`, `text`, `boolean`, `money`, ...; `unknown` for NULL |
| `pg_input_is_valid(text, type)` | whether the text can be read as that type (`'true', 'bool'` is true; `'maybe', 'bool'` is false) |
| `scale(x)` | the number of digits after the decimal point (numbers are stored without trailing zeros, so `scale(1.50)` is 1) |
| `min_scale(x)`, `trim_scale(x)` | the same; the number without trailing zeros |
| `float8(x)`, `int4(x)`, `name(x)`, `text(x)`, ... | a type name used as a function is a cast to the type, also with `x::type` and `CAST` |

## Text

`concat_ws(separator, a, b, ...)` joins with the separator and skips NULLs (a NULL separator gives NULL). `regexp_instr(text, pattern [, start [, n [, end [, flags]]]])` is the position (from 1) of the `n`th match, or of its end when `end` is 1, and 0 when there is none; the pattern syntax is that of [regex.md](regex.md).

`format(pattern, args...)` fills a pattern: `%s` is the argument as text (NULL is empty), `%I` quotes it as an identifier when it needs quoting (NULL is an error), `%L` as a literal (`'it''s'`, NULL is `NULL`), `%%` is a percent sign; `%2$s` takes the second argument, and a width pads (`%10s` right-aligned, `%-10s` left-aligned). A call is read as PostgreSQL's `format` when the first argument is a text containing one of those, or when there are three or more arguments; otherwise it is Access's `Format(value, pattern)`.

`encode(text, 'hex' | 'base64' | 'escape')` and `decode(text, format)` convert between a text and its encoded form (the bytes are read as UTF-8: there is no `bytea` type). `left(s, -n)` and `right(s, -n)` leave off `n` characters from the other end.

## Mathematics

`width_bucket(x, low, high, count)` is the number of the equal-width bucket `x` falls in (0 below the range, `count + 1` above). `pg_relation_size`, `pg_total_relation_size`, `pg_table_size` and `pg_database_size` are 0; `to_regclass`, `to_regtype`, `to_regproc`, `to_regrole` and `to_regnamespace` return their argument.

`asinh(x)`, `acosh(x)`, `atanh(x)`; the arguments of the last two must be in range.

## Arrays

`array_length`, `array_append`, `array_cat`, `array_position`, `array_to_string`, `string_to_array`, `array_agg`, `unnest`, `cardinality` and the rest work on the `ARRAY` type: see [../types/array.md](../types/array.md).

## Full-text search

A `tsvector` and a `tsquery` are kept as text in the form PostgreSQL prints them (`'cat':3 'fat':2`, `'fat' & 'cat'`), so a column of either type is a text column, and `'a fat cat'::tsvector` and `'fat & cat'::tsquery` read and normalise their text.

| Function | What it gives |
| --- | --- |
| `to_tsvector([config,] text)` | the words of the text, lower-cased, with their positions; the `english` configuration (the default) drops stop words and stems with the Snowball English stemmer, any other configuration (`simple`) keeps the words as they are |
| `to_tsquery([config,] text)` | the query `'fat & rats'` with its words stemmed and its stop words dropped; `&`, `\|`, `!`, `<->`, `<n>`, `( )`, `word:*` and `word:AB` are read |
| `plainto_tsquery`, `phraseto_tsquery`, `websearch_to_tsquery` | a query from plain text (ANDed), a phrase (`<->`, or `<n>` over a stop word), or a search box (`"a phrase"`, `or`, `-word`) |
| `vector @@ query` | true when the query holds; phrases use the positions. Plain text on either side is read as a document or a query. (`jsonb @@ path` is still the JSON path match.) |
| `setweight(vector, 'A'..'D' [, words])`, `strip(vector)`, `ts_delete`, `ts_filter`, `tsvector_to_array`, `array_to_tsvector`, `tsvector_concat`, `numnode`, `querytree` | the vector or query changed or measured |
| `ts_rank(vector, query [, norm])`, `ts_rank_cd(...)` | PostgreSQL's rank formulas (weights 0.1, 0.2, 0.4, 1.0); `ts_rank_cd` gives the same numbers as `ts_rank`; the array of weights is not accepted |
| `ts_headline([config,] text, query [, options])` | the text with the words of the query between `<b>` and `</b>` (`StartSel`, `StopSel`, `MaxWords`, `MinWords`, `FragmentDelimiter`, `HighlightAll`) |
| `json_to_tsvector`, `jsonb_to_tsvector` | the strings (and, by the filter, numbers, booleans, keys) of a JSON document |

The parser sees a word as letters and digits (`3.14` is one word); e-mail addresses, URLs and hyphenated words are not told apart. `CREATE INDEX ... USING gin (to_tsvector('english', col))` is accepted and builds nothing: an index on an expression is read and ignored.

## For a database with one user

`has_table_privilege`, `has_schema_privilege`, `has_column_privilege`, `has_function_privilege`, `has_database_privilege`, `has_sequence_privilege`, `has_type_privilege`, `has_language_privilege`, `has_tablespace_privilege`, `has_server_privilege`, `has_foreign_data_wrapper_privilege`, `has_largeobject_privilege` and `has_parameter_privilege` are **always true** (NULL if an argument is NULL): they do not look at the roles of [roles-and-privileges.md](../sql/roles-and-privileges.md). `pg_sleep(seconds)` waits that long, at most a second. `pg_advisory_lock`, `pg_advisory_unlock` and `pg_stat_force_next_flush` do nothing and return NULL. `pg_get_viewdef('name')` is the text of a saved query (the name must be written in quotes in the statement).

## Not available

The text search objects (`CREATE TEXT SEARCH CONFIGURATION`, dictionaries, parsers: read and ignored), `ts_stat`, `tsvector || tsvector` (the two texts are joined; use `tsvector_concat`), and the exact byte layout of the XML Schema the `*_to_xmlschema` functions write (see below). XML values are text here, so a column of type `xml` is not told from `text`.

## More built-in functions

`STRPOS`, `TO_HEX`, `TO_BIN`, `TO_OCT`, `BTRIM`, `CHR`, `STARTS_WITH`, `QUOTE_LITERAL`, `QUOTE_NULLABLE`, `NORMALIZE` (the text as it is), `CASEFOLD`, `UNISTR`, `NUM_NULLS`, `NUM_NONNULLS`, `REGEXP_MATCH`, `REGEXP_SPLIT_TO_ARRAY`, `FACTORIAL`, `LGAMMA`, `PG_SIZE_PRETTY`, `PG_SIZE_BYTES`, `JSON[B]_BUILD_OBJECT`, `JSON[B]_BUILD_ARRAY`, `JSON[B]_EXTRACT_PATH[_TEXT]`, `JSONB_EXISTS[_ANY|_ALL]`, `JSONB_PRETTY`, `FIPSHASH` (the first 32 hex digits of the SHA-256 of the text), `LO_CREAT`, `JSONB_CONTAINS`, `JSONB_CONTAINED`, `JSONB_DELETE`, `JSONB_DELETE_PATH`, `JSONB_OBJECT`, `JSON_SCALAR`, `JSON_SERIALIZE`, `JSON_EXISTS(doc, path)`, `ARRAY_TO_JSON`, `ARRAY_REVERSE`, `ARRAY_SORT(array [, descending [, nulls_first]])`, `ARRAY_SHUFFLE`, `ARRAY_SAMPLE` and `UUID_EXTRACT_VERSION` are built in. Regular-expression functions are case-sensitive unless the flags have `i`. 

| Function | Result |
| --- | --- |
| `current_setting(name [, missing_ok])` | the value of a [run-time parameter](../sql/session.md) as text, including made-up ones such as `my.flag`; the name must be written in quotes in the statement. An unknown name is an error unless `missing_ok` is true, which gives NULL |
| `row_to_json(row)`, `to_json(row)`, `to_jsonb(row)` | a row as an object of its columns: a table alias (`row_to_json(t)`), `ROW(1, 'x')` (keys `f1`, `f2`) or a value of a composite type |
| `enum_range(NULL::mood)`, `enum_range(a, b)`, `enum_first(NULL::mood)`, `enum_last(...)` | the labels of an enum type in order (as an array), the labels between two of them, the first, the last. The type must be written as a cast (`NULL::mood`, `'ok'::mood`) or the value must be a label of only one enum |
| `jsonb_populate_record(NULL::type, doc)`, `json_populate_record`, `FROM jsonb_populate_recordset(NULL::type, array)` | a composite type filled from the keys of a JSON object (missing keys are NULL, or the field of the first argument if it is not NULL). Only composite types made with `CREATE TYPE ... AS (...)` are known, not a table's row type |
| `justify_hours(i)`, `justify_days(i)`, `justify_interval(i)` | an interval with 24 hours turned into days, 30 days into months, and the signs of its parts brought into line |
| `uuidv7([shift])`, `uuidv4()`, `gen_random_uuid()`, `uuid_extract_timestamp(uuid)` | a version 7 or 4 UUID (the shift moves the time of a v7 one), and the time of a version 1 or 7 UUID (whole seconds; NULL for other versions) |
| `hashtext`, `hashtextextended(text, seed)`, `hashint2`/`hashint4`/`hashint8` (and `...extended(v, seed)`), `hashchar`, `hashname`, `hashbpchar` | PostgreSQL's own hash numbers (Jenkins' lookup3), which are also what choose a hash partition |
| `satisfies_hash_partition(table, modulus, remainder, values...)` | whether a row with those key values belongs in that hash partition |
| `to_regcollation(name)` | the name if it is one of `default`, `C`, `POSIX`, `ucs_basic`, `en_US` or the ICU names `und-x-icu`, `en-x-icu`, else NULL |
| `lo_create(oid)`, `lo_from_bytea(oid, bytes)`, `lo_get(oid [, offset, length])`, `lo_put(oid, offset, bytes)`, `lo_unlink(oid)` | large objects, kept in memory for the session (an oid of 0 takes the next free number, from 16384). They are not part of the database file and are not undone by `ROLLBACK` |
| `lo_open(oid, mode)`, `loread(fd, n)`, `lowrite(fd, bytes)`, `lo_lseek(fd, offset, whence)`, `lo_tell(fd)`, `lo_truncate(fd, len)`, `lo_close(fd)` | read and write a large object through a descriptor (`mode` 262144 reads, 131072 writes, 393216 both; `whence` 0, 1 or 2 for start, current position or end). Descriptors, like the objects, last for the session |
| `lo_import(path [, oid])`, `lo_export(oid, path)` | a large object made from a file of the machine the engine runs on, and written to one |
| `convert_from(bytes, encoding)`, `convert_to(text, encoding)` | bytes read as UTF-8 text, and text as its UTF-8 bytes (the encoding is not looked at) |
| `set_config(name, value, is_local)` | sets a [run-time parameter](../sql/session.md) (as `SET name = value`) and gives the new value. The name and value must be written as constants; the setting takes effect for the statement that follows (a `current_setting` in the same statement sees it), and `is_local` is not honoured: the setting lasts for the session |
| `query_to_xml(query, nulls, tableforest, targetns)`, `table_to_xml(table, nulls, tableforest, targetns)` | the rows of a query (a constant text) or table as XML text, in PostgreSQL's layout: `<table>` with a `<row>` element per row (or, with `tableforest`, one element per row), each column an element with the name escaped as PostgreSQL does (`my col` is `my_x0020_col`). A NULL is left out, or `xsi:nil="true"` with `nulls`. The table must be a constant name |
| `cursor_to_xml(cursor, count, nulls, tableforest, targetns)` | the next `count` rows of a [cursor](../sql/queries-and-triggers.md) as `query_to_xml` writes them; the cursor is not moved by it |
| `xmlelement(NAME n [, xmlattributes(v [AS a], ...)] [, content ...])`, `xmlforest(v [AS n], ...)`, `xmlconcat(...)`, `xmlcomment(text)`, `xmlpi(NAME n [, content])`, `xmlroot(xml, VERSION v \| NO VALUE [, STANDALONE YES \| NO \| NO VALUE])`, `xmlparse(DOCUMENT \| CONTENT text)`, `xmlserialize(DOCUMENT \| CONTENT x AS type [[NO] INDENT])`, `xmlexists(path PASSING [BY REF] xml)`, `xml_is_well_formed[_document \| _content](text)`, and the aggregate `xmlagg(x [ORDER BY ...])` | build and check XML text. Text content and attribute values are escaped; content that is itself well-formed markup (a nested `xmlelement`, an `xml` value) is kept as it is, which also means a text value that looks like markup is not escaped. A NULL attribute is left out; `xmlforest` leaves out NULL columns. `xmlserialize ... INDENT` puts each element and text on a line of its own |
| `schema_to_xml(schema, nulls, tableforest, targetns)`, `database_to_xml(nulls, tableforest, targetns)`, `table_to_xmlschema`, `query_to_xmlschema`, `cursor_to_xmlschema`, `schema_to_xmlschema`, `database_to_xmlschema`, and the `..._to_xml_and_xmlschema` forms | every table (of the `public` schema) as XML inside a root element named for the schema or database, and the XML Schema of a table, query, cursor, schema or database; the `_and_xmlschema` forms put the schema first inside the data, with `xsi:noNamespaceSchemaLocation="#"`. The column types are this engine's, so every whole number is `BIGINT` and the layout of the schema follows PostgreSQL's only roughly |
| `XMLTABLE(rowpath PASSING doc COLUMNS ...)` | in `FROM`: one row per element the row path selects (see [table functions](table-functions.md)) |
| `xpath(path, xml)`, `xpath_exists(path, xml)` | the nodes the path selects as an array of XML text, or whether it selects any. The XPath is the small subset of [EXTRACTVALUE](mysql.md): absolute and relative paths, `//`, `*`, `@attribute`, `text()` and the common predicates; no namespaces |
| `ts_lexize(dictionary, word)` | the lexemes a dictionary makes of a word: `english_stem` (a stop word gives `{}`) or `simple` |
| `pg_restore_relation_stats(...)`, `pg_restore_attribute_stats(...)` | check that the name/value pairs are paired and `relname` is given, and return true; the statistics themselves are not kept. `pg_clear_relation_stats` and `pg_clear_attribute_stats` return NULL |

`GROUP BY` queries can use [the two-argument statistics](aggregate.md#two-argument-statistics-postgresql) (`CORR`, `COVAR_*`, `REGR_*`).
