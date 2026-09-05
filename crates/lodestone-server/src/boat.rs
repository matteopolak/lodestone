//! Placing a boat — the real boat item's use handler, its raytrace, and the
//! vehicle it creates.
//!
//! # What it is
//!
//! The owner's report was *"i cant place a boat down (and probably other
//! things). i think this i related to 'use item'"*, and it was right on both
//! counts: `USE_ITEM` reached [`crate::server`]'s `apply_use_item`, fell past the
//! eat and equip arms, and returned `Nothing`. A boat is not a block item and not
//! a spawn egg, so no other arm could have caught it either.
//!
//! This module answers **where a boat goes**, and [`apply_boat_item`] is the
//! composition that also puts it in the sim. The mount/steer half lives in
//! [`crate::mobs`]' vehicle registry (see [`crate::mobs::MobSim::spawn_vehicle`]).
//!
//! # A boat is not a mob, and that is the whole reason this is its own module
//!
//! An earlier attempt at this deliberately stopped rather than route a boat
//! through the general mob-spawning path, and the reasoning is worth keeping: a
//! boat has no attributes, no goals and no AI, so giving it a mob's component
//! set produces a boat that *wanders*. It also must stop being server-driven the
//! moment a player sits in it — an entity defers authority to its controlling
//! passenger, and a player passenger is always client-authoritative — so an
//! unridden boat needs server-side buoyancy and a ridden one must accept the
//! client's `MoveVehicle`. Both live on the vehicle registry, not here.
//!
//! # The raytrace, which is the part that is easy to get almost right
//!
//! The real boat item's use handler does **not** place relative to the clicked
//! block. It runs its own raytrace and puts the boat at the exact hit **point**:
//!
//! 1. Cast the player's eye-to-reach ray with a clip context that accepts any
//!    fluid. A miss returns "pass" — nothing consumed, nothing created.
//! 2. Take the hit location as-is and set it as the new boat's initial position,
//!    and the boat's yaw to the player's own yaw.
//! 3. If the boat's bounding box at that position collides with the world,
//!    return "fail" instead of placing it.
//!
//! and the underlying point-of-view raytrace itself is: start at the player's
//! eye, extend along the view vector derived from the player's pitch and yaw,
//! scaled by their block interaction range, and clip that segment against the
//! world using block **outline** shapes (not colliders) plus that same
//! any-fluid acceptance.
//!
//! Three consequences, each of which a "one block toward the player" rule gets
//! wrong:
//!
//! * **The clip picks up fluids.** Accepting any non-empty fluid state means the
//!   fluid's shape — `box(0, 0, 0, 1, height, 1)` where `height` is `1.0` when the same fluid is
//!   directly above and `amount / 9.0` otherwise. So a boat aimed at open water
//!   lands on the surface at `y + 0.888…`, not at `y + 1.0` and not on the
//!   seabed. This is why a boat can be placed on water *and* on land by one rule.
//! * **The block shape is the `OUTLINE`, not the collider.** Tall grass, a
//!   flower, a lily pad and wire all have an empty collider and a real outline, so
//!   a boat aimed at them sits on them.
//! * **The position is a point, not a cell.** [`BoatUse::Place::position`] is
//!   un-snapped, and the boat's yaw is the player's own yaw — not the clicked
//!   face's direction.
//!
//! A raytrace pointing **straight down** is a coincident input: the hit point is
//! directly under the eye, which is also what several wrong rules produce. The
//! discriminating case is an angled trace, which [`clip`]'s tests use.
//!
//! # The item → entity derivation
//!
//! Every one of the real item registry's twenty boat-item registrations pairs
//! the item id with the entity type id for the *same* name, irregular names
//! included — `bamboo_raft` and `bamboo_chest_raft` carry no
//! `_boat` suffix at all, and the chest boats are a second axis on top of the
//! wood species. So the derivation is "the item id **is** the entity type id",
//! validated against [`lodestone_data::entity_types`] and against the committed
//! twenty-name extraction in this module's tests, exactly as
//! [`crate::spawn_egg`]'s 88-egg list works.
//!
//! Deriving from a suffix instead would be the trap: `strip_suffix("_boat")`
//! misses both bamboo rafts, and `ends_with("boat") || ends_with("raft")` would
//! accept `minecraft:bamboo` mangled into anything.
//!
//! # How to change it
//!
//! * **A new boat species** needs nothing but the entity type being in the
//!   registry table; the derivation covers it. Add its item id to
//!   `JAR_BOAT_ITEMS` so the count gate stays honest.
//! * **The obstruction test** is deliberately the boat's own box against block
//!   *collision* shapes ([`crate::spawn_egg`]'s resolution helper is the shared
//!   pattern). The real world-collision check also excludes other entities,
//!   which this crate has no world-wide entity query for at this seam — a boat can
//!   therefore be placed overlapping a mob. Documented, small, and visible only as
//!   two things briefly sharing a cell.
//! * **The client half is here.** Our shell's `Sim::use_item_live` now falls
//!   through from a missed/fluid-only crosshair ray to the generic `USE_ITEM`,
//!   the way the real client's use-item entry point does — the client-side gap this
//!   note used to describe (a boat aimed at land never reaching this module) is
//!   closed. `ServerBound::UseItemOn` (a block-target right-click) still carries
//!   no boat handling of its own; every placement, land or water, goes through
//!   `ServerBound::UseItem` → [`apply_boat_item`], because the crosshair ray
//!   ignores fluids and treats land beyond the clicked block's face the same
//!   way the real point-of-view raytrace does for this item.
//!
//! # Dependencies
//!
//! [`lodestone_data::outline_shapes`] for the clip target,
//! [`lodestone_data::collision_shapes`] for the obstruction test,
//! [`crate::fluid::fluid_state_of`] for the fluid surface height, and
//! [`crate::mobs::MobSim`] for the spawn. No protocol and no world handle — the
//! caller supplies a block-state reader, as `use_spawn_egg` does.

