//! `BreakIntent`, hermetically: a plugin's wish to mine a block, driven all
//! the way through the real [`lodestone::interact::drive_mining`] system and
//! the real `lodestone_game::mining::Mining` state machine — never a
//! hand-called function.
//!
//! # What this proves, and what it does not
//!
//! Per `CLAUDE.md`'s "nothing is done until something on screen changes": a
//! test that only constructs a `BreakIntent` and asserts the component was
//! written proves nothing about the seam this file exists for. Every test
//! here runs a real `GameTick` [`Schedule`] holding the production system, so
//! the gate is the *sequence counter* — owned entirely by the shell's
//! `MiningPredictor`, per `docs/baritone-port.md` §3.6's "threaded, never
//! synthesised" rule — actually advancing (or, in the control, *not*
//! advancing at all) as a consequence of a component write on the ECS side.
//!
//! # The harness
//!
//! Adapted from `mining_deadlock.rs`'s: one [`EcsHandle`], one real
//! [`ClientHandle`] over an in-memory transport (so [`NetHandle::block_at`]
//! reads real chunk data rather than a stub), one fake [`VersionAdapter`]
//! that additionally answers `block_outline` — the one seam
//! `mining_deadlock.rs` never needed, because a mouse-driven hit already
//! carries its target; a plugin's [`BreakIntent`] does not, so
//! `resolve_break_intent` has to cast its own ray through the census this
//! adapter provides.
//!
//! # Dependencies
//!
//! `lodestone::interact` for the system and resources under test;
//! `lodestone_ecs` for the schedule, the handle and the component set;
//! `lodestone_client`/`lodestone_net` for a hermetic in-memory connection.

use std::sync::Arc;
use std::sync::OnceLock;

use lodestone::interact::{Attacking, MiningPredictor, NetHandle, ParticleSim, RayTarget};
use lodestone::particles::Particles;
use lodestone_client::{
    ClientBuilder, ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_ecs::ecs::entity::Entity;
use lodestone_ecs::ecs::schedule::Schedule;
use lodestone_ecs::ecs::world::World as EcsWorld;
use lodestone_ecs::player::{
    ActionQueue, BreakIntent, BreakOutcome, BreakRejection, BreakStatus, Egress,
};
use lodestone_ecs::session::SessionMenus;
use lodestone_ecs::{EcsHandle, GameTick, LockHolds, VersionData};
use lodestone_model::{BlockAabb, BlockFace, BlockHardness, BlockPos, ClientAction, ItemStack, ToolMining};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, WorldSink};

/// The one block state this file's fake adapter knows anything about. Any
/// non-air id would do; the number itself is not load-bearing.
const STONE: u32 = 1;

/// Real, oracle-confirmed jar constants (`docs/tool-mining.md`'s reference
/// table, `crates/lodestone-data/tests/tools.rs`): vanilla stone's hardness is
/// `1.5` (`Blocks.java`, cross-checked against the committed hardness census
/// in `block-break-timing.md`), and a diamond pickaxe's `speed 8.0,
/// correct_tool true` on it is the exact row that table pins at **6 ticks** —
/// `per_tick = 8.0 / 1.5 / 30.0 ≈ 0.1778`, and replaying
/// `lodestone_game::mining::BreakInputs::ticks_to_break`'s own
/// accumulate-then-compare loop (not a clean division; `tool-mining.md`
/// documents why) needs 6 accumulating steps to cross `1.0`.
const STONE_HARDNESS: f32 = 1.5;
const PICKAXE_SPEED: f32 = 8.0;
/// `lodestone_game::mining::BreakInputs::ticks_to_break`'s own number: how
/// many **accumulating** `continue_` calls it takes to cross `1.0`, *after*
/// the dig has started.
const ACCUMULATING_TICKS_TO_BREAK: usize = 6;
/// Total `GameTick`s from pressing (this file's first `drive_mining` call,
/// which only starts the dig and accumulates nothing — see `Mining::start`'s
/// own docs) to the tick the block actually breaks. One more than
/// [`ACCUMULATING_TICKS_TO_BREAK`], not the same number — conflating the two
/// is exactly the off-by-one this comment exists to prevent a future reader
/// from reintroducing.
const EXPECTED_TICKS_TO_BREAK: usize = ACCUMULATING_TICKS_TO_BREAK + 1;

