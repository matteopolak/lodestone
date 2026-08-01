//! The mining deadlock, hermetically: a `GameTick` system that reaches a
//! `World`-lock-backed `ClientHandle` accessor while the driver holds the
//! `World` **write** guard.
//!
//! # What this is
//!
//! `crate::interact::drive_mining` became a `TickSet::Send` system in Stage 5, so
//! it runs inside `run_schedule(GameTick)` — which the driver runs inside
//! `lodestone_ecs::hold_write`, i.e. under `EcsHandle`'s `parking_lot` **write**
//! guard. It read the held item through `ClientHandle::player_menu`, and that
//! accessor takes `ecs.read()` on the *same* `Arc<RwLock<World>>`.
//! `parking_lot::RwLock` is not reentrant, so the first tick of a real dig
//! wedged the render thread with no panic and no log line.
//!
//! # How it works
//!
//! Two tests, and the pairing is the point:
//!
//! * [`the_world_lock_is_not_reentrant_through_client_handle`] is the **control**.
//!   It performs the exact pre-fix call — `player_menu()` inside a `hold_write`
//!   closure — on a watchdog thread and asserts it does *not* finish. Without it,
//!   the gate below is satisfied by a system that never reached the menu at all.
//! * [`a_full_dig_tick_completes_under_the_world_write_guard`] is the **gate**. It
//!   runs the real `drive_mining` in a real `GameTick` schedule, under a real
//!   write guard, against a real `ClientHandle` that adopted the same `EcsHandle`,
//!   with a loaded chunk and a resolving hardness census — every early return in
//!   `drive_mining` bypassed. It asserts the tick finishes *and* that it produced
//!   the dig packets, so a fix that silently stopped digging would fail it.
//!
//! Every potentially-wedging call runs on a spawned thread joined through a
//! bounded channel: a deadlocked thread is leaked, never awaited, so CI cannot
//! hang on this file.
//!
//! # How to change it
//!
//! The watchdog budget is [`WEDGE_TIMEOUT`]. It is deliberately asymmetric in
//! effect: the gate wants "finished well inside it" (the real work is
//! microseconds) and the control wants "still stuck after it", so a slow machine
//! makes the gate flaky long before it makes the control wrong.
//!
//! If `drive_mining` grows another input, add it to [`Harness::build`] rather
//! than weakening the gate — a resource missing from the `World` is a bevy panic,
//! which this file reports as a *failure to complete*, and that would read as a
//! deadlock. The `Err(RecvTimeoutError::Disconnected)` arm exists to tell those
//! two apart.
//!
//! # Dependencies
//!
//! `lodestone_net::memory_pair` and a fake [`VersionAdapter`] for a hermetic
//! `ClientHandle`; `lodestone_ecs` for the handle, the schedule and the component
//! set; `lodestone::interact` for the system under test.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::Duration;

use lodestone::interact::{Attacking, MiningPredictor, NetHandle, ParticleSim, RayTarget};
use lodestone::particles::Particles;
use lodestone::raycast::RayHit;
use lodestone_client::{
    ClientBuilder, ClientHandle, ConnectionState, Directive, LoginProfile, ServerAddress,
    VersionAdapter,
};
use lodestone_ecs::ecs::schedule::Schedule;
use lodestone_ecs::ecs::world::World as EcsWorld;
use lodestone_ecs::player::{ActionQueue, Egress};
use lodestone_ecs::session::SessionMenus;
use lodestone_ecs::{EcsHandle, GameTick, LockHolds, VersionData, hold_write};
use lodestone_model::{AdapterError, BlockHardness, ClientAction, ItemStack, ToolMining};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, WorldSink,
};

/// How long a watchdog waits before calling a thread wedged.
///
/// The work under test is microseconds; this is three orders of magnitude of
/// slack so the *gate* cannot flake, while still bounding the *control* to
/// something a test run can afford to wait out.
const WEDGE_TIMEOUT: Duration = Duration::from_secs(3);

/// The block state the dig targets. Any non-air id the census resolves works;
/// the number itself is not load-bearing.
const STONE: u32 = 1;

/// The world position of that block, and therefore of the pick target.
const TARGET: [i32; 3] = [3, 4, 5];

