//! Item-frame rules: where a frame sits, whether it survives there, and what a
//! frame item does against a clicked block. The sim half (storage, items,
//! rotation, breaking) is [`crate::mobs`]'s item-frame slice;
//! `docs/item-frames.md` has the end-to-end picture.
//!
//! A frame occupies the air cell in front of the clicked face and faces
//! outwards. Its box is a 1/16-thick slab on the wall side of that cell:
//! 0.75 square, or a full block when it holds a map. The same box shrunk back
//! to 0.75 (the "pop box") is what survival tests against.

use lodestone_data::block_states::StateId;
use lodestone_data::{block_solidity, collision_shapes};
use lodestone_model::{BlockFace, BlockPos, ItemStack, ResourceKey, Vec3};

use crate::cushion::Aabb;

/// Eighth turns in a full rotation of the framed item.
pub const NUM_ROTATIONS: u8 = 8;

/// How far the box centre sits from the cell centre towards the wall.
const WALL_SHIFT: f64 = 0.46875;
const THICKNESS: f64 = 0.0625;
const BARE_SIZE: f64 = 0.75;
const MAP_SIZE: f64 = 1.0;
/// The inset applied before asking which cells or colliders a box touches.
const DEFLATE: f64 = 1.0e-7;
/// Where the dropped item appears, out from the frame along its facing.
const DROP_OFFSET: f64 = 0.15;

/// The unit step of a face.
#[must_use]
pub fn step(face: BlockFace) -> [i32; 3] {
    match face {
        BlockFace::Down => [0, -1, 0],
        BlockFace::Up => [0, 1, 0],
        BlockFace::North => [0, 0, -1],
        BlockFace::South => [0, 0, 1],
        BlockFace::West => [-1, 0, 0],
        BlockFace::East => [1, 0, 0],
    }
}

/// The face looking the other way.
#[must_use]
pub fn opposite(face: BlockFace) -> BlockFace {
    match face {
        BlockFace::Down => BlockFace::Up,
        BlockFace::Up => BlockFace::Down,
        BlockFace::North => BlockFace::South,
        BlockFace::South => BlockFace::North,
        BlockFace::West => BlockFace::East,
        BlockFace::East => BlockFace::West,
    }
}

/// The cell one step from `pos` towards `face`.
#[must_use]
pub fn relative(pos: BlockPos, face: BlockFace) -> BlockPos {
    let [dx, dy, dz] = step(face);
    BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
}

/// The wire and save value of a facing: down 0, up 1, north 2, south 3, west 4, east 5.
#[must_use]
pub fn data_3d(face: BlockFace) -> u8 {
    match face {
        BlockFace::Down => 0,
        BlockFace::Up => 1,
        BlockFace::North => 2,
        BlockFace::South => 3,
        BlockFace::West => 4,
        BlockFace::East => 5,
    }
}

/// The facing a wire or save value names.
#[must_use]
pub fn from_data_3d(value: i32) -> Option<BlockFace> {
    Some(match value {
        0 => BlockFace::Down,
        1 => BlockFace::Up,
        2 => BlockFace::North,
        3 => BlockFace::South,
        4 => BlockFace::West,
        5 => BlockFace::East,
        _ => return None,
    })
}

/// A horizontal facing's quarter turns from south (south 0, west 1, north 2,
/// east 3), or -1 for a floor or ceiling frame.
#[must_use]
pub fn data_2d(face: BlockFace) -> i32 {
    match face {
        BlockFace::South => 0,
        BlockFace::West => 1,
        BlockFace::North => 2,
        BlockFace::East => 3,
        BlockFace::Down | BlockFace::Up => -1,
    }
}

/// The entity yaw and pitch for a facing: horizontal frames turn by quarter
/// turns with no pitch; a floor frame pitches 90 down-facing and a ceiling
/// frame -90 up-facing.
#[must_use]
pub fn yaw_pitch(face: BlockFace) -> (f32, f32) {
    match face {
        BlockFace::Up => (0.0, -90.0),
        BlockFace::Down => (0.0, 90.0),
        horizontal => (data_2d(horizontal) as f32 * 90.0, 0.0),
    }
}

/// The item a frame of this kind drops and is picked as.
#[must_use]
pub fn item_key(glow: bool) -> ResourceKey {
    ResourceKey::new("minecraft", if glow { "glow_item_frame" } else { "item_frame" })
        .expect("a frame item name is a valid key")
}

