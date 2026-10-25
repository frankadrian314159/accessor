//! `CREATE SEQUENCE`, `ALTER SEQUENCE` and `DROP SEQUENCE`.

use super::QueryResult;
use crate::database::{key, Database};
use crate::error::{Error, Result};
use crate::sequence::{SequenceDef, SequenceOptions};
use crate::storage::Op;

impl Database {
    pub(crate) fn exec_create_sequence(&mut self, name: &str, options: &SequenceOptions) -> Result<QueryResult> {
        let def = SequenceDef::create(name, options)?;
        self.apply_all(vec![Op::CreateSequence(def)])?;
        Ok(QueryResult::default())
    }

    pub(crate) fn exec_alter_sequence(&mut self, name: &str, options: &SequenceOptions) -> Result<QueryResult> {
        let mut def = self
            .seqs
            .lock()
            .expect("sequences lock")
            .defs
            .get(&key(name))
            .cloned()
            .ok_or_else(|| Error::catalog(format!("the sequence '{name}' does not exist")))?;
        def.alter(options)?;
        self.apply_all(vec![Op::AlterSequence(def)])?;
        Ok(QueryResult::default())
    }

    pub(crate) fn exec_drop_sequence(&mut self, name: &str) -> Result<QueryResult> {
        let real = self
            .seqs
            .lock()
            .expect("sequences lock")
            .defs
            .get(&key(name))
            .map(|d| d.name.clone())
            .ok_or_else(|| Error::catalog(format!("the sequence '{name}' does not exist")))?;
        self.apply_all(vec![Op::DropSequence(real)])?;
        Ok(QueryResult::default())
    }

    /// The sequences, with their settings and the last number each handed out.
    pub fn sequences(&self) -> Vec<SequenceDef> {
        self.seqs.lock().expect("sequences lock").defs.values().cloned().collect()
    }
}
