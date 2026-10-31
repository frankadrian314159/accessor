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
    /// MySQL's `SIGNAL`: a condition raised by the program, with its SQLSTATE, error number and message (and the other
    /// items of the condition that `GET DIAGNOSTICS` can read).
    #[error("{message}")]
    Signal { state: String, errno: i64, message: String, items: Vec<(String, String)> },
}

/// The diagnostics area: what `GET DIAGNOSTICS` reads of a condition.
#[derive(Clone, Debug, PartialEq)]
pub struct Diag {
    pub state: String,
    pub errno: i64,
    pub message: String,
    pub items: Vec<(String, String)>,
}

impl Diag {
    /// The value of a condition item (`MESSAGE_TEXT`, `RETURNED_SQLSTATE`, `MYSQL_ERRNO`, `CLASS_ORIGIN`, ...).
    pub fn item(&self, name: &str) -> Option<String> {
        let name = name.to_ascii_uppercase();
        match name.as_str() {
            "MESSAGE_TEXT" => Some(self.message.clone()),
            "RETURNED_SQLSTATE" => Some(self.state.clone()),
            "MYSQL_ERRNO" => Some(self.errno.to_string()),
            "CLASS_ORIGIN" | "SUBCLASS_ORIGIN" => {
                if let Some((_, v)) = self.items.iter().find(|(k, _)| *k == name) {
                    return Some(v.clone());
                }
                Some(if self.state.starts_with("HY") || self.errno >= 1000 && !self.state.starts_with("45") && !self.state.starts_with("01") && !self.state.starts_with("02") { "MySQL" } else { "ISO 9075" }.to_string())
            }
            "CONSTRAINT_CATALOG" | "CATALOG_NAME" => Some(self.items.iter().find(|(k, _)| *k == name).map_or_else(|| "def".to_string(), |(_, v)| v.clone())),
            _ => Some(self.items.iter().find(|(k, _)| *k == name).map_or_else(String::new, |(_, v)| v.clone())),
        }
    }
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

    /// The SQLSTATE of an error, as far as it can be told from what went wrong.
    pub fn sqlstate(&self) -> String {
        if let Error::Signal { state, .. } = self {
            return state.clone();
        }
        let m = self.to_string().to_ascii_lowercase();
        if m.starts_with("no data to fetch") {
            "02000"
        } else if m.contains("duplicate") || m.contains("cannot contain a null") || m.contains("cannot be null") || m.contains("null value") {
            "23000"
        } else if m.contains("already exists") {
            "42S01"
        } else if m.contains("does not exist") || m.contains("was not found") {
            "42S02"
        } else if m.contains("division by zero") {
            "22012"
        } else if m.starts_with("syntax error") {
            "42000"
        } else {
            "HY000"
        }
        .to_string()
    }

    /// The MySQL error number of an error, as far as it can be told.
    pub fn errno(&self) -> i64 {
        if let Error::Signal { errno, .. } = self {
            return *errno;
        }
        match self.sqlstate().as_str() {
            "02000" => 1329,
            "42S01" => 1050,
            "42S02" => 1146,
            "22012" => 1365,
            "23000" if self.to_string().to_ascii_lowercase().contains("duplicate") => 1062,
            "23000" => 1048,
            "42000" => 1064,
            _ => 1105,
        }
    }

    /// The diagnostics area this error leaves.
    pub fn diag(&self) -> Diag {
        match self {
            Error::Signal { state, errno, message, items } => Diag { state: state.clone(), errno: *errno, message: message.clone(), items: items.clone() },
            other => Diag { state: other.sqlstate(), errno: other.errno(), message: other.to_string(), items: Vec::new() },
        }
    }

    /// A condition raised by `SIGNAL`.
    pub fn signal(state: &str, errno: i64, message: String, items: Vec<(String, String)>) -> Self {
        Error::Signal { state: state.to_string(), errno, message, items }
    }
}
