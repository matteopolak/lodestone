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

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bevy_app::{App, Plugin};
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::Res;
use lodestone_core::State;
use lodestone_data::block_states::StateId;
use uuid::Uuid;

use crate::chunk::{ChunkColumn, ChunkSource};
use crate::ecs::{GameTick, ServerApp, TickSet};
use crate::integrated::IntegratedServer;
use crate::protocol::{ServerBound, ServerDirective, ServerProtocol};

#[derive(Resource, Clone)]
struct PluginTickCount(Arc<AtomicU64>);

#[derive(Clone)]
struct CountingServerPlugin(Arc<AtomicU64>);

impl Plugin for CountingServerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(PluginTickCount(Arc::clone(&self.0)));
        app.add_systems(
            GameTick,
            (|count: Res<PluginTickCount>| {
                count.0.fetch_add(1, Ordering::Relaxed);
            })
            .in_set(TickSet::Publish),
        );
    }
}

/// Every column is bare air — the cheapest terrain that still lets
/// `MobHandle::new` build a `ChunkWorld`. Mirrors `tick.rs`'s own
/// `EmptyWorld` fixture rather than sharing it, because that one is private to
/// that module's test block.
struct AirWorld;

impl ChunkSource for AirWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        // The plain column-regenerating form; this gate only drives a server
        // tick, it never places blocks, so a cheap read is not needed.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state_id(lx, y, lz)
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
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {
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
        1,
    );
    // No client joins, so lift the holds that wait for one.
    server.world_state().release_initial_tick_holds();
    server
}

/// Real-time budget for any wait below. The tick task runs on its own OS
/// thread against the wall clock, so a heavily loaded machine only slows these
/// tests down; it must never fail them. Nothing asserts on elapsed time.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// Real-time poll interval while waiting on the tick thread.
const POLL: std::time::Duration = std::time::Duration::from_millis(2);

fn completed_ticks(server: &IntegratedServer) -> u64 {
    server
        .tick_stats()
        .expect("a production tick-task constructor must expose TickStats")
        .tick_count
}

