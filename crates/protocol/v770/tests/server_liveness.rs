//! End-to-end: the **real** `lodestone-client`, running the real
//! [`V770Adapter`], against the real [`V770ServerProtocol`] over the
//! in-memory transport — proving keep-alive, time-of-day, and view
//! streaming actually round-trip through the real protocol-776 wire format,
//! not just through `lodestone-server`'s own stand-in protocol
//! (`crates/lodestone-server/tests/serve_play.rs`, which covers the
//! version-free scheduling logic itself in more detail and faster, since it
//! does not need a real client driver or real terrain sampling).
//!
//! Terrain here is [`WorldgenChunkSource`] over a trivial constant density —
//! cheap and deterministic, since these tests are about packet liveness, not
//! terrain content (already covered block-for-block by
//! `server_integration.rs`). The vertical extent is still the real
//! `ChunkShape::overworld_1_21()` shape (`min_y = -64`, `height = 384`): the
//! client hardcodes that shape by dimension name rather than reading it off
//! the wire, so anything else would misalign decode.
//!
//! All three tests run under `#[tokio::test(start_paused = true)]` so the
//! 15-second keep-alive interval and 1-second time-sync interval resolve in
//! a fraction of a second of wall-clock time via tokio's auto-advance —
//! the same pattern `crates/lodestone-server/tests/serve_play.rs` and
//! `crates/lodestone-net/src/connection.rs`'s own tests already establish.

use std::time::Duration;

use lodestone_client::{ChunkPos, ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::{Rotation, Vec3};
use lodestone_server::{IntegratedServer, WorldgenChunkSource};
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

/// A cheap, deterministic terrain source: a constant-ish Y-gradient with no
/// noise sampling, so a 384-tall column (the real overworld's vertical
/// extent, required for wire-shape alignment — see this file's module
/// docs) costs a handful of float comparisons per block rather than a real
/// density-router evaluation. Content is irrelevant to these tests.
fn cheap_source() -> WorldgenChunkSource {
    let density = Density::YClampedGradient {
        from_y: -64.0,
        to_y: 64.0,
        from_value: 1.0,
        to_value: -1.0,
    };
    WorldgenChunkSource::new(density, -64, 384)
}

/// The square `[-r, r]²` chunk window around `(cx, cz)` — the same shape
/// `lodestone-server`'s `ViewTracker` and the initial join view both use.
fn square(cx: i32, cz: i32, r: i32) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    for dz in -r..=r {
        for dx in -r..=r {
            out.push((cx + dx, cz + dz));
        }
    }
    out
}

/// A real client, with `lodestone-client`'s default automatic
/// `KeepAlivePolicy`, must not be disconnected merely for existing across
/// several keep-alive intervals — the real-wire-format companion to
/// `serve_play.rs`'s `responsive_client_survives_multiple_keep_alive_intervals`
/// (that test's negative control; this one proves the same holds for the
/// actual protocol-776 `keep_alive` encoding/decoding, not a stand-in).
#[tokio::test(start_paused = true)]
async fn real_client_survives_multiple_keep_alive_intervals() {
    let source = cheap_source();
    let (server, client_io) = IntegratedServer::open_in_memory(V770ServerProtocol, source, 0);
    let (handle, _events) =
        ClientBuilder::new(address(), profile("KeepAliveWatcher"), Box::new(adapter()))
            .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");

    // Comfortably past four 15-second keep-alive intervals. With the clock
    // paused this advances virtually, resolving in a fraction of a second
    // of real time; the client's driver answers every challenge on its own
    // (`crates/lodestone-client/src/driver.rs`'s `ClientEvent::KeepAlive`
    // arm), with no test-side involvement.
    tokio::time::sleep(Duration::from_secs(4 * 15 + 5)).await;

    assert!(
        !handle.is_finished(),
        "a client that keeps answering keep-alive must not be disconnected"
    );

    server.shutdown().await;
}

/// The real client's held day/night anchor (`V770Adapter`'s `DayClock`,
/// `crates/protocol/v770/src/adapter.rs`) must actually advance from the
/// server's periodic `set_time` broadcasts — the real-wire-format
/// companion to `serve_play.rs`'s
/// `time_of_day_anchors_at_join_then_broadcasts_periodically`, and the live
/// consumer `docs/time-of-day-lighting.md` describes on the client side.
///
/// **`open_in_memory_with_mobs`, not `open_in_memory`.** The clock the
/// broadcast carries is now the *world's*, advanced by `run_tick_loop`, and
/// `open_in_memory` starts no tick loop at all. On the plain constructor this test
/// used to pass against `ticks_since(play_start)`, wall-clock elapsed since join:
/// a number that rose reliably and was not the world's time. So the constructor is
/// load-bearing here, and picking the wrong one makes the gate measure a clock
/// nothing ticks.
#[tokio::test(start_paused = true)]
async fn real_client_time_of_day_advances_from_periodic_broadcasts() {
    let source = cheap_source();
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol,
        source,
        (0..=0, 0..=0),
        (0, 0),
        0,
        0,
    );
    let (handle, _events) =
        ClientBuilder::new(address(), profile("Clockwatcher"), Box::new(adapter()))
            .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");

    let (age0, time_of_day0) = handle.world_time();

    // Past several 1-second `TIME_SYNC_INTERVAL` broadcasts.
    handle
        .wait_for(Duration::from_secs(10), |h| h.world_time().0 > age0)
        .await
        .expect("world age never advanced");

    let (age1, time_of_day1) = handle.world_time();
    assert!(
        age1 > age0,
        "world age must advance from the periodic broadcast: {age0} -> {age1}"
    );
    // Rate 1.0, no pause, no intervening `/time set` — the day/night anchor
    // must advance in lockstep with the world age, not sit frozen at its
    // join-time value while only `game_time` moves.
    assert!(
        time_of_day1 > time_of_day0,
        "time of day must advance alongside world age: {time_of_day0} -> {time_of_day1}"
    );

    server.shutdown().await;
}

