//! End-to-end: the **real** `lodestone-client`, running the real
//! [`V770Adapter`], against the real [`V770ServerProtocol`] over the
//! in-memory transport — proving the air-supply/drowning-damage wire format
//! (`encode_air_supply_update`'s hand-rolled `SET_ENTITY_DATA` metadata
//! list, `encode_set_health`'s `SET_HEALTH`) actually round-trips through
//! the real protocol-776 decoder into [`PlayerSnapshot`], the same
//! real-wire-format companion role
//! `crates/protocol/v770/tests/server_liveness.rs` plays for keep-alive,
//! time-of-day and view streaming. The version-free scheduling itself
//! (`PlayerVitals`'s tick-exact cadence assertions, and both controls) is
//! covered in more detail and faster by
//! `crates/lodestone-server/tests/serve_play.rs`'s
//! `submerged_player_loses_air_and_takes_drowning_damage_on_vanilla_cadence`/
//! `dry_player_keeps_full_air_and_takes_no_damage` — this file exists to
//! prove the *real* wire bytes reach a *real* client, not to re-derive the
//! cadence a second time.
//!
//! Both tests run under `#[tokio::test(start_paused = true)]`, the same
//! paused-clock auto-advance pattern `server_liveness.rs` already
//! establishes for its 15s+ spans.

use std::time::Duration;

use lodestone_client::{ClientBuilder, EventStream, LoginProfile, ServerAddress};
use lodestone_model::{Rotation, Vec3};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer, WorldgenChunkSource};
use lodestone_v770::{V770ServerProtocol, adapter};
use lodestone_worldgen::density::Density;

fn profile(name: &str) -> LoginProfile {
    LoginProfile {
        username: name.into(),
        uuid: uuid::Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

/// Spawns a background task that drains and discards a client's event
/// stream for the rest of the test.
///
/// **Load-bearing, not cosmetic.** `ClientBuilder`'s event channel is
/// bounded (`DEFAULT_EVENT_BUFFER` = 256 —
/// `crates/lodestone-client/src/builder.rs`); `driver.rs`'s per-packet loop
/// awaits `events.send(event)` on it, so an undrained receiver eventually
/// makes that `send` block forever once full, silently stalling the *entire*
/// client driver — no more reads, no more keep-alive echoes, nothing — with
/// no error surfaced anywhere a test would see (`IntegratedServer`'s own
/// `select!` around `serve_connection` discards its result the same way, so
/// a stall looks identical to nothing having gone wrong). This first showed
/// up as air freezing mid-count at a value with no error message at all —
/// exactly a silent stall, not a crash. These tests generate one event per
/// vitals tick over many seconds (300+ over the subject test's span), which
/// crosses that 256-entry buffer well before the first drowning hit; a
/// lower-traffic test (like `server_liveness.rs`'s) can get away with an
/// undrained `_events` binding for a while, but that is surviving on
/// borrowed time, not evidence the pattern is safe.
fn drain_events(mut events: EventStream) {
    tokio::spawn(async move { while events.recv().await.is_some() {} });
}

/// A [`ChunkSource`] whose every block is `minecraft:water`, at the real
/// overworld vertical extent (`min_y = -64`, `height = 384` — required for
/// wire-shape alignment, same reasoning as `server_liveness.rs`'s
/// `cheap_source`). Spawn (`V770ServerProtocol::begin_play`'s `(8, 100, 8)`)
/// therefore sits inside water without needing any extra movement to get
/// there — the "world" species of vacuous test this guards against would be
/// a player who spawns dry and never actually gets wet.
struct WaterSource;

impl ChunkSource for WaterSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(-64, 384);
        for x in 0..16 {
            for z in 0..16 {
                for y in -64..320 {
                    col.set_block(x, y, z, "minecraft:water");
                }
            }
        }
        col
    }

    /// Overridden rather than falling through to the default (which would
    /// rebuild an entire 384-tall column from scratch on every call): the
    /// server's per-tick submersion test (`crate::vitals` via
    /// `crates/lodestone-server/src/server.rs`'s `vitals_tick` branch) calls
    /// this every 50ms of virtual time for the whole span of these tests, so
    /// the cheap answer matters for real (CPU, not virtual) wall-clock test
    /// time even though `#[tokio::test(start_paused = true)]` makes the
    /// *virtual* duration free.
    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:water".to_string()
    }
}

