//! Domains and enumerated types made with `CREATE DOMAIN` and `CREATE TYPE ... AS ENUM`.
//!
//! A user type is not a data type of the engine: it is a name for a built-in type together with constraints, so the
//! parser expands it where it is used. A column of a domain gets the base type, the domain's default and `NOT NULL`,
//! and its `CHECK` (with `VALUE` standing for the column); a column of an enum is text with a `CHECK` that the value is
//! one of the labels. The types known to the session live in the database's catalog and are copied here before each
//! statement is read; the parser also adds the ones a script creates, so later statements of the same script see them.

use std::cell::RefCell;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UserType {
    /// The SQL name of the type it is made from (`TEXT` for an enum).
    pub base: String,
    pub domain: bool,
    pub not_null: bool,
    /// Source text of the default (a literal).
    pub default: Option<String>,
    /// `CHECK` expressions, with `VALUE` for the value checked.
    pub checks: Vec<String>,
    /// The labels of an enum, in order.
    pub labels: Vec<String>,
}

thread_local! {
    static TYPES: RefCell<HashMap<String, UserType>> = RefCell::new(HashMap::new());
    static LENIENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// With `SET lenient_types = on` a type name the engine does not know is kept as `TEXT` instead of being refused, and so
/// is an array of any type (`int[]`): a table of PostgreSQL's types can be made and the statements that follow it can run.
pub(crate) struct LenientGuard(bool);

/// Sets the flag until the guard is dropped, then puts back what it was.
pub(crate) fn lenient_guard(on: bool) -> LenientGuard {
    LenientGuard(LENIENT.with(|l| l.replace(on)))
}

impl Drop for LenientGuard {
    fn drop(&mut self) {
        LENIENT.with(|l| l.set(self.0));
    }
}

pub(crate) fn lenient() -> bool {
    LENIENT.with(|l| l.get())
}

/// The data type for a type name: a built-in one, or (when lenient) text.
pub(crate) fn resolve(name: &str) -> Option<(crate::value::DataType, bool)> {
    crate::value::DataType::from_sql_name(name).or_else(|| lenient().then_some((crate::value::DataType::Text, false)))
}

/// The types the parser knows now.
pub(crate) fn snapshot() -> HashMap<String, UserType> {
    TYPES.with(|t| t.borrow().clone())
}

/// Replaces the types the parser knows.
pub(crate) fn set_types(types: HashMap<String, UserType>) {
    TYPES.with(|t| *t.borrow_mut() = types);
}

pub(crate) fn register(name: &str, t: UserType) {
    TYPES.with(|m| m.borrow_mut().insert(name.to_ascii_lowercase(), t));
}

pub(crate) fn unregister(name: &str) {
    TYPES.with(|m| m.borrow_mut().remove(&name.to_ascii_lowercase()));
}

pub(crate) fn lookup(name: &str) -> Option<UserType> {
    TYPES.with(|m| m.borrow().get(&name.to_ascii_lowercase()).cloned())
}

impl UserType {
    /// The checks of the type for a column called `column`.
    pub fn checks_for(&self, column: &str) -> Vec<String> {
        let mut out: Vec<String> = self.checks.iter().map(|c| replace_value(c, column)).collect();
        if !self.labels.is_empty() {
            let list: Vec<String> = self.labels.iter().map(|l| format!("'{}'", l.replace('\'', "''"))).collect();
            out.push(format!("{column} IN ({})", list.join(", ")));
        }
        out
    }
}

/// `VALUE` (as a whole word, outside quotes) becomes `column`.
fn replace_value(text: &str, column: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            out.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            out.push(c);
            i += 1;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if word.eq_ignore_ascii_case("VALUE") {
                out.push_str(column);
            } else {
                out.push_str(&word);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}
