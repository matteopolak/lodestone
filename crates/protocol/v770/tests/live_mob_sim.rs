//! End-to-end acceptance gate for issue #217: a real `lodestone-client`
//! observes a **real, AI-ticked** [`lodestone_server::MobSim`] — not a
//! hand-mutated stand-in.
//!
//! `tests/entity_streaming_live.rs` already proved the spawn/update/remove
//! *wire pipeline* (encode/decode/client fold) end to end, but explicitly
//! could not use a real `MobSim` there: at the time, `MobSim` was `!Send`
//! (`lodestone_entity::ai::Goal` carried no `Send` bound), and
//! `IntegratedServer::open_in_memory_with_entities` spawns its serving task
//! with `tokio::spawn`, which requires the captured future to be `Send`. That
//! comment is why this is a separate file rather than an addition to that
//! one: it is proof the blocker it described no longer holds, using the
//! production entry point (`IntegratedServer::open_in_memory_with_mobs`)
//! rather than reaching into `lodestone-server`'s private tick-loop function.
//!
//! What would have to break for this to fail: `MobSim`/`Box<dyn Goal>`
//! becoming `!Send` again (a compile failure, not a runtime one),
//! `open_in_memory_with_mobs` not actually spawning a tick task, the tick
//! task not calling `MobSim::tick`, or the same wire pipeline
//! `entity_streaming_live.rs` already covers regressing.
//!
//! `#[tokio::test(start_paused = true)]` (this crate's own `tests/serve_play.rs`
//! precedent) was tried here first and does not work: this test's two tasks
//! (the served connection and the mob tick loop) exchange real duplex I/O
//! continuously, so the runtime is never purely blocked on timers and virtual
//! time never auto-advances — it hangs instead of running fast. So this runs
//! on the real clock and waits out `RandomStrollGoal`'s real (~120-tick
//! average, i.e. ~6s at the tick loop's 50ms cadence) interval before a
//! wandering mob picks its first destination, and is `#[ignore]`d like this
//! workspace's other live/real-timing gates (`CLAUDE.md`: "Live and GPU gates
//! are `#[ignore]`d. Run them explicitly.").

use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_server::{IntegratedServer, WorldgenChunkSource};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::adapter;
use lodestone_worldgen::density::Density;
use uuid::Uuid;

fn profile() -> LoginProfile {
    LoginProfile {
        username: "SinglePlayer".into(),
        uuid: Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

/// A flat solid floor with its surface at y=0 — same shape
/// `tests/mob_sim.rs` and `entity_streaming_live.rs` both already use, so the
/// mob has real, if trivial, terrain to path over and stand on.
fn floor_density() -> Density {
    Density::YClampedGradient {
        from_y: -64.0,
        to_y: 64.0,
        from_value: 1.0,
        to_value: -1.0,
    }
}

#[tokio::test]
#[ignore = "real wall-clock timing (waits out RandomStrollGoal's ~6s average \
            first-move interval); run explicitly with -- --ignored"]
async fn a_real_client_observes_a_real_ai_ticked_mob_sim() {
    let min_y = -64;
    let height = 128;
    let view_radius = 1;

    // Two independent instances of the same deterministic generator — one
    // for the terrain the client is streamed, one for the terrain the mob
    // sim paths over. `open_in_memory_with_mobs`'s own doc comment explains
    // why this is two handles onto the same world, not two different worlds.
    let source = WorldgenChunkSource::new(floor_density(), min_y, height);
    let mob_world_source = WorldgenChunkSource::new(floor_density(), min_y, height);

    let mob_count = 3;
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol,
        source,
        mob_world_source,
        (-1..=1, -1..=1),
        (8, 8),
        mob_count,
        view_radius,
    );

    let (handle, _events) =
        ClientBuilder::new(address(), profile(), Box::new(adapter())).connect_with(client_io);

    // Join: wait for the initial chunk view.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while handle.loaded_chunk_count() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "client never received a chunk within 60s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // `run_mob_tick_loop` starts mob ids at 1000 (`MobSim::set_next_id`,
    // avoiding a collision with `LOCAL_PLAYER_ENTITY_ID` — see that method's
    // doc comment) and `seed_demo_mobs` assigns them in spawn order, so ids
    // 1000..1000+mob_count are deterministic here.
    let mob_ids: Vec<i32> = (1000..1000 + i32::try_from(mob_count).unwrap()).collect();

    let spawn_deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut initial = std::collections::HashMap::new();
    while std::time::Instant::now() < spawn_deadline && initial.len() < mob_ids.len() {
        for &id in &mob_ids {
            if let Some(view) = handle.entity(id) {
                initial.entry(id).or_insert(view);
            }
        }
        let _ = handle.chat("poke");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        initial.len(),
        mob_ids.len(),
        "not every demo mob reached the client within 60s"
    );
    for view in initial.values() {
        assert_eq!(view.entity_type.namespace(), "minecraft");
        assert_eq!(view.entity_type.path(), "zombie");
    }

    // Move: poll for *any* mob's client-folded position to diverge from its
    // spawn snapshot — proof this is a *ticking* simulation (`MobSim::tick`
    // really running goal AI over the wire), not a spawn-only echo. Polling
    // all `mob_count` mobs rather than just one keeps this from being
    // flaky on `RandomStrollGoal`'s per-mob random first-move interval.
    let move_deadline = std::time::Instant::now() + Duration::from_secs(90);
    let mut moved: Option<(i32, lodestone_model::Vec3)> = None;
    while std::time::Instant::now() < move_deadline && moved.is_none() {
        let _ = handle.chat("poke");
        for &id in &mob_ids {
            if let Some(view) = handle.entity(id)
                && view.position != initial[&id].position
            {
                moved = Some((id, view.position));
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (moved_id, moved_pos) = moved.expect(
        "no demo mob moved from its spawn position within 90s — AI does not appear to be ticking",
    );

    println!(
        "real client observed a real AI-ticked MobSim: mob {moved_id} moved from {:?} to {moved_pos:?}",
        initial[&moved_id].position
    );

    drop(handle);
    server.shutdown().await;
}
