//! Block model parsing and resolution.
//!
//! [`RawModel`] mirrors a single model JSON file verbatim (parent, texture
//! variables, elements, and display metadata). [`ModelResolver`] flattens a
//! model's parent chain and substitutes `#variable` texture references, yielding
//! a [`ResolvedModel`] the renderer can consume without ever walking JSON or
//! following parents itself.
//!
//! This layer is geometry-only: no atlas/UV baking or PNG decoding happens here.
//! Texture references resolve to [`ResourceLocation`]s for a later stage to bake.

use crate::error::ModelError;
use crate::location::ResourceLocation;
use crate::manager::ResourceManager;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;

/// Maximum parent-chain depth before giving up (guards against pathological
/// packs even absent an exact cycle).
const MAX_PARENT_DEPTH: usize = 128;

/// A cube face direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// -Y
    Down,
    /// +Y
    Up,
    /// -Z
    North,
    /// +Z
    South,
    /// +X
    East,
    /// -X
    West,
}

impl Direction {
    /// Parses a direction name (`down`, `up`, `north`, `south`, `east`, `west`).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "down" => Direction::Down,
            "up" => Direction::Up,
            "north" => Direction::North,
            "south" => Direction::South,
            "east" => Direction::East,
            "west" => Direction::West,
            _ => return None,
        })
    }
}

/// A rotation axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// The X axis.
    X,
    /// The Y axis.
    Y,
    /// The Z axis.
    Z,
}

impl Axis {
    /// Parses an axis name (`x`, `y`, `z`).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "x" => Axis::X,
            "y" => Axis::Y,
            "z" => Axis::Z,
            _ => return None,
        })
    }
}

/// A single face of an element.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    /// Explicit `[u1, v1, u2, v2]` texture coordinates, if given (otherwise the
    /// renderer derives them from the element geometry).
    pub uv: Option<[f32; 4]>,
    /// The texture reference, typically a `#variable`.
    pub texture: String,
    /// The face that, when obscured by a neighbour, culls this face.
    pub cullface: Option<Direction>,
    /// Texture rotation in degrees (`0`, `90`, `180`, `270`).
    pub rotation: i32,
    /// Tint index for biome/colour tinting, if any.
    pub tintindex: Option<i32>,
}

/// An element-level rotation.
///
/// Vanilla models use two shapes:
/// - the classic single-axis form `{ "origin", "axis", "angle", "rescale" }`, and
/// - a Euler triple `{ "origin", "x", "y", "z" }` (introduced for hanging signs
///   in 1.21+).
///
/// Both are normalised here into a single `angles` triple of degrees about
/// `[x, y, z]`, so the renderer never has to branch on the source shape. Use
/// [`ElementRotation::single_axis`] to recover the classic axis/angle pair when
/// exactly one component is non-zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ElementRotation {
    /// The rotation origin.
    pub origin: [f32; 3],
    /// Rotation in degrees about each axis, `[x, y, z]`.
    pub angles: [f32; 3],
    /// Whether the element is rescaled to fit after rotation.
    pub rescale: bool,
}

impl ElementRotation {
    /// Returns the `(axis, angle)` pair when exactly one component is non-zero,
    /// i.e. the classic single-axis form. Returns `None` for a general Euler
    /// rotation (or when there is no rotation at all).
    pub fn single_axis(&self) -> Option<(Axis, f32)> {
        let [x, y, z] = self.angles;
        match (x != 0.0, y != 0.0, z != 0.0) {
            (true, false, false) => Some((Axis::X, x)),
            (false, true, false) => Some((Axis::Y, y)),
            (false, false, true) => Some((Axis::Z, z)),
            _ => None,
        }
    }
}

/// A box element within a model.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// The `[x, y, z]` start corner.
    pub from: [f32; 3],
    /// The `[x, y, z]` end corner.
    pub to: [f32; 3],
    /// An optional element rotation.
    pub rotation: Option<ElementRotation>,
    /// The per-direction faces.
    pub faces: HashMap<Direction, Face>,
    /// Whether this element casts ambient-occlusion shadows (vanilla `shade`).
    pub shade: Option<bool>,
    /// Optional emitted light level.
    pub light_emission: Option<i32>,
    /// Optional element name (editor metadata).
    pub name: Option<String>,
}

