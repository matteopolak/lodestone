//! Material rules: the data-driven rule trees that decide which block each
//! solid cell becomes once terrain shape is known (surface layers, bedrock,
//! ore veins, strata).
//!
//! Parsing happens at load time ([`parse_rule`]) into a [`Rule`] tree whose
//! noises, positional random streams and ore density functions are held by
//! index; [`MaterialSystem`] resolves those indices against a compiled
//! [`Program`]. Evaluation lives in `surface`.

use std::collections::HashMap;

use serde_json::Value;

use super::biome::{BiomeId, BiomeTable};
use super::tree::{NodeId, Resources, Tree, TreeError};

/// An interned block state, identified by `name[sorted=props]` exactly as the
/// data writes it. Mapping a state to a registry block state is the consumer's job.
pub type StateId = u32;

#[derive(Clone, Debug, Default)]
pub struct StateTable {
    keys: Vec<String>,
    index: HashMap<String, StateId>,
}

impl StateTable {
    pub fn intern(&mut self, key: &str) -> StateId {
        if let Some(&id) = self.index.get(key) {
            return id;
        }
        let id = self.keys.len() as StateId;
        self.keys.push(key.to_owned());
        self.index.insert(key.to_owned(), id);
        id
    }

    pub fn key(&self, id: StateId) -> &str {
        &self.keys[id as usize]
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    fn block_name(&self, id: StateId) -> &str {
        let k = self.key(id);
        k.split_once('[').map_or(k, |(n, _)| n)
    }

    pub fn is_air(&self, id: StateId) -> bool {
        matches!(self.block_name(id), "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air")
    }

    /// Whether the state carries a fluid (a fluid block, or a waterlogged one).
    pub fn has_fluid(&self, id: StateId) -> bool {
        matches!(self.block_name(id), "minecraft:water" | "minecraft:lava") || self.key(id).contains("waterlogged=true")
    }

    pub fn same_block(&self, a: StateId, b: StateId) -> bool {
        self.block_name(a) == self.block_name(b)
    }

    pub fn is_block(&self, id: StateId, block: &str) -> bool {
        self.block_name(id) == block
    }
}

/// `name` or `name[k=v,...]` with properties sorted, from either JSON form of a state.
pub fn state_key(v: &Value) -> Result<String, TreeError> {
    let norm = |n: &str| if n.contains(':') { n.to_owned() } else { format!("minecraft:{n}") };
    match v {
        Value::String(s) => Ok(norm(s)),
        Value::Object(o) => {
            let name = o.get("Name").and_then(Value::as_str).ok_or_else(|| TreeError::Invalid("block state without Name".into()))?;
            let mut props: Vec<(String, String)> = o
                .get("Properties")
                .and_then(Value::as_object)
                .map(|p| {
                    p.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), str::to_owned)))
                        .collect()
                })
                .unwrap_or_default();
            props.sort();
            if props.is_empty() {
                Ok(norm(name))
            } else {
                let body: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
                Ok(format!("{}[{}]", norm(name), body.join(",")))
            }
        }
        other => Err(TreeError::Invalid(format!("not a block state: {other}"))),
    }
}

/// A vertical position relative to the dimension's generation range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Absolute(i32),
    AboveBottom(i32),
    BelowTop(i32),
    RelativeToSeaLevel(i32),
}

impl Anchor {
    fn parse(v: &Value) -> Result<Self, TreeError> {
        let o = v.as_object().ok_or_else(|| TreeError::Invalid("vertical anchor must be an object".into()))?;
        let get = |k: &str| o.get(k).and_then(Value::as_i64).map(|n| n as i32);
        if let Some(n) = get("absolute") {
            Ok(Self::Absolute(n))
        } else if let Some(n) = get("above_bottom") {
            Ok(Self::AboveBottom(n))
        } else if let Some(n) = get("below_top") {
            Ok(Self::BelowTop(n))
        } else if let Some(n) = get("relative_to_sea_level") {
            Ok(Self::RelativeToSeaLevel(n))
        } else {
            Err(TreeError::Invalid(format!("unknown vertical anchor {v}")))
        }
    }

