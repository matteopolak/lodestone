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
//! Faithful to vanilla's own item-renderer package (its own item-models,
//! range-select-item-model, select-item-model, composite-model classes) in
//! the decompiled
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
        /// Every `"transformation"` on the path from the definition's root down
        /// to this node, **outermost first** — the same chain
        /// [`ItemModelNode::Special::transformation`] carries, for the same
        /// reason: `"transformation"` is a field of *every* unbaked item-model
        /// record, not of the special-model-wrapper's unbaked variant alone, and
        /// vanilla's own block-model-wrapper bake step composes the accumulated
        /// parent matrix onto
        /// this node's own exactly as the special-model-wrapper's own bake step does — see
        /// [`ItemModelNode::Special::transformation`]'s doc for the full
        /// derivation and the composition order a caller must use.
        ///
        /// Measured against the shipped 26.2 jar: 2,131 `minecraft:model`
        /// leaves total, of which 16 carry their **own** `"transformation"`
        /// (every coloured bed's `foot` sub-model, offsetting it from the
        /// `head` sub-model the sibling `minecraft:model` node in the same
        /// `composite` carries — `black_bed.json`'s own two-entry list is the
        /// worked example) and **zero** inherit one from an ancestor
        /// `composite`/`condition`/`select`/`range_dispatch` node. So unlike
        /// `special` (14 of 91 inherit, none carry their own), every real case
        /// here is "carries", not "inherits" — but the chain shape still holds
        /// for a pack that combines both, which an `Option` could not
        /// represent.
        transformation: Vec<ItemNodeTransform>,
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
        /// Every `"transformation"` on the path from the definition's root down
        /// to this node, **outermost first** — this node's own is last, and the
        /// list is empty when nothing on the path carries one.
        ///
        /// # Why a chain and not this node's own field
        ///
        /// `"transformation"` is a field of *every* unbaked item-model record,
        /// not of the special-model-wrapper's unbaked variant alone: `bake(context,
        /// transformation)` takes the accumulated parent matrix, and each node
        /// composes a transformation-compose step `(parent, this.transformation)` before
        /// handing it to its children. Reading only the `special` node's own
        /// field silently drops an ancestor's — which is exactly what happened
        /// to `minecraft:shield`, whose `scale [1, -1, -1]` (vanilla's own
        /// shield-special-renderer flip, hoisted into data) sits on the
        /// enclosing `minecraft:condition` node. See
        /// `docs/banner-shield-patterns.md` for the pixel-level consequence.
        ///
        /// A caller folds the chain left to right onto its outer placement:
        /// `outer * m[0] * m[1] * …`.
        transformation: Vec<ItemNodeTransform>,
    },
    /// `minecraft:empty` — renders nothing.
    Empty,
    /// A node type this parser does not model (preserved, not rejected).
    Other {
        /// The unrecognised `type` id.
        kind: String,
    },
}

