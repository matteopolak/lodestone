//! Code-defined entity model geometry (mobs, the player, block entities).
//!
//! # Why this is a separate pipeline
//!
//! Block models are JSON and resolve through [`crate::model`]. Entity models are
//! **not data** in vanilla — they are Java code that assembles a cuboid hierarchy
//! (`LayerDefinition` → `MeshDefinition` → `PartDefinition` → `CubeDefinition`),
//! each part carrying a pivot (`PartPose`) and each cube carrying a box + a
//! texel offset that is unwrapped onto a single per-entity texture sheet by a
//! fixed "box unwrap". Nothing in `.cache/mc/26.2/generated/` or
//! `vendor/minecraft-data/` exposes this geometry as data — `entities.json` is
//! metadata (hitbox, id, type) only. So the geometry has to be *described* on our
//! side; there is no extraction path short of running an exporter mod.
//!
//! # The seam
//!
//! This module owns the **version-free primitive**: the cube unwrap (faithful to
//! `ModelPart.Cube` in the decompiled 26.2 client) and the part-hierarchy
//! transform (translate by `pivot/16`, then `rotationZYX(zRot, yRot, xRot)`, then
//! scale — exactly `ModelPart.translateAndRotate`). A version crate supplies the
//! per-mob [`EntityModelDef`] data; [`bake_entity`] turns it into posed,
//! UV-mapped [`EntityQuad`]s the renderer can upload. The renderer poses parts at
//! runtime (walk cycles etc.) by adjusting each [`PartPose`] before baking, or by
//! transforming the emitted parts — that animation layer lives above this crate.
//!
//! Textures resolve through the normal [`crate::ResourceManager`] path
//! (`assets/<ns>/textures/entity/...`, and skins likewise); this crate does not
//! special-case them.

use crate::model::Direction;

/// A cuboid "grow"/inflation applied symmetrically to a box before unwrapping,
/// mirroring vanilla's `CubeDeformation`. Overlay layers (hat, sleeves, armour)
/// use a small positive grow so they sit just outside the base layer.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Deformation {
    /// Grow along X, in model texels.
    pub x: f32,
    /// Grow along Y, in model texels.
    pub y: f32,
    /// Grow along Z, in model texels.
    pub z: f32,
}

impl Deformation {
    /// A uniform grow of `v` texels on every axis (vanilla `CubeDeformation(v)`).
    pub fn uniform(v: f32) -> Self {
        Self { x: v, y: v, z: v }
    }
}

/// The six box faces, in the order vanilla emits them.
const FACE_ORDER: [Direction; 6] = [
    Direction::Down,
    Direction::Up,
    Direction::West,
    Direction::North,
    Direction::East,
    Direction::South,
];

/// A single box within a part, unwrapped onto the entity's texture sheet exactly
/// as vanilla's `CubeDefinition`/`ModelPart.Cube` does.
#[derive(Debug, Clone, PartialEq)]
pub struct CubeDef {
    /// Box origin `(minX, minY, minZ)` in model texels, relative to the part
    /// pivot.
    pub origin: [f32; 3],
    /// Box `(width, height, depth)` in model texels.
    pub size: [f32; 3],
    /// Texel offset `(xTexOffs, yTexOffs)` of the unwrap on the sheet.
    pub tex_offset: [f32; 2],
    /// Symmetric inflation applied before unwrapping.
    pub grow: Deformation,
    /// Whether the box is mirrored across X (flips X extent and winding).
    pub mirror: bool,
    /// Per-cube texture-scale multiplier (`texScale`, default `1.0`), used by the
    /// handful of models that stretch the sheet.
    pub tex_scale: [f32; 2],
    /// Which faces to emit; `[true; 6]` (all) by default.
    pub visible_faces: [bool; 6],
}

impl CubeDef {
    /// A box with all faces visible, no grow, no mirror, unit texture scale.
    pub fn new(origin: [f32; 3], size: [f32; 3], tex_offset: [f32; 2]) -> Self {
        Self {
            origin,
            size,
            tex_offset,
            grow: Deformation::default(),
            mirror: false,
            tex_scale: [1.0, 1.0],
            visible_faces: [true; 6],
        }
    }

    /// Returns the box with a uniform grow (overlay layers).
    pub fn grown(mut self, v: f32) -> Self {
        self.grow = Deformation::uniform(v);
        self
    }

    /// Returns the box mirrored across X.
    pub fn mirrored(mut self) -> Self {
        self.mirror = true;
        self
    }
}

/// A part's pivot and orientation, mirroring vanilla's `PartPose`. Rotations are
/// radians and applied `rotationZYX(zRot, yRot, xRot)` (Z, then Y, then X).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PartPose {
    /// Pivot X in model texels.
    pub x: f32,
    /// Pivot Y in model texels.
    pub y: f32,
    /// Pivot Z in model texels.
    pub z: f32,
    /// Rotation about X, radians.
    pub x_rot: f32,
    /// Rotation about Y, radians.
    pub y_rot: f32,
    /// Rotation about Z, radians.
    pub z_rot: f32,
    /// Per-axis scale (default `1.0`).
    pub scale: [f32; 3],
}

impl PartPose {
    /// The identity pose.
    pub const ZERO: PartPose = PartPose {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        x_rot: 0.0,
        y_rot: 0.0,
        z_rot: 0.0,
        scale: [1.0, 1.0, 1.0],
    };

    /// A pose translated to `(x, y, z)` model texels with no rotation.
    pub fn offset(x: f32, y: f32, z: f32) -> Self {
        Self {
            x,
            y,
            z,
            ..Self::ZERO
        }
    }

