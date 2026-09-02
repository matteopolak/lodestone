//! `PlaceIntent`, hermetically: a plugin's wish to place a block, driven all
//! the way through the real [`lodestone::interact::drive_placement`] system,
//! the real [`lodestone_game::placement::Placement`] state machine and a real
//! [`lodestone_ecs::ChunkWorld`] write — never a hand-called function.
//!
//! Adapted from `break_intent.rs`, which this file mirrors deliberately: same
//! harness shape (one [`EcsHandle`], one real [`ClientHandle`] over an
//! in-memory transport, one fake [`VersionAdapter`] answering
//! `block_outline`), same gate/control split, same "read the sequence off the
//! wire, never off the predictor's own private counter" method.
//!
//! # One real difference from `BreakIntent`'s harness
//!
//! `break_intent.rs`'s fake adapter can invent hardness/tool-speed for an
//! arbitrary made-up state id, because [`VersionData::block_hardness`] and
//! `tool_mining` are version-adapter seams. Placement's
//! `orientation_for_placement`/`state_for_placement`/`block_states_of` are
//! **not** — they read `lodestone_data::block_states` directly, the real
//! compiled-in 26.2 census, by block *name*. So this harness places a real
//! `minecraft:stone` (the simplest placeable block: no `facing`/`axis`/`half`/
//! `shape` property at all, so [`OrientationKind::Fixed`] and an empty
//! property list resolve trivially) against a real `minecraft:dirt` ground
//! block, and the fake adapter's `block_outline` answers for whichever real
//! state ids the fixture actually uses rather than an arbitrary `STONE = 1`.
//!
//! # What this proves, and what it does not
//!
//! Per `CLAUDE.md`'s "nothing is done until something on screen changes": the
//! gate below does not stop at "a `PlaceOutcome::Predicted` was written" — it
//! reads the block back out of the real [`lodestone_ecs::ChunkWorld`] the
//! system wrote through, so a `drive_placement` that resolved a state and
//! then forgot to call `write_predicted_block` would still fail this file.
//! It does not cover pixels (see `placed_chest_block_entity_pixels.rs` for
//! that, GPU-gated and `#[ignore]`d) — this is the component/system wiring
//! one layer below the render pass.

use std::sync::Arc;
use std::sync::OnceLock;

use lodestone::interact::{
    Attacking, NetHandle, ParticleSim, PlacementPredictor, RayTarget, UsingItem,
};
use lodestone::mesher::{MeshScheduler, TerrainMesh};
use lodestone::particles::Particles;
use lodestone::sim::AudioEngine;
use lodestone_client::{
    ClientBuilder, ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_ecs::ecs::entity::Entity;
use lodestone_ecs::ecs::schedule::Schedule;
use lodestone_ecs::ecs::world::World as EcsWorld;
use lodestone_ecs::player::{
    ActionQueue, Egress, PlaceIntent, PlaceOutcome, PlaceRejection, PlaceStatus, Profile,
};
use lodestone_ecs::session::SessionMenus;
use lodestone_ecs::{EcsHandle, FrameClock, GameTick, LockHolds, VersionData};
use lodestone_model::event::ClientEvent;
use lodestone_model::{
    BlockAabb, BlockFace, BlockHardness, BlockPos, ClientAction, ItemStack as ModelItemStack,
    ToolMining,
};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, WorldSink};

/// A full unit cube, block-local — every real state id this file's fixture
/// world uses gets this outline for the raycast `resolve_place_intent` casts.
/// A `static`, not a `const`: taking `&CUBE` needs a genuinely `'static`
/// place to back [`VersionAdapter::block_outline`]'s return type.
static CUBE: BlockAabb = BlockAabb {
    min: [0.0, 0.0, 0.0],
    max: [1.0, 1.0, 1.0],
};

/// The first block-state id of a named block, from the real 26.2 census.
/// Never a hardcoded id — those shift with every data bump. Mirrors
/// `placed_chest_block_entity_pixels.rs`'s own helper.
fn first_state_named(name: &str) -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the 26.2 block-state table"))
}

// ---------------------------------------------------------------------------
// A fake version, standing in for every seam `drive_placement` reads besides
// the block-state census itself
// ---------------------------------------------------------------------------