/// One node's `"transformation"` field — vanilla's own transformation record
/// (in its own math package): a TRS composed as `translation *
/// left_rotation * scale * right_rotation`, with a *quaternion pair* rather
/// than the Euler-angle rotation [`DisplayTransform`] uses.
///
/// **Any** node type may carry one, not only `special` — measured against the
/// 26.2 jar, 67 of the shipped `assets/minecraft/items/*.json` do, and 14 of
/// the 91 `special` nodes sit under an ancestor that carries it instead of
/// carrying it themselves (`shield`, `trident`, …). An earlier version of this
/// doc said "only the skull family", which was true of the *files this parser
/// then looked at* and not of the format.
///
/// Stored as raw JSON numbers, same convention as [`DisplayTransform`]:
/// units are already blocks (no vanilla `/16` here — that division is
/// specific to vanilla's own item-transform deserializer, not the
/// transformation record's), and
/// no clamp is applied, because the transformation record's own codec has none.
///
/// # How a caller must compose this
///
/// Vanilla's own special-model-wrapper unbaked-bake step computes
/// its own transformation-compose step `(transformation, this.transformation)`, which is
/// its own transformation-compose step `(final Matrix4fc parent, Optional<Transformation>
/// transform)` → `parent.mul(transform.getMatrix())` when present. JOML's own
/// matrix-multiply step multiplies as `this * other`, and applying a column-vector
/// matrix product right-to-left means `other` (this node's own transform) is
/// applied to the model *first*, and `parent` (the display-context transform
/// already in effect — vanilla's `display.gui`/`display.firstperson_*`/etc.,
/// or the equivalent world placement chain) applied *second*. In this
/// crate's own matrix convention (`glam`, also column-vector, also
/// right-to-left composition) that is:
///
/// ```text
/// final_placement = existing_outer_placement * node_transform_matrix
/// ```
///
/// where `node_transform_matrix = T(translation) * Q(left_rotation) *
/// S(scale) * Q(right_rotation)` and `existing_outer_placement` is whatever
/// the caller already builds for an *ordinary* special-renderer item (the
/// GUI icon's `gui_item_pose`, the held-item hand chain, or a dropped/other-
/// entity-hand/item-frame world placement) — none of which change; this
/// value is composed on top, not substituted for them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemNodeTransform {
    /// Translation, in blocks (already the JSON's raw units — no `/16`).
    pub translation: [f32; 3],
    /// The left (pre-scale) rotation quaternion, `[x, y, z, w]`.
    pub left_rotation: [f32; 4],
    /// Per-axis scale.
    pub scale: [f32; 3],
    /// The right (post-scale) rotation quaternion, `[x, y, z, w]`.
    pub right_rotation: [f32; 4],
}

impl Default for ItemNodeTransform {
    /// Vanilla's own transformation-identity constant: zero translation, identity rotations, unit
    /// scale.
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            left_rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
            right_rotation: [0.0, 0.0, 0.0, 1.0],
        }
    }
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

