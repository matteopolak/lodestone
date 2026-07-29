//! Adapts a block world to the physics engine's [`CollisionView`], so the shell
//! drives the **real** bit-exact movement code in `lodestone-physics` rather
//! than any ad-hoc integrator.
//!
//! Two adapters live here, one per world source:
//! - [`WorldCollision`] over the offline demo [`lodestone_world::World`], keyed
//!   by the demo palette.
//! - [`LiveCollision`] over an owned snapshot of the **live server world**, keyed
//!   by vanilla block-state ids and the vanilla classifier.
//!
//! # Real per-state shapes, not a unit cube per solid block
//!
//! Both adapters used to emit *one unit cube* for every block that occludes, and
//! nothing for everything else. In the demo palette that is exactly right — its
//! nine blocks are all full cubes or air. In live play it was the single largest
//! correctness gap in the client: **no slabs, no stairs, no fences, no walls, no
//! ice, no ladders, no cobwebs, no soul sand**. A player stood on top of a
//! bottom slab at `y + 1.0` instead of `y + 0.5`, walked through the top half of
//! a stair, and could step over a fence that vanilla makes 1.5 blocks tall.
//!
//! That mattered quantitatively, not just aesthetically. 26.2's server replays
//! our movement delta through `move(MoverType.PLAYER, …)` and rubber-bands as
//! soon as horizontal disagreement passes **0.25 blocks in a single packet, with
//! no accumulator** (`docs/baritone-port.md` §3.2). `lodestone-physics` is
//! bit-exact against two independent oracles across 26 zero-tolerance golden
//! traces, so the integrator was never the problem — it was being fed a world in
//! which slabs did not exist. Half a block of vertical error on the first slab is
//! **2× the rubber-band threshold**.
//!
//! The real shapes come from the version crate's generated collision census
//! (dumped from the real server's `Block.BLOCK_STATE_REGISTRY`) and reach this
//! version-free module through [`VersionAdapter::block_collision`] — the same
//! sanctioned seam `block_hardness` and `tool_mining` use. See
//! [`docs/collision-shapes.md`](https://github.com/) in this repo for the census
//! and how to extend it.
//!
//! # One answer per question, shared by both adapters
//!
//! The two adapters implement *one* trait, so any disagreement between them is a
//! bug that hides: a test passes against `WorldCollision` while the game
//! misbehaves against `LiveCollision`. They are kept honest structurally rather
//! than by review — every one of [`CollisionView`]'s answers is computed **once**,
//! in a free function over the private [`BlockView`] trait, and each `impl
//! CollisionView` block is nothing but one-line delegation. The only things the
//! two adapters supply differently are:
//!
//! | question | [`WorldCollision`] | [`LiveCollision`] |
//! |---|---|---|
//! | state id at a cell | demo palette | snapshot section |
//! | shape of a state | full cube if solid | version collision census |
//! | fluid kind | [`demo_fluid`] | [`vanilla_fluid`] |
//! | fluid cell (level) | `None` — the palette has no `level` property | `BlockModels::fluid` |
//! | vanilla block name | fixed demo id → name table | [`VersionAdapter::block_name`] |
//!
//! # Fluids have one classifier, not one per consumer
//!
//! Neither adapter decides what water *is*. Both call
//! [`crate::blocks::vanilla_fluid`] / [`crate::blocks::demo_fluid`], and the
//! vanilla one delegates to [`BlockModels::fluid`](lodestone_render::BlockModels::fluid)
//! — the classification the mesher already bakes the water surface from. That is
//! deliberate: `is_water` gates swimming, and the [`FluidState`] it feeds
//! (`lodestone_physics::compute_fluid_state`) gates the submerged fog, the
//! underwater overlay and the `ambient.underwater.*` sounds. This adapter matching
//! an exact `minecraft:water` id — while the mesher drew water for every
//! `waterlogged=true` block and for kelp/seagrass/bubble columns — is precisely
//! how a player ended up standing *visibly* underwater, unable to swim, with clear
//! sky fog. One question, one answer.
//!
//! # …but "is this cell fluid" is not "can I break what is in it"
//!
//! Unifying the fluid answer immediately broke a *second* consumer that had been
//! borrowing it: the pick ray in `Sim::update_target` used `!is_water(cell)` as
//! shorthand for "this cell holds something breakable". Once `is_water` correctly
//! included kelp, seagrass and every waterlogged stair, that shorthand started
//! refusing all of them — **kelp could not be broken, because it could not be
//! targeted**. Vanilla asks a genuinely different question there (the *outline*
//! shape, with fluid shapes switched off), so the answer lives in its own
//! predicate, [`LiveCollision::is_pickable`], not in a negation of `is_water`. One
//! question one answer is right; the mistake was assuming there was one question.
//!
//! The same warning applies to the collision census wired in here: **collision
//! shape is not outline shape**. A fluid has a full collision-less cell and an
//! *empty* outline; kelp has an outline and no collision; soul sand collides to
//! `y = 0.875` and outlines to `1.0`. [`LiveCollision::shape_of`](BlockView::shape_of)
//! (collision) must never be used to decide what the crosshair selects.
//! [`LiveCollision::is_pickable`] now answers that question from the real
//! per-state **outline** census (`VersionAdapter::block_outline`, dumped from
//! `BlockStateBase.getShape`) rather than the "has baked model quads" proxy
//! that shipped with the kelp fix — see that method's docs for what changed
//! and the one deliberate behaviour change (`minecraft:light`).
//!
//! [`FluidState`]: lodestone_physics::FluidState
//! [`demo_fluid`]: crate::blocks::demo_fluid
//! [`vanilla_fluid`]: crate::blocks::vanilla_fluid

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use lodestone_model::{BlockAabb, VersionAdapter};
use lodestone_physics::{Aabb, CollisionView, FluidCell, HorizontalDir, Vec3d};
use lodestone_render::{BlockAtlas, BlockClassifier, FluidKind};
use lodestone_world::{ChunkPos, ChunkSection, World};

use crate::blocks::{demo_fluid, id, vanilla_fluid};

// ---------------------------------------------------------------------------
// The shared seam: what the two adapters answer differently
// ---------------------------------------------------------------------------

/// A block-local unit cube — the shape of every full block, and the fallback a
/// [`LiveCollision`] with no version data is reduced to.
const FULL_CUBE: &[BlockAabb] = &[BlockAabb {
    min: [0.0, 0.0, 0.0],
    max: [1.0, 1.0, 1.0],
}];

/// No collision at all. Distinct from a zero-volume box: vanilla's
/// `Shapes.empty()` contributes nothing to a sweep, and a zero-volume box would
/// still be tested (and, with the `1.0E-7` epsilon, could stop a movement).
const NO_COLLISION: &[BlockAabb] = &[];

/// The four per-cell facts an adapter must supply. Everything [`CollisionView`]
/// answers is derived from these by the free functions below, so both adapters
/// share one body per answer and cannot drift.
///
/// Private on purpose: this is an internal factorisation, not a public seam. The
/// public seam is [`CollisionView`] itself.
trait BlockView {
    /// Block-state id at world coordinates, in this adapter's own id space.
    /// Outside the loaded world this must report *air*, never a solid.
    fn state_at(&self, x: i32, y: i32, z: i32) -> u32;

    /// Block-local collision boxes for a state. An empty slice means "no
    /// collision", which is a real answer (air, water, kelp, cobweb).
    fn shape_of(&self, state: u32) -> &'static [BlockAabb];

    /// Which fluid, if any, this state's cell carries — for
    /// [`CollisionView::is_water`] / [`is_lava`](CollisionView::is_lava).
    fn fluid_kind_of(&self, state: u32) -> Option<FluidKind>;

    /// The fluid's *dynamic* state (amount + falling) for
    /// [`CollisionView::fluid_at`], or `None` when this adapter cannot know it.
    /// See the module table: the demo palette genuinely cannot.
    fn fluid_cell_of(&self, state: u32) -> Option<FluidCell>;

    /// The vanilla block identifier for a state (`"minecraft:ice"`), for the
    /// name-keyed physics constants. `None` when unresolvable, in which case
    /// every name-keyed answer falls back to vanilla's *default* for that
    /// property (0.6 friction, 1.0 factors, no bounce, not climbable) — the same
    /// value the overwhelming majority of blocks have.
    fn name_of(&self, state: u32) -> Option<&'static str>;
}

// ---------------------------------------------------------------------------
// Shape geometry: the shared bodies
// ---------------------------------------------------------------------------

/// Appends a block-local shape to `out` in **world space**, which is the
/// coordinate space [`CollisionView::collision_boxes`] is contracted in.
///
/// Widening `f32 -> f64` is exact, so this is lossless against the game's
/// `double` shapes.
fn emit_world_boxes(shape: &[BlockAabb], x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
    let (bx, by, bz) = (f64::from(x), f64::from(y), f64::from(z));
    for b in shape {
        out.push(Aabb::new(
            bx + f64::from(b.min[0]),
            by + f64::from(b.min[1]),
            bz + f64::from(b.min[2]),
            bx + f64::from(b.max[0]),
            by + f64::from(b.max[1]),
            bz + f64::from(b.max[2]),
        ));
    }
}

/// `shape.max(Axis.Y)`, block-local and **uncapped** — a fence is `1.5`, a bottom
/// slab `0.5`, soul sand `0.875`, air `0.0`.
///
/// Overriding [`CollisionView::collision_top`] rather than taking its default is
/// not a micro-optimisation: the default gathers into a `Vec` it allocates on
/// every call, and a pathfinder asks this for every candidate cell.
fn shape_top(shape: &[BlockAabb]) -> f64 {
    shape
        .iter()
        .map(|b| f64::from(b.max[1]))
        .fold(0.0_f64, f64::max)
}

/// The union bounding box of a shape, block-local, or `None` for an empty shape.
fn shape_bounds(shape: &[BlockAabb]) -> Option<([f32; 3], [f32; 3])> {
    let mut it = shape.iter();
    let first = it.next()?;
    let (mut min, mut max) = (first.min, first.max);
    for b in it {
        for a in 0..3 {
            min[a] = min[a].min(b.min[a]);
            max[a] = max[a].max(b.max[a]);
        }
    }
    Some((min, max))
}

