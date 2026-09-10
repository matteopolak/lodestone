//! End-only biome-decoration entries that can be represented by an
//! [`super::EndColumn`].
//!
//! The common vegetation decoder deliberately does not own these feature types:
//! their placement rules either have End-specific geometry or need data that a
//! block column cannot carry.  Keeping the small supported subset here makes the
//! boundary explicit instead of silently treating an End document as an
//! Overworld vegetation document.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::Value;
use lodestone_data::biomes::BuiltinBiome;

use crate::dense_grid::DenseBlockGrid;
use crate::density::Resolver;
use crate::rng::{RandomSource, WorldgenRandom, XoroshiroRandomSource};

use super::{END_HIGHLANDS, SMALL_END_ISLANDS, THE_END, EndBiomeSource, EndSpike, end_spike_blocks, end_spikes_for_seed};

/// Built-in End biomes visible in one source chunk's 3x3 feature region.
///
/// The source can return only five identities, so a compact bitset avoids a
/// general-purpose set allocation for every FEATURES source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EndBiomeSet(u8);

impl EndBiomeSet {
    #[must_use]
    pub(crate) fn around_source(
        source_x: i32,
        source_z: i32,
        mut biome_at_chunk: impl FnMut(i32, i32) -> BuiltinBiome,
    ) -> Self {
        let mut biomes = Self::default();
        for dx in -1..=1 {
            for dz in -1..=1 {
                biomes.insert(biome_at_chunk(source_x + dx, source_z + dz));
            }
        }
        biomes
    }

    fn insert(&mut self, biome: BuiltinBiome) {
        let bit = match biome {
            BuiltinBiome::TheEnd => 1 << 0,
            BuiltinBiome::EndHighlands => 1 << 1,
            BuiltinBiome::EndMidlands => 1 << 2,
            BuiltinBiome::SmallEndIslands => 1 << 3,
            BuiltinBiome::EndBarrens => 1 << 4,
            _ => 0,
        };
        self.0 |= bit;
    }

    #[must_use]
    fn contains(self, biome: BuiltinBiome) -> bool {
        let mut singleton = Self::default();
        singleton.insert(biome);
        singleton.0 != 0 && self.0 & singleton.0 != 0
    }
}

#[cfg(test)]
mod biome_set_tests {
    use super::*;

    #[test]
    fn source_neighbourhood_collects_each_typed_identity_without_duplicates() {
        let biomes = EndBiomeSet::around_source(10, 20, |x, z| match (x - 10, z - 20) {
            (-1, -1) => BuiltinBiome::TheEnd,
            (1, 1) => BuiltinBiome::SmallEndIslands,
            _ => BuiltinBiome::EndHighlands,
        });

        assert!(biomes.contains(BuiltinBiome::TheEnd));
        assert!(biomes.contains(BuiltinBiome::EndHighlands));
        assert!(biomes.contains(BuiltinBiome::SmallEndIslands));
        assert!(!biomes.contains(BuiltinBiome::EndBarrens));
        assert!(!biomes.contains(BuiltinBiome::Plains));
    }
}

/// The exit metadata attached to a generated return gateway.  The block itself
/// belongs in the palette; this record is the data the gateway block entity
/// needs in order to be functional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndGateway {
    /// Gateway block position.
    pub pos: (i32, i32, i32),
    /// Exact destination declared by the configured feature.
    pub exit: (i32, i32, i32),
    /// Whether the exit bypasses a destination search.
    pub exact: bool,
}

/// Final state written by one End FEATURES source.
///
/// The lifecycle materializer applies these absolute transitions to its
/// resident columns in admission order. Text is intentional at this boundary:
/// state ids belong to one generator's interner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndDecorationSpill {
    pub source: (i32, i32),
    pub position: (i32, i32, i32),
    pub state: String,
}

/// Complete output of one source-filtered End FEATURES invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndDecorationResult {
    pub spills: Vec<EndDecorationSpill>,
    pub gateways: Vec<EndGateway>,
}

/// One fixed platform origin read from the `end_platform` placed-feature data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlatformOrigin {
    x: i32,
    y: i32,
    z: i32,
}

/// The configured part of an End gateway feature.
///
/// The return gateway is the only End decoration whose configured feature has
/// meaningful data in the bundled registry.  Keeping that data next to the
/// placement flag prevents the decoration driver from silently replacing a
/// datapack (or version refresh) with the historical `(100, 50, 0)` default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GatewayConfig {
    exit: (i32, i32, i32),
    exact: bool,
}

/// End decoration with a block-column representation.
///
/// The fixed platform, outer islands, chorus plants, and return-gateway blocks
/// all write into the three-by-three region. A return gateway also creates an
/// [`EndGateway`] sidecar. Spikes write their feature-owned blocks here; their
/// crystals remain gameplay entities.
#[derive(Debug, Clone, Default)]
pub(crate) struct EndDecoration {
    platforms: Vec<PlatformOrigin>,
    outer_islands: bool,
    outer_island_index: Option<usize>,
    chorus: bool,
    chorus_index: Option<usize>,
    chorus_supports: HashSet<String>,
    gateway_return: Option<GatewayConfig>,
    gateway_index: Option<usize>,
    /// `None` means the End biome has no spike feature. An empty list means
    /// the feature uses its seed-derived ten-spike fallback; a non-empty list
    /// is the configured explicit layout and must be used verbatim.
    spikes: Option<Vec<EndSpike>>,
    spike_index: Option<usize>,
}