// ---------------------------------------------------------------------------
// A fake version, standing in for both seams `drive_mining` reads
// ---------------------------------------------------------------------------

/// A [`VersionAdapter`] that speaks no protocol and knows exactly one block.
///
/// It plays two roles at once, which is deliberate: `ClientBuilder` needs *an*
/// adapter to build a handle, and `VersionData` needs one that resolves
/// [`BlockHardness`] or `drive_mining` aborts the dig before it ever reaches the
/// held-item read this file is about. One type, so the two cannot drift apart.
///
/// The hardness is a plain positive number rather than vanilla's stone value: the
/// gate asserts a dig *starts*, not how long it takes, and pinning a real hardness
/// here would be an expected value sourced from our own guess.
#[derive(Debug, Default)]
struct OneBlockVersion;

impl VersionAdapter for OneBlockVersion {
    fn protocol_version(&self) -> i32 {
        0
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["fake"]
    }

    fn supports(&self, _protocol: i32) -> bool {
        true
    }

    fn begin_login(
        &self,
        _profile: &LoginProfile,
        _server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(Vec::new())
    }

    fn handle_packet(
        &self,
        _world: &mut dyn WorldSink,
        _state: ConnectionState,
        _packet_id: i32,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(Vec::new())
    }

    fn encode_action(
        &self,
        _state: ConnectionState,
        _action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        Ok(None)
    }

    fn block_hardness(&self, state_id: u32) -> Option<BlockHardness> {
        (state_id == STONE).then_some(BlockHardness {
            hardness: 1.0,
            requires_correct_tool: false,
        })
    }

    fn tool_mining(&self, _held: Option<&ItemStack>, state_id: u32) -> Option<ToolMining> {
        (state_id == STONE).then_some(ToolMining {
            speed: 1.0,
            correct_tool: true,
            damage_per_block: 0,
        })
    }
}

// ---------------------------------------------------------------------------
// The harness: one `World`, one `ClientHandle` onto it, one loaded chunk
// ---------------------------------------------------------------------------

/// A live-shaped session with no server: the one `EcsHandle`, a `ClientHandle`
/// that adopted it, and the `GameTick` schedule holding the system under test.
struct Harness {
    ecs: EcsHandle,
    client: Arc<ClientHandle>,
    /// The tokio runtime the client's driver task lives on. Dropping it aborts
    /// that task, so it is held for the test's lifetime and never `_`-bound.
    _runtime: tokio::runtime::Runtime,
    /// The far end of the in-memory transport. Held for the same reason: dropping
    /// it closes the connection and ends the session.
    _server_io: tokio::io::DuplexStream,
}

impl Harness {
    /// Builds the whole thing: `World`, session entity, client, chunk, schedule.
    fn build() -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime");

        // `new_ingest_handle` rather than a bare `World`: this is the *authoritative*
        // shape — the one a driver hands down — and it carries the ingest/session
        // systems `ClientBuilder::ecs` requires of the `World` it is given.
        let ecs = lodestone_ecs::new_handle();
        let session = {
            let mut world = ecs.write();
            world.insert_resource(LockHolds::default());
            build_resources(&mut world);
            spawn_player(&mut world)
        };

        // Build the client *inside* the runtime: `connect_with` spawns the driver
        // task, which needs a reactor in scope.
        let (client, server_io) = {
            let _enter = runtime.enter();
            let (client_io, server_io) = lodestone_net::memory_pair();
            let (client, _events) = ClientBuilder::new(
                ServerAddress {
                    host: "memory".into(),
                    port: 0,
                },
                LoginProfile {
                    username: "deadlock".into(),
                    uuid: uuid::Uuid::nil(),
                },
                Box::new(OneBlockVersion),
            )
            // The unification: fold into the `World` we already own, so the
            // handle's `ecs.read()` and our `hold_write` are the same lock.
            .ecs(Arc::clone(&ecs), session)
            .connect_with(client_io);
            (Arc::new(client), server_io)
        };

        // The block the pick ray "hit", written into the *client's* chunk store —
        // which is what `NetHandle::block_at` reads, and which `ClientHandle`
        // hands out as the same `Arc` it writes decoded columns into.
        client.chunk_world().write().load(
            ChunkPos::new(TARGET[0].div_euclid(16), TARGET[2].div_euclid(16)),
            column_with(TARGET, STONE),
        );

