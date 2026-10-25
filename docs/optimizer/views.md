# Views and the optimizer

A saved query used in a `FROM` clause is, by default, **computed first and then filtered**: `SELECT * FROM Live WHERE name = 'nut'` would build all of `Live` and only then look for the nut. For the commonest kind of view the optimizer does better by **merging** it into the query.

## View merging

A view is merged when it

* reads exactly one table (not another view, a join, a derived table or a sub-query in `FROM`),
* shows **every column** of that table, in the table's order (they may be renamed),
* has no `DISTINCT`, grouping, aggregate, limit, window function, set operator or parameters,

that is, it is `SELECT * FROM Table [WHERE condition]`, possibly with a column-name list. Such a view is just a table with an extra condition, so the query reads the table directly and the view's condition is applied **during the scan**, together with the query's own:

```sql
CREATE VIEW Live AS SELECT * FROM Prod WHERE active;
CREATE INDEX pname ON Prod (name);

EXPLAIN SELECT id FROM Live WHERE name = 'nut';
```

```
scan Live [5 rows]: index pname on name = (nut) (2 condition(s) pushed down)
```

The index on `name` is used even though the query is written against the view. In a join the view's condition filters the table before the join, like any other pushed-down condition, and `COUNT(*)`, `ORDER BY ... LIMIT` and join reordering all see the real table.

Merging never changes the answer. The one place it is not done is where the view would be the **preserved side** of a `RIGHT` or `FULL` join (where a row filtered out by the view must not reappear), and there the view is simply computed first. Views that select only some of a table's columns, or compute columns, are also computed first; they are still correct, only without the index help. If you want index use through a projection, make the view `SELECT *` or select from the table directly.

## Changing data through a view

`INSERT`, `UPDATE` and `DELETE` on a *simple* view (one table, plain columns, no grouping or limit) are carried out on its table with the view's condition added to the statement, so they use the table's indexes like any other statement. See [../sql/queries-and-triggers.md](../sql/queries-and-triggers.md#changing-data-through-a-view).

## Other saved queries

A view that groups, joins or limits is a derived table: it is computed in full, then the outer query filters it. Conditions on its output columns are not pushed inside. Write the condition inside the view, or use a parameterised query (`CREATE QUERY ... (@p)`), when you want the filter applied before the summarising.

## Switching it off

With the optimizer off (`.optimizer off`), no view is merged; every view is computed first. The answers are the same.
