//! The data layer vegetal decoration is parsed into: heightmap kinds, block
//! predicates, block-state providers, placement modifiers, decorators, and the
//! placed-feature/configured-feature resolution that turns a registry document into
//! one of them.
//!
//! Moved here verbatim from `feature/vegetation.rs` by U16 Phase B; see [`super`]'s own
//! module doc for the scope and the named approximations.

use std::collections::HashSet;

use serde_json::Value;

use crate::density::Resolver;
use crate::feature::{BlockPos, IntProvider};
use crate::rng::RandomSource;

use super::grid::VegGrid;
use super::grid::census::bump as census_bump;
use super::ids::{IdTags, Tag, tag_at};
use super::tree::{FoliagePlacerCfg, RootPlacerCfg, TrunkPlacerCfg};

/// The reference heightmap-type enum (the subset vegetal decoration references). See this
/// module's doc "Approximations, named" for why only two scans back all
/// five.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightmapKind {
    OceanFloor,
    OceanFloorWg,
    WorldSurface,
    WorldSurfaceWg,
    MotionBlocking,
}

impl HeightmapKind {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "OCEAN_FLOOR" => Some(Self::OceanFloor),
            "OCEAN_FLOOR_WG" => Some(Self::OceanFloorWg),
            "WORLD_SURFACE" => Some(Self::WorldSurface),
            "WORLD_SURFACE_WG" => Some(Self::WorldSurfaceWg),
            "MOTION_BLOCKING" | "MOTION_BLOCKING_NO_LEAVES" => Some(Self::MotionBlocking),
            _ => None,
        }
    }

    fn scan(self, grid: &VegGrid, x: i32, z: i32) -> i32 {
        match self {
            Self::OceanFloor | Self::OceanFloorWg => grid.height_ocean_floor(x, z),
            Self::WorldSurface | Self::WorldSurfaceWg | Self::MotionBlocking => {
                grid.height_world_surface(x, z)
            }
        }
    }
}

/// The reference block-predicate base kind (the
/// subset grass/flower/tree placement and the rule-based state provider use).
/// Unknown predicate types degrade to [`BlockPredicate::True`] (this
/// module's blanket "unsupported degrades, never panics" rule) — see this
/// module's doc comment for why that must never be a panic.
#[derive(Clone, Debug)]
pub enum BlockPredicate {
    True,
    Solid,
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
    MatchingBlockTag(String),
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
        blocks: Vec<String>,
        offset: (i32, i32, i32),
    },
    /// The matching-fluid predicate — `fluids` is the JSON's raw
    /// `minecraft:water`/`minecraft:flowing_water`/`minecraft:lava`/
    /// `minecraft:flowing_lava` id list; `offset` is `(dx, dy, dz)` added to
    /// the tested position. Matched via [`fluid_base_matches`] because this
    /// engine's grid never distinguishes a fluid's source/flowing variant
    /// (the same "known representation gap: fluid `level`"
    /// `docs/worldgen-parity.md` already names) — both JSON ids for one
    /// fluid collapse onto the one base id our grid can ever hold.
    MatchingFluid {
        fluids: Vec<String>,
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
pub(super) fn fluid_tag_of(fluid_id: &str) -> Option<Tag> {
    match fluid_id {
        "minecraft:water" | "minecraft:flowing_water" => Some(Tag::Water),
        "minecraft:lava" | "minecraft:flowing_lava" => Some(Tag::Lava),
        _ => None,
    }
}

impl BlockPredicate {
    fn parse(v: &Value) -> Self {
        let ty = v["type"].as_str().unwrap_or("minecraft:true");
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "solid" => BlockPredicate::Solid,
            "not" => BlockPredicate::Not(Box::new(BlockPredicate::parse(&v["predicate"]))),
            "all_of" => BlockPredicate::AllOf(parse_predicate_list(v)),
            "any_of" => BlockPredicate::AnyOf(parse_predicate_list(v)),
            "matching_block_tag" => {
                BlockPredicate::MatchingBlockTag(v["tag"].as_str().unwrap_or_default().to_string())
            }
            "matching_blocks" => BlockPredicate::MatchingBlocks {
                blocks: parse_id_list(&v["blocks"]),
                offset: parse_offset(&v["offset"]),
            },
            "matching_fluids" => BlockPredicate::MatchingFluid {
                fluids: v["fluids"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|f| f.as_str().map(str::to_string)).collect())
                    .unwrap_or_default(),
                offset: parse_offset(&v["offset"]),
            },
            "would_survive" => match v["state"]["Name"].as_str().unwrap_or("") {
                "minecraft:cactus" => BlockPredicate::WouldSurviveCactus,
                "minecraft:sugar_cane" => BlockPredicate::WouldSurviveSugarCane,
                _ => BlockPredicate::WouldSurviveOnSupportsVegetation,
            },
            _ => BlockPredicate::True,
        }
    }

pub(super)     fn test(&self, grid: &VegGrid, tags: &VegTags, pos: BlockPos) -> bool {
        match self {
            BlockPredicate::True => true,
            BlockPredicate::Solid => {
                let base = super::base_id(grid.get(pos.x, pos.y, pos.z));
                !is_air(base) && !is_fluid(base) && blocks_motion(base)
            }
            BlockPredicate::Not(inner) => !inner.test(grid, tags, pos),
            BlockPredicate::AllOf(list) => list.iter().all(|p| p.test(grid, tags, pos)),
            BlockPredicate::AnyOf(list) => list.iter().any(|p| p.test(grid, tags, pos)),
            BlockPredicate::MatchingBlockTag(tag) => {
                // The JSON tag name is a per-feature constant, so matching it as
                // a string costs nothing per attempt; what used to cost is the
                // *state* side, now a bit test. Unit 8.
                if tag == "minecraft:air" {
                    tag_at(grid, tags, Tag::Air, pos.x, pos.y, pos.z)
                } else if tag == "minecraft:cannot_replace_below_tree_trunk" {
                    tag_at(grid, tags, Tag::CannotReplaceBelowTreeTrunk, pos.x, pos.y, pos.z)
                } else if tag == "minecraft:huge_brown_mushroom_can_place_on" {
                    tag_at(grid, tags, Tag::HugeBrownMushroomCanPlaceOn, pos.x, pos.y, pos.z)
                } else if tag == "minecraft:huge_red_mushroom_can_place_on" {
                    tag_at(grid, tags, Tag::HugeRedMushroomCanPlaceOn, pos.x, pos.y, pos.z)
                } else if tag == "minecraft:replaceable_by_trees" {
                    tag_at(grid, tags, Tag::ReplaceableByTrees, pos.x, pos.y, pos.z)
                } else if tag == "minecraft:azalea_grows_on" {
                    tags.azalea_grows_on.contains(super::base_id(grid.get(pos.x, pos.y, pos.z)))
                } else {
                    false
                }
            }
            BlockPredicate::MatchingBlocks { blocks, offset } => {
                let (dx, dy, dz) = *offset;
                let base = super::base_id(grid.get(pos.x + dx, pos.y + dy, pos.z + dz));
                blocks.iter().any(|b| b == base)
            }
            BlockPredicate::MatchingFluid { fluids, offset } => {
                let (dx, dy, dz) = *offset;
                let id = grid.get_id(pos.x + dx, pos.y + dy, pos.z + dz);
                fluids.iter().any(|f| {
                    fluid_tag_of(f).is_some_and(|tag| tags.has(grid.interner(), tag, id))
                })
            }
            BlockPredicate::WouldSurviveOnSupportsVegetation => {
                tag_at(grid, tags, Tag::SupportsVegetation, pos.x, pos.y - 1, pos.z)
            }
            BlockPredicate::WouldSurviveCactus => {
                let below = grid.get_id(pos.x, pos.y - 1, pos.z);
                if !tags.has(grid.interner(), Tag::Cactus, below)
                    && !tags.has(grid.interner(), Tag::SupportsCactus, below)
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
                tags.has(grid.interner(), Tag::SugarCane, below)
                    || tags.has(grid.interner(), Tag::SupportsSugarCane, below)
            }
        }
    }
}

