//! The debris burst at the moment a block finishes breaking.
//!
//! # What this is
//!
//! Before this fix, the **only** place `Particles::destroy_block` (the burst
//! emitter) was called on the live path was `Sim::step`'s `NetUpdate::BlockDestroyed`
//! arm, fed by the server's `ClientboundLevelEventPacket` id `2001`. Verified
//! against `.cache/mc/26.2/src` (see `crate::interact::drive_mining`'s doc
//! comment on the fix): the player's own break is handled server-side by
//! `ServerPlayerGameMode.destroyBlock`, which calls `Level.removeBlock` — a
//! plain block-state write that never touches `levelEvent` at all. The `2001`
//! event only exists on the *separate* `Level.destroyBlock` method, which a
//! cascading break (a torch losing support, fire, an explosion) goes through
//! instead. So the player's own break could never produce a burst; only a
//! break some other cause reported could.
//!
//! The fix adds a local **predicted** emit in `drive_mining` — vanilla's
//! `MultiPlayerGameMode.destroyBlock` does the identical thing client-side,
//! synchronously, without waiting for a server round trip.
//!
//! # The same emit, keyed on the wrong thing
//!
//! That fix keyed the emit on the predictor queueing
//! `BlockAction { action: StopDestroy, .. }`. Reported from play afterwards:
//! punching grass and saplings still produced no debris, while blocks that take
//! a real mining cycle did.
//!
//! `StopDestroy` is not the moment a block breaks — it is one of the **four**
//! ways vanilla's `MultiPlayerGameMode` reaches its single `destroyBlock(pos)`
//! funnel (the two creative branches, `startDestroyBlock`'s instant-break
//! branch, and `continueDestroyBlock`'s progress-reached-`1.0` branch), and it
//! is the one a one-shot block never takes: `Mining::start`'s
//! `progress_per_tick() >= 1.0` branch emits `StartDestroy` and nothing else,
//! because the block is already gone. The destroy effect hangs off the funnel,
//! not off a packet — `destroyBlock` → `Block.playerWillDestroy` →
//! `Block.spawnDestroyParticles` → `level.levelEvent(player, 2001, pos, id)`,
//! which `ClientLevel` dispatches locally into `LevelEventHandler`'s `case
//! 2001`.
//!
//! So `Mining` now latches the destruction itself
//! (`Mining::take_destroyed`), both branches set it, and `drive_mining` keys on
//! that. [`an_instant_break_throws_a_debris_burst_on_its_very_first_tick`] is
//! the gate for the one-shot path;
//! [`a_completed_dig_throws_a_debris_burst_on_the_tick_it_finishes`] keeps the
//! progressive path pinned so the re-keying cannot fix one by breaking the
//! other.
//!
//! # How this gate proves it
//!
//! Reuses `tests/mining_deadlock.rs`'s harness shape (a real `ClientHandle`
//! over an in-memory transport, a real chunk holding one breakable block, and
//! the real `drive_mining` system in a real `GameTick` schedule) rather than a
//! synthetic one, so the gate exercises the actual predictor and the actual
//! particle sink, not a hand-simulated stand-in for either.
//!
//! It ticks the dig to completion and records `Particles::count()` **every**
//! tick. The **control** is every tick before completion: mining a single
//! block produces at most one small per-tick "chip" particle
//! (`breaking_block`), so the count must never jump by more than a few on any
//! of those ticks — if it did, the control would be worthless, because a gate
//! that only checks the final count cannot tell a real burst from noise
//! already present beforehand. The **gate** is the tick `StopDestroy` appears
//! in the queued actions: the count must jump by a large amount that tick,
//! matching a real debris burst (`destroy_block_effect` spawns an `N×N×N`
//! grid, not a handful of particles).
//!
//! # Dependencies
//!
//! Same as `mining_deadlock.rs`: `lodestone_net::memory_pair` and a fake
//! [`VersionAdapter`] for a hermetic `ClientHandle`; `lodestone_ecs` for the
//! handle, schedule and component set; `lodestone::interact` for the system
//! under test. Plus `lodestone_data::{block_states, hardness}` — the jar-derived
//! censuses the instant-break fixture is *derived from* rather than asserted
//! against, so a block that stopped being one-shot cannot leave the gate
//! measuring a slow dig (the *precondition* species of vacuous test, and the one
//! this fixture is most exposed to).

use std::sync::Arc;
use std::sync::OnceLock;

