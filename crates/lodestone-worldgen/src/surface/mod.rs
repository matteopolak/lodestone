//! Version-free interpreter for vanilla's `SurfaceRules` / `SurfaceSystem`.
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
//! aquifer-filled block field, exactly what vanilla's `NoiseChunk` +
//! `NoiseBasedChunkGenerator.doFill` produce) and the `WORLD_SURFACE_WG`
//! heightmap, and reproduces vanilla's `SurfaceSystem.buildSurface` scan
//! block-for-block. The pre-surface strings are taken as given (so the engine
//! needs no block registry): a rule only ever *replaces* a `defaultBlock`
//! (stone) with one of the surface rule's result states, whose canonical form
//! is supplied by the caller (version data, exactly like the block registry).
//!
//! # Parity discipline
//!
//! The oracle (`scripts/worldgen-oracle/SurfaceOracle.java`) drives vanilla's
//! *own* compiled `doFill` + `buildSurface` and dumps both columns; the test
//! compares block-for-block over the whole chunk and names the divergent
//! `x,y,z`. No Mojang source is transliterated — this is written from the
//! documented algorithm and checked against the running server (plan §11).

use std::collections::HashMap;

use serde_json::Value;

use crate::density::{Builder, Context as DfContext, Density};
use crate::math::{floor, lerp2, map, random_between_inclusive, round};
use crate::noise::NormalNoise;
use crate::rng::{PositionalRandomFactory, RandomSource, XoroshiroPositionalFactory};

/// The vanilla `Integer.MIN_VALUE` sentinel meaning "no water above".
const NO_WATER: i32 = i32::MIN;

/// Maps a result-state *partial key* (`name` + sorted specified `[k=v]`) to its
/// full canonical block string (all properties, defaults filled). Supplied by
/// the caller from the version's block data — see the oracle's `canonmap.*`
/// lines. Keeping this out of the engine preserves the version-free split.
pub type BlockCanon = HashMap<String, String>;

/// A parsed surface-rule condition (`SurfaceRules.ConditionSource` applied to
/// a context).
enum Cond {
    AbovePreliminarySurface,
    /// `biome` — issue #405 made this a per-column runtime check
    /// (`ctx.biome` membership) rather than a build-time constant, since a
    /// generator run no longer has one fixed biome for its whole life. The
    /// list is the rule's raw `biome_is` set, exactly as written in JSON.
    BiomeIs(Vec<String>),
    NoiseThreshold {
        noise: NormalNoise,
        min: f64,
        max: f64,
        is_3d: bool,
    },
    Not(Box<Cond>),
    Steep,
    StoneDepth {
        offset: i32,
        add_surface_depth: bool,
        secondary_depth_range: i32,
        ceiling: bool,
    },
    Temperature,
    Hole,
    VerticalGradient {
        factory: XoroshiroPositionalFactory,
        true_at_and_below: i32,
        false_at_and_above: i32,
    },
    Water {
        offset: i32,
        surface_depth_multiplier: i32,
        add_stone_depth: bool,
    },
    YAbove {
        anchor_y: i32,
        surface_depth_multiplier: i32,
        add_stone_depth: bool,
    },
}

/// A parsed surface rule (`SurfaceRules.RuleSource`).
enum Rule {
    /// Emits a fully-canonical block string.
    Block(String),
    /// First non-`None` child wins.
    Sequence(Vec<Rule>),
    /// Runs `then` only when `cond` holds.
    Condition(Cond, Box<Rule>),
    /// Badlands/eroded_badlands/wooded_badlands' banded-terracotta rule
    /// (`SurfaceRules.bandlands()` — `context.system::getBand`, issue #405's
    /// carried-over gap, closed here). Unconditional and parameterless in
    /// vanilla's own DSL (`SurfaceRules.Bandlands` is a zero-field enum
    /// singleton), so the [`BandBlocks`] payload is built once at parse time
    /// from the generator's own seed, not from anything in the JSON node —
    /// see [`RuleParser::bandlands`].
    Bandlands(Box<BandBlocks>),
}