/// A [`VersionAdapter`] whose only real job is `block_outline`: every state
/// this file's fixture world loads gets the full-cube answer, so
/// `resolve_place_intent`'s cast has real geometry to clip against. Hardness
/// and tool speed are never read on the placement path, so they answer
/// nothing at all — unlike `break_intent.rs`'s `OneBlockVersion`.
#[derive(Debug, Default)]
struct OutlineOnlyVersion;

impl VersionAdapter for OutlineOnlyVersion {
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

    fn block_hardness(&self, _state_id: u32) -> Option<BlockHardness> {
        None
    }

    fn tool_mining(&self, _held: Option<&lodestone_model::ItemStack>, _state_id: u32) -> Option<ToolMining> {
        None
    }

    fn block_outline(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
        // Air (real vanilla air, and every loaded column's default fill
        // outside the cells this file's fixtures explicitly set) must stay
        // untargetable, exactly `is_air_state`'s real production rule
        // (`sim/placement.rs`) — the shell's own census-backed check is
        // `pub(crate)` and unreachable from this external test crate, so
        // this re-derives the same three names by hand. Getting this wrong
        // is not cosmetic: an air cell that answers a full cube here makes
        // the ray clip the very first cell along its path (often at the
        // eye's own position) and every intent report
        // `UnreachableOrObstructed`, which is exactly the failure this
        // comment exists to prevent a future reader from reintroducing —
        // measured directly, not assumed.
        match lodestone_data::block_states::block_name(state_id) {
            Some("minecraft:air" | "minecraft:cave_air" | "minecraft:void_air") => None,
            _ => Some(std::slice::from_ref(&CUBE)),
        }
    }
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

/// A live-shaped session with no server: one [`EcsHandle`], a [`ClientHandle`]
/// that adopted it, and a `GameTick` [`Schedule`] holding [`drive_placement`].
struct Harness {
    ecs: EcsHandle,
    entity: Entity,
    _runtime: tokio::runtime::Runtime,
    _server_io: tokio::io::DuplexStream,
}

impl Harness {
    /// Player spawned at `(3.5, 5.0, 5.5)`, on the ground, facing nowhere in
    /// particular — `drive_placement` reads nothing but the [`PlaceIntent`]
    /// path under test. `stone_at` is every `(pos, state)` pair to load into
    /// the client's (and the ECS's) shared chunk store.
    fn build(blocks_at: &[([i32; 3], u32)]) -> Self {
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
                    username: "place-intent".into(),
                    uuid: uuid::Uuid::nil(),
                },
                Box::new(OutlineOnlyVersion),
            )
            .ecs(Arc::clone(&ecs), session)
            .connect_with(client_io);
            (Arc::new(client), server_io)
        };

        let chunk_world = client.chunk_world();
        // The write side, paired with the read handle on the same
        // `Arc`. The store's columns are loaded through *this*; `drive_placement`
        // then needs the matching resource installed (below).
        let chunk_world_write = client.chunk_world_write();
        {
            // Grouped by chunk column first, same reasoning as
            // `break_intent.rs`'s identical loop: two positions sharing a
            // column must land as two blocks in **one** loaded column.
            let mut columns: std::collections::BTreeMap<(i32, i32), ChunkColumn> =
                std::collections::BTreeMap::new();
            for (pos, state) in blocks_at {
                let key = (pos[0].div_euclid(16), pos[2].div_euclid(16));
                let column = columns.entry(key).or_insert_with(|| {
                    ChunkColumn::new(0, 16, PaletteKind::block_states(), PaletteKind::biomes(), 0, 0)
                });
                column.set_block(
                    pos[0].rem_euclid(16) as usize,
                    pos[1],
                    pos[2].rem_euclid(16) as usize,
                    *state,
                );
            }
            let mut store = chunk_world_write.write();
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
            // The same store `NetHandle::block_at` reads, so a write through
            // this resource and a read through that one see each other —
            // exactly `Sim::adopt_live_world`'s invariant, reproduced by hand.
            world.insert_resource(chunk_world);
            world.insert_resource(chunk_world_write);
            let mut schedule = Schedule::new(GameTick);
            schedule.add_systems(lodestone::interact::drive_placement);
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
    /// [`ActionQueue`] exactly as the real driver does between ticks.
    fn tick(&self) -> Vec<ClientAction> {
        let mut world = self.ecs.write();
        world.run_schedule(GameTick);
        world.resource_mut::<ActionQueue>().0.drain(..).collect()
    }

    fn outcome(&self) -> PlaceOutcome {
        let world = self.ecs.write();
        *world.get::<PlaceOutcome>(self.entity).unwrap()
    }

    fn set_intent(&self, intent: PlaceIntent) {
        let mut world = self.ecs.write();
        world.entity_mut(self.entity).insert(intent);
    }

    fn has_intent(&self) -> bool {
        let world = self.ecs.write();
        world.get::<PlaceIntent>(self.entity).is_some()
    }

    /// Read a block straight out of the same [`lodestone_ecs::ChunkWorld`]
    /// `drive_placement` wrote through — the "did anything actually appear"
    /// half of the gate, independent of what `PlaceOutcome` claims.
    fn block_at(&self, pos: [i32; 3]) -> u32 {
        let mut world = self.ecs.write();
        let store = world.resource_mut::<lodestone_ecs::ChunkWorld>().clone();
        let column = store.read();
        let chunk = column
            .get(ChunkPos {
                x: pos[0].div_euclid(16),
                z: pos[2].div_euclid(16),
            })
            .expect("fixture column must be loaded");
        lodestone_world::BlockVolume::block(
            &chunk.column,
            pos[0].rem_euclid(16) as usize,
            pos[1],
            pos[2].rem_euclid(16) as usize,
        )
    }
}

