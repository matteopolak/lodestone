//! A deliberately small storage-engine comparison harness.
//!
//! It models the write pattern a world save needs before any save format is
//! chosen: replace a mixture of chunk-sized and smaller records, recover an
//! interrupted final append, and detect corruption in a completed record. The
//! custom engine is an append-only segment with an in-memory latest-record
//! index. [`RedbStore`] performs the same logical replacements through redb.
//! Neither type is a production save backend.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use redb::ReadableDatabase;

const MAGIC: [u8; 4] = *b"LSRP";
const FORMAT_VERSION: u16 = 1;
const HEADER_LEN: usize = 35;
const SEGMENT_NAME: &str = "segment-0000.lsrp";

/// A type of independently dirty world state.
///
/// The discriminator is persisted, so values are intentionally explicit
/// rather than derived from declaration order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum RecordKind {
    Chunk = 1,
    Entity = 2,
    BlockEntity = 3,
    Player = 4,
    Global = 5,
}

impl TryFrom<u8> for RecordKind {
    type Error = StoreError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Chunk),
            2 => Ok(Self::Entity),
            3 => Ok(Self::BlockEntity),
            4 => Ok(Self::Player),
            5 => Ok(Self::Global),
            other => Err(StoreError::Corrupt(format!(
                "unknown record kind {other}"
            ))),
        }
    }
}

/// The key for one independently replaceable record.
///
/// `id` identifies a sub-record within the same column, such as an entity or
/// block entity. Global and player records reserve `x` and `z` as zero and use
/// `id` for their local numeric identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RecordKey {
    pub x: i32,
    pub z: i32,
    pub id: u32,
    pub kind: RecordKind,
}

impl RecordKey {
    /// Returns the fixed external representation shared by both engines.
    pub fn to_bytes(self) -> [u8; 13] {
        let mut bytes = [0; 13];
        bytes[..4].copy_from_slice(&self.x.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.z.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.id.to_le_bytes());
        bytes[12] = self.kind as u8;
        bytes
    }
}

/// Errors distinguished by recovery policy rather than hidden as I/O errors.
#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Corrupt(String),
    Redb(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "storage I/O failed: {error}"),
            Self::Corrupt(message) => write!(f, "corrupt storage segment: {message}"),
            Self::Redb(message) => write!(f, "redb operation failed: {message}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Number of complete records recovered while opening a custom segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Recovery {
    pub records: usize,
    pub discarded_tail_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
struct IndexEntry {
    payload_offset: u64,
    len: u32,
}

/// A purpose-built append/index segment used only for this comparison.
///
/// The index is reconstructed from the segment on open. A successful `put`
/// writes one whole record, calls `sync_data`, then exposes it through the
/// index. A trailing incomplete header or payload is truncated during open;
/// invalid magic, version, kind, length, or checksum in a *complete* record is
/// corruption and refuses to open the segment.
#[derive(Debug)]
pub struct AppendIndexStore {
    file: File,
    path: PathBuf,
    index: BTreeMap<RecordKey, IndexEntry>,
    recovery: Recovery,
}

impl AppendIndexStore {
    /// Opens (or creates) one segment and rebuilds its latest-record index.
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, StoreError> {
        fs::create_dir_all(directory.as_ref())?;
        let path = directory.as_ref().join(SEGMENT_NAME);
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        let (index, recovery) = scan_segment(&mut file)?;
        if recovery.discarded_tail_bytes != 0 {
            let durable_len = file.metadata()?.len() - recovery.discarded_tail_bytes;
            file.set_len(durable_len)?;
            file.sync_data()?;
        }
        Ok(Self {
            file,
            path,
            index,
            recovery,
        })
    }

    /// Adds a replacement record and makes it durable before indexing it.
    pub fn put(&mut self, key: RecordKey, payload: &[u8]) -> Result<(), StoreError> {
        let len = u32::try_from(payload.len())
            .map_err(|_| StoreError::Corrupt("payload exceeds u32 length".to_owned()))?;
        let header = encode_header(key, len, crc32(payload));
        let record_offset = self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&header)?;
        self.file.write_all(payload)?;
        self.file.sync_data()?;
        self.index.insert(
            key,
            IndexEntry {
                payload_offset: record_offset + HEADER_LEN as u64,
                len,
            },
        );
        self.recovery.records += 1;
        Ok(())
    }

    /// Reads the newest durable value for `key`.
    pub fn get(&mut self, key: RecordKey) -> Result<Option<Vec<u8>>, StoreError> {
        let Some(entry) = self.index.get(&key).copied() else {
            return Ok(None);
        };
        self.file.seek(SeekFrom::Start(entry.payload_offset))?;
        let mut payload = vec![0; entry.len as usize];
        self.file.read_exact(&mut payload)?;
        Ok(Some(payload))
    }

    /// Rewrites only the newest values into a replacement segment.
    ///
    /// This is intentionally a simple compaction control, not a production
    /// transaction or retention design. A future engine must define directory
    /// swap recovery before using this operation for a real world.
    pub fn compact(&mut self) -> Result<(), StoreError> {
        let compacting = self.path.with_extension("compacting");
        let mut replacement = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&compacting)?;
        for key in self.index.keys().copied().collect::<Vec<_>>() {
            let payload = self.get(key)?.expect("key came from index");
            replacement.write_all(&encode_header(key, payload.len() as u32, crc32(&payload)))?;
            replacement.write_all(&payload)?;
        }
        replacement.sync_all()?;
        drop(replacement);
        fs::rename(&compacting, &self.path)?;
        self.file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&self.path)?;
        let (index, recovery) = scan_segment(&mut self.file)?;
        self.index = index;
        self.recovery = recovery;
        Ok(())
    }

    pub fn recovery(&self) -> Recovery {
        self.recovery
    }

    pub fn segment_path(&self) -> &Path {
        &self.path
    }
}

