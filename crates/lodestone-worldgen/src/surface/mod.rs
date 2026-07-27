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
use crate::math::{floor, lerp2, map};
use crate::noise::NormalNoise;
use crate::rng::{PositionalRandomFactory, RandomSource, XoroshiroPositionalFactory};

/// The vanilla `Integer.MIN_VALUE` sentinel meaning "no water above".
const NO_WATER: i32 = i32::MIN;

/// Maps a result-state *partial key* (`name` + sorted specified `[k=v]`) to its
/// full canonical block string (all properties, defaults filled). Supplied by
/// the caller from the version's block data — see the oracle's `canonmap.*`
/// lines. Keeping this out of the engine preserves the version-free split.
pub type BlockCanon = HashMap<String, String>;

/// A parsed surface-rule condition (`SurfaceRules.ConditionSource` applied to a
/// context). `biome` conditions are resolved to a constant at build time
/// because the biome is fixed per column for a given run.
enum Cond {
    AbovePreliminarySurface,
    Const(bool),
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
    /// Badlands clay-band rule — only reachable inside badlands biomes, which
    /// this build does not exercise. Reaching it is a bug, so it panics loudly
    /// rather than silently mismatching.
    Bandlands,
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
    cold_enough_to_snow: bool,
}

impl SurfaceSystem {
    /// Builds the interpreter for `settings` (a `noise_settings` JSON value)
    /// using `builder` (already seeded with the same seed) to instantiate
    /// noises and derive random factories exactly as `RandomState` does.
    /// `biome` is the fixed biome id for this run; `canon` resolves result-state
    /// partial keys to full canonical strings. `cold_enough_to_snow` is the
    /// fixed biome's `coldEnoughToSnow` answer (only consulted by the
    /// `temperature` condition, which the default overworld path reaches for
    /// cold biomes only).
    #[must_use]
    pub fn new(
        settings: &Value,
        builder: &Builder,
        biome: &str,
        canon: &BlockCanon,
        cold_enough_to_snow: bool,
    ) -> Self {
        let min_y = settings["noise"]["min_y"].as_i64().unwrap_or(-64) as i32;
        let gen_depth = settings["noise"]["height"].as_i64().unwrap_or(384) as i32;
        let default_block = canonical_from_block_json(&settings["default_block"], canon);

        let surface_noise = builder.noise("minecraft:surface");
        let surface_secondary_noise = builder.noise("minecraft:surface_secondary");
        let master = builder.positional_factory();
        let prelim = builder.build(&settings["noise_router"]["preliminary_surface_level"]);

        let parser = RuleParser {
            builder,
            biome,
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
            cold_enough_to_snow,
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
    fn min_surface_level(&self, block_x: i32, block_z: i32, surface_depth: i32) -> i32 {
        let corner_cell_x = block_x >> 4;
        let corner_cell_z = block_z >> 4;
        let c0 = self.preliminary_surface_level(corner_cell_x << 4, corner_cell_z << 4);
        let c1 = self.preliminary_surface_level((corner_cell_x + 1) << 4, corner_cell_z << 4);
        let c2 = self.preliminary_surface_level(corner_cell_x << 4, (corner_cell_z + 1) << 4);
        let c3 = self.preliminary_surface_level((corner_cell_x + 1) << 4, (corner_cell_z + 1) << 4);
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
    /// * `min_block_x`/`min_block_z` are the chunk's world-space origin.
    ///
    /// Returns the post-surface column: a map from local `(x, y, z)` to
    /// canonical block string for every Y in `[min_y, min_y + gen_depth)`.
    #[must_use]
    pub fn build_surface(
        &self,
        pre: &dyn Fn(i32, i32, i32) -> String,
        heightmap: &dyn Fn(i32, i32) -> i32,
        min_block_x: i32,
        min_block_z: i32,
    ) -> HashMap<(i32, i32, i32), String> {
        let y_lo = self.min_y;
        let y_hi = self.min_y + self.gen_depth; // exclusive
        let way_below_min_y = self.min_y << 4;

        let mut out: HashMap<(i32, i32, i32), String> = HashMap::new();
        for x in 0..16 {
            for z in 0..16 {
                for y in y_lo..y_hi {
                    out.insert((x, y, z), pre(x, y, z));
                }
            }
        }

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
                let mut ctx = Ctx {
                    block_x,
                    block_z,
                    surface_depth,
                    surface_secondary: self.surface_secondary(block_x, block_z),
                    min_surface_level: self.min_surface_level(block_x, block_z, surface_depth),
                    block_y: 0,
                    water_height: NO_WATER,
                    stone_depth_above: 0,
                    stone_depth_below: 0,
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
    pub fn top_material(
        &self,
        block_x: i32,
        block_y: i32,
        block_z: i32,
        under_fluid: bool,
        heightmap: &dyn Fn(i32, i32) -> i32,
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
            Rule::Bandlands => {
                panic!("bandlands surface rule reached for a non-badlands biome — unsupported")
            }
        }
    }

    fn test(&self, cond: &Cond, heightmap: &dyn Fn(i32, i32) -> i32, ctx: &Ctx) -> bool {
        match cond {
            Cond::Const(b) => *b,
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
            Cond::Temperature => self.cold_enough_to_snow,
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
    biome: &'a str,
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
            "bandlands" => Rule::Bandlands,
            other => panic!("unhandled surface rule type: minecraft:{other}"),
        }
    }

    fn cond(&self, node: &Value) -> Cond {
        let ty = strip(node["type"].as_str().expect("condition type"));
        match ty {
            "above_preliminary_surface" => Cond::AbovePreliminarySurface,
            "biome" => {
                let matches = node["biome_is"]
                    .as_array()
                    .map(|a| a.iter().any(|b| b.as_str() == Some(self.biome)))
                    .unwrap_or_else(|| node["biome_is"].as_str() == Some(self.biome));
                Cond::Const(matches)
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

/// Resolves a `{Name, Properties?}` block JSON to its full canonical string via
/// the caller-supplied [`BlockCanon`] table (produced by vanilla's own
/// `BlockState.CODEC`).
fn canonical_from_block_json(node: &Value, canon: &BlockCanon) -> String {
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
    canon
        .get(&key)
        .cloned()
        .unwrap_or_else(|| panic!("no canonical block for result_state key {key:?}"))
}
