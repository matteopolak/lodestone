//! The 1.21.4+ item definition model (`assets/<ns>/items/<id>.json`).
//!
//! Since 1.21.4 an item no longer names a model directly through the old
//! `overrides`/`predicate` list on the model JSON. Instead each item has a small
//! *definition* file whose `model` field is a recursive tree of selectors —
//! `condition`, `select`, `range_dispatch`, `composite` — bottoming out in
//! `model` leaves (a concrete block/item model to render) or `special` nodes (a
//! code-driven renderer such as chests, shields, or player heads).
//!
//! This module is the **data** half of that seam: it parses the tree and lets a
//! caller (a) enumerate every model a stack could resolve to
//! ([`ItemModel::model_refs`]) so the atlas/baker knows what to build, and (b)
//! resolve the tree for a concrete stack via an [`ItemPropertyContext`] the
//! *game* supplies — because the property values (`using_item`, `broken`, dye
//! colours, block-state properties) are runtime state this GPU-free crate does
//! not own. Parsing never evaluates a predicate itself.
//!
//! Faithful to `net/minecraft/client/renderer/item` (`ItemModels`,
//! `RangeSelectItemModel`, `SelectItemModel`, `CompositeModel`) in the decompiled
//! 26.2 client. Unknown node types are preserved as [`ItemModelNode::Other`]
//! rather than rejected, so a newer pack's node cannot fail the whole parse.

use crate::error::ItemModelError;
use crate::location::ResourceLocation;
use serde_json::Value;

/// A parsed item definition: the root of its selector tree.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemModel {
    /// The root selector node (the file's top-level `model`).
    pub root: ItemModelNode,
}

/// One node of the item selector tree.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemModelNode {
    /// `minecraft:model` — a concrete model to render, with optional tint sources.
    Model {
        /// The model resource location (e.g. `minecraft:item/bow`).
        model: ResourceLocation,
        /// Tint sources applied to the model's layers (data; evaluated by the game).
        tints: Vec<TintSource>,
    },
    /// `minecraft:composite` — render every listed sub-model together.
    Composite {
        /// The sub-models, all rendered.
        models: Vec<ItemModelNode>,
    },
    /// `minecraft:condition` — a boolean predicate branch.
    Condition {
        /// The predicate property (e.g. `minecraft:using_item`).
        property: String,
        /// Optional `component` the predicate reads (e.g. `minecraft:lodestone_tracker`).
        component: Option<String>,
        /// Chosen when the predicate is true.
        on_true: Box<ItemModelNode>,
        /// Chosen when the predicate is false.
        on_false: Box<ItemModelNode>,
    },
    /// `minecraft:select` — a string-keyed branch.
    Select {
        /// The property producing the key (e.g. `minecraft:charge_type`).
        property: String,
        /// The cases, each matching one or more key strings.
        cases: Vec<SelectCase>,
        /// Chosen when no case matches.
        fallback: Option<Box<ItemModelNode>>,
    },
    /// `minecraft:range_dispatch` — a numeric-threshold branch.
    RangeDispatch {
        /// The property producing the value (e.g. `minecraft:use_duration`).
        property: String,
        /// Multiplier applied to the property value before comparison (default `1.0`).
        scale: f32,
        /// Entries, chosen by the greatest `threshold` not exceeding the value.
        entries: Vec<RangeEntry>,
        /// Chosen when the value is below every threshold.
        fallback: Option<Box<ItemModelNode>>,
    },
    /// `minecraft:special` — a code-driven special renderer over a `base` model.
    /// The `kind` is the special renderer id (e.g. `minecraft:chest`); this crate
    /// carries the *data* (which renderer, which base), the geometry is code.
    Special {
        /// The base item model providing the GUI/thrown sprite.
        base: ResourceLocation,
        /// The special renderer id.
        kind: String,
    },
    /// `minecraft:empty` — renders nothing.
    Empty,
    /// A node type this parser does not model (preserved, not rejected).
    Other {
        /// The unrecognised `type` id.
        kind: String,
    },
}

/// One `select` case.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectCase {
    /// The key strings this case matches (`when`; vanilla allows one or a list).
    pub when: Vec<String>,
    /// The model chosen when a key matches.
    pub model: ItemModelNode,
}

