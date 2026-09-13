//! Large dripstone configured-feature placement.
//!
//! A large dripstone feature finds one empty or water-filled cave column,
//! chooses a radius from the column height, and grows a tapered stalactite and
//! stalagmite from the two stone edges.  The body is kept separate from the
//! smaller speleothem cluster so its paired-cone geometry, cave scan, and
//! optional wind offset remain independently testable.
//!
//! # What it is
//!
//! This module interprets the `large_dripstone` configured-feature record and
//! writes `minecraft:dripstone_block` into the live vegetation grid.
//!
//! # How it works
//!
//! The candidate column must be empty or water at the origin, have a valid
//! replaceable/edge block above and below, and be at least four blocks high.
//! The configured integer provider supplies the radius range; the cave height
//! caps that range before one inclusive radius draw.  Each cone samples its
//! bluntness and height scale, backs its root into the surrounding stone, and
//! then fills the tapered cross-section.  Wide, blunt cones also get a
//! height-dependent horizontal wind offset.
//!
//! # How to change it
//!
//! Preserve the order of the radius, cone-provider, wind, and per-column
//! shortening draws.  The external fixture below deliberately uses constant
//! providers so geometry and the two placement loops can be checked without a
//! second implementation of the random stream.  If a new provider shape is
//! accepted, add a fixture that exercises its draw count before changing the
//! parser.
//!
//! # Configuration
//!
//! `column_radius` is an integer-provider range, `height_scale`, the two
//! bluntness fields, and `wind_speed` are constant or uniform float providers,
//! and `replaceable_blocks` is resolved through the block-tag resolver.
//! `floor_to_ceiling_search_range` defaults to 30 and is bounded to 1..=512;
//! the remaining scalar limits follow the configured feature's documented
//! ranges.
//!
//! # Dependencies
//!
//! [`VegGrid`] provides live block and heightmap reads plus bounded writes;
//! [`IntProvider`] and [`RandomSource`] provide the configured radius and
//! random stream; [`Resolver`] expands the replacement tag.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

use crate::density::Resolver;
use crate::feature::{BlockPos, IntProvider};
use crate::math;
use crate::rng::RandomSource;

use super::base_id;
use super::config::{is_air, is_fluid, resolve_block_set, try_parse_int_provider};
use super::grid::VegGrid;

const DRIPSTONE_BLOCK: &str = "minecraft:dripstone_block";

/// The float-provider subset used by the bundled large-dripstone record.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum FloatRange {
    Constant(f32),
    Uniform { min: f32, max: f32 },
}

impl FloatRange {
    fn try_parse(value: &Value) -> Option<Self> {
        match serde_json::from_value::<FloatRangeDocument>(value.clone()).ok()? {
            FloatRangeDocument::Number(value) => finite_f32(f64::from(value)).map(Self::Constant),
            FloatRangeDocument::Provider(provider) => match provider.kind {
                FloatProviderKind::Constant => {
                    finite_f32(f64::from(provider.value?)).map(Self::Constant)
                }
                FloatProviderKind::Uniform => {
                    let min = finite_f32(f64::from(provider.min_inclusive?))?;
                    let max = finite_f32(f64::from(provider.max_exclusive?))?;
                    (min <= max).then_some(Self::Uniform { min, max })
                }
            },
        }
    }

    fn sample<R: RandomSource>(self, random: &mut R) -> f32 {
        match self {
            Self::Constant(value) => value,
            Self::Uniform { min, max } => math::random_between(random, min, max),
        }
    }

    fn within(self, min: f32, max: f32) -> bool {
        match self {
            Self::Constant(value) => (min..=max).contains(&value),
            Self::Uniform { min: low, max: high } => {
                (min..=max).contains(&low) && (min..=max).contains(&high)
            }
        }
    }
}