/// A tint source on a `model` leaf: the discriminant plus every JSON field
/// vanilla's own tint sources read. Evaluate one with
/// [`item_tint::resolve`](crate::item_tint::resolve), which needs runtime state
/// (the stack's components, the pack's grass colormap) this parser does not own.
///
/// Faithful to vanilla's own item-tint-sources registration class's eight
/// registrations (its own bootstrap step).
#[derive(Debug, Clone, PartialEq)]
pub struct TintSource {
    /// The tint type id (e.g. `minecraft:dye`, `minecraft:grass`).
    pub kind: String,
    /// The packed ARGB fallback colour, when the type provides one.
    ///
    /// # This is `default` **or** `value`, and conflating them dropped 12 files
    ///
    /// Seven of vanilla's eight tint sources name this field `default`;
    /// `minecraft:constant` alone names it **`value`** (the `Constant` record's
    /// component is `value`). This parser read only `default`, so every
    /// `minecraft:constant` tint in the game parsed to `None` and its colour was
    /// silently discarded — all six leaves items, `vine`, `lily_pad`,
    /// `filled_map`'s layer 0, `firework_star`'s layer 0 and `wolf_armor`'s.
    /// Nothing failed; the layers simply rendered untinted, which for a
    /// greyscale leaf sprite is indistinguishable from "no tint was specified".
    ///
    /// Both spellings are accepted here rather than switching on `kind`, because
    /// a source carrying neither is already handled (the tint applies nothing)
    /// and a source carrying the wrong one is a pack bug we need not punish.
    ///
    /// Also accepts vanilla's own RGB-color-codec `[r, g, b]` float-triple
    /// alternative, converted by
    /// [`item_tint::color_from_float`](crate::item_tint::color_from_float). No
    /// vanilla file uses that form; resource packs may.
    pub default: Option<i32>,
    /// `minecraft:grass`'s climate inputs, `[temperature, downfall]`, which index
    /// the grass colormap (vanilla's own grass-color-source "calculate" step). Both are required
    /// fields on that source; `None` means the JSON omitted them, and
    /// `item_tint` then substitutes vanilla's own grass-color-source no-argument default
    /// of `[0.5, 1.0]` — which is also the value
    /// all six vanilla `grass` item definitions carry.
    ///
    /// Meaningless for the other seven sources, and `None` for them.
    pub grass: Option<[f32; 2]>,
    /// `minecraft:custom_model_data`'s `index` into that component's `colors`
    /// list (`CustomModelDataSource`'s codec, optional, default `0`).
    ///
    /// Meaningless for the other seven sources, and `0` for them.
    pub index: u32,
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
        /// Every `"transformation"` on the root-to-node path, outermost first
        /// — see [`ItemModelNode::Model`]'s field of the same name for why
        /// this is a chain, and [`ItemNodeTransform`]'s doc for how a caller
        /// must compose each entry (on top of whatever pose the caller
        /// already builds for this model — its own resolved `display.gui`/
        /// `display.firstperson_*` transform, none of which this replaces).
        transformation: &'a [ItemNodeTransform],
    },
    /// Invoke this special renderer over this base model.
    Special {
        /// The base sprite model.
        base: &'a ResourceLocation,
        /// The special renderer id.
        kind: &'a str,
        /// Every `"transformation"` on the root-to-node path, outermost first —
        /// see [`ItemModelNode::Special`]'s field of the same name for why this
        /// is a chain, and [`ItemNodeTransform`]'s doc for how a caller must
        /// compose each entry.
        transformation: &'a [ItemNodeTransform],
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

    /// Every output the tree can produce across **every** branch, in tree order
    /// (duplicates preserved).
    ///
    /// The context-free union of [`Self::resolve`]: whatever context is supplied,
    /// `resolve`'s result is a subsequence of this. That is the property a
    /// *baker* needs — it must build geometry for every form a stack could take
    /// before it knows which one any given frame will ask for, and for a flat
    /// `builtin/generated` variant that means seeding its `layerN` sprites into
    /// the atlas *before the atlas is stitched*.
    ///
    /// Distinct from [`Self::model_refs`], which throws the tint list away and so
    /// cannot round-trip through [`crate::icon::ItemIconBuilder::part_for_model`];
    /// and from [`Self::special_renderers`], which keeps only the `special`
    /// nodes. This keeps both kinds, in the one type [`Self::resolve`] speaks, so
    /// a caller can run the same classification over a variant it discovered as
    /// over one a context chose.
    pub fn outputs(&self) -> Vec<ItemModelOutput<'_>> {
        let mut out = Vec::new();
        collect_outputs(&self.root, &mut out);
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

    // `"transformation"` is a field of every unbaked item-model record, not of
    // `special` alone (see `ItemModelNode::Special::transformation`). The
    // `special` arm below consumes its own; every other arm's is pushed into the
    // subtree at the end of this function.
    let node = parse_node_body(obj, strip_ns(kind))?;
    let Some(own) = obj.get("transformation").map(parse_node_transform) else {
        return Ok(node);
    };
    let mut node = node;
    // `Special` and `Model` both consume *their own* `"transformation"` field
    // directly in `parse_node_body` (into the tail of their own chain), so
    // skipping them here is what stops this node's own field being applied
    // twice — once directly, once more via `prepend_node_transform` finding
    // itself as its own descendant, which it structurally cannot (the guard
    // exists for the *other six* node kinds, which have no chain of their
    // own and must push this transform down into whichever `Special`/`Model`
    // descendants they have).
    if !matches!(node, ItemModelNode::Special { .. } | ItemModelNode::Model { .. }) {
        prepend_node_transform(&mut node, own);
    }
    Ok(node)
}