/// One `range_dispatch` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct RangeEntry {
    /// The lower bound at which this entry becomes active.
    pub threshold: f32,
    /// The model chosen for values in `[threshold, next_threshold)`.
    pub model: ItemModelNode,
}

/// A tint source on a `model` leaf. Only the discriminant and constant default
/// are captured; the game evaluates the live value.
#[derive(Debug, Clone, PartialEq)]
pub struct TintSource {
    /// The tint type id (e.g. `minecraft:dye`, `minecraft:grass`).
    pub kind: String,
    /// The packed ARGB `default` colour, when the type provides one.
    pub default: Option<i32>,
}

/// One resolved output of [`ItemModel::resolve`]: either a model to render or a
/// special renderer to invoke.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ItemModelOutput<'a> {
    /// Render this model with these tint sources.
    Model {
        /// The model to render.
        model: &'a ResourceLocation,
        /// Its tint sources.
        tints: &'a [TintSource],
    },
    /// Invoke this special renderer over this base model.
    Special {
        /// The base sprite model.
        base: &'a ResourceLocation,
        /// The special renderer id.
        kind: &'a str,
    },
}

/// The runtime state an [`ItemModel`] tree is resolved against. The game
/// implements this; the tree itself is version data.
pub trait ItemPropertyContext {
    /// Evaluates a boolean `condition` property (with its optional component).
    fn condition(&self, property: &str, component: Option<&str>) -> bool;
    /// Evaluates a string `select` property, or `None` if unset.
    fn select(&self, property: &str) -> Option<String>;
    /// Evaluates a numeric `range_dispatch` property (before `scale`).
    fn range(&self, property: &str) -> f32;
}

impl ItemModel {
    /// Parses an item definition file's bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, ItemModelError> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|e| ItemModelError::Json(e.to_string()))?;
        let model = value.get("model").ok_or(ItemModelError::MissingModel)?;
        Ok(Self {
            root: parse_node(model)?,
        })
    }

    /// Every model resource location the tree can resolve to, in tree order
    /// (duplicates preserved — `select`/`range_dispatch` often reuse a model).
    /// This is the set the atlas builder and baker must be able to produce.
    pub fn model_refs(&self) -> Vec<&ResourceLocation> {
        let mut out = Vec::new();
        collect_refs(&self.root, &mut out);
        out
    }

    /// Every special renderer the tree references, as `(base, kind)`, in tree
    /// order. These are the code-driven renderers (chest, shield, trident, …)
    /// that the entity/block-entity renderer must supply — the data-vs-code seam.
    pub fn special_renderers(&self) -> Vec<(&ResourceLocation, &str)> {
        let mut out = Vec::new();
        collect_specials(&self.root, &mut out);
        out
    }

    /// Resolves the tree for a concrete stack, returning the output(s) to render.
    /// A `composite` yields several; `empty`/unknown yields none. Pure over the
    /// tree (data) and the context (runtime state).
    pub fn resolve<'a>(&'a self, ctx: &impl ItemPropertyContext) -> Vec<ItemModelOutput<'a>> {
        let mut out = Vec::new();
        resolve_node(&self.root, ctx, &mut out);
        out
    }
}

