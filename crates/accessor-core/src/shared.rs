//! One database, many connections: any number of readers at once, or one writer.
//!
//! A [`SharedDatabase`] owns a [`Database`] and hands out [`Connection`]s, which may be used from
//! different threads. The rules are those of a read/write lock with one addition, the transaction:
//!
//! * `SELECT` and `EXPLAIN` (and prepared `SELECT`s) take a **read** turn. Any number of them run at
//!   the same time on any number of threads, each seeing the database as it is between writes.
//! * Anything else takes the **write** turn: it waits for the readers to finish, runs alone, and
//!   lets the next in.
//! * `BEGIN` keeps the write turn **for the connection** until `COMMIT` or `ROLLBACK`, across calls.
//!   Every other connection waits meanwhile, readers too, because the database holds the
//!   transaction's uncommitted changes and there are no row versions to read around them. A
//!   connection that is dropped with a transaction open rolls it back.
//!
//! A waiting writer holds off new readers, so a steady stream of reads cannot starve it.
//!
//! A [`busy timeout`](SharedDatabase::set_busy_timeout) turns waiting forever into an
//! [`Error::Locked`], which is the way out of a connection that forgot to commit.
//!
//! The file lock ([`Error::Locked`] on a second open) is per database, and a shared database is one
//! open, so all its connections share the one lock, and other processes are still kept out.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use crate::database::Database;
use crate::error::{Error, Result};
use crate::exec::{Prepared, QueryResult};
use crate::value::Value;

struct Gate {
    /// Connections reading right now.
    readers: usize,
    /// The connection holding the write turn: for one statement, or for a whole transaction.
    writer: Option<u64>,
    /// Connections waiting for the write turn (new readers wait for them).
    waiting_writers: usize,
}

struct Inner {
    db: RwLock<Database>,
    gate: Mutex<Gate>,
    changed: Condvar,
    next_id: AtomicU64,
    busy_timeout: Mutex<Option<Duration>>,
}

impl Inner {
    fn gate(&self) -> MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn timeout(&self) -> Option<Duration> {
        *self.busy_timeout.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits until `ready` holds for the gate, or the busy timeout runs out.
    fn wait_for<'a>(&'a self, mut g: MutexGuard<'a, Gate>, ready: impl Fn(&Gate) -> bool, what: &str) -> Result<MutexGuard<'a, Gate>> {
        let deadline = self.timeout().map(|t| Instant::now() + t);
        while !ready(&g) {
            match deadline {
                None => g = self.changed.wait(g).unwrap_or_else(|e| e.into_inner()),
                Some(d) => {
                    let left = d.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(Error::Locked(format!(
                            "the database is busy: another connection is {what} (the busy timeout ran out)"
                        )));
                    }
                    g = self.changed.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
                }
            }
        }
        Ok(g)
    }
}

/// Gives back a read turn when dropped, even on a panic.
struct ReadTurn<'a>(&'a Inner);

impl Drop for ReadTurn<'_> {
    fn drop(&mut self) {
        let mut g = self.0.gate();
        g.readers -= 1;
        self.0.changed.notify_all();
    }
}

/// Gives back the write turn when dropped, unless a transaction is still open.
struct WriteTurn<'a> {
    inner: &'a Inner,
    keep: bool,
}

impl Drop for WriteTurn<'_> {
    fn drop(&mut self) {
        if !self.keep {
            self.inner.gate().writer = None;
            self.inner.changed.notify_all();
        }
    }
}

/// A database that several threads can use at once, through [`Connection`]s. Cloning it is cheap
/// and gives another handle to the same database.
#[derive(Clone)]
pub struct SharedDatabase {
    inner: Arc<Inner>,
}

impl SharedDatabase {
    /// Shares `db`.
    pub fn new(db: Database) -> Self {
        SharedDatabase {
            inner: Arc::new(Inner {
                db: RwLock::new(db),
                gate: Mutex::new(Gate { readers: 0, writer: None, waiting_writers: 0 }),
                changed: Condvar::new(),
                next_id: AtomicU64::new(1),
                busy_timeout: Mutex::new(None),
            }),
        }
    }

    /// A shared database that lives only in memory.
    pub fn in_memory() -> Self {
        Self::new(Database::in_memory())
    }

    /// Opens (or creates) the database file at `path` and shares it. The file is locked for as long
    /// as any handle or connection is alive.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Database::open(path).map(Self::new)
    }

    /// A new connection. Each has its own transaction; open as many as you like.
    pub fn connect(&self) -> Connection {
        Connection { shared: self.clone(), id: self.inner.next_id.fetch_add(1, Ordering::Relaxed) }
    }

    /// How long a connection waits for its turn before giving up with [`Error::Locked`]
    /// (`None`, the default, waits for as long as it takes).
    pub fn set_busy_timeout(&self, timeout: Option<Duration>) {
        *self.inner.busy_timeout.lock().unwrap_or_else(|e| e.into_inner()) = timeout;
    }
}

