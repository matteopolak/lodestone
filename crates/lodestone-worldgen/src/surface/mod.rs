//! Data-driven surface-rule evaluation for generated terrain.
//!
//! [`SurfaceSystem`] consumes an aquifer-filled column and a world-surface
//! heightmap, then returns the sparse set of blocks rewritten by the configured
//! surface rule tree. Rules are parsed once, use interned result states, and
//! evaluate through a continuation graph during production scans.
//!
//! The pre-surface boundary carries both a [`StateId`] and its [`PreClass`].
//! This keeps classification explicit and avoids deriving it from shared state
//! tables while scanning.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::density::{Builder, Context as DfContext, Density};
use crate::interner::{StateId, StateInterner};
use crate::math::{floor, lerp2, map, random_between_inclusive, round};
use crate::noise::NormalNoise;
use crate::rng::{PositionalRandomFactory, RandomSource, AnyPositionalFactory};

/// Sentinel meaning that no water surface has been seen above the current block.
const NO_WATER: i32 = i32::MIN;

/// Fixed below-generation-window sentinel for the ceiling-depth scan.
///
/// This value is deliberately independent of the dimension's configured
/// minimum Y. The scan keeps it until it finds a lower non-stone block.
const WAY_BELOW_MIN_Y: i32 = -2032 << 4;

/// Sparse local `(x, y, z)` rewrites in scan-owned column storage.
///
/// The surface evaluator visits columns in `x,z` order and visits each column
/// from high Y to low Y. Its small change list therefore stays descending in
/// Y, and materializers consume each column backwards while their palette walk
/// advances upward. This keeps the boundary typed and ordered without a
/// coordinate hash, a conversion pass, or a sort. An absent position retains
/// its pre-surface state.
#[derive(Debug, Clone)]
pub struct SurfaceDiff {
    /// Rewrites packed by column. `column_offsets[n..=n + 1]` bounds the
    /// column with ordinal `n = x * 16 + z`.
    changes: Vec<(i32, StateId)>,
    column_offsets: [usize; 257],
}

impl Default for SurfaceDiff {
    fn default() -> Self {
        Self {
            changes: Vec::new(),
            column_offsets: [0; 257],
        }
    }
}

impl SurfaceDiff {
    #[inline]
    fn begin_column(&mut self, x: i32, z: i32) {
        let column = (x * 16 + z) as usize;
        debug_assert_eq!(self.column_offsets[column], self.changes.len());
    }

    #[inline]
    fn push(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        debug_assert!((0..16).contains(&x));
        debug_assert!((0..16).contains(&z));
        self.changes.push((y, state));
    }

    #[inline]
    fn finish_column(&mut self, x: i32, z: i32) {
        let column = (x * 16 + z) as usize;
        self.column_offsets[column + 1] = self.changes.len();
    }

    #[inline]
    fn range(&self, x: i32, z: i32) -> Option<std::ops::Range<usize>> {
        if !(0..16).contains(&x) || !(0..16).contains(&z) {
            return None;
        }
        let column = (x * 16 + z) as usize;
        Some(self.column_offsets[column]..self.column_offsets[column + 1])
    }

    /// Returns the rewrite at one local position, if the surface rules emitted
    /// one. This preserves the old map-shaped read seam for non-overworld
    /// callers while using a compact ordered lookup internally.
    #[must_use]
    pub fn get(&self, position: &(i32, i32, i32)) -> Option<&StateId> {
        let &(x, y, z) = position;
        let range = self.range(x, z)?;
        let start = range.start;
        self.changes[range]
            .binary_search_by(|&(candidate, _)| candidate.cmp(&y).reverse())
            .ok()
            .map(|index| &self.changes[start + index].1)
    }

    /// Number of rewritten positions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Whether no positions were rewritten.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Values in the deterministic scan order. Kept for diagnostics and the
    /// focused allocation gate; production materializers use point/column
    /// access so they do not need to iterate this boundary.
    pub fn values(&self) -> impl Iterator<Item = &StateId> {
        self.changes.iter().map(|(_, state)| state)
    }

    /// Clears rewrites while retaining the vector capacity for a worker's next
    /// chunk.
    pub fn clear(&mut self) {
        self.changes.clear();
        self.column_offsets.fill(0);
    }

    /// Returns one column's rewrites as a contiguous descending-Y slice.
    #[inline]
    pub(crate) fn column_slice(&self, x: i32, z: i32) -> &[(i32, StateId)] {
        self.range(x, z)
            .map_or(&[], |range| &self.changes[range])
    }
}

/// Classification used by the surface scan for a pre-surface block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreClass {
    /// An air state.
    Air,
    /// A non-empty water or lava state.
    Fluid,
    /// Any other state.
    Stone,
}

/// One pre-surface block as [`SurfaceSystem::build_surface`] needs it: the
/// interned state plus its [`PreClass`].
///
/// The class is supplied by the producer so scanning does not need a state-name
/// lookup. [`class_of_name`] is available for callers that start from names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PreState {
    /// The interned canonical pre-surface state.
    pub state: StateId,
    /// Its air/fluid/stone class.
    pub class: PreClass,
}

impl PreState {
    /// Air returned for an out-of-range Y coordinate.
    pub const AIR: Self = Self {
        state: StateId::AIR,
        class: PreClass::Air,
    };

    /// Builds a pre-surface state from a canonical name.
    #[must_use]
    pub fn from_name(interner: &StateInterner, name: &str) -> Self {
        Self {
            state: interner.id_of(name),
            class: class_of_name(name),
        }
    }
}

/// Classifies a canonical block-state string.
#[must_use]
pub fn class_of_name(name: &str) -> PreClass {
    if is_air(name) {
        PreClass::Air
    } else if is_fluid(name) {
        PreClass::Fluid
    } else {
        PreClass::Stone
    }
}

/// Maps a result-state partial key (`name` plus sorted specified properties) to
/// its canonical block-state string.
pub type BlockCanon = HashMap<String, String>;

/// A parsed surface-rule condition.
enum Cond {
    AbovePreliminarySurface,
    /// `biome` — a per-position runtime check
    /// (`ctx.biome` membership) rather than a build-time constant, since a
    /// generator run no longer has one fixed biome for its whole life. The
    /// list is the rule's raw `biome_is` set, exactly as written in JSON.
    BiomeIs {
        list: Vec<String>,
        cache: usize,
    },
    NoiseThreshold {
        noise: NormalNoise,
        min: f64,
        max: f64,
        is_3d: bool,
        cache: usize,
    },
    Not(Box<Cond>),
    Steep {
        cache: usize,
    },
    StoneDepth {
        offset: i32,
        add_surface_depth: bool,
        secondary_depth_range: i32,
        ceiling: bool,
        cache: usize,
    },
    Temperature {
        cache: usize,
    },
    Hole {
        cache: usize,
    },
    VerticalGradient {
        factory: AnyPositionalFactory,
        true_at_and_below: i32,
        false_at_and_above: i32,
        cache: usize,
    },
    Water {
        offset: i32,
        surface_depth_multiplier: i32,
        add_stone_depth: bool,
        cache: usize,
    },
    YAbove {
        anchor_y: i32,
        surface_depth_multiplier: i32,
        add_stone_depth: bool,
        cache: usize,
    },
}

impl Cond {
    #[inline]
    fn is_column_invariant(&self) -> bool {
        match self {
            Self::AbovePreliminarySurface
            | Self::BiomeIs { .. }
            | Self::StoneDepth { .. }
            | Self::Temperature { .. }
            | Self::VerticalGradient { .. }
            | Self::Water { .. }
            | Self::YAbove { .. } => false,
            Self::NoiseThreshold { is_3d, .. } => !is_3d,
            Self::Not(inner) => inner.is_column_invariant(),
            Self::Steep { .. } | Self::Hole { .. } => true,
        }
    }
}

/// A parsed surface rule tree.
enum Rule {
    /// Emits a canonical, interned block state.
    Block(StateId),
    /// First non-`None` child wins.
    Sequence(Vec<Rule>),
    /// Runs `then` only when `cond` holds.
    Condition(usize, Box<Rule>),
    /// Emits a state selected from the generated terracotta band table.
    Bandlands(usize),
}

/// Per-system band table and its offset noise.
struct BandBlocks {
    /// Interned entries selected by the band index.
    clay_bands: Vec<StateId>,
    /// Offset noise used by the band index.
    offset_noise: NormalNoise,
}

const NO_RULE_EDGE: usize = usize::MAX;

enum CompiledRuleNode {
    Block(StateId),
    Bandlands(usize),
    Condition {
        condition: usize,
        if_true: usize,
        if_false: usize,
    },
}

struct CompiledRule {
    nodes: Vec<CompiledRuleNode>,
    entry: usize,
}

impl CompiledRule {
    fn new(rule: &Rule) -> Self {
        let mut nodes = Vec::new();
        let entry = Self::compile(rule, &mut nodes, NO_RULE_EDGE);
        Self { nodes, entry }
    }

    fn compile(rule: &Rule, nodes: &mut Vec<CompiledRuleNode>, fallback: usize) -> usize {
        match rule {
            Rule::Block(state) => {
                let entry = nodes.len();
                nodes.push(CompiledRuleNode::Block(*state));
                entry
            }
            Rule::Bandlands(bands) => {
                let entry = nodes.len();
                nodes.push(CompiledRuleNode::Bandlands(*bands));
                entry
            }
            Rule::Condition(condition, then_run) => {
                let if_true = Self::compile(then_run, nodes, fallback);
                let entry = nodes.len();
                nodes.push(CompiledRuleNode::Condition {
                    condition: *condition,
                    if_true,
                    if_false: fallback,
                });
                entry
            }
            Rule::Sequence(rules) => {
                let mut entry = fallback;
                for rule in rules.iter().rev() {
                    entry = Self::compile(rule, nodes, entry);
                }
                entry
            }
        }
    }

