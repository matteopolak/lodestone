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

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::density::Resolver;
use crate::feature::BlockPos;
use crate::rng::RandomSource;
use lodestone_data::block_states::StateId as CanonicalStateId;

use super::base_id;
use super::config::is_air;
use super::grid::VegGrid;

/// A validated registry key from a configured-feature document.
///
/// The resolver still owns registry lookup, but this boundary does not carry
/// arbitrary strings: a key has exactly one namespace and path. Tags are a
/// separate field in the schema and never hide behind a leading `#` here.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ResourceKey(String);

impl ResourceKey {
    fn parse(value: String) -> Result<Self, String> {
        let Some((namespace, path)) = value.split_once(':') else {
            return Err(format!("resource key {value:?} is missing its namespace"));
        };
        if namespace.is_empty()
            || path.is_empty()
            || namespace
                .chars()
                .any(|character| !matches!(character, 'a'..='z' | '0'..='9' | '_' | '.' | '-'))
            || path
                .chars()
                .any(|character| !matches!(character, 'a'..='z' | '0'..='9' | '_' | '.' | '-' | '/'))
        {
            return Err(format!("resource key {value:?} is not namespaced lowercase id"));
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ResourceKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl Serialize for ResourceKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// A property key/value pair from a block-state object.
///
/// Property names are versioned registry data, so the key set intentionally
/// stays open. The values are still strings, never arbitrary JSON values.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PropertyKey(String);

impl<'de> Deserialize<'de> for PropertyKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.is_empty()
            || value
                .chars()
                .any(|character| !matches!(character, 'a'..='z' | '0'..='9' | '_' | '-'))
        {
            return Err(serde::de::Error::custom(format!(
                "invalid block-state property name {value:?}"
            )));
        }
        Ok(Self(value))
    }
}

impl Serialize for PropertyKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PropertyValue(String);

impl<'de> Deserialize<'de> for PropertyValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            return Err(serde::de::Error::custom(format!(
                "invalid block-state property value {value:?}"
            )));
        }
        Ok(Self(value))
    }
}

impl Serialize for PropertyValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockStateDocument {
    #[serde(rename = "Name")]
    name: ResourceKey,
    #[serde(rename = "Properties", default)]
    properties: BTreeMap<PropertyKey, PropertyValue>,
}

