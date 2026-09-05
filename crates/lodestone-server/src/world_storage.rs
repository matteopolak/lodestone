//! Selected backend for integrated-server dirty typed records.
//!
//! This module is deliberately the **record** seam, not a claim that a native
//! selection can already load every part of a world. `Anvil` remains the
//! integrated server's terrain/entity/metadata implementation. A host selects
//! `LodestoneNative` only for producers that can emit validated
//! `RecordWrite`s; each call writes exactly the records made dirty by that
//! producer in one transaction.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Mutex;

use lodestone_core::{Reader, Writer, read_named_nbt, write_named_nbt};
use lodestone_storage::{
    Compaction, ExtensionRegistration, NativeChunkCoordinate, NativeStore, RecordKey, RecordWrite,
    StoreError,
};
use lodestone_storage_schema::{
    BiomeSection, BuiltinDimension, ChunkRecord, ChunkSection, ExtensionTable, FORMAT_VERSION_V1,
    EntityRecord, GameMode as StoredGameMode, GeneralRecord, LightData as StoredLightData,
    LightSection, PlayerRecord,
    RegisteredExtension, WorldProperties,
    ScheduledTick as StoredScheduledTick, ScheduledTickKind, ScheduledTickPriority, StorageRecord,
    generated::{general_record, light_data, storage_record},
};

const GAME_DATA_VERSION: u32 = 46_002;
const SECTION_CELLS: usize = 16 * 16 * 16;
const GENERAL_KEY_DOMAIN_BIT: u32 = 1 << 31;
const PLAYER_KEY_DOMAIN: u32 = 0;
const ENTITY_KEY_DOMAIN: u32 = GENERAL_KEY_DOMAIN_BIT;

/// The fixed native key for the world's one typed properties record.
///
/// General-record keys otherwise identify players and resident entities. This
/// reserved slot has no source path in its body, so a metadata import replaces
/// the previous typed record without retaining source file names or NBT.
pub const WORLD_PROPERTIES_KEY: RecordKey = RecordKey::general(i32::MIN, i32::MIN, u32::MAX);

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

/// One typed native player value that extends a bounded locator.
///
/// The current field group contains only game mode. It remains separate from
/// [`NativePlayerRecord`] so existing locator producers keep their exact
/// contract, while an importer with a complete player root can persist this
/// independently consumable value in the same atomic record replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativePlayerData {
    /// The durable player identity and locator.
    pub locator: NativePlayerRecord,
    /// The saved game mode, or `None` for locator-only records.
    pub game_mode: Option<lodestone_model::GameMode>,
}

impl From<NativePlayerRecord> for NativePlayerData {
    fn from(locator: NativePlayerRecord) -> Self {
        Self {
            locator,
            game_mode: None,
        }
    }
}

/// One bounded native resident-entity pose.
///
/// The UUID and type key are durable identities; position and rotation retain
/// the live IEEE values rather than applying an undocumented fixed-point
/// conversion. This is deliberately not a replacement for an Anvil entity:
/// motion, health, item state, AI state, and opaque fields have no lossless
/// native consumer here and are therefore absent.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeEntityRecord {
    /// The complete durable entity identity.
    pub uuid: [u8; 16],
    /// The canonical type key, never a registry-local numeric ID.
    pub entity_type: lodestone_model::ResourceKey,
    /// The built-in dimension containing the resident entity.
    pub dimension: BuiltinDimension,
    /// Exact feet position.
    pub position: lodestone_model::Vec3,
    /// Exact yaw and pitch.
    pub rotation: lodestone_model::Rotation,
}

/// One complete, currently supported typed general record in native storage.
///
/// This is an export/read boundary, not an opaque general-record passthrough.
/// The returned value has already been checked against its reserved key and
/// decoded through the same bounded readers used by direct lookups. A future
/// general body needs an explicit variant and consumer here before a snapshot
/// can expose it.
#[derive(Clone, Debug, PartialEq)]
pub enum NativeGeneralRecord {
    /// The world's one typed scalar-properties record.
    WorldProperties(WorldProperties),
    /// One bounded player locator plus supported player fields.
    Player(NativePlayerData),
    /// One bounded resident-entity pose.
    Entity(NativeEntityRecord),
}

/// One source-column batch in a reviewed resident-entity import.
///
/// The column and vertical extent remain part of the input, so every pose is
/// checked before a whole-world import opens its one native transaction.
#[derive(Clone, Debug)]
pub struct NativeDirtyEntityChunk {
    /// Source chunk-column X coordinate.
    pub column_x: i32,
    /// Source chunk-column Z coordinate.
    pub column_z: i32,
    /// Inclusive lower bound of the source vertical window.
    pub min_y: i32,
    /// Positive size of the source vertical window in blocks.
    pub height: i32,
    /// Resident entity poses selected from this source column.
    pub entities: Vec<NativeEntityRecord>,
}

/// The complete typed input for one dirty native chunk replacement.
///
/// A native chunk replacement is deliberately one value rather than a family
/// of partial writers. `ChunkColumn` owns blocks, biomes, heightmaps, and
/// resident block entities; `ColumnLight` owns the canonical light sections;
/// and `ScheduledTickHandle` owns the pending block and fluid queues. Requiring
/// all three at this boundary prevents a later save from replacing a complete
/// record with a terrain-only snapshot.
#[derive(Clone, Copy, Debug)]
pub struct NativeDirtyChunkRecord<'a> {
    /// The chunk's horizontal column X coordinate.
    pub column_x: i32,
    /// The chunk's horizontal column Z coordinate.
    pub column_z: i32,
    /// The complete column payload, including typed biome and entity state.
    pub column: &'a crate::chunk::ChunkColumn,
    /// The complete canonical light payload, including boundary sections.
    pub light: &'a lodestone_world::ColumnLight,
    /// The live scheduler whose pending ticks belong to this save snapshot.
    pub scheduled: &'a crate::scheduled_tick::ScheduledTickHandle,
}

impl<'a> NativeDirtyChunkRecord<'a> {
    /// Creates one complete typed dirty-chunk input.
    #[must_use]
    pub const fn new(
        column_x: i32,
        column_z: i32,
        column: &'a crate::chunk::ChunkColumn,
        light: &'a lodestone_world::ColumnLight,
        scheduled: &'a crate::scheduled_tick::ScheduledTickHandle,
    ) -> Self {
        Self {
            column_x,
            column_z,
            column,
            light,
            scheduled,
        }
    }
}

/// Every typed value reconstructed by a native chunk reopen.
///
/// The pending ticks remain as persisted records until the caller explicitly
/// stages them into its live [`crate::scheduled_tick::ScheduledTickHandle`].
/// Keeping both queues on the return value means a caller cannot accidentally
/// lose them by choosing a light-only tuple. [`Self::stage_scheduled_ticks`]
/// performs that handoff while retaining each tick's world-wide insertion
/// order.
#[derive(Debug)]
pub struct NativeChunkRecord {
    /// The reconstructed block, biome, heightmap, and block-entity column.
    pub column: crate::chunk::ChunkColumn,
    /// The reconstructed canonical sky and block light column.
    pub light: lodestone_world::ColumnLight,
    /// Pending typed block ticks owned by this column.
    pub block_scheduled_ticks: Vec<crate::scheduled_tick::PersistedScheduledTick>,
    /// Pending typed fluid ticks owned by this column.
    pub fluid_scheduled_ticks: Vec<crate::scheduled_tick::PersistedScheduledTick>,
}

/// One complete native terrain record together with its recovered coordinate.
///
/// The coordinate comes from the committed native-key index and the record is
/// decoded while that same backend lock is held. This makes the value suitable
/// for a typed export selection without making its caller rediscover and
/// reopen one column at a time.
#[derive(Debug)]
pub struct NativeChunkSnapshot {
    /// The horizontal coordinate selected from the recovered native index.
    pub coordinate: NativeChunkCoordinate,
    /// The complete typed terrain record at [`Self::coordinate`].
    pub record: NativeChunkRecord,
}

