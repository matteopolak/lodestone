//! Placed-feature registry, the per-step feature ordering and the decoration driver.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use lodestone_worldgen_core::engine::release26_3::biome::{BiomeId, BiomeTable};
use lodestone_worldgen_core::rng::XoroshiroRandomSource;
use serde_json::Value;

use crate::env::Env;
use crate::feature::Feature;
use crate::json::{Res, array, get, strip};
use crate::level::Level;
use crate::placement::{Placement, PlacementCtx};
use crate::pos::{Pos, Rng};

/// A feature with the modifiers that choose where it is placed.
#[derive(Clone, Debug)]
pub struct PlacedFeature {
    pub name: String,
    pub feature: Arc<Feature>,
    pub placement: Vec<Placement>,
}

impl PlacedFeature {
    /// Parses a placed-feature document (the feature may be a name or an inline document).
    pub fn parse(env: &Env, loader: &mut Loader<'_>, name: &str, v: &Value) -> Res<Self> {
        let feature = loader.feature_ref(env, get(v, "feature", name)?, name)?;
        let placement = array(v, "placement", name)?.iter().map(|p| Placement::parse(env, p, name)).collect::<Res<_>>()?;
        Ok(Self { name: name.to_owned(), feature, placement })
    }

    /// What keeps this placed feature from matching the reference (see [`Feature::gaps`]).
    pub fn gaps(&self, env: &Env, out: &mut std::collections::BTreeSet<String>) {
        let mut states = Vec::new();
        self.placement.iter().for_each(|p| p.survive_states(&mut states));
        for s in states {
            let b = env.blocks.block_of(s);
            if !env.survive.supported(b) {
                out.insert(format!("survive {}", env.blocks.block_name(b)));
            }
        }
        self.feature.gaps(env, out);
    }

    /// Places as a nested feature does (no biome filtering context).
    pub fn place_nested(&self, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
        let ctx = PlacementCtx { top: None, biome_has: &|_, _| false };
        self.place_from(0, level, rng, origin, &ctx)
    }

    /// Runs the modifiers depth-first and places at each surviving position.
    pub fn place(&self, level: &mut Level<'_>, rng: &mut Rng, origin: Pos, ctx: &PlacementCtx<'_>) -> bool {
        self.place_from(0, level, rng, origin, ctx)
    }

    fn place_from(&self, index: usize, level: &mut Level<'_>, rng: &mut Rng, pos: Pos, ctx: &PlacementCtx<'_>) -> bool {
        let Some(modifier) = self.placement.get(index) else {
            return self.feature.place(level, rng, pos);
        };
        let mut out = Vec::new();
        modifier.modify(ctx, level, rng, pos, &mut out);
        let mut any = false;
        for p in out {
            any |= self.place_from(index + 1, level, rng, p, ctx);
        }
        any
    }
}

/// Resolves named features while documents are parsed.
#[derive(Debug)]
pub struct Loader<'a> {
    features: HashMap<String, Arc<Feature>>,
    placed: HashMap<String, Arc<PlacedFeature>>,
    env: &'a Env,
}

impl<'a> Loader<'a> {
    #[must_use]
    pub fn new(env: &'a Env) -> Self {
        Self { features: HashMap::new(), placed: HashMap::new(), env }
    }

    fn feature_ref(&mut self, env: &Env, v: &Value, ctx: &str) -> Res<Arc<Feature>> {
        match v {
            Value::String(name) => self.named_feature(strip(name)),
            Value::Object(_) => Ok(Arc::new(Feature::parse(env, self, v, ctx)?)),
            _ => Err(format!("{ctx}: feature reference expected")),
        }
    }

