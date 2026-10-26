//! The database handle: catalog, transactions, and durability.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::sequence::SequenceDef;
use crate::sql::ast::{ReportDef, RoutineDef, Statement, TriggerDef};
use crate::sql::parse_statement;
use crate::storage::persist::{Durability, Persistence};
use crate::storage::mvcc::{self, View};
use crate::storage::{Op, RowId, Table};

/// Log size that triggers an automatic checkpoint.
const CHECKPOINT_BYTES: u64 = 64 << 20;

#[derive(Default)]
pub(crate) struct Tx {
    /// Committed to the log on COMMIT (only retained for persistent databases).
    ops: Vec<Op>,
    /// How to take back everything done so far, oldest first.
    undo: Vec<Undo>,
    savepoints: Vec<Savepoint>,
    /// `SET CONSTRAINTS` in force, latest last: the relationship (`None`: all) and whether it is deferred.
    modes: Vec<(Option<String>, bool)>,
    /// Deferred relationships (child table, constraint) to verify when the transaction ends.
    pending: std::collections::BTreeSet<(String, String)>,
    /// The id of the transaction, and the commit timestamp its snapshot was taken at (databases that
    /// are shared; see `mvcc`).
    pub(crate) id: u64,
    pub(crate) snapshot: u64,
    /// Opened by the engine around one statement, not by the user.
    auto: bool,
    /// Has changed the schema, so no other transaction may run until it ends.
    ddl: bool,
}

/// A point in a transaction that can be returned to without abandoning the rest.
struct Savepoint {
    name: String,
    undo_len: usize,
    ops_len: usize,
}

/// One step of an undo log.
pub(crate) enum Undo {
    /// The inverse of a row operation.
    Dml(Op),
    /// What a schema change replaced.
    Catalog(Box<CatalogUndo>),
}

/// The parts of the catalog a schema change touched, as they were before it.
pub(crate) struct CatalogUndo {
    /// Lowercase table name and the table as it was (`None`: it did not exist).
    tables: Vec<(String, Option<Table>)>,
    views: Option<BTreeMap<String, (String, String)>>,
    triggers: Option<BTreeMap<String, Arc<TriggerDef>>>,
    /// Lowercase sequence name and its definition before the change (`None`: it did not exist).
    sequence: Option<(String, Option<SequenceDef>)>,
    routines: Option<BTreeMap<String, Arc<RoutineDef>>>,
    reports: Option<BTreeMap<String, Arc<ReportDef>>>,
    catalog: Option<BTreeMap<(String, String), String>>,
}