/// The entity type of a frame of this kind.
#[must_use]
pub fn entity_key(glow: bool) -> ResourceKey {
    item_key(glow)
}

/// Whether `stack` places a frame, and whether it is the glowing kind.
#[must_use]
pub fn glow_of_item(stack: &ItemStack) -> Option<bool> {
    if stack.item.namespace() != "minecraft" {
        return None;
    }
    match stack.item.path() {
        "item_frame" => Some(false),
        "glow_item_frame" => Some(true),
        _ => None,
    }
}

/// The frame entity's own position: the integer corner of the cell it hangs
/// in. This is what the spawn packet carries; the box centre is [`centre`].
#[must_use]
pub fn anchor(cell: BlockPos) -> Vec3 {
    Vec3::new(f64::from(cell.x), f64::from(cell.y), f64::from(cell.z))
}

/// The centre of the frame's box: the cell centre pushed to the wall.
#[must_use]
pub fn centre(cell: BlockPos, face: BlockFace) -> Vec3 {
    let [dx, dy, dz] = step(face);
    Vec3::new(
        f64::from(cell.x) + 0.5 - f64::from(dx) * WALL_SHIFT,
        f64::from(cell.y) + 0.5 - f64::from(dy) * WALL_SHIFT,
        f64::from(cell.z) + 0.5 - f64::from(dz) * WALL_SHIFT,
    )
}

/// The cell holding a point, for restoring a frame from its saved box centre.
#[must_use]
pub fn cell_containing(point: Vec3) -> BlockPos {
    BlockPos::new(point.x.floor() as i32, point.y.floor() as i32, point.z.floor() as i32)
}

/// Where the item popped out of a frame appears.
#[must_use]
pub fn drop_position(cell: BlockPos, face: BlockFace) -> Vec3 {
    let c = centre(cell, face);
    let [dx, _, dz] = step(face);
    Vec3::new(c.x + f64::from(dx) * DROP_OFFSET, c.y, c.z + f64::from(dz) * DROP_OFFSET)
}

fn slab(cell: BlockPos, face: BlockFace, side: f64) -> Aabb {
    let c = centre(cell, face);
    let [dx, dy, dz] = step(face);
    let half = |axis_step: i32| if axis_step == 0 { side / 2.0 } else { THICKNESS / 2.0 };
    let (hx, hy, hz) = (half(dx), half(dy), half(dz));
    Aabb { min: [c.x - hx, c.y - hy, c.z - hz], max: [c.x + hx, c.y + hy, c.z + hz] }
}

/// A frame's bounding box: 0.75 square, or a full block across when it holds a map.
#[must_use]
pub fn bounding_box(cell: BlockPos, face: BlockFace, has_map: bool) -> Aabb {
    slab(cell, face, if has_map { MAP_SIZE } else { BARE_SIZE })
}

/// The box survival tests: always the bare size, whatever the frame holds.
#[must_use]
pub fn pop_box(cell: BlockPos, face: BlockFace) -> Aabb {
    slab(cell, face, BARE_SIZE)
}

/// Another frame, as far as coexistence cares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Neighbour {
    /// The cell it hangs in.
    pub cell: BlockPos,
    /// Its facing.
    pub facing: BlockFace,
    /// Whether it holds a filled map, which widens its box.
    pub has_map: bool,
}

fn blocked_by_blocks(bb: &Aabb, state: &dyn Fn(i32, i32, i32) -> StateId) -> bool {
    let bb = bb.deflated(DEFLATE);
    bb.cells().any(|(x, y, z)| {
        collision_shapes::collision_boxes(state(x, y, z)).iter().any(|b| {
            Aabb { min: b.min.map(f64::from), max: b.max.map(f64::from) }.shifted([x, y, z]).intersects(&bb)
        })
    })
}

/// Whether a frame at `cell` facing `face` can stay: nothing solid in its
/// slab, a solid wall (or, on a horizontal frame, a repeater or comparator)
/// behind it, and no other frame facing the same way across it. `others`
/// excludes the frame itself.
#[must_use]
pub fn survives(
    cell: BlockPos,
    face: BlockFace,
    state: &dyn Fn(i32, i32, i32) -> StateId,
    others: &[Neighbour],
) -> bool {
    let pop = pop_box(cell, face);
    if blocked_by_blocks(&pop, state) {
        return false;
    }
    let wall = relative(cell, opposite(face));
    let behind = state(wall.x, wall.y, wall.z);
    let supported = block_solidity::legacy_solid(behind)
        || (data_2d(face) >= 0 && crate::redstone::is_diode(behind));
    supported
        && !others.iter().any(|other| {
            other.facing == face && bounding_box(other.cell, other.facing, other.has_map).intersects(&pop)
        })
}