/// [`parse_node`]'s per-`type` dispatch, split out so the shared
/// `"transformation"` handling has one place to sit rather than one per arm.
fn parse_node_body(
    obj: &serde_json::Map<String, Value>,
    kind: &str,
) -> Result<ItemModelNode, ItemModelError> {
    match kind {
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
            // This node's own field only, the same shape `"special"`'s own arm
            // uses — `parse_node` prepends every ancestor's below, which is
            // what makes the stored value a chain.
            let transformation = obj
                .get("transformation")
                .map(parse_node_transform)
                .into_iter()
                .collect();
            Ok(ItemModelNode::Model {
                model,
                tints,
                transformation,
            })
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
            // `model.type` names the special renderer as a resource location.
            // Server packs commonly omit its default namespace (`"head"`), but
            // every special-item consumer dispatches on the canonical id.
            let kind = ResourceLocation::parse(
                obj
                .get("model")
                .and_then(|m| m.get("type"))
                .and_then(Value::as_str)
                .ok_or_else(|| ItemModelError::BadField("special missing model.type".into()))?,
            )?
            .to_string();
            // This node's own field only; `parse_node` prepends every ancestor's
            // below, which is what makes the stored value a chain.
            let transformation = obj
                .get("transformation")
                .map(parse_node_transform)
                .into_iter()
                .collect();
            Ok(ItemModelNode::Special {
                base,
                kind,
                transformation,
            })
        }
        "empty" => Ok(ItemModelNode::Empty),
        other => Ok(ItemModelNode::Other {
            kind: other.to_string(),
        }),
    }
}