/// Vanilla's `BlockStateBase.calculateSolid` over the collision shape:
/// non-empty, and either the *mean* of the bounding box's three dimensions is at
/// least `0.7291666666666666` or its Y size is at least `1.0`.
///
/// That magic constant is not arbitrary — it is exactly `(1 + 1 + 3/16) / 3`, the
/// mean size of a ladder's collision box, so a ladder lands precisely *on* the
/// threshold. Vanilla flips it back off with `forceSolidOff()`; see
/// [`blocks_motion_at`] for the two lists this cannot see.
fn shape_is_solid(shape: &[BlockAabb]) -> bool {
    let Some((min, max)) = shape_bounds(shape) else {
        return false;
    };
    let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let mean = f64::from(size[0] + size[1] + size[2]) / 3.0;
    mean >= 0.7291666666666666 || f64::from(size[1]) >= 1.0
}

/// `Block.isFaceFull(collisionShape, dir)` for a horizontal direction: does the
/// shape cover that whole 1×1 face?
///
/// **Under-approximation, deliberately.** Vanilla unions the whole `VoxelShape`
/// before testing, so a face covered by *several* boxes jointly counts; this
/// asks whether any *single* box covers it. Every real block whose face is full
/// has one box that does it (a cube, a slab's own side, a double-height door),
/// so the two agree on the blocks that exist — but the general case is not
/// proven, and the honest statement is "no false positives, possible false
/// negatives". The only consumer is a *falling* fluid's downward jet
/// ([`lodestone_physics::get_flow`]), where a false negative loses a `-6.0`
/// vertical nudge on a waterfall hugging a multi-box wall.
fn shape_face_is_full(shape: &[BlockAabb], dir: HorizontalDir) -> bool {
    // Axis normal to the face, and the two axes spanning it.
    let (axis, at_max) = match dir {
        HorizontalDir::North => (2, false),
        HorizontalDir::South => (2, true),
        HorizontalDir::West => (0, false),
        HorizontalDir::East => (0, true),
    };
    let span = if axis == 0 { [1, 2] } else { [0, 1] };
    shape.iter().any(|b| {
        let touches = if at_max {
            b.max[axis] >= 1.0
        } else {
            b.min[axis] <= 0.0
        };
        touches && span.iter().all(|&a| b.min[a] <= 0.0 && b.max[a] >= 1.0)
    })
}

// ---------------------------------------------------------------------------
// Name-keyed physics constants
// ---------------------------------------------------------------------------
//
// These six values are `BlockBehaviour.Properties` fields and tag memberships,
// not geometry, so no collision census can carry them — they are keyed by block
// *name*, which is why `VersionAdapter::block_name` exists. Every value below was
// read out of the decompiled 26.2 `Blocks.java` / `data/minecraft/tags/block/*`,
// with the line cited, rather than recalled.

/// `Block.getFriction` — `BlockBehaviour.Properties.friction`, default `0.6`.
///
/// `Blocks.java`: ice/packed ice/frosted ice `0.98` (1950, 3021, 3732), blue ice
/// `0.989` (4227), slime block `0.8` (2926). Nothing else in 26.2 sets it.
fn friction_for(name: &str) -> f32 {
    match name {
        "minecraft:ice" | "minecraft:packed_ice" | "minecraft:frosted_ice" => 0.98,
        "minecraft:blue_ice" => 0.989,
        "minecraft:slime_block" => 0.8,
        _ => 0.6,
    }
}

/// `Block.getSpeedFactor` — default `1.0`. `Blocks.java`: soul sand `0.4` (2024),
/// honey block `0.4` (4843). Nothing else in 26.2 sets it.
fn speed_factor_for(name: &str) -> f32 {
    match name {
        "minecraft:soul_sand" | "minecraft:honey_block" => 0.4,
        _ => 1.0,
    }
}

/// `Block.getJumpFactor` — default `1.0`. `Blocks.java`: honey block `0.5`
/// (4843), the only block in 26.2 that sets it.
fn jump_factor_for(name: &str) -> f32 {
    match name {
        "minecraft:honey_block" => 0.5,
        _ => 1.0,
    }
}

/// `Block.getBounceRestitution`, already net of `BlockTags.SUPPRESSES_BOUNCE`
/// — default `0.0`. `Blocks.java`: slime block `1.0` (2926), every bed `0.75`
/// (684, via the `BED` colour collection).
///
/// The suppression tag needs no subtraction here: its sole member is
/// `minecraft:honey_block` (`tags/block/suppresses_bounce.json`), which sets no
/// restitution in the first place, so tag-aware and tag-blind agree on every
/// block in 26.2. Should a future version add a bouncy suppressor, this is where
/// it breaks — and it will break silently, so re-read the tag on a data bump.
///
/// All 16 `*_bed` states are matched by suffix; `block_states.rs` confirms
/// exactly 16 names end in `_bed` in 26.2 and all of them are beds.
fn bounce_for(name: &str) -> f32 {
    match name {
        "minecraft:slime_block" => 1.0,
        n if n.ends_with("_bed") => 0.75,
        _ => 0.0,
    }
}

/// `Block.entityInside` → `Entity.makeStuckInBlock` — the per-axis speed
/// multiplier of the three blocks that grab you. `None` for everything else.
///
/// `WebBlock.java:33` `(0.25, 0.05, 0.25)`, `PowderSnowBlock.java:66`
/// `(0.9, 1.5, 0.9)`, `SweetBerryBushBlock.java:86` `(0.8, 0.75, 0.8)`. Note
/// `WebBlock` gives a `WEAVING` mob `(0.5, 0.25, 0.5)` instead; that is a
/// per-entity override which `CollisionView` deliberately does not model here.
fn stuck_for(name: &str) -> Option<Vec3d> {
    match name {
        "minecraft:cobweb" => Some(Vec3d::new(0.25, 0.05, 0.25)),
        "minecraft:powder_snow" => Some(Vec3d::new(0.9, 1.5, 0.9)),
        "minecraft:sweet_berry_bush" => Some(Vec3d::new(0.8, 0.75, 0.8)),
        _ => None,
    }
}

/// `BlockTags.CLIMBABLE`, verbatim from `data/minecraft/tags/block/climbable.json`
/// in the 26.2 jar — all nine entries, no guesses.
///
/// `cave_vines`/`cave_vines_plant` are in the tag even though they are the glow
/// berry vine, and `scaffolding` is in it but holds differently when sneaking (a
/// distinction [`CollisionView::is_climbable`] does not carry).
fn is_climbable_name(name: &str) -> bool {
    matches!(
        name,
        "minecraft:ladder"
            | "minecraft:vine"
            | "minecraft:scaffolding"
            | "minecraft:weeping_vines"
            | "minecraft:weeping_vines_plant"
            | "minecraft:twisting_vines"
            | "minecraft:twisting_vines_plant"
            | "minecraft:cave_vines"
            | "minecraft:cave_vines_plant"
    )
}

// ---------------------------------------------------------------------------
// The shared answers: one body each, both adapters delegate here
// ---------------------------------------------------------------------------

fn boxes_at(v: &impl BlockView, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
    emit_world_boxes(v.shape_of(v.state_at(x, y, z)), x, y, z, out);
}

fn top_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> f64 {
    shape_top(v.shape_of(v.state_at(x, y, z)))
}

fn friction_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> f32 {
    v.name_of(v.state_at(x, y, z)).map_or(0.6, friction_for)
}

fn speed_factor_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> f32 {
    v.name_of(v.state_at(x, y, z)).map_or(1.0, speed_factor_for)
}

fn jump_factor_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> f32 {
    v.name_of(v.state_at(x, y, z)).map_or(1.0, jump_factor_for)
}

fn bounce_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> f32 {
    v.name_of(v.state_at(x, y, z)).map_or(0.0, bounce_for)
}

fn stuck_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> Option<Vec3d> {
    v.name_of(v.state_at(x, y, z)).and_then(stuck_for)
}

fn climbable_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> bool {
    v.name_of(v.state_at(x, y, z))
        .is_some_and(is_climbable_name)
}

fn is_water_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> bool {
    v.fluid_kind_of(v.state_at(x, y, z)) == Some(FluidKind::Water)
}

fn is_lava_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> bool {
    v.fluid_kind_of(v.state_at(x, y, z)) == Some(FluidKind::Lava)
}

fn fluid_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> Option<FluidCell> {
    v.fluid_cell_of(v.state_at(x, y, z))
}

/// `BlockState.blocksMotion()` = `block != COBWEB && block != BAMBOO_SAPLING &&
/// isSolid()`, where `isSolid` is the cached `legacySolid` flag
/// (`BlockBehaviour.java:542-549`).
///
/// # What this gets wrong, and why it is not silent
///
/// `legacySolid` is [`shape_is_solid`] *unless* the block overrides it with
/// `forceSolidOn()` / `forceSolidOff()`, and 26.2 has **143 blocks with
/// `forceSolidOn` and 8 with `forceSolidOff`** — no committed table in this repo
/// carries that flag, so those 151 are answered from geometry and some of them
/// are wrong:
///
/// * `forceSolidOn` with an empty or thin collision shape reads here as **not**
///   blocking motion when vanilla says it does: every sign and hanging sign,
///   every pressure plate, an *open* fence gate, lanterns, chains, cobweb,
///   bamboo, cake, bell, dead corals, turtle egg.
/// * `forceSolidOff` reads as blocking when vanilla says it does not: ladder
///   (which sits exactly on the `0.7291666…` threshold — that is why the
///   override exists), snow, azalea, big dripleaf, chorus plant/flower, end rod.
///   Ladder is hard-coded off below because it is the one a player meets
///   constantly.
///
/// The blast radius is small and known: `blocks_motion` has exactly one consumer,
/// [`lodestone_physics::get_flow`]'s empty-neighbour branch, which decides whether
/// a fluid spills over an edge. Nothing about the player's own movement reads it.
/// Closing the gap properly means a `legacySolid` (or `forceSolid*`) column beside
/// the collision census in the version crate.
fn blocks_motion_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> bool {
    let state = v.state_at(x, y, z);
    match v.name_of(state) {
        // The two explicit exclusions in `blocksMotion` itself, plus the one
        // `forceSolidOff` block a player touches every session.
        Some("minecraft:cobweb" | "minecraft:bamboo_sapling" | "minecraft:ladder") => false,
        _ => shape_is_solid(v.shape_of(state)),
    }
}

