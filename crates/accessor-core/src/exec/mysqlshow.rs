//! MySQL's `SHOW ...` statements, answered from the catalog.
//!
//! `SHOW TABLES`, `DATABASES`, `COLUMNS`, `INDEX`, `CREATE TABLE`, `VARIABLES`, `TABLE STATUS`, `ENGINES`, `CHARACTER SET`
//! and `WARNINGS` give rows in the shape MySQL uses (the engine is always `InnoDB`, the character set `utf8mb4`).
//! The ones that describe a server (`PROCESSLIST`, `GRANTS`, `STATUS`, `PLUGINS`, ...) give no rows.

use std::sync::Arc;

use super::QueryResult;
use crate::database::{key, Database};
use crate::error::{Error, Result};
use crate::schema::TableSchema;
use crate::sql::ast::{Expr, From as FromClause, Select, SelectItem, ShowStmt, TableFactor};
use crate::value::{DataType, Row, Value};

fn text(s: &str) -> Value {
    Value::text(s)
}

/// `LIKE` with `%` and `_`, ignoring case.
fn like(pattern: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.to_lowercase().chars().collect(), s.to_lowercase().chars().collect());
    fn go(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('%') => (0..=t.len()).any(|k| go(&p[1..], &t[k..])),
            Some('_') => !t.is_empty() && go(&p[1..], &t[1..]),
            Some('\\') if p.len() > 1 => t.first() == Some(&p[1]) && go(&p[2..], &t[1..]),
            Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
        }
    }
    go(&p, &t)
}

pub(crate) fn mysql_type(t: DataType) -> &'static str {
    match t {
        DataType::Integer => "int",
        DataType::Bignum => "decimal(65,0)",
        DataType::Float => "float",
        DataType::Double => "double",
        DataType::Decimal => "decimal(10,0)",
        DataType::Currency => "decimal(19,4)",
        DataType::Rational => "text",
        DataType::Text => "text",
        DataType::YesNo => "tinyint(1)",
        DataType::DateTime => "datetime",
        DataType::TimestampTz => "timestamp",
        DataType::Interval => "time",
        DataType::Json => "json",
        DataType::Array(_) => "json",
        DataType::Calculated => "text",
    }
}

fn quoted(s: &str) -> String {
    format!("`{}`", s.replace('`', "``"))
}

impl Database {
    pub(crate) fn exec_show(&self, s: &ShowStmt) -> Result<QueryResult> {
        let (columns, mut rows) = self.show_rows(s)?;
        if let Some(p) = &s.like {
            rows.retain(|r| r.first().is_some_and(|v| like(p, &v.to_string())));
        }
        if let Some(filter) = &s.filter {
            let select = Select {
                items: vec![SelectItem::Wildcard],
                from: Some(FromClause {
                    first: TableFactor::Materialized { rows: Arc::new(rows), columns: columns.clone(), alias: "show".into() },
                    joins: Vec::new(),
                }),
                filter: Some(filter.clone() as Expr),
                ..Select::default()
            };
            return self.run_select(&select);
        }
        Ok(QueryResult { columns, rows, affected: 0 })
    }

    fn show_schema(&self, name: &str) -> Result<TableSchema> {
        Ok(self.table_checked(name)?.schema().clone())
    }