/// Prepends an ancestor node's `"transformation"` to every [`Special`] node in
/// `node`'s subtree, so a `Special` ends up holding the whole root-to-node
/// chain outermost-first.
///
/// This is vanilla's own unbaked item-model bake step's own transformation-compose
/// step `(parent, this.transformation)` done once at parse time rather than per resolve: the
/// accumulation is static (it does not depend on which branch a predicate
/// picks), exactly as vanilla's is, so there is nothing to defer.
///
/// [`Special`]: ItemModelNode::Special
fn prepend_node_transform(node: &mut ItemModelNode, outer: ItemNodeTransform) {
    match node {
        ItemModelNode::Special { transformation, .. } => transformation.insert(0, outer),
        ItemModelNode::Composite { models } => {
            for child in models {
                prepend_node_transform(child, outer);
            }
        }
        ItemModelNode::Condition {
            on_true, on_false, ..
        } => {
            prepend_node_transform(on_true, outer);
            prepend_node_transform(on_false, outer);
        }
        ItemModelNode::Select {
            cases, fallback, ..
        } => {
            for case in cases {
                prepend_node_transform(&mut case.model, outer);
            }
            if let Some(fallback) = fallback {
                prepend_node_transform(fallback, outer);
            }
        }
        ItemModelNode::RangeDispatch {
            entries, fallback, ..
        } => {
            for entry in entries {
                prepend_node_transform(&mut entry.model, outer);
            }
            if let Some(fallback) = fallback {
                prepend_node_transform(fallback, outer);
            }
        }
        // A `model` leaf accumulates the chain exactly like `Special` now —
        // this used to say the accumulated transform was dropped outright
        // (`ItemModelOutput::Model` carried no such field), which was the
        // same defect the shield fix closed one variant over: `bake(context,
        // transformation)` threads the parent matrix down to *every*
        // unbaked item-model record, not the special-model-wrapper's unbaked
        // variant alone.
        ItemModelNode::Model { transformation, .. } => transformation.insert(0, outer),
        ItemModelNode::Empty | ItemModelNode::Other { .. } => {}
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
    // `default` for seven of the eight sources, `value` for `minecraft:constant`
    // — see `TintSource::default`'s doc for what reading only the first cost.
    let default = value
        .get("default")
        .or_else(|| value.get("value"))
        .and_then(parse_rgb_color);
    let grass = parse_grass_climate(value);
    let index = value
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    TintSource {
        kind,
        default,
        grass,
        index,
    }
}

/// Vanilla's own RGB-color codec:
/// a signed int, or an
/// `[r, g, b]` float triple folded through
/// vanilla's own ARGB "color from float" step `(1.0F, r, g, b)`.
fn parse_rgb_color(value: &Value) -> Option<i32> {
    if let Some(i) = value.as_i64() {
        return Some(i as i32);
    }
    let [r, g, b] = value.as_array()?.as_slice() else {
        return None;
    };
    Some(crate::item_tint::color_from_float(
        r.as_f64()? as f32,
        g.as_f64()? as f32,
        b.as_f64()? as f32,
    ))
}

/// `minecraft:grass`'s two required climate fields
/// (`GrassColorSource`'s codec). Both must be present to be usable — a
/// half-specified source falls back to vanilla's own default pair rather than
/// mixing one authored value with one invented one.
fn parse_grass_climate(value: &Value) -> Option<[f32; 2]> {
    let temperature = value.get("temperature")?.as_f64()? as f32;
    let downfall = value.get("downfall")?.as_f64()? as f32;
    Some([temperature, downfall])
}

/// A `minecraft:special` node's `"transformation"` object —
/// `com.mojang.math.Transformation`'s codec (`translation`, `left_rotation`,
/// `scale`, `right_rotation`, all required fields on the real record). A
/// component whose field is absent or malformed falls back to
/// [`ItemNodeTransform::default`]'s value for that component rather than
/// failing the whole item's parse — consistent with this parser's general
/// leniency (unrecognised node types are preserved, not rejected).
fn parse_node_transform(value: &Value) -> ItemNodeTransform {
    let default = ItemNodeTransform::default();
    ItemNodeTransform {
        translation: parse_vec3(value.get("translation")).unwrap_or(default.translation),
        left_rotation: parse_vec4(value.get("left_rotation")).unwrap_or(default.left_rotation),
        scale: parse_vec3(value.get("scale")).unwrap_or(default.scale),
        right_rotation: parse_vec4(value.get("right_rotation")).unwrap_or(default.right_rotation),
    }
}

fn parse_vec3(value: Option<&Value>) -> Option<[f32; 3]> {
    let [x, y, z] = value?.as_array()?.as_slice() else {
        return None;
    };
    Some([x.as_f64()? as f32, y.as_f64()? as f32, z.as_f64()? as f32])
}

fn parse_vec4(value: Option<&Value>) -> Option<[f32; 4]> {
    let [x, y, z, w] = value?.as_array()?.as_slice() else {
        return None;
    };
    Some([
        x.as_f64()? as f32,
        y.as_f64()? as f32,
        z.as_f64()? as f32,
        w.as_f64()? as f32,
    ])
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
        ItemModelNode::Special { base, kind, .. } => out.push((base, kind)),
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

/// [`ItemModel::outputs`]'s walker: every branch, not the chosen one.
///
/// Deliberately shaped as a near-copy of [`resolve_node`] rather than sharing
/// code with it through an "all branches" context. An `ItemPropertyContext` that
/// claimed to take every branch cannot exist — `condition` returns one `bool` —
/// so the alternative was a second trait, for no gain over eight arms.
fn collect_outputs<'a>(node: &'a ItemModelNode, out: &mut Vec<ItemModelOutput<'a>>) {
    match node {
        ItemModelNode::Model { model, tints, transformation } => out.push(ItemModelOutput::Model { model, tints, transformation }),
        ItemModelNode::Special {
            base,
            kind,
            transformation,
        } => out.push(ItemModelOutput::Special {
            base,
            kind,
            transformation,
        }),
        ItemModelNode::Composite { models } => {
            models.iter().for_each(|m| collect_outputs(m, out));
        }
        ItemModelNode::Condition {
            on_true, on_false, ..
        } => {
            collect_outputs(on_true, out);
            collect_outputs(on_false, out);
        }
        ItemModelNode::Select {
            cases, fallback, ..
        } => {
            cases.iter().for_each(|c| collect_outputs(&c.model, out));
            if let Some(f) = fallback {
                collect_outputs(f, out);
            }
        }
        ItemModelNode::RangeDispatch {
            entries, fallback, ..
        } => {
            entries.iter().for_each(|e| collect_outputs(&e.model, out));
            if let Some(f) = fallback {
                collect_outputs(f, out);
            }
        }
        ItemModelNode::Empty | ItemModelNode::Other { .. } => {}
    }
}

fn resolve_node<'a>(
    node: &'a ItemModelNode,
    ctx: &impl ItemPropertyContext,
    out: &mut Vec<ItemModelOutput<'a>>,
) {
    match node {
        ItemModelNode::Model { model, tints, transformation } => out.push(ItemModelOutput::Model { model, tints, transformation }),
        ItemModelNode::Special {
            base,
            kind,
            transformation,
        } => out.push(ItemModelOutput::Special {
            base,
            kind,
            transformation,
        }),
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
