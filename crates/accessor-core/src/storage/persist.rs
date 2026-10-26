//! On-disk durability: an atomically replaced snapshot file plus an
//! append-only, checksummed write-ahead log of [`Op`] batches.
//!
//! Layout (all integers little-endian):
//!
//! * `name.adb`      — `"ACDB" version:u32 epoch:u64 crc:u32 len:u64 payload`
//! * `name.adb-wal`  — `"ACWL" version:u32 epoch:u64` then frames of
//!   `len:u32 crc:u32 payload`, each frame one atomically committed batch.
//!
//! The WAL carries the epoch of the snapshot it extends, so a crash between
//! writing a new snapshot and resetting the log can never replay stale ops.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Op, Table};
use crate::error::{Error, Result};
use crate::schema::TableSchema;
use crate::sequence::SequenceDef;
use crate::value::Row;

const SNAP_MAGIC: &[u8; 4] = b"ACDB";
const WAL_MAGIC: &[u8; 4] = b"ACWL";
/// Current file format. Version 6 adds descending index columns (a flag on the column number, so a file
/// without any is laid out as version 5 was). Older versions are still read, and are rewritten as the
/// current one on open; version 1 predates foreign keys.
const VERSION: u32 = 6;
/// The version of the file format this build writes (older versions are still read).
pub const FILE_FORMAT_VERSION: u32 = VERSION;
const V1: u32 = 1;
const V2: u32 = 2;
const V3: u32 = 3;
const V4: u32 = 4;
const V5: u32 = 5;
const WAL_HEADER_LEN: u64 = 16;
const SNAP_HEADER_LEN: usize = 28;
/// Frames larger than this are treated as corruption rather than allocated.
const MAX_FRAME: usize = 1 << 30;

/// How aggressively commits are flushed to stable storage.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Durability {
    /// `fsync` on every commit (default). Survives power loss.
    #[default]
    Fsync,
    /// Hand data to the OS on commit. Survives process crashes only.
    OsBuffered,
}

#[derive(Serialize)]
pub(crate) struct TableOut<'a> {
    pub schema: &'a TableSchema,
    pub rows: Vec<Option<&'a Row>>,
    pub next_auto: i64,
}

#[derive(Serialize)]
pub(crate) struct SnapshotOut<'a> {
    pub tables: Vec<TableOut<'a>>,
    pub views: Vec<(&'a str, &'a str)>,
    pub triggers: Vec<(&'a str, &'a str)>,
    pub sequences: Vec<SequenceDef>,
    /// `(table, constraint, deferrable, initially deferred)` of the relationships that can be deferred.
    pub fk_timing: Vec<(String, String, bool, bool)>,
    pub routines: Vec<(&'a str, &'a str)>,
    pub reports: Vec<(&'a str, &'a str)>,
}

#[derive(Deserialize)]
pub(crate) struct TableIn {
    pub schema: TableSchema,
    pub rows: Vec<Option<Row>>,
    pub next_auto: i64,
}

#[derive(Deserialize)]
pub(crate) struct SnapshotIn {
    pub tables: Vec<TableIn>,
    pub views: Vec<(String, String)>,
    pub triggers: Vec<(String, String)>,
    pub sequences: Vec<SequenceDef>,
    pub fk_timing: Vec<(String, String, bool, bool)>,
    pub routines: Vec<(String, String)>,
    pub reports: Vec<(String, String)>,
}

pub(crate) struct Loaded {
    pub tables: Vec<Table>,
    pub views: Vec<(String, String)>,
    pub triggers: Vec<(String, String)>,
    pub sequences: Vec<SequenceDef>,
    pub fk_timing: Vec<(String, String, bool, bool)>,
    pub routines: Vec<(String, String)>,
    pub reports: Vec<(String, String)>,
    pub epoch: u64,
    /// The file was written in an older format.
    pub legacy: bool,
}

pub(crate) struct Persistence {
    path: PathBuf,
    wal_path: PathBuf,
    wal: File,
    wal_len: u64,
    epoch: u64,
    pub durability: Durability,
    /// Either file is in an older format and must be rewritten.
    pub needs_upgrade: bool,
    /// Holds the exclusive lock on the database for as long as it is open (released when dropped).
    _lock: Option<File>,
}

