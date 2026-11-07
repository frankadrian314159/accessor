//! `INFORMATION_SCHEMA`: the catalog, as tables a query can read.
//!
//! `SELECT * FROM INFORMATION_SCHEMA.COLUMNS WHERE table_name = 'Orders'` lists the fields of a
//! table, and so on. The rows are worked out from the catalog each time the view is read, so they
//! are always current; nothing is stored. Names follow the SQL standard where it has one, and a few
//! columns are added for what ASQL has that the standard does not.

use crate::database::Database;
use crate::error::{Error, Result};
use crate::sql::ast::{TriggerEvent, TriggerTiming};
use crate::value::{Row, Value};

thread_local! {
    static PROBING_VIEWS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Set while `information_schema.views` is being filled: a view over it would otherwise ask itself whether it can be
    /// updated, which reads `information_schema.views` again.
    static LISTING_VIEWS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The views of the schema, for the error message.
const VIEWS: &str = "TABLES, COLUMNS, VIEWS, TABLE_CONSTRAINTS, KEY_COLUMN_USAGE, REFERENTIAL_CONSTRAINTS, INDEXES, ROUTINES, PARAMETERS, SEQUENCES, TRIGGERS, REPORTS, SCHEMATA, STATISTICS, CHARACTER_SETS, COLLATIONS, ENGINES, PARTITIONS, CHECK_CONSTRAINTS, PROCESSLIST and the empty MySQL views (COLUMN_STATISTICS, USER_PRIVILEGES, EVENTS, ...)";

/// Views of MySQL that describe a server: columns, and no rows.
const EMPTY_VIEWS: [(&str, &[&str]); 22] = [
    ("processlist", &["id", "user", "host", "db", "command", "time", "state", "info"]),
    ("column_statistics", &["schema_name", "table_name", "column_name", "histogram"]),
    ("user_privileges", &["grantee", "table_catalog", "privilege_type", "is_grantable"]),
    ("schema_privileges", &["grantee", "table_catalog", "table_schema", "privilege_type", "is_grantable"]),
    ("table_privileges", &["grantee", "table_catalog", "table_schema", "table_name", "privilege_type", "is_grantable"]),
    ("column_privileges", &["grantee", "table_catalog", "table_schema", "table_name", "column_name", "privilege_type", "is_grantable"]),
    ("events", &["event_catalog", "event_schema", "event_name", "definer", "event_body", "status"]),
    ("optimizer_trace", &["query", "trace", "missing_bytes_beyond_max_mem_size", "insufficient_privileges"]),
    ("resource_groups", &["resource_group_name", "resource_group_type", "resource_group_enabled", "vcpu_ids", "thread_priority"]),
    ("files", &["file_id", "file_name", "file_type", "tablespace_name"]),
    ("tablespaces", &["tablespace_name", "engine", "tablespace_type"]),
    ("plugins", &["plugin_name", "plugin_version", "plugin_status", "plugin_type"]),
    ("view_table_usage", &["view_catalog", "view_schema", "view_name", "table_catalog", "table_schema", "table_name"]),
    ("view_routine_usage", &["table_catalog", "table_schema", "table_name", "specific_catalog", "specific_schema", "specific_name"]),
    ("columns_extensions", &["table_schema", "table_name", "column_name", "engine_attribute", "secondary_engine_attribute"]),
    ("tables_extensions", &["table_schema", "table_name", "engine_attribute", "secondary_engine_attribute"]),
    ("table_constraints_extensions", &["constraint_schema", "constraint_name", "table_name", "engine_attribute"]),
    ("libraries", &["library_schema", "library_name", "library_definition"]),
    ("routine_libraries", &["routine_schema", "routine_name", "library_name"]),
    ("keywords", &["word", "reserved"]),
    ("innodb_tables", &["table_id", "name", "flag", "n_cols"]),
    ("innodb_trx", &["trx_id", "trx_state", "trx_started"]),
];

fn t(s: &str) -> Value {
    Value::text(s)
}

fn yes_no(b: bool) -> Value {
    t(if b { "YES" } else { "NO" })
}

/// The schema part of a table name (`test` for a plain one).
fn schema_of(name: &str) -> &str {
    name.split_once('.').map_or("test", |(s, _)| s)
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn opt(s: Option<String>) -> Value {
    s.map_or(Value::Null, |s| Value::text(&s))
}

impl Database {
    /// The columns and rows of `INFORMATION_SCHEMA.<view>`.
    pub(crate) fn information_schema(&self, view: &str) -> Result<(Vec<String>, Vec<Row>)> {
        let mut rows: Vec<Row> = Vec::new();
        let cols = match view.to_ascii_lowercase().as_str() {
            "tables" => {
                for tb in self.tables.values() {
                    let auto = if tb.schema().autonumber_column().is_some() { Value::Int(tb.next_auto()) } else { Value::Null };
                    rows.push(vec![
                        t(tb.name()),
                        t("BASE TABLE"),
                        Value::Int(tb.len() as i64),
                        t("def"),
                        t(schema_of(tb.name())),
                        t("InnoDB"),
                        Value::Int(tb.len() as i64),
                        t("Dynamic"),
                        t(""),
                        t("utf8mb4_0900_ai_ci"),
                        auto,
                        Value::Null,
                        Value::Null,
                        Value::Int(0),
                        Value::Int(0),
                        Value::Int(0),
                        Value::Int(10),
                        t(""),
                    ]);
                }
                for (name, _) in self.views.values() {
                    rows.push(vec![
                        t(name),
                        t("VIEW"),
                        Value::Null,
                        t("def"),
                        t(schema_of(name)),
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        t("VIEW"),
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                    ]);
                }
                names(&["table_name", "table_type", "row_count", "table_catalog", "table_schema", "engine", "table_rows", "row_format", "table_comment", "table_collation", "auto_increment", "create_time", "update_time", "data_length", "index_length", "avg_row_length", "version", "create_options"])
            }
            "columns" => {
                for tb in self.tables.values() {
                    for (i, c) in tb.schema().columns.iter().enumerate() {
                        rows.push(vec![
                            t(tb.name()),
                            t(&c.name),
                            Value::Int(i as i64 + 1),
                            t(c.data_type.sql_name()),
                            yes_no(!c.not_null),
                            match (&c.default, c.is_calculated()) {
                                (_, true) | (None, _) => Value::Null,
                                (Some(Value::Null), _) => Value::Null,
                                (Some(v), _) => match (c.sequence_default(), c.expr_default()) {
                                    (Some(seq), _) => t(&format!("NEXT VALUE FOR {seq}")),
                                    (None, Some(src)) => t(src),
                                    (None, None) => t(&v.to_string()),
                                },
                            },
                            Value::Bool(c.primary_key),
                            Value::Bool(c.autonumber),
                            opt(c.expression().map(str::to_string)),
                            t("def"),
                            t(schema_of(tb.name())),
                            t(super::mysqlshow::mysql_type(c.data_type)),
                            t(if c.primary_key { "PRI" } else { "" }),
                            t(if c.autonumber { "auto_increment" } else { "" }),
                            if c.data_type == crate::value::DataType::Text { t("utf8mb4") } else { Value::Null },
                            if c.data_type == crate::value::DataType::Text { t("utf8mb4_0900_ai_ci") } else { Value::Null },
                            t(""),
                            Value::Null,
                            Value::Null,
                            Value::Null,
                            Value::Null,
                            t(""),
                            t("select,insert,update,references"),
                        ]);
                    }
                }
                // The columns of a query are what it returns. (A view that reads this very view, as one over
                // INFORMATION_SCHEMA.COLUMNS does, would never finish: inside the probe, views add no columns.)
                let probing = PROBING_VIEWS.with(|p| p.replace(true));
                for (name, _) in self.views.values().filter(|_| !probing) {
                    let probe = crate::sql::ast::Select {
                        items: vec![crate::sql::ast::SelectItem::Wildcard],
                        from: Some(crate::sql::ast::From {
                            first: crate::sql::ast::TableFactor::Named { name: name.clone(), alias: None, only: false },
                            joins: Vec::new(),
                        }),
                        limit: Some(0),
                        ..Default::default()
                    };
                    if let Ok(res) = self.unlogged(|| self.run_select(&probe)) {
                        for (i, c) in res.columns.iter().enumerate() {
                            rows.push(vec![
                                t(name),
                                t(c),
                                Value::Int(i as i64 + 1),
                                Value::Null,
                                Value::Null,
                                Value::Null,
                                Value::Bool(false),
                                Value::Bool(false),
                                Value::Null,
                                t("def"),
                                t(schema_of(name)),
                                Value::Null,
                                t(""),
                                t(""),
                                Value::Null,
                                Value::Null,
                                t(""),
                                Value::Null,
                                Value::Null,
                                Value::Null,
                                Value::Null,
                                t(""),
                                t("select,insert,update,references"),
                            ]);
                        }
                    }
                }
                PROBING_VIEWS.with(|p| p.set(probing));
                names(&["table_name", "column_name", "ordinal_position", "data_type", "is_nullable", "column_default", "is_primary_key", "is_autonumber", "calculated_expression", "table_catalog", "table_schema", "column_type", "column_key", "extra", "character_set_name", "collation_name", "column_comment", "character_maximum_length", "numeric_precision", "numeric_scale", "datetime_precision", "generation_expression", "privileges"])
            }
            "views" => {
                let nested = LISTING_VIEWS.with(|p| p.replace(true));
                for (name, sql) in self.views.values() {
                    let updatable = !nested && self.view_target(name).is_ok();
                    rows.push(vec![
                        t(name),
                        t(sql.trim_start_matches("/*CHECK OPTION*/").trim()),
                        yes_no(updatable),
                        yes_no(sql.trim_start().starts_with("/*CHECK OPTION*/")),
                    ]);
                }
                LISTING_VIEWS.with(|p| p.set(nested));
                names(&["table_name", "view_definition", "is_updatable", "check_option"])
            }
            "table_constraints" => {
                for tb in self.tables.values() {
                    for ix in tb.index_defs().filter(|d| d.unique) {
                        let primary = tb.schema().columns.iter().enumerate().filter(|(_, c)| c.primary_key).map(|(i, _)| i).collect::<Vec<_>>() == ix.columns && ix.name == "PrimaryKey";
                        rows.push(vec![t(&ix.name), t(tb.name()), t(if primary { "PRIMARY KEY" } else { "UNIQUE" })]);
                    }
                    for fk in &tb.schema().foreign_keys {
                        rows.push(vec![t(&fk.name), t(tb.name()), t("FOREIGN KEY")]);
                    }
                }
                names(&["constraint_name", "table_name", "constraint_type"])
            }
            "key_column_usage" => {
                for tb in self.tables.values() {
                    let col = |i: usize| tb.schema().columns[i].name.clone();
                    for ix in tb.index_defs().filter(|d| d.unique) {
                        for (k, &c) in ix.columns.iter().enumerate() {
                            rows.push(vec![t(&ix.name), t(tb.name()), t(&col(c)), Value::Int(k as i64 + 1), Value::Null, Value::Null]);
                        }
                    }
                    for fk in &tb.schema().foreign_keys {
                        let parent = self.table(&fk.ref_table);
                        for (k, &c) in fk.columns.iter().enumerate() {
                            let rc = parent.and_then(|p| fk.ref_columns.get(k).map(|&i| p.schema().columns[i].name.clone()));
                            rows.push(vec![t(&fk.name), t(tb.name()), t(&col(c)), Value::Int(k as i64 + 1), t(&fk.ref_table), opt(rc)]);
                        }
                    }
                }
                names(&["constraint_name", "table_name", "column_name", "ordinal_position", "referenced_table", "referenced_column"])
            }
            "referential_constraints" => {
                for tb in self.tables.values() {
                    for fk in &tb.schema().foreign_keys {
                        rows.push(vec![
                            t(&fk.name),
                            t(tb.name()),
                            t(&fk.ref_table),
                            t(fk.on_update.sql()),
                            t(fk.on_delete.sql()),
                            yes_no(fk.deferrable),
                            yes_no(fk.initially_deferred),
                        ]);
                    }
                }
                names(&["constraint_name", "table_name", "referenced_table", "update_rule", "delete_rule", "is_deferrable", "initially_deferred"])
            }
            "indexes" => {
                for tb in self.tables.values() {
                    for ix in tb.index_defs() {
                        for (k, &c) in ix.columns.iter().enumerate() {
                            rows.push(vec![
                                t(tb.name()),
                                t(&ix.name),
                                t(&tb.schema().columns[c].name),
                                Value::Int(k as i64 + 1),
                                Value::Bool(ix.unique),
                                Value::Bool(ix.is_desc(k)),
                            ]);
                        }
                    }
                }
                names(&["table_name", "index_name", "column_name", "ordinal_position", "is_unique", "is_descending"])
            }
            "routines" => {
                for r in self.routines.values() {
                    rows.push(vec![
                        t(&r.name),
                        t(if r.function { "FUNCTION" } else { "PROCEDURE" }),
                        r.returns.map_or(Value::Null, |d| t(d.sql_name())),
                        t(&r.sql),
                        t("test"),
                        t("def"),
                        t(&r.name),
                        t("SQL"),
                        Value::Null,
                        Value::Null,
                        t("SQL"),
                        t("NO"),
                        t("CONTAINS SQL"),
                        t("DEFINER"),
                        t("root@localhost"),
                        t(""),
                    ]);
                }
                names(&["routine_name", "routine_type", "data_type", "routine_definition", "routine_schema", "routine_catalog", "specific_name", "routine_body", "external_name", "external_language", "parameter_style", "is_deterministic", "sql_data_access", "security_type", "definer", "routine_comment"])
            }
            "parameters" => {
                for r in self.routines.values() {
                    for (i, p) in r.params.iter().enumerate() {
                        rows.push(vec![
                            t(&r.name),
                            t(&p.name),
                            Value::Int(i as i64 + 1),
                            p.data_type.map_or(Value::Null, |d| t(d.sql_name())),
                            Value::Bool(p.default.is_some()),
                        ]);
                    }
                }
                names(&["routine_name", "parameter_name", "ordinal_position", "data_type", "has_default"])
            }
            "sequences" => {
                for s in self.sequences() {
                    rows.push(vec![
                        t(&s.name),
                        Value::Int(s.start),
                        Value::Int(s.increment),
                        Value::Int(s.min),
                        Value::Int(s.max),
                        yes_no(s.cycle),
                        s.last.map_or(Value::Null, Value::Int),
                    ]);
                }
                names(&["sequence_name", "start_value", "increment", "minimum_value", "maximum_value", "cycle_option", "last_value"])
            }
            "triggers" => {
                for tr in self.triggers.values() {
                    let events: Vec<&str> = tr
                        .events
                        .iter()
                        .map(|e| match e {
                            TriggerEvent::Insert => "INSERT",
                            TriggerEvent::Update => "UPDATE",
                            TriggerEvent::Delete => "DELETE",
                        })
                        .collect();
                    rows.push(vec![
                        t(&tr.name),
                        t(&tr.table),
                        t(match tr.timing {
                            TriggerTiming::Before => "BEFORE",
                            TriggerTiming::After => "AFTER",
                            TriggerTiming::InsteadOf => "INSTEAD OF",
                        }),
                        t(&events.join(", ")),
                        t(if tr.each_statement { "STATEMENT" } else { "ROW" }),
                        t(&tr.sql),
                        t("test"),
                        t("def"),
                        t("test"),
                        Value::Int(1),
                        t("root@localhost"),
                    ]);
                }
                names(&["trigger_name", "event_object_table", "action_timing", "event_manipulation", "action_orientation", "action_statement", "trigger_schema", "trigger_catalog", "event_object_schema", "action_order", "definer"])
            }
            "reports" => {
                for r in self.reports.values() {
                    rows.push(vec![t(&r.name), t(&r.sql)]);
                }
                names(&["report_name", "report_definition"])
            }
            "schemata" => {
                let mut dbs: Vec<String> = ["information_schema", "mysql", "performance_schema", "sys", "test"].iter().map(|s| s.to_string()).collect();
                dbs.extend(self.catalog.keys().filter(|(k, _)| k == "schema").map(|(_, n)| n.clone()));
                dbs.sort();
                for d in dbs {
                    rows.push(vec![t("def"), t(&d), t("utf8mb4"), t("utf8mb4_0900_ai_ci"), Value::Null]);
                }
                names(&["catalog_name", "schema_name", "default_character_set_name", "default_collation_name", "sql_path"])
            }
            "statistics" => {
                for tb in self.tables.values() {
                    let schema = tb.schema();
                    for d in &schema.indexes {
                        for (seq, &c) in d.columns.iter().enumerate() {
                            rows.push(vec![
                                t("def"),
                                t(schema_of(tb.name())),
                                t(tb.name()),
                                Value::Int(i64::from(!d.unique)),
                                t(schema_of(tb.name())),
                                t(if d.name == "PrimaryKey" { "PRIMARY" } else { &d.name }),
                                Value::Int(seq as i64 + 1),
                                t(&schema.columns[c].name),
                                t("A"),
                                Value::Null,
                                Value::Null,
                                Value::Null,
                                t(if schema.columns[c].not_null { "" } else { "YES" }),
                                t("BTREE"),
                                t("YES"),
                                t(""),
                                t(""),
                                Value::Null,
                            ]);
                        }
                    }
                }
                names(&["table_catalog", "table_schema", "table_name", "non_unique", "index_schema", "index_name", "seq_in_index", "column_name", "collation", "cardinality", "sub_part", "packed", "nullable", "index_type", "is_visible", "comment", "index_comment", "expression"])
            }
            "character_sets" => {
                for (cs, desc, coll, len) in [("binary", "Binary pseudo charset", "binary", 1), ("latin1", "cp1252 West European", "latin1_swedish_ci", 1), ("utf8mb3", "UTF-8 Unicode", "utf8mb3_general_ci", 3), ("utf8mb4", "UTF-8 Unicode", "utf8mb4_0900_ai_ci", 4)] {
                    rows.push(vec![t(cs), t(coll), t(desc), Value::Int(len)]);
                }
                names(&["character_set_name", "default_collate_name", "description", "maxlen"])
            }
            "collations" => {
                rows.push(vec![t("utf8mb4_0900_ai_ci"), t("utf8mb4"), Value::Int(255), t("Yes"), t("Yes"), Value::Int(0)]);
                names(&["collation_name", "character_set_name", "id", "is_default", "is_compiled", "sortlen"])
            }
            "engines" => {
                rows.push(vec![t("InnoDB"), t("DEFAULT"), t("Supports transactions, row-level locking, and foreign keys"), t("YES"), t("YES"), t("YES")]);
                names(&["engine", "support", "comment", "transactions", "xa", "savepoints"])
            }
            "partitions" => {
                for tb in self.tables.values() {
                    rows.push(vec![t("def"), t(schema_of(tb.name())), t(tb.name()), Value::Null, Value::Null, Value::Null, Value::Int(tb.len() as i64), Value::Null]);
                }
                names(&["table_catalog", "table_schema", "table_name", "partition_name", "partition_method", "partition_expression", "table_rows", "partition_description"])
            }
            "check_constraints" => names(&["constraint_catalog", "constraint_schema", "constraint_name", "check_clause"]),
            "foreign_data_wrappers" | "foreign_data_wrapper_options" | "foreign_servers" | "foreign_server_options" | "user_mappings" | "user_mapping_options" | "foreign_tables" | "foreign_table_options" => {
                let db = "regression";
                let s = |d: &serde_json::Value, k: &str| d.get(k).and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map_or(Value::Null, t);
                let options = |d: &serde_json::Value| -> Vec<(String, String)> {
                    d["options"].as_array().map(|o| o.iter().map(|p| (p[0].as_str().unwrap_or("").to_string(), p[1].as_str().unwrap_or("").to_string())).collect()).unwrap_or_default()
                };
                match view.to_ascii_lowercase().as_str() {
                    "foreign_data_wrappers" => {
                        for (_, d) in self.foreign_objects("fdw") {
                            rows.push(vec![t(db), s(&d, "name"), s(&d, "owner"), Value::Null, t("c")]);
                        }
                        names(&["foreign_data_wrapper_catalog", "foreign_data_wrapper_name", "authorization_identifier", "library_name", "foreign_data_wrapper_language"])
                    }
                    "foreign_data_wrapper_options" => {
                        for (_, d) in self.foreign_objects("fdw") {
                            for (k, v) in options(&d) {
                                rows.push(vec![t(db), s(&d, "name"), t(&k), t(&v)]);
                            }
                        }
                        names(&["foreign_data_wrapper_catalog", "foreign_data_wrapper_name", "option_name", "option_value"])
                    }
                    "foreign_servers" => {
                        for (_, d) in self.foreign_objects("server") {
                            rows.push(vec![t(db), s(&d, "name"), t(db), s(&d, "fdw"), s(&d, "type"), s(&d, "version"), s(&d, "owner")]);
                        }
                        names(&["foreign_server_catalog", "foreign_server_name", "foreign_data_wrapper_catalog", "foreign_data_wrapper_name", "foreign_server_type", "foreign_server_version", "authorization_identifier"])
                    }
                    "foreign_server_options" => {
                        for (_, d) in self.foreign_objects("server") {
                            for (k, v) in options(&d) {
                                rows.push(vec![t(db), s(&d, "name"), t(&k), t(&v)]);
                            }
                        }
                        names(&["foreign_server_catalog", "foreign_server_name", "option_name", "option_value"])
                    }
                    "user_mappings" => {
                        for (_, d) in self.foreign_objects("umapping") {
                            rows.push(vec![s(&d, "user"), t(db), s(&d, "server")]);
                        }
                        names(&["authorization_identifier", "foreign_server_catalog", "foreign_server_name"])
                    }
                    "user_mapping_options" => {
                        for (_, d) in self.foreign_objects("umapping") {
                            for (k, v) in options(&d) {
                                rows.push(vec![s(&d, "user"), t(db), s(&d, "server"), t(&k), t(&v)]);
                            }
                        }
                        names(&["authorization_identifier", "foreign_server_catalog", "foreign_server_name", "option_name", "option_value"])
                    }
                    "foreign_tables" => {
                        for (_, d) in self.foreign_objects("ftable") {
                            let full = d["name"].as_str().unwrap_or("").to_string();
                            rows.push(vec![t(db), t(if full.contains('.') { schema_of(&full) } else { "public" }), t(full.rsplit('.').next().unwrap_or("")), t(db), s(&d, "server")]);
                        }
                        names(&["foreign_table_catalog", "foreign_table_schema", "foreign_table_name", "foreign_server_catalog", "foreign_server_name"])
                    }
                    _ => {
                        for (_, d) in self.foreign_objects("ftable") {
                            let full = d["name"].as_str().unwrap_or("").to_string();
                            for (k, v) in options(&d) {
                                rows.push(vec![t(db), t(if full.contains('.') { schema_of(&full) } else { "public" }), t(full.rsplit('.').next().unwrap_or("")), t(&k), t(&v)]);
                            }
                        }
                        names(&["foreign_table_catalog", "foreign_table_schema", "foreign_table_name", "option_name", "option_value"])
                    }
                }
            }
            other if EMPTY_VIEWS.iter().any(|(n, _)| *n == other) => {
                let (_, cols) = EMPTY_VIEWS.iter().find(|(n, _)| *n == other).expect("checked");
                names(cols)
            }
            other => {
                return Err(Error::catalog(format!("INFORMATION_SCHEMA has no view '{other}' (it has {VIEWS})")));
            }
        };
        Ok((cols, rows))
    }
}
