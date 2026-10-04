use std::collections::HashMap;

use lodestone_data::biomes::BuiltinBiome;
use lodestone_data::block_states::StateId;
use serde_json::Value;

use crate::surface::{BiomeSet, CompiledRule, Rule};

use super::{FrontendError, resource_id};
use super::state::parse_state_at;

const MAX_DEPTH: usize = 128;
const MAX_NODES: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MaterialDensityId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MaterialNoiseId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MaterialRandomId(pub usize);

#[derive(Debug)]
pub enum MaterialCondition {
    AbovePreliminarySurface,
    Biome(BiomeSet),
    NoiseThreshold { noise: MaterialNoiseId, min: f64, max: f64, is_3d: bool },
    Not(Box<Self>),
    Steep,
    StoneDepth { offset: i32, add_surface_depth: bool, secondary_depth_range: i32, ceiling: bool },
    Temperature,
    Hole,
    VerticalGradient { random: MaterialRandomId, true_at_and_below: i32, false_at_and_above: i32 },
    Water { offset: i32, surface_depth_multiplier: i32, add_stone_depth: bool },
    YAbove { anchor_y: i32, surface_depth_multiplier: i32, add_stone_depth: bool },
}

impl MaterialCondition {
    fn biome_value(&self, biome: BuiltinBiome) -> Option<bool> {
        match self {
            Self::Biome(set) => Some(set.contains_builtin(biome)),
            Self::Not(inner) => inner.biome_value(biome).map(|value| !value),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialSampling { VolumeOrScalar, Scalar }

/// Request-local inputs at the current scan position. Numeric ids are bound
/// before scanning; ordinal zero starts a fresh positional random stream.
pub trait MaterialInputs {
    fn condition(&mut self, index: usize, condition: &MaterialCondition) -> bool;
    fn bandlands(&mut self) -> StateId;
    fn density(&mut self, id: MaterialDensityId, sampling: MaterialSampling) -> f32;
    fn random_float(&mut self, id: MaterialRandomId, ordinal: u8) -> f32;
    fn builtin_biome(&mut self) -> Option<BuiltinBiome> { None }
}

#[derive(Debug)]
struct OreVein {
    density: MaterialDensityId,
    richness: MaterialDensityId,
    gap: MaterialDensityId,
    ore: StateId,
    raw: StateId,
    filler: StateId,
    raw_chance: f32,
    random: MaterialRandomId,
}

impl OreVein {
    fn apply(&self, inputs: &mut dyn MaterialInputs) -> Option<StateId> {
        let density = inputs.density(self.density, MaterialSampling::VolumeOrScalar);
        if density <= 0.0 { return None; }
        if inputs.random_float(self.random, 0) > density { return None; }
        let richness = inputs.density(self.richness, MaterialSampling::VolumeOrScalar);
        if inputs.random_float(self.random, 1) < richness
            && inputs.density(self.gap, MaterialSampling::Scalar) < 0.0
        {
            Some(if inputs.random_float(self.random, 2) < self.raw_chance { self.raw } else { self.ore })
        } else {
            Some(self.filler)
        }
    }
}

/// Typed current material rules lowered through the surface continuation graph.
/// This owns no numeric workspace or retained coordinate cache.
pub struct MaterialGraph {
    compiled: CompiledRule,
    conditions: Vec<MaterialCondition>,
    veins: Vec<OreVein>,
}

impl std::fmt::Debug for MaterialGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaterialGraph").field("conditions", &self.conditions.len())
            .field("ore_veins", &self.veins.len()).finish_non_exhaustive()
    }
}

impl MaterialGraph {
    pub fn conditions(&self) -> &[MaterialCondition] { &self.conditions }
    pub fn apply(&self, inputs: &mut dyn MaterialInputs) -> Option<StateId> {
        self.compiled.run_material(
            inputs,
            |inputs, index| inputs.condition(index, &self.conditions[index]),
            |inputs, _| inputs.bandlands(),
            |inputs, index| self.veins[index].apply(inputs),
        )
    }
}

/// Load-time density documents must be compiled together and bound to their
/// matching `MaterialDensityId` ordinals before the graph can execute.
#[derive(Debug)]
pub struct BakedMaterial {
    pub graph: MaterialGraph,
    pub density_roots: Vec<Value>,
}

/// Resolves separate material-rule and material-condition resource domains.
pub struct MaterialBaker<'a> {
    rules: &'a dyn Fn(&str) -> Option<Value>,
    conditions: &'a dyn Fn(&str) -> Option<Value>,
    documents: HashMap<(bool, String), Value>,
    active: Vec<(bool, String)>,
    nodes: usize,
    min_y: i32,
    height: i32,
    predicates: Vec<MaterialCondition>,
    veins: Vec<OreVein>,
    densities: Vec<Value>,
    noises: Vec<String>,
    randoms: Vec<String>,
}

