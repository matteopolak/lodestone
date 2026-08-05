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
//! time never auto-advances — it hangs instead of running fast. So this runs on
//! the real clock, and is `#[ignore]`d like this workspace's other live/
//! real-timing gates (`CLAUDE.md`: "Live and GPU gates are `#[ignore]`d. Run
//! them explicitly.").
//!
//! # This gate used to be red, and part of the reason was its own premise
//!
//! It previously claimed to wait out "`RandomStrollGoal`'s real (~120-tick
//! average, i.e. ~6s at the tick loop's 50ms cadence) interval before a
//! wandering mob picks its first destination". Both halves of that were wrong,
//! and the second half was wrong in a way no wait could fix:
//!
//! * The interval is not a fresh per-mob random draw. Every `NavigatingMob`
//!   shares one hardcoded seed (`lodestone-entity/src/ai/navigating_mob.rs`'s
//!   `SplitMix64(0x1234_5678_9ABC_DEF0)`), so all three demo mobs roll the
//!   *same* stream, whose first successful `next_u64() % 120 == 0` draw is draw
//!   130 — not an average, a fixed tick.
//! * That draw was unreachable. `RandomStrollGoal::can_use` early-returns once
//!   `no_action_time >= 100`, and nothing in production ever reset that counter
//!   (`MobSim::despawn_pass` owns the reset and has no production caller), so
//!   the throttle closed at tick 100, thirty draws early, and **no mob could
//!   ever stroll**. See `lodestone-server/tests/mob_idle_throttle.rs` for the
//!   hermetic gate on that fix and its control.
//!
//! And it also could not have worked for a third reason, which is what this file
//! itself had to change: **the client never told the server where it was.**
//! `MobSim::set_players` — the sole producer of every mob's player perception —
//! is fed from the `PlayerMoved` arm of `serve_play` and nowhere else, so a
//! client that never sends a movement packet leaves the sim with an empty player
//! list. A bare `lodestone-client` sends one only when asked; `lodestone-shell`,
//! the real client, queues a `ClientAction::Move` **every** 20 Hz tick even for
//! a player standing perfectly still (`lodestone-shell/src/net.rs`'s module
//! doc). So this gate's client was the one client in the workspace that reports
//! no position, and that made it structurally unable to exercise the production
//! path it exists to cover — vacuous by its *input*, not by its assertions. It
//! now reports its position once per poll, exactly as the shell does.
//!
//! So the motion this gate observes is a real zombie from the real roster
//! acquiring the real player it was told about and closing on it — arriving in
//! about a second rather than after a 130-tick stroll wait.
//!
//! # Why this world's floor is at y=100 and not y=0
//!
//! Reporting a position is only half of it: the position has to be somewhere the
//! mobs are. `v770`'s `begin_play` teleports a joining client to a hardcoded
//! `(8, 100, 8)` (`lodestone-server/src/server.rs`'s `JOIN_SPAWN_POSITION`, and
//! the `spawn_x`/`spawn_y`/`spawn_z` literals it must agree with), and there is
//! no terrain-derived spawn anywhere in the server. This gate's floor used to be
//! at y=0 inside a world of `-64..=63`, so the reported player sat **100 blocks
//! above the mobs and above the world itself** — outside the zombie's 35-block
//! `follow_range`, so nothing could target it, and outside the 32-block
//! `noDespawnDistance` immune radius, so the `no_action_time` reset could not
//! fire either. Both goal families were distance-gated false and the gate stayed
//! red even with the perception feed connected.
//!
//! So the floor is put where the player actually spawns rather than the player
//! made to fall to the floor. Falling is what a real client does — it owns its
//! own gravity, this crate's client runs no physics — but a 100-block drop is
//! lethal, and a dead player is held on the death screen, which sends no chunks
//! (`CLAUDE.md`, live-server hazards). That would turn this gate red for a reason
//! that has nothing to do with what it measures.
//!
//! The hardcoded spawn altitude is a real defect in its own right — on normal
//! terrain, whose surface is nearer y=64, it puts every joining player in the air
//! by construction — but it is filed separately: fixing it needs the
//! `ServerProtocol` "where is spawn" seam that `JOIN_SPAWN_POSITION`'s own doc
//! comment describes, which is wider than this gate.

use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientHandle, LoginProfile, ServerAddress};
use lodestone_model::Rotation;
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
/// Reports the client's own current position back to the server, unchanged —
/// what a real client does every tick whether or not the player moved.
///
/// This is the producer for `MobSim::set_players`, and therefore for every
/// mob's `nearest_player` perception; see this file's module doc for why its
/// absence made this gate unable to observe any AI at all. Standing still is
/// deliberate: nothing here is trying to *walk* the player, only to be a client
/// that reports where it is, so any mob motion observed below is the server's
/// AI and not the test dragging a target around.
///
/// A no-op before the server's first position sync (`position()` is `None`
/// until then), which is why it is called inside the poll loops rather than
/// once up front.
fn report_position(handle: &ClientHandle) {
    if let Some(pos) = handle.position() {
        let _ = handle.move_to(pos, Rotation::new(0.0, 0.0), true, false);
    }
}

/// A flat solid floor whose surface is at **y=100** — the altitude `v770`'s
/// `begin_play` spawns a joining client at. See the module doc: with the surface
/// at y=0 the player hovered 100 blocks above every mob and both the targeting
/// and idle-reset distance gates were false.
///
/// Positive below the crossover and negative above, so the gradient's midpoint
/// *is* the surface: `36 + (164 - 36) / 2 == 100`. Built from the same
/// density-function source the server streams to clients, not a bespoke double.
fn floor_density() -> Density {
    Density::YClampedGradient {
        from_y: 36.0,
        to_y: 164.0,
        from_value: 1.0,
        to_value: -1.0,
    }
}

#[tokio::test]
#[ignore = "real wall-clock timing and real duplex I/O between two tasks, so \
            virtual time cannot drive it (see the module doc); run explicitly \
            with -- --ignored"]
async fn a_real_client_observes_a_real_ai_ticked_mob_sim() {
    let min_y = -64;
    // Tall enough to *contain* the y=100 spawn altitude: `-64..=127`. At the
    // previous 128 the world topped out at y=63 and the joining player was
    // teleported above every loaded section, which is the other half of why the
    // distance gates below could never be satisfied.
    let height = 192;
    let view_radius = 1;

    // **One** generator instance, shared. It used to be two independent
    // instances of the same deterministic generator — one for the terrain the
    // client is streamed, one for the terrain the mob sim paths over — which was
    // observationally equivalent and generated the whole mob area twice at world
    // open (issue #454). Since #436 there is one parameter and one source, so the
    // mob sim paths over the byte-identical terrain the client was sent rather
    // than over a second copy that merely agrees.
    let source = WorldgenChunkSource::new(floor_density(), min_y, height);

    let mob_count = 3;
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol,
        source,
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
        report_position(&handle);
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
    // really running goal AI over the wire), not a spawn-only echo. Polling all
    // `mob_count` mobs rather than just one keeps this from depending on any one
    // mob's goal choice.
    //
    // `report_position` inside the loop is load-bearing, not hygiene: it is what
    // keeps a player in the sim's perception at all, and therefore what any
    // player-driven goal has to work with. See this file's module doc.
    let move_deadline = std::time::Instant::now() + Duration::from_secs(90);
    let mut moved: Option<(i32, lodestone_model::Vec3)> = None;
    while std::time::Instant::now() < move_deadline && moved.is_none() {
        let _ = handle.chat("poke");
        report_position(&handle);
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
