//! The windowless, GPU-less **simulation**: the generated world, the player
//! driven by the real physics engine, the off-thread mesh scheduler, and the
//! optional live connection. Keeping this free of winit and wgpu is what lets
//! the interesting logic — stepping, meshing, camera derivation — be unit tested
//! headlessly, with the windowed layer in [`crate::app`] staying a thin driver.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World as EcsWorld;
use lodestone_assets::{Language, ResourceLocation};
use lodestone_client::{BlockPos, ClientAction, Hand, OpenMenuSnapshot, Rotation};
use lodestone_controller::{ControllerPlugin, InputState, RawInput, apply_look};
use lodestone_ecs::entity::{EntityKind, MinecraftEntityId, Position};
use lodestone_ecs::player::{
    ActionQueue, CollisionSource, Dead, Egress, Flying, LocalPlayerPlugin, MovementIntent,
    NearbyEntities, PhysicsState, PlayerCollision, PrevPosition, Profile, SelectedSlot,
    Submersion, reset_local_player, spawn_local_player,
};
use lodestone_ecs::session::{
    ActionBarOverlay, HudEffects, Phase, RespawnCount, ServerEntityId, SessionChat,
    SessionHudPlugin, TitleOverlay, Vitals, Xp, insert_hud_components,
};
use lodestone_ecs::{
    ChunkWorld, CorePlugin, EcsHandle, Extract, FrameClock, GameTick, Update, VersionData,
};
pub use lodestone_ecs::SessionPhase;
use lodestone_entity::pose::EntityPose;
use lodestone_game::menu::Menu;
use lodestone_game::mining::{BreakInputs, Mining};
use lodestone_game::placement::{
    OrientationKind, Placement, PlacementWorld, UseOnContext, UseOnDecision,
};
use lodestone_model::event::EquipmentSlot;
use lodestone_model::{BlockFace, EntityInteraction, Vec3f};
use lodestone_particle::emit as particle_emit;
use lodestone_physics::{
    CollisionView, EntityDimensions, FluidState, NearbyEntity, PhysicsProfile, PlayerState, Vec3d,
};
use lodestone_render::{AnimInput, BlockAtlas, Camera};
use lodestone_world::{ChunkPos, World};

use crate::audio::ShellAudio;
use crate::blocks::id;
use crate::camera_rig::{build_camera, third_person_camera};
use crate::chat::compose_chat_action;
use crate::collision::{LiveCollision, WorldCollision};
use crate::config::{Config, Mode};
use crate::entities::EntityDraw;
use crate::gpu::ThirdPersonBodyState;
use crate::hud::{DebugStats, process_rss_bytes};
use crate::interact::{
    Attacking, EntityRayTarget, InteractPlugin, MiningPredictor, NetHandle, ParticleSim,
    PlacementPredictor, RayTarget,
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
/// Vanilla's `DEFAULT_ENTITY_INTERACTION_RANGE` (`Player.java:134`) — the reach
/// for attacking/interacting with an entity, distinct from and shorter than
/// [`REACH`] (block interaction range, `Player.java:133`'s `4.5`). Creative
/// adds a further `+2.0` modifier (`Player.java:150`) that this shell does not
/// track, so every session uses the unmodified survival default.
const ENTITY_REACH: f64 = 3.0;
/// Number of hotbar slots (vanilla is a fixed 9).
const HOTBAR_SLOTS: usize = 9;

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
pub(crate) fn bare_handed_tool_mining(entry: lodestone_model::BlockHardness) -> lodestone_model::ToolMining {
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
pub(crate) fn tool_mining_item(held: &lodestone_game::item::ItemStack) -> lodestone_model::ItemStack {
    let tool = match held.components().get_str(lodestone_game::item::TOOL_COMPONENT) {
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

/// Distance fog for a render distance of `render_distance` chunks.
///
/// Fog is what hides the render-distance edge — without it the loaded world
/// ends in a hard wall of geometry against the sky. It therefore has to track
/// the *configured* distance rather than a fixed default, or raising
/// `--render-distance` would fog out the very chunks it just loaded, making a
/// larger view look worse than a smaller one.
///
/// Free-standing so the relationship is testable without generating a world:
/// [`Sim::new`] at render distance 32 builds thousands of sections, which is a
/// minute of work to check a multiplication.
pub(crate) fn fog_for_render_distance(render_distance: u32) -> lodestone_render::fog::FogSettings {
    lodestone_render::fog::FogSettings::for_view_distance(
        crate::gpu::SKY_COLOR,
        render_distance as f32 * 16.0,
        crate::gpu::FOG_START_FRACTION,
    )
}

/// Short, near-eye distance fog for an eye submerged in water.
///
/// Vanilla water vision is only a few chunks, so the far edge is capped short
/// (and never past where chunks actually stop) and the ramp starts at the eye
/// (`start_fraction` 0) so terrain dissolves close rather than at the sky edge.
/// The colour is the default ocean underwater fog — the per-biome water fog
/// colour is not yet reachable from the shell, so this is the documented
/// fallback rather than a biome-correct tint.
fn water_fog(render_distance: u32) -> lodestone_render::fog::FogSettings {
    let far = 32.0_f32.min(render_distance as f32 * 16.0);
    lodestone_render::fog::FogSettings::for_view_distance([0.05, 0.19, 0.44], far, 0.0)
}

/// Near-opaque, few-block distance fog for an eye submerged in lava: submerging
/// in lava blinds fast in vanilla, so the range is very short and the colour a
/// hot orange.
fn lava_fog() -> lodestone_render::fog::FogSettings {
    lodestone_render::fog::FogSettings::for_view_distance([0.6, 0.1, 0.0], 3.0, 0.0)
}

/// Which section meshes a set of changed cells invalidates.
///
/// A section's geometry is a function of its whole 3×3×3 neighbourhood (face
/// culling reads the 6 face-adjacent sections; AO samples the 3 cells around
/// every vertex corner, which reach across section *edges and corners* too), so
/// a changed cell dirties its own section **plus** every neighbour section it
/// physically touches — and no others. A cell at local x=15 touches the +x
/// neighbour; an interior cell touches nothing else. Skipping the neighbour is
/// the defect that leaves a stale face at a chunk border while mining on a live
/// server; dirtying all 27 unconditionally pays a 27× re-mesh for every redstone
/// tick. Hence the per-axis filter rather than either extreme.
///
/// Coordinates are **section-relative** (`0..=15`), matching the wire form of
/// `SECTION_BLOCKS_UPDATE`, and the result is in absolute section coordinates.
fn dirty_sections_for_blocks(
    sx: i32,
    sy: i32,
    sz: i32,
    blocks: &[[u8; 3]],
) -> BTreeSet<(i32, i32, i32)> {
    let mut dirty: BTreeSet<(i32, i32, i32)> = BTreeSet::new();
    for &[bx, by, bz] in blocks {
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if (dx == -1 && bx != 0) || (dx == 1 && bx != 15) {
                        continue;
                    }
                    if (dy == -1 && by != 0) || (dy == 1 && by != 15) {
                        continue;
                    }
                    if (dz == -1 && bz != 0) || (dz == 1 && bz != 15) {
                        continue;
                    }
                    dirty.insert((sx + dx, sy + dy, sz + dz));
                }
            }
        }
        // Every further cell can only add sections already reachable from a full
        // 3×3×3, so once all 27 are queued there is nothing left to find. This
        // is what bounds a 4096-cell `SECTION_BLOCKS_UPDATE` to 27 re-meshes.
        if dirty.len() == 27 {
            break;
        }
    }
    dirty
}

/// A trivial [`PlacementWorld`] for the live path. The shell cannot classify
/// blocks (no version-free replaceable/interactable seam is exposed by
/// `lodestone-model`; see the report), and it does not need to: the server is
/// authoritative and re-runs the place-vs-interact decision itself, while
/// [`Placement::use_on`] returns the `use_item_on` action to send in every
/// branch. The shell sends that action unconditionally and lets the server
/// decide, so the local classification never changes what goes on the wire.
struct ServerAuthoritativeWorld;

impl PlacementWorld for ServerAuthoritativeWorld {
    fn is_replaceable(&self, _pos: BlockPos) -> bool {
        false
    }

    fn is_interactable(&self, _pos: BlockPos) -> bool {
        false
    }
}

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

/// The block-local hit position at the centre of the struck face, in the `0..1`
/// coordinates `use_item_on` expects. The shell's raycast reports only the block
/// and its face normal, not the exact sub-block hit point; the face centre is
/// exact for full-cube placement and the server re-derives fine detail anyway.
fn face_center_cursor(normal: [i32; 3]) -> Vec3f {
    // On the struck face's normal axis the hit sits on the block boundary (1.0
    // for a positive normal, 0.0 for a negative one); the two in-plane axes sit
    // at the face centre.
    let coord = |c: i32| -> f32 {
        match c.signum() {
            1 => 1.0,
            -1 => 0.0,
            _ => 0.5,
        }
    };
    Vec3f::new(coord(normal[0]), coord(normal[1]), coord(normal[2]))
}

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
        f.debug_struct("LiveCollisionSource").finish_non_exhaustive()
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
    /// The stitched vanilla atlas for the live world, or `None` when running on
    /// the demo palette. Its presence is the single discriminant for "render the
    /// live server world with the vanilla atlas" vs "mesh the demo world": the
    /// two use disjoint block-id spaces and must never be meshed with the wrong
    /// classifier.
    vanilla_atlas: Option<Arc<BlockAtlas>>,
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
    /// Test seam (normal play: always `true`): when `false`, death is treated as
    /// the terminal `SessionPhase::Ended` it used to be, reproducing the "stuck
    /// on the death screen forever" bug as the live gate's negative control. Never
    /// flipped in real play.
    pub recover_from_death: bool,
    /// Live audio, or `None` when disabled (no asset root, no device — see
    /// [`ShellAudio::from_env`]). The whole audio path is `if let Some`, so a
    /// disabled engine is simply silent, never a crash.
    audio: Option<ShellAudio>,
    /// The camera mode: `false` is first person (the only mode that existed
    /// before this field), `true` is third person. There is deliberately no
    /// richer enum — [`RenderState::set_third_person_body_source`]'s own doc
    /// says the closure's `None`/`Some` *is* the camera-mode toggle, and this
    /// bool is the one thing that decides which of the two it returns each
    /// frame (see [`Self::third_person_body_state`] and
    /// [`Self::render_camera`]).
    ///
    /// [`RenderState::set_third_person_body_source`]: crate::gpu::RenderState::set_third_person_body_source
    third_person: bool,
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
}

impl Sim {
    /// Build the simulation for a **real client session**: no offline world.
    ///
    /// The client renders exactly one world — the server's. Nothing is generated,
    /// meshed or uploaded here; terrain appears only as the live session's chunks
    /// arrive (`mark_column_dirty`), and the player's position comes from the
    /// login teleport.
    ///
    /// # Why there is no offline world any more
    ///
    /// There used to be one, generated unconditionally and meshed whenever the
    /// vanilla atlas was absent — which was *every windowed run that did not pass
    /// `--live`*, because the atlas choice was keyed off `config.connect_in_window`
    /// (see the report). Joining a server from the main menu then left the demo
    /// world resident and drawn around the origin while the player stood at the
    /// server's real spawn, with the live columns never meshed at all (the live
    /// branch of `mark_column_dirty` is gated on the vanilla atlas, which that
    /// session did not have). Two candidate worlds, one of them wrong, is a defect
    /// class rather than a bug: the fix is that the client only ever has one.
    ///
    /// `Mode::Headless` is the single remaining exception and delegates to
    /// [`Sim::with_demo_world`]: it is the offline, GPU-only evidence path
    /// (`app::run_headless` renders one offscreen frame and *fails* below 5%
    /// terrain coverage), so it needs a world that exists without a server.
    #[must_use]
    pub fn new(config: Config) -> Self {
        if config.mode == Mode::Headless {
            return Self::with_demo_world(config);
        }
        Self::build(config, false)
    }

    /// Build the simulation **around the offline demo world** — a fixture, not a
    /// product path.
    ///
    /// Generates `worldgen`'s world on the demo palette and schedules every
    /// non-empty section, i.e. exactly what [`Sim::new`] used to do for any run
    /// without `--live`. Two callers, both deliberate:
    ///
    /// * every hermetic gate that needs terrain without a server — this crate's
    ///   own unit tests (via `test_config`, which is `Mode::Headless`) and
    ///   `tests/break_particles_pixels.rs`;
    /// * `--headless`, through [`Sim::new`]'s `Mode::Headless` delegation.
    ///
    /// **Do not call this from an interactive path.** The demo palette and the
    /// vanilla registry are disjoint block-id spaces, so a session holding this
    /// world cannot mesh a server's chunks (see `mark_column_dirty`).
    #[must_use]
    pub fn with_demo_world(config: Config) -> Self {
        Self::build(config, true)
    }

