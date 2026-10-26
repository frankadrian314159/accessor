//! Run-time parameters: `SET name TO value`, `RESET name`, `SHOW name`.
//!
//! The engine has one session, so a parameter is a named setting of the `Database`. Most of PostgreSQL's names are
//! accepted so that scripts written for it run, and remember what they are given (`SHOW` gives it back);
//! a few have an effect here:
//!
//! * `timezone` (also `SET TIME ZONE`): how timestamps are read and shown;
//! * `optimizer` (`on`/`off`): the query optimizer, as [`Database::set_optimizer`].
//!
//! Names with a dot in them (`my.setting`) may be made up, as in PostgreSQL. `SET LOCAL` is the same as `SET`
//! (there is no per-transaction state), and `SET TRANSACTION ...` and `SET ROLE` are read and ignored.

use super::QueryResult;
use crate::database::Database;
use crate::error::{Error, Result};
use crate::value::Value;

/// `(name, default, kind)`; kind `b` is a boolean (on/off), anything else free text.
const PARAMETERS: &[(&str, &str, char)] = &[
    ("application_name", "", 't'),
    ("array_nulls", "on", 'b'),
    ("backslash_quote", "safe_encoding", 't'),
    ("bytea_output", "hex", 't'),
    ("check_function_bodies", "on", 'b'),
    ("client_encoding", "UTF8", 't'),
    ("client_min_messages", "notice", 't'),
    ("commit_delay", "0", 't'),
    ("compute_query_id", "auto", 't'),
    ("constraint_exclusion", "partition", 't'),
    ("cpu_index_tuple_cost", "0.005", 't'),
    ("cpu_operator_cost", "0.0025", 't'),
    ("cpu_tuple_cost", "0.01", 't'),
    ("cursor_tuple_fraction", "0.1", 't'),
    ("datestyle", "ISO, MDY", 't'),
    ("deadlock_timeout", "1s", 't'),
    ("debug_parallel_query", "off", 't'),
    ("default_statistics_target", "100", 't'),
    ("default_table_access_method", "heap", 't'),
    ("default_tablespace", "", 't'),
    ("default_text_search_config", "pg_catalog.english", 't'),
    ("default_toast_compression", "pglz", 't'),
    ("default_transaction_deferrable", "off", 'b'),
    ("default_transaction_isolation", "read committed", 't'),
    ("default_transaction_read_only", "off", 'b'),
    ("effective_cache_size", "4GB", 't'),
    ("escape_string_warning", "on", 'b'),
    ("extra_float_digits", "1", 't'),
    ("from_collapse_limit", "8", 't'),
    ("geqo", "on", 'b'),
    ("geqo_threshold", "12", 't'),
    ("gin_fuzzy_search_limit", "0", 't'),
    ("hash_mem_multiplier", "2", 't'),
    ("idle_in_transaction_session_timeout", "0", 't'),
    ("intervalstyle", "postgres", 't'),
    ("jit", "off", 'b'),
    ("join_collapse_limit", "8", 't'),
    ("lc_messages", "C", 't'),
    ("lc_monetary", "C", 't'),
    ("lc_numeric", "C", 't'),
    ("lc_time", "C", 't'),
    ("lock_timeout", "0", 't'),
    ("log_min_messages", "warning", 't'),
    ("maintenance_work_mem", "64MB", 't'),
    ("max_parallel_workers", "8", 't'),
    ("max_parallel_workers_per_gather", "2", 't'),
    ("max_stack_depth", "2MB", 't'),
    ("min_parallel_index_scan_size", "512kB", 't'),
    ("min_parallel_table_scan_size", "8MB", 't'),
    ("optimizer", "on", 'b'),
    ("parallel_setup_cost", "1000", 't'),
    ("parallel_tuple_cost", "0.1", 't'),
    ("password_encryption", "scram-sha-256", 't'),
    ("plan_cache_mode", "auto", 't'),
    ("quote_all_identifiers", "off", 'b'),
    ("random_page_cost", "4", 't'),
    ("row_security", "on", 'b'),
    ("search_path", "\"$user\", public", 't'),
    ("seq_page_cost", "1", 't'),
    ("session_replication_role", "origin", 't'),
    ("standard_conforming_strings", "on", 'b'),
    ("statement_timeout", "0", 't'),
    ("synchronous_commit", "on", 'b'),
    ("temp_buffers", "8MB", 't'),
    ("timezone", "UTC", 't'),
    ("trace_sort", "off", 'b'),
    ("transaction_isolation", "read committed", 't'),
    ("transaction_read_only", "off", 'b'),
    ("vacuum_freeze_min_age", "50000000", 't'),
    ("wal_compression", "off", 't'),
    ("work_mem", "4MB", 't'),
    ("xmlbinary", "base64", 't'),
    ("xmloption", "content", 't'),
    ("allow_in_place_tablespaces", "off", 'b'),
    ("transaction", "", 't'),
    ("characteristics", "", 't'),
    ("authorization", "", 't'),
    ("role", "", 't'),
];