impl EndDecoration {
    pub(crate) fn from_resolver(resolver: &dyn Resolver) -> Self {
        let mut chorus_supports = HashSet::new();
        crate::compose::resolve_block_tag(
            resolver,
            "minecraft:supports_chorus_plant",
            &mut chorus_supports,
            &mut HashSet::new(),
        );
        let document = resolver.biome_document(THE_END);
        let platform_entries = document
            .get("features")
            .and_then(Value::as_array)
            .and_then(|steps| steps.get(10))
            .and_then(Value::as_array);

        let mut platforms = Vec::new();
        if let Some(entries) = platform_entries {
            for entry in entries {
                let Some(id) = entry.as_str() else {
                    continue;
                };
                let placed = resolver.placed_feature(id);
                let Some(configured_id) = placed.get("feature").and_then(Value::as_str) else {
                    continue;
                };
                if resolver
                    .configured_feature(configured_id)
                    .get("type")
                    .and_then(Value::as_str)
                    != Some("minecraft:end_platform")
                {
                    continue;
                }
                let Some(placement) = placed.get("placement").and_then(Value::as_array) else {
                    continue;
                };
                for modifier in placement {
                    if modifier.get("type").and_then(Value::as_str) != Some("minecraft:fixed_placement") {
                        continue;
                    }
                    let Some(positions) = modifier.get("positions").and_then(Value::as_array) else {
                        continue;
                    };
                    for position in positions {
                        let Some(position) = position.as_array() else {
                            continue;
                        };
                        let [x, y, z] = position.as_slice() else {
                            continue;
                        };
                        let (Some(x), Some(y), Some(z)) = (x.as_i64(), y.as_i64(), z.as_i64()) else {
                            continue;
                        };
                        let (Ok(x), Ok(y), Ok(z)) = (i32::try_from(x), i32::try_from(y), i32::try_from(z)) else {
                            continue;
                        };
                        platforms.push(PlatformOrigin { x, y, z });
                    }
                }
            }
        }
        let outer_island_index = feature_index_in_step(resolver, SMALL_END_ISLANDS, 0, "minecraft:end_island");
        let chorus_index = feature_index_in_step(resolver, END_HIGHLANDS, 9, "minecraft:chorus_plant");
        let gateway_index = feature_index_in_step(resolver, END_HIGHLANDS, 4, "minecraft:end_gateway");
        let spike_index = feature_index_in_step(resolver, THE_END, 4, "minecraft:end_spike");
        Self {
            platforms,
            outer_islands: outer_island_index.is_some(),
            outer_island_index,
            chorus: chorus_index.is_some(),
            chorus_index,
            chorus_supports,
            gateway_return: gateway_config_in_step(resolver, END_HIGHLANDS, 4),
            gateway_index,
            spikes: spike_config_in_step(resolver, THE_END, 4),
            spike_index,
        }
    }

    /// Applies every fixed platform which intersects this materialized column.
    ///
    /// Feature execution may be requested from any main-island chunk, but every
    /// invocation targets the same fixed coordinate.  Restricting writes to the
    /// materialized column makes independently requested columns converge on the
    /// same world state without a shared cache.
    pub(crate) fn apply(&self, world: &mut DenseBlockGrid) {
        for origin in &self.platforms {
            apply_platform(world, *origin);
        }
    }

    /// Executes the End features whose source chunks may write into `cx,cz`.
    /// The 3×3 source window is the feature write radius: an outer island's
    /// disc and a chorus plant may cross one chunk boundary.
    pub(crate) fn apply_region(
        &self,
        seed: i64,
        cx: i32,
        cz: i32,
        world: &mut DenseBlockGrid,
        biome_at_chunk: impl Fn(i32, i32) -> BuiltinBiome,
    ) -> Vec<EndGateway> {
        self.apply(world);
        let mut gateways = Vec::new();
        for source_x in cx - 1..=cx + 1 {
            for source_z in cz - 1..=cz + 1 {
                let biomes = EndBiomeSet::around_source(source_x, source_z, &biome_at_chunk);
                gateways.extend(self.apply_source(seed, source_x, source_z, world, biomes).into_iter().filter(|gateway| {
                    gateway.pos.0.div_euclid(16) == cx && gateway.pos.2.div_euclid(16) == cz
                }));
            }
        }
        gateways
    }

    pub(crate) fn apply_source(
        &self,
        seed: i64,
        source_x: i32,
        source_z: i32,
        world: &mut DenseBlockGrid,
        source_biomes: EndBiomeSet,
    ) -> Vec<EndGateway> {
        let biome_source = EndBiomeSource::new(seed);
        let mut gateways = Vec::new();
        // Fixed placement is a source-owned TOP_LAYER feature.  The complete
        // region path applies all fixed placements before replaying the
        // mutable source window; the lifecycle path must emit the same write
        // from the chunk containing each configured placement origin so the
        // spill reaches both the origin and adjacent resident columns.
        for &origin in &self.platforms {
            if origin.x.div_euclid(16) == source_x && origin.z.div_euclid(16) == source_z {
                apply_platform(world, origin);
            }
        }
        let generated_spikes = self
            .spikes
            .as_ref()
            .filter(|configured| configured.is_empty())
            .map(|_| end_spikes_for_seed(seed));
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        let decoration_seed = random.set_decoration_seed(seed, source_x * 16, source_z * 16);
        if source_biomes.contains(BuiltinBiome::TheEnd) {
            random.set_feature_seed(decoration_seed, self.spike_index.unwrap_or_default() as i32, 4);
            let spikes: &[EndSpike] = match self.spikes.as_deref() {
                Some(configured) if !configured.is_empty() => configured,
                Some(_) => generated_spikes.as_ref().expect("empty spike config must have a generated fallback"),
                None => &[],
            };
            for spike in spikes {
                if spike.center_x.div_euclid(16) == source_x && spike.center_z.div_euclid(16) == source_z {
                    for block in end_spike_blocks(spike, 0) {
                        world.set(block.x, block.y, block.z, &block.state);
                    }
                }
            }
        }
        if self.outer_islands && source_biomes.contains(BuiltinBiome::SmallEndIslands) {
            apply_outer_islands(
                world,
                &mut random,
                decoration_seed,
                self.outer_island_index.unwrap_or_default() as i32,
                source_x,
                source_z,
                |x, y, z| biome_source.biome_at_block_typed(x, y, z) == BuiltinBiome::SmallEndIslands,
            );
        }
        if source_biomes.contains(BuiltinBiome::EndHighlands) && self.gateway_return.is_some() {
            random.set_feature_seed(decoration_seed, self.gateway_index.unwrap_or_default() as i32, 4);
            if random.next_float() < 1.0 / 700.0 {
                let x = source_x * 16 + random.next_int_bounded(16);
                let z = source_z * 16 + random.next_int_bounded(16);
                let y = surface_y(world, x, z) + 3 + random.next_int_bounded(7);
                if biome_source.biome_at_block_typed(x, y, z) == BuiltinBiome::EndHighlands {
                    write_gateway(world, (x, y, z));
                    let config = self.gateway_return.expect("gateway flag checked immediately above");
                    gateways.push(EndGateway { pos: (x, y, z), exit: config.exit, exact: config.exact });
                }
            }
        }
        if self.chorus {
            random.set_feature_seed(decoration_seed, self.chorus_index.unwrap_or_default() as i32, 9);
            let attempts = random.next_int_bounded(5);
            for _ in 0..attempts {
                let x = source_x * 16 + random.next_int_bounded(16);
                let z = source_z * 16 + random.next_int_bounded(16);
                let y = surface_y(world, x, z);
                if biome_source.biome_at_block_typed(x, y, z) == BuiltinBiome::EndHighlands
                    && world.get(x, y, z) == "minecraft:air"
                    && world.get(x, y - 1, z) == "minecraft:end_stone"
                {
                    grow_chorus(world, &mut random, (x, y, z), (x, y, z), 0, &self.chorus_supports);
                }
            }
        }
        gateways
    }

}