/// Takes the exclusive lock that keeps a second connection or process from opening the same
/// database. The lock is on a small file beside the database (`name.adb-lock`), not on the database
/// itself, because the database file is replaced at every checkpoint. The operating system drops the
/// lock when the connection closes or the process dies, so a crash never leaves a database locked.
/// On a file system that cannot lock (some network shares) the database opens unlocked.
fn lock_database(path: &Path) -> Result<Option<File>> {
    use std::fs::TryLockError;
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(sibling(path, "-lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Err(Error::Locked(format!(
            "the database '{}' is in use by another connection or process",
            path.display()
        ))),
        Err(TryLockError::Error(e)) if e.kind() == std::io::ErrorKind::Unsupported => Ok(None),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("slice length"))
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("slice length"))
}

impl Persistence {
    /// Opens (or creates) the files for `path`, returning the persisted
    /// snapshot and the committed batches that must be replayed on top of it.
    pub(crate) fn open(path: &Path) -> Result<(Self, Option<Loaded>, Vec<Vec<Op>>)> {
        let lock = lock_database(path)?;
        let wal_path = sibling(path, "-wal");
        let loaded = if path.exists() { Some(read_snapshot(path)?) } else { None };
        let epoch = loaded.as_ref().map_or(0, |l| l.epoch);

        let mut batches = Vec::new();
        let mut valid_len = WAL_HEADER_LEN;
        let mut fresh = true;
        let mut wal_old = false;
        if wal_path.exists() {
            let bytes = fs::read(&wal_path)?;
            if bytes.len() >= WAL_HEADER_LEN as usize && &bytes[..4] == WAL_MAGIC {
                let version = u32_at(&bytes, 4);
                if !matches!(version, V1 | V2 | V3 | V4 | V5 | VERSION) {
                    return Err(Error::Corrupt("unsupported WAL version".into()));
                }
                let wal_legacy = version == V1;
                wal_old = version != VERSION;
                let wal_epoch = u64_at(&bytes, 8);
                if wal_epoch == epoch {
                    fresh = false;
                    let (b, len) = parse_frames(&bytes, wal_legacy);
                    batches = b;
                    valid_len = len;
                } else if wal_epoch > epoch {
                    return Err(Error::Corrupt("write-ahead log is newer than the database file".into()));
                }
                // wal_epoch < epoch: stale log from before the last checkpoint.
            }
        }

        let mut wal = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&wal_path)?;
        if fresh {
            wal.set_len(0)?;
            wal.write_all(&wal_header(epoch))?;
            valid_len = WAL_HEADER_LEN;
        } else {
            wal.set_len(valid_len)?; // discard any torn tail
        }
        wal.seek(SeekFrom::Start(valid_len))?;
        let p = Persistence {
            path: path.to_path_buf(),
            wal_path,
            wal,
            wal_len: valid_len,
            epoch,
            durability: Durability::default(),
            needs_upgrade: wal_old || loaded.as_ref().is_some_and(|l| l.legacy),
            _lock: lock,
        };
        Ok((p, loaded, batches))
    }

    /// Appends one atomic batch.
    pub(crate) fn append(&mut self, ops: &[Op]) -> Result<()> {
        let payload = postcard::to_allocvec(ops).map_err(|e| Error::runtime(format!("encode failed: {e}")))?;
        let mut frame = Vec::with_capacity(payload.len() + 8);
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        frame.extend_from_slice(&payload);
        self.wal.write_all(&frame)?;
        if self.durability == Durability::Fsync {
            self.wal.sync_data()?;
        }
        self.wal_len += frame.len() as u64;
        Ok(())
    }

    pub(crate) fn wal_len(&self) -> u64 {
        self.wal_len
    }

    /// True if the log holds committed batches not yet in the snapshot.
    pub(crate) fn is_dirty(&self) -> bool {
        self.wal_len > WAL_HEADER_LEN
    }

    /// Writes a new snapshot of the given state and resets the log.
    pub(crate) fn checkpoint<'a>(
        &mut self,
        tables: impl Iterator<Item = &'a Table>,
        views: impl Iterator<Item = (&'a str, &'a str)>,
        triggers: impl Iterator<Item = (&'a str, &'a str)>,
        sequences: Vec<SequenceDef>,
        fk_timing: Vec<(String, String, bool, bool)>,
        routines: impl Iterator<Item = (&'a str, &'a str)>,
        reports: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> Result<()> {
        let out = SnapshotOut {
            tables: tables
                .map(|t| TableOut { schema: t.schema(), rows: t.committed_slots(), next_auto: t.next_auto() })
                .collect(),
            views: views.collect(),
            triggers: triggers.collect(),
            sequences,
            fk_timing,
            routines: routines.collect(),
            reports: reports.collect(),
        };
        let payload = postcard::to_allocvec(&out).map_err(|e| Error::runtime(format!("encode failed: {e}")))?;
        let epoch = self.epoch + 1;

        let mut buf = Vec::with_capacity(SNAP_HEADER_LEN + payload.len());
        buf.extend_from_slice(SNAP_MAGIC);
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&epoch.to_le_bytes());
        buf.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        buf.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        buf.extend_from_slice(&payload);

        let tmp = sibling(&self.path, ".tmp");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&buf)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;

        self.wal.set_len(0)?;
        self.wal.seek(SeekFrom::Start(0))?;
        self.wal.write_all(&wal_header(epoch))?;
        if self.durability == Durability::Fsync {
            self.wal.sync_all()?;
        }
        self.wal_len = WAL_HEADER_LEN;
        self.epoch = epoch;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn wal_path(&self) -> &Path {
        &self.wal_path
    }
}

