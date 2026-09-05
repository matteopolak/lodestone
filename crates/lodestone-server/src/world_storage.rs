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

use lodestone_core::{Reader, Writer, read_named_nbt, write_named_nbt};
use lodestone_storage::{
    ExtensionRegistration, NativeStore, RecordKey, RecordWrite, StoreError,
};
use lodestone_storage_schema::{
    BiomeSection, BuiltinDimension, ChunkRecord, ChunkSection, ExtensionTable, FORMAT_VERSION_V1,
    GeneralRecord, PlayerRecord, RegisteredExtension, StorageRecord,
    generated::{general_record, storage_record},
};

const GAME_DATA_VERSION: u32 = 46_002;
const SECTION_CELLS: usize = 16 * 16 * 16;

/// One native player record's deliberately bounded, typed locator state.
///
/// This is not an Anvil player-data replacement: inventory, health, velocity,
/// and fields this build does not model stay on the established Anvil path.
/// Position values are the schema's producer-owned signed fixed-point integers;
/// the adapter retains them exactly and deliberately performs no float rounding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativePlayerRecord {
    /// The complete 16-byte player identity.
    pub uuid: [u8; 16],
    /// The built-in dimension containing the stored position.
    pub dimension: BuiltinDimension,
    /// Producer-defined signed fixed-point feet position, retained verbatim.
    pub x_fixed: i32,
    /// Producer-defined signed fixed-point feet position, retained verbatim.
    pub y_fixed: i32,
    /// Producer-defined signed fixed-point feet position, retained verbatim.
    pub z_fixed: i32,
    /// Yaw in millidegrees.
    pub yaw_millidegrees: i32,
    /// Pitch in millidegrees.
    pub pitch_millidegrees: i32,
}

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
    /// A native chunk record cannot be represented by this server build.
    Chunk(ChunkRecordError),
    /// A native player locator record is malformed, unsupported, or ambiguous.
    Player(PlayerRecordError),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AnvilDoesNotAcceptTypedRecords => {
                formatter.write_str("the Anvil backend does not accept typed dirty records")
            }
            Self::Native(error) => write!(formatter, "native world storage failed: {error}"),
            Self::Chunk(error) => write!(formatter, "native chunk record failed: {error}"),
            Self::Player(error) => write!(formatter, "native player record failed: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<StoreError> for Error {
    fn from(error: StoreError) -> Self {
        Self::Native(error)
    }
}

impl From<ChunkRecordError> for Error {
    fn from(error: ChunkRecordError) -> Self {
        Self::Chunk(error)
    }
}

impl From<PlayerRecordError> for Error {
    fn from(error: PlayerRecordError) -> Self {
        Self::Player(error)
    }
}

/// Native chunk data this bounded adapter cannot safely discard.
///
/// Every `true` flag means a caller must retain the existing Anvil path (or a
/// later native schema revision) rather than turn a save into a terrain-only
/// replacement. Resident block entities are represented as lossless named-NBT
/// roots in the version-1 record; the remaining flags deliberately describe
/// data, not the source that happened to create it, so a plugin-created column
/// gets the same protection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UnsupportedChunkFields {
    /// The column has block-entity state this adapter cannot retain.
    pub block_entities: bool,
    /// The column has structure starts or references.
    pub structures: bool,
    /// The column is only shaped terrain, not a full playable column.
    pub shaped_generation: bool,
    /// The column owns not-yet-consumed one-shot spawn candidates.
    pub pending_generation_spawns: bool,
}

impl UnsupportedChunkFields {
    fn any(self) -> bool {
        self.block_entities
            || self.structures
            || self.shaped_generation
            || self.pending_generation_spawns
    }
}

