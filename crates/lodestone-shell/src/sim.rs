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
use lodestone_controller::{ControllerPlugin, InputState, RawInput, apply_look_inverted};
pub use lodestone_ecs::SessionPhase;
use lodestone_ecs::entity::{Attributes, EntityIndex, EntityKind, MinecraftEntityId, Position};
use lodestone_ecs::player::{
    ActionQueue, AttackStrengthTicker, CollisionSource, Dead, Egress, LocalPlayerPlugin,
    MovementIntent, NearbyEntities, PhysicsState, PlayerCollision, PrevPosition, Profile,
    SelectedSlot, Submersion, reset_local_player, spawn_local_player,
};
use lodestone_ecs::session::{
    ActionBarOverlay, HudEffects, Phase, RespawnCount, ServerEntityId, SessionChat,
    SessionHudPlugin, TitleOverlay, Vitals, Xp, insert_hud_components,
};
use lodestone_ecs::{
    ChunkWorld, CorePlugin, EcsHandle, Extract, FrameClock, GameTick, Update, VersionData,
};
use lodestone_entity::attribute::attribute_value;
use lodestone_entity::pose::EntityPose;
use lodestone_game::menu::Menu;
use lodestone_game::mining::{BreakInputs, Mining};
use lodestone_game::placement::{
    Axis, Half, OrientationKind, Placement, PlacedState, PlacementWorld, UseOnContext,
    UseOnDecision,
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
use lodestone_world::{BlockEntitySync, ChunkPos, World, WorldSink};

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
    // `for_render_distance`, not `for_view_distance`: the latter deliberately does
    // **not** populate the environmental fog pair, so the live overworld was still
    // getting only the render-distance term after that fix landed. The Nether and
    // the End already had it, because `Sim::fog_settings` calls `FogSettings::nether`
    // /`the_end` directly — so the one dimension a player actually starts in was the
    // one the fix did not reach.
    //
    // The span is unchanged: `for_render_distance` is algebraically identical to the
    // fraction form across render distance 3..=40, which `gpu.rs`'s
    // `fog_start_fraction_matches_vanillas_span` pins. `gpu::FOG_START_FRACTION` is
    // still used by that test, so it does not become dead.
    lodestone_render::fog::FogSettings::for_render_distance(crate::gpu::SKY_COLOR, render_distance)
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

// ---------------------------------------------------------------------------
// Local placement prediction (issue #381)
//
// `use_item_live` used to send `use_item_on` and wait: `Placement` is a
// *decision* machine and nothing wrote the world, so a placed block — a chest
// especially, since #374 made a state write create its block entity — was a hole
// for one server round trip. Everything below is what turns that decision into a
// local write. See `docs/block-placement-prediction.md`.
// ---------------------------------------------------------------------------

/// The world facts [`Placement::use_on`] asks for, **read once, before the
/// decision runs** rather than from inside it.
///
/// [`PlacementWorld`] is queried re-entrantly by `use_on`, and every answer needs
/// the chunk store's read lock — while `use_on` itself needs the ECS write guard
/// (it mutates the [`PlacementPredictor`] resource). Answering live would nest
/// those two guards, which is the `chunks → World` order `EcsHandle`'s rule 3
/// exists to forbid. Precomputing keeps the guards disjoint *and* makes the whole
/// decision hermetically testable, with no `Sim` and no server.
///
/// `use_on` asks exactly four questions over two positions:
/// `is_replaceable(clicked)` (which picks the target), then
/// `is_replaceable(target)` / `is_obstructed(target)` (legality) and
/// `is_interactable(clicked)`. Any other position answers conservatively — not
/// replaceable, not interactable — which can only make the shell predict *less*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlacementFacts {
    /// The block the ray hit.
    clicked: BlockPos,
    /// Where a placement would land: `clicked` itself when it is replaceable,
    /// otherwise the cell across the hit face. Same rule as
    /// [`lodestone_game::placement::resolve_target`], evaluated here because it
    /// needs the same world read.
    target: BlockPos,
    clicked_replaceable: bool,
    clicked_interactable: bool,
    target_replaceable: bool,
    target_obstructed: bool,
}

impl PlacementWorld for PlacementFacts {
    fn is_replaceable(&self, pos: BlockPos) -> bool {
        if pos == self.clicked {
            self.clicked_replaceable
        } else if pos == self.target {
            self.target_replaceable
        } else {
            false
        }
    }

    fn is_interactable(&self, pos: BlockPos) -> bool {
        pos == self.clicked && self.clicked_interactable
    }

    fn is_obstructed(&self, pos: BlockPos) -> bool {
        pos == self.target && self.target_obstructed
    }
}

/// Whether a block state is one the client may place *into*.
///
/// Deliberately only the three air blocks, not vanilla's full
/// `BlockState.canBeReplaced` set (water, lava, tall grass, snow layers, …):
/// that set is per-block-state registry data no census in this tree carries, and
/// guessing it would make the shell predict placements the server then refuses.
/// Narrowing it costs nothing but a *missing* prediction — i.e. today's
/// behaviour, a one-round-trip wait — for the cases it excludes, and it is what
/// makes the `waterlogged = false` rule in [`state_for_placement`] exact rather
/// than assumed.
fn is_air_state(state: u32) -> bool {
    matches!(
        lodestone_data::block_states::block_name(state),
        Some("minecraft:air" | "minecraft:cave_air" | "minecraft:void_air")
    )
}

/// Name fragments of blocks whose right-click **actuates** them, for the
/// place-vs-interact question `use_on` asks first.
///
/// This is an over-approximation on purpose, and the asymmetry is the whole
/// design: calling an inert block interactable only *suppresses* a prediction
/// (the shell falls back to sending and waiting, exactly today's behaviour),
/// while calling an interactable block inert makes the shell predict a block into
/// the cell next to the chest you meant to open. So the list errs long, and every
/// block that owns a block entity is treated as interactable regardless of
/// whether it appears here — which covers every container in the game through
/// [`lodestone_data::block_entity_types`]' census rather than through this list.
///
/// Vanilla asks `BlockState.useItemOn`/`useWithoutItem` — real per-block
/// behaviour with no census anywhere in this tree. A mislabelled block costs one
/// round trip either way, because the server re-sends the block state at *both*
/// candidate positions after every `use_item_on` (see [`Sim::use_item_live`]).
const INTERACTABLE_FRAGMENTS: &[&str] = &[
    "_door",
    "_trapdoor",
    "_fence_gate",
    "_button",
    "_bed",
    "_sign",
    "_shelf",
    "_head",
    "_skull",
    "candle",
    "cauldron",
    "anvil",
    "_pot",
    "note_block",
    "lever",
    "_table",
    "grindstone",
    "loom",
    "stonecutter",
    "repeater",
    "comparator",
    "daylight_detector",
    "cake",
    "composter",
    "respawn_anchor",
    "dragon_egg",
    "tnt",
    "lightning_rod",
    "bell",
    "beehive",
    "bee_nest",
    "campfire",
    "redstone",
    "copper_bulb",
    "berries",
    "berry_bush",
    "cave_vines",
    "sculk_",
    "shulker_box",
];

/// Whether right-clicking this block state actuates it instead of placing.
fn is_interactable_state(state: u32) -> bool {
    if lodestone_data::block_entity_types::block_entity_type(state).is_some() {
        return true;
    }
    let Some(name) = lodestone_data::block_states::block_name(state) else {
        return false;
    };
    INTERACTABLE_FRAGMENTS
        .iter()
        .any(|fragment| name.contains(fragment))
}

/// Blocks whose `facing` is `getHorizontalDirection().getOpposite()` — vanilla's
/// `HorizontalDirectionalBlock` family, i.e. "faces the player".
///
/// A hand-written list, and the reason it is a list rather than a derivation:
/// nothing in the block-state census distinguishes a 4-way `facing` that points
/// *toward* the player (`StairBlock`, `LadderBlock`, `BedBlock`, `DoorBlock`,
/// `FaceAttachedHorizontalDirectionalBlock`) from one that points *away*
/// (`ChestBlock`, `AbstractFurnaceBlock`, `CarvedPumpkinBlock`, …) — the two
/// have identical property signatures and differ only in Java. There are 293
/// blocks with a 4-value `facing` in 26.2; a block that is not named here (and is
/// not a stair) simply does not predict.
///
/// Sourced by grepping `getStateForPlacement` for
/// `getHorizontalDirection().getOpposite()` across
/// `.cache/mc/26.2/src/net/minecraft/world/level/block/`, then restricted to the
/// single-cell blocks whose remaining properties [`state_for_placement`] can also
/// resolve. Namespace-stripped paths.
const FACING_HORIZONTAL_OPPOSITE: &[&str] = &[
    // `ChestBlock.java:213`, `EnderChestBlock.java:75`.
    "chest",
    "trapped_chest",
    "ender_chest",
    "copper_chest",
    "exposed_copper_chest",
    "weathered_copper_chest",
    "oxidized_copper_chest",
    "waxed_copper_chest",
    "waxed_exposed_copper_chest",
    "waxed_weathered_copper_chest",
    "waxed_oxidized_copper_chest",
    // `AbstractFurnaceBlock.java:53`.
    "furnace",
    "blast_furnace",
    "smoker",
    // `CarvedPumpkinBlock.java:140`.
    "carved_pumpkin",
    "jack_o_lantern",
    // `BeehiveBlock.java:271`.
    "beehive",
    "bee_nest",
    // One-off `HorizontalDirectionalBlock`s.
    "end_portal_frame",   // `EndPortalFrameBlock.java:56`
    "chiseled_bookshelf", // `ChiseledBookShelfBlock.java:172`
    "lectern",            // `LecternBlock.java:93`
    "loom",               // `LoomBlock.java:54`
    "stonecutter",        // `StonecutterBlock.java:45`
    "vault",              // `VaultBlock.java:99`
    "repeater",           // `DiodeBlock.java:158`
    // `GlazedTerracottaBlock.java:28`.
    "white_glazed_terracotta",
    "orange_glazed_terracotta",
    "magenta_glazed_terracotta",
    "light_blue_glazed_terracotta",
    "yellow_glazed_terracotta",
    "lime_glazed_terracotta",
    "pink_glazed_terracotta",
    "gray_glazed_terracotta",
    "light_gray_glazed_terracotta",
    "cyan_glazed_terracotta",
    "purple_glazed_terracotta",
    "blue_glazed_terracotta",
    "brown_glazed_terracotta",
    "green_glazed_terracotta",
    "red_glazed_terracotta",
    "black_glazed_terracotta",
];

/// Blocks whose 6-way `facing` is `getNearestLookingDirection().getOpposite()` —
/// vanilla's `DirectionalBlock` family.
///
/// Same reasoning as [`FACING_HORIZONTAL_OPPOSITE`], and likewise a list rather
/// than "every block with a 6-value `facing`": 41 blocks have one in 26.2, and
/// several derive it from the *clicked face* instead (`amethyst_cluster`,
/// `end_rod`, `shulker_box`'s successors), which is a different rule with the same
/// property signature.
const FACING_ALL: &[&str] = &[
    "dispenser",
    "dropper",
    "observer",
    "piston",
    "sticky_piston",
    "barrel",
];

