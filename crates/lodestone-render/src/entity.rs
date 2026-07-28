//! The entity render plan: the version-free layer that turns *tracked entities*
//! into *a culled, instanced set of draws* for one frame.
//!
//! Entities are the second geometry pipeline (terrain being the first). They are
//! fundamentally different from blocks and reuse none of the mesher:
//!
//! * Their geometry is a **cuboid part hierarchy**, not paletted voxels, and it
//!   is already baked version-free by [`lodestone_assets::entity::bake_entity`]
//!   from the hand-ported [`entity_models`](lodestone_assets::entity_models)
//!   corpus. This module never re-derives geometry; it converts the baked quads
//!   into the shared [`ModelVertex`] format once per model type.
//! * They are **instanced**: a mob farm is hundreds of the same model at
//!   different transforms, so the mesh is uploaded once and each visible entity
//!   contributes only a per-instance model matrix (see
//!   [`crate::entity_pipeline`]). This module produces those matrices.
//!
//! # The placement transform (why it is exactly this and not eyeballed)
//!
//! Vanilla places a living entity with a fixed sequence of pose-stack ops in
//! `LivingEntityRenderer.render`, read here from the decompiled 26.2 client:
//!
//! ```text
//!   translate(feetPos)                     // EntityRenderDispatcher
//!   rotateY(180° - bodyYaw)                // setupRotations
//!   scale(-1, -1, 1)                       // model space is Y-down, Z-forward
//!   translate(0, -1.501, 0)                // lift feet to the ground plane
//! ```
//!
//! Model space has **Y pointing down** (the head cube spans `y ∈ [-8, 0]`, the
//! feet reach `y = +24`), which is why the `scale(-1, -1, 1)` flip is load
//! bearing rather than cosmetic: without it every mob renders upside down but
//! still recognisable, the exact "looks plausible, is wrong" trap. The
//! composition order is copied from source, not inferred, and
//! [`entity_model_matrix`] is unit-tested to put feet on the ground and the head
//! above them. The `scale(-1,-1,1)` has determinant `+1`, so it preserves
//! winding — a front face in model space stays a front face in world space, and
//! back-face culling remains valid.
//!
//! Per-part animation (head tracking, walk cycles) is a layer *above* this: it
//! adjusts each [`PartPose`](lodestone_assets::entity::PartPose) before baking.
//! This module renders the static rest pose posed only by body yaw, which is
//! what the incoming [`EntityView`](../../lodestone_client/state/struct.EntityView.html)
//! data supports today (position + rotation, no limb angles).

use glam::{Mat4, Vec3};
use lodestone_assets::entity::{EntityModelDef, bake_entity_parts};
use lodestone_assets::entity_models::{EntityModelEntry, entity_models};
use lodestone_assets::{BakedQuad, DisplayTransform, GuiLight};

use crate::camera::Frustum;
use crate::entity_anim::{AnimInput, HumanoidArms, Skeleton};
use crate::item_render::display_matrix;
use crate::models::{ModelMesh, ModelVertex, mesh_item_quads};

/// The vanilla feet-to-model lift (`LivingEntityRenderer`'s
/// `translate(0, -1.501, 0)`), in blocks.
pub const MODEL_FEET_OFFSET: f32 = 1.501;

/// Packed sky/block light meaning "full sky, no block light" (sky in the high
/// nibble), the value an entity carries when the caller has no world to sample.
///
/// This is a **fallback, not the normal path**. Vanilla samples the lightmap
/// once per entity at its block position
/// (`LivingEntityRenderer` → `Level::getLightColor`), which is why light is one
/// byte per *instance* ([`EntityInstance::light`]) and not per vertex: a mob is
/// uniformly lit by the block it stands in. A caller that has a world supplies
/// the real byte via [`EntityInstance::with_light`] or
/// [`EntitySpawn::light`]; one that does not (the offline demo, a mesh-only
/// test) gets this and renders as it always did.
pub const ENTITY_FULLBRIGHT: u8 = 15 << 4;

/// Look up the ported entity model for a canonical entity-type path (the
/// `path()` of an entity type key, e.g. `"pig"` from `minecraft:pig`).
///
/// Returns the matching [`EntityModelEntry`] from the version-free
/// [`entity_models`] corpus, or `None` if we have no model for that type yet —
/// in which case the renderer skips the entity rather than substituting a wrong
/// mesh.
#[must_use]
pub fn model_for_type(type_path: &str) -> Option<EntityModelEntry> {
    let name = canonical_model_name(type_path)?;
    entity_models().into_iter().find(|e| e.name == name)
}

/// The corpus entry names, cached so the per-entity, per-frame
/// [`canonical_model_name`] lookup does not rebuild the whole `entity_models()`
/// vector. The corpus is a compile-time constant set, so caching it can never go
/// stale.
fn corpus_names() -> &'static [&'static str] {
    static NAMES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| entity_models().into_iter().map(|e| e.name).collect())
}

/// Maps an entity-type path to the `name` of the [`entity_models`] entry that
/// renders it.
///
/// **The corpus is the source of truth**: a type path that names a corpus entry
/// resolves to *that* entry, and only the handful of types whose registry path
/// differs from the model name are listed here. The inverse — an explicit table
/// enumerating every drawable type — is what shipped the "a drowned renders as
/// an ordinary zombie" defect: `drowned` was aliased onto `zombie` back when the
/// corpus had no drowned mesh, and the alias outlived the mesh's arrival by the
/// whole tier-3 port. Deriving identity from the corpus means a newly ported mob
/// is drawable the moment its mesh lands, and a wrong-mesh substitution has to be
/// *written down* rather than left behind.
///
/// The aliases that remain are genuine "vanilla renders this type with another
/// mob's model class" cases, not placeholders.
fn canonical_model_name(type_path: &str) -> Option<&'static str> {
    match type_path {
        // `PlayerRenderer` picks a skin model; wide/`steve` is the default.
        "player" => return Some("player_wide"),
        // `BoggedModel` (a skeleton with mushrooms) is not ported yet; the plain
        // skeleton is the closest ported mesh. Unlike the drowned alias this is
        // deliberate and outlives no mesh — remove it when `bogged` is ported.
        "bogged" => return Some("skeleton"),
        _ => {}
    }
    corpus_names().iter().copied().find(|n| *n == type_path)
}

/// Which humanoid arm rig a model animates with — the render-crate side of
/// vanilla's `AbstractZombieModel` overriding `HumanoidModel`'s arm swing.
///
/// [`AnimFamily`](crate::entity_anim::AnimFamily) is classified *structurally*
/// from part names, on purpose (see that module's docs). A zombie's skeleton is
/// part-for-part identical to a player's, so no structural rule can separate
/// them: the distinction is which Java class vanilla instantiates. That fact is
/// a name mapping, so it lives here next to [`canonical_model_name`] — the
/// module that already owns "which vanilla class draws this mob" — rather than
/// being smuggled into the structural classifier.
#[must_use]
pub fn humanoid_arms_for(model_name: &str) -> HumanoidArms {
    match model_name {
        // `ZombieModel`, `DrownedModel` and `ZombieVillagerModel` all call
        // `AnimationUtils.animateZombieArms` after `super.setupAnim`.
        "zombie" | "husk" | "drowned" | "zombie_villager" => HumanoidArms::Zombie,
        _ => HumanoidArms::Swinging,
    }
}

/// The in-jar sheet path for a corpus texture reference (`"entity/zombie/zombie"`
/// → `"assets/minecraft/textures/entity/zombie/zombie.png"`).
fn sheet_path(reference: &str) -> &'static str {
    Box::leak(format!("assets/minecraft/textures/{reference}.png").into_boxed_str())
}

