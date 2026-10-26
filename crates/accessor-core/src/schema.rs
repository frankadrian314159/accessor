//! Table definitions.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::value::{DataType, Row, Value};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub data_type: DataType,
    pub not_null: bool,
    pub primary_key: bool,
    pub autonumber: bool,
    pub default: Option<Value>,
}

/// Text that stands in for the default of a column filled from a sequence
/// (`DEFAULT NEXT VALUE FOR s`): this prefix, then the sequence name.
pub const SEQUENCE_DEFAULT: &str = "\u{1}nextval:";

impl Column {
    /// The sequence that fills this column when no value is given.
    pub fn sequence_default(&self) -> Option<&str> {
        match &self.default {
            Some(Value::Text(s)) if !self.is_calculated() => (**s).strip_prefix(SEQUENCE_DEFAULT),
            _ => None,
        }
    }

    pub fn is_calculated(&self) -> bool {
        self.data_type == DataType::Calculated
    }

    /// The expression of a calculated field (kept in `default`, which a
    /// calculated field has no other use for).
    pub fn expression(&self) -> Option<&str> {
        match (&self.default, self.is_calculated()) {
            (Some(Value::Text(s)), true) => Some(&**s),
            _ => None,
        }
    }

    pub fn new(name: impl Into<String>, data_type: DataType) -> Self {
        Column {
            name: name.into(),
            data_type,
            not_null: false,
            primary_key: false,
            autonumber: false,
            default: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndexDef {
    pub name: String,
    /// Column positions, in key order.
    pub columns: Vec<usize>,
    pub unique: bool,
    /// Which key columns run from high to low (`DESC`). Empty, or shorter than `columns`, means ascending.
    pub descending: Vec<bool>,
}

impl IndexDef {
    /// An index whose columns are all ascending.
    pub fn new(name: impl Into<String>, columns: Vec<usize>, unique: bool) -> Self {
        IndexDef { name: name.into(), columns, unique, descending: Vec::new() }
    }

    /// The same index with the given directions (`true`: descending), one per column.
    pub fn with_order(mut self, descending: Vec<bool>) -> Self {
        self.descending = if descending.iter().any(|d| *d) { descending } else { Vec::new() };
        self
    }

    /// Does key column `i` run from high to low?
    pub fn is_desc(&self, i: usize) -> bool {
        self.descending.get(i).copied().unwrap_or(false)
    }

    /// Is any key column descending?
    pub fn has_desc(&self) -> bool {
        self.descending.iter().any(|d| *d)
    }
}

/// How an index is stored in files and logs: as it always was, with the top bit of a column number
/// marking a descending column. (An index that is all ascending is written exactly as before.)
const DESC_FLAG: usize = 1 << 31;

#[derive(Serialize, Deserialize)]
struct IndexWire {
    name: String,
    columns: Vec<usize>,
    unique: bool,
}

impl Serialize for IndexDef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        IndexWire {
            name: self.name.clone(),
            columns: self.columns.iter().enumerate().map(|(i, &c)| if self.is_desc(i) { c | DESC_FLAG } else { c }).collect(),
            unique: self.unique,
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for IndexDef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let w = IndexWire::deserialize(d)?;
        let descending: Vec<bool> = w.columns.iter().map(|c| c & DESC_FLAG != 0).collect();
        let columns = w.columns.iter().map(|c| c & !DESC_FLAG).collect();
        Ok(IndexDef { name: w.name, columns, unique: w.unique, descending: Vec::new() }.with_order(descending))
    }
}

/// What happens to referencing rows when the referenced row is deleted or
/// its key changes.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefAction {
    /// Refuse the change (default).
    #[default]
    Restrict,
    /// Delete / update the referencing rows too.
    Cascade,
    /// Set the referencing fields to Null.
    SetNull,
    /// Set the referencing fields to their defaults.
    SetDefault,
    /// Like `Restrict`, but a deferrable relationship may wait until the transaction ends.
    NoAction,
}

impl RefAction {
    pub const ALL: [RefAction; 5] =
        [RefAction::Restrict, RefAction::NoAction, RefAction::Cascade, RefAction::SetNull, RefAction::SetDefault];

