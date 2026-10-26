//! Syntax tree for the supported SQL dialect.

use std::sync::Arc;

use crate::schema::RefAction;
use crate::value::{DataType, Row, Value};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    /// `/`: floating-point division, so `1 / 3` is `0.3333333333333333`.
    Div,
    /// `DIV`: exact division: `1 DIV 3` is the decimal `0.3333333333333333333333333333` (or a fraction, for
    /// rationals), with no rounding to a binary number.
    ExactDiv,
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
    /// A named window this one starts from (`OVER (w ORDER BY x)` or `OVER w`).
    pub base: Option<String>,
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<OrderItem>,
    pub frame: Option<Frame>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FrameUnits {
    Rows,
    Range,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FrameBound {
    UnboundedPreceding,
    /// `n PRECEDING` in a ROWS frame: n rows.
    Preceding(usize),
    CurrentRow,
    Following(usize),
    UnboundedFollowing,
    /// `x PRECEDING` in a RANGE frame: rows whose sort value is within `x` before the current one.
    RangePreceding(Value),
    RangeFollowing(Value),
}

#[derive(Clone, Debug, PartialEq)]
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
    Named { name: String, alias: Option<String>, only: bool },
    /// A parenthesised query. With `lateral`, it may read columns of the tables before it
    /// in the FROM clause and is evaluated once for each of their rows.
    Derived { query: Box<Select>, alias: String, columns: Vec<String>, lateral: bool },
    /// `source PIVOT (agg FOR column IN (v1, v2, ...))`: the values of `column` become columns.
    Pivot { source: Box<TableFactor>, agg: Expr, pivot: Expr, values: Vec<(Expr, Option<String>)>, alias: String },
    /// `source UNPIVOT (value FOR name IN (c1, c2, ...))`: the listed columns become rows.
    Unpivot {
        source: Box<TableFactor>,
        value_col: String,
        name_col: String,
        columns: Vec<(String, Option<String>)>,
        include_nulls: bool,
        alias: String,
    },
    /// A table function such as `GENERATE_SERIES(1, 10)`; its arguments may read earlier tables.
    Function { name: String, args: Vec<Expr>, alias: String, columns: Vec<String> },
    /// Rows already computed (the result of a common table expression).
    Materialized { rows: Arc<Vec<Row>>, columns: Vec<String>, alias: String },
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
    /// `WINDOW w AS (...)` definitions that window functions can name.
    pub windows: Vec<(String, WindowSpec)>,
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
    pub deferrable: bool,
    pub initially_deferred: bool,
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
    RenameTable(String),
    /// `ALTER COLUMN c SET DEFAULT v` (`Some`) or `DROP DEFAULT` (`None`).
    SetDefault { name: String, value: Option<Value> },
    /// `ALTER COLUMN c SET NOT NULL` / `DROP NOT NULL`.
    SetNotNull { name: String, not_null: bool },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReportTotal {
    Sum,
    Avg,
    Count,
    Min,
    Max,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReportAlign {
    Left,
    Right,
    Center,
}

/// `BREAK BY column`: a new group whenever the column's value changes.
#[derive(Clone, Debug, PartialEq)]
pub struct ReportBreak {
    pub column: String,
    /// Text for the group's heading; `{column}` is replaced by that column's value.
    pub header: Option<String>,
    /// Text for the line of totals closing the group; `{count}` is the number of rows.
    pub footer: Option<String>,
    pub new_page: bool,
}

/// `COLUMN name ...`: how one column of the query is shown.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ReportColumn {
    pub name: String,
    pub heading: Option<String>,
    pub format: Option<String>,
    pub align: Option<ReportAlign>,
    pub total: Option<ReportTotal>,
    pub width: Option<usize>,
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ReportLayout {
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub breaks: Vec<ReportBreak>,
    pub columns: Vec<ReportColumn>,
    pub grand_total: Option<String>,
    pub page_lines: Option<usize>,
}

/// A stored report. `sql` is the original `CREATE REPORT` text, which is what gets stored.
#[derive(Clone, Debug, PartialEq)]
pub struct ReportDef {
    pub name: String,
    pub layout: ReportLayout,
    pub query: Select,
    pub sql: String,
}

/// What `INSERT` does with a row that conflicts with an existing one (on a unique key).
#[derive(Clone, Debug, PartialEq)]
pub enum ConflictAction {
    /// `DO NOTHING` / `INSERT OR IGNORE`: the new row is dropped.
    Nothing,
    /// `REPLACE INTO` / `INSERT OR REPLACE`: the existing row is deleted, the new one inserted.
    Replace,
    /// `DO UPDATE SET ... [WHERE ...]`: the existing row is updated; `EXCLUDED.col` is the new row's value.
    Update { sets: Vec<(String, Expr)>, filter: Option<Expr> },
}

/// The `ON CONFLICT` part of an insert; `target` is the columns of the unique key (any, if empty).
#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    pub target: Vec<String>,
    pub action: ConflictAction,
}

/// A parameter of a stored function or procedure.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutineParam {
    pub name: String,
    pub data_type: Option<DataType>,
    pub default: Option<Expr>,
}

