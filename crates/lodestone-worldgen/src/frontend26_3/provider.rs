use std::collections::{BTreeMap, HashMap};

use lodestone_data::block_states::StateId;
use serde_json::Value;

use crate::feature::IntProvider;
use crate::feature::vegetation::{BlockPredicate, BlockStateProvider};
use crate::feature::vegetation::ids::Tag;

use super::{FrontendError, resource_id};
use super::state::parse_state_at;

const MAX_DEPTH: usize = 128;
const MAX_NODES: usize = 65_536;

/// Resolves one immutable provider registry at load time. The lookup receives
/// canonical namespaced resource ids and returns the direct resource document.
/// Successfully resolved resources are cached as typed providers, never JSON.
pub struct ProviderBaker<'a> {
    lookup: &'a dyn Fn(&str) -> Option<Value>,
    ready: HashMap<String, (BlockStateProvider, usize, usize)>,
    active: Vec<String>,
    nodes: usize,
}

impl std::fmt::Debug for ProviderBaker<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderBaker").field("cached_resources", &self.ready.len())
            .field("active_references", &self.active.len()).field("visited_nodes", &self.nodes)
            .finish_non_exhaustive()
    }
}

impl<'a> ProviderBaker<'a> {
    pub fn new(lookup: &'a dyn Fn(&str) -> Option<Value>) -> Self {
        Self { lookup, ready: HashMap::new(), active: Vec::new(), nodes: 0 }
    }

    /// Parses a holder: strings name registry resources; objects are inline.
    pub fn bake_holder(&mut self, value: &Value) -> Result<BlockStateProvider, FrontendError> {
        self.nodes = 0;
        self.holder(value, "provider", 0)
    }

    /// Parses a registry entry: strings here are compact block states.
    pub fn bake_direct(&mut self, value: &Value) -> Result<BlockStateProvider, FrontendError> {
        self.nodes = 0;
        self.direct(value, "provider", 0)
    }

    fn visit(&mut self, path: &str, depth: usize) -> Result<(), FrontendError> {
        self.nodes += 1;
        if depth > MAX_DEPTH || self.nodes > MAX_NODES {
            return Err(FrontendError::new(path, "provider graph exceeds depth or node budget"));
        }
        Ok(())
    }

    fn holder(&mut self, value: &Value, path: &str, depth: usize) -> Result<BlockStateProvider, FrontendError> {
        self.visit(path, depth)?;
        let Some(raw_id) = value.as_str() else { return self.direct(value, path, depth + 1) };
        let id = resource_id(raw_id, path)?;
        if self.active.contains(&id) {
            return Err(FrontendError::new(path, format!("provider reference cycle through {id}")));
        }
        if let Some((provider, cost, height)) = self.ready.get(&id) {
            self.nodes = self.nodes.saturating_add(*cost);
            if self.nodes > MAX_NODES || depth + height > MAX_DEPTH {
                return Err(FrontendError::new(path, "expanded provider graph exceeds depth or node budget"));
            }
            return Ok(provider.clone());
        }
        let doc = (self.lookup)(&id)
            .ok_or_else(|| FrontendError::new(path, format!("unresolved block_state_provider {id}")))?;
        self.active.push(id.clone());
        let before = self.nodes;
        let result = self.direct(&doc, &format!("block_state_provider/{id}"), depth + 1);
        self.active.pop();
        let provider = result?;
        let height = provider_height(&provider);
        self.ready.insert(id, (provider.clone(), self.nodes - before, height));
        Ok(provider)
    }

