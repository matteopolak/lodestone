//! Nether portals: frame detection, ignition, the destination search, and the
//! per-player transition counter.
//!
//! # What it is
//!
//! The server half of "portals work". Four independent pieces, each a port of one
//! vanilla type:
//!
//! | here | vanilla |
//! |---|---|
//! | [`find_empty_portal_shape`] / [`PortalShape`] | `PortalShape` |
//! | [`ignite`] | `BaseFireBlock.onPlace`'s portal branch |
//! | [`find_exit_portal`] / [`create_portal`] | `PortalForcer` |
//! | [`PortalTracker`] | `Entity.portalProcess` + `PortalProcessor` |
//!
//! Everything here is a pure function of a [`ChunkSource`] plus coordinates, so it
//! is drivable from a test with no connection, no protocol and no runtime — which
//! is what `tests/nether_portal_round_trip.rs` does.
//!
//! # How it works
//!
//! **Ignition.** A fire source used on a block calls [`ignite`], which runs
//! vanilla's frame search from the clicked cell on the **X axis first** (vanilla's
//! own preferred axis in `BaseFireBlock.onPlace`), falls back to Z, and requires
//! the shape be *valid* (2–21 wide, 3–21 tall) **and hold zero portal blocks**
//! already. On success it returns the `nether_portal` states to write, so the
//! caller owns both the `set_block` fan-out and the block updates the client needs.
//!
//! **Travel.** [`PortalTracker::tick`] is fed "am I standing in a portal this
//! tick" once per server tick. It counts up while inside and *decays by four* per
//! tick while outside — vanilla's `PortalProcessor.decayTick`, not a reset — and
//! fires once the count reaches the transition time the game rules supply. After a
//! trip it holds a 10-tick cooldown (`Player.getDimensionChangingDelay`) so the
//! destination portal the player materialises inside does not immediately send
//! them back.
//!
//! **Destination.** [`find_exit_portal`] looks for an existing portal near the
//! scaled arrival point; [`create_portal`] builds a fresh 2×3 one when there is
//! none, in a spot chosen the way `PortalForcer.createPortal` chooses it.
//!
//! # How to change it
//!
//! * **The frame search's bounds are exact and the constants are not decoration.**
//!   2–21 wide and 3–21 tall, and the vertical walk starts by descending at most
//!   21 blocks from the clicked cell. A "2×3 only" implementation lights a normal
//!   portal correctly and refuses every larger one, which reads as a client bug.
//! * **[`is_empty`] includes `nether_portal` itself.** That is what makes a portal
//!   re-lightable and what makes `numPortalBlocks` meaningful: the interior scan
//!   walks *through* existing portal blocks counting them, rather than stopping at
//!   the first one.
//! * **[`PortalIndex`] is this crate's stand-in for vanilla's POI manager**, and it
//!   is the reason the destination search is affordable. Anything that creates a
//!   portal must publish it here; a portal missing from the index is a portal the
//!   return trip will build a duplicate beside.
//!
//! ## Gotchas
//!
//! * **The tracker's counter decays rather than resetting.** Vanilla subtracts 4
//!   per tick outside, so brushing past a portal repeatedly still accumulates. A
//!   reset-to-zero implementation makes the creative-mode 0-tick delay behave
//!   identically and the survival 80-tick delay subtly wrong.
//! * **`portalTime++ >= transitionTime` is a post-increment comparison**, so a
//!   transition time of 80 fires on the **81st** consecutive tick inside, and a
//!   transition time of 0 fires on the first. Reading it as a pre-increment makes
//!   creative mode take one tick too long — invisible — and survival one tick
//!   short, also invisible.
//! * **[`create_portal`] writes obsidian *and* air.** Its fallback arm carves the
//!   2×2×4 interior out of whatever was there, because the spot it picked was only
//!   required to be *clampable*, not empty. Writing the frame and not the interior
//!   leaves a portal embedded in netherrack that the frame search then rejects.
//!
//! # Dependencies
//!
//! [`crate::ChunkSource`] and [`crate::dimension`]. No protocol: the packets a
//! trip produces are `crate::server`'s business.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_model::BlockPos;

use crate::chunk::ChunkSource;
use crate::dimension::Dimension;

/// The narrowest a portal frame's interior may be (`PortalShape.MIN_WIDTH`).
pub const MIN_WIDTH: i32 = 2;
/// The widest (`PortalShape.MAX_WIDTH`).
pub const MAX_WIDTH: i32 = 21;
/// The shortest (`PortalShape.MIN_HEIGHT`).
pub const MIN_HEIGHT: i32 = 3;
/// The tallest (`PortalShape.MAX_HEIGHT`).
pub const MAX_HEIGHT: i32 = 21;

/// The block a portal frame is made of (`PortalShape.FRAME`).
pub const FRAME_BLOCK: &str = "minecraft:obsidian";

/// Ticks a player is immune to portals for after a trip —
/// `Player.getDimensionChangingDelay`, which is 10 and **not** the 300 every other
/// entity gets.
pub const PLAYER_PORTAL_COOLDOWN: i32 = 10;

/// The horizontal axis a portal's plane lies in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// The portal's face is perpendicular to X — `axis=x`.
    X,
    /// `axis=z`.
    Z,
}

impl Axis {
    /// The property value the block state carries.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Z => "z",
        }
    }

    /// Parses the `axis` property off a `nether_portal` state string, defaulting to
    /// `X` — vanilla's own `getOptionalValue(AXIS).orElse(Direction.Axis.X)`.
    #[must_use]
    pub fn from_state(state: &str) -> Self {
        if state.contains("axis=z") {
            Self::Z
        } else {
            Self::X
        }
    }

    /// The other one.
    #[must_use]
    pub fn other(self) -> Self {
        match self {
            Self::X => Self::Z,
            Self::Z => Self::X,
        }
    }

    /// `PortalShape.findAnyShape`'s `rightDir`: **WEST** for the X axis and
    /// **SOUTH** for Z. The asymmetry is vanilla's and is load-bearing — swapping
    /// it mirrors the frame search, so an off-centre frame is found on one axis and
    /// missed on the other.
    #[must_use]
    fn right(self) -> (i32, i32) {
        match self {
            Self::X => (-1, 0),
            Self::Z => (0, 1),
        }
    }

    /// The positive direction *along* the axis — `Direction.get(POSITIVE, axis)`,
    /// which `PortalForcer.createPortal` uses to lay the frame out.
    #[must_use]
    fn positive(self) -> (i32, i32) {
        match self {
            Self::X => (1, 0),
            Self::Z => (0, 1),
        }
    }

    /// `direction.getClockWise()` for [`positive`](Self::positive) — the axis the
    /// created portal's *thickness* box is measured along.
    #[must_use]
    fn positive_clockwise(self) -> (i32, i32) {
        match self {
            // EAST.getClockWise() == SOUTH
            Self::X => (0, 1),
            // SOUTH.getClockWise() == WEST
            Self::Z => (-1, 0),
        }
    }
}

