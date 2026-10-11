//! Cushion rules: what placing a cushion item does, and when a placed cushion
//! survives. The sim half (storage, seats, breaking) is [`crate::mobs`]'s
//! cushion slice; `docs/cushions.md` has the end-to-end picture.
//!
//! The geometry is the block-attached rule: a cushion is a 1 x 0.25 box, centred
//! on its cell and resting at the clicked height. It needs a block shape under
//! it, must not sit buried in colliders, and must not be wholly inside
//! suffocating blocks. Everything reads block state through a closure, like
//! [`crate::boat`].

use lodestone_data::block_states::StateId;
use lodestone_data::{collision_shapes, outline_shapes, tool};
use lodestone_model::{BlockFace, BlockPos, ItemStack, ResourceKey, Vec3, Vec3f};

/// The cushion's box width, in blocks.
pub const WIDTH: f64 = 1.0;
/// The cushion's box height, in blocks.
pub const HEIGHT: f64 = 0.25;

/// Where the seat sits above the cushion's feet: the passenger attachment is the
/// default one, the top of the box.
pub const SEAT_HEIGHT: f64 = HEIGHT;

/// Thickness of the probe under the box that must touch a block shape.
const ANCHOR_PROBE: f64 = 0.015_625;
/// How far below the probe a block shape may start and still count.
const ANCHOR_REACH: f64 = 0.125;
/// The inset applied before asking which cells or colliders a box touches.
const DEFLATE: f64 = 1.0e-7;
/// How far past the click the collision-shape re-aim extends its ray.
const COLLISION_RAY_EPSILON: f64 = 0.001;

const SUFFOCATION_TAG: &str = "minecraft:causes_suffocation";
const FIRE_TAG: &str = "minecraft:fire";
const COLLISION_SHAPE_TAG: &str = "minecraft:cushion_uses_collision_shape";

/// A world-space axis-aligned box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    /// Minimum corner `[x, y, z]`.
    pub min: [f64; 3],
    /// Maximum corner `[x, y, z]`.
    pub max: [f64; 3],
}

impl Aabb {
    /// The box a cushion standing at `at` occupies.
    #[must_use]
    pub fn cushion_at(at: Vec3) -> Self {
        Self {
            min: [at.x - WIDTH / 2.0, at.y, at.z - WIDTH / 2.0],
            max: [at.x + WIDTH / 2.0, at.y + HEIGHT, at.z + WIDTH / 2.0],
        }
    }

    pub(crate) fn deflated(self, by: f64) -> Self {
        Self {
            min: self.min.map(|v| v + by),
            max: self.max.map(|v| v - by),
        }
    }

    /// Whether the open interiors overlap: boxes that only share a face do not.
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        (0..3).all(|a| self.min[a] < other.max[a] && self.max[a] > other.min[a])
    }

    pub(crate) fn shifted(self, cell: [i32; 3]) -> Self {
        let d = cell.map(f64::from);
        Self {
            min: [self.min[0] + d[0], self.min[1] + d[1], self.min[2] + d[2]],
            max: [self.max[0] + d[0], self.max[1] + d[1], self.max[2] + d[2]],
        }
    }

    /// Every cell from the floor of the minimum to the floor of the maximum.
    pub(crate) fn cells(&self) -> impl Iterator<Item = (i32, i32, i32)> {
        let lo = self.min.map(|v| v.floor() as i32);
        let hi = self.max.map(|v| v.floor() as i32);
        (lo[0]..=hi[0]).flat_map(move |x| {
            (lo[1]..=hi[1]).flat_map(move |y| (lo[2]..=hi[2]).map(move |z| (x, y, z)))
        })
    }
}

/// The dye ordinal a cushion item path names (`red_cushion` is 14).
fn color_of_path(path: &str) -> Option<u8> {
    let name = path.strip_suffix("_cushion")?;
    crate::mobs::appearance::DYE_NAMES
        .iter()
        .position(|dye| *dye == name)
        .map(|i| i as u8)
}

/// The colour a stack places: its `cushion/color` component when present, else
/// the colour its item carries by default. `None` for a non-cushion item.
#[must_use]
pub fn color_for_stack(stack: &ItemStack) -> Option<u8> {
    if stack.item.namespace() != "minecraft" {
        return None;
    }
    let item_color = color_of_path(stack.item.path())?;
    let patched = stack
        .components
        .cushion_color
        .as_deref()
        .and_then(|name| crate::mobs::appearance::DYE_NAMES.iter().position(|dye| *dye == name));
    Some(patched.map_or(item_color, |i| i as u8))
}

