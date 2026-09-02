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

/// The block-*identity* facts a goal needs, classified by the host.
///
/// [`PathType`] answers "can a mob walk here", which is all the pathfinder ever
/// asks and is deliberately blind to which block it is: `grass_block`, `stone`
/// and `dirt` are one `Blocked`. But several vanilla goals branch on identity —
/// a sheep eats grass and not stone — so they were inexpressible at the
/// [`MobController`](crate::ai::MobController) seam: the
/// trait declared 33 methods and not one read a block.
///
/// # Why booleans rather than a block id or a `PathType`-style enum
///
/// Vanilla's own tests are **predicates over tags**, not equality against a
/// block: `EatBlockGoal`'s is `state.is(BlockTags.EDIBLE_FOR_SHEEP)`
/// (its `IS_EDIBLE` field) beside `state.is(Blocks.GRASS_BLOCK)`
/// (`EatBlockGoal.canUse`). Two independent predicates that can hold together, so an enum would
/// have to enumerate the combinations. A block id would drag a registry into
/// `lodestone-entity`, which the whole `PathWorld` seam exists to avoid, and
/// would put tag resolution in the goal — the wrong side, exactly as with
/// `TemptGoal`'s per-species food tags.
///
/// # How to add a cue
///
/// Add a field, answer it in the host's `PathWorld` impl, and cite the jar
/// predicate it stands for in a doc comment. Do **not** add one speculatively:
/// a cue nothing reads is a per-block cost paid on the host's side for nothing.
/// Cues are cheap here precisely because they are pulled on demand — see
/// [`MobController::block_cues_below`](crate::ai::MobController::block_cues_below)
/// for why this is a query and not a per-tick feed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockCues {
    /// The block is in `#minecraft:edible_for_sheep`
    /// (`BlockTags.EDIBLE_FOR_SHEEP`) — what a sheep grazes when it is standing
    /// *in* it (`short_grass` and friends), consumed by
    /// `EatBlockGoal`'s `IS_EDIBLE` field.
    pub edible_for_sheep: bool,
    /// The block is exactly `minecraft:grass_block` — what a sheep grazes when
    /// standing *on* it, and the only cue whose vanilla test is block equality
    /// rather than a tag (`EatBlockGoal.canUse` and `EatBlockGoal.tick`).
    pub grass_block: bool,
}

impl BlockCues {
    /// No cue applies — the correct answer for the overwhelming majority of
    /// blocks, and the default a host that classifies nothing returns.
    pub const NONE: Self = Self {
        edible_for_sheep: false,
        grass_block: false,
    };
}

/// The pathfinder's read-only view of the world.
///
/// Coordinates are block coordinates. Only [`base_path_type`](PathWorld::base_path_type)
/// and [`collides`](PathWorld::collides) encode version/registry knowledge; the
/// rest of the pathfinder is built on them.
///
/// `Send + Sync` mirrors the other cross-crate world seams (`CollisionView`,
/// `ChunkSource`): a `NavigatingMob`/`MobSim` borrows a `&dyn PathWorld`, and
/// the integrated server hands the sim to a `tokio::spawn`ed task, which
/// requires everything it captures — including that borrow — to be `Send`
/// (`&dyn T: Send` needs `T: Sync`). Real world adapters are plain terrain
/// stores, so this is free.
pub trait PathWorld: Send + Sync {
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

    /// The block-identity [`BlockCues`] at this position — the goal-facing
    /// counterpart to [`base_path_type`](PathWorld::base_path_type), which
    /// cannot tell `grass_block` from `stone`.
    ///
    /// This is on the *world* seam rather than on
    /// [`MobController`](crate::ai::MobController) because that is where
    /// registry knowledge already lives: every other version-specific block
    /// question in this crate is answered here, by the host adapter that owns
    /// the block registry. A goal reaches it through the controller, whose
    /// production implementor (`NavigatingMob`) already holds a
    /// `&dyn PathWorld` and so needs no new borrow, no lifetime and no change
    /// to the controller's object safety.
    ///
    /// Defaults to [`BlockCues::NONE`], so an adapter that classifies nothing
    /// still compiles — at the price of every cue-reading goal being inert.
    /// **That is not a neutral default**: a sheep in a world whose adapter does
    /// not answer this will never graze, and nothing will fail.
    fn block_cues(&self, x: i32, y: i32, z: i32) -> BlockCues {
        let _ = (x, y, z);
        BlockCues::NONE
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

    /// Integer BB extent used to iterate the mob's occupied cells,
    /// matching vanilla's own floor-plus-one step.
    #[must_use]
    pub fn cell_width(&self) -> i32 {
        (self.width + 1.0).floor() as i32
    }

    /// Integer BB height used to iterate the mob's occupied cells,
    /// matching vanilla's own floor-plus-one step.
    #[must_use]
    pub fn cell_height(&self) -> i32 {
        (self.height + 1.0).floor() as i32
    }
}
