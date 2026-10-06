//! The data layer vegetal decoration is parsed into: heightmap kinds, block
//! predicates, block-state providers, placement modifiers, decorators, and the
//! placed-feature/configured-feature resolution that turns a registry document into
//! one of them.
//!
//! Moved here verbatim from `feature/vegetation.rs` by U16 Phase B; see [`super`]'s own
//! module doc for the scope and the named approximations.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

use crate::density::Resolver;
use crate::feature::{BlockPos, IntProvider};
use crate::rng::RandomSource;
use lodestone_data::block::{Block, BlockMask};
use lodestone_data::block_states::StateId as CanonicalStateId;

use super::grid::VegGrid;
use super::ids::{IdTags, Tag, tag_at};
use super::super::state_predicate::StatePredicate;
use super::tree::{FoliagePlacerCfg, TrunkPlacerCfg};

/// The reference block-predicate base kind (the
/// subset grass/flower/tree placement and the rule-based state provider use).
/// Unknown predicate types degrade to [`BlockPredicate::True`] (this
/// module's blanket "unsupported degrades, never panics" rule) — see this
/// module's doc comment for why that must never be a panic. Predicates whose
/// placement semantics are needed by the bundled data are represented
/// explicitly below, including the downward sturdy-face check used by
/// hanging cave vegetation.
#[derive(Clone, Debug)]
pub(crate) enum BlockPredicate {
    True,
    Solid,
    /// Tests the state at `position + offset` for center support on its
    /// downward face. This is the target condition for the bundled upward
    /// environment scan that anchors hanging vegetation to a ceiling.
    HasSturdyFaceDown { offset: (i32, i32, i32) },
    Not(Box<BlockPredicate>),
    /// The all-of/any-of predicate combinators — added for `patch_sugar_cane*`'s
    /// `block_predicate_filter`, which nests a `matching_block_tag` +
    /// `would_survive` + `any_of(matching_fluids)` combinator. Before these
    /// two variants existed, every combinator type fell through to
    /// [`BlockPredicate::True`] — harmless while nothing in scope used one,
    /// but it would have made sugar cane's water-adjacency requirement a
    /// silent no-op *in the wrong direction* (always-pass instead of
    /// always-fail) the moment the block-column feature support let sugar cane's
    /// placed feature actually run — named here because that direction of
    /// bug is the more dangerous one this module's "degrade, don't panic"
    /// convention can produce.
    AllOf(Vec<BlockPredicate>),
    AnyOf(Vec<BlockPredicate>),
    /// A block tag match at the predicate's position plus an optional offset.
    /// Root-system candidate predicates use the offset form to inspect the
    /// supporting block below the candidate. The registry id is decoded once
    /// into [`Tag`], so every placement attempt performs one typed bit lookup
    /// rather than repeating an eight-way string dispatch.
    MatchingBlockTag {
        tag: Option<Tag>,
        offset: (i32, i32, i32),
    },
    /// The matching-blocks predicate. `blocks` is the JSON's `blocks`
    /// field, which is either one id or a list; `offset` is added to the tested
    /// position. Matched by **base** id, so `minecraft:water[level=0]` counts as
    /// `minecraft:water` — the same collapse [`BlockPredicate::MatchingFluid`]
    /// already documents, for the same reason (this grid never distinguishes a
    /// fluid's source/flowing variant).
    ///
    /// Before this variant existed, `matching_blocks` fell through to
    /// [`BlockPredicate::True`], which is the *dangerous* direction: `disk`'s
    /// `target` would have matched every cell in its radius and paved a
    /// column of sand through whatever was there.
    MatchingBlocks {
        blocks: BlockMask,
        offset: (i32, i32, i32),
    },
    /// The matching-fluid predicate — `fluids` is the JSON's raw
    /// `minecraft:water`/`minecraft:flowing_water`/`minecraft:lava`/
    /// `minecraft:flowing_lava` id list; `offset` is `(dx, dy, dz)` added to
    /// the tested position. Matched by base block because this
    /// engine's grid never distinguishes a fluid's source/flowing variant
    /// (the same "known representation gap: fluid `level`"
    /// `docs/worldgen-parity.md` already names) — both JSON ids for one
    /// fluid collapse onto the one base id our grid can ever hold.
    MatchingFluid {
        fluids: BlockMask,
        offset: (i32, i32, i32),
    },
    /// Approximates every `would_survive` check this module reaches as
    /// the vegetation block's own may-place-on rule — see module doc. The default for any
    /// `would_survive` whose tested state isn't one of the two special-cased
    /// below.
    WouldSurviveOnSupportsVegetation,
    /// `would_survive` on a `minecraft:cactus` state — the cactus block's
    /// own survival check: below is cactus itself or `#minecraft:supports_cactus`,
    /// all 4 horizontal neighbours non-solid, block above not a fluid.
    /// "Non-solid" is approximated as "air" (see [`BlockPredicate::test`]'s
    /// own doc on this one) — a named narrowing, not the full reference
    /// solidity table, which this crate has no other reason to carry.
    WouldSurviveCactus,
    /// `would_survive` on a `minecraft:sugar_cane` state — deliberately
    /// **omits** the sugar cane block's own survival check's water-adjacency half: every
    /// `patch_sugar_cane*` placed feature already re-checks that adjacency
    /// explicitly via a sibling `any_of(matching_fluids)` predicate in the
    /// same `all_of`, so modelling it twice would be redundant, not more
    /// correct.
    WouldSurviveSugarCane,
}

pub(super) fn parse_predicate_list(v: &Value) -> Vec<BlockPredicate> {
    v["predicates"]
        .as_array()
        .map(|arr| arr.iter().map(BlockPredicate::parse).collect())
        .unwrap_or_default()
}

/// A `HolderSet<Block>`-shaped JSON field: one id, or a list of ids. A `#tag`
/// reference resolves to nothing here (the closure needs a `Resolver` this
/// function does not have) — every `valid_blocks`/`replaceable`/`can_be_placed_on`
/// in the bundled data is a literal list, and a tag would degrade to "matches
/// nothing" rather than "matches everything", which is the safe direction.
pub(super) fn parse_id_list(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(arr) => arr
            .iter()
            .filter_map(|e| e.as_str().map(str::to_string))
            .filter(|s| !s.starts_with('#'))
            .collect(),
        _ => Vec::new(),
    }
}

pub(super) fn parse_offset(v: &Value) -> (i32, i32, i32) {
    let Some(arr) = v.as_array() else {
        return (0, 0, 0);
    };
    let get = |i: usize| arr.get(i).and_then(Value::as_i64).unwrap_or(0) as i32;
    (get(0), get(1), get(2))
}

/// See [`BlockPredicate::MatchingFluid`]'s doc: both the source and flowing JSON
/// ids for one fluid collapse onto this engine's single base id, so a JSON fluid
/// id selects one of two [`Tag`]s and an unrecognised one matches nothing.
///
/// Unit 8 turned this from a `&str`-vs-`&str` comparison into a tag selection:
/// the *state* side of the question is now answered by
/// [`super::ids`]'s bitset, so only the JSON side is still a string — and that
/// side is a fixed list of at most two entries from the placed-feature document,
/// not a per-block value.
fn parse_block_set(v: &Value) -> BlockMask {
    parse_id_list(v)
        .into_iter()
        .filter_map(|name| Block::from_name(&name))
        .collect()
}

fn parse_fluid_set(v: &Value) -> BlockMask {
    let fluid_block = |id: &str| match id {
            "minecraft:water" | "minecraft:flowing_water" => Some(Block::Water),
            "minecraft:lava" | "minecraft:flowing_lava" => Some(Block::Lava),
            _ => None,
        };
    match v {
        Value::String(id) => fluid_block(id).into_iter().collect(),
        Value::Array(ids) => ids.iter().filter_map(Value::as_str).filter_map(fluid_block).collect(),
        _ => BlockMask::default(),
    }
}

