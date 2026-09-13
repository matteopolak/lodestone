//! Fossil configured-feature placement.
//!
//! Fossils are the one decoration family whose configured body is a pair of
//! structure-template lists rather than a procedural shape.  The first list
//! supplies the bone template and the second supplies the matching ore overlay;
//! both are selected with one shared index and placed at one settled position.
//! Keeping the body here leaves the registry parser responsible only for
//! resolving data and keeps the placement dispatcher small.
//!
//! # How it works
//!
//! The body consumes a horizontal rotation draw, a template-index draw, and a
//! ten-block vertical jitter draw.  It samples the ocean-floor height below the
//! unrotated template footprint, settles the template no lower than the build
//! floor plus ten, rejects a placement whose eight bounding-box corners contain
//! too much air or fluid, then places the bone and overlay templates in order.
//! A dense scratch field lets the existing template processor engine read the
//! pre-placement world and clips the resulting writes back through [`VegGrid`].
//!
//! # How to change it
//!
//! Add a processor shape to [`parse_processor`] only when its random and world
//! reads are fully represented by the structure processor engine.  Keep the
//! template lists parallel: the index draw is shared, so silently dropping one
//! side changes every later placement.  The shared configured-feature enum,
//! parser arm and dispatcher arm live in the parent vegetation modules; keep
//! those three registry edges in lockstep with this body so a parsed fossil
//! cannot become an unconsumed island.
//!
//! # Configuration
//!
//! The `fossil_structures` and `overlay_structures` arrays contain structure
//! template ids.  `fossil_processors` and `overlay_processors` are processor
//! list ids (or inline processor-list objects), and
//! `max_empty_corners_allowed` is bounded to the eight corners of the selected
//! fossil template's transformed box.
//!
//! # Dependencies
//!
//! [`StructureTemplate`] and [`Processor`] provide template decoding and
//! processor semantics; [`VegGrid`] supplies the live heightmap, read field and
//! clipped write surface.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use crate::density::Resolver;
use crate::feature::BlockPos;
use crate::dense_grid::DenseBlockGrid;
use crate::rng::RandomSource;
use crate::structure::processor::{PosTest, Processor, ProcessorRule, RuleTest};
use crate::structure::template::{
    BlockState, Mirror, PlaceOrigin, PlaceSettings, Rotation, StructureTemplate,
};

use super::base_id;
use super::config::{canon_state, is_air, is_fluid, resolve_block_set};
use super::grid::VegGrid;

/// Parsed lists and processors for one fossil configured feature.
#[derive(Clone, Debug)]
pub struct FossilCfg {
    pub(super) fossil_structures: Vec<Arc<StructureTemplate>>,
    pub(super) overlay_structures: Vec<Arc<StructureTemplate>>,
    pub(super) fossil_processors: Vec<Processor>,
    pub(super) overlay_processors: Vec<Processor>,
    pub(super) max_empty_corners_allowed: usize,
}

impl FossilCfg {
    /// Resolves the template and processor references in one fossil document.
    ///
    /// A fossil is useful only when every pair is available.  Returning `None`
    /// for a missing or malformed member preserves the configured-feature
    /// parser's existing degrade-to-unsupported contract without creating a
    /// partially overlaid fossil.
    pub(super) fn try_parse(resolver: &dyn Resolver, config: &Value) -> Option<Self> {
        let fossil_ids = parse_id_list(config.get("fossil_structures")?)?;
        let overlay_ids = parse_id_list(config.get("overlay_structures")?)?;
        if fossil_ids.is_empty() || fossil_ids.len() != overlay_ids.len() {
            return None;
        }

        let fossil_structures = load_templates(resolver, &fossil_ids)?;
        let overlay_structures = load_templates(resolver, &overlay_ids)?;
        if fossil_structures.len() != overlay_structures.len() {
            return None;
        }

        let fossil_processors = parse_processor_list(resolver, config.get("fossil_processors")?)?;
        let overlay_processors = parse_processor_list(resolver, config.get("overlay_processors")?)?;
        let max_empty_corners_allowed = config
            .get("max_empty_corners_allowed")
            .and_then(Value::as_i64)
            .filter(|value| (0..=7).contains(value))? as usize;

        Some(Self {
            fossil_structures,
            overlay_structures,
            fossil_processors,
            overlay_processors,
            max_empty_corners_allowed,
        })
    }
}