/// `SurfaceSystem.getBand`'s own state: the 192-entry clay-band table plus
/// the noise that perturbs which entry a given `y` lands on.
///
/// Built once per world seed ([`RuleParser::bandlands`]), not per column or
/// per block — matching vanilla, where `SurfaceSystem.clayBands` is an
/// instance field generated once in the constructor
/// (`SurfaceSystem.generateBands`), never touched again after `new
/// SurfaceSystem(...)` returns.
struct BandBlocks {
    /// `SurfaceSystem.clayBands` — always exactly
    /// [`CLAY_BANDS_LEN`] entries long, each already the full canonical
    /// block string (these seven blocks carry no properties at 26.2 — see
    /// [`generate_bands`]'s doc comment — so no [`BlockCanon`] lookup is
    /// needed, unlike every other [`Rule::Block`] result state).
    clay_bands: Vec<String>,
    /// `SurfaceSystem.clayBandsOffsetNoise` (`minecraft:clay_bands_offset`).
    offset_noise: NormalNoise,
}

/// `SurfaceSystem.clayBands.length` — vanilla's own hardcoded table size
/// (`SurfaceSystem.generateBands`'s `new BlockState[192]`), not derived from
/// anything version-supplied.
const CLAY_BANDS_LEN: usize = 192;

impl BandBlocks {
    /// `SurfaceSystem.getBand(worldX, y, worldZ)`. Never returns `None` —
    /// vanilla's own `Bandlands` rule (`context.system::getBand`) is a bare
    /// `SurfaceRule` function reference with no condition wrapped around it,
    /// so every call that reaches [`Rule::Bandlands`] gets a real block back.
    fn get_band(&self, world_x: i32, y: i32, world_z: i32) -> String {
        let offset = round(
            self.offset_noise
                .get_value(f64::from(world_x), 0.0, f64::from(world_z))
                * 4.0,
        );
        let len = CLAY_BANDS_LEN as i32;
        // `y` ranges over this engine's own `min_y..min_y+gen_depth` (as low
        // as vanilla's `-64`) and `offset` is a noise sample scaled by 4, so
        // `y + offset + len` is always positive in practice — matching why
        // vanilla adds `clayBands.length` here at all (`SurfaceSystem.java`'s
        // own `% this.clayBands.length` line) rather than needing a true
        // Euclidean modulo.
        let index = (y + offset + len) % len;
        self.clay_bands[index as usize].clone()
    }
}