/// `FlowingFluid.isSolidFace` (`FlowingFluid.java:105-115`), horizontal case:
/// `false` if the cell holds the same fluid, `false` for ice, else
/// `isFaceSturdy(FULL)` = [`shape_face_is_full`].
///
/// Two approximations, both narrowing:
/// * the seam does not say *which* fluid is flowing, so **any** fluid in the cell
///   answers `false` (vanilla only excludes the same fluid — so water beside lava
///   loses the jet);
/// * `isFaceSturdy` is the under-approximating [`shape_face_is_full`].
///
/// Vanilla's `direction == UP -> true` branch is unreachable here: the seam is
/// typed [`HorizontalDir`], so the vertical case cannot be asked.
fn is_solid_face_at(v: &impl BlockView, x: i32, y: i32, z: i32, dir: HorizontalDir) -> bool {
    let state = v.state_at(x, y, z);
    if v.fluid_kind_of(state).is_some() {
        return false;
    }
    // `IceBlock` covers ice, frosted ice and blue ice; packed ice is a plain
    // `Block`, so it is *not* excluded (`IceBlock` subclasses only).
    if matches!(
        v.name_of(state),
        Some("minecraft:ice" | "minecraft:frosted_ice" | "minecraft:blue_ice")
    ) {
        return false;
    }
    shape_face_is_full(v.shape_of(state), dir)
}

// ---------------------------------------------------------------------------
// The demo world
// ---------------------------------------------------------------------------

/// A [`CollisionView`] over a borrowed [`World`].
#[derive(Debug)]
pub struct WorldCollision<'a> {
    world: &'a World,
}

impl<'a> WorldCollision<'a> {
    /// Wrap a world.
    #[must_use]
    pub fn new(world: &'a World) -> Self {
        Self { world }
    }

    /// Block-state id at world coordinates, or [`id::AIR`] outside loaded chunks.
    #[must_use]
    pub fn block_at(&self, x: i32, y: i32, z: i32) -> u32 {
        let pos = ChunkPos {
            x: x.div_euclid(16),
            z: z.div_euclid(16),
        };
        let Some(chunk) = self.world.get(pos) else {
            return id::AIR;
        };
        let col = &chunk.column;
        if y < col.min_y() || y >= col.max_y() {
            return id::AIR;
        }
        col.get_block(x.rem_euclid(16) as usize, y, z.rem_euclid(16) as usize)
    }

    /// Whether the block at these coordinates is a full-cube collider.
    ///
    /// In the demo palette this really is the whole story — every block in it is
    /// either air, water, or a full cube — which is why [`shape_of`] can hand back
    /// [`FULL_CUBE`] on the strength of it. The live adapter's counterpart is
    /// *not* the collision answer; see [`LiveCollision::is_solid`].
    ///
    /// [`shape_of`]: BlockView::shape_of
    #[must_use]
    pub fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        let b = self.block_at(x, y, z);
        b != id::AIR && b != id::WATER
    }

    /// Whether the view ray can *target* this cell — the demo counterpart of
    /// [`LiveCollision::is_pickable`], which carries the full explanation.
    ///
    /// The demo palette has no waterlogging, no plants sharing a cell with water
    /// and no air variants, so here the question really does reduce to "not air and
    /// not the water block".
    #[must_use]
    pub fn is_pickable(&self, x: i32, y: i32, z: i32) -> bool {
        let b = self.block_at(x, y, z);
        b != id::AIR && demo_fluid(b).is_none()
    }
}

/// The vanilla block each demo-palette id stands in for, so the demo world reads
/// the *same* name-keyed constant tables the live world does.
///
/// Every one of these resolves to vanilla's default (0.6 friction, no bounce, not
/// climbable), so the mapping changes no demo behaviour today. It exists so the
/// two adapters share one code path rather than one having stubs: if someone adds
/// ice to the demo palette, it becomes slippery with no further wiring, and if the
/// name tables gain a row that is wrong, both worlds show it.
fn demo_block_name(state: u32) -> Option<&'static str> {
    Some(match state {
        id::STONE => "minecraft:stone",
        id::DIRT => "minecraft:dirt",
        id::GRASS => "minecraft:grass_block",
        id::SAND => "minecraft:sand",
        id::WATER => "minecraft:water",
        id::LOG => "minecraft:oak_log",
        id::LEAVES => "minecraft:oak_leaves",
        id::BEDROCK => "minecraft:bedrock",
        id::GRAVEL => "minecraft:gravel",
        _ => return None,
    })
}

impl BlockView for WorldCollision<'_> {
    fn state_at(&self, x: i32, y: i32, z: i32) -> u32 {
        self.block_at(x, y, z)
    }

    fn shape_of(&self, state: u32) -> &'static [BlockAabb] {
        if state == id::AIR || state == id::WATER {
            NO_COLLISION
        } else {
            FULL_CUBE
        }
    }

    fn fluid_kind_of(&self, state: u32) -> Option<FluidKind> {
        demo_fluid(state)
    }

    /// **Always `None`, on purpose.** `fluid_at` must report the fluid's *amount*
    /// (`1..=8`) and falling flag, which vanilla derives from a `level` property.
    /// The demo palette's water is a single property-less id, so there is no
    /// amount to report — and fabricating "source, amount 8" would invent a
    /// current (`get_flow` reads neighbour levels) that this world's flat lakes do
    /// not have. Reporting `None` keeps demo water inert, exactly as before real
    /// shapes were wired in.
    fn fluid_cell_of(&self, _state: u32) -> Option<FluidCell> {
        None
    }

    fn name_of(&self, state: u32) -> Option<&'static str> {
        demo_block_name(state)
    }
}

impl CollisionView for WorldCollision<'_> {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        boxes_at(self, x, y, z, out);
    }

    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        top_at(self, x, y, z)
    }

    fn friction(&self, x: i32, y: i32, z: i32) -> f32 {
        friction_at(self, x, y, z)
    }

    fn speed_factor(&self, x: i32, y: i32, z: i32) -> f32 {
        speed_factor_at(self, x, y, z)
    }

    fn jump_factor(&self, x: i32, y: i32, z: i32) -> f32 {
        jump_factor_at(self, x, y, z)
    }

    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        is_water_at(self, x, y, z)
    }

    fn is_climbable(&self, x: i32, y: i32, z: i32) -> bool {
        climbable_at(self, x, y, z)
    }

    fn is_lava(&self, x: i32, y: i32, z: i32) -> bool {
        is_lava_at(self, x, y, z)
    }

    fn stuck_multiplier(&self, x: i32, y: i32, z: i32) -> Option<Vec3d> {
        stuck_at(self, x, y, z)
    }

    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        fluid_at(self, x, y, z)
    }

    fn blocks_motion(&self, x: i32, y: i32, z: i32) -> bool {
        blocks_motion_at(self, x, y, z)
    }

    fn is_solid_face(&self, x: i32, y: i32, z: i32, dir: HorizontalDir) -> bool {
        is_solid_face_at(self, x, y, z, dir)
    }

    fn bounce_restitution(&self, x: i32, y: i32, z: i32) -> f32 {
        bounce_at(self, x, y, z)
    }
}

// ---------------------------------------------------------------------------
// The live world
// ---------------------------------------------------------------------------

/// A [`CollisionView`] over the **live server world**, keyed by vanilla block
/// state ids.
///
/// This is the multiplayer companion to [`WorldCollision`]: it makes the shell's
/// physics walk on the *server's* terrain instead of the offline demo world, with
/// the server's *real* per-state collision geometry (see the module docs for what
/// standing on a unit cube per solid block used to cost).
///
/// Two independent inputs, and it is worth keeping them straight:
///
/// * **the world** — an *owned snapshot* of block-state ids, a map of
///   `Arc<ChunkSection>` pulled from the client-owned world under a single lock
///   (see [`crate::net::NetClient::sections_at`]), so no world lock is held while
///   physics queries it and the many per-tick lookups touch only local memory;
/// * **the version data** — [`VersionAdapter`], consulted for each state's
///   collision shape and block name. Both of its accessors are `&'static` rodata
///   reads, so this adapter holds an `Arc` and copies nothing.
///
/// Without version data the view degrades to the old coarse behaviour (unit cube
/// per occluding block) rather than losing collision entirely — a player who
/// cannot stand up is worse than one standing slightly too high — but it *says
/// so*, once, at `warn` level, and [`Self::has_real_shapes`] reports it for the
/// debug overlay. A silent fallback here is how the gap survived nine months.
#[derive(Debug)]
pub struct LiveCollision {
    /// `(chunk-x, chunk-z, section-index)` → owned block section. A missing key
    /// (unloaded or all-air section) reads as air.
    sections: HashMap<(i32, i32, usize), Arc<ChunkSection>>,
    /// World-space bottom of the dimension (overworld `-64`).
    min_y: i32,
    /// Number of 16-block sections stacked in a column.
    section_count: usize,
    /// The vanilla classifier: its attached [`BlockModels`](lodestone_render::BlockModels)
    /// is the fluid oracle behind [`is_water`](CollisionView::is_water) /
    /// [`is_lava`](CollisionView::is_lava) and [`fluid_at`](CollisionView::fluid_at)
    /// — see [`vanilla_fluid`] — and `classify(id).occludes` is both the
    /// no-version-data shape fallback and the answer to [`Self::is_solid`].
    atlas: Arc<BlockAtlas>,
    /// The version data behind [`VersionAdapter::block_collision`] and
    /// [`VersionAdapter::block_name`]. `None` degrades to unit cubes; see the
    /// type docs.
    version: Option<Arc<dyn VersionAdapter>>,
    /// Resolved state ids of the three air blocks (see [`AIR_BLOCKS`]), for
    /// [`is_pickable`](Self::is_pickable). Small and fixed, so a linear scan beats
    /// a set.
    air_states: Vec<u32>,
}

/// The blocks whose **outline** shape is `Shapes.empty()` *and* whose cell holds
/// no fluid, i.e. the ones `Entity.pick` must walk straight through without them
/// being identifiable as "a fluid cell".
///
/// All three register as `AirBlock` (`Blocks.java:4273-4278`), whose `getShape`
/// returns `Shapes.empty()` (`AirBlock.java:30-32`), so none of them is targetable
/// in vanilla. This matters because **`minecraft:air` is not the only air**:
/// `WorldCarver` writes `Blocks.CAVE_AIR` (`WorldCarver.java:36`), as do lakes,
/// monster rooms and strongholds, and the end's void column is `void_air`. Each is
/// a *distinct block-state id*, so a pick predicate written as `state_id != 0`
/// targets the empty space one block in front of the player's face in any carved
/// cave, in preference to whatever real block is behind it.
const AIR_BLOCKS: [&str; 3] = ["minecraft:air", "minecraft:cave_air", "minecraft:void_air"];

