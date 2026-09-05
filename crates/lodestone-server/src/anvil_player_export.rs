//! Staged export of typed native player records into Anvil player files.
//!
//! The complete native general-record snapshot is decoded before the output is
//! touched. Player files are then written below a same-filesystem staging root
//! and the complete `players/data` directory is published with one rename.

use std::{
    fs,
    path::{Path, PathBuf},
};

use lodestone_model::{Rotation, Vec3};
use lodestone_storage_schema::BuiltinDimension;

use crate::{
    anvil_player_storage::POSITION_UNITS_PER_BLOCK,
    player_data::{PlayerData, PlayerDataStore},
    world_storage::{NativeGeneralRecord, NativePlayerData, WorldStorage},
};

/// Result of publishing one complete native-player snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlayerExportResult {
    /// Number of player files published.
    pub players_exported: usize,
}

/// Failure before or during a native-player export.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Native storage could not produce a complete typed general snapshot.
    #[error("native player snapshot failed: {0}")]
    Storage(#[source] crate::world_storage::Error),
    /// The destination must already be an Anvil world directory.
    #[error("Anvil player export destination is not an existing directory: {path}")]
    MissingDestination { path: PathBuf },
    /// Existing player data is never replaced implicitly.
    #[error("Anvil player export destination already has players/data: {path}")]
    DestinationExists { path: PathBuf },
    /// A prior interrupted export left the deterministic staging root behind.
    #[error("Anvil player export staging path already exists: {path}")]
    StagingExists { path: PathBuf },
    /// An Anvil container operation failed.
    #[error("Anvil player export failed: {0}")]
    Anvil(#[source] lodestone_anvil::Error),
    /// A filesystem publication operation failed.
    #[error("Anvil player export filesystem operation failed: {0}")]
    Io(#[source] std::io::Error),
}

/// Exports every typed native player in one recovered snapshot.
///
/// Non-player general records are ignored only after they have passed the
/// native snapshot's complete typed validation. The destination must already
/// exist and must not contain `players/data`; merging or replacing player files
/// would need a separate reviewed conflict policy.
pub fn export_all_players(
    storage: &WorldStorage,
    destination: &Path,
) -> Result<PlayerExportResult, Error> {
    let players: Vec<_> = storage
        .native_general_records()
        .map_err(Error::Storage)?
        .into_iter()
        .filter_map(|record| match record {
            NativeGeneralRecord::Player(player) => Some(player),
            NativeGeneralRecord::WorldProperties(_) | NativeGeneralRecord::Entity(_) => None,
        })
        .collect();

    if !destination.is_dir() {
        return Err(Error::MissingDestination {
            path: destination.to_owned(),
        });
    }
    let target = lodestone_anvil::player_dat::dir_in(destination);
    if target.exists() {
        return Err(Error::DestinationExists { path: target });
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("world");
    let staging = parent.join(format!(".{name}.lodestone-player-exporting"));
    if staging.exists() {
        return Err(Error::StagingExists { path: staging });
    }

    let result = (|| {
        let staged_store = PlayerDataStore::new(&staging).map_err(Error::Anvil)?;
        for player in &players {
            let uuid = uuid::Uuid::from_bytes(player.locator.uuid);
            staged_store
                .write(uuid, &to_anvil_player(player))
                .map_err(Error::Anvil)?;
        }
        let players_dir = destination.join("players");
        fs::create_dir_all(&players_dir).map_err(Error::Io)?;
        fs::rename(lodestone_anvil::player_dat::dir_in(&staging), &target).map_err(Error::Io)?;
        Ok(PlayerExportResult {
            players_exported: players.len(),
        })
    })();
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(Error::Io)?;
    }
    result
}

fn to_anvil_player(player: &NativePlayerData) -> PlayerData {
    let locator = player.locator;
    let units = POSITION_UNITS_PER_BLOCK;
    let runtime = player.runtime;
    PlayerData {
        pos: Vec3::new(
            f64::from(locator.x_fixed) / units,
            f64::from(locator.y_fixed) / units,
            f64::from(locator.z_fixed) / units,
        ),
        rotation: Rotation::new(
            locator.yaw_millidegrees as f32 / 1_000.0,
            locator.pitch_millidegrees as f32 / 1_000.0,
        ),
        dimension: match locator.dimension {
            BuiltinDimension::Overworld => "minecraft:overworld",
            BuiltinDimension::Nether => "minecraft:the_nether",
            BuiltinDimension::End => "minecraft:the_end",
            BuiltinDimension::Unspecified => {
                unreachable!("native decoder rejects unspecified dimensions")
            }
        }
        .to_owned(),
        game_mode: player.game_mode,
        health: runtime.map_or(20.0, |state| state.health),
        air_supply: runtime.map_or(300, |state| state.air_supply),
        experience: runtime.map_or_else(
            crate::experience::PlayerExperience::default,
            |state| state.experience,
        ),
        selected_slot: player
            .inventory
            .as_ref()
            .map_or(0, crate::inventory::PlayerInventory::selected_hotbar_slot),
        inventory: player
            .inventory
            .as_ref()
            .map(|inventory| {
                (0..crate::inventory::PLAYER_NATIVE_SIZE)
                    .map(|slot| inventory.native(slot).cloned())
                    .collect()
            })
            .unwrap_or_else(|| vec![None; crate::inventory::PLAYER_NATIVE_SIZE]),
        ..PlayerData::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_storage::{NativePlayerRecord, WorldStorageBackend};

    fn scratch(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lodestone-native-player-export-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated player-export scratch directory");
        path
    }

    fn player(uuid: [u8; 16], dimension: BuiltinDimension) -> NativePlayerData {
        let mut inventory = crate::inventory::PlayerInventory::new();
        inventory.set_native(
            4,
            Some(lodestone_model::ItemStack::new(
                "minecraft:stone".parse().unwrap(),
                32,
            )),
        );
        assert!(inventory.set_selected_hotbar_slot(4));
        NativePlayerData {
            locator: NativePlayerRecord {
                uuid,
                dimension,
                x_fixed: -12_345,
                y_fixed: 64_125,
                z_fixed: 98_765,
                yaw_millidegrees: -90_001,
                pitch_millidegrees: 45_002,
            },
            game_mode: Some(lodestone_model::GameMode::Creative),
            runtime: Some(crate::world_storage::NativePlayerRuntimeState {
                health: 11.5,
                air_supply: 222,
                experience: crate::experience::PlayerExperience::restored(4, 0.75, 57),
            }),
            inventory: Some(inventory),
        }
    }

    #[test]
    fn all_native_players_publish_as_reopenable_anvil_files() {
        let scratch = scratch("round-trip");
        let native = scratch.join("native");
        let destination = scratch.join("anvil");
        fs::create_dir(&destination).expect("create existing Anvil world directory");
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native,
        })
        .expect("open native store");
        let overworld = player([0x11; 16], BuiltinDimension::Overworld);
        let nether = player([0x22; 16], BuiltinDimension::Nether);
        storage
            .write_dirty_player_data(nether.clone())
            .expect("seed Nether player");
        storage
            .write_dirty_player_data(overworld.clone())
            .expect("seed Overworld player");

        assert_eq!(
            export_all_players(&storage, &destination).expect("export player snapshot"),
            PlayerExportResult {
                players_exported: 2,
            }
        );
        let anvil = PlayerDataStore::new(&destination).expect("open published Anvil players");
        for expected in [overworld, nether] {
            let uuid = uuid::Uuid::from_bytes(expected.locator.uuid);
            let actual = anvil
                .read(uuid)
                .expect("read exported Anvil player")
                .expect("exported player exists");
            let locator = expected.locator;
            assert_eq!(actual.pos, to_anvil_player(&expected).pos);
            assert_eq!(actual.rotation, to_anvil_player(&expected).rotation);
            assert_eq!(actual.dimension, to_anvil_player(&expected).dimension);
            assert_eq!(actual.game_mode, expected.game_mode);
            let runtime = expected.runtime.expect("fixture carries runtime state");
            assert_eq!(actual.health, runtime.health);
            assert_eq!(actual.air_supply, runtime.air_supply);
            assert_eq!(actual.experience, runtime.experience);
            assert_eq!(actual.selected_slot, 4);
            assert_eq!(
                actual.inventory[4],
                expected
                    .inventory
                    .as_ref()
                    .and_then(|inventory| inventory.native(4))
                    .cloned(),
            );
            assert!(anvil.path_for(uuid).is_file());
            assert_ne!(locator.dimension, BuiltinDimension::Unspecified);
        }
        drop(storage);
        fs::remove_dir_all(scratch).expect("remove player-export scratch directory");
    }

    #[test]
    fn existing_player_directory_is_not_merged_or_replaced() {
        let scratch = scratch("existing");
        let destination = scratch.join("anvil");
        let existing = lodestone_anvil::player_dat::dir_in(&destination);
        fs::create_dir_all(&existing).expect("create existing player directory");
        let marker = existing.join("owned.dat");
        fs::write(&marker, b"owned").expect("write existing marker");
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: scratch.join("native"),
        })
        .expect("open native store");

        assert!(matches!(
            export_all_players(&storage, &destination),
            Err(Error::DestinationExists { .. })
        ));
        assert_eq!(fs::read(marker).expect("read unchanged marker"), b"owned");
        drop(storage);
        fs::remove_dir_all(scratch).expect("remove player-export scratch directory");
    }
}
