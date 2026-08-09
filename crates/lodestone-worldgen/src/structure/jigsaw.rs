//! **Jigsaw assembly** — `JigsawPlacement.addPieces` and its `Placer` (issue
//! #514's S4).
//!
//! # What it is
//!
//! The breadth-first walk that turns one start pool into a village: pick a centre
//! element, then repeatedly take a placed piece, shuffle its jigsaw blocks, and for
//! each one try every element of the pool it names, in every rotation, at every
//! matching jigsaw, until one fits in the free space that is left.
//!
//! # How it works
//!
//! ```text
//! addPieces:
//!     height  = start_height.sample(random)              <- 0 draws for `absolute`
//!     rotation = Rotation.getRandom(random)              <- 1 draw
//!     centre  = start_pool.getRandomTemplate(random)     <- 1 draw
//!     [start_jigsaw_name: shuffle the centre's jigsaws and find it by name]
//!     move the centre so its ground sits on the heightmap
//!     stub position = (centreX, centreY, centreZ)        <- the biome filter's point
//!  -- everything below happens only if the biome check passed --
//!     free = maxDistanceAabb - centreBox
//!     queue = [centre]
//!     while queue: tryPlacingChildren(pop the highest placement priority)
//! ```
//!
//! **RNG draw order and count are the specification**, and the nesting is where
//! that bites: `getShuffledTemplates` is drawn once per source jigsaw,
//! `Rotation.getShuffled` once per candidate element, and
//! `getShuffledJigsawBlocks` once per *(element, rotation)* pair — inside two
//! loops. A draw hoisted out of the innermost loop produces a village that is
//! entirely plausible and is not vanilla's.
//!
//! # Free space is exact, not approximate
//!
//! Vanilla accumulates free space in a `VoxelShape` and asks
//! `joinIsNotEmpty(free, targetBox.deflate(0.25), ONLY_SECOND)`. Every operation it
//! performs is "subtract an axis-aligned box", so the whole shape is always
//! `positive \ (b₁ ∪ b₂ ∪ …)` and the containment question reduces exactly to
//! `target ⊆ positive ∧ ∀i target ∩ bᵢ = ∅` — see [`FreeSpace`]. This is not a
//! simplification that happens to work on villages; it is the same set.
//!
//! # Where this runs, and why that is a deviation
//!
//! Vanilla assembles lazily inside `getPiecesBuilder()` and then fixes piece Y
//! inside `postProcess`, mutating a shared `StructureStart`. Assembly here happens
//! **eagerly at start time**, in
//! [`StructureKind::generate_pieces`](super::StructureKind), for the reason S2
//! recorded: our chunks are generated independently and memoised, so anything
//! resolved at placement time would shear a village along a chunk border. Jigsaw
//! assembly is a whole-structure computation in the first place, so this is not
//! even a compromise — the only real consequence is that the surface heights a
//! `terrain_matching` element's gravity processor reads come from a fresh `_WG`
//! noise column rather than from the decorating chunk's post-beard heightmap.
//!
//! # How to change it
//!
//! * A new jigsaw *structure* needs its `type`-specific fields in
//!   [`JigsawConfig::parse`], which **refuses** rather than defaults anything it
//!   does not model — `pool_aliases` is the live example, and defaulting it would
//!   silently place a trial chamber with no spawners.
//! * The three "warn and skip" branches in `try_placing_children` (missing pool,
//!   empty pool, empty fallback) are `continue`s in vanilla too. They consume the
//!   draws that came before them and none of the ones after, so turning one into
//!   an early `return` would change every later structure at that seed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use lodestone_worldgen_core::rng::{
    LegacyRandomSource, PositionalRandomFactory, RandomSource,
};
use serde_json::Value;

use super::beardifier::{Junction, PieceBeard};
use super::pool::{PoolElement, PoolStore, Projection, place_settings, shuffle};
use super::processor::ColumnHeights;
use super::template::{
    BlockNbt, Rotation, TemplateBlockInfo, direction_step, nbt_int, nbt_string, opposite_direction,
};
use super::{
    BoundingBox, HeightmapKind, PiecePlacement, StartContext, StructurePiece, free_height,
};

/// `JigsawBlockEntity.JointType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JointType {
    /// `aligned` — the two blocks' `top` facings must match as well as their
    /// fronts being opposed.
    Aligned,
    /// `rollable` — only the fronts have to oppose.
    Rollable,
}

/// One jigsaw block of an element — vanilla's
/// `StructureTemplate.JigsawBlockInfo`.
#[derive(Debug, Clone)]
pub struct JigsawBlockInfo {
    /// The block's world position (already transformed by the scan's position and
    /// rotation).
    pub pos: [i32; 3],
    /// `front()` of the rotated `orientation`, i.e. the direction the connection
    /// points.
    pub front: String,
    /// `top()` of the rotated `orientation`.
    pub top: String,
    /// `joint`, defaulting by front axis (`getDefaultJointType`).
    pub joint: JointType,
    /// `name`.
    pub name: String,
    /// `pool` — the pool this connection draws its neighbour from.
    pub pool: String,
    /// `target` — the `name` the neighbour's jigsaw must carry.
    pub target: String,
    /// `placement_priority`, the queue order a child is expanded in.
    pub placement_priority: i32,
    /// `selection_priority`, the order this block is *tried* in within its piece.
    pub selection_priority: i32,
}

impl JigsawBlockInfo {
    /// `JigsawBlockInfo.of(info)` — reads the whole configuration out of the
    /// block's retained `nbt`.
    ///
    /// This is the method the whole of S4 was blocked on: S2 dropped `nbt`, so
    /// every field below was unreachable and `JigsawPlacement` had nothing to read.
    #[must_use]
    pub fn of(info: TemplateBlockInfo) -> Self {
        let (front, top) = info
            .state
            .front_and_top()
            .map_or(("north".to_string(), "up".to_string()), |(f, t)| {
                (f.to_string(), t.to_string())
            });
        let empty: BlockNbt = Vec::new();
        let nbt = info.nbt.as_deref().unwrap_or(&empty);
        // `getDefaultJointType`: a horizontal front defaults to ALIGNED, a
        // vertical one to ROLLABLE. Every village connection is horizontal, so an
        // implementation that defaulted everything to ROLLABLE would attach
        // upside-down houses and still look like it worked.
        let default_joint = if front == "up" || front == "down" {
            JointType::Rollable
        } else {
            JointType::Aligned
        };
        let joint = match nbt_string(nbt, "joint") {
            Some("rollable") => JointType::Rollable,
            Some("aligned") => JointType::Aligned,
            _ => default_joint,
        };
        Self {
            pos: info.pos,
            front,
            top,
            joint,
            name: nbt_string(nbt, "name").unwrap_or("minecraft:empty").to_string(),
            pool: nbt_string(nbt, "pool").unwrap_or("minecraft:empty").to_string(),
            target: nbt_string(nbt, "target").unwrap_or("minecraft:empty").to_string(),
            placement_priority: nbt_int(nbt, "placement_priority").unwrap_or(0),
            selection_priority: nbt_int(nbt, "selection_priority").unwrap_or(0),
        }
    }

    /// `FeaturePoolElement.getShuffledJigsawBlocks`' single synthetic block:
    /// `fromFrontAndTop(DOWN, SOUTH)`, pool and target `minecraft:empty`, name
    /// `minecraft:bottom`, joint `rollable`, both priorities 0.
    #[must_use]
    pub fn feature_default(position: [i32; 3]) -> Self {
        Self {
            pos: position,
            front: "down".to_string(),
            top: "south".to_string(),
            joint: JointType::Rollable,
            name: "minecraft:bottom".to_string(),
            pool: "minecraft:empty".to_string(),
            target: "minecraft:empty".to_string(),
            placement_priority: 0,
            selection_priority: 0,
        }
    }