/// Wait until the tick task has completed at least `expected` clock ticks.
async fn wait_for_completed_ticks(server: &IntegratedServer, expected: u64) {
    let start = lodestone_time::Instant::now();
    while completed_ticks(server) < expected {
        assert!(
            start.elapsed() < DEADLINE,
            "the tick task did not complete {expected} ticks within {DEADLINE:?} \
             (observed {}); its completion path is not live",
            completed_ticks(server)
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Poll a counter that the tick task advances until it settles on a consistent
/// relationship with the clock's completed-tick count.
///
/// `expected(n)` is the value `read` must report once `n` world ticks have
/// completed. `TickStats` and the observable are independent atomics, and a
/// tick's `GameTick` run precedes its clock `record_tick`, so a sample taken
/// mid-tick can legitimately see the observable one tick ahead of the clock.
/// Every sample is therefore bracketed: a read taken between clock reads `a`
/// and `b` must lie in `expected(a)..=expected(b + 1)`, which fails at once on
/// a counter that lags or runs ahead. The wait ends on the first sample whose
/// two clock reads agree, which has reached at least `min_ticks`, and whose
/// value equals `expected` exactly; a sample inside the one-tick window is
/// retried, not accepted. `expected` must be non-decreasing. Returns the clock
/// count of the accepted sample.
async fn wait_for_tracking(
    server: &IntegratedServer,
    min_ticks: u64,
    expected: impl Fn(u64) -> u64,
    read: impl Fn() -> u64,
) -> u64 {
    let start = lodestone_time::Instant::now();
    loop {
        let before = completed_ticks(server);
        let value = read();
        let after = completed_ticks(server);
        assert!(
            value >= expected(before) && value <= expected(after + 1),
            "observable {value} is outside [{}, {}] for clock ticks {before}..={after}",
            expected(before),
            expected(after + 1)
        );
        if before == after && before >= min_ticks && value == expected(before) {
            return before;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "no consistent sample within {DEADLINE:?}: clock {before}..={after}, observable \
             {value}, expected {} (needed at least {min_ticks} ticks)",
            expected(after)
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Constructing a real integrated server must build a server `World` and run
/// its startup schedule once.
///
/// The assertion is an exact `Some(1)`, not `>= 1` and not "is some": one
/// `ServerBoot` run, one `advance_server_tick` execution. `Some(0)` is the
/// island — the `App` was constructed and no schedule ran against it, which is
/// the same inert-scaffold shape. `None` means production stopped constructing
/// the `World` at all. The lockstep gates below cover the later
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
    // No client joins, so lift the holds that wait for one.
    server.world_state().release_initial_tick_holds();
    assert_eq!(
        server.server_tick_count(),
        Some(1),
        "open-to-LAN must build its own server World and run ServerBoot exactly once"
    );
}

/// The primary singleplayer loop must drive `GameTick` once for every completed
/// world tick. The tick runs on its own thread in real time, so the test cannot
/// stop the clock at an exact count; it takes a snapshot whose two clock reads
/// agree and requires the witness to equal `ServerBoot` plus that many runs.
#[tokio::test]
async fn the_primary_in_memory_tick_loop_drives_game_tick_in_lockstep() {
    const TICKS: u64 = 5;

    let server = production_server();
    assert_eq!(server.server_tick_count(), Some(1), "ServerBoot must run once at construction");

    wait_for_tracking(
        &server,
        TICKS,
        |ticks| 1 + ticks,
        || server.server_tick_count().expect("primary world has a server World"),
    )
    .await;
    server.shutdown().await;
}

/// A caller-composed server plugin must survive the production constructor and
/// run on the primary world's real tick task, not only on a hand-built `App`.
#[tokio::test]
async fn a_supplied_server_plugin_runs_on_the_primary_world_tick_task() {
    const TICKS: u64 = 4;

    let observed = Arc::new(AtomicU64::new(0));
    let plugin_observed = Arc::clone(&observed);
    let server_app = ServerApp::bootstrap_with(|app| {
        app.add_plugins(CountingServerPlugin(plugin_observed));
    });
    let (server, _client) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        Silent,
        AirWorld,
        (0..=0, 0..=0),
        (0, 0),
        1,
        server_app,
    );
    // No client joins, so lift the holds that wait for one.
    server.world_state().release_initial_tick_holds();

    assert_eq!(
        observed.load(Ordering::Relaxed),
        0,
        "a GameTick plugin must not run during ServerBoot"
    );
    wait_for_tracking(&server, TICKS, |ticks| ticks, || observed.load(Ordering::Relaxed)).await;
    server.shutdown().await;
}

/// Delayed and repeating callbacks must survive the production `App` → `World`
/// handoff with the same tick schedule as the scheduler's focused tests. The
/// exact trace distinguishes a one-tick phase error from a mere firing count.
#[tokio::test]
async fn the_production_primary_world_runs_deterministic_scheduler_tasks() {
    let observed = Arc::new(AtomicU64::new(0));
    let task_observed = Arc::clone(&observed);
    let server_app = ServerApp::bootstrap_with(|app| {
        app.world_mut()
            .resource_mut::<crate::ecs::ServerTaskScheduler>()
            .schedule_repeating(2, 3, move |_, _| {
                task_observed.fetch_add(1, Ordering::Relaxed);
            });
    });
    let (server, _client) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        Silent,
        AirWorld,
        (0..=0, 0..=0),
        (0, 0),
        1,
        server_app,
    );
    // No client joins, so lift the holds that wait for one.
    server.world_state().release_initial_tick_holds();

    assert_eq!(observed.load(Ordering::Relaxed), 0, "ServerBoot must not advance GameTick");
    // First fire on GameTick 2, then every third: 0, 1, 1, 1, 2, 2, 2, 3, ...
    // Bracketing every sample against this trace catches a one-tick phase
    // error as well as a wrong firing count.
    wait_for_tracking(
        &server,
        8,
        |ticks| if ticks < 2 { 0 } else { 1 + (ticks - 2) / 3 },
        || observed.load(Ordering::Relaxed),
    )
    .await;
    server.shutdown().await;
}

/// The async scheduler is only useful if its result crosses the production
/// extracted-`World` boundary. A hand-built scheduler test cannot prove that
/// the primary tick task drains this queue after `ServerApp::into_world`.
#[tokio::test]
async fn completed_async_work_reaches_the_production_primary_world_tick_task() {
    let observed = Arc::new(AtomicU64::new(0));
    let hand_back_observed = Arc::clone(&observed);
    let server_app = ServerApp::bootstrap_with(|app| {
        app.world_mut()
            .resource_mut::<crate::ecs::ServerTaskScheduler>()
            .spawn_with_handback(
                || 19_u64,
                move |value, _| {
                    hand_back_observed.fetch_add(value, Ordering::Relaxed);
                },
            )
            .expect("one async hand-back fits the scheduler's default capacity");
    });
    let (server, _client) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        Silent,
        AirWorld,
        (0..=0, 0..=0),
        (0, 0),
        1,
        server_app,
    );
    // No client joins, so lift the holds that wait for one.
    server.world_state().release_initial_tick_holds();

    // The worker runs off-thread in real time, so the hand-back lands on some
    // tick; it must land within the deadline, exactly once.
    let start = lodestone_time::Instant::now();
    while observed.load(Ordering::Relaxed) != 19 {
        assert!(
            observed.load(Ordering::Relaxed) < 19 && start.elapsed() < DEADLINE,
            "a completed worker result must be drained by the production primary world \
             (observed {})",
            observed.load(Ordering::Relaxed)
        );
        tokio::time::sleep(POLL).await;
    }
    let landed = completed_ticks(&server);
    wait_for_completed_ticks(&server, landed + 4).await;
    assert_eq!(
        observed.load(Ordering::Relaxed),
        19,
        "the hand-back must run exactly once, not once per later tick"
    );
    server.shutdown().await;
}

#[derive(bevy_ecs::message::Message)]
struct PluginNotice(u64);

struct NoticeProducer;

impl Plugin for NoticeProducer {
    fn build(&self, app: &mut App) {
        app.add_message::<PluginNotice>();
        app.add_systems(GameTick, (
            |mut sequence: bevy_ecs::system::Local<u64>,
             mut notices: bevy_ecs::message::MessageWriter<PluginNotice>| {
                *sequence += 1;
                notices.write(PluginNotice(*sequence));
            }
        ).in_set(TickSet::Apply));
    }
}

struct NoticeConsumer(Arc<AtomicU64>);

impl Plugin for NoticeConsumer {
    fn build(&self, app: &mut App) {
        app.add_message::<PluginNotice>();
        app.insert_resource(PluginTickCount(Arc::clone(&self.0)));
        app.add_systems(GameTick, (
            |mut reader: bevy_ecs::message::MessageReader<PluginNotice>,
             messages: Res<bevy_ecs::message::Messages<PluginNotice>>,
             observed: Res<PluginTickCount>| {
                assert!(messages.len() <= 2, "plugin message history must expire");
                for notice in reader.read() {
                    observed.0.fetch_add(notice.0, Ordering::Relaxed);
                }
            }
        ).in_set(TickSet::Publish));
    }
}

#[tokio::test]
async fn independent_plugins_exchange_bounded_messages_on_the_primary_tick_task() {
    let observed = Arc::new(AtomicU64::new(0));
    let plugin_observed = Arc::clone(&observed);
    let server_app = ServerApp::bootstrap_with(|app| {
        app.add_plugins((NoticeConsumer(plugin_observed), NoticeProducer));
    });
    let (server, _client) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        Silent, AirWorld, (0..=0, 0..=0), (0, 0), 1, server_app,
    );
    // No client joins, so lift the holds that wait for one.
    server.world_state().release_initial_tick_holds();
    assert_eq!(observed.load(Ordering::Relaxed), 0);
    // After n ticks the consumer has summed 1 + 2 + ... + n.
    wait_for_tracking(
        &server,
        4,
        |ticks| ticks * (ticks + 1) / 2,
        || observed.load(Ordering::Relaxed),
    )
    .await;
    server.shutdown().await;
}

/// Control for the plugin gate: carrying the same observable as a resource is
/// insufficient unless a caller actually registers the plugin system.
#[tokio::test]
async fn a_supplied_resource_without_a_plugin_system_never_runs() {
    const TICKS: u64 = 4;

    let observed = Arc::new(AtomicU64::new(0));
    let resource_observed = Arc::clone(&observed);
    let server_app = ServerApp::bootstrap_with(|app| {
        app.insert_resource(PluginTickCount(resource_observed));
    });
    let (server, _client) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        Silent,
        AirWorld,
        (0..=0, 0..=0),
        (0, 0),
        1,
        server_app,
    );
    // No client joins, so lift the holds that wait for one.
    server.world_state().release_initial_tick_holds();

    wait_for_completed_ticks(&server, TICKS).await;

    assert_eq!(
        observed.load(Ordering::Relaxed),
        0,
        "control failed: the observable changed without the plugin system"
    );
    server.shutdown().await;
}

/// The LAN primary loop has its own `World`, and therefore needs the same
/// lockstep proof rather than inheriting singleplayer's result by inference.
#[tokio::test]
async fn the_primary_lan_tick_loop_drives_game_tick_in_lockstep() {
    const TICKS: u64 = 5;

    let server = IntegratedServer::bind("127.0.0.1:0", Silent, AirWorld, 1)
        .await
        .expect("binding loopback on an OS-assigned port must succeed");
    // No client joins, so lift the holds that wait for one.
    server.world_state().release_initial_tick_holds();
    assert_eq!(server.server_tick_count(), Some(1), "ServerBoot must run once at construction");

    wait_for_tracking(
        &server,
        TICKS,
        |ticks| 1 + ticks,
        || server.server_tick_count().expect("LAN primary world has a server World"),
    )
    .await;
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