impl NativeChunkRecord {
    /// Stages this record's pending ticks into the live scheduler.
    ///
    /// Staging is deferred by
    /// [`crate::scheduled_tick::ScheduledTickHandle::stage_persisted`] so a
    /// caller loading from the tick thread cannot re-enter its queue lock.
    /// The returned count is the number handed to the staging boundary; the
    /// next scheduler access makes both queues visible.
    pub fn stage_scheduled_ticks(
        &self,
        scheduled: &crate::scheduled_tick::ScheduledTickHandle,
    ) -> u64 {
        scheduled.stage_persisted(
            self.block_scheduled_ticks.clone(),
            self.fluid_scheduled_ticks.clone(),
        )
    }
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
    /// An explicitly selected native terrain column was not committed.
    MissingNativeChunk {
        /// The absent native chunk coordinate.
        coordinate: NativeChunkCoordinate,
    },
    /// A native player locator record is malformed, unsupported, or ambiguous.
    Player(PlayerRecordError),
    /// A native resident-entity record is malformed, unsupported, or ambiguous.
    Entity(EntityRecordError),
    /// The one native world-properties record is malformed or unsupported.
    WorldProperties(WorldPropertiesError),
    /// A committed general record cannot be safely exposed as a typed value.
    GeneralRecord(GeneralRecordError),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AnvilDoesNotAcceptTypedRecords => {
                formatter.write_str("the Anvil backend does not accept typed dirty records")
            }
            Self::Native(error) => write!(formatter, "native world storage failed: {error}"),
            Self::Chunk(error) => write!(formatter, "native chunk record failed: {error}"),
            Self::MissingNativeChunk { coordinate } => write!(
                formatter,
                "selected native chunk ({}, {}) is absent",
                coordinate.column_x, coordinate.column_z
            ),
            Self::Player(error) => write!(formatter, "native player record failed: {error}"),
            Self::Entity(error) => write!(formatter, "native entity record failed: {error}"),
            Self::WorldProperties(error) => {
                write!(formatter, "native world-properties record failed: {error}")
            }
            Self::GeneralRecord(error) => {
                write!(formatter, "native general-record snapshot failed: {error}")
            }
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

impl From<EntityRecordError> for Error {
    fn from(error: EntityRecordError) -> Self {
        Self::Entity(error)
    }
}

impl From<WorldPropertiesError> for Error {
    fn from(error: WorldPropertiesError) -> Self {
        Self::WorldProperties(error)
    }
}

impl From<GeneralRecordError> for Error {
    fn from(error: GeneralRecordError) -> Self {
        Self::GeneralRecord(error)
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
    /// A caller requested the fast light-aware reopen for an older record that
    /// did not persist any light state.
    MissingStoredLight,
    /// A stored light layer has a value outside the four-bit range.
    InvalidLightUniform(u32),
    /// A stored light layer has an invalid packed nibble-array length.
    InvalidLightArrayLength(usize),
    /// A source light column does not match the requested block-section extent.
    LightSectionCount { expected: usize, actual: usize },
    /// A stored light section is not the next coordinate in the canonical range.
    UnexpectedLightSectionY { expected: i32, actual: i32 },
    /// A source column names a biome not available in this built-in census.
    UnsupportedBiome(String),
    /// The production world source did not provide its complete derived
    /// motion-blocking heightmap for a dirty column.
    MissingMotionBlockingHeightmap,
    /// The production source could not provide a pending column snapshot.
    SourceSnapshot(String),
    /// The configured protocol did not produce a light column for a pending
    /// production chunk.
    MissingComputedLight,
    /// A stored integer is not one of this format version's biome enum values.
    UnknownBuiltinBiome(i32),
    /// A stored numeric block-state ID is not in this build's registry.
    UnknownBlockStateId(u32),
    /// Packed local palette data is structurally invalid.
    InvalidPackedStates(&'static str),
    /// A scheduled tick action is not one of this build's typed built-ins.
    UnsupportedScheduledTickKind(String),
    /// A stored scheduled-tick enum value has no known meaning.
    UnknownScheduledTickKind(i32),
    /// A stored scheduled-tick priority has no known meaning.
    UnknownScheduledTickPriority(i32),
    /// A stored scheduled tick belongs to a different column than its record.
    ScheduledTickOutsideColumn { x: i32, z: i32, expected_x: i32, expected_z: i32 },
    /// Two stored entries claim the same global insertion position.
    DuplicateScheduledTickOrder(u64),
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
            Self::MissingStoredLight => {
                formatter.write_str("chunk record has no persisted light state")
            }
            Self::InvalidLightUniform(value) => {
                write!(formatter, "light uniform value {value} exceeds the four-bit range")
            }
            Self::InvalidLightArrayLength(actual) => {
                write!(formatter, "expected 2048 light bytes, found {actual}")
            }
            Self::LightSectionCount { expected, actual } => {
                write!(formatter, "expected {expected} light sections, found {actual}")
            }
            Self::UnexpectedLightSectionY { expected, actual } => {
                write!(formatter, "expected light section Y {expected}, found {actual}")
            }
            Self::UnsupportedBiome(name) => write!(formatter, "unsupported built-in biome {name}"),
            Self::MissingMotionBlockingHeightmap => {
                formatter.write_str("dirty production column has no motion-blocking heightmap")
            }
            Self::SourceSnapshot(error) => write!(formatter, "could not snapshot dirty column: {error}"),
            Self::MissingComputedLight => {
                formatter.write_str("protocol did not compute light for dirty production column")
            }
            Self::UnknownBuiltinBiome(id) => write!(formatter, "unknown built-in biome ID {id}"),
            Self::UnknownBlockStateId(id) => write!(formatter, "unknown built-in block-state ID {id}"),
            Self::InvalidPackedStates(reason) => write!(formatter, "invalid packed block states: {reason}"),
            Self::UnsupportedScheduledTickKind(kind) => {
                write!(formatter, "scheduled tick kind has no typed native representation: {kind}")
            }
            Self::UnknownScheduledTickKind(kind) => {
                write!(formatter, "unknown stored scheduled-tick kind {kind}")
            }
            Self::UnknownScheduledTickPriority(priority) => {
                write!(formatter, "unknown stored scheduled-tick priority {priority}")
            }
            Self::ScheduledTickOutsideColumn { x, z, expected_x, expected_z } => write!(
                formatter,
                "scheduled tick at ({x}, {z}) is outside chunk ({expected_x}, {expected_z})"
            ),
            Self::DuplicateScheduledTickOrder(order) => {
                write!(formatter, "duplicate scheduled-tick insertion order {order}")
            }
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

/// A malformed or unsupported native world-properties record.
#[derive(Debug, Eq, PartialEq)]
pub enum WorldPropertiesError {
    /// The envelope has a format version this reader does not understand.
    UnsupportedFormatVersion(u32),
    /// The envelope did not contain a typed general body.
    MissingGeneralBody,
    /// The general body was not a world-properties record.
    MissingWorldPropertiesBody,
    /// World properties have no extension consumer in this format revision.
    UnsupportedExtensions,
}

impl fmt::Display for WorldPropertiesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormatVersion(version) => {
                write!(formatter, "unsupported world-properties record format version {version}")
            }
            Self::MissingGeneralBody => {
                formatter.write_str("record does not contain a general body")
            }
            Self::MissingWorldPropertiesBody => {
                formatter.write_str("general record does not contain world properties")
            }
            Self::UnsupportedExtensions => {
                formatter.write_str("world-properties record carries unsupported extension payloads")
            }
        }
    }
}

impl std::error::Error for WorldPropertiesError {}

/// A committed general record that cannot be routed to one supported typed
/// native value.
#[derive(Debug, PartialEq)]
pub enum GeneralRecordError {
    /// The envelope did not contain a general body.
    MissingGeneralBody,
    /// The general body did not select a known typed native record.
    MissingTypedBody,
    /// The world's scalar record was not stored at its one reserved key.
    WorldPropertiesKey { actual: RecordKey },
    /// A player body's UUID-derived key does not match its stored key.
    PlayerKey { expected: RecordKey, actual: RecordKey },
    /// An entity body's UUID-derived key does not match its stored key.
    EntityKey { expected: RecordKey, actual: RecordKey },
    /// The world-properties body failed its bounded typed reader.
    WorldProperties(WorldPropertiesError),
    /// The player body failed its bounded typed reader.
    Player(PlayerRecordError),
    /// The entity body failed its bounded typed reader.
    Entity(EntityRecordError),
}

impl fmt::Display for GeneralRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingGeneralBody => formatter.write_str("record does not contain a general body"),
            Self::MissingTypedBody => {
                formatter.write_str("general record does not select a supported typed body")
            }
            Self::WorldPropertiesKey { actual } => write!(
                formatter,
                "world-properties record uses {actual:?}, not its reserved key"
            ),
            Self::PlayerKey { expected, actual } => write!(
                formatter,
                "player record key {actual:?} does not match UUID-derived key {expected:?}"
            ),
            Self::EntityKey { expected, actual } => write!(
                formatter,
                "entity record key {actual:?} does not match UUID-derived key {expected:?}"
            ),
            Self::WorldProperties(error) => write!(formatter, "world-properties body failed: {error}"),
            Self::Player(error) => write!(formatter, "player body failed: {error}"),
            Self::Entity(error) => write!(formatter, "entity body failed: {error}"),
        }
    }
}

impl std::error::Error for GeneralRecordError {}

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
    /// The typed game-mode field was neither absent nor a known built-in value.
    UnsupportedGameMode(i32),
    /// This bounded reader cannot preserve opaque player extensions.
    UnsupportedExtensions,
    /// The key's 96-bit UUID prefix resolves to a different complete UUID.
    KeyCollision {
        requested: [u8; 16],
        stored: [u8; 16],
    },
    /// A single requested batch listed one complete UUID twice.
    DuplicateUuid([u8; 16]),
    /// Two UUIDs in one requested batch share the compact native key.
    BatchKeyCollision {
        /// UUID selected first for the compact key.
        first: [u8; 16],
        /// UUID selected later for the same compact key.
        second: [u8; 16],
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
            Self::UnsupportedGameMode(mode) => {
                write!(formatter, "unsupported player game mode {mode}")
            }
            Self::UnsupportedExtensions => {
                formatter.write_str("player record carries unsupported extension payloads")
            }
            Self::KeyCollision { requested, stored } => write!(
                formatter,
                "player key collision: requested UUID {requested:02x?} conflicts with stored UUID {stored:02x?}"
            ),
            Self::DuplicateUuid(uuid) => {
                write!(formatter, "duplicate player UUID in write batch: {uuid:02x?}")
            }
            Self::BatchKeyCollision { first, second } => write!(
                formatter,
                "player batch key collision: UUID {first:02x?} conflicts with UUID {second:02x?}"
            ),
        }
    }
}

impl std::error::Error for PlayerRecordError {}

/// A malformed, unsupported, or ambiguous native resident-entity record.
#[derive(Debug, PartialEq)]
pub enum EntityRecordError {
    /// The envelope has a format version this reader does not understand.
    UnsupportedFormatVersion(u32),
    /// The envelope did not contain a typed general body.
    MissingGeneralBody,
    /// The general body was not a resident-entity record.
    MissingEntityBody,
    /// The stored entity UUID did not have its required 16 bytes.
    InvalidUuidLength { actual: usize },
    /// The stored type is not a canonical resource key.
    InvalidEntityType(String),
    /// The typed record names an unknown or custom dimension.
    UnsupportedDimension(i32),
    /// A position coordinate is NaN or infinite.
    NonFinitePosition,
    /// A rotation angle is NaN or infinite.
    NonFiniteRotation,
    /// A finite coordinate cannot be converted to a server block coordinate.
    CoordinateOutOfRange,
    /// The entity is not resident in the caller's horizontal column.
    OutsideColumn {
        x: i32,
        z: i32,
        expected_x: i32,
        expected_z: i32,
    },
    /// The entity's feet block is outside the caller's vertical extent.
    OutsideExtent { y: i32, min_y: i32, height: i32 },
    /// This bounded reader cannot preserve opaque entity extensions.
    UnsupportedExtensions,
    /// The compact key resolves to a different complete UUID.
    KeyCollision {
        requested: [u8; 16],
        stored: [u8; 16],
    },
    /// A write batch listed the same complete UUID more than once.
    DuplicateUuid([u8; 16]),
}

impl fmt::Display for EntityRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormatVersion(version) => {
                write!(formatter, "unsupported entity record format version {version}")
            }
            Self::MissingGeneralBody => formatter.write_str("record does not contain a general body"),
            Self::MissingEntityBody => {
                formatter.write_str("general record does not contain a resident entity body")
            }
            Self::InvalidUuidLength { actual } => {
                write!(formatter, "expected a 16-byte entity UUID, found {actual} bytes")
            }
            Self::InvalidEntityType(entity_type) => {
                write!(formatter, "entity type is not a valid resource key: {entity_type}")
            }
            Self::UnsupportedDimension(dimension) => {
                write!(formatter, "unsupported built-in entity dimension {dimension}")
            }
            Self::NonFinitePosition => formatter.write_str("entity position contains a non-finite coordinate"),
            Self::NonFiniteRotation => formatter.write_str("entity rotation contains a non-finite angle"),
            Self::CoordinateOutOfRange => {
                formatter.write_str("entity position cannot be converted to a block coordinate")
            }
            Self::OutsideColumn { x, z, expected_x, expected_z } => write!(
                formatter,
                "entity at ({x}, {z}) is outside chunk ({expected_x}, {expected_z})"
            ),
            Self::OutsideExtent { y, min_y, height } => write!(
                formatter,
                "entity at Y {y} is outside [{min_y}, {})",
                min_y.saturating_add(*height)
            ),
            Self::UnsupportedExtensions => {
                formatter.write_str("entity record carries unsupported extension payloads")
            }
            Self::KeyCollision { requested, stored } => write!(
                formatter,
                "entity key collision: requested UUID {requested:02x?} conflicts with stored UUID {stored:02x?}"
            ),
            Self::DuplicateUuid(uuid) => write!(formatter, "duplicate entity UUID in write batch: {uuid:02x?}"),
        }
    }
}

