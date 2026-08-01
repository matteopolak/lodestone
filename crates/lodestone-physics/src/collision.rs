//! Swept-AABB collision against block collision shapes, reproducing vanilla's
//! axis order, per-box epsilon semantics, and the 0.6-block auto-step mechanic.
//!
//! Vanilla resolves movement in [`Shapes.collide`] one axis at a time, in an
//! order chosen by [`Direction.axisStepOrder`]: always `Y` first, then `X`/`Z`
//! with the smaller-magnitude horizontal component last. Collision against a
//! block's [`VoxelShape`] reduces, for the box shapes used by real blocks, to a
//! per-box sweep with a `1.0E-7` epsilon on every comparison. We reproduce that
//! epsilon placement rather than the classic pre-1.13 form, because the epsilon
//! decides borderline contacts that the server's anti-cheat also sees.

use crate::fluid::{FluidCell, HorizontalDir};
use crate::geometry::{Aabb, Axis, Vec3d};

/// Vanilla's collision epsilon (`1.0E-7`), used throughout `VoxelShape.collideX`.
const EPSILON: f64 = 1.0E-7;

/// Read-only view of the world's block collision geometry, in world space.
///
/// This is intentionally a trait rather than a dependency on `lodestone-world`:
/// physics must be testable against synthetic worlds, and the real world crate
/// can implement it later. Coordinates are block coordinates; shapes are
/// returned as **world-space** [`Aabb`]s (block-local box plus the block
/// offset), matching how vanilla gathers colliders over an expanded box.
pub trait CollisionView {
    /// Appends the world-space collision boxes for the block at `(x, y, z)` to
    /// `out`. A block with no collision (air, most plants) appends nothing.
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>);

    /// Block-local top surface Y of the collision shape at `(x, y, z)` —
    /// vanilla's `shape.max(Axis.Y)`. **Uncapped**: this is *not* clamped to
    /// `1.0`. Fences, walls and fence gates return **1.5**; `soul_sand` `0.875`;
    /// a bottom slab `0.5`; air / water / lava / cobweb `0.0`.
    ///
    /// The uncapped contract is load-bearing for consumers (e.g. a pathfinder's
    /// step-up check): the 0.6-block auto-step *cannot* mount a 1.5-tall fence,
    /// so clamping this to `1.0` would make fences look step-able and route
    /// navigation straight through them. Do not clamp.
    ///
    /// The default derives the value from [`Self::collision_boxes`] (the true,
    /// already-uncapped shapes), so implementers that return correct boxes get a
    /// correct top for free; override only to serve it directly from a shape
    /// table. Returns `0.0` for a block with no collision boxes.
    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        let mut boxes = Vec::new();
        self.collision_boxes(x, y, z, &mut boxes);
        boxes
            .iter()
            .map(|b| b.max_y - f64::from(y))
            .fold(0.0_f64, f64::max)
    }

    /// Friction (`Block.getFriction`) of the block at `(x, y, z)`; default `0.6`.
    fn friction(&self, _x: i32, _y: i32, _z: i32) -> f32 {
        0.6
    }

    /// Speed factor (`Block.getSpeedFactor`); default `1.0`. Soul sand is
    /// `0.4`, honey `0.4`.
    fn speed_factor(&self, _x: i32, _y: i32, _z: i32) -> f32 {
        1.0
    }

    /// Jump factor (`Block.getJumpFactor`); default `1.0`. Honey is `0.5`.
    fn jump_factor(&self, _x: i32, _y: i32, _z: i32) -> f32 {
        1.0
    }

    /// Whether the block at `(x, y, z)` is a water source/flowing water column
    /// for the purposes of `Entity.isInWater`. Default `false`.
    ///
    /// This is a deliberately coarse hook: it models a fully-submerged player
    /// (the tractable, well-defined part of fluid movement). It does **not**
    /// model per-block fluid height, fluid-push, or the partial-submersion
    /// transition tick, which vanilla derives from the fluid's `level`.
    fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }

    /// Whether the block at `(x, y, z)` is climbable (in `BlockTags.CLIMBABLE`:
    /// ladders, vines, twisting/weeping vines, scaffolding). Default `false`.
    ///
    /// Vanilla's `LivingEntity.onClimbable` tests the block at the entity's
    /// *feet* block position, so a consumer maps this to the ladder/vine tag.
    /// The sneak-to-hold behaviour differs for scaffolding, which this coarse
    /// hook does not distinguish; ladders and vines (the common case) hold.
    fn is_climbable(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }

    /// Whether the block at `(x, y, z)` is lava for `Entity.isInLava`. Default
    /// `false`. Like [`Self::is_water`], this is the coarse fully-submerged hook:
    /// it does not model lava's fluid height (the shallow-vs-deep branch in
    /// `travelInLava`), so a consumer modelling *deep* lava returns `true` here.
    fn is_lava(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }

    /// The **"stuck in block" speed multiplier** for the block at `(x, y, z)`,
    /// if that block impedes movement (`Block.entityInside` →
    /// `Entity.makeStuckInBlock`). Returns `None` for the overwhelming majority
    /// of blocks (air and everything that does not grab you), so an implementer
    /// only overrides for the handful that do:
    ///
    /// | block | multiplier `(x, y, z)` |
    /// |-------|------------------------|
    /// | cobweb | `(0.25, 0.05, 0.25)` |
    /// | powder snow | `(0.9, 1.5, 0.9)` |
    /// | sweet berry bush | `(0.8, 0.75, 0.8)` |
    ///
    /// The block *chooses* the vector, so the version crate — which alone knows
    /// block-state ids — maps id → multiplier here, keeping physics version-free
    /// (pre-1.17 worlds simply never return the powder-snow row). The physics
    /// engine consumes it as a per-tick component-wise scale of movement, exactly
    /// as vanilla, rather than folding it into drag.
    ///
    /// This is the base, entity-independent value. Vanilla lets a per-entity
    /// effect override it (a `WEAVING` mob gets `(0.5, 0.25, 0.5)` in cobweb);
    /// that refinement is deferred and, if ever needed, belongs on the entity,
    /// not this block-keyed seam.
    fn stuck_multiplier(&self, _x: i32, _y: i32, _z: i32) -> Option<Vec3d> {
        None
    }

    /// The fluid occupying `(x, y, z)`, if any, for **flow-current** (fluid-push)
    /// computation (`FlowingFluid.getFlow` / `EntityFluidInteraction.update`).
    /// Default `None` (no fluid) → no current, preserving existing behaviour.
    ///
    /// This is the finer-grained companion to [`Self::is_water`]/[`Self::is_lava`]:
    /// where those coarse booleans only report *presence* (enough for buoyancy and
    /// drag), the current push needs each cell's fluid **level** and its
    /// neighbours' levels, because a fluid flows from a higher column toward a
    /// lower one. Return the fluid's [`FluidCell`] (kind, `getAmount()` in
    /// `1..=8`, and the `FALLING` flag).
    fn fluid_at(&self, _x: i32, _y: i32, _z: i32) -> Option<FluidCell> {
        None
    }

    /// `BlockState.blocksMotion()` for the block at `(x, y, z)`, consulted only by
    /// [`crate::fluid::get_flow`]'s empty-neighbour downflow branch (a fluid spills
    /// over an open edge but not through a solid wall). Default `false` (air-like).
    fn blocks_motion(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }

    /// Whether the block at `(x, y, z)` presents a sturdy solid face toward `dir`
    /// (`FlowingFluid.isSolidFace`), used only by a *falling* fluid's downward jet.
    /// Default `false`.
    fn is_solid_face(&self, _x: i32, _y: i32, _z: i32, _dir: HorizontalDir) -> bool {
        false
    }

    /// `BubbleColumnBlock`'s `DRAG_DOWN` blockstate property for the cell at
    /// `(x, y, z)`, or `None` when the cell is not a bubble column.
    ///
    /// * `Some(true)` — **drag down**. The column stands over a
    ///   `BlockTags.ENABLES_BUBBLE_COLUMN_DRAG_DOWN` block, which in 26.2 is
    ///   `minecraft:magma_block` alone (`data/minecraft/tags/block/
    ///   enables_bubble_column_drag_down.json`). This is also the block's
    ///   *default* state (`BubbleColumnBlock:49`).
    /// * `Some(false)` — **push up**. The column stands over a
    ///   `ENABLES_BUBBLE_COLUMN_PUSH_UP` block, which in 26.2 is
    ///   `minecraft:soul_sand` alone.
    ///
    /// The property is on the wire and in the generated state table — the two
    /// bubble-column states are `15294` (`drag=true`, default) and `15295`
    /// (`drag=false`) in the 26.2 global palette, per Mojang's own
    /// `generated/reports/blocks.json`. Physics stays version-free by asking here
    /// rather than knowing ids, exactly as [`Self::stuck_multiplier`] does.
    ///
    /// **The base block is not this seam's business.** Vanilla resolves soul sand
    /// versus magma once, at *block-update* time, into this single boolean
    /// (`BubbleColumnBlock.getColumnState`); the entity-side code
    /// (`Entity.handleOnInsideBubbleColumn`) only ever reads the boolean and never
    /// looks below the column. So there is no "doubled if a magma block is the
    /// base" term to model — see [`crate::player::tick_water`]'s notes.
    fn bubble_column(&self, _x: i32, _y: i32, _z: i32) -> Option<bool> {
        None
    }

    /// Effective bounce restitution of the block at `(x, y, z)`
    /// (`Block.getBounceRestitution`, already accounting for
    /// `BlockTags.SUPPRESSES_BOUNCE` → `0.0`). Default `0.0`.
    ///
    /// Slime is `1.0`, bed `0.75`. Consulted by `restituteMovementAfterCollisions`
    /// when the entity lands (vertical-collision-below) *fast enough*
    /// (`-vy >= effectiveGravity`) and is **not** sneaking — the sneak-cancels-
    /// bounce rule (`isSuppressingBounce`). A `LivingEntity` (player) does **not**
    /// get the `×0.8` that non-living entities do, so return the raw value.
    fn bounce_restitution(&self, _x: i32, _y: i32, _z: i32) -> f32 {
        0.0
    }
}