/// A per-slot display transform (`translation`/`rotation`/`scale`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayTransform {
    /// Rotation in degrees per axis.
    pub rotation: [f32; 3],
    /// Translation.
    pub translation: [f32; 3],
    /// Scale.
    pub scale: [f32; 3],
}

impl Default for DisplayTransform {
    fn default() -> Self {
        Self {
            rotation: [0.0; 3],
            translation: [0.0; 3],
            scale: [1.0; 3],
        }
    }
}

/// One of vanilla's `display` slots — the *context* an item model is being drawn
/// in, which selects which [`DisplayTransform`] poses it.
///
/// Mirrors vanilla's own item-display-context enum, and
/// [`json_name`](Self::json_name) is that enum's own "get serialized name"
/// accessor. The
/// `NONE` variant is deliberately absent: it has no `display` key and vanilla's
/// own "get transform" accessor answers it with `NO_TRANSFORM`, which is what
/// [`DisplayTransforms::get`] returns for any undeclared slot anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplaySlot {
    /// `thirdperson_lefthand` — held in another entity's left hand.
    ThirdPersonLeftHand,
    /// `thirdperson_righthand` — held in another entity's right hand.
    ThirdPersonRightHand,
    /// `firstperson_lefthand` — held in *our* left hand.
    FirstPersonLeftHand,
    /// `firstperson_righthand` — held in *our* right hand.
    FirstPersonRightHand,
    /// `head` — worn in the helmet slot.
    Head,
    /// `gui` — an inventory/hotbar slot.
    Gui,
    /// `ground` — a dropped item entity.
    Ground,
    /// `fixed` — an item frame.
    Fixed,
    /// `on_shelf` — 26.2's shelf block (vanilla's own "fixed from bottom" variant).
    OnShelf,
}

impl DisplaySlot {
    /// Every slot, in the order [`DisplayTransforms`] stores them.
    pub const ALL: [DisplaySlot; 9] = [
        DisplaySlot::ThirdPersonLeftHand,
        DisplaySlot::ThirdPersonRightHand,
        DisplaySlot::FirstPersonLeftHand,
        DisplaySlot::FirstPersonRightHand,
        DisplaySlot::Head,
        DisplaySlot::Gui,
        DisplaySlot::Ground,
        DisplaySlot::Fixed,
        DisplaySlot::OnShelf,
    ];

    /// The JSON key this slot appears under inside a model's `display` object.
    #[must_use]
    pub const fn json_name(self) -> &'static str {
        match self {
            DisplaySlot::ThirdPersonLeftHand => "thirdperson_lefthand",
            DisplaySlot::ThirdPersonRightHand => "thirdperson_righthand",
            DisplaySlot::FirstPersonLeftHand => "firstperson_lefthand",
            DisplaySlot::FirstPersonRightHand => "firstperson_righthand",
            DisplaySlot::Head => "head",
            DisplaySlot::Gui => "gui",
            DisplaySlot::Ground => "ground",
            DisplaySlot::Fixed => "fixed",
            DisplaySlot::OnShelf => "on_shelf",
        }
    }

    /// The index this slot occupies in [`DisplaySlot::ALL`].
    #[must_use]
    const fn index(self) -> usize {
        match self {
            DisplaySlot::ThirdPersonLeftHand => 0,
            DisplaySlot::ThirdPersonRightHand => 1,
            DisplaySlot::FirstPersonLeftHand => 2,
            DisplaySlot::FirstPersonRightHand => 3,
            DisplaySlot::Head => 4,
            DisplaySlot::Gui => 5,
            DisplaySlot::Ground => 6,
            DisplaySlot::Fixed => 7,
            DisplaySlot::OnShelf => 8,
        }
    }

    /// The right-hand slot a *left*-hand slot falls back to when the model
    /// declares no left-hand variant, or `None` for a slot that is not a
    /// left-hand one.
    ///
    /// This is vanilla's own item-transforms deserializer, which does exactly this
    /// substitution while reading one model's `display` object. It matters in
    /// practice: neither `block/block` nor `item/generated` declares
    /// `thirdperson_lefthand`, so without the fallback every block and every
    /// flat item would be posed with the identity in an off hand.
    #[must_use]
    pub const fn left_hand_fallback(self) -> Option<DisplaySlot> {
        match self {
            DisplaySlot::ThirdPersonLeftHand => Some(DisplaySlot::ThirdPersonRightHand),
            DisplaySlot::FirstPersonLeftHand => Some(DisplaySlot::FirstPersonRightHand),
            _ => None,
        }
    }
}