impl std::error::Error for EntityRecordError {}

trait DirtyRecordStore: Send {
    fn write_transaction(&mut self, writes: Vec<RecordWrite>) -> Result<(), StoreError>;
    fn compact(&mut self) -> Result<Compaction, StoreError>;
    fn get(&mut self, key: RecordKey) -> Result<Option<StorageRecord>, StoreError>;
    fn committed_chunk_coordinates(&self) -> Vec<NativeChunkCoordinate>;
    fn committed_general_keys(&self) -> Vec<RecordKey>;
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

    fn compact(&mut self) -> Result<Compaction, StoreError> {
        NativeStore::compact(self)
    }

    fn get(&mut self, key: RecordKey) -> Result<Option<StorageRecord>, StoreError> {
        NativeStore::get(self, key)
    }

    fn committed_chunk_coordinates(&self) -> Vec<NativeChunkCoordinate> {
        NativeStore::committed_chunk_coordinates(self)
    }

    fn committed_general_keys(&self) -> Vec<RecordKey> {
        NativeStore::committed_general_keys(self)
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

    /// Compacts the selected native segment at an explicit maintenance point.
    ///
    /// The caller must ensure no other process has the store open. This method
    /// serializes Lodestone handles through the backend mutex, but it cannot
    /// establish a cross-process maintenance window. Compaction preserves the
    /// recovered latest value for every key in one replacement transaction and
    /// returns the measured segment sizes. The Anvil backend rejects this
    /// native-only operation without touching compatibility files.
    pub fn compact_native(&self) -> Result<Compaction, Error> {
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        native
            .lock()
            .expect("world storage lock poisoned")
            .compact()
            .map_err(Into::into)
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

    /// Snapshots every committed native terrain-column coordinate.
    ///
    /// The native format version selected here has no dimension key, so this
    /// result contains only horizontal columns. The store copies its recovered
    /// latest-record index while holding the backend lock; it does not seek to
    /// or deserialize chunk payloads, and a concurrent writer cannot change
    /// the returned selection after this method returns. Anvil has no matching
    /// typed index and rejects the request instead of implying a world scan.
    pub fn native_chunk_coordinates(&self) -> Result<Vec<NativeChunkCoordinate>, Error> {
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        Ok(native
            .lock()
            .expect("world storage lock poisoned")
            .committed_chunk_coordinates())
    }

    /// Snapshots every committed complete native terrain record in coordinate order.
    ///
    /// `min_y` and `height` remain explicit because version 1 keys and chunk
    /// records do not define a dimension height. The native backend lock stays
    /// held from copying the recovered index through decoding every envelope,
    /// so a concurrent writer cannot replace a discovered column before this
    /// snapshot has decoded it. A malformed, terrain-only, or unsupported
    /// record rejects the entire selection rather than becoming a partial
    /// export candidate. Anvil rejects this native-only request without
    /// scanning compatibility files.
    pub fn native_chunk_records(
        &self,
        min_y: i32,
        height: i32,
    ) -> Result<Vec<NativeChunkSnapshot>, Error> {
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        validate_extent(min_y, height)?;
        let mut native = native.lock().expect("world storage lock poisoned");
        let coordinates = native.committed_chunk_coordinates();
        snapshot_native_chunk_records(native.as_mut(), coordinates, min_y, height)
    }

    /// Snapshots explicitly selected committed terrain records in caller order.
    ///
    /// The native backend lock covers every selected lookup and typed decode,
    /// so a writer cannot replace a selected column between one member of the
    /// batch and the next. The supplied coordinates are deliberately copied
    /// rather than rediscovered: callers that reviewed a narrow selection can
    /// retain that exact set without scanning unrelated terrain. A missing
    /// selected column fails the complete snapshot instead of becoming a
    /// silently smaller export candidate.
    pub fn native_chunk_records_for(
        &self,
        coordinates: &[NativeChunkCoordinate],
        min_y: i32,
        height: i32,
    ) -> Result<Vec<NativeChunkSnapshot>, Error> {
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        validate_extent(min_y, height)?;
        let mut native = native.lock().expect("world storage lock poisoned");
        snapshot_native_chunk_records(
            native.as_mut(),
            coordinates.iter().copied(),
            min_y,
            height,
        )
    }

    /// Snapshots every committed supported native general record in key order.
    ///
    /// The store keeps its lock while it copies the recovered key index and
    /// decodes each selected envelope, so a later writer cannot replace one
    /// record between discovery and decoding. Each body must occupy the exact
    /// key reserved by its typed identity; malformed, extension-bearing, or
    /// unknown bodies fail the entire snapshot rather than becoming an opaque
    /// export candidate. Anvil has no native general-record index and rejects
    /// this request without reading compatibility files.
    pub fn native_general_records(&self) -> Result<Vec<NativeGeneralRecord>, Error> {
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        let mut native = native.lock().expect("world storage lock poisoned");
        native
            .committed_general_keys()
            .into_iter()
            .map(|key| {
                let record = native
                    .get(key)?
                    .expect("a key copied from the native index must remain readable while locked");
                decode_general_record(key, record).map_err(Into::into)
            })
            .collect()
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

    /// Reopens the one typed native world-properties record, if present.
    ///
    /// This is the read boundary for the operator metadata import. The record
    /// carries no extension consumer in format version 1, so any extension
    /// payload fails closed instead of becoming ignored data on reopen.
    pub fn load_world_properties(&self) -> Result<Option<WorldProperties>, Error> {
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        native
            .lock()
            .expect("world storage lock poisoned")
            .get(WORLD_PROPERTIES_KEY)?
            .map(decode_world_properties)
            .transpose()
            .map_err(Into::into)
    }

    /// Saves the world's one independently dirty typed properties record.
    ///
    /// The fixed [`WORLD_PROPERTIES_KEY`] and the generated general-record
    /// envelope are owned here, so a metadata producer cannot accidentally
    /// write a valid world-properties body under an unfindable general key.
    /// `Anvil` rejects this native-only path without writing any compatibility
    /// files.
    pub fn write_dirty_world_properties(
        &self,
        properties: WorldProperties,
    ) -> Result<(), Error> {
        self.write_dirty([RecordWrite::new(
            WORLD_PROPERTIES_KEY,
            encode_world_properties(properties),
        )])
        .map(|_| ())
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
        self.write_dirty_player_data(player.into())
    }

    /// Atomically saves a non-empty batch of independently dirty player
    /// locators.
    ///
    /// All records are encoded and every compact-key collision is checked
    /// before the native transaction begins. This is the boundary for a
    /// reviewed filesystem import: a malformed later player cannot leave an
    /// earlier selected player committed. Empty batches do no I/O.
    pub fn write_dirty_players(
        &self,
        players: impl IntoIterator<Item = NativePlayerRecord>,
    ) -> Result<usize, Error> {
        self.write_dirty_player_data_batch(players.into_iter().map(Into::into))
    }

    /// Saves one native player record with the typed full-player fields this
    /// build can consume, without changing locator-only producers.
    pub fn write_dirty_player_data(&self, player: NativePlayerData) -> Result<(), Error> {
        self.write_dirty_player_data_batch([player]).map(|_| ())
    }

    /// Atomically saves a non-empty batch of typed player records.
    ///
    /// This shares the locator batch's pre-transaction UUID and compact-key
    /// checks. A complete player import therefore cannot commit an earlier
    /// player after a later one is rejected.
    pub fn write_dirty_player_data_batch(
        &self,
        players: impl IntoIterator<Item = NativePlayerData>,
    ) -> Result<usize, Error> {
        let players: Vec<_> = players.into_iter().collect();
        if players.is_empty() {
            return Ok(0);
        }
        let mut requested = HashSet::new();
        let mut keys = BTreeMap::new();
        let mut writes = Vec::with_capacity(players.len());
        for player in players {
            if !requested.insert(player.locator.uuid) {
                return Err(PlayerRecordError::DuplicateUuid(player.locator.uuid).into());
            }
            let key = player_key(player.locator.uuid);
            if let Some(first) = keys.insert(key, player.locator.uuid) {
                return Err(PlayerRecordError::BatchKeyCollision {
                    first,
                    second: player.locator.uuid,
                }
                .into());
            }
            writes.push((key, player.locator.uuid, encode_player(player)?));
        }
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        let mut native = native.lock().expect("world storage lock poisoned");
        for (key, uuid, _) in &writes {
            if let Some(existing) = native.get(*key)? {
                decode_player(*uuid, existing)?;
            }
        }
        let count = writes.len();
        native.write_transaction(
            writes
                .into_iter()
                .map(|(key, _, record)| RecordWrite::new(key, record))
                .collect(),
        )?;
        Ok(count)
    }

    /// Loads one bounded native player locator record by its complete UUID.
    ///
    /// Missing records return `None`. A record whose compact key maps to a
    /// different UUID is an explicit collision error, and extensions or custom
    /// dimensions are refused rather than being silently discarded.
    pub fn load_player(&self, uuid: [u8; 16]) -> Result<Option<NativePlayerRecord>, Error> {
        self.load_player_data(uuid)
            .map(|player| player.map(|player| player.locator))
    }

    /// Loads one native player record including the typed full-player fields
    /// currently represented by this build.
    ///
    /// Locator-only records written before these fields existed remain readable
    /// and report `None` for each absent field.
    pub fn load_player_data(&self, uuid: [u8; 16]) -> Result<Option<NativePlayerData>, Error> {
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

    /// Saves one bounded batch of resident-entity poses for a column.
    ///
    /// Each UUID owns one world-wide compact key, so saving a moved entity
    /// replaces its previous record rather than leaving a stale copy in the
    /// former column. The explicit column and extent are checked against every
    /// pose before any batch reaches storage. This is an opt-in typed producer,
    /// not the Anvil entity save path, and accepts no opaque state.
    pub fn write_dirty_entities(
        &self,
        column_x: i32,
        column_z: i32,
        min_y: i32,
        height: i32,
        entities: impl IntoIterator<Item = NativeEntityRecord>,
    ) -> Result<usize, Error> {
        self.write_dirty_entity_chunks([NativeDirtyEntityChunk {
            column_x,
            column_z,
            min_y,
            height,
            entities: entities.into_iter().collect(),
        }])
    }

    /// Atomically saves every reviewed resident-entity source chunk.
    ///
    /// Every column, pose, UUID and compact key is validated before the one
    /// native transaction begins. This keeps a later corrupt sidecar from
    /// committing an earlier column during a filesystem conversion.
    pub fn write_dirty_entity_chunks(
        &self,
        chunks: impl IntoIterator<Item = NativeDirtyEntityChunk>,
    ) -> Result<usize, Error> {
        let chunks: Vec<_> = chunks.into_iter().collect();
        if chunks.is_empty() {
            return Ok(0);
        }
        let mut seen_uuids = HashSet::new();
        let mut keys = BTreeMap::new();
        let mut writes = Vec::new();
        for chunk in chunks {
            validate_extent(chunk.min_y, chunk.height)?;
            for entity in &chunk.entities {
                if !seen_uuids.insert(entity.uuid) {
                    return Err(EntityRecordError::DuplicateUuid(entity.uuid).into());
                }
                validate_entity_residency(
                    entity,
                    chunk.column_x,
                    chunk.column_z,
                    chunk.min_y,
                    chunk.height,
                )?;
                let key = entity_key(entity.uuid);
                if let Some(stored) = keys.insert(key, entity.uuid) {
                    return Err(EntityRecordError::KeyCollision {
                        requested: entity.uuid,
                        stored,
                    }
                    .into());
                }
                writes.push((key, entity.uuid, encode_entity(entity)?));
            }
        }
        if writes.is_empty() {
            return Ok(0);
        }

        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        let mut native = native.lock().expect("world storage lock poisoned");
        for (key, uuid, _) in &writes {
            if let Some(existing) = native.get(*key)? {
                decode_entity(*uuid, existing)?;
            }
        }
        let count = writes.len();
        native.write_transaction(
            writes
                .into_iter()
                .map(|(key, _, record)| RecordWrite::new(key, record))
                .collect(),
        )?;
        Ok(count)
    }

    /// Loads one resident entity by its complete UUID and verifies it is still
    /// resident in the caller's requested column and vertical extent.
    ///
    /// A missing UUID returns `None`; a malformed type, extension payload,
    /// compact-key collision, or pose outside the requested bounds is an error
    /// rather than a partially restored entity.
    pub fn load_entity(
        &self,
        uuid: [u8; 16],
        column_x: i32,
        column_z: i32,
        min_y: i32,
        height: i32,
    ) -> Result<Option<NativeEntityRecord>, Error> {
        validate_extent(min_y, height)?;
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        let entity = native
            .lock()
            .expect("world storage lock poisoned")
            .get(entity_key(uuid))?
            .map(|record| decode_entity(uuid, record))
            .transpose()?;
        let Some(entity) = entity else {
            return Ok(None);
        };
        validate_entity_residency(&entity, column_x, column_z, min_y, height)?;
        Ok(Some(entity))
    }

    /// Atomically saves one complete dirty chunk through the native typed
    /// record path.
    ///
    /// The input requires block/biome/entity state, canonical light, and both
    /// pending tick queues together. `Anvil` stays unchanged and refuses this
    /// method before inspecting or converting the supplied values.
    pub fn write_dirty_chunk(&self, dirty: NativeDirtyChunkRecord<'_>) -> Result<(), Error> {
        self.write_dirty_chunks(std::iter::once(dirty)).map(|_| ())
    }

    /// Atomically saves a complete batch of dirty native chunk replacements.
    ///
    /// Conversion happens before the single native transaction, so one
    /// incomplete column cannot leave a partially updated batch. The backend
    /// check remains first: Anvil refuses the typed path without inspecting
    /// or converting any input, preserving its established save behavior.
    pub fn write_dirty_chunks<'a>(
        &self,
        dirty: impl IntoIterator<Item = NativeDirtyChunkRecord<'a>>,
    ) -> Result<usize, Error> {
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        let mut writes = Vec::new();
        for dirty in dirty {
            let ticks = dirty
                .scheduled
                .snapshot_column(dirty.column_x, dirty.column_z);
            let record = encode_chunk_with_light(
                dirty.column_x,
                dirty.column_z,
                dirty.column,
                dirty.light,
                Some(ticks),
            )?;
            writes.push(RecordWrite::new(
                RecordKey::chunk(dirty.column_x, dirty.column_z),
                record,
            ));
        }
        if writes.is_empty() {
            return Ok(0);
        }
        let count = writes.len();
        native
            .lock()
            .expect("world storage lock poisoned")
            .write_transaction(writes)?;
        Ok(count)
    }

    /// Reopens one complete typed native chunk record.
    ///
    /// `min_y` and `height` remain an explicit dimension contract because the
    /// version-1 record stores section coordinates, not a dimension definition.
    /// Every stored field is returned in [`NativeChunkRecord`], including both
    /// pending tick queues. A missing light stream is rejected rather than
    /// interpreted as darkness, and malformed or unsupported payloads are
    /// rejected rather than partially restored.
    pub fn load_chunk(
        &self,
        column_x: i32,
        column_z: i32,
        min_y: i32,
        height: i32,
    ) -> Result<Option<NativeChunkRecord>, Error> {
        let Some(native) = &self.native else {
            return Err(Error::AnvilDoesNotAcceptTypedRecords);
        };
        validate_extent(min_y, height)?;
        let record = native
            .lock()
            .expect("world storage lock poisoned")
            .get(RecordKey::chunk(column_x, column_z))?;
        let Some(record) = record else {
            return Ok(None);
        };
        Ok(Some(decode_native_chunk(
            column_x, column_z, min_y, height, record,
        )?))
    }
}

fn snapshot_native_chunk_records(
    native: &mut dyn DirtyRecordStore,
    coordinates: impl IntoIterator<Item = NativeChunkCoordinate>,
    min_y: i32,
    height: i32,
) -> Result<Vec<NativeChunkSnapshot>, Error> {
    coordinates
        .into_iter()
        .map(|coordinate| {
            let record = native
                .get(RecordKey::chunk(coordinate.column_x, coordinate.column_z))?
                .ok_or(Error::MissingNativeChunk { coordinate })?;
            let record = decode_native_chunk(
                coordinate.column_x,
                coordinate.column_z,
                min_y,
                height,
                record,
            )?;
            Ok(NativeChunkSnapshot { coordinate, record })
        })
        .collect()
}

fn decode_native_chunk(
    column_x: i32,
    column_z: i32,
    min_y: i32,
    height: i32,
    record: StorageRecord,
) -> Result<NativeChunkRecord, ChunkRecordError> {
    let (column, (block_scheduled_ticks, fluid_scheduled_ticks), light) =
        decode_chunk(column_x, column_z, min_y, height, record)?;
    let light = light.ok_or(ChunkRecordError::MissingStoredLight)?;
    Ok(NativeChunkRecord {
        column,
        light,
        block_scheduled_ticks,
        fluid_scheduled_ticks,
    })
}

fn encode_chunk(
    column_x: i32,
    column_z: i32,
    column: &crate::chunk::ChunkColumn,
    scheduled_ticks: Option<(Vec<crate::scheduled_tick::PersistedScheduledTick>, Vec<crate::scheduled_tick::PersistedScheduledTick>)>,
) -> Result<StorageRecord, ChunkRecordError> {
    encode_chunk_inner(column_x, column_z, column, None, scheduled_ticks)
}

fn encode_chunk_with_light(
    column_x: i32,
    column_z: i32,
    column: &crate::chunk::ChunkColumn,
    light: &lodestone_world::ColumnLight,
    scheduled_ticks: Option<(Vec<crate::scheduled_tick::PersistedScheduledTick>, Vec<crate::scheduled_tick::PersistedScheduledTick>)>,
) -> Result<StorageRecord, ChunkRecordError> {
    encode_chunk_inner(column_x, column_z, column, Some(light), scheduled_ticks)
}

fn encode_chunk_inner(
    column_x: i32,
    column_z: i32,
    column: &crate::chunk::ChunkColumn,
    light: Option<&lodestone_world::ColumnLight>,
    scheduled_ticks: Option<(Vec<crate::scheduled_tick::PersistedScheduledTick>, Vec<crate::scheduled_tick::PersistedScheduledTick>)>,
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

    let (block_scheduled_ticks, fluid_scheduled_ticks) = scheduled_ticks
        .map(|(block, fluid)| {
            Ok((
                block.into_iter().map(encode_scheduled_tick).collect::<Result<Vec<_>, _>>()?,
                fluid.into_iter().map(encode_scheduled_tick).collect::<Result<Vec<_>, _>>()?,
            ))
        })
        .transpose()?
        .unwrap_or_default();
    let light_sections = light
        .map(|light| encode_light_sections(column, light))
        .transpose()?
        .unwrap_or_default();
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
            block_scheduled_ticks,
            extensions: Vec::new(),
            fluid_scheduled_ticks,
            light_sections,
        })),
    })
}

