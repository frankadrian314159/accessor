//! The C interface to Accessor.
//!
//! Each open database lives on a worker thread of its own, because the engine's `Database` is
//! single-threaded. A connection handle (`acc_db`) sends jobs to that thread and waits for the
//! answer, so a connection can be shared between threads and its calls are carried out one at a
//! time. Results are copied into plain C-friendly cells (`acc_result`) before they leave the worker.
//!
//! Every `extern "C"` function catches panics, checks its pointers for NULL and its text for valid
//! UTF-8, and reports failure through its return code; nothing here unwinds into the caller.

#![allow(non_camel_case_types, clippy::missing_safety_doc)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use accessor_core::{DataType, Database, Durability, Error, Prepared, QueryResult, ReportFormat, Value};

// ----- constants ---------------------------------------------------------------------------------

/// Success.
pub const ACC_OK: c_int = 0;
/// The SQL text could not be read.
pub const ACC_ERR_SYNTAX: c_int = 1;
/// A table, column, index or other object is missing, duplicated or in the way.
pub const ACC_ERR_CATALOG: c_int = 2;
/// A constraint (not null, unique, foreign key, check, trigger `RAISE`) refused the change.
pub const ACC_ERR_CONSTRAINT: c_int = 3;
/// A value has the wrong type or cannot be converted.
pub const ACC_ERR_TYPE: c_int = 4;
/// Any other error while running a statement.
pub const ACC_ERR_RUNTIME: c_int = 5;
/// The operating system refused a file operation.
pub const ACC_ERR_IO: c_int = 6;
/// The database file is damaged.
pub const ACC_ERR_CORRUPT: c_int = 7;
/// Another connection or process has the database open.
pub const ACC_ERR_LOCKED: c_int = 8;
/// A function was called wrongly: NULL or closed handle, index out of range, value not bound.
pub const ACC_ERR_MISUSE: c_int = 9;
/// Text passed in is not valid UTF-8.
pub const ACC_ERR_ENCODING: c_int = 10;
/// A bug in the library (a panic was caught). The connection stays usable.
pub const ACC_ERR_INTERNAL: c_int = 11;
/// Two transactions collided and this one must be retried (only a database shared between
/// transactions raises it).
pub const ACC_ERR_CONFLICT: c_int = 12;

/// A text length meaning "the text is NUL-terminated".
pub const ACC_NTS: usize = usize::MAX; // (the header defines it by hand: cbindgen cannot evaluate usize::MAX)

/// Type tags of the values in a result (`acc_value_type`).
pub const ACC_TYPE_NULL: c_int = 0;
/// `INTEGER`: read with `acc_value_int64`.
pub const ACC_TYPE_INTEGER: c_int = 1;
/// `FLOAT`, `DOUBLE`: read with `acc_value_double`.
pub const ACC_TYPE_DOUBLE: c_int = 2;
/// `TEXT`: read with `acc_value_text`.
pub const ACC_TYPE_TEXT: c_int = 3;
/// `YESNO`: `acc_value_int64` is 0 or 1.
pub const ACC_TYPE_BOOL: c_int = 4;
/// `DECIMAL`: exact; read the digits with `acc_value_text`.
pub const ACC_TYPE_DECIMAL: c_int = 5;
/// `BIGNUM`: whole number of any size; read the digits with `acc_value_text`.
pub const ACC_TYPE_BIGNUM: c_int = 6;
/// `CURRENCY`: exact; read the digits (without a currency sign) with `acc_value_text`.
pub const ACC_TYPE_CURRENCY: c_int = 7;
/// `RATIONAL`: an exact fraction such as `1/3`, read with `acc_value_text`.
pub const ACC_TYPE_RATIONAL: c_int = 8;
/// `DATETIME`: `acc_value_int64` is seconds since 1970-01-01 00:00:00 with no time zone; text is `YYYY-MM-DD HH:MM:SS`.
pub const ACC_TYPE_DATETIME: c_int = 9;
/// `TIMESTAMPTZ`: `acc_value_int64` is seconds since the Unix epoch in UTC; text is shown in the session time zone.
pub const ACC_TYPE_TIMESTAMPTZ: c_int = 10;
/// `INTERVAL`: read as text, such as `1 mon 2 days 03:00:00`.
pub const ACC_TYPE_INTERVAL: c_int = 11;
/// `JSON`: read the document with `acc_value_text`.
pub const ACC_TYPE_JSON: c_int = 12;

/// `acc_set_durability` modes.
pub const ACC_DURABILITY_FSYNC: c_int = 0;
/// Hand data to the operating system on commit: survives a crash of the program, not of the machine.
pub const ACC_DURABILITY_OS_BUFFERED: c_int = 1;

/// `acc_render_report` formats.
pub const ACC_REPORT_TEXT: c_int = 0;
pub const ACC_REPORT_HTML: c_int = 1;
pub const ACC_REPORT_MARKDOWN: c_int = 2;
pub const ACC_REPORT_CSV: c_int = 3;
pub const ACC_REPORT_PDF: c_int = 4;

/// Version of this interface; changes only when an existing function changes meaning.
const ABI_VERSION: c_int = 1;

// ----- errors ------------------------------------------------------------------------------------

struct Failure {
    code: c_int,
    msg: String,
}

impl Failure {
    fn new(code: c_int, msg: impl Into<String>) -> Failure {
        Failure { code, msg: msg.into() }
    }
    fn misuse(msg: impl Into<String>) -> Failure {
        Failure::new(ACC_ERR_MISUSE, msg)
    }
}