/// The process-wide default version data, resolved once from the compiled-in
/// family set. See [`default_version_data`].
static DEFAULT_VERSION_DATA: OnceLock<Option<Arc<dyn VersionAdapter>>> = OnceLock::new();

/// The version data [`LiveCollision::new`] uses when the caller injects none.
///
/// A live session's protocol is settled by the time anything collides — but
/// `LiveCollision::new` is not handed it, so rather than leave the field empty
/// (which reduces the whole world to unit cubes: see the type docs) this resolves
/// the **sole compiled-in family**. That inference is sound in the only case it
/// fires: a live connection exists at all *because*
/// [`lodestone_registry::adapter_for_protocol`] matched a compiled family
/// (`net.rs`), so with exactly one family compiled it is that one. A default build
/// (no `live` feature) has none, and a hypothetical multi-family build is
/// ambiguous; both log and fall back.
///
/// Prefer [`LiveCollision::with_version_data`], which passes the *connected*
/// protocol's adapter and needs no inference. This exists so that no build can
/// silently lose collision geometry by forgetting to wire it.
///
/// Resolved once for the process: `adapter_for_protocol` builds a boxed adapter
/// per call, and `LiveCollision` is rebuilt every tick.
fn default_version_data() -> Option<Arc<dyn VersionAdapter>> {
    DEFAULT_VERSION_DATA
        .get_or_init(|| {
            let protocols = lodestone_registry::supported_protocols();
            let &[protocol] = protocols.as_slice() else {
                if protocols.is_empty() {
                    tracing::warn!(
                        target: "physics",
                        "no version family compiled in: live collision falls back to a unit cube \
                         per solid block, so slabs, stairs, fences and ice will be wrong \
                         (build with --features live)"
                    );
                } else {
                    tracing::warn!(
                        target: "physics",
                        families = ?lodestone_registry::compiled_families(),
                        "more than one version family compiled in, so the collision-shape source \
                         is ambiguous; falling back to a unit cube per solid block. Wire \
                         LiveCollision::with_version_data from the connected protocol."
                    );
                }
                return None;
            };
            let adapter = lodestone_registry::adapter_for_protocol(protocol).map(Arc::from);
            if adapter.is_none() {
                tracing::warn!(
                    target: "physics",
                    protocol,
                    "compiled family does not resolve its own protocol; live collision falls back \
                     to a unit cube per solid block"
                );
            }
            adapter
        })
        .clone()
}

impl LiveCollision {
    /// Build a view from a pre-fetched section snapshot and the dimension geometry.
    ///
    /// Version data defaults to [`default_version_data`]; override it with
    /// [`with_version_data`](Self::with_version_data).
    #[must_use]
    pub fn new(
        sections: HashMap<(i32, i32, usize), Arc<ChunkSection>>,
        min_y: i32,
        section_count: usize,
        atlas: Arc<BlockAtlas>,
    ) -> Self {
        // Three `state_id_of` lookups (a hash of a `(name, properties)` key each)
        // per snapshot. The snapshot itself already clones ~200 `Arc<ChunkSection>`
        // handles, so this is noise; resolving by *name* rather than hardcoding ids
        // is what keeps the list checkable against the jar.
        //
        // `0` is seeded unconditionally: it is `minecraft:air`, and it is also what
        // `block_at` returns for a cell outside the snapshot, so a missing name
        // index must never make unloaded space targetable.
        let mut air_states = vec![0u32];
        for name in AIR_BLOCKS {
            if let Some(id) = atlas.state_id_of(name)
                && !air_states.contains(&id)
            {
                air_states.push(id);
            }
        }
        Self {
            sections,
            min_y,
            section_count,
            atlas,
            version: default_version_data(),
            air_states,
        }
    }

    /// Use `version` as the source of per-state collision shapes and block names.
    ///
    /// This is the explicit form of what [`new`](Self::new) infers: pass the
    /// adapter for the protocol actually connected. Cheap — the adapter is shared
    /// by `Arc` and every lookup through it returns `&'static` rodata.
    #[must_use]
    pub fn with_version_data(mut self, version: Option<Arc<dyn VersionAdapter>>) -> Self {
        self.version = version;
        self
    }

    /// Whether this view has real per-state collision geometry, or is degraded to
    /// a unit cube per occluding block. For the debug overlay — a player standing
    /// half a block above a slab should be able to see *why* from inside the game.
    #[must_use]
    pub fn has_real_shapes(&self) -> bool {
        self.version.is_some()
    }

    /// Vanilla block-state id at world coordinates, or `0` (`minecraft:air`)
    /// outside the snapshot / world.
    #[must_use]
    pub fn block_at(&self, x: i32, y: i32, z: i32) -> u32 {
        if y < self.min_y || y >= self.min_y + (self.section_count as i32) * 16 {
            return 0;
        }
        let si = ((y - self.min_y) / 16) as usize;
        let key = (x.div_euclid(16), z.div_euclid(16), si);
        let Some(section) = self.sections.get(&key) else {
            return 0;
        };
        let ly = (y - self.min_y).rem_euclid(16) as usize;
        section.get_block(x.rem_euclid(16) as usize, ly, z.rem_euclid(16) as usize)
    }

    /// Whether the block at these coordinates is a full six-faced opaque cube per
    /// the vanilla classifier — i.e. whether it **occludes**.
    ///
    /// # This is no longer the collision answer, and it never should have been
    ///
    /// Occlusion and collision are different questions that agree on plain cubes
    /// and disagree on plenty else: a slab collides and does not occlude; soul
    /// sand occludes and collides only to `y = 0.875`; a barrier collides and
    /// occludes nothing. Using this *as* the collision shape is what removed every
    /// partial block from the live world. Collision now comes from
    /// [`VersionAdapter::block_collision`]; this remains only as the occlusion
    /// predicate (and as the fallback shape when no version data is available).
    #[must_use]
    pub fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        self.atlas.classify(self.block_at(x, y, z), 0, 0).occludes
    }

    /// The fluid occupying these coordinates, from the *same* classification the
    /// mesher draws the water surface with ([`vanilla_fluid`]). This is the one
    /// place the live view answers "is there fluid here"; both
    /// [`is_water`](CollisionView::is_water) and [`is_lava`](CollisionView::is_lava)
    /// are thin reads of it, so they cannot disagree about a waterlogged block.
    #[must_use]
    fn fluid_kind(&self, x: i32, y: i32, z: i32) -> Option<FluidKind> {
        vanilla_fluid(&self.atlas, self.block_at(x, y, z))
    }

    /// The block-local **outline** boxes for a state — vanilla
    /// `BlockStateBase.getShape(...).toAabbs()`, via
    /// [`VersionAdapter::block_outline`] — or a degraded proxy when no version
    /// census is available for this state.
    ///
    /// This is the shape [`is_pickable`](Self::is_pickable) tests for
    /// emptiness, and it is also what a selection box should be drawn from:
    /// unlike [`shape_of`](BlockView::shape_of) (collision), the outline is
    /// what vanilla's crosshair and selection box actually use
    /// (`ClipContext.Block.OUTLINE`).
    ///
    /// # Two fallback tiers, and why neither guesses a cube from nothing
    ///
    /// 1. **Real answer**: `self.version`'s `block_outline(state)` returns
    ///    `Some(_)` — the common case for every state a live 26.2 session can
    ///    produce. An empty slice here is a real, meaningful answer (open
    ///    water, lava, air, `minecraft:light` — see below), not a data gap.
    /// 2. **Degraded**: no version data at all, or a state id the census does
    ///    not recognise (an out-of-range or corrupt palette entry, not a data
    ///    gap). This reduces to the *pre-census* proxy this method replaces:
    ///    baked model quads if the block has real geometry, else "no fluid in
    ///    this cell". It exists so a build with no version family compiled in
    ///    degrades the same way [`shape_of`](BlockView::shape_of) does —
    ///    coarsely, loudly (see [`has_real_shapes`](Self::has_real_shapes)),
    ///    but not by refusing to target anything at all.
    ///
    /// # `minecraft:light`: a deliberate behaviour change
    ///
    /// The pre-census proxy's second clause ("no fluid ⇒ pickable") kept
    /// `light` targetable as a side effect of having no baked model geometry.
    /// The real census says `LightBlock.getShape` is
    /// `isHoldingItem(Items.LIGHT) ? block() : empty()`
    /// (`LightBlock.java:66-68`), dumped with no item held — so light is now
    /// **un**pickable, matching what vanilla does for every player who is not
    /// holding a light item. This is the correct default-case answer, and
    /// implementing the held-item exception would need the held stack
    /// threaded down from `Sim::update_target` (out of this module's reach —
    /// `sim.rs` is not in scope for this change); it is not implemented here.
    /// `minecraft:barrier` is unaffected: its outline is a real,
    /// context-free unit cube (`BarrierBlock` sets no shape override), so it
    /// stays targetable exactly as before. Bake failures (a model that failed
    /// to compile geometry) are also unaffected in the live case: the census
    /// answers from the block's *state*, not its baked quads, so a failed
    /// bake no longer has any bearing on pickability at all — it only still
    /// matters in the degraded fallback tier above.
    #[must_use]
    fn outline_of(&self, state: u32) -> &'static [BlockAabb] {
        if let Some(version) = &self.version
            && let Some(shape) = version.block_outline(state)
        {
            return shape;
        }
        // Degraded fallback: exactly the proxy `is_pickable` used before the
        // outline census existed. Reachable only with no version data, or an
        // unrecognised state id — never as a substitute for a real outline
        // that happens to be empty (open water, air, light).
        if let Some(models) = self.atlas.models()
            && !models.quads(state).is_empty()
        {
            return FULL_CUBE;
        }
        if vanilla_fluid(&self.atlas, state).is_none() {
            FULL_CUBE
        } else {
            NO_COLLISION
        }
    }

    /// Public read of [`outline_of`](Self::outline_of) at world coordinates —
    /// block-local boxes translated to world space, for drawing the real
    /// selection box shape (a half-height box on a slab, kelp's 9/16 column,
    /// …) instead of a unit cube. Not yet consumed: the selection-box
    /// wireframe is `gpu.rs`'s `OutlineRenderer::prepare`, which is out of
    /// this module's file scope and still builds a hard-coded unit cube
    /// around [`Sim::target`](crate::sim::Sim)'s block — see
    /// `docs/block-outline-shapes.md` for the spec to wire this in.
    #[must_use]
    pub fn outline_boxes_at(&self, x: i32, y: i32, z: i32) -> Vec<Aabb> {
        let state = self.block_at(x, y, z);
        let mut out = Vec::new();
        emit_world_boxes(self.outline_of(state), x, y, z, &mut out);
        out
    }

    /// Whether the view ray can **target** this cell — the client-side stand-in for
    /// vanilla's `clip(ClipContext.Block.OUTLINE, ClipContext.Fluid.NONE)`.
    ///
    /// # This is a different question from `is_water`, and conflating them is the bug
    ///
    /// Picking is *not* collision (a cross-plant has an empty collision shape and is
    /// still breakable), and it is *not* "the cell has no fluid" either. Vanilla
    /// walks the **outline** shape (`BlockStateBase.getShape`) with fluid shapes
    /// switched off (`Fluid.NONE`, `Entity.pick`, `Entity.java:2012-2017`). So the
    /// real question is *does the block in this cell have a non-empty outline*:
    ///
    /// * `LiquidBlock.getShape` → `Shapes.empty()` (`LiquidBlock.java:145-147`), so
    ///   open water and lava are never targeted;
    /// * `KelpBlock`'s is `Block.column(16, 0, 9)` (`KelpBlock.java:24`) and
    ///   `SeagrassBlock`'s is `Block.column(12, 0, 12)` (`SeagrassBlock.java:29`) —
    ///   **non-empty**, so kelp and seagrass are targeted and breakable, even though
    ///   both hardcode `getFluidState` → `Fluids.WATER`.
    ///
    /// The pick used to be `is_solid(cell) || (state != AIR && !is_water && !is_lava)`.
    /// That worked only for as long as `is_water` meant literally
    /// `minecraft:water`. Fixing `is_water` to mean *"this cell has a fluid"* — the
    /// correct answer for buoyancy, fog and the overlay, and the whole point of the
    /// one-classifier rule in this module's docs — silently made `!is_water` false
    /// for **kelp, seagrass, tall seagrass, bubble columns and every
    /// `waterlogged=true` stair/slab/fence**. All of them are non-occluding, so
    /// `is_solid` was false too: the ray passed straight through and `Sim::target`
    /// stayed `None`, which makes `drive_mining` abort before it ever sends a
    /// `START_DESTROY`. The dig was not slow or refused — the block was never
    /// *selected*. One classifier, but **two** questions.
    ///
    /// # What this tests now: the real outline census, not a proxy
    ///
    /// This used to fall back to "has baked model quads, or has no fluid" — a
    /// structurally sound proxy (fluids are the one thing vanilla does not draw
    /// through the model pipeline) but a proxy, because outline and interaction
    /// shapes had no census of their own yet. They do now
    /// (`VersionAdapter::block_outline`, `docs/block-outline-shapes.md`), so this
    /// asks the real question directly: [`outline_of`](Self::outline_of) is
    /// non-empty. **`block_collision` must never be substituted here** — kelp's
    /// collision shape is empty while its outline is not, and using collision
    /// would re-break kelp breaking, the exact bug this predicate exists to fix.
    ///
    /// A cell is pickable when it is:
    /// 1. not one of the three air blocks ([`AIR_BLOCKS`]); **and**
    /// 2. its outline shape ([`outline_of`](Self::outline_of)) is non-empty.
    ///
    /// [`is_solid`](Self::is_solid) no longer implies this in general — occlusion
    /// is collision-adjacent geometry, outline is a third, independent shape (see
    /// the module docs) — so the two are tested independently now.
    ///
    /// Still coarse in one respect: [`raycast`](crate::raycast::raycast) itself
    /// still steps cell-by-cell and reports whichever cell the DDA enters first,
    /// so a ray that grazes the *empty* corner of a genuinely partial outline (a
    /// slab's top half, kelp's 9/16 column) can still register a hit at that
    /// cell. The selection box now drawn from
    /// [`outline_boxes_at`](Self::outline_boxes_at) is the real shape; the ray's
    /// own geometry-aware clipping is unchanged by this fix.
    #[must_use]
    pub fn is_pickable(&self, x: i32, y: i32, z: i32) -> bool {
        let state = self.block_at(x, y, z);
        if self.air_states.contains(&state) {
            return false;
        }
        !self.outline_of(state).is_empty()
    }
}

