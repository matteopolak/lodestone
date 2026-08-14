//! The windowless, GPU-less **simulation**: the generated world, the player
//! driven by the real physics engine, the off-thread mesh scheduler, and the
//! optional live connection. Keeping this free of winit and wgpu is what lets
//! the interesting logic — stepping, meshing, camera derivation — be unit tested
//! headlessly, with the windowed layer in [`crate::app`] staying a thin driver.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World as EcsWorld;
use lodestone_assets::{Language, ResourceLocation};
use lodestone_client::{BlockPos, ClientAction, Hand, OpenMenuSnapshot, Rotation};
// `ControllerPlugin` is no longer named here: composition moved to
// `Sim::client_app`, which reaches it through `lodestone_app::client_app` along
// with `CorePlugin`, `LocalPlayerPlugin` and `SessionHudPlugin`.
use lodestone_controller::{InputState, RawInput, apply_look_inverted};
pub use lodestone_ecs::{SessionEnd, SessionEndKind, SessionPhase};
use lodestone_ecs::entity::{Attributes, EntityIndex, EntityKind, MinecraftEntityId, Position};
use lodestone_ecs::player::{
    ActionQueue, AttackStrengthTicker, CollisionSource, Dead, Egress, MovementIntent,
    NearbyEntities, PhysicsState, PlayerCollision, PrevPosition, Profile, SelectedSlot, Submersion,
    reset_local_player,
};
use lodestone_ecs::session::{
    ActionBarOverlay, HudEffects, Phase, RespawnCount, ServerDifficulty, ServerEntityId,
    SessionBlockDestruction, SessionChat, TitleOverlay, Vitals, Xp,
};
use lodestone_ecs::{
    ChunkWorld, ChunkWorldWrite, EcsHandle, Extract, FrameClock, GameTick, Update, VersionData,
};
use lodestone_entity::attribute::attribute_value;
use lodestone_entity::pose::EntityPose;
use lodestone_game::menu::Menu;
use lodestone_game::mining::{BreakInputs, Mining};
use lodestone_game::placement::{
    Axis, Half, OrientationKind, Placement, PlacedState, UseOnContext, UseOnDecision,
};
use lodestone_model::event::EquipmentSlot;
use lodestone_model::{BlockFace, EntityInteraction, Vec3f};
use lodestone_particle::emit as particle_emit;
use lodestone_physics::{
    CollisionView, EntityDimensions, FluidState, NearbyEntity, PhysicsProfile, PlayerState, Vec3d,
};
// `SectionLight` anonymously: it carries `sky_light`/`block_light` on
// `WorldSectionLight`, and naming it would collide with `lodestone_world`'s
// storage type of the same name.
use lodestone_render::{AnimInput, BlockAtlas, Camera, SectionLight as _};
use lodestone_world::{BlockEntitySync, ChunkPos, World};

use crate::audio::ShellAudio;
use crate::blocks::id;
use crate::camera_rig::{
    ViewBob, apply_spyglass_fov, bobbed_camera, build_camera, third_person_camera,
};
use crate::chat::compose_chat_action;
use crate::collision::{LiveCollision, WorldCollision};
use crate::config::{Config, Mode};
use crate::entities::EntityDraw;
use crate::gpu::ThirdPersonBodyState;
use crate::hud::{DebugStats, process_rss_bytes};
use crate::interact::{
    Attacking, EntityRayTarget, InteractPlugin, MiningPredictor, NetHandle, ParticleSim,
    PlacementPredictor, RayTarget, UsingItem,
};
use crate::mesher::{MeshPolicy, MeshScheduler, Meshed, SectionKey, TerrainMesh, TerrainPlugin};
use crate::net::{NetClient, NetUpdate};
use crate::overlay::{BossBarView, Sidebar};
use crate::particles::{ParticleFrame, ParticleInstance, Particles};
use crate::raycast::{REACH, RayHit, ray_aabb, raycast};
use crate::resources::BlockResources;
use crate::worldgen;

/// A borrowed translation closure: `key → resolved format string`, the shape
/// [`lodestone_game::text::resolve`] consumes. Factored out so the projection
/// helpers and the `Sim` accessors share one name for it.
pub(crate) type Translator<'a> = Box<dyn Fn(&str) -> Option<String> + 'a>;

// The fixed timestep constant used to live here as `TICK_DT`. It is
// `lodestone_ecs::TICK_PERIOD` now, beside the one accumulator that counts in it
// (§4.1(c)) — a driver-local copy is how the shell came to have a `0.25 s` catch-up
// clamp that nothing else in the tree knew about.
/// Cap how far worldgen spans regardless of render distance, so start-up meshing
/// stays snappy for the demo. Only [`Sim::with_demo_world`] generates that world;
/// a real client session has no offline terrain at all.
const MAX_WORLD_RADIUS: i32 = 6;
/// Where the player stands before a session exists, in the real client
/// ([`Sim::new`]) which has no offline world to place them in.
///
/// A pure placeholder: the login teleport overwrites it within the first few
/// packets, and physics is frozen until then (see [`Sim::physics_tick`]) because
/// there is nothing to stand on. Deliberately *not* `worldgen::spawn_feet()` —
/// that samples the demo generator's noise, and the client no longer has a demo
/// world for the answer to mean anything.
const PRE_SESSION_FEET: [f64; 3] = [0.5, 71.0, 0.5];
/// Block placed by right-click interaction (the demo palette has no inventory).
const PLACE_BLOCK: u32 = id::STONE;
/// Vanilla's `DEFAULT_ENTITY_INTERACTION_RANGE` (`Player.java`) — the reach
/// for attacking/interacting with an entity, distinct from and shorter than
/// [`REACH`] (block interaction range, `Player.java`'s `4.5`). Creative
/// adds a further `+2.0` modifier (`Player.java`) that this shell does not
/// track, so every session uses the unmodified survival default.
const ENTITY_REACH: f64 = 3.0;
/// Number of hotbar slots (vanilla is a fixed 9).
///
/// `pub(crate)` so `interact.rs`'s `drive_select_slot` can apply the same range
/// gate `Sim::select_slot` does, without a second copy of the `9` to drift.
pub(crate) const HOTBAR_SLOTS: usize = 9;

/// The live [`Mining`] predictor's [`BreakInputs`] for one dig tick, built from
/// the version's block-state hardness census, the resolved held-item
/// contribution, and the player's own state.
///
/// Free-standing and pure so the two traps folded into it are testable without a
/// server, a GPU, or a generated world — both are wrong in the direction of
/// *breaking too fast*, which is exactly the defect this path already shipped
/// once.
///
/// # Trap 1: `correct_tool` comes from `tool`, never re-derived from `requires_correct_tool`
///
/// [`BlockHardness::requires_correct_tool`] is `BlockState.requiresCorrectToolForDrops`
/// — a property of the *block* ("does this drop nothing unless mined with a
/// suitable tool?"). [`ToolMining::correct_tool`] (and, downstream,
/// [`BreakInputs::correct_tool`]) is `Player.hasCorrectToolForDrops` — a property
/// of the *held item vs. the block* — and it selects vanilla's `30` (correct) vs
/// `100` (wrong) speed divider. `tool.correct_tool` is **already folded**
/// (`!requires_correct_tool || item_is_correct`, see the warning on
/// [`ToolMining`]/[`BlockHardness`]) by whatever produced `tool` — bare-handed
/// that reduces to `!requires_correct_tool`. Assign it straight across; combining
/// it with `requires_correct_tool` again makes stone break in **45 ticks instead
/// of 151** (3.4× too fast) while dirt goes the other way (51 instead of 15).
///
/// # Trap 2: `submerged` is `eye_in_water`, not `under_water()`
///
/// Vanilla's `getDestroySpeed` gates the 5×-slower underwater factor on
/// `isEyeInFluid(WATER)` **alone**. [`FluidState::under_water`] is
/// `eye_in_water && in_water()` — the predicate the *fog* wants, and vanilla's
/// `isUnderWater()`. The two agree in nearly every real pose but are not the
/// same function, so the mining path reads the raw `eye_in_water` flag.
///
/// # Mining efficiency, haste and fatigue are still unmodeled
///
/// Everything but `hardness`/`correct_tool`/`tool_speed`/`is_air`/`on_ground`/
/// `submerged` is left at [`BreakInputs::default`] — no enchantment, potion or
/// attribute inputs are modeled yet, only the tool census resolved by
/// [`lodestone_model::VersionAdapter::tool_mining`].
pub(crate) fn dig_break_inputs(
    entry: lodestone_model::BlockHardness,
    tool: lodestone_model::ToolMining,
    is_air: bool,
    on_ground: bool,
    submerged: bool,
) -> BreakInputs {
    BreakInputs {
        hardness: entry.hardness,
        is_air,
        // See "Trap 1" above — this straight assignment (not a re-negation) is
        // load-bearing.
        correct_tool: tool.correct_tool,
        tool_speed: tool.speed,
        on_ground,
        submerged,
        ..BreakInputs::default()
    }
}

