//! Error type shared by every layer of the engine.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Syntax error: {0}")]
    Syntax(String),
    /// Unknown / duplicate table, column, index, view.
    #[error("{0}")]
    Catalog(String),
    /// Not-null, uniqueness, and similar integrity violations.
    #[error("{0}")]
    Constraint(String),
    #[error("Type mismatch: {0}")]
    Type(String),
    #[error("{0}")]
    Runtime(String),
    #[error("Database file is corrupt: {0}")]
    Corrupt(String),
    /// Another connection or process has the database file open.
    #[error("{0}")]
    Locked(String),
    /// Two transactions changed the same record, or a record changed after this transaction's
    /// snapshot was taken: this transaction (or statement) cannot go on and should be retried.
    #[error("{0}")]
    Conflict(String),
    /// Another transaction has an uncommitted change that this statement needs to get past. It
    /// goes away when that transaction ends, so the statement can be tried again.
    #[error("{0}")]
    Busy(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn syntax(msg: impl Into<String>) -> Self {
        Error::Syntax(msg.into())
    }
    pub fn catalog(msg: impl Into<String>) -> Self {
        Error::Catalog(msg.into())
    }
    pub fn constraint(msg: impl Into<String>) -> Self {
        Error::Constraint(msg.into())
    }
    pub fn type_err(msg: impl Into<String>) -> Self {
        Error::Type(msg.into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Error::Conflict(format!("could not serialize access due to a concurrent update: {}; retry the transaction", msg.into()))
    }
    pub fn busy(msg: impl Into<String>) -> Self {
        Error::Busy(format!("a record is being changed by another transaction: {}", msg.into()))
    }
    pub fn runtime(msg: impl Into<String>) -> Self {
        Error::Runtime(msg.into())
    }
}
