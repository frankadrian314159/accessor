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

```sql
SET CONSTRAINTS {ALL | name [, ...]} {DEFERRED | IMMEDIATE};
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
* **Isolation.** Transactions never overlap: a database file is open once (see [locking](../extern/locking.md)), and connections that share it through `SharedDatabase` take turns, a transaction keeping the turn until it ends ([concurrency](../concurrency.md)). So the isolation is `SERIALIZABLE`, got by running one at a time, and a transaction sees its own changes immediately and nobody else's.

## SET CONSTRAINTS

Inside a transaction, `SET CONSTRAINTS ALL DEFERRED` (or naming relationships) postpones the checking of [deferrable](ddl.md#deferrable-relationships) relationships until `COMMIT`; `SET CONSTRAINTS ... IMMEDIATE` returns to checking at the end of each statement, and checks at once whatever was waiting. If the check fails, the statement fails and the relationships stay deferred. The setting lasts until the transaction ends. Naming a relationship that is not deferrable is an error; outside a transaction the statement does nothing.

```sql
BEGIN;
SET CONSTRAINTS ALL DEFERRED;
INSERT INTO Child  VALUES (1, 7);     -- parent 7 does not exist yet
INSERT INTO Parent VALUES (7);
COMMIT;                               -- verified here: fine
```

A `COMMIT` that finds a deferred relationship broken rolls back the whole transaction and reports the first broken record. The check reads the rows of the tables whose relationships were deferred, using the parent's index for each; it is one pass over the child rows, so deferring in a transaction that touches millions of rows costs a scan of them at the end.

## Sequences

The numbers [sequences](sequences.md) hand out are not part of a transaction: rolling back does not give them back.

`MERGE` uses a transaction and a savepoint internally so that it is atomic.
