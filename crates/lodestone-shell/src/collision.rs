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

use lodestone_model::{
    BlockAabb, BlockPhysics, DEFAULT_BLOCK_PHYSICS, VersionAdapter, block_physics,
};
use lodestone_physics::{Aabb, CollisionView, FluidCell, HorizontalDir, Vec3d};
use lodestone_render::{BlockAtlas, BlockClassifier, FluidKind};
use lodestone_world::{ChunkPos, ChunkSection, World};

use crate::blocks::{demo_fluid, id, vanilla_fluid};
use crate::raycast::PickBox;

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

    /// Vanilla `BlockState.blocksMotion()` for a state, or `None` when this
    /// adapter has no census for it — in which case [`blocks_motion_at`] falls
    /// back to deriving it from the shape, which is wrong for 202 blocks. See
    /// that function's docs; this must never be synthesised from
    /// [`shape_of`](Self::shape_of) *inside* an adapter, or the fallback stops
    /// being distinguishable from a real answer.
    fn blocks_motion_of(&self, state: u32) -> Option<bool>;

    /// `BubbleColumnBlock`'s `DRAG_DOWN` property for a state, or `None` when the
    /// state is not a bubble column — which is every state but two.
    ///
    /// Keyed by state rather than by name because both bubble-column states share
    /// one name and differ only in this property; see
    /// [`VersionAdapter::block_bubble_column_drag`]. That fix.
    fn bubble_column_of(&self, state: u32) -> Option<bool>;
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
// Six of `CollisionView`'s answers — friction, speed factor, jump factor, bounce
// restitution, stuck multiplier and climbable — are `BlockBehaviour.Properties`
// fields and tag memberships rather than geometry, so no collision census can
// carry them. They are keyed by block *name*, which is why
// `VersionAdapter::block_name` exists.
//
// **They no longer live here.** They used to be six private functions in this
// module, hand-transcribed from the decompiled `Blocks.java`. Two things were
// wrong with that: nothing outside the code under test pinned the numbers (every
// other block table in this repo is dumped from the real server), and a
// third-party plugin — for which "how expensive is it to walk over this block" is
// the whole of a pathfinder's cost function — structurally could not reach a
// private item in the client shell. Both are fixed by
// `lodestone_model::block_physics`, a `pub fn` in the version-free model crate,
// anchored to a JVM dump of all 1,196 registered blocks by
// `crates/lodestone-data/tests/block_physics.rs`. See
// `docs/block-physics-constants.md`.
//
// The lookup is done once per query rather than field by field: `block_physics`
// returns the whole `BlockPhysics` by value (six words, no allocation), so
// `friction_at` and friends each pay one name match.

/// The name-keyed constants for the block at a cell, or vanilla's defaults when
/// the adapter cannot resolve a name for the state.
///
/// `DEFAULT_BLOCK_PHYSICS` is the right answer for an unresolvable name rather
/// than a fudge: it is what 1,166 of 26.2's 1,196 blocks report.
fn physics_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> BlockPhysics {
    v.name_of(v.state_at(x, y, z))
        .map_or(DEFAULT_BLOCK_PHYSICS, block_physics)
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
    physics_at(v, x, y, z).friction
}

fn speed_factor_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> f32 {
    physics_at(v, x, y, z).speed_factor
}

fn jump_factor_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> f32 {
    physics_at(v, x, y, z).jump_factor
}

fn bounce_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> f32 {
    physics_at(v, x, y, z).bounce_restitution
}

fn stuck_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> Option<Vec3d> {
    physics_at(v, x, y, z)
        .stuck_multiplier
        .map(|[x, y, z]| Vec3d::new(x, y, z))
}

fn climbable_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> bool {
    physics_at(v, x, y, z).climbable
}

/// [`CollisionView::is_scaffolding`] — `state.is(Blocks.SCAFFOLDING)`, the one
/// vanilla conjunct [`climbable_at`]'s tag membership cannot express. Named
/// directly rather than routed through [`physics_at`]'s `BlockPhysics` table:
/// it is a single-block identity check, not a `Properties`/tag fold shared
/// with anything else `physics_at` already answers.
fn is_scaffolding_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> bool {
    matches!(v.name_of(v.state_at(x, y, z)), Some("minecraft:scaffolding"))
}

/// [`CollisionView::is_powder_snow`] — `state.is(Blocks.POWDER_SNOW)`, for the
/// freezing mechanic. Named directly for the same reason as
/// [`is_scaffolding_at`]: it is a block identity, not a shared physics fold.
fn is_powder_snow_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> bool {
    matches!(v.name_of(v.state_at(x, y, z)), Some("minecraft:powder_snow"))
}

