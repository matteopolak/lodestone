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
use crate::neighbor_update::Direction;
use crate::redstone::{base_name, direction_from_str, direction_to_str, get_bool_property, get_str_property};

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

/// Vanilla's `NetherPortalBlock.updateShape` — whether the portal cell at
/// `pos` (on `axis`) must be extinguished (replaced with air) because its
/// neighbour in `direction_to_neighbour` just changed to `neighbour_state`.
///
/// ```java
/// Direction.Axis updateAxis = directionToNeighbour.getAxis();
/// Direction.Axis axis = state.getValue(AXIS);
/// boolean wrongAxis = axis != updateAxis && updateAxis.isHorizontal();
/// return !wrongAxis && !neighbourState.is(this) && !PortalShape.findAnyShape(level, pos, axis).isComplete()
///    ? Blocks.AIR.defaultBlockState()
///    : super.updateShape(...);
/// ```
///
/// Three clauses, all required (this crate's own evidence standard: a
/// conjunction ported as "the interesting clause" silently drops the other
/// two the day an input needs them):
///
/// 1. **`!wrongAxis`** — vanilla's `Direction.Axis` has three values (X, Y,
///    Z) where this crate's [`Axis`] only models the horizontal two, so it is
///    expressed here as "vertical, or horizontal along the portal's own
///    axis". A neighbour change *perpendicular* to the portal's plane (in
///    front of or behind it) cannot have touched the frame and is skipped
///    outright — this is what stops a torch placed against the portal's
///    face from re-triggering a frame scan on every flicker.
/// 2. **`!neighbourState.is(this)`** — [`is_portal`]: a notification from
///    another portal cell (not a frame block) needs no re-validation: an
///    interior cell changing does not mean the frame broke.
/// 3. **`!findAnyShape(...).isComplete()`** — [`find_any_shape`] rebuilt on
///    `axis`, `pos`'s own frame is still intact. `isComplete`, not
///    `isValid`: a frame whose obsidian is intact but has lost a *portal*
///    block (through some other cell already having been cleared) must also
///    extinguish this one, matching vanilla exactly — see
///    [`PortalShape::is_complete`]'s own doc comment.
///
/// # Dependencies
///
/// [`find_any_shape`] for the frame re-scan; [`crate::neighbor_update::Direction`]
/// for `direction_to_neighbour`, matching the shape every other neighbour-change
/// reaction in this crate ([`crate::server::collapse_unsupported`],
/// `crate::random_tick::react_to_notification`) already uses.
#[must_use]
pub fn should_extinguish<S: ChunkSource + ?Sized>(
    world: &S,
    dimension: Dimension,
    pos: BlockPos,
    axis: Axis,
    direction_to_neighbour: Direction,
    neighbour_state: &str,
) -> bool {
    // `updateAxis.isHorizontal()`: only North/South (Z) and East/West (X)
    // qualify: Up/Down is vertical, so `wrong_axis` is unconditionally
    // `false` for them regardless of the match below — a broken block above
    // or below a portal cell must always be re-validated.
    let horizontal_matches_axis = match (axis, direction_to_neighbour) {
        (Axis::X, Direction::West | Direction::East) => true,
        (Axis::Z, Direction::North | Direction::South) => true,
        _ => false,
    };
    let update_is_horizontal = !matches!(direction_to_neighbour, Direction::Up | Direction::Down);
    let wrong_axis = update_is_horizontal && !horizontal_matches_axis;
    if wrong_axis {
        return false;
    }
    if is_portal(neighbour_state) {
        return false;
    }
    !find_any_shape(world, dimension, pos, axis).is_complete()
}

/// Vanilla's `maxChainedNeighborUpdates` for this cascade specifically —
/// mirrors [`crate::server::collapse_unsupported`]'s own `MAX_SUPPORT_COLLAPSE`
/// for the identical reason: a runaway guard rather than a behavioural limit.
/// The tallest/widest real portal is 21×21 = 441 interior cells, so this bound
/// is never reached by any frame [`PortalShape::is_valid`] would accept —
/// only a `crate::block_support`-style data error in [`is_portal`] could walk
/// the world, and this stops it rather than assuming it cannot happen.
const MAX_PORTAL_EXTINGUISH_CASCADE: usize = 1024;