    fn show_rows(&self, s: &ShowStmt) -> Result<(Vec<String>, Vec<Row>)> {
        let cols = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let target = s.target.clone().unwrap_or_default();
        Ok(match s.what.as_str() {
            "tables" => {
                let mut names: Vec<(String, &str)> = self
                    .table_names()
                    .into_iter()
                    .filter(|n| !n.contains('.'))
                    .map(|n| (n.to_string(), "BASE TABLE"))
                    .chain(self.view_names().into_iter().filter(|n| !n.contains('.')).map(|n| (n.to_string(), "VIEW")))
                    .collect();
                names.sort_by_key(|(n, _)| n.to_lowercase());
                if s.full {
                    (cols(&["Tables_in_test", "Table_type"]), names.into_iter().map(|(n, t)| vec![text(&n), text(t)]).collect())
                } else {
                    (cols(&["Tables_in_test"]), names.into_iter().map(|(n, _)| vec![text(&n)]).collect())
                }
            }
            "databases" => {
                let mut dbs: Vec<String> = ["information_schema", "mysql", "performance_schema", "sys", "test"].iter().map(|s| s.to_string()).collect();
                dbs.extend(self.catalog.keys().filter(|(k, _)| k == "schema").map(|(_, n)| n.clone()));
                dbs.sort();
                (cols(&["Database"]), dbs.into_iter().map(|d| vec![text(&d)]).collect())
            }
            "columns" => {
                let schema = self.show_schema(&target)?;
                let pk: Vec<usize> = (0..schema.columns.len()).filter(|&i| schema.columns[i].primary_key).collect();
                let mut rows = Vec::new();
                for (i, c) in schema.columns.iter().enumerate() {
                    let unique = schema.indexes.iter().any(|d| d.unique && d.columns.first() == Some(&i) && d.columns.len() == 1);
                    let multi = schema.indexes.iter().any(|d| d.columns.first() == Some(&i));
                    let keyflag = if pk.contains(&i) { "PRI" } else if unique { "UNI" } else if multi { "MUL" } else { "" };
                    let default = match &c.default {
                        _ if c.now_default() => text("CURRENT_TIMESTAMP"),
                        Some(v) if !v.is_null() && c.sequence_default().is_none() => text(&v.to_string()),
                        _ => Value::Null,
                    };
                    let extra = if c.autonumber { "auto_increment" } else { "" };
                    let null = if c.not_null || c.primary_key { "NO" } else { "YES" };
                    if s.full {
                        rows.push(vec![text(&c.name), text(mysql_type(c.data_type)), if c.data_type == DataType::Text { text("utf8mb4_0900_ai_ci") } else { Value::Null }, text(null), text(keyflag), default, text(extra), text("select,insert,update,references"), text("")]);
                    } else {
                        rows.push(vec![text(&c.name), text(mysql_type(c.data_type)), text(null), text(keyflag), default, text(extra)]);
                    }
                }
                let names = if s.full {
                    cols(&["Field", "Type", "Collation", "Null", "Key", "Default", "Extra", "Privileges", "Comment"])
                } else {
                    cols(&["Field", "Type", "Null", "Key", "Default", "Extra"])
                };
                (names, rows)
            }
            "index" => {
                let schema = self.show_schema(&target)?;
                let mut rows = Vec::new();
                for d in &schema.indexes {
                    for (seq, &c) in d.columns.iter().enumerate() {
                        let col = &schema.columns[c];
                        rows.push(vec![
                            text(&schema.name),
                            Value::Int(i64::from(!d.unique)),
                            text(if d.name == "PrimaryKey" { "PRIMARY" } else { &d.name }),
                            Value::Int(seq as i64 + 1),
                            text(&col.name),
                            text("A"),
                            Value::Null,
                            Value::Null,
                            Value::Null,
                            text(if col.not_null || col.primary_key { "" } else { "YES" }),
                            text("BTREE"),
                            text(""),
                            text(""),
                            text("YES"),
                            Value::Null,
                        ]);
                    }
                }
                (cols(&["Table", "Non_unique", "Key_name", "Seq_in_index", "Column_name", "Collation", "Cardinality", "Sub_part", "Packed", "Null", "Index_type", "Comment", "Index_comment", "Visible", "Expression"]), rows)
            }
            "create table" => {
                let schema = self.show_schema(&target)?;
                let mut lines: Vec<String> = Vec::new();
                for c in &schema.columns {
                    let mut l = format!("  {} {}", quoted(&c.name), mysql_type(c.data_type));
                    if c.not_null || c.primary_key {
                        l.push_str(" NOT NULL");
                    }
                    if c.autonumber {
                        l.push_str(" AUTO_INCREMENT");
                    } else if c.now_default() {
                        l.push_str(" DEFAULT CURRENT_TIMESTAMP");
                    } else if let Some(v) = c.default.as_ref().filter(|v| !v.is_null() && c.sequence_default().is_none()) {
                        l.push_str(&format!(" DEFAULT '{}'", v.to_string().replace('\'', "''")));
                    } else if !(c.not_null || c.primary_key) {
                        l.push_str(" DEFAULT NULL");
                    }
                    lines.push(l);
                }
                for d in &schema.indexes {
                    let names: Vec<String> = d.columns.iter().map(|&c| quoted(&schema.columns[c].name)).collect();
                    lines.push(if d.name == "PrimaryKey" {
                        format!("  PRIMARY KEY ({})", names.join(","))
                    } else if d.unique {
                        format!("  UNIQUE KEY {} ({})", quoted(&d.name), names.join(","))
                    } else {
                        format!("  KEY {} ({})", quoted(&d.name), names.join(","))
                    });
                }
                let ddl = format!("CREATE TABLE {} (\n{}\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci", quoted(&schema.name), lines.join(",\n"));
                (cols(&["Table", "Create Table"]), vec![vec![text(&schema.name), text(&ddl)]])
            }
            "create view" => {
                let sql = self.view_sql(&target).ok_or_else(|| Error::catalog(format!("Unknown table '{target}'")))?;
                (cols(&["View", "Create View", "character_set_client", "collation_connection"]), vec![vec![text(&target), text(&format!("CREATE VIEW {} AS {}", quoted(&target), sql)), text("utf8mb4"), text("utf8mb4_0900_ai_ci")]])
            }
            "variables" => (cols(&["Variable_name", "Value"]), self.mysql_variable_rows().into_iter().map(|(n, v)| vec![text(&n), text(&v)]).collect()),
            "status" => (cols(&["Variable_name", "Value"]), vec![vec![text("Threads_connected"), text("1")], vec![text("Uptime"), text("1")]]),
            "warnings" | "errors" => (cols(&["Level", "Code", "Message"]), Vec::new()),
            "engines" => (
                cols(&["Engine", "Support", "Comment", "Transactions", "XA", "Savepoints"]),
                vec![vec![text("InnoDB"), text("DEFAULT"), text("Supports transactions, row-level locking, and foreign keys"), text("YES"), text("YES"), text("YES")]],
            ),
            "character set" => (
                cols(&["Charset", "Description", "Default collation", "Maxlen"]),
                vec![
                    vec![text("binary"), text("Binary pseudo charset"), text("binary"), Value::Int(1)],
                    vec![text("latin1"), text("cp1252 West European"), text("latin1_swedish_ci"), Value::Int(1)],
                    vec![text("utf8mb3"), text("UTF-8 Unicode"), text("utf8mb3_general_ci"), Value::Int(3)],
                    vec![text("utf8mb4"), text("UTF-8 Unicode"), text("utf8mb4_0900_ai_ci"), Value::Int(4)],
                ],
            ),
            "collation" => (
                cols(&["Collation", "Charset", "Id", "Default", "Compiled", "Sortlen", "Pad_attribute"]),
                vec![vec![text("utf8mb4_0900_ai_ci"), text("utf8mb4"), Value::Int(255), text("Yes"), text("Yes"), Value::Int(0), text("NO PAD")]],
            ),
            "table status" => {
                let mut names: Vec<String> = self.table_names().into_iter().filter(|n| !n.contains('.')).map(String::from).collect();
                names.sort_by_key(|n| n.to_lowercase());
                let rows = names
                    .into_iter()
                    .map(|n| {
                        let len = self.table(&n).map_or(0, |t| t.estimated_len() as i64);
                        let mut r = vec![text(&n), text("InnoDB"), Value::Int(10), text("Dynamic"), Value::Int(len)];
                        r.extend(std::iter::repeat_n(Value::Null, 13));
                        r
                    })
                    .collect();
                (cols(&["Name", "Engine", "Version", "Row_format", "Rows", "Avg_row_length", "Data_length", "Max_data_length", "Index_length", "Data_free", "Auto_increment", "Create_time", "Update_time", "Check_time", "Collation", "Checksum", "Create_options", "Comment"]), rows)
            }
            other if other.starts_with("other ") => (cols(&["Name"]), Vec::new()),
            other => return Err(Error::runtime(format!("SHOW {other} is not available"))),
        })
    }
}

#[allow(dead_code)]
fn unused(_: &str) -> String {
    key("")
}