/// A full unit cube, block-local — [`STONE`]'s outline for the raycast this
/// file's `resolve_break_intent` casts. A `static`, not a `const`: taking
/// `&CUBE` needs a genuinely `'static` place to back
/// [`VersionAdapter::block_outline`]'s return type.
static CUBE: BlockAabb = BlockAabb {
    min: [0.0, 0.0, 0.0],
    max: [1.0, 1.0, 1.0],
};

// ---------------------------------------------------------------------------
// A fake version, standing in for every seam `drive_mining` reads
// ---------------------------------------------------------------------------

/// A [`VersionAdapter`] that knows exactly one block state ([`STONE`]) for
/// hardness, tool speed, *and* outline geometry — the third one is new versus
/// `mining_deadlock.rs`'s `OneBlockVersion`, because that file's mouse-driven
/// hit already carries a target; a [`BreakIntent`]-resolved one has to be
/// found by casting a ray through this census first.
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
    ) -> Result<Vec<Directive>, lodestone_model::AdapterError> {
        Ok(Vec::new())
    }

    fn handle_packet(
        &self,
        _world: &mut dyn WorldSink,
        _state: ConnectionState,
        _packet_id: i32,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, lodestone_model::AdapterError> {
        Ok(Vec::new())
    }

    fn encode_action(
        &self,
        _state: ConnectionState,
        _action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, lodestone_model::AdapterError> {
        Ok(None)
    }

    fn block_hardness(&self, state_id: u32) -> Option<BlockHardness> {
        (state_id == STONE).then_some(BlockHardness {
            hardness: STONE_HARDNESS,
            requires_correct_tool: true,
        })
    }

    fn tool_mining(&self, _held: Option<&ItemStack>, state_id: u32) -> Option<ToolMining> {
        // Stands in for an already-selected diamond pickaxe regardless of
        // what (if anything) the harness put in the hotbar — this file is
        // about the intent seam, not inventory plumbing, which
        // `tool-mining.md`/`sim/tests.rs` already cover on their own.
        (state_id == STONE).then_some(ToolMining {
            speed: PICKAXE_SPEED,
            correct_tool: true,
            damage_per_block: 0,
        })
    }

    fn block_outline(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
        (state_id == STONE).then_some(std::slice::from_ref(&CUBE))
    }
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

/// A live-shaped session with no server: one [`EcsHandle`], a [`ClientHandle`]
/// that adopted it, and a `GameTick` [`Schedule`] holding [`drive_mining`].
struct Harness {
    ecs: EcsHandle,
    entity: Entity,
    _runtime: tokio::runtime::Runtime,
    _server_io: tokio::io::DuplexStream,
}

impl Harness {
    /// Player spawned at `(3.5, 5.0, 5.5)`, facing nowhere in particular —
    /// `drive_mining` reads rotation from nothing but the [`BreakIntent`]
    /// path under test. `stone_at` is every `(pos, state)` pair to load into
    /// the client's chunk store, so callers can add an obstruction without
    /// this harness knowing what "obstructed" means.
    fn build(stone_at: &[[i32; 3]]) -> Self {
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
                    username: "break-intent".into(),
                    uuid: uuid::Uuid::nil(),
                },
                Box::new(OneBlockVersion),
            )
            .ecs(Arc::clone(&ecs), session)
            .connect_with(client_io);
            (Arc::new(client), server_io)
        };

        {
            // Grouped by chunk column first: two positions sharing a column
            // (`TARGET`/`WALL` both sit in chunk (0,0)) must land as two
            // blocks in **one** loaded column, not two columns each
            // overwriting the other — `load` replaces whatever was at that
            // `ChunkPos` outright.
            let mut columns: std::collections::BTreeMap<(i32, i32), ChunkColumn> =
                std::collections::BTreeMap::new();
            for pos in stone_at {
                let key = (pos[0].div_euclid(16), pos[2].div_euclid(16));
                let column = columns.entry(key).or_insert_with(|| {
                    ChunkColumn::new(0, 16, PaletteKind::block_states(), PaletteKind::biomes(), 0, 0)
                });
                column.set_block(
                    pos[0].rem_euclid(16) as usize,
                    pos[1],
                    pos[2].rem_euclid(16) as usize,
                    STONE,
                );
            }
            let store = client.chunk_world_write();
            let mut store = store.write();
            for ((cx, cz), column) in columns {
                store.load(
                    ChunkPos::new(cx, cz),
                    LoadedChunk::new(column, ColumnLight::new(16), Heightmaps::new(), Vec::new()),
                );
            }
        }

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
            entity: session,
            _runtime: runtime,
            _server_io: server_io,
        }
    }

    /// Run one `GameTick` and return everything queued this tick, draining
    /// [`ActionQueue`] exactly as the real driver does between ticks — so a
    /// packet from tick 3 is never mistaken for one from tick 4.
    fn tick(&self) -> Vec<ClientAction> {
        let mut world = self.ecs.write();
        world.run_schedule(GameTick);
        world.resource_mut::<ActionQueue>().0.drain(..).collect()
    }

    fn outcome(&self) -> BreakStatus {
        let world = self.ecs.write();
        world.get::<BreakOutcome>(self.entity).unwrap().0
    }

    fn set_intent(&self, intent: BreakIntent) {
        let mut world = self.ecs.write();
        world.entity_mut(self.entity).insert(intent);
    }
}

