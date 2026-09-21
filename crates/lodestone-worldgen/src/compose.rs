//! Version-free composition glue: resolves per-biome carver
//! lists, per-biome ore-feature lists, and block-tag closures from a
//! [`Resolver`], for [`crate::overworld::OverworldGenerator`] to consume when
//! composing carvers/features into the served chunk. Holds no data of its
//! own — everything here reads through the `Resolver` trait, matching every
//! other module in this crate (plan §3) — and every lookup degrades to "no
//! data for this id" rather than panicking, so a `Resolver` that only
//! supplies shape/surface data (most of this crate's own test fixtures) is
//! unaffected by this module existing.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;

use crate::carver::CarverConfig;
use crate::density::Resolver;
use crate::feature::{
    FeatureMembershipId, PlacedOre, RuleTest, STEP_UNDERGROUND_ORES, parse_ore_config,
    parse_placements,
};
use lodestone_data::biomes::{BiomeRef, BuiltinBiome};
use lodestone_data::block_states::StateId;

/// Recursively resolves a block tag's closure into a set of base block names.
/// Sub-tag references (`"#minecraft:..."`) recurse; plain ids are added
/// directly. Mirrors the resolution `carver_parity.rs`/`feature_parity.rs`'s
/// own hand-rolled `resolve_block_tag` test helpers already perform by
/// reading tag JSON straight off disk, just routed through
/// [`Resolver::block_tag`] so it also works against embedded server data
/// (and any other `Resolver`), not only a fixture directory.
///
/// A tag id with no data (`Resolver::block_tag`'s default `Value::Null`, or
/// a tag genuinely absent from the version's data) resolves to "no members"
/// rather than panicking — composition degrades gracefully, not loudly,
/// because an unrecognised tag is common (e.g. a `Resolver` that only ships
/// the shape/surface subset).
pub fn resolve_block_tag(
    resolver: &dyn Resolver,
    id: &str,
    out: &mut HashSet<String>,
    seen: &mut HashSet<String>,
) {
    if !seen.insert(id.to_string()) {
        return;
    }
    let doc = resolver.block_tag(id);
    let Some(values) = doc.get("values").and_then(Value::as_array) else {
        return;
    };
    for entry in values {
        let s = match entry {
            Value::String(s) => s.as_str(),
            Value::Object(o) => o.get("id").and_then(Value::as_str).unwrap_or_default(),
            _ => continue,
        };
        if let Some(sub) = s.strip_prefix('#') {
            resolve_block_tag(resolver, sub, out, seen);
        } else if !s.is_empty() {
            out.insert(s.to_string());
        }
    }
}

/// Resolves one biome's `carvers` list into parsed [`CarverConfig`]s, in the
/// biome JSON's own declared order — `WorldgenRandom::set_large_feature_seed`'s
/// `index` (see [`crate::carver::apply_carvers`]) is the position in *this*
/// list, so order matters and remains the JSON array's own order; nothing
/// reorders it.
///
/// The data representation is either one carver id or an ordered array of ids.
/// Empty if [`Resolver::biome_document`] has no data for `biome` — a biome
/// genuinely absent from the resolver's data carves nothing, matching the
/// earlier behaviour for a `Resolver` that never implemented this method.
#[must_use]
pub fn build_biome_carvers(resolver: &dyn Resolver, biome: &str) -> Vec<CarverConfig> {
    let doc = resolver.biome_document(biome);
    let Some(carvers) = doc.get("carvers") else {
        return Vec::new();
    };
    match carvers {
        Value::String(id) => vec![CarverConfig::parse(&resolver.configured_carver(id))],
        Value::Array(ids) => ids
            .iter()
            .filter_map(Value::as_str)
            .map(|id| CarverConfig::parse(&resolver.configured_carver(id)))
            .collect(),
        _ => Vec::new(),
    }
}

/// Resolves one placed-feature id when its configured feature is an ore.
///
/// The returned [`PlacedOre`] carries a placeholder index because a placed
/// feature's actual decoration index belongs to the global per-step ordering,
/// not to the feature document. [`DecorationCatalog::select_ores`] assigns that
/// index at selection time.
fn parse_placed_ore(resolver: &dyn Resolver, placed_id: &str) -> Option<PlacedOre> {
    let placed = resolver.placed_feature(placed_id);
    if placed.is_null() {
        return None;
    }
    let cf_id = placed.get("feature").and_then(Value::as_str)?;
    let configured = resolver.configured_feature(cf_id);
    (configured.get("type").and_then(Value::as_str) == Some("minecraft:ore")).then(|| PlacedOre {
        registry_id: Some(placed_id.to_string()),
        index: 0,
        placements: parse_placements(&placed),
        config: {
            let mut config = parse_ore_config(&configured["config"]);
            crate::feature::compile_ore_targets(resolver, &mut config.targets);
            config
        },
    })
}

const BIOME_MASK_WORDS: usize = 2;
const _: () = assert!(BuiltinBiome::COUNT <= (BIOME_MASK_WORDS * 64) as u8);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BiomeMask([u64; BIOME_MASK_WORDS]);

impl BiomeMask {
    fn insert(&mut self, biome: BuiltinBiome) {
        let index = biome as usize;
        self.0[index / 64] |= 1_u64 << (index % 64);
    }

    fn contains(self, biome: BuiltinBiome) -> bool {
        let index = biome as usize;
        self.0[index / 64] & (1_u64 << (index % 64)) != 0
    }
}

/// Immutable numeric eligibility compiled from the biome feature lists.
/// Built-in biomes are represented by the generated enum discriminants; a
/// feature's hot-path check is consequently one token lookup and one mask bit.
/// Names remain only in the setup-time token table and legacy constructor
/// inputs; extension identities use the cold typed arm below.
#[derive(Clone, Debug, Default)]
pub struct FeatureBiomePlan {
    masks: Arc<[BiomeMask]>,
    extensions: Arc<[Vec<BiomeRef>]>,
    tokens: Arc<HashMap<String, FeatureMembershipId>>,
}