/// The in-jar texture path(s) for a model, in priority order — the first that
/// the resource pack actually contains wins. Version-free: these are vanilla
/// resource-pack paths keyed by the model name [`canonical_model_name`]
/// produces, not protocol data.
///
/// Biome/variant-correct selection (a cold pig, a black horse) is a refinement:
/// this returns each entry's canonical sheet, which is the `_temperate` skin for
/// the mobs 26.2 split by climate. Returns an empty slice for a model with no
/// known sheet, so the caller falls back to a placeholder rather than failing.
///
/// **Derived from the corpus, not hand-listed.** Each entry already carries its
/// own [`EntityTexture`](lodestone_assets::entity::EntityTexture); a second
/// hand-written table here can only ever drift out of step with it, and did:
/// `drowned` had `entity/zombie/drowned` in the corpus while this table knew
/// only nine models. The per-model paths are interned once (the corpus is a
/// fixed compile-time set of ~90 entries) so the `&'static` signature holds.
#[must_use]
pub fn entity_texture_candidates(model_name: &str) -> &'static [&'static str] {
    static SHEETS: std::sync::OnceLock<Vec<(&'static str, &'static [&'static str])>> =
        std::sync::OnceLock::new();
    let sheets = SHEETS.get_or_init(|| {
        entity_models()
            .into_iter()
            .map(|entry| {
                let reference = entry.texture.default_path();
                let mut paths = vec![sheet_path(reference)];
                // 26.2 split several farm mobs into `_temperate`/`_cold`/`_warm`
                // and removed the bare sheet; older packs ship only the bare one.
                // Listing the legacy name second resolves both without this crate
                // learning a version.
                if let Some(legacy) = reference.strip_suffix("_temperate") {
                    paths.push(sheet_path(legacy));
                }
                let paths: &'static [&'static str] = Box::leak(paths.into_boxed_slice());
                (entry.name, paths)
            })
            .collect()
    });
    sheets
        .iter()
        .find(|(n, _)| *n == model_name)
        .map_or(&[], |(_, paths)| *paths)
}

/// A CPU entity mesh in the shared wide [`ModelVertex`] format, plus the model's
/// local-space bounding box for culling.
///
/// One of these is built per *model type* (not per entity) and uploaded once;
/// every instance of that type reuses it. Positions are in the baked model frame
/// (blocks, Y-down): the per-instance [`entity_model_matrix`] moves them into the
/// world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartRange {
    /// First index belonging to this part.
    pub index_start: u32,
    /// Number of indices in this part.
    pub index_count: u32,
    /// First vertex belonging to this part.
    pub vertex_start: u32,
    /// Number of vertices in this part.
    pub vertex_count: u32,
}

/// A baked entity model split into animatable parts.
///
/// Vertices are in **part-local** space: the part's own rest pose is *not* folded
/// in, so a joint stays a joint. Multiplying a part's vertices by its matrix from
/// [`Skeleton::rest_pose`] reproduces the whole-model bake exactly — asserted
/// over the entire corpus by `lodestone-assets`' `part_bake_recomposes_to_the_
/// whole_model_bake`. That equivalence is what lets the renderer keep one static
/// vertex buffer per model *type* and move only matrices per frame.
#[derive(Debug, Clone)]
pub struct EntityMesh {
    /// Four vertices per quad, in the shared model-vertex format, part-local.
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad, wound so front faces point outward.
    pub indices: Vec<u32>,
    /// One index sub-range per part, in [`Skeleton`] part order.
    pub parts: Vec<PartRange>,
    /// The part hierarchy and its animator.
    pub skeleton: Skeleton,
    /// Local-space AABB minimum (model frame, blocks), at rest.
    pub local_min: Vec3,
    /// Local-space AABB maximum (model frame, blocks), at rest.
    pub local_max: Vec3,
}

impl EntityMesh {
    /// Bake a model definition into a renderable mesh.
    ///
    /// Each [`EntityQuad`](lodestone_assets::entity::EntityQuad) becomes four
    /// [`ModelVertex`] and six indices. The winding is chosen per quad from the
    /// baked outward normal so front faces are counter-clockwise (matching the
    /// pipeline's `Ccw`/back-cull), independent of the quad's corner order or
    /// mirror flag.
    #[must_use]
    pub fn from_model(def: &EntityModelDef) -> Self {
        Self::from_named_model("", def)
    }

    /// Bake a model definition into a renderable mesh, applying the arm rig
    /// [`humanoid_arms_for`] assigns to `model_name`.
    ///
    /// The name has to be known *here* rather than at pose time because a zombie
    /// rig moves the arms in its **resting** pose, and the mesh's local AABB is
    /// taken from that resting pose. Choosing the rig later would leave every
    /// zombie with a culling box drawn around a mob standing to attention while
    /// the drawn one has its arms out in front — the classic "correct until it
    /// clips at the screen edge" bug.
    #[must_use]
    pub fn from_named_model(model_name: &str, def: &EntityModelDef) -> Self {
        let baked = bake_entity_parts(def);
        let skeleton =
            Skeleton::from_parts(&baked).with_humanoid_arms(humanoid_arms_for(model_name));
        let rest = skeleton.rest_pose();

        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::with_capacity(baked.len());
        let mut local_min = Vec3::splat(f32::INFINITY);
        let mut local_max = Vec3::splat(f32::NEG_INFINITY);

        for (part_index, part) in baked.iter().enumerate() {
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            // The rest matrix is used only for the local AABB: the vertices
            // themselves stay part-local so the animator can rotate the joint.
            let rest_m = rest[part_index];
            for quad in &part.quads {
                let base = vertices.len() as u32;
                for i in 0..4 {
                    let p = quad.positions[i];
                    let pos = Vec3::from(p);
                    let posed = rest_m.transform_point3(pos);
                    local_min = local_min.min(posed);
                    local_max = local_max.max(posed);
                    vertices.push(ModelVertex {
                        position: p,
                        uv: quad.uvs[i],
                        ao: 1.0,
                        // The entity shader does **not** read this byte: entity
                        // light is per *instance* (one lightmap sample per mob,
                        // as vanilla does), so it arrives on the instance
                        // buffer, not here. The field is filled anyway because
                        // the vertex layout is shared with terrain, and a
                        // full-bright value keeps a mis-wired reader honest
                        // rather than rendering every mob black.
                        light: ENTITY_FULLBRIGHT,
                        tint: 255,
                        anim: 0,
                        _pad: 0,
                    });
                }
                // Wind the two triangles so the geometric normal agrees with the
                // baked outward normal; otherwise back-face culling would drop
                // the visible side.
                let n = Vec3::from(quad.normal);
                let p0 = Vec3::from(quad.positions[0]);
                let p1 = Vec3::from(quad.positions[1]);
                let p2 = Vec3::from(quad.positions[2]);
                let facing = (p1 - p0).cross(p2 - p0).dot(n);
                if facing >= 0.0 {
                    indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base,
                        base + 2,
                        base + 3,
                    ]);
                } else {
                    indices.extend_from_slice(&[
                        base,
                        base + 2,
                        base + 1,
                        base,
                        base + 3,
                        base + 2,
                    ]);
                }
            }
            parts.push(PartRange {
                index_start,
                index_count: indices.len() as u32 - index_start,
                vertex_start,
                vertex_count: vertices.len() as u32 - vertex_start,
            });
        }

        if indices.is_empty() {
            local_min = Vec3::ZERO;
            local_max = Vec3::ZERO;
        }

        EntityMesh {
            vertices,
            indices,
            parts,
            skeleton,
            local_min,
            local_max,
        }
    }

    /// Number of quads in the mesh.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }
}

/// The world placement transform for a standing mob, matching vanilla's
/// `LivingEntityRenderer` pose-stack order exactly (see the module docs).
///
/// `feet` is the entity's world position (its feet, as the protocol reports it),
/// `body_yaw_deg` its body yaw in degrees (Minecraft convention: `0` faces `+Z`),
/// and `scale` a uniform size multiplier (`1.0` for a normal adult; babies and
/// scaled mobs pass a smaller value). Applying this to a baked model vertex
/// yields its world position.
#[must_use]
pub fn entity_model_matrix(feet: Vec3, body_yaw_deg: f32, scale: f32) -> Mat4 {
    let translate_feet = Mat4::from_translation(feet);
    let rotate = Mat4::from_rotation_y((180.0 - body_yaw_deg).to_radians());
    // scale(-1,-1,1) folded with the uniform entity scale.
    let flip_scale = Mat4::from_scale(Vec3::new(-scale, -scale, scale));
    let lift = Mat4::from_translation(Vec3::new(0.0, -MODEL_FEET_OFFSET, 0.0));
    translate_feet * rotate * flip_scale * lift
}

/// A single entity to render: which model type draws it, its world transform,
/// and its world-space AABB for frustum culling.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityInstance {
    /// The [`entity_models`] entry name that supplies this entity's mesh.
    pub model: &'static str,
    /// The per-instance model → world matrix (whole-entity placement).
    pub transform: Mat4,
    /// One `entity → world` matrix per skeleton part, in mesh part order:
    /// `transform * part_matrix`. Drawing part `p`'s index range instanced over
    /// `part_transforms[p]` is what makes a limb swing.
    pub part_transforms: Vec<Mat4>,
    /// World-space AABB minimum.
    pub aabb_min: Vec3,
    /// World-space AABB maximum.
    pub aabb_max: Vec3,
    /// Packed sky/block light (`sky << 4 | block`, `0..=15` each) sampled once
    /// at this entity's block position, exactly as vanilla samples it — one
    /// value for the whole mob, not per vertex. Defaults to
    /// [`ENTITY_FULLBRIGHT`]; set the real world value with
    /// [`with_light`](Self::with_light).
    pub light: u8,
}