    pub fn resolve(self, g: GenContext) -> i32 {
        match self {
            Self::Absolute(y) => y,
            Self::AboveBottom(o) => g.min_y + o,
            Self::BelowTop(o) => g.height - 1 + g.min_y - o,
            Self::RelativeToSeaLevel(o) => g.sea_level + o,
        }
    }
}

/// The generation range anchors resolve against: the lower of the dimension's
/// and the settings' bounds.
#[derive(Clone, Copy, Debug)]
pub struct GenContext {
    pub min_y: i32,
    pub height: i32,
    pub sea_level: i32,
}

#[derive(Clone, Debug)]
pub enum Cond {
    Biome(Vec<BiomeId>),
    /// `slot` indexes a memo shared by conditions on the same noise and dimensionality.
    NoiseThreshold { noise: usize, slot: usize, min: f64, max: f64, is_3d: bool },
    VerticalGradient { random: usize, true_at_and_below: Anchor, false_at_and_above: Anchor },
    YAbove { anchor: Anchor, surface_depth_multiplier: i32, add_stone_depth: bool },
    Water { offset: i32, surface_depth_multiplier: i32, add_stone_depth: bool },
    Temperature,
    Steep,
    Not(Box<Cond>),
    Hole,
    AbovePreliminarySurface,
    StoneDepth { offset: i32, add_surface_depth: bool, secondary_depth_range: i32, ceiling: bool },
}

#[derive(Clone, Debug)]
pub struct OreVein {
    pub ore: StateId,
    pub raw_ore: StateId,
    pub filler: StateId,
    pub raw_ore_chance: f32,
    /// Index into [`MaterialSystem::ore_functions`].
    pub functions: usize,
}

#[derive(Clone, Debug)]
pub enum Rule {
    Block(StateId),
    Bandlands,
    Sequence(Vec<Rule>),
    Condition(Cond, Box<Rule>),
    OreVein(OreVein),
}

/// Everything parsing collects besides the rule tree itself.
#[derive(Debug, Default)]
pub struct ParsedMaterial {
    pub rule: Option<Rule>,
    /// Noise registry names, indexed by `Cond::NoiseThreshold::noise`.
    pub noise_names: Vec<String>,
    /// `(noise, is_3d)` pairs, indexed by `slot`.
    pub noise_slots: Vec<(usize, bool)>,
    /// Positional random stream names, indexed by `Cond::VerticalGradient::random`.
    pub random_names: Vec<String>,
    /// Ore density, richness and filler-gap function roots, in rule order.
    pub ore_nodes: Vec<[NodeId; 3]>,
}

/// Where material rules and conditions are looked up by name.
pub trait MaterialResources: Resources {
    fn material_rule(&self, name: &str) -> Option<&Value>;
    fn material_condition(&self, name: &str) -> Option<&Value>;
}

struct Parser<'a> {
    res: &'a dyn MaterialResources,
    biomes: &'a BiomeTable,
    tree: &'a mut Tree,
    states: &'a mut StateTable,
    out: ParsedMaterial,
}

const MAX_DEPTH: u32 = 64;

/// Parses a material rule (a registry name or an inline document).
pub fn parse_rule(
    value: &Value,
    res: &dyn MaterialResources,
    biomes: &BiomeTable,
    tree: &mut Tree,
    states: &mut StateTable,
) -> Result<ParsedMaterial, TreeError> {
    let mut p = Parser { res, biomes, tree, states, out: ParsedMaterial::default() };
    let rule = p.rule(value, 0)?;
    p.out.rule = Some(rule);
    Ok(p.out)
}

fn invalid<T>(msg: impl Into<String>) -> Result<T, TreeError> {
    Err(TreeError::Invalid(msg.into()))
}

