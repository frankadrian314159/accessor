//! Storage layer: tables, the logical operation log, snapshots.

pub mod persist;
pub mod table;

use serde::{Deserialize, Serialize};

use crate::schema::{Column, ForeignKey, IndexDef, TableSchema};
use crate::value::{DataType, Row};
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
}

impl Op {
    pub fn is_dml(&self) -> bool {
        matches!(self, Op::Insert { .. } | Op::Update { .. } | Op::Delete { .. })
    }

    pub fn is_ddl(&self) -> bool {
        !self.is_dml()
    }
}