impl BlockView for LiveCollision {
    fn state_at(&self, x: i32, y: i32, z: i32) -> u32 {
        self.block_at(x, y, z)
    }

    /// One `&'static` slice out of the version crate's rodata: a bounds-checked
    /// index into `STATE_SHAPE: [u16; 32366]` and one more into
    /// `SHAPES: [&[Aabb]; 326]`, behind a single virtual call. No allocation, no
    /// scan, no per-state cache to invalidate.
    ///
    /// Falls back to occlusion-implies-cube in two cases, which are not the same
    /// thing: no version data at all (logged once — see the type docs), or a state
    /// id the census does not know, which means a corrupt or out-of-range palette
    /// entry rather than a data gap.
    fn shape_of(&self, state: u32) -> &'static [BlockAabb] {
        if let Some(version) = &self.version
            && let Some(shape) = version.block_collision(state)
        {
            return shape;
        }
        if self.atlas.classify(state, 0, 0).occludes {
            FULL_CUBE
        } else {
            NO_COLLISION
        }
    }

    fn fluid_kind_of(&self, state: u32) -> Option<FluidKind> {
        vanilla_fluid(&self.atlas, state)
    }

    /// The fluid's amount and falling flag, from the same
    /// [`BlockModels::fluid`](lodestone_render::BlockModels::fluid) call
    /// [`vanilla_fluid`] makes — read one level deeper, because `vanilla_fluid`
    /// discards the dynamic state and `fluid_at` is the consumer that needs it.
    /// Still one rule in one place; two callers of it.
    fn fluid_cell_of(&self, state: u32) -> Option<FluidCell> {
        let cell = self.atlas.models()?.fluid(state)?;
        Some(FluidCell {
            kind: match cell.kind {
                FluidKind::Water => lodestone_physics::FluidKind::Water,
                FluidKind::Lava => lodestone_physics::FluidKind::Lava,
            },
            amount: cell.state.amount,
            falling: cell.state.falling,
        })
    }

    /// The block identifier, `&'static str` from the version crate's rodata.
    fn name_of(&self, state: u32) -> Option<&'static str> {
        self.version.as_ref()?.block_name(state)
    }
}