/// The bare-hand [`lodestone_model::ToolMining`] fold, for when
/// [`lodestone_model::VersionAdapter::tool_mining`] has nothing to resolve
/// against (no held item) or is unreachable.
///
/// Mirrors v770's `tool::bare_handed` exactly (`speed: 1.0`,
/// `correct_tool: !requires_correct_tool`, `damage_per_block: 0`) rather than
/// depending on it, since this version-free crate cannot name a protocol crate.
/// This is Trap 1's negation, kept in exactly one place.
pub(crate) fn bare_handed_tool_mining(
    entry: lodestone_model::BlockHardness,
) -> lodestone_model::ToolMining {
    lodestone_model::ToolMining {
        speed: 1.0,
        correct_tool: !entry.requires_correct_tool,
        damage_per_block: 0,
    }
}

/// Lifts a hotbar slot's canonical stack into the minimal
/// [`lodestone_model::ItemStack`] shape
/// [`lodestone_model::VersionAdapter::tool_mining`] expects.
///
/// # The `minecraft:tool` patch is carried, not defaulted
///
/// This used to hand `tool_mining` a bare
/// [`lodestone_model::ItemComponents::default`], i.e. `ToolPatch::Inherited`,
/// on the grounds that the canonical stack could not carry a tool component
/// anyway. That was true when it was written and is no longer: the
/// `&lodestone_model::ItemStack → lodestone_game::item::ItemStack` conversion
/// now folds the tool patch in (as [`ComponentValue::Tool`], and *only* when the
/// wire patch was `Set` or `Removed` — `Inherited` is deliberately left absent
/// so "no override" cannot be confused with "an empty override").
///
/// So the round trip is exact in both directions, and the case the old
/// behaviour could not see now works: a server or datapack that overrides
/// `minecraft:tool` explicitly on the wire (`/give …[minecraft:tool={…}]`).
/// Before this, such an item resolved as if the *item default* applied — a
/// custom-speed pickaxe dug at its vanilla rate.
///
/// An ordinary vanilla tool is unaffected: it ships an `Inherited` patch (the
/// component lives in the item's built-in prototype, not the wire delta — see
/// `docs/tool-mining.md`), so nothing is inserted, nothing is read back, and
/// `tool_mining` resolves it against the item id via the version's generated
/// prototype table exactly as before.
///
/// [`ComponentValue::Tool`]: lodestone_game::item::ComponentValue::Tool
pub(crate) fn tool_mining_item(
    held: &lodestone_game::item::ItemStack,
) -> lodestone_model::ItemStack {
    let tool = match held
        .components()
        .get_str(lodestone_game::item::TOOL_COMPONENT)
    {
        Some(lodestone_game::item::ComponentValue::Tool(patch)) => patch.clone(),
        // Absent (the `Inherited` case) or — defensively — some other component
        // value stored under the tool key: fall back to "no override", which is
        // what `Inherited` means.
        _ => lodestone_model::ToolPatch::Inherited,
    };
    lodestone_model::ItemStack {
        item: held.item().clone(),
        count: u32::try_from(held.count()).unwrap_or(0),
        components: lodestone_model::ItemComponents {
            tool,
            ..lodestone_model::ItemComponents::default()
        },
    }
}

mod placement;

// Re-exported so every existing call site in this file (and `sim/tests.rs`'s
// `use super::*;`) keeps compiling unqualified. `predicted_placement_state`/
// `write_predicted_block` are `pub use`, not plain `use`: both are named by
// their original `crate::sim::`/`lodestone::sim::` path from
// `block_entities.rs` and from `tests/placed_chest_block_entity_pixels.rs`
// (an external integration test), and only `pub use` preserves that path
// through the move. See `sim/placement.rs`'s own module doc for why this
// block moved out at all.
pub use placement::{predicted_placement_state, write_predicted_block};
use placement::is_air_state;
// `#[cfg(test)]`, unlike `is_air_state` above: `sim/tests.rs`'s own
// `PlacementFacts { .. }` literals and `is_interactable_state(..)` calls are
// the only remaining callers now that `placement_facts` (the free function in
// `sim/placement.rs`) moved the real call site out of `sim/actions.rs`
// entirely — same "dead code in a `--lib`-only build" reasoning `#[cfg(test)]
// use meshing::dirty_sections_for_blocks;` already documents a few hundred
// lines down.
#[cfg(test)]
use placement::{PlacementFacts, is_interactable_state};
// `pub(crate)`, not a plain `use`: `crate::interact::drive_placement` is a
// sibling module of `sim`, not a descendant, so it names these as
// `crate::sim::{placement_facts, block_intersects_player, block_states_of,
// orientation_for_placement, state_for_placement}` rather than inheriting
// them through `sim::actions`'s `use super::*;` the way every `sim/*.rs` seam
// file does. See `sim/placement.rs`'s own doc.
pub(crate) use placement::{
    block_intersects_player, block_states_of, orientation_for_placement, placement_facts,
    state_for_placement,
};


/// Map a raycast hit's outward face normal to the [`BlockFace`] that was struck.
pub(crate) fn face_from_normal(normal: [i32; 3]) -> BlockFace {
    match normal {
        [0, 1, 0] => BlockFace::Up,
        [0, -1, 0] => BlockFace::Down,
        [0, 0, 1] => BlockFace::South,
        [0, 0, -1] => BlockFace::North,
        [1, 0, 0] => BlockFace::East,
        // The raycast only ever yields a unit axis normal; treat any residue as
        // the remaining west face rather than panicking on malformed input.
        _ => BlockFace::West,
    }
}

// `parse_goto_command` lived here — a strict `goto x z` parser for the
// `#goto` chat command (issue #38, M1). It went with the command: the shell no
// longer depends on `lodestone-autopilot`, so there is nothing for a parsed
// coordinate pair to be handed to. See `Sim::send_chat`, which still reserves
// the `#` namespace so such a line cannot leak to chat, and issue #118, which
// is where a plugin will eventually register its own commands (and its own
// argument parsing — `crates/lodestone-command` already exists for that, and
// re-adding a bespoke parser here would be the wrong direction).

/// [`BlockFace`] to [`particle_emit::Face`] — the two enumerate the same six
/// directions under different names because they come from different crates
/// (`lodestone-model` for protocol-facing code, `lodestone-particle` for the
/// vanilla particle simulation), not because they disagree about anything.
pub(crate) fn particle_face(face: BlockFace) -> particle_emit::Face {
    match face {
        BlockFace::Down => particle_emit::Face::Down,
        BlockFace::Up => particle_emit::Face::Up,
        BlockFace::North => particle_emit::Face::North,
        BlockFace::South => particle_emit::Face::South,
        BlockFace::West => particle_emit::Face::West,
        BlockFace::East => particle_emit::Face::East,
    }
}

/// The block-local hit position `use_item_on` expects — vanilla's
/// `BlockHitResult` cursor, which is `location - blockPos`
/// (`writeBlockHitResult`).
///
/// This used to be the centre of the struck *cube* face, because the raycast
/// reported only a block and a normal. It now reports the exact entry point of
/// the outline box it hit ([`RayHit::cursor`], issue #375), which is what the
/// server uses to pick a slab's half, a stair's orientation and which side of a
/// block a torch attaches to — so the approximation is gone rather than
/// documented.
pub(crate) fn hit_cursor(hit: RayHit) -> Vec3f {
    let [x, y, z] = hit.cursor();
    Vec3f::new(x, y, z)
}

/// Native player-inventory index of the off-hand slot
/// (`lodestone_game::menu`'s table: hotbar `0..=8`, off-hand `40`).
///
/// Shared between `Sim::use_item_live`'s off-hand `haveSomethingInOurHands`
/// check and `crate::interact::drive_placement`'s identical one, rather than
/// two copies of a bare `40` agreeing only by coincidence.
pub(crate) const OFFHAND_NATIVE_INDEX: usize = 40;

/// The [`ChunkWorld`] store, adapted to the ECS's [`CollisionSource`].
///
/// The indirection exists because [`WorldCollision`] borrows its world, and a
/// `bevy_ecs` `Resource` must be `'static`. Handing the view out through a
/// callback (rather than storing one) is what lets a borrowed adapter reach a
/// scheduled system at all — see [`CollisionSource`]'s docs. `ChunkWorld` is an
/// `Arc` handle, so this is `'static` while still reading the live store.
///
/// # What Stage 4 deleted here
///
/// This replaced `DemoCollision(World)`, an **owned clone** of the whole offline
/// world rebuilt lazily whenever it went stale. The clone existed only because
/// there was no `Arc` to hold, and it came with a hazard recorded in
/// `docs/local-player-components.md`: *"anything that mutates `Sim.world` must
/// clear `Sim.demo_collision`"*, a missed invalidation reading as "I mined the
/// block but still cannot walk through it". With one shared store there is
/// nothing to invalidate and no `O(loaded columns)` clone per block edit — the
/// rule is retired rather than merely followed.
///
/// Deliberately *not* a hand-written `impl CollisionView` that re-delegates
/// [`WorldCollision`]'s thirteen methods: a method later added to
/// `CollisionView` would be overridden by `WorldCollision` and silently fall
/// back to the trait default here, which is exactly the "two adapters, one of
/// them subtly wrong" failure [`crate::collision`]'s module docs warn about.
/// This constructs the real adapter and asks it.
struct ChunkWorldCollision(ChunkWorld);

impl std::fmt::Debug for ChunkWorldCollision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never dump a whole world; the useful scalar is how much of one this is.
        f.debug_struct("ChunkWorldCollision")
            .field("columns", &self.0.len())
            .finish_non_exhaustive()
    }
}