/// `Direction.axisStepOrder(Vec3)` — the per-axis resolution order.
///
/// `Math.abs(x) < Math.abs(z) ? [Y, Z, X] : [Y, X, Z]`.
fn axis_step_order(movement: Vec3d) -> [Axis; 3] {
    if movement.x.abs() < movement.z.abs() {
        [Axis::Y, Axis::Z, Axis::X]
    } else {
        [Axis::Y, Axis::X, Axis::Z]
    }
}

/// Gathers every block collision box overlapping `region`, expanded to include
/// the blocks the box touches, mirroring `level.getBlockCollisions`.
fn gather_colliders(view: &dyn CollisionView, region: Aabb) -> Vec<Aabb> {
    // Vanilla iterates the block cursor over floor(min) ..= (ceil(max) - 1),
    // i.e. every block cell the AABB overlaps. We use inclusive floor bounds.
    let min_x = region.min_x.floor() as i32 - 1;
    let min_y = region.min_y.floor() as i32 - 1;
    let min_z = region.min_z.floor() as i32 - 1;
    let max_x = region.max_x.floor() as i32 + 1;
    let max_y = region.max_y.floor() as i32 + 1;
    let max_z = region.max_z.floor() as i32 + 1;
    let mut out = Vec::new();
    for x in min_x..=max_x {
        for y in min_y..=max_y {
            for z in min_z..=max_z {
                view.collision_boxes(x, y, z, &mut out);
            }
        }
    }
    out
}

