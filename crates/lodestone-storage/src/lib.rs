//! Native append/index storage for validated Lodestone world records.
//!
//! A transaction contains one or more independently dirty records. Its final
//! commit marker is the durability boundary: opening ignores and truncates a
//! partial final transaction, but rejects any corruption in a fully committed
//! transaction. The crate is deliberately not yet an integrated-server
//! consumer; it establishes the native on-disk boundary that a dirty-record
//! producer can call later.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use lodestone_storage_schema::{generated::storage_record::Record, validate_record, StorageRecord};
use prost::Message;

const SEGMENT_NAME: &str = "world.ls";
const FORMAT_VERSION: u16 = 1;
const TRANSACTION_START_MAGIC: [u8; 4] = *b"LSTB";
const TRANSACTION_COMMIT_MAGIC: [u8; 4] = *b"LSTC";
const TRANSACTION_HEADER_LEN: usize = 22;
const RECORD_HEADER_LEN: usize = 21;

/// The type of state addressed by a [`RecordKey`].
///
/// The values are persisted and deliberately do not depend on declaration
/// order. A general record's compact local ID distinguishes its typed body
/// (world properties, player, or entity) without putting names in hot keys.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum RecordKind {
    Chunk = 1,
    General = 2,
}

impl TryFrom<u8> for RecordKind {
    type Error = StoreError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Chunk),
            2 => Ok(Self::General),
            other => Err(StoreError::corrupt(
                0,
                format!("unknown record kind {other}"),
            )),
        }
    }
}

/// A fixed-width key for one independently replaceable native record.
///
/// `local_id` is a compact application-assigned identity. Chunk records use
/// zero; general records reserve their coordinate and local-ID conventions for
/// the future dirty-record producer rather than serializing string identifiers.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RecordKey {
    pub column_x: i32,
    pub column_z: i32,
    pub local_id: u32,
    pub kind: RecordKind,
}

impl RecordKey {
    /// The key for a whole-column chunk envelope.
    pub const fn chunk(column_x: i32, column_z: i32) -> Self {
        Self {
            column_x,
            column_z,
            local_id: 0,
            kind: RecordKind::Chunk,
        }
    }

    /// The 13-byte little-endian persistent key representation.
    pub fn to_bytes(self) -> [u8; 13] {
        let mut bytes = [0; 13];
        bytes[..4].copy_from_slice(&self.column_x.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.column_z.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.local_id.to_le_bytes());
        bytes[12] = self.kind as u8;
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, StoreError> {
        debug_assert_eq!(bytes.len(), 13);
        Ok(Self {
            column_x: i32::from_le_bytes(bytes[..4].try_into().expect("fixed key slice")),
            column_z: i32::from_le_bytes(bytes[4..8].try_into().expect("fixed key slice")),
            local_id: u32::from_le_bytes(bytes[8..12].try_into().expect("fixed key slice")),
            kind: RecordKind::try_from(bytes[12])?,
        })
    }
}

/// One record to include in a single atomic append transaction.
#[derive(Clone, Debug)]
pub struct RecordWrite {
    pub key: RecordKey,
    pub record: StorageRecord,
}

impl RecordWrite {
    pub const fn new(key: RecordKey, record: StorageRecord) -> Self {
        Self { key, record }
    }
}

/// Recovery facts observed while rebuilding the latest-record index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Recovery {
    /// Number of fully committed transactions retained in the segment.
    pub transactions: usize,
    /// Number of fully committed records retained in the segment.
    pub records: usize,
    /// Bytes from one incomplete final transaction removed during open.
    pub discarded_tail_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
struct IndexEntry {
    payload_offset: u64,
    payload_len: u32,
    checksum: u32,
}

/// An error that preserves the difference between incomplete crash tails and
/// committed corruption.
#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    InvalidRecord(lodestone_storage_schema::ValidationError),
    EmptyTransaction,
    DuplicateKey(RecordKey),
    RecordTooLarge,
    Corrupt { offset: u64, reason: String },
}