impl CollisionSource for ChunkWorldCollision {
    fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView)) {
        f(&WorldCollision::new(&self.0.read()));
    }
}

/// The live server terrain around the player, as a [`CollisionSource`].
///
/// [`LiveCollision`] is already an owned snapshot (`Arc<ChunkSection>` handles
/// plus the atlas), so this is pure plumbing — but it is the reason the whole
/// seam works: `Sim::live_collision` returning owned data is what makes the
/// live path expressible as a resource at all.
struct LiveCollisionSource(LiveCollision);

impl std::fmt::Debug for LiveCollisionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveCollisionSource")
            .finish_non_exhaustive()
    }
}

impl CollisionSource for LiveCollisionSource {
    fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView)) {
        f(&self.0);
    }
}

/// The whole non-graphical game state.
#[derive(Debug)]
pub struct Sim {
    /// Parsed configuration.
    pub config: Config,
    /// Latest debug stats (the app fills in FPS/frame-time/GPU counters).
    pub stats: DebugStats,
    /// **The** bevy `World`, per §4.1(c) — one per process, behind the shared
    /// lock so the net thread can fold into it.
    ///
    /// `Sim` holds **no** `PlayerState`, `InputState`, `PhysicsProfile`,
    /// `FluidState`, hotbar slot, fly flag, death flag or wire edge-tracker of
    /// its own, since Stage 4 **no `lodestone_world::World`, no mesh scheduler
    /// and no mesh queues** either, since Stage 5 **no frame clock, chat log,
    /// pick target, particle simulation, mining/placement predictor or version
    /// adapter**, and since §4.1(c) **no entity interpolator**: this is the sole
    /// store, reached through the accessors below. That is the stages' authority
    /// test — a second copy here would make a plugin's write to a component a
    /// write to nothing.
    ///
    /// # What made this the *one* `World`
    ///
    /// There were three: the net thread's (`lodestone_client::state::SharedState`,
    /// authoritative over the network read-model), the entity interpolator's, and
    /// this one. `Sim` now builds this one and threads the handle **down** —
    /// `attach_net` hands it to `NetClient::connect`, which hands it to
    /// `ClientBuilder::ecs`, so ingest folds into the `World` these systems read.
    /// The interpolator's is gone: [`crate::entities::EntityInterpPlugin`] is
    /// installed here and its systems run in these schedules off this clock.
    ///
    /// The direction is load-bearing and not symmetric: adopting the *client's*
    /// handle instead would make the `World`'s identity change at every connect,
    /// and [`Self::local`] — held across [`Self::end_session`] by the
    /// voluntary-teardown path — would be invalidated by each reconnect.
    ///
    /// # Locking
    ///
    /// Every access goes through [`Self::read`] / [`Self::write`], which take a
    /// short guard and drop it. Read [`lodestone_ecs::EcsHandle`]'s three rules
    /// before adding a call site: in particular **never** call into `NetClient` /
    /// `ClientHandle` with a guard live, because those lock this same `World` and
    /// `parking_lot::RwLock` is neither reentrant nor upgradable.
    ecs: EcsHandle,
    /// The local player's entity in [`Self::ecs`]. Stable for the lifetime of
    /// the `Sim`, including across [`Sim::end_session`], because the driver and
    /// (later) plugins hold it.
    ///
    /// Since §4.1(c) it is **also** the client's session entity: one `World`, one
    /// `LocalPlayer`. `spawn_local_player` and `spawn_session` both spawn an
    /// entity marked `LocalPlayer`, so leaving them separate in one `World` would
    /// give every `With<LocalPlayer>` system two players.
    local: Entity,
    net: Option<NetClient>,
    /// Whether the [`ChunkWorld`] resource in [`Self::ecs`] is the *client's*
    /// store rather than this `Sim`'s own offline one.
    ///
    /// Not a second copy of anything: it records **which** store the one resource
    /// currently names, which nothing else can answer once `net` is dropped. It is
    /// what lets [`Sim::end_session`] release a server's terrain while leaving the
    /// `with_demo_world` fixture's terrain alone.
    adopted_live_world: bool,
    status: String,
    /// The loading screen's current step (issue #449) — set only from
    /// `NetUpdate::ConnectPhase`/`LoggedIn`, i.e. from real boundaries in the
    /// session task, so it can never advance on a timer. Read by
    /// `WindowApp::drive_ui_from_session`.
    connect_phase: crate::menu::loading::ConnectPhase,
    /// Columns the initial view will contain, `None` until a session declares
    /// its view radius (`Sim::set_view_radius`). The progress bar's denominator;
    /// `None` means "no denominator, so no bar" rather than a guessed one.
    expected_view_columns: Option<usize>,
    /// The raw view radius `Sim::set_view_radius` was called with — the same
    /// value `expected_view_columns` was squared from, kept alongside it
    /// because the chunk-status grid (issue #568) needs a side length, not an
    /// area. `None` under the same condition `expected_view_columns` is.
    expected_view_radius: Option<u32>,
    /// When the terrain-streaming phase began, for
    /// [`crate::menu::loading::CLIENT_WAIT_TIMEOUT`].
    ///
    /// `crate::platform::Instant`, not `std::time::Instant`: the shell has a wasm
    /// target and `std`'s panics there. Set once, by `Sim::set_connect_phase` on
    /// the transition *into* `LoadingTerrain`, so it measures the phase rather than
    /// the session — and only from that real boundary, which is what keeps the
    /// screen's phase label off a timer even though its dismissal now has one.
    /// `None` outside that phase, which reads as "not waiting".
    terrain_wait_started: Option<crate::platform::Instant>,
    /// The stitched vanilla atlas for the live world, or `None` when running on
    /// the demo palette. Its presence is the single discriminant for "render the
    /// live server world with the vanilla atlas" vs "mesh the demo world": the
    /// two use disjoint block-id spaces and must never be meshed with the wrong
    /// classifier.
    vanilla_atlas: Option<Arc<BlockAtlas>>,
    /// The [`crate::resources::pack_generation`] value as of the last time
    /// [`Sim::reload_resource_pack_atlas`] looked — an equality guard with
    /// the same job as [`crate::mesher::TerrainMesh::set_cutout_leaves`]'s:
    /// that method is polled every frame, and without this a resource-pack
    /// reload (rebuilding the atlas, respawning the mesh worker pool,
    /// re-meshing every loaded column) would run on *every* frame instead of
    /// once per real pack-selection change.
    last_pack_generation: u64,
    /// The stitched particle sheet, kept alive so `app.rs` can upload the *same*
    /// object to the GPU that the emitter's `(Sheet, frame) -> UV` table was
    /// built from — see [`Sim::particle_sheet_atlas`] and issue #45. `None` on
    /// the demo palette.
    particle_atlas: Option<Arc<lodestone_assets::ParticleAtlas>>,
    /// The vanilla `en_us.json` table for resolving server-authored `translate`
    /// components (death messages, scoreboard titles, tab-list names, …) into
    /// words before they reach the HUD. `None` on the demo palette or a pack
    /// without a language file, in which case components render via their own
    /// `fallback`/key — never a raw error. Loaded once with the atlas from the
    /// same pack, so it shares the atlas's ownership and lifetime.
    language: Option<Arc<Language>>,
    /// Count of server `TeleportPlayer` corrections adopted since start. At rest
    /// on settled ground this stays flat; a burst *during* a jump is the
    /// signature of the server rejecting the ascent and snapping the camera down
    /// (the "jumping glitches down" defect). Read by the live jump gate to
    /// distinguish a clean vanilla arc from a server-corrected one.
    pub teleport_count: u64,
    /// Diagnostic switch (normal play: always `true`): when `false`, the live
    /// path collides against **an empty world** instead of the server terrain.
    /// This exists to reproduce the pre-collision "fall through absent ground /
    /// rubber-band" behaviour as a negative control in the live gate; it is never
    /// flipped in real play.
    ///
    /// "An empty world" is what this always *meant*: the pre-live-collision shell
    /// collided a live session against its own offline world, and a client session
    /// has none, so at a far spawn there was nothing under the player. Stage 4
    /// made that explicit rather than incidental — with one chunk store, "the
    /// offline world" and "the server's world" are the same handle, so falling
    /// back to it would have quietly started colliding the control against real
    /// live terrain under the *demo* classifier and the control would have stopped
    /// failing. See [`Sim::tick_collision`].
    pub collide_against_live_world: bool,
    /// Debug-overlay line set when vanilla assets failed to load and the session
    /// fell back to the demo palette.
    asset_banner: Option<String>,
    /// Whether [`Self::refresh_mesh_policy`] has already fired its one-time
    /// `tracing::error!` for an id-space mismatch this session. Without this gate
    /// a live session with no vanilla atlas would repeat the diagnostic every
    /// frame — the exact per-column noise problem that diagnostic exists to
    /// replace, one layer up. Reset by [`Self::end_session`] so a reconnect that
    /// hits the same defect is warned again rather than staying silenced by the
    /// previous session's flag.
    warned_id_space_mismatch: bool,
    /// Test seam (normal play: always `true`): when `false`, death is treated as
    /// the terminal `SessionPhase::Ended` it used to be, reproducing the "stuck
    /// on the death screen forever" bug as the live gate's negative control. Never
    /// flipped in real play.
    pub recover_from_death: bool,
    /// The most recent death's message (`NetUpdate::Death`'s `message`, already
    /// flattened to plain text), for the death screen (issue #103) to draw.
    /// `Some` from the moment [`Self::set_dead`] marks the player dead until the
    /// server-confirmed respawn clears it (or [`Self::end_session`] resets it).
    /// Not an ECS component: it is read by exactly one consumer (`app.rs`'s
    /// per-frame UI reconciliation) and does not need to survive a session
    /// teardown/reconnect the way `RespawnCount` and the other session-lifetime
    /// state in `lodestone_ecs::session` do.
    death_message: Option<String>,
    /// Set once `NetUpdate::WinGame` (issue #192) has arrived — the local
    /// player exited the End through the exit portal after the dragon fight.
    /// Latched rather than transient for the same reason [`Self::death_message`]
    /// is a plain field, not an ECS component: exactly one consumer reads it
    /// (`app.rs`'s per-frame UI reconciliation, `drive_ui_from_session`), and
    /// there is no per-session state to fold it into. Reset by
    /// [`Self::end_session`] so a later session starts un-won.
    won: bool,
    /// The dimension whose one-time client-side reset has already run — **an edge
    /// detector, not a source of truth.**
    ///
    /// "We are in dimension X" is owned by
    /// [`lodestone_ecs::session::ServerDimension`] and read through
    /// [`Self::dimension`]; this field answers only "have I already dropped the
    /// entities and meshes for the dimension I am in". `Some` for a live session
    /// from the first `Login`, `None` before login and after
    /// [`Self::end_session`].
    ///
    /// See `sim/dimension.rs`'s module doc for why an edge is needed here when the
    /// per-frame reads (fog, sky mode, sky-light default) need none.
    applied_dimension: Option<lodestone_client::DimensionId>,
    /// Vanilla's `LocalPlayer.portalEffectIntensity`, `0.0..=1.0` — the
    /// portal-transition screen effect's own state, advanced once per tick by
    /// `Sim::tick_portal_effect`.
    ///
    /// A plain field for the same reason [`Self::death_message`] and
    /// [`Self::won`] are: it is client-only, has one consumer
    /// (`app/redraw.rs`'s `ScreenEffects`), and must not survive a session
    /// teardown — which [`Self::end_session`] handles through
    /// `Sim::reset_dimension_state` so this and `applied_dimension` cannot be
    /// reset in one place and forgotten in the other.
    portal_effect_intensity: f32,
    /// The previous tick's [`Self::portal_effect_intensity`] — vanilla's
    /// `oPortalEffectIntensity`.
    ///
    /// Present so the overlay can be *interpolated* rather than sampled: the ramp
    /// advances at 20 Hz over four seconds and the overlay draws at the frame
    /// rate, so reading the raw value paints a visible staircase.
    prev_portal_effect_intensity: f32,
    /// The camera mode — vanilla's [`CameraType`](crate::camera_rig::CameraType),
    /// all three states, cycled by `F5` through [`Self::cycle_camera_type`].
    ///
    /// This was a plain `bool` and the doc here argued no richer enum was needed,
    /// on the grounds that
    /// [`RenderState::set_third_person_body_source`]'s closure returning
    /// `None`/`Some` *is* the camera-mode toggle. That part is still true and is
    /// still the seam the renderer sees — but it is a two-state seam because it
    /// answers `isFirstPerson()`, and vanilla's own state has three. The front
    /// view was simply missing, not implemented-and-unwired.
    ///
    /// Every consumer here asks
    /// [`CameraType::is_first_person`](crate::camera_rig::CameraType::is_first_person)
    /// rather than comparing against a variant; only
    /// [`crate::camera_rig::third_person_camera`] cares which of the two detached
    /// modes it is.
    ///
    /// [`RenderState::set_third_person_body_source`]: crate::gpu::RenderState::set_third_person_body_source
    camera_type: crate::camera_rig::CameraType,
    /// The local player's own walk/head-look/**arm-swing** animation clock,
    /// driven once per physics tick from its real position/orientation exactly the
    /// way `entities.rs` drives an [`EntityPose`] for a tracked network entity
    /// (see [`Self::step`]). Ticked unconditionally (cheap, and always correct if
    /// the camera mode flips mid-flight).
    ///
    /// Read by **two** consumers, and it is the only thing they share:
    /// [`Self::third_person_body_state`] for the self-avatar's whole pose, and
    /// [`Self::hand_swing_progress`] for the first-person arm's swing. The swing
    /// half is started by [`Self::swing_hand`].
    body_pose: EntityPose,
    /// The camera's own eased eye height — vanilla's `Camera.eyeHeight` /
    /// `eyeHeightOld` pair, **not** the entity's.
    ///
    /// `Camera.tick()` does `eyeHeight += (entity.getEyeHeight() - eyeHeight) * 0.5F`,
    /// so the camera *chases* the entity's eye rather than adopting it. We had no
    /// equivalent, so [`Self::camera`] was handed the raw pose eye height every
    /// frame — and since the pose fit gate made that atomically snap between
    /// `1.62` standing and `0.4` swimming, entering or leaving water jerked the
    /// view by 1.22 blocks in a single frame. Ticked once per physics tick beside
    /// [`Self::body_pose`], read interpolated in [`Self::camera`].
    eye_height_smoother: crate::camera_rig::EyeHeightSmoother,
    /// The walk bob's phase and amplitude (issue #58) — like
    /// [`Self::eye_height_smoother`] and [`Self::body_pose`], per-tick state that
    /// cannot be a pure function of the current [`PlayerState`]. Ticked once per
    /// physics tick in [`Self::step`], read interpolated in
    /// [`Self::render_camera`] — **not** [`Self::camera`], which is the pick ray
    /// and the audio listener; see that method's docs.
    view_bob: ViewBob,
    /// Vanilla's View Bobbing option ([`crate::config::Options::view_bobbing`]),
    /// pushed down from the menu layer by [`Self::set_view_bobbing`] rather than
    /// read from disk here — `Sim` owns no `Options`, and the menu is the only
    /// thing that can change it.
    view_bobbing: bool,
    /// Vanilla's Damage Tilt accessibility option
    /// ([`crate::config::Options::damage_tilt_strength`]), pushed down the same way
    /// as [`Self::view_bobbing`] by [`Self::set_damage_tilt_strength`].
    ///
    /// Seeded to vanilla's `1.0` rather than `0.0` so a caller that forgets the
    /// setter gets the vanilla behaviour — the same convention `view_bobbing`'s
    /// build-time default follows, and the reason matters here: `0.0` is a *valid*
    /// user choice that disables the effect, so a zero default would be
    /// indistinguishable from the accessibility option being on.
    damage_tilt_strength: f32,
    /// Vanilla's **FOV** option ([`crate::config::Options::fov`]) in degrees,
    /// pushed down the same way as [`Self::view_bobbing`] by
    /// [`Self::set_fov_y_degrees`] and read by [`Self::camera`].
    ///
    /// Seeded to [`crate::camera_rig::FOV_Y_DEGREES`] — vanilla's own `70` — so a
    /// caller that never calls the setter (a headless bot, a test) sees exactly
    /// the projection it saw when `build_camera` wrote that constant itself.
    ///
    /// A whole `f32` rather than the option's `u32` degrees because that is what
    /// [`Camera::fov_y_degrees`](lodestone_render::Camera) is, and the spyglass
    /// zoom multiplies it by `0.1`: rounding back to an integer between the option
    /// and the camera would quantise a 3° scoped view to 3.
    fov_y_degrees: f32,
    /// Vanilla's `invertMouseX`/`invertMouseY` options
    /// ([`crate::config::Options::invert_mouse_x`]/`invert_mouse_y`, issue
    /// #203), pushed down the same way as [`Self::view_bobbing`] — see
    /// [`Self::set_mouse_invert`]. Read by [`Self::apply_mouse`], which calls
    /// [`lodestone_controller::apply_look_inverted`] instead of the plain
    /// `apply_look` now that there is somewhere to source the two bools from.
    invert_mouse_x: bool,
    invert_mouse_y: bool,
    /// Vanilla's `sensitivity` option ([`crate::config::Options::sensitivity`],
    /// issue #443), pushed down the same way as [`Self::invert_mouse_x`] — see
    /// [`Self::set_sensitivity`].
    ///
    /// This exists because [`Self::apply_mouse`] previously read
    /// `self.config.sensitivity`, which is the **argv-derived** [`Config`]
    /// value and is therefore fixed for the process's lifetime. #443 made the
    /// option persist, so dragging the slider wrote to disk and changed
    /// nothing until the next launch. Seeded from `config.sensitivity` at
    /// construction so a caller that never calls the setter (a headless bot,
    /// or a test) keeps exactly the old behaviour.
    sensitivity: f32,
    /// Vanilla's `key.sneak`/`key.sprint` hold-vs-toggle options
    /// ([`crate::config::Options::toggle_sneak`]/`toggle_sprint`, issue
    /// #202), pushed down the same way. Applied to the live [`InputState`]
    /// once per frame at the top of [`Self::step`] (see
    /// [`Self::set_toggle_modes`]) rather than at each key event: the option
    /// cannot change mid-frame, and `InputState::set_toggle_modes` is cheap
    /// and idempotent, so one push per frame covers every catch-up tick that
    /// frame runs.
    toggle_sneak: bool,
    toggle_sprint: bool,
    /// Vanilla's `key.attack`/`key.use` hold-vs-toggle options
    /// ([`crate::config::Options::toggle_attack`]/`toggle_use`, issue #444),
    /// pushed down the same way as [`Self::toggle_sneak`]/[`Self::toggle_sprint`]
    /// and applied to the live [`InputState`] in the same place. Carried by the
    /// sim even though `interact.rs` has no toggle-mode consumer yet — the
    /// *option* reaches the model end to end, so a future consumer reads it
    /// without touching the plumbing again.
    toggle_attack: bool,
    toggle_use: bool,
    /// Vanilla's `options.autoJump` ([`crate::config::Options::auto_jump`],
    /// issue #444), pushed down by [`Self::set_auto_jump`]. Read once per tick
    /// in [`Self::step`]'s loop to decide whether to request an auto-jump for
    /// the `GameTick` schedule — see the request firing there.
    auto_jump: bool,
    /// Vanilla's `options.sprintWindow` ([`crate::config::Options::sprint_window_ticks`],
    /// issue #444) — the double-tap-forward window in 20 Hz ticks, pushed down
    /// by [`Self::set_sprint_window_ticks`] and forwarded to the live
    /// [`InputState`] once per frame at the top of [`Self::step`].
    sprint_window_ticks: u8,
    /// Wall-clock instant the first chunk arrived at this client, for join-latency
    /// measurement. `None` until the first `NetUpdate::Chunk` is processed.
    /// `crate::platform::Instant`, not `std::time::Instant`, for the same reason
    /// `terrain_wait_started` above says: `std`'s traps on the shell's wasm32
    /// target. Latent rather than live today — this field is only ever set to
    /// `None` — but a future `Some(Instant::now())` here would be a browser crash,
    /// and `scripts/wasm-check.sh`'s `lodestone-shell instant-confinement` rule
    /// keeps it from being written with the trapping type.
    first_chunk_at: Option<crate::platform::Instant>,
    /// Per-position chest lid animation state (issue #23) — vanilla's
    /// `ChestLidController`, one per open or closing chest.
    ///
    /// A plain field rather than an ECS resource for the same reason
    /// [`Self::death_message`] is one: exactly one consumer reads it
    /// (`app.rs`'s per-frame render-source install, via
    /// [`Self::block_entity_source`]) and nothing needs it to survive a session
    /// teardown — a reconnect re-derives every lid from the block events the new
    /// session sends.
    ///
    /// Fed by `NetUpdate::BlockEvent` in [`Self::poll_net`] and advanced once per
    /// physics tick in [`Self::step`]. Both halves are required: the wire carries
    /// only "somebody is looking in this chest", and the *angle* is a client-side
    /// accumulator — see `crate::block_entities::ChestLids`.
    chest_lids: crate::block_entities::ChestLids,
    /// Per-position bell shake state (issue #23). Fed by `NetUpdate::BlockEvent`
    /// in [`Self::poll_net`] and advanced once per tick, exactly like
    /// [`chest_lids`](Self::chest_lids) — the same `b0 == 1` event drives both,
    /// and the *gather* is what decides which tracker a given position reads
    /// from (see `block_entities::BellShakes::apply_block_event`).
    bell_shakes: crate::block_entities::BellShakes,
    /// Per-position enchanting-table book animation state (issue #23).
    ///
    /// **Not fed by any packet**, which is what makes it the odd one of the three:
    /// `chest_lids` and `bell_shakes` are both started by `NetUpdate::BlockEvent`,
    /// and this one is started by the local player *standing near a block*. It is
    /// advanced once per tick in [`Self::step`] from the player's own position, so
    /// nothing on the wire would ever reveal that it had stopped — see
    /// `crate::block_entities::EnchantingTableBooks`.
    enchanting_table_books: crate::block_entities::EnchantingTableBooks,
    /// Per-position moving-piston animation state (issue #23).
    ///
    /// The **fourth** block-entity clock and the shortest by far: a whole push is
    /// two ticks. Like [`enchanting_table_books`](Self::enchanting_table_books) no
    /// packet starts it — the trigger is a `moving_piston` block entity appearing in
    /// the world — but unlike that one the wire does carry a *seed* (the NBT's
    /// `progress`), which is why an untracked position draws from the NBT rather
    /// than from zero. See `crate::block_entities::PistonMoves`.
    moving_pistons: crate::block_entities::PistonMoves,