fn encode_light_sections(
    column: &crate::chunk::ChunkColumn,
    light: &lodestone_world::ColumnLight,
) -> Result<Vec<LightSection>, ChunkRecordError> {
    let expected = column.section_count() + 2;
    if light.light_section_count() != expected {
        return Err(ChunkRecordError::LightSectionCount {
            expected,
            actual: light.light_section_count(),
        });
    }
    let first_y = column.min_y.div_euclid(16) - 1;
    (0..expected)
        .map(|index| {
            Ok(LightSection {
                section_y: first_y + index as i32,
                sky_light: encode_light_data(light.sky(index))?,
                block_light: encode_light_data(light.block(index))?,
            })
        })
        .collect()
}

fn encode_light_data(
    data: &lodestone_world::LightData,
) -> Result<Option<StoredLightData>, ChunkRecordError> {
    let data = match data {
        lodestone_world::LightData::Missing => None,
        lodestone_world::LightData::Uniform(value) => {
            if *value > 15 {
                return Err(ChunkRecordError::InvalidLightUniform(u32::from(*value)));
            }
            Some(light_data::Data::Uniform(u32::from(*value)))
        }
        lodestone_world::LightData::Values(array) => {
            match array.uniform_value() {
                Some(value) => Some(light_data::Data::Uniform(u32::from(value))),
                None => Some(light_data::Data::Values(array.as_bytes().to_vec())),
            }
        }
    };
    Ok(data.map(|data| StoredLightData { data: Some(data) }))
}

fn decode_chunk(
    expected_x: i32,
    expected_z: i32,
    min_y: i32,
    height: i32,
    record: StorageRecord,
) -> Result<(
    crate::chunk::ChunkColumn,
    (
        Vec<crate::scheduled_tick::PersistedScheduledTick>,
        Vec<crate::scheduled_tick::PersistedScheduledTick>,
    ),
    Option<lodestone_world::ColumnLight>,
), ChunkRecordError> {
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
    let light = if chunk.light_sections.is_empty() {
        None
    } else {
        let expected_light_sections = expected_sections + 2;
        if chunk.light_sections.len() != expected_light_sections {
            return Err(ChunkRecordError::LightSectionCount {
                expected: expected_light_sections,
                actual: chunk.light_sections.len(),
            });
        }
        let first_y = min_y.div_euclid(16) - 1;
        let mut light = lodestone_world::ColumnLight::new(expected_sections);
        for (index, section) in chunk.light_sections.iter().enumerate() {
            let expected_section_y = first_y + index as i32;
            if section.section_y != expected_section_y {
                return Err(ChunkRecordError::UnexpectedLightSectionY {
                    expected: expected_section_y,
                    actual: section.section_y,
                });
            }
            *light.sky_mut(index) = decode_light_data(section.sky_light.as_ref())?;
            *light.block_mut(index) = decode_light_data(section.block_light.as_ref())?;
        }
        Some(light)
    };
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
    let scheduled_ticks = decode_scheduled_ticks(expected_x, expected_z, &chunk)?;
    Ok((column, scheduled_ticks, light))
}