/// The block state a portal cell holds, for `axis`.
#[must_use]
pub fn portal_state(axis: Axis) -> String {
    format!("minecraft:nether_portal[axis={}]", axis.name())
}

/// Whether `state` is a portal block of any axis.
#[must_use]
pub fn is_portal(state: &str) -> bool {
    state == "minecraft:nether_portal" || state.starts_with("minecraft:nether_portal[")
}

/// Whether `state` is a frame block.
#[must_use]
pub fn is_frame(state: &str) -> bool {
    state == FRAME_BLOCK || state.starts_with("minecraft:obsidian[")
}

/// `PortalShape.isEmpty`: air, anything in `#minecraft:fire`, or an existing
/// portal block.
///
/// **The portal arm is not a convenience.** Without it the interior scan stops at
/// the first portal block, `numPortalBlocks` never exceeds zero, and
/// [`PortalShape::is_complete`] — which `updateShape` uses to decide whether a
/// portal should survive a neighbour change — is false for every real portal.
#[must_use]
pub fn is_empty(state: &str) -> bool {
    state == "minecraft:air"
        || state == "minecraft:cave_air"
        || state == "minecraft:void_air"
        || state.starts_with("minecraft:fire")
        || state.starts_with("minecraft:soul_fire")
        || is_portal(state)
}

/// A candidate portal frame found around some cell — vanilla's `PortalShape`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalShape {
    axis: Axis,
    right: (i32, i32),
    portal_blocks: i32,
    bottom_left: BlockPos,
    width: i32,
    height: i32,
}

impl PortalShape {
    /// The plane the portal lies in.
    #[must_use]
    pub fn axis(&self) -> Axis {
        self.axis
    }

    /// The lower-left interior cell.
    #[must_use]
    pub fn bottom_left(&self) -> BlockPos {
        self.bottom_left
    }

    /// Interior width in cells.
    #[must_use]
    pub fn width(&self) -> i32 {
        self.width
    }

    /// Interior height in cells.
    #[must_use]
    pub fn height(&self) -> i32 {
        self.height
    }

    /// How many interior cells already hold a portal block.
    #[must_use]
    pub fn portal_blocks(&self) -> i32 {
        self.portal_blocks
    }

    /// `PortalShape.isValid` — the size bounds, and nothing else.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        (MIN_WIDTH..=MAX_WIDTH).contains(&self.width)
            && (MIN_HEIGHT..=MAX_HEIGHT).contains(&self.height)
    }

    /// `PortalShape.isComplete` — valid *and* every interior cell already lit.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.is_valid() && self.portal_blocks == self.width * self.height
    }

    /// The `(position, state)` pairs `createPortalBlocks` would write.
    ///
    /// Returned rather than written so the caller can drive both the chunk source
    /// and the per-cell `block_update` packets from one list — an ignition that
    /// changes the world without telling the client leaves an invisible portal that
    /// nonetheless teleports, which is worse than not lighting it at all.
    #[must_use]
    pub fn fill(&self) -> Vec<(BlockPos, String)> {
        let state = portal_state(self.axis);
        let (rx, rz) = self.right;
        let mut cells = Vec::with_capacity((self.width * self.height) as usize);
        for up in 0..self.height {
            for across in 0..self.width {
                cells.push((
                    BlockPos::new(
                        self.bottom_left.x + rx * across,
                        self.bottom_left.y + up,
                        self.bottom_left.z + rz * across,
                    ),
                    state.clone(),
                ));
            }
        }
        cells
    }

    /// Every interior cell, whatever it currently holds — the set
    /// [`largest_rectangle_around`] measures and the set a *removal* would clear.
    #[must_use]
    pub fn cells(&self) -> Vec<BlockPos> {
        self.fill().into_iter().map(|(pos, _)| pos).collect()
    }
}

/// `PortalShape.findAnyShape` — the frame around `pos` on `axis`, valid or not.
///
/// Always returns a shape; an invalid one carries `width == 0` or `height == 0`.
/// That is vanilla's shape too, and it matters because the *predicate* is applied
/// by the caller ([`find_empty_portal_shape`]), which needs to distinguish "no
/// frame" from "a complete frame".
#[must_use]
pub fn find_any_shape<S: ChunkSource + ?Sized>(
    world: &S,
    dimension: Dimension,
    pos: BlockPos,
    axis: Axis,
) -> PortalShape {
    let right = axis.right();
    let Some(bottom_left) = calculate_bottom_left(world, dimension, right, pos) else {
        return PortalShape {
            axis,
            right,
            portal_blocks: 0,
            bottom_left: pos,
            width: 0,
            height: 0,
        };
    };

    let width = calculate_width(world, bottom_left, right);
    if width == 0 {
        return PortalShape {
            axis,
            right,
            portal_blocks: 0,
            bottom_left,
            width: 0,
            height: 0,
        };
    }

    let (height, portal_blocks) = calculate_height(world, bottom_left, right, width);
    PortalShape {
        axis,
        right,
        portal_blocks,
        bottom_left,
        width,
        height,
    }
}

/// `PortalShape.findPortalShape` with `findEmptyPortalShape`'s predicate: valid,
/// and holding **no** portal blocks yet.
///
/// Tries `preferred` first, then the other axis — the order vanilla uses, and the
/// reason a frame that would be valid on both axes lights along X.
#[must_use]
pub fn find_empty_portal_shape<S: ChunkSource + ?Sized>(
    world: &S,
    dimension: Dimension,
    pos: BlockPos,
    preferred: Axis,
) -> Option<PortalShape> {
    for axis in [preferred, preferred.other()] {
        let shape = find_any_shape(world, dimension, pos, axis);
        if shape.is_valid() && shape.portal_blocks == 0 {
            return Some(shape);
        }
    }
    None
}

/// A fire source used at `pos`: the portal cells to write, or `None` if there is no
/// valid unlit frame there.
///
/// Vanilla's `BaseFireBlock.onPlace` portal branch, including its **X-first** axis
/// preference. `dimension` is the `inPortalDimension` guard — the overworld and the
/// Nether only — which is why an End portal frame cannot be lit with flint.
#[must_use]
pub fn ignite<S: ChunkSource + ?Sized>(
    world: &S,
    dimension: Dimension,
    pos: BlockPos,
) -> Option<Vec<(BlockPos, String)>> {
    // `inPortalDimension`: both hosted dimensions qualify today, so this is a
    // total match rather than a filter — written as one anyway so adding the End
    // is a compile error here rather than a silently lightable End frame.
    match dimension {
        Dimension::Overworld | Dimension::Nether => {}
    }
    let shape = find_empty_portal_shape(world, dimension, pos, Axis::X)?;
    Some(shape.fill())
}