impl From<Error> for Failure {
    fn from(e: Error) -> Failure {
        let code = match &e {
            Error::Syntax(_) => ACC_ERR_SYNTAX,
            Error::Catalog(_) => ACC_ERR_CATALOG,
            Error::Constraint(_) => ACC_ERR_CONSTRAINT,
            Error::Type(_) => ACC_ERR_TYPE,
            Error::Runtime(_) => ACC_ERR_RUNTIME,
            Error::Io(_) => ACC_ERR_IO,
            Error::Corrupt(_) => ACC_ERR_CORRUPT,
            Error::Locked(_) => ACC_ERR_LOCKED,
            Error::Conflict(_) | Error::Busy(_) => ACC_ERR_CONFLICT,
        };
        Failure { code, msg: e.to_string() }
    }
}

type Outcome<T> = Result<T, Failure>;

fn c_string(s: &str) -> CString {
    CString::new(s.replace('\0', " ")).expect("NULs were removed")
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ----- the worker --------------------------------------------------------------------------------

struct Worker {
    db: Option<Database>,
    prepared: HashMap<u64, Prepared>,
    next_id: u64,
    closing: bool,
}

type Job = Box<dyn FnOnce(&mut Worker) + Send>;

// A one-value reply from the worker. It is a Mutex and a Condvar rather than a channel because a
// channel's `recv` registers the *calling* thread with the Rust runtime (`thread::current`), which
// leaves a small allocation behind on a thread the C program owns and never lets us clean up.
enum Slot<T> {
    Waiting,
    Done(T),
    Abandoned,
}

struct Reply<T>(Mutex<Slot<T>>, Condvar);

impl<T> Reply<T> {
    fn new() -> Arc<Reply<T>> {
        Arc::new(Reply(Mutex::new(Slot::Waiting), Condvar::new()))
    }

    /// Waits for the answer; `None` if the sender went away without giving one.
    fn wait(&self) -> Option<T> {
        let mut g = lock(&self.0);
        loop {
            match std::mem::replace(&mut *g, Slot::Waiting) {
                Slot::Done(v) => return Some(v),
                Slot::Abandoned => return None,
                Slot::Waiting => g = self.1.wait(g).unwrap_or_else(|e| e.into_inner()),
            }
        }
    }
}

/// The sending half: gives the answer, or marks the reply abandoned when dropped without one.
struct ReplyTx<T>(Arc<Reply<T>>, bool);

impl<T> ReplyTx<T> {
    fn send(mut self, v: T) {
        *lock(&self.0 .0) = Slot::Done(v);
        self.1 = true;
        self.0 .1.notify_one();
    }
}

impl<T> Drop for ReplyTx<T> {
    fn drop(&mut self) {
        if !self.1 {
            *lock(&self.0 .0) = Slot::Abandoned;
            self.0 .1.notify_one();
        }
    }
}

impl Worker {
    fn db(&mut self) -> Outcome<&mut Database> {
        self.db.as_mut().ok_or_else(|| Failure::misuse("the connection is closed"))
    }
}

struct Shared {
    tx: Mutex<Option<Sender<Job>>>,
    last: Mutex<(c_int, CString)>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Shared {
    /// Runs `f` on the worker thread and waits for its answer.
    fn run<R: Send + 'static>(&self, f: impl FnOnce(&mut Worker) -> Outcome<R> + Send + 'static) -> Outcome<R> {
        let reply = Reply::new();
        let reply_tx = ReplyTx(Arc::clone(&reply), false);
        let job: Job = Box::new(move |w| {
            let r = catch_unwind(AssertUnwindSafe(|| f(w)));
            reply_tx.send(r);
        });
        {
            let guard = lock(&self.tx);
            let Some(tx) = guard.as_ref() else { return Err(Failure::misuse("the connection is closed")) };
            tx.send(job).map_err(|_| Failure::new(ACC_ERR_INTERNAL, "the database thread has stopped"))?;
        }
        match reply.wait() {
            Some(Ok(r)) => r,
            Some(Err(_)) => Err(Failure::new(ACC_ERR_INTERNAL, "internal error: the library panicked while running this call")),
            None => Err(Failure::new(ACC_ERR_INTERNAL, "the database thread has stopped")),
        }
    }

    /// Records the outcome as the connection's last error and turns it into a return code.
    fn finish<T>(&self, r: Outcome<T>) -> (c_int, Option<T>) {
        match r {
            Ok(v) => {
                *lock(&self.last) = (ACC_OK, CString::default());
                (ACC_OK, Some(v))
            }
            Err(f) => {
                *lock(&self.last) = (f.code, c_string(&f.msg));
                (f.code, None)
            }
        }
    }

    fn close(&self) {
        let _ = self.run(|w| {
            w.prepared.clear();
            w.db = None; // closes the files and releases the lock
            w.closing = true;
            Ok(())
        });
        lock(&self.tx).take();
        if let Some(h) = lock(&self.thread).take() {
            let _ = h.join();
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        // A connection that was never closed: stop the thread so the database is closed.
        lock(&self.tx).take();
        if let Some(h) = lock(&self.thread).take() {
            let _ = h.join();
        }
    }
}

// ----- handles -----------------------------------------------------------------------------------

/// An open database (a connection).
pub struct acc_db {
    shared: Arc<Shared>,
}

/// A statement read once, to be run many times with different values.
pub struct acc_stmt {
    db: Arc<Shared>,
    id: u64,
    params: Vec<CString>,
    values: Vec<Option<Value>>,
}

/// The rows of a query, copied out of the engine.
pub struct acc_result {
    columns: Vec<CString>,
    rows: usize,
    cells: Vec<Cell>,
    affected: usize,
}

struct Cell {
    tag: c_int,
    int: i64,
    dbl: f64,
    /// UTF-8, with a NUL after the end (not counted in the length).
    text: Option<Box<[u8]>>,
}

impl Cell {
    fn null() -> Cell {
        Cell { tag: ACC_TYPE_NULL, int: 0, dbl: 0.0, text: None }
    }
    fn text_of(tag: c_int, s: &str) -> Cell {
        let mut bytes = Vec::with_capacity(s.len() + 1);
        bytes.extend_from_slice(s.as_bytes());
        bytes.push(0);
        Cell { tag, int: 0, dbl: 0.0, text: Some(bytes.into_boxed_slice()) }
    }
    fn of(v: &Value) -> Cell {
        match v {
            Value::Null => Cell::null(),
            Value::Bool(b) => Cell { tag: ACC_TYPE_BOOL, int: *b as i64, ..Cell::text_of(ACC_TYPE_BOOL, if *b { "true" } else { "false" }) },
            Value::Int(i) => Cell { tag: ACC_TYPE_INTEGER, int: *i, dbl: *i as f64, text: None },
            Value::Float(f) => Cell { tag: ACC_TYPE_DOUBLE, int: *f as i64, dbl: *f as f64, text: None },
            Value::Double(f) => Cell { tag: ACC_TYPE_DOUBLE, int: *f as i64, dbl: *f, text: None },
            Value::Big(b) => Cell::text_of(ACC_TYPE_BIGNUM, &b.to_string()),
            Value::Dec(d) => Cell::text_of(ACC_TYPE_DECIMAL, &d.to_string()),
            // Money is shown with a sign; the interface gives the plain digits.
            Value::Currency(d) => Cell::text_of(ACC_TYPE_CURRENCY, &d.to_string()),
            Value::Rational(r) => Cell::text_of(ACC_TYPE_RATIONAL, &r.to_string()),
            Value::DateTime(t) => Cell { tag: ACC_TYPE_DATETIME, int: *t, ..Cell::text_of(ACC_TYPE_DATETIME, &accessor_core::datetime::format(*t)) },
            Value::TimestampTz(t) => Cell { tag: ACC_TYPE_TIMESTAMPTZ, int: *t, ..Cell::text_of(ACC_TYPE_TIMESTAMPTZ, &v.to_string()) },
            Value::Interval(_) => Cell::text_of(ACC_TYPE_INTERVAL, &v.to_string()),
            Value::Json(s) => Cell::text_of(ACC_TYPE_JSON, s),
            Value::Text(s) => Cell::text_of(ACC_TYPE_TEXT, s),
            #[allow(unreachable_patterns)]
            other => Cell::text_of(ACC_TYPE_TEXT, &other.to_string()),
        }
    }
}

impl acc_result {
    fn from_query(q: QueryResult) -> acc_result {
        let width = q.columns.len();
        let mut cells = Vec::with_capacity(q.rows.len() * width);
        for row in &q.rows {
            for c in 0..width {
                cells.push(row.get(c).map_or_else(Cell::null, Cell::of));
            }
        }
        acc_result { columns: q.columns.iter().map(|c| c_string(c)).collect(), rows: q.rows.len(), cells, affected: q.affected }
    }

    fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        if row < self.rows && col < self.columns.len() {
            self.cells.get(row * self.columns.len() + col)
        } else {
            None
        }
    }
}

// ----- argument helpers --------------------------------------------------------------------------

/// Reads the text argument `(p, len)`.
///
/// # Safety
/// `p` must point to `len` readable bytes, or (for `len == ACC_NTS`) to a NUL-terminated string.
unsafe fn text_arg<'a>(p: *const c_char, len: usize) -> Outcome<&'a str> {
    if p.is_null() {
        return Err(Failure::misuse("a text argument is NULL"));
    }
    let bytes: &[u8] = if len == ACC_NTS {
        CStr::from_ptr(p).to_bytes()
    } else {
        if len > isize::MAX as usize {
            return Err(Failure::misuse("a text length is impossibly large"));
        }
        std::slice::from_raw_parts(p.cast::<u8>(), len)
    };
    std::str::from_utf8(bytes).map_err(|e| Failure::new(ACC_ERR_ENCODING, format!("text is not valid UTF-8 (bad byte at offset {})", e.valid_up_to())))
}

unsafe fn db_ref<'a>(db: *mut acc_db) -> Outcome<&'a Arc<Shared>> {
    match db.as_ref() {
        Some(d) => Ok(&d.shared),
        None => Err(Failure::misuse("the connection handle is NULL")),
    }
}

/// Runs an API function body, turning a panic into `ACC_ERR_INTERNAL`.
fn guarded(f: impl FnOnce() -> c_int) -> c_int {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(ACC_ERR_INTERNAL)
}

// ----- version -----------------------------------------------------------------------------------

/// The library version as text, such as `"0.1.0"`. The string is static.
#[no_mangle]
pub extern "C" fn acc_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// The library version as a number: `major * 10000 + minor * 100 + patch`, so 0.1.0 is 100.
#[no_mangle]
pub extern "C" fn acc_version_number() -> c_int {
    let mut parts = env!("CARGO_PKG_VERSION").split('.').map(|p| p.parse::<c_int>().unwrap_or(0));
    let (major, minor, patch) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    major * 10_000 + minor * 100 + patch
}

/// The version of this C interface. It changes only when an existing function changes meaning, so a
/// program built for ABI version 1 works with any library that reports 1.
#[no_mangle]
pub extern "C" fn acc_abi_version() -> c_int {
    ABI_VERSION
}

/// The version of the database file format this library writes. It reads older ones too, and a
/// file written by this library cannot be read by an older library.
#[no_mangle]
pub extern "C" fn acc_file_format_version() -> c_int {
    accessor_core::FILE_FORMAT_VERSION as c_int
}

// ----- connections -------------------------------------------------------------------------------

fn start(path: Option<String>) -> Result<Arc<Shared>, (Arc<Shared>, Failure)> {
    let shared = Arc::new(Shared { tx: Mutex::new(None), last: Mutex::new((ACC_OK, CString::default())), thread: Mutex::new(None) });
    let (tx, rx) = mpsc::channel::<Job>();
    let ready = Reply::<Outcome<()>>::new();
    let ready_tx = ReplyTx(Arc::clone(&ready), false);
    let spawned = std::thread::Builder::new().name("accessor-db".into()).spawn(move || {
        // The database is made on this thread and never leaves it.
        let opened = match &path {
            Some(p) => Database::open(p),
            None => Ok(Database::in_memory()),
        };
        match opened {
            Err(e) => ready_tx.send(Err(e.into())),
            Ok(db) => {
                ready_tx.send(Ok(()));
                let mut w = Worker { db: Some(db), prepared: HashMap::new(), next_id: 1, closing: false };
                while let Ok(job) = rx.recv() {
                    job(&mut w);
                    if w.closing {
                        break;
                    }
                }
            }
        }
    });
    let handle = match spawned {
        Ok(h) => h,
        Err(e) => return Err((shared, Failure::new(ACC_ERR_RUNTIME, format!("cannot start the database thread: {e}")))),
    };
    match ready.wait() {
        Some(Ok(())) => {
            *lock(&shared.tx) = Some(tx);
            *lock(&shared.thread) = Some(handle);
            Ok(shared)
        }
        Some(Err(f)) => {
            let _ = handle.join();
            Err((shared, f))
        }
        None => {
            let _ = handle.join();
            Err((shared, Failure::new(ACC_ERR_INTERNAL, "the database thread stopped while opening")))
        }
    }
}

/// Opens (creating it if needed) the database file `path`, or a temporary in-memory database if
/// `path` is NULL or `":memory:"`, and stores the connection in `*out`.
///
/// `*out` is set whenever a handle could be made, **even if opening failed**; check the return value
/// and, on failure, read `acc_errmsg(*out)` and then close it. A database file can be open in only one
/// connection at a time, in this process or any other: a second open fails with `ACC_ERR_LOCKED`.
///
/// Returns `ACC_OK`, `ACC_ERR_IO`, `ACC_ERR_CORRUPT`, `ACC_ERR_LOCKED`, `ACC_ERR_ENCODING` or
/// `ACC_ERR_MISUSE` (`out` is NULL).
#[no_mangle]
pub unsafe extern "C" fn acc_open(path: *const c_char, out: *mut *mut acc_db) -> c_int {
    guarded(|| {
        if out.is_null() {
            return ACC_ERR_MISUSE;
        }
        *out = ptr::null_mut();
        let path = if path.is_null() {
            Ok(None)
        } else {
            text_arg(path, ACC_NTS).map(|p| if p == ":memory:" { None } else { Some(p.to_string()) })
        };
        let (shared, outcome) = match path {
            Err(f) => {
                let shared = Arc::new(Shared { tx: Mutex::new(None), last: Mutex::new((f.code, c_string(&f.msg))), thread: Mutex::new(None) });
                (shared, Err(f))
            }
            Ok(p) => match start(p) {
                Ok(s) => (s, Ok(())),
                Err((s, f)) => (s, Err(f)),
            },
        };
        let (code, _) = shared.finish(outcome);
        *out = Box::into_raw(Box::new(acc_db { shared }));
        code
    })
}

/// Closes the connection: the database is checkpointed, its files are closed and its lock is
/// released before this returns. Statements made from it can no longer be run (they fail with
/// `ACC_ERR_MISUSE`) but must still be freed. NULL is accepted.
#[no_mangle]
pub unsafe extern "C" fn acc_close(db: *mut acc_db) {
    let _ = guarded(|| {
        if !db.is_null() {
            let db = Box::from_raw(db);
            db.shared.close();
        }
        ACC_OK
    });
}

/// The code of the last call on this connection (`ACC_OK` if it succeeded), or `ACC_ERR_MISUSE` for NULL.
#[no_mangle]
pub unsafe extern "C" fn acc_errcode(db: *mut acc_db) -> c_int {
    guarded(|| match db_ref(db) {
        Ok(s) => lock(&s.last).0,
        Err(_) => ACC_ERR_MISUSE,
    })
}

/// The message of the last failure on this connection; an empty string if the last call succeeded.
/// The string is owned by the connection and valid until the next call on it.
#[no_mangle]
pub unsafe extern "C" fn acc_errmsg(db: *mut acc_db) -> *const c_char {
    catch_unwind(AssertUnwindSafe(|| match db_ref(db) {
        Ok(s) => lock(&s.last).1.as_ptr(),
        Err(_) => c"the connection handle is NULL".as_ptr(),
    }))
    .unwrap_or(c"internal error".as_ptr())
}

/// Runs one or more SQL statements separated by `;`. If `out` is not NULL it receives the result of
/// the last statement (columns and rows for a query, a count for a change) and must be freed with
/// `acc_result_free`; on failure `*out` is set to NULL.
#[no_mangle]
pub unsafe extern "C" fn acc_exec(db: *mut acc_db, sql: *const c_char, sql_len: usize, out: *mut *mut acc_result) -> c_int {
    guarded(|| {
        if !out.is_null() {
            *out = ptr::null_mut();
        }
        let shared = match db_ref(db) {
            Ok(s) => s,
            Err(f) => return f.code,
        };
        let sql = match text_arg(sql, sql_len) {
            Ok(s) => s.to_string(),
            Err(f) => return shared.finish::<()>(Err(f)).0,
        };
        let r = shared.run(move |w| {
            let db = w.db()?;
            let q = db.execute(&sql)?;
            Ok(acc_result::from_query(q))
        });
        let (code, result) = shared.finish(r);
        if let (Some(res), false) = (result, out.is_null()) {
            *out = Box::into_raw(Box::new(res));
        }
        code
    })
}

/// Writes the log into the database file now and empties it.
#[no_mangle]
pub unsafe extern "C" fn acc_checkpoint(db: *mut acc_db) -> c_int {
    simple(db, |d| d.checkpoint().map_err(Failure::from))
}

/// Turns the query optimizer on (non-zero) or off (zero). Answers are the same either way.
#[no_mangle]
pub unsafe extern "C" fn acc_set_optimizer(db: *mut acc_db, on: c_int) -> c_int {
    simple(db, move |d| {
        d.set_optimizer(on != 0);
        Ok(())
    })
}

/// Chooses how commits reach the disk: `ACC_DURABILITY_FSYNC` (the default) or `ACC_DURABILITY_OS_BUFFERED`.
#[no_mangle]
pub unsafe extern "C" fn acc_set_durability(db: *mut acc_db, mode: c_int) -> c_int {
    let d = match mode {
        ACC_DURABILITY_FSYNC => Durability::Fsync,
        ACC_DURABILITY_OS_BUFFERED => Durability::OsBuffered,
        _ => {
            return match db_ref(db) {
                Ok(s) => s.finish::<()>(Err(Failure::misuse("unknown durability mode"))).0,
                Err(f) => f.code,
            }
        }
    };
    simple(db, move |db| {
        db.set_durability(d);
        Ok(())
    })
}

/// 1 if a transaction is open on this connection, 0 if not, negative on error.
#[no_mangle]
pub unsafe extern "C" fn acc_in_transaction(db: *mut acc_db) -> c_int {
    let mut answer = 0;
    let code = guarded(|| {
        let shared = match db_ref(db) {
            Ok(s) => s,
            Err(f) => return f.code,
        };
        let (code, v) = shared.finish(shared.run(|w| Ok(w.db()?.in_transaction())));
        answer = v.map_or(0, |b| b as c_int);
        code
    });
    if code == ACC_OK {
        answer
    } else {
        -code
    }
}

unsafe fn simple(db: *mut acc_db, f: impl FnOnce(&mut Database) -> Outcome<()> + Send + 'static) -> c_int {
    guarded(|| {
        let shared = match db_ref(db) {
            Ok(s) => s,
            Err(f) => return f.code,
        };
        shared.finish(shared.run(move |w| f(w.db()?))).0
    })
}

/// Renders the stored report `name` (made with `CREATE REPORT`) as a document of the given format
/// (`ACC_REPORT_*`). The bytes are UTF-8 text except for `ACC_REPORT_PDF`. On success `*out` and
/// `*out_len` describe a buffer that must be released with `acc_buffer_free`.
#[no_mangle]
pub unsafe extern "C" fn acc_render_report(
    db: *mut acc_db,
    name: *const c_char,
    name_len: usize,
    format: c_int,
    out: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    guarded(|| {
        if out.is_null() || out_len.is_null() {
            return ACC_ERR_MISUSE;
        }
        *out = ptr::null_mut();
        *out_len = 0;
        let shared = match db_ref(db) {
            Ok(s) => s,
            Err(f) => return f.code,
        };
        let name = match text_arg(name, name_len) {
            Ok(s) => s.to_string(),
            Err(f) => return shared.finish::<()>(Err(f)).0,
        };
        let fmt = match format {
            ACC_REPORT_TEXT => ReportFormat::Text,
            ACC_REPORT_HTML => ReportFormat::Html,
            ACC_REPORT_MARKDOWN => ReportFormat::Markdown,
            ACC_REPORT_CSV => ReportFormat::Csv,
            ACC_REPORT_PDF => ReportFormat::Pdf,
            _ => return shared.finish::<()>(Err(Failure::misuse("unknown report format"))).0,
        };
        let r = shared.run(move |w| w.db()?.render_report(&name, fmt).map_err(Failure::from));
        let (code, bytes) = shared.finish(r);
        if let Some(b) = bytes {
            let boxed = b.into_boxed_slice();
            *out_len = boxed.len();
            *out = Box::into_raw(boxed).cast::<u8>();
        }
        code
    })
}

/// Releases a buffer returned by `acc_render_report`. NULL is accepted.
#[no_mangle]
pub unsafe extern "C" fn acc_buffer_free(buf: *mut u8, len: usize) {
    let _ = guarded(|| {
        if !buf.is_null() {
            drop(Box::from_raw(ptr::slice_from_raw_parts_mut(buf, len)));
        }
        ACC_OK
    });
}

// ----- prepared statements -----------------------------------------------------------------------

/// Reads `sql` (one or more statements) once, to be run many times with `acc_stmt_run`. Values are
/// supplied for the parameters written `@name`; `acc_stmt_param_count` and `acc_stmt_param_name`
/// list them in order of first appearance, and the `acc_bind_*` functions set them by that position.
/// On failure `*out` is NULL.
#[no_mangle]
pub unsafe extern "C" fn acc_prepare(db: *mut acc_db, sql: *const c_char, sql_len: usize, out: *mut *mut acc_stmt) -> c_int {
    guarded(|| {
        if out.is_null() {
            return ACC_ERR_MISUSE;
        }
        *out = ptr::null_mut();
        let shared = match db_ref(db) {
            Ok(s) => s,
            Err(f) => return f.code,
        };
        let sql = match text_arg(sql, sql_len) {
            Ok(s) => s.to_string(),
            Err(f) => return shared.finish::<()>(Err(f)).0,
        };
        let r = shared.run(move |w| {
            let p = w.db()?.prepare(&sql)?;
            let params: Vec<String> = p.parameters().to_vec();
            let id = w.next_id;
            w.next_id += 1;
            w.prepared.insert(id, p);
            Ok((id, params))
        });
        let (code, made) = shared.finish(r);
        if let Some((id, params)) = made {
            let n = params.len();
            *out = Box::into_raw(Box::new(acc_stmt {
                db: Arc::clone(shared),
                id,
                params: params.iter().map(|p| c_string(p)).collect(),
                values: vec![None; n],
            }));
        }
        code
    })
}

/// Releases a statement. NULL is accepted. It is fine to free statements after their connection was closed.
#[no_mangle]
pub unsafe extern "C" fn acc_stmt_free(stmt: *mut acc_stmt) {
    let _ = guarded(|| {
        if !stmt.is_null() {
            let s = Box::from_raw(stmt);
            let id = s.id;
            // Fire and forget: if the connection is closed there is nothing to release.
            let _ = s.db.run(move |w| {
                w.prepared.remove(&id);
                Ok(())
            });
        }
        ACC_OK
    });
}

unsafe fn stmt_mut<'a>(stmt: *mut acc_stmt) -> Outcome<&'a mut acc_stmt> {
    stmt.as_mut().ok_or_else(|| Failure::misuse("the statement handle is NULL"))
}

/// The number of parameters (`@name`) the statement uses; negative on error.
#[no_mangle]
pub unsafe extern "C" fn acc_stmt_param_count(stmt: *mut acc_stmt) -> c_int {
    catch_unwind(AssertUnwindSafe(|| stmt_mut(stmt).map_or(-ACC_ERR_MISUSE, |s| s.params.len() as c_int))).unwrap_or(-ACC_ERR_INTERNAL)
}

/// The name of parameter `index` (from 0), without the `@`; NULL if out of range. Valid until the statement is freed.
#[no_mangle]
pub unsafe extern "C" fn acc_stmt_param_name(stmt: *mut acc_stmt, index: c_int) -> *const c_char {
    catch_unwind(AssertUnwindSafe(|| match stmt_mut(stmt) {
        Ok(s) if index >= 0 => s.params.get(index as usize).map_or(ptr::null(), |p| p.as_ptr()),
        _ => ptr::null(),
    }))
    .unwrap_or(ptr::null())
}

/// The position of the parameter called `name` (case-insensitive, with or without `@`), or -1.
#[no_mangle]
pub unsafe extern "C" fn acc_stmt_param_index(stmt: *mut acc_stmt, name: *const c_char, name_len: usize) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        let (Ok(s), Ok(n)) = (stmt_mut(stmt), text_arg(name, name_len)) else { return -1 };
        let n = n.trim_start_matches('@');
        s.params.iter().position(|p| p.to_str().is_ok_and(|p| p.eq_ignore_ascii_case(n))).map_or(-1, |i| i as c_int)
    }))
    .unwrap_or(-1)
}

