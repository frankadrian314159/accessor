//! The database handle: catalog, transactions, and durability.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::sql::ast::{Statement, TriggerDef};
use crate::sql::parse_statement;
use crate::storage::persist::{Durability, Persistence};
use crate::storage::{Op, Table};

/// Log size that triggers an automatic checkpoint.
const CHECKPOINT_BYTES: u64 = 64 << 20;

#[derive(Default)]
struct Tx {
    /// Committed to the log on COMMIT (only retained for persistent databases).
    ops: Vec<Op>,
    undo: Vec<Op>,
}

pub struct Database {
    pub(crate) tables: BTreeMap<String, Table>,
    /// Saved queries: lowercase name → (display name, SQL text).
    pub(crate) views: BTreeMap<String, (String, String)>,
    /// Triggers by lowercase name; the definition keeps its own source text.
    pub(crate) triggers: BTreeMap<String, Arc<TriggerDef>>,
    /// Compiled calculated-field expressions per table; see `calc.rs`.
    pub(crate) calc_cache: crate::calc::CalcCache,
    /// Whether the batch being applied touches a table with calculated fields.
    pub(crate) calc_active: bool,
    persist: Option<Persistence>,
    tx: Option<Tx>,
}

pub(crate) fn key(name: &str) -> String {
    name.to_ascii_lowercase()
}

impl Database {
    /// A database that lives only in memory.
    pub fn in_memory() -> Self {
        Database {
            tables: BTreeMap::new(),
            views: BTreeMap::new(),
            triggers: BTreeMap::new(),
            calc_cache: Default::default(),
            calc_active: false,
            persist: None,
            tx: None,
        }
    }

