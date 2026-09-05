//! Authorization-gated conversion of one Anvil world-properties or chunk record.
//!
//! This is deliberately a small consumer of [`crate::world_storage::WorldStorage`],
//! not a world walker. It reads the two source records that carry the native
//! world's supported metadata, reruns the payload-free preflight, and emits
//! one typed general record. It also has one field-bounded chunk consumer:
//! block states, biomes, the motion-blocking heightmap, and per-section light
//! are mapped into one complete native chunk input while entities, ticks,
//! structures, and other unsupported payloads are reported and dropped.
//! Players, entities, auxiliary files, and a filesystem walker remain on their
//! existing Anvil paths until each has a lossless native destination.

use lodestone_anvil::import_preflight::{
    ImportAuthorization, LossDecision, PreflightReport,
};
use lodestone_anvil::{level_dat, world_gen_settings};
use lodestone_core::{Nbt, Reader, read_named_nbt};
use lodestone_storage::{RecordKey, RecordWrite};
use lodestone_storage_schema::{
    BuiltinDimension, FORMAT_VERSION_V1, GameMode, GeneralRecord, StorageRecord,
    WorldProperties,
    generated::{general_record, storage_record},
};
use lodestone_world::{ColumnLight, Heightmap, LightData, NibbleArray};

use crate::{chunk_nbt, scheduled_tick, world_storage};

/// The fixed key for the world's one native-properties record.
///
/// General record keys are otherwise owned by players and entities. Keeping
/// this key in a reserved coordinate/local-id slot makes this bounded import
/// replaceable and avoids retaining a source filename or NBT path in native
/// storage.
pub const WORLD_PROPERTIES_KEY: RecordKey = RecordKey::general(
    i32::MIN,
    i32::MIN,
    u32::MAX,
);

/// The result of importing one bounded chunk.
///
/// `report.unsupported()` is the authoritative list of source payloads that
/// were intentionally omitted from the native record. Keeping the report next
/// to the write count makes a successful lossy conversion observable without
/// retaining any unsupported NBT values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkImportResult {
    /// The field-level preflight used to authorize this write.
    pub report: PreflightReport,
    /// Number of native records committed; this consumer writes exactly one.
    pub records_written: usize,
}

