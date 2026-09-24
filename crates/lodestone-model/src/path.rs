//! The node-evaluator path-type seam ([`PathTypeRegistry`]).
//!
//! Land navigation classifies each block a mob might stand in or move through
//! into a *path type*: is it open, blocked, water, lava, a fence it cannot step
//! over, a closed door, a rail, damaging, and so on. Vanilla computes this in
//! `WalkNodeEvaluator.getPathTypeFromState` from the block's tags, class and
//! fluid state; the result drives both traversability and pathfinding cost.
//!
//! Like block-state resolution (see [`crate::registry`]), the *base* per-state
//! classification is version-specific data — it depends on tags and blocks that
//! change between game versions — so the table that produces it lives in a
//! version crate. This module defines only the version-free vocabulary
//! ([`PathType`]) and the lookup trait ([`PathTypeRegistry`]); a version crate
//! generates the id -> [`PathType`] table authoritatively from the game and
//! implements the trait.
//!
//! [`PathType`] mirrors vanilla's full `PathType` enum, including the
//! neighbour-context variants (e.g. [`PathType::WaterBorder`],
//! [`PathType::FireInNeighbor`]). A [`PathTypeRegistry`] built from the
//! per-state classifier only ever *returns* the base variants; the context
//! variants exist so a pathfinder's own neighbour pass has one shared
//! vocabulary to name its results in.

/// A block's navigation classification, mirroring vanilla's `PathType`.
///
/// Each variant carries a *malus* (see [`PathType::malus`]): the pathfinding
/// cost penalty vanilla assigns, where a negative value marks a node a land mob
/// treats as impassable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathType {
    /// Solid or otherwise non-traversable — a land mob cannot enter.
    Blocked,
    /// Empty space with nothing to stand in the way.
    Open,
    /// A surface a mob can stand on.
    Walkable,
    /// Standing on an open door.
    WalkableDoor,
    /// A trapdoor (also lily pads and big dripleaf) — steppable.
    Trapdoor,
    /// Powder snow — a land mob sinks and is trapped.
    PowderSnow,
    /// Standing on top of powder snow.
    OnTopOfPowderSnow,
    /// A fence, wall, or closed fence gate — too tall to step over (1.5 high).
    Fence,
    /// Lava.
    Lava,
    /// Water.
    Water,
    /// A water cell bordering non-water, used to bias entry/exit.
    WaterBorder,
    /// A rail.
    Rail,
    /// A rail a mob cannot pass.
    UnpassableRail,
    /// A cell adjacent to fire.
    FireInNeighbor,
    /// Fire (or another burning block).
    Fire,
    /// A cell adjacent to a damaging block.
    DamagingInNeighbor,
    /// A damaging block (cactus, sweet berry bush, magma, …).
    Damaging,
    /// An open door.
    DoorOpen,
    /// A closed wooden door (can be opened by hand).
    DoorWoodClosed,
    /// A closed iron door (cannot be opened by hand).
    DoorIronClosed,
    /// A gap a mob may breach through (e.g. swimming up).
    Breach,
    /// Leaves.
    Leaves,
    /// Honey block — sticky, restricts movement.
    StickyHoney,
    /// Cocoa.
    Cocoa,
    /// A block that damages cautiously (wither rose, dripstone/speleothems).
    DamageCautious,
    /// Standing on top of a trapdoor.
    OnTopOfTrapdoor,
    /// A large mob positioned close to danger.
    BigMobsCloseToDanger,
}

impl PathType {
    /// The pathfinding cost penalty vanilla assigns this path type.
    ///
    /// A negative malus (`-1.0`) marks the node impassable to a land mob;
    /// non-negative values are additive route penalties. These are the values
    /// baked into vanilla's `PathType` enum.
    pub const fn malus(self) -> f32 {
        match self {
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
            PathType::Blocked
            | PathType::PowderSnow
            | PathType::Fence
            | PathType::Lava
            | PathType::UnpassableRail
            | PathType::Damaging
            | PathType::DoorWoodClosed
            | PathType::DoorIronClosed
            | PathType::Leaves => -1.0,
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

/// Maps numeric block state ids to their base navigation [`PathType`].
///
/// This is the seam between a version crate (which owns the generated id table)
/// and version-free navigation consumers. Implementations are cheap, read-only
/// lookups over a table built once at load. Only the per-state *base* path types
/// are returned; a pathfinder layers neighbour context on top itself.
///
/// # Examples
///
/// ```
/// use lodestone_model::{PathType, PathTypeRegistry};
///
/// struct Table(Vec<PathType>);
///
/// impl PathTypeRegistry for Table {
///     fn path_type(&self, id: u32) -> Option<PathType> {
///         self.0.get(id as usize).copied()
///     }
///     fn state_count(&self) -> u32 {
///         self.0.len() as u32
///     }
/// }
///
/// let table = Table(vec![PathType::Open, PathType::Blocked]);
/// assert_eq!(table.path_type(0), Some(PathType::Open));
/// assert_eq!(table.path_type(1), Some(PathType::Blocked));
/// assert_eq!(table.path_type(2), None);
/// assert_eq!(table.state_count(), 2);
/// ```
pub trait PathTypeRegistry {
    /// Resolves a block state id to its base navigation path type.
    ///
    /// Returns `None` if the id is not part of this registry.
    fn path_type(&self, id: u32) -> Option<PathType>;

    /// Returns the number of registered block states.
    ///
    /// Ids form the same contiguous `0..state_count()` range as the block-state
    /// registry, so the two tables are index-compatible.
    fn state_count(&self) -> u32;
}
