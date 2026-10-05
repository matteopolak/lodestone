//! Tests for saved-player restore and join position.

use super::*;
use crate::server::tests::DimensionOnly;
use crate::chunk::ChunkColumn;
use lodestone_model::{Rotation, Vec3};

struct NativeRestoreWorld;

impl ChunkSource for NativeRestoreWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 256)
    }
    fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
        crate::chunk::air_state()
    }
    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        crate::chunk::DEFAULT_BIOME.to_owned()
    }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
    fn dimension(&self) -> Option<crate::dimension::Dimension> {
        Some(crate::dimension::Dimension::Overworld)
    }
    fn sibling(
        &self,
        dimension: crate::dimension::Dimension,
    ) -> Option<Arc<dyn ChunkSource>> {
        (dimension == crate::dimension::Dimension::Nether)
            .then(|| Arc::new(DimensionOnly(dimension)) as Arc<dyn ChunkSource>)
    }
}

fn native_nether_session() -> (tempfile::TempDir, NativePlayerSession) {
    let directory = tempfile::tempdir().unwrap();
    let storage = Arc::new(
        crate::world_storage::WorldStorage::open(
            crate::world_storage::WorldStorageBackend::LodestoneNative {
                directory: directory.path().to_owned(),
            },
        )
        .unwrap(),
    );
    let loaded = crate::world_storage::NativePlayerData {
        locator: crate::world_storage::NativePlayerRecord {
            uuid: [0x71; 16],
            dimension: lodestone_storage_schema::BuiltinDimension::Nether,
            x_fixed: 12_500,
            y_fixed: 64_250,
            z_fixed: -3_750,
            yaw_millidegrees: 91_000,
            pitch_millidegrees: -12_500,
        },
        game_mode: Some(GameMode::Creative),
        runtime: Some(crate::world_storage::NativePlayerRuntimeState {
            health: 7.5,
            air_supply: 42,
            experience: crate::experience::PlayerExperience::restored(12, 0.5, 345),
        }),
        inventory: None,
    };
    (
        directory,
        NativePlayerSession {
            storage,
            uuid: loaded.locator.uuid,
            loaded: Some(loaded),
            save_blocked: false,
        },
    )
}

#[test]
fn native_nether_restore_selects_the_sibling_and_uses_its_pose() {
    let (_directory, mut session) = native_nether_session();
    let sibling = session
        .restored_source(&NativeRestoreWorld, true)
        .expect("a hosted saved dimension must become the join source");
    assert_eq!(sibling.dimension(), Some(crate::dimension::Dimension::Nether));
    assert_eq!(
        session.join_position(Vec3::new(8.0, 80.0, 8.0)),
        Vec3::new(12.5, 64.25, -3.75),
    );
    assert_eq!(session.initial_rotation(), Some(Rotation::new(91.0, -12.5)));
    let runtime = session.runtime().expect("typed runtime state restores");
    assert_eq!(runtime.health, 7.5);
    assert_eq!(runtime.air_supply, 42);
    assert_eq!(runtime.experience.level(), 12);
    assert_eq!(runtime.experience.progress(), 0.5);
    assert_eq!(runtime.experience.total(), 345);
}

#[test]
fn native_dimension_restore_failure_preserves_the_record() {
    let (_directory, mut session) = native_nether_session();
    assert!(session.restored_source(&NativeRestoreWorld, false).is_none());
    let fallback = Vec3::new(8.0, 80.0, 8.0);
    assert_eq!(session.join_position(fallback), fallback);
    assert!(
        session
            .snapshot(
                Some((1.0, 2.0, 3.0)),
                None,
                fallback,
                crate::dimension::Dimension::Overworld,
                GameMode::Survival,
                &PlayerVitals::default(),
                &crate::experience::PlayerExperience::default(),
                &PlayerInventory::default(),
            )
            .is_none(),
        "a failed cross-dimension restore must not overwrite its evidence on disconnect"
    );
}

/// No saved player at all falls back to the world spawn.
#[test]
fn join_position_for_saved_player_is_world_spawn_with_no_save() {
    let spawn = Vec3::new(8.0, 71.0, 8.0);
    assert_eq!(join_position_for_saved_player(None, spawn), spawn);
}

/// **The discriminating gate for the "buried in the ground"
/// report.** The same raw position, saved under two different dimension
/// tags: the overworld-tagged save is trusted verbatim (predicted to
/// equal the saved position exactly, not merely "differs from spawn"),
/// and the Nether-tagged one must fall back to the world spawn rather
/// than being joined into the overworld as a raw coordinate — which is
/// exactly the bug a player who died or disconnected in the Nether hit.
#[test]
fn join_position_for_saved_player_distrusts_a_non_overworld_position() {
    let spawn = Vec3::new(8.0, 71.0, 8.0);
    // Deliberately not equal to `spawn` on any axis, so a bug that
    // silently returned the wrong constant could not hide.
    let raw = Vec3::new(15.0, 64.0, -3.0);
    let inventory = PlayerInventory::default();

    let overworld_save = crate::player_data::PlayerData::capture(
        raw,
        Rotation::new(0.0, 0.0),
        20.0,
        300,
        GameMode::Survival,
        &inventory,
        crate::experience::PlayerExperience::default(),
        Vec::new(),
        crate::dimension::Dimension::Overworld,
    );
    assert_eq!(
        join_position_for_saved_player(Some(&overworld_save), spawn),
        raw,
        "an overworld-tagged save must be trusted verbatim"
    );

    let nether_save = crate::player_data::PlayerData::capture(
        raw,
        Rotation::new(0.0, 0.0),
        20.0,
        300,
        GameMode::Survival,
        &inventory,
        crate::experience::PlayerExperience::default(),
        Vec::new(),
        crate::dimension::Dimension::Nether,
    );
    assert_eq!(
        join_position_for_saved_player(Some(&nether_save), spawn),
        spawn,
        "a Nether-tagged save must fall back to the world spawn rather than be \
         joined as a raw overworld coordinate"
    );
}

/// An unparseable or unknown dimension tag falls back like any other
/// non-overworld tag instead of being treated as trustworthy.
#[test]
fn join_position_for_saved_player_distrusts_an_unparseable_dimension_tag() {
    let spawn = Vec3::new(8.0, 71.0, 8.0);
    let raw = Vec3::new(15.0, 64.0, -3.0);
    let inventory = PlayerInventory::default();
    let mut save = crate::player_data::PlayerData::capture(
        raw,
        Rotation::new(0.0, 0.0),
        20.0,
        300,
        GameMode::Survival,
        &inventory,
        crate::experience::PlayerExperience::default(),
        Vec::new(),
        crate::dimension::Dimension::Overworld,
    );
    save.dimension = "not a real dimension key".to_string();
    assert_eq!(
        join_position_for_saved_player(Some(&save), spawn),
        spawn,
        "an unparseable dimension tag must not be trusted as the overworld"
    );
}