    /// A placed feature given by registry name or as an inline document.
    pub fn placed_ref(&mut self, env: &Env, v: &Value, ctx: &str) -> Res<Arc<PlacedFeature>> {
        match v {
            Value::String(name) => {
                let name = strip(name);
                if let Some(p) = self.placed.get(name) {
                    return Ok(p.clone());
                }
                let doc = lodestone_worldgen_data_26_3::find(lodestone_worldgen_data_26_3::PLACED_FEATURE, name)
                    .ok_or_else(|| format!("{ctx}: unknown placed feature {name}"))?;
                let v: Value = serde_json::from_str(doc).map_err(|e| format!("placed_feature/{name}: {e}"))?;
                let p = Arc::new(PlacedFeature::parse(env, self, &format!("placed_feature/{name}"), &v)?);
                self.placed.insert(name.to_owned(), p.clone());
                Ok(p)
            }
            Value::Object(_) => Ok(Arc::new(PlacedFeature::parse(env, self, ctx, v)?)),
            _ => Err(format!("{ctx}: placed feature expected")),
        }
    }

    /// A list of placed features (or a single one).
    pub fn placed_list(&mut self, env: &Env, v: &Value, ctx: &str) -> Res<Vec<Arc<PlacedFeature>>> {
        match v {
            Value::Array(a) => a.iter().map(|e| self.placed_ref(env, e, ctx)).collect(),
            other => Ok(vec![self.placed_ref(env, other, ctx)?]),
        }
    }

    /// A configured feature by registry name.
    pub fn named_feature(&mut self, name: &str) -> Res<Arc<Feature>> {
        if let Some(f) = self.features.get(name) {
            return Ok(f.clone());
        }
        let doc = lodestone_worldgen_data_26_3::find(lodestone_worldgen_data_26_3::FEATURE, name).ok_or_else(|| format!("unknown feature {name}"))?;
        let v: Value = serde_json::from_str(doc).map_err(|e| format!("feature {name}: {e}"))?;
        let env = self.env;
        let f = Arc::new(Feature::parse(env, self, &v, &format!("feature/{name}"))?);
        self.features.insert(name.to_owned(), f.clone());
        Ok(f)
    }
}

/// Every placed feature of the release, by name.
#[derive(Debug)]
pub struct Features {
    pub placed: Vec<PlacedFeature>,
    by_name: HashMap<String, usize>,
}

impl Features {
    /// Parses every bundled placed feature.
    pub fn load(env: &Env) -> Res<Self> {
        let mut loader = Loader::new(env);
        let mut placed = Vec::new();
        let mut by_name = HashMap::new();
        for (name, json) in lodestone_worldgen_data_26_3::PLACED_FEATURE {
            let v: Value = serde_json::from_str(json).map_err(|e| format!("placed_feature/{name}: {e}"))?;
            by_name.insert((*name).to_owned(), placed.len());
            placed.push(PlacedFeature::parse(env, &mut loader, &format!("placed_feature/{name}"), &v)?);
            placed.last_mut().expect("just pushed").name = (*name).to_owned();
        }
        Ok(Self { placed, by_name })
    }

    #[must_use]
    pub fn index(&self, name: &str) -> Option<usize> {
        self.by_name.get(strip(name)).copied()
    }
}

