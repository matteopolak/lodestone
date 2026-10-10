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
use lodestone_model::Vec3;
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
/// block: `GrazeGoal`'s is `state.is(BlockTags.EDIBLE_FOR_SHEEP)`
/// (its `IS_EDIBLE` field) beside `state.is(Blocks.GRASS_BLOCK)`
/// (`GrazeGoal.canUse`). Two independent predicates that can hold together, so an enum would
/// have to enumerate the combinations. A block id would drag a registry into
/// `lodestone-entity`, which the whole `PathWorld` seam exists to avoid, and
/// would put tag resolution in the goal — the wrong side, exactly as with
/// `LureGoal`'s per-species food tags.
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
    /// (vanilla's own edible-for-sheep block tag) — what a sheep grazes when it is standing
    /// *in* it (`short_grass` and friends), consumed by
    /// `GrazeGoal`'s `IS_EDIBLE` field.
    pub edible_for_sheep: bool,
    /// The block is exactly `minecraft:grass_block` — what a sheep grazes when
    /// standing *on* it, and the only cue whose vanilla test is block equality
    /// rather than a tag (`GrazeGoal.canUse` and `GrazeGoal.tick`).
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

/// What a block does to a body walking on or through it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Footing {
    /// Slipperiness of the block under the feet (stone 0.6, ice 0.98).
    pub friction: f32,
    /// Horizontal velocity multiplier while standing in or on it (soul sand 0.4).
    pub speed_factor: f32,
}

impl Footing {
    /// Ordinary ground: the value every block without its own entry has.
    pub const DEFAULT: Self = Self { friction: 0.6, speed_factor: 1.0 };
}

/// A beehive or bee nest as a bee sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HiveView {
    /// Bees currently inside.
    pub occupants: u8,
    /// Whether fire burns within one block of it, which drives bees out and
    /// keeps them from entering.
    pub fire_nearby: bool,
}

impl HiveView {
    /// Most bees a hive holds.
    pub const CAPACITY: u8 = 3;

    /// Whether another bee fits.
    #[must_use]
    pub const fn is_full(self) -> bool {
        self.occupants >= Self::CAPACITY
    }
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

    /// Whether the cell is lit by the open sky: nothing above it dims light.
    /// Defaults to `false` for a world with no sky.
    fn sees_sky(&self, x: i32, y: i32, z: i32) -> bool {
        let _ = (x, y, z);
        false
    }

    /// The block state in this cell, or `None` where the world has no answer.
    fn block_state(&self, x: i32, y: i32, z: i32) -> Option<lodestone_data::block_states::StateId> {
        let _ = (x, y, z);
        None
    }

    /// Whether the cell holds an air block. A world that cannot name its block
    /// states treats every passable cell as air.
    fn is_air(&self, x: i32, y: i32, z: i32) -> bool {
        match self.block_state(x, y, z) {
            Some(state) => crate::ai::turtle_egg::is_air(state),
            None => matches!(self.base_path_type(x, y, z), PathType::Open),
        }
    }

    /// Whether the block is a full cube a bat can hang from.
    fn is_roost(&self, x: i32, y: i32, z: i32) -> bool {
        let _ = (x, y, z);
        false
    }

    /// Whether the cell holds a bloom a bee pollinates.
    fn attracts_bees(&self, x: i32, y: i32, z: i32) -> bool {
        let _ = (x, y, z);
        false
    }

    /// The state a crop, stem, berry bush or cave vine at this cell grows to
    /// when a bee tends it, or `None` if it is not growable or fully grown.
    fn bee_growth(&self, x: i32, y: i32, z: i32) -> Option<lodestone_data::block_states::StateId> {
        let _ = (x, y, z);
        None
    }

    /// The beehive or bee nest at this cell, if the world has one.
    fn hive_at(&self, x: i32, y: i32, z: i32) -> Option<HiveView> {
        let _ = (x, y, z);
        None
    }

    /// Every hive within `range` blocks (Euclidean) of the cell.
    fn hives_within(&self, x: i32, y: i32, z: i32, range: i32) -> Vec<(i32, i32, i32)> {
        let _ = (x, y, z, range);
        Vec::new()
    }