/// Every resource `drive_placement` names, with the human path held **idle**
/// (`UsingItem(false)`) so every hit this file's tests see comes from a
/// [`PlaceIntent`], never a mouse click.
fn build_resources(world: &mut EcsWorld) {
    world.insert_resource(Egress {
        in_world: true,
        live: true,
    });
    world.insert_resource(Attacking(false));
    world.insert_resource(UsingItem(false));
    world.insert_resource(RayTarget(None));
    world.insert_resource(PlacementPredictor::default());
    world.insert_resource(ParticleSim(Particles::new(None)));
    world.insert_resource(ActionQueue::default());
    world.insert_resource(VersionData(Some(Box::new(OutlineOnlyVersion))));
    world.insert_resource(FrameClock::default());
    world.insert_resource(Profile::default());
    world.insert_resource(AudioEngine(None));
    world.insert_resource(TerrainMesh::new(MeshScheduler::new(
        1,
        lodestone::blocks::ShellClassifier::Demo(lodestone::blocks::DemoClassifier),
    )));
}

/// One entity carrying both halves of the local player's component set, same
/// shape `break_intent.rs`/`mining_deadlock.rs` use.
fn spawn_player(world: &mut EcsWorld) -> Entity {
    let mut state = lodestone_physics::PlayerState::at(lodestone_physics::Vec3d::new(3.5, 5.0, 5.5), 0.0);
    state.on_ground = true;
    let entity = lodestone_ecs::spawn_local_player(world, state);
    // Empty by default; individual tests overwrite this with a stocked
    // `Menus` via `stock_hotbar_slot_zero` when they need a real held item.
    world
        .entity_mut(entity)
        .insert(SessionMenus(lodestone_game::menus::Menus::default()));
    entity
}