/// The value vanilla's `getStateForPlacement` leaves each **non-geometric**
/// property at, for every property whose registered default is the *same across
/// every block that has it*.
///
/// # Provenance, and why this is a measurement rather than a guess
///
/// Derived from `.cache/mc/26.2/generated/reports/blocks.json` — Mojang's own
/// generator output, data source #1 — by taking each block's `"default": true`
/// state and collecting, per property name, the set of values it holds there.
/// 93 property names appear; **60 of them take one value across all 1,196
/// blocks** and are listed below. The 17 that do not (`facing`, `axis`, `half`,
/// `type`, `shape`, `lit`, `waterlogged`, `level`, `mode`, `rotation`, `up`,
/// `down`, `north`, `south`, `east`, `west`, `bottom`) are either resolved from
/// geometry by [`OrientationKind`], handled by an explicit rule in
/// [`state_for_placement`], or a reason to decline the prediction outright.
///
/// A further 16 unambiguous names are **deliberately left out** because vanilla
/// computes them at placement time from geometry or neighbours, so their
/// registered default is the wrong answer for a *placed* block: `attachment`
/// (`BellBlock`), `face` (`FaceAttachedHorizontalDirectionalBlock`),
/// `orientation` (`CrafterBlock`, `JigsawBlock`), `hinge` (`DoorBlock`), `part`
/// (`BedBlock`), `vertical_direction`/`thickness` (`PointedDripstoneBlock`),
/// `hanging` (`LanternBlock`), `distance`/`persistent`/`leaves`
/// (`LeavesBlock` — note `persistent` is set **true** for a player-placed leaf,
/// so its `false` default would be actively wrong), `instrument`
/// (`NoteBlock`, read from the block below), `side_chain`, `tip`, `tilt`, `drag`.
/// Omitting a name makes every block carrying it decline, which is the safe
/// direction.
///
/// Measured coverage of the whole scheme: **721 of 1,196 blocks** resolve to a
/// state, and every one of those 721 matches the block's own registered default
/// once the geometry properties are put back — except the 22 aquatic blocks
/// (corals, coral fans, `sea_pickle`, `conduit`) whose registered default is
/// `waterlogged = true`. Those are not a divergence in practice: vanilla sets
/// `waterlogged` from the fluid at the placement position, and
/// [`is_air_state`] means the shell only ever predicts into a cell with no fluid.
const NON_GEOMETRIC_DEFAULTS: &[(&str, &str)] = &[
    ("age", "0"),
    ("attached", "false"),
    ("berries", "false"),
    ("bites", "0"),
    ("bloom", "false"),
    ("can_summon", "false"),
    ("candles", "1"),
    ("charges", "0"),
    ("conditional", "false"),
    ("copper_golem_pose", "standing"),
    ("cracked", "false"),
    ("crafting", "false"),
    ("creaking_heart_state", "uprooted"),
    ("delay", "1"),
    ("disarmed", "false"),
    ("dusted", "0"),
    ("eggs", "1"),
    ("enabled", "true"),
    ("extended", "false"),
    ("eye", "false"),
    ("flower_amount", "1"),
    ("has_book", "false"),
    ("has_bottle_0", "false"),
    ("has_bottle_1", "false"),
    ("has_bottle_2", "false"),
    ("has_record", "false"),
    ("hatch", "0"),
    ("honey_level", "0"),
    ("hydration", "0"),
    ("in_wall", "false"),
    ("inverted", "false"),
    ("layers", "1"),
    ("locked", "false"),
    ("moisture", "0"),
    ("natural", "false"),
    ("note", "0"),
    ("occupied", "false"),
    ("ominous", "false"),
    ("open", "false"),
    ("pickles", "1"),
    ("potent_sulfur_state", "dry"),
    ("power", "0"),
    ("powered", "false"),
    ("sculk_sensor_phase", "inactive"),
    ("segment_amount", "1"),
    ("short", "false"),
    ("shrieking", "false"),
    ("signal_fire", "false"),
    ("slot_0_occupied", "false"),
    ("slot_1_occupied", "false"),
    ("slot_2_occupied", "false"),
    ("slot_3_occupied", "false"),
    ("slot_4_occupied", "false"),
    ("slot_5_occupied", "false"),
    ("snowy", "false"),
    ("stage", "0"),
    ("trial_spawner_state", "inactive"),
    ("triggered", "false"),
    ("unstable", "false"),
    ("vault_state", "inactive"),
];

/// Per-block values for a property whose default is *not* consistent across
/// blocks, so it cannot live in [`NON_GEOMETRIC_DEFAULTS`].
///
/// `lit` splits 48 `false` / 4 `true` over the blocks that have it — a furnace
/// places unlit, a `redstone_torch` places lit. Rather than pick one and be wrong
/// for the other, only the named blocks get an answer; everything else with a
/// `lit` property declines. `(block path, property, value)`, from the same
/// `blocks.json` default states.
const BLOCK_PROPERTY_OVERRIDES: &[(&str, &str, &str)] = &[
    ("furnace", "lit", "false"),
    ("blast_furnace", "lit", "false"),
    ("smoker", "lit", "false"),
];

/// Every 26.2 state of one block, plus the value domain of each of its
/// properties.
///
/// Built by one linear pass over the 32,366-entry state table. That is only
/// ever run on a right-click, and it is what lets the two functions below work
/// from the real census instead of a second, hand-maintained table keyed by
/// block name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockStates {
    /// This block's state ids, ascending.
    ids: Vec<u32>,
    /// `(property name, distinct values)`. Sorted by name, because
    /// [`lodestone_data::block_states::properties`] hands back sorted pairs.
    domains: Vec<(&'static str, Vec<&'static str>)>,
}

impl BlockStates {
    fn domain(&self, name: &str) -> Option<&[&'static str]> {
        self.domains
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, values)| values.as_slice())
    }
}

/// Collect [`BlockStates`] for `block` (a full identifier, e.g.
/// `minecraft:chest`), or `None` if no such block exists — which is how a
/// non-block item (a sword, bread) is recognised: vanilla's `BlockItem` shares
/// its block's registry name, so "is this item placeable?" is "is there a block
/// with this name?".
fn block_states_of(block: &str) -> Option<BlockStates> {
    let mut ids = Vec::new();
    let mut domains: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
    for id in 0..lodestone_data::block_states::STATE_COUNT {
        if lodestone_data::block_states::block_name(id) != Some(block) {
            continue;
        }
        ids.push(id);
        for &(name, value) in lodestone_data::block_states::properties(id).unwrap_or(&[]) {
            match domains.iter_mut().find(|(candidate, _)| *candidate == name) {
                Some((_, values)) => {
                    if !values.contains(&value) {
                        values.push(value);
                    }
                }
                None => domains.push((name, vec![value])),
            }
        }
    }
    (!ids.is_empty()).then_some(BlockStates { ids, domains })
}

/// Classify how `block` derives its orientation from placement geometry, or
/// `None` when the census cannot say — in which case the shell does not predict
/// this item at all.
///
/// Everything decidable from the property signature is decided from it; the two
/// facing families that are *not* (see [`FACING_HORIZONTAL_OPPOSITE`]) come from
/// a named list. Declining is always safe: it reproduces the pre-#381 behaviour
/// of sending `use_item_on` and waiting.
fn orientation_for_placement(block: &str, states: &BlockStates) -> Option<OrientationKind> {
    let path = block.strip_prefix("minecraft:").unwrap_or(block);
    // A pillar's axis is the clicked face's axis (`RotatedPillarBlock`). A
    // 2-value `axis` is `nether_portal`, which is not placed by an item.
    if let Some(axis) = states.domain("axis") {
        return (axis.len() == 3).then_some(OrientationKind::Pillar);
    }
    // `SlabBlock`'s `type` is `top`/`bottom`/`double`; a chest's is
    // `single`/`left`/`right`, which is not geometry and is handled as a
    // non-geometric default instead.
    if states.domain("type").is_some_and(|d| d.contains(&"double")) {
        return Some(OrientationKind::Slab);
    }
    match states.domain("facing").map(<[&str]>::len) {
        Some(4) => {
            if states.domain("half").is_some_and(|d| d.contains(&"bottom"))
                && states.domain("shape").is_some()
            {
                return Some(OrientationKind::Stairs);
            }
            FACING_HORIZONTAL_OPPOSITE
                .contains(&path)
                .then_some(OrientationKind::FacingHorizontalOpposite)
        }
        Some(6) => FACING_ALL
            .contains(&path)
            .then_some(OrientationKind::FacingAll),
        // A 5-value `facing` is a hopper, whose placement rule is its own.
        Some(_) => None,
        // No `facing`: orientation-free, as long as nothing else in the
        // signature says the placement reads geometry we are not modelling
        // (a rail's `shape`, a door's `half`).
        None => (states.domain("shape").is_none() && states.domain("half").is_none())
            .then_some(OrientationKind::Fixed),
    }
}

/// The block-state id a predicted placement should write, or `None` when any
/// property of the block cannot be resolved.
///
/// This is a **total** specification, not a best effort: every property the block
/// has is given a value — from `placed` when [`OrientationKind`] defines it, from
/// [`BLOCK_PROPERTY_OVERRIDES`] / [`NON_GEOMETRIC_DEFAULTS`] / the two explicit
/// rules otherwise — and the matching state id is then the *unique* state whose
/// property set equals it. A partial specification would need the block's
/// registered default state to fill the rest, and no census in this tree carries
/// one (`blocks.json`'s `"default": true` flag is not in
/// [`lodestone_data::block_states`]). That absence is exactly why this function
/// declines instead of guessing.
fn state_for_placement(
    block: &str,
    states: &BlockStates,
    orientation: OrientationKind,
    placed: &PlacedState,
) -> Option<u32> {
    let path = block.strip_prefix("minecraft:").unwrap_or(block);
    let mut wanted: Vec<(&'static str, &'static str)> = Vec::with_capacity(states.domains.len());
    for (name, domain) in &states.domains {
        let value = match *name {
            "facing"
                if matches!(
                    orientation,
                    OrientationKind::FacingAll
                        | OrientationKind::FacingHorizontal
                        | OrientationKind::FacingHorizontalOpposite
                        | OrientationKind::Stairs
                ) =>
            {
                face_property(placed.facing?)
            }
            "axis" if orientation == OrientationKind::Pillar => axis_property(placed.axis?),
            "type" if orientation == OrientationKind::Slab => half_property(placed.half?),
            "half" if orientation == OrientationKind::Stairs => half_property(placed.half?),
            // `StairBlock.getStateForPlacement` computes `shape` from the
            // neighbouring stairs; `straight` is the no-neighbour answer and is
            // what every one of the 64 stair blocks defaults to. The server
            // corrects a corner with its own block update.
            "shape" if orientation == OrientationKind::Stairs => "straight",
            // Vanilla reads this from the fluid at the placement position
            // (`SimpleWaterloggedBlock`'s `copyWaterloggedFrom`). We only predict
            // into air (see `is_air_state`), so `false` is the answer rather than
            // a default.
            "waterlogged" => "false",
            // `ChestBlock.getStateForPlacement` scans for an adjacent chest to
            // make a double; `single` is the no-neighbour answer, and the server
            // re-sends the state when a neighbour makes it a double. Keyed on the
            // value rather than the property name because `type` is also a slab's
            // (`top`/`bottom`/`double`) and a piston head's (`normal`/`sticky`) —
            // only the ten chest blocks have a `single`, measured across the 26.2
            // census.
            "type" if domain.contains(&"single") => "single",
            _ => BLOCK_PROPERTY_OVERRIDES
                .iter()
                .find(|(candidate, property, _)| *candidate == path && property == name)
                .map(|&(_, _, value)| value)
                .or_else(|| {
                    NON_GEOMETRIC_DEFAULTS
                        .iter()
                        .find(|(property, _)| property == name)
                        .map(|&(_, value)| value)
                })?,
        };
        wanted.push((name, value));
    }
    // `domains` is in the census's own sorted-by-name order, so `wanted` is too
    // and this is a slice comparison rather than a per-property search.
    states
        .ids
        .iter()
        .copied()
        .find(|&id| lodestone_data::block_states::properties(id) == Some(wanted.as_slice()))
}

/// The block-state id a right-click on `block` predicts, given the
/// geometry-derived [`PlacedState`] [`Placement::use_on`] resolved — or `None`
/// when the shell declines to predict this block at all.
///
/// The whole resolution behind [`Sim::use_item_live`]'s local write, in one
/// callable place: classify the orientation from the census
/// ([`orientation_for_placement`]) then specify every property
/// ([`state_for_placement`]). `pub` so a pixel gate can drive the *same*
/// resolution a click does instead of choosing a state of its own and proving
/// nothing about which one the shell would pick.
#[must_use]
pub fn predicted_placement_state(block: &str, placed: &PlacedState) -> Option<u32> {
    let states = block_states_of(block)?;
    let orientation = orientation_for_placement(block, &states)?;
    state_for_placement(block, &states, orientation, placed)
}

/// [`BlockFace`] to the `facing` property value (`Direction.getSerializedName`).
fn face_property(face: BlockFace) -> &'static str {
    match face {
        BlockFace::Down => "down",
        BlockFace::Up => "up",
        BlockFace::North => "north",
        BlockFace::South => "south",
        BlockFace::West => "west",
        BlockFace::East => "east",
    }
}

/// [`Axis`] to the `axis` property value.
fn axis_property(axis: Axis) -> &'static str {
    match axis {
        Axis::X => "x",
        Axis::Y => "y",
        Axis::Z => "z",
    }
}

