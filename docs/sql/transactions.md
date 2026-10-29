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
* **Isolation.** A `Database` used directly has one transaction at a time. Connections that share a database through `SharedDatabase` ([concurrency](../concurrency.md)) overlap, and each transaction sees a **snapshot** of the committed data as of its beginning plus its own changes (snapshot isolation): no dirty reads, no non-repeatable reads, no phantoms. Two transactions that change the same row cannot both commit: the later one gets `Error::Conflict` and is run again. Snapshot isolation is not serializable: *write skew* is possible.

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

## Cursors

```sql
DECLARE name [BINARY] [INSENSITIVE] [[NO] SCROLL] CURSOR [{WITH | WITHOUT} HOLD] FOR query
FETCH [direction] [FROM | IN] name         -- or MOVE, which only moves
CLOSE name | CLOSE ALL
```

`DECLARE` runs the query and keeps its rows: the cursor does not see changes made to the tables afterwards (PostgreSQL's *insensitive* cursor, which is what an unmarked one is there too). The cursor stands before the first row, on a row, or after the last, and `FETCH` moves it. The direction is `NEXT` (the default), `PRIOR`, `FIRST`, `LAST`, `ABSOLUTE n` (from the end if `n` is negative), `RELATIVE n`, a count `n` (forward; a negative count goes backward), `ALL`, or `FORWARD` / `BACKWARD` with a count or `ALL`; backward rows come last row first. Beyond either end a fetch gives no row and the cursor stays outside the rows. `MOVE` takes the same directions and gives no rows. A cursor not declared `WITH HOLD` is closed by `COMMIT` and `ROLLBACK`. Every cursor can scan both ways (`SCROLL` is read and not needed), and a cursor can be declared outside a transaction.

## Sequences

The numbers [sequences](sequences.md) hand out are not part of a transaction: rolling back does not give them back.

`MERGE` uses a transaction and a savepoint internally so that it is atomic.
