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
const VIEWS: &str = "TABLES, COLUMNS, VIEWS, TABLE_CONSTRAINTS, KEY_COLUMN_USAGE, REFERENTIAL_CONSTRAINTS, INDEXES, ROUTINES, PARAMETERS, SEQUENCES, TRIGGERS, REPORTS";

fn t(s: &str) -> Value {
    Value::text(s)
}

fn yes_no(b: bool) -> Value {
    t(if b { "YES" } else { "NO" })
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
                    rows.push(vec![t(tb.name()), t("BASE TABLE"), Value::Int(tb.len() as i64)]);
                }
                for (name, _) in self.views.values() {
                    rows.push(vec![t(name), t("VIEW"), Value::Null]);
                }
                names(&["table_name", "table_type", "row_count"])
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
                        ]);
                    }
                }
                // The columns of a query are what it returns.
                for (name, _) in self.views.values() {
                    let probe = crate::sql::ast::Select {
                        items: vec![crate::sql::ast::SelectItem::Wildcard],
                        from: Some(crate::sql::ast::From {
                            first: crate::sql::ast::TableFactor::Named { name: name.clone(), alias: None },
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
                            ]);
                        }
                    }
                }
                names(&["table_name", "column_name", "ordinal_position", "data_type", "is_nullable", "column_default", "is_primary_key", "is_autonumber", "calculated_expression"])
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
            other => {
                return Err(Error::catalog(format!("INFORMATION_SCHEMA has no view '{other}' (it has {VIEWS})")));
            }
        };
        Ok((cols, rows))
    }
}