/// [`Half`] to the `half` (stairs) / `type` (slab) property value — the two share
/// the `top`/`bottom` vocabulary.
fn half_property(half: Half) -> &'static str {
    match half {
        Half::Bottom => "bottom",
        Half::Top => "top",
    }
}

/// Write a locally predicted block state, block entity included.
///
/// **This is the local mirror of the v770 adapter's `BLOCK_UPDATE` arm**, and it
/// is deliberately the same two calls in the same order:
/// [`WorldSink::set_block`] then [`WorldSink::sync_block_entity`] with the new
/// state's `BLOCK_ENTITY_TYPE` id. Writing the state alone is issue #374 — a
/// chest with a state, no record, and zero pixels — and #381 is that same bug
/// reached through the *prediction* rather than through a packet.
///
/// A free function over `&mut dyn WorldSink` rather than a `Sim` method so a test
/// can drive the production write with a bare [`World`], no GPU and no server.
/// The `Option<u32>` the world takes comes from [`lodestone_data`]: `lodestone-world`
/// cannot depend on it (`data → model → world` is a cycle), which is why the
/// caller resolves the type and the world only applies it.
pub fn write_predicted_block(
    world: &mut dyn WorldSink,
    block: [i32; 3],
    state: u32,
) -> BlockEntitySync {
    world.set_block(block[0], block[1], block[2], state);
    world.sync_block_entity(
        block[0],
        block[1],
        block[2],
        lodestone_data::block_entity_types::block_entity_type(state),
    )
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
fn hit_cursor(hit: RayHit) -> Vec3f {
    let [x, y, z] = hit.cursor();
    Vec3f::new(x, y, z)
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
    /// The stitched vanilla atlas for the live world, or `None` when running on
    /// the demo palette. Its presence is the single discriminant for "render the
    /// live server world with the vanilla atlas" vs "mesh the demo world": the
    /// two use disjoint block-id spaces and must never be meshed with the wrong
    /// classifier.
    vanilla_atlas: Option<Arc<BlockAtlas>>,
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
    /// Vanilla's `invertMouseX`/`invertMouseY` options
    /// ([`crate::config::Options::invert_mouse_x`]/`invert_mouse_y`, issue
    /// #203), pushed down the same way as [`Self::view_bobbing`] — see
    /// [`Self::set_mouse_invert`]. Read by [`Self::apply_mouse`], which calls
    /// [`lodestone_controller::apply_look_inverted`] instead of the plain
    /// `apply_look` now that there is somewhere to source the two bools from.
    invert_mouse_x: bool,
    invert_mouse_y: bool,
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
        // The sheet stitch is *also* kept on `Sim` (see `Sim::particle_atlas`):
        // the emitter needs its UV rects and the GPU needs its pixels, and
        // issue #45 is what happens when those two come from different images.
        let particle_atlas = resources.particle_atlas;
        let particles = match resources.vanilla_atlas.as_ref() {
            Some(atlas) => Particles::new(atlas.models()),
            None => Particles::with_demo_palette(&crate::blocks::build_atlas().uv_table),
        }
        .with_particle_atlas(particle_atlas.as_deref());

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
            // Autonomous navigation (`docs/autonomous-navigation.md`, issue #38):
            // the M1 walk-only plugin. Registration order relative to the rest of
            // this tuple does not matter — its two systems are chained
            // `.after(TickSet::Intent).before(TickSet::Physics)` internally,
            // rather than `.in_set(TickSet::Intent)`, specifically so it never has
            // to be ordered against `compute_movement_intent` by name (see that
            // doc's "Why `.after(TickSet::Intent)`" section) — but that is a claim
            // about the plugin's own `.add_systems` calls, not proof this call
            // site actually reaches them, which is exactly the shape of bug
            // `CLAUDE.md`'s island rule warns about. Adds no systems that fire
            // without an `AutopilotGoal` set, so this is inert for every session
            // until something (a chat command, not yet built) sets one.
            lodestone_autopilot::AutopilotPlugin,
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
            particle_atlas,
            language: resources.language,
            teleport_count: 0,
            collide_against_live_world: true,
            asset_banner: resources.banner,
            recover_from_death: true,
            death_message: None,
            audio: ShellAudio::from_env(),
            third_person: false,
            body_pose: EntityPose::new(feet[0], feet[2], player.yaw, false),
            // Seeded from the spawn pose so the very first frame does not ease up
            // from zero — vanilla's `Camera` is likewise aligned before its first
            // tick, not zero-initialised.
            eye_height_smoother: crate::camera_rig::EyeHeightSmoother::new(player.eye_height),
            view_bob: ViewBob::new(),
            // Vanilla's default. A fresh `Sim` bobs until told otherwise, so a
            // caller that forgets `set_view_bobbing` gets the vanilla behaviour
            // rather than a silently disabled feature.
            view_bobbing: true,
            // Vanilla's defaults (both options default `false` — see
            // `docs/input-options.md`); a caller that forgets the setters gets
            // vanilla's own behaviour, not a silently-inverted or
            // silently-toggling one.
            invert_mouse_x: false,
            invert_mouse_y: false,
            toggle_sneak: false,
            toggle_sprint: false,
            chest_lids: crate::block_entities::ChestLids::new(),
            pickups: lodestone_game::mining::PickupFeed::new(),
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

    /// Whether the use button is currently held down on an item (armed by
    /// [`Self::use_item`], cleared by [`Self::end_use`]).
    ///
    /// Half of vanilla's `Player.isScoping()` (issue #154):
    /// `isUsingItem() && getUseItem().is(Items.SPYGLASS)`
    /// (`Player.java:1936-1938`). This crate has no held-item identity check
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
        // A death screen (issue #103) must not survive into the next session —
        // `reset_local_player` below clears the `Dead` marker itself, but this
        // field is plain `Sim` state, not an ECS component, so it needs its own
        // line (see its doc comment on why it lives here rather than in
        // `lodestone_ecs::session`).
        self.death_message = None;

        // §4.1(c): the entity interpolator no longer owns a `World` to throw away,
        // so its tracks are cleared explicitly. Replacing the whole interpolator
        // used to *also* zero that `World`'s private `TickAccum` while leaving the
        // player's accumulator alone — a quit-to-title re-phased the two clocks
        // arbitrarily on top of the clamp divergence. There is one accumulator now
        // and it is reset on the next line, deliberately rather than incidentally.
        self.write(|w| {
            crate::entities::reset_entity_tracks(w);
            // The ingest-side twin of the line above: `reset_entity_tracks` only
            // clears the *render* fold, and until this call nothing ever cleared
            // `lodestone_ecs::entity::EntityIndex`, so a rejoin's fresh server ids
            // left every previous session's entity indexed, still enumerated by
            // `SharedState::entities`, and redrawn frozen alongside its live
            // duplicate. See `reset_ingest_entities`'s own docs for the full trace.
            lodestone_ecs::ingest::reset_ingest_entities(w);
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
            w.insert_resource(UsingItem(false));
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

    /// The current death's message, for the death screen (issue #103) to draw
    /// — `None` once the player is alive again, or before any death this
    /// session. See [`Self::death_message`]'s field doc.
    #[must_use]
    pub fn death_message(&self) -> Option<&str> {
        self.death_message.as_deref()
    }

    /// Submit a manual respawn request (`ClientAction::Respawn`) — the death
    /// screen's Respawn button. A no-op unless the player is actually flagged
    /// dead, so a stray call (a double-click, a leftover queued action after
    /// the server already respawned us) cannot send an unsolicited respawn
    /// mid-game, and a no-op off a live session (nothing to send to).
    ///
    /// Manual because [`crate::net::run`] now builds the client with
    /// [`lodestone_client::RespawnPolicy::Manual`] (issue #103): the library
    /// used to answer every `Death` event with an automatic
    /// `ClientAction::Respawn`, which is what let the shell ride through death
    /// with no screen at all. See `docs/pause-menu.md`'s note on the death
    /// screen for the full picture.
    pub fn respawn(&mut self) {
        if self.is_dead()
            && let Some(net) = &self.net
        {
            net.send_action(ClientAction::Respawn);
        }
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

    /// The ticks a fresh attack must wait before it is back at full strength —
    /// vanilla's `getCurrentItemAttackStrengthDelay`, `(1.0 /
    /// getAttributeValue(Attributes.ATTACK_SPEED)) * 20.0`
    /// (`Player.java:1816-1818`, `.cache/mc/26.2/src`).
    ///
    /// Reads `minecraft:attack_speed` off the local player's own
    /// [`Attributes`] snapshot — the same server-fed, per-item-aware value
    /// `lodestone_ecs::player::player_physics`'s `WATER_MOVEMENT_EFFICIENCY`
    /// injection already reads through `attribute_value`. This is *not* a hardcoded
    /// constant and does not need `lodestone-data`'s `item_prototypes` census
    /// (which was checked and does not carry attack speed at all — no
    /// `minecraft:attribute_modifiers` census exists in this repo yet): a
    /// weapon's `-2.4` (sword) / `-3.0` (axe) modifier arrives the same way
    /// any other equipment-driven attribute change does, as a server
    /// `update_attributes` packet the instant the held item changes
    /// (`AttributeMap`'s dirty-tracking on `LivingEntity.setItemSlot`), and
    /// [`Attributes`] already folds it. Before the first such packet (a fresh
    /// demo-world player, or a live session before login's fold lands)
    /// `attribute_value` reads the registry default (`4.0`, unarmed), giving a
    /// 5-tick delay — the correct unarmed value, not a guess.
    #[must_use]
    fn attack_strength_delay(&self) -> f32 {
        let key = lodestone_model::Identifier::new("minecraft", "attack_speed")
            .expect("valid built-in identifier");
        let speed = self.read(|w| {
            w.get::<Attributes>(self.local)
                .map_or(4.0, |attrs| attribute_value(&attrs.0, &key))
        });
        // `getAttributeValue` cannot legitimately reach 0 (the registry clamps
        // `attack_speed` to `>= 0.0`, and no vanilla modifier stack takes an
        // unarmed 4.0 base all the way there), but a hostile/future value of
        // exactly 0 must not become a divide-by-zero `inf` delay.
        20.0 / (speed.max(f64::from(f32::EPSILON)) as f32)
    }

    /// The attack-cooldown fraction the crosshair indicator fills to,
    /// `0.0..=1.0` — vanilla's `getAttackStrengthScale(0.0F)`
    /// (`Player.java:1826-1828`), the exact call `Hud.extractCrosshair` makes
    /// for the crosshair-style indicator (`Hud.java:448`). The `a` (partial
    /// tick) argument is fixed at `0.0` here, same as that call site; nothing
    /// in this shell threads a render-time partial tick into `Sim`'s other
    /// accessors either (see [`Self::health`]/[`Self::xp`]).
    #[must_use]
    pub fn attack_strength_scale(&self) -> f32 {
        self.attack_strength_scale_at(0.0)
    }

    /// `getAttackStrengthScale(a)` (`Player.java:1826-1828`) with the partial
    /// tick argument exposed, because vanilla itself calls this with two
    /// different values for two different purposes: `0.0F` for the crosshair
    /// indicator ([`Self::attack_strength_scale`], `Hud.java:448`) and `0.5F`
    /// for `Player.attack`'s own `fullStrengthAttack` gate
    /// (`Player.java:956,962`), which [`Self::maybe_spawn_crit_particles`]
    /// needs. One private helper rather than two public accessors that would
    /// otherwise duplicate the ticker read and delay computation.
    #[must_use]
    fn attack_strength_scale_at(&self, a: f32) -> f32 {
        let delay = self.attack_strength_delay();
        let ticker = self.read(|w| w.get::<AttackStrengthTicker>(self.local).map_or(0, |t| t.0));
        ((ticker as f32 + a) / delay).clamp(0.0, 1.0)
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

    /// The held-item name highlight (issue #126) as `(styled name, alpha)`,
    /// `Some` while a selected item's name is showing. Ticked in
    /// [`lodestone_ecs::session::tick_hud_overlays`], keyed on the selected
    /// stack's *identity* rather than slot — see
    /// [`lodestone_ecs::session::HeldItemOverlay`]'s doc.
    #[must_use]
    pub fn held_item_overlay(&self) -> Option<(String, f32)> {
        self.read(|w| {
            let overlay = w
                .get::<lodestone_ecs::session::HeldItemOverlay>(self.local)
                .expect("the local player always carries HeldItemOverlay");
            let name = overlay.0.name()?;
            Some((name.to_owned(), overlay.0.alpha()))
        })
    }

    /// `Player.hasInfiniteMaterials()` — `Abilities.instabuild`
    /// (`Player.java`; `AnvilMenu.mayPickup` and
    /// `EnchantmentScreen.java:111` both gate on it). Used by
    /// `app.rs`'s `ContainerFrame::with_cost_context` for the anvil/enchanting
    /// affordability colours — see `docs/container-cost-screens.md`.
    #[must_use]
    pub fn has_infinite_materials(&self) -> bool {
        self.read(|w| {
            w.get::<lodestone_ecs::session::Abilities>(self.local)
                .is_some_and(|a| a.instabuild)
        })
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
                data: menus.opened_data().to_vec(),
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
                menus
                    .0
                    .apply(&lodestone_model::ClientEvent::ScreenClosed { window_id });
            }
        });
    }

    /// Compose a typed chat line onto the outbound [`ClientAction`] seam and hand
    /// it to the live client (a leading `/` is a command, else a chat message).
    /// A blank line sends nothing. No-op without a live connection. Returns
    /// whether anything was sent, so the caller can echo command feedback.
    ///
    /// **Nothing is intercepted here.** A `/givedebug` wrapper used to run ahead
    /// of [`compose_chat_action`] and rewrite itself into the server's real
    /// `/give @s <item> <amount>`; issue #382 deleted it, because typing `/give`
    /// does the same thing with no bespoke parser to keep in step with the
    /// server's. Every line now goes to the server verbatim, and every command
    /// response — including "you are not op" — arrives back over the ordinary
    /// inbound chat path.
    pub fn send_chat(&mut self, line: &str) -> bool {
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
            let (yaw, pitch) = apply_look_inverted(
                player.yaw,
                player.pitch,
                dx,
                dy,
                sensitivity,
                self.invert_mouse_x,
                self.invert_mouse_y,
            );
            self.player_mut(|player| {
                player.yaw = yaw;
                player.pitch = pitch;
            });
        }
    }

    /// Push vanilla's `invertMouseX`/`invertMouseY` options down from the menu
    /// layer (issue #203), the same way [`Self::set_view_bobbing`] does for
    /// View Bobbing. Cheap and idempotent; `app.rs` calls it once per frame,
    /// before [`Self::step`] so the very tick the option changes already
    /// sees it.
    pub fn set_mouse_invert(&mut self, invert_x: bool, invert_y: bool) {
        self.invert_mouse_x = invert_x;
        self.invert_mouse_y = invert_y;
    }

    /// Push vanilla's `key.sneak`/`key.sprint` hold-vs-toggle options down
    /// from the menu layer (issue #202). Stored rather than applied directly
    /// because the actual [`InputState::set_toggle_modes`] call has to
    /// happen inside [`Self::step`] (see that field's doc) — `Sim` has no
    /// `MenuNav` to read from at that point, only whatever was last pushed
    /// here.
    pub fn set_toggle_modes(&mut self, toggle_sneak: bool, toggle_sprint: bool) {
        self.toggle_sneak = toggle_sneak;
        self.toggle_sprint = toggle_sprint;
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

    /// This frame's block entities (chests, issue #23) as a `'static` closure for
    /// [`RenderState::set_block_entity_source`].
    ///
    /// `None` without a live session — the offline demo world has no chests, and a
    /// closure that always returned an empty vec would look installed while
    /// carrying nothing.
    ///
    /// # Why this is not gated on `vanilla_atlas`
    ///
    /// [`Self::outline_shape_source`] is, because an outline is only meaningful
    /// against real block states. This is not, and the difference matters: the
    /// chest sheets are loaded by the *renderer* from its own jar lookup, so a
    /// session with a live world but no stitched atlas still draws chests
    /// correctly. Copying the atlas gate here would silently switch chests off in
    /// exactly the configuration that most needs them visible.
    ///
    /// # Two snapshots, both deliberate
    ///
    /// The lid map is **cloned** and the partial tick **sampled** rather than
    /// borrowed, because the closure outlives this call (`RenderState` owns it)
    /// and must not hold `&self`. The clone is one small `HashMap` — it holds only
    /// chests that are open or moving, since
    /// [`ChestLids::tick`](crate::block_entities::ChestLids::tick) drops the
    /// settled-shut — and re-taking it every frame is what makes the animation
    /// move at all. Installing this once at connect freezes every lid at the
    /// fraction of a tick it was installed on.
    ///
    /// [`RenderState::set_block_entity_source`]: crate::gpu::RenderState::set_block_entity_source
    #[must_use]
    pub fn block_entity_source(
        &self,
    ) -> Option<impl Fn(glam::Vec3) -> Vec<lodestone_render::ChestSpawn> + Send + Sync + 'static>
    {
        let handle = self.net.as_ref()?.shared_handle();
        let lids = self.chest_lids.clone();
        let partial_tick = self.clock().interp_alpha;
        Some(move |eye: glam::Vec3| {
            crate::block_entities::chest_spawns(&handle, &lids, eye, partial_tick)
        })
    }

    /// The skull/head sibling of [`Self::block_entity_source`], for
    /// [`RenderState::set_skull_source`](crate::gpu::RenderState::set_skull_source).
    ///
    /// Unlike the chest source this captures **no partial tick and no animation
    /// state**: none of the five ported skull types animate, so there is nothing
    /// whose interpolation could freeze at the fraction of a tick the closure was
    /// installed on. That asymmetry is the whole reason these are two sources
    /// rather than one closure returning a pair.
    #[must_use]
    pub fn skull_source(
        &self,
    ) -> Option<impl Fn(glam::Vec3) -> Vec<lodestone_render::SkullSpawn> + Send + Sync + 'static>
    {
        let handle = self.net.as_ref()?.shared_handle();
        Some(move |eye: glam::Vec3| crate::block_entities::skull_spawns(&handle, eye))
    }

    /// The sign sibling of [`Self::skull_source`] — see
    /// `crate::block_entities::sign_spawns`. Captures no partial tick and no
    /// animation state, for the same reason skulls do not: sign text does not
    /// animate.
    #[must_use]
    pub fn sign_source(
        &self,
    ) -> Option<impl Fn(glam::Vec3) -> Vec<lodestone_render::SignSpawn> + Send + Sync + 'static>
    {
        let handle = self.net.as_ref()?.shared_handle();
        Some(move |eye: glam::Vec3| crate::block_entities::sign_spawns(&handle, eye))
    }

    /// The bell sibling of [`Self::skull_source`] — see
    /// `crate::block_entities::bell_spawns`. Same per-frame install shape as
    /// chest/skull/sign; see `docs/block-entity-renderers.md`'s Bell section
    /// for why the render pass and the CPU-side gather were already landed
    /// and only this call site (plus `app.rs`'s install) was missing.
    #[must_use]
    pub fn bell_source(
        &self,
    ) -> Option<impl Fn(glam::Vec3) -> Vec<lodestone_render::BellSpawn> + Send + Sync + 'static>
    {
        let handle = self.net.as_ref()?.shared_handle();
        Some(move |eye: glam::Vec3| crate::block_entities::bell_spawns(&handle, eye))
    }

    /// How many chest lids are currently animating or open — for the debug
    /// overlay and for the live gate, which needs to distinguish "the block event
    /// never arrived" from "the lid is drawn shut".
    #[must_use]
    pub fn chest_lid_count(&self) -> usize {
        self.chest_lids.len()
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
        self.body_pose
            .start_swing(lodestone_entity::pose::swing_duration(
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
        // Issue #202: apply the hold-vs-toggle option to the live
        // `InputState` before any `GameTick` schedule this call runs reads
        // it. One push per `step` call is enough — the option cannot change
        // mid-frame, and every catch-up tick inside this call shares it.
        let (toggle_sneak, toggle_sprint) = (self.toggle_sneak, self.toggle_sprint);
        self.input_mut(|i| i.set_toggle_modes(toggle_sneak, toggle_sprint));
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
            // The walk bob's amplitude reads the state vanilla's `updateBob` sees,
            // which is the state **before** this tick's movement: `aiStep` calls
            // `updateBob()` and only then `super.aiStep()`, so `getDeltaMovement()`
            // is still last tick's post-friction velocity there. Captured here,
            // before the `GameTick` write guard, for that reason and not merely
            // for lock hygiene.
            let (pre_position, pre_speed, pre_on_ground, pre_swimming) = {
                let p = self.player();
                (
                    p.position,
                    (p.velocity.x * p.velocity.x + p.velocity.z * p.velocity.z).sqrt() as f32,
                    p.on_ground,
                    p.pose == lodestone_physics::Pose::Swimming,
                )
            };
            let pre_dead = self.is_dead();
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
            // The bob's *phase* is the distance the feet actually travelled, which
            // is why this is a post-tick subtraction rather than a velocity:
            // `LocalPlayer.move` adds `length(getX() - prevX, getZ() - prevZ) * 0.6`
            // **after** `super.move` has already clipped the delta against
            // collision, so walking into a wall does not advance the stride.
            let moved = ((p.position.x - pre_position.x) as f32)
                .hypot((p.position.z - pre_position.z) as f32);
            self.view_bob.tick(
                moved,
                pre_speed,
                pre_on_ground,
                pre_dead,
                pre_swimming,
            );
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
            // Chest lids (issue #23), on the same fixed 20 Hz as everything else
            // here: `ChestLidController.tickLid()` ramps by ±0.1 per tick, so a
            // lid takes exactly 10 ticks to swing. Advancing it per *frame*
            // instead would open a chest in a third of a second at 60 fps and
            // make the animation speed a function of the frame rate.
            self.chest_lids.tick();
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
    /// [`LiveCollision::pick_boxes`] / [`WorldCollision::pick_boxes`] — read its
    /// docs, which record why an earlier inlined `!is_water(...)` here made **kelp
    /// and every waterlogged block unbreakable**. Deliberately a single call and not
    /// an `||` chain: the geometry the collision tests exercise has to be the exact
    /// geometry the ray uses, or the gate proves nothing about the pick.
    ///
    /// # Issue #375: boxes, not a boolean
    ///
    /// This used to pass `is_pickable` — a per-*cell* occupancy predicate — so
    /// every pickable block was a unit cube to the hit test while the selection
    /// box was already drawn from the real outline census. Leaf litter therefore
    /// stayed targetable with the crosshair well above it. The closure now emits
    /// the cell's real outline boxes and [`raycast`] clips against them, which is
    /// vanilla's `ClipContext.Block.OUTLINE`.
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
            self.live_collision().and_then(|view| {
                raycast(origin, dir, REACH, |x, y, z, out| {
                    view.pick_boxes(x, y, z, out);
                })
            })
        } else {
            let store = self.chunk_world();
            let world = store.read();
            let view = WorldCollision::new(&world);
            raycast(origin, dir, REACH, |x, y, z, out| {
                view.pick_boxes(x, y, z, out);
            })
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
    /// an entity is never picked through.
    ///
    /// That distance is [`RayHit::distance`], the entry point of the **outline
    /// box** the ray actually struck. It used to be re-derived here by clipping
    /// a unit cube around `block_hit.block`, which was wrong in both directions
    /// on a partial block — too *short* whenever the real box sits deeper in the
    /// cell than its near face, which hid an entity standing in front of a
    /// fence. The ray now reports its own entry distance, so there is nothing
    /// left to approximate (issue #375).
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
        let search_limit = block_hit.map_or(ENTITY_REACH, |hit| hit.distance.min(ENTITY_REACH));

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
                    let dims =
                        EntityDimensions::new(facts.dimensions.width, facts.dimensions.height, 0.6);
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
        .with_biome_sky_color(self.biome_sky_color())
    }

    /// The standing biome's `minecraft:visual/sky_color` in **linear** RGB, or
    /// `None` when there is nothing better than the dimension default to draw
    /// (issue #96).
    ///
    /// # The chain, and the one hop that is not a lookup
    ///
    /// The colour table arrives whole, indexed by biome holder id, on
    /// `ClientEvent::BiomeVisuals` and reaches here as
    /// `PlayerSnapshot::biome_sky_colors`. **The biome itself is not on the
    /// network at all** — it lives in the chunk section's biome palette, so this
    /// is the hop that has to happen at the camera every frame, and it is the
    /// reason the whole table travels rather than one resolved colour.
    ///
    /// # Why it scans downward for a section
    ///
    /// `sections_at` elides an empty section to `None`, and the section holding
    /// the player's own feet is very often empty — standing on a plain at `y=64`
    /// puts the eye in section `64..80` while the ground is the last block of
    /// `48..64`. Sampling only the eye's section would therefore leave the sky
    /// untinted over open ground, which is precisely where a sky is visible.
    /// Biomes are all but columnar (one cell is 4×4×4 blocks, and vanilla's own
    /// biome sources vary far more horizontally than vertically), so the first
    /// present section at or below the eye is the right answer, not an
    /// approximation worth a second mechanism.
    ///
    /// The `None`s are all deliberate and all mean the same thing: *the server
    /// has not told us*. Pre-login, a server that sent no biome registry, a
    /// column that has not streamed in, a biome with no `sky_color` (the ten
    /// Nether/End biomes) — each falls back to the dimension colour the caller
    /// already computed, which is the same explicit-fallback shape #34 was filed
    /// over. Never a plausible-looking overworld blue.
    #[must_use]
    fn biome_sky_color(&self) -> Option<[f32; 3]> {
        let net = self.net.as_ref()?;
        let table = net.shared_handle().get()?.player().biome_sky_colors;
        if table.is_empty() {
            return None;
        }
        let dims = net.world_dimensions()?;
        let section_count = dims.section_count();

        let position = self.player().position;
        let block_x = position.x.floor() as i32;
        let block_y = position.y.floor() as i32;
        let block_z = position.z.floor() as i32;
        let chunk = lodestone_client::ChunkPos {
            x: block_x.div_euclid(16),
            z: block_z.div_euclid(16),
        };
        let base_si = dims.min_y.div_euclid(16);
        let eye_si = block_y.div_euclid(16) - base_si;
        // Clamp rather than reject: an eye above the build limit still stands in
        // a biome, and the topmost section is the one that holds it.
        let top = eye_si.clamp(0, i32::try_from(section_count).unwrap_or(0).saturating_sub(1));
        if section_count == 0 {
            return None;
        }

        // Top-down: one lock acquisition for the whole column, then the highest
        // present section at or below the eye.
        let requests: Vec<(lodestone_client::ChunkPos, usize)> = (0..=top)
            .rev()
            .map(|si| (chunk, usize::try_from(si).unwrap_or(0)))
            .collect();
        let (section, si) = net
            .sections_at(&requests)
            .into_iter()
            .zip(requests.iter().map(|(_, si)| *si))
            .find_map(|(section, si)| section.map(|s| (s, si)))?;

        // The sampled `y` is the eye's own within its section, or the top of
        // whichever lower section answered.
        let local_y = if si == usize::try_from(top).unwrap_or(0) {
            block_y.rem_euclid(16) as usize
        } else {
            15
        };
        let biome = section.biome_at_block(
            block_x.rem_euclid(16) as usize,
            local_y,
            block_z.rem_euclid(16) as usize,
        );
        let packed = (*table.get(usize::try_from(biome).ok()?)?)?;
        // sRGB bytes → linear, exactly as `FogSettings::nether`/`the_end` do with
        // their own hex constants. The *day/night* multiply stays in gamma space
        // inside the sky pass (`SkyFrame`); this is only the transfer function
        // for the base colour, which every colour handed to the renderer gets.
        Some(lodestone_render::fog::srgb_u8_to_linear([
            ((packed >> 16) & 0xFF) as u8,
            ((packed >> 8) & 0xFF) as u8,
            (packed & 0xFF) as u8,
        ]))
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
        let Some(hit) = self.target() else {
            return false;
        };
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
            // Vanilla's own break is *predicted*, not received: the client's
            // `MultiPlayerGameMode.destroyBlock` runs `playerWillDestroy` →
            // `spawnDestroyParticles` → `level.levelEvent(player, 2001, …)`, and
            // `ClientLevel.levelEvent` ignores the exclusion and dispatches
            // straight into `LevelEventHandler`'s `case 2001` locally
            // (`ClientLevel.java:877-882`) — sound and debris together. This is
            // the offline mirror of that; the live predicted break is still
            // silent because its emit lives in `interact.rs`'s ECS system, which
            // has no audio handle (see `docs/sound-playback.md`).
            self.play_block_break_sound(hit.block, broken);
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
    ///
    /// Also resets [`AttackStrengthTicker`] to `0` — vanilla's
    /// `MultiPlayerGameMode.attack` calling `player.resetAttackStrengthTicker()`
    /// right after the client-side `player.attack(entity)`
    /// (`MultiPlayerGameMode.java:425-430`, `.cache/mc/26.2/client-src`).
    /// Unconditional on every entity target, exactly like vanilla's call site:
    /// there is no client-side `cannotAttack` gate here (damage is fully
    /// server-authoritative per `docs/combat.md`), so every left-click on an
    /// entity restarts the cooldown regardless of whether the server ends up
    /// applying any damage.
    fn attack_entity(&mut self, entity_id: i32) {
        // The same tick-driven intent `use_item_live` reads for its own
        // sneaking bit, so a sneak-attack cannot disagree with what the wire
        // already told the server this tick's crouch state is.
        let sneaking = self.movement_intent().sneak;
        let local = self.local;
        if let Some(net) = &self.net {
            net.send_action(ClientAction::InteractEntity {
                entity_id,
                interaction: EntityInteraction::Attack,
                sneaking,
            });
        }
        // Vanilla's own order (`MultiPlayerGameMode.attack`,
        // `MultiPlayerGameMode.java:427-429`): the packet, then the
        // client-side `player.attack(entity)` prediction — whose crit
        // condition reads `attackStrengthTicker` **before** it is reset — and
        // only then `resetAttackStrengthTicker()`. Reading the ticker after
        // zeroing it here would make `fullStrengthAttack` false on every
        // attack, including the one that just landed at full charge, so this
        // call must stay above the reset below.
        self.maybe_spawn_crit_particles(entity_id);
        self.write(|w| {
            if let Some(mut ticker) = w.get_mut::<AttackStrengthTicker>(local) {
                ticker.0 = 0;
            }
        });
    }

    /// Vanilla's local-only crit-particle prediction — `Player.attack`'s
    /// `criticalAttack = fullStrengthAttack && canCriticalAttack(entity)`
    /// (`Player.java:970-971,1032-1041`), whose visual half is
    /// `attackVisualEffects`' `this.crit(entity)` call (`Player.java:1063-1066`,
    /// `LocalPlayer.crit` → `ParticleEngine.createTrackingEmitter`,
    /// `LocalPlayer.java:664-665`).
    ///
    /// # This is real vanilla dual simulation, not an approximation invented
    /// for this port
    ///
    /// `MultiPlayerGameMode.attack` runs the **client's own copy** of
    /// `player.attack(entity)` (`MultiPlayerGameMode.java:428`) independently
    /// of, and before, the server's authoritative copy of the same method —
    /// the server computes the real damage, the client predicts only the
    /// cosmetic trigger (sound + particle) so it does not wait a round trip to
    /// see feedback on its own swing. The wire `Attack` packet itself carries
    /// no damage or crit flag (`docs/combat.md`); nothing here affects what
    /// the server decides.
    ///
    /// # Condition, checked against the jar rather than assumed
    ///
    /// `canCriticalAttack` (`Player.java:1032-1041`): `fallDistance > 0.0 &&
    /// !onGround && !onClimbable && !isInWater && !isMobilityRestricted &&
    /// !isPassenger && entity is LivingEntity && !isSprinting`.
    /// `fullStrengthAttack = getAttackStrengthScale(0.5F) > 0.9F`
    /// (`Player.java:956,962`) is the caller's own gate, not part of
    /// `canCriticalAttack` — hence [`Self::attack_strength_scale_at`] rather
    /// than reusing [`Self::attack_strength_scale`]'s `a = 0.0`, which is a
    /// different call site's (the crosshair's) partial-tick argument.
    ///
    /// Two vanilla clauses are not modelled, and the divergence is small and
    /// explained rather than silent:
    /// - **`!onClimbable` is not read separately.** This engine resets
    ///   `fall_distance` to `0.0` the instant `tick_air` finds a climbable —
    ///   `LivingEntity.handleOnClimbable`, folded into `tick_air` per
    ///   [`lodestone_physics::player::PlayerState::fall_distance`]'s own
    ///   "Climbable reset" bullet — so `fall_distance > 0.0` already implies
    ///   not-on-climbable in this port's physics model. Checked against that
    ///   source rather than guessed.
    /// - **`!isMobilityRestricted`/`!isPassenger`, and the outer `baseDamage >
    ///   0.0F || magicBoost > 0.0F` gate, are not modelled.** This shell has
    ///   no riding state (`docs/combat.md`'s knockback section notes the same
    ///   absence for a different mechanic) and no local weapon-damage/
    ///   enchantment computation to derive `baseDamage`/`magicBoost` from —
    ///   the identical gap [`Self::attack_strength_delay`]'s own doc names for
    ///   `lodestone-data` carrying no per-item attack-speed census. The only
    ///   case this can diverge on is an attack that deals zero base damage
    ///   (an already-broken or damage-less item), which vanilla itself treats
    ///   as "nothing happens" at the outer `if` — the crit particle is cosmetic
    ///   and no damage number depends on it either way.
    ///
    /// # The particle burst: one tick of `TrackingEmitter`, not three
    ///
    /// `TrackingEmitter` (`TrackingEmitter.java:29-41`) runs for **3 ticks**,
    /// spawning up to 16 candidates per tick (filtered to a unit sphere,
    /// ~52% pass) that track the entity's *current* position each tick. This
    /// shell's particle system has no per-attack persistent emitter — every
    /// existing local spawn ([`crate::particles::Particles::destroy_block`]/
    /// `breaking_block`) is a one-shot burst — so this spawns **one** tick's
    /// worth (16 candidates, same unit-sphere filter) at the target's
    /// position at the moment of the attack, rather than adding new
    /// multi-tick emitter machinery for a purely cosmetic burst. The
    /// per-candidate position/velocity formula (`Entity.getX(double)` etc.,
    /// `Entity.java:3770-3811`) and the emitted particle's own physics
    /// (`lodestone_particle::emit::crit`) are both exact; only the tick count
    /// is a disclosed simplification.
    fn maybe_spawn_crit_particles(&mut self, entity_id: i32) {
        if self.attack_strength_scale_at(0.5) <= 0.9 {
            return;
        }
        let Some((feet, width, height)) = self.read(|w| {
            let target = w.resource::<EntityIndex>().get(entity_id)?;
            let pos = w.get::<Position>(target)?;
            let kind = w.get::<EntityKind>(target)?;
            let facts = w.resource::<VersionData>().entity_facts(&kind.0)?;
            let type_id = lodestone_data::entity_types::entity_type_id_parts(
                kind.0.namespace(),
                kind.0.path(),
            )?;
            lodestone_data::entity_census::is_living(type_id)
                .unwrap_or(false)
                .then_some((pos.0, facts.dimensions.width, facts.dimensions.height))
        }) else {
            return;
        };
        let local = self.local;
        let (fall_distance, on_ground) = self.read(|w| {
            w.get::<PhysicsState>(local)
                .map_or((0.0, true), |s| (s.0.fall_distance, s.0.on_ground))
        });
        if fall_distance <= 0.0 || on_ground {
            return;
        }
        if self.fluid_state().in_water() || self.movement_intent().sprint {
            return;
        }
        self.particles_mut(|p| {
            let engine = p.engine_mut();
            for _ in 0..16 {
                let xa = f64::from(engine.rng().next_float()) * 2.0 - 1.0;
                let ya = f64::from(engine.rng().next_float()) * 2.0 - 1.0;
                let za = f64::from(engine.rng().next_float()) * 2.0 - 1.0;
                if xa * xa + ya * ya + za * za > 1.0 {
                    continue;
                }
                let x = f64::from(feet.x) + f64::from(width) * (xa / 4.0);
                let y = f64::from(feet.y) + f64::from(height) * (0.5 + ya / 4.0);
                let z = f64::from(feet.z) + f64::from(width) * (za / 4.0);
                particle_emit::crit(engine, x, y, z, xa, ya + 0.2, za);
            }
        });
    }

    /// Send the serverbound **use-on-entity** for `entity_id` — vanilla's
    /// `MultiPlayerGameMode.interact`, the outbound half of mounting a boat,
    /// minecart or saddled animal.
    ///
    /// This is the mirror image of [`Self::attack_entity`]: same packet family,
    /// same direct-send reasoning (a click is a discrete event, not a per-tick
    /// one, and [`ActionQueue`] only drains inside the tick loop), same
    /// tick-derived `sneaking` bit so the local decision cannot disagree with the
    /// crouch state the wire already reported this tick. The differences are the
    /// interaction kind and that there is no attack cooldown to reset.
    ///
    /// **`Interact`, never `InteractAt`** — see [`Self::use_item_live`]'s entity
    /// branch for why the entity-local hit position is not fabricated here.
    ///
    /// The swing is vanilla's too: `MultiPlayerGameMode.interact` is followed by
    /// `player.swing(hand)` at the `Minecraft.startUseItem` call site whenever the
    /// result `consumesAction()`. We swing unconditionally, matching what
    /// [`Self::use_item_live`]'s block path already does with its own
    /// `SwingArm` — the result is server-side and one round trip away, and a
    /// suppressed swing on a refused interaction is a smaller error than a
    /// missing swing on an accepted one.
    fn interact_entity(&mut self, entity_id: i32) {
        let sneaking = self.movement_intent().sneak;
        if let Some(net) = &self.net {
            net.send_action(ClientAction::InteractEntity {
                entity_id,
                interaction: EntityInteraction::Interact { hand: Hand::Main },
                sneaking,
            });
            net.send_action(ClientAction::SwingArm { hand: Hand::Main });
        }
        // Client-side animation, so it runs with or without a socket — the same
        // split `use_item_live` makes for its own unconditional `swing_hand`.
        self.swing_hand();
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

    /// Release the use button — vanilla's `Minecraft.java:1914-1917`:
    ///
    /// ```text
    /// if (this.player.isUsingItem()) {
    ///    if (!this.options.keyUse.isDown()) {
    ///       this.gameMode.releaseUsingItem(this.player);
    ///    }
    ///    ...
    /// }
    /// ```
    ///
    /// which itself lowers to `MultiPlayerGameMode.releaseUsingItem`
    /// (`:513-517`) sending a bare `ServerboundPlayerActionPacket`
    /// (`RELEASE_USE_ITEM`) — [`ClientAction::ReleaseUseItem`] here, encoded
    /// by all four protocol adapters already
    /// (`crates/protocol/{v47,v340,v735,v770}/src/adapter.rs`) but with no
    /// producer anywhere in this shell before this method. Bow, crossbow and
    /// shield are all `useOnRelease() == true`
    /// (`LivingEntity.java:3471-3475,3602-3616`) and structurally cannot
    /// complete a use without this packet — food and potions are
    /// `useOnRelease() == false` and auto-complete on the server's own tick
    /// count, which is exactly why this gap went unnoticed: eating and
    /// drinking still worked.
    ///
    /// A no-op on the demo world (nothing there tracks an in-progress use).
    pub fn end_use(&mut self) {
        if self.is_live() {
            self.end_use_live();
        }
    }

    /// The live half of [`Self::end_use`], split out the same way
    /// [`Self::begin_attack_live`] is — reachable directly from a test with no
    /// `vanilla_atlas`, since the swing/send logic itself needs no GPU asset.
    ///
    /// A no-op if [`UsingItem`] is already `false`: no button was ever pressed
    /// down (via [`Self::use_item_live`]) for this to be the release edge of.
    /// Sending `RELEASE_USE_ITEM` in that case would still be harmless —
    /// `LivingEntity.releaseUsingItem`
    /// (`.cache/mc/26.2/src/…/LivingEntity.java:3602-3613`) no-ops whenever
    /// the server has no `useItem` in progress — but there is nothing to
    /// justify sending it for.
    fn end_use_live(&mut self) {
        let was_using = self.write(|w| {
            let mut using = w.resource_mut::<UsingItem>();
            std::mem::replace(&mut using.0, false)
        });
        if !was_using {
            return;
        }
        if let Some(net) = &self.net {
            net.send_action(ClientAction::ReleaseUseItem);
        }
    }

    /// Lower a live right-click into the server's `use_item_on` action **and
    /// predict the placement locally** (issue #381).
    ///
    /// The server stays authoritative: [`Placement::use_on`] returns the action to
    /// send in *every* branch, so the shell sends it unconditionally (with a
    /// proper prediction sequence) and lets the server decide, exactly as vanilla
    /// does. Because the server owns the sneak state derived from the wire, the
    /// crouch input must have been sent (see
    /// [`send_player_input`](Self::send_player_input)) for a sneak-placement
    /// against a chest/door to suppress the interaction.
    ///
    /// # Why the local write exists
    ///
    /// This method used to send and wait, so a placed block did not exist
    /// client-side until the server's `BLOCK_UPDATE` came back — one round trip of
    /// hole. For a chest that is #374 reached through a different door: the state
    /// write is what creates the block entity, and with no local state write there
    /// was no local record and nothing to draw. The prediction now writes through
    /// [`write_predicted_block`], the same `set_block` + `sync_block_entity` pair
    /// the adapter's `BLOCK_UPDATE` arm calls.
    ///
    /// # What happens when the server refuses
    ///
    /// Nothing here has to detect it, because vanilla's server corrects **both**
    /// candidate positions after *every* `use_item_on`, unconditionally — accepted,
    /// refused, or actually an interaction
    /// (`ServerGamePacketListenerImpl.java:1397-1398`):
    ///
    /// ```text
    /// this.send(new ClientboundBlockUpdatePacket(level, pos));
    /// this.send(new ClientboundBlockUpdatePacket(level, pos.relative(direction)));
    /// ```
    ///
    /// `pos` is `clicked` and `pos.relative(direction)` is the adjacent cell, and a
    /// prediction can only ever land on one of those two. So a refused placement is
    /// overwritten by the authoritative state within one round trip — and since
    /// #374 that path calls `sync_block_entity`, which **removes** the block-entity
    /// record the prediction created (`BlockEntitySync::Removed`). The removal half
    /// is not a second mechanism to build; it is the same one, pointing the other
    /// way. `crates/lodestone-shell/tests/placed_chest_block_entity_pixels.rs`
    /// gates it rather than assuming it.
    ///
    /// A mispredicted placement therefore costs exactly the round trip the hole
    /// used to cost, which is why every classification below is allowed to err
    /// toward *not* predicting but never toward predicting something wrong.
    fn use_item_live(&mut self) {
        if self.is_dead() {
            return;
        }
        // Marks [`UsingItem`] so a later [`Self::end_use`] knows the button
        // was actually pressed — see that resource's own docs for why this is
        // an input-state mirror rather than vanilla's real `isUsingItem()`.
        // Set unconditionally here rather than in every branch below: vanilla
        // arms `player.isUsingItem()` from the held item's own `use()` call,
        // which can happen inside any of this method's block/entity/generic
        // branches, and this client has no equivalent per-item hook to mark
        // it from.
        self.write(|w| w.resource_mut::<UsingItem>().0 = true);
        // **Entity before block, and this branch is the whole of "get in a boat".**
        // Vanilla's `Minecraft.startUseItem` switches on `hitResult.getType()` and
        // `case ENTITY` comes first (`Minecraft.java`'s `useItem`), the identical
        // priority [`Self::begin_attack_live`] already implements for the left
        // button off the same [`EntityRayTarget`]. Before this, `use_item_live`
        // returned early on `self.target()` being `None` and never looked at the
        // entity ray at all, so a right-click on a boat, minecart or saddled horse
        // sent nothing — the mount packet had no producer, which is the outbound
        // half of the island `EntityPassengersChanged` was the inbound half of.
        //
        // `InteractAt` is deliberately **not** used, even though vanilla sends both
        // it and `Interact` for a `case ENTITY` click: `InteractAt` carries the
        // entity-local hit position, and [`Self::update_entity_target`] keeps only
        // the winning entity's id, not the ray's hit point on its box. A fabricated
        // local offset would be a wrong number where the server accepts a missing
        // one — `ServerGamePacketListenerImpl` dispatches mounting off the plain
        // `Interact` (it is `Entity.interact` that returns `InteractionResult` and
        // calls `player.startRiding`), and `InteractAt` only matters for the
        // per-part hit an armour stand or a horse's saddle slot resolves. So the
        // honest subset is sent, and refining it needs the ray to start reporting
        // its hit position, not a guess here.
        //
        // **`case ENTITY` only returns here on a *successful* interact.**
        // Vanilla's own switch (`Minecraft.java:1693-1708`) returns
        // immediately only when `gameMode.interact(...) instanceof
        // InteractionResult.Success`; anything else hits an explicit `break;`
        // at `:1708` and falls through to the unconditional generic-use call
        // at `:1730` (`gameMode.useItem`) — which is what actually raises a
        // shield or starts drawing a bow when the crosshair happens to be
        // over a mob with no special right-click behaviour (hostile mobs,
        // overwhelmingly, which is exactly the combat case). Before this fix
        // `use_item_live` always returned here, so `entity_target()` being
        // `Some` for *any* living entity in `ENTITY_REACH` — hostile or not —
        // permanently short-circuited the fallback.
        //
        // This client has no local classification of an interact's result to
        // match vanilla's `instanceof Success` test against: there is no
        // `player.interactOn` equivalent here, only the wire send (the same
        // gap `Self::interact_entity`'s own docs cover for why `InteractAt`
        // is not fabricated). So every entity interact is treated as
        // non-consuming for this decision and always falls through to
        // [`Self::use_item_generic`]. The one place this can diverge from
        // vanilla is a genuinely successful mount (an empty boat, a saddled
        // and rideable horse): vanilla's own local prediction would skip the
        // fallback there, and this does not, so an item held while boarding a
        // vehicle can also start its use. That is judged the smaller error
        // next to a shield/bow that could never fire at all.
        if let Some(entity_id) = self.entity_target() {
            self.interact_entity(entity_id);
            self.use_item_generic();
            return;
        }
        let Some(hit) = self.target() else {
            // Vanilla's own MISS/no-target path: a `null` `hitResult` skips
            // the whole `if (this.hitResult != null)` switch in
            // `startUseItem` (`Minecraft.java:1681,1691`) and still reaches
            // the unconditional fallback at `:1730`. This used to `return`
            // here with **nothing sent at all** — aiming at open air, or at a
            // mob standing just past block reach with nothing behind it,
            // silently dropped the click.
            self.use_item_generic();
            return;
        };
        let clicked = BlockPos::new(hit.block[0], hit.block[1], hit.block[2]);
        let face = face_from_normal(hit.normal);
        let cursor = hit_cursor(hit);
        // The intent this tick's physics ran on — the same one
        // `lodestone_controller::ecs::send_player_input` derived the wire's shift
        // bit from, so the local decision and the server's cannot disagree. This
        // used to re-read the keyboard, which was frame-granular; vanilla is
        // tick-granular here too (`Minecraft.handleKeybinds` runs in the tick).
        let sneaking = self.movement_intent().sneak;

        // Native player-inventory index of the off-hand slot
        // (`lodestone_game::menu`'s table: hotbar `0..=8`, off-hand `40`).
        const OFFHAND_NATIVE_INDEX: usize = 40;
        let menu = self.player_menu();
        let main = menu
            .player_native(self.selected_slot())
            .filter(|stack| !stack.is_empty())
            .map(|stack| stack.item().clone());
        // Vanilla's `haveSomethingInOurHands` — *either* hand, and it is what
        // makes a sneak-click suppress the block's own use.
        let has_item_in_hand = main.is_some()
            || menu
                .player_native(OFFHAND_NATIVE_INDEX)
                .is_some_and(|stack| !stack.is_empty());
        // Placeable only when the census can name the block *and* classify how it
        // orients. Leaving `placing` at `None` otherwise is what makes an
        // unclassifiable item fall back to send-and-wait rather than write a state
        // we are not confident in.
        let placeable = main.as_ref().and_then(|item| {
            let name = item.to_string();
            let states = block_states_of(&name)?;
            let orientation = orientation_for_placement(&name, &states)?;
            Some((name, states, orientation))
        });
        let ctx = UseOnContext {
            hand: Hand::Main,
            clicked,
            face,
            cursor,
            inside_block: false,
            rotation: Rotation::new(self.player().yaw, self.player().pitch),
            sneaking,
            has_item_in_hand,
            placing: placeable.as_ref().and_then(|_| main.clone()),
            orientation: placeable
                .as_ref()
                .map_or(OrientationKind::Fixed, |&(_, _, kind)| kind),
        };
        // Read the world facts before taking the ECS guard `use_on` needs — see
        // `PlacementFacts` on why the two guards must not nest.
        let facts = self.placement_facts(clicked, face);
        let decision = self.write(|w| {
            w.resource_mut::<PlacementPredictor>()
                .0
                .use_on(&ctx, &facts)
        });
        let (UseOnDecision::Interact { action }
        | UseOnDecision::Place { action, .. }
        | UseOnDecision::Nothing { action }) = &decision;
        if let Some(net) = &self.net {
            net.send_action(action.clone());
            net.send_action(ClientAction::SwingArm { hand: Hand::Main });
        }
        // This swing bypasses `ActionQueue` (the two sends above go straight to
        // the socket so their wire order is fixed), so it also bypasses
        // `drain_action_queue`'s hook and has to start the animation itself.
        // Unconditional, not inside the `if let` above: the animation is
        // client-side and does not need a socket.
        self.swing_hand();

        // The prediction. `placeable` is `Some` whenever `use_on` could have
        // returned `Place` at all (it is what filled `ctx.placing`), so the only
        // way this declines is `state_for_placement` failing on a property it
        // cannot resolve.
        if let (UseOnDecision::Place { prediction, .. }, Some((name, states, orientation))) =
            (&decision, &placeable)
        {
            if let Some(state) = state_for_placement(name, states, *orientation, &prediction.state) {
                let pos = prediction.pos;
                self.predict_block([pos.x, pos.y, pos.z], state);
                // Vanilla's placement sound is the tail of `BlockItem.place`
                // (`BlockItem.java:87`), which passes the placing player as
                // `playSound`'s **excluded** entity — so the server broadcasts it
                // to everyone but us, and our own copy is predicted locally by
                // `ClientLevel.playSound`, whose exclusion test is inverted
                // (`if (except == this.minecraft.player)`, `ClientLevel.java:705`).
                // It therefore hangs off the prediction, exactly as vanilla's
                // does: no prediction, no sound, and no double-play either.
                //
                // Tied to the *predicted state* rather than to the item, because
                // the sound is `placedState.getSoundType()` — a waterlogged or
                // half-slab placement can be a different `SoundType` from the
                // block's default state.
                self.play_block_place_sound([pos.x, pos.y, pos.z], state);
            }
        }
    }

    /// Vanilla's unconditional generic-use fallback at the bottom of
    /// `Minecraft.startUseItem`'s per-hand loop (`Minecraft.java:1730`,
    /// `gameMode.useItem`) — the send that actually raises a shield, draws a
    /// bow, or starts eating/drinking, independent of any block or entity
    /// under the crosshair. Called from [`Self::use_item_live`]'s entity and
    /// no-target branches; see that method's docs for exactly which vanilla
    /// cases reach it.
    ///
    /// Lowers to [`ClientAction::UseItem`] — a **second** serverbound island
    /// this investigation found alongside `ReleaseUseItem`: encoded by all
    /// four protocol adapters
    /// (`crates/protocol/{v47,v340,v735,v770}/src/adapter.rs`) with zero
    /// producers anywhere in this shell before this method.
    ///
    /// Guarded on the main hand actually holding something, matching
    /// vanilla's own `!heldItem.isEmpty()` check at the same call site —
    /// there is nothing to use and no packet to justify for an empty hand.
    /// Only `Hand::Main` is considered, matching every other send in this
    /// method; vanilla's per-hand loop also tries the off hand, which this
    /// shell does not model here.
    ///
    /// The prediction sequence is borrowed from [`PlacementPredictor`]'s own
    /// counter rather than a second, independent one — see
    /// [`Placement::take_use_sequence`]'s docs for why that matches vanilla's
    /// own single shared counter.
    fn use_item_generic(&mut self) {
        let has_item = self
            .player_menu()
            .player_native(self.selected_slot())
            .is_some_and(|stack| !stack.is_empty());
        if !has_item {
            return;
        }
        let rotation = Rotation::new(self.player().yaw, self.player().pitch);
        let sequence =
            self.write(|w| w.resource_mut::<PlacementPredictor>().0.take_use_sequence());
        if let Some(net) = &self.net {
            net.send_action(ClientAction::UseItem {
                hand: Hand::Main,
                rotation,
                sequence,
            });
            net.send_action(ClientAction::SwingArm { hand: Hand::Main });
        }
        // Client-side animation, so it runs with or without a socket — the
        // same split every other swing site in this method makes.
        self.swing_hand();
    }

    /// The [`PlacementWorld`] facts for one right-click, read from the
    /// client-owned world in one go. See [`PlacementFacts`].
    fn placement_facts(&self, clicked: BlockPos, face: BlockFace) -> PlacementFacts {
        let state_at = |pos: BlockPos| self.net.as_ref().and_then(|net| net.block_at(pos));
        let clicked_state = state_at(clicked);
        let clicked_replaceable = clicked_state.is_some_and(is_air_state);
        // `resolve_target`'s rule, evaluated here because it is the same read: a
        // replaceable clicked cell is replaced in place, otherwise the placement
        // goes to the cell across the hit face.
        let target = if clicked_replaceable {
            clicked
        } else {
            lodestone_game::placement::offset(clicked, face)
        };
        PlacementFacts {
            clicked,
            target,
            clicked_replaceable,
            clicked_interactable: clicked_state.is_some_and(is_interactable_state),
            // An unloaded column reads `None` and therefore "not replaceable",
            // which declines the prediction — the same conservative direction as
            // every other unknown here.
            target_replaceable: state_at(target).is_some_and(is_air_state),
            target_obstructed: self.block_intersects_player([target.x, target.y, target.z]),
        }
    }

    /// Apply a locally predicted block state to the one chunk store and re-mesh.
    ///
    /// The write itself is [`write_predicted_block`] — state *and* block entity,
    /// the adapter's `BLOCK_UPDATE` pair — so a predicted chest exists as a
    /// block-entity record the moment it is placed instead of one round trip
    /// later.
    fn predict_block(&mut self, block: [i32; 3], state: u32) -> BlockEntitySync {
        let store = self.chunk_world();
        // The chunk guard is taken and dropped before `remesh_around` reaches for
        // the ECS resource again, so the two are never held together.
        let outcome = {
            let mut world = store.write();
            write_predicted_block(&mut *world, block, state)
        };
        self.remesh_around(block);
        outcome
    }

    /// Place [`PLACE_BLOCK`] against the targeted face on the **demo world**, if
    /// the cell is empty and doesn't intersect the player. Returns whether a
    /// block was placed. The live path uses [`use_item`](Self::use_item) instead
    /// so the server actually hears the placement.
    pub fn place_block(&mut self) -> bool {
        let Some(hit) = self.target() else {
            return false;
        };
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
                // The dimension's absent-sky-light policy, read per sample from the
                // cell `refresh_mesh_policy` publishes into. Same reason
                // `net::entity_light_at` takes one: `sky_at` resolves
                // `LightData::Missing` to **0**, so a particle in open air above the
                // top of the lit column used to come out unlit and near-black. A
                // captured value would go stale on a portal.
                let sky_policy = net.shared_sky_default();
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
                    let got = handle.get()?.sections_and_light_at(&[(
                        pos,
                        section as usize,
                        section as usize + 1,
                    )]);
                    let (_, light) = got.into_iter().next()?;
                    let light = light?;
                    let ly = (y - dims.min_y).rem_euclid(16) as usize;
                    let lx = x.rem_euclid(16) as usize;
                    let lz = z.rem_euclid(16) as usize;
                    // Through the same adapter the terrain draw uses, so absent sky
                    // data gets the dimension's default rather than `sky_at`'s bare
                    // `0`. Not a second `match` restating 15 — one expression.
                    let resolved =
                        lodestone_render::WorldSectionLight::new(&light, sky_policy.get());
                    // Vanilla's `LightTexture.pack`: block light at bit 4, sky
                    // light at bit 20. The particle shader reproduces the
                    // terrain term `0.2 + 0.8 * max(sky, block)` from these.
                    Some(
                        u32::from(resolved.block_light(lx, ly, lz)) << 4
                            | u32::from(resolved.sky_light(lx, ly, lz)) << 20,
                    )
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
    ///
    /// # Why this does *not* call `sync_block_entity`, unlike every other writer
    ///
    /// `value` is a [`crate::blocks::id`] constant — the shell's **own** ten-entry
    /// demo palette, deliberately unrelated to any protocol's ids (see that
    /// module's docs). Running it through `lodestone_data`'s 26.2
    /// `state_id → block_entity_type` census would be a category error: `id::WATER`
    /// is `5`, and real state `5` is some unrelated 26.2 block that may well own a
    /// block entity. So the demo world has no block entities, correctly — the
    /// palette contains nothing that could have one. The live prediction's writer
    /// is [`write_predicted_block`], which is fed real census state ids.
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
    fn remesh_section(&mut self, cx: i32, cz: i32, si: usize, min_y: i32, section_count: usize) {
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
            self.remesh_section(nsx, nsz, si as usize, extent.min_y, extent.section_count);
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

    /// Settle any placement prediction the server has just overwritten.
    ///
    /// [`NetUpdate::SectionBlocks`] is the shell's view of `BLOCK_UPDATE` /
    /// `SECTION_BLOCKS_UPDATE`: the authoritative state has **already** been
    /// applied to the one store by the adapter, which (since #374) already created
    /// or removed the block entity with it. So this does not correct the world —
    /// the world is corrected by construction, including a refused placement whose
    /// bogus chest record is dropped by that arm's `sync_block_entity` — it only
    /// clears the prediction from [`Placement`]'s ledger and asks whether the
    /// server agreed.
    ///
    /// Both halves matter. Without the clear the ledger grows without bound for the
    /// whole session, one entry per right-click, because nothing else drains it (the
    /// `block_changed_ack` sequence is decoded by the adapter but has no shell
    /// consumer). Without the answer a refusal is invisible.
    fn reconcile_predictions(&mut self, sx: i32, sy: i32, sz: i32, blocks: &[[u8; 3]]) {
        let pending: Vec<BlockPos> = self.read(|w| {
            w.resource::<PlacementPredictor>()
                .0
                .pending()
                .iter()
                .map(|prediction| prediction.pos)
                .collect()
        });
        // The common case by far — one `O(1)` read, and a `/fill` of 4096 cells
        // does no per-cell work at all.
        if pending.is_empty() {
            return;
        }
        for &[rel_x, rel_y, rel_z] in blocks {
            let pos = BlockPos::new(
                (sx << 4) | i32::from(rel_x),
                (sy << 4) | i32::from(rel_y),
                (sz << 4) | i32::from(rel_z),
            );
            if !pending.contains(&pos) {
                continue;
            }
            let server_block = self
                .net
                .as_ref()
                .and_then(|net| net.block_at(pos))
                .and_then(lodestone_data::block_states::block_name)
                .and_then(|name| name.parse::<lodestone_model::Identifier>().ok());
            let outcome = self.write(|w| {
                w.resource_mut::<PlacementPredictor>()
                    .0
                    .reconcile(pos, server_block.as_ref())
            });
            if outcome.corrected {
                tracing::debug!(
                    target: "placement",
                    "server overrode the predicted block at {:?} with {:?}",
                    pos,
                    server_block
                );
            }
        }
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
                    self.reconcile_predictions(x, y, z, &blocks);
                    self.remesh_changed_blocks(x, y, z, &blocks);
                }
                NetUpdate::BlockEvent { pos, b0, b1 } => {
                    // Chest lids (issue #23). `ChestBlockEntity.triggerEvent`
                    // takes `b0 == 1` and `b1 > 0` as "somebody is looking in
                    // this chest"; `ChestLids` owns both that rule and the
                    // per-tick ramp, so this arm forwards the raw bytes rather
                    // than interpreting them here. Every other `b0` belongs to
                    // some other block type (a note block's pitch, a piston's
                    // direction) and is dropped by `apply_block_event`.
                    self.chest_lids.apply_block_event(pos, b0, b1);
                }
                NetUpdate::ItemPickup(event) => {
                    // Issue #365. Accumulated, not acted on here: the drain at the
                    // end of this function needs a `&mut World` guard and there is
                    // no reason to take one per collected item.
                    self.pickups.apply(&event);
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
                    // The *other* half of vanilla's `case 2001`, which this arm
                    // used to drop: `playLocalSound(pos, getBreakSound(), …)`.
                    // `Level.destroyBlock` fires the event with **no** excluded
                    // entity (`Level.java:280-289`), so this is a genuinely
                    // server-sent sound, not a prediction — every client in range
                    // hears it, the breaker included. Note which breaks reach here:
                    // `Level.destroyBlock`'s callers (a torch losing support, fire
                    // spread, an explosion), *not* a player's own dig, which
                    // `ServerPlayerGameMode.destroyBlock` routes through
                    // `removeBlock` with no `levelEvent` at all — see the long note
                    // in `interact.rs` on the same asymmetry for the particles.
                    self.play_block_break_sound([pos.x, pos.y, pos.z], state);
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
                // ([`NetUpdate::Death`]); health reaching zero is not itself a
                // session event and does not unload chunks.
                NetUpdate::Death { message } => {
                    // Death is a state the shell rides through, not the end of the
                    // session. `net::run` now builds the client with
                    // `RespawnPolicy::Manual` (issue #103), so nothing respawns
                    // automatically here: this arm marks the player dead (which
                    // freezes movement in `step`) and records the message for the
                    // death screen (`app.rs`'s `drive_ui_from_session` notices
                    // `is_dead()` and shows it); the screen's Respawn button is
                    // what eventually calls `Self::respawn`. The new position
                    // rides in on the placement teleport that follows
                    // `NetUpdate::Respawned`, whose arm snaps `prev_position` too.
                    if self.recover_from_death {
                        self.set_dead(true);
                        self.death_message = Some(message);
                        self.status = "you died".into();
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
                    self.death_message = None;
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
                    // `reason` is an unresolved `Text` (issue #68): a kicked
                    // player's disconnect reason is a `translate` component
                    // like `multiplayer.disconnect.kicked`, so it has to go
                    // through the same read-boundary translator that
                    // `title_overlay`/`action_bar_overlay` already use,
                    // rather than being formatted straight into `status`.
                    let reason = self.resolve_text(&reason).to_legacy_string();
                    self.status = format!("disconnected: {reason}");
                    self.set_phase(SessionPhase::Ended(format!("disconnected: {reason}")));
                }
                NetUpdate::Error(e) => {
                    self.status = format!("net error: {e}");
                    self.set_phase(SessionPhase::Ended(format!("net error: {e}")));
                }
            }
        }

        // Start this frame's pickup animations (issue #365) — **inside `poll_net`,
        // ahead of `fold_entities`, and that ordering is the whole trick.**
        // `handleTakeItemEntity` removes the item entity in the same breath as it
        // spawns the animation, so by the time `Sim::step` reaches `fold_entities`
        // the server has stopped reporting the item and `fold_snapshots` prunes its
        // render track and its `ItemStacks` entry. `begin_item_pickup` reads both.
        // Deferring this by even one call site draws nothing, silently.
        let pickups = self.pickups.drain();
        if !pickups.is_empty() {
            self.write(|w| {
                for pickup in pickups {
                    // `false` is "the item was not tracked on the render side" —
                    // no stack ever reported, or the track already pruned. Nothing
                    // to animate, and that is the pre-#365 behaviour rather than a
                    // failure worth logging every time somebody walks over an
                    // unreported drop.
                    let _ = crate::entities::begin_item_pickup(
                        w,
                        pickup.item_entity_id,
                        pickup.collector_id,
                    );
                }
            });
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

    /// Play a block's break sound at the centre of `block`, the half of vanilla's
    /// `LevelEventHandler` `case 2001` this shell used to drop on the floor.
    ///
    /// `case 2001` does *two* things with the state id the event carries
    /// (`LevelEventHandler.java:283-291`): `addDestroyBlockEffect` **and**
    /// `playLocalSound(pos, soundType.getBreakSound(), SoundSource.BLOCKS, …)`.
    /// Only the first was wired, so every block break in the game was visually
    /// right and silent — from an event already decoded, routed and handled. See
    /// `docs/sound-playback.md`.
    fn play_block_break_sound(&mut self, block: [i32; 3], state: u32) {
        self.play_block_surface_sound(block, state, lodestone_data::sound_types::break_sound_name);
    }

    /// Play a block's place sound at the centre of `block` — vanilla's
    /// `BlockItem.place` tail (`BlockItem.java:87`), which passes the placing
    /// player as the *excluded* entity, so on the acting client the sound is
    /// **predicted** rather than received. (`ClientLevel.playSound` inverts the
    /// exclusion: it plays only when `except == minecraft.player`.) Another
    /// player's placement arrives as an ordinary `SOUND` packet and is already
    /// audible through the [`NetUpdate::Sound`] arm.
    fn play_block_place_sound(&mut self, block: [i32; 3], state: u32) {
        self.play_block_surface_sound(block, state, lodestone_data::sound_types::place_sound_name);
    }

    /// The shared body of the two above: resolve the block state's `SoundType`,
    /// pick one of its sounds, and play it at the block's centre with vanilla's
    /// break/place scaling.
    ///
    /// Three things here are vanilla's, not ours, and all three come from the
    /// same two call sites (`LevelEventHandler.java:288-289` and
    /// `BlockItem.java:87`):
    ///
    /// * the position is the **block centre** — `Level.playLocalSound(BlockPos, …)`
    ///   forwards `pos.getX() + 0.5` and so on (`Level.java:472-476`);
    /// * the volume is `(soundType.getVolume() + 1.0) / 2.0` and the pitch is
    ///   `soundType.getPitch() * 0.8`, both computed by
    ///   [`lodestone_data::sound_types::BlockSoundType`] so neither multiplier is
    ///   retyped per call site;
    /// * the category is `SoundSource.BLOCKS`.
    ///
    /// The **air guard** is vanilla's too (`case 2001`'s `if (!blockState.isAir())`)
    /// and is not redundant: air has a `SoundType` in the table — `STONE`, as it
    /// happens — so without it an air-state level event would play a stone break.
    fn play_block_surface_sound(
        &mut self,
        block: [i32; 3],
        state: u32,
        pick: fn(u32) -> Option<&'static str>,
    ) {
        if is_air_state(state) {
            return;
        }
        let Some(sound) = lodestone_data::sound_types::sound_type(state) else {
            return;
        };
        // `None` also covers `minecraft:intentionally_empty`, the sentinel vanilla
        // parks in a slot it does not want to fill (water, lava and bubble columns
        // are the three blocks with no break sound at all).
        let Some(name) = pick(state) else {
            return;
        };
        let seed = self.block_sound_seed(block);
        let volume = sound.break_or_place_volume();
        let pitch = sound.break_or_place_pitch();
        let Some(audio) = &mut self.audio else {
            return;
        };
        audio.play_sound(
            name,
            lodestone_model::event::SoundCategory::Block,
            glam::Vec3::new(
                block[0] as f32 + 0.5,
                block[1] as f32 + 0.5,
                block[2] as f32 + 0.5,
            ),
            volume,
            pitch,
            seed,
        );
    }

    /// A variant-selection seed for a sound this client decided to play.
    ///
    /// Vanilla uses `this.random.nextLong()` for a level event
    /// (`ClientLevel.java:723-733`), i.e. the variant is *client*-chosen and needs
    /// no cross-client agreement — unlike a `SOUND` packet's seed, which must be
    /// passed through unchanged (`lodestone-audio/src/select.rs`).
    ///
    /// So this is a `splitmix64` finalizer over the block position and the fixed
    /// tick count. Two properties are deliberate:
    ///
    /// * **not `Instant::now`** — `select.rs` rules it out (it panics on wasm), and
    ///   this crate's other RNG-free paths avoid `getrandom` for the same reason;
    /// * **not the particle engine's `JavaRandom`** (`Particles`' own
    ///   `engine.rng()`), even though it is already in scope at the break site.
    ///   Drawing from it would shift every subsequent particle draw, and the
    ///   destroy-burst golden tests (`mining_destroy_burst`,
    ///   `break_particle_tint`) are written against that exact sequence.
    ///
    /// Mixing in `ticks` rather than position alone is what stops re-breaking one
    /// cell from picking the same `.ogg` variant every time.
    fn block_sound_seed(&self, block: [i32; 3]) -> i64 {
        let mut x = (block[0] as i64 as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (block[1] as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ (block[2] as i64 as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
            ^ self.clock().ticks;
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (x ^ (x >> 31)) as i64
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
    /// Push vanilla's View Bobbing option down from the menu layer. Cheap and
    /// idempotent; `app.rs` calls it once per presented frame rather than on the
    /// toggle, for the same reason the deleted present-mode poll did — the menu
    /// is pure and owns the `Options`, and `Sim` owns none.
    pub fn set_view_bobbing(&mut self, on: bool) {
        self.view_bobbing = on;
    }

    /// The interpolated walk bob this frame, or an all-zero frame when the option
    /// is off. Exposed so a gate can assert the *input* to the camera fold
    /// separately from the fold itself.
    #[must_use]
    pub fn bob_frame(&self) -> crate::camera_rig::BobFrame {
        if !self.view_bobbing {
            return crate::camera_rig::BobFrame::default();
        }
        self.view_bob.frame(self.clock().interp_alpha)
    }

    #[must_use]
    pub fn render_camera(&self, aspect: f32) -> Camera {
        // The bob lands **here and not in `Self::camera`**, which is deliberate
        // and is the difference between a wobbling camera and a wobbling *game*:
        // `Self::camera` is also the block-targeting ray origin and the audio
        // listener, and vanilla bobs neither. `GameRenderer.renderLevel` folds the
        // bob into the *projection matrix* (`:539`), so `Camera`'s own position
        // and rotation — what `getPickRay` and the listener read — never see it.
        //
        // Not gated on `third_person`: 26.2's `renderLevel` applies `bobView`
        // whenever `optionsRenderState.bobView` is set, with no camera-type check
        // (`GameRenderer.java:534-536`), and `bobView` itself only tests
        // `isPlayer`. Older versions did suppress it in third person and issue
        // #58's body says so; re-read against `.cache/mc/26.2/client-src`, that is
        // no longer true.
        let eye = bobbed_camera(
            self.camera(aspect),
            self.bob_frame(),
            // `bobHurt` is deliberately **not** driven from here yet: it is almost
            // entirely a roll, and `bobbed_camera` cannot carry roll, so wiring it
            // would produce a visibly wrong tilt rather than a slightly imprecise
            // one. `ViewBob::hurt` and `BobFrame::hurt_roll_degrees` are
            // implemented and tested against vanilla; see `docs/view-bobbing.md`
            // for what the last hop needs.
            0.0,
        );
        if !self.third_person {
            // Issue #154: vanilla's FOV zoom is gated on `firstPerson &&
            // isScoping()` (`AbstractClientPlayer.getFieldOfViewModifier`,
            // `AbstractClientPlayer.java:92-114`) — a third-person camera
            // never zooms, so this composition only runs on the early
            // first-person return, not the two third-person branches below.
            return apply_spyglass_fov(eye, self.spyglass_scoping());
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

    /// Vanilla's `Player.isScoping()` (issue #154):
    /// `isUsingItem() && getUseItem().is(Items.SPYGLASS)`
    /// (`Player.java:1936-1938`), computed entirely from `Sim`'s own state so
    /// [`Self::render_camera`] needs no new parameter — `app.rs` computes the
    /// same condition independently for `ScreenEffects::scoping` (it already
    /// has the held item at hand for the first-person render source), and
    /// the two are expected to agree rather than share a call, the same way
    /// `wearing_pumpkin` is computed locally in `app.rs` rather than exposed
    /// from here.
    #[must_use]
    fn spyglass_scoping(&self) -> bool {
        self.using_item()
            && self
                .player_menu()
                .player_native(self.selected_slot())
                .is_some_and(|st| st.item().to_string() == "minecraft:spyglass")
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
                // **Not wired for the local player yet (issue #57).** Remote
                // entities get their bow/crossbow pose from
                // `entities::arm_pose_for`, driven by the `ItemUse` component that
                // `ingest::apply_entity_item_use` folds off the living-flags byte.
                // The local player cannot use that path: it has no `EntityKind`/
                // `Position`/`Rotation`/`HeadYaw` (deliberately — that absence is
                // what keeps a self-model off `ClientHandle::entities()`), so
                // `entity_view()`'s early `?` returns before the flags are read,
                // exactly as it does for `Vitals::on_fire`. Reaching it needs a
                // session-scoped fold and a `PlayerSnapshot` field, the same shape
                // `apply_local_player_on_fire` has. Left explicit rather than
                // spread with `..AnimInput::REST` so the gap is visible here.
                arm_pose: lodestone_render::ArmPose::Empty,
                arm_pose_left_hand: false,
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
mod tests;