impl BlockPredicate {
    fn parse(v: &Value) -> Self {
        match serde_json::from_value::<PredicateDocument>(v.clone()) {
            Ok(PredicateDocument::Solid) => BlockPredicate::Solid,
            Ok(PredicateDocument::HasSturdyFace { direction }) if direction == "down" => {
                BlockPredicate::HasSturdyFaceDown {
                    offset: parse_offset(&v["offset"]),
                }
            }
            Ok(PredicateDocument::Not) => {
                BlockPredicate::Not(Box::new(BlockPredicate::parse(&v["predicate"])))
            }
            Ok(PredicateDocument::AllOf) => BlockPredicate::AllOf(parse_predicate_list(v)),
            Ok(PredicateDocument::AnyOf) => BlockPredicate::AnyOf(parse_predicate_list(v)),
            Ok(PredicateDocument::MatchingBlockTag { tag }) => BlockPredicate::MatchingBlockTag {
                tag: tag.map(Tag::from),
                offset: parse_offset(&v["offset"]),
            },
            Ok(PredicateDocument::MatchingBlocks) => BlockPredicate::MatchingBlocks {
                blocks: parse_block_set(&v["blocks"]),
                offset: parse_offset(&v["offset"]),
            },
            Ok(PredicateDocument::MatchingFluids) => BlockPredicate::MatchingFluid {
                fluids: parse_fluid_set(&v["fluids"]),
                offset: parse_offset(&v["offset"]),
            },
            Ok(PredicateDocument::WouldSurvive) => match v["state"]["Name"].as_str().unwrap_or("") {
                "minecraft:cactus" => BlockPredicate::WouldSurviveCactus,
                "minecraft:sugar_cane" => BlockPredicate::WouldSurviveSugarCane,
                _ => BlockPredicate::WouldSurviveOnSupportsVegetation,
            },
            Ok(PredicateDocument::True) | Err(_) | Ok(PredicateDocument::HasSturdyFace { .. }) => {
                BlockPredicate::True
            }
        }
    }

pub(super)     fn test(&self, grid: &VegGrid, tags: &VegTags, pos: BlockPos) -> bool {
        match self {
            BlockPredicate::True => true,
            BlockPredicate::Solid => {
                let state = grid.get_id(pos.x, pos.y, pos.z);
                if !tags.solid.is_empty() {
                    tags.solid.test_id(state)
                } else {
                    let block = state.block();
                    let is_air = matches!(block, Block::Air | Block::CaveAir | Block::VoidAir);
                    let is_fluid = matches!(block, Block::Water | Block::Lava);
                    !is_air && !is_fluid && lodestone_data::block_solidity::blocks_motion(state)
                }
            }
            BlockPredicate::HasSturdyFaceDown { offset } => {
                let (dx, dy, dz) = *offset;
                let state = grid.get_id(pos.x + dx, pos.y + dy, pos.z + dz);
                lodestone_data::block_survival::center_support_down(state)
            }
            BlockPredicate::Not(inner) => !inner.test(grid, tags, pos),
            BlockPredicate::AllOf(list) => list.iter().all(|p| p.test(grid, tags, pos)),
            BlockPredicate::AnyOf(list) => list.iter().any(|p| p.test(grid, tags, pos)),
            BlockPredicate::MatchingBlockTag { tag, offset } => {
                let (dx, dy, dz) = *offset;
                let pos = BlockPos {
                    x: pos.x + dx,
                    y: pos.y + dy,
                    z: pos.z + dz,
                };
                tag.is_some_and(|tag| tag_at(grid, tags, tag, pos.x, pos.y, pos.z))
            }
            BlockPredicate::MatchingBlocks { blocks, offset } => {
                let (dx, dy, dz) = *offset;
                let state = grid.get_id(pos.x + dx, pos.y + dy, pos.z + dz);
                blocks.contains(state.block())
            }
            BlockPredicate::MatchingFluid { fluids, offset } => {
                let (dx, dy, dz) = *offset;
                let id = grid.get_id(pos.x + dx, pos.y + dy, pos.z + dz);
                fluids.contains(id.block())
            }
            BlockPredicate::WouldSurviveOnSupportsVegetation => {
                tag_at(grid, tags, Tag::SupportsVegetation, pos.x, pos.y - 1, pos.z)
            }
            BlockPredicate::WouldSurviveCactus => {
                let below = grid.get_id(pos.x, pos.y - 1, pos.z);
                if !tags.has(Tag::Cactus, below)
                    && !tags.has(Tag::SupportsCactus, below)
                {
                    return false;
                }
                let neighbours_ok = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .all(|&(dx, dz)| tag_at(grid, tags, Tag::Air, pos.x + dx, pos.y, pos.z + dz));
                if !neighbours_ok {
                    return false;
                }
                !tag_at(grid, tags, Tag::Fluid, pos.x, pos.y + 1, pos.z)
            }
            BlockPredicate::WouldSurviveSugarCane => {
                let below = grid.get_id(pos.x, pos.y - 1, pos.z);
                tags.has(Tag::SugarCane, below)
                    || tags.has(Tag::SupportsSugarCane, below)
            }
        }
    }
}

/// Closed discriminator set for block-predicate registry documents. Payloads
/// that need recursive or shape-specific decoding remain in `Value` until the
/// corresponding typed variant is selected; an extension `type` fails this
/// decode and follows the module's explicit unsupported fallback.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum PredicateDocument {
    #[serde(rename = "minecraft:solid")]
    Solid,
    #[serde(rename = "minecraft:has_sturdy_face")]
    HasSturdyFace { direction: String },
    #[serde(rename = "minecraft:not")]
    Not,
    #[serde(rename = "minecraft:all_of")]
    AllOf,
    #[serde(rename = "minecraft:any_of")]
    AnyOf,
    #[serde(rename = "minecraft:matching_block_tag")]
    MatchingBlockTag {
        #[serde(default, deserialize_with = "deserialize_optional_block_tag")]
        tag: Option<BlockTagDocument>,
    },
    #[serde(rename = "minecraft:matching_blocks")]
    MatchingBlocks,
    #[serde(rename = "minecraft:matching_fluids")]
    MatchingFluids,
    #[serde(rename = "minecraft:would_survive")]
    WouldSurvive,
    #[serde(rename = "minecraft:true")]
    True,
}

fn deserialize_optional_block_tag<'de, D>(deserializer: D) -> Result<Option<BlockTagDocument>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// Closed registry-tag ids accepted by [`BlockPredicate::MatchingBlockTag`].
///
/// Serde performs the text-to-type conversion at the input boundary. Unknown
/// ids become `None` through the optional field rather than entering the
/// placement path as strings.
#[derive(Clone, Copy, Debug, Deserialize)]
enum BlockTagDocument {
    #[serde(rename = "minecraft:air")]
    Air,
    #[serde(rename = "minecraft:cannot_replace_below_tree_trunk")]
    CannotReplaceBelowTreeTrunk,
    #[serde(rename = "minecraft:huge_brown_mushroom_can_place_on")]
    HugeBrownMushroomCanPlaceOn,
    #[serde(rename = "minecraft:huge_red_mushroom_can_place_on")]
    HugeRedMushroomCanPlaceOn,
    #[serde(rename = "minecraft:replaceable_by_mushrooms")]
    ReplaceableByMushrooms,
    #[serde(rename = "minecraft:replaceable_by_trees")]
    ReplaceableByTrees,
    #[serde(rename = "minecraft:beneath_tree_podzol_replaceable")]
    BeneathTreePodzolReplaceable,
    #[serde(rename = "minecraft:azalea_grows_on")]
    AzaleaGrowsOn,
}

impl From<BlockTagDocument> for Tag {
    fn from(value: BlockTagDocument) -> Self {
        match value {
            BlockTagDocument::Air => Self::Air,
            BlockTagDocument::CannotReplaceBelowTreeTrunk => Self::CannotReplaceBelowTreeTrunk,
            BlockTagDocument::HugeBrownMushroomCanPlaceOn => Self::HugeBrownMushroomCanPlaceOn,
            BlockTagDocument::HugeRedMushroomCanPlaceOn => Self::HugeRedMushroomCanPlaceOn,
            BlockTagDocument::ReplaceableByMushrooms => Self::ReplaceableByMushrooms,
            BlockTagDocument::ReplaceableByTrees => Self::ReplaceableByTrees,
            BlockTagDocument::BeneathTreePodzolReplaceable => Self::BeneathTreePodzolReplaceable,
            BlockTagDocument::AzaleaGrowsOn => Self::AzaleaGrowsOn,
        }
    }
}