fn known(name: &str) -> Option<(&'static str, char)> {
    if let Some(&(_, d, k)) = PARAMETERS.iter().find(|(n, _, _)| *n == name) {
        return Some((d, k));
    }
    // enable_seqscan, enable_hashjoin, ...: the planner switches
    name.starts_with("enable_").then_some(("on", 'b'))
}

fn boolean(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" | "t" | "y" => Some("on"),
        "off" | "false" | "no" | "0" | "f" | "n" => Some("off"),
        _ => None,
    }
}

impl Database {
    fn unrecognized(name: &str) -> Error {
        Error::runtime(format!("unrecognized configuration parameter \"{name}\""))
    }

    /// `SET name TO value`.
    pub(crate) fn set_parameter(&mut self, name: &str, value: Option<&str>) -> Result<()> {
        let name = name.to_ascii_lowercase();
        let Some(value) = value else { return self.reset_parameter(Some(&name)) };
        if name == "timezone" {
            self.set_time_zone(value)?;
            self.settings.insert(name, value.to_string());
            return Ok(());
        }
        let shown = match known(&name) {
            Some((_, 'b')) => boolean(value)
                .ok_or_else(|| Error::runtime(format!("invalid value for parameter \"{name}\": \"{value}\" (use on or off)")))?
                .to_string(),
            Some(_) => value.to_string(),
            None if name.contains('.') => value.to_string(),
            None => return Err(Self::unrecognized(&name)),
        };
        if name == "optimizer" {
            self.set_optimizer(shown == "on");
        }
        self.settings.insert(name, shown);
        Ok(())
    }

    /// `RESET name` (or `RESET ALL`).
    pub(crate) fn reset_parameter(&mut self, name: Option<&str>) -> Result<()> {
        let Some(name) = name else {
            let names: Vec<String> = self.settings.keys().cloned().collect();
            for n in names {
                self.reset_parameter(Some(&n))?;
            }
            return Ok(());
        };
        let name = name.to_ascii_lowercase();
        match known(&name) {
            Some((default, _)) => {
                if name == "timezone" {
                    self.set_time_zone("UTC")?;
                } else if name == "optimizer" {
                    self.set_optimizer(true);
                }
                let _ = default;
                self.settings.remove(&name);
                Ok(())
            }
            None if name.contains('.') => {
                self.settings.remove(&name);
                Ok(())
            }
            None => Err(Self::unrecognized(&name)),
        }
    }

    /// `SHOW name` (or `SHOW ALL`).
    pub(crate) fn show_parameter(&self, name: Option<&str>) -> Result<QueryResult> {
        let current = |name: &str, default: &str| -> String {
            if name == "timezone" {
                return self.time_zone();
            }
            if name == "optimizer" {
                return if self.optimizer_enabled() { "on" } else { "off" }.to_string();
            }
            self.settings.get(name).cloned().unwrap_or_else(|| default.to_string())
        };
        match name {
            Some(n) => {
                let n = n.to_ascii_lowercase();
                let value = match known(&n) {
                    Some((default, _)) => current(&n, default),
                    None if n.contains('.') => self.settings.get(&n).cloned().ok_or_else(|| Self::unrecognized(&n))?,
                    None => return Err(Self::unrecognized(&n)),
                };
                Ok(QueryResult { columns: vec![if n == "timezone" { "TimeZone".into() } else { n }], rows: vec![vec![Value::text(&value)]], affected: 0 })
            }
            None => {
                let mut rows: Vec<Vec<Value>> = PARAMETERS
                    .iter()
                    .filter(|(n, _, _)| !matches!(*n, "transaction" | "characteristics" | "authorization" | "role"))
                    .map(|(n, d, _)| vec![Value::text(n), Value::text(&current(n, d)), Value::text("")])
                    .collect();
                for (n, v) in self.settings.iter().filter(|(n, _)| n.contains('.')) {
                    rows.push(vec![Value::text(n), Value::text(v), Value::text("")]);
                }
                Ok(QueryResult { columns: vec!["name".into(), "setting".into(), "description".into()], rows, affected: 0 })
            }
        }
    }
}
