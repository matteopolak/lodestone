//! Storage-neutral durable records for plugin-owned data.
//!
//! [`PluginDataStore`] is the native plugin side of the persistent-data
//! contract. It deliberately does not open a world directory or choose an
//! Anvil/Lodestone backend. A backend snapshots the store before unloading a
//! scope, writes the returned records in its own transaction, and restores
//! them with [`PluginDataStore::restore_entries`] on the next open. Keeping
//! that boundary here means the record shape is shared without making the
//! plugin support crate depend on the server or either storage engine.
//!
//! Every entry is an opaque byte blob with a plugin-owned schema version. The
//! store never decodes the blob while snapshotting, so fields added by a newer
//! plugin are not silently discarded by an older server. Typed convenience
//! methods use JSON only at the plugin boundary; a backend can carry the
//! resulting bytes unchanged. Entity scopes include both a stable UUID and a
//! lifecycle generation. A runtime entity id by itself is not a durable key:
//! it can be reused after despawn and would expose stale data to a new entity.
//!
//! The optional [`PluginDataStore::load_snapshot_file`] and
//! [`PluginDataStore::save_snapshot_file`] helpers provide an atomic
//! file-backed adapter for embedders that do not already have a world-save
//! transaction. The server still owns its world persistence integration.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bevy_ecs::resource::Resource;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The record format understood by this module.
pub const PLUGIN_DATA_FORMAT_VERSION: u32 = 1;

/// Maximum encoded size of one plugin-owned blob.
pub const MAX_PLUGIN_DATA_BLOB_BYTES: usize = 1024 * 1024;

/// Maximum UTF-8 byte length of either a plugin namespace or a bare key.
pub const MAX_PLUGIN_DATA_KEY_BYTES: usize = 256;

/// Maximum number of records accepted in one restore operation.
pub const MAX_PLUGIN_DATA_RECORDS: usize = 16_384;

/// Maximum encoded size of the optional JSON snapshot helper.
pub const MAX_PLUGIN_DATA_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;

/// The sidecar name shared by native world owners that use the file adapter.
///
/// The file is deliberately independent of the terrain format: both an Anvil
/// world and a Lodestone-native world can put it beside their own stores
/// without teaching either terrain codec how to decode plugin-owned bytes.
pub const PLUGIN_DATA_SNAPSHOT_FILE: &str = "lodestone-plugin-data.json";

static NEXT_SNAPSHOT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Returns the conventional plugin-data sidecar path for one world directory.
#[must_use]
pub fn plugin_data_snapshot_path(world_directory: impl AsRef<Path>) -> PathBuf {
    world_directory.as_ref().join(PLUGIN_DATA_SNAPSHOT_FILE)
}

/// The scope to which one plugin-owned record belongs.
///
/// The byte-array identities are UUID bytes, kept as arrays here so this
/// storage-neutral crate does not impose a UUID serialization format on an
/// Anvil or Lodestone adapter. `Entity::generation` must advance whenever a
/// runtime entity identity is reused.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum DataScope {
    /// Data that belongs to the plugin itself, independent of a world.
    Plugin,
    /// Data belonging to one stable world identity.
    World { id: [u8; 16] },
    /// Data belonging to one player profile identity.
    Player { id: [u8; 16] },
    /// Data belonging to one entity identity and one lifecycle generation.
    Entity { id: [u8; 16], generation: u64 },
}

/// A validated plugin namespace, scope and bare key.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PluginDataKey {
    plugin: String,
    scope: DataScope,
    key: String,
}

impl PluginDataKey {
    /// Creates a key for `plugin` and `key` in `scope`.
    ///
    /// Names and keys are intentionally path-safe: empty values, control
    /// characters, separators and `:` are rejected. The colon belongs to the
    /// external `"plugin:key"` spelling and must not be smuggled into either
    /// component.
    pub fn new(
        plugin: impl Into<String>,
        scope: DataScope,
        key: impl Into<String>,
    ) -> Result<Self, PluginDataKeyError> {
        let candidate = Self {
            plugin: plugin.into(),
            scope,
            key: key.into(),
        };
        candidate.validate()?;
        Ok(candidate)
    }

    /// Returns the plugin namespace.
    #[must_use]
    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    /// Returns the record scope.
    #[must_use]
    pub fn scope(&self) -> &DataScope {
        &self.scope
    }

