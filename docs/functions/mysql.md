# MySQL functions

Functions MySQL has that the other pages do not already give. NULL in, NULL out, except where said.

| Function | Gives |
| --- | --- |
| `IF(cond, a, b)` | `a` if `cond` is true, else `b` (the same as `IIf`) |
| `IFNULL(a, b)` | `a`, or `b` if `a` is NULL |
| `HEX(x)` | upper-case hexadecimal of a number, or of the bytes of a text |
| `UNHEX(s)` | the text the hexadecimal digits stand for (NULL if they are not hexadecimal) |
| `ASCII(s)`, `ORD(s)` | the code of the first byte (0 for an empty text) |
| `CHAR(n, ...)` | the text of the code points |
| `BIN(n)`, `OCT(n)` | binary and octal text of a number |
| `SPACE(n)` | `n` spaces |
| `LOCATE(sub, s [, from])` | position (from 1) of `sub` in `s`, ignoring case; 0 if absent |
| `FIELD(x, a, b, ...)` | position of `x` among the others, 0 if absent or `x` is NULL |
| `ELT(n, a, b, ...)` | the `n`-th of the others, NULL if there is none |
| `FIND_IN_SET(s, 'a,b,c')` | position of `s` in the comma-separated list, 0 if absent |
| `SUBSTRING_INDEX(s, delim, n)` | the text before the `n`-th `delim` (after the `n`-th from the end if `n` is negative) |
| `SOUNDEX(s)` | the four-character sound code |
| `MD5(s)`, `SHA2(s, 256)` | hash as hexadecimal text (`SHA2` with other sizes gives NULL) |
| `UUID()` | a random version-4 UUID; a new one each call |
| `STR_TO_DATE(s, fmt)` | the date-time `s` is written as in `fmt` (`%Y %y %m %c %d %e %H %k %h %I %l %i %s %S %p %b %M %a %W %T %j %f %%`); NULL if it does not fit |
| `INSERT(s, pos, len, new)` | `s` with `len` characters from the 1-based `pos` replaced by `new` (`s` itself when `pos` is outside) |
| `BIT_LENGTH(s)` | the length of the text in bits (8 for each byte of UTF-8) |
| `a SOUNDS LIKE b` | `SOUNDEX(a) = SOUNDEX(b)` |
| `TIME_FORMAT(t, fmt)` | a time (or the time of a date and time) by `fmt`; the hours wrap at 24 |
| `WEEK(d [, mode])` | the week of the year, modes 0 to 7 as in MySQL (default 0: Sunday first, 0 to 53) |
| `EXPORT_SET(bits, on, off [, sep [, n]])` | one `on` or `off` text for each of the low `n` (default 64) bits, joined by `sep` (default a comma) |
| `LAST_INSERT_ID()` | the last AutoNumber value given to a row, by this program (a row of several gets the last, not the first) |
| `FOUND_ROWS()` | the number of rows the last query returned (`SQL_CALC_FOUND_ROWS` is not known, so a `LIMIT` counts) |
| `DATE_FORMAT(d, fmt)` | the date as text by `fmt` (the same specifiers, and `%r %w`) |
| `UNIX_TIMESTAMP([d])` | seconds since 1970-01-01 (now, without an argument) |
| `FROM_UNIXTIME(n [, fmt])` | the date-time (or its text by `fmt`) |
| `SEC_TO_TIME(n)` | `HH:MM:SS` text |
| `MAKETIME(h, m, s)` | `HH:MM:SS` text, with a minus sign for negative hours (more than two digits of hours are kept); NULL if `m` or `s` is out of range |
| `ADDTIME(t, t2)`, `SUBTIME(t, t2)` | `t` is a time (`HH:MM:SS`, the hours may pass 24; text) or a date and time; `t2` is a time. A time gives a time, a date and time gives a date and time. A fraction of a second is dropped |
| `TIMEDIFF(a, b)` | `a - b` as a time, when both are times or both are dates and times |
| `TIMESTAMPDIFF(unit, a, b)` | whole units from `a` to `b` (negative if `b` is earlier): `SECOND`, `MINUTE`, `HOUR`, `DAY`, `WEEK`, `MONTH`, `QUARTER`, `YEAR`; a month is whole only when its day and time have come round (January 31 to February 29 is 0 months) |
| `TIMESTAMPADD(unit, n, d)` | `d` plus `n` of the unit, as `DateAdd` (the same units) |
| `CONVERT_TZ(dt, from, to)` | the date and time in another zone: zones as `+05:30` or a region name (`America/New_York`); NULL for a zone it does not know |
| `STRCMP(a, b)` | -1, 0 or 1, ignoring case |
| `BIT_COUNT(n)` | the number of 1 bits of the 64-bit number |
| `LOAD_FILE(path)` | the text of the file, NULL if it cannot be read |
| `TO_BASE64(s)`, `FROM_BASE64(s)` | base64 text of the text, and the text of base64 (NULL if it is not valid base64) |
| `DAYNAME(d)`, `MONTHNAME(d)`, `LAST_DAY(d)`, `TO_DAYS(d)` | the name of the day, the name of the month, the last day of the month, days since year 0 |
| `INET_ATON(s)`, `INET_NTOA(n)`, `IS_IPV4(s)`, `IS_IPV6(s)` | IPv4 address and number, and the checks (1 or 0) |
| `JSON_CONTAINS_PATH(doc, 'one' or 'all', path, ...)` | 1 if the paths exist (in any / all) |
| `CHARSET(x)`, `COLLATION(x)`, `COERCIBILITY(x)` | `utf8mb4`, `utf8mb4_0900_ai_ci`, 2 |
| `SLEEP(n)`, `BENCHMARK(n, expr)` | 0, at once |
| `GET_LOCK(name, t)`, `RELEASE_LOCK(name)`, `IS_FREE_LOCK(name)`, `IS_USED_LOCK(name)` | 1; there is nobody to lock against |
| `CONNECTION_ID()` | 1 |
| `CURRENT_USER()`, `USER()`, `DATABASE()`, `VERSION()` | `root@localhost`, `root@localhost`, `test`, `8.0.40` |
| `STD(x)` | population standard deviation (the aggregate `STDDEV_POP`) |
| `DATE_ADD`, `DATE_SUB`, `ADDDATE`, `SUBDATE` | date arithmetic with `INTERVAL n unit` ([mysql.md](../sql/mysql.md#dates-and-intervals)) |
| `CONVERT(x, type)`, `CONVERT(x USING cs)` | a cast; the text itself |
| `a \| b`, `~a`, `a << b`, `a >> b` | bitwise OR, NOT, and shifts (`&` joins text and `^` is a power, so there is no AND or XOR operator) |
