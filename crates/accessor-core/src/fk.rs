//! Referential integrity: foreign-key checks, cascades, and the guards that
//! stop schema changes from breaking a relationship.
//!
//! Enforcement runs inside [`Database::apply_all`], after the user's
//! operations have been applied, against the resulting state. That makes the
//! outcome independent of row order within a statement (a parent and its
//! children may be inserted together). Cascaded operations are applied
//! immediately and appended to the batch, so the write-ahead log replays the
//! full effect without re-running any checks.

use std::collections::{HashMap, VecDeque};

use crate::database::{key, Database};
use crate::error::{Error, Result};
use crate::schema::{ForeignKey, RefAction};
use crate::sql::ast::TriggerTiming;
use crate::storage::{Op, RowId};
use crate::value::{Row, Value};

/// Child table name and the foreign key it owns, grouped by parent table.
pub(crate) type RefMap = HashMap<String, Vec<(String, ForeignKey)>>;

/// One row-level change that has been applied and still needs checking.
pub(crate) struct Applied {
    pub table: String,
    pub id: RowId,
    pub old: Option<Row>,
    pub new: Option<Row>,
}

fn project(row: &[Value], cols: &[usize]) -> Vec<Value> {
    cols.iter().map(|&c| row[c].clone()).collect()
}

fn same_set(a: &[usize], b: &[usize]) -> bool {
    a.len() == b.len() && a.iter().all(|c| b.contains(c))
}

impl Database {
    pub(crate) fn has_foreign_keys(&self) -> bool {
        self.tables.values().any(|t| !t.schema().foreign_keys.is_empty())
    }

    fn reference_map(&self) -> RefMap {
        let mut map = RefMap::new();
        for t in self.tables.values() {
            for fk in &t.schema().foreign_keys {
                map.entry(key(&fk.ref_table)).or_default().push((t.name().to_string(), fk.clone()));
            }
        }
        map
    }

    // ----- DML enforcement -----------------------------------------------------

    /// Applies `ops` (growing the list with any cascaded operations) and
    /// verifies every relationship they touch. On error the caller reverts
    /// whatever was pushed to `undo`.
    pub(crate) fn apply_enforced(&mut self, ops: &mut Vec<Op>, undo: &mut Vec<Op>) -> Result<()> {
        let refs = self.reference_map();
        let mut queue = VecDeque::new();
        for op in std::mem::take(ops) {
            self.apply_tracked(op, ops, undo, &mut queue, &refs, 0)?;
        }
        while let Some(a) = queue.pop_front() {
            self.check_references_out(&a)?;
            self.check_references_in(&a, &refs, ops, undo, &mut queue)?;
        }
        Ok(())
    }

    /// Applies one operation, firing triggers around it and queueing it for
    /// the relationship checks. `depth` counts nested trigger invocations.
    pub(crate) fn apply_tracked(
        &mut self,
        op: Op,
        ops: &mut Vec<Op>,
        undo: &mut Vec<Op>,
        queue: &mut VecDeque<Applied>,
        refs: &RefMap,
        depth: usize,
    ) -> Result<()> {
        if depth > super::trigger::MAX_DEPTH {
            return Err(Error::runtime("triggers are nested too deeply (is a trigger firing itself?)"));
        }
        let mut op = op;
        if self.calc_active {
            self.finalize_op(&mut op)?;
        }
        let fired = self.trigger_context(&op);
        if let Some(f) = &fired {
            self.fire_triggers(TriggerTiming::Before, f, ops, undo, queue, refs, depth)?;
        }
        let inverse = self.apply(&op)?;
        let change = match (&op, &inverse) {
            (Op::Insert { table, id, row }, _) => Some((table, *id, None, Some(row.clone()))),
            (Op::Update { table, id, row }, Some(Op::Update { row: old, .. })) => {
                Some((table, *id, Some(old.clone()), Some(row.clone())))
            }
            (Op::Delete { table, id }, Some(Op::Insert { row: old, .. })) => Some((table, *id, Some(old.clone()), None)),
            _ => None,
        };
        // Tables outside every relationship need no bookkeeping.
        if let Some((table, id, old, new)) = change {
            if refs.contains_key(&key(table)) || self.table(table).is_some_and(|t| !t.schema().foreign_keys.is_empty()) {
                queue.push_back(Applied { table: table.clone(), id, old, new });
            }
        }
        undo.extend(inverse);
        ops.push(op);
        if let Some(f) = &fired {
            self.fire_triggers(TriggerTiming::After, f, ops, undo, queue, refs, depth)?;
        }
        Ok(())
    }

    /// The changed row's own foreign keys must point at existing rows.
    fn check_references_out(&self, a: &Applied) -> Result<()> {
        let Some(new) = &a.new else { return Ok(()) };
        let t = self.table_checked(&a.table)?;
        // Skip rows a later operation in the same batch already replaced or removed.
        if t.get(a.id) != Some(new) {
            return Ok(());
        }
        for fk in &t.schema().foreign_keys {
            let k = project(new, &fk.columns);
            if k.iter().any(Value::is_null) || a.old.as_ref().is_some_and(|o| project(o, &fk.columns) == k) {
                continue;
            }
            if self.table_checked(&fk.ref_table)?.lookup(&fk.ref_columns, &k).is_empty() {
                let fields: Vec<&str> = fk.columns.iter().map(|&c| t.schema().columns[c].name.as_str()).collect();
                return Err(Error::constraint(format!(
                    "cannot save the record: '{}' ({}) requires a matching record in '{}'",
                    t.name(),
                    fields.join(", "),
                    fk.ref_table
                )));
            }
        }
        Ok(())
    }