/// The reference block-state-provider base kind
/// (the subset grass/flower/tree configs use). Parsing degrades to `None`
/// on an unsupported provider type or a sub-provider that itself failed to
/// parse — see module doc.
#[derive(Clone, Debug)]
pub(crate) enum BlockStateProvider {
    Simple(CanonicalStateId),
    /// `(weight, state)` pairs, declaration order (the same walk as
    /// [`IntProvider::WeightedProviders`]).
    Weighted(Vec<(i32, CanonicalStateId)>),
    NoiseThreshold {
        seed: i64,
        first_octave: i32,
        amplitudes: Vec<f64>,
        scale: f64,
        threshold: f64,
        high_chance: f32,
        default_state: CanonicalStateId,
        low_states: Vec<CanonicalStateId>,
        high_states: Vec<CanonicalStateId>,
    },
    /// Selects one of its configured states from deterministic normal noise.
    Noise {
        seed: i64,
        first_octave: i32,
        amplitudes: Vec<f64>,
        scale: f64,
        states: Vec<CanonicalStateId>,
    },
    /// Uses a slow normal-noise field to choose the fast-field frequency,
    /// then selects a configured state from that fast field.
    DualNoise {
        seed: i64,
        first_octave: i32,
        amplitudes: Vec<f64>,
        scale: f64,
        slow_first_octave: i32,
        slow_amplitudes: Vec<f64>,
        slow_scale: f64,
        variety_min: i32,
        variety_max: i32,
        states: Vec<CanonicalStateId>,
    },
    RandomizedInt {
        source: Vec<(i32, Vec<CanonicalStateId>)>,
        values: IntProvider,
    },
    RuleBased {
        rules: Vec<(BlockPredicate, Box<BlockStateProvider>)>,
        fallback: Option<Box<BlockStateProvider>>,
    },
}

pub(super) fn canon_state(v: &Value) -> String {
    crate::feature::canonical_text(v)
}

fn parse_provider_state(v: &Value) -> Option<CanonicalStateId> {
    CanonicalStateId::from_state_str(&canon_state(v))
}

impl BlockStateProvider {
    /// Constructs a provider for a fixed built-in state at a configuration
    /// boundary. Runtime placement only sees the canonical id.
    #[cfg(test)]
    pub(super) fn simple(state: &str) -> Self {
        Self::Simple(
            CanonicalStateId::from_state_str(state)
                .unwrap_or_else(|| panic!("unknown built-in test state: {state}")),
        )
    }

pub(super)     fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "simple_state_provider" => Some(BlockStateProvider::Simple(parse_provider_state(&v["state"])?)),
            "weighted_state_provider" => {
                let entries = v["entries"].as_array()?;
                let parsed = entries
                    .iter()
                    .map(|e| {
                        let weight = e["weight"].as_i64().unwrap_or(1) as i32;
                        Some((weight, parse_provider_state(&e["data"]) ?))
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(BlockStateProvider::Weighted(parsed))
            }
            "noise_threshold_provider" => Some(BlockStateProvider::NoiseThreshold {
                seed: v["seed"].as_i64()?,
                first_octave: v["noise"]["firstOctave"].as_i64().unwrap_or(0) as i32,
                amplitudes: v["noise"]["amplitudes"]
                    .as_array()?
                    .iter()
                    .map(|a| a.as_f64().unwrap_or(0.0))
                    .collect(),
                scale: v["scale"].as_f64()?,
                threshold: v["threshold"].as_f64()?,
                high_chance: v["high_chance"].as_f64().unwrap_or(0.0) as f32,
                default_state: parse_provider_state(&v["default_state"])? ,
                low_states: v["low_states"]
                    .as_array()?
                    .iter()
                    .map(parse_provider_state)
                    .collect::<Option<Vec<_>>>()?,
                high_states: v["high_states"]
                    .as_array()?
                    .iter()
                    .map(parse_provider_state)
                    .collect::<Option<Vec<_>>>()?,
            }),
            "noise_provider" => Some(BlockStateProvider::Noise {
                seed: v["seed"].as_i64()?,
                first_octave: v["noise"]["firstOctave"].as_i64().unwrap_or(0) as i32,
                amplitudes: v["noise"]["amplitudes"]
                    .as_array()?
                    .iter()
                    .map(|a| a.as_f64().unwrap_or(0.0))
                    .collect(),
                scale: v["scale"].as_f64()?,
                states: v["states"]
                    .as_array()?
                    .iter()
                    .map(parse_provider_state)
                    .collect::<Option<Vec<_>>>()?,
            }),
            "dual_noise_provider" => Some(BlockStateProvider::DualNoise {
                seed: v["seed"].as_i64()?,
                first_octave: v["noise"]["firstOctave"].as_i64().unwrap_or(0) as i32,
                amplitudes: v["noise"]["amplitudes"]
                    .as_array()?
                    .iter()
                    .map(|a| a.as_f64().unwrap_or(0.0))
                    .collect(),
                scale: v["scale"].as_f64()?,
                slow_first_octave: v["slow_noise"]["firstOctave"].as_i64().unwrap_or(0) as i32,
                slow_amplitudes: v["slow_noise"]["amplitudes"]
                    .as_array()?
                    .iter()
                    .map(|a| a.as_f64().unwrap_or(0.0))
                    .collect(),
                slow_scale: v["slow_scale"].as_f64()?,
                variety_min: v["variety"].get(0)?.as_i64()? as i32,
                variety_max: v["variety"].get(1)?.as_i64()? as i32,
                states: v["states"]
                    .as_array()?
                    .iter()
                    .map(parse_provider_state)
                    .collect::<Option<Vec<_>>>()?,
            }),
            "randomized_int_state_provider" => {
                let property = v["property"].as_str()?;
                let values = try_parse_int_provider(&v["values"])?;
                let source = match BlockStateProvider::try_parse(&v["source"])? {
                    BlockStateProvider::Simple(state) => vec![(1, vec![state])],
                    BlockStateProvider::Weighted(entries) => entries
                        .into_iter()
                        .map(|(weight, state)| (weight, vec![state]))
                        .collect(),
                    _ => return None,
                };
                let (value_min, value_max) = match &values {
                    IntProvider::Constant(value) => (*value, *value),
                    IntProvider::Uniform { min, max } => (*min, *max),
                    _ => return None,
                };
                Some(BlockStateProvider::RandomizedInt {
                    source: source
                        .into_iter()
                        .map(|(weight, states)| (
                            weight,
                            states.into_iter().flat_map(|state| {
                                (value_min..=value_max).filter_map(move |value| {
                                    let text = replace_state_property(&state.canonical_state(), property, value);
                                    CanonicalStateId::from_state_str(&text)
                                })
                            }).collect(),
                        ))
                        .collect(),
                    values,
                })
            }
            "rule_based_state_provider" => {
                let raw_rules = v["rules"].as_array()?;
                let mut rules = Vec::with_capacity(raw_rules.len());
                for rule in raw_rules {
                    let then = BlockStateProvider::try_parse(&rule["then"])?;
                    rules.push((BlockPredicate::parse(&rule["if_true"]), Box::new(then)));
                }
                let fallback = match v.get("fallback") {
                    Some(f) if !f.is_null() => Some(Box::new(BlockStateProvider::try_parse(f)?)),
                    _ => None,
                };
                Some(BlockStateProvider::RuleBased { rules, fallback })
            }
            _ => None,
        }
    }

    /// Keeps the provider's already-canonical states ready for placement.
    pub(super) fn bind(&self) {
        match self {
            Self::Simple(_) | Self::Weighted(_) | Self::Noise { .. } | Self::DualNoise { .. }
            | Self::RandomizedInt { .. } => {}
            Self::NoiseThreshold {
                ..
            } => {}
            Self::RuleBased { rules, fallback } => {
                rules.iter().for_each(|(_, provider)| provider.bind());
                if let Some(provider) = fallback {
                    provider.bind();
                }
            }
        }
    }