impl EntityInstance {
    /// Build an instance for `model` at `feet`/`yaw`/`scale`, computing both the
    /// transform and a world AABB by transforming the model's local-bounds
    /// corners. `local_min`/`local_max` come from the model's [`EntityMesh`].
    #[must_use]
    pub fn new(
        model: &'static str,
        mesh: &EntityMesh,
        feet: Vec3,
        yaw_deg: f32,
        scale: f32,
        anim: &AnimInput,
    ) -> Self {
        let transform = entity_model_matrix(feet, yaw_deg, scale);
        let (aabb_min, aabb_max) = transformed_aabb(&transform, mesh.local_min, mesh.local_max);
        let part_transforms = mesh
            .skeleton
            .pose(anim)
            .into_iter()
            .map(|part| transform * part)
            .collect();
        EntityInstance {
            model,
            transform,
            part_transforms,
            aabb_min,
            aabb_max,
            light: ENTITY_FULLBRIGHT,
        }
    }

    /// Set this instance's packed sky/block light (`sky << 4 | block`).
    ///
    /// Builder-style rather than a seventh argument to [`new`](Self::new)
    /// because the great majority of call sites (mesh tests, the offline demo)
    /// have no world to sample and want the [`ENTITY_FULLBRIGHT`] default; only
    /// a caller wired to a real light source has anything to pass.
    #[must_use]
    pub fn with_light(mut self, light: u8) -> Self {
        self.light = light;
        self
    }
}

/// The version-free description of one tracked entity to render this frame: the
/// minimal fields the render layer needs, deliberately decoupled from any
/// client or wire type.
///
/// This is the seam a live scene loop adapts its entities into — e.g. mapping
/// each `EntityView` from the client handle into one of these. Keeping it a
/// small borrow of a type-path string plus world placement means the render
/// crate never depends on the client or a protocol version: the caller owns the
/// mapping from *its* entity representation, and this crate owns everything from
/// a type path to pixels. That is why [`EntityModelSet::plan`] takes these
/// rather than an `EntityView` directly — the dependency would point the wrong
/// way (render → client), and this crate must stay usable headless.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntitySpawn<'a> {
    /// The entity type's resource path, e.g. `"pig"` (namespace stripped).
    pub type_path: &'a str,
    /// Feet position in world space — the entity's on-ground origin.
    pub feet: Vec3,
    /// Whole-body yaw in degrees. Head tracking and limb poses are a layer above
    /// this and are not yet applied, so this is the body facing.
    pub body_yaw_deg: f32,
    /// Uniform model scale: `1.0` for an adult; babies and slimes pass a smaller
    /// value. The caller owns the baby/variant → scale decision.
    pub scale: f32,
    /// Per-part animation drive: head tracking, walk phase, attack swing, age.
    /// Pass [`AnimInput::REST`] for a static pose.
    pub anim: AnimInput,
    /// Packed sky/block light (`sky << 4 | block`) at this entity's block
    /// position — the caller's one job on the lighting side, because only the
    /// caller has a world to sample. Pass [`ENTITY_FULLBRIGHT`] when there is
    /// no world (the offline demo); passing it *because it is convenient*
    /// against a live server is the "mobs are super bright" defect.
    pub light: u8,
}

/// corners and takes their component-wise min/max. Correct for the entity flip
/// and yaw rotation (an axis-aligned box stays conservative under rotation).
fn transformed_aabb(m: &Mat4, local_min: Vec3, local_max: Vec3) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for i in 0..8 {
        let corner = Vec3::new(
            if i & 1 == 0 { local_min.x } else { local_max.x },
            if i & 2 == 0 { local_min.y } else { local_max.y },
            if i & 4 == 0 { local_min.z } else { local_max.z },
        );
        let world = m.transform_point3(corner);
        min = min.min(world);
        max = max.max(world);
    }
    (min, max)
}

/// A version-free baked corpus of every entity model the renderer can draw,
/// baked once on the CPU (no GPU) so the local bounds needed for culling and the
/// meshes needed for upload live in one place.
///
/// The GPU side ([`crate::entity_pipeline`]) uploads one buffer per entry here;
/// this pure set is what makes type→instance resolution testable headlessly.
#[derive(Debug, Clone)]
pub struct EntityModelSet {
    models: Vec<(&'static str, EntityMesh)>,
}

impl Default for EntityModelSet {
    fn default() -> Self {
        Self::load()
    }
}

impl EntityModelSet {
    /// Bake every entry in the [`entity_models`] corpus into a renderable mesh.
    #[must_use]
    pub fn load() -> Self {
        let models = entity_models()
            .into_iter()
            .map(|entry| {
                (
                    entry.name,
                    EntityMesh::from_named_model(entry.name, &(entry.build)()),
                )
            })
            .collect();
        Self { models }
    }

    /// The baked mesh for a model name, if present.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&EntityMesh> {
        self.models.iter().find(|(n, _)| *n == name).map(|(_, m)| m)
    }

    /// Every `(name, mesh)` pair, in corpus order (for uploading each once).
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &EntityMesh)> {
        self.models.iter().map(|(n, m)| (*n, m))
    }

    /// Number of baked models.
    #[must_use]
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Resolve a tracked entity (its type path, feet position, body yaw and
    /// scale) into a renderable [`EntityInstance`], or `None` if its type has no
    /// model yet. This is the type→geometry seam: it consumes only version-free
    /// data (a type path string and world coordinates), never a wire type.
    #[must_use]
    pub fn resolve(
        &self,
        type_path: &str,
        feet: Vec3,
        yaw_deg: f32,
        scale: f32,
        anim: &AnimInput,
    ) -> Option<EntityInstance> {
        let name = canonical_model_name(type_path)?;
        let mesh = self.get(name)?;
        Some(EntityInstance::new(name, mesh, feet, yaw_deg, scale, anim))
    }

    /// Resolve, frustum-cull and group a set of tracked entities into an
    /// [`EntityFrame`] in one call — the one-shot entry point for a live scene
    /// loop, so a caller with a list of entities never has to hand-assemble the
    /// intermediate [`EntityInstance`] vector or call [`plan_entities`] itself.
    ///
    /// Each [`EntitySpawn`] whose `type_path` has a baked model becomes an
    /// instance; a type with no model yet (e.g. `ender_dragon`) is silently
    /// skipped, exactly as [`resolve`](Self::resolve) skips it, so an
    /// unsupported mob never aborts the frame. Survivors are culled and grouped
    /// by [`plan_entities`], producing one [`EntityBatch`] per visible model
    /// type. Note [`EntityCullStats::total`] counts entities that *have a model*
    /// — modelless types are dropped before culling, not counted as culled.
    ///
    /// # Instance-buffer contract (the GPU side of the seam)
    ///
    /// Each [`EntityBatch::transforms`] entry is a model→world [`Mat4`]. Upload a
    /// batch's transforms column-major with
    /// [`upload_instances`](crate::entity_pipeline::upload_instances); the entity
    /// shader reads each as the `mat4x4` spanning vertex locations 4–7 (four
    /// `Float32x4` columns, `step_mode: Instance`). Draw the uploaded mesh for
    /// [`EntityBatch::model`] instanced over that buffer, one instanced draw per
    /// batch. That is the whole contract — the caller supplies entities and a
    /// frustum; this crate owns type→mesh→matrix→pixels.
    #[must_use]
    pub fn plan<'a, I>(&self, spawns: I, frustum: &Frustum) -> EntityFrame
    where
        I: IntoIterator<Item = EntitySpawn<'a>>,
    {
        let instances: Vec<EntityInstance> = spawns
            .into_iter()
            .filter_map(|s| {
                self.resolve(s.type_path, s.feet, s.body_yaw_deg, s.scale, &s.anim)
                    .map(|i| i.with_light(s.light))
            })
            .collect();
        plan_entities(&instances, frustum)
    }
}

/// Per-frame entity culling accounting. Mirrors [`crate::scene::CullStats`]'s
/// anti-vacuity discipline: a frame that drew nothing, or culled nothing while a
/// populated set straddles the frustum, is a bug rather than a fast frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntityCullStats {
    /// Entities considered this frame.
    pub total: usize,
    /// Entities whose model is known *and* survived frustum culling.
    pub drawn: usize,
    /// Entities frustum-culled.
    pub culled_frustum: usize,
}

impl EntityCullStats {
    /// A frame is *meaningful* only if it both drew and culled something, so a
    /// benchmark or gate cannot pass by culling (or drawing) everything.
    #[must_use]
    pub fn is_meaningful(&self) -> bool {
        self.drawn > 0 && self.culled_frustum > 0
    }
}