    /// `JigsawBlock.canAttach(source, target)`.
    ///
    /// Three conditions, and the third is the one that is easy to get backwards:
    /// the **source's `target`** must equal the **target's `name`**, not the other
    /// way round and not both ways.
    #[must_use]
    pub fn can_attach(&self, target: &Self) -> bool {
        self.front == opposite_direction(&target.front)
            && (self.joint == JointType::Rollable || self.top == target.top)
            && self.target == target.name
    }
}

/// An axis-aligned box in `f64`, i.e. `AABB.of(BoundingBox)`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Aabb {
    min: [f64; 3],
    max: [f64; 3],
}

impl Aabb {
    /// `AABB.of(box)` — a `BoundingBox` is inclusive, an `AABB` half-open, so the
    /// maximum grows by one.
    fn of(b: BoundingBox) -> Self {
        Self {
            min: [f64::from(b.min[0]), f64::from(b.min[1]), f64::from(b.min[2])],
            max: [
                f64::from(b.max[0]) + 1.0,
                f64::from(b.max[1]) + 1.0,
                f64::from(b.max[2]) + 1.0,
            ],
        }
    }

    fn deflate(self, n: f64) -> Self {
        Self {
            min: [self.min[0] + n, self.min[1] + n, self.min[2] + n],
            max: [self.max[0] - n, self.max[1] - n, self.max[2] - n],
        }
    }

    fn contains(self, other: Self) -> bool {
        (0..3).all(|i| other.min[i] >= self.min[i] && other.max[i] <= self.max[i])
    }

    /// Strict overlap — two boxes that merely touch share no volume, and a
    /// `VoxelShape` join of them is empty.
    fn overlaps(self, other: Self) -> bool {
        (0..3).all(|i| self.min[i] < other.max[i] && self.max[i] > other.min[i])
    }
}

/// The free-space accumulator: `positive` minus everything in `subtracted`.
///
/// See the module doc for why this is exactly vanilla's `VoxelShape` and not an
/// approximation of it.
#[derive(Debug, Clone)]
struct FreeSpace {
    positive: Aabb,
    subtracted: Vec<Aabb>,
}

impl FreeSpace {
    /// `!Shapes.joinIsNotEmpty(free, create(AABB.of(box).deflate(0.25)),
    /// ONLY_SECOND)` — is the deflated box entirely inside the free region?
    ///
    /// The `0.25` deflation exists so that two boxes sharing a face do not count
    /// as colliding. Over integer boxes it changes no answer, which is worth
    /// knowing but not worth exploiting: keeping it makes this the same expression
    /// as vanilla's.
    fn accepts(&self, box_: BoundingBox) -> bool {
        let candidate = Aabb::of(box_).deflate(0.25);
        self.positive.contains(candidate) && !self.subtracted.iter().any(|s| s.overlaps(candidate))
    }

    /// `joinUnoptimized(free, create(AABB.of(box)), ONLY_FIRST)` — subtract the
    /// box, undeflated.
    fn occupy(&mut self, box_: BoundingBox) {
        self.subtracted.push(Aabb::of(box_));
    }
}

/// `SequencedPriorityIterator` — FIFO within a priority, highest priority first.
///
/// Vanilla iterates a fastutil hash map to find the next highest priority, which
/// looks order-dependent and is not: one queue per priority means the maximum is
/// unique, so a `BTreeMap` keyed by priority is the same iterator.
#[derive(Debug)]
struct PriorityQueue<T> {
    queues: BTreeMap<i32, VecDeque<T>>,
}

// A hand-written `Default`: the derive would demand `T: Default`, which
// `PieceState` has no sensible value for.
impl<T> Default for PriorityQueue<T> {
    fn default() -> Self {
        Self {
            queues: BTreeMap::new(),
        }
    }
}

impl<T> PriorityQueue<T> {
    fn add(&mut self, value: T, priority: i32) {
        self.queues.entry(priority).or_default().push_back(value);
    }

    fn pop(&mut self) -> Option<T> {
        let (&priority, _) = self.queues.iter().next_back()?;
        let queue = self.queues.get_mut(&priority)?;
        let value = queue.pop_front();
        if queue.is_empty() {
            self.queues.remove(&priority);
        }
        value
    }
}

/// `VerticalAnchor` + `HeightProvider`, restricted to the shapes the bundled
/// jigsaw structures use.
#[derive(Debug, Clone, Copy)]
pub enum HeightProvider {
    /// `constant` — the bare `{"absolute": n}` shape included. **No draw.**
    Constant(VerticalAnchor),
    /// `uniform` — `Mth.randomBetweenInclusive`, exactly **one** draw.
    Uniform(VerticalAnchor, VerticalAnchor),
}

/// `VerticalAnchor`.
#[derive(Debug, Clone, Copy)]
pub enum VerticalAnchor {
    /// `absolute`.
    Absolute(i32),
    /// `above_bottom`.
    AboveBottom(i32),
    /// `below_top`.
    BelowTop(i32),
}

impl VerticalAnchor {
    fn parse(value: &Value) -> Option<Self> {
        if let Some(v) = value["absolute"].as_i64() {
            return Some(Self::Absolute(v as i32));
        }
        if let Some(v) = value["above_bottom"].as_i64() {
            return Some(Self::AboveBottom(v as i32));
        }
        if let Some(v) = value["below_top"].as_i64() {
            return Some(Self::BelowTop(v as i32));
        }
        None
    }

    fn resolve(self, min_y: i32, height: i32) -> i32 {
        match self {
            Self::Absolute(v) => v,
            Self::AboveBottom(v) => min_y + v,
            Self::BelowTop(v) => min_y + height - 1 - v,
        }
    }
}

impl HeightProvider {
    /// Parses the `height` / `start_height` field. `None` for a shape no bundled
    /// structure uses, which the caller turns into a ledger row rather than a guess.
    #[must_use]
    pub fn parse(value: &Value) -> Option<Self> {
        match value["type"].as_str() {
            None | Some("minecraft:constant") => {
                let anchor = VerticalAnchor::parse(value)
                    .or_else(|| VerticalAnchor::parse(&value["value"]))?;
                Some(Self::Constant(anchor))
            }
            Some("minecraft:uniform") => Some(Self::Uniform(
                VerticalAnchor::parse(&value["min_inclusive"])?,
                VerticalAnchor::parse(&value["max_inclusive"])?,
            )),
            _ => None,
        }
    }

    /// One sample. **`Constant` draws nothing and `Uniform` draws exactly once**,
    /// which is part of the stream specification for every caller.
    pub fn sample<R: RandomSource>(self, random: &mut R, min_y: i32, height: i32) -> i32 {
        match self {
            Self::Constant(anchor) => anchor.resolve(min_y, height),
            Self::Uniform(min, max) => {
                let lo = min.resolve(min_y, height);
                let hi = max.resolve(min_y, height);
                random.next_int_bounded(hi - lo + 1) + lo
            }
        }
    }
}

/// One `PoolAliasBinding` — a per-structure-instance redirection of one pool id
/// to another.
///
/// **This is what makes two trial chambers different.** A chamber's templates all
/// name the *alias* `trial_chambers/spawner/contents/melee`, which is not a
/// bundled pool at all; the binding maps it to one of
/// `spawner/melee/{zombie,husk,spider}` for the whole structure, so every spawner
/// in one chamber holds the same mob. Ignoring the bindings does not produce a
/// chamber with default spawners — `pools.get(alias)` finds nothing, vanilla warns
/// and `continue`s, and the chamber assembles with no spawner rooms at all.
#[derive(Debug, Clone)]
pub enum PoolAlias {
    /// `direct` — an unconditional mapping. **No draw.**
    Direct {
        /// `alias`.
        alias: String,
        /// `target`.
        target: String,
    },
    /// `random` — one weighted choice, **one draw**.
    Random {
        /// `alias`.
        alias: String,
        /// `targets`, as `(pool id, weight)` in document order.
        targets: Vec<(String, i32)>,
    },
    /// `random_group` — one weighted choice of a *list* of bindings, then each of
    /// those resolves in turn. One draw for the group, plus whatever its members
    /// cost.
    ///
    /// The reason a chamber's ranged and slow-ranged spawners agree with each other
    /// (skeleton with skeleton, stray with stray) while its melee spawner is drawn
    /// independently.
    RandomGroup {
        /// `groups`, as `(bindings, weight)` in document order.
        groups: Vec<(Vec<PoolAlias>, i32)>,
    },
}

