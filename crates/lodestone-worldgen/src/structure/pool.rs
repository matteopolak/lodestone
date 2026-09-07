//! Jigsaw **template pools** and processor lists — the data half of S4.
//!
//! # What it is
//!
//! A port of vanilla's own template-pool + pool-element +
//! processor-list types: the 188 `worldgen/template_pool/*.json` and 40
//! `worldgen/processor_list/*.json` documents decoded into [`TemplatePool`]s of
//! [`PoolElement`]s that [`super::jigsaw`] assembles. This module answers
//! "what can attach here, and how does it place itself"; `jigsaw` answers
//! "which one, where, and in what order".
//!
//! # How it works
//!
//! ```text
//! PoolStore::load(start_pool)                  <- transitive closure
//!     parse the pool document
//!     for each element: parse its processor list, load its template
//!     for each jigsaw block in each template: recurse into `pool`
//!     recurse into `fallback`
//! ```
//!
//! Two vanilla shapes are load-bearing and easy to miss:
//!
//! * **The element list is weight-expanded.** Vanilla's own template-pool
//!   constructor pushes each element `weight` times into `templates`, and *that*
//!   list is what vanilla's own random-template lookup indexes and its own
//!   shuffled-templates accessor shuffles.
//!   `village/plains/town_centers` has weights `50,50,50,50,1,1,1,1`, so its
//!   expanded list is 204 entries and one shuffle of it consumes **203** draws.
//!   Shuffling the 8 raw entries instead would consume 7 and desynchronise every
//!   subsequent draw in the structure.
//! * **A `terrain_matching` projection carries a processor**:
//!   vanilla's own terrain-matching projection's own list is
//!   `[gravity processor(WORLD_SURFACE_WG, -1)]`, appended *after* the element's
//!   own processors. That single entry is what makes a village street follow a
//!   hillside, and a projection treated as a mere flag places every street flat.
//!
//! # How to change it
//!
//! * **An unsupported `processor_type`, `predicate_type` or `element_type` makes
//!   the whole pool unusable and demotes its structure** ([`PoolStore::load`]
//!   returns the reason, [`super::StructureRegistry::unsupported`] records it).
//!   That is deliberate and not defensive: a rule whose `position_predicate` we
//!   silently treat as `always_true` still *consumes no draw*, so every later rule
//!   in that list rolls a different number and the structure is quietly wrong
//!   rather than loudly absent.
//! * Vanilla's own max-size accessor is only read by the expansion hack, and it is cached per pool
//!   exactly as vanilla caches it, because it walks every element's box.
//!
//! # Dependencies
//!
//! [`Resolver::template_pool`], [`Resolver::processor_list`],
//! [`Resolver::block_tag`] and [`Resolver::structure_template`] (through
//! [`super::TemplateStore`]).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;

use super::jigsaw::JigsawBlockInfo;
use super::processor::{ColumnHeights, ProcessorRule, RuleTest};
use super::template::{BlockState, Rotation, StructureTemplate};
use super::{BoundingBox, TemplateStore};
use crate::density::Resolver;
use crate::feature::BlockPos;
use crate::feature::vegetation::{PlacedRef, VegGrid, VegTags};
use lodestone_worldgen_core::rng::RandomSource;

/// `StructureTemplatePool.Projection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    /// `rigid` — the element sits where the joint puts it, and no processor is
    /// appended.
    Rigid,
    /// `terrain_matching` — the element's Y comes from the terrain, and
    /// `GravityProcessor(WORLD_SURFACE_WG, -1)` is appended to its chain.
    TerrainMatching,
}

impl Projection {
    fn parse(value: &Value) -> Option<Self> {
        match value.as_str() {
            Some("rigid") => Some(Self::Rigid),
            Some("terrain_matching") => Some(Self::TerrainMatching),
            _ => None,
        }
    }

    /// True for `RIGID` — the flag `JigsawPlacement` branches on in five places.
    #[must_use]
    pub fn is_rigid(self) -> bool {
        self == Self::Rigid
    }
}

/// One element of a pool — vanilla's `StructurePoolElement` hierarchy.
#[derive(Debug, Clone)]
pub enum PoolElement {
    /// `single_pool_element` / `legacy_single_pool_element`. The two differ only
    /// in their `BlockIgnoreProcessor`: legacy also drops the template's **air**
    /// (`STRUCTURE_AND_AIR` instead of `STRUCTURE_BLOCK`), which is why a legacy
    /// village house does not clear the ground it stands on to an air box.
    Single {
        /// The template id.
        template: String,
        /// The decoded template.
        decoded: Arc<StructureTemplate>,
        /// True for `legacy_single_pool_element`.
        legacy: bool,
        /// The element's own processors, in `addProcessor` order — **without** the
        /// leading ignore/jigsaw-replacement pair and without the projection's,
        /// both of which are appended at placement time.
        processors: Arc<Vec<super::processor::Processor>>,
        /// `projection`.
        projection: Projection,
        /// `override_liquid_settings`, as "apply waterlogging".
        override_waterlogging: Option<bool>,
    },
    /// `list_pool_element` — every sub-element placed at the same position, and
    /// **only the first** consulted for jigsaw blocks.
    List {
        /// The sub-elements, in document order.
        elements: Vec<PoolElement>,
        /// `projection`, which is also forced onto every sub-element.
        projection: Projection,
    },
    /// `feature_pool_element` — places a `placed_feature`, not a template.
    ///
    /// Represented, not skipped: it has a **size of zero, a single-block box and
    /// exactly one synthetic jigsaw block** facing down, so it participates in the
    /// free-space accumulator and in the joint graph. Dropping it would change
    /// which elements the pool's shuffle offers and therefore the whole village.
    /// Its resolved placed-feature body is retained so the placement stage can
    /// run the same feature driver as ordinary vegetal decoration, at the
    /// structure-supplied origin. The placement stage owns clipping and the
    /// random stream; this element owns only the data and origin hand-off.
    Feature {
        /// The `placed_feature` id, for the ledger.
        feature: String,
        /// The resolved placed-feature document. `None` is retained for
        /// programmatically-built elements; parsed data always supplies it,
        /// including an `Unsupported` body for an unmodelled feature type.
        placed: Option<Arc<PlacedRef>>,
        /// `projection`.
        projection: Projection,
    },
    /// `empty_pool_element` — terminates a branch. `getShuffledTemplates`
    /// **breaks** on it rather than skipping it, so its position in a pool
    /// matters.
    Empty,
}