    #[cfg(test)]
    fn run(
        &self,
        mut condition: impl FnMut(usize) -> bool,
        mut bandlands: impl FnMut(usize) -> StateId,
    ) -> Option<StateId> {
        let mut pc = self.entry;
        while pc != NO_RULE_EDGE {
            pc = match &self.nodes[pc] {
                CompiledRuleNode::Block(state) => return Some(*state),
                CompiledRuleNode::Bandlands(bands) => return Some(bandlands(*bands)),
                CompiledRuleNode::Condition {
                    condition: condition_id,
                    if_true,
                    if_false,
                } => {
                    if condition(*condition_id) {
                        *if_true
                    } else {
                        *if_false
                    }
                }
            };
        }
        None
    }
}

#[cfg(test)]
impl Rule {
    fn run(
        &self,
        condition: &mut impl FnMut(usize) -> bool,
        bandlands: &mut impl FnMut(usize) -> StateId,
    ) -> Option<StateId> {
        match self {
            Self::Block(state) => Some(*state),
            Self::Bandlands(bands) => Some(bandlands(*bands)),
            Self::Condition(condition_id, then_run) => {
                condition(*condition_id).then(|| then_run.run(condition, bandlands))?
            }
            Self::Sequence(rules) => rules
                .iter()
                .find_map(|rule| rule.run(condition, bandlands)),
        }
    }
}

/// Size of the generated clay-band table.
const CLAY_BANDS_LEN: usize = 192;

/// Allowed block names for generated clay-band entries.
const BAND_BLOCK_NAMES: [&str; 7] = [
    "minecraft:terracotta",
    "minecraft:orange_terracotta",
    "minecraft:yellow_terracotta",
    "minecraft:brown_terracotta",
    "minecraft:red_terracotta",
    "minecraft:white_terracotta",
    "minecraft:light_gray_terracotta",
];

impl BandBlocks {
    /// Selects a band entry for a world position.
    fn get_band(&self, world_x: i32, y: i32, world_z: i32) -> StateId {
        let offset = round(
            self.offset_noise
                .get_value(f64::from(world_x), 0.0, f64::from(world_z))
                * 4.0,
        );
        let len = CLAY_BANDS_LEN as i32;
        let index = (y + offset + len) % len;
        self.clay_bands[index as usize]
    }
}

/// Builds the deterministic clay-band table for a generator.
fn generate_bands<R: RandomSource>(random: &mut R) -> Vec<String> {
    let mut clay_bands = vec!["minecraft:terracotta".to_string(); CLAY_BANDS_LEN];

    let len = CLAY_BANDS_LEN as i32;
    let mut i: i32 = 0;
    while i < len {
        i += random.next_int_bounded(5) + 1;
        if i < len {
            clay_bands[i as usize] = "minecraft:orange_terracotta".to_string();
        }
        i += 1;
    }

    make_bands(random, &mut clay_bands, 1, "minecraft:yellow_terracotta");
    make_bands(random, &mut clay_bands, 2, "minecraft:brown_terracotta");
    make_bands(random, &mut clay_bands, 1, "minecraft:red_terracotta");

    let white_band_count = random_between_inclusive(random, 9, 15);
    let mut placed = 0;
    let mut start: i32 = 0;
    while placed < white_band_count && start < len {
        clay_bands[start as usize] = "minecraft:white_terracotta".to_string();
        if start - 1 > 0 && random.next_bool() {
            clay_bands[(start - 1) as usize] = "minecraft:light_gray_terracotta".to_string();
        }
        if start + 1 < len && random.next_bool() {
            clay_bands[(start + 1) as usize] = "minecraft:light_gray_terracotta".to_string();
        }
        placed += 1;
        start += random.next_int_bounded(16) + 4;
    }

    clay_bands
}

/// Scatters random runs of one state through the band table.
fn make_bands<R: RandomSource>(random: &mut R, clay_bands: &mut [String], base_width: i32, state: &str) {
    let band_count = random_between_inclusive(random, 6, 15);
    let len = clay_bands.len() as i32;
    for _ in 0..band_count {
        let width = base_width + random.next_int_bounded(3);
        let start = random.next_int_bounded(len);
        let mut p = 0;
        while start + p < len && p < width {
            clay_bands[(start + p) as usize] = state.to_string();
            p += 1;
        }
    }
}

/// A condition result tagged with the scan epoch in which it was computed.
#[derive(Debug, Clone, Copy)]
struct CachedBool {
    epoch: u32,
    value: bool,
}

/// Per-scan storage for lazy X/Z and Y condition results.
#[derive(Debug)]
struct EvalCache {
    xz_epoch: u32,
    y_epoch: u32,
    xz: Vec<CachedBool>,
    y: Vec<CachedBool>,
}

impl EvalCache {
    fn new(xz_slots: usize, y_slots: usize) -> Self {
        Self {
            // Zero is reserved as the never-valid epoch. The first update
            // advances both domains before any slot can be read.
            xz_epoch: 0,
            y_epoch: 0,
            xz: vec![CachedBool { epoch: 0, value: false }; xz_slots],
            y: vec![CachedBool { epoch: 0, value: false }; y_slots],
        }
    }

    #[inline(always)]
    fn begin_column(&mut self) {
        self.xz_epoch = self.xz_epoch.wrapping_add(1).max(1);
        self.y_epoch = self.y_epoch.wrapping_add(1).max(1);
    }

    #[inline(always)]
    fn begin_y(&mut self) {
        self.y_epoch = self.y_epoch.wrapping_add(1).max(1);
    }

    #[inline(always)]
    fn get_xz(&self, slot: usize) -> Option<bool> {
        let entry = &self.xz[slot];
        (self.xz_epoch != 0 && entry.epoch == self.xz_epoch).then_some(entry.value)
    }

    #[inline(always)]
    fn set_xz(&mut self, slot: usize, value: bool) {
        let epoch = self.xz_epoch;
        self.xz[slot] = CachedBool { epoch, value };
    }

    #[inline(always)]
    fn get_y(&self, slot: usize) -> Option<bool> {
        let entry = &self.y[slot];
        (self.y_epoch != 0 && entry.epoch == self.y_epoch).then_some(entry.value)
    }

    #[inline(always)]
    fn set_y(&mut self, slot: usize, value: bool) {
        let epoch = self.y_epoch;
        self.y[slot] = CachedBool { epoch, value };
    }
}

/// Per-column and per-Y state used while evaluating surface rules.
struct Ctx<'a, 'b, 'c> {
    block_x: i32,
    block_z: i32,
    surface_depth: i32,
    surface_secondary: f64,
    min_surface_level: i32,
    block_y: i32,
    water_height: i32,
    stone_depth_above: i32,
    stone_depth_below: i32,
    /// The current position's biome answer, populated only when needed.
    biome: Option<(&'a str, bool)>,
    /// The callback used by the normal chunk scan. `top_material` supplies a
    /// fixed answer instead, so its context leaves this as `None`.
    biome_at: Option<&'b dyn Fn(i32, i32, i32) -> (&'a str, bool)>,
    /// Per-call condition storage. X/Z values survive Y updates.
    cache: &'c mut EvalCache,
    /// Whether Y-condition memoization is useful for this caller. The linear
    /// column scan does not revisit a compiled node, so it can skip the cache;
    /// top-material keeps the general cached behavior.
    cache_y: bool,
}

impl<'a, 'b, 'c> Ctx<'a, 'b, 'c> {
    #[inline(always)]
    fn biome(&mut self) -> (&'a str, bool) {
        if let Some(value) = self.biome {
            return value;
        }
        let value = (self
            .biome_at
            .expect("surface rule requested a biome without a source"))(
            self.block_x & 15,
            self.block_y,
            self.block_z & 15,
        );
        self.biome = Some(value);
        value
    }

    #[inline(always)]
    fn get_y_cache(&self, slot: usize) -> Option<bool> {
        self.cache_y.then(|| self.cache.get_y(slot)).flatten()
    }

    #[inline(always)]
    fn set_y_cache(&mut self, slot: usize, value: bool) {
        if self.cache_y {
            self.cache.set_y(slot, value);
        }
    }
}

/// Parsed surface rules and their instantiated inputs.
#[allow(missing_debug_implementations)]
pub struct SurfaceSystem {
    min_y: i32,
    gen_depth: i32,
    /// Canonical state that surface rules may replace.
    default_block: StateId,
    /// State table used to convert carver results at the public boundary.
    interner: Arc<StateInterner>,
    surface_noise: NormalNoise,
    surface_secondary_noise: NormalNoise,
    master: AnyPositionalFactory,
    prelim: Arc<Density>,
    preliminary_shared: Arc<crate::aquifer::PreliminarySurfaceCache>,
    #[cfg(test)]
    rule: Rule,
    conditions: Vec<Cond>,
    column_invariant: Vec<bool>,
    bandlands: Vec<BandBlocks>,
    compiled_rule: CompiledRule,
    /// Number of condition slots used by the scan's X/Z-local and Y-local
    /// lazy predicates. Slots are assigned while parsing so one context can
    /// cache repeated condition sources without giving the rule tree interior
    /// mutability or sharing mutable state between generator calls.
    xz_cache_slots: usize,
    y_cache_slots: usize,
}

impl SurfaceSystem {
    /// Builds the surface system from dimension settings and seeded noise data.
    /// `canon` resolves result-state partial keys to full canonical strings.
    ///
    /// Biome and climate values are supplied at scan time because they vary by
    /// column.
    ///
    /// Result states and band entries are interned once during construction.
    #[must_use]
    pub fn new(
        settings: &Value,
        builder: &Builder,
        canon: &BlockCanon,
        interner: &Arc<StateInterner>,
    ) -> Self {
        Self::new_with_preliminary_cache(
            settings,
            builder,
            canon,
            interner,
            Arc::new(crate::aquifer::PreliminarySurfaceCache::new()),
        )
    }