/// What a frame item does against a clicked block.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameUse {
    /// Nothing is placed and the stack is kept.
    Refused,
    /// Hang a frame in `cell`, facing `facing`.
    Place {
        /// The air cell in front of the clicked face.
        cell: BlockPos,
        /// The clicked face.
        facing: BlockFace,
    },
}

/// The frame item's use against a block: allowed for a player who may build,
/// inside the world's height, where the frame would survive.
#[must_use]
pub fn use_frame_item(
    clicked: BlockPos,
    face: BlockFace,
    may_build: bool,
    build_range: (i32, i32),
    state: &dyn Fn(i32, i32, i32) -> StateId,
    others: &[Neighbour],
) -> FrameUse {
    let cell = relative(clicked, face);
    if !may_build || cell.y < build_range.0 || cell.y >= build_range.1 {
        return FrameUse::Refused;
    }
    if survives(cell, face, state, others) {
        FrameUse::Place { cell, facing: face }
    } else {
        FrameUse::Refused
    }
}

/// What [`apply_frame_item`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameApplied {
    /// The stack is not a frame; the caller carries on.
    NotAFrame,
    /// Nothing was placed and the stack is kept.
    Refused,
    /// A frame was created; the caller consumes one item.
    Placed {
        /// The new entity's network id.
        entity_id: i32,
        /// The box centre, where the sounds play.
        centre: Vec3,
        /// Whether it is the glowing kind.
        glow: bool,
    },
}

/// [`use_frame_item`] plus the spawn.
#[must_use]
pub fn apply_frame_item(
    stack: &ItemStack,
    clicked: BlockPos,
    face: BlockFace,
    may_build: bool,
    build_range: (i32, i32),
    state: &dyn Fn(i32, i32, i32) -> StateId,
    mobs: &crate::mobs::MobHandle,
) -> FrameApplied {
    let Some(glow) = glow_of_item(stack) else {
        return FrameApplied::NotAFrame;
    };
    mobs.with(|sim| {
        let others = sim.frame_neighbours(None);
        match use_frame_item(clicked, face, may_build, build_range, state, &others) {
            FrameUse::Refused => FrameApplied::Refused,
            FrameUse::Place { cell, facing } => FrameApplied::Placed {
                entity_id: sim.spawn_item_frame(cell, facing, glow),
                centre: centre(cell, facing),
                glow,
            },
        }
    })
}

/// The four sounds a frame makes, by kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameSound {
    /// The frame was hung.
    Place,
    /// An item went in.
    AddItem,
    /// The item turned an eighth.
    Rotate,
    /// The item was knocked out.
    RemoveItem,
    /// The frame broke.
    Break,
}

/// Publishes one frame sound at `pos` to every player. Frames play their
/// sounds at full volume and pitch in the neutral category, and the acting
/// client predicts none of them.
pub(crate) fn publish_sound(feed: &crate::BlockTickFeed, kind: FrameSound, glow: bool, pos: Vec3) {
    let seed = (pos.x.to_bits() ^ pos.y.to_bits().rotate_left(17) ^ pos.z.to_bits().rotate_left(34)) as i64;
    if let Some(effect) = crate::effects::item_frame_sound(kind, glow, pos, seed) {
        feed.publish_effect(effect);
    }
}

/// Tells a map it is no longer shown in the frame that held it.
pub(crate) fn forget_map(maps: &crate::maps::MapHandle, map: Option<crate::mobs::FramedMapRef>) {
    if let Some(map) = map {
        maps.removed_from_frame(map.map_id, map.cell, map.entity_id);
    }
}

/// Publishes what a breaking frame gives off, and drops its map marker.
pub(crate) fn publish_broken(
    feed: &crate::BlockTickFeed,
    maps: &crate::maps::MapHandle,
    broken: crate::mobs::FrameBroken,
) {
    publish_sound(feed, FrameSound::Break, broken.glow, broken.centre);
    forget_map(maps, broken.map);
}

#[cfg(test)]
mod tests;