/// Runs [`should_extinguish`] outward from `origin` — the cell that just
/// changed (typically just written to air or a fluid by a break) —
/// extinguishing (writing air, in `world`) any `nether_portal` neighbour
/// whose frame no longer validates, and **cascading**: each cell this
/// extinguishes is itself a change, so its own neighbours are re-checked in
/// turn, exactly the shape [`crate::server::collapse_unsupported`]'s queue
/// already uses for the gravity/attachment-support cascade.
///
/// The cascade is what makes mining *one* frame block clear an entire
/// multi-cell portal rather than only the one interior cell nearest the
/// break: vanilla's `setBlock` always re-runs `updateNeighbourShapes` on the
/// cell it just changed, so extinguishing portal cell A (adjacent to the
/// broken frame block) asks portal cell B (adjacent to A, part of the same
/// portal) the same question — B's own [`find_any_shape`] rescan now finds
/// one fewer `portal_blocks` than its rectangle needs, so
/// [`PortalShape::is_complete`] is false for B too, and so on until the
/// whole rectangle has cleared.
///
/// This is the caller [`should_extinguish`] itself cannot be: vanilla's
/// `updateNeighbourShapes` runs `updateShape` on **every** direct neighbour of
/// a changed cell, not just ones a support table names (unlike
/// [`crate::server::collapse_unsupported`], which is scoped to
/// [`crate::block_support`]'s survives table and does not know about
/// `nether_portal` at all — a portal frame is not "supported by one specific
/// neighbour", it is re-validated against its whole rectangle). A block break
/// or placement's caller should run this alongside that pass, on the same
/// `origin`.
///
/// Returns `(pos, state_before)` for each cell extinguished, matching
/// [`crate::server::collapse_unsupported`]'s own return shape so a caller can
/// feed both into the same "send a `block_update`, roll the loot (there is
/// none here), relight" pipeline without a second code path.
#[must_use]
pub fn extinguish_broken_frames<S: ChunkSource + ?Sized>(
    world: &S,
    dimension: Dimension,
    origin: BlockPos,
) -> Vec<(BlockPos, String)> {
    use crate::neighbor_update::{ALL_DIRECTIONS, Notification};
    use std::collections::VecDeque;

    let mut removed: Vec<(BlockPos, String)> = Vec::new();
    // Seeded exactly like `NeighborPropagator::propagate`'s own fan-out: one
    // `Notification` per direction off `origin`, `from` carrying the
    // direction that produced it — see that type's own doc comment for the
    // "from is the causing direction" convention this reuses rather than
    // reinventing.
    let mut queue: VecDeque<Notification> = ALL_DIRECTIONS
        .iter()
        .map(|&from| Notification { pos: from.relative(origin), from })
        .collect();

    while let Some(n) = queue.pop_front() {
        if removed.len() >= MAX_PORTAL_EXTINGUISH_CASCADE {
            tracing::warn!(
                "nether portal extinguish cascade from {origin:?} hit its \
                 {MAX_PORTAL_EXTINGUISH_CASCADE}-cell bound"
            );
            break;
        }
        if removed.iter().any(|(seen, _)| *seen == n.pos) {
            continue;
        }
        let state = world.block_state(n.pos.x, n.pos.y, n.pos.z);
        if !is_portal(&state) {
            continue;
        }
        let axis = Axis::from_state(&state);
        // The direction *from this cell* to whichever neighbour just
        // changed — the opposite of `n.from`, which names the direction
        // *into* `n.pos` the change arrived from. See `Notification`'s own
        // doc comment.
        let direction_to_neighbour = n.from.opposite();
        let causing_pos = direction_to_neighbour.relative(n.pos);
        let causing_state = world.block_state(causing_pos.x, causing_pos.y, causing_pos.z);
        if !should_extinguish(world, dimension, n.pos, axis, direction_to_neighbour, &causing_state) {
            continue;
        }
        world.set_block(n.pos.x, n.pos.y, n.pos.z, "minecraft:air");
        removed.push((n.pos, state));
        for &from in &ALL_DIRECTIONS {
            queue.push_back(Notification { pos: from.relative(n.pos), from });
        }
    }
    removed
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
    // `inPortalDimension`: only the overworld and the Nether qualify — vanilla's
    // `BaseFireBlock.onPlace` guard, `Level.getRespawnData().dimension() ==
    // Level.OVERWORLD || level.dimension() == Level.NETHER`. An End portal is a
    // different mechanism entirely (`EndPortalFrameBlock`'s eye-of-ender ring, not
    // fire), so this declines rather than searching for a frame that fire cannot
    // light.
    match dimension {
        Dimension::Overworld | Dimension::Nether => {}
        Dimension::End => return None,
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
/// **Persisted across a restart now.**
/// [`crate::integrated::IntegratedServer::open_persistent_with_mobs`] restores
/// this index at world open from [`crate::poi_storage::PoiStorage::load_all`]
/// for every dimension, and its autosave task and shutdown path write it back
/// through [`poi_chunks_for_index`] on the same schedule
/// [`crate::entity_storage::EntityStorage`]'s own wiring uses — see that
/// constructor for the exact sequence. [`poi_records_for_index`],
/// [`restore_index_from_poi`] and [`poi_chunks_for_index`] are the three
/// conversions, proven by a real round trip through
/// [`crate::poi_storage::PoiStorage`] in `tests/poi_persistence_round_trip.rs`
/// and by a full restart in `tests/portal_persistence_restart.rs`. All three
/// are native-only (`#[cfg(not(target_arch = "wasm32"))]`), matching
/// [`crate::poi_storage`] itself, which does not exist on `wasm32` at all —
/// this module is *not* gated (portals work in a browser singleplayer world
/// too), so leaving these three ungated was a real, if quiet, wasm32 compile
/// break: `crate::poi_storage::PoiRecord`/`PoiSection` are configured out of
/// that target entirely, and nothing in `just check`/`just health` builds for
/// it — only `just wasm-check` (or `cargo check --target wasm32-unknown-unknown`)
/// would have caught it.
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
///
/// Native only — see [`PortalIndex`]'s own doc for why.
#[cfg(not(target_arch = "wasm32"))]
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
///
/// Native only — see [`PortalIndex`]'s own doc for why.
#[cfg(not(target_arch = "wasm32"))]
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

/// Groups [`poi_records_for_index`]'s output into the `(chunk_x, chunk_z)` →
/// [`crate::poi_storage::PoiChunk`] map [`crate::poi_storage::PoiStorage::save`]
/// wants — the write half of the wire [`restore_index_from_poi`] is the read
/// half of.
///
/// [`insert_record`](crate::poi_storage::PoiSection::insert_record), not
/// [`add`](crate::poi_storage::PoiSection::add): every cell here came from a
/// live index or a previous reload, not a block just discovered, so its
/// `free_tickets` (always `0` for a portal — see [`poi_records_for_index`])
/// must be kept rather than reset. Using `add` here would compile, save, and
/// reload cleanly, and still be wrong the moment this index ever tracks a
/// claimable type.
///
/// Native only — see [`PortalIndex`]'s own doc for why.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn poi_chunks_for_index(
    index: &PortalIndex,
    dimension: Dimension,
) -> HashMap<(i32, i32), crate::poi_storage::PoiChunk> {
    let mut out: HashMap<(i32, i32), crate::poi_storage::PoiChunk> = HashMap::new();
    for record in poi_records_for_index(index, dimension) {
        let chunk_pos = record.pos.chunk_pos();
        let section_y = record.pos.section_pos().y;
        let chunk = out.entry((chunk_pos.x, chunk_pos.z)).or_default();
        chunk
            .sections
            .entry(section_y)
            .or_insert_with(crate::poi_storage::PoiSection::new)
            .insert_record(record);
    }
    out
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
        // A paired-portal search never runs for the End: every arrival is the
        // fixed obsidian platform at `Dimension::end_spawn_point`, not a nearby
        // portal `find_exit_portal` looks for. Callers must not reach this arm —
        // see `create_end_platform` for the End's actual arrival rule.
        Dimension::End => unreachable!(
            "the End does not use paired-portal search; see create_end_platform"
        ),
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
#[cfg(not(target_arch = "wasm32"))]
fn columns_touched_by_the_site_search(origin: BlockPos) -> Vec<(i32, i32)> {
    // Deterministic from `origin` alone: every `(dx, dz)` in the 33 x 33 offsets
    // square below maps onto one of these, regardless of what the search finds —
    // see `create_portal`'s own doc comment for why nothing here can be
    // shortcut by an early exit.
    let mut seen = std::collections::HashSet::with_capacity(16);
    let mut out = Vec::with_capacity(16);
    for dx in -16..=16 {
        for dz in -16..=16 {
            let chunk = ((origin.x + dx).div_euclid(16), (origin.z + dz).div_euclid(16));
            if seen.insert(chunk) {
                out.push(chunk);
            }
        }
    }
    out
}

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

    // Warm every column the scan below is about to touch, **in parallel**,
    // before walking them one `block_state` read at a time.
    //
    // The loop below visits every one of the 33 x 33 offsets unconditionally —
    // vanilla's own `PortalForcer.createPortal` does too (see this function's
    // doc comment: "both visit the same set of columns"), so there is no early
    // exit to lose by prefetching everything up front. For a *destination*
    // dimension nothing has looked at yet (the common case: a player's first
    // trip into a fresh Nether), that footprint spans a handful of un-generated
    // chunk columns, and `crate::chunk_store`'s own module doc measures a single
    // fresh column at **~909 ms**. Touched serially, one `block_state` call at a
    // time, a first trip was paying that cost N times over before any packet
    // told the client it had arrived — the same "how much work sits inside one
    // unserviced window" shape as the join-strip stall this crate already fixed
    // once (`DESIGN.md` §12.165): offloading the caller to a blocking thread (see
    // `server::travel_through_portal`) does not shorten a suspension point, only
    // moving the work off the wall-clock the search actually spends does.
    //
    // `is_column_resident` is the cheap pre-check `crate::chunk::ChunkSource`
    // already exists for exactly this reason (no generation on a hit), so a
    // *warm* dimension — every return trip, and every outbound trip after the
    // first — pays only that check and skips the parallel fan-out entirely.
    // Native only: `generate_columns_parallel` fans out over
    // `std::thread::scope`, which is `Builder::spawn`'s panic-on-`Err` call
    // site on `wasm32-unknown-unknown` (no threads there at all) — see this
    // crate's wasm hazard notes. Skipping it there costs nothing beyond the
    // serial cost the scan already pays; it buys nothing either, since a
    // browser singleplayer world has no second core to fan out to.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let stale: Vec<(i32, i32)> = columns_touched_by_the_site_search(origin)
            .into_iter()
            .filter(|&(cx, cz)| !world.is_column_resident(cx, cz))
            .collect();
        if !stale.is_empty() {
            let _ = crate::chunk::generate_columns_parallel(world, &stale);
        }
    }

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

/// `minecraft:end_portal` — the block a completed end-portal-frame ring fills its
/// 3×3 interior with, and the block stepping into which triggers a trip to the
/// End (`EndPortalBlock.entityInside`).
pub const END_PORTAL_BLOCK: &str = "minecraft:end_portal";

/// `minecraft:end_portal_frame` — the block the stronghold portal room's 5×5 ring
/// is built from (`EndPortalFrameBlock`). Its `eye` property is what
/// `EnderEyeItem.useOn` flips true; this crate has no code that flips it (see
/// this module's doc for exactly what is and is not implemented here).
pub const END_PORTAL_FRAME_BLOCK: &str = "minecraft:end_portal_frame";

/// Whether `state` is an `end_portal` block. Unlike [`is_portal`], the End's
/// block has no `axis` (or any other) property, so this is a plain equality.
#[must_use]
pub fn is_end_portal(state: &str) -> bool {
    state == END_PORTAL_BLOCK
}

/// The blocks [`ensure_end_platform`] writes for the fixed 5×5×4 obsidian
/// platform every End arrival stands on — `EndPlatformFeature.createEndPlatform`,
/// ported field for field rather than reasoned about, since a transposed loop
/// bound here either strands the player over the void or buries them in
/// obsidian.
///
/// `origin` is the platform's **obsidian layer**, not the spawn point itself:
/// `EndPortalBlock.getPortalDestination` calls this with
/// `BlockPos.containing(Vec3.atBottomCenterOf(END_SPAWN_POINT)).below()`, which
/// for the integer constant `(100, 50, 0)` is `(100, 49, 0)` — one block *below*
/// [`crate::dimension::Dimension::end_spawn_point`], not the point itself.
///
/// The loop is `dz`/`dx` in `-2..=2` (5×5) and `dy` in `-1..3` (4 tall): obsidian
/// at `dy == -1`, air at `dy` 0, 1, 2. 100 cells total, always — this does not
/// stop early on an already-correct cell, unlike vanilla's own `!is(block)` guard
/// (see [`ensure_end_platform`] for the version that keeps it).
#[must_use]
pub fn end_platform_writes(origin: BlockPos) -> Vec<(BlockPos, &'static str)> {
    let mut writes = Vec::with_capacity(5 * 5 * 4);
    for dz in -2..=2 {
        for dx in -2..=2 {
            for dy in -1..3 {
                let block = if dy == -1 { "minecraft:obsidian" } else { "minecraft:air" };
                writes.push((BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz), block));
            }
        }
    }
    writes
}