impl std::fmt::Debug for MaterialBaker<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaterialBaker").field("cached_documents", &self.documents.len())
            .field("visited_nodes", &self.nodes).finish_non_exhaustive()
    }
}

impl<'a> MaterialBaker<'a> {
    pub fn new(
        rules: &'a dyn Fn(&str) -> Option<Value>,
        conditions: &'a dyn Fn(&str) -> Option<Value>,
        min_y: i32,
        height: i32,
    ) -> Result<Self, FrontendError> {
        if height <= 0 || min_y.checked_add(height - 1).is_none() {
            return Err(FrontendError::new("material", "invalid generation window"));
        }
        Ok(Self { rules, conditions, documents: HashMap::new(), active: Vec::new(),
            nodes: 0, min_y, height, predicates: Vec::new(), veins: Vec::new(),
            densities: Vec::new(), noises: Vec::new(), randoms: Vec::new() })
    }

    pub fn bake_holder(mut self, value: &Value) -> Result<BakedMaterial, FrontendError> {
        let rule = self.rule(value, "material_rule", 0)?;
        let mut compiled = CompiledRule::new(&rule);
        compiled.prepare_material_biomes(|index, biome| self.predicates[index].biome_value(biome));
        Ok(BakedMaterial { graph: MaterialGraph { compiled, conditions: self.predicates,
            veins: self.veins }, density_roots: self.densities })
    }

    fn visit(&mut self, path: &str, depth: usize) -> Result<(), FrontendError> {
        self.nodes += 1;
        if depth > MAX_DEPTH || self.nodes > MAX_NODES {
            return Err(FrontendError::new(path, "material graph exceeds depth or node budget"));
        }
        Ok(())
    }

    fn document(&mut self, raw: &str, condition: bool, path: &str) -> Result<(String, Value), FrontendError> {
        let id = resource_id(raw, path)?;
        let key = (condition, id.clone());
        if self.active.contains(&key) {
            return Err(FrontendError::new(path, format!("material reference cycle through {id}")));
        }
        let value = if let Some(value) = self.documents.get(&key) { value.clone() } else {
            let lookup = if condition { self.conditions } else { self.rules };
            let value = lookup(&id).ok_or_else(|| FrontendError::new(path, format!("unresolved material holder {id}")))?;
            self.documents.insert(key.clone(), value.clone());
            value
        };
        self.active.push(key);
        Ok((id, value))
    }

    fn rule(&mut self, value: &Value, path: &str, depth: usize) -> Result<Rule, FrontendError> {
        self.visit(path, depth)?;
        if let Some(raw) = value.as_str() {
            let (id, document) = self.document(raw, false, path)?;
            let result = self.rule(&document, &format!("material_rule/{id}"), depth + 1);
            self.active.pop();
            return result;
        }
        match kind(value, path)?.as_str() {
            "minecraft:block" => Ok(Rule::Block(parse_state_at(&value["result_state"], path)?)),
            "minecraft:bandlands" => Ok(Rule::Bandlands(0)),
            "minecraft:sequence" => {
                let children = value["sequence"].as_array().ok_or_else(|| FrontendError::new(path, "sequence must be an array"))?;
                children.iter().enumerate().map(|(i, child)| self.rule(child, &format!("{path}.sequence[{i}]"), depth + 1))
                    .collect::<Result<Vec<_>, _>>().map(Rule::Sequence)
            }
            "minecraft:condition" => {
                let condition = self.condition(&value["if_true"], &format!("{path}.if_true"), depth + 1)?;
                let index = self.predicates.len();
                self.predicates.push(condition);
                Ok(Rule::Condition(index, Box::new(self.rule(&value["then_run"], &format!("{path}.then_run"), depth + 1)?)))
            }
            "minecraft:ore_vein" => {
                let chance = number(&value["raw_ore_chance"], path)? as f32;
                if !(0.0..=1.0).contains(&chance) { return Err(FrontendError::new(path, "raw_ore_chance must be in [0, 1]")); }
                let vein = OreVein { density: self.density(&value["density"], path)?,
                    richness: self.density(&value["richness"], path)?, gap: self.density(&value["filler_gap"], path)?,
                    ore: parse_state_at(&value["ore_block"], path)?, raw: parse_state_at(&value["raw_ore_block"], path)?,
                    filler: parse_state_at(&value["filler_block"], path)?, raw_chance: chance,
                    random: MaterialRandomId(intern(&mut self.randoms, "minecraft:ore".to_owned())) };
                let index = self.veins.len();
                self.veins.push(vein);
                Ok(Rule::OreVein(index))
            }
            other => Err(FrontendError::new(path, format!("unsupported material rule {other}"))),
        }
    }