    /// The state this provider yields at `pos`, **borrowed from the provider**.
    ///
    /// Unit 8: this used to return `Option<String>`, cloning out of the very
    /// config it was reading. Every grass blade, every log and every leaf paid one
    /// heap allocation for a string the provider already owned, and those clones
    /// are a large part of the 20,621 allocations
    /// `docs/worldgen-state-interning.md` attributes to this stage. Borrowing is
    /// enough because the provider outlives the placement: it lives in the
    /// generator's resolved feature list.
    ///
    /// Prefer [`Self::get_state_id`] at a placement site — the grid stores ids, so
    /// the name is only ever an intermediate.
    fn get_state<R: RandomSource>(
        &self,
        grid: &VegGrid,
        tags: &VegTags,
        random: &mut R,
        pos: BlockPos,
    ) -> Option<CanonicalStateId> {
        match self {
            BlockStateProvider::Simple(state) => Some(*state),
            BlockStateProvider::Weighted(entries) => {
                let total: i32 = entries.iter().map(|(w, _)| *w).sum();
                let mut roll = random.next_int_bounded(total.max(1));
                for (weight, state) in entries {
                    roll -= *weight;
                    if roll < 0 {
                        return Some(*state);
                    }
                }
                entries.last().map(|(_, s)| *s)
            }
            BlockStateProvider::NoiseThreshold {
                seed,
                first_octave,
                amplitudes,
                scale,
                threshold,
                high_chance,
                default_state,
                low_states,
                high_states,
            } => {
                let mut legacy = crate::rng::LegacyRandomSource::new(*seed);
                let noise =
                    crate::noise::NormalNoise::create(&mut legacy, *first_octave, amplitudes);
                let value = noise.get_value(
                    f64::from(pos.x) * scale,
                    f64::from(pos.y) * scale,
                    f64::from(pos.z) * scale,
                );
                if value < *threshold {
                    let idx = random.next_int_bounded(low_states.len().max(1) as i32) as usize;
                    Some(*low_states.get(idx).unwrap_or(default_state))
                } else if random.next_float() < *high_chance {
                    let idx = random.next_int_bounded(high_states.len().max(1) as i32) as usize;
                    Some(*high_states.get(idx).unwrap_or(default_state))
                } else {
                    Some(*default_state)
                }
            }
            BlockStateProvider::Noise { seed, first_octave, amplitudes, scale, states } => {
                let mut legacy = crate::rng::LegacyRandomSource::new(*seed);
                let noise = crate::noise::NormalNoise::create(&mut legacy, *first_octave, amplitudes);
                let value = noise.get_value(
                    f64::from(pos.x) * scale,
                    f64::from(pos.y) * scale,
                    f64::from(pos.z) * scale,
                );
                states.get(noise_state_index(value, states.len())).copied()
            }
            BlockStateProvider::DualNoise {
                seed,
                first_octave,
                amplitudes,
                scale,
                slow_first_octave,
                slow_amplitudes,
                slow_scale,
                variety_min,
                variety_max,
                states,
            } => {
                let mut slow_random = crate::rng::LegacyRandomSource::new(*seed);
                let slow_noise = crate::noise::NormalNoise::create(
                    &mut slow_random,
                    *slow_first_octave,
                    slow_amplitudes,
                );
                let slow = slow_noise.get_value(
                    f64::from(pos.x) * slow_scale,
                    f64::from(pos.y) * slow_scale,
                    f64::from(pos.z) * slow_scale,
                );
                let variety = (((slow + 1.0) * 0.5 * f64::from(variety_max - variety_min + 1))
                    as i32
                    + variety_min)
                    .clamp(*variety_min, *variety_max)
                    .max(1);
                let mut fast_random = crate::rng::LegacyRandomSource::new(*seed);
                let fast_noise = crate::noise::NormalNoise::create(
                    &mut fast_random,
                    *first_octave,
                    amplitudes,
                );
                let value = fast_noise.get_value(
                    f64::from(pos.x) * scale / f64::from(variety),
                    f64::from(pos.y) * scale / f64::from(variety),
                    f64::from(pos.z) * scale / f64::from(variety),
                );
                states.get(noise_state_index(value, states.len())).copied()
            }
            BlockStateProvider::RandomizedInt { source, values } => {
                let total: i32 = source.iter().map(|(weight, _)| *weight).sum();
                let mut roll = random.next_int_bounded(total.max(1));
                let states = source.iter().find_map(|(weight, states)| {
                    roll -= *weight;
                    (roll < 0).then_some(states)
                }).or_else(|| source.last().map(|(_, states)| states))?;
                let value = values.sample(random);
                match values {
                    IntProvider::Constant(_) => states.first().copied(),
                    IntProvider::Uniform { min, .. } => states.get((value - min) as usize).copied(),
                    _ => None,
                }
            }
            BlockStateProvider::RuleBased { rules, fallback } => {
                for (predicate, then) in rules {
                    if predicate.test(grid, tags, pos) {
                        return then.get_state(grid, tags, random, pos);
                    }
                }
                fallback
                    .as_ref()
                    .and_then(|f| f.get_state(grid, tags, random, pos))
            }
        }
    }

    /// [`Self::get_state`] already returns the canonical id the grid stores.
pub(super)     fn get_state_id<R: RandomSource>(
        &self,
        grid: &VegGrid,
        tags: &VegTags,
        random: &mut R,
        pos: BlockPos,
    ) -> Option<CanonicalStateId> {
        let state = self.get_state(grid, tags, random, pos)?;
        Some(state)
    }
}

fn replace_state_property(state: &str, property: &str, value: i32) -> String {
    let needle = format!("{property}=");
    let Some(start) = state.find(&needle) else { return state.to_string() };
    let value_start = start + needle.len();
    let value_end = state[value_start..]
        .find([',', ']'])
        .map_or(state.len(), |offset| value_start + offset);
    let mut out = state.to_string();
    out.replace_range(value_start..value_end, &value.to_string());
    out
}

fn noise_state_index(value: f64, state_count: usize) -> usize {
    if state_count == 0 {
        return 0;
    }
    (((value + 1.0) * 0.5 * state_count as f64) as usize).min(state_count - 1)
}