fn parse_id_list(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|entry| entry.as_str().map(str::to_owned))
        .collect()
}

fn load_templates(resolver: &dyn Resolver, ids: &[String]) -> Option<Vec<Arc<StructureTemplate>>> {
    ids.iter()
        .map(|id| {
            let bytes = resolver.structure_template(id)?;
            let template = StructureTemplate::parse(&bytes).ok()?;
            template.size().iter().all(|size| *size > 0).then(|| Arc::new(template))
        })
        .collect()
}

fn parse_processor_list(resolver: &dyn Resolver, value: &Value) -> Option<Vec<Processor>> {
    let document = match value {
        Value::String(id) => resolver.processor_list(id),
        Value::Object(_) => value.clone(),
        _ => return None,
    };
    let document = serde_json::from_value::<ProcessorListDocument>(document).ok()?;
    document
        .processors
        .into_iter()
        .map(|entry| parse_processor(resolver, entry))
        .collect()
}

/// Closed processor-list schema used by fossil templates.
///
/// The processor and predicate names are decoded before any placement code sees
/// them.  In particular, a new namespaced discriminator or an extra field cannot
/// silently fall through to a nearby processor with different random draws.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessorListDocument {
    processors: Vec<ProcessorDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "processor_type", deny_unknown_fields)]
enum ProcessorDocument {
    #[serde(rename = "minecraft:nop", alias = "nop")]
    Nop {},
    #[serde(rename = "minecraft:block_rot", alias = "block_rot")]
    BlockRot {
        integrity: f32,
        rottable_blocks: Option<Value>,
    },
    #[serde(rename = "minecraft:protected_blocks", alias = "protected_blocks")]
    ProtectedBlocks { value: Value },
    #[serde(rename = "minecraft:rule", alias = "rule")]
    Rule { rules: Vec<RuleDocument> },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleDocument {
    input_predicate: Value,
    location_predicate: Option<Value>,
    position_predicate: Option<Value>,
    output_state: Value,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "predicate_type", deny_unknown_fields)]
enum RuleTestDocument {
    #[serde(rename = "minecraft:always_true", alias = "always_true")]
    AlwaysTrue {},
    #[serde(rename = "minecraft:block_match", alias = "block_match")]
    BlockMatch { block: String },
    #[serde(rename = "minecraft:blockstate_match", alias = "blockstate_match")]
    BlockStateMatch { block_state: Value },
    #[serde(rename = "minecraft:random_block_match", alias = "random_block_match")]
    RandomBlockMatch { block: String, probability: f64 },
    #[serde(rename = "minecraft:random_blockstate_match", alias = "random_blockstate_match")]
    RandomBlockStateMatch { block_state: Value, probability: f64 },
    #[serde(rename = "minecraft:tag_match", alias = "tag_match")]
    TagMatch { tag: String },
}

fn parse_processor(resolver: &dyn Resolver, value: ProcessorDocument) -> Option<Processor> {
    match value {
        ProcessorDocument::Nop {} => Some(Processor::BlockIgnore(Vec::new())),
        ProcessorDocument::BlockRot {
            integrity,
            rottable_blocks,
        } => {
            let integrity = probability_f32(integrity)?;
            let rottable = match rottable_blocks {
                None | Some(Value::Null) => None,
                Some(blocks) => Some(Arc::new(resolve_block_set(resolver, &blocks)?)),
            };
            Some(Processor::BlockRot { rottable, integrity })
        }
        ProcessorDocument::ProtectedBlocks { value } => {
            let blocks = resolve_block_set(resolver, &value)?;
            Some(Processor::ProtectedBlocks(Arc::new(blocks)))
        }
        ProcessorDocument::Rule { rules } => {
            let rules = rules
                .iter()
                .map(|rule| parse_rule(resolver, rule))
                .collect::<Option<Vec<_>>>()?;
            Some(Processor::Rule(rules))
        }
    }
}

