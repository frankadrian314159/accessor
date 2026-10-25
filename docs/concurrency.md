# Sharing a database between threads

Accessor has three levels of use from several threads. All are in `accessor-core`.

| You want | Use |
| --- | --- |
| to move a database to another thread | `Database` itself: it is `Send` |
| several threads reading one database you own | `&Database` and `query` / `query_prepared`: it is `Sync`. No writer can run meanwhile, because writing needs `&mut Database` |
| several threads that **both read and write**, each with its own transactions | `SharedDatabase` and `Connection` (this page) |

## SharedDatabase and Connection

```rust
use accessor_core::{SharedDatabase, Value};

let db = SharedDatabase::open("shop.adb")?;          // or SharedDatabase::in_memory()
db.connect().execute("CREATE TABLE IF NOT EXISTS c (id COUNTER PRIMARY KEY, n INTEGER)")?;

std::thread::scope(|s| {
    for _ in 0..4 {
        let conn = db.connect();                      // one per thread
        s.spawn(move || {
            conn.execute("BEGIN").unwrap();
            let _count = conn.execute("SELECT COUNT(*) FROM c").unwrap();
            conn.execute("INSERT INTO c (n) VALUES (1)").unwrap();
            conn.execute("COMMIT").unwrap();
        });
    }
});
```

`SharedDatabase` is cheap to clone (another handle to the same database). `connect()` gives a `Connection`, which can be moved to any thread. A connection has its own transaction: `BEGIN` … `COMMIT` / `ROLLBACK` through `execute`, as in the shell. `Connection` has `execute`, `prepare`, `execute_prepared`, `checkpoint`, `in_transaction`, and the general `read(|db| …)` and `write(|db| …)`, which borrow the `Database` for the length of a closure.

## The rules

**Any number of readers, or one writer.**

* `SELECT` and `EXPLAIN`, plain or prepared, take a **read turn**. Readers run at the same time, on as many threads as there are connections, and see the database as of between writes.
* Everything else takes the **write turn**: it waits for the readers to finish, runs alone, and lets the next one in. This includes `INSERT`, `UPDATE`, `DELETE`, DDL, `CALL`, `EXEC`, reports and `SET TIME ZONE`.
* A `SELECT` that could change something is a write: one that calls `NEXTVAL` or `SETVAL`, calls a stored function, or reads a saved query that mentions a sequence. (If it were a read, two readers could be handed the same number, or a number could be handed out and not logged.) Sequence numbers are distinct across threads, and logged before the statement returns.
* A **transaction** keeps the write turn for its connection from `BEGIN` to `COMMIT` or `ROLLBACK`, across calls. Every other connection waits, readers too. This is the price of keeping the engine's design: the data lives once, in memory, with no row versions, so a reader cannot be shown "the database before the transaction". In particular a connection never sees another's uncommitted changes.
* A waiting writer holds off new readers, so a steady stream of reads cannot keep a write waiting for ever.
* A connection **dropped** with a transaction open rolls it back and gives up the turn. So does a connection whose thread panics (the turn is returned; the transaction stays open until the connection is dropped or rolled back).
* A statement that fails inside a transaction is undone alone and the transaction stays open, as everywhere.

Transactions on different connections are therefore **serial**: the result is as if they ran one after another, whichever order they got their turns in. This is the strongest isolation level (`SERIALIZABLE`), reached by never running two at once. Read-modify-write in a transaction is safe; read-modify-write across two separate statements in autocommit mode is not (another connection can write between them), as in any database.

### Waiting, and not waiting for ever

By default a connection waits as long as it takes. A forgotten open transaction would hold everyone else up for ever, so a timeout can be set:

```rust
db.set_busy_timeout(Some(std::time::Duration::from_secs(5)));
```

A connection that waits longer gets `Error::Locked`, "the database is busy: another connection is …". The statement has not run, and the connection can try again.

Do not call a connection's methods from inside its own `read` or `write` closure: it would wait for itself. Use one connection from one thread at a time; two threads on one connection would be mixing their statements into one transaction.

## What is shared

All connections to one `SharedDatabase` see the same **database settings**, because they are properties of the `Database`, not of a session:

* the time zone (`SET TIME ZONE`), the optimizer switch and the durability mode;
* statements named with `PREPARE name AS …` (use `Connection::prepare`, which gives a handle you own, for per-connection statements);
* constraint timing set with `SET CONSTRAINTS` (it ends with the transaction, so in practice it belongs to the connection that holds the turn).

## Files and processes

A database file can be open once ([locking](extern/locking.md)). A `SharedDatabase` is one open, so its connections share the one lock, and other processes are kept out as before. Opening the same file twice in one process, with `Database::open` or `SharedDatabase::open`, fails with `Error::Locked`: connect to the one you have. There is no shared-read mode across processes; the answer to that is a server process owning the database, with the C interface or JDBC driver of other programs talking to it.

## What this does not give

* **No parallel writers** and no parallelism *inside* a statement. A long `UPDATE` stops everyone, and so does an open transaction.
* **No snapshot reads while a transaction is open**, as above.
* **No deadlocks between connections**, since each connection takes at most one turn at a time and keeps it only when in a transaction; but a connection can wait for ever for a transaction that never ends, hence the busy timeout.

## Tests

`crates/accessor-core/tests/shared.rs` has twelve tests: six readers inside a barrier at once (proving that reads overlap), eight threads reading, eight writing, six running read-modify-write transactions whose total must be exact, an open transaction holding up others, a waiter released by a commit, rollback and drop, a failed statement in a transaction, the busy timeout, sequences handed out to four threads without a repeat, prepared statements and a shared file. `tests/send_sync.rs` keeps `Database` `Send` and `Sync`.