impl Parser<'_> {
    fn field<'v>(obj: &'v Value, key: &str) -> Result<&'v Value, TreeError> {
        obj.get(key).ok_or_else(|| TreeError::Invalid(format!("missing field {key}")))
    }

    fn state(&mut self, v: &Value) -> Result<StateId, TreeError> {
        Ok(self.states.intern(&state_key(v)?))
    }

    fn rule(&mut self, v: &Value, depth: u32) -> Result<Rule, TreeError> {
        if depth > MAX_DEPTH {
            return invalid("material rule nesting too deep");
        }
        if let Value::String(name) = v {
            let doc = self.res.material_rule(name).ok_or_else(|| TreeError::Missing(format!("material_rule {name}")))?.clone();
            return self.rule(&doc, depth + 1);
        }
        let ty = Self::field(v, "type")?.as_str().ok_or_else(|| TreeError::Invalid("rule type".into()))?;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "block" => Ok(Rule::Block(self.state(Self::field(v, "result_state")?)?)),
            "bandlands" => Ok(Rule::Bandlands),
            "sequence" => {
                let items = Self::field(v, "sequence")?.as_array().ok_or_else(|| TreeError::Invalid("sequence".into()))?;
                if items.is_empty() {
                    return invalid("a sequence needs at least one rule");
                }
                let mut rules = Vec::with_capacity(items.len());
                for item in items {
                    rules.push(self.rule(item, depth + 1)?);
                }
                Ok(Rule::Sequence(rules))
            }
            "condition" => {
                let cond = self.cond(Self::field(v, "if_true")?, depth + 1)?;
                let then = self.rule(Self::field(v, "then_run")?, depth + 1)?;
                Ok(Rule::Condition(cond, Box::new(then)))
            }
            "ore_vein" => {
                let ore = self.state(Self::field(v, "ore_block")?)?;
                let raw_ore = self.state(Self::field(v, "raw_ore_block")?)?;
                let filler = self.state(Self::field(v, "filler_block")?)?;
                let raw_ore_chance = Self::field(v, "raw_ore_chance")?.as_f64().ok_or_else(|| TreeError::Invalid("raw_ore_chance".into()))? as f32;
                let density = self.tree.parse(Self::field(v, "density")?, self.res)?;
                let richness = self.tree.parse(Self::field(v, "richness")?, self.res)?;
                let filler_gap = self.tree.parse(Self::field(v, "filler_gap")?, self.res)?;
                self.out.ore_nodes.push([density, richness, filler_gap]);
                Ok(Rule::OreVein(OreVein { ore, raw_ore, filler, raw_ore_chance, functions: self.out.ore_nodes.len() - 1 }))
            }
            other => invalid(format!("unknown material rule type {other}")),
        }
    }

    fn noise_index(&mut self, name: &str) -> Result<usize, TreeError> {
        self.tree.load_noise(name, self.res)?;
        let key = if name.contains(':') { name.to_owned() } else { format!("minecraft:{name}") };
        if let Some(i) = self.out.noise_names.iter().position(|n| *n == key) {
            return Ok(i);
        }
        self.out.noise_names.push(key);
        Ok(self.out.noise_names.len() - 1)
    }

    fn random_index(&mut self, name: &str) -> usize {
        let key = if name.contains(':') { name.to_owned() } else { format!("minecraft:{name}") };
        if let Some(i) = self.out.random_names.iter().position(|n| *n == key) {
            return i;
        }
        self.out.random_names.push(key);
        self.out.random_names.len() - 1
    }

    fn cond(&mut self, v: &Value, depth: u32) -> Result<Cond, TreeError> {
        if depth > MAX_DEPTH {
            return invalid("material condition nesting too deep");
        }
        if let Value::String(name) = v {
            let doc = self.res.material_condition(name).ok_or_else(|| TreeError::Missing(format!("material_condition {name}")))?.clone();
            return self.cond(&doc, depth + 1);
        }
        let ty = Self::field(v, "type")?.as_str().ok_or_else(|| TreeError::Invalid("condition type".into()))?;
        let int = |k: &str| -> Result<i32, TreeError> {
            Self::field(v, k)?.as_i64().map(|n| n as i32).ok_or_else(|| TreeError::Invalid(format!("{k} must be an integer")))
        };
        let boolean = |k: &str| -> Result<bool, TreeError> {
            Self::field(v, k)?.as_bool().ok_or_else(|| TreeError::Invalid(format!("{k} must be a boolean")))
        };
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "biome" => {
                let names: Vec<&str> = match Self::field(v, "biome_is")? {
                    Value::String(s) => vec![s.as_str()],
                    Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                    other => return invalid(format!("biome_is: {other}")),
                };
                let mut ids = Vec::new();
                for n in names {
                    if n.starts_with('#') {
                        return invalid(format!("biome tags are not supported: {n}"));
                    }
                    ids.push(self.biomes.id(n).ok_or_else(|| TreeError::Missing(format!("biome {n}")))?);
                }
                Ok(Cond::Biome(ids))
            }
            "noise_threshold" => {
                let name = Self::field(v, "noise")?.as_str().ok_or_else(|| TreeError::Invalid("noise".into()))?;
                let noise = self.noise_index(name)?;
                let min = Self::field(v, "min_threshold")?.as_f64().ok_or_else(|| TreeError::Invalid("min_threshold".into()))?;
                let max = Self::field(v, "max_threshold")?.as_f64().ok_or_else(|| TreeError::Invalid("max_threshold".into()))?;
                let is_3d = v.get("is_3d").and_then(Value::as_bool).unwrap_or(false);
                let slot = match self.out.noise_slots.iter().position(|&s| s == (noise, is_3d)) {
                    Some(s) => s,
                    None => {
                        self.out.noise_slots.push((noise, is_3d));
                        self.out.noise_slots.len() - 1
                    }
                };
                Ok(Cond::NoiseThreshold { noise, slot, min, max, is_3d })
            }
            "vertical_gradient" => {
                let name = Self::field(v, "random_name")?.as_str().ok_or_else(|| TreeError::Invalid("random_name".into()))?;
                Ok(Cond::VerticalGradient {
                    random: self.random_index(name),
                    true_at_and_below: Anchor::parse(Self::field(v, "true_at_and_below")?)?,
                    false_at_and_above: Anchor::parse(Self::field(v, "false_at_and_above")?)?,
                })
            }
            "y_above" => Ok(Cond::YAbove {
                anchor: Anchor::parse(Self::field(v, "anchor")?)?,
                surface_depth_multiplier: int("surface_depth_multiplier")?,
                add_stone_depth: boolean("add_stone_depth")?,
            }),
            "water" => Ok(Cond::Water {
                offset: int("offset")?,
                surface_depth_multiplier: int("surface_depth_multiplier")?,
                add_stone_depth: boolean("add_stone_depth")?,
            }),
            "temperature" => Ok(Cond::Temperature),
            "steep" => Ok(Cond::Steep),
            "not" => Ok(Cond::Not(Box::new(self.cond(Self::field(v, "invert")?, depth + 1)?))),
            "hole" => Ok(Cond::Hole),
            "above_preliminary_surface" => Ok(Cond::AbovePreliminarySurface),
            "stone_depth" => Ok(Cond::StoneDepth {
                offset: int("offset")?,
                add_surface_depth: boolean("add_surface_depth")?,
                secondary_depth_range: int("secondary_depth_range")?,
                ceiling: match Self::field(v, "surface_type")?.as_str() {
                    Some("ceiling") => true,
                    Some("floor") => false,
                    other => return invalid(format!("surface_type: {other:?}")),
                },
            }),
            other => invalid(format!("unknown material condition type {other}")),
        }
    }
}