/// A resolved `feature_pool_element` ready for placement.
///
/// Unlike biome decoration, a pool feature is not anchored to a chunk origin:
/// its origin is the assembled piece's world position. The caller must invoke
/// [`Self::place`] with the structure placement stream and the real placement
/// grid, preserving modifier draws and clipping alongside the other piece
/// placements.
#[derive(Debug, Clone)]
pub struct PoolFeaturePlacement {
    /// Registry id used by the feature's biome modifier, when the source was a
    /// registry holder rather than an inline value.
    pub feature: String,
    /// The parsed placed-feature pipeline and configured body.
    pub placed: Arc<PlacedRef>,
    /// The assembled world-space origin of the feature.
    pub origin: BlockPos,
    /// The pool element projection, retained for placement-stage diagnostics.
    pub projection: Projection,
}

impl PoolFeaturePlacement {
    /// Place the feature at its structure-supplied origin using the caller's
    /// stream. This is deliberately not reseeded: structure placement has one
    /// stream, and the feature's modifiers consume it in declaration order.
    pub fn place<R: RandomSource>(
        &self,
        random: &mut R,
        grid: &mut VegGrid,
        tags: &VegTags,
    ) {
        // The ordinary decoration driver binds once per pass. A structure can
        // contain several feature elements and calls this bridge directly, so
        // bind idempotently here instead of making every placement-stage caller
        // remember a vegetation-internal cache invariant.
        tags.bind(grid.interner());
        crate::feature::vegetation::place_placed_feature_at(
            random,
            self.origin,
            &self.placed,
            grid,
            tags,
        );
    }
}

impl PoolElement {
    /// `getProjection()`.
    #[must_use]
    pub fn projection(&self) -> Projection {
        match self {
            Self::Single { projection, .. }
            | Self::List { projection, .. }
            | Self::Feature { projection, .. } => *projection,
            // `EmptyPoolElement`'s constructor passes `TERRAIN_MATCHING`.
            Self::Empty => Projection::TerrainMatching,
        }
    }

    /// `getGroundLevelDelta()`.
    ///
    /// **Constant 1 in 26.2**, for every element type — `StructurePoolElement`
    /// declares it and nothing overrides it. Older versions read a `bottom` data
    /// marker out of the template, and the S3 handoff note expected that; the
    /// record definition says otherwise, so the marker is not needed here at all.
    #[must_use]
    pub fn ground_level_delta(&self) -> i32 {
        1
    }

    /// Returns the resolved feature placement for this element at `origin`.
    ///
    /// `None` means this is not a feature element, or it was constructed by a
    /// caller without a resolved feature body. Parsed pool data never takes
    /// the latter path: an unsupported body is represented by the feature
    /// driver's `ConfiguredFeature::Unsupported` variant and remains an
    /// observable, intentional no-op rather than disappearing from the pool.
    #[must_use]
    pub fn feature_placement(&self, origin: [i32; 3]) -> Option<PoolFeaturePlacement> {
        match self {
            Self::Feature {
                feature,
                placed: Some(placed),
                projection,
            } => Some(PoolFeaturePlacement {
                feature: feature.clone(),
                placed: Arc::clone(placed),
                origin: BlockPos {
                    x: origin[0],
                    y: origin[1],
                    z: origin[2],
                },
                projection: *projection,
            }),
            _ => None,
        }
    }

    /// `getBoundingBox(manager, position, rotation)`.
    ///
    /// `None` for [`Self::Empty`], where vanilla throws — every call site filters
    /// it first, and returning `None` makes that filter checkable instead of a
    /// panic in chunk generation.
    #[must_use]
    pub fn bounding_box(&self, position: [i32; 3], rotation: Rotation) -> Option<BoundingBox> {
        match self {
            Self::Single { decoded, .. } => {
                let settings = super::template::PlaceSettings {
                    rotation,
                    ..Default::default()
                };
                Some(decoded.bounding_box(position, &settings))
            }
            Self::List { elements, .. } => elements
                .iter()
                .filter(|e| !matches!(e, Self::Empty))
                .filter_map(|e| e.bounding_box(position, rotation))
                .reduce(BoundingBox::encapsulate),
            // `size` is `Vec3i.ZERO`, so `new BoundingBox(pos, pos + 0)`.
            Self::Feature { .. } => Some(BoundingBox {
                min: position,
                max: position,
            }),
            Self::Empty => None,
        }
    }