impl FeatureBiomePlan {
    fn new(
        masks: Vec<BiomeMask>,
        extensions: Vec<Vec<BiomeRef>>,
        tokens: HashMap<String, FeatureMembershipId>,
    ) -> Self {
        Self {
            masks: masks.into(),
            extensions: extensions.into(),
            tokens: Arc::new(tokens),
        }
    }

    /// Returns the setup-only token for a registry id.
    #[must_use]
    pub fn token_for(&self, id: &str) -> Option<FeatureMembershipId> {
        self.tokens.get(id).copied()
    }

    /// Numeric built-in membership check used by the production placement
    /// paths. Extension ids are deliberately handled by the cold fallback.
    #[must_use]
    pub fn allows(&self, token: FeatureMembershipId, biome: BiomeRef) -> bool {
        let Some(mask) = self.masks.get(token.0 as usize).copied() else {
            return false;
        };
        biome.builtin_or_none().map_or_else(
            || self.allows_extension(token, biome),
            |builtin| mask.contains(builtin),
        )
    }

    /// Extension identities are intentionally a separate cold arm. They are
    /// supplied as typed registry ids by an extension-aware setup boundary;
    /// no resource name is parsed or looked up while placing a candidate.
    #[cold]
    fn allows_extension(&self, token: FeatureMembershipId, biome: BiomeRef) -> bool {
        self.extensions
            .get(token.0 as usize)
            .is_some_and(|members| members.contains(&biome))
    }

    #[must_use]
    pub fn from_legacy(map: &HashMap<String, HashSet<String>>) -> Self {
        let mut tokens = HashMap::new();
        let mut masks = Vec::new();
        for (feature, biomes) in map {
            let token = FeatureMembershipId(masks.len() as u32);
            tokens.insert(feature.clone(), token);
            let mut mask = BiomeMask::default();
            for biome in biomes {
                if let Some(builtin) = BuiltinBiome::from_name(biome) {
                    mask.insert(builtin);
                }
            }
            masks.push(mask);
        }
        Self::new(masks, vec![Vec::new(); tokens.len()], tokens)
    }
}

/// Globally sorted decoration features plus each biome's membership in that
/// order. Decoration seeds use the global index, not the entry's local index
/// in a biome document, so a source with more than one biome needs this shared
/// catalog before it can select a subset to place.
#[derive(Clone, Debug, Default)]
pub struct DecorationCatalog {
    ordered: Arc<[DecorationEntry]>,
    members: HashMap<String, Arc<[u64]>>,
    /// Immutable numeric feature-to-biome eligibility shared by every replay
    /// context. The catalog is generator-scoped and never changes.
    feature_biomes: Arc<FeatureBiomePlan>,
}

#[derive(Clone, Debug)]
struct DecorationEntry {
    step: i32,
    index: usize,
    id: String,
    placed: Arc<crate::feature::vegetation::PlacedRef>,
}

/// All decoration streams selected for one source biome union.
///
/// The catalog has one global order. Keeping these streams together lets the
/// caller build the selected `(step, id)` set once and walk that order once,
/// while still exposing the same per-stream vectors that the dispatchers use.
#[derive(Debug, Default)]
pub(crate) struct DecorationSelection {
    pub features: Vec<(i32, usize, Arc<crate::feature::vegetation::PlacedRef>)>,
    pub step6_disks: Vec<(i32, usize, Arc<crate::feature::vegetation::PlacedRef>)>,
    pub step6_non_ore: Vec<(i32, usize, Arc<crate::feature::vegetation::PlacedRef>)>,
    pub ores: Vec<PlacedOre>,
}