unsafe fn bind(stmt: *mut acc_stmt, index: c_int, value: impl FnOnce() -> Outcome<Value>) -> c_int {
    guarded(|| {
        let s = match stmt_mut(stmt) {
            Ok(s) => s,
            Err(f) => return f.code,
        };
        let r = (|| {
            let slot = usize::try_from(index)
                .ok()
                .and_then(|i| s.values.get_mut(i))
                .ok_or_else(|| Failure::misuse(format!("parameter index {index} is out of range (the statement has {})", s.params.len())))?;
            *slot = Some(value()?);
            Ok(())
        })();
        s.db.finish(r).0
    })
}

/// Sets parameter `index` (from 0) to NULL.
#[no_mangle]
pub unsafe extern "C" fn acc_bind_null(stmt: *mut acc_stmt, index: c_int) -> c_int {
    bind(stmt, index, || Ok(Value::Null))
}

/// Sets a parameter to a whole number.
#[no_mangle]
pub unsafe extern "C" fn acc_bind_int64(stmt: *mut acc_stmt, index: c_int, v: i64) -> c_int {
    bind(stmt, index, || Ok(Value::Int(v)))
}

/// Sets a parameter to a floating-point number. NaN and infinity are refused (`ACC_ERR_TYPE`).
#[no_mangle]
pub unsafe extern "C" fn acc_bind_double(stmt: *mut acc_stmt, index: c_int, v: f64) -> c_int {
    bind(stmt, index, || Value::from_f64(v).ok_or_else(|| Failure::new(ACC_ERR_TYPE, "NaN and infinity cannot be stored")))
}

