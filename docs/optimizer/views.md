# Views and the optimizer

A saved query used in a `FROM` clause is, by default, **computed first and then filtered**: `SELECT * FROM Live WHERE name = 'nut'` would build all of `Live` and only then look for the nut. For the commonest kind of view the optimizer does better by **merging** it into the query.

## View merging

A view is merged when it

* reads exactly one table (not another view, a join, a derived table or a sub-query in `FROM`),
* shows plain columns of that table (any of them, but **in the table's own order**; they may be renamed),
* has no `DISTINCT`, grouping, aggregate, limit, window function, set operator or parameters,

that is, it is `SELECT col, col, ... FROM Table [WHERE condition]` (or `SELECT *`), possibly with a column-name list. Such a view is just a table with an extra condition and some columns hidden, so the query reads the table directly and the view's condition is applied **during the scan**, together with the query's own:

```sql
CREATE VIEW Live AS SELECT * FROM Prod WHERE active;
CREATE INDEX pname ON Prod (name);

EXPLAIN SELECT id FROM Live WHERE name = 'nut';
```

```
scan Live [5 rows]: index pname on name = (nut) (2 condition(s) pushed down)
```

The index on `name` is used even though the query is written against the view. In a join the view's condition filters the table before the join, like any other pushed-down condition, and `COUNT(*)`, `ORDER BY ... LIMIT` and join reordering all see the real table.

Merging never changes the answer. The one place it is not done is where the view would be the **preserved side** of a `RIGHT` or `FULL` join (where a row filtered out by the view must not reappear), and there the view is simply computed first. Views that compute a column, put the columns in another order than the table's, repeat a column, join, group or limit are computed first; they are still correct, only without the index help. If you want index use through such a view, select from the table directly. The same rule merges a derived table, `FROM (SELECT ...) AS d`: see [subqueries.md](subqueries.md#derived-tables-and-views-read-as-their-table). The columns a merged view does not show cannot be named from outside it.

## Changing data through a view

`INSERT`, `UPDATE` and `DELETE` on a *simple* view (one table, plain columns, no grouping or limit) are carried out on its table with the view's condition added to the statement, so they use the table's indexes like any other statement. See [../sql/queries-and-triggers.md](../sql/queries-and-triggers.md#changing-data-through-a-view).

## Other saved queries

A view that groups, joins or limits is a derived table: it is computed in full, then the outer query filters it. Conditions on its output columns are not pushed inside. Write the condition inside the view, or use a parameterised query (`CREATE QUERY ... (@p)`), when you want the filter applied before the summarising.

## Switching it off

With the optimizer off (`.optimizer off`), no view is merged; every view is computed first. The answers are the same.
