//! Blockstate JSON parsing ([`BlockStates`]).
//!
//! A blockstate file maps a block's property combinations to the models that
//! render them. It comes in two shapes: `variants` (a property-string key mapped
//! to one model or a weighted list) and `multipart` (a list of conditional
//! cases). This module parses both into strongly typed values; selecting and
//! resolving the referenced [`crate::model`] happens on top of these types.

use crate::error::BlockStateError;
use crate::location::ResourceLocation;
use serde_json::Value;
use std::collections::BTreeMap;

/// A reference to a model, with optional rotation, uv-lock, and random weight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    /// The referenced model, for example `minecraft:block/stone`.
    pub model: ResourceLocation,
    /// Rotation around the X axis in degrees (`0`, `90`, `180`, `270`).
    pub x: i32,
    /// Rotation around the Y axis in degrees.
    pub y: i32,
    /// Whether texture UVs are locked against the rotation.
    pub uvlock: bool,
    /// Relative weight when picking randomly from a list (default `1`).
    pub weight: u32,
}

/// The two shapes a blockstate definition can take.
#[derive(Debug, Clone)]
pub enum BlockStateDefinition {
    /// A map (kept in file order) of property-string key to a weighted list of
    /// candidate models.
    Variants(Vec<(String, Vec<ModelRef>)>),
    /// A list of conditional cases, each contributing models when its condition
    /// holds.
    Multipart(Vec<MultipartCase>),
}

/// A single `multipart` case.
#[derive(Debug, Clone)]
pub struct MultipartCase {
    /// The condition under which this case applies; `None` means "always".
    pub when: Option<When>,
    /// The models this case contributes (a weighted list).
    pub apply: Vec<ModelRef>,
}

/// A multipart `when` condition.
///
/// Multiple properties in a single `when` object are ANDed. The explicit `OR`
/// and `AND` keys combine sub-conditions, and a single property value may list
/// `|`-separated alternatives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum When {
    /// A property must equal one of the listed values.
    Match {
        /// The property name.
        property: String,
        /// The acceptable values (from `|`-separated alternatives).
        values: Vec<String>,
    },
    /// All sub-conditions must hold.
    And(Vec<When>),
    /// At least one sub-condition must hold.
    Or(Vec<When>),
}

impl When {
    /// Evaluates the condition against a set of property values.
    pub fn matches(&self, props: &BTreeMap<String, String>) -> bool {
        match self {
            When::Match { property, values } => props
                .get(property)
                .is_some_and(|actual| values.iter().any(|v| v == actual)),
            When::And(children) => children.iter().all(|c| c.matches(props)),
            When::Or(children) => children.iter().any(|c| c.matches(props)),
        }
    }
}

/// Parsed blockstate file.
#[derive(Debug, Clone)]
pub struct BlockStates {
    /// The definition (variants or multipart).
    pub definition: BlockStateDefinition,
}

impl BlockStates {
    /// Parses blockstate JSON.
    pub fn parse(bytes: &[u8]) -> Result<Self, BlockStateError> {
        let root: Value =
            serde_json::from_slice(bytes).map_err(|e| BlockStateError::Json(e.to_string()))?;
        let obj = root.as_object().ok_or(BlockStateError::MissingDefinition)?;

        if let Some(variants) = obj.get("variants") {
            let map = variants
                .as_object()
                .ok_or_else(|| BlockStateError::InvalidField {
                    field: "variants",
                    reason: "expected an object".to_string(),
                })?;
            let mut out = Vec::with_capacity(map.len());
            for (key, value) in map {
                out.push((key.clone(), parse_model_refs(value)?));
            }
            Ok(Self {
                definition: BlockStateDefinition::Variants(out),
            })
        } else if let Some(multipart) = obj.get("multipart") {
            let list = multipart
                .as_array()
                .ok_or_else(|| BlockStateError::InvalidField {
                    field: "multipart",
                    reason: "expected an array".to_string(),
                })?;
            let mut cases = Vec::with_capacity(list.len());
            for case in list {
                cases.push(parse_multipart_case(case)?);
            }
            Ok(Self {
                definition: BlockStateDefinition::Multipart(cases),
            })
        } else {
            Err(BlockStateError::MissingDefinition)
        }
    }

    /// Selects the models for the variant matching `props`, or `None` if no key
    /// matches. Only meaningful for [`BlockStateDefinition::Variants`].
    pub fn select_variant(&self, props: &BTreeMap<String, String>) -> Option<&[ModelRef]> {
        let BlockStateDefinition::Variants(variants) = &self.definition else {
            return None;
        };
        variants
            .iter()
            .find(|(key, _)| variant_key_matches(key, props))
            .map(|(_, models)| models.as_slice())
    }

    /// Selects the model groups that apply to a set of property values.
    ///
    /// For [`BlockStateDefinition::Variants`] this is at most one group — the
    /// weighted candidate list of the single matching variant key (or nothing
    /// if no key matches). For [`BlockStateDefinition::Multipart`] it is one
    /// group per case whose `when` condition holds (a case without `when`
    /// always applies), unioned in file order. Each returned group is a weighted
    /// candidate list from which exactly one model should be chosen at bake
    /// time.
    pub fn applicable_models(&self, props: &BTreeMap<String, String>) -> Vec<&[ModelRef]> {
        match &self.definition {
            BlockStateDefinition::Variants(variants) => variants
                .iter()
                .find(|(key, _)| variant_key_matches(key, props))
                .map(|(_, models)| vec![models.as_slice()])
                .unwrap_or_default(),
            BlockStateDefinition::Multipart(cases) => cases
                .iter()
                .filter(|case| case.when.as_ref().is_none_or(|w| w.matches(props)))
                .map(|case| case.apply.as_slice())
                .collect(),
        }
    }