    /// Opens (creating if necessary) a database file. Committed changes made
    /// since the last checkpoint are recovered from the write-ahead log.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let (persist, loaded, batches) = Persistence::open(path.as_ref())?;
        let mut db = Database::in_memory();
        let is_new = loaded.is_none();
        if let Some(l) = loaded {
            for t in l.tables {
                db.tables.insert(key(t.name()), t);
            }
            for (n, s) in l.views {
                db.views.insert(key(&n), (n, s));
            }
            for (_, sql) in l.triggers {
                let def = parse_trigger(&sql)?;
                db.triggers.insert(key(&def.name), Arc::new(def));
            }
        }
        let replayed = !batches.is_empty();
        for batch in &batches {
            for op in batch {
                // A log from an earlier version may hold numbers of another numeric type than their column's.
                let mut op = op.clone();
                if let Op::Insert { table, row, .. } | Op::Update { table, row, .. } = &mut op {
                    if let Some(t) = db.table(table) {
                        t.schema().clone().conform_row(row);
                    }
                }
                db.apply(&op)
                    .map_err(|e| Error::Corrupt(format!("replaying the write-ahead log failed: {e}")))?;
            }
        }
        let mut compacted = persist.needs_upgrade;
        for t in db.tables.values_mut() {
            if t.tombstones() > 0 {
                t.compact()?;
                compacted = true;
            }
        }
        db.persist = Some(persist);
        if is_new || replayed || compacted {
            db.checkpoint()?;
        }
        Ok(db)
    }

    pub fn is_persistent(&self) -> bool {
        self.persist.is_some()
    }

    pub fn set_durability(&mut self, d: Durability) {
        if let Some(p) = &mut self.persist {
            p.durability = d;
        }
    }

    pub fn in_transaction(&self) -> bool {
        self.tx.is_some()
    }

    // ----- catalog -----------------------------------------------------------

    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.get(&key(name))
    }

    pub(crate) fn table_checked(&self, name: &str) -> Result<&Table> {
        self.table(name)
            .ok_or_else(|| Error::catalog(format!("the table '{name}' does not exist")))
    }

    pub fn table_names(&self) -> Vec<&str> {
        self.tables.values().map(Table::name).collect()
    }

    pub fn view_names(&self) -> Vec<&str> {
        self.views.values().map(|(n, _)| n.as_str()).collect()
    }

    pub fn view_sql(&self, name: &str) -> Option<&str> {
        self.views.get(&key(name)).map(|(_, s)| s.as_str())
    }

    pub fn trigger_names(&self) -> Vec<&str> {
        self.triggers.values().map(|t| t.name.as_str()).collect()
    }

    /// The `CREATE TRIGGER` text of a trigger.
    pub fn trigger_sql(&self, name: &str) -> Option<&str> {
        self.triggers.get(&key(name)).map(|t| t.sql.as_str())
    }

    /// Names of the parameters a saved query declares, in order.
    pub fn query_parameters(&self, name: &str) -> Option<Vec<String>> {
        let (decls, _) = self.resolve_query(name, 0).ok()?;
        Some(decls.into_iter().map(|d| d.name).collect())
    }

    // ----- applying operations -------------------------------------------------

    /// Applies one operation, returning its inverse for DML.
    pub(crate) fn apply(&mut self, op: &Op) -> Result<Option<Op>> {
        if op.is_ddl() {
            self.calc_cache.clear();
        }
        match op {
            Op::CreateTable(schema) => {
                schema.validate()?;
                let k = key(&schema.name);
                if self.tables.contains_key(&k) || self.views.contains_key(&k) {
                    return Err(Error::catalog(format!("'{}' already exists", schema.name)));
                }
                self.tables.insert(k, Table::new(schema.clone()));
                Ok(None)
            }
            Op::DropTable(name) => {
                self.guard_drop_table(name)?;
                self.triggers.retain(|_, t| !t.table.eq_ignore_ascii_case(name));
                self.tables
                    .remove(&key(name))
                    .ok_or_else(|| Error::catalog(format!("the table '{name}' does not exist")))?;
                Ok(None)
            }
            Op::CreateIndex { table, def } => {
                self.table_mut(table)?.add_index(def.clone())?;
                Ok(None)
            }
            Op::DropIndex { table, name } => {
                self.guard_drop_index(table, name)?;
                self.table_mut(table)?.drop_index(name)?;
                Ok(None)
            }
            Op::CreateView { name, sql } => {
                let k = key(name);
                if self.tables.contains_key(&k) || self.views.contains_key(&k) {
                    return Err(Error::catalog(format!("'{name}' already exists")));
                }
                self.views.insert(k, (name.clone(), sql.clone()));
                Ok(None)
            }
            Op::DropView(name) => {
                self.views
                    .remove(&key(name))
                    .ok_or_else(|| Error::catalog(format!("the query '{name}' does not exist")))?;
                Ok(None)
            }
            Op::AddColumn { table, column } => {
                self.table_mut(table)?.add_column(column.clone())?;
                if column.is_calculated() {
                    self.calc_cache.clear();
                    if let Err(e) = self.recalculate_table(table) {
                        // Leave the table as it was.
                        self.table_mut(table)?.drop_column(&column.name)?;
                        self.calc_cache.clear();
                        return Err(e);
                    }
                }
                Ok(None)
            }
            Op::SetExpression { table, name, expr } => {
                let (pos, was_calculated) = {
                    let t = self.table_checked(table)?;
                    let pos = t.schema().column_index(name).ok_or_else(|| {
                        Error::catalog(format!("the field \u{27}{name}\u{27} does not exist in table \u{27}{table}\u{27}"))
                    })?;
                    (pos, t.schema().columns[pos].is_calculated())
                };
                if !was_calculated {
                    // An ordinary field becomes calculated: its values will be replaced.
                    self.guard_column_change(table, name, "made calculated")?;
                }
                let saved: Vec<_> = self.table_checked(table)?.iter().map(|(id, r)| (id, r[pos].clone())).collect();
                let old = self.table_mut(table)?.set_expression(name, expr)?;
                self.calc_cache.clear();
                if let Err(e) = self.recalculate_table(table) {
                    // Put the field back as it was, values included.
                    self.table_mut(table)?.restore_column(pos, old, saved)?;
                    self.calc_cache.clear();
                    let _ = self.recalculate_table(table);
                    return Err(e);
                }
                Ok(None)
            }
            Op::DropColumn { table, name } => {
                let pos = self.guard_column_change(table, name, "deleted")?;
                self.guard_calculated_dependency(table, name, "deleted")?;
                self.table_mut(table)?.drop_column(name)?;
                self.shift_references(table, pos);
                Ok(None)
            }
            Op::RenameColumn { table, from, to } => {
                self.guard_calculated_dependency(table, from, "renamed")?;
                self.table_mut(table)?.rename_column(from, to)?;
                Ok(None)
            }
            Op::AlterColumnType { table, name, data_type } => {
                self.guard_column_change(table, name, "converted")?;
                self.guard_calculated_dependency(table, name, "converted")?;
                self.table_mut(table)?.alter_column_type(name, *data_type)?;
                Ok(None)
            }
            Op::AddForeignKey { table, fk } => {
                self.table_mut(table)?.add_foreign_key(fk.clone())?;
                Ok(None)
            }
            Op::DropForeignKey { table, name } => {
                self.table_mut(table)?.drop_foreign_key(name)?;
                Ok(None)
            }
            Op::CreateTrigger { name, sql } => {
                let def = parse_trigger(sql)?;
                self.table_checked(&def.table)?;
                let k = key(name);
                if self.triggers.contains_key(&k) {
                    return Err(Error::catalog(format!("the trigger '{name}' already exists")));
                }
                self.triggers.insert(k, Arc::new(def));
                Ok(None)
            }
            Op::DropTrigger(name) => {
                self.triggers
                    .remove(&key(name))
                    .ok_or_else(|| Error::catalog(format!("the trigger '{name}' does not exist")))?;
                Ok(None)
            }
            Op::Insert { table, id, row } => {
                self.table_mut(table)?.insert_at(*id, row.clone())?;
                Ok(Some(Op::Delete { table: table.clone(), id: *id }))
            }
            Op::Update { table, id, row } => {
                let old = self.table_mut(table)?.replace(*id, row.clone())?;
                Ok(Some(Op::Update { table: table.clone(), id: *id, row: old }))
            }
            Op::Delete { table, id } => {
                let old = self
                    .table_mut(table)?
                    .remove(*id)
                    .ok_or_else(|| Error::runtime(format!("row {id} does not exist in '{table}'")))?;
                Ok(Some(Op::Insert { table: table.clone(), id: *id, row: old }))
            }
        }
    }

    pub(crate) fn table_mut(&mut self, name: &str) -> Result<&mut Table> {
        self.tables
            .get_mut(&key(name))
            .ok_or_else(|| Error::catalog(format!("the table '{name}' does not exist")))
    }

    pub(crate) fn revert(&mut self, undo: Vec<Op>) {
        for op in undo.into_iter().rev() {
            // Inverses of operations that just succeeded cannot fail.
            let _ = self.apply(&op);
        }
    }

    /// Atomically applies a batch: either every op takes effect (and is made
    /// durable, or queued in the open transaction) or none does.
    pub(crate) fn apply_all(&mut self, ops: Vec<Op>) -> Result<()> {
        if self.tx.is_some() && ops.iter().any(Op::is_ddl) {
            return Err(Error::runtime("schema changes are not allowed inside a transaction"));
        }
        let mut ops = ops;
        let mut undo = Vec::with_capacity(ops.len());
        self.calc_active = ops.iter().any(Op::is_dml) && self.any_calculated();
        let applied = if ops.iter().any(Op::is_dml) && (self.has_foreign_keys() || !self.triggers.is_empty()) {
            // May append cascaded operations to `ops`.
            self.apply_enforced(&mut ops, &mut undo)
        } else {
            ops.iter_mut().try_for_each(|op| {
                if self.calc_active {
                    self.finalize_op(op)?;
                }
                self.apply(op).map(|inv| undo.extend(inv))
            })
        };
        if let Err(e) = applied {
            self.revert(undo);
            return Err(e);
        }
        if let Some(tx) = &mut self.tx {
            if self.persist.is_some() {
                tx.ops.extend(ops);
            }
            tx.undo.extend(undo);
            return Ok(());
        }
        self.log(&ops, undo)
    }

    fn log(&mut self, ops: &[Op], undo: Vec<Op>) -> Result<()> {
        let Some(p) = &mut self.persist else { return Ok(()) };
        if ops.is_empty() {
            return Ok(());
        }
        if let Err(e) = p.append(ops) {
            self.revert(undo);
            return Err(e);
        }
        if p.wal_len() > CHECKPOINT_BYTES && self.tx.is_none() {
            self.checkpoint()?;
        }
        Ok(())
    }

    // ----- transactions ----------------------------------------------------------

    pub fn begin(&mut self) -> Result<()> {
        if self.tx.is_some() {
            return Err(Error::runtime("a transaction is already in progress"));
        }
        self.tx = Some(Tx::default());
        Ok(())
    }

    pub fn commit(&mut self) -> Result<()> {
        let tx = self.tx.take().ok_or_else(|| Error::runtime("no transaction is in progress"))?;
        self.log(&tx.ops, tx.undo)
    }

    pub fn rollback(&mut self) -> Result<()> {
        let tx = self.tx.take().ok_or_else(|| Error::runtime("no transaction is in progress"))?;
        self.revert(tx.undo);
        Ok(())
    }

    // ----- durability ------------------------------------------------------------

    /// Writes a fresh snapshot and empties the write-ahead log.
    pub fn checkpoint(&mut self) -> Result<()> {
        if self.tx.is_some() {
            return Err(Error::runtime("cannot checkpoint inside a transaction"));
        }
        if let Some(p) = &mut self.persist {
            p.checkpoint(
                self.tables.values(),
                self.views.values().map(|(n, s)| (n.as_str(), s.as_str())),
                self.triggers.values().map(|t| (t.name.as_str(), t.sql.as_str())),
            )?;
        }
        Ok(())
    }

    /// "Compact and Repair": drops deleted-row slots, rebuilds every index,
    /// and rewrites the file. Row ids are renumbered.
    pub fn compact(&mut self) -> Result<()> {
        if self.tx.is_some() {
            return Err(Error::runtime("cannot compact inside a transaction"));
        }
        for t in self.tables.values_mut() {
            t.compact()?;
        }
        self.checkpoint()
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            self.revert(tx.undo);
        }
        if self.persist.as_ref().is_some_and(Persistence::is_dirty) {
            let _ = self.checkpoint();
        }
    }
}

/// Parses the stored text of a trigger.
fn parse_trigger(sql: &str) -> Result<TriggerDef> {
    match parse_statement(sql)? {
        Statement::CreateTrigger(def) => Ok(*def),
        _ => Err(Error::Corrupt("a stored trigger is not a CREATE TRIGGER statement".into())),
    }
}