use lodestone::interact::{Attacking, MiningPredictor, NetHandle, ParticleSim, RayTarget};
use lodestone::particles::Particles;
use lodestone::raycast::RayHit;
use lodestone_client::{
    ClientBuilder, ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_ecs::ecs::schedule::Schedule;
use lodestone_ecs::ecs::world::World as EcsWorld;
use lodestone_ecs::player::{ActionQueue, Egress};
use lodestone_ecs::session::SessionMenus;
use lodestone_ecs::{EcsHandle, GameTick, LockHolds, VersionData};
use lodestone_game::mining::BreakInputs;
use lodestone_model::{AdapterError, BlockHardness, ClientAction, ItemStack, ToolMining};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, WorldSink,
};

/// The progressive fixture's block state id. Any non-air id works — the dig
/// timing comes from the fixture's hardness, not from the id.
const STONE: u32 = 1;

/// The progressive fixture's hardness. Deliberately a synthetic `1.0` rather than
/// the census value for `minecraft:stone`: this fixture's whole job is to take
/// *several* ticks, so the "no burst yet" control window exists.
const STONE_HARDNESS: f32 = 1.0;

/// The name of the instant-break fixture. Grass is what the play report named;
/// [`instant_break_fixture`] proves it is genuinely one-shot rather than assuming
/// so.
const INSTANT_BREAK_BLOCK: &str = "minecraft:short_grass";

/// The world position of the targeted block, and therefore of the pick target.
const TARGET: [i32; 3] = [3, 4, 5];

/// Generous upper bound on ticks to reach `StopDestroy`. Hardness `1.0` at
/// speed `1.0` finishes in well under this; the bound exists only so a
/// regression that stalls the predictor fails with a clear message instead of
/// spinning forever.
const MAX_TICKS: u32 = 200;

/// A jump this large or more on the `StopDestroy` tick can only be the burst:
/// `breaking_block`'s per-tick chip is a single particle, and a debris burst
/// is an `N×N×N` grid (`lodestone_particle::emit::destroy_block_effect`).
const BURST_THRESHOLD: usize = 8;

/// A [`VersionAdapter`] that speaks no protocol and knows exactly one block —
/// the same shape as `mining_deadlock.rs`'s `OneBlockVersion`, duplicated rather
/// than shared because the two test binaries do not share a support crate for it
/// and the type is small.
///
/// Carries the state id and hardness so the two tests can point the *same*
/// harness, predictor and particle sink at a slow block and at a one-shot one.
/// The hardness is the only thing that decides which of `Mining`'s two destroy
/// branches runs, which is exactly what that fix is about.
#[derive(Debug, Clone, Copy)]
struct OneBlockVersion {
    /// The one block-state id this adapter answers for.
    state: u32,
    /// That state's hardness, as `BreakInputs` will see it.
    hardness: f32,
}

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
        (state_id == self.state).then_some(BlockHardness {
            hardness: self.hardness,
            requires_correct_tool: false,
        })
    }

    fn tool_mining(&self, _held: Option<&ItemStack>, state_id: u32) -> Option<ToolMining> {
        (state_id == self.state).then_some(ToolMining {
            speed: 1.0,
            correct_tool: true,
            damage_per_block: 0,
        })
    }
}

/// The block-state id `name` resolves to in the jar-derived state census, taking
/// the block's **first** state (grass and saplings have one or two, all of the
/// same hardness).
///
/// Scanned rather than hardcoded: a bare id would silently start naming an
/// unrelated block the next time the census is regenerated, and that is the
/// failure mode this whole fixture exists to avoid.
fn state_id_of(name: &str) -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the protocol-776 block-state census"))
}

/// The instant-break fixture, **derived** from the hardness census rather than
/// asserted against it.
///
/// This is the *precondition* guard the issue calls out. A fixture whose block
/// takes even one extra tick exercises `Mining::continue_`'s progress branch —
/// the path that already worked — and the gate below would then pass while the
/// reported bug was untouched. So the census supplies the hardness the adapter
/// reports, and this function refuses to hand back anything the predictor's own
/// formula does not classify as one-shot.
fn instant_break_fixture() -> OneBlockVersion {
    let state = state_id_of(INSTANT_BREAK_BLOCK);
    let census = lodestone_data::hardness::hardness(state)
        .unwrap_or_else(|| panic!("{INSTANT_BREAK_BLOCK} (state {state}) has no census hardness"));

    // The predictor's own instant-break condition, not a proxy for it: this is
    // the expression `Mining::start` branches on. `tool_speed`/`on_ground` are
    // `BreakInputs::default()`'s bare-handed-on-dry-land values, matching what
    // `dig_break_inputs` will build for this harness's player, and
    // `correct_tool` mirrors the adapter's `ToolMining` above.
    let inputs = BreakInputs {
        hardness: census.hardness,
        correct_tool: true,
        ..BreakInputs::default()
    };
    assert_eq!(
        inputs.ticks_to_break(),
        Some(0),
        "{INSTANT_BREAK_BLOCK} (state {state}) must be genuinely one-shot for this \
         gate to exercise issue #387 at all — the census reports hardness {} \
         (requires_correct_tool={}), which needs {:?} mining ticks. Pick a block \
         the census still says is instant-break.",
        census.hardness,
        census.requires_correct_tool,
        inputs.ticks_to_break(),
    );
    assert_ne!(
        state, 0,
        "the fixture must not be air — `dig_break_inputs` sets `is_air`, which \
         short-circuits the instant-break branch entirely"
    );

    OneBlockVersion {
        state,
        hardness: census.hardness,
    }
}