fn parse_node(value: &Value) -> Result<ItemModelNode, ItemModelError> {
    let obj = value
        .as_object()
        .ok_or_else(|| ItemModelError::BadField("model node is not an object".into()))?;
    let kind = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ItemModelError::BadField("model node missing \"type\"".into()))?;

    match strip_ns(kind) {
        "model" => {
            let model =
                ResourceLocation::parse(obj.get("model").and_then(Value::as_str).ok_or_else(
                    || ItemModelError::BadField("model leaf missing \"model\"".into()),
                )?)?;
            let tints = obj
                .get("tints")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().map(parse_tint).collect())
                .unwrap_or_default();
            Ok(ItemModelNode::Model { model, tints })
        }
        "composite" => {
            let models = obj
                .get("models")
                .and_then(Value::as_array)
                .ok_or_else(|| ItemModelError::BadField("composite missing \"models\"".into()))?
                .iter()
                .map(parse_node)
                .collect::<Result<_, _>>()?;
            Ok(ItemModelNode::Composite { models })
        }
        "condition" => {
            let property = required_str(obj, "property")?;
            let component = obj
                .get("component")
                .and_then(Value::as_str)
                .map(str::to_string);
            let on_true = Box::new(parse_node(required(obj, "on_true")?)?);
            let on_false = Box::new(parse_node(required(obj, "on_false")?)?);
            Ok(ItemModelNode::Condition {
                property,
                component,
                on_true,
                on_false,
            })
        }
        "select" => {
            // The distinguishing property may be `property` or a specialised key
            // (`block_state_property`, `component`, …); capture whichever names it.
            let property = obj
                .get("property")
                .and_then(Value::as_str)
                .or_else(|| obj.get("block_state_property").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            let cases = obj
                .get("cases")
                .and_then(Value::as_array)
                .ok_or_else(|| ItemModelError::BadField("select missing \"cases\"".into()))?
                .iter()
                .map(parse_case)
                .collect::<Result<_, _>>()?;
            let fallback = optional_boxed(obj, "fallback")?;
            Ok(ItemModelNode::Select {
                property,
                cases,
                fallback,
            })
        }
        "range_dispatch" => {
            let property = required_str(obj, "property")?;
            let scale = obj
                .get("scale")
                .and_then(Value::as_f64)
                .map(|v| v as f32)
                .unwrap_or(1.0);
            let mut entries: Vec<RangeEntry> = obj
                .get("entries")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    ItemModelError::BadField("range_dispatch missing \"entries\"".into())
                })?
                .iter()
                .map(parse_entry)
                .collect::<Result<_, _>>()?;
            // Vanilla sorts entries ascending by threshold before dispatch.
            entries.sort_by(|a, b| a.threshold.total_cmp(&b.threshold));
            let fallback = optional_boxed(obj, "fallback")?;
            Ok(ItemModelNode::RangeDispatch {
                property,
                scale,
                entries,
                fallback,
            })
        }
        "special" => {
            let base = ResourceLocation::parse(required_str(obj, "base")?.as_str())?;
            let kind = obj
                .get("model")
                .and_then(|m| m.get("type"))
                .and_then(Value::as_str)
                .ok_or_else(|| ItemModelError::BadField("special missing model.type".into()))?
                .to_string();
            Ok(ItemModelNode::Special { base, kind })
        }
        "empty" => Ok(ItemModelNode::Empty),
        other => Ok(ItemModelNode::Other {
            kind: other.to_string(),
        }),
    }
}

fn parse_case(value: &Value) -> Result<SelectCase, ItemModelError> {
    let obj = value
        .as_object()
        .ok_or_else(|| ItemModelError::BadField("select case is not an object".into()))?;
    let when = match obj.get("when") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str().map(str::to_string).ok_or_else(|| {
                    ItemModelError::BadField("select \"when\" entry not a string".into())
                })
            })
            .collect::<Result<_, _>>()?,
        // A non-string scalar `when` (bool/number) — keep it as its text form.
        Some(other) => vec![other.to_string()],
        None => {
            return Err(ItemModelError::BadField(
                "select case missing \"when\"".into(),
            ));
        }
    };
    let model = parse_node(required(obj, "model")?)?;
    Ok(SelectCase { when, model })
}

fn parse_entry(value: &Value) -> Result<RangeEntry, ItemModelError> {
    let obj = value
        .as_object()
        .ok_or_else(|| ItemModelError::BadField("range entry is not an object".into()))?;
    let threshold = obj
        .get("threshold")
        .and_then(Value::as_f64)
        .ok_or_else(|| ItemModelError::BadField("range entry missing \"threshold\"".into()))?
        as f32;
    let model = parse_node(required(obj, "model")?)?;
    Ok(RangeEntry { threshold, model })
}

fn parse_tint(value: &Value) -> TintSource {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let default = value
        .get("default")
        .and_then(Value::as_i64)
        .map(|v| v as i32);
    TintSource { kind, default }
}

fn strip_ns(kind: &str) -> &str {
    kind.strip_prefix("minecraft:").unwrap_or(kind)
}

fn required<'a>(
    obj: &'a serde_json::Map<String, Value>,
    key: &'static str,
) -> Result<&'a Value, ItemModelError> {
    obj.get(key).ok_or(ItemModelError::MissingKey(key))
}

fn required_str(
    obj: &serde_json::Map<String, Value>,
    key: &'static str,
) -> Result<String, ItemModelError> {
    required(obj, key)?
        .as_str()
        .map(str::to_string)
        .ok_or(ItemModelError::MissingKey(key))
}