    /// The shared constructor. `demo_world` picks between the two mutually
    /// exclusive block-id worlds *and* whether any offline terrain exists at all;
    /// the two must agree, which is why this is one function and not two.
    fn build(config: Config, demo_world: bool) -> Self {
        let (world, feet) = if demo_world {
            let radius = (config.render_distance as i32).clamp(1, MAX_WORLD_RADIUS);
            (worldgen::generate(radius), worldgen::spawn_feet())
        } else {
            (World::new(), PRE_SESSION_FEET)
        };

        let mut player = PlayerState::at(Vec3d::new(feet[0], feet[1], feet[2]), 180.0);
        player.pitch = 10.0;

        let workers = std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).max(1))
            .unwrap_or(2);

        // Pick the block-id world once. A client session wants the vanilla atlas
        // (the server's world streams vanilla ids); the demo-world fixture uses
        // the demo palette. A vanilla load failure falls back to the demo palette
        // and records a banner — see `mark_column_dirty`, which counts and logs
        // the live chunks such a session cannot mesh instead of dropping them
        // silently.
        let resources = BlockResources::load(!demo_world);
        let render_live = resources.vanilla_atlas.is_some();
        let mut terrain = TerrainMesh::new(MeshScheduler::new(workers, resources.classifier));
        let chunk_world = ChunkWorld::new(world);

        // `BlockResources::load(false)` always yields the demo palette, so this
        // never schedules demo ids under the vanilla atlas.
        debug_assert!(
            !(demo_world && render_live),
            "the demo world must never be meshed with the vanilla classifier"
        );
        if demo_world {
            for (cx, cz) in chunk_world
                .read()
                .iter()
                .map(|(pos, _)| (pos.x, pos.z))
                .collect::<Vec<_>>()
            {
                terrain.mesh_column(&chunk_world, cx, cz);
            }
        }

        let status = if render_live {
            "live world (vanilla atlas)".to_string()
        } else if let Some(banner) = &resources.banner {
            format!("demo palette — {banner}")
        } else {
            "local world".to_string()
        };
        let mut stats = DebugStats {
            status: status.clone(),
            ..Default::default()
        };
        stats.chunk_count = chunk_world.len();

        // The particle sprite table is indexed by whatever id the emitter will
        // be handed, so it must be built from the *same* palette the world uses.
        // With the vanilla atlas that is a baked-model state id; on the demo
        // world it is the shell's own small block table. Binding the wrong one
        // does not fail — it draws correctly-shaped debris in some other block's
        // colours, which reads as an art bug rather than a wiring bug.
        // Sheet particles (smoke, flame, crits, splashes) live in their own
        // stitch — they are unreachable from any blockstate, so the block atlas
        // above never contains them. Without this the emitter still runs and
        // every sheet quad is counted into `ParticleFrame::unresolved` rather
        // than drawn, which is why the HUD reports `0/0+Nunres` on a jar-less
        // run instead of silently showing nothing.
        let particles = match resources.vanilla_atlas.as_ref() {
            Some(atlas) => Particles::new(atlas.models()),
            None => Particles::with_demo_palette(&crate::blocks::build_atlas().uv_table),
        }
        .with_particle_atlas(resources.particle_atlas.as_deref());

        // Per-block-state data (hardness, for the mining predictor) comes from
        // whichever version family the registry has compiled in for the
        // configured protocol. Resolved once here rather than per dig tick: the
        // lookup itself is a table index, but minting a boxed adapter 20× a
        // second to perform it would not be.
        let version_data = lodestone_registry::adapter_for_protocol(config.protocol);

        // The local player's `World`. Built through an `App` because `Plugin::build`
        // is the only way to register schedules and systems, then the `World` is
        // taken and the `App` dropped — azalea's own shape
        // (`azalea-client/src/client.rs:143`), and `crate::entities` does the same,
        // which is why nothing here ever calls `App::update`.
        //
        // `LocalPlayerPlugin` owns `TickSet::Physics`; `ControllerPlugin` owns
        // `TickSet::Input` and `TickSet::Send`; `SessionHudPlugin` owns
        // `TickSet::Animate` (ageing the title/action-bar/effect overlays at the
        // fixed 20 Hz their durations are counted in). All three are needed for a
        // player that is driven, reported *and* drawn, and they are separate
        // plugins so a harness can take one without the others.
        let mut app = lodestone_ecs::app::App::new();
        app.add_plugins((
            CorePlugin,
            LocalPlayerPlugin,
            ControllerPlugin,
            SessionHudPlugin,
            // §4.1(c). `IngestPlugin` + `SessionPlugin` are the *net thread's*
            // folds — the systems `lodestone_client::state::SharedState` runs — and
            // they are installed here because there is now one `World` and this is
            // it. Exactly once: `SessionPlugin` guards the shared
            // `drain_ingest_queue` with `is_plugin_added`, because `add_systems`
            // does not deduplicate and a second copy blanks every batch the first
            // one filled (Stage 3 shipped that as a total ingest blackout).
            lodestone_ecs::ingest::IngestPlugin,
            lodestone_ecs::SessionPlugin,
            // §4.1(c). The render-side entity interpolation, which used to own a
            // second `World` and therefore a second 20 Hz accumulator.
            crate::entities::EntityInterpPlugin,
            // Stage 4: the chunk store and the terrain-mesh queues become
            // resources, and `heal_dirty_columns` becomes an `Update` system in
            // `FrameSet::Terrain`.
            TerrainPlugin,
            // Stage 5: the pick target, the two interaction predictors and the
            // particle emitter become resources, and the sprint edge and the
            // hold-to-mine loop become `TickSet::Send` systems. Added *after*
            // `ControllerPlugin` because it asserts that plugin is present rather
            // than adding it itself — `add_systems` does not deduplicate.
            InteractPlugin,
        ));
        let mut ecs = std::mem::take(app.world_mut());
        ecs.insert_resource(Profile(PhysicsProfile::mc_1_21()));
        // Stage 5. `ParticleSim` cannot come from `InteractPlugin`: like the mesh
        // worker pool, the emitter has to be built with the sprite table for
        // whichever block-id space this session's world holds.
        ecs.insert_resource(ParticleSim(particles));
        ecs.insert_resource(VersionData(version_data));
        // `FrameClock` and `WorldTime` come from `CorePlugin` now (§4.1(c) retired
        // the guard that refused to insert them), so there is nothing to seed here.
        // `TerrainPlugin` inserts a *default* (empty) store; this replaces it with
        // the one this session actually meshes. The worker pool cannot come from a
        // plugin at all: it has to be built with the classifier for whichever
        // block-id space that store holds.
        ecs.insert_resource(chunk_world);
        ecs.insert_resource(terrain);
        // Physics-walk is the default everywhere, including live: the shell
        // collides against the live client-owned world (see `LiveCollision` /
        // `Sim::tick_collision`), so the player stands on the server's ground.
        // While a column is still streaming in, `PlayerCollision::Pending` holds
        // the player in place rather than letting them fall.
        let local = spawn_local_player(&mut ecs, player);
        // Stage 3's session/HUD half goes on the same entity. Separate from
        // `spawn_local_player` because the two component sets belong to different
        // plugins, and a plugin a harness leaves out must not leave a component
        // its systems never look at behind.
        insert_hud_components(&mut ecs, local);
        // §4.1(c): the shared-fold half goes on the *same* entity too, instead of
        // `lodestone_client::state::SharedState::default` spawning a second
        // `LocalPlayer` in a `World` of its own. This is the entity
        // `attach_net` names to `ClientBuilder::ecs`.
        lodestone_ecs::session::insert_session_components(&mut ecs, local);

        let mut sim = Self {
            config,
            stats,
            ecs: std::sync::Arc::new(lodestone_ecs::parking_lot::RwLock::new(ecs)),
            local,
            net: None,
            adopted_live_world: false,
            status,
            vanilla_atlas: resources.vanilla_atlas,
            language: resources.language,
            teleport_count: 0,
            collide_against_live_world: true,
            asset_banner: resources.banner,
            recover_from_death: true,
            audio: ShellAudio::from_env(),
            third_person: false,
            body_pose: EntityPose::new(feet[0], feet[2], player.yaw, false),
            // Seeded from the spawn pose so the very first frame does not ease up
            // from zero — vanilla's `Camera` is likewise aligned before its first
            // tick, not zero-initialised.
            eye_height_smoother: crate::camera_rig::EyeHeightSmoother::new(player.eye_height),
        };
        sim.refresh_mesh_policy();
        sim
    }

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

    /// The **one** chunk store this session meshes, collides against and edits.
    ///
    /// A handle, cheap to clone, onto the same `lodestone_world::World` the net
    /// thread writes decoded columns into once
    /// [`adopt_live_world`](Self::adopt_live_world) has run. Before Stage 4 there
    /// were two of these — `Sim`'s offline one and the client's live one — and
    /// every read site branched on which it meant.
    #[must_use]
    pub fn chunk_world(&self) -> ChunkWorld {
        self.read(|w| w.resource::<ChunkWorld>().clone())
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
            Some(net) => crate::mesher::sky_default_for_dimension(
                net.shared_handle()
                    .get()
                    .and_then(|h| h.player().dimension)
                    .as_ref(),
            ),
            // The offline fixture world is the overworld.
            None => lodestone_render::SkyDefault::Full,
        };
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
        let policy = MeshPolicy {
            sky_default,
            id_spaces_agree,
        };
        self.terrain_mut(|terrain| {
            if terrain.policy != policy {
                terrain.policy = policy;
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
        let adopt = self.read(|w| {
            let mine = w.resource::<ChunkWorld>();
            !(mine.is_same_store(&live) || !mine.is_empty())
        });
        if !adopt {
            return;
        }
        self.write(|w| w.insert_resource(live));
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

    /// Open a live connection to `host:port` and attach it, threading this `Sim`'s
    /// one `World` into the client so ingest folds where these systems read.
    ///
    /// This is the §4.1(c) wiring, and it is a `Sim` method rather than three lines
    /// at every call site because getting it wrong is silent: a `NetClient` built
    /// without the handle gets a `World` of its own, the session fold lands in it,
    /// and every HUD read here returns an empty default. Prefer this over
    /// [`Self::attach_net`], which exists for a client that has no connection to
    /// share (the loopback test double).
    pub fn connect(&mut self, host: String, port: u16, protocol: i32) {
        let net = NetClient::connect(
            host,
            port,
            protocol,
            Some((Arc::clone(&self.ecs), self.local)),
        );
        self.attach_net(net);
    }

    /// Attach a live connection whose updates are polled each frame.
    // `mut` is used only by the `#[cfg(test)]` `bind_session` below.
    #[cfg_attr(not(test), allow(unused_mut))]
    pub fn attach_net(&mut self, mut net: NetClient) {
        // The `World`-sharing half of §4.1(c) for a test double, which has no
        // `ClientBuilder` to hand the handle to. Production goes through
        // [`Self::connect`], where the real client adopts it at build time.
        #[cfg(test)]
        net.bind_session(Arc::clone(&self.ecs), self.local);
        // Stage 5: the `Send + Sync` half of the connection goes into the `World`
        // so the `TickSet::Send` systems can read the client. Not a second copy —
        // it is the same `Arc<OnceLock<_>>` the net thread publishes into, and
        // `NetClient` itself can never be a resource because its `mpsc::Receiver`
        // is `!Sync`. See `crate::interact::NetHandle`.
        let handle = net.shared_handle();
        self.write(|w| w.insert_resource(NetHandle(Some(handle))));
        self.net = Some(net);
        self.status = "connecting…".into();
        self.set_phase(SessionPhase::Connecting);
        // The store itself is adopted later, in `poll_net`: `NetClient::connect`
        // publishes its `ClientHandle` from the net thread, so there is nothing to
        // adopt until login. The *policy* changes immediately, though — a session
        // with no vanilla atlas cannot mesh the server's ids and must start
        // counting that rather than silently rendering nothing.
        self.refresh_mesh_policy();
    }

    /// Tear down whatever live session is attached and reset every piece of
    /// per-session state, so a later [`Sim::attach_net`] behaves exactly like
    /// the very first connection rather than starting with leftovers from
    /// the one that just ended.
    ///
    /// Driven by the pause menu's Quit to Title
    /// (`crate::menu::nav::MenuAction::QuitToTitle`); `UiState` has already
    /// left for the main menu by the time this runs, independent of this
    /// teardown's own success.
    ///
    /// # What this resets
    ///
    /// - **The connection**: `net` is dropped — `NetClient`'s `Drop` signals
    ///   its background thread to stop and joins it (see `net.rs`), so this
    ///   cannot leak a thread — and [`Self::phase`] returns to
    ///   [`SessionPhase::LocalOnly`]. Left at a stale
    ///   [`SessionPhase::Ended`] this would otherwise immediately re-fail the
    ///   *new* main-menu screen the moment
    ///   `crate::app::WindowApp::drive_ui_from_session` next runs.
    /// - **Every read-model [`Sim::poll_net`] feeds**: the chat log and the
    ///   teleport-count diagnostic directly, and everything else via
    ///   `insert_hud_components` — the status-effect overlay, title/subtitle,
    ///   action bar, health, food, experience, respawn count, the session phase,
    ///   and the server-assigned entity id (stale, not merely wrong: left in
    ///   place it would misattribute the *next* session's
    ///   `EffectApplied`/`EffectRemoved` to whichever entity the new server
    ///   happens to assign that same id to first).
    /// - **The shared-fold set — the tab list, scoreboard, boss bars, menus, and
    ///   (since the vitals collapse) health/food/saturation, experience, the
    ///   server entity id, game mode, dimension and liveness** — via
    ///   `insert_session_components`, the same one-call reset
    ///   `insert_hud_components` is for the driver half.
    ///
    ///   This bullet used to say those needed no clearing at all, "and that is
    ///   Stage 3 working rather than an omission: they are components in the
    ///   *client's* `World`, so dropping `net` above drops the only route to
    ///   them". **That went stale the moment §4.1(c) merged the two `World`s** —
    ///   it is one `World` and one entity now, `Sim::sidebar`/`player_rows`/
    ///   `boss_bars` read `self.local` directly, and dropping `net` drops no route
    ///   to anything. Left as written, the previous server's sidebar and tab list
    ///   really did survive a quit-to-title. A stale-but-true-when-written note
    ///   about state that "cannot" leak is exactly the shape `CLAUDE.md`'s rule 2
    ///   warns about.
    /// - **In-flight prediction state**: `mining` and `placement` are
    ///   replaced wholesale rather than merely stopped — both track a
    ///   monotonic sequence counter with no public reset, and `Mining` also
    ///   tracks a post-break cooldown `stop()` alone does not clear (see the
    ///   report). `attacking` clears, and the last-sent player-input/sprint
    ///   edge trackers reset to their [`Sim::new`] values so the next
    ///   session's first packet is not suppressed as a redundant resend.
    /// - **Meshing**: mesh jobs still in flight for the old server's chunks
    ///   are flushed and discarded (not left to land silently in whatever
    ///   session comes next), `dirty_columns`/`mesh_drops` clear, and every
    ///   section this session ever uploaded is queued into
    ///   `pending_removals` — the app's existing per-frame drain — per
    ///   [`Self::uploaded_sections`]'s doc.
    /// - **The player**: returned to the same spawn the constructor used
    ///   ([`PRE_SESSION_FEET`] for a real client, the demo surface for the
    ///   [`Sim::with_demo_world`] fixture), and free-fly clears. A live
    ///   reconnect immediately overrides this with the new server's login
    ///   teleport; leaving the old server's coordinates in place would
    ///   otherwise show the title screen's frozen player at wherever they
    ///   happened to quit.
    /// - **`status`**: recomputed with the same rule [`Sim::new`] uses, so
    ///   the debug overlay reads "local world"/"live world (vanilla atlas)"
    ///   again instead of whatever the old session last wrote there (e.g.
    ///   "connecting…" or a disconnect reason).
    ///
    /// # What this deliberately leaves alone
    ///
    /// GPU pipelines/buffers and loaded assets (`vanilla_atlas`, `language`,
    /// `version_data`) are config- or asset-derived, not session state —
    /// `Sim::new` never reloads them on `attach_net` either, so a teardown
    /// should not either. `particles` is intentionally untouched: every
    /// particle already expires within a couple of seconds on its own, and
    /// nothing drives its `tick`/`extract` once the title screen stops
    /// calling into the render path, so a leftover burst is inert rather
    /// than a bug. See the report on this change for what is genuinely
    /// unverified rather than merely reasoned about.
    pub fn end_session(&mut self) {
        // Drop first: `NetClient::drop` signals its net thread and joins it,
        // so nothing below can race a still-running poll against state this
        // method is about to reset out from under it.
        self.net = None;

        self.teleport_count = 0;

        // §4.1(c): the entity interpolator no longer owns a `World` to throw away,
        // so its tracks are cleared explicitly. Replacing the whole interpolator
        // used to *also* zero that `World`'s private `TickAccum` while leaving the
        // player's accumulator alone — a quit-to-title re-phased the two clocks
        // arbitrarily on top of the clamp divergence. There is one accumulator now
        // and it is reset on the next line, deliberately rather than incidentally.
        self.write(|w| {
            crate::entities::reset_entity_tracks(w);
            w.resource_mut::<FrameClock>().reset_accumulator();
        });

        // Stage 5: all four are resources now, and `chat_log` moved out of this
        // list entirely — it is a `SessionChat` component that
        // `insert_hud_components` below puts back with the rest of the set, which
        // is what stops it being the field a later addition forgets.
        self.write(|w| {
            w.insert_resource(MiningPredictor(Mining::new()));
            w.insert_resource(PlacementPredictor(Placement::new()));
            w.insert_resource(Attacking(false));
            w.insert_resource(NetHandle(None));
        });

        // Flush and discard mesh jobs still in flight for the old server's
        // chunks rather than letting them complete later and land silently
        // in whatever session comes next; clear the dirty set and the drop
        // counter; and queue every section this session ever uploaded for removal
        // through the app's ordinary drain path.
        self.terrain_mut(TerrainMesh::end_session);

        // Release the server's chunk store. A client session adopted the client's
        // `World` at login (`adopt_live_world`); handing it back an empty store is
        // both the teardown *and* what makes a later `attach_net` adopt again —
        // adoption is gated on our store being empty. A `with_demo_world` fixture
        // never adopted, so its terrain is not the live store and survives, which
        // is the behaviour `resident_after_connect`'s control asserts.
        if std::mem::take(&mut self.adopted_live_world) {
            self.write(|w| w.insert_resource(ChunkWorld::default()));
        }

        // Back to whatever spawn this `Sim` was built around — the demo world's
        // surface for the fixture, the pre-session placeholder for a real client
        // (which has no offline world to return to).
        let feet = if self.chunk_world().is_empty() {
            PRE_SESSION_FEET
        } else {
            worldgen::spawn_feet()
        };
        let mut player = PlayerState::at(Vec3d::new(feet[0], feet[1], feet[2]), 180.0);
        player.pitch = 10.0;
        // One call rather than a field-by-field reset: `reset_local_player` puts
        // the whole component set back to what `spawn_local_player` produces —
        // pose, camera anchor, submersion, intent, free-fly, hotbar slot, the two
        // wire edge-trackers (to their `Sim::new` values, so the next session's
        // first packet is not suppressed as a redundant resend), and the `Dead`
        // marker. Keeping that list in one place is what stops a component added
        // later from being silently missed here.
        let local = self.local;
        self.write(|w| reset_local_player(w, local, player));
        // The Stage-3 half of the same reset, in two calls because the set is in
        // two halves. `insert_hud_components` writes the driver half back to its
        // just-spawned value (phase, the two overlays, the effect stack, the
        // respawn counter, the chat log); `insert_session_components` does the
        // shared half (scoreboard, tab list, boss bars, menus, vitals, xp, and the
        // server entity id — which is *stale*, not merely wrong: left in place it
        // would misattribute the next session's mob effects to whichever entity
        // the new server happens to assign that id to first). Two calls rather
        // than a field-by-field reset, for the same reason `reset_local_player` is
        // one: a component added to a spawn path and missed here leaks the old
        // session into the new one.
        self.write(|w| {
            insert_hud_components(w, local);
            lodestone_ecs::insert_session_components(w, local);
        });
        self.set_target(None);
        self.input_mut(InputState::release_all);

        self.status = if self.vanilla_atlas.is_some() {
            "live world (vanilla atlas)".to_string()
        } else if let Some(banner) = &self.asset_banner {
            format!("demo palette — {banner}")
        } else {
            "local world".to_string()
        };
    }

    /// The live connection, when one is attached. Lets a harness read the
    /// client-owned world (`loaded_chunks`, `sections_and_light_at`,
    /// `world_dimensions`) to check the shell's live mesh against ground truth.
    #[must_use]
    pub fn net(&self) -> Option<&NetClient> {
        self.net.as_ref()
    }

    /// The coarse session phase, for the menu state machine.
    ///
    /// Reads the [`Phase`] component; `Sim` holds no phase field.
    #[must_use]
    pub fn session_phase(&self) -> SessionPhase {
        self.read(|w| {
            w.get::<Phase>(self.local)
                .expect("the local player always carries Phase")
                .0
                .clone()
        })
    }

    /// Record a new session phase.
    fn set_phase(&mut self, phase: SessionPhase) {
        self.write_local(|w, local| {
            if let Some(mut current) = w.get_mut::<Phase>(local) {
                current.0 = phase;
            }
        });
    }

    /// Whether the local player is currently dead (awaiting the server-confirmed
    /// respawn). Movement is frozen while this holds.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.read(|w| w.get::<Dead>(self.local).is_some())
    }

    /// Number of respawns observed since the session started — a diagnostic the
    /// live death gate reads to confirm the client recovered from a death.
    #[must_use]
    pub fn respawn_count(&self) -> u64 {
        self.read(|w| {
            w.get::<RespawnCount>(self.local)
                .expect("the local player always carries RespawnCount")
                .0
        })
    }

    /// The most recent chat/system lines (oldest-first) for the HUD to draw,
    /// each paired with its **age in seconds** (now − arrival) so the HUD can
    /// apply the vanilla fade-out. Lines carry legacy `§` colour codes.
    #[must_use]
    pub fn recent_chat(&self, n: usize) -> Vec<(String, f32)> {
        let now = self.clock().secs;
        self.read(|w| {
            w.get::<SessionChat>(self.local)
                .expect("the local player always carries SessionChat")
                .0
                .recent_ages(n, now)
        })
    }

    /// Server-reported health in `0..=20`, or `None` off a live survival server.
    #[must_use]
    pub fn health(&self) -> Option<f32> {
        self.vitals().health
    }

    /// Server-reported food level in `0..=20`, or `None` off a live server.
    #[must_use]
    pub fn food(&self) -> Option<i32> {
        self.vitals().food
    }

    /// Server-reported air supply in ticks (`0..=300`), or `None` before the
    /// first entity-metadata update naming the local player arrives (see
    /// [`Vitals::air`]'s doc for why this rides a different event family than
    /// `health`/`food`).
    #[must_use]
    pub fn air(&self) -> Option<i32> {
        self.vitals().air
    }

    /// The [`Vitals`] component.
    ///
    /// # Read-only from this side
    ///
    /// There is no `set_vitals`, and there must not be one again. `Vitals`, [`Xp`]
    /// and [`ServerEntityId`] are folded by
    /// `lodestone_ecs::session::apply_local_player_state` on the **net thread**,
    /// into this same `World` and onto this same entity (§4.1(c) made
    /// `SharedState`'s session entity and `Sim.local` one entity). The shell used
    /// to fold `NetUpdate::{Health, Experience, LoggedIn}` into them itself, which
    /// after the `World` unification meant two writers of one component; those
    /// arms and the two `NetUpdate` variants are deleted.
    fn vitals(&self) -> Vitals {
        self.read(|w| {
            *w.get::<Vitals>(self.local)
                .expect("the local player always carries Vitals")
        })
    }

    /// The server-assigned entity id for the local player, `None` before login.
    ///
    /// Read by every entity-scoped update that has to decide "is this us" — mob
    /// effects, most obviously, whose packet applies to any entity. Written only
    /// by the net thread's fold; see [`Self::vitals`].
    #[must_use]
    fn server_entity_id(&self) -> Option<i32> {
        self.read(|w| {
            w.get::<ServerEntityId>(self.local)
                .expect("the local player always carries ServerEntityId")
                .0
        })
    }

    /// Server-reported experience as `(progress, level, total)`, or `None`
    /// before `set_experience` has arrived (e.g. the local dev world, or a
    /// live server before the first packet). `progress` is `0.0..1.0` toward
    /// the next level.
    #[must_use]
    pub fn experience(&self) -> Option<(f32, i32, i32)> {
        self.read(|w| {
            w.get::<Xp>(self.local)
                .expect("the local player always carries Xp")
                .0
        })
    }

    /// The current tab-list, formatted as `NAME  <latency>ms` rows sorted by
    /// vanilla display order. Empty until the server sends player-list data.
    ///
    /// # Read straight off the component since §4.1(c)
    ///
    /// This and the three accessors below used to go out through `NetClient` into
    /// the *client's* `World`, because the net thread's fold lived there and a
    /// component in one `World` is unreachable from another. There is one `World`
    /// now and [`Self::local`] is the entity the fold writes, so the round trip is
    /// gone. Still exactly one fold — `lodestone_ecs::session`'s `NetIngest`
    /// systems — and still one copy of it; what changed is only who reads it.
    #[must_use]
    pub fn player_rows(&self) -> Vec<String> {
        let list = self.read(|w| {
            w.get::<lodestone_ecs::SessionTabList>(self.local)
                .map(|list| list.0.clone())
                .unwrap_or_default()
        });
        crate::tablist::player_rows(&list, self.translator().as_ref())
    }

    /// The scoreboard sidebar to draw, or `None` when none is displayed (or off
    /// a live server). Folded through [`lodestone_game::scoreboard::Scoreboard`].
    #[must_use]
    pub fn sidebar(&self) -> Option<Sidebar> {
        let board = self.read(|w| {
            w.get::<lodestone_ecs::SessionScoreboard>(self.local)
                .map(|board| board.0.clone())
                .unwrap_or_default()
        });
        crate::scoreboard::sidebar_from(&board, self.translator().as_ref())
    }

    /// The active boss bars to draw, in render order. Empty off a live server.
    #[must_use]
    pub fn boss_bars(&self) -> Vec<BossBarView> {
        self.read(|w| {
            w.get::<lodestone_ecs::SessionBossBars>(self.local)
                .map_or_else(Vec::new, |bars| {
                    crate::overlay::boss_bars_from(&bars.0, self.translator().as_ref())
                })
        })
    }

    /// The XP bar to draw as `(level, progress 0..=1)`, `Some` only once the
    /// server has sent an experience update. Reads the already-folded
    /// [`Sim::experience`]; off a live server it stays `None` and no bar draws.
    #[must_use]
    pub fn xp(&self) -> Option<(i32, f32)> {
        self.experience()
            .map(|(progress, level, _total)| (level, progress))
    }

    /// The title/subtitle overlay as `(title, subtitle, alpha)`, `Some` while a
    /// server-sent title is visible. `Text` is flattened to a legacy `§` string
    /// at read time, matching the chat path, so colour survives once decoded.
    #[must_use]
    pub fn title_overlay(&self) -> Option<(String, Option<String>, f32)> {
        let state = self.read(|w| {
            w.get::<TitleOverlay>(self.local)
                .expect("the local player always carries TitleOverlay")
                .0
                .clone()
        });
        let title = state.title()?;
        Some((
            self.resolve_text(title).to_legacy_string(),
            state
                .subtitle()
                .map(|s| self.resolve_text(s).to_legacy_string()),
            state.alpha(),
        ))
    }

    /// The action-bar message as `(text, alpha)`, `Some` while a GameInfo
    /// message is visible (fades over its final ticks).
    #[must_use]
    pub fn action_bar_overlay(&self) -> Option<(String, f32)> {
        let state = self.read(|w| {
            w.get::<ActionBarOverlay>(self.local)
                .expect("the local player always carries ActionBarOverlay")
                .0
                .clone()
        });
        let text = state.text()?;
        Some((self.resolve_text(text).to_legacy_string(), state.alpha()))
    }

    /// The local player's active status effects, for the top-right HUD overlay.
    /// Empty until a server applies one; ticked down in [`Sim::step`].
    #[must_use]
    pub fn active_effects(&self) -> lodestone_game::effect::ActiveEffects {
        self.read(|w| {
            w.get::<HudEffects>(self.local)
                .expect("the local player always carries HudEffects")
                .0
                .clone()
        })
    }

    /// The folded player inventory menu. Off a live connection this returns an
    /// empty player menu so the local inventory screen can still render.
    ///
    /// Reads the [`lodestone_ecs::SessionMenus`] component — see
    /// [`Self::player_rows`] on why that is a direct read since §4.1(c). Note the
    /// *write* side is still `ClientHandle::menu_click`, which predicts against
    /// this same component under its own short guard: prediction has to mutate the
    /// one copy, and a clone has nowhere for the mutation to land.
    #[must_use]
    pub fn player_menu(&self) -> Menu {
        self.read(|w| {
            w.get::<lodestone_ecs::SessionMenus>(self.local)
                .map_or_else(Menu::player, |menus| menus.0.player().clone())
        })
    }

    /// The currently open server menu, if any.
    #[must_use]
    pub fn open_menu(&self) -> Option<OpenMenuSnapshot> {
        self.read(|w| {
            let menus = &w.get::<lodestone_ecs::SessionMenus>(self.local)?.0;
            Some(OpenMenuSnapshot {
                window_id: menus.opened_window_id()?,
                menu_type: menus.opened_menu_type()?.clone(),
                title: menus.opened_title()?.clone(),
                menu: menus.opened()?.clone(),
            })
        })
    }

    /// Close the open server menu: clear it locally **and** tell the server.
    ///
    /// # Both halves are required, and the local one is why this takes `&mut self`
    ///
    /// This used to only send `ContainerClose`, and the screen therefore never went
    /// away — you could open a crafting table and not get out of it. A vanilla
    /// server does **not** echo a close back; `ClientboundContainerClosePacket` is
    /// sent only when the *server* forces a close. So waiting for the wire to clear
    /// [`Self::open_menu`] waits forever, and every consumer that keys off it —
    /// `active_container_menu`, the key-dispatch gate, the container draw — stayed
    /// convinced a menu was open.
    ///
    /// Vanilla's `Player.closeContainer()` clears the client's own menu immediately
    /// and *then* notifies the server, which is what this now mirrors. The local
    /// clear reuses [`ClientEvent::ScreenClosed`] rather than poking the component,
    /// so the close travels the same fold as a server-driven one and cannot drift
    /// from it (`lodestone_game::menus::Menus::apply`).
    ///
    /// It needs `&mut self` for that write. The old `&self` signature was not a
    /// style choice — it made the local clear *unrepresentable*, which is why the
    /// bug survived a fix to the key dispatch that reached this function correctly.
    pub fn close_open_menu(&mut self) {
        let Some(open) = self.open_menu() else { return };
        if let Some(net) = &self.net {
            net.send_action(ClientAction::ContainerClose {
                window_id: open.window_id,
            });
        }
        let window_id = open.window_id;
        self.write_local(|w, local| {
            if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
                menus.0.apply(&lodestone_model::ClientEvent::ScreenClosed { window_id });
            }
        });
    }

    /// Compose a typed chat line onto the outbound [`ClientAction`] seam and hand
    /// it to the live client (a leading `/` is a command, else a chat message).
    /// A blank line sends nothing. No-op without a live connection. Returns
    /// whether anything was sent, so the caller can echo command feedback.
    ///
    /// `/givedebug <item> <amount>` is intercepted first (see
    /// [`crate::chat::intercept_give_debug`]): a well-formed line is translated to
    /// the server's real `/give @s <item> <amount>` and both the translation and
    /// the send happen here, so the user always sees what was actually sent. A
    /// malformed line produces a local-only chat message and never reaches the
    /// network — a debug command that fails silently is worse than none.
    pub fn send_chat(&mut self, line: &str) -> bool {
        match crate::chat::intercept_give_debug(line) {
            crate::chat::GiveDebugOutcome::Send { local_echo, action } => {
                self.push_local_chat(local_echo);
                if let Some(net) = &self.net {
                    net.send_action(action);
                    return true;
                }
                return false;
            }
            crate::chat::GiveDebugOutcome::Error(message) => {
                self.push_local_chat(message);
                return false;
            }
            crate::chat::GiveDebugOutcome::NotGiveDebug => {}
        }
        let Some(action) = compose_chat_action(line) else {
            return false;
        };
        if let Some(net) = &self.net {
            net.send_action(action);
            true
        } else {
            false
        }
    }

    /// Append a client-local line (never sent to the server) to the session's
    /// chat feed, stamped with the driver's own clock. Used for local-only
    /// feedback such as a malformed `/givedebug` line, mirroring how the
    /// `NetUpdate::Chat` handler stamps an inbound server line.
    fn push_local_chat(&mut self, text: impl Into<String>) {
        let now = self.clock().secs;
        let text = lodestone_model::Text::literal(text.into());
        self.write_local(|w, local| {
            if let Some(mut chat) = w.get_mut::<SessionChat>(local) {
                chat.0.push_system(text, now);
            }
        });
    }

    /// The currently selected hotbar slot, `0..9`.
    #[must_use]
    pub fn selected_slot(&self) -> usize {
        self.read(|w| {
            w.get::<SelectedSlot>(self.local)
                .expect("the local player always carries SelectedSlot")
                .0
        })
    }

    /// Select hotbar slot `slot` (`0..9`); out-of-range values are ignored. When
    /// the selection actually changes, echoes it to the server via
    /// [`ClientAction::SetCarriedItem`] so the held item stays in sync. No-op
    /// off a live connection beyond updating the local selection the HUD draws.
    pub fn select_slot(&mut self, slot: usize) {
        if slot >= HOTBAR_SLOTS || slot == self.selected_slot() {
            return;
        }
        self.write_local(|w, local| {
            if let Some(mut selected) = w.get_mut::<SelectedSlot>(local) {
                selected.0 = slot;
            }
        });
        self.send_selected_slot();
    }

    /// Advance the hotbar selection by `delta` slots, wrapping at both ends
    /// (mouse-wheel behaviour). A positive `delta` moves right, matching vanilla
    /// scroll-down.
    pub fn cycle_slot(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        let n = HOTBAR_SLOTS as i32;
        let next = (self.selected_slot() as i32 + delta).rem_euclid(n) as usize;
        self.select_slot(next);
    }

    /// Push the current selection to the server. Best-effort: no-op without a
    /// live connection, and a closed session just drops it.
    fn send_selected_slot(&self) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SetCarriedItem {
                slot: self.selected_slot() as i32,
            });
        }
    }

    /// Number of meshing jobs still outstanding.
    #[must_use]
    pub fn pending_meshes(&self) -> usize {
        self.terrain(|t| t.scheduler.pending())
    }

    /// Collect finished meshes for the caller to upload to the GPU.
    ///
    /// Also records each key into `TerrainMesh::uploaded_sections`, which is how
    /// [`Sim::end_session`] later knows every section the GPU is holding for
    /// this session and can queue every one of them for removal.
    pub fn drain_meshes(&mut self) -> Vec<Meshed> {
        self.terrain_mut(TerrainMesh::drain_meshes)
    }

    /// Block until every scheduled mesh is ready (used by headless runs/tests).
    pub fn drain_all_meshes(&mut self) -> Vec<Meshed> {
        self.terrain_mut(TerrainMesh::drain_all_meshes)
    }

    /// Sections that became empty (drained by the app to remove GPU meshes).
    pub fn drain_removals(&mut self) -> Vec<SectionKey> {
        self.terrain_mut(TerrainMesh::drain_removals)
    }

    /// Whether free-fly mode is active.
    #[must_use]
    pub fn flying(&self) -> bool {
        self.read(|w| {
            w.get::<Flying>(self.local)
                .expect("the local player always carries Flying")
                .0
        })
    }

    /// Toggle free-fly (noclip) mode. Entering fly zeroes velocity so the player
    /// doesn't keep any fall momentum.
    pub fn toggle_fly(&mut self) {
        let flying = !self.flying();
        self.write_local(|w, local| {
            if let Some(mut fly) = w.get_mut::<Flying>(local) {
                fly.0 = flying;
            }
        });
        self.player_mut(|player| {
            player.velocity = Vec3d::ZERO;
            player.on_ground = false;
        });
    }

    /// Frames rendered per physics tick since start (fixed-timestep health).
    #[must_use]
    pub fn frames_per_tick(&self) -> f32 {
        self.clock().frames_per_tick()
    }

    /// Apply accumulated mouse motion to the view angles.
    ///
    /// Deliberately **not** a `GameTick` system: mouse-look is per-frame in
    /// vanilla too (`MouseHandler.turnPlayer` runs off the render loop, not the
    /// tick), so binding it to 20 Hz would make aiming feel stepped at high
    /// frame rates.
    pub fn apply_mouse(&mut self) {
        let (dx, dy) = self.input_mut(InputState::take_mouse);
        if dx != 0.0 || dy != 0.0 {
            let sensitivity = self.config.sensitivity;
            let player = self.player();
            let (yaw, pitch) = apply_look(player.yaw, player.pitch, dx, dy, sensitivity);
            self.player_mut(|player| {
                player.yaw = yaw;
                player.pitch = pitch;
            });
        }
    }

    /// What this tick's physics collides against.
    ///
    /// The *decision* is the shell's — it needs the session, the atlas and the
    /// diagnostic switch — but the geometry is handed to the ECS as an owned
    /// [`PlayerCollision`] so `player_physics` can be a real scheduled system.
    /// See [`CollisionSource`] for why the borrow could not cross that boundary
    /// directly.
    fn tick_collision(&mut self) -> PlayerCollision {
        // No session and no terrain: there is nothing to stand on and nobody to
        // be.
        if self.net.is_none() && self.chunk_world().is_empty() {
            return PlayerCollision::NoWorld;
        }

        if self.vanilla_atlas.is_some() && self.net.is_some() {
            if !self.collide_against_live_world {
                // The negative control, and the one place Stage 4's single store
                // must *not* be used. See `collide_against_live_world`'s doc: the
                // pre-fix behaviour it reproduces is "collide against terrain we
                // do not have", so it has to name an explicitly empty store.
                // Falling through to `chunk_collision()` would collide against the
                // server's real terrain through the demo classifier — where every
                // non-air vanilla id happens to read as solid — and the control
                // would silently stop failing.
                return PlayerCollision::View(Arc::new(ChunkWorldCollision(ChunkWorld::default())));
            }
            // Live path: collide against the server's terrain. This changes
            // *where blocks come from*, not how collision resolves —
            // `LiveCollision` fills the exact same `CollisionView` hooks
            // `WorldCollision` does, so movement stays bit-exact.
            return match self.live_collision() {
                Some(view) => PlayerCollision::View(Arc::new(LiveCollisionSource(view))),
                // The player's own column has not streamed in yet.
                None => PlayerCollision::Pending,
            };
        }

        PlayerCollision::View(self.chunk_collision())
    }

    /// This tick's entity-push neighbourhood — an owned snapshot handed to the
    /// ECS as [`NearbyEntities`] so [`lodestone_ecs::player::player_physics`]
    /// can stay a plain scheduled system, exactly the pattern
    /// [`Self::tick_collision`] already established for [`PlayerCollision`].
    ///
    /// # Which entities: a jar-dumped census, default-**deny**
    ///
    /// [`VersionData::entity_facts`] answers it, from
    /// `lodestone_data::entity_census` — a table generated from a headless 26.2
    /// server dump of all 158 entity types (`EntityCensusOracle.java`). A
    /// neighbour pushes the player only if vanilla's crowd pass reaches
    /// `player.push(neighbour)`, which needs three things: the type is a
    /// `LivingEntity` (the sole caller of `pushEntities()`, at
    /// `LivingEntity.java:3163`), its `pushEntities()` can still see a player
    /// (`Bat.java:95` empties it; `ArmorStand.java:178` narrows it to ridable
    /// minecarts), and its `doPush(Entity)` still reaches `entity.push(this)`
    /// for one (`Parrot.java:390` skips players outright).
    ///
    /// Note this is *not* the neighbour's `isPushable()`. That gates the
    /// **pushee** — it is the `input` of `EntitySelector.pushableBy` — which is
    /// why `lodestone_physics::push::pair_admitted` takes our own
    /// `self_pushable` and never reads the neighbour's. Keying the census on
    /// `isPushable()` would admit boats and minecarts, which both override it
    /// to `true`.
    ///
    /// An unknown type — and a build with no version family compiled in —
    /// reports `false`. That polarity is the whole point. The denylist this
    /// replaced wrongly admitted seven real 26.2 types: `bamboo_raft` and
    /// `bamboo_chest_raft` (its substring check looked for `boat`, and 1.21.2
    /// named those *rafts*), `splash_potion` and `lingering_potion` (26.2 split
    /// `potion` in two), `ominous_item_spawner`, and the living-but-inert `bat`
    /// and `parrot`. Every one of them would have shoved the player.
    ///
    /// # What the census deliberately excludes
    ///
    /// Boats and rideable minecarts do push players in vanilla, but from their
    /// own ticks — `AbstractBoat.push(Entity)` (`AbstractBoat.java:289`, with a
    /// Y-ordering condition at `:181`) and
    /// `NewMinecartBehavior.pushEntities(AABB)` (`:537`, gated on
    /// `isRideable()` and querying a `1.0E-7`-inflated box). Those cannot join
    /// this list without changing the gate, so the census reports them `false`
    /// rather than approximating them into the wrong pass. See
    /// [`lodestone_model::EntityFacts::pushes_players`].
    fn tick_nearby_entities(&mut self) -> NearbyEntities {
        let center = self.player().position;
        let nearby = self.write(|w| {
            let mut state = w.query::<(&Position, &EntityKind)>();
            // Read once, before the loop. Building the `QueryState` ends the
            // mutable borrow, so the resource handle and the iteration coexist
            // as two immutable reborrows — which is what lets this stay a single
            // `write` pass instead of a resource lookup per candidate.
            let version = w.resource::<VersionData>();
            state
                .iter(w)
                .filter_map(|(pos, kind)| {
                    let feet = Vec3d::new(pos.0.x, pos.0.y, pos.0.z);
                    if (feet.x - center.x).abs() > NEARBY_ENTITY_RADIUS
                        || (feet.y - center.y).abs() > NEARBY_ENTITY_RADIUS
                        || (feet.z - center.z).abs() > NEARBY_ENTITY_RADIUS
                    {
                        return None;
                    }
                    // A type outside the census, or no adapter at all, is a
                    // miss — never a permissive fallthrough.
                    let facts = version.entity_facts(&kind.0)?;
                    if !facts.pushes_players {
                        return None;
                    }
                    // `step_height` plays no part in vanilla's `makeBoundingBox`;
                    // the `RangedAttribute` default is passed so the field never
                    // reads as a real step height resolved from an attribute map.
                    let dims =
                        EntityDimensions::new(facts.dimensions.width, facts.dimensions.height, 0.6);
                    Some(NearbyEntity::living(feet, dims.bounding_box(feet)))
                })
                .collect::<Vec<_>>()
        });
        NearbyEntities(nearby)
    }

    /// A `'static` sampler of the **outline** boxes of the block at a world
    /// position, for `RenderState::set_outline_shape_source`.
    ///
    /// `None` when this session cannot answer: no live connection, no vanilla
    /// atlas (the demo palette has no outline census and is all full cubes, which
    /// is what an empty result already means), or no version family compiled in
    /// for the configured protocol.
    ///
    /// # Why this is not `CollisionSource`, which is what the plan expected
    ///
    /// Stage 2's [`CollisionSource`] hands out a `CollisionView`, whose geometry
    /// is the **collision** shape. The selection box needs the **outline** shape,
    /// and those are a different vanilla shape family: kelp has an outline and no
    /// collision, cobweb's outline is a full cube while its collision is empty, and
    /// **half of all 26.2 block states have an outline that differs from their
    /// collision shape** (`VersionAdapter::block_outline`'s docs). Wiring
    /// `CollisionSource` here would replace one wrong box with a differently wrong
    /// box in half of all cases, which is worse than a unit cube because it would
    /// look right.
    ///
    /// # Why this did not need Stage 4 either
    ///
    /// The brief listed the selection box as blocked on the chunk-world
    /// unification. It was not: everything the closure needs was already `'static`
    /// and `Send + Sync` — `NetClient::shared_handle` is an
    /// `Arc<OnceLock<Arc<ClientHandle>>>`, `ClientHandle::block_at` is public, and
    /// `VersionAdapter` is declared `Send + Sync + Debug` at
    /// `lodestone-model/src/adapter.rs:391`. Capturing the *handle* rather than the
    /// store is also what makes this installable before login, when there is no
    /// store to capture yet.
    ///
    /// A second boxed adapter is minted rather than sharing
    /// [`Self::version_data`]: adapters are stateless value types, so the copy
    /// costs a `Box` and answers identically — the same reasoning `version_data`'s
    /// own doc records for why it is already a second instance.
    #[must_use]
    pub fn outline_shape_source(
        &self,
    ) -> Option<impl Fn([i32; 3]) -> Vec<lodestone_physics::Aabb> + Send + Sync + 'static> {
        self.vanilla_atlas.as_ref()?;
        let handle = self.net.as_ref()?.shared_handle();
        let adapter = lodestone_registry::adapter_for_protocol(self.config.protocol)?;
        Some(move |block: [i32; 3]| {
            let Some(client) = handle.get() else {
                return Vec::new();
            };
            let Some(state) = client.block_at(BlockPos {
                x: block[0],
                y: block[1],
                z: block[2],
            }) else {
                return Vec::new();
            };
            let Some(boxes) = adapter.block_outline(state) else {
                return Vec::new();
            };
            // The census is block-local `0..1`; the renderer wants world space.
            boxes
                .iter()
                .map(|b| {
                    lodestone_physics::Aabb::new(
                        f64::from(block[0]) + f64::from(b.min[0]),
                        f64::from(block[1]) + f64::from(b.min[1]),
                        f64::from(block[2]) + f64::from(b.min[2]),
                        f64::from(block[0]) + f64::from(b.max[0]),
                        f64::from(block[1]) + f64::from(b.max[1]),
                        f64::from(block[2]) + f64::from(b.max[2]),
                    )
                })
                .collect()
        })
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

    /// Hand everything the `GameTick` systems queued to the socket, in order.
    ///
    /// The queue is drained (not read) even with no connection, so a
    /// disconnected session cannot accumulate a session's worth of stale
    /// actions to deliver on reconnect.
    ///
    /// # Also the animation half of every queued swing
    ///
    /// A [`ClientAction::SwingArm`] on this queue is the *same* event vanilla's
    /// `LivingEntity.swing` handles: it both sends `ClientboundAnimatePacket` to
    /// everyone else **and** starts the swinger's own animation clock. This is the
    /// single funnel every tick-driven swing passes through — notably
    /// `interact.rs`'s hold-to-mine loop via `lodestone_game::mining`, which is
    /// what makes the arm swing while breaking a block — so hooking it here means
    /// a new producer of swings animates for free rather than having to remember
    /// to. [`Self::use_item_live`] is the one swing that does *not* come through
    /// here (it writes to the socket directly, to control wire order) and calls
    /// [`Self::swing_hand`] itself.
    ///
    /// Deliberately **outside** the `if let Some(net)` below: the animation is
    /// client-side and must not depend on having a live socket, exactly as the
    /// demo world's [`Self::break_block`] swings with no connection at all.
    fn drain_action_queue(&mut self) {
        // The guard is released before `net.send_action`, per `EcsHandle`'s rule 1:
        // `send_action` is a channel push today, but the whole `NetClient` surface
        // otherwise reads this same `World` through `ClientHandle`, and holding a
        // write guard into it would deadlock the moment one of those was reached.
        let actions = self.write(|w| std::mem::take(&mut w.resource_mut::<ActionQueue>().0));
        // Only the *main* hand drives the first-person arm and the self-avatar's
        // right arm. An off-hand swing animates the left arm, which neither
        // consumer draws — treating it as a main-hand swing would swing the wrong
        // limb, so it is ignored rather than approximated.
        if actions
            .iter()
            .any(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main }))
        {
            self.swing_hand();
        }
        if let Some(net) = &self.net {
            for action in actions {
                // Best-effort — a closed session just drops it.
                net.send_action(action);
            }
        }
    }

    /// Start the local player's arm-swing animation, like `LivingEntity.swing`.
    ///
    /// Idempotent within the first half of a running swing — [`EntityPose::start_swing`]
    /// swallows a restart before its half-way point, which is what turns
    /// `interact.rs`'s once-per-tick swing during a held mine into a continuous
    /// arc instead of a stutter.
    ///
    /// # One-tick offset from vanilla, and why it is left alone
    ///
    /// Vanilla calls `swing()` from `Minecraft.handleKeybinds`, which runs
    /// *before* `updateSwingTime` in the same tick, so `swingTime` reaches `0` on
    /// the tick the click happened. Here [`Self::step`] ticks `body_pose` before
    /// draining the action queue, so the clock starts on the **next** tick — a
    /// 50 ms delay on the animation beginning, invisible at any frame rate, and
    /// worth less than reordering a tick loop whose wire ordering is load-bearing.
    ///
    /// The duration is [`lodestone_entity::pose::swing_duration`] with **no**
    /// effect inputs: neither Haste nor Mining Fatigue has a modelled source in
    /// this engine (`lodestone_game::mining::BreakInputs` has the identical hole —
    /// see `tool_inputs_stay_at_bare_hand_defaults`), so this is vanilla's
    /// component default of 6 ticks. Closing that hole is a change of arguments
    /// here, not a change of clock.
    pub(crate) fn swing_hand(&mut self) {
        self.body_pose.start_swing(lodestone_entity::pose::swing_duration(
            lodestone_entity::pose::DEFAULT_SWING_DURATION,
            None,
            None,
        ));
    }

    /// How far through an arm swing the local player is **this frame**, in
    /// `0.0..=1.0` — vanilla's `Player.getAttackAnim(partialTick)`.
    ///
    /// This is the value `RenderState::set_hand_swing_source`'s closure returns and
    /// the value `third_person_body_state` puts on [`AnimInput::attack_anim`]; both
    /// consumers read this one accessor so they can never disagree about where in
    /// the swing the player is.
    ///
    /// The swing clock advances in [`Self::step`]'s 20 Hz loop and is only
    /// *interpolated* here, so calling this more often does not make the arm swing
    /// faster. Reading it per frame is the correct and intended use.
    #[must_use]
    pub fn hand_swing_progress(&self) -> f32 {
        self.body_pose.attack_anim_lerp(self.clock().interp_alpha)
    }

    /// Advance the simulation by real elapsed time, running fixed 20 Hz `GameTick`
    /// schedules against the world's collision. Rendering interpolates between
    /// ticks via [`Sim::interp_alpha`].
    ///
    /// # What the tick loop is, since Stage 2
    ///
    /// Each iteration of the fixed-timestep loop resolves this tick's collision
    /// geometry, runs one `GameTick` schedule (`TickSet::Input` →
    /// `Physics` → `Send`), then hands whatever the systems queued to the
    /// socket. Everything the schedule needs is a component or resource, so a
    /// plugin can insert a system anywhere in that order.
    ///
    /// **Movement intent is now recomputed per tick, not per frame.** It used to
    /// be computed once before the loop, so a frame long enough to run several
    /// catch-up ticks reused one decision for all of them — see
    /// `lodestone_controller::ecs::compute_movement_intent` for exactly what
    /// that changes (nothing at all at 20 fps or better; the difference is
    /// confined to stalls).
    pub fn step(&mut self, dt: f64) {
        self.apply_mouse();
        // The **one** accumulator, on the **one** catch-up policy
        // (`lodestone_ecs::MAX_CATCH_UP_SECS` — ten ticks, vanilla's own; see that
        // constant for why the shell's old inner `0.25 s` clamp lost).
        self.clock_mut(|clock| clock.begin_frame(dt));

        // The derived egress gate. Refreshed once per frame because both of its
        // inputs are frame-stable: `poll_net` is the only thing that changes the
        // phase and it runs after the loop.
        let egress = Egress {
            in_world: self.session_phase() == SessionPhase::Connected,
            live: self.is_live(),
        };
        self.write(|w| w.insert_resource(egress));

        // `Update` before the tick loop, not after it. `FrameSet::Interpolate`'s
        // `advance_interp_clocks` has to run first, because the tick systems
        // (`tick_item_physics`, `tick_walk_animation`) measure off the *drawn*
        // pose and would otherwise measure last frame's. That ordering was
        // internal to `EntityInterpolator::update_with_view` before §4.1(c) and is
        // now the frame's own.
        //
        // The one behaviour change this carries: `FrameSet::Terrain`'s
        // `heal_dirty_columns` now runs *before* `poll_net`, so a column that
        // arrives this frame has its neighbours healed on the next one. It is a
        // coalescing drain feeding an async worker pool on a per-frame budget, so a
        // single frame of latency is inside the noise it already tolerates —
        // but it is a change, not a no-op.
        let frame_dt = dt as f32;
        self.write(|w| {
            w.insert_resource(crate::entities::FrameDelta(frame_dt));
            w.run_schedule(Update);
        });

        loop {
            if !self.clock_mut(FrameClock::take_tick) {
                break;
            }
            let collision = self.tick_collision();
            let item_collision = self.item_collision();
            let nearby = self.tick_nearby_entities();
            self.write(|w| {
                w.insert_resource(collision);
                w.insert_resource(item_collision);
                w.insert_resource(nearby);
                w.run_schedule(GameTick);
            });
            // Drive the local player's own walk/head-look clock off the
            // post-physics position, exactly like a tracked network entity's
            // `EntityPose::tick` — see `Self::body_pose`'s doc for why this
            // is unconditional rather than gated on `third_person`. Read
            // *after* the `GameTick` write guard above is dropped: `Self::player`
            // takes its own short read guard, and holding one across another
            // accessor is exactly what this crate's locking rules forbid.
            let p = self.player();
            self.body_pose
                .tick(p.position.x, p.position.z, p.yaw, p.yaw, p.pitch);
            // The camera's eye chases the entity's, half the gap per tick, so a
            // pose change eases instead of snapping. Same read guard as above.
            self.eye_height_smoother.tick(p.eye_height);
            // Vanilla emits a movement packet every tick (20 Hz); mirror that so
            // the server sees our authoritative position/rotation and never has
            // to correct us. `TickSet::Send` produced it; this is where it and
            // everything else the tick queued reach the socket, in order.
            //
            // Since Stage 5 that includes the sprint edge and the hold-to-mine
            // loop, which used to be sent *after* this drain by a hand-written
            // `drive_interaction()` below. Wire order is unchanged: they are now
            // `TickSet::Send` systems ordered after `send_player_input`, so their
            // actions sit behind the movement packet in the same single queue.
            self.drain_action_queue();
            // The tick was counted and withdrawn by `FrameClock::take_tick` at the
            // top of this loop, so there is nothing to book-keep here any more.
            self.tick_particles();
            // The HUD status effects and the title/action-bar overlays used to be
            // aged by three hand-written `tick(1)` calls right here. They are now
            // `lodestone_ecs::session::tick_hud_overlays` in `TickSet::Animate`,
            // which the `run_schedule(GameTick)` above already ran — same fixed
            // 20 Hz, but a plugin can now order against it and the components are
            // the only copy.
            // The live block interactions — the sprint edge and the held dig —
            // used to be driven from here by `drive_interaction()`. They are
            // `crate::interact`'s `send_sprint_command` / `drive_mining` systems in
            // `TickSet::Send` since Stage 5, which the `run_schedule(GameTick)`
            // above already ran; the `Egress` resource inserted before this loop
            // carries the `phase == Connected && is_live()` gate that used to be
            // written here. See `docs/sim-dissolution.md` for why the blocker
            // Stage 2 recorded (`Sim.target` / `version_data` / the live block
            // store) was not the real one.
        }
        // Publish the sub-tick residual. One number now: the camera's between-tick
        // ease and `extract_entity_draws`'s walk-cycle partial tick both read it,
        // where they used to read two accumulators' residuals.
        self.clock_mut(FrameClock::end_frame);

        self.poll_net();
        // Fold this frame's server report into the render-side tracks, then extract.
        // Still after the tick loop and after ingest, which is the order the ~25
        // interpolation tests are written against — see `fold_snapshots`' docs for
        // why it is not a `NetIngest` system even now that it could reach the
        // components directly.
        self.fold_entities();
        self.write(|w| w.run_schedule(Extract));
        self.refresh_stats();
    }

    /// What **dropped items** are simulated against this tick.
    ///
    /// Deliberately not [`Self::tick_collision`], and the difference is the whole
    /// reason [`crate::entities::ItemCollision`] is a second resource — see its
    /// docs for the two cases where the player's answer is wrong for an item.
    /// [`Self::live_collision`] is the same 3×3-column snapshot the physics tick
    /// builds; off a live connection there are no tracked items either, so the
    /// offline fallback is never actually asked to resolve real terrain.
    fn item_collision(&self) -> crate::entities::ItemCollision {
        crate::entities::ItemCollision(match self.live_collision() {
            Some(view) => PlayerCollision::View(Arc::new(LiveCollisionSource(view))),
            None => PlayerCollision::View(self.chunk_collision()),
        })
    }

    /// Fold this frame's entity snapshots into the render-side component set, so
    /// [`entity_draws`](Self::entity_draws) yields smooth per-frame transforms.
    /// No live connection means no entities.
    ///
    /// # What §4.1(c) changed here
    ///
    /// This used to be `update_entities`, which drove
    /// `EntityInterpolator::update_with_view` — a whole second `World` running its
    /// own `Update`, its own `GameTick` loop off its own accumulator, and its own
    /// `Extract`. Those three schedule runs are now the frame's own, so all this
    /// does is the fold. The item collision it used to pass by argument is the
    /// [`crate::entities::ItemCollision`] resource the tick loop inserts.
    fn fold_entities(&mut self) {
        let snapshots = self
            .net
            .as_ref()
            .map_or_else(Vec::new, NetClient::entity_snapshots);
        // `entity_snapshots` reads this same `World` through `ClientHandle`, so it
        // is resolved to an owned `Vec` *before* the guard below is taken. Doing it
        // the other way round is `EcsHandle`'s rule 1 and deadlocks.
        self.write(|w| crate::entities::fold_entity_snapshots(w, &snapshots));
    }

    /// The interpolated entities to draw this frame, resolved by the renderer
    /// into instanced draws. Empty off a live server.
    #[must_use]
    pub fn entity_draws(&self) -> Vec<EntityDraw> {
        self.read(crate::entities::extracted_entity_draws)
    }

    /// The local player's water/lava submersion this tick, for the shell's
    /// submerged-fog decision (and, later, the underwater overlay, ambient
    /// sounds and swim pose). Version-free and bit-exact — the shell reads this
    /// shared truth rather than deriving its own boolean.
    ///
    /// Written by `lodestone_ecs::player::player_physics` against the very view
    /// movement collided against, so it is consistent with where the tick left
    /// the player.
    #[must_use]
    pub fn fluid_state(&self) -> FluidState {
        self.read(|w| {
            w.get::<Submersion>(self.local)
                .expect("the local player always carries Submersion")
                .0
        })
    }

    /// Overwrite the submersion summary.
    ///
    /// Only for a caller that needs to place the player in a fluid without
    /// simulating one — i.e. a test. Real play never calls this: the value
    /// belongs to the physics producer, and a shell-side write would be exactly
    /// the "invents its own submerged boolean" this type exists to prevent.
    #[cfg(test)]
    fn set_fluid_state(&mut self, fluid: FluidState) {
        self.write_local(|w, local| {
            if let Some(mut submersion) = w.get_mut::<Submersion>(local) {
                submersion.0 = fluid;
            }
        });
    }

    /// Build a [`LiveCollision`] snapshot of the server terrain around the
    /// player, or `None` when the live world can't yet be collided against
    /// (no atlas/net/dimensions, or the player's own column hasn't streamed in).
    ///
    /// Snapshots the 3×3 columns centred on the player over the full vertical
    /// range under a single lock (`sections_at`), returning owned
    /// `Arc<ChunkSection>` handles so no world lock is held while physics queries
    /// it. The 3×3 span covers the player's ±0.3-wide hitbox and its swept path
    /// within a tick; all-air sections are elided by `sections_at` and simply
    /// read as air.
    fn live_collision(&self) -> Option<LiveCollision> {
        let atlas = self.vanilla_atlas.clone()?;
        let net = self.net.as_ref()?;
        let dims = net.world_dimensions()?;
        let min_y = dims.min_y;
        let section_count = dims.section_count();

        let position = self.player().position;
        let pcx = (position.x.floor() as i32).div_euclid(16);
        let pcz = (position.z.floor() as i32).div_euclid(16);

        // Hold the player until the ground under them is known. `sections_at`
        // elides all-air sections to `None`, so an absent section is *not* proof
        // of an unloaded column — key the hold on the column being loaded.
        if !net.is_chunk_loaded(lodestone_client::ChunkPos { x: pcx, z: pcz }) {
            return None;
        }

        let mut requests: Vec<(lodestone_client::ChunkPos, usize)> =
            Vec::with_capacity(9 * section_count);
        for cz in (pcz - 1)..=(pcz + 1) {
            for cx in (pcx - 1)..=(pcx + 1) {
                for si in 0..section_count {
                    requests.push((lodestone_client::ChunkPos { x: cx, z: cz }, si));
                }
            }
        }

        let fetched = net.sections_at(&requests);
        let mut sections = HashMap::new();
        for ((pos, si), section) in requests.iter().zip(fetched) {
            if let Some(section) = section {
                sections.insert((pos.x, pos.z, *si), section);
            }
        }

        Some(LiveCollision::new(
            sections,
            min_y,
            section_count,
            atlas,
            crate::collision::inferred_version_data(),
        ))
    }

    /// Whether this session is rendering a live server world (as opposed to the
    /// offline demo). The stitched vanilla atlas plus a live connection is the
    /// single discriminant used everywhere the live and demo paths diverge.
    fn is_live(&self) -> bool {
        self.vanilla_atlas.is_some() && self.net.is_some()
    }

    /// Recompute the targeted block by casting the view ray from the (already
    /// interpolated) camera. Call once per frame before rendering the outline.
    ///
    /// The pick ray does **not** consult `is_solid`. `is_solid` is the *collision*
    /// predicate (also fed to the physics engine), and vanilla deliberately gives
    /// cross-plants (`short_grass`, ferns, flowers, kelp) an empty collision shape —
    /// you walk through grass — while picking them still works, because vanilla's
    /// `clip`/`clipWithInteractionOverride` walks a *separate* outline/interaction
    /// shape (`BlockBehaviour.getShape` / `getInteractionShape`), not the collision
    /// shape.
    ///
    /// The whole question therefore lives in one place,
    /// [`LiveCollision::is_pickable`] / [`WorldCollision::is_pickable`] — read its
    /// docs, which record why an earlier inlined `!is_water(...)` here made **kelp
    /// and every waterlogged block unbreakable**. Deliberately a single call and not
    /// an `||` chain: the predicate the collision tests exercise has to be the exact
    /// predicate the ray uses, or the gate proves nothing about the pick.
    pub fn update_target(&mut self, aspect: f32) {
        let cam = self.camera(aspect);
        let origin = [
            f64::from(cam.position.x),
            f64::from(cam.position.y),
            f64::from(cam.position.z),
        ];
        let fwd = cam.forward();
        let dir = [f64::from(fwd.x), f64::from(fwd.y), f64::from(fwd.z)];
        // Live: raycast the server's terrain (client-owned world), not the demo
        // world, or dig/place would target phantom offline blocks. The 3×3
        // column snapshot spans ±16 blocks — far more than REACH (4.5) — so a
        // face at the edge of reach is always covered. A `None` snapshot means
        // the player's own column has not streamed in; nothing is targetable.
        let hit = if self.is_live() {
            self.live_collision()
                .and_then(|view| raycast(origin, dir, REACH, |x, y, z| view.is_pickable(x, y, z)))
        } else {
            let store = self.chunk_world();
            let world = store.read();
            let view = WorldCollision::new(&world);
            raycast(origin, dir, REACH, |x, y, z| view.is_pickable(x, y, z))
        };
        self.set_target(hit);
        // Shared with the demo world too (harmlessly a no-op there — the demo
        // ECS holds no networked entities), so `crack_target`/the outline and
        // `EntityRayTarget` are always derived from the exact same ray.
        self.update_entity_target(origin, dir, hit);
    }

    /// Recompute [`EntityRayTarget`] from the same ray [`Self::update_target`]
    /// just cast against blocks — vanilla's entity half of
    /// `GameRenderer.pick`, which [`Self::begin_attack`] reads to decide
    /// between `case ENTITY` and `case BLOCK`.
    ///
    /// The search radius is [`ENTITY_REACH`] (`3.0`, vanilla's
    /// `DEFAULT_ENTITY_INTERACTION_RANGE`, `Player.java:134`), shortened to
    /// `block_hit`'s own entry distance when a block sits closer than that —
    /// matching vanilla's `blockDistance` clamp, so a wall between the eye and
    /// an entity is never picked through. The clamp treats the hit block as a
    /// unit cube rather than its real outline shape (this module does not
    /// carry outline geometry — see [`Self::outline_shape_source`]'s docs on
    /// the same gap); that only ever shortens the entity search, so the worst
    /// case is a slightly conservative cutoff, never a pick through solid
    /// terrain.
    ///
    /// Candidates come from the same `(Position, EntityKind)` query
    /// [`Self::tick_nearby_entities`] uses for pushers, resolved to a hitbox
    /// through the identical [`VersionData::entity_facts`] seam — an unknown
    /// type is excluded, never approximated. The local player is never a
    /// candidate: `apply_entity_spawn`/`apply_local_player_login`
    /// (`lodestone_ecs::ingest`) never give the local player's own `Entity` a
    /// `Position`/`EntityKind` component, so the query structurally cannot
    /// return it — the same property vanilla's `clip()` gets from excluding
    /// `this` explicitly.
    fn update_entity_target(&mut self, origin: [f64; 3], dir: [f64; 3], block_hit: Option<RayHit>) {
        let block_limit = block_hit.and_then(|hit| {
            let min = [
                f64::from(hit.block[0]),
                f64::from(hit.block[1]),
                f64::from(hit.block[2]),
            ];
            let max = [min[0] + 1.0, min[1] + 1.0, min[2] + 1.0];
            ray_aabb(origin, dir, REACH, min, max)
        });
        let search_limit = block_limit.map_or(ENTITY_REACH, |d| d.min(ENTITY_REACH));

        let target = self.write(|w| {
            let mut state = w.query::<(&Position, &EntityKind, &MinecraftEntityId)>();
            let version = w.resource::<VersionData>();
            state
                .iter(w)
                .filter_map(|(pos, kind, id)| {
                    let feet = Vec3d::new(pos.0.x, pos.0.y, pos.0.z);
                    // Cheap pre-filter before the exact ray-vs-box test: an
                    // entity whose *feet* are already further than the search
                    // radius plus a generous per-axis margin for its own
                    // hitbox cannot possibly be hit. Same shape as
                    // `tick_nearby_entities`'s box, sized off `search_limit`
                    // instead of the fixed push radius.
                    let margin = search_limit + 4.0;
                    if (feet.x - origin[0]).abs() > margin
                        || (feet.y - origin[1]).abs() > margin
                        || (feet.z - origin[2]).abs() > margin
                    {
                        return None;
                    }
                    let facts = version.entity_facts(&kind.0)?;
                    let dims = EntityDimensions::new(
                        facts.dimensions.width,
                        facts.dimensions.height,
                        0.6,
                    );
                    let aabb = dims.bounding_box(feet);
                    let t = ray_aabb(
                        origin,
                        dir,
                        search_limit,
                        [aabb.min_x, aabb.min_y, aabb.min_z],
                        [aabb.max_x, aabb.max_y, aabb.max_z],
                    )?;
                    Some((id.0, t))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(id, _)| id)
        });
        self.write(|w| w.resource_mut::<EntityRayTarget>().0 = target);
    }

    /// Distance fog for this frame: sized to the configured render distance
    /// normally (further specialised by the connected *dimension* — the
    /// Nether's fixed dense red haze, the End's near-black edge fade — when
    /// neither override below applies), and swapped for a short, dense
    /// water/lava fog while the player's eye is submerged.
    ///
    /// Selected from the bit-exact eye-in-fluid state (`FluidState`) the physics
    /// producer computes each tick, so the fog matches vanilla's submerged view
    /// rather than a locally-guessed boolean. Lava is checked before water,
    /// matching vanilla's lava-first submersion order, and both take priority
    /// over the dimension fog: standing in lava in the Nether still gets lava
    /// fog, not Nether fog.
    ///
    /// The dimension is read the same way `refresh_mesh_policy` reads it
    /// for `SkyDefault` — `net.shared_handle().get().and_then(|h|
    /// h.player().dimension)` — which is `None` before login and (per
    /// `docs/dimension-visuals.md`) stale after a portal trip until
    /// `lodestone-client`'s `Inner::apply` gets a `Respawned` arm; that staleness
    /// is a pre-existing condition of the dimension field itself; this reads it
    /// the same way every other dimension-conditioned decision in this crate
    /// does, no better and no worse.
    #[must_use]
    pub fn fog_settings(&self) -> lodestone_render::fog::FogSettings {
        let fluid = self.fluid_state();
        if fluid.under_lava() {
            return lava_fog();
        }
        if fluid.under_water() {
            return water_fog(self.config.render_distance);
        }
        let dimension = self
            .net
            .as_ref()
            .and_then(|net| net.shared_handle().get().and_then(|h| h.player().dimension));
        match dimension {
            Some(d) if d.namespace() == "minecraft" && d.path() == "the_nether" => {
                lodestone_render::fog::FogSettings::nether(self.config.render_distance)
            }
            Some(d) if d.namespace() == "minecraft" && d.path() == "the_end" => {
                lodestone_render::fog::FogSettings::the_end(
                    self.config.render_distance,
                    crate::gpu::FOG_START_FRACTION,
                )
            }
            _ => fog_for_render_distance(self.config.render_distance),
        }
    }

    /// The progressive-mining crack to draw on the targeted block this frame, or
    /// `None` when no dig is in progress.
    ///
    /// The stage is the client predictor's own `getDestroyStage` (`0..=9`); the
    /// block state id must be in the *same* id space the model atlas was built
    /// from, so on a live server it is read from the client-owned world
    /// (`NetClient::block_at`) — not [`block_at_world`](Self::block_at_world),
    /// which reads the offline demo world and would return air on a live join,
    /// leaving the resolver with no faces and drawing no crack. Progressive
    /// mining only runs on the live path (demo attack is a one-shot break that
    /// never drives the predictor), so `mining.destroy_stage()` is `-1` off a
    /// server and this returns `None` there regardless.
    ///
    /// The stage advances at the block's *own* rate: the predictor is fed the
    /// version's real per-state hardness (see
    /// [`drive_mining`](Self::drive_mining)), so the ten stages fill smoothly
    /// over the true break time and obsidian visibly crawls where dirt flickers
    /// past. An unbreakable block (`hardness == -1.0`, bedrock/barrier) has
    /// `progress_per_tick() == 0.0`, so progress never leaves `0.0`,
    /// `destroy_stage()` stays `-1` and this returns `None` — no crack is drawn
    /// at all, matching vanilla.
    #[must_use]
    pub fn crack_target(&self) -> Option<crate::gpu::CrackTarget> {
        let stage = self.mining(Mining::destroy_stage);
        if stage < 0 {
            return None;
        }
        let block = self.target()?.block;
        let state_id = if self.is_live() {
            let pos = BlockPos::new(block[0], block[1], block[2]);
            self.net.as_ref()?.block_at(pos)?
        } else {
            self.block_at_world(block)
        };
        Some(crate::gpu::CrackTarget {
            block,
            state_id,
            stage: (stage as u8).min(9),
        })
    }

    /// Break the currently targeted block (set it to air) and remesh. Returns
    /// whether a block was broken.
    ///
    /// This is the **demo-world** direct edit: it mutates the shell's offline
    /// world in place. On a live server the shell must instead route the dig
    /// through the server (see [`begin_attack`](Self::begin_attack)), or the
    /// break would be local-only and the server would restore the block on the
    /// next chunk update.
    pub fn break_block(&mut self) -> bool {
        let Some(hit) = self.target() else { return false };
        // Read the state *before* clearing the cell: the debris takes its
        // texture from the block that broke, and after `set_block_world` the
        // cell is air and that information is gone.
        let broken = self.block_at_world(hit.block);
        if self.set_block_world(hit.block, id::AIR) {
            // The demo world has no `ActionQueue` swing to piggy-back on (see
            // `drain_action_queue`), so the animation is started here. Without
            // this the offline demo — including every headless scene — could not
            // exercise the swing at all, which is the one world structurally
            // guaranteed not to.
            self.swing_hand();
            // Full-cube shape: vanilla derives the fragment grid from the
            // block's outline shape, which the shell does not carry, so debris
            // from a slab or fence fills the whole cell rather than hugging the
            // model.
            self.particles_mut(|p| p.destroy_block(hit.block, broken, [1.0; 3]));
            self.remesh_around(hit.block);
            self.set_target(None);
            true
        } else {
            false
        }
    }

    /// Begin an attack (left-click / attack button pressed).
    ///
    /// Vanilla's `Minecraft.startAttack` (`Minecraft.java:1603-1672`) switches
    /// on `hitResult.getType()` and swings the arm **unconditionally after the
    /// switch**, on every arm of it, miss included:
    ///
    /// * `ENTITY` — `this.gameMode.attack(player, entity)`, i.e. send the
    ///   attack.
    /// * `BLOCK`, and the block is *not* air — `startDestroyBlock`, i.e. begin
    ///   mining. (Vanilla deliberately **falls through** to `MISS` when the
    ///   block at `hitResult`'s position is air; this shell's `target()`
    ///   never reports a hit on an air cell in the first place — the ray only
    ///   stops at a *solid* cell — so that fallthrough has no case to cover
    ///   here.)
    /// * `MISS` (or no target at all) — nothing happens server-side, but the
    ///   arm still swings.
    ///
    /// Before this fix, only the `BLOCK`-with-a-dig-that-actually-starts arm
    /// ever reached [`Self::swing_hand`] (through `drive_mining`'s own queued
    /// `SwingArm`, see `drain_action_queue`'s docs) — so punching air, an
    /// entity, or empty space produced no animation at all (issue #72). This
    /// method is the one place all three branches now funnel through.
    ///
    /// `case ENTITY` takes priority over `case BLOCK`: [`EntityRayTarget`] is
    /// already the nearer of an entity-or-block pick (see
    /// [`Self::update_entity_target`]'s docs), so a `Some` there means mining
    /// must not start on this click even when [`RayTarget`] also holds a
    /// block.
    ///
    /// # What is deliberately not modelled here
    ///
    /// Vanilla's `attackStrengthTicker`/`getAttackStrengthScale` cooldown, the
    /// crit condition and the sweep-attack condition are real per-hit vanilla
    /// mechanics, but every one of them exists only to scale **local** sound/
    /// particle feedback and the crosshair cooldown indicator — the damage
    /// number itself is server-authoritative (the wire `Attack` packet
    /// carries only the target id, no damage or strength scalar; see
    /// `EntityInteraction::Attack`'s encoding in
    /// `crates/protocol/v770/src/adapter.rs`). None of those consumers exist
    /// in this shell yet: the crosshair indicator is `hud.rs`'s (held by
    /// another agent), and sweep/crit sound-and-particle feedback is
    /// `entities.rs`/asset work, also out of this file's scope. Building a
    /// ticker nothing reads would be exactly the unconsumed-island class
    /// `CLAUDE.md`'s core rule warns about, so it stays unbuilt rather than
    /// built and orphaned — whoever adds the crosshair pip or the sweep sound
    /// is the right owner for it, alongside the half it feeds.
    pub fn begin_attack(&mut self) {
        if self.is_live() {
            self.begin_attack_live();
        } else {
            self.begin_attack_demo();
        }
    }

    /// The demo-world half of [`Self::begin_attack`]: break the targeted
    /// block if there is one ([`Self::break_block`] already swings on
    /// success), or swing on a miss — the offline mirror of vanilla's
    /// unconditional swing. The demo ECS holds no networked entities (see
    /// [`Self::update_entity_target`]'s docs), so there is no `case ENTITY` to
    /// take here; only `BLOCK` vs `MISS`.
    fn begin_attack_demo(&mut self) {
        if !self.break_block() {
            self.swing_hand();
        }
    }

    /// The live half of [`Self::begin_attack`]. See that method's docs for the
    /// three-way switch this implements.
    fn begin_attack_live(&mut self) {
        if self.is_dead() {
            return;
        }
        if let Some(entity_id) = self.entity_target() {
            self.attack_entity(entity_id);
            self.swing_hand();
            return;
        }
        if self.target().is_some() {
            // Unchanged from before this fix: arms the hold-to-mine loop.
            // `drive_mining` itself queues the `SwingArm` the instant a dig
            // actually starts, through the same `ActionQueue`/
            // `drain_action_queue` funnel every other tick-driven swing uses.
            self.write(|w| w.resource_mut::<Attacking>().0 = true);
            return;
        }
        // MISS: no block, no entity. Vanilla still swings.
        self.swing_hand();
    }

    /// The entity [`EntityRayTarget`] currently names, if any — the live
    /// left-click's attack target.
    #[must_use]
    pub fn entity_target(&self) -> Option<i32> {
        self.read(|w| w.resource::<EntityRayTarget>().0)
    }

    /// Send the serverbound attack for `entity_id` — vanilla's
    /// `MultiPlayerGameMode.attack`'s outbound half. Lowers to
    /// `ClientAction::InteractEntity { interaction: EntityInteraction::Attack,
    /// .. }`, which the v770 adapter already encodes as the dedicated `Attack`
    /// packet (26.2 split entity-attack out of the old combined interact
    /// packet; see `crates/protocol/v770/src/adapter.rs`'s `InteractEntity`
    /// arm) — this method is the first caller that ever constructs the
    /// variant; the encoder was previously dead, unused code.
    ///
    /// Sent directly, like [`Self::use_item_live`]'s two sends, rather than
    /// queued through [`ActionQueue`]: that queue only drains inside the tick
    /// loop (see `crate::interact`'s "how to change it"), and an attack is a
    /// discrete click event, not a per-tick one.
    fn attack_entity(&mut self, entity_id: i32) {
        // The same tick-driven intent `use_item_live` reads for its own
        // sneaking bit, so a sneak-attack cannot disagree with what the wire
        // already told the server this tick's crouch state is.
        let sneaking = self.movement_intent().sneak;
        if let Some(net) = &self.net {
            net.send_action(ClientAction::InteractEntity {
                entity_id,
                interaction: EntityInteraction::Attack,
                sneaking,
            });
        }
    }

    /// End an attack (attack button released). Aborts a live dig in progress so
    /// the server stops mining; a no-op on the demo world.
    pub fn end_attack(&mut self) {
        if !self.is_live() {
            return;
        }
        let actions = self.write(|w| {
            w.resource_mut::<Attacking>().0 = false;
            w.resource_mut::<MiningPredictor>().0.stop()
        });
        // Sent directly rather than queued: `ActionQueue` is only drained inside
        // the tick loop, so a release on a frame that runs no tick would sit for
        // up to 50 ms before the `ABORT` reached the server. See
        // `crate::interact`'s "how to change it".
        if let Some(net) = &self.net {
            for action in actions {
                net.send_action(action);
            }
        }
    }

    /// Use the held item on the targeted block (use button pressed). On a live
    /// server this lowers the click into the server's `use_item_on` action
    /// through the placement predictor; on the demo world it places directly.
    pub fn use_item(&mut self) {
        if self.is_live() {
            self.use_item_live();
        } else {
            self.place_block();
        }
    }

    /// Lower a live right-click into the server's `use_item_on` action.
    ///
    /// The shell does not carry the held item or classify blocks — the server
    /// is authoritative: it places whatever is in the selected hotbar slot and
    /// re-runs the interact-vs-place decision itself. [`Placement::use_on`]
    /// returns the action to send in *every* branch, so the shell sends it
    /// unconditionally (with a proper prediction sequence) and lets the server
    /// decide, exactly as vanilla does. Because the server owns the sneak state
    /// derived from the wire, the crouch input must have been sent (see
    /// [`send_player_input`](Self::send_player_input)) for a sneak-placement
    /// against a chest/door to suppress the interaction.
    fn use_item_live(&mut self) {
        if self.is_dead() {
            return;
        }
        let Some(hit) = self.target() else { return };
        let clicked = BlockPos::new(hit.block[0], hit.block[1], hit.block[2]);
        let face = face_from_normal(hit.normal);
        let cursor = face_center_cursor(hit.normal);
        // The intent this tick's physics ran on — the same one
        // `lodestone_controller::ecs::send_player_input` derived the wire's shift
        // bit from, so the local decision and the server's cannot disagree. This
        // used to re-read the keyboard, which was frame-granular; vanilla is
        // tick-granular here too (`Minecraft.handleKeybinds` runs in the tick).
        let sneaking = self.movement_intent().sneak;
        let ctx = UseOnContext {
            hand: Hand::Main,
            clicked,
            face,
            cursor,
            inside_block: false,
            rotation: Rotation::new(self.player().yaw, self.player().pitch),
            sneaking,
            has_item_in_hand: true,
            placing: None,
            orientation: OrientationKind::Fixed,
        };
        let (UseOnDecision::Interact { action }
        | UseOnDecision::Place { action, .. }
        | UseOnDecision::Nothing { action }) = self.write(|w| {
            w.resource_mut::<PlacementPredictor>()
                .0
                .use_on(&ctx, &ServerAuthoritativeWorld)
        });
        if let Some(net) = &self.net {
            net.send_action(action);
            net.send_action(ClientAction::SwingArm { hand: Hand::Main });
        }
        // This swing bypasses `ActionQueue` (the two sends above go straight to
        // the socket so their wire order is fixed), so it also bypasses
        // `drain_action_queue`'s hook and has to start the animation itself.
        // Unconditional, not inside the `if let` above: the animation is
        // client-side and does not need a socket.
        self.swing_hand();
    }

    /// Place [`PLACE_BLOCK`] against the targeted face on the **demo world**, if
    /// the cell is empty and doesn't intersect the player. Returns whether a
    /// block was placed. The live path uses [`use_item`](Self::use_item) instead
    /// so the server actually hears the placement.
    pub fn place_block(&mut self) -> bool {
        let Some(hit) = self.target() else { return false };
        let pos = hit.place_position();
        let cell_empty = {
            let store = self.chunk_world();
            let world = store.read();
            let view = WorldCollision::new(&world);
            view.block_at(pos[0], pos[1], pos[2]) == id::AIR
        };
        if !cell_empty || self.block_intersects_player(pos) {
            return false;
        }
        if self.set_block_world(pos, PLACE_BLOCK) {
            self.remesh_around(pos);
            // Demo-world placement, same reasoning as `break_block`.
            self.swing_hand();
            true
        } else {
            false
        }
    }

    fn block_intersects_player(&self, block: [i32; 3]) -> bool {
        let bb = self.player().bounding_box(&self.profile());
        let (x0, y0, z0) = (
            f64::from(block[0]),
            f64::from(block[1]),
            f64::from(block[2]),
        );
        bb.max_x > x0
            && bb.min_x < x0 + 1.0
            && bb.max_y > y0
            && bb.min_y < y0 + 1.0
            && bb.max_z > z0
            && bb.min_z < z0 + 1.0
    }

    /// Advance the particle simulation one 20 Hz tick.
    ///
    /// Particles collide against the same view the player does, so debris rests
    /// on the terrain it fell onto rather than sinking through it. On the live
    /// path the column may not have streamed in; vanilla ticks particles
    /// regardless, so an absent view falls back to the offline world rather than
    /// freezing them.
    fn tick_particles(&mut self) {
        if self.vanilla_atlas.is_some() && self.net.is_some() && self.collide_against_live_world {
            if let Some(view) = self.live_collision() {
                // `O(live particles)`, so the emitter comes out of the `World`
                // first — the same reason `extract_particles` does it.
                self.with_particles_unlocked(|p| p.tick(&view));
                return;
            }
        }
        // The chunk guard is taken *inside* `f`, i.e. with no `World` guard held,
        // so the two are never held simultaneously and there is no order to get
        // wrong. This used to be written inside-out (`World` guard outside, chunk
        // guard inside) to obey `EcsHandle`'s rule 3, because the obvious spelling
        // — take the chunk read guard, then reach for the emitter — was
        // `chunks → World`, the one order that can ABBA against the net thread.
        // Holding neither across the other retires that hazard rather than
        // navigating it.
        let store = self.chunk_world();
        self.with_particles_unlocked(|p| {
            let world = store.read();
            p.tick(&WorldCollision::new(&world));
        });
    }

    /// Rebuild this frame's particle instances for `camera` and report what
    /// happened, so a silent "simulating fine, drawing nothing" is visible in
    /// the HUD rather than invisible.
    pub fn extract_particles(&mut self, camera: &Camera) -> ParticleFrame {
        // The same alpha every other interpolated draw uses, rather than a
        // second computation of it -- two frame alphas that drift apart show up
        // as particles lagging the terrain by a fraction of a tick.
        let partial = self.clock().interp_alpha;
        // Light is sampled from the live world when there is one. A `None` here
        // is not darkness: `ParticleEngine::extract` substitutes full sky light,
        // matching how the demo terrain is meshed.
        let light: Box<dyn Fn(i32, i32, i32) -> Option<u32>> = match self.net.as_ref() {
            Some(net) => {
                let dims = net.world_dimensions();
                // An **owned** `SharedHandle` (an `Arc<OnceLock<_>>`), not a borrow
                // of `self.net`. That is what lets the whole extract go through
                // `with_particles_unlocked`: a closure borrowing `self` cannot be
                // passed to a `&mut self` method, which is exactly why this
                // function used to take the write guard by hand and hold it across
                // every per-particle light lookup.
                let handle = net.shared_handle();
                Box::new(move |x, y, z| {
                    let dims = dims?;
                    let section = (y - dims.min_y).div_euclid(16);
                    if section < 0 || section >= dims.section_count() as i32 {
                        return None;
                    }
                    // `sections_and_light_at` takes `lodestone_client::ChunkPos`,
                    // which is a *different type* from the `lodestone_world`
                    // one imported at the top of this file (see mesher.rs:224).
                    let pos = lodestone_client::ChunkPos {
                        x: x.div_euclid(16),
                        z: z.div_euclid(16),
                    };
                    // Light section `i` covers block section `i-1`, so a caller
                    // for block section `n` asks for light section `n+1`. This
                    // offset is deliberate, not a bug to "align".
                    let got = handle
                        .get()?
                        .sections_and_light_at(&[(pos, section as usize, section as usize + 1)]);
                    let (_, light) = got.into_iter().next()?;
                    let light = light?;
                    let ly = (y - dims.min_y).rem_euclid(16) as usize;
                    let lx = x.rem_euclid(16) as usize;
                    let lz = z.rem_euclid(16) as usize;
                    // Vanilla's `LightTexture.pack`: block light at bit 4, sky
                    // light at bit 20. The particle shader reproduces the
                    // terrain term `0.2 + 0.8 * max(sky, block)` from these.
                    Some(u32::from(light.block_at(lx, ly, lz)) << 4
                        | u32::from(light.sky_at(lx, ly, lz)) << 20)
                })
            }
            None => Box::new(|_, _, _| None),
        };
        // **This used to be the longest `World` guard hold in the process.** It
        // took the write guard by hand and held it across the whole extract *and*
        // every per-particle invocation of `light` above — one chunk-store lock
        // acquisition per live particle, with the `World` write-locked throughout.
        // That was order-legal (`World → chunks`, rule 3) and unbounded: the hold
        // grew with particle volume, i.e. precisely during rain and mass block
        // breaks, and per `lodestone_ecs::EcsHandle` an ingest write waits behind
        // it while the driver task that owns the socket is blocked.
        //
        // Now the emitter leaves the `World` first, so `light` is called with no
        // guard held and the hold is two resource moves regardless of particle
        // count. Measured, not argued —
        // `extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work`
        // bounds it against the call's own wall time, and its negative control
        // reproduces the shape above and fails that bound.
        self.with_particles_unlocked(|p| p.extract(camera, partial, &light))
    }

    /// This frame's particle instances, ready for upload.
    ///
    /// Owned rather than borrowed since §4.1(c). The alternative — handing back a
    /// mapped read guard — would keep the one `World` read-locked for the whole GPU
    /// upload, which is exactly the "ingest stalls the frame" failure this change
    /// has to avoid, only inverted: the frame would stall ingest. A `memcpy` of a
    /// few thousand POD instances is the cheaper end of that trade.
    #[must_use]
    pub fn particle_instances(&self) -> Vec<ParticleInstance> {
        self.read(|w| w.resource::<ParticleSim>().0.instances().to_vec())
    }

    /// The number of fixed simulation ticks (20/s) elapsed. Drives animated
    /// block sprites, whose vanilla frame timing is measured in game ticks; the
    /// renderer samples each animation at this tick each frame.
    #[must_use]
    pub fn tick_count(&self) -> u64 {
        self.clock().ticks
    }

    /// The block state id at a world position, or air when the column is not
    /// loaded or the y is outside the build range.
    fn block_at_world(&self, block: [i32; 3]) -> u32 {
        let pos = ChunkPos {
            x: block[0].div_euclid(16),
            z: block[2].div_euclid(16),
        };
        let store = self.chunk_world();
        let world = store.read();
        let Some(chunk) = world.get(pos) else {
            return id::AIR;
        };
        let col = &chunk.column;
        if block[1] < col.min_y() || block[1] >= col.max_y() {
            return id::AIR;
        }
        lodestone_world::BlockVolume::block(
            col,
            block[0].rem_euclid(16) as usize,
            block[1],
            block[2].rem_euclid(16) as usize,
        )
    }

    /// Write a block into the chunk store. Offline-world editing only: on a live
    /// session the server is authoritative and the edit arrives as a block-update
    /// packet.
    ///
    /// There is nothing to invalidate afterwards. Before Stage 4 this was the one
    /// write path to `Sim.world` and therefore the one place the cached offline
    /// collision clone had to be cleared by hand — a missed clear reading as "I
    /// mined the block but still cannot walk through it". The collision source now
    /// reads the store itself, so the rule is gone rather than merely obeyed.
    fn set_block_world(&mut self, block: [i32; 3], value: u32) -> bool {
        let pos = ChunkPos {
            x: block[0].div_euclid(16),
            z: block[2].div_euclid(16),
        };
        let store = self.chunk_world();
        let mut world = store.write();
        let Some(chunk) = world.get_mut(pos) else {
            return false;
        };
        let col = &mut chunk.column;
        if block[1] < col.min_y() || block[1] >= col.max_y() {
            return false;
        }
        col.set_block(
            block[0].rem_euclid(16) as usize,
            block[1],
            block[2].rem_euclid(16) as usize,
            value,
        );
        true
    }

    /// Re-snapshot and re-schedule the section holding `block`, plus any
    /// neighbour section that shares the boundary the block sits on (a face on a
    /// section edge changes the neighbour's mesh via culling/AO). Sections that
    /// became all-air are queued for GPU removal instead.
    fn remesh_around(&mut self, block: [i32; 3]) {
        let Some(extent) = self.chunk_world().extent() else {
            return;
        };
        let (min_y, section_count) = (extent.min_y, extent.section_count);
        let cx = block[0].div_euclid(16);
        let cz = block[2].div_euclid(16);
        let lx = block[0].rem_euclid(16);
        let lz = block[2].rem_euclid(16);
        let si = (block[1] - min_y).div_euclid(16);
        let ly = (block[1] - min_y).rem_euclid(16);

        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if (dx == -1 && lx != 0) || (dx == 1 && lx != 15) {
                        continue;
                    }
                    if (dy == -1 && ly != 0) || (dy == 1 && ly != 15) {
                        continue;
                    }
                    if (dz == -1 && lz != 0) || (dz == 1 && lz != 15) {
                        continue;
                    }
                    let nsi = si + dy;
                    if nsi < 0 || nsi as usize >= section_count {
                        continue;
                    }
                    self.remesh_section(cx + dx, cz + dz, nsi as usize, min_y, section_count);
                }
            }
        }
    }

    /// Re-snapshot and re-schedule one section. A section that snapshots to
    /// nothing is queued for GPU removal rather than left showing stale geometry.
    ///
    /// One path, not two: before Stage 4 this branched on `vanilla_atlas &&
    /// net && world_dimensions` to pick which of the two `World`s to read.
    fn remesh_section(
        &mut self,
        cx: i32,
        cz: i32,
        si: usize,
        min_y: i32,
        section_count: usize,
    ) {
        let key = SectionKey { cx, cz, si, min_y };
        self.terrain_and_world(|store, terrain| terrain.mesh_section(store, key, section_count));
    }

    /// Re-mesh after a server-authoritative edit inside section
    /// `(sx, sy, sz)`, where `blocks` are the section-relative coordinates of
    /// every changed cell.
    ///
    /// Section granularity, not column: this is the signal every redstone tick
    /// carries, and a whole-column re-mesh is ~24 sections each snapshotting a
    /// 27-section neighbourhood. A cell on a section face also dirties the
    /// section across that face — culling, AO and fluid corner heights all read
    /// across the boundary — so an edit at local x=15 fixes the neighbouring
    /// column's seam too, which a column-scoped signal cannot express. Keys are
    /// deduplicated first, so a 4096-cell update still submits at most 27
    /// snapshots.
    fn remesh_changed_blocks(&mut self, sx: i32, sy: i32, sz: i32, blocks: &[[u8; 3]]) {
        let Some(extent) = self.chunk_world().extent() else {
            return;
        };
        let base_si = extent.min_y.div_euclid(16);
        for (nsx, nsy, nsz) in dirty_sections_for_blocks(sx, sy, sz, blocks) {
            let si = nsy - base_si;
            if si < 0 || si as usize >= extent.section_count {
                continue;
            }
            self.remesh_section(
                nsx,
                nsz,
                si as usize,
                extent.min_y,
                extent.section_count,
            );
        }
    }

    /// Handle a chunk-arrival signal for `(cx, cz)`: mesh that column now, and
    /// queue its **loaded horizontal neighbours** for a boundary re-mesh.
    ///
    /// A section's geometry is a function of its whole 3×3×3 neighbourhood, so a
    /// column that was meshed while `(cx, cz)` was still absent baked its seam
    /// against air. Left alone that is permanent, and it is exactly what a
    /// play-test sees: **water grows a falling "wall" at every chunk border**
    /// (the neighbour cell reads as no-fluid, so the side face is emitted and the
    /// corner heights collapse), plus wrong cross-chunk AO and stray culled
    /// faces. The tell that it is a staleness bug and not a mesher bug is that
    /// breaking any block in the column fixes it — [`Sim::remesh_around`] already
    /// re-meshes neighbours.
    ///
    /// The centre column meshes immediately (load responsiveness); the eight
    /// neighbours are coalesced into `TerrainMesh::dirty_columns` and drained on a
    /// budget by the `heal_dirty_columns` system, so a spiral load re-meshes each
    /// column a small constant number of times instead of nine.
    fn on_column_arrived(&mut self, cx: i32, cz: i32) {
        self.mark_column_dirty(cx, cz);
        self.terrain_and_world(|store, terrain| terrain.mark_neighbours_dirty(store, cx, cz));
    }

    /// Handle a `ChunkLoaded` / [`NetUpdate::Chunk`] dirty-region signal: the
    /// column at `(cx, cz)` changed, so re-mesh every section it holds.
    ///
    /// **One path since Stage 4.** This used to be two, chosen by
    /// `vanilla_atlas.is_some() && net.is_some() && world_dimensions().is_some()`:
    /// one reading the client-owned world through `NetClient`, one reading `Sim`'s
    /// own. With a single [`ChunkWorld`] store there is one world to read, and the
    /// only thing the old guard genuinely encoded — *is the mesh classifier's
    /// block-id space the store's?* — survives as `MeshPolicy::id_spaces_agree`.
    /// Light stays server-authoritative on the live path: nothing here recomputes
    /// it (that would overwrite the server's seam-complete cross-chunk light with a
    /// partial result — a divergence bug). Multiplayer *consumes* light;
    /// singleplayer computes it.
    fn mark_column_dirty(&mut self, cx: i32, cz: i32) {
        self.terrain_and_world(|store, terrain| terrain.mesh_column(store, cx, cz));
    }

    fn poll_net(&mut self) {
        // Collect owned updates first so the immutable borrow of `self.net`
        // ends before the loop — the sound arms need `&mut self.audio` and (for
        // entity sounds) a fresh read of `self.net` for positions, neither of
        // which can coexist with a borrow held across the loop.
        // Adopt the client's chunk store the first frame a handle exists — this
        // is where the process comes to have exactly one `lodestone_world::World`
        // (`docs/chunk-world-resource.md`). Idempotent and a pointer compare
        // thereafter.
        self.adopt_live_world();
        // The connected dimension (and therefore the absent-sky policy) can change
        // mid-session on a portal trip, so the mesh policy is refreshed every poll
        // rather than only at attach.
        self.refresh_mesh_policy();
        let updates = match &self.net {
            Some(net) => net.poll(),
            None => return,
        };
        for update in updates {
            match update {
                NetUpdate::Connecting => {
                    self.status = "connecting…".into();
                    self.set_phase(SessionPhase::Connecting);
                }
                NetUpdate::LoggedIn { entity_id } => {
                    // The id is *not* recorded here. `ClientEvent::Login` folds it
                    // into the `ServerEntityId` component (and into
                    // `EntityIndex`) on the net thread, in the same `World` this
                    // `Sim` reads — a second write here would be the duplicate the
                    // vitals collapse deleted. It stays in the status line because
                    // that is a human-readable string, not state.
                    self.status = format!("connected (entity {entity_id})");
                    self.set_phase(SessionPhase::Connected);
                }
                NetUpdate::Chunk { x, z } => {
                    // §12.24 dirty-region signal: no block data travels on the
                    // event — the client applies decoded chunks to its own
                    // `World`, which we read via `NetClient::sections_and_light_at`
                    // (+ `world_dimensions` for geometry). `mark_column_dirty`
                    // meshes live columns through the vanilla classifier.
                    self.on_column_arrived(x, z);
                }
                NetUpdate::SectionBlocks { x, y, z, blocks } => {
                    // A server-authoritative edit inside one loaded section.
                    // Re-mesh at *section* granularity, not the whole column:
                    // the same signal carries every redstone tick, and a column
                    // re-mesh is ~24 sections × a 27-section snapshot each.
                    // `remesh_around` also handles the boundary case, so a break
                    // at x=15 dirties the neighbouring column's face too.
                    self.remesh_changed_blocks(x, y, z, &blocks);
                }
                NetUpdate::Teleport {
                    pos,
                    rotation,
                    flags,
                } => {
                    // Adopt the server's authoritative placement. The shell runs
                    // its own physics and streams an optimistic position every
                    // tick from the demo spawn; on a server whose spawn is far
                    // from the origin the server ignores that bogus claim and
                    // keeps us at the real spawn, streaming chunks there. Snap the
                    // camera onto it (resolving any relative components against the
                    // current pose) so it sits where the world actually is instead
                    // of stranded over the unmeshed demo platform. `prev_position`
                    // is moved with it so the frame interpolator does not smear the
                    // camera across the teleport.
                    let placed = self.player_mut(|player| {
                    let base = player.position;
                    player.position = Vec3d::new(
                        if flags.relative_x {
                            base.x + pos.x
                        } else {
                            pos.x
                        },
                        if flags.relative_y {
                            base.y + pos.y
                        } else {
                            pos.y
                        },
                        if flags.relative_z {
                            base.z + pos.z
                        } else {
                            pos.z
                        },
                    );
                    player.yaw = if flags.relative_yaw {
                        player.yaw + rotation.yaw
                    } else {
                        rotation.yaw
                    };
                    player.pitch = if flags.relative_pitch {
                        player.pitch + rotation.pitch
                    } else {
                        rotation.pitch
                    };
                    player.velocity = Vec3d::ZERO;
                    // A teleport is not a fall. Vanilla resets fall distance on
                    // every position snap, and this one handler covers server
                    // corrections, respawn and every teleport packet — so
                    // without it, a corrective teleport mid-fall leaves the
                    // accumulated distance behind to feed `maybeBackOffFromEdge`
                    // (and, later, fall damage) as though the fall continued.
                    player.reset_fall_distance();
                    player.position
                    });
                    self.set_prev_position(placed);
                    self.teleport_count += 1;
                }
                NetUpdate::Chat { text, player } => {
                    // Resolve translate nodes (death messages, join/leave, …) to
                    // words once, at arrival, against the language table — so the
                    // stored scrollback and the log line both read as prose, not
                    // raw keys like `entity.minecraft.spider`.
                    let text = self.resolve_text(&text);
                    tracing::info!(target: "chat", "{}", text.to_legacy_string());
                    // Stamped with the driver's own clock, which is why the log and
                    // the clock had to move to the ECS together (Stage 3 deferred
                    // both for exactly this reason). `local` is the session entity,
                    // so a `SessionChat` that somehow went missing drops the line
                    // rather than panicking mid-poll.
                    let now = self.clock().secs;
                    let local = self.local;
                    self.write(|w| {
                        if let Some(mut chat) = w.get_mut::<SessionChat>(local) {
                            if player {
                                chat.0.push_player(
                                    text,
                                    lodestone_game::chat::MessageTrust::NotSecure,
                                    now,
                                );
                            } else {
                                chat.0.push_system(text, now);
                            }
                        }
                    });
                }
                NetUpdate::BlockDestroyed { pos, state } => {
                    // The live counterpart of the offline `break_block` emit.
                    // It is driven by the server rather than by our own click
                    // because the server is authoritative about *whether* the
                    // block broke and *what* it was — a predicted break that the
                    // server rejects would otherwise throw debris off a block
                    // still standing there.
                    //
                    // Shape is a full cube for the same reason as the offline
                    // path: vanilla derives the fragment grid from the block's
                    // outline shape, which the shell does not carry. Debris from
                    // a slab or a fence therefore fills the whole cell rather
                    // than hugging the model.
                    self.particles_mut(|p| {
                        p.destroy_block([pos.x, pos.y, pos.z], state, [1.0, 1.0, 1.0]);
                    });
                }
                NetUpdate::Particles {
                    kind,
                    long_distance,
                    pos,
                    offset,
                    max_speed,
                    count,
                } => {
                    // `ClientLevel.doAddParticle`'s render cutoff: a particle
                    // farther than 32 blocks (`1024.0` == `32.0` squared) from
                    // the viewer is dropped unless the packet set the
                    // override-limiter flag (`long_distance` here). Vanilla
                    // measures from the render camera; the player's feet
                    // position is close enough for a cutoff whose only
                    // visible effect is "does this puff bother rendering,"
                    // and it is what the rest of the shell's render-adjacent
                    // logic already keys off.
                    let feet = self.player().position;
                    let dx = pos.x - feet.x;
                    let dy = pos.y - feet.y;
                    let dz = pos.z - feet.z;
                    let within_cutoff =
                        long_distance || dx.mul_add(dx, dy.mul_add(dy, dz * dz)) <= 1024.0;
                    if within_cutoff {
                        self.particles_mut(|p| {
                            p.spawn_particles(
                                &kind,
                                [pos.x, pos.y, pos.z],
                                [offset.x, offset.y, offset.z],
                                max_speed,
                                count,
                            );
                        });
                    }
                }
                // No `Health`/`Experience` arms, and no `NetUpdate` variants for
                // them either: the net thread folds `ClientEvent::HealthChanged`
                // and `ExperienceChanged` straight into the `Vitals`/`Xp`
                // components on `self.local`, so [`Self::health`], [`Self::food`]
                // and [`Self::experience`] read what they always read and this
                // side has nothing left to do. Death is still a separate event
                // ([`NetUpdate::Death`], which the library always emits on the
                // death packet); health reaching zero is not itself a session
                // event and does not unload chunks.
                NetUpdate::Death => {
                    // Death is a state the shell rides through, not the end of the
                    // session. The client library's `RespawnPolicy::Automatic`
                    // already answers the death packet with a `ClientAction::
                    // Respawn`, so the shell does not send anything here: it marks
                    // itself dead (which freezes movement in `step`) and stays
                    // Connected, waiting for the server-confirmed respawn. The new
                    // position rides in on the placement teleport that follows
                    // `NetUpdate::Respawned`, whose arm snaps `prev_position` too.
                    if self.recover_from_death {
                        self.set_dead(true);
                        self.status = "you died — respawning…".into();
                    } else {
                        // Retained only as the live death gate's negative control:
                        // the pre-fix behaviour that declared the session over and
                        // stranded the client on the death screen forever.
                        self.status = "server: died".into();
                        self.set_phase(SessionPhase::Ended("player died".into()));
                    }
                }
                NetUpdate::Respawned => {
                    // The server confirmed the respawn: the player is alive again.
                    // The fresh spawn position arrives in the placement teleport
                    // that immediately follows this event; the `NetUpdate::Teleport`
                    // arm snaps `position` and `prev_position` together, so the
                    // frame interpolator never smears the camera from the death
                    // site across the world to the new spawn (the same class of
                    // bug as the original far-spawn camera gap).
                    self.set_dead(false);
                    let local = self.local;
                    self.write(|w| {
                        if let Some(mut count) = w.get_mut::<RespawnCount>(local) {
                            count.0 += 1;
                        }
                    });
                    self.status = "respawned".into();
                }
                NetUpdate::Sound {
                    name,
                    category,
                    pos,
                    volume,
                    pitch,
                    seed,
                } => {
                    if let Some(audio) = &mut self.audio {
                        let pos = glam::Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32);
                        audio.play_sound(&name, category, pos, volume, pitch, seed);
                    }
                }
                NetUpdate::EntitySound {
                    name,
                    category,
                    entity_id,
                    volume,
                    pitch,
                    seed,
                } => {
                    // Resolve the entity's live position *before* borrowing the
                    // audio engine mutably (disjoint, sequential borrows).
                    let pos = self.entity_sound_position(entity_id);
                    if let Some(audio) = &mut self.audio {
                        audio.play_entity_sound(&name, category, pos, volume, pitch, seed);
                    }
                }
                // Only the local player's effects are folded: they feed both the
                // physics view ([`PlayerState::effects`]) and the display view
                // ([`Sim::hud_effects`]). Entity-scoped effects are filtered here
                // rather than in `net::forward`, keeping the wire event
                // entity-agnostic.
                NetUpdate::EffectApplied {
                    entity_id,
                    effect,
                    amplifier,
                    duration_ticks,
                    ambient,
                    show_icon,
                } => {
                    if self.server_entity_id() == Some(entity_id) {
                        let local = self.local;
                        self.write(|w| {
                            if let Some(mut state) = w.get_mut::<PhysicsState>(local) {
                                state.0.effects.apply(&effect, amplifier);
                            }
                            if let Ok(id) =
                                lodestone_model::Identifier::new("minecraft", effect.as_str())
                                && let Some(mut effects) = w.get_mut::<HudEffects>(local)
                            {
                                effects.0.apply(lodestone_game::effect::StatusEffect {
                                    id,
                                    amplifier: u8::try_from(amplifier).unwrap_or(u8::MAX),
                                    duration_ticks,
                                    ambient,
                                    show_particles: true,
                                    show_icon,
                                });
                            }
                        });
                    }
                }
                NetUpdate::EffectRemoved { entity_id, effect } => {
                    if self.server_entity_id() == Some(entity_id) {
                        let local = self.local;
                        self.write(|w| {
                            if let Some(mut state) = w.get_mut::<PhysicsState>(local) {
                                state.0.effects.remove(&effect);
                            }
                            if let Ok(id) =
                                lodestone_model::Identifier::new("minecraft", effect.as_str())
                                && let Some(mut effects) = w.get_mut::<HudEffects>(local)
                            {
                                effects.0.remove(&id);
                            }
                        });
                    }
                }
                // The tab-list and scoreboard arms are *deleted*, not moved:
                // `lodestone_ecs::session`'s systems fold them inside the
                // client, and `Sim::sidebar`/`player_rows` read that one copy
                // through `NetClient`. Keeping a fold here as well is precisely
                // the two-sources-of-truth Stage 3 exists to remove.
                NetUpdate::TitleEvent(event) => {
                    let local = self.local;
                    self.write(|w| {
                        if let Some(mut title) = w.get_mut::<TitleOverlay>(local) {
                            let _ = title.0.apply(&event);
                        }
                    });
                }
                NetUpdate::ActionBar(text) => {
                    let local = self.local;
                    self.write(|w| {
                        if let Some(mut bar) = w.get_mut::<ActionBarOverlay>(local) {
                            bar.0.set(text);
                        }
                    });
                }
                NetUpdate::Disconnected(reason) => {
                    self.status = format!("disconnected: {reason}");
                    self.set_phase(SessionPhase::Ended(format!("disconnected: {reason}")));
                }
                NetUpdate::Error(e) => {
                    self.status = format!("net error: {e}");
                    self.set_phase(SessionPhase::Ended(format!("net error: {e}")));
                }
            }
        }
    }

    /// World-space origin for an entity-attached sound: the entity's live feet
    /// position raised half a block so the source sits at body centre. Falls
    /// back to the player's current position if the entity is unknown (so the
    /// sound is still heard rather than dropped) — the same "audible, not
    /// silent" preference the live gate encodes.
    fn entity_sound_position(&self, entity_id: i32) -> glam::Vec3 {
        if let Some(net) = &self.net
            && let Some(snap) = net
                .entity_snapshots()
                .into_iter()
                .find(|s| s.id == entity_id)
        {
            return snap.feet + glam::Vec3::new(0.0, 0.5, 0.0);
        }
        let p = self.player().position;
        glam::Vec3::new(p.x as f32, p.y as f32, p.z as f32)
    }

    /// Push the listener transform to the audio engine from the render camera.
    /// Called once per frame by [`crate::app`] with the exact interpolated
    /// camera it renders, so what the player hears matches what they see.
    pub fn set_audio_listener(&self, camera: &Camera) {
        if let Some(audio) = &self.audio {
            audio.set_listener(camera);
        }
    }

    fn refresh_stats(&mut self) {
        let player = self.player();
        self.stats.position = [player.position.x, player.position.y, player.position.z];
        self.stats.yaw = player.yaw;
        self.stats.pitch = player.pitch;
        let store = self.chunk_world();
        self.stats.chunk_count = store.len();
        self.stats.live_columns = self.net.as_ref().map_or(0, |n| n.loaded_chunks().len());
        self.stats.mesh_drops = self.terrain(|t| t.drops);
        self.stats.world_bytes = store.read().heap_bytes();
        self.stats.rss_bytes = process_rss_bytes();
        self.stats.frames_per_tick = self.frames_per_tick();
        self.stats.flying = self.flying();
        self.stats.target = self.target().map(|h| h.block);
        self.stats.status = self.status.clone();
    }

    /// The player's physics state with `position` replaced by the feet
    /// interpolated between the last two physics ticks — the "drawn" position
    /// every per-frame consumer of the player's own placement wants, rather
    /// than the raw tick-boundary value [`Self::player`] returns. Shared by
    /// [`Self::camera`] and [`Self::third_person_body_state`] so the eye and
    /// the third-person body it stands next to never disagree about where
    /// "here" is.
    #[must_use]
    fn interpolated_player(&self) -> PlayerState {
        let a = f64::from(self.clock().interp_alpha);
        let mut interp = self.player();
        let prev = self.prev_position();
        interp.position = Vec3d::new(
            prev.x + (interp.position.x - prev.x) * a,
            prev.y + (interp.position.y - prev.y) * a,
            prev.z + (interp.position.z - prev.z) * a,
        );
        interp
    }

    /// Build the **true first-person eye** camera for the given viewport
    /// aspect ratio, with the feet position interpolated between the last two
    /// physics ticks so motion stays smooth even though physics runs at a
    /// fixed 20 Hz. View angles are current (mouse-look is per-frame, matching
    /// vanilla).
    ///
    /// The pose's eye height is passed to [`build_camera`] explicitly, so the
    /// position handed to it is the player's real interpolated feet in every pose
    /// (`Avatar.java:22-36`: `0.4` swimming, `1.27` crouching, `1.62` standing).
    /// It used to be folded into the feet Y as a bias instead — arithmetically the
    /// same, but the argument was then not the feet whenever a non-standing pose
    /// was active. See `camera_rig.rs`'s module docs.
    ///
    /// This is also the ray origin for [`update_target`](Self::update_target)
    /// and the audio listener ([`Self::set_audio_listener`]'s caller in
    /// `app.rs`), **deliberately unmodified by third-person mode**: block
    /// interaction and hearing both originate from the real eye in vanilla,
    /// not from wherever a pulled-back camera happens to be. Only the actual
    /// render pass wants the third-person offset — see [`Self::render_camera`].
    #[must_use]
    pub fn camera(&self, aspect: f32) -> Camera {
        let interp = self.interpolated_player();
        build_camera(
            &interp,
            // The *camera's* eased eye, not `interp.eye_height` — see the field's
            // doc. Interpolating the entity's eye height would still snap, because
            // the value being interpolated between two ticks is itself the
            // post-snap one.
            self.eye_height_smoother.lerp(self.clock().interp_alpha),
            aspect,
            self.config.render_distance,
        )
    }

    /// Flips the camera mode (vanilla's `F5`): first person ↔ third person.
    ///
    /// This one bool is the entire "camera mode" state in this shell —
    /// [`RenderState::set_third_person_body_source`](crate::gpu::RenderState::set_third_person_body_source)'s
    /// own doc says the closure's `None`/`Some` split *is* the camera-mode
    /// toggle by design, and [`Self::render_camera`] /
    /// [`Self::third_person_body_state`] are exactly that closure's two
    /// halves: the same flag decides both, so they can never disagree about
    /// which mode is active this frame.
    pub fn toggle_third_person(&mut self) {
        self.third_person = !self.third_person;
    }

    /// The camera the frame is actually **drawn** from: [`Self::camera`]
    /// unmodified in first person, or that same eye pulled straight backward
    /// along its own view direction in third person — vanilla's real
    /// "back" algorithm, not a stand-in for it — clamped against live
    /// collision geometry so it never clips through a wall (see
    /// [`crate::camera_rig::collision_pullback`]).
    ///
    /// Reads whichever collision adapter [`Self::update_target`] would use
    /// (`LiveCollision` on a server, `WorldCollision` on the offline fixture),
    /// so a third-person camera respects the exact same geometry the player
    /// collides against. A live session whose own column has not streamed in
    /// yet (`Self::live_collision` returning `None`) has nothing real to
    /// clamp against, so this falls back to the desired distance unclamped
    /// rather than jamming the camera into the eye.
    #[must_use]
    pub fn render_camera(&self, aspect: f32) -> Camera {
        let eye = self.camera(aspect);
        if !self.third_person {
            return eye;
        }
        if self.is_live() {
            match self.live_collision() {
                Some(view) => third_person_camera(eye, true, &view),
                None => third_person_camera(eye, true, &NoCollision),
            }
        } else {
            let store = self.chunk_world();
            let world = store.read();
            let view = WorldCollision::new(&world);
            third_person_camera(eye, true, &view)
        }
    }

    /// The local player's own third-person body for this frame, or `None` in
    /// first person — exactly the value `app.rs` hands
    /// [`RenderState::set_third_person_body_source`](crate::gpu::RenderState::set_third_person_body_source)'s
    /// closure every frame.
    ///
    /// The walk cycle, **arm swing** and idle age come from [`Self::body_pose`],
    /// ticked once per physics tick the same way `entities.rs`'s `render_anim`
    /// drives one for a tracked network entity, and interpolated here for the
    /// current sub-tick alpha. Facing does **not** come from that pose,
    /// though: `body_yaw_deg`/`head_pitch_deg` are read straight off the
    /// interpolated player instead, so the avatar's own facing tracks the
    /// camera with no per-tick lag — the lag `EntityPose`'s body-yaw smoothing
    /// exists to model is a *third-party observer's* view of a remote entity,
    /// which does not apply to your own body.
    ///
    /// Two gaps, both left exactly where the equivalent gap already is
    /// elsewhere in this codebase rather than guessed at:
    /// * **Head yaw never diverges from body yaw** (`head_yaw_deg` is always
    ///   `0`): vanilla's independent head-turn-then-body-catches-up
    ///   (`LivingEntity.tickHeadTurn`) is not modelled for the local player
    ///   anywhere in this engine.
    /// * **`slim`/skin data**: [`ThirdPersonBodyState::slim`]'s own doc
    ///   already records that no real skin-model bit exists yet; `false`
    ///   reproduces the first-person arm's existing default.
    /// * **Equipment covers main hand, off hand, and all four armour
    ///   slots.** Main hand is the selected hotbar slot; off hand is native
    ///   inventory index `40`; the armour slots are native indices
    ///   `39/38/37/36` for head/chest/legs/feet (`lodestone_game::menu`'s own
    ///   table, `Menu::player`).
    #[must_use]
    pub fn third_person_body_state(&self) -> Option<ThirdPersonBodyState> {
        if !self.third_person {
            return None;
        }
        let partial_tick = self.clock().interp_alpha;
        let interp = self.interpolated_player();
        let feet = glam::Vec3::new(
            interp.position.x as f32,
            interp.position.y as f32,
            interp.position.z as f32,
        );
        let walk = self.body_pose.render(partial_tick);
        /// Native player-inventory index of the off-hand slot
        /// (`lodestone_game::menu`'s doc table: hotbar `0..=8`, off-hand `40`).
        const OFFHAND_NATIVE_INDEX: usize = 40;
        let menu = self.player_menu();
        let mut equipment = Vec::new();
        if let Some(loc) = menu
            .player_native(self.selected_slot())
            .and_then(|st| ResourceLocation::parse(&st.item().to_string()).ok())
        {
            equipment.push((EquipmentSlot::MainHand, loc));
        }
        if let Some(loc) = menu
            .player_native(OFFHAND_NATIVE_INDEX)
            .and_then(|st| ResourceLocation::parse(&st.item().to_string()).ok())
        {
            equipment.push((EquipmentSlot::OffHand, loc));
        }
        // Native player-inventory indices of the four armour slots
        // (`lodestone_game::menu::Menu::player`'s own table: menu slots
        // `5..=8` are head/chest/legs/feet at native indices `39/38/37/36` —
        // the native indices run backwards, feet-first).
        const ARMOUR_NATIVE_SLOTS: [(usize, EquipmentSlot); 4] = [
            (39, EquipmentSlot::Head),
            (38, EquipmentSlot::Chest),
            (37, EquipmentSlot::Legs),
            (36, EquipmentSlot::Feet),
        ];
        for (native, eq) in ARMOUR_NATIVE_SLOTS {
            if let Some(loc) = menu
                .player_native(native)
                .and_then(|st| ResourceLocation::parse(&st.item().to_string()).ok())
            {
                equipment.push((eq, loc));
            }
        }
        Some(ThirdPersonBodyState {
            feet,
            body_yaw_deg: interp.yaw,
            anim: AnimInput {
                head_yaw_deg: 0.0,
                head_pitch_deg: interp.pitch,
                limb_swing: walk.limb_swing,
                limb_swing_amount: walk.limb_swing_amount,
                // The self-avatar's *body* half of the swing:
                // `HumanoidModel.setupAttackAnimation`, via
                // `lodestone_render::entity_anim::Skeleton::pose`. The same scalar
                // the first-person arm pass polls through
                // `Sim::hand_swing_progress`, but a completely different pose
                // function — see `ThirdPersonBodyState`'s docs on why the two must
                // never share one.
                //
                // `walk.attack_anim` rather than `self.hand_swing_progress()`: both
                // are `body_pose.attack_anim_lerp(partial_tick)`, and this one is
                // already in hand from the `render` call above at the *same*
                // partial tick, so the arm and the body cannot drift by a frame.
                attack_anim: walk.attack_anim,
                age_ticks: walk.age,
                aggressive: false,
            },
            scale: 1.0,
            slim: false,
            equipment,
        })
    }
}