/// Registry-backed vegetation tags, resolved once at generator construction
/// via [`crate::block_tag::resolve_block_tag`] — the same tag-closure machinery
/// the ore rule tests already use for `RuleTest::TagMatch`, applied here to every tag this module's own
/// predicates/checks reference.
#[derive(Debug, Default, Clone)]
pub struct VegTags {
    pub(crate) cannot_replace_below_tree_trunk: BlockMask,
    pub(crate) supports_vegetation: BlockMask,
    pub(crate) replaceable_by_trees: BlockMask,
    pub(crate) logs: BlockMask,
    /// `#minecraft:supports_cactus` — the cactus block's own survival check's
    /// below-block check (cactus/block-column feature, added alongside sugar cane).
    pub(crate) supports_cactus: BlockMask,
    /// `#minecraft:supports_sugar_cane` — the sugar cane block's own survival
    /// check's below-block check. The adjacency-to-water half of that same check
    /// is *not* modelled here; it doesn't need to be, because every biome's
    /// own `patch_sugar_cane*` placed-feature JSON already encodes it as an
    /// explicit sibling `any_of`/`matching_fluids` predicate — see
    /// [`BlockPredicate::MatchingFluid`].
    pub(crate) supports_sugar_cane: BlockMask,
    /// `#minecraft:leaves` — the air-or-leaves check, the anchor gate the dark
    /// oak trunk placer checks before attempting each 2×2 log layer
    /// (a dark oak trunk can grow up through a neighbour's already-placed
    /// canopy; dense dark forests depend on that).
    pub(crate) leaves: BlockMask,
    /// `#minecraft:mangrove_logs_can_grow_through`, for the `matching_block_tag`
    /// predicate.
    pub(crate) mangrove_logs_can_grow_through: BlockMask,
    /// `#minecraft:mangrove_roots_can_grow_through`, for the `matching_block_tag`
    /// predicate.
    pub(crate) mangrove_roots_can_grow_through: BlockMask,
    /// `#minecraft:huge_brown_mushroom_can_place_on` — the exact floor gate
    /// in the bundled brown mushroom record.
    pub(crate) huge_brown_mushroom_can_place_on: BlockMask,
    /// `#minecraft:huge_red_mushroom_can_place_on` — the exact floor gate
    /// in the bundled red mushroom record.
    pub(crate) huge_red_mushroom_can_place_on: BlockMask,
    /// `#minecraft:replaceable_by_mushrooms` — the write target set for huge
    /// mushroom caps and stems. The feature's clearance check is narrower and
    /// accepts only air or leaves; this set is used after that check succeeds.
    pub(crate) replaceable_by_mushrooms: BlockMask,
    /// `#minecraft:supports_bamboo` — bamboo's floor survival rule.
    pub(crate) supports_bamboo: BlockMask,
    /// The dedicated floor tag for dry grass and dead bushes.
    pub(crate) supports_dry_vegetation: BlockMask,
    /// The dedicated floor tag for azalea bushes.
    pub(crate) supports_azalea: BlockMask,
    /// The dedicated floor tag for crimson roots.
    pub(crate) supports_crimson_roots: BlockMask,
    /// The dedicated floor tag for the lower half of small dripleaves.
    pub(crate) supports_small_dripleaf: BlockMask,
    /// The only two valid floor blocks for soul fire.
    pub(crate) soul_fire_base_blocks: BlockMask,
    /// Mushroom floors which bypass the raw-brightness check during decoration.
    pub(crate) overrides_mushroom_light_requirement: BlockMask,
    /// Non-fluid floors that can support lily pads.
    pub(crate) supports_lily_pad: BlockMask,
    /// Exact per-state solidity used by dungeon geometry and chest support
    /// checks. Unlike the older base-name vegetation helper, this preserves
    /// state properties whose shapes do not block the room.
    pub(crate) solid: StatePredicate,
    /// Exact canonical-state capability facts supplied by the version boundary.
    pub(crate) simple_block_support: SimpleBlockSupport,
    /// Ground accepted by the cave-root system's nested tree candidate.
    pub(crate) azalea_grows_on: BlockMask,
    /// Ground blocks accepted by giant-conifer ground alteration.
    pub(crate) beneath_tree_podzol_replaceable: BlockMask,
    /// Blocks that a live sculk spread may replace after the first charge
    /// reaches a substrate. This is the ordinary spread tag; world generation
    /// has a separate, slightly wider closure below.
    pub(crate) sculk_replaceable: BlockMask,
    /// Blocks the world-generation sculk spread may replace. Keeping this
    /// separate from [`Self::sculk_replaceable`] is load-bearing: the two
    /// tag closures intentionally differ for world-gen-only substrate.
    pub(crate) sculk_replaceable_world_gen: BlockMask,
    /// The same membership questions as the sets above, as bitsets
    /// indexed by canonical `StateId` — see [`super::ids`] for the whole
    /// design, including why the sets above must not be mutated after
    /// [`Self::bind`] has run.
    ///
    /// Not `pub`: it is a derived cache of this struct's own public sets, and a
    /// caller that could reach it could desynchronise it. Callers ask through
    /// [`Self::bind`] (once per pass) and [`super::ids::tag_at`] (per query).
    pub(super) id_tags: IdTags,
}

/// State-shape and fire facts consumed by the simple-block survival dispatcher.
/// Each predicate is a complete canonical-state override map plus default-state
/// bases, produced from the version-specific block-state registry.
#[derive(Debug, Default, Clone)]
pub(crate) struct SimpleBlockSupport {
    pub(crate) solid_render: StatePredicate,
    pub(crate) sturdy_up: StatePredicate,
    pub(crate) center_support_down: StatePredicate,
    pub(crate) fire_flammable: StatePredicate,
}

impl SimpleBlockSupport {
    #[must_use]
    pub(crate) fn parse(facts: &Value) -> Self {
        Self {
            solid_render: StatePredicate::parse(&facts["solid_render"]),
            sturdy_up: StatePredicate::parse(&facts["sturdy_up"]),
            center_support_down: StatePredicate::parse(&facts["center_support_down"]),
            fire_flammable: StatePredicate::parse(&facts["fire_flammable"]),
        }
    }
}

/// Resolves [`VegTags`] from a [`Resolver`]. Empty sets (never a panic) if
/// the resolver has no data for a given tag id — matches every other
/// resolver method's "no data supplied" convention.
#[must_use]
pub(crate) fn build_veg_tags(resolver: &dyn Resolver) -> VegTags {
    let freeze_facts = resolver.block_freeze_facts();
    let resolve = |id: &str| {
        let mut out = HashSet::new();
        let mut seen = HashSet::new();
        crate::block_tag::resolve_block_tag(resolver, id, &mut out, &mut seen);
        out.into_iter()
            .filter_map(|name| Block::from_name(&name))
            .collect::<BlockMask>()
    };
    VegTags {
        cannot_replace_below_tree_trunk: resolve("minecraft:cannot_replace_below_tree_trunk"),
        supports_vegetation: resolve("minecraft:supports_vegetation"),
        replaceable_by_trees: resolve("minecraft:replaceable_by_trees"),
        logs: resolve("minecraft:logs"),
        supports_cactus: resolve("minecraft:supports_cactus"),
        supports_sugar_cane: resolve("minecraft:supports_sugar_cane"),
        leaves: resolve("minecraft:leaves"),
        mangrove_logs_can_grow_through: resolve("minecraft:mangrove_logs_can_grow_through"),
        mangrove_roots_can_grow_through: resolve("minecraft:mangrove_roots_can_grow_through"),
        huge_brown_mushroom_can_place_on: resolve("minecraft:huge_brown_mushroom_can_place_on"),
        huge_red_mushroom_can_place_on: resolve("minecraft:huge_red_mushroom_can_place_on"),
        replaceable_by_mushrooms: resolve("minecraft:replaceable_by_mushrooms"),
        supports_bamboo: resolve("minecraft:supports_bamboo"),
        supports_dry_vegetation: resolve("minecraft:supports_dry_vegetation"),
        supports_azalea: resolve("minecraft:supports_azalea"),
        supports_crimson_roots: resolve("minecraft:supports_crimson_roots"),
        supports_small_dripleaf: resolve("minecraft:supports_small_dripleaf"),
        soul_fire_base_blocks: resolve("minecraft:soul_fire_base_blocks"),
        overrides_mushroom_light_requirement: resolve("minecraft:overrides_mushroom_light_requirement"),
        supports_lily_pad: resolve("minecraft:supports_lily_pad"),
        solid: StatePredicate::parse(&freeze_facts["solid"]),
        simple_block_support: SimpleBlockSupport::parse(&resolver.block_survival_facts()),
        azalea_grows_on: resolve("minecraft:azalea_grows_on"),
        beneath_tree_podzol_replaceable: resolve("minecraft:beneath_tree_podzol_replaceable"),
        sculk_replaceable: resolve("minecraft:sculk_replaceable"),
        sculk_replaceable_world_gen: resolve("minecraft:sculk_replaceable_world_gen"),
        // The bitsets are populated lazily from the process-wide canonical state
        // table on the first decoration pass. See [`super::ids`].
        id_tags: IdTags::default(),
    }
}

/// The placement modifiers a bundled structure feature-pool element uses.
/// A placed feature naming any other modifier parses to an unsupported
/// feature as a whole, since dropping one modifier would move every position.
#[derive(Clone, Debug)]
pub(crate) enum VegPlacement {
    Count(IntProvider),
    RandomOffset {
        xz: IntProvider,
        y: IntProvider,
    },
    BlockPredicateFilter(BlockPredicate),
    /// Walks up or down until `target` matches.
    EnvironmentScan {
        /// `+1` for `up`, `-1` for `down`.
        dy: i32,
        target: BlockPredicate,
        allowed: BlockPredicate,
        max_steps: i32,
    },
}