/// One model type's visible instances for a frame: the model name and the
/// per-instance transforms to draw instanced.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityBatch {
    /// The [`entity_models`] entry name.
    pub model: &'static str,
    /// One whole-entity model → world matrix per visible instance.
    pub transforms: Vec<Mat4>,
    /// Per-part instance matrices: `parts[p][i]` places part `p` of instance
    /// `i`. Outer length equals the mesh's part count; every inner vector has
    /// one entry per visible instance, in the same order as `transforms`.
    pub parts: Vec<Vec<Mat4>>,
    /// One packed sky/block light byte per visible instance, in the same order
    /// as `transforms` — widened to `u32` because that is what the instance
    /// vertex attribute carries. The *same* slice is uploaded alongside every
    /// part's matrices: a mob's light is per entity, so each of its parts reads
    /// the identical value.
    pub lights: Vec<u32>,
}

/// The visible entity draws for one frame, grouped by model type, plus the
/// accounting that produced them.
#[derive(Debug, Clone, Default)]
pub struct EntityFrame {
    /// Visible instances grouped by model type (one [`EntityBatch`] per model
    /// that has at least one visible instance).
    pub batches: Vec<EntityBatch>,
    /// Culling accounting for this frame.
    pub stats: EntityCullStats,
}

impl EntityFrame {
    /// Total visible instances across all batches.
    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.batches.iter().map(|b| b.transforms.len()).sum()
    }
}

/// Cull a set of entity instances against `frustum` and group the survivors by
/// model type for instanced drawing.
///
/// Instances whose world AABB is outside the frustum are dropped; the rest are
/// bucketed by [`EntityInstance::model`] preserving first-seen model order, so
/// the caller issues one instanced draw per model type. Cost is `O(instances)`
/// frustum tests plus the grouping.
#[must_use]
pub fn plan_entities(instances: &[EntityInstance], frustum: &Frustum) -> EntityFrame {
    let mut batches: Vec<EntityBatch> = Vec::new();
    let mut stats = EntityCullStats {
        total: instances.len(),
        ..EntityCullStats::default()
    };

    for inst in instances {
        if !frustum.intersects_aabb(inst.aabb_min, inst.aabb_max) {
            stats.culled_frustum += 1;
            continue;
        }
        stats.drawn += 1;
        match batches.iter_mut().find(|b| b.model == inst.model) {
            Some(batch) => {
                batch.transforms.push(inst.transform);
                batch.lights.push(u32::from(inst.light));
                for (slot, m) in batch.parts.iter_mut().zip(&inst.part_transforms) {
                    slot.push(*m);
                }
            }
            None => batches.push(EntityBatch {
                model: inst.model,
                transforms: vec![inst.transform],
                parts: inst.part_transforms.iter().map(|m| vec![*m]).collect(),
                lights: vec![u32::from(inst.light)],
            }),
        }
    }

    EntityFrame { batches, stats }
}

// ---------------------------------------------------------------------------
// Dropped items
// ---------------------------------------------------------------------------
//
// A dropped item is an entity that is **not** a cuboid part rig, so none of the
// machinery above applies to it: it has no skeleton, no per-mob sheet, and no
// `entity_models` corpus entry. What it has is an *item model* — the same baked
// geometry [`BlockModels::item_quads`](crate::BlockModels::item_quads) already
// supplies for a hotbar icon — drawn in the world through the ordinary
// [`ModelPipeline`](crate::ModelPipeline) rather than the entity pipeline.
//
// This section owns the *placement*: where in the world that geometry goes, and
// how it bobs and spins. Transcribed from 26.2's `ItemEntityRenderer.submit`:
//
// ```text
//   AABB box     = state.item.getModelBoundingBox()      // the GROUND-posed model
//   minOffsetY   = -box.minY + 0.0625
//   bob          = sin(ageInTicks / 10 + bobOffs) * 0.1 + 0.1
//   translate(0, bob + minOffsetY, 0)
//   mulPose(Axis.YP.rotation(getSpin(ageInTicks, bobOffs)))   // radians
//   // then the item is drawn under its display.ground transform
// ```
//
// and `ItemEntity.getSpin(age, bobOffs) = age / 20 + bobOffs`.
//
// # The winding invariant, stated for a *world* pose
//
// The GUI item path composes `gui_ortho * gui_item_pose`, and each of those two
// matrices has a negative determinant so that the **product**'s determinant sign
// matches [`Camera::view_projection`](crate::Camera::view_projection)'s — which
// is itself negative, because `glam`'s DirectX right-handed perspective is.
// That is a statement about the *composed* matrix, and it does not transfer to
// this path.
//
// Here the pose is a **world-space model matrix** left-multiplied by the very
// same `Camera::view_projection`, exactly like a terrain section's. So the pose
// must not flip anything: its determinant has to be **positive**, and the
// composed `view_projection * pose` then inherits the camera's negative sign.
// Reading the GUI rule as "the pose determinant must be negative" and coding to
// it would ship an item you are looking at the *inside* of — which spins
// convincingly in a screenshot. `dropped_item_pose_preserves_winding` derives
// the reference sign from the camera rather than hardcoding either answer.

/// Vanilla's `ItemEntityRenderer.ITEM_MIN_HOVER_HEIGHT`: how far the lowest
/// point of the posed model floats above the entity's own position, in blocks.
pub const ITEM_MIN_HOVER_HEIGHT: f32 = 0.0625;

/// Vanilla's `ItemEntityRenderer.FLAT_ITEM_DEPTH_THRESHOLD`: a posed model
/// thinner than this in `z` is treated as a flat sprite and a stack of them is
/// fanned along `z` rather than jittered in three axes.
pub const FLAT_ITEM_DEPTH_THRESHOLD: f32 = 0.0625;

/// Bob amplitude in blocks (`… * 0.1F + 0.1F`), so the bob spans `0.0..=0.2`.
pub const ITEM_BOB_AMPLITUDE: f32 = 0.1;

/// Ticks per radian of bob phase (`sin(ageInTicks / 10.0F + bobOffs)`).
pub const ITEM_BOB_TICKS_PER_RADIAN: f32 = 10.0;

/// Ticks per radian of spin (`getSpin = ageInTicks / 20.0F + bobOffs`).
pub const ITEM_SPIN_TICKS_PER_RADIAN: f32 = 20.0;

/// `display.ground` of `minecraft:block/block`, which **every** block item model
/// inherits (verified against 26.2's `client.jar`).
///
/// # Why this is a constant and not read from the model
///
/// It should be read from the model. It cannot be yet:
/// [`IconPart::Model`](lodestone_assets::IconPart) keeps only the `gui` slot —
/// `icon.rs` does `resolved.display.get("gui")` and drops the rest — so
/// [`ItemGeometry::transform`](crate::ItemGeometry) carries the isometric
/// inventory pose and nothing else. Posing a dropped item with the *gui*
/// transform would tilt it 30°/225° and scale it 0.625 instead of 0.25: visibly
/// a "3-D item is there", and wrong in both orientation and size. Until the
/// asset layer carries the whole `display` map, naming the real vanilla numbers
/// here is the honest stopgap — and it is exact for the block items, which are
/// what a broken block actually drops.
pub const BLOCK_ITEM_GROUND: DisplayTransform = DisplayTransform {
    rotation: [0.0, 0.0, 0.0],
    translation: [0.0, 3.0, 0.0],
    scale: [0.25, 0.25, 0.25],
};

/// `display.ground` of `minecraft:item/generated`, the parent of every flat
/// sprite item. See [`BLOCK_ITEM_GROUND`] for why this is a constant.
pub const GENERATED_ITEM_GROUND: DisplayTransform = DisplayTransform {
    rotation: [0.0, 0.0, 0.0],
    translation: [0.0, 2.0, 0.0],
    scale: [0.5, 0.5, 0.5],
};

/// The `display.ground` transform to pose an item under, chosen by its GUI
/// lighting mode: `side` is the block-model family (`block/block`), `front` the
/// flat-sprite family (`item/generated`). Vanilla makes the same split — the two
/// `gui_light` values partition the item models almost exactly along the same
/// line — and it is the only signal reachable from a baked
/// [`ItemGeometry`](crate::ItemGeometry) today.
#[must_use]
pub fn ground_transform_for(gui_light: GuiLight) -> DisplayTransform {
    match gui_light {
        GuiLight::Side => BLOCK_ITEM_GROUND,
        GuiLight::Front => GENERATED_ITEM_GROUND,
    }
}