/// Sets a parameter to yes/no (non-zero is yes).
#[no_mangle]
pub unsafe extern "C" fn acc_bind_bool(stmt: *mut acc_stmt, index: c_int, v: c_int) -> c_int {
    bind(stmt, index, || Ok(Value::Bool(v != 0)))
}

/// Sets a parameter to text.
#[no_mangle]
pub unsafe extern "C" fn acc_bind_text(stmt: *mut acc_stmt, index: c_int, text: *const c_char, len: usize) -> c_int {
    bind(stmt, index, || Ok(Value::text(text_arg(text, len)?)))
}

fn exact(ty: DataType, what: &str, s: &str) -> Outcome<Value> {
    Value::text(s.trim()).coerce_to(ty).map_err(|_| Failure::new(ACC_ERR_TYPE, format!("'{s}' is not a valid {what}")))
}

/// Sets a parameter to an exact decimal number given as digits (`"-12.50"`, `"1e3"`).
#[no_mangle]
pub unsafe extern "C" fn acc_bind_decimal(stmt: *mut acc_stmt, index: c_int, text: *const c_char, len: usize) -> c_int {
    bind(stmt, index, || exact(DataType::Decimal, "decimal number", text_arg(text, len)?))
}

/// Sets a parameter to a whole number of any size given as digits.
#[no_mangle]
pub unsafe extern "C" fn acc_bind_bignum(stmt: *mut acc_stmt, index: c_int, text: *const c_char, len: usize) -> c_int {
    bind(stmt, index, || {
        let s = text_arg(text, len)?;
        // Converting would round "1.5"; a whole number must be written as one.
        let digits = s.trim().strip_prefix(['-', '+']).unwrap_or(s.trim());
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Failure::new(ACC_ERR_TYPE, format!("'{s}' is not a whole number")));
        }
        exact(DataType::Bignum, "whole number", s)
    })
}