/// Parses an `IntProvider` field, returning `None` rather than panicking on a
/// type this module does not model: nothing here may panic on data.
pub(super) fn try_parse_int_provider(v: &Value) -> Option<IntProvider> {
    match v {
        Value::Number(n) => Some(IntProvider::Constant(n.as_i64()? as i32)),
        Value::Object(_) => {
            let ty = v["type"].as_str().unwrap_or("minecraft:constant");
            match ty.strip_prefix("minecraft:").unwrap_or(ty) {
                "constant" => Some(IntProvider::Constant(v["value"].as_i64()? as i32)),
                "uniform" => Some(IntProvider::Uniform {
                    min: v["min_inclusive"].as_i64()? as i32,
                    max: v["max_inclusive"].as_i64()? as i32,
                }),
                "clamped" => Some(IntProvider::Clamped {
                    source: Box::new(try_parse_int_provider(v.get("source")?)?),
                    min: v["min_inclusive"].as_i64()? as i32,
                    max: v["max_inclusive"].as_i64()? as i32,
                }),
                // The REAL trapezoid-int sample (two draws, triangular),
                // not a `Uniform` stand-in — see `IntProvider::Trapezoid`'s
                // own doc comment on why the approximation this replaced
                // was a real bug, not just a shape simplification: it
                // changed how many `nextInt` calls this placement consumed,
                // desyncing every RNG draw after the first `random_offset`
                // from vanilla's own stream.
                "trapezoid" => Some(IntProvider::Trapezoid {
                    min: v["min"].as_i64()? as i32,
                    max: v["max"].as_i64()? as i32,
                    plateau: v["plateau"].as_i64().unwrap_or(0) as i32,
                }),
                "clamped_normal" => Some(IntProvider::ClampedNormal {
                    mean: v["mean"].as_f64()? as f32,
                    deviation: v["deviation"].as_f64()? as f32,
                    min: v["min_inclusive"].as_i64()? as i32,
                    max: v["max_inclusive"].as_i64()? as i32,
                }),
                "weighted_list" => {
                    let entries = v["distribution"]
                        .as_array()?
                        .iter()
                        .map(|e| {
                            Some((try_parse_int_provider(&e["data"])?, e["weight"].as_i64()? as i32))
                        })
                        .collect::<Option<Vec<_>>>()?;
                    Some(IntProvider::WeightedProviders(
                        entries
                            .into_iter()
                            .map(|(provider, weight)| (Box::new(provider), weight))
                            .collect(),
                    ))
                }
                "biased_to_bottom" => Some(IntProvider::BiasedToBottom {
                    min: v["min_inclusive"].as_i64()? as i32,
                    max: v["max_inclusive"].as_i64()? as i32,
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

impl VegPlacement {
    fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "count" => Some(VegPlacement::Count(try_parse_int_provider(&v["count"])?)),
            "random_offset" => Some(VegPlacement::RandomOffset {
                xz: try_parse_int_provider(&v["xz_spread"])?,
                y: try_parse_int_provider(&v["y_spread"])?,
            }),
            "block_predicate_filter" => Some(VegPlacement::BlockPredicateFilter(
                BlockPredicate::parse(&v["predicate"]),
            )),
            "environment_scan" => Some(VegPlacement::EnvironmentScan {
                dy: match v["direction_of_search"].as_str()? {
                    "up" => 1,
                    "down" => -1,
                    _ => return None,
                },
                target: BlockPredicate::parse(&v["target_condition"]),
                allowed: match v.get("allowed_search_condition") {
                    Some(p) if !p.is_null() => BlockPredicate::parse(p),
                    _ => BlockPredicate::True,
                },
                max_steps: v["max_steps"].as_i64()? as i32,
            }),
            _ => None,
        }
    }

    pub(super) fn get_positions<R: RandomSource>(
        &self,
        random: &mut R,
        pos: BlockPos,
        grid: &VegGrid,
        tags: &VegTags,
    ) -> Positions {
        match self {
            VegPlacement::Count(ip) => {
                let n = ip.sample(random);
                Positions::Repeat(pos, n.max(0))
            }
            VegPlacement::RandomOffset { xz, y } => {
                // Two independent samples of `xz` (x, then z), not one shared draw.
                let scatter_x = pos.x + xz.sample(random);
                let scatter_y = pos.y + y.sample(random);
                let scatter_z = pos.z + xz.sample(random);
                let out = BlockPos {
                    x: scatter_x,
                    y: scatter_y,
                    z: scatter_z,
                };
                Positions::One(out)
            }
            VegPlacement::BlockPredicateFilter(pred) => {
                let allowed = pred.test(grid, tags, pos);
                if allowed {
                    Positions::One(pos)
                } else {
                    Positions::None
                }
            }
            VegPlacement::EnvironmentScan {
                dy,
                target,
                allowed,
                max_steps,
            } => {
                let mut cur = pos;
                if !allowed.test(grid, tags, cur) {
                    return Positions::None;
                }
                for _ in 0..*max_steps {
                    if target.test(grid, tags, cur) {
                        return Positions::One(cur);
                    }
                    cur.y += dy;
                    if cur.y < grid.min_y || cur.y >= grid.min_y + grid.height {
                        return Positions::None;
                    }
                    if !allowed.test(grid, tags, cur) {
                        break;
                    }
                }
                if target.test(grid, tags, cur) {
                    Positions::One(cur)
                } else {
                    Positions::None
                }
            }
        }
    }
}

/// The positions one placement modifier emits for one input position,
/// without allocating. The draws happen inside `get_positions`, before this
/// is returned, so the depth-first walk over it keeps the draw order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Positions {
    /// The modifier filtered this position out.
    None,
    /// Exactly one position (possibly moved from the input).
    One(BlockPos),
    /// `n` copies of one position, from a count modifier. `n <= 0` means none.
    Repeat(BlockPos, i32),
}

/// The tree decorators this engine models. Any other type parses to
/// [`Decorator::Unsupported`] and places nothing, which also skips that
/// decorator's draws.
#[derive(Clone, Debug)]
pub(crate) enum Decorator {
    Beehive { probability: f32 },
    /// Replaces eligible terrain under a tree using fixed rounded patches and
    /// seeded perimeter probes.
    AlterGround {
        provider: BlockStateProvider,
    },
    /// A hanging vine on each of a log's four horizontal neighbours, one
    /// independent coin flip per side.
    TrunkVine,
    Unsupported,
}

impl Decorator {
    fn parse(v: &Value) -> Self {
        let ty = v["type"].as_str().unwrap_or("");
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "beehive" => Decorator::Beehive {
                probability: v["probability"].as_f64().unwrap_or(0.0) as f32,
            },
            "alter_ground" => match BlockStateProvider::try_parse(&v["provider"]) {
                Some(provider) => Decorator::AlterGround { provider },
                None => Decorator::Unsupported,
            },
            "trunk_vine" => Decorator::TrunkVine,
            _ => Decorator::Unsupported,
        }
    }

    pub(super) fn bind_states(&self) {
        match self {
            Self::AlterGround { provider } => provider.bind(),
            Self::Beehive { .. } | Self::TrunkVine | Self::Unsupported => {}
        }
    }
}

/// The reference feature-size base kind — both
/// subclasses a faithful implementation ships, each reachable from tree configs this module
/// implements: [`Self::TwoLayers`] (oak, birch, spruce, pine, acacia) and
/// [`Self::ThreeLayers`] (dark oak, pale oak — the 2×2-trunk species).
/// The two share the same size-at-height shape but
/// answer it differently: `TwoLayers` splits at `limit`; `ThreeLayers` splits
/// into lower/middle/upper bands using `upper_limit` measured down from the
/// tree's own height, which is why the caller must pass `tree_height`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FeatureSizeCfg {
    TwoLayers {
        limit: i32,
        lower_size: i32,
        upper_size: i32,
        /// The feature size's own min-clipped-height field — `fancy_oak`'s own `4` (added
        /// with the savanna/acacia increment). `None` for every other species' `two_layers_feature_size`
        /// (oak's straight branch, birch, spruce, pine, acacia), which is
        /// exactly a faithful implementation's own empty-optional default. The
        /// tree placer is the one place this is read: a tree
        /// clipped by an obstruction can still place a shorter version of
        /// itself when this is `Some` and the clip doesn't cut below it —
        /// every other species requires an UNCLIPPED height instead.
        min_clipped_height: Option<i32>,
    },
    ThreeLayers {
        limit: i32,
        upper_limit: i32,
        lower_size: i32,
        middle_size: i32,
        upper_size: i32,
        /// See [`Self::TwoLayers`]'s own field — no shipped
        /// `three_layers_feature_size` (dark oak, pale oak) sets this, but
        /// a faithful implementation's codec allows it on either subclass, so it is parsed
        /// here too rather than only where a config happens to use it.
        min_clipped_height: Option<i32>,
    },
}

impl FeatureSizeCfg {
    fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        let min_clipped_height = v.get("min_clipped_height").and_then(Value::as_i64).map(|n| n as i32);
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "two_layers_feature_size" => Some(Self::TwoLayers {
                limit: v.get("limit").and_then(Value::as_i64).unwrap_or(1) as i32,
                lower_size: v.get("lower_size").and_then(Value::as_i64).unwrap_or(0) as i32,
                upper_size: v.get("upper_size").and_then(Value::as_i64).unwrap_or(1) as i32,
                min_clipped_height,
            }),
            "three_layers_feature_size" => Some(Self::ThreeLayers {
                limit: v.get("limit").and_then(Value::as_i64).unwrap_or(1) as i32,
                upper_limit: v.get("upper_limit").and_then(Value::as_i64).unwrap_or(1) as i32,
                lower_size: v.get("lower_size").and_then(Value::as_i64).unwrap_or(0) as i32,
                middle_size: v.get("middle_size").and_then(Value::as_i64).unwrap_or(1) as i32,
                upper_size: v.get("upper_size").and_then(Value::as_i64).unwrap_or(1) as i32,
                min_clipped_height,
            }),
            _ => None,
        }
    }

    /// The feature size's own min-clipped-height accessor — see [`Self::TwoLayers`]'s own doc
    /// on the one caller that reads this.