    fn condition(&mut self, value: &Value, path: &str, depth: usize) -> Result<MaterialCondition, FrontendError> {
        self.visit(path, depth)?;
        if let Some(raw) = value.as_str() {
            let (id, document) = self.document(raw, true, path)?;
            let result = self.condition(&document, &format!("material_condition/{id}"), depth + 1);
            self.active.pop();
            return result;
        }
        let condition = match kind(value, path)?.as_str() {
            "minecraft:above_preliminary_surface" => MaterialCondition::AbovePreliminarySurface,
            "minecraft:biome" => {
                let names = match &value["biome_is"] {
                    Value::String(name) => vec![resource_id(name, path)?],
                    Value::Array(names) => names.iter().map(|name| string(name, path).and_then(|name| resource_id(name, path))).collect::<Result<_, _>>()?,
                    _ => return Err(FrontendError::new(path, "biome_is requires a name or array of names")),
                };
                MaterialCondition::Biome(BiomeSet::from_names(names))
            }
            "minecraft:noise_threshold" => MaterialCondition::NoiseThreshold {
                noise: MaterialNoiseId(intern(&mut self.noises, resource_id(string(&value["noise"], path)?, path)?)),
                min: number(&value["min_threshold"], path)?, max: number(&value["max_threshold"], path)?,
                is_3d: value.get("is_3d").map(|v| boolean(v, path)).transpose()?.unwrap_or(false),
            },
            "minecraft:not" => MaterialCondition::Not(Box::new(self.condition(&value["invert"], &format!("{path}.invert"), depth + 1)?)),
            "minecraft:steep" => MaterialCondition::Steep,
            "minecraft:stone_depth" => MaterialCondition::StoneDepth { offset: integer(&value["offset"], path)?,
                add_surface_depth: boolean(&value["add_surface_depth"], path)?,
                secondary_depth_range: integer(&value["secondary_depth_range"], path)?,
                ceiling: match string(&value["surface_type"], path)? { "ceiling" => true, "floor" => false,
                    _ => return Err(FrontendError::new(path, "surface_type requires floor or ceiling")) } },
            "minecraft:temperature" => MaterialCondition::Temperature,
            "minecraft:hole" => MaterialCondition::Hole,
            "minecraft:vertical_gradient" => MaterialCondition::VerticalGradient {
                random: MaterialRandomId(intern(&mut self.randoms, resource_id(string(&value["random_name"], path)?, path)?)),
                true_at_and_below: self.anchor(&value["true_at_and_below"], path)?,
                false_at_and_above: self.anchor(&value["false_at_and_above"], path)?,
            },
            "minecraft:water" => MaterialCondition::Water { offset: integer(&value["offset"], path)?,
                surface_depth_multiplier: multiplier(&value["surface_depth_multiplier"], path)?,
                add_stone_depth: boolean(&value["add_stone_depth"], path)? },
            "minecraft:y_above" => MaterialCondition::YAbove { anchor_y: self.anchor(&value["anchor"], path)?,
                surface_depth_multiplier: multiplier(&value["surface_depth_multiplier"], path)?,
                add_stone_depth: boolean(&value["add_stone_depth"], path)? },
            other => return Err(FrontendError::new(path, format!("unsupported material condition {other}"))),
        };
        Ok(condition)
    }

    fn anchor(&self, value: &Value, path: &str) -> Result<i32, FrontendError> {
        let object = value.as_object().filter(|o| o.len() == 1)
            .ok_or_else(|| FrontendError::new(path, "anchor requires one coordinate field"))?;
        let (kind, value) = object.iter().next().unwrap();
        let value = integer(value, path)?;
        match kind.as_str() {
            "absolute" => Some(value),
            "above_bottom" => self.min_y.checked_add(value),
            "below_top" => (self.min_y + (self.height - 1)).checked_sub(value),
            _ => return Err(FrontendError::new(path, "unknown anchor coordinate field")),
        }.ok_or_else(|| FrontendError::new(path, "resolved anchor overflows i32"))
    }