    /// Builds a surface system using a generator/session-scoped preliminary
    /// cache shared with its aquifer systems.
    pub(crate) fn new_with_preliminary_cache(
        settings: &Value,
        builder: &Builder,
        canon: &BlockCanon,
        interner: &Arc<StateInterner>,
        preliminary_shared: Arc<crate::aquifer::PreliminarySurfaceCache>,
    ) -> Self {
        let min_y = settings["noise"]["min_y"].as_i64().unwrap_or(-64) as i32;
        let gen_depth = settings["noise"]["height"].as_i64().unwrap_or(384) as i32;
        let default_block =
            interner.id_of(&canonical_from_block_json(&settings["default_block"], canon));

        let surface_noise = builder.noise("minecraft:surface");
        let surface_secondary_noise = builder.noise("minecraft:surface_secondary");
        let master = builder.positional_factory();
        let prelim = Arc::new(
            builder
                .build(&settings["noise_router"]["preliminary_surface_level"])
                .expect("bundled preliminary_surface_level density-function document"),
        );

        let parser = RuleParser {
            builder,
            canon,
            interner,
            min_y,
            gen_depth,
            xz_cache_slots: Cell::new(0),
            y_cache_slots: Cell::new(0),
            conditions: RefCell::new(Vec::new()),
            bandlands: RefCell::new(Vec::new()),
        };
        let rule = parser.rule(&settings["surface_rule"]);
        let xz_cache_slots = parser.xz_cache_slots.get();
        let y_cache_slots = parser.y_cache_slots.get();
        let conditions = parser.conditions.into_inner();
        let column_invariant = conditions
            .iter()
            .map(Cond::is_column_invariant)
            .collect();
        let bandlands = parser.bandlands.into_inner();
        let compiled_rule = CompiledRule::new(&rule);

        Self {
            min_y,
            gen_depth,
            default_block,
            interner: Arc::clone(interner),
            surface_noise,
            surface_secondary_noise,
            master,
            prelim,
            preliminary_shared,
            #[cfg(test)]
            rule,
            conditions,
            column_invariant,
            bandlands,
            compiled_rule,
            xz_cache_slots,
            y_cache_slots,
        }
    }

    /// Computes the per-column surface depth.
    fn surface_depth(&self, x: i32, z: i32) -> i32 {
        let noise = self
            .surface_noise
            .get_value(f64::from(x), 0.0, f64::from(z));
        let extra = self.master.at(x, 0, z).next_double() * 0.25;
        (noise * 2.75 + 3.0 + extra) as i32
    }

    /// Computes the secondary surface noise at `(x, z)`.
    fn surface_secondary(&self, x: i32, z: i32) -> f64 {
        self.surface_secondary_noise
            .get_value(f64::from(x), 0.0, f64::from(z))
    }

    /// Preliminary surface-height estimate at `(sample_x, sample_z)`.
    ///
    /// Returns the low-detail preliminary surface level used by surface rules.
    pub(crate) fn preliminary_surface_level(&self, sample_x: i32, sample_z: i32) -> i32 {
        self.preliminary_surface_level_with_cache(sample_x, sample_z, &self.preliminary_shared)
    }

    fn preliminary_surface_level_with_cache(
        &self,
        sample_x: i32,
        sample_z: i32,
        preliminary_shared: &crate::aquifer::PreliminarySurfaceCache,
    ) -> i32 {
        let qx = (sample_x >> 2) << 2;
        let qz = (sample_z >> 2) << 2;
        crate::counters::bump_preliminary_surface_request(qx, qz);
        let prelim = Arc::clone(&self.prelim);
        preliminary_shared.get_or_compute_preliminary(qx, qz, || {
            crate::counters::bump_preliminary_surface_compute();
            floor(prelim.compute(DfContext::new(qx, 0, qz)))
        })
    }

    fn preliminary_surface_corners_with_cache(
        &self,
        corner_cell_x: i32,
        corner_cell_z: i32,
        preliminary_shared: &crate::aquifer::PreliminarySurfaceCache,
    ) -> [i32; 4] {
        let keys = [
            (corner_cell_x << 4, corner_cell_z << 4),
            ((corner_cell_x + 1) << 4, corner_cell_z << 4),
            (corner_cell_x << 4, (corner_cell_z + 1) << 4),
            ((corner_cell_x + 1) << 4, (corner_cell_z + 1) << 4),
        ];
        for &(qx, qz) in &keys {
            crate::counters::bump_preliminary_surface_request(qx, qz);
        }
        let mut values = [0i32; 4];
        let prelim = &self.prelim;
        preliminary_shared.get_or_compute_preliminary_batch(
            &keys,
            &mut values,
            |qx, qz| {
                crate::counters::bump_preliminary_surface_compute();
                floor(prelim.compute(DfContext::new(qx, 0, qz)))
            },
        );
        values
    }

    /// Computes the interpolated minimum surface level for one position.
    fn min_surface_level(&self, block_x: i32, block_z: i32, surface_depth: i32) -> i32 {
        let corner_cell_x = block_x >> 4;
        let corner_cell_z = block_z >> 4;
        let [c0, c1, c2, c3] = self.preliminary_surface_corners_with_cache(
            corner_cell_x,
            corner_cell_z,
            &self.preliminary_shared,
        );
        Self::interpolate_min_surface_level(block_x, block_z, surface_depth, c0, c1, c2, c3)
    }