    /// A pose rotated `(x, y, z)` radians at the origin.
    pub fn rotation(x_rot: f32, y_rot: f32, z_rot: f32) -> Self {
        Self {
            x_rot,
            y_rot,
            z_rot,
            ..Self::ZERO
        }
    }

    /// Returns the pose with a uniform per-axis scale — vanilla's
    /// `PartPose.scaled(float)`.
    ///
    /// A *part* scale, not a cube grow: it multiplies everything under the
    /// part, children included, and is applied after the rotation by
    /// `part_transform`. `CubeDef::grown` inflates one box by a texel amount
    /// and is the wrong tool for a rig authored at a fraction of its natural
    /// size (the ender dragon's head, reused whole by the dragon-head skull).
    pub fn scaled(mut self, v: f32) -> Self {
        self.scale = [v, v, v];
        self
    }

    /// A pose with both an offset and a rotation.
    pub fn offset_and_rotation(x: f32, y: f32, z: f32, x_rot: f32, y_rot: f32, z_rot: f32) -> Self {
        Self {
            x,
            y,
            z,
            x_rot,
            y_rot,
            z_rot,
            scale: [1.0, 1.0, 1.0],
        }
    }
}

/// A node in the entity's part hierarchy: a pivot, its boxes, and named children.
/// Children are an ordered list (not a map) so baking is deterministic.
#[derive(Debug, Clone, PartialEq)]
pub struct PartDef {
    /// This part's pivot and orientation.
    pub pose: PartPose,
    /// Boxes attached directly to this part.
    pub cubes: Vec<CubeDef>,
    /// Named child parts, in insertion order.
    pub children: Vec<(String, PartDef)>,
}

impl PartDef {
    /// An empty part with the given pose.
    pub fn new(pose: PartPose) -> Self {
        Self {
            pose,
            cubes: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Adds a box to this part (builder style).
    pub fn with_cube(mut self, cube: CubeDef) -> Self {
        self.cubes.push(cube);
        self
    }

    /// Adds a named child part (builder style).
    pub fn with_child(mut self, name: &str, child: PartDef) -> Self {
        self.children.push((name.to_string(), child));
        self
    }

    /// Looks up a mutable child by name (for authoring meshes incrementally).
    pub fn child_mut(&mut self, name: &str) -> Option<&mut PartDef> {
        self.children
            .iter_mut()
            .find(|(n, _)| n == name)
            .map(|(_, c)| c)
    }
}

/// A complete entity model: a texture-sheet size and a root part.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityModelDef {
    /// Texture sheet width in pixels (the UV normaliser).
    pub texture_width: u32,
    /// Texture sheet height in pixels.
    pub texture_height: u32,
    /// The root part; its children are the model's top-level parts.
    pub root: PartDef,
}

/// The runtime state that selects an entity's texture variant, for mobs whose
/// skin depends on more than their type: the temperature/biome family (26.2's
/// `_temperate`/`_cold`/`_warm` split of pig/cow/chicken/…), breed (cat, wolf),
/// profession/type (villager), and so on.
///
/// **Not sheep wool.** A sheep's *body* texture is dye-independent — only the
/// separate wool *layer*'s colour varies, by a flat gamma-space tint multiply
/// (`sheep_wool_tint`), never by selecting a different sheet. There is
/// deliberately no `Dyed`/`Collar` arm here: a per-colour texture would be the
/// wrong mechanism for what vanilla does with one multiply.
///
/// This is a **primitive owned by `lodestone-assets`** on purpose: it lets the
/// per-mob data in `entity_models.rs` stay a pure data module (no reach into
/// `lodestone-model`, `-net`, or the shell), so that module remains a one-file
/// move into a version crate. The consumer (the shell) maps its own
/// `EntityView`/entity metadata onto this enum at the boundary. Each arm carries
/// a small typed value rather than a raw int so a selector can be written total.
///
/// New variant axes are added here as they are wired; a selector only matches
/// the axis it cares about and falls through to its default for the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntityVariant {
    /// The climate family a mob spawned into. 26.2 gave pigs, cows, chickens and
    /// several others per-temperature skins instead of one universal sheet.
    Temperature(Temperature),
    /// A horse's colour coat (`Horse.Variant` / `HorseRenderer.getTextureLocation`).
    /// This is deliberately *only* the base-colour layer: vanilla's markings
    /// overlay (`Markings`, drawn by `HorseMarkingLayer` as an independent
    /// second translucent pass over the same model) is a second, unrelated
    /// selection axis, not a sub-case of colour — see `horse_markings_texture`
    /// in `entity_models.rs`, which is intentionally *not* routed through
    /// `EntityTexture`/`ByVariant` because that shape only carries one path.
    HorseColor(HorseColor),
    /// A llama or trader llama's wool colour (`Llama.Variant`).
    Llama(LlamaColor),
    /// A cat's breed (`CatVariant`/`CatVariants.java`).
    Cat(CatCoat),
    /// A wolf's breed and tame/angry state, combined: vanilla's own
    /// `Wolf.getTexture()` resolves both together to one texture path, so
    /// they are one axis here rather than two independent ones.
    Wolf {
        /// The breed (`WolfVariant`/`WolfVariants.java`).
        coat: WolfCoat,
        /// Wild, tame, or (tamed-and-)angry — each breed ships one file per state.
        state: WolfState,
    },
    /// A parrot's plumage colour (`Parrot.Variant`).
    Parrot(ParrotColor),
    /// A mooshroom's mushroom colour (`MushroomCow.Variant`), reusing the
    /// plain cow mesh — an independent axis from `Temperature`, since a
    /// mooshroom is never re-skinned by climate.
    Mooshroom(MooshroomColor),
}

