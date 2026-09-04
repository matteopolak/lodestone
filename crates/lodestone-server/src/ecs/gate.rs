//! Production wiring gates for the server ECS `World`.
//!
//! # Why this test is in `src/` and not `tests/`
//!
//! These tests need access to the private ECS setup, so they live beside it
//! rather than in the external integration-test crate.
//!
//! # Why it drives `IntegratedServer` rather than `ServerApp::bootstrap`
//!
//! Because a hand-built `App` passes whether or not production wires anything,
//! and that is precisely how `WindowApp.ecs` (an inert scaffold
//! nothing reads) happened on the client. The subject here is
//! `IntegratedServer::open_in_memory_with_mobs`, the same call a real
//! singleplayer session makes, observed through the same public
//! `server_tick_count()` accessor a shell would use.

use lodestone_core::State;
use uuid::Uuid;

use crate::chunk::{ChunkColumn, ChunkSource};
use crate::integrated::IntegratedServer;
use crate::protocol::{ServerBound, ServerDirective, ServerProtocol};

/// Every column is bare air — the cheapest terrain that still lets
/// `MobHandle::seeded` build a `ChunkWorld`. Mirrors `tick.rs`'s own
/// `EmptyWorld` fixture rather than sharing it, because that one is private to
/// that module's test block.
struct AirWorld;

impl ChunkSource for AirWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The plain column-regenerating form; this gate only drives a server
        // tick, it never places blocks, so a cheap read is not needed.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // The plain column-regenerating form; this gate only drives a server
        // tick, it never places blocks, so a cheap read is not needed.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    // Built into `IntegratedServer` (which wraps sources in a `ChunkStore`),
    // so a player action could reach this through the store's write-through.
    // The source has no storage — `column()` is a fresh blank column — so the
    // edit is deliberately discarded. Explicit rather than inherited.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design for this fixture.
    }
}

/// The minimum `ServerProtocol`: the seven required methods, each answering with
/// something inert. This gate never inspects wire bytes.
#[derive(Debug)]
struct Silent;