fn decode_light_data(
    data: Option<&StoredLightData>,
) -> Result<lodestone_world::LightData, ChunkRecordError> {
    let Some(data) = data else {
        return Ok(lodestone_world::LightData::Missing);
    };
    match data.data.as_ref() {
        None => Ok(lodestone_world::LightData::Missing),
        Some(light_data::Data::Uniform(value)) => {
            if *value > 15 {
                return Err(ChunkRecordError::InvalidLightUniform(*value));
            }
            Ok(lodestone_world::LightData::Uniform(*value as u8))
        }
        Some(light_data::Data::Values(values)) => {
            let array = lodestone_world::NibbleArray::from_bytes(values)
                .map_err(|_| ChunkRecordError::InvalidLightArrayLength(values.len()))?;
            if let Some(value) = array.uniform_value() {
                Ok(lodestone_world::LightData::Uniform(value))
            } else {
                Ok(lodestone_world::LightData::Values(array))
            }
        }
    }
}

fn encode_scheduled_tick(
    tick: crate::scheduled_tick::PersistedScheduledTick,
) -> Result<StoredScheduledTick, ChunkRecordError> {
    Ok(StoredScheduledTick {
        x: tick.pos.0,
        y: tick.pos.1,
        z: tick.pos.2,
        kind: scheduled_tick_kind(&tick.kind)? as i32,
        trigger_tick: tick.trigger_tick,
        priority: stored_tick_priority(tick.priority) as i32,
        insertion_order: tick.insertion_order,
    })
}

fn decode_scheduled_ticks(
    column_x: i32,
    column_z: i32,
    chunk: &ChunkRecord,
) -> Result<
    (
        Vec<crate::scheduled_tick::PersistedScheduledTick>,
        Vec<crate::scheduled_tick::PersistedScheduledTick>,
    ),
    ChunkRecordError,
> {
    let mut block_orders = HashSet::new();
    let mut block = chunk
        .block_scheduled_ticks
        .iter()
        .map(|stored| decode_scheduled_tick(stored, column_x, column_z, &mut block_orders))
        .collect::<Result<Vec<_>, _>>()?;
    let mut fluid_orders = HashSet::new();
    let mut fluid = chunk
        .fluid_scheduled_ticks
        .iter()
        .map(|stored| decode_scheduled_tick(stored, column_x, column_z, &mut fluid_orders))
        .collect::<Result<Vec<_>, _>>()?;
    block.sort_by_key(|tick| tick.insertion_order);
    fluid.sort_by_key(|tick| tick.insertion_order);
    Ok((block, fluid))
}

fn decode_scheduled_tick(
    stored: &StoredScheduledTick,
    column_x: i32,
    column_z: i32,
    insertion_orders: &mut HashSet<u64>,
) -> Result<crate::scheduled_tick::PersistedScheduledTick, ChunkRecordError> {
    if (stored.x.div_euclid(16), stored.z.div_euclid(16)) != (column_x, column_z) {
        return Err(ChunkRecordError::ScheduledTickOutsideColumn {
            x: stored.x,
            z: stored.z,
            expected_x: column_x,
            expected_z: column_z,
        });
    }
    if !insertion_orders.insert(stored.insertion_order) {
        return Err(ChunkRecordError::DuplicateScheduledTickOrder(stored.insertion_order));
    }
    let kind = ScheduledTickKind::try_from(stored.kind)
        .map_err(|_| ChunkRecordError::UnknownScheduledTickKind(stored.kind))?;
    let priority = ScheduledTickPriority::try_from(stored.priority)
        .map_err(|_| ChunkRecordError::UnknownScheduledTickPriority(stored.priority))?;
    Ok(crate::scheduled_tick::PersistedScheduledTick {
        pos: (stored.x, stored.y, stored.z),
        kind: tick_kind_string(kind)?,
        trigger_tick: stored.trigger_tick,
        priority: tick_priority(priority),
        insertion_order: stored.insertion_order,
    })
}

fn scheduled_tick_kind(kind: &str) -> Result<ScheduledTickKind, ChunkRecordError> {
    let kind = match kind {
        crate::fluid::TICK_FLUID => ScheduledTickKind::Fluid,
        crate::redstone::TICK_TORCH => ScheduledTickKind::Torch,
        crate::redstone::TICK_REPEATER => ScheduledTickKind::Repeater,
        crate::redstone::TICK_COMPARATOR => ScheduledTickKind::Comparator,
        crate::redstone::TICK_OBSERVER => ScheduledTickKind::Observer,
        crate::redstone_target::TICK_TARGET_DECAY => ScheduledTickKind::TargetDecay,
        crate::redstone_tripwire::TICK_TRIPWIRE_RECHECK => ScheduledTickKind::TripwireRecheck,
        crate::piston::TICK_PISTON => ScheduledTickKind::Piston,
        crate::gravity_tick::TICK_GRAVITY => ScheduledTickKind::Gravity,
        crate::fire::TICK_FIRE => ScheduledTickKind::Fire,
        crate::mobs::tnt::TICK_TNT_PRIME => ScheduledTickKind::TntPrime,
        crate::command_block::TICK_COMMAND_BLOCK => ScheduledTickKind::CommandBlock,
        crate::hand_use::TICK_BUTTON => ScheduledTickKind::ButtonRelease,
        crate::redstone_dispenser::TICK_DISPENSER_FIRE => ScheduledTickKind::DispenserFire,
        _ => return Err(ChunkRecordError::UnsupportedScheduledTickKind(kind.to_owned())),
    };
    Ok(kind)
}

fn tick_kind_string(kind: ScheduledTickKind) -> Result<String, ChunkRecordError> {
    let kind = match kind {
        ScheduledTickKind::Fluid => crate::fluid::TICK_FLUID,
        ScheduledTickKind::Torch => crate::redstone::TICK_TORCH,
        ScheduledTickKind::Repeater => crate::redstone::TICK_REPEATER,
        ScheduledTickKind::Comparator => crate::redstone::TICK_COMPARATOR,
        ScheduledTickKind::Observer => crate::redstone::TICK_OBSERVER,
        ScheduledTickKind::TargetDecay => crate::redstone_target::TICK_TARGET_DECAY,
        ScheduledTickKind::TripwireRecheck => crate::redstone_tripwire::TICK_TRIPWIRE_RECHECK,
        ScheduledTickKind::Piston => crate::piston::TICK_PISTON,
        ScheduledTickKind::Gravity => crate::gravity_tick::TICK_GRAVITY,
        ScheduledTickKind::Fire => crate::fire::TICK_FIRE,
        ScheduledTickKind::TntPrime => crate::mobs::tnt::TICK_TNT_PRIME,
        ScheduledTickKind::CommandBlock => crate::command_block::TICK_COMMAND_BLOCK,
        ScheduledTickKind::ButtonRelease => crate::hand_use::TICK_BUTTON,
        ScheduledTickKind::DispenserFire => crate::redstone_dispenser::TICK_DISPENSER_FIRE,
        ScheduledTickKind::Unspecified => return Err(ChunkRecordError::UnknownScheduledTickKind(0)),
    };
    Ok(kind.to_owned())
}

fn stored_tick_priority(priority: crate::scheduled_tick::TickPriority) -> ScheduledTickPriority {
    match priority {
        crate::scheduled_tick::TickPriority::ExtremelyHigh => ScheduledTickPriority::ExtremelyHigh,
        crate::scheduled_tick::TickPriority::VeryHigh => ScheduledTickPriority::VeryHigh,
        crate::scheduled_tick::TickPriority::High => ScheduledTickPriority::High,
        crate::scheduled_tick::TickPriority::Normal => ScheduledTickPriority::Normal,
        crate::scheduled_tick::TickPriority::Low => ScheduledTickPriority::Low,
        crate::scheduled_tick::TickPriority::VeryLow => ScheduledTickPriority::VeryLow,
        crate::scheduled_tick::TickPriority::ExtremelyLow => ScheduledTickPriority::ExtremelyLow,
    }
}

fn tick_priority(priority: ScheduledTickPriority) -> crate::scheduled_tick::TickPriority {
    match priority {
        ScheduledTickPriority::ExtremelyHigh => crate::scheduled_tick::TickPriority::ExtremelyHigh,
        ScheduledTickPriority::VeryHigh => crate::scheduled_tick::TickPriority::VeryHigh,
        ScheduledTickPriority::High => crate::scheduled_tick::TickPriority::High,
        ScheduledTickPriority::Normal => crate::scheduled_tick::TickPriority::Normal,
        ScheduledTickPriority::Low => crate::scheduled_tick::TickPriority::Low,
        ScheduledTickPriority::VeryLow => crate::scheduled_tick::TickPriority::VeryLow,
        ScheduledTickPriority::ExtremelyLow => crate::scheduled_tick::TickPriority::ExtremelyLow,
    }
}

fn decode_world_properties(record: StorageRecord) -> Result<WorldProperties, WorldPropertiesError> {
    if record.format_version != FORMAT_VERSION_V1 {
        return Err(WorldPropertiesError::UnsupportedFormatVersion(record.format_version));
    }
    let Some(storage_record::Record::General(general)) = record.record else {
        return Err(WorldPropertiesError::MissingGeneralBody);
    };
    if !general.extensions.is_empty() {
        return Err(WorldPropertiesError::UnsupportedExtensions);
    }
    let Some(general_record::Record::WorldProperties(properties)) = general.record else {
        return Err(WorldPropertiesError::MissingWorldPropertiesBody);
    };
    Ok(properties)
}

fn decode_general_record(
    key: RecordKey,
    record: StorageRecord,
) -> Result<NativeGeneralRecord, GeneralRecordError> {
    let Some(storage_record::Record::General(general)) = record.record.as_ref() else {
        return Err(GeneralRecordError::MissingGeneralBody);
    };
    match general.record.as_ref() {
        Some(general_record::Record::WorldProperties(_)) => {
            if key != WORLD_PROPERTIES_KEY {
                return Err(GeneralRecordError::WorldPropertiesKey { actual: key });
            }
            decode_world_properties(record)
                .map(NativeGeneralRecord::WorldProperties)
                .map_err(GeneralRecordError::WorldProperties)
        }
        Some(general_record::Record::Player(player)) => {
            let actual = player.player_uuid.len();
            let uuid = player
                .player_uuid
                .as_slice()
                .try_into()
                .map_err(|_| GeneralRecordError::Player(PlayerRecordError::InvalidUuidLength {
                    actual,
                }))?;
            let expected = player_key(uuid);
            if key != expected {
                return Err(GeneralRecordError::PlayerKey { expected, actual: key });
            }
            decode_player(uuid, record)
                .map(NativeGeneralRecord::Player)
                .map_err(GeneralRecordError::Player)
        }
        Some(general_record::Record::Entity(entity)) => {
            let actual = entity.entity_uuid.len();
            let uuid = entity
                .entity_uuid
                .as_slice()
                .try_into()
                .map_err(|_| GeneralRecordError::Entity(EntityRecordError::InvalidUuidLength {
                    actual,
                }))?;
            let expected = entity_key(uuid);
            if key != expected {
                return Err(GeneralRecordError::EntityKey { expected, actual: key });
            }
            decode_entity(uuid, record)
                .map(NativeGeneralRecord::Entity)
                .map_err(GeneralRecordError::Entity)
        }
        None => Err(GeneralRecordError::MissingTypedBody),
    }
}

