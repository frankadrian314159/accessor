# Format

```sql
Format(value [, pattern])
```

Turns a number, date, yes/no value or text into text, following a pattern in the style of Access. Without a pattern the value is converted the ordinary way. A NULL value gives NULL.

The pattern is a text; it is case-insensitive in its date and named parts. Which kind of pattern applies depends on the value: a number, a date, or text. Text that looks like a number is formatted as a number unless the pattern is a text pattern (contains `>`, `<`, `@` or `&`).

Numbers are formatted from their exact decimal value, so `Format(0.125, '0.00')` is `0.12` (a half goes to the even digit) whatever the type of the number, and a `CURRENCY` or `BIGNUM` is never passed through a float.

## Named formats

| Name | Numbers | Dates |
| --- | --- | --- |
| `General Number` | the number as is | |
| `Currency` | `$#,##0.00;($#,##0.00)` | |
| `Fixed` | `0.00` | |
| `Standard` | `#,##0.00` | |
| `Percent` | `0.00%` | |
| `Scientific` | `0.00E+00` | |
| `Yes/No`, `True/False`, `On/Off` | the second word for zero, the first otherwise | |
| `General Date` | | `2024-03-05` or `2024-03-05 14:30:00` |
| `Long Date` | | `Tuesday, March 5, 2024` |
| `Medium Date` | | `05-Mar-24` |
| `Short Date` | | `3/5/2024` |
| `Long Time` | | `2:30:00 PM` |
| `Medium Time` | | `02:30 PM` |
| `Short Time` | | `14:30` |

`Yes/No`, `True/False` and `On/Off` also work on yes/no values.

```sql
SELECT Format(1234.5, 'Standard');        -- 1,234.50
SELECT Format(0.256, 'Percent');          -- 25.60%
SELECT Format(-3.5, 'Currency');          -- ($3.50)
```

## Numbers

A pattern is built from these characters:

| Character | Meaning |
| --- | --- |
| `0` | a digit place that always shows a digit (a zero if there is none) |
| `#` | a digit place that shows a digit only if there is one |
| `.` | where the decimal point goes |
| `,` | between two digit places: put thousands separators in the whole part |
| `%` | multiply by 100 and show a percent sign |
| `E+` `E-` `e+` `e-` followed by `0`s | scientific notation; `E+` always shows the exponent's sign, `E-` only when negative |
| `"text"` or `\c` | text or one character shown as written |
| any other character | shown as written (`$`, `-`, `(`, `)`, space, `:`, `/` and so on) |

The number is rounded to the places the pattern has after the point. Digits beyond the places before the point are not lost: they are all shown.

```sql
SELECT Format(1234567.891, '#,##0.00');   -- 1,234,567.89
SELECT Format(5, '000');                  -- 005
SELECT Format(0.5, '#.##');               -- .5
SELECT Format(0.5, '0.##');               -- 0.5
SELECT Format(1234.5, '0.00E+00');        -- 1.23E+03
SELECT Format(12, '"No. "0');             -- No. 12
```

### Sections

A pattern may have up to three sections separated by semicolons: **positive; negative; zero**. A number uses the section for its sign. In a negative section the sign is not added: write it, or parentheses, yourself. With one section only, negative numbers get a minus sign in front.

```sql
SELECT Format(-7, '0;(0)');               -- (7)
SELECT Format(0, '0;(0);"zero"');         -- zero
SELECT Format(-7, '0.00');                -- -7.00
```

A section without digit places is just text.

## Dates

| Letters | Result |
| --- | --- |
| `yyyy` | four-digit year |
| `yy` | two-digit year |
| `y` | day of the year, 1–366 |
| `q` | quarter, 1–4 |
| `mmmm` | month name: January |
| `mmm` | short month name: Jan |
| `mm` | month number with two digits |
| `m` | month number |
| `dddd` | weekday name: Tuesday |
| `ddd` | short weekday name: Tue |
| `dd` | day of the month with two digits |
| `d` | day of the month |
| `ddddd` | short date, as `m/d/yyyy` |
| `dddddd` | long date |
| `w` | weekday number, Sunday = 1 |
| `ww` | week of the year |
| `hh`, `h` | hour, with or without a leading zero |
| `nn`, `n` | minute, with or without a leading zero |
| `ss`, `s` | second, with or without a leading zero |
| `AM/PM`, `am/pm`, `A/P`, `a/p` | the marker; its presence switches the hours to 12-hour |
| `"text"` or `\c` | text shown as written |
| other characters | shown as written (`-`, `/`, `:`, space, `,`) |

`m` and `mm` mean *minutes* instead of months when they come right after an hour (`h:mm`) or right before seconds (`mm:ss`).

```sql
SELECT Format(#2024-03-05 14:07:09#, 'yyyy-mm-dd hh:nn:ss');   -- 2024-03-05 14:07:09
SELECT Format(#2024-03-05 14:07:09#, 'dddd, mmmm d, yyyy');    -- Tuesday, March 5, 2024
SELECT Format(#2024-03-05 14:07:09#, 'h:mm AM/PM');            -- 2:07 PM
SELECT Format(#2024-03-05#, 'ddd d mmm');                      -- Tue 5 Mar
```

## Text

| Character | Meaning |
| --- | --- |
| `>` | show the text in capitals |
| `<` | show the text in lower case |
| `@` | a character place; shows a space if the text has no character for it |
| `&` | a character place; shows nothing if the text has no character for it |
| other characters | shown as written |

Characters fill the places from the right; extra characters on the left go to the first place.

```sql
SELECT Format('abc', '>');                -- ABC
SELECT Format('12345', '@@-@@@');         -- 12-345
```