impl BlockStateDocument {
    fn into_state_id(self) -> Option<CanonicalStateId> {
        let mut canonical = self.name.0;
        if !self.properties.is_empty() {
            canonical.push('[');
            for (index, (key, value)) in self.properties.into_iter().enumerate() {
                if index != 0 {
                    canonical.push(',');
                }
                canonical.push_str(&key.0);
                canonical.push('=');
                canonical.push_str(&value.0);
            }
            canonical.push(']');
        }
        CanonicalStateId::from_state_str(&canonical)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            Self::One(value) => vec![value],
            Self::Many(values) => values,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum MatchingBlocksDocument {
    #[serde(rename = "minecraft:matching_blocks", alias = "matching_blocks")]
    MatchingBlocks { blocks: OneOrMany<ResourceKey> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum MatchingBlockTagDocument {
    #[serde(rename = "minecraft:matching_block_tag", alias = "matching_block_tag")]
    MatchingBlockTag { tag: ResourceKey },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IceSpikeConfigDocument {
    can_place_on: MatchingBlocksDocument,
    can_replace: MatchingBlockTagDocument,
    state: BlockStateDocument,
}

/// Parsed support, replacement, and output state for one packed-ice spike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IceSpikeCfg {
    pub(super) state: CanonicalStateId,
    pub(super) can_place_on: HashSet<String>,
    pub(super) can_replace: HashSet<String>,
}

impl IceSpikeCfg {
    /// Parses the predicate forms used by the bundled spike record.
    ///
    /// The generic serialization adapter keeps the shared resolver's JSON
    /// boundary out of this closed schema module. `parse_configured_feature_doc`
    /// still hands us its ordinary JSON value, while this function immediately
    /// decodes only the fields this feature owns.
    pub(super) fn try_parse<S>(resolver: &dyn Resolver, config: &S) -> Option<Self>
    where
        S: serde::Serialize + ?Sized,
    {
        let config = serde_json::to_value(config).ok()?;
        let config = serde_json::from_value::<IceSpikeConfigDocument>(config).ok()?;
        let can_place_on: HashSet<String> = match config.can_place_on {
            MatchingBlocksDocument::MatchingBlocks { blocks } => blocks
                .into_vec()
                .into_iter()
                .map(|state| base_id(state.as_str()).to_owned())
                .collect(),
        };
        if can_place_on.is_empty() {
            return None;
        }

        let tag = match config.can_replace {
            MatchingBlockTagDocument::MatchingBlockTag { tag } => tag,
        };
        let mut can_replace = HashSet::new();
        crate::compose::resolve_block_tag(
            resolver,
            tag.as_str(),
            &mut can_replace,
            &mut HashSet::new(),
        );
        if can_replace.is_empty() {
            return None;
        }

        let state = config.state.into_state_id()?;

        Some(Self {
            state,
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

    let state_id = grid.interner().id_of_canonical(config.state);
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
                let current = grid.get_id(origin.x + x_offset, y, origin.z + z_offset);
                if !can_replace_at(grid, config, BlockPos {
                    x: origin.x + x_offset,
                    y,
                    z: origin.z + z_offset,
                }) && grid.interner().canonical_id(current) != Some(config.state)
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

    use super::*;
    use crate::feature::vegetation::{place_configured_feature, ConfiguredFeature, VegTags};
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
            state: CanonicalStateId::from_state_str("minecraft:packed_ice")
                .expect("packed ice is a generated block state"),
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

    #[test]
    fn typed_config_accepts_the_external_shape_and_roundtrips() {
        let input = r##"{
            "can_place_on": {
                "type": "minecraft:matching_blocks",
                "blocks": ["minecraft:snow_block", "minecraft:ice"]
            },
            "can_replace": {
                "type": "minecraft:matching_block_tag",
                "tag": "minecraft:ice_spike_replaceable"
            },
            "state": {
                "Name": "minecraft:oak_log",
                "Properties": {"axis": "y"}
            }
        }"##;
        let parsed: IceSpikeConfigDocument =
            serde_json::from_str(input).expect("ice-spike schema fixture must parse");
        let encoded = serde_json::to_string(&parsed).expect("typed config must serialize");
        let decoded: IceSpikeConfigDocument =
            serde_json::from_str(&encoded).expect("serialized typed config must parse");
        assert_eq!(decoded, parsed);
        assert_eq!(
            parsed.state.into_state_id().unwrap().canonical_state(),
            "minecraft:oak_log[axis=y]"
        );
    }

    #[test]
    fn typed_config_rejects_unknown_fields_and_unvalidated_ids() {
        let error = serde_json::from_str::<IceSpikeConfigDocument>(
            r##"{
                "can_place_on": {"type":"minecraft:matching_blocks", "blocks":"minecraft:snow_block"},
                "can_replace": {"type":"minecraft:matching_block_tag", "tag":"minecraft:ice_spike_replaceable"},
                "state": {"Name":"minecraft:packed_ice", "mystery":true}
            }"##,
        )
        .expect_err("block-state unknown fields must be rejected");
        assert!(error.to_string().contains("unknown field"), "{error}");

        let error = serde_json::from_str::<IceSpikeConfigDocument>(
            r##"{
                "can_place_on": {"type":"minecraft:matching_blocks", "blocks":["Snow_Block"]},
                "can_replace": {"type":"minecraft:matching_block_tag", "tag":"minecraft:ice_spike_replaceable"},
                "state": {"Name":"minecraft:packed_ice"}
            }"##,
        )
        .expect_err("resource keys must retain lowercase namespaced identity");
        let _ = error;

        let error = serde_json::from_str::<IceSpikeConfigDocument>(
            r##"{
                "can_place_on": {"type":"minecraft:future_predicate", "blocks":"minecraft:snow_block"},
                "can_replace": {"type":"minecraft:matching_block_tag", "tag":"minecraft:ice_spike_replaceable"},
                "state": {"Name":"minecraft:packed_ice"}
            }"##,
        )
        .expect_err("unknown predicate discriminators must be rejected");
        assert!(error.to_string().contains("unknown variant"), "{error}");

        let error = serde_json::from_str::<IceSpikeConfigDocument>(
            r##"{
                "can_place_on": {"type":"minecraft:matching_blocks", "blocks":"minecraft:snow_block"},
                "can_replace": {"type":"minecraft:matching_block_tag", "tag":"minecraft:ice_spike_replaceable"},
                "state": {"Name":"minecraft:packed_ice", "Properties":{"axis":true}}
            }"##,
        )
        .expect_err("state property values must remain strings");
        assert!(error.to_string().contains("string"), "{error}");
    }

}