impl PoolAlias {
    /// Parses one binding document, or returns why it cannot be modelled.
    pub fn parse(value: &Value) -> Result<Self, String> {
        match value["type"].as_str().unwrap_or_default() {
            "minecraft:direct" => Ok(Self::Direct {
                alias: field_string(value, "alias")?,
                target: field_string(value, "target")?,
            }),
            "minecraft:random" => Ok(Self::Random {
                alias: field_string(value, "alias")?,
                targets: parse_weighted(&value["targets"], |v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| "pool alias target is not a string".to_string())
                })?,
            }),
            "minecraft:random_group" => Ok(Self::RandomGroup {
                groups: parse_weighted(&value["groups"], |v| {
                    v.as_array()
                        .ok_or_else(|| "pool alias group is not a list".to_string())?
                        .iter()
                        .map(Self::parse)
                        .collect()
                })?,
            }),
            other => Err(format!("pool_alias type '{other}'")),
        }
    }

    /// `forEachResolved(random, consumer)` — the draws, in vanilla's order.
    fn for_each_resolved<R: RandomSource>(
        &self,
        random: &mut R,
        out: &mut BTreeMap<String, String>,
    ) {
        match self {
            Self::Direct { alias, target } => {
                out.insert(alias.clone(), target.clone());
            }
            Self::Random { alias, targets } => {
                if let Some(target) = weighted_pick(targets, random) {
                    out.insert(alias.clone(), target.clone());
                }
            }
            Self::RandomGroup { groups } => {
                if let Some(group) = weighted_pick(groups, random) {
                    // Cloned so the borrow of `groups` ends before the recursion
                    // takes `random` again; the list is three entries of two.
                    for binding in group.clone() {
                        binding.for_each_resolved(random, out);
                    }
                }
            }
        }
    }

    /// `allTargets()` — every pool id any resolution of this binding can name.
    ///
    /// [`super::StructureRegistry`] loads all of them, because only one is chosen
    /// per structure instance and which one is not known until start time.
    pub fn all_targets(&self, out: &mut BTreeSet<String>) {
        match self {
            Self::Direct { target, .. } => {
                out.insert(target.clone());
            }
            Self::Random { targets, .. } => {
                for (target, _) in targets {
                    out.insert(target.clone());
                }
            }
            Self::RandomGroup { groups } => {
                for (group, _) in groups {
                    for binding in group {
                        binding.all_targets(out);
                    }
                }
            }
        }
    }

    /// Every id that appears as an `alias`.
    ///
    /// Needed by [`super::pool::PoolStore::load`]: an alias is generally **not** a
    /// bundled pool (`trial_chambers/spawner/contents/melee` has no JSON), so the
    /// transitive walk would otherwise refuse the structure for a pool that is not
    /// supposed to exist.
    pub fn all_aliases(&self, out: &mut BTreeSet<String>) {
        match self {
            Self::Direct { alias, .. } | Self::Random { alias, .. } => {
                out.insert(alias.clone());
            }
            Self::RandomGroup { groups } => {
                for (group, _) in groups {
                    for binding in group {
                        binding.all_aliases(out);
                    }
                }
            }
        }
    }
}

fn field_string(value: &Value, key: &str) -> Result<String, String> {
    value[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("pool alias has no `{key}`"))
}

/// A `WeightedList` document: `[{"data": …, "weight": n}, …]`.
fn parse_weighted<T>(
    value: &Value,
    mut item: impl FnMut(&Value) -> Result<T, String>,
) -> Result<Vec<(T, i32)>, String> {
    let entries = value
        .as_array()
        .ok_or_else(|| "weighted list is not a list".to_string())?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        out.push((
            item(&entry["data"])?,
            entry["weight"].as_i64().unwrap_or(1) as i32,
        ));
    }
    Ok(out)
}

/// `WeightedList.getRandomOrThrow(random)` — one `nextInt(totalWeight)`, then the
/// cumulative walk. `Flat` and `Compact` are the same function of that index, so
/// there is only one behaviour to reproduce, and a **zero-weight entry is
/// unreachable** rather than reachable-with-probability-zero.
fn weighted_pick<'a, T, R: RandomSource>(entries: &'a [(T, i32)], random: &mut R) -> Option<&'a T> {
    let total: i32 = entries.iter().map(|(_, w)| *w).sum();
    if total <= 0 {
        return None;
    }
    let mut selection = random.next_int_bounded(total);
    for (value, weight) in entries {
        selection -= *weight;
        if selection < 0 {
            return Some(value);
        }
    }
    None
}

/// `PoolAliasLookup` — the resolved alias → target map for one structure instance.
///
/// # Its random is not the structure's stream
///
/// `PoolAliasLookup.create(bindings, startPos, seed)` builds
/// `RandomSource.create(seed).forkPositional().at(startPos)` — a *positional* fork
/// of the **world** seed, entirely separate from the `WorldgenRandom` the rest of
/// jigsaw assembly draws from. So the bindings cost the structure's own stream
/// nothing, and the map is a pure function of `(world seed, start position)`.
#[derive(Debug, Clone, Default)]
pub struct PoolAliasLookup {
    map: BTreeMap<String, String>,
}

impl PoolAliasLookup {
    /// `PoolAliasLookup.create(bindings, pos, seed)`. An empty binding list is
    /// vanilla's `EMPTY`, i.e. the identity, and derives no random at all.
    #[must_use]
    pub fn create(bindings: &[PoolAlias], pos: [i32; 3], seed: i64) -> Self {
        if bindings.is_empty() {
            return Self::default();
        }
        let mut random = LegacyRandomSource::new(seed)
            .fork_positional()
            .at(pos[0], pos[1], pos[2]);
        let mut map = BTreeMap::new();
        for binding in bindings {
            binding.for_each_resolved(&mut random, &mut map);
        }
        Self { map }
    }

    /// `lookup(key)` — `getOrDefault(key, key)`.
    #[must_use]
    pub fn lookup<'a>(&'a self, key: &'a str) -> &'a str {
        self.map.get(key).map_or(key, String::as_str)
    }
}

/// One jigsaw structure's `type`-specific configuration.
#[derive(Debug, Clone)]
pub struct JigsawConfig {
    /// `start_pool`.
    pub start_pool: String,
    /// `start_jigsaw_name` — the named jigsaw the whole structure is anchored on
    /// (`ancient_city`'s `city_anchor`).
    pub start_jigsaw_name: Option<String>,
    /// `size`, vanilla's `maxDepth`.
    pub max_depth: i32,
    /// `start_height`.
    pub start_height: HeightProvider,
    /// `use_expansion_hack`.
    pub use_expansion_hack: bool,
    /// `project_start_to_heightmap`; `None` means "use the sampled height
    /// directly", which is how the two deep structures sit underground.
    pub project_start_to_heightmap: Option<HeightmapKind>,
    /// `max_distance_from_center.horizontal`.
    pub max_horizontal: i32,
    /// `max_distance_from_center.vertical`.
    pub max_vertical: i32,
    /// `liquid_settings`, as "apply waterlogging".
    pub waterlogging: bool,
    /// `dimension_padding.bottom`.
    pub padding_bottom: i32,
    /// `dimension_padding.top`.
    pub padding_top: i32,
    /// `pool_aliases`, in document order — the order
    /// [`PoolAliasLookup::create`] resolves them in, which is the draw order.
    pub pool_aliases: Vec<PoolAlias>,
}