use lodestone_data::{block_states, collision_shapes, entity_types, outline_shapes};
use lodestone_model::{BlockPos, ResourceKey, Vec3};

/// Every boat's width — every boat, chest boat and raft in 26.2 shares
/// `1.375 × 0.5625` (`lodestone_data::entity_dimensions`).
pub const BOAT_WIDTH: f64 = 1.375;

/// The shared boat height, `0.5625`.
pub const BOAT_HEIGHT: f64 = 0.5625;

/// The default block-interaction reach attribute's base value, which is what
/// the point-of-view raytrace scales the view vector by.
pub const BLOCK_INTERACTION_RANGE: f64 = 4.5;

/// The additive `minecraft:creative_mode_block_range` attribute modifier a
/// creative player carries, so creative reach is `5.0` rather than `4.5`.
///
/// Worth stating rather than folding in: a gate written at 4.5 for both modes
/// passes for a hit inside 4.5 and cannot see the difference at all.
pub const CREATIVE_BLOCK_INTERACTION_RANGE_BONUS: f64 = 0.5;

/// The block-interaction reach of a player in `game_mode`.
#[must_use]
pub fn block_interaction_range(creative: bool) -> f64 {
    if creative {
        BLOCK_INTERACTION_RANGE + CREATIVE_BLOCK_INTERACTION_RANGE_BONUS
    } else {
        BLOCK_INTERACTION_RANGE
    }
}

/// The entity type a boat-family item id names, or `None` when `item` is not one
/// of the twenty or names no registered entity type.
///
/// The identity mapping — see the module doc for why a suffix rule is wrong. The
/// registry check is what turns an assumption about the *name* into a validated
/// answer, so a modded `foo:driftwood_boat` refuses rather than proposing an
/// entity type nothing can draw.
#[must_use]
pub fn entity_type_for_boat_item(item: &str) -> Option<ResourceKey> {
    let (namespace, path) = match item.split_once(':') {
        Some((namespace, path)) => (namespace, path),
        None => ("minecraft", item),
    };
    if !is_boat_item_path(path) {
        return None;
    }
    let key = ResourceKey::new(namespace, path).ok()?;
    entity_types::entity_type_id(&key.to_string())?;
    Some(key)
}

/// Whether a bare item path is one of the twenty boat items.
///
/// A `&&`-of-suffixes rather than a twenty-entry table: `_boat`/`_chest_boat`
/// covers eighteen and the two bamboo rafts are named outright, which is the same
/// split `crate::furnace`'s fuel table already documents for `bamboo_raft`. The
/// registry check in [`entity_type_for_boat_item`] is what rejects a name that
/// passes here and is not real.
fn is_boat_item_path(path: &str) -> bool {
    path.ends_with("_boat") || path == "bamboo_raft" || path == "bamboo_chest_raft"
}

/// One hit from [`clip`] — the real block-hit result reduced to the two facts
/// the boat item's use handler reads off it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipHit {
    /// The exact point on the surface, which is what the boat's initial position
    /// receives. **Not** snapped to a cell.
    pub position: Vec3,
    /// The cell whose shape was hit, carried for diagnostics and for a caller
    /// that wants to know which block it landed on.
    pub cell: BlockPos,
}

/// A block-outline-and-any-fluid raycast, or `None` for a miss.
///
/// `from`/`to` are the eye and the eye plus `view * reach`.
///
/// # The traversal reproduces the real one, not a resampling
///
/// The real block-traversal is an exact voxel DDA: both endpoints are nudged
/// inward by a tenth-of-a-micron linear interpolation toward each other, the
/// first cell is tested before any step, and the loop advances whichever axis'
/// next boundary is nearest until all three parameters exceed `1.0`. A
/// fixed-step resampler (the shape [`crate::mobs`]' projectile ray uses,
/// quarter-block spacing) is fine for "does this segment cross a solid cell" and
/// **wrong** here: the answer needed is the first *surface* along the ray to
/// within float precision, and a boat placed a quarter block out sits visibly
/// inside the shoreline.
///
/// # What is not modelled
///
/// A later refinement step derives the hit's **face direction** from the
/// block's interaction shape and leaves the location untouched, and this
/// returns no direction — the boat item's use handler reads only the location.
/// Adding the face later needs `lodestone_data::outline_shapes::interaction_boxes`.
#[must_use]
pub fn clip(from: Vec3, to: Vec3, block_state: &dyn Fn(i32, i32, i32) -> String) -> Option<ClipHit> {
    if (from.x - to.x).abs() < f64::EPSILON
        && (from.y - to.y).abs() < f64::EPSILON
        && (from.z - to.z).abs() < f64::EPSILON
    {
        return None;
    }
    // The real nudge is `a - 1e-7 * (b - a)`: both endpoints move
    // *outward* along the segment by a tenth of a micron. Faithful because the
    // nudge is what decides which cell a ray grazing a boundary starts in.
    let nudge = 1.0e-7;
    let lerp = |a: f64, b: f64| a - nudge * (b - a);
    let sx = lerp(from.x, to.x);
    let sy = lerp(from.y, to.y);
    let sz = lerp(from.z, to.z);
    let ex = lerp(to.x, from.x);
    let ey = lerp(to.y, from.y);
    let ez = lerp(to.z, from.z);

    let mut cell_x = sx.floor() as i32;
    let mut cell_y = sy.floor() as i32;
    let mut cell_z = sz.floor() as i32;
    if let Some(hit) = clip_cell(BlockPos::new(cell_x, cell_y, cell_z), from, to, block_state) {
        return Some(hit);
    }

    let dx = ex - sx;
    let dy = ey - sy;
    let dz = ez - sz;
    let sign = |d: f64| {
        if d > 0.0 {
            1
        } else if d < 0.0 {
            -1
        } else {
            0
        }
    };
    let (step_x, step_y, step_z) = (sign(dx), sign(dy), sign(dz));
    let delta = |step: i32, d: f64| {
        if step == 0 {
            f64::MAX
        } else {
            f64::from(step) / d
        }
    };
    let (delta_x, delta_y, delta_z) = (
        delta(step_x, dx),
        delta(step_y, dy),
        delta(step_z, dz),
    );
    // The real fractional part is `x - floor(x)`, always in `[0, 1)`.
    let frac = |v: f64| v - v.floor();
    let first = |step: i32, delta: f64, start: f64| {
        delta
            * if step > 0 {
                1.0 - frac(start)
            } else {
                frac(start)
            }
    };
    let mut t_x = first(step_x, delta_x, sx);
    let mut t_y = first(step_y, delta_y, sy);
    let mut t_z = first(step_z, delta_z, sz);

    while t_x <= 1.0 || t_y <= 1.0 || t_z <= 1.0 {
        if t_x < t_y {
            if t_x < t_z {
                cell_x += step_x;
                t_x += delta_x;
            } else {
                cell_z += step_z;
                t_z += delta_z;
            }
        } else if t_y < t_z {
            cell_y += step_y;
            t_y += delta_y;
        } else {
            cell_z += step_z;
            t_z += delta_z;
        }
        if let Some(hit) = clip_cell(BlockPos::new(cell_x, cell_y, cell_z), from, to, block_state) {
            return Some(hit);
        }
    }
    None
}

