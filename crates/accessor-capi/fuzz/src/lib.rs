//! What the fuzz targets share: a connection to a small database, and the checks that every call
//! must satisfy whatever it is given.
//!
//! The properties checked are the ones the C interface promises:
//!
//! * a call returns normally, with a code that is one of the documented ones;
//! * the code is never `ACC_ERR_INTERNAL`, which means a panic was caught (a bug in the library, even
//!   though the interface contained it);
//! * text that comes back is valid UTF-8, readable up to its length, and NUL-terminated;
//! * the connection still works afterwards.

#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};
use std::ptr;

pub use accessor_capi::*;

/// SQL that is not run: it can ask the engine for unbounded amounts of memory or time, which is a
/// different kind of problem from the ones fuzzing looks for here (the engine has no resource limits).
const RESOURCE_HOGS: &[&str] = &["generate_series", "repeat", "replicate", "lpad", "rpad", "recursive", "json_each", "split_string"];

pub fn too_expensive(sql: &[u8]) -> bool {
    let lower = String::from_utf8_lossy(sql).to_ascii_lowercase();
    RESOURCE_HOGS.iter().any(|w| lower.contains(w))
}

pub struct Conn {
    pub db: *mut acc_db,
    uses: u32,
}

impl Conn {
    fn open() -> Conn {
        let mut db = ptr::null_mut();
        unsafe {
            assert_eq!(acc_open(ptr::null(), &mut db), ACC_OK);
            run(
                db,
                "CREATE TABLE t (i INTEGER, d DOUBLE, s TEXT, b YESNO, dec DECIMAL, big BIGNUM, dt DATETIME, tz TIMESTAMPTZ, js JSON);\
                 INSERT INTO t VALUES (1, 1.5, 'one', TRUE, 1.25, 12345678901234567890123, #2024-03-05 14:30:00#, \
                   TIMESTAMP WITH TIME ZONE '2024-03-05 14:30:00+01:00', '{\"a\":[1,2]}');\
                 INSERT INTO t (i) VALUES (NULL);\
                 CREATE REPORT r TITLE 'T' BREAK BY b COLUMN i SUM GRAND TOTAL AS SELECT b, i FROM t;",
            );
        }
        Conn { db, uses: 0 }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        unsafe { acc_close(self.db) };
    }
}

thread_local! {
    static CONN: RefCell<Option<Conn>> = const { RefCell::new(None) };
}

/// Runs `f` on a connection that is renewed now and then, so that tables made by the fuzzed SQL
/// cannot pile up.
pub fn with_conn<R>(f: impl FnOnce(*mut acc_db) -> R) -> R {
    CONN.with(|c| {
        let mut c = c.borrow_mut();
        if c.as_ref().is_none_or(|x| x.uses >= 1000) {
            *c = Some(Conn::open());
        }
        let conn = c.as_mut().expect("just made");
        conn.uses += 1;
        let db = conn.db;
        let r = f(db);
        unsafe { assert_healthy(db) };
        r
    })
}

pub unsafe fn run(db: *mut acc_db, sql: &str) {
    let code = acc_exec(db, sql.as_ptr().cast(), sql.len(), ptr::null_mut());
    assert_eq!(code, ACC_OK, "setup SQL failed: {sql}: {}", message(db));
}

pub unsafe fn message(db: *mut acc_db) -> String {
    CStr::from_ptr(acc_errmsg(db)).to_string_lossy().into_owned()
}

/// Every code a call may return, and never an internal error.
pub unsafe fn ok_code(db: *mut acc_db, code: c_int, what: &str) {
    assert!(
        (ACC_OK..=ACC_ERR_ENCODING).contains(&code),
        "{what} returned {code}, which is {}",
        if code == ACC_ERR_INTERNAL { format!("an internal error: {}", message(db)) } else { "not a documented code".to_string() }
    );
}

pub unsafe fn assert_healthy(db: *mut acc_db) {
    let mut res = ptr::null_mut();
    let code = acc_exec(db, c"SELECT 1".as_ptr(), usize::MAX, &mut res);
    assert_eq!(code, ACC_OK, "the connection stopped working: {}", message(db));
    assert_eq!(acc_value_int64(res, 0, 0), 1);
    acc_result_free(res);
}

/// Reads every cell of a result with every accessor, checking what comes back.
pub unsafe fn read_all(res: *const acc_result) {
    let rows = acc_result_row_count(res);
    let cols = acc_result_column_count(res);
    for c in 0..cols {
        let name = acc_result_column_name(res, c);
        assert!(!name.is_null());
        assert!(CStr::from_ptr(name).to_str().is_ok(), "a column name is not UTF-8");
    }
    assert!(acc_result_column_name(res, cols).is_null());
    for r in 0..rows.min(200) {
        for c in 0..cols {
            let tag = acc_value_type(res, r, c);
            assert!((ACC_TYPE_NULL..=ACC_TYPE_JSON).contains(&tag), "unknown type tag {tag}");
            let _ = acc_value_int64(res, r, c);
            let _ = acc_value_double(res, r, c);
            let mut len = usize::MAX;
            let text = acc_value_text(res, r, c, &mut len);
            if text.is_null() {
                assert_eq!(len, 0);
            } else {
                let bytes = std::slice::from_raw_parts(text.cast::<u8>(), len + 1);
                assert_eq!(bytes[len], 0, "text is not NUL-terminated");
                assert!(std::str::from_utf8(&bytes[..len]).is_ok(), "text is not UTF-8");
            }
        }
    }
    // Positions past the end give defaults and never touch memory.
    for (r, c) in [(rows, 0), (0, cols), (usize::MAX, 0), (0, usize::MAX), (usize::MAX, usize::MAX)] {
        assert_eq!(acc_value_type(res, r, c), ACC_TYPE_NULL);
        assert_eq!(acc_value_int64(res, r, c), 0);
        let mut len = 7;
        assert!(acc_value_text(res, r, c, &mut len).is_null() && len == 0);
    }
}

/// A NUL-terminated copy of `bytes` cut at the first NUL.
pub fn c_text(bytes: &[u8]) -> CString {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    CString::new(&bytes[..end]).expect("no NUL before the end")
}

pub fn as_ptr(bytes: &[u8]) -> *const c_char {
    if bytes.is_empty() {
        // A valid, non-NULL pointer for a length of zero.
        c"".as_ptr()
    } else {
        bytes.as_ptr().cast()
    }
}
