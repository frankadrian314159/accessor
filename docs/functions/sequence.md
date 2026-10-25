# Sequence functions

These work with [sequences](../sql/sequences.md). The sequence is named by a text value; the name is not case-sensitive.

## NEXTVAL

`NEXTVAL('name')` advances the sequence and returns the new number. The standard form is `NEXT VALUE FOR name`. If the sequence has a limit and no `CYCLE`, passing it is an error.

```sql
SELECT NEXTVAL('ids'), NEXT VALUE FOR ids;
```

## CURRVAL

`CURRVAL('name')` returns the number the sequence last handed out, without advancing it. It is an error if none has been handed out yet. `PREVIOUS VALUE FOR name` is the standard spelling.

## LASTVAL

`LASTVAL()` returns the number last handed out by any sequence in this session.

## SETVAL

`SETVAL('name', n [, is_called])` sets the position and returns `n`. With `is_called` true (the default) the next `NEXTVAL` returns `n + increment`; with false it returns `n`. The value must lie between the sequence's minimum and maximum.

```sql
SELECT SETVAL('ids', (SELECT MAX(No) FROM Invoice));     -- continue after the largest used
```

## Notes

* These functions change the sequence even in a query that is only reading; they are the one case where a `SELECT` has an effect.
* They are volatile: never evaluated once for a whole query, and never folded away by the optimizer.
* A NULL name gives NULL.