    /// Interpolates a minimum surface level from four corner values.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn interpolate_min_surface_level(
        block_x: i32,
        block_z: i32,
        surface_depth: i32,
        c0: i32,
        c1: i32,
        c2: i32,
        c3: i32,
    ) -> i32 {
        let dx = f64::from((block_x & 15) as f32 / 16.0);
        let dz = f64::from((block_z & 15) as f32 / 16.0);
        let level = floor(lerp2(
            dx,
            dz,
            f64::from(c0),
            f64::from(c1),
            f64::from(c2),
            f64::from(c3),
        ));
        level + surface_depth - 8
    }

    /// Evaluates the configured surface rules for one 16×16 chunk.
    ///
    /// * `pre` yields the pre-surface (aquifer-filled) block at local
    ///   `(x, y, z)` (`x, z` in `0..16`, `y` a world Y) as a [`PreState`] —
    ///   interned id plus [`PreClass`]. Out-of-range Y is treated as air, and
    ///   this method applies that clamp itself, so `pre` is never asked.
    /// * `heightmap` yields `WORLD_SURFACE_WG` at local `(x, z)`.
    /// * `biome_at` yields `(biome id, cold_enough_to_snow)` at local `(x, y, z)`
    ///   when a rule actually needs the current biome. A caller whose biome
    ///   varies at quart (not block) resolution can answer from a lazy cell
    ///   context. The id is **borrowed** from the caller's own biome table.
    /// * `min_block_x`/`min_block_z` are the chunk's world-space origin.
    ///
    /// Returns a **sparse** [`SurfaceDiff`]: local `(x, y, z)` -> interned
    /// state, present only where a surface rule actually rewrote the
    /// pre-surface block. A position absent from the diff is unchanged, i.e.
    /// still exactly `pre(x, y, z)` — callers that need the full column
    /// reconstruct it from `pre` overlaid with this diff.
    ///
    #[must_use]
    pub fn build_surface<'b>(
        &self,
        pre: &dyn Fn(i32, i32, i32) -> PreState,
        heightmap: &dyn Fn(i32, i32) -> i32,
        biome_at: &dyn Fn(i32, i32, i32) -> (&'b str, bool),
        min_block_x: i32,
        min_block_z: i32,
    ) -> SurfaceDiff {
        self.build_surface_reusing_with_column_biome(
            SurfaceDiff::default(),
            pre,
            heightmap,
            biome_at,
            &|_, _, _| {},
            min_block_x,
            min_block_z,
        )
    }

    /// [`Self::build_surface`] with caller-owned scratch storage. The ordered
    /// change vector is cleared before evaluation and its capacity is retained.
    #[must_use]
    pub fn build_surface_reusing<'b>(
        &self,
        out: SurfaceDiff,
        pre: &dyn Fn(i32, i32, i32) -> PreState,
        heightmap: &dyn Fn(i32, i32) -> i32,
        biome_at: &dyn Fn(i32, i32, i32) -> (&'b str, bool),
        min_block_x: i32,
        min_block_z: i32,
    ) -> SurfaceDiff {
        self.build_surface_reusing_with_column_biome(
            out,
            pre,
            heightmap,
            biome_at,
            &|_, _, _| {},
            min_block_x,
            min_block_z,
        )
    }

    /// [`Self::build_surface_reusing`] with an initial per-column biome lookup
    /// before the descending Y scan.
    #[inline]
    #[must_use]
    pub(crate) fn build_surface_reusing_with_column_biome<'b, P, H, B, C>(
        &self,
        out: SurfaceDiff,
        pre: &P,
        heightmap: &H,
        biome_at: &B,
        column_biome_at: &C,
        min_block_x: i32,
        min_block_z: i32,
    ) -> SurfaceDiff
    where
        P: Fn(i32, i32, i32) -> PreState + ?Sized,
        H: Fn(i32, i32) -> i32 + ?Sized,
        B: Fn(i32, i32, i32) -> (&'b str, bool) + ?Sized,
        C: Fn(i32, i32, i32) + ?Sized,
    {
        self.build_surface_reusing_with_column_biome_and_preliminary_cache(
            out,
            pre,
            heightmap,
            biome_at,
            column_biome_at,
            min_block_x,
            min_block_z,
            &self.preliminary_shared,
        )
    }

    pub(crate) fn build_surface_reusing_with_preliminary_cache<'b, P, H, B, C>(
        &self,
        out: SurfaceDiff,
        pre: &P,
        heightmap: &H,
        biome_at: &B,
        column_biome_at: &C,
        min_block_x: i32,
        min_block_z: i32,
        preliminary_shared: &crate::aquifer::PreliminarySurfaceCache,
    ) -> SurfaceDiff
    where
        P: Fn(i32, i32, i32) -> PreState + ?Sized,
        H: Fn(i32, i32) -> i32 + ?Sized,
        B: Fn(i32, i32, i32) -> (&'b str, bool) + ?Sized,
        C: Fn(i32, i32, i32) + ?Sized,
    {
        self.build_surface_reusing_with_column_biome_and_preliminary_cache(
            out,
            pre,
            heightmap,
            biome_at,
            column_biome_at,
            min_block_x,
            min_block_z,
            preliminary_shared,
        )
    }

    #[inline]
    fn build_surface_reusing_with_column_biome_and_preliminary_cache<'b, P, H, B, C>(
        &self,
        mut out: SurfaceDiff,
        pre: &P,
        heightmap: &H,
        biome_at: &B,
        column_biome_at: &C,
        min_block_x: i32,
        min_block_z: i32,
        preliminary_shared: &crate::aquifer::PreliminarySurfaceCache,
    ) -> SurfaceDiff
    where
        P: Fn(i32, i32, i32) -> PreState + ?Sized,
        H: Fn(i32, i32) -> i32 + ?Sized,
        B: Fn(i32, i32, i32) -> (&'b str, bool) + ?Sized,
        C: Fn(i32, i32, i32) + ?Sized,
    {
        out.clear();
        let y_lo = self.min_y;
        let y_hi = self.min_y + self.gen_depth; // exclusive
        let way_below_min_y = WAY_BELOW_MIN_Y;

        // All columns in this chunk share these four interpolation corners.
        let corner_cell_x = min_block_x >> 4;
        let corner_cell_z = min_block_z >> 4;
        let [corner_c0, corner_c1, corner_c2, corner_c3] =
            self.preliminary_surface_corners_with_cache(
                corner_cell_x,
                corner_cell_z,
                preliminary_shared,
            );

        let mut cache = EvalCache::new(self.xz_cache_slots, self.y_cache_slots);
        let mut column_conditions = vec![0u8; self.conditions.len()];

        for x in 0..16 {
            for z in 0..16 {
                out.begin_column(x, z);
                let block_x = min_block_x + x;
                let block_z = min_block_z + z;
                let surface_depth = self.surface_depth(block_x, block_z);
                cache.begin_column();
                column_conditions.fill(0);
                let mut ctx = Ctx {
                    block_x,
                    block_z,
                    surface_depth,
                    surface_secondary: self.surface_secondary(block_x, block_z),
                    min_surface_level: Self::interpolate_min_surface_level(
                        block_x,
                        block_z,
                        surface_depth,
                        corner_c0,
                        corner_c1,
                        corner_c2,
                        corner_c3,
                    ),
                    block_y: 0,
                    water_height: NO_WATER,
                    stone_depth_above: 0,
                    stone_depth_below: 0,
                    biome: None,
                    biome_at: Some(&biome_at),
                    cache: &mut cache,
                    cache_y: false,
                };

                let height = heightmap(x, z) + 1;
                // Keep this initial lookup separate from per-block rule lookup.
                column_biome_at(x, height, z);
                let mut stone_above_depth = 0;
                let mut water_height = NO_WATER;
                let mut next_ceiling_stone_y = i32::MAX;
                let end_y = y_lo;

                let mut y = if height >= y_hi { y_hi - 1 } else { height };
                if y < y_lo {
                    out.finish_column(x, z);
                    continue;
                }
                while y >= end_y {
                    let old = pre(x, y, z);
                    if old.class == PreClass::Air {
                        stone_above_depth = 0;
                        water_height = NO_WATER;
                    } else if old.class == PreClass::Fluid {
                        if water_height == NO_WATER {
                            water_height = y + 1;
                        }
                    } else {
                        if next_ceiling_stone_y >= y {
                            next_ceiling_stone_y = way_below_min_y;
                            let mut lookahead_y = y - 1;
                            while lookahead_y >= end_y {
                                if pre(x, lookahead_y, z).class != PreClass::Stone {
                                    next_ceiling_stone_y = lookahead_y + 1;
                                    break;
                                }
                                lookahead_y -= 1;
                            }
                            if next_ceiling_stone_y == way_below_min_y {
                                next_ceiling_stone_y = end_y;
                            }
                        }

                        stone_above_depth += 1;
                        let stone_below_depth = y - next_ceiling_stone_y + 1;
                        ctx.block_y = y;
                        ctx.water_height = water_height;
                        ctx.stone_depth_above = stone_above_depth;
                        ctx.stone_depth_below = stone_below_depth;
                        ctx.cache.begin_y();
                        ctx.biome = None;

                        if old.state == self.default_block {
                            if let Some(state) = self.try_apply_compiled_column(
                                heightmap,
                                &mut ctx,
                                &mut column_conditions,
                            ) {
                                out.push(x, y, z, state);
                            }
                        }
                    }
                    y -= 1;
                }
                out.finish_column(x, z);
            }
        }

        out
    }

    /// Evaluates the surface rule for one position with the carver context
    /// (`stoneDepthAbove = 1`,
    /// `stoneDepthBelow = 1`, `waterHeight = underFluid ? y+1 : NONE`). Carvers
    /// use this to re-cap a dirt block exposed directly beneath a carved
    /// grass/mycelium block. Returns the canonical result state, or `None` if no
    /// rule matched. `heightmap(local_x, local_z)` is only consulted by the
    /// `steep` condition.
    ///
    /// The public carver boundary returns a canonical state name, so the
    /// selected interned state is resolved only after rule evaluation.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn top_material(
        &self,
        block_x: i32,
        block_y: i32,
        block_z: i32,
        under_fluid: bool,
        heightmap: &dyn Fn(i32, i32) -> i32,
        biome: &str,
        cold_enough_to_snow: bool,
    ) -> Option<String> {
        let surface_depth = self.surface_depth(block_x, block_z);
        let mut cache = EvalCache::new(self.xz_cache_slots, self.y_cache_slots);
        cache.begin_column();
        cache.begin_y();
        let mut ctx = Ctx {
            block_x,
            block_z,
            surface_depth,
            surface_secondary: self.surface_secondary(block_x, block_z),
            min_surface_level: self.min_surface_level(block_x, block_z, surface_depth),
            block_y,
            water_height: if under_fluid { block_y + 1 } else { NO_WATER },
            stone_depth_above: 1,
            stone_depth_below: 1,
            biome: Some((biome, cold_enough_to_snow)),
            biome_at: None,
            cache: &mut cache,
            cache_y: true,
        };
        self.try_apply_compiled(heightmap, &mut ctx)
            .map(|id| self.interner.name_of(id).to_string())
    }

    #[inline]
    fn try_apply_compiled<H: Fn(i32, i32) -> i32 + ?Sized>(
        &self,
        heightmap: &H,
        ctx: &mut Ctx<'_, '_, '_>,
    ) -> Option<StateId> {
        let mut pc = self.compiled_rule.entry;
        while pc != NO_RULE_EDGE {
            pc = match &self.compiled_rule.nodes[pc] {
                CompiledRuleNode::Block(state) => return Some(*state),
                CompiledRuleNode::Bandlands(bands) => {
                    return Some(self.bandlands[*bands].get_band(
                        ctx.block_x,
                        ctx.block_y,
                        ctx.block_z,
                    ));
                }
                CompiledRuleNode::Condition {
                    condition,
                    if_true,
                    if_false,
                } => {
                    if self.test(&self.conditions[*condition], heightmap, ctx) {
                        *if_true
                    } else {
                        *if_false
                    }
                }
            };
        }
        None
    }

    #[inline]
    fn try_apply_compiled_column<H: Fn(i32, i32) -> i32 + ?Sized>(
        &self,
        heightmap: &H,
        ctx: &mut Ctx<'_, '_, '_>,
        column_conditions: &mut [u8],
    ) -> Option<StateId> {
        let mut pc = self.compiled_rule.entry;
        while pc != NO_RULE_EDGE {
            pc = match &self.compiled_rule.nodes[pc] {
                CompiledRuleNode::Block(state) => return Some(*state),
                CompiledRuleNode::Bandlands(bands) => {
                    return Some(self.bandlands[*bands].get_band(
                        ctx.block_x,
                        ctx.block_y,
                        ctx.block_z,
                    ));
                }
                CompiledRuleNode::Condition {
                    condition,
                    if_true,
                    if_false,
                } => {
                    let cond = &self.conditions[*condition];
                    let value = if self.column_invariant[*condition] {
                        match column_conditions[*condition] {
                            1 => false,
                            2 => true,
                            _ => {
                                let value = self.test(cond, heightmap, ctx);
                                column_conditions[*condition] = if value { 2 } else { 1 };
                                value
                            }
                        }
                    } else {
                        // A compiled path visits each condition node at most once.
                        // The general evaluator's Y epoch cache is useful only
                        // for callers that may revisit a source; on this linear
                        // production walk it adds a bounds check and a write for
                        // every Y-dependent predicate without a possible hit.
                        self.test(cond, heightmap, ctx)
                    };
                    if value { *if_true } else { *if_false }
                }
            };
        }
        None
    }

    #[cfg(test)]
    fn try_apply_compiled_counted<H: Fn(i32, i32) -> i32 + ?Sized>(
        &self,
        heightmap: &H,
        ctx: &mut Ctx<'_, '_, '_>,
        mut column_conditions: Option<&mut [u8]>,
        counts: &mut [usize],
    ) -> Option<StateId> {
        let mut pc = self.compiled_rule.entry;
        while pc != NO_RULE_EDGE {
            pc = match &self.compiled_rule.nodes[pc] {
                CompiledRuleNode::Block(state) => return Some(*state),
                CompiledRuleNode::Bandlands(bands) => {
                    return Some(self.bandlands[*bands].get_band(
                        ctx.block_x,
                        ctx.block_y,
                        ctx.block_z,
                    ));
                }
                CompiledRuleNode::Condition {
                    condition,
                    if_true,
                    if_false,
                } => {
                    let cond = &self.conditions[*condition];
                    let value = match column_conditions.as_deref_mut() {
                        Some(values) if self.column_invariant[*condition] => {
                            match values[*condition] {
                                1 => false,
                                2 => true,
                                _ => {
                                    counts[*condition] += 1;
                                    let value = self.test(cond, heightmap, ctx);
                                    values[*condition] = if value { 2 } else { 1 };
                                    value
                                }
                            }
                        }
                        _ => {
                            counts[*condition] += 1;
                            self.test(cond, heightmap, ctx)
                        }
                    };
                    if value { *if_true } else { *if_false }
                }
            };
        }
        None
    }

    #[cfg(test)]
    #[inline]
    fn try_apply<H: Fn(i32, i32) -> i32 + ?Sized>(
        &self,
        rule: &Rule,
        heightmap: &H,
        ctx: &mut Ctx<'_, '_, '_>,
    ) -> Option<StateId> {
        match rule {
            Rule::Block(state) => Some(*state),
            Rule::Sequence(rules) => {
                for r in rules {
                    if let Some(s) = self.try_apply(r, heightmap, ctx) {
                        return Some(s);
                    }
                }
                None
            }
            Rule::Condition(condition, then) => {
                if self.test(&self.conditions[*condition], heightmap, ctx) {
                    self.try_apply(then, heightmap, ctx)
                } else {
                    None
                }
            }
            Rule::Bandlands(bands) => Some(self.bandlands[*bands].get_band(
                ctx.block_x,
                ctx.block_y,
                ctx.block_z,
            )),
        }
    }

    #[inline]
    fn test<H: Fn(i32, i32) -> i32 + ?Sized>(
        &self,
        cond: &Cond,
        heightmap: &H,
        ctx: &mut Ctx<'_, '_, '_>,
    ) -> bool {
        match cond {
            Cond::BiomeIs { list, cache } => {
                if let Some(value) = ctx.get_y_cache(*cache) {
                    return value;
                }
                let value = list
                    .iter()
                    .any(|b| b.as_str() == ctx.biome().0);
                ctx.set_y_cache(*cache, value);
                value
            }
            Cond::AbovePreliminarySurface => {
                ctx.block_y >= ctx.min_surface_level
            }
            Cond::NoiseThreshold {
                noise,
                min,
                max,
                is_3d,
                cache,
            } => {
                let cached = if *is_3d {
                    ctx.get_y_cache(*cache)
                } else {
                    ctx.cache.get_xz(*cache)
                };
                if let Some(value) = cached {
                    return value;
                }
                let x = f64::from(ctx.block_x);
                let y = f64::from(ctx.block_y);
                let z = f64::from(ctx.block_z);
                let v = if *is_3d {
                    noise.get_value(x, y, z)
                } else {
                    noise.get_value(x, 0.0, z)
                };
                let value = v >= *min && v <= *max;
                if *is_3d {
                    ctx.set_y_cache(*cache, value);
                } else {
                    ctx.cache.set_xz(*cache, value);
                }
                value
            }
            Cond::Not(inner) => !self.test(inner, heightmap, ctx),
            Cond::Steep { cache } => {
                if let Some(value) = ctx.cache.get_xz(*cache) {
                    return value;
                }
                let cbx = ctx.block_x & 15;
                let cbz = ctx.block_z & 15;
                let z_north = (cbz - 1).max(0);
                let z_south = (cbz + 1).min(15);
                let h_north = heightmap(cbx, z_north);
                let h_south = heightmap(cbx, z_south);
                let value = if h_south >= h_north + 4 {
                    true
                } else {
                    let x_west = (cbx - 1).max(0);
                    let x_east = (cbx + 1).min(15);
                    let h_west = heightmap(x_west, cbz);
                    let h_east = heightmap(x_east, cbz);
                    h_west >= h_east + 4
                };
                ctx.cache.set_xz(*cache, value);
                value
            }
            Cond::StoneDepth {
                offset,
                add_surface_depth,
                secondary_depth_range,
                ceiling,
                cache,
            } => {
                if let Some(value) = ctx.get_y_cache(*cache) {
                    return value;
                }
                let stone_depth = if *ceiling {
                    ctx.stone_depth_below
                } else {
                    ctx.stone_depth_above
                };
                let surface_depth = if *add_surface_depth {
                    ctx.surface_depth
                } else {
                    0
                };
                let secondary = if *secondary_depth_range == 0 {
                    0
                } else {
                    map(
                        ctx.surface_secondary,
                        -1.0,
                        1.0,
                        0.0,
                        f64::from(*secondary_depth_range),
                    ) as i32
                };
                let value = stone_depth <= 1 + offset + surface_depth + secondary;
                ctx.set_y_cache(*cache, value);
                value
            }
            Cond::Temperature { cache } => {
                if let Some(value) = ctx.get_y_cache(*cache) {
                    return value;
                }
                let value = ctx.biome().1;
                ctx.set_y_cache(*cache, value);
                value
            }
            Cond::Hole { cache } => {
                if let Some(value) = ctx.cache.get_xz(*cache) {
                    return value;
                }
                let value = ctx.surface_depth <= 0;
                ctx.cache.set_xz(*cache, value);
                value
            }
            Cond::VerticalGradient {
                factory,
                true_at_and_below,
                false_at_and_above,
                cache,
            } => {
                if let Some(value) = ctx.get_y_cache(*cache) {
                    return value;
                }
                let block_y = ctx.block_y;
                let value = if block_y <= *true_at_and_below {
                    true
                } else if block_y >= *false_at_and_above {
                    false
                } else {
                    let probability = map(
                        f64::from(block_y),
                        f64::from(*true_at_and_below),
                        f64::from(*false_at_and_above),
                        1.0,
                        0.0,
                    );
                    let mut random = factory.at(ctx.block_x, block_y, ctx.block_z);
                    f64::from(random.next_float()) < probability
                };
                ctx.set_y_cache(*cache, value);
                value
            }
            Cond::Water {
                offset,
                surface_depth_multiplier,
                add_stone_depth,
                cache,
            } => {
                if let Some(value) = ctx.get_y_cache(*cache) {
                    return value;
                }
                let value = ctx.water_height == NO_WATER
                    || ctx.block_y
                        + if *add_stone_depth {
                            ctx.stone_depth_above
                        } else {
                            0
                        }
                        >= ctx.water_height + offset + ctx.surface_depth * surface_depth_multiplier;
                ctx.set_y_cache(*cache, value);
                value
            }
            Cond::YAbove {
                anchor_y,
                surface_depth_multiplier,
                add_stone_depth,
                cache,
            } => {
                if let Some(value) = ctx.get_y_cache(*cache) {
                    return value;
                }
                let value = ctx.block_y
                    + if *add_stone_depth {
                        ctx.stone_depth_above
                    } else {
                        0
                    }
                    >= anchor_y + ctx.surface_depth * surface_depth_multiplier;
                ctx.set_y_cache(*cache, value);
                value
            }
        }
    }
}