/// `SurfaceSystem.generateBands(RandomSource)` — the one-time table build.
/// `random` must be `noiseRandom.fromHashOf("minecraft:clay_bands")`
/// ([`RuleParser::bandlands`]), matching vanilla's own derivation exactly
/// (a *positional* factory's `from_hash_of`, not any per-block draw).
///
/// The seven result blocks (`minecraft:terracotta` and six
/// `minecraft:*_terracotta` dye variants) are hardcoded here rather than
/// routed through [`BlockCanon`]/[`canonical_from_block_json`] because
/// `SurfaceRules.bandlands()`'s JSON node carries no `result_state` at all
/// (it is `{"type": "minecraft:bandlands"}`, nothing else — vanilla's own
/// `SurfaceRules.Bandlands` enum has zero fields), so
/// [`identity_canon`](crate::surface::identity_canon)'s walk of the
/// `surface_rule` tree never sees these block names and has no key for them.
/// Confirmed property-less at 26.2 by `docs/worldgen-parity.md`'s own
/// measured oracle output, which names them bare (`orange_terracotta`, not
/// `orange_terracotta[...]`) in the pre-#295 badlands gap breakdown.
fn generate_bands<R: RandomSource>(random: &mut R) -> Vec<String> {
    let mut clay_bands = vec!["minecraft:terracotta".to_string(); CLAY_BANDS_LEN];

    // `for (int i = 0; i < clayBands.length; i++) { i += random.nextInt(5) + 1; ... }`
    // — the for-loop's own `i++` still fires every iteration *in addition to*
    // the body's `i +=`, so each step advances `i` by `nextInt(5) + 2`, not
    // `+ 1`. Translated as an explicit `while` with both increments spelled
    // out so that trap can't silently drop the `+ 1` a naive `for i in ...`
    // rewrite would.
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

/// `SurfaceSystem.makeBands` — scatters `bandCount` runs of `state`, each
/// `baseWidth..baseWidth+3` entries wide, at independently random starts.
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

/// Per-column / per-Y scan state mirroring `SurfaceRules.Context`.
struct Ctx {
    block_x: i32,
    block_z: i32,
    surface_depth: i32,
    surface_secondary: f64,
    min_surface_level: i32,
    block_y: i32,
    water_height: i32,
    stone_depth_above: i32,
    stone_depth_below: i32,
    /// This column's biome id (issue #405) — consulted by [`Cond::BiomeIs`].
    biome: String,
    /// This column's biome's `coldEnoughToSnow` answer — consulted by
    /// [`Cond::Temperature`]. See [`crate::biome::cold_enough_to_snow`].
    cold_enough_to_snow: bool,
}

/// The interpreter: instantiated noises + parsed rule tree, ready to build any
/// chunk's surface from its pre-surface column.
#[allow(missing_debug_implementations)]
pub struct SurfaceSystem {
    min_y: i32,
    gen_depth: i32,
    default_block: String,
    surface_noise: NormalNoise,
    surface_secondary_noise: NormalNoise,
    master: XoroshiroPositionalFactory,
    prelim: Density,
    rule: Rule,
}

impl SurfaceSystem {
    /// Builds the interpreter for `settings` (a `noise_settings` JSON value)
    /// using `builder` (already seeded with the same seed) to instantiate
    /// noises and derive random factories exactly as `RandomState` does.
    /// `canon` resolves result-state partial keys to full canonical strings.
    ///
    /// Unlike before issue #405, this takes **no biome** — a generator run no
    /// longer has one fixed biome for its whole life, so `biome`/
    /// `cold_enough_to_snow` moved from build-time constants here to
    /// per-column runtime inputs on [`build_surface`](Self::build_surface)/
    /// [`top_material`](Self::top_material) instead.
    #[must_use]
    pub fn new(settings: &Value, builder: &Builder, canon: &BlockCanon) -> Self {
        let min_y = settings["noise"]["min_y"].as_i64().unwrap_or(-64) as i32;
        let gen_depth = settings["noise"]["height"].as_i64().unwrap_or(384) as i32;
        let default_block = canonical_from_block_json(&settings["default_block"], canon);

        let surface_noise = builder.noise("minecraft:surface");
        let surface_secondary_noise = builder.noise("minecraft:surface_secondary");
        let master = builder.positional_factory();
        let prelim = builder.build(&settings["noise_router"]["preliminary_surface_level"]);

        let parser = RuleParser {
            builder,
            canon,
            min_y,
            gen_depth,
        };
        let rule = parser.rule(&settings["surface_rule"]);

        Self {
            min_y,
            gen_depth,
            default_block,
            surface_noise,
            surface_secondary_noise,
            master,
            prelim,
            rule,
        }
    }

    /// `SurfaceSystem.getSurfaceDepth(x, z)`.
    fn surface_depth(&self, x: i32, z: i32) -> i32 {
        let noise = self
            .surface_noise
            .get_value(f64::from(x), 0.0, f64::from(z));
        let extra = self.master.at(x, 0, z).next_double() * 0.25;
        (noise * 2.75 + 3.0 + extra) as i32
    }

    /// `SurfaceSystem.getSurfaceSecondary(x, z)`.
    fn surface_secondary(&self, x: i32, z: i32) -> f64 {
        self.surface_secondary_noise
            .get_value(f64::from(x), 0.0, f64::from(z))
    }

    /// `NoiseChunk.preliminarySurfaceLevel(sampleX, sampleZ)`.
    fn preliminary_surface_level(&self, sample_x: i32, sample_z: i32) -> i32 {
        // QuartPos.toBlock(QuartPos.fromBlock(v)) == (v >> 2) << 2.
        let qx = (sample_x >> 2) << 2;
        let qz = (sample_z >> 2) << 2;
        floor(self.prelim.compute(DfContext::new(qx, 0, qz)))
    }

    /// `SurfaceRules.Context.getMinSurfaceLevel()`.
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

    /// Reproduces `SurfaceSystem.buildSurface` for one 16×16 chunk.
    ///
    /// * `pre` yields the pre-surface (aquifer-filled) canonical block string at
    ///   local `(x, y, z)` (`x, z` in `0..16`, `y` a world Y). Out-of-range Y is
    ///   treated as air.
    /// * `heightmap` yields `WORLD_SURFACE_WG` at local `(x, z)`.
    /// * `biome_at` yields `(biome id, cold_enough_to_snow)` at local `(x, z)`
    ///   (issue #405) — called once per column, not per block, so a caller
    ///   whose biome varies at quart (not block) resolution can cheaply
    ///   return the same pair for every `(x, z)` in one 4×4 cell.
    /// * `min_block_x`/`min_block_z` are the chunk's world-space origin.
    ///
    /// Returns a **sparse diff**: local `(x, y, z)` -> canonical block string,
    /// present only where a surface rule actually rewrote the pre-surface
    /// block. A position absent from the map is unchanged, i.e. still exactly
    /// `pre(x, y, z)` — callers that need the full column reconstruct it from
    /// `pre` overlaid with this map, rather than the map alone.
    ///
    /// This used to be an exhaustive map (every one of a chunk's 16×16×`gen_depth`
    /// positions inserted up front from `pre`, then selectively overwritten by
    /// matched rules) so callers could treat the return value as the whole
    /// column. Profiling (`docs/benchmark-harness.md`) showed that exhaustive
    /// pre-fill — 98304 `String` clones and `HashMap` inserts per chunk for a
    /// gen_depth of 384, the overwhelming majority of them immediately
    /// discarded unread — was itself close to a fifth of total column-generation
    /// time (`SipHasher`/`RawTable::reserve_rehash`/`memmove` self-time). The
    /// scan below still needs `pre`/`block_at` for its own classification logic
    /// (unchanged); only the redundant up-front full-column copy is gone.
    #[must_use]
    pub fn build_surface(
        &self,
        pre: &dyn Fn(i32, i32, i32) -> String,
        heightmap: &dyn Fn(i32, i32) -> i32,
        biome_at: &dyn Fn(i32, i32) -> (String, bool),
        min_block_x: i32,
        min_block_z: i32,
    ) -> HashMap<(i32, i32, i32), String> {
        let y_lo = self.min_y;
        let y_hi = self.min_y + self.gen_depth; // exclusive
        let way_below_min_y = self.min_y << 4;

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

        let mut out: HashMap<(i32, i32, i32), String> = HashMap::new();

        // Immutable classification source: vanilla only ever reads the original
        // column while scanning (`old` is at the current, not-yet-written Y and
        // the ceiling look-ahead only reads lower, unvisited Y).
        let block_at = |x: i32, y: i32, z: i32| -> String {
            if y < y_lo || y >= y_hi {
                "minecraft:air".to_string()
            } else {
                pre(x, y, z)
            }
        };

        for x in 0..16 {
            for z in 0..16 {
                let block_x = min_block_x + x;
                let block_z = min_block_z + z;
                let surface_depth = self.surface_depth(block_x, block_z);
                let (biome, cold_enough_to_snow) = biome_at(x, z);
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
                    biome,
                    cold_enough_to_snow,
                };

                let height = heightmap(x, z) + 1;
                let mut stone_above_depth = 0;
                let mut water_height = NO_WATER;
                let mut next_ceiling_stone_y = i32::MAX;
                let end_y = y_lo;

                let mut y = height;
                while y >= end_y {
                    let old = block_at(x, y, z);
                    if is_air(&old) {
                        stone_above_depth = 0;
                        water_height = NO_WATER;
                    } else if is_fluid(&old) {
                        if water_height == NO_WATER {
                            water_height = y + 1;
                        }
                    } else {
                        if next_ceiling_stone_y >= y {
                            next_ceiling_stone_y = way_below_min_y;
                            let mut lookahead_y = y - 1;
                            while lookahead_y >= end_y - 1 {
                                if !is_stone(&block_at(x, lookahead_y, z)) {
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

                        if old == self.default_block {
                            if let Some(state) = self.try_apply(&self.rule, heightmap, &ctx) {
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
        let ctx = Ctx {
            block_x,
            block_z,
            surface_depth,
            surface_secondary: self.surface_secondary(block_x, block_z),
            min_surface_level: self.min_surface_level(block_x, block_z, surface_depth),
            block_y,
            water_height: if under_fluid { block_y + 1 } else { NO_WATER },
            stone_depth_above: 1,
            stone_depth_below: 1,
            biome: biome.to_string(),
            cold_enough_to_snow,
        };
        self.try_apply(&self.rule, heightmap, &ctx)
    }

    fn try_apply(
        &self,
        rule: &Rule,
        heightmap: &dyn Fn(i32, i32) -> i32,
        ctx: &Ctx,
    ) -> Option<String> {
        match rule {
            Rule::Block(state) => Some(state.clone()),
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

    fn test(&self, cond: &Cond, heightmap: &dyn Fn(i32, i32) -> i32, ctx: &Ctx) -> bool {
        match cond {
            Cond::BiomeIs(list) => list.iter().any(|b| b == &ctx.biome),
            Cond::AbovePreliminarySurface => ctx.block_y >= ctx.min_surface_level,
            Cond::NoiseThreshold {
                noise,
                min,
                max,
                is_3d,
            } => {
                let v = if *is_3d {
                    noise.get_value(
                        f64::from(ctx.block_x),
                        f64::from(ctx.block_y),
                        f64::from(ctx.block_z),
                    )
                } else {
                    noise.get_value(f64::from(ctx.block_x), 0.0, f64::from(ctx.block_z))
                };
                v >= *min && v <= *max
            }
            Cond::Not(inner) => !self.test(inner, heightmap, ctx),
            Cond::Steep => {
                let cbx = ctx.block_x & 15;
                let cbz = ctx.block_z & 15;
                let z_north = (cbz - 1).max(0);
                let z_south = (cbz + 1).min(15);
                let h_north = heightmap(cbx, z_north);
                let h_south = heightmap(cbx, z_south);
                if h_south >= h_north + 4 {
                    return true;
                }
                let x_west = (cbx - 1).max(0);
                let x_east = (cbx + 1).min(15);
                let h_west = heightmap(x_west, cbz);
                let h_east = heightmap(x_east, cbz);
                h_west >= h_east + 4
            }
            Cond::StoneDepth {
                offset,
                add_surface_depth,
                secondary_depth_range,
                ceiling,
            } => {
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
                stone_depth <= 1 + offset + surface_depth + secondary
            }
            Cond::Temperature => ctx.cold_enough_to_snow,
            Cond::Hole => ctx.surface_depth <= 0,
            Cond::VerticalGradient {
                factory,
                true_at_and_below,
                false_at_and_above,
            } => {
                let block_y = ctx.block_y;
                if block_y <= *true_at_and_below {
                    return true;
                }
                if block_y >= *false_at_and_above {
                    return false;
                }
                let probability = map(
                    f64::from(block_y),
                    f64::from(*true_at_and_below),
                    f64::from(*false_at_and_above),
                    1.0,
                    0.0,
                );
                let mut random = factory.at(ctx.block_x, block_y, ctx.block_z);
                f64::from(random.next_float()) < probability
            }
            Cond::Water {
                offset,
                surface_depth_multiplier,
                add_stone_depth,
            } => {
                ctx.water_height == NO_WATER
                    || ctx.block_y
                        + if *add_stone_depth {
                            ctx.stone_depth_above
                        } else {
                            0
                        }
                        >= ctx.water_height + offset + ctx.surface_depth * surface_depth_multiplier
            }
            Cond::YAbove {
                anchor_y,
                surface_depth_multiplier,
                add_stone_depth,
            } => {
                ctx.block_y
                    + if *add_stone_depth {
                        ctx.stone_depth_above
                    } else {
                        0
                    }
                    >= anchor_y + ctx.surface_depth * surface_depth_multiplier
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
    min_y: i32,
    gen_depth: i32,
}

impl RuleParser<'_, '_> {
    fn rule(&self, node: &Value) -> Rule {
        let ty = strip(node["type"].as_str().expect("rule type"));
        match ty {
            "block" => Rule::Block(canonical_from_block_json(&node["result_state"], self.canon)),
            "sequence" => Rule::Sequence(
                node["sequence"]
                    .as_array()
                    .expect("sequence")
                    .iter()
                    .map(|n| self.rule(n))
                    .collect(),
            ),
            "condition" => Rule::Condition(
                self.cond(&node["if_true"]),
                Box::new(self.rule(&node["then_run"])),
            ),
            "bandlands" => Rule::Bandlands(Box::new(self.bandlands())),
            other => panic!("unhandled surface rule type: minecraft:{other}"),
        }
    }

    /// Builds [`BandBlocks`] for a `"minecraft:bandlands"` rule node — once
    /// per occurrence of that node in the `surface_rule` tree at parse time
    /// (there is exactly one in vanilla's real `overworld.json`), matching
    /// `SurfaceSystem`'s constructor calling `generateBands` exactly once
    /// per world. `self.builder.positional_factory()` is the same `master`
    /// factory [`SurfaceSystem::new`] itself stores (`RandomState.random`,
    /// i.e. vanilla's `noiseRandom`) — see this module's own `master` field
    /// doc for why that identity holds.
    fn bandlands(&self) -> BandBlocks {
        let offset_noise = self.builder.noise("minecraft:clay_bands_offset");
        let mut random = self
            .builder
            .positional_factory()
            .from_hash_of("minecraft:clay_bands");
        let clay_bands = generate_bands(&mut random);
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
                Cond::BiomeIs(list)
            }
            "noise_threshold" => Cond::NoiseThreshold {
                noise: self
                    .builder
                    .noise(node["noise"].as_str().expect("noise id")),
                min: node["min_threshold"].as_f64().expect("min_threshold"),
                max: node["max_threshold"].as_f64().expect("max_threshold"),
                is_3d: node["is_3d"].as_bool().unwrap_or(false),
            },
            "not" => Cond::Not(Box::new(self.cond(&node["invert"]))),
            "steep" => Cond::Steep,
            "stone_depth" => Cond::StoneDepth {
                offset: node["offset"].as_i64().expect("offset") as i32,
                add_surface_depth: node["add_surface_depth"]
                    .as_bool()
                    .expect("add_surface_depth"),
                secondary_depth_range: node["secondary_depth_range"]
                    .as_i64()
                    .expect("secondary_depth_range") as i32,
                ceiling: node["surface_type"].as_str() == Some("ceiling"),
            },
            "temperature" => Cond::Temperature,
            "hole" => Cond::Hole,
            "vertical_gradient" => Cond::VerticalGradient {
                factory: self
                    .builder
                    .positional_factory()
                    .from_hash_of(node["random_name"].as_str().expect("random_name"))
                    .fork_positional(),
                true_at_and_below: self.resolve_anchor(&node["true_at_and_below"]),
                false_at_and_above: self.resolve_anchor(&node["false_at_and_above"]),
            },
            "water" => Cond::Water {
                offset: node["offset"].as_i64().expect("offset") as i32,
                surface_depth_multiplier: node["surface_depth_multiplier"]
                    .as_i64()
                    .expect("surface_depth_multiplier")
                    as i32,
                add_stone_depth: node["add_stone_depth"].as_bool().expect("add_stone_depth"),
            },
            "y_above" => Cond::YAbove {
                anchor_y: self.resolve_anchor(&node["anchor"]),
                surface_depth_multiplier: node["surface_depth_multiplier"]
                    .as_i64()
                    .expect("surface_depth_multiplier")
                    as i32,
                add_stone_depth: node["add_stone_depth"].as_bool().expect("add_stone_depth"),
            },
            other => panic!("unhandled surface condition type: minecraft:{other}"),
        }
    }

    /// `VerticalAnchor.resolveY(WorldGenerationContext)`.
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
    s == "minecraft:air"
}

fn is_fluid(s: &str) -> bool {
    let name = s.split('[').next().unwrap_or(s);
    name == "minecraft:water" || name == "minecraft:lava"
}

fn is_stone(s: &str) -> bool {
    !is_air(s) && !is_fluid(s)
}

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

/// Resolves a `{Name, Properties?}` block JSON to its full canonical string via
/// the caller-supplied [`BlockCanon`] table (produced by vanilla's own
/// `BlockState.CODEC`).
fn canonical_from_block_json(node: &Value, canon: &BlockCanon) -> String {
    let key = block_json_key(node);
    canon
        .get(&key)
        .cloned()
        .unwrap_or_else(|| panic!("no canonical block for result_state key {key:?}"))
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