pub(super)     fn min_clipped_height(&self) -> Option<i32> {
        match *self {
            Self::TwoLayers { min_clipped_height, .. } | Self::ThreeLayers { min_clipped_height, .. } => {
                min_clipped_height
            }
        }
    }

    /// The feature size's own size-at-height accessor. The `tree_height`
    /// argument only matters for `ThreeLayers` (the upper band is `yo >=
    /// treeHeight - upperLimit`); `TwoLayers` ignores it.
pub(super)     fn size_at_height(&self, tree_height: i32, y: i32) -> i32 {
        match *self {
            Self::TwoLayers { limit, lower_size, upper_size, .. } => {
                if y < limit {
                    lower_size
                } else {
                    upper_size
                }
            }
            Self::ThreeLayers { limit, upper_limit, lower_size, middle_size, upper_size, .. } => {
                if y < limit {
                    lower_size
                } else if y >= tree_height - upper_limit {
                    upper_size
                } else {
                    middle_size
                }
            }
        }
    }
}

/// The reference tree-configuration record.
#[derive(Clone, Debug)]
pub(crate) struct TreeConfig {
pub(super)     below_trunk_provider: Option<BlockStateProvider>,
pub(super)     trunk_provider: BlockStateProvider,
pub(super)     foliage_provider: BlockStateProvider,
pub(super)     trunk_placer: TrunkPlacerCfg,
pub(super)     foliage_placer: FoliagePlacerCfg,
pub(super)     feature_size: FeatureSizeCfg,
pub(super)     decorators: Vec<Decorator>,
}

impl TreeConfig {
    pub(super) fn bind_states(&self) {
        if let Some(provider) = &self.below_trunk_provider {
            provider.bind();
        }
        self.trunk_provider.bind();
        self.foliage_provider.bind();
        for decorator in &self.decorators {
            decorator.bind_states();
        }
    }

    /// `None` if any required sub-part (trunk placer, foliage placer,
    /// feature size, trunk/foliage provider) is a kind this module doesn't
    /// implement — see module doc on why that must degrade rather than
    /// panic. `below_trunk_provider`/`decorators` degrade individually
    /// instead (a missing/unsupported one just does less, it doesn't sink
    /// the whole tree). A present root placer also fails the whole config:
    /// none is modelled, and dropping one would float the trunk at the wrong
    /// origin with no roots under it.
    fn try_parse(cfg: &Value) -> Option<Self> {
        let trunk_provider = BlockStateProvider::try_parse(&cfg["trunk_provider"])?;
        let foliage_provider = BlockStateProvider::try_parse(&cfg["foliage_provider"])?;
        let trunk_placer = TrunkPlacerCfg::try_parse(&cfg["trunk_placer"])?;
        let foliage_placer = FoliagePlacerCfg::try_parse(&cfg["foliage_placer"])?;
        let feature_size = FeatureSizeCfg::try_parse(&cfg["minimum_size"])?;
        let below_trunk_provider = cfg
            .get("below_trunk_provider")
            .and_then(BlockStateProvider::try_parse);
        let decorators = cfg
            .get("decorators")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().map(Decorator::parse).collect())
            .unwrap_or_default();
        if cfg.get("root_placer").is_some_and(|r| !r.is_null()) {
            return None;
        }
        Some(Self {
            below_trunk_provider,
            trunk_provider,
            foliage_provider,
            trunk_placer,
            foliage_placer,
            feature_size,
            decorators,
        })
    }
}

/// The reference block-column configuration and feature, backing
/// `cactus` (desert) and `sugar_cane` (desert/swamp/badlands/beach), both
/// previously a silent no-op under [`ConfiguredFeature::Unsupported`].
/// `direction` is `(dx, dy, dz)`; only `up`/`down` parse (every configured
/// feature in this crate's embedded data uses one of those two — see
/// [`BlockColumnConfig::try_parse`]'s doc), matching this module's blanket
/// "unsupported degrades, never panics" rule for anything else.
#[derive(Clone, Debug)]
pub(crate) struct BlockColumnConfig {
pub(super)     layers: Vec<(IntProvider, BlockStateProvider)>,
pub(super)     direction: (i32, i32, i32),
pub(super)     allowed_placement: BlockPredicate,
pub(super)     prioritize_tip: bool,
}

impl BlockColumnConfig {
    pub(super) fn bind_states(&self) {
        for (_, provider) in &self.layers {
            provider.bind();
        }
    }

    /// `direction` is a JSON string (`"up"`/`"down"`/four horizontal names);
    /// only the two vertical directions parse — the only two any
    /// `block_column` configured feature in `crates/lodestone-server/assets/worldgen`
    /// actually uses (`cactus.json`, `sugar_cane.json`, `cave_vine*.json`,
    /// `dripleaf.json`), checked at the time this was written. A horizontal
    /// direction degrades the whole feature to [`ConfiguredFeature::Unsupported`]
    /// rather than guessing.
    fn try_parse(v: &Value) -> Option<Self> {
        let layers = v["layers"]
            .as_array()?
            .iter()
            .map(|l| {
                let height = try_parse_int_provider(&l["height"])?;
                let provider = BlockStateProvider::try_parse(&l["provider"])?;
                Some((height, provider))
            })
            .collect::<Option<Vec<_>>>()?;
        let direction = match v["direction"].as_str()? {
            "up" => (0, 1, 0),
            "down" => (0, -1, 0),
            _ => return None,
        };
        Some(Self {
            layers,
            direction,
            allowed_placement: BlockPredicate::parse(&v["allowed_placement"]),
            prioritize_tip: v["prioritize_tip"].as_bool().unwrap_or(false),
        })
    }
}

/// The configured-feature kinds a bundled structure feature-pool element
/// reaches. Any other type parses to [`Self::Unsupported`], which carries the
/// type string for diagnostics and places nothing.
#[derive(Clone, Debug)]
pub(crate) enum ConfiguredFeature {
    SimpleBlock(BlockStateProvider),
    Tree(Box<TreeConfig>),
    BlockColumn(Box<BlockColumnConfig>),
    BlockPile(BlockStateProvider),
    SculkPatch(Box<super::features::SculkPatchCfg>),
    /// The no-op feature: genuinely nothing, and distinct from
    /// [`ConfiguredFeature::Unsupported`].
    NoOp,
    Unsupported(String),
}

impl ConfiguredFeature {
    /// Binds all built-in provider states at the feature boundary. Placement
    /// then performs only canonical-id to local-id lookups.
    pub(super) fn bind_states(&self) {
        match self {
            Self::SimpleBlock(provider) | Self::BlockPile(provider) => provider.bind(),
            Self::Tree(cfg) => cfg.bind_states(),
            Self::BlockColumn(cfg) => cfg.bind_states(),
            Self::SculkPatch(_) | Self::NoOp | Self::Unsupported(_) => {}
        }
    }
}

/// The reference placed-feature record — an ordered
/// [`VegPlacement`] pipeline plus the [`ConfiguredFeature`] it terminates in.
/// Every reference to a placed feature (top-level biome step entry, or a
/// nested option inside a selector) resolves to one of these — a faithful
/// implementation's own placed-feature placement runs its *own* placement pipeline even when reached
/// as a selector's branch, and `place_placed_feature` reproduces that
/// uniformly rather than special-casing "top level" vs "nested".
#[derive(Clone, Debug)]
pub struct PlacedRef {
    pub(crate) placements: Vec<VegPlacement>,
    pub(crate) feature: Box<ConfiguredFeature>,
}