    /// This frame's item pickups (`take_item_entity`), awaiting the fly-to-collector
    /// animation — issue #365.
    ///
    /// A **frame-scoped batch**, not persistent state: [`Self::poll_net`] folds
    /// every `NetUpdate::ItemPickup` into it and drains the whole lot into
    /// `crate::entities::begin_item_pickup` once, at the end of the same call, so a
    /// burst of pickups (walking through a pile of drops) takes one ECS write guard
    /// rather than one per item. The animation state itself lives in the one
    /// `World`, as `crate::entities::PickupAnimations`.
    ///
    /// `PickupFeed` had a correct, tested `apply`/`drain` and **no caller anywhere**
    /// before this field existed; it is reused rather than reimplemented for that
    /// reason.
    pickups: lodestone_game::mining::PickupFeed,
}

/// The live audio engine, promoted from a private `Sim` field to a bevy
/// [`Resource`] — see `docs/sim-dissolution.md`'s audio section.
///
/// `None` when disabled (no asset root, no device — see [`ShellAudio::from_env`]);
/// the whole audio path is `if let Some`, so a disabled engine is simply
/// silent, never a crash. `Send + Sync` was already measured for `ShellAudio`
/// itself (`docs/sim-dissolution.md`'s scratch probe); this is a pure move,
/// not a new guarantee, which is what makes it safe to do with no behaviour
/// change.
///
/// # Why this had to move
///
/// A private `Sim` field is invisible to a `GameTick` **system** — only a
/// `Sim` *method* could reach it, and a system is a free function over
/// `&mut World`, not a method. That is the whole reason `docs/plugin-api.md`
/// recorded `PlaceIntent` as blocked rather than built (`f6ab384`'s commit
/// message): a plugin-driven placement's local write is reachable from
/// `ChunkWorld` alone, but playing its placement sound the same way a human
/// placement does needs the audio engine from *inside* a system
/// (`crate::interact::drive_placement`), which a private field on a
/// `sim.rs`-only type can never be. It also frees a second, already-recorded
/// gap: `app.rs`'s `WindowApp::weather` doc names `lodestone_render::RainAmbience`
/// as having "no producer, because the only `ShellAudio` in the process is a
/// private field on `Sim` with no public play method" — this resource plus
/// [`Sim::play_local_sound`] is the fix for both at once.
#[derive(Debug, Default, Resource)]
pub struct AudioEngine(pub Option<ShellAudio>);