    /// Returns the bare key, without the plugin namespace.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Validates a key after deserialization from a backend.
    pub fn validate(&self) -> Result<(), PluginDataKeyError> {
        validate_component("plugin", &self.plugin)?;
        validate_component("key", &self.key)
    }
}

fn validate_component(component: &'static str, value: &str) -> Result<(), PluginDataKeyError> {
    if value.is_empty() {
        return Err(PluginDataKeyError::Empty { component });
    }
    if value.len() > MAX_PLUGIN_DATA_KEY_BYTES {
        return Err(PluginDataKeyError::TooLong {
            component,
            actual: value.len(),
            max: MAX_PLUGIN_DATA_KEY_BYTES,
        });
    }
    if let Some(character) = value
        .chars()
        .find(|character| character.is_control() || matches!(character, '/' | '\\' | ':'))
    {
        return Err(PluginDataKeyError::InvalidCharacter {
            component,
            character,
        });
    }
    Ok(())
}

/// Why a plugin namespace or bare key was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginDataKeyError {
    /// The component was empty.
    Empty { component: &'static str },
    /// The component exceeded [`MAX_PLUGIN_DATA_KEY_BYTES`].
    TooLong {
        component: &'static str,
        actual: usize,
        max: usize,
    },
    /// The component contained a control character or path/namespace separator.
    InvalidCharacter {
        component: &'static str,
        character: char,
    },
}

impl fmt::Display for PluginDataKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { component } => write!(formatter, "{component} must not be empty"),
            Self::TooLong {
                component,
                actual,
                max,
            } => write!(formatter, "{component} is {actual} bytes, maximum is {max}"),
            Self::InvalidCharacter {
                component,
                character,
            } => write!(
                formatter,
                "{component} contains invalid character {character:?}"
            ),
        }
    }
}

impl Error for PluginDataKeyError {}

/// One plugin-owned, versioned opaque blob.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginDataRecord {
    format_version: u32,
    schema_version: u32,
    blob: Vec<u8>,
}

impl PluginDataRecord {
    /// Builds a record after enforcing the format, schema and blob limits.
    pub fn new(schema_version: u32, blob: impl AsRef<[u8]>) -> Result<Self, PluginDataError> {
        if schema_version == 0 {
            return Err(PluginDataError::InvalidSchemaVersion);
        }
        let blob = blob.as_ref();
        if blob.len() > MAX_PLUGIN_DATA_BLOB_BYTES {
            return Err(PluginDataError::BlobTooLarge {
                actual: blob.len(),
                max: MAX_PLUGIN_DATA_BLOB_BYTES,
            });
        }
        Ok(Self {
            format_version: PLUGIN_DATA_FORMAT_VERSION,
            schema_version,
            blob: blob.to_vec(),
        })
    }

    /// The storage-record format version.
    #[must_use]
    pub fn format_version(&self) -> u32 {
        self.format_version
    }

    /// The plugin-owned schema version for this blob.
    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// The opaque bytes, without copying them.
    #[must_use]
    pub fn blob(&self) -> &[u8] {
        &self.blob
    }

    fn validate(&self) -> Result<(), PluginDataError> {
        if self.format_version != PLUGIN_DATA_FORMAT_VERSION {
            return Err(PluginDataError::UnsupportedFormatVersion(
                self.format_version,
            ));
        }
        if self.schema_version == 0 {
            return Err(PluginDataError::InvalidSchemaVersion);
        }
        if self.blob.len() > MAX_PLUGIN_DATA_BLOB_BYTES {
            return Err(PluginDataError::BlobTooLarge {
                actual: self.blob.len(),
                max: MAX_PLUGIN_DATA_BLOB_BYTES,
            });
        }
        Ok(())
    }
}

/// A key and its record, suitable for a backend snapshot or restore.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginDataEntry {
    key: PluginDataKey,
    record: PluginDataRecord,
}

impl PluginDataEntry {
    /// Creates one backend-facing entry.
    pub fn new(key: PluginDataKey, record: PluginDataRecord) -> Result<Self, PluginDataError> {
        key.validate()?;
        record.validate()?;
        Ok(Self { key, record })
    }

    /// Returns the entry key.
    #[must_use]
    pub fn key(&self) -> &PluginDataKey {
        &self.key
    }