impl CollisionView for LiveCollision {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        boxes_at(self, x, y, z, out);
    }

    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        top_at(self, x, y, z)
    }

    fn friction(&self, x: i32, y: i32, z: i32) -> f32 {
        friction_at(self, x, y, z)
    }

    fn speed_factor(&self, x: i32, y: i32, z: i32) -> f32 {
        speed_factor_at(self, x, y, z)
    }

    fn jump_factor(&self, x: i32, y: i32, z: i32) -> f32 {
        jump_factor_at(self, x, y, z)
    }

    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        is_water_at(self, x, y, z)
    }

    fn is_climbable(&self, x: i32, y: i32, z: i32) -> bool {
        climbable_at(self, x, y, z)
    }

    fn is_lava(&self, x: i32, y: i32, z: i32) -> bool {
        is_lava_at(self, x, y, z)
    }

    fn stuck_multiplier(&self, x: i32, y: i32, z: i32) -> Option<Vec3d> {
        stuck_at(self, x, y, z)
    }

    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        fluid_at(self, x, y, z)
    }

    fn blocks_motion(&self, x: i32, y: i32, z: i32) -> bool {
        blocks_motion_at(self, x, y, z)
    }

    fn is_solid_face(&self, x: i32, y: i32, z: i32, dir: HorizontalDir) -> bool {
        is_solid_face_at(self, x, y, z, dir)
    }

    fn bounce_restitution(&self, x: i32, y: i32, z: i32) -> f32 {
        bounce_at(self, x, y, z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_physics::{
        MovementInput, PhysicsProfile, PlayerState, Vec3d, compute_fluid_state, tick,
    };
    use lodestone_world::PaletteKind;

    #[test]
    fn solid_ground_reports_a_box() {
        let world = crate::worldgen::generate(0);
        let view = WorldCollision::new(&world);
        let s = crate::worldgen::surface_height(0, 0);
        let mut boxes = Vec::new();
        view.collision_boxes(0, s, 0, &mut boxes);
        assert_eq!(boxes.len(), 1, "surface block is a collider");
        assert!(boxes[0].max_y - boxes[0].min_y == 1.0);
    }

    #[test]
    fn air_above_surface_is_empty() {
        let world = crate::worldgen::generate(0);
        let view = WorldCollision::new(&world);
        let s = crate::worldgen::surface_height(0, 0);
        let mut boxes = Vec::new();
        view.collision_boxes(0, s + 5, 0, &mut boxes);
        assert!(boxes.is_empty());
    }

    #[test]
    fn player_falls_and_lands_on_the_floor() {
        let world = crate::worldgen::generate(0);
        let view = WorldCollision::new(&world);
        let profile = PhysicsProfile::mc_1_21();
        let s = crate::worldgen::surface_height(0, 0);

        // Drop the player 6 blocks above the surface, feet-centred.
        let start = Vec3d::new(0.5, f64::from(s) + 7.0, 0.5);
        let mut state = PlayerState::at(start, 0.0);
        let input = MovementInput {
            forward: 0.0,
            strafe: 0.0,
            jump: false,
            sneak: false,
            sprint: false,
        };

        for _ in 0..80 {
            tick(&mut state, input, &view, &profile);
        }

        assert!(state.on_ground, "player should have landed");
        // Feet should rest on top of the surface block (y = s + 1).
        let expected = f64::from(s) + 1.0;
        assert!(
            (state.position.y - expected).abs() < 0.05,
            "feet at {} expected ~{expected}",
            state.position.y
        );
    }

    /// The shape helpers are pure functions of a box list, so they can be pinned
    /// without any world, atlas or version data — and the expected numbers are
    /// vanilla's own (`Blocks.java` / `BlockBehaviour.calculateSolid`), not this
    /// module's output.
    #[test]
    fn shape_helpers_match_vanilla_on_hand_written_shapes() {
        let slab = [BlockAabb {
            min: [0.0, 0.0, 0.0],
            max: [1.0, 0.5, 1.0],
        }];
        let fence_post = [BlockAabb {
            min: [0.375, 0.0, 0.375],
            max: [0.625, 1.5, 0.625],
        }];
        let ladder = [BlockAabb {
            min: [0.0, 0.0, 0.0],
            max: [1.0, 1.0, 0.1875],
        }];

        assert_eq!(shape_top(&slab), 0.5, "a bottom slab's top is 8/16");
        assert_eq!(shape_top(FULL_CUBE), 1.0);
        assert_eq!(
            shape_top(&fence_post),
            1.5,
            "a fence is 1.5 tall and must NOT be capped to 1.0, or a 0.6 auto-step \
             would mount it"
        );
        assert_eq!(shape_top(NO_COLLISION), 0.0, "no collision, no top");

        // `calculateSolid`: mean size >= 0.7291666… or Y size >= 1.0.
        assert!(shape_is_solid(FULL_CUBE));
        assert!(!shape_is_solid(NO_COLLISION));
        assert!(
            shape_is_solid(&slab),
            "a slab's Y size is 0.5 but its mean is (1+0.5+1)/3 = 0.833 >= 0.729"
        );
        assert!(
            shape_is_solid(&fence_post),
            "a fence post is thin but 1.5 tall, so the Y branch carries it"
        );
        assert!(
            shape_is_solid(&ladder),
            "a ladder's mean is exactly (1+1+0.1875)/3 = 0.7291666…, i.e. ON the \
             threshold — which is precisely why vanilla needs forceSolidOff for it"
        );

        // A full cube fills every face; a 3/16-deep ladder plate fills only the
        // face it is pressed against.
        for dir in HorizontalDir::ALL {
            assert!(shape_face_is_full(FULL_CUBE, dir), "cube fills {dir:?}");
            assert!(!shape_face_is_full(NO_COLLISION, dir));
            assert!(
                !shape_face_is_full(&fence_post, dir),
                "a 4/16-wide post fills no face ({dir:?})"
            );
        }
        assert!(
            shape_face_is_full(&ladder, HorizontalDir::North),
            "the ladder plate spans x and y at z = 0, so the north face is full"
        );
        assert!(
            !shape_face_is_full(&ladder, HorizontalDir::South),
            "…and the south face is 13/16 away from the plate"
        );
    }

    /// The name-keyed tables, against the values read out of the decompiled 26.2
    /// jar with line numbers (see each function's docs). The **controls** are the
    /// `_` arms: a block that sets none of these must come back with vanilla's
    /// default, or a table that returned "slippery" for everything would satisfy
    /// every positive assertion here.
    #[test]
    fn name_keyed_constants_match_the_decompiled_values() {
        assert_eq!(friction_for("minecraft:ice"), 0.98);
        assert_eq!(friction_for("minecraft:packed_ice"), 0.98);
        assert_eq!(friction_for("minecraft:frosted_ice"), 0.98);
        assert_eq!(friction_for("minecraft:blue_ice"), 0.989);
        assert_eq!(friction_for("minecraft:slime_block"), 0.8);
        assert_eq!(friction_for("minecraft:stone"), 0.6, "control: default");
        assert_eq!(friction_for("minecraft:ice_bricks_that_do_not_exist"), 0.6);

        assert_eq!(speed_factor_for("minecraft:soul_sand"), 0.4);
        assert_eq!(speed_factor_for("minecraft:honey_block"), 0.4);
        assert_eq!(speed_factor_for("minecraft:sand"), 1.0, "control: default");

        assert_eq!(jump_factor_for("minecraft:honey_block"), 0.5);
        assert_eq!(
            jump_factor_for("minecraft:soul_sand"),
            1.0,
            "control: soul sand slows you but does not shorten your jump"
        );

        assert_eq!(bounce_for("minecraft:slime_block"), 1.0);
        assert_eq!(bounce_for("minecraft:white_bed"), 0.75);
        assert_eq!(bounce_for("minecraft:black_bed"), 0.75);
        assert_eq!(bounce_for("minecraft:stone"), 0.0, "control: default");
        assert_eq!(
            bounce_for("minecraft:honey_block"),
            0.0,
            "control: the only SUPPRESSES_BOUNCE member sets no restitution anyway"
        );

        assert_eq!(
            stuck_for("minecraft:cobweb"),
            Some(Vec3d::new(0.25, 0.05, 0.25))
        );
        assert_eq!(
            stuck_for("minecraft:powder_snow"),
            Some(Vec3d::new(0.9, 1.5, 0.9))
        );
        assert_eq!(
            stuck_for("minecraft:sweet_berry_bush"),
            Some(Vec3d::new(0.8, 0.75, 0.8))
        );
        assert_eq!(stuck_for("minecraft:stone"), None, "control: default");

        for name in [
            "minecraft:ladder",
            "minecraft:vine",
            "minecraft:scaffolding",
            "minecraft:weeping_vines",
            "minecraft:weeping_vines_plant",
            "minecraft:twisting_vines",
            "minecraft:twisting_vines_plant",
            "minecraft:cave_vines",
            "minecraft:cave_vines_plant",
        ] {
            assert!(is_climbable_name(name), "{name} is in BlockTags.CLIMBABLE");
        }
        // Controls: near-misses that are *not* in the tag.
        for name in [
            "minecraft:sugar_cane",
            "minecraft:glow_lichen",
            "minecraft:chain",
            "minecraft:stone",
        ] {
            assert!(
                !is_climbable_name(name),
                "{name} is NOT in BlockTags.CLIMBABLE"
            );
        }
    }

    /// The vanilla atlas (with baked models attached), or a loud failure naming
    /// the fix. Every test below needs the real state ids — a waterlogged stair
    /// or a kelp plant only exists in the vanilla id space.
    fn vanilla_atlas() -> Arc<BlockAtlas> {
        let resources = crate::resources::BlockResources::load(true);
        resources.vanilla_atlas.expect(
            "vanilla assets did not load; set LODESTONE_ASSETS to a pack root with \
             client.jar + generated/reports/blocks.json",
        )
    }

    /// Resolve a full vanilla block-state string, failing loudly (rather than
    /// silently testing air) when the name or its property set is wrong.
    fn state_id(atlas: &BlockAtlas, name: &str) -> u32 {
        atlas
            .state_id_of(name)
            .unwrap_or_else(|| panic!("no such block state: {name}"))
    }

    /// A one-section live view (chunk `0,0`, `min_y = 0`) whose cells at
    /// `y_range` hold `state` and whose remaining cells are air.
    fn live_column(
        atlas: Arc<BlockAtlas>,
        state: u32,
        y_range: std::ops::RangeInclusive<usize>,
    ) -> LiveCollision {
        // 20 direct bits, comfortably above the ~28k vanilla state ids, so no id
        // in this fixture can be truncated by the container's packing.
        let mut section = ChunkSection::new(
            PaletteKind::block_states_with_direct_bits(20),
            PaletteKind::biomes(),
            0,
            0,
        );
        for y in y_range {
            for x in 0..16 {
                for z in 0..16 {
                    section.set_block(x, y, z, state);
                }
            }
        }
        let mut sections = HashMap::new();
        sections.insert((0, 0, 0), Arc::new(section));
        LiveCollision::new(sections, 0, 1, atlas)
    }

    /// The block-local boxes this view resolves for a single cell holding
    /// `state`, rounded back into block-local space so they can be compared with
    /// vanilla's own numbers.
    fn local_boxes(atlas: &Arc<BlockAtlas>, state: u32) -> Vec<[f64; 6]> {
        let view = live_column(Arc::clone(atlas), state, 4..=4);
        let mut out = Vec::new();
        view.collision_boxes(0, 4, 0, &mut out);
        out.iter()
            .map(|b| {
                [
                    b.min_x,
                    b.min_y - 4.0,
                    b.min_z,
                    b.max_x,
                    b.max_y - 4.0,
                    b.max_z,
                ]
            })
            .collect()
    }

    /// **The routing gate.** The generated collision census must actually reach
    /// [`CollisionView::collision_boxes`] — for nine months it did not, and every
    /// solid block in live play was a unit cube.
    ///
    /// # Where the expected values come from
    ///
    /// Not from this module. Each one is vanilla's own constructor call, read out
    /// of the decompiled 26.2 jar and converted from sixteenths by hand:
    /// `SlabBlock.SHAPE_BOTTOM = Block.column(16, 0, 8)` and `SHAPE_TOP =
    /// Block.column(16, 8, 16)`; `SoulSandBlock.SHAPE = Block.column(16, 0, 14)`;
    /// `FenceBlock` passes `collisionHeight = 24` with `postWidth = 4` to
    /// `CrossCollisionBlock`, whose post is `Block.column(4, 0, 24)`; `COBWEB` is
    /// registered `.noCollision()`. `Block.column(w, lo, hi)` centres `w`, so
    /// `column(4, …)` spans `6/16..10/16`.
    ///
    /// # The controls
    ///
    /// Three of them, because "the boxes are right" is easy to satisfy vacuously:
    ///
    /// 1. **bottom vs top slab** — the same block, the same `8/16`, at opposite
    ///    ends of the cell. A resolver that returned a hard-coded slab shape, or
    ///    ignored the `type` property, passes one and fails the other.
    /// 2. **empty is not a cube** — cobweb, water and kelp must resolve to *no*
    ///    boxes. An adapter that cubed everything non-air passes every positive
    ///    assertion above and fails these.
    /// 3. **the degraded view** — the same states through
    ///    [`LiveCollision::with_version_data(None)`](LiveCollision::with_version_data),
    ///    which is exactly what this module did before the fix, must produce the
    ///    *wrong* answer. This is the proof the detector fires: without it, every
    ///    assertion here could be satisfied by a census that happened to be
    ///    reachable for some unrelated reason.
    #[test]
    #[ignore = "requires the vanilla pack AND --features live (the version collision census)"]
    fn the_real_per_state_collision_census_reaches_the_collision_view() {
        let atlas = vanilla_atlas();

        // Precondition, loud rather than skipped: with no version data every
        // assertion below would be testing the fallback, not the census.
        let probe = live_column(Arc::clone(&atlas), 0, 0..=0);
        assert!(
            probe.has_real_shapes(),
            "no version collision census is wired in — run this gate with \
             --features live, or LiveCollision has lost its shape source"
        );

        let cases: &[(&str, &[[f64; 6]])] = &[
            ("minecraft:stone", &[[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]]),
            (
                "minecraft:oak_slab[type=bottom,waterlogged=false]",
                &[[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]],
            ),
            (
                "minecraft:oak_slab[type=top,waterlogged=false]",
                &[[0.0, 0.5, 0.0, 1.0, 1.0, 1.0]],
            ),
            (
                "minecraft:oak_slab[type=double,waterlogged=false]",
                &[[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]],
            ),
            ("minecraft:soul_sand", &[[0.0, 0.0, 0.0, 1.0, 0.875, 1.0]]),
            (
                "minecraft:oak_fence[east=false,north=false,south=false,waterlogged=false,west=false]",
                &[[0.375, 0.0, 0.375, 0.625, 1.5, 0.625]],
            ),
            // Control 2: an empty shape is a real answer, not a missing one.
            ("minecraft:cobweb", &[]),
            ("minecraft:water[level=0]", &[]),
            ("minecraft:kelp_plant", &[]),
        ];

        for (name, expected) in cases {
            let id = state_id(&atlas, name);
            let got = local_boxes(&atlas, id);
            assert_eq!(
                got.len(),
                expected.len(),
                "{name}: expected {} box(es), got {got:?}",
                expected.len()
            );
            for (g, e) in got.iter().zip(expected.iter()) {
                for a in 0..6 {
                    assert!(
                        (g[a] - e[a]).abs() < 1e-6,
                        "{name}: component {a} is {} , vanilla says {} (whole box \
                         {g:?} vs {e:?})",
                        g[a],
                        e[a]
                    );
                }
            }
        }

        // Control 1, stated as the property rather than the numbers: the two slab
        // halves must not resolve to the same thing.
        let bottom = local_boxes(
            &atlas,
            state_id(&atlas, "minecraft:oak_slab[type=bottom,waterlogged=false]"),
        );
        let top = local_boxes(
            &atlas,
            state_id(&atlas, "minecraft:oak_slab[type=top,waterlogged=false]"),
        );
        assert_ne!(
            bottom, top,
            "a bottom and a top slab must have different collision boxes"
        );

        // …and the uncapped-top contract, which a pathfinder's step-up check reads.
        let fence = state_id(
            &atlas,
            "minecraft:oak_fence[east=false,north=false,south=false,waterlogged=false,west=false]",
        );
        let fence_view = live_column(Arc::clone(&atlas), fence, 4..=4);
        assert!(
            (fence_view.collision_top(0, 4, 0) - 1.5).abs() < 1e-6,
            "a fence's collision_top must be 1.5, never clamped to 1.0"
        );

        // Control 3: the pre-fix behaviour, on the same states, must be wrong.
        let slab = state_id(&atlas, "minecraft:oak_slab[type=bottom,waterlogged=false]");
        let degraded = live_column(Arc::clone(&atlas), slab, 4..=4).with_version_data(None);
        assert!(
            (degraded.collision_top(0, 4, 0) - 1.0).abs() < 1e-6,
            "control did not fire: without the census a bottom slab must read as a \
             full cube (that IS the bug), got {}",
            degraded.collision_top(0, 4, 0)
        );
        let cobweb = state_id(&atlas, "minecraft:cobweb");
        let real = live_column(Arc::clone(&atlas), cobweb, 4..=4);
        assert_eq!(
            real.collision_top(0, 4, 0),
            0.0,
            "cobweb has no collision at all"
        );
    }

    /// The name-keyed constants must reach the view through the *version* seam,
    /// not just exist as a table. Ice is the case a player feels immediately.
    ///
    /// Controls: stone (friction default) and the fact that ice's *shape* is a
    /// full cube — a view that resolved ice to nothing would report the default
    /// friction for the air above it and look identical.
    #[test]
    #[ignore = "requires the vanilla pack AND --features live (the version block-name census)"]
    fn name_keyed_constants_reach_the_view_through_the_version_seam() {
        let atlas = vanilla_atlas();
        let probe = live_column(Arc::clone(&atlas), 0, 0..=0);
        assert!(
            probe.has_real_shapes(),
            "no version data wired in — run with --features live"
        );

        let ice = live_column(Arc::clone(&atlas), state_id(&atlas, "minecraft:ice"), 4..=4);
        assert_eq!(ice.friction(0, 4, 0), 0.98, "ice is slippery");
        assert_eq!(ice.collision_top(0, 4, 0), 1.0, "…and is still a full cube");

        let stone = live_column(
            Arc::clone(&atlas),
            state_id(&atlas, "minecraft:stone"),
            4..=4,
        );
        assert_eq!(stone.friction(0, 4, 0), 0.6, "control: default friction");

        let soul_sand = live_column(
            Arc::clone(&atlas),
            state_id(&atlas, "minecraft:soul_sand"),
            4..=4,
        );
        assert_eq!(soul_sand.speed_factor(0, 4, 0), 0.4);
        assert_eq!(
            soul_sand.jump_factor(0, 4, 0),
            1.0,
            "control: soul sand slows but does not shorten a jump"
        );

        let honey = live_column(
            Arc::clone(&atlas),
            state_id(&atlas, "minecraft:honey_block"),
            4..=4,
        );
        assert_eq!(honey.speed_factor(0, 4, 0), 0.4);
        assert_eq!(honey.jump_factor(0, 4, 0), 0.5);

        let slime = live_column(
            Arc::clone(&atlas),
            state_id(&atlas, "minecraft:slime_block"),
            4..=4,
        );
        assert_eq!(slime.bounce_restitution(0, 4, 0), 1.0);
        assert_eq!(slime.friction(0, 4, 0), 0.8);
        assert_eq!(
            stone.bounce_restitution(0, 4, 0),
            0.0,
            "control: stone does not bounce"
        );

        let cobweb = live_column(
            Arc::clone(&atlas),
            state_id(&atlas, "minecraft:cobweb"),
            4..=4,
        );
        assert_eq!(
            cobweb.stuck_multiplier(0, 4, 0),
            Some(Vec3d::new(0.25, 0.05, 0.25))
        );
        assert_eq!(
            stone.stuck_multiplier(0, 4, 0),
            None,
            "control: stone does not grab you"
        );

        let ladder = live_column(
            Arc::clone(&atlas),
            state_id(&atlas, "minecraft:ladder[facing=north,waterlogged=false]"),
            4..=4,
        );
        assert!(ladder.is_climbable(0, 4, 0), "ladders are climbable");
        assert!(
            !stone.is_climbable(0, 4, 0),
            "control: stone is not climbable"
        );
        assert!(
            !ladder.blocks_motion(0, 4, 0),
            "a ladder is forceSolidOff in vanilla despite sitting exactly on the \
             calculateSolid threshold"
        );
        assert!(
            stone.blocks_motion(0, 4, 0),
            "control: stone does block motion"
        );

        // `fluid_at` now reports the level, not just presence: a source is amount
        // 8, and level 3 flowing water is amount 5 (`8 - level`).
        let source = live_column(
            Arc::clone(&atlas),
            state_id(&atlas, "minecraft:water[level=0]"),
            4..=4,
        );
        let flowing = live_column(
            Arc::clone(&atlas),
            state_id(&atlas, "minecraft:water[level=3]"),
            4..=4,
        );
        assert_eq!(source.fluid_at(0, 4, 0).map(|c| c.amount), Some(8));
        assert_eq!(
            flowing.fluid_at(0, 4, 0).map(|c| c.amount),
            Some(5),
            "control: a flowing cell is shallower than a source, which is what \
             makes get_flow produce a current at all"
        );
        assert_eq!(
            stone.fluid_at(0, 4, 0),
            None,
            "control: stone holds no fluid"
        );
    }

    /// The player's fluid summary standing feet-first at `feet_y` in the column,
    /// with an explicit pose eye height.
    fn fluid_state_at(
        view: &LiveCollision,
        feet_y: f64,
        eye_height: f32,
    ) -> lodestone_physics::FluidState {
        let profile = PhysicsProfile::mc_1_21();
        let position = Vec3d::new(0.5, feet_y, 0.5);
        let player = PlayerState::at(position, 0.0).with_eye_height(eye_height);
        compute_fluid_state(player.bounding_box(&profile), position, eye_height, view)
    }

    /// The defect: a **waterlogged** block and the plants that hardcode a water
    /// `getFluidState` carry a water source in vanilla, so an eye inside one is
    /// under water — swimming, submerged fog, the overlay and the ambient sounds
    /// all hang off this one answer. The mesher has always classified these
    /// correctly (`BlockModels::fluid`); physics used to match an exact water id
    /// and saw none of them.
    ///
    /// The `waterlogged=false` stair and the land plant are the **controls**: if
    /// they submerged too, the classifier would be saying "yes" to everything and
    /// the positive assertions would prove nothing.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn waterlogged_blocks_and_underwater_plants_submerge_the_eye() {
        let atlas = vanilla_atlas();

        let wet = [
            "minecraft:oak_stairs[facing=north,half=bottom,shape=straight,waterlogged=true]",
            "minecraft:oak_slab[type=bottom,waterlogged=true]",
            "minecraft:kelp_plant",
            "minecraft:kelp[age=0]",
            "minecraft:seagrass",
            "minecraft:tall_seagrass[half=lower]",
            "minecraft:water[level=0]",
        ];
        for name in wet {
            let id = state_id(&atlas, name);
            let view = live_column(Arc::clone(&atlas), id, 0..=15);
            let fs = fluid_state_at(&view, 1.0, 1.62);
            assert!(
                fs.under_water(),
                "eye inside {name} must read as under water, got {fs:?}"
            );
        }

        let dry = [
            "minecraft:oak_stairs[facing=north,half=bottom,shape=straight,waterlogged=false]",
            "minecraft:oak_slab[type=bottom,waterlogged=false]",
            "minecraft:short_grass",
            "minecraft:fern",
        ];
        for name in dry {
            let id = state_id(&atlas, name);
            let view = live_column(Arc::clone(&atlas), id, 0..=15);
            let fs = fluid_state_at(&view, 1.0, 1.62);
            assert!(
                !fs.under_water(),
                "eye inside {name} must NOT read as under water, got {fs:?}"
            );
        }
    }

    /// **The kelp bug.** Every one of these `wet` states carries a water source, so
    /// the pick predicate that asked `!is_water(cell)` refused all of them: kelp
    /// could not be targeted, so it could not be broken, and neither could a
    /// waterlogged stair or slab. None of them is a full cube either, so `is_solid`
    /// did not save it. Vanilla targets all of them, because their **outline**
    /// shapes are non-empty (`KelpBlock.java:24`, `SeagrassBlock.java:29`) and
    /// `Entity.pick` runs with `ClipContext.Fluid.NONE`.
    ///
    /// The `dry` list is not decoration, it is the control: open water and lava must
    /// stay un-targetable (else you would break the ocean instead of the sand under
    /// it) and so must all three air blocks — including `cave_air`, which is what a
    /// carved cave is full of and which any `state_id != 0` test wrongly targets.
    /// Without those five failing, "kelp is pickable" would be satisfied by a
    /// predicate that just returns `true`.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn submerged_plants_and_waterlogged_blocks_are_pickable_but_open_fluid_is_not() {
        let atlas = vanilla_atlas();

        let pickable = [
            // The reported symptom.
            "minecraft:kelp_plant",
            "minecraft:kelp[age=0]",
            "minecraft:seagrass",
            "minecraft:tall_seagrass[half=lower]",
            "minecraft:tall_seagrass[half=upper]",
            // The same defect on blocks that carry water via `waterlogged`.
            "minecraft:oak_stairs[facing=north,half=bottom,shape=straight,waterlogged=true]",
            "minecraft:oak_slab[type=bottom,waterlogged=true]",
            "minecraft:oak_fence[east=false,north=false,south=false,waterlogged=true,west=false]",
            // Already worked, kept so a regression the other way is visible: a dry
            // shapeless plant and a plain full cube.
            "minecraft:short_grass",
            "minecraft:stone",
            "minecraft:oak_slab[type=bottom,waterlogged=false]",
        ];
        for name in pickable {
            let id = state_id(&atlas, name);
            let view = live_column(Arc::clone(&atlas), id, 0..=15);
            assert!(
                view.is_pickable(0, 1, 0),
                "{name} must be targetable by the view ray"
            );
        }

        let not_pickable = [
            "minecraft:water[level=0]",
            "minecraft:water[level=3]",
            "minecraft:lava[level=0]",
            "minecraft:air",
            "minecraft:cave_air",
            "minecraft:void_air",
        ];
        for name in not_pickable {
            let id = state_id(&atlas, name);
            let view = live_column(Arc::clone(&atlas), id, 0..=15);
            assert!(
                !view.is_pickable(0, 1, 0),
                "{name} must NOT be targetable by the view ray"
            );
        }
    }

    /// **The deliberate behaviour change.** The pre-census proxy's "no fluid ⇒
    /// pickable" clause kept `minecraft:light` targetable as a side effect of it
    /// having no baked model geometry. The real outline census says
    /// `LightBlock.getShape` is `isHoldingItem(Items.LIGHT) ? block() : empty()`
    /// (`LightBlock.java:66-68`), dumped with no item held, so vanilla itself does
    /// not let you target a bare-handed light block — and now neither do we.
    ///
    /// `minecraft:barrier` is the control proving this is a real outline read and
    /// not "everything with no rendered geometry is now unpickable": barrier's
    /// shape is a context-free unit cube (`BarrierBlock` overrides no shape
    /// getter), so it must stay targetable exactly as before, even though it also
    /// has no baked model quads.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn light_is_not_pickable_without_a_held_item_but_barrier_still_is() {
        let atlas = vanilla_atlas();

        let light = state_id(&atlas, "minecraft:light[level=15,waterlogged=false]");
        let light_view = live_column(Arc::clone(&atlas), light, 0..=15);
        assert!(
            !light_view.is_pickable(0, 1, 0),
            "minecraft:light must not be targetable: its census outline is empty \
             without a held light item"
        );

        let barrier = state_id(&atlas, "minecraft:barrier[waterlogged=false]");
        let barrier_view = live_column(Arc::clone(&atlas), barrier, 0..=15);
        assert!(
            barrier_view.is_pickable(0, 1, 0),
            "control: barrier's outline is a real, context-free unit cube, so it \
             must stay targetable"
        );
    }

    /// **The visible half of this change.** Before the outline census, every
    /// pickable cell was drawn as a full unit-cube selection box; a bottom slab's
    /// box should be a half-height box like its own collision shape
    /// (`SlabBlock.java:35-36`: `SHAPE_BOTTOM = Block.column(16, 0, 8)`), not the
    /// full cell.
    ///
    /// Stone is the control: a state whose outline genuinely *is* a full unit
    /// cube must still report one, so "the outline boxes are always smaller now"
    /// would fail here even though it would pass the slab assertion alone.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn a_slabs_selection_box_is_half_height_not_a_full_cube() {
        let atlas = vanilla_atlas();

        let slab = state_id(&atlas, "minecraft:oak_slab[type=bottom,waterlogged=false]");
        let slab_view = live_column(Arc::clone(&atlas), slab, 4..=4);
        let boxes = slab_view.outline_boxes_at(0, 4, 0);
        assert_eq!(boxes.len(), 1, "a bottom slab has one outline box");
        let b = boxes[0];
        assert!(
            (b.min_y - 4.0).abs() < 1e-6 && (b.max_y - 4.5).abs() < 1e-6,
            "a bottom slab's outline top is 8/16, not the full cell: got y {}..{}",
            b.min_y - 4.0,
            b.max_y - 4.0
        );

        // Control: a genuine full cube must still report one, full height.
        let stone = state_id(&atlas, "minecraft:stone");
        let stone_view = live_column(Arc::clone(&atlas), stone, 4..=4);
        let stone_boxes = stone_view.outline_boxes_at(0, 4, 0);
        assert_eq!(stone_boxes.len(), 1, "stone has one outline box");
        let s = stone_boxes[0];
        assert!(
            (s.min_y - 4.0).abs() < 1e-6 && (s.max_y - 5.0).abs() < 1e-6,
            "control: stone's outline is a full unit cube, got y {}..{}",
            s.min_y - 4.0,
            s.max_y - 4.0
        );
    }

    /// The pick predicate is only useful if the *ray* honours it, and the ray is
    /// what `Sim::update_target` runs. Fire the real
    /// [`crate::raycast::raycast`] through a kelp cell with a stone block behind it:
    /// the near cell must win. Before the fix the ray skipped the kelp and reported
    /// the stone — a targeting bug that reads on screen as "kelp cannot be broken".
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn the_view_ray_stops_at_kelp_rather_than_the_block_behind_it() {
        let atlas = vanilla_atlas();
        let kelp = state_id(&atlas, "minecraft:kelp_plant");
        let stone = state_id(&atlas, "minecraft:stone");

        // One section: kelp at y = 4, stone at y = 2, air between. Looking straight
        // down from y = 6.5 the ray meets kelp first.
        let mut section = ChunkSection::new(
            PaletteKind::block_states_with_direct_bits(20),
            PaletteKind::biomes(),
            0,
            0,
        );
        for x in 0..16 {
            for z in 0..16 {
                section.set_block(x, 4, z, kelp);
                section.set_block(x, 2, z, stone);
            }
        }
        let mut sections = HashMap::new();
        sections.insert((0, 0, 0), Arc::new(section));
        let view = LiveCollision::new(sections, 0, 1, Arc::clone(&atlas));

        let hit = crate::raycast::raycast([0.5, 6.5, 0.5], [0.0, -1.0, 0.0], 4.0, |x, y, z| {
            view.is_pickable(x, y, z)
        })
        .expect("the ray must hit something within 4 blocks");
        assert_eq!(
            hit.block,
            [0, 4, 0],
            "the ray must stop at the kelp, not tunnel through to the stone"
        );
    }

    /// Lava is the same question with the other answer: the live view used to
    /// leave [`CollisionView::is_lava`] at its `false` default, so a live player
    /// could stand in lava and read as dry.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn lava_submerges_the_eye_and_is_not_water() {
        let atlas = vanilla_atlas();
        let id = state_id(&atlas, "minecraft:lava[level=0]");
        let view = live_column(Arc::clone(&atlas), id, 0..=15);
        let fs = fluid_state_at(&view, 1.0, 1.62);
        assert!(
            fs.under_lava(),
            "eye inside lava must read as under lava: {fs:?}"
        );
        assert!(!fs.under_water(), "lava must not read as water: {fs:?}");
    }

    /// The boundary the four consumers now share: **eye exactly at the water
    /// surface**. Vanilla's `isEyeInFluid` test is `eyeY <= fluidTop` — inclusive
    /// — so an eye resting exactly on the surface plane counts as submerged.
    /// Pinned here because fog, overlay, sounds and pose all flip on it.
    ///
    /// The column is water up to `y = 2` (top plane `y = 3.0`, the coarse
    /// full-cell height this adapter commits to). The eye height is chosen so the
    /// eye Y is *exactly* `3.0` with no float slop, and the two neighbours
    /// straddle it.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn eye_exactly_at_the_water_surface_counts_as_submerged() {
        let atlas = vanilla_atlas();
        let id = state_id(&atlas, "minecraft:water[level=0]");
        let view = live_column(Arc::clone(&atlas), id, 0..=2);

        // Eye exactly on the surface plane: 2.0 + 1.0 == 3.0, both exact in f32
        // and f64, so this is the true boundary and not a rounding artefact.
        let on = fluid_state_at(&view, 2.0, 1.0);
        assert!(
            on.under_water(),
            "an eye exactly at the water surface is submerged (vanilla's <=): {on:?}"
        );

        // A hair above the surface: dry eye, but the box is still in water, so
        // `in_water` stays true while `under_water` flips. That pair is what makes
        // this a boundary rather than an on/off.
        let above = fluid_state_at(&view, 2.0, 1.001);
        assert!(
            !above.under_water(),
            "an eye above the surface is not submerged: {above:?}"
        );
        assert!(
            above.in_water(),
            "the box is still in water even when the eye is out: {above:?}"
        );

        // A hair below: submerged.
        let below = fluid_state_at(&view, 2.0, 0.999);
        assert!(
            below.under_water(),
            "an eye below the surface is submerged: {below:?}"
        );
    }
}