/// One user of a [`SharedDatabase`]: it has its own transaction, and takes its turn with the others.
///
/// A connection may be moved to any thread, but use each from one thread at a time: statements run
/// from two threads on one connection would be mixed into one transaction.
pub struct Connection {
    shared: SharedDatabase,
    id: u64,
}

fn unavailable() -> Error {
    Error::runtime("the database is unavailable: a thread panicked while using it")
}

impl Connection {
    fn inner(&self) -> &Inner {
        &self.shared.inner
    }

    /// True while this connection has a transaction open (and so holds everyone else up).
    pub fn in_transaction(&self) -> bool {
        self.inner().gate().writer == Some(self.id)
    }

    /// Runs `f` with the database borrowed for reading, alongside any other readers. Inside `f` do not
    /// use another call of this connection: it would wait for itself.
    pub fn read<R>(&self, f: impl FnOnce(&Database) -> Result<R>) -> Result<R> {
        let inner = self.inner();
        if self.in_transaction() {
            // Its own uncommitted changes are visible to it: it holds the turn already.
            let db = inner.db.read().map_err(|_| unavailable())?;
            return f(&db);
        }
        let mut g = inner.wait_for(inner.gate(), |g| g.writer.is_none() && g.waiting_writers == 0, "writing")?;
        g.readers += 1;
        drop(g);
        let _turn = ReadTurn(inner);
        let db = inner.db.read().map_err(|_| unavailable())?;
        f(&db)
    }

    /// Runs `f` with the database borrowed for writing: alone, after the readers have finished. If `f`
    /// leaves a transaction open, this connection keeps the turn until it ends.
    pub fn write<R>(&self, f: impl FnOnce(&mut Database) -> Result<R>) -> Result<R> {
        let inner = self.inner();
        let mut turn = WriteTurn { inner, keep: self.in_transaction() };
        if !turn.keep {
            let mut g = inner.gate();
            g.waiting_writers += 1;
            let waited = inner.wait_for(g, |g| g.writer.is_none() && g.readers == 0, "using it");
            let mut g = match waited {
                Ok(g) => g,
                Err(e) => {
                    inner.gate().waiting_writers -= 1;
                    inner.changed.notify_all();
                    return Err(e);
                }
            };
            g.waiting_writers -= 1;
            g.writer = Some(self.id);
        }
        let mut db = inner.db.write().map_err(|_| unavailable())?;
        let result = f(&mut db);
        turn.keep = db.in_transaction();
        result
    }

    /// Runs every statement in `sql` and returns the result of the last. A `SELECT` runs alongside
    /// other readers; anything else runs alone.
    pub fn execute(&self, sql: &str) -> Result<QueryResult> {
        if !self.in_transaction() {
            let read = self.read(|db| {
                let parsed = db.parse_cached(sql)?;
                if db.is_pure_read(&parsed) {
                    db.run_read(&parsed).map(Some)
                } else {
                    Ok(None)
                }
            })?;
            if let Some(r) = read {
                return Ok(r);
            }
        }
        self.write(|db| db.execute(sql))
    }

    /// Reads `sql` once, to be run many times with [`execute_prepared`](Connection::execute_prepared).
    pub fn prepare(&self, sql: &str) -> Result<Prepared> {
        self.read(|db| db.prepare(sql))
    }

    /// Runs a prepared statement; a `SELECT` runs alongside other readers.
    pub fn execute_prepared(&self, prepared: &Prepared, params: &[(&str, Value)]) -> Result<QueryResult> {
        if !self.in_transaction() {
            let read = self.read(|db| {
                if db.is_pure_read(prepared.statements()) {
                    db.query_prepared(prepared, params).map(Some)
                } else {
                    Ok(None)
                }
            })?;
            if let Some(r) = read {
                return Ok(r);
            }
        }
        self.write(|db| db.execute_prepared(prepared, params))
    }

    /// Folds the log into the snapshot (see [`Database::checkpoint`]). Waits its turn to write.
    pub fn checkpoint(&self) -> Result<()> {
        self.write(|db| db.checkpoint())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if self.in_transaction() {
            let _ = self.write(|db| db.execute("ROLLBACK").map(|_| ()));
            // `write` kept the turn only if the rollback failed; give it back regardless.
            let mut g = self.inner().gate();
            if g.writer == Some(self.id) {
                g.writer = None;
                self.inner().changed.notify_all();
            }
        }
    }
}