/// [`CollisionView::bubble_column`] — the `DRAG_DOWN` property of the bubble column
/// at this cell, if it is one.
///
/// Not routed through [`physics_at`] like its neighbours above: that table is keyed
/// by block *name*, and the two bubble-column states share a name. This is the one
/// physics answer in this module that a name cannot give.
fn bubble_column_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> Option<bool> {
    v.bubble_column_of(v.state_at(x, y, z))
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

/// `BlockState.blocksMotion()`, from the version crate's per-state census
/// ([`VersionAdapter::block_blocks_motion`]) when there is one, and from
/// [`shape_is_solid`] only when there is not.
///
/// # Why the census had to exist
///
/// `blocksMotion()` is `block != COBWEB && block != BAMBOO_SAPLING && isSolid()`,
/// and `isSolid()` reads the cached `legacySolid` flag
/// (`BlockBehaviour.java:541-550`) that `calculateSolid()`
/// (`BlockBehaviour.java:484-504`) computes once per state. Only the *last* of
/// that method's branches is geometry — the first three are
/// `Properties.forceSolidOn` (237 blocks in 26.2), `forceSolidOff` (8), and a
/// null shape cache for the 23 `dynamicShape()` blocks. None of the three has a
/// getter, appears in `blocks.json`, or is recoverable from a shape.
///
/// This function used to be the geometry branch plus a hard-coded ladder
/// exception, and that was **wrong for 2,618 of 32,366 states across 202
/// blocks** — measured in `crates/lodestone-data/tests/block_physics.rs`, not
/// estimated. 2,497 of those states are cells vanilla stops you in and we let you
/// walk through: every sign, hanging sign, banner, wall, pressure plate, chain,
/// lantern, lightning rod, dead coral, *open* fence gate, cake, bell, conduit,
/// amethyst cluster and turtle egg. The other 121 are the reverse (azalea,
/// flowering azalea, big dripleaf, chorus plant/flower, end rod, snow,
/// scaffolding).
///
/// The blast radius of getting it wrong is still small and still known —
/// `blocks_motion` has exactly one consumer, [`lodestone_physics::get_flow`]'s
/// empty-neighbour branch, which decides whether a fluid spills over an edge, and
/// nothing about the player's own movement reads it. This was correctness debt,
/// not a live bug; it is repaid so that the *next* consumer (a pathfinder asking
/// "can I stand here") inherits a right answer instead of a plausible one.
///
/// # The fallback is loud about being wrong
///
/// With no version data the census is unreachable and this degrades to the old
/// geometry derivation, keeping the same three name exclusions so a ladder still
/// reads correctly. That path is reached in exactly the cases
/// [`LiveCollision::has_real_shapes`] already reports (`--features live`
/// missing), and by [`WorldCollision`], whose ten-block demo palette is entirely
/// full cubes and air — the one world where the derivation is exact.
fn blocks_motion_at(v: &impl BlockView, x: i32, y: i32, z: i32) -> bool {
    let state = v.state_at(x, y, z);
    if let Some(real) = v.blocks_motion_of(state) {
        return real;
    }
    match v.name_of(state) {
        // The two explicit exclusions in `blocksMotion` itself, plus the one
        // `forceSolidOff` block a player touches every session.
        Some("minecraft:cobweb" | "minecraft:bamboo_sapling" | "minecraft:ladder") => false,
        _ => shape_is_solid(v.shape_of(state)),
    }
}

/// `FlowingFluid.isSolidFace` (`FlowingFluid.java:105-115`), horizontal case:
/// `false` if the cell holds **the same fluid as `kind`** (the fluid asking —
/// see [`CollisionView::is_solid_face`]'s doc for why that is not the cell's own
/// fluid), `false` for ice, else `isFaceSturdy(FULL)` = [`shape_face_is_full`].
///
/// One narrowing approximation remains: `isFaceSturdy` is the
/// under-approximating [`shape_face_is_full`]. The "any fluid → false"
/// shortcut this used to take is gone — a *different*-fluid neighbour (e.g. a
/// waterlogged solid block asked by a falling lava jet) now falls through to
/// the sturdy-face check instead of being forced to `false`.
///
/// Vanilla's `direction == UP -> true` branch is unreachable here: the seam is
/// typed [`HorizontalDir`], so the vertical case cannot be asked.
fn is_solid_face_at(
    v: &impl BlockView,
    x: i32,
    y: i32,
    z: i32,
    dir: HorizontalDir,
    kind: lodestone_physics::FluidKind,
) -> bool {
    let state = v.state_at(x, y, z);
    if let Some(neighbour_kind) = v.fluid_kind_of(state) {
        let neighbour_kind = match neighbour_kind {
            FluidKind::Water => lodestone_physics::FluidKind::Water,
            FluidKind::Lava => lodestone_physics::FluidKind::Lava,
        };
        if neighbour_kind == kind {
            return false;
        }
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

    /// The boxes [`crate::raycast::raycast`] clips against in this cell — the demo
    /// counterpart of [`LiveCollision::pick_boxes`].
    ///
    /// A full cube for every pickable cell, and that is **exact** rather than a
    /// fallback: every block in the demo palette is a full cube, air or water
    /// (`crate::blocks`). Derived from [`is_pickable`](Self::is_pickable) rather
    /// than restating it, so the two cannot disagree about what is targetable.
    pub fn pick_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<PickBox>) {
        if self.is_pickable(x, y, z) {
            out.push(PickBox::CUBE);
        }
    }
}

/// One census box widened into the pick ray's block-local `f64` box.
///
/// The census is `f32` and the ray is `f64`; the widening is exact, and it is the
/// only conversion between the two spaces (see [`PickBox`]'s docs on why
/// `raycast` does not name the census type).
fn pick_box(b: &BlockAabb) -> PickBox {
    PickBox {
        min: [
            f64::from(b.min[0]),
            f64::from(b.min[1]),
            f64::from(b.min[2]),
        ],
        max: [
            f64::from(b.max[0]),
            f64::from(b.max[1]),
            f64::from(b.max[2]),
        ],
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

    /// **Always `None`.** Demo-palette ids are not vanilla block-state ids, so no
    /// version census can be indexed by them. The shape derivation
    /// [`blocks_motion_at`] falls back to is *exact* for this world — every block
    /// in the palette is either a full cube or air — so unlike the live view there
    /// is nothing lost, and mapping the ten ids onto vanilla state ids purely to
    /// reach the census would be inventing a translation that has no other user.
    fn blocks_motion_of(&self, _state: u32) -> Option<bool> {
        None
    }

    /// **Always `None`.** The demo palette has no bubble column at all — it is ten
    /// full cubes, air and water (`crate::blocks`) — so there is no state here that
    /// could answer anything else. The offline world simply has no elevators.
    fn bubble_column_of(&self, _state: u32) -> Option<bool> {
        None
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

    fn is_scaffolding(&self, x: i32, y: i32, z: i32) -> bool {
        is_scaffolding_at(self, x, y, z)
    }

    fn is_lava(&self, x: i32, y: i32, z: i32) -> bool {
        is_lava_at(self, x, y, z)
    }

    fn stuck_multiplier(&self, x: i32, y: i32, z: i32) -> Option<Vec3d> {
        stuck_at(self, x, y, z)
    }

    fn is_powder_snow(&self, x: i32, y: i32, z: i32) -> bool {
        is_powder_snow_at(self, x, y, z)
    }

    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        fluid_at(self, x, y, z)
    }

    fn blocks_motion(&self, x: i32, y: i32, z: i32) -> bool {
        blocks_motion_at(self, x, y, z)
    }

    fn is_solid_face(
        &self,
        x: i32,
        y: i32,
        z: i32,
        dir: HorizontalDir,
        kind: lodestone_physics::FluidKind,
    ) -> bool {
        is_solid_face_at(self, x, y, z, dir, kind)
    }

    fn bounce_restitution(&self, x: i32, y: i32, z: i32) -> f32 {
        bounce_at(self, x, y, z)
    }

    fn bubble_column(&self, x: i32, y: i32, z: i32) -> Option<bool> {
        bubble_column_at(self, x, y, z)
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
    /// Dense grid of owned block sections, one entry per `(column, section)`
    /// slot in the observed footprint of the snapshot handed to [`Self::new`].
    ///
    /// This used to be a `HashMap<(i32, i32, usize), Arc<ChunkSection>>`, so
    /// every one of [`BlockView::state_at`]'s per-*queried-cell* lookups —
    /// which is to say every collision, friction, fluid and physics answer at
    /// every candidate block, every physics substep — hashed a 3-tuple and
    /// probed the table. `Sim::live_collision` (`sim.rs`) always snapshots
    /// exactly the 3×3 columns centred on the player (see
    /// `docs/chunk-world-resource.md`), so the key space is bounded and known
    /// at construction time, and every per-cell lookup is `origin`-relative
    /// array-index arithmetic — no hashing, no probing.
    ///
    /// The map is gone from the *build* path too, not only from the lookups:
    /// `Sim::live_collision` orders its `sections_at` request list in exactly
    /// this grid's order, so the aligned response **is** the grid and there is
    /// no intermediate table to fill and consume. See [`SectionGrid`].
    grid: Vec<Option<Arc<ChunkSection>>>,
    /// Chunk-x of `grid`'s `(0, 0, _)` slot.
    origin_cx: i32,
    /// Chunk-z of `grid`'s `(0, 0, _)` slot.
    origin_cz: i32,
    /// Columns spanned by `grid` along x — the *requested* footprint (3 from
    /// `Sim::live_collision`). A column of the request with no non-air section
    /// at all is still in the footprint and reads as air through its `None`
    /// slots, which is the same answer the bounding-box footprint gave; see
    /// [`Self::block_at`].
    width_x: i32,
    /// Columns spanned by `grid` along z. See [`width_x`](Self::width_x).
    width_z: i32,
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

/// The process-wide inferred version data, resolved once from the compiled-in
/// family set. See [`inferred_version_data`].
static DEFAULT_VERSION_DATA: OnceLock<Option<Arc<dyn VersionAdapter>>> = OnceLock::new();

/// Infers the connected protocol's version data from the compiled-in family
/// set, for callers of [`LiveCollision::new`] that have no better source.
///
/// # That fix — this used to be reached for *inside* `new`, not passed in
///
/// `LiveCollision::new` used to call this itself whenever it wasn't handed a
/// value, which made every `LiveCollision` in the process implicitly depend on
/// a `OnceLock` nobody could see at the call site — including in tests, where
/// "does this fixture have real shapes" silently depended on whether the test
/// binary happened to be built `--features live`, not on anything the test
/// itself stated. `version` is now a required constructor parameter (see
/// [`LiveCollision::new`]'s docs): the shell's one production caller
/// (`Sim::live_collision`, `sim.rs`) calls this function *explicitly* and
/// passes the result in, and a test passes whatever it wants — `None`,
/// `Some(a_real_adapter)`, or this same function — with no hidden state either
/// way.
///
/// A live session's protocol is settled by the time anything collides, but the
/// production caller has no cheaper way to name it than this inference: a live
/// connection exists at all *because* [`lodestone_registry::adapter_for_protocol`]
/// matched a compiled family (`net.rs`), so with exactly one family compiled it
/// is that one. A default build (no `live` feature) has none, and a
/// hypothetical multi-family build is ambiguous; both log and fall back to
/// `None` (which reduces the whole world to unit cubes — see the type docs).
///
/// Resolved once for the process: `adapter_for_protocol` builds a boxed adapter
/// per call, and `LiveCollision` is rebuilt every tick.
pub(crate) fn inferred_version_data() -> Option<Arc<dyn VersionAdapter>> {
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

/// The dense `(chunk column, section index)` grid [`LiveCollision`] reads, in the
/// order [`ClientHandle::sections_at`](lodestone_client::ClientHandle::sections_at)
/// already answers a request list in.
///
/// # Why this type exists at all
///
/// `LiveCollision::new` used to take a `HashMap<(i32, i32, usize), Arc<ChunkSection>>`,
/// which `Sim::live_collision` built by zipping its request list against
/// `sections_at`'s aligned response — and which `new` then immediately consumed into
/// a dense `Vec`. The map was pure overhead on a path rebuilt 100–160 times a second
/// (per frame in `update_target`, again in third person, and twice per tick), and the
/// comment excusing it argued in a circle: *"`sections` keeps its `HashMap` shape at
/// this boundary because `Sim::live_collision` already builds one"*. The request
/// order is dense and known, so ordering the request list the way the grid is
/// indexed makes the response the grid. `DESIGN.md` §12.114.
///
/// # How to change it, and the gotcha
///
/// **The order is load-bearing and is not the obvious one.** `cells` is indexed
/// `((cx - origin_cx) * width_z + (cz - origin_cz)) * section_count + si` — x-major
/// over z, *not* z-major — because that is the layout
/// [`LiveCollision::block_at`] indexes. A producer that emits its request list
/// z-major compiles, allocates the right length, and silently transposes the world
/// on any non-square footprint. [`Self::from_sparse`] is the reference build the
/// equivalence gate compares against.
#[derive(Debug)]
pub struct SectionGrid {
    cells: Vec<Option<Arc<ChunkSection>>>,
    origin_cx: i32,
    origin_cz: i32,
    width_x: i32,
    width_z: i32,
}

impl SectionGrid {
    /// Wrap the aligned response of a `sections_at` request list that was built
    /// in this grid's own order.
    ///
    /// # Panics
    /// Debug-only: panics if `cells.len()` is not
    /// `width_x * width_z * section_count`, which is the one way a caller can get
    /// the order wrong *and* be detectable.
    #[must_use]
    pub fn from_aligned(
        cells: Vec<Option<Arc<ChunkSection>>>,
        origin_cx: i32,
        origin_cz: i32,
        width_x: i32,
        width_z: i32,
        section_count: usize,
    ) -> Self {
        debug_assert_eq!(
            cells.len(),
            (width_x.max(0) * width_z.max(0)) as usize * section_count,
            "a `sections_at` response must stay aligned with its request list"
        );
        Self {
            cells,
            origin_cx,
            origin_cz,
            width_x,
            width_z,
        }
    }

    /// Build from a sparse `(cx, cz, si)` map, with the footprint taken as the
    /// bounding box of the keys actually present.
    ///
    /// This is the **old production build**, kept verbatim as the reference the
    /// equivalence gate compares [`Self::from_aligned`] against, and as the
    /// ergonomic shape for fixtures that place a section or two at arbitrary
    /// coordinates. It is `#[cfg(test)]` because production has no map to hand it:
    /// keeping it compiled in would be a second build path for a caller that does
    /// not exist.
    ///
    /// Its footprint differs from `from_aligned`'s and the two still agree on every
    /// input: a `(cx, cz)` outside the observed bounding box was never a key in the
    /// map either, so [`LiveCollision::block_at`]'s bounds check and a `None` slot
    /// give the same answer — air.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_sparse(
        sections: HashMap<(i32, i32, usize), Arc<ChunkSection>>,
        section_count: usize,
    ) -> Self {
        if sections.is_empty() {
            return Self {
                cells: Vec::new(),
                origin_cx: 0,
                origin_cz: 0,
                width_x: 0,
                width_z: 0,
            };
        }
        let mut min_cx = i32::MAX;
        let mut max_cx = i32::MIN;
        let mut min_cz = i32::MAX;
        let mut max_cz = i32::MIN;
        for &(cx, cz, _) in sections.keys() {
            min_cx = min_cx.min(cx);
            max_cx = max_cx.max(cx);
            min_cz = min_cz.min(cz);
            max_cz = max_cz.max(cz);
        }
        let width_x = max_cx - min_cx + 1;
        let width_z = max_cz - min_cz + 1;
        let mut cells = vec![None; (width_x * width_z) as usize * section_count];
        for ((cx, cz, si), section) in sections {
            let dx = cx - min_cx;
            let dz = cz - min_cz;
            cells[((dx * width_z + dz) as usize) * section_count + si] = Some(section);
        }
        Self {
            cells,
            origin_cx: min_cx,
            origin_cz: min_cz,
            width_x,
            width_z,
        }
    }
}

impl LiveCollision {
    /// Build a view from a pre-fetched section grid and the dimension geometry.
    ///
    /// `sections` arrives dense and already in [`Self::block_at`]'s index order —
    /// see [`SectionGrid`] for why that is the producer's job rather than a
    /// conversion here.
    ///
    /// `version` is a required parameter, not an inferred default:
    /// the caller states what collision geometry this view has, rather than
    /// `new` reaching for [`inferred_version_data`] on its own. The production
    /// caller (`Sim::live_collision`) passes `inferred_version_data()`
    /// explicitly; a test passes `None`, a hand-built adapter, or the same
    /// function, whichever the case under test needs — see
    /// [`inferred_version_data`]'s docs for why the implicit form was a
    /// problem. [`with_version_data`](Self::with_version_data) remains for
    /// overriding it after construction (the "degraded view" test fixtures use
    /// it that way).
    #[must_use]
    pub fn new(
        sections: SectionGrid,
        min_y: i32,
        section_count: usize,
        atlas: Arc<BlockAtlas>,
        version: Option<Arc<dyn VersionAdapter>>,
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
            grid: sections.cells,
            origin_cx: sections.origin_cx,
            origin_cz: sections.origin_cz,
            width_x: sections.width_x,
            width_z: sections.width_z,
            min_y,
            section_count,
            atlas,
            version,
            air_states,
        }
    }

    /// Override the version data [`new`](Self::new) was built with, after
    /// construction.
    ///
    /// `new` already requires `version` as a constructor parameter (issue
    /// That fix), so the only remaining use for this builder is *changing* it on an
    /// existing view — chiefly the "degraded view" test fixtures, which build
    /// a view with real data and then call `with_version_data(None)` to
    /// exercise the no-census fallback on the same states. Cheap either way —
    /// the adapter is shared by `Arc` and every lookup through it returns
    /// `&'static` rodata.
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
    ///
    /// Was one `HashMap<(i32, i32, usize), _>` probe per call; is now bounds
    /// checks plus one array index into [`Self::grid`], built once in
    /// [`Self::new`]. Every physics substep queries this for every candidate
    /// block in the player's sweep, so this is the function the dense-grid
    /// change in [`Self::new`] exists to speed up.
    #[must_use]
    pub fn block_at(&self, x: i32, y: i32, z: i32) -> u32 {
        if y < self.min_y || y >= self.min_y + (self.section_count as i32) * 16 {
            return 0;
        }
        let si = ((y - self.min_y) / 16) as usize;
        let dx = x.div_euclid(16) - self.origin_cx;
        let dz = z.div_euclid(16) - self.origin_cz;
        if dx < 0 || dx >= self.width_x || dz < 0 || dz >= self.width_z {
            return 0;
        }
        let idx = ((dx * self.width_z + dz) as usize) * self.section_count + si;
        let Some(Some(section)) = self.grid.get(idx) else {
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
    /// …) instead of a unit cube.
    ///
    /// The *renderer* does not go through here: `gpu.rs`'s
    /// `OutlineRenderer::prepare` takes its shapes from
    /// [`Sim::outline_shape_source`](crate::sim::Sim::outline_shape_source),
    /// a `'static` closure over the same `VersionAdapter::block_outline`
    /// census, because it needs no borrowed world snapshot. This accessor is
    /// the view-shaped read of the same answer, used by the shape gates below.
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
    /// This is the **cell-coarse** half of the question: whether this cell holds
    /// anything targetable at all. Whether the *ray* passes through the shape is
    /// [`pick_boxes`](Self::pick_boxes), which the ray uses; both read the same
    /// [`outline_of`](Self::outline_of), so they cannot disagree.
    #[must_use]
    pub fn is_pickable(&self, x: i32, y: i32, z: i32) -> bool {
        self.pick_outline(x, y, z).is_some_and(|s| !s.is_empty())
    }

    /// The boxes [`crate::raycast::raycast`] clips the view ray against in this
    /// cell — vanilla's `state.getShape(…).toAabbs()`, block-local, appended.
    ///
    /// # This is what the ray takes instead of a boolean
    ///
    /// The pick used to hand `raycast` an occupancy *predicate*
    /// ([`is_pickable`](Self::is_pickable)), so every pickable block was a unit
    /// cube to the hit test even after the selection box on screen was being
    /// drawn from the real census. Reported from play: leaf litter stayed
    /// highlighted with the crosshair well above it. Vanilla clips the outline
    /// boxes themselves (`ClipContext.Block.OUTLINE`, `Entity.java:2012-2016`),
    /// which is what emitting them here lets the ray do.
    ///
    /// Empty output means "nothing to target here", the correct answer for air,
    /// water, lava and `minecraft:light` — see
    /// [`outline_of`](Self::outline_of) for why that is a real answer rather
    /// than a data gap, and why there is deliberately no cube fallback for it.
    ///
    /// The **degraded** tier still works and still targets: with no version
    /// census `outline_of` hands back a full cube for anything with baked model
    /// geometry, so a build with no version family compiled in picks blocks
    /// exactly as coarsely as it did before this change — never "no target at
    /// all".
    pub fn pick_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<PickBox>) {
        if let Some(shape) = self.pick_outline(x, y, z) {
            out.extend(shape.iter().map(pick_box));
        }
    }

    /// The outline of this cell for *picking* purposes: `None` for the three air
    /// blocks, otherwise [`outline_of`](Self::outline_of).
    ///
    /// The air check is not redundant with an empty outline. In the **degraded**
    /// tier (no version census) `outline_of`'s last clause hands a full cube to
    /// anything that carries no fluid, which includes air — so without this,
    /// a version-free build would target the empty cell in front of the
    /// player's face. See [`AIR_BLOCKS`] for why `state != 0` is not the test.
    fn pick_outline(&self, x: i32, y: i32, z: i32) -> Option<&'static [BlockAabb]> {
        let state = self.block_at(x, y, z);
        if self.air_states.contains(&state) {
            return None;
        }
        Some(self.outline_of(state))
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

    /// One bit out of the version crate's `legacySolid`/`blocksMotion` bitset —
    /// the flag `calculateSolid` caches, which the collision census cannot
    /// reproduce (237 blocks force it on, 8 force it off, 23 have no shape cache
    /// at all). `None` only with no version data, or for a state id the census
    /// does not know.
    fn blocks_motion_of(&self, state: u32) -> Option<bool> {
        self.version.as_ref()?.block_blocks_motion(state)
    }

    /// The bubble column's `drag` property, read off the version crate's state
    /// table. `None` with no version data — which degrades a bubble column to the
    /// plain water it already classifies as, rather than to a wrong impulse.
    fn bubble_column_of(&self, state: u32) -> Option<bool> {
        self.version.as_ref()?.block_bubble_column_drag(state)
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

    fn is_scaffolding(&self, x: i32, y: i32, z: i32) -> bool {
        is_scaffolding_at(self, x, y, z)
    }

    fn is_lava(&self, x: i32, y: i32, z: i32) -> bool {
        is_lava_at(self, x, y, z)
    }

    fn stuck_multiplier(&self, x: i32, y: i32, z: i32) -> Option<Vec3d> {
        stuck_at(self, x, y, z)
    }

    fn is_powder_snow(&self, x: i32, y: i32, z: i32) -> bool {
        is_powder_snow_at(self, x, y, z)
    }

    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        fluid_at(self, x, y, z)
    }

    fn blocks_motion(&self, x: i32, y: i32, z: i32) -> bool {
        blocks_motion_at(self, x, y, z)
    }

    fn is_solid_face(
        &self,
        x: i32,
        y: i32,
        z: i32,
        dir: HorizontalDir,
        kind: lodestone_physics::FluidKind,
    ) -> bool {
        is_solid_face_at(self, x, y, z, dir, kind)
    }

    fn bounce_restitution(&self, x: i32, y: i32, z: i32) -> f32 {
        bounce_at(self, x, y, z)
    }

    fn bubble_column(&self, x: i32, y: i32, z: i32) -> Option<bool> {
        bubble_column_at(self, x, y, z)
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

    /// The name-keyed constants now come from `lodestone_model::block_physics`,
    /// which is anchored to a dump of all 1,196 blocks the real 26.2 server
    /// registers (`crates/lodestone-data/tests/block_physics.rs`). This test is not
    /// a second copy of that gate — it is the **shell's** contract on the table:
    /// the rows this module's `*_at` helpers depend on, plus the fallback
    /// behaviour that is this module's own (`DEFAULT_BLOCK_PHYSICS` for an
    /// unresolvable name).
    ///
    /// The **controls** are the default arms: a table that returned "slippery" for
    /// everything would satisfy every positive assertion here.
    #[test]
    fn name_keyed_constants_come_from_the_shared_model_table() {
        assert_eq!(block_physics("minecraft:ice").friction, 0.98);
        assert_eq!(block_physics("minecraft:packed_ice").friction, 0.98);
        assert_eq!(block_physics("minecraft:frosted_ice").friction, 0.98);
        assert_eq!(block_physics("minecraft:blue_ice").friction, 0.989);
        assert_eq!(block_physics("minecraft:slime_block").friction, 0.8);
        assert_eq!(
            block_physics("minecraft:stone").friction,
            0.6,
            "control: default"
        );
        assert_eq!(
            block_physics("minecraft:ice_bricks_that_do_not_exist").friction,
            0.6
        );

        assert_eq!(block_physics("minecraft:soul_sand").speed_factor, 0.4);
        assert_eq!(block_physics("minecraft:honey_block").speed_factor, 0.4);
        assert_eq!(
            block_physics("minecraft:sand").speed_factor,
            1.0,
            "control: default"
        );

        assert_eq!(block_physics("minecraft:honey_block").jump_factor, 0.5);
        assert_eq!(
            block_physics("minecraft:soul_sand").jump_factor,
            1.0,
            "control: soul sand slows you but does not shorten your jump"
        );

        assert_eq!(
            block_physics("minecraft:slime_block").bounce_restitution,
            1.0
        );
        assert_eq!(block_physics("minecraft:white_bed").bounce_restitution, 0.75);
        assert_eq!(block_physics("minecraft:black_bed").bounce_restitution, 0.75);
        assert_eq!(
            block_physics("minecraft:stone").bounce_restitution,
            0.0,
            "control: default"
        );
        assert_eq!(
            block_physics("minecraft:honey_block").bounce_restitution,
            0.0,
            "control: the only SUPPRESSES_BOUNCE member sets no restitution anyway"
        );

        assert_eq!(
            block_physics("minecraft:cobweb").stuck_multiplier,
            Some([0.25, 0.05, 0.25])
        );
        assert_eq!(
            block_physics("minecraft:powder_snow").stuck_multiplier,
            Some([0.9, 1.5, 0.9])
        );
        assert_eq!(
            block_physics("minecraft:sweet_berry_bush").stuck_multiplier,
            Some([0.8, 0.75, 0.8])
        );
        assert_eq!(
            block_physics("minecraft:stone").stuck_multiplier,
            None,
            "control: default"
        );

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
            assert!(
                block_physics(name).climbable,
                "{name} is in BlockTags.CLIMBABLE"
            );
        }
        // Controls: near-misses that are *not* in the tag.
        for name in [
            "minecraft:sugar_cane",
            "minecraft:glow_lichen",
            "minecraft:chain",
            "minecraft:stone",
        ] {
            assert!(
                !block_physics(name).climbable,
                "{name} is NOT in BlockTags.CLIMBABLE"
            );
        }

        // This module's own contract: an unresolvable name is vanilla's default,
        // *not* a panic and not some neighbouring row.
        assert_eq!(block_physics("not even an identifier"), DEFAULT_BLOCK_PHYSICS);
    }

    /// The name-keyed constants must reach [`CollisionView`] **through both
    /// adapters**, not merely exist as a shared function. `WorldCollision` is the
    /// cheap half to check (no atlas, no version data), and it is the half that
    /// would silently stub out if someone gave the two adapters separate bodies —
    /// the failure mode this module's structure exists to prevent.
    ///
    /// The demo palette maps onto real vanilla names, all of which take the
    /// default row, so the assertions are the defaults; the control is that a
    /// *name* the mapping does not cover reads as air rather than as a block.
    #[test]
    fn the_demo_view_reads_the_shared_table_rather_than_a_stub() {
        let world = crate::worldgen::generate(0);
        let view = WorldCollision::new(&world);
        let s = crate::worldgen::surface_height(0, 0);

        assert_eq!(view.friction(0, s, 0), 0.6);
        assert_eq!(view.speed_factor(0, s, 0), 1.0);
        assert_eq!(view.jump_factor(0, s, 0), 1.0);
        assert_eq!(view.bounce_restitution(0, s, 0), 0.0);
        assert_eq!(view.stuck_multiplier(0, s, 0), None);
        assert!(!view.is_climbable(0, s, 0));
        assert!(
            view.blocks_motion(0, s, 0),
            "the demo palette is full cubes, so the shape derivation is exact here"
        );
        assert!(
            !view.blocks_motion(0, s + 5, 0),
            "control: air above the surface blocks nothing"
        );

        // Every demo id must map to a name the shared table recognises as a real
        // block, or the mapping has rotted and both adapters silently default.
        for state in [id::STONE, id::DIRT, id::GRASS, id::SAND, id::WATER] {
            let name = demo_block_name(state).expect("demo id maps to a vanilla name");
            assert!(
                name.starts_with("minecraft:"),
                "demo id {state} maps to {name:?}, which is not a vanilla identifier"
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

    /// A one-section live view (chunk `0,0`, `min_y = 0`) holding exactly the
    /// listed `(x, y, z, state)` cells, everything else air — for the pick-ray
    /// gates, which need a *single* cell with a partial outline and empty space
    /// around it rather than [`live_column`]'s uniform fill.
    ///
    /// `version` is explicit so the same fixture can exercise the degraded
    /// (no-census) tier by passing `None`.
    fn live_cells(
        atlas: Arc<BlockAtlas>,
        version: Option<Arc<dyn VersionAdapter>>,
        cells: &[(usize, usize, usize, u32)],
    ) -> LiveCollision {
        let mut section = ChunkSection::new(
            PaletteKind::block_states_with_direct_bits(20),
            PaletteKind::biomes(),
            0,
            0,
        );
        for &(x, y, z, state) in cells {
            section.set_block(x, y, z, state);
        }
        let mut sections = HashMap::new();
        sections.insert((0, 0, 0), Arc::new(section));
        LiveCollision::new(SectionGrid::from_sparse(sections, 1), 0, 1, atlas, version)
    }

    /// A one-section live view (chunk `0,0`, `min_y = 0`) whose cells at
    /// `y_range` hold `state` and whose remaining cells are air.
    ///
    /// Passes [`inferred_version_data`] explicitly (`new` no longer
    /// reaches for it on its own) — the real census when the test binary is
    /// built `--features live` against a compiled family, `None` otherwise,
    /// exactly [`LiveCollision::new`]'s old implicit behaviour, now visible at
    /// the call site instead of hidden inside it.
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
        LiveCollision::new(
            SectionGrid::from_sparse(sections, 1),
            0,
            1,
            atlas,
            inferred_version_data(),
        )
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
        //
        // It asserted the *specific* wrong answer `1.0` — "a bottom slab reads as a
        // full cube" — and that premise is false and was never true here. The
        // fallback is `classify(state).occludes ? FULL_CUBE : NO_COLLISION`, and
        // `occludes` is derived from the **baked model geometry**
        // (`StateModel::face_occludes`): a bottom
        // slab's up face has no quad at the cell boundary, so it does not occlude
        // and the fallback gives it *no collision at all*, i.e. `0.0`. Which is
        // the worse of the two bugs — you fall through the slab rather than
        // standing 0.5 too high. So the control fires; it just fires the other
        // way, and the assertion is stated as the property (not the real shape)
        // rather than as one of the two wrong numbers.
        let slab = state_id(&atlas, "minecraft:oak_slab[type=bottom,waterlogged=false]");
        let degraded = live_column(Arc::clone(&atlas), slab, 4..=4).with_version_data(None);
        let degraded_top = degraded.collision_top(0, 4, 0);
        assert!(
            (degraded_top - 0.5).abs() > 1e-6,
            "control did not fire: without the census a bottom slab must NOT reach \
             its real 0.5 top (that IS the bug), got {degraded_top}"
        );
        let real_top = live_column(Arc::clone(&atlas), slab, 4..=4).collision_top(0, 4, 0);
        // `0.5`, not `4.5`: `CollisionView::collision_top` is **block-local** —
        // its own doc says so and names a bottom slab's `0.5` explicitly, and
        // `collision_top_is_uncapped_and_local` in `lodestone-physics` pins it.
        // The `4.5` this used to assert was a world-space value, carried in from
        // the pick-ray work on *outline* boxes, which really are world-space. The
        // control immediately above already calls `0.5` "its real 0.5 top", so
        // the two assertions contradicted each other four lines apart. The fence
        // assertion earlier in this test is the other witness: it expects `1.5`
        // for a fence at y=4, which is only local.
        assert!(
            (real_top - 0.5).abs() < 1e-6,
            "…and with the census it must: got {real_top}, expected the block-local 0.5"
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

        // **The routing gate for the `blocksMotion` census.** These four are
        // `forceSolidOn` blocks whose collision shape is far too thin for
        // `calculateSolid`'s geometry branch, so they are the states that can only
        // be right if `VersionAdapter::block_blocks_motion` is actually being
        // consulted. A view still deriving from the shape answers `false` for all
        // four and passes every other assertion in this test.
        for name in [
            "minecraft:oak_sign[rotation=0,waterlogged=false]",
            "minecraft:stone_pressure_plate[powered=false]",
            "minecraft:lantern[hanging=false,waterlogged=false]",
            "minecraft:turtle_egg[eggs=1,hatch=0]",
        ] {
            let id = state_id(&atlas, name);
            let view = live_column(Arc::clone(&atlas), id, 4..=4);
            assert!(
                view.blocks_motion(0, 4, 0),
                "{name} is forceSolidOn in vanilla and must block motion — if this \
                 fails, the census is not reaching the view and the shape \
                 derivation is answering instead"
            );
            // The control that proves the detector fires: the derivation this
            // replaced, on the same state, gives the *wrong* answer.
            assert!(
                !shape_is_solid(view.shape_of(id)),
                "control did not fire: {name}'s shape must be too thin for \
                 calculateSolid's geometry branch"
            );
        }
        // …and the reverse direction, so the census is not simply answering "true"
        // for everything the derivation called false.
        for name in [
            "minecraft:azalea",
            "minecraft:big_dripleaf[facing=north,tilt=none,waterlogged=false]",
            "minecraft:scaffolding[bottom=false,distance=0,waterlogged=false]",
        ] {
            let id = state_id(&atlas, name);
            let view = live_column(Arc::clone(&atlas), id, 4..=4);
            assert!(
                !view.blocks_motion(0, 4, 0),
                "{name} does not block motion in vanilla"
            );
            assert!(
                shape_is_solid(view.shape_of(id)),
                "control did not fire: {name}'s shape must look solid to the \
                 geometry branch"
            );
        }

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
        let view = LiveCollision::new(
            SectionGrid::from_sparse(sections, 1),
            0,
            1,
            Arc::clone(&atlas),
            inferred_version_data(),
        );

        let hit = crate::raycast::raycast(
            [0.5, 6.5, 0.5],
            [0.0, -1.0, 0.0],
            4.0,
            |x, y, z, out| view.pick_boxes(x, y, z, out),
        )
        .expect("the ray must hit something within 4 blocks");
        assert_eq!(
            hit.block,
            [0, 4, 0],
            "the ray must stop at the kelp, not tunnel through to the stone"
        );

        // The 9/16 column belongs to the **head**, not the body. `Block.column(16,
        // 0, 9)` is `KelpBlock`'s `SHAPE` (`KelpBlock.java:24`), passed to
        // `GrowingPlantHeadBlock`; `kelp_plant` is the `GrowingPlantBodyBlock`
        // half, which overrides no shape and so outlines to a full cube. The
        // committed JVM dump agrees: `kelp_plant` is `0..1`, `kelp[age=0]` is
        // `0..0.5625`. Casting at both pins that the ray reads a *per-state*
        // shape rather than one shape per block — and the head is the one whose
        // entry point a cube-shaped hit test got wrong.
        assert!(
            (hit.hit[1] - 5.0).abs() < 1e-6,
            "kelp_plant's outline is a full cube, so the entry is the cell top: {}",
            hit.hit[1]
        );
        let head = state_id(&atlas, "minecraft:kelp[age=0]");
        let head_view =
            live_cells(Arc::clone(&atlas), inferred_version_data(), &[(0, 4, 0, head)]);
        let head_hit = cast(&head_view, [0.5, 6.5, 0.5], [0.0, -1.0, 0.0])
            .expect("the kelp head must be targetable too");
        assert!(
            (head_hit.hit[1] - (4.0 + 9.0 / 16.0)).abs() < 1e-6,
            "entered the kelp head's real 9/16 top, got y = {}",
            head_hit.hit[1]
        );
    }

    /// Fire the real [`crate::raycast::raycast`] through a live view, so the
    /// caller looks exactly like `Sim::update_target`'s.
    fn cast(view: &LiveCollision, origin: [f64; 3], dir: [f64; 3]) -> Option<crate::raycast::RayHit> {
        crate::raycast::raycast(origin, dir, crate::raycast::REACH, |x, y, z, out| {
            view.pick_boxes(x, y, z, out);
        })
    }

    /// **That fix, against the real per-state census.** Reported from play:
    /// flat blocks like leaf litter stayed highlighted and stayed targetable
    /// with the crosshair plainly above them, because the ray took a per-cell
    /// *boolean* and treated every pickable block as a unit cube.
    ///
    /// The geometry expected below is vanilla's own, transcribed from 26.2
    /// source and cross-checked against the committed JVM outline dump
    /// (`lodestone-data/tests/support/outline_shape_jvm.txt`), not produced by
    /// the ray under test:
    ///
    /// * `CarpetBlock.java:17` — `SHAPE = Block.column(16.0, 0.0, 1.0)`, i.e. the
    ///   full cell in x/z and **`1/16` of a block tall**;
    /// * `LeafLitterBlock` via `SegmentableBlock.java:20,39-41` —
    ///   `Block.box(0, 0, 0, 8, getShapeHeight() = 1, 8)` per segment, so a
    ///   four-segment litter is the same `1/16`-tall plate over the whole cell.
    ///
    /// The first assertion is the **world-species guard**: if the census handed
    /// this cell a unit cube, every ray assertion below would be testing a cube
    /// against a cube and would pass either way.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn the_view_ray_clips_a_flat_blocks_real_outline_box() {
        let atlas = vanilla_atlas();
        // One-sixteenth of a block: vanilla's own number for both of these.
        const TOP: f64 = 1.0 / 16.0;

        for name in [
            "minecraft:leaf_litter[facing=north,segment_amount=4]",
            "minecraft:white_carpet",
        ] {
            let state = state_id(&atlas, name);
            let view = live_cells(Arc::clone(&atlas), inferred_version_data(), &[(0, 4, 0, state)]);

            // The world-species guard: this cell really does carry a 1/16-tall
            // plate in the census the ray reads, not a cube.
            let boxes = view.outline_boxes_at(0, 4, 0);
            assert_eq!(boxes.len(), 1, "{name} has one outline box, got {boxes:?}");
            assert!(
                (boxes[0].min_y - 4.0).abs() < 1e-6
                    && (boxes[0].max_y - (4.0 + TOP)).abs() < 1e-6
                    && (boxes[0].max_x - boxes[0].min_x - 1.0).abs() < 1e-6,
                "{name}'s census outline must be a full-cell 1/16-tall plate — \
                 otherwise these ray assertions test a cube against a cube: {boxes:?}"
            );

            // Straight down: a hit, and the entry point is the *box* top, not the
            // cell top. `5.0` here is the pre-fix answer.
            let down = cast(&view, [0.5, 6.0, 0.5], [0.0, -1.0, 0.0])
                .unwrap_or_else(|| panic!("{name} must still be targetable from above"));
            assert_eq!(down.block, [0, 4, 0]);
            assert_eq!(down.normal, [0, 1, 0]);
            assert!(
                (down.hit[1] - (4.0 + TOP)).abs() < 1e-6,
                "{name}: entered at y = {}, expected the box top 4 + 1/16 (a cube \
                 would say 5.0)",
                down.hit[1]
            );
            assert!(
                (f64::from(down.cursor()[1]) - TOP).abs() < 1e-6,
                "{name}: so use_item_on's cursor y is 1/16, not 1.0"
            );

            // **The reported bug.** A ray crossing the cell at eye height passes
            // 7/16 of a block above the plate: vanilla does not target it, and
            // before that fix this hit.
            assert!(
                cast(&view, [0.5, 4.5, 3.0], [0.0, 0.0, -1.0]).is_none(),
                "{name}: a ray half a block above a 1/16-tall plate must miss"
            );
            // Bracketing the same boundary from the other side is the magnitude
            // control — a ray that rejects *everything* also passes a miss test.
            assert!(
                cast(&view, [0.5, 4.0 + TOP + 0.01, 3.0], [0.0, 0.0, -1.0]).is_none(),
                "{name}: 0.01 above the plate must still miss, so the boundary is \
                 1/16 and not some coarser cutoff"
            );
            let grazing = cast(&view, [0.5, 4.0 + TOP - 0.01, 3.0], [0.0, 0.0, -1.0])
                .unwrap_or_else(|| panic!("{name}: 0.01 below the plate top must hit"));
            assert_eq!(grazing.block, [0, 4, 0]);
            assert_eq!(
                grazing.normal,
                [0, 0, 1],
                "{name}: entered the plate's +Z side"
            );
        }
    }

    /// A block whose outline is several **disjoint** boxes must have all of them
    /// tested, and the gaps between them must be gaps.
    ///
    /// `minecraft:oak_fence` with all four sides connected is three boxes in the
    /// census — the west–east bar through the middle plus the two z arms
    /// (`CrossCollisionBlock.java:55-56`: `column(4, 0, 16)` for the post and
    /// `boxZ(4, 0, 16, 0, 8)` rotated for each arm, unioned and decomposed):
    ///
    /// ```text
    /// [0.0,   0, 0.375] .. [1.0,   1, 0.625]
    /// [0.375, 0, 0.0  ] .. [0.625, 1, 0.375]
    /// [0.375, 0, 0.625] .. [0.625, 1, 1.0  ]
    /// ```
    ///
    /// So the four corners of the cell — `x < 0.375` with `z < 0.375`, and the
    /// three rotations of that — are **empty**, and a ray down a corner must miss
    /// while a ray down any arm must hit. That is the shape of the assertion no
    /// cell-shaped hit test can satisfy, in either direction.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn every_box_of_a_fences_outline_is_tested_and_the_corner_gap_is_a_gap() {
        let atlas = vanilla_atlas();
        let fence = state_id(
            &atlas,
            "minecraft:oak_fence[east=true,north=true,south=true,waterlogged=false,west=true]",
        );
        let view = live_cells(Arc::clone(&atlas), inferred_version_data(), &[(0, 4, 0, fence)]);

        // The world-species guard again: three boxes, or this proves nothing
        // about multi-box clipping.
        let boxes = view.outline_boxes_at(0, 4, 0);
        assert_eq!(
            boxes.len(),
            3,
            "a four-way fence's census outline is the bar plus two arms, got {boxes:?}"
        );

        // One ray per box, straight down onto its top at y = 5.
        for (x, z, what) in [
            (0.5, 0.5, "the post/bar centre"),
            (0.1, 0.5, "the west end of the x bar"),
            (0.5, 0.1, "the north arm"),
            (0.5, 0.9, "the south arm"),
        ] {
            let hit = cast(&view, [x, 6.0, z], [0.0, -1.0, 0.0])
                .unwrap_or_else(|| panic!("{what} must be targetable"));
            assert_eq!(hit.block, [0, 4, 0], "{what}");
            assert_eq!(hit.normal, [0, 1, 0], "{what}");
            assert!(
                (hit.hit[1] - 5.0).abs() < 1e-6,
                "{what}: the fence outline is a full block tall, got {}",
                hit.hit[1]
            );
        }

        // …and the four empty corners are empty. Pre-fix all four hit.
        for (x, z) in [(0.1, 0.1), (0.9, 0.1), (0.1, 0.9), (0.9, 0.9)] {
            assert!(
                cast(&view, [x, 6.0, z], [0.0, -1.0, 0.0]).is_none(),
                "the fence's ({x}, {z}) corner is empty in the census, so the ray \
                 must pass through it"
            );
        }
    }

    /// **The degraded tier still targets.** With no version census the outline
    /// falls back to "has baked model quads ⇒ a unit cube"
    /// ([`LiveCollision::outline_of`]), which is *coarse* — the whole point of
    /// That fix is that a cube is the wrong shape — but it must never become "no
    /// target at all", and air must still not be targetable through it.
    ///
    /// Both halves matter: without the second, a fallback that returned a cube
    /// for *every* cell (air included) would pass the first.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn with_no_version_census_the_ray_still_targets_coarsely() {
        let atlas = vanilla_atlas();
        let litter = state_id(&atlas, "minecraft:leaf_litter[facing=north,segment_amount=4]");
        let view = live_cells(Arc::clone(&atlas), None, &[(0, 4, 0, litter)]);
        assert!(
            !view.has_real_shapes(),
            "this fixture must be the degraded tier, or it proves nothing"
        );

        let hit = cast(&view, [0.5, 6.0, 0.5], [0.0, -1.0, 0.0])
            .expect("a version-free build must still target blocks, coarsely");
        assert_eq!(hit.block, [0, 4, 0]);
        assert!(
            (hit.hit[1] - 5.0).abs() < 1e-6,
            "and coarsely means the whole cell: got y = {}",
            hit.hit[1]
        );

        // The air cells the ray crossed on the way must not have answered — the
        // fallback's last clause hands a cube to anything carrying no fluid.
        assert!(
            cast(&view, [0.5, 6.0, 0.5], [0.0, 1.0, 0.0]).is_none(),
            "looking up into air must find nothing even with no census"
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

    /// **That fix, the world-species control.** A uniform pool — every cell
    /// the same source, as the two spot checks above use — can satisfy every
    /// `fluid_at` assertion even if the resolver ignored the `level` property
    /// outright and hard-coded amount 8: the fixture never contains two
    /// different levels to disagree about (`CLAUDE.md`'s "world" species of
    /// vacuous test — the flaw lives in the input data, not the assertions).
    /// This builds the one structure that can actually falsify it: a single
    /// column stepping through every real `level` value, `0..=8`, the way a
    /// waterfall or a draining pool actually looks in a live world.
    ///
    /// Layer `y` holds `minecraft:water[level=y]`. Expected `(amount,
    /// falling)` pairs are vanilla's own `LiquidBlock` state-cache rule, read
    /// out of `LiquidBlock.java`'s constructor — `stateCache.add(fluid.
    /// getSource(false))` for `level 0`, then `fluid.getFlowing(8 - level,
    /// false)` for `level` in `1..8`, then `fluid.getFlowing(8, true)` for
    /// `level >= 8` — not derived from this crate's own encoder, per the
    /// task's evidence standard.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn a_flowing_water_column_reports_the_real_per_level_amount_and_falling_flag() {
        let atlas = vanilla_atlas();

        let mut section = ChunkSection::new(
            PaletteKind::block_states_with_direct_bits(20),
            PaletteKind::biomes(),
            0,
            0,
        );
        for level in 0u8..=8 {
            let id = state_id(&atlas, &format!("minecraft:water[level={level}]"));
            for x in 0..16 {
                for z in 0..16 {
                    section.set_block(x, usize::from(level), z, id);
                }
            }
        }
        let mut sections = HashMap::new();
        sections.insert((0, 0, 0), Arc::new(section));
        let view = LiveCollision::new(
            SectionGrid::from_sparse(sections, 1),
            0,
            1,
            atlas,
            inferred_version_data(),
        );
        assert!(
            view.has_real_shapes(),
            "no version collision census is wired in — run with --features live"
        );

        // (y, expected amount, expected falling)
        let expected: &[(i32, u8, bool)] = &[
            (0, 8, false), // level 0: source
            (1, 7, false),
            (2, 6, false),
            (3, 5, false),
            (4, 4, false),
            (5, 3, false),
            (6, 2, false),
            (7, 1, false),
            (8, 8, true), // level >= 8: falling, full again
        ];
        for &(y, amount, falling) in expected {
            let cell = view
                .fluid_at(0, y, 0)
                .unwrap_or_else(|| panic!("y={y} must report a fluid cell"));
            assert_eq!(cell.amount, amount, "y={y}: wrong amount");
            assert_eq!(cell.falling, falling, "y={y}: wrong falling flag");
        }

        // The control that proves the detector fires: a cell this fixture never
        // set (y=9, outside the column) must report no fluid at all. Without
        // this, a resolver that reported *some* fixed cell for every `y` —
        // vacuously "passing" by never returning `None` — would not be caught.
        assert_eq!(
            view.fluid_at(0, 9, 0),
            None,
            "control: air above the column carries no fluid"
        );
    }

    /// The boundary the four consumers now share: **eye exactly at the water
    /// surface**. Vanilla's `isEyeInFluid` test is `eyeY <= fluidTop` — inclusive
    /// — so an eye resting exactly on the surface plane counts as submerged.
    /// Pinned here because fog, overlay, sounds and pose all flip on it.
    ///
    /// # Correction (found while closing that fix, `fluid_at`)
    ///
    /// This test used to seed a column `0..=2` and assert the surface sat at the
    /// coarse full-cell plane `y = 3.0` (`2.0 + 1.0`). That was right *when it was
    /// written* — `LiveCollision::fluid_at` returned `None` (nothing but
    /// `is_water`/`is_lava` presence) until `67ff7c3`, one commit later,
    /// implemented [`fluid_cell_of`](LiveCollision::fluid_cell_of) for real. The
    /// moment that landed, this test started asserting a height vanilla never
    /// produces for a *source block with air above*: `FluidState.getHeight` is
    /// `hasSameAbove ? 1.0 : getOwnHeight()`, and a lone source's own height is
    /// `getAmount()/9.0 = 8/9`, not `1.0` — `1.0` only applies to a cell that
    /// itself has the *same fluid* directly above it (see [`fluid_at`]'s own
    /// doc). This test's own bottom two cells (`y = 0, 1`) do get the `1.0`
    /// treatment for exactly that reason; only the *top* cell of a body of water
    /// is a fractional surface. Running this test caught the drift outright — a
    /// hard failure (`eye_in_water: false` at the old boundary), not a silent
    /// wrong answer — which is the point of pinning a boundary instead of an
    /// interior point.
    ///
    /// The column is water up to `y = 2`; the top cell (`y = 2`) has air above
    /// it, so its own height is `8.0/9.0`, computed here with the same `f32`
    /// widening [`crate::collision`]'s `cell_height` uses (via
    /// [`lodestone_physics::FluidCell::own_height`]) so the two land on the exact
    /// same bit pattern rather than an independently-rounded approximation.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn eye_exactly_at_the_water_surface_counts_as_submerged() {
        let atlas = vanilla_atlas();
        let id = state_id(&atlas, "minecraft:water[level=0]");
        let view = live_column(Arc::clone(&atlas), id, 0..=2);

        // `FluidCell::own_height()` for a source (amount 8): `8.0f32 / 9.0f32`.
        // The top cell (y=2) has no water above it, so `hasSameAbove` is false
        // and this — not `1.0` — is vanilla's real surface height there.
        let own_height = 8.0f32 / 9.0f32;

        // Eye exactly on the surface plane.
        let on = fluid_state_at(&view, 2.0, own_height);
        assert!(
            on.under_water(),
            "an eye exactly at the water surface is submerged (vanilla's <=): {on:?}"
        );

        // A hair above the surface: dry eye, but the box is still in water, so
        // `in_water` stays true while `under_water` flips. That pair is what makes
        // this a boundary rather than an on/off.
        let above = fluid_state_at(&view, 2.0, own_height + 0.001);
        assert!(
            !above.under_water(),
            "an eye above the surface is not submerged: {above:?}"
        );
        assert!(
            above.in_water(),
            "the box is still in water even when the eye is out: {above:?}"
        );

        // A hair below: submerged.
        let below = fluid_state_at(&view, 2.0, own_height - 0.001);
        assert!(
            below.under_water(),
            "an eye below the surface is submerged: {below:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Two closed fixes: name-keyed scaffolding/powder-snow hooks, and
    // `is_solid_face`'s same-fluid distinction. A minimal synthetic
    // `BlockView` — no vanilla pack needed, so these run in every ordinary
    // `cargo test`, unlike the `--features live` gates above.
    // -----------------------------------------------------------------------

    /// Per-cell facts for [`TestBlocks`]: just enough of [`BlockView`]'s
    /// surface for the tests below to control directly.
    #[derive(Clone, Default)]
    struct Facts {
        name: Option<&'static str>,
        fluid: Option<FluidKind>,
        shape: &'static [BlockAabb],
    }

    #[derive(Default)]
    struct TestBlocks {
        ids: HashMap<(i32, i32, i32), u32>,
        facts: Vec<Facts>,
    }

    impl TestBlocks {
        fn set(&mut self, x: i32, y: i32, z: i32, f: Facts) {
            let id = u32::try_from(self.facts.len()).unwrap();
            self.facts.push(f);
            self.ids.insert((x, y, z), id);
        }
    }

    impl BlockView for TestBlocks {
        fn state_at(&self, x: i32, y: i32, z: i32) -> u32 {
            // No entry = air: `u32::MAX` deliberately indexes nothing in
            // `facts`, so every other method's `.get()` falls through to its
            // "unknown" default, exactly as an out-of-world query must.
            self.ids.get(&(x, y, z)).copied().unwrap_or(u32::MAX)
        }
        fn shape_of(&self, state: u32) -> &'static [BlockAabb] {
            self.facts
                .get(state as usize)
                .map_or(NO_COLLISION, |f| f.shape)
        }
        fn fluid_kind_of(&self, state: u32) -> Option<FluidKind> {
            self.facts.get(state as usize).and_then(|f| f.fluid)
        }
        fn fluid_cell_of(&self, _state: u32) -> Option<FluidCell> {
            None
        }
        fn name_of(&self, state: u32) -> Option<&'static str> {
            self.facts.get(state as usize).and_then(|f| f.name)
        }
        fn blocks_motion_of(&self, _state: u32) -> Option<bool> {
            None
        }
        fn bubble_column_of(&self, _state: u32) -> Option<bool> {
            None
        }
    }

    #[test]
    fn scaffolding_and_powder_snow_are_told_apart_by_name() {
        let mut w = TestBlocks::default();
        w.set(
            0,
            0,
            0,
            Facts {
                name: Some("minecraft:scaffolding"),
                ..Facts::default()
            },
        );
        w.set(
            0,
            0,
            1,
            Facts {
                name: Some("minecraft:powder_snow"),
                ..Facts::default()
            },
        );
        w.set(
            0,
            0,
            2,
            Facts {
                name: Some("minecraft:stone"),
                ..Facts::default()
            },
        );

        assert!(is_scaffolding_at(&w, 0, 0, 0));
        assert!(
            !is_powder_snow_at(&w, 0, 0, 0),
            "scaffolding is not powder snow"
        );
        assert!(is_powder_snow_at(&w, 0, 0, 1));
        assert!(
            !is_scaffolding_at(&w, 0, 0, 1),
            "powder snow is not scaffolding"
        );
        assert!(
            !is_scaffolding_at(&w, 0, 0, 2) && !is_powder_snow_at(&w, 0, 0, 2),
            "control: stone is neither"
        );
        // Control: an unqueried cell (no `Facts` at all) must not spuriously
        // report either — proves the match isn't defaulting to true.
        assert!(!is_scaffolding_at(&w, 9, 9, 9));
        assert!(!is_powder_snow_at(&w, 9, 9, 9));
    }

    #[test]
    fn is_solid_face_distinguishes_same_fluid_from_a_different_one() {
        // A waterlogged solid block: a real full-cube shape *and* a fluid.
        // Vanilla's `isSolidFace` only excludes the fluid asking the question
        // (`FlowingFluid.java:108`), so a *different* fluid's falling jet must
        // still see the sturdy face — the exact case the old "any fluid
        // present -> false" shortcut got wrong.
        const FULL_CUBE_LOCAL: &[BlockAabb] = &[BlockAabb {
            min: [0.0, 0.0, 0.0],
            max: [1.0, 1.0, 1.0],
        }];
        let mut w = TestBlocks::default();
        w.set(
            1,
            0,
            0,
            Facts {
                name: Some("minecraft:stone"),
                fluid: Some(FluidKind::Water),
                shape: FULL_CUBE_LOCAL,
            },
        );

        assert!(
            !is_solid_face_at(
                &w,
                1,
                0,
                0,
                HorizontalDir::West,
                lodestone_physics::FluidKind::Water
            ),
            "water's own flow must not see itself as a solid face"
        );
        assert!(
            is_solid_face_at(
                &w,
                1,
                0,
                0,
                HorizontalDir::West,
                lodestone_physics::FluidKind::Lava
            ),
            "a falling lava jet must see the waterlogged block's sturdy face — \
             this is the case the pre-#216 'any fluid -> not solid' shortcut \
             answered false"
        );
    }

    // -----------------------------------------------------------------------
    // `SectionGrid`: the dense build must answer exactly what the map build did
    // -----------------------------------------------------------------------

    /// Sections per column in the equivalence fixture. More than one, because a
    /// wrong `section_count` stride is invisible at 1.
    const GRID_SECTIONS: usize = 3;
    /// Deliberately non-zero and not symmetric between x and z, so a transposed
    /// index lands somewhere else instead of on itself.
    const GRID_ORIGIN: (i32, i32) = (5, -3);

    /// Which `(cx, cz, si)` slots of the 3x3x`GRID_SECTIONS` request are present.
    ///
    /// Two shapes `sections_at` really produces are both in here and both matter:
    /// individual **elided all-air sections** (`None` slots inside a live column),
    /// and a whole **edge column with no non-air section anywhere** — which
    /// contributed no keys at all to the old map, so its bounding-box footprint was
    /// 2 columns wide where the request was 3. That difference is the one thing the
    /// old build's doc claimed was harmless, and this is what checks it.
    fn grid_fixture_present(cx: i32, cz: i32, si: usize) -> bool {
        if cx == GRID_ORIGIN.0 + 2 {
            return false; // the absent edge column
        }
        !(si == 1 && cz == GRID_ORIGIN.1) // an elided section inside a live column
    }

    /// A section filled uniformly with `state`, so every cell compared below is a
    /// distinguishing cell rather than air.
    fn filled_section(state: u32) -> Arc<ChunkSection> {
        let mut section = ChunkSection::new(
            PaletteKind::block_states_with_direct_bits(20),
            PaletteKind::biomes(),
            0,
            0,
        );
        for x in 0..16 {
            for y in 0..16 {
                for z in 0..16 {
                    section.set_block(x, y, z, state);
                }
            }
        }
        Arc::new(section)
    }

    /// A unique id per slot, so a mis-indexed grid reads as *another slot's* block
    /// rather than as air — the failure a uniform fill could not see.
    fn grid_fixture_state(cx: i32, cz: i32, si: usize) -> u32 {
        let dx = (cx - GRID_ORIGIN.0) as u32;
        let dz = (cz - GRID_ORIGIN.1) as u32;
        100 + (dx * 3 + dz) * GRID_SECTIONS as u32 + si as u32
    }

    /// The dense grid `Sim::live_collision` hands in: `sections_at`'s response to a
    /// request list built **x-major over z**.
    ///
    /// `transpose` swaps the two loops, which is the one mistake a producer can
    /// make that still yields a correctly-sized `Vec` — the control below.
    fn grid_fixture_aligned(transpose: bool) -> SectionGrid {
        let mut cells = Vec::new();
        if transpose {
            for cz in GRID_ORIGIN.1..=(GRID_ORIGIN.1 + 2) {
                for cx in GRID_ORIGIN.0..=(GRID_ORIGIN.0 + 2) {
                    for si in 0..GRID_SECTIONS {
                        cells.push(grid_fixture_present(cx, cz, si).then(|| {
                            filled_section(grid_fixture_state(cx, cz, si))
                        }));
                    }
                }
            }
        } else {
            for cx in GRID_ORIGIN.0..=(GRID_ORIGIN.0 + 2) {
                for cz in GRID_ORIGIN.1..=(GRID_ORIGIN.1 + 2) {
                    for si in 0..GRID_SECTIONS {
                        cells.push(grid_fixture_present(cx, cz, si).then(|| {
                            filled_section(grid_fixture_state(cx, cz, si))
                        }));
                    }
                }
            }
        }
        SectionGrid::from_aligned(
            cells,
            GRID_ORIGIN.0,
            GRID_ORIGIN.1,
            3,
            3,
            GRID_SECTIONS,
        )
    }

    /// The same content as the old `HashMap` snapshot, for
    /// [`SectionGrid::from_sparse`] — the pre-change production build, kept as the
    /// reference this equivalence is measured against.
    fn grid_fixture_sparse() -> SectionGrid {
        let mut sections = HashMap::new();
        for cx in GRID_ORIGIN.0..=(GRID_ORIGIN.0 + 2) {
            for cz in GRID_ORIGIN.1..=(GRID_ORIGIN.1 + 2) {
                for si in 0..GRID_SECTIONS {
                    if grid_fixture_present(cx, cz, si) {
                        sections.insert(
                            (cx, cz, si),
                            filled_section(grid_fixture_state(cx, cz, si)),
                        );
                    }
                }
            }
        }
        SectionGrid::from_sparse(sections, GRID_SECTIONS)
    }

    fn grid_fixture_view(sections: SectionGrid, atlas: &Arc<BlockAtlas>) -> LiveCollision {
        LiveCollision::new(
            sections,
            0,
            GRID_SECTIONS,
            Arc::clone(atlas),
            inferred_version_data(),
        )
    }

    /// Every cell of the footprint **plus a one-chunk ring and one section above
    /// and below**, so the bounds checks are compared too and not only the hits.
    fn grid_fixture_mismatches(a: &LiveCollision, b: &LiveCollision) -> Vec<(i32, i32, i32, u32, u32)> {
        let mut out = Vec::new();
        let x0 = (GRID_ORIGIN.0 - 1) * 16;
        let z0 = (GRID_ORIGIN.1 - 1) * 16;
        for x in x0..(x0 + 16 * 5) {
            for z in z0..(z0 + 16 * 5) {
                for y in -16..(16 * (GRID_SECTIONS as i32 + 1)) {
                    let (l, r) = (a.block_at(x, y, z), b.block_at(x, y, z));
                    if l != r {
                        out.push((x, y, z, l, r));
                    }
                }
            }
        }
        out
    }

    /// Removing the intermediate `HashMap` must not move a single block.
    ///
    /// `#[ignore]`d for the same reason the thirteen collision gates above it are:
    /// `LiveCollision::new` needs a real [`BlockAtlas`] and `vanilla_atlas()` reads
    /// the client jar out of `.cache/`, which no `git worktree` has. Left
    /// un-ignored it passed in the main checkout and failed in every isolated
    /// verification worktree — a test that is green only where it was written.
    ///
    /// Not "the grid is built": the dense build and the old map build are compared
    /// cell for cell over the whole footprint and a ring outside it, including the
    /// absent edge column whose two footprints genuinely differ.
    #[test]
    #[ignore = "requires the vanilla client.jar (LiveCollision::new needs a real BlockAtlas); \
                run with -- --ignored"]
    fn the_dense_grid_answers_exactly_what_the_map_build_did() {
        let atlas = vanilla_atlas();
        let dense = grid_fixture_view(grid_fixture_aligned(false), &atlas);
        let sparse = grid_fixture_view(grid_fixture_sparse(), &atlas);

        // The two footprints really do differ, or this is comparing one shape with
        // itself and the absent-edge-column case is untested.
        assert_eq!((dense.width_x, dense.width_z), (3, 3));
        assert_eq!(
            (sparse.width_x, sparse.width_z),
            (2, 3),
            "the sparse build's footprint is the bounding box of present keys, so \
             the absent edge column must shrink it — if it does not, the fixture \
             stopped exercising the difference"
        );

        let mismatches = grid_fixture_mismatches(&dense, &sparse);
        assert!(
            mismatches.is_empty(),
            "{} cells disagree; first at {:?} (dense {} vs map {}), last at {:?}",
            mismatches.len(),
            mismatches.first().map(|m| (m.0, m.1, m.2)),
            mismatches.first().map(|m| m.3).unwrap_or(0),
            mismatches.first().map(|m| m.4).unwrap_or(0),
            mismatches.last().map(|m| (m.0, m.1, m.2)),
        );

        // And the comparison is not vacuously empty: the footprint really holds
        // non-air blocks, at the ids the fixture assigned.
        let cx = GRID_ORIGIN.0;
        let cz = GRID_ORIGIN.1 + 1;
        assert_eq!(
            dense.block_at(cx * 16 + 3, 5, cz * 16 + 7),
            grid_fixture_state(cx, cz, 0),
            "the fixture must actually put its own state ids in the footprint"
        );
    }

    /// The control for the gate above: the one producer mistake that still yields a
    /// correctly-sized `Vec` is emitting the request list z-major, and that must be
    /// caught. Run it and watch it disagree — a gate that only ever sees the
    /// correct order proves nothing about the order.
    #[test]
    #[ignore = "requires the vanilla client.jar, like its subject above"]
    fn a_transposed_request_order_is_detected_by_that_comparison() {
        let atlas = vanilla_atlas();
        let transposed = grid_fixture_view(grid_fixture_aligned(true), &atlas);
        let sparse = grid_fixture_view(grid_fixture_sparse(), &atlas);
        let mismatches = grid_fixture_mismatches(&transposed, &sparse);
        assert!(
            !mismatches.is_empty(),
            "a z-major request list must disagree with the map build; if it does \
             not, the comparison above cannot see an index-order regression"
        );
        // Located, not merely counted: the disagreement is inside the footprint.
        let (x, _, z, _, _) = mismatches[0];
        assert!(
            x.div_euclid(16) >= GRID_ORIGIN.0 && x.div_euclid(16) <= GRID_ORIGIN.0 + 2,
            "first mismatch at x={x} is outside the footprint, so it is not the \
             transposition being detected"
        );
        assert!(z.div_euclid(16) >= GRID_ORIGIN.1 && z.div_euclid(16) <= GRID_ORIGIN.1 + 2);
    }
}