    fn direct(&mut self, value: &Value, path: &str, depth: usize) -> Result<BlockStateProvider, FrontendError> {
        self.visit(path, depth)?;
        if value.is_string() || value.get("id").is_some() {
            return parse_state_at(value, path).map(BlockStateProvider::Simple);
        }
        let ty = value.get("type").and_then(Value::as_str)
            .ok_or_else(|| FrontendError::new(path, "provider requires a type or inline state"))?;
        let ty = resource_id(ty, path)?;
        match ty.as_str() {
            "minecraft:simple" => parse_state_at(&value["state"], path).map(BlockStateProvider::Simple),
            "minecraft:weighted" => {
                let entries = value["entries"].as_array()
                    .ok_or_else(|| FrontendError::new(path, "weighted provider requires entries"))?;
                let mut total = 0_i32;
                let mut out = Vec::with_capacity(entries.len());
                for (index, entry) in entries.iter().enumerate() {
                    let at = format!("{path}.entries[{index}]");
                    let weight = match entry.get("weight") {
                        None => 1,
                        Some(value) => integer(value, &at)?,
                    };
                    if weight < 0 {
                        return Err(FrontendError::new(&at, "weight must be nonnegative"));
                    }
                    total = total.checked_add(weight)
                        .ok_or_else(|| FrontendError::new(&at, "weight sum exceeds i32"))?;
                    out.push((weight, parse_state_at(&entry["data"], &at)?));
                }
                if total == 0 {
                    return Err(FrontendError::new(path, "weighted provider needs positive total weight"));
                }
                Ok(BlockStateProvider::Weighted(out))
            }
            "minecraft:randomized_int" => {
                let source = self.holder(&value["source"], &format!("{path}.source"), depth + 1)?;
                let BlockStateProvider::Weighted(entries) = source else {
                    return Err(FrontendError::new(path,
                        "randomized_int source requires direct-source RNG preservation in executor"));
                };
                let property = value["property"].as_str()
                    .ok_or_else(|| FrontendError::new(path, "randomized_int requires property"))?;
                let (values, min, max) = int_range(&value["values"], path)?;
                let mut source = Vec::with_capacity(entries.len());
                for (weight, state) in entries {
                    let mut properties: BTreeMap<String, String> = state.properties().iter()
                        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect();
                    if !properties.contains_key(property) {
                        return Err(FrontendError::new(path, format!("unknown property {}.{property}", state.name())));
                    }
                    let mut states = Vec::new();
                    for number in min..=max {
                        self.visit(path, depth + 1)?;
                        properties.insert(property.to_owned(), number.to_string());
                        states.push(StateId::from_exact_parts(state.name(), &properties)
                            .ok_or_else(|| FrontendError::new(path, format!("invalid {property} value {number}")))?);
                    }
                    source.push((weight, states));
                }
                Ok(BlockStateProvider::RandomizedInt { source, values })
            }
            "minecraft:rule_based" => {
                let entries = value["rules"].as_array()
                    .ok_or_else(|| FrontendError::new(path, "rule_based requires rules"))?;
                let mut rules = Vec::with_capacity(entries.len());
                for (index, rule) in entries.iter().enumerate() {
                    let at = format!("{path}.rules[{index}]");
                    let predicate = self.predicate(&rule["if_true"], &at, depth + 1)?;
                    let then = self.holder(&rule["then"], &format!("{at}.then"), depth + 1)?;
                    if nullable(&then) {
                        return Err(FrontendError::new(&at,
                            "nullable rule branch requires continue-after-empty execution"));
                    }
                    rules.push((predicate, Box::new(then)));
                }
                let fallback = value.get("fallback")
                    .map(|v| self.holder(v, &format!("{path}.fallback"), depth + 1).map(Box::new)).transpose()?;
                Ok(BlockStateProvider::RuleBased { rules, fallback })
            }
            other => Err(FrontendError::new(path, format!("unsupported block_state_provider discriminator {other}"))),
        }
    }