/// Sets a parameter to a date and time with no time zone, as seconds since 1970-01-01 00:00:00.
#[no_mangle]
pub unsafe extern "C" fn acc_bind_datetime(stmt: *mut acc_stmt, index: c_int, seconds: i64) -> c_int {
    bind(stmt, index, || Ok(Value::DateTime(seconds)))
}

/// Sets a parameter to a moment in time, as seconds since the Unix epoch in UTC.
#[no_mangle]
pub unsafe extern "C" fn acc_bind_timestamptz(stmt: *mut acc_stmt, index: c_int, seconds: i64) -> c_int {
    bind(stmt, index, || Ok(Value::TimestampTz(seconds)))
}

/// Forgets every value set with `acc_bind_*`.
#[no_mangle]
pub unsafe extern "C" fn acc_stmt_clear_bindings(stmt: *mut acc_stmt) -> c_int {
    guarded(|| match stmt_mut(stmt) {
        Ok(s) => {
            s.values.iter_mut().for_each(|v| *v = None);
            ACC_OK
        }
        Err(f) => f.code,
    })
}

/// Runs the statement with the values bound so far (every parameter must have one, even if NULL).
/// If `out` is not NULL it receives the result of the last statement, to be freed with
/// `acc_result_free`; on failure `*out` is set to NULL. The bindings are kept for the next run.
#[no_mangle]
pub unsafe extern "C" fn acc_stmt_run(stmt: *mut acc_stmt, out: *mut *mut acc_result) -> c_int {
    guarded(|| {
        if !out.is_null() {
            *out = ptr::null_mut();
        }
        let s = match stmt_mut(stmt) {
            Ok(s) => s,
            Err(f) => return f.code,
        };
        let mut args: Vec<(String, Value)> = Vec::with_capacity(s.params.len());
        for (name, value) in s.params.iter().zip(&s.values) {
            match value {
                Some(v) => args.push((name.to_string_lossy().into_owned(), v.clone())),
                None => {
                    let f = Failure::misuse(format!("parameter @{} has no value; bind one (NULL is a value)", name.to_string_lossy()));
                    return s.db.finish::<()>(Err(f)).0;
                }
            }
        }
        let id = s.id;
        let r = s.db.run(move |w| {
            let p = w.prepared.get(&id).cloned().ok_or_else(|| Failure::misuse("the statement was released"))?;
            let refs: Vec<(&str, Value)> = args.iter().map(|(n, v)| (n.as_str(), v.clone())).collect();
            let q = w.db()?.execute_prepared(&p, &refs)?;
            Ok(acc_result::from_query(q))
        });
        let (code, result) = s.db.finish(r);
        if let (Some(res), false) = (result, out.is_null()) {
            *out = Box::into_raw(Box::new(res));
        }
        code
    })
}

