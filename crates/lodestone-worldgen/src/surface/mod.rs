//! Version-free interpreter for vanilla's surface-rule system.
//!
//! This is the stage that turns the post-aquifer density field (a column of
//! stone / water / lava / air) into recognisable terrain — grass over dirt over
//! stone, sand near water, gravel on the ocean floor, bedrock at the bottom and
//! deepslate below `y = 0`. Like the noise router it is **data-driven**: the
//! `surface_rule` tree lives in the version crate's `noise_settings` and this
//! engine only *interprets* it (plan §3).
//!
//! # What it consumes
//!
//! [`SurfaceSystem::build_surface`] takes the **pre-surface column** (the
//! aquifer-filled block field, exactly what vanilla's noise-chunk sampler
//! and its fill pass produce) and the `WORLD_SURFACE_WG`
//! heightmap, and reproduces vanilla's own surface-building scan
//! block-for-block. The pre-surface states are taken as given (so the engine
//! needs no block registry): a rule only ever *replaces* the default block
//! (stone) with one of the surface rule's result states, whose canonical form
//! is supplied by the caller (version data, exactly like the block registry).
//!
//! # This seam speaks [`StateId`], not `String`
//!
//! Every block-state that crosses this engine's boundary is an **interned
//! [`StateId`]**, resolved once at construction. Before U21 the `pre` callback
//! returned `String`, [`SurfaceSystem::try_apply`] returned `Option<String>`
//! and the diff was `HashMap<_, String>`: measured over a 3×3 cold sweep at
//! seed 42 (`tests/ore_alloc_attribution.rs`), that was **3,847,972 real
//! `GlobalAlloc` calls, 97.3% of the whole pipeline's heap traffic** — 18× the
//! entire ore path — from four `to_string()`/`clone()` sites on a per-probe
//! path. `docs/worldgen-surface-ids.md` carries the measurement.
//!
//! Three properties make the conversion total rather than a relocation, and
//! each is the thing to preserve if you change this file:
//!
//! * **Nothing is interned during a scan.** [`Rule::Block`] holds a `StateId`
//!   resolved at parse time, and the caller hands [`PreState`]s built from
//!   ids it already owns. There is no `id_of` and no `name_of` — and therefore
//!   no `RwLock` — anywhere under [`SurfaceSystem::build_surface`]. That
//!   matters beyond allocation: `4307b59` is this repo's scar for putting many
//!   concurrent generator calls on one shared cache line.
//! * **`Rule::Bandlands`' "computed" name is a table subscript.**
//!   Vanilla's own band lookup looked like the blocker — it *computes* which
//!   block it returns rather than selecting a static one — but the set it
//!   computes over is vanilla's own clay-bands table, exactly [`CLAY_BANDS_LEN`]
//!   entries drawn from the [`BAND_BLOCK_NAMES`] seven. So the whole band set
//!   is known once per world seed and pre-interned into `Vec<StateId>` by
//!   [`RuleParser::bandlands`], which also *asserts* the finiteness rather
//!   than assuming it. `get_band` is now an index and a `Copy`.
//! * **Classification is supplied, not re-derived.** The scan branches on
//!   air/fluid/stone, which a `String` let it read off the name. [`PreState`]
//!   carries a [`PreClass`] beside the id so the branch costs nothing, and
//!   [`class_of_name`] keeps the *string* definition (`is_air`/`is_fluid`)
//!   as the single source of truth that a supplied class is checked against.
//!
//! # Parity discipline
//!
//! The oracle (`scripts/worldgen-oracle/SurfaceOracle.java`) drives vanilla's
//! *own* compiled surface-building pass and dumps both columns; the test
//! compares block-for-block over the whole chunk and names the divergent
//! `x,y,z`. No Mojang source is transliterated — this is written from the
//! documented algorithm and checked against the running server (plan §11).

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use lodestone_worldgen_core::hash::FastMap;
use serde::Deserialize;
use serde_json::Value;

use crate::density::{Builder, Context as DfContext, Density};
use crate::interner::{StateId, StateInterner};
use crate::math::{floor, lerp2, map, random_between_inclusive, round};
use crate::noise::NormalNoise;
use crate::rng::{PositionalRandomFactory, RandomSource, AnyPositionalFactory};

