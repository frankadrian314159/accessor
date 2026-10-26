//! Storage layer: tables, the logical operation log, snapshots.

pub mod mvcc;
pub mod persist;
pub mod stats;
pub mod table;

use serde::{Deserialize, Serialize};

use crate::schema::{Column, ForeignKey, IndexDef, TableSchema};
use crate::sequence::SequenceDef;
use crate::value::{DataType, Row, Value};
pub use table::{RowId, Table};

/// A single logical mutation. Every change to a database — whether from SQL,
/// the GUI, replay of the write-ahead log, or undo — is expressed as an `Op`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Op {
    CreateTable(TableSchema),
    DropTable(String),
    CreateIndex { table: String, def: IndexDef },
    DropIndex { table: String, name: String },
    CreateView { name: String, sql: String },
    DropView(String),
    Insert { table: String, id: RowId, row: Row },
    Update { table: String, id: RowId, row: Row },
    Delete { table: String, id: RowId },
    // Appended after the original variants so existing logs keep decoding.
    AddColumn { table: String, column: Column },
    DropColumn { table: String, name: String },
    RenameColumn { table: String, from: String, to: String },
    AlterColumnType { table: String, name: String, data_type: DataType },
    AddForeignKey { table: String, fk: ForeignKey },
    DropForeignKey { table: String, name: String },
    CreateTrigger { name: String, sql: String },
    DropTrigger(String),
    SetExpression { table: String, name: String, expr: String },
    RenameTable { from: String, to: String },
    /// `ALTER COLUMN ... SET DEFAULT v` / `DROP DEFAULT` (`None`).
    SetDefault { table: String, name: String, value: Option<Value> },
    /// `ALTER COLUMN ... SET NOT NULL` / `DROP NOT NULL`.
    SetNotNull { table: String, name: String, not_null: bool },
    /// Whether a relationship may be (and starts out) deferred; the table's stored form does not carry it.
    SetForeignKeyTiming { table: String, name: String, deferrable: bool, initially_deferred: bool },
    /// A stored function or procedure, as its `CREATE` text.
    CreateRoutine { name: String, sql: String },
    /// A stored report, as its `CREATE REPORT` text.
    CreateReport { name: String, sql: String },
    DropReport(String),
    DropRoutine(String),
    CreateSequence(SequenceDef),
    DropSequence(String),
    /// `ALTER SEQUENCE`: replaces the definition.
    AlterSequence(SequenceDef),
    /// The last number a sequence handed out; written as it happens, outside any transaction.
    SequenceValue { name: String, last: Option<i64> },
    /// A named object of the catalog that has no table of its own (a schema, domain, type, role, grant, extension,
    /// rule, or a fact about a table such as its parents): `value` is its description as text, `None` removes it.
    Catalog { kind: String, name: String, value: Option<String> },
}

impl Op {
    pub fn is_dml(&self) -> bool {
        matches!(self, Op::Insert { .. } | Op::Update { .. } | Op::Delete { .. })
    }

    pub fn is_ddl(&self) -> bool {
        !self.is_dml()
    }
}
