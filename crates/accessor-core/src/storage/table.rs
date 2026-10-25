//! In-memory table: a slot vector of rows (stable row ids) plus ordered
//! secondary indexes backed by `BTreeSet`.

use std::collections::BTreeSet;
use std::ops::Bound;

use crate::error::{Error, Result};
use crate::schema::{Column, ForeignKey, IndexDef, TableSchema};
use crate::value::{DataType, Row, Value};

use super::stats::{ColumnStats, StatsCache};

pub type RowId = u32;

#[derive(Clone, Debug)]
struct Index {
    def: IndexDef,
    tree: BTreeSet<(Vec<Value>, RowId)>,
}

impl Index {
    fn key(&self, row: &[Value]) -> Vec<Value> {
        self.def.columns.iter().map(|&c| row[c].clone()).collect()
    }

    /// True if another row (not `except`) already holds `key`. Keys that
    /// contain `Null` never conflict.
    fn conflicts(&self, key: &[Value], except: Option<RowId>) -> bool {
        if !self.def.unique || key.iter().any(Value::is_null) {
            return false;
        }
        let k = key.to_vec();
        self.tree
            .range((k.clone(), 0)..=(k, RowId::MAX))
            .any(|(_, id)| Some(*id) != except)
    }
}

#[derive(Clone, Debug)]
pub struct Table {
    schema: TableSchema,
    rows: Vec<Option<Row>>,
    live: usize,
    next_auto: i64,
    indexes: Vec<Index>,
    /// Counts the changes made to the rows or columns, so that statistics worked out earlier can be told from current ones.
    generation: u64,
    stats: StatsCache,
}

impl Table {
    pub fn new(schema: TableSchema) -> Self {
        let indexes = schema
            .indexes
            .iter()
            .map(|def| Index { def: def.clone(), tree: BTreeSet::new() })
            .collect();
        Table { schema, rows: Vec::new(), live: 0, next_auto: 1, indexes, generation: 0, stats: StatsCache::default() }
    }

    /// Rebuilds a table from persisted slots, recreating every index.
    pub(crate) fn from_parts(schema: TableSchema, rows: Vec<Option<Row>>, next_auto: i64) -> Result<Self> {
        let mut t = Table::new(schema);
        t.next_auto = next_auto;
        for (id, row) in rows.into_iter().enumerate() {
            if let Some(mut row) = row {
                t.schema.conform_row(&mut row);
                t.insert_at(id as RowId, row)?;
            } else {
                t.rows.push(None);
            }
        }
        Ok(t)
    }

    /// Statistics of column `col` (its position): distinct values, common values and a histogram.
    /// They are worked out when first asked for and kept until the table changes.
    pub fn column_stats(&self, col: usize) -> std::sync::Arc<ColumnStats> {
        self.stats.get(self.generation, col, || {
            ColumnStats::compute(self.rows.iter().flatten().map(|r| &r[col]), self.live)
        })
    }