impl DecorationCatalog {
    /// Whether no configured placed feature was available from any biome.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    /// Selects every driven decoration stream in one catalog walk.
    ///
    /// The selected biome set is compiled into a bitset and walked in global
    /// order. The raw index is part of the decoration RNG identity, so a
    /// selector must not independently reconstruct or sort it.
    pub(crate) fn select_all<'a>(
        &self,
        biomes: impl IntoIterator<Item = &'a str>,
        ore_definitions: &HashMap<String, PlacedOre>,
    ) -> DecorationSelection {
        let mut selected = vec![0_u64; self.ordered.len().div_ceil(64)];
        for biome in biomes {
            if let Some(words) = self.members.get(biome) {
                for (dst, src) in selected.iter_mut().zip(words.iter()) {
                    *dst |= *src;
                }
            }
        }

        let mut out = DecorationSelection::default();
        for (word_index, word) in selected.into_iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                let entry = &self.ordered[word_index * 64 + bit];
                if entry.step == STEP_UNDERGROUND_ORES {
                    match entry.placed.feature.as_ref() {
                        crate::feature::vegetation::ConfiguredFeature::Disk(_) => {
                            out.step6_disks.push((entry.step, entry.index, Arc::clone(&entry.placed)));
                        }
                        crate::feature::vegetation::ConfiguredFeature::UnderwaterMagma(_) => {
                            out.step6_non_ore.push((entry.step, entry.index, Arc::clone(&entry.placed)));
                        }
                        _ => {}
                    }
                    if let Some(mut ore) = ore_definitions.get(&entry.id).cloned() {
                        ore.index = entry.index;
                        out.ores.push(ore);
                    }
                } else {
                    out.features.push((entry.step, entry.index, Arc::clone(&entry.placed)));
                }
                bits &= bits - 1;
            }
        }
        out
    }

    /// Selects the deduplicated global feature order for the supplied biome set,
    /// excluding `UNDERGROUND_ORES`.
    ///
    /// That step has two consumers: [`Self::select_ores`] and
    /// [`Self::select_step6_disks`]. Both walk the same catalog and therefore
    /// retain the one raw per-step index even though they dispatch different
    /// configured-feature kinds.
    #[must_use]
    pub fn select<'a>(
        &'a self,
        biomes: impl IntoIterator<Item = &'a str>,
    ) -> Vec<(i32, usize, crate::feature::vegetation::PlacedRef)> {
        self.select_all(biomes, &HashMap::new())
            .features
            .into_iter()
            .map(|(step, index, placed)| (step, index, (*placed).clone()))
            .collect()
    }

    /// Selects disk features from `UNDERGROUND_ORES` with their global raw
    /// index. Ore entries and other unsupported step-6 entries remain in the
    /// index walk but are dispatched by their own engine or as no-ops.
    #[must_use]
    pub fn select_step6_disks<'a>(
        &'a self,
        biomes: impl IntoIterator<Item = &'a str>,
    ) -> Vec<(i32, usize, crate::feature::vegetation::PlacedRef)> {
        self.select_all(biomes, &HashMap::new())
            .step6_disks
            .into_iter()
            .map(|(step, index, placed)| (step, index, (*placed).clone()))
            .collect()
    }

    /// Selects modeled non-ore features from `UNDERGROUND_ORES` with their
    /// global raw index. This remains separate from [`Self::select`] because
    /// step 6 has an interleaved ore stream; currently underwater magma is the
    /// only non-ore body in that stream.
    #[must_use]
    pub fn select_step6_non_ore<'a>(
        &'a self,
        biomes: impl IntoIterator<Item = &'a str>,
    ) -> Vec<(i32, usize, crate::feature::vegetation::PlacedRef)> {
        self.select_all(biomes, &HashMap::new())
            .step6_non_ore
            .into_iter()
            .map(|(step, index, placed)| (step, index, (*placed).clone()))
            .collect()
    }

    /// Resolves all ore-capable selected entries with their **global** index in
    /// `UNDERGROUND_ORES`. The eligible set is the source chunk's complete
    /// section-biome container: one point biome is insufficient when an
    /// underground feature's eligible biome differs from the surface, while a
    /// neighbouring chunk's container belongs to that neighbour's source pass.
    ///
    /// `ore_definitions` is keyed by placed-feature id and contains only the
    /// configured features this engine can place as ores. Unsupported entries
    /// still advance the global per-step index, but produce no placement work.
    #[must_use]
    pub fn select_ores<'a>(
        &self,
        biomes: impl IntoIterator<Item = &'a str>,
        ore_definitions: &HashMap<String, PlacedOre>,
    ) -> Vec<PlacedOre> {
        self.select_all(biomes, ore_definitions).ores
    }

    /// The biomes which list each placed feature at any decoration step.
    /// [`crate::feature::vegetation::VegGrid`] uses this with its 3-D biome
    /// cells when a placement pipeline reaches the `biome` modifier.
    #[must_use]
    pub fn feature_biomes(&self) -> Arc<FeatureBiomePlan> {
        Arc::clone(&self.feature_biomes)
    }
}