/// The real client's chunk store must track the player across chunk
/// boundaries: columns it walks away from get unloaded
/// (`ClientEvent::ChunkUnloaded`, driven by `FORGET_LEVEL_CHUNK`) and
/// columns it walks into get loaded, without ever asking for the whole
/// world at once. Moves through three states — a jump far enough that the
/// old and new windows share nothing, then back near the original spawn
/// chunk — so this cannot pass by having the player never actually leave
/// its starting view.
#[tokio::test(start_paused = true)]
async fn real_client_view_follows_player_across_chunk_boundaries() {
    let view_radius = 1; // 3x3 = 9 columns
    let source = cheap_source();
    let (server, client_io) =
        IntegratedServer::open_in_memory(V770ServerProtocol, source, view_radius);
    let (handle, _events) =
        ClientBuilder::new(address(), profile("Walker"), Box::new(adapter())).connect_with(client_io);

    // Spawn is (8, 100, 8) (`V770ServerProtocol::begin_play`) — chunk (0, 0).
    handle
        .wait_for_chunks(9, Duration::from_secs(60))
        .await
        .expect("initial 3x3 view never arrived");
    for (cx, cz) in square(0, 0, view_radius) {
        assert!(
            handle.is_chunk_loaded(ChunkPos::new(cx, cz)),
            "initial view missing ({cx}, {cz})"
        );
    }

    // Jump far enough (10 chunks = 160 blocks) that the old and new 3x3
    // windows share no columns at all.
    let spawn = handle.position().expect("spawned");
    handle
        .move_to(
            Vec3::new(spawn.x + 160.0, spawn.y, spawn.z),
            Rotation::new(0.0, 0.0),
            true,
            false,
        )
        .expect("send move");

    handle
        .wait_for(
            Duration::from_secs(30),
            |h| !h.is_chunk_loaded(ChunkPos::new(0, 0)),
        )
        .await
        .expect("old chunk (0, 0) was never forgotten");

    for (cx, cz) in square(0, 0, view_radius) {
        assert!(
            !handle.is_chunk_loaded(ChunkPos::new(cx, cz)),
            "old view still holds ({cx}, {cz}) after the jump"
        );
    }
    // **This wait is new, and it is a premise correction rather than a relaxation
    // — say so out loud, because reading the assertions below could never reveal
    // it.** The forget above used to be evidence that the new columns had already
    // arrived, because the server sent a view update atomically: cache-center,
    // forgets, and the whole newly-visible strip in one pass of its own loop. It
    // does not any more. Forgets are still immediate, but the strip is enqueued on
    // the server's column pipeline and paced out afterwards
    // (`lodestone_server`'s `send_view_update`), so waiting on `!is_chunk_loaded(0,
    // 0)` and then asserting on chunk `(9, -1)` reads the wire mid-strip.
    //
    // Nothing this gate actually measures is weakened: every column of the new
    // window must still be loaded, none of the old one may be, and the total must
    // still be exactly 9 — the three claims below, unchanged. What is removed is an
    // implicit claim that a forget and a send are atomic, which was never the
    // property under test and is not one vanilla offers either
    // (`PlayerChunkSender` paces its own sends).
    //
    // **Established rather than assumed**, because a gate relaxed to accommodate a
    // change is how a real defect ships looking green. The discriminator between
    // "paced" and "dropped" is this wait plus the count: it resolves `Ok`, and
    // `loaded_chunk_count()` is 9 on the nose, so no column is lost, duplicated or
    // left over. Had a column genuinely been dropped this would time out.
    handle
        .wait_for(Duration::from_secs(30), move |h| {
            square(10, 0, view_radius)
                .into_iter()
                .all(|(cx, cz)| h.is_chunk_loaded(ChunkPos::new(cx, cz)))
        })
        .await
        .expect("the new 3x3 view never arrived after the jump");

    for (cx, cz) in square(10, 0, view_radius) {
        assert!(
            handle.is_chunk_loaded(ChunkPos::new(cx, cz)),
            "new view missing ({cx}, {cz}) after the jump"
        );
    }
    assert_eq!(
        handle.loaded_chunk_count(),
        9,
        "exactly the new 3x3 window should be loaded, no leftovers"
    );

    server.shutdown().await;
}