impl JigsawConfig {
    /// Parses a `minecraft:jigsaw` structure document, or returns why it cannot
    /// be modelled.
    ///
    /// **Refuses rather than defaults**, for everything it does not model. The
    /// bundled data needs no such refusal today: `pool_aliases` used to be one and
    /// is now honoured (see [`PoolAlias`]), which is what closes `trial_chambers`.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let mut pool_aliases = Vec::new();
        for binding in value["pool_aliases"].as_array().cloned().unwrap_or_default() {
            pool_aliases.push(PoolAlias::parse(&binding)?);
        }
        let start_pool = value["start_pool"]
            .as_str()
            .ok_or("jigsaw structure with no `start_pool`")?
            .to_string();
        let start_height = HeightProvider::parse(&value["start_height"])
            .ok_or_else(|| format!("jigsaw `start_height` shape {}", value["start_height"]))?;
        let project_start_to_heightmap = match value["project_start_to_heightmap"].as_str() {
            None => None,
            Some("WORLD_SURFACE_WG") => Some(HeightmapKind::WorldSurfaceWg),
            Some("OCEAN_FLOOR_WG") => Some(HeightmapKind::OceanFloorWg),
            Some(other) => return Err(format!("jigsaw `project_start_to_heightmap` '{other}'")),
        };
        // `MaxDistance`'s codec is `either(FULL, HORIZONTAL)`, and the bare-int
        // branch sets vertical **equal to** horizontal rather than to the
        // dimension height — the full-object branch is the one that defaults
        // vertical to `Y_SIZE`.
        let (max_horizontal, max_vertical) = match &value["max_distance_from_center"] {
            Value::Number(n) => {
                let v = n.as_i64().unwrap_or(80) as i32;
                (v, v)
            }
            object => {
                let h = object["horizontal"].as_i64().unwrap_or(80) as i32;
                let v = object["vertical"].as_i64().unwrap_or(4064) as i32;
                (h, v)
            }
        };
        let (padding_bottom, padding_top) = match &value["dimension_padding"] {
            Value::Null => (0, 0),
            Value::Number(n) => {
                let v = n.as_i64().unwrap_or(0) as i32;
                (v, v)
            }
            object => (
                object["bottom"].as_i64().unwrap_or(0) as i32,
                object["top"].as_i64().unwrap_or(0) as i32,
            ),
        };
        Ok(Self {
            start_pool,
            start_jigsaw_name: value["start_jigsaw_name"].as_str().map(str::to_string),
            max_depth: value["size"].as_i64().unwrap_or(0) as i32,
            start_height,
            use_expansion_hack: value["use_expansion_hack"].as_bool().unwrap_or(false),
            project_start_to_heightmap,
            max_horizontal,
            max_vertical,
            waterlogging: value["liquid_settings"].as_str() != Some("ignore_waterlogging"),
            padding_bottom,
            padding_top,
            pool_aliases,
        })
    }

    /// Every pool id the aliases can redirect **to**, for eager loading.
    #[must_use]
    pub fn alias_targets(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for binding in &self.pool_aliases {
            binding.all_targets(&mut out);
        }
        out
    }

    /// Every pool id that is an alias, i.e. one the bundle is *not* expected to
    /// contain a document for.
    #[must_use]
    pub fn alias_names(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for binding in &self.pool_aliases {
            binding.all_aliases(&mut out);
        }
        out
    }
}

/// One assembled piece, before it becomes a [`StructurePiece`].
#[derive(Debug, Clone)]
struct PlacedPiece {
    element: Arc<PoolElement>,
    position: [i32; 3],
    rotation: Rotation,
    box_: BoundingBox,
    ground_level_delta: i32,
    junctions: Vec<Junction>,
}

/// The centre piece and the RNG stream that produced it — vanilla's
/// `GenerationStub`, split so the biome filter can run between the two halves
/// exactly as `findValidGenerationPoint` does.
///
/// Holding the *random* is the point: the centre draws come out of the structure's
/// per-chunk stream before the biome check, and the whole BFS continues from the
/// same stream after it. Re-seeding in between (which is what every other
/// [`StructureKind`](super::StructureKind) does, correctly, because their
/// `findGenerationPoint` draws nothing) would restart the stream and produce a
/// different village.
#[allow(missing_debug_implementations)]
pub struct JigsawStub<R> {
    random: R,
    centre: PlacedPiece,
    /// `(centerX, centerY, centerZ)` — the stub position the biome filter uses.
    pub position: [i32; 3],
    free: FreeSpace,
    /// The resolved alias map. Built in [`begin`] because vanilla builds it from
    /// `startPos`, which only exists there, and carried across the biome filter for
    /// the same reason `random` is.
    aliases: PoolAliasLookup,
}

/// `JigsawPlacement.addPieces`' eager half: everything up to the stub position.
///
/// `Ok(None)` is vanilla's `Optional.empty()` — an empty centre pool, a missing
/// named start jigsaw, or a centre piece that does not fit the dimension padding.
/// No start at all, before any biome check.
#[allow(clippy::too_many_arguments)]
pub fn begin<R: RandomSource>(
    config: &JigsawConfig,
    pools: &PoolStore,
    cx: i32,
    cz: i32,
    min_y: i32,
    height: i32,
    seed: i64,
    ctx: &dyn StartContext,
    mut random: R,
) -> Option<JigsawStub<R>> {
    let start_height = config.start_height.sample(&mut random, min_y, height);
    let position = [cx * 16, start_height, cz * 16];
    // `PoolAliasLookup.create(this.poolAliases, startPos, context.seed())` is an
    // *argument* to `addPieces`, so it is built after the start height is sampled
    // and before `Rotation.getRandom`. Its own random is a positional fork of the
    // world seed, so where it sits in that sequence changes nothing — but the
    // position it keys on is the pre-adjustment `startPos`, not the centre.
    let aliases = PoolAliasLookup::create(&config.pool_aliases, position, seed);
    let centre_rotation = Rotation::random(&mut random);
    let centre_pool = pools.get(aliases.lookup(&config.start_pool))?;
    let centre_element = centre_pool.random_template(&mut random);
    if matches!(*centre_element, PoolElement::Empty) {
        return None;
    }

    let anchored = match &config.start_jigsaw_name {
        None => position,
        Some(name) => {
            let blocks =
                centre_element.shuffled_jigsaw_blocks(position, centre_rotation, &mut random);
            blocks.iter().find(|b| &b.name == name)?.pos
        }
    };
    let local_anchor = [
        anchored[0] - position[0],
        anchored[1] - position[1],
        anchored[2] - position[2],
    ];
    let adjusted = [
        position[0] - local_anchor[0],
        position[1] - local_anchor[1],
        position[2] - local_anchor[2],
    ];
    let box_ = centre_element.bounding_box(adjusted, centre_rotation)?;
    let centre_x = (box_.max[0] + box_.min[0]) / 2;
    let centre_z = (box_.max[2] + box_.min[2]) / 2;
    let bottom_y = match config.project_start_to_heightmap {
        None => adjusted[1],
        Some(heightmap) => position[1] + free_height(ctx, centre_x, centre_z, heightmap),
    };
    let ground_level_delta = centre_element.ground_level_delta();
    let old_ground_y = box_.min[1] + ground_level_delta;
    let shift = bottom_y - old_ground_y;
    let box_ = BoundingBox {
        min: [box_.min[0], box_.min[1] + shift, box_.min[2]],
        max: [box_.max[0], box_.max[1] + shift, box_.max[2]],
    };
    let centre = PlacedPiece {
        element: centre_element,
        position: [adjusted[0], adjusted[1] + shift, adjusted[2]],
        rotation: centre_rotation,
        box_,
        ground_level_delta,
        junctions: Vec::new(),
    };
    if config.padding_bottom != 0 || config.padding_top != 0 {
        let min_with_padding = min_y + config.padding_bottom;
        let max_with_padding = min_y + height - 1 - config.padding_top;
        if centre.box_.min[1] < min_with_padding || centre.box_.max[1] > max_with_padding {
            return None;
        }
    }

    let centre_y = bottom_y + local_anchor[1];
    let free = FreeSpace {
        positive: Aabb {
            min: [
                f64::from(centre_x - config.max_horizontal),
                f64::from((centre_y - config.max_vertical).max(min_y + config.padding_bottom)),
                f64::from(centre_z - config.max_horizontal),
            ],
            max: [
                f64::from(centre_x + config.max_horizontal + 1),
                f64::from(
                    (centre_y + config.max_vertical + 1)
                        .min(min_y + height - config.padding_top),
                ),
                f64::from(centre_z + config.max_horizontal + 1),
            ],
        },
        subtracted: vec![Aabb::of(centre.box_)],
    };
    Some(JigsawStub {
        random,
        centre,
        position: [centre_x, centre_y, centre_z],
        free,
        aliases,
    })
}

