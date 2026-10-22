//! Types shared between the application shell and the individual views.

use accessor_core::Value;

/// Something a view wants the application to do once the frame is drawn.
pub enum Event {
    /// Data or schema changed; every open view must refresh.
    Changed,
    Error(String),
    /// Run a statement; `ok` becomes the status text on success.
    Execute { sql: String, ok: String },
    Status(String),
    OpenDatasheet(String),
    OpenDesign(String),
    OpenForm(String),
    OpenQuery { name: Option<String>, sql: String },
}

/// Text shown for a cell in a read-only grid.
pub fn display(v: &Value) -> String {
    v.to_string()
}

/// Quotes an identifier for use in generated SQL.
pub fn bracket(name: &str) -> String {
    format!("[{}]", name.replace(']', ""))
}