/// One statement of a stored procedure.
#[derive(Clone, Debug, PartialEq)]
pub enum ProcStmt {
    Declare { name: String, data_type: Option<DataType>, init: Option<Expr> },
    Set { name: String, value: Expr },
    If { branches: Vec<(Expr, Vec<ProcStmt>)>, otherwise: Vec<ProcStmt> },
    While { cond: Expr, body: Vec<ProcStmt> },
    /// Leaves the innermost `WHILE`.
    Leave,
    Return(Option<Expr>),
    Sql(Statement),
}

#[derive(Clone, Debug, PartialEq)]
pub enum RoutineBody {
    /// A function: one expression, in which the parameters are `@name`.
    Expr(Expr),
    /// A procedure: statements with variables and control flow.
    Block(Vec<ProcStmt>),
}

/// A stored function or procedure. `sql` is the original `CREATE` text, which is what gets stored.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutineDef {
    pub name: String,
    pub function: bool,
    pub params: Vec<RoutineParam>,
    pub returns: Option<DataType>,
    pub body: RoutineBody,
    /// `RETURNS trigger`: for `CREATE TRIGGER ... EXECUTE FUNCTION`.
    pub trigger: bool,
    pub sql: String,
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
    /// Replaces the change to a view.
    InsteadOf,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TriggerEvent {
    Insert,
    Update,
    Delete,
}