fn optional_boxed(
    obj: &serde_json::Map<String, Value>,
    key: &'static str,
) -> Result<Option<Box<ItemModelNode>>, ItemModelError> {
    match obj.get(key) {
        Some(v) => Ok(Some(Box::new(parse_node(v)?))),
        None => Ok(None),
    }
}

fn collect_refs<'a>(node: &'a ItemModelNode, out: &mut Vec<&'a ResourceLocation>) {
    match node {
        ItemModelNode::Model { model, .. } => out.push(model),
        ItemModelNode::Composite { models } => models.iter().for_each(|m| collect_refs(m, out)),
        ItemModelNode::Condition {
            on_true, on_false, ..
        } => {
            collect_refs(on_true, out);
            collect_refs(on_false, out);
        }
        ItemModelNode::Select {
            cases, fallback, ..
        } => {
            cases.iter().for_each(|c| collect_refs(&c.model, out));
            if let Some(f) = fallback {
                collect_refs(f, out);
            }
        }
        ItemModelNode::RangeDispatch {
            entries, fallback, ..
        } => {
            entries.iter().for_each(|e| collect_refs(&e.model, out));
            if let Some(f) = fallback {
                collect_refs(f, out);
            }
        }
        // A special node's `base` is also a real, bakeable item model sprite.
        ItemModelNode::Special { base, .. } => out.push(base),
        ItemModelNode::Empty | ItemModelNode::Other { .. } => {}
    }
}

fn collect_specials<'a>(node: &'a ItemModelNode, out: &mut Vec<(&'a ResourceLocation, &'a str)>) {
    match node {
        ItemModelNode::Special { base, kind } => out.push((base, kind)),
        ItemModelNode::Composite { models } => models.iter().for_each(|m| collect_specials(m, out)),
        ItemModelNode::Condition {
            on_true, on_false, ..
        } => {
            collect_specials(on_true, out);
            collect_specials(on_false, out);
        }
        ItemModelNode::Select {
            cases, fallback, ..
        } => {
            cases.iter().for_each(|c| collect_specials(&c.model, out));
            if let Some(f) = fallback {
                collect_specials(f, out);
            }
        }
        ItemModelNode::RangeDispatch {
            entries, fallback, ..
        } => {
            entries.iter().for_each(|e| collect_specials(&e.model, out));
            if let Some(f) = fallback {
                collect_specials(f, out);
            }
        }
        ItemModelNode::Model { .. } | ItemModelNode::Empty | ItemModelNode::Other { .. } => {}
    }
}

fn resolve_node<'a>(
    node: &'a ItemModelNode,
    ctx: &impl ItemPropertyContext,
    out: &mut Vec<ItemModelOutput<'a>>,
) {
    match node {
        ItemModelNode::Model { model, tints } => out.push(ItemModelOutput::Model { model, tints }),
        ItemModelNode::Special { base, kind } => out.push(ItemModelOutput::Special { base, kind }),
        ItemModelNode::Composite { models } => {
            models.iter().for_each(|m| resolve_node(m, ctx, out))
        }
        ItemModelNode::Condition {
            property,
            component,
            on_true,
            on_false,
        } => {
            let branch = if ctx.condition(property, component.as_deref()) {
                on_true
            } else {
                on_false
            };
            resolve_node(branch, ctx, out);
        }
        ItemModelNode::Select {
            property,
            cases,
            fallback,
        } => {
            let key = ctx.select(property);
            let chosen = key.as_deref().and_then(|k| {
                cases
                    .iter()
                    .find(|c| c.when.iter().any(|w| w == k))
                    .map(|c| &c.model)
            });
            if let Some(n) = chosen.or(fallback.as_deref()) {
                resolve_node(n, ctx, out);
            }
        }
        ItemModelNode::RangeDispatch {
            property,
            scale,
            entries,
            fallback,
        } => {
            let value = ctx.range(property) * scale;
            // Entries are sorted ascending: pick the greatest threshold <= value.
            let chosen = entries
                .iter()
                .rev()
                .find(|e| value >= e.threshold)
                .map(|e| &e.model);
            if let Some(n) = chosen.or(fallback.as_deref()) {
                resolve_node(n, ctx, out);
            }
        }
        ItemModelNode::Empty | ItemModelNode::Other { .. } => {}
    }
}
