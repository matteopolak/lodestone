//! Issue #360: the debris burst at the moment a block finishes breaking.
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
//! The fix adds a local **predicted** emit in `drive_mining`, on the same tick
//! its own `Mining` predictor emits `BlockAction { action: StopDestroy, .. }`
//! — vanilla's `MultiPlayerGameMode.destroyBlock` does the identical thing
//! client-side, synchronously, without waiting for a server round trip.
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
//! under test.

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
use lodestone_model::{AdapterError, BlockHardness, ClientAction, ItemStack, ToolMining};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, WorldSink,
};

/// The block state the dig targets. Any non-air id the census resolves works.
const STONE: u32 = 1;

/// The world position of that block, and therefore of the pick target.
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
/// identical in shape to `mining_deadlock.rs`'s `OneBlockVersion`, duplicated
/// rather than shared because the two test binaries do not share a support
/// crate for it and the type is small.
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

/// A live-shaped session with no server, holding just enough state for
/// `drive_mining` to run a real dig to completion. See `mining_deadlock.rs`'s
/// `Harness` for the same shape with more extensive commentary.
struct Harness {
    ecs: EcsHandle,
    _runtime: tokio::runtime::Runtime,
    _server_io: tokio::io::DuplexStream,
}

impl Harness {
    fn build() -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime");

        let ecs = lodestone_ecs::new_handle();
        let session = {
            let mut world = ecs.write();
            world.insert_resource(LockHolds::default());
            build_resources(&mut world);
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
                Box::new(OneBlockVersion),
            )
            .ecs(Arc::clone(&ecs), session)
            .connect_with(client_io);
            (Arc::new(client), server_io)
        };

        client.chunk_world().write().load(
            ChunkPos::new(TARGET[0].div_euclid(16), TARGET[2].div_euclid(16)),
            column_with(TARGET, STONE),
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

fn build_resources(world: &mut EcsWorld) {
    world.insert_resource(Egress {
        in_world: true,
        live: true,
    });
    world.insert_resource(Attacking(true));
    world.insert_resource(RayTarget(Some(RayHit {
        block: TARGET,
        normal: [0, 1, 0],
    })));
    world.insert_resource(MiningPredictor::default());
    world.insert_resource(ParticleSim(Particles::new(None)));
    world.insert_resource(ActionQueue::default());
    world.insert_resource(VersionData(Some(Box::new(OneBlockVersion))));
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

/// **The gate.** Ticks a real dig to completion, recording the per-tick
/// particle-count delta, and asserts the shape: small (or zero) before
/// `StopDestroy`, large exactly on it.
#[test]
fn a_completed_dig_throws_a_debris_burst_on_the_tick_it_finishes() {
    let harness = Harness::build();
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

        let stopped = actions.iter().any(|a| {
            matches!(
                a,
                ClientAction::BlockAction {
                    action: lodestone_model::BlockActionKind::StopDestroy,
                    ..
                }
            )
        });

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
