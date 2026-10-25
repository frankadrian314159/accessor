//! One database, many connections, many transactions at once.
//!
//! A [`SharedDatabase`] owns a [`Database`] and hands out [`Connection`]s, which may be used from
//! different threads. Each connection has its own transaction, and transactions overlap: while one is
//! open, others begin, read, write and commit.
//!
//! **Snapshots.** A transaction sees the database as it was when it began, plus its own changes
//! (*snapshot isolation*). Rows are kept as versions, so a reader is never held up by a writer and
//! never sees half of what one has done. A connection that is not in a transaction sees everything
//! committed so far, statement by statement.
//!
//! **Writers.** Any number of transactions can have changes pending at once. Two of them cannot change
//! the same row: the one that gets there second is told. If the first has not finished, the second
//! waits (the statement is simply tried again) up to the busy timeout; if it has committed since the
//! second began, the second gets [`Error::Conflict`] and should roll back and start over. Nothing is
//! ever blocked by a *read*. The same rule decides who wins a unique key (two transactions adding the
//! same value) and a foreign key (a child added while its parent is removed).
//!
//! **Turns.** Statements are run one at a time, because the tables are one shared structure. Reads
//! come first: a statement that only reads (`SELECT`, `EXPLAIN`, prepared or not) takes a *read turn*
//! and any number of those run together; anything else takes the *write turn* and waits until no read
//! is running. A steady stream of reads can therefore hold writers off; that is the trade the design
//! makes. A turn lasts for one statement, so a transaction that sits open holds nothing up except the
//! rows it has changed.
//!
//! **Schema changes** (`CREATE`, `ALTER`, `DROP`, …) are not versioned. One waits until no other
//! transaction is open, and then keeps everyone else out until its own transaction ends.
//!
//! The file lock ([`Error::Locked`] on a second open) is per database, and a shared database is one
//! open, so all its connections share the one lock, and other processes are still kept out.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use crate::database::{Database, Tx};
use crate::error::{Error, Result};
use crate::exec::{Prepared, QueryResult};
use crate::storage::mvcc::{self, View};
use crate::value::Value;

/// How long a statement waits for another transaction, if the busy timeout is not set.
const DEFAULT_BUSY: Duration = Duration::from_secs(10);

struct Gate {
    /// Statements reading right now.
    readers: usize,
    /// Whether a statement is writing.
    writing: bool,
}

struct Inner {
    db: RwLock<Database>,
    gate: Mutex<Gate>,
    changed: Condvar,
    busy_timeout: Mutex<Option<Duration>>,
    /// Transactions waiting for another one, and which: to find the ones waiting for each other.
    waits: Mutex<HashMap<u64, u64>>,
}

impl Inner {
    fn gate(&self) -> MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn timeout(&self) -> Duration {
        self.busy_timeout.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or(DEFAULT_BUSY)
    }

    /// Waits until `ready` holds for the gate.
    fn wait_for<'a>(&'a self, mut g: MutexGuard<'a, Gate>, ready: impl Fn(&Gate) -> bool) -> Result<MutexGuard<'a, Gate>> {
        let deadline = Instant::now() + self.timeout();
        while !ready(&g) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::Locked("the database is busy (the busy timeout ran out)".into()));
            }
            g = self.changed.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        Ok(g)
    }

    /// Lets the others go ahead for a moment, waiting for something to change.
    fn pause(&self, until: Instant) {
        let g = self.gate();
        let slice = until.saturating_duration_since(Instant::now()).min(Duration::from_millis(20));
        let _ = self.changed.wait_timeout(g, slice);
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

/// Gives back the write turn when dropped.
struct WriteTurn<'a>(&'a Inner);

impl Drop for WriteTurn<'_> {
    fn drop(&mut self) {
        self.0.gate().writing = false;
        self.0.changed.notify_all();
    }
}

/// A database that several threads can use at once, through [`Connection`]s. Cloning it is cheap
/// and gives another handle to the same database.
#[derive(Clone)]
pub struct SharedDatabase {
    inner: Arc<Inner>,
}

