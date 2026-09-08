//! Packed-ice spike configured-feature placement.
//!
//! This body grows a tapered column from a snow-block support, mirrors the
//! lower half when the width allows it, and fills a narrow support pillar
//! toward the build floor.  The parser keeps the two configured predicates
//! as resolved block sets so the hot placement loop has no registry lookups.
//!
//! # How it works
//!
//! The candidate settles downward through air until it reaches a support or
//! the lower build bound.  Four bounded random draws choose the vertical
//! offset, height, width, and rare elevated form.  Each tapered layer tests a
//! circular footprint; edge cells consume a float before they are admitted.
//! Existing air and configured replacement blocks receive the packed-ice
//! state, while protected terrain ends the downward support pillar.
//!
//! # How to change it
//!
//! Keep `try_parse` strict about predicate shapes: treating an unknown
//! support or replacement rule as permissive would fabricate spikes in
//! ordinary terrain.  Changes to draw order belong in the fixture-backed
//! tests below, because every conditional boundary draw affects later
//! decoration features in the same source stream.
//!
//! # Configuration
//!
//! The configured document supplies `can_place_on` as a matching-blocks
//! predicate, `can_replace` as a matching-block-tag predicate, and a block
//! state under `state`.  The bundled Overworld record supports on
//! `minecraft:snow_block`, replaces the resolved `ice_spike_replaceable`
//! closure, and writes `minecraft:packed_ice`.
//!
//! # Dependencies
//!
//! [`Resolver`] resolves the replacement tag, [`RandomSource`] supplies the
//! feature stream, and [`VegGrid`] provides live heightmap reads plus the
//! bounded write surface used by every decoration body.

use std::collections::HashSet;

use serde_json::Value;

use crate::density::Resolver;
use crate::feature::BlockPos;
use crate::rng::RandomSource;

use super::base_id;
use super::config::{canon_state, is_air, parse_id_list, resolve_block_set};
use super::grid::VegGrid;

/// Parsed support, replacement, and output state for one packed-ice spike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IceSpikeCfg {
    pub(super) state: String,
    pub(super) can_place_on: HashSet<String>,
    pub(super) can_replace: HashSet<String>,
}

impl IceSpikeCfg {
    /// Parses the predicate forms used by the bundled spike record.
    pub(super) fn try_parse(resolver: &dyn Resolver, config: &Value) -> Option<Self> {
        let can_place_on = config.get("can_place_on")?;
        let support_type = can_place_on.get("type")?.as_str()?;
        if support_type.strip_prefix("minecraft:").unwrap_or(support_type) != "matching_blocks" {
            return None;
        }
        let can_place_on: HashSet<String> = parse_id_list(can_place_on.get("blocks")?)
            .into_iter()
            .map(|state| base_id(&state).to_owned())
            .collect();
        if can_place_on.is_empty() {
            return None;
        }

        let can_replace = config.get("can_replace")?;
        let replacement_type = can_replace.get("type")?.as_str()?;
        if replacement_type
            .strip_prefix("minecraft:")
            .unwrap_or(replacement_type)
            != "matching_block_tag"
        {
            return None;
        }
        let tag = can_replace.get("tag")?.as_str()?;
        let can_replace = resolve_block_set(resolver, &Value::String(format!("#{tag}")))?;
        if can_replace.is_empty() {
            return None;
        }

        let state = config.get("state")?;
        state.get("Name").and_then(Value::as_str)?;

        Some(Self {
            state: canon_state(state),
            can_place_on,
            can_replace,
        })
    }
}