/// Parses the `surface_rule` JSON into rule and condition arenas.
struct RuleParser<'a, 'b> {
    builder: &'a Builder<'b>,
    canon: &'a BlockCanon,
    /// State interner used while parsing result blocks.
    interner: &'a StateInterner,
    min_y: i32,
    gen_depth: i32,
    /// Cache-slot counters. Each parsed condition receives its own slot.
    xz_cache_slots: Cell<usize>,
    y_cache_slots: Cell<usize>,
    conditions: RefCell<Vec<Cond>>,
    bandlands: RefCell<Vec<BandBlocks>>,
}

impl RuleParser<'_, '_> {
    fn next_xz_cache_slot(&self) -> usize {
        let slot = self.xz_cache_slots.get();
        self.xz_cache_slots.set(slot + 1);
        slot
    }

    fn next_y_cache_slot(&self) -> usize {
        let slot = self.y_cache_slots.get();
        self.y_cache_slots.set(slot + 1);
        slot
    }

    fn rule(&self, node: &Value) -> Rule {
        let ty = strip(node["type"].as_str().expect("rule type"));
        match ty {
            "block" => Rule::Block(
                self.interner
                    .id_of(&canonical_from_block_json(&node["result_state"], self.canon)),
            ),
            "sequence" => Rule::Sequence(
                node["sequence"]
                    .as_array()
                    .expect("sequence")
                    .iter()
                    .map(|n| self.rule(n))
                    .collect(),
            ),
            "condition" => {
                let condition = self.cond(&node["if_true"]);
                let condition_id = {
                    let mut conditions = self.conditions.borrow_mut();
                    let id = conditions.len();
                    conditions.push(condition);
                    id
                };
                Rule::Condition(condition_id, Box::new(self.rule(&node["then_run"])))
            }
            "bandlands" => {
                let bands = self.bandlands();
                let mut bandlands = self.bandlands.borrow_mut();
                let id = bandlands.len();
                bandlands.push(bands);
                Rule::Bandlands(id)
            }
            other => panic!("unhandled surface rule type: minecraft:{other}"),
        }
    }