    /// Returns the opaque record.
    #[must_use]
    pub fn record(&self) -> &PluginDataRecord {
        &self.record
    }
}

/// Native plugin-owned records indexed by validated namespaced keys.
///
/// This is a [`Resource`] so a native plugin can install it in an ECS app,
/// but it does not install any systems. The owner of the world-save lifecycle
/// calls [`snapshot`](Self::snapshot) before writing and
/// [`unload_scope`](Self::unload_scope) after a scope is no longer resident.
#[derive(Clone, Debug, Default, Resource)]
pub struct PluginDataStore {
    records: BTreeMap<PluginDataKey, PluginDataRecord>,
}

impl PluginDataStore {
    /// Stores an opaque blob, replacing the previous value only after the new
    /// key and blob pass validation.
    pub fn set_blob(
        &mut self,
        key: PluginDataKey,
        schema_version: u32,
        blob: impl AsRef<[u8]>,
    ) -> Result<(), PluginDataError> {
        key.validate()?;
        let record = PluginDataRecord::new(schema_version, blob)?;
        self.records.insert(key, record);
        Ok(())
    }

    /// Serializes a typed value with JSON and stores the resulting bytes as an
    /// opaque record. The backend still carries those bytes without decoding.
    pub fn set<T: Serialize>(
        &mut self,
        key: PluginDataKey,
        schema_version: u32,
        value: &T,
    ) -> Result<(), PluginDataError> {
        let blob = serde_json::to_vec(value)
            .map_err(|error| PluginDataError::Serialization(error.to_string()))?;
        self.set_blob(key, schema_version, blob)
    }

    /// Returns an opaque blob, if the key is present.
    #[must_use]
    pub fn get_blob(&self, key: &PluginDataKey) -> Option<&[u8]> {
        self.records.get(key).map(PluginDataRecord::blob)
    }

    /// Decodes a typed value stored by [`Self::set`].
    pub fn get<T: DeserializeOwned>(
        &self,
        key: &PluginDataKey,
    ) -> Result<Option<T>, PluginDataError> {
        self.records
            .get(key)
            .map(|record| {
                serde_json::from_slice(record.blob())
                    .map_err(|error| PluginDataError::Serialization(error.to_string()))
            })
            .transpose()
    }

    /// Returns the record metadata, if the key is present.
    #[must_use]
    pub fn record(&self, key: &PluginDataKey) -> Option<&PluginDataRecord> {
        self.records.get(key)
    }

    /// Removes one key and returns its record, making deletion explicit to the
    /// caller that will write the next backend snapshot.
    pub fn remove(&mut self, key: &PluginDataKey) -> Option<PluginDataRecord> {
        self.records.remove(key)
    }

    /// Number of records currently resident in memory.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether no records are resident in memory.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Returns a deterministic, owned snapshot for a backend transaction.
    #[must_use]
    pub fn snapshot(&self) -> Vec<PluginDataEntry> {
        self.records
            .iter()
            .map(|(key, record)| PluginDataEntry {
                key: key.clone(),
                record: record.clone(),
            })
            .collect()
    }

    /// Returns a deterministic, owned snapshot for one scope.
    ///
    /// Scope snapshots let a world owner persist one lifecycle boundary
    /// without exposing unrelated plugin, player or entity records to that
    /// backend transaction.
    #[must_use]
    pub fn snapshot_scope(&self, scope: &DataScope) -> Vec<PluginDataEntry> {
        self.records
            .iter()
            .filter(|(key, _)| key.scope() == scope)
            .map(|(key, record)| PluginDataEntry {
                key: key.clone(),
                record: record.clone(),
            })
            .collect()
    }

    /// Restores a store from backend-owned entries, rejecting malformed,
    /// oversized or duplicate records before returning a value.
    pub fn restore_entries(
        entries: impl IntoIterator<Item = PluginDataEntry>,
    ) -> Result<Self, PluginDataError> {
        let mut records = BTreeMap::new();
        for entry in entries {
            if records.len() == MAX_PLUGIN_DATA_RECORDS {
                return Err(PluginDataError::TooManyRecords {
                    max: MAX_PLUGIN_DATA_RECORDS,
                });
            }
            entry.key.validate()?;
            entry.record.validate()?;
            if records.insert(entry.key, entry.record).is_some() {
                // The duplicate was only inserted after validation. Replacing
                // it would make backend ordering decide which plugin value
                // wins, so fail closed instead.
                return Err(PluginDataError::DuplicateKey);
            }
        }
        Ok(Self { records })
    }