/// A trigger, for each row or each statement. `sql` is the original `CREATE TRIGGER` text,
/// which is what gets stored.
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerDef {
    pub name: String,
    pub timing: TriggerTiming,
    pub events: Vec<TriggerEvent>,
    /// `UPDATE OF a, b`: fires only when one of these columns changes.
    pub update_of: Vec<String>,
    /// `FOR EACH STATEMENT`: fires once per statement, with no `OLD` or `NEW`.
    pub each_statement: bool,
    pub table: String,
    pub when: Option<Expr>,
    pub body: Vec<Statement>,
    /// `EXECUTE FUNCTION f()`: the body is that PL/pgSQL trigger function, and `body` is empty.
    pub function: Option<String>,
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
    /// `descending[i]` is true for a column written `DESC`.
    CreateIndex { name: String, table: String, columns: Vec<String>, descending: Vec<bool>, unique: bool },
    /// `DROP INDEX name [ON table]`; without the table, the index of that name wherever it is.
    DropIndex { name: String, table: Option<String> },
    /// `REINDEX [table | index]`: builds the indexes again from the rows.
    Reindex(Option<String>),
    CreateView { name: String, sql: String },
    DropView(String),
    /// Header of a parameterised query; a no-op when executed on its own.
    Parameters(Vec<ParamDecl>),
    Exec { name: String, args: Vec<(Option<String>, Expr)> },
    /// Aborts the statement with a message (trigger bodies).
    Raise(Expr),
    CreateTrigger(Box<TriggerDef>),
    DropTrigger(String),
    /// `INSERT | UPDATE | DELETE | MERGE ... RETURNING items`.
    Returning { stmt: Box<Statement>, items: Vec<SelectItem> },
    /// An `INSERT` that deals with conflicts: `ON CONFLICT`, `REPLACE INTO`, `INSERT OR IGNORE`.
    Upsert { insert: Box<Statement>, conflict: Conflict },
    CreateReport(Box<ReportDef>),
    DropReport(String),
    /// `RUN REPORT name [AS format]`.
    RunReport { name: String, format: String },
    CreateRoutine(Box<RoutineDef>),
    DropRoutine { name: String, function: bool },
    CreateSequence { name: String, options: crate::sequence::SequenceOptions },
    AlterSequence { name: String, options: crate::sequence::SequenceOptions },
    DropSequence(String),
    /// `CREATE OR REPLACE VIEW|QUERY|TRIGGER`: drop an existing object of that name, then create.
    OrReplace(Box<Statement>),
    /// `TRUNCATE [TABLE] t`: delete every record.
    Truncate(String),
    /// `SET TIME ZONE 'Europe/Paris'`: the zone of the session.
    SetTimeZone(String),
    /// `PREPARE name AS statement`: remember a statement to run later with `EXEC name (args)`.
    Prepare { name: String, statement: Box<Statement> },
    /// `DEALLOCATE name` (or `ALL`).
    Deallocate(String),
    /// `SET CONSTRAINTS ALL | names DEFERRED | IMMEDIATE`.
    SetConstraints { names: Option<Vec<String>>, deferred: bool },
    /// `EXPLAIN query`: run the query and report how it was planned.
    Explain(Box<Select>),
    /// `EXPLAIN` of a statement that is not a query: described, not run.
    ExplainOther(Box<Statement>),
    /// Creating or dropping an object kept in the catalog without a table of its own (see `exec/catalog.rs`).
    Catalog(Box<CatalogChange>),
    /// `ALTER ROLE name [WITH] options` / `ALTER ROLE name RENAME TO new`.
    AlterRole { name: String, rename: Option<String>, options: Vec<(String, String)> },
    /// `GRANT privileges ON object TO roles` / `REVOKE ...`; `GRANT role TO role` has no `object`.
    Grant(Box<GrantDef>),
    /// `SET ROLE name` / `SET SESSION AUTHORIZATION name`; `None` is `NONE` / `DEFAULT`.
    SetRole(Option<String>),
    /// `CREATE TABLE name AS query [WITH [NO] DATA]`.
    CreateTableAs { name: String, query: Box<Select>, with_data: bool },
    /// A command that is read and does nothing (`VACUUM`, `ANALYZE`, `COMMENT ON`, `LOCK`, ...).
    Ignored(String),
    /// `CREATE TABLE` with `INHERITS`, `PARTITION BY` or `PARTITION OF`.
    CreateTableExt(Box<TableExt>),
    /// `DROP TABLE a, b [CASCADE]`.
    DropTables { names: Vec<String>, cascade: bool, if_exists: bool },
    /// `UPDATE ONLY` / `DELETE FROM ONLY`: the table without the tables that inherit from it.
    Only(Box<Statement>),
    /// `SET [SESSION | LOCAL] name { TO | = } value [, ...]`; `None` is `DEFAULT`.
    SetParameter { name: String, value: Option<String>, local: bool },
    /// `RESET name` / `RESET ALL` (`None`).
    ResetParameter(Option<String>),
    /// `SHOW name` / `SHOW ALL` (`None`).
    ShowParameter(Option<String>),
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

/// A schema, domain, type, extension, role or rule made or dropped.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogChange {
    /// `schema`, `type` (a domain or an enum), `extension`, `role`, `rule`.
    pub kind: &'static str,
    pub names: Vec<String>,
    /// The description to store; `None` drops.
    pub value: Option<String>,
    pub replace: bool,
    pub cascade: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrantDef {
    pub revoke: bool,
    /// Lower-case privilege words; empty for a role membership.
    pub privileges: Vec<String>,
    /// `table`, `schema`, `sequence`, `function`, `database`, ...; for `ALL TABLES IN SCHEMA s` it is `all_tables` and the
    /// object is the schema.
    pub object_kind: String,
    pub objects: Vec<String>,
    pub grantees: Vec<String>,
    pub cascade: bool,
}

/// What `INHERITS`, `PARTITION BY` and `PARTITION OF` add to a `CREATE TABLE` (which is `create`).
#[derive(Clone, Debug, PartialEq)]
pub struct TableExt {
    pub create: Statement,
    pub parents: Vec<String>,
    /// `(method, columns)` of `PARTITION BY`.
    pub partition_by: Option<(String, Vec<String>)>,
    /// `(parent, bound)` of `PARTITION OF`; the bound is saved as JSON text.
    pub partition_of: Option<(String, String)>,
}