    /// Builds the per-system band table and offset noise.
    fn bandlands(&self) -> BandBlocks {
        let offset_noise = self.builder.noise("minecraft:clay_bands_offset");
        let mut random = self
            .builder
            .positional_factory()
            .from_hash_of("minecraft:clay_bands");
        let names = generate_bands(&mut random);

        assert_eq!(
            names.len(),
            CLAY_BANDS_LEN,
            "generate_bands must produce the configured clay-band table length"
        );
        if let Some(unknown) = names
            .iter()
            .find(|n| !BAND_BLOCK_NAMES.contains(&n.as_str()))
        {
            panic!(
                "clay band table contains {unknown:?}, which is not in \
                 BAND_BLOCK_NAMES {BAND_BLOCK_NAMES:?}"
            );
        }

        let clay_bands = names.iter().map(|n| self.interner.id_of(n)).collect();
        BandBlocks {
            clay_bands,
            offset_noise,
        }
    }

    fn cond(&self, node: &Value) -> Cond {
        let ty = strip(node["type"].as_str().expect("condition type"));
        match ty {
            "above_preliminary_surface" => Cond::AbovePreliminarySurface,
            "biome" => {
                let list = node["biome_is"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|b| b.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_else(|| {
                        vec![
                            node["biome_is"]
                                .as_str()
                                .expect("biome_is must be a string or array of strings")
                            .to_string(),
                        ]
                    });
                Cond::BiomeIs {
                    list,
                    cache: self.next_y_cache_slot(),
                }
            }
            "noise_threshold" => {
                let is_3d = node["is_3d"].as_bool().unwrap_or(false);
                let cache = if is_3d {
                    self.next_y_cache_slot()
                } else {
                    self.next_xz_cache_slot()
                };
                Cond::NoiseThreshold {
                    noise: self
                        .builder
                        .noise(node["noise"].as_str().expect("noise id")),
                    min: node["min_threshold"].as_f64().expect("min_threshold"),
                    max: node["max_threshold"].as_f64().expect("max_threshold"),
                    is_3d,
                    cache,
                }
            }
            "not" => Cond::Not(Box::new(self.cond(&node["invert"]))),
            "steep" => Cond::Steep {
                cache: self.next_xz_cache_slot(),
            },
            "stone_depth" => Cond::StoneDepth {
                offset: node["offset"].as_i64().expect("offset") as i32,
                add_surface_depth: node["add_surface_depth"]
                    .as_bool()
                    .expect("add_surface_depth"),
                secondary_depth_range: node["secondary_depth_range"]
                    .as_i64()
                    .expect("secondary_depth_range") as i32,
                ceiling: node["surface_type"].as_str() == Some("ceiling"),
                cache: self.next_y_cache_slot(),
            },
            "temperature" => Cond::Temperature {
                cache: self.next_y_cache_slot(),
            },
            "hole" => Cond::Hole {
                cache: self.next_xz_cache_slot(),
            },
            "vertical_gradient" => Cond::VerticalGradient {
                factory: self
                    .builder
                    .positional_factory()
                    .from_hash_of(node["random_name"].as_str().expect("random_name"))
                    .fork_positional(),
                true_at_and_below: self.resolve_anchor(&node["true_at_and_below"]),
                false_at_and_above: self.resolve_anchor(&node["false_at_and_above"]),
                cache: self.next_y_cache_slot(),
            },
            "water" => Cond::Water {
                offset: node["offset"].as_i64().expect("offset") as i32,
                surface_depth_multiplier: node["surface_depth_multiplier"]
                    .as_i64()
                    .expect("surface_depth_multiplier")
                    as i32,
                add_stone_depth: node["add_stone_depth"].as_bool().expect("add_stone_depth"),
                cache: self.next_y_cache_slot(),
            },
            "y_above" => Cond::YAbove {
                anchor_y: self.resolve_anchor(&node["anchor"]),
                surface_depth_multiplier: node["surface_depth_multiplier"]
                    .as_i64()
                    .expect("surface_depth_multiplier")
                    as i32,
                add_stone_depth: node["add_stone_depth"].as_bool().expect("add_stone_depth"),
                cache: self.next_y_cache_slot(),
            },
            other => panic!("unhandled surface condition type: minecraft:{other}"),
        }
    }

    /// Resolves a vertical anchor against the configured generation window.
    fn resolve_anchor(&self, node: &Value) -> i32 {
        if let Some(y) = node["absolute"].as_i64() {
            y as i32
        } else if let Some(offset) = node["above_bottom"].as_i64() {
            self.min_y + offset as i32
        } else if let Some(offset) = node["below_top"].as_i64() {
            self.gen_depth - 1 + self.min_y - offset as i32
        } else {
            panic!("unhandled vertical anchor: {node:?}")
        }
    }
}

fn strip(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

fn is_air(s: &str) -> bool {
    let name = s.split('[').next().unwrap_or(s);
    matches!(
        name,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

fn is_fluid(s: &str) -> bool {
    let name = s.split('[').next().unwrap_or(s);
    name == "minecraft:water" || name == "minecraft:lava"
}

/// Builds the partial key (`name` plus sorted specified properties) for a block
/// JSON node.
fn block_json_key(node: &Value) -> String {
    let name = node["Name"].as_str().expect("block Name");
    let mut key = String::from(name);
    if let Some(props) = node.get("Properties").and_then(Value::as_object) {
        let mut entries: Vec<(&str, String)> = props
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap_or_default().to_string()))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        if !entries.is_empty() {
            key.push('[');
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    key.push(',');
                }
                key.push_str(k);
                key.push('=');
                key.push_str(v);
            }
            key.push(']');
        }
    }
    key
}

/// Resolves a block JSON node through the caller-supplied canonical table.
fn canonical_from_block_json(node: &Value, canon: &BlockCanon) -> String {
    let key = block_json_key(node);
    canon
        .get(&key)
        .cloned()
        .unwrap_or_else(|| panic!("no canonical block for result_state key {key:?}"))
}