/// Places one packed-ice spike and reports whether its support gate admitted
/// the candidate.  The body intentionally retains every conditional draw in
/// the source algorithm's order, including edge-cell floats and pillar gaps.
pub(super) fn place_ice_spike<R: RandomSource>(
    random: &mut R,
    candidate: BlockPos,
    config: &IceSpikeCfg,
    grid: &mut VegGrid,
) -> bool {
    let mut origin = candidate;
    while is_air(base_id(grid.get(origin.x, origin.y, origin.z)))
        && origin.y > grid.min_y + 2
    {
        origin.y -= 1;
    }

    if !config
        .can_place_on
        .contains(base_id(grid.get(origin.x, origin.y, origin.z)))
    {
        return false;
    }

    origin.y += random.next_int_bounded(4);
    let height = random.next_int_bounded(4) + 7;
    let width = height / 4 + random.next_int_bounded(2);
    if width > 1 && random.next_int_bounded(60) == 0 {
        origin.y += 10 + random.next_int_bounded(30);
    }

    let state_id = grid.interner().id_of(&config.state);
    for y_offset in 0..height {
        let scale = (1.0_f32 - y_offset as f32 / height as f32) * width as f32;
        let new_width = scale.ceil() as i32;
        for x_offset in -new_width..=new_width {
            let dx = x_offset.abs() as f32 - 0.25;
            for z_offset in -new_width..=new_width {
                let dz = z_offset.abs() as f32 - 0.25;
                let inside = (x_offset == 0 && z_offset == 0)
                    || !(dx * dx + dz * dz > scale * scale);
                let edge = x_offset == -new_width
                    || x_offset == new_width
                    || z_offset == -new_width
                    || z_offset == new_width;
                if !inside || (edge && random.next_float() > 0.75) {
                    continue;
                }

                let upper = BlockPos {
                    x: origin.x + x_offset,
                    y: origin.y + y_offset,
                    z: origin.z + z_offset,
                };
                if can_replace_at(grid, config, upper) {
                    grid.set_id_if_in_bounds(upper.x, upper.y, upper.z, state_id);
                }

                if y_offset != 0 && new_width > 1 {
                    let lower = BlockPos {
                        x: origin.x + x_offset,
                        y: origin.y - y_offset,
                        z: origin.z + z_offset,
                    };
                    if can_replace_at(grid, config, lower) {
                        grid.set_id_if_in_bounds(lower.x, lower.y, lower.z, state_id);
                    }
                }
            }
        }
    }

    let pillar_width = (width - 1).clamp(0, 1);
    for x_offset in -pillar_width..=pillar_width {
        for z_offset in -pillar_width..=pillar_width {
            let mut y = origin.y - 1;
            let mut run_length = 50;
            if x_offset.abs() == 1 && z_offset.abs() == 1 {
                run_length = random.next_int_bounded(5);
            }
            while y > 50 {
                let current = grid.get(origin.x + x_offset, y, origin.z + z_offset);
                if !can_replace_at(grid, config, BlockPos {
                    x: origin.x + x_offset,
                    y,
                    z: origin.z + z_offset,
                }) && current != config.state
                {
                    break;
                }
                grid.set_id_if_in_bounds(
                    origin.x + x_offset,
                    y,
                    origin.z + z_offset,
                    state_id,
                );
                y -= 1;
                run_length -= 1;
                if run_length <= 0 {
                    y -= random.next_int_bounded(5) + 1;
                    run_length = random.next_int_bounded(5);
                }
            }
        }
    }

    true
}