/// A material rule bound to a compiled program and a seed.
#[derive(Debug)]
pub struct MaterialSystem {
    pub rule: Rule,
    pub states: StateTable,
    pub(crate) noise_map: Vec<usize>,
    pub(crate) noise_slots: Vec<(usize, bool)>,
    pub(crate) randoms: Vec<crate::rng::AnyPositionalFactory>,
    pub(crate) ore_functions: Vec<[super::sampler::SId; 3]>,
    pub(crate) noises: SurfaceNoises,
    pub(crate) noise_random: crate::rng::AnyPositionalFactory,
    pub(crate) clay_bands: Vec<StateId>,
    pub(crate) ore_random: crate::rng::AnyPositionalFactory,
    pub default_block: StateId,
    pub air: StateId,
    pub water: StateId,
    pub lava: StateId,
    pub(crate) snow_block: StateId,
    pub(crate) packed_ice: StateId,
    pub sea_level: i32,
    pub(crate) preliminary_surface: super::sampler::SId,
    pub(crate) eroded_badlands: Option<BiomeId>,
    pub(crate) frozen_ocean: Option<BiomeId>,
    pub(crate) deep_frozen_ocean: Option<BiomeId>,
}

/// Program noise indices of the fixed surface noises.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SurfaceNoises {
    pub surface: usize,
    pub surface_secondary: usize,
    pub clay_bands_offset: usize,
    pub badlands_pillar: usize,
    pub badlands_pillar_roof: usize,
    pub badlands_surface: usize,
    pub iceberg_pillar: usize,
    pub iceberg_pillar_roof: usize,
    pub iceberg_surface: usize,
}