/// Builds (or repairs) the End's fixed arrival platform through `world`,
/// skipping any cell that already holds the target block — vanilla's own
/// `!newLevel.getBlockState(blockPos).is(block)` guard on
/// `EndPlatformFeature.createEndPlatform`, kept so a second arrival does not
/// rewrite 100 already-correct cells (and, more importantly, does not
/// re-destroy a floor a player has built on top of the platform since their
/// first visit — vanilla's guard is precisely what makes that survive).
///
/// Called from `crate::server`'s `travel_through_end_portal`, the End's
/// counterpart to `travel_through_portal`, via [`end_portal_arrival`]'s own
/// `platform_origin`.
pub fn ensure_end_platform<S: ChunkSource + ?Sized>(world: &S, origin: BlockPos) {
    for (pos, block) in end_platform_writes(origin) {
        if world.block_state(pos.x, pos.y, pos.z) != block {
            world.set_block(pos.x, pos.y, pos.z, block);
        }
    }
}

/// Where a trip **into** the End lands — `EndPortalBlock.getPortalDestination`'s
/// `fromEnd == false` arm, restricted to the `ServerPlayer` branch (the only
/// entity kind this crate teleports through a portal).
///
/// Returns `(platform_origin, arrival)`:
/// * `platform_origin` is [`ensure_end_platform`]'s own parameter — the
///   obsidian layer's `y`, which is `end_spawn_point().1 - 1`
///   (`BlockPos.containing(Vec3.atBottomCenterOf(spawnBlockPos)).below()` for
///   the integer constant `(100, 50, 0)`).
/// * `arrival` is where the *player* materialises: **one block below**
///   `end_spawn_point()` itself, standing on the obsidian floor rather than
///   floating over it. Vanilla applies this only for `entity instanceof
///   ServerPlayer` — `spawnPos.subtract(0.0, 1.0, 0.0)` — every other entity
///   arrives a block higher, which this function does not model since this
///   crate only teleports players through a portal.
///
/// Dropping the subtraction is the plausible off-by-one here: it reads as
/// "the platform and the arrival point obviously share a `y`," and the two
/// are one block apart in vanilla specifically because the platform's
/// obsidian is *two* blocks below `end_spawn_point()`, not one — see
/// [`end_platform_writes`]'s own doc for why the platform's origin is
/// already offset once before this function offsets it again.
#[must_use]
pub fn end_portal_arrival() -> (BlockPos, lodestone_model::Vec3) {
    let (sx, sy, sz) = Dimension::end_spawn_point();
    let platform_origin = BlockPos::new(sx, sy - 1, sz);
    let arrival =
        lodestone_model::Vec3::new(f64::from(sx) + 0.5, f64::from(sy - 1), f64::from(sz) + 0.5);
    (platform_origin, arrival)
}

/// The result of successfully placing an eye of ender into an
/// `end_portal_frame` — `EnderEyeItem.useOn`.
#[derive(Debug, Clone)]
pub struct EndPortalIgnition {
    /// The frame cell's own new state (`eye=true`) — always present, whether
    /// or not this eye completed a ring.
    pub frame: (BlockPos, String),
    /// The 3×3 interior `end_portal` cells, present only when this eye
    /// completed a full 5×5 ring of 12 correctly-facing, already-eyed frames.
    pub portal_fill: Option<Vec<(BlockPos, String)>>,
}