    /// Whether the cell's chunk is loaded.
    fn is_loaded(&self, x: i32, y: i32, z: i32) -> bool {
        let _ = (x, y, z);
        true
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

    /// Whether a ray from `from` to `to` crosses no collision shape: the sight
    /// test between two eyes. Sampled every 1/16 block with a point-sized box
    /// through [`collides`](PathWorld::collides), finer than any shape a mob
    /// cares to hide behind (a pane or bar is 1/8 thick), so it uses the same
    /// shapes and the same absent-terrain rule as movement.
    fn has_line_of_sight(&self, from: Vec3, to: Vec3) -> bool {
        const STEP: f64 = 1.0 / 16.0;
        const HALF: f64 = 1.0e-5;
        let delta = to - from;
        let samples = (delta.length() / STEP).ceil().max(1.0) as u32;
        (0..=samples).all(|i| {
            let p = from + delta * (f64::from(i) / f64::from(samples));
            !self.collides(Aabb::new(p.x - HALF, p.y - HALF, p.z - HALF, p.x + HALF, p.y + HALF, p.z + HALF))
        })
    }

    /// The [`Footing`] of one block. Defaults to ordinary ground, so an adapter
    /// that answers nothing makes every surface stone.
    fn footing(&self, x: i32, y: i32, z: i32) -> Footing {
        let _ = (x, y, z);
        Footing::DEFAULT
    }
}

/// How far a mob will path down a drop: 3 blocks while it has no target, and
/// with one, 3 plus the health it is willing to spend, `health - 33% of max`
/// less 4 per difficulty step below Hard. A creeper spends `health - 1`.
#[must_use]
pub fn max_fall_distance(
    has_target: bool,
    health: f32,
    max_health: f32,
    difficulty: lodestone_model::Difficulty,
    spends_all_but_one: bool,
) -> i32 {
    const COMFORTABLE: f32 = 3.0;
    if !has_target {
        return COMFORTABLE as i32;
    }
    let spare = if spends_all_but_one {
        health - 1.0
    } else {
        let hard_step = match difficulty {
            lodestone_model::Difficulty::Peaceful => 0,
            lodestone_model::Difficulty::Easy => 1,
            lodestone_model::Difficulty::Normal => 2,
            lodestone_model::Difficulty::Hard => 3,
        };
        let sacrifice = (health - max_health * 0.33) as i32 - (3 - hard_step) * 4;
        sacrifice.max(0) as f32
    };
    (spare + COMFORTABLE).floor() as i32
}

/// The pathfinding-malus overrides a species sets on itself, from
/// `data/path_malus.json` (regenerate with `just regen-path-malus`). A species
/// not listed keeps the default table.
#[must_use]
pub fn species_malus_overrides(species: &str) -> &'static [(PathType, f32)] {
    static TABLE: std::sync::OnceLock<HashMap<String, Vec<(PathType, f32)>>> =
        std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let raw: HashMap<String, HashMap<String, f32>> =
            serde_json::from_str(include_str!("../../data/path_malus.json"))
                .expect("path_malus.json is generated and well-formed");
        raw.into_iter()
            .map(|(species, costs)| {
                let costs = costs
                    .into_iter()
                    .map(|(kind, cost)| {
                        (PathType::from_name(&kind).expect("path_malus.json names a known type"), cost)
                    })
                    .collect();
                (species, costs)
            })
            .collect()
    });
    table.get(species).map_or(&[], Vec::as_slice)
}

/// How a mob moves through the world, which picks its node evaluator and its
/// locomotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NavMode {
    /// Walks, jumps and falls on solid ground.
    #[default]
    Ground,
    /// Swims through water cells in all six directions and never leaves it.
    Swim,
    /// Moves by pushing a velocity chosen by its goals in pulses, with no path.
    Drift,
    /// Floats through open air toward a wanted position, with no path or gravity.
    Fly,
    /// Flies along a path through open air cells, thrusting toward each waypoint
    /// under no gravity.
    Air,
    /// Flutters toward a random nearby block with its own velocity easing, and
    /// hangs from a ceiling to rest.
    Flutter,
    /// Circles an anchor point and swoops at its target by easing its velocity
    /// toward a move-target point.
    Swoop,
    /// Walks on land and swims through water, along one path that crosses the
    /// shoreline; the water locomotion follows the shape's [`SwimRule`].
    Amphibious,
}