/// The stub's lazy half: `addPieces`' BFS, then the piece list.
///
/// Run **only after the biome filter passes**, which is what makes a
/// biome-rejected jigsaw candidate consume exactly the draws vanilla's rejected
/// candidate consumes and no more.
#[must_use]
pub fn finish<R: RandomSource>(
    stub: JigsawStub<R>,
    config: &JigsawConfig,
    pools: &PoolStore,
    ctx: &dyn StartContext,
) -> Vec<StructurePiece> {
    let JigsawStub {
        mut random,
        centre,
        free,
        aliases,
        ..
    } = stub;
    let mut placer = Placer {
        pools,
        max_depth: config.max_depth,
        expansion_hack: config.use_expansion_hack,
        pieces: vec![centre],
        frees: vec![free],
        placing: PriorityQueue::default(),
        aliases,
    };
    if config.max_depth > 0 {
        placer.try_placing_children(0, 0, 0, ctx, &mut random);
        while let Some(state) = placer.placing.pop() {
            placer.try_placing_children(state.piece, state.free, state.depth, ctx, &mut random);
        }
    }
    placer.into_pieces(config.waterlogging, ctx)
}

/// The `referencePos` every piece of a start is processed against:
/// `StructureStart.placeInChunk`'s
/// `new BlockPos(centre.getX(), pieces[0].boundingBox.minY(), centre.getZ())`,
/// where `centre` is the **first** piece's box centre.
///
/// A whole-start fact, computed once from the piece list rather than carried by
/// each piece — which is also why it is public here and consumed by
/// `structure_place_stage` rather than baked into a [`PiecePlacement`].
/// `BoundingBox.getCenter()` is `min + (max - min + 1) / 2`, an integer divide, and
/// it is **not** `(min + max) / 2` for an even span.
#[must_use]
pub fn reference_position(pieces: &[StructurePiece]) -> [i32; 3] {
    let Some(first) = pieces.first() else {
        return [0, 0, 0];
    };
    let b = first.bounding_box;
    [
        b.min[0] + (b.max[0] - b.min[0] + 1) / 2,
        b.min[1],
        b.min[2] + (b.max[2] - b.min[2] + 1) / 2,
    ]
}

/// One entry of `JigsawPlacement.Placer.placing`.
#[derive(Debug, Clone, Copy)]
struct PieceState {
    piece: usize,
    free: usize,
    depth: i32,
}

struct Placer<'a> {
    pools: &'a PoolStore,
    max_depth: i32,
    expansion_hack: bool,
    pieces: Vec<PlacedPiece>,
    /// The `MutableObject<VoxelShape>`s, as an arena: a `PieceState` refers to one
    /// by index because vanilla's shapes are **shared and mutated** between sibling
    /// states, and cloning one per state would let two siblings overlap.
    frees: Vec<FreeSpace>,
    placing: PriorityQueue<PieceState>,
    /// The alias map, applied to **every** `sourceJigsaw.pool()` and to nothing
    /// else — notably **not** to a pool's `fallback`, which vanilla looks up
    /// unaliased.
    aliases: PoolAliasLookup,
}