/// Runs the `end_island` placement chain for one source chunk. The final
/// biome filter is deliberately evaluated at each sampled origin, after the
/// rarity, count, square, and height modifiers have consumed their draws.
/// This keeps a source-centre biome from suppressing an eligible origin near a
/// biome boundary while retaining the feature's exact random stream.
fn apply_outer_islands<R: RandomSource, F: FnMut(i32, i32, i32) -> bool>(
    world: &mut DenseBlockGrid,
    random: &mut WorldgenRandom<R>,
    decoration_seed: i64,
    feature_index: i32,
    source_x: i32,
    source_z: i32,
    mut biome_at_origin: F,
) {
    random.set_feature_seed(decoration_seed, feature_index, 0);
    if random.next_float() >= 1.0 / 14.0 {
        return;
    }
    let count = if random.next_int_bounded(4) < 3 { 1 } else { 2 };
    for _ in 0..count {
        let x = source_x * 16 + random.next_int_bounded(16);
        let z = source_z * 16 + random.next_int_bounded(16);
        let y = 55 + random.next_int_bounded(16);
        if biome_at_origin(x, y, z) {
            place_outer_island(world, random, (x, y, z));
        }
    }
}

fn apply_platform(world: &mut DenseBlockGrid, origin: PlatformOrigin) {
    for dz in -2..=2 {
        for dx in -2..=2 {
            world.set(origin.x + dx, origin.y - 1, origin.z + dz, "minecraft:obsidian");
            for dy in 0..3 {
                world.set(origin.x + dx, origin.y + dy, origin.z + dz, "minecraft:air");
            }
        }
    }
}

fn is_chorus(state: &str) -> bool {
    state.starts_with("minecraft:chorus_plant") || state.starts_with("minecraft:chorus_flower")
}

fn chorus_plant_state(world: &DenseBlockGrid, pos: (i32, i32, i32), supports: &HashSet<String>) -> String {
    let chorus_at = |x, y, z| is_chorus(world.get(x, y, z));
    let down = world.get(pos.0, pos.1 - 1, pos.2);
    let down_connected = chorus_at(pos.0, pos.1 - 1, pos.2)
        || supports.contains(down.split('[').next().unwrap_or(down));
    format!(
        "minecraft:chorus_plant[down={},east={},north={},south={},up={},west={}]",
        down_connected,
        chorus_at(pos.0 + 1, pos.1, pos.2),
        chorus_at(pos.0, pos.1, pos.2 - 1),
        chorus_at(pos.0, pos.1, pos.2 + 1),
        chorus_at(pos.0, pos.1 + 1, pos.2),
        chorus_at(pos.0 - 1, pos.1, pos.2),
    )
}

fn set_chorus_plant(world: &mut DenseBlockGrid, pos: (i32, i32, i32), supports: &HashSet<String>) {
    world.set(pos.0, pos.1, pos.2, &chorus_plant_state(world, pos, supports));
}

fn horizontally_empty(world: &DenseBlockGrid, pos: (i32, i32, i32), ignore: Option<(i32, i32)>) -> bool {
    [(-1, 0), (1, 0), (0, -1), (0, 1)].into_iter().all(|(dx, dz)| {
        ignore == Some((dx, dz)) || world.get(pos.0 + dx, pos.1, pos.2 + dz) == "minecraft:air"
    })
}