impl SharedDatabase {
    /// Shares `db`. A transaction left open in it is rolled back.
    pub fn new(mut db: Database) -> Self {
        let _ = db.rollback();
        let _ = db.share();
        SharedDatabase {
            inner: Arc::new(Inner {
                db: RwLock::new(db),
                gate: Mutex::new(Gate { readers: 0, writing: false }),
                changed: Condvar::new(),
                busy_timeout: Mutex::new(None),
                waits: Mutex::new(HashMap::new()),
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
        Connection { shared: self.clone(), tx: Mutex::new(None) }
    }

    /// Runs `f` on the database, for tests and tools: it sees the newest state of every table.
    #[doc(hidden)]
    pub fn inspect<R>(&self, f: impl FnOnce(&Database) -> R) -> R {
        let db = self.inner.db.read().unwrap_or_else(|e| e.into_inner());
        f(&db)
    }

    /// Closes the files without the final checkpoint, as if the process had died at this point (see
    /// [`Database::simulate_crash`]). Every connection must have been dropped. For testing recovery.
    #[doc(hidden)]
    pub fn simulate_crash(self) {
        match Arc::try_unwrap(self.inner) {
            Ok(inner) => inner.db.into_inner().unwrap_or_else(|e| e.into_inner()).simulate_crash(),
            Err(_) => panic!("simulate_crash needs every connection to be dropped"),
        }
    }

    /// How long a statement waits for something it needs from another transaction (a row it has
    /// changed but not committed, a turn that does not come) before giving up: with [`Error::Conflict`] in
    /// the first case and [`Error::Locked`] in the second. The default is ten seconds.
    pub fn set_busy_timeout(&self, timeout: Option<Duration>) {
        *self.inner.busy_timeout.lock().unwrap_or_else(|e| e.into_inner()) = timeout;
    }
}

/// One user of a [`SharedDatabase`], with a transaction of its own.
///
/// A connection may be moved to any thread, but use each from one thread at a time: statements run
/// from two threads on one connection would be mixed into one transaction.
pub struct Connection {
    shared: SharedDatabase,
    /// This connection's open transaction, between statements.
    tx: Mutex<Option<Tx>>,
}

fn unavailable() -> Error {
    Error::runtime("the database is unavailable: a thread panicked while using it")
}

impl Connection {
    fn inner(&self) -> &Inner {
        &self.shared.inner
    }

    fn tx(&self) -> MutexGuard<'_, Option<Tx>> {
        self.tx.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// True while this connection has a transaction open.
    pub fn in_transaction(&self) -> bool {
        self.tx().is_some()
    }

    /// Runs `f` with the database borrowed for reading, alongside other readers, seeing what this
    /// connection's transaction sees (or, outside one, everything committed so far).
    pub fn read<R>(&self, f: impl FnOnce(&Database) -> Result<R>) -> Result<R> {
        let inner = self.inner();
        let mut g = inner.wait_for(inner.gate(), |g| !g.writing)?;
        g.readers += 1;
        drop(g);
        let _turn = ReadTurn(inner);
        let db = inner.db.read().map_err(|_| unavailable())?;
        let view = self.tx().as_ref().map(|t| View { snapshot: t.snapshot, txn: t.id });
        match view {
            Some(v) => mvcc::with_view(v, || {
                db.enter_session();
                f(&db)
            }),
            None => {
                db.enter_session();
                f(&db)
            }
        }
    }

    /// Runs `f` with the database borrowed for writing: alone, after the readers have finished, as this
    /// connection's transaction.
    pub fn write<R>(&self, f: impl FnOnce(&mut Database) -> Result<R>) -> Result<R> {
        let inner = self.inner();
        let mut g = inner.wait_for(inner.gate(), |g| !g.writing && g.readers == 0)?;
        g.writing = true;
        drop(g);
        let _turn = WriteTurn(inner);
        let mut db = inner.db.write().map_err(|_| unavailable())?;
        let mut mine = self.tx();
        db.swap_tx(&mut mine);
        db.enter_session();
        let result = f(&mut db);
        db.swap_tx(&mut mine);
        result
    }

    /// Tries `f` again, while it fails because another transaction has an unfinished change it needs to
    /// get past, until the busy timeout.
    fn retrying<R>(&self, mut f: impl FnMut() -> Result<R>) -> Result<R> {
        let until = Instant::now() + self.inner().timeout();
        let result = loop {
            match f() {
                Err(Error::Busy(m)) => {
                    if Instant::now() >= until {
                        break Err(Error::conflict(format!("{m} (waited for the busy timeout)")));
                    }
                    if self.deadlocked() {
                        break Err(Error::conflict(format!("{m} (a deadlock: the other transaction is waiting for this one)")));
                    }
                    self.inner().pause(until);
                }
                other => break other,
            }
        };
        if let Some(id) = self.tx().as_ref().map(|t| t.id) {
            self.inner().waits.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        }
        result
    }

    /// Notes that this connection's transaction waits for the one in its way, and says whether that
    /// one, directly or through others, waits for this: neither could ever go on.
    fn deadlocked(&self) -> bool {
        let (Some(me), Some(blocker)) = (self.tx().as_ref().map(|t| t.id), mvcc::take_blocker()) else { return false };
        let mut waits = self.inner().waits.lock().unwrap_or_else(|e| e.into_inner());
        waits.insert(me, blocker);
        let mut at = blocker;
        for _ in 0..waits.len() + 1 {
            match waits.get(&at) {
                Some(&next) if next == me => return true,
                Some(&next) => at = next,
                None => return false,
            }
        }
        false
    }

    /// Runs every statement in `sql` and returns the result of the last. A `SELECT` runs alongside
    /// other readers; anything else runs alone, for the length of the statement.
    pub fn execute(&self, sql: &str) -> Result<QueryResult> {
        self.retrying(|| {
            let read = self.read(|db| {
                let parsed = db.parse_cached(sql)?;
                if db.is_pure_read(&parsed) {
                    db.run_read(&parsed).map(Some)
                } else {
                    Ok(None)
                }
            })?;
            match read {
                Some(r) => Ok(r),
                None => self.write(|db| db.execute(sql)),
            }
        })
    }

    /// Reads `sql` once, to be run many times with [`execute_prepared`](Connection::execute_prepared).
    pub fn prepare(&self, sql: &str) -> Result<Prepared> {
        self.read(|db| db.prepare(sql))
    }

    /// Runs a prepared statement; a `SELECT` runs alongside other readers.
    pub fn execute_prepared(&self, prepared: &Prepared, params: &[(&str, Value)]) -> Result<QueryResult> {
        self.retrying(|| {
            let read = self.read(|db| {
                if db.is_pure_read(prepared.statements()) {
                    db.query_prepared(prepared, params).map(Some)
                } else {
                    Ok(None)
                }
            })?;
            match read {
                Some(r) => Ok(r),
                None => self.write(|db| db.execute_prepared(prepared, params)),
            }
        })
    }

    /// Folds the log into the snapshot (see [`Database::checkpoint`]). Waits its turn to write; other
    /// connections' unfinished transactions are not part of it.
    pub fn checkpoint(&self) -> Result<()> {
        self.write(|db| db.checkpoint())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if self.in_transaction() {
            let _ = self.write(|db| db.rollback());
        }
    }
}