    /// Iterates every model referenced anywhere in this blockstate.
    pub fn model_refs(&self) -> impl Iterator<Item = &ModelRef> {
        let iter: Box<dyn Iterator<Item = &ModelRef>> = match &self.definition {
            BlockStateDefinition::Variants(variants) => {
                Box::new(variants.iter().flat_map(|(_, m)| m.iter()))
            }
            BlockStateDefinition::Multipart(cases) => {
                Box::new(cases.iter().flat_map(|c| c.apply.iter()))
            }
        };
        iter
    }
}

/// Parses a variant key such as `facing=north,half=top` into a property map.
/// The empty key yields an empty map (it matches every state).
pub fn parse_variant_key(key: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if key.is_empty() {
        return map;
    }
    for pair in key.split(',') {
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

/// Whether every constraint in a variant key is satisfied by `props`.
fn variant_key_matches(key: &str, props: &BTreeMap<String, String>) -> bool {
    parse_variant_key(key)
        .iter()
        .all(|(k, v)| props.get(k) == Some(v))
}

/// Parses either a single model object or an array of them into a weighted list.
fn parse_model_refs(value: &Value) -> Result<Vec<ModelRef>, BlockStateError> {
    match value {
        Value::Array(items) => items.iter().map(parse_model_ref).collect(),
        Value::Object(_) => Ok(vec![parse_model_ref(value)?]),
        _ => Err(BlockStateError::InvalidField {
            field: "model",
            reason: "expected an object or array".to_string(),
        }),
    }
}

/// Parses a single model reference object.
fn parse_model_ref(value: &Value) -> Result<ModelRef, BlockStateError> {
    let obj = value
        .as_object()
        .ok_or_else(|| BlockStateError::InvalidField {
            field: "model",
            reason: "expected an object".to_string(),
        })?;
    let model_str =
        obj.get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| BlockStateError::InvalidField {
                field: "model",
                reason: "missing string \"model\"".to_string(),
            })?;
    let model = ResourceLocation::parse(model_str)?;
    let x = obj.get("x").and_then(Value::as_i64).unwrap_or(0) as i32;
    let y = obj.get("y").and_then(Value::as_i64).unwrap_or(0) as i32;
    let uvlock = obj.get("uvlock").and_then(Value::as_bool).unwrap_or(false);
    let weight = obj.get("weight").and_then(Value::as_u64).unwrap_or(1) as u32;
    Ok(ModelRef {
        model,
        x,
        y,
        uvlock,
        weight,
    })
}

/// Parses a single multipart case (`{when?, apply}`).
fn parse_multipart_case(value: &Value) -> Result<MultipartCase, BlockStateError> {
    let obj = value
        .as_object()
        .ok_or_else(|| BlockStateError::InvalidField {
            field: "multipart",
            reason: "case must be an object".to_string(),
        })?;
    let apply = obj
        .get("apply")
        .ok_or_else(|| BlockStateError::InvalidField {
            field: "apply",
            reason: "missing".to_string(),
        })?;
    let apply = parse_model_refs(apply)?;
    let when = obj.get("when").map(parse_when).transpose()?;
    Ok(MultipartCase { when, apply })
}

/// Parses a `when` object into a [`When`] tree.
fn parse_when(value: &Value) -> Result<When, BlockStateError> {
    let obj = value
        .as_object()
        .ok_or_else(|| BlockStateError::InvalidField {
            field: "when",
            reason: "expected an object".to_string(),
        })?;

    let parse_list = |v: &Value| -> Result<Vec<When>, BlockStateError> {
        v.as_array()
            .ok_or_else(|| BlockStateError::InvalidField {
                field: "when",
                reason: "OR/AND must be an array".to_string(),
            })?
            .iter()
            .map(parse_when)
            .collect()
    };

    if let Some(or) = obj.get("OR") {
        return Ok(When::Or(parse_list(or)?));
    }
    if let Some(and) = obj.get("AND") {
        return Ok(When::And(parse_list(and)?));
    }

    // Otherwise an implicit AND of property predicates.
    let mut predicates = Vec::with_capacity(obj.len());
    for (property, raw) in obj {
        let as_string = value_to_property_string(raw)?;
        let values = as_string.split('|').map(str::to_string).collect();
        predicates.push(When::Match {
            property: property.clone(),
            values,
        });
    }
    match predicates.len() {
        0 => Err(BlockStateError::InvalidField {
            field: "when",
            reason: "empty condition".to_string(),
        }),
        1 => Ok(predicates.pop().unwrap()),
        _ => Ok(When::And(predicates)),
    }
}

/// Coerces a `when` predicate value (string, bool, or number) to a string.
fn value_to_property_string(value: &Value) -> Result<String, BlockStateError> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err(BlockStateError::InvalidField {
            field: "when",
            reason: "property value must be a string, bool, or number".to_string(),
        }),
    }
}
