//! `IntegratedServer::shutdown` must return even when it fires before its
//! background tasks have ever been polled.
//!
//! # The defect
//!
//! Every background task in `crate::integrated` is `select!`ed against a shutdown
//! signal, and `shutdown()` **joins** several of them — the connection task, the
//! tick task, the query listener, LAN discovery. `tokio::sync::Notify`'s
//! `notify_waiters()` stores no permit: it wakes whoever is registered at that
//! instant, and a `notified()` future registers only when first polled. A
//! `shutdown()` that runs before a just-spawned task's first poll therefore loses
//! the notification outright — the signal arm never completes, the other arm is a
//! serve loop that never returns on its own, and `join().await` waits forever.
//!
//! Reported as `tests/level_dat_round_trip.rs` hanging for ~25 minutes in a
//! contended workspace run and holding the shared cargo lock. That binary passes
//! in 0.8 s alone, measured twice, which is the signature of a scheduling race
//! rather than of a slow test.
//!
//! # Why this gate is deterministic where that one was a coin flip
//!
//! `level_dat_round_trip.rs`'s tests are `#[tokio::test(flavor =
//! "multi_thread", worker_threads = 2)]`, so a spawned task is usually picked up
//! by the second worker before `shutdown()` gets there and the notification is
//! usually delivered. Hence "passes alone, hangs under load".
//!
//! This file uses the **default `#[tokio::test]`**, which is a `current_thread`
//! runtime. There, `spawn` only queues a task; nothing polls it until the current
//! task yields. So "shutdown fires before the task's first poll" is not a race to
//! be won, it is the guaranteed ordering — the discriminating input, chosen because
//! the multi-threaded flavour is the one where both hypotheses coincide.
//!
//! Every wait is wrapped in a real timeout, so a regression **fails** rather than
//! hanging: a gate for a hang must not itself be able to hang, or it reintroduces
//! the very hazard it was written for.

use std::path::Path;
use std::time::Duration;

use lodestone_core::State;
use lodestone_server::{ChunkColumn, ChunkSource, ServerBound, ServerDirective, ServerProtocol};
use uuid::Uuid;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
/// Generous against a loaded machine and far below any plausible "it hung"
/// reading. The measured healthy shutdown is milliseconds; the defect is
/// unbounded, so anything in between separates them.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(20);

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
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block(x, 60, z, "minecraft:stone");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; this fixture serves fresh columns by design.
    }
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-shutdown-{tag}-3q7v"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp world dir");
    dir
}

fn open(dir: &Path) -> lodestone_server::IntegratedServer {
    let (server, client, _world) = lodestone_server::IntegratedServer::open_persistent_with_mobs(
        SilentProtocol,
        dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        1,
        Duration::from_secs(3600),
    )
    .expect("open persistent world");
    // **Held deliberately, and this is load-bearing.** With the client end dropped
    // the connection task would finish on its own and the join would return
    // regardless of the signal — the precondition species of vacuous test, and the
    // reason `level_dat_round_trip.rs` binds its own client end too. Leaking it
    // keeps the task's only exit the shutdown signal.
    std::mem::forget(client);
    server
}

/// **The gate.** Shut down before anything has been polled.
///
/// The `open` above spawns the connection task, the tick task and the autosave
/// task; on this runtime none of them has run a single instruction when
/// `shutdown()` fires. If the signal is not sticky, the join inside it never
/// returns.
#[tokio::test]
async fn shutdown_returns_when_it_fires_before_the_tasks_first_poll() {
    let dir = tempdir("before-poll");
    let server = open(&dir);

    let outcome = tokio::time::timeout(SHUTDOWN_DEADLINE, server.shutdown()).await;

    assert!(
        outcome.is_ok(),
        "shutdown did not return within {SHUTDOWN_DEADLINE:?}: the signal was fired \
         before the background tasks were first polled and was lost, so the join \
         inside shutdown will never complete"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The control for the input**, run rather than described: with a yield between
/// open and shutdown the tasks get their first poll, register as waiters, and even
/// a non-sticky signal reaches them.
///
/// So this passes both before and after the fix — which is the measured reason the
/// multi-threaded gates in `level_dat_round_trip.rs` were a coin flip rather than a
/// reliable red. It is here so the choice of `current_thread` above is a fact and
/// not a claim.
#[tokio::test]
async fn a_yield_before_shutdown_is_the_input_that_cannot_discriminate() {
    let dir = tempdir("after-poll");
    let server = open(&dir);

    // One yield is enough on a current_thread runtime: it drains the spawn queue,
    // so every task reaches its `select!` and registers.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let outcome = tokio::time::timeout(SHUTDOWN_DEADLINE, server.shutdown()).await;
    assert!(
        outcome.is_ok(),
        "with the tasks already registered as waiters, shutdown must return under \
         either signal implementation"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