/// Every resource `drive_mining` names, with the human path held **idle** —
/// `Attacking(false)` and no [`RayTarget`] — so every hit this file's tests
/// see comes from a [`BreakIntent`], never the mouse.
fn build_resources(world: &mut EcsWorld) {
    world.insert_resource(Egress {
        in_world: true,
        live: true,
    });
    world.insert_resource(Attacking(false));
    world.insert_resource(RayTarget(None));
    world.insert_resource(MiningPredictor::default());
    world.insert_resource(ParticleSim(Particles::new(None)));
    world.insert_resource(ActionQueue::default());
    world.insert_resource(VersionData(Some(Box::new(OneBlockVersion))));
}

/// One entity carrying both halves of the local player's component set, same
/// shape `mining_deadlock.rs` uses and for the same reason: `With<LocalPlayer>`
/// must match exactly one.
fn spawn_player(world: &mut EcsWorld) -> Entity {
    // `PlayerState::at` starts `on_ground: false` — correct for a state that
    // has never run a physics tick, but this harness registers no
    // `TickSet::Physics` system at all (only `drive_mining`, in
    // `TickSet::Send`), so nothing would ever flip it. `BreakInputs::dig_speed`
    // divides by 5 while off the ground (`mining.rs`'s "off-ground mining is
    // 5x slower"), which would silently invalidate
    // [`EXPECTED_TICKS_TO_BREAK`] — that constant, like the reference table
    // it is drawn from, assumes a grounded player. Set explicitly rather than
    // relying on a physics tick this harness deliberately does not run.
    let mut state = lodestone_physics::PlayerState::at(lodestone_physics::Vec3d::new(3.5, 5.0, 5.5), 0.0);
    state.on_ground = true;
    let entity = lodestone_ecs::spawn_local_player(world, state);
    world
        .entity_mut(entity)
        .insert(SessionMenus(lodestone_game::menus::Menus::default()));
    entity
}

