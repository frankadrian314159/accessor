# ASQL function reference

Function names are not case-sensitive: `Upper`, `UPPER` and `upper` are the same function. Many functions have two or three names, the Access name and the standard one; all are listed.

Functions you define yourself with [`CREATE FUNCTION`](../sql/routines.md) are called the same way.

## NULL handling

Unless a function's page says otherwise, a function returns NULL when any of its arguments is NULL. The exceptions are the functions made to deal with NULLs: `IIf`, `Nz`, `IsNull`, `Coalesce`, `NullIf`, `Concat`, the `&` operator, and the aggregates (which skip NULLs).

## Index

| Page | Functions |
| --- | --- |
| [text.md](text.md) | `Concat`, `InStr`, `InitCap`, `Left`, `Len`, `LCase`, `LPad`, `LTrim`, `Mid`, `OCTET_LENGTH`, `Overlay`, `POSITION`, `Repeat`, `Replace`, `Reverse`, `Right`, `RPad`, `RTrim`, `SUBSTRING`, `Translate`, `Trim`, `UCase` |
| [regex.md](regex.md) | `REGEXP_LIKE`, `REGEXP_REPLACE`, `REGEXP_SUBSTR`, `REGEXP_COUNT`, `SIMILAR TO` |
| [numeric.md](numeric.md) | `Abs`, `Acos`, `Asin`, `Atan`, `Atan2`, `Ceil`, `Cos`, `Cosh`, `Degrees`, `Exp`, `Fix`, `Floor`, `Int`, `Log`, `Log10`, `Mod`, `Pi`, `Power`, `Radians`, `Random`, `Round`, `Sgn`, `Sin`, `Sinh`, `Sqr`, `Tan`, `Tanh`, `Trunc` |
| [datetime.md](datetime.md) | `Date`, `DateAdd`, `DateDiff`, `DatePart`, `DateSerial`, `Day`, `EXTRACT`, `Hour`, `Minute`, `Month`, `Now`, `Second`, `Weekday`, `Year`, `CURRENT_DATE`, `CURRENT_TIMESTAMP` |
| [conversion.md](conversion.md) | `CAST`, `CBig`, `CBool`, `CCur`, `CDate`, `CDbl`, `CDec`, `CInt`, `CRat`, `CSng`, `CStr`, `Denominator`, `Numerator`, `Rational`, `TypeName`, `Val` |
| [conditional.md](conditional.md) | `CASE`, `Coalesce`, `Greatest`, `IIf`, `IsNull`, `Least`, `NullIf`, `Nz` |
| [aggregate.md](aggregate.md) | `Avg`, `Count`, `Every`, `First`, `Grouping`, `Last`, `Max`, `Min`, `StdDev`, `String_Agg`, `Sum`, `Variance`, and relatives |
| [window.md](window.md) | `Cume_Dist`, `Dense_Rank`, `First_Value`, `Lag`, `Last_Value`, `Lead`, `Nth_Value`, `Ntile`, `Percent_Rank`, `Rank`, `Row_Number`, and aggregates used with `OVER` |
| [table-functions.md](table-functions.md) | `GENERATE_SERIES`, `SPLIT_STRING`, `JSON_EACH` (used in `FROM`) |
| [sequence.md](sequence.md) | `NEXTVAL`, `CURRVAL`, `LASTVAL`, `SETVAL` |
| [json.md](json.md) | `JSON_VALUE`, `JSON_EXTRACT`, `JSON_SET`, `JSON_OBJECT`, `JSON_ARRAYAGG`, `->`, `->>`, and the rest |
| [format.md](format.md) | `Format` with its number, date and text patterns |

## Alphabetical list