/// One cell of [`clip`]'s traversal: the nearer of the block outline's hit and
/// the fluid shape's, matching the real clip's own tie rule — on an equal
/// squared distance, the block result wins over the liquid result.
fn clip_cell(
    cell: BlockPos,
    from: Vec3,
    to: Vec3,
    block_state: &dyn Fn(i32, i32, i32) -> String,
) -> Option<ClipHit> {
    let state = block_state(cell.x, cell.y, cell.z);
    let block_t = outline_boxes_for(&state)
        .iter()
        .filter_map(|b| {
            clip_box(
                from,
                to,
                cell,
                [f64::from(b.min[0]), f64::from(b.min[1]), f64::from(b.min[2])],
                [f64::from(b.max[0]), f64::from(b.max[1]), f64::from(b.max[2])],
            )
        })
        .fold(None::<f64>, |best, t| {
            Some(best.map_or(t, |best| best.min(t)))
        });
    let fluid_t = fluid_surface_height(&state, block_state, cell).and_then(|height| {
        clip_box(from, to, cell, [0.0, 0.0, 0.0], [1.0, height, 1.0])
    });
    // The real comparison is a squared-distance tie going to the **block**.
    // That collapses to a plain `min` here because the only thing this returns
    // is the location, and on a tie the two locations are the same point — the
    // tie is real (a waterlogged slab's outline top and its fluid top are both
    // 1.0 under water) and the two answers differ only in the hit face, which
    // this does not carry. If a face is ever added, the tie rule has to come
    // back.
    let t = match (block_t, fluid_t) {
        (Some(block), Some(fluid)) => block.min(fluid),
        (Some(block), None) => block,
        (None, Some(fluid)) => fluid,
        (None, None) => return None,
    };
    Some(ClipHit {
        position: Vec3::new(
            from.x + t * (to.x - from.x),
            from.y + t * (to.y - from.y),
            from.z + t * (to.z - from.z),
        ),
        cell,
    })
}

/// The real AABB-clip face/entry-point pair, returning only the parameter
/// `s` of the entry crossing.
///
/// `min`/`max` are block-local; `cell` offsets them. `s` is required to be
/// **strictly** greater than zero and below the running best, and the other two
/// axes are bounded with the real `1.0E-7` slack — a ray exactly along a
/// shared face must still hit.
///
/// A start point *inside* the shape yields `None` here, and the real search
/// handles that case one level up, over the whole voxel shape rather than one
/// box: it nudges the start forward by `0.001` of the segment, and if that
/// lands inside the shape returns a hit at that nudged point flagged as an
/// inside hit. [`clip_cell`] does not reproduce the inside branch, so a player
/// whose *eye* is submerged places the boat on the first surface ahead rather
/// than at their own eye. Stated because it is a real divergence and only
/// reachable while swimming.
fn clip_box(from: Vec3, to: Vec3, cell: BlockPos, min: [f64; 3], max: [f64; 3]) -> Option<f64> {
    let d = [to.x - from.x, to.y - from.y, to.z - from.z];
    let origin = [from.x, from.y, from.z];
    let base = [f64::from(cell.x), f64::from(cell.y), f64::from(cell.z)];
    let lo = [base[0] + min[0], base[1] + min[1], base[2] + min[2]];
    let hi = [base[0] + max[0], base[1] + max[1], base[2] + max[2]];
    let mut best = 1.0f64;
    let mut found = false;
    // The real test walks the three axes in x, y, z order, each against the
    // *near* plane for a positive component and the *far* plane for a negative
    // one.
    for axis in 0..3 {
        let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
        let plane = if d[axis] > 1.0e-7 {
            lo[axis]
        } else if d[axis] < -1.0e-7 {
            hi[axis]
        } else {
            continue;
        };
        let s = (plane - origin[axis]) / d[axis];
        let pb = origin[b] + s * d[b];
        let pc = origin[c] + s * d[c];
        if s > 0.0
            && s < best
            && lo[b] - 1.0e-7 < pb
            && pb < hi[b] + 1.0e-7
            && lo[c] - 1.0e-7 < pc
            && pc < hi[c] + 1.0e-7
        {
            best = s;
            found = true;
        }
    }
    found.then_some(best)
}