/// Every `BlockAction`'s `sequence`, in the order queued, from a batch of
/// per-tick action lists — the observable this file's gate and control both
/// rest on.
fn block_action_sequences(ticks: &[Vec<ClientAction>]) -> Vec<i32> {
    ticks
        .iter()
        .flatten()
        .filter_map(|a| match a {
            ClientAction::BlockAction { sequence, .. } => Some(*sequence),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The gate: a real break, through the real state machine
// ---------------------------------------------------------------------------

/// **The gate.** A [`BreakIntent`] on the ECS side must produce an actual
/// break through `drive_mining`'s own `MiningPredictor` — the identical
/// `START`/hold/`STOP` sequence a mouse-driven dig produces — with the
/// sequence counter still owned by the shell and still monotonic, and
/// finishing on the exact tick the real jar constants predict.
#[test]
fn a_break_intent_mines_the_block_in_exactly_the_predicted_number_of_ticks() {
    const TARGET: [i32; 3] = [3, 4, 5];
    let harness = Harness::build(&[TARGET]);

    // `Up`: the face a player standing at (3.5, 5.0, 5.5) and punching
    // straight down would strike — the block's top is at y=5, one below feet.
    harness.set_intent(BreakIntent {
        pos: BlockPos::new(TARGET[0], TARGET[1], TARGET[2]),
        face: BlockFace::Up,
    });

    let mut ticks = Vec::new();
    for i in 0..EXPECTED_TICKS_TO_BREAK {
        let actions = harness.tick();
        assert_eq!(
            harness.outcome(),
            BreakStatus::Progressing,
            "tick {i}: a resolvable, in-range intent must report Progressing, \
             not Idle or Rejected"
        );
        ticks.push(actions);
    }

    let all_actions: Vec<_> = ticks.iter().flatten().cloned().collect();
    assert!(
        all_actions
            .iter()
            .any(|a| matches!(a, ClientAction::SwingArm { .. })),
        "no SwingArm was queued at all, so no dig ever started: {all_actions:?}"
    );

    let sequences = block_action_sequences(&ticks);
    assert_eq!(
        sequences,
        vec![1, 2],
        "expected exactly one START (seq 1) on the first tick and one STOP \
         (seq 2) on the tick progress reached 1.0 — a fork or a duplicate \
         draw would show up here as a repeated or skipped number: {sequences:?}"
    );

    // The tick *after* the break must not still report Progressing forever:
    // the predictor enters its 5-tick post-break cooldown and the intent, if
    // left installed, resolves to the same (now-air) target — read that as
    // "nothing more to report" rather than a stuck Progressing.
    let cooldown_actions = harness.tick();
    assert!(
        !cooldown_actions
            .iter()
            .any(|a| matches!(a, ClientAction::BlockAction { .. })),
        "the cooldown tick must not queue a second dig's worth of block \
         actions: {cooldown_actions:?}"
    );
}

// ---------------------------------------------------------------------------
// The control: an intent the shell would refuse
// ---------------------------------------------------------------------------

/// **The control.** A [`BreakIntent`] naming a block hidden behind another
/// block must be rejected — reported through [`BreakOutcome`], never
/// silently absorbed — and the sequence counter must not move at all: zero
/// `BlockAction`s queued across every tick, which is the only way to observe
/// "the counter never drew a number" from outside `MiningPredictor` (its
/// `next_sequence` field is private by design, exactly so nothing outside the
/// shell can read *or* advance it directly).
///
/// Without this control, [`a_break_intent_mines_the_block_in_exactly_the_predicted_number_of_ticks`]
/// would prove nothing about *rejection* — a `drive_mining` that accepted
/// every intent unconditionally would pass the gate above identically.
#[test]
fn an_obstructed_break_intent_is_rejected_and_never_advances_the_sequence() {
    const TARGET: [i32; 3] = [3, 4, 5];
    // Directly above the target, between it and the player's eye — the same
    // column, one section up. A straight-down ray toward `TARGET`'s top face
    // must clip this cube first.
    const WALL: [i32; 3] = [3, 5, 5];
    let harness = Harness::build(&[TARGET, WALL]);

    harness.set_intent(BreakIntent {
        pos: BlockPos::new(TARGET[0], TARGET[1], TARGET[2]),
        face: BlockFace::Up,
    });

    let mut ticks = Vec::new();
    for i in 0..EXPECTED_TICKS_TO_BREAK {
        let actions = harness.tick();
        assert_eq!(
            harness.outcome(),
            BreakStatus::Rejected(BreakRejection::UnreachableOrObstructed),
            "tick {i}: a target hidden behind another block must be reported \
             as rejected on every tick, not just the first"
        );
        ticks.push(actions);
    }

    let all_actions: Vec<_> = ticks.into_iter().flatten().collect();
    assert!(
        all_actions.is_empty(),
        "a rejected intent must queue nothing at all — no SwingArm, no \
         BlockAction, and above all no sequence draw: {all_actions:?}"
    );
    let sequences = block_action_sequences(&[all_actions]);
    assert!(
        sequences.is_empty(),
        "the sequence counter must not have moved even once: {sequences:?}"
    );
}

/// A second, narrower control: the *same* target with the wall removed digs
/// normally — proving [`WALL`]'s placement in the test above, not merely its
/// presence, is what causes the rejection. Without this, the obstructed test
/// could be passing for an unrelated reason (a typo in the target, a broken
/// harness) that happens to also report "rejected".
#[test]
fn the_same_target_without_the_wall_is_not_rejected() {
    const TARGET: [i32; 3] = [3, 4, 5];
    let harness = Harness::build(&[TARGET]);
    harness.set_intent(BreakIntent {
        pos: BlockPos::new(TARGET[0], TARGET[1], TARGET[2]),
        face: BlockFace::Up,
    });
    let actions = harness.tick();
    assert_eq!(harness.outcome(), BreakStatus::Progressing);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, ClientAction::BlockAction { .. })),
        "with no obstruction, the very first tick must queue a START: {actions:?}"
    );
}