/// A malformed, incompatible, or lossy native chunk conversion.
#[derive(Debug)]
pub enum ChunkRecordError {
    /// Saving would omit fields represented by [`UnsupportedChunkFields`].
    UnsupportedFields(UnsupportedChunkFields),
    /// The column cannot be represented as whole 16-row sections.
    UnalignedMinimumY(i32),
    /// A caller supplied an invalid expected column extent for loading.
    InvalidExtent { min_y: i32, height: i32 },
    /// The envelope did not contain a chunk body.
    MissingChunkBody,
    /// The stored coordinates do not match the key requested from the segment.
    CoordinateMismatch {
        expected_x: i32,
        expected_z: i32,
        actual_x: i32,
        actual_z: i32,
    },
    /// This build has no safe interpretation for a different game-data census.
    UnsupportedGameDataVersion(u32),
    /// A stored section is not the expected next 16-row window.
    UnexpectedSectionY { expected: i32, actual: i32 },
    /// The record contains too few or too many sections for the requested extent.
    SectionCount { expected: usize, actual: usize },
    /// A stored biome section is not the expected next 16-row window.
    UnexpectedBiomeSectionY { expected: i32, actual: i32 },
    /// The record contains too few or too many biome sections for its extent.
    BiomeSectionCount { expected: usize, actual: usize },
    /// A biome section's stated vertical-quart extent is invalid.
    InvalidBiomeQuartRows { expected: usize, actual: u32 },
    /// A biome section does not contain one enum value per quart cell.
    InvalidBiomeCellCount { expected: usize, actual: usize },
    /// The stored surface-biome answer is not its required four-by-four grid.
    InvalidSurfaceBiomeCount { actual: usize },
    /// The optional stored motion-blocking map has the wrong fixed grid size.
    InvalidMotionBlockingHeightCount { actual: usize },
    /// A stored motion-blocking value cannot fit the server's u16 representation.
    MotionBlockingHeightOutOfRange(u32),
    /// A section carries light or extension payloads this adapter cannot retain.
    UnsupportedStoredSectionData,
    /// A source column names a biome not available in this built-in census.
    UnsupportedBiome(String),
    /// A stored integer is not one of this format version's biome enum values.
    UnknownBuiltinBiome(i32),
    /// A stored numeric block-state ID is not in this build's registry.
    UnknownBlockStateId(u32),
    /// Packed local palette data is structurally invalid.
    InvalidPackedStates(&'static str),
    /// A persisted block-entity NBT root is malformed or cannot be interpreted.
    InvalidBlockEntityNbt { index: usize, reason: String },
    /// A block entity belongs to a different horizontal column than its record.
    BlockEntityOutsideColumn {
        index: usize,
        x: i32,
        z: i32,
        expected_x: i32,
        expected_z: i32,
    },
    /// A block entity's absolute Y coordinate is outside the requested extent.
    BlockEntityOutsideExtent { index: usize, y: i32, min_y: i32, height: i32 },
    /// A source entity's tuple position and lossless NBT root disagree.
    BlockEntityNbtPositionMismatch {
        index: usize,
        expected_x: i32,
        expected_y: i32,
        expected_z: i32,
        actual_x: i32,
        actual_y: i32,
        actual_z: i32,
    },
    /// More than one persisted entity claims one absolute block position.
    DuplicateBlockEntityPosition { x: i32, y: i32, z: i32 },
}

impl fmt::Display for ChunkRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFields(fields) => {
                write!(formatter, "would lose unsupported chunk fields: {fields:?}")
            }
            Self::UnalignedMinimumY(min_y) => {
                write!(formatter, "minimum Y {min_y} is not section-aligned")
            }
            Self::InvalidExtent { min_y, height } => {
                write!(formatter, "invalid expected chunk extent min_y={min_y}, height={height}")
            }
            Self::MissingChunkBody => formatter.write_str("record does not contain a chunk body"),
            Self::CoordinateMismatch { expected_x, expected_z, actual_x, actual_z } => {
                write!(formatter, "record coordinates ({actual_x}, {actual_z}) do not match requested ({expected_x}, {expected_z})")
            }
            Self::UnsupportedGameDataVersion(version) => {
                write!(formatter, "unsupported game data version {version}")
            }
            Self::UnexpectedSectionY { expected, actual } => {
                write!(formatter, "expected section Y {expected}, found {actual}")
            }
            Self::SectionCount { expected, actual } => {
                write!(formatter, "expected {expected} sections, found {actual}")
            }
            Self::UnexpectedBiomeSectionY { expected, actual } => {
                write!(formatter, "expected biome section Y {expected}, found {actual}")
            }
            Self::BiomeSectionCount { expected, actual } => {
                write!(formatter, "expected {expected} biome sections, found {actual}")
            }
            Self::InvalidBiomeQuartRows { expected, actual } => {
                write!(formatter, "expected {expected} biome quart rows, found {actual}")
            }
            Self::InvalidBiomeCellCount { expected, actual } => {
                write!(formatter, "expected {expected} biome cells, found {actual}")
            }
            Self::InvalidSurfaceBiomeCount { actual } => {
                write!(formatter, "expected 16 surface biome cells, found {actual}")
            }
            Self::InvalidMotionBlockingHeightCount { actual } => {
                write!(formatter, "expected 256 motion-blocking heights, found {actual}")
            }
            Self::MotionBlockingHeightOutOfRange(height) => {
                write!(formatter, "motion-blocking height {height} exceeds u16")
            }
            Self::UnsupportedStoredSectionData => {
                formatter.write_str("stored section carries unsupported light or extension data")
            }
            Self::UnsupportedBiome(name) => write!(formatter, "unsupported built-in biome {name}"),
            Self::UnknownBuiltinBiome(id) => write!(formatter, "unknown built-in biome ID {id}"),
            Self::UnknownBlockStateId(id) => write!(formatter, "unknown built-in block-state ID {id}"),
            Self::InvalidPackedStates(reason) => write!(formatter, "invalid packed block states: {reason}"),
            Self::InvalidBlockEntityNbt { index, reason } => {
                write!(formatter, "invalid block entity NBT at index {index}: {reason}")
            }
            Self::BlockEntityOutsideColumn {
                index,
                x,
                z,
                expected_x,
                expected_z,
            } => write!(
                formatter,
                "block entity {index} at ({x}, {z}) is outside chunk ({expected_x}, {expected_z})"
            ),
            Self::BlockEntityOutsideExtent {
                index,
                y,
                min_y,
                height,
            } => write!(
                formatter,
                "block entity {index} at Y {y} is outside [{min_y}, {})",
                min_y.saturating_add(*height)
            ),
            Self::BlockEntityNbtPositionMismatch {
                index,
                expected_x,
                expected_y,
                expected_z,
                actual_x,
                actual_y,
                actual_z,
            } => write!(
                formatter,
                "block entity {index} tuple position ({expected_x}, {expected_y}, {expected_z}) \
                 does not match its NBT position ({actual_x}, {actual_y}, {actual_z})"
            ),
            Self::DuplicateBlockEntityPosition { x, y, z } => {
                write!(formatter, "duplicate block entity at ({x}, {y}, {z})")
            }
        }
    }
}

impl std::error::Error for ChunkRecordError {}

/// A malformed, unsupported, or ambiguous native player locator record.
#[derive(Debug, Eq, PartialEq)]
pub enum PlayerRecordError {
    /// The envelope has a format version this reader does not understand.
    UnsupportedFormatVersion(u32),
    /// The envelope did not contain a typed general body.
    MissingGeneralBody,
    /// The general body was not a player locator record.
    MissingPlayerBody,
    /// The stored player UUID did not have its required 16 bytes.
    InvalidUuidLength { actual: usize },
    /// The typed record names an unknown or custom dimension.
    UnsupportedDimension(i32),
    /// This bounded reader cannot preserve opaque player extensions.
    UnsupportedExtensions,
    /// The key's 96-bit UUID prefix resolves to a different complete UUID.
    KeyCollision {
        requested: [u8; 16],
        stored: [u8; 16],
    },
}

