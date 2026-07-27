//! Path nodes, targets, and the [`PathType`] classification with its malus
//! table.
//!
//! [`PathType`] and its per-variant malus are lifted directly from vanilla's
//! `PathType` enum (the malus is the *default* danger weight; a mob may override
//! it, see [`MobShape`](crate::pathfinding::MobShape)). The malus is not a
//! generic "cost 1 per step" — it is vanilla's specific danger model: `-1` means
//! blocked, `8` for water/border, `16` for standing in fire, and so on. These
//! numbers change how a real mob routes, so they are reproduced exactly rather
//! than approximated.

use lodestone_model::BlockPos;

/// A block's traversal classification for a land pathfinder.
///
/// Mirrors vanilla's `PathType`. The associated malus (via
/// [`PathType::malus`]) is the default danger weight; a negative malus marks the
/// type as impassable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PathType {
    /// Impassable.
    Blocked,
    /// Empty air (no floor here).
    Open,
    /// A solid floor a mob can stand on.
    Walkable,
    /// A walkable door the mob may pass.
    WalkableDoor,
    /// A trapdoor / lily pad surface.
    Trapdoor,
    /// Powder snow (impassable by default).
    PowderSnow,
    /// Standing on top of powder snow.
    OnTopOfPowderSnow,
    /// A fence, wall, or closed fence gate (impassable, but climbable by some).
    Fence,
    /// Lava.
    Lava,
    /// Water.
    Water,
    /// A block bordering water.
    WaterBorder,
    /// A rail.
    Rail,
    /// A rail the mob cannot traverse.
    UnpassableRail,
    /// A block neighbouring fire.
    FireInNeighbor,
    /// Standing in fire.
    Fire,
    /// A block neighbouring a damaging block.
    DamagingInNeighbor,
    /// A damaging block (cactus, sweet berries).
    Damaging,
    /// An open door.
    DoorOpen,
    /// A closed wooden door.
    DoorWoodClosed,
    /// A closed iron door.
    DoorIronClosed,
    /// A breach (surface transition for amphibious mobs).
    Breach,
    /// Leaves.
    Leaves,
    /// Sticky honey.
    StickyHoney,
    /// Cocoa.
    Cocoa,
    /// A block a cautious mob avoids.
    DamageCautious,
    /// Standing on top of a trapdoor.
    OnTopOfTrapdoor,
    /// Big mobs close to danger.
    BigMobsCloseToDanger,
}

impl PathType {
    /// The default danger malus for this type (`PathType.getMalus`). A negative
    /// malus means impassable.
    #[must_use]
    pub const fn malus(self) -> f32 {
        match self {
            PathType::Blocked
            | PathType::PowderSnow
            | PathType::Fence
            | PathType::Lava
            | PathType::UnpassableRail
            | PathType::Damaging
            | PathType::DoorWoodClosed
            | PathType::DoorIronClosed
            | PathType::Leaves => -1.0,
            PathType::Open
            | PathType::Walkable
            | PathType::WalkableDoor
            | PathType::Trapdoor
            | PathType::OnTopOfPowderSnow
            | PathType::Rail
            | PathType::DoorOpen
            | PathType::Cocoa
            | PathType::DamageCautious
            | PathType::OnTopOfTrapdoor => 0.0,
            PathType::Water
            | PathType::WaterBorder
            | PathType::FireInNeighbor
            | PathType::DamagingInNeighbor
            | PathType::StickyHoney => 8.0,
            PathType::Breach | PathType::BigMobsCloseToDanger => 4.0,
            PathType::Fire => 16.0,
        }
    }
}

/// A search node at integer block coordinates. Cost/heap state is mutated in
/// place during a search, matching vanilla's `Node`.
#[derive(Debug, Clone)]
pub struct Node {
    /// Block X.
    pub x: i32,
    /// Block Y.
    pub y: i32,
    /// Block Z.
    pub z: i32,
    /// Index in the open-set heap, or `-1` when not queued.
    pub heap_idx: i32,
    /// Cost from start (`g`).
    pub g: f32,
    /// Heuristic to goal (`h`).
    pub h: f32,
    /// Total estimated cost (`f = g + h`).
    pub f: f32,
    /// Arena index of the predecessor, or `usize::MAX` for none.
    pub came_from: usize,
    /// Whether the node has been popped/closed.
    pub closed: bool,
    /// Accumulated walked distance along the path so far.
    pub walked_distance: f32,
    /// This node's danger malus.
    pub cost_malus: f32,
    /// The classified path type at this node.
    pub kind: PathType,
}