pub struct Database {
    pub(crate) tables: BTreeMap<String, Table>,
    /// Saved queries: lowercase name → (display name, SQL text).
    pub(crate) views: BTreeMap<String, (String, String)>,
    /// Triggers by lowercase name; the definition keeps its own source text.
    pub(crate) triggers: BTreeMap<String, Arc<TriggerDef>>,
    /// Sequences; shared with the expression evaluator (`NEXTVAL`).
    pub(crate) seqs: crate::sequence::Shared,
    /// Stored functions and procedures by lowercase name.
    pub(crate) routines: BTreeMap<String, Arc<RoutineDef>>,
    /// Stored reports by lowercase name.
    pub(crate) reports: BTreeMap<String, Arc<ReportDef>>,
    /// The rows of one table changed so far by a statement with `RETURNING`.
    pub(crate) capture: Option<Capture>,
    /// How many procedures are running, one inside another.
    pub(crate) call_depth: std::sync::atomic::AtomicU32,
    /// While above zero (inside the body of a view), privileges are not checked.
    pub(crate) priv_suspended: std::sync::atomic::AtomicU32,
    /// Set while an `UPDATE ONLY` / `DELETE FROM ONLY` runs.
    pub(crate) only_dml: std::sync::atomic::AtomicBool,
    /// SQL text recently read, with the statements it came to.
    pub(crate) parse_cache: Mutex<HashMap<String, Arc<Vec<Statement>>>>,
    /// `PREPARE`d statements of this session.
    pub(crate) prepared: BTreeMap<String, crate::exec::Prepared>,
    /// Compiled calculated-field expressions per table; see `calc.rs`.
    pub(crate) calc_cache: crate::calc::CalcCache,
    /// Whether the batch being applied touches a table with calculated fields.
    pub(crate) calc_active: bool,
    /// Collects the plan while an `EXPLAIN` runs.
    pub(crate) plan_log: crate::exec::optimizer::PlanLog,
    /// Whether the query optimiser is on (it is, by default).
    pub(crate) optimizer_on: bool,
    /// The session time zone (`SET TIME ZONE`); not stored in the file.
    pub(crate) time_zone: crate::tz::Zone,
    persist: Option<Persistence>,
    pub(crate) tx: Option<Tx>,
    /// Whether rows are kept as versions, so that transactions can overlap (see `SharedDatabase`).
    pub(crate) mvcc: bool,
    /// Run-time parameters that have been set (`SET name TO value`); see `exec/settings.rs`.
    pub(crate) settings: BTreeMap<String, String>,
    /// Catalog objects without a table of their own: `(kind, name)` to a description in text. See `exec/catalog.rs`.
    pub(crate) catalog: BTreeMap<(String, String), String>,
    /// The commit timestamp of the latest commit.
    clock: u64,
    next_txn: u64,
    /// Snapshots of the open transactions, with how many have each: versions older than the oldest are garbage.
    active: BTreeMap<u64, usize>,
    /// Rows with versions waiting to be cleaned away (table, row).
    gc_queue: std::collections::BTreeSet<(String, RowId)>,
    /// The oldest snapshot as of the last full sweep of the queue.
    gc_floor: u64,
    /// The transaction that has changed the schema and is still open.
    ddl_owner: Option<u64>,
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
            seqs: Default::default(),
            routines: BTreeMap::new(),
            reports: BTreeMap::new(),
            capture: None,
            call_depth: std::sync::atomic::AtomicU32::new(0),
            priv_suspended: std::sync::atomic::AtomicU32::new(0),
            only_dml: std::sync::atomic::AtomicBool::new(false),
            parse_cache: Mutex::new(HashMap::new()),
            prepared: BTreeMap::new(),
            calc_cache: Default::default(),
            calc_active: false,
            plan_log: Default::default(),
            optimizer_on: true,
            time_zone: crate::tz::Zone::UTC,
            persist: None,
            tx: None,
            mvcc: std::env::var_os("ACCESSOR_FORCE_MVCC").is_some(),
            settings: BTreeMap::new(),
            catalog: BTreeMap::new(),
            clock: 0,
            next_txn: 0,
            active: BTreeMap::new(),
            gc_queue: Default::default(),
            gc_floor: 0,
            ddl_owner: None,
        }
    }

    /// Makes the database ready to be shared by several transactions at once: from now on rows are
    /// kept as versions. Done by [`SharedDatabase`](crate::SharedDatabase).
    pub(crate) fn share(&mut self) -> Result<()> {
        if self.tx.is_some() {
            return Err(Error::runtime("a database cannot be shared inside a transaction"));
        }
        self.mvcc = true;
        Ok(())
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
            {
                let mut s = db.seqs.lock().expect("fresh lock");
                for d in l.sequences {
                    s.defs.insert(key(&d.name), d);
                }
            }
            for (table, name, deferrable, initially_deferred) in l.fk_timing {
                if let Some(t) = db.tables.get_mut(&key(&table)) {
                    let _ = t.set_foreign_key_timing(&name, deferrable, initially_deferred);
                }
            }
            for (kind, name, value) in l.catalog {
                db.catalog.insert((kind, name), value);
            }
            for (_, sql) in l.reports {
                let def = parse_report(&sql)?;
                db.reports.insert(key(&def.name), Arc::new(def));
            }
            for (_, sql) in l.routines {
                let def = parse_routine(&sql)?;
                db.routines.insert(key(&def.name), Arc::new(def));
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

    /// Closes the files without the final checkpoint, as if the process had died at this point:
    /// what was committed is in the log and is recovered when the database is next opened. For
    /// testing recovery.
    #[doc(hidden)]
    pub fn simulate_crash(mut self) {
        if let Some(tx) = self.tx.take() {
            self.revert(tx.undo);
        }
        self.persist = None;
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

    /// The stored functions and procedures: name, and whether it is a function.
    pub fn routine_names(&self) -> Vec<(&str, bool)> {
        self.routines.values().map(|r| (r.name.as_str(), r.function)).collect()
    }

    /// The stored reports.
    pub fn report_names(&self) -> Vec<&str> {
        self.reports.values().map(|r| r.name.as_str()).collect()
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
        let inverse = self.apply_op(op)?;
        if let Some(c) = &mut self.capture {
            c.record(op, inverse.as_ref());
        }
        Ok(inverse)
    }

    fn apply_op(&mut self, op: &Op) -> Result<Option<Op>> {
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
                self.triggers.retain(|_, t| !t.table.eq_ignore_ascii_case(name)); // INSTEAD OF triggers go with their query
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
            Op::RenameTable { from, to } => {
                let (kf, kt) = (key(from), key(to));
                if !self.tables.contains_key(&kf) {
                    return Err(Error::catalog(format!("the table '{from}' does not exist")));
                }
                if kf != kt && (self.tables.contains_key(&kt) || self.views.contains_key(&kt)) {
                    return Err(Error::catalog(format!("'{to}' already exists")));
                }
                let mut t = self.tables.remove(&kf).expect("checked above");
                t.set_name(to);
                self.tables.insert(kt, t);
                for t in self.tables.values_mut() {
                    t.rename_references(from, to);
                }
                // Triggers and saved queries name tables in their text; follow the rename.
                let triggers = std::mem::take(&mut self.triggers);
                for (k, def) in triggers {
                    let sql = crate::sql::parser::rename_table_in_sql(&def.sql, from, to);
                    let def = parse_trigger(&sql).map(Arc::new).unwrap_or(def);
                    self.triggers.insert(k, def);
                }
                for (_, (_, sql)) in self.views.iter_mut() {
                    *sql = crate::sql::parser::rename_table_in_sql(sql, from, to);
                }
                self.calc_cache.clear();
                Ok(None)
            }
            Op::SetDefault { table, name, value } => {
                self.table_mut(table)?.set_default(name, value.clone())?;
                Ok(None)
            }
            Op::SetNotNull { table, name, not_null } => {
                self.table_mut(table)?.set_not_null(name, *not_null)?;
                Ok(None)
            }
            Op::SetForeignKeyTiming { table, name, deferrable, initially_deferred } => {
                self.table_mut(table)?.set_foreign_key_timing(name, *deferrable, *initially_deferred)?;
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
            Op::CreateSequence(d) => {
                let mut s = self.seqs.lock().expect("sequences lock");
                let k = key(&d.name);
                if s.defs.contains_key(&k) {
                    return Err(Error::catalog(format!("the sequence '{}' already exists", d.name)));
                }
                s.defs.insert(k, d.clone());
                Ok(None)
            }
            Op::DropSequence(name) => {
                let mut s = self.seqs.lock().expect("sequences lock");
                s.defs.remove(&key(name)).ok_or_else(|| Error::catalog(format!("the sequence '{name}' does not exist")))?;
                Ok(None)
            }
            Op::AlterSequence(d) => {
                let mut s = self.seqs.lock().expect("sequences lock");
                let slot = s
                    .defs
                    .get_mut(&key(&d.name))
                    .ok_or_else(|| Error::catalog(format!("the sequence '{}' does not exist", d.name)))?;
                *slot = d.clone();
                Ok(None)
            }
            Op::SequenceValue { name, last } => {
                // A value logged for a sequence that was since dropped, or never committed, is ignored.
                if let Some(d) = self.seqs.lock().expect("sequences lock").defs.get_mut(&key(name)) {
                    d.last = *last;
                }
                Ok(None)
            }
            Op::Catalog { kind, name, value } => {
                let k = (kind.clone(), key(name));
                match value {
                    Some(v) => {
                        self.catalog.insert(k, v.clone());
                    }
                    None => {
                        self.catalog.remove(&k);
                    }
                }
                Ok(None)
            }
            Op::CreateReport { name, sql } => {
                let def = parse_report(sql)?;
                let k = key(name);
                if self.reports.contains_key(&k) {
                    return Err(Error::catalog(format!("the report '{name}' already exists")));
                }
                self.reports.insert(k, Arc::new(def));
                Ok(None)
            }
            Op::DropReport(name) => {
                self.reports
                    .remove(&key(name))
                    .ok_or_else(|| Error::catalog(format!("the report '{name}' does not exist")))?;
                Ok(None)
            }
            Op::CreateRoutine { name, sql } => {
                let def = parse_routine(sql)?;
                let k = key(name);
                if self.routines.contains_key(&k) {
                    return Err(Error::catalog(format!("the function or procedure '{name}' already exists")));
                }
                self.routines.insert(k, Arc::new(def));
                Ok(None)
            }
            Op::DropRoutine(name) => {
                self.routines
                    .remove(&key(name))
                    .ok_or_else(|| Error::catalog(format!("the function or procedure '{name}' does not exist")))?;
                Ok(None)
            }
            Op::CreateTrigger { name, sql } => {
                let def = parse_trigger(sql)?;
                if def.timing == crate::sql::ast::TriggerTiming::InsteadOf {
                    if !self.views.contains_key(&key(&def.table)) {
                        return Err(Error::catalog(format!("the query '{}' does not exist", def.table)));
                    }
                } else {
                    self.table_checked(&def.table)?;
                }
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
                let old = self.table_mut(table)?.remove(*id)?;
                Ok(Some(Op::Insert { table: table.clone(), id: *id, row: old }))
            }
        }
    }

    pub(crate) fn table_mut(&mut self, name: &str) -> Result<&mut Table> {
        self.tables
            .get_mut(&key(name))
            .ok_or_else(|| Error::catalog(format!("the table '{name}' does not exist")))
    }

    /// Takes back a sequence of changes, newest first.
    pub(crate) fn revert(&mut self, undo: Vec<Undo>) {
        for step in undo.into_iter().rev() {
            match step {
                // Inverses of operations that just succeeded cannot fail.
                Undo::Dml(op) if self.mvcc => match &op {
                    // What each inverse takes back, in the version chain of the row.
                    Op::Delete { table, id } => self.unwind(table, |t| t.unwind_insert(*id)),
                    Op::Update { table, id, .. } => self.unwind(table, |t| t.unwind_update(*id)),
                    Op::Insert { table, id, .. } => self.unwind(table, |t| t.unwind_delete(*id)),
                    _ => {}
                },
                Undo::Dml(op) => {
                    let _ = self.apply(&op);
                }
                Undo::Catalog(c) => self.restore_catalog(*c),
            }
        }
    }

    fn unwind(&mut self, table: &str, f: impl FnOnce(&mut Table)) {
        if let Some(t) = self.tables.get_mut(&key(table)) {
            f(t);
        }
    }

    fn revert_rows(&mut self, undo: Vec<Op>) {
        self.revert(undo.into_iter().map(Undo::Dml).collect());
    }

    /// Records what the schema change `op` is about to replace.
    fn catalog_snapshot(&self, op: &Op) -> CatalogUndo {
        let mut names: Vec<String> = Vec::new();
        let mut undo = CatalogUndo { tables: Vec::new(), views: None, triggers: None, sequence: None, routines: None, reports: None, catalog: None };
        match op {
            Op::CreateTable(schema) => names.push(key(&schema.name)),
            Op::DropTable(table) => {
                names.push(key(table));
                undo.triggers = Some(self.triggers.clone()); // they go with their table
            }
            Op::RenameTable { from, to } => {
                names.push(key(from));
                names.push(key(to));
                for t in self.tables.values() {
                    if t.schema().foreign_keys.iter().any(|f| f.ref_table.eq_ignore_ascii_case(from)) {
                        names.push(key(t.name()));
                    }
                }
                undo.views = Some(self.views.clone());
                undo.triggers = Some(self.triggers.clone());
            }
            Op::CreateView { .. } => undo.views = Some(self.views.clone()),
            Op::DropView(_) => {
                undo.views = Some(self.views.clone());
                undo.triggers = Some(self.triggers.clone());
            }
            Op::CreateSequence(SequenceDef { name, .. }) | Op::DropSequence(name) | Op::AlterSequence(SequenceDef { name, .. }) => {
                let k = key(name);
                let old = self.seqs.lock().expect("sequences lock").defs.get(&k).cloned();
                undo.sequence = Some((k, old));
            }
            Op::SequenceValue { .. } => {}
            Op::Catalog { .. } => undo.catalog = Some(self.catalog.clone()),
            Op::CreateRoutine { .. } | Op::DropRoutine(_) => undo.routines = Some(self.routines.clone()),
            Op::CreateReport { .. } | Op::DropReport(_) => undo.reports = Some(self.reports.clone()),
            Op::CreateTrigger { .. } | Op::DropTrigger(_) => undo.triggers = Some(self.triggers.clone()),
            Op::DropColumn { table, .. } => {
                names.push(key(table));
                // Relationships in other tables that point at the later fields are renumbered.
                for t in self.tables.values() {
                    if t.schema().foreign_keys.iter().any(|f| f.ref_table.eq_ignore_ascii_case(table)) {
                        names.push(key(t.name()));
                    }
                }
            }
            Op::CreateIndex { table, .. }
            | Op::DropIndex { table, .. }
            | Op::AddColumn { table, .. }
            | Op::RenameColumn { table, .. }
            | Op::AlterColumnType { table, .. }
            | Op::AddForeignKey { table, .. }
            | Op::DropForeignKey { table, .. }
            | Op::SetForeignKeyTiming { table, .. }
            | Op::SetDefault { table, .. }
            | Op::SetNotNull { table, .. }
            | Op::SetExpression { table, .. } => names.push(key(table)),
            Op::Insert { .. } | Op::Update { .. } | Op::Delete { .. } => {}
        }
        names.sort();
        names.dedup();
        undo.tables = names.into_iter().map(|n| (self.tables.get(&n).cloned(), n)).map(|(t, n)| (n, t)).collect();
        undo
    }

    fn restore_catalog(&mut self, undo: CatalogUndo) {
        for (name, table) in undo.tables {
            match table {
                Some(t) => {
                    self.tables.insert(name, t);
                }
                None => {
                    self.tables.remove(&name);
                }
            }
        }
        if let Some(views) = undo.views {
            self.views = views;
        }
        if let Some(triggers) = undo.triggers {
            self.triggers = triggers;
        }
        if let Some(routines) = undo.routines {
            self.routines = routines;
        }
        if let Some(reports) = undo.reports {
            self.reports = reports;
        }
        if let Some(catalog) = undo.catalog {
            self.catalog = catalog;
        }
        if let Some((k, old)) = undo.sequence {
            let mut s = self.seqs.lock().expect("sequences lock");
            match old {
                Some(d) => {
                    s.defs.insert(k, d);
                }
                None => {
                    s.defs.remove(&k);
                }
            }
        }
        self.calc_cache.clear();
    }

    /// Atomically applies a batch: either every op takes effect (and is made
    /// durable, or queued in the open transaction) or none does.
    ///
    /// A batch holds either row operations or schema changes, never both.
    pub(crate) fn apply_all(&mut self, ops: Vec<Op>) -> Result<()> {
        if self.mvcc && self.tx.is_none() {
            // Rows are versions: even a lone statement is a transaction, committed at its end.
            self.start_tx(true);
            return match self.apply_all(ops) {
                Ok(()) => self.commit(),
                Err(e) => {
                    let _ = self.rollback();
                    Err(e)
                }
            };
        }
        let has_schema_change = ops.iter().any(Op::is_ddl);
        if has_schema_change && self.mvcc {
            self.claim_schema()?;
        }
        if has_schema_change && ops.iter().any(Op::is_dml) {
            return Err(Error::runtime("a schema change cannot share a batch with row changes"));
        }
        let mut ops = ops;
        let undo: Vec<Undo> = if has_schema_change {
            // Remember what each change replaces whenever it might have to be taken back:
            // in a transaction, when the log may refuse it, or when a later part of the batch may fail.
            let remember = self.tx.is_some() || self.persist.is_some() || ops.len() > 1;
            let mut undo = Vec::new();
            for op in &ops {
                let snapshot = remember.then(|| self.catalog_snapshot(op));
                match self.apply(op) {
                    Ok(_) => undo.extend(snapshot.map(|s| Undo::Catalog(Box::new(s)))),
                    Err(e) => {
                        if let Some(s) = snapshot {
                            self.restore_catalog(s);
                        }
                        self.revert(undo);
                        return Err(e);
                    }
                }
            }
            undo
        } else {
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
                self.revert_rows(undo);
                return Err(e);
            }
            undo.into_iter().map(Undo::Dml).collect()
        };
        if let Some(tx) = &mut self.tx {
            if self.persist.is_some() {
                tx.ops.extend(ops);
            }
            tx.undo.extend(undo);
            return Ok(());
        }
        self.log(&ops, undo)
    }

    fn log(&mut self, ops: &[Op], undo: Vec<Undo>) -> Result<()> {
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
    //
    // Everything done after `begin` — rows and schema alike — either becomes
    // permanent together at `commit` (one atomic record in the write-ahead log)
    // or is taken back at `rollback`. A statement that fails inside a
    // transaction undoes only itself; the transaction stays open.

    /// Starts a transaction. Transactions do not nest; use savepoints inside one.
    pub fn begin(&mut self) -> Result<()> {
        if self.tx.is_some() {
            return Err(Error::runtime("a transaction is already in progress"));
        }
        self.start_tx(false);
        Ok(())
    }

    fn start_tx(&mut self, auto: bool) {
        let mut tx = Tx { auto, ..Tx::default() };
        if self.mvcc {
            self.next_txn += 1;
            tx.id = self.next_txn;
            tx.snapshot = self.clock;
            *self.active.entry(tx.snapshot).or_insert(0) += 1;
        }
        self.tx = Some(tx);
        mvcc::set_view(self.view_of());
    }

    /// The transaction is over, however it ended: it no longer holds a snapshot or the schema.
    fn finish_tx(&mut self, id: u64, snapshot: u64) {
        self.release_tx(id, snapshot);
        // The oldest snapshot may have moved on: versions kept for it can go.
        if self.mvcc && !self.gc_queue.is_empty() {
            self.sweep(Default::default());
        }
        mvcc::set_view(self.view_of());
    }

    fn release_tx(&mut self, id: u64, snapshot: u64) {
        if self.mvcc {
            if let Some(n) = self.active.get_mut(&snapshot) {
                *n -= 1;
                if *n == 0 {
                    self.active.remove(&snapshot);
                }
            }
            if self.ddl_owner == Some(id) {
                self.ddl_owner = None;
            }
        }
    }

    /// A transaction that changes the schema must be the only one open, and keeps everyone else out
    /// until it ends: the catalog has no versions.
    fn claim_schema(&mut self) -> Result<()> {
        let open: usize = self.active.values().sum();
        if open > 1 {
            return Err(Error::busy("a schema change must wait for the other open transactions to end"));
        }
        if let Some(tx) = &mut self.tx {
            tx.ddl = true;
            self.ddl_owner = Some(tx.id);
        }
        Ok(())
    }

    /// The rows (table, id) the open transaction has changed.
    fn written(tx: &Tx) -> std::collections::BTreeSet<(String, RowId)> {
        tx.undo
            .iter()
            .filter_map(|u| match u {
                Undo::Dml(Op::Insert { table, id, .. } | Op::Update { table, id, .. } | Op::Delete { table, id }) => Some((key(table), *id)),
                _ => None,
            })
            .collect()
    }

    /// Makes every change since `begin` permanent.
    pub fn commit(&mut self) -> Result<()> {
        let tx = self.tx.take().ok_or_else(no_transaction)?;
        mvcc::set_view(if self.mvcc { View { snapshot: tx.snapshot, txn: tx.id } } else { View::LATEST });
        if let Err(e) = self.verify_deferred(&tx.pending) {
            self.revert(tx.undo);
            self.finish_tx(tx.id, tx.snapshot);
            return Err(e);
        }
        // A sequence made in the transaction is logged as it was made; bring its value up to date.
        let made: Vec<Op> = {
            let s = self.seqs.lock().expect("sequences lock");
            tx.ops
                .iter()
                .filter_map(|op| match op {
                    Op::CreateSequence(d) | Op::AlterSequence(d) => s.defs.get(&key(&d.name)),
                    _ => None,
                })
                .map(|d| Op::SequenceValue { name: d.name.clone(), last: d.last })
                .collect()
        };
        let (id, snapshot, ddl) = (tx.id, tx.snapshot, tx.ddl);
        let slots = if self.mvcc { Self::written(&tx) } else { Default::default() };
        if let Err(e) = self.log(&tx.ops, tx.undo) {
            self.finish_tx(id, snapshot);
            return Err(e);
        }
        self.release_tx(id, snapshot);
        if self.mvcc {
            self.stamp_commit(id, ddl, slots);
        }
        mvcc::set_view(self.view_of());
        if let (Some(p), false) = (&mut self.persist, made.is_empty()) {
            p.append(&made)?;
        }
        Ok(())
    }

    /// Gives the transaction's changes a commit timestamp, and cleans up what is no longer needed.
    fn stamp_commit(&mut self, id: u64, ddl: bool, slots: std::collections::BTreeSet<(String, RowId)>) {
        if ddl || !slots.is_empty() {
            let ts = self.clock + 1;
            if ddl {
                for t in self.tables.values_mut() {
                    t.commit_all(id, ts);
                }
            } else {
                for (table, row) in &slots {
                    if let Some(t) = self.tables.get_mut(table) {
                        t.commit_slot(*row, id, ts);
                    }
                }
            }
            self.clock = ts;
        }
        let fresh = if ddl {
            // The tables were replaced: find again what needs cleaning.
            self.gc_queue.clear();
            self.tables.iter().flat_map(|(name, t)| t.unsettled().into_iter().map(|row| (name.clone(), row))).collect()
        } else {
            slots
        };
        self.sweep(fresh);
    }

    /// Cleans away the versions nobody from `oldest` on can see.
    fn sweep(&mut self, fresh: std::collections::BTreeSet<(String, RowId)>) {
        let mut snapshots: Vec<u64> = self.active.keys().copied().collect();
        snapshots.push(self.clock);
        let oldest = snapshots[0];
        // Rows just changed are cleaned at once; the rest of the queue when the oldest snapshot has moved on.
        let work = if oldest > self.gc_floor {
            self.gc_floor = oldest;
            let mut all = std::mem::take(&mut self.gc_queue);
            all.extend(fresh);
            all
        } else {
            fresh
        };
        for (table, row) in work {
            let Some(t) = self.tables.get_mut(&table) else { continue };
            if t.clean_slot(row, &snapshots) {
                self.gc_queue.remove(&(table, row));
            } else {
                self.gc_queue.insert((table, row));
            }
        }
    }

    /// Takes back every change since `begin`.
    pub fn rollback(&mut self) -> Result<()> {
        let tx = self.tx.take().ok_or_else(no_transaction)?;
        self.revert(tx.undo);
        self.finish_tx(tx.id, tx.snapshot);
        Ok(())
    }

    /// Runs the open transaction `tx` (if any) in place of the database's own, for a connection that
    /// keeps its transaction between calls.
    pub(crate) fn swap_tx(&mut self, other: &mut Option<Tx>) {
        std::mem::swap(&mut self.tx, other);
        mvcc::set_view(self.view_of());
    }

    /// Runs `f` in a transaction: committed if it returns `Ok`, rolled back if
    /// it returns an error (which is then returned). `f` must not end the
    /// transaction itself.
    ///
    /// ```
    /// # use accessor_core::Database;
    /// let mut db = Database::in_memory();
    /// db.execute("CREATE TABLE t (n INTEGER)").unwrap();
    /// let failed = db.transaction(|db| {
    ///     db.execute("INSERT INTO t VALUES (1)")?;
    ///     db.execute("INSERT INTO t VALUES ('not a number')")?;
    ///     Ok(())
    /// });
    /// assert!(failed.is_err());
    /// assert_eq!(db.table("t").unwrap().len(), 0); // the first insert went with it
    /// ```
    pub fn transaction<T>(&mut self, f: impl FnOnce(&mut Database) -> Result<T>) -> Result<T> {
        self.begin()?;
        match f(self) {
            Ok(value) => {
                self.commit()?;
                Ok(value)
            }
            Err(e) => {
                let _ = self.rollback();
                Err(e)
            }
        }
    }

    /// Turns the query optimiser on or off. Off, queries run exactly as written: joins
    /// in the order given, conditions applied after the joins, no index-ordered reads.
    /// Answers are the same either way; this exists to compare plans and for testing.
    pub fn set_optimizer(&mut self, on: bool) {
        self.optimizer_on = on;
    }

    /// Sets the session time zone: how timestamps without an offset are read, and how
    /// `TIMESTAMPTZ` values are shown. Accepts `UTC`, an offset (`+05:30`) or an IANA
    /// name (`Europe/Paris`). It is a property of this `Database` value, not of the file.
    pub fn set_time_zone(&mut self, name: &str) -> Result<()> {
        let zone = if name.eq_ignore_ascii_case("local") || name.eq_ignore_ascii_case("default") {
            crate::tz::Zone::UTC
        } else {
            crate::tz::Zone::parse(name).ok_or_else(|| Error::runtime(format!("unknown time zone '{name}'")))?
        };
        self.time_zone = zone;
        crate::tz::set_session(zone);
        Ok(())
    }

    pub fn time_zone(&self) -> String {
        self.time_zone.name()
    }

    /// Makes this database's zone the one values are read and shown in.
    pub(crate) fn enter_session(&self) {
        crate::tz::set_session(self.time_zone);
        crate::sequence::set_current(&self.seqs);
        // A connection reading for a transaction that is not the one the database has open says so.
        mvcc::set_view(mvcc::override_view().unwrap_or_else(|| self.view_of()));
    }

    /// What the open transaction sees; outside one, everything committed so far.
    pub(crate) fn view_of(&self) -> View {
        if !self.mvcc {
            return View::LATEST;
        }
        match &self.tx {
            Some(tx) => View { snapshot: tx.snapshot, txn: tx.id },
            None => View { snapshot: self.clock, txn: 0 },
        }
    }

    /// Rows of `t` whose `cols` equal `key`, for a foreign key to respect: in a shared database, the
    /// newest state of the table rather than what the caller's snapshot shows.
    pub(crate) fn rows_with(&self, t: &Table, cols: &[usize], key: &[crate::value::Value]) -> Result<Vec<RowId>> {
        if self.mvcc {
            t.lookup_latest(cols, key)
        } else {
            Ok(t.lookup(cols, key))
        }
    }

    /// Fails while another transaction has changed the schema and not yet finished.
    pub(crate) fn admit(&self) -> Result<()> {
        match self.ddl_owner {
            Some(owner) if owner != mvcc::view().txn => Err(Error::busy("another transaction is changing the schema")),
            _ => Ok(()),
        }
    }

    pub fn optimizer_enabled(&self) -> bool {
        self.optimizer_on
    }

    /// Runs `f` so that it either happens completely or not at all, with or without an open
    /// transaction (it uses one if there is none, and a savepoint inside one).
    pub(crate) fn atomically<T>(&mut self, f: impl FnOnce(&mut Database) -> Result<T>) -> Result<T> {
        const SAVEPOINT: &str = "__atomic";
        let own = !self.in_transaction();
        if own {
            self.begin()?;
        }
        self.savepoint(SAVEPOINT)?;
        match f(self) {
            Ok(v) => {
                let released = self.release(SAVEPOINT);
                if own {
                    released.and_then(|_| self.commit())?;
                } else {
                    released?;
                }
                Ok(v)
            }
            Err(e) => {
                let _ = self.rollback_to(SAVEPOINT);
                let _ = self.release(SAVEPOINT);
                if own {
                    let _ = self.rollback();
                }
                Err(e)
            }
        }
    }

    /// Number of changes made in the open transaction (0 when there is none).
    pub fn pending_changes(&self) -> usize {
        self.tx.as_ref().map_or(0, |tx| tx.undo.len())
    }

    /// Marks a point in the open transaction to which [`rollback_to`](Self::rollback_to)
    /// can return. Using a name again hides the earlier savepoint of that name.
    pub fn savepoint(&mut self, name: &str) -> Result<()> {
        let tx = self.tx.as_mut().ok_or_else(no_transaction)?;
        tx.savepoints.push(Savepoint { name: key(name), undo_len: tx.undo.len(), ops_len: tx.ops.len() });
        Ok(())
    }

    /// Takes back everything done since savepoint `name`, keeping the
    /// transaction (and the savepoint itself) open. Later savepoints are discarded.
    pub fn rollback_to(&mut self, name: &str) -> Result<()> {
        let tx = self.tx.as_mut().ok_or_else(no_transaction)?;
        let i = tx.savepoints.iter().rposition(|s| s.name == key(name)).ok_or_else(|| no_savepoint(name))?;
        let (undo_len, ops_len) = (tx.savepoints[i].undo_len, tx.savepoints[i].ops_len);
        tx.savepoints.truncate(i + 1);
        tx.ops.truncate(ops_len);
        let tail = tx.undo.split_off(undo_len);
        self.revert(tail);
        Ok(())
    }

    /// Forgets savepoint `name` and any made after it; the changes stay.
    pub fn release(&mut self, name: &str) -> Result<()> {
        let tx = self.tx.as_mut().ok_or_else(no_transaction)?;
        let i = tx.savepoints.iter().rposition(|s| s.name == key(name)).ok_or_else(|| no_savepoint(name))?;
        tx.savepoints.truncate(i);
        Ok(())
    }

    // ----- durability ------------------------------------------------------------

    /// Writes a fresh snapshot and empties the write-ahead log.
    pub fn checkpoint(&mut self) -> Result<()> {
        if self.tx.is_some() {
            return Err(Error::runtime("cannot checkpoint inside a transaction"));
        }
        let sequences: Vec<SequenceDef> = self.seqs.lock().expect("sequences lock").defs.values().cloned().collect();
        let fk_timing = self.fk_timing();
        if let Some(p) = &mut self.persist {
            p.checkpoint(
                self.tables.values(),
                self.views.values().map(|(n, s)| (n.as_str(), s.as_str())),
                self.triggers.values().map(|t| (t.name.as_str(), t.sql.as_str())),
                sequences,
                fk_timing,
                self.routines.values().map(|r| (r.name.as_str(), r.sql.as_str())),
                self.reports.values().map(|r| (r.name.as_str(), r.sql.as_str())),
                self.catalog.iter().map(|((k, n), v)| (k.clone(), n.clone(), v.clone())).collect(),
            )?;
        }
        self.seqs.lock().expect("sequences lock").dirty.clear();
        Ok(())
    }

    /// Writes the numbers sequences have handed out since the last time to the log. These are
    /// never part of a transaction: a number once handed out stays used, even after a rollback.
    pub(crate) fn flush_sequences(&mut self) -> Result<()> {
        let ops: Vec<Op> = {
            let mut s = self.seqs.lock().expect("sequences lock");
            let names = std::mem::take(&mut s.dirty);
            names
                .iter()
                .filter_map(|n| s.defs.get(n))
                .map(|d| Op::SequenceValue { name: d.name.clone(), last: d.last })
                .collect()
        };
        match (&mut self.persist, ops.is_empty()) {
            (Some(p), false) => p.append(&ops),
            _ => Ok(()),
        }
    }

    /// `(table, constraint, deferrable, initially deferred)` for every relationship that can be deferred.
    fn fk_timing(&self) -> Vec<(String, String, bool, bool)> {
        let mut out = Vec::new();
        for t in self.tables.values() {
            for f in t.schema().foreign_keys.iter().filter(|f| f.deferrable) {
                out.push((t.name().to_string(), f.name.clone(), f.deferrable, f.initially_deferred));
            }
        }
        out
    }

    // ----- deferred relationships --------------------------------------------------

    /// True if a violation of `fk` is waiting for the end of the transaction rather than
    /// being refused at the end of the statement.
    pub(crate) fn fk_deferred(&self, fk: &crate::schema::ForeignKey) -> bool {
        let Some(tx) = &self.tx else { return false };
        if tx.auto || !fk.deferrable {
            return false;
        }
        tx.modes
            .iter()
            .rev()
            .find(|(n, _)| n.as_ref().is_none_or(|n| n.eq_ignore_ascii_case(&fk.name)))
            .map_or(fk.initially_deferred, |(_, d)| *d)
    }

    /// Notes that `fk` of `table` must be checked when the transaction ends.
    pub(crate) fn defer_check(&mut self, table: &str, fk: &str) {
        if let Some(tx) = &mut self.tx {
            tx.pending.insert((key(table), key(fk)));
        }
    }

    /// Checks every row of the child tables of the given relationships.
    fn verify_deferred(&self, pending: &std::collections::BTreeSet<(String, String)>) -> Result<()> {
        for (table, name) in pending {
            let Some(t) = self.tables.get(table) else { continue };
            let Some(fk) = t.schema().foreign_keys.iter().find(|f| key(&f.name) == *name) else { continue };
            let Some(parent) = self.table(&fk.ref_table) else { continue };
            for (_, row) in t.iter() {
                let k: Vec<crate::value::Value> = fk.columns.iter().map(|&c| row[c].clone()).collect();
                if k.iter().any(crate::value::Value::is_null) {
                    continue;
                }
                if self.rows_with(parent, &fk.ref_columns, &k)?.is_empty() {
                    let fields: Vec<&str> = fk.columns.iter().map(|&c| t.schema().columns[c].name.as_str()).collect();
                    return Err(Error::constraint(format!(
                        "cannot commit: '{}' ({}) requires a matching record in '{}' (relationship '{}')",
                        t.name(),
                        fields.join(", "),
                        fk.ref_table,
                        fk.name
                    )));
                }
            }
        }
        Ok(())
    }

    /// `SET CONSTRAINTS`: defers the named relationships (or all of them) to the end of the
    /// transaction, or makes them immediate, checking at once what was waiting.
    pub(crate) fn set_constraints(&mut self, names: Option<&[String]>, deferred: bool) -> Result<()> {
        if let Some(names) = names {
            for n in names {
                let found = self
                    .tables
                    .values()
                    .flat_map(|t| &t.schema().foreign_keys)
                    .find(|f| f.name.eq_ignore_ascii_case(n))
                    .ok_or_else(|| Error::catalog(format!("there is no relationship named '{n}'")))?;
                if !found.deferrable {
                    return Err(Error::catalog(format!("the relationship '{n}' is not DEFERRABLE")));
                }
            }
        }
        let Some(tx) = &mut self.tx else { return Ok(()) }; // outside a transaction each statement is its own
        let entries: Vec<Option<String>> = match names {
            None => vec![None],
            Some(ns) => ns.iter().map(|n| Some(n.clone())).collect(),
        };
        let before = tx.modes.len();
        for e in entries {
            tx.modes.push((e, deferred));
        }
        if !deferred {
            let mine: std::collections::BTreeSet<(String, String)> = tx
                .pending
                .iter()
                .filter(|(_, f)| names.is_none_or(|ns| ns.iter().any(|n| key(n) == *f)))
                .cloned()
                .collect();
            if let Err(e) = self.verify_deferred(&mine) {
                if let Some(tx) = &mut self.tx {
                    tx.modes.truncate(before);
                }
                return Err(e);
            }
            if let Some(tx) = &mut self.tx {
                for p in &mine {
                    tx.pending.remove(p);
                }
            }
        }
        Ok(())
    }

    /// "Compact and Repair": drops deleted-row slots, rebuilds every index,
    /// and rewrites the file. Row ids are renumbered.
    pub fn compact(&mut self) -> Result<()> {
        if self.tx.is_some() || !self.active.is_empty() {
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
        let _ = self.flush_sequences();
        if self.persist.as_ref().is_some_and(Persistence::is_dirty) {
            let _ = self.checkpoint();
        }
    }
}

/// What a statement with `RETURNING` has done to the rows of its table: for each row, how it was
/// before and how it is now (an inserted row has no earlier state, a deleted one no later).
pub(crate) struct Capture {
    pub table: String,
    pub changes: BTreeMap<crate::storage::RowId, (Option<crate::value::Row>, Option<crate::value::Row>)>,
}

impl Capture {
    fn record(&mut self, op: &Op, inverse: Option<&Op>) {
        match (op, inverse) {
            (Op::Insert { table, id, row }, _) if key(table) == self.table => {
                self.changes.entry(*id).or_insert((None, None)).1 = Some(row.clone());
            }
            (Op::Update { table, id, row }, Some(Op::Update { row: old, .. })) if key(table) == self.table => {
                let e = self.changes.entry(*id).or_insert((Some(old.clone()), None));
                e.1 = Some(row.clone());
            }
            (Op::Delete { table, id }, Some(Op::Insert { row: old, .. })) if key(table) == self.table => {
                let e = self.changes.entry(*id).or_insert((Some(old.clone()), None));
                e.1 = None;
            }
            _ => {}
        }
    }
}

/// Parses the stored text of a report.
fn parse_report(sql: &str) -> Result<ReportDef> {
    match parse_statement(sql)? {
        Statement::CreateReport(def) => Ok(*def),
        _ => Err(Error::Corrupt("a stored report is not a CREATE REPORT statement".into())),
    }
}

/// Parses the stored text of a function or procedure.
fn parse_routine(sql: &str) -> Result<RoutineDef> {
    match parse_statement(sql)? {
        Statement::CreateRoutine(def) => Ok(*def),
        _ => Err(Error::Corrupt("a stored routine is not a CREATE FUNCTION or CREATE PROCEDURE statement".into())),
    }
}

/// Parses the stored text of a trigger.
fn parse_trigger(sql: &str) -> Result<TriggerDef> {
    match parse_statement(sql)? {
        Statement::CreateTrigger(def) => Ok(*def),
        _ => Err(Error::Corrupt("a stored trigger is not a CREATE TRIGGER statement".into())),
    }
}

fn no_transaction() -> Error {
    Error::runtime("no transaction is in progress")
}

fn no_savepoint(name: &str) -> Error {
    Error::runtime(format!("there is no savepoint named '{name}' in this transaction"))
}
