//! The "what consumes this" gate for `IntegratedServer::spawn_mob`/`despawn_mob`
//! (the server-side half of a native plugin's spawn/despawn/modify surface) and
//! `MobSim::remove_mob` underneath them.
//!
//! Drives a real, running [`IntegratedServer`] — the same constructor
//! `hostile_mob_attack_reaches_the_wire.rs` and `natural_spawn_reaches_the_wire.rs`
//! use — through its **public** `spawn_mob`/`despawn_mob` API, and reads the
//! result back through [`IntegratedServer::mobs`]'s real [`MobHandle`]: the exact
//! same handle `crate::server::apply_attack` and the tick loop's own mob
//! simulation share. Never calls `MobSim::spawn_species`/`remove_mob` directly,
//! which would prove only that the underlying primitives work, not that a native
//! plugin embedding the server can actually reach them.

use std::time::Duration;

use lodestone_core::State;
use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol,
};
use uuid::Uuid;

const MIN_Y: i32 = 0;
const HEIGHT: i32 = 16;

/// A connection is never established in this file — `spawn_mob`/`despawn_mob`
/// mutate the live sim directly through the same mutex-guarded [`MobHandle`]
/// a connection task uses, with no packet round trip needed — so this protocol
/// only needs to exist to satisfy `open_in_memory_with_mobs`'s type parameter.
#[derive(Debug)]
struct SilentProtocol;

impl ServerProtocol for SilentProtocol {
    fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
        ServerBound::Ignored
    }
    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }
    fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
        ServerDirective::None
    }
    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }
}

#[derive(Debug)]
struct FlatWorld;

impl ChunkSource for FlatWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(MIN_Y, HEIGHT)
    }

    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:air".to_string()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; this fixture serves fresh columns by design.
    }
}

fn open() -> IntegratedServer {
    // mob_count: 0 -- no natural spawning to confuse "is my spawned mob really
    // there" with "some unrelated natural mob happened to be there too".
    let (server, client) = IntegratedServer::open_in_memory_with_mobs(
        SilentProtocol,
        FlatWorld,
        (0..=0, 0..=0),
        (0, 0),
        0,
        1,
    );
    // Never used: no connection is established in this file. Leaked rather
    // than dropped so dropping it cannot race the tick task's own teardown
    // and turn an unrelated shutdown timing issue into a false failure here.
    std::mem::forget(client);
    server
}

/// The primary gate: a plugin-facing spawn is visible through the real
/// [`MobHandle`] immediately (no tick needed -- this is a direct mutation
/// under the same lock [`crate::server::apply_attack`] uses, not something
/// that waits for `GameTick`), modifiable through the same handle, and a
/// plugin-facing despawn actually removes it.
#[tokio::test]
async fn a_spawned_mob_is_live_through_the_real_handle_and_despawn_removes_it() {
    let server = open();
    let mobs = server.mobs().expect("open_in_memory_with_mobs starts a tick loop");

    let id = server
        .spawn_mob(
            "minecraft:cow".parse::<ResourceKey>().expect("valid resource key"),
            Vec3::new(4.0, 8.0, 4.0),
        )
        .expect("a running server must accept a spawn");

    assert!(
        mobs.with(|sim| sim.get(id).is_some()),
        "the spawned mob must be visible through the same MobHandle apply_attack uses"
    );
    assert_eq!(
        mobs.with(|sim| sim.get(id).map(|mob| mob.position())),
        Some(Vec3::new(4.0, 8.0, 4.0))
    );

    // "Modify" needs no new API: a plugin already reaches a live mob through
    // the same handle, e.g. to heal or reposition it.
    mobs.with(|sim| {
        sim.get_mut(id).expect("just spawned").heal(1.0);
    });

    let despawned = server.despawn_mob(id).expect("a running server must accept a despawn");
    assert!(despawned, "despawn_mob must report a real removal");
    assert!(
        mobs.with(|sim| sim.get(id).is_none()),
        "the despawned mob must be gone from the live sim"
    );

    // A repeat despawn of the same, now-gone id is a harmless no-op, never a
    // panic and never a spurious "removed" report.
    let repeat = server.despawn_mob(id).expect("still a running server");
    assert!(!repeat, "despawning an already-gone id must report false");

    server.shutdown().await;
}

/// Negative control: `MobSim::remove_mob` must never fire on an id nothing
/// holds. Guards against a predicate loose enough to remove the nearest mob,
/// or any mob at all, rather than exactly the one named.
#[tokio::test]
async fn despawning_an_id_nothing_holds_removes_nothing() {
    let server = open();
    let mobs = server.mobs().expect("open_in_memory_with_mobs starts a tick loop");

    let real = server
        .spawn_mob(
            "minecraft:pig".parse::<ResourceKey>().expect("valid resource key"),
            Vec3::new(0.0, 8.0, 0.0),
        )
        .expect("spawn must succeed");

    let result = server.despawn_mob(real + 999_999);
    assert_eq!(result, Some(false), "an untracked id must report no removal");
    assert!(
        mobs.with(|sim| sim.get(real).is_some()),
        "control: the real mob must be untouched by an unrelated despawn call"
    );

    server.shutdown().await;
}

/// `mobs()`/`spawn_mob`/`despawn_mob` all answer `None` for a constructor with
/// no tick loop -- the same contract every other `IntegratedServer` accessor
/// documented against a mob-less constructor already gives.
#[tokio::test]
async fn spawn_and_despawn_answer_none_with_no_tick_loop() {
    let (server, client) = IntegratedServer::open_in_memory(SilentProtocol, FlatWorld, 1);
    std::mem::forget(client);

    assert!(server.mobs().is_none());
    assert_eq!(
        server.spawn_mob(
            "minecraft:cow".parse::<ResourceKey>().expect("valid resource key"),
            Vec3::new(0.0, 8.0, 0.0)
        ),
        None
    );
    assert_eq!(server.despawn_mob(0), None);

    // No tick loop was started, so there is nothing to await a graceful
    // shutdown against beyond the default teardown; bound it anyway so a
    // regression here fails rather than hangs.
    let _ = tokio::time::timeout(Duration::from_secs(5), server.shutdown()).await;
}