/// `PortalShape.calculateBottomLeft`.
fn calculate_bottom_left<S: ChunkSource + ?Sized>(
    world: &S,
    dimension: Dimension,
    right: (i32, i32),
    mut pos: BlockPos,
) -> Option<BlockPos> {
    let min_y = dimension.min_y().max(pos.y - MAX_HEIGHT);
    while pos.y > min_y && is_empty(&world.block_state(pos.x, pos.y - 1, pos.z)) {
        pos = BlockPos::new(pos.x, pos.y - 1, pos.z);
    }

    let left = (-right.0, -right.1);
    let edge = distance_until_edge_above_frame(world, pos, left) - 1;
    (edge >= 0).then(|| BlockPos::new(pos.x + left.0 * edge, pos.y, pos.z + left.1 * edge))
}

/// `PortalShape.getDistanceUntilEdgeAboveFrame`.
///
/// Walks along `direction` while the cell is empty **and the cell below it is
/// frame**, and returns the distance at which it hits a frame block. The
/// below-is-frame condition is what stops the search escaping through the bottom
/// of an incomplete frame — dropping it finds a "frame" in open air.
fn distance_until_edge_above_frame<S: ChunkSource + ?Sized>(
    world: &S,
    pos: BlockPos,
    direction: (i32, i32),
) -> i32 {
    for width in 0..=MAX_WIDTH {
        let x = pos.x + direction.0 * width;
        let z = pos.z + direction.1 * width;
        let state = world.block_state(x, pos.y, z);
        if !is_empty(&state) {
            if is_frame(&state) {
                return width;
            }
            break;
        }
        if !is_frame(&world.block_state(x, pos.y - 1, z)) {
            break;
        }
    }
    0
}

/// `PortalShape.calculateWidth`.
fn calculate_width<S: ChunkSource + ?Sized>(
    world: &S,
    bottom_left: BlockPos,
    right: (i32, i32),
) -> i32 {
    let width = distance_until_edge_above_frame(world, bottom_left, right);
    if (MIN_WIDTH..=MAX_WIDTH).contains(&width) {
        width
    } else {
        0
    }
}

/// `PortalShape.calculateHeight`, returning `(height, portal_block_count)`.
fn calculate_height<S: ChunkSource + ?Sized>(
    world: &S,
    bottom_left: BlockPos,
    right: (i32, i32),
    width: i32,
) -> (i32, i32) {
    let (height, portal_blocks) = distance_until_top(world, bottom_left, right, width);
    if (MIN_HEIGHT..=MAX_HEIGHT).contains(&height)
        && has_top_frame(world, bottom_left, right, width, height)
    {
        (height, portal_blocks)
    } else {
        (0, portal_blocks)
    }
}

/// `PortalShape.hasTopFrame`.
fn has_top_frame<S: ChunkSource + ?Sized>(
    world: &S,
    bottom_left: BlockPos,
    right: (i32, i32),
    width: i32,
    height: i32,
) -> bool {
    (0..width).all(|i| {
        is_frame(&world.block_state(
            bottom_left.x + right.0 * i,
            bottom_left.y + height,
            bottom_left.z + right.1 * i,
        ))
    })
}

/// `PortalShape.getDistanceUntilTop`, returning `(height, portal_block_count)`.
///
/// The count accumulates across every row it *did* walk, including on the row that
/// terminated the loop — vanilla's `MutableInt` output parameter is not rolled back
/// either.
fn distance_until_top<S: ChunkSource + ?Sized>(
    world: &S,
    bottom_left: BlockPos,
    right: (i32, i32),
    width: i32,
) -> (i32, i32) {
    let mut portal_blocks = 0;
    for height in 0..MAX_HEIGHT {
        let y = bottom_left.y + height;
        // The two side columns, one cell outside the interior on each side.
        for across in [-1, width] {
            let x = bottom_left.x + right.0 * across;
            let z = bottom_left.z + right.1 * across;
            if !is_frame(&world.block_state(x, y, z)) {
                return (height, portal_blocks);
            }
        }
        for i in 0..width {
            let x = bottom_left.x + right.0 * i;
            let z = bottom_left.z + right.1 * i;
            let state = world.block_state(x, y, z);
            if !is_empty(&state) {
                return (height, portal_blocks);
            }
            if is_portal(&state) {
                portal_blocks += 1;
            }
        }
    }
    (MAX_HEIGHT, portal_blocks)
}

/// The largest contiguous rectangle of the *same* portal state around `pos`, as
/// `(min_corner, along_axis_size, height)` — vanilla's
/// `BlockUtil.getLargestRectangleAround(pos, axis, 21, Y, 21, …)`.
///
/// Used to place an arriving entity inside a portal it did not create: the relative
/// position within the source portal's rectangle is carried to the destination's,
/// which is why a wide portal does not dump everyone at its left edge.
#[must_use]
pub fn largest_rectangle_around<S: ChunkSource + ?Sized>(
    world: &S,
    pos: BlockPos,
    axis: Axis,
) -> (BlockPos, i32, i32) {
    let state = world.block_state(pos.x, pos.y, pos.z);
    let matches = |x: i32, y: i32, z: i32| world.block_state(x, y, z) == state;
    let (ax, az) = match axis {
        Axis::X => (1, 0),
        Axis::Z => (0, 1),
    };
    let mut min_across = 0;
    while min_across < MAX_WIDTH
        && matches(
            pos.x - ax * (min_across + 1),
            pos.y,
            pos.z - az * (min_across + 1),
        )
    {
        min_across += 1;
    }
    let mut max_across = 0;
    while max_across < MAX_WIDTH
        && matches(
            pos.x + ax * (max_across + 1),
            pos.y,
            pos.z + az * (max_across + 1),
        )
    {
        max_across += 1;
    }
    let mut down = 0;
    while down < MAX_HEIGHT && matches(pos.x, pos.y - (down + 1), pos.z) {
        down += 1;
    }
    let mut up = 0;
    while up < MAX_HEIGHT && matches(pos.x, pos.y + (up + 1), pos.z) {
        up += 1;
    }
    let corner = BlockPos::new(pos.x - ax * min_across, pos.y - down, pos.z - az * min_across);
    (corner, min_across + max_across + 1, down + up + 1)
}

