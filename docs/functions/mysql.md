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
| `DATE_FORMAT(d, fmt)` | the date as text by `fmt` (the same specifiers, and `%r %w`) |
| `UNIX_TIMESTAMP([d])` | seconds since 1970-01-01 (now, without an argument) |
| `FROM_UNIXTIME(n [, fmt])` | the date-time (or its text by `fmt`) |
| `SEC_TO_TIME(n)` | `HH:MM:SS` text |
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