        // Publish the handle exactly as the net thread does, then install it.
        let shared: lodestone::net::SharedHandle = Arc::new(OnceLock::new());
        shared
            .set(Arc::clone(&client))
            .expect("a freshly built OnceLock is empty");
        {
            let mut world = ecs.write();
            world.insert_resource(NetHandle(Some(shared)));
            let mut schedule = Schedule::new(GameTick);
            schedule.add_systems(lodestone::interact::drive_mining);
            world.add_schedule(schedule);
        }

        Self {
            ecs,
            client,
            _runtime: runtime,
            _server_io: server_io,
        }
    }
}

/// Every resource `drive_mining` names, set so that no early return fires: the
/// egress gate open, the button held, the ray on [`TARGET`], the census
/// resolving.
fn build_resources(world: &mut EcsWorld) {
    world.insert_resource(Egress {
        in_world: true,
        live: true,
    });
    world.insert_resource(Attacking(true));
    // `+Y`: the face a player punching the top of a block strikes.
    world.insert_resource(RayTarget(Some(RayHit::face_center(TARGET, [0, 1, 0]))));
    world.insert_resource(MiningPredictor::default());
    world.insert_resource(ParticleSim(Particles::new(None)));
    world.insert_resource(ActionQueue::default());
    world.insert_resource(VersionData(Some(Box::new(OneBlockVersion))));
}

/// One entity carrying both halves of the local player's component set — the
/// physics/hotbar half `drive_mining` queries and the session half that holds the
/// folded inventory. **One** entity, because `With<LocalPlayer>` must match
/// exactly one (see `docs/world-unification.md`), which is also why this goes
/// through the real `spawn_local_player` and then *inserts* the session half
/// rather than calling `spawn_session` as well.
fn spawn_player(world: &mut EcsWorld) -> lodestone_ecs::ecs::entity::Entity {
    let entity = lodestone_ecs::spawn_local_player(
        world,
        lodestone_physics::PlayerState::at(lodestone_physics::Vec3d::new(3.5, 5.0, 5.5), 0.0),
    );
    world
        .entity_mut(entity)
        .insert(SessionMenus(lodestone_game::menus::Menus::default()));
    entity
}

/// A single-section column with one non-air block at world `pos`.
fn column_with(pos: [i32; 3], id: u32) -> LoadedChunk {
    let mut column = ChunkColumn::new(
        0,
        16,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        0,
        0,
    );
    column.set_block(
        pos[0].rem_euclid(16) as usize,
        pos[1],
        pos[2].rem_euclid(16) as usize,
        id,
    );
    LoadedChunk::new(column, ColumnLight::new(16), Heightmaps::new(), Vec::new())
}

/// Run `f` on a fresh thread and wait at most [`WEDGE_TIMEOUT`] for it.
///
/// A wedged thread is **leaked on purpose**: joining it is the one thing that
/// would turn this file into the hang it exists to detect.
///
/// The two error arms are kept apart deliberately. `Timeout` is the deadlock this
/// file is about; `Disconnected` is `f` *panicking* — a missing resource, a bevy
/// query that matched nothing, an `expect` in the system. Collapsing them would
/// let a plain setup mistake read as "still deadlocked" and send the next reader
/// hunting a lock that is already fixed.
fn within_budget<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, RecvTimeoutError> {
    let (tx, rx) = sync_channel(1);
    std::thread::spawn(move || {
        let value = f();
        // A full channel cannot happen (capacity 1, one send); a disconnected one
        // means the test already gave up, which is not this thread's problem.
        let _ = tx.send(value);
    });
    rx.recv_timeout(WEDGE_TIMEOUT)
}

/// Assert that `outcome` timed out rather than panicked — "still running", not
/// "died on the way".
fn assert_wedged<T: std::fmt::Debug>(outcome: &Result<T, RecvTimeoutError>, what: &str) {
    match outcome {
        Ok(value) => panic!(
            "{what} completed ({value:?}), but it must not: parking_lot's RwLock is \
             not reentrant. This control no longer detects the hazard, so every gate \
             resting on it proves nothing."
        ),
        Err(RecvTimeoutError::Disconnected) => panic!(
            "{what} panicked instead of wedging, so this control measured the wrong \
             thing — libtest printed the panic above."
        ),
        Err(RecvTimeoutError::Timeout) => {}
    }
}