/// Whether `state` is an `end_portal_frame` block, of any facing or eye value.
#[must_use]
pub fn is_end_portal_frame(state: &str) -> bool {
    base_name(state) == END_PORTAL_FRAME_BLOCK
}

/// The canonical `end_portal_frame` state string for `facing`/`eye`.
#[must_use]
pub fn end_portal_frame_state(facing: Direction, eye: bool) -> String {
    format!(
        "{END_PORTAL_FRAME_BLOCK}[eye={eye},facing={}]",
        direction_to_str(facing)
    )
}

/// An eye of ender used on `pos` — `EnderEyeItem.useOn`. `None` when `pos` is
/// not an *unfired* `end_portal_frame` (not a frame at all, or one that
/// already carries an eye), vanilla's `InteractionResult.PASS` guard.
///
/// Otherwise always returns the frame's own `eye=true` write, plus — when
/// this eye completes a full ring — the 3×3 interior `end_portal` cells. The
/// caller (`crate::server`'s `apply_use_item_on`) owns every write and the
/// item's consumption, the same division `ignite` uses for the Nether.
///
/// # Why this is not vanilla's generic `BlockPattern` search
///
/// `EndPortalFrameBlock.getOrCreatePortalShape` builds a reusable
/// `BlockPattern` — Mojang's general multi-block matcher, also used for the
/// iron golem and the wither — and `find` searches a translated, rotated
/// window for it. Porting that generic engine for one fixed 5×5 pattern would
/// be porting infrastructure this crate has no other user for. What matters
/// is the *result* of applying it to this one pattern, and that result is a
/// single geometric rule, derived (not assumed) from
/// `BlockPattern.translateAndRotate`'s cross-product construction applied to
/// this pattern's aisle string
/// (`"?vvv?" / ">???<" ×3 / "?^^^?"`, `'v'`→`FACING NORTH`, `'^'`→`SOUTH`,
/// `'>'`→`WEST`, `'<'`→`EAST`): **every one of the 12 rim frames must be eyed
/// and must face the ring's centre** — the north edge (`min_z`) requires
/// `facing=south`, the south edge `facing=north`, the west edge `facing=east`,
/// the east edge `facing=west`. This is exactly the "arrow points inward"
/// rule every vanilla stronghold portal room exhibits, not folklore standing
/// in for it — every one of the pattern's 8 valid `(forwards, up)`
/// orientation branches produces this same characterization, because "faces
/// the centre" is invariant under the rotations and reflections the generic
/// search tries.
///
/// `pos`'s own facing pins which edge it can be on (a frame facing south can
/// only be a north-edge cell), so the search below only has to try the three
/// lateral offsets along that one edge — not the full 24-orientation sweep
/// `BlockPattern.find` performs.
#[must_use]
pub fn ignite_end_portal_frame<S: ChunkSource + ?Sized>(
    world: &S,
    pos: BlockPos,
) -> Option<EndPortalIgnition> {
    let state = world.block_state(pos.x, pos.y, pos.z);
    if !is_end_portal_frame(&state) || get_bool_property(&state, "eye") == Some(true) {
        return None;
    }
    let facing = direction_from_str(get_str_property(&state, "facing").unwrap_or("north"));
    let new_state = end_portal_frame_state(facing, true);
    let portal_fill = find_completed_ring(world, pos, facing, &new_state);
    Some(EndPortalIgnition {
        frame: (pos, new_state),
        portal_fill,
    })
}

/// The 5×5 ring's `(min_x, min_z)` corner for `pos` sitting at lateral
/// `offset` (1, 2 or 3) along the one edge its `facing` pins it to. `None` for
/// a non-horizontal facing, which cannot occur for a real `end_portal_frame`
/// state (its own `FACING` property is `HorizontalDirectionalBlock`'s).
fn ring_origin_for(pos: BlockPos, facing: Direction, offset: i32) -> Option<(i32, i32)> {
    match facing {
        // A south-facing frame points into the ring from its north edge
        // (min_z); the frame itself sits at that edge's `offset`-th column.
        Direction::South => Some((pos.x - offset, pos.z)),
        Direction::North => Some((pos.x - offset, pos.z - 4)),
        Direction::East => Some((pos.x, pos.z - offset)),
        Direction::West => Some((pos.x - 4, pos.z - offset)),
        Direction::Up | Direction::Down => None,
    }
}

/// Whether every one of the 12 rim cells of the 5×5 ring anchored at
/// `(min_x, min_z, y)` is an eyed `end_portal_frame` facing the ring's
/// centre. `override_pos`/`override_state` stand in for the cell an eye was
/// just placed into — vanilla writes that state to the world **before**
/// running the pattern search (`level.setBlock` precedes
/// `getOrCreatePortalShape().find`), so the search must see it too.
fn ring_is_complete<S: ChunkSource + ?Sized>(
    world: &S,
    min_x: i32,
    min_z: i32,
    y: i32,
    override_pos: BlockPos,
    override_state: &str,
) -> bool {
    let read = |x: i32, z: i32| -> String {
        if x == override_pos.x && z == override_pos.z {
            override_state.to_owned()
        } else {
            world.block_state(x, y, z)
        }
    };
    let faces_centre = |x: i32, z: i32, want: Direction| -> bool {
        let state = read(x, z);
        is_end_portal_frame(&state)
            && get_bool_property(&state, "eye") == Some(true)
            && get_str_property(&state, "facing") == Some(direction_to_str(want))
    };
    let max_x = min_x + 4;
    let max_z = min_z + 4;
    for x in (min_x + 1)..=(min_x + 3) {
        if !faces_centre(x, min_z, Direction::South) || !faces_centre(x, max_z, Direction::North) {
            return false;
        }
    }
    for z in (min_z + 1)..=(min_z + 3) {
        if !faces_centre(min_x, z, Direction::East) || !faces_centre(max_x, z, Direction::West) {
            return false;
        }
    }
    true
}

/// The 3×3 interior of the ring anchored at `(min_x, min_z, y)` — the cells
/// [`ignite_end_portal_frame`] fills with [`END_PORTAL_BLOCK`] once the ring
/// is complete. `EnderEyeItem.useOn`'s `match.getFrontTopLeft().offset(-3, 0,
/// -3)` plus its `0..3 × 0..3` loop, re-derived from the ring's own bounding
/// box rather than from `frontTopLeft` (which this module never constructs).
fn interior_fill(min_x: i32, min_z: i32, y: i32) -> Vec<(BlockPos, String)> {
    let mut cells = Vec::with_capacity(9);
    for x in (min_x + 1)..=(min_x + 3) {
        for z in (min_z + 1)..=(min_z + 3) {
            cells.push((BlockPos::new(x, y, z), END_PORTAL_BLOCK.to_owned()));
        }
    }
    cells
}