fn encode_world_properties(properties: WorldProperties) -> StorageRecord {
    StorageRecord {
        format_version: FORMAT_VERSION_V1,
        record: Some(storage_record::Record::General(GeneralRecord {
            record: Some(general_record::Record::WorldProperties(properties)),
            extensions: Vec::new(),
        })),
    }
}

fn player_key(uuid: [u8; 16]) -> RecordKey {
    general_uuid_key(uuid, PLAYER_KEY_DOMAIN)
}

/// Builds one compact general-record key in a disjoint typed domain.
///
/// A general key has 96 bits. Its most-significant local-ID bit identifies
/// the record body family, leaving 95 UUID bits for each family. The complete
/// UUID remains in the envelope and is compared on both reads and writes, so
/// the omitted bit cannot turn either an in-family collision or a cross-family
/// identity into a silent replacement.
fn general_uuid_key(uuid: [u8; 16], domain: u32) -> RecordKey {
    debug_assert!(matches!(domain, PLAYER_KEY_DOMAIN | ENTITY_KEY_DOMAIN));
    RecordKey::general(
        i32::from_le_bytes(uuid[..4].try_into().expect("four UUID bytes")),
        i32::from_le_bytes(uuid[4..8].try_into().expect("four UUID bytes")),
        (u32::from_le_bytes(uuid[8..12].try_into().expect("four UUID bytes"))
            & !GENERAL_KEY_DOMAIN_BIT)
            | domain,
    )
}

fn encode_player(player: NativePlayerData) -> Result<StorageRecord, PlayerRecordError> {
    let dimension = player.locator.dimension as i32;
    validate_player_dimension(dimension)?;
    Ok(StorageRecord {
        format_version: FORMAT_VERSION_V1,
        record: Some(storage_record::Record::General(GeneralRecord {
            record: Some(general_record::Record::Player(PlayerRecord {
                player_uuid: player.locator.uuid.to_vec(),
                dimension,
                x_fixed: player.locator.x_fixed,
                y_fixed: player.locator.y_fixed,
                z_fixed: player.locator.z_fixed,
                yaw_millidegrees: player.locator.yaw_millidegrees,
                pitch_millidegrees: player.locator.pitch_millidegrees,
                game_mode: encode_player_game_mode(player.game_mode),
            })),
            extensions: Vec::new(),
        })),
    })
}

fn decode_player(
    requested_uuid: [u8; 16],
    record: StorageRecord,
) -> Result<NativePlayerData, PlayerRecordError> {
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
    Ok(NativePlayerData {
        locator: NativePlayerRecord {
            uuid,
            dimension: BuiltinDimension::try_from(player.dimension)
                .expect("validated built-in player dimension"),
            x_fixed: player.x_fixed,
            y_fixed: player.y_fixed,
            z_fixed: player.z_fixed,
            yaw_millidegrees: player.yaw_millidegrees,
            pitch_millidegrees: player.pitch_millidegrees,
        },
        game_mode: decode_player_game_mode(player.game_mode)?,
    })
}

fn encode_player_game_mode(mode: Option<lodestone_model::GameMode>) -> i32 {
    match mode {
        None => StoredGameMode::Unspecified as i32,
        Some(lodestone_model::GameMode::Survival) => StoredGameMode::Survival as i32,
        Some(lodestone_model::GameMode::Creative) => StoredGameMode::Creative as i32,
        Some(lodestone_model::GameMode::Adventure) => StoredGameMode::Adventure as i32,
        Some(lodestone_model::GameMode::Spectator) => StoredGameMode::Spectator as i32,
    }
}