/// A [`CollisionView`] with no geometry at all, for
/// [`Sim::render_camera`]'s third-person pullback when no live collision
/// snapshot exists yet (the player's own column has not streamed in): there
/// is nothing real to clamp against, so the camera pulls back the full
/// desired distance rather than treating "no data" as "solid".
struct NoCollision;

impl CollisionView for NoCollision {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<lodestone_physics::Aabb>) {}
}

/// Radius, in blocks, within which [`Sim::tick_nearby_entities`] hands a
/// tracked entity to the crowd push as a candidate.
///
/// Vanilla queries `getPushableEntities(this, this.getBoundingBox())` — the
/// *un-inflated* player box — but `docs/entity-push.md`'s own wiring note is
/// explicit that "a generous neighbourhood is fine: candidates that fail a
/// gate contribute nothing". This is a coarse pre-filter, not the gate: the
/// real predicate is `lodestone_physics::push::pair_admitted` downstream, so a
/// too-large radius costs only a few wasted overlap tests while a too-small one
/// **silently drops real candidates** and no test can see it.
///
/// It was `4.0`, chosen for "a happy-ghast-sized neighbour" back when every
/// candidate was handed the player's own `0.6 × 1.8` box. Now that the census
/// supplies real dimensions that value is provably too small, and the bound
/// follows from the census maxima rather than from a guess:
///
/// - widest pusher is `ender_dragon` at `16.0`, and two boxes touch when their
///   centres are within `(0.6 + 16.0) / 2 = 8.3` — so x/z needs `>= 8.3`;
/// - tallest is `giant` at `12.0`, and this compares *feet* to *feet*, so a
///   giant whose feet are `12.0` below ours still overlaps — y needs `>= 12.0`.
///
/// `16.0` is the largest extent in the census and covers both with margin.
/// Deriving it programmatically from the census maxima, rather than restating
/// them here, is the remaining nit — see `docs/entity-push.md`.
const NEARBY_ENTITY_RADIUS: f64 = 16.0;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::config::{Config, Mode};
    use lodestone_ecs::player::SWIMMING_EYE_HEIGHT;

    fn test_config() -> Config {
        Config {
            mode: Mode::Headless,
            render_distance: 2,
            ..Config::default()
        }
    }

    /// Fold one `ClientEvent` into this `Sim`'s `World` exactly the way the net
    /// thread's `lodestone_client::state::SharedState::apply` does — enqueue,
    /// run `NetIngest` once, one event per run.
    ///
    /// # Why the loopback feed is not enough for these
    ///
    /// `NetClient::loopback_with_feed` models the `NetUpdate` channel — the
    /// *driver's* reaction path. It does not model `SharedState::apply`, which is
    /// where the local player's server-reported state (vitals, xp, the entity id,
    /// game mode, dimension, liveness) is folded, and there is no `SharedState` in
    /// a loopback harness at all. Production runs **both** paths for one packet,
    /// so a test that needs both drives both — which is closer to production than
    /// the `NetUpdate::Health` these tests used to feed, because that arm was the
    /// duplicate fold the collapse deleted.
    fn ingest(sim: &mut Sim, event: lodestone_client::ClientEvent) {
        sim.write(|w| {
            w.resource_mut::<lodestone_ecs::ingest::IngestQueue>().push(event);
            w.run_schedule(lodestone_ecs::NetIngest);
        });
    }

    /// A `ClientEvent::Login` for `entity_id`, creative in the overworld — the
    /// event that seeds `ServerEntityId` **and** the local player's `EntityIndex`
    /// entry.
    fn login_event(entity_id: i32) -> lodestone_client::ClientEvent {
        lodestone_client::ClientEvent::Login {
            entity_id,
            game_mode: lodestone_client::GameMode::Creative,
            dimension: "minecraft:overworld".parse().expect("valid dimension id"),
        }
    }

    /// The objective name currently displayed in the sidebar slot, read straight
    /// off the [`lodestone_ecs::SessionScoreboard`] component rather than through
    /// `Sim::sidebar` — which also needs the objective's own `ObjectiveUpdate` and
    /// a translator, neither of which this is asking about.
    fn displayed_sidebar(sim: &Sim) -> Option<String> {
        sim.read(|w| {
            w.get::<lodestone_ecs::SessionScoreboard>(sim.local)?
                .0
                .displayed(lodestone_game::scoreboard::DisplaySlot::Sidebar)
                .map(str::to_owned)
        })
    }

    /// What a real windowed client is built from — the path that must never hold
    /// an offline world. `Mode::Window` matters: `Mode::Headless` deliberately
    /// delegates to the demo-world fixture (see [`Sim::new`]).
    fn client_config() -> Config {
        Config {
            mode: Mode::Window,
            render_distance: 2,
            ..Config::default()
        }
    }

    /// Sections the GPU is holding, counted the way `app::WindowApp::redraw`
    /// drives it: upload everything that has meshed, then apply the removals.
    /// `TerrainMesh::uploaded_sections` is the record of exactly that set.
    fn resident_sections(sim: &mut Sim) -> usize {
        let _ = sim.drain_all_meshes();
        let _ = sim.drain_removals();
        sim.terrain(|t| t.uploaded_sections.len())
    }

    /// Drive one loopback session to `Connected` and report what is resident.
    /// The feed sends **no chunks**, so the live world's section set is empty and
    /// any non-zero count is offline terrain.
    fn resident_after_connect(mut sim: Sim) -> usize {
        use crate::net::NetUpdate;
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        sim.attach_net(net);
        feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
        sim.poll_net();
        assert_eq!(sim.session_phase(), SessionPhase::Connected);
        sim.step(5.0 / 20.0);
        resident_sections(&mut sim)
    }

    #[test]
    fn a_client_session_holds_only_the_live_world_never_offline_terrain() {
        // The two-worlds regression: the client came up with `worldgen`'s demo
        // world meshed and uploaded around the origin, then a multiplayer join
        // added the server's columns *alongside* it — the player standing at the
        // server's spawn with the wrong world drawn several hundred blocks away.
        //
        // The assertion is on the counters the report was diagnosed from: total
        // resident sections must equal the live set, not the sum. It comes first
        // in this test so that the control below — the pre-fix construction —
        // fails on *this* check rather than on a structural one.
        assert_eq!(
            resident_after_connect(Sim::new(client_config())),
            0,
            "after attaching a live session the resident set must be exactly the \
             live world's sections (none here — the loopback feed sends no chunks); \
             anything else is the offline world left behind"
        );

        // Same property, one layer earlier: nothing to tear down beats tearing
        // it down, so the offline world must never be built or scheduled at all.
        let mut sim = Sim::new(client_config());
        assert!(
            sim.chunk_world().is_empty(),
            "a client session must not generate an offline world"
        );
        assert_eq!(
            sim.pending_meshes(),
            0,
            "a client session must not schedule offline sections for meshing"
        );
        assert_eq!(
            resident_sections(&mut sim),
            0,
            "nothing may be uploaded before a session exists"
        );
    }

    #[test]
    fn the_demo_world_fixture_is_the_control_that_fails_the_gate_above() {
        // The detector's positive control. `Sim::with_demo_world` *is* what
        // `Sim::new` used to do for every windowed run without `--live`, so this
        // reproduces the reported state exactly: offline sections meshed,
        // uploaded, and still resident after a live session attaches. If this ever
        // reports zero, the gate above has stopped being able to fail and is
        // vacuous — it is not measuring residency any more.
        let mut fixture = Sim::with_demo_world(test_config());
        assert!(
            !fixture.chunk_world().is_empty(),
            "the fixture must build a world"
        );
        assert!(
            resident_sections(&mut fixture) > 0,
            "control: the fixture must actually upload offline sections"
        );
        assert!(
            resident_after_connect(Sim::with_demo_world(test_config())) > 0,
            "control: offline sections must still be resident after a live \
             session attaches — this is the assertion the client path must not \
             be able to satisfy"
        );
    }

    #[test]
    fn fog_reaches_full_at_the_configured_render_distance() {
        // Fog is what hides the render-distance edge, so its end must track the
        // *configured* distance. A fixed default would fog out the outer chunks
        // of a larger view, making `--render-distance 16` look worse than 8.
        for rd in [2u32, 8, 16, 32] {
            let fog = fog_for_render_distance(rd);
            assert_eq!(
                fog.end,
                rd as f32 * 16.0,
                "fog should reach full at the render distance for rd={rd}"
            );
            assert!(
                fog.start < fog.end,
                "fog range must be non-degenerate, else fog silently disables"
            );
        }
    }

    #[test]
    fn fog_stays_well_inside_the_camera_far_plane() {
        // If fog completed at or beyond the far plane, geometry would clip
        // against a still-visible background instead of dissolving into it.
        for rd in [2u32, 8, 16, 32] {
            let far = lodestone_render::Camera::far_for_render_distance(rd, 0);
            assert!(
                fog_for_render_distance(rd).end < far,
                "fog end must precede the far plane for rd={rd}"
            );
        }
    }

    #[test]
    fn fog_fades_into_the_same_colour_the_frame_clears_to() {
        // Terrain fades into the sky. If these two drifted apart, the horizon
        // would show a band of haze in a colour the sky never is.
        assert_eq!(fog_for_render_distance(8).color, crate::gpu::SKY_COLOR);
    }

    #[test]
    fn sim_fog_follows_its_own_config_not_a_default() {
        // Proves the delegation, so the cheap tests above actually cover what
        // the renderer is handed.
        let sim = Sim::new(test_config());
        assert_eq!(
            sim.fog_settings(),
            fog_for_render_distance(sim.config.render_distance)
        );
        assert_ne!(
            sim.fog_settings(),
            fog_for_render_distance(8),
            "test config is not the default distance, so these must differ"
        );
    }

    #[test]
    fn a_submerged_eye_selects_short_dense_fog_over_the_sky_fog() {
        // The whole point of threading the fluid state through: while the eye is
        // under water the fog must become the short, dense water fog, not the
        // render-distance sky fog that would leave the seabed sharp to the
        // horizon (the pre-change bug, confirmed on pixels). Guards the
        // *selection*; the colour/vanilla-likeness is a pixel concern.
        let mut sim = Sim::new(test_config());
        let rd = sim.config.render_distance;
        let sky = fog_for_render_distance(rd);

        // Dry: the render-distance sky fog.
        assert_eq!(sim.fog_settings(), sky, "a dry eye keeps the sky fog");

        // Eye in water: shorter than, and a different colour from, the sky fog.
        sim.set_fluid_state(FluidState {
            water_height: 1.0,
            eye_in_water: true,
            ..FluidState::NONE
        });
        assert!(sim.fluid_state().under_water());
        let water = sim.fog_settings();
        assert_ne!(water, sky, "a submerged eye must not keep the sky fog");
        assert!(water.end <= sky.end, "water fog cannot reach past the sky edge");
        assert_eq!(water.start, 0.0, "water fog ramps from the eye");
        assert!(
            water.start < sky.start,
            "water fog is denser (starts nearer) than the sky fog"
        );

        // Eye in lava wins over water and is shorter still.
        sim.set_fluid_state(FluidState {
            water_height: 1.0,
            eye_in_water: true,
            lava_height: 1.0,
            eye_in_lava: true,
        });
        assert!(sim.fluid_state().under_lava());
        assert!(
            sim.fog_settings().end < water.end,
            "lava blinds faster than water"
        );
    }

    /// Real census entries as the version's table reports them (v770's
    /// `hardness.rs`, dumped from a headless 26.2 server). Spelled out here so
    /// the shell's unit tests assert against real numbers while still naming no
    /// version crate; the `live`-gated test below proves these are the values
    /// that actually arrive through the registry seam.
    mod census {
        use lodestone_model::BlockHardness;

        pub const STONE: BlockHardness = BlockHardness {
            hardness: 1.5,
            requires_correct_tool: true,
        };
        pub const DIRT: BlockHardness = BlockHardness {
            hardness: 0.5,
            requires_correct_tool: false,
        };
        pub const OBSIDIAN: BlockHardness = BlockHardness {
            hardness: 50.0,
            requires_correct_tool: true,
        };
        pub const BEDROCK: BlockHardness = BlockHardness {
            hardness: -1.0,
            requires_correct_tool: false,
        };
    }

    /// Bare-hand inputs on flat, dry ground — the pose every timing figure below
    /// is quoted at.
    fn dry_ground(entry: lodestone_model::BlockHardness) -> BreakInputs {
        dig_break_inputs(entry, bare_handed_tool_mining(entry), false, true, false)
    }

    #[test]
    fn bare_hand_correct_tool_is_the_negation_of_the_blocks_requirement() {
        // The defect this whole path exists to fix, pinned as a number. Feeding
        // `requires_correct_tool` straight into `correct_tool` is the naive
        // wiring: it reads like faithful data and flips stone from the 100
        // divider to the 30, breaking it 3.4x too fast — i.e. it reintroduces
        // "block breaking is too fast" while looking correct.
        let naive_stone = BreakInputs {
            hardness: census::STONE.hardness,
            correct_tool: census::STONE.requires_correct_tool,
            ..BreakInputs::default()
        };
        assert_eq!(
            naive_stone.ticks_to_break(),
            Some(45),
            "sanity: the naive wiring really is the fast one"
        );
        assert_eq!(
            dry_ground(census::STONE).ticks_to_break(),
            Some(151),
            "bare-hand stone must take 151 ticks (~8.0s), server-confirmed over RCON; \
             45 here means `correct_tool` was fed `requires_correct_tool` unnegated"
        );

        // Dirt moves the *other* way, so a test that only looked at stone could
        // be satisfied by a blanket `correct_tool: false`.
        assert_eq!(
            dry_ground(census::DIRT).ticks_to_break(),
            Some(15),
            "bare-hand dirt is the correct tool for its own drops: 30 divider"
        );
        let naive_dirt = BreakInputs {
            hardness: census::DIRT.hardness,
            correct_tool: census::DIRT.requires_correct_tool,
            ..BreakInputs::default()
        };
        assert_eq!(naive_dirt.ticks_to_break(), Some(51));
    }

    #[test]
    fn a_resolved_tool_mining_speeds_up_the_dig_not_just_bare_hands() {
        // This is the actual regression the `sim.rs` wiring exists to close:
        // before it, `drive_mining` fed `BreakInputs::default()` for every tool
        // field regardless of what the version adapter resolved, so a diamond
        // pickaxe mined stone no faster than a fist. `dig_break_inputs` must
        // fold a real `ToolMining` straight through — reference numbers from
        // `docs/tool-mining.md` (also pinned externally by
        // `crates/lodestone-data/tests/tools.rs`): a diamond pickaxe (`speed:
        // 8.0`, `correct_tool: true`) on stone is 6 ticks, not the bare-hand
        // 151.
        let diamond_pickaxe = lodestone_model::ToolMining {
            speed: 8.0,
            correct_tool: true,
            damage_per_block: 1,
        };
        let tooled = dig_break_inputs(census::STONE, diamond_pickaxe, false, true, false);
        assert_eq!(tooled.tool_speed, 8.0);
        assert!(tooled.correct_tool);
        assert_eq!(
            tooled.ticks_to_break(),
            Some(6),
            "a diamond pickaxe on stone must be 6 ticks, matching the v770 tool oracle"
        );
        assert_eq!(
            dry_ground(census::STONE).ticks_to_break(),
            Some(151),
            "bare hand on the same block must be unaffected by the tooled case above"
        );
    }

    #[test]
    fn tool_mining_item_lifts_the_hotbar_stacks_id_and_count_with_no_tool_override() {
        // `tool_mining_item` is what `drive_mining` feeds `VersionAdapter::tool_mining`
        // for the selected hotbar slot. It must carry the real item id and count
        // across, and leave `tool` at `Inherited` when the wire said nothing, so
        // `tool_mining` resolves the item's *built-in* tool from the version's
        // generated prototype table rather than silently treating every held item
        // as toolless. This is the control for
        // `an_explicit_wire_tool_override_survives_the_lift_to_the_version_seam`.
        let item_id: lodestone_model::Identifier =
            "minecraft:diamond_pickaxe".parse().expect("valid id");
        let held = lodestone_game::item::ItemStack::new(item_id.clone(), 1);
        let lifted = tool_mining_item(&held);
        assert_eq!(lifted.item, item_id);
        assert_eq!(lifted.count, 1);
        assert_eq!(
            lifted.components.tool,
            lodestone_model::ToolPatch::Inherited,
            "no wire override means Inherited — the item id alone must resolve the tool"
        );
    }

    /// An explicit `minecraft:tool` from the wire (`/give
    /// …[minecraft:tool={…}]`, or a datapack item) must survive the lift into the
    /// version seam.
    ///
    /// It did not before: `tool_mining_item` built a fresh
    /// `ItemComponents::default()`, i.e. `ToolPatch::Inherited`, so an overridden
    /// tool resolved as if the *item default* applied — a custom-speed pickaxe
    /// dug at its vanilla rate, and `[!minecraft:tool]` dug like a real pickaxe
    /// instead of a bare hand. The canonical stack has carried the patch since
    /// `67ff7c3`; this reads it back.
    ///
    /// Both directions are checked, because `Removed` is the one that fails
    /// *unsafely*: an item that should mine like a bare hand mining at tool speed
    /// makes the client predict a break the server will not grant.
    #[test]
    fn an_explicit_wire_tool_override_survives_the_lift_to_the_version_seam() {
        use lodestone_game::item::{ComponentValue, ItemComponents, TOOL_COMPONENT};

        let item_id: lodestone_model::Identifier =
            "minecraft:diamond_pickaxe".parse().expect("valid id");
        let key: lodestone_model::Identifier = TOOL_COMPONENT.parse().expect("valid id");

        for patch in [
            lodestone_model::ToolPatch::Removed,
            // A rule-less tool with a distinctly non-vanilla speed: if the patch
            // were dropped, `tool_mining` would answer with the diamond
            // pickaxe's real table instead and the equality below would fail.
            lodestone_model::ToolPatch::Set(lodestone_model::ItemTool::new(
                Vec::new(),
                12.5,
                3,
                true,
            )),
        ] {
            let mut components = ItemComponents::new();
            components.insert(key.clone(), ComponentValue::Tool(patch.clone()));
            let held = lodestone_game::item::ItemStack::with_components(
                item_id.clone(),
                1,
                components,
            );
            assert_eq!(
                tool_mining_item(&held).components.tool,
                patch,
                "an explicit wire tool patch must reach `VersionAdapter::tool_mining`"
            );
        }
    }

    #[test]
    fn submerged_reads_eye_in_water_not_the_fogs_under_water() {
        // Vanilla's `getDestroySpeed` gates the 5x underwater penalty on
        // `isEyeInFluid(WATER)` alone; `FluidState::under_water()` additionally
        // requires `in_water()` and is what the *fog* selects on. The two
        // disagree exactly here — an eye in water whose box is not — so reading
        // the fog's predicate would silently drop the penalty in that pose.
        let eye_only = FluidState {
            eye_in_water: true,
            ..FluidState::NONE
        };
        assert!(eye_only.eye_in_water);
        assert!(
            !eye_only.under_water(),
            "the two predicates must actually differ here, or this proves nothing"
        );

        let dry = dry_ground(census::STONE);
        let wet = dig_break_inputs(
            census::STONE,
            bare_handed_tool_mining(census::STONE),
            false,
            true,
            eye_only.eye_in_water,
        );
        // Compare the *rate*, not the tick count: `ticks_to_break` replays
        // vanilla's f32 accumulate-and-compare loop, so a 5x slower rate lands
        // near — not exactly on — 5x the ticks (the same rounding that makes
        // bare-hand stone 151 rather than the textbook 150).
        assert_eq!(
            wet.dig_speed(),
            dry.dig_speed() * 0.2,
            "submerged mining is 5x slower (the 0.2 submerged_mining_speed factor)"
        );
        assert!(
            wet.ticks_to_break().unwrap() > dry.ticks_to_break().unwrap() * 4,
            "and it shows up in the break time"
        );
    }

    #[test]
    fn off_ground_mining_is_five_times_slower() {
        // `on_ground` was already wired before the hardness seam; keep it pinned
        // so a rewrite of the input builder cannot quietly drop it.
        let grounded = dry_ground(census::STONE);
        let airborne = dig_break_inputs(
            census::STONE,
            bare_handed_tool_mining(census::STONE),
            false,
            false,
            false,
        );
        assert_eq!(airborne.dig_speed(), grounded.dig_speed() / 5.0);
        assert!(
            airborne.ticks_to_break().unwrap() > grounded.ticks_to_break().unwrap() * 4,
            "off-ground mining must be materially slower"
        );
    }

    #[test]
    fn tool_inputs_stay_at_bare_hand_defaults() {
        // `dry_ground` builds its inputs from `bare_handed_tool_mining`
        // specifically (an empty main hand), so `tool_speed` must stay at the
        // bare-hand `1.0` here — a live dig instead resolves a real
        // `ToolMining` through `VersionAdapter::tool_mining` in `drive_mining`.
        // Mining efficiency, haste and fatigue have no modeled source at all
        // yet (no enchantment/potion/attribute inputs), so those stay at
        // `BreakInputs::default` regardless of what is held.
        let inputs = dry_ground(census::STONE);
        assert_eq!(inputs.tool_speed, 1.0);
        assert_eq!(inputs.mining_efficiency, 0.0);
        assert_eq!(inputs.haste_amplifier, None);
        assert_eq!(inputs.mining_fatigue, None);
        assert_eq!(inputs.block_break_speed, 1.0);
    }

    /// Replay a held dig for `ticks` and report the crack stage the shell would
    /// draw, mirroring `crack_target`'s read of `Mining::destroy_stage`.
    fn stage_after(entry: lodestone_model::BlockHardness, ticks: u32) -> i32 {
        let pos = BlockPos::new(0, 64, 0);
        let inputs = dry_ground(entry);
        let mut machine = Mining::new();
        machine.start(pos, BlockFace::Up, &inputs, None);
        for _ in 0..ticks {
            machine.continue_(pos, BlockFace::Up, &inputs, None);
        }
        machine.destroy_stage()
    }

    #[test]
    fn unbreakable_blocks_draw_no_crack_at_all() {
        // `hardness == -1.0` makes `progress_per_tick` return 0.0, so progress
        // never leaves 0.0 and `destroy_stage()` stays -1 — which is what
        // `crack_target` turns into `None`. Under the old fixed hardness bedrock
        // cracked like anything else.
        assert_eq!(dry_ground(census::BEDROCK).progress_per_tick(), 0.0);
        assert_eq!(dry_ground(census::BEDROCK).ticks_to_break(), None);
        for ticks in [0u32, 1, 10, 200] {
            assert_eq!(
                stage_after(census::BEDROCK, ticks),
                -1,
                "bedrock must never show a crack stage (t={ticks})"
            );
        }
    }

    #[test]
    fn crack_stages_advance_at_per_block_rates() {
        // The visible half of the defect: under one fixed hardness every block
        // pulsed through all ten stages at the same speed. Obsidian is 100x
        // stone's hardness and must crawl where dirt races.
        let t = 8;
        let dirt = stage_after(census::DIRT, t);
        let stone = stage_after(census::STONE, t);
        let obsidian = stage_after(census::OBSIDIAN, t);
        assert!(
            dirt > stone && stone >= obsidian,
            "stages must order dirt > stone >= obsidian at t={t}, got {dirt}/{stone}/{obsidian}"
        );
        assert!(dirt >= 5, "dirt is half-broken in 8 ticks, got stage {dirt}");
        assert_eq!(
            obsidian, 0,
            "obsidian (5000 ticks) must still be on stage 0 after 8 ticks"
        );
        // ... and it really does eventually crack, so `0` above is slowness and
        // not an unbreakable-style dead stop.
        assert!(stage_after(census::OBSIDIAN, 600) > 0);
    }

    #[cfg(feature = "live")]
    #[test]
    fn the_registry_seam_feeds_the_same_numbers_the_unit_tests_assume() {
        // Closes the loop: everything above asserts against hand-written census
        // constants, which would keep passing if `Sim` resolved no adapter at all
        // or the seam regressed to the trait's `None` default. This asserts the
        // shell's *own* lookup, for the protocol its config names.
        let sim = Sim::new(test_config());
        // Stage 5 deleted the `Sim.version_data` *field*; the adapter is the
        // `VersionData` resource. This gate still read the field and so had not
        // compiled since — invisible without `--features live`.
        let world = sim.ecs().read();
        let version = world.resource::<VersionData>();
        assert!(
            version.0.is_some(),
            "the `live` feature must compile a family in for protocol {}",
            sim.config.protocol
        );

        // Air is state 0 in every version's block-state registry, so it is the
        // one id the shell can name without naming a version.
        let air = version
            .block_hardness(id::AIR)
            .expect("air must resolve through the seam");
        assert_eq!(air.hardness, 0.0);

        // Find the census entries the unit tests above assume, by value rather
        // than by id (ids renumber every data bump).
        let entries: Vec<_> = (0..40_000)
            .filter_map(|id| version.block_hardness(id))
            .collect();
        assert!(
            entries.len() > 30_000,
            "expected a full state census, got {} entries",
            entries.len()
        );
        for expected in [
            census::STONE,
            census::DIRT,
            census::OBSIDIAN,
            census::BEDROCK,
        ] {
            assert!(
                entries.contains(&expected),
                "{expected:?} is not in the version's census — the hand-written \
                 constants in `census` have drifted from the real table"
            );
        }

        // An id past the census reports unknown rather than a guess, which is
        // what makes `drive_mining` refuse to dig instead of inventing a rate.
        assert_eq!(version.block_hardness(u32::MAX), None);
    }

    /// Live break-timing gate for the shell's own mining inputs, against the
    /// survival oracle (`lodestone-survival`, game :25565, RCON :25566).
    ///
    /// The hermetic tests above prove the *arithmetic*. What they cannot prove is
    /// the thing that made retiring the old fixed hardness risky: feeding a real
    /// hardness moves the client's `STOP_DESTROY` from ~5 ticks to the block's
    /// true completion tick, which is a change in **protocol interaction**, not
    /// just in a number. The server has two branches on `STOP` and this change
    /// swaps which one runs, so it has to be measured rather than reasoned about.
    ///
    /// Both regimes are driven back-to-back on the same connection and the same
    /// block, so the comparison is not across two runs of a shared server:
    ///
    /// * **before** — the retired `LIVE_DIG_HARDNESS` (`0.05` for every block).
    ///   `STOP` lands at ~5 ticks, `getDestroyProgress * (ticks + 1)` is ≈`0.04`,
    ///   under the server's `0.7` gate, so the server sets `hasDelayedDestroy`
    ///   and finishes on its own timer: the block becomes air **seconds after**
    ///   the `STOP`.
    /// * **after** — the shell's real inputs. `STOP` lands at tick 151, the
    ///   product is ≈`1.05`, over the gate, so the server takes the immediate
    ///   `destroyAndAck` branch: air lands **right behind** the `STOP`.
    ///
    /// The `stop → air` gap is therefore the discriminator between the branches,
    /// and the `start → air` total is the regression guard on player-visible
    /// break time (which must *not* move).
    ///
    /// ```text
    /// cargo test -p lodestone-shell --features live --lib \
    ///     sim::tests::live_bare_hand_stone -- --ignored --nocapture
    /// ```
    #[cfg(feature = "live")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires the lodestone-survival server on 127.0.0.1:25565 (RCON :25566)"]
    async fn live_bare_hand_stone_timing_survives_the_real_hardness_seam() {
        // `Instant` was missing here and this whole gate did not compile under
        // `--features live`; `--all-targets` alone cannot see it and `--lib`
        // without the feature cannot either, which is the exact blind spot
        // `CLAUDE.md`'s second health-check command exists to close. Pre-existing
        // at `84ffba2`, found by running that command.
        use std::time::{Duration, Instant};

        use lodestone_client::{ClientBuilder, ClientHandle, LoginProfile, ServerAddress};
        use lodestone_testsupport::{AsyncRconClient as Rcon, poll_until, unique_username};

        /// The hardness this path used to feed for *every* block, kept only here
        /// as the "before" leg of the measurement. It is not reachable from
        /// production code any more, and must not become so again.
        const RETIRED_FIXED_HARDNESS: f32 = 0.05;

        /// One dig, driven tick-by-tick through the real [`Mining`] machine with
        /// every emitted action lowered onto the wire. Returns
        /// `(stop_tick, start_to_stop, start_to_air)`, with air read from the
        /// *server* over RCON — never from our own optimistic prediction.
        async fn dig(
            handle: &ClientHandle,
            rcon: &mut Rcon,
            pos: BlockPos,
            inputs: &BreakInputs,
            max_ticks: u32,
        ) -> Option<(u32, Duration, Duration)> {
            let mut machine = Mining::new();
            let face = BlockFace::West;
            let t0 = Instant::now();
            for action in machine.start(pos, face, inputs, None) {
                let _ = handle.send_action(action);
            }
            let mut stop_at = None;
            let mut ticks = 0u32;
            while machine.is_destroying() && ticks < max_ticks {
                tokio::time::sleep(Duration::from_millis(50)).await;
                ticks += 1;
                for action in machine.continue_(pos, face, inputs, None) {
                    if matches!(
                        action,
                        ClientAction::BlockAction {
                            action: lodestone_model::BlockActionKind::StopDestroy,
                            ..
                        }
                    ) {
                        stop_at = Some((ticks, t0.elapsed()));
                    }
                    let _ = handle.send_action(action);
                }
            }
            let (stop_tick, to_stop) = stop_at?;
            // Poll server truth. `execute if block` reports "Test passed" only on
            // a match, so this never mistakes an error string for a break.
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let resp = rcon
                    .cmd(&format!(
                        "execute if block {} {} {} minecraft:air",
                        pos.x, pos.y, pos.z
                    ))
                    .await;
                if resp.contains("Test passed") {
                    return Some((stop_tick, to_stop, t0.elapsed()));
                }
                if Instant::now() >= deadline {
                    return None;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        async fn place(rcon: &mut Rcon, pos: BlockPos, block: &str) -> bool {
            rcon.cmd(&format!("setblock {} {} {} {block}", pos.x, pos.y, pos.z))
                .await;
            rcon.cmd(&format!(
                "execute if block {} {} {} {block}",
                pos.x, pos.y, pos.z
            ))
            .await
            .contains("Test passed")
        }

        let user = unique_username();
        let protocol = test_config().protocol;
        let adapter = lodestone_registry::adapter_for_protocol(protocol)
            .expect("the `live` feature compiles a family in for the configured protocol");
        let (handle, mut events) = ClientBuilder::new(
            ServerAddress {
                host: "127.0.0.1".into(),
                port: 25565,
            },
            LoginProfile {
                username: user.clone(),
                uuid: uuid::Uuid::new_v4(),
            },
            adapter,
        )
        .connect()
        .await
        .expect("connect to lodestone-survival on 127.0.0.1:25565");
        // Drain the event stream so the driver's bounded channel never blocks.
        let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });

        assert!(
            poll_until(Duration::from_secs(30), Duration::from_millis(100), || async {
                handle
                    .players()
                    .into_iter()
                    .find(|p| p.name.as_deref() == Some(user.as_str()))
            })
            .await
            .is_some(),
            "player {user} never reached Play on the oracle"
        );

        let mut rcon = Rcon::connect(("127.0.0.1", 25566), "lodestone")
            .await
            .expect("connect RCON on 127.0.0.1:25566");
        // Survival is required (creative insta-breaks everything, making the
        // timing vacuous); op clears spawn protection; the effects keep a stray
        // mob, fall or hunger from killing the player mid-dig, which would
        // teleport the entity and strand every later command.
        let _ = rcon.cmd(&format!("op {user}")).await;
        let _ = rcon.cmd(&format!("gamemode survival {user}")).await;
        for eff in [
            "minecraft:resistance 999999 255 true",
            "minecraft:regeneration 999999 9 true",
            "minecraft:fire_resistance 999999 0 true",
            "minecraft:saturation 999999 9 true",
        ] {
            let _ = rcon.cmd(&format!("effect give {user} {eff}")).await;
        }

        let p = poll_until(Duration::from_secs(15), Duration::from_millis(200), || async {
            handle.position()
        })
        .await
        .expect("client never reported a position");
        // Two blocks east at feet level: clear of the player box, inside reach,
        // and never the floor being stood on.
        let target = BlockPos::new(p.x.floor() as i32 + 2, p.y.floor() as i32, p.z.floor() as i32);
        let gate = BlockPos::new(target.x, target.y, target.z + 2);
        for q in [target, gate] {
            for dy in 0..=1 {
                let _ = rcon
                    .cmd(&format!("setblock {} {} {} minecraft:air", q.x, q.y + dy, q.z))
                    .await;
            }
        }

        // Clear the server's `hasClientLoaded()` gate, which drops every
        // `player_action` for ~60 ticks after join. A hardness-0 block breaks on
        // START alone, so retrying it until it vanishes both proves the
        // instant-break branch and tells us the gate is open — without it the
        // first timed dig silently measures the gate instead of the block.
        let gate_deadline = Instant::now() + Duration::from_secs(30);
        let mut gate_cleared = false;
        while Instant::now() < gate_deadline {
            assert!(place(&mut rcon, gate, "minecraft:slime_block").await);
            let mut m = Mining::new();
            let gate_entry = lodestone_model::BlockHardness {
                hardness: 0.0,
                requires_correct_tool: false,
            };
            let inputs = dig_break_inputs(
                gate_entry,
                bare_handed_tool_mining(gate_entry),
                false,
                true,
                false,
            );
            assert!(inputs.progress_per_tick() >= 1.0, "hardness 0 is instant");
            for action in m.start(gate, BlockFace::Up, &inputs, None) {
                let _ = handle.send_action(action);
            }
            assert!(!m.is_destroying(), "an instant break retains no live dig");
            tokio::time::sleep(Duration::from_millis(500)).await;
            if rcon
                .cmd(&format!(
                    "execute if block {} {} {} minecraft:air",
                    gate.x, gate.y, gate.z
                ))
                .await
                .contains("Test passed")
            {
                gate_cleared = true;
                break;
            }
        }
        assert!(gate_cleared, "the server's client-loaded gate never opened");
        println!("load gate clear");

        // --- BEFORE: the retired fixed hardness ---
        assert!(place(&mut rcon, target, "minecraft:stone").await);
        let before = dig(
            &handle,
            &mut rcon,
            target,
            &BreakInputs {
                hardness: RETIRED_FIXED_HARDNESS,
                on_ground: true,
                ..BreakInputs::default()
            },
            400,
        )
        .await
        .expect("the retired-constant dig never reached air");
        println!(
            "BEFORE (fixed {RETIRED_FIXED_HARDNESS}): STOP at tick {} ({:?}), air at {:?} \
             — stop→air gap {:?}",
            before.0,
            before.1,
            before.2,
            before.2 - before.1
        );

        // --- AFTER: the shell's own inputs, from the real census entry ---
        assert!(place(&mut rcon, target, "minecraft:stone").await);
        let stone = dig_break_inputs(
            census::STONE,
            bare_handed_tool_mining(census::STONE),
            false,
            true,
            false,
        );
        assert_eq!(stone.ticks_to_break(), Some(151));
        let after = dig(&handle, &mut rcon, target, &stone, 400)
            .await
            .expect("the real-hardness dig never reached air");
        println!(
            "AFTER  (census stone): STOP at tick {} ({:?}), air at {:?} — stop→air gap {:?}",
            after.0,
            after.1,
            after.2,
            after.2 - after.1
        );

        // 1. The predictor now stops at the block's true completion tick.
        assert_eq!(
            after.0, 151,
            "the real-hardness dig must emit its STOP on tick 151, not earlier"
        );
        assert!(
            before.0 < 20,
            "sanity: the retired constant really did stop early (tick {})",
            before.0
        );

        // 2. Player-visible break time is unchanged — the regression guard. Both
        //    legs land near ~8s; the driving loop sleeps 50ms per tick so real
        //    scheduling jitter accumulates over 151 ticks, hence the window.
        for (label, total) in [("before", before.2), ("after", after.2)] {
            assert!(
                total > Duration::from_millis(6_500) && total < Duration::from_millis(12_000),
                "{label}: bare-hand stone must still take ~8s, got {total:?}"
            );
        }

        // 3. The branch really did swap: the retired constant left the server to
        //    finish the block seconds after the STOP (delayed-destroy), while the
        //    real hardness has the STOP itself destroy it (immediate).
        assert!(
            before.2 - before.1 > Duration::from_secs(3),
            "before: the server should have finished on its own timer well after the \
             early STOP, got a {:?} gap",
            before.2 - before.1
        );
        assert!(
            after.2 - after.1 < Duration::from_secs(2),
            "after: the STOP should destroy the block immediately (progress*(ticks+1) \
             ≈ 1.01 clears the 0.7 gate), got a {:?} gap",
            after.2 - after.1
        );

        // Best-effort cleanup on the shared oracle.
        for q in [target, gate] {
            let _ = rcon
                .cmd(&format!("setblock {} {} {} minecraft:air", q.x, q.y, q.z))
                .await;
        }
        let _ = rcon.cmd(&format!("effect clear {user}")).await;
        let _ = rcon.cmd(&format!("deop {user}")).await;
        drain.abort();
    }

    #[test]
    fn new_generates_world_and_schedules_meshes() {
        let sim = Sim::new(test_config());
        assert!(!sim.chunk_world().is_empty(), "world should have chunks");
        assert!(sim.pending_meshes() > 0, "sections should be scheduled");
    }

    #[test]
    fn all_scheduled_sections_mesh() {
        let mut sim = Sim::new(test_config());
        let meshes = sim.drain_all_meshes();
        assert!(!meshes.is_empty());
        assert!(meshes.iter().any(|m| m.mesh.quad_count() > 0));
    }

    #[test]
    fn stepping_settles_the_player_on_the_ground() {
        let mut sim = Sim::new(test_config());
        for _ in 0..60 {
            sim.step(1.0 / 20.0);
        }
        assert!(sim.player().on_ground, "player should be standing on terrain");
        assert_eq!(sim.stats.position[1], sim.player().position.y);
    }

    #[test]
    fn mouse_look_updates_view_and_clears_delta() {
        let mut sim = Sim::new(test_config());
        let yaw0 = sim.player().yaw;
        sim.input_mut(|i| i.add_mouse(50.0, 0.0));
        sim.apply_mouse();
        assert_ne!(sim.player().yaw, yaw0);
        assert_eq!(sim.input().mouse_dx, 0.0);
    }

    #[test]
    fn connected_sim_emits_one_move_per_physics_tick() {
        use crate::net::NetUpdate;
        let (net, actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        // Before login the adapter has no Play-state Move packet, so the shell
        // must not spew movement yet: drive to Connected first.
        feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
        sim.poll_net(); // → Connected
        assert_eq!(sim.session_phase(), SessionPhase::Connected);
        sim.step(5.0 / 20.0); // ~5 ticks, all now in-world.
        let sent = std::iter::from_fn(|| actions.try_recv().ok()).count();
        assert!(sent > 0, "a connected sim should send movement packets");
        assert_eq!(
            sent as u64, sim.tick_count(),
            "exactly one outbound Move per physics tick"
        );
    }

    #[test]
    fn move_is_withheld_until_connected() {
        // A sim that is merely Connecting (attached, not yet logged in) must send
        // nothing — otherwise every pre-Play tick is a dropped-action on the wire.
        let (net, actions, _feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        assert_eq!(sim.session_phase(), SessionPhase::Connecting);
        sim.step(5.0 / 20.0);
        assert!(sim.tick_count() > 0, "ticks must still run while connecting");
        let sent = std::iter::from_fn(|| actions.try_recv().ok()).count();
        assert_eq!(sent, 0, "no movement should be sent before login");
    }

    /// Both [`CollisionSource`] implementors must actually be `Send + Sync +
    /// 'static`, or they could not be held in a `Resource` at all.
    ///
    /// Asserted rather than reasoned about: the Stage 1 report recorded this as
    /// "likely, unverified" for [`LiveCollision`] (which holds
    /// `Arc<ChunkSection>`, `Arc<BlockAtlas>` and `Option<Arc<dyn
    /// VersionAdapter>>`), and it is the single fact the whole Stage-2 collision
    /// seam rests on. It compiles today because `Arc<dyn CollisionSource>` is
    /// used; this pins it so the reason stays visible if it ever stops holding.
    #[test]
    fn both_collision_sources_are_send_sync_and_static() {
        fn assert_resource_shaped<T: CollisionSource>() {}
        assert_resource_shaped::<ChunkWorldCollision>();
        assert_resource_shaped::<LiveCollisionSource>();
    }

    /// The authority test for the stage, at the shell level: the components are
    /// the *only* store, so a write through the `World` — which is what a plugin
    /// gets — changes what the server is told on the next tick.
    ///
    /// If `Sim` still held a `PlayerState` of its own, this would pass a write
    /// into a field nobody reads and the wire would report the unmodified pose.
    #[test]
    fn a_write_through_the_world_reaches_the_wire() {
        use crate::net::NetUpdate;
        let (net, actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
        sim.poll_net();
        while actions.try_recv().is_ok() {}

        let local = sim.local_player();
        sim.ecs()
            .write()
            .get_mut::<PhysicsState>(local)
            .expect("local player")
            .0
            .position = Vec3d::new(11.5, 200.0, -3.5);

        sim.step(lodestone_ecs::TICK_PERIOD);
        let moved: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok())
            .filter_map(|a| match a {
                ClientAction::Move { pos, .. } => Some(pos),
                _ => None,
            })
            .collect();
        assert_eq!(moved.len(), 1, "one move per tick");
        // No world to collide against in this fixture beyond the demo terrain far
        // below, so the tick's only change is gravity — x and z are untouched.
        assert!((moved[0].x - 11.5).abs() < 1e-9, "got {moved:?}");
        assert!((moved[0].z + 3.5).abs() < 1e-9, "got {moved:?}");
        // …and the accessor agrees with the wire, because there is one store.
        assert!((sim.player().position.x - 11.5).abs() < 1e-9);
    }

    /// The other half of the authority test: `Sim`'s accessors are views onto the
    /// same components, not onto a copy. A write through the accessor must be
    /// visible in the `World` a plugin queries.
    #[test]
    fn the_accessors_and_the_world_are_the_same_store() {
        let mut sim = Sim::new(test_config());
        sim.player_mut(|p| p.yaw = 42.0);
        sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));

        let local = sim.local_player();
        let world = sim.ecs().read();
        assert_eq!(world.get::<PhysicsState>(local).expect("local").0.yaw, 42.0);
        assert_eq!(
            lodestone_controller::movement_intent(&world.resource::<RawInput>().0).forward,
            1.0
        );
    }

    /// **Stage 4's authority test at the shell level.** The `ChunkWorld` resource
    /// is the *only* chunk store, so a write through the handle a plugin would get
    /// (`sim.chunk_world()`, or `sim.ecs().resource::<ChunkWorld>()`) is what the
    /// sim collides against, raycasts into and meshes.
    ///
    /// If `Sim` still owned a `World` field, this would write into a store nobody
    /// reads and `block_at_world` would report the pre-edit block.
    #[test]
    fn a_write_through_the_chunk_world_resource_is_what_the_sim_reads() {
        let sim = Sim::new(test_config());
        let feet = sim.player().position;
        let (bx, bz) = (feet.x.floor() as i32 + 4, feet.z.floor() as i32 + 4);
        let above = crate::worldgen::surface_height(bx, bz) + 4;

        assert_eq!(
            sim.block_at_world([bx, above, bz]),
            id::AIR,
            "the cell starts empty"
        );

        // The write goes through the resource handle, not through any `Sim` method.
        {
            let store = sim.chunk_world();
            let mut world = store.write();
            let chunk = world
                .get_mut(ChunkPos {
                    x: bx.div_euclid(16),
                    z: bz.div_euclid(16),
                })
                .expect("the fixture holds this column");
            chunk.column.set_block(
                bx.rem_euclid(16) as usize,
                above,
                bz.rem_euclid(16) as usize,
                PLACE_BLOCK,
            );
        }

        assert_eq!(
            sim.block_at_world([bx, above, bz]),
            PLACE_BLOCK,
            "the sim reads the store a plugin writes, with no propagation step"
        );
        // And collision sees it in the same instant — there is no cached clone to
        // invalidate any more. Before Stage 4 this needed
        // `Sim::set_block_world` to clear `demo_collision` by hand, and a missed
        // clear read as "I mined the block but still cannot walk through it".
        let source = sim.chunk_collision();
        let mut solid = false;
        source.with_view(&mut |view: &dyn CollisionView| {
            let mut boxes = Vec::new();
            view.collision_boxes(bx, above, bz, &mut boxes);
            solid = !boxes.is_empty();
        });
        assert!(
            solid,
            "the collision source reads the same store, uncached — a plugin's edit \
             is collidable on the next tick"
        );
    }

    /// The control for the test above: the same probe against a cell nobody wrote
    /// must report empty, so "solid" is a measurement rather than a constant.
    #[test]
    fn the_collision_source_reports_empty_where_nothing_was_written() {
        let sim: Sim = Sim::new(test_config());
        let feet = sim.player().position;
        let (bx, bz) = (feet.x.floor() as i32 + 4, feet.z.floor() as i32 + 4);
        let above = crate::worldgen::surface_height(bx, bz) + 4;

        let source = sim.chunk_collision();
        let mut solid = false;
        source.with_view(&mut |view: &dyn CollisionView| {
            let mut boxes = Vec::new();
            view.collision_boxes(bx, above, bz, &mut boxes);
            solid = !boxes.is_empty();
        });
        assert!(!solid, "control: an untouched air cell must not collide");
    }

    /// `heal_dirty_columns` must actually be registered in the `Update` schedule
    /// `Sim::step` runs — the island check for Stage 4's one system. A dirtied
    /// column that `run_schedule(Update)` does not drain is a chunk seam that
    /// stays baked against air forever.
    #[test]
    fn the_update_schedule_drains_the_dirty_column_set() {
        let mut sim = Sim::new(test_config());
        let _ = sim.drain_all_meshes();
        let pos = *sim
            .chunk_world()
            .read()
            .iter()
            .next()
            .expect("the fixture holds a column")
            .0;
        sim.terrain_mut(|t| t.dirty_columns.insert((pos.x, pos.z)));
        assert_eq!(sim.pending_meshes(), 0, "drained to a clean slate");

        sim.ecs().write().run_schedule(lodestone_ecs::Update);

        assert!(
            sim.terrain(|t| t.dirty_columns.is_empty()),
            "the Update schedule must drain the dirty set"
        );
        assert!(
            sim.pending_meshes() > 0,
            "and draining it must submit real mesh jobs, not just empty the set"
        );
    }

    #[test]
    fn disconnected_sim_sends_nothing() {
        // Without a net attached, stepping must not attempt to send.
        let mut sim = Sim::new(test_config());
        sim.step(5.0 / 20.0);
        assert!(sim.net.is_none());
    }

    #[test]
    fn mob_effect_applied_for_local_player_reaches_status_effects() {
        use crate::net::NetUpdate;
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
        // `ServerEntityId` — the "is this effect ours" test — is folded from
        // `ClientEvent::Login` on the net thread, not from `NetUpdate::LoggedIn`.
        // Production sees both for one packet; so does this test.
        ingest(&mut sim, login_event(7));
        sim.poll_net();
        assert_eq!(sim.server_entity_id(), Some(7), "setup: the id must be folded");
        assert!(sim.player().effects.levitation.is_none());

        feed.send(NetUpdate::EffectApplied {
            entity_id: 7,
            effect: "levitation".into(),
            amplifier: 2,
            duration_ticks: 200,
            ambient: false,
            show_icon: true,
        })
        .unwrap();
        sim.poll_net();
        assert_eq!(
            sim.player().effects.levitation,
            Some(2),
            "the wire→StatusEffects seam must fold an effect for the local entity id"
        );
        // The same event must also reach the display model with its full data.
        let chips = crate::effects::chips_from(&sim.active_effects());
        assert_eq!(chips.len(), 1, "the HUD effect model must fold it too");
        assert_eq!(chips[0].label, "levitation III"); // amplifier 2 → level III
        assert_eq!(chips[0].time, "0:10"); // 200 ticks → 10 s

        feed.send(NetUpdate::EffectRemoved {
            entity_id: 7,
            effect: "levitation".into(),
        })
        .unwrap();
        sim.poll_net();
        assert!(sim.player().effects.levitation.is_none());
        assert!(
            sim.active_effects().is_empty(),
            "removal must clear the HUD effect model as well"
        );
    }

    #[test]
    fn mob_effect_for_a_different_entity_is_not_applied_to_the_local_player() {
        use crate::net::NetUpdate;
        // `update_mob_effect` is entity-agnostic on the wire; only the entity id
        // that matches the local player's should ever mutate `sim.player`.
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
        ingest(&mut sim, login_event(7));
        sim.poll_net();

        feed.send(NetUpdate::EffectApplied {
            entity_id: 1234, // some other (mob) entity, not the local player
            effect: "levitation".into(),
            amplifier: 0,
            duration_ticks: 200,
            ambient: false,
            show_icon: true,
        })
        .unwrap();
        sim.poll_net();
        assert!(
            sim.player().effects.levitation.is_none(),
            "a remote entity's effect must not leak into the local player's StatusEffects"
        );
        assert!(
            sim.active_effects().is_empty(),
            "a remote entity's effect must not reach the local HUD overlay either"
        );
    }

    /// Hermetic proof that `NetUpdate::Particles` actually reaches the
    /// emitter: idle, `stats`/the HUD counter would also read
    /// `particles=0/0+0unres`, which cannot distinguish "the route works but
    /// nothing has fired" from "the route is missing" (`grep -rn
    /// "ClientEvent::Particles" crates/lodestone-shell/src/` returned zero
    /// hits before this change). So this feeds a live event and asserts the
    /// *caused* output, not the idle baseline.
    #[test]
    fn net_particles_reaches_the_emitter_and_resolves() {
        use crate::net::NetUpdate;
        use lodestone_client::Vec3;
        use lodestone_particle::Sheet;

        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
        sim.poll_net();

        // A headless `Sim` has no vanilla jar, so `flame`'s sheet has no atlas
        // UVs by default — install the same kind of fixture table
        // `particles.rs`'s own hermetic tests use, so `unresolved == 0` is
        // actually reachable without fetching `client.jar`.
        let rect = [0.0f32, 0.0, 0.0625, 0.0625];
        sim.particles_mut(|p| {
            p.install_test_sheet_uv(HashMap::from([((Sheet::Flame, 0u16), rect)]));
        });

        // Keep the particle origin within vanilla's 32-block render cutoff of
        // wherever `Sim::new` spawned the player.
        let origin = sim.player().position;
        feed.send(NetUpdate::Particles {
            kind: "flame".into(),
            long_distance: false,
            pos: Vec3::new(origin.x, origin.y, origin.z),
            offset: Vec3f::new(0.1, 0.1, 0.1),
            max_speed: 0.02,
            count: 9,
        })
        .unwrap();
        sim.poll_net();

        assert_eq!(
            sim.particles_mut(|p| p.engine_mut().particles().len()),
            9,
            "count must be honoured exactly once the event reaches the emitter"
        );
        let cam = sim.camera(1.0);
        let frame = sim.particles_mut(|p| {
            p.extract(&cam, 0.0, &|_, _, _| Some(lodestone_particle::FULL_BRIGHT))
        });
        assert_eq!(frame.alive, 9);
        assert_eq!(
            frame.unresolved, 0,
            "flame is a sheet-sourced type with an installed atlas entry"
        );
        assert_eq!(frame.drawn, 9);
    }

    /// How many particles the two hold measurements below run over. High enough
    /// that the per-particle work dominates two resource moves by orders of
    /// magnitude, well under `ParticleEngine::DEFAULT_CAPACITY` (16 384) so the
    /// engine does not silently drop the tail.
    const HOLD_MEASUREMENT_PARTICLES: i32 = 4_000;

    /// Spawns [`HOLD_MEASUREMENT_PARTICLES`] live particles around the player and
    /// returns the `Sim` and a camera to extract them with.
    fn sim_with_many_particles() -> (Sim, Camera) {
        let mut sim = Sim::new(test_config());
        let origin = sim.player().position;
        sim.particles_mut(|p| {
            p.spawn_particles(
                "smoke",
                [origin.x, origin.y, origin.z],
                [0.5, 0.5, 0.5],
                0.02,
                HOLD_MEASUREMENT_PARTICLES,
            );
        });
        let camera = sim.camera(1.0);
        (sim, camera)
    }

    /// **The measurement §4.1(c) could not make.**
    ///
    /// `Sim::extract_particles` was the longest `World` guard hold in the process:
    /// it took the write guard by hand and held it across the whole extract *and*
    /// one chunk-store lookup per live particle for light. `docs/world-unification.md`
    /// bounded that structurally — "no guard spans a frame" — and said so out loud:
    /// *treat the bound as structural, not measured*. A duration claim with nothing
    /// measuring the duration is the species of vacuous test `CLAUDE.md` names, so
    /// this is the number.
    ///
    /// The assertion is a **ratio against the call's own wall time**, not an
    /// absolute nanosecond ceiling: an absolute bound is a statement about this
    /// machine and fails on a slower one (or under a loaded CI), whereas both sides
    /// of a ratio are measured in the same run on the same core. Expected value is
    /// a fraction of a percent; the threshold is 25%, i.e. two orders of margin.
    ///
    /// Its negative control is
    /// [`the_pre_fix_shape_of_extract_particles_fails_the_hold_bound`], which
    /// reproduces the old shape and must fail this same bound.
    #[test]
    fn extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work() {
        let (mut sim, camera) = sim_with_many_particles();

        sim.reset_lock_holds();
        let started = std::time::Instant::now();
        let frame = sim.extract_particles(&camera);
        let wall = started.elapsed();
        let holds = sim.lock_holds();

        // The *world*-species guard: the flaw in a vacuous duration test lives in
        // the input, not the assert. An extract over an empty engine would satisfy
        // the ratio below trivially and prove nothing, so assert the volume first.
        assert!(
            frame.alive >= HOLD_MEASUREMENT_PARTICLES as usize,
            "the measurement is only meaningful over real particle volume; alive={}",
            frame.alive
        );
        eprintln!(
            "extract_particles over {} particles: wall {:?}, guarded {} ns across {} holds \
             (longest {} ns)",
            frame.alive, wall, holds.total_ns, holds.holds, holds.longest_ns
        );
        assert!(
            u128::from(holds.total_ns) * 4 < wall.as_nanos(),
            "the `World` guard must not span the per-particle work: guarded {} ns of a {} ns \
             call over {} particles",
            holds.total_ns,
            wall.as_nanos(),
            frame.alive
        );
    }

    /// The negative control for the bound above, and the reason it is evidence
    /// rather than decoration: the *pre-fix shape* — extract run inside the write
    /// guard — must fail the same assertion, measured by the same counter.
    ///
    /// This is deliberately hand-written rather than a switch on `Sim`: a test
    /// switch would have to survive in production code, and what needs proving is
    /// that the detector distinguishes two shapes, not that a flag works.
    #[test]
    fn the_pre_fix_shape_of_extract_particles_fails_the_hold_bound() {
        let (mut sim, camera) = sim_with_many_particles();

        sim.reset_lock_holds();
        let started = std::time::Instant::now();
        // Exactly what `extract_particles` used to do. `light` is the offline arm
        // (`self.net == None`), so this control *understates* the old hold — the
        // live arm additionally took a chunk-store lock per particle inside it.
        let frame = lodestone_ecs::hold_write(sim.ecs(), |w| {
            w.resource_mut::<ParticleSim>()
                .0
                .extract(&camera, 0.0, &|_, _, _| None)
        });
        let wall = started.elapsed();
        let holds = sim.lock_holds();

        assert!(
            frame.alive >= HOLD_MEASUREMENT_PARTICLES as usize,
            "same input volume as the positive case; alive={}",
            frame.alive
        );
        eprintln!(
            "pre-fix shape over {} particles: wall {:?}, guarded {} ns across {} holds \
             (longest {} ns)",
            frame.alive, wall, holds.total_ns, holds.holds, holds.longest_ns
        );
        assert!(
            u128::from(holds.total_ns) * 4 >= wall.as_nanos(),
            "the detector must fire on the shape it exists to reject; it reported only {} ns \
             guarded of a {} ns call, so the bound in \
             `extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work` \
             is not discriminating",
            holds.total_ns,
            wall.as_nanos()
        );
    }

    /// The frame-level claim, also measured: `Sim::step` takes **many short
    /// guards**, not one long one.
    ///
    /// `docs/world-unification.md` said "counted from the code it takes on the
    /// order of 15 short guards plus ~8 per catch-up tick". This counts them, so a
    /// future refactor that coalesced the frame into one long guard — which would
    /// read as a tidy-up and would stall ingest for a whole frame — fails here.
    /// The control for the mechanism is `lodestone_ecs`'s
    /// `the_hold_meter_reports_a_deliberately_long_hold`.
    #[test]
    fn a_frame_takes_many_short_world_guards_and_no_long_one() {
        let mut sim = Sim::with_demo_world(test_config());
        // One frame long enough to run at least one catch-up tick.
        sim.step(0.1);

        sim.reset_lock_holds();
        let started = std::time::Instant::now();
        sim.step(0.1);
        let wall = started.elapsed();
        let holds = sim.lock_holds();

        eprintln!(
            "Sim::step(0.1): wall {:?}, {} holds totalling {} ns, longest {} ns",
            wall, holds.holds, holds.total_ns, holds.longest_ns
        );
        assert!(
            holds.holds >= 15,
            "a frame must be many short guards rather than one long one; counted {}",
            holds.holds
        );
        // A ceiling, not a target: 25 ms is "no single guard spans a 40 fps frame".
        // Absolute rather than a ratio here because a whole `step` legitimately
        // *is* mostly its two `run_schedule` holds, so a ratio would assert
        // nothing. Loose enough to survive a preempted CI core; the control above
        // shows a 30 ms hold is visible, so this ceiling can actually be crossed.
        assert!(
            holds.longest_ns < 25_000_000,
            "no single `World` guard in a frame may approach a frame: longest was {} ns",
            holds.longest_ns
        );
    }

    /// Vanilla's render cutoff (`ClientLevel.doAddParticle`): a particle
    /// farther than 32 blocks from the viewer is dropped unless the packet
    /// sets `long_distance`. Two events at the same far-away position, one
    /// with the flag and one without, must differ in whether anything
    /// spawns — proving the cutoff is actually wired to the flag rather than
    /// always on or always off.
    #[test]
    fn long_distance_flag_gates_the_far_away_cutoff() {
        use crate::net::NetUpdate;
        use lodestone_client::Vec3;
        use lodestone_particle::Sheet;

        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
        sim.poll_net();
        sim.particles_mut(|p| {
            p.install_test_sheet_uv(HashMap::from([(
                (Sheet::Flame, 0u16),
                [0.0f32, 0.0, 0.0625, 0.0625],
            )]));
        });

        // Comfortably past the 32-block (sqrt(1024)) cutoff on every axis.
        let origin = sim.player().position;
        let far = Vec3::new(origin.x + 1000.0, origin.y, origin.z);

        feed.send(NetUpdate::Particles {
            kind: "flame".into(),
            long_distance: false,
            pos: far,
            offset: Vec3f::new(0.0, 0.0, 0.0),
            max_speed: 0.0,
            count: 3,
        })
        .unwrap();
        sim.poll_net();
        assert_eq!(
            sim.particles_mut(|p| p.engine_mut().particles().len()),
            0,
            "a far-away burst without long_distance must be dropped, not spawned off-screen"
        );

        feed.send(NetUpdate::Particles {
            kind: "flame".into(),
            long_distance: true,
            pos: far,
            offset: Vec3f::new(0.0, 0.0, 0.0),
            max_speed: 0.0,
            count: 3,
        })
        .unwrap();
        sim.poll_net();
        assert_eq!(
            sim.particles_mut(|p| p.engine_mut().particles().len()),
            3,
            "the same burst with long_distance set must bypass the cutoff"
        );
    }

    #[test]
    fn session_phase_tracks_net_updates() {
        use crate::net::NetUpdate;
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        // Before any connection: purely local.
        assert_eq!(sim.session_phase(), SessionPhase::LocalOnly);

        // Attaching a live connection moves us to Connecting immediately, so the
        // menu shows a loading screen rather than a lie.
        sim.attach_net(net);
        assert_eq!(sim.session_phase(), SessionPhase::Connecting);

        // LoggedIn ⇒ Connected (the menu's "session_ready").
        feed.send(NetUpdate::LoggedIn { entity_id: 42 }).unwrap();
        sim.poll_net();
        assert_eq!(sim.session_phase(), SessionPhase::Connected);

        // A mid-game disconnect ⇒ Ended with the reason preserved, which is what
        // drives the menu's Error screen. Assert the reason survives, so a
        // blank/again-Connected mapping can't pass.
        feed.send(NetUpdate::Disconnected("Server closed".into()))
            .unwrap();
        sim.poll_net();
        match sim.session_phase() {
            SessionPhase::Ended(reason) => {
                assert!(reason.contains("Server closed"), "reason lost: {reason}");
            }
            other => panic!("expected Ended, got {other:?}"),
        }
    }

    #[test]
    fn session_phase_reports_net_error_as_ended() {
        use crate::net::NetUpdate;
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::Error("connection refused".into()))
            .unwrap();
        sim.poll_net();
        match sim.session_phase() {
            SessionPhase::Ended(reason) => {
                assert!(reason.contains("connection refused"), "got {reason}");
            }
            other => panic!("expected Ended, got {other:?}"),
        }
    }

    #[test]
    fn end_session_tears_down_and_a_fresh_connect_afterward_starts_clean() {
        // The real acceptance test for `Sim::end_session`: not just that it
        // clears fields, but that a *second* connect afterward behaves
        // exactly like the first, with nothing from the old session leaking
        // through.
        use crate::net::NetUpdate;
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
        ingest(&mut sim, login_event(7));
        sim.poll_net();
        assert_eq!(sim.session_phase(), SessionPhase::Connected);

        // Populate every read-model `end_session` is responsible for
        // clearing, so this test can actually observe the reset rather than
        // asserting on fields that were already empty. The vitals go in through
        // the *net thread's* fold (`ingest`) because that is now the only writer;
        // the chat log still arrives on the `NetUpdate` channel.
        feed.send(NetUpdate::Chat {
            text: lodestone_model::Text::literal("hello"),
            player: false,
        })
        .unwrap();
        ingest(
            &mut sim,
            lodestone_client::ClientEvent::HealthChanged {
                health: 12.0,
                food: 8,
                saturation: 3.0,
            },
        );
        // A shared-fold component that is *not* a vital, to pin the other half of
        // the stale-note fix: before this change `end_session` left the previous
        // server's sidebar standing.
        ingest(
            &mut sim,
            lodestone_client::ClientEvent::DisplayObjective {
                slot: lodestone_model::event::DisplaySlot::Sidebar,
                objective: Some("kills".into()),
            },
        );
        sim.poll_net();
        assert!(
            !sim.recent_chat(10).is_empty(),
            "setup: chat must be populated before the teardown can be observed clearing it"
        );
        assert_eq!(sim.health(), Some(12.0), "setup: health must be populated");
        assert_eq!(
            sim.server_entity_id(),
            Some(7),
            "setup: entity id must be populated"
        );
        assert_eq!(
            displayed_sidebar(&sim).as_deref(),
            Some("kills"),
            "setup: the sidebar must be populated"
        );

        sim.end_session();

        assert!(sim.net().is_none(), "the connection must be dropped");
        assert_eq!(sim.session_phase(), SessionPhase::LocalOnly);
        assert!(sim.recent_chat(10).is_empty(), "chat log must clear");
        assert_eq!(sim.health(), None, "health must clear");
        assert_eq!(sim.food(), None, "food must clear");
        assert_eq!(
            sim.server_entity_id(),
            None,
            "the local entity id must clear"
        );
        assert_eq!(
            displayed_sidebar(&sim),
            None,
            "the previous server's sidebar must clear too — §4.1(c) made this \
             reachable from `Sim.local`, so the old 'it goes away with `net`' \
             reasoning no longer holds"
        );

        // The negative control this test exists for: a fresh connect
        // afterward must reach `Connected` and must not carry the old
        // session's chat forward, proving the reset actually took rather
        // than merely reporting empty because nothing polled yet.
        let (net2, _actions2, feed2) = NetClient::loopback_with_feed();
        sim.attach_net(net2);
        assert_eq!(sim.session_phase(), SessionPhase::Connecting);
        feed2.send(NetUpdate::LoggedIn { entity_id: 9 }).unwrap();
        ingest(&mut sim, login_event(9));
        sim.poll_net();
        assert_eq!(sim.session_phase(), SessionPhase::Connected);
        assert_eq!(sim.server_entity_id(), Some(9));
        assert!(
            sim.recent_chat(10).is_empty(),
            "the new session must not inherit the old one's chat"
        );
    }

    #[test]
    fn inbound_chat_is_logged_and_typed_lines_route_to_the_action_seam() {
        use crate::net::NetUpdate;
        use lodestone_client::ClientAction;
        let (net, actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);

        // Inbound server chat must surface in the HUD log (not merely logged).
        feed.send(NetUpdate::Chat {
            text: lodestone_model::Text::literal("hello world"),
            player: false,
        })
        .unwrap();
        sim.poll_net();
        let lines: Vec<String> = sim.recent_chat(10).into_iter().map(|(l, _)| l).collect();
        assert_eq!(
            lines,
            vec!["hello world".to_string()],
            "inbound chat must reach the display log"
        );

        // Typed lines route through the one outbound action seam: a leading '/'
        // is a command (slash stripped), otherwise a chat message.
        assert!(sim.send_chat("/say hi"), "a command line must send");
        assert!(sim.send_chat("plain message"), "a chat line must send");
        // Anti-vacuity: a blank line must send *nothing*, so "everything sends"
        // can't pass — and neither can "nothing sends", guarded by the two above.
        assert!(!sim.send_chat("   "), "blank input must not send");

        let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert_eq!(
            sent,
            vec![
                ClientAction::SendCommand {
                    command: "say hi".into()
                },
                ClientAction::SendChat {
                    text: "plain message".into()
                },
            ],
            "exactly the two non-blank lines route, with the command slash stripped"
        );
    }

    #[test]
    fn chat_lines_age_as_the_clock_advances() {
        use crate::net::NetUpdate;
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);

        feed.send(NetUpdate::Chat {
            text: lodestone_model::Text::literal("aged line"),
            player: false,
        })
        .unwrap();
        sim.poll_net();
        // Freshly received: age is ~0.
        assert!(
            sim.recent_chat(1)[0].1 < 0.001,
            "a just-received line is young"
        );

        // Advancing the sim clock ages the line by real elapsed time.
        sim.step(2.5);
        let age = sim.recent_chat(1)[0].1;
        assert!(
            (2.4..=2.6).contains(&age),
            "line age must track the sim clock, got {age}"
        );
    }

    /// The HUD's health/food accessors must reflect the **net thread's** fold.
    ///
    /// This used to feed `NetUpdate::Health` and assert the shell's own arm folded
    /// it. That arm was the duplicate the vitals collapse deleted, so the test now
    /// drives `ClientEvent::HealthChanged` through the one remaining fold — the
    /// `NetIngest` schedule inside this `Sim`'s own `World`, which is exactly what
    /// production does — and asserts the same accessors. Sharper, not weaker: the
    /// old version could have passed with the production fold missing entirely.
    #[test]
    fn server_health_and_food_reach_the_hud_accessors() {
        let (net, _actions, _feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        // Off a live server there is no survival state, so the HUD draws no bars.
        assert_eq!(sim.health(), None);
        assert_eq!(sim.food(), None);

        ingest(
            &mut sim,
            lodestone_client::ClientEvent::HealthChanged {
                health: 14.0,
                food: 17,
                saturation: 2.5,
            },
        );
        // Both fields must land — a one-sided store would leave the other None.
        assert_eq!(sim.health(), Some(14.0));
        assert_eq!(sim.food(), Some(17));
    }

    /// The negative control for the two tests above: enqueueing without running
    /// the schedule must change nothing, so "the accessor reports 14" is evidence
    /// the *fold* ran and not merely that the event was constructed.
    #[test]
    fn queueing_health_without_running_net_ingest_folds_nothing() {
        let mut sim = Sim::new(test_config());
        let local = sim.local;
        sim.write(|w| {
            w.resource_mut::<lodestone_ecs::ingest::IngestQueue>().push(
                lodestone_client::ClientEvent::HealthChanged {
                    health: 14.0,
                    food: 17,
                    saturation: 2.5,
                },
            );
        });
        assert_eq!(sim.health(), None, "pushing must not fold; only NetIngest folds");
        // …and the local player really is the entity the fold would write, so the
        // assertion above is not passing because it is looking at the wrong one.
        assert!(
            sim.read(|w| w.get::<Vitals>(local).is_some()),
            "the local player must carry Vitals for this control to mean anything"
        );
    }

    #[test]
    fn server_experience_reaches_the_hud_accessor() {
        let (net, _actions, _feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        // Off a live server (or before the first packet) there is no real XP
        // value, so the HUD must not draw a faked bar.
        assert_eq!(sim.experience(), None);

        ingest(
            &mut sim,
            lodestone_client::ClientEvent::ExperienceChanged {
                progress: 0.6,
                level: 30,
                total: 1395,
            },
        );
        assert_eq!(sim.experience(), Some((0.6, 30, 1395)));
    }

    #[test]
    fn title_events_fold_into_the_title_overlay() {
        use crate::net::NetUpdate;
        use lodestone_model::{ClientEvent, Text};

        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        // No title yet → nothing to draw.
        assert!(sim.title_overlay().is_none());

        feed.send(NetUpdate::TitleEvent(ClientEvent::TitleText {
            text: Text::literal("Welcome"),
        }))
        .unwrap();
        feed.send(NetUpdate::TitleEvent(ClientEvent::SubtitleText {
            text: Text::literal("to the server"),
        }))
        .unwrap();
        sim.poll_net();

        let (title, subtitle, _alpha) = sim
            .title_overlay()
            .expect("a server-sent title must reach the HUD accessor");
        assert_eq!(title, "Welcome");
        assert_eq!(subtitle.as_deref(), Some("to the server"));

        // A clear packet must empty the overlay again.
        feed.send(NetUpdate::TitleEvent(ClientEvent::TitlesCleared {
            reset_times: false,
        }))
        .unwrap();
        sim.poll_net();
        assert!(sim.title_overlay().is_none());
    }

    #[test]
    fn game_info_chat_folds_into_the_action_bar_not_the_feed() {
        use crate::net::NetUpdate;
        use lodestone_model::Text;

        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        assert!(sim.action_bar_overlay().is_none());

        feed.send(NetUpdate::ActionBar(Text::literal("Boss incoming")))
            .unwrap();
        sim.poll_net();

        let (text, alpha) = sim
            .action_bar_overlay()
            .expect("a GameInfo message must reach the action-bar accessor");
        assert_eq!(text, "Boss incoming");
        assert!(alpha > 0.0, "a fresh action-bar message is fully opaque");
        // It must not have leaked into the chat scrollback.
        assert!(
            sim.recent_chat(10).is_empty(),
            "GameInfo is the action bar, not chat — it must not enter the feed"
        );
    }

    /// The read-through the shell now depends on: it folds nothing itself, so
    /// the rows must come out of the **client's** one `SessionTabList`.
    ///
    /// `ingest_session_event` runs the same `lodestone_ecs::session` systems the
    /// real net thread runs (see `NetClient::session`); what this pins is the
    /// chain `component → NetClient::tab_list → Sim::player_rows`, which is
    /// exactly what the deleted `NetUpdate::TabListEvent` fold used to short.
    #[test]
    fn tab_overlay_rows_read_the_clients_one_folded_tab_list() {
        use lodestone_model::{ClientEvent, GameMode, PlayerListEntry, Text};
        use uuid::Uuid;

        let (net, _actions, _feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);

        let alice = Uuid::from_u128(1);
        let bob = Uuid::from_u128(2);
        let ingest = |sim: &Sim, event: ClientEvent| {
            sim.net().expect("net attached").ingest_session_event(event);
        };
        ingest(
            &sim,
            ClientEvent::PlayerListUpdate {
                entries: vec![
                    PlayerListEntry {
                        uuid: bob,
                        name: Some("Bob".into()),
                        game_mode: Some(GameMode::Spectator),
                        latency: Some(30),
                        display_name: None,
                        listed: Some(true),
                    },
                    PlayerListEntry {
                        uuid: alice,
                        name: Some("Alice".into()),
                        game_mode: Some(GameMode::Survival),
                        latency: Some(12),
                        display_name: Some(Text::literal("Alice the Brave")),
                        listed: Some(true),
                    },
                ],
            },
        );

        assert_eq!(
            sim.player_rows(),
            vec!["Alice the Brave  12ms".to_string(), "Bob  30ms".to_string(),],
            "tab overlay rows must come from the client's folded TabList state"
        );

        ingest(
            &sim,
            ClientEvent::PlayerListRemove {
                profile_ids: vec![alice],
            },
        );
        assert_eq!(sim.player_rows(), vec!["Bob  30ms".to_string()]);
    }

    /// The negative control for the pair above: with no connection there is no
    /// session `World` to read, so both projections must be empty rather than
    /// falling back to some shell-local copy — which is the assertion that
    /// `Sim` really holds neither aggregate any more.
    #[test]
    fn without_a_connection_the_shell_has_no_session_state_of_its_own() {
        let sim = Sim::new(test_config());
        assert!(sim.player_rows().is_empty());
        assert!(sim.sidebar().is_none());
        assert!(sim.boss_bars().is_empty());
    }

    /// The scoreboard twin of the tab-list read-through above.
    #[test]
    fn sidebar_rows_read_the_clients_one_folded_scoreboard() {
        use lodestone_model::event::{DisplaySlot, ObjectiveMode, ObjectiveRenderType};
        use lodestone_model::{ClientEvent, Text};

        let (net, _actions, _feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);

        for event in [
            ClientEvent::ObjectiveUpdate {
                name: "kills".into(),
                mode: ObjectiveMode::Add,
                display_name: Some(Text::literal("Kills")),
                render_type: Some(ObjectiveRenderType::Integer),
                number_format: None,
            },
            ClientEvent::DisplayObjective {
                slot: DisplaySlot::Sidebar,
                objective: Some("kills".into()),
            },
            ClientEvent::ScoreUpdate {
                holder: "Alice".into(),
                objective: "kills".into(),
                value: 7,
                display: Some(Text::literal("Alice the Brave")),
                number_format: None,
            },
            ClientEvent::ScoreUpdate {
                holder: "Bob".into(),
                objective: "kills".into(),
                value: 3,
                display: None,
                number_format: None,
            },
        ] {
            sim.net()
                .expect("net attached")
                .ingest_session_event(event);
        }

        let sidebar = sim.sidebar().expect("sidebar objective should be visible");
        assert_eq!(sidebar.title, "Kills");
        let rows: Vec<(&str, &str)> = sidebar
            .lines
            .iter()
            .map(|line| (line.label.as_str(), line.score.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![("Alice the Brave", "7"), ("Bob", "3")],
            "sidebar rows must come from the client's folded Scoreboard state"
        );
    }

    #[test]
    fn hotbar_selection_updates_and_echoes_to_the_server() {
        use lodestone_client::ClientAction;
        let (net, actions, _feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);

        // Vanilla default is slot 0, and selecting it again is a no-op (no
        // redundant packet).
        assert_eq!(sim.selected_slot(), 0);
        sim.select_slot(0);

        // A direct selection moves and echoes exactly one SetCarriedItem.
        sim.select_slot(3);
        assert_eq!(sim.selected_slot(), 3);

        // Out-of-range is ignored (no 10th slot), leaving selection and the
        // wire untouched.
        sim.select_slot(9);
        assert_eq!(sim.selected_slot(), 3);

        // Scroll wraps at both ends: +1 from 3 → 4, and from 8 → 0.
        sim.cycle_slot(1);
        assert_eq!(sim.selected_slot(), 4);
        sim.select_slot(8);
        sim.cycle_slot(1);
        assert_eq!(
            sim.selected_slot(),
            0,
            "scroll past the last slot wraps to 0"
        );
        sim.cycle_slot(-1);
        assert_eq!(
            sim.selected_slot(),
            8,
            "scroll before the first slot wraps to 8"
        );

        let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        // Every *change* echoes SetCarriedItem; the no-op select_slot(0) and the
        // rejected select_slot(9) send nothing, so the wire shows only the moves.
        assert_eq!(
            sent,
            vec![
                ClientAction::SetCarriedItem { slot: 3 },
                ClientAction::SetCarriedItem { slot: 4 },
                ClientAction::SetCarriedItem { slot: 8 },
                ClientAction::SetCarriedItem { slot: 0 },
                ClientAction::SetCarriedItem { slot: 8 },
            ],
            "only real selection changes reach the outbound action seam"
        );
    }

    #[test]
    fn camera_interpolates_between_ticks() {
        // Force a known prev/current split and a half-way alpha, then check the
        // camera eye sits between the two feet positions.
        let mut sim = Sim::new(test_config());
        sim.set_prev_position(Vec3d::new(0.0, 64.0, 0.0));
        sim.player_mut(|p| p.position = Vec3d::new(10.0, 64.0, 0.0));
        sim.clock_mut(|c| c.interp_alpha = 0.5);
        let cam = sim.camera(1.0);
        assert!(
            (cam.position.x - 5.0).abs() < 1e-4,
            "expected midpoint x=5, got {}",
            cam.position.x
        );
    }

    #[test]
    fn frames_per_tick_tracks_ratio() {
        let mut sim = Sim::new(test_config());
        // Two frames of one full tick each ⇒ 2 frames / 2 ticks = 1.0.
        sim.step(1.0 / 20.0);
        sim.step(1.0 / 20.0);
        assert!((sim.frames_per_tick() - 1.0).abs() < 1e-6);
        // A frame with no accumulated tick still counts as a frame, so the
        // frames-per-tick ratio rises above 1.
        sim.step(0.0);
        assert!(sim.frames_per_tick() > 1.0, "extra frame raises the ratio");
    }

    #[test]
    fn sprint_moves_faster_than_walk_via_attribute_seam() {
        // Walk forward for a second, then sprint the same time from the same
        // spot; sprinting must cover more ground. This drives the physics
        // `with_movement_speed` seam from a real caller.
        //
        // The local world is now real vanilla terrain (`lodestone-worldgen`),
        // so spawn sits on a slope and walking north walls the player out after
        // ~0.2 blocks — a wall, not the speed seam, would otherwise decide the
        // result. Flatten a private corridor along the walking line so what we
        // measure is physics speed and nothing else.
        fn distance(sprint: bool) -> f64 {
            let mut sim = Sim::new(test_config());
            // Player spawns at (0.5, feet, 0.5) facing north (-Z, yaw 180).
            // Lay a solid floor and clear head-room along -Z so the walk is
            // unobstructed regardless of the generated surface.
            let feet_y = sim.player().position.y.floor() as i32;
            for dz in -25..=1 {
                for dx in -1..=1 {
                    sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
                    sim.set_block_world([dx, feet_y, dz], id::AIR);
                    sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
                    sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
                }
            }
            // Settle on the fresh floor first.
            for _ in 0..20 {
                sim.step(1.0 / 20.0);
            }
            let start = sim.player().position;
            sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
            sim.input_mut(|i| i.set(lodestone_controller::Action::Sprint, sprint));
            for _ in 0..20 {
                sim.step(1.0 / 20.0);
            }
            let d = sim.player().position.subtract(start);
            (d.x * d.x + d.z * d.z).sqrt()
        }
        let walk = distance(false);
        let sprint = distance(true);
        assert!(
            sprint > walk * 1.1,
            "sprint ({sprint:.3}) should clearly exceed walk ({walk:.3})"
        );
    }

    /// Swimming has to reach the *player*, not just exist in the physics crate.
    /// Flood a pool in the demo world (whose palette has a real water block), hold
    /// sprint + forward, and check the pose actually flips: `swimming` set, the eye
    /// dropped to `Pose.SWIMMING`'s `0.4`, and the camera moved with it.
    ///
    /// The first phase is the control: standing in exactly the same water without
    /// sprinting must **not** swim, so the assertions below are about sprinting
    /// while submerged and not about "being wet".
    #[test]
    fn sprinting_underwater_enters_the_swim_pose_and_drops_the_camera() {
        let mut sim = Sim::new(test_config());
        let feet_y = sim.player().position.y.floor() as i32;
        // A private pool: stone floor, water from the feet to well over the eye,
        // wide enough that a second of swimming (~1 block) stays inside it. Filling
        // the column with water is also what flattens the generated slope the player
        // spawns on — see `sprint_moves_faster_than_walk_via_attribute_seam`.
        for dz in -5..=5 {
            for dx in -5..=5 {
                sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
                for dy in 0..=4 {
                    sim.set_block_world([dx, feet_y + dy, dz], id::WATER);
                }
            }
        }

        for _ in 0..10 {
            sim.step(1.0 / 20.0);
        }
        assert!(
            sim.fluid_state().under_water(),
            "the pool must actually submerge the eye, or this gate proves nothing"
        );
        assert!(
            !sim.player().swimming,
            "control: submerged but not sprinting is not swimming"
        );
        assert_eq!(
            sim.player().eye_height,
            lodestone_physics::player::DEFAULT_EYE_HEIGHT
        );

        sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
        sim.input_mut(|i| i.set(lodestone_controller::Action::Sprint, true));
        // Step until the pose flips, so the tick the change lands on is known.
        let mut ticks_to_swim = None;
        for tick in 0..10 {
            sim.step(1.0 / 20.0);
            if sim.player().swimming {
                ticks_to_swim = Some(tick);
                break;
            }
        }
        assert!(
            ticks_to_swim.is_some(),
            "sprinting while submerged must enter the swim pose"
        );
        assert_eq!(
            sim.player().eye_height, SWIMMING_EYE_HEIGHT,
            "the shell owns the pose eye height; physics only reads it"
        );

        // Helper: pin the *position* interpolation so a camera assertion is about
        // the eye height, not about where between two ticks the feet are.
        //
        // `alpha` is deliberately a parameter, because it selects **which** of the
        // smoother's two values you see: `lerp(0.0)` is the *previous* tick's eased
        // eye height and `lerp(1.0)` is this tick's. That is the whole point of the
        // `O` twin, and reading at `0.0` right after a pose flip therefore shows the
        // pre-flip height — correct, and not what a mid-ease assertion wants.
        let camera_offset = |sim: &mut Sim, alpha: f32| {
            let settled = sim.player().position;
            sim.set_prev_position(settled);
            sim.clock_mut(|c| c.interp_alpha = alpha);
            sim.camera(1.0).position.y - sim.player().position.y as f32
        };

        // **The camera must NOT have snapped.** `Camera.tick()` eases its own eye
        // height toward the entity's — `eyeHeight += (target - eyeHeight) * 0.5F` —
        // so one tick after the pose flips it is still most of the way up at the
        // standing height. This is the assertion that proves `Sim::camera` reads
        // `eye_height_smoother` and not the raw pose value; before that existed the
        // view jerked 1.22 blocks in a single frame on entering water.
        let standing = lodestone_physics::player::DEFAULT_EYE_HEIGHT;
        let after_flip = camera_offset(&mut sim, 1.0);
        assert!(
            after_flip > SWIMMING_EYE_HEIGHT + 0.1 && after_flip < standing,
            "camera should be mid-ease between {SWIMMING_EYE_HEIGHT} and {standing} \
             one tick after the pose flip, got {after_flip}"
        );

        // …and it must converge. Each tick halves the remaining gap, so the
        // original `1e-4` tolerance needs ~14 ticks from a 1.22-block step; 24 is
        // comfortably past it without being sensitive to the exact rate.
        for _ in 0..24 {
            sim.step(1.0 / 20.0);
        }
        let settled_offset = camera_offset(&mut sim, 1.0);
        assert!(
            (settled_offset - SWIMMING_EYE_HEIGHT).abs() < 1e-4,
            "swim camera should settle {SWIMMING_EYE_HEIGHT} above the feet: got \
             {settled_offset}"
        );
    }

    /// Sneak is how you swim *downward* (`goDownInWater`), so the land-side
    /// "sneaking cancels sprint" gate must not apply while submerged — otherwise
    /// holding shift underwater stops the swim dead. Control: the same shift+sprint
    /// on dry land still cancels sprint.
    ///
    /// The *rule* now lives in `lodestone_controller::swim_adjusted_intent` and
    /// is tested there against the pure function, and in that crate's
    /// `the_intent_system_reads_submersion_for_the_swim_exception` against the
    /// system. This one is deliberately kept as well, and asserts something
    /// neither of those can: that a `Sim::step` — the real driver, with the real
    /// `RawInput` resource and the real `Submersion` component — reaches the
    /// intent the physics set will read. Without it, `Sim` could stop feeding the
    /// ECS entirely and both of the controller's tests would still pass.
    #[test]
    fn sneak_cancels_sprint_on_land_but_not_under_water() {
        let mut sim = Sim::new(test_config());
        sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
        sim.input_mut(|i| i.set(lodestone_controller::Action::Sprint, true));
        sim.input_mut(|i| i.set(lodestone_controller::Action::Sneak, true));

        sim.step(lodestone_ecs::TICK_PERIOD);
        assert!(
            !sim.movement_intent().sprint,
            "control: on land, sneaking still vetoes sprint"
        );

        sim.set_fluid_state(FluidState {
            water_height: 2.0,
            eye_in_water: true,
            ..FluidState::NONE
        });
        sim.step(lodestone_ecs::TICK_PERIOD);
        let intent = sim.movement_intent();
        assert!(
            intent.sprint,
            "submerged, shift must not cancel a swim-sprint"
        );
        assert!(
            intent.sneak,
            "…and shift itself must survive, or the sink impulse is lost"
        );
    }

    /// The server derives the swimming pose itself, from `isSprinting()` — and it
    /// only learns that from `ServerboundPlayerCommandPacket`, never from the input
    /// packet's `sprint` bit. So the sprint *edge* has to reach the wire as a
    /// `PlayerCommand`, exactly once per change.
    #[test]
    fn sprint_edges_reach_the_wire_as_player_commands() {
        use crate::net::NetUpdate;
        use lodestone_ecs::ecs::system::RunSystemOnce;
        use lodestone_model::PlayerCommand;

        let (net, actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        // Both halves of one login packet, because the packet carries the entity id
        // and `send_sprint_command` will not send without one. `NetUpdate::LoggedIn`
        // drives the phase (and therefore `Egress::in_world`); `ClientEvent::Login`
        // is what folds `ServerEntityId`, on the net thread, since the vitals
        // collapse deleted `poll_net`'s duplicate `set_server_entity_id` write.
        // Feeding only the `NetUpdate` left the id `None`, which made the whole
        // test a *precondition*-species vacuity: the query hit
        // `let Some(entity_id) = … else { continue }` every time, so the two
        // "no packet" assertions below held for a reason that had nothing to do
        // with edge-triggering.
        ingest(&mut sim, login_event(7));
        feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
        sim.poll_net();
        assert_eq!(
            sim.server_entity_id(),
            Some(7),
            "setup: without the folded id no sprint command can be sent at all, \
             and every assertion below passes vacuously"
        );
        while actions.try_recv().is_ok() {}

        let drain = |actions: &std::sync::mpsc::Receiver<ClientAction>| -> Vec<ClientAction> {
            std::iter::from_fn(|| actions.try_recv().ok()).collect()
        };

        // Since Stage 5 the sprint edge is `crate::interact::send_sprint_command`,
        // a `TickSet::Send` system. Run *that system* and then the driver's own
        // queue drain, rather than the whole `GameTick` schedule: the schedule also
        // emits the per-tick movement packet, which would swamp the
        // "no edge, no packet" assertions below. Deliberately **not** an assertion
        // on `ActionQueue` — the queue is not the wire, and this test's whole point
        // is that the command reaches the socket.
        //
        // `Egress` has to be set by hand for the same reason the old direct call
        // needed no gate: the demo fixture has no vanilla atlas, so `is_live()` is
        // false and `step` would derive `live: false`. The gate moved from the call
        // site into the system, which is where `send_player_input` already keeps
        // its identical one.
        let sprint_once = |sim: &mut Sim| {
            {
                let mut world = sim.ecs().write();
                world.insert_resource(Egress {
                    in_world: true,
                    live: true,
                });
                world
                    .run_system_once(crate::interact::send_sprint_command)
                    .expect("send_sprint_command runs");
            }
            sim.drain_action_queue();
        };

        // Not sprinting and never was: no packet at all (vanilla's `wasSprinting`
        // starts false).
        sprint_once(&mut sim);
        assert!(
            drain(&actions).is_empty(),
            "no sprint edge, no sprint packet"
        );

        sim.player_mut(|p| p.sprinting = true);
        sprint_once(&mut sim);
        assert_eq!(
            drain(&actions),
            vec![ClientAction::PlayerCommand {
                entity_id: 7,
                command: PlayerCommand::StartSprinting,
            }]
        );

        // Edge-triggered: holding sprint must not spam the server every tick.
        sprint_once(&mut sim);
        sprint_once(&mut sim);
        assert!(drain(&actions).is_empty(), "sprint is edge-triggered");

        sim.player_mut(|p| p.sprinting = false);
        sprint_once(&mut sim);
        assert_eq!(
            drain(&actions),
            vec![ClientAction::PlayerCommand {
                entity_id: 7,
                command: PlayerCommand::StopSprinting,
            }]
        );
    }

    #[test]
    fn breaking_the_target_clears_it_and_schedules_a_remesh() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        // Aim straight down at the block under the player's feet.
        let feet = sim.player().position;
        sim.set_target(Some(crate::raycast::RayHit {
            block: [
                feet.x.floor() as i32,
                feet.y.floor() as i32 - 1,
                feet.z.floor() as i32,
            ],
            normal: [0, 1, 0],
        }));
        assert!(sim.break_block(), "should break the solid block");
        assert!(sim.target().is_none(), "target cleared after break");
        assert!(sim.pending_meshes() > 0, "a remesh was scheduled");
    }

    // -----------------------------------------------------------------------
    // Arm swing: the producer -> consumer wiring
    // -----------------------------------------------------------------------
    //
    // `lodestone_entity::pose` proves the swing clock ticks and
    // `lodestone_render::entity` proves the arm matrix moves. Neither can prove
    // that anything in this shell ever *starts* a swing — the failure this repo
    // has hit nine times. These gates assert the seam: a swing produced the way
    // the real producers produce one reaches `hand_swing_progress` (which
    // `app.rs` hands `RenderState::set_hand_swing_source`) and
    // `third_person_body_state` (which feeds the self-avatar's
    // `setupAttackAnimation`).

    /// Aim straight down at the block under the player's feet, like
    /// `breaking_the_target_clears_it_and_schedules_a_remesh`.
    fn aim_at_the_floor(sim: &mut Sim) {
        let feet = sim.player().position;
        sim.set_target(Some(crate::raycast::RayHit {
            block: [
                feet.x.floor() as i32,
                feet.y.floor() as i32 - 1,
                feet.z.floor() as i32,
            ],
            normal: [0, 1, 0],
        }));
    }

    /// Run whole ticks and report the largest swing progress seen.
    fn peak_swing_over(sim: &mut Sim, ticks: u32) -> f32 {
        let mut peak = 0.0f32;
        for _ in 0..ticks {
            sim.step(1.0 / 20.0);
            peak = peak.max(sim.hand_swing_progress());
        }
        peak
    }

    #[test]
    fn a_queued_main_hand_swing_reaches_the_arm_pose() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();

        // The negative control first, and it is the one that matters: with no
        // swing produced, the arm must sit at exact rest for the whole window.
        // Without this, "progress > 0" is also satisfied by a clock that free-runs
        // off frame time — which is the specific bug `entities.rs` documents
        // finding in the limb-swing code.
        let idle_peak = peak_swing_over(&mut sim, 20);
        assert_eq!(
            idle_peak, 0.0,
            "an idle player's arm must be at rest, but progress peaked at {idle_peak}"
        );

        // Now produce a swing exactly the way `lodestone_game::mining` does — it
        // pushes `SwingArm { Main }` onto `ActionQueue`, and `drive_mining`
        // forwards that queue verbatim. `mining.rs`'s own tests already pin that
        // it emits one; this pins that the shell animates it.
        sim.write(|w| {
            w.resource_mut::<ActionQueue>()
                .0
                .push(ClientAction::SwingArm { hand: Hand::Main });
        });
        let peak = peak_swing_over(&mut sim, 10);
        assert!(
            peak > 0.4,
            "a queued main-hand swing must drive the arm pose, but progress \
             peaked at only {peak} — `drain_action_queue` is not calling `swing_hand`, \
             or `hand_swing_progress` is not reading the clock it sets"
        );

        // And it ends: the swing is 6 ticks, so well after that the arm is rested
        // again. A swing that never finishes reads as a permanently cocked arm.
        let after = peak_swing_over(&mut sim, 30);
        assert_eq!(
            after, 0.0,
            "the swing must return to rest, but progress still peaked at {after}"
        );
    }

    /// An **off-hand** swing must not drive the arm. `drain_action_queue` matches
    /// on `Hand::Main` specifically; without this control that match is untested
    /// and a `SwingArm { .. }` wildcard would swing the right arm for a left-hand
    /// action.
    #[test]
    fn an_off_hand_swing_does_not_drive_the_main_arm() {
        let mut sim = Sim::new(test_config());
        sim.write(|w| {
            w.resource_mut::<ActionQueue>()
                .0
                .push(ClientAction::SwingArm { hand: Hand::Off });
        });
        let peak = peak_swing_over(&mut sim, 10);
        assert_eq!(
            peak, 0.0,
            "an off-hand swing must leave the main arm at rest, got {peak}"
        );
    }

    /// The demo world has no action queue to piggy-back on, so `break_block` and
    /// `place_block` start the swing themselves. This is the only world a headless
    /// scene can exercise, so if it did not swing, no offline gate ever could.
    #[test]
    fn a_demo_world_break_swings_the_arm() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        aim_at_the_floor(&mut sim);
        // Load-bearing: if the break did not happen this test would pass
        // vacuously by asserting nothing about a swing that was never produced.
        assert!(sim.break_block(), "the demo block should have broken");
        let peak = peak_swing_over(&mut sim, 10);
        assert!(
            peak > 0.4,
            "a demo-world break must swing the arm, progress peaked at {peak}"
        );
    }

    /// Issue #72: a demo-world left-click with **nothing** targeted must still
    /// swing — vanilla's `Minecraft.startAttack` reaches `player.swing(...)`
    /// unconditionally after the switch, `MISS` included. Before this fix
    /// `Sim::begin_attack` called `break_block()` alone on the demo world,
    /// which swings only on a *successful* break and produces nothing when
    /// there is no target.
    #[test]
    fn begin_attack_swings_the_arm_on_a_demo_world_miss() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        assert!(
            sim.target().is_none(),
            "test setup: nothing should be targeted yet"
        );
        sim.begin_attack();
        let peak = peak_swing_over(&mut sim, 10);
        assert!(
            peak > 0.4,
            "a miss must still swing the arm (issue #72), progress peaked at {peak}"
        );
    }

    /// Regression companion to the miss test above: routing `begin_attack`
    /// through the new demo/live split must not break the existing
    /// successful-break path.
    #[test]
    fn begin_attack_still_breaks_a_targeted_demo_block() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        aim_at_the_floor(&mut sim);
        sim.begin_attack();
        assert!(
            sim.target().is_none(),
            "a successful break clears the target, as `break_block` always did"
        );
        let peak = peak_swing_over(&mut sim, 10);
        assert!(
            peak > 0.4,
            "breaking a targeted demo block must still swing, progress peaked at {peak}"
        );
    }

    /// Issue #72's live-path miss case: no block, no entity, and the arm still
    /// swings. Exercises `begin_attack_live` directly (no net connection is
    /// needed — the swing is client-side and does not require one, matching
    /// every other swing site's contract).
    #[test]
    fn begin_attack_live_swings_on_a_miss() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        assert!(sim.target().is_none());
        assert!(sim.entity_target().is_none());
        sim.begin_attack_live();
        let peak = peak_swing_over(&mut sim, 10);
        assert!(
            peak > 0.4,
            "a live miss must still swing the arm, progress peaked at {peak}"
        );
    }

    /// The `BLOCK`-only case: with no entity targeted, `begin_attack_live`
    /// must still arm the hold-to-mine loop exactly as it did before this
    /// change (the pre-existing, unmodified behaviour this fix must not
    /// regress).
    #[test]
    fn begin_attack_live_arms_mining_when_only_a_block_is_targeted() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        aim_at_the_floor(&mut sim);
        sim.begin_attack_live();
        let attacking = sim.read(|w| w.resource::<Attacking>().0);
        assert!(
            attacking,
            "a block-only target must still arm the hold-to-mine loop"
        );
    }

    /// `case ENTITY` takes priority over `case BLOCK`: with both an entity and
    /// a block targeted, attacking the entity must swing the arm and must
    /// **not** also arm the hold-to-mine loop — vanilla's `hitResult` is one
    /// value, never both at once.
    #[test]
    fn begin_attack_live_prefers_an_entity_target_over_mining() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        aim_at_the_floor(&mut sim);
        sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));
        sim.begin_attack_live();
        let peak = peak_swing_over(&mut sim, 10);
        assert!(
            peak > 0.4,
            "attacking an entity target must swing the arm, progress peaked at {peak}"
        );
        let attacking = sim.read(|w| w.resource::<Attacking>().0);
        assert!(
            !attacking,
            "an entity attack must not also arm the hold-to-mine loop"
        );
    }

    /// A dead local player must not attack — mirrors `use_item_live`'s own
    /// `is_dead()` guard, and vanilla drops input entirely on the death
    /// screen.
    #[test]
    fn begin_attack_live_does_nothing_while_dead() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        let local = sim.local_player();
        sim.write(|w| {
            w.entity_mut(local).insert(Dead);
            w.resource_mut::<EntityRayTarget>().0 = Some(42);
        });
        sim.begin_attack_live();
        let peak = peak_swing_over(&mut sim, 10);
        assert_eq!(peak, 0.0, "a dead player must not swing on attack");
    }

    /// The geometric half of entity targeting: [`Sim::update_entity_target`]
    /// must find a spawned entity the ray points straight at, and report it
    /// by its server (`MinecraftEntityId`), never a `bevy_ecs::Entity`.
    #[test]
    fn update_entity_target_finds_a_spawned_entity_along_the_ray() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        let feet = sim.player().position;
        ingest(
            &mut sim,
            lodestone_client::ClientEvent::EntitySpawned {
                entity_id: 99,
                uuid: None,
                entity_type: "minecraft:pig".parse().expect("valid entity type key"),
                pos: lodestone_model::Vec3::new(feet.x + 2.0, feet.y, feet.z),
                rotation: Rotation::new(0.0, 0.0),
                velocity: None,
            },
        );
        // A horizontal ray at a height just above the pig's own feet — safely
        // inside any real pig hitbox's vertical span without needing to know
        // its exact height, and well below a human eye height (1.6), which
        // would sail clean over a pig-sized box on a perfectly level ray.
        let origin = [feet.x, feet.y + 0.1, feet.z];
        let dir = [1.0, 0.0, 0.0];
        sim.update_entity_target(origin, dir, None);
        assert_eq!(
            sim.entity_target(),
            Some(99),
            "the ray should find the spawned pig by its server entity id"
        );
    }

    /// An entity past [`ENTITY_REACH`] must not be targetable, even though it
    /// is well within block [`REACH`] — vanilla's shorter entity-interaction
    /// range, not the block one.
    #[test]
    fn update_entity_target_ignores_an_entity_beyond_entity_reach() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        let feet = sim.player().position;
        ingest(
            &mut sim,
            lodestone_client::ClientEvent::EntitySpawned {
                entity_id: 7,
                uuid: None,
                entity_type: "minecraft:pig".parse().expect("valid entity type key"),
                // Within block REACH (4.5) but past ENTITY_REACH (3.0).
                pos: lodestone_model::Vec3::new(feet.x + 4.0, feet.y, feet.z),
                rotation: Rotation::new(0.0, 0.0),
                velocity: None,
            },
        );
        // Same height convention as `update_entity_target_finds_a_spawned_entity_along_the_ray`
        // — this must fail on *reach*, not on the ray sailing over the box.
        let origin = [feet.x, feet.y + 0.1, feet.z];
        let dir = [1.0, 0.0, 0.0];
        sim.update_entity_target(origin, dir, None);
        assert_eq!(
            sim.entity_target(),
            None,
            "an entity beyond entity-interaction range must not be targetable"
        );
    }

    /// Issue #12's knockback half: a `ClientboundSetEntityMotionPacket`
    /// (`ClientEvent::EntityVelocity`) naming the local player's own server
    /// entity id must overwrite `PlayerState.velocity` outright — vanilla's
    /// `Entity.lerpMotion` is `setDeltaMovement(movement)`, an unconditional
    /// replace, and `LocalPlayer` declares no override (`Entity.java:2649-2651`).
    /// Before this fix the event fell into the generic `Velocity` component
    /// instead, which nothing reads for the local player, so a server-applied
    /// hit never moved the client at all.
    #[test]
    fn server_sent_knockback_replaces_the_local_players_velocity() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        ingest(&mut sim, login_event(3));
        assert_eq!(
            sim.player().velocity,
            Vec3d::ZERO,
            "test setup: a fresh player starts at rest"
        );
        ingest(
            &mut sim,
            lodestone_client::ClientEvent::EntityVelocity {
                entity_id: 3,
                velocity: lodestone_model::Vec3::new(1.0, 2.0, -3.0),
            },
        );
        assert_eq!(
            sim.player().velocity,
            Vec3d::new(1.0, 2.0, -3.0),
            "knockback naming our own id must land in PlayerState.velocity, \
             the field `player_physics` actually integrates"
        );
    }

    /// The swing is a **tick** state machine. Reading it across many sub-tick
    /// frames must not advance it — the defect
    /// `limb_swing_tracks_per_tick_travel_not_the_interpolation_gap` records for
    /// the walk cycle, where a per-frame drive made the animation up to 3x too
    /// fast and frame-rate dependent.
    #[test]
    fn swing_progress_is_tick_driven_not_frame_driven() {
        let mut sim = Sim::new(test_config());
        sim.swing_hand();
        sim.step(1.0 / 20.0); // one whole tick: the clock starts
        sim.step(1.0 / 20.0); // and advances once
        let after_two_ticks = sim.hand_swing_progress();

        // 200 sub-tick frames at 1 ms. `FrameClock` accumulates them, so a few
        // whole ticks *will* elapse across 200 ms — the claim is not "nothing
        // changes", it is that the change tracks elapsed *ticks*, so 200 tiny
        // frames advance the swing no further than the 4 ticks their total
        // duration contains.
        for _ in 0..200 {
            sim.step(0.001);
        }
        let after_frames = sim.hand_swing_progress();
        let ticks_elapsed = 4; // 200 ms / 50 ms
        let ceiling = after_two_ticks + (ticks_elapsed + 1) as f32 / 6.0;
        assert!(
            after_frames <= ceiling,
            "200 sub-tick frames advanced the swing to {after_frames}, past the {ceiling} \
             that {ticks_elapsed} ticks of elapsed time allows — the clock is being \
             driven per frame"
        );
    }

    /// Both consumers read the same clock, so the first-person arm and the
    /// self-avatar's body can never disagree about where in the swing we are.
    #[test]
    fn the_third_person_body_swings_off_the_same_clock_as_the_arm() {
        let mut sim = Sim::new(test_config());
        sim.toggle_third_person();
        sim.swing_hand();
        // Step to a tick where the swing is genuinely mid-arc, so `assert_eq` is
        // comparing something other than two zeroes.
        let mut arm = 0.0;
        for _ in 0..4 {
            sim.step(1.0 / 20.0);
            arm = sim.hand_swing_progress();
            if arm > 0.1 {
                break;
            }
        }
        assert!(arm > 0.1, "the swing should be mid-arc, got {arm}");
        let body = sim
            .third_person_body_state()
            .expect("third person is on")
            .anim
            .attack_anim;
        assert!(
            (body - arm).abs() < 1e-6,
            "the self-avatar's attack_anim ({body}) must match the arm's ({arm})"
        );
    }

    #[test]
    fn chunk_dirty_signal_reschedules_a_loaded_column() {
        // A `ChunkLoaded`/`NetUpdate::Chunk { x, z }` signal must re-mesh the
        // column it names (the §12.24 dirty-region trigger), so the live-world
        // swap is a source change, not new plumbing.
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        assert_eq!(sim.pending_meshes(), 0, "drained to a clean slate");
        let pos = *sim
            .chunk_world()
            .read()
            .iter()
            .next()
            .expect("local world has a column")
            .0;
        let (cx, cz) = (pos.x, pos.z);
        sim.mark_column_dirty(cx, cz);
        assert!(
            sim.pending_meshes() > 0,
            "the loaded column was re-scheduled"
        );
    }

    #[test]
    fn chunk_arrival_also_remeshes_its_loaded_neighbours() {
        // A section's geometry depends on its whole 3×3×3 neighbourhood, so a
        // column meshed before its neighbour loaded baked its seam against air —
        // which is what puts a falling water "wall" at every chunk border. The
        // arrival signal must therefore dirty the eight loaded neighbours too.
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        let pos = *sim
            .chunk_world()
            .read()
            .iter()
            .next()
            .expect("local world has a column")
            .0;
        // Pick a column with at least one loaded horizontal neighbour.
        let (cx, cz) = (pos.x, pos.z);
        let neighbours: Vec<(i32, i32)> = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dz| (dx, dz)))
            .filter(|&(dx, dz)| (dx, dz) != (0, 0))
            .map(|(dx, dz)| (cx + dx, cz + dz))
            .filter(|&(nx, nz)| sim.chunk_world().contains_column(nx, nz))
            .collect();
        assert!(
            !neighbours.is_empty(),
            "fixture must have a loaded neighbour, else this asserts nothing"
        );

        sim.on_column_arrived(cx, cz);
        // `heal_dirty_columns` is an `Update` system now; run the schedule the way
        // `Sim::step` does rather than calling a method. `DIRTY_COLUMN_BUDGET` is
        // 4 and the fixture has up to 8 loaded neighbours, so drive it until the
        // dirty set is empty.
        while !sim.terrain(|t| t.dirty_columns.is_empty()) {
            sim.ecs().write().run_schedule(lodestone_ecs::Update);
        }
        let _ = neighbours.len();
        let meshed: HashSet<(i32, i32)> = sim
            .drain_all_meshes()
            .into_iter()
            .map(|m| (m.key.cx, m.key.cz))
            .chain(sim.drain_removals().into_iter().map(|k| (k.cx, k.cz)))
            .collect();

        assert!(meshed.contains(&(cx, cz)), "the arriving column was meshed");
        for n in &neighbours {
            assert!(
                meshed.contains(n),
                "loaded neighbour {n:?} was not re-meshed — its seam stays baked \
                 against air (the chunk-border water wall)"
            );
        }
    }

    #[test]
    fn neighbour_remesh_skips_columns_that_are_not_loaded() {
        // The control for the test above: queueing absent columns would mesh
        // nothing, log a drop, and let "every arrival dirties 8 neighbours" pass
        // without any of them being real.
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        sim.on_column_arrived(9999, 9999);
        assert!(
            sim.terrain(|t| t.dirty_columns.is_empty()),
            "no neighbour of an out-of-world column is loaded, so none is queued"
        );
    }

    #[test]
    fn chunk_dirty_signal_ignores_an_absent_column() {
        // Columns we don't hold (e.g. before the live world source is wired in)
        // must be a no-op, never a panic or spurious work.
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        sim.mark_column_dirty(9999, 9999);
        assert_eq!(sim.pending_meshes(), 0, "absent column schedules nothing");
    }

    #[test]
    fn placing_against_a_face_adds_a_block() {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        let feet = sim.player().position;
        // Target a floor block a few blocks away (clear of the player AABB),
        // place on its top face.
        let bx = feet.x.floor() as i32 + 3;
        let bz = feet.z.floor() as i32;
        let s = crate::worldgen::surface_height(bx, bz);
        sim.set_target(Some(crate::raycast::RayHit {
            block: [bx, s, bz],
            normal: [0, 1, 0],
        }));
        {
            let store = sim.chunk_world();
            let world = store.read();
            let view = WorldCollision::new(&world);
            assert_eq!(view.block_at(bx, s + 1, bz), id::AIR, "cell starts empty");
        }
        assert!(sim.place_block(), "should place onto the top face");
        let store = sim.chunk_world();
        let world = store.read();
        let view = WorldCollision::new(&world);
        assert_ne!(view.block_at(bx, s + 1, bz), id::AIR, "block now present");
    }

    #[test]
    fn cannot_place_inside_the_player() {
        let mut sim = Sim::new(test_config());
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        let feet = sim.player().position;
        // Target the block under the feet, whose top face is where the player
        // stands — placing there would clip the player, so it must be refused.
        sim.set_target(Some(crate::raycast::RayHit {
            block: [
                feet.x.floor() as i32,
                feet.y.floor() as i32 - 1,
                feet.z.floor() as i32,
            ],
            normal: [0, 1, 0],
        }));
        assert!(!sim.place_block(), "placing inside the player is refused");
    }

    #[test]
    fn fly_mode_ignores_gravity() {
        let mut sim = Sim::new(test_config());
        sim.toggle_fly();
        assert!(sim.flying());
        let y0 = sim.player().position.y;
        // No vertical input: fly holds altitude (physics-walk would fall).
        for _ in 0..40 {
            sim.step(1.0 / 20.0);
        }
        assert!(
            (sim.player().position.y - y0).abs() < 1e-9,
            "fly holds altitude"
        );
        // Jump ascends.
        sim.input_mut(|i| i.set(lodestone_controller::Action::Jump, true));
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        assert!(sim.player().position.y > y0, "jump lifts in fly mode");
    }

    #[test]
    fn an_interior_block_change_dirties_exactly_its_own_section() {
        // Local (8,8,8) touches no section boundary, so a live block update
        // there must cost one re-mesh — not the 27 a blanket neighbourhood
        // would submit, and not the ~216 a whole-column signal would.
        let dirty = dirty_sections_for_blocks(3, 4, 5, &[[8, 8, 8]]);
        assert_eq!(
            dirty.iter().copied().collect::<Vec<_>>(),
            vec![(3, 4, 5)],
            "an interior cell reaches no neighbouring section"
        );
    }

    #[test]
    fn a_block_change_on_a_face_also_dirties_that_neighbour() {
        // The bug this pins: breaking a block at local x=15 on a live server
        // leaves the +x neighbour's face baked against the *old* state, which
        // shows as a stale face or z-fighting at every chunk border while
        // mining. The -x neighbour must NOT be dirtied — that is the half of
        // the filter a "dirty all 27" implementation gets wrong.
        let dirty = dirty_sections_for_blocks(3, 4, 5, &[[15, 8, 8]]);
        assert_eq!(
            dirty.iter().copied().collect::<Vec<_>>(),
            vec![(3, 4, 5), (4, 4, 5)],
            "a +x face cell dirties its own section and the +x neighbour only"
        );
    }

    #[test]
    fn a_corner_block_change_dirties_the_full_corner_octant() {
        // (0,0,0) touches three faces, three edges and one corner: 8 sections.
        // Edge and corner neighbours matter because AO samples the 3 cells
        // around each vertex, which reach diagonally across section corners.
        let dirty = dirty_sections_for_blocks(0, 0, 0, &[[0, 0, 0]]);
        assert_eq!(dirty.len(), 8, "a corner cell reaches an octant: {dirty:?}");
        assert!(dirty.contains(&(-1, -1, -1)), "the diagonal corner is included");
        assert!(!dirty.contains(&(1, 0, 0)), "the far side is not reachable");
    }

    #[test]
    fn a_whole_section_update_is_bounded_by_the_neighbourhood_not_the_cell_count() {
        // A 4096-cell `SECTION_BLOCKS_UPDATE` (a full section rewrite) must not
        // submit 4096 re-meshes. 27 is the hard ceiling because that is the
        // entire neighbourhood any cell in the section can reach.
        let all: Vec<[u8; 3]> = (0..16u8)
            .flat_map(|x| (0..16u8).flat_map(move |y| (0..16u8).map(move |z| [x, y, z])))
            .collect();
        assert_eq!(all.len(), 4096, "control: the fixture really is a full section");
        let dirty = dirty_sections_for_blocks(0, 0, 0, &all);
        assert_eq!(dirty.len(), 27, "bounded by the 3x3x3 neighbourhood");
    }

    // -----------------------------------------------------------------------
    // §4.1(c): one `World`, one `GameTick`, one accumulator
    // -----------------------------------------------------------------------

    /// **The (c) authority test.** One `World` means one `LocalPlayer`.
    ///
    /// `spawn_local_player` and `spawn_session` both spawn an entity carrying the
    /// `LocalPlayer` marker. They used to be in different `World`s, so both could
    /// exist; in one `World` they have to be one entity, or every
    /// `With<LocalPlayer>` system (`tick_hud_overlays`, the physics and egress
    /// systems) silently runs against two players and the HUD reads whichever the
    /// query happened to yield.
    #[test]
    fn the_one_world_holds_exactly_one_local_player() {
        let sim = Sim::new(test_config());
        assert_eq!(local_player_count(sim.ecs()), 1);
        // …and it is the entity the driver named, not some other one.
        assert!(
            sim.ecs()
                .read()
                .get::<lodestone_ecs::SessionScoreboard>(sim.local_player())
                .is_some(),
            "the session fold's components must hang off Sim's own local player"
        );
    }

    /// The control that proves the count above discriminates: spawning the session
    /// entity separately — which is exactly what
    /// `lodestone_client::state::SharedState::default` does when it is *not* handed
    /// a `World` — takes it to two.
    #[test]
    fn a_separately_spawned_session_entity_makes_two_local_players() {
        let sim = Sim::new(test_config());
        lodestone_ecs::spawn_session(&mut sim.ecs().write());
        assert_eq!(
            local_player_count(sim.ecs()),
            2,
            "the detector must be able to see a second LocalPlayer"
        );
    }

    /// Note the shape: **one** guard, named, then queried.
    ///
    /// The obvious spelling — `handle.write().query_filtered::<…>().iter(&handle.write())`
    /// — takes the write lock twice in one expression and hangs forever, because
    /// `parking_lot::RwLock` is not reentrant. It was written that way first and
    /// deadlocked the test binary, which is why `EcsHandle`'s rule 1 is stated as
    /// "one statement, one guard" rather than as advice.
    fn local_player_count(handle: &EcsHandle) -> usize {
        let mut world = handle.write();
        let mut state =
            world.query_filtered::<Entity, bevy_ecs::prelude::With<lodestone_ecs::LocalPlayer>>();
        state.iter(&world).count()
    }

    /// **The clock-divergence gate.** A maximal stall must advance the *entity*
    /// systems' tick count and the player's by the same amount, and that amount
    /// must be vanilla's ten.
    ///
    /// This is the measurement Stage 5 recorded and could not fix: `Sim::step`
    /// banked `dt.clamp(0.0, 0.25)` (five ticks) while `EntityInterpolator` banked
    /// the pacer's `0.5 s` unclamped (ten), so a maximal stall advanced item
    /// physics five ticks further than player physics — per stall, cumulatively,
    /// with the excess real time discarded rather than reconciled. Counting a
    /// system in `TickSet::Animate` (where `tick_walk_animation` lives) against
    /// `FrameClock::ticks` is what would have caught it: before (c) those were two
    /// schedules in two `World`s and could not have agreed.
    #[test]
    fn a_maximal_stall_advances_the_entity_and_player_clocks_by_the_same_ten_ticks() {
        use bevy_ecs::resource::Resource;
        use bevy_ecs::schedule::IntoScheduleConfigs;

        #[derive(Resource, Default)]
        struct AnimateRuns(u64);

        let mut sim = Sim::new(test_config());
        {
            let mut world = sim.ecs().write();
            world.init_resource::<AnimateRuns>();
            world.schedule_scope(GameTick, |_w, schedule| {
                schedule.add_systems(
                    (|mut runs: bevy_ecs::system::ResMut<AnimateRuns>| runs.0 += 1)
                        .in_set(lodestone_ecs::TickSet::Animate),
                );
            });
        }

        let before = sim.tick_count();
        // Sixty seconds: 1200 ticks of real time, i.e. far past any budget.
        sim.step(60.0);
        let player_ticks = sim.tick_count() - before;
        let animate_runs = sim.ecs().read().resource::<AnimateRuns>().0;

        assert_eq!(
            player_ticks,
            u64::from(lodestone_ecs::MAX_CATCH_UP_TICKS),
            "the one accumulator's catch-up policy is vanilla's ten, not the \
             shell's old five"
        );
        assert_eq!(
            animate_runs, player_ticks,
            "the entity animation tick and the player tick are one schedule on \
             one clock; a difference here is the divergence §4.1(c) deleted"
        );
        // The excess is dropped, not carried: the next frame owes nothing.
        assert!(
            sim.clock().accumulator < lodestone_ecs::TICK_PERIOD,
            "accumulator {} should be a sub-tick residual",
            sim.clock().accumulator
        );
    }

    /// A quit-to-title resets the **one** accumulator and leaves monotonic time
    /// alone.
    ///
    /// `end_session` used to reset the interpolator's accumulator (by replacing the
    /// whole interpolator) and not the player's, so a reconnect re-phased the two
    /// clocks arbitrarily. There is one to reset now, and the chat timestamps that
    /// ride on `FrameClock::secs` must survive it — a line stamped before the
    /// teardown still has to age correctly afterwards.
    #[test]
    fn end_session_resets_the_one_accumulator_and_not_the_monotonic_clock() {
        let mut sim = Sim::with_demo_world(test_config());
        // Leave a deliberate sub-tick residual.
        sim.step(lodestone_ecs::TICK_PERIOD * 1.5);
        assert!(sim.clock().accumulator > 0.0, "control: there is a residual");
        let secs_before = sim.clock().secs;
        let ticks_before = sim.tick_count();

        sim.end_session();

        assert_eq!(sim.clock().accumulator, 0.0);
        assert_eq!(sim.clock().interp_alpha, 0.0);
        assert!(
            (sim.clock().secs - secs_before).abs() < 1e-12,
            "monotonic time must not rewind, or pre-teardown chat ages break"
        );
        assert_eq!(sim.tick_count(), ticks_before);
    }

    /// A session teardown clears the render-side entity tracks.
    ///
    /// This used to be a side effect of replacing the whole `EntityInterpolator`
    /// (and therefore of dropping its `World`). With one `World` it has to be an
    /// explicit despawn, which is exactly the kind of thing that gets dropped in a
    /// refactor and shows up as the previous server's mobs still drawn on the title
    /// **You could open a crafting table and not get out of it.**
    ///
    /// `close_open_menu` sent `ContainerClose` and nothing else, so
    /// [`Sim::open_menu`] stayed `Some` forever — a vanilla server does not echo a
    /// close back. Everything downstream keys off that: `active_container_menu`,
    /// the key-dispatch gate, the container draw. The dispatch was fixed first and
    /// the bug survived, because the function the keys correctly reached did not
    /// clear anything.
    ///
    /// The control matters as much as the assertion: it proves the menu really was
    /// open first, so a fold that silently failed to open it could not make this
    /// pass vacuously.
    #[test]
    fn closing_a_server_menu_clears_it_locally_without_waiting_for_the_server() {
        use lodestone_model::ClientEvent;

        let mut sim = Sim::with_demo_world(test_config());
        let local = sim.local;
        sim.write(|w| {
            if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
                menus.0.apply(&ClientEvent::ScreenOpened {
                    window_id: 5,
                    menu_type: lodestone_model::Identifier::new("minecraft", "crafting").unwrap(),
                    title: lodestone_model::Text::literal("Crafting"),
                });
                // 3x3 grid + result + 36 player slots: the content packet is what
                // actually promotes `pending` to `opened`.
                menus.0.apply(&ClientEvent::ContainerContent {
                    window_id: 5,
                    state_id: 1,
                    items: vec![None; 46],
                    carried_item: None,
                });
            }
        });
        assert!(
            sim.open_menu().is_some(),
            "control: the menu must actually be open, or this gate proves nothing"
        );

        sim.close_open_menu();

        assert!(
            sim.open_menu().is_none(),
            "closing must clear the local menu immediately — a vanilla server sends \
             no close back, so anything that waits for the wire waits forever"
        );
    }

    /// screen.
    #[test]
    fn end_session_clears_the_entity_tracks() {
        use crate::entities::EntitySnapshot;

        let mut sim = Sim::with_demo_world(test_config());
        let snap = EntitySnapshot {
            id: 7,
            type_path: "pig".into(),
            feet: glam::Vec3::new(1.0, 64.0, 1.0),
            scale: 1.0,
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            item: lodestone_model::Reported::Unreported,
            velocity: None,
            on_ground: true,
            equipment: Vec::new(),
            variant: None,
            count: 1,
        };
        sim.write(|w| crate::entities::fold_entity_snapshots(w, &[snap]));
        assert_eq!(
            sim.read(crate::entities::tracked_entity_count),
            1,
            "control: the fold really did spawn a track"
        );

        sim.end_session();
        assert_eq!(sim.read(crate::entities::tracked_entity_count), 0);
        assert!(sim.entity_draws().is_empty());
    }
}