fn wal_header(epoch: u64) -> [u8; WAL_HEADER_LEN as usize] {
    let mut h = [0u8; WAL_HEADER_LEN as usize];
    h[..4].copy_from_slice(WAL_MAGIC);
    h[4..8].copy_from_slice(&VERSION.to_le_bytes());
    h[8..16].copy_from_slice(&epoch.to_le_bytes());
    h
}

/// Decodes frames until the first torn or corrupt one.
fn parse_frames(bytes: &[u8], legacy: bool) -> (Vec<Vec<Op>>, u64) {
    let mut batches = Vec::new();
    let mut pos = WAL_HEADER_LEN as usize;
    while pos + 8 <= bytes.len() {
        let len = u32_at(bytes, pos) as usize;
        let crc = u32_at(bytes, pos + 4);
        let start = pos + 8;
        if len > MAX_FRAME || start + len > bytes.len() {
            break;
        }
        let payload = &bytes[start..start + len];
        if crc32fast::hash(payload) != crc {
            break;
        }
        let decoded = if legacy {
            postcard::from_bytes::<Vec<v1::Op>>(payload).map(|ops| ops.into_iter().map(Op::from).collect())
        } else {
            postcard::from_bytes::<Vec<Op>>(payload)
        };
        match decoded {
            Ok(ops) => batches.push(ops),
            Err(_) => break,
        }
        pos = start + len;
    }
    (batches, pos as u64)
}

fn read_snapshot(path: &Path) -> Result<Loaded> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    if bytes.len() < SNAP_HEADER_LEN || &bytes[..4] != SNAP_MAGIC {
        return Err(Error::Corrupt("not an Accessor database file".into()));
    }
    let version = u32_at(&bytes, 4);
    if !matches!(version, V1 | V2 | V3 | V4 | V5 | VERSION) {
        return Err(Error::Corrupt("unsupported database version".into()));
    }
    let epoch = u64_at(&bytes, 8);
    let crc = u32_at(&bytes, 16);
    let len = u64_at(&bytes, 20) as usize;
    let payload = bytes
        .get(SNAP_HEADER_LEN..SNAP_HEADER_LEN + len)
        .ok_or_else(|| Error::Corrupt("snapshot is truncated".into()))?;
    if crc32fast::hash(payload) != crc {
        return Err(Error::Corrupt("snapshot checksum mismatch".into()));
    }
    let decode_err = |e: postcard::Error| Error::Corrupt(format!("snapshot decode failed: {e}"));
    let snap: SnapshotIn = match version {
        V1 => postcard::from_bytes::<v1::SnapshotIn>(payload).map_err(decode_err)?.into(),
        V2 => postcard::from_bytes::<v2::SnapshotIn>(payload).map_err(decode_err)?.into(),
        V3 => postcard::from_bytes::<v3::SnapshotIn>(payload).map_err(decode_err)?.into(),
        V4 => postcard::from_bytes::<v4::SnapshotIn>(payload).map_err(decode_err)?.into(),
        _ => postcard::from_bytes(payload).map_err(decode_err)?,
    };
    let tables = snap
        .tables
        .into_iter()
        .map(|t| Table::from_parts(t.schema, t.rows, t.next_auto))
        .collect::<Result<Vec<_>>>()?;
    Ok(Loaded {
        tables,
        views: snap.views,
        triggers: snap.triggers,
        sequences: snap.sequences,
        fk_timing: snap.fk_timing,
        routines: snap.routines,
        reports: snap.reports,
        epoch,
        legacy: version != VERSION,
    })
}