/// The three climate families 26.2 ships variant skins for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Temperature {
    /// The default/overworld skin (`*_temperate`).
    Temperate,
    /// Cold biomes (`*_cold`).
    Cold,
    /// Warm biomes (`*_warm`).
    Warm,
}

/// A horse's base coat colour (`Horse.Variant`, `HorseRenderer.java`). Ordered
/// as vanilla's own `id` field, not alphabetically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HorseColor {
    /// `entity/horse/horse_white`.
    White,
    /// `entity/horse/horse_creamy`.
    Creamy,
    /// `entity/horse/horse_chestnut`.
    Chestnut,
    /// `entity/horse/horse_brown`.
    Brown,
    /// `entity/horse/horse_black`.
    Black,
    /// `entity/horse/horse_gray`.
    Gray,
    /// `entity/horse/horse_darkbrown`.
    DarkBrown,
}

/// A horse's independent markings overlay (`Markings.java`). Not part of
/// [`EntityVariant`] — see the note on [`EntityVariant::HorseColor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HorseMarkings {
    /// No overlay pass at all (vanilla maps this to an invisible texture).
    None,
    /// `entity/horse/horse_markings_white`.
    White,
    /// `entity/horse/horse_markings_whitefield`.
    WhiteField,
    /// `entity/horse/horse_markings_whitedots`.
    WhiteDots,
    /// `entity/horse/horse_markings_blackdots`.
    BlackDots,
}

/// A llama/trader llama's wool colour (`Llama.Variant`, `LlamaRenderer.java`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LlamaColor {
    /// `entity/llama/llama_creamy`, vanilla's `DEFAULT`.
    Creamy,
    /// `entity/llama/llama_white`.
    White,
    /// `entity/llama/llama_brown`.
    Brown,
    /// `entity/llama/llama_gray`.
    Gray,
}

/// A cat's breed (`CatVariants.java`). Ordered as vanilla registers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CatCoat {
    /// `entity/cat/cat_tabby`, vanilla's default breed.
    Tabby,
    /// `entity/cat/cat_black`.
    Black,
    /// `entity/cat/cat_red`.
    Red,
    /// `entity/cat/cat_siamese`.
    Siamese,
    /// `entity/cat/cat_british_shorthair`.
    BritishShorthair,
    /// `entity/cat/cat_calico`.
    Calico,
    /// `entity/cat/cat_persian`.
    Persian,
    /// `entity/cat/cat_ragdoll`.
    Ragdoll,
    /// `entity/cat/cat_white`.
    White,
    /// `entity/cat/cat_jellie`.
    Jellie,
    /// `entity/cat/cat_all_black`.
    AllBlack,
}

/// A wolf's breed (`WolfVariants.java`). Ordered as vanilla registers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WolfCoat {
    /// `entity/wolf/wolf`, vanilla's default breed (`WolfVariants.DEFAULT`).
    Pale,
    /// `entity/wolf/wolf_spotted`.
    Spotted,
    /// `entity/wolf/wolf_snowy`.
    Snowy,
    /// `entity/wolf/wolf_black`.
    Black,
    /// `entity/wolf/wolf_ashen`.
    Ashen,
    /// `entity/wolf/wolf_rusty`.
    Rusty,
    /// `entity/wolf/wolf_woods`.
    Woods,
    /// `entity/wolf/wolf_chestnut`.
    Chestnut,
    /// `entity/wolf/wolf_striped`.
    Striped,
}

/// A wolf's wild/tame/angry state (`Wolf.getTexture()`); each breed ships one
/// texture file per state (`<breed>`, `<breed>_tame`, `<breed>_angry`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WolfState {
    /// Undomesticated (no suffix on the breed's texture file).
    Wild,
    /// Tamed by a player (`_tame` suffix).
    Tame,
    /// Tamed but currently hostile (`_angry` suffix).
    Angry,
}

/// A parrot's plumage colour (`Parrot.Variant`). Note vanilla's own filename
/// for `GRAY` is spelled `parrot_grey.png` — transcribed verbatim, not
/// "corrected" to the American spelling used by the enum case name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParrotColor {
    /// `entity/parrot/parrot_red_blue`, vanilla's default.
    RedBlue,
    /// `entity/parrot/parrot_blue`.
    Blue,
    /// `entity/parrot/parrot_green`.
    Green,
    /// `entity/parrot/parrot_yellow_blue`.
    YellowBlue,
    /// `entity/parrot/parrot_grey` — note the vanilla file uses British
    /// spelling despite the enum case matching `Parrot.Variant.GRAY`.
    Gray,
}

/// A mooshroom's mushroom colour (`MushroomCow.Variant`,
/// `MushroomCowRenderer.TEXTURES`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MooshroomColor {
    /// `entity/cow/mooshroom_red`, vanilla's default.
    Red,
    /// `entity/cow/mooshroom_brown`.
    Brown,
}

/// How an entry resolves its texture: a single fixed sheet, or a selector over
/// [`EntityVariant`] carrying an explicit default so a consumer can render (or
/// cache) it without first synthesising a variant.
///
/// One uniform representation for every entry — `Fixed` for the invariant mobs,
/// `ByVariant` for the rest — so the consumer never has to special-case two
/// mechanisms. The `default` sheet is what the shell uses today, where entity
/// skins are still synthetic tints and no runtime variant is plumbed yet.
#[derive(Clone, Copy, Debug)]
pub enum EntityTexture {
    /// A single texture path (relative to `assets/<ns>/textures/`, no extension).
    Fixed(&'static str),
    /// A variant-driven texture: `default` is the canonical sheet, `select`
    /// maps a runtime [`EntityVariant`] to the right one.
    ByVariant {
        /// The sheet used when no variant is known (the canonical/`_temperate` skin).
        default: &'static str,
        /// Maps a runtime variant to its texture path. Written total.
        select: fn(EntityVariant) -> &'static str,
    },
}

impl EntityTexture {
    /// The canonical sheet, ignoring any variant — what to load when the runtime
    /// variant is unknown (as it is in the shell today).
    pub fn default_path(&self) -> &'static str {
        match self {
            EntityTexture::Fixed(p) => p,
            EntityTexture::ByVariant { default, .. } => default,
        }
    }