fn parse_rule(resolver: &dyn Resolver, value: &RuleDocument) -> Option<ProcessorRule> {
    let input = parse_rule_test(resolver, &value.input_predicate)?;
    let location = value
        .location_predicate
        .as_ref()
        .map(|predicate| parse_rule_test(resolver, predicate))
        .unwrap_or(Some(RuleTest::AlwaysTrue))?;
    let position = value
        .position_predicate
        .as_ref()
        .map(parse_position_test)
        .unwrap_or(Some(PosTest::AlwaysTrue))?;
    let output = BlockState::parse(&canonical_state(&value.output_state)?);
    Some(ProcessorRule {
        input,
        location,
        position,
        output,
    })
}

fn parse_position_test(value: &Value) -> Option<PosTest> {
    let document = with_default_predicate_type(value)?;
    matches!(
        serde_json::from_value::<RuleTestDocument>(document).ok()?,
        RuleTestDocument::AlwaysTrue {}
    )
    .then_some(PosTest::AlwaysTrue)
}

fn parse_rule_test(resolver: &dyn Resolver, value: &Value) -> Option<RuleTest> {
    match serde_json::from_value::<RuleTestDocument>(with_default_predicate_type(value)?).ok()? {
        RuleTestDocument::AlwaysTrue {} => Some(RuleTest::AlwaysTrue),
        RuleTestDocument::BlockMatch { block } => Some(RuleTest::BlockMatch(block)),
        RuleTestDocument::BlockStateMatch { block_state } => {
            Some(RuleTest::BlockStateMatch(canonical_state(&block_state)?))
        }
        RuleTestDocument::RandomBlockMatch { block, probability } => {
            Some(RuleTest::RandomBlockMatch(block, probability_f64(probability)?))
        }
        RuleTestDocument::RandomBlockStateMatch {
            block_state,
            probability,
        } => Some(RuleTest::RandomBlockStateMatch(
            canonical_state(&block_state)?,
            probability_f64(probability)?,
        )),
        RuleTestDocument::TagMatch { tag } => {
            let holder = Value::String(format!("#{tag}"));
            Some(RuleTest::TagMatch(Arc::new(resolve_block_set(resolver, &holder)?)))
        }
    }
}

fn with_default_predicate_type(value: &Value) -> Option<Value> {
    let Value::Object(object) = value else {
        return None;
    };
    if object.contains_key("predicate_type") {
        return Some(value.clone());
    }
    let mut value = object.clone();
    value.insert(
        "predicate_type".to_owned(),
        Value::String("minecraft:always_true".to_owned()),
    );
    Some(Value::Object(value))
}

fn canonical_state(value: &Value) -> Option<String> {
    value.get("Name").and_then(Value::as_str)?;
    Some(canon_state(value))
}

fn probability_f32(value: f32) -> Option<f32> {
    (value.is_finite() && (0.0..=1.0).contains(&value)).then_some(value)
}

fn probability_f64(value: f64) -> Option<f32> {
    (value.is_finite() && (0.0..=1.0).contains(&value)).then_some(value as f32)
}

/// Places one fossil and returns whether the corner gate admitted it.
pub(super) fn place_fossil<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    config: &FossilCfg,
    grid: &mut VegGrid,
) -> bool {
    let rotation = Rotation::random(random);
    let index = random.next_int_bounded(config.fossil_structures.len() as i32) as usize;
    let fossil = &config.fossil_structures[index];
    let overlay = &config.overlay_structures[index];

    let template_size = fossil.size();
    let size = match rotation {
        Rotation::Cw90 | Rotation::Ccw90 => [template_size[2], template_size[1], template_size[0]],
        Rotation::None | Rotation::Cw180 => template_size,
    };
    let low_corner = [origin.x - size[0] / 2, origin.y, origin.z - size[2] / 2];
    let mut lowest_surface = origin.y;
    for x in 0..size[0].max(0) {
        for z in 0..size[2].max(0) {
            lowest_surface = lowest_surface.min(grid.height_ocean_floor(
                low_corner[0] + x,
                low_corner[2] + z,
            ));
        }
    }
    let target_y = (lowest_surface - 15 - random.next_int_bounded(10)).max(grid.min_y + 10);
    let target = zero_position_with_transform(
        [low_corner[0], target_y, low_corner[2]],
        rotation,
        template_size[0],
        template_size[2],
    );

    let settings = PlaceSettings {
        rotation,
        mirror: Mirror::None,
        pivot: [0, 0, 0],
        processors: Vec::new(),
        waterlogging: true,
    };
    let fossil_box = fossil.bounding_box(target, &settings);
    if count_empty_corners(grid, fossil_box) > config.max_empty_corners_allowed {
        return false;
    }

    place_template(fossil, &config.fossil_processors, target, rotation, grid);
    place_template(overlay, &config.overlay_processors, target, rotation, grid);
    true
}

