//! The movement graph: nodes that carry arrival state, and the legality
//! predicates over [`NavView`] (`docs/baritone-port.md` §4.3).
//!
//! A node is the block cell the player's **feet** occupy, plus how they got there.
//! The arrival dimension exists because whether a movement is possible, and how
//! long it takes, are both functions of entry velocity — a purely positional graph
//! must either assume the worst (and refuse moves it could make) or the best (and
//! plan moves it cannot), and the second failure is the plan-fail-replan loop.
//!
//! # M1 scope
//!
//! `Walk` only, `Arrival::{Still, Walking}`. `WalkDiagonal`, `StepUp`, `Descend`,
//! `Drop`, `Climb`, `Gap`, `Break`, `Place` and `Swim` are later milestones.
//! Adding one means a variant here, a legality rule, an input script and a
//! template key — deliberately four small edits in four named places rather than a
//! new cost formula.

use crate::view::NavView;

/// The 0.6-block auto-step height, from
/// [`lodestone_physics::EntityDimensions::PLAYER`] rather than a literal: 26.2's
/// player does not override `maxUpStep()`, so it is the `STEP_HEIGHT` attribute
/// default.
pub const STEP_HEIGHT: f64 = lodestone_physics::EntityDimensions::PLAYER.step_height as f64;

/// The player's height, for the head-clearance check.
pub const BODY_HEIGHT: f64 = lodestone_physics::EntityDimensions::PLAYER.height as f64;

/// Tolerance on surface comparisons. Shapes are exact in `f32`, so this only
/// absorbs the `f32 -> f64` widening and the `y as f64` arithmetic around it.
///
/// `pub(crate)` because [`crate::search`] compares a `Step::to_surface` against its
/// cell floor with the same tolerance, to decide which cell holds the block the feet
/// rest on.
pub(crate) const SURFACE_EPS: f64 = 1e-6;

/// A horizontal direction, in `Direction.Plane.HORIZONTAL` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir4 {
    /// `-Z`.
    North,
    /// `+X`.
    East,
    /// `+Z`.
    South,
    /// `-X`.
    West,
}

impl Dir4 {
    /// All four, in vanilla's horizontal order — the order neighbour expansion
    /// uses, so expansion order is fixed and the search is reproducible.
    pub const ALL: [Self; 4] = [Self::North, Self::East, Self::South, Self::West];

    /// Block delta `(dx, dz)`.
    #[must_use]
    pub const fn delta(self) -> (i32, i32) {
        match self {
            Self::North => (0, -1),
            Self::East => (1, 0),
            Self::South => (0, 1),
            Self::West => (-1, 0),
        }
    }

    /// Dense index in `0..4`.
    #[must_use]
    pub const fn index(self) -> u8 {
        match self {
            Self::North => 0,
            Self::East => 1,
            Self::South => 2,
            Self::West => 3,
        }
    }

    /// Inverse of [`Self::index`].
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        Some(match index {
            0 => Self::North,
            1 => Self::East,
            2 => Self::South,
            3 => Self::West,
            _ => return None,
        })
    }

    /// Quarter-turns clockwise from `self` to `other`, in `0..4`.
    #[must_use]
    pub const fn turns_to(self, other: Self) -> u8 {
        (other.index() + 4 - self.index()) % 4
    }
}

/// How the player entered a node.
///
/// Five variants for M1: standing still, or walking in one of four directions. The
/// plan's `Sprinting(Dir4)` arm is M3 and slots in here without touching the
/// packing (three bits are reserved for exactly nine states).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arrival {
    /// At rest, or slow enough that entry velocity does not help.
    Still,
    /// Walking in this direction at (roughly) steady speed.
    Walking(Dir4),
}

impl Arrival {
    /// Dense index in `0..8`.
    #[must_use]
    pub const fn index(self) -> u8 {
        match self {
            Self::Still => 0,
            Self::Walking(dir) => 1 + dir.index(),
        }
    }

