# The query optimizer and EXPLAIN

The optimizer and `EXPLAIN` have their own folder of documentation: **[../optimizer/](../optimizer/README.md)**.

| Page | Contents |
| --- | --- |
| [README](../optimizer/README.md) | what the optimizer does, with an example |
| [pushdown](../optimizer/pushdown.md) | applying conditions to a table before joining |
| [join-order](../optimizer/join-order.md) | join reordering and join methods |
| [access-paths](../optimizer/access-paths.md) | index selection, ordered reads, `COUNT(*)` |
| [estimates](../optimizer/estimates.md) | how row counts are estimated |
| [explain](../optimizer/explain.md) | the `EXPLAIN` statement and how to read its output |
| [tuning](../optimizer/tuning.md) | indexes, writing conditions, switching it off, limits |

The statement itself:

```sql
EXPLAIN query
EXPLAIN QUERY PLAN query
```

runs the query and returns its plan, one line of text per row, in a column called `plan`.