/// The item a cushion of `color` drops and is picked as.
#[must_use]
pub fn item_for_color(color: u8) -> ResourceKey {
    let name = crate::mobs::appearance::DYE_NAMES[usize::from(color & 0x0F)];
    ResourceKey::new("minecraft", &format!("{name}_cushion")).expect("a dye name makes a valid key")
}

/// The yaw a cushion is placed with: the placing player's yaw rounded to the
/// nearest quarter turn, as south 0, west 90, north 180, east 270.
#[must_use]
pub fn snapped_yaw(player_yaw: f32) -> f32 {
    let quarter = ((f64::from(player_yaw) / 90.0 + 0.5).floor() as i32) & 3;
    quarter as f32 * 90.0
}

/// Whether any block shape's bounds touch the probe under the box.
fn has_anchor_below(bb: &Aabb, state: &dyn Fn(i32, i32, i32) -> StateId) -> bool {
    let probe = Aabb {
        min: [bb.min[0], bb.min[1] - ANCHOR_PROBE, bb.min[2]],
        max: [bb.max[0].next_down(), bb.min[1], bb.max[2].next_down()],
    };
    let mut search = probe;
    search.min[1] -= ANCHOR_REACH;
    search.cells().any(|(x, y, z)| {
        let boxes = outline_shapes::outline_boxes(state(x, y, z));
        if boxes.is_empty() {
            return false;
        }
        let mut bounds = Aabb { min: [f64::MAX; 3], max: [f64::MIN; 3] };
        for b in boxes {
            for a in 0..3 {
                bounds.min[a] = bounds.min[a].min(f64::from(b.min[a]));
                bounds.max[a] = bounds.max[a].max(f64::from(b.max[a]));
            }
        }
        bounds.shifted([x, y, z]).intersects(&probe)
    })
}

/// Whether colliders cover the whole thin slice at the box's base.
fn is_anchor_buried(bb: &Aabb, state: &dyn Fn(i32, i32, i32) -> StateId) -> bool {
    let slice = Aabb {
        min: bb.min,
        max: [bb.max[0], bb.min[1] + ANCHOR_PROBE, bb.max[2]],
    }
    .deflated(DEFLATE);
    let mut reach = slice;
    for a in 0..3 {
        reach.min[a] -= 1.0;
        reach.max[a] += 1.0;
    }
    let mut covering = Vec::new();
    for (x, y, z) in reach.cells() {
        for b in collision_shapes::collision_boxes(state(x, y, z)) {
            let world = Aabb {
                min: b.min.map(f64::from),
                max: b.max.map(f64::from),
            }
            .shifted([x, y, z]);
            if world.intersects(&slice) {
                covering.push(world);
            }
        }
    }
    covered_by(&slice, &covering)
}

/// Whether the union of `covering` contains every point of `region`. Compresses
/// the coordinates the boxes cut the region at and tests each resulting cell.
fn covered_by(region: &Aabb, covering: &[Aabb]) -> bool {
    let cuts: [Vec<f64>; 3] = std::array::from_fn(|a| {
        let mut v = vec![region.min[a], region.max[a]];
        for b in covering {
            v.extend([b.min[a], b.max[a]].into_iter().filter(|c| *c > region.min[a] && *c < region.max[a]));
        }
        v.sort_by(f64::total_cmp);
        v.dedup();
        v
    });
    for xs in cuts[0].windows(2) {
        for ys in cuts[1].windows(2) {
            for zs in cuts[2].windows(2) {
                let mid = [
                    f64::midpoint(xs[0], xs[1]),
                    f64::midpoint(ys[0], ys[1]),
                    f64::midpoint(zs[0], zs[1]),
                ];
                let inside = covering
                    .iter()
                    .any(|b| (0..3).all(|a| b.min[a] <= mid[a] && mid[a] <= b.max[a]));
                if !inside {
                    return false;
                }
            }
        }
    }
    true
}

/// Whether the state suffocates: in the suffocation tag with a full-cube collider.
fn is_suffocating(state: StateId) -> bool {
    tool::block_tag_contains(SUFFOCATION_TAG, state.block())
        && collision_shapes::collision_boxes(state).iter().any(|b| {
            (0..3).all(|a| b.min[a] <= 1.0e-4 && b.max[a] >= 1.0 - 1.0e-4)
        })
}