    /// Inverse of [`Self::index`].
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        match index {
            0 => Some(Self::Still),
            1..=4 => match Dir4::from_index(index - 1) {
                Some(dir) => Some(Self::Walking(dir)),
                None => None,
            },
            _ => None,
        }
    }

    /// The direction of travel, or `None` at rest.
    #[must_use]
    pub const fn dir(self) -> Option<Dir4> {
        match self {
            Self::Still => None,
            Self::Walking(dir) => Some(dir),
        }
    }
}

/// A search node: the feet cell plus the arrival state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NavNode {
    /// Feet cell `x`.
    pub x: i32,
    /// Feet cell `y`.
    pub y: i32,
    /// Feet cell `z`.
    pub z: i32,
    /// How the player got here.
    pub arrival: Arrival,
}

/// Bits `x` and `z` each get in the packed key. `±2^25` blocks covers the world
/// border (`±29,999,984`) with room to spare.
const XZ_BITS: u32 = 26;
/// Bits `y` gets, offset by [`Y_OFFSET`]. 9 bits = 512 values, which covers every
/// vanilla dimension (`-64..=319` in the overworld).
const Y_BITS: u32 = 9;
/// What is added to world `y` before packing.
const Y_OFFSET: i32 = 64;

const XZ_MASK: u64 = (1u64 << XZ_BITS) - 1;
const XZ_HALF: i32 = 1 << (XZ_BITS - 1);
const Y_MASK: u64 = (1u64 << Y_BITS) - 1;

impl NavNode {
    /// A node at rest.
    #[must_use]
    pub const fn still(x: i32, y: i32, z: i32) -> Self {
        Self {
            x,
            y,
            z,
            arrival: Arrival::Still,
        }
    }

    /// The same cell with a different arrival.
    #[must_use]
    pub const fn with_arrival(self, arrival: Arrival) -> Self {
        Self { arrival, ..self }
    }

    /// Pack into a `u64` key, or `None` outside the representable range.
    ///
    /// `None` is not "clamp and carry on". A hash collision or a wrong equality on
    /// this key silently corrupts a plan (`docs/baritone-port.md` §2.3), so an
    /// unrepresentable node is refused at the door and the bijection is
    /// unit-tested directly.
    #[must_use]
    pub fn try_pack(self) -> Option<u64> {
        if self.x < -XZ_HALF || self.x >= XZ_HALF || self.z < -XZ_HALF || self.z >= XZ_HALF {
            return None;
        }
        let y = self.y.checked_add(Y_OFFSET)?;
        if y < 0 {
            return None;
        }
        #[allow(clippy::cast_sign_loss)]
        let y = y as u64;
        if y > Y_MASK {
            return None;
        }
        #[allow(clippy::cast_sign_loss)]
        let x = (self.x as u64) & XZ_MASK;
        #[allow(clippy::cast_sign_loss)]
        let z = (self.z as u64) & XZ_MASK;
        Some(
            (x << (XZ_BITS + Y_BITS + 3))
                | (z << (Y_BITS + 3))
                | (y << 3)
                | u64::from(self.arrival.index()),
        )
    }

    /// Inverse of [`Self::try_pack`].
    #[must_use]
    pub fn unpack(key: u64) -> Option<Self> {
        #[allow(clippy::cast_possible_truncation)]
        let arrival = Arrival::from_index((key & 0b111) as u8)?;
        #[allow(clippy::cast_possible_truncation)]
        let y = ((key >> 3) & Y_MASK) as i32 - Y_OFFSET;
        let sign_extend = |raw: u64| -> i32 {
            #[allow(clippy::cast_possible_truncation)]
            let v = (raw & XZ_MASK) as i32;
            if v >= XZ_HALF { v - (XZ_HALF << 1) } else { v }
        };
        let z = sign_extend(key >> (Y_BITS + 3));
        let x = sign_extend(key >> (XZ_BITS + Y_BITS + 3));
        Some(Self { x, y, z, arrival })
    }
}

/// One movement kind. Direction is a parameter, not a separate type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MoveKind {
    /// A same-cell-height step to an orthogonal neighbour, including a step up or
    /// down within [`STEP_HEIGHT`] — which is what makes bottom slabs, soul sand
    /// (top `0.875`), farmland and snow layers work.
    Walk(Dir4),
}