/// `Shapes.collide(axis, moving, shapes, distance)` for one axis, iterating the
/// candidate boxes and short-circuiting to `0.0` once the residual distance is
/// within epsilon.
fn collide_axis(axis: Axis, moving: &Aabb, shapes: &[Aabb], mut distance: f64) -> f64 {
    for shape in shapes {
        if distance.abs() < EPSILON {
            return 0.0;
        }
        distance = collide_one_box(axis, shape, moving, distance);
    }
    distance
}

/// One box's contribution to `VoxelShape.collideX`, specialised for a single
/// axis-aligned box (which is what real block shapes decompose into).
///
/// The epsilon placement is derived directly from vanilla's index arithmetic in
/// `VoxelShape.collideX`/`findIndex` (see the crate docs), so borderline
/// contacts resolve identically.
fn collide_one_box(axis: Axis, shape: &Aabb, moving: &Aabb, distance: f64) -> f64 {
    let (a, b, c) = perpendicular_axes(axis);

    // Perpendicular overlap on the two other axes, using vanilla's asymmetric
    // epsilon: `mv_max > box_min + eps` and `mv_min + eps < box_max`.
    if !(moving.max(b) - EPSILON > shape.min(b) && moving.min(b) + EPSILON < shape.max(b)) {
        return distance;
    }
    if !(moving.max(c) - EPSILON > shape.min(c) && moving.min(c) + EPSILON < shape.max(c)) {
        return distance;
    }

    let max_a = moving.max(a);
    let min_a = moving.min(a);
    if distance > 0.0 {
        // Moving in +A: only boxes ahead of our leading face can stop us.
        if max_a - EPSILON <= shape.min(a) {
            let new_distance = shape.min(a) - max_a;
            if new_distance >= -EPSILON {
                return distance.min(new_distance);
            }
        }
    } else if distance < 0.0 {
        // Moving in -A: only boxes behind our trailing face can stop us.
        if min_a + EPSILON >= shape.max(a) {
            let new_distance = shape.max(a) - min_a;
            if new_distance <= EPSILON {
                return distance.max(new_distance);
            }
        }
    }
    distance
}

