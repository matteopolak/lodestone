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

use lodestone_storage_schema::{
    ExtensionTable, RegisteredExtension, StorageRecord, generated::storage_record::Record,
    validate_extension_table, validate_record, validate_record_with_extensions,
};
use prost::Message;

const SEGMENT_NAME: &str = "world.ls";
const COMPACTING_SEGMENT_NAME: &str = "world.ls.compacting";
const PREVIOUS_SEGMENT_NAME: &str = "world.ls.previous";
const EXTENSION_TABLE_NAME: &str = "extensions.ls";
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

/// One committed native chunk coordinate.
///
/// Version-1 native chunk keys contain only the horizontal column pair; the
/// format does not persist a dimension discriminator. Values from
/// [`NativeStore::committed_chunk_coordinates`] are copied from the recovered
/// latest-record index, so reading this type never seeks to or decodes a
/// chunk payload.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct NativeChunkCoordinate {
    /// Chunk X coordinate.
    pub column_x: i32,
    /// Chunk Z coordinate.
    pub column_z: i32,
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

    /// A key for one independently replaceable typed general record.
    ///
    /// General-record producers define their own collision-checked coordinate
    /// and local-ID convention. The key deliberately contains no names: the
    /// protobuf body supplies the typed record discriminator and identity.
    #[must_use]
    pub const fn general(column_x: i32, column_z: i32, local_id: u32) -> Self {
        Self {
            column_x,
            column_z,
            local_id,
            kind: RecordKind::General,
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

/// One named extension schema a native store can register.
///
/// The store assigns compact IDs in a stable order and retains that assignment
/// in its extension-table sidecar. Records then carry only the assigned ID and
/// their payload; neither a chunk nor a general record repeats these strings.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExtensionRegistration {
    /// Extension namespace, such as `example`.
    pub namespace: String,
    /// Schema name within `namespace`.
    pub name: String,
    /// The extension's non-zero payload schema version.
    pub schema_version: u32,
}

impl ExtensionRegistration {
    /// Builds one requested extension schema.
    #[must_use]
    pub fn new(namespace: impl Into<String>, name: impl Into<String>, schema_version: u32) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
            schema_version,
        }
    }
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