/// Version 1 layouts, from before tables carried foreign keys.
mod v1 {
    use serde::Deserialize;

    use super::{SnapshotIn as New, TableIn as NewTable};
    use crate::schema::{Column, IndexDef, TableSchema};
    use crate::storage::{Op as NewOp, RowId};
    use crate::value::{DataType, Row};

    #[derive(Deserialize)]
    pub struct Schema {
        name: String,
        columns: Vec<Column>,
        indexes: Vec<IndexDef>,
    }

    impl From<Schema> for TableSchema {
        fn from(s: Schema) -> Self {
            TableSchema { name: s.name, columns: s.columns, indexes: s.indexes, foreign_keys: Vec::new() }
        }
    }

    #[derive(Deserialize)]
    struct Table {
        schema: Schema,
        rows: Vec<Option<Row>>,
        next_auto: i64,
    }

    #[derive(Deserialize)]
    pub struct SnapshotIn {
        tables: Vec<Table>,
        views: Vec<(String, String)>,
    }

    impl From<SnapshotIn> for New {
        fn from(s: SnapshotIn) -> Self {
            New {
                tables: s
                    .tables
                    .into_iter()
                    .map(|t| NewTable { schema: t.schema.into(), rows: t.rows, next_auto: t.next_auto })
                    .collect(),
                views: s.views,
                triggers: Vec::new(),
                sequences: Vec::new(),
                fk_timing: Vec::new(),
                routines: Vec::new(),
                reports: Vec::new(),
            }
        }
    }

    /// Variant order must match the version 1 `Op` exactly.
    #[derive(Deserialize)]
    pub enum Op {
        CreateTable(Schema),
        DropTable(String),
        CreateIndex { table: String, def: IndexDef },
        DropIndex { table: String, name: String },
        CreateView { name: String, sql: String },
        DropView(String),
        Insert { table: String, id: RowId, row: Row },
        Update { table: String, id: RowId, row: Row },
        Delete { table: String, id: RowId },
        AddColumn { table: String, column: Column },
        DropColumn { table: String, name: String },
        RenameColumn { table: String, from: String, to: String },
        AlterColumnType { table: String, name: String, data_type: DataType },
    }

    impl From<Op> for NewOp {
        fn from(op: Op) -> Self {
            match op {
                Op::CreateTable(s) => NewOp::CreateTable(s.into()),
                Op::DropTable(n) => NewOp::DropTable(n),
                Op::CreateIndex { table, def } => NewOp::CreateIndex { table, def },
                Op::DropIndex { table, name } => NewOp::DropIndex { table, name },
                Op::CreateView { name, sql } => NewOp::CreateView { name, sql },
                Op::DropView(n) => NewOp::DropView(n),
                Op::Insert { table, id, row } => NewOp::Insert { table, id, row },
                Op::Update { table, id, row } => NewOp::Update { table, id, row },
                Op::Delete { table, id } => NewOp::Delete { table, id },
                Op::AddColumn { table, column } => NewOp::AddColumn { table, column },
                Op::DropColumn { table, name } => NewOp::DropColumn { table, name },
                Op::RenameColumn { table, from, to } => NewOp::RenameColumn { table, from, to },
                Op::AlterColumnType { table, name, data_type } => NewOp::AlterColumnType { table, name, data_type },
            }
        }
    }
}

/// Version 2 snapshots: foreign keys, but no triggers.
mod v2 {
    use serde::Deserialize;

    use super::{SnapshotIn as New, TableIn};

    #[derive(Deserialize)]
    pub struct SnapshotIn {
        tables: Vec<TableIn>,
        views: Vec<(String, String)>,
    }

    impl From<SnapshotIn> for New {
        fn from(s: SnapshotIn) -> Self {
            New { tables: s.tables, views: s.views, triggers: Vec::new(), sequences: Vec::new(), fk_timing: Vec::new(), routines: Vec::new(), reports: Vec::new() }
        }
    }
}

/// Version 3 snapshots: no sequences, and no record of deferrable relationships.
mod v3 {
    use serde::Deserialize;