/// Returns `(moving_axis, perp1, perp2)` for a step axis.
fn perpendicular_axes(axis: Axis) -> (Axis, Axis, Axis) {
    match axis {
        Axis::X => (Axis::X, Axis::Y, Axis::Z),
        Axis::Y => (Axis::Y, Axis::X, Axis::Z),
        Axis::Z => (Axis::Z, Axis::X, Axis::Y),
    }
}

/// `Entity.collectCollidersIgnoringWorldBorder` (`Entity.java:1220-1236`) — the
/// one collider list the sweep sees: **entity colliders first**, then the world
/// border (unmodelled), then the block colliders over `region`.
///
/// The order is load-bearing, not cosmetic. [`collide_axis`] short-circuits to
/// `0.0` as soon as the residual distance falls inside `EPSILON`, so which box is
/// visited first can decide between `0.0` and a surviving sub-epsilon value.
/// Vanilla puts entities first; so do we.
fn collect_colliders(
    view: &dyn CollisionView,
    region: Aabb,
    entity_colliders: &[Aabb],
) -> Vec<Aabb> {
    if entity_colliders.is_empty() {
        return gather_colliders(view, region);
    }
    let mut out = entity_colliders.to_vec();
    let blocks = gather_colliders(view, region);
    out.extend_from_slice(&blocks);
    out
}

/// `Entity.collideWithShapes(Vec3, AABB, List<VoxelShape>)` — resolves movement
/// axis by axis, moving the box after each resolved axis.
fn collide_with_shapes(movement: Vec3d, bounding_box: Aabb, shapes: &[Aabb]) -> Vec3d {
    if shapes.is_empty() {
        return movement;
    }
    let mut resolved = Vec3d::ZERO;
    for axis in axis_step_order(movement) {
        let axis_movement = movement.get(axis);
        if axis_movement != 0.0 {
            let moved = bounding_box.move_vec(resolved);
            let collision = collide_axis(axis, &moved, shapes, axis_movement);
            resolved = resolved.with(axis, collision);
        }
    }
    resolved
}

/// `Entity.collideBoundingBox` — gather colliders over the swept box, then
/// resolve.
fn collide_bounding_box(
    view: &dyn CollisionView,
    movement: Vec3d,
    bounding_box: Aabb,
    entity_colliders: &[Aabb],
) -> Vec3d {
    let region = bounding_box.expand_towards(movement.x, movement.y, movement.z);
    let shapes = collect_colliders(view, region, entity_colliders);
    collide_with_shapes(movement, bounding_box, &shapes)
}