/// Facts about one completed segment compaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Compaction {
    /// Latest records retained in the replacement segment.
    pub records: usize,
    /// Bytes in the append segment before replacing obsolete records.
    pub before_bytes: u64,
    /// Bytes in the fully committed replacement segment.
    pub after_bytes: u64,
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
    InvalidExtensionTable(lodestone_storage_schema::ValidationError),
    ExtensionSchemaConflict {
        namespace: String,
        name: String,
        existing_version: u32,
        requested_version: u32,
    },
    ExtensionIdExhausted,
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
            Self::InvalidExtensionTable(error) => {
                write!(formatter, "invalid native extension table: {error}")
            }
            Self::ExtensionSchemaConflict {
                namespace,
                name,
                existing_version,
                requested_version,
            } => write!(
                formatter,
                "extension {namespace}:{name} is already registered at schema version \
                 {existing_version}, not requested version {requested_version}"
            ),
            Self::ExtensionIdExhausted => formatter.write_str("native extension IDs are exhausted"),
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
    extension_table_path: PathBuf,
    extension_table: ExtensionTable,
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
        recover_interrupted_compaction(directory.as_ref(), &path)?;
        let extension_table_path = directory.as_ref().join(EXTENSION_TABLE_NAME);
        let extension_table = read_extension_table(&extension_table_path)?;
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
        let mut store = Self {
            file,
            path,
            extension_table_path,
            extension_table,
            index,
            recovery,
        };
        store.validate_index_extensions()?;
        Ok(store)
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
            validate_record_with_extensions(&write.record, &self.extension_table)
                .map_err(StoreError::InvalidRecord)?;
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
        validate_record_with_extensions(&record, &self.extension_table)
            .map_err(StoreError::InvalidRecord)?;
        Ok(Some(record))
    }

    /// Returns the compact-ID table that resolves extension values in this store.
    #[must_use]
    pub fn extension_table(&self) -> &ExtensionTable {
        &self.extension_table
    }

    /// Registers extension schemas and persists their compact IDs before any
    /// record may reference them.
    ///
    /// Requests are sorted and deduplicated before IDs are assigned, so one
    /// registration batch has one result independent of its caller's ordering.
    /// Existing schemas keep their IDs forever; reusing a name at a different
    /// schema version is refused rather than silently reinterpreting old bytes.
    pub fn register_extensions(
        &mut self,
        registrations: impl IntoIterator<Item = ExtensionRegistration>,
    ) -> Result<Vec<RegisteredExtension>, StoreError> {
        let requested: BTreeSet<_> = registrations.into_iter().collect();
        let mut next = self.extension_table.clone();
        let mut assigned = Vec::with_capacity(requested.len());
        let mut used: BTreeSet<_> = next.extensions.iter().map(|entry| entry.local_id).collect();

        for registration in requested {
            if let Some(existing) = next.extensions.iter().find(|entry| {
                entry.namespace == registration.namespace && entry.name == registration.name
            }) {
                if existing.schema_version != registration.schema_version {
                    return Err(StoreError::ExtensionSchemaConflict {
                        namespace: registration.namespace,
                        name: registration.name,
                        existing_version: existing.schema_version,
                        requested_version: registration.schema_version,
                    });
                }
                assigned.push(existing.clone());
                continue;
            }
            let local_id = first_available_extension_id(&used)?;
            let entry = RegisteredExtension {
                local_id,
                namespace: registration.namespace,
                name: registration.name,
                schema_version: registration.schema_version,
            };
            used.insert(local_id);
            assigned.push(entry.clone());
            next.extensions.push(entry);
        }
        next.extensions.sort_by_key(|entry| entry.local_id);
        validate_extension_table(&next).map_err(StoreError::InvalidExtensionTable)?;
        if next != self.extension_table {
            write_extension_table(&self.extension_table_path, &next)?;
            self.extension_table = next;
        }
        Ok(assigned)
    }

    /// Returns the recovery result captured during [`Self::open`].
    pub const fn recovery(&self) -> Recovery {
        self.recovery
    }

    /// Snapshots every latest committed native chunk key in canonical order.
    ///
    /// The returned vector is sorted by `(column_x, column_z)` because the
    /// latest-record index is a [`BTreeMap`]. It is a point-in-time copy of
    /// that index: later writes cannot alter it. Opening has already applied
    /// the segment's crash-tail recovery before the index exists, so an
    /// incomplete final transaction contributes no coordinates. This method
    /// neither reads record frames nor deserializes chunk payloads.
    #[must_use]
    pub fn committed_chunk_coordinates(&self) -> Vec<NativeChunkCoordinate> {
        self.index
            .keys()
            .filter(|key| key.kind == RecordKind::Chunk)
            .map(|key| NativeChunkCoordinate {
                column_x: key.column_x,
                column_z: key.column_z,
            })
            .collect()
    }

    /// Replaces the append history with one transaction containing every latest record.
    ///
    /// The replacement is first written and synced as `world.ls.compacting`.
    /// The active segment is then renamed to `world.ls.previous` before the
    /// replacement becomes `world.ls`. Opening after an interruption either
    /// keeps the already-published replacement or restores the previous
    /// committed segment; it never treats an uncommitted replacement as data.
    ///
    /// The extension table is unchanged. Callers can use the returned byte
    /// counts to decide whether a maintenance window reclaimed enough space.
    pub fn compact(&mut self) -> Result<Compaction, StoreError> {
        let directory = self
            .path
            .parent()
            .expect("native segment has a parent")
            .to_path_buf();
        recover_interrupted_compaction(&directory, &self.path)?;
        let compacting = directory.join(COMPACTING_SEGMENT_NAME);
        let previous = directory.join(PREVIOUS_SEGMENT_NAME);
        let before_bytes = self.file.metadata()?.len();

        let keys = self.index.keys().copied().collect::<Vec<_>>();
        let (body_len, body_checksum) = self.compacted_body_properties(&keys)?;
        let record_count = u32::try_from(keys.len()).map_err(|_| StoreError::RecordTooLarge)?;
        if record_count == 0 {
            return Ok(Compaction {
                records: 0,
                before_bytes,
                after_bytes: before_bytes,
            });
        }

        let header = encode_transaction_header(
            TRANSACTION_START_MAGIC,
            record_count,
            body_len,
            body_checksum,
        );
        let commit = encode_transaction_header(
            TRANSACTION_COMMIT_MAGIC,
            record_count,
            body_len,
            body_checksum,
        );
        {
            let mut replacement = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&compacting)?;
            replacement.write_all(&header)?;
            for key in &keys {
                let payload = self.read_payload(*key)?;
                write_record_frame(&mut replacement, *key, &payload)?;
            }
            replacement.sync_data()?;
            replacement.write_all(&commit)?;
            replacement.sync_data()?;
        }
        sync_directory(&directory)?;

        fs::rename(&self.path, &previous)?;
        sync_directory(&directory)?;
        fs::rename(&compacting, &self.path)?;
        sync_directory(&directory)?;

        let mut replacement = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&self.path)?;
        let (index, recovery) = scan_segment(&mut replacement)?;
        debug_assert_eq!(recovery.transactions, 1);
        debug_assert_eq!(recovery.records, keys.len());
        let after_bytes = replacement.metadata()?.len();
        self.file = replacement;
        self.index = index;
        self.recovery = recovery;

        fs::remove_file(&previous)?;
        sync_directory(&directory)?;
        Ok(Compaction {
            records: keys.len(),
            before_bytes,
            after_bytes,
        })
    }

    /// Returns this store's segment path for operational tooling and tests.
    pub fn segment_path(&self) -> &Path {
        &self.path
    }

    fn validate_index_extensions(&mut self) -> Result<(), StoreError> {
        for key in self.index.keys().copied().collect::<Vec<_>>() {
            let _ = self.get(key)?;
        }
        Ok(())
    }

    fn compacted_body_properties(
        &mut self,
        keys: &[RecordKey],
    ) -> Result<(u64, u32), StoreError> {
        let mut body_len = 0_u64;
        let mut crc = !0_u32;
        for key in keys {
            let payload = self.read_payload(*key)?;
            let payload_len =
                u32::try_from(payload.len()).map_err(|_| StoreError::RecordTooLarge)?;
            body_len = body_len
                .checked_add(RECORD_HEADER_LEN as u64 + u64::from(payload_len))
                .ok_or(StoreError::RecordTooLarge)?;
            crc = crc32_continue(crc, &key.to_bytes());
            crc = crc32_continue(crc, &payload_len.to_le_bytes());
            crc = crc32_continue(crc, &crc32(&payload).to_le_bytes());
            crc = crc32_continue(crc, &payload);
        }
        Ok((body_len, !crc))
    }

    fn read_payload(&mut self, key: RecordKey) -> Result<Vec<u8>, StoreError> {
        let entry = self.index.get(&key).copied().expect("key came from index");
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
        validate_record_with_extensions(&record, &self.extension_table)
            .map_err(StoreError::InvalidRecord)?;
        Ok(payload)
    }
}