    use super::{SnapshotIn as New, TableIn};

    #[derive(Deserialize)]
    pub struct SnapshotIn {
        tables: Vec<TableIn>,
        views: Vec<(String, String)>,
        triggers: Vec<(String, String)>,
    }

    impl From<SnapshotIn> for New {
        fn from(s: SnapshotIn) -> Self {
            New {
                tables: s.tables,
                views: s.views,
                triggers: s.triggers,
                sequences: Vec::new(),
                fk_timing: Vec::new(),
                routines: Vec::new(),
                reports: Vec::new(),
            }
        }
    }
}

/// Version 4 snapshots: no stored functions or procedures.
mod v4 {
    use serde::Deserialize;

    use super::{SnapshotIn as New, TableIn};
    use crate::sequence::SequenceDef;

    #[derive(Deserialize)]
    pub struct SnapshotIn {
        tables: Vec<TableIn>,
        views: Vec<(String, String)>,
        triggers: Vec<(String, String)>,
        sequences: Vec<SequenceDef>,
        fk_timing: Vec<(String, String, bool, bool)>,
    }

    impl From<SnapshotIn> for New {
        fn from(s: SnapshotIn) -> Self {
            New {
                tables: s.tables,
                views: s.views,
                triggers: s.triggers,
                sequences: s.sequences,
                fk_timing: s.fk_timing,
                routines: Vec::new(),
                reports: Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Column, TableSchema};
    use crate::value::DataType;

    fn snapshot_bytes(version: u32, payload: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(SNAP_MAGIC);
        b.extend_from_slice(&version.to_le_bytes());
        b.extend_from_slice(&7u64.to_le_bytes());
        b.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
        b.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        b.extend_from_slice(payload);
        b
    }

    fn one_table() -> Vec<(TableSchema, Vec<Option<Row>>, i64)> {
        let schema = TableSchema {
            name: "t".into(),
            columns: vec![Column::new("n", DataType::Integer)],
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
        };
        vec![(schema, vec![Some(vec![crate::value::Value::Int(5)])], 1)]
    }

    #[derive(Serialize)]
    struct OldTable<'a> {
        schema: &'a TableSchema,
        rows: &'a [Option<Row>],
        next_auto: i64,
    }

    #[derive(Serialize)]
    struct V3Out<'a> {
        tables: Vec<OldTable<'a>>,
        views: Vec<(&'a str, &'a str)>,
        triggers: Vec<(&'a str, &'a str)>,
    }

    #[derive(Serialize)]
    struct V4Out<'a> {
        tables: Vec<OldTable<'a>>,
        views: Vec<(&'a str, &'a str)>,
        triggers: Vec<(&'a str, &'a str)>,
        sequences: Vec<SequenceDef>,
        fk_timing: Vec<(String, String, bool, bool)>,
    }

    #[test]
    fn snapshots_of_versions_3_and_4_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.adb");
        let tables = one_table();
        let old: Vec<OldTable> = tables.iter().map(|(s, r, n)| OldTable { schema: s, rows: r, next_auto: *n }).collect();
        let v3 = postcard::to_allocvec(&V3Out { tables: old, views: vec![("v", "SELECT 1")], triggers: vec![] }).unwrap();
        std::fs::write(&path, snapshot_bytes(3, &v3)).unwrap();
        let l = read_snapshot(&path).unwrap();
        assert_eq!((l.tables.len(), l.views.len(), l.routines.len(), l.reports.len(), l.epoch, l.legacy), (1, 1, 0, 0, 7, true));

        let old: Vec<OldTable> = tables.iter().map(|(s, r, n)| OldTable { schema: s, rows: r, next_auto: *n }).collect();
        let seq = SequenceDef { name: "s".into(), start: 1, increment: 1, min: 1, max: 9, cycle: false, last: Some(4) };
        let v4 = postcard::to_allocvec(&V4Out {
            tables: old,
            views: vec![],
            triggers: vec![],
            sequences: vec![seq.clone()],
            fk_timing: vec![("t".into(), "fk".into(), true, false)],
        })
        .unwrap();
        std::fs::write(&path, snapshot_bytes(4, &v4)).unwrap();
        let l = read_snapshot(&path).unwrap();
        assert_eq!((l.tables.len(), l.sequences, l.fk_timing.len(), l.routines.len(), l.legacy), (1, vec![seq], 1, 0, true));
    }
}