    /// Vanilla's own shuffled-jigsaw-blocks accessor at `(manager, position,
    /// rotation, random)` — the
    /// template's jigsaw blocks, shuffled via vanilla's own shuffle then **stably** sorted by
    /// descending `selection_priority`.
    ///
    /// Both steps are the specification. The shuffle's draw count is
    /// `max(0, n - 1)`, so an element with one jigsaw block costs no draw and an
    /// element with none costs no draw either.
    pub fn shuffled_jigsaw_blocks<R: RandomSource>(
        &self,
        position: [i32; 3],
        rotation: Rotation,
        random: &mut R,
    ) -> Vec<JigsawBlockInfo> {
        let mut blocks = match self {
            Self::Single { decoded, .. } => decoded
                .filter_blocks("minecraft:jigsaw", position, rotation)
                .into_iter()
                .map(JigsawBlockInfo::of)
                .collect(),
            // `ListPoolElement` delegates to `elements.get(0)` only.
            Self::List { elements, .. } => elements
                .first()
                .map(|e| e.shuffled_jigsaw_blocks(position, rotation, random))
                .unwrap_or_default(),
            Self::Feature { .. } => vec![JigsawBlockInfo::feature_default(position)],
            Self::Empty => Vec::new(),
        };
        if matches!(self, Self::List { .. }) {
            // The delegate already shuffled and sorted; doing it twice would draw
            // twice.
            return blocks;
        }
        shuffle(&mut blocks, random);
        // Vanilla's own descending-priority comparator through a stable sort —
        // so equal priorities keep the shuffled
        // order, and an unstable sort here would be a silent divergence.
        blocks.sort_by_key(|b| -b.selection_priority);
        blocks
    }

    /// Every pool this element's jigsaw blocks name — the edges
    /// [`PoolStore::load`] follows.
    fn referenced_pools(&self, out: &mut BTreeSet<String>) {
        match self {
            Self::Single { decoded, .. } => {
                for block in decoded.filter_blocks("minecraft:jigsaw", [0, 0, 0], Rotation::None) {
                    if let Some(nbt) = &block.nbt {
                        if let Some(pool) = super::template::nbt_string(nbt, "pool") {
                            out.insert(pool.to_string());
                        }
                    }
                }
            }
            Self::List { elements, .. } => {
                for element in elements {
                    element.referenced_pools(out);
                }
            }
            Self::Feature { .. } | Self::Empty => {}
        }
    }
}

/// Vanilla's own list-shuffle at `(list, random)` — Fisher–Yates walked **downward** from the end.
///
/// The direction and the bound are both the specification: vanilla's own loop
/// counts down from `size` while above 1, drawing a bounded swap target below
/// the current count on each step. An upward Fisher–Yates draws the same
/// *number* of values in a different order and produces a different permutation.
pub fn shuffle<T, R: RandomSource>(list: &mut [T], random: &mut R) {
    let size = i32::try_from(list.len()).unwrap_or(0);
    let mut i = size;
    while i > 1 {
        let swap_to = random.next_int_bounded(i);
        list.swap((i - 1) as usize, swap_to.clamp(0, i - 1) as usize);
        i -= 1;
    }
}

/// One decoded `template_pool` document.
#[derive(Debug, Clone)]
pub struct TemplatePool {
    /// `fallback`, as a pool id.
    pub fallback: String,
    /// The **weight-expanded** element list — what `getRandomTemplate` indexes
    /// and `getShuffledTemplates` shuffles. See the module doc.
    pub expanded: Vec<Arc<PoolElement>>,
    /// `getMaxSize`, computed on first use exactly as vanilla caches it.
    max_size: std::sync::OnceLock<i32>,
}

impl TemplatePool {
    /// `size()`.
    #[must_use]
    pub fn size(&self) -> usize {
        self.expanded.len()
    }

    /// `getRandomTemplate(random)` — one `nextInt(size)`, or
    /// `EmptyPoolElement.INSTANCE` and **no draw** for an empty pool.
    pub fn random_template<R: RandomSource>(&self, random: &mut R) -> Arc<PoolElement> {
        if self.expanded.is_empty() {
            return Arc::new(PoolElement::Empty);
        }
        let index = random.next_int_bounded(i32::try_from(self.expanded.len()).unwrap_or(1));
        let index = usize::try_from(index).unwrap_or(0).min(self.expanded.len() - 1);
        Arc::clone(&self.expanded[index])
    }

    /// Vanilla's own shuffled-templates accessor — vanilla's own shuffled-copy of the expanded list.
    pub fn shuffled_templates<R: RandomSource>(&self, random: &mut R) -> Vec<Arc<PoolElement>> {
        let mut copy: Vec<Arc<PoolElement>> = self.expanded.clone();
        shuffle(&mut copy, random);
        copy
    }

    /// Vanilla's own max-size accessor — the tallest non-empty element's Y span at
    /// the zero position with no rotation. Only the expansion hack reads it.
    #[must_use]
    pub fn max_size(&self) -> i32 {
        *self.max_size.get_or_init(|| {
            self.expanded
                .iter()
                .filter(|e| !matches!(***e, PoolElement::Empty))
                .filter_map(|e| e.bounding_box([0, 0, 0], Rotation::None))
                .map(|b| b.max[1] - b.min[1] + 1)
                .max()
                .unwrap_or(0)
        })
    }
}