/// Builds an identity canonical table from the result states in a settings
/// value. Callers with non-identity canonicalization provide their own table.
#[must_use]
pub fn identity_canon(settings: &Value) -> BlockCanon {
    fn walk(node: &Value, canon: &mut BlockCanon) {
        match node {
            Value::Object(map) => {
                if map.get("Name").and_then(Value::as_str).is_some() {
                    let key = block_json_key(node);
                    canon.entry(key.clone()).or_insert(key);
                }
                for v in map.values() {
                    walk(v, canon);
                }
            }
            Value::Array(items) => {
                for v in items {
                    walk(v, canon);
                }
            }
            _ => {}
        }
    }
    let mut canon = BlockCanon::new();
    walk(&settings["surface_rule"], &mut canon);
    walk(&settings["default_block"], &mut canon);
    canon
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use serde_json::Value;

    use super::{
        class_of_name, CompiledRule, Cond, Ctx, EvalCache, PreClass, PreState, Rule, StateId, SurfaceDiff,
        SurfaceSystem, WAY_BELOW_MIN_Y, NO_WATER,
    };
    use crate::density::{Builder, NoiseParams, Resolver};
    use crate::interner::StateInterner;

    struct FsResolver {
        root: PathBuf,
    }

    impl FsResolver {
        fn read(&self, kind: &str, id: &str) -> Value {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            let path = self.root.join(kind).join(format!("{name}.json"));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
            serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
        }
    }

    impl Resolver for FsResolver {
        fn density_function(&self, id: &str) -> Value {
            self.read("density_function", id)
        }

        fn noise(&self, id: &str) -> NoiseParams {
            let value = self.read("noise", id);
            NoiseParams {
                first_octave: value["firstOctave"].as_i64().expect("firstOctave") as i32,
                amplitudes: value["amplitudes"]
                    .as_array()
                    .expect("amplitudes")
                    .iter()
                    .map(|amplitude| amplitude.as_f64().expect("amplitude"))
                    .collect(),
            }
        }
    }

    #[test]
    fn block_classification_treats_all_air_states_as_air() {
        for name in [
            "minecraft:air",
            "minecraft:cave_air",
            "minecraft:void_air",
        ] {
            assert_eq!(class_of_name(name), PreClass::Air, "{name}");
        }
        assert_eq!(
            class_of_name("minecraft:water[level=0]"),
            PreClass::Fluid,
            "water remains fluid even with a level property"
        );
        assert_eq!(class_of_name("minecraft:stone"), PreClass::Stone);
    }

    #[test]
    fn condition_cache_respects_xz_and_y_epochs() {
        let mut cache = EvalCache::new(1, 1);
        assert_eq!(cache.get_xz(0), None, "unstarted X/Z epoch must not hit");
        assert_eq!(cache.get_y(0), None, "unstarted Y epoch must not hit");

        cache.begin_column();
        cache.set_xz(0, true);
        cache.set_y(0, false);
        assert_eq!(cache.get_xz(0), Some(true));
        assert_eq!(cache.get_y(0), Some(false));

        cache.begin_y();
        assert_eq!(cache.get_xz(0), Some(true), "Y updates preserve X/Z values");
        assert_eq!(cache.get_y(0), None, "Y updates invalidate Y values");

        cache.begin_column();
        assert_eq!(cache.get_xz(0), None, "a new column invalidates X/Z values");
    }

    #[test]
    fn ceiling_scan_sentinel_is_dimension_independent() {
        assert_eq!(WAY_BELOW_MIN_Y, -2032 << 4);
        assert_ne!(WAY_BELOW_MIN_Y, -64 << 4);
    }

    #[test]
    fn compiled_rule_matches_recursive_short_circuit_order() {
        let rule = Rule::Sequence(vec![
            Rule::Condition(0, Box::new(Rule::Block(StateId::from_raw(7)))),
            Rule::Condition(1, Box::new(Rule::Block(StateId::from_raw(11)))),
            Rule::Bandlands(3),
        ]);
        let compiled = CompiledRule::new(&rule);

        let mut recursive_events = Vec::new();
        let recursive = rule.run(
            &mut |id| {
                recursive_events.push(id);
                id == 1
            },
            &mut |_| StateId::from_raw(13),
        );
        let mut compiled_events = Vec::new();
        let compiled_result = compiled.run(
            |id| {
                compiled_events.push(id);
                id == 1
            },
            |_| StateId::from_raw(13),
        );

        assert_eq!(compiled_result, recursive);
        assert_eq!(compiled_events, recursive_events);
        assert_eq!(compiled_events, [0, 1]);

        let mut recursive_events = Vec::new();
        let recursive_band_called = std::cell::Cell::new(false);
        let recursive = rule.run(
            &mut |id| {
                recursive_events.push(id);
                false
            },
            &mut |id| {
                assert_eq!(id, 3);
                recursive_band_called.set(true);
                StateId::from_raw(13)
            },
        );
        let mut compiled_events = Vec::new();
        let compiled_band_called = std::cell::Cell::new(false);
        let compiled_result = compiled.run(
            |id| {
                compiled_events.push(id);
                false
            },
            |id| {
                assert_eq!(id, 3);
                compiled_band_called.set(true);
                StateId::from_raw(13)
            },
        );
        assert_eq!(compiled_result, Some(StateId::from_raw(13)));
        assert_eq!(compiled_result, recursive);
        assert_eq!(compiled_events, recursive_events);
        assert_eq!(compiled_events, [0, 1]);
        assert!(recursive_band_called.get());
        assert!(compiled_band_called.get());
    }

    #[test]
    fn compiled_rule_keeps_lazy_callback_and_draw_events_in_order() {
        let rule = Rule::Sequence(vec![
            Rule::Condition(2, Box::new(Rule::Block(StateId::from_raw(17)))),
            Rule::Condition(4, Box::new(Rule::Block(StateId::from_raw(19)))),
        ]);
        let compiled = CompiledRule::new(&rule);

        let mut recursive_events = Vec::new();
        let recursive = rule.run(
            &mut |id| {
                recursive_events.push(format!("predicate:{id}"));
                if id == 2 {
                    recursive_events.push("random:0".to_string());
                }
                id == 4
            },
            &mut |_| StateId::AIR,
        );
        let mut compiled_events = Vec::new();
        let compiled_result = compiled.run(
            |id| {
                compiled_events.push(format!("predicate:{id}"));
                if id == 2 {
                    compiled_events.push("random:0".to_string());
                }
                id == 4
            },
            |_| StateId::AIR,
        );

        assert_eq!(compiled_result, recursive);
        assert_eq!(compiled_events, recursive_events);
        assert_eq!(compiled_events, ["predicate:2", "random:0", "predicate:4"]);
        assert_ne!(compiled_events, ["predicate:4", "predicate:2"]);
    }

    #[test]
    fn compiled_empty_sequence_keeps_recursive_no_match() {
        let rule = Rule::Sequence(Vec::new());
        let compiled = CompiledRule::new(&rule);
        assert_eq!(compiled.run(|_| true, |_| StateId::AIR), None);
        assert_eq!(rule.run(&mut |_| true, &mut |_| StateId::AIR), None);
    }

    #[test]
    fn compiled_real_settings_match_recursive_evaluation() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
        let resolver = FsResolver { root: root.clone() };
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
        )
        .unwrap();
        let builder = Builder::new(42, &resolver);
        let interner = Arc::new(StateInterner::new());
        let canon = super::identity_canon(&settings);
        let surface = SurfaceSystem::new(&settings, &builder, &canon, &interner);
        let heightmap = |x: i32, z: i32| 62 + (x * 3 + z * 5).rem_euclid(9);
        let biomes = [
            "minecraft:plains",
            "minecraft:desert",
            "minecraft:badlands",
            "minecraft:snowy_plains",
        ];
        let mut checked = 0usize;

        for &block_x in &[-17, -1, 0, 7, 15, 16, 31] {
            for &block_z in &[-17, -1, 0, 7, 15, 16, 31] {
                let surface_depth = surface.surface_depth(block_x, block_z);
                let min_surface_level =
                    surface.min_surface_level(block_x, block_z, surface_depth);
                for &biome in &biomes {
                    let biome_at = |_x: i32, _y: i32, _z: i32| (biome, false);
                    let mut compiled_cache =
                        EvalCache::new(surface.xz_cache_slots, surface.y_cache_slots);
                    let mut recursive_cache =
                        EvalCache::new(surface.xz_cache_slots, surface.y_cache_slots);
                    let mut column_cache =
                        EvalCache::new(surface.xz_cache_slots, surface.y_cache_slots);
                    let mut column_conditions = vec![0u8; surface.conditions.len()];
                    compiled_cache.begin_column();
                    recursive_cache.begin_column();
                    column_cache.begin_column();
                    column_conditions.fill(0);

                    for &block_y in &[-64, -32, 0, 32, 64, 96, 128, 160, 256, 319] {
                        for &(stone_depth_above, stone_depth_below) in
                            &[(1, 1), (2, 5), (5, 2), (12, 20)]
                        {
                            for &under_fluid in &[false, true] {
                                compiled_cache.begin_y();
                                recursive_cache.begin_y();
                                column_cache.begin_y();
                                let compiled = {
                                    let mut ctx = Ctx {
                                        block_x,
                                        block_z,
                                        surface_depth,
                                        surface_secondary: surface
                                            .surface_secondary(block_x, block_z),
                                        min_surface_level,
                                        block_y,
                                        water_height: if under_fluid {
                                            block_y + 1
                                        } else {
                                            NO_WATER
                                        },
                                        stone_depth_above,
                                        stone_depth_below,
                                        biome: None,
                                        biome_at: Some(&biome_at),
                                        cache: &mut compiled_cache,
                                        cache_y: true,
                                    };
                                    surface.try_apply_compiled(&heightmap, &mut ctx)
                                };
                                let recursive = {
                                    let mut ctx = Ctx {
                                        block_x,
                                        block_z,
                                        surface_depth,
                                        surface_secondary: surface
                                            .surface_secondary(block_x, block_z),
                                        min_surface_level,
                                        block_y,
                                        water_height: if under_fluid {
                                            block_y + 1
                                        } else {
                                            NO_WATER
                                        },
                                        stone_depth_above,
                                        stone_depth_below,
                                        biome: None,
                                        biome_at: Some(&biome_at),
                                        cache: &mut recursive_cache,
                                        cache_y: true,
                                    };
                                    surface.try_apply(&surface.rule, &heightmap, &mut ctx)
                                };
                                let column = {
                                    let mut ctx = Ctx {
                                        block_x,
                                        block_z,
                                        surface_depth,
                                        surface_secondary: surface
                                            .surface_secondary(block_x, block_z),
                                        min_surface_level,
                                        block_y,
                                        water_height: if under_fluid {
                                            block_y + 1
                                        } else {
                                            NO_WATER
                                        },
                                        stone_depth_above,
                                        stone_depth_below,
                                        biome: None,
                                        biome_at: Some(&biome_at),
                                        cache: &mut column_cache,
                                        cache_y: false,
                                    };
                                    surface.try_apply_compiled_column(
                                        &heightmap,
                                        &mut ctx,
                                        &mut column_conditions,
                                    )
                                };
                                assert_eq!(
                                    compiled, recursive,
                                    "compiled surface result diverged at ({block_x},{block_y},{block_z}) biome={biome} fluid={under_fluid} depths=({stone_depth_above},{stone_depth_below})"
                                );
                                assert_eq!(
                                    compiled, column,
                                    "column-specialized surface result diverged at ({block_x},{block_y},{block_z}) biome={biome} fluid={under_fluid} depths=({stone_depth_above},{stone_depth_below})"
                                );
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }

        assert_eq!(checked, 7 * 7 * 4 * 10 * 4 * 2);
    }

    #[test]
    fn column_specialization_reuses_invariant_predicates() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
        let resolver = FsResolver { root: root.clone() };
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
        )
        .unwrap();
        let builder = Builder::new(42, &resolver);
        let interner = Arc::new(StateInterner::new());
        let canon = super::identity_canon(&settings);
        let surface = SurfaceSystem::new(&settings, &builder, &canon, &interner);
        let heightmap = |x: i32, z: i32| 62 + (x * 3 + z * 5).rem_euclid(9);
        let biome = "minecraft:badlands";
        let biome_at = |_x: i32, _y: i32, _z: i32| (biome, false);
        let block_x = 7;
        let block_z = 9;
        let surface_depth = surface.surface_depth(block_x, block_z);
        let surface_secondary = surface.surface_secondary(block_x, block_z);
        let min_surface_level = surface.min_surface_level(block_x, block_z, surface_depth);
        let mut ordinary_cache =
            EvalCache::new(surface.xz_cache_slots, surface.y_cache_slots);
        let mut column_cache = EvalCache::new(surface.xz_cache_slots, surface.y_cache_slots);
        ordinary_cache.begin_column();
        column_cache.begin_column();
        let mut ordinary_ctx = Ctx {
            block_x,
            block_z,
            surface_depth,
            surface_secondary,
            min_surface_level,
            block_y: 0,
            water_height: NO_WATER,
            stone_depth_above: 0,
            stone_depth_below: 0,
            biome: None,
            biome_at: Some(&biome_at),
            cache: &mut ordinary_cache,
            cache_y: true,
        };
        let mut column_ctx = Ctx {
            block_x,
            block_z,
            surface_depth,
            surface_secondary,
            min_surface_level,
            block_y: 0,
            water_height: NO_WATER,
            stone_depth_above: 0,
            stone_depth_below: 0,
            biome: None,
            biome_at: Some(&biome_at),
            cache: &mut column_cache,
            cache_y: false,
        };
        let mut column_conditions = vec![0u8; surface.conditions.len()];
        let mut ordinary_counts = vec![0usize; surface.conditions.len()];
        let mut column_counts = vec![0usize; surface.conditions.len()];
        let mut y = surface.min_y + surface.gen_depth - 1;
        while y >= surface.min_y {
            let stone_above_depth = 1;
            let stone_below_depth = 1;
            ordinary_ctx.block_y = y;
            ordinary_ctx.stone_depth_above = stone_above_depth;
            ordinary_ctx.stone_depth_below = stone_below_depth;
            ordinary_ctx.biome = None;
            ordinary_ctx.cache.begin_y();
            let ordinary = surface.try_apply_compiled_counted(
                &heightmap,
                &mut ordinary_ctx,
                None,
                &mut ordinary_counts,
            );
            let production_ordinary = surface.try_apply_compiled(&heightmap, &mut ordinary_ctx);
            assert_eq!(ordinary, production_ordinary);

            column_ctx.block_y = y;
            column_ctx.stone_depth_above = stone_above_depth;
            column_ctx.stone_depth_below = stone_below_depth;
            column_ctx.biome = None;
            column_ctx.cache.begin_y();
            let specialized = surface.try_apply_compiled_counted(
                &heightmap,
                &mut column_ctx,
                Some(&mut column_conditions),
                &mut column_counts,
            );
            assert_eq!(ordinary, specialized, "surface result diverged at y={y}");
            let production_specialized = surface.try_apply_compiled_column(
                &heightmap,
                &mut column_ctx,
                &mut column_conditions,
            );
            assert_eq!(specialized, production_specialized);
            y -= 1;
        }

        let ordinary_invariant = ordinary_counts
            .iter()
            .enumerate()
            .filter(|(id, _)| surface.column_invariant[*id])
            .map(|(_, count)| count)
            .sum::<usize>();
        let specialized_invariant = column_counts
            .iter()
            .enumerate()
            .filter(|(id, _)| surface.column_invariant[*id])
            .map(|(_, count)| count)
            .sum::<usize>();
        let visited_invariant = column_counts
            .iter()
            .enumerate()
            .filter(|(id, count)| surface.column_invariant[*id] && **count != 0)
            .count();
        assert!(
            ordinary_invariant > specialized_invariant,
            "ordinary={ordinary_invariant} specialized={specialized_invariant} visited={visited_invariant} available={}",
            surface.column_invariant.iter().filter(|&&value| value).count(),
        );
        assert_eq!(specialized_invariant, visited_invariant);
        for (id, invariant) in surface.column_invariant.iter().enumerate() {
            if !invariant {
                assert_eq!(
                    ordinary_counts[id],
                    column_counts[id],
                    "Y-dependent condition {id} was hoisted"
                );
            }
        }
        let y_dependent_id = surface
            .conditions
            .iter()
            .position(|condition| matches!(condition, Cond::AbovePreliminarySurface))
            .expect("real settings must exercise a Y-dependent predicate");
        assert!(ordinary_counts[y_dependent_id] > 1);
        assert_eq!(ordinary_counts[y_dependent_id], column_counts[y_dependent_id]);
    }

    #[test]
    fn surface_diff_orders_each_column_for_materialization() {
        let mut diff = SurfaceDiff::default();
        diff.begin_column(0, 0);
        diff.push(0, 9, 0, StateId::from_raw(11));
        diff.push(0, 3, 0, StateId::from_raw(12));
        diff.finish_column(0, 0);
        diff.begin_column(0, 1);
        diff.push(0, 7, 1, StateId::from_raw(13));
        diff.finish_column(0, 1);

        assert_eq!(diff.len(), 3);
        assert_eq!(diff.get(&(0, 3, 0)).copied(), Some(StateId::from_raw(12)));
        assert_eq!(diff.get(&(0, 9, 0)).copied(), Some(StateId::from_raw(11)));
        assert_eq!(diff.get(&(0, 6, 0)), None);
        assert_eq!(diff.column_slice(0, 0), &[(9, StateId::from_raw(11)), (3, StateId::from_raw(12))]);
        assert_eq!(diff.column_slice(0, 1), &[(7, StateId::from_raw(13))]);
    }

    /// Bounded release-facing characterization for the compiled column path.
    /// Run explicitly with `--ignored --nocapture`; the digest is the control
    /// that makes an A/B kernel comparison meaningful when timings move.
    #[test]
    #[ignore]
    fn surface_kernel_profile_shaped_fixture() {
        use lodestone_time::Instant;
        use sha2::{Digest, Sha256};

        #[cfg(target_os = "macos")]
        #[allow(unsafe_code)]
        fn usage() -> (u64, u64) {
            #[repr(C)]
            #[derive(Default)]
            struct Rusage {
                _uuid: [u8; 16],
                _user_time: u64,
                _system_time: u64,
                _pkg_idle_wkups: u64,
                _interrupt_wkups: u64,
                _pageins: u64,
                _wired_size: u64,
                _resident_size: u64,
                _phys_footprint: u64,
                _proc_start_abstime: u64,
                _proc_exit_abstime: u64,
                _child_user_time: u64,
                _child_system_time: u64,
                _child_pkg_idle_wkups: u64,
                _child_interrupt_wkups: u64,
                _child_pageins: u64,
                _child_elapsed_abstime: u64,
                _diskio_bytesread: u64,
                _diskio_byteswritten: u64,
                _cpu_time_qos_default: u64,
                _cpu_time_qos_maintenance: u64,
                _cpu_time_qos_background: u64,
                _cpu_time_qos_utility: u64,
                _cpu_time_qos_legacy: u64,
                _cpu_time_qos_user_initiated: u64,
                _cpu_time_qos_user_interactive: u64,
                _billed_system_time: u64,
                _serviced_system_time: u64,
                _logical_writes: u64,
                _lifetime_max_phys_footprint: u64,
                instructions: u64,
                cycles: u64,
                _billed_energy: u64,
                _serviced_energy: u64,
                _interval_max_phys_footprint: u64,
                _runnable_time: u64,
                _flags: u64,
            }
            unsafe extern "C" {
                fn proc_pid_rusage(
                    pid: i32,
                    flavor: i32,
                    buffer: *mut core::ffi::c_void,
                ) -> i32;
            }
            let mut info = Rusage::default();
            let rc = unsafe {
                proc_pid_rusage(
                    i32::try_from(std::process::id()).unwrap(),
                    4,
                    (&raw mut info).cast::<core::ffi::c_void>(),
                )
            };
            assert_eq!(rc, 0, "proc_pid_rusage failed with {rc}");
            (info.instructions, info.cycles)
        }

        #[cfg(not(target_os = "macos"))]
        fn usage() -> (u64, u64) {
            panic!("surface instruction/cycle profile requires macOS proc_pid_rusage");
        }

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
        let resolver = FsResolver { root: root.clone() };
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
        )
        .unwrap();
        let builder = Builder::new(42, &resolver);
        let interner = Arc::new(StateInterner::new());
        let canon = super::identity_canon(&settings);
        let surface = SurfaceSystem::new(&settings, &builder, &canon, &interner);
        let stone = PreState::from_name(&interner, "minecraft:stone");
        let water = PreState::from_name(&interner, "minecraft:water");
        let heightmap = |x: i32, z: i32| 62 + (x * 7 + z * 11).rem_euclid(17);
        let pre = |x: i32, y: i32, z: i32| {
            let top = 72 + (x * 7 + z * 11).rem_euclid(41);
            if y > top {
                PreState::AIR
            } else if y > top - 3 && (x + z).rem_euclid(5) == 0 {
                water
            } else {
                stone
            }
        };
        let biome_at = |_x: i32, _y: i32, _z: i32| ("minecraft:plains", false);
        let digest = |diff: &SurfaceDiff| {
            let mut digest = Sha256::new();
            for x in 0..16 {
                for z in 0..16 {
                    for y in surface.min_y..surface.min_y + surface.gen_depth {
                        let state = diff
                            .get(&(x, y, z))
                            .copied()
                            .unwrap_or_else(|| pre(x, y, z).state);
                        digest.update(state.raw().to_le_bytes());
                    }
                }
            }
            digest.finalize()
        };
        let run = || surface.build_surface(&pre, &heightmap, &biome_at, 0, 0);
        let warm = run();
        let _ = digest(&warm);
        let mut times = Vec::with_capacity(9);
        let mut instructions = Vec::with_capacity(9);
        let mut cycles = Vec::with_capacity(9);
        let mut output_digest = None;
        let mut rewrites = 0;
        for _ in 0..9 {
            let (before_instructions, before_cycles) = usage();
            let start = Instant::now();
            let diff = run();
            times.push(start.elapsed().as_nanos());
            let (after_instructions, after_cycles) = usage();
            instructions.push(after_instructions - before_instructions);
            cycles.push(after_cycles - before_cycles);
            rewrites = diff.len();
            output_digest = Some(digest(&diff));
        }
        times.sort_unstable();
        instructions.sort_unstable();
        cycles.sort_unstable();
        println!(
            "SURFACE_KERNEL seed=42 fixture=shaped-full rewrites={rewrites} median_ns={} median_instructions={} median_cycles={} digest={:02x}",
            times[times.len() / 2],
            instructions[instructions.len() / 2],
            cycles[cycles.len() / 2],
            output_digest.expect("profile produced output"),
        );
    }
}