/// Every `UseItemOn`'s `sequence`, in the order queued, from a batch of
/// per-tick action lists — the observable this file's gate and control both
/// rest on, mirroring `break_intent.rs`'s `block_action_sequences` exactly
/// for the placement wire shape.
fn use_item_on_sequences(ticks: &[Vec<ClientAction>]) -> Vec<i32> {
    ticks
        .iter()
        .flatten()
        .filter_map(|a| match a {
            ClientAction::UseItemOn { sequence, .. } => Some(*sequence),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Held-item setup: a real `minecraft:stone` in hotbar slot 0
// ---------------------------------------------------------------------------

/// Populate hotbar native slot `0` (window-0 menu/wire slot `36` — see
/// `Menu::player()`'s own layout comment) with one of `name`, through the
/// same [`ClientEvent::ContainerContent`] a real server's inventory sync
/// would send. There is no lower-level public setter on
/// [`lodestone_game::menus::Menus`] — `apply` is the sanctioned seam, same as
/// production.
fn stock_hotbar_slot_zero(menus: &mut lodestone_game::menus::Menus, name: &str) {
    let id: lodestone_model::Identifier = name.parse().unwrap_or_else(|_| panic!("{name} parses"));
    let mut items: Vec<Option<ModelItemStack>> = vec![None; 46];
    items[36] = Some(ModelItemStack::new(id, 1));
    menus.apply(&ClientEvent::ContainerContent {
        window_id: 0,
        state_id: 0,
        items,
        carried_item: None,
    });
}

// ---------------------------------------------------------------------------
// The gate: a real placement, through the real state machine
// ---------------------------------------------------------------------------

/// **The gate.** A [`PlaceIntent`] on the ECS side, with a real
/// `minecraft:stone` held, must produce an actual local write through
/// `drive_placement`'s own `PlacementPredictor` — read back out of the real
/// [`lodestone_ecs::ChunkWorld`] — with the block-prediction sequence counter
/// drawing exactly one number.
#[test]
fn a_place_intent_writes_the_predicted_block_and_draws_one_sequence() {
    // Three blocks in front of the player (not underfoot): a face=Up ray from
    // the eye at (3.5, ~6.62, 5.5) toward this block's top face descends
    // smoothly to y=5.0 only at the very end of the cast, so it clips no
    // other face of the same cube first — measured directly. `GROUND`
    // adjacent to the player's own column (e.g. underfoot with a *side*
    // face) does not have this property: the ray grazes the top face before
    // ever reaching the intended side, resolving to the wrong face and, for
    // a target that then falls on the player's own feet cell, a spurious
    // `IntersectsPlayer`.
    const GROUND: [i32; 3] = [3, 4, 8];
    let stone = first_state_named("minecraft:stone");
    let dirt = first_state_named("minecraft:dirt");
    let harness = Harness::build(&[(GROUND, dirt)]);
    {
        let mut world = harness.ecs.write();
        let mut menus = lodestone_game::menus::Menus::default();
        stock_hotbar_slot_zero(&mut menus, "minecraft:stone");
        world.entity_mut(harness.entity).insert(SessionMenus(menus));
    }

    harness.set_intent(PlaceIntent {
        pos: BlockPos::new(GROUND[0], GROUND[1], GROUND[2]),
        face: BlockFace::Up,
    });
    const TARGET: [i32; 3] = [3, 5, 8];

    let actions = harness.tick();
    let outcome = harness.outcome();
    assert_eq!(
        outcome.status,
        PlaceStatus::Predicted,
        "a resolvable, in-range, holding-a-real-block intent must predict, not \
         reject or send-unpredicted: {outcome:?}"
    );
    assert_eq!(outcome.generation, 1, "exactly one attempt must have been counted");
    assert!(
        !harness.has_intent(),
        "the shell must remove the intent after resolving it — one insertion is \
         one attempt"
    );

    assert!(
        actions.iter().any(|a| matches!(a, ClientAction::SwingArm { .. })),
        "no SwingArm was queued at all: {actions:?}"
    );
    let sequences = use_item_on_sequences(&[actions]);
    assert_eq!(
        sequences,
        vec![1],
        "expected exactly one `UseItemOn` sequence draw: {sequences:?}"
    );

    assert_eq!(
        harness.block_at(TARGET),
        stone,
        "the predicted stone must actually be in the chunk store drive_placement \
         wrote through, not merely claimed by PlaceOutcome"
    );
}

// ---------------------------------------------------------------------------
// The control: an intent the shell would refuse, and the sequence must not
// move at all
// ---------------------------------------------------------------------------

/// **The control.** A [`PlaceIntent`] naming a cell far outside vanilla's
/// 4.5-block reach must be rejected — reported through [`PlaceOutcome`],
/// never silently absorbed — and the block-prediction sequence counter must
/// not move at all: zero `UseItemOn`s queued, which is the only way to
/// observe "the counter never drew a number" from outside
/// `PlacementPredictor` (its sequence field is private by design). Nothing
/// must land in the chunk store either — the "paint nothing" half of the
/// gate.
///
/// Without this control, the gate above would prove nothing about
/// *rejection* — a `drive_placement` that accepted every intent
/// unconditionally would pass it identically.
#[test]
fn a_place_intent_far_outside_reach_is_rejected_and_never_advances_the_sequence() {
    const GROUND: [i32; 3] = [3, 4, 5];
    const FAR: [i32; 3] = [3, 4, 25]; // ~19.5 blocks from the eye — well past 4.5.
    let dirt = first_state_named("minecraft:dirt");
    let harness = Harness::build(&[(GROUND, dirt), (FAR, dirt)]);
    {
        let mut world = harness.ecs.write();
        let mut menus = lodestone_game::menus::Menus::default();
        stock_hotbar_slot_zero(&mut menus, "minecraft:stone");
        world.entity_mut(harness.entity).insert(SessionMenus(menus));
    }

    harness.set_intent(PlaceIntent {
        pos: BlockPos::new(FAR[0], FAR[1], FAR[2]),
        face: BlockFace::Up,
    });
    const TARGET: [i32; 3] = [3, 5, 25];
    let air = first_state_named("minecraft:air");

    let actions = harness.tick();
    let outcome = harness.outcome();
    assert_eq!(
        outcome.status,
        PlaceStatus::Rejected(PlaceRejection::UnreachableOrObstructed),
        "a target far outside reach must be rejected, not predicted or \
         sent-unpredicted: {outcome:?}"
    );
    assert_eq!(
        outcome.generation, 1,
        "a rejected intent is still an attempt — generation must advance by \
         exactly one"
    );
    assert!(
        !harness.has_intent(),
        "a rejected intent is still resolved, and the shell removes it either way"
    );

    assert!(
        actions.is_empty(),
        "a rejected intent must queue nothing at all — no SwingArm, no \
         UseItemOn, and above all no sequence draw: {actions:?}"
    );
    let sequences = use_item_on_sequences(&[actions]);
    assert!(
        sequences.is_empty(),
        "the sequence counter must not have moved even once: {sequences:?}"
    );

    assert_eq!(
        harness.block_at(TARGET),
        air,
        "a rejected placement must paint nothing in the chunk store"
    );
}

/// A second, narrower control: nothing placeable held. Unlike a human
/// right-click — which vanilla still sends, in case the clicked block is
/// interactable — a `PlaceIntent` with an empty hand is refused before
/// anything reaches the wire, because the intent specifically asked to
/// *place*.
#[test]
fn a_place_intent_with_an_empty_hand_is_rejected_as_nothing_placeable() {
    const GROUND: [i32; 3] = [3, 4, 5];
    let dirt = first_state_named("minecraft:dirt");
    // No `stock_hotbar_slot_zero` call at all — the fresh `Menus::default()`
    // `spawn_player` installs is empty.
    let harness = Harness::build(&[(GROUND, dirt)]);
    harness.set_intent(PlaceIntent {
        pos: BlockPos::new(GROUND[0], GROUND[1], GROUND[2]),
        face: BlockFace::North,
    });

    let actions = harness.tick();
    assert_eq!(
        harness.outcome().status,
        PlaceStatus::Rejected(PlaceRejection::NothingPlaceableHeld)
    );
    assert!(actions.is_empty(), "nothing placeable must queue nothing: {actions:?}");
}

/// While the human use button is held, a plugin's [`PlaceIntent`] must be
/// ignored entirely — not merely overridden but *not consulted* — mirroring
/// `break_intent.rs`'s identical human-priority test for mining. Unlike
/// [`PlaceOutcome`]'s reset-every-idle-tick cousin `BreakOutcome`, the
/// outcome here is left **untouched** rather than forced to `Idle` — see
/// that type's own doc for why.
#[test]
fn a_human_use_takes_priority_and_leaves_the_outcome_untouched() {
    const GROUND: [i32; 3] = [3, 4, 5];
    let dirt = first_state_named("minecraft:dirt");
    let harness = Harness::build(&[(GROUND, dirt)]);
    {
        let mut world = harness.ecs.write();
        let mut menus = lodestone_game::menus::Menus::default();
        stock_hotbar_slot_zero(&mut menus, "minecraft:stone");
        world.entity_mut(harness.entity).insert(SessionMenus(menus));
    }
    harness.set_intent(PlaceIntent {
        pos: BlockPos::new(GROUND[0], GROUND[1], GROUND[2]),
        face: BlockFace::North,
    });

    {
        let mut world = harness.ecs.write();
        world.insert_resource(UsingItem(true));
    }
    let actions = harness.tick();
    assert_eq!(
        harness.outcome(),
        PlaceOutcome::default(),
        "the human path must take priority: the outcome must read exactly as \
         it did at spawn, not a fabricated Idle overwrite and not the \
         plugin's own attempt"
    );
    assert!(
        harness.has_intent(),
        "the intent must survive a human-preempted tick — it was never \
         consulted, so it is not yet resolved and must not be dropped"
    );
    assert!(actions.is_empty());
}