/// Every `display` slot of a resolved model, so a renderer can pose the same
/// baked geometry as an inventory icon, a dropped item, a held item or a hat.
///
/// # Why this exists rather than a bare `HashMap<String, _>`
///
/// [`ResolvedModel::display`] is that map, keyed by raw JSON slug. This type is
/// the *interpreted* view of it, and it carries the one rule the map cannot: a
/// missing left-hand slot resolves to the right-hand one
/// ([`DisplaySlot::left_hand_fallback`]). Looking a slug up directly is how you
/// get an identity-posed sword in an off hand.
///
/// # Declared versus resolved
///
/// [`declared`](Self::declared) reports only what the model's parent chain
/// actually wrote down; [`get`](Self::get) applies the left-hand fallback and
/// then defaults to the identity transform, mirroring vanilla's own
/// item-transform "no transform" constant. Prefer `get` for drawing and `declared` when
/// you need to know whether the data was really there — for instance to decide
/// whether a fallback constant is still being relied on.
///
/// Values are the **raw JSON numbers**, exactly as [`DisplayTransform`] stores
/// them: the `/16` on translation and vanilla's `±5`/`±4` clamps belong to the
/// renderer's matrix builder, not here.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DisplayTransforms([Option<DisplayTransform>; 9]);

impl DisplayTransforms {
    /// A model that declares no `display` block at all: every slot resolves to
    /// the identity.
    pub const NONE: DisplayTransforms = DisplayTransforms([None; 9]);

    /// Interprets a raw slug-keyed `display` map (i.e. [`RawModel::display`] or
    /// [`ResolvedModel::display`]). Unrecognised slugs are ignored.
    #[must_use]
    pub fn from_map(map: &HashMap<String, DisplayTransform>) -> Self {
        let mut slots = [None; 9];
        for slot in DisplaySlot::ALL {
            slots[slot.index()] = map.get(slot.json_name()).copied();
        }
        DisplayTransforms(slots)
    }

    /// What the model's parent chain actually declared for `slot`, with no
    /// fallback of any kind.
    #[must_use]
    pub fn declared(&self, slot: DisplaySlot) -> Option<DisplayTransform> {
        self.0[slot.index()]
    }

    /// The transform to pose with in `slot`: the declared one, else the
    /// right-hand mirror for a left-hand slot, else the identity.
    #[must_use]
    pub fn get(&self, slot: DisplaySlot) -> DisplayTransform {
        self.declared(slot)
            .or_else(|| slot.left_hand_fallback().and_then(|s| self.declared(s)))
            .unwrap_or_default()
    }

    /// Overwrites one slot. Useful for building a fixture without a pack.
    #[must_use]
    pub fn with(mut self, slot: DisplaySlot, transform: DisplayTransform) -> Self {
        self.0[slot.index()] = Some(transform);
        self
    }
}

/// The lighting model used to render the item form in a GUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiLight {
    /// Flat, front-lit (used by flat item models).
    Front,
    /// Side-lit (the default for block models).
    Side,
}