/// The progressive fixture: several ticks of accumulation before it breaks.
fn progressive_fixture() -> OneBlockVersion {
    let version = OneBlockVersion {
        state: STONE,
        hardness: STONE_HARDNESS,
    };
    let inputs = BreakInputs {
        hardness: version.hardness,
        correct_tool: true,
        ..BreakInputs::default()
    };
    assert!(
        inputs.ticks_to_break().is_some_and(|t| t > 1),
        "the progressive fixture must take more than one tick, or its own \
         'no burst yet' control window does not exist"
    );
    version
}

/// Whether the tick's queued actions contain the `StopDestroy` this emit used to
/// be keyed on.
///
/// Deliberately one function used by both tests. The progressive gate asserts it
/// answers **true** on the finishing tick, which is the control proving the
/// detector works; the instant-break gate asserts it answers **false** on a tick
/// that nonetheless bursts, which is what pins the re-keying. Without the shared
/// helper, "no `StopDestroy`" would be an absence measured by an unproven
/// detector.
fn stop_destroy_queued(actions: &[ClientAction]) -> bool {
    actions.iter().any(|a| {
        matches!(
            a,
            ClientAction::BlockAction {
                action: lodestone_model::BlockActionKind::StopDestroy,
                ..
            }
        )
    })
}

/// A live-shaped session with no server, holding just enough state for
/// `drive_mining` to run a real dig to completion. See `mining_deadlock.rs`'s
/// `Harness` for the same shape with more extensive commentary.
struct Harness {
    ecs: EcsHandle,
    _runtime: tokio::runtime::Runtime,
    _server_io: tokio::io::DuplexStream,
}

impl Harness {
    fn build(version: OneBlockVersion) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime");

        let ecs = lodestone_ecs::new_handle();
        let session = {
            let mut world = ecs.write();
            world.insert_resource(LockHolds::default());
            build_resources(&mut world, version);
            spawn_player(&mut world)
        };

        let (client, server_io) = {
            let _enter = runtime.enter();
            let (client_io, server_io) = lodestone_net::memory_pair();
            let (client, _events) = ClientBuilder::new(
                ServerAddress {
                    host: "memory".into(),
                    port: 0,
                },
                LoginProfile {
                    username: "destroy_burst".into(),
                    uuid: uuid::Uuid::nil(),
                },
                Box::new(version),
            )
            .ecs(Arc::clone(&ecs), session)
            .connect_with(client_io);
            (Arc::new(client), server_io)
        };

        client.chunk_world_write().write().load(
            ChunkPos::new(TARGET[0].div_euclid(16), TARGET[2].div_euclid(16)),
            column_with(TARGET, version.state),
        );

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
            _runtime: runtime,
            _server_io: server_io,
        }
    }
}

fn build_resources(world: &mut EcsWorld, version: OneBlockVersion) {
    world.insert_resource(Egress {
        in_world: true,
        live: true,
    });
    world.insert_resource(Attacking(true));
    world.insert_resource(RayTarget(Some(RayHit::face_center(TARGET, [0, 1, 0]))));
    world.insert_resource(MiningPredictor::default());
    world.insert_resource(ParticleSim(Particles::new(None)));
    world.insert_resource(ActionQueue::default());
    world.insert_resource(VersionData(Some(Box::new(version))));
}

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