/// Every pool one jigsaw structure can reach, keyed by pool id.
///
/// Loaded eagerly, once per generator, for the same reason
/// [`TemplateStore`] is: a start predicate runs inside the chunk pipeline where
/// there is no `&dyn Resolver` to reach.
#[derive(Debug, Default)]
pub struct PoolStore {
    pools: HashMap<String, Arc<TemplatePool>>,
    processor_lists: HashMap<String, Arc<Vec<super::processor::Processor>>>,
    block_tags: HashMap<String, Arc<HashSet<String>>>,
}

/// The alias names and targets of one jigsaw structure, as
/// [`PoolStore::load`] needs them.
///
/// Two sets rather than one map because the walk asks two different questions:
/// *"is this missing pool an alias, and therefore fine?"* (`names`) and *"what else
/// must be loaded even though nothing references it?"* (`targets`). One map keyed
/// by alias could answer neither cleanly, since a `random_group` binding has many
/// possible targets per alias.
#[derive(Debug, Default, Clone)]
pub struct AliasedPools {
    /// Every id that appears as an `alias`.
    pub names: BTreeSet<String>,
    /// Every id any binding can resolve **to**.
    pub targets: BTreeSet<String>,
}

impl PoolStore {
    /// One loaded pool, or `None` when it was not bundled.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Arc<TemplatePool>> {
        self.pools.get(id)
    }

    /// How many pools are loaded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pools.len()
    }

    /// True when nothing is loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pools.is_empty()
    }

    /// Loads `start` and everything transitively reachable from it, returning
    /// `Err` with the first blocking reason.
    ///
    /// Transitive through **both** edges a pool has: the `pool` field of every
    /// jigsaw block in every element's template, and the pool's own `fallback`.
    /// Missing either one produces a structure that assembles its first ring and
    /// then stops, which looks like a working village of one building.
    pub fn load(
        &mut self,
        resolver: &dyn Resolver,
        templates: &mut TemplateStore,
        start: &str,
        aliases: &AliasedPools,
    ) -> Result<(), String> {
        let mut queue = vec![start.to_string()];
        // Every alias *target* is a root of its own: only one is chosen per
        // structure instance, at start time, so all of them have to be loaded now.
        queue.extend(aliases.targets.iter().cloned());
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some(id) = queue.pop() {
            if !seen.insert(id.clone()) || self.pools.contains_key(&id) {
                continue;
            }
            let document = resolver.template_pool(&id);
            if document.is_null() {
                // An **alias** is not expected to be a pool at all
                // (`trial_chambers/spawner/contents/melee` ships no document), so a
                // missing document for a declared alias is data being correct, not
                // data being absent. Every other missing pool still refuses the
                // structure.
                if aliases.names.contains(&id) {
                    continue;
                }
                return Err(format!("template pool '{id}' not bundled"));
            }
            let fallback = document["fallback"]
                .as_str()
                .unwrap_or("minecraft:empty")
                .to_string();
            let mut expanded: Vec<Arc<PoolElement>> = Vec::new();
            let entries = document["elements"].as_array().cloned().unwrap_or_default();
            for entry in &entries {
                let element = self.parse_element(resolver, templates, &entry["element"])?;
                let weight = entry["weight"].as_i64().unwrap_or(1).max(0);
                let element = Arc::new(element);
                for _ in 0..weight {
                    expanded.push(Arc::clone(&element));
                }
                let mut referenced = BTreeSet::new();
                element.referenced_pools(&mut referenced);
                queue.extend(referenced);
            }
            queue.push(fallback.clone());
            self.pools.insert(
                id,
                Arc::new(TemplatePool {
                    fallback,
                    expanded,
                    max_size: std::sync::OnceLock::new(),
                }),
            );
        }
        Ok(())
    }

    fn parse_element(
        &mut self,
        resolver: &dyn Resolver,
        templates: &mut TemplateStore,
        value: &Value,
    ) -> Result<PoolElement, String> {
        let element_type = value["element_type"].as_str().unwrap_or_default();
        match element_type {
            "minecraft:empty_pool_element" => Ok(PoolElement::Empty),
            "minecraft:feature_pool_element" => {
                let projection = Projection::parse(&value["projection"])
                    .ok_or_else(|| format!("element projection '{}'", value["projection"]))?;
                let placed = value
                    .get("feature")
                    .filter(|feature| !feature.is_null())
                    .map(|feature| {
                        Arc::new(crate::feature::vegetation::resolve_placed_feature_ref(
                            resolver, feature,
                        ))
                    });
                Ok(PoolElement::Feature {
                    feature: value["feature"].as_str().unwrap_or("<inline>").to_string(),
                    placed,
                    projection,
                })
            }
            "minecraft:list_pool_element" => {
                let projection = Projection::parse(&value["projection"])
                    .ok_or_else(|| format!("element projection '{}'", value["projection"]))?;
                let mut elements = Vec::new();
                for sub in value["elements"].as_array().cloned().unwrap_or_default() {
                    elements.push(self.parse_element(resolver, templates, &sub)?);
                }
                if elements.is_empty() {
                    return Err("list_pool_element with no elements".to_string());
                }
                // `setProjectionOnEachElement` — the list's projection wins.
                for element in &mut elements {
                    match element {
                        PoolElement::Single { projection: p, .. }
                        | PoolElement::List { projection: p, .. }
                        | PoolElement::Feature { projection: p, .. } => *p = projection,
                        PoolElement::Empty => {}
                    }
                }
                Ok(PoolElement::List {
                    elements,
                    projection,
                })
            }
            "minecraft:single_pool_element" | "minecraft:legacy_single_pool_element" => {
                let legacy = element_type == "minecraft:legacy_single_pool_element";
                let projection = Projection::parse(&value["projection"])
                    .ok_or_else(|| format!("element projection '{}'", value["projection"]))?;
                let template = value["location"]
                    .as_str()
                    .ok_or("single pool element with no `location`")?
                    .to_string();
                // Two failure modes with two different answers, and collapsing
                // them loses one of the two things this can tell you:
                //
                // * **the resolver serves no templates at all** — an unwired
                //   resolver, the S2 island. Hard `Err`, so the structure is
                //   demoted and the ledger says so loudly. This is what
                //   `lodestone-server`'s own `no_structure_is_demoted_for_unloadable_templates`
                //   gate detects, and it must keep detecting it.
                // * **this one template is missing from an otherwise complete
                //   bundle** — the data's deliberate dangling reference. Substitute
                //   an empty template, preserving its shuffle position and pool
                //   graph. Refusing here would delete `ancient_city` over one wall
                //   variant the data deliberately leaves empty.
                let failures = templates.load(resolver, &[template.as_str()]);
                let decoded = match templates.get(&template) {
                    Some(loaded) => Arc::clone(loaded),
                    None => {
                        if templates.is_empty() {
                            let why = failures
                                .first()
                                .map_or("template not bundled", |(_, why)| why.as_str());
                            return Err(format!("template '{template}' unusable ({why})"));
                        }
                        Arc::new(StructureTemplate::empty())
                    }
                };
                let processors = self.processors(resolver, &value["processors"])?;
                let override_waterlogging = match value["override_liquid_settings"].as_str() {
                    Some("apply_waterlogging") => Some(true),
                    Some("ignore_waterlogging") => Some(false),
                    _ => None,
                };
                Ok(PoolElement::Single {
                    template,
                    decoded,
                    legacy,
                    processors,
                    projection,
                    override_waterlogging,
                })
            }
            other => Err(format!("pool element_type '{other}'")),
        }
    }

    /// An element's `processors` field: either a reference to one of the 40
    /// `processor_list` documents, or an inline `{"processors": [...]}`.
    fn processors(
        &mut self,
        resolver: &dyn Resolver,
        value: &Value,
    ) -> Result<Arc<Vec<super::processor::Processor>>, String> {
        if let Some(id) = value.as_str() {
            if let Some(existing) = self.processor_lists.get(id) {
                return Ok(Arc::clone(existing));
            }
            let document = resolver.processor_list(id);
            if document.is_null() {
                return Err(format!("processor list '{id}' not bundled"));
            }
            let parsed = Arc::new(self.parse_processors(resolver, &document)?);
            self.processor_lists.insert(id.to_string(), Arc::clone(&parsed));
            return Ok(parsed);
        }
        Ok(Arc::new(self.parse_processors(resolver, value)?))
    }

    fn parse_processors(
        &mut self,
        resolver: &dyn Resolver,
        document: &Value,
    ) -> Result<Vec<super::processor::Processor>, String> {
        let mut out = Vec::new();
        for entry in document["processors"].as_array().cloned().unwrap_or_default() {
            out.push(self.parse_processor(resolver, &entry)?);
        }
        Ok(out)
    }

    fn parse_processor(
        &mut self,
        resolver: &dyn Resolver,
        value: &Value,
    ) -> Result<super::processor::Processor, String> {
        use super::processor::Processor;
        match value["processor_type"].as_str().unwrap_or_default() {
            "minecraft:nop" => Ok(Processor::BlockIgnore(Vec::new())),
            "minecraft:block_ignore" => Ok(Processor::BlockIgnore(
                value["blocks"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|b| b["Name"].as_str().or_else(|| b.as_str()))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            )),
            "minecraft:block_rot" => Ok(Processor::BlockRot {
                rottable: match value.get("rottable_blocks") {
                    None | Some(Value::Null) => None,
                    Some(set) => Some(self.block_set(resolver, set)),
                },
                integrity: value["integrity"].as_f64().unwrap_or(1.0) as f32,
            }),
            "minecraft:protected_blocks" => {
                Ok(Processor::ProtectedBlocks(self.block_set(resolver, &value["value"])))
            }
            "minecraft:rule" => {
                let mut rules = Vec::new();
                for rule in value["rules"].as_array().cloned().unwrap_or_default() {
                    // `append_loot` chooses the *state* and additionally writes a
                    // loot table into the block entity. The state half is honoured;
                    // the loot half is ledgered under `block_entity:append_loot`,
                    // because a worldgen chunk here has no block entities at all.
                    // Any other modifier type is still refused — its effect is not
                    // confined to something we can name.
                    match rule["block_entity_modifier"]["type"].as_str() {
                        None | Some("minecraft:passthrough") | Some("minecraft:clear")
                        | Some("minecraft:append_loot") => {}
                        Some(other) => {
                            return Err(format!("rule block_entity_modifier '{other}'"));
                        }
                    }
                    rules.push(ProcessorRule {
                        input: self.rule_test(resolver, &rule["input_predicate"])?,
                        location: self.rule_test(resolver, &rule["location_predicate"])?,
                        position: parse_pos_test(&rule["position_predicate"])?,
                        output: parse_state(&rule["output_state"]),
                    });
                }
                Ok(Processor::Rule(rules))
            }
            "minecraft:capped" => {
                let delegate = self.parse_processor(resolver, &value["delegate"])?;
                // `IntProviders.POSITIVE_CODEC` accepts a bare int (`ConstantInt`,
                // no draw) or a tagged provider (which *does* draw, before the
                // shuffle). Every bundled `capped` uses the bare form; anything
                // else is refused rather than approximated, because a draw here
                // moves the entire shuffled index walk.
                let limit = match &value["limit"] {
                    Value::Number(n) => n.as_i64().unwrap_or(0) as i32,
                    other => {
                        return Err(format!("capped `limit` provider {other}"));
                    }
                };
                Ok(Processor::Capped {
                    delegate: Box::new(delegate),
                    limit,
                })
            }
            other => Err(format!("processor_type '{other}'")),
        }
    }

    fn rule_test(&mut self, resolver: &dyn Resolver, value: &Value) -> Result<RuleTest, String> {
        match value["predicate_type"].as_str().unwrap_or("minecraft:always_true") {
            "minecraft:always_true" => Ok(RuleTest::AlwaysTrue),
            "minecraft:block_match" => Ok(RuleTest::BlockMatch(
                value["block"].as_str().unwrap_or_default().to_string(),
            )),
            "minecraft:blockstate_match" => Ok(RuleTest::BlockStateMatch(
                parse_state(&value["block_state"]).canonical(),
            )),
            "minecraft:random_block_match" => Ok(RuleTest::RandomBlockMatch(
                value["block"].as_str().unwrap_or_default().to_string(),
                value["probability"].as_f64().unwrap_or(1.0) as f32,
            )),
            "minecraft:random_blockstate_match" => Ok(RuleTest::RandomBlockStateMatch(
                parse_state(&value["block_state"]).canonical(),
                value["probability"].as_f64().unwrap_or(1.0) as f32,
            )),
            // `tag_match`'s `tag` field is a bare tag *name* (`"minecraft:doors"`),
            // where a `HolderSet<Block>` field spells the same thing `"#minecraft:doors"`.
            // Two spellings of one concept, and reading the bare one as a block id
            // yields an empty set that matches nothing.
            "minecraft:tag_match" => Ok(RuleTest::TagMatch(self.block_set(
                resolver,
                &Value::String(format!("#{}", value["tag"].as_str().unwrap_or_default())),
            ))),
            other => Err(format!("predicate_type '{other}'")),
        }
    }

    /// A `HolderSet<Block>`-shaped field — a `#tag`, a bare id, or a list of
    /// either — flattened to block ids, memoised per spelling.
    fn block_set(&mut self, resolver: &dyn Resolver, value: &Value) -> Arc<HashSet<String>> {
        let key = value.to_string();
        if let Some(existing) = self.block_tags.get(&key) {
            return Arc::clone(existing);
        }
        let mut out = HashSet::new();
        let mut seen = BTreeSet::new();
        collect_blocks(resolver, value, &mut out, &mut seen);
        let out = Arc::new(out);
        self.block_tags.insert(key, Arc::clone(&out));
        out
    }
}

