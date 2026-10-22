//! Syntax tree for the supported SQL dialect.

use crate::schema::RefAction;
use crate::value::{DataType, Value};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    IntDiv,
    Mod,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Literal(Value),
    Column { table: Option<String>, name: String },
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Binary { op: BinOp, left: Box<Expr>, right: Box<Expr> },
    IsNull { expr: Box<Expr>, negated: bool },
    Like { expr: Box<Expr>, pattern: Box<Expr>, negated: bool },
    Between { expr: Box<Expr>, low: Box<Expr>, high: Box<Expr>, negated: bool },
    InList { expr: Box<Expr>, list: Vec<Expr>, negated: bool },
    InSelect { expr: Box<Expr>, query: Box<Select>, negated: bool },
    Exists { query: Box<Select>, negated: bool },
    Subquery(Box<Select>),
    Function { name: String, args: Vec<Expr>, distinct: bool, star: bool },
    Case { whens: Vec<(Expr, Expr)>, else_expr: Option<Box<Expr>> },
    /// `@name`, bound when a parameterised query runs.
    Param(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SelectItem {
    Wildcard,
    QualifiedWildcard(String),
    Expr { expr: Expr, alias: Option<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum TableFactor {
    Named { name: String, alias: Option<String> },
    Derived { query: Box<Select>, alias: String },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Cross,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Join {
    pub kind: JoinKind,
    pub factor: TableFactor,
    pub on: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct From {
    pub first: TableFactor,
    pub joins: Vec<Join>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderItem {
    pub expr: Expr,
    pub desc: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Select {
    pub distinct: bool,
    pub top: Option<usize>,
    pub items: Vec<SelectItem>,
    pub from: Option<From>,
    pub filter: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub order_by: Vec<OrderItem>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub not_null: bool,
    pub primary_key: bool,
    pub unique: bool,
    pub autonumber: bool,
    pub default: Option<Value>,
    pub references: Option<ForeignKeyDef>,
    /// Source text of the expression of a `CALCULATED` field.
    pub expression: Option<String>,
}

/// `FOREIGN KEY (columns) REFERENCES ref_table (ref_columns)`; an empty
/// `ref_columns` means the referenced table's primary key.
#[derive(Clone, Debug, PartialEq)]
pub struct ForeignKeyDef {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: RefAction,
    pub on_update: RefAction,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AlterAction {
    AddColumn(ColumnDef),
    DropColumn(String),
    RenameColumn { from: String, to: String },
    AddForeignKey(ForeignKeyDef),
    DropConstraint(String),
    AlterColumn { name: String, data_type: DataType },
    /// `ALTER COLUMN c CALCULATED AS (expr)`: change a calculated field's expression.
    SetExpression { name: String, expr: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum InsertSource {
    Values(Vec<Vec<Expr>>),
    Select(Box<Select>),
}

/// One entry of a `PARAMETERS @a [type], @b;` header.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamDecl {
    pub name: String,
    pub data_type: Option<DataType>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TriggerTiming {
    Before,
    After,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TriggerEvent {
    Insert,
    Update,
    Delete,
}

/// A row-level trigger. `sql` is the original `CREATE TRIGGER` text, which is
/// what gets stored.
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerDef {
    pub name: String,
    pub timing: TriggerTiming,
    pub event: TriggerEvent,
    pub table: String,
    pub when: Option<Expr>,
    pub body: Vec<Statement>,
    pub sql: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    Select(Select),
    Insert { table: String, columns: Option<Vec<String>>, source: InsertSource },
    Update { table: String, assignments: Vec<(String, Expr)>, filter: Option<Expr> },
    Delete { table: String, filter: Option<Expr> },
    CreateTable {
        name: String,
        columns: Vec<ColumnDef>,
        primary_key: Vec<String>,
        uniques: Vec<Vec<String>>,
        foreign_keys: Vec<ForeignKeyDef>,
    },
    DropTable(String),
    AlterTable { table: String, action: AlterAction },
    CreateIndex { name: String, table: String, columns: Vec<String>, unique: bool },
    DropIndex { name: String, table: String },
    CreateView { name: String, sql: String },
    DropView(String),
    /// Header of a parameterised query; a no-op when executed on its own.
    Parameters(Vec<ParamDecl>),
    Exec { name: String, args: Vec<(Option<String>, Expr)> },
    /// Aborts the statement with a message (trigger bodies).
    Raise(Expr),
    CreateTrigger(Box<TriggerDef>),
    DropTrigger(String),
    Begin,
    Commit,
    Rollback,
    Savepoint(String),
    ReleaseSavepoint(String),
    RollbackTo(String),
}
