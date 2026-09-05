//! Selected backend for integrated-server dirty typed records.
//!
//! This module is deliberately the **record** seam, not a claim that a native
//! selection can already load every part of a world. `Anvil` remains the
//! integrated server's terrain/entity/metadata implementation. A host selects
//! `LodestoneNative` only for producers that can emit validated
//! `RecordWrite`s; each call writes exactly the records made dirty by that
//! producer in one transaction.

use std::fmt;
use std::path::PathBuf;
use std::sync::Mutex;

use lodestone_storage::{NativeStore, RecordWrite, StoreError};

/// The explicit persistent-record backend selected by a host.
///
/// `Anvil` is the compatibility selection: existing region-file persistence
/// remains responsible for its current save set and does not accept typed
/// record writes. `LodestoneNative` stores only records a producer submits
/// through [`WorldStorage::write_dirty`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorldStorageBackend {
    /// Keep the established Anvil-backed integrated-world behaviour.
    Anvil,
    /// Persist submitted, independently dirty typed records in `directory`.
    LodestoneNative {
        /// Directory containing the native `world.ls` segment.
        directory: PathBuf,
    },
}

/// A backend-open or dirty-record write failure.
#[derive(Debug)]
pub enum Error {
    /// The selected Anvil path has no typed-record adapter yet.
    AnvilDoesNotAcceptTypedRecords,
    /// The native segment rejected or could not commit a record batch.
    Native(StoreError),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AnvilDoesNotAcceptTypedRecords => {
                formatter.write_str("the Anvil backend does not accept typed dirty records")
            }
            Self::Native(error) => write!(formatter, "native world storage failed: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<StoreError> for Error {
    fn from(error: StoreError) -> Self {
        Self::Native(error)
    }
}

trait DirtyRecordStore: Send {
    fn write_transaction(&mut self, writes: Vec<RecordWrite>) -> Result<(), StoreError>;
}

impl DirtyRecordStore for NativeStore {
    fn write_transaction(&mut self, writes: Vec<RecordWrite>) -> Result<(), StoreError> {
        NativeStore::write_transaction(self, writes)
    }
}

/// One selected world-record backend, safe to share with an integrated-server
/// handle and any future producer.
pub struct WorldStorage {
    backend: WorldStorageBackend,
    native: Option<Mutex<Box<dyn DirtyRecordStore>>>,
}

impl fmt::Debug for WorldStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorldStorage")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

impl WorldStorage {
    /// Opens the requested record backend.
    pub fn open(backend: WorldStorageBackend) -> Result<Self, Error> {
        let native = match &backend {
            WorldStorageBackend::Anvil => None,
            WorldStorageBackend::LodestoneNative { directory } => {
                Some(Mutex::new(Box::new(NativeStore::open(directory)?) as Box<dyn DirtyRecordStore>))
            }
        };
        Ok(Self { backend, native })
    }

    /// Returns the host's explicit backend selection.
    #[must_use]
    pub const fn backend(&self) -> &WorldStorageBackend {
        &self.backend
    }

    /// Atomically commits exactly one producer's currently dirty records.
    ///
    /// An empty producer batch performs no I/O and returns zero. The native
    /// store does not scan or serialize resident world state: callers must
    /// pass only changed records, so an unrelated dirty player or block entity
    /// cannot make a column save rewrite every record in the segment.
    pub fn write_dirty(
        &self,
        writes: impl IntoIterator<Item = RecordWrite>,
    ) -> Result<usize, Error> {
        let writes: Vec<_> = writes.into_iter().collect();
        if writes.is_empty() {
            return Ok(0);
        }
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        let count = writes.len();
        native
            .lock()
            .expect("world storage lock poisoned")
            .write_transaction(writes)?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use lodestone_storage::RecordKey;
    use lodestone_storage_schema::{ChunkRecord, ChunkSection, StorageRecord, generated::storage_record};

    use super::*;

    #[derive(Debug, Clone)]
    struct RecordingStore(Arc<Mutex<Vec<Vec<RecordWrite>>>>);

    impl DirtyRecordStore for RecordingStore {
        fn write_transaction(&mut self, writes: Vec<RecordWrite>) -> Result<(), StoreError> {
            self.0.lock().expect("recording store lock poisoned").push(writes);
            Ok(())
        }
    }

    fn chunk(x: i32, z: i32, state: u32) -> RecordWrite {
        RecordWrite::new(
            RecordKey::chunk(x, z),
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
                        sky_light: Vec::new(),
                        block_light: Vec::new(),
                    }],
                    extensions: Vec::new(),
                })),
            },
        )
    }

    #[test]
    fn dirty_producer_writes_only_its_submitted_records() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let storage = WorldStorage {
            backend: WorldStorageBackend::LodestoneNative {
                directory: PathBuf::from("unused-in-fake-store"),
            },
            native: Some(Mutex::new(Box::new(RecordingStore(Arc::clone(&recorded))))),
        };

        assert_eq!(storage.write_dirty([chunk(2, 3, 9)]).unwrap(), 1);
        assert_eq!(storage.write_dirty(std::iter::empty()).unwrap(), 0);

        let batches = recorded.lock().expect("recording store lock poisoned");
        assert_eq!(batches.len(), 1, "an empty dirty set must not reach storage");
        assert_eq!(batches[0].len(), 1);
        assert_eq!(batches[0][0].key, RecordKey::chunk(2, 3));
    }

    #[test]
    fn anvil_selection_refuses_typed_records_instead_of_discarding_them() {
        let storage = WorldStorage::open(WorldStorageBackend::Anvil).unwrap();
        assert!(matches!(
            storage.write_dirty([chunk(2, 3, 9)]),
            Err(Error::AnvilDoesNotAcceptTypedRecords)
        ));
    }
}