    /// Encodes a deterministic JSON snapshot for small native integration
    /// tests and simple plugin-owned files. World backends may use
    /// [`Self::snapshot`] instead and keep the same entries in their own
    /// format.
    pub fn to_snapshot_bytes(&self) -> Result<Vec<u8>, PluginDataError> {
        let snapshot = Snapshot {
            format_version: PLUGIN_DATA_FORMAT_VERSION,
            entries: self.snapshot(),
        };
        let bytes = serde_json::to_vec(&snapshot)
            .map_err(|error| PluginDataError::Serialization(error.to_string()))?;
        if bytes.len() > MAX_PLUGIN_DATA_SNAPSHOT_BYTES {
            return Err(PluginDataError::SnapshotTooLarge {
                actual: bytes.len(),
                max: MAX_PLUGIN_DATA_SNAPSHOT_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decodes and validates a JSON snapshot produced by
    /// [`Self::to_snapshot_bytes`].
    pub fn from_snapshot_bytes(bytes: &[u8]) -> Result<Self, PluginDataError> {
        if bytes.len() > MAX_PLUGIN_DATA_SNAPSHOT_BYTES {
            return Err(PluginDataError::SnapshotTooLarge {
                actual: bytes.len(),
                max: MAX_PLUGIN_DATA_SNAPSHOT_BYTES,
            });
        }
        let snapshot: Snapshot = serde_json::from_slice(bytes)
            .map_err(|error| PluginDataError::Serialization(error.to_string()))?;
        if snapshot.format_version != PLUGIN_DATA_FORMAT_VERSION {
            return Err(PluginDataError::UnsupportedFormatVersion(
                snapshot.format_version,
            ));
        }
        Self::restore_entries(snapshot.entries)
    }

    /// Loads a JSON snapshot from `path`, treating a missing file as an empty
    /// store. Existing files must be valid snapshots; malformed, oversized or
    /// future-format data is returned as an error rather than silently
    /// starting with an empty store.
    pub fn load_snapshot_file(path: impl AsRef<Path>) -> Result<Self, PluginDataError> {
        match fs::read(path.as_ref()) {
            Ok(bytes) => Self::from_snapshot_bytes(&bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(PluginDataError::Io(error.to_string())),
        }
    }

    /// Atomically replaces `path` with this store's JSON snapshot.
    ///
    /// The temporary file is created beside the destination with exclusive
    /// creation, written and synced before rename. A failed write leaves the
    /// previous destination untouched; the caller's backend still owns the
    /// surrounding world-save transaction and any directory policy.
    pub fn save_snapshot_file(&self, path: impl AsRef<Path>) -> Result<(), PluginDataError> {
        let path = path.as_ref();
        let bytes = self.to_snapshot_bytes()?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|error| PluginDataError::Io(error.to_string()))?;

        let temp_path = loop {
            let sequence = NEXT_SNAPSHOT_TEMP.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(
                ".{}.tmp-{}-{}",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("plugin-data"),
                std::process::id(),
                sequence,
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
                        let _ = fs::remove_file(&candidate);
                        return Err(PluginDataError::Io(error.to_string()));
                    }
                    drop(file);
                    break candidate;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(PluginDataError::Io(error.to_string())),
            }
        };

        if let Err(error) = fs::rename(&temp_path, path) {
            let _ = fs::remove_file(&temp_path);
            return Err(PluginDataError::Io(error.to_string()));
        }
        // The rename makes the new snapshot visible; syncing its directory
        // makes that name replacement durable across a crash. This is the
        // same two-step boundary used by the WASM host's confined writer.
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| PluginDataError::Io(error.to_string()))?;
        Ok(())
    }

    /// Applies a plugin-owned schema migration to one record.
    ///
    /// The callback receives the current schema version and opaque bytes. It
    /// returns the new bytes; this method writes them only after the target
    /// version and size checks pass. Downgrades are refused. `false` means the
    /// key was absent or already at `target_schema_version`.
    pub fn migrate<F>(
        &mut self,
        key: &PluginDataKey,
        target_schema_version: u32,
        migration: F,
    ) -> Result<bool, PluginDataError>
    where
        F: FnOnce(u32, &[u8]) -> Result<Vec<u8>, PluginDataError>,
    {
        key.validate()?;
        if target_schema_version == 0 {
            return Err(PluginDataError::InvalidSchemaVersion);
        }
        let Some(current) = self.records.get(key).cloned() else {
            return Ok(false);
        };
        if current.schema_version() == target_schema_version {
            return Ok(false);
        }
        if current.schema_version() > target_schema_version {
            return Err(PluginDataError::MigrationDowngrade {
                current: current.schema_version(),
                target: target_schema_version,
            });
        }
        let blob = migration(current.schema_version(), current.blob())?;
        let record = PluginDataRecord::new(target_schema_version, blob)?;
        self.records.insert(key.clone(), record);
        Ok(true)
    }

    /// Removes all records for one scope and returns them in deterministic key
    /// order. The caller should persist this returned vector before dropping
    /// it; unloading memory is not deletion from the backend.
    pub fn unload_scope(&mut self, scope: &DataScope) -> Vec<PluginDataEntry> {
        let keys: Vec<_> = self
            .records
            .keys()
            .filter(|key| key.scope() == scope)
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|key| {
                self.records
                    .remove(&key)
                    .map(|record| PluginDataEntry { key, record })
            })
            .collect()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    format_version: u32,
    entries: Vec<PluginDataEntry>,
}

/// Errors from key validation, record limits, snapshots and migrations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginDataError {
    /// A namespace/key component failed validation.
    InvalidKey(PluginDataKeyError),
    /// A typed JSON conversion failed.
    Serialization(String),
    /// A schema version of zero is not meaningful.
    InvalidSchemaVersion,
    /// A blob exceeded [`MAX_PLUGIN_DATA_BLOB_BYTES`].
    BlobTooLarge { actual: usize, max: usize },
    /// A snapshot exceeded [`MAX_PLUGIN_DATA_SNAPSHOT_BYTES`].
    SnapshotTooLarge { actual: usize, max: usize },
    /// A restore had too many records.
    TooManyRecords { max: usize },
    /// A restore contained the same validated key more than once.
    DuplicateKey,
    /// The record or snapshot was written by a future format.
    UnsupportedFormatVersion(u32),
    /// A plugin attempted to migrate backwards.
    MigrationDowngrade { current: u32, target: u32 },
    /// The snapshot file could not be read or atomically replaced.
    Io(String),
}

impl From<PluginDataKeyError> for PluginDataError {
    fn from(error: PluginDataKeyError) -> Self {
        Self::InvalidKey(error)
    }
}

impl fmt::Display for PluginDataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey(error) => write!(formatter, "invalid plugin data key: {error}"),
            Self::Serialization(error) => {
                write!(formatter, "plugin data serialization failed: {error}")
            }
            Self::InvalidSchemaVersion => {
                write!(formatter, "plugin data schema version must be non-zero")
            }
            Self::BlobTooLarge { actual, max } => {
                write!(
                    formatter,
                    "plugin data blob is {actual} bytes, maximum is {max}"
                )
            }
            Self::SnapshotTooLarge { actual, max } => {
                write!(
                    formatter,
                    "plugin data snapshot is {actual} bytes, maximum is {max}"
                )
            }
            Self::TooManyRecords { max } => {
                write!(
                    formatter,
                    "plugin data restore exceeds the {max}-record limit"
                )
            }
            Self::DuplicateKey => write!(formatter, "plugin data restore contains a duplicate key"),
            Self::UnsupportedFormatVersion(version) => {
                write!(
                    formatter,
                    "unsupported plugin data format version {version}"
                )
            }
            Self::MigrationDowngrade { current, target } => write!(
                formatter,
                "plugin data migration cannot downgrade schema {current} to {target}"
            ),
            Self::Io(error) => write!(formatter, "plugin data snapshot I/O failed: {error}"),
        }
    }
}