/// One generation step: its features in decoration order.
#[derive(Clone, Debug, Default)]
pub struct Step {
    pub features: Vec<usize>,
    index_of: HashMap<usize, usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FeatureData {
    step: usize,
    index: usize,
    placed: usize,
}

/// Orders every source's feature lists into one global per-step sequence, as the reference
/// sorter does: each source contributes edges between consecutive features, and a depth-first
/// search over (step, first-seen index) order yields a reverse topological order.
///
/// # Errors
/// If the sources' orders contradict each other.
pub fn sort_features(sources: &[Vec<Vec<usize>>]) -> Res<Vec<Step>> {
    let mut first_seen: HashMap<usize, usize> = HashMap::new();
    let mut max_step = 0;
    // Edges keyed by (step, index); the sets iterate in the same order.
    let mut edges: BTreeMap<(usize, usize), (FeatureData, BTreeSet<(usize, usize)>)> = BTreeMap::new();
    let mut data_of: HashMap<(usize, usize), FeatureData> = HashMap::new();
    for source in sources {
        let mut list: Vec<FeatureData> = Vec::new();
        max_step = max_step.max(source.len());
        for (step, features) in source.iter().enumerate() {
            for &p in features {
                let next = first_seen.len();
                let index = *first_seen.entry(p).or_insert(next);
                let d = FeatureData { step, index, placed: p };
                data_of.insert((step, index), d);
                list.push(d);
            }
        }
        for i in 0..list.len() {
            let entry = edges.entry((list[i].step, list[i].index)).or_insert_with(|| (list[i], BTreeSet::new()));
            if i + 1 < list.len() {
                entry.1.insert((list[i + 1].step, list[i + 1].index));
            }
        }
    }
    let mut discovered: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut visiting: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut sorted: Vec<FeatureData> = Vec::new();
    fn dfs(
        edges: &BTreeMap<(usize, usize), (FeatureData, BTreeSet<(usize, usize)>)>,
        discovered: &mut BTreeSet<(usize, usize)>,
        visiting: &mut BTreeSet<(usize, usize)>,
        sorted: &mut Vec<FeatureData>,
        current: (usize, usize),
    ) -> bool {
        if discovered.contains(&current) {
            return false;
        }
        if visiting.contains(&current) {
            return true;
        }
        visiting.insert(current);
        if let Some((_, next)) = edges.get(&current) {
            for &n in next {
                if dfs(edges, discovered, visiting, sorted, n) {
                    return true;
                }
            }
        }
        visiting.remove(&current);
        discovered.insert(current);
        sorted.push(edges.get(&current).expect("every node is a key").0);
        false
    }
    for &key in edges.keys() {
        assert!(visiting.is_empty(), "dfs left a vertex in progress");
        if !discovered.contains(&key) && dfs(&edges, &mut discovered, &mut visiting, &mut sorted, key) {
            return Err("feature order cycle".into());
        }
    }
    sorted.reverse();
    let mut steps = vec![Step::default(); max_step];
    for d in sorted {
        let s = &mut steps[d.step];
        s.index_of.insert(d.placed, s.features.len());
        s.features.push(d.placed);
    }
    Ok(steps)
}

/// Whether a placed feature runs under an `only` filter. Entries are feature type names (a
/// placed feature runs when its top-level type is listed), `*` (everything), or `!name` to exclude
/// a placed feature by its registry name without namespace (for example `!oak_checked`).
#[must_use]
pub fn selected(only: Option<&[&str]>, type_name: &str, placed_name: &str) -> bool {
    let Some(only) = only else { return true };
    let name = placed_name.strip_prefix("placed_feature/").unwrap_or(placed_name);
    if only.iter().any(|e| e.strip_prefix('!') == Some(name)) {
        return false;
    }
    only.iter().any(|e| *e == "*" || *e == type_name)
}

/// What the driver reports for each executed feature.
#[derive(Debug)]
pub struct FeatureReport {
    pub step: usize,
    pub index: usize,
    pub placed: usize,
    pub draws: u32,
    /// Changed cells (x, y, z, new state), sorted by position.
    pub changed: Vec<(i32, i32, i32, crate::blocks::State)>,
}

/// The decoration driver for one biome source.
#[derive(Debug)]
pub struct Decorator {
    pub features: Features,
    pub steps: Vec<Step>,
    /// Per biome, per step, the placed features in the biome's own order.
    biome_steps: HashMap<BiomeId, Vec<Vec<usize>>>,
    biome_has: HashMap<BiomeId, HashSet<usize>>,
    biomes: Arc<BiomeTable>,
}

impl Decorator {
    /// `possible` is the biome source's possible biomes in its own order.
    pub fn new(features: Features, biomes: &BiomeTable, possible: &[BiomeId]) -> Res<Self> {
        let mut biome_steps = HashMap::new();
        let mut biome_has = HashMap::new();
        for &b in possible {
            let name = biomes.info(b).name.clone();
            let doc = lodestone_worldgen_data_26_3::find(lodestone_worldgen_data_26_3::BIOME, &name).ok_or_else(|| format!("no biome document {name}"))?;
            let v: Value = serde_json::from_str(doc).map_err(|e| format!("biome {name}: {e}"))?;
            let mut steps = Vec::new();
            let mut has = HashSet::new();
            for step in array(&v, "features", &name)? {
                let mut list = Vec::new();
                for f in step.as_array().ok_or_else(|| format!("{name}: step list expected"))? {
                    let fname = f.as_str().ok_or_else(|| format!("{name}: feature name expected"))?;
                    let idx = features.index(fname).ok_or_else(|| format!("{name}: unknown placed feature {fname}"))?;
                    has.insert(idx);
                    list.push(idx);
                }
                steps.push(list);
            }
            biome_steps.insert(b, steps);
            biome_has.insert(b, has);
        }
        let sources: Vec<Vec<Vec<usize>>> = possible.iter().map(|b| biome_steps[b].clone()).collect();
        let steps = sort_features(&sources)?;
        Ok(Self { features, steps, biome_steps, biome_has, biomes: Arc::new(biomes.clone()) })
    }

