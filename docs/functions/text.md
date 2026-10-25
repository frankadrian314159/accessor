# Text functions

Positions in text start at 1. Characters are Unicode characters, not bytes, except in `OCTET_LENGTH`. A number or date given where text is expected is converted to its text form first.

## Concat

`Concat(value, value, ...)` joins its arguments. **NULL arguments are skipped.** At least one argument is needed.

```sql
SELECT Concat('a', NULL, 'b', 3);     -- 'ab3'
```

The `&` and `||` operators do the same for two operands.

## Len

`Len(text)` — also `LENGTH`, `CHAR_LENGTH`, `CHARACTER_LENGTH` — returns the number of characters.

```sql
SELECT Len('héllo');                  -- 5
```

## OCTET_LENGTH

`OCTET_LENGTH(text)` returns the number of bytes in the UTF-8 form of the text.

```sql
SELECT OCTET_LENGTH('héllo');         -- 6
```

## UCase and LCase

`UCase(text)` (`UPPER`) returns the text in capitals; `LCase(text)` (`LOWER`) in lower case.

## Left and Right

`Left(text, n)` returns the first `n` characters; `Right(text, n)` the last `n`. A negative `n` is treated as 0; an `n` longer than the text gives the whole text.

```sql
SELECT Left('database', 4), Right('database', 4);    -- 'data', 'base'
```

## Mid

`Mid(text, start [, length])` — also `SUBSTR` and `SUBSTRING` — returns the part of `text` beginning at position `start`. Without `length` it runs to the end. A `start` below 1 is treated as 1.

The standard form `SUBSTRING(text FROM start [FOR length])` is the same function.

```sql
SELECT Mid('database', 5);            -- 'base'
SELECT Mid('database', 2, 3);         -- 'ata'
SELECT SUBSTRING('database' FROM 5 FOR 2);   -- 'ba'
```

## InStr

`InStr([start,] text, find)` returns the position of the first occurrence of `find` in `text`, searching from position `start` (default 1), or 0 when it is not there. **The search ignores case.** An empty `find` is found at `start`.

`POSITION(find IN text)` is `InStr(text, find)`.

```sql
SELECT InStr('Database', 'BASE');     -- 5
SELECT InStr(3, 'banana', 'an');      -- 4
SELECT POSITION('x' IN 'abc');        -- 0
```

## Replace

`Replace(text, find, replacement)` replaces every occurrence of `find` with `replacement`. **The match is case-sensitive.** An empty `find` leaves the text unchanged.

```sql
SELECT Replace('a-b-c', '-', '+');    -- 'a+b+c'
```

## Trim, LTrim and RTrim

`Trim(text)` removes spaces and other white space from both ends; `LTrim` from the start only; `RTrim` from the end only.

The standard form removes chosen characters:

```
TRIM( [LEADING | TRAILING | BOTH] [characters] FROM text )
```

`characters` is a text whose every character is to be removed (default: a space); `BOTH` is the default.

```sql
SELECT Trim('  hi  ');                       -- 'hi'
SELECT TRIM(LEADING '0' FROM '000120');      -- '120'
SELECT TRIM(BOTH 'xy' FROM 'xyxhixy');       -- 'hi'
```

## LPad and RPad

`LPAD(text, width [, fill])` and `RPAD(text, width [, fill])` pad the text to `width` characters with `fill` (a space by default; a longer `fill` is repeated and cut). Text already longer than `width` is **cut** to it.

```sql
SELECT LPAD('7', 3, '0'), RPAD('ab', 5, '-'), LPAD('abcdef', 3);      -- 007, ab---, abc
```

## Repeat, Reverse and InitCap

`REPEAT(text, n)` (also `REPLICATE`) repeats the text `n` times (nothing for `n` below 1; more than a hundred million characters is an error). `REVERSE(text)` reverses the characters. `INITCAP(text)` (also `PROPER`) makes the first letter of each word upper-case and the rest lower-case; a word is a run of letters and digits.

## Overlay

`OVERLAY(text PLACING replacement FROM start [FOR length])` replaces `length` characters (default: the length of the replacement) from position `start` (counting from 1) with the replacement. The function form `OVERLAY(text, replacement, start [, length])` is the same.

```sql
SELECT OVERLAY('Txxxxas' PLACING 'hom' FROM 2 FOR 4);     -- Thomas
```

## Translate

`TRANSLATE(text, from, to)` replaces each character of `from` by the character in the same position of `to`; a character of `from` with no counterpart in `to` is removed.

```sql
SELECT TRANSLATE('12345', '143', 'ax');                   -- a2x5
```

## Related

* [Regular expressions](regex.md): `REGEXP_LIKE`, `REGEXP_REPLACE`, `REGEXP_SUBSTR`, `SIMILAR TO`.

* [`Format`](format.md) formats numbers, dates and text with a pattern.
* [`CStr`](conversion.md#conversion-functions) and `CAST(x AS TEXT)` convert values to text.
* `LIKE` and `ESCAPE` for pattern matching: [expressions](../sql/expressions.md#like).