/// The top of the fluid shape in `cell`, block-local, or `None` when the cell
/// holds no fluid.
///
/// The real fluid shape is `box(0, 0, 0, 1, height, 1)`, where `height` is
/// `1.0` when the same fluid sits directly above, and otherwise the fluid's own
/// height, `amount / 9.0`. So a water source under air tops out at `0.888…` and
/// one under more water at `1.0` — and that difference is exactly the boat's
/// resting height on the surface, so it is the value the placement rule most
/// depends on.
///
/// (The real shape's own "amount equals 9" fast path is unreachable for water
/// and lava, whose maximum amount is 8, and would give the same `1.0` anyway.)
fn fluid_surface_height(
    state: &str,
    block_state: &dyn Fn(i32, i32, i32) -> String,
    cell: BlockPos,
) -> Option<f64> {
    let fluid = crate::fluid::fluid_state_of(state)?;
    let above = block_state(cell.x, cell.y + 1, cell.z);
    let same_above = crate::fluid::fluid_state_of(&above).is_some_and(|a| a.kind == fluid.kind);
    Some(if same_above {
        1.0
    } else {
        f64::from(fluid.own_height())
    })
}

/// The **outline** boxes of a full block-state string, empty for air and for a
/// name outside the table.
///
/// Resolution is `block_state_id` then `block_states::state_id`, never
/// `block_state_id_or_default` — the same choice, for the same reason,
/// [`crate::spawn_egg`]'s own helper documents: the default answer for a bare
/// name is not the block's *lowest* state id.
fn outline_boxes_for(state: &str) -> &'static [lodestone_model::BlockAabb] {
    let id = crate::mobs::block_state_id(state).or_else(|| block_states::state_id(state));
    id.and_then(block_states::StateId::new)
        .map(outline_shapes::outline_boxes)
        .unwrap_or(&[])
}

/// The **collision** boxes of a full block-state string, for the obstruction
/// test. Empty for air, a fluid, and every plant.
fn collision_boxes_for(state: &str) -> &'static [collision_shapes::Aabb] {
    let id = crate::mobs::block_state_id(state).or_else(|| block_states::state_id(state));
    id.and_then(block_states::StateId::new)
        .map(collision_shapes::collision_boxes)
        .unwrap_or(&[])
}

/// What a right-click with the held item means for this module.
#[derive(Debug, Clone, PartialEq)]
pub enum BoatUse {
    /// Not a boat item. The caller continues to whatever it would have done.
    NotABoat,
    /// A boat item, and the real handler returns "pass" or "fail": the raytrace
    /// missed everything, or the boat's box would overlap a block. **The stack is
    /// not consumed.**
    Refused,
    /// Create `entity_type` at `position` facing `yaw`, then consume one.
    Place {
        /// The entity type the item names — identical to the item id.
        entity_type: ResourceKey,
        /// The raytrace's own hit point, un-snapped, used as the new boat's
        /// initial position.
        position: Vec3,
        /// The placing player's own yaw, in degrees.
        yaw: f32,
    },
}

/// The real boat item's use handler, as a decision.
///
/// `eye` is the player's eye position (feet plus the eye height, which the caller
/// has and this does not), `yaw`/`pitch` the player's rotation in degrees, and
/// `reach` [`block_interaction_range`] for their game mode.
///
/// # What is deliberately not modelled
///
/// The pickable-entity sweep near the top of the real handler — the real
/// handler returns "pass" when the player's own eye is inside a pickable
/// entity's inflated box, so that a boat is not placed while you stand inside
/// another one. This crate has no world-wide entity query at this seam. Its absence is
/// visible only as being able to place a boat while already sitting in one, and
/// closing it needs [`crate::mobs::MobSim`] to expose an entity-box query.
#[must_use]
pub fn use_boat_item(
    item: &str,
    eye: Vec3,
    yaw: f32,
    pitch: f32,
    reach: f64,
    block_state: &dyn Fn(i32, i32, i32) -> String,
) -> BoatUse {
    let Some(entity_type) = ({
        let path = item.split_once(':').map_or(item, |(_, path)| path);
        if is_boat_item_path(path) {
            // A real boat item name that resolves to no entity type is `FAIL`,
            // not "not a boat": falling through would place a block.
            match entity_type_for_boat_item(item) {
                Some(key) => Some(key),
                None => return BoatUse::Refused,
            }
        } else {
            None
        }
    }) else {
        return BoatUse::NotABoat;
    };

    let view = view_vector(yaw, pitch);
    let to = Vec3::new(
        eye.x + view.x * reach,
        eye.y + view.y * reach,
        eye.z + view.z * reach,
    );
    // A ray miss maps to "pass". Nothing is consumed and nothing is created;
    // this is the arm that fires when you right-click at the sky.
    let Some(hit) = clip(eye, to, block_state) else {
        return BoatUse::Refused;
    };
    // A collision with the world maps to "fail".
    if boat_box_is_obstructed(hit.position, block_state) {
        return BoatUse::Refused;
    }
    BoatUse::Place {
        entity_type,
        position: hit.position,
        yaw,
    }
}