// ----- results -----------------------------------------------------------------------------------

/// Releases a result and everything that was returned from it. NULL is accepted.
#[no_mangle]
pub unsafe extern "C" fn acc_result_free(res: *mut acc_result) {
    let _ = guarded(|| {
        if !res.is_null() {
            drop(Box::from_raw(res));
        }
        ACC_OK
    });
}

/// The number of rows; 0 for NULL.
#[no_mangle]
pub unsafe extern "C" fn acc_result_row_count(res: *const acc_result) -> usize {
    res.as_ref().map_or(0, |r| r.rows)
}

/// The number of columns; 0 for NULL or for a statement that returns no rows.
#[no_mangle]
pub unsafe extern "C" fn acc_result_column_count(res: *const acc_result) -> usize {
    res.as_ref().map_or(0, |r| r.columns.len())
}

/// The number of rows an `INSERT`, `UPDATE` or `DELETE` changed (also the row count of a `RETURNING`).
#[no_mangle]
pub unsafe extern "C" fn acc_result_affected(res: *const acc_result) -> usize {
    res.as_ref().map_or(0, |r| r.affected)
}

/// The name of column `col` (from 0), or NULL if out of range. Valid until the result is freed.
#[no_mangle]
pub unsafe extern "C" fn acc_result_column_name(res: *const acc_result, col: usize) -> *const c_char {
    res.as_ref().and_then(|r| r.columns.get(col)).map_or(ptr::null(), |c| c.as_ptr())
}