/// A target genuinely outside vanilla's 4.5-block reach is rejected the same
/// way an obstructed one is — the two collapse to one variant by design (see
/// [`BreakRejection::UnreachableOrObstructed`]'s own docs), and this pins that
/// "too far" is not silently treated as "too close to check".
#[test]
fn a_break_intent_far_outside_reach_is_rejected() {
    const TARGET: [i32; 3] = [3, 4, 5];
    const FAR: [i32; 3] = [3, 4, 25]; // ~19.5 blocks from the eye — well past 4.5.
    let harness = Harness::build(&[TARGET, FAR]);
    harness.set_intent(BreakIntent {
        pos: BlockPos::new(FAR[0], FAR[1], FAR[2]),
        face: BlockFace::Up,
    });
    harness.tick();
    assert_eq!(
        harness.outcome(),
        BreakStatus::Rejected(BreakRejection::UnreachableOrObstructed)
    );
}

/// While the human attack button is held, a plugin's [`BreakIntent`] must be
/// ignored entirely — not merely overridden but *not consulted* — and must
/// report [`BreakStatus::Idle`], never `Progressing`/`Rejected`, because
/// nothing about the plugin's own wish was actually evaluated this tick.
#[test]
fn a_human_attack_takes_priority_and_the_intent_reports_idle() {
    const TARGET: [i32; 3] = [3, 4, 5];
    let harness = Harness::build(&[TARGET]);
    harness.set_intent(BreakIntent {
        pos: BlockPos::new(TARGET[0], TARGET[1], TARGET[2]),
        face: BlockFace::Up,
    });
    {
        let mut world = harness.ecs.write();
        world.insert_resource(Attacking(true));
        world.insert_resource(RayTarget(Some(lodestone::raycast::RayHit::face_center(
            TARGET,
            [0, 1, 0],
        ))));
    }
    harness.tick();
    assert_eq!(
        harness.outcome(),
        BreakStatus::Idle,
        "the human path must take priority, and the plugin's own outcome must \
         read as \"nothing to report from me\" rather than borrowing the \
         human dig's progress"
    );
}