/// Whether the cushion box could stay where it is: an anchor under it, and not
/// every cell of it a suffocating block.
#[must_use]
pub fn would_survive_at(bb: &Aabb, state: &dyn Fn(i32, i32, i32) -> StateId) -> bool {
    has_anchor_below(bb, state)
        && !bb.deflated(DEFLATE).cells().all(|(x, y, z)| is_suffocating(state(x, y, z)))
}

/// Whether a new cushion may be placed in `bb`: it survives and is not buried.
#[must_use]
pub fn can_be_placed_at(bb: &Aabb, state: &dyn Fn(i32, i32, i32) -> StateId) -> bool {
    would_survive_at(bb, state) && !is_anchor_buried(bb, state)
}

/// Whether any fire block touches the box.
#[must_use]
pub fn fire_in(bb: &Aabb, state: &dyn Fn(i32, i32, i32) -> StateId) -> bool {
    bb.deflated(DEFLATE)
        .cells()
        .any(|(x, y, z)| tool::block_tag_contains(FIRE_TAG, state(x, y, z).block()))
}

/// What a cushion item does against a clicked block.
#[derive(Debug, Clone, PartialEq)]
pub enum CushionUse {
    /// Nothing is placed and the stack is kept.
    Refused,
    /// Place a cushion standing at `position`, facing `yaw`, of `color`.
    Place {
        /// Where the cushion stands: the clicked cell's centre at the click height.
        position: Vec3,
        /// The snapped placement yaw, in degrees.
        yaw: f32,
        /// The dye ordinal.
        color: u8,
    },
}

/// Re-aims a click on a collision-shape block (cauldron, hopper, composter) at
/// the block's collider, so the cushion rests on what the player sees stood on
/// rather than the outline. Returns the new click point and face, or `None` to
/// keep the original.
fn reaim_at_collision_shape(
    clicked: BlockPos,
    click: Vec3,
    eye: Vec3,
    state: StateId,
) -> Option<(Vec3, BlockFace)> {
    let ray = Vec3::new(click.x - eye.x, click.y - eye.y, click.z - eye.z);
    let len = (ray.x * ray.x + ray.y * ray.y + ray.z * ray.z).sqrt();
    if len == 0.0 {
        return None;
    }
    let to = Vec3::new(
        click.x + ray.x / len * COLLISION_RAY_EPSILON,
        click.y + ray.y / len * COLLISION_RAY_EPSILON,
        click.z + ray.z / len * COLLISION_RAY_EPSILON,
    );
    let d = [to.x - eye.x, to.y - eye.y, to.z - eye.z];
    let origin = [eye.x, eye.y, eye.z];
    let cell = [clicked.x, clicked.y, clicked.z];
    let mut best: Option<(f64, usize)> = None;
    for b in collision_shapes::collision_boxes(state) {
        let world = Aabb {
            min: b.min.map(f64::from),
            max: b.max.map(f64::from),
        }
        .shifted(cell);
        for axis in 0..3 {
            if d[axis].abs() < 1.0e-12 {
                continue;
            }
            let plane = if d[axis] > 0.0 { world.min[axis] } else { world.max[axis] };
            let t = (plane - origin[axis]) / d[axis];
            if !(0.0..=1.0).contains(&t) || best.is_some_and(|(bt, _)| t >= bt) {
                continue;
            }
            let on_box = (0..3).filter(|o| *o != axis).all(|o| {
                let p = origin[o] + t * d[o];
                world.min[o] - 1.0e-7 <= p && p <= world.max[o] + 1.0e-7
            });
            if on_box {
                best = Some((t, axis));
            }
        }
    }
    let (t, axis) = best?;
    let point = Vec3::new(origin[0] + t * d[0], origin[1] + t * d[1], origin[2] + t * d[2]);
    let face = match (axis, d[axis] > 0.0) {
        (0, true) => BlockFace::West,
        (0, false) => BlockFace::East,
        (1, true) => BlockFace::Down,
        (1, false) => BlockFace::Up,
        (2, true) => BlockFace::North,
        _ => BlockFace::South,
    };
    Some((point, face))
}