fn recover_interrupted_compaction(directory: &Path, segment: &Path) -> Result<(), StoreError> {
    let compacting = directory.join(COMPACTING_SEGMENT_NAME);
    let previous = directory.join(PREVIOUS_SEGMENT_NAME);
    match (
        segment.try_exists()?,
        compacting.try_exists()?,
        previous.try_exists()?,
    ) {
        (true, false, false) | (false, false, false) => Ok(()),
        // The old active segment still exists, so the replacement was never
        // published and must not influence the recovered state.
        (true, true, false) => {
            fs::remove_file(compacting)?;
            sync_directory(directory)
        }
        // The previous segment is the last known active value until the new
        // name is published. Restoring it deliberately chooses the old atomic
        // state rather than promoting an interrupted maintenance operation.
        (false, true, true) => {
            fs::rename(previous, segment)?;
            fs::remove_file(compacting)?;
            sync_directory(directory)
        }
        // The new segment name is published, so the old segment is only a
        // recovery anchor left behind before cleanup.
        (true, false, true) => {
            fs::remove_file(previous)?;
            sync_directory(directory)
        }
        (false, false, true) => {
            fs::rename(previous, segment)?;
            sync_directory(directory)
        }
        (false, true, false) => Err(StoreError::corrupt(
            0,
            "compaction replacement has no active or previous segment",
        )),
        (true, true, true) => Err(StoreError::corrupt(
            0,
            "compaction has active, replacement, and previous segments",
        )),
    }
}