impl MoveKind {
    /// Dense id, for the template key.
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Self::Walk(_) => 0,
        }
    }

    /// The direction this movement travels.
    #[must_use]
    pub const fn dir(self) -> Dir4 {
        match self {
            Self::Walk(dir) => dir,
        }
    }

    /// The cells this movement's legality predicate reads, relative to the source
    /// feet cell.
    ///
    /// **Static**, which is what makes it free to translate and union after the
    /// fact into a committed plan's witness set (`docs/baritone-port.md` §4.5).
    /// Recording witnesses *during* the search would add a hash insert per cell
    /// read per expanded node, which is exactly the wrong place to spend.
    #[must_use]
    pub const fn stencil(self) -> &'static [[i32; 3]] {
        match self {
            Self::Walk(Dir4::North) => &WALK_NORTH,
            Self::Walk(Dir4::East) => &WALK_EAST,
            Self::Walk(Dir4::South) => &WALK_SOUTH,
            Self::Walk(Dir4::West) => &WALK_WEST,
        }
    }
}

/// `Walk`'s stencil: the source column's support/body/head cells and the
/// destination column's. The four rotations are spelled out because the stencil
/// has to be `&'static`.
const WALK_EAST: [[i32; 3]; 8] = [
    [0, -1, 0],
    [0, 0, 0],
    [0, 1, 0],
    [0, 2, 0],
    [1, -1, 0],
    [1, 0, 0],
    [1, 1, 0],
    [1, 2, 0],
];
const WALK_WEST: [[i32; 3]; 8] = [
    [0, -1, 0],
    [0, 0, 0],
    [0, 1, 0],
    [0, 2, 0],
    [-1, -1, 0],
    [-1, 0, 0],
    [-1, 1, 0],
    [-1, 2, 0],
];
const WALK_SOUTH: [[i32; 3]; 8] = [
    [0, -1, 0],
    [0, 0, 0],
    [0, 1, 0],
    [0, 2, 0],
    [0, -1, 1],
    [0, 0, 1],
    [0, 1, 1],
    [0, 2, 1],
];
const WALK_NORTH: [[i32; 3]; 8] = [
    [0, -1, 0],
    [0, 0, 0],
    [0, 1, 0],
    [0, 2, 0],
    [0, -1, -1],
    [0, 0, -1],
    [0, 1, -1],
    [0, 2, -1],
];

/// Where the player's feet rest when standing in cell `(x, y, z)`, as a
/// world-space `y`, or `None` when that cell is not a place a body can stand.
///
/// This is the function `docs/baritone-port.md` §2.3 warns is **not**
/// `floor(position)`. Two cases, and both are real terrain:
///
/// * the cell holds a partial block whose top is under `1.0` — a bottom slab
///   (`0.5`), soul sand (`0.875`), farmland, a snow layer — and the feet rest on
///   *it*, inside this cell;
/// * the cell is passable and the feet rest on the block **below**, which must
///   present a full-height top.
///
/// A cell whose own shape reaches `1.0` or beyond is filled: you stand on top of
/// it, which is the cell above, not here.
///
/// # The one M1 restriction, stated so it is not mistaken for a bug
///
/// A support whose top exceeds `1.0` — a fence or a wall, which report `1.5` — is
/// **refused** rather than treated as a surface at `y + 0.5`. Walking a fence line
/// is legal in vanilla and the geometry is fiddly (the post is `0.25` wide);
/// refusing it costs nothing a walking bot wants and cannot produce a wrong answer.
/// The uncapped `top` is what makes the refusal possible at all: clamped to `1.0`,
/// a fence would read as ordinary ground.
#[must_use]
pub fn stand_surface(view: &dyn NavView, x: i32, y: i32, z: i32) -> Option<f64> {
    let inside = view.facts_at(x, y, z)?;
    let inside_top = f64::from(inside.top);
    if inside_top > 0.0 {
        if inside_top >= 1.0 {
            // Filled (full cube, fence, wall): stand above it, not in it.
            return None;
        }
        return Some(f64::from(y) + inside_top);
    }
    let below = view.facts_at(x, y - 1, z)?;
    let below_top = f64::from(below.top);
    if (below_top - 1.0).abs() <= SURFACE_EPS {
        Some(f64::from(y))
    } else {
        // Either nothing to stand on, or a fence/wall top, which M1 refuses.
        None
    }
}