    fn density(&mut self, value: &Value, path: &str) -> Result<MaterialDensityId, FrontendError> {
        let value = if let Some(name) = value.as_str() { Value::String(resource_id(name, path)?) }
            else if value.is_number() || value.is_object() { value.clone() }
            else { return Err(FrontendError::new(path, "density requires a holder or document")); };
        let index = self.densities.iter().position(|root| root == &value).unwrap_or_else(|| {
            let index = self.densities.len(); self.densities.push(value); index
        });
        Ok(MaterialDensityId(index))
    }
}

fn intern(names: &mut Vec<String>, name: String) -> usize {
    names.iter().position(|candidate| candidate == &name).unwrap_or_else(|| {
        let index = names.len(); names.push(name); index
    })
}

fn kind(value: &Value, path: &str) -> Result<String, FrontendError> {
    resource_id(string(&value["type"], path)?, path)
}

fn string<'a>(value: &'a Value, path: &str) -> Result<&'a str, FrontendError> {
    value.as_str().ok_or_else(|| FrontendError::new(path, "expected string"))
}

fn boolean(value: &Value, path: &str) -> Result<bool, FrontendError> {
    value.as_bool().ok_or_else(|| FrontendError::new(path, "expected boolean"))
}

fn integer(value: &Value, path: &str) -> Result<i32, FrontendError> {
    value.as_i64().and_then(|v| i32::try_from(v).ok()).ok_or_else(|| FrontendError::new(path, "expected i32 integer"))
}

fn multiplier(value: &Value, path: &str) -> Result<i32, FrontendError> {
    let value = integer(value, path)?;
    if !(-20..=20).contains(&value) { return Err(FrontendError::new(path, "surface depth multiplier must be in [-20, 20]")); }
    Ok(value)
}