/// A redb implementation of the same key/value replacement interface.
#[derive(Debug)]
pub struct RedbStore {
    database: redb::Database,
}

const RECORDS: redb::TableDefinition<&[u8], &[u8]> =
    redb::TableDefinition::new("records");

impl RedbStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let database = redb::Database::create(path).map_err(redb_error)?;
        Ok(Self { database })
    }

    pub fn put(&self, key: RecordKey, payload: &[u8]) -> Result<(), StoreError> {
        let write = self.database.begin_write().map_err(redb_error)?;
        {
            let mut table = write.open_table(RECORDS).map_err(redb_error)?;
            table.insert(key.to_bytes().as_slice(), payload)
                .map_err(redb_error)?;
        }
        write.commit().map_err(redb_error)
    }

    pub fn get(&self, key: RecordKey) -> Result<Option<Vec<u8>>, StoreError> {
        let read = self.database.begin_read().map_err(redb_error)?;
        let table = read.open_table(RECORDS).map_err(redb_error)?;
        let value = table.get(key.to_bytes().as_slice()).map_err(redb_error)?;
        Ok(value.map(|value| value.value().to_vec()))
    }
}

fn redb_error(error: impl fmt::Display) -> StoreError {
    StoreError::Redb(error.to_string())
}

fn encode_header(key: RecordKey, payload_len: u32, checksum: u32) -> [u8; HEADER_LEN] {
    let mut header = [0; HEADER_LEN];
    header[..4].copy_from_slice(&MAGIC);
    header[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    header[6] = key.kind as u8;
    header[7..11].copy_from_slice(&key.x.to_le_bytes());
    header[11..15].copy_from_slice(&key.z.to_le_bytes());
    header[15..19].copy_from_slice(&key.id.to_le_bytes());
    header[19..27].copy_from_slice(&0_u64.to_le_bytes());
    header[27..31].copy_from_slice(&payload_len.to_le_bytes());
    header[31..35].copy_from_slice(&checksum.to_le_bytes());
    header
}

fn scan_segment(
    file: &mut File,
) -> Result<(BTreeMap<RecordKey, IndexEntry>, Recovery), StoreError> {
    let file_len = file.metadata()?.len();
    file.seek(SeekFrom::Start(0))?;
    let mut offset = 0_u64;
    let mut index = BTreeMap::new();
    let mut records = 0;

    while offset < file_len {
        let remaining = file_len - offset;
        if remaining < HEADER_LEN as u64 {
            return Ok((
                index,
                Recovery {
                    records,
                    discarded_tail_bytes: remaining,
                },
            ));
        }

        let mut header = [0; HEADER_LEN];
        file.read_exact(&mut header)?;
        let (key, len, checksum) = decode_header(&header, offset)?;
        let payload_offset = offset + HEADER_LEN as u64;
        let remaining_payload = file_len - payload_offset;
        if remaining_payload < len as u64 {
            return Ok((
                index,
                Recovery {
                    records,
                    discarded_tail_bytes: HEADER_LEN as u64 + remaining_payload,
                },
            ));
        }

        let mut payload = vec![0; len as usize];
        file.read_exact(&mut payload)?;
        if crc32(&payload) != checksum {
            return Err(StoreError::Corrupt(format!(
                "checksum mismatch at offset {offset}"
            )));
        }
        index.insert(
            key,
            IndexEntry {
                payload_offset,
                len,
            },
        );
        offset = payload_offset + len as u64;
        records += 1;
    }

    Ok((
        index,
        Recovery {
            records,
            discarded_tail_bytes: 0,
        },
    ))
}

fn decode_header(
    header: &[u8; HEADER_LEN],
    offset: u64,
) -> Result<(RecordKey, u32, u32), StoreError> {
    if header[..4] != MAGIC {
        return Err(StoreError::Corrupt(format!("bad magic at offset {offset}")));
    }
    let version = u16::from_le_bytes(header[4..6].try_into().expect("fixed header slice"));
    if version != FORMAT_VERSION {
        return Err(StoreError::Corrupt(format!(
            "unsupported version {version} at offset {offset}"
        )));
    }
    let kind = RecordKind::try_from(header[6])?;
    let x = i32::from_le_bytes(header[7..11].try_into().expect("fixed header slice"));
    let z = i32::from_le_bytes(header[11..15].try_into().expect("fixed header slice"));
    let id = u32::from_le_bytes(header[15..19].try_into().expect("fixed header slice"));
    let generation = u64::from_le_bytes(header[19..27].try_into().expect("fixed header slice"));
    if generation != 0 {
        return Err(StoreError::Corrupt(format!(
            "reserved generation is {generation} at offset {offset}"
        )));
    }
    let len = u32::from_le_bytes(header[27..31].try_into().expect("fixed header slice"));
    let checksum = u32::from_le_bytes(header[31..35].try_into().expect("fixed header slice"));
    Ok((RecordKey { x, z, id, kind }, len, checksum))
}

/// IEEE CRC-32, kept local so the prototype's external layout has no hidden
/// algorithm dependency.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use tempfile::tempdir;

    fn chunk_key() -> RecordKey {
        RecordKey {
            x: -12,
            z: 34,
            id: 0,
            kind: RecordKind::Chunk,
        }
    }

    #[test]
    fn newest_incremental_record_survives_reopen() {
        let directory = tempdir().unwrap();
        let key = chunk_key();
        {
            let mut store = AppendIndexStore::open(directory.path()).unwrap();
            store.put(key, b"first terrain snapshot").unwrap();
            store.put(key, b"one dirty-section replacement").unwrap();
            store
                .put(
                    RecordKey {
                        x: -12,
                        z: 34,
                        id: 99,
                        kind: RecordKind::BlockEntity,
                    },
                    b"block entity",
                )
                .unwrap();
        }
        let mut reopened = AppendIndexStore::open(directory.path()).unwrap();
        assert_eq!(
            reopened.get(key).unwrap(),
            Some(b"one dirty-section replacement".to_vec())
        );
        assert_eq!(reopened.recovery().records, 3);
    }

    #[test]
    fn incomplete_tail_is_discarded_but_completed_corruption_is_rejected() {
        let directory = tempdir().unwrap();
        let path;
        {
            let mut store = AppendIndexStore::open(directory.path()).unwrap();
            store.put(chunk_key(), b"durable payload").unwrap();
            path = store.segment_path().to_owned();
        }
        let mut tail = OpenOptions::new().append(true).open(&path).unwrap();
        tail.write_all(&encode_header(chunk_key(), 32, 0)).unwrap();
        tail.write_all(b"interrupted").unwrap();
        drop(tail);
        let mut recovered = AppendIndexStore::open(directory.path()).unwrap();
        assert_eq!(recovered.recovery().discarded_tail_bytes, 46);
        let entity = RecordKey {
            x: -12,
            z: 34,
            id: 3,
            kind: RecordKind::Entity,
        };
        recovered.put(entity, b"post-recovery append").unwrap();
        drop(recovered);

        let mut reopened = AppendIndexStore::open(directory.path()).unwrap();
        assert_eq!(
            reopened.get(entity).unwrap(),
            Some(b"post-recovery append".to_vec())
        );
        drop(reopened);

        let mut corrupt = OpenOptions::new().write(true).open(&path).unwrap();
        corrupt.seek(SeekFrom::Start(HEADER_LEN as u64)).unwrap();
        corrupt.write_all(b"X").unwrap();
        drop(corrupt);
        assert!(matches!(
            AppendIndexStore::open(directory.path()),
            Err(StoreError::Corrupt(message)) if message.contains("checksum mismatch")
        ));
    }

    #[test]
    fn compaction_preserves_only_latest_values() {
        let directory = tempdir().unwrap();
        let key = chunk_key();
        let mut store = AppendIndexStore::open(directory.path()).unwrap();
        store.put(key, b"old").unwrap();
        store.put(key, b"new").unwrap();
        let before = store.segment_path().metadata().unwrap().len();
        store.compact().unwrap();
        assert_eq!(store.get(key).unwrap(), Some(b"new".to_vec()));
        assert!(store.segment_path().metadata().unwrap().len() < before);
    }

    #[test]
    fn redb_has_the_same_latest_value_semantics() {
        let directory = tempdir().unwrap();
        let key = chunk_key();
        let store = RedbStore::open(directory.path().join("comparison.redb")).unwrap();
        store.put(key, b"old").unwrap();
        store.put(key, b"new").unwrap();
        assert_eq!(store.get(key).unwrap(), Some(b"new".to_vec()));
    }
}