/// Every lit portal this world knows about, per dimension — this crate's stand-in
/// for vanilla's `PoiManager` index of `PoiTypes.NETHER_PORTAL`.
///
/// # Why an index rather than a scan
///
/// `PortalForcer.findClosestPortalPosition` searches a **128-block radius** in the
/// overworld. Without an index that is 257 × 257 × 384 block reads across 289 chunk
/// columns — most of them not generated yet — per return trip. Vanilla does not pay
/// that because its POI manager is a persisted per-section index built as blocks
/// are placed. This is the same idea, minus persistence.
///
/// **Not persisted across a restart by *this struct* — but the storage seam now
/// exists.** A portal lit in an earlier session is not in a fresh index, so the
/// first trip after a restart falls back to the bounded local scan
/// [`find_exit_portal`] also performs, and beyond its 16-block radius will build a
/// second portal beside the first. [`crate::poi_storage`] is vanilla's real fix for
/// this (`PoiManager` persists exactly the `NETHER_PORTAL` type this index tracks),
/// and [`poi_records_for_index`]/[`restore_index_from_poi`] below convert between
/// the two — proven by a real round trip through [`crate::poi_storage::PoiStorage`]
/// in `tests/poi_persistence_round_trip.rs`. What is **not** wired yet is calling
/// them from world open/shutdown, which lives beside
/// [`crate::entity_storage::EntityStorage`]'s own wiring in `crate::integrated`.
#[derive(Debug, Clone, Default)]
pub struct PortalIndex(Arc<Mutex<HashMap<Dimension, Vec<BlockPos>>>>);

impl PortalIndex {
    /// An empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a lit portal cell.
    pub fn insert(&self, dimension: Dimension, pos: BlockPos) {
        let mut index = self.0.lock().expect("portal index lock poisoned");
        let cells = index.entry(dimension).or_default();
        if !cells.contains(&pos) {
            cells.push(pos);
        }
    }

    /// Records every cell of a freshly lit or freshly built portal.
    pub fn extend(&self, dimension: Dimension, cells: impl IntoIterator<Item = BlockPos>) {
        for pos in cells {
            self.insert(dimension, pos);
        }
    }

    /// Forgets a cell — a portal that was broken, or one the index recorded and the
    /// world no longer agrees with.
    pub fn remove(&self, dimension: Dimension, pos: BlockPos) {
        if let Ok(mut index) = self.0.lock() {
            if let Some(cells) = index.get_mut(&dimension) {
                cells.retain(|cell| *cell != pos);
            }
        }
    }