    /// Rows that refer to a key this change removed are restricted,
    /// cascaded, or nulled.
    fn check_references_in(
        &mut self,
        a: &Applied,
        refs: &RefMap,
        ops: &mut Vec<Op>,
        undo: &mut Vec<Op>,
        queue: &mut VecDeque<Applied>,
    ) -> Result<()> {
        let (Some(old), Some(children)) = (&a.old, refs.get(&key(&a.table))) else { return Ok(()) };
        for (child_name, fk) in children {
            let old_key = project(old, &fk.ref_columns);
            if old_key.iter().any(Value::is_null) {
                continue;
            }
            let new_key = a.new.as_ref().map(|n| project(n, &fk.ref_columns));
            if new_key.as_ref() == Some(&old_key) {
                continue;
            }
            let parent = self.table_checked(&a.table)?;
            if !parent.lookup(&fk.ref_columns, &old_key).is_empty() {
                continue; // another row still provides the key
            }
            let child = self.table_checked(child_name)?;
            let ids = child.lookup(&fk.columns, &old_key);
            if ids.is_empty() {
                continue;
            }
            let action = if new_key.is_some() { fk.on_update } else { fk.on_delete };
            if action == RefAction::Restrict {
                return Err(Error::constraint(format!(
                    "cannot {} the record in '{}': {} record(s) in '{}' still refer to it",
                    if new_key.is_some() { "change" } else { "delete" },
                    parent.name(),
                    ids.len(),
                    child.name()
                )));
            }
            let mut follow_ups = Vec::with_capacity(ids.len());
            for id in ids {
                let table = child.name().to_string();
                let mut row = child.get(id).expect("id from lookup").clone();
                follow_ups.push(match (action, &new_key) {
                    (RefAction::Cascade, None) => Op::Delete { table, id },
                    (RefAction::Cascade, Some(nk)) => {
                        for (&c, v) in fk.columns.iter().zip(nk) {
                            row[c] = v.clone();
                        }
                        Op::Update { table, id, row }
                    }
                    _ => {
                        for &c in &fk.columns {
                            row[c] = Value::Null;
                        }
                        Op::Update { table, id, row }
                    }
                });
            }
            for op in follow_ups {
                self.apply_tracked(op, ops, undo, queue, refs, 0)?;
            }
        }
        Ok(())
    }

    // ----- schema guards ---------------------------------------------------------

    /// Name of a relationship that uses column `pos` of `table`, on either side.
    fn column_relationship(&self, table: &str, pos: usize) -> Option<String> {
        let own = self.table(table)?;
        if let Some(fk) = own.schema().foreign_keys.iter().find(|f| f.columns.contains(&pos)) {
            return Some(fk.name.clone());
        }
        self.tables
            .values()
            .flat_map(|t| &t.schema().foreign_keys)
            .find(|f| f.ref_table.eq_ignore_ascii_case(table) && f.ref_columns.contains(&pos))
            .map(|f| f.name.clone())
    }

    /// Refuses to delete or retype a field that takes part in a relationship.
    /// Returns the field's position.
    pub(crate) fn guard_column_change(&self, table: &str, column: &str, verb: &str) -> Result<usize> {
        let t = self.table_checked(table)?;
        let pos = t
            .schema()
            .column_index(column)
            .ok_or_else(|| Error::catalog(format!("the field '{column}' does not exist in table '{}'", t.name())))?;
        match self.column_relationship(table, pos) {
            Some(name) => Err(Error::catalog(format!(
                "the field '{column}' cannot be {verb} because it is used by the relationship '{name}'; delete the relationship first"
            ))),
            None => Ok(pos),
        }
    }

    pub(crate) fn guard_drop_table(&self, name: &str) -> Result<()> {
        for t in self.tables.values() {
            if t.name().eq_ignore_ascii_case(name) {
                continue;
            }
            if let Some(fk) = t.schema().foreign_keys.iter().find(|f| f.ref_table.eq_ignore_ascii_case(name)) {
                return Err(Error::catalog(format!(
                    "the table '{name}' cannot be deleted because '{}' refers to it (relationship '{}')",
                    t.name(),
                    fk.name
                )));
            }
        }
        Ok(())
    }

    /// An index that a relationship depends on (as the referenced unique key,
    /// or as the lookup index on the referencing side) must stay.
    pub(crate) fn guard_drop_index(&self, table: &str, index: &str) -> Result<()> {
        let t = self.table_checked(table)?;
        let Some(doomed) = t.index_defs().find(|d| d.name.eq_ignore_ascii_case(index)) else { return Ok(()) };
        let covered = |cols: &[usize], unique: bool| {
            t.index_defs()
                .any(|d| !std::ptr::eq(d, doomed) && (d.unique || !unique) && same_set(&d.columns, cols))
        };
        for fk in &t.schema().foreign_keys {
            if same_set(&doomed.columns, &fk.columns) && !covered(&fk.columns, false) {
                return Err(Error::catalog(format!("the index '{index}' is required by the relationship '{}'", fk.name)));
            }
        }
        for other in self.tables.values() {
            for fk in &other.schema().foreign_keys {
                if fk.ref_table.eq_ignore_ascii_case(table)
                    && doomed.unique
                    && same_set(&doomed.columns, &fk.ref_columns)
                    && !covered(&fk.ref_columns, true)
                {
                    return Err(Error::catalog(format!(
                        "the index '{index}' is required by the relationship '{}' on '{}'",
                        fk.name,
                        other.name()
                    )));
                }
            }
        }
        Ok(())
    }

    /// After field `pos` of `table` was removed, re-point relationships
    /// that referenced later fields of it.
    pub(crate) fn shift_references(&mut self, table: &str, pos: usize) {
        for t in self.tables.values_mut() {
            t.shift_references(table, pos);
        }
    }
}