fn decode_player_game_mode(
    mode: i32,
) -> Result<Option<lodestone_model::GameMode>, PlayerRecordError> {
    match StoredGameMode::try_from(mode) {
        Ok(StoredGameMode::Unspecified) => Ok(None),
        Ok(StoredGameMode::Survival) => Ok(Some(lodestone_model::GameMode::Survival)),
        Ok(StoredGameMode::Creative) => Ok(Some(lodestone_model::GameMode::Creative)),
        Ok(StoredGameMode::Adventure) => Ok(Some(lodestone_model::GameMode::Adventure)),
        Ok(StoredGameMode::Spectator) => Ok(Some(lodestone_model::GameMode::Spectator)),
        Err(_) => Err(PlayerRecordError::UnsupportedGameMode(mode)),
    }
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

fn entity_key(uuid: [u8; 16]) -> RecordKey {
    general_uuid_key(uuid, ENTITY_KEY_DOMAIN)
}

fn encode_entity(entity: &NativeEntityRecord) -> Result<StorageRecord, EntityRecordError> {
    validate_entity_dimension(entity.dimension as i32)?;
    validate_entity_pose(entity.position, entity.rotation)?;
    Ok(StorageRecord {
        format_version: FORMAT_VERSION_V1,
        record: Some(storage_record::Record::General(GeneralRecord {
            record: Some(general_record::Record::Entity(EntityRecord {
                entity_uuid: entity.uuid.to_vec(),
                entity_type: entity.entity_type.to_string(),
                dimension: entity.dimension as i32,
                x: entity.position.x,
                y: entity.position.y,
                z: entity.position.z,
                yaw: entity.rotation.yaw,
                pitch: entity.rotation.pitch,
                ..EntityRecord::default()
            })),
            extensions: Vec::new(),
        })),
    })
}

fn decode_entity(
    requested_uuid: [u8; 16],
    record: StorageRecord,
) -> Result<NativeEntityRecord, EntityRecordError> {
    if record.format_version != FORMAT_VERSION_V1 {
        return Err(EntityRecordError::UnsupportedFormatVersion(record.format_version));
    }
    let Some(storage_record::Record::General(general)) = record.record else {
        return Err(EntityRecordError::MissingGeneralBody);
    };
    if !general.extensions.is_empty() {
        return Err(EntityRecordError::UnsupportedExtensions);
    }
    let Some(general_record::Record::Entity(entity)) = general.record else {
        return Err(EntityRecordError::MissingEntityBody);
    };
    let actual = entity.entity_uuid.len();
    let uuid: [u8; 16] = entity
        .entity_uuid
        .try_into()
        .map_err(|_| EntityRecordError::InvalidUuidLength { actual })?;
    if uuid != requested_uuid {
        return Err(EntityRecordError::KeyCollision {
            requested: requested_uuid,
            stored: uuid,
        });
    }
    let entity_type = entity
        .entity_type
        .parse::<lodestone_model::ResourceKey>()
        .map_err(|_| EntityRecordError::InvalidEntityType(entity.entity_type))?;
    validate_entity_dimension(entity.dimension)?;
    let position = lodestone_model::Vec3::new(entity.x, entity.y, entity.z);
    let rotation = lodestone_model::Rotation::new(entity.yaw, entity.pitch);
    validate_entity_pose(position, rotation)?;
    Ok(NativeEntityRecord {
        uuid,
        entity_type,
        dimension: BuiltinDimension::try_from(entity.dimension)
            .expect("validated built-in entity dimension"),
        position,
        rotation,
    })
}

fn validate_entity_dimension(dimension: i32) -> Result<(), EntityRecordError> {
    match BuiltinDimension::try_from(dimension) {
        Ok(BuiltinDimension::Overworld | BuiltinDimension::Nether | BuiltinDimension::End) => {
            Ok(())
        }
        Ok(BuiltinDimension::Unspecified) | Err(_) => {
            Err(EntityRecordError::UnsupportedDimension(dimension))
        }
    }
}

fn validate_entity_pose(
    position: lodestone_model::Vec3,
    rotation: lodestone_model::Rotation,
) -> Result<(), EntityRecordError> {
    if !position.x.is_finite() || !position.y.is_finite() || !position.z.is_finite() {
        return Err(EntityRecordError::NonFinitePosition);
    }
    if !rotation.yaw.is_finite() || !rotation.pitch.is_finite() {
        return Err(EntityRecordError::NonFiniteRotation);
    }
    Ok(())
}

fn validate_entity_residency(
    entity: &NativeEntityRecord,
    column_x: i32,
    column_z: i32,
    min_y: i32,
    height: i32,
) -> Result<(), EntityRecordError> {
    validate_entity_pose(entity.position, entity.rotation)?;
    let x = entity_block_coordinate(entity.position.x)?;
    let y = entity_block_coordinate(entity.position.y)?;
    let z = entity_block_coordinate(entity.position.z)?;
    if (x.div_euclid(16), z.div_euclid(16)) != (column_x, column_z) {
        return Err(EntityRecordError::OutsideColumn {
            x,
            z,
            expected_x: column_x,
            expected_z: column_z,
        });
    }
    if !(min_y..min_y.saturating_add(height)).contains(&y) {
        return Err(EntityRecordError::OutsideExtent { y, min_y, height });
    }
    Ok(())
}

fn entity_block_coordinate(value: f64) -> Result<i32, EntityRecordError> {
    let floored = value.floor();
    if floored < f64::from(i32::MIN) || floored > f64::from(i32::MAX) {
        return Err(EntityRecordError::CoordinateOutOfRange);
    }
    Ok(floored as i32)
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

    use lodestone_storage::{RecordKey, RecordWrite};
    use lodestone_storage_schema::{ChunkRecord, ChunkSection, StorageRecord, generated::storage_record};

    use super::*;

    #[derive(Debug, Clone)]
    struct RecordingStore(Arc<Mutex<Vec<Vec<RecordWrite>>>>);

    impl DirtyRecordStore for RecordingStore {
        fn write_transaction(&mut self, writes: Vec<RecordWrite>) -> Result<(), StoreError> {
            self.0.lock().expect("recording store lock poisoned").push(writes);
            Ok(())
        }

        fn compact(&mut self) -> Result<Compaction, StoreError> {
            Err(StoreError::Corrupt {
                offset: 0,
                reason: "recording store does not model compaction".to_owned(),
            })
        }

        fn get(&mut self, _key: RecordKey) -> Result<Option<StorageRecord>, StoreError> {
            Ok(None)
        }

        fn committed_chunk_coordinates(&self) -> Vec<NativeChunkCoordinate> {
            Vec::new()
        }

        fn committed_general_keys(&self) -> Vec<RecordKey> {
            Vec::new()
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
                    block_scheduled_ticks: Vec::new(),
                    extensions: Vec::new(),
                    fluid_scheduled_ticks: Vec::new(),
                    light_sections: Vec::new(),
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
    fn player_batch_prepares_every_locator_before_one_transaction() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let storage = WorldStorage {
            backend: WorldStorageBackend::LodestoneNative {
                directory: PathBuf::from("unused-in-fake-store"),
            },
            native: Some(Mutex::new(Box::new(RecordingStore(Arc::clone(&recorded))))),
        };
        let players = [[0x11; 16], [0x22; 16]].map(|uuid| NativePlayerRecord {
            uuid,
            dimension: BuiltinDimension::Overworld,
            x_fixed: 1,
            y_fixed: 2,
            z_fixed: 3,
            yaw_millidegrees: 4,
            pitch_millidegrees: 5,
        });
        assert_eq!(storage.write_dirty_players(players).unwrap(), 2);
        let batches = recorded.lock().expect("recording store lock poisoned");
        assert_eq!(batches.len(), 1, "player import needs one native commit");
        assert_eq!(batches[0].len(), 2, "every preflighted locator is committed together");
    }

    #[test]
    fn anvil_selection_refuses_typed_records_instead_of_discarding_them() {
        let storage = WorldStorage::open(WorldStorageBackend::Anvil).unwrap();
        assert!(matches!(
            storage.write_dirty([chunk(2, 3, 9)]),
            Err(Error::AnvilDoesNotAcceptTypedRecords)
        ));
        let column = crate::chunk::ChunkColumn::new(0, 16);
        let light = lodestone_world::ColumnLight::new(column.section_count());
        let scheduled = crate::scheduled_tick::ScheduledTickHandle::new();
        assert!(matches!(
            storage.write_dirty_chunk(NativeDirtyChunkRecord::new(
                0, 0, &column, &light, &scheduled,
            )),
            Err(Error::AnvilDoesNotAcceptTypedRecords)
        ));
        assert!(matches!(
            storage.load_chunk(0, 0, 1, 0),
            Err(Error::AnvilDoesNotAcceptTypedRecords)
        ));
        assert!(matches!(
            storage.native_chunk_coordinates(),
            Err(Error::AnvilDoesNotAcceptTypedRecords)
        ));
        assert!(matches!(
            storage.native_chunk_records(0, 16),
            Err(Error::AnvilDoesNotAcceptTypedRecords)
        ));
        assert!(matches!(
            storage.native_general_records(),
            Err(Error::AnvilDoesNotAcceptTypedRecords)
        ));
        assert!(matches!(
            storage.compact_native(),
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
        assert!(matches!(
            storage.write_dirty_entities(
                0,
                0,
                0,
                16,
                [NativeEntityRecord {
                    uuid: [2; 16],
                    entity_type: "minecraft:cow".parse().unwrap(),
                    dimension: BuiltinDimension::Overworld,
                    position: lodestone_model::Vec3::new(0.5, 1.0, 0.5),
                    rotation: lodestone_model::Rotation::new(0.0, 0.0),
                }],
            ),
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
            reopened.load_player_data(player.uuid).unwrap(),
            Some(NativePlayerData {
                locator: player,
                game_mode: None,
            }),
            "a locator-only writer must reopen with every locator field and no invented game mode"
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
    fn typed_world_properties_use_the_reserved_key_and_anvil_refuses_them() {
        let properties = WorldProperties {
            game_data_version: GAME_DATA_VERSION,
            seed: -195_764_831,
            spawn_dimension: BuiltinDimension::Overworld as i32,
            spawn_x: 12,
            spawn_y: 64,
            spawn_z: -33,
            day_time: 5_432,
            default_game_mode: StoredGameMode::Adventure as i32,
        };
        let anvil = WorldStorage::open(WorldStorageBackend::Anvil).expect("open Anvil backend");
        assert!(matches!(
            anvil.write_dirty_world_properties(properties.clone()),
            Err(Error::AnvilDoesNotAcceptTypedRecords)
        ));

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-world-properties-{}-{unique}",
            std::process::id()
        ));
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native backend");
        storage
            .write_dirty_world_properties(properties.clone())
            .expect("write typed native world properties");
        drop(storage);

        let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("reopen native backend");
        assert_eq!(
            reopened
                .load_world_properties()
                .expect("read typed native world properties"),
            Some(properties),
            "the typed writer and reader must agree on the one reserved global key"
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
        let mut record = encode_player(player.into()).expect("built-in player encodes");
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
    fn native_player_data_reopens_game_mode_and_refuses_unknown_mode() {
        let player = NativePlayerData {
            locator: NativePlayerRecord {
                uuid: [0x6d; 16],
                dimension: BuiltinDimension::Overworld,
                x_fixed: 1,
                y_fixed: 2,
                z_fixed: 3,
                yaw_millidegrees: 4,
                pitch_millidegrees: 5,
            },
            game_mode: Some(lodestone_model::GameMode::Adventure),
        };
        let record = encode_player(player).expect("typed game mode encodes");
        assert_eq!(
            decode_player(player.locator.uuid, record).expect("known game mode decodes"),
            player
        );

        let mut invalid = encode_player(player).expect("typed game mode encodes again");
        let Some(storage_record::Record::General(general)) = &mut invalid.record else {
            panic!("player encoder must produce a general record");
        };
        let Some(general_record::Record::Player(player)) = &mut general.record else {
            panic!("player encoder must produce a player record");
        };
        player.game_mode = 99;
        assert_eq!(
            decode_player([0x6d; 16], invalid),
            Err(PlayerRecordError::UnsupportedGameMode(99))
        );
    }

    fn native_entity(position: lodestone_model::Vec3) -> NativeEntityRecord {
        NativeEntityRecord {
            uuid: [
                0x71, 0x24, 0x33, 0xa7, 0x9c, 0x42, 0x40, 0x11, 0xb6, 0x85, 0x71, 0x35, 0x0f,
                0xc2, 0xb6, 0x3e,
            ],
            entity_type: "minecraft:cow".parse().expect("valid type key"),
            dimension: BuiltinDimension::Overworld,
            position,
            rotation: lodestone_model::Rotation::new(136.5, -12.25),
        }
    }

    #[test]
    fn native_resident_entity_reopens_with_stable_identity_type_and_pose() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-entity-record-{}-{unique}",
            std::process::id()
        ));
        let entity = native_entity(lodestone_model::Vec3::new(-1.5, 64.25, 31.75));
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native store");
        assert_eq!(
            storage
                .write_dirty_entities(-1, 1, -64, 384, [entity.clone()])
                .expect("write bounded entity pose"),
            1
        );
        drop(storage);

        let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("reopen native store");
        assert_eq!(
            reopened.load_entity(entity.uuid, -1, 1, -64, 384).unwrap(),
            Some(entity.clone()),
            "the second handle must read the durable UUID, type, position, and rotation"
        );
        assert!(
            reopened.load_entity([0x7f; 16], -1, 1, -64, 384).unwrap().is_none(),
            "a different UUID is an independent absence control"
        );
        assert!(matches!(
            reopened.load_entity(entity.uuid, 0, 1, -64, 384),
            Err(Error::Entity(EntityRecordError::OutsideColumn { .. }))
        ));
        drop(reopened);
        std::fs::remove_dir_all(directory).expect("remove native test segment");
    }

    #[test]
    fn native_player_and_entity_with_the_same_uuid_use_separate_typed_keys() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-general-key-domains-{}-{unique}",
            std::process::id()
        ));
        let entity = native_entity(lodestone_model::Vec3::new(0.5, 64.0, 0.5));
        let player = NativePlayerRecord {
            uuid: entity.uuid,
            dimension: BuiltinDimension::Overworld,
            x_fixed: 12_345,
            y_fixed: 64_000,
            z_fixed: -54_321,
            yaw_millidegrees: 90_000,
            pitch_millidegrees: -15_000,
        };
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native store");
        storage
            .write_dirty_player(player)
            .expect("write player under its typed key domain");
        assert_eq!(
            storage
                .write_dirty_entities(0, 0, -64, 384, [entity.clone()])
                .expect("write entity under its distinct typed key domain"),
            1
        );
        drop(storage);

        let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("reopen native store");
        assert_eq!(reopened.load_player(player.uuid).unwrap(), Some(player));
        assert_eq!(
            reopened.load_entity(entity.uuid, 0, 0, -64, 384).unwrap(),
            Some(entity),
            "one UUID must not make a player locator and resident entity alias"
        );
        drop(reopened);
        std::fs::remove_dir_all(directory).expect("remove native test segment");
    }

    #[test]
    fn native_general_snapshot_decodes_reserved_typed_records_in_key_order() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-general-snapshot-{}-{unique}",
            std::process::id()
        ));
        let properties = WorldProperties {
            game_data_version: GAME_DATA_VERSION,
            seed: 12_345,
            spawn_dimension: BuiltinDimension::Overworld as i32,
            spawn_x: 1,
            spawn_y: 64,
            spawn_z: -2,
            day_time: 54_321,
            default_game_mode: StoredGameMode::Survival as i32,
        };
        let player = NativePlayerData {
            locator: NativePlayerRecord {
                uuid: [1; 16],
                dimension: BuiltinDimension::Nether,
                x_fixed: -11,
                y_fixed: 22,
                z_fixed: -33,
                yaw_millidegrees: 44,
                pitch_millidegrees: -55,
            },
            game_mode: Some(lodestone_model::GameMode::Creative),
        };
        let entity = NativeEntityRecord {
            uuid: [2; 16],
            entity_type: "minecraft:cow".parse().expect("valid entity type"),
            dimension: BuiltinDimension::End,
            position: lodestone_model::Vec3::new(32.5, 70.25, -4.75),
            rotation: lodestone_model::Rotation::new(45.0, -20.0),
        };
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native store");
        storage
            .write_dirty_world_properties(properties.clone())
            .expect("write properties");
        storage
            .write_dirty_player_data(player)
            .expect("write player data");
        assert_eq!(
            storage
                .write_dirty_entities(2, -1, 0, 128, [entity.clone()])
                .expect("write entity"),
            1
        );

        assert_eq!(
            storage.native_general_records().expect("decode typed snapshot"),
            [
                NativeGeneralRecord::WorldProperties(properties),
                NativeGeneralRecord::Player(player),
                NativeGeneralRecord::Entity(entity),
            ],
            "the recovered index order must become an ordered, complete typed snapshot"
        );
        drop(storage);
        std::fs::remove_dir_all(directory).expect("remove native test segment");
    }

    #[test]
    fn native_general_snapshot_refuses_a_typed_body_under_the_wrong_key() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-general-snapshot-key-{}-{unique}",
            std::process::id()
        ));
        let player = NativePlayerData {
            locator: NativePlayerRecord {
                uuid: [0x5a; 16],
                dimension: BuiltinDimension::Overworld,
                x_fixed: 0,
                y_fixed: 64,
                z_fixed: 0,
                yaw_millidegrees: 0,
                pitch_millidegrees: 0,
            },
            game_mode: None,
        };
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native store");
        let actual = RecordKey::general(8, 9, 10);
        storage
            .write_dirty([RecordWrite::new(
                actual,
                encode_player(player).expect("player record encodes"),
            )])
            .expect("native store permits general producer-owned keys");

        assert!(matches!(
            storage.native_general_records(),
            Err(Error::GeneralRecord(GeneralRecordError::PlayerKey {
                actual: rejected,
                ..
            })) if rejected == actual
        ));
        drop(storage);
        std::fs::remove_dir_all(directory).expect("remove native test segment");
    }

    #[test]
    fn native_chunk_snapshot_decodes_complete_records_in_coordinate_order() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-chunk-snapshot-{}-{unique}",
            std::process::id()
        ));
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native store");
        let mut later = crate::chunk::ChunkColumn::new(0, 16);
        later.set_block(3, 4, 5, "minecraft:stone");
        let later_light = lodestone_world::ColumnLight::new(later.section_count());
        let mut earlier = crate::chunk::ChunkColumn::new(0, 16);
        earlier.set_block(7, 8, 9, "minecraft:oak_log[axis=z]");
        let mut earlier_light = lodestone_world::ColumnLight::new(earlier.section_count());
        *earlier_light.sky_mut(1) = lodestone_world::LightData::Uniform(13);
        let scheduled = crate::scheduled_tick::ScheduledTickHandle::new();

        assert_eq!(
            storage
                .write_dirty_chunks([
                    NativeDirtyChunkRecord::new(7, -4, &later, &later_light, &scheduled),
                    NativeDirtyChunkRecord::new(-2, 8, &earlier, &earlier_light, &scheduled),
                ])
                .expect("write complete terrain batch"),
            2
        );

        let snapshot = storage
            .native_chunk_records(0, 16)
            .expect("decode complete terrain snapshot");
        assert_eq!(
            snapshot
                .iter()
                .map(|chunk| chunk.coordinate)
                .collect::<Vec<_>>(),
            [
                NativeChunkCoordinate {
                    column_x: -2,
                    column_z: 8,
                },
                NativeChunkCoordinate {
                    column_x: 7,
                    column_z: -4,
                },
            ],
            "the recovered index order must define export order"
        );
        assert_eq!(
            snapshot[0].record.column.block_state(7, 8, 9),
            "minecraft:oak_log[axis=z]"
        );
        assert_eq!(
            snapshot[0].record.light.sky(1),
            &lodestone_world::LightData::Uniform(13),
            "a snapshot returns the complete canonical-light record, not terrain alone"
        );
        assert_eq!(
            snapshot[1].record.column.block_state(3, 4, 5),
            "minecraft:stone"
        );
        drop(storage);
        std::fs::remove_dir_all(directory).expect("remove native test segment");
    }

    #[test]
    fn native_chunk_snapshot_refuses_a_terrain_only_record_before_export() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-chunk-snapshot-incomplete-{}-{unique}",
            std::process::id()
        ));
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native store");
        let complete = crate::chunk::ChunkColumn::new(0, 16);
        let complete_light = lodestone_world::ColumnLight::new(complete.section_count());
        let scheduled = crate::scheduled_tick::ScheduledTickHandle::new();
        storage
            .write_dirty_chunk(NativeDirtyChunkRecord::new(
                -1,
                0,
                &complete,
                &complete_light,
                &scheduled,
            ))
            .expect("write complete first coordinate");
        let incomplete = crate::chunk::ChunkColumn::new(0, 16);
        storage
            .write_dirty([RecordWrite::new(
                RecordKey::chunk(1, 0),
                encode_chunk(1, 0, &incomplete, None).expect("encode terrain-only record"),
            )])
            .expect("storage accepts a legacy terrain-only envelope");

        assert!(matches!(
            storage.native_chunk_records(0, 16),
            Err(Error::Chunk(ChunkRecordError::MissingStoredLight))
        ));
        drop(storage);
        std::fs::remove_dir_all(directory).expect("remove native test segment");
    }

    #[test]
    fn native_resident_entity_rejects_duplicate_identity_and_invalid_residence_before_write() {
        let entity = native_entity(lodestone_model::Vec3::new(0.5, 64.0, 0.5));
        let storage = WorldStorage {
            backend: WorldStorageBackend::LodestoneNative {
                directory: PathBuf::from("unused-in-fake-store"),
            },
            native: Some(Mutex::new(Box::new(RecordingStore(Arc::new(Mutex::new(Vec::new())))))),
        };
        assert!(matches!(
            storage.write_dirty_entities(0, 0, -64, 384, [entity.clone(), entity.clone()]),
            Err(Error::Entity(EntityRecordError::DuplicateUuid(uuid))) if uuid == entity.uuid
        ));
        assert!(matches!(
            storage.write_dirty_entities(0, 0, -64, 384, [native_entity(lodestone_model::Vec3::new(16.0, 64.0, 0.5))]),
            Err(Error::Entity(EntityRecordError::OutsideColumn { .. }))
        ));
        assert!(matches!(
            storage.write_dirty_entities(0, 0, -64, 384, [native_entity(lodestone_model::Vec3::new(0.5, 320.0, 0.5))]),
            Err(Error::Entity(EntityRecordError::OutsideExtent { .. }))
        ));
    }

    #[test]
    fn native_resident_entity_refuses_an_extension_before_it_can_be_dropped() {
        let entity = native_entity(lodestone_model::Vec3::new(0.5, 64.0, 0.5));
        let mut record = encode_entity(&entity).expect("built-in entity encodes");
        let Some(storage_record::Record::General(general)) = &mut record.record else {
            panic!("entity encoder must produce a general record");
        };
        general
            .extensions
            .push(lodestone_storage_schema::ExtensionValue {
                local_id: 1,
                payload: vec![0xde, 0xad],
            });

        assert_eq!(
            decode_entity(entity.uuid, record),
            Err(EntityRecordError::UnsupportedExtensions)
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
        let light = lodestone_world::ColumnLight::new(source.section_count());
        let scheduled = crate::scheduled_tick::ScheduledTickHandle::new();
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
            .write_dirty_chunk(NativeDirtyChunkRecord::new(
                -7, 11, &source, &light, &scheduled,
            ))
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
        assert_eq!(loaded.column.block_state(1, -16, 2), "minecraft:stone");
        assert_eq!(loaded.column.block_state(3, -1, 4), "minecraft:oak_log[axis=x]");
        assert_eq!(loaded.column.block_state(5, 15, 6), "minecraft:water[level=3]");
        assert_eq!(loaded.column.block_state(0, 0, 0), "minecraft:air");
        assert_eq!(
            loaded.column.block_entities(),
            source.block_entities(),
            "resident simulated and opaque block entities survive a native reopen"
        );
        assert_eq!(loaded.light, light);
        assert!(loaded.block_scheduled_ticks.is_empty());
        assert!(loaded.fluid_scheduled_ticks.is_empty());
        assert!(
            reopened.load_chunk(-8, 11, -16, 32).unwrap().is_none(),
            "a distinct key is the independent absence control"
        );
        drop(reopened);
        std::fs::remove_dir_all(directory).expect("remove native test segment");
    }

    #[test]
    fn native_chunk_reopens_canonical_light_without_expanding_uniform_layers() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-chunk-light-{}-{unique}",
            std::process::id()
        ));
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native store");
        let mut source = crate::chunk::ChunkColumn::new(-16, 32);
        source.set_block(1, -16, 2, "minecraft:stone");
        let mut light = lodestone_world::ColumnLight::new(source.section_count());
        let scheduled = crate::scheduled_tick::ScheduledTickHandle::new();
        *light.sky_mut(1) = lodestone_world::LightData::Uniform(15);
        *light.block_mut(1) = lodestone_world::LightData::Uniform(0);
        let mut sky_values = lodestone_world::NibbleArray::filled(0);
        sky_values.set(
            lodestone_world::NibbleArray::index(2, 3, 4),
            9,
        );
        *light.sky_mut(2) = lodestone_world::LightData::Values(sky_values);
        let mut block_values = lodestone_world::NibbleArray::filled(0);
        block_values.set(
            lodestone_world::NibbleArray::index(5, 6, 7),
            12,
        );
        *light.block_mut(2) = lodestone_world::LightData::Values(block_values);

        storage
            .write_dirty_chunk(NativeDirtyChunkRecord::new(
                -3, 8, &source, &light, &scheduled,
            ))
            .expect("write terrain and canonical light");
        drop(storage);

        let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("reopen native store");
        let (loaded, loaded_light) = reopened
            .load_chunk(-3, 8, -16, 32)
            .expect("decode reopened chunk and light")
            .map(|record| (record.column, record.light))
            .expect("stored chunk is present");
        assert_eq!(loaded.block_state(1, -16, 2), "minecraft:stone");
        assert_eq!(loaded_light.sky(1), &lodestone_world::LightData::Uniform(15));
        assert_eq!(loaded_light.block(1), &lodestone_world::LightData::Uniform(0));
        assert_eq!(
            loaded_light.sky(2).get(lodestone_world::NibbleArray::index(2, 3, 4)),
            Some(9)
        );
        assert_eq!(
            loaded_light
                .block(2)
                .get(lodestone_world::NibbleArray::index(5, 6, 7)),
            Some(12)
        );
        drop(reopened);
        std::fs::remove_dir_all(directory).expect("remove native test segment");
    }

    #[test]
    fn native_chunk_reopen_refuses_a_terrain_only_record() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lodestone-native-chunk-missing-light-{}-{unique}",
            std::process::id()
        ));
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native store");
        let source = crate::chunk::ChunkColumn::new(0, 16);
        let record = encode_chunk(0, 0, &source, None).expect("encode terrain-only record");
        storage
            .write_dirty([RecordWrite::new(RecordKey::chunk(0, 0), record)])
            .expect("write legacy terrain-only record");

        assert!(matches!(
            storage.load_chunk(0, 0, 0, 16),
            Err(Error::Chunk(ChunkRecordError::MissingStoredLight))
        ));
        drop(storage);
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
        let light = lodestone_world::ColumnLight::new(source.section_count());
        let scheduled = crate::scheduled_tick::ScheduledTickHandle::new();
        source.set_biome_cell(0, 0, 0, "minecraft:desert");
        source.set_biome_cell(1, 3, 2, "minecraft:deep_dark");
        let mut surface = vec!["minecraft:plains".to_string(); 16];
        surface[5] = "minecraft:cherry_grove".to_string();
        source.set_biome_quarts(&surface);

        storage
            .write_dirty_chunk(NativeDirtyChunkRecord::new(
                0, 0, &source, &light, &scheduled,
            ))
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
        assert_eq!(loaded.column.biome_state_at(0, 0, 0), "minecraft:desert");
        assert_eq!(loaded.column.biome_state_at(4, 15, 8), "minecraft:deep_dark");
        assert_eq!(loaded.column.biome_state(4, 4), "minecraft:cherry_grove");
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
            encode_chunk(0, 0, &source, None),
            Err(ChunkRecordError::BlockEntityNbtPositionMismatch { .. })
        ));
    }

    #[test]
    fn native_chunk_refuses_a_malformed_stored_block_entity_instead_of_dropping_it() {
        let source = crate::chunk::ChunkColumn::new(0, 16);
        let mut record = encode_chunk(0, 0, &source, None).expect("empty terrain encodes");
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