/// A single model JSON file, parsed verbatim (before parent resolution).
#[derive(Debug, Clone, PartialEq)]
pub struct RawModel {
    /// The parent model, if any.
    pub parent: Option<ResourceLocation>,
    /// Texture variables; values are either a `#reference` or a texture path.
    pub textures: HashMap<String, String>,
    /// Elements, if this model defines its own geometry.
    pub elements: Option<Vec<Element>>,
    /// Whether ambient occlusion is enabled.
    pub ambient_occlusion: Option<bool>,
    /// The GUI light mode string (`side`/`front`), preserved verbatim.
    pub gui_light: Option<String>,
    /// Display transforms keyed by slot (`gui`, `firstperson_righthand`, ...).
    pub display: HashMap<String, DisplayTransform>,
    /// Optional `[width, height]` texture atlas size.
    pub texture_size: Option<[u32; 2]>,
}

impl RawModel {
    /// Parses a model JSON document.
    pub fn parse(bytes: &[u8]) -> Result<Self, ModelError> {
        let root: Value = serde_json::from_slice(bytes).map_err(|e| ModelError::Json {
            location: "<model>".to_string(),
            reason: e.to_string(),
        })?;
        Self::from_value(&root, "<model>")
    }

    fn from_value(root: &Value, loc: &str) -> Result<Self, ModelError> {
        let err = |reason: String| ModelError::Json {
            location: loc.to_string(),
            reason,
        };
        let obj = root
            .as_object()
            .ok_or_else(|| err("expected an object".to_string()))?;

        let parent = match obj.get("parent") {
            Some(Value::String(s)) => {
                Some(ResourceLocation::parse(s).map_err(|e| err(e.to_string()))?)
            }
            Some(_) => return Err(err("\"parent\" must be a string".to_string())),
            None => None,
        };

        let mut textures = HashMap::new();
        if let Some(map) = obj.get("textures").and_then(Value::as_object) {
            for (k, v) in map {
                // A texture value is normally a string ("block/stone" or a
                // "#var" reference). 26.2 also allows an object form for
                // translucent textures: `{"sprite": "<loc>",
                // "force_translucent": true}`. The geometry layer only needs
                // the sprite reference; the translucency hint is a render
                // concern handled downstream.
                if let Some(s) = v.as_str() {
                    textures.insert(k.clone(), s.to_string());
                } else if let Some(s) = v.get("sprite").and_then(Value::as_str) {
                    textures.insert(k.clone(), s.to_string());
                }
            }
        }

        let elements = match obj.get("elements") {
            Some(Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(parse_element(item, loc)?);
                }
                Some(out)
            }
            Some(_) => return Err(err("\"elements\" must be an array".to_string())),
            None => None,
        };

        let ambient_occlusion = obj.get("ambientocclusion").and_then(Value::as_bool);
        let gui_light = obj
            .get("gui_light")
            .and_then(Value::as_str)
            .map(str::to_string);

        let mut display = HashMap::new();
        if let Some(map) = obj.get("display").and_then(Value::as_object) {
            for (slot, v) in map {
                display.insert(slot.clone(), parse_display_transform(v));
            }
        }

        let texture_size = obj
            .get("texture_size")
            .and_then(Value::as_array)
            .and_then(|a| Some([a.first()?.as_u64()? as u32, a.get(1)?.as_u64()? as u32]));

        Ok(Self {
            parent,
            textures,
            elements,
            ambient_occlusion,
            gui_light,
            display,
            texture_size,
        })
    }
}

fn parse_vec3(value: Option<&Value>) -> Option<[f32; 3]> {
    let a = value?.as_array()?;
    Some([
        a.first()?.as_f64()? as f32,
        a.get(1)?.as_f64()? as f32,
        a.get(2)?.as_f64()? as f32,
    ])
}

fn parse_vec4(value: Option<&Value>) -> Option<[f32; 4]> {
    let a = value?.as_array()?;
    Some([
        a.first()?.as_f64()? as f32,
        a.get(1)?.as_f64()? as f32,
        a.get(2)?.as_f64()? as f32,
        a.get(3)?.as_f64()? as f32,
    ])
}

