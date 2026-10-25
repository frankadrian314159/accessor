# Sharing a database between threads

Accessor has three levels of use from several threads. All are in `accessor-core`.

| You want | Use |
| --- | --- |
| to move a database to another thread | `Database` itself: it is `Send` |
| several threads reading one database you own | `&Database` and `query` / `query_prepared`: it is `Sync`. No writer can run meanwhile, because writing needs `&mut Database` |
| several threads that **both read and write**, each with its own transactions, **at the same time** | `SharedDatabase` and `Connection` (this page) |

## SharedDatabase and Connection

```rust
use accessor_core::{Error, SharedDatabase};

let db = SharedDatabase::open("shop.adb")?;          // or SharedDatabase::in_memory()
db.connect().execute("CREATE TABLE IF NOT EXISTS c (id COUNTER PRIMARY KEY, n INTEGER)")?;

std::thread::scope(|s| {
    for _ in 0..4 {
        let conn = db.connect();                      // one per thread
        s.spawn(move || loop {
            conn.execute("BEGIN").unwrap();
            let result = (|| {
                conn.execute("INSERT INTO c (n) VALUES (1)")?;
                conn.execute("UPDATE c SET n = n + 1 WHERE id = 1")?;
                conn.execute("COMMIT")
            })();
            match result {
                Ok(_) => break,
                Err(Error::Conflict(_)) => { conn.execute("ROLLBACK").unwrap(); } // another transaction got there first: try again
                Err(e) => panic!("{e}"),
            }
        });
    }
});
```

`SharedDatabase` is cheap to clone (another handle to the same database). `connect()` gives a `Connection`, which can be moved to any thread. A connection has its own transaction: `BEGIN` … `COMMIT` / `ROLLBACK` through `execute`, as in the shell. `Connection` has `execute`, `prepare`, `execute_prepared`, `checkpoint`, `in_transaction`, and the general `read(|db| …)` and `write(|db| …)`, which borrow the `Database` for the length of a closure.

## What a transaction sees: snapshots

Rows are kept as **versions**. A transaction sees the database as it was at the moment it began (its *snapshot*) plus its own changes. This is *snapshot isolation*, the level that Oracle and PostgreSQL's `REPEATABLE READ` give.

* **No dirty reads.** Nobody sees another transaction's changes until it commits, and then only transactions that begin afterwards do.
* **Repeatable reads.** Asked twice, a transaction gets the same answer, however much commits meanwhile: no rows appear, vanish or change under it. A report that reads three tables sees one moment in time.
* **Readers never wait for writers, and writers never wait for readers.** An open transaction, however long, holds up nobody's reads.
* A connection that is **not** in a transaction sees everything committed so far, statement by statement, and each statement is its own transaction.

## What happens when writers collide: first to commit wins

Any number of transactions can have changes pending at once. They only get in each other's way if they change **the same row** (or the same unique key, or the two sides of a foreign key):

| The second transaction wants | The first has | What happens |
| --- | --- | --- |
| a row the first has changed, not yet committed | an open change | the second **waits**, trying again, until the first ends: if it rolled back, the second goes ahead; if it committed, see the next line |
| a row changed by a transaction that **committed after the second began** | a committed change | the second gets `Error::Conflict` ("could not serialize access due to a concurrent update"). **Roll back and run the transaction again** |
| a row nobody else touched | | nothing: it goes ahead whatever else is open |

A statement outside a transaction that has to wait is simply tried again until the other transaction ends, up to the busy timeout. A statement inside a transaction that gets `Conflict` has been undone, and the transaction is still open, but it cannot see the row's new state, so the usual answer is `ROLLBACK` and start over. `Error::Conflict` is the one error a program using several writers must expect; `docs/extern` lists it as SQL state `40001` and `ACC_ERR_CONFLICT`.

The same rule decides:

* **Unique keys and primary keys.** Two transactions adding the same value: the second waits for the first, then gets either `Error::Constraint` (the key is taken) or goes ahead (the first rolled back).
* **Foreign keys.** A child added while its parent is being deleted, or a parent deleted while a child is being added, waits and then fails the usual way: whichever commits first wins, and the other sees a relationship that is no longer satisfied. Foreign keys are checked against the *newest* state, not the snapshot, which is what keeps them whole. A cascade that would have to delete a child the transaction cannot see is a `Conflict` rather than a skipped row.
* **Deadlocks.** Two transactions each holding a row the other wants would wait for ever. The one that notices first (the second to start waiting) gets `Conflict` ("a deadlock") at once, rolls back, and the other goes on.

### What snapshot isolation does not prevent

*Write skew.* Two transactions each read the same rows, each decide on that basis, and each write a **different** row; neither collides with the other, and both commit, so the result is one no serial order would give. The textbook case: two doctors on call, each checks that the other is on call before going off. The test `write_skew_is_possible_under_snapshot_isolation` shows it happening. If a rule spans rows, make the transactions touch a common row (update a counter, or a summary row, in each) so that they collide.

## Turns: how statements are run