    /// The sheet for a specific runtime variant. `Fixed` ignores the variant.
    pub fn resolve(&self, variant: EntityVariant) -> &'static str {
        match self {
            EntityTexture::Fixed(p) => p,
            EntityTexture::ByVariant { select, .. } => select(variant),
        }
    }

    /// Whether this entry's skin depends on a runtime variant. Lets a consumer
    /// decide up front whether its texture cache needs a variant dimension.
    pub fn is_variant(&self) -> bool {
        matches!(self, EntityTexture::ByVariant { .. })
    }
}

/// One baked quad of an entity model: four corners in world units (model texels
/// divided by 16, with the full part transform applied), UVs normalised to
/// `[0, 1]` against the texture sheet, and the outward face direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityQuad {
    /// The four corner positions, world units.
    pub positions: [[f32; 3]; 4],
    /// The four corner UVs, normalised to `[0, 1]`.
    pub uvs: [[f32; 2]; 4],
    /// The transformed outward normal (unit length).
    pub normal: [f32; 3],
    /// The box face this quad came from, in the part's local frame.
    pub direction: Direction,
}

/// A 3×3 linear map plus a translation, used to accumulate the part hierarchy.
///
/// Public so an animating renderer can rebuild a part's transform chain itself
/// from [`BakedPart`]s without reimplementing vanilla's `rotationZYX` order —
/// a second implementation of that would be free to drift from this one, and
/// nothing would notice until a limb bent the wrong way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    /// Row-major 3×3 linear part.
    pub m: [[f32; 3]; 3],
    /// Translation, in blocks.
    pub t: [f32; 3],
}

impl Affine {
    /// The identity transform.
    pub const IDENTITY: Affine = Affine {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        t: [0.0, 0.0, 0.0],
    };

    /// The local transform for a part pose: `translate(pivot/16) ∘ rotZYX ∘ scale`,
    /// matching `ModelPart.translateAndRotate`.
    #[must_use]
    pub fn of_pose(pose: &PartPose) -> Affine {
        part_transform(pose)
    }

    /// Maps a point.
    pub fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        [
            self.m[0][0] * p[0] + self.m[0][1] * p[1] + self.m[0][2] * p[2] + self.t[0],
            self.m[1][0] * p[0] + self.m[1][1] * p[1] + self.m[1][2] * p[2] + self.t[1],
            self.m[2][0] * p[0] + self.m[2][1] * p[1] + self.m[2][2] * p[2] + self.t[2],
        ]
    }

    /// Applies only the linear part (for normals).
    fn apply_linear(&self, p: [f32; 3]) -> [f32; 3] {
        [
            self.m[0][0] * p[0] + self.m[0][1] * p[1] + self.m[0][2] * p[2],
            self.m[1][0] * p[0] + self.m[1][1] * p[1] + self.m[1][2] * p[2],
            self.m[2][0] * p[0] + self.m[2][1] * p[1] + self.m[2][2] * p[2],
        ]
    }

    /// `self ∘ other` (apply `other` first).
    #[must_use]
    pub fn compose(&self, other: &Affine) -> Affine {
        let mut m = [[0.0f32; 3]; 3];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = self.m[i][0] * other.m[0][j]
                    + self.m[i][1] * other.m[1][j]
                    + self.m[i][2] * other.m[2][j];
            }
        }
        let t = self.apply(other.t);
        Affine { m, t }
    }
}

/// Builds the local transform for a part: translate(pivot/16) ∘ rotZYX ∘ scale,
/// matching `ModelPart.translateAndRotate`.
fn part_transform(pose: &PartPose) -> Affine {
    // Rotation R = Rz * Ry * Rx (JOML rotationZYX), so a vector is rotated X, then
    // Y, then Z.
    let (sx, cx) = pose.x_rot.sin_cos();
    let (sy, cy) = pose.y_rot.sin_cos();
    let (sz, cz) = pose.z_rot.sin_cos();
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    let rzy = mat_mul(rz, ry);
    let rot = mat_mul(rzy, rx);
    // Fold the per-axis scale into the columns of the rotation matrix.
    let mut m = rot;
    for row in m.iter_mut() {
        row[0] *= pose.scale[0];
        row[1] *= pose.scale[1];
        row[2] *= pose.scale[2];
    }
    Affine {
        m,
        t: [pose.x / 16.0, pose.y / 16.0, pose.z / 16.0],
    }
}