/// `CollisionGetter.noCollision(entity, box)` restricted to **block** shapes:
/// whether `box` overlaps no block collider at all.
///
/// This is the `noBlockCollision` term alone. The entity term is
/// [`crate::push::no_entity_collision`] and the conjunction of both is
/// [`crate::push::no_collision_among_entities`], which is what
/// `Player.canPlayerFitWithinBlocksAndEntitiesWhen` and `Player.canFallAtLeast`
/// actually call; this block-only form stays for callers with no entity snapshot,
/// which today is every caller inside this crate. The gap it leaves is narrow by
/// construction — `getEntityCollisions` filters on `canBeCollidedWith`, which no
/// player and no mob satisfies, so only a boat, a shulker or a happy ghast is ever
/// missing from the answer. The remaining unmodelled term of the three is
/// `noBorderCollision` (no world border in this engine).
///
/// Overlap is the
/// strict `min < max` test vanilla's `Shapes.joinIsNotEmpty(…, AND)` reduces to
/// for box shapes — a flush contact does **not** count as a collision, so a
/// player standing exactly on a ledge can still hop onto it.
#[must_use]
pub fn no_collision(view: &dyn CollisionView, box_: Aabb) -> bool {
    let shapes = gather_colliders(view, box_);
    !shapes.iter().any(|s| overlaps(&box_, s))
}

/// Strict AABB overlap (`Shapes.joinIsNotEmpty(a, b, BooleanOp.AND)` for boxes).
fn overlaps(a: &Aabb, b: &Aabb) -> bool {
    a.min_x < b.max_x
        && a.max_x > b.min_x
        && a.min_y < b.max_y
        && a.max_y > b.min_y
        && a.min_z < b.max_z
        && a.max_z > b.min_z
}

