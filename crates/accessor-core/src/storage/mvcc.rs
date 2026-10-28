//! Row versions and who can see them.
//!
//! Every row of a table is a chain of versions, newest first. A version is born at `begin` and dies at
//! `end`; both are *stamps*:
//!
//! * a commit timestamp (small numbers, counted up by the database at every commit), once the
//!   transaction that made the change has committed;
//! * `PENDING | transaction id` while it has not. Only that transaction sees such a version.
//! * `INF` for "has not died".
//!
//! A reader has a [`View`]: a snapshot (it sees every version committed at or before it) and the id of
//! the transaction it belongs to (it also sees that transaction's own changes). The view is kept per
//! thread and installed when a statement starts, so the table code needs no extra arguments.
//!
//! View transaction id 0 means *no versions*: rows are changed in place, as in a database that does
//! not share (the default for [`Database`](crate::Database)); readers use it too, since they change
//! nothing.

use std::cell::Cell;

pub type Stamp = u64;

/// Marks a stamp as belonging to a transaction that has not committed.
pub const PENDING: Stamp = 1 << 63;
/// "Not ended": greater than every commit timestamp, less than every pending stamp.
pub const INF: Stamp = PENDING - 1;

pub fn pending(txn: u64) -> Stamp {
    PENDING | txn
}

#[derive(Clone, Copy, Debug)]
pub struct View {
    /// Versions committed at or before this are visible.
    pub snapshot: Stamp,
    /// The transaction whose uncommitted changes are visible too (0: none).
    pub txn: u64,
}

impl View {
    /// Everything committed, nothing pending: how an unshared database sees itself.
    pub const LATEST: View = View { snapshot: INF, txn: 0 };

    fn mine(&self, stamp: Stamp) -> bool {
        self.txn != 0 && stamp == pending(self.txn)
    }

    /// Was a version born at `begin` already there for this view?
    pub fn sees_birth(&self, begin: Stamp) -> bool {
        if begin >= PENDING {
            self.mine(begin)
        } else {
            begin <= self.snapshot
        }
    }

    /// Has a version that ends at `end` already ended for this view?
    pub fn sees_death(&self, end: Stamp) -> bool {
        if end == INF {
            false
        } else if end >= PENDING {
            self.mine(end)
        } else {
            end <= self.snapshot
        }
    }

    pub fn sees(&self, begin: Stamp, end: Stamp) -> bool {
        self.sees_birth(begin) && !self.sees_death(end)
    }
}

thread_local! {
    static VIEW: Cell<View> = const { Cell::new(View::LATEST) };
}

thread_local! {
    static OVERRIDE: Cell<Option<View>> = const { Cell::new(None) };
}

/// The view a connection has asked this thread to read with, if any, in place of the database's own.
pub fn override_view() -> Option<View> {
    OVERRIDE.with(Cell::get)
}

/// Runs `f` reading as `view` does.
pub fn with_view<R>(view: View, f: impl FnOnce() -> R) -> R {
    let before = OVERRIDE.with(|c| c.replace(Some(view)));
    let out = f();
    OVERRIDE.with(|c| c.set(before));
    out
}

thread_local! {
    static BLOCKER: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Notes which transaction a statement just had to give way to (the owner of the pending `stamp`).
pub fn blocked_by(stamp: Stamp) {
    BLOCKER.with(|c| c.set(Some(stamp & !PENDING)));
}

/// The transaction the last statement on this thread gave way to, if it says.
pub fn take_blocker() -> Option<u64> {
    BLOCKER.with(Cell::take)
}

pub fn view() -> View {
    VIEW.with(Cell::get)
}

pub fn set_view(v: View) {
    VIEW.with(|c| c.set(v));
}

thread_local! {
    static ONLY: std::cell::RefCell<Option<(String, std::collections::HashSet<u32>)>> = const { std::cell::RefCell::new(None) };
    static ONLY_ON: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` with the rows of `table` limited to `ids`: how a locking read leaves out the rows it skipped.
pub fn only_rows<R>(table: &str, ids: std::collections::HashSet<u32>, f: impl FnOnce() -> R) -> R {
    let before = ONLY.with(|c| c.replace(Some((table.to_ascii_lowercase(), ids))));
    ONLY_ON.with(|c| c.set(true));
    let out = f();
    let was_on = before.is_some();
    ONLY.with(|c| *c.borrow_mut() = before);
    ONLY_ON.with(|c| c.set(was_on));
    out
}

/// Whether row `id` of `table` may be seen, given any limit set by [`only_rows`].
pub fn row_allowed(table: &str, id: u32) -> bool {
    if !ONLY_ON.with(Cell::get) {
        return true;
    }
    ONLY.with(|c| match &*c.borrow() {
        Some((t, ids)) => !t.eq_ignore_ascii_case(table) || ids.contains(&id),
        None => true,
    })
}

/// Whether [`only_rows`] is in force (the table cannot then count its rows from its totals).
pub fn rows_limited() -> bool {
    ONLY_ON.with(Cell::get)
}