/// Fixed surface noise names, in the order of [`SurfaceNoises`].
pub(crate) const SURFACE_NOISE_NAMES: [&str; 9] = [
    "minecraft:surface",
    "minecraft:surface_secondary",
    "minecraft:clay_bands_offset",
    "minecraft:badlands_pillar",
    "minecraft:badlands_pillar_roof",
    "minecraft:badlands_surface",
    "minecraft:iceberg_pillar",
    "minecraft:iceberg_pillar_roof",
    "minecraft:iceberg_surface",
];

/// The 192 stratified terracotta bands, drawn from the world's `clay_bands` stream.
pub(crate) fn generate_bands<R: crate::rng::RandomSource>(random: &mut R, st: &mut StateTable) -> Vec<StateId> {
    let terracotta = st.intern("minecraft:terracotta");
    let orange = st.intern("minecraft:orange_terracotta");
    let yellow = st.intern("minecraft:yellow_terracotta");
    let brown = st.intern("minecraft:brown_terracotta");
    let red = st.intern("minecraft:red_terracotta");
    let white = st.intern("minecraft:white_terracotta");
    let light_gray = st.intern("minecraft:light_gray_terracotta");
    let mut bands = vec![terracotta; 192];
    let mut i = 0usize;
    while i < bands.len() {
        i += random.next_int_bounded(5) as usize + 1;
        if i < bands.len() {
            bands[i] = orange;
        }
        i += 1;
    }
    let mut make = |random: &mut R, base_width: i32, state: StateId| {
        let count = random.next_int_bounded(10) + 6;
        for _ in 0..count {
            let width = base_width + random.next_int_bounded(3);
            let start = random.next_int_bounded(bands.len() as i32) as usize;
            let mut p = 0usize;
            while start + p < bands.len() && (p as i32) < width {
                bands[start + p] = state;
                p += 1;
            }
        }
    };
    make(random, 1, yellow);
    make(random, 2, brown);
    make(random, 1, red);
    let white_count = random.next_int_bounded(7) + 9;
    let mut n = 0;
    let mut start = 0usize;
    while n < white_count && start < bands.len() {
        bands[start] = white;
        if start >= 2 && random.next_bool() {
            bands[start - 1] = light_gray;
        }
        if start + 1 < bands.len() && random.next_bool() {
            bands[start + 1] = light_gray;
        }
        n += 1;
        start += random.next_int_bounded(16) as usize + 4;
    }
    bands
}