/// How an amphibious body moves while it is in water.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SwimRule {
    /// Smooth swimming: the speed is scaled by `in_water`, the heading pitches
    /// toward the waypoint 5 degrees a tick, and the thrust follows the pitch.
    /// `buoyant` adds a small upward push each tick.
    Smooth {
        /// Factor on the requested speed while in water.
        in_water: f64,
        /// Factor on the requested speed while walking.
        on_land: f64,
        /// Whether the body rises slightly each tick.
        buoyant: bool,
    },
    /// A turtle: a vertical push toward the waypoint, thrust of a tenth of the
    /// speed along the heading, and a slow sink unless it has somewhere to be.
    Turtle,
    /// A drowned hunting a target in water: pushes proportional to the raw
    /// distance to the waypoint.
    Drowned,
}

impl NavMode {
    /// Whether the body moves under no gravity through open air.
    #[must_use]
    pub fn is_airborne(self) -> bool {
        matches!(self, Self::Fly | Self::Air | Self::Flutter | Self::Swoop)
    }

    /// Whether the path search moves through a volume of cells in all six
    /// directions instead of walking on a floor.
    #[must_use]
    pub fn is_volume(self) -> bool {
        matches!(self, Self::Swim | Self::Air)
    }
}

/// Per-mob parameters that make traversability mob-specific.
#[derive(Debug, Clone)]
pub struct MobShape {
    /// The evaluator and locomotion this mob uses.
    pub nav_mode: NavMode,
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
    /// Whether the mob climbs walls it presses against.
    pub can_climb: bool,
    /// Whether the mob may pass through doorways.
    pub can_pass_doors: bool,
    /// Whether the mob can open wooden doors.
    pub can_open_doors: bool,
    /// Per-type malus overrides (`Mob.getPathfindingMalus`); absent types use
    /// the [`PathType::malus`] default.
    pub malus_overrides: HashMap<PathType, f32>,
    /// The water locomotion of an [`NavMode::Amphibious`] body.
    pub swim_rule: SwimRule,
}

impl MobShape {
    /// A generic land mob of the given size (pig/cow-like defaults).
    #[must_use]
    pub fn land(width: f32, height: f32) -> Self {
        Self {
            nav_mode: NavMode::Ground,
            width,
            height,
            max_up_step: 0.6,
            max_fall_distance: 3,
            can_float: false,
            can_walk_over_fences: false,
            can_climb: false,
            can_pass_doors: true,
            can_open_doors: false,
            malus_overrides: HashMap::new(),
            swim_rule: SwimRule::Smooth { in_water: 0.1, on_land: 0.5, buoyant: false },
        }
    }

    /// A swimming mob of the given size: it moves only through water and treats
    /// water as free.
    #[must_use]
    pub fn swimmer(width: f32, height: f32) -> Self {
        let mut shape = Self::land(width, height);
        shape.nav_mode = NavMode::Swim;
        shape.can_float = true;
        shape.malus_overrides.insert(PathType::Water, 0.0);
        shape
    }

    /// A body that drifts in water on pulsed velocities instead of following paths.
    #[must_use]
    pub fn drifter(width: f32, height: f32) -> Self {
        let mut shape = Self::swimmer(width, height);
        shape.nav_mode = NavMode::Drift;
        shape
    }

    /// A body that moves through air under no gravity in the given airborne mode.
    #[must_use]
    pub fn flier(mode: NavMode, width: f32, height: f32) -> Self {
        let mut shape = Self::land(width, height);
        shape.nav_mode = mode;
        shape
    }

    /// A body that walks and swims in the given way.
    #[must_use]
    pub fn amphibian(rule: SwimRule, width: f32, height: f32) -> Self {
        let mut shape = Self::land(width, height);
        shape.nav_mode = NavMode::Amphibious;
        shape.swim_rule = rule;
        shape
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