/// `LevelReader.containsAnyLiquid(AABB)` — whether any cell the box spans holds a
/// fluid (`!blockState.getFluidState().isEmpty()`).
///
/// The cell range is vanilla's **exclusive-max** `floor(min) ..< ceil(max)`
/// (`LevelReader.java:140-155`), which is *not* the `..= ceil(max) - 1` range the
/// fluid-height sweep uses — the two differ for a box whose max lands exactly on
/// a cell boundary, and this one is deliberately the wider of the two.
#[must_use]
pub fn contains_any_liquid(view: &dyn CollisionView, box_: Aabb) -> bool {
    let x0 = crate::mth::floor(box_.min_x);
    let x1 = crate::mth::ceil(box_.max_x);
    let y0 = crate::mth::floor(box_.min_y);
    let y1 = crate::mth::ceil(box_.max_y);
    let z0 = crate::mth::floor(box_.min_z);
    let z1 = crate::mth::ceil(box_.max_z);
    for x in x0..x1 {
        for y in y0..y1 {
            for z in z0..z1 {
                if view.fluid_at(x, y, z).is_some()
                    || view.is_water(x, y, z)
                    || view.is_lava(x, y, z)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// `Entity.collide(Vec3)` including the auto-step mechanic, against **block
/// geometry only**.
///
/// `on_ground` and `max_up_step` come from the entity; for a player on the
/// ground `max_up_step` is `0.6`. Returns the resolved movement.
///
/// Equivalent to [`collide_among_entities`] with an empty collider slice, which is
/// what makes that addition provably inert for every existing caller: vanilla's
/// `collectCollidersIgnoringWorldBorder` prepends an empty entity list to produce a
/// bit-identical collider list.
#[must_use]
pub fn collide(
    view: &dyn CollisionView,
    movement: Vec3d,
    bounding_box: Aabb,
    on_ground: bool,
    max_up_step: f32,
) -> Vec3d {
    collide_among_entities(view, movement, bounding_box, on_ground, max_up_step, &[])
}

/// `Entity.collide(Vec3)` (`Entity.java:1143-1172`) with the entity half wired: the
/// same swept resolve, over blocks **and** the collider boxes of nearby collidable
/// entities.
///
/// `entity_colliders` must be gathered by
/// [`crate::push::entity_collision_boxes`] over
/// `bounding_box.expand_towards(movement)` — vanilla's
/// `getEntityCollisions(this, aabb.expandTowards(movement))` (`:1145`). Two things
/// about that follow the source and would be natural to "improve" wrongly:
///
/// * the list is gathered **once**, from the *movement* box, and then reused
///   verbatim for the step-up pass even though the step-up box (`stepUpAABB`) is
///   strictly larger (`:1158`). An entity that only overlaps the taller step-up box
///   is therefore invisible to the step. That is vanilla; do not re-gather.
/// * entity colliders participate in `collectCandidateStepUpHeights`, so you can
///   auto-step onto a boat's deck exactly as onto a slab.
#[must_use]
pub fn collide_among_entities(
    view: &dyn CollisionView,
    movement: Vec3d,
    bounding_box: Aabb,
    on_ground: bool,
    max_up_step: f32,
    entity_colliders: &[Aabb],
) -> Vec3d {
    let movement_step = if movement.length_sqr() == 0.0 {
        movement
    } else {
        collide_bounding_box(view, movement, bounding_box, entity_colliders)
    };

    let x_collision = movement.x != movement_step.x;
    let y_collision = movement.y != movement_step.y;
    let z_collision = movement.z != movement_step.z;
    let on_ground_after = y_collision && movement.y < 0.0;

    if max_up_step > 0.0 && (on_ground_after || on_ground) && (x_collision || z_collision) {
        let grounded = if on_ground_after {
            bounding_box.moved(0.0, movement_step.y, 0.0)
        } else {
            bounding_box
        };
        let mut step_up_box =
            grounded.expand_towards(movement.x, f64::from(max_up_step), movement.z);
        if !on_ground_after {
            step_up_box = step_up_box.expand_towards(0.0, -EPSILON, 0.0);
        }

        let colliders = collect_colliders(view, step_up_box, entity_colliders);
        let step_height_to_skip = movement_step.y as f32;
        let candidates =
            candidate_step_up_heights(&grounded, &colliders, max_up_step, step_height_to_skip);

        for candidate in candidates {
            let step = collide_with_shapes(
                Vec3d::new(movement.x, f64::from(candidate), movement.z),
                grounded,
                &colliders,
            );
            if step.horizontal_distance_sqr() > movement_step.horizontal_distance_sqr() {
                let distance_to_ground = bounding_box.min_y - grounded.min_y;
                return step.subtract(Vec3d::new(0.0, distance_to_ground, 0.0));
            }
        }
    }

    movement_step
}

/// `Entity.collectCandidateStepUpHeights` — the sorted set of candidate step
/// heights derived from the top faces of nearby colliders.
fn candidate_step_up_heights(
    bounding_box: &Aabb,
    colliders: &[Aabb],
    max_step_height: f32,
    step_height_to_skip: f32,
) -> Vec<f32> {
    let mut candidates: Vec<f32> = Vec::with_capacity(4);
    'outer: for collider in colliders {
        // A box shape contributes its two Y coordinates (min then max).
        for coord in [collider.min_y, collider.max_y] {
            let relative = (coord - bounding_box.min_y) as f32;
            if relative >= 0.0 && relative != step_height_to_skip {
                if relative > max_step_height {
                    // Vanilla `break`s the inner loop when coords exceed the max
                    // step; coords are ascending per box, so continue to next box.
                    continue 'outer;
                }
                if !candidates.contains(&relative) {
                    candidates.push(relative);
                }
            }
        }
    }
    candidates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic world: a set of solid unit-cube block coordinates plus
    /// optional custom boxes and frictions keyed by block coordinate.
    #[derive(Default)]
    struct TestWorld {
        solid: std::collections::HashSet<(i32, i32, i32)>,
        boxes: std::collections::HashMap<(i32, i32, i32), Vec<Aabb>>,
        friction: std::collections::HashMap<(i32, i32, i32), f32>,
    }

    impl TestWorld {
        fn solid(mut self, x: i32, y: i32, z: i32) -> Self {
            self.solid.insert((x, y, z));
            self
        }
        fn shape(mut self, x: i32, y: i32, z: i32, local: Aabb) -> Self {
            let world = Aabb::new(
                local.min_x + f64::from(x),
                local.min_y + f64::from(y),
                local.min_z + f64::from(z),
                local.max_x + f64::from(x),
                local.max_y + f64::from(y),
                local.max_z + f64::from(z),
            );
            self.boxes.entry((x, y, z)).or_default().push(world);
            self
        }
    }

    impl CollisionView for TestWorld {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if let Some(bs) = self.boxes.get(&(x, y, z)) {
                out.extend_from_slice(bs);
            } else if self.solid.contains(&(x, y, z)) {
                out.push(Aabb::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(z),
                    f64::from(x) + 1.0,
                    f64::from(y) + 1.0,
                    f64::from(z) + 1.0,
                ));
            }
        }
        fn friction(&self, x: i32, y: i32, z: i32) -> f32 {
            *self.friction.get(&(x, y, z)).unwrap_or(&0.6)
        }
    }

    fn player_box(x: f64, y: f64, z: f64) -> Aabb {
        Aabb::new(x - 0.3, y, z - 0.3, x + 0.3, y + 1.8, z + 0.3)
    }

    #[test]
    fn collision_top_is_uncapped_and_local() {
        // Contract enforcement for the pathfinder seam (impl-entity): the top
        // surface is vanilla's `shape.max(Axis.Y)`, block-local and NOT clamped
        // to 1.0. A fence's 1.5 must survive so 0.6 auto-step can't mount it.
        let fence = Aabb::new(0.375, 0.0, 0.375, 0.625, 1.5, 0.625);
        let slab = Aabb::new(0.0, 0.0, 0.0, 1.0, 0.5, 1.0);
        let soul_sand = Aabb::new(0.0, 0.0, 0.0, 1.0, 0.875, 1.0);
        let w = TestWorld::default()
            .shape(2, 64, 2, fence)
            .shape(3, 64, 3, slab)
            .shape(4, 64, 4, soul_sand)
            .solid(5, 64, 5);
        assert_eq!(
            w.collision_top(2, 64, 2),
            1.5,
            "fence must not be capped to 1.0"
        );
        assert_eq!(w.collision_top(3, 64, 3), 0.5);
        assert_eq!(w.collision_top(4, 64, 4), 0.875);
        assert_eq!(w.collision_top(5, 64, 5), 1.0, "full cube");
        assert_eq!(w.collision_top(0, 64, 0), 0.0, "air => 0.0");
    }

    #[test]
    fn falls_onto_floor_stops_at_surface() {
        // Floor of solid blocks at y=0 (top face at y=1). Player at y=1.2 moving
        // down 0.5 should stop with min_y at exactly 1.0 => resolved dy=-0.2.
        let mut w = TestWorld::default();
        for x in -1..=1 {
            for z in -1..=1 {
                w = w.solid(x, 0, z);
            }
        }
        let bb = player_box(0.5, 1.2, 0.5);
        let resolved = collide(&w, Vec3d::new(0.0, -0.5, 0.0), bb, false, 0.0);
        // Bit-exact IEEE result of 1.0 - 1.2, not the naive -0.2.
        assert_eq!(resolved.y, -0.19999999999999996);
    }

    #[test]
    fn walks_into_wall_is_blocked() {
        // Wall block at x=1. Player leading face at x=0.8 moving +0.5 stops at
        // x=1.0 => dx = 0.2.
        let w = TestWorld::default().solid(1, 5, 0).solid(1, 6, 0);
        let bb = player_box(0.5, 5.0, 0.5);
        let resolved = collide(&w, Vec3d::new(0.5, 0.0, 0.0), bb, true, 0.6);
        // Bit-exact IEEE result of 1.0 - 0.8.
        assert_eq!(resolved.x, 0.19999999999999996);
    }

    #[test]
    fn steps_up_half_slab() {
        // A slab (height 0.5) at x=1, floor at y=0. Player on ground walking +x
        // should auto-step up onto the slab: resolved y becomes +0.5.
        let mut w = TestWorld::default();
        for x in -1..=2 {
            for z in -1..=1 {
                w = w.solid(x, 0, z);
            }
        }
        // Slab occupying x=1 from y=1..1.5.
        w = w.shape(1, 1, 0, Aabb::new(0.0, 0.0, 0.0, 1.0, 0.5, 1.0));
        let bb = player_box(0.5, 1.0, 0.5);
        let resolved = collide(&w, Vec3d::new(0.4, 0.0, 0.0), bb, true, 0.6);
        // Horizontal movement is preserved and we rise by the slab height.
        assert_eq!(resolved.x, 0.4);
        assert_eq!(resolved.y, 0.5);
    }

    #[test]
    fn no_colliders_returns_movement_unchanged() {
        let w = TestWorld::default();
        let bb = player_box(0.5, 50.0, 0.5);
        let m = Vec3d::new(0.1, -0.2, 0.3);
        assert_eq!(collide(&w, m, bb, false, 0.0), m);
    }
}