/// A cheap, deterministic, all-dry terrain source — the same shape
/// `server_liveness.rs`'s `cheap_source` uses. No water anywhere, so the
/// player's eye is never submerged regardless of position.
fn dry_source() -> WorldgenChunkSource {
    let density = Density::YClampedGradient {
        from_y: -64.0,
        to_y: 64.0,
        from_value: 1.0,
        to_value: -1.0,
    };
    WorldgenChunkSource::new(density, -64, 384)
}

/// **Subject, real wire format**: a real client spawning into an all-water
/// world must see its own air supply actually fall and its own health
/// actually drop by exactly one drowning hit (20.0 -> 18.0) — proving
/// `V770ServerProtocol::encode_air_supply_update`'s hand-rolled metadata
/// bytes decode correctly through the real `V770Adapter` into
/// `PlayerSnapshot::air`, and `encode_set_health`'s packet reaches
/// `ClientHandle::health()`, not just that `lodestone-server`'s own
/// version-free scheduling computed the right numbers internally.
#[tokio::test(start_paused = true)]
async fn real_client_air_falls_and_drowning_damage_lands_underwater() {
    let (server, client_io) = IntegratedServer::open_in_memory(V770ServerProtocol, WaterSource, 0);
    let (handle, events) =
        ClientBuilder::new(address(), profile("Diver"), Box::new(adapter())).connect_with(client_io);
    drain_events(events);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");

    // Fresh-spawn defaults, proving the baseline this test's drop is
    // measured against is real (not e.g. already `None`/unknown).
    assert_eq!(handle.player().air, 300, "must join at full air");
    assert_eq!(handle.health(), Some(20.0), "must join at full health");

    // Spawn (8, 100, 8) is already inside `WaterSource`'s all-water column,
    // but the server only learns the player's position from an inbound
    // `PlayerMoved` (`crates/lodestone-server/src/server.rs`'s
    // `dispatch_play_packet`) — nothing assumes the join teleport as a
    // position. `move_to` must actually move the position (a call with an
    // unchanged position/rotation is suppressed by `V770Adapter`'s own
    // `select_move_packet` dedup — `crates/protocol/v770/src/adapter.rs`'s
    // `moved`/`rotated` gate — and never reaches the wire at all), so nudge
    // down by one block, still comfortably inside the all-water column.
    let spawn = handle.position().expect("spawned");
    handle
        .move_to(
            Vec3::new(spawn.x, spawn.y - 1.0, spawn.z),
            Rotation::new(0.0, 0.0),
            true,
            false,
        )
        .expect("send initial position");

    // Comfortably past the 16s (320-tick) real cadence to the first hit
    // (`crate::vitals`'s module doc comment in `lodestone-server`) —
    // resolved in a fraction of a second of wall time by paused-clock
    // auto-advance.
    handle
        .wait_for(Duration::from_secs(25), |h| h.health() == Some(18.0))
        .await
        .expect("drowning damage never landed on the real client");

    // Air must have actually fallen, not merely have coincided with a
    // health drop from something else this crate does not yet model.
    let air = handle.player().air;
    assert!(
        air < 300,
        "air must have fallen from full by the time drowning damage lands, got {air}"
    );

    server.shutdown().await;
}

/// **Control, real wire format**: a real client in a bone-dry world must
/// receive neither packet — no air-supply metadata update, no health
/// update — across a window as long as the subject test's above. This is
/// the control that proves `encode_air_supply_update`/`encode_set_health`
/// are gated by real submersion on the real wire, not merely that the
/// subject test happened to show a drop.
#[tokio::test(start_paused = true)]
async fn real_client_stays_full_air_and_full_health_when_dry() {
    let (server, client_io) = IntegratedServer::open_in_memory(V770ServerProtocol, dry_source(), 0);
    let (handle, events) =
        ClientBuilder::new(address(), profile("Landlubber"), Box::new(adapter())).connect_with(client_io);
    drain_events(events);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");

    // Must actually move (see the subject test's comment on
    // `select_move_packet`'s dedup) so the server genuinely learns a
    // position and runs its submersion test against it — otherwise this
    // control would vacuously pass by the vitals tick never having a
    // position to test at all, proving nothing about the dry world.
    let spawn = handle.position().expect("spawned");
    handle
        .move_to(
            Vec3::new(spawn.x, spawn.y - 1.0, spawn.z),
            Rotation::new(0.0, 0.0),
            true,
            false,
        )
        .expect("send initial position");

    tokio::time::sleep(Duration::from_secs(25)).await;

    assert_eq!(handle.player().air, 300, "a dry player's air must never fall");
    assert_eq!(handle.health(), Some(20.0), "a dry player must never take drowning damage");

    server.shutdown().await;
}