pub(super) fn unsupported_placed_ref(why: &str) -> PlacedRef {
    PlacedRef {
        placements: Vec::new(),
        feature: Box::new(ConfiguredFeature::Unsupported(why.to_string())),
    }
}

/// Resolves a `Holder<PlacedFeature>`-shaped JSON value — either a plain
/// string (a `placed_feature` registry id) or an inline `{feature,
/// placement}` object — into a [`PlacedRef`]. Never panics: any parse
/// failure anywhere in the (possibly deeply nested, selector-within-
/// selector) tree degrades the *innermost* failing node to
/// [`ConfiguredFeature::Unsupported`], per this module's blanket
/// "degrade, don't crash" rule.
#[must_use]
pub(crate) fn resolve_placed_feature_ref(resolver: &dyn Resolver, value: &Value) -> PlacedRef {
    match value {
        Value::String(id) => {
            let doc = resolver.placed_feature(id);
            if doc.is_null() {
                return unsupported_placed_ref("missing placed_feature data");
            }
            parse_placed_feature_doc(resolver, &doc)
        }
        Value::Object(_) => parse_placed_feature_doc(resolver, value),
        _ => unsupported_placed_ref("unexpected placed-feature ref shape"),
    }
}

pub(super) fn parse_placed_feature_doc(resolver: &dyn Resolver, doc: &Value) -> PlacedRef {
    let placements = match doc.get("placement").and_then(Value::as_array) {
        Some(arr) => match arr.iter().map(VegPlacement::try_parse).collect::<Option<Vec<_>>>() {
            Some(placements) => placements,
            None => return unsupported_placed_ref("unsupported placement modifier"),
        },
        None => Vec::new(),
    };
    let Some(feature_ref) = doc.get("feature") else {
        return unsupported_placed_ref("placed-feature doc missing 'feature'");
    };
    let feature = resolve_configured_feature_ref(resolver, feature_ref);
    PlacedRef {
        placements,
        feature: Box::new(feature),
    }
}

/// Resolves a `Holder<ConfiguredFeature>`-shaped JSON value the same way
/// [`resolve_placed_feature_ref`] resolves a placed-feature one.
#[must_use]
pub(crate) fn resolve_configured_feature_ref(resolver: &dyn Resolver, value: &Value) -> ConfiguredFeature {
    match value {
        Value::String(id) => {
            let doc = resolver.configured_feature(id);
            if doc.is_null() {
                return ConfiguredFeature::Unsupported("missing configured_feature data".into());
            }
            parse_configured_feature_doc(&doc)
        }
        Value::Object(_) => parse_configured_feature_doc(value),
        _ => ConfiguredFeature::Unsupported("unexpected configured-feature ref shape".into()),
    }
}

pub(super) fn parse_configured_feature_doc(doc: &Value) -> ConfiguredFeature {
    let ty = doc["type"].as_str().unwrap_or("");
    let short = ty.strip_prefix("minecraft:").unwrap_or(ty);
    match short {
        "simple_block" => match BlockStateProvider::try_parse(&doc["config"]["to_place"]) {
            Some(p) => ConfiguredFeature::SimpleBlock(p),
            None => ConfiguredFeature::Unsupported("simple_block: unsupported to_place".into()),
        },
        "tree" => match TreeConfig::try_parse(&doc["config"]) {
            Some(cfg) => ConfiguredFeature::Tree(Box::new(cfg)),
            None => ConfiguredFeature::Unsupported(
                "tree: unsupported trunk/foliage/size/provider".into(),
            ),
        },
        "block_column" => match BlockColumnConfig::try_parse(&doc["config"]) {
            Some(cfg) => ConfiguredFeature::BlockColumn(Box::new(cfg)),
            None => ConfiguredFeature::Unsupported(
                "block_column: unsupported layer/direction/predicate".into(),
            ),
        },
        "block_pile" => match BlockStateProvider::try_parse(&doc["config"]["state_provider"]) {
            Some(p) => ConfiguredFeature::BlockPile(p),
            None => ConfiguredFeature::Unsupported("block_pile: unsupported provider".into()),
        },
        "sculk_patch" => {
            let c = &doc["config"];
            match try_parse_int_provider(&c["extra_rare_growths"]) {
                Some(extra_rare_growths) => {
                    ConfiguredFeature::SculkPatch(Box::new(super::features::SculkPatchCfg {
                        charge_count: c["charge_count"].as_i64().unwrap_or(1) as i32,
                        amount_per_charge: c["amount_per_charge"].as_i64().unwrap_or(1) as i32,
                        spread_attempts: c["spread_attempts"].as_i64().unwrap_or(1) as i32,
                        growth_rounds: c["growth_rounds"].as_i64().unwrap_or(0) as i32,
                        spread_rounds: c["spread_rounds"].as_i64().unwrap_or(0) as i32,
                        extra_rare_growths,
                        catalyst_chance: c["catalyst_chance"].as_f64().unwrap_or(0.0) as f32,
                    }))
                }
                None => ConfiguredFeature::Unsupported(
                    "sculk_patch: unsupported extra_rare_growths".into(),
                ),
            }
        }
        "no_op" => ConfiguredFeature::NoOp,
        other => ConfiguredFeature::Unsupported(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::BlockPredicate;
    use crate::feature::BlockPos;
    use crate::feature::vegetation::grid::VegGrid;
    use crate::feature::vegetation::ids::Tag;
    use lodestone_data::block::Block;

    #[test]
    fn matching_fluids_accepts_the_registry_string_shape() {
        let predicate = BlockPredicate::parse(&serde_json::json!({
            "type": "minecraft:matching_fluids",
            "fluids": "minecraft:water"
        }));
        assert!(matches!(
            predicate,
            BlockPredicate::MatchingFluid { fluids, offset: (0, 0, 0) }
                if fluids == [Block::Water].into_iter().collect()
        ));
    }

    #[test]
    fn matching_block_tag_is_typed_at_parse_time_and_unknown_tags_fail_closed() {
        let known = BlockPredicate::parse(&serde_json::json!({
            "type": "minecraft:matching_block_tag",
            "tag": "minecraft:azalea_grows_on"
        }));
        assert!(matches!(
            known,
            BlockPredicate::MatchingBlockTag { tag: Some(Tag::AzaleaGrowsOn), offset: (0, 0, 0) }
        ));

        let unknown = BlockPredicate::parse(&serde_json::json!({
            "type": "minecraft:matching_block_tag",
            "tag": "example:unregistered"
        }));
        assert!(matches!(
            unknown,
            BlockPredicate::MatchingBlockTag { tag: None, offset: (0, 0, 0) }
        ));
        assert!(!unknown.test(
            &VegGrid::new(0, 4, 0, 0),
            &super::VegTags::default(),
            BlockPos { x: 0, y: 0, z: 0 },
        ));
    }

    #[test]
    fn has_sturdy_face_down_parser_keeps_the_ceiling_target() {
        let predicate = BlockPredicate::parse(&serde_json::json!({
            "type": "minecraft:has_sturdy_face",
            "direction": "down"
        }));
        assert!(matches!(
            predicate,
            BlockPredicate::HasSturdyFaceDown { offset: (0, 0, 0) }
        ));

        let unsupported_direction = BlockPredicate::parse(&serde_json::json!({
            "type": "minecraft:has_sturdy_face",
            "direction": "north"
        }));
        assert!(matches!(unsupported_direction, BlockPredicate::True));
    }

    #[test]
    fn has_sturdy_face_down_tests_the_state_at_the_scan_target() {
        let predicate = BlockPredicate::HasSturdyFaceDown { offset: (0, 0, 0) };
        let mut grid = VegGrid::new(0, 4, 0, 0);
        grid.seed_id(8, 3, 8, Block::Stone.default_state());
        let tags = super::VegTags::default();

        assert!(predicate.test(&grid, &tags, BlockPos { x: 8, y: 3, z: 8 }));
        assert!(!predicate.test(&grid, &tags, BlockPos { x: 8, y: 2, z: 8 }));

        let offset = BlockPredicate::HasSturdyFaceDown { offset: (0, 1, 0) };
        assert!(offset.test(&grid, &tags, BlockPos { x: 8, y: 2, z: 8 }));
    }
}