fn parse_element(value: &Value, loc: &str) -> Result<Element, ModelError> {
    let err = |reason: String| ModelError::Json {
        location: loc.to_string(),
        reason,
    };
    let obj = value
        .as_object()
        .ok_or_else(|| err("element must be an object".to_string()))?;
    let from = parse_vec3(obj.get("from"))
        .ok_or_else(|| err("element missing valid \"from\"".to_string()))?;
    let to =
        parse_vec3(obj.get("to")).ok_or_else(|| err("element missing valid \"to\"".to_string()))?;

    let rotation = match obj.get("rotation") {
        Some(r) => Some(parse_element_rotation(r, loc)?),
        None => None,
    };

    let mut faces = HashMap::new();
    if let Some(map) = obj.get("faces").and_then(Value::as_object) {
        for (dir, face) in map {
            let Some(direction) = Direction::parse(dir) else {
                continue;
            };
            faces.insert(direction, parse_face(face, loc)?);
        }
    }

    Ok(Element {
        from,
        to,
        rotation,
        faces,
        shade: obj.get("shade").and_then(Value::as_bool),
        light_emission: obj
            .get("light_emission")
            .and_then(Value::as_i64)
            .map(|v| v as i32),
        name: obj.get("name").and_then(Value::as_str).map(str::to_string),
    })
}

fn parse_element_rotation(value: &Value, loc: &str) -> Result<ElementRotation, ModelError> {
    let err = |reason: String| ModelError::Json {
        location: loc.to_string(),
        reason,
    };
    let obj = value
        .as_object()
        .ok_or_else(|| err("rotation must be an object".to_string()))?;
    let origin = parse_vec3(obj.get("origin"))
        .ok_or_else(|| err("rotation missing valid \"origin\"".to_string()))?;
    let rescale = obj.get("rescale").and_then(Value::as_bool).unwrap_or(false);

    // Euler form: any of x/y/z present (used by hanging signs in 1.21+).
    if obj.contains_key("x") || obj.contains_key("y") || obj.contains_key("z") {
        let component = |k: &str| obj.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
        return Ok(ElementRotation {
            origin,
            angles: [component("x"), component("y"), component("z")],
            rescale,
        });
    }

    // Classic single-axis form.
    let axis = obj
        .get("axis")
        .and_then(Value::as_str)
        .and_then(Axis::parse)
        .ok_or_else(|| err("rotation missing valid \"axis\"".to_string()))?;
    let angle = obj
        .get("angle")
        .and_then(Value::as_f64)
        .ok_or_else(|| err("rotation missing valid \"angle\"".to_string()))? as f32;
    let mut angles = [0.0f32; 3];
    angles[match axis {
        Axis::X => 0,
        Axis::Y => 1,
        Axis::Z => 2,
    }] = angle;
    Ok(ElementRotation {
        origin,
        angles,
        rescale,
    })
}

fn parse_face(value: &Value, loc: &str) -> Result<Face, ModelError> {
    let err = |reason: String| ModelError::Json {
        location: loc.to_string(),
        reason,
    };
    let obj = value
        .as_object()
        .ok_or_else(|| err("face must be an object".to_string()))?;
    let texture = obj
        .get("texture")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(Face {
        uv: parse_vec4(obj.get("uv")),
        texture,
        cullface: obj
            .get("cullface")
            .and_then(Value::as_str)
            .and_then(Direction::parse),
        rotation: obj.get("rotation").and_then(Value::as_i64).unwrap_or(0) as i32,
        tintindex: obj
            .get("tintindex")
            .and_then(Value::as_i64)
            .map(|v| v as i32),
    })
}

fn parse_display_transform(value: &Value) -> DisplayTransform {
    let mut t = DisplayTransform::default();
    if let Some(obj) = value.as_object() {
        if let Some(r) = parse_vec3(obj.get("rotation")) {
            t.rotation = r;
        }
        if let Some(tr) = parse_vec3(obj.get("translation")) {
            t.translation = tr;
        }
        if let Some(s) = parse_vec3(obj.get("scale")) {
            t.scale = s;
        }
    }
    t
}

