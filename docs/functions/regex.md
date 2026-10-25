# Regular expressions

Regular expressions (the syntax of the Rust `regex` crate: [documentation](https://docs.rs/regex/latest/regex/#syntax)) match text more precisely than `LIKE`. Like `LIKE` and text comparison in ASQL, **matching ignores case** unless the flags ask otherwise.

## Flags

An optional last argument of text letters:

| Flag | Meaning |
| --- | --- |
| `c` | case-sensitive |
| `i` | ignore case (the default) |
| `n` | `.` also matches a line break |
| `m` | `^` and `$` match at the start and end of each line |

Any other letter is an error. A pattern that is not a valid regular expression is an error. Compiled patterns are remembered, so a pattern used on every row of a big table is compiled once.

## REGEXP_LIKE

`REGEXP_LIKE(text, pattern [, flags])` is true if the pattern matches **anywhere** in the text (use `^` and `$` to anchor it).

```sql
SELECT REGEXP_LIKE('Hello World', 'WORLD$');             -- yes (case ignored)
SELECT REGEXP_LIKE('Hello', 'h', 'c');                   -- no
SELECT Name FROM Customers WHERE REGEXP_LIKE(Phone, '^\+?[0-9 ()-]{7,}$');
```

## REGEXP_REPLACE

`REGEXP_REPLACE(text, pattern, replacement [, flags])` replaces **every** match. In the replacement, `\1` to `\9` (or `$1`, `${1}`, `${name}`) are the groups of the match, and `\\` is a backslash.

```sql
SELECT REGEXP_REPLACE('2024-03-05', '(\d+)-(\d+)-(\d+)', '\3/\2/\1');   -- 05/03/2024
SELECT REGEXP_REPLACE('a1b22c333', '[0-9]+', '#');                         -- a#b#c#
```

## REGEXP_SUBSTR

`REGEXP_SUBSTR(text, pattern [, group [, flags]])` is the first match (or, with `group`, the text of that group of it; 0 is the whole match), and NULL if there is none.

```sql
SELECT REGEXP_SUBSTR('order 4711 shipped', '[0-9]+');         -- 4711
SELECT REGEXP_SUBSTR('key=value', '(\w+)=(\w+)', 2);         -- value
```

## REGEXP_COUNT

`REGEXP_COUNT(text, pattern [, flags])` is the number of non-overlapping matches.

## SIMILAR TO

`text [NOT] SIMILAR TO pattern` is the standard's middle ground between `LIKE` and a regular expression. The pattern must match the **whole** text. `%` and `_` mean what they do in `LIKE`; `|` is alternation, `*`, `+`, `?` and `{m,n}` repeat, `( )` group and `[ ]` choose a character; and `.` is an ordinary dot. Case is ignored.

```sql
SELECT 'abc' SIMILAR TO 'a%';              -- yes
SELECT 'xyz' SIMILAR TO '(x|y)+z';         -- yes
SELECT 'a.c' SIMILAR TO 'a.c';             -- yes; 'abc' SIMILAR TO 'a.c' is no
```

## The optimizer

A regular expression is a condition on the *contents* of a column, so no index can answer it (neither can `LIKE`). It is applied to rows as they are read. Put another condition that an index can answer in the same `WHERE` to narrow the rows first.
