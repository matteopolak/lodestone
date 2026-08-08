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

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use lodestone_worldgen_core::rng::RandomSource;
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
    fn parse(value: &Value) -> Option<Self> {
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

    fn sample<R: RandomSource>(self, random: &mut R, min_y: i32, height: i32) -> i32 {
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
}

impl JigsawConfig {
    /// Parses a `minecraft:jigsaw` structure document, or returns why it cannot
    /// be modelled.
    ///
    /// **Refuses rather than defaults.** `pool_aliases` is the case that matters:
    /// a trial chamber whose aliases were ignored would place spawners with no
    /// contents and consume a different number of draws, so it is demoted and
    /// named instead.
    pub fn parse(value: &Value) -> Result<Self, String> {
        if value["pool_aliases"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            return Err("jigsaw `pool_aliases` (a random-group alias binding \
                        redirects a pool per structure instance)"
                .to_string());
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
        })
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
}

/// `JigsawPlacement.addPieces`' eager half: everything up to the stub position.
///
/// `Ok(None)` is vanilla's `Optional.empty()` — an empty centre pool, a missing
/// named start jigsaw, or a centre piece that does not fit the dimension padding.
/// No start at all, before any biome check.
pub fn begin<R: RandomSource>(
    config: &JigsawConfig,
    pools: &PoolStore,
    cx: i32,
    cz: i32,
    min_y: i32,
    height: i32,
    ctx: &dyn StartContext,
    mut random: R,
) -> Option<JigsawStub<R>> {
    let start_height = config.start_height.sample(&mut random, min_y, height);
    let position = [cx * 16, start_height, cz * 16];
    let centre_rotation = Rotation::random(&mut random);
    let centre_pool = pools.get(&config.start_pool)?;
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
        ..
    } = stub;
    let mut placer = Placer {
        pools,
        max_depth: config.max_depth,
        expansion_hack: config.use_expansion_hack,
        pieces: vec![centre],
        frees: vec![free],
        placing: PriorityQueue::default(),
    };
    if config.max_depth > 0 {
        placer.try_placing_children(0, 0, 0, ctx, &mut random);
        while let Some(state) = placer.placing.pop() {
            placer.try_placing_children(state.piece, state.free, state.depth, ctx, &mut random);
        }
    }
    placer.into_pieces(config.waterlogging, ctx)
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

            let Some(target_pool) = self.pools.get(&source_jigsaw.pool) else {
                // "Empty or non-existent pool" — a warn and a `continue`.
                continue;
            };
            if target_pool.size() == 0 && source_jigsaw.pool != "minecraft:empty" {
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

    /// `pool_aliases` is refused, not defaulted — the ledger's job.
    #[test]
    fn pool_aliases_are_refused_by_name() {
        let value: Value = serde_json::from_str(
            r#"{"start_pool":"minecraft:x","start_height":{"absolute":0},"size":6,
                "pool_aliases":[{"type":"minecraft:random","alias":"a","targets":[]}]}"#,
        )
        .unwrap();
        let why = JigsawConfig::parse(&value).expect_err("aliases must be refused");
        assert!(why.contains("pool_aliases"), "{why}");
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