fn grow_chorus<R: RandomSource>(
    world: &mut DenseBlockGrid,
    random: &mut R,
    current: (i32, i32, i32),
    start: (i32, i32, i32),
    depth: i32,
    supports: &HashSet<String>,
) {
    set_chorus_plant(world, current, supports);
    let height = random.next_int_bounded(4) + 1 + i32::from(depth == 0);
    for i in 0..height {
        let target = (current.0, current.1 + i + 1, current.2);
        if !horizontally_empty(world, target, None) { return; }
        set_chorus_plant(world, target, supports);
        set_chorus_plant(world, (target.0, target.1 - 1, target.2), supports);
    }
    let mut branched = false;
    if depth < 4 {
        let stems = random.next_int_bounded(4) + i32::from(depth == 0);
        for _ in 0..stems {
            let (dx, dz) = [(0, -1), (1, 0), (0, 1), (-1, 0)][random.next_int_bounded(4) as usize];
            let target = (current.0 + dx, current.1 + height, current.2 + dz);
            if (target.0 - start.0).abs() < 8
                && (target.2 - start.2).abs() < 8
                && world.get(target.0, target.1, target.2) == "minecraft:air"
                && world.get(target.0, target.1 - 1, target.2) == "minecraft:air"
                && horizontally_empty(world, target, Some((-dx, -dz)))
            {
                branched = true;
                set_chorus_plant(world, target, supports);
                set_chorus_plant(world, (target.0 - dx, target.1, target.2 - dz), supports);
                grow_chorus(world, random, target, start, depth + 1, supports);
            }
        }
    }
    if !branched {
        world.set(current.0, current.1 + height, current.2, "minecraft:chorus_flower[age=5]");
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct FeatureNode {
    step: i32,
    first_seen: usize,
}

/// Visits the global feature-order graph in the same post-order used by the
/// native decoration scheduler. A feature's seed index is assigned after this
/// graph is reversed and filtered to its generation step; it is not the local
/// index in one biome document.
fn visit_feature_node(
    node: FeatureNode,
    edges: &BTreeMap<FeatureNode, BTreeSet<FeatureNode>>,
    discovered: &mut HashSet<FeatureNode>,
    visiting: &mut HashSet<FeatureNode>,
    ordered: &mut Vec<FeatureNode>,
) -> bool {
    if discovered.contains(&node) {
        return false;
    }
    if !visiting.insert(node) {
        return true;
    }
    for next in edges.get(&node).into_iter().flatten().copied() {
        if visit_feature_node(next, edges, discovered, visiting, ordered) {
            return true;
        }
    }
    visiting.remove(&node);
    discovered.insert(node);
    ordered.push(node);
    false
}

/// Returns the global raw seed index for the first placed feature of `kind` in
/// `biome`'s `step`. The graph is built from all five End biomes in source
/// order, because a serving chunk's nearby-biome set only selects members from
/// a global order that was computed once for the dimension.
fn feature_index_in_step(resolver: &dyn Resolver, biome: &str, step: usize, kind: &str) -> Option<usize> {
    let mut first_seen = HashMap::<String, usize>::new();
    let mut node_ids = BTreeMap::<FeatureNode, String>::new();
    let mut edges = BTreeMap::<FeatureNode, BTreeSet<FeatureNode>>::new();
    let mut next_first_seen = 0usize;
    let mut target_ids = HashSet::<String>::new();

    for source_biome in super::EndBiomeSource::possible_biomes() {
        let document = resolver.biome_document(source_biome);
        let Some(steps) = document.get("features").and_then(Value::as_array) else {
            continue;
        };
        let mut sequence = Vec::new();
        for (source_step, entries) in steps.iter().enumerate() {
            let Some(entries) = entries.as_array() else {
                continue;
            };
            for entry in entries {
                let Some(id) = entry.as_str() else {
                    continue;
                };
                if resolver.placed_feature(id).is_null() {
                    continue;
                }
                let ordinal = *first_seen.entry(id.to_owned()).or_insert_with(|| {
                    let assigned = next_first_seen;
                    next_first_seen += 1;
                    assigned
                });
                let node = FeatureNode { step: source_step as i32, first_seen: ordinal };
                node_ids.entry(node).or_insert_with(|| id.to_owned());
                edges.entry(node).or_default();
                sequence.push(node);
                if source_biome == biome
                    && source_step == step
                    && resolver
                        .placed_feature(id)
                        .get("feature")
                        .and_then(Value::as_str)
                        .is_some_and(|configured| {
                            resolver.configured_feature(configured).get("type").and_then(Value::as_str) == Some(kind)
                        })
                {
                    target_ids.insert(id.to_owned());
                }
            }
        }
        for pair in sequence.windows(2) {
            edges.entry(pair[0]).or_default().insert(pair[1]);
        }
    }

    if target_ids.is_empty() {
        return None;
    }
    let mut discovered = HashSet::new();
    let mut visiting = HashSet::new();
    let mut ordered = Vec::with_capacity(node_ids.len());
    for node in edges.keys().copied() {
        if visit_feature_node(node, &edges, &mut discovered, &mut visiting, &mut ordered) {
            return None;
        }
    }
    ordered.reverse();

    ordered
        .into_iter()
        .filter(|node| node.step == step as i32)
        .enumerate()
        .find_map(|(index, node)| {
            node_ids
                .get(&node)
                .filter(|id| target_ids.contains(*id))
                .map(|_| index)
        })
}

/// Resolves the End spike feature's optional explicit layout.
///
/// The feature's codec treats an absent or empty `spikes` list as a request
/// for the seed-derived ten-spike layout. A non-empty list carries the complete
/// geometry, so each entry is retained rather than silently replacing it with
/// the default ring.
fn spike_config_in_step(resolver: &dyn Resolver, biome: &str, step: usize) -> Option<Vec<EndSpike>> {
    let document = resolver.biome_document(biome);
    let entries = document
        .get("features")
        .and_then(Value::as_array)
        .and_then(|steps| steps.get(step))
        .and_then(Value::as_array)?;

    for id in entries.iter().filter_map(Value::as_str) {
        let placed = resolver.placed_feature(id);
        let Some(configured_id) = placed.get("feature").and_then(Value::as_str) else {
            continue;
        };
        let configured = resolver.configured_feature(configured_id);
        if configured.get("type").and_then(Value::as_str) != Some("minecraft:end_spike") {
            continue;
        }
        let spikes = configured
            .get("config")
            .and_then(|config| config.get("spikes"))
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(parse_end_spike).collect())
            .unwrap_or_default();
        return Some(spikes);
    }
    None
}

/// Parses one `EndSpike` record. The bundled codec gives every field a zero or
/// false default; preserving those defaults keeps malformed-but-readable
/// records deterministic while valid records retain all configured geometry.
fn parse_end_spike(value: &Value) -> Option<EndSpike> {
    let object = value.as_object()?;
    let integer = |key: &str| -> i32 {
        object
            .get(key)
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(0)
    };
    Some(EndSpike {
        center_x: integer("centerX"),
        center_z: integer("centerZ"),
        radius: integer("radius"),
        height: integer("height"),
        guarded: object.get("guarded").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Resolves the first End-gateway configured feature in a biome step.
///
/// An End gateway can be configured without an exit (the delayed gameplay
/// gateway uses that form), but the block-column output can only retain
/// gateways whose exit metadata is known.  The worldgen End document points at
/// the exit-bearing return gateway, so a malformed or delayed configuration is
/// rejected here instead of being silently emitted with a guessed destination.
fn gateway_config_in_step(
    resolver: &dyn Resolver,
    biome: &str,
    step: usize,
) -> Option<GatewayConfig> {
    let document = resolver.biome_document(biome);
    let entries = document
        .get("features")
        .and_then(Value::as_array)
        .and_then(|steps| steps.get(step))
        .and_then(Value::as_array)?;

    for id in entries.iter().filter_map(Value::as_str) {
        let placed = resolver.placed_feature(id);
        let Some(configured_id) = placed.get("feature").and_then(Value::as_str) else {
            continue;
        };
        let configured = resolver.configured_feature(configured_id);
        if configured.get("type").and_then(Value::as_str) != Some("minecraft:end_gateway") {
            continue;
        }
        let Some(config) = configured.get("config") else {
            continue;
        };
        let Some(exit) = config.get("exit").and_then(parse_position) else {
            continue;
        };
        let exact = config.get("exact").and_then(Value::as_bool).unwrap_or(false);
        return Some(GatewayConfig { exit, exact });
    }
    None
}

fn parse_position(value: &Value) -> Option<(i32, i32, i32)> {
    let values = value.as_array()?;
    let [x, y, z] = values.as_slice() else {
        return None;
    };
    Some((
        i32::try_from(x.as_i64()?).ok()?,
        i32::try_from(y.as_i64()?).ok()?,
        i32::try_from(z.as_i64()?).ok()?,
    ))
}

fn surface_y(world: &DenseBlockGrid, x: i32, z: i32) -> i32 {
    for y in (0..128).rev() {
        let state = world.get_id(x, y, z);
        let Some(state) = world.interner().canonical_id(state) else {
            continue;
        };
        if lodestone_data::block_solidity::blocks_motion(state)
            || lodestone_data::snow_support::has_fluid_state(state)
        {
            return y + 1;
        }
    }
    0
}

fn write_gateway(world: &mut DenseBlockGrid, origin: (i32, i32, i32)) {
    for y in origin.1 - 2..=origin.1 + 2 {
        for x in origin.0 - 1..=origin.0 + 1 {
            for z in origin.2 - 1..=origin.2 + 1 {
                let same_x = x == origin.0;
                let same_y = y == origin.1;
                let same_z = z == origin.2;
                let end = (y - origin.1).abs() == 2;
                let state = if same_x && same_y && same_z {
                    "minecraft:end_gateway"
                } else if same_y {
                    "minecraft:air"
                } else if (end && same_x && same_z) || ((same_x || same_z) && !end) {
                    "minecraft:bedrock"
                } else {
                    "minecraft:air"
                };
                world.set(x, y, z, state);
            }
        }
    }
}

/// Writes one outer-island feature at its already-selected origin.
///
/// The region driver performs its rarity/count/in-square selection once per
/// surrounding source chunk, then this primitive writes the resulting shape.
pub(crate) fn place_outer_island<R: RandomSource>(
    world: &mut DenseBlockGrid,
    random: &mut R,
    origin: (i32, i32, i32),
) {
    let mut radius = random.next_int_bounded(3) as f32 + 4.0;
    let mut y_offset = 0;
    while radius > 0.5 {
        let lower = (-radius).floor() as i32;
        let upper = radius.ceil() as i32;
        for x_offset in lower..=upper {
            for z_offset in lower..=upper {
                if (x_offset * x_offset + z_offset * z_offset) as f32 <= (radius + 1.0) * (radius + 1.0) {
                    world.set(
                        origin.0 + x_offset,
                        origin.1 + y_offset,
                        origin.2 + z_offset,
                        "minecraft:end_stone",
                    );
                }
            }
        }
        radius -= random.next_int_bounded(2) as f32 + 0.5;
        y_offset -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::density::{NoiseParams, Resolver};
    use crate::rng::LegacyRandomSource;

    struct GatewayResolver {
        configured: Value,
    }

    impl Resolver for GatewayResolver {
        fn density_function(&self, _id: &str) -> Value {
            Value::Null
        }

        fn noise(&self, _id: &str) -> NoiseParams {
            NoiseParams { first_octave: 0, amplitudes: Vec::new() }
        }

        fn biome_document(&self, id: &str) -> Value {
            if id == THE_END {
                serde_json::json!({ "features": [[], [], [], [], [], [], [], [], [], [], []] })
            } else if id == END_HIGHLANDS {
                serde_json::json!({
                    "features": [[], [], [], [], ["minecraft:test_gateway"]]
                })
            } else {
                Value::Null
            }
        }

        fn placed_feature(&self, id: &str) -> Value {
            if id == "minecraft:test_gateway" {
                serde_json::json!({
                    "feature": "minecraft:test_gateway",
                    "placement": []
                })
            } else {
                Value::Null
            }
        }

        fn configured_feature(&self, id: &str) -> Value {
            if id == "minecraft:test_gateway" {
                self.configured.clone()
            } else {
                Value::Null
            }
        }
    }

    /// The native feature fixture is intentionally a direct feature invocation:
    /// placement modifiers belong to the future three-by-three region driver.
    #[test]
    fn outer_island_shape_matches_the_independent_feature_fixture() {
        let mut world = DenseBlockGrid::new(-32, 0, -32, 64, 128, 64, "minecraft:air");
        let mut random = LegacyRandomSource::new(918_273);
        place_outer_island(&mut world, &mut random, (24, 70, 24));

        let mut writes = 0usize;
        for line in include_str!("../../tests/support/end_decoration_jvm.txt").lines() {
            let mut words = line.split_whitespace();
            if words.next() != Some("island") {
                continue;
            }
            let mut position = words.next().expect("island position").split(',');
            let x: i32 = position.next().expect("x").parse().expect("integer x");
            let y: i32 = position.next().expect("y").parse().expect("integer y");
            let z: i32 = position.next().expect("z").parse().expect("integer z");
            assert!(position.next().is_none(), "extra island coordinate: {line}");
            let state = words.next().expect("island state");
            assert!(words.next().is_none(), "trailing island fixture data: {line}");
            assert_eq!(world.get(x, y, z), state, "island fixture write ({x},{y},{z})");
            writes += 1;
        }
        assert_eq!(writes, 402, "fixture must exercise a non-trivial island");
    }

    /// The source chunk's biome is not a substitute for the final placement
    /// modifier. A source-centre `end_barrens` answer can still produce an
    /// eligible `small_end_islands` sample at one of the in-square origins.
    /// The true arm accepts every sampled origin; the control rejects every
    /// sampled origin while using the same source seed and therefore writes no
    /// island. This also guards that the rarity/count/square/height draws stay
    /// ahead of the per-origin biome decision.
    #[test]
    fn outer_island_filter_runs_per_sampled_origin() {
        let mut accepted_seed = None;
        for decoration_seed in 0..100_000 {
            let mut world = DenseBlockGrid::new(-16, 0, -16, 48, 128, 48, "minecraft:air");
            apply_outer_islands(
                &mut world,
                &mut WorldgenRandom::new(XoroshiroRandomSource::new(0)),
                decoration_seed,
                0,
                0,
                0,
                |_, _, _| true,
            );
            let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
            let has_island = (min_x..min_x + size_x).any(|x| {
                (min_y..min_y + size_y).any(|y| {
                    (min_z..min_z + size_z).any(|z| world.get(x, y, z) == "minecraft:end_stone")
                })
            });
            if has_island {
                accepted_seed = Some(decoration_seed);
                break;
            }
        }
        let decoration_seed = accepted_seed.expect("test search must find a rarity pass");
        let mut rejected = DenseBlockGrid::new(-16, 0, -16, 48, 128, 48, "minecraft:air");
        apply_outer_islands(
            &mut rejected,
            &mut WorldgenRandom::new(XoroshiroRandomSource::new(0)),
            decoration_seed,
            0,
            0,
            0,
            |_, _, _| false,
        );
        let (min_x, min_y, min_z, size_x, size_y, size_z) = rejected.bounds();
        assert!((min_x..min_x + size_x).all(|x| {
            (min_y..min_y + size_y).all(|y| {
                (min_z..min_z + size_z).all(|z| rejected.get(x, y, z) == "minecraft:air")
            })
        }), "an ineligible sampled origin must suppress only that feature placement");
    }

    /// Causal control for the production path: search an outer ring until the
    /// source-centre biome is ineligible but one of that source's sampled
    /// origins resolves to `small_end_islands`. The latter must still place the
    /// island at its origin.
    #[test]
    fn outer_island_uses_origin_biome_when_source_centre_is_ineligible() {
        let seed = 42;
        let decoration = EndDecoration::from_resolver(&FeatureOrderResolver);
        let biome_source = EndBiomeSource::new(seed);
        let mut found = false;
        'sources: for source_x in 240..272 {
            for source_z in 240..272 {
                let center = biome_source.biome_at_quart_typed(source_x * 4, 0, source_z * 4);
                if center == BuiltinBiome::SmallEndIslands {
                    continue;
                }
                let mut candidates = Vec::new();
                let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
                let decoration_seed = random.set_decoration_seed(seed, source_x * 16, source_z * 16);
                let mut scratch = DenseBlockGrid::new(-16, 0, -16, 48, 128, 48, "minecraft:air");
                apply_outer_islands(
                    &mut scratch,
                    &mut random,
                    decoration_seed,
                    0,
                    source_x,
                    source_z,
                    |x, y, z| {
                        candidates.push((x, y, z));
                        false
                    },
                );
                let Some(origin) = candidates
                    .into_iter()
                    .find(|&(x, y, z)| biome_source.biome_at_block_typed(x, y, z) == BuiltinBiome::SmallEndIslands)
                else {
                    continue;
                };
                let mut world = DenseBlockGrid::new(source_x * 16 - 16, 0, source_z * 16 - 16, 48, 128, 48, "minecraft:air");
                decoration.apply_source(
                    seed,
                    source_x,
                    source_z,
                    &mut world,
                    EndBiomeSet::around_source(source_x, source_z, |x, z| {
                        biome_source.biome_at_quart_typed(x * 4, 0, z * 4)
                    }),
                );
                assert_eq!(world.get(origin.0, origin.1, origin.2), "minecraft:end_stone");
                found = true;
                break 'sources;
            }
        }
        assert!(found, "outer-ring search must find a cross-boundary sampled origin");
    }

    #[test]
    fn chorus_plant_uses_the_resolved_support_tag_only_for_down() {
        let mut world = DenseBlockGrid::new(-1, 0, -1, 3, 3, 3, "minecraft:air");
        world.set(0, 0, 0, "minecraft:end_stone");
        let supports = HashSet::from(["minecraft:end_stone".to_owned()]);
        assert_eq!(
            chorus_plant_state(&world, (0, 1, 0), &supports),
            "minecraft:chorus_plant[down=true,east=false,north=false,south=false,up=false,west=false]",
        );
        assert_eq!(
            chorus_plant_state(&world, (0, 1, 0), &HashSet::new()),
            "minecraft:chorus_plant[down=false,east=false,north=false,south=false,up=false,west=false]",
        );
    }

    #[test]
    fn chorus_shape_matches_the_independent_feature_fixture() {
        let mut world = DenseBlockGrid::new(-32, 0, -32, 64, 128, 64, "minecraft:air");
        world.set(0, 64, 0, "minecraft:end_stone");
        let mut random = LegacyRandomSource::new(12_345);
        grow_chorus(&mut world, &mut random, (0, 65, 0), (0, 65, 0), 0, &HashSet::new());

        let mut writes = 0usize;
        for line in include_str!("../../tests/support/end_decoration_jvm.txt").lines() {
            let mut words = line.split_whitespace();
            if words.next() != Some("chorus") {
                continue;
            }
            let mut position = words.next().expect("chorus coordinate").split(',');
            let x: i32 = position.next().expect("x").parse().expect("integer x");
            let y: i32 = position.next().expect("y").parse().expect("integer y");
            let z: i32 = position.next().expect("z").parse().expect("integer z");
            assert!(position.next().is_none(), "extra coordinate: {line}");
            let state = words.next().expect("chorus state");
            assert!(words.next().is_none(), "trailing chorus fixture data: {line}");
            assert_eq!(world.get(x, y, z), state, "chorus write ({x}, {y}, {z})");
            writes += 1;
        }
        assert_eq!(writes, 19, "fixture must include every native chorus write");
    }

    #[test]
    fn return_gateway_shape_and_exit_match_the_independent_feature_fixture() {
        let mut world = DenseBlockGrid::new(0, 0, 0, 128, 128, 128, "minecraft:air");
        write_gateway(&mut world, (50, 70, 50));

        let mut writes = 0usize;
        let mut exit = None;
        for line in include_str!("../../tests/support/end_decoration_jvm.txt").lines() {
            let mut words = line.split_whitespace();
            match words.next() {
                Some("gateway") => {
                    let mut position = words.next().expect("gateway coordinate").split(',');
                    let x: i32 = position.next().expect("x").parse().expect("integer x");
                    let y: i32 = position.next().expect("y").parse().expect("integer y");
                    let z: i32 = position.next().expect("z").parse().expect("integer z");
                    assert!(position.next().is_none(), "extra coordinate: {line}");
                    let state = words.next().expect("gateway state");
                    assert!(words.next().is_none(), "trailing gateway fixture data: {line}");
                    assert_eq!(world.get(x, y, z), state, "gateway write ({x}, {y}, {z})");
                    writes += 1;
                }
                Some("gateway_exit") => {
                    let mut position = words.next().expect("gateway exit").split(',');
                    let x: i32 = position.next().expect("x").parse().expect("integer x");
                    let y: i32 = position.next().expect("y").parse().expect("integer y");
                    let z: i32 = position.next().expect("z").parse().expect("integer z");
                    assert!(position.next().is_none(), "extra coordinate: {line}");
                    assert_eq!(words.next(), Some("exact=true"), "gateway exit setting: {line}");
                    assert!(words.next().is_none(), "trailing gateway exit data: {line}");
                    exit = Some(EndGateway { pos: (50, 70, 50), exit: (x, y, z), exact: true });
                }
                _ => {}
            }
        }
        assert_eq!(writes, 45, "fixture must include the complete gateway box");
        assert_eq!(exit, Some(EndGateway { pos: (50, 70, 50), exit: (100, 50, 0), exact: true }));
    }

    /// **Control** for the old hardcoded destination: a configured return
    /// gateway with a different exit and exactness must reach the decoration
    /// state unchanged.  Reading only the feature type would leave the
    /// historical `(100, 50, 0), exact=true` pair in place and this would fail.
    #[test]
    fn return_gateway_uses_configured_exit_and_exact_flag() {
        let resolver = GatewayResolver {
            configured: serde_json::json!({
                "type": "minecraft:end_gateway",
                "config": { "exit": [-17, 88, 203], "exact": false }
            }),
        };
        let decoration = EndDecoration::from_resolver(&resolver);
        assert_eq!(
            decoration.gateway_return,
            Some(GatewayConfig { exit: (-17, 88, 203), exact: false })
        );
    }

    /// A delayed gateway has no configured exit and belongs to gameplay's
    /// destination-search path.  It must not accidentally enable the
    /// worldgen return-gateway writer with a guessed destination.
    #[test]
    fn gateway_without_configured_exit_does_not_enable_return_writer() {
        let resolver = GatewayResolver {
            configured: serde_json::json!({
                "type": "minecraft:end_gateway",
                "config": { "exact": false }
            }),
        };
        let decoration = EndDecoration::from_resolver(&resolver);
        assert_eq!(decoration.gateway_return, None);
    }

    struct SpikeResolver;

    impl Resolver for SpikeResolver {
        fn density_function(&self, _id: &str) -> Value {
            Value::Null
        }

        fn noise(&self, _id: &str) -> NoiseParams {
            NoiseParams { first_octave: 0, amplitudes: Vec::new() }
        }

        fn biome_document(&self, id: &str) -> Value {
            if id == THE_END {
                serde_json::json!({
                    "features": [[], [], [], [], ["minecraft:test_spike"]]
                })
            } else {
                Value::Null
            }
        }

        fn placed_feature(&self, id: &str) -> Value {
            if id == "minecraft:test_spike" {
                serde_json::json!({
                    "feature": "minecraft:test_spike",
                    "placement": [{ "type": "minecraft:biome" }]
                })
            } else {
                Value::Null
            }
        }

        fn configured_feature(&self, id: &str) -> Value {
            if id == "minecraft:test_spike" {
                serde_json::json!({
                    "type": "minecraft:end_spike",
                    "config": {
                        "spikes": [{
                            "centerX": 0,
                            "centerZ": 0,
                            "radius": 1,
                            "height": 70,
                            "guarded": true
                        }]
                    }
                })
            } else {
                Value::Null
            }
        }
    }

    /// A non-empty configured spike list is the feature's explicit geometry,
    /// not a hint to regenerate the ten-spike ring from the world seed. This
    /// test drives the production region writer so a list that only parses but
    /// never reaches a served grid cannot pass.
    #[test]
    fn explicit_spike_layout_reaches_the_region_writer() {
        let decoration = EndDecoration::from_resolver(&SpikeResolver);
        assert_eq!(decoration.spikes.as_ref().map(Vec::len), Some(1));

        let mut world = DenseBlockGrid::new(-16, 0, -16, 48, 128, 48, "minecraft:air");
        decoration.apply_region(17, 0, 0, &mut world, |_, _| BuiltinBiome::TheEnd);

        assert_eq!(world.get(0, 69, 0), "minecraft:obsidian", "configured center/radius must be used");
        assert_eq!(world.get(2, 69, 0), "minecraft:air", "the configured radius must clip the pillar");
        assert_eq!(
            world.get(2, 70, 0),
            "minecraft:iron_bars[north=true,south=true,west=false,east=false]",
            "the configured guarded flag must reach cage placement",
        );
    }

    struct FeatureOrderResolver;

    impl Resolver for FeatureOrderResolver {
        fn density_function(&self, _id: &str) -> Value {
            Value::Null
        }

        fn noise(&self, _id: &str) -> NoiseParams {
            NoiseParams { first_octave: 0, amplitudes: Vec::new() }
        }

        fn biome_document(&self, id: &str) -> Value {
            match id {
                THE_END => serde_json::json!({
                    "features": [[], [], [], [], ["minecraft:end_spike"], [], [], [], [], [], ["minecraft:end_platform"]]
                }),
                END_HIGHLANDS => serde_json::json!({
                    "features": [[], [], [], [], ["minecraft:end_gateway_return"], [], [], [], [], ["minecraft:chorus_plant"]]
                }),
                super::SMALL_END_ISLANDS => serde_json::json!({
                    "features": [["minecraft:end_island_decorated"]]
                }),
                _ => Value::Null,
            }
        }

        fn placed_feature(&self, id: &str) -> Value {
            if matches!(
                id,
                "minecraft:end_spike"
                    | "minecraft:end_platform"
                    | "minecraft:end_gateway_return"
                    | "minecraft:chorus_plant"
                    | "minecraft:end_island_decorated"
            ) {
                serde_json::json!({ "feature": id, "placement": [] })
            } else {
                Value::Null
            }
        }

        fn configured_feature(&self, id: &str) -> Value {
            let kind = match id {
                "minecraft:end_spike" => "minecraft:end_spike",
                "minecraft:end_platform" => "minecraft:end_platform",
                "minecraft:end_gateway_return" => "minecraft:end_gateway",
                "minecraft:chorus_plant" => "minecraft:chorus_plant",
                "minecraft:end_island_decorated" => "minecraft:end_island",
                _ => return Value::Null,
            };
            if kind == "minecraft:end_gateway" {
                serde_json::json!({
                    "type": kind,
                    "config": { "exit": [100, 50, 0], "exact": true }
                })
            } else {
                serde_json::json!({ "type": kind, "config": {} })
            }
        }
    }

    #[test]
    fn chorus_origin_filter_uses_the_sampled_biome_not_the_source_center() {
        let decoration = EndDecoration::from_resolver(&FeatureOrderResolver);
        let seed = 42;
        let mut world = DenseBlockGrid::new(2944, 0, 1392, 48, 128, 48, "minecraft:air");
        for z in 1392..1440 {
            for x in 2944..2992 {
                world.set(x, 63, z, "minecraft:end_stone");
            }
        }

        let biome_source = EndBiomeSource::new(seed);
        let gateways = decoration.apply_source(
            seed,
            185,
            87,
            &mut world,
            EndBiomeSet::around_source(185, 87, |x, z| {
                biome_source.biome_at_quart_typed(x * 4, 0, z * 4)
            }),
        );
        assert!(gateways.is_empty(), "this source does not produce a return gateway");
        assert!(
            (1392..1440).any(|z| (2944..2992).any(|x| is_chorus(world.get(x, 64, z)))),
            "a sampled highlands origin must remain eligible even when the source centre has another biome",
        );
    }

    /// The global order fixture is independent of the End driver's local
    /// resolver walk. In particular, the spike is index 1 because the gateway
    /// occupies index 0 in the same generation step, even though each feature
    /// is the first entry in its own biome document.
    #[test]
    fn end_feature_indices_match_the_independent_order_fixture() {
        let resolver = FeatureOrderResolver;
        let decoration = EndDecoration::from_resolver(&resolver);
        let mut rows = 0usize;
        for line in include_str!("../../tests/support/end_feature_order_jvm.txt").lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 4, "malformed feature-order fixture row: {line}");
            assert_eq!(fields[0], "feature");
            let step: usize = fields[1].parse().expect("feature step");
            let index: usize = fields[2].parse().expect("feature index");
            let actual = match fields[3] {
                "minecraft:end_island_decorated" => feature_index_in_step(&resolver, SMALL_END_ISLANDS, step, "minecraft:end_island"),
                "minecraft:end_gateway_return" => feature_index_in_step(&resolver, END_HIGHLANDS, step, "minecraft:end_gateway"),
                "minecraft:end_spike" => feature_index_in_step(&resolver, THE_END, step, "minecraft:end_spike"),
                "minecraft:chorus_plant" => feature_index_in_step(&resolver, END_HIGHLANDS, step, "minecraft:chorus_plant"),
                "minecraft:end_platform" => feature_index_in_step(&resolver, THE_END, step, "minecraft:end_platform"),
                other => panic!("unknown feature in order fixture: {other}"),
            };
            assert_eq!(actual, Some(index), "feature-order row: {line}");
            rows += 1;
        }
        assert_eq!(rows, 5, "fixture must cover every End feature");
        assert_eq!(decoration.spike_index, Some(1), "the production decoration state must retain the global spike index");
        assert_eq!(decoration.gateway_index, Some(0), "the production decoration state must retain the global gateway index");
    }
}