impl fmt::Display for PlayerRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormatVersion(version) => {
                write!(formatter, "unsupported player record format version {version}")
            }
            Self::MissingGeneralBody => {
                formatter.write_str("record does not contain a general body")
            }
            Self::MissingPlayerBody => {
                formatter.write_str("general record does not contain a player body")
            }
            Self::InvalidUuidLength { actual } => {
                write!(formatter, "expected a 16-byte player UUID, found {actual} bytes")
            }
            Self::UnsupportedDimension(dimension) => {
                write!(formatter, "unsupported built-in player dimension {dimension}")
            }
            Self::UnsupportedExtensions => {
                formatter.write_str("player record carries unsupported extension payloads")
            }
            Self::KeyCollision { requested, stored } => write!(
                formatter,
                "player key collision: requested UUID {requested:02x?} conflicts with stored UUID {stored:02x?}"
            ),
        }
    }
}

impl std::error::Error for PlayerRecordError {}

trait DirtyRecordStore: Send {
    fn write_transaction(&mut self, writes: Vec<RecordWrite>) -> Result<(), StoreError>;
    fn get(&mut self, key: RecordKey) -> Result<Option<StorageRecord>, StoreError>;
    fn extension_table(&self) -> ExtensionTable;
    fn register_extensions(
        &mut self,
        registrations: &[ExtensionRegistration],
    ) -> Result<Vec<RegisteredExtension>, StoreError>;
}

impl DirtyRecordStore for NativeStore {
    fn write_transaction(&mut self, writes: Vec<RecordWrite>) -> Result<(), StoreError> {
        NativeStore::write_transaction(self, writes)
    }

    fn get(&mut self, key: RecordKey) -> Result<Option<StorageRecord>, StoreError> {
        NativeStore::get(self, key)
    }

    fn extension_table(&self) -> ExtensionTable {
        NativeStore::extension_table(self).clone()
    }