/// The three air states, by **base** name.
///
/// Since Unit 8 almost every caller asks this of a [`crate::interner::StateId`]
/// via [`Tag::Air`] instead. This function survives as the *definition* those
/// bits are filled from ([`super::ids`]'s `member`), so there is exactly one
/// place that decides what counts as air — a second `matches!` inlined next to
/// the bitset fill would be free to drift from this one, and nothing would fail.
///
/// **Air states carry no block-state properties**, which is what lets
/// [`VegGrid`]'s heightmap scans test air by comparing against three cached ids
/// rather than resolving a name: for air, `base_id(name) == name`, so
/// "base is one of three names" and "id is one of three ids" are the same
/// question. Do not extend this list with a state that *does* carry properties
/// without revisiting `VegGrid::is_air_id`.
#[must_use]
pub fn is_air(base: &str) -> bool {
    matches!(
        base,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

/// The two fluid states, by **base** name — so `minecraft:water[level=0]` (which
/// `crate::carver` really does write) counts.
///
/// As with [`is_air`], this is now the definition [`Tag::Fluid`]'s bits are
/// filled from rather than a hot-path call. The one remaining hot caller is
/// [`VegGrid::height_ocean_floor`], which reaches it only for cells it has
/// already established are not air — a handful per probe instead of the whole
/// column.
#[must_use]
pub fn is_fluid(base: &str) -> bool {
    matches!(base, "minecraft:water" | "minecraft:lava")
}

/// A block state's own blocks-motion flag — the predicate
/// the ocean-floor/motion-blocking heightmap kinds actually test, as opposed to
/// "is not air and is not a fluid".
///
/// # Why this exists: stacked, floating seagrass
///
/// [`VegGrid::height_ocean_floor`] used to answer "topmost non-air, non-fluid",
/// and seagrass is neither air nor fluid — so **an already-placed seagrass counted
/// as the ocean floor**, and the next `seagrass` placement on the same column
/// started one block higher and stacked on top of it. Two blocks is the plant's
/// maximum height, so the result read as seagrass floating in open water. It needed
/// two placements to land on one column, which is why it was intermittent.
///
/// A faithful implementation has no such problem: the ocean-floor heightmap tests
/// the blocks-motion flag, seagrass does
/// not block motion, so every placement on a column resolves to the same real
/// floor. The heightmap scans here read the *currently mutating* grid by design
/// (that is what gives a later feature write-visibility of an earlier one), which is
/// exactly what let a wrong predicate compound instead of merely being wrong once.
///
/// # What this is, given there is no blocks-motion table in this crate
///
/// The blocks-motion flag is a per-block properties flag, per block, and this crate
/// carries no per-block-state property table at all — `lodestone-data` has the
/// collision shapes but wiring that dependency in here is a bigger change than the
/// defect. So this is a **deny-list**, and the default direction is deliberate:
/// **anything not listed blocks motion**, which is byte-for-byte the previous
/// behaviour. Only the listed states change, so the ripple is bounded to blocks
/// this engine can actually write.
///
/// The list is the 26 non-air, non-fluid members of vanilla's own
/// `#minecraft:replaceable` tag (read from
/// `assets/worldgen/../tags/block/replaceable.json`, not from memory) plus the
/// non-motion-blocking states the decoration engine places that the tag happens not
/// to include — kelp, sea pickles, sugar cane, the flower set, nether vines and
/// mushrooms. To extend it, add the state and say where you checked.
#[must_use]
pub fn blocks_motion(base: &str) -> bool {
    !matches!(
        base,
        // `#minecraft:replaceable`, minus air and the two fluids (callers test
        // those separately and more cheaply).
        "minecraft:short_grass"
            | "minecraft:fern"
            | "minecraft:dead_bush"
            | "minecraft:bush"
            | "minecraft:short_dry_grass"
            | "minecraft:tall_dry_grass"
            | "minecraft:seagrass"
            | "minecraft:tall_seagrass"
            | "minecraft:fire"
            | "minecraft:soul_fire"
            | "minecraft:snow"
            | "minecraft:vine"
            | "minecraft:glow_lichen"
            | "minecraft:resin_clump"
            | "minecraft:light"
            | "minecraft:tall_grass"
            | "minecraft:large_fern"
            | "minecraft:structure_void"
            | "minecraft:bubble_column"
            | "minecraft:warped_roots"
            | "minecraft:nether_sprouts"
            | "minecraft:crimson_roots"
            | "minecraft:leaf_litter"
            | "minecraft:hanging_roots"
            // Placed by this engine, non-motion-blocking, absent from that tag.
            | "minecraft:kelp"
            | "minecraft:kelp_plant"
            | "minecraft:sea_pickle"
            | "minecraft:sugar_cane"
            | "minecraft:twisting_vines"
            | "minecraft:twisting_vines_plant"
            | "minecraft:weeping_vines"
            | "minecraft:weeping_vines_plant"
            | "minecraft:sculk_vein"
            | "minecraft:brown_mushroom"
            | "minecraft:red_mushroom"
            | "minecraft:crimson_fungus"
            | "minecraft:warped_fungus"
            | "minecraft:nether_wart"
            | "minecraft:dandelion"
            | "minecraft:poppy"
            | "minecraft:blue_orchid"
            | "minecraft:allium"
            | "minecraft:azure_bluet"
            | "minecraft:red_tulip"
            | "minecraft:orange_tulip"
            | "minecraft:white_tulip"
            | "minecraft:pink_tulip"
            | "minecraft:oxeye_daisy"
            | "minecraft:cornflower"
            | "minecraft:lily_of_the_valley"
            | "minecraft:wither_rose"
            | "minecraft:torchflower"
            | "minecraft:closed_eyeblossom"
            | "minecraft:open_eyeblossom"
            | "minecraft:pink_petals"
            | "minecraft:wildflowers"
            | "minecraft:sunflower"
            | "minecraft:lilac"
            | "minecraft:rose_bush"
            | "minecraft:peony"
            | "minecraft:spore_blossom"
            | "minecraft:cave_vines"
            | "minecraft:cave_vines_plant"
    )
}

/// The reference block-state-provider base kind
/// (the subset grass/flower/tree configs use). Parsing degrades to `None`
/// on an unsupported provider type or a sub-provider that itself failed to
/// parse — see module doc.
#[derive(Clone, Debug)]
pub enum BlockStateProvider {
    Simple(String),
    /// `(weight, state)` pairs, declaration order (matches
    /// [`IntProvider::WeightedList`]'s own walk).
    Weighted(Vec<(i32, String)>),
    NoiseThreshold {
        seed: i64,
        first_octave: i32,
        amplitudes: Vec<f64>,
        scale: f64,
        threshold: f64,
        high_chance: f32,
        default_state: String,
        low_states: Vec<String>,
        high_states: Vec<String>,
    },
    /// Selects one of its configured states from deterministic normal noise.
    Noise {
        seed: i64,
        first_octave: i32,
        amplitudes: Vec<f64>,
        scale: f64,
        states: Vec<String>,
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
        states: Vec<String>,
    },
    RandomizedInt {
        source: Vec<(i32, Vec<String>)>,
        values: IntProvider,
    },
    RuleBased {
        rules: Vec<(BlockPredicate, Box<BlockStateProvider>)>,
        fallback: Option<Box<BlockStateProvider>>,
    },
}

pub(super) fn canon_state(v: &Value) -> String {
    crate::feature::canon_state(v)
}

impl BlockStateProvider {
pub(super)     fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "simple_state_provider" => Some(BlockStateProvider::Simple(canon_state(&v["state"]))),
            "weighted_state_provider" => {
                let entries = v["entries"].as_array()?;
                let parsed = entries
                    .iter()
                    .map(|e| {
                        let weight = e["weight"].as_i64().unwrap_or(1) as i32;
                        (weight, canon_state(&e["data"]))
                    })
                    .collect();
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
                default_state: canon_state(&v["default_state"]),
                low_states: v["low_states"]
                    .as_array()?
                    .iter()
                    .map(canon_state)
                    .collect(),
                high_states: v["high_states"]
                    .as_array()?
                    .iter()
                    .map(canon_state)
                    .collect(),
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
                    .map(canon_state)
                    .collect(),
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
                    .map(canon_state)
                .collect(),
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
                                (value_min..=value_max)
                                    .map(move |value| replace_state_property(&state, property, value))
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
pub(super)     fn get_state<'a, R: RandomSource>(
        &'a self,
        grid: &VegGrid,
        tags: &VegTags,
        random: &mut R,
        pos: BlockPos,
    ) -> Option<&'a str> {
        match self {
            BlockStateProvider::Simple(state) => Some(state.as_str()),
            BlockStateProvider::Weighted(entries) => {
                let total: i32 = entries.iter().map(|(w, _)| *w).sum();
                let mut roll = random.next_int_bounded(total.max(1));
                for (weight, state) in entries {
                    roll -= *weight;
                    if roll < 0 {
                        return Some(state.as_str());
                    }
                }
                entries.last().map(|(_, s)| s.as_str())
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
                    Some(low_states.get(idx).unwrap_or(default_state).as_str())
                } else if random.next_float() < *high_chance {
                    let idx = random.next_int_bounded(high_states.len().max(1) as i32) as usize;
                    Some(high_states.get(idx).unwrap_or(default_state).as_str())
                } else {
                    Some(default_state.as_str())
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
                states.get(noise_state_index(value, states.len())).map(String::as_str)
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
                states.get(noise_state_index(value, states.len())).map(String::as_str)
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
                    IntProvider::Constant(_) => states.first().map(String::as_str),
                    IntProvider::Uniform { min, .. } => states.get((value - min) as usize).map(String::as_str),
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

    /// [`Self::get_state`] resolved to the id the grid actually stores.
    ///
    /// The interner lookup here is not new cost: `VegGrid::set_if_in_bounds`
    /// already performed exactly this `id_of` on the `String` it was handed, so
    /// Unit 8 removed the allocation and left the lookup where it was. It is the
    /// next lever if the shared read guard ever shows up in a profile — the fix
    /// would be caching the resolved id in the provider, which needs a
    /// `Sync` interior-mutability cell and a per-interner guard, and is not worth
    /// it until measured.
pub(super)     fn get_state_id<R: RandomSource>(
        &self,
        grid: &VegGrid,
        tags: &VegTags,
        random: &mut R,
        pos: BlockPos,
    ) -> Option<crate::interner::StateId> {
        let name = self.get_state(grid, tags, random, pos)?;
        Some(grid.interner().id_of(name))
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

/// `#minecraft:cannot_replace_below_tree_trunk`/`#minecraft:supports_vegetation`/
/// `#minecraft:replaceable_by_trees`/`#minecraft:logs`, resolved once at
/// generator construction via [`crate::compose::resolve_block_tag`] — the
/// same tag-closure machinery [`crate::compose::build_ore_tag_map`] already
/// uses for ore `RuleTest::TagMatch`, applied here to the four tags this
/// module's own predicates/checks reference.
#[derive(Debug, Default, Clone)]
pub struct VegTags {
    pub cannot_replace_below_tree_trunk: HashSet<String>,
    pub supports_vegetation: HashSet<String>,
    pub replaceable_by_trees: HashSet<String>,
    pub logs: HashSet<String>,
    /// `#minecraft:supports_cactus` — the cactus block's own survival check's
    /// below-block check (cactus/block-column feature, added alongside sugar cane).
    pub supports_cactus: HashSet<String>,
    /// `#minecraft:supports_sugar_cane` — the sugar cane block's own survival
    /// check's below-block check. The adjacency-to-water half of that same check
    /// is *not* modelled here; it doesn't need to be, because every biome's
    /// own `patch_sugar_cane*` placed-feature JSON already encodes it as an
    /// explicit sibling `any_of`/`matching_fluids` predicate — see
    /// [`BlockPredicate::MatchingFluid`].
    pub supports_sugar_cane: HashSet<String>,
    /// `#minecraft:leaves` — the air-or-leaves check, the anchor gate
    /// [`place_dark_oak_trunk`] checks before attempting each 2×2 log layer
    /// (a dark oak trunk can grow up through a neighbour's already-placed
    /// canopy; dense dark forests depend on that).
    pub leaves: HashSet<String>,
    /// `#minecraft:mangrove_logs_can_grow_through` —
    /// [`TrunkPlacerCfg::UpwardsBranching`]'s extra valid-tree-position OR-arm.
    pub mangrove_logs_can_grow_through: HashSet<String>,
    /// `#minecraft:mangrove_roots_can_grow_through` — [`RootPlacerCfg::Mangrove`]'s
    /// can-place-root extra OR-arm.
    pub mangrove_roots_can_grow_through: HashSet<String>,
    /// `#minecraft:huge_brown_mushroom_can_place_on` — the exact floor gate
    /// in the bundled brown mushroom record.
    pub huge_brown_mushroom_can_place_on: HashSet<String>,
    /// `#minecraft:huge_red_mushroom_can_place_on` — the exact floor gate
    /// in the bundled red mushroom record.
    pub huge_red_mushroom_can_place_on: HashSet<String>,
    /// `#minecraft:supports_bamboo` — bamboo's floor survival rule.
    pub supports_bamboo: HashSet<String>,
    /// Ground accepted by the cave-root system's nested tree candidate.
    pub azalea_grows_on: HashSet<String>,
    /// Ground blocks replaced by bamboo's optional podzol disk.
    pub beneath_bamboo_podzol_replaceable: HashSet<String>,
    /// Unit 8: the same membership questions as the sets above, as bitsets
    /// indexed by [`crate::interner::StateId`] — see [`super::ids`] for the whole
    /// design, including why the sets above must not be mutated after
    /// [`Self::bind`] has run.
    ///
    /// Not `pub`: it is a derived cache of this struct's own public sets, and a
    /// caller that could reach it could desynchronise it. Callers ask through
    /// [`Self::bind`] (once per pass) and [`super::ids::tag_at`] (per query).
    pub(super) id_tags: IdTags,
}

/// Resolves [`VegTags`] from a [`Resolver`]. Empty sets (never a panic) if
/// the resolver has no data for a given tag id — matches every other
/// resolver method's "no data supplied" convention.
#[must_use]
pub fn build_veg_tags(resolver: &dyn Resolver) -> VegTags {
    let resolve = |id: &str| {
        let mut out = HashSet::new();
        let mut seen = HashSet::new();
        crate::compose::resolve_block_tag(resolver, id, &mut out, &mut seen);
        out
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
        supports_bamboo: resolve("minecraft:supports_bamboo"),
        azalea_grows_on: resolve("minecraft:azalea_grows_on"),
        beneath_bamboo_podzol_replaceable: resolve("minecraft:beneath_bamboo_podzol_replaceable"),
        // Unbound: the bitsets are per-interner and the interner does not exist
        // yet at generator-construction time. The decoration driver binds them
        // once per pass. See [`super::ids`].
        id_tags: IdTags::default(),
    }
}

/// The reference placement-modifier base kind (the
/// vegetal-decoration subset). A separate type from [`super::Placement`]
/// (the ore engine's) rather than an extension of it — the two engines share
/// no placement instances and vegetal decoration needs live grid reads
/// (heightmap, air/tag checks) the ore engine's modifiers never did, so
/// giving them their own `get_positions` signature avoids retrofitting a
/// grid parameter onto ore's already-proven, already-tested type.
#[derive(Clone, Debug)]
pub enum VegPlacement {
    Count(IntProvider),
    InSquare,
    Heightmap(HeightmapKind),
    Biome,
    RarityFilter(i32),
    SurfaceWaterDepthFilter(i32),
    /// The noise-threshold count placement — a biome-info-noise gated count.
    NoiseThresholdCount {
        noise_level: f64,
        below: i32,
        above: i32,
    },
    RandomOffset {
        xz: IntProvider,
        y: IntProvider,
    },
    BlockPredicateFilter(BlockPredicate),
    // --- the five modifiers neither engine had, plus `height_range`,
    // which existed only in the ore engine. 86 of the bundled placed features use
    // `height_range`, so before this every one of them reached a decoration step
    // and was silently dropped.
    HeightRange(crate::feature::HeightProvider),
    /// The count-on-every-layer placement — fans out to one position per air/solid
    /// interface, layer by layer, until a layer produces nothing.
    CountOnEveryLayer(IntProvider),
    /// The environment-scan placement — walks up or down until `target` matches.
    EnvironmentScan {
        /// `+1` for `up`, `-1` for `down`.
        dy: i32,
        target: BlockPredicate,
        allowed: BlockPredicate,
        max_steps: i32,
    },
    /// The noise-based count placement.
    NoiseBasedCount {
        noise_to_count_ratio: i32,
        noise_factor: f64,
        noise_offset: f64,
    },
    /// `SurfaceRelativeThresholdFilter`.
    SurfaceRelativeThresholdFilter {
        heightmap: HeightmapKind,
        min_inclusive: i32,
        max_inclusive: i32,
    },
    /// `FixedPlacement` — the listed positions that fall in this chunk.
    FixedPlacement(Vec<BlockPos>),
}

/// Parses an `IntProvider` for a vegetal-decoration placement field without
/// risking [`IntProvider::parse`]'s panic on an unrecognised type — see
/// module doc on why nothing in this file may panic on data it doesn't yet
/// model. A dedicated, duplicated mini-parser (not a new
/// [`IntProvider::try_parse`] on the shared type) so the change carries zero
/// risk to the already-proven ore engine's parsing contract.
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
                // The REAL trapezoid-int sample (two draws, triangular),
                // not a `Uniform` stand-in — see `IntProvider::Trapezoid`'s
                // own doc comment on why the approximation this replaced
                // was a real bug, not just a shape simplification: it
                // changed how many `nextInt` calls this placement consumed,
                // desyncing every RNG draw after the first `random_offset`
                // from vanilla's own stream. Found via a real JVM oracle
                // (`tests/vegetation_parity.rs`), not by inspection.
                "trapezoid" => Some(IntProvider::Trapezoid {
                    min: v["min"].as_i64()? as i32,
                    max: v["max"].as_i64()? as i32,
                    plateau: v["plateau"].as_i64().unwrap_or(0) as i32,
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
            "in_square" => Some(VegPlacement::InSquare),
            "heightmap" => Some(VegPlacement::Heightmap(HeightmapKind::parse(
                v["heightmap"].as_str()?,
            )?)),
            "biome" => Some(VegPlacement::Biome),
            "rarity_filter" => Some(VegPlacement::RarityFilter(v["chance"].as_i64()? as i32)),
            "surface_water_depth_filter" => Some(VegPlacement::SurfaceWaterDepthFilter(
                v["max_water_depth"].as_i64()? as i32,
            )),
            "noise_threshold_count" => Some(VegPlacement::NoiseThresholdCount {
                noise_level: v["noise_level"].as_f64()?,
                below: v["below_noise"].as_i64()? as i32,
                above: v["above_noise"].as_i64()? as i32,
            }),
            "random_offset" => Some(VegPlacement::RandomOffset {
                xz: try_parse_int_provider(&v["xz_spread"])?,
                y: try_parse_int_provider(&v["y_spread"])?,
            }),
            "block_predicate_filter" => Some(VegPlacement::BlockPredicateFilter(
                BlockPredicate::parse(&v["predicate"]),
            )),
            "height_range" => Some(VegPlacement::HeightRange(
                crate::feature::HeightProvider::try_parse(&v["height"])?,
            )),
            "count_on_every_layer" => Some(VegPlacement::CountOnEveryLayer(
                try_parse_int_provider(&v["count"])?,
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
            "noise_based_count" => Some(VegPlacement::NoiseBasedCount {
                noise_to_count_ratio: v["noise_to_count_ratio"].as_i64()? as i32,
                noise_factor: v["noise_factor"].as_f64()?,
                noise_offset: v["noise_offset"].as_f64().unwrap_or(0.0),
            }),
            "surface_relative_threshold_filter" => {
                Some(VegPlacement::SurfaceRelativeThresholdFilter {
                    heightmap: HeightmapKind::parse(v["heightmap"].as_str()?)?,
                    min_inclusive: v["min_inclusive"].as_i64().unwrap_or(i64::from(i32::MIN)) as i32,
                    max_inclusive: v["max_inclusive"].as_i64().unwrap_or(i64::from(i32::MAX)) as i32,
                })
            }
            "fixed_placement" => {
                let positions = v["positions"]
                    .as_array()?
                    .iter()
                    .filter_map(|p| {
                        let arr = p.as_array()?;
                        Some(BlockPos {
                            x: arr.first()?.as_i64()? as i32,
                            y: arr.get(1)?.as_i64()? as i32,
                            z: arr.get(2)?.as_i64()? as i32,
                        })
                    })
                    .collect();
                Some(VegPlacement::FixedPlacement(positions))
            }
            _ => None,
        }
    }

pub(super)     fn get_positions<R: RandomSource>(
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
            VegPlacement::InSquare => {
                let x = pos.x + random.next_int_bounded(16);
                let z = pos.z + random.next_int_bounded(16);
                Positions::One(BlockPos { x, y: pos.y, z })
            }
            VegPlacement::Heightmap(kind) => {
                let height = kind.scan(grid, pos.x, pos.z);
                if height > grid.min_y {
                    Positions::One(BlockPos {
                        x: pos.x,
                        y: height,
                        z: pos.z,
                    })
                } else {
                    Positions::None
                }
            }
            VegPlacement::Biome => Positions::One(pos),
            VegPlacement::RarityFilter(chance) => {
                if random.next_float() < 1.0 / *chance as f32 {
                    Positions::One(pos)
                } else {
                    Positions::None
                }
            }
            VegPlacement::SurfaceWaterDepthFilter(max_depth) => {
                let ocean = grid.height_ocean_floor(pos.x, pos.z);
                let surface = grid.height_world_surface(pos.x, pos.z);
                if surface - ocean <= *max_depth {
                    Positions::One(pos)
                } else {
                    Positions::None
                }
            }
            VegPlacement::NoiseThresholdCount {
                noise_level,
                below,
                above,
            } => {
                let noise = crate::noise::biome_info_noise_value(
                    f64::from(pos.x) / 200.0,
                    f64::from(pos.z) / 200.0,
                );
                let n = if noise < *noise_level { *below } else { *above };
                Positions::Repeat(pos, n.max(0))
            }
            VegPlacement::RandomOffset { xz, y } => {
                // Two INDEPENDENT samples of `xz` (x, then z) — matches
                // the random-offset placement's own two separate
                // `this.xzSpread.sample(random)` calls, not one shared draw.
                let scatter_x = pos.x + xz.sample(random);
                let scatter_y = pos.y + y.sample(random);
                let scatter_z = pos.z + xz.sample(random);
                Positions::One(BlockPos {
                    x: scatter_x,
                    y: scatter_y,
                    z: scatter_z,
                })
            }
            VegPlacement::BlockPredicateFilter(pred) => {
                census_bump(|c| c.block_predicate_filter_in += 1);
                if pred.test(grid, tags, pos) {
                    census_bump(|c| c.block_predicate_filter_out += 1);
                    Positions::One(pos)
                } else {
                    Positions::None
                }
            }
            VegPlacement::HeightRange(hp) => {
                // `VerticalAnchor` resolves against the *generated* column, which
                // for this engine is the grid's own vertical extent — the same
                // (min_gen_y, gen_depth) pair the ore engine passes.
                let y = hp.sample(random, grid.min_y, grid.height);
                Positions::One(BlockPos { x: pos.x, y, z: pos.z })
            }
            VegPlacement::CountOnEveryLayer(ip) => {
                let mut out = Vec::new();
                let mut layer = 0;
                loop {
                    let mut found_any = false;
                    let n = ip.sample(random);
                    for _ in 0..n.max(0) {
                        let x = random.next_int_bounded(16) + pos.x;
                        let z = random.next_int_bounded(16) + pos.z;
                        let start_y = grid.height_world_surface(x, z);
                        if let Some(y) = find_on_ground_y(grid, tags, x, start_y, z, layer) {
                            out.push(BlockPos { x, y, z });
                            found_any = true;
                        }
                    }
                    layer += 1;
                    // The loop is `do { … } while (foundAny)`. `layer` is bounded
                    // by the column height in practice, but a grid that answered
                    // air/solid alternately forever would not terminate — cap it
                    // at the column height, which no real world can exceed.
                    if !found_any || layer > grid.height {
                        break;
                    }
                }
                Positions::from_vec(out)
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
            VegPlacement::NoiseBasedCount {
                noise_to_count_ratio,
                noise_factor,
                noise_offset,
            } => {
                let noise = crate::noise::biome_info_noise_value(
                    f64::from(pos.x) / noise_factor,
                    f64::from(pos.z) / noise_factor,
                );
                let n = ((noise + noise_offset) * f64::from(*noise_to_count_ratio)).ceil() as i32;
                Positions::Repeat(pos, n.max(0))
            }
            VegPlacement::SurfaceRelativeThresholdFilter {
                heightmap,
                min_inclusive,
                max_inclusive,
            } => {
                let surface = i64::from(heightmap.scan(grid, pos.x, pos.z));
                let min_y = surface + i64::from(*min_inclusive);
                let max_y = surface + i64::from(*max_inclusive);
                let y = i64::from(pos.y);
                if min_y <= y && y <= max_y {
                    Positions::One(pos)
                } else {
                    Positions::None
                }
            }
            VegPlacement::FixedPlacement(positions) => {
                let (cx, cz) = (pos.x >> 4, pos.z >> 4);
                let kept: Vec<BlockPos> = positions
                    .iter()
                    .copied()
                    .filter(|p| (p.x >> 4) == cx && (p.z >> 4) == cz)
                    .collect();
                Positions::from_vec(kept)
            }
        }
    }
}

/// The count-on-every-layer placement's own find-ground-y search — the `layer`-th
/// air-above-solid interface below `y_start`, or `None`.
fn find_on_ground_y(
    grid: &VegGrid,
    tags: &VegTags,
    x: i32,
    y_start: i32,
    z: i32,
    layer_to_place_on: i32,
) -> Option<i32> {
    // The empty check is air-or-water-or-lava, which is exactly `Tag::Air | Tag::Fluid`.
    let empty = |y: i32| {
        tag_at(grid, tags, Tag::Air, x, y, z) || tag_at(grid, tags, Tag::Fluid, x, y, z)
    };
    let mut current_layer = 0;
    let mut current_empty = empty(y_start);
    let mut y = y_start;
    while y >= grid.min_y + 1 {
        let below_empty = empty(y - 1);
        let below_bedrock =
            super::base_id(grid.get(x, y - 1, z)) == "minecraft:bedrock";
        if !below_empty && current_empty && !below_bedrock {
            if current_layer == layer_to_place_on {
                return Some(y);
            }
            current_layer += 1;
        }
        current_empty = below_empty;
        y -= 1;
    }
    None
}

/// What one [`VegPlacement`] yields for one input position — the allocation-free
/// replacement for the `Vec<BlockPos>` this used to return.
///
/// # Why exactly three shapes, and why that is not a narrowing
///
/// Unit 8 of [`docs/plans/worldgen-rewrite.md`](../../../../../docs/plans/worldgen-rewrite.md)
/// had to remove a heap allocation **per placement modifier per attempt** without
/// moving one RNG draw. Enumerating every arm of
/// [`VegPlacement::get_positions`] shows the returned `Vec` only ever had one of
/// three shapes, so this enum is exhaustive over what the old code could produce
/// rather than a subset of it:
///
/// | arm | old | new |
/// |---|---|---|
/// | `Count`, `NoiseThresholdCount` | `vec![pos; n]` | [`Positions::Repeat`] |
/// | `InSquare`, `RandomOffset`, `Biome` | `vec![one]` | [`Positions::One`] |
/// | `Heightmap`, `RarityFilter`, `SurfaceWaterDepthFilter`, `BlockPredicateFilter` | `vec![one]` or `Vec::new()` | [`Positions::One`] / [`Positions::None`] |
///
/// **No modifier vanilla ships in the vegetal-decoration subset returns two
/// *different* positions.** If one is ever added (a real `EnvironmentScan`-style
/// modifier that fans out), it does **not** get to smuggle itself in as a
/// `Repeat` — add a variant and handle it in the driver's walk, because
/// `Repeat`'s consumer recurses `n` times on the *same* position, which is
/// precisely what `vec![pos; n]` meant and is not what a fan-out means.
///
/// The draw still happens inside `get_positions`, before this value is returned,
/// so the consumption order is byte-identical: the driver's depth-first `recurse`
/// walks `Repeat`'s `n` copies in the same order `for next in vec` did. The plan
/// marks U8 **"must not"** change RNG order and names breadth-first
/// "optimisation" of this exact recursion as instant desync — the walk below never
/// touches the recursion's shape, only what it iterates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Positions {
    /// The modifier filtered this position out.
    None,
    /// Exactly one position (possibly moved from the input).
    One(BlockPos),
    /// `n` copies of one position — `Count`/`NoiseThresholdCount`'s
    /// `vec![pos; n]`. `n <= 0` means none.
    Repeat(BlockPos, i32),
    /// `n` **different** positions — the fan-out shape the doc above says must
    /// not be smuggled in as a `Repeat`. A later change added the two modifiers that
    /// need it (`count_on_every_layer`, `fixed_placement`); `Positions` stopped
    /// being `Copy` at the same time, which is why this variant is the only one
    /// that allocates and why nothing else was converted to use it.
    List(Vec<BlockPos>),
}

impl Positions {
    /// Collapses the degenerate cases so the common paths stay allocation-free.
    fn from_vec(mut v: Vec<BlockPos>) -> Self {
        match v.len() {
            0 => Positions::None,
            1 => Positions::One(v.pop().expect("len checked")),
            _ => Positions::List(v),
        }
    }
}

/// The reference tree-decorator base kind
/// (the subset reachable from oak/birch's `_bees_*` variants — see module
/// doc). Any other decorator type parses to [`Decorator::Unsupported`] (a
/// silent no-op — see [`place_beehive_decorator`]'s doc on the RNG-continuity
/// cost of skipping one).
#[derive(Clone, Debug)]
pub enum Decorator {
    Beehive { probability: f32 },
    /// The trunk-vine decorator — a hanging vine on each of a log's four
    /// horizontal neighbours, one independent coin flip per side (the
    /// savanna/acacia increment: reached from `mega_jungle_tree`/`jungle_tree`'s own
    /// `decorators` list, and from every `fallen_*_tree`'s
    /// `stump_decorators`). See [`super::place::place_trunk_vine_decorator`].
    TrunkVine,
    /// The attached-to-logs decorator — one block (a mushroom, for every shipped
    /// instance) on a random direction off a random log, gated by
    /// `probability` (every `fallen_*_tree`'s `log_decorators`).
    /// See [`super::place::place_attached_to_logs_decorator`].
    AttachedToLogs {
        probability: f32,
        block_provider: BlockStateProvider,
        directions: Vec<(i32, i32, i32)>,
    },
    Unsupported,
}

/// The reference direction codec — the six cardinal names the attached-to-logs decorator's
/// `directions` list can name (every shipped instance uses only `"up"`, but
/// the field is a general list in a faithful implementation's own codec).
fn parse_direction(s: &str) -> Option<(i32, i32, i32)> {
    match s {
        "down" => Some((0, -1, 0)),
        "up" => Some((0, 1, 0)),
        "north" => Some((0, 0, -1)),
        "south" => Some((0, 0, 1)),
        "west" => Some((-1, 0, 0)),
        "east" => Some((1, 0, 0)),
        _ => None,
    }
}

impl Decorator {
    fn parse(v: &Value) -> Self {
        let ty = v["type"].as_str().unwrap_or("");
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "beehive" => Decorator::Beehive {
                probability: v["probability"].as_f64().unwrap_or(0.0) as f32,
            },
            "trunk_vine" => Decorator::TrunkVine,
            "attached_to_logs" => {
                let block_provider = BlockStateProvider::try_parse(&v["block_provider"]);
                let directions: Option<Vec<(i32, i32, i32)>> = v["directions"].as_array().map(|arr| {
                    arr.iter().filter_map(|d| d.as_str().and_then(parse_direction)).collect()
                });
                match (block_provider, directions) {
                    (Some(block_provider), Some(directions)) if !directions.is_empty() => {
                        Decorator::AttachedToLogs {
                            probability: v["probability"].as_f64().unwrap_or(0.0) as f32,
                            block_provider,
                            directions,
                        }
                    }
                    _ => Decorator::Unsupported,
                }
            }
            _ => Decorator::Unsupported,
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
pub enum FeatureSizeCfg {
    TwoLayers {
        limit: i32,
        lower_size: i32,
        upper_size: i32,
        /// The feature size's own min-clipped-height field — `fancy_oak`'s own `4` (added
        /// with the savanna/acacia increment). `None` for every other species' `two_layers_feature_size`
        /// (oak's straight branch, birch, spruce, pine, acacia), which is
        /// exactly a faithful implementation's own empty-optional default. See
        /// [`place_tree`]'s own doc on the one place this is read: a tree
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
pub struct TreeConfig {
pub(super)     below_trunk_provider: Option<BlockStateProvider>,
pub(super)     trunk_provider: BlockStateProvider,
pub(super)     foliage_provider: BlockStateProvider,
pub(super)     trunk_placer: TrunkPlacerCfg,
pub(super)     foliage_placer: FoliagePlacerCfg,
pub(super)     feature_size: FeatureSizeCfg,
pub(super)     decorators: Vec<Decorator>,
    /// The tree configuration's own root-placer field — `Optional<RootPlacer>`. Absent for
    /// every species except mangrove/tall_mangrove. A `root_placer`
    /// key that's present in the JSON but fails to parse into a
    /// [`RootPlacerCfg`] this module implements fails the WHOLE [`TreeConfig`]
    /// (see [`Self::try_parse`]) rather than silently dropping it — dropping it
    /// would still place a trunk, just floating at the wrong origin with no
    /// roots under it, which is the "dangerous direction" `CLAUDE.md` names for
    /// silent degradation: a present-but-unmodelled root placer must not look
    /// like a tree with no root placer at all.
pub(super)     root_placer: Option<RootPlacerCfg>,
}

impl TreeConfig {
    /// `None` if any required sub-part (trunk placer, foliage placer,
    /// feature size, trunk/foliage provider) is a kind this module doesn't
    /// implement — see module doc on why that must degrade rather than
    /// panic. `below_trunk_provider`/`decorators` degrade individually
    /// instead (a missing/unsupported one just does less, it doesn't sink
    /// the whole tree). `root_placer` is a THIRD shape: absent is fine
    /// (`None`), but present-and-unparseable fails the whole config — see
    /// this struct's own `root_placer` field doc.
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
        let root_placer = match cfg.get("root_placer") {
            Some(r) if !r.is_null() => Some(RootPlacerCfg::try_parse(r)?),
            _ => None,
        };
        Some(Self {
            below_trunk_provider,
            trunk_provider,
            foliage_provider,
            trunk_placer,
            foliage_placer,
            feature_size,
            decorators,
            root_placer,
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
pub struct BlockColumnConfig {
pub(super)     layers: Vec<(IntProvider, BlockStateProvider)>,
pub(super)     direction: (i32, i32, i32),
pub(super)     allowed_placement: BlockPredicate,
pub(super)     prioritize_tip: bool,
}

impl BlockColumnConfig {
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

/// The reference configured-feature base kind (the
/// subset reached from grass/flower/tree biome steps). [`Unsupported`]
/// carries the reference type string purely for diagnostics — placing it is
/// always a no-op.
#[derive(Clone, Debug)]
pub enum ConfiguredFeature {
    SimpleBlock(BlockStateProvider),
    Tree(Box<TreeConfig>),
    BlockColumn(Box<BlockColumnConfig>),
    /// The fallen-tree feature — a real, distinct feature type, NOT
    /// a [`Self::Tree`] variant: a vertical stump plus a horizontal fallen
    /// log, no trunk/foliage placer involved at all. Reachable from many
    /// biomes' `fallen_*_tree` `RandomSelector` branches at a small
    /// (~1-1.25%) chance each. See [`super::features::place_fallen_tree`].
    FallenTree(Box<super::features::FallenTreeCfg>),
    RootSystem(Box<super::features::RootSystemCfg>),
    Coral(super::features::CoralKind),
    RandomSelector {
        default: Box<PlacedRef>,
        options: Vec<(f32, PlacedRef)>,
    },
    SimpleRandomSelector(Vec<PlacedRef>),
    // --- the types beyond the original seven. Bodies live in
    // [`super::features`]; each arm's parse is immediately below in
    // `parse_configured_feature_doc`.
    Spring(Box<super::features::SpringCfg>),
    Disk(Box<super::features::DiskCfg>),
    BlockPile(BlockStateProvider),
    NetherForestVegetation(Box<super::features::NetherForestVegetationCfg>),
    BlockBlob(Box<super::features::BlockBlobCfg>),
    ReplaceBlobs(Box<super::features::ReplaceBlobsCfg>),
    GlowstoneBlob,
    BasaltPillar,
    DesertWell,
    BlueIce,
    Kelp,
    SeaPickle(IntProvider),
    Seagrass(f64),
    Vines,
    TwistingVines(super::features::TwistingVinesCfg),
    WeepingVines,
    MultifaceGrowth(Box<super::features::MultifaceGrowthCfg>),
    Lake(Box<super::features::LakeCfg>),
    HugeMushroom(Box<super::features::HugeMushroomCfg>),
    Bamboo(f64),
    VegetationPatch(Box<super::features::VegetationPatchCfg>),
    SculkPatch(Box<super::features::SculkPatchCfg>),
    /// The random-boolean-selector feature — one boolean draw, then one branch.
    RandomBooleanSelector {
        yes: Box<PlacedRef>,
        no: Box<PlacedRef>,
    },
    /// The weighted-random-selector feature — a weighted list of placed features.
    WeightedRandomSelector(Vec<(i32, PlacedRef)>),
    /// The sequence feature — every entry in order, stopping at the first that
    /// reports failure. This engine's placement bodies do not report success, so
    /// every entry runs; that matches a faithful implementation for the one bundled instance.
    Sequence(Vec<PlacedRef>),
    /// The no-op feature — genuinely nothing, and distinct from
    /// [`ConfiguredFeature::Unsupported`] so it is not counted as a gap.
    NoOp,
    Unsupported(String),
}

/// The reference placed-feature record — an ordered
/// [`VegPlacement`] pipeline plus the [`ConfiguredFeature`] it terminates in.
/// Every reference to a placed feature (top-level biome step entry, or a
/// nested option inside a selector) resolves to one of these — a faithful
/// implementation's own placed-feature placement runs its *own* placement pipeline even when reached
/// as a selector's branch, and [`place_placed_feature`] reproduces that
/// uniformly rather than special-casing "top level" vs "nested".
#[derive(Clone, Debug)]
pub struct PlacedRef {
    pub placements: Vec<VegPlacement>,
    pub feature: Box<ConfiguredFeature>,
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
pub fn resolve_placed_feature_ref(resolver: &dyn Resolver, value: &Value) -> PlacedRef {
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
    let placements = doc
        .get("placement")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(VegPlacement::try_parse).collect())
        .unwrap_or_default();
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
pub fn resolve_configured_feature_ref(resolver: &dyn Resolver, value: &Value) -> ConfiguredFeature {
    match value {
        Value::String(id) => {
            let doc = resolver.configured_feature(id);
            if doc.is_null() {
                return ConfiguredFeature::Unsupported("missing configured_feature data".into());
            }
            parse_configured_feature_doc(resolver, &doc)
        }
        Value::Object(_) => parse_configured_feature_doc(resolver, value),
        _ => ConfiguredFeature::Unsupported("unexpected configured-feature ref shape".into()),
    }
}

pub(super) fn parse_configured_feature_doc(resolver: &dyn Resolver, doc: &Value) -> ConfiguredFeature {
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
        "fallen_tree" => {
            let c = &doc["config"];
            match (
                BlockStateProvider::try_parse(&c["trunk_provider"]),
                try_parse_int_provider(&c["log_length"]),
            ) {
                (Some(trunk_provider), Some(log_length)) => {
                    let stump_decorators = c
                        .get("stump_decorators")
                        .and_then(Value::as_array)
                        .map(|arr| arr.iter().map(Decorator::parse).collect())
                        .unwrap_or_default();
                    let log_decorators = c
                        .get("log_decorators")
                        .and_then(Value::as_array)
                        .map(|arr| arr.iter().map(Decorator::parse).collect())
                        .unwrap_or_default();
                    ConfiguredFeature::FallenTree(Box::new(super::features::FallenTreeCfg {
                        trunk_provider,
                        log_length,
                        stump_decorators,
                        log_decorators,
                    }))
                }
                _ => ConfiguredFeature::Unsupported(
                    "fallen_tree: unsupported trunk_provider/log_length".into(),
                ),
            }
        }
        "root_system" => {
            let c = &doc["config"];
            match (
                BlockStateProvider::try_parse(&c["root_state_provider"]),
                BlockStateProvider::try_parse(&c["hanging_root_state_provider"]),
            ) {
                (Some(root_state_provider), Some(hanging_root_state_provider)) => {
                    let mut root_replaceable = std::collections::HashSet::new();
                    let mut seen = std::collections::HashSet::new();
                    if let Some(tag) = c["root_replaceable"].as_str().and_then(|s| s.strip_prefix('#')) {
                        crate::compose::resolve_block_tag(resolver, tag, &mut root_replaceable, &mut seen);
                    } else {
                        root_replaceable.extend(parse_id_list(&c["root_replaceable"]));
                    }
                    ConfiguredFeature::RootSystem(Box::new(super::features::RootSystemCfg {
                        feature: resolve_placed_feature_ref(resolver, &c["feature"]),
                        required_vertical_space_for_tree: c["required_vertical_space_for_tree"].as_i64().unwrap_or(1) as i32,
                        level_test_distance: c["level_test_distance"].as_i64().unwrap_or(0) as i32,
                        max_level_deviation: c["max_level_deviation"].as_i64().unwrap_or(0) as i32,
                        root_radius: c["root_radius"].as_i64().unwrap_or(1) as i32,
                        root_replaceable,
                        root_state_provider,
                        root_placement_attempts: c["root_placement_attempts"].as_i64().unwrap_or(1) as i32,
                        root_column_max_height: c["root_column_max_height"].as_i64().unwrap_or(1) as i32,
                        hanging_root_radius: c["hanging_root_radius"].as_i64().unwrap_or(1) as i32,
                        hanging_roots_vertical_span: c["hanging_roots_vertical_span"].as_i64().unwrap_or(1) as i32,
                        hanging_root_state_provider,
                        hanging_root_placement_attempts: c["hanging_root_placement_attempts"].as_i64().unwrap_or(1) as i32,
                        allowed_vertical_water_for_tree: c["allowed_vertical_water_for_tree"].as_i64().unwrap_or(1) as i32,
                        allowed_tree_position: BlockPredicate::parse(&c["allowed_tree_position"]),
                    }))
                }
                _ => ConfiguredFeature::Unsupported("root_system: unsupported state provider".into()),
            }
        }
        "coral_tree" => ConfiguredFeature::Coral(super::features::CoralKind::Tree),
        "coral_claw" => ConfiguredFeature::Coral(super::features::CoralKind::Claw),
        "coral_mushroom" => ConfiguredFeature::Coral(super::features::CoralKind::Mushroom),
        "random_selector" => {
            let cfg = &doc["config"];
            let default = resolve_placed_feature_ref(resolver, &cfg["default"]);
            let options = cfg["features"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|e| {
                            let chance = e["chance"].as_f64().unwrap_or(0.0) as f32;
                            (chance, resolve_placed_feature_ref(resolver, &e["feature"]))
                        })
                        .collect()
                })
                .unwrap_or_default();
            ConfiguredFeature::RandomSelector {
                default: Box::new(default),
                options,
            }
        }
        "simple_random_selector" => {
            let list = doc["config"]["features"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|e| resolve_placed_feature_ref(resolver, e))
                        .collect()
                })
                .unwrap_or_default();
            ConfiguredFeature::SimpleRandomSelector(list)
        }
        // ------------------------------------------------------------------
        // Every arm here is `Option`-shaped or defaulted: a field
        // this engine cannot read degrades the *feature* to `Unsupported`, never
        // panics, and never silently places the wrong block. See [`super`]'s
        // module doc for why that rule is absolute in this file.
        // ------------------------------------------------------------------
        "spring_feature" => {
            let c = &doc["config"];
            ConfiguredFeature::Spring(Box::new(super::features::SpringCfg {
                state: canon_state(&c["state"]),
                requires_block_below: c["requires_block_below"].as_bool().unwrap_or(true),
                rock_count: c["rock_count"].as_i64().unwrap_or(4) as i32,
                hole_count: c["hole_count"].as_i64().unwrap_or(1) as i32,
                valid_blocks: parse_id_list(&c["valid_blocks"]).into_iter().collect(),
            }))
        }
        "disk" => {
            let c = &doc["config"];
            match (
                BlockStateProvider::try_parse(&c["state_provider"]),
                try_parse_int_provider(&c["radius"]),
            ) {
                (Some(provider), Some(radius)) => {
                    ConfiguredFeature::Disk(Box::new(super::features::DiskCfg {
                        provider,
                        target: BlockPredicate::parse(&c["target"]),
                        radius,
                        half_height: c["half_height"].as_i64().unwrap_or(0) as i32,
                    }))
                }
                _ => ConfiguredFeature::Unsupported("disk: unsupported provider/radius".into()),
            }
        }
        "block_pile" => match BlockStateProvider::try_parse(&doc["config"]["state_provider"]) {
            Some(p) => ConfiguredFeature::BlockPile(p),
            None => ConfiguredFeature::Unsupported("block_pile: unsupported provider".into()),
        },
        "nether_forest_vegetation" => {
            let c = &doc["config"];
            match BlockStateProvider::try_parse(&c["state_provider"]) {
                Some(provider) => ConfiguredFeature::NetherForestVegetation(Box::new(
                    super::features::NetherForestVegetationCfg {
                        provider,
                        spread_width: c["spread_width"].as_i64().unwrap_or(8) as i32,
                        spread_height: c["spread_height"].as_i64().unwrap_or(4) as i32,
                    },
                )),
                None => ConfiguredFeature::Unsupported(
                    "nether_forest_vegetation: unsupported provider".into(),
                ),
            }
        }
        "block_blob" => {
            let c = &doc["config"];
            ConfiguredFeature::BlockBlob(Box::new(super::features::BlockBlobCfg {
                state: canon_state(&c["state"]),
                can_place_on: BlockPredicate::parse(&c["can_place_on"]),
            }))
        }
        "netherrack_replace_blobs" => {
            let c = &doc["config"];
            match try_parse_int_provider(&c["radius"]) {
                Some(radius) => {
                    ConfiguredFeature::ReplaceBlobs(Box::new(super::features::ReplaceBlobsCfg {
                        target: canon_state(&c["target"]),
                        state: canon_state(&c["state"]),
                        radius,
                    }))
                }
                None => ConfiguredFeature::Unsupported(
                    "netherrack_replace_blobs: unsupported radius".into(),
                ),
            }
        }
        "glowstone_blob" => ConfiguredFeature::GlowstoneBlob,
        "basalt_pillar" => ConfiguredFeature::BasaltPillar,
        "desert_well" => ConfiguredFeature::DesertWell,
        "blue_ice" => ConfiguredFeature::BlueIce,
        "kelp" => ConfiguredFeature::Kelp,
        "sea_pickle" => match try_parse_int_provider(&doc["config"]["count"]) {
            Some(ip) => ConfiguredFeature::SeaPickle(ip),
            None => ConfiguredFeature::Unsupported("sea_pickle: unsupported count".into()),
        },
        "seagrass" => {
            ConfiguredFeature::Seagrass(doc["config"]["probability"].as_f64().unwrap_or(0.0))
        }
        "vines" => ConfiguredFeature::Vines,
        "twisting_vines" => {
            let c = &doc["config"];
            ConfiguredFeature::TwistingVines(super::features::TwistingVinesCfg {
                spread_width: c["spread_width"].as_i64().unwrap_or(8) as i32,
                spread_height: c["spread_height"].as_i64().unwrap_or(4) as i32,
                max_height: c["max_height"].as_i64().unwrap_or(8) as i32,
            })
        }
        "weeping_vines" => ConfiguredFeature::WeepingVines,
        "bamboo" => ConfiguredFeature::Bamboo(doc["config"]["probability"].as_f64().unwrap_or(0.0)),
        "multiface_growth" => {
            let c = &doc["config"];
            ConfiguredFeature::MultifaceGrowth(Box::new(super::features::MultifaceGrowthCfg {
                block: c["block"].as_str().unwrap_or("minecraft:glow_lichen").to_string(),
                search_range: c["search_range"].as_i64().unwrap_or(10) as i32,
                // Vanilla's codec defaults: all three false.
                can_place_on_floor: c["can_place_on_floor"].as_bool().unwrap_or(false),
                can_place_on_ceiling: c["can_place_on_ceiling"].as_bool().unwrap_or(false),
                can_place_on_wall: c["can_place_on_wall"].as_bool().unwrap_or(false),
                chance_of_spreading: c["chance_of_spreading"].as_f64().unwrap_or(0.5) as f32,
                can_be_placed_on: parse_id_list(&c["can_be_placed_on"]).into_iter().collect(),
            }))
        }
        "lake" => {
            let c = &doc["config"];
            match (
                BlockStateProvider::try_parse(&c["fluid"]),
                BlockStateProvider::try_parse(&c["barrier"]),
            ) {
                (Some(fluid), Some(barrier)) => {
                    ConfiguredFeature::Lake(Box::new(super::features::LakeCfg {
                        fluid,
                        barrier,
                        can_place_feature: BlockPredicate::parse(&c["can_place_feature"]),
                        can_replace_with_air_or_fluid: BlockPredicate::parse(
                            &c["can_replace_with_air_or_fluid"],
                        ),
                        can_replace_with_barrier: BlockPredicate::parse(
                            &c["can_replace_with_barrier"],
                        ),
                    }))
                }
                _ => ConfiguredFeature::Unsupported("lake: unsupported fluid/barrier".into()),
            }
        }
        "huge_brown_mushroom" | "huge_red_mushroom" => {
            let c = &doc["config"];
            match (
                BlockStateProvider::try_parse(&c["cap_provider"]),
                BlockStateProvider::try_parse(&c["stem_provider"]),
            ) {
                (Some(cap_provider), Some(stem_provider)) => ConfiguredFeature::HugeMushroom(Box::new(
                    super::features::HugeMushroomCfg {
                        can_place_on: BlockPredicate::parse(&c["can_place_on"]),
                        cap_provider,
                        stem_provider,
                        // Brown explicitly supplies 3; red uses the codec default 2.
                        foliage_radius: c["foliage_radius"].as_i64().unwrap_or(2) as i32,
                        kind: if short == "huge_brown_mushroom" {
                            super::features::HugeMushroomKind::Brown
                        } else {
                            super::features::HugeMushroomKind::Red
                        },
                    },
                )),
                _ => ConfiguredFeature::Unsupported(
                    "huge_mushroom: unsupported cap/stem provider".into(),
                ),
            }
        }
        "vegetation_patch" | "waterlogged_vegetation_patch" => {
            let c = &doc["config"];
            let surface = match c["surface"].as_str().unwrap_or("floor") {
                "ceiling" => super::features::CaveSurface::Ceiling,
                _ => super::features::CaveSurface::Floor,
            };
            match (
                BlockStateProvider::try_parse(&c["ground_state"]),
                try_parse_int_provider(&c["depth"]),
                try_parse_int_provider(&c["xz_radius"]),
            ) {
                (Some(ground_state), Some(depth), Some(xz_radius)) => {
                    ConfiguredFeature::VegetationPatch(Box::new(
                        super::features::VegetationPatchCfg {
                            replaceable: parse_id_list(&c["replaceable"]).into_iter().collect(),
                            ground_state,
                            vegetation_feature: resolve_placed_feature_ref(
                                resolver,
                                &c["vegetation_feature"],
                            ),
                            surface,
                            depth,
                            extra_bottom_block_chance: c["extra_bottom_block_chance"]
                                .as_f64()
                                .unwrap_or(0.0) as f32,
                            vertical_range: c["vertical_range"].as_i64().unwrap_or(1) as i32,
                            vegetation_chance: c["vegetation_chance"].as_f64().unwrap_or(0.0) as f32,
                            xz_radius,
                            extra_edge_column_chance: c["extra_edge_column_chance"]
                                .as_f64()
                                .unwrap_or(0.0) as f32,
                            waterlogged: short == "waterlogged_vegetation_patch",
                        },
                    ))
                }
                _ => ConfiguredFeature::Unsupported(
                    "vegetation_patch: unsupported ground/depth/radius".into(),
                ),
            }
        }
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
        "random_boolean_selector" => {
            let c = &doc["config"];
            ConfiguredFeature::RandomBooleanSelector {
                yes: Box::new(resolve_placed_feature_ref(resolver, &c["feature_true"])),
                no: Box::new(resolve_placed_feature_ref(resolver, &c["feature_false"])),
            }
        }
        "weighted_random_selector" => {
            let list = doc["config"]["features"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|e| {
                            let weight = e["weight"].as_i64().unwrap_or(1) as i32;
                            (weight, resolve_placed_feature_ref(resolver, &e["data"]))
                        })
                        .collect()
                })
                .unwrap_or_default();
            ConfiguredFeature::WeightedRandomSelector(list)
        }
        "sequence" => {
            let list = doc["config"]["features"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|e| resolve_placed_feature_ref(resolver, e))
                        .collect()
                })
                .unwrap_or_default();
            ConfiguredFeature::Sequence(list)
        }
        "no_op" => ConfiguredFeature::NoOp,
        other => ConfiguredFeature::Unsupported(other.to_string()),
    }
}

/// Walks a resolved vegetal-decoration tree — through `RandomSelector`'s
/// `default`/`options` and `SimpleRandomSelector`'s list, the only two ways
/// this module's own [`ConfiguredFeature`] nests — collecting every
/// [`ConfiguredFeature::Unsupported`] reason string actually reachable from
/// `placed`. This is the read side of this module's "unsupported degrades to
/// a silent no-op" rule: a caller that wants that silence to be **loud**
/// (the "does this biome's declared vegetation include a placer we
/// don't implement" gate, in `lodestone_server::worldgen_data`) diffs this
/// against a maintained allow-list instead of trusting the resolved tree to
/// run and simply place fewer blocks than vanilla. Reasons are **not**
/// deduplicated here — the caller decides whether it wants a set or a count.
#[must_use]
pub fn collect_unsupported(placed: &PlacedRef) -> Vec<String> {
    fn walk(feature: &ConfiguredFeature, out: &mut Vec<String>) {
        match feature {
            ConfiguredFeature::Unsupported(reason) => out.push(reason.clone()),
            ConfiguredFeature::RandomSelector { default, options } => {
                walk(&default.feature, out);
                for (_, opt) in options {
                    walk(&opt.feature, out);
                }
            }
            ConfiguredFeature::SimpleRandomSelector(list) | ConfiguredFeature::Sequence(list) => {
                for opt in list {
                    walk(&opt.feature, out);
                }
            }
            ConfiguredFeature::RandomBooleanSelector { yes, no } => {
                walk(&yes.feature, out);
                walk(&no.feature, out);
            }
            ConfiguredFeature::WeightedRandomSelector(list) => {
                for (_, opt) in list {
                    walk(&opt.feature, out);
                }
            }
            ConfiguredFeature::VegetationPatch(cfg) => walk(&cfg.vegetation_feature.feature, out),
            // The direct compiled-server map is exact, but the real composed
            // cave fixture remains red. Keep this feature visible to the
            // production gap census until that end-to-end gate turns green.
            ConfiguredFeature::RootSystem(cfg) => {
                out.push("root_system".to_string());
                walk(&cfg.feature.feature, out);
            }
            ConfiguredFeature::Coral(kind) => out.push(match kind {
                super::features::CoralKind::Tree => "coral_tree",
                super::features::CoralKind::Claw => "coral_claw",
                super::features::CoralKind::Mushroom => "coral_mushroom",
            }.to_string()),
            // Every terminal (modelled) feature type. Listed rather than `_ => {}`
            // so a newly added variant is a compile error here — this walk is the
            // read side of the "which types are still gaps" instrument, and a
            // catch-all would silently report a new type as fully modelled.
            ConfiguredFeature::SimpleBlock(_)
            | ConfiguredFeature::Tree(_)
            | ConfiguredFeature::BlockColumn(_)
            | ConfiguredFeature::FallenTree(_)
            | ConfiguredFeature::Spring(_)
            | ConfiguredFeature::Disk(_)
            | ConfiguredFeature::BlockPile(_)
            | ConfiguredFeature::NetherForestVegetation(_)
            | ConfiguredFeature::BlockBlob(_)
            | ConfiguredFeature::ReplaceBlobs(_)
            | ConfiguredFeature::GlowstoneBlob
            | ConfiguredFeature::BasaltPillar
            | ConfiguredFeature::DesertWell
            | ConfiguredFeature::BlueIce
            | ConfiguredFeature::Kelp
            | ConfiguredFeature::SeaPickle(_)
            | ConfiguredFeature::Seagrass(_)
            | ConfiguredFeature::Vines
            | ConfiguredFeature::TwistingVines(_)
            | ConfiguredFeature::WeepingVines
            | ConfiguredFeature::MultifaceGrowth(_)
            | ConfiguredFeature::Lake(_)
            | ConfiguredFeature::HugeMushroom(_)
            | ConfiguredFeature::Bamboo(_)
            | ConfiguredFeature::SculkPatch(_)
            | ConfiguredFeature::NoOp => {}
        }
    }
    let mut out = Vec::new();
    walk(&placed.feature, &mut out);
    out
}