/// Strict schema for the surface-rule tree embedded in noise settings.
///
/// The rule and condition objects are closed discriminated unions. The only
/// untagged shape is `biome_is` (one string or a list), which is a documented
/// two-shape field in the registry format. Block `Properties` remains a map
/// because property names are defined by the referenced block registry rather
/// than by this engine.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum SurfaceRuleNode {
    #[serde(rename = "minecraft:block")]
    Block(BlockRuleFields),
    #[serde(rename = "minecraft:sequence")]
    Sequence(SequenceRuleFields),
    #[serde(rename = "minecraft:condition")]
    Condition(ConditionRuleFields),
    #[serde(rename = "minecraft:bandlands")]
    Bandlands(EmptySurfaceObject),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptySurfaceObject {}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockRuleFields {
    result_state: BlockStateDocument,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SequenceRuleFields {
    sequence: Vec<SurfaceRuleNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConditionRuleFields {
    if_true: SurfaceConditionNode,
    then_run: Box<SurfaceRuleNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockStateDocument {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Properties", default)]
    properties: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum SurfaceConditionNode {
    #[serde(rename = "minecraft:above_preliminary_surface")]
    AbovePreliminarySurface(EmptySurfaceObject),
    #[serde(rename = "minecraft:biome")]
    Biome(BiomeConditionFields),
    #[serde(rename = "minecraft:noise_threshold")]
    NoiseThreshold(NoiseThresholdConditionFields),
    #[serde(rename = "minecraft:not")]
    Not(NotConditionFields),
    #[serde(rename = "minecraft:steep")]
    Steep(EmptySurfaceObject),
    #[serde(rename = "minecraft:stone_depth")]
    StoneDepth(StoneDepthConditionFields),
    #[serde(rename = "minecraft:temperature")]
    Temperature(EmptySurfaceObject),
    #[serde(rename = "minecraft:hole")]
    Hole(EmptySurfaceObject),
    #[serde(rename = "minecraft:vertical_gradient")]
    VerticalGradient(VerticalGradientConditionFields),
    #[serde(rename = "minecraft:water")]
    Water(WaterConditionFields),
    #[serde(rename = "minecraft:y_above")]
    YAbove(YAboveConditionFields),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BiomeConditionFields {
    biome_is: OneOrManyStrings,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum OneOrManyStrings {
    One(String),
    Many(Vec<String>),
}

impl OneOrManyStrings {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(value) => vec![value],
            Self::Many(values) => values,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoiseThresholdConditionFields {
    noise: String,
    min_threshold: f64,
    max_threshold: f64,
    #[serde(default)]
    is_3d: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NotConditionFields {
    invert: Box<SurfaceConditionNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoneDepthConditionFields {
    offset: i32,
    add_surface_depth: bool,
    secondary_depth_range: i32,
    surface_type: SurfaceType,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SurfaceType {
    Floor,
    Ceiling,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerticalGradientConditionFields {
    random_name: String,
    true_at_and_below: VerticalAnchor,
    false_at_and_above: VerticalAnchor,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaterConditionFields {
    offset: i32,
    surface_depth_multiplier: i32,
    add_stone_depth: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct YAboveConditionFields {
    anchor: VerticalAnchor,
    surface_depth_multiplier: i32,
    add_stone_depth: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum VerticalAnchor {
    Absolute(AbsoluteAnchor),
    AboveBottom(AboveBottomAnchor),
    BelowTop(BelowTopAnchor),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AbsoluteAnchor {
    absolute: i32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AboveBottomAnchor {
    above_bottom: i32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BelowTopAnchor {
    below_top: i32,
}

/// The vanilla `Integer.MIN_VALUE` sentinel meaning "no water above".
const NO_WATER: i32 = i32::MIN;

/// Fixed below-generation-window sentinel for the ceiling-depth scan.
///
/// This value is deliberately independent of the dimension's configured
/// minimum Y. The scan keeps it until it finds a lower non-stone block.
const WAY_BELOW_MIN_Y: i32 = -2032 << 4;

/// The **sparse** surface diff [`SurfaceSystem::build_surface`] returns: local
/// `(x, y, z)` -> the interned state a surface rule rewrote that position to.
/// A position absent from the map is unchanged from the pre-surface column.
///
/// # Why a [`FastMap`] is safe here
///
/// `docs/worldgen-fast-hashing.md` requires the other half of the argument to
/// be established **at the map**, in one of exactly two forms, because a
/// hasher swap changes iteration order and this repo has already shipped a
/// palette permutation from exactly that (`crate::overworld`'s module doc).
/// This map takes the *first* form — **never iterated**. Its only consumer,
/// [`crate::overworld::OverworldGenerator::materialize_world`], reads it by
/// **point lookup** in the same fixed `(lz, lx, ly)` order as its own base
/// fill, precisely so that the `DenseBlockGrid` palette is appended in a
/// deterministic order independent of this map; that call site's own comment
/// carries the post-mortem. The parity tests likewise only `get`.
///
/// So the check to re-run after editing anything here is a grep for `.iter()`
/// / `.keys()` / `.values()` / `.drain()` / `for (.., ..) in` against a
/// `SurfaceDiff` **binding**, not against a file. If a consumer ever needs to
/// iterate it, this alias must go back to `HashMap` or the consumer must
/// impose a total order of its own and say so.
pub type SurfaceDiff = FastMap<(i32, i32, i32), StateId>;

/// Which of vanilla's three surface-building classes a pre-surface block is in.
///
/// The scan branches on this and nothing else about the block's identity, so
/// carrying it beside the id (see [`PreState`]) is what lets the pre-surface
/// callback stop returning a `String` that only ever got its *name* read to
/// answer these three questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreClass {
    /// Vanilla's own "is this state air" check.
    Air,
    /// Vanilla's own "is this state a non-empty fluid" check — water or lava, at any property set.
    Fluid,
    /// Neither: vanilla's implicit "stone" case, `!isAir && fluid.isEmpty()`.
    Stone,
}

/// One pre-surface block as [`SurfaceSystem::build_surface`] needs it: the
/// interned state plus its [`PreClass`].
///
/// # Why the class is a field and not recomputed
///
/// Deriving the class from an id needs either the state's *name* (an interner
/// `RwLock` read per probe — ~60,000 per chunk, on a table shared by every
/// concurrent generator call) or a base-name lookup (the same lock). The
/// caller always already knows the class for free: `overworld/fill.rs` reads
/// it straight off the `BlockKind` its own aquifer fill wrote.
///
/// That is a shortcut, so it is *checked* rather than trusted —
/// [`class_of_name`] is the string definition it must agree with, and
/// `surface_stage` asserts the agreement for every id it can produce on every
/// call under `debug_assertions`. Without that check this would be the
/// fully-connected-wire-carrying-the-wrong-value shape `CLAUDE.md` warns
/// about: a mis-classified pre-surface block changes which rules fire and
/// still produces a plausible column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PreState {
    /// The interned canonical pre-surface state.
    pub state: StateId,
    /// Its air/fluid/stone class.
    pub class: PreClass,
}

impl PreState {
    /// `minecraft:air` — what an out-of-range Y reads as, matching vanilla's
    /// own out-of-section behaviour.
    pub const AIR: Self = Self {
        state: StateId::AIR,
        class: PreClass::Air,
    };

    /// A pre-surface block whose class is derived from its **name**, for a
    /// caller holding a canonical string rather than a pre-classified field
    /// (the JVM parity fixtures). Production does not take this path; it is
    /// also the reference [`class_of_name`] agreement is asserted against.
    #[must_use]
    pub fn from_name(interner: &StateInterner, name: &str) -> Self {
        Self {
            state: interner.id_of(name),
            class: class_of_name(name),
        }
    }
}

/// The [`PreClass`] of a canonical block-state string — the definition the
/// scan used to apply to every probe, kept as one function so a caller that
/// supplies a class can be checked against it.
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

/// Maps a result-state *partial key* (`name` + sorted specified `[k=v]`) to its
/// full canonical block string (all properties, defaults filled). Supplied by
/// the caller from the version's block data — see the oracle's `canonmap.*`
/// lines. Keeping this out of the engine preserves the version-free split.
pub type BlockCanon = HashMap<String, String>;

/// A parsed surface-rule condition (vanilla's condition-source tree applied to
/// a context).
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

/// A parsed surface rule (vanilla's rule-source tree).
enum Rule {
    /// Emits a fully-canonical block state, interned at parse time (U21) so a
    /// match is a `u16` copy rather than a `String` clone. This arm was
    /// `try_apply`'s `Some(state.clone())`, measured at 21.92% of the surface
    /// stage's 3,847,972 allocations.
    Block(StateId),
    /// First non-`None` child wins.
    Sequence(Vec<Rule>),
    /// Runs `then` only when `cond` holds.
    Condition(Cond, Box<Rule>),
    /// Badlands/eroded_badlands/wooded_badlands' banded-terracotta rule
    /// (vanilla's bandlands rule, whose logic delegates to its surface
    /// system's own band lookup — previously a carried-over gap, closed
    /// here). Unconditional and parameterless in
    /// vanilla's own DSL (its bandlands rule is a zero-field enum
    /// singleton), so the [`BandBlocks`] payload is built once at parse time
    /// from the generator's own seed, not from anything in the JSON node —
    /// see [`RuleParser::bandlands`].
    Bandlands(Box<BandBlocks>),
}

/// Vanilla's own band-lookup state: the 192-entry clay-band table plus
/// the noise that perturbs which entry a given `y` lands on.
///
/// Built once per world seed ([`RuleParser::bandlands`]), not per column or
/// per block — matching vanilla, where the clay-bands table is an
/// instance field generated once in the constructor
/// (by vanilla's own one-time band-table build), never touched again after
/// construction.
struct BandBlocks {
    /// Vanilla's own clay-bands table — always exactly
    /// [`CLAY_BANDS_LEN`] entries long, each the **interned id** of a full
    /// canonical block string (these seven blocks carry no properties at 26.2
    /// — see [`generate_bands`]'s doc comment — so no [`BlockCanon`] lookup is
    /// needed, unlike every other [`Rule::Block`] result state).
    ///
    /// Interned once per world seed by [`RuleParser::bandlands`], which is the
    /// whole reason `Bandlands` was not the blocker it looked like: see the
    /// module doc's third bullet.
    clay_bands: Vec<StateId>,
    /// Vanilla's own clay-bands offset noise (`minecraft:clay_bands_offset`).
    offset_noise: NormalNoise,
}

/// Vanilla's own hardcoded clay-bands table size
/// (its one-time band-table build allocates exactly 192 entries), not derived from
/// anything version-supplied.
const CLAY_BANDS_LEN: usize = 192;

/// Every block [`generate_bands`] can write into the clay-band table, and
/// therefore the *entire* value set vanilla's own band lookup can return.
///
/// This list is what makes `Rule::Bandlands` pre-internable: the band index is
/// computed per block, but the thing indexed is drawn from these seven names,
/// fixed at 26.2. [`RuleParser::bandlands`] asserts the built table against
/// this rather than trusting it (`CLAUDE.md` rule 2), so an eighth band block
/// in a future version fails loudly at generator construction instead of
/// silently reintroducing a per-block intern.
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
    /// Vanilla's own band lookup at `(world_x, y, world_z)`. Never returns `None` —
    /// vanilla's own bandlands rule (delegating to this same lookup on its
    /// context's surface system) is a bare
    /// rule function reference with no condition wrapped around it,
    /// so every call that reaches [`Rule::Bandlands`] gets a real block back.
    fn get_band(&self, world_x: i32, y: i32, world_z: i32) -> StateId {
        let offset = round(
            self.offset_noise
                .get_value(f64::from(world_x), 0.0, f64::from(world_z))
                * 4.0,
        );
        let len = CLAY_BANDS_LEN as i32;
        // `y` ranges over this engine's own `min_y..min_y+gen_depth` (as low
        // as vanilla's `-64`) and `offset` is a noise sample scaled by 4, so
        // `y + offset + len` is always positive in practice — matching why
        // vanilla adds the clay-bands length here at all rather than needing
        // a true Euclidean modulo.
        let index = (y + offset + len) % len;
        // A `Copy` out of a pre-interned table. This line was
        // `self.clay_bands[index as usize].clone()`.
        self.clay_bands[index as usize]
    }
}

/// Vanilla's own one-time clay-bands table build.
/// `random` must be the noise random's positional factory forked from the
/// hash of `"minecraft:clay_bands"`
/// ([`RuleParser::bandlands`]), matching vanilla's own derivation exactly
/// (a *positional* factory's `from_hash_of`, not any per-block draw).
///
/// The seven result blocks (`minecraft:terracotta` and six
/// `minecraft:*_terracotta` dye variants) are hardcoded here rather than
/// routed through [`BlockCanon`]/[`canonical_from_block_document`] because
/// vanilla's own `minecraft:bandlands` rule JSON node carries no `result_state` at all
/// (it is `{"type": "minecraft:bandlands"}`, nothing else — vanilla's own
/// rule type has zero fields), so
/// [`identity_canon`](crate::surface::identity_canon)'s walk of the
/// `surface_rule` tree never sees these block names and has no key for them.
/// Confirmed property-less at 26.2 by `docs/worldgen-parity.md`'s own
/// measured oracle output, which names them bare (`orange_terracotta`, not
/// `orange_terracotta[...]`) in the earlier badlands gap breakdown.
fn generate_bands<R: RandomSource>(random: &mut R) -> Vec<String> {
    let mut clay_bands = vec!["minecraft:terracotta".to_string(); CLAY_BANDS_LEN];

    // Vanilla's own loop is a C-style `for` over the table whose header still
    // fires its own `i++` every iteration *in addition to* the body's own
    // `i += (bounded random draw over [0,5)) + 1`, so each step actually
    // advances `i` by that draw plus 2, not plus 1. Translated as an explicit
    // `while` with both increments spelled out so that trap can't silently
    // drop the `+ 1` a naive `for i in ...` rewrite would.
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

/// Vanilla's own band-scattering routine — scatters a random count of runs
/// of `state`, each `base_width..base_width+3` entries wide, at independently
/// random starts.
/// Plain `for` loops in the original (no self-modifying index), so this is a
/// direct, non-tricky translation unlike [`generate_bands`]'s first loop.
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

/// A lazily-filled condition result. The epoch is advanced when the scan
/// changes its X/Z column or its Y position, matching the two invalidation
/// domains of the surface-rule context.
#[derive(Debug, Clone, Copy)]
struct CachedBool {
    epoch: u64,
    value: bool,
}

/// Scratch storage for the surface-rule context's lazy predicates.
///
/// The rule tree is shared by every generated chunk and therefore cannot hold
/// mutable per-scan values itself. Conversely, putting a new map behind every
/// condition would add hashing and allocation to the hottest stage. The parser
/// assigns each condition a compact slot, and one small epoch-tagged array is
/// reused for a complete `build_surface`/`top_material` call. X/Z conditions
/// survive Y updates; Y conditions are invalidated for every scanned block.
#[derive(Debug)]
struct EvalCache {
    xz_epoch: u64,
    y_epoch: u64,
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

    fn begin_column(&mut self) {
        self.xz_epoch = self.xz_epoch.wrapping_add(1).max(1);
        self.y_epoch = self.y_epoch.wrapping_add(1).max(1);
    }

    fn begin_y(&mut self) {
        self.y_epoch = self.y_epoch.wrapping_add(1).max(1);
    }

    fn get_xz(&self, slot: usize) -> Option<bool> {
        let entry = self
            .xz
            .get(slot)
            .unwrap_or_else(|| panic!("surface X/Z cache slot {slot} is out of range"));
        (self.xz_epoch != 0 && entry.epoch == self.xz_epoch).then_some(entry.value)
    }

    fn set_xz(&mut self, slot: usize, value: bool) {
        let epoch = self.xz_epoch;
        let entry = self
            .xz
            .get_mut(slot)
            .unwrap_or_else(|| panic!("surface X/Z cache slot {slot} is out of range"));
        *entry = CachedBool { epoch, value };
    }

    fn get_y(&self, slot: usize) -> Option<bool> {
        let entry = self
            .y
            .get(slot)
            .unwrap_or_else(|| panic!("surface Y cache slot {slot} is out of range"));
        (self.y_epoch != 0 && entry.epoch == self.y_epoch).then_some(entry.value)
    }

    fn set_y(&mut self, slot: usize, value: bool) {
        let epoch = self.y_epoch;
        let entry = self
            .y
            .get_mut(slot)
            .unwrap_or_else(|| panic!("surface Y cache slot {slot} is out of range"));
        *entry = CachedBool { epoch, value };
    }
}

/// Per-column / per-Y scan state mirroring vanilla's own surface-rule scan context.
struct Ctx<'a, 'b> {
    block_x: i32,
    block_z: i32,
    surface_depth: i32,
    surface_secondary: f64,
    min_surface_level: i32,
    block_y: i32,
    water_height: i32,
    stone_depth_above: i32,
    stone_depth_below: i32,
    /// This position's biome id — consulted by [`Cond::BiomeIs`],
    /// which only ever *compares* it.
    ///
    /// **Borrowed, not owned** (U21). It was a `String`, and both producers had
    /// to clone into it: `build_surface`'s `biome_at` callback for each
    /// rule-evaluated position and `top_material` once per call on the carver
    /// path. Neither clone bought anything — the biome table this borrows from
    /// outlives the scan in both cases.
    biome: &'a str,
    /// This column's biome's "cold enough to snow" answer — consulted by
    /// [`Cond::Temperature`]. See [`crate::biome::cold_enough_to_snow`].
    cold_enough_to_snow: bool,
    /// Per-call lazy condition storage. This is borrowed so concurrent
    /// generators never share cache state, while one scan can reuse X/Z
    /// values across all Y positions in its column.
    cache: &'b mut EvalCache,
}

/// The interpreter: instantiated noises + parsed rule tree, ready to build any
/// chunk's surface from its pre-surface column.
#[allow(missing_debug_implementations)]
pub struct SurfaceSystem {
    min_y: i32,
    gen_depth: i32,
    /// The settings' `default_block`, interned — vanilla's own
    /// "is this the default block" guard is now a `u16` compare rather than a string compare.
    default_block: StateId,
    /// The table every id in this system was issued by. Held so
    /// [`Self::top_material`] can still hand a `String` to the carver seam,
    /// which is *not* part of this unit and keeps its `Option<String>`
    /// signature; see that method's own note. Never touched by
    /// [`Self::build_surface`].
    interner: Arc<StateInterner>,
    surface_noise: NormalNoise,
    surface_secondary_noise: NormalNoise,
    master: AnyPositionalFactory,
    prelim: Density,
    rule: Rule,
    /// Number of condition slots used by the scan's X/Z-local and Y-local
    /// lazy predicates. Slots are assigned while parsing so one context can
    /// cache repeated condition sources without giving the rule tree interior
    /// mutability or sharing mutable state between generator calls.
    xz_cache_slots: usize,
    y_cache_slots: usize,
}

impl SurfaceSystem {
    /// Builds the interpreter for `settings` (a `noise_settings` JSON value)
    /// using `builder` (already seeded with the same seed) to instantiate
    /// noises and derive random factories exactly as vanilla's own
    /// per-world random-state holder does.
    /// `canon` resolves result-state partial keys to full canonical strings.
    ///
    /// This takes **no biome** — a generator run no
    /// longer has one fixed biome for its whole life, so `biome`/
    /// `cold_enough_to_snow` moved from build-time constants here to
    /// per-column runtime inputs on [`build_surface`](Self::build_surface)/
    /// [`top_material`](Self::top_material) instead.
    ///
    /// `interner` is the generator's own [`StateInterner`] (U21). Every result
    /// state in the `surface_rule` tree, every clay band and `default_block`
    /// are interned **here**, once, so nothing under
    /// [`build_surface`](Self::build_surface) ever takes the interner's lock.
    /// This is a bounded set walked out of the parsed data itself, not a
    /// hand-maintained pre-intern list, so it cannot drift from the data the
    /// way `crate::overworld`'s own note about pre-populating warns.
    #[must_use]
    pub fn new(
        settings: &Value,
        builder: &Builder,
        canon: &BlockCanon,
        interner: &Arc<StateInterner>,
    ) -> Self {
        let min_y = settings["noise"]["min_y"].as_i64().unwrap_or(-64) as i32;
        let gen_depth = settings["noise"]["height"].as_i64().unwrap_or(384) as i32;
        let default_block_document: BlockStateDocument = parse_surface_document(&settings["default_block"])
            .expect("bundled noise settings default_block must match the strict block-state schema");
        let default_block = interner.id_of(&canonical_from_block_document(&default_block_document, canon));

        let surface_noise = builder.noise("minecraft:surface");
        let surface_secondary_noise = builder.noise("minecraft:surface_secondary");
        let master = builder.positional_factory();
        let prelim = builder
            .build(&settings["noise_router"]["preliminary_surface_level"])
            .expect("bundled preliminary_surface_level density-function document");

        let parser = RuleParser {
            builder,
            canon,
            interner,
            min_y,
            gen_depth,
            xz_cache_slots: Cell::new(0),
            y_cache_slots: Cell::new(0),
        };
        let surface_rule: SurfaceRuleNode = parse_surface_document(&settings["surface_rule"])
            .expect("bundled noise settings surface_rule must match the strict rule schema");
        let rule = parser.rule(&surface_rule);
        let xz_cache_slots = parser.xz_cache_slots.get();
        let y_cache_slots = parser.y_cache_slots.get();

        Self {
            min_y,
            gen_depth,
            default_block,
            interner: Arc::clone(interner),
            surface_noise,
            surface_secondary_noise,
            master,
            prelim,
            rule,
            xz_cache_slots,
            y_cache_slots,
        }
    }

    /// Vanilla's own surface-depth lookup at `(x, z)`.
    fn surface_depth(&self, x: i32, z: i32) -> i32 {
        let noise = self
            .surface_noise
            .get_value(f64::from(x), 0.0, f64::from(z));
        let extra = self.master.at(x, 0, z).next_double() * 0.25;
        (noise * 2.75 + 3.0 + extra) as i32
    }

    /// Vanilla's own secondary surface-noise lookup at `(x, z)`.
    fn surface_secondary(&self, x: i32, z: i32) -> f64 {
        self.surface_secondary_noise
            .get_value(f64::from(x), 0.0, f64::from(z))
    }

    /// Preliminary surface-height estimate at `(sample_x, sample_z)`.
    ///
    /// This has no block-grid allocation or generation-store access. It is the
    /// deliberate low-detail input for a distant heightfield, not a substitute
    /// for a generated column: carving, aquifers, and surface rules can still
    /// alter the final visible top block.
    pub(crate) fn preliminary_surface_level(&self, sample_x: i32, sample_z: i32) -> i32 {
        // Vanilla's quart<->block conversion round-trip collapses to
        // (v >> 2) << 2.
        let qx = (sample_x >> 2) << 2;
        let qz = (sample_z >> 2) << 2;
        floor(self.prelim.compute(DfContext::new(qx, 0, qz)))
    }

    /// Vanilla's own scan-context minimum-surface-level lookup.
    ///
    /// Used by [`Self::top_material`], which queries one arbitrary position at a
    /// time (carvers), so it computes its own corner cell fresh. [`Self::build_surface`]
    /// scans a whole 16×16 chunk at once — every column in that chunk shares the
    /// same `block_x >> 4` / `block_z >> 4` corner cell (chunk width is exactly
    /// 16, and `min_block_x`/`min_block_z` are always chunk-aligned per the
    /// contract this type is built around), so it hoists the four corner
    /// `preliminary_surface_level` calls out to once per chunk via
    /// [`Self::interpolate_min_surface_level`] instead of once per column —
    /// same four corner values, just not recomputed 256 times over.
    fn min_surface_level(&self, block_x: i32, block_z: i32, surface_depth: i32) -> i32 {
        let corner_cell_x = block_x >> 4;
        let corner_cell_z = block_z >> 4;
        let c0 = self.preliminary_surface_level(corner_cell_x << 4, corner_cell_z << 4);
        let c1 = self.preliminary_surface_level((corner_cell_x + 1) << 4, corner_cell_z << 4);
        let c2 = self.preliminary_surface_level(corner_cell_x << 4, (corner_cell_z + 1) << 4);
        let c3 = self.preliminary_surface_level((corner_cell_x + 1) << 4, (corner_cell_z + 1) << 4);
        Self::interpolate_min_surface_level(block_x, block_z, surface_depth, c0, c1, c2, c3)
    }

    /// The interpolation half of [`Self::min_surface_level`], factored out so a
    /// caller that already knows the four corner `preliminary_surface_level`
    /// values (e.g. one chunk's worth of columns, all sharing the same corner
    /// cell) can skip recomputing them per column.
    #[allow(clippy::too_many_arguments)]
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

    /// Reproduces vanilla's own surface-building scan for one 16×16 chunk.
    ///
    /// * `pre` yields the pre-surface (aquifer-filled) block at local
    ///   `(x, y, z)` (`x, z` in `0..16`, `y` a world Y) as a [`PreState`] —
    ///   interned id plus [`PreClass`]. Out-of-range Y is treated as air, and
    ///   this method applies that clamp itself, so `pre` is never asked.
    /// * `heightmap` yields `WORLD_SURFACE_WG` at local `(x, z)`.
    /// * `biome_at` yields `(biome id, cold_enough_to_snow)` at local `(x, y, z)`
    ///   — called for each stone position that reaches rule evaluation. A
    ///   caller whose biome varies at quart (not block) resolution can cheaply
    ///   answer from its precomputed cell context. The id is **borrowed** from
    ///   the caller's own biome table (U21).
    /// * `min_block_x`/`min_block_z` are the chunk's world-space origin.
    ///
    /// Returns a **sparse** [`SurfaceDiff`]: local `(x, y, z)` -> interned
    /// state, present only where a surface rule actually rewrote the
    /// pre-surface block. A position absent from the map is unchanged, i.e.
    /// still exactly `pre(x, y, z)` — callers that need the full column
    /// reconstruct it from `pre` overlaid with this map, rather than the map
    /// alone. Read [`SurfaceDiff`]'s own doc before iterating it.
    ///
    /// This used to be an exhaustive map (every one of a chunk's 16×16×`gen_depth`
    /// positions inserted up front from `pre`, then selectively overwritten by
    /// matched rules) so callers could treat the return value as the whole
    /// column. Profiling with CPU-time weighting (see `docs/roadmap/benchmarks.md`)
    /// showed that exhaustive
    /// pre-fill — 98304 `String` clones and `HashMap` inserts per chunk for a
    /// gen_depth of 384, the overwhelming majority of them immediately
    /// discarded unread — was itself close to a fifth of total column-generation
    /// time (`SipHasher`/`RawTable::reserve_rehash`/`memmove` self-time). The
    /// scan below still needs `pre`/`block_at` for its own classification logic
    /// (unchanged); only the redundant up-front full-column copy is gone.
    #[must_use]
    pub fn build_surface<'b>(
        &self,
        pre: &dyn Fn(i32, i32, i32) -> PreState,
        heightmap: &dyn Fn(i32, i32) -> i32,
        biome_at: &dyn Fn(i32, i32, i32) -> (&'b str, bool),
        min_block_x: i32,
        min_block_z: i32,
    ) -> SurfaceDiff {
        let y_lo = self.min_y;
        let y_hi = self.min_y + self.gen_depth; // exclusive
        let way_below_min_y = WAY_BELOW_MIN_Y;

        // The four `preliminary_surface_level` corner values for this chunk's
        // corner cell. Every one of the 256 columns below shares the same
        // `block_x >> 4` / `block_z >> 4` (chunk width is exactly 16 and
        // `min_block_x`/`min_block_z` are chunk-aligned), so — unlike
        // `min_surface_level`'s single-position form used by `top_material` —
        // these are computed once per chunk rather than once per column. Each
        // `preliminary_surface_level` call walks a `find_top_surface` density
        // search (up to `(upper_bound - lower_bound) / cell_height` steps), so
        // this turns 256 searches into 4.
        let corner_cell_x = min_block_x >> 4;
        let corner_cell_z = min_block_z >> 4;
        let corner_c0 = self.preliminary_surface_level(corner_cell_x << 4, corner_cell_z << 4);
        let corner_c1 =
            self.preliminary_surface_level((corner_cell_x + 1) << 4, corner_cell_z << 4);
        let corner_c2 =
            self.preliminary_surface_level(corner_cell_x << 4, (corner_cell_z + 1) << 4);
        let corner_c3 =
            self.preliminary_surface_level((corner_cell_x + 1) << 4, (corner_cell_z + 1) << 4);

        let mut out: SurfaceDiff = SurfaceDiff::default();
        let mut cache = EvalCache::new(self.xz_cache_slots, self.y_cache_slots);

        // Immutable classification source: vanilla only ever reads the original
        // column while scanning (`old` is at the current, not-yet-written Y and
        // the ceiling look-ahead only reads lower, unvisited Y).
        let block_at = |x: i32, y: i32, z: i32| -> PreState {
            if y < y_lo || y >= y_hi {
                PreState::AIR
            } else {
                pre(x, y, z)
            }
        };

        for x in 0..16 {
            for z in 0..16 {
                let block_x = min_block_x + x;
                let block_z = min_block_z + z;
                let surface_depth = self.surface_depth(block_x, block_z);
                cache.begin_column();
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
                    biome: "",
                    cold_enough_to_snow: false,
                    cache: &mut cache,
                };

                let height = heightmap(x, z) + 1;
                let mut stone_above_depth = 0;
                let mut water_height = NO_WATER;
                let mut next_ceiling_stone_y = i32::MAX;
                let end_y = y_lo;

                let mut y = height;
                while y >= end_y {
                    let old = block_at(x, y, z);
                    // Was `is_air(&old)` / `is_fluid(&old)` on the block's
                    // *name*; the class is now supplied beside the id and
                    // checked against `class_of_name` at the production seam.
                    // The three arms are the same three, in the same order.
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
                            while lookahead_y >= end_y - 1 {
                                // `!is_stone(..)` — `is_stone` was exactly
                                // `!is_air && !is_fluid`, i.e. `PreClass::Stone`.
                                if block_at(x, lookahead_y, z).class != PreClass::Stone {
                                    next_ceiling_stone_y = lookahead_y + 1;
                                    break;
                                }
                                lookahead_y -= 1;
                            }
                        }

                        stone_above_depth += 1;
                        let stone_below_depth = y - next_ceiling_stone_y + 1;
                        ctx.block_y = y;
                        ctx.water_height = water_height;
                        ctx.stone_depth_above = stone_above_depth;
                        ctx.stone_depth_below = stone_below_depth;
                        ctx.cache.begin_y();
                        let (biome, cold_enough_to_snow) = biome_at(x, y, z);
                        ctx.biome = biome;
                        ctx.cold_enough_to_snow = cold_enough_to_snow;

                        if old.state == self.default_block {
                            if let Some(state) = self.try_apply(&self.rule, heightmap, &mut ctx) {
                                out.insert((x, y, z), state);
                            }
                        }
                    }
                    y -= 1;
                }
            }
        }

        out
    }

    /// `SurfaceSystem.topMaterial` — evaluate the surface rule for a single
    /// position with the carver's fixed context (`stoneDepthAbove = 1`,
    /// `stoneDepthBelow = 1`, `waterHeight = underFluid ? y+1 : NONE`). Carvers
    /// use this to re-cap a dirt block exposed directly beneath a carved
    /// grass/mycelium block. Returns the canonical result state, or `None` if no
    /// rule matched. `heightmap(local_x, local_z)` is only consulted by the
    /// `steep` condition.
    ///
    /// # Why this still returns an owned `String` (U21)
    ///
    /// The carver seam (`crate::carver`'s `top_material: &dyn Fn(..) ->
    /// Option<String>`) is out of scope here and was left untouched,
    /// so this method resolves its id back to a name at the boundary. That is
    /// **allocation-neutral, by construction**: the pre-U21 body allocated one
    /// `String` for the biome and one for the matched state, and this one
    /// allocates one for the matched state and none for the biome — so the
    /// carve stage can only go down, never up. Converting the carver seam to
    /// ids is the obvious follow-up and is deliberately not done here.
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
            biome,
            cold_enough_to_snow,
            cache: &mut cache,
        };
        self.try_apply(&self.rule, heightmap, &mut ctx)
            .map(|id| self.interner.name_of(id).to_string())
    }

    fn try_apply(
        &self,
        rule: &Rule,
        heightmap: &dyn Fn(i32, i32) -> i32,
        ctx: &mut Ctx<'_, '_>,
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
            Rule::Condition(cond, then) => {
                if self.test(cond, heightmap, ctx) {
                    self.try_apply(then, heightmap, ctx)
                } else {
                    None
                }
            }
            Rule::Bandlands(bands) => Some(bands.get_band(ctx.block_x, ctx.block_y, ctx.block_z)),
        }
    }

    fn test(&self, cond: &Cond, heightmap: &dyn Fn(i32, i32) -> i32, ctx: &mut Ctx<'_, '_>) -> bool {
        match cond {
            Cond::BiomeIs { list, cache } => {
                if let Some(value) = ctx.cache.get_y(*cache) {
                    return value;
                }
                let value = list.iter().any(|b| b.as_str() == ctx.biome);
                ctx.cache.set_y(*cache, value);
                value
            }
            Cond::AbovePreliminarySurface => ctx.block_y >= ctx.min_surface_level,
            Cond::NoiseThreshold {
                noise,
                min,
                max,
                is_3d,
                cache,
            } => {
                let cached = if *is_3d {
                    ctx.cache.get_y(*cache)
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
                    ctx.cache.set_y(*cache, value);
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
                if let Some(value) = ctx.cache.get_y(*cache) {
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
                ctx.cache.set_y(*cache, value);
                value
            }
            Cond::Temperature { cache } => {
                if let Some(value) = ctx.cache.get_y(*cache) {
                    return value;
                }
                let value = ctx.cold_enough_to_snow;
                ctx.cache.set_y(*cache, value);
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
                if let Some(value) = ctx.cache.get_y(*cache) {
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
                ctx.cache.set_y(*cache, value);
                value
            }
            Cond::Water {
                offset,
                surface_depth_multiplier,
                add_stone_depth,
                cache,
            } => {
                if let Some(value) = ctx.cache.get_y(*cache) {
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
                ctx.cache.set_y(*cache, value);
                value
            }
            Cond::YAbove {
                anchor_y,
                surface_depth_multiplier,
                add_stone_depth,
                cache,
            } => {
                if let Some(value) = ctx.cache.get_y(*cache) {
                    return value;
                }
                let value = ctx.block_y
                    + if *add_stone_depth {
                        ctx.stone_depth_above
                    } else {
                        0
                    }
                    >= anchor_y + ctx.surface_depth * surface_depth_multiplier;
                ctx.cache.set_y(*cache, value);
                value
            }
        }
    }
}

/// Parses the `surface_rule` JSON into [`Rule`]/[`Cond`] trees, instantiating
/// noises and random factories at parse time (mirroring vanilla's
/// `ConditionSource.apply`).
struct RuleParser<'a, 'b> {
    builder: &'a Builder<'b>,
    canon: &'a BlockCanon,
    /// Where every result state and clay band is interned, once, at parse time
    /// — so `try_apply` never touches the table. See [`SurfaceSystem::new`].
    interner: &'a StateInterner,
    min_y: i32,
    gen_depth: i32,
    /// Compact cache-slot counters. Each parsed condition gets its own slot,
    /// preserving the source-level lazy-condition identity even when two
    /// conditions happen to have equal JSON values.
    xz_cache_slots: Cell<usize>,
    y_cache_slots: Cell<usize>,
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

    fn rule(&self, node: &SurfaceRuleNode) -> Rule {
        match node {
            SurfaceRuleNode::Block(fields) => Rule::Block(
                self.interner
                    .id_of(&canonical_from_block_document(&fields.result_state, self.canon)),
            ),
            SurfaceRuleNode::Sequence(fields) => Rule::Sequence(
                fields.sequence.iter().map(|child| self.rule(child)).collect(),
            ),
            SurfaceRuleNode::Condition(fields) => Rule::Condition(
                self.cond(&fields.if_true),
                Box::new(self.rule(&fields.then_run)),
            ),
            SurfaceRuleNode::Bandlands(_) => Rule::Bandlands(Box::new(self.bandlands())),
        }
    }

    /// Builds [`BandBlocks`] for a `"minecraft:bandlands"` rule node — once
    /// per occurrence of that node in the `surface_rule` tree at parse time
    /// (there is exactly one in vanilla's real `overworld.json`), matching
    /// vanilla's own generator constructor calling its one-time band-table
    /// build exactly once
    /// per world. `self.builder.positional_factory()` is the same `master`
    /// factory [`SurfaceSystem::new`] itself stores (vanilla's own
    /// generator-wide random-state field, i.e. what vanilla calls its noise
    /// random) — see this module's own `master` field
    /// doc for why that identity holds.
    /// U21 added the interning of the finished table, and the two assertions
    /// that make "the band set is finite" a checked claim rather than an
    /// assumption. `generate_bands` itself is untouched — every RNG draw in it
    /// is world-defining, and its `Vec<String>` is built exactly once per
    /// generator, so leaving it in strings costs 192 allocations per world.
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
            "generate_bands must produce exactly vanilla's clay-bands table length in entries"
        );
        if let Some(unknown) = names
            .iter()
            .find(|n| !BAND_BLOCK_NAMES.contains(&n.as_str()))
        {
            panic!(
                "clay band table contains {unknown:?}, which is not one of \
                 BAND_BLOCK_NAMES {BAND_BLOCK_NAMES:?} — Rule::Bandlands' \
                 pre-interning assumes the band set is exactly those seven \
                 blocks (see this module's doc); add the new block to the list \
                 rather than reintroducing a per-block intern"
            );
        }

        let clay_bands = names.iter().map(|n| self.interner.id_of(n)).collect();
        BandBlocks {
            clay_bands,
            offset_noise,
        }
    }

    fn cond(&self, node: &SurfaceConditionNode) -> Cond {
        match node {
            SurfaceConditionNode::AbovePreliminarySurface(_) => Cond::AbovePreliminarySurface,
            SurfaceConditionNode::Biome(fields) => {
                let list = fields.biome_is.clone().into_vec();
                Cond::BiomeIs {
                    list,
                    cache: self.next_y_cache_slot(),
                }
            }
            SurfaceConditionNode::NoiseThreshold(fields) => {
                let is_3d = fields.is_3d;
                let cache = if is_3d {
                    self.next_y_cache_slot()
                } else {
                    self.next_xz_cache_slot()
                };
                Cond::NoiseThreshold {
                    noise: self
                        .builder
                        .noise(&fields.noise),
                    min: fields.min_threshold,
                    max: fields.max_threshold,
                    is_3d,
                    cache,
                }
            }
            SurfaceConditionNode::Not(fields) => Cond::Not(Box::new(self.cond(&fields.invert))),
            SurfaceConditionNode::Steep(_) => Cond::Steep {
                cache: self.next_xz_cache_slot(),
            },
            SurfaceConditionNode::StoneDepth(fields) => Cond::StoneDepth {
                offset: fields.offset,
                add_surface_depth: fields.add_surface_depth,
                secondary_depth_range: fields.secondary_depth_range,
                ceiling: matches!(fields.surface_type, SurfaceType::Ceiling),
                cache: self.next_y_cache_slot(),
            },
            SurfaceConditionNode::Temperature(_) => Cond::Temperature {
                cache: self.next_y_cache_slot(),
            },
            SurfaceConditionNode::Hole(_) => Cond::Hole {
                cache: self.next_xz_cache_slot(),
            },
            SurfaceConditionNode::VerticalGradient(fields) => Cond::VerticalGradient {
                factory: self
                    .builder
                    .positional_factory()
                    .from_hash_of(&fields.random_name)
                    .fork_positional(),
                true_at_and_below: self.resolve_anchor(&fields.true_at_and_below),
                false_at_and_above: self.resolve_anchor(&fields.false_at_and_above),
                cache: self.next_y_cache_slot(),
            },
            SurfaceConditionNode::Water(fields) => Cond::Water {
                offset: fields.offset,
                surface_depth_multiplier: fields.surface_depth_multiplier,
                add_stone_depth: fields.add_stone_depth,
                cache: self.next_y_cache_slot(),
            },
            SurfaceConditionNode::YAbove(fields) => Cond::YAbove {
                anchor_y: self.resolve_anchor(&fields.anchor),
                surface_depth_multiplier: fields.surface_depth_multiplier,
                add_stone_depth: fields.add_stone_depth,
                cache: self.next_y_cache_slot(),
            },
        }
    }

    /// Vanilla's own vertical-anchor resolution against the world-generation context.
    fn resolve_anchor(&self, node: &VerticalAnchor) -> i32 {
        match node {
            VerticalAnchor::Absolute(anchor) => anchor.absolute,
            VerticalAnchor::AboveBottom(anchor) => self.min_y + anchor.above_bottom,
            VerticalAnchor::BelowTop(anchor) => self.gen_depth - 1 + self.min_y - anchor.below_top,
        }
    }
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

// `is_stone` was `!is_air(s) && !is_fluid(s)`. It is gone as a function because
// `class_of_name`'s `else` arm *is* that expression, and the scan now compares
// `PreClass::Stone` rather than calling it — see `build_surface`'s lookahead.

/// The partial key (`name` + sorted specified `[k=v]`) for a `{Name, Properties?}`
/// block JSON node — the lookup key into a [`BlockCanon`].
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

fn canonical_from_block_document(node: &BlockStateDocument, canon: &BlockCanon) -> String {
    let mut key = node.name.clone();
    if let Some(properties) = &node.properties {
        if !properties.is_empty() {
            key.push('[');
            for (index, (property, value)) in properties.iter().enumerate() {
                if index > 0 {
                    key.push(',');
                }
                key.push_str(property);
                key.push('=');
                key.push_str(value);
            }
            key.push(']');
        }
    }
    canon
        .get(&key)
        .cloned()
        .unwrap_or_else(|| panic!("no canonical block for result_state key {key:?}"))
}

fn parse_surface_document<T: for<'de> Deserialize<'de>>(
    value: &Value,
) -> Result<T, String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
    serde_path_to_error::deserialize(&mut deserializer)
        .map_err(|error| format!("{} at {}", error, error.path()))
}

/// Builds an **identity** [`BlockCanon`] for a settings value by walking its
/// `surface_rule` tree and `default_block`, mapping each result state's partial
/// key to itself.
///
/// This exists so the composed generator ([`crate::overworld`]) can run without
/// a JVM: 26.2's real `BlockState.CODEC` canonicalisation is the identity on
/// every key the overworld surface rule emits (verified — every `canonmap.*`
/// line in the surface parity fixtures has `value == key`, because the result
/// states already carry their full property set). A version whose CODEC is
/// non-identity would supply its own table instead of calling this. The
/// per-stage `surface_parity` test still uses the JVM-dumped canon, so this
/// helper's identity assumption is never what a parity claim rests on.
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
    use super::{class_of_name, parse_surface_document, EvalCache, PreClass, SurfaceConditionNode,
        SurfaceRuleNode, WAY_BELOW_MIN_Y};
    use serde_json::json;

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
    fn surface_schema_accepts_real_rule_and_condition_shapes() {
        let rule: SurfaceRuleNode = parse_surface_document(&json!({
            "type": "minecraft:condition",
            "if_true": {"type": "minecraft:biome", "biome_is": ["minecraft:plains"]},
            "then_run": {"type": "minecraft:block", "result_state": {"Name": "minecraft:grass_block"}}
        }))
        .expect("document uses the real rule schema");
        assert!(matches!(rule, SurfaceRuleNode::Condition(_)));
        let condition: SurfaceConditionNode = parse_surface_document(&json!({
            "type": "minecraft:biome", "biome_is": "minecraft:plains"
        }))
        .expect("single biome string is a documented alternate shape");
        assert!(matches!(condition, SurfaceConditionNode::Biome(_)));
    }

    #[test]
    fn surface_schema_rejects_unknown_rule_fields_and_discriminators() {
        let unknown_field = parse_surface_document::<SurfaceRuleNode>(&json!({
            "type": "minecraft:block",
            "result_state": {"Name": "minecraft:stone"},
            "typo": true
        }))
        .expect_err("unknown rule fields must not be ignored");
        assert!(unknown_field.contains("typo"), "{unknown_field}");

        let unknown_type = parse_surface_document::<SurfaceRuleNode>(&json!({
            "type": "minecraft:not_a_rule"
        }))
        .expect_err("unknown rule discriminators must not fall through");
        assert!(unknown_type.contains("type"), "{unknown_type}");
    }
}
