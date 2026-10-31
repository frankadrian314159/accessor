//! `PERFORMANCE_SCHEMA` (MySQL): the tables a query on the server's own state reads.
//!
//! There is no instrumentation here, so most of these are empty tables with the columns of MySQL's. The ones with
//! something to say are the variables (`global_variables`, `session_variables`, `variables_info`), the status
//! (`global_status`, `session_status`), `threads` (the one connection) and the consumers and instruments, which are the
//! usual ones, enabled.

use crate::database::Database;
use crate::error::{Error, Result};
use crate::value::{Row, Value};

fn t(s: &str) -> Value {
    Value::text(s)
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// Tables that are always empty: their columns.
const EMPTY: [(&str, &[&str]); 28] = [
    ("status_by_thread", &["thread_id", "variable_name", "variable_value"]),
    ("status_by_account", &["user", "host", "variable_name", "variable_value"]),
    ("status_by_host", &["host", "variable_name", "variable_value"]),
    ("status_by_user", &["user", "variable_name", "variable_value"]),
    ("variables_by_thread", &["thread_id", "variable_name", "variable_value"]),
    ("persisted_variables", &["variable_name", "variable_value"]),
    ("user_variables_by_thread", &["thread_id", "variable_name", "variable_value"]),
    (
        "metadata_locks",
        &["object_type", "object_schema", "object_name", "column_name", "object_instance_begin", "lock_type", "lock_duration", "lock_status", "source", "owner_thread_id", "owner_event_id"],
    ),
    (
        "data_locks",
        &["engine", "engine_lock_id", "engine_transaction_id", "thread_id", "event_id", "object_schema", "object_name", "partition_name", "subpartition_name", "index_name", "object_instance_begin", "lock_type", "lock_mode", "lock_status", "lock_data"],
    ),
    ("data_lock_waits", &["engine", "requesting_engine_lock_id", "requesting_engine_transaction_id", "requesting_thread_id", "blocking_engine_lock_id", "blocking_engine_transaction_id", "blocking_thread_id"]),
    ("error_log", &["logged", "thread_id", "prio", "error_code", "subsystem", "data"]),
    (
        "memory_summary_global_by_event_name",
        &["event_name", "count_alloc", "count_free", "sum_number_of_bytes_alloc", "sum_number_of_bytes_free", "low_count_used", "current_count_used", "high_count_used", "low_number_of_bytes_used", "current_number_of_bytes_used", "high_number_of_bytes_used"],
    ),
    ("memory_summary_by_thread_by_event_name", &["thread_id", "event_name", "count_alloc", "count_free", "sum_number_of_bytes_alloc", "sum_number_of_bytes_free", "current_count_used", "current_number_of_bytes_used"]),
    ("file_summary_by_event_name", &["event_name", "count_star", "sum_timer_wait", "min_timer_wait", "avg_timer_wait", "max_timer_wait", "count_read", "count_write", "sum_number_of_bytes_read", "sum_number_of_bytes_write"]),
    ("file_summary_by_instance", &["file_name", "event_name", "object_instance_begin", "count_star", "sum_timer_wait"]),
    (
        "events_statements_summary_by_digest",
        &["schema_name", "digest", "digest_text", "count_star", "sum_timer_wait", "min_timer_wait", "avg_timer_wait", "max_timer_wait", "sum_lock_time", "sum_errors", "sum_warnings", "sum_rows_affected", "sum_rows_sent", "sum_rows_examined", "first_seen", "last_seen"],
    ),
    ("events_statements_history", &["thread_id", "event_id", "end_event_id", "event_name", "source", "timer_start", "timer_end", "timer_wait", "sql_text", "digest", "digest_text", "current_schema", "rows_affected", "rows_sent", "rows_examined", "errors", "warnings"]),
    ("events_statements_history_long", &["thread_id", "event_id", "end_event_id", "event_name", "source", "timer_start", "timer_end", "timer_wait", "sql_text", "digest", "digest_text", "current_schema", "rows_affected", "rows_sent", "rows_examined", "errors", "warnings"]),
    ("events_statements_current", &["thread_id", "event_id", "end_event_id", "event_name", "source", "timer_start", "timer_end", "timer_wait", "sql_text", "digest", "digest_text", "current_schema", "rows_affected", "rows_sent", "rows_examined", "errors", "warnings"]),
    ("events_waits_current", &["thread_id", "event_id", "end_event_id", "event_name", "source", "timer_start", "timer_end", "timer_wait", "spins", "object_schema", "object_name", "index_name", "object_type", "object_instance_begin", "nesting_event_id", "operation", "number_of_bytes", "flags"]),
    ("events_waits_history", &["thread_id", "event_id", "end_event_id", "event_name", "source", "timer_start", "timer_end", "timer_wait", "object_schema", "object_name", "operation"]),
    ("events_waits_history_long", &["thread_id", "event_id", "end_event_id", "event_name", "source", "timer_start", "timer_end", "timer_wait", "object_schema", "object_name", "operation"]),
    ("events_waits_summary_by_account_by_event_name", &["user", "host", "event_name", "count_star", "sum_timer_wait", "min_timer_wait", "avg_timer_wait", "max_timer_wait"]),
    ("events_waits_summary_global_by_event_name", &["event_name", "count_star", "sum_timer_wait", "min_timer_wait", "avg_timer_wait", "max_timer_wait"]),
    ("prepared_statements_instances", &["object_instance_begin", "statement_id", "statement_name", "owner_thread_id", "owner_event_id", "sql_text", "count_execute"]),
    ("session_connect_attrs", &["processlist_id", "attr_name", "attr_value", "ordinal_position"]),
    ("accounts", &["user", "host", "current_connections", "total_connections"]),
    ("host_cache", &["ip", "host", "host_validated", "sum_connect_errors", "count_host_blocked_errors"]),
];

/// The consumers of events that are on by default.
const CONSUMERS: [(&str, bool); 15] = [
    ("events_stages_current", false),
    ("events_stages_history", false),
    ("events_stages_history_long", false),
    ("events_statements_cpu", false),
    ("events_statements_current", true),
    ("events_statements_history", true),
    ("events_statements_history_long", false),
    ("events_transactions_current", true),
    ("events_transactions_history", true),
    ("events_transactions_history_long", false),
    ("events_waits_current", false),
    ("events_waits_history", false),
    ("events_waits_history_long", false),
    ("global_instrumentation", true),
    ("thread_instrumentation", true),
];

impl Database {
    /// The columns and rows of `PERFORMANCE_SCHEMA.<view>`.
    pub(crate) fn performance_schema(&self, view: &str) -> Result<(Vec<String>, Vec<Row>)> {
        let view = view.to_ascii_lowercase();
        let pair = |rows: Vec<(String, String)>| -> Vec<Row> { rows.into_iter().map(|(n, v)| vec![t(&n), t(&v)]).collect() };
        Ok(match view.as_str() {
            "global_variables" | "session_variables" => (names(&["variable_name", "variable_value"]), pair(self.mysql_variable_rows())),
            "variables_info" => {
                let rows = self
                    .mysql_variable_rows()
                    .into_iter()
                    .map(|(n, _)| vec![t(&n), t("COMPILED"), t(""), Value::Int(0), Value::Int(0), Value::Null, t(""), t("")])
                    .collect();
                (names(&["variable_name", "variable_source", "variable_path", "min_value", "max_value", "set_time", "set_user", "set_host"]), rows)
            }
            "global_status" | "session_status" => (names(&["variable_name", "variable_value"]), pair(vec![("Threads_connected".into(), "1".into()), ("Uptime".into(), "1".into())])),
            "threads" => (
                names(&[
                    "thread_id", "name", "type", "processlist_id", "processlist_user", "processlist_host", "processlist_db", "processlist_command", "processlist_time", "processlist_state", "processlist_info",
                    "parent_thread_id", "role", "instrumented", "history", "connection_type", "thread_os_id", "resource_group",
                ]),
                vec![vec![
                    Value::Int(1),
                    t("thread/sql/one_connection"),
                    t("FOREGROUND"),
                    Value::Int(1),
                    t("root"),
                    t("localhost"),
                    Value::Null,
                    t("Query"),
                    Value::Int(0),
                    t("executing"),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    t("YES"),
                    t("YES"),
                    t("Socket"),
                    Value::Int(1),
                    t("SYS_default"),
                ]],
            ),
            "setup_consumers" => (names(&["name", "enabled"]), CONSUMERS.iter().map(|(n, on)| vec![t(n), t(if *on { "YES" } else { "NO" })]).collect()),
            "setup_instruments" => (names(&["name", "enabled", "timed", "properties", "flags", "volatility", "documentation"]), Vec::new()),
            "setup_actors" => (names(&["host", "user", "role", "enabled", "history"]), vec![vec![t("%"), t("%"), t("%"), t("YES"), t("YES")]]),
            "setup_objects" => (names(&["object_type", "object_schema", "object_name", "enabled", "timed"]), Vec::new()),
            "users" => (names(&["user", "current_connections", "total_connections"]), vec![vec![t("root"), Value::Int(1), Value::Int(1)]]),
            "hosts" => (names(&["host", "current_connections", "total_connections"]), vec![vec![t("localhost"), Value::Int(1), Value::Int(1)]]),
            other => match EMPTY.iter().find(|(n, _)| *n == other) {
                Some((_, cols)) => (names(cols), Vec::new()),
                None => return Err(Error::catalog(format!("Table 'performance_schema.{other}' doesn't exist"))),
            },
        })
    }
}