The tables are one shared structure, so the engine runs **one statement at a time**, but a transaction is not a statement: other transactions' statements run in between.

* A statement that only reads (`SELECT`, `EXPLAIN`, prepared or not) takes a **read turn**. Any number of those run together on any number of threads.
* Anything else takes the **write turn**: it waits until no read is running, and runs alone for the length of that one statement.
* **Reads come first.** A reader waits only for a statement that is writing at that moment, never for one that is waiting. The price is that a steady stream of reads can hold writes off: a writer waits for a moment with no read running.
* A `SELECT` that could change something takes the write turn: one that calls `NEXTVAL` or `SETVAL`, a stored function, or a saved query that mentions a sequence. Sequence numbers are distinct across threads and logged before the statement returns.

So "simultaneous writers" means simultaneous **transactions**: any number are open, with changes pending, reading their own snapshots and writing, and their individual statements take turns. Statements are short; a transaction can be as long as its owner likes.

## Schema changes

The catalog (tables, columns, indexes, views, triggers, …) has no versions, so a schema change cannot be hidden from other transactions. `CREATE`, `ALTER`, `DROP` and the like:

* wait until no other transaction is open (they are tried again until the busy timeout, then `Conflict`);
* and from then until their own transaction ends, **everyone else is kept out**, readers too (they wait and try again).

Do schema changes when nothing else is going on, or accept the pause. A transaction may mix row changes and schema changes; they commit together.

## Waiting, and not waiting for ever

```rust
db.set_busy_timeout(Some(std::time::Duration::from_secs(5)));   // the default is ten seconds
```

A statement that has to wait for another transaction (to finish a change it needs to get past, or to leave the schema) gives up after the busy timeout with `Error::Conflict`; one that cannot get its turn at all gets `Error::Locked`. In both cases nothing was changed by the statement.

Do not call a connection's methods from inside its own `read` or `write` closure: it would wait for itself. Use one connection from one thread at a time; two threads on one connection would be mixing their statements into one transaction.

## Cleaning up

Old versions are kept only as long as some open transaction could still see them, and cleaned away when the last such transaction ends (and as writes happen). A connection that opens a transaction and never ends it makes the database keep every version written since: close long transactions. A connection that is dropped with a transaction open rolls it back.

## What is shared

All connections to one `SharedDatabase` see the same **database settings**, because they are properties of the `Database`, not of a session:

* the time zone (`SET TIME ZONE`), the optimizer switch and the durability mode;
* statements named with `PREPARE name AS …` (use `Connection::prepare`, which gives a handle you own, for per-connection statements).

## Files, durability and processes

A database file can be open once ([locking](extern/locking.md)). A `SharedDatabase` is one open, so its connections share the one lock, and other processes are kept out as before. Opening the same file twice in one process, with `Database::open` or `SharedDatabase::open`, fails with `Error::Locked`: connect to the one you have. There is no shared mode across processes; to use one database from several programs, let one own it and serve the others, or take turns ([several processes](extern/locking.md#using-one-database-from-several-programs)).

Each commit is one record in the write-ahead log, written when the transaction commits, in commit order. After a crash a database holds exactly the transactions that had committed, whatever order they began in, and none that had not. A checkpoint (`Connection::checkpoint`, or the automatic one) writes the committed state and leaves out changes still pending.

## Cost

* **A database not shared** (`Database` used directly) changes rows in place, as before: no versions, no measurable cost.
* **A shared one** adds a stamp pair and a link to each row version, a visibility check when rows are read, and the clean-up. In the project's own timings (`cargo run --release -p accessor-core --example bench_versions`) point lookups and scans cost the same, bulk inserts 10 to 15% more, and a statement that rewrites every row about twice as much while the old versions exist.
* The optimizer's estimates use the number of rows that are current, which while transactions overlap can include rows not yet committed or just deleted; the answers are never affected, only the choice of plan, and only a little.

## Not done

* **No parallel execution of statements.** Two writers' statements never run at the same moment; nor does a writer's with a reader's. Doing that needs per-table latches throughout the executor.
* **Not serializable.** See write skew above.
* **The C interface and JDBC driver** still use one connection per open database, on its own thread; they do not expose `SharedDatabase`. (`ACC_ERR_CONFLICT` and SQL state `40001` exist for when they do.)

## Tests

* `tests/mvcc.rs`: snapshots (no dirty reads, repeatable reads, indexes finding the right version, savepoints), the first-committer-wins rules for rows, unique keys and foreign keys, deadlock detection, schema changes among open transactions, clean-up (the number of versions kept), crash recovery with interleaved commits and a checkpoint in the middle, and two stress tests: transfers between accounts while readers check that every snapshot adds up, and a mixed load of transfers, deletes, key changes and cascades that ends by checking every row against every index and that no versions are left over.
* `tests/shared.rs`: the basics from several threads. `tests/send_sync.rs` keeps `Database` `Send` and `Sync`.
* The whole test suite is also run with every `Database` in version mode (`ACCESSOR_FORCE_MVCC=1 cargo test --workspace`), which checks that the versioned storage behaves like the plain one.
