//! Authorization-gated conversion of one Anvil world-properties record.
//!
//! This is deliberately a small consumer of [`crate::world_storage::WorldStorage`],
//! not a world walker. It reads the two source records that carry the native
//! world's supported metadata, reruns the payload-free preflight, and emits
//! one typed general record. Chunks, players, entities, and auxiliary files
//! remain on their existing Anvil paths until each has a lossless native
//! destination.

use lodestone_anvil::import_preflight::{
    ImportAuthorization, LossDecision, PreflightReport,
};
use lodestone_anvil::{level_dat, world_gen_settings};
use lodestone_storage::{RecordKey, RecordWrite};
use lodestone_storage_schema::{
    BuiltinDimension, FORMAT_VERSION_V1, GameMode, GeneralRecord, StorageRecord,
    WorldProperties,
    generated::{general_record, storage_record},
};

use crate::world_storage;

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
