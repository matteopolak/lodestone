//! Staged export of typed native world properties into Anvil metadata.
//!
//! The native schema owns only the values the server consumes. A caller must
//! therefore provide a compatible Anvil metadata template whose complete
//! world-generation settings tree is retained while the typed seed is
//! replaced. The settings file publishes first and `level.dat` publishes last
//! as the recognition marker for the converted world.

use std::{
    fs,
    path::{Path, PathBuf},
};

use lodestone_anvil::{level_dat, world_gen_settings};
use lodestone_core::Nbt;
use lodestone_storage_schema::{BuiltinDimension, GameMode, WorldProperties};

use crate::world_storage::WorldStorage;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetadataExportResult {
    /// Game data version written to the Anvil metadata pair.
    pub game_data_version: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("native metadata snapshot failed: {0}")]
    Storage(#[source] crate::world_storage::Error),
    #[error("native storage has no typed world-properties record")]
    MissingProperties,
    #[error("native day_time {value} cannot be exported until the dimension-clock file is modelled")]
    UnsupportedDayTime { value: u64 },
    #[error("metadata template has no complete dimensions tree: {path}")]
    IncompleteTemplate { path: PathBuf },
    #[error("Anvil metadata export destination is not an existing directory: {path}")]
    MissingDestination { path: PathBuf },
    #[error("Anvil metadata export destination already has {path}")]
    DestinationExists { path: PathBuf },
    #[error("Anvil metadata export staging path already exists: {path}")]
    StagingExists { path: PathBuf },
    #[error("native metadata contains unsupported enum value for {field}: {value}")]
    UnsupportedEnum { field: &'static str, value: i32 },
    #[error("native game data version {actual} does not match the template version {template}")]
    VersionMismatch { actual: u32, template: i32 },
    #[error("Anvil metadata operation failed: {0}")]
    Anvil(#[source] lodestone_anvil::Error),
    #[error("Anvil metadata filesystem operation failed: {0}")]
    Io(#[source] std::io::Error),
}

/// Exports one typed native world-properties record through a complete Anvil template.
pub fn export_metadata(
    storage: &WorldStorage,
    template: &Path,
    destination: &Path,
    world_name: &str,
    last_played_millis: i64,
) -> Result<MetadataExportResult, Error> {
    let properties = storage
        .load_world_properties()
        .map_err(Error::Storage)?
        .ok_or(Error::MissingProperties)?;
    if properties.day_time != 0 {
        return Err(Error::UnsupportedDayTime { value: properties.day_time });
    }
    if !destination.is_dir() {
        return Err(Error::MissingDestination { path: destination.to_owned() });
    }
    let target_level = level_dat::path_in(destination);
    let target_settings = world_gen_settings::path_in(destination);
    for target in [&target_level, &target_settings] {
        if target.exists() {
            return Err(Error::DestinationExists { path: target.clone() });
        }
    }

    let mut level =
        level_dat::read_from_file(&level_dat::path_in(template)).map_err(Error::Anvil)?;
    let mut settings =
        world_gen_settings::read_from_file(&world_gen_settings::path_in(template))
            .map_err(Error::Anvil)?;
    if !settings.has_dimensions() {
        return Err(Error::IncompleteTemplate { path: template.to_owned() });
    }
    let template_version = level.data_version().map_err(Error::Anvil)?;
    let settings_version = settings.data_version().map_err(Error::Anvil)?;
    if settings_version != template_version
        || u32::try_from(template_version).ok() != Some(properties.game_data_version)
    {
        return Err(Error::VersionMismatch {
            actual: properties.game_data_version,
            template: template_version,
        });
    }
    apply_properties(&mut level, &mut settings, &properties, world_name, last_played_millis)?;

    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let name = destination.file_name().and_then(|name| name.to_str()).unwrap_or("world");
    let staging = parent.join(format!(".{name}.lodestone-metadata-exporting"));
    if staging.exists() {
        return Err(Error::StagingExists { path: staging });
    }
    let result = (|| {
        fs::create_dir_all(&staging).map_err(Error::Io)?;
        level_dat::write_to_file(&level, &level_dat::path_in(&staging)).map_err(Error::Anvil)?;
        world_gen_settings::write_to_file(&settings, &world_gen_settings::path_in(&staging))
            .map_err(Error::Anvil)?;
        if let Some(parent) = target_settings.parent() {
            fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        fs::rename(world_gen_settings::path_in(&staging), &target_settings).map_err(Error::Io)?;
        fs::rename(level_dat::path_in(&staging), &target_level).map_err(Error::Io)?;
        Ok(MetadataExportResult {
            game_data_version: properties.game_data_version,
        })
    })();
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(Error::Io)?;
    }
    result
}

fn apply_properties(
    level: &mut level_dat::LevelDat,
    settings: &mut world_gen_settings::WorldGenSettings,
    properties: &WorldProperties,
    world_name: &str,
    last_played_millis: i64,
) -> Result<(), Error> {
    let dimension = match BuiltinDimension::try_from(properties.spawn_dimension) {
        Ok(BuiltinDimension::Overworld) => "minecraft:overworld",
        Ok(BuiltinDimension::Nether) => "minecraft:the_nether",
        Ok(BuiltinDimension::End) => "minecraft:the_end",
        _ => {
            return Err(Error::UnsupportedEnum {
                field: "spawn_dimension",
                value: properties.spawn_dimension,
            });
        }
    };
    let game_mode = match GameMode::try_from(properties.default_game_mode) {
        Ok(GameMode::Survival) => 0,
        Ok(GameMode::Creative) => 1,
        Ok(GameMode::Adventure) => 2,
        Ok(GameMode::Spectator) => 3,
        _ => {
            return Err(Error::UnsupportedEnum {
                field: "default_game_mode",
                value: properties.default_game_mode,
            });
        }
    };
    level.set_data_field("LevelName", Nbt::String(world_name.to_owned())).map_err(Error::Anvil)?;
    level.set_data_field("GameType", Nbt::Int(game_mode)).map_err(Error::Anvil)?;
    level.set_time(0).map_err(Error::Anvil)?;
    level.set_last_played(last_played_millis).map_err(Error::Anvil)?;
    level.set_spawn(&level_dat::Spawn {
        pos: [properties.spawn_x, properties.spawn_y, properties.spawn_z],
        yaw: 0.0,
        pitch: 0.0,
        dimension: dimension.to_owned(),
    }).map_err(Error::Anvil)?;
    settings.set_seed(properties.seed).map_err(Error::Anvil)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_storage::WorldStorageBackend;

    fn template(root: &Path) {
        fs::create_dir_all(root).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../lodestone-anvil/tests/support/level_dat_26_2_vanilla.dat"),
            level_dat::path_in(root),
        ).unwrap();
        let target = world_gen_settings::path_in(root);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../lodestone-anvil/tests/support/world_gen_settings_26_2_vanilla.dat"),
            target,
        ).unwrap();
    }

    fn properties(day_time: u64) -> WorldProperties {
        WorldProperties {
            game_data_version: level_dat::DATA_VERSION_26_2 as u32,
            seed: -8_765_432_109,
            spawn_dimension: BuiltinDimension::Nether as i32,
            spawn_x: -41,
            spawn_y: 73,
            spawn_z: 902,
            day_time,
            default_game_mode: GameMode::Adventure as i32,
        }
    }

    #[test]
    fn typed_properties_replace_values_without_dropping_template_dimensions() {
        let native = tempfile::tempdir().unwrap();
        let template_dir = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        template(template_dir.path());
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native.path().to_owned(),
        }).unwrap();
        storage.write_dirty_world_properties(properties(0)).unwrap();

        let result = export_metadata(
            &storage,
            template_dir.path(),
            destination.path(),
            "Typed export",
            1_789_000_123_456,
        ).unwrap();
        assert_eq!(result.game_data_version, level_dat::DATA_VERSION_26_2 as u32);

        let level = level_dat::read_from_file(&level_dat::path_in(destination.path())).unwrap();
        assert_eq!(level.level_name(), Some("Typed export"));
        assert_eq!(level.game_type(), Some(2));
        assert_eq!(level.time(), Some(0));
        assert_eq!(level.last_played(), Some(1_789_000_123_456));
        assert_eq!(level.spawn().unwrap().pos, [-41, 73, 902]);
        assert_eq!(level.spawn().unwrap().dimension, "minecraft:the_nether");
        let settings = world_gen_settings::read_from_file(
            &world_gen_settings::path_in(destination.path()),
        ).unwrap();
        assert_eq!(settings.seed().unwrap(), -8_765_432_109);
        assert!(settings.has_dimensions());
    }

    #[test]
    fn nonzero_day_clock_refuses_before_touching_destination() {
        let native = tempfile::tempdir().unwrap();
        let template_dir = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        template(template_dir.path());
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native.path().to_owned(),
        }).unwrap();
        storage.write_dirty_world_properties(properties(6_001)).unwrap();

        assert!(matches!(
            export_metadata(&storage, template_dir.path(), destination.path(), "world", 0),
            Err(Error::UnsupportedDayTime { value: 6_001 })
        ));
        assert!(!level_dat::path_in(destination.path()).exists());
        assert!(!world_gen_settings::path_in(destination.path()).exists());
    }
}