    fn register_extensions(
        &mut self,
        registrations: &[ExtensionRegistration],
    ) -> Result<Vec<RegisteredExtension>, StoreError> {
        NativeStore::register_extensions(self, registrations.iter().cloned())
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

    /// Returns the native extension table selected for this world.
    ///
    /// The Anvil backend has no typed extension-table sidecar, so treating it as
    /// one is an error rather than an empty, unwired registration set.
    pub fn native_extension_table(&self) -> Result<ExtensionTable, Error> {
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        Ok(native
            .lock()
            .expect("world storage lock poisoned")
            .extension_table())
    }

    /// Registers named extension schemas in the selected native backend.
    ///
    /// Registration is durable before a record can reference the returned local
    /// IDs. Callers must use those IDs in `ExtensionValue`; an unregistered ID
    /// is rejected by the native store before it appends a transaction.
    pub fn register_native_extensions(
        &self,
        registrations: impl IntoIterator<Item = ExtensionRegistration>,
    ) -> Result<Vec<RegisteredExtension>, Error> {
        let registrations: Vec<_> = registrations.into_iter().collect();
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        native
            .lock()
            .expect("world storage lock poisoned")
            .register_extensions(&registrations)
            .map_err(Into::into)
    }

    /// Saves one independently dirty bounded player locator record.
    ///
    /// This path intentionally retains only the fields represented by
    /// [`NativePlayerRecord`]. It does not inspect or replace the live player
    /// tick state, and it must not be substituted for the Anvil player-data
    /// writer. A UUID's first 96 bits form the compact native key; the complete
    /// UUID remains in the body and is checked before any replacement, so the
    /// unkeyed final 32 bits can never cause a silent overwrite.
    pub fn write_dirty_player(&self, player: NativePlayerRecord) -> Result<(), Error> {
        let key = player_key(player.uuid);
        let record = encode_player(player)?;
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        let mut native = native.lock().expect("world storage lock poisoned");
        if let Some(existing) = native.get(key)? {
            decode_player(player.uuid, existing)?;
        }
        native.write_transaction(vec![RecordWrite::new(key, record)])?;
        Ok(())
    }

    /// Loads one bounded native player locator record by its complete UUID.
    ///
    /// Missing records return `None`. A record whose compact key maps to a
    /// different UUID is an explicit collision error, and extensions or custom
    /// dimensions are refused rather than being silently discarded.
    pub fn load_player(&self, uuid: [u8; 16]) -> Result<Option<NativePlayerRecord>, Error> {
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        native
            .lock()
            .expect("world storage lock poisoned")
            .get(player_key(uuid))?
            .map(|record| decode_player(uuid, record))
            .transpose()
            .map_err(Into::into)
    }

    /// Saves one independently dirty server chunk through the native typed
    /// record path.
    ///
    /// The adapter is intentionally bounded: it stores block-state sections,
    /// built-in biome grids, the optional motion-blocking heightmap, and
    /// complete resident block-entity NBT roots, while refusing a column
    /// carrying any other state it cannot preserve. `Anvil` stays unchanged
    /// and refuses this method just as it
    /// refuses [`Self::write_dirty`].
    pub fn write_dirty_chunk(
        &self,
        column_x: i32,
        column_z: i32,
        column: &crate::chunk::ChunkColumn,
    ) -> Result<(), Error> {
        let record = encode_chunk(column_x, column_z, column)?;
        self.write_dirty([RecordWrite::new(RecordKey::chunk(column_x, column_z), record)])?;
        Ok(())
    }

    /// Reopens a typed native chunk record as a real [`crate::chunk::ChunkColumn`].
    ///
    /// `min_y` and `height` remain an explicit dimension contract because the
    /// version-1 record stores section coordinates, not a dimension definition.
    /// A mismatch, a future data version, extensions, light bytes, malformed
    /// block-entity NBT, or block entities outside this record's extent is an
    /// error rather than a partial load.
    pub fn load_chunk(
        &self,
        column_x: i32,
        column_z: i32,
        min_y: i32,
        height: i32,
    ) -> Result<Option<crate::chunk::ChunkColumn>, Error> {
        validate_extent(min_y, height)?;
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        let record = native
            .lock()
            .expect("world storage lock poisoned")
            .get(RecordKey::chunk(column_x, column_z))?;
        record
            .map(|record| decode_chunk(column_x, column_z, min_y, height, record))
            .transpose()
            .map_err(Into::into)
    }
}

fn encode_chunk(
    column_x: i32,
    column_z: i32,
    column: &crate::chunk::ChunkColumn,
) -> Result<StorageRecord, ChunkRecordError> {
    validate_extent(column.min_y, column.height)?;
    let unsupported = unsupported_fields(column);
    if unsupported.any() {
        return Err(ChunkRecordError::UnsupportedFields(unsupported));
    }

    let mut cells = Vec::with_capacity(SECTION_CELLS);
    let sections = (0..column.section_count())
        .map(|section_index| {
            cells.clear();
            column.append_section_cells(section_index, &mut cells);
            let mut palette_state_ids = Vec::new();
            let mut local_indices = Vec::with_capacity(cells.len());
            for &column_palette_index in &cells {
                let state_id = column.palette_state_ids()[column_palette_index as usize].raw();
                let local = match palette_state_ids.iter().position(|&id| id == state_id) {
                    Some(index) => index,
                    None => {
                        palette_state_ids.push(state_id);
                        palette_state_ids.len() - 1
                    }
                };
                local_indices.push(
                    u16::try_from(local).expect("one section has at most 4096 states"),
                );
            }
            let palette_bits = palette_bits(palette_state_ids.len())?;
            Ok(ChunkSection {
                section_y: column.min_y.div_euclid(16) + section_index as i32,
                palette_bits,
                palette_state_ids,
                block_state_indices: pack_indices(&local_indices, palette_bits),
                sky_light: Vec::new(),
                block_light: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>, ChunkRecordError>>()?;
    let biome_sections = (0..column.section_count())
        .map(|section_index| {
            let quart_rows = section_rows(column.height, section_index).div_ceil(4);
            let mut biome_ids = Vec::with_capacity(quart_rows * 16);
            for qy in 0..quart_rows {
                for qz in 0..4 {
                    for qx in 0..4 {
                        biome_ids.push(builtin_biome_id(column.biome_cell(
                            qx,
                            section_index * 4 + qy,
                            qz,
                        ))?);
                    }
                }
            }
            Ok(BiomeSection {
                section_y: column.min_y.div_euclid(16) + section_index as i32,
                quart_rows: quart_rows as u32,
                biome_ids,
            })
        })
        .collect::<Result<Vec<_>, ChunkRecordError>>()?;
    let mut block_entity_nbt = Vec::with_capacity(column.block_entities().len());
    let mut block_entity_positions = Vec::with_capacity(column.block_entities().len());
    for (index, (pos, entity)) in column.block_entities().iter().enumerate() {
        validate_block_entity_position(index, *pos, column_x, column_z, column.min_y, column.height)?;
        let nbt = crate::chunk_nbt::block_entity_to_nbt(*pos, entity);
        let (nbt_pos, _) = crate::chunk_nbt::block_entity_from_nbt(&nbt).ok_or_else(|| {
            ChunkRecordError::InvalidBlockEntityNbt {
                index,
                reason: "source entity does not encode as a block-entity compound with id and absolute coordinates".to_owned(),
            }
        })?;
        if nbt_pos != *pos {
            return Err(ChunkRecordError::BlockEntityNbtPositionMismatch {
                index,
                expected_x: pos.x,
                expected_y: pos.y,
                expected_z: pos.z,
                actual_x: nbt_pos.x,
                actual_y: nbt_pos.y,
                actual_z: nbt_pos.z,
            });
        }
        if block_entity_positions.contains(pos) {
            return Err(ChunkRecordError::DuplicateBlockEntityPosition {
                x: pos.x,
                y: pos.y,
                z: pos.z,
            });
        }
        let mut writer = Writer::default();
        write_named_nbt(&mut writer, "", &nbt).map_err(|error| {
            ChunkRecordError::InvalidBlockEntityNbt {
                index,
                reason: error.to_string(),
            }
        })?;
        block_entity_nbt.push(writer.into_vec());
        block_entity_positions.push(*pos);
    }
    let surface_biome_ids = column
        .biome_quarts()
        .iter()
        .map(|name| builtin_biome_id(name))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(StorageRecord {
        format_version: FORMAT_VERSION_V1,
        record: Some(storage_record::Record::Chunk(ChunkRecord {
            column_x,
            column_z,
            game_data_version: GAME_DATA_VERSION,
            sections,
            biome_sections,
            surface_biome_ids,
            motion_blocking_heights: column
                .motion_blocking()
                .map(|heights| heights.iter().map(|&height| u32::from(height)).collect())
                .unwrap_or_default(),
            block_entity_nbt,
            extensions: Vec::new(),
        })),
    })
}

fn decode_chunk(
    expected_x: i32,
    expected_z: i32,
    min_y: i32,
    height: i32,
    record: StorageRecord,
) -> Result<crate::chunk::ChunkColumn, ChunkRecordError> {
    if record.format_version != FORMAT_VERSION_V1 {
        return Err(ChunkRecordError::InvalidPackedStates("unsupported record format version"));
    }
    let Some(storage_record::Record::Chunk(chunk)) = record.record else {
        return Err(ChunkRecordError::MissingChunkBody);
    };
    if (chunk.column_x, chunk.column_z) != (expected_x, expected_z) {
        return Err(ChunkRecordError::CoordinateMismatch {
            expected_x,
            expected_z,
            actual_x: chunk.column_x,
            actual_z: chunk.column_z,
        });
    }
    if chunk.game_data_version != GAME_DATA_VERSION {
        return Err(ChunkRecordError::UnsupportedGameDataVersion(chunk.game_data_version));
    }
    if !chunk.extensions.is_empty() {
        return Err(ChunkRecordError::UnsupportedStoredSectionData);
    }
    let expected_sections = (height as usize).div_ceil(16);
    if chunk.sections.len() != expected_sections {
        return Err(ChunkRecordError::SectionCount {
            expected: expected_sections,
            actual: chunk.sections.len(),
        });
    }

    let mut column = crate::chunk::ChunkColumn::new(min_y, height);
    for (section_index, section) in chunk.sections.iter().enumerate() {
        let expected_section_y = min_y.div_euclid(16) + section_index as i32;
        if section.section_y != expected_section_y {
            return Err(ChunkRecordError::UnexpectedSectionY {
                expected: expected_section_y,
                actual: section.section_y,
            });
        }
        if !section.sky_light.is_empty() || !section.block_light.is_empty() {
            return Err(ChunkRecordError::UnsupportedStoredSectionData);
        }
        let expected_cells = section_rows(height, section_index) * 16 * 16;
        let local_indices = unpack_indices(section, expected_cells)?;
        let local_palette = section
            .palette_state_ids
            .iter()
            .map(|&state_id| state_string(state_id))
            .collect::<Result<Vec<_>, _>>()?;
        let local_palette_refs: Vec<_> = local_palette.iter().map(String::as_str).collect();
        column.set_section_from_local_palette(
            min_y + section_index as i32 * 16,
            &local_palette_refs,
            &local_indices,
        );
    }
    // Older terrain-only records intentionally omitted both biome fields. They
    // decode to `ChunkColumn::new`'s all-default biome state, while a record
    // that carries either field must carry the complete paired representation.
    if !chunk.biome_sections.is_empty() || !chunk.surface_biome_ids.is_empty() {
        if chunk.biome_sections.len() != expected_sections {
            return Err(ChunkRecordError::BiomeSectionCount {
                expected: expected_sections,
                actual: chunk.biome_sections.len(),
            });
        }
        if chunk.surface_biome_ids.len() != 16 {
            return Err(ChunkRecordError::InvalidSurfaceBiomeCount {
                actual: chunk.surface_biome_ids.len(),
            });
        }
        for (section_index, section) in chunk.biome_sections.iter().enumerate() {
            let expected_section_y = min_y.div_euclid(16) + section_index as i32;
            if section.section_y != expected_section_y {
                return Err(ChunkRecordError::UnexpectedBiomeSectionY {
                    expected: expected_section_y,
                    actual: section.section_y,
                });
            }
            let expected_quart_rows = section_rows(height, section_index).div_ceil(4);
            if section.quart_rows != expected_quart_rows as u32 {
                return Err(ChunkRecordError::InvalidBiomeQuartRows {
                    expected: expected_quart_rows,
                    actual: section.quart_rows,
                });
            }
            let expected_cells = expected_quart_rows * 16;
            if section.biome_ids.len() != expected_cells {
                return Err(ChunkRecordError::InvalidBiomeCellCount {
                    expected: expected_cells,
                    actual: section.biome_ids.len(),
                });
            }
            for (offset, &biome_id) in section.biome_ids.iter().enumerate() {
                let qy = section_index * 4 + offset / 16;
                let qz = (offset / 4) % 4;
                let qx = offset % 4;
                let name = builtin_biome_name(biome_id)?;
                column.set_biome_cell(qx, qy, qz, &name);
            }
        }
        let surface = chunk
            .surface_biome_ids
            .iter()
            .map(|&biome_id| builtin_biome_name(biome_id))
            .collect::<Result<Vec<_>, _>>()?;
        column.set_biome_quarts(&surface);
    }
    if !chunk.motion_blocking_heights.is_empty() {
        let actual = chunk.motion_blocking_heights.len();
        let heights: [u16; 256] = chunk
            .motion_blocking_heights
            .iter()
            .copied()
            .map(|height| {
                u16::try_from(height)
                    .map_err(|_| ChunkRecordError::MotionBlockingHeightOutOfRange(height))
            })
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| ChunkRecordError::InvalidMotionBlockingHeightCount { actual })?;
        column.set_motion_blocking(heights);
    }
    let mut block_entities = Vec::with_capacity(chunk.block_entity_nbt.len());
    for (index, bytes) in chunk.block_entity_nbt.iter().enumerate() {
        let mut reader = Reader::new(bytes);
        let (name, nbt) = read_named_nbt(&mut reader).map_err(|error| {
            ChunkRecordError::InvalidBlockEntityNbt {
                index,
                reason: error.to_string(),
            }
        })?;
        if !name.is_empty() {
            return Err(ChunkRecordError::InvalidBlockEntityNbt {
                index,
                reason: "root name is not empty".to_owned(),
            });
        }
        reader.ensure_empty().map_err(|error| ChunkRecordError::InvalidBlockEntityNbt {
            index,
            reason: error.to_string(),
        })?;
        let (pos, entity) = crate::chunk_nbt::block_entity_from_nbt(&nbt).ok_or_else(|| {
            ChunkRecordError::InvalidBlockEntityNbt {
                index,
                reason: "root is not a block-entity compound with id and absolute coordinates".to_owned(),
            }
        })?;
        validate_block_entity_position(index, pos, expected_x, expected_z, min_y, height)?;
        if block_entities.iter().any(|(other, _)| *other == pos) {
            return Err(ChunkRecordError::DuplicateBlockEntityPosition {
                x: pos.x,
                y: pos.y,
                z: pos.z,
            });
        }
        block_entities.push((pos, entity));
    }
    column.set_block_entities(block_entities);
    Ok(column)
}

fn player_key(uuid: [u8; 16]) -> RecordKey {
    // The compact key has exactly 96 identity bits. The complete UUID remains
    // in `PlayerRecord`, and `write_dirty_player`/`load_player` compare it
    // before replacing or returning a record so the remaining 32 bits cannot
    // turn a collision into a different player's save.
    RecordKey::general(
        i32::from_le_bytes(uuid[..4].try_into().expect("four UUID bytes")),
        i32::from_le_bytes(uuid[4..8].try_into().expect("four UUID bytes")),
        u32::from_le_bytes(uuid[8..12].try_into().expect("four UUID bytes")),
    )
}

fn encode_player(player: NativePlayerRecord) -> Result<StorageRecord, PlayerRecordError> {
    let dimension = player.dimension as i32;
    validate_player_dimension(dimension)?;
    Ok(StorageRecord {
        format_version: FORMAT_VERSION_V1,
        record: Some(storage_record::Record::General(GeneralRecord {
            record: Some(general_record::Record::Player(PlayerRecord {
                player_uuid: player.uuid.to_vec(),
                dimension,
                x_fixed: player.x_fixed,
                y_fixed: player.y_fixed,
                z_fixed: player.z_fixed,
                yaw_millidegrees: player.yaw_millidegrees,
                pitch_millidegrees: player.pitch_millidegrees,
            })),
            extensions: Vec::new(),
        })),
    })
}

fn decode_player(
    requested_uuid: [u8; 16],
    record: StorageRecord,
) -> Result<NativePlayerRecord, PlayerRecordError> {
    if record.format_version != FORMAT_VERSION_V1 {
        return Err(PlayerRecordError::UnsupportedFormatVersion(record.format_version));
    }
    let Some(storage_record::Record::General(general)) = record.record else {
        return Err(PlayerRecordError::MissingGeneralBody);
    };
    if !general.extensions.is_empty() {
        return Err(PlayerRecordError::UnsupportedExtensions);
    }
    let Some(general_record::Record::Player(player)) = general.record else {
        return Err(PlayerRecordError::MissingPlayerBody);
    };
    let actual = player.player_uuid.len();
    let uuid: [u8; 16] = player
        .player_uuid
        .try_into()
        .map_err(|_| PlayerRecordError::InvalidUuidLength { actual })?;
    if uuid != requested_uuid {
        return Err(PlayerRecordError::KeyCollision {
            requested: requested_uuid,
            stored: uuid,
        });
    }
    validate_player_dimension(player.dimension)?;
    Ok(NativePlayerRecord {
        uuid,
        dimension: BuiltinDimension::try_from(player.dimension)
            .expect("validated built-in player dimension"),
        x_fixed: player.x_fixed,
        y_fixed: player.y_fixed,
        z_fixed: player.z_fixed,
        yaw_millidegrees: player.yaw_millidegrees,
        pitch_millidegrees: player.pitch_millidegrees,
    })
}

fn validate_player_dimension(dimension: i32) -> Result<(), PlayerRecordError> {
    match BuiltinDimension::try_from(dimension) {
        Ok(BuiltinDimension::Overworld | BuiltinDimension::Nether | BuiltinDimension::End) => {
            Ok(())
        }
        Ok(BuiltinDimension::Unspecified) | Err(_) => {
            Err(PlayerRecordError::UnsupportedDimension(dimension))
        }
    }
}

fn validate_block_entity_position(
    index: usize,
    pos: lodestone_model::BlockPos,
    column_x: i32,
    column_z: i32,
    min_y: i32,
    height: i32,
) -> Result<(), ChunkRecordError> {
    if (pos.x.div_euclid(16), pos.z.div_euclid(16)) != (column_x, column_z) {
        return Err(ChunkRecordError::BlockEntityOutsideColumn {
            index,
            x: pos.x,
            z: pos.z,
            expected_x: column_x,
            expected_z: column_z,
        });
    }
    if !(min_y..min_y.saturating_add(height)).contains(&pos.y) {
        return Err(ChunkRecordError::BlockEntityOutsideExtent {
            index,
            y: pos.y,
            min_y,
            height,
        });
    }
    Ok(())
}

fn unsupported_fields(column: &crate::chunk::ChunkColumn) -> UnsupportedChunkFields {
    UnsupportedChunkFields {
        block_entities: false,
        structures: !column.structure_starts().is_empty()
            || !column.structure_references().is_empty(),
        shaped_generation: column.generation_stage() == crate::chunk::ChunkGenerationStage::Shaped,
        pending_generation_spawns: column.has_pending_generation_spawns(),
    }
}

fn builtin_biome_id(name: &str) -> Result<i32, ChunkRecordError> {
    let Some(path) = name.strip_prefix("minecraft:") else {
        return Err(ChunkRecordError::UnsupportedBiome(name.to_string()));
    };
    let index = lodestone_data::biomes::BIOME_NAMES
        .binary_search(&path)
        .map_err(|_| ChunkRecordError::UnsupportedBiome(name.to_string()))?;
    Ok((index + 1) as i32)
}

fn builtin_biome_name(id: i32) -> Result<String, ChunkRecordError> {
    lodestone_storage_schema::BuiltinBiome::try_from(id)
        .map_err(|_| ChunkRecordError::UnknownBuiltinBiome(id))?;
    let Some(path) = id
        .checked_sub(1)
        .and_then(|index| lodestone_data::biomes::BIOME_NAMES.get(index as usize))
    else {
        return Err(ChunkRecordError::UnknownBuiltinBiome(id));
    };
    Ok(format!("minecraft:{path}"))
}

fn validate_extent(min_y: i32, height: i32) -> Result<(), ChunkRecordError> {
    if min_y.rem_euclid(16) != 0 {
        return Err(ChunkRecordError::UnalignedMinimumY(min_y));
    }
    if height <= 0 {
        return Err(ChunkRecordError::InvalidExtent { min_y, height });
    }
    Ok(())
}

fn section_rows(height: i32, section_index: usize) -> usize {
    (height as usize).saturating_sub(section_index * 16).min(16)
}

fn palette_bits(palette_len: usize) -> Result<u32, ChunkRecordError> {
    let max_index = palette_len
        .checked_sub(1)
        .ok_or(ChunkRecordError::InvalidPackedStates("empty palette"))?;
    let bits = (usize::BITS - max_index.leading_zeros()).max(1);
    if bits > 15 {
        return Err(ChunkRecordError::InvalidPackedStates("palette needs more than 15 bits"));
    }
    Ok(bits)
}

fn pack_indices(indices: &[u16], bits: u32) -> Vec<u8> {
    let mut packed = vec![0; (indices.len() * bits as usize).div_ceil(8)];
    for (index, &value) in indices.iter().enumerate() {
        let start = index * bits as usize;
        for bit in 0..bits as usize {
            if (value >> bit) & 1 != 0 {
                packed[(start + bit) / 8] |= 1 << ((start + bit) % 8);
            }
        }
    }
    packed
}

fn unpack_indices(
    section: &ChunkSection,
    expected_cells: usize,
) -> Result<Vec<u16>, ChunkRecordError> {
    if !(1..=15).contains(&section.palette_bits) {
        return Err(ChunkRecordError::InvalidPackedStates("palette width is outside 1..=15"));
    }
    if section.palette_state_ids.is_empty() {
        return Err(ChunkRecordError::InvalidPackedStates("palette is empty"));
    }
    let expected_bytes = (expected_cells * section.palette_bits as usize).div_ceil(8);
    if section.block_state_indices.len() != expected_bytes {
        return Err(ChunkRecordError::InvalidPackedStates(
            "packed byte length does not match section extent",
        ));
    }
    let mut indices = Vec::with_capacity(expected_cells);
    for index in 0..expected_cells {
        let start = index * section.palette_bits as usize;
        let mut value = 0_u16;
        for bit in 0..section.palette_bits as usize {
            value |= u16::from(
                (section.block_state_indices[(start + bit) / 8] >> ((start + bit) % 8)) & 1,
            ) << bit;
        }
        if value as usize >= section.palette_state_ids.len() {
            return Err(ChunkRecordError::InvalidPackedStates("index exceeds local palette"));
        }
        indices.push(value);
    }
    Ok(indices)
}

fn state_string(state_id: u32) -> Result<String, ChunkRecordError> {
    let state = lodestone_data::block_states::StateId::new(state_id)
        .ok_or(ChunkRecordError::UnknownBlockStateId(state_id))?;
    let mut value = state.name().to_string();
    let properties = state.properties();
    if !properties.is_empty() {
        value.push('[');
        for (index, (key, property)) in properties.iter().enumerate() {
            if index != 0 {
                value.push(',');
            }
            value.push_str(key);
            value.push('=');
            value.push_str(property);
        }
        value.push(']');
    }
    Ok(value)
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

        fn get(&mut self, _key: RecordKey) -> Result<Option<StorageRecord>, StoreError> {
            Ok(None)
        }

        fn extension_table(&self) -> ExtensionTable {
            ExtensionTable {
                table_version: FORMAT_VERSION_V1,
                extensions: Vec::new(),
            }
        }

        fn register_extensions(
            &mut self,
            _registrations: &[ExtensionRegistration],
        ) -> Result<Vec<RegisteredExtension>, StoreError> {
            Err(StoreError::Corrupt {
                offset: 0,
                reason: "recording store does not model extension registration".to_owned(),
            })
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
                    biome_sections: Vec::new(),
                    surface_biome_ids: Vec::new(),
                    motion_blocking_heights: Vec::new(),
                    block_entity_nbt: Vec::new(),
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
        assert!(matches!(
            storage.write_dirty_player(NativePlayerRecord {
                uuid: [1; 16],
                dimension: BuiltinDimension::Overworld,
                x_fixed: 0,
                y_fixed: 0,
                z_fixed: 0,
                yaw_millidegrees: 0,
                pitch_millidegrees: 0,
            }),
            Err(Error::AnvilDoesNotAcceptTypedRecords)
        ));
    }

    #[test]
    fn native_player_locator_reopens_without_touching_anvil_player_data() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-player-record-{}-{unique}",
            std::process::id()
        ));
        let player = NativePlayerRecord {
            uuid: [
                0x3c, 0x16, 0x72, 0x95, 0x2b, 0xd0, 0x41, 0x5a, 0x87, 0xef, 0x91, 0x44, 0xee,
                0x77, 0x31, 0x09,
            ],
            dimension: BuiltinDimension::Nether,
            x_fixed: -12_345,
            y_fixed: 2_048,
            z_fixed: 98_765,
            yaw_millidegrees: -179_999,
            pitch_millidegrees: 89_999,
        };
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native store");
        storage
            .write_dirty_player(player)
            .expect("write bounded player locator");
        drop(storage);

        let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("reopen native store");
        assert_eq!(
            reopened.load_player(player.uuid).unwrap(),
            Some(player),
            "the persisted body retains every typed locator field"
        );
        let absent = [0x7f; 16];
        assert!(
            reopened.load_player(absent).unwrap().is_none(),
            "a different UUID prefix is the independent absence control"
        );
        drop(reopened);
        std::fs::remove_dir_all(directory).expect("remove native test segment");
    }

    #[test]
    fn native_player_locator_refuses_the_same_compact_key_for_another_uuid() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-player-collision-{}-{unique}",
            std::process::id()
        ));
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native store");
        let first = NativePlayerRecord {
            uuid: [0x55; 16],
            dimension: BuiltinDimension::End,
            x_fixed: 1,
            y_fixed: 2,
            z_fixed: 3,
            yaw_millidegrees: 4,
            pitch_millidegrees: 5,
        };
        let mut collision = first;
        collision.uuid[15] = 0x99;
        storage.write_dirty_player(first).unwrap();
        assert!(matches!(
            storage.write_dirty_player(collision),
            Err(Error::Player(PlayerRecordError::KeyCollision { .. }))
        ));
        drop(storage);
        std::fs::remove_dir_all(directory).expect("remove native test segment");
    }

    #[test]
    fn native_player_locator_refuses_an_extension_before_it_can_be_dropped() {
        let player = NativePlayerRecord {
            uuid: [0x21; 16],
            dimension: BuiltinDimension::Overworld,
            x_fixed: -1,
            y_fixed: 2,
            z_fixed: -3,
            yaw_millidegrees: 4,
            pitch_millidegrees: -5,
        };
        let mut record = encode_player(player).expect("built-in player encodes");
        let Some(storage_record::Record::General(general)) = &mut record.record else {
            panic!("player encoder must produce a general record");
        };
        general
            .extensions
            .push(lodestone_storage_schema::ExtensionValue {
                local_id: 1,
                payload: vec![0xde, 0xad],
            });

        assert_eq!(
            decode_player(player.uuid, record),
            Err(PlayerRecordError::UnsupportedExtensions)
        );
    }

    #[test]
    fn native_chunk_reopens_as_the_same_real_block_grid() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-chunk-record-{}-{unique}",
            std::process::id()
        ));
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native store");
        let mut source = crate::chunk::ChunkColumn::new(-16, 32);
        source.set_block(1, -16, 2, "minecraft:stone");
        source.set_block(3, -1, 4, "minecraft:oak_log[axis=x]");
        source.set_block(5, 15, 6, "minecraft:water[level=3]");
        let opaque_pos = lodestone_model::BlockPos::new(-109, 0, 184);
        let opaque_nbt = lodestone_core::Nbt::Compound(vec![
            ("id".to_owned(), lodestone_core::Nbt::String("example:archive".to_owned())),
            ("x".to_owned(), lodestone_core::Nbt::Int(opaque_pos.x)),
            ("y".to_owned(), lodestone_core::Nbt::Int(opaque_pos.y)),
            ("z".to_owned(), lodestone_core::Nbt::Int(opaque_pos.z)),
            (
                "example:payload".to_owned(),
                lodestone_core::Nbt::Compound(vec![(
                    "untouched".to_owned(),
                    lodestone_core::Nbt::Long(9_876_543_210),
                )]),
            ),
        ]);
        source.set_block_entities(vec![
            (
                lodestone_model::BlockPos::new(-111, -1, 182),
                crate::block_entities::BlockEntity::Beacon(Default::default()),
            ),
            (
                opaque_pos,
                crate::block_entities::BlockEntity::Opaque {
                    id: "example:archive".to_owned(),
                    nbt: opaque_nbt.clone(),
                },
            ),
        ]);

        storage
            .write_dirty_chunk(-7, 11, &source)
            .expect("write supported terrain-only chunk");
        drop(storage);

        let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("reopen native store");
        let loaded = reopened
            .load_chunk(-7, 11, -16, 32)
            .expect("decode reopened chunk")
            .expect("stored chunk is present");
        assert_eq!(loaded.block_state(1, -16, 2), "minecraft:stone");
        assert_eq!(loaded.block_state(3, -1, 4), "minecraft:oak_log[axis=x]");
        assert_eq!(loaded.block_state(5, 15, 6), "minecraft:water[level=3]");
        assert_eq!(loaded.block_state(0, 0, 0), "minecraft:air");
        assert_eq!(
            loaded.block_entities(),
            source.block_entities(),
            "resident simulated and opaque block entities survive a native reopen"
        );
        assert!(
            reopened.load_chunk(-8, 11, -16, 32).unwrap().is_none(),
            "a distinct key is the independent absence control"
        );
        drop(reopened);
        std::fs::remove_dir_all(directory).expect("remove native test segment");
    }

    #[test]
    fn native_chunk_reopens_surface_and_three_dimensional_biomes() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-chunk-biomes-{}-{unique}",
            std::process::id()
        ));
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native store");
        let mut source = crate::chunk::ChunkColumn::new(0, 16);
        source.set_biome_cell(0, 0, 0, "minecraft:desert");
        source.set_biome_cell(1, 3, 2, "minecraft:deep_dark");
        let mut surface = vec!["minecraft:plains".to_string(); 16];
        surface[5] = "minecraft:cherry_grove".to_string();
        source.set_biome_quarts(&surface);

        storage
            .write_dirty_chunk(0, 0, &source)
            .expect("write built-in biome metadata");
        drop(storage);

        let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("reopen native store");
        let loaded = reopened
            .load_chunk(0, 0, 0, 16)
            .expect("decode saved biome metadata")
            .expect("saved chunk is present");
        assert_eq!(loaded.biome_state_at(0, 0, 0), "minecraft:desert");
        assert_eq!(loaded.biome_state_at(4, 15, 8), "minecraft:deep_dark");
        assert_eq!(loaded.biome_state(4, 4), "minecraft:cherry_grove");
        drop(reopened);
        std::fs::remove_dir_all(&directory).expect("remove native test segment");
    }

    #[test]
    fn native_chunk_refuses_an_opaque_entity_whose_nbt_position_disagrees() {
        let mut source = crate::chunk::ChunkColumn::new(0, 16);
        let tuple_pos = lodestone_model::BlockPos::new(1, 4, 1);
        source.set_block_entities(vec![(
            tuple_pos,
            crate::block_entities::BlockEntity::Opaque {
                id: "example:custom".to_owned(),
                nbt: lodestone_core::Nbt::Compound(vec![
                    ("id".to_owned(), lodestone_core::Nbt::String("example:custom".to_owned())),
                    ("x".to_owned(), lodestone_core::Nbt::Int(2)),
                    ("y".to_owned(), lodestone_core::Nbt::Int(tuple_pos.y)),
                    ("z".to_owned(), lodestone_core::Nbt::Int(tuple_pos.z)),
                ]),
            },
        )]);

        assert!(matches!(
            encode_chunk(0, 0, &source),
            Err(ChunkRecordError::BlockEntityNbtPositionMismatch { .. })
        ));
    }

    #[test]
    fn native_chunk_refuses_a_malformed_stored_block_entity_instead_of_dropping_it() {
        let source = crate::chunk::ChunkColumn::new(0, 16);
        let mut record = encode_chunk(0, 0, &source).expect("empty terrain encodes");
        let Some(storage_record::Record::Chunk(chunk)) = record.record.as_mut() else {
            panic!("chunk encoder must produce a chunk record");
        };
        // A named-NBT `End` root has no compound, id, or coordinates. It is a
        // structurally valid NBT byte sequence, so accepting it would prove a
        // decoder drop rather than merely a parser error.
        chunk.block_entity_nbt = vec![vec![lodestone_core::NbtTag::End.id()]];

        assert!(matches!(
            decode_chunk(0, 0, 0, 16, record),
            Err(ChunkRecordError::InvalidBlockEntityNbt { index: 0, .. })
        ));
    }

    #[test]
    fn stored_biome_discriminants_match_the_built_in_census() {
        for (index, &path) in lodestone_data::biomes::BIOME_NAMES.iter().enumerate() {
            let id = (index + 1) as i32;
            assert_eq!(builtin_biome_id(&format!("minecraft:{path}")).unwrap(), id);
            assert_eq!(builtin_biome_name(id).unwrap(), format!("minecraft:{path}"));
            assert_eq!(
                lodestone_storage_schema::BuiltinBiome::try_from(id)
                    .unwrap()
                    .as_str_name(),
                format!("BUILTIN_BIOME_{}", path.to_ascii_uppercase())
            );
        }
    }
}
