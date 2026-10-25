//! Syntax tree for the supported SQL dialect.

use std::rc::Rc;

use crate::schema::RefAction;
use crate::value::{DataType, Row, Value};

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
    /// A window function call: `func(...) OVER (...)`.
    Window { func: Box<Expr>, spec: WindowSpec },
    Function { name: String, args: Vec<Expr>, distinct: bool, star: bool },
    Case { whens: Vec<(Expr, Expr)>, else_expr: Option<Box<Expr>> },
    /// `@name`, bound when a parameterised query runs.
    Param(String),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct WindowSpec {
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<OrderItem>,
    pub frame: Option<Frame>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FrameUnits {
    Rows,
    Range,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FrameBound {
    UnboundedPreceding,
    Preceding(usize),
    CurrentRow,
    Following(usize),
    UnboundedFollowing,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub units: FrameUnits,
    pub start: FrameBound,
    pub end: FrameBound,
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
    /// A parenthesised query. With `lateral`, it may read columns of the tables before it
    /// in the FROM clause and is evaluated once for each of their rows.
    Derived { query: Box<Select>, alias: String, columns: Vec<String>, lateral: bool },
    /// A table function such as `GENERATE_SERIES(1, 10)`; its arguments may read earlier tables.
    Function { name: String, args: Vec<Expr>, alias: String, columns: Vec<String> },
    /// Rows already computed (the result of a common table expression).
    Materialized { rows: Rc<Vec<Row>>, columns: Vec<String>, alias: String },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Join {
    pub kind: JoinKind,
    pub factor: TableFactor,
    pub on: Option<Expr>,
    /// `USING (a, b)`: join on the like-named columns, shown once.
    pub using: Vec<String>,
    /// `NATURAL JOIN`: `USING` all like-named columns.
    pub natural: bool,
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
    /// `NULLS FIRST` (`Some(true)`) or `NULLS LAST` (`Some(false)`); `None` leaves NULLs
    /// where they sort naturally: first when ascending, last when descending.
    pub nulls_first: Option<bool>,
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
    /// `UNION [ALL]` members after this one. When present, `order_by`, `limit`
    /// and `offset` apply to the combined result, not to this member alone.
    pub unions: Vec<SetPart>,
    /// `GROUP BY ROLLUP/CUBE/GROUPING SETS`: the grouping sets, as indexes into
    /// `group_by`. Empty for an ordinary GROUP BY.
    pub group_sets: Vec<Vec<usize>>,
    pub with: Option<Box<With>>,
    /// `TRANSFORM ... PIVOT ...`: this select supplies the row headings and the
    /// crosstab adds one column per pivot value.
    pub crosstab: Option<Box<Crosstab>>,
}

/// The `TRANSFORM aggregate ... PIVOT column [IN (values)]` parts of a crosstab query.
#[derive(Clone, Debug, PartialEq)]
pub struct Crosstab {
    /// The aggregate computed in each cell, such as `Sum(Amount)`.
    pub agg: Expr,
    /// The column or expression whose values become columns.
    pub pivot: Expr,
    /// Fixed column values; `None` means every distinct value of `pivot`.
    pub values: Option<Vec<Expr>>,
}

/// A `WITH [RECURSIVE]` clause.
#[derive(Clone, Debug, PartialEq)]
pub struct With {
    pub recursive: bool,
    pub ctes: Vec<Cte>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cte {
    pub name: String,
    pub columns: Vec<String>,
    pub query: Select,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SetOp {
    Union,
    Intersect,
    Except,
}

/// One `UNION [ALL] SELECT ...` member.
#[derive(Clone, Debug, PartialEq)]
pub struct SetPart {
    pub op: SetOp,
    pub all: bool,
    pub select: Select,
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
    /// Source text of a column `CHECK (...)`.
    pub check: Option<String>,
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
    AddCheck { name: Option<String>, expr: String },
    DropConstraint(String),
    AlterColumn { name: String, data_type: DataType },
    /// `ALTER COLUMN c CALCULATED AS (expr)`: change a calculated field's expression.
    SetExpression { name: String, expr: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum InsertSource {
    Values(Vec<Vec<Expr>>),
    DefaultValues,
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
        /// `CHECK (...)` constraints (name, expression text), column and table level.
        checks: Vec<(Option<String>, String)>,
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
    /// `TRUNCATE [TABLE] t`: delete every record.
    Truncate(String),
    /// `EXPLAIN query`: run the query and report how it was planned.
    Explain(Box<Select>),
    /// `DROP ... IF EXISTS` (`missing_ok`) or `CREATE ... IF NOT EXISTS`.
    Guarded { missing_ok: bool, stmt: Box<Statement> },
    Merge(Box<MergeDef>),
    Begin,
    Commit,
    Rollback,
    Savepoint(String),
    ReleaseSavepoint(String),
    RollbackTo(String),
}

/// `MERGE INTO target USING source ON cond WHEN ...`.
#[derive(Clone, Debug, PartialEq)]
pub struct MergeDef {
    pub target: String,
    pub target_alias: Option<String>,
    pub source: TableFactor,
    pub on: Expr,
    pub whens: Vec<MergeWhen>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MergeWhen {
    pub matched: bool,
    pub condition: Option<Expr>,
    pub action: MergeAction,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MergeAction {
    Update(Vec<(String, Expr)>),
    Delete,
    Insert { columns: Option<Vec<String>>, values: Vec<Expr> },
}