fn number(value: &Value, path: &str) -> Result<f64, FrontendError> {
    value.as_f64().filter(|v| v.is_finite()).ok_or_else(|| FrontendError::new(path, "expected finite number"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Inputs {
        values: [f32; 3],
        draws: [f32; 3],
        trace: Vec<String>,
    }

    impl MaterialInputs for Inputs {
        fn condition(&mut self, _: usize, condition: &MaterialCondition) -> bool {
            self.trace.push("condition".to_owned());
            matches!(condition, MaterialCondition::Hole)
        }
        fn bandlands(&mut self) -> StateId { panic!("unexpected band rule") }
        fn density(&mut self, id: MaterialDensityId, sampling: MaterialSampling) -> f32 {
            self.trace.push(format!("density{}:{sampling:?}", id.0));
            self.values[id.0]
        }
        fn random_float(&mut self, _: MaterialRandomId, ordinal: u8) -> f32 {
            self.trace.push(format!("draw{ordinal}"));
            self.draws[ordinal as usize]
        }
    }

    fn ore() -> Value {
        json!({"type":"minecraft:ore_vein", "density":"test:density", "richness":"test:richness",
            "filler_gap":"test:gap", "raw_ore_chance":0.02, "ore_block":"minecraft:copper_ore",
            "raw_ore_block":"minecraft:raw_copper_block", "filler_block":"minecraft:granite"})
    }

    fn bake(value: &Value) -> BakedMaterial {
        MaterialBaker::new(&|_| None, &|_| None, -64, 384).unwrap().bake_holder(value).unwrap()
    }

    #[test]
    fn optional_ore_preserves_fallback_and_exact_demand_order() {
        let baked = bake(&json!({"type":"minecraft:sequence", "sequence":[ore(),
            {"type":"minecraft:block", "result_state":"minecraft:dirt"}]}));
        let mut inputs = Inputs { values: [0.0, 0.3, -0.125], draws: [0.6, 0.2, 0.019], trace: Vec::new() };
        assert_eq!(baked.graph.apply(&mut inputs), Some(StateId::from_state_str("minecraft:dirt").unwrap()));
        assert_eq!(inputs.trace, ["density0:VolumeOrScalar"]);
        inputs.values[0] = 0.7;
        inputs.trace.clear();
        assert_eq!(baked.graph.apply(&mut inputs), Some(StateId::from_state_str("minecraft:raw_copper_block").unwrap()));
        assert_eq!(inputs.trace, ["density0:VolumeOrScalar", "draw0", "density1:VolumeOrScalar",
            "draw1", "density2:Scalar", "draw2"]);
        inputs.values[2] = 0.0;
        inputs.trace.clear();
        assert_eq!(baked.graph.apply(&mut inputs), Some(StateId::from_state_str("minecraft:granite").unwrap()));
        assert_eq!(inputs.trace.len(), 5);
        assert!(!inputs.trace.iter().any(|call| call == "draw2"));
    }

    #[test]
    fn first_result_and_strict_random_boundaries_are_independent_of_later_rules() {
        let baked = bake(&json!({"type":"minecraft:sequence", "sequence":[
            {"type":"minecraft:block", "result_state":"minecraft:bedrock"}, ore()]}));
        let mut inputs = Inputs { values: [0.7, 0.3, -0.125], draws: [0.7, 0.2, 0.02], trace: Vec::new() };
        assert_eq!(baked.graph.apply(&mut inputs), Some(StateId::from_state_str("minecraft:bedrock").unwrap()));
        assert!(inputs.trace.is_empty());
        let ore = bake(&ore());
        assert_eq!(ore.graph.apply(&mut inputs), Some(StateId::from_state_str("minecraft:copper_ore").unwrap()));
        inputs.draws[0] = 0.71;
        inputs.trace.clear();
        assert_eq!(ore.graph.apply(&mut inputs), None);
        assert_eq!(inputs.trace, ["density0:VolumeOrScalar", "draw0"]);
    }

    #[test]
    fn holders_keep_rule_condition_and_state_domains_distinct() {
        let rules = |id: &str| match id {
            "test:root" => Some(json!({"type":"minecraft:condition", "if_true":"test:gate",
                "then_run":{"type":"minecraft:block", "result_state":"minecraft:deepslate"}})),
            "test:cycle" => Some(json!({"type":"minecraft:sequence", "sequence":["test:cycle"]})),
            _ => None,
        };
        let conditions = |id: &str| (id == "test:gate").then(|| json!({"type":"minecraft:hole"}));
        let baked = MaterialBaker::new(&rules, &conditions, -64, 384).unwrap().bake_holder(&json!("test:root")).unwrap();
        let mut inputs = Inputs { values: [0.0; 3], draws: [0.0; 3], trace: Vec::new() };
        let state = baked.graph.apply(&mut inputs).unwrap();
        assert!(state.is_default());
        assert_eq!(state.name(), "minecraft:deepslate");
        assert_eq!(inputs.trace, ["condition"]);
        let error = MaterialBaker::new(&rules, &conditions, -64, 384).unwrap().bake_holder(&json!("test:cycle")).unwrap_err();
        assert!(error.reason.contains("cycle"));
        assert!(MaterialBaker::new(&rules, &conditions, -64, 384).unwrap().bake_holder(&json!("test:missing")).is_err());
    }

    #[test]
    fn typed_conditions_keep_double_thresholds_and_only_documented_defaults() {
        let baked = bake(&json!({"type":"minecraft:condition", "if_true":{
            "type":"minecraft:noise_threshold", "noise":"minecraft:surface",
            "min_threshold":0.400000003, "max_threshold":0.400000003},
            "then_run":{"type":"minecraft:block", "result_state":"minecraft:stone"}}));
        let MaterialCondition::NoiseThreshold { min, max, is_3d, .. } = baked.graph.conditions()[0] else { panic!("expected noise predicate") };
        assert!(!is_3d);
        let sample = f64::from(0.4_f32);
        assert!(!(sample >= min && sample <= max));
        assert!(0.4_f32 >= min as f32 && 0.4_f32 <= max as f32);
        let noise = 0.3636363446712494_f32;
        assert_eq!((f64::from(noise) * 2.75 + 3.0) as i32, 3);
        assert_eq!((noise * 2.75 + 3.0) as i32, 4);
        let bad = json!({"type":"minecraft:condition", "if_true":{"type":"minecraft:stone_depth",
            "offset":2147483648_i64, "add_surface_depth":false, "secondary_depth_range":0, "surface_type":"floor"},
            "then_run":{"type":"minecraft:block", "result_state":"minecraft:stone"}});
        assert!(MaterialBaker::new(&|_| None, &|_| None, -64, 384).unwrap().bake_holder(&bad).is_err());
    }
}