/// **The gate for the progressive path**, and — since that fix re-keyed
/// the emit off `StopDestroy` — the control proving
/// [`stop_destroy_queued`] actually detects something.
///
/// Ticks a real dig to completion, recording the per-tick particle-count delta,
/// and asserts the shape: small (or zero) before completion, large exactly on it.
#[test]
fn a_completed_dig_throws_a_debris_burst_on_the_tick_it_finishes() {
    let harness = Harness::build(progressive_fixture());
    let mut world = harness.ecs.write();

    let mut previous_count = world.resource_mut::<ParticleSim>().0.engine_mut().particles().len();
    let mut stop_tick: Option<u32> = None;
    let mut pre_completion_jumps = Vec::new();

    for tick in 0..MAX_TICKS {
        world.run_schedule(GameTick);
        let actions = std::mem::take(&mut world.resource_mut::<ActionQueue>().0);
        let count = world.resource_mut::<ParticleSim>().0.engine_mut().particles().len();
        let delta = count.saturating_sub(previous_count);
        previous_count = count;

        let stopped = stop_destroy_queued(&actions);

        if stopped {
            stop_tick = Some(tick);
            assert!(
                delta >= BURST_THRESHOLD,
                "the tick `StopDestroy` was queued must throw a debris burst — \
                 particle count only grew by {delta} (wanted at least \
                 {BURST_THRESHOLD}). This is issue #360: a completed dig \
                 produced no burst."
            );
            break;
        }

        // The control: every tick *before* completion must not already look
        // like a burst. A mining chip is at most one particle; if this ever
        // fires, the gate above is not distinguishing a real burst from
        // ordinary per-tick noise, and proves nothing.
        pre_completion_jumps.push(delta);
        assert!(
            delta < BURST_THRESHOLD,
            "tick {tick} (before completion) already jumped particle count by \
             {delta} — the control is compromised: something other than the \
             finishing burst is producing burst-sized particle counts."
        );
    }

    assert!(
        stop_tick.is_some(),
        "the dig never completed within {MAX_TICKS} ticks — hardness/speed \
         changed, or the predictor stalled; this harness needs updating \
         either way, since the gate above cannot run without a completed dig"
    );
    assert!(
        !pre_completion_jumps.is_empty(),
        "the dig completed on the very first tick, so there were no \
         pre-completion ticks to serve as the control — widen the block's \
         hardness so this gate actually exercises the 'no burst yet' window"
    );
}

/// **The gate for that fix.** A genuinely one-shot block must throw its debris
/// burst on the very first tick of the dig, on a tick where no `StopDestroy` is
/// queued at all.
///
/// # What each assertion is for
///
/// * The fixture comes from [`instant_break_fixture`], which derives the hardness
///   from the jar census and refuses to return anything the predictor's own
///   formula does not call one-shot. That is the *precondition* guard: a fixture
///   with a real `destroySpeed` would quietly turn this into a second copy of the
///   progressive test.
/// * `delta >= BURST_THRESHOLD` on tick 0 is the gate. Pre-fix this was `0`:
///   `drive_mining` emitted nothing, and it emitted no per-tick mining chip
///   either, because an instant break leaves `Mining::target()` `None` both
///   before and after the call.
/// * **The control**, and the reason this is not simply a duplicate assertion:
///   `stop_destroy_queued` must answer **false** on that same tick. That is what
///   proves the burst did not come back through the old key — the emit really is
///   keyed on destruction now. The detector itself is proven live by
///   [`a_completed_dig_throws_a_debris_burst_on_the_tick_it_finishes`], which
///   calls the same function and requires a `true` from it; without that pairing
///   this `false` would be an absence measured by an untested detector.
#[test]
fn an_instant_break_throws_a_debris_burst_on_its_very_first_tick() {
    let version = instant_break_fixture();
    let harness = Harness::build(version);
    let mut world = harness.ecs.write();

    let before = world
        .resource_mut::<ParticleSim>()
        .0
        .engine_mut()
        .particles()
        .len();
    assert_eq!(
        before, 0,
        "the harness must start with an empty particle engine, or the delta below \
         is measuring something else"
    );

    world.run_schedule(GameTick);

    let actions = std::mem::take(&mut world.resource_mut::<ActionQueue>().0);
    let after = world
        .resource_mut::<ParticleSim>()
        .0
        .engine_mut()
        .particles()
        .len();
    let delta = after.saturating_sub(before);

    assert!(
        actions.iter().any(|a| matches!(
            a,
            ClientAction::BlockAction {
                action: lodestone_model::BlockActionKind::StartDestroy,
                ..
            }
        )),
        "the dig must actually have started — queued actions were {actions:?}. \
         Without a START this tick did nothing at all and the burst assertion \
         below would be measuring an unrelated failure."
    );
    assert!(
        !stop_destroy_queued(&actions),
        "the control is broken: a one-shot break queued a `StopDestroy`, so this \
         gate is exercising the progressive path that already worked rather than \
         issue #387. Queued actions were {actions:?}."
    );
    assert!(
        delta >= BURST_THRESHOLD,
        "breaking {INSTANT_BREAK_BLOCK} (state {}, census hardness {}) in one \
         swing threw {delta} particles, wanted at least {BURST_THRESHOLD}. This \
         is issue #387: the burst was keyed on the `StopDestroy` packet, which a \
         one-shot break never sends, so grass and saplings produced no debris \
         while stone did.",
        version.state,
        version.hardness,
    );
}
