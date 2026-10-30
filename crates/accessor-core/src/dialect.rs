//! The SQL dialect a session speaks: how its text is read and how its values are shown.
//!
//! * `asql` (the default): Accessor's own, shaped by Microsoft Access: `[bracketed]` names, `#date#` literals, Yes/No
//!   for a boolean, which is a number (Yes is -1);
//! * `postgresql`: PostgreSQL's reading and output: `#` is no comment, a boolean is `true`/`false` (`t`/`f` in a
//!   result), and `false < true`;
//! * `mysql`: MySQL's: `# ...` is a comment to the end of the line, a boolean is 1 or 0, and `false < true`.
//!
//! The dialect is the `dialect` setting of the session (`SET dialect = 'mysql'`, [`Database::set_dialect`]); the
//! statement being run keeps it in a thread-local, as the time zone is kept, for the lexer, the parser and the
//! printers to read.

use std::cell::Cell;
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dialect {
    #[default]
    Asql,
    Postgresql,
    Mysql,
}

impl Dialect {
    /// The dialect called `name` (`asql`, `access`, `postgresql`, `postgres`, `pg`, `mysql`), in any case.
    pub fn parse(name: &str) -> Option<Dialect> {
        match name.trim().to_ascii_lowercase().as_str() {
            "asql" | "access" => Some(Dialect::Asql),
            "postgresql" | "postgres" | "pg" => Some(Dialect::Postgresql),
            "mysql" => Some(Dialect::Mysql),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dialect::Asql => "asql",
            Dialect::Postgresql => "postgresql",
            Dialect::Mysql => "mysql",
        }
    }
}

impl fmt::Display for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

thread_local! {
    static CURRENT: Cell<Dialect> = const { Cell::new(Dialect::Asql) };
}

/// The dialect of the statement being read or run on this thread.
pub fn current() -> Dialect {
    CURRENT.with(Cell::get)
}

pub(crate) fn set_session(d: Dialect) {
    CURRENT.with(|c| c.set(d));
}

thread_local! {
    static TABLEOID: Cell<bool> = const { Cell::new(false) };
}

/// The name of the hidden column that carries `tableoid`: the table a row of a scan came from.
pub(crate) const TABLEOID_COLUMN: &str = "\u{1}tableoid";

/// Does the statement being run mention `tableoid`? (Only then do the scans of PostgreSQL carry it.)
pub(crate) fn wants_tableoid() -> bool {
    TABLEOID.with(Cell::get)
}

pub(crate) fn set_wants_tableoid(on: bool) {
    TABLEOID.with(|c| c.set(on));
}
