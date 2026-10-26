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
                    rows.push(vec![t(tb.name()), t("BASE TABLE"), Value::Int(tb.len() as i64), t("def"), t(schema_of(tb.name())), t("InnoDB")]);
                }
                for (name, _) in self.views.values() {
                    rows.push(vec![t(name), t("VIEW"), Value::Null, t("def"), t(schema_of(name)), Value::Null]);
                }
                names(&["table_name", "table_type", "row_count", "table_catalog", "table_schema", "engine"])
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
                                (Some(v), _) => match c.sequence_default() {
                                    Some(seq) => t(&format!("NEXT VALUE FOR {seq}")),
                                    None => t(&v.to_string()),
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
                        ]);
                    }
                }
                // The columns of a query are what it returns.
                for (name, _) in self.views.values() {
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
                            ]);
                        }
                    }
                }
                names(&["table_name", "column_name", "ordinal_position", "data_type", "is_nullable", "column_default", "is_primary_key", "is_autonumber", "calculated_expression", "table_catalog", "table_schema", "column_type", "column_key", "extra", "character_set_name", "collation_name"])
            }
            "views" => {
                for (name, sql) in self.views.values() {
                    let target = self.view_target(name);
                    rows.push(vec![
                        t(name),
                        t(sql.trim_start_matches("/*CHECK OPTION*/").trim()),
                        yes_no(target.is_ok()),
                        yes_no(sql.trim_start().starts_with("/*CHECK OPTION*/")),
                    ]);
                }
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
                    ]);
                }
                names(&["routine_name", "routine_type", "data_type", "routine_definition"])
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
                    ]);
                }
                names(&["trigger_name", "event_object_table", "action_timing", "event_manipulation", "action_orientation", "action_statement"])
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
                            ]);
                        }
                    }
                }
                names(&["table_catalog", "table_schema", "table_name", "non_unique", "index_schema", "index_name", "seq_in_index", "column_name", "collation", "cardinality", "sub_part", "packed", "nullable", "index_type"])
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
                    rows.push(vec![t("def"), t(schema_of(tb.name())), t(tb.name()), Value::Null, Value::Null, Value::Null, Value::Int(tb.len() as i64)]);
                }
                names(&["table_catalog", "table_schema", "table_name", "partition_name", "partition_method", "partition_expression", "table_rows"])
            }
            "check_constraints" => names(&["constraint_catalog", "constraint_schema", "constraint_name", "check_clause"]),
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