/// A stable per-entity bob/spin phase in `[0, 2π)`, standing in for vanilla's
/// `bobOffs = random.nextFloat() * PI * 2`.
///
/// Vanilla seeds it from the client's RNG at spawn; we cannot observe that, and
/// re-rolling it every frame would make an item jitter instead of spin. Hashing
/// the server-assigned entity id gives the same *property* that matters — two
/// items dropped together do not bob in lockstep — while staying a pure function
/// of data both the renderer and a test can see.
#[must_use]
pub fn item_bob_offset(entity_id: i32) -> f32 {
    // A single multiplicative-hash round over the id, taken as a fraction.
    let mixed = (entity_id as u32).wrapping_mul(0x9E37_79B9);
    let frac = f32::from(u16::try_from(mixed >> 16).unwrap_or(0)) / 65536.0;
    frac * std::f32::consts::TAU
}

/// Vanilla's vertical bob at `age_ticks`: `sin(age / 10 + offs) * 0.1 + 0.1`,
/// so the result is in `0.0..=0.2` blocks and never negative.
#[must_use]
pub fn item_bob_height(age_ticks: f32, bob_offset: f32) -> f32 {
    (age_ticks / ITEM_BOB_TICKS_PER_RADIAN + bob_offset).sin() * ITEM_BOB_AMPLITUDE
        + ITEM_BOB_AMPLITUDE
}

/// Vanilla's `ItemEntity.getSpin`: the item's yaw in **radians** at `age_ticks`.
#[must_use]
pub fn item_spin_radians(age_ticks: f32, bob_offset: f32) -> f32 {
    age_ticks / ITEM_SPIN_TICKS_PER_RADIAN + bob_offset
}

/// The model-space `y` extent of `quads` once posed by `ground`, as
/// `(min_y, max_y)`. `(0, 0)` for an empty quad list.
///
/// This is vanilla's `state.item.getModelBoundingBox()` for the `y` axis: it is
/// measured on the **posed** model, which is why it cannot be a constant — a
/// scaled-down cube and a full-size one hover differently.
#[must_use]
pub fn posed_item_y_extent(quads: &[BakedQuad], ground: &DisplayTransform) -> (f32, f32) {
    let pose = display_matrix(ground);
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for quad in quads {
        for p in &quad.positions {
            let y = pose.transform_point3(Vec3::from(*p)).y;
            min = min.min(y);
            max = max.max(y);
        }
    }
    if min > max { (0.0, 0.0) } else { (min, max) }
}

/// Vanilla's `minOffsetY`: the lift that puts the posed model's lowest point
/// exactly [`ITEM_MIN_HOVER_HEIGHT`] above the entity's own position.
#[must_use]
pub fn item_hover_lift(quads: &[BakedQuad], ground: &DisplayTransform) -> f32 {
    -posed_item_y_extent(quads, ground).0 + ITEM_MIN_HOVER_HEIGHT
}

/// The world placement matrix for a dropped item, matching
/// `ItemEntityRenderer.submit`'s pose-stack order exactly:
///
/// ```text
/// T(position) · T(0, bob + hover_lift, 0) · Ry(spin) · display_matrix(ground)
/// ```
///
/// `position` is the item entity's reported world position, `age_ticks` its
/// continuous age (`ageInTicks`, fractional between server ticks), `bob_offset`
/// its per-entity phase ([`item_bob_offset`]) and `hover_lift`
/// [`item_hover_lift`] for the same quads and transform.
///
/// The determinant is **positive** (a translation, a rotation and a positive
/// uniform scale), so this composes with `Camera::view_projection` to the same
/// winding as terrain. See the section note above for why "negative" is the
/// tempting wrong answer.
#[must_use]
pub fn dropped_item_matrix(
    position: Vec3,
    age_ticks: f32,
    bob_offset: f32,
    ground: &DisplayTransform,
    hover_lift: f32,
) -> Mat4 {
    let bob = item_bob_height(age_ticks, bob_offset);
    let spin = item_spin_radians(age_ticks, bob_offset);
    Mat4::from_translation(position)
        * Mat4::from_translation(Vec3::new(0.0, bob + hover_lift, 0.0))
        * Mat4::from_rotation_y(spin)
        * display_matrix(ground)
}