fn mat_mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut m = [[0.0f32; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    m
}

/// One part of a model baked on its own, for renderers that pose parts
/// independently (walk cycles, head tracking).
///
/// Unlike [`bake_entity`], the quads here are in the part's **own** frame with
/// *no* transform applied — not even the part's own pose. The renderer is
/// expected to rebuild the transform chain itself each frame from [`rest`]
/// (adjusted by whatever animation it applies) and [`parent`], which is exactly
/// what vanilla's `ModelPart.render` does. Baking the rest pose in would freeze
/// the very joints an animator needs to move.
///
/// [`rest`]: BakedPart::rest
/// [`parent`]: BakedPart::parent
#[derive(Debug, Clone, PartialEq)]
pub struct BakedPart {
    /// The part's name as declared by its parent (`""` for the root).
    pub name: String,
    /// Index of this part's parent in the flat list, or `None` for the root.
    /// Always less than this part's own index (the list is in pre-order), so a
    /// single forward pass can accumulate parent transforms.
    pub parent: Option<usize>,
    /// The authored (unanimated) pose. An animator copies this, adjusts it, and
    /// composes the chain.
    pub rest: PartPose,
    /// This part's own boxes, in part-local space with no transform applied.
    pub quads: Vec<EntityQuad>,
}

/// Bakes an entity model into one [`BakedPart`] per node, in pre-order.
///
/// The union of every part's quads *after* applying its transform chain equals
/// [`bake_entity`]'s output exactly; the difference is only that the chain is
/// left for the caller to apply, so it can be animated.
pub fn bake_entity_parts(model: &EntityModelDef) -> Vec<BakedPart> {
    let mut out = Vec::new();
    collect_parts(
        "",
        &model.root,
        None,
        model.texture_width as f32,
        model.texture_height as f32,
        &mut out,
    );
    out
}

fn collect_parts(
    name: &str,
    part: &PartDef,
    parent: Option<usize>,
    tw: f32,
    th: f32,
    out: &mut Vec<BakedPart>,
) {
    let mut quads = Vec::new();
    for cube in &part.cubes {
        bake_cube(cube, &Affine::IDENTITY, tw, th, &mut quads);
    }
    let index = out.len();
    out.push(BakedPart {
        name: name.to_string(),
        parent,
        rest: part.pose,
        quads,
    });
    for (child_name, child) in &part.children {
        collect_parts(child_name, child, Some(index), tw, th, out);
    }
}

/// Bakes an entity model into posed, UV-mapped quads.
pub fn bake_entity(model: &EntityModelDef) -> Vec<EntityQuad> {
    let mut out = Vec::new();
    bake_part(
        &model.root,
        Affine::IDENTITY,
        model.texture_width as f32,
        model.texture_height as f32,
        &mut out,
    );
    out
}

fn bake_part(part: &PartDef, parent: Affine, tw: f32, th: f32, out: &mut Vec<EntityQuad>) {
    let world = parent.compose(&part_transform(&part.pose));
    for cube in &part.cubes {
        bake_cube(cube, &world, tw, th, out);
    }
    for (_, child) in &part.children {
        bake_part(child, world, tw, th, out);
    }
}

/// Emits the visible faces of one box, faithful to `ModelPart.Cube`.
fn bake_cube(cube: &CubeDef, world: &Affine, tw: f32, th: f32, out: &mut Vec<EntityQuad>) {
    let [ox, oy, oz] = cube.origin;
    let [w, h, d] = cube.size;
    let (mut min_x, mut min_y, mut min_z) = (ox, oy, oz);
    let (mut max_x, mut max_y, mut max_z) = (ox + w, oy + h, oz + d);
    // Grow (CubeDeformation) inflates the box symmetrically.
    min_x -= cube.grow.x;
    min_y -= cube.grow.y;
    min_z -= cube.grow.z;
    max_x += cube.grow.x;
    max_y += cube.grow.y;
    max_z += cube.grow.z;
    if cube.mirror {
        std::mem::swap(&mut min_x, &mut max_x);
    }

    // Eight corners in model texels (named as in ModelPart.Cube: t = near z=min,
    // l = far z=max).
    let t0 = [min_x, min_y, min_z];
    let t1 = [max_x, min_y, min_z];
    let t2 = [max_x, max_y, min_z];
    let t3 = [min_x, max_y, min_z];
    let l0 = [min_x, min_y, max_z];
    let l1 = [max_x, min_y, max_z];
    let l2 = [max_x, max_y, max_z];
    let l3 = [min_x, max_y, max_z];

    // Texel unwrap offsets.
    let xo = cube.tex_offset[0];
    let yo = cube.tex_offset[1];
    let u0 = xo;
    let u1 = xo + d;
    let u2 = xo + d + w;
    let u22 = xo + d + w + w;
    let u3 = xo + d + w + d;
    let u4 = xo + d + w + d + w;
    let v0 = yo;
    let v1 = yo + d;
    let v2 = yo + d + h;

    let x_tex_size = tw * cube.tex_scale[0];
    let y_tex_size = th * cube.tex_scale[1];

    // (verts, uMinTex, vMinTex, uMaxTex, vMaxTex) per face, exactly as vanilla.
    type FaceSpec = ([[f32; 3]; 4], f32, f32, f32, f32);
    let faces: [FaceSpec; 6] = [
        ([l1, l0, t0, t1], u1, v0, u2, v1),  // DOWN
        ([t2, t3, l3, l2], u2, v1, u22, v0), // UP
        ([t0, l0, l3, t3], u0, v1, u1, v2),  // WEST
        ([t1, t0, t3, t2], u1, v1, u2, v2),  // NORTH
        ([l1, t1, t2, l2], u2, v1, u3, v2),  // EAST
        ([l0, l1, l2, l3], u3, v1, u4, v2),  // SOUTH
    ];

    for (fi, dir) in FACE_ORDER.iter().enumerate() {
        if !cube.visible_faces[fi] {
            continue;
        }
        let (verts, umin, vmin, umax, vmax) = faces[fi];
        // Polygon remap: [0]=(uMax,vMin) [1]=(uMin,vMin) [2]=(uMin,vMax) [3]=(uMax,vMax).
        let mut positions = [
            world.apply([verts[0][0] / 16.0, verts[0][1] / 16.0, verts[0][2] / 16.0]),
            world.apply([verts[1][0] / 16.0, verts[1][1] / 16.0, verts[1][2] / 16.0]),
            world.apply([verts[2][0] / 16.0, verts[2][1] / 16.0, verts[2][2] / 16.0]),
            world.apply([verts[3][0] / 16.0, verts[3][1] / 16.0, verts[3][2] / 16.0]),
        ];
        let mut uvs = [
            [umax / x_tex_size, vmin / y_tex_size],
            [umin / x_tex_size, vmin / y_tex_size],
            [umin / x_tex_size, vmax / y_tex_size],
            [umax / x_tex_size, vmax / y_tex_size],
        ];
        if cube.mirror {
            positions.reverse();
            uvs.reverse();
        }
        let n = face_normal(*dir, cube.mirror);
        let normal = normalize(world.apply_linear(n));
        out.push(EntityQuad {
            positions,
            uvs,
            normal,
            direction: *dir,
        });
    }
}

fn face_normal(dir: Direction, mirror: bool) -> [f32; 3] {
    let n = match dir {
        Direction::Down => [0.0, -1.0, 0.0],
        Direction::Up => [0.0, 1.0, 0.0],
        Direction::North => [0.0, 0.0, -1.0],
        Direction::South => [0.0, 0.0, 1.0],
        Direction::West => [-1.0, 0.0, 0.0],
        Direction::East => [1.0, 0.0, 0.0],
    };
    // Vanilla flips only X-axis facings when mirrored.
    if mirror && matches!(dir, Direction::East | Direction::West) {
        [-n[0], n[1], n[2]]
    } else {
        n
    }
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len <= f32::EPSILON {
        v
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

/// `HumanoidModel.OVERLAY_SCALE` — the grow every player skin overlay cube
/// (jacket, both sleeves, both pants legs) sits at relative to its base-layer
/// cube in [`player_model`].
///
/// Named separately from [`crate::equipment::OUTER_ARMOUR_INFLATION`] (`1.0`)
/// and [`crate::equipment::INNER_ARMOUR_INFLATION`] (`0.5`) on purpose: vanilla
/// draws the skin's own second layer and a worn armour piece at two
/// *different* inflations precisely so the two can never be coincident and
/// z-fight, and a port that let them collapse to the same number would
/// reintroduce exactly that. See `player_overlay_and_armour_inflations_differ`.
pub const PLAYER_OVERLAY_INFLATION: f32 = 0.25;

/// `HumanoidModel.HAT_OVERLAY_SCALE` — the player skin's `hat` cube's own grow
/// (a child of `head`, so this is its *own* extra grow on top of whatever the
/// head cube itself carries — `0.0` for the bare skin in [`player_model`],
/// see [`crate::equipment::HAT_OVERLAY_INFLATION`] for the armour-mesh case
/// where the head cube itself is already grown).
pub const PLAYER_HAT_OVERLAY_INFLATION: f32 = 0.5;

/// The vanilla player model (`net/minecraft/client/model/player/PlayerModel`)
/// on a 64×64 sheet, as the concrete case that exercises the whole primitive:
/// pivots, overlay layers (`grow`), and the wide-vs-slim arm variants.
///
/// `slim` selects the 3-wide-arm ("Alex") variant; `false` is the classic
/// 4-wide ("Steve") model. Texel offsets and poses are the vanilla values.
pub fn player_model(slim: bool) -> EntityModelDef {
    let mut root = PartDef::new(PartPose::ZERO);

    // Head + hat overlay (grow 0.5).
    let head = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_cube(CubeDef::new(
            [-4.0, -8.0, -4.0],
            [8.0, 8.0, 8.0],
            [0.0, 0.0],
        ))
        .with_child(
            "hat",
            PartDef::new(PartPose::ZERO).with_cube(
                CubeDef::new([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [32.0, 0.0]).grown(PLAYER_HAT_OVERLAY_INFLATION),
            ),
        );
    root = root.with_child("head", head);

    // Body + jacket overlay.
    let body = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_cube(CubeDef::new(
            [-4.0, 0.0, -2.0],
            [8.0, 12.0, 4.0],
            [16.0, 16.0],
        ))
        .with_child(
            "jacket",
            PartDef::new(PartPose::ZERO).with_cube(
                CubeDef::new([-4.0, 0.0, -2.0], [8.0, 12.0, 4.0], [16.0, 32.0]).grown(PLAYER_OVERLAY_INFLATION),
            ),
        );
    root = root.with_child("body", body);

    // Arms differ between wide and slim — in **two** numbers, not one.
    //
    // `PlayerModel.createMesh` (26.2) replaces the arms wholesale rather than
    // narrowing them in place:
    //
    // ```text
    // slim  right_arm: addBox(-2, -2, -2, 3, 12, 4)   left_arm: addBox(-1, -2, -2, 3, 12, 4)
    // wide  right_arm: addBox(-3, -2, -2, 4, 12, 4)   left_arm: addBox(-1, -2, -2, 4, 12, 4)
    //       ^ from HumanoidModel.createMesh
    // ```
    //
    // The **left** arm keeps origin `-1` in both, so narrowing alone is right
    // there. The **right** arm's origin moves with the width, because the edge
    // that must stay put is the one against the body (`origin + width == +1`
    // relative to the pivot in both cases) — and this port had `-3` for both,
    // which put the slim right arm a pixel out from the shoulder with a
    // one-pixel gap beside the body. It was invisible until the slim rig became
    // reachable at all: nothing in this workspace ever selected it.
    let arm_w = if slim { 3.0 } else { 4.0 };
    let right_arm_x = 1.0 - arm_w;
    let right_arm = PartDef::new(PartPose::offset(-5.0, 2.0, 0.0))
        .with_cube(CubeDef::new(
            [right_arm_x, -2.0, -2.0],
            [arm_w, 12.0, 4.0],
            [40.0, 16.0],
        ))
        .with_child(
            "right_sleeve",
            PartDef::new(PartPose::ZERO).with_cube(
                CubeDef::new([right_arm_x, -2.0, -2.0], [arm_w, 12.0, 4.0], [40.0, 32.0])
                    .grown(PLAYER_OVERLAY_INFLATION),
            ),
        );
    let left_arm = PartDef::new(PartPose::offset(5.0, 2.0, 0.0))
        .with_cube(CubeDef::new(
            [-1.0, -2.0, -2.0],
            [arm_w, 12.0, 4.0],
            [32.0, 48.0],
        ))
        .with_child(
            "left_sleeve",
            PartDef::new(PartPose::ZERO).with_cube(
                CubeDef::new([-1.0, -2.0, -2.0], [arm_w, 12.0, 4.0], [48.0, 48.0]).grown(PLAYER_OVERLAY_INFLATION),
            ),
        );
    root = root.with_child("right_arm", right_arm);
    root = root.with_child("left_arm", left_arm);

    // Legs + overlay.
    let right_leg = PartDef::new(PartPose::offset(-1.9, 12.0, 0.0))
        .with_cube(CubeDef::new(
            [-2.0, 0.0, -2.0],
            [4.0, 12.0, 4.0],
            [0.0, 16.0],
        ))
        .with_child(
            "right_pants",
            PartDef::new(PartPose::ZERO).with_cube(
                CubeDef::new([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 32.0]).grown(PLAYER_OVERLAY_INFLATION),
            ),
        );
    let left_leg = PartDef::new(PartPose::offset(1.9, 12.0, 0.0))
        .with_cube(CubeDef::new(
            [-2.0, 0.0, -2.0],
            [4.0, 12.0, 4.0],
            [16.0, 48.0],
        ))
        .with_child(
            "left_pants",
            PartDef::new(PartPose::ZERO).with_cube(
                CubeDef::new([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 48.0]).grown(PLAYER_OVERLAY_INFLATION),
            ),
        );
    root = root.with_child("right_leg", right_leg);
    root = root.with_child("left_leg", left_leg);

    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The player's cape overlay (`PlayerCapeModel.createCapeLayer`, 26.2): a
/// single 10×16×1 box hanging off the **body** pivot, on its own 64×64 sheet
/// (the model's declared unwrap size, not the cape PNG's real 64×32 —
/// `LayerDefinition.create(mesh, 64, 64)` says so explicitly even though the
/// texture itself is shorter).
///
/// Vanilla nests this under a `PlayerModel.createMesh` copy and clears every
/// other cube (`root.clearRecursively()`), so the only shared coordinate frame
/// that matters is the **body pivot** — reproduced here as a bare identity
/// `"body"` part, matching [`player_model`]'s own `body` pose exactly, so a
/// caller can pair this mesh's `"cape"` part against the wearer's `"body"`
/// part transform the same way armour pairs against named body parts.
///
/// The cube itself is baked **without** `PlayerCapeModel`'s static
/// `PartPose.offsetAndRotation(0, 0, 2, 0, PI, 0)` rotation folded in — only
/// the `z = 2` translation. That rotation is not lost: `CapeLayer`/
/// `PlayerCapeModel.setupAnim` immediately calls `cape.rotateBy(new
/// Quaternionf().rotateY(-PI)...)`, and composing `oldRotation.rotate(newQuat)`
/// (`ModelPart.rotateBy`) makes the static `Ry(PI)` and the quaternion's
/// leading `Ry(-PI)` term cancel exactly — the two are inverses on the same
/// axis. What survives is `Rx(theta_x) * Rz(theta_z) * Ry(theta_y2)`, which is
/// exactly what `lodestone_render::entity::cape_local_rotation` computes at
/// draw time; baking the now-cancelled static rotation into the geometry here
/// would double it. See that function's doc for the full per-frame formula.
pub fn player_cape_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::ZERO).with_child(
        "cape",
        PartDef::new(PartPose::offset(0.0, 0.0, 2.0)).with_cube(CubeDef::new(
            [-5.0, 0.0, -1.0],
            [10.0, 16.0, 1.0],
            [0.0, 0.0],
        )),
    );
    let root = PartDef::new(PartPose::ZERO).with_child("body", body);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// In-pack path of the elytra's own wing texture — the `wings` layer of
/// `assets/minecraft/equipment/elytra.json`, whose sole entry is
/// `{"texture": "minecraft:elytra", "use_player_texture": true}`.
///
/// This is a bare constant rather than an [`crate::equipment::ArmourLayerType`]
/// variant on purpose. `wings` is a real `EquipmentClientInfo.LayerType`, but
/// adding it to that enum would widen the *armour* layer-type space that
/// `armour_layers` and the trim sprite id are keyed on, and an elytra has
/// neither armour layers nor a trim. One texture, one constant.
///
/// `use_player_texture` is what lets a player's own cape (or a dedicated
/// elytra texture) replace this — `WingsLayer.getPlayerElytraTexture` prefers
/// `skin.elytra()`, then `skin.cape()` when the cape is shown, and falls back
/// to this. [`crate::skin::ProfileTextures`] already parses both URLs.
pub const ELYTRA_TEXTURE_PATH: &str =
    "assets/minecraft/textures/entity/equipment/wings/elytra.png";

/// The elytra's wings (`ElytraModel.createLayer`, 26.2): two 10x20x2 boxes
/// inflated by 1.0, sharing one `(22, 0)` unwrap on a **64x32** sheet, hung
/// off the wearer's **body** pivot.
///
/// Structured exactly as [`player_cape_model`] is, and for the same reason —
/// a bare identity `"body"` part so a caller can pair the wing parts against
/// the wearer's own `"body"` transform. The mirrored right wing is what makes
/// one unwrap serve both sides.
///
/// # Why no rotation is baked, and why that is *not* the cape's reason
///
/// `createLayer` gives each wing a static
/// `PartPose.offsetAndRotation(±5, 0, 0, PI/12, 0, ∓PI/12)`, and only the
/// **offset** is reproduced here. The cape drops its static rotation because
/// `setupAnim` *composes* an inverse onto it and the two cancel; this model
/// drops its static rotation for the opposite mechanical reason —
/// `ElytraModel.setupAnim` **assigns** `xRot`/`yRot`/`zRot` outright
/// (`this.leftWing.xRot = state.elytraRotX;`), so the authored rotation is
/// overwritten on every frame that runs and is never composed with anything.
///
/// The two conclusions coincide and the reasons do not, which is worth
/// stating: a reader who ports this by analogy with the cape gets the right
/// answer for the wrong reason, and would then get `y` wrong — `setupAnim`
/// also assigns `y` (3.0 when crouching, 0.0 otherwise) while leaving `x` and
/// `z` alone, so the `±5` **must** be baked and the `y` **must not** be.
///
/// The rest-pose angles are not lost either: `ElytraAnimationState`'s
/// not-flying, not-crouching target is `(PI/12, 0, -PI/12)`, the same triple,
/// which is why a standing player's wings look like the authored pose. See
/// `lodestone_render::entity::elytra_rest_rotations`.
#[must_use]
pub fn elytra_model() -> EntityModelDef {
    // One `texOffs(22, 0)` box per wing. The right wing is the mirrored copy,
    // and its origin is `0` where the left's is `-10`: mirroring flips the X
    // extent, so both describe the same 10-wide box on opposite sides of
    // their own pivot.
    let wing_grow = 1.0;
    let left_wing = PartDef::new(PartPose::offset(5.0, 0.0, 0.0)).with_cube(
        CubeDef::new([-10.0, 0.0, 0.0], [10.0, 20.0, 2.0], [22.0, 0.0]).grown(wing_grow),
    );
    let right_wing = PartDef::new(PartPose::offset(-5.0, 0.0, 0.0)).with_cube(
        CubeDef::new([0.0, 0.0, 0.0], [10.0, 20.0, 2.0], [22.0, 0.0])
            .grown(wing_grow)
            .mirrored(),
    );
    let body = PartDef::new(PartPose::ZERO)
        .with_child("left_wing", left_wing)
        .with_child("right_wing", right_wing);
    let root = PartDef::new(PartPose::ZERO).with_child("body", body);
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

#[cfg(test)]
mod player_model_tests {
    use super::*;

    /// Finds a named descendant part, depth-first.
    fn part<'a>(root: &'a PartDef, name: &str) -> Option<&'a PartDef> {
        for (n, child) in &root.children {
            if n == name {
                return Some(child);
            }
            if let Some(found) = part(child, name) {
                return Some(found);
            }
        }
        None
    }

    /// Every arm box in **both** rigs, against `PlayerModel.createMesh`'s own
    /// literals (26.2, with the wide right arm coming from
    /// `HumanoidModel.createMesh`).
    ///
    /// The load-bearing row is the **slim right arm's origin, `-2` and not
    /// `-3`**: the left arm keeps origin `-1` in both rigs, so "slim just
    /// narrows the arms" is true of the left and false of the right, and the
    /// wrong version put the slim right arm a pixel outboard with a gap at the
    /// shoulder. This table is transcribed from the jar, not derived from the
    /// implementation, so `right_arm_x = 1.0 - arm_w` is checked against
    /// vanilla's two independent literals rather than against itself.
    #[test]
    fn both_player_rigs_arms_match_the_vanilla_mesh_definition() {
        for (slim, want_w, want_right_x) in [(false, 4.0_f32, -3.0_f32), (true, 3.0, -2.0)] {
            let def = player_model(slim);
            let checks: [(&str, f32); 4] = [
                ("right_arm", want_right_x),
                ("right_sleeve", want_right_x),
                // The left arm's origin does **not** move with the width.
                ("left_arm", -1.0),
                ("left_sleeve", -1.0),
            ];
            for (name, want_x) in checks {
                let p = part(&def.root, name).unwrap_or_else(|| panic!("{name} missing"));
                let cube = p.cubes.first().unwrap_or_else(|| panic!("{name} has no cube"));
                assert!(
                    (cube.origin[0] - want_x).abs() < 1e-6,
                    "slim={slim} {name} origin.x is {} but vanilla says {want_x}",
                    cube.origin[0]
                );
                assert!(
                    (cube.size[0] - want_w).abs() < 1e-6,
                    "slim={slim} {name} width is {} but vanilla says {want_w}",
                    cube.size[0]
                );
                // The edge against the body is the invariant behind the moving
                // origin: for the right arm it is `origin + width == +1`, and it
                // must hold for both rigs. This is the *reason* the two literals
                // above differ, asserted rather than left as a comment.
                if name.starts_with("right") {
                    assert!(
                        (cube.origin[0] + cube.size[0] - 1.0).abs() < 1e-6,
                        "slim={slim} {name}'s inner edge moved off +1"
                    );
                }
            }
            // Legs and body never differ between the rigs — the control on the
            // branch's scope, so a future edit cannot narrow something else too.
            for name in ["right_leg", "left_leg", "body", "head"] {
                let p = part(&def.root, name).unwrap_or_else(|| panic!("{name} missing"));
                let cube = p.cubes.first().unwrap();
                let wide = player_model(false);
                let other = part(&wide.root, name).unwrap().cubes[0].clone();
                assert_eq!(cube.origin, other.origin, "{name} origin differs by rig");
                assert_eq!(cube.size, other.size, "{name} size differs by rig");
            }
        }
    }
}