/// Tries the three lateral offsets `pos` (now carrying `new_state_for_pos`)
/// could sit at along the one edge its facing pins it to, and returns the
/// interior fill for the first that completes a ring.
fn find_completed_ring<S: ChunkSource + ?Sized>(
    world: &S,
    pos: BlockPos,
    facing: Direction,
    new_state_for_pos: &str,
) -> Option<Vec<(BlockPos, String)>> {
    for offset in 1..=3 {
        let (min_x, min_z) = ring_origin_for(pos, facing, offset)?;
        if ring_is_complete(world, min_x, min_z, pos.y, pos, new_state_for_pos) {
            return Some(interior_fill(min_x, min_z, pos.y));
        }
    }
    None
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

    /// Directly exercises [`should_extinguish`]'s `wrongAxis`
    /// clause against a fixture that is **genuinely** broken (a frame block
    /// removed by hand, independent of [`extinguish_broken_frames`] entirely)
    /// — so the same-axis and perpendicular answers on the *identical* input
    /// must disagree, proving the perpendicular direction really is declined
    /// by the axis check and not merely by a frame that happens to still be
    /// complete.
    #[test]
    fn should_extinguish_only_checks_the_frame_on_a_matching_or_vertical_axis() {
        let world = FlatWorld::new();
        world.frame(0, 70, 0, Axis::X, 2, 3);
        for (pos, state) in ignite(&world, Dimension::Overworld, BlockPos::new(0, 70, 0)).unwrap() {
            world.set_block(pos.x, pos.y, pos.z, &state);
        }
        // Break the bottom-middle frame cell directly, bypassing
        // `extinguish_broken_frames` entirely, so the frame is genuinely
        // incomplete going into this test rather than assumed to be.
        world.set_block(0, 69, 0, "minecraft:air");
        let shape = find_any_shape(&world, Dimension::Overworld, BlockPos::new(0, 70, 0), Axis::X);
        assert!(!shape.is_complete(), "fixture setup: the frame must actually be broken here");

        let interior = BlockPos::new(0, 70, 0);
        assert!(
            should_extinguish(&world, Dimension::Overworld, interior, Axis::X, Direction::West, "minecraft:air"),
            "a same-axis (East/West) neighbour must reach the real, broken frame"
        );
        assert!(
            !should_extinguish(&world, Dimension::Overworld, interior, Axis::X, Direction::North, "minecraft:air"),
            "a perpendicular (North/South) neighbour must decline without consulting \
             the frame at all -- on the *same* broken fixture the same-axis case above \
             correctly extinguishes"
        );
        assert!(
            should_extinguish(&world, Dimension::Overworld, interior, Axis::X, Direction::Down, "minecraft:air"),
            "vertical is never wrongAxis and must also reach the real frame"
        );
    }

    /// [`should_extinguish`]'s second clause: a neighbour that is
    /// itself a portal cell must decline, even reaching a genuinely broken
    /// frame — paired against the non-portal neighbour case on the identical
    /// fixture so the two answers must disagree.
    #[test]
    fn should_extinguish_declines_when_the_neighbour_is_itself_a_portal_cell() {
        let world = FlatWorld::new();
        world.frame(0, 70, 0, Axis::X, 2, 3);
        for (pos, state) in ignite(&world, Dimension::Overworld, BlockPos::new(0, 70, 0)).unwrap() {
            world.set_block(pos.x, pos.y, pos.z, &state);
        }
        world.set_block(0, 69, 0, "minecraft:air");
        let interior = BlockPos::new(0, 70, 0);
        assert!(
            should_extinguish(&world, Dimension::Overworld, interior, Axis::X, Direction::West, "minecraft:air"),
            "a non-portal neighbour reaches the real, broken frame"
        );
        assert!(
            !should_extinguish(
                &world,
                Dimension::Overworld,
                interior,
                Axis::X,
                Direction::West,
                "minecraft:nether_portal[axis=x]"
            ),
            "a neighbour that is itself a portal cell must decline, on the same broken fixture"
        );
    }

    /// The end-to-end shape: mining a single frame block clears
    /// every interior cell of the portal it supported, not just the one cell
    /// touching the break -- vanilla's `setBlock` re-triggers `updateShape`
    /// on the cell it just changed, cascading through the whole rectangle.
    ///
    /// Predicts the exact count (6, the full 2x3 interior) rather than merely
    /// "at least one", which is what a single-hop (non-cascading) mistake
    /// would satisfy -- a single-hop implementation extinguishes only the one
    /// interior cell directly adjacent to the broken frame block and leaves
    /// the other five lit.
    #[test]
    fn breaking_one_frame_block_extinguishes_the_whole_portal() {
        let world = FlatWorld::new();
        world.frame(0, 70, 0, Axis::X, 2, 3);
        for (pos, state) in ignite(&world, Dimension::Overworld, BlockPos::new(0, 70, 0)).unwrap() {
            world.set_block(pos.x, pos.y, pos.z, &state);
        }
        let shape_before = find_any_shape(&world, Dimension::Overworld, BlockPos::new(0, 70, 0), Axis::X);
        assert_eq!(shape_before.portal_blocks(), 6, "fixture setup: the full 2x3 interior is lit");

        // The bottom-middle frame obsidian, adjacent to interior cell (0, 70, 0)
        // -- the caller's own `set_block` (already-broken, matching every real
        // call site: `destroy_block` writes the break before running this).
        let broken = BlockPos::new(0, 69, 0);
        world.set_block(broken.x, broken.y, broken.z, "minecraft:air");

        let removed = extinguish_broken_frames(&world, Dimension::Overworld, broken);
        assert_eq!(removed.len(), 6, "every interior cell of the 2x3 portal must extinguish");
        let mut removed_positions: Vec<BlockPos> = removed.iter().map(|(pos, _)| *pos).collect();
        removed_positions.sort_by_key(|p| (p.x, p.y, p.z));
        let mut expected: Vec<BlockPos> = (0..2)
            .flat_map(|x| (70..73).map(move |y| BlockPos::new(x, y, 0)))
            .collect();
        expected.sort_by_key(|p| (p.x, p.y, p.z));
        assert_eq!(removed_positions, expected, "exactly the 2x3 interior, no more and no less");
        for (pos, state) in &removed {
            assert!(is_portal(state), "each removed cell's *recorded* prior state must have been a real portal block: {pos:?} was {state:?}");
        }
        for y in 70..73 {
            for x in 0..2 {
                assert_eq!(
                    world.block_state(x, y, 0),
                    "minecraft:air",
                    "cell ({x}, {y}, 0) must have been written to air"
                );
            }
        }
    }

    /// The control for the cascade test above: breaking a frame block on one
    /// portal must not touch a second, unrelated portal far away -- rules out
    /// a bug where the cascade queue is unbounded by distance/dimension
    /// rather than by "is this cell actually a neighbour of something that
    /// changed".
    #[test]
    fn extinguishing_one_portal_does_not_touch_an_unrelated_one() {
        let world = FlatWorld::new();
        world.frame(0, 70, 0, Axis::X, 2, 3);
        world.frame(500, 70, 500, Axis::X, 2, 3);
        for (pos, state) in ignite(&world, Dimension::Overworld, BlockPos::new(0, 70, 0)).unwrap() {
            world.set_block(pos.x, pos.y, pos.z, &state);
        }
        for (pos, state) in ignite(&world, Dimension::Overworld, BlockPos::new(500, 70, 500)).unwrap() {
            world.set_block(pos.x, pos.y, pos.z, &state);
        }

        let broken = BlockPos::new(0, 69, 0);
        world.set_block(broken.x, broken.y, broken.z, "minecraft:air");
        let removed = extinguish_broken_frames(&world, Dimension::Overworld, broken);
        assert_eq!(removed.len(), 6, "the near portal still fully extinguishes");

        let far_shape = find_any_shape(&world, Dimension::Overworld, BlockPos::new(500, 70, 500), Axis::X);
        assert!(far_shape.is_complete(), "the unrelated portal must still be fully lit");
        assert_eq!(far_shape.portal_blocks(), 6);
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

    /// [`poi_chunks_for_index`] is the write half of the wire
    /// [`restore_index_from_poi`] is the read half of — this proves the pair
    /// round-trips through a real [`crate::poi_storage::PoiStorage`], not
    /// just through each other in memory.
    ///
    /// Pairwise-distinct positions across chunk **and** section boundaries
    /// (same standard `poi_storage.rs`'s own chunk-round-trip test holds
    /// itself to), so a transposition of chunk-x/chunk-z or section-y cannot
    /// survive unnoticed.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn poi_chunks_for_index_round_trips_through_a_real_poi_store() {
        let dir = std::env::temp_dir().join("lodestone-portal-poi-chunks-b2w7");
        let _ = std::fs::remove_dir_all(&dir);
        let storage =
            crate::poi_storage::PoiStorage::new(&dir, Dimension::Overworld).expect("create");

        let index = PortalIndex::new();
        let cells = [
            BlockPos::new(4001, -40, -19),
            BlockPos::new(-385, 71, -897),
        ];
        index.extend(Dimension::Overworld, cells);

        let chunks = poi_chunks_for_index(&index, Dimension::Overworld);
        assert_eq!(
            chunks
                .values()
                .map(|c| c.sections.values().map(|s| s.records.len()).sum::<usize>())
                .sum::<usize>(),
            cells.len()
        );
        let written = storage.save(&chunks).expect("save");
        assert_eq!(written, cells.len());

        let (cx0, cz0) = (cells[0].x >> 4, cells[0].z >> 4);
        let (cx1, cz1) = (cells[1].x >> 4, cells[1].z >> 4);
        let loaded0 = storage.load_chunk(cx0, cz0).expect("load first chunk");
        let loaded1 = storage.load_chunk(cx1, cz1).expect("load second chunk");
        let rebuilt = restore_index_from_poi(
            Dimension::Overworld,
            loaded0
                .sections
                .values()
                .chain(loaded1.sections.values()),
        );
        let mut got = rebuilt.cells(Dimension::Overworld);
        got.sort_by_key(|p| (p.x, p.y, p.z));
        let mut want = cells.to_vec();
        want.sort_by_key(|p| (p.x, p.y, p.z));
        assert_eq!(got, want);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `end_platform_writes`'s shape against the geometry read directly out of
    /// `EndPlatformFeature.createEndPlatform` (`dz`/`dx` in `-2..=2`, `dy` in
    /// `-1..3`): 100 cells, exactly 25 obsidian (the `dy == -1` layer) and 75 air
    /// (three layers of 25), and the two corner cells that a transposed `dx`/`dz`
    /// loop bound would put in the wrong place.
    #[test]
    fn the_end_platform_is_a_five_by_five_obsidian_slab_under_three_layers_of_air() {
        let origin = BlockPos::new(100, 49, 0);
        let writes = end_platform_writes(origin);
        assert_eq!(writes.len(), 100, "5 * 5 * 4 cells");

        let obsidian = writes.iter().filter(|(_, b)| *b == "minecraft:obsidian").count();
        let air = writes.iter().filter(|(_, b)| *b == "minecraft:air").count();
        assert_eq!(obsidian, 25, "one 5x5 layer of obsidian, at dy = -1");
        assert_eq!(air, 75, "three 5x5 layers of air, at dy = 0, 1, 2");

        // The obsidian layer's own y is origin.y - 1; the air above spans
        // origin.y ..= origin.y + 1.
        let obsidian_y: std::collections::BTreeSet<i32> = writes
            .iter()
            .filter(|(_, b)| *b == "minecraft:obsidian")
            .map(|(p, _)| p.y)
            .collect();
        assert_eq!(obsidian_y, std::collections::BTreeSet::from([origin.y - 1]));
        let air_y: std::collections::BTreeSet<i32> = writes
            .iter()
            .filter(|(_, b)| *b == "minecraft:air")
            .map(|(p, _)| p.y)
            .collect();
        assert_eq!(air_y, std::collections::BTreeSet::from([origin.y, origin.y + 1, origin.y + 2]));

        // The horizontal extent is exactly [-2, 2] on both axes — a corner and an
        // edge-midpoint, pairwise-distinct so a transposed dx/dz cannot survive.
        let has = |dx: i32, dz: i32| {
            writes.iter().any(|(p, _)| p.x == origin.x + dx && p.z == origin.z + dz)
        };
        assert!(has(-2, -2) && has(2, 2), "the far corners must be present");
        assert!(has(2, -1), "an edge cell distinct on both axes must be present");
        assert!(!has(3, 0) && !has(0, 3), "one block past the edge must be absent");
    }

    /// `ensure_end_platform` actually writes through a [`ChunkSource`], and a
    /// second call over an already-correct platform is a no-op — vanilla's
    /// `!is(block)` guard, observed rather than assumed: every write in the
    /// second pass is skipped because the state already matches.
    #[test]
    fn ensure_end_platform_writes_through_the_world_and_repeats_are_idempotent() {
        let world = FlatWorld::new();
        // `origin` is the platform's obsidian layer's y **plus one** — see
        // `end_platform_writes`'s own doc: it is `Dimension::end_spawn_point`
        // minus one block, i.e. the obsidian sits at `origin.y - 1`, not at
        // `origin.y` itself.
        let origin = BlockPos::new(100, 49, 0);
        ensure_end_platform(&world, origin);

        // Obsidian one block below the platform's own origin — the layer a
        // player standing at the spawn point (100, 50, 0) has underfoot.
        assert_eq!(world.block_state(100, 48, 0), "minecraft:obsidian");
        // Air where a player would stand: origin.y itself, and one above.
        assert_eq!(world.block_state(100, 49, 0), "minecraft:air");
        assert_eq!(world.block_state(100, 50, 0), "minecraft:air");
        // The platform's far corner, still obsidian, same layer as the centre.
        assert_eq!(world.block_state(98, 48, -2), "minecraft:obsidian");
        // One block outside the platform on every axis: untouched (still the
        // world's own default, not written by this call).
        assert_eq!(world.block_state(103, 48, 0), "minecraft:air");

        // A player builds a torch on the platform...
        world.put(100, 50, 0, "minecraft:torch");
        // ...and a second arrival must not clear it: `ensure_end_platform` only
        // rewrites a cell whose state does not already match the target, and
        // torch != air is the one cell in this pass that legitimately does not
        // match — asserting the *count* pins the guard rather than eyeballing it.
        let mut world_writes = 0usize;
        for (pos, block) in end_platform_writes(origin) {
            if world.block_state(pos.x, pos.y, pos.z) != block {
                world_writes += 1;
            }
        }
        assert_eq!(world_writes, 1, "only the torched cell should differ before a repair pass");
        ensure_end_platform(&world, origin);
        assert_eq!(
            world.block_state(100, 50, 0),
            "minecraft:air",
            "a repair pass does overwrite a non-matching cell — this is the platform's own \
             guarantee, not a claim that ensure_end_platform preserves player builds inside its \
             footprint"
        );
    }

    /// `is_end_portal` is a plain equality (the block carries no properties),
    /// unlike [`is_portal`]'s axis-aware prefix match.
    #[test]
    fn is_end_portal_matches_only_the_bare_state() {
        assert!(is_end_portal("minecraft:end_portal"));
        assert!(!is_end_portal("minecraft:end_portal_frame"));
        assert!(!is_end_portal("minecraft:end_portal_frame[eye=true,facing=north]"));
        assert!(!is_end_portal("minecraft:air"));
    }

    /// `is_end_portal_frame` matches any facing/eye combination, unlike
    /// [`is_end_portal`]'s bare equality.
    #[test]
    fn is_end_portal_frame_matches_any_facing_or_eye_value() {
        assert!(is_end_portal_frame("minecraft:end_portal_frame"));
        assert!(is_end_portal_frame(
            "minecraft:end_portal_frame[eye=false,facing=west]"
        ));
        assert!(!is_end_portal_frame("minecraft:end_portal"));
        assert!(!is_end_portal_frame("minecraft:obsidian"));
    }

    /// The eight-cell endpoint of [`end_portal_arrival`]: the platform's own
    /// origin is one below `end_spawn_point`, and the player's arrival is a
    /// **second**, independent one-block drop — not the same offset applied
    /// twice, and not the two collapsing onto the same `y`. Ties the two
    /// together through a real [`ensure_end_platform`] write, which is the
    /// assertion that would catch a dropped `ServerPlayer` subtraction: an
    /// arrival left at `end_spawn_point`'s own `y` (50) would read as
    /// "floating one block above the platform," air either way and easy to
    /// miss without checking what is directly underfoot.
    #[test]
    fn end_portal_arrival_stands_on_the_platform_the_same_call_builds() {
        let (platform_origin, arrival) = end_portal_arrival();
        assert_eq!(platform_origin, BlockPos::new(100, 49, 0));
        assert_eq!(arrival, lodestone_model::Vec3::new(100.5, 49.0, 0.5));

        let world = FlatWorld::new();
        ensure_end_platform(&world, platform_origin);
        let feet = BlockPos::new(
            arrival.x.floor() as i32,
            arrival.y.round() as i32,
            arrival.z.floor() as i32,
        );
        assert_eq!(
            world.block_state(feet.x, feet.y - 1, feet.z),
            "minecraft:obsidian",
            "the arrival cell must have solid ground directly underfoot"
        );
        assert_eq!(
            world.block_state(feet.x, feet.y, feet.z),
            "minecraft:air",
            "and the arrival cell itself must be clear, not embedded in the platform"
        );
    }

    /// The 12 rim cells of a ring anchored at `(min_x, min_z, y)`, paired with
    /// the facing a correctly-oriented frame there must have — every edge's
    /// three cells, in ring order.
    fn ring_positions(min_x: i32, y: i32, min_z: i32) -> Vec<(BlockPos, Direction)> {
        let mut cells = Vec::with_capacity(12);
        for x in (min_x + 1)..=(min_x + 3) {
            cells.push((BlockPos::new(x, y, min_z), Direction::South));
            cells.push((BlockPos::new(x, y, min_z + 4), Direction::North));
        }
        for z in (min_z + 1)..=(min_z + 3) {
            cells.push((BlockPos::new(min_x, y, z), Direction::East));
            cells.push((BlockPos::new(min_x + 4, y, z), Direction::West));
        }
        cells
    }

    /// The discriminating ring gate: eleven correctly-facing, already-eyed
    /// frames plus one ring cell that is genuinely **absent** (not merely
    /// un-eyed) must not ignite — the near miss a count-only implementation
    /// (one with no completion check at all, or one that only checks "does a
    /// frame exist here" and not "is it eyed") would pass. Placing the
    /// twelfth and clicking it must then ignite, in the same test, so the
    /// negative arm cannot be satisfied by an implementation that simply
    /// never ignites.
    #[test]
    fn eleven_eyed_frames_and_one_missing_cell_does_not_ignite_but_the_twelfth_does() {
        let world = FlatWorld::new();
        let (min_x, y, min_z) = (200, 70, 300);
        let positions = ring_positions(min_x, y, min_z);

        // Ten cells already eyed, correctly facing the centre.
        for &(pos, facing) in &positions[..10] {
            world.put(pos.x, pos.y, pos.z, &end_portal_frame_state(facing, true));
        }
        // The eleventh: a real frame, correctly facing, not yet eyed — the
        // click under test.
        let (eleventh_pos, eleventh_facing) = positions[10];
        world.put(
            eleventh_pos.x,
            eleventh_pos.y,
            eleventh_pos.z,
            &end_portal_frame_state(eleventh_facing, false),
        );
        // The twelfth ring cell is left as air entirely.
        let (twelfth_pos, twelfth_facing) = positions[11];
        assert_eq!(world.block_state(twelfth_pos.x, twelfth_pos.y, twelfth_pos.z), "minecraft:air");

        let ignition = ignite_end_portal_frame(&world, eleventh_pos)
            .expect("an unfired frame always accepts the eye");
        assert_eq!(
            ignition.frame,
            (eleventh_pos, end_portal_frame_state(eleventh_facing, true))
        );
        assert!(
            ignition.portal_fill.is_none(),
            "eleven eyed, correctly-facing frames plus one missing cell must not complete the ring"
        );
        world.set_block(eleventh_pos.x, eleventh_pos.y, eleventh_pos.z, &ignition.frame.1);

        // Now the twelfth appears (correctly facing, unfired) and is clicked:
        // the ring is finally 12 for 12.
        world.put(
            twelfth_pos.x,
            twelfth_pos.y,
            twelfth_pos.z,
            &end_portal_frame_state(twelfth_facing, false),
        );
        let ignition = ignite_end_portal_frame(&world, twelfth_pos)
            .expect("an unfired frame always accepts the eye");
        let fill = ignition
            .portal_fill
            .expect("the twelfth eye must complete the ring");
        assert_eq!(fill.len(), 9, "a 3x3 interior");
        assert!(fill.iter().all(|(_, state)| state == END_PORTAL_BLOCK));
        let mut got: Vec<(i32, i32)> = fill.iter().map(|(p, _)| (p.x, p.z)).collect();
        got.sort_unstable();
        let mut want: Vec<(i32, i32)> = Vec::new();
        for x in (min_x + 1)..=(min_x + 3) {
            for z in (min_z + 1)..=(min_z + 3) {
                want.push((x, z));
            }
        }
        want.sort_unstable();
        assert_eq!(got, want, "the interior is exactly the ring's own 3x3 centre");
    }

    /// The other half of the discriminating gate: twelve **correctly
    /// positioned** frames that all face the same direction — right for the
    /// three cells that direction happens to match, wrong for the other
    /// nine — must not ignite. A naive implementation that checks "is there a
    /// frame at each of the 12 rim cells, eyed" but never reads `facing`
    /// would wrongly light this.
    #[test]
    fn a_ring_of_correctly_placed_but_uniformly_rotated_frames_does_not_ignite() {
        let world = FlatWorld::new();
        let (min_x, y, min_z) = (400, 70, -100);
        let positions = ring_positions(min_x, y, min_z);

        // Every rim cell holds a real frame at the right position, but all
        // facing south — correct only for the three north-edge cells.
        for &(pos, _correct_facing) in &positions {
            world.put(pos.x, pos.y, pos.z, &end_portal_frame_state(Direction::South, true));
        }
        // Click a north-edge cell specifically, so the search's own geometry
        // hypothesis is right and the only possible failure is the facing
        // check — a click on a wrongly-hypothesised cell would fail for the
        // uninteresting reason of never finding the ring's bounding box at
        // all.
        let (click_pos, _) = positions
            .iter()
            .copied()
            .find(|&(pos, _)| pos.z == min_z)
            .expect("the north edge has three cells");
        world.set_block(
            click_pos.x,
            click_pos.y,
            click_pos.z,
            &end_portal_frame_state(Direction::South, false),
        );

        let ignition = ignite_end_portal_frame(&world, click_pos)
            .expect("an unfired frame always accepts the eye, regardless of its own rotation");
        assert!(
            ignition.portal_fill.is_none(),
            "a ring of uniformly-rotated frames must not ignite: presence and eye state are not \
             enough, facing must point at the centre"
        );
    }

    // -----------------------------------------------------------------
    // `create_portal`'s prefetch — the fix for "entering the Nether takes
    // forever": a first trip into a fresh dimension touched a dozen never-
    // generated columns one `block_state` read at a time, and this crate's
    // own measurement puts a single fresh column at ~909 ms. The fix warms
    // every column the site search is about to touch in parallel first.
    // -----------------------------------------------------------------

    /// A world with a real, standable floor (so the search's own scan is
    /// realistic, not a contrived no-op), that records every `(cx, cz)`
    /// [`ChunkSource::column`] was called with and which OS thread called it.
    ///
    /// `resident` is fixed at construction: `true` makes every chunk look
    /// already warm (the control — nothing left to prefetch), `false` makes
    /// every chunk look cold (every call site's realistic state on a player's
    /// first trip into an empty dimension).
    struct ParallelProbeWorld {
        floor_top: i32,
        resident: bool,
        columns_touched: Mutex<std::collections::HashSet<(i32, i32)>>,
        threads_seen: Mutex<std::collections::HashSet<std::thread::ThreadId>>,
    }

    impl ChunkSource for ParallelProbeWorld {
        fn column(&self, cx: i32, cz: i32) -> crate::chunk::ChunkColumn {
            self.columns_touched.lock().unwrap().insert((cx, cz));
            self.threads_seen.lock().unwrap().insert(std::thread::current().id());
            crate::chunk::ChunkColumn::new(0, 256)
        }
        fn block_state(&self, _x: i32, y: i32, _z: i32) -> String {
            if y <= self.floor_top {
                "minecraft:netherrack".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        }
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
        fn is_column_resident(&self, _cx: i32, _cz: i32) -> bool {
            self.resident
        }
    }

    /// **The fix, as a magnitude claim.** A cold dimension's site search must
    /// warm more than one distinct chunk column — the geometric fact this
    /// prefetch exists to exploit — and it must do so from more than one OS
    /// thread, which is the difference between "parallel" and "still serial
    /// but through a different function". Both numbers are predicted from the
    /// search's own fixed 33 x 33 block footprint rather than merely asserted
    /// non-zero: `origin = (0, _, 0)` spans chunk x/z each in `{-1, 0, 1}`
    /// (`(-16).div_euclid(16) == -1`, `16.div_euclid(16) == 1`), so exactly
    /// **9** distinct columns, never more and never fewer.
    #[test]
    fn a_cold_dimensions_site_search_warms_its_columns_in_parallel() {
        let world = ParallelProbeWorld {
            floor_top: 30,
            resident: false,
            columns_touched: Mutex::new(std::collections::HashSet::new()),
            threads_seen: Mutex::new(std::collections::HashSet::new()),
        };
        let origin = BlockPos::new(0, 40, 0);
        let _ = create_portal(&world, Dimension::Nether, origin, Axis::X);

        assert_eq!(
            world.columns_touched.lock().unwrap().len(),
            9,
            "the 33x33 footprint around a chunk-aligned-ish origin spans exactly 9 columns"
        );
        // The parallelism claim itself. `std::thread::available_parallelism`
        // is the same query `generate_columns_parallel` sizes its own
        // fan-out from, so a single-core sandbox is the only way this could
        // fail honestly — everywhere else, more than one thread touching 9
        // columns is exactly what "warmed in parallel" means.
        let cores = std::thread::available_parallelism().map(std::num::NonZero::get).unwrap_or(1);
        let threads = world.threads_seen.lock().unwrap().len();
        assert!(
            cores <= 1 || threads > 1,
            "with {cores} cores available, a 9-column prefetch used only {threads} thread(s) — \
             the fan-out did not engage"
        );
    }

    /// **The control.** A world where every chunk already reports resident
    /// must touch `.column()` **zero** times: `create_portal`'s own scan
    /// reads `block_state`, never `column`, so any call at all can only have
    /// come from the prefetch — and the prefetch's whole premise is that
    /// there is nothing to warm.
    #[test]
    fn a_warm_dimensions_site_search_touches_no_columns_at_all() {
        let world = ParallelProbeWorld {
            floor_top: 30,
            resident: true,
            columns_touched: Mutex::new(std::collections::HashSet::new()),
            threads_seen: Mutex::new(std::collections::HashSet::new()),
        };
        let origin = BlockPos::new(0, 40, 0);
        let _ = create_portal(&world, Dimension::Nether, origin, Axis::X);

        assert_eq!(
            world.columns_touched.lock().unwrap().len(),
            0,
            "every chunk was already resident, so the prefetch must not call `column` at all"
        );
    }
}