/// A conversion failure before or at the native backend boundary.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The caller did not provide the explicit preflight decision required by
    /// a potentially lossy conversion.
    #[error("Anvil import requires an explicit ImportAuthorization")]
    MissingAuthorization,
    /// The supplied decision does not permit conversion.
    #[error("Anvil import authorization does not permit conversion: {authorization:?}")]
    AuthorizationDenied {
        /// The decision the caller supplied.
        authorization: ImportAuthorization,
    },
    /// The decision came from a different source/report, or its acknowledged
    /// loss count no longer matches the source being converted.
    #[error(
        "Anvil import authorization does not match this source: supplied {supplied:?}, required {required:?}"
    )]
    AuthorizationMismatch {
        /// The supplied decision.
        supplied: ImportAuthorization,
        /// The decision for this source when ordinary loss is accepted.
        required: ImportAuthorization,
    },
    /// The source's `level.dat` could not provide a field preflight declared
    /// supported.
    #[error("level.dat field {field} was not available after preflight")]
    MissingLevelField {
        /// The source field that disappeared or changed shape.
        field: &'static str,
    },
    /// The Anvil level wrapper rejected a required field.
    #[error("level.dat read error: {0}")]
    LevelDat(#[source] lodestone_anvil::Error),
    /// The Anvil world-generation wrapper rejected a required field.
    #[error("world-generation settings read error: {0}")]
    WorldGenSettings(#[source] lodestone_anvil::Error),
    /// The spawn dimension is outside the three built-in native dimensions.
    #[error("unsupported spawn dimension {0:?}")]
    UnsupportedSpawnDimension(String),
    /// The selected backend refused or failed the native record write.
    #[error("native world storage failed: {0}")]
    Storage(#[source] world_storage::Error),
    /// The supplied chunk bytes were not one complete named-NBT root.
    #[error("chunk NBT read error: {0}")]
    ChunkNbt(#[source] lodestone_core::Error),
    /// The named-NBT root carried a non-empty name.
    #[error("chunk NBT root name must be empty, found {0:?}")]
    ChunkRootName(String),
    /// The supported terrain section mapping rejected the source tree.
    #[error("chunk conversion error: {0}")]
    Chunk(#[source] chunk_nbt::Error),
    /// The supported heightmap payload did not match its fixed packed shape.
    #[error("chunk heightmap error: {0}")]
    Heightmap(#[source] lodestone_world::WorldError),
    /// A source light array was not one complete 16³ nibble layer.
    #[error("chunk light field {field} has {actual} bytes; expected 2048")]
    InvalidLight { field: String, actual: usize },
    /// The source chunk's coordinate fields disagree with the region key.
    #[error("chunk coordinate field {field} is not ({expected}), found {actual}")]
    ChunkCoordinateMismatch {
        /// The coordinate field that disagreed.
        field: &'static str,
        /// Coordinate supplied by the region key.
        expected: i32,
        /// Coordinate stored in the NBT root.
        actual: i32,
    },
    /// The importer has no dimension-specific native chunk contract for this
    /// source identifier yet.
    #[error("unsupported chunk dimension {0:?}")]
    UnsupportedChunkDimension(String),
    /// A packed heightmap value cannot fit the native u16 representation.
    #[error("chunk heightmap value {value} at index {index} exceeds u16")]
    HeightmapValue { index: usize, value: u32 },
    /// The requested native chunk extent is not a positive section-aligned
    /// window.
    #[error("invalid chunk extent min_y={min_y}, height={height}")]
    InvalidChunkExtent { min_y: i32, height: i32 },
}

/// Converts and commits exactly one native world-properties record.
///
/// The source values are borrowed and never modified. Before constructing the
/// native record, this function rebuilds the payload-free preflight from both
/// source wrappers and requires `authorization` to equal the resulting
/// `ProceedAndDiscardUnsupported` authorization. That check prevents a stale
/// loss acknowledgement from silently widening the conversion boundary.
/// Unsupported source fields are absent from the generated record; they are
/// not retained as extensions or opaque NBT.
///
/// `None`, `Aborted`, and `Blocked` all refuse conversion without touching the
/// selected backend. The one successful write returns `1`.
pub fn import_world_properties(
    storage: &world_storage::WorldStorage,
    level: &level_dat::LevelDat,
    settings: &world_gen_settings::WorldGenSettings,
    authorization: Option<ImportAuthorization>,
) -> Result<usize, Error> {
    let Some(authorization) = authorization else {
        return Err(Error::MissingAuthorization);
    };
    if !authorization.permits_conversion() {
        return Err(Error::AuthorizationDenied { authorization });
    }

    let report = preflight(level, settings);
    let required = report.decide(LossDecision::ProceedAndDiscardUnsupported);
    if authorization != required {
        return Err(Error::AuthorizationMismatch {
            supplied: authorization,
            required,
        });
    }

    let record = world_properties_record(level, settings)?;
    storage
        .write_dirty([RecordWrite::new(WORLD_PROPERTIES_KEY, record)])
        .map_err(Error::Storage)
}

/// Builds the field-level authorization report for one chunk NBT root.
///
/// The report is payload-free. It records the terrain, biome, heightmap, and
/// light fields that this consumer can map, plus every unsupported top-level or
/// section field that a lossy conversion would omit.
#[must_use]
pub fn preflight_chunk(
    dimension: impl Into<String>,
    column_x: i32,
    column_z: i32,
    chunk: &Nbt,
) -> PreflightReport {
    let mut builder = PreflightReport::builder();
    builder.inspect_native_chunk(dimension, column_x, column_z, chunk);
    builder.finish()
}

/// Converts and commits exactly one authorized Anvil chunk NBT root.
///
/// The source root is borrowed and the unsupported block-entity, tick,
/// structure, status, and auxiliary fields remain absent from the native
/// record. The returned report is the same report whose accepted-loss count
/// authorized the write. The empty scheduler supplied to
/// [`world_storage::NativeDirtyChunkRecord`] is deliberate: source tick lists
/// are reported as dropped until a typed tick consumer is added.
pub fn import_chunk(
    storage: &world_storage::WorldStorage,
    dimension: impl Into<String>,
    column_x: i32,
    column_z: i32,
    chunk: &Nbt,
    min_y: i32,
    height: i32,
    authorization: Option<ImportAuthorization>,
) -> Result<ChunkImportResult, Error> {
    let dimension = dimension.into();
    if height <= 0 || min_y.rem_euclid(16) != 0 {
        return Err(Error::InvalidChunkExtent { min_y, height });
    }
    let Some(authorization) = authorization else {
        return Err(Error::MissingAuthorization);
    };
    if !authorization.permits_conversion() {
        return Err(Error::AuthorizationDenied { authorization });
    }

    let report = preflight_chunk(&dimension, column_x, column_z, chunk);
    let required = report.decide(LossDecision::ProceedAndDiscardUnsupported);
    if authorization != required {
        return Err(Error::AuthorizationMismatch {
            supplied: authorization,
            required,
        });
    }
    if !is_builtin_dimension(&dimension) {
        return Err(Error::UnsupportedChunkDimension(dimension));
    }

    let actual_x = chunk_int(chunk, "xPos")?;
    if actual_x != column_x {
        return Err(Error::ChunkCoordinateMismatch {
            field: "xPos",
            expected: column_x,
            actual: actual_x,
        });
    }
    let actual_z = chunk_int(chunk, "zPos")?;
    if actual_z != column_z {
        return Err(Error::ChunkCoordinateMismatch {
            field: "zPos",
            expected: column_z,
            actual: actual_z,
        });
    }

    // `column_from_nbt` is the existing, version-free Anvil chunk decoder. It
    // restores only the block/biome state and intentionally does not attach
    // block entities, structures, or source tick queues.
    let mut column = chunk_nbt::column_from_nbt(chunk, min_y, height).map_err(Error::Chunk)?;
    if let Some(heights) = motion_blocking_from_nbt(chunk, height)? {
        column.set_motion_blocking(heights);
    }
    let light = light_from_nbt(chunk, min_y, column.section_count())?;
    let scheduled = scheduled_tick::ScheduledTickHandle::new();
    let dirty = world_storage::NativeDirtyChunkRecord::new(
        column_x,
        column_z,
        &column,
        &light,
        &scheduled,
    );
    storage.write_dirty_chunk(dirty).map_err(Error::Storage)?;
    Ok(ChunkImportResult {
        report,
        records_written: 1,
    })
}

/// Decodes one complete named-NBT chunk root and passes it to [`import_chunk`].
///
/// This helper consumes no filesystem paths; a region caller supplies bytes it
/// has already selected. A trailing byte or non-empty root name is rejected so
/// a concatenated or differently framed payload cannot be imported as a valid
/// chunk.
pub fn import_chunk_bytes(
    storage: &world_storage::WorldStorage,
    dimension: impl Into<String>,
    column_x: i32,
    column_z: i32,
    bytes: &[u8],
    min_y: i32,
    height: i32,
    authorization: Option<ImportAuthorization>,
) -> Result<ChunkImportResult, Error> {
    let mut reader = Reader::new(bytes);
    let (name, chunk) = read_named_nbt(&mut reader).map_err(Error::ChunkNbt)?;
    if !name.is_empty() {
        return Err(Error::ChunkRootName(name));
    }
    reader.ensure_empty().map_err(Error::ChunkNbt)?;
    import_chunk(
        storage,
        dimension,
        column_x,
        column_z,
        &chunk,
        min_y,
        height,
        authorization,
    )
}

fn is_builtin_dimension(dimension: &str) -> bool {
    matches!(
        dimension,
        "minecraft:overworld" | "minecraft:the_nether" | "minecraft:the_end"
    )
}

fn nbt_field<'a>(root: &'a Nbt, name: &str) -> Option<&'a Nbt> {
    let Nbt::Compound(fields) = root else {
        return None;
    };
    fields.iter().find(|(field, _)| field == name).map(|(_, value)| value)
}

fn chunk_int(root: &Nbt, name: &'static str) -> Result<i32, Error> {
    match nbt_field(root, name) {
        Some(Nbt::Int(value)) => Ok(*value),
        _ => Err(Error::Chunk(chunk_nbt::Error::BadField {
            field: name.to_owned(),
        })),
    }
}

fn motion_blocking_from_nbt(root: &Nbt, height: i32) -> Result<Option<[u16; 256]>, Error> {
    let Some(heightmaps) = nbt_field(root, "Heightmaps") else {
        return Ok(None);
    };
    let Nbt::Compound(fields) = heightmaps else {
        return Err(Error::Chunk(chunk_nbt::Error::BadField {
            field: "Heightmaps".to_owned(),
        }));
    };
    let Some(value) = fields
        .iter()
        .find(|(name, _)| name == "MOTION_BLOCKING")
        .map(|(_, value)| value)
    else {
        return Ok(None);
    };
    let Nbt::LongArray(values) = value else {
        return Err(Error::Chunk(chunk_nbt::Error::BadField {
            field: "Heightmaps.MOTION_BLOCKING".to_owned(),
        }));
    };
    let map = Heightmap::from_longs(
        u32::try_from(height).expect("positive chunk height checked above"),
        values.iter().map(|value| *value as u64).collect(),
    )
    .map_err(Error::Heightmap)?;
    let mut heights = [0u16; 256];
    for z in 0..16 {
        for x in 0..16 {
            let index = Heightmap::index(x, z);
            let value = map.get(x, z);
            heights[index] = u16::try_from(value)
                .map_err(|_| Error::HeightmapValue { index, value })?;
        }
    }
    Ok(Some(heights))
}

fn light_from_nbt(root: &Nbt, min_y: i32, section_count: usize) -> Result<ColumnLight, Error> {
    let mut light = ColumnLight::new(section_count);
    let Some(Nbt::List { elements, .. }) = nbt_field(root, "sections") else {
        return Err(Error::Chunk(chunk_nbt::Error::BadField {
            field: "sections".to_owned(),
        }));
    };
    let source_min_section = min_y.div_euclid(16);
    for (index, section) in elements.iter().enumerate() {
        let Some(Nbt::Byte(section_y)) = nbt_field(section, "Y") else {
            continue;
        };
        let light_index = i32::from(*section_y) - source_min_section + 1;
        if !(0..light.light_section_count() as i32).contains(&light_index) {
            continue;
        }
        let light_index = light_index as usize;
        for (name, destination) in [("SkyLight", true), ("BlockLight", false)] {
            let Some(value) = nbt_field(section, name) else {
                continue;
            };
            let Nbt::ByteArray(bytes) = value else {
                return Err(Error::InvalidLight {
                    field: format!("sections[{index}].{name}"),
                    actual: 0,
                });
            };
            if bytes.len() != 2048 {
                return Err(Error::InvalidLight {
                    field: format!("sections[{index}].{name}"),
                    actual: bytes.len(),
                });
            }
            let bytes: Vec<u8> = bytes.iter().map(|value| *value as u8).collect();
            let array = NibbleArray::from_bytes(&bytes).map_err(|_| Error::InvalidLight {
                field: format!("sections[{index}].{name}"),
                actual: bytes.len(),
            })?;
            let data = match array.uniform_value() {
                Some(value) => LightData::Uniform(value),
                None => LightData::Values(array),
            };
            if destination {
                *light.sky_mut(light_index) = data;
            } else {
                *light.block_mut(light_index) = data;
            }
        }
    }
    Ok(light)
}

fn preflight(
    level: &level_dat::LevelDat,
    settings: &world_gen_settings::WorldGenSettings,
) -> PreflightReport {
    let mut builder = PreflightReport::builder();
    builder.inspect_level_dat(level);
    builder.inspect_world_gen_settings(settings);
    builder.finish()
}

fn world_properties_record(
    level: &level_dat::LevelDat,
    settings: &world_gen_settings::WorldGenSettings,
) -> Result<StorageRecord, Error> {
    let game_data_version = u32::try_from(
        level
            .data_version()
            .map_err(Error::LevelDat)?,
    )
    .map_err(|_| Error::MissingLevelField {
        field: "Data.DataVersion",
    })?;
    let default_game_mode = game_mode(
        level
            .game_type()
            .ok_or(Error::MissingLevelField { field: "Data.GameType" })?,
    )?;
    let spawn = level
        .spawn()
        .ok_or(Error::MissingLevelField { field: "Data.spawn" })?;
    let spawn_dimension = dimension(&spawn.dimension)?;
    let seed = settings
        .seed()
        .map_err(Error::WorldGenSettings)?;
    // `Time` is total world age in this source format, not a dimension day
    // clock. It is currently classified as unsupported and therefore stays at
    // the native record's neutral default until a clock consumer exists.
    let properties = WorldProperties {
        game_data_version,
        seed,
        spawn_dimension: spawn_dimension as i32,
        spawn_x: spawn.pos[0],
        spawn_y: spawn.pos[1],
        spawn_z: spawn.pos[2],
        day_time: 0,
        default_game_mode: default_game_mode as i32,
    };
    Ok(StorageRecord {
        format_version: FORMAT_VERSION_V1,
        record: Some(storage_record::Record::General(GeneralRecord {
            record: Some(general_record::Record::WorldProperties(properties)),
            extensions: Vec::new(),
        })),
    })
}

fn game_mode(value: i32) -> Result<GameMode, Error> {
    match value {
        0 => Ok(GameMode::Survival),
        1 => Ok(GameMode::Creative),
        2 => Ok(GameMode::Adventure),
        3 => Ok(GameMode::Spectator),
        _ => Err(Error::MissingLevelField {
            field: "Data.GameType",
        }),
    }
}

fn dimension(value: &str) -> Result<BuiltinDimension, Error> {
    match value {
        "minecraft:overworld" => Ok(BuiltinDimension::Overworld),
        "minecraft:the_nether" => Ok(BuiltinDimension::Nether),
        "minecraft:the_end" => Ok(BuiltinDimension::End),
        other => Err(Error::UnsupportedSpawnDimension(other.to_owned())),
    }
}