impl StoreError {
    fn corrupt(offset: u64, reason: impl Into<String>) -> Self {
        Self::Corrupt {
            offset,
            reason: reason.into(),
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "storage I/O failed: {error}"),
            Self::InvalidRecord(error) => write!(formatter, "invalid storage record: {error}"),
            Self::EmptyTransaction => formatter.write_str("storage transaction has no records"),
            Self::DuplicateKey(key) => write!(formatter, "storage transaction repeats key {key:?}"),
            Self::RecordTooLarge => formatter.write_str("storage record exceeds u32 length"),
            Self::Corrupt { offset, reason } => {
                write!(
                    formatter,
                    "corrupt storage segment at offset {offset}: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// A native, single-writer append segment plus a rebuilt latest-record index.
///
/// The type is intentionally mutable for reads as well as writes because its
/// one file handle seeks to the indexed payload. A later snapshot/read-sharing
/// layer can own separate read handles without weakening this file format.
#[derive(Debug)]
pub struct NativeStore {
    file: File,
    path: PathBuf,
    index: BTreeMap<RecordKey, IndexEntry>,
    recovery: Recovery,
}

impl NativeStore {
    /// Opens a segment and reconstructs its committed latest-record index.
    ///
    /// A partial final transaction is removed before this method returns. A
    /// malformed transaction whose commit marker is complete is never skipped.
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
            let retained_len = file.metadata()?.len() - recovery.discarded_tail_bytes;
            file.set_len(retained_len)?;
            file.sync_data()?;
        }
        Ok(Self {
            file,
            path,
            index,
            recovery,
        })
    }

    /// Appends and commits one atomic set of validated envelope replacements.
    ///
    /// The in-memory index changes only after the commit marker is written and
    /// synced. A crash before that point leaves an uncommitted tail that
    /// [`Self::open`] removes as one unit.
    pub fn write_transaction(
        &mut self,
        writes: impl IntoIterator<Item = RecordWrite>,
    ) -> Result<(), StoreError> {
        let writes: Vec<_> = writes.into_iter().collect();
        if writes.is_empty() {
            return Err(StoreError::EmptyTransaction);
        }
        let count = u32::try_from(writes.len()).map_err(|_| StoreError::RecordTooLarge)?;
        let mut seen = BTreeSet::new();
        let mut body = Vec::new();
        let mut changes = Vec::with_capacity(writes.len());

        for write in writes {
            validate_record(&write.record).map_err(StoreError::InvalidRecord)?;
            validate_key_kind(write.key, &write.record)?;
            if !seen.insert(write.key) {
                return Err(StoreError::DuplicateKey(write.key));
            }
            let payload = write.record.encode_to_vec();
            let payload_len =
                u32::try_from(payload.len()).map_err(|_| StoreError::RecordTooLarge)?;
            let checksum = crc32(&payload);
            let frame_offset = u64::try_from(body.len()).map_err(|_| StoreError::RecordTooLarge)?;
            body.extend_from_slice(&write.key.to_bytes());
            body.extend_from_slice(&payload_len.to_le_bytes());
            body.extend_from_slice(&checksum.to_le_bytes());
            body.extend_from_slice(&payload);
            changes.push((write.key, frame_offset, payload_len, checksum));
        }

        let body_len = u64::try_from(body.len()).map_err(|_| StoreError::RecordTooLarge)?;
        let body_checksum = crc32(&body);
        let start =
            encode_transaction_header(TRANSACTION_START_MAGIC, count, body_len, body_checksum);
        let commit =
            encode_transaction_header(TRANSACTION_COMMIT_MAGIC, count, body_len, body_checksum);
        let transaction_offset = self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&start)?;
        self.file.write_all(&body)?;
        self.file.sync_data()?;
        self.file.write_all(&commit)?;
        self.file.sync_data()?;

        let body_offset = transaction_offset + TRANSACTION_HEADER_LEN as u64;
        for (key, frame_offset, payload_len, checksum) in changes {
            self.index.insert(
                key,
                IndexEntry {
                    payload_offset: body_offset + frame_offset + RECORD_HEADER_LEN as u64,
                    payload_len,
                    checksum,
                },
            );
        }
        self.recovery.transactions += 1;
        self.recovery.records += count as usize;
        Ok(())
    }

    /// Reads the latest committed envelope for one key.
    pub fn get(&mut self, key: RecordKey) -> Result<Option<StorageRecord>, StoreError> {
        let Some(entry) = self.index.get(&key).copied() else {
            return Ok(None);
        };
        self.file.seek(SeekFrom::Start(entry.payload_offset))?;
        let mut payload = vec![0; entry.payload_len as usize];
        self.file.read_exact(&mut payload)?;
        if crc32(&payload) != entry.checksum {
            return Err(StoreError::corrupt(
                entry.payload_offset,
                "indexed payload checksum mismatch",
            ));
        }
        let record = decode_record(&payload, entry.payload_offset)?;
        validate_key_kind(key, &record)?;
        Ok(Some(record))
    }

    /// Returns the recovery result captured during [`Self::open`].
    pub const fn recovery(&self) -> Recovery {
        self.recovery
    }

    /// Returns this store's segment path for operational tooling and tests.
    pub fn segment_path(&self) -> &Path {
        &self.path
    }
}

fn validate_key_kind(key: RecordKey, record: &StorageRecord) -> Result<(), StoreError> {
    let matches = matches!(
        (key.kind, &record.record),
        (RecordKind::Chunk, Some(Record::Chunk(_)))
            | (RecordKind::General, Some(Record::General(_)))
    );
    if matches {
        Ok(())
    } else {
        Err(StoreError::corrupt(
            0,
            "record key kind does not match protobuf envelope body",
        ))
    }
}

fn scan_segment(
    file: &mut File,
) -> Result<(BTreeMap<RecordKey, IndexEntry>, Recovery), StoreError> {
    let file_len = file.metadata()?.len();
    file.seek(SeekFrom::Start(0))?;
    let mut offset = 0_u64;
    let mut index = BTreeMap::new();
    let mut recovery = Recovery {
        transactions: 0,
        records: 0,
        discarded_tail_bytes: 0,
    };

    while offset < file_len {
        let transaction_offset = offset;
        let remaining = file_len - offset;
        if remaining < TRANSACTION_HEADER_LEN as u64 {
            recovery.discarded_tail_bytes = remaining;
            break;
        }
        let mut start = [0; TRANSACTION_HEADER_LEN];
        file.read_exact(&mut start)?;
        let header =
            decode_transaction_header(&start, transaction_offset, TRANSACTION_START_MAGIC)?;
        let body_offset = transaction_offset + TRANSACTION_HEADER_LEN as u64;
        let after_start = file_len - body_offset;
        if after_start < header.body_len {
            recovery.discarded_tail_bytes = file_len - transaction_offset;
            break;
        }
        let body_len = usize::try_from(header.body_len).map_err(|_| {
            StoreError::corrupt(transaction_offset, "transaction body is too large")
        })?;
        let mut body = vec![0; body_len];
        file.read_exact(&mut body)?;
        let commit_offset = body_offset + header.body_len;
        let after_body = file_len - commit_offset;
        if after_body < TRANSACTION_HEADER_LEN as u64 {
            recovery.discarded_tail_bytes = file_len - transaction_offset;
            break;
        }
        let mut commit = [0; TRANSACTION_HEADER_LEN];
        file.read_exact(&mut commit)?;
        let committed =
            decode_transaction_header(&commit, commit_offset, TRANSACTION_COMMIT_MAGIC)?;
        if committed != header {
            return Err(StoreError::corrupt(
                commit_offset,
                "commit marker does not match transaction header",
            ));
        }
        if crc32(&body) != header.body_checksum {
            return Err(StoreError::corrupt(
                transaction_offset,
                "transaction checksum mismatch",
            ));
        }
        apply_committed_body(&body, body_offset, header.record_count, &mut index)?;
        recovery.transactions += 1;
        recovery.records += header.record_count as usize;
        offset = commit_offset + TRANSACTION_HEADER_LEN as u64;
    }
    Ok((index, recovery))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TransactionHeader {
    record_count: u32,
    body_len: u64,
    body_checksum: u32,
}

fn encode_transaction_header(
    magic: [u8; 4],
    record_count: u32,
    body_len: u64,
    body_checksum: u32,
) -> [u8; TRANSACTION_HEADER_LEN] {
    let mut bytes = [0; TRANSACTION_HEADER_LEN];
    bytes[..4].copy_from_slice(&magic);
    bytes[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes[6..10].copy_from_slice(&record_count.to_le_bytes());
    bytes[10..18].copy_from_slice(&body_len.to_le_bytes());
    bytes[18..22].copy_from_slice(&body_checksum.to_le_bytes());
    bytes
}

fn decode_transaction_header(
    bytes: &[u8; TRANSACTION_HEADER_LEN],
    offset: u64,
    expected_magic: [u8; 4],
) -> Result<TransactionHeader, StoreError> {
    if bytes[..4] != expected_magic {
        return Err(StoreError::corrupt(offset, "unexpected transaction marker"));
    }
    let version = u16::from_le_bytes(bytes[4..6].try_into().expect("fixed header slice"));
    if version != FORMAT_VERSION {
        return Err(StoreError::corrupt(
            offset,
            format!("unsupported storage version {version}"),
        ));
    }
    let record_count = u32::from_le_bytes(bytes[6..10].try_into().expect("fixed header slice"));
    if record_count == 0 {
        return Err(StoreError::corrupt(offset, "transaction has no records"));
    }
    let body_len = u64::from_le_bytes(bytes[10..18].try_into().expect("fixed header slice"));
    let body_checksum = u32::from_le_bytes(bytes[18..22].try_into().expect("fixed header slice"));
    Ok(TransactionHeader {
        record_count,
        body_len,
        body_checksum,
    })
}

fn apply_committed_body(
    body: &[u8],
    body_offset: u64,
    expected_count: u32,
    index: &mut BTreeMap<RecordKey, IndexEntry>,
) -> Result<(), StoreError> {
    let mut cursor = 0_usize;
    let mut keys = BTreeSet::new();
    for _ in 0..expected_count {
        if body.len().saturating_sub(cursor) < RECORD_HEADER_LEN {
            return Err(StoreError::corrupt(
                body_offset + cursor as u64,
                "record frame header is incomplete",
            ));
        }
        let key = RecordKey::from_bytes(&body[cursor..cursor + 13])?;
        let payload_len = u32::from_le_bytes(
            body[cursor + 13..cursor + 17]
                .try_into()
                .expect("fixed frame slice"),
        );
        let checksum = u32::from_le_bytes(
            body[cursor + 17..cursor + 21]
                .try_into()
                .expect("fixed frame slice"),
        );
        let payload_start = cursor + RECORD_HEADER_LEN;
        let payload_end = payload_start
            .checked_add(payload_len as usize)
            .ok_or_else(|| {
                StoreError::corrupt(body_offset + cursor as u64, "record length overflows")
            })?;
        if payload_end > body.len() {
            return Err(StoreError::corrupt(
                body_offset + cursor as u64,
                "record payload is shorter than its frame length",
            ));
        }
        let payload = &body[payload_start..payload_end];
        if crc32(payload) != checksum {
            return Err(StoreError::corrupt(
                body_offset + payload_start as u64,
                "record payload checksum mismatch",
            ));
        }
        let record = decode_record(payload, body_offset + payload_start as u64)?;
        validate_key_kind(key, &record)?;
        if !keys.insert(key) {
            return Err(StoreError::corrupt(
                body_offset + cursor as u64,
                "transaction repeats a record key",
            ));
        }
        index.insert(
            key,
            IndexEntry {
                payload_offset: body_offset + payload_start as u64,
                payload_len,
                checksum,
            },
        );
        cursor = payload_end;
    }
    if cursor != body.len() {
        return Err(StoreError::corrupt(
            body_offset + cursor as u64,
            "transaction body has trailing bytes",
        ));
    }
    Ok(())
}

fn decode_record(payload: &[u8], offset: u64) -> Result<StorageRecord, StoreError> {
    let record = StorageRecord::decode(payload).map_err(|error| {
        StoreError::corrupt(offset, format!("invalid protobuf envelope: {error}"))
    })?;
    validate_record(&record).map_err(|error| {
        StoreError::corrupt(
            offset,
            format!("protobuf envelope violates schema invariants: {error}"),
        )
    })?;
    Ok(record)
}

/// IEEE CRC-32 over exactly the persisted transaction or record bytes.
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
    use lodestone_storage_schema::{
        generated::{general_record, storage_record},
        ChunkRecord, ChunkSection, GeneralRecord, WorldProperties,
    };
    use tempfile::tempdir;

    fn chunk(x: i32, z: i32, state: u32) -> StorageRecord {
        StorageRecord {
            format_version: 1,
            record: Some(storage_record::Record::Chunk(ChunkRecord {
                column_x: x,
                column_z: z,
                game_data_version: 46_002,
                sections: vec![ChunkSection {
                    section_y: 0,
                    palette_bits: 1,
                    palette_state_ids: vec![state],
                    block_state_indices: vec![0; 512],
                    sky_light: vec![],
                    block_light: vec![],
                }],
                biome_sections: vec![],
                surface_biome_ids: vec![],
                motion_blocking_heights: vec![],
                block_entity_nbt: vec![],
                extensions: vec![],
            })),
        }
    }

    fn key() -> RecordKey {
        RecordKey::chunk(-12, 34)
    }

    fn world_properties(seed: i64) -> StorageRecord {
        StorageRecord {
            format_version: 1,
            record: Some(storage_record::Record::General(GeneralRecord {
                record: Some(general_record::Record::WorldProperties(WorldProperties {
                    game_data_version: 46_002,
                    seed,
                    spawn_dimension: 1,
                    spawn_x: 0,
                    spawn_y: 64,
                    spawn_z: 0,
                    day_time: 0,
                    default_game_mode: 1,
                })),
                extensions: vec![],
            })),
        }
    }

    #[test]
    fn committed_batch_is_visible_only_as_one_latest_index_update() {
        let directory = tempdir().unwrap();
        let mut store = NativeStore::open(directory.path()).unwrap();
        store
            .write_transaction([RecordWrite::new(key(), chunk(-12, 34, 1))])
            .unwrap();
        store
            .write_transaction([RecordWrite::new(key(), chunk(-12, 34, 2))])
            .unwrap();
        drop(store);

        let mut reopened = NativeStore::open(directory.path()).unwrap();
        let Some(StorageRecord {
            record: Some(Record::Chunk(chunk)),
            ..
        }) = reopened.get(key()).unwrap()
        else {
            panic!("chunk record should survive reopen");
        };
        assert_eq!(chunk.sections[0].palette_state_ids, [2]);
        assert_eq!(
            reopened.recovery(),
            Recovery {
                transactions: 2,
                records: 2,
                discarded_tail_bytes: 0,
            }
        );
    }

    #[test]
    fn incomplete_commit_discards_the_entire_uncommitted_batch() {
        let directory = tempdir().unwrap();
        let path;
        {
            let mut store = NativeStore::open(directory.path()).unwrap();
            store
                .write_transaction([RecordWrite::new(key(), chunk(-12, 34, 1))])
                .unwrap();
            path = store.segment_path().to_owned();
        }
        let before = path.metadata().unwrap().len();
        let body = encode_body_for_test(&[
            RecordWrite::new(key(), chunk(-12, 34, 2)),
            RecordWrite::new(RecordKey::chunk(-11, 34), chunk(-11, 34, 3)),
        ]);
        let header =
            encode_transaction_header(TRANSACTION_START_MAGIC, 2, body.len() as u64, crc32(&body));
        let mut interrupted = OpenOptions::new().append(true).open(&path).unwrap();
        interrupted.write_all(&header).unwrap();
        interrupted.write_all(&body).unwrap();
        interrupted
            .write_all(&TRANSACTION_COMMIT_MAGIC[..2])
            .unwrap();
        drop(interrupted);

        let mut recovered = NativeStore::open(directory.path()).unwrap();
        assert_eq!(recovered.get(key()).unwrap(), Some(chunk(-12, 34, 1)));
        assert_eq!(recovered.get(RecordKey::chunk(-11, 34)).unwrap(), None);
        assert_eq!(
            recovered.recovery().discarded_tail_bytes,
            22 + body.len() as u64 + 2
        );
        assert_eq!(path.metadata().unwrap().len(), before);
    }

    #[test]
    fn completed_commit_with_corrupt_payload_refuses_to_open() {
        let directory = tempdir().unwrap();
        let path;
        {
            let mut store = NativeStore::open(directory.path()).unwrap();
            store
                .write_transaction([RecordWrite::new(key(), chunk(-12, 34, 1))])
                .unwrap();
            path = store.segment_path().to_owned();
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(
            (TRANSACTION_HEADER_LEN + RECORD_HEADER_LEN) as u64,
        ))
        .unwrap();
        file.write_all(&[0xff]).unwrap();
        drop(file);
        assert!(matches!(
            NativeStore::open(directory.path()),
            Err(StoreError::Corrupt { reason, .. }) if reason.contains("checksum")
        ));
    }

    #[test]
    fn key_kind_must_match_the_envelope_body() {
        let directory = tempdir().unwrap();
        let mut store = NativeStore::open(directory.path()).unwrap();
        let error = store
            .write_transaction([RecordWrite::new(
                RecordKey {
                    kind: RecordKind::General,
                    ..key()
                },
                chunk(-12, 34, 1),
            )])
            .unwrap_err();
        assert!(
            matches!(error, StoreError::Corrupt { reason, .. } if reason.contains("does not match"))
        );
    }

    #[test]
    fn compact_local_id_addresses_a_general_envelope_without_names() {
        let directory = tempdir().unwrap();
        let general_key = RecordKey {
            column_x: 0,
            column_z: 0,
            local_id: 41,
            kind: RecordKind::General,
        };
        let mut store = NativeStore::open(directory.path()).unwrap();
        store
            .write_transaction([RecordWrite::new(general_key, world_properties(123_456))])
            .unwrap();
        assert_eq!(
            store.get(general_key).unwrap(),
            Some(world_properties(123_456))
        );
        assert_eq!(general_key.to_bytes().len(), 13);
    }

    fn encode_body_for_test(writes: &[RecordWrite]) -> Vec<u8> {
        let mut body = Vec::new();
        for write in writes {
            let payload = write.record.encode_to_vec();
            body.extend_from_slice(&write.key.to_bytes());
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(&crc32(&payload).to_le_bytes());
            body.extend_from_slice(&payload);
        }
        body
    }
}