fn zero_position_with_transform(
    zero: [i32; 3],
    rotation: Rotation,
    size_x: i32,
    size_z: i32,
) -> [i32; 3] {
    let sx = size_x - 1;
    let sz = size_z - 1;
    let relative = match rotation {
        Rotation::Ccw90 => [0, 0, sx],
        Rotation::Cw90 => [sz, 0, 0],
        Rotation::Cw180 => [sx, 0, sz],
        Rotation::None => [0, 0, 0],
    };
    [zero[0] + relative[0], zero[1], zero[2] + relative[2]]
}

fn count_empty_corners(grid: &VegGrid, bounds: crate::structure::BoundingBox) -> usize {
    let mut empty = 0;
    for x in [bounds.min[0], bounds.max[0]] {
        for y in [bounds.min[1], bounds.max[1]] {
            for z in [bounds.min[2], bounds.max[2]] {
                let base = base_id(grid.interner().name_of(grid.get_id(x, y, z)));
                if is_air(base) || is_fluid(base) {
                    empty += 1;
                }
            }
        }
    }
    empty
}

fn place_template(
    template: &StructureTemplate,
    processors: &[Processor],
    target: [i32; 3],
    rotation: Rotation,
    grid: &mut VegGrid,
) {
    let settings = PlaceSettings {
        rotation,
        mirror: Mirror::None,
        pivot: [0, 0, 0],
        processors: processors.to_vec(),
        waterlogging: true,
    };
    let bounds = template.bounding_box(target, &settings);
    let size_x = bounds.max[0] - bounds.min[0] + 1;
    let size_y = bounds.max[1] - bounds.min[1] + 1;
    let size_z = bounds.max[2] - bounds.min[2] + 1;
    let air = grid.interner().id_of("minecraft:air");
    let mut scratch = DenseBlockGrid::with_interner(
        Arc::clone(grid.interner()),
        bounds.min[0],
        bounds.min[1],
        bounds.min[2],
        size_x,
        size_y,
        size_z,
        air,
    );
    let mut before = Vec::with_capacity((size_x * size_y * size_z) as usize);
    for y in bounds.min[1]..=bounds.max[1] {
        for z in bounds.min[2]..=bounds.max[2] {
            for x in bounds.min[0]..=bounds.max[0] {
                let state = grid.get_id(x, y, z);
                before.push(state);
                scratch.set_id(x, y, z, state);
            }
        }
    }

    template.place(
        PlaceOrigin {
            position: target,
            reference: target,
            seed: 0,
        },
        &settings,
        &mut scratch,
    );

    let mut i = 0;
    for y in bounds.min[1]..=bounds.max[1] {
        for z in bounds.min[2]..=bounds.max[2] {
            for x in bounds.min[0]..=bounds.max[0] {
                let state = scratch.get_id(x, y, z);
                if state != before[i] {
                    let _ = grid.set_id_if_in_bounds(x, y, z, state);
                }
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::*;
    use crate::rng::XoroshiroPositionalFactory;

    const EXTERNAL: &str = include_str!("../../../tests/support/fossil_feature_external.txt");

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

    fn flat_grid() -> VegGrid {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        for x in 0..16 {
            for z in 0..16 {
                for y in -64..=64 {
                    grid.seed(x, y, z, "minecraft:stone".to_owned());
                }
            }
        }
        grid
    }

    fn cfg() -> FossilCfg {
        let bone = BlockState::of("minecraft:bone_block");
        let coal = BlockState::of("minecraft:coal_ore");
        FossilCfg {
            fossil_structures: vec![Arc::new(StructureTemplate::from_blocks(
                [3, 1, 2],
                vec![bone],
                vec![([0, 0, 0], 0), ([1, 0, 0], 0), ([2, 0, 1], 0)],
            ))],
            overlay_structures: vec![Arc::new(StructureTemplate::from_blocks(
                [3, 1, 2],
                vec![coal],
                vec![([0, 0, 0], 0), ([1, 0, 0], 0)],
            ))],
            fossil_processors: Vec::new(),
            overlay_processors: Vec::new(),
            max_empty_corners_allowed: 0,
        }
    }

    struct ScriptedRandom {
        values: Vec<i32>,
        next: usize,
    }

    impl ScriptedRandom {
        fn new(values: &[i32]) -> Self {
            Self {
                values: values.to_vec(),
                next: 0,
            }
        }
    }

    impl RandomSource for ScriptedRandom {
        type Positional = XoroshiroPositionalFactory;

        fn fork_positional(&mut self) -> Self::Positional {
            panic!("the fossil fixture does not fork randomness")
        }

        fn set_seed(&mut self, _seed: i64) {
            panic!("the fossil fixture does not reseed randomness")
        }

        fn next_bits(&mut self, _bits: u32) -> i32 {
            panic!("the fossil fixture does not request raw bits")
        }

        fn next_int(&mut self) -> i32 {
            panic!("the fossil fixture does not request an unbounded integer")
        }

        fn next_int_bounded(&mut self, bound: i32) -> i32 {
            let value = *self.values.get(self.next).expect("fixture random draw");
            self.next += 1;
            assert!((0..bound).contains(&value), "scripted draw {value} outside 0..{bound}");
            value
        }

        fn next_long(&mut self) -> i64 {
            panic!("the fossil fixture does not request a long")
        }

        fn next_bool(&mut self) -> bool {
            panic!("the fossil fixture does not request a boolean")
        }

        fn next_float(&mut self) -> f32 {
            panic!("the fossil fixture does not request a float")
        }

        fn next_double(&mut self) -> f64 {
            panic!("the fossil fixture does not request a double")
        }

        fn next_gaussian(&mut self) -> f64 {
            panic!("the fossil fixture does not request a gaussian")
        }

        fn consume_count(&mut self, _rounds: u32) {
            panic!("the fossil fixture does not consume random rounds")
        }
    }

    #[test]
    fn external_fixture_captures_parallel_overlay_output() {
        let expected = fixture();
        let mut grid = flat_grid();
        let mut random = ScriptedRandom::new(&[0, 0, 0]);
        assert!(place_fossil(&mut random, expected.origin, &cfg(), &mut grid));
        assert_eq!(random.next, 3, "rotation, template, and jitter each consume one draw");

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
    fn corner_gate_rejects_air_control_before_any_template_write() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        let mut random = ScriptedRandom::new(&[0, 0, 0]);
        assert!(!place_fossil(
            &mut random,
            BlockPos { x: 8, y: 80, z: 8 },
            &cfg(),
            &mut grid,
        ));
        assert_eq!(grid.dirty_len(), 0);
    }

    #[test]
    fn transformed_zero_position_matches_external_rotation_table() {
        let values = [
            (Rotation::None, [7, 50, 7]),
            (Rotation::Cw90, [8, 50, 7]),
            (Rotation::Cw180, [9, 50, 8]),
            (Rotation::Ccw90, [7, 50, 9]),
        ];
        for (rotation, expected) in values {
            assert_eq!(
                zero_position_with_transform([7, 50, 7], rotation, 3, 2),
                expected,
            );
        }
    }

    struct AssetResolver {
        root: std::path::PathBuf,
    }

    impl AssetResolver {
        fn json(&self, kind: &str, id: &str) -> Value {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            let path = self.root.join("worldgen").join(kind).join(format!("{name}.json"));
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

        fn noise(&self, _id: &str) -> crate::density::NoiseParams {
            unreachable!("the fossil configuration does not use noise")
        }

        fn block_tag(&self, id: &str) -> Value {
            self.json("tags/block", id)
        }

        fn processor_list(&self, id: &str) -> Value {
            self.json("processor_list", id)
        }

        fn structure_template(&self, id: &str) -> Option<Vec<u8>> {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            std::fs::read(self.root.join("structure").join(format!("{name}.nbt"))).ok()
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
    fn bundled_configuration_resolves_every_pair_and_processor_chain() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets");
        let resolver = AssetResolver { root };
        for (name, overlay_processor_count) in [("fossil_coal", 2), ("fossil_diamonds", 3)] {
            let document = resolver.json("configured_feature", &format!("minecraft:{name}"));
            let config = FossilCfg::try_parse(&resolver, &document["config"])
                .expect("bundled fossil configuration must resolve");
            assert_eq!(config.fossil_structures.len(), 8);
            assert_eq!(config.overlay_structures.len(), 8);
            assert_eq!(config.max_empty_corners_allowed, 4);
            assert!(matches!(
                config.fossil_processors.first(),
                Some(Processor::BlockRot { integrity, .. }) if (*integrity - 0.9).abs() < f32::EPSILON
            ));
            assert_eq!(config.overlay_processors.len(), overlay_processor_count);
            assert!(matches!(
                config.overlay_processors.first(),
                Some(Processor::BlockRot { integrity, .. }) if (*integrity - 0.1).abs() < f32::EPSILON
            ));
            assert!(matches!(config.overlay_processors.last(), Some(Processor::ProtectedBlocks(_))));
        }
    }

    #[test]
    fn fossil_processor_boundary_rejects_unknown_discriminators_and_fields() {
        let unknown_processor = serde_json::json!({
            "processors": [{"processor_type": "minecraft:not_a_processor"}],
        });
        assert!(serde_json::from_value::<ProcessorListDocument>(unknown_processor).is_err());

        let unknown_namespace = serde_json::json!({
            "processors": [{"processor_type": "example:block_rot", "integrity": 0.5}],
        });
        assert!(serde_json::from_value::<ProcessorListDocument>(unknown_namespace).is_err());

        let extra_processor_field = serde_json::json!({
            "processors": [{
                "processor_type": "minecraft:block_rot",
                "integrity": 0.5,
                "unexpected": true,
            }],
        });
        assert!(serde_json::from_value::<ProcessorListDocument>(extra_processor_field).is_err());

        let extra_rule_field = serde_json::json!({
            "processors": [{
                "processor_type": "minecraft:rule",
                "rules": [{
                    "input_predicate": {"predicate_type": "minecraft:always_true"},
                    "output_state": {"Name": "minecraft:stone"},
                    "unexpected": true,
                }],
            }],
        });
        assert!(serde_json::from_value::<ProcessorListDocument>(extra_rule_field).is_err());

        let unknown_predicate = serde_json::json!({
            "predicate_type": "minecraft:not_a_predicate",
        });
        assert!(serde_json::from_value::<RuleTestDocument>(unknown_predicate).is_err());

        let extra_predicate_field = serde_json::json!({
            "predicate_type": "minecraft:block_match",
            "block": "minecraft:stone",
            "unexpected": true,
        });
        assert!(serde_json::from_value::<RuleTestDocument>(extra_predicate_field).is_err());
    }

    #[test]
    fn bundled_catalog_resolves_fossil_as_a_production_feature() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets");
        let resolver = AssetResolver { root };
        let catalog = crate::compose::build_decoration_catalog(
            &resolver,
            &["minecraft:desert".to_owned()],
        );
        let fossil = catalog
            .select(["minecraft:desert"])
            .into_iter()
            .find(|(_, _, placed)| placed.registry_id.as_deref() == Some("minecraft:fossil_upper"))
            .map(|(_, _, placed)| placed)
            .expect("desert's fossil_upper must be selected by the production catalog");
        assert!(matches!(
            fossil.feature.as_ref(),
            super::super::config::ConfiguredFeature::Fossil(_)
        ));
    }

    #[test]
    fn configured_feature_dispatch_reaches_fossil_body() {
        let expected = fixture();
        let mut grid = flat_grid();
        let mut random = ScriptedRandom::new(&[0, 0, 0]);
        let feature = super::super::config::ConfiguredFeature::Fossil(Box::new(cfg()));
        super::super::place_configured_feature(
            &mut random,
            expected.origin,
            &feature,
            &mut grid,
            &super::super::config::VegTags::default(),
        );
        assert_eq!(random.next, 3, "the dispatcher must reach the fossil body's three draws");
        for (&(x, y, z), state) in &expected.states {
            assert_eq!(grid.get(x, y, z), state);
        }
    }

}