impl Error for PluginDataError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(scope: DataScope, name: &str) -> PluginDataKey {
        PluginDataKey::new("claims", scope, name).expect("valid key")
    }

    #[test]
    fn all_scopes_round_trip_as_typed_opaque_records() {
        let scopes = [
            DataScope::Plugin,
            DataScope::World { id: [1; 16] },
            DataScope::Player { id: [2; 16] },
            DataScope::Entity {
                id: [3; 16],
                generation: 9,
            },
        ];
        let mut store = PluginDataStore::default();
        for (index, scope) in scopes.iter().cloned().enumerate() {
            store
                .set(key(scope, "value"), 1, &(index as u32))
                .expect("store value");
        }

        let restored = PluginDataStore::from_snapshot_bytes(
            &store.to_snapshot_bytes().expect("encode snapshot"),
        )
        .expect("restore snapshot");
        for (index, scope) in scopes.iter().cloned().enumerate() {
            assert_eq!(
                restored
                    .get::<u32>(&key(scope, "value"))
                    .expect("decode value"),
                Some(index as u32)
            );
        }
    }

    #[test]
    fn entity_generation_prevents_reused_identity_from_reading_stale_data() {
        let old = key(
            DataScope::Entity {
                id: [7; 16],
                generation: 1,
            },
            "claim",
        );
        let new = key(
            DataScope::Entity {
                id: [7; 16],
                generation: 2,
            },
            "claim",
        );
        let mut store = PluginDataStore::default();
        store
            .set(old.clone(), 1, &"old occupant")
            .expect("store old");
        assert_eq!(store.get::<String>(&new).expect("lookup new"), None);
        assert_eq!(
            store.get::<String>(&old).expect("lookup old"),
            Some("old occupant".into())
        );

        let unloaded = store.unload_scope(old.scope());
        assert_eq!(unloaded.len(), 1);
        assert!(store.is_empty());
    }

    #[test]
    fn oversize_replacement_is_rejected_without_clobbering_the_old_value() {
        let key = key(DataScope::Plugin, "bounded");
        let mut store = PluginDataStore::default();
        store.set_blob(key.clone(), 1, b"keep me").expect("seed");
        let error = store
            .set_blob(key.clone(), 2, vec![0; MAX_PLUGIN_DATA_BLOB_BYTES + 1])
            .expect_err("oversized blob must be rejected");
        assert!(matches!(error, PluginDataError::BlobTooLarge { .. }));
        assert_eq!(store.get_blob(&key), Some(b"keep me".as_slice()));
        assert_eq!(store.record(&key).expect("record").schema_version(), 1);
    }

    #[test]
    fn migration_is_versioned_and_runs_only_for_an_older_record() {
        let key = key(DataScope::Plugin, "schema");
        let mut store = PluginDataStore::default();
        store.set_blob(key.clone(), 1, b"v1").expect("seed");
        assert!(
            store
                .migrate(&key, 2, |version, bytes| {
                    assert_eq!(version, 1);
                    assert_eq!(bytes, b"v1");
                    Ok(b"v2".to_vec())
                })
                .expect("migrate")
        );
        assert_eq!(store.record(&key).expect("record").schema_version(), 2);
        assert_eq!(store.get_blob(&key), Some(b"v2".as_slice()));
        assert!(
            !store
                .migrate(&key, 2, |_, _| panic!(
                    "same-version migration must not run"
                ))
                .expect("no-op migration")
        );
    }

    #[test]
    fn restore_rejects_duplicate_keys_before_returning_a_store() {
        let key = key(DataScope::Plugin, "duplicate");
        let first = PluginDataEntry::new(
            key.clone(),
            PluginDataRecord::new(1, b"first").expect("record"),
        )
        .expect("entry");
        let second =
            PluginDataEntry::new(key, PluginDataRecord::new(1, b"second").expect("record"))
                .expect("entry");
        assert_eq!(
            PluginDataStore::restore_entries([first, second]).expect_err("duplicate"),
            PluginDataError::DuplicateKey
        );
    }

    #[test]
    fn scope_snapshot_excludes_unrelated_lifecycle_records() {
        let world = DataScope::World { id: [11; 16] };
        let player = DataScope::Player { id: [12; 16] };
        let world_key = key(world.clone(), "owner");
        let player_key = key(player, "owner");
        let mut store = PluginDataStore::default();
        store.set(world_key.clone(), 1, &"alice").expect("world value");
        store.set(player_key, 1, &"bob").expect("player value");

        let snapshot = store.snapshot_scope(&world);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].key(), &world_key);
    }
}