impl Placer<'_> {
    #[allow(clippy::too_many_lines)]
    fn try_placing_children<R: RandomSource>(
        &mut self,
        source: usize,
        context_free: usize,
        depth: i32,
        ctx: &dyn StartContext,
        random: &mut R,
    ) {
        let source_element = Arc::clone(&self.pieces[source].element);
        let source_position = self.pieces[source].position;
        let source_rotation = self.pieces[source].rotation;
        let source_rigid = source_element.projection().is_rigid();
        let source_box = self.pieces[source].box_;
        let source_box_y = source_box.min[1];
        // `MutableObject<VoxelShape> sourceFree = new MutableObject()` — one per
        // call, created empty and filled on the first *inner* attachment.
        let mut source_free: Option<usize> = None;

        let source_jigsaws =
            source_element.shuffled_jigsaw_blocks(source_position, source_rotation, random);
        for source_jigsaw in &source_jigsaws {
            let source_front = source_jigsaw.front.as_str();
            let step = direction_step(source_front);
            let source_jigsaw_pos = source_jigsaw.pos;
            let target_jigsaw_pos = [
                source_jigsaw_pos[0] + step[0],
                source_jigsaw_pos[1] + step[1],
                source_jigsaw_pos[2] + step[2],
            ];
            let source_jigsaw_local_y = source_jigsaw_pos[1] - source_box_y;
            let mut source_base_height: Option<i32> = None;

            // `poolAliasLookup.lookup(sourceJigsaw.pool())`.
            let pool_name = self.aliases.lookup(&source_jigsaw.pool);
            let Some(target_pool) = self.pools.get(pool_name) else {
                // "Empty or non-existent pool" — a warn and a `continue`.
                continue;
            };
            if target_pool.size() == 0 && pool_name != "minecraft:empty" {
                continue;
            }
            let fallback_id = target_pool.fallback.clone();
            let Some(fallback) = self.pools.get(&fallback_id) else {
                continue;
            };
            if fallback.size() == 0 && fallback_id != "minecraft:empty" {
                continue;
            }

            let inside_source = is_inside(source_box, target_jigsaw_pos);
            let children_free = if inside_source {
                let index = *source_free.get_or_insert_with(|| {
                    self.frees.push(FreeSpace {
                        positive: Aabb::of(source_box),
                        subtracted: Vec::new(),
                    });
                    self.frees.len() - 1
                });
                index
            } else {
                context_free
            };

            let mut candidates: Vec<Arc<PoolElement>> = Vec::new();
            if depth != self.max_depth {
                candidates.extend(target_pool.shuffled_templates(random));
            }
            candidates.extend(fallback.shuffled_templates(random));
            let placement_priority = source_jigsaw.placement_priority;

            'candidates: for target_element in candidates {
                if matches!(*target_element, PoolElement::Empty) {
                    // A `break`, not a `continue`: an empty element terminates the
                    // candidate list, so its position in a shuffled pool decides
                    // how many later candidates are considered at all.
                    break;
                }
                let mut rotations = [
                    Rotation::None,
                    Rotation::Cw90,
                    Rotation::Cw180,
                    Rotation::Ccw90,
                ];
                shuffle(&mut rotations, random);
                for target_rotation in rotations {
                    let target_jigsaws =
                        target_element.shuffled_jigsaw_blocks([0, 0, 0], target_rotation, random);
                    let Some(hack_box) = target_element.bounding_box([0, 0, 0], target_rotation)
                    else {
                        continue;
                    };
                    let expand_to = if self.expansion_hack
                        && hack_box.max[1] - hack_box.min[1] + 1 <= 16
                    {
                        target_jigsaws
                            .iter()
                            .map(|j| {
                                let step = direction_step(&j.front);
                                let ahead =
                                    [j.pos[0] + step[0], j.pos[1] + step[1], j.pos[2] + step[2]];
                                if !is_inside(hack_box, ahead) {
                                    return 0;
                                }
                                let child = self.pools.get(&j.pool);
                                let child_size = child.map_or(0, |p| p.max_size());
                                let child_fallback_size = child
                                    .and_then(|p| self.pools.get(&p.fallback))
                                    .map_or(0, |p| p.max_size());
                                child_size.max(child_fallback_size)
                            })
                            .max()
                            .unwrap_or(0)
                    } else {
                        0
                    };

                    for target_jigsaw in &target_jigsaws {
                        if !source_jigsaw.can_attach(target_jigsaw) {
                            continue;
                        }
                        let target_local = target_jigsaw.pos;
                        let raw_box_pos = [
                            target_jigsaw_pos[0] - target_local[0],
                            target_jigsaw_pos[1] - target_local[1],
                            target_jigsaw_pos[2] - target_local[2],
                        ];
                        let Some(raw_box) =
                            target_element.bounding_box(raw_box_pos, target_rotation)
                        else {
                            continue;
                        };
                        let target_rigid = target_element.projection().is_rigid();
                        let target_local_y = target_local[1];
                        let delta_y = source_jigsaw_local_y - target_local_y + step[1];
                        let target_box_y = if source_rigid && target_rigid {
                            source_box_y + delta_y
                        } else {
                            let base = *source_base_height.get_or_insert_with(|| {
                                free_height(
                                    ctx,
                                    source_jigsaw_pos[0],
                                    source_jigsaw_pos[2],
                                    HeightmapKind::WorldSurfaceWg,
                                )
                            });
                            base - target_local_y
                        };
                        let y_offset = target_box_y - raw_box.min[1];
                        let mut target_box = BoundingBox {
                            min: [raw_box.min[0], raw_box.min[1] + y_offset, raw_box.min[2]],
                            max: [raw_box.max[0], raw_box.max[1] + y_offset, raw_box.max[2]],
                        };
                        let target_box_position =
                            [raw_box_pos[0], raw_box_pos[1] + y_offset, raw_box_pos[2]];
                        if expand_to > 0 {
                            // `BoundingBox.encapsulate(pos)` **mutates**, and the
                            // grown box is what the piece, the free-space
                            // subtraction and the beard all see.
                            let new_size =
                                (expand_to + 1).max(target_box.max[1] - target_box.min[1]);
                            target_box.max[1] = target_box.max[1].max(target_box.min[1] + new_size);
                        }
                        if !self.frees[children_free].accepts(target_box) {
                            continue;
                        }
                        self.frees[children_free].occupy(target_box);

                        let source_gld = self.pieces[source].ground_level_delta;
                        let target_gld = if target_rigid {
                            source_gld - delta_y
                        } else {
                            target_element.ground_level_delta()
                        };
                        let junction_y = if source_rigid {
                            source_box_y + source_jigsaw_local_y
                        } else if target_rigid {
                            target_box_y + target_local_y
                        } else {
                            let base = *source_base_height.get_or_insert_with(|| {
                                free_height(
                                    ctx,
                                    source_jigsaw_pos[0],
                                    source_jigsaw_pos[2],
                                    HeightmapKind::WorldSurfaceWg,
                                )
                            });
                            base + delta_y / 2
                        };
                        self.pieces[source].junctions.push(Junction {
                            source_x: target_jigsaw_pos[0],
                            source_ground_y: junction_y - source_jigsaw_local_y + source_gld,
                            source_z: target_jigsaw_pos[2],
                        });
                        let target_index = self.pieces.len();
                        self.pieces.push(PlacedPiece {
                            element: Arc::clone(&target_element),
                            position: target_box_position,
                            rotation: target_rotation,
                            box_: target_box,
                            ground_level_delta: target_gld,
                            junctions: vec![Junction {
                                source_x: source_jigsaw_pos[0],
                                source_ground_y: junction_y - target_local_y + target_gld,
                                source_z: source_jigsaw_pos[2],
                            }],
                        });
                        if depth + 1 <= self.max_depth {
                            self.placing.add(
                                PieceState {
                                    piece: target_index,
                                    free: children_free,
                                    depth: depth + 1,
                                },
                                placement_priority,
                            );
                        }
                        // `continue label129` — this source jigsaw is satisfied.
                        continue 'candidates;
                    }
                }
            }
        }
    }

    /// Turns the assembled pieces into [`StructurePiece`]s, resolving each
    /// `terrain_matching` element's gravity heights on the way.
    fn into_pieces(self, waterlogging: bool, ctx: &dyn StartContext) -> Vec<StructurePiece> {
        let mut out = Vec::with_capacity(self.pieces.len());
        for piece in self.pieces {
            // Sampled over the piece's own footprint, once, so the same piece
            // placed from two different chunks agrees — see the module doc.
            let gravity = if piece.element.projection() == Projection::TerrainMatching {
                Some(Arc::new(ColumnHeights::build(
                    piece.box_.min[0],
                    piece.box_.min[2],
                    piece.box_.max[0],
                    piece.box_.max[2],
                    |x, z| free_height(ctx, x, z, HeightmapKind::WorldSurfaceWg),
                )))
            } else {
                None
            };
            let beard = Some(PieceBeard {
                rigid: piece.element.projection().is_rigid(),
                ground_level_delta: piece.ground_level_delta,
                junctions: piece.junctions,
            });
            let mut placements = element_placements(
                &piece.element,
                piece.position,
                piece.rotation,
                waterlogging,
                gravity.as_ref(),
            );
            let first = placements.first().cloned();
            let extra = if placements.is_empty() {
                Vec::new()
            } else {
                placements.drain(1..).collect()
            };
            out.push(StructurePiece {
                id: "minecraft:jigsaw".to_string(),
                bounding_box: piece.box_,
                // `PoolElementStructurePiece` extends `StructurePiece` directly and
                // never calls `setOrientation`, so vanilla persists `O = -1`.
                orientation: None,
                gen_depth: 0,
                template: first.as_ref().map(|(id, _)| id.clone()),
                placement: first.map(|(_, placement)| placement),
                extra_placements: extra.into_iter().map(|(_, placement)| placement).collect(),
                blocks: None,
                loot: Vec::new(),
                beard,
            });
        }
        out
    }
}

/// Every `(template id, placement)` one element writes — one for a single
/// element, several for a `list_pool_element`, none for a feature or empty one.
fn element_placements(
    element: &PoolElement,
    position: [i32; 3],
    rotation: Rotation,
    waterlogging: bool,
    gravity: Option<&Arc<ColumnHeights>>,
) -> Vec<(String, Arc<PiecePlacement>)> {
    match element {
        PoolElement::Single {
            template, decoded, ..
        } => vec![(
            template.clone(),
            Arc::new(PiecePlacement {
                template: Arc::clone(decoded),
                position,
                settings: place_settings(element, rotation, waterlogging, gravity.map(Arc::clone)),
            }),
        )],
        PoolElement::List { elements, .. } => elements
            .iter()
            .flat_map(|sub| element_placements(sub, position, rotation, waterlogging, gravity))
            .collect(),
        PoolElement::Feature { .. } | PoolElement::Empty => Vec::new(),
    }
}

