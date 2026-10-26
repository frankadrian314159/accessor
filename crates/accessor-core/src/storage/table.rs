//! In-memory table: a slot vector of rows (stable row ids) plus ordered
//! secondary indexes backed by `BTreeSet`.

use std::collections::BTreeSet;
use std::ops::Bound;

use crate::error::{Error, Result};
use crate::schema::{Column, ForeignKey, IndexDef, TableSchema};
use crate::value::{DataType, Row, Value};

use super::mvcc::{self, pending, Stamp, View, INF, PENDING};
use super::stats::{ColumnStats, StatsCache};

pub type RowId = u32;

#[derive(Clone, Debug)]
struct Index {
    def: IndexDef,
    /// An entry for every key any version of a row has, until the version is cleaned away: a search
    /// finds candidates, and the version a reader can see is checked to have the key.
    tree: BTreeSet<(Vec<Cell>, RowId)>,
}

/// One value of an index key. In a descending column the order is reversed, so the tree runs in the
/// order the index was declared in, and walking it gives `ORDER BY a ASC, b DESC` without a sort.
#[derive(Clone, Debug)]
struct Cell {
    v: Value,
    desc: bool,
}

impl PartialEq for Cell {
    fn eq(&self, other: &Self) -> bool {
        self.v == other.v
    }
}

impl Eq for Cell {}

impl PartialOrd for Cell {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Cell {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let o = self.v.cmp(&other.v);
        if self.desc {
            o.reverse()
        } else {
            o
        }
    }
}

impl Index {
    fn key(&self, row: &[Value]) -> Vec<Cell> {
        self.def.columns.iter().enumerate().map(|(i, &c)| Cell { v: row[c].clone(), desc: self.def.is_desc(i) }).collect()
    }

    /// The leading part of a key, from values.
    fn cells(&self, values: &[Value]) -> Vec<Cell> {
        values.iter().enumerate().map(|(i, v)| Cell { v: v.clone(), desc: self.def.is_desc(i) }).collect()
    }
}

/// One state of a row. Rows are chains of these, newest first; see [`mvcc`].
#[derive(Clone, Debug)]
struct Version {
    row: Row,
    begin: Stamp,
    end: Stamp,
    prev: Option<Box<Version>>,
}

impl Version {
    /// One committed version, still alive: nothing to look up, nothing to clean.
    fn is_simple(&self) -> bool {
        self.prev.is_none() && self.begin < PENDING && self.end == INF
    }

    /// This version and the older ones, newest first.
    fn chain(&self) -> impl Iterator<Item = &Version> {
        std::iter::successors(Some(self), |v| v.prev.as_deref())
    }

    fn each_mut(&mut self, f: &mut impl FnMut(&mut Version)) {
        f(self);
        if let Some(p) = &mut self.prev {
            p.each_mut(f);
        }
    }
}

/// `Busy`, remembering which transaction is in the way.
fn busy(stamp: Stamp, what: String) -> Error {
    mvcc::blocked_by(stamp);
    Error::busy(what)
}

fn simple(slot: &Option<Version>) -> bool {
    slot.as_ref().is_none_or(Version::is_simple)
}