/// A resolved texture binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextureBinding {
    /// The variable resolved to a concrete texture location.
    Resolved(ResourceLocation),
    /// The variable could not be resolved (dangling `#reference`, a cycle, or an
    /// invalid identifier). The stored string is the last value seen.
    Unresolved(String),
}

/// A fully resolved model: parent chain flattened, texture variables resolved.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// The resolved texture bindings, keyed by variable name (without `#`).
    pub textures: HashMap<String, TextureBinding>,
    /// The geometry (from the nearest ancestor that defined `elements`).
    pub elements: Vec<Element>,
    /// Whether ambient occlusion is enabled (default `true`).
    pub ambient_occlusion: bool,
    /// The GUI light mode (default [`GuiLight::Side`]).
    pub gui_light: GuiLight,
    /// Display transforms keyed by slot.
    pub display: HashMap<String, DisplayTransform>,
    /// The `[width, height]` texture size (default `[16, 16]`).
    pub texture_size: [u32; 2],
    /// The `builtin/*` special model the parent chain terminated at, if any
    /// (`"generated"` for the sprite-extrusion path, `"entity"` for models drawn
    /// by a dedicated entity renderer). `None` for ordinary geometry models.
    pub builtin: Option<String>,
}

impl ResolvedModel {
    /// Resolves a texture reference to a concrete location.
    ///
    /// Accepts either a bare variable name (`all`) or a `#reference` (`#all`).
    /// Returns `None` if the variable is unknown or unresolved.
    pub fn resolve_texture(&self, name_or_ref: &str) -> Option<&ResourceLocation> {
        let key = name_or_ref.strip_prefix('#').unwrap_or(name_or_ref);
        match self.textures.get(key) {
            Some(TextureBinding::Resolved(loc)) => Some(loc),
            _ => None,
        }
    }

    /// The interpreted view of [`display`](Self::display): every vanilla slot,
    /// with the left-hand fallback applied on lookup.
    ///
    /// [`display`](Self::display) is merged **per slot** down the parent chain
    /// (child overrides parent), which is what vanilla's own
    /// resolved-model "find top transform" step does — it walks the chain once per slot
    /// rather than taking one ancestor's whole `display` object. Getting that
    /// wrong would give `item/handheld` (which declares only the four hand
    /// slots) no `ground` transform at all, instead of inheriting
    /// `item/generated`'s.
    #[must_use]
    pub fn display_transforms(&self) -> DisplayTransforms {
        DisplayTransforms::from_map(&self.display)
    }

    /// Returns the names of texture variables that failed to resolve.
    pub fn unresolved_textures(&self) -> Vec<String> {
        self.textures
            .iter()
            .filter(|(_, b)| matches!(b, TextureBinding::Unresolved(_)))
            .map(|(k, _)| k.clone())
            .collect()
    }
}

/// Loads and resolves block models from a [`ResourceManager`].
///
/// Raw parses are cached, so shared ancestors (e.g. `block/block`) are parsed
/// once even when many models inherit from them.
#[derive(Debug)]
pub struct ModelResolver<'a> {
    manager: &'a ResourceManager,
    cache: RefCell<HashMap<ResourceLocation, RawModel>>,
}

