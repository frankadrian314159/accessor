//! # accessor-core
//!
//! An embedded relational engine in the spirit of Microsoft Access / Jet:
//! typed tables with AutoNumber and a tower of numeric types (`INTEGER`, `BIGNUM`,
//! `FLOAT`, `DOUBLE`, `DECIMAL`, `CURRENCY`, `RATIONAL`), case-insensitive text,
//! `[bracketed names]`, `*`/`?` LIKE wildcards, saved queries, and a SQL
//! dialect covering joins, grouping, subqueries and transactions.
//!
//! Data lives in memory (rows in slot vectors, ordered B-tree indexes) and
//! is made durable with a checksummed write-ahead log plus atomic snapshots.
//!
//! ```
//! use accessor_core::{Database, Value};
//!
//! let mut db = Database::in_memory();
//! db.execute("CREATE TABLE Customers (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL)").unwrap();
//! db.execute("INSERT INTO Customers (Name) VALUES ('Ada'), ('Grace')").unwrap();
//! let r = db.query("SELECT Name FROM Customers WHERE Name LIKE 'g*'").unwrap();
//! assert_eq!(r.rows, vec![vec![Value::text("Grace")]]);
//! ```

pub mod csv;
pub mod decimal;
pub mod rational;
pub mod database;
pub mod datetime;
pub mod interval;
pub mod tz;
pub mod error;
pub mod exec;
pub mod expr;
mod calc;
mod numeric;
mod fk;
pub mod sequence;
pub mod report;
pub mod shared;
mod json;
mod jsonpath;
mod trigger;
pub mod schema;
pub mod sql;
pub mod storage;
pub mod value;

pub use database::Database;
pub use error::{Error, Result};
pub use exec::QueryResult;
pub use schema::{Column, ForeignKey, IndexDef, RefAction, TableSchema};
pub use storage::persist::{Durability, FILE_FORMAT_VERSION};
pub use exec::Prepared;
pub use report::ReportFormat;
pub use shared::{Connection, SharedDatabase};
pub use storage::stats::ColumnStats;
pub use storage::{RowId, Table};
pub use value::{DataType, Row, Value};