    /// Every recorded cell in `dimension`, as a snapshot.
    #[must_use]
    pub fn cells(&self, dimension: Dimension) -> Vec<BlockPos> {
        self.0
            .lock()
            .map(|index| index.get(&dimension).cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Whether this handle and `other` share one store — the control a sharing gate
    /// needs, since two empty indices are otherwise indistinguishable.
    #[must_use]
    pub fn is_same_store(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// `PoiTypes.NETHER_PORTAL`'s resource key — the type every cell in
/// [`PortalIndex`] persists as.
pub const NETHER_PORTAL_POI_TYPE: &str = "minecraft:nether_portal";

/// Every cell recorded for `dimension`, as `nether_portal` POI records ready for
/// [`crate::poi_storage::PoiStorage::save`].
///
/// A fresh [`crate::poi_storage::PoiRecord`] for this type starts at
/// `free_tickets: 0` (`PoiTypes.bootstrap` registers `NETHER_PORTAL` with
/// `maxTickets 0`), matching vanilla exactly: a portal is indexed for lookup, never
/// claimed the way a workstation is.
#[must_use]
pub fn poi_records_for_index(
    index: &PortalIndex,
    dimension: Dimension,
) -> Vec<crate::poi_storage::PoiRecord> {
    index
        .cells(dimension)
        .into_iter()
        .map(|pos| {
            crate::poi_storage::PoiRecord::new(
                pos,
                NETHER_PORTAL_POI_TYPE
                    .parse()
                    .expect("NETHER_PORTAL_POI_TYPE is a valid resource key"),
            )
        })
        .collect()
}

/// Rebuilds a [`PortalIndex`] from persisted POI sections — the read half of the
/// gap [`PortalIndex`]'s own doc names. `sections` is every [`crate::poi_storage::PoiSection`]
/// covering the loaded area, for whichever dimension `sections` was read from;
/// the caller (world open, once wired) is responsible for calling this once per
/// dimension with that dimension's own POI store.
///
/// Only `nether_portal`-typed records are taken — a POI store may hold workstation
/// or bed records too, none of which this index tracks.
#[must_use]
pub fn restore_index_from_poi<'a>(
    dimension: Dimension,
    sections: impl IntoIterator<Item = &'a crate::poi_storage::PoiSection>,
) -> PortalIndex {
    let index = PortalIndex::new();
    for section in sections {
        for record in &section.records {
            if record.poi_type.path() == "nether_portal" {
                index.insert(dimension, record.pos);
            }
        }
    }
    index
}

/// `PortalForcer.NETHER_PORTAL_RADIUS` — the search radius when arriving *in* the
/// Nether.
pub const NETHER_SEARCH_RADIUS: i32 = 16;
/// `PortalForcer.OVERWORLD_PORTAL_RADIUS` — the (much larger) radius when arriving
/// in the overworld, because the same Nether portal serves a 128-block overworld
/// area under the 8:1 scale.
pub const OVERWORLD_SEARCH_RADIUS: i32 = 128;

/// Horizontal reach of [`find_exit_portal`]'s index-missed fallback scan. **Not**
/// vanilla's radius — vanilla has a persisted POI index and needs no fallback. See
/// that function for why this is small.
pub const FALLBACK_SCAN_RADIUS: i32 = 8;
/// Vertical reach of the same fallback, measured from the scaled arrival `y`.
pub const FALLBACK_Y_REACH: i32 = 16;

/// The search radius for arriving in `dimension`.
#[must_use]
pub fn search_radius(dimension: Dimension) -> i32 {
    match dimension {
        Dimension::Nether => NETHER_SEARCH_RADIUS,
        Dimension::Overworld => OVERWORLD_SEARCH_RADIUS,
    }
}

/// The closest existing portal to `origin` in `dimension` — vanilla's
/// `PortalForcer.findClosestPortalPosition`.
///
/// Two sources, in this order, because neither alone is enough:
///
/// 1. [`PortalIndex`], filtered to the search radius and **re-validated against the
///    world** (an indexed cell whose block is gone must not be returned, or every
///    trip lands at a portal that was mined out three sessions ago).
/// 2. A bounded local scan of a 16-block radius. This is what makes the return trip
///    work at all after a restart, when the index is empty — see [`PortalIndex`]'s
///    own doc.
///
/// Ties break on squared distance then lower `y`, matching vanilla's
/// `comparingDouble(distSqr).thenComparingInt(getY)`.
#[must_use]
pub fn find_exit_portal<S: ChunkSource + ?Sized>(
    world: &S,
    dimension: Dimension,
    index: Option<&PortalIndex>,
    origin: BlockPos,
) -> Option<BlockPos> {
    let radius = search_radius(dimension);
    let mut best: Option<(i64, i32, BlockPos)> = None;

    if let Some(index) = index {
        for pos in index.cells(dimension) {
            if (pos.x - origin.x).abs() <= radius && (pos.z - origin.z).abs() <= radius {
                consider_portal_cell(world, origin, pos, &mut best);
            }
        }
    }
    if let Some((_, _, pos)) = best {
        return Some(pos);
    }

    // The index missed. Fall back to a bounded scan, and **bounded is the operative
    // word**: this runs synchronously inside a server tick, and every cell it touches
    // may be the read that generates a whole column. The dimension's full placeable
    // range over a 33 × 33 footprint is 418,000 reads across a dozen columns the
    // generator has not produced yet — measured at seconds, which is a keep-alive
    // timeout, not a slow frame.
    //
    // What the fallback is *for* is narrow: finding the portal the player themselves
    // built, in a session that started after it was lit and so has an empty index. A
    // portal within ±8 blocks and ±`FALLBACK_Y_REACH` of the scaled arrival point
    // covers that; anything further away is what the index exists for.
    let scan = FALLBACK_SCAN_RADIUS;
    let y_lo = (origin.y - FALLBACK_Y_REACH).max(dimension.min_y());
    let y_hi = (origin.y + FALLBACK_Y_REACH).min(dimension.max_placeable_y());
    for dx in -scan..=scan {
        for dz in -scan..=scan {
            for y in y_lo..=y_hi {
                consider_portal_cell(
                    world,
                    origin,
                    BlockPos::new(origin.x + dx, y, origin.z + dz),
                    &mut best,
                );
            }
        }
    }
    best.map(|(_, _, pos)| pos)
}

/// Folds one candidate cell into [`find_exit_portal`]'s running best.
///
/// **The `is_portal` re-read is the load-bearing line.** Both callers feed
/// positions they only *believe* hold a portal — one from an index that is not
/// re-validated anywhere else, one from a blind scan — so a candidate that the
/// world disagrees with must be dropped here or a trip lands wherever a portal used
/// to be.
fn consider_portal_cell<S: ChunkSource + ?Sized>(
    world: &S,
    origin: BlockPos,
    pos: BlockPos,
    best: &mut Option<(i64, i32, BlockPos)>,
) {
    if !is_portal(&world.block_state(pos.x, pos.y, pos.z)) {
        return;
    }
    let dx = i64::from(pos.x - origin.x);
    let dy = i64::from(pos.y - origin.y);
    let dz = i64::from(pos.z - origin.z);
    let dist = dx * dx + dy * dy + dz * dz;
    let better = match best {
        None => true,
        Some((best_dist, best_y, _)) => {
            dist < *best_dist || (dist == *best_dist && pos.y < *best_y)
        }
    };
    if better {
        *best = Some((dist, pos.y, pos));
    }
}

/// A freshly built portal: its lower-left interior cell, its axis, and every block
/// the caller must write and broadcast.
#[derive(Debug, Clone)]
pub struct CreatedPortal {
    /// The lower-left interior cell — `BlockUtil.FoundRectangle`'s `minCorner`.
    pub origin: BlockPos,
    /// The axis the frame was laid out on.
    pub axis: Axis,
    /// Frame, interior air and portal cells, in write order.
    pub blocks: Vec<(BlockPos, String)>,
    /// Just the portal cells, for [`PortalIndex`].
    pub portal_cells: Vec<BlockPos>,
}

/// `PortalForcer.createPortal` — sites and builds a 2×3 portal near `origin`.
///
/// # What is ported exactly, and what is not
///
/// Exact: the two-pass site search (a spot that can host the frame *and* both
/// neighbouring thickness slices scores as "full", one that can host only the
/// middle slice as "partial", and a full hit always wins), the `y + 4 <=
/// maxPlaceableY` headroom check, the `deltaY <= 0 || deltaY >= 3` air-pocket rule,
/// the `-1..3 × -1..4` frame ring, and the fallback that clamps `origin.y` into the
/// placeable band and carves its own 2×2×4 pocket.
///
/// **Not exact: the scan order.** Vanilla walks `BlockPos.spiralAround(origin, 16,
/// EAST, SOUTH)`; this walks the same 33 × 33 square sorted by squared distance
/// then `(dx, dz)`. Both visit the same set of columns, and the winner is chosen by
/// a strict `>` on distance either way, so the two can only disagree between
/// candidates at *identical* distance — which changes which of two equally good
/// spots is used, not whether a spot is found.
///
/// Returns `None` only when the destination dimension has no placeable band at all,
/// vanilla's `Optional.empty()` "unable to create a portal".
#[must_use]
pub fn create_portal<S: ChunkSource + ?Sized>(
    world: &S,
    dimension: Dimension,
    origin: BlockPos,
    axis: Axis,
) -> Option<CreatedPortal> {
    let forward = axis.positive();
    let clockwise = axis.positive_clockwise();
    let max_placeable_y = dimension.max_placeable_y();

    let mut offsets: Vec<(i32, i32)> = Vec::with_capacity(33 * 33);
    for dx in -16..=16 {
        for dz in -16..=16 {
            offsets.push((dx, dz));
        }
    }
    offsets.sort_by_key(|&(dx, dz)| (dx * dx + dz * dz, dx, dz));

    let mut closest_full: Option<(i64, BlockPos)> = None;
    let mut closest_partial: Option<(i64, BlockPos)> = None;

    for (dx, dz) in offsets {
        let column_x = origin.x + dx;
        let column_z = origin.z + dz;
        // `columnPos.move(direction, 1)` then back: a world-border check we have no
        // per-dimension border for, so the column is always in bounds. The moves
        // themselves are what leaves `columnPos` where the scan expects it, and
        // since they cancel there is nothing to reproduce here.
        let top = max_placeable_y.min(motion_blocking_height(world, dimension, column_x, column_z));
        let mut y = top;
        while y >= dimension.min_y() {
            if !can_portal_replace(world, column_x, y, column_z) {
                y -= 1;
                continue;
            }
            let first_empty_y = y;
            while y > dimension.min_y() && can_portal_replace(world, column_x, y - 1, column_z) {
                y -= 1;
            }
            if y + 4 <= max_placeable_y {
                let delta_y = first_empty_y - y;
                if delta_y <= 0 || delta_y >= 3 {
                    let candidate = BlockPos::new(column_x, y, column_z);
                    if can_host_frame(world, candidate, forward, clockwise, 0) {
                        let dist =
                            i64::from(dx) * i64::from(dx) + i64::from(dz) * i64::from(dz);
                        let full = can_host_frame(world, candidate, forward, clockwise, -1)
                            && can_host_frame(world, candidate, forward, clockwise, 1);
                        let beats = |held: &Option<(i64, BlockPos)>| match held {
                            None => true,
                            Some((best, _)) => *best > dist,
                        };
                        if full && beats(&closest_full) {
                            closest_full = Some((dist, candidate));
                        }
                        if closest_full.is_none() && beats(&closest_partial) {
                            closest_partial = Some((dist, candidate));
                        }
                    }
                }
            }
            y -= 1;
        }
    }

    let mut blocks: Vec<(BlockPos, String)> = Vec::new();
    let site = match closest_full.or(closest_partial) {
        Some((_, pos)) => pos,
        None => {
            // Nothing suitable anywhere in range: clamp into the placeable band and
            // carve a pocket. `origin.x - forward.x` is vanilla's own one-block
            // step back, so the frame's `-1` column lands on the arrival point.
            let y = dimension.clamp_portal_y(origin.y)?;
            let site = BlockPos::new(origin.x - forward.0, y, origin.z - forward.1);
            for box_offset in -1..2 {
                for across in 0..2 {
                    for up in -1..3 {
                        let state = if up < 0 {
                            FRAME_BLOCK.to_owned()
                        } else {
                            "minecraft:air".to_owned()
                        };
                        blocks.push((
                            BlockPos::new(
                                site.x + across * forward.0 + box_offset * clockwise.0,
                                site.y + up,
                                site.z + across * forward.1 + box_offset * clockwise.1,
                            ),
                            state,
                        ));
                    }
                }
            }
            site
        }
    };

    // The frame ring: the border of the `-1..=2 × -1..=3` rectangle in the portal's
    // own plane.
    for across in -1..3 {
        for up in -1..4 {
            if across == -1 || across == 2 || up == -1 || up == 3 {
                blocks.push((
                    BlockPos::new(
                        site.x + across * forward.0,
                        site.y + up,
                        site.z + across * forward.1,
                    ),
                    FRAME_BLOCK.to_owned(),
                ));
            }
        }
    }

    let state = portal_state(axis);
    let mut portal_cells = Vec::with_capacity(6);
    for across in 0..2 {
        for up in 0..3 {
            let pos = BlockPos::new(
                site.x + across * forward.0,
                site.y + up,
                site.z + across * forward.1,
            );
            portal_cells.push(pos);
            blocks.push((pos, state.clone()));
        }
    }

    Some(CreatedPortal {
        origin: site,
        axis,
        blocks,
        portal_cells,
    })
}

/// Whether a state blocks motion, out of `lodestone_data`'s jar-derived census.
///
/// A state the census does not carry answers **yes**, the same direction
/// `crate::fluid` chose for the same question: an unclassified block should stop
/// the site search rather than be quietly carved out.
fn blocks_motion(state: &str) -> bool {
    lodestone_data::block_solidity::blocks_motion(crate::chunk::resolve_palette_state_id(state))
        .unwrap_or(true)
}

/// `BlockState.canBeReplaced() && getFluidState().isEmpty()` —
/// `PortalForcer.canPortalReplaceBlock`.
///
/// **Two clauses, and the fluid one is not redundant.** Water and lava are
/// non-motion-blocking, so the census clause alone calls them replaceable — and
/// vanilla explicitly does not, because a portal carved into a lava lake fills
/// straight back in. `canBeReplaced` itself has no census in this crate;
/// "does not block motion" is the standing proxy for it (air, plants, snow layers,
/// fire all qualify), and it errs toward *refusing* a column, which only costs the
/// search one more candidate.
fn can_portal_replace<S: ChunkSource + ?Sized>(world: &S, x: i32, y: i32, z: i32) -> bool {
    let state = world.block_state(x, y, z);
    if state.starts_with("minecraft:water") || state.starts_with("minecraft:lava") {
        return false;
    }
    is_empty(&state) || !blocks_motion(&state)
}

/// `PortalForcer.canHostFrame`: the `-1..3 × -1..4` box offset sideways by
/// `offset` must be **solid below the origin row** and **replaceable at and above
/// it**.
fn can_host_frame<S: ChunkSource + ?Sized>(
    world: &S,
    origin: BlockPos,
    forward: (i32, i32),
    clockwise: (i32, i32),
    offset: i32,
) -> bool {
    for across in -1..3 {
        for up in -1..4 {
            let x = origin.x + forward.0 * across + clockwise.0 * offset;
            let y = origin.y + up;
            let z = origin.z + forward.1 * across + clockwise.1 * offset;
            if up < 0 {
                // `BlockState.isSolid`, proxied by the motion census — the frame
                // needs something to stand on.
                if !blocks_motion(&world.block_state(x, y, z)) {
                    return false;
                }
            } else if !can_portal_replace(world, x, y, z) {
                return false;
            }
        }
    }
    true
}

/// The highest non-replaceable `y` in a column, plus one — a stand-in for
/// `Heightmap.Types.MOTION_BLOCKING`, which this crate does not maintain per
/// dimension.
///
/// Scans down from the dimension's ceiling. This is where the Nether's `height` vs
/// `logical_height` split bites: starting at `max_y` (255) rather than
/// `max_placeable_y` (127) would spend 128 reads per column on guaranteed air, and
/// the caller then clamps to 127 anyway.
fn motion_blocking_height<S: ChunkSource + ?Sized>(
    world: &S,
    dimension: Dimension,
    x: i32,
    z: i32,
) -> i32 {
    let mut y = dimension.max_placeable_y();
    while y > dimension.min_y() {
        if !can_portal_replace(world, x, y, z) {
            return y + 1;
        }
        y -= 1;
    }
    dimension.min_y()
}

/// Where a trip lands, and the portal that had to be built for it to.
#[derive(Debug, Clone)]
pub struct PortalDestination {
    /// The arrival position: the bottom-centre of the exit portal's own rectangle.
    pub position: lodestone_model::Vec3,
    /// `Some` when no exit portal existed and one was sited. **The caller owns the
    /// writes** — this function does not touch the world, so a caller can resolve a
    /// destination without committing to it (and a gate can assert on the plan).
    pub created: Option<CreatedPortal>,
    /// The dimension the trip lands in.
    pub dimension: Dimension,
}

/// Resolves where a player standing in a portal at `player_pos` in `from` arrives in
/// `to` — vanilla's `NetherPortalBlock.getPortalDestination` plus `getExitPortal`.
///
/// `destination` is the *target* dimension's terrain; `source_axis` is the axis of
/// the portal block the player is standing in, which is what a newly built exit
/// portal is aligned to.
///
/// Returns `None` when the destination has no placeable band at all — vanilla's
/// "unable to create a portal", which is a decline rather than an error.
///
/// **This is the whole of a trip that has nothing to do with packets**, and it is
/// public for that reason: `crate::server`'s travel path calls it, and so does
/// `tests/nether_portal_round_trip.rs`, so the gate measures production's own
/// destination logic rather than a second copy of it.
#[must_use]
pub fn resolve_destination<S: ChunkSource + ?Sized>(
    destination: &S,
    from: Dimension,
    to: Dimension,
    index: Option<&PortalIndex>,
    player_pos: (f64, f64, f64),
    source_axis: Axis,
) -> Option<PortalDestination> {
    let (x, y, z) = player_pos;
    let (sx, sy, sz) = crate::dimension::scaled_destination(from, to, x, y, z)?;
    let approximate = BlockPos::new(sx, sy, sz);

    if let Some(existing) = find_exit_portal(destination, to, index, approximate) {
        // Land at the bottom-centre of the portal's own rectangle, so a wide portal
        // does not dump everyone at its left edge.
        let axis = Axis::from_state(&destination.block_state(existing.x, existing.y, existing.z));
        let (corner, _, _) = largest_rectangle_around(destination, existing, axis);
        return Some(PortalDestination {
            position: lodestone_model::Vec3::new(
                f64::from(corner.x) + 0.5,
                f64::from(corner.y),
                f64::from(corner.z) + 0.5,
            ),
            created: None,
            dimension: to,
        });
    }

    let created = create_portal(destination, to, approximate, source_axis)?;
    Some(PortalDestination {
        position: lodestone_model::Vec3::new(
            f64::from(created.origin.x) + 0.5,
            f64::from(created.origin.y),
            f64::from(created.origin.z) + 0.5,
        ),
        created: Some(created),
        dimension: to,
    })
}

/// The per-player portal transition counter — vanilla's `Entity.portalProcess`
/// (`PortalProcessor`) plus `Entity.portalCooldown`, in one small value.
///
/// Lives as a `serve_play` local, next to `take_xp_delay`, because that is where
/// every other per-player tick counter in this crate lives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PortalTracker {
    /// The cell the player entered the portal at, carried so the destination search
    /// can read the *source* portal's axis and rectangle.
    entry: Option<BlockPos>,
    /// `PortalProcessor.portalTime`.
    ticks: i32,
    /// `Entity.portalCooldown`.
    cooldown: i32,
}

impl PortalTracker {
    /// A player who has never touched a portal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a trip is currently forbidden — `Entity.isOnPortalCooldown`.
    #[must_use]
    pub fn on_cooldown(&self) -> bool {
        self.cooldown > 0
    }

    /// The accumulated counter, for a gate that wants to see it climb.
    #[must_use]
    pub fn ticks(&self) -> i32 {
        self.ticks
    }

    /// Starts the cooldown after a completed trip —
    /// `Entity.setPortalCooldown()`, which for a player is 10 ticks.
    pub fn begin_cooldown(&mut self) {
        self.cooldown = PLAYER_PORTAL_COOLDOWN;
        self.ticks = 0;
        self.entry = None;
    }

    /// One server tick. `inside` is the portal cell the player currently occupies,
    /// if any; `transition_ticks` is the game-rule delay for this player.
    ///
    /// Returns the entry position when the player should travel **now**.
    ///
    /// Faithful in three places that each look like a detail:
    ///
    /// * the cooldown is decremented first and, while non-zero, standing in a
    ///   portal *re-arms* it (`setAsInsidePortal`'s `isOnPortalCooldown` branch
    ///   calls `setPortalCooldown()` again) — which is what stops a player who
    ///   materialises inside the destination portal bouncing back and forth;
    /// * the counter **decays by 4** rather than resetting when outside;
    /// * the comparison is post-increment, so `transition_ticks == 0` fires on the
    ///   very first tick inside and `80` fires on the 81st.
    pub fn tick(&mut self, inside: Option<BlockPos>, transition_ticks: i32) -> Option<BlockPos> {
        if self.cooldown > 0 {
            self.cooldown -= 1;
            if inside.is_some() {
                self.cooldown = PLAYER_PORTAL_COOLDOWN;
            }
            return None;
        }

        match inside {
            Some(pos) => {
                self.entry = Some(pos);
                let reached = self.ticks >= transition_ticks;
                self.ticks += 1;
                reached.then_some(pos)
            }
            None => {
                self.ticks = (self.ticks - 4).max(0);
                if self.ticks == 0 {
                    self.entry = None;
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;
    use std::sync::Mutex as Lock;

    /// A block-map world, so the frame tests exercise the search and nothing else.
    struct FlatWorld(Lock<Map<(i32, i32, i32), String>>);

    impl FlatWorld {
        fn new() -> Self {
            Self(Lock::new(Map::new()))
        }
        fn put(&self, x: i32, y: i32, z: i32, state: &str) {
            self.0
                .lock()
                .unwrap()
                .insert((x, y, z), state.to_owned());
        }
        /// A frame whose interior is `width × height`, lower-left interior cell at
        /// `(x, y, z)`, lying in the plane of `axis`.
        fn frame(&self, x: i32, y: i32, z: i32, axis: Axis, width: i32, height: i32) {
            let (ax, az) = match axis {
                Axis::X => (1, 0),
                Axis::Z => (0, 1),
            };
            for across in -1..=width {
                for up in -1..=height {
                    let edge = across == -1 || across == width || up == -1 || up == height;
                    if edge {
                        self.put(x + ax * across, y + up, z + az * across, FRAME_BLOCK);
                    }
                }
            }
        }
    }

    impl ChunkSource for FlatWorld {
        fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
            crate::chunk::ChunkColumn::new(0, 256)
        }
        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            self.0
                .lock()
                .unwrap()
                .get(&(x, y, z))
                .cloned()
                .unwrap_or_else(|| "minecraft:air".to_owned())
        }
        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            self.put(x, y, z, name);
        }
    }

    /// A 2×3 frame lights, and the cells written are exactly its interior.
    ///
    /// The negative control is the same frame **one block short**: 2×2 is inside
    /// every "is there obsidian here" heuristic and outside vanilla's `MIN_HEIGHT`,
    /// so a search that ignored the bounds would light it.
    #[test]
    fn a_two_by_three_frame_lights_and_a_two_by_two_does_not() {
        let world = FlatWorld::new();
        world.frame(100, 70, 200, Axis::X, 2, 3);
        let cells = ignite(&world, Dimension::Overworld, BlockPos::new(100, 70, 200))
            .expect("a 2x3 obsidian frame is a valid portal");
        assert_eq!(cells.len(), 6, "2 x 3 interior cells");
        assert!(
            cells
                .iter()
                .all(|(_, state)| state == "minecraft:nether_portal[axis=x]"),
            "the whole interior is portal blocks on the clicked axis"
        );

        let short = FlatWorld::new();
        short.frame(0, 70, 0, Axis::X, 2, 2);
        assert!(
            ignite(&short, Dimension::Overworld, BlockPos::new(0, 70, 0)).is_none(),
            "a 2x2 frame is below MIN_HEIGHT and must not light"
        );
    }

    /// The bounds are the *real* ones, not 2×3 — a 21-wide frame is legal and a
    /// 22-wide one is not.
    ///
    /// Both arms matter: passing only the wide case is satisfied by an
    /// implementation with no upper bound at all.
    #[test]
    fn the_frame_bounds_are_two_to_twenty_one_wide() {
        let wide = FlatWorld::new();
        wide.frame(0, 70, 0, Axis::Z, 21, 3);
        let shape = find_empty_portal_shape(&wide, Dimension::Overworld, BlockPos::new(0, 70, 0), Axis::X)
            .expect("21 wide is the documented maximum");
        assert_eq!(shape.width(), 21);
        assert_eq!(shape.axis(), Axis::Z, "a Z-plane frame lights on Z");

        let too_wide = FlatWorld::new();
        too_wide.frame(0, 70, 0, Axis::Z, 22, 3);
        assert!(
            find_empty_portal_shape(&too_wide, Dimension::Overworld, BlockPos::new(0, 70, 0), Axis::X)
                .is_none(),
            "22 wide exceeds MAX_WIDTH"
        );
    }

    /// A frame that is already lit does not re-light — `numPortalBlocks == 0` — but
    /// it *is* recognised as complete, which is the other half of the same scan.
    #[test]
    fn an_already_lit_frame_is_complete_and_not_relightable() {
        let world = FlatWorld::new();
        world.frame(0, 70, 0, Axis::X, 2, 3);
        for (pos, state) in ignite(&world, Dimension::Overworld, BlockPos::new(0, 70, 0)).unwrap() {
            world.set_block(pos.x, pos.y, pos.z, &state);
        }
        assert!(
            ignite(&world, Dimension::Overworld, BlockPos::new(0, 70, 0)).is_none(),
            "a lit frame holds portal blocks, so findEmptyPortalShape must reject it"
        );
        let shape = find_any_shape(&world, Dimension::Overworld, BlockPos::new(0, 70, 0), Axis::X);
        assert!(shape.is_complete(), "every interior cell is lit");
        assert_eq!(shape.portal_blocks(), 6);
    }

    /// The transition counter's three faithful details, each against an input that
    /// separates it from the plausible wrong version.
    #[test]
    fn the_transition_counter_is_post_increment_and_decays_by_four() {
        // Post-increment: a delay of 0 fires on the first tick inside.
        let mut creative = PortalTracker::new();
        let at = BlockPos::new(1, 2, 3);
        assert_eq!(creative.tick(Some(at), 0), Some(at));

        // A delay of 80 fires on the 81st consecutive tick, not the 80th.
        let mut survival = PortalTracker::new();
        for tick in 0..80 {
            assert_eq!(
                survival.tick(Some(at), 80),
                None,
                "tick {tick} of 80 must not fire"
            );
        }
        assert_eq!(survival.tick(Some(at), 80), Some(at), "the 81st fires");

        // Decay is -4, not a reset: 10 ticks in, one tick out, and the counter is 7.
        let mut decaying = PortalTracker::new();
        for _ in 0..11 {
            decaying.tick(Some(at), 80);
        }
        assert_eq!(decaying.ticks(), 11);
        decaying.tick(None, 80);
        assert_eq!(decaying.ticks(), 7, "decay subtracts four, it does not reset");
        for _ in 0..2 {
            decaying.tick(None, 80);
        }
        assert_eq!(decaying.ticks(), 0, "and it floors at zero");
    }

    /// The cooldown blocks a trip and, crucially, *re-arms* while the player is
    /// still standing in the destination portal — the property that stops an
    /// arriving player bouncing straight back.
    #[test]
    fn the_cooldown_re_arms_while_still_inside_a_portal() {
        let mut tracker = PortalTracker::new();
        let at = BlockPos::new(0, 70, 0);
        tracker.begin_cooldown();
        // Standing in the arrival portal for far longer than the cooldown itself.
        for tick in 0..(PLAYER_PORTAL_COOLDOWN * 5) {
            assert_eq!(
                tracker.tick(Some(at), 0),
                None,
                "tick {tick}: an arriving player must not be sent back"
            );
        }
        assert!(tracker.on_cooldown());
        // Step out, let it lapse, and the very next entry travels (delay 0).
        for _ in 0..=PLAYER_PORTAL_COOLDOWN {
            tracker.tick(None, 0);
        }
        assert!(!tracker.on_cooldown());
        assert_eq!(tracker.tick(Some(at), 0), Some(at));
    }

    /// `create_portal` in mid-air produces a portal whose own frame search accepts
    /// it — the composition the two halves have no name for otherwise.
    ///
    /// This is the assertion that would have caught a frame written without its
    /// interior being carved: the ring would be right, the search would find
    /// netherrack in the interior, and `is_complete` would be false.
    #[test]
    fn a_created_portal_is_a_portal_its_own_frame_search_accepts() {
        let world = FlatWorld::new();
        // A floor, so the site search has something solid to stand the frame on.
        for x in -20..=20 {
            for z in -20..=20 {
                for y in 0..=69 {
                    world.put(x, y, z, "minecraft:netherrack");
                }
            }
        }
        let created = create_portal(
            &world,
            Dimension::Nether,
            BlockPos::new(0, 96, 0),
            Axis::X,
        )
        .expect("the Nether has a placeable band");
        for (pos, state) in &created.blocks {
            world.set_block(pos.x, pos.y, pos.z, state);
        }
        assert_eq!(created.portal_cells.len(), 6, "a created portal is 2 x 3");

        let anchor = created.portal_cells[0];
        let shape = find_any_shape(&world, Dimension::Nether, anchor, created.axis);
        assert!(
            shape.is_complete(),
            "the built portal must satisfy the same search that lights one: {shape:?}"
        );
        // And it is below the Nether roof, which is the trap the brief names.
        assert!(
            created.origin.y + 3 <= Dimension::Nether.max_placeable_y(),
            "portal top at {} exceeds the Nether's placeable ceiling {}",
            created.origin.y + 3,
            Dimension::Nether.max_placeable_y()
        );
    }

    /// The index and the fallback scan are two different mechanisms, and each has
    /// to work without the other — so both arms are driven separately.
    #[test]
    fn the_exit_search_uses_the_index_and_falls_back_to_a_scan() {
        let world = FlatWorld::new();
        let portal = BlockPos::new(40, 71, -12);
        world.put(portal.x, portal.y, portal.z, "minecraft:nether_portal[axis=x]");

        // Index arm.
        let index = PortalIndex::new();
        index.insert(Dimension::Nether, portal);
        assert_eq!(
            find_exit_portal(&world, Dimension::Nether, Some(&index), BlockPos::new(44, 70, -10)),
            Some(portal)
        );

        // Scan arm: no index at all, same answer, because the portal is inside the
        // 16-block fallback radius.
        assert_eq!(
            find_exit_portal(&world, Dimension::Nether, None, BlockPos::new(44, 70, -10)),
            Some(portal)
        );

        // A stale index entry whose block is gone must not be returned — the arm
        // that makes the re-validation load-bearing rather than decorative.
        let stale = PortalIndex::new();
        stale.insert(Dimension::Nether, BlockPos::new(41, 71, -12));
        assert_eq!(
            find_exit_portal(&world, Dimension::Nether, Some(&stale), BlockPos::new(41, 71, -12)),
            Some(portal),
            "a stale entry falls through to the scan rather than teleporting into rock"
        );
    }
}