/// A `HolderSet<Block>` spelling: `"#tag"`, a bare block id, or a list of either.
/// Recursive, because block tags nest, and cycle-guarded — the same shape
/// `crate::compose::resolve_block_tag` and `super::resolve_biome_set` use.
fn collect_blocks(
    resolver: &dyn Resolver,
    value: &Value,
    out: &mut HashSet<String>,
    seen: &mut BTreeSet<String>,
) {
    match value {
        Value::String(entry) => match entry.strip_prefix('#') {
            Some(tag) => {
                if !seen.insert(tag.to_string()) {
                    return;
                }
                let document = resolver.block_tag(tag);
                if let Some(values) = document["values"].as_array() {
                    for v in values {
                        match v {
                            Value::Object(o) => {
                                if let Some(id) = o.get("id") {
                                    collect_blocks(resolver, id, out, seen);
                                }
                            }
                            other => collect_blocks(resolver, other, out, seen),
                        }
                    }
                }
            }
            None => {
                out.insert(entry.clone());
            }
        },
        Value::Array(entries) => {
            for entry in entries {
                collect_blocks(resolver, entry, out, seen);
            }
        }
        _ => {}
    }
}

/// A `PosRuleTest` document, or why it cannot be modelled.
///
/// `min_dist`/`max_dist` default to `0`, which vanilla's own constructor rejects
/// (`minDist >= maxDist` throws) — so a document that omits both is malformed
/// data, and refusing it here keeps that a loud ledger row rather than a
/// divide-by-zero producing `NaN` and a chance of `min_chance` everywhere.
fn parse_pos_test(value: &Value) -> Result<super::processor::PosTest, String> {
    use super::processor::{Axis, PosTest};
    match value["predicate_type"].as_str() {
        None | Some("minecraft:always_true") => Ok(PosTest::AlwaysTrue),
        Some("minecraft:axis_aligned_linear_pos") => {
            let min_dist = value["min_dist"].as_i64().unwrap_or(0) as i32;
            let max_dist = value["max_dist"].as_i64().unwrap_or(0) as i32;
            if min_dist >= max_dist {
                return Err(format!(
                    "axis_aligned_linear_pos range [{min_dist},{max_dist}]"
                ));
            }
            let axis = match value["axis"].as_str() {
                None | Some("y") => Axis::Y,
                Some("x") => Axis::X,
                Some("z") => Axis::Z,
                Some(other) => return Err(format!("position_predicate axis '{other}'")),
            };
            Ok(PosTest::AxisAlignedLinear {
                min_chance: value["min_chance"].as_f64().unwrap_or(0.0) as f32,
                max_chance: value["max_chance"].as_f64().unwrap_or(0.0) as f32,
                min_dist,
                max_dist,
                axis,
            })
        }
        Some(other) => Err(format!("position_predicate '{other}'")),
    }
}