/// Parses the ore-capable entries in a global decoration catalog once at
/// generator construction. The catalog retains every feature (including the
/// unsupported ones) for global index accounting; this map only avoids parsing
/// the same ore document each time a source chunk is decorated.
#[must_use]
pub fn build_ore_definitions(
    resolver: &dyn Resolver,
    catalog: &DecorationCatalog,
) -> HashMap<String, PlacedOre> {
    let mut out = HashMap::new();
    for entry in catalog.ordered.iter() {
        if let Some(mut ore) = parse_placed_ore(resolver, &entry.id) {
            if let Some(token) = catalog.feature_biomes.token_for(&entry.id) {
                crate::feature::Placement::bind_membership(&mut ore.placements, token);
            }
            out.entry(entry.id.clone()).or_insert(ore);
        }
    }
    out
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct DecorationNode {
    step: i32,
    first_seen: usize,
}

/// Builds the global feature order once from the biome-source order.
///
/// The externally observable order is a topological ordering of every
/// biome's complete feature sequence. Keeping entries from steps this crate
/// does not drive is deliberate: they still constrain the relative order of
/// entries in driven steps. `biome_order` must preserve the source's first
/// occurrence order rather than sorting biome names.
#[must_use]
pub fn build_decoration_catalog(
    resolver: &dyn Resolver,
    biome_order: &[String],
) -> DecorationCatalog {
    let mut first_seen = HashMap::<String, usize>::new();
    let mut placed = HashMap::<String, crate::feature::vegetation::PlacedRef>::new();
    let mut members = HashMap::<String, HashSet<(i32, String)>>::new();
    let mut edges = BTreeMap::<DecorationNode, BTreeSet<DecorationNode>>::new();
    let mut node_ids = HashMap::<DecorationNode, String>::new();
    let mut next_seen = 0usize;

    for biome in biome_order {
        let document = resolver.biome_document(biome);
        let Some(steps) = document.get("features").and_then(Value::as_array) else {
            continue;
        };
        let mut sequence = Vec::new();
        for (step, entries) in steps.iter().enumerate() {
            let Some(entries) = entries.as_array() else { continue };
            for entry in entries {
                let Some(id) = entry.as_str() else { continue };
                if resolver.placed_feature(id).is_null() {
                    continue;
                }
                let ordinal = *first_seen.entry(id.to_string()).or_insert_with(|| {
                    let assigned = next_seen;
                    next_seen += 1;
                    assigned
                });
                placed.entry(id.to_string()).or_insert_with(|| {
                    crate::feature::vegetation::resolve_placed_feature_ref(resolver, entry)
                });
                let node = DecorationNode { step: step as i32, first_seen: ordinal };
                edges.entry(node.clone()).or_default();
                node_ids.entry(node.clone()).or_insert_with(|| id.to_string());
                members.entry(biome.clone()).or_default().insert((step as i32, id.to_string()));
                sequence.push(node);
            }
        }
        for pair in sequence.windows(2) {
            edges.entry(pair[0].clone()).or_default().insert(pair[1].clone());
        }
    }

    fn visit(
        node: &DecorationNode,
        edges: &BTreeMap<DecorationNode, BTreeSet<DecorationNode>>,
        discovered: &mut BTreeSet<DecorationNode>,
        visiting: &mut BTreeSet<DecorationNode>,
        reverse: &mut Vec<DecorationNode>,
    ) {
        if discovered.contains(node) {
            return;
        }
        assert!(visiting.insert(node.clone()), "decoration feature order contains a cycle");
        for next in edges.get(node).into_iter().flatten() {
            visit(next, edges, discovered, visiting, reverse);
        }
        visiting.remove(node);
        discovered.insert(node.clone());
        reverse.push(node.clone());
    }

    let mut discovered = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    let mut reverse = Vec::new();
    for node in edges.keys() {
        visit(node, &edges, &mut discovered, &mut visiting, &mut reverse);
    }
    reverse.reverse();
    let ordered_nodes = reverse
        .into_iter()
        .map(|node| (node.step, node_ids.remove(&node).expect("every graph node has an id")))
        .collect::<Vec<_>>();
    let mut raw_indices = HashMap::<i32, usize>::new();
    let mut ordered = ordered_nodes
        .iter()
        .map(|(step, id)| {
            let index = raw_indices.entry(*step).or_default();
            let entry = DecorationEntry {
                step: *step,
                index: *index,
                id: id.clone(),
                placed: Arc::new(placed.get(id).expect("every catalog entry is placed").clone()),
            };
            *index += 1;
            entry
        })
        .collect::<Vec<_>>();
    let ordered_indices = ordered_nodes
        .iter()
        .enumerate()
        .map(|(index, (step, id))| ((*step, id.clone()), index))
        .collect::<HashMap<_, _>>();
    let mut tokens = HashMap::new();
    for (_, id) in &ordered_nodes {
        let next = tokens.len() as u32;
        tokens.entry(id.clone()).or_insert(FeatureMembershipId(next));
    }
    let mut masks = vec![BiomeMask::default(); tokens.len()];
    let extensions = vec![Vec::new(); tokens.len()];
    for (biome, entries) in &members {
        let parsed = BuiltinBiome::from_name(biome);
        for (_, id) in entries {
            let Some(token) = tokens.get(id).copied() else { continue };
            let index = token.0 as usize;
            if let Some(builtin) = parsed {
                masks[index].insert(builtin);
            }
        }
    }
    let feature_biomes = Arc::new(FeatureBiomePlan::new(masks, extensions, tokens));
    for (id, placed) in &mut placed {
        if let Some(token) = feature_biomes.token_for(id) {
            crate::feature::vegetation::bind_placed_feature_membership(placed, token);
        }
    }
    for entry in &mut ordered {
        entry.placed = Arc::new(placed.get(&entry.id).expect("every catalog entry is placed").clone());
    }
    let mut member_words = HashMap::<String, Vec<u64>>::new();
    for (biome, entries) in members {
        let words = member_words
            .entry(biome)
            .or_insert_with(|| vec![0; ordered.len().div_ceil(64)]);
        for (step, id) in entries {
            if let Some(&index) = ordered_indices.get(&(step, id)) {
                words[index / 64] |= 1_u64 << (index % 64);
            }
        }
    }
    DecorationCatalog {
        ordered: ordered.into(),
        members: member_words
            .into_iter()
            .map(|(biome, words)| (biome, words.into()))
            .collect(),
        feature_biomes,
    }
}

/// The decoration-step indices the [`DecorationCatalog`] drives, in source
/// order. The catalog retains unsupported entries for index accounting.
pub const DRIVEN_STEPS: &[i32] = &[
    0, // RAW_GENERATION
    1, // LAKES
    2, // LOCAL_MODIFICATIONS
    3, // UNDERGROUND_STRUCTURES  (monster_room/fossil are plain features)
    4, // SURFACE_STRUCTURES
    7, // UNDERGROUND_DECORATION
    8, // FLUID_SPRINGS
    crate::feature::STEP_VEGETAL_DECORATION,
];

/// Whether a biome document lists `minecraft:freeze_top_layer` in its
/// `TOP_LAYER_MODIFICATION` step.
///
/// In vanilla 26.2 **every** biome does — vanilla's own default per-biome
/// feature registration adds it
/// from a shared tail, so the step self-gates on temperature rather than on
/// biome membership, and `docs/plans/worldgen-parity.md`'s census row 6i verified
/// that across `assets/worldgen/biome/*.json`. This function exists anyway
/// because "every biome" is a property of the *data*, not of the engine: a
/// trimmed or modified datapack that omits the entry must produce a snow-free
/// world rather than snow the engine assumed.
///
/// Unlike the placed-feature selection in [`DecorationCatalog`], this does not consult
/// [`Resolver::placed_feature`]: `placed_feature/freeze_top_layer.json` carries
/// nothing the engine reads (its whole placement is `[{"type":
/// "minecraft:biome"}]`, and `configured_feature/freeze_top_layer.json`'s config
/// is `{}` — `NoneFeatureConfiguration`), so requiring it to resolve would gate
/// the step on an asset with no content.
#[must_use]
pub fn biome_lists_freeze_top_layer(document: &Value) -> bool {
    document
        .get("features")
        .and_then(Value::as_array)
        .and_then(|steps| {
            steps.get(crate::feature::top_layer::STEP_TOP_LAYER_MODIFICATION as usize)
        })
        .and_then(Value::as_array)
        .is_some_and(|step| {
            step.iter().any(|entry| {
                entry.as_str().is_some_and(|id| {
                    id.strip_prefix("minecraft:").unwrap_or(id) == "freeze_top_layer"
                })
            })
        })
}

/// Resolves every block tag referenced by `ores`' [`RuleTest::TagMatch`]
/// targets into a `tag id -> member block set` map, for
/// [`crate::feature::OreInput::in_tag`].
#[must_use]
pub fn build_ore_tag_map(
    resolver: &dyn Resolver,
    ores: &[PlacedOre],
) -> HashMap<String, HashSet<String>> {
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    for ore in ores {
        for target in &ore.config.targets {
            if let RuleTest::TagMatch(tag) = &target.target {
                map.entry(tag.clone()).or_insert_with(|| {
                    let mut out = HashSet::new();
                    let mut seen = HashSet::new();
                    resolve_block_tag(resolver, tag, &mut out, &mut seen);
                    out
                });
            }
        }
    }
    map
}

/// Where chunk-local `(lx, ly, lz)` lands in a column field built by
/// [`fill_column`] or read back by [`materialize_column`].
///
/// **Every caller must go through this rather than restating
/// `((ly * 16 + lz) * 16 + lx)`.** A restated index that transposed `lx` and `lz`
/// reads a mirrored column and reports a plausible-looking wrong answer, and the
/// two spellings could then drift apart independently — which is the same argument
/// `OverworldGenerator::shape_index` makes for forwarding to its own private `idx`.
#[must_use]
pub fn column_index(lx: i32, ly: i32, lz: i32, height: i32) -> usize {
    debug_assert!((0..height).contains(&ly));
    ((ly * 16 + lz) * 16 + lx) as usize
}

/// `fillFromNoise` for one chunk of a **disabled-aquifer** dimension: the
/// interpolated `final_density` plus the beard term, mapped to
/// [`BlockKind`](crate::aquifer::BlockKind).
///
/// Shared by [`crate::nether::NetherGenerator`] and
/// [`crate::end::EndGenerator`], which differ only in the settings they hand the
/// aquifer. The Overworld keeps its own copy because its fill is instrumented by
/// the allocation-attribution bench and carries a `StageGuard` this one must not.
///
/// # The two loops are a correctness property, not a micro-optimisation
///
/// An **empty** beardifier takes the loop that calls
/// [`AquiferSystem::block_at`](crate::aquifer::AquiferSystem::block_at) with no
/// addition at all. Adding `0.0` is the identity for every finite `f64` *except*
/// `-0.0`, whose sign bit it flips; nothing downstream distinguishes the two today
/// (`compute_substance` only asks `density > 0.0`), and the branch means that claim
/// about the rest of the pipeline never has to be made. It is also what keeps a
/// dimension with no adaptation-bearing structure bit-identical to the same
/// dimension before structures existed.
#[must_use]
pub fn fill_column(
    aquifer: &crate::aquifer::AquiferSystem,
    base_x: i32,
    base_z: i32,
    min_y: i32,
    height: i32,
    beard: &crate::structure::beardifier::Beardifier,
) -> Vec<crate::aquifer::BlockKind> {
    use crate::aquifer::BlockKind;
    let mut field = vec![BlockKind::Air; 16 * 16 * height as usize];
    if beard.is_empty() {
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..height {
                    field[column_index(lx, ly, lz, height)] =
                        aquifer.block_at(base_x + lx, min_y + ly, base_z + lz);
                }
            }
        }
        return field;
    }
    for lz in 0..16i32 {
        for lx in 0..16i32 {
            for ly in 0..height {
                let (wx, wy, wz) = (base_x + lx, min_y + ly, base_z + lz);
                field[column_index(lx, ly, lz, height)] =
                    aquifer.block_at_beard(wx, wy, wz, beard.compute(wx, wy, wz));
            }
        }
    }
    field
}