/// Sentinel for "no predecessor".
pub const NO_PARENT: usize = usize::MAX;

impl Node {
    /// Creates a fresh, un-queued blocked node.
    #[must_use]
    pub fn new(x: i32, y: i32, z: i32) -> Self {
        Self {
            x,
            y,
            z,
            heap_idx: -1,
            g: 0.0,
            h: 0.0,
            f: 0.0,
            came_from: NO_PARENT,
            closed: false,
            walked_distance: 0.0,
            cost_malus: 0.0,
            kind: PathType::Blocked,
        }
    }

    /// Vanilla's `Node.createHash`, used to deduplicate nodes by position.
    #[must_use]
    pub const fn hash(x: i32, y: i32, z: i32) -> i32 {
        (y & 0xFF)
            | ((x & 32767) << 8)
            | ((z & 32767) << 24)
            | (if x < 0 { i32::MIN } else { 0 })
            | (if z < 0 { 32768 } else { 0 })
    }

    /// Euclidean distance to another node (`float` precision, as vanilla).
    #[must_use]
    pub fn distance_to(&self, other: &Node) -> f32 {
        let xd = (other.x - self.x) as f32;
        let yd = (other.y - self.y) as f32;
        let zd = (other.z - self.z) as f32;
        (xd * xd + yd * yd + zd * zd).sqrt()
    }

    /// Euclidean distance to a block position.
    #[must_use]
    pub fn distance_to_pos(&self, pos: BlockPos) -> f32 {
        let xd = (pos.x - self.x) as f32;
        let yd = (pos.y - self.y) as f32;
        let zd = (pos.z - self.z) as f32;
        (xd * xd + yd * yd + zd * zd).sqrt()
    }

    /// Manhattan distance to a block position.
    #[must_use]
    pub fn distance_manhattan(&self, pos: BlockPos) -> f32 {
        let xd = (pos.x - self.x).abs() as f32;
        let yd = (pos.y - self.y).abs() as f32;
        let zd = (pos.z - self.z).abs() as f32;
        xd + yd + zd
    }

    /// Whether the node is currently in the open set.
    #[must_use]
    pub fn in_open_set(&self) -> bool {
        self.heap_idx >= 0
    }

    /// This node's block position.
    #[must_use]
    pub fn as_block_pos(&self) -> BlockPos {
        BlockPos::new(self.x, self.y, self.z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malus_matches_vanilla_table() {
        assert_eq!(PathType::Blocked.malus(), -1.0);
        assert_eq!(PathType::Open.malus(), 0.0);
        assert_eq!(PathType::Walkable.malus(), 0.0);
        assert_eq!(PathType::Water.malus(), 8.0);
        assert_eq!(PathType::Lava.malus(), -1.0);
        assert_eq!(PathType::Fire.malus(), 16.0);
        assert_eq!(PathType::Breach.malus(), 4.0);
        assert_eq!(PathType::Fence.malus(), -1.0);
        assert_eq!(PathType::DamagingInNeighbor.malus(), 8.0);
    }

    #[test]
    fn hash_is_position_stable_and_distinct() {
        assert_eq!(Node::hash(1, 2, 3), Node::hash(1, 2, 3));
        assert_ne!(Node::hash(1, 2, 3), Node::hash(3, 2, 1));
        assert_ne!(Node::hash(-1, 0, 0), Node::hash(1, 0, 0));
    }

    #[test]
    fn distances() {
        let a = Node::new(0, 0, 0);
        let b = Node::new(3, 0, 4);
        assert_eq!(a.distance_to(&b), 5.0);
        assert_eq!(a.distance_manhattan(BlockPos::new(1, 2, 3)), 6.0);
    }
}
