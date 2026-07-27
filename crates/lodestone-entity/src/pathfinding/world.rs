//! The world seam and mob shape for pathfinding.
//!
//! [`PathWorld`] is the pathfinder's only view of the world, deliberately a
//! trait rather than a dependency on `lodestone-world` — the same decoupling
//! `lodestone-physics` uses for its `CollisionView`. A version crate (or a test)
//! implements it; the real adapter answers the two version-specific questions
//! (what *kind* of block sits at a coordinate, and does an AABB collide) while
//! all of vanilla's neighbour/step/drop reasoning stays version-free above it.
//!
//! [`MobShape`] carries the per-mob parameters that make path validity
//! *per-mob* rather than global: a 0.9-wide pig and a 1.4-wide zombie disagree
//! about which gaps are passable and how far they can drop.

use super::node::PathType;
use std::collections::HashMap;

/// An axis-aligned bounding box in world space, `f64` like vanilla's `AABB`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    /// Minimum X.
    pub min_x: f64,
    /// Minimum Y.
    pub min_y: f64,
    /// Minimum Z.
    pub min_z: f64,
    /// Maximum X.
    pub max_x: f64,
    /// Maximum Y.
    pub max_y: f64,
    /// Maximum Z.
    pub max_z: f64,
}

impl Aabb {
    /// Creates a box from explicit bounds.
    #[must_use]
    pub const fn new(
        min_x: f64,
        min_y: f64,
        min_z: f64,
        max_x: f64,
        max_y: f64,
        max_z: f64,
    ) -> Self {
        Self {
            min_x,
            min_y,
            min_z,
            max_x,
            max_y,
            max_z,
        }
    }

    /// Translates the box by a delta.
    #[must_use]
    pub fn moved(&self, dx: f64, dy: f64, dz: f64) -> Self {
        Self::new(
            self.min_x + dx,
            self.min_y + dy,
            self.min_z + dz,
            self.max_x + dx,
            self.max_y + dy,
            self.max_z + dz,
        )
    }

    /// Width along X.
    #[must_use]
    pub fn x_size(&self) -> f64 {
        self.max_x - self.min_x
    }

    /// Height along Y.
    #[must_use]
    pub fn y_size(&self) -> f64 {
        self.max_y - self.min_y
    }

    /// Depth along Z.
    #[must_use]
    pub fn z_size(&self) -> f64 {
        self.max_z - self.min_z
    }

    /// The largest dimension (`AABB.getSize`, the average in vanilla — see note).
    ///
    /// Vanilla's `AABB.getSize` returns the mean of the three sizes; we match
    /// that so the step counts in collision sweeps agree.
    #[must_use]
    pub fn size(&self) -> f64 {
        (self.x_size() + self.y_size() + self.z_size()) / 3.0
    }
}

/// The pathfinder's read-only view of the world.
///
/// Coordinates are block coordinates. Only [`base_path_type`](PathWorld::base_path_type)
/// and [`collides`](PathWorld::collides) encode version/registry knowledge; the
/// rest of the pathfinder is built on them.
pub trait PathWorld {
    /// The world's minimum block Y (`level.getMinY()`), the floor of downward
    /// searches.
    fn min_y(&self) -> i32;

    /// The **raw** per-block classification, equivalent to vanilla's
    /// `WalkNodeEvaluator.getPathTypeFromState`. This is the single seam holding
    /// block-registry semantics; everything else (neighbour damage borders,
    /// "open over walkable = walkable", per-mob aggregation) is derived from it
    /// in version-free code.
    ///
    /// Air is [`PathType::Open`]; a solid full block is [`PathType::Blocked`];
    /// water is [`PathType::Water`]; and so on.
    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType;

    /// The top of the block's collision shape within its own cell, i.e.
    /// vanilla's `shape.max(Direction.Axis.Y)`, or `0.0` if the block has no
    /// collision. Used to compute floor heights for step-up decisions.
    ///
    /// **This is NOT clamped to 1.0.** It is the raw shape maximum, which for
    /// blocks that stick up past their cell exceeds one block:
    /// - full block = `1.0`, slab = `0.5`, `soul_sand` = `0.875`
    /// - **fence / wall / closed fence-gate = `1.5`** (this is why a 0.6 step
    ///   height cannot mount them and mobs don't path over pens)
    /// - air / water / lava / cobweb = `0.0` (empty collision shape)
    ///
    /// A version-crate adapter must source this from the authoritative per-state
    /// shape table (the real-server dump `impl-world` is baking into the version
    /// crate), *not* from a naïve "one block tall" assumption — clamping fences
    /// to 1.0 here silently makes them look step-able and the pathfinder will
    /// confidently route through walls.
    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64;

    /// Whether the given box overlaps any block collision shape. Used for the
    /// jump-clearance and diagonal-reachability checks, matching vanilla's
    /// `level.noCollision` (negated).
    fn collides(&self, aabb: Aabb) -> bool;

    /// Whether the block holds a water fluid, for the floating floor-height
    /// case. Defaults to matching [`PathType::Water`].
    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        matches!(self.base_path_type(x, y, z), PathType::Water)
    }
}

/// Per-mob parameters that make traversability mob-specific.
#[derive(Debug, Clone)]
pub struct MobShape {
    /// Bounding-box width (`getBbWidth`).
    pub width: f32,
    /// Bounding-box height (`getBbHeight`).
    pub height: f32,
    /// Auto-step / jump-up height (`maxUpStep`, the `STEP_HEIGHT` attribute).
    pub max_up_step: f32,
    /// Maximum safe fall distance in blocks (`getMaxFallDistance`, default 3).
    pub max_fall_distance: i32,
    /// Whether the mob swims/floats rather than sinking (`canFloat`).
    pub can_float: bool,
    /// Whether the mob can walk over fence tops.
    pub can_walk_over_fences: bool,
    /// Whether the mob may pass through doorways.
    pub can_pass_doors: bool,
    /// Whether the mob can open wooden doors.
    pub can_open_doors: bool,
    /// Per-type malus overrides (`Mob.getPathfindingMalus`); absent types use
    /// the [`PathType::malus`] default.
    pub malus_overrides: HashMap<PathType, f32>,
}

impl MobShape {
    /// A generic land mob of the given size (pig/cow-like defaults).
    #[must_use]
    pub fn land(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            max_up_step: 0.6,
            max_fall_distance: 3,
            can_float: false,
            can_walk_over_fences: false,
            can_pass_doors: true,
            can_open_doors: false,
            malus_overrides: HashMap::new(),
        }
    }

    /// The mob's malus for a path type (`Mob.getPathfindingMalus`).
    #[must_use]
    pub fn malus(&self, kind: PathType) -> f32 {
        self.malus_overrides
            .get(&kind)
            .copied()
            .unwrap_or_else(|| kind.malus())
    }

    /// Integer BB extent used to iterate the mob's occupied cells
    /// (`Mth.floor(width + 1)`).
    #[must_use]
    pub fn cell_width(&self) -> i32 {
        (self.width + 1.0).floor() as i32
    }

    /// Integer BB height used to iterate the mob's occupied cells
    /// (`Mth.floor(height + 1)`).
    #[must_use]
    pub fn cell_height(&self) -> i32 {
        (self.height + 1.0).floor() as i32
    }
}