/// The type tag (`ACC_TYPE_*`) of the value at row `row`, column `col` (both from 0); `ACC_TYPE_NULL` if
/// the value is NULL or the position is out of range.
#[no_mangle]
pub unsafe extern "C" fn acc_value_type(res: *const acc_result, row: usize, col: usize) -> c_int {
    res.as_ref().and_then(|r| r.cell(row, col)).map_or(ACC_TYPE_NULL, |c| c.tag)
}

/// The value as a 64-bit integer: exact for `INTEGER`, `YESNO` (0 or 1), `DATETIME` and `TIMESTAMPTZ`
/// (seconds); a floating-point value is truncated; 0 for NULL, out of range, and types read as text.
#[no_mangle]
pub unsafe extern "C" fn acc_value_int64(res: *const acc_result, row: usize, col: usize) -> i64 {
    res.as_ref().and_then(|r| r.cell(row, col)).map_or(0, |c| c.int)
}

/// The value as a floating-point number: exact for `FLOAT`/`DOUBLE`, converted for `INTEGER`; 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn acc_value_double(res: *const acc_result, row: usize, col: usize) -> f64 {
    res.as_ref().and_then(|r| r.cell(row, col)).map_or(0.0, |c| c.dbl)
}

/// The text of the value, NUL-terminated, with its length in bytes in `*len` (which may be NULL).
/// Text values may contain NUL bytes, so use the length. Returns NULL for NULL, for out-of-range
/// positions and for `INTEGER` and `DOUBLE` values (read those with the numeric functions). Valid
/// until the result is freed.
#[no_mangle]
pub unsafe extern "C" fn acc_value_text(res: *const acc_result, row: usize, col: usize, len: *mut usize) -> *const c_char {
    let text = res.as_ref().and_then(|r| r.cell(row, col)).and_then(|c| c.text.as_deref());
    if !len.is_null() {
        *len = text.map_or(0, |t| t.len() - 1);
    }
    text.map_or(ptr::null(), |t| t.as_ptr().cast())
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn exec(db: *mut acc_db, sql: &str) -> (c_int, *mut acc_result) {
        let mut res = ptr::null_mut();
        let code = acc_exec(db, sql.as_ptr().cast(), sql.len(), &mut res);
        (code, res)
    }

    #[test]
    fn version_and_basics() {
        unsafe {
            assert_eq!(CStr::from_ptr(acc_version()).to_str().unwrap(), env!("CARGO_PKG_VERSION"));
            assert_eq!(acc_version_number(), 100);
            assert_eq!(acc_abi_version(), 1);
            let mut db = ptr::null_mut();
            assert_eq!(acc_open(ptr::null(), &mut db), ACC_OK);
            assert_eq!(exec(db, "CREATE TABLE t (n INTEGER, s TEXT)").0, ACC_OK);
            assert_eq!(exec(db, "INSERT INTO t VALUES (1, 'a'), (2, NULL)").0, ACC_OK);
            let (code, res) = exec(db, "SELECT n, s FROM t ORDER BY n");
            assert_eq!(code, ACC_OK);
            assert_eq!((acc_result_row_count(res), acc_result_column_count(res)), (2, 2));
            assert_eq!(acc_value_int64(res, 1, 0), 2);
            assert_eq!(acc_value_type(res, 1, 1), ACC_TYPE_NULL);
            let mut len = 0;
            let p = acc_value_text(res, 0, 1, &mut len);
            assert_eq!((CStr::from_ptr(p).to_str().unwrap(), len), ("a", 1));
            acc_result_free(res);
            acc_close(db);
        }
    }

    #[test]
    fn errors_have_codes_and_messages() {
        unsafe {
            let mut db = ptr::null_mut();
            acc_open(c":memory:".as_ptr(), &mut db);
            assert_eq!(exec(db, "SELEKT 1").0, ACC_ERR_SYNTAX);
            assert!(CStr::from_ptr(acc_errmsg(db)).to_str().unwrap().contains("Syntax"));
            assert_eq!(exec(db, "SELECT * FROM nothing").0, ACC_ERR_CATALOG);
            assert_eq!(acc_exec(db, ptr::null(), 0, ptr::null_mut()), ACC_ERR_MISUSE);
            assert_eq!(acc_exec(db, b"\xff\xfe".as_ptr().cast(), 2, ptr::null_mut()), ACC_ERR_ENCODING);
            assert_eq!(exec(db, "SELECT 1").0, ACC_OK);
            assert_eq!(acc_errcode(db), ACC_OK);
            acc_close(db);
            // After close the handle is gone; a second connection is unaffected.
            assert_eq!(acc_exec(ptr::null_mut(), c"SELECT 1".as_ptr(), ACC_NTS, ptr::null_mut()), ACC_ERR_MISUSE);
        }
    }
}