    pub fn schema(&self) -> &TableSchema {
        &self.schema
    }
    pub fn name(&self) -> &str {
        &self.schema.name
    }
    /// Number of live rows.
    pub fn len(&self) -> usize {
        self.live
    }
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }
    /// Number of slots ever allocated (live rows plus tombstones).
    pub fn slot_count(&self) -> usize {
        self.rows.len()
    }
    pub(crate) fn raw_slots(&self) -> &[Option<Row>] {
        &self.rows
    }
    pub fn next_auto(&self) -> i64 {
        self.next_auto
    }
    pub fn get(&self, id: RowId) -> Option<&Row> {
        self.rows.get(id as usize).and_then(Option::as_ref)
    }
    pub fn iter(&self) -> impl Iterator<Item = (RowId, &Row)> + '_ {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.as_ref().map(|r| (i as RowId, r)))
    }

    fn check_row(&self, row: &[Value]) -> Result<()> {
        if row.len() != self.schema.columns.len() {
            return Err(Error::runtime("row width does not match table schema"));
        }
        for (v, c) in row.iter().zip(&self.schema.columns) {
            if v.is_null() && (c.not_null || c.primary_key) {
                return Err(Error::constraint(format!(
                    "field '{}.{}' cannot contain a Null value",
                    self.schema.name, c.name
                )));
            }
            if c.autonumber && !matches!(v, Value::Null | Value::Int(_)) {
                return Err(Error::constraint(format!(
                    "field '{}.{}' is an AutoNumber and holds whole numbers up to 9223372036854775807",
                    self.schema.name, c.name
                )));
            }
        }
        Ok(())
    }

    fn dup_error(&self, ix: &Index) -> Error {
        Error::constraint(format!(
            "duplicate value in unique index '{}' on table '{}'",
            ix.def.name, self.schema.name
        ))
    }

    /// Inserts `row` into slot `id`, which must be empty or past the end.
    pub fn insert_at(&mut self, id: RowId, row: Row) -> Result<()> {
        self.generation += 1;
        self.check_row(&row)?;
        let slot = id as usize;
        if self.rows.get(slot).is_some_and(Option::is_some) {
            return Err(Error::Corrupt(format!("row id {id} already occupied in '{}'", self.schema.name)));
        }
        let keys: Vec<Vec<Value>> = self.indexes.iter().map(|ix| ix.key(&row)).collect();
        for (ix, key) in self.indexes.iter().zip(&keys) {
            if ix.conflicts(key, None) {
                return Err(self.dup_error(ix));
            }
        }
        for (ix, key) in self.indexes.iter_mut().zip(keys) {
            ix.tree.insert((key, id));
        }
        if let Some(a) = self.schema.autonumber_column() {
            if let Value::Int(n) = row[a] {
                self.next_auto = self.next_auto.max(n.saturating_add(1));
            }
        }
        if slot >= self.rows.len() {
            self.rows.resize(slot + 1, None);
        }
        self.rows[slot] = Some(row);
        self.live += 1;
        Ok(())
    }

    /// Removes and returns the row in slot `id`.
    pub fn remove(&mut self, id: RowId) -> Option<Row> {
        self.generation += 1;
        let row = self.rows.get_mut(id as usize)?.take()?;
        for ix in &mut self.indexes {
            let key = ix.key(&row);
            ix.tree.remove(&(key, id));
        }
        self.live -= 1;
        Some(row)
    }

    /// Replaces the row in slot `id`, returning the previous contents.
    pub fn replace(&mut self, id: RowId, new: Row) -> Result<Row> {
        self.generation += 1;
        self.check_row(&new)?;
        let old = self
            .get(id)
            .ok_or_else(|| Error::runtime(format!("row {id} does not exist in '{}'", self.schema.name)))?;
        let mut changed = Vec::new();
        for (pos, ix) in self.indexes.iter().enumerate() {
            let (ok, nk) = (ix.key(old), ix.key(&new));
            // Keys equal under the index ordering need no tree update, even
            // if their text differs only in case.
            if ok != nk {
                if ix.conflicts(&nk, Some(id)) {
                    return Err(self.dup_error(ix));
                }
                changed.push((pos, ok, nk));
            }
        }
        for (pos, ok, nk) in changed {
            let tree = &mut self.indexes[pos].tree;
            tree.remove(&(ok, id));
            tree.insert((nk, id));
        }
        if let Some(a) = self.schema.autonumber_column() {
            if let Value::Int(n) = new[a] {
                self.next_auto = self.next_auto.max(n.saturating_add(1));
            }
        }
        Ok(self.rows[id as usize].replace(new).expect("checked above"))
    }

    pub fn tombstones(&self) -> usize {
        self.rows.len() - self.live
    }

    /// Drops tombstones, renumbering rows and rebuilding indexes.
    pub(crate) fn compact(&mut self) -> Result<()> {
        if self.tombstones() == 0 {
            return Ok(());
        }
        let rows = std::mem::take(&mut self.rows).into_iter().flatten().map(Some).collect();
        let mut fresh = Table::from_parts(self.schema.clone(), rows, self.next_auto)?;
        fresh.generation = self.generation + 1;
        std::mem::swap(self, &mut fresh);
        Ok(())
    }

    // ----- index management ------------------------------------------------

    pub fn index_position(&self, name: &str) -> Option<usize> {
        self.indexes.iter().position(|i| i.def.name.eq_ignore_ascii_case(name))
    }

    pub fn index_defs(&self) -> impl Iterator<Item = &IndexDef> {
        self.indexes.iter().map(|i| &i.def)
    }

    pub(crate) fn add_index(&mut self, def: IndexDef) -> Result<()> {
        if self.index_position(&def.name).is_some() {
            return Err(Error::catalog(format!("index '{}' already exists on '{}'", def.name, self.schema.name)));
        }
        if def.columns.is_empty() || def.columns.iter().any(|&c| c >= self.schema.columns.len()) {
            return Err(Error::catalog("index references an unknown column"));
        }
        let mut ix = Index { def: def.clone(), tree: BTreeSet::new() };
        for (id, row) in self.iter() {
            let key = ix.key(row);
            if ix.conflicts(&key, None) {
                return Err(Error::constraint(format!(
                    "cannot create unique index '{}': duplicate values exist",
                    def.name
                )));
            }
            ix.tree.insert((key, id));
        }
        self.indexes.push(ix);
        self.schema.indexes.push(def);
        Ok(())
    }

    pub(crate) fn drop_index(&mut self, name: &str) -> Result<()> {
        let pos = self
            .index_position(name)
            .ok_or_else(|| Error::catalog(format!("no index named '{name}' on '{}'", self.schema.name)))?;
        self.indexes.remove(pos);
        self.schema.indexes.remove(pos);
        Ok(())
    }

    // ----- column management -------------------------------------------------

    fn column_checked(&self, name: &str) -> Result<usize> {
        self.schema.column_index(name).ok_or_else(|| {
            Error::catalog(format!("the field '{name}' does not exist in table '{}'", self.schema.name))
        })
    }

    pub(crate) fn set_name(&mut self, name: &str) {
        self.schema.name = name.to_string();
    }

    /// Points relationships that name table `old` at `new`.
    pub(crate) fn rename_references(&mut self, old: &str, new: &str) {
        for fk in &mut self.schema.foreign_keys {
            if fk.ref_table.eq_ignore_ascii_case(old) {
                fk.ref_table = new.to_string();
            }
        }
    }

    /// Sets (or with `None` removes) the default of a field.
    pub(crate) fn set_default(&mut self, name: &str, value: Option<Value>) -> Result<()> {
        let pos = self.column_checked(name)?;
        let col = &mut self.schema.columns[pos];
        if col.is_calculated() {
            return Err(Error::catalog(format!("the field '{name}' is calculated and has no default")));
        }
        if col.autonumber {
            return Err(Error::catalog(format!("the AutoNumber field '{name}' has no default")));
        }
        col.default = match value {
            Some(v) => Some(v.coerce_to(col.data_type).map_err(|e| Error::type_err(format!("default for '{name}': {e}")))?),
            None => None,
        };
        Ok(())
    }

    /// Makes a field required (refused while any record has no value for it) or optional.
    pub(crate) fn set_not_null(&mut self, name: &str, not_null: bool) -> Result<()> {
        let pos = self.column_checked(name)?;
        if self.schema.columns[pos].is_calculated() {
            return Err(Error::catalog(format!("the calculated field '{name}' cannot be required")));
        }
        if not_null {
            let missing = self.iter().filter(|(_, r)| r[pos].is_null()).count();
            if missing > 0 {
                return Err(Error::constraint(format!(
                    "cannot make '{name}' required: {missing} record(s) in '{}' have no value for it",
                    self.schema.name
                )));
            }
        } else if self.schema.columns[pos].primary_key {
            return Err(Error::catalog(format!("'{name}' is part of the primary key and must have a value")));
        }
        self.schema.columns[pos].not_null = not_null;
        Ok(())
    }

    /// Appends a column; existing rows receive its default (or Null).
    pub(crate) fn add_column(&mut self, mut column: Column) -> Result<()> {
        self.generation += 1;
        if self.schema.column_index(&column.name).is_some() {
            return Err(Error::catalog(format!("duplicate column '{}' in table '{}'", column.name, self.schema.name)));
        }
        if column.autonumber || column.primary_key {
            return Err(Error::catalog("AutoNumber and primary key columns cannot be added to an existing table"));
        }
        if let Some(d) = column.default.take() {
            column.default = Some(if d.is_null() { d } else { d.coerce_to(column.data_type)? });
        }
        if column.sequence_default().is_some() && self.live > 0 {
            return Err(Error::catalog("a column filled from a sequence cannot be added to a table that has records"));
        }
        // A calculated field's "default" is its expression; its values are computed afterwards.
        let fill = if column.is_calculated() { Value::Null } else { column.default.clone().unwrap_or(Value::Null) };
        if fill.is_null() && column.not_null && self.live > 0 {
            return Err(Error::constraint(format!(
                "cannot add required field '{}' without a default to a table that has records",
                column.name
            )));
        }
        for row in self.rows.iter_mut().flatten() {
            row.push(fill.clone());
        }
        self.schema.columns.push(column);
        Ok(())
    }

    /// Removes a column. Columns used by an index must be freed first.
    pub(crate) fn drop_column(&mut self, name: &str) -> Result<()> {
        self.generation += 1;
        let pos = self.column_checked(name)?;
        if self.schema.columns.len() == 1 {
            return Err(Error::catalog("a table must keep at least one field"));
        }
        if let Some(ix) = self.indexes.iter().find(|ix| ix.def.columns.contains(&pos)) {
            return Err(Error::catalog(format!(
                "the field '{name}' is part of index '{}'; drop the index first",
                ix.def.name
            )));
        }
        for row in self.rows.iter_mut().flatten() {
            row.remove(pos);
        }
        self.schema.columns.remove(pos);
        // Index keys hold values, not positions, so only the definitions shift.
        for (ix, def) in self.indexes.iter_mut().zip(&mut self.schema.indexes) {
            for c in ix.def.columns.iter_mut().chain(def.columns.iter_mut()) {
                if *c > pos {
                    *c -= 1;
                }
            }
        }
        for fk in &mut self.schema.foreign_keys {
            for c in &mut fk.columns {
                if *c > pos {
                    *c -= 1;
                }
            }
        }
        Ok(())
    }

    /// Makes `name` a calculated field computing `expr`, converting an ordinary
    /// field if need be. Returns the previous definition. The stored values are
    /// not touched here; the caller recomputes them.
    pub(crate) fn set_expression(&mut self, name: &str, expr: &str) -> Result<Column> {
        self.generation += 1;
        let pos = self.column_checked(name)?;
        let old = self.schema.columns[pos].clone();
        if !old.is_calculated() && (old.primary_key || old.autonumber) {
            return Err(Error::catalog(format!(
                "the field '{name}' is a key or an AutoNumber and cannot become a calculated field"
            )));
        }
        let col = &mut self.schema.columns[pos];
        col.data_type = DataType::Calculated;
        col.default = Some(Value::text(expr));
        col.not_null = false;
        Ok(old)
    }

    /// Undoes [`set_expression`](Self::set_expression): puts back the column
    /// definition and the stored values it had.
    pub(crate) fn restore_column(&mut self, pos: usize, column: Column, values: Vec<(RowId, Value)>) -> Result<()> {
        self.generation += 1;
        self.schema.columns[pos] = column;
        for (id, v) in values {
            if let Some(row) = self.get(id) {
                let mut row = row.clone();
                row[pos] = v;
                self.replace(id, row)?;
            }
        }
        Ok(())
    }

    pub(crate) fn rename_column(&mut self, from: &str, to: &str) -> Result<()> {
        let pos = self.column_checked(from)?;
        if self.schema.columns.iter().enumerate().any(|(i, c)| i != pos && c.name.eq_ignore_ascii_case(to)) {
            return Err(Error::catalog(format!("duplicate column '{to}' in table '{}'", self.schema.name)));
        }
        self.schema.columns[pos].name = to.to_string();
        Ok(())
    }

    /// Converts a column's type. Nothing changes if any value fails to
    /// convert or the converted values violate a unique index.
    pub(crate) fn alter_column_type(&mut self, name: &str, data_type: DataType) -> Result<()> {
        self.generation += 1;
        let pos = self.column_checked(name)?;
        let col = &self.schema.columns[pos];
        if col.autonumber {
            return Err(Error::catalog("the type of an AutoNumber field cannot be changed"));
        }
        if col.data_type == data_type {
            return Ok(());
        }
        if data_type == DataType::Calculated {
            return Err(Error::catalog("use ALTER COLUMN ... CALCULATED AS (expression) to make a field calculated"));
        }
        let was_calculated = col.is_calculated();
        let mut schema = self.schema.clone();
        schema.columns[pos].data_type = data_type;
        if was_calculated {
            // The field keeps its current values as ordinary data; the expression goes.
            schema.columns[pos].default = None;
        } else if let Some(d) = &schema.columns[pos].default {
            schema.columns[pos].default = Some(if d.is_null() { Value::Null } else { d.coerce_to(data_type)? });
        }
        let mut rows = Vec::with_capacity(self.rows.len());
        for slot in &self.rows {
            rows.push(match slot {
                Some(r) => {
                    let mut r = r.clone();
                    if !r[pos].is_null() {
                        r[pos] = r[pos]
                            .coerce_to(data_type)
                            .map_err(|e| Error::type_err(format!("field '{name}': {e}; no data was changed")))?;
                    }
                    Some(r)
                }
                None => None,
            });
        }
        *self = Table::from_parts(schema, rows, self.next_auto)?;
        Ok(())
    }

    // ----- foreign keys / lookups ----------------------------------------------

    pub(crate) fn add_foreign_key(&mut self, fk: ForeignKey) -> Result<()> {
        if self.schema.foreign_keys.iter().any(|f| f.name.eq_ignore_ascii_case(&fk.name)) {
            return Err(Error::catalog(format!("constraint '{}' already exists on '{}'", fk.name, self.schema.name)));
        }
        if fk.columns.is_empty() || fk.columns.iter().any(|&c| c >= self.schema.columns.len()) {
            return Err(Error::catalog("foreign key references an unknown column"));
        }
        self.schema.foreign_keys.push(fk);
        Ok(())
    }

    pub(crate) fn shift_references(&mut self, parent: &str, pos: usize) {
        for fk in self.schema.foreign_keys.iter_mut().filter(|f| f.ref_table.eq_ignore_ascii_case(parent)) {
            for c in &mut fk.ref_columns {
                if *c > pos {
                    *c -= 1;
                }
            }
        }
    }

    pub(crate) fn set_foreign_key_timing(&mut self, name: &str, deferrable: bool, initially_deferred: bool) -> Result<()> {
        let fk = self
            .schema
            .foreign_keys
            .iter_mut()
            .find(|f| f.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::catalog(format!("no constraint named '{name}' on '{}'", self.schema.name)))?;
        fk.deferrable = deferrable;
        fk.initially_deferred = initially_deferred;
        Ok(())
    }

    pub(crate) fn drop_foreign_key(&mut self, name: &str) -> Result<()> {
        let pos = self
            .schema
            .foreign_keys
            .iter()
            .position(|f| f.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::catalog(format!("no constraint named '{name}' on '{}'", self.schema.name)))?;
        self.schema.foreign_keys.remove(pos);
        Ok(())
    }

    /// Row ids whose `cols` equal `key` (Null never matches). Uses an index
    /// over exactly that column set when there is one, else scans.
    pub fn lookup(&self, cols: &[usize], key: &[Value]) -> Vec<RowId> {
        if key.iter().any(Value::is_null) {
            return Vec::new();
        }
        let same_set = |d: &IndexDef| d.columns.len() == cols.len() && cols.iter().all(|c| d.columns.contains(c));
        if let Some(pos) = self.indexes.iter().position(|ix| same_set(&ix.def)) {
            let probe: Vec<Value> = self.indexes[pos]
                .def
                .columns
                .iter()
                .map(|c| key[cols.iter().position(|x| x == c).expect("same column set")].clone())
                .collect();
            return self.index_prefix(pos, &probe).collect();
        }
        self.iter()
            .filter(|(_, r)| cols.iter().zip(key).all(|(&c, k)| r[c] == *k))
            .map(|(id, _)| id)
            .collect()
    }

    /// Row ids whose index key starts with `prefix`.
    pub fn index_prefix(&self, idx: usize, prefix: &[Value]) -> impl Iterator<Item = RowId> + '_ {
        let p = prefix.to_vec();
        let start = (p.clone(), 0);
        self.indexes[idx]
            .tree
            .range(start..)
            .take_while(move |(k, _)| k.len() >= p.len() && k[..p.len()] == p[..])
            .map(|(_, id)| *id)
    }

    /// Row ids in index order, for keys starting with `prefix` (all keys when it is
    /// empty). Records whose first `tie_len` key values are equal keep ascending row-id
    /// order even when `desc` reverses the rest, so the result matches a stable sort.
    pub fn index_ordered(&self, idx: usize, prefix: &[Value], tie_len: usize, desc: bool) -> Box<dyn Iterator<Item = RowId> + '_> {
        let p = prefix.to_vec();
        let tree = &self.indexes[idx].tree;
        let range = tree
            .range((p.clone(), 0)..)
            .take_while(move |(k, _)| k.len() >= p.len() && k[..p.len()] == p[..]);
        if !desc {
            return Box::new(range.map(|(_, id)| *id));
        }
        let entries: Vec<(&[Value], RowId)> = range.map(|(k, id)| (&k[..], *id)).collect();
        let mut ids = Vec::with_capacity(entries.len());
        let mut end = entries.len();
        while end > 0 {
            let tie = |e: &(&[Value], RowId)| e.0[..tie_len.min(e.0.len())].to_vec();
            let key = tie(&entries[end - 1]);
            let mut start = end - 1;
            while start > 0 && tie(&entries[start - 1]) == key {
                start -= 1;
            }
            let from = ids.len();
            ids.extend(entries[start..end].iter().map(|e| e.1));
            ids[from..].sort_unstable();
            end = start;
        }
        Box::new(ids.into_iter())
    }

    /// Row ids ordered by the first column of index `idx`, within bounds.
    /// `Null` keys never qualify.
    pub fn index_range(&self, idx: usize, lo: Bound<Value>, hi: Bound<Value>) -> impl Iterator<Item = RowId> + '_ {
        let start = match &lo {
            Bound::Included(v) | Bound::Excluded(v) => (vec![v.clone()], 0),
            Bound::Unbounded => (Vec::new(), 0),
        };
        self.indexes[idx]
            .tree
            .range(start..)
            .skip_while(move |(k, _)| match &lo {
                Bound::Unbounded => k[0].is_null(),
                Bound::Included(_) => false,
                Bound::Excluded(v) => k[0] == *v,
            })
            .take_while(move |(k, _)| match &hi {
                Bound::Unbounded => true,
                Bound::Included(v) => k[0] <= *v,
                Bound::Excluded(v) => k[0] < *v,
            })
            .map(|(_, id)| *id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Column;
    use crate::value::DataType;

    fn table() -> Table {
        let mut id = Column::new("id", DataType::Integer);
        id.primary_key = true;
        Table::new(TableSchema {
            name: "t".into(),
            columns: vec![id, Column::new("name", DataType::Text)],
            indexes: vec![IndexDef { name: "PrimaryKey".into(), columns: vec![0], unique: true }],
            foreign_keys: Vec::new(),
        })
    }

    #[test]
    fn unique_and_replace() {
        let mut t = table();
        t.insert_at(0, vec![Value::Int(1), "a".into()]).unwrap();
        t.insert_at(1, vec![Value::Int(2), "b".into()]).unwrap();
        assert!(t.insert_at(2, vec![Value::Int(1), "c".into()]).is_err());
        assert!(t.replace(1, vec![Value::Int(1), "b".into()]).is_err());
        t.replace(1, vec![Value::Int(5), "b".into()]).unwrap();
        assert_eq!(t.index_prefix(0, &[Value::Int(5)]).collect::<Vec<_>>(), vec![1]);
        assert_eq!(t.index_prefix(0, &[Value::Int(2)]).count(), 0);
    }

    #[test]
    fn range_scan() {
        let mut t = table();
        for i in 0..10 {
            t.insert_at(i, vec![Value::Int(i as i64 * 10), Value::Null]).unwrap();
        }
        let got: Vec<_> = t
            .index_range(0, Bound::Excluded(Value::Int(20)), Bound::Included(Value::Int(50)))
            .collect();
        assert_eq!(got, vec![3, 4, 5]);
        t.remove(4);
        t.compact().unwrap();
        assert_eq!(t.slot_count(), 9);
        assert_eq!(t.len(), 9);
    }

    #[test]
    fn not_null_enforced() {
        let mut t = table();
        assert!(t.insert_at(0, vec![Value::Null, "x".into()]).is_err());
    }
}