/// Vanilla's `MusicManager` and its RNG, as a resource beside [`AudioEngine`].
///
/// Config-scoped for the same reason [`AudioEngine`] is: a reconnect must not
/// restart the music or re-roll its delay clock, so this must never gain a line in
/// [`Sim::end_session`]'s reset list.
///
/// # Why an `Option` rather than a plain `ShellMusic`
///
/// It is a **move-out slot**, not an "audio might be missing" flag — music state
/// always exists. [`Sim::tick_music`] has to hold `ShellMusic` and
/// [`AudioEngine`] mutably at the same instant, and two `World::resource_mut`
/// borrows cannot coexist, so the state is taken out for the duration of the tick
/// and put straight back. `None` is therefore only ever observable *during* a
/// tick, from inside the tick itself.
#[derive(Debug, Default, Resource)]
pub struct MusicState(pub Option<crate::audio::music::ShellMusic>);

/// Vanilla's `BiomeAmbientSoundsHandler` plus the rain cadence, beside
/// [`MusicState`] and for the same reasons: config-scoped (a reconnect must not
/// re-roll the mood clock, so it must never gain a line in
/// [`Sim::end_session`]'s reset list) and an `Option` only because
/// [`Sim::tick_ambience`] has to hold it and [`AudioEngine`] mutably at one
/// instant, which two `World::resource_mut` borrows cannot express.
#[derive(Debug, Default, Resource)]
pub struct AmbienceState(pub Option<crate::audio::ambient::ShellAmbience>);

impl Sim {
    // -----------------------------------------------------------------------
    // The local player, which lives in `self.ecs` and nowhere else
    // -----------------------------------------------------------------------

    /// The one `World`, for a caller that wants to query or mutate the components
    /// directly — a plugin, a test, or the net thread.
    ///
    /// This is the seam that keeps the component set from being an island: a
    /// plugin can write [`PhysicsState`] and the next tick — and the next movement
    /// packet — reflect it. Since §4.1(c) it is also what a *plugin* would be
    /// handed, which is why it is the shared handle rather than a `&World`: there
    /// is exactly one, and the net thread holds a clone of it.
    ///
    /// Callers take their own short guard. Do not hold one across a call into
    /// `NetClient`/`ClientHandle` — see [`lodestone_ecs::EcsHandle`].
    #[must_use]
    pub fn ecs(&self) -> &EcsHandle {
        &self.ecs
    }

    /// Run `f` under a short **read** guard on the one `World`.
    ///
    /// A closure rather than a returned guard on purpose: a guard that outlives one
    /// statement is the shape that deadlocks, because `parking_lot`'s `read()`
    /// blocks behind a queued writer even on a thread that already holds a read
    /// lock. This keeps every borrow inside a scope the compiler can see.
    ///
    /// Goes through [`lodestone_ecs::hold_read`], so this hold is *measured* —
    /// see [`Self::lock_holds`].
    fn read<R>(&self, f: impl FnOnce(&EcsWorld) -> R) -> R {
        lodestone_ecs::hold_read(&self.ecs, f)
    }