/// The cushion item's use against a block: the top face only, resting at the
/// click height in the cell above (or the clicked cell itself when that is
/// replaceable), where [`can_be_placed_at`] holds.
///
/// `eye` is the placing player's eye, used to re-aim a click on a
/// collision-shape block; `None` skips the re-aim. The caller still has to
/// refuse a spot another cushion occupies.
#[must_use]
pub fn use_cushion_item(
    color: u8,
    clicked: BlockPos,
    face: BlockFace,
    cursor: Vec3f,
    eye: Option<Vec3>,
    player_yaw: f32,
    state: &dyn Fn(i32, i32, i32) -> StateId,
) -> CushionUse {
    let clicked_state = state(clicked.x, clicked.y, clicked.z);
    let mut click = Vec3::new(
        f64::from(clicked.x) + f64::from(cursor.x),
        f64::from(clicked.y) + f64::from(cursor.y),
        f64::from(clicked.z) + f64::from(cursor.z),
    );
    let mut face = face;
    if tool::block_tag_contains(COLLISION_SHAPE_TAG, clicked_state.block())
        && let Some(eye) = eye
        && let Some((point, new_face)) = reaim_at_collision_shape(clicked, click, eye, clicked_state)
    {
        click = point;
        face = new_face;
    }
    if face != BlockFace::Up {
        return CushionUse::Refused;
    }
    let cell = if crate::block_placement::is_replaceable_for_placement(clicked_state) {
        clicked
    } else {
        BlockPos::new(clicked.x, clicked.y + 1, clicked.z)
    };
    let position = Vec3::new(f64::from(cell.x) + 0.5, click.y, f64::from(cell.z) + 0.5);
    if !can_be_placed_at(&Aabb::cushion_at(position), state) {
        return CushionUse::Refused;
    }
    CushionUse::Place {
        position,
        yaw: snapped_yaw(player_yaw),
        color,
    }
}

/// What [`apply_cushion_item`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum CushionApplied {
    /// The stack is not a cushion; the caller carries on.
    NotACushion,
    /// Nothing was placed and the stack is kept.
    Refused,
    /// A cushion was created; the caller consumes one item.
    Placed {
        /// The new entity's network id.
        entity_id: i32,
        /// Where it stands.
        position: Vec3,
        /// Its yaw, in degrees.
        yaw: f32,
        /// Its dye ordinal.
        color: u8,
        /// Whether fire in its box broke it at once; the item still drops.
        burned: bool,
    },
}

/// [`use_cushion_item`] plus the spawn: refuses a spot another cushion
/// occupies, creates the entity, and breaks it at once when fire touches it.
#[must_use]
pub fn apply_cushion_item(
    stack: &ItemStack,
    clicked: BlockPos,
    face: BlockFace,
    cursor: Vec3f,
    eye: Option<Vec3>,
    player_yaw: f32,
    state: &dyn Fn(i32, i32, i32) -> StateId,
    mobs: &crate::mobs::MobHandle,
) -> CushionApplied {
    let Some(color) = color_for_stack(stack) else {
        return CushionApplied::NotACushion;
    };
    let CushionUse::Place { position, yaw, color } =
        use_cushion_item(color, clicked, face, cursor, eye, player_yaw, state)
    else {
        return CushionApplied::Refused;
    };
    let bb = Aabb::cushion_at(position);
    let burned = fire_in(&bb, state);
    let placed = mobs.with(|sim| {
        if sim.cushion_occupies(&bb) {
            return None;
        }
        let id = sim.spawn_cushion(position, yaw, color);
        if burned {
            sim.break_cushion(id, true);
        }
        Some(id)
    });
    match placed {
        Some(entity_id) => CushionApplied::Placed { entity_id, position, yaw, color, burned },
        None => CushionApplied::Refused,
    }
}

/// Publishes one cushion sound at `pos` to every player. The acting client
/// predicts none of them, so none is withheld from it.
pub(crate) fn publish_sound(feed: &crate::BlockTickFeed, kind: crate::effects::CushionSound, pos: Vec3) {
    // The seed only picks between a sound event's variants; the position bits
    // vary it per cushion without a random source.
    let seed = (pos.x.to_bits() ^ pos.y.to_bits().rotate_left(17) ^ pos.z.to_bits().rotate_left(34)) as i64;
    if let Some(effect) = crate::effects::cushion_sound(kind, pos, seed) {
        feed.publish_effect(effect);
    }
}

/// Publishes what a breaking cushion gives off: its sound and the wool burst.
pub(crate) fn publish_broken(feed: &crate::BlockTickFeed, pos: Vec3, color: u8) {
    publish_sound(feed, crate::effects::CushionSound::Break, pos);
    if let Some(effect) = crate::effects::cushion_break_particles(pos, color) {
        feed.publish_effect(effect);
    }
}

#[cfg(test)]
mod tests;
