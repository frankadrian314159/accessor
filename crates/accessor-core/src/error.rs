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

thread_local! {
    /// The error a T-SQL `CATCH` block is dealing with (what `ERROR_MESSAGE()` and the like read).
    static HANDLED: std::cell::RefCell<Option<Diag>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn set_handled(d: Option<Diag>) {
    HANDLED.with(|h| *h.borrow_mut() = d);
}

pub(crate) fn handled() -> Option<Diag> {
    HANDLED.with(|h| h.borrow().clone())
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

/// The SQLSTATE of a plpgsql condition name (`division_by_zero`), if it is one this engine knows.
pub fn pg_condition_state(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "successful_completion" => "00000",
        "warning" => "01000",
        "no_data" => "02000",
        "sql_statement_not_yet_complete" => "03000",
        "connection_exception" => "08000",
        "feature_not_supported" => "0A000",
        "invalid_transaction_initiation" => "0B000",
        "data_exception" => "22000",
        "string_data_right_truncation" => "22001",
        "null_value_not_allowed" => "22004",
        "numeric_value_out_of_range" => "22003",
        "invalid_datetime_format" => "22007",
        "datetime_field_overflow" => "22008",
        "division_by_zero" => "22012",
        "invalid_parameter_value" => "22023",
        "invalid_text_representation" => "22P02",
        "invalid_binary_representation" => "22P03",
        "invalid_escape_sequence" => "22025",
        "array_subscript_error" => "2202E",
        "integrity_constraint_violation" => "23000",
        "restrict_violation" => "23001",
        "not_null_violation" => "23502",
        "foreign_key_violation" => "23503",
        "unique_violation" => "23505",
        "check_violation" => "23514",
        "exclusion_violation" => "23P01",
        "invalid_cursor_state" => "24000",
        "invalid_transaction_state" => "25000",
        "invalid_sql_statement_name" => "26000",
        "triggered_data_change_violation" => "27000",
        "invalid_authorization_specification" => "28000",
        "invalid_cursor_name" => "34000",
        "syntax_error_or_access_rule_violation" => "42000",
        "syntax_error" => "42601",
        "insufficient_privilege" => "42501",
        "undefined_column" => "42703",
        "undefined_function" => "42883",
        "undefined_table" => "42P01",
        "undefined_object" => "42704",
        "duplicate_column" => "42701",
        "duplicate_function" => "42723",
        "duplicate_table" => "42P07",
        "duplicate_object" => "42710",
        "datatype_mismatch" => "42804",
        "ambiguous_column" => "42702",
        "wrong_object_type" => "42809",
        "cardinality_violation" => "21000",
        "plpgsql_error" => "P0000",
        "raise_exception" => "P0001",
        "no_data_found" => "P0002",
        "too_many_rows" => "P0003",
        "assert_failure" => "P0004",
        "internal_error" => "XX000",
        "data_corrupted" => "XX001",
        "lock_not_available" => "55P03",
        "query_canceled" => "57014",
        "serialization_failure" => "40001",
        "deadlock_detected" => "40P01",
        "transaction_rollback" => "40000",
        _ => return None,
    })
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

    /// The SQLSTATE PostgreSQL would give the error, as far as it can be told (what a plpgsql `EXCEPTION` handler matches).
    pub fn pg_sqlstate(&self) -> String {
        match self {
            Error::Signal { state, .. } => return state.clone(),
            Error::Syntax(_) => return "42601".into(),
            _ => {}
        }
        let m = self.to_string().to_ascii_lowercase();
        let has = |s: &str| m.contains(s);
        if has("division by zero") {
            "22012"
        } else if has("duplicate value") || has("duplicate key") {
            "23505"
        } else if has("cannot contain a null") || has("null value in column") {
            "23502"
        } else if has("requires a matching record") || has("still refer to it") || has("foreign key") {
            "23503"
        } else if has("check constraint failed") || has("violates check") {
            "23514"
        } else if has("query returned more than one row") || has("more than one row") {
            "P0003"
        } else if has("query returned no rows") {
            "P0002"
        } else if has("invalid input syntax") {
            "22P02"
        } else if has("out of range") {
            "22003"
        } else if has("value too long") {
            "22001"
        } else if has("already exists") {
            if has("column") { "42701" } else { "42P07" }
        } else if has("does not exist") || has("was not found") {
            if has("column") {
                "42703"
            } else if has("function") {
                "42883"
            } else {
                "42P01"
            }
        } else if matches!(self, Error::Constraint(_)) {
            "23000"
        } else {
            "XX000"
        }
        .to_string()
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