/// The closed discriminator set accepted by the bundled float providers.
///
/// Serde owns the JSON boundary here so an unknown provider cannot silently
/// become a constant or fall through to a string-based compatibility branch.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
enum FloatProviderKind {
    #[serde(rename = "minecraft:constant", alias = "constant")]
    Constant,
    #[serde(rename = "minecraft:uniform", alias = "uniform")]
    Uniform,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TypedFloatProvider {
    #[serde(rename = "type")]
    kind: FloatProviderKind,
    value: Option<f32>,
    min_inclusive: Option<f32>,
    max_exclusive: Option<f32>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(untagged)]
enum FloatRangeDocument {
    Number(f32),
    Provider(TypedFloatProvider),
}

fn finite_f32(value: f64) -> Option<f32> {
    let value = value as f32;
    value.is_finite().then_some(value)
}

/// Parsed parameters for one large-dripstone configured feature.
#[derive(Clone, Debug)]
pub struct LargeDripstoneCfg {
    pub(super) replaceable_blocks: HashSet<String>,
    pub(super) floor_to_ceiling_search_range: i32,
    pub(super) column_radius: IntProvider,
    pub(super) height_scale: FloatRange,
    pub(super) max_column_radius_to_cave_height_ratio: f32,
    pub(super) stalactite_bluntness: FloatRange,
    pub(super) stalagmite_bluntness: FloatRange,
    pub(super) wind_speed: FloatRange,
    pub(super) min_radius_for_wind: i32,
    pub(super) min_bluntness_for_wind: f32,
}

impl LargeDripstoneCfg {
    /// Parses the configured record without turning malformed datapack data
    /// into a panic or a permissive feature.
    pub(super) fn try_parse(resolver: &dyn Resolver, config: &Value) -> Option<Self> {
        let replaceable_blocks = resolve_block_set(resolver, config.get("replaceable_blocks")?)?;
        if replaceable_blocks.is_empty() {
            return None;
        }

        let floor_to_ceiling_search_range = match config.get("floor_to_ceiling_search_range") {
            Some(value) => value
                .as_i64()
                .filter(|value| (1..=512).contains(value))? as i32,
            None => 30,
        };
        let column_radius = try_parse_int_provider(config.get("column_radius")?)?;
        let (radius_min, radius_max) = provider_bounds(&column_radius)?;
        if !(1..=60).contains(&radius_min)
            || !(1..=60).contains(&radius_max)
            || radius_min > radius_max
        {
            return None;
        }

        let height_scale = FloatRange::try_parse(config.get("height_scale")?)?;
        if !height_scale.within(0.0, 20.0) {
            return None;
        }
        let max_column_radius_to_cave_height_ratio = bounded_f32(
            config.get("max_column_radius_to_cave_height_ratio")?,
            0.0..=1.0,
        )?;
        let stalactite_bluntness = FloatRange::try_parse(config.get("stalactite_bluntness")?)?;
        let stalagmite_bluntness = FloatRange::try_parse(config.get("stalagmite_bluntness")?)?;
        let wind_speed = FloatRange::try_parse(config.get("wind_speed")?)?;
        if !stalactite_bluntness.within(0.1, 10.0)
            || !stalagmite_bluntness.within(0.1, 10.0)
            || !wind_speed.within(0.0, 2.0)
        {
            return None;
        }
        let min_radius_for_wind = config
            .get("min_radius_for_wind")?
            .as_i64()
            .filter(|value| (0..=100).contains(value))? as i32;
        let min_bluntness_for_wind = bounded_f32(
            config.get("min_bluntness_for_wind")?,
            0.0..=5.0,
        )?;

        Some(Self {
            replaceable_blocks,
            floor_to_ceiling_search_range,
            column_radius,
            height_scale,
            max_column_radius_to_cave_height_ratio,
            stalactite_bluntness,
            stalagmite_bluntness,
            wind_speed,
            min_radius_for_wind,
            min_bluntness_for_wind,
        })
    }
}

fn bounded_f32(value: &Value, bounds: std::ops::RangeInclusive<f64>) -> Option<f32> {
    let value = value.as_f64()?;
    (value.is_finite() && bounds.contains(&value)).then_some(value as f32)
}

/// Returns the complete support range of an integer provider.  The large
/// dripstone radius uses the provider's support, rather than sampling it as a
/// second random value.
fn provider_bounds(provider: &IntProvider) -> Option<(i32, i32)> {
    let bounds = match provider {
        IntProvider::Constant(value) => (*value, *value),
        IntProvider::Uniform { min, max }
        | IntProvider::BiasedToBottom { min, max }
        | IntProvider::Trapezoid { min, max, .. }
        | IntProvider::ClampedNormal { min, max, .. } => (*min, *max),
        IntProvider::Clamped { source, min, max } => {
            if min > max {
                return None;
            }
            let (source_min, source_max) = provider_bounds(source)?;
            (source_min.clamp(*min, *max), source_max.clamp(*min, *max))
        }
        IntProvider::WeightedList(entries) => entries
            .iter()
            .map(|(value, _)| *value)
            .min()
            .zip(entries.iter().map(|(value, _)| *value).max())?,
        IntProvider::WeightedProviders(entries) => entries
            .iter()
            .filter_map(|(provider, _)| provider_bounds(provider))
            .fold(None::<(i32, i32)>, |bounds, (min, max)| {
                Some(match bounds {
                    Some((current_min, current_max)) => {
                        (current_min.min(min), current_max.max(max))
                    }
                    None => (min, max),
                })
            })?,
    };
    (bounds.0 <= bounds.1).then_some(bounds)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CaveColumn {
    floor: i32,
    ceiling: i32,
}

impl CaveColumn {
    fn height(self) -> i32 {
        self.ceiling - self.floor - 1
    }
}

fn empty_or_water(grid: &VegGrid, pos: BlockPos) -> bool {
    let base = base_id(grid.get(pos.x, pos.y, pos.z));
    is_air(base) || base == "minecraft:water"
}

fn empty_or_water_or_lava(grid: &VegGrid, pos: BlockPos) -> bool {
    let base = base_id(grid.get(pos.x, pos.y, pos.z));
    is_air(base) || is_fluid(base)
}

fn is_lava(grid: &VegGrid, pos: BlockPos) -> bool {
    base_id(grid.get(pos.x, pos.y, pos.z)) == "minecraft:lava"
}

fn valid_edge(grid: &VegGrid, config: &LargeDripstoneCfg, pos: BlockPos) -> bool {
    let base = base_id(grid.get(pos.x, pos.y, pos.z));
    base == DRIPSTONE_BLOCK || base == "minecraft:lava" || config.replaceable_blocks.contains(base)
}

fn scan_column(
    grid: &VegGrid,
    config: &LargeDripstoneCfg,
    origin: BlockPos,
) -> Option<CaveColumn> {
    if !empty_or_water(grid, origin) {
        return None;
    }

    let scan = |dy: i32| {
        let mut y = origin.y;
        for _ in 1..config.floor_to_ceiling_search_range {
            if !empty_or_water(
                grid,
                BlockPos {
                    y,
                    ..origin
                },
            ) {
                break;
            }
            y += dy;
        }
        valid_edge(
            grid,
            config,
            BlockPos {
                y,
                ..origin
            },
        )
        .then_some(y)
    };

    Some(CaveColumn {
        floor: scan(-1)?,
        ceiling: scan(1)?,
    })
}

#[derive(Clone, Copy, Debug)]
struct WindOffset {
    origin_y: i32,
    x: f32,
    z: f32,
    max_offset: i32,
}

impl WindOffset {
    fn none() -> Self {
        Self {
            origin_y: 0,
            x: 0.0,
            z: 0.0,
            max_offset: 0,
        }
    }

    fn at(self, pos: BlockPos) -> BlockPos {
        let dy = (self.origin_y - pos.y) as f32;
        let dx = math::floor(f64::from(self.x * dy)).clamp(-self.max_offset, self.max_offset);
        let dz = math::floor(f64::from(self.z * dy)).clamp(-self.max_offset, self.max_offset);
        BlockPos {
            x: pos.x + dx,
            y: pos.y,
            z: pos.z + dz,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Dripstone {
    root: BlockPos,
    pointing_up: bool,
    radius: i32,
    bluntness: f32,
    height_scale: f32,
}

impl Dripstone {
    fn height_at(self, distance: f32) -> i32 {
        get_speleothem_height(distance, self.radius, self.height_scale, self.bluntness)
    }

    fn move_back_into_stone(
        &mut self,
        grid: &VegGrid,
        wind: WindOffset,
    ) -> bool {
        let mut radius = self.radius;
        while radius > 1 {
            let mut root = self.root;
            let max_tries = self.height_at(0.0).min(10);
            for _ in 0..max_tries {
                if is_lava(grid, root) {
                    return false;
                }
                let probe = wind.at(root);
                if circle_mostly_embedded(grid, probe, radius) {
                    self.root = root;
                    self.radius = radius;
                    return true;
                }
                root.y += if self.pointing_up { -1 } else { 1 };
            }
            radius /= 2;
        }
        false
    }

    fn place<R: RandomSource>(
        self,
        random: &mut R,
        grid: &mut VegGrid,
        wind: WindOffset,
        replaceable_blocks: &HashSet<String>,
    ) {
        let state_id = grid.interner().id_of(DRIPSTONE_BLOCK);
        for dx in -self.radius..=self.radius {
            for dz in -self.radius..=self.radius {
                let distance = ((dx * dx + dz * dz) as f32).sqrt();
                if distance > self.radius as f32 {
                    continue;
                }
                let height = self.height_at(distance);
                if height <= 0 {
                    continue;
                }
                let height = if random.next_float() < 0.2 {
                    (height as f32 * math::random_between(random, 0.8, 1.0)) as i32
                } else {
                    height
                };
                let max_y = if self.pointing_up {
                    grid.height_world_surface(self.root.x + dx, self.root.z + dz)
                } else {
                    i32::MAX
                };
                let mut pos = BlockPos {
                    x: self.root.x + dx,
                    y: self.root.y,
                    z: self.root.z + dz,
                };
                let mut has_been_out = false;
                for _ in 0..height {
                    if pos.y >= max_y {
                        break;
                    }
                    let shifted = wind.at(pos);
                    let base = base_id(grid.get(shifted.x, shifted.y, shifted.z));
                    if empty_or_water_or_lava(grid, shifted) {
                        has_been_out = true;
                        let _ = grid.set_id_if_in_bounds(
                            shifted.x,
                            shifted.y,
                            shifted.z,
                            state_id,
                        );
                    } else if has_been_out && replaceable_blocks.contains(base) {
                        break;
                    }
                    pos.y += if self.pointing_up { 1 } else { -1 };
                }
            }
        }
    }
}

fn circle_mostly_embedded(grid: &VegGrid, center: BlockPos, radius: i32) -> bool {
    if empty_or_water_or_lava(grid, center) {
        return false;
    }
    let step = 6.0 / radius as f64;
    let mut angle = 0.0;
    while angle < std::f64::consts::TAU {
        let dx = (math::cos(angle) * radius as f32) as i32;
        let dz = (math::sin(angle) * radius as f32) as i32;
        if empty_or_water_or_lava(
            grid,
            BlockPos {
                x: center.x + dx,
                y: center.y,
                z: center.z + dz,
            },
        ) {
            return false;
        }
        angle += step;
    }
    true
}

fn get_speleothem_height(distance: f32, radius: i32, height_scale: f32, bluntness: f32) -> i32 {
    let distance = f64::from(distance.max(bluntness));
    let scaled = distance / f64::from(radius) * 0.384;
    let height_relative = f64::from(height_scale)
        * (0.75 * scaled.powf(1.333_333_333_333_333_3)
            - scaled.powf(0.666_666_666_666_666_6)
            - scaled.ln() / 3.0);
    (height_relative.max(0.0) / 0.384 * f64::from(radius)) as i32
}

/// Places one paired large-dripstone cone and reports whether its cave-column
/// gate admitted the candidate.
pub(super) fn place_large_dripstone<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    config: &LargeDripstoneCfg,
    grid: &mut VegGrid,
) -> bool {
    let Some(column) = scan_column(grid, config, origin) else {
        return false;
    };
    if column.height() < 4 {
        return false;
    }
    let (radius_min, radius_max) = provider_bounds(&config.column_radius)
        .expect("validated large-dripstone radius provider");
    let max_radius = ((column.height() as f32 * config.max_column_radius_to_cave_height_ratio)
        as i32)
        .clamp(radius_min, radius_max);
    let radius = radius_min + random.next_int_bounded(max_radius - radius_min + 1);

    let mut stalactite = Dripstone {
        root: BlockPos {
            y: column.ceiling - 1,
            ..origin
        },
        pointing_up: false,
        radius,
        bluntness: config.stalactite_bluntness.sample(random),
        height_scale: config.height_scale.sample(random),
    };
    let mut stalagmite = Dripstone {
        root: BlockPos {
            y: column.floor + 1,
            ..origin
        },
        pointing_up: true,
        radius,
        bluntness: config.stalagmite_bluntness.sample(random),
        height_scale: config.height_scale.sample(random),
    };

    let wind = if radius >= config.min_radius_for_wind
        && stalactite.bluntness >= config.min_bluntness_for_wind
        && stalagmite.bluntness >= config.min_bluntness_for_wind
    {
        let speed = config.wind_speed.sample(random);
        let direction = random.next_float() as f64 * std::f64::consts::PI;
        WindOffset {
            origin_y: origin.y,
            x: math::cos(direction) * speed,
            z: math::sin(direction) * speed,
            max_offset: (16 - radius).max(0),
        }
    } else {
        WindOffset::none()
    };

    let stalactite_ready = stalactite.move_back_into_stone(
        grid,
        wind,
    );
    let stalagmite_ready = stalagmite.move_back_into_stone(
        grid,
        wind,
    );
    if stalactite_ready {
        stalactite.place(random, grid, wind, &config.replaceable_blocks);
    }
    if stalagmite_ready {
        stalagmite.place(random, grid, wind, &config.replaceable_blocks);
    }
    stalactite_ready || stalagmite_ready
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::compose::build_decoration_catalog;
    use crate::density::NoiseParams;
    use crate::feature::vegetation::{place_configured_feature, ConfiguredFeature, VegTags};
    use crate::rng::XoroshiroPositionalFactory;

    const EXTERNAL: &str = include_str!("../../../tests/support/large_dripstone_feature_external.txt");

    #[derive(Debug)]
    struct Expected {
        origin: BlockPos,
        states: HashMap<(i32, i32, i32), String>,
    }

    fn fixture() -> Expected {
        let mut origin = None;
        let mut states = HashMap::new();
        for line in EXTERNAL.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (kind, rest) = line.split_once(' ').expect("fixture record");
            if kind == "origin" {
                let mut coords = rest.split(',');
                origin = Some(BlockPos {
                    x: coords.next().expect("origin x").parse().expect("origin x int"),
                    y: coords.next().expect("origin y").parse().expect("origin y int"),
                    z: coords.next().expect("origin z").parse().expect("origin z int"),
                });
            } else if let Some(coords) = kind.strip_prefix("state.") {
                let mut pos = coords.split(',');
                let key = (
                    pos.next().expect("state x").parse().expect("state x int"),
                    pos.next().expect("state y").parse().expect("state y int"),
                    pos.next().expect("state z").parse().expect("state z int"),
                );
                states.insert(key, rest.to_owned());
            }
        }
        Expected {
            origin: origin.expect("fixture origin"),
            states,
        }
    }

    fn cfg() -> LargeDripstoneCfg {
        LargeDripstoneCfg {
            replaceable_blocks: HashSet::from(["minecraft:stone".to_owned()]),
            floor_to_ceiling_search_range: 30,
            column_radius: IntProvider::Constant(3),
            height_scale: FloatRange::Constant(1.0),
            max_column_radius_to_cave_height_ratio: 1.0,
            stalactite_bluntness: FloatRange::Constant(0.4),
            stalagmite_bluntness: FloatRange::Constant(0.4),
            wind_speed: FloatRange::Constant(0.0),
            min_radius_for_wind: 3,
            min_bluntness_for_wind: 0.6,
        }
    }

    #[test]
    fn float_provider_json_uses_closed_typed_discriminator() {
        let document = serde_json::json!({
            "type": "minecraft:uniform",
            "min_inclusive": 0.25,
            "max_exclusive": 0.75,
        });
        let parsed = FloatRange::try_parse(&document).expect("uniform provider parses");
        assert_eq!(parsed, FloatRange::Uniform { min: 0.25, max: 0.75 });

        let unknown = serde_json::json!({
            "type": "minecraft:triangular",
            "min_inclusive": 0.25,
            "max_exclusive": 0.75,
        });
        assert!(serde_json::from_value::<FloatRangeDocument>(unknown.clone()).is_err());
        assert!(FloatRange::try_parse(&unknown).is_none());

        let extra_field = serde_json::json!({
            "type": "minecraft:uniform",
            "min_inclusive": 0.25,
            "max_exclusive": 0.75,
            "unexpected": true,
        });
        assert!(serde_json::from_value::<FloatRangeDocument>(extra_field).is_err());
    }

    fn cave_grid(origin: BlockPos) -> VegGrid {
        let mut grid = VegGrid::new(-16, 64, 0, 0);
        for x in 0..16 {
            for z in 0..16 {
                for y in -16..48 {
                    grid.seed(x, y, z, "minecraft:stone".to_owned());
                }
            }
        }
        for x in 5..=11 {
            for z in 5..=11 {
                for y in 0..=10 {
                    grid.seed(x, y, z, "minecraft:air".to_owned());
                }
            }
        }
        assert_eq!(origin, BlockPos { x: 8, y: 5, z: 8 });
        grid
    }

    struct ScriptedRandom {
        ints: Vec<i32>,
        next_int: usize,
        floats: usize,
    }

    impl ScriptedRandom {
        fn new(ints: &[i32]) -> Self {
            Self {
                ints: ints.to_vec(),
                next_int: 0,
                floats: 0,
            }
        }
    }

    impl RandomSource for ScriptedRandom {
        type Positional = XoroshiroPositionalFactory;

        fn fork_positional(&mut self) -> Self::Positional {
            panic!("the large-dripstone fixture does not fork randomness")
        }

        fn set_seed(&mut self, _seed: i64) {
            panic!("the large-dripstone fixture does not reseed randomness")
        }

        fn next_bits(&mut self, _bits: u32) -> i32 {
            panic!("the large-dripstone fixture does not request raw bits")
        }

        fn next_int(&mut self) -> i32 {
            panic!("the large-dripstone fixture does not request an unbounded integer")
        }

        fn next_int_bounded(&mut self, bound: i32) -> i32 {
            let value = *self.ints.get(self.next_int).expect("fixture random draw");
            self.next_int += 1;
            assert!((0..bound).contains(&value), "scripted draw {value} outside 0..{bound}");
            value
        }

        fn next_long(&mut self) -> i64 {
            panic!("the large-dripstone fixture does not request a long")
        }

        fn next_bool(&mut self) -> bool {
            panic!("the large-dripstone fixture does not request a boolean")
        }

        fn next_float(&mut self) -> f32 {
            self.floats += 1;
            0.99
        }

        fn next_double(&mut self) -> f64 {
            panic!("the large-dripstone fixture does not request a double")
        }

        fn next_gaussian(&mut self) -> f64 {
            panic!("the large-dripstone fixture does not request a gaussian")
        }

        fn consume_count(&mut self, _rounds: u32) {
            panic!("the large-dripstone fixture does not consume random rounds")
        }
    }

    #[test]
    fn external_fixture_captures_paired_tapered_cones() {
        let expected = fixture();
        let mut grid = cave_grid(expected.origin);
        let mut random = ScriptedRandom::new(&[0]);
        assert!(place_large_dripstone(&mut random, expected.origin, &cfg(), &mut grid));
        assert_eq!(random.next_int, 1, "the radius range consumes one draw");
        assert_eq!(random.floats, 26, "each admitted cone column draws one shortening float");

        let got: HashMap<_, _> = expected
            .states
            .keys()
            .map(|&pos| (pos, grid.get(pos.0, pos.1, pos.2).to_owned()))
            .collect();
        assert_eq!(got, expected.states);
        let written: HashSet<_> = grid
            .dirty_cells()
            .map(|(x, y, z, _)| (x, y, z))
            .collect();
        assert_eq!(written, expected.states.keys().copied().collect());
    }

    #[test]
    fn short_cave_is_rejected_before_random_draws() {
        let origin = BlockPos { x: 8, y: 1, z: 8 };
        let mut grid = cave_grid(BlockPos { x: 8, y: 5, z: 8 });
        for y in 0..=10 {
            grid.seed(origin.x, y, origin.z, "minecraft:stone".to_owned());
        }
        for y in 0..=2 {
            grid.seed(origin.x, y, origin.z, "minecraft:air".to_owned());
        }
        let mut random = ScriptedRandom::new(&[]);
        assert!(!place_large_dripstone(&mut random, origin, &cfg(), &mut grid));
        assert_eq!(random.next_int, 0);
        assert_eq!(random.floats, 0);
        assert_eq!(grid.dirty_len(), 0);
    }

    #[test]
    fn configured_dispatch_reaches_large_dripstone_body() {
        let expected = fixture();
        let mut grid = cave_grid(expected.origin);
        let mut random = ScriptedRandom::new(&[0]);
        let feature = ConfiguredFeature::LargeDripstone(Box::new(cfg()));
        place_configured_feature(
            &mut random,
            expected.origin,
            &feature,
            &mut grid,
            &VegTags::default(),
        );
        assert_eq!(random.next_int, 1);
        assert_eq!(random.floats, 26);
        for (&(x, y, z), state) in &expected.states {
            assert_eq!(grid.get(x, y, z), state);
        }
    }

    struct AssetResolver {
        root: PathBuf,
    }

    impl AssetResolver {
        fn json(&self, kind: &str, id: &str) -> Value {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            let path = self
                .root
                .join("worldgen")
                .join(kind)
                .join(format!("{name}.json"));
            serde_json::from_str(
                &std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("reading {}: {error}", path.display())),
            )
            .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
        }
    }

    impl Resolver for AssetResolver {
        fn density_function(&self, _id: &str) -> Value {
            Value::Null
        }

        fn noise(&self, _id: &str) -> NoiseParams {
            unreachable!("the large-dripstone configuration does not use noise")
        }

        fn block_tag(&self, id: &str) -> Value {
            self.json("tags/block", id)
        }

        fn biome_document(&self, id: &str) -> Value {
            self.json("biome", id)
        }

        fn configured_feature(&self, id: &str) -> Value {
            self.json("configured_feature", id)
        }

        fn placed_feature(&self, id: &str) -> Value {
            self.json("placed_feature", id)
        }
    }

    #[test]
    fn bundled_config_and_catalog_reach_large_dripstone() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets");
        let resolver = AssetResolver { root };
        let document = resolver.json("configured_feature", "minecraft:large_dripstone");
        let parsed = super::super::config::parse_configured_feature_doc(&resolver, &document);
        let ConfiguredFeature::LargeDripstone(config) = parsed else {
            panic!("large_dripstone must parse as the dedicated feature body");
        };
        assert_eq!(config.floor_to_ceiling_search_range, 30);
        assert_eq!(provider_bounds(&config.column_radius), Some((3, 16)));
        assert!(config.replaceable_blocks.contains("minecraft:stone"));

        let catalog = build_decoration_catalog(&resolver, &["minecraft:dripstone_caves".to_owned()]);
        let selected = catalog
            .select(["minecraft:dripstone_caves"])
            .into_iter()
            .find(|(_, _, placed)| placed.registry_id.as_deref() == Some("minecraft:large_dripstone"))
            .expect("dripstone_caves must select its placed feature");
        assert!(matches!(
            selected.2.feature.as_ref(),
            ConfiguredFeature::LargeDripstone(_)
        ));
    }
}