// ---------------------------------------------------------------------------
// The control
// ---------------------------------------------------------------------------

/// **The control.** Reaching a `World`-lock-backed `ClientHandle` accessor from
/// inside a `hold_write` closure deadlocks, silently and permanently.
///
/// This is the pre-fix call verbatim: `drive_mining` resolved the held item with
/// `net.get().map(ClientHandle::player_menu)`, and `SharedState::player_menu`
/// takes `self.ecs.read()` on the same handle `hold_write` is holding for the
/// `GameTick` schedule.
///
/// Without this test the gate below is vacuous in the *world* sense: a
/// `drive_mining` that returned early (no target, no census, no chunk) would
/// complete just as fast, and nothing would show that the input actually
/// contained the structure the fix exists to handle.
///
/// `chunk_world().read()` is asserted *not* to wedge in the same guard, which is
/// what pins the distinction the §4.1(c) audit turned on: the chunk store is a
/// different lock, so chunk-backed reads from a system were legal all along and
/// are not what broke.
#[test]
fn the_world_lock_is_not_reentrant_through_client_handle() {
    let harness = Harness::build();

    let ecs = Arc::clone(&harness.ecs);
    let client = Arc::clone(&harness.client);
    let chunk_read = within_budget(move || {
        hold_write(&ecs, |_| {
            // The chunk lock is not the `World` lock. This must return.
            client.block_at(lodestone_client::BlockPos::new(
                TARGET[0], TARGET[1], TARGET[2],
            ))
        })
    });
    assert_eq!(
        chunk_read,
        Ok(Some(STONE)),
        "a chunk-backed read under the World write guard must complete — it takes \
         the chunk lock, not this one. If this wedges, the §4.1(c) audit's \
         conclusion is wrong and every chunk read from a system is a deadlock too."
    );

    let ecs = Arc::clone(&harness.ecs);
    let client = Arc::clone(&harness.client);
    let menu_read = within_budget(move || hold_write(&ecs, |_| client.player_menu()));
    assert_wedged(
        &menu_read,
        "ClientHandle::player_menu() inside a World write guard",
    );
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// **The regression gate.** One `GameTick` tick of a live dig, under the write
/// guard, must finish — and must actually dig.
///
/// Every early return in `drive_mining` is bypassed by [`Harness::build`], so the
/// system runs all the way through the held-item resolution that used to
/// deadlock. The action assertion is what stops a "fix" that removed the hang by
/// removing the feature: a tick that produced no `SwingArm` would mean the dig
/// never started, which is the same screen as the freeze from the player's side.
#[test]
fn a_full_dig_tick_completes_under_the_world_write_guard() {
    let harness = Harness::build();
    let ecs = Arc::clone(&harness.ecs);

    let outcome = within_budget(move || {
        hold_write(&ecs, |world| {
            world.run_schedule(GameTick);
            world.resource::<ActionQueue>().0.clone()
        })
    });
    let actions = match outcome {
        Ok(actions) => actions,
        Err(RecvTimeoutError::Timeout) => panic!(
            "one GameTick tick of a live dig never returned under the World write \
             guard: a system in it is taking a second guard on the same lock. See \
             the control above — that is what the freeze-on-punch was."
        ),
        Err(RecvTimeoutError::Disconnected) => panic!(
            "the tick panicked rather than wedging — a resource or component this \
             harness owes drive_mining is missing. This is a harness bug, not the \
             deadlock; libtest printed the panic above."
        ),
    };

    assert!(
        actions
            .iter()
            .any(|a| matches!(a, ClientAction::SwingArm { .. })),
        "the tick completed but queued no SwingArm, so no dig started: {actions:?}. \
         The deadlock is gone but so is block breaking."
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, ClientAction::BlockAction { .. })),
        "no block-action packet was queued, so the server was never told the dig \
         began: {actions:?}"
    );
}