/// Whether a body standing at `surface` in column `(x, z)` has room for its
/// [`BODY_HEIGHT`].
///
/// The body spans `[surface, surface + 1.8]`. The cell the feet are in contributes
/// only the surface they rest on, so the cells that must be clear are the two above
/// — and the second only when the surface is high enough in its cell to push the
/// head past `y + 2`.
#[must_use]
pub fn head_room(view: &dyn NavView, x: i32, y: i32, z: i32, surface: f64) -> bool {
    let head = surface + BODY_HEIGHT;
    let mut cell = y + 1;
    loop {
        let Some(facts) = view.facts_at(x, cell, z) else {
            return false;
        };
        if !facts.passable {
            return false;
        }
        if f64::from(cell) + 1.0 >= head - SURFACE_EPS {
            return true;
        }
        cell += 1;
    }
}

/// A cell the navigator will put the body in: standable, with head room, and
/// carrying no hazard anywhere the body occupies.
#[must_use]
pub fn standable(view: &dyn NavView, x: i32, y: i32, z: i32) -> Option<f64> {
    let surface = stand_surface(view, x, y, z)?;
    if !head_room(view, x, y, z, surface) {
        return None;
    }
    // Nothing the body passes through may be a hazard, and neither may the block
    // being stood on: a magma block is a full cube you can stand on and must not.
    let head = surface + BODY_HEIGHT;
    let mut cell = y;
    while f64::from(cell) < head - SURFACE_EPS {
        if view.facts_at(x, cell, z)?.must_not_enter {
            return None;
        }
        cell += 1;
    }
    if view.facts_at(x, y - 1, z)?.must_not_enter {
        return None;
    }
    Some(surface)
}

/// A legal edge out of a node: where it goes, and the surface it lands on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    /// The movement performed.
    pub kind: MoveKind,
    /// The node reached.
    pub to: NavNode,
    /// World-space feet `y` at the source.
    pub from_surface: f64,
    /// World-space feet `y` at the destination.
    pub to_surface: f64,
}

/// Every legal movement out of `from`, appended to `out` in a fixed order.
///
/// Order is [`Dir4::ALL`], which is vanilla's horizontal order, so expansion is
/// deterministic — a prerequisite for the byte-identical-plan gate.
pub fn successors(view: &dyn NavView, from: NavNode, out: &mut Vec<Step>) {
    let Some(from_surface) = standable(view, from.x, from.y, from.z) else {
        return;
    };
    for dir in Dir4::ALL {
        if let Some(step) = walk_step(view, from, from_surface, dir) {
            out.push(step);
        }
    }
}

/// `Walk(dir)` out of `from`, or `None` when illegal.
///
/// # The destination **cell** is not always `from.y`
///
/// A `Walk` is defined by its *surface* delta — anything inside the 0.6 auto-step —
/// and by [`stand_surface`]'s convention the feet cell is `floor(feet y)`, which for
/// a partial block is the partial block's **own** cell. Those two facts together
/// mean a walk within the auto-step can still change the cell index: soul sand's top
/// is `0.875`, so a body on soul sand stands in the soul sand's cell, while a body on
/// the full block beside it stands in the cell *above* the block it rests on. Walking
/// between them is a 0.125 step and a one-cell change of `y`.
///
/// This function used to pin `ty = from.y`, which refused that step in **both**
/// directions — every soul sand, farmland, snow-layer and path column in the world
/// unreachable, and the `0.4` speed factor soul sand exists to impose therefore
/// unreachable too. It is *not* a `Descend`: no fall is paid for, the surfaces are
/// 0.125 apart, and the auto-step check below is still the only legality rule.
///
/// The candidate order (`from.y`, then below, then above) mirrors [`seed_node`]'s and
/// exists only for determinism: at most one candidate can pass the auto-step gate,
/// because two standable cells in one column are always more than `STEP_HEIGHT`
/// apart (a cell standable on its own partial block is filled from the perspective of
/// the cell below, and a cell standable on the block below requires that block to be
/// full height).
#[must_use]
pub fn walk_step(view: &dyn NavView, from: NavNode, from_surface: f64, dir: Dir4) -> Option<Step> {
    let (dx, dz) = dir.delta();
    let (tx, tz) = (from.x + dx, from.z + dz);
    for ty in [from.y, from.y - 1, from.y + 1] {
        let Some(to_surface) = standable(view, tx, ty, tz) else {
            continue;
        };
        // A step the 0.6 auto-step cannot make is not a `Walk`. `StepUp` (M2) is the
        // kind that pays for a jump; `Descend` (M2) is the one that pays for a fall.
        if (to_surface - from_surface).abs() > STEP_HEIGHT + SURFACE_EPS {
            continue;
        }
        return Some(Step {
            kind: MoveKind::Walk(dir),
            to: NavNode {
                x: tx,
                y: ty,
                z: tz,
                arrival: Arrival::Walking(dir),
            },
            from_surface,
            to_surface,
        });
    }
    None
}