    /// Run `f` under a short **write** guard on the one `World`.
    ///
    /// `&mut self` even though the lock makes it unnecessary: it is the borrow
    /// checker, not the lock, that stops a caller from reaching another `Sim`
    /// accessor (and so the same lock) from inside `f`. Note that [`Self::read`]
    /// takes `&self` and so does *not* get that protection — a `read` closure
    /// that called another accessor would deadlock behind a queued writer, and the
    /// only thing keeping that out is review.
    fn write<R>(&mut self, f: impl FnOnce(&mut EcsWorld) -> R) -> R {
        lodestone_ecs::hold_write(&self.ecs, f)
    }

    /// This `World`'s measured guard-hold statistics.
    ///
    /// The bound `lodestone_ecs::EcsHandle`'s lock discipline rests on is a
    /// *duration* one — a packet waits at most one guard hold — and §4.1(c)
    /// shipped it as an argument from reading the code. This is the number.
    /// `longest_ns` is the one that matters: it is how long an ingest write can be
    /// kept waiting. Pair with [`Self::reset_lock_holds`] to measure one interval.
    #[must_use]
    pub fn lock_holds(&self) -> lodestone_ecs::HoldStats {
        self.read(|w| w.resource::<lodestone_ecs::LockHolds>().snapshot())
    }

    /// Zero [`Self::lock_holds`], so a caller can attribute holds to one frame or
    /// one call rather than to the whole session.
    pub fn reset_lock_holds(&mut self) {
        // Reset *and* snapshot nothing: `write` records its own hold after `f`
        // returns, so the interval starts from one hold, not zero. That is why the
        // measurements below compare against a wall-clock total rather than
        // asserting an exact count.
        self.write(|w| w.resource::<lodestone_ecs::LockHolds>().reset());
    }

    /// [`Self::write`], with the local player's `Entity` handed in.
    ///
    /// Exists only because `&mut self` makes the closure unable to read
    /// `self.local` — which is the *point* of the `&mut`: it is what stops a
    /// closure reaching another accessor and so the same lock a second time. This
    /// is the one field it legitimately needs, so it is passed rather than
    /// captured.
    fn write_local<R>(&mut self, f: impl FnOnce(&mut EcsWorld, Entity) -> R) -> R {
        let local = self.local;
        lodestone_ecs::hold_write(&self.ecs, |w| f(w, local))
    }

    /// The local player's entity, for pairing with [`Self::ecs`].
    #[must_use]
    pub fn local_player(&self) -> Entity {
        self.local
    }

    // -----------------------------------------------------------------------
    // The chunk world and terrain meshing, which live in `self.ecs` (Stage 4)
    // -----------------------------------------------------------------------

    /// The **one** chunk store this session meshes, collides against and edits,
    /// as the read handle (issue #423's split).
    ///
    /// A handle, cheap to clone, onto the same `lodestone_world::World` the net
    /// thread writes decoded columns into once
    /// [`adopt_live_world`](Self::adopt_live_world) has run. Before Stage 4 there
    /// were two of these — `Sim`'s offline one and the client's live one — and
    /// every read site branched on which it meant. The read handle has no write
    /// path; to edit the store, take [`chunk_world_write`](Self::chunk_world_write)
    /// instead.
    #[must_use]
    pub fn chunk_world(&self) -> ChunkWorld {
        self.read(|w| w.resource::<ChunkWorld>().clone())
    }

    /// The write handle paired with [`chunk_world`](Self::chunk_world) — the
    /// only way this session may mutate the one chunk store.
    ///
    /// Installed at the same site as the read resource and always naming the
    /// same `Arc`; panics if it is missing, which is a bug in the installer, not
    /// a recoverable absence — every `Sim` construction routes through
    /// `sim/build.rs`, which pairs the two.
    #[must_use]
    pub fn chunk_world_write(&self) -> ChunkWorldWrite {
        self.read(|w| w.resource::<ChunkWorldWrite>().clone())
    }

    /// Loaded column count in [`Self::chunk_world`]. The debug overlay's
    /// `world chunks` line.
    #[must_use]
    pub fn chunk_count(&self) -> usize {
        self.read(|w| w.resource::<ChunkWorld>().len())
    }

    // -----------------------------------------------------------------------
    // Stage 5 residents of `self.ecs`: the driver clock, the pick target, the
    // two interaction predictors, the particle emitter and the version adapter.
    //
    // These accessors exist for the same reason the Stage 2/3/4 ones above do —
    // `crate::app`, `crate::gpu` and `crate::hud` still reach the state through
    // `Sim` — but the resource is the only copy. A read here that cached its
    // result on `Sim` would be the second source of truth the migration exists
    // to delete.
    // -----------------------------------------------------------------------

    /// The driver's frame clock — the process's only one since §4.1(c).
    #[must_use]
    fn clock(&self) -> FrameClock {
        self.read(|w| *w.resource::<FrameClock>())
    }

    /// Mutate the frame clock in place.
    fn clock_mut<R>(&mut self, f: impl FnOnce(&mut FrameClock) -> R) -> R {
        self.write(|w| f(&mut w.resource_mut::<FrameClock>()))
    }

    /// The block the view ray currently points at.
    #[must_use]
    pub fn target(&self) -> Option<RayHit> {
        self.read(|w| w.resource::<RayTarget>().0)
    }

    /// Place a crosshair target directly, for gates that need one without
    /// standing up the whole raycast (which needs a camera, a player pose and a
    /// meshed world).
    ///
    /// `#[cfg(test)]` rather than a runtime flag, deliberately: the fork is
    /// then *absent* from the shipped binary instead of being a branch nothing
    /// takes, which is the shape `docs/` records for `cfg!(test)` early-returns
    /// that silently skip in production.
    #[cfg(test)]
    pub(crate) fn set_ray_target_for_test(&mut self, hit: Option<RayHit>) {
        self.write(|w| w.resource_mut::<RayTarget>().0 = hit);
    }

    /// Whether the use button is currently held down on an item (armed by
    /// [`Self::use_item`], cleared by [`Self::end_use`]).
    ///
    /// Half of vanilla's `Player.isScoping()` (issue #154):
    /// `isUsingItem() && getUseItem().is(Items.SPYGLASS)`
    /// (`Player.java`). This crate has no held-item identity check
    /// — the caller already has `held` (the `ResourceLocation` used for
    /// `set_main_hand_source`), so `app.rs` combines the two rather than this
    /// method reaching into inventory state it does not otherwise need. See
    /// `docs/screen-overlays.md`'s Spyglass section.
    #[must_use]
    pub fn using_item(&self) -> bool {
        self.read(|w| w.resource::<UsingItem>().0)
    }

    /// Overwrite the pick target. Only the per-frame raycast and the two edit
    /// paths that consume a target should call this.
    fn set_target(&mut self, hit: Option<RayHit>) {
        self.write(|w| w.resource_mut::<RayTarget>().0 = hit);
    }

    /// Mutate the particle simulation in place, under the guard.
    ///
    /// For `O(1)` work only — one emission, installing a fixture. Anything that
    /// touches *every* particle must go through
    /// [`Self::with_particles_unlocked`] instead.
    fn particles_mut<R>(&mut self, f: impl FnOnce(&mut Particles) -> R) -> R {
        self.write(|w| f(&mut w.resource_mut::<ParticleSim>().0))
    }

    /// Move the particle simulation **out** of the `World` under a short guard,
    /// run `f` on it with **no guard held at all**, and move it back under a
    /// second short guard.
    ///
    /// # Why the resource leaves the `World`
    ///
    /// Both per-frame particle passes are `O(live particles)` and one of them
    /// (`extract`) calls out to the chunk store once per particle for light. Doing
    /// either under the write guard makes the hold scale with particle volume —
    /// during heavy rain or a mass block break, the two moments the number is
    /// largest — and per `lodestone_ecs::EcsHandle`'s notes that hold is what an
    /// ingest write waits behind, because `SharedState::apply` runs *inline in the
    /// driver task* and blocking it stops the socket being read.
    ///
    /// This is the same move `Self::particle_instances` (owned `Vec`, not a mapped
    /// guard) and `Self::drain_action_queue` (`mem::take`, then send) already make:
    /// get the data out, then work. It is the only shape available here because
    /// `Particles` is not `Clone` and `extract` needs `&mut`.
    ///
    /// # The absence window, and why it is safe
    ///
    /// Between the two guards the `World` has no `ParticleSim`. Nothing can
    /// observe that: `&mut self` makes this exclusive on the driver thread, `f`
    /// runs no schedule, and the only other reader of the resource is
    /// `crate::interact::drive_mining` in `TickSet::Send` — a driver-thread
    /// `GameTick` system, never concurrent with this. The net thread's
    /// `NetIngest` systems live in `lodestone-ecs` and cannot name a
    /// `lodestone-shell` resource at all. A panic inside `f` would leave the
    /// resource missing, which the `expect` below names.
    ///
    /// # Lock order
    ///
    /// `f` is free to take the chunk lock (`tick_particles` does) because it holds
    /// **no** `World` guard while it runs, so the two are never held together and
    /// rule 3's `World → chunks` ordering has nothing to order. That is strictly
    /// safer than the previous nesting, not a relaxation of it — but do not read
    /// the trailing `write` as "chunks then `World`": the chunk guard is a
    /// temporary inside `f` and is gone before it.
    fn with_particles_unlocked<R>(&mut self, f: impl FnOnce(&mut Particles) -> R) -> R {
        let mut sim = self.write(|w| {
            w.remove_resource::<ParticleSim>().expect(
                "ParticleSim is inserted by Sim::build and only ever removed by \
                 with_particles_unlocked, which always puts it back — a missing one means a \
                 previous particle pass panicked",
            )
        });
        let out = f(&mut sim.0);
        self.write(|w| w.insert_resource(sim));
        out
    }

