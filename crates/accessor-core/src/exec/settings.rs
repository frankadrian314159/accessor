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
    ("cast_truncates", "on", 'b'),
    ("integer_division", "on", 'b'),
    ("lenient_types", "off", 'b'),
    ("round_half_even", "on", 'b'),
    ("strict_types", "off", 'b'),
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

pub(crate) fn known(name: &str) -> Option<(&'static str, char)> {
    if let Some(&(_, d, k)) = PARAMETERS.iter().find(|(n, _, _)| *n == name) {
        return Some((d, k));
    }
    if let Some(&(_, d)) = MYSQL_VARIABLES.iter().find(|(n, _)| *n == name) {
        return Some((d, 't'));
    }
    if MYSQL_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Some(("", 't'));
    }
    // enable_seqscan, enable_hashjoin, ...: the planner switches
    name.starts_with("enable_").then_some(("on", 'b'))
}

/// MySQL system variables with a default value; other names with one of the prefixes are accepted with an empty default.
const MYSQL_VARIABLES: &[(&str, &str)] = &[
    ("sql_mode", "ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION"),
    ("autocommit", "1"),
    ("version", "8.0.40"),
    ("version_comment", "Accessor"),
    ("character_set_client", "utf8mb4"),
    ("character_set_connection", "utf8mb4"),
    ("character_set_results", "utf8mb4"),
    ("character_set_server", "utf8mb4"),
    ("character_set_database", "utf8mb4"),
    ("collation_connection", "utf8mb4_0900_ai_ci"),
    ("collation_server", "utf8mb4_0900_ai_ci"),
    ("collation_database", "utf8mb4_0900_ai_ci"),
    ("foreign_key_checks", "1"),
    ("unique_checks", "1"),
    ("sql_auto_is_null", "0"),
    ("sql_safe_updates", "0"),
    ("sql_select_limit", "18446744073709551615"),
    ("sort_buffer_size", "262144"),
    ("join_buffer_size", "262144"),
    ("max_heap_table_size", "16777216"),
    ("tmp_table_size", "16777216"),
    ("group_concat_max_len", "1024"),
    ("default_storage_engine", "InnoDB"),
    ("default_tmp_storage_engine", "InnoDB"),
    ("div_precision_increment", "4"),
    ("lc_time_names", "en_US"),
    ("time_zone", "SYSTEM"),
    ("wait_timeout", "28800"),
    ("max_allowed_packet", "67108864"),
    ("innodb_lock_wait_timeout", "50"),
    ("explicit_defaults_for_timestamp", "1"),
    ("max_execution_time", "0"),
    ("big_tables", "0"),
    ("sql_log_bin", "1"),
    ("big_tests", "0"),
    ("read_only", "0"),
    ("max_connections", "151"),
    ("max_join_size", "18446744073709551615"),
    ("optimizer_switch", "index_merge=on,index_merge_union=on,index_merge_sort_union=on,index_merge_intersection=on,engine_condition_pushdown=on,index_condition_pushdown=on,mrr=on,mrr_cost_based=on,block_nested_loop=on,batched_key_access=off,materialization=on,semijoin=on,loosescan=on,firstmatch=on,duplicateweedout=on,subquery_materialization_cost_based=on,use_index_extensions=on,condition_fanout_filter=on,derived_merge=on"),
    ("lower_case_table_names", "0"),
    ("have_innodb", "YES"),
    ("have_ssl", "DISABLED"),
    ("port", "3306"),
    ("hostname", "localhost"),
    ("datadir", "./"),
    ("protocol_version", "10"),
];

const MYSQL_PREFIXES: &[&str] = &[
    "innodb_", "myisam_", "optimizer_", "performance_schema_", "binlog_", "gtid_", "slave_", "replica_", "rpl_", "ndb_", "thread_", "table_", "tx_",
    "sql_", "max_", "min_", "net_", "read_", "query_cache_", "character_set_", "collation_", "default_", "key_", "tmp_", "session_track_", "log_",
    "long_query", "slow_query", "general_log", "event_scheduler", "local_infile", "secure_file_priv", "version_", "have_", "ft_", "histogram_",
    "mysqlx_", "validate_password", "caching_sha2", "authentication", "offline_mode", "super_read_only", "skip_", "use_secondary_engine",
    "internal_tmp", "explain_", "regexp_", "block_encryption_mode", "cte_max", "end_markers", "information_schema_stats_expiry", "keep_files",
    "lower_case", "low_priority", "old_alter", "preload_buffer", "pseudo_", "rand_seed", "rbr_", "relay_", "select_into", "show_create_table",
    "stored_program", "sync_", "system_time_zone", "temptable_", "updatable_views", "windowing_use", "debug", "debug_sync", "concurrent_insert",
    "delay_key_write", "delayed_", "flush", "foreign_key_checks", "interactive_timeout", "lock_wait", "mandatory_", "master_", "mysql_", "new",
    "open_files", "parser_", "plugin_", "range_", "schema_", "server_", "session_", "sha256_", "socket", "ssl_", "tls_", "transaction_", "unique_checks",
    "wait_", "completion_type", "connect_timeout", "div_precision", "group_replication", "init_", "insert_id", "last_insert_id", "identity", "timestamp",
    "check_proxy", "disabled_storage", "enforce_", "external_user", "ngram_", "resultset_", "rewriter_", "thread", "ndbinfo", "back_log", "autocommit",
    "bulk_insert", "build_id", "big_", "binary_", "auto_increment", "admin_", "activate_all", "allow_", "audit_", "authentication_",
];

impl Database {
    /// The value of `@name` (a user variable) or `@@name` (a system variable), if there is one.
    pub(crate) fn session_value(&self, name: &str) -> Option<Value> {
        if let Some(sys) = name.strip_prefix("@@") {
            let sys = sys.to_ascii_lowercase();
            let (default, _) = known(&sys)?;
            let text = if sys == "timezone" { self.time_zone() } else { self.settings.get(&sys).cloned().unwrap_or_else(|| default.to_string()) };
            // numbers and switches are numbers, as MySQL gives them
            return Some(match text.parse::<i64>() {
                Ok(n) => Value::Int(n),
                Err(_) => match text.as_str() {
                    "on" => Value::Int(1),
                    "off" => Value::Int(0),
                    _ => Value::text(&text),
                },
            });
        }
        self.user_vars.get(&name.to_ascii_lowercase()).cloned()
    }

    /// `(name, value)` of the system variables, for `SHOW VARIABLES`.
    pub(crate) fn mysql_variable_rows(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = MYSQL_VARIABLES
            .iter()
            .map(|(n, d)| (n.to_string(), self.settings.get(*n).cloned().unwrap_or_else(|| d.to_string())))
            .collect();
        for (n, v) in &self.settings {
            if !out.iter().any(|(m, _)| m == n) {
                out.push((n.clone(), v.clone()));
            }
        }
        out.sort();
        out
    }
}

fn boolean(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" | "t" | "y" => Some("on"),
        "off" | "false" | "no" | "0" | "f" | "n" => Some("off"),
        _ => None,
    }
}

impl Database {
    /// Is MySQL's `ONLY_FULL_GROUP_BY` in the `sql_mode` (it is in the default one)?
    pub(crate) fn only_full_group_by(&self) -> bool {
        self.settings.get("sql_mode").is_none_or(|m| m.to_ascii_uppercase().contains("ONLY_FULL_GROUP_BY"))
    }

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
        if name == "sql_mode" {
            // MySQL: without a STRICT mode a value that does not fit becomes the nearest one that does
            crate::schema::set_lenient(!shown.to_ascii_uppercase().contains("STRICT"));
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