/// Where the player's feet cell is **for planning purposes**, from a real position,
/// or `None` when nothing under them is standable.
///
/// `docs/baritone-port.md` §2.3: this is not `floor(position)`. Soul sand's top is
/// `0.875` and a bottom slab's `0.5`, so the block whose cell your feet occupy is
/// not always the block you are standing on. The resolution here is to try the cell
/// `floor(y)` names first, then the cell below (the common case: feet at `y = 64.0`
/// floor to cell 64, whose support is 63), then the cell above (mid-step onto a
/// partial block), and to require the candidate's own surface to be within a block
/// of where the body actually is — so a candidate is only accepted if the body is
/// plausibly standing on *it*.
///
/// `None` is the honest answer while airborne over terrain the snapshot does not
/// cover, and the caller must treat it as "cannot plan yet" rather than guessing.
#[must_use]
pub fn seed_node(view: &dyn NavView, position: lodestone_physics::Vec3d) -> Option<NavNode> {
    #[allow(clippy::cast_possible_truncation)]
    let (x, z) = (position.x.floor() as i32, position.z.floor() as i32);
    #[allow(clippy::cast_possible_truncation)]
    let y = position.y.floor() as i32;
    for candidate in [y, y - 1, y + 1] {
        if let Some(surface) = standable(view, x, candidate, z)
            && (surface - position.y).abs() < 1.0
        {
            return Some(NavNode::still(x, candidate, z));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{FactsTable, FixtureCensus};
    use crate::view::GridView;
    use std::sync::Arc;

    fn flat() -> GridView {
        let facts = Arc::new(FactsTable::build(&FixtureCensus));
        let mut view = GridView::new(facts, FixtureCensus::AIR, -64, 320, Some((-16, -16, 16, 16)));
        view.fill(-16, 0, -16, 16, 0, 16, FixtureCensus::STONE);
        view
    }

    /// The bijection §2.3 says a collision in silently corrupts a plan.
    #[test]
    fn node_packing_is_a_bijection_over_a_realistic_range() {
        let mut seen = std::collections::HashSet::new();
        let mut count = 0usize;
        for x in [
            -33_554_432,
            -1_000_000,
            -1,
            0,
            1,
            12_345,
            1_000_000,
            33_554_431,
        ] {
            for z in [-33_554_432, -777, 0, 1, 999_999, 33_554_431] {
                for y in [-64, -1, 0, 63, 64, 319, 447] {
                    for a in 0..5u8 {
                        let node = NavNode {
                            x,
                            y,
                            z,
                            arrival: Arrival::from_index(a).unwrap(),
                        };
                        let key = node.try_pack().expect("in range");
                        assert!(seen.insert(key), "collision at {node:?}");
                        assert_eq!(NavNode::unpack(key), Some(node));
                        count += 1;
                    }
                }
            }
        }
        assert_eq!(count, seen.len());
    }

    /// Out of range is refused, not clamped — the control proving the range check
    /// does anything.
    #[test]
    fn out_of_range_nodes_are_refused() {
        assert!(NavNode::still(33_554_432, 0, 0).try_pack().is_none());
        assert!(NavNode::still(-33_554_433, 0, 0).try_pack().is_none());
        assert!(NavNode::still(0, 448, 0).try_pack().is_none());
        assert!(NavNode::still(0, -65, 0).try_pack().is_none());
    }

    #[test]
    fn flat_ground_gives_four_walks() {
        let view = flat();
        let mut out = Vec::new();
        successors(&view, NavNode::still(0, 1, 0), &mut out);
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|s| (s.to_surface - 1.0).abs() < 1e-9));
    }

    /// A bottom slab is a `Walk`, because 0.5 is under the 0.6 auto-step. This is
    /// the case that cannot be verified against any scene currently in the tree
    /// (`docs/baritone-port.md` §3.2's world-species trap), which is why the
    /// fixture is synthetic and explicit.
    #[test]
    fn a_bottom_slab_is_walkable_and_reports_the_slab_surface() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::SLAB);
        let step = walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).expect("legal");
        assert_eq!(
            step.to,
            NavNode {
                x: 1,
                y: 1,
                z: 0,
                arrival: Arrival::Walking(Dir4::East)
            }
        );
        assert!((step.to_surface - 1.5).abs() < 1e-9, "{}", step.to_surface);
    }

    /// Soul sand's top is `0.875`, so a soul-sand *floor* presents a surface 0.125
    /// **below** the stone floor beside it: walking onto it is a step down of 0.125
    /// and walking off it a step up of 0.125, both inside the 0.6 auto-step. Both
    /// directions are asserted because getting the sign wrong here refuses half the
    /// terrain in the nether.
    ///
    /// # The fixture: the soul sand is the **floor**, not a block on top of it
    ///
    /// This test was originally written as `set(1, 1, 0, SOUL_SAND)` — soul sand
    /// placed in the *feet* cell, i.e. sitting on top of the stone floor — and then
    /// asserted a `to_surface` of `1.875`. That is a step **up of 0.875**, which the
    /// auto-step genuinely cannot make (it is M2's `StepUp`), so the fixture
    /// contradicted the paragraph above it: the 0.125 the comment reasons about only
    /// exists when the soul sand *replaces* the floor block. It is the world-species
    /// trap of `CLAUDE.md`'s evidence standards, one layer down — the assertion was
    /// fine and the input was the wrong scene.
    ///
    /// Note the **cell** the walk lands in: `y = 0`, the soul sand's own cell, not
    /// `y = 1`. That is [`stand_surface`]'s convention (the feet cell is
    /// `floor(feet y)`), and it is what `walk_step` used to be unable to express.
    #[test]
    fn soul_sand_is_walkable_in_both_directions() {
        let mut view = flat();
        view.set(1, 0, 0, FixtureCensus::SOUL_SAND);
        let onto = walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).expect("onto");
        assert!((onto.to_surface - 0.875).abs() < 1e-9, "{}", onto.to_surface);
        assert_eq!(
            (onto.to.x, onto.to.y, onto.to.z),
            (1, 0, 0),
            "the feet cell is the soul sand's own cell"
        );
        let off = walk_step(&view, NavNode::still(1, 0, 0), 0.875, Dir4::East).expect("off");
        assert!((off.to_surface - 1.0).abs() < 1e-9, "{}", off.to_surface);
        assert_eq!(
            (off.to.x, off.to.y, off.to.z),
            (2, 1, 0),
            "and stepping back off it returns to the cell above the floor"
        );
    }

    /// The control that proves the `y`-candidate search above did not simply make
    /// everything legal: soul sand *on top of* the floor is a step up of 0.875 and is
    /// still refused, in both directions. Without this, "soul sand is walkable" could
    /// be satisfied by a rule that accepts any neighbour with a standable cell
    /// anywhere in the column.
    #[test]
    fn soul_sand_stacked_on_the_floor_is_still_too_tall_to_walk_onto() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::SOUL_SAND);
        assert!(
            walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none(),
            "0.875 is above the 0.6 auto-step: that is M2's StepUp, not a Walk"
        );
        assert!(
            walk_step(&view, NavNode::still(1, 1, 0), 1.875, Dir4::East).is_none(),
            "and the drop back off it is 0.875 too"
        );
    }

    /// The negative control for the slab: a *full* block one up is not a `Walk`,
    /// because 1.0 exceeds the auto-step. Without this, "the slab was walkable"
    /// could be satisfied by a rule that accepts everything.
    #[test]
    fn a_full_block_step_up_is_not_a_walk() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::STONE);
        assert!(walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none());
    }

    /// A fence is `1.5` tall and the 0.6 auto-step cannot mount it. This is trap 1
    /// of `docs/baritone-port.md` §10, and the assertion that the uncapped `top`
    /// survived all the way from the census to the legality rule.
    #[test]
    fn a_fence_is_not_walkable_through_or_over() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::FENCE);
        assert_eq!(view.facts_at(1, 1, 0).unwrap().top, 1.5);
        assert!(
            walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none(),
            "a clamped collision_top would make this legal, and route paths \
             through pens"
        );
    }

    #[test]
    fn a_hazard_cell_is_refused_even_though_it_is_passable() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::WATER);
        assert!(view.facts_at(1, 1, 0).unwrap().passable);
        assert!(walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none());
    }

    /// A magma block is a full cube you can stand on and must not, so the hazard
    /// check has to look at the *support* too, not only the cells the body spans.
    #[test]
    fn a_hazardous_support_is_refused() {
        let mut view = flat();
        view.set(1, 0, 0, FixtureCensus::LAVA);
        assert!(walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none());
    }

    #[test]
    fn no_head_room_is_refused() {
        let mut view = flat();
        view.set(1, 2, 0, FixtureCensus::STONE);
        assert!(walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none());
    }

    /// A slab pushes the head into the cell two above, so the second head cell has
    /// to be checked — this is the case a one-cell head-room check gets wrong.
    #[test]
    fn standing_on_a_slab_needs_two_clear_cells_above() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::SLAB);
        view.set(1, 3, 0, FixtureCensus::STONE);
        assert!(
            walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none(),
            "feet at 1.5 put the head at 3.3, inside cell 3"
        );
    }

    /// Outside the snapshot terminates a branch. This is the mechanism, not a
    /// penalty.
    #[test]
    fn unknown_cells_terminate_expansion() {
        let view = flat();
        let mut out = Vec::new();
        // (16, 1, 0) is the last in-bounds column, so its `+X` neighbour is
        // outside and must not be proposed.
        successors(&view, NavNode::still(16, 1, 0), &mut out);
        assert_eq!(out.len(), 3, "{out:?}");
        assert!(!out.iter().any(|s| s.to.x == 17));
    }

    #[test]
    fn seeding_from_a_real_position_finds_the_supporting_cell() {
        let view = flat();
        let node = seed_node(&view, lodestone_physics::Vec3d::new(0.5, 1.0, 0.5)).expect("seed");
        assert_eq!((node.x, node.y, node.z), (0, 1, 0));
    }

    #[test]
    fn seeding_on_a_slab_finds_the_cell_the_slab_is_in() {
        let mut view = flat();
        view.set(0, 1, 0, FixtureCensus::SLAB);
        let node = seed_node(&view, lodestone_physics::Vec3d::new(0.5, 1.5, 0.5)).expect("seed");
        assert_eq!((node.x, node.y, node.z), (0, 1, 0));
    }

    /// The honest answer while there is nothing under the body.
    #[test]
    fn seeding_over_a_void_returns_none_rather_than_guessing() {
        let facts = Arc::new(FactsTable::build(&FixtureCensus));
        let view = GridView::new(facts, FixtureCensus::AIR, -64, 320, Some((-4, -4, 4, 4)));
        assert!(seed_node(&view, lodestone_physics::Vec3d::new(0.5, 100.0, 0.5)).is_none());
    }

    #[test]
    fn turns_to_is_a_quarter_turn_count() {
        assert_eq!(Dir4::East.turns_to(Dir4::East), 0);
        assert_eq!(Dir4::East.turns_to(Dir4::South), 1);
        assert_eq!(Dir4::East.turns_to(Dir4::West), 2);
        assert_eq!(Dir4::East.turns_to(Dir4::North), 3);
    }
}