    /// Read the live mining predictor.
    fn mining<R>(&self, f: impl FnOnce(&Mining) -> R) -> R {
        self.read(|w| f(&w.resource::<MiningPredictor>().0))
    }

    /// Read the audio engine, under the guard. See [`AudioEngine`].
    fn audio<R>(&self, f: impl FnOnce(&Option<ShellAudio>) -> R) -> R {
        self.read(|w| f(&w.resource::<AudioEngine>().0))
    }

    /// The mutable form of [`Self::audio`].
    fn audio_mut<R>(&mut self, f: impl FnOnce(&mut Option<ShellAudio>) -> R) -> R {
        self.write(|w| f(&mut w.resource_mut::<AudioEngine>().0))
    }

    /// Read terrain-meshing state (worker pool, dirty set, removal queue, drops).
    fn terrain<R>(&self, f: impl FnOnce(&TerrainMesh) -> R) -> R {
        self.read(|w| f(w.resource::<TerrainMesh>()))
    }

    /// The mutable form of [`Self::terrain`].
    fn terrain_mut<R>(&mut self, f: impl FnOnce(&mut TerrainMesh) -> R) -> R {
        self.write(|w| f(&mut w.resource_mut::<TerrainMesh>()))
    }

    /// Read the chunk store and the terrain state together — the shape every
    /// mesh-scheduling call site needs, and the reason [`TerrainMesh`] is one
    /// resource rather than five.
    fn terrain_and_world<R>(&mut self, f: impl FnOnce(&ChunkWorld, &mut TerrainMesh) -> R) -> R {
        self.write(|w| {
            let store = w.resource::<ChunkWorld>().clone();
            f(&store, &mut w.resource_mut::<TerrainMesh>())
        })
    }

    /// Recompute the two session facts terrain meshing cannot read off the store.
    ///
    /// Called once per frame (and at construction): the connected dimension
    /// changes on a portal trip, and the id-space agreement changes the moment a
    /// session attaches.
    fn refresh_mesh_policy(&mut self) {
        let sky_default = match &self.net {
            Some(net) => {
                // Since #288 the server's own dimension **type** decides this;
                // the level id is only the fallback for a server that sent no
                // `registry_data`. Both come off one `player()` snapshot so they
                // cannot describe two different moments.
                let player = net.shared_handle().get().map(|h| h.player());
                crate::mesher::sky_default_for_dimension(
                    player.as_ref().and_then(|p| p.dimension.as_ref()),
                    player.as_ref().and_then(|p| p.dimension_type.as_ref()),
                )
            }
            // The offline fixture world is the overworld.
            None => lodestone_render::SkyDefault::Full,
        };
        // Publish it for the render thread's *point* samplers — entity light, the
        // rain probe, particles. They cannot compute this themselves: each is a
        // `'static` closure installed once at connect, so it has no per-frame value
        // to read, and calling `ClientHandle::player()` per entity per frame would
        // cost an ECS lock and a snapshot clone each time. This function stays the
        // single place the policy is decided; see `net::SkyDefaultCell`.
        if let Some(net) = &self.net {
            net.shared_sky_default().set(sky_default);
        }
        // The worker pool's classifier was chosen at construction from
        // `!demo_world`, so "the atlas we have" *is* "the id space the pool
        // meshes". A live session with no vanilla atlas (jar-less run, demo-palette
        // fallback) therefore cannot mesh the server's ids, and that is the case
        // this bit exists to make loud rather than silent.
        let id_spaces_agree = if self.net.is_some() {
            self.vanilla_atlas.is_some()
        } else {
            self.vanilla_atlas.is_none()
        };
        // A condition that discards 100% of terrain must not be indistinguishable
        // from an empty world. `TerrainMesh::mesh_column_inner`'s own
        // `tracing::warn!` fires once *per dropped column* — thousands of
        // identical lines at a real render distance, which reads as noise rather
        // than as a cause, and it never names *why* the atlas is missing.
        //
        // Deliberately narrower than `!id_spaces_agree`: the "reverse" arm above
        // (no `net`, a vanilla atlas present) is the ordinary pre-connection
        // window of any windowed session — `Sim::new` loads the vanilla atlas
        // eagerly at construction, before the player has joined anything, so
        // `net` is `None` for real on every title screen. No column is ever
        // meshed in that window (there is no world to dirty one), so it is inert
        // by construction and would make this an always-on warning — exactly as
        // useless as the silence it replaces. The one branch that can actually
        // drop live terrain is a *live* session with no atlas.
        if self.net.is_some() && self.vanilla_atlas.is_none() && !self.warned_id_space_mismatch {
            self.warned_id_space_mismatch = true;
            let reason = self.asset_banner.as_deref().unwrap_or(
                "no reason recorded — no vanilla-load failure was captured to \
                 explain the missing atlas",
            );
            tracing::error!(
                target: "assets",
                "every terrain column is about to be dropped unmeshed for the rest \
                 of this session: the mesh classifier's block-id space does not \
                 match the world it is meshing ({reason})"
            );
            self.status = format!("TERRAIN NOT LOADING — {reason}");
        }
        let policy = MeshPolicy {
            sky_default,
            id_spaces_agree,
        };
        // The live biome registry's ordered entry names (issue #96's
        // follow-up), refreshed the same way and for the same reason as
        // `sky_default` just above: a mesh worker thread only ever sees the
        // jobs on its channel, never a live `Sim`/`NetClient`, so the current
        // value has to be read here and carried along on the `SectionSnapshot`
        // itself (`TerrainMesh::mesh_column`/`mesh_section`'s
        // `with_biome_names` call). `None`/no connection or no registry yet
        // resolves to empty, which `mesher::biome_name_at` already treats as
        // "fall back to `FALLBACK_BIOME_NAMES`" — never as "holder id 0".
        let biome_names: Arc<[&'static str]> = match &self.net {
            Some(net) => Arc::from(net.shared_biome_names().snapshot()),
            None => Arc::from([]),
        };
        self.terrain_mut(|terrain| {
            if terrain.policy != policy {
                terrain.policy = policy;
            }
            if *terrain.biome_names != *biome_names {
                terrain.biome_names = Arc::clone(&biome_names);
            }
        });
    }

    /// Adopt the client's chunk store as ours, once the net thread has published
    /// a handle.
    ///
    /// This is where "there is one chunk store in the process" actually happens,
    /// and it is deferred rather than done in [`Sim::attach_net`] because
    /// `NetClient::connect` publishes its `ClientHandle` asynchronously — there is
    /// no store to adopt until login. Idempotent: once the two are the same `Arc`,
    /// this is a pointer comparison.
    ///
    /// **A session that has offline terrain of its own keeps it.** That is the
    /// `Sim::with_demo_world` fixture attaching a loopback feed: its store is
    /// non-empty, the server sends no chunks, and its uploaded sections must stay
    /// resident (there is a control test for exactly that). A real client session's
    /// store is empty at this point, which is what makes the emptiness test the
    /// right discriminant rather than a proxy for one.
    fn adopt_live_world(&mut self) {
        let Some(net) = &self.net else { return };
        let Some(handle) = net.shared_handle().get().cloned() else {
            return;
        };
        let live = handle.chunk_world();
        let live_write = handle.chunk_world_write();
        let adopt = self.read(|w| {
            let mine = w.resource::<ChunkWorld>();
            !(mine.is_same_store(&live) || !mine.is_empty())
        });
        if !adopt {
            return;
        }
        // Issue #423: adopt the *write* handle alongside the read one, so the
        // store `drive_placement` / `Sim::predict_block` edit is the store the
        // mesher reads — one `Arc`, both resources.
        self.write(|w| {
            w.insert_resource(live_write);
            w.insert_resource(live);
        });
        self.adopted_live_world = true;
    }

    /// The player's bit-exact physics state.
    ///
    /// Panics only on a corrupted `World` — [`spawn_local_player`] inserts the
    /// whole component set eagerly and nothing ever removes it, so a missing
    /// component means someone despawned the local player, which is a bug in
    /// the caller rather than a state a reader should have to handle.
    ///
    /// Returns by value rather than by reference since §4.1(c): the component
    /// lives behind [`Self::ecs`]'s lock, and handing out a `&PlayerState` would
    /// mean handing out a live read guard for a caller to hold for as long as it
    /// liked. `PlayerState` is `Copy`, so this is a register copy and every
    /// existing `sim.player().position`-shaped call site is unchanged.
    #[must_use]
    /// The local player's ECS entity.
    ///
    /// Exists for the `Send + Sync + 'static` render-source closures in
    /// `app/session.rs`, which hold the shared `World` but cannot hold a `&Sim`
    /// to ask [`Self::player`]. Everything inside this crate should prefer
    /// `player()`/`with_player()`.
    #[must_use]
    pub fn local_entity(&self) -> Entity {
        self.local
    }