/// Whether the world collides with a boat's bounding box for a boat whose feet
/// are at `position`.
///
/// The box is built the real way: centred horizontally on the
/// position, sitting on it vertically. Every cell the box spans is tested against
/// its collision boxes, and the overlap test uses a `1.0E-7` epsilon so a boat
/// resting exactly on a surface is not "inside" it — the whole point of placing
/// on a shoreline is that the box touches the ground.
///
/// Entities are not tested; see [`use_boat_item`]'s own note.
#[must_use]
pub fn boat_box_is_obstructed(
    position: Vec3,
    block_state: &dyn Fn(i32, i32, i32) -> String,
) -> bool {
    let half = BOAT_WIDTH / 2.0;
    let (min_x, max_x) = (position.x - half, position.x + half);
    let (min_y, max_y) = (position.y, position.y + BOAT_HEIGHT);
    let (min_z, max_z) = (position.z - half, position.z + half);
    let eps = 1.0e-7;
    for cx in (min_x - 1.0).floor() as i32..=(max_x).floor() as i32 {
        for cy in (min_y - 1.0).floor() as i32..=(max_y).floor() as i32 {
            for cz in (min_z - 1.0).floor() as i32..=(max_z).floor() as i32 {
                let state = block_state(cx, cy, cz);
                for b in collision_boxes_for(&state) {
                    let bx = (
                        f64::from(cx) + f64::from(b.min[0]),
                        f64::from(cx) + f64::from(b.max[0]),
                    );
                    let by = (
                        f64::from(cy) + f64::from(b.min[1]),
                        f64::from(cy) + f64::from(b.max[1]),
                    );
                    let bz = (
                        f64::from(cz) + f64::from(b.min[2]),
                        f64::from(cz) + f64::from(b.max[2]),
                    );
                    if min_x < bx.1 - eps
                        && max_x > bx.0 + eps
                        && min_y < by.1 - eps
                        && max_y > by.0 + eps
                        && min_z < bz.1 - eps
                        && max_z > bz.0 + eps
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// The real player view-vector derivation, in `f64`.
///
/// The real computation goes through a quantized `float` sine table, which
/// [`lodestone_physics`]'s player path reproduces bit-for-bit because a
/// divergence there compounds over a movement tick. Here it only aims a
/// single-use ray whose result is immediately quantised by the clip's own `1e-7`
/// slack, so the plain `f64` trig is used and the choice is stated rather than
/// left to be rediscovered.
#[must_use]
fn view_vector(yaw: f32, pitch: f32) -> Vec3 {
    let x_rot = f64::from(pitch).to_radians();
    let y_rot = -f64::from(yaw).to_radians();
    let (y_sin, y_cos) = y_rot.sin_cos();
    let (x_sin, x_cos) = x_rot.sin_cos();
    Vec3::new(y_sin * x_cos, -x_sin, y_cos * x_cos)
}

/// What [`apply_boat_item`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum BoatApplied {
    /// Not a boat item.
    NotABoat,
    /// "Pass" or "fail": nothing created, **stack not consumed**.
    Refused,
    /// The boat exists in the sim and will stream to every viewer on the next
    /// snapshot diff. The caller now consumes one from the stack — the real
    /// handler consumes the item from the stack only *after* the entity is
    /// actually added to the world.
    Placed {
        /// The new entity's network id.
        entity_id: i32,
        /// What was created.
        entity_type: ResourceKey,
        /// Where its feet went.
        position: Vec3,
        /// Its yaw, in degrees.
        yaw: f32,
    },
}

/// [`use_boat_item`] **plus the spawn** — the composition, named, for the reason
/// [`crate::spawn_egg::apply_spawn_egg`] is: the decision and the spawn can each
/// be right while the seam between them is where the defect lives, and a seam
/// with no name has nothing to point a gate at.
pub fn apply_boat_item(
    item: &str,
    eye: Vec3,
    yaw: f32,
    pitch: f32,
    reach: f64,
    block_state: &dyn Fn(i32, i32, i32) -> String,
    mobs: &crate::MobHandle,
) -> BoatApplied {
    match use_boat_item(item, eye, yaw, pitch, reach, block_state) {
        BoatUse::NotABoat => BoatApplied::NotABoat,
        BoatUse::Refused => BoatApplied::Refused,
        BoatUse::Place {
            entity_type,
            position,
            yaw,
        } => {
            let entity_id =
                mobs.with(|sim| sim.spawn_vehicle(entity_type.clone(), position, yaw));
            BoatApplied::Placed {
                entity_id,
                entity_type,
                position,
                yaw,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every boat-item registration in the pinned 26.2 decompile's real item
    /// registry, extracted as (item id, entity type id) pairs
    /// and committed here so the gate does not need `.cache/` present. All twenty
    /// pair a name with **itself**, which is what makes the derivation exact.
    static JAR_BOAT_ITEMS: [&str; 20] = [
        "acacia_boat",
        "acacia_chest_boat",
        "bamboo_chest_raft",
        "bamboo_raft",
        "birch_boat",
        "birch_chest_boat",
        "cherry_boat",
        "cherry_chest_boat",
        "dark_oak_boat",
        "dark_oak_chest_boat",
        "jungle_boat",
        "jungle_chest_boat",
        "mangrove_boat",
        "mangrove_chest_boat",
        "oak_boat",
        "oak_chest_boat",
        "pale_oak_boat",
        "pale_oak_chest_boat",
        "spruce_boat",
        "spruce_chest_boat",
    ];

    /// **Every real boat item resolves, to its own name.** Collected rather than
    /// asserted in the loop: an `assert!` inside would abort on the first miss and
    /// prove exactly one arm, so a systematic gap (say, both bamboo rafts, which a
    /// `_boat` suffix rule loses) would be reported as a single name.
    #[test]
    fn every_jar_registered_boat_item_resolves_to_its_own_entity_type() {
        let mut wrong = Vec::new();
        for item in JAR_BOAT_ITEMS {
            let full = format!("minecraft:{item}");
            match entity_type_for_boat_item(&full).map(|k| k.to_string()) {
                Some(got) if got == full => {}
                other => wrong.push(format!("{full}: {other:?}")),
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
        let mut sorted = JAR_BOAT_ITEMS;
        sorted.sort_unstable();
        assert_eq!(sorted, JAR_BOAT_ITEMS, "the list must stay sorted");
        assert_eq!(
            JAR_BOAT_ITEMS.len(),
            20,
            "the real item registry carries exactly 20 boat item registrations in 26.2"
        );
    }

    /// The refusals, each for its own reason. `bamboo` is the row that separates
    /// "matches a suffix" from "is a boat item": it is a real item and not a boat.
    #[test]
    fn only_boat_items_resolve() {
        let mut wrong = Vec::new();
        for (item, want_some) in [
            ("minecraft:oak_boat", true),
            ("minecraft:bamboo_raft", true),
            ("minecraft:bamboo_chest_raft", true),
            ("minecraft:oak_chest_boat", true),
            ("minecraft:bamboo", false),
            ("minecraft:stone", false),
            ("minecraft:oak_planks", false),
            // A name with the right shape and no entity type behind it.
            ("minecraft:driftwood_boat", false),
            ("foo:oak_boat", false),
        ] {
            if entity_type_for_boat_item(item).is_some() != want_some {
                wrong.push(item);
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// A world of stone at `y <= 63`, water at `y == 64` for `x >= 0`, air above,
    /// unless a cell is overridden.
    ///
    /// The water is a **source under air**, so its shape tops out at `8/9`, and
    /// the shore at `x < 0` is stone up to `y == 64` — the shape that makes an
    /// angled trace land on either side depending only on the angle.
    fn shore(overrides: Vec<(BlockPos, &'static str)>) -> impl Fn(i32, i32, i32) -> String {
        move |x, y, z| {
            let at = BlockPos::new(x, y, z);
            if let Some((_, name)) = overrides.iter().find(|(p, _)| *p == at) {
                return (*name).to_owned();
            }
            if y <= 63 {
                return "minecraft:stone".to_owned();
            }
            if y == 64 {
                return if x >= 0 {
                    "minecraft:water[level=0]".to_owned()
                } else {
                    "minecraft:stone".to_owned()
                };
            }
            "minecraft:air".to_owned()
        }
    }

    /// The top of a water source with air above, **as an `f32`**.
    ///
    /// The real fluid's own height is its fluid amount divided by `9.0F` — a *float* divide,
    /// widened to `double` only when the shape is built. So the surface is
    /// `0.88888889…`, not `0.888888888…`: predicting the exact `f64` value of 8/9
    /// fails by 6.6e-9, which is far outside any tolerance worth using and reads
    /// exactly like a wrong rule. The narrowing is part of the answer.
    const WATER_SOURCE_TOP: f64 = 64.0 + (8.0f32 / 9.0f32) as f64;

    /// **A straight-down trace is the coincident input**, and it is asserted here
    /// only to pin the fluid surface height — the discriminating angled case is
    /// below.
    ///
    /// The eye is at `(4.5, 66.6, 4.5)`; straight down (`pitch = 90`) the ray
    /// enters the water cell `(4, 64, 4)` at its shape's top.
    #[test]
    fn straight_down_onto_water_lands_on_the_eight_ninths_surface() {
        let hit = clip(
            Vec3::new(4.5, 66.6, 4.5),
            Vec3::new(4.5, 66.6 - 4.5, 4.5),
            &shore(vec![]),
        )
        .expect("the ray must reach the water within 4.5 blocks");
        assert_eq!(hit.cell, BlockPos::new(4, 64, 4));
        assert!(
            (hit.position.y - WATER_SOURCE_TOP).abs() < 1e-12,
            "a source under air tops out at 8/9, not 1.0 and not the seabed: {:?}",
            hit.position
        );
        // A cell-snapping rule would answer 4.5 here too, which is why the
        // horizontal coordinates are *not* the discriminating part of this gate.
        assert!((hit.position.x - 4.5).abs() < 1e-9);
    }

    /// **Water directly under more water tops out at `1.0`, not `8/9`** —
    /// `getHeight`'s `hasSameAbove` branch.
    ///
    /// The discriminating input is a **horizontal** ray at `y = 65.95`, which is
    /// above `65 + 8/9` and below `66`. With water stacked above, the lower cell's
    /// shape is a full cube and the ray hits its west face; without, the shape
    /// stops short and the ray passes clean over it. A *vertical* trace cannot
    /// separate the two at all — it always hits the topmost water cell's own
    /// surface, whose neighbour above is air in both worlds, which is how the
    /// first version of this gate managed to assert 66.0 against a fixture that
    /// could only ever produce 65.888.
    #[test]
    fn a_submerged_water_cell_tops_out_at_a_full_block() {
        let stacked = shore(vec![
            (BlockPos::new(0, 65, 4), "minecraft:water[level=0]"),
            (BlockPos::new(0, 66, 4), "minecraft:water[level=0]"),
        ]);
        let hit = clip(
            Vec3::new(-2.0, 65.95, 4.5),
            Vec3::new(3.0, 65.95, 4.5),
            &stacked,
        )
        .expect("a full-cube water shape must be hit at 65.95");
        let mut wrong = Vec::new();
        if hit.cell != BlockPos::new(0, 65, 4) {
            wrong.push(format!("cell {:?}", hit.cell));
        }
        if (hit.position.x - 0.0).abs() > 1e-9 {
            wrong.push(format!("x {} != 0.0 (the west face)", hit.position.x));
        }
        assert!(wrong.is_empty(), "{wrong:#?}");

        // The control that makes the row above mean something: the *same* ray with
        // no water above the cell tops the shape out at 65.888… and misses it.
        let single = shore(vec![(BlockPos::new(0, 65, 4), "minecraft:water[level=0]")]);
        assert_eq!(
            clip(
                Vec3::new(-2.0, 65.95, 4.5),
                Vec3::new(3.0, 65.95, 4.5),
                &single
            ),
            None,
            "8/9 of a block is below 65.95, so there is nothing to hit"
        );
    }

    /// **The angled case, which is the whole point of porting the raytrace.**
    ///
    /// The eye sits over the stone shore at `x = -0.5`, `y = 65.62`, looking
    /// east-and-down at 45°. The ray therefore travels `+x` and `-y` at equal
    /// rates and must cross into the water column, hitting the water's top plane
    /// at [`WATER_SOURCE_TOP`] — which is `0.7311…` below the eye, so `x` advances
    /// by the same amount to `0.2311…`.
    ///
    /// Both numbers are derived from the plane arithmetic here, not read off the
    /// implementation, and neither is what a "one block toward the player" or a
    /// cell-snapping rule produces: those give an integer-ish `x` and `y = 65`.
    ///
    /// **Yaw `-90` is east.** Minecraft's yaw runs `0` = `+z` (south), `90` =
    /// `-x` (west), so the first version of this gate used `90`, walked *into* the
    /// shore, and hit stone at `y = 65` — a plausible-looking number from the
    /// wrong direction.
    #[test]
    fn an_angled_trace_lands_where_the_plane_arithmetic_says() {
        let eye = Vec3::new(-0.5, 65.62, 4.5);
        let view = view_vector(-90.0, 45.0);
        let reach = BLOCK_INTERACTION_RANGE;
        let to = Vec3::new(
            eye.x + view.x * reach,
            eye.y + view.y * reach,
            eye.z + view.z * reach,
        );
        let hit = clip(eye, to, &shore(vec![])).expect("the ray must hit the water surface");
        let surface = WATER_SOURCE_TOP;
        let drop = eye.y - surface;
        let mut wrong = Vec::new();
        if (hit.position.y - surface).abs() > 1e-6 {
            wrong.push(format!("y {} != {surface}", hit.position.y));
        }
        if (hit.position.x - (eye.x + drop)).abs() > 1e-6 {
            wrong.push(format!("x {} != {}", hit.position.x, eye.x + drop));
        }
        if hit.cell != BlockPos::new(0, 64, 4) {
            wrong.push(format!("cell {:?} != (0, 64, 4)", hit.cell));
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// A trace at the sky misses, and a miss is `PASS` — no boat, no consume.
    #[test]
    fn a_trace_into_open_sky_misses() {
        assert_eq!(
            clip(
                Vec3::new(4.5, 90.0, 4.5),
                Vec3::new(4.5, 94.5, 4.5),
                &shore(vec![])
            ),
            None
        );
        assert_eq!(
            use_boat_item(
                "minecraft:oak_boat",
                Vec3::new(4.5, 90.0, 4.5),
                0.0,
                -90.0,
                BLOCK_INTERACTION_RANGE,
                &shore(vec![])
            ),
            BoatUse::Refused
        );
    }

    /// **The outline, not the collider.** Short grass has an empty collision
    /// shape and a real outline, so a boat aimed at it rests on the grass. A clip
    /// against collision shapes would fall through to the block below and answer
    /// a different `y`.
    #[test]
    fn the_clip_targets_the_outline_so_grass_is_hittable() {
        let grass = BlockPos::new(4, 64, 4);
        let world = shore(vec![(grass, "minecraft:short_grass")]);
        let hit = clip(
            Vec3::new(4.5, 66.0, 4.5),
            Vec3::new(4.5, 66.0 - 4.5, 4.5),
            &world,
        )
        .expect("short grass has a non-empty outline");
        assert_eq!(hit.cell, grass);
        // `short_grass`' outline in 26.2 is **13/16** tall, so the top plane is at
        // 64.8125 — a value neither the collider (none, so 64.0 or the cell below)
        // nor a cell snap (65.0) produces. Read out of the generated census, not
        // guessed: the first version of this gate predicted the plausible round
        // 0.8 and failed by 1/80th of a block.
        let want = 64.0 + 13.0 / 16.0;
        assert!(
            (hit.position.y - want).abs() < 1e-6,
            "expected the grass' own outline top {want}: {:?}",
            hit.position
        );
    }

    /// **A boat with a block in the way is `FAIL`, not a placement.** The
    /// obstruction test needs its own gate because the raytrace succeeding is not
    /// the same question: aiming into a one-cell alcove hits a surface and the
    /// boat (1.375 wide) still does not fit.
    #[test]
    fn a_boat_that_would_overlap_a_block_is_refused() {
        // The shore's top surface is `y = 65` (stone fills `y <= 64` inclusive), so
        // a boat resting on it has its feet at 65.0 and its box spans
        // x ∈ [-4.6875, -3.3125], y ∈ [65, 65.5625] — all air.
        let world = shore(vec![]);
        let mut wrong = Vec::new();
        if boat_box_is_obstructed(Vec3::new(-4.0, 65.0, 4.5), &world) {
            wrong.push("resting exactly on the shore must not read as an overlap");
        }
        // A third of a block lower and the box is inside the stone. The epsilon in
        // the overlap test is what separates these two rows; without it they answer
        // the same.
        if !boat_box_is_obstructed(Vec3::new(-4.0, 64.7, 4.5), &world) {
            wrong.push("sinking into the shore is an overlap");
        }
        // A stone pillar beside the landing point catches the boat's **width**,
        // which a point test at the hit position cannot see. The boat sits well out
        // over the water (x = 1.5, box x ∈ [0.8125, 2.1875]) so the shore behind it
        // cannot be what fires — the control below proves that.
        let pillar = shore(vec![(BlockPos::new(2, 64, 4), "minecraft:stone")]);
        if !boat_box_is_obstructed(Vec3::new(1.5, 64.0, 4.5), &pillar) {
            wrong.push("the boat is 1.375 wide and a neighbouring cell is solid");
        }
        if boat_box_is_obstructed(Vec3::new(1.5, 64.0, 4.5), &world) {
            wrong.push("without the pillar the same box is over open water");
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The end-to-end decision, including the yaw. The yaw is the *player's*, not
    /// derived from the hit: a boat placed while facing north must face north.
    #[test]
    fn a_boat_item_places_at_the_hit_point_facing_the_player() {
        let eye = Vec3::new(4.5, 66.6, 4.5);
        let out = use_boat_item(
            "minecraft:bamboo_raft",
            eye,
            137.0,
            90.0,
            BLOCK_INTERACTION_RANGE,
            &shore(vec![]),
        );
        let BoatUse::Place {
            entity_type,
            position,
            yaw,
        } = out
        else {
            panic!("a raft aimed down at water must place: {out:?}");
        };
        assert_eq!(entity_type.to_string(), "minecraft:bamboo_raft");
        assert!((position.y - WATER_SOURCE_TOP).abs() < 1e-12);
        assert!((yaw - 137.0).abs() < f32::EPSILON);
        // And a non-boat is not this module's business.
        assert_eq!(
            use_boat_item(
                "minecraft:stone",
                eye,
                0.0,
                90.0,
                BLOCK_INTERACTION_RANGE,
                &shore(vec![])
            ),
            BoatUse::NotABoat
        );
    }

    /// Creative reach is 5.0 and survival 4.5, and the discriminating input is a
    /// surface between them. A single-range implementation passes any gate whose
    /// target sits inside 4.5.
    #[test]
    fn creative_reaches_half_a_block_further() {
        // The water surface is at 64.888…; put the eye 4.7 above it, so survival
        // (4.5) cannot reach and creative (5.0) can.
        let eye = Vec3::new(4.5, WATER_SOURCE_TOP + 4.7, 4.5);
        let survival = use_boat_item(
            "minecraft:oak_boat",
            eye,
            0.0,
            90.0,
            block_interaction_range(false),
            &shore(vec![]),
        );
        let creative = use_boat_item(
            "minecraft:oak_boat",
            eye,
            0.0,
            90.0,
            block_interaction_range(true),
            &shore(vec![]),
        );
        assert_eq!(survival, BoatUse::Refused, "4.5 does not reach 4.7 away");
        assert!(
            matches!(creative, BoatUse::Place { .. }),
            "5.0 does: {creative:?}"
        );
    }

    /// **The composition.** A placed boat must reach an entity that is on the
    /// wire, which is what neither [`use_boat_item`] nor `spawn_vehicle` can be
    /// tested for alone. `snapshots()` is the subject deliberately: it is what
    /// `EntityStreamer::sync` diffs into `ADD_ENTITY`.
    #[test]
    fn a_placed_boat_reaches_the_snapshot_set_with_its_yaw() {
        let mobs = crate::MobHandle::new(crate::ChunkWorld::new(0, 128));
        let before = mobs.with(|sim| sim.snapshots().len());
        let applied = apply_boat_item(
            "minecraft:oak_boat",
            Vec3::new(4.5, 66.6, 4.5),
            42.0,
            90.0,
            BLOCK_INTERACTION_RANGE,
            &shore(vec![]),
            &mobs,
        );
        let BoatApplied::Placed {
            entity_id,
            position,
            yaw,
            ..
        } = applied
        else {
            panic!("a boat aimed at water must place: {applied:?}");
        };
        let snapshots = mobs.with(|sim| sim.snapshots());
        assert_eq!(snapshots.len(), before + 1);
        let spawned = snapshots
            .iter()
            .find(|s| s.id == entity_id)
            .expect("the boat must be in the set that becomes ADD_ENTITY");
        assert_eq!(spawned.entity_type.to_string(), "minecraft:oak_boat");
        assert_eq!(spawned.position, position);
        assert!(
            (spawned.rotation.yaw - yaw).abs() < f32::EPSILON,
            "the wire must carry the placing player's yaw, not 0: {:?}",
            spawned.rotation
        );

        // Refused creates nothing. Without this arm the count above is satisfied
        // by an implementation that spawns unconditionally.
        let refused = apply_boat_item(
            "minecraft:oak_boat",
            Vec3::new(4.5, 90.0, 4.5),
            0.0,
            -90.0,
            BLOCK_INTERACTION_RANGE,
            &shore(vec![]),
            &mobs,
        );
        assert_eq!(refused, BoatApplied::Refused);
        assert_eq!(mobs.with(|sim| sim.snapshots().len()), before + 1);
    }
}
