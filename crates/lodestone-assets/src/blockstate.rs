//! Blockstate JSON parsing ([`BlockStates`]).
//!
//! A blockstate file maps a block's property combinations to the models that
//! render them. It comes in two shapes: `variants` (a property-string key mapped
//! to one model or a weighted list) and `multipart` (a list of conditional
//! cases). This module parses both into strongly typed values; selecting and
//! resolving the referenced [`crate::model`] happens on top of these types.

use crate::error::BlockStateError;
use crate::location::ResourceLocation;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The closed top-level shape of a blockstate document.
///
/// The public API deliberately lowers this transport model into
/// [`BlockStateDefinition`], so the rest of the asset pipeline never has to
/// inspect JSON. `variants` wins when a malformed pack supplies both keys,
/// matching the parser's historical precedence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockStateDocument {
    variants: Option<BTreeMap<String, ModelRefsDocument>>,
    multipart: Option<Vec<MultipartCaseDocument>>,
}

/// A variant or multipart `apply` value: one model object or a weighted list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum ModelRefsDocument {
    One(ModelRefDocument),
    Many(Vec<ModelRefDocument>),
}

/// The closed shape of one model reference in a blockstate document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelRefDocument {
    model: String,
    #[serde(default)]
    x: i32,
    #[serde(default)]
    y: i32,
    #[serde(default)]
    uvlock: bool,
    #[serde(default = "default_weight")]
    weight: u32,
}

fn default_weight() -> u32 {
    1
}

/// The closed shape of a multipart case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MultipartCaseDocument {
    #[serde(default)]
    when: Option<WhenDocument>,
    apply: ModelRefsDocument,
}

/// A typed recursive `when` document.
///
/// Property names are intentionally open because blockstate properties come
/// from the block definition rather than this file. Their values are still
/// closed to the three scalar forms accepted by the resource-pack format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct WhenDocument {
    #[serde(rename = "OR", default)]
    or: Option<Vec<WhenDocument>>,
    #[serde(rename = "AND", default)]
    and: Option<Vec<WhenDocument>>,
    #[serde(flatten)]
    properties: BTreeMap<String, PropertyValueDocument>,
}

/// A scalar blockstate property value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum PropertyValueDocument {
    String(String),
    Bool(bool),
    Number(serde_json::Number),
}

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
        let document: BlockStateDocument =
            serde_json::from_slice(bytes).map_err(|e| BlockStateError::Json(e.to_string()))?;

        if let Some(variants) = document.variants {
            let mut out = Vec::with_capacity(variants.len());
            for (key, value) in variants {
                out.push((key, model_refs_from_document(value)?));
            }
            Ok(Self {
                definition: BlockStateDefinition::Variants(out),
            })
        } else if let Some(multipart) = document.multipart {
            let mut cases = Vec::with_capacity(multipart.len());
            for case in multipart {
                cases.push(multipart_case_from_document(case)?);
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

/// Lowers either a single model object or an array of them into a weighted list.
fn model_refs_from_document(value: ModelRefsDocument) -> Result<Vec<ModelRef>, BlockStateError> {
    match value {
        ModelRefsDocument::One(model) => Ok(vec![model_ref_from_document(model)?]),
        ModelRefsDocument::Many(models) => models
            .into_iter()
            .map(model_ref_from_document)
            .collect(),
    }
}

/// Lowers a single typed model reference into the public representation.
fn model_ref_from_document(value: ModelRefDocument) -> Result<ModelRef, BlockStateError> {
    let model = ResourceLocation::parse(&value.model)?;
    Ok(ModelRef {
        model,
        x: value.x,
        y: value.y,
        uvlock: value.uvlock,
        weight: value.weight,
    })
}

/// Lowers a single multipart case (`{when?, apply}`).
fn multipart_case_from_document(
    value: MultipartCaseDocument,
) -> Result<MultipartCase, BlockStateError> {
    let apply = model_refs_from_document(value.apply)?;
    let when = value.when.map(when_from_document).transpose()?;
    Ok(MultipartCase { when, apply })
}

/// Lowers a typed `when` object into a [`When`] tree.
fn when_from_document(value: WhenDocument) -> Result<When, BlockStateError> {
    if let Some(or) = value.or {
        return Ok(When::Or(
            or.into_iter().map(when_from_document).collect::<Result<_, _>>()?,
        ));
    }
    if let Some(and) = value.and {
        return Ok(When::And(
            and.into_iter().map(when_from_document).collect::<Result<_, _>>()?,
        ));
    }

    // Otherwise an implicit AND of property predicates.
    let mut predicates = Vec::with_capacity(value.properties.len());
    for (property, raw) in value.properties {
        let as_string = property_value_to_string(raw);
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

/// Coerces a typed `when` predicate value (string, bool, or number) to a string.
fn property_value_to_string(value: PropertyValueDocument) -> String {
    match value {
        PropertyValueDocument::String(s) => s,
        PropertyValueDocument::Bool(b) => b.to_string(),
        PropertyValueDocument::Number(n) => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockStateDocument, PropertyValueDocument};

    #[test]
    fn typed_document_round_trips_the_recursive_schema() {
        let source = r#"{
            "multipart":[
                {"apply":{"model":"minecraft:block/stone"}},
                {"when":{"OR":[{"north":"true"},{"south":false}]},"apply":[
                    {"model":"minecraft:block/oak_fence","weight":2}
                ]}
            ]
        }"#;
        let document: BlockStateDocument = serde_json::from_str(source).unwrap();
        let encoded = serde_json::to_string(&document).unwrap();
        let decoded: BlockStateDocument = serde_json::from_str(&encoded).unwrap();
        assert_eq!(document, decoded);
        assert!(matches!(
            decoded
                .multipart
                .as_ref()
                .and_then(|cases| cases.get(1))
                .and_then(|case| case.when.as_ref())
                .and_then(|when| when.properties.get("south")),
            None | Some(PropertyValueDocument::Bool(false))
        ));
    }
}