| Function | Page |
| --- | --- |
| Abs | [numeric](numeric.md#abs) |
| Atan2, Degrees, Pi, Radians | [numeric](numeric.md#trigonometric-and-hyperbolic-functions) |
| InitCap, Proper | [text](text.md#repeat-reverse-and-initcap) |
| LPad, RPad | [text](text.md#lpad-and-rpad) |
| Overlay | [text](text.md#overlay) |
| Random, Rand, Rnd | [numeric](numeric.md#random) |
| REGEXP_COUNT, REGEXP_LIKE, REGEXP_REPLACE, REGEXP_SUBSTR, SIMILAR TO | [regex](regex.md) |
| Repeat, Replicate, Reverse | [text](text.md#repeat-reverse-and-initcap) |
| Translate | [text](text.md#translate) |
| Trunc, Truncate | [numeric](numeric.md#trunc) |
| AGE | [datetime](datetime.md#time-zones-and-intervals) |
| AT TIME ZONE, TimeZone | [datetime](datetime.md#time-zones-and-intervals) |
| CInterval, CTimestampTz | [conversion](conversion.md#conversion-functions) |
| GENERATE_SERIES, SPLIT_STRING, JSON_EACH | [table functions](table-functions.md) |
| JSON_ARRAY, JSON_OBJECT, TO_JSON, JSON_PARSE | [json](json.md#building) |
| JSON_ARRAYAGG, JSON_AGG | [json](json.md#building) |
| JSON_CONTAINS, JSON_EXTRACT, JSON_GET, JSON_GET_TEXT, JSON_KEYS, JSON_LENGTH, JSON_TYPE, JSON_VALID, JSON_VALUE | [json](json.md#reading) |
| JSON_MERGE_PATCH, JSON_REMOVE, JSON_SET | [json](json.md#changing) |
| NEXTVAL, CURRVAL, LASTVAL, SETVAL | [sequence](sequence.md) |
| MakeInterval | [datetime](datetime.md#time-zones-and-intervals) |
| NowTz, SessionTimeZone | [datetime](datetime.md#now-and-date) |
| Acos | [numeric](numeric.md#trigonometric-and-hyperbolic-functions) |
| Asin | [numeric](numeric.md#trigonometric-and-hyperbolic-functions) |
| Atan | [numeric](numeric.md#trigonometric-and-hyperbolic-functions) |
| Avg | [aggregate](aggregate.md#avg) |
| BOOL_AND, BOOL_OR | [aggregate](aggregate.md#every-bool_and-and-bool_or) |
| CAST | [conversion](conversion.md#cast) |
| CBig, CBool, CCur, CDate, CDbl, CDec, CInt, CRat, CSng, CStr | [conversion](conversion.md#conversion-functions) |
| Ceil, Ceiling | [numeric](numeric.md#ceil) |
| CHAR_LENGTH, CHARACTER_LENGTH | [text](text.md#len) |
| Coalesce | [conditional](conditional.md#coalesce) |
| Concat | [text](text.md#concat) |
| Cos, Cosh | [numeric](numeric.md#trigonometric-and-hyperbolic-functions) |
| Count | [aggregate](aggregate.md#count) |
| CUME_DIST | [window](window.md#cume_dist) |
| CURRENT_DATE, CURRENT_TIME, CURRENT_TIMESTAMP | [datetime](datetime.md#now-and-date) |
| Date, Today | [datetime](datetime.md#now-and-date) |
| DateAdd | [datetime](datetime.md#dateadd) |
| DateDiff | [datetime](datetime.md#datediff) |
| DatePart, DATE_PART | [datetime](datetime.md#datepart) |
| DateSerial | [datetime](datetime.md#dateserial) |
| Day | [datetime](datetime.md#year-month-day-hour-minute-second) |
| Denominator | [conversion](conversion.md#rational-numerator-and-denominator) |
| DENSE_RANK | [window](window.md#rank-dense_rank-and-row_number) |
| EVERY | [aggregate](aggregate.md#every-bool_and-and-bool_or) |
| Exp | [numeric](numeric.md#exp-and-log) |
| EXTRACT | [datetime](datetime.md#extract) |
| First | [aggregate](aggregate.md#first-last-and-any_value) |
| FIRST_VALUE | [window](window.md#first_value-last_value-and-nth_value) |
| Fix | [numeric](numeric.md#int-fix-and-floor) |
| Floor | [numeric](numeric.md#int-fix-and-floor) |
| Format | [format](format.md) |
| Greatest | [conditional](conditional.md#greatest-and-least) |
| GROUPING | [aggregate](aggregate.md#grouping) |
| Hour | [datetime](datetime.md#year-month-day-hour-minute-second) |
| IIf | [conditional](conditional.md#iif) |
| InStr | [text](text.md#instr) |
| Int | [numeric](numeric.md#int-fix-and-floor) |
| IsNull | [conditional](conditional.md#isnull) |
| Last | [aggregate](aggregate.md#first-last-and-any_value) |
| LAG, LEAD | [window](window.md#lag-and-lead) |
| LAST_VALUE | [window](window.md#first_value-last_value-and-nth_value) |
| LCase, Lower | [text](text.md#ucase-and-lcase) |
| Least | [conditional](conditional.md#greatest-and-least) |
| Left | [text](text.md#left-and-right) |
| Len, Length | [text](text.md#len) |
| Ln, Log | [numeric](numeric.md#exp-and-log) |
| Log10 | [numeric](numeric.md#exp-and-log) |
| LTrim | [text](text.md#trim-ltrim-and-rtrim) |
| Max, Min | [aggregate](aggregate.md#min-and-max) |
| Mid | [text](text.md#mid) |
| Minute, Month | [datetime](datetime.md#year-month-day-hour-minute-second) |
| Mod | [numeric](numeric.md#mod-and-power) |
| Now | [datetime](datetime.md#now-and-date) |
| NTH_VALUE | [window](window.md#first_value-last_value-and-nth_value) |
| NTILE | [window](window.md#ntile) |
| NullIf | [conditional](conditional.md#nullif) |
| Numerator | [conversion](conversion.md#rational-numerator-and-denominator) |
| Nz | [conditional](conditional.md#nz) |
| OCTET_LENGTH | [text](text.md#octet_length) |
| PERCENT_RANK | [window](window.md#percent_rank-and-cume_dist) |
| POSITION | [text](text.md#instr) |
| Power, Pow | [numeric](numeric.md#mod-and-power) |
| RANK | [window](window.md#rank-dense_rank-and-row_number) |
| Rational | [conversion](conversion.md#rational-numerator-and-denominator) |
| Replace | [text](text.md#replace) |
| Right | [text](text.md#left-and-right) |
| Round | [numeric](numeric.md#round) |
| ROW_NUMBER | [window](window.md#rank-dense_rank-and-row_number) |
| RTrim | [text](text.md#trim-ltrim-and-rtrim) |
| Second | [datetime](datetime.md#year-month-day-hour-minute-second) |
| Sgn, Sign | [numeric](numeric.md#sgn) |
| Sin, Sinh | [numeric](numeric.md#trigonometric-and-hyperbolic-functions) |
| Sqr, Sqrt | [numeric](numeric.md#sqr) |
| STDDEV, STDDEV_POP, STDDEV_SAMP | [aggregate](aggregate.md#variance-and-standard-deviation) |
| STRING_AGG, LISTAGG, GROUP_CONCAT | [aggregate](aggregate.md#string_agg) |
| Substr, Substring | [text](text.md#mid) |
| Sum | [aggregate](aggregate.md#sum) |
| Tan, Tanh | [numeric](numeric.md#trigonometric-and-hyperbolic-functions) |
| Trim | [text](text.md#trim-ltrim-and-rtrim) |
| TypeName | [conversion](conversion.md#typename) |
| UCase, Upper | [text](text.md#ucase-and-lcase) |
| Val | [conversion](conversion.md#val) |
| VAR_POP, VAR_SAMP, VARIANCE | [aggregate](aggregate.md#variance-and-standard-deviation) |
| Weekday | [datetime](datetime.md#weekday) |
| Year | [datetime](datetime.md#year-month-day-hour-minute-second) |