impl<'a> ModelResolver<'a> {
    /// Creates a resolver over the given pack stack.
    pub fn new(manager: &'a ResourceManager) -> Self {
        Self {
            manager,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Loads and parses a single raw model (using the cache).
    pub fn load_raw(&self, location: &ResourceLocation) -> Result<RawModel, ModelError> {
        if let Some(cached) = self.cache.borrow().get(location) {
            return Ok(cached.clone());
        }
        let bytes = self
            .manager
            .read_asset(location, "models", "json")
            .ok_or_else(|| ModelError::NotFound {
                location: location.to_string(),
            })?;
        let model = RawModel::from_value(
            &serde_json::from_slice(&bytes).map_err(|e| ModelError::Json {
                location: location.to_string(),
                reason: e.to_string(),
            })?,
            &location.to_string(),
        )?;
        self.cache
            .borrow_mut()
            .insert(location.clone(), model.clone());
        Ok(model)
    }

    /// Resolves a model: flattens its parent chain and substitutes texture
    /// variables.
    pub fn resolve(&self, location: &ResourceLocation) -> Result<ResolvedModel, ModelError> {
        // Walk from child to root, collecting the chain and guarding cycles.
        let mut chain: Vec<RawModel> = Vec::new();
        let mut visited: Vec<ResourceLocation> = Vec::new();
        let mut current = Some(location.clone());
        let mut builtin: Option<String> = None;
        while let Some(loc) = current {
            // `builtin/*` models (generated, entity) have no JSON file; they are
            // terminal sentinels, not something to load.
            if let Some(kind) = loc.path().strip_prefix("builtin/") {
                builtin = Some(kind.to_string());
                break;
            }
            if visited.contains(&loc) {
                return Err(ModelError::ParentCycle {
                    location: location.to_string(),
                });
            }
            if chain.len() >= MAX_PARENT_DEPTH {
                return Err(ModelError::MaxDepthExceeded {
                    location: location.to_string(),
                });
            }
            visited.push(loc.clone());
            let raw = self.load_raw(&loc)?;
            let parent = raw.parent.clone();
            chain.push(raw);
            current = parent;
        }

        // Merge texture variables from root down to child (child overrides).
        let mut textures: HashMap<String, String> = HashMap::new();
        let mut display: HashMap<String, DisplayTransform> = HashMap::new();
        for raw in chain.iter().rev() {
            for (k, v) in &raw.textures {
                textures.insert(k.clone(), v.clone());
            }
            for (slot, t) in &raw.display {
                display.insert(slot.clone(), *t);
            }
        }

        // Nearest-defined wins for these (child first).
        let elements = chain
            .iter()
            .find_map(|m| m.elements.clone())
            .unwrap_or_default();
        let ambient_occlusion = chain
            .iter()
            .find_map(|m| m.ambient_occlusion)
            .unwrap_or(true);
        let gui_light = chain
            .iter()
            .find_map(|m| m.gui_light.as_deref())
            .map(|s| match s {
                "front" => GuiLight::Front,
                _ => GuiLight::Side,
            })
            .unwrap_or(GuiLight::Side);
        let texture_size = chain
            .iter()
            .find_map(|m| m.texture_size)
            .unwrap_or([16, 16]);

        let resolved_textures = resolve_texture_vars(&textures);

        Ok(ResolvedModel {
            textures: resolved_textures,
            elements,
            ambient_occlusion,
            gui_light,
            display,
            texture_size,
            builtin,
        })
    }
}

/// Resolves every `#variable` reference within a merged texture map.
fn resolve_texture_vars(raw: &HashMap<String, String>) -> HashMap<String, TextureBinding> {
    let mut out = HashMap::with_capacity(raw.len());
    for key in raw.keys() {
        out.insert(key.clone(), resolve_one(key, raw));
    }
    out
}

/// Resolves a single variable, following `#reference` chains with cycle guarding.
fn resolve_one(start: &str, raw: &HashMap<String, String>) -> TextureBinding {
    let mut seen: Vec<String> = Vec::new();
    let mut key = start.to_string();
    loop {
        if seen.contains(&key) {
            return TextureBinding::Unresolved(format!("#{key}"));
        }
        seen.push(key.clone());
        let Some(value) = raw.get(&key) else {
            return TextureBinding::Unresolved(format!("#{key}"));
        };
        if let Some(reference) = value.strip_prefix('#') {
            key = reference.to_string();
            continue;
        }
        return match ResourceLocation::parse(value) {
            Ok(loc) => TextureBinding::Resolved(loc),
            Err(_) => TextureBinding::Unresolved(value.clone()),
        };
    }
}