/// `BoundingBox.isInside(pos)`.
fn is_inside(box_: BoundingBox, pos: [i32; 3]) -> bool {
    (0..3).all(|i| pos[i] >= box_.min[i] && pos[i] <= box_.max[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jigsaw(front: &str, top: &str, name: &str, target: &str, joint: JointType) -> JigsawBlockInfo {
        JigsawBlockInfo {
            pos: [0, 0, 0],
            front: front.to_string(),
            top: top.to_string(),
            joint,
            name: name.to_string(),
            pool: "minecraft:empty".to_string(),
            target: target.to_string(),
            placement_priority: 0,
            selection_priority: 0,
        }
    }

    /// `canAttach`'s three conditions, each falsified independently — including
    /// the asymmetric one, which is `source.target == target.name` and **not** the
    /// reverse.
    #[test]
    fn can_attach_needs_opposed_fronts_matching_tops_and_a_named_target() {
        let source = jigsaw("north", "up", "minecraft:a", "minecraft:b", JointType::Aligned);
        let ok = jigsaw("south", "up", "minecraft:b", "minecraft:a", JointType::Aligned);
        assert!(source.can_attach(&ok));

        // Same front, not opposed.
        assert!(!source.can_attach(&jigsaw(
            "north",
            "up",
            "minecraft:b",
            "minecraft:a",
            JointType::Aligned
        )));
        // Opposed fronts, mismatched tops, and an ALIGNED source.
        assert!(!source.can_attach(&jigsaw(
            "south",
            "east",
            "minecraft:b",
            "minecraft:a",
            JointType::Aligned
        )));
        // The same pair with a ROLLABLE *source* joint attaches — the joint of the
        // source is what matters, never the target's.
        let rollable = jigsaw("north", "up", "minecraft:a", "minecraft:b", JointType::Rollable);
        assert!(rollable.can_attach(&jigsaw(
            "south",
            "east",
            "minecraft:b",
            "minecraft:a",
            JointType::Aligned
        )));
        // Target's `name` is not the source's `target`.
        assert!(!source.can_attach(&jigsaw(
            "south",
            "up",
            "minecraft:c",
            "minecraft:a",
            JointType::Aligned
        )));
        // The reverse pairing must not be accepted: `target == name`, not
        // `name == target`.
        let reversed = jigsaw("north", "up", "minecraft:b", "minecraft:x", JointType::Aligned);
        assert!(!reversed.can_attach(&ok));
    }

    /// Two boxes sharing a face do not collide; overlapping by one block does. The
    /// `deflate(0.25)` is what makes the first true, and a village whose houses
    /// cannot touch is a village of one house.
    #[test]
    fn free_space_allows_touching_boxes_and_refuses_overlapping_ones() {
        let region = BoundingBox {
            min: [0, 0, 0],
            max: [63, 63, 63],
        };
        let mut free = FreeSpace {
            positive: Aabb::of(region),
            subtracted: Vec::new(),
        };
        let first = BoundingBox {
            min: [0, 0, 0],
            max: [7, 7, 7],
        };
        assert!(free.accepts(first));
        free.occupy(first);
        // Sharing the x = 7/8 face.
        let touching = BoundingBox {
            min: [8, 0, 0],
            max: [15, 7, 7],
        };
        assert!(free.accepts(touching), "a shared face is not a collision");
        // Overlapping by exactly one block.
        let overlapping = BoundingBox {
            min: [7, 0, 0],
            max: [15, 7, 7],
        };
        assert!(!free.accepts(overlapping), "one block of overlap is a collision");
        // Outside the positive region entirely.
        assert!(!free.accepts(BoundingBox {
            min: [60, 0, 0],
            max: [70, 7, 7],
        }));
    }

    /// The queue is highest-priority-first and FIFO within a priority — the two
    /// halves of `SequencedPriorityIterator`, and the reason a village's streets
    /// (priority 0) expand before its houses when the data says so.
    #[test]
    fn priority_queue_is_highest_first_then_fifo() {
        let mut queue: PriorityQueue<&str> = PriorityQueue::default();
        queue.add("a0", 0);
        queue.add("b5", 5);
        queue.add("c0", 0);
        queue.add("d5", 5);
        queue.add("e9", 9);
        let mut order = Vec::new();
        while let Some(next) = queue.pop() {
            order.push(next);
            // A late high-priority insert jumps the queue.
            if next == "b5" {
                queue.add("f7", 7);
            }
        }
        assert_eq!(order, vec!["e9", "b5", "f7", "d5", "a0", "c0"]);
    }

    /// `MaxDistance`'s bare-int spelling sets vertical **equal to** horizontal.
    /// Reading it as "horizontal only, vertical unbounded" would let a village
    /// stack 300 blocks high.
    #[test]
    fn max_distance_from_center_parses_both_spellings() {
        let bare: Value = serde_json::from_str(
            r#"{"start_pool":"minecraft:x","start_height":{"absolute":0},
                "size":6,"max_distance_from_center":80}"#,
        )
        .unwrap();
        let config = JigsawConfig::parse(&bare).unwrap();
        assert_eq!((config.max_horizontal, config.max_vertical), (80, 80));

        let full: Value = serde_json::from_str(
            r#"{"start_pool":"minecraft:x","start_height":{"absolute":0},
                "size":6,"max_distance_from_center":{"horizontal":116}}"#,
        )
        .unwrap();
        let config = JigsawConfig::parse(&full).unwrap();
        assert_eq!((config.max_horizontal, config.max_vertical), (116, 4064));
    }

    /// `trial_chambers`' own bindings, transcribed: one `random_group` of three
    /// two-member groups, then two `random` bindings.
    fn trial_chamber_aliases() -> Vec<PoolAlias> {
        let value: Value = serde_json::from_str(
            r#"[
              {"type":"minecraft:random_group","groups":[
                {"weight":1,"data":[
                  {"type":"minecraft:direct","alias":"a:ranged","target":"a:skeleton"},
                  {"type":"minecraft:direct","alias":"a:slow","target":"a:slow_skeleton"}]},
                {"weight":1,"data":[
                  {"type":"minecraft:direct","alias":"a:ranged","target":"a:stray"},
                  {"type":"minecraft:direct","alias":"a:slow","target":"a:slow_stray"}]},
                {"weight":1,"data":[
                  {"type":"minecraft:direct","alias":"a:ranged","target":"a:poison"},
                  {"type":"minecraft:direct","alias":"a:slow","target":"a:slow_poison"}]}]},
              {"type":"minecraft:random","alias":"a:melee","targets":[
                {"weight":1,"data":"a:zombie"},
                {"weight":1,"data":"a:husk"},
                {"weight":1,"data":"a:spider"}]},
              {"type":"minecraft:random","alias":"a:small","targets":[
                {"weight":1,"data":"a:slime"},
                {"weight":1,"data":"a:cave_spider"},
                {"weight":1,"data":"a:silverfish"},
                {"weight":1,"data":"a:baby_zombie"}]}
            ]"#,
        )
        .unwrap();
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| PoolAlias::parse(v).expect("the bundled shapes all parse"))
            .collect()
    }

    /// **The draw budget is the specification**, and it is asserted by stream
    /// position rather than by comparing resolved values: three bindings, of which
    /// the `random_group` costs one draw (the group) plus zero (two `direct`s) and
    /// each `random` costs one. Three draws, on a `nextInt(totalWeight)` each.
    ///
    /// The two wrong implementations this excludes are the ones with no visible
    /// symptom: resolving the group's members with a *fresh* random (same map,
    /// wrong count) and drawing per `direct` binding (5 draws, plausible chamber).
    #[test]
    fn pool_alias_draw_counts_are_the_specification() {
        use lodestone_worldgen_core::rng::{LegacyRandomSource, PositionalRandomFactory};
        let bindings = trial_chamber_aliases();
        let seed = -195_764_831_i64;
        let pos = [16, 32, -48];

        let lookup = PoolAliasLookup::create(&bindings, pos, seed);
        // Every alias is mapped, and the two members of the chosen group agree —
        // a `stray` ranged spawner cannot sit beside a `skeleton` slow-ranged one.
        let ranged = lookup.lookup("a:ranged").to_string();
        let slow = lookup.lookup("a:slow").to_string();
        let expected_slow = match ranged.as_str() {
            "a:skeleton" => "a:slow_skeleton",
            "a:stray" => "a:slow_stray",
            "a:poison" => "a:slow_poison",
            other => panic!("unexpected ranged target {other}"),
        };
        assert_eq!(slow, expected_slow);
        // An unbound id is the identity, which is `getOrDefault(key, key)`.
        assert_eq!(lookup.lookup("a:unbound"), "a:unbound");

        // Exactly three draws, against a hand-driven stream seeded the same way.
        let mut oracle = LegacyRandomSource::new(seed)
            .fork_positional()
            .at(pos[0], pos[1], pos[2]);
        let group = oracle.next_int_bounded(3);
        let melee = oracle.next_int_bounded(3);
        let small = oracle.next_int_bounded(4);
        assert_eq!(
            ranged,
            ["a:skeleton", "a:stray", "a:poison"][group as usize],
            "the group index is not the first draw"
        );
        assert_eq!(
            lookup.lookup("a:melee"),
            ["a:zombie", "a:husk", "a:spider"][melee as usize]
        );
        assert_eq!(
            lookup.lookup("a:small"),
            ["a:slime", "a:cave_spider", "a:silverfish", "a:baby_zombie"][small as usize]
        );

        // The map is a function of `(seed, position)` and of nothing else, and a
        // different position really does move it — otherwise every chamber in the
        // world would hold the same mob.
        assert_eq!(
            PoolAliasLookup::create(&bindings, pos, seed).lookup("a:melee"),
            lookup.lookup("a:melee")
        );
        let distinct = (0..64)
            .map(|i| {
                PoolAliasLookup::create(&bindings, [i * 16, 32, -48], seed)
                    .lookup("a:melee")
                    .to_string()
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert!(distinct.len() >= 2, "position does not move the alias map");

        // An empty binding list is `PoolAliasLookup.EMPTY`: identity, no random.
        assert_eq!(PoolAliasLookup::create(&[], pos, seed).lookup("a:melee"), "a:melee");
    }

    /// `allTargets` reaches through a `random_group`, and `all_aliases` names the
    /// ids the bundle deliberately has no document for.
    #[test]
    fn alias_targets_and_names_span_the_whole_binding_tree() {
        let bindings = trial_chamber_aliases();
        let mut targets = BTreeSet::new();
        let mut names = BTreeSet::new();
        for binding in &bindings {
            binding.all_targets(&mut targets);
            binding.all_aliases(&mut names);
        }
        // 6 from the group + 3 melee + 4 small.
        assert_eq!(targets.len(), 13, "{targets:?}");
        assert!(targets.contains("a:slow_poison"), "{targets:?}");
        assert_eq!(
            names,
            ["a:melee", "a:ranged", "a:slow", "a:small"]
                .into_iter()
                .map(str::to_string)
                .collect::<BTreeSet<_>>()
        );
    }

    /// A binding `type` this engine does not know is still refused by name, so a
    /// future one lands on the ledger instead of silently resolving to nothing.
    #[test]
    fn an_unknown_pool_alias_type_is_refused() {
        let value: Value =
            serde_json::from_str(r#"{"type":"minecraft:future","alias":"a","target":"b"}"#).unwrap();
        let why = PoolAlias::parse(&value).expect_err("an unknown type must be refused");
        assert!(why.contains("minecraft:future"), "{why}");
    }

    /// `WeightedList.getRandomOrThrow` is one `nextInt(totalWeight)` plus the
    /// cumulative walk, and a zero-weight entry is unreachable rather than rare.
    #[test]
    fn weighted_pick_walks_cumulative_weights() {
        use lodestone_worldgen_core::rng::LegacyRandomSource;
        let entries = vec![("a", 1), ("b", 0), ("c", 3)];
        let mut seen = std::collections::BTreeMap::new();
        // One stream, not one seed per trial: sequential LCG seeds correlate in
        // their first draw hard enough that the per-seed spelling of this
        // measurement reported `a` at 0.0 out of 4,000 with the code correct.
        let mut random = LegacyRandomSource::new(0xC0FF_EE01);
        let trials = 40_000;
        for _ in 0..trials {
            let pick = weighted_pick(&entries, &mut random).expect("total weight is 4");
            *seen.entry(*pick).or_insert(0) += 1;
        }
        assert_eq!(seen.get("b"), None, "a zero-weight entry was selected");
        let a = f64::from(*seen.get("a").unwrap_or(&0)) / f64::from(trials);
        assert!((a - 0.25).abs() < 0.01, "'a' selected {a} of the time, want 0.25");
        // An all-zero list is `isEmpty()`: no draw, no answer.
        let empty = vec![("x", 0)];
        let mut random = LegacyRandomSource::new(1);
        let mut control = LegacyRandomSource::new(1);
        assert!(weighted_pick(&empty, &mut random).is_none());
        assert_eq!(
            random.next_int_bounded(1_000_000),
            control.next_int_bounded(1_000_000),
            "an empty weighted list consumed a draw"
        );
    }

    /// `referencePos` is the **first** piece's box centre with its own `minY`, and
    /// `getCenter` is `min + (max - min + 1) / 2` — not `(min + max) / 2`, which
    /// differs by one on every even span.
    #[test]
    fn reference_position_is_the_first_pieces_centre_column_and_floor() {
        let piece = |min: [i32; 3], max: [i32; 3]| StructurePiece {
            id: "minecraft:jigsaw".to_string(),
            bounding_box: BoundingBox { min, max },
            orientation: None,
            gen_depth: 0,
            template: None,
            placement: None,
            extra_placements: Vec::new(),
            blocks: None,
            loot: Vec::new(),
            beard: None,
        };
        let pieces = vec![
            piece([10, 60, -20], [19, 70, -11]),
            piece([100, 100, 100], [101, 101, 101]),
        ];
        // x: 10 + (19 - 10 + 1)/2 = 15 (the `(min+max)/2` reading gives 14).
        // z: -20 + (-11 + 20 + 1)/2 = -15.
        // y: the box's own minY, not its centre.
        assert_eq!(reference_position(&pieces), [15, 60, -15]);
        assert_eq!(reference_position(&[]), [0, 0, 0]);
    }

    /// A `uniform` start height costs exactly **one** `nextInt(span)` draw and a
    /// `constant` one costs none.
    ///
    /// Asserted by stream *position* rather than by a differing value: comparing two
    /// randoms' next output would pass 999 times in 1000 even if the draw count were
    /// wrong. Each arm is compared against a stream advanced by hand exactly as many
    /// times as the record definition says.
    #[test]
    fn start_height_draw_counts_are_the_specification() {
        use lodestone_worldgen_core::rng::{LegacyRandomSource, WorldgenRandom};
        let fresh = || WorldgenRandom::new(LegacyRandomSource::new(99));

        // `ConstantHeight.sample` returns `value.resolveY(context)` — no draw.
        let mut constant_arm = fresh();
        assert_eq!(
            HeightProvider::Constant(VerticalAnchor::Absolute(0)).sample(&mut constant_arm, -64, 384),
            0
        );
        let mut untouched = fresh();
        assert_eq!(
            constant_arm.next_int_bounded(1_000_000),
            untouched.next_int_bounded(1_000_000),
            "a constant start height consumed a draw"
        );

        // `UniformHeight.sample` is `Mth.randomBetweenInclusive(random, min, max)`,
        // i.e. one `nextInt(max - min + 1)`.
        let uniform =
            HeightProvider::Uniform(VerticalAnchor::Absolute(-40), VerticalAnchor::Absolute(-20));
        let mut uniform_arm = fresh();
        let sampled = uniform.sample(&mut uniform_arm, -64, 384);
        assert!((-40..=-20).contains(&sampled), "sampled {sampled}");
        let mut oracle = fresh();
        let expected = oracle.next_int_bounded(21) - 40;
        assert_eq!(sampled, expected, "wrong span or wrong offset");
        assert_eq!(
            uniform_arm.next_int_bounded(1_000_000),
            oracle.next_int_bounded(1_000_000),
            "a uniform start height consumed more or fewer than one draw"
        );
    }

    /// `above_bottom` and `below_top` resolve against the dimension, not against
    /// zero.
    #[test]
    fn vertical_anchors_resolve_against_the_dimension() {
        assert_eq!(VerticalAnchor::Absolute(-27).resolve(-64, 384), -27);
        assert_eq!(VerticalAnchor::AboveBottom(8).resolve(-64, 384), -56);
        assert_eq!(VerticalAnchor::BelowTop(0).resolve(-64, 384), 319);
    }
}
