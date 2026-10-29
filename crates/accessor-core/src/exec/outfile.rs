//! `SELECT ... INTO OUTFILE | DUMPFILE 'file'`, and `DO expr`.

use super::QueryResult;
use crate::database::Database;
use crate::error::{Error, Result};
use crate::sql::ast::{Expr, OutfileSpec, Select, SelectItem};
use crate::value::Value;

impl Database {
    /// Runs the query and writes its rows to a file that does not exist yet, as MySQL does.
    pub(crate) fn select_outfile(&self, select: &Select, spec: &OutfileSpec) -> Result<QueryResult> {
        let res = self.run_select(select)?;
        if std::path::Path::new(&spec.path).exists() {
            return Err(Error::runtime(format!("File '{}' already exists", spec.path)));
        }
        let mut out = String::new();
        if spec.dump {
            if res.rows.len() > 1 {
                return Err(Error::runtime("Result consisted of more than one row"));
            }
            if let Some(v) = res.rows.first().and_then(|r| r.first()) {
                out.push_str(&v.to_string());
            }
        } else {
            for row in &res.rows {
                out.push_str(&spec.lines_starting);
                for (i, v) in row.iter().enumerate() {
                    if i > 0 {
                        out.push_str(&spec.fields_terminated);
                    }
                    out.push_str(&field_text(v, spec));
                }
                out.push_str(&spec.lines_terminated);
            }
        }
        std::fs::write(&spec.path, out).map_err(|e| Error::runtime(format!("cannot write '{}': {e}", spec.path)))?;
        Ok(QueryResult { columns: Vec::new(), rows: Vec::new(), affected: res.rows.len() })
    }

    /// `DO expr, ...`: the expressions are evaluated (their side effects happen) and nothing comes back.
    pub(crate) fn run_do(&self, exprs: &[Expr]) -> Result<QueryResult> {
        let items = exprs.iter().map(|e| SelectItem::Expr { expr: e.clone(), alias: None }).collect();
        self.run_select(&Select { items, ..Select::default() })?;
        Ok(QueryResult::default())
    }
}

/// One value as a field of the file: NULL is `\N`, and the characters that would end the field or the line, the enclosing
/// character and the escape character itself are given the escape character in front.
fn field_text(v: &Value, spec: &OutfileSpec) -> String {
    if v.is_null() {
        return match spec.escaped {
            Some(e) => format!("{e}N"),
            None => "NULL".to_string(),
        };
    }
    let text = match v {
        Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
        other => other.to_string(),
    };
    let numeric = v.num().is_some();
    let enclose = spec.enclosed.is_some() && (!spec.optionally || !numeric);
    let mut s = String::new();
    for c in text.chars() {
        let special = Some(c) == spec.escaped
            || (enclose && Some(c) == spec.enclosed)
            || spec.fields_terminated.starts_with(c)
            || spec.lines_terminated.starts_with(c);
        if special {
            if let Some(e) = spec.escaped {
                s.push(e);
            }
        }
        s.push(c);
    }
    match spec.enclosed.filter(|_| enclose) {
        Some(e) => format!("{e}{s}{e}"),
        None => s,
    }
}