impl ServerProtocol for Silent {
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

/// Builds the production singleplayer server the way a shell does.
fn production_server() -> IntegratedServer {
    let (server, _client) = IntegratedServer::open_in_memory_with_mobs(
        Silent,
        AirWorld,
        (0..=0, 0..=0),
        (0, 0),
        0,
        1,
    );
    server
}

/// Wait until the tick task has completed exactly `expected` clock ticks.
///
/// `TickStats` and `ServerTickWitness` are independent atomic observations,
/// so they are not a synchronized snapshot. Reaching this barrier through the
/// clock's final `record_tick` is what makes the witness assertion below a
/// statement about the same completed tick rather than a race between two
/// readers. The bounded cooperative polling makes a missing completion fail
/// deterministically instead of hanging this test forever.
async fn wait_for_completed_ticks(server: &IntegratedServer, expected: u64) {
    const MAX_YIELDS: usize = 100;

    for _ in 0..MAX_YIELDS {
        let observed = server
            .tick_stats()
            .expect("a production tick-task constructor must expose TickStats")
            .tick_count;
        match observed.cmp(&expected) {
            std::cmp::Ordering::Equal => return,
            std::cmp::Ordering::Greater => panic!(
                "the tick task completed {observed} ticks while waiting for exactly {expected}; \
                 paused time must not create an extra world tick"
            ),
            std::cmp::Ordering::Less => tokio::task::yield_now().await,
        }
    }

    panic!(
        "the tick task did not complete {expected} ticks after {MAX_YIELDS} cooperative yields; \
         its completion path is not live"
    );
}

/// Constructing a real integrated server must build a server `World` and run
/// its startup schedule once.
///
/// The assertion is an exact `Some(1)`, not `>= 1` and not "is some": one
/// `ServerBoot` run, one `advance_server_tick` execution. `Some(0)` is the
/// island — the `App` was constructed and no schedule ran against it, which is
/// the same inert-scaffold shape. `None` means production stopped constructing
/// the `World` at all. The paused-time lockstep gates below cover the later
/// `GameTick` executions separately.
///
/// No polling, no timing, no `yield_now`: `open_in_memory_with_mobs` calls
/// `ServerApp::bootstrap` **synchronously**, before it spawns anything, so this
/// is deterministic under any load. Keep it that way — a gate that has to wait
/// for a background task is a gate that can go green by accident.
#[tokio::test]
async fn the_production_integrated_server_runs_a_registered_system() {
    let server = production_server();
    assert_eq!(
        server.server_tick_count(),
        Some(1),
        "production must build a server World and run ServerBoot exactly once; \
         Some(0) means no startup schedule ran, None means the World is no longer constructed"
    );
}

/// The same gate for the **LAN** path. `IntegratedServer::bind` gained its own
/// world-tick loop, and "one world, one loop" means it gets its
/// own server `World` rather than sharing singleplayer's — so it needs its own
/// evidence that the `World` is live, not an inference from the constructor
/// above.
///
/// Port `0` so the OS assigns one and this never races another test.
#[tokio::test]
async fn the_production_lan_server_runs_a_registered_system() {
    let server = IntegratedServer::bind("127.0.0.1:0", Silent, AirWorld, 1)
        .await
        .expect("binding loopback on an OS-assigned port must succeed");
    assert_eq!(
        server.server_tick_count(),
        Some(1),
        "open-to-LAN must build its own server World and run ServerBoot exactly once"
    );
}

/// The primary singleplayer loop must drive `GameTick` once for every completed
/// world tick. The clock completion barrier is deliberate: its final
/// `record_tick` runs after `GameTick`, so observing exactly `TICKS` there
/// establishes that the witness must already include the same `TICKS` runs.
#[tokio::test(start_paused = true)]
async fn the_primary_in_memory_tick_loop_drives_game_tick_in_lockstep() {
    const TICKS: u64 = 5;

    let server = production_server();
    assert_eq!(server.server_tick_count(), Some(1), "ServerBoot must run once at construction");

    // `tokio::spawn` does not poll synchronously. Establish its `sleep_until`
    // baseline before advancing virtual time, or the first period could be lost.
    tokio::task::yield_now().await;
    for _ in 0..TICKS {
        tokio::time::advance(crate::tick::TICK_PERIOD).await;
    }
    wait_for_completed_ticks(&server, TICKS).await;

    assert_eq!(
        server.server_tick_count(),
        Some(1 + TICKS),
        "ServerBoot plus exactly one GameTick per completed primary-world tick"
    );
    assert_eq!(
        server.tick_stats().expect("primary world has TickStats").overrun_count,
        0,
        "regular paused-time periods must not record an overrun"
    );
    server.shutdown().await;
}

/// The LAN primary loop has its own `World`, and therefore needs the same
/// lockstep proof rather than inheriting singleplayer's result by inference.
#[tokio::test(start_paused = true)]
async fn the_primary_lan_tick_loop_drives_game_tick_in_lockstep() {
    const TICKS: u64 = 5;

    let server = IntegratedServer::bind("127.0.0.1:0", Silent, AirWorld, 1)
        .await
        .expect("binding loopback on an OS-assigned port must succeed");
    assert_eq!(server.server_tick_count(), Some(1), "ServerBoot must run once at construction");

    tokio::task::yield_now().await;
    for _ in 0..TICKS {
        tokio::time::advance(crate::tick::TICK_PERIOD).await;
    }
    wait_for_completed_ticks(&server, TICKS).await;

    assert_eq!(
        server.server_tick_count(),
        Some(1 + TICKS),
        "ServerBoot plus exactly one GameTick per completed LAN primary-world tick"
    );
    assert_eq!(
        server.tick_stats().expect("LAN primary world has TickStats").overrun_count,
        0,
        "regular paused-time periods must not record an overrun"
    );
    server.shutdown().await;
}

/// Negative control's encodable half: a constructor that does **not** build a
/// server `World` must report `None`, so the gate above is distinguishing
/// "production wired this" from "the accessor always answers something
/// plausible". `open_in_memory` is that constructor today — it spawns no tick
/// task, so per `docs/server-ecs.md` there is nobody to own a `World`.
///
/// The other half of the control — deleting production's `run_schedule` call and
/// watching the gate fail — is a statement about a *different build* of this
/// crate and so cannot be encoded here. It was run by hand; the observed failure
/// is recorded in `docs/server-ecs-phase0.md`.
#[tokio::test]
async fn a_constructor_with_no_tick_task_reports_no_world() {
    let (server, _client) = IntegratedServer::open_in_memory(Silent, AirWorld, 1);
    assert_eq!(
        server.server_tick_count(),
        None,
        "control failed: a handle with no tick task reported a server World, so the gate \
         above cannot tell a wired constructor from an unwired one"
    );
}