/// `BlockState.CODEC` — `{"Name": "...", "Properties": {...}}`.
fn parse_state(value: &Value) -> BlockState {
    let name = value["Name"].as_str().unwrap_or("minecraft:air").to_string();
    let mut properties = BTreeMap::new();
    if let Some(map) = value["Properties"].as_object() {
        for (key, v) in map {
            if let Some(v) = v.as_str() {
                properties.insert(key.clone(), v.to_string());
            }
        }
    }
    BlockState { name, properties }
}

/// The `PlaceSettings` one pool element places itself with, assembled in
/// vanilla's own single-pool-element settings accessor's order.
///
/// The order is the specification: an ignore processor that ran *after* the
/// jigsaw replacement would see `final_state` rather than `minecraft:jigsaw`, and
/// a gravity processor that ran before the rules would move the block the rules
/// then test the world under.
#[must_use]
pub fn place_settings(
    element: &PoolElement,
    rotation: Rotation,
    waterlogging: bool,
    gravity: Option<Arc<ColumnHeights>>,
) -> super::template::PlaceSettings {
    use super::processor::Processor;
    let mut processors = Vec::new();
    let (own, legacy, projection, override_waterlogging) = match element {
        PoolElement::Single {
            processors,
            legacy,
            projection,
            override_waterlogging,
            ..
        } => (
            Some(Arc::clone(processors)),
            *legacy,
            *projection,
            *override_waterlogging,
        ),
        other => (None, false, other.projection(), None),
    };
    if legacy {
        // Vanilla's own legacy single-pool-element settings accessor pops the
        // structure-block ignore entry and adds
        // the structure-and-air one — and adds it *after* the jigsaw-replacement processor,
        // because that pop/add pair runs on the already-built list.
        processors.push(Processor::JigsawReplacement);
        processors.push(Processor::structure_and_air());
    } else {
        processors.push(Processor::structure_block());
        processors.push(Processor::JigsawReplacement);
    }
    if let Some(own) = own {
        processors.extend(own.iter().cloned());
    }
    if projection == Projection::TerrainMatching {
        if let Some(heights) = gravity {
            processors.push(Processor::Gravity {
                heights,
                offset: -1,
            });
        }
    }
    super::template::PlaceSettings {
        rotation,
        mirror: super::template::Mirror::None,
        pivot: [0, 0, 0],
        processors,
        waterlogging: override_waterlogging.unwrap_or(waterlogging),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_worldgen_core::rng::{LegacyRandomSource, WorldgenRandom};

    /// Vanilla's own list-shuffle walks **down** from the end, and its draw count is
    /// `max(0, n - 1)`. Both halves are asserted against a hand-expanded trace of
    /// vanilla's own downward-counting loop on a fresh legacy LCG.
    #[test]
    fn shuffle_matches_utils_downward_fisher_yates() {
        // Hand-expanded: seed 0, size 4 → draws nextInt(4), nextInt(3), nextInt(2).
        let mut oracle = WorldgenRandom::new(LegacyRandomSource::new(0));
        let picks = [
            oracle.next_int_bounded(4),
            oracle.next_int_bounded(3),
            oracle.next_int_bounded(2),
        ];
        let mut expected = [0, 1, 2, 3];
        expected.swap(3, picks[0] as usize);
        expected.swap(2, picks[1] as usize);
        expected.swap(1, picks[2] as usize);

        let mut random = WorldgenRandom::new(LegacyRandomSource::new(0));
        let mut actual = [0, 1, 2, 3];
        shuffle(&mut actual, &mut random);
        assert_eq!(actual, expected);

        // A one-element and an empty list cost no draw at all: the two randoms
        // stay in lockstep afterwards.
        let mut a = WorldgenRandom::new(LegacyRandomSource::new(7));
        let mut b = WorldgenRandom::new(LegacyRandomSource::new(7));
        shuffle(&mut [9i32], &mut a);
        shuffle::<i32, _>(&mut [], &mut a);
        assert_eq!(a.next_int_bounded(1000), b.next_int_bounded(1000));
    }

    /// Selection priority sorts descending and **stably** — equal priorities keep
    /// the shuffled order, which is what makes the sort a filter on top of the
    /// shuffle rather than a second permutation.
    #[test]
    fn selection_priority_sorts_descending_and_stably() {
        let mut blocks: Vec<(i32, &str)> =
            vec![(0, "a"), (5, "b"), (0, "c"), (5, "d"), (-1, "e")];
        blocks.sort_by_key(|b| -b.0);
        assert_eq!(
            blocks.iter().map(|b| b.1).collect::<Vec<_>>(),
            vec!["b", "d", "a", "c", "e"]
        );
    }

    /// `place_settings` puts the ignore/jigsaw pair in the order the element type
    /// demands, and only a `terrain_matching` element gets gravity.
    #[test]
    fn place_settings_orders_the_processor_chain() {
        use super::super::processor::Processor;
        let heights = Arc::new(ColumnHeights::build(0, 0, 0, 0, |_, _| 64));
        let feature_rigid = PoolElement::Feature {
            feature: "minecraft:x".into(),
            placed: None,
            projection: Projection::Rigid,
        };
        let settings = place_settings(
            &feature_rigid,
            Rotation::None,
            true,
            Some(Arc::clone(&heights)),
        );
        assert!(
            !settings
                .processors
                .iter()
                .any(|p| matches!(p, Processor::Gravity { .. })),
            "a rigid element must not get a gravity processor"
        );
        let feature_matching = PoolElement::Feature {
            feature: "minecraft:x".into(),
            placed: None,
            projection: Projection::TerrainMatching,
        };
        let settings = place_settings(&feature_matching, Rotation::None, true, Some(heights));
        assert!(matches!(
            settings.processors.last(),
            Some(Processor::Gravity { .. })
        ));
        // Jigsaw replacement always runs, or every village keeps its jigsaw blocks.
        assert!(
            settings
                .processors
                .iter()
                .any(|p| matches!(p, Processor::JigsawReplacement))
        );
    }

    /// The bundled plains tree pool is the external control for this path: it
    /// names the real `oak` placed feature, rather than a hand-written test
    /// feature. The descriptor must retain the feature body and the exact
    /// structure origin, while a programmatic feature with no body remains a
    /// detectable no-op.
    #[test]
    fn bundled_feature_pool_element_resolves_and_places_at_its_origin() {
        struct Resolver;

        impl crate::density::Resolver for Resolver {
            fn density_function(&self, _id: &str) -> Value {
                Value::Null
            }

            fn noise(&self, _id: &str) -> crate::density::NoiseParams {
                crate::density::NoiseParams {
                    first_octave: 0,
                    amplitudes: Vec::new(),
                }
            }

            fn template_pool(&self, id: &str) -> Value {
                match id {
                    "minecraft:test" => serde_json::from_str(include_str!(
                        "../../../lodestone-server/assets/worldgen/template_pool/village/plains/trees.json"
                    ))
                    .expect("bundled pool JSON"),
                    "minecraft:empty" => serde_json::json!({
                        "elements": [],
                        "fallback": "minecraft:empty"
                    }),
                    _ => Value::Null,
                }
            }

            fn placed_feature(&self, id: &str) -> Value {
                if id == "minecraft:oak" {
                    serde_json::from_str(include_str!(
                        "../../../lodestone-server/assets/worldgen/placed_feature/oak.json"
                    ))
                    .expect("bundled placed-feature JSON")
                } else {
                    Value::Null
                }
            }

            fn configured_feature(&self, id: &str) -> Value {
                if id == "minecraft:oak" {
                    serde_json::from_str(include_str!(
                        "../../../lodestone-server/assets/worldgen/configured_feature/oak.json"
                    ))
                    .expect("bundled configured-feature JSON")
                } else {
                    Value::Null
                }
            }
        }

        let resolver = Resolver;
        let mut pools = PoolStore::default();
        pools
            .load(
                &resolver,
                &mut TemplateStore::default(),
                "minecraft:test",
                &AliasedPools::default(),
            )
            .expect("bundled pool must load");
        let element = pools
            .get("minecraft:test")
            .expect("test pool")
            .expanded
            .first()
            .expect("weighted feature element")
            .clone();
        let placement = element
            .feature_placement([3, 1, 5])
            .expect("parsed feature body");
        assert_eq!(placement.feature, "minecraft:oak");
        assert_eq!(placement.origin, BlockPos { x: 3, y: 1, z: 5 });
        assert_eq!(placement.projection, Projection::Rigid);
        assert!(matches!(
            placement.placed.feature.as_ref(),
            crate::feature::vegetation::ConfiguredFeature::Tree(_)
        ));

        let mut grid = VegGrid::with_footprint(0, 32, 0, 0, -8, 24);
        for x in -8..24 {
            for z in -8..24 {
                grid.seed(x, 0, z, "minecraft:dirt".to_string());
            }
        }
        let mut tags = VegTags::default();
        tags.supports_vegetation.insert("minecraft:dirt".to_string());
        let mut random = LegacyRandomSource::new(0);
        placement.place(&mut random, &mut grid, &tags);
        let writes: Vec<_> = grid.dirty_cells().collect();
        assert!(!writes.is_empty(), "feature placement must write real blocks");
        assert!(
            writes
                .iter()
                .any(|(_, _, _, state)| (*state).starts_with("minecraft:oak_log")),
            "bundled oak feature must place at least one trunk block: {writes:?}"
        );

        let mut no_op_grid = VegGrid::with_footprint(0, 32, 0, 0, -8, 24);
        for x in -8..24 {
            for z in -8..24 {
                no_op_grid.seed(x, 0, z, "minecraft:dirt".to_string());
            }
        }
        let no_op = PoolElement::Feature {
            feature: "minecraft:oak".to_string(),
            placed: None,
            projection: Projection::Rigid,
        };
        assert!(
            no_op.feature_placement([3, 1, 5]).is_none(),
            "the negative control must exercise the no-op detector"
        );
        assert!(no_op_grid.dirty_cells().next().is_none());
    }
}
