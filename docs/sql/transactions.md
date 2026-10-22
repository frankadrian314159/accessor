# Transactions

By default every statement is its own transaction: it either takes effect completely, and is on disk when the call returns, or has no effect. A transaction groups several statements so that they succeed or fail together.

```sql
BEGIN [TRANSACTION | WORK];
COMMIT [TRANSACTION | WORK];
ROLLBACK [TRANSACTION | WORK];
SAVEPOINT name;
ROLLBACK [TRANSACTION | WORK] TO [SAVEPOINT] name;
RELEASE [SAVEPOINT] name;
```

## Example

```sql
BEGIN;
INSERT INTO Orders (CustID, Total) VALUES (1, 50);
SAVEPOINT after_order;
INSERT INTO Lines (OrderID, Item) VALUES (1, 'x');
ROLLBACK TO after_order;      -- undoes only the line
COMMIT;                       -- the order is saved
```

## Behaviour

* **Statement atomicity.** Inside a transaction, a statement that fails is undone completely and the transaction stays open. You can fix the problem and carry on.
* **Schema changes are transactional.** `CREATE TABLE`, `ALTER TABLE`, `DROP TABLE`, indexes, views and triggers can all be rolled back.
* **Savepoints** mark a point to return to. `ROLLBACK TO` undoes what came after it and leaves the savepoint in place; `RELEASE` forgets it. Names are not case-sensitive, and re-using a name moves the savepoint.
* **One commit, one write.** `COMMIT` writes the whole transaction to the log in a single checksummed record. After a crash, the transaction is either wholly present or wholly absent.
* **No nesting.** `BEGIN` inside a transaction is an error; use savepoints.
* **Not allowed inside a transaction:** checkpointing and compacting the file.
* **Closing a database with a transaction open rolls it back.**
* **Isolation.** There is one connection at a time to a database, so there are no concurrent transactions to isolate from: a transaction sees its own changes immediately.

`MERGE` uses a transaction and a savepoint internally so that it is atomic.