fn sync_directory(directory: &Path) -> Result<(), StoreError> {
    File::open(directory)?.sync_data()?;
    Ok(())
}

fn write_record_frame(
    file: &mut File,
    key: RecordKey,
    payload: &[u8],
) -> Result<(), StoreError> {
    let payload_len = u32::try_from(payload.len()).map_err(|_| StoreError::RecordTooLarge)?;
    file.write_all(&key.to_bytes())?;
    file.write_all(&payload_len.to_le_bytes())?;
    file.write_all(&crc32(payload).to_le_bytes())?;
    file.write_all(payload)?;
    Ok(())
}

fn first_available_extension_id(used: &BTreeSet<u32>) -> Result<u32, StoreError> {
    (1..=u32::MAX)
        .find(|id| !used.contains(id))
        .ok_or(StoreError::ExtensionIdExhausted)
}

fn read_extension_table(path: &Path) -> Result<ExtensionTable, StoreError> {
    if !path.exists() {
        return Ok(ExtensionTable {
            table_version: 1,
            extensions: Vec::new(),
        });
    }
    let bytes = fs::read(path)?;
    let table = ExtensionTable::decode(bytes.as_slice()).map_err(|error| {
        StoreError::corrupt(0, format!("invalid extension table protobuf: {error}"))
    })?;
    validate_extension_table(&table).map_err(StoreError::InvalidExtensionTable)?;
    Ok(table)
}

fn write_extension_table(path: &Path, table: &ExtensionTable) -> Result<(), StoreError> {
    let temporary = path.with_extension("ls.new");
    {
        let mut file = File::create(&temporary)?;
        file.write_all(&table.encode_to_vec())?;
        file.sync_data()?;
    }
    fs::rename(&temporary, path)?;
    File::open(path.parent().expect("extension table has a parent"))?.sync_data()?;
    Ok(())
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
    !crc32_continue(!0_u32, bytes)
}