/// The heightmap the biome and surface stages consume: the highest local
/// `(lx, lz)` position whose block is *solid* (`BlockKind::Stone` — non-air,
/// non-fluid), floored at `sea_level - 1`.
///
/// This is `ComposedChunkOracle.java`'s `solidTop`, same definition and same
/// fallback, which is why biome sampling agrees between the two languages.
///
/// The floor matters in a dimension with no sea: the End's `sea_level` is `0` and
/// its `min_y` is `0`, so `sea_level - 1` is `min_y - 1` — the same "nothing solid
/// in this column" sentinel the loop already produces, rather than a spurious
/// clamp to a water line that does not exist.
#[must_use]
pub fn solid_top_heights(
    field: &[crate::aquifer::BlockKind],
    min_y: i32,
    height: i32,
    sea_level: i32,
) -> [i32; 256] {
    use crate::aquifer::BlockKind;
    let mut heights = [i32::MIN; 256];
    for lz in 0..16i32 {
        for lx in 0..16i32 {
            let mut top = min_y - 1;
            for ly in (0..height).rev() {
                if field[column_index(lx, ly, lz, height)] == BlockKind::Stone {
                    top = min_y + ly;
                    break;
                }
            }
            heights[(lz * 16 + lx) as usize] = top.max(sea_level - 1);
        }
    }
    heights
}