/// Mesh one dropped item's baked geometry into a world-space [`ModelMesh`],
/// ready for [`GpuModelMesh::upload`](crate::GpuModelMesh) and a draw through
/// the ordinary [`ModelPipeline`](crate::ModelPipeline) with a *world* camera
/// uniform (`section_origin` zero).
///
/// The geometry and the shading come from [`mesh_item_quads`], which the hotbar
/// already uses, so a dropped stone and a stone in slot 0 are textured and shaded
/// from the identical quads. The one thing overridden afterwards is the packed
/// light byte: `mesh_item_quads` nails every vertex to
/// [`GUI_ITEM_LIGHT`](crate::GUI_ITEM_LIGHT) because an inventory slot is
/// full-bright by definition, and a dropped item in a dark cave is emphatically
/// not. Pass the world sample (see [`EntityLightSource`](crate::EntityLightSource)
/// on the shell side); pass `GUI_ITEM_LIGHT` when there is no world to sample.
#[must_use]
pub fn dropped_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    ground: &DisplayTransform,
    position: Vec3,
    age_ticks: f32,
    bob_offset: f32,
    light: u8,
) -> ModelMesh {
    let lift = item_hover_lift(quads, ground);
    let pose = dropped_item_matrix(position, age_ticks, bob_offset, ground, lift);
    let mut mesh = mesh_item_quads(quads, pose, gui_light);
    for vertex in &mut mesh.vertices {
        vertex.light = light;
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_assets::Direction;

    fn pig_mesh() -> EntityMesh {
        EntityMesh::from_model(&lodestone_assets::entity_models::pig_model())
    }

    #[test]
    fn maps_known_entity_types_to_models() {
        assert_eq!(model_for_type("pig").unwrap().name, "pig");
        assert_eq!(model_for_type("cow").unwrap().name, "cow");
        assert_eq!(model_for_type("chicken").unwrap().name, "chicken");
        assert_eq!(model_for_type("sheep").unwrap().name, "sheep");
        assert_eq!(model_for_type("zombie").unwrap().name, "zombie");
        assert_eq!(model_for_type("skeleton").unwrap().name, "skeleton");
        assert_eq!(model_for_type("creeper").unwrap().name, "creeper");
        assert_eq!(model_for_type("spider").unwrap().name, "spider");
        // The two surviving aliases: a type path that is not a corpus name.
        assert_eq!(model_for_type("player").unwrap().name, "player_wide");
        assert_eq!(model_for_type("bogged").unwrap().name, "skeleton");
    }

    /// The reported defect: a drowned rendered as an ordinary zombie. Its mesh
    /// and its sheet both exist in the corpus; a stale alias in this module was
    /// routing it to the zombie's. Every mob here is one that alias table used
    /// to swallow, so each assertion is a distinct wrong-mesh substitution.
    #[test]
    fn mob_variants_resolve_to_their_own_model_not_a_base_mob() {
        for (ty, wrong) in [
            ("drowned", "zombie"),
            ("husk", "zombie"),
            ("zombie_villager", "zombie"),
            ("stray", "skeleton"),
            ("wither_skeleton", "skeleton"),
            ("cave_spider", "spider"),
            ("mooshroom", "cow"),
        ] {
            let model = model_for_type(ty).unwrap_or_else(|| panic!("{ty} has a corpus model"));
            assert_eq!(
                model.name, ty,
                "{ty} resolved to {} — a variant is being drawn as its base mob",
                model.name
            );
            assert_ne!(model.name, wrong);
            // ...and it must not merely resolve: the *sheet* has to differ too,
            // or the mob is still a zombie with a zombie skin under a new name.
            assert_ne!(
                entity_texture_candidates(ty),
                entity_texture_candidates(wrong),
                "{ty} shares {wrong}'s texture candidates"
            );
        }
    }

    #[test]
    fn unknown_entity_type_has_no_model() {
        // Types the corpus genuinely has no mesh for — the renderer skips them
        // rather than substituting something mob-shaped.
        assert!(model_for_type("arrow").is_none());
        assert!(model_for_type("experience_orb").is_none());
        assert!(model_for_type("tnt").is_none());
        assert!(model_for_type("").is_none());
    }

    #[test]
    fn every_drawable_model_has_a_texture_candidate() {
        // Now that the drawable set *is* the corpus, sweep the whole corpus:
        // every baked model gets uploaded with a sheet by the shell, so a model
        // with no candidate is a mob that draws as a flat placeholder colour.
        let mut checked = 0;
        for entry in entity_models() {
            let candidates = entity_texture_candidates(entry.name);
            assert!(
                !candidates.is_empty(),
                "model {:?} has no texture candidate",
                entry.name
            );
            for path in candidates {
                assert!(
                    path.starts_with("assets/minecraft/textures/entity/") && path.ends_with(".png"),
                    "candidate {path:?} for {:?} is not an entity sheet path",
                    entry.name
                );
            }
            checked += 1;
        }
        assert!(checked > 60, "only {checked} models swept");
        // The temperature-variant mobs keep their pre-26.2 sheet as a fallback,
        // so one binary works against both pack layouts.
        assert_eq!(
            entity_texture_candidates("pig"),
            [
                "assets/minecraft/textures/entity/pig/pig_temperate.png",
                "assets/minecraft/textures/entity/pig/pig.png",
            ]
        );
        // A name that is not a model resolves to nothing rather than a wrong sheet.
        assert!(entity_texture_candidates("arrow").is_empty());
    }

    /// The other half of the drowned defect: even with its own mesh, a drowned
    /// wearing `zombie.png` still reads as an ordinary zombie. The path is
    /// derived from the corpus entry, so this pins the derivation, not a table.
    #[test]
    fn variant_mobs_point_at_their_own_sheet() {
        assert_eq!(
            entity_texture_candidates("drowned"),
            ["assets/minecraft/textures/entity/zombie/drowned.png"]
        );
        assert_eq!(
            entity_texture_candidates("husk"),
            ["assets/minecraft/textures/entity/zombie/husk.png"]
        );
        assert_eq!(
            entity_texture_candidates("stray"),
            ["assets/minecraft/textures/entity/skeleton/stray.png"]
        );
    }

    #[test]
    fn mesh_has_four_verts_and_six_indices_per_quad() {
        let mesh = pig_mesh();
        assert!(mesh.quad_count() > 0, "pig must produce geometry");
        assert_eq!(mesh.vertices.len(), mesh.quad_count() * 4);
        assert_eq!(mesh.indices.len(), mesh.quad_count() * 6);
        // Matches the underlying bake exactly (one quad per baked quad).
        let baked =
            lodestone_assets::entity::bake_entity(&lodestone_assets::entity_models::pig_model());
        assert_eq!(mesh.quad_count(), baked.len());
    }

    #[test]
    fn mesh_indices_are_all_in_range() {
        let mesh = pig_mesh();
        let n = mesh.vertices.len() as u32;
        assert!(mesh.indices.iter().all(|&i| i < n));
    }

    #[test]
    fn model_matrix_stands_the_mob_upright_at_its_feet() {
        // A humanoid: head cube top is model y = -8/16 = -0.5, feet reach
        // y = 24/16 = 1.5 (Y-down). After placement, feet ≈ world feet, head above.
        let feet = Vec3::new(10.0, 64.0, -20.0);
        let m = entity_model_matrix(feet, 0.0, 1.0);

        let model_feet = m.transform_point3(Vec3::new(0.0, 1.5, 0.0));
        let model_head = m.transform_point3(Vec3::new(0.0, -0.5, 0.0));

        // Feet land on the ground plane (within a couple of cm of the offset).
        assert!(
            (model_feet.y - feet.y).abs() < 0.05,
            "feet should sit at the entity position, got {model_feet:?}",
        );
        // Head is clearly above the feet: upright, not upside-down.
        assert!(
            model_head.y > model_feet.y + 1.5,
            "head must be above feet (upright), head={model_head:?} feet={model_feet:?}",
        );
        // Horizontal position stays at the feet column.
        assert!((model_feet.x - feet.x).abs() < 1e-4);
        assert!((model_feet.z - feet.z).abs() < 1e-4);
    }

    #[test]
    fn model_matrix_preserves_handedness() {
        // scale(-1,-1,1) has det +1, so combined with rotation/translation the
        // transform must preserve winding (positive determinant).
        let m = entity_model_matrix(Vec3::new(1.0, 2.0, 3.0), 37.0, 1.0);
        assert!(
            m.determinant() > 0.0,
            "det must stay positive so back-face culling remains valid, got {}",
            m.determinant(),
        );
    }

    #[test]
    fn yaw_rotates_about_the_vertical_axis() {
        // A point offset in +X (model) with feet at origin: under a 90° body-yaw
        // change it must swing in the horizontal plane while its height is
        // unchanged (rotation is about Y only).
        let feet = Vec3::ZERO;
        let probe = Vec3::new(1.0, 0.5, 0.0);
        let a = entity_model_matrix(feet, 0.0, 1.0).transform_point3(probe);
        let b = entity_model_matrix(feet, 90.0, 1.0).transform_point3(probe);
        assert!(
            (a.y - b.y).abs() < 1e-5,
            "yaw must not change height: {a:?} vs {b:?}",
        );
        let horizontal = ((a.x - b.x).powi(2) + (a.z - b.z).powi(2)).sqrt();
        assert!(
            horizontal > 0.5,
            "a 90° yaw change must move the point horizontally: {a:?} vs {b:?}",
        );
    }

    #[test]
    fn scale_shrinks_the_model_about_the_feet() {
        let feet = Vec3::new(0.0, 0.0, 0.0);
        let full = entity_model_matrix(feet, 0.0, 1.0).transform_point3(Vec3::new(0.0, -0.5, 0.0));
        let baby = entity_model_matrix(feet, 0.0, 0.5).transform_point3(Vec3::new(0.0, -0.5, 0.0));
        // Feet stay near the ground for both; the head of the scaled mob is lower.
        assert!(baby.y < full.y, "scaled-down mob's head must be lower");
        assert!(baby.y > feet.y, "scaled mob still stands above its feet");
    }

    /// A zombie's resting arms stick out ~0.75 blocks in front of it, so its
    /// culling box has to be drawn around the mob *as posed*, not around a mob
    /// standing to attention. `EntityMesh::from_named_model` gets that by
    /// choosing the arm rig before taking the local bounds; if it did not, the
    /// error would be invisible until a zombie clipped out at the screen edge.
    #[test]
    fn a_zombies_local_bounds_include_its_outstretched_arms() {
        let plain = EntityMesh::from_model(&lodestone_assets::entity_models::zombie_model());
        let zombie = EntityMesh::from_named_model(
            "zombie",
            &lodestone_assets::entity_models::zombie_model(),
        );
        assert_eq!(
            humanoid_arms_for("zombie"),
            crate::entity_anim::HumanoidArms::Zombie
        );
        // Model -Z is the mob's facing, so the arms extend the *minimum* Z.
        // The arm cube ends 10 texels (0.625 blocks) down from its pivot, so at
        // -80° it reaches ~0.63 blocks forward against an arms-down torso whose
        // frontmost point is the 0.28-block hat overlay.
        assert!(
            zombie.local_min.z < plain.local_min.z - 0.3,
            "the zombie's bounds reach {} forward against an arms-down {} — the rig was not \
             applied before the AABB was taken",
            zombie.local_min.z,
            plain.local_min.z
        );

        // And the bound must actually hold for every posed vertex.
        let feet = Vec3::new(5.0, 70.0, 5.0);
        let inst = EntityInstance::new("zombie", &zombie, feet, 37.0, 1.0, &AnimInput::REST);
        for (part, range) in zombie.parts.iter().enumerate() {
            let m = inst.part_transforms[part];
            let lo = range.vertex_start as usize;
            let hi = lo + range.vertex_count as usize;
            for v in &zombie.vertices[lo..hi] {
                let w = m.transform_point3(Vec3::from(v.position));
                assert!(
                    w.cmpge(inst.aabb_min - Vec3::splat(1e-2)).all()
                        && w.cmple(inst.aabb_max + Vec3::splat(1e-2)).all(),
                    "vertex {w:?} escaped AABB [{:?}, {:?}]",
                    inst.aabb_min,
                    inst.aabb_max,
                );
            }
        }
    }

    #[test]
    fn instance_world_aabb_contains_the_transformed_mesh() {
        let mesh = pig_mesh();
        let feet = Vec3::new(5.0, 70.0, 5.0);
        let inst = EntityInstance::new("pig", &mesh, feet, 45.0, 1.0, &AnimInput::REST);
        // Vertices are part-local, so a vertex only lands in the world once it
        // has been through *its own part's* matrix — the same matrix the GPU
        // draws it with. Using `inst.transform` alone would collapse every part
        // onto the model origin and the AABB check would be meaningless.
        for (part, range) in mesh.parts.iter().enumerate() {
            let m = inst.part_transforms[part];
            let lo = range.vertex_start as usize;
            let hi = lo + range.vertex_count as usize;
            for v in &mesh.vertices[lo..hi] {
                let w = m.transform_point3(Vec3::from(v.position));
                assert!(
                    w.cmpge(inst.aabb_min - Vec3::splat(1e-3)).all()
                        && w.cmple(inst.aabb_max + Vec3::splat(1e-3)).all(),
                    "vertex {w:?} escaped AABB [{:?}, {:?}]",
                    inst.aabb_min,
                    inst.aabb_max,
                );
            }
        }
    }

    fn frustum_looking_down_pos_z() -> Frustum {
        use crate::camera::Camera;
        Camera {
            position: Vec3::new(0.0, 64.0, 0.0),
            yaw: 0.0, // faces +Z
            pitch: 0.0,
            fov_y_degrees: 70.0,
            aspect: 1.0,
            near: 0.05,
            far: 128.0,
        }
        .frustum()
    }

    #[test]
    fn frustum_culls_entities_behind_the_camera() {
        let mesh = pig_mesh();
        let frustum = frustum_looking_down_pos_z();
        let in_front = EntityInstance::new(
            "pig",
            &mesh,
            Vec3::new(0.0, 63.0, 20.0),
            0.0,
            1.0,
            &AnimInput::REST,
        );
        let behind = EntityInstance::new(
            "pig",
            &mesh,
            Vec3::new(0.0, 63.0, -20.0),
            0.0,
            1.0,
            &AnimInput::REST,
        );

        let frame = plan_entities(&[in_front, behind], &frustum);
        assert_eq!(frame.stats.drawn, 1, "only the front entity draws");
        assert_eq!(frame.stats.culled_frustum, 1, "the one behind is culled");
        assert!(frame.stats.is_meaningful());
        assert_eq!(frame.instance_count(), 1);
    }

    #[test]
    fn plan_groups_instances_by_model_type() {
        let pig = pig_mesh();
        let cow = EntityMesh::from_model(&lodestone_assets::entity_models::cow_model());
        let frustum = frustum_looking_down_pos_z();
        let at = |model, m: &EntityMesh, z: f32| {
            EntityInstance::new(
                model,
                &m,
                Vec3::new(0.0, 63.0, z),
                0.0,
                1.0,
                &AnimInput::REST,
            )
        };
        let instances = [
            at("pig", &pig, 10.0),
            at("cow", &cow, 12.0),
            at("pig", &pig, 14.0),
            // one behind the camera to force a cull so the frame is meaningful.
            at("pig", &pig, -30.0),
        ];
        let frame = plan_entities(&instances, &frustum);
        assert_eq!(frame.stats.drawn, 3);
        assert_eq!(frame.stats.culled_frustum, 1);
        assert!(frame.stats.is_meaningful());

        let pig_batch = frame.batches.iter().find(|b| b.model == "pig").unwrap();
        let cow_batch = frame.batches.iter().find(|b| b.model == "cow").unwrap();
        assert_eq!(
            pig_batch.transforms.len(),
            2,
            "two visible pigs batch together"
        );
        assert_eq!(cow_batch.transforms.len(), 1);
    }

    #[test]
    fn model_set_resolves_types_and_skips_unknowns() {
        let set = EntityModelSet::load();
        assert!(!set.is_empty());
        assert_eq!(set.len(), entity_models().len());

        let feet = Vec3::new(0.0, 63.0, 10.0);
        let pig = set
            .resolve("pig", feet, 0.0, 1.0, &AnimInput::REST)
            .expect("pig resolves");
        assert_eq!(pig.model, "pig");
        assert_eq!(
            set.resolve("cave_spider", feet, 0.0, 1.0, &AnimInput::REST)
                .unwrap()
                .model,
            "cave_spider"
        );
        // Unknown type resolves to nothing (renderer skips it).
        assert!(
            set.resolve("experience_orb", feet, 0.0, 1.0, &AnimInput::REST)
                .is_none()
        );
        // The resolved instance's model is present in the set for upload.
        assert!(set.get(pig.model).is_some());
    }

    #[test]
    fn plan_seam_resolves_culls_and_skips_modelless_in_one_call() {
        let set = EntityModelSet::load();
        let frustum = frustum_looking_down_pos_z();
        // A mix mirroring a live scene: two drawable pigs, one drawable cow, a
        // modelless type that must be dropped (not culled), and one pig behind
        // the camera to force a real cull.
        // The two visible pigs carry *different* light so the batch's `lights`
        // can be checked to stay in step with its `transforms`: a batch that
        // merged or reordered them would still have the right length.
        let spawns = [
            EntitySpawn {
                type_path: "pig",
                feet: Vec3::new(0.0, 63.0, 10.0),
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: ENTITY_FULLBRIGHT,
            },
            EntitySpawn {
                type_path: "cow",
                feet: Vec3::new(0.0, 63.0, 12.0),
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: 0x0A, // block light 10, no sky: a torch-lit cow indoors
            },
            EntitySpawn {
                type_path: "experience_orb", // no model — dropped, not counted
                feet: Vec3::new(0.0, 63.0, 14.0),
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: ENTITY_FULLBRIGHT,
            },
            EntitySpawn {
                type_path: "pig",
                feet: Vec3::new(0.0, 63.0, 16.0),
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: 0x00, // pitch dark
            },
            EntitySpawn {
                type_path: "pig",
                feet: Vec3::new(0.0, 63.0, -30.0), // behind camera
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: ENTITY_FULLBRIGHT,
            },
        ];

        let frame = set.plan(spawns, &frustum);

        // The modelless dragon is dropped before culling, so `total` counts only
        // the four entities that had a model, and exactly one of those culled.
        assert_eq!(frame.stats.total, 4, "modelless types are not counted");
        assert_eq!(frame.stats.drawn, 3);
        assert_eq!(frame.stats.culled_frustum, 1);
        assert!(frame.stats.is_meaningful());
        let pig_batch = frame.batches.iter().find(|b| b.model == "pig").unwrap();
        assert_eq!(pig_batch.transforms.len(), 2, "two visible pigs batch");
        assert!(frame.batches.iter().any(|b| b.model == "cow"));

        // Light must ride through `plan` per instance and stay index-aligned
        // with `transforms` — the culled pig drops out of both, so the surviving
        // pair is the lit one then the dark one, in spawn order.
        assert_eq!(
            pig_batch.lights,
            vec![u32::from(ENTITY_FULLBRIGHT), 0x00],
            "per-entity light must survive resolve + cull in transform order"
        );
        let cow_batch = frame.batches.iter().find(|b| b.model == "cow").unwrap();
        assert_eq!(cow_batch.lights, vec![0x0A]);
        for batch in &frame.batches {
            assert_eq!(
                batch.lights.len(),
                batch.transforms.len(),
                "one light per instance, or the instance buffer would misalign"
            );
        }

        // The one-call seam is exactly manual resolve + plan_entities: same frame.
        let manual: Vec<EntityInstance> = spawns
            .iter()
            .filter_map(|s| {
                set.resolve(s.type_path, s.feet, s.body_yaw_deg, s.scale, &s.anim)
                    .map(|i| i.with_light(s.light))
            })
            .collect();
        let manual_frame = plan_entities(&manual, &frustum);
        assert_eq!(frame.batches, manual_frame.batches);
        assert_eq!(frame.instance_count(), manual_frame.instance_count());
    }

    // ---- dropped items ---------------------------------------------------

    /// A unit cube's six outward-wound faces, in `mesh_item_quads`' vertex
    /// order, as a stand-in for a baked block item's geometry.
    fn cube_face(dir: Direction) -> [Vec3; 4] {
        let n = match dir {
            Direction::East => Vec3::X,
            Direction::West => -Vec3::X,
            Direction::Up => Vec3::Y,
            Direction::Down => -Vec3::Y,
            Direction::South => Vec3::Z,
            Direction::North => -Vec3::Z,
        };
        let u = if n.x.abs() < 0.5 { Vec3::X } else { Vec3::Y };
        let v = n.cross(u);
        let centre = Vec3::splat(0.5) + n * 0.5;
        [
            centre - u * 0.5 - v * 0.5,
            centre + u * 0.5 - v * 0.5,
            centre + u * 0.5 + v * 0.5,
            centre - u * 0.5 + v * 0.5,
        ]
    }

    fn cube_quad(dir: Direction) -> BakedQuad {
        let p = cube_face(dir);
        BakedQuad {
            positions: [p[0].into(), p[1].into(), p[2].into(), p[3].into()],
            uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            direction: dir,
            cullface: None,
            tint_index: None,
            shade: true,
            layer: 0,
            anim: 0,
        }
    }

    fn unit_cube_quads() -> Vec<BakedQuad> {
        [
            Direction::East,
            Direction::West,
            Direction::Up,
            Direction::Down,
            Direction::South,
            Direction::North,
        ]
        .into_iter()
        .map(cube_quad)
        .collect()
    }

    /// The signed screen area of a quad's first triangle after `m` — the sign
    /// `FrontFace::Ccw` + `cull_mode: Back` acts on.
    fn screen_area(m: Mat4, q: [Vec3; 4]) -> f32 {
        let p: Vec<Vec3> = q.iter().map(|v| m.project_point3(*v)).collect();
        let a = p[1] - p[0];
        let b = p[2] - p[0];
        a.x * b.y - a.y * b.x
    }

    #[test]
    fn the_bob_never_dips_below_the_entity_position() {
        // `sin(..) * 0.1 + 0.1` is bounded to 0.0..=0.2, so a dropped item
        // hovers and never sinks into the block it landed on.
        for tick in 0..400 {
            let age = tick as f32 * 0.5;
            let bob = item_bob_height(age, 1.234);
            assert!(
                (0.0..=2.0 * ITEM_BOB_AMPLITUDE + 1e-6).contains(&bob),
                "bob {bob} at age {age} escaped 0..=0.2"
            );
        }
    }

    #[test]
    fn the_bob_and_the_spin_have_vanillas_periods() {
        // Bob: sin(age/10 + offs) has period 20*PI ticks. Spin: age/20 + offs
        // radians, so a full turn is 40*PI ticks. Asserting the *ratio* as well
        // catches a swapped pair of divisors, which equal-period tests do not.
        let offs = 0.0;
        let bob_period = std::f32::consts::TAU * ITEM_BOB_TICKS_PER_RADIAN;
        assert!(
            (item_bob_height(0.0, offs) - item_bob_height(bob_period, offs)).abs() < 1e-4,
            "the bob must repeat after {bob_period} ticks"
        );
        let spin_period = std::f32::consts::TAU * ITEM_SPIN_TICKS_PER_RADIAN;
        assert!(
            (item_spin_radians(spin_period, offs) - item_spin_radians(0.0, offs)
                - std::f32::consts::TAU)
                .abs()
                < 1e-4,
            "the spin must complete exactly one turn after {spin_period} ticks"
        );
        assert!(
            (spin_period / bob_period - 2.0).abs() < 1e-4,
            "vanilla bobs twice per revolution"
        );
    }

    #[test]
    fn two_entities_do_not_bob_in_lockstep() {
        // The whole point of a per-entity phase: a pile of drops must not
        // pulse as one object.
        let offsets: Vec<f32> = (1..=8).map(item_bob_offset).collect();
        for (i, a) in offsets.iter().enumerate() {
            assert!(
                (0.0..std::f32::consts::TAU).contains(a),
                "phase {a} out of range"
            );
            for b in &offsets[i + 1..] {
                assert!((a - b).abs() > 1e-3, "ids share a phase: {a} vs {b}");
            }
        }
        // ...and it must be stable, or the item jitters instead of spinning.
        assert_eq!(item_bob_offset(7), item_bob_offset(7));
    }

    #[test]
    fn the_hover_lift_puts_the_lowest_point_one_pixel_up() {
        // Vanilla's `minOffsetY = -box.minY + 0.0625`, measured on the GROUND-
        // posed model. Under block/block's ground pose the unit cube is scaled
        // to 0.25 and centred on y = 3/16, so its base sits at 3/16 - 1/8.
        let quads = unit_cube_quads();
        let (min_y, max_y) = posed_item_y_extent(&quads, &BLOCK_ITEM_GROUND);
        assert!((min_y - (3.0 / 16.0 - 0.125)).abs() < 1e-5, "min_y = {min_y}");
        assert!((max_y - (3.0 / 16.0 + 0.125)).abs() < 1e-5, "max_y = {max_y}");

        let lift = item_hover_lift(&quads, &BLOCK_ITEM_GROUND);
        let pose = dropped_item_matrix(Vec3::ZERO, 0.0, 0.0, &BLOCK_ITEM_GROUND, lift);
        let lowest = unit_cube_quads()
            .iter()
            .flat_map(|q| q.positions)
            .map(|p| pose.transform_point3(Vec3::from(p)).y)
            .fold(f32::INFINITY, f32::min);
        // At age 0 with phase 0 the bob is exactly its 0.1 midpoint.
        let expected = ITEM_MIN_HOVER_HEIGHT + item_bob_height(0.0, 0.0);
        assert!(
            (lowest - expected).abs() < 1e-5,
            "the posed model's base must float {expected} above the entity, got {lowest}"
        );
    }

    #[test]
    fn the_spin_is_about_the_entity_position_not_the_model_origin() {
        // The centring inside `display_matrix` is what makes the item rotate on
        // the spot. If it were dropped, the cube would orbit its own corner and
        // swing half a block sideways every revolution.
        let quads = unit_cube_quads();
        let lift = item_hover_lift(&quads, &BLOCK_ITEM_GROUND);
        let feet = Vec3::new(10.0, 64.0, -3.0);
        for age in [0.0f32, 13.0, 27.5, 61.0] {
            let pose = dropped_item_matrix(feet, age, 0.4, &BLOCK_ITEM_GROUND, lift);
            let centre = pose.transform_point3(Vec3::splat(0.5));
            assert!(
                (centre.x - feet.x).abs() < 1e-4 && (centre.z - feet.z).abs() < 1e-4,
                "at age {age} the item centre drifted to {centre} from {feet}"
            );
        }
    }

    #[test]
    fn dropped_item_pose_preserves_winding() {
        // Derive the front-facing sign from the camera rather than asserting
        // "positive" or "negative" — the same discipline `item_render`'s
        // `winding_matches_the_world_camera` uses, and the reason that test
        // cannot be fooled by a misremembered glam/wgpu convention.
        //
        // The trap this pins: the GUI rule is that `gui_ortho * gui_item_pose`
        // matches `view_projection`'s determinant SIGN (negative). Applying that
        // to a *world* pose — which is left-multiplied by that same
        // `view_projection` — inverts it. A world pose must have a POSITIVE
        // determinant, and the composition then inherits the camera's negative.
        let camera = crate::camera::Camera {
            position: Vec3::new(0.5, 0.5, 4.0),
            yaw: 180.0,
            pitch: 0.0,
            ..crate::camera::Camera::default()
        };
        let world = camera.view_projection();
        assert!(
            world.determinant() < 0.0,
            "the reference camera's determinant is expected to be negative \
             (glam's DirectX RH perspective); it is {}",
            world.determinant()
        );
        let front_sign = screen_area(world, cube_face(Direction::South)).signum();
        assert_eq!(
            screen_area(world, cube_face(Direction::North)).signum(),
            -front_sign,
            "the reference camera must disagree about the far face"
        );

        let quads = unit_cube_quads();
        let lift = item_hover_lift(&quads, &BLOCK_ITEM_GROUND);
        // Several ages, so a spin angle cannot be what rescues the sign.
        for age in [0.0f32, 5.0, 17.0, 33.0, 70.0] {
            let pose = dropped_item_matrix(
                Vec3::new(0.5, 0.5, 0.0),
                age,
                0.0,
                &BLOCK_ITEM_GROUND,
                lift,
            );
            assert!(
                pose.determinant() > 0.0,
                "a world-space item pose must not flip handedness; det = {} at age {age}",
                pose.determinant()
            );
            let composed = world * pose;
            assert_eq!(
                composed.determinant().signum(),
                world.determinant().signum(),
                "view_projection * pose must keep the camera's winding at age {age}"
            );
            // And on-screen: whichever cube face currently points at the camera
            // must carry the front-facing sign.
            let towards_camera = if (item_spin_radians(age, 0.0) / std::f32::consts::TAU).fract()
                < 0.25
            {
                Direction::South
            } else {
                continue;
            };
            assert_eq!(
                screen_area(composed, cube_face(towards_camera)).signum(),
                front_sign,
                "the face turned towards the camera must survive back-face culling at age {age}"
            );
        }
    }

    #[test]
    fn the_mesh_carries_the_world_light_not_the_gui_full_bright() {
        // The regression this guards: reusing `mesh_item_quads` verbatim nails
        // every vertex to GUI_ITEM_LIGHT, so a drop in a pitch-black cave glows
        // exactly as brightly as one at noon.
        let quads = unit_cube_quads();
        let dark = dropped_item_mesh(
            &quads,
            GuiLight::Side,
            &BLOCK_ITEM_GROUND,
            Vec3::ZERO,
            0.0,
            0.0,
            0x02,
        );
        assert!(!dark.vertices.is_empty(), "the cube must mesh to something");
        assert!(
            dark.vertices.iter().all(|v| v.light == 0x02),
            "every vertex must carry the sampled world light"
        );
        assert_eq!(dark.quad_count(), quads.len());
    }

    #[test]
    fn the_two_ground_transforms_are_selected_by_gui_light() {
        assert_eq!(ground_transform_for(GuiLight::Side), BLOCK_ITEM_GROUND);
        assert_eq!(ground_transform_for(GuiLight::Front), GENERATED_ITEM_GROUND);
        // The flat family is posed twice as large and one pixel lower; a swap
        // would halve every dropped block.
        const {
            assert!(GENERATED_ITEM_GROUND.scale[0] > BLOCK_ITEM_GROUND.scale[0]);
        }
    }
}