fn can_replace_at(grid: &VegGrid, config: &IceSpikeCfg, pos: BlockPos) -> bool {
    let state = grid.get(pos.x, pos.y, pos.z);
    is_air(base_id(state)) || config.can_replace.contains(base_id(state))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::compose::build_decoration_catalog;
    use crate::density::NoiseParams;
    use crate::feature::vegetation::{
        place_configured_feature, ConfiguredFeature, VegTags,
    };
    use crate::rng::XoroshiroPositionalFactory;

    const EXTERNAL: &str = include_str!("../../../tests/support/ice_spike_feature_external.txt");

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

    fn cfg() -> IceSpikeCfg {
        IceSpikeCfg {
            state: "minecraft:packed_ice".to_owned(),
            can_place_on: HashSet::from(["minecraft:snow_block".to_owned()]),
            can_replace: HashSet::from([
                "minecraft:snow_block".to_owned(),
                "minecraft:ice".to_owned(),
            ]),
        }
    }

    fn flat_grid(origin: BlockPos) -> VegGrid {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        for x in 0..16 {
            for z in 0..16 {
                for y in -64..origin.y {
                    grid.seed(x, y, z, "minecraft:stone".to_owned());
                }
            }
        }
        grid.seed(origin.x, origin.y, origin.z, "minecraft:snow_block".to_owned());
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
            panic!("the spike fixture does not fork randomness")
        }

        fn set_seed(&mut self, _seed: i64) {
            panic!("the spike fixture does not reseed randomness")
        }

        fn next_bits(&mut self, _bits: u32) -> i32 {
            panic!("the spike fixture does not request raw bits")
        }

        fn next_int(&mut self) -> i32 {
            panic!("the spike fixture does not request an unbounded integer")
        }

        fn next_int_bounded(&mut self, bound: i32) -> i32 {
            let value = *self.ints.get(self.next_int).expect("fixture random draw");
            self.next_int += 1;
            assert!((0..bound).contains(&value), "scripted draw {value} outside 0..{bound}");
            value
        }

        fn next_long(&mut self) -> i64 {
            panic!("the spike fixture does not request a long")
        }

        fn next_bool(&mut self) -> bool {
            panic!("the spike fixture does not request a boolean")
        }

        fn next_float(&mut self) -> f32 {
            self.floats += 1;
            0.0
        }

        fn next_double(&mut self) -> f64 {
            panic!("the spike fixture does not request a double")
        }

        fn next_gaussian(&mut self) -> f64 {
            panic!("the spike fixture does not request a gaussian")
        }

        fn consume_count(&mut self, _rounds: u32) {
            panic!("the spike fixture does not consume random rounds")
        }
    }

    #[test]
    fn external_fixture_captures_taper_and_support_gate() {
        let expected = fixture();
        let mut grid = flat_grid(expected.origin);
        let mut random = ScriptedRandom::new(&[0, 0, 0]);
        assert!(place_ice_spike(
            &mut random,
            expected.origin,
            &cfg(),
            &mut grid,
        ));
        assert_eq!(random.next_int, 3, "height, width and offset consume three draws");
        assert_eq!(random.floats, 8, "only admitted tapered edge cells draw floats");

        let got: HashMap<_, _> = expected
            .states
            .keys()
            .map(|&pos| (pos, grid.get(pos.0, pos.1, pos.2).to_owned()))
            .collect();
        assert_eq!(got, expected.states);
        let written: std::collections::HashSet<_> = grid
            .dirty_cells()
            .map(|(x, y, z, _)| (x, y, z))
            .collect();
        assert_eq!(written, expected.states.keys().copied().collect());
    }

    #[test]
    fn unsupported_surface_is_rejected_before_random_draws() {
        let origin = BlockPos { x: 8, y: 64, z: 8 };
        let mut grid = flat_grid(origin);
        grid.seed(origin.x, origin.y, origin.z, "minecraft:stone".to_owned());
        let mut random = ScriptedRandom::new(&[0, 0, 0]);
        assert!(!place_ice_spike(&mut random, origin, &cfg(), &mut grid));
        assert_eq!(random.next_int, 0);
        assert_eq!(random.floats, 0);
        assert_eq!(grid.dirty_len(), 0);
    }

    #[test]
    fn configured_dispatch_reaches_spike_body() {
        let expected = fixture();
        let mut grid = flat_grid(expected.origin);
        let mut random = ScriptedRandom::new(&[0, 0, 0]);
        let feature = ConfiguredFeature::IceSpike(Box::new(cfg()));
        place_configured_feature(
            &mut random,
            expected.origin,
            &feature,
            &mut grid,
            &VegTags::default(),
        );
        assert_eq!(random.next_int, 3);
        assert_eq!(random.floats, 8);
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
            unreachable!("the spike configuration does not use noise")
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
    fn bundled_config_and_catalog_reach_ice_spike() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets");
        let resolver = AssetResolver { root };
        let document = resolver.json("configured_feature", "minecraft:ice_spike");
        let parsed = super::super::config::parse_configured_feature_doc(&resolver, &document);
        let ConfiguredFeature::IceSpike(config) = parsed else {
            panic!("ice_spike must parse as the dedicated feature body");
        };
        assert_eq!(config.state, "minecraft:packed_ice");
        assert!(config.can_place_on.contains("minecraft:snow_block"));
        assert!(config.can_replace.contains("minecraft:dirt"));
        assert!(config.can_replace.contains("minecraft:ice"));

        let catalog = build_decoration_catalog(&resolver, &["minecraft:ice_spikes".to_owned()]);
        let selected = catalog
            .select(["minecraft:ice_spikes"])
            .into_iter()
            .find(|(_, _, placed)| placed.registry_id.as_deref() == Some("minecraft:ice_spike"))
            .expect("ice_spikes must select its placed feature");
        assert!(matches!(
            selected.2.feature.as_ref(),
            ConfiguredFeature::IceSpike(_)
        ));
    }
}