fn crc32_continue(mut crc: u32, bytes: &[u8]) -> u32 {
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_storage_schema::{
        ChunkRecord, ChunkSection, ExtensionValue, GeneralRecord, LightData, LightSection,
        WorldProperties, generated::{general_record, light_data, storage_record},
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
                block_scheduled_ticks: vec![],
                extensions: vec![],
                fluid_scheduled_ticks: vec![],
                light_sections: vec![],
            })),
        }
    }

    fn key() -> RecordKey {
        RecordKey::chunk(-12, 34)
    }

    #[test]
    fn typed_light_chunk_survives_transactional_reopen_compactly() {
        let directory = tempdir().unwrap();
        let mut record = chunk(-12, 34, 1);
        let Some(Record::Chunk(body)) = &mut record.record else {
            panic!("test chunk has a chunk body");
        };
        body.light_sections = vec![
            LightSection {
                section_y: -1,
                sky_light: Some(LightData {
                    data: Some(light_data::Data::Uniform(15)),
                }),
                block_light: Some(LightData {
                    data: Some(light_data::Data::Uniform(0)),
                }),
            },
            LightSection {
                section_y: 0,
                sky_light: Some(LightData {
                    data: Some(light_data::Data::Values(
                        (0..2048).map(|index| (index as u8).wrapping_mul(5)).collect(),
                    )),
                }),
                // An absent oneof is the canonical Missing layer.
                block_light: None,
            },
            LightSection {
                section_y: 1,
                sky_light: None,
                block_light: None,
            },
        ];
        let expected_bytes = record.encode_to_vec().len();
        assert!(expected_bytes < 2800, "uniform and missing layers must stay compact");

        {
            let mut store = NativeStore::open(directory.path()).unwrap();
            store
                .write_transaction([RecordWrite::new(key(), record.clone())])
                .unwrap();
        }
        let mut reopened = NativeStore::open(directory.path()).unwrap();
        assert_eq!(reopened.get(key()).unwrap(), Some(record));
        assert_eq!(reopened.recovery().transactions, 1);
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
    fn chunk_enumeration_is_sorted_unique_and_excludes_recovered_tail() {
        let directory = tempdir().unwrap();
        let path;
        {
            let mut store = NativeStore::open(directory.path()).unwrap();
            store
                .write_transaction([
                    RecordWrite::new(RecordKey::chunk(4, -3), chunk(4, -3, 1)),
                    RecordWrite::new(RecordKey::general(0, 0, 7), world_properties(1)),
                    RecordWrite::new(RecordKey::chunk(-2, 9), chunk(-2, 9, 2)),
                    RecordWrite::new(RecordKey::chunk(4, 8), chunk(4, 8, 3)),
                ])
                .unwrap();
            store
                .write_transaction([RecordWrite::new(
                    RecordKey::chunk(4, -3),
                    chunk(4, -3, 4),
                )])
                .unwrap();
            path = store.segment_path().to_owned();
            assert_eq!(
                store.committed_chunk_coordinates(),
                [
                    NativeChunkCoordinate {
                        column_x: -2,
                        column_z: 9,
                    },
                    NativeChunkCoordinate {
                        column_x: 4,
                        column_z: -3,
                    },
                    NativeChunkCoordinate {
                        column_x: 4,
                        column_z: 8,
                    },
                ]
            );
        }

        let body = encode_body_for_test(&[RecordWrite::new(
            RecordKey::chunk(-99, -99),
            chunk(-99, -99, 5),
        )]);
        let header = encode_transaction_header(
            TRANSACTION_START_MAGIC,
            1,
            body.len() as u64,
            crc32(&body),
        );
        let mut interrupted = OpenOptions::new().append(true).open(&path).unwrap();
        interrupted.write_all(&header).unwrap();
        interrupted.write_all(&body).unwrap();
        drop(interrupted);

        let recovered = NativeStore::open(directory.path()).unwrap();
        assert_eq!(
            recovered.committed_chunk_coordinates(),
            [
                NativeChunkCoordinate {
                    column_x: -2,
                    column_z: 9,
                },
                NativeChunkCoordinate {
                    column_x: 4,
                    column_z: -3,
                },
                NativeChunkCoordinate {
                    column_x: 4,
                    column_z: 8,
                },
            ],
            "a recovered uncommitted tail must not become part of enumeration"
        );
    }

    #[test]
    fn duplicate_chunk_write_does_not_change_enumeration() {
        let directory = tempdir().unwrap();
        let mut store = NativeStore::open(directory.path()).unwrap();
        let coordinate = RecordKey::chunk(7, -4);
        store
            .write_transaction([RecordWrite::new(coordinate, chunk(7, -4, 1))])
            .unwrap();
        assert!(matches!(
            store.write_transaction([
                RecordWrite::new(coordinate, chunk(7, -4, 2)),
                RecordWrite::new(coordinate, chunk(7, -4, 3)),
            ]),
            Err(StoreError::DuplicateKey(key)) if key == coordinate
        ));
        assert_eq!(
            store.committed_chunk_coordinates(),
            [NativeChunkCoordinate {
                column_x: 7,
                column_z: -4,
            }]
        );
        assert_eq!(store.get(coordinate).unwrap(), Some(chunk(7, -4, 1)));
    }

    #[test]
    fn interrupted_compaction_restores_the_pre_compaction_commit() {
        let directory = tempdir().unwrap();
        let path;
        {
            let mut store = NativeStore::open(directory.path()).unwrap();
            store
                .write_transaction([RecordWrite::new(key(), chunk(-12, 34, 1))])
                .unwrap();
            path = store.segment_path().to_owned();
        }
        let previous = directory.path().join(PREVIOUS_SEGMENT_NAME);
        let replacement = directory.path().join(COMPACTING_SEGMENT_NAME);
        fs::rename(&path, &previous).unwrap();
        write_committed_segment_for_test(
            &replacement,
            &[RecordWrite::new(key(), chunk(-12, 34, 2))],
        );

        let mut reopened = NativeStore::open(directory.path()).unwrap();
        assert_eq!(
            reopened.get(key()).unwrap(),
            Some(chunk(-12, 34, 1)),
            "an unpublished maintenance replacement must not become a world commit"
        );
        assert!(!previous.exists());
        assert!(!replacement.exists());
    }

    #[test]
    fn compaction_recovery_discards_unpublished_or_superseded_artifacts() {
        let unpublished_directory = tempdir().unwrap();
        {
            let mut store = NativeStore::open(unpublished_directory.path()).unwrap();
            store
                .write_transaction([RecordWrite::new(key(), chunk(-12, 34, 1))])
                .unwrap();
        }
        let replacement = unpublished_directory.path().join(COMPACTING_SEGMENT_NAME);
        write_committed_segment_for_test(
            &replacement,
            &[RecordWrite::new(key(), chunk(-12, 34, 2))],
        );
        let mut reopened = NativeStore::open(unpublished_directory.path()).unwrap();
        assert_eq!(reopened.get(key()).unwrap(), Some(chunk(-12, 34, 1)));
        assert!(
            !replacement.exists(),
            "the old active name proves that this replacement was not published"
        );

        let published_directory = tempdir().unwrap();
        {
            let mut store = NativeStore::open(published_directory.path()).unwrap();
            store
                .write_transaction([RecordWrite::new(key(), chunk(-12, 34, 2))])
                .unwrap();
        }
        let previous = published_directory.path().join(PREVIOUS_SEGMENT_NAME);
        write_committed_segment_for_test(
            &previous,
            &[RecordWrite::new(key(), chunk(-12, 34, 1))],
        );
        let mut reopened = NativeStore::open(published_directory.path()).unwrap();
        assert_eq!(reopened.get(key()).unwrap(), Some(chunk(-12, 34, 2)));
        assert!(
            !previous.exists(),
            "the new active name proves that publication completed before cleanup"
        );
    }

    #[test]
    fn compaction_reclaims_replaced_frames_and_keeps_each_latest_record() {
        let directory = tempdir().unwrap();
        let general_key = RecordKey::general(0, 0, 41);
        let mut store = NativeStore::open(directory.path()).unwrap();
        store
            .write_transaction([
                RecordWrite::new(key(), chunk(-12, 34, 1)),
                RecordWrite::new(general_key, world_properties(10)),
            ])
            .unwrap();
        store
            .write_transaction([RecordWrite::new(key(), chunk(-12, 34, 2))])
            .unwrap();

        let compaction = store.compact().unwrap();
        assert_eq!(compaction.records, 2);
        assert!(
            compaction.after_bytes < compaction.before_bytes,
            "the superseded chunk frame must not remain in the compacted segment"
        );
        assert_eq!(store.get(key()).unwrap(), Some(chunk(-12, 34, 2)));
        assert_eq!(
            store.get(general_key).unwrap(),
            Some(world_properties(10)),
            "compaction must retain a latest record that was not replaced"
        );
        drop(store);

        let mut reopened = NativeStore::open(directory.path()).unwrap();
        assert_eq!(reopened.get(key()).unwrap(), Some(chunk(-12, 34, 2)));
        assert_eq!(reopened.get(general_key).unwrap(), Some(world_properties(10)));
        assert_eq!(
            reopened.recovery(),
            Recovery {
                transactions: 1,
                records: 2,
                discarded_tail_bytes: 0,
            },
            "the replacement has one complete transaction, independent of append history"
        );
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

    #[test]
    fn extension_registration_is_sorted_durable_and_required_by_record_writes() {
        let directory = tempdir().unwrap();
        let mut store = NativeStore::open(directory.path()).unwrap();
        let assigned = store
            .register_extensions([
                ExtensionRegistration::new("example", "weather", 2),
                ExtensionRegistration::new("example", "claims", 1),
            ])
            .unwrap();
        assert_eq!(
            assigned
                .iter()
                .map(|entry| (entry.local_id, entry.name.as_str()))
                .collect::<Vec<_>>(),
            [(1, "claims"), (2, "weather")],
            "one batch must receive the same local IDs regardless of request order"
        );

        let mut rejected = chunk(-12, 34, 1);
        let Some(Record::Chunk(body)) = &mut rejected.record else {
            panic!("test chunk has a chunk body");
        };
        body.extensions = vec![ExtensionValue {
            local_id: 3,
            payload: vec![1, 2, 3],
        }];
        assert!(matches!(
            store.write_transaction([RecordWrite::new(key(), rejected)]),
            Err(StoreError::InvalidRecord(
                lodestone_storage_schema::ValidationError::UnregisteredExtensionId(3)
            ))
        ));

        let mut accepted = chunk(-12, 34, 2);
        let Some(Record::Chunk(body)) = &mut accepted.record else {
            panic!("test chunk has a chunk body");
        };
        body.extensions = vec![ExtensionValue {
            local_id: 1,
            payload: vec![4, 5, 6],
        }];
        store
            .write_transaction([RecordWrite::new(key(), accepted.clone())])
            .unwrap();
        drop(store);

        let mut reopened = NativeStore::open(directory.path()).unwrap();
        assert_eq!(reopened.extension_table().extensions, assigned);
        assert_eq!(reopened.get(key()).unwrap(), Some(accepted));
    }

    #[test]
    fn extension_registration_refuses_a_schema_version_reinterpretation() {
        let directory = tempdir().unwrap();
        let mut store = NativeStore::open(directory.path()).unwrap();
        store
            .register_extensions([ExtensionRegistration::new("example", "claims", 1)])
            .unwrap();
        assert!(matches!(
            store.register_extensions([ExtensionRegistration::new("example", "claims", 2)]),
            Err(StoreError::ExtensionSchemaConflict {
                existing_version: 1,
                requested_version: 2,
                ..
            })
        ));
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

    fn write_committed_segment_for_test(path: &Path, writes: &[RecordWrite]) {
        let body = encode_body_for_test(writes);
        let header = encode_transaction_header(
            TRANSACTION_START_MAGIC,
            writes.len() as u32,
            body.len() as u64,
            crc32(&body),
        );
        let commit = encode_transaction_header(
            TRANSACTION_COMMIT_MAGIC,
            writes.len() as u32,
            body.len() as u64,
            crc32(&body),
        );
        let mut file = File::create(path).unwrap();
        file.write_all(&header).unwrap();
        file.write_all(&body).unwrap();
        file.write_all(&commit).unwrap();
        file.sync_data().unwrap();
    }
}