/// Turns a [`fill_column`] field plus a surface diff into the working block grid
/// the carve and structure stages mutate.
///
/// # The loop order is the specification
///
/// A [`crate::dense_grid::DenseBlockGrid`]'s palette is built in `set` order.
/// `surface_diff` is grouped by column and retains the surface scan's
/// descending Y order, so this fixed `(lz, lx, ly)` loop can consume each
/// column backwards without a coordinate conversion or a separate sort.
/// Keeping palette insertion here makes the output order explicit and stable
/// across independently constructed generators.
#[must_use]
pub fn materialize_column(
    field: &[crate::aquifer::BlockKind],
    surface_diff: &crate::surface::SurfaceDiff,
    base_x: i32,
    base_z: i32,
    min_y: i32,
    height: i32,
    solid: StateId,
    fluid: StateId,
) -> crate::dense_grid::DenseBlockGrid {
    use crate::aquifer::BlockKind;
    use lodestone_data::block_states::StateId;
    let mut world = crate::dense_grid::DenseBlockGrid::with_default(
        base_x,
        min_y,
        base_z,
        16,
        height,
        16,
        StateId::AIR,
    );
    for lz in 0..16i32 {
        for lx in 0..16i32 {
            let changes = surface_diff.column_slice(lx, lz);
            let mut next_change = changes.len();
            for ly in 0..height {
                let y = min_y + ly;
                let base = match field[column_index(lx, ly, lz, height)] {
                    BlockKind::Stone => solid,
                    BlockKind::Water | BlockKind::Lava => fluid,
                    BlockKind::Air => StateId::AIR,
                };
                let state = (next_change != 0 && changes[next_change - 1].0 == y)
                    .then(|| {
                        next_change -= 1;
                        let state = changes[next_change].1;
                        state
                    })
                    .unwrap_or(base);
                world.set_id(base_x + lx, y, base_z + lz, state);
            }
        }
    }
    world
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeResolver {
        tags: HashMap<&'static str, Value>,
        biomes: HashMap<&'static str, Value>,
        carvers: HashMap<&'static str, Value>,
        features: HashMap<&'static str, Value>,
        placed: HashMap<&'static str, Value>,
    }

    impl Resolver for FakeResolver {
        fn density_function(&self, _id: &str) -> Value {
            Value::Null
        }
        fn noise(&self, _id: &str) -> crate::density::NoiseParams {
            unimplemented!("not needed by this test")
        }
        fn block_tag(&self, id: &str) -> Value {
            self.tags.get(id).cloned().unwrap_or(Value::Null)
        }
        fn biome_document(&self, id: &str) -> Value {
            self.biomes.get(id).cloned().unwrap_or(Value::Null)
        }
        fn configured_carver(&self, id: &str) -> Value {
            self.carvers.get(id).cloned().unwrap_or(Value::Null)
        }
        fn configured_feature(&self, id: &str) -> Value {
            self.features.get(id).cloned().unwrap_or(Value::Null)
        }
        fn placed_feature(&self, id: &str) -> Value {
            self.placed.get(id).cloned().unwrap_or(Value::Null)
        }
    }

    #[test]
    fn resolve_block_tag_follows_subtag_references() {
        let mut tags = HashMap::new();
        tags.insert(
            "minecraft:leaf",
            serde_json::json!({"values": ["minecraft:oak_log", "#minecraft:sub"]}),
        );
        tags.insert(
            "minecraft:sub",
            serde_json::json!({"values": ["minecraft:stone"]}),
        );
        let resolver = FakeResolver {
            tags,
            biomes: HashMap::new(),
            carvers: HashMap::new(),
            features: HashMap::new(),
            placed: HashMap::new(),
        };
        let mut out = HashSet::new();
        let mut seen = HashSet::new();
        resolve_block_tag(&resolver, "minecraft:leaf", &mut out, &mut seen);
        assert_eq!(
            out,
            HashSet::from([
                "minecraft:oak_log".to_string(),
                "minecraft:stone".to_string()
            ])
        );
    }

    #[test]
    fn resolve_block_tag_missing_id_is_empty_not_panic() {
        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes: HashMap::new(),
            carvers: HashMap::new(),
            features: HashMap::new(),
            placed: HashMap::new(),
        };
        let mut out = HashSet::new();
        let mut seen = HashSet::new();
        resolve_block_tag(&resolver, "minecraft:does_not_exist", &mut out, &mut seen);
        assert!(out.is_empty());
    }

    #[test]
    fn build_biome_carvers_preserves_declared_order() {
        let mut biomes = HashMap::new();
        biomes.insert(
            "minecraft:test",
            serde_json::json!({"carvers": ["minecraft:a", "minecraft:b"]}),
        );
        let mut carvers = HashMap::new();
        let cave = |probability: f64| {
            serde_json::json!({
                "type": "minecraft:cave",
                "config": {
                    "probability": probability,
                    "y": {"type": "minecraft:uniform", "min_inclusive": {"absolute": 0}, "max_inclusive": {"absolute": 10}},
                    "yScale": 1.0,
                    "horizontal_radius_multiplier": 1.0,
                    "vertical_radius_multiplier": 1.0,
                    "floor_level": 0.0,
                    "lava_level": {"absolute": -54},
                    "replaceable": "#minecraft:overworld_carver_replaceables"
                }
            })
        };
        carvers.insert("minecraft:a", cave(0.1));
        carvers.insert("minecraft:b", cave(0.2));
        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes,
            carvers,
            features: HashMap::new(),
            placed: HashMap::new(),
        };
        let list = build_biome_carvers(&resolver, "minecraft:test");
        assert_eq!(list.len(), 2);
        let probs: Vec<f32> = list
            .iter()
            .map(|c| match c {
                CarverConfig::Cave(c) => c.probability,
                CarverConfig::Canyon(c) => c.probability,
            })
            .collect();
        assert_eq!(probs, vec![0.1_f32, 0.2_f32]);
    }

    #[test]
    fn build_biome_carvers_accepts_a_single_declared_id() {
        let mut biomes = HashMap::new();
        biomes.insert(
            "minecraft:test",
            serde_json::json!({"carvers": "minecraft:only"}),
        );
        let mut carvers = HashMap::new();
        carvers.insert(
            "minecraft:only",
            serde_json::json!({
                "type": "minecraft:nether_cave",
                "config": {
                    "probability": 0.2,
                    "y": {
                        "type": "minecraft:uniform",
                        "min_inclusive": {"absolute": 0},
                        "max_inclusive": {"absolute": 10}
                    },
                    "yScale": 0.5,
                    "horizontal_radius_multiplier": 1.0,
                    "vertical_radius_multiplier": 1.0,
                    "floor_level": -0.7,
                    "lava_level": {"absolute": 10},
                    "replaceable": "#minecraft:nether_carver_replaceables"
                }
            }),
        );
        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes,
            carvers,
            features: HashMap::new(),
            placed: HashMap::new(),
        };

        let list = build_biome_carvers(&resolver, "minecraft:test");
        assert_eq!(list.len(), 1, "a singleton carver must not be discarded");
        assert!(matches!(
            list[0],
            CarverConfig::Cave(ref cave) if cave.nether
        ));
    }

    #[test]
    fn build_biome_carvers_unknown_biome_is_empty() {
        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes: HashMap::new(),
            carvers: HashMap::new(),
            features: HashMap::new(),
            placed: HashMap::new(),
        };
        assert!(build_biome_carvers(&resolver, "minecraft:nowhere").is_empty());
    }

    #[test]
    fn global_ore_selection_keeps_the_global_step_index() {
        let mut first_steps = vec![Value::Array(Vec::new()); 7];
        first_steps[STEP_UNDERGROUND_ORES as usize] = serde_json::json!([
            "minecraft:non_ore_a",
            "minecraft:test_ore"
        ]);
        let mut second_steps = vec![Value::Array(Vec::new()); 7];
        second_steps[STEP_UNDERGROUND_ORES as usize] = serde_json::json!([
            "minecraft:non_ore_b",
            "minecraft:test_ore"
        ]);
        let mut biomes = HashMap::new();
        biomes.insert("minecraft:first", serde_json::json!({"features": first_steps}));
        biomes.insert("minecraft:second", serde_json::json!({"features": second_steps}));

        let mut placed = HashMap::new();
        for id in ["minecraft:non_ore_a", "minecraft:non_ore_b"] {
            placed.insert(id, serde_json::json!({"feature": "minecraft:plain", "placement": []}));
        }
        placed.insert(
            "minecraft:test_ore",
            serde_json::json!({"feature": "minecraft:test_ore_config", "placement": []}),
        );
        let mut features = HashMap::new();
        features.insert("minecraft:plain", serde_json::json!({"type": "minecraft:lake", "config": {}}));
        features.insert(
            "minecraft:test_ore_config",
            serde_json::json!({
                "type": "minecraft:ore",
                "config": {"size": 1, "discard_chance_on_air_exposure": 0.0, "targets": []}
            }),
        );
        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes,
            carvers: HashMap::new(),
            features,
            placed,
        };

        let catalog = build_decoration_catalog(
            &resolver,
            &["minecraft:first".to_string(), "minecraft:second".to_string()],
        );
        let definitions = build_ore_definitions(&resolver, &catalog);
        let ores = catalog.select_ores(["minecraft:first"], &definitions);
        let combined = catalog.select_all(["minecraft:first"], &definitions);
        assert_eq!(ores.len(), 1);
        assert_eq!(
            ores[0].index, 2,
            "the second biome's preceding feature occupies global index 1 even when it is not eligible"
        );
        assert_eq!(combined.ores.len(), ores.len());
        assert_eq!(combined.ores[0].index, ores[0].index);
    }

    #[test]
    fn compiled_selection_handles_multiple_membership_words() {
        let ids = (0..65)
            .map(|index| Box::leak(format!("minecraft:feature_{index}").into_boxed_str()) as &'static str)
            .collect::<Vec<_>>();
        let entries = ids
            .iter()
            .map(|id| Value::String((*id).to_owned()))
            .collect::<Vec<_>>();
        let mut biomes = HashMap::new();
        biomes.insert(
            "minecraft:many",
            serde_json::json!({"features": [entries]}),
        );
        let mut placed = HashMap::new();
        for id in &ids {
            placed.insert(
                *id,
                serde_json::json!({"feature": "minecraft:lake", "placement": []}),
            );
        }
        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes,
            carvers: HashMap::new(),
            features: HashMap::from([(
                "minecraft:lake",
                serde_json::json!({"type": "minecraft:lake", "config": {}}),
            )]),
            placed,
        };
        let catalog = build_decoration_catalog(
            &resolver,
            &["minecraft:many".to_string()],
        );
        let selected = catalog.select(["minecraft:many"]);
        assert_eq!(selected.len(), ids.len());
        assert_eq!(selected.first().map(|entry| entry.1), Some(0));
        assert_eq!(selected.last().map(|entry| entry.1), Some(64));
    }

    #[test]
    fn step6_disk_selection_keeps_global_index_and_excludes_ores() {
        let mut first_steps = vec![Value::Array(Vec::new()); 7];
        first_steps[0] = serde_json::json!(["minecraft:non_disk_a"]);
        first_steps[STEP_UNDERGROUND_ORES as usize] = serde_json::json!([
            "minecraft:non_disk_a",
            "minecraft:underwater_magma",
            "minecraft:disk_gravel"
        ]);
        let mut second_steps = vec![Value::Array(Vec::new()); 7];
        second_steps[STEP_UNDERGROUND_ORES as usize] = serde_json::json!([
            "minecraft:non_disk_b",
            "minecraft:disk_gravel"
        ]);
        let mut biomes = HashMap::new();
        biomes.insert("minecraft:first", serde_json::json!({"features": first_steps}));
        biomes.insert("minecraft:second", serde_json::json!({"features": second_steps}));

        let mut placed = HashMap::new();
        for id in ["minecraft:non_disk_a", "minecraft:non_disk_b"] {
            placed.insert(id, serde_json::json!({"feature": "minecraft:plain", "placement": []}));
        }
        placed.insert(
            "minecraft:disk_gravel",
            serde_json::json!({"feature": "minecraft:disk_gravel_cf", "placement": []}),
        );
        placed.insert(
            "minecraft:underwater_magma",
            serde_json::json!({"feature": "minecraft:underwater_magma_cf", "placement": []}),
        );
        let mut features = HashMap::new();
        features.insert("minecraft:plain", serde_json::json!({"type": "minecraft:lake", "config": {}}));
        features.insert(
            "minecraft:underwater_magma_cf",
            serde_json::json!({
                "type": "minecraft:underwater_magma",
                "config": {
                    "floor_search_range": 5,
                    "placement_probability_per_valid_position": 0.5,
                    "placement_radius_around_floor": 1
                }
            }),
        );
        features.insert(
            "minecraft:disk_gravel_cf",
            serde_json::json!({
                "type": "minecraft:disk",
                "config": {
                    "half_height": 2,
                    "radius": {"type": "minecraft:uniform", "min_inclusive": 2, "max_inclusive": 5},
                    "state_provider": {
                        "type": "minecraft:simple_state_provider",
                        "state": {"Name": "minecraft:gravel"}
                    },
                    "target": {"type": "minecraft:matching_blocks", "blocks": ["minecraft:dirt"]}
                }
            }),
        );
        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes,
            carvers: HashMap::new(),
            features,
            placed,
        };

        let catalog = build_decoration_catalog(
            &resolver,
            &["minecraft:first".to_string(), "minecraft:second".to_string()],
        );
        let definitions = build_ore_definitions(&resolver, &catalog);
        let combined = catalog.select_all(["minecraft:first"], &definitions);
        let ordinary = catalog.select(["minecraft:first"]);
        let disks = catalog.select_step6_disks(["minecraft:first"]);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].0, STEP_UNDERGROUND_ORES);
        assert_eq!(disks[0].1, 3, "step-6 index includes all preceding global entries");
        assert!(matches!(
            disks[0].2.feature.as_ref(),
            crate::feature::vegetation::ConfiguredFeature::Disk(_)
        ));
        let non_ore = catalog.select_step6_non_ore(["minecraft:first"]);
        assert_eq!(non_ore.len(), 1);
        assert_eq!(non_ore[0].0, STEP_UNDERGROUND_ORES);
        assert_eq!(non_ore[0].1, 2, "step-6 non-ore keeps its raw position before the disk");
        assert!(matches!(
            non_ore[0].2.feature.as_ref(),
            crate::feature::vegetation::ConfiguredFeature::UnderwaterMagma(_)
        ));
        assert!(ordinary
            .iter()
            .all(|(step, _, _)| *step != STEP_UNDERGROUND_ORES));
        assert_eq!(combined.features.len(), ordinary.len());
        assert_eq!(combined.features[0].0, ordinary[0].0);
        assert_eq!(combined.features[0].1, ordinary[0].1);
        let repeated = catalog.select_all(["minecraft:first"], &definitions);
        assert!(Arc::ptr_eq(&combined.features[0].2, &repeated.features[0].2));
        assert_ne!(
            Arc::as_ptr(&combined.features[0].2),
            &ordinary[0].2 as *const crate::feature::vegetation::PlacedRef
        );
        assert_eq!(
            combined.features[0].2.registry_id.as_deref(),
            ordinary[0].2.registry_id.as_deref()
        );
        assert_eq!(combined.step6_disks.len(), disks.len());
        assert_eq!(combined.step6_disks[0].0, disks[0].0);
        assert_eq!(combined.step6_disks[0].1, disks[0].1);
        assert_eq!(combined.step6_non_ore.len(), non_ore.len());
        assert_eq!(combined.step6_non_ore[0].0, non_ore[0].0);
        assert_eq!(combined.step6_non_ore[0].1, non_ore[0].1);
        assert!(combined.ores.is_empty());
    }

    #[test]
    fn local_modification_geode_keeps_its_raw_step_index() {
        let mut steps = vec![Value::Array(Vec::new()); 3];
        steps[2] = serde_json::json!([
            "minecraft:preceding_a",
            "minecraft:preceding_b",
            "minecraft:amethyst_geode"
        ]);
        let mut biomes = HashMap::new();
        biomes.insert("minecraft:test", serde_json::json!({"features": steps}));

        let mut placed = HashMap::new();
        placed.insert(
            "minecraft:preceding_a",
            serde_json::json!({"feature": "minecraft:no_op", "placement": []}),
        );
        placed.insert(
            "minecraft:preceding_b",
            serde_json::json!({"feature": "minecraft:no_op", "placement": []}),
        );
        placed.insert(
            "minecraft:amethyst_geode",
            serde_json::json!({"feature": "minecraft:geode", "placement": []}),
        );

        let mut features = HashMap::new();
        features.insert("minecraft:no_op", serde_json::json!({"type": "minecraft:no_op"}));
        features.insert(
            "minecraft:geode",
            serde_json::json!({
                "type": "minecraft:geode",
                "config": {
                    "blocks": {
                        "filling_provider": {"type": "minecraft:simple_state_provider", "state": {"Name": "minecraft:air"}},
                        "inner_layer_provider": {"type": "minecraft:simple_state_provider", "state": {"Name": "minecraft:amethyst_block"}},
                        "alternate_inner_layer_provider": {"type": "minecraft:simple_state_provider", "state": {"Name": "minecraft:budding_amethyst"}},
                        "middle_layer_provider": {"type": "minecraft:simple_state_provider", "state": {"Name": "minecraft:calcite"}},
                        "outer_layer_provider": {"type": "minecraft:simple_state_provider", "state": {"Name": "minecraft:smooth_basalt"}},
                        "inner_placements": [{"Name": "minecraft:amethyst_cluster", "Properties": {"facing": "up", "waterlogged": "false"}}],
                        "cannot_replace": "#minecraft:features_cannot_replace",
                        "invalid_blocks": "#minecraft:geode_invalid_blocks"
                    },
                    "crack": {},
                    "layers": {},
                    "invalid_blocks_threshold": 1
                }
            }),
        );
        let mut tags = HashMap::new();
        tags.insert("minecraft:features_cannot_replace", serde_json::json!({"values": []}));
        tags.insert("minecraft:geode_invalid_blocks", serde_json::json!({"values": []}));
        let resolver = FakeResolver {
            tags,
            biomes,
            carvers: HashMap::new(),
            features,
            placed,
        };

        let catalog = build_decoration_catalog(&resolver, &["minecraft:test".to_string()]);
        let selected = catalog.select(["minecraft:test"]);
        let (_, index, placed) = selected
            .into_iter()
            .find(|(step, _, placed)| {
                *step == 2
                    && placed.registry_id.as_deref() == Some("minecraft:amethyst_geode")
            })
            .expect("local-modification geode must remain selected");
        assert_eq!(index, 2);
        assert!(matches!(
            placed.feature.as_ref(),
            crate::feature::vegetation::ConfiguredFeature::Geode(_)
        ));
    }

}