    /// The SQL keyword(s) for this action.
    pub fn sql(self) -> &'static str {
        match self {
            RefAction::Restrict => "RESTRICT",
            RefAction::Cascade => "CASCADE",
            RefAction::SetNull => "SET NULL",
            RefAction::SetDefault => "SET DEFAULT",
            RefAction::NoAction => "NO ACTION",
        }
    }
}

/// A relationship: `columns` of the owning table must match the key
/// `ref_columns` of `ref_table` (or be Null).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<usize>,
    pub ref_table: String,
    pub ref_columns: Vec<usize>,
    pub on_delete: RefAction,
    pub on_update: RefAction,
    /// May be checked at the end of the transaction instead of the end of each statement.
    /// (Not part of the table's stored form; the file keeps it separately.)
    #[serde(skip)]
    pub deferrable: bool,
    #[serde(skip)]
    pub initially_deferred: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<Column>,
    pub indexes: Vec<IndexDef>,
    pub foreign_keys: Vec<ForeignKey>,
}

impl TableSchema {
    pub fn has_calculated(&self) -> bool {
        self.columns.iter().any(Column::is_calculated)
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn autonumber_column(&self) -> Option<usize> {
        self.columns.iter().position(|c| c.autonumber)
    }

    /// Checks structural rules and fills in implied indexes' invariants.
    pub fn validate(&self) -> Result<()> {
        if self.columns.is_empty() {
            return Err(Error::catalog(format!("table '{}' must have at least one column", self.name)));
        }
        for (i, c) in self.columns.iter().enumerate() {
            if self.columns[..i].iter().any(|o| o.name.eq_ignore_ascii_case(&c.name)) {
                return Err(Error::catalog(format!("duplicate column '{}' in table '{}'", c.name, self.name)));
            }
            if c.autonumber && c.data_type != DataType::Integer {
                return Err(Error::catalog("AutoNumber columns must be of type INTEGER"));
            }
            if c.is_calculated() {
                if c.expression().is_none() {
                    return Err(Error::catalog(format!("the calculated field '{}' has no expression", c.name)));
                }
                if c.not_null || c.primary_key || c.autonumber {
                    return Err(Error::catalog(format!(
                        "the calculated field '{}' cannot be required, a primary key, or an AutoNumber",
                        c.name
                    )));
                }
            }
        }
        if self.columns.iter().filter(|c| c.autonumber).count() > 1 {
            return Err(Error::catalog("a table can have only one AutoNumber column"));
        }
        for ix in &self.indexes {
            if ix.columns.is_empty() || ix.columns.iter().any(|&c| c >= self.columns.len()) {
                return Err(Error::catalog(format!("index '{}' references an unknown column", ix.name)));
            }
        }
        Ok(())
    }

    /// Converts numeric values to the numeric type of their column where the two
    /// differ. Only data written by earlier versions can differ (a stored integer
    /// in a column that is now a DECIMAL, say), so this runs when loading, not on writes.
    pub(crate) fn conform_row(&self, row: &mut Row) {
        for (v, c) in row.iter_mut().zip(&self.columns) {
            if c.data_type.is_numeric() && v.data_type().is_some_and(|t| t != c.data_type) {
                if let Ok(converted) = v.coerce_to(c.data_type) {
                    *v = converted;
                }
            }
        }
    }

    /// Coerces every value to its column type. Does not apply defaults,
    /// autonumbering, or NOT NULL checks.
    pub fn coerce_row(&self, mut row: Row) -> Result<Row> {
        if row.len() != self.columns.len() {
            return Err(Error::runtime(format!(
                "table '{}' has {} columns but {} values were supplied",
                self.name,
                self.columns.len(),
                row.len()
            )));
        }
        for (v, c) in row.iter_mut().zip(&self.columns) {
            if v.data_type() != Some(c.data_type) && !v.is_null() && !c.is_calculated() {
                *v = v
                    .coerce_to(c.data_type)
                    .map_err(|e| Error::type_err(format!("column '{}': {e}", c.name)))?;
            }
        }
        Ok(row)
    }
}