    /// The placed features that still have gaps, with their gaps (see [`Feature::gaps`]).
    #[must_use]
    pub fn gaps(&self, env: &Env) -> Vec<(String, Vec<String>)> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for steps in self.biome_steps.values() {
            for list in steps {
                for &p in list {
                    if seen.insert(p) {
                        let mut g = std::collections::BTreeSet::new();
                        self.features.placed[p].gaps(env, &mut g);
                        if !g.is_empty() {
                            out.push((self.features.placed[p].name.trim_start_matches("placed_feature/").to_owned(), g.into_iter().collect()));
                        }
                    }
                }
            }
        }
        out.sort();
        out
    }

    /// Decorates the chunk at `(chunk_x, chunk_z)`. `present` are the biomes stored in the
    /// nine chunks' sections (restricted here to the possible biomes). `only` limits which
    /// feature types run; `report` receives each executed feature.
    #[allow(clippy::too_many_arguments)]
    pub fn decorate(
        &self,
        level: &mut Level<'_>,
        chunk_x: i32,
        chunk_z: i32,
        present: &[BiomeId],
        only: Option<&[&str]>,
        mut report: impl FnMut(FeatureReport),
    ) {
        level.biomes = Some(self.biomes.clone());
        let mut rng = Rng::new(XoroshiroRandomSource::new(0));
        let origin = Pos::new(chunk_x * 16, level.min_y, chunk_z * 16);
        let decoration_seed = rng.set_decoration_seed(level.seed, origin.x, origin.z);
        let possible: Vec<BiomeId> = present.iter().copied().filter(|b| self.biome_steps.contains_key(b)).collect();
        level.begin_journal();
        let biome_has = |b: BiomeId, placed: usize| self.biome_has.get(&b).is_some_and(|s| s.contains(&placed));
        for (step_index, step) in self.steps.iter().enumerate() {
            let mut indices: BTreeSet<usize> = BTreeSet::new();
            for b in &possible {
                if let Some(list) = self.biome_steps[b].get(step_index) {
                    for p in list {
                        indices.insert(step.index_of[p]);
                    }
                }
            }
            for global in indices {
                let placed_index = step.features[global];
                let placed = &self.features.placed[placed_index];
                if !selected(only, placed.feature.type_name(), &placed.name) {
                    continue;
                }
                rng.set_feature_seed(decoration_seed, global as i32, step_index as i32);
                let c0 = rng.count();
                let ctx = PlacementCtx { top: Some(placed_index), biome_has: &biome_has };
                placed.place(level, &mut rng, origin, &ctx);
                let draws = rng.count() - c0;
                let changed = level.drain_changes();
                report(FeatureReport { step: step_index, index: global, placed: placed_index, draws, changed });
            }
        }
    }
}