    fn predicate(&mut self, value: &Value, path: &str, depth: usize) -> Result<BlockPredicate, FrontendError> {
        self.visit(path, depth)?;
        let ty = value["type"].as_str()
            .ok_or_else(|| FrontendError::new(path, "predicate requires type"))?;
        let ty = resource_id(ty, path)?;
        match ty.as_str() {
            "minecraft:true" => Ok(BlockPredicate::True),
            "minecraft:not" => self.predicate(&value["predicate"], path, depth + 1)
                .map(|p| BlockPredicate::Not(Box::new(p))),
            "minecraft:all_of" | "minecraft:any_of" => {
                let values = value["predicates"].as_array()
                    .ok_or_else(|| FrontendError::new(path, "predicate requires predicates array"))?;
                let predicates = values.iter().map(|v| self.predicate(v, path, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(if ty == "minecraft:all_of" { BlockPredicate::AllOf(predicates) }
                    else { BlockPredicate::AnyOf(predicates) })
            }
            "minecraft:matching_block_tag" => {
                let id = value["tag"].as_str()
                    .ok_or_else(|| FrontendError::new(path, "tag predicate requires tag"))?;
                let id = resource_id(id, path)?;
                let tag = match id.as_str() {
                    "minecraft:cannot_replace_below_tree_trunk" => Tag::CannotReplaceBelowTreeTrunk,
                    "minecraft:beneath_tree_podzol_replaceable" => Tag::BeneathTreePodzolReplaceable,
                    _ => return Err(FrontendError::new(path, format!("unsupported provider predicate tag {id}"))),
                };
                let offset = match value.get("offset") {
                    None => (0, 0, 0),
                    Some(v) => {
                        let a = v.as_array().filter(|a| a.len() == 3)
                            .ok_or_else(|| FrontendError::new(path, "offset must contain three integers"))?;
                        (integer(&a[0], path)?, integer(&a[1], path)?, integer(&a[2], path)?)
                    }
                };
                Ok(BlockPredicate::MatchingBlockTag { tag: Some(tag), offset })
            }
            _ => Err(FrontendError::new(path, format!("unsupported provider predicate discriminator {ty}"))),
        }
    }
}

fn nullable(provider: &BlockStateProvider) -> bool {
    match provider {
        BlockStateProvider::RuleBased { fallback, .. } => fallback.as_ref().is_none_or(|p| nullable(p)),
        _ => false,
    }
}

fn provider_height(provider: &BlockStateProvider) -> usize {
    match provider {
        BlockStateProvider::RuleBased { rules, fallback } => 1 + rules.iter()
            .map(|(predicate, provider)| predicate_height(predicate).max(provider_height(provider)))
            .chain(fallback.iter().map(|p| provider_height(p)))
            .max().unwrap_or(0),
        _ => 1,
    }
}

fn predicate_height(predicate: &BlockPredicate) -> usize {
    match predicate {
        BlockPredicate::Not(inner) => 1 + predicate_height(inner),
        BlockPredicate::AllOf(children) | BlockPredicate::AnyOf(children) => {
            1 + children.iter().map(predicate_height).max().unwrap_or(0)
        }
        _ => 1,
    }
}

fn integer(value: &Value, path: &str) -> Result<i32, FrontendError> {
    value.as_i64().and_then(|n| i32::try_from(n).ok())
        .ok_or_else(|| FrontendError::new(path, "expected i32 integer"))
}

fn int_range(value: &Value, path: &str) -> Result<(IntProvider, i32, i32), FrontendError> {
    if value.is_number() {
        let number = integer(value, path)?;
        return Ok((IntProvider::Constant(number), number, number));
    }
    let ty = value["type"].as_str()
        .ok_or_else(|| FrontendError::new(path, "integer provider requires type"))?;
    if resource_id(ty, path)? != "minecraft:uniform" {
        return Err(FrontendError::new(path, format!("unsupported randomized_int values discriminator {ty}")));
    }
    let min = integer(&value["min_inclusive"], path)?;
    let max = integer(&value["max_inclusive"], path)?;
    if min > max || i64::from(max) - i64::from(min) >= MAX_NODES as i64 {
        return Err(FrontendError::new(path, "invalid or excessive integer provider range"));
    }
    Ok((IntProvider::Uniform { min, max }, min, max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn holders_and_compact_states_have_distinct_domains() {
        let lookup = |id: &str| (id == "minecraft:stone").then(|| json!({"id":"dirt"}));
        let mut baker = ProviderBaker::new(&lookup);
        let BlockStateProvider::Simple(holder) = baker.bake_holder(&json!("stone")).unwrap() else { panic!() };
        let BlockStateProvider::Simple(direct) = baker.bake_direct(&json!("stone")).unwrap() else { panic!() };
        assert_eq!(holder.name(), "minecraft:dirt");
        assert_eq!(direct.name(), "minecraft:stone");
        assert!(baker.bake_holder(&json!("absent")).unwrap_err().reason.contains("unresolved"));
    }

    #[test]
    fn cycle_and_unsupported_discriminators_are_errors() {
        let lookup = |_: &str| Some(json!({"type":"rule_based", "rules":[], "fallback":"loop"}));
        let mut baker = ProviderBaker::new(&lookup);
        assert!(baker.bake_holder(&json!("loop")).unwrap_err().reason.contains("cycle"));
        assert!(baker.bake_direct(&json!({"type":"copy_properties"})).unwrap_err().reason.contains("copy_properties"));
        assert!(baker.bake_direct(&json!({"type":"weighted", "entries":[]})).is_err());
    }

    #[test]
    fn nullable_rule_children_and_simple_random_sources_are_not_silently_reinterpreted() {
        let mut baker = ProviderBaker::new(&|_| None);
        let nested = json!({"type":"rule_based", "rules":[
            {"if_true":{"type":"true"}, "then":{"type":"rule_based", "rules":[]}}],
            "fallback":{"id":"stone"}});
        assert!(baker.bake_direct(&nested).unwrap_err().reason.contains("continue-after-empty"));
        let random = json!({"type":"randomized_int", "property":"age", "source":{"id":"cave_vines"}, "values":23});
        assert!(baker.bake_direct(&random).unwrap_err().reason.contains("RNG preservation"));
    }

    #[test]
    fn nesting_limits_are_checked_before_recursion() {
        let mut baker = ProviderBaker::new(&|_| None);
        let mut value = json!({"id":"stone"});
        for _ in 0..MAX_DEPTH {
            value = json!({"type":"rule_based", "rules":[], "fallback":value});
        }
        assert!(baker.bake_direct(&value).unwrap_err().reason.contains("budget"));
    }

    #[test]
    fn weighted_states_and_property_tables_are_baked_in_declaration_order() {
        let mut baker = ProviderBaker::new(&|_| None);
        let value = json!({"type":"randomized_int", "property":"age",
            "source":{"type":"weighted", "entries":[
                {"weight":4, "data":"cave_vines"},
                {"weight":1, "data":{"id":"cave_vines", "properties":{"berries":"true"}}}]},
            "values":{"type":"uniform", "min_inclusive":23, "max_inclusive":25}});
        let BlockStateProvider::RandomizedInt { source, .. } = baker.bake_direct(&value).unwrap() else { panic!() };
        assert_eq!(source.iter().map(|(weight, _)| *weight).collect::<Vec<_>>(), [4, 1]);
        for (index, (_, states)) in source.iter().enumerate() {
            assert_eq!(states.len(), 3);
            for (offset, state) in states.iter().enumerate() {
                let expected = (23 + offset).to_string();
                assert!(state.properties().contains(&("age", expected.as_str())));
                assert!(state.properties().contains(&("berries", if index == 0 { "false" } else { "true" })));
            }
        }
    }
}