#[derive(Clone, Debug)]
pub struct Table {
    schema: TableSchema,
    rows: Vec<Option<Version>>,
    /// Slots whose newest version has not ended.
    live: usize,
    /// Slots that are not simple: more than one version, a change not yet committed, or a dead row
    /// waiting to be cleaned away. While there are none, `len` is just `live`.
    outstanding: usize,
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
        Table { schema, rows: Vec::new(), live: 0, outstanding: 0, next_auto: 1, indexes, generation: 0, stats: StatsCache::default() }
    }

    /// Rebuilds a table from persisted slots, recreating every index.
    pub(crate) fn from_parts(schema: TableSchema, rows: Vec<Option<Row>>, next_auto: i64) -> Result<Self> {
        let mut t = Table::new(schema);
        t.next_auto = next_auto;
        // Rows put back are simply there: committed, and changed in place.
        let saved = mvcc::view();
        mvcc::set_view(View::LATEST);
        let built = (|| {
            for (id, row) in rows.into_iter().enumerate() {
                if let Some(mut row) = row {
                    t.schema.conform_row(&mut row);
                    t.insert_at(id as RowId, row)?;
                } else {
                    t.rows.push(None);
                }
            }
            Ok(())
        })();
        mvcc::set_view(saved);
        built.map(|()| t)
    }

    /// Statistics of column `col` (its position): distinct values, common values and a histogram.
    /// They are worked out when first asked for and kept until the table changes.
    pub fn column_stats(&self, col: usize) -> std::sync::Arc<ColumnStats> {
        self.stats.get(self.generation, col, || {
            ColumnStats::compute(self.rows.iter().flatten().filter(|v| v.end == INF).map(|v| &v.row[col]), self.live)
        })
    }

    pub fn schema(&self) -> &TableSchema {
        &self.schema
    }
    pub fn name(&self) -> &str {
        &self.schema.name
    }
    /// Number of rows the caller can see.
    pub fn len(&self) -> usize {
        if self.outstanding == 0 {
            self.live
        } else {
            self.iter().count()
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// About how many rows there are, cheaply: what planning needs. Differs from [`len`](Self::len) only
    /// while changes are in flight or old versions are waiting to be cleaned away.
    pub fn estimated_len(&self) -> usize {
        self.live
    }
    /// Number of slots ever allocated (live rows plus tombstones).
    pub fn slot_count(&self) -> usize {
        self.rows.len()
    }
    /// The rows as the last commit left them, slot by slot: what a snapshot file holds. Changes not
    /// yet committed are left out.
    pub(crate) fn committed_slots(&self) -> Vec<Option<&Row>> {
        self.rows
            .iter()
            .map(|slot| {
                let v = slot.as_ref()?.chain().find(|v| v.begin < PENDING)?;
                (v.end == INF || v.end >= PENDING).then_some(&v.row)
            })
            .collect()
    }
    pub fn next_auto(&self) -> i64 {
        self.next_auto
    }
    pub fn get(&self, id: RowId) -> Option<&Row> {
        self.visible(id).map(|v| &v.row)
    }
    pub fn iter(&self) -> impl Iterator<Item = (RowId, &Row)> + '_ {
        let view = mvcc::view();
        self.rows.iter().enumerate().filter_map(move |(i, slot)| {
            let v = slot.as_ref()?.chain().find(|v| view.sees(v.begin, v.end))?;
            Some((i as RowId, &v.row))
        })
    }

    /// The version of row `id` the caller can see.
    fn visible(&self, id: RowId) -> Option<&Version> {
        let view = mvcc::view();
        self.rows.get(id as usize)?.as_ref()?.chain().find(|v| view.sees(v.begin, v.end))
    }

    /// Changes slot `id` with `f`, keeping the counts right.
    fn edit<R>(&mut self, id: RowId, f: impl FnOnce(&mut Option<Version>) -> R) -> R {
        let slot = &mut self.rows[id as usize];
        let (was_simple, was_live) = (simple(slot), slot.as_ref().is_some_and(|v| v.end == INF));
        let out = f(slot);
        let (is_simple, is_live) = (simple(slot), slot.as_ref().is_some_and(|v| v.end == INF));
        self.outstanding = self.outstanding + usize::from(!is_simple) - usize::from(!was_simple);
        self.live = self.live + usize::from(is_live) - usize::from(was_live);
        out
    }

    /// Removes from the indexes the keys that only the versions in `gone` had.
    fn forget_keys(&mut self, id: RowId, gone: &[Row]) {
        let mut doomed: Vec<(usize, Vec<Cell>)> = Vec::new();
        {
            let kept: Vec<&Row> = self.rows.get(id as usize).and_then(Option::as_ref).map_or_else(Vec::new, |h| h.chain().map(|v| &v.row).collect());
            for (pos, ix) in self.indexes.iter().enumerate() {
                for row in gone {
                    let key = ix.key(row);
                    if !kept.iter().any(|r| ix.key(r) == key) {
                        doomed.push((pos, key));
                    }
                }
            }
        }
        for (pos, key) in doomed {
            self.indexes[pos].tree.remove(&(key, id));
        }
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

    /// Fails if another row holds `key` in unique index `ix` (`except` is the row being changed). With
    /// versions the answer can be "it depends on how another transaction ends": that is `Busy`.
    fn check_unique(&self, ix: &Index, key: &[Cell], except: Option<RowId>) -> Result<()> {
        if !ix.def.unique || key.iter().any(|c| c.v.is_null()) {
            return Ok(());
        }
        let view = mvcc::view();
        let k = key.to_vec();
        for (_, id) in ix.tree.range((k.clone(), 0)..=(k.clone(), RowId::MAX)) {
            if Some(*id) == except {
                continue;
            }
            let Some(head) = self.rows.get(*id as usize).and_then(Option::as_ref) else { continue };
            let has = |v: &Version| ix.key(&v.row) == k;
            if view.txn == 0 {
                if has(head) {
                    return Err(self.dup_error(ix));
                }
                continue;
            }
            let me = pending(view.txn);
            if head.begin >= PENDING && head.begin != me {
                if has(head) || head.prev.as_deref().is_some_and(has) {
                    return Err(busy(if head.begin >= PENDING { head.begin } else { head.end }, format!("the value is being added or changed in unique index '{}' of '{}'", ix.def.name, self.schema.name)));
                }
                continue;
            }
            if !has(head) {
                continue;
            }
            if head.end >= PENDING && head.end != me {
                return Err(busy(head.end, format!("the value is being removed from unique index '{}' of '{}'", ix.def.name, self.schema.name)));
            }
            if head.end != INF {
                continue; // deleted, by this transaction or one that has committed
            }
            if head.begin == me || head.begin <= view.snapshot {
                return Err(self.dup_error(ix));
            }
            return Err(Error::conflict(format!("the value in unique index '{}' of '{}' was added after this transaction began", ix.def.name, self.schema.name)));
        }
        Ok(())
    }

    /// The newest version of row `id`, if the caller may change the row: it is the one they see, and no
    /// other transaction has an unfinished change to it.
    fn writable(&self, id: RowId, view: View) -> Result<&Version> {
        let missing = || Error::runtime(format!("row {id} does not exist in '{}'", self.schema.name));
        let head = self.rows.get(id as usize).and_then(Option::as_ref).ok_or_else(missing)?;
        let me = pending(view.txn);
        if (head.begin >= PENDING && head.begin != me) || (head.end >= PENDING && head.end != me) {
            return Err(busy(if head.begin >= PENDING && head.begin != me { head.begin } else { head.end }, format!("row {id} of '{}'", self.schema.name)));
        }
        // The row was deleted by a transaction that has committed: if that was after this one's
        // snapshot, this one still sees it, and must not delete it a second time.
        if !view.sees(head.begin, head.end) || (head.end != INF && head.end < PENDING) {
            return Err(if view.sees_death(head.end) {
                missing()
            } else {
                Error::conflict(format!("row {id} of '{}' was changed after this transaction began", self.schema.name))
            });
        }
        Ok(head)
    }

    /// Inserts `row` into slot `id`, which must be empty or past the end.
    pub fn insert_at(&mut self, id: RowId, row: Row) -> Result<()> {
        self.generation += 1;
        self.check_row(&row)?;
        let slot = id as usize;
        if self.rows.get(slot).is_some_and(Option::is_some) {
            return Err(Error::Corrupt(format!("row id {id} already occupied in '{}'", self.schema.name)));
        }
        let keys: Vec<Vec<Cell>> = self.indexes.iter().map(|ix| ix.key(&row)).collect();
        for (ix, key) in self.indexes.iter().zip(&keys) {
            self.check_unique(ix, key, None)?;
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
        let view = mvcc::view();
        let begin = if view.txn == 0 { 0 } else { pending(view.txn) };
        self.edit(id, |s| *s = Some(Version { row, begin, end: INF, prev: None }));
        Ok(())
    }

    /// Removes and returns the row in slot `id`. With versions the row stays, ended, for those who
    /// can still see it.
    pub fn remove(&mut self, id: RowId) -> Result<Row> {
        self.generation += 1;
        let view = mvcc::view();
        if view.txn != 0 {
            let row = self.writable(id, view)?.row.clone();
            let me = pending(view.txn);
            self.edit(id, |s| s.as_mut().expect("checked").end = me);
            return Ok(row);
        }
        let missing = || Error::runtime(format!("row {id} does not exist in '{}'", self.schema.name));
        let old = self.rows.get(id as usize).and_then(Option::as_ref).ok_or_else(missing)?;
        let row = old.row.clone();
        self.forget_all(id);
        self.edit(id, |s| *s = None);
        Ok(row)
    }

    /// Drops every index entry of slot `id`.
    fn forget_all(&mut self, id: RowId) {
        let rows: Vec<Row> = self.rows[id as usize].as_ref().map_or_else(Vec::new, |h| h.chain().map(|v| v.row.clone()).collect());
        for ix in &mut self.indexes {
            for r in &rows {
                let key = ix.key(r);
                ix.tree.remove(&(key, id));
            }
        }
    }

    /// Replaces the row in slot `id`, returning the previous contents. With versions the previous
    /// contents stay, ended, for those who can still see them.
    pub fn replace(&mut self, id: RowId, new: Row) -> Result<Row> {
        self.generation += 1;
        self.check_row(&new)?;
        let view = mvcc::view();
        let old = if view.txn != 0 {
            self.writable(id, view)?
        } else {
            self.rows
                .get(id as usize)
                .and_then(Option::as_ref)
                .ok_or_else(|| Error::runtime(format!("row {id} does not exist in '{}'", self.schema.name)))?
        };
        let old_row = old.row.clone();
        let mut changed = Vec::new();
        for (pos, ix) in self.indexes.iter().enumerate() {
            let (ok, nk) = (ix.key(&old_row), ix.key(&new));
            // Keys equal under the index ordering need no tree update, even
            // if their text differs only in case.
            if ok != nk {
                self.check_unique(ix, &nk, Some(id))?;
                changed.push((pos, ok, nk));
            }
        }
        for (pos, ok, nk) in changed {
            let tree = &mut self.indexes[pos].tree;
            if view.txn == 0 {
                tree.remove(&(ok, id));
            }
            tree.insert((nk, id));
        }
        if let Some(a) = self.schema.autonumber_column() {
            if let Value::Int(n) = new[a] {
                self.next_auto = self.next_auto.max(n.saturating_add(1));
            }
        }
        if view.txn == 0 {
            self.edit(id, |s| s.as_mut().expect("checked").row = new);
        } else {
            let me = pending(view.txn);
            self.edit(id, |s| {
                let mut old = s.take().expect("checked");
                old.end = me;
                *s = Some(Version { row: new, begin: me, end: INF, prev: Some(Box::new(old)) });
            });
        }
        Ok(old_row)
    }

    // ----- transactions: taking changes back, making them final, cleaning up --------------------

    /// Takes back the newest version of row `id`, an insert by the transaction that is being undone.
    pub(crate) fn unwind_insert(&mut self, id: RowId) {
        self.generation += 1;
        let gone: Vec<Row> = self.edit(id, |s| {
            let mut head = s.take().expect("an insert to take back");
            *s = head.prev.take().map(|b| *b);
            vec![head.row]
        });
        self.forget_keys(id, &gone);
    }

    /// Takes back the newest version of row `id`, made by an update that is being undone.
    pub(crate) fn unwind_update(&mut self, id: RowId) {
        self.generation += 1;
        let gone: Vec<Row> = self.edit(id, |s| {
            let mut head = s.take().expect("an update to take back");
            let mut older = head.prev.take().expect("an update has an older version");
            older.end = INF;
            *s = Some(*older);
            vec![head.row]
        });
        self.forget_keys(id, &gone);
    }

    /// Takes back a delete: the row lives again.
    pub(crate) fn unwind_delete(&mut self, id: RowId) {
        self.generation += 1;
        self.edit(id, |s| s.as_mut().expect("a delete to take back").end = INF);
    }

    /// Makes the changes transaction `txn` made to row `id` final, at commit timestamp `ts`.
    pub(crate) fn commit_slot(&mut self, id: RowId, txn: u64, ts: Stamp) {
        let me = pending(txn);
        let mut gone = Vec::new();
        self.edit(id, |s| {
            let Some(head) = s.as_mut() else { return };
            drop_mine(head, me, &mut gone);
            let mut stamp = |v: &mut Version| {
                if v.begin == me {
                    v.begin = ts;
                }
                if v.end == me {
                    v.end = ts;
                }
            };
            head.each_mut(&mut stamp);
        });
        self.forget_keys(id, &gone);
    }

    /// Makes every change of transaction `txn` final, wherever it is.
    pub(crate) fn commit_all(&mut self, txn: u64, ts: Stamp) {
        for id in 0..self.rows.len() as RowId {
            if self.rows[id as usize].as_ref().is_some_and(|h| h.chain().any(|v| v.begin == pending(txn) || v.end == pending(txn))) {
                self.commit_slot(id, txn, ts);
            }
        }
    }

    /// Throws away the versions of row `id` that none of `snapshots` can see (nor any later reader, whose
    /// snapshot is the last one listed or newer): ones that had ended before every snapshot, and ones
    /// born after one and replaced before the next. Returns whether the row is now simple.
    pub(crate) fn clean_slot(&mut self, id: RowId, snapshots: &[u64]) -> bool {
        let mut gone = Vec::new();
        self.edit(id, |slot| {
            let Some(head) = slot.take() else { return };
            let mut chain = Vec::new();
            let mut cur = Some(Box::new(head));
            while let Some(mut v) = cur {
                cur = v.prev.take();
                chain.push(v);
            }
            let needed = |v: &Version| {
                v.begin >= PENDING || v.end >= PENDING || v.end == INF || snapshots.iter().any(|&s| v.begin <= s && s < v.end)
            };
            let mut kept = Vec::new();
            for v in chain {
                if needed(&v) {
                    kept.push(v);
                } else {
                    gone.push(v.row);
                }
            }
            let mut head = None;
            for mut v in kept.into_iter().rev() {
                v.prev = head;
                head = Some(v);
            }
            *slot = head.map(|b| *b);
        });
        self.forget_keys(id, &gone);
        simple(&self.rows[id as usize])
    }

    pub fn tombstones(&self) -> usize {
        self.rows.len() - self.live
    }

    /// Drops tombstones, renumbering rows and rebuilding indexes. Only a table with no versions
    /// outstanding can be compacted.
    pub(crate) fn compact(&mut self) -> Result<()> {
        if self.tombstones() == 0 || self.outstanding > 0 {
            return Ok(());
        }
        let rows = std::mem::take(&mut self.rows).into_iter().flatten().map(|v| Some(v.row)).collect();
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
        let mut seen: BTreeSet<Vec<Cell>> = BTreeSet::new();
        for (id, slot) in self.rows.iter().enumerate() {
            let Some(head) = slot else { continue };
            for (age, v) in head.chain().enumerate() {
                let key = ix.key(&v.row);
                let counts = age == 0 && v.end == INF && ix.def.unique && !key.iter().any(|c| c.v.is_null());
                if counts && !seen.insert(key.clone()) {
                    return Err(Error::constraint(format!(
                        "cannot create unique index '{}': duplicate values exist",
                        def.name
                    )));
                }
                ix.tree.insert((key, id as RowId));
            }
        }
        self.indexes.push(ix);
        self.schema.indexes.push(def);
        Ok(())
    }

    /// Builds every index again from the rows (all versions of them). Fails, leaving the old index, if a unique
    /// index finds a duplicate.
    pub(crate) fn rebuild_indexes(&mut self) -> Result<()> {
        self.generation += 1;
        for pos in 0..self.indexes.len() {
            let mut tree = BTreeSet::new();
            let mut seen: BTreeSet<Vec<Cell>> = BTreeSet::new();
            for (id, slot) in self.rows.iter().enumerate() {
                let Some(head) = slot else { continue };
                for (age, v) in head.chain().enumerate() {
                    let ix = &self.indexes[pos];
                    let key = ix.key(&v.row);
                    if age == 0 && v.end == INF && ix.def.unique && !key.iter().any(|c| c.v.is_null()) && !seen.insert(key.clone()) {
                        return Err(self.dup_error(ix));
                    }
                    tree.insert((key, id as RowId));
                }
            }
            self.indexes[pos].tree = tree;
        }
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
        for head in self.rows.iter_mut().flatten() {
            head.each_mut(&mut |v| v.row.push(fill.clone()));
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
        for head in self.rows.iter_mut().flatten() {
            head.each_mut(&mut |v| {
                v.row.remove(pos);
            });
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
        let mut converted = self.clone();
        let mut failure = None;
        for head in converted.rows.iter_mut().flatten() {
            head.each_mut(&mut |v| {
                if failure.is_none() && !v.row[pos].is_null() {
                    match v.row[pos].coerce_to(data_type) {
                        Ok(x) => v.row[pos] = x,
                        Err(e) => failure = Some(Error::type_err(format!("field '{name}': {e}; no data was changed"))),
                    }
                }
            });
        }
        if let Some(e) = failure {
            return Err(e);
        }
        converted.schema = schema;
        // The keys changed type with their values: build the indexes again from every version.
        for pos in 0..converted.indexes.len() {
            let mut tree = BTreeSet::new();
            let mut seen: BTreeSet<Vec<Cell>> = BTreeSet::new();
            for (id, slot) in converted.rows.iter().enumerate() {
                let Some(head) = slot else { continue };
                for (age, v) in head.chain().enumerate() {
                    let ix = &converted.indexes[pos];
                    let key = ix.key(&v.row);
                    if age == 0 && v.end == INF && ix.def.unique && !key.iter().any(|c| c.v.is_null()) && !seen.insert(key.clone()) {
                        return Err(converted.dup_error(ix));
                    }
                    tree.insert((key, id as RowId));
                }
            }
            converted.indexes[pos].tree = tree;
        }
        converted.generation = self.generation + 1;
        *self = converted;
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

    /// How many versions of rows the table holds in all, dead ones not yet cleaned away included.
    #[doc(hidden)]
    pub fn version_count(&self) -> usize {
        self.rows.iter().flatten().map(|h| h.chain().count()).sum()
    }

    /// The version chains, for finding out what went wrong.
    #[doc(hidden)]
    pub fn dump_chains(&self) -> String {
        let mut out = String::new();
        for (i, slot) in self.rows.iter().enumerate() {
            if let Some(h) = slot {
                let fmt = |s: Stamp| if s == INF { "inf".to_string() } else if s >= PENDING { format!("p{}", s & !PENDING) } else { s.to_string() };
                let chain: Vec<String> = h.chain().map(|v| format!("[{}..{} {:?}]", fmt(v.begin), fmt(v.end), v.row)).collect();
                out.push_str(&format!("row {i}: {}
", chain.join(" <- ")));
            }
        }
        out
    }

    /// Slots that still hold more than one version, or an uncommitted or ended one: work for the cleaner.
    pub(crate) fn unsettled(&self) -> Vec<RowId> {
        self.rows.iter().enumerate().filter(|(_, s)| !simple(s)).map(|(i, _)| i as RowId).collect()
    }

    /// The newest state of row `id` whoever made it, committed or not.
    pub fn get_latest(&self, id: RowId) -> Option<&Row> {
        self.rows.get(id as usize)?.as_ref().map(|v| &v.row)
    }

    /// Is row `id`, as the caller sees it, one that has `key` in index `idx`? (The index also holds the
    /// keys of versions the caller cannot see.)
    fn has_key(&self, idx: usize, key: &[Cell], id: RowId) -> bool {
        let cols = &self.indexes[idx].def.columns;
        self.visible(id).is_some_and(|v| cols.iter().zip(key).all(|(&c, k)| v.row[c] == k.v))
    }

    /// Row ids whose index key starts with `prefix`.
    pub fn index_prefix(&self, idx: usize, prefix: &[Value]) -> impl Iterator<Item = RowId> + '_ {
        let p = self.indexes[idx].cells(prefix);
        let start = (p.clone(), 0);
        self.indexes[idx]
            .tree
            .range(start..)
            .take_while(move |(k, _)| k.len() >= p.len() && k[..p.len()] == p[..])
            .filter(move |(k, id)| self.has_key(idx, k, *id))
            .map(|(_, id)| *id)
    }

    /// Row ids in index order, for keys starting with `prefix` (all keys when it is
    /// empty). Records whose first `tie_len` key values are equal keep ascending row-id
    /// order even when `desc` reverses the rest, so the result matches a stable sort.
    pub fn index_ordered(&self, idx: usize, prefix: &[Value], tie_len: usize, desc: bool) -> Box<dyn Iterator<Item = RowId> + '_> {
        let p = self.indexes[idx].cells(prefix);
        let tree = &self.indexes[idx].tree;
        let range = tree
            .range((p.clone(), 0)..)
            .take_while(move |(k, _)| k.len() >= p.len() && k[..p.len()] == p[..])
            .filter(move |(k, id)| self.has_key(idx, k, *id));
        if !desc {
            return Box::new(range.map(|(_, id)| *id));
        }
        let entries: Vec<(&[Cell], RowId)> = range.map(|(k, id)| (&k[..], *id)).collect();
        let mut ids = Vec::with_capacity(entries.len());
        let mut end = entries.len();
        while end > 0 {
            let tie = |e: &(&[Cell], RowId)| e.0[..tie_len.min(e.0.len())].to_vec();
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

    /// Row ids of the rows whose first index column is within bounds, in the order of the index (low to
    /// high, or high to low if that column is descending). `Null` keys never qualify.
    pub fn index_range(&self, idx: usize, lo: Bound<Value>, hi: Bound<Value>) -> impl Iterator<Item = RowId> + '_ {
        let desc = self.indexes[idx].def.is_desc(0);
        // The tree starts at the end of the range it reaches first.
        let (first, last) = if desc { (hi, lo) } else { (lo, hi) };
        let start = match &first {
            Bound::Included(v) | Bound::Excluded(v) => (vec![Cell { v: v.clone(), desc }], 0),
            Bound::Unbounded => (Vec::new(), 0),
        };
        self.indexes[idx]
            .tree
            .range(start..)
            .skip_while(move |(k, _)| match &first {
                // NULLs sort lowest: first in an ascending column, last in a descending one.
                Bound::Unbounded => !desc && k[0].v.is_null(),
                Bound::Included(_) => false,
                Bound::Excluded(v) => k[0].v == *v,
            })
            .take_while(move |(k, _)| match &last {
                Bound::Unbounded => !(desc && k[0].v.is_null()),
                Bound::Included(v) => if desc { k[0].v >= *v } else { k[0].v <= *v },
                Bound::Excluded(v) => if desc { k[0].v > *v } else { k[0].v < *v },
            })
            .filter(move |(k, id)| self.has_key(idx, k, *id))
            .map(|(_, id)| *id)
    }

    /// Row ids whose `cols` equal `key` in the *newest* state of the table, which is what a foreign key
    /// has to respect whatever the caller's snapshot: a row another transaction has committed counts,
    /// and so does one it has not yet committed, which cannot be counted on and so is `Busy`.
    pub fn lookup_latest(&self, cols: &[usize], key: &[Value]) -> Result<Vec<RowId>> {
        if key.iter().any(Value::is_null) {
            return Ok(Vec::new());
        }
        let view = mvcc::view();
        let me = pending(view.txn);
        let matches = |v: &Version| cols.iter().zip(key).all(|(&c, k)| v.row[c] == *k);
        let candidates: Vec<RowId> = {
            let same_set = |d: &IndexDef| d.columns.len() == cols.len() && cols.iter().all(|c| d.columns.contains(c));
            match self.indexes.iter().position(|ix| same_set(&ix.def)) {
                Some(pos) => {
                    let probe: Vec<Value> = self.indexes[pos]
                        .def
                        .columns
                        .iter()
                        .map(|c| key[cols.iter().position(|x| x == c).expect("same column set")].clone())
                        .collect();
                    let p = self.indexes[pos].cells(&probe);
                    self.indexes[pos]
                        .tree
                        .range((p.clone(), 0)..)
                        .take_while(move |(k, _)| k.len() >= p.len() && k[..p.len()] == p[..])
                        .map(|(_, id)| *id)
                        .collect()
                }
                None => (0..self.rows.len() as RowId).collect(),
            }
        };
        let mut out = Vec::new();
        for id in candidates {
            let Some(head) = self.rows.get(id as usize).and_then(Option::as_ref) else { continue };
            let other_begin = head.begin >= PENDING && head.begin != me;
            let other_end = head.end >= PENDING && head.end != me;
            // A change not yet committed by another transaction: whether the row is there depends on it.
            if (other_begin && (matches(head) || head.prev.as_deref().is_some_and(matches))) || (other_end && matches(head)) {
                return Err(busy(if other_begin { head.begin } else { head.end }, format!("a row of '{}' is being changed", self.schema.name)));
            }
            if head.end == INF && matches(head) {
                out.push(id);
            }
        }
        out.dedup();
        Ok(out)
    }
}

/// Drops the versions of this row made by `me` that a later change of `me` has replaced: they were
/// never seen by anyone else.
fn drop_mine(v: &mut Version, me: Stamp, gone: &mut Vec<Row>) {
    while v.prev.as_ref().is_some_and(|p| p.begin == me) {
        let mut p = v.prev.take().expect("checked");
        v.prev = p.prev.take();
        gone.push(p.row);
    }
    if let Some(p) = v.prev.as_deref_mut() {
        drop_mine(p, me, gone);
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
            indexes: vec![IndexDef::new("PrimaryKey", vec![0], true)],
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
        t.remove(4).unwrap();
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