    pub fn player(&self) -> PlayerState {
        self.read(|w| {
            w.get::<PhysicsState>(self.local)
                .expect("the local player always carries PhysicsState")
                .0
        })
    }

    /// Mutate the player's physics state in place.
    ///
    /// A closure rather than `-> &mut PlayerState`, for the reason on
    /// [`Self::player`]: the write guard must not escape.
    pub fn player_mut<R>(&mut self, f: impl FnOnce(&mut PlayerState) -> R) -> R {
        self.write_local(|w, local| {
            f(&mut w
                .get_mut::<PhysicsState>(local)
                .expect("the local player always carries PhysicsState")
                .0)
        })
    }

    /// Held keys plus accumulated mouse motion. The platform layer
    /// ([`crate::app`]) is the only writer.
    #[must_use]
    pub fn input(&self) -> InputState {
        self.read(|w| w.resource::<RawInput>().0.clone())
    }

    /// Mutate the raw input state in place — the platform layer's only writer.
    pub fn input_mut<R>(&mut self, f: impl FnOnce(&mut InputState) -> R) -> R {
        self.write(|w| f(&mut w.resource_mut::<RawInput>().0))
    }

    /// The physics tuning profile this world is simulated under.
    #[must_use]
    pub fn profile(&self) -> PhysicsProfile {
        self.read(|w| w.resource::<Profile>().0)
    }

    /// Feet position at the start of the most recent physics tick — the camera's
    /// interpolation anchor.
    #[must_use]
    fn prev_position(&self) -> Vec3d {
        self.read(|w| {
            w.get::<PrevPosition>(self.local)
                .expect("the local player always carries PrevPosition")
                .0
        })
    }

    /// Overwrite the camera's interpolation anchor, for a discontinuity the
    /// interpolator must not smear across (a server teleport).
    fn set_prev_position(&mut self, position: Vec3d) {
        self.write_local(|w, local| {
            if let Some(mut prev) = w.get_mut::<PrevPosition>(local) {
                prev.0 = position;
            }
        });
    }

    /// This tick's movement intent, as computed in `TickSet::Input`.
    #[must_use]
    fn movement_intent(&self) -> lodestone_physics::MovementInput {
        self.read(|w| {
            w.get::<MovementIntent>(self.local)
                .expect("the local player always carries MovementIntent")
                .0
        })
    }

    /// Mark the local player dead (server death packet) or alive again (respawn).
    ///
    /// Death is a transient *state*, not the end of the session: the client
    /// library's `RespawnPolicy::Automatic` answers the death packet with a
    /// `Respawn` action, so the shell rides through the death screen rather
    /// than tearing the session down. While it holds, the corpse does not walk
    /// — the intent system forces `MovementInput::NONE` and the movement packet
    /// is withheld until the post-respawn placement teleport lands.
    fn set_dead(&mut self, dead: bool) {
        self.write_local(|w, local| {
            let Ok(mut entity) = w.get_entity_mut(local) else {
                return;
            };
            if dead {
                entity.insert(Dead);
            } else {
                entity.remove::<Dead>();
            }
        });
    }

    /// The stitched vanilla atlas, when the session is rendering the live server
    /// world. `None` on the demo palette. The app threads this into the GPU atlas
    /// so the live world draws real textures instead of procedural colours.
    #[must_use]
    pub fn vanilla_atlas(&self) -> Option<&BlockAtlas> {
        self.vanilla_atlas.as_deref()
    }

    /// The stitched **particle sheet** — flame, smoke, crits, splashes — when
    /// the session loaded real vanilla assets. `None` on the demo palette.
    ///
    /// Exposed so `app.rs` can upload *this exact object* to the GPU
    /// ([`crate::gpu::RenderState::install_particle_sheet_atlas`]) rather than
    /// re-stitching the pack a second time. The CPU-side `(Sheet, frame) -> UV`
    /// table inside [`Particles`] was built from these sprite rects; a second
    /// `AtlasBuilder` run happens to pack identically today, but issue #45 was
    /// *exactly* the bug of UVs addressing a different image than the one bound,
    /// so the identity is made explicit instead of assumed.
    #[must_use]
    pub fn particle_sheet_atlas(&self) -> Option<&lodestone_assets::ParticleAtlas> {
        self.particle_atlas.as_deref()
    }

    /// A one-line note when vanilla assets failed to load and the session fell
    /// back to the demo palette, for the debug overlay. `None` on success.
    #[must_use]
    pub fn asset_banner(&self) -> Option<&str> {
        self.asset_banner.as_deref()
    }

    /// A translation closure over the loaded language table — the exact shape
    /// [`lodestone_game::text::resolve`] consumes. On the demo palette (no table)
    /// it resolves nothing, so a component falls back to its own `fallback`/key.
    /// The table itself stays owned centrally by the `Sim`; only this borrowed
    /// closure is handed to the pure projection helpers, matching how vanilla
    /// resolves components at the render boundary.
    pub(crate) fn translator(&self) -> Translator<'_> {
        match &self.language {
            Some(lang) => Box::new(lang.translator()),
            None => Box::new(|_: &str| None),
        }
    }

    /// Lower a server-authored component's `translate` nodes into literals
    /// against the loaded language table, preserving styling. Used at the read
    /// boundary for the title/action-bar and at ingest for chat, so raw keys
    /// like `entity.minecraft.spider` never reach the HUD.
    fn resolve_text(&self, text: &lodestone_model::Text) -> lodestone_model::Text {
        lodestone_game::text::resolve(text, self.translator().as_ref())
    }

    /// The chunk store as a `'static` [`CollisionSource`].
    ///
    /// Cheap: an `Arc` refcount bump, with the read lock taken inside
    /// `with_view` for exactly as long as the collision resolve. Before Stage 4
    /// this was an `O(loaded columns)` clone of the whole offline world, cached on
    /// `Sim` and invalidated by hand — see [`ChunkWorldCollision`] for what that
    /// deleted.
    fn chunk_collision(&self) -> Arc<ChunkWorldCollision> {
        Arc::new(ChunkWorldCollision(self.chunk_world()))
    }

}

mod actions;
mod net_apply;
mod audio;
mod camera;
mod meshing;

// Seams 9-13, landed together (seam 8 was the `audio` field -> `AudioEngine`
// resource move, a field dissolution; `docs/sim-dissolution.md` numbers both
// axes in one sequence). Bare `mod` lines like the five above, and with
// no re-exports of their own: every item they carry is an `impl Sim` method, and
// a method call resolves through the `Sim` type regardless of which file defines
// it -- the same reason `sim/actions.rs` needed none. `sim/collide.rs` also
// carries one private `const`, read only inside that file.
//
// The root deliberately keeps the whole lock-scoped accessor layer (`read`,
// `write`, `write_local`, the per-resource accessors, `refresh_mesh_policy`,
// `adopt_live_world`, the local-player/physics-intent reads, `translator`/
// `resolve_text`) plus the two `CollisionSource` adapters and `chunk_collision`.
// Not by omission: those are what every seam file below reaches through, and a
// parent's private item is visible to every descendant while a child's is
// invisible to its siblings. Leaving them here widened nothing; moving them out
// would have made `read`/`write` `pub(crate)` and put this crate's whole lock
// discipline -- see `EcsHandle`'s three rules -- on the crate-internal surface.
mod build;
mod session;
mod collide;
mod step;
mod render_sources;
// The dimension cluster: `dimension`/`sky_mode` (the one read of "which dimension
// are we in"), the portal-transition effect's tick and lerp, and the
// dimension-change reset. Same bare-`mod` shape as the seams above — every item
// is an `impl Sim` method plus two private constants and one private free
// function, so nothing needs re-exporting.
mod dimension;

// `sim/tests.rs`'s `dirty_sections_for_blocks(...)` calls cross the new
// sibling boundary; this private `use` re-enters its `use super::*;` glob
// the same way `placement::is_air_state` already does. No `pub use`: nothing
// outside this crate names it. `#[cfg(test)]`, unlike the other seam
// re-exports here: every non-test caller (`meshing::remesh_changed_blocks`)
// already lives inside `sim::meshing` itself and needs no re-export, so a
// plain `use` is dead code — and therefore a warning, not just noise — in a
// `--lib`-only build.
#[cfg(test)]
use meshing::dirty_sections_for_blocks;

// `app.rs` names this by its full path (`crate::sim::fog_for_render_distance`),
// and `app.rs` is neither `sim` nor a descendant of it, so a private `use`
// here (visible only to `sim` and its descendants, the same rule
// `sim/camera.rs`'s doc explains) would not reach it — needs `pub(crate)`,
// matching the item's own visibility before it moved. Not `pub use`: nothing
// outside this crate names it.
pub(crate) use camera::fog_for_render_distance;

// Same reasoning as `fog_for_render_distance` just above: `crate::interact::
// drive_placement` names this by full path, and it is a sibling of `sim`, not
// a descendant, so the plain `use` inside `sim/audio.rs`'s own `impl Sim`
// method is not enough on its own — needs `pub(crate)` here too.
pub(crate) use audio::block_sound_seed;

#[cfg(test)]
mod tests;
