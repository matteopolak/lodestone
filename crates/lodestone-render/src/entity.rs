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
use lodestone_assets::equipment::{ArmourLayer, ArmourSlot, armour_item, humanoid_armour_model};
use lodestone_assets::{BakedQuad, DisplaySlot, DisplayTransform, DisplayTransforms, GuiLight};

use crate::camera::Frustum;
use crate::entity_anim::{AnimInput, HandPoseOverride, HumanoidArms, Skeleton};
use crate::item_render::{UNITS_PER_BLOCK, display_matrix, display_matrix_for_hand};
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

/// The factor the **sky** half of the lightmap is scaled by at a given server
/// `time_of_day` — `1.0` at noon, `0.24` at midnight. Feed it to
/// [`EntityCameraUniform::with_sky_darken`](crate::entity_pipeline::EntityCameraUniform::with_sky_darken).
///
/// # Why this is needed even when world light is sampled correctly
///
/// A server's sky-light array is time-**invariant** — it records how much sky
/// reaches a block, not how bright the sky is right now. Measured live against a
/// vanilla 26.2 oracle at a single sky-lit position, with the server's own clock
/// as the control:
///
/// ```text
/// noon     clock= 6000  packed=0xF0  light_term=1.000
/// midnight clock=18000  packed=0xF0  light_term=1.000
/// ```
///
/// So a mob sampling world light perfectly is still full-bright at midnight.
/// Vanilla applies the darkening client-side only.
///
/// # The curve
///
/// Three vanilla steps composed, in order:
///
/// 1. `DimensionType::timeOfDay(dayTime)` — the celestial angle, `0.0` at noon
///    and `0.5` at midnight.
/// 2. `Level::getSkyDarken(partialTick)` — `1.0` in full day, floored at `0.2`.
/// 3. `LightTexture::updateLightTexture`'s `skyDarken * 0.95 + 0.05` lift, which
///    maps that `[0.2, 1.0]` onto `[0.24, 1.0]`.
///
/// Step 3 is included so the returned number is *directly* the shader's
/// multiplier and no caller has to remember the lift.
///
/// # How to change it
///
/// Rain and thunder are two further multipliers inside step 2
/// (`1 - rainLevel * 5/16`, likewise thunder). They are omitted because the
/// shell tracks neither yet; add them as arguments here rather than at the call
/// site, so the one place that knows vanilla's curve stays the one place. The
/// `0.0`-means-daylight sentinel lives in the shader, not here — this function
/// never returns `0.0`.
#[must_use]
pub fn sky_darken_for_time_of_day(time_of_day: i64) -> f32 {
    // 1. Celestial angle. `time_of_day` counts up without wrapping, so reduce
    //    into the day first; f64 because the day fraction is what `Mth.frac`
    //    operates on and f32 loses ticks at large world ages.
    let day = time_of_day.rem_euclid(24_000) as f64 / 24_000.0;
    let frac = (day - 0.25).rem_euclid(1.0);
    let eased = 0.5 - (frac * std::f64::consts::PI).cos() / 2.0;
    let celestial = ((frac * 2.0 + eased) / 3.0) as f32;

    // 2. `Level::getSkyDarken`. The double negation is vanilla's, kept literal:
    //    a cosine that saturates the clamp for most of the day, inverted, then
    //    lifted onto [0.2, 1.0].
    let mut f = 1.0 - ((celestial * std::f32::consts::TAU).cos() * 2.0 + 0.2);
    f = f.clamp(0.0, 1.0);
    f = 1.0 - f;
    let darken = f * 0.8 + 0.2;

    // 3. `LightTexture`'s lift.
    darken * 0.95 + 0.05
}

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

/// The [`entity_models`] entry name for a player's own body, chosen by skin
/// model rather than the `"player"`-type-path default [`canonical_model_name`]
/// falls back to.
///
/// `AvatarRenderer` (26.2's player renderer) picks between `player_wide` and
/// `player_slim` per skin — a player's uploaded skin reports which model it
/// wants — so the choice is genuinely per-player data, not a constant. Both
/// rigs are already first-class [`entity_models`] entries (`player_wide` and
/// `player_slim` both appear as top-level corpus names, not just as
/// `canonical_model_name`'s hidden alias target), so a caller that already
/// knows which skin a player wears can pass this straight through as a
/// `type_path` — [`canonical_model_name`] resolves a literal `"player_wide"`/
/// `"player_slim"` via its corpus-name fallback with no extra plumbing.
///
/// `canonical_model_name("player")` deliberately keeps resolving to
/// `player_wide` alone: it has no per-instance signal to read, and the other
/// callers that go through it (the first-person arm, a remote player with no
/// skin data yet) want exactly that default.
///
/// No caller in this codebase has real skin-model data yet — see
/// `RenderState::prepare_first_person_arm`'s "the shell has no skin-model
/// signal" note in `lodestone-shell`, which is still true here. This function
/// exists so that the day that signal arrives (from the tab-list player-info
/// packet, decoded in the network layer), selecting the right rig for the
/// local player's own third-person body — or a remote one — is a one-line
/// change at the call site rather than new plumbing in this crate.
#[must_use]
pub fn player_model_name(slim: bool) -> &'static str {
    if slim { "player_slim" } else { "player_wide" }
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

/// Which [`HandPoseOverride`] a model's `translateToHand` needs, keyed by the
/// same [`entity_models`] name [`humanoid_arms_for`] reads. The five corpus
/// models with an override; see [`HandPoseOverride`] and
/// `held_item_matrix`'s doc comment for the source table this was read from.
#[must_use]
pub fn hand_pose_override_for(model_name: &str) -> HandPoseOverride {
    match model_name {
        "skeleton" | "stray" | "wither_skeleton" => HandPoseOverride::PivotShiftTexels(1.0),
        "player_slim" => HandPoseOverride::PivotShiftTexels(0.5),
        "vex" => HandPoseOverride::Vex,
        "allay" => HandPoseOverride::Allay,
        _ => HandPoseOverride::Structural,
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
    /// This model's `translateToHand` override, if vanilla's subclass departs
    /// from `HumanoidModel`'s. See [`HandPoseOverride`] and
    /// [`hand_pose_override_for`]; consumed by [`EntityInstance::new`] to fill
    /// [`EntityInstance::hand_transforms`].
    pub hand_override: HandPoseOverride,
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
                for p in &quad.positions {
                    let posed = rest_m.transform_point3(Vec3::from(*p));
                    local_min = local_min.min(posed);
                    local_max = local_max.max(posed);
                }
            }
            push_part_quads(&part.quads, &mut vertices, &mut indices);
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
            hand_override: hand_pose_override_for(model_name),
        }
    }

    /// Number of quads in the mesh.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }
}

/// Append one part's baked quads to a shared vertex/index buffer as
/// **part-local** geometry, winding each triangle pair from the quad's own baked
/// outward normal.
///
/// The one implementation of that winding rule, shared by [`EntityMesh`] and
/// [`ArmourMesh`]. It has to be shared rather than copied: an armour layer whose
/// winding disagreed with the mob it sits on would be invisible from exactly the
/// half of the angles the mob is visible from, and only once back-face culling is
/// eventually turned on — a defect that cannot be seen today and would land
/// later, on somebody else's change.
fn push_part_quads(
    quads: &[lodestone_assets::entity::EntityQuad],
    vertices: &mut Vec<ModelVertex>,
    indices: &mut Vec<u32>,
) {
    for quad in quads {
        let base = vertices.len() as u32;
        for i in 0..4 {
            vertices.push(ModelVertex {
                position: quad.positions[i],
                uv: quad.uvs[i],
                ao: 1.0,
                // The entity shader does **not** read this byte: entity light is
                // per *instance* (one lightmap sample per mob, as vanilla does),
                // so it arrives on the instance buffer, not here. The field is
                // filled anyway because the vertex layout is shared with
                // terrain, and a full-bright value keeps a mis-wired reader
                // honest rather than rendering every mob black.
                light: ENTITY_FULLBRIGHT,
                tint: 255,
                anim: 0,
                _pad: 0,
            });
        }
        // Wind the two triangles so the geometric normal agrees with the baked
        // outward normal; otherwise back-face culling would drop the visible
        // side.
        let n = Vec3::from(quad.normal);
        let p0 = Vec3::from(quad.positions[0]);
        let p1 = Vec3::from(quad.positions[1]);
        let p2 = Vec3::from(quad.positions[2]);
        if (p1 - p0).cross(p2 - p0).dot(n) >= 0.0 {
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        } else {
            indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
        }
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
    /// The `entity → world` `translateToHand` matrix for `[Arm::Right,
    /// Arm::Left]`, honoring this model's [`HandPoseOverride`] — `None` for an
    /// arm the model doesn't have.
    ///
    /// **Prefer this over indexing `part_transforms` by
    /// `skeleton.index_of(arm.part_name())` when placing a held item.** For
    /// [`HandPoseOverride::Structural`] the two are numerically identical, but
    /// for the five corpus models with a real override they are not, and
    /// cannot be made to be: the override is scoped to `translateToHand`
    /// alone, while `part_transforms[arm]` is shared with the whole-body
    /// instanced draw and also places the arm's own visible mesh. See
    /// [`HandPoseOverride`]'s doc comment for why folding the override into
    /// `part_transforms` would be a new bug, not a fix.
    pub hand_transforms: [Option<Mat4>; 2],
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
        // `false`/`true` here is `Arm::Right`/`Arm::Left`'s own `is_left()` —
        // spelled out rather than iterating `[Arm::Right, Arm::Left]` because
        // `Arm` is defined below this impl and `entity_anim::Skeleton` takes
        // the mirror sign as a bare bool, not this crate's `Arm` type.
        let hand_transforms = [false, true].map(|left| {
            mesh.skeleton
                .translate_to_hand(anim, left, mesh.hand_override)
                .map(|local| transform * local)
        });
        EntityInstance {
            model,
            transform,
            part_transforms,
            hand_transforms,
            aabb_min,
            aabb_max,
            light: ENTITY_FULLBRIGHT,
        }
    }

    /// The `entity → world` `translateToHand` matrix for `arm`, honoring this
    /// model's [`HandPoseOverride`]. `None` only if the model has no such arm
    /// at all. See [`Self::hand_transforms`]'s doc for why this is not the
    /// same value as `part_transforms[skeleton.index_of(arm.part_name())]` for
    /// five corpus models.
    #[must_use]
    pub fn hand_transform(&self, arm: Arm) -> Option<Mat4> {
        self.hand_transforms[if arm.is_left() { 1 } else { 0 }]
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
// Humanoid armour
// ---------------------------------------------------------------------------
//
// Armour is the one drawable in this module that is **not** an entity. It is a
// layer over somebody else's rig, and the whole design follows from one
// consequence of that:
//
// # Every armour piece is posed by the *wearer's* part matrix, never its own
//
// Vanilla does this too, and does it by a route we cannot copy: the armour
// model is an instance of the wearer's own model *class*
// (`AbstractZombieRenderer` builds an `ArmorModelSet<M extends ZombieModel>`),
// and `submitModel` calls `setupAnim` on it with the wearer's render state. A
// zombie's chestplate therefore reaches out in front with `animateZombieArms`,
// because the chestplate ran the same animator.
//
// Here there is one animator per *mesh*, so the faithful equivalent is to skip
// the second pose entirely and read the wearer's already-composed
// `EntityInstance::part_transforms[i]` for the part of the same name. That is
// exact, because [`ArmourMesh`]'s geometry is part-local and its pivots come
// from the very same `humanoid_root` builder the wearer's rig does
// (`lodestone_assets::equipment` shares it deliberately).
//
// **Reading, never mutating.** `EntityInstance::hand_transforms` exists because
// folding a held item's pivot shift into `part_transforms` would have dragged
// the mob's visible arm along with the item. The same discipline applies with
// less effort here: an armour layer needs *exactly* the wearer's matrix with
// nothing added, so there is nothing to fold in and nothing to copy — see
// [`ArmourMesh::attach`], which hands back `(range, wearer part index)` pairs
// and leaves the caller indexing the wearer's own slice.
//
// # Two measured deviations from vanilla, both sub-texel
//
// Reusing the wearer's pivot rather than the armour model's own means a rig
// whose pivots differ from `HumanoidModel`'s gets its armour at *its* pivot,
// not at vanilla's:
//
// * `skeleton`/`stray`/`wither_skeleton` put their legs at `x = ±2.0` where
//   `HumanoidModel` has `±1.9`, so skeleton leg armour sits 0.1 texel
//   (0.00625 blocks) further out than vanilla draws it.
// * `player_slim`'s arms pivot 0.5 texel lower than the wide rig's, and vanilla
//   bakes only *one* player armour set (`PlayerModel.createArmorMeshSet` takes
//   no slim flag and adds only empty sleeve/pants/jacket nodes), so a slim
//   player's sleeves get armour 0.5 texel (0.03 blocks) low.
//
// Both are deliberate: following the visible limb is worth more than matching
// vanilla's pivot to a thirtieth of a block, and the alternative — posing a
// second skeleton — would reintroduce exactly the zombie-arm divergence vanilla
// avoids by construction.

/// One armour slot's baked mesh, in the shared part-local [`ModelVertex`]
/// format, with its parts keyed by the **wearer's** part names.
///
/// One of these per [`ArmourSlot`], not per material: the geometry depends only
/// on the slot's inflation, and every material paints the same four meshes with
/// a different sheet.
#[derive(Debug, Clone)]
pub struct ArmourMesh {
    /// Four vertices per quad, part-local (the part's own pose is *not* folded
    /// in — the wearer's matrix supplies it).
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad, wound so front faces point outward.
    pub indices: Vec<u32>,
    /// `(wearer part name, index range)` for every part that actually carries
    /// geometry, in bake order. Parts pruned by the slot's retention rule are
    /// absent rather than present-and-empty, so a caller cannot accidentally
    /// issue a zero-index draw.
    pub parts: Vec<(&'static str, PartRange)>,
}

impl ArmourMesh {
    /// Bake the mesh for one slot.
    #[must_use]
    pub fn for_slot(slot: ArmourSlot) -> Self {
        let def = humanoid_armour_model(slot);
        let baked = bake_entity_parts(&def);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::new();
        for part in &baked {
            if part.quads.is_empty() {
                continue;
            }
            // Resolve the baked name back to the `&'static str` the slot
            // declares, so the pairing in `attach` is a pointer-cheap compare
            // and a name this mesh carries but the slot does not is a bake bug
            // that shows up here rather than as a missing draw.
            let Some(name) = slot
                .part_names()
                .iter()
                .find(|n| **n == part.name.as_str())
                .copied()
            else {
                continue;
            };
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push((
                name,
                PartRange {
                    index_start,
                    index_count: indices.len() as u32 - index_start,
                    vertex_start,
                    vertex_count: vertices.len() as u32 - vertex_start,
                },
            ));
        }
        ArmourMesh {
            vertices,
            indices,
            parts,
        }
    }

    /// Number of quads in the mesh.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }

    /// Pair each of this mesh's parts with the index of the wearer's part of the
    /// same name, dropping any part the wearer's rig does not have.
    ///
    /// The caller then reads `instance.part_transforms[wearer_index]` — the
    /// wearer's own, already-animated, already-world-space matrix — and draws
    /// `range` instanced over it. Nothing is written back: see this section's
    /// header for why an armour layer must not touch `part_transforms`.
    ///
    /// A non-humanoid rig yields nothing — see [`wearer_carries_armour`], which
    /// this enforces so a caller cannot forget it.
    pub fn attach<'a>(
        &'a self,
        wearer: &'a Skeleton,
    ) -> impl Iterator<Item = (PartRange, usize)> + 'a {
        let humanoid = wearer_carries_armour(wearer);
        self.parts
            .iter()
            .filter(move |_| humanoid)
            .filter_map(|(name, range)| wearer.index_of(name).map(|i| (*range, i)))
    }
}

/// Whether a rig wears humanoid armour at all.
///
/// Vanilla's real gate is which *renderer* owns a `HumanoidArmorLayer`
/// (`HumanoidMobRenderer`, `AvatarRenderer`, `ArmorStandRenderer`, the piglin
/// and zombie families), and the structural equivalent here is the animation
/// family: [`AnimFamily::Humanoid`] is exactly "has both arms and both legs",
/// which is what `HumanoidModel` means.
///
/// **Part names alone are not sufficient and that is the trap.** A pig has both
/// `head` and `body`, so a chestplate keyed on part names would attach its
/// `body` cube to a pig's torso and draw a floating breastplate on a farm
/// animal — geometry that resolves perfectly and is completely wrong. Vanilla
/// draws nothing there.
#[must_use]
pub fn wearer_carries_armour(wearer: &Skeleton) -> bool {
    wearer.family() == crate::entity_anim::AnimFamily::Humanoid
}

/// The four baked humanoid armour meshes, one per [`ArmourSlot`].
///
/// Built once (CPU only, like [`EntityModelSet`]) and uploaded once; a mob's
/// armour costs one instance matrix per drawn part, exactly as its own body
/// does.
#[derive(Debug, Clone)]
pub struct ArmourModelSet {
    meshes: Vec<(ArmourSlot, ArmourMesh)>,
}

impl Default for ArmourModelSet {
    fn default() -> Self {
        Self::load()
    }
}

impl ArmourModelSet {
    /// Bake all four slot meshes, in [`ArmourSlot::ALL`] order — which is
    /// `HumanoidArmorLayer.submit`'s own submit order, so a caller that walks
    /// [`iter`](Self::iter) draws in vanilla's sequence.
    #[must_use]
    pub fn load() -> Self {
        Self {
            meshes: ArmourSlot::ALL
                .into_iter()
                .map(|slot| (slot, ArmourMesh::for_slot(slot)))
                .collect(),
        }
    }

    /// The baked mesh for a slot.
    #[must_use]
    pub fn get(&self, slot: ArmourSlot) -> Option<&ArmourMesh> {
        self.meshes
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, m)| m)
    }

    /// Every `(slot, mesh)` pair, in submit order (for uploading each once).
    pub fn iter(&self) -> impl Iterator<Item = (ArmourSlot, &ArmourMesh)> {
        self.meshes.iter().map(|(s, m)| (*s, m))
    }
}

/// The texture layers to draw for an item sitting in `slot`, in draw order —
/// empty when this item is not humanoid armour, or is armour for a *different*
/// slot, or its material declares no layers for this slot's layer type.
///
/// The slot equality check is `HumanoidArmorLayer.shouldRender`'s
/// `equippable.slot() == slot` (`HumanoidArmorLayer.java:42-44`): a plugin can
/// put a helmet in the boots slot, and vanilla draws nothing rather than
/// drawing a helmet around the ankles.
#[must_use]
pub fn armour_layers(slot: ArmourSlot, item_path: &str) -> &'static [ArmourLayer] {
    match armour_item(item_path) {
        Some((item_slot, asset)) if item_slot == slot => asset.layers(slot.layer_type()),
        _ => &[],
    }
}

/// The gamma-space RGB a layer multiplies its texel by:
/// `Dyeable.colorWhenUndyed` for a dyeable layer, white for any other.
///
/// This is `EquipmentLayerRenderer.getColorForLayer` with the stack's own
/// `minecraft:dyed_color` **absent**, which is currently always: the wire
/// component is dropped at the shell's `entity_snapshot` boundary, so no dye
/// value can reach here. See `docs/armour-rendering.md` for the wiring that
/// would change that; the only thing needed at this seam is a second argument.
#[must_use]
pub fn armour_layer_tint(layer: &ArmourLayer) -> [u8; 3] {
    layer.dye.unwrap_or([255, 255, 255])
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
/// # This is now a *fallback*, not the only source
///
/// It used to be the only one: `icon.rs` did `resolved.display.get("gui")` and
/// dropped every other slot, so [`ItemGeometry`](crate::ItemGeometry) carried
/// the isometric inventory pose and nothing else. The asset layer now carries
/// all nine slots ([`ItemGeometry::display`](crate::ItemGeometry::display)), and
/// [`ground_transform`] reads the real declared `ground` in preference to this.
///
/// The constants stay because the *fallback still has to be right*: an item
/// whose model chain declares no `ground` at all would otherwise be posed with
/// the identity, i.e. a full-size 1×1×1 block lying in the grass. Being wrong by
/// a factor of four in scale is the visible signature.
///
/// Verified against 26.2's `client.jar`: `models/block/block.json` declares
/// `ground` as `translation [0, 3, 0]`, `scale 0.25`.
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

/// The `display.ground` transform to pose a **dropped** item under: the one the
/// item's own model chain declares, falling back to
/// [`ground_transform_for`]`(gui_light)` when it declares none.
///
/// This is the accessor a drop should use.
/// [`DisplayTransforms::declared`] rather than `get` is the whole point: `get`
/// answers an undeclared slot with the identity, which for `ground` means a
/// full-size block lying in the grass rather than vanilla's quarter-scale one.
/// Distinguishing "the pack said identity" from "we found nothing" is what makes
/// the [`GuiLight`]-keyed guess a fallback instead of dead code.
///
/// # How to change it
///
/// The other slots want exactly this shape — a `hand_transform(&DisplayTransforms,
/// Arm, /* first person */ bool)` for held items, reading
/// `thirdperson_righthand`/`firstperson_righthand` with
/// [`DisplaySlot::left_hand_fallback`](lodestone_assets::DisplaySlot::left_hand_fallback)
/// already handled inside `DisplayTransforms::get`. There is **no** sensible
/// `GuiLight`-keyed fallback for those (`block/block` and `item/generated`
/// disagree on far more than scale), so an undeclared hand slot should draw the
/// identity and be counted, not guessed at.
#[must_use]
pub fn ground_transform(display: &DisplayTransforms, gui_light: GuiLight) -> DisplayTransform {
    display
        .declared(DisplaySlot::Ground)
        .unwrap_or_else(|| ground_transform_for(gui_light))
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
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// [`mesh_item_quads`] followed by the world-light override both
/// [`dropped_item_mesh`] and [`held_item_mesh`] need: the baked geometry nails
/// every vertex to [`GUI_ITEM_LIGHT`](crate::GUI_ITEM_LIGHT) (an inventory slot
/// is full-bright by definition), and a world-placed item is not, so the caller's
/// own world sample overwrites it here, in one place, after meshing.
fn mesh_item_quads_with_light(
    quads: &[BakedQuad],
    pose: Mat4,
    gui_light: GuiLight,
    light: u8,
) -> ModelMesh {
    let mut mesh = mesh_item_quads(quads, pose, gui_light);
    for vertex in &mut mesh.vertices {
        vertex.light = light;
    }
    mesh
}

// ---------------------------------------------------------------------------
// Held items, and the first-person arm
// ---------------------------------------------------------------------------
//
// Both are *item/part geometry hung off an arm*, and both are transcribed from
// the 26.2 client rather than tuned by eye. The two chains are deliberately kept
// separate (`held_item_matrix` vs `first_person_arm_pose`) because vanilla's are:
// one hangs off the third-person part hierarchy, the other replaces it entirely.

/// Which arm of a humanoid rig something is attached to — vanilla's
/// `HumanoidArm`.
///
/// A mob's `getMainArm()` is `RIGHT` for every `Mob` (only a `Player` can be
/// left-handed), so the wire's `MainHand` maps to [`Arm::Right`] and `OffHand`
/// to [`Arm::Left`]. That mapping belongs to the caller, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arm {
    /// The right arm — a mob's main hand.
    Right,
    /// The left arm — a mob's off hand.
    Left,
}

impl Arm {
    /// The `entity_models` part name for this arm.
    #[must_use]
    pub const fn part_name(self) -> &'static str {
        match self {
            Arm::Right => "right_arm",
            Arm::Left => "left_arm",
        }
    }

    /// The overlay ("sleeve") part parented to this arm at `PartPose::ZERO`, for
    /// the models that have one (the two player rigs). It shares the arm's matrix
    /// exactly — see [`first_person_arm_pose`].
    #[must_use]
    pub const fn sleeve_part_name(self) -> &'static str {
        match self {
            Arm::Right => "right_sleeve",
            Arm::Left => "left_sleeve",
        }
    }

    /// Whether this is a left-hand context, i.e. whether
    /// [`display_matrix_for_hand`]'s mirror applies.
    #[must_use]
    pub const fn is_left(self) -> bool {
        matches!(self, Arm::Left)
    }

    /// Vanilla's `invert`/`isLeftHand ? -1 : 1` sign, used for every mirrored
    /// term in both chains below.
    #[must_use]
    pub const fn invert(self) -> f32 {
        match self {
            Arm::Right => 1.0,
            Arm::Left => -1.0,
        }
    }

    /// The `display` slot an item held in this arm is posed by.
    #[must_use]
    pub const fn display_slot(self, first_person: bool) -> DisplaySlot {
        match (self, first_person) {
            (Arm::Right, false) => DisplaySlot::ThirdPersonRightHand,
            (Arm::Left, false) => DisplaySlot::ThirdPersonLeftHand,
            (Arm::Right, true) => DisplaySlot::FirstPersonRightHand,
            (Arm::Left, true) => DisplaySlot::FirstPersonLeftHand,
        }
    }
}

/// `ItemInHandLayer.submitArmWithItem`'s adult hand offset, in model texels
/// (`offsetX`, `offsetY`, `offsetZ`). `x` is mirrored by [`Arm::invert`].
///
/// Read from 26.2's
/// `client/renderer/entity/layers/ItemInHandLayer.java:45-48`, where the three
/// values are `1.0F`, `2.0F` and `-10.0F` and the translate is
/// `((isLeftHand ? -1 : 1) * offsetX / 16, offsetY / 16, offsetZ / 16)`.
pub const HELD_ITEM_OFFSET_TEXELS: [f32; 3] = [1.0, 2.0, -10.0];

/// The same offsets for a **baby** (`useBabyOffset`): `0.0`, `1.0`, `-4.5`.
///
/// Vanilla's predicate is `state.isBaby && state.entityType != ARMOR_STAND`; an
/// armour stand is never a baby in the shell's data, so the caller's
/// "is this mob drawn small?" test is sufficient.
pub const HELD_ITEM_BABY_OFFSET_TEXELS: [f32; 3] = [0.0, 1.0, -4.5];

/// The `display` transform to pose an item held in `arm` under.
///
/// Uses [`DisplayTransforms::get`] rather than `declared`, because unlike
/// `ground` there is **no** sensible fallback constant for a hand slot:
/// `block/block` and `item/generated` disagree on far more than scale, so an
/// undeclared hand slot should get vanilla's own answer — the identity
/// (`ItemTransform.NO_TRANSFORM`, which is only the `-0.5` centring) — and not a
/// guess. `get` also applies
/// [`DisplaySlot::left_hand_fallback`](lodestone_assets::DisplaySlot::left_hand_fallback),
/// which matters in practice: neither `block/block` nor `item/generated` declares
/// `thirdperson_lefthand`.
#[must_use]
pub fn hand_transform(
    display: &DisplayTransforms,
    arm: Arm,
    first_person: bool,
) -> DisplayTransform {
    display.get(arm.display_slot(first_person))
}

/// The world placement matrix for an item held in a mob's hand, matching
/// `ItemInHandLayer.submitArmWithItem`'s pose-stack order exactly:
///
/// ```text
/// part_transforms[arm] · Rx(-90°) · Ry(180°) · T(±ox/16, oy/16, oz/16)
///                      · display_matrix_for_hand(thirdperson_?hand, is_left)
/// ```
///
/// `arm_transform` is vanilla's `translateToHand(arm)` result, an
/// **entity→world** matrix: [`EntityInstance::hand_transform`]`(arm)` — *not*
/// `part_transforms[skeleton.index_of(arm.part_name())]`, which is the same
/// value only for the models with no override (see the table below and
/// [`HandPoseOverride`](crate::entity_anim::HandPoseOverride)).
///
/// # Verified against source, and the three offsets are not the whole story
///
/// Read from the 26.2 decompile, not transcribed from a summary. Two things the
/// short form hides:
///
/// * The item's own `display` transform is **not** applied by the layer — it
///   happens one level down, inside `ItemStackRenderState.LayerRenderState.submit`
///   → `applyTransform` → `itemTransform.apply(displayContext.leftHand(), pose)`.
///   That is why the left-hand mirror lives in [`display_matrix_for_hand`] and is
///   applied here even when the transform came from the right-hand fallback:
///   `ItemDisplayContext.leftHand()` is a property of the *context*, not of where
///   the numbers came from.
/// * `submitArmWithItem` has two further pose steps this does not model, both
///   gated on state the shell does not track: `SpearAnimations.thirdPersonAttackItem`
///   (a `STAB` swing mid-attack) and `ArmPose.animateUseItem` (`ticksUsingItem != 0`,
///   i.e. drawing a bow, eating, blocking with a shield). Both are the identity in
///   the resting case this renders.
///
/// # How to change it: the per-model `translateToHand` overrides
///
/// For most models `arm_transform` is `HumanoidModel.translateToHand`, which
/// `IllagerModel` and `ArmorStandModel` use too, and — because the composed
/// part matrix already carries the *whole* parent chain — also covers models
/// whose arms hang off `body` rather than `root` (`CopperGolemModel` spells out
/// `root · body · arm`). Five corpus models in 26.2 append or prepend more, and
/// [`Skeleton::translate_to_hand`](crate::entity_anim::Skeleton::translate_to_hand)
/// now models every one of them, selected per model name by
/// [`hand_pose_override_for`]:
///
/// | model | override |
/// |---|---|
/// | `skeleton`, `stray`, `wither_skeleton` | pivot `x += ±1` texel *before* the arm's own matrix |
/// | `player_slim` | pivot `x += ±0.5` texel, same position |
/// | `vex` | then `scale(0.55)`, then `translate(±0.046875, -0.15625, 0.078125)` |
/// | `allay` | a different chain entirely: `root · body`, then `T(0, 1/16, 3/16) · Rx(right_arm.xRot) · S(0.7) · T(1/16, 0, 0)` — the arm's matrix is never used |
/// | `copper_golem` | not in the corpus |
///
/// The two *pivot-shift* rows cannot be expressed as a pre- or post-multiplication
/// of the arm's already-composed matrix, because the shift goes between the
/// parent chain and the arm's own rotation, which that matrix has already
/// folded together. That is why the fix lives in `entity_anim`
/// ([`Skeleton::translate_to_hand`](crate::entity_anim::Skeleton::translate_to_hand)),
/// operating on the posed-but-not-yet-composed parts, rather than as a
/// correction applied to `arm_transform` here.
///
/// **Not yet wired to a live server.** `lodestone-shell`'s `merge_held_items`
/// (`crates/lodestone-shell/src/gpu.rs`) still builds `arm_transform` by
/// indexing `instance.part_transforms[skeleton.index_of(arm.part_name())]`
/// directly, which is exactly [`EntityInstance::hand_transform`]'s
/// [`HandPoseOverride::Structural`](crate::entity_anim::HandPoseOverride::Structural)
/// case and therefore still correct for every model but these five. Swapping
/// that one lookup for `instance.hand_transform(arm)` is the remaining step —
/// deliberately left undone here because this file's remit was
/// `lodestone-render` only.
#[must_use]
pub fn held_item_matrix(
    arm_transform: Mat4,
    arm: Arm,
    baby: bool,
    transform: &DisplayTransform,
) -> Mat4 {
    let [ox, oy, oz] = if baby {
        HELD_ITEM_BABY_OFFSET_TEXELS
    } else {
        HELD_ITEM_OFFSET_TEXELS
    };
    arm_transform
        * Mat4::from_rotation_x((-90.0f32).to_radians())
        * Mat4::from_rotation_y(180.0f32.to_radians())
        * Mat4::from_translation(Vec3::new(
            arm.invert() * ox / UNITS_PER_BLOCK,
            oy / UNITS_PER_BLOCK,
            oz / UNITS_PER_BLOCK,
        ))
        * display_matrix_for_hand(transform, arm.is_left())
}

/// Mesh one held item's baked geometry into a world-space [`ModelMesh`], ready
/// for the ordinary [`ModelPipeline`](crate::ModelPipeline) with a *world* camera
/// uniform — the same treatment [`dropped_item_mesh`] gives a drop, and for the
/// same reason (the pose is folded into vertex positions, so there is no
/// per-instance matrix to batch on).
///
/// `light` is the holder's own packed sky/block sample: the geometry comes from
/// [`mesh_item_quads`], which nails every vertex to
/// [`GUI_ITEM_LIGHT`](crate::GUI_ITEM_LIGHT) because an inventory slot is
/// full-bright by definition, and a sword in a zombie's hand in a cave is not.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn held_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    arm_transform: Mat4,
    arm: Arm,
    baby: bool,
    transform: &DisplayTransform,
    light: u8,
) -> ModelMesh {
    let pose = held_item_matrix(arm_transform, arm, baby, transform);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// The arm's forced `zRot` in `AvatarRenderer.renderHand`, in **radians**
/// (`model.rightArm.zRot = 0.1F`, `model.leftArm.zRot = -0.1F`). Mirrored by
/// [`Arm::invert`].
pub const FIRST_PERSON_ARM_Z_ROT: f32 = 0.1;

/// Vertical FOV the first-person arm is projected with, in degrees.
///
/// **Not the player's FOV.** `GameRenderer.renderLevel` sets a *separate*
/// projection for the hand — `hudProjection.setupPerspective(0.05F, 100.0F,
/// cameraState.hudFov, w, h)` — and `Camera.calculateHudFov` is a hard-coded
/// `70.0F` passed through `modifyFovBasedOnDeathOrFluid`. So the arm keeps a
/// constant apparent size while the world FOV changes (sprinting, the FOV
/// slider), which is exactly the behaviour players expect and would be lost by
/// reusing `Camera::projection_matrix`.
pub const HAND_FOV_Y_DEGREES: f32 = 70.0;

/// Near plane for [`hand_projection`] (vanilla's `0.05F`).
pub const HAND_NEAR: f32 = 0.05;

/// Far plane for [`hand_projection`] (vanilla's `100.0F` — *not* the world's
/// render-distance-derived far plane).
pub const HAND_FAR: f32 = 100.0;

/// The projection the first-person arm is drawn with: vanilla's `hudProjection`.
///
/// This is the **whole** transform for the hand pass. `GameRenderer.renderItemInHand`
/// does `poseStack.mulPose(modelViewMatrix.invert())` while pushing
/// `modelViewStack.mul(modelViewMatrix)`, and the shader multiplies
/// `Proj · ModelViewStack · PoseStack` — so the view rotation cancels exactly and
/// the arm pose is already in **camera space**. `modelViewMatrix` there is
/// `cameraState.viewRotationMatrix`, rotation-only, which is why nothing has to
/// undo a camera translation either.
///
/// A view matrix is orthonormal-plus-translation, so `det(view) = +1` and
/// `sign(det(hand_projection)) == sign(det(Camera::view_projection))`. The arm
/// pose must therefore have a **positive** determinant, exactly like a world
/// model matrix and unlike the GUI item pose — see
/// `first_person_arm_pose_preserves_winding`.
#[must_use]
pub fn hand_projection(aspect: f32) -> Mat4 {
    // The *same* constructor `Camera::projection_matrix` uses, so the two cannot
    // disagree about depth range or handedness — `[0,1]` DirectX-style depth, and
    // a negative determinant, which is where the winding invariant comes from.
    glam::camera::rh::proj::directx::perspective(
        HAND_FOV_Y_DEGREES.to_radians(),
        if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.0
        },
        HAND_NEAR,
        HAND_FAR,
    )
}

/// The camera-space chain `ItemInHandRenderer.renderPlayerArm` builds, driven by
/// `attack_anim`.
///
/// `attack_anim` is vanilla's `attackValue` — `Player.getAttackAnim(partialTick)`,
/// i.e. swing progress in `0.0..=1.0`, interpolated from the **tick** clock
/// (`lodestone_entity::pose::EntityPose::attack_anim_lerp`). `0.0` is a fully
/// rested arm and reproduces this function's behaviour before the swing existed,
/// byte for byte, which is what `arm_chain_at_rest_matches_the_static_chain`
/// pins. Values outside the range are clamped rather than extrapolated: the
/// shaping functions below are periodic, so an out-of-range value does not fail,
/// it silently animates something else.
///
/// ```text
/// s  = sqrt(a)                     -- `Mth.sqrt(attackValue)`
/// xs = -0.3 · sin(s·π)
/// ys =  0.4 · sin(s·2π)
/// zs = -0.4 · sin(a·π)
/// yr =  sin(s·π)                   -- `ySwingRotation`
/// zr =  sin(a²·π)                  -- `zSwingRotation`
///
/// T(i·(xs + 0.64000005), ys - 0.6, zs - 0.71999997)
///   · Ry(i·45°) · Ry(i·yr·70°) · Rz(i·zr·-20°)
///   · T(i·-1, 3.6, 3.5) · Rz(i·120°) · Rx(200°) · Ry(i·-135°) · T(i·5.6, 0, 0)
/// ```
///
/// with `i` = [`Arm::invert`].
///
/// # The `sqrt` is the shape of the animation, not a detail
///
/// Three of the five terms are driven by `sqrt(a)` and one by `a²`, and only
/// `zSwingPosition` is linear in `a`. `sin(sqrt(a)·π)` rises far faster than
/// `sin(a·π)` and decays slowly — the arm snaps out and eases back, which is what
/// a swing *reads* as. Substituting a linear ramp gives a symmetric, sluggish
/// pendulum that is visibly not Minecraft, so this is transcribed term by term
/// from `ItemInHandRenderer.renderPlayerArm` in
/// `.cache/mc/26.2/client-src` rather than eyeballed.
///
/// Note `ySwingPosition` uses `2π`, not `π`: over one swing the arm's vertical
/// offset goes up, back through zero, and down again, rather than making a single
/// hump like `x` and `z`.
///
/// The dropped terms and why:
///
/// * `inverseArmHeight` is `swapAnimationScale(item) * (1 - lerp(oHeight, height))`
///   — vanilla's equip/swap raise. It contributes `-0.6 · inverseArmHeight` to `y`.
///   The shell tracks neither the held stack's identity for the local player nor
///   the two interpolated heights, so this is `0`: the arm sits permanently at its
///   fully-equipped height and never dips on a hotbar change.
/// * `submitHandsWithItems` prefixes `Rx((viewXRot - xBob) · 0.1°)` and
///   `Ry((viewYRot - yBob) · 0.1°)`, and `renderItemInHand` prefixes `bobHurt` and
///   `bobView`. All four need state the shell does not have (`xBob`/`yBob`, hurt
///   time, walk distance); all four are the identity when standing still.
/// * `applyItemArmAttackTransform` — the *item*-in-hand swing (`45° + yr·-20°`,
///   `zr'·-20°`, `xzr·-80°`) — is a **different** chain for the case where the
///   main hand is not empty and vanilla draws the item instead of the arm. It is
///   not this one and must not be folded in; see
///   `RenderState::prepare_first_person_arm`'s note on the shell always drawing
///   the empty-hand case.
///
/// There is no `scale` anywhere in the chain, and that is not an omission — the
/// large constants (`3.6`, `3.5`, `5.6`) are in blocks and largely cancel through
/// the three rotations. At rest the composed arm cube lands roughly `0.35..0.9`
/// blocks right, `0.29..0.99` down and `0.44..1.19` forward of the eye, i.e.
/// bottom-right of frame, which is what
/// `the_first_person_arm_lands_in_the_bottom_right_of_frame` pins.
#[must_use]
pub fn first_person_arm_chain(arm: Arm, attack_anim: f32) -> Mat4 {
    let i = arm.invert();
    let ArmSwingTerms {
        x_position,
        y_position,
        z_position,
        y_rotation,
        z_rotation,
    } = ArmSwingTerms::new(attack_anim);
    Mat4::from_translation(Vec3::new(
        i * (x_position + 0.640_000_05),
        y_position - 0.6,
        z_position - 0.719_999_97,
    )) * Mat4::from_rotation_y((i * 45.0).to_radians())
        * Mat4::from_rotation_y((i * y_rotation * 70.0).to_radians())
        * Mat4::from_rotation_z((i * z_rotation * -20.0).to_radians())
        * Mat4::from_translation(Vec3::new(i * -1.0, 3.6, 3.5))
        * Mat4::from_rotation_z((i * 120.0).to_radians())
        * Mat4::from_rotation_x(200.0f32.to_radians())
        * Mat4::from_rotation_y((i * -135.0).to_radians())
        * Mat4::from_translation(Vec3::new(i * 5.6, 0.0, 0.0))
}

/// The five scalars `renderPlayerArm` derives from `attackValue`, split out from
/// [`first_person_arm_chain`] so the *shaping* can be asserted against
/// hand-evaluated vanilla values on its own. Buried inside the matrix product,
/// swapping a `sqrt(a)` for an `a` is invisible: the matrix still moves, still has
/// determinant +1, and still keeps the arm on screen — it just animates wrong.
///
/// Every field is `0.0` at `attack_anim == 0.0`, which is what makes the swing
/// purely additive on top of the rest chain.
struct ArmSwingTerms {
    /// `xSwingPosition`, pre-`invert`: `-0.3 · sin(sqrt(a)·π)`.
    x_position: f32,
    /// `ySwingPosition`: `0.4 · sin(sqrt(a)·2π)` — note the `2π`.
    y_position: f32,
    /// `zSwingPosition`: `-0.4 · sin(a·π)`, the one linear-in-`a` term.
    z_position: f32,
    /// `ySwingRotation`: `sin(sqrt(a)·π)`, scaled by `70°` at the call site.
    y_rotation: f32,
    /// `zSwingRotation`: `sin(a²·π)`, scaled by `-20°` at the call site.
    z_rotation: f32,
}

impl ArmSwingTerms {
    /// `attack_anim` outside `0.0..=1.0` is clamped — see
    /// [`first_person_arm_chain`] on why extrapolating a periodic shaping
    /// function is worse than clamping it.
    fn new(attack_anim: f32) -> Self {
        use std::f32::consts::{PI, TAU};
        let a = attack_anim.clamp(0.0, 1.0);
        let s = a.sqrt();
        Self {
            x_position: -0.3 * (s * PI).sin(),
            y_position: 0.4 * (s * TAU).sin(),
            z_position: -0.4 * (a * PI).sin(),
            y_rotation: (s * PI).sin(),
            z_rotation: (a * a * PI).sin(),
        }
    }
}

/// The camera-space matrix to draw the first-person arm (and its sleeve) with, or
/// `None` if `mesh` has no such arm part.
///
/// ```text
/// first_person_arm_chain(arm, attack_anim) · rest_pose()[arm] · Rz(±0.1)
/// ```
///
/// `AvatarRenderer.renderHand` calls `arm.resetPose()` and then forces
/// `zRot = ±0.1F`, so the arm part itself is drawn from its **authored rest pose**
/// with one rotation replaced — never from the third-person `setupAnim` result.
/// That is why this is a separate function from [`EntityInstance::part_transforms`]
/// and must stay one: the third-person player body needs the animated chain
/// (`HumanoidModel.setupAttackAnimation`, which is
/// [`crate::entity_anim::Skeleton::pose`]'s `attack_anim`), and sharing a code
/// path would silently give one of the two the other's pose.
///
/// **The swing lives in the chain, not in the part pose**, and that is the whole
/// reason both can be animated by the same `attack_anim` number without sharing
/// any code: first person swings the *camera-space chain* the rested arm hangs
/// off, third person swings the *arm part* inside a rested body. Feeding this
/// function's `attack_anim` to `Skeleton::pose`, or vice versa, produces a
/// plausible-looking wrong answer, so the two paths take the same scalar and
/// nothing else.
///
/// `rest_pose()[arm] · Rz(0.1)` is *exact* rather than approximate because
/// `player_wide`'s `right_arm` is `PartPose::offset(-5, 2, 0)` with **zero** rest
/// rotation and hangs directly off an identity root — asserted by
/// `the_player_arm_rest_pose_is_a_pure_translation`, not commented.
///
/// `right_sleeve` is a child of `right_arm` at `PartPose::ZERO`, so it shares this
/// matrix exactly; [`first_person_arm_parts`] returns both indices for one matrix.
#[must_use]
pub fn first_person_arm_pose(mesh: &EntityMesh, arm: Arm, attack_anim: f32) -> Option<Mat4> {
    let index = mesh.skeleton.index_of(arm.part_name())?;
    let rest = mesh.skeleton.rest_pose();
    let local = *rest.get(index)?;
    Some(
        first_person_arm_chain(arm, attack_anim)
            * local
            * Mat4::from_rotation_z(arm.invert() * FIRST_PERSON_ARM_Z_ROT),
    )
}

/// The mesh part indices [`first_person_arm_pose`]'s matrix draws: the arm, and
/// its sleeve overlay when the model has one.
///
/// Empty when the model has no such arm, so a caller can treat "no first-person
/// arm for this rig" as "draw nothing" without a second lookup.
#[must_use]
pub fn first_person_arm_parts(mesh: &EntityMesh, arm: Arm) -> Vec<usize> {
    let Some(index) = mesh.skeleton.index_of(arm.part_name()) else {
        return Vec::new();
    };
    let mut parts = vec![index];
    if let Some(sleeve) = mesh.skeleton.index_of(arm.sleeve_part_name()) {
        parts.push(sleeve);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_assets::Direction;

    fn pig_mesh() -> EntityMesh {
        EntityMesh::from_model(&lodestone_assets::entity_models::pig_model())
    }

    // -----------------------------------------------------------------------
    // Humanoid armour
    // -----------------------------------------------------------------------

    /// Every armour slot must bake real geometry, and every *load-bearing* part
    /// it bakes must attach to a real part of the humanoid rigs that wear
    /// armour. An armour mesh whose parts do not attach draws nothing at all —
    /// the island defect, with a green mesh test.
    ///
    /// `hat` is the one excusable miss: it is the helmet's outermost shell, it
    /// unwraps onto a region measured empty in all nine of 26.2's humanoid
    /// armour sheets, and the corpus `armor_stand` rig deliberately has no `hat`
    /// part at all (vanilla forces `hat.visible = false` there). So it is
    /// required to attach *only* where the wearer has one — which is itself an
    /// assertion, not a shrug.
    #[test]
    fn every_armour_slot_attaches_to_every_humanoid_rig() {
        let set = ArmourModelSet::load();
        let models = EntityModelSet::load();
        for wearer_name in [
            "player_wide",
            "player_slim",
            "zombie",
            "skeleton",
            "armor_stand",
        ] {
            let wearer = models
                .get(wearer_name)
                .unwrap_or_else(|| panic!("{wearer_name} must be in the corpus"));
            assert!(
                wearer_carries_armour(&wearer.skeleton),
                "{wearer_name} must classify as humanoid, or it wears nothing"
            );
            for (slot, mesh) in set.iter() {
                assert!(mesh.quad_count() > 0, "{slot:?} baked no geometry at all");
                let attached: Vec<&'static str> = mesh
                    .attach(&wearer.skeleton)
                    .map(|(range, wearer_index)| {
                        assert!(range.index_count > 0, "{slot:?} attached an empty range");
                        assert!(wearer_index < wearer.skeleton.len());
                        mesh.parts
                            .iter()
                            .find(|(_, r)| *r == range)
                            .map(|(n, _)| *n)
                            .expect("range came from this mesh")
                    })
                    .collect();
                let expected: Vec<&'static str> = mesh
                    .parts
                    .iter()
                    .map(|(n, _)| *n)
                    .filter(|n| *n != "hat" || wearer.skeleton.index_of("hat").is_some())
                    .collect();
                assert_eq!(
                    attached, expected,
                    "{wearer_name} cannot carry every part of {slot:?}"
                );
            }
        }
    }

    /// A non-humanoid rig carries no armour, and that is the correct answer
    /// rather than a fallback: `HumanoidArmorLayer` is only attached to
    /// renderers whose model is a `HumanoidModel`, so a pig handed a chestplate
    /// by a plugin wears nothing in vanilla either.
    ///
    /// The negative control matters here: a pig **does** have `head` and
    /// `body`, so a name-keyed attach would happily bolt a chestplate to it.
    /// That is why the gate is the animation family, and why this asserts the
    /// name lookup would otherwise have succeeded.
    #[test]
    fn a_pig_attaches_no_armour_despite_having_a_body_part() {
        let set = ArmourModelSet::load();
        let pig = pig_mesh();
        assert!(!wearer_carries_armour(&pig.skeleton));
        assert!(
            pig.skeleton.index_of("body").is_some() && pig.skeleton.index_of("head").is_some(),
            "control: the pig must have the parts a name-keyed attach would match"
        );
        for (_, mesh) in set.iter() {
            assert_eq!(mesh.attach(&pig.skeleton).count(), 0);
        }
    }

    /// The armour a wearer draws with is *its own* posed part matrix, so the
    /// world-pose determinant invariant is inherited rather than re-derived:
    /// every matrix an armour layer is drawn under has to be positive, because
    /// `view_projection` left-multiplies and carries the negative sign.
    ///
    /// The reference sign comes from a real camera, not from an assumed
    /// polarity — `CLAUDE.md`'s rule, applied to a world pose.
    #[test]
    fn armour_is_drawn_under_positive_determinant_wearer_matrices() {
        let camera = crate::camera::Camera::default();
        let view_proj_sign = camera.view_projection().determinant().signum();
        assert_eq!(
            view_proj_sign, -1.0,
            "the reference camera must carry the negative sign, or this test is \
             asserting a polarity instead of deriving one"
        );

        let set = ArmourModelSet::load();
        let models = EntityModelSet::load();
        let instance = models
            .resolve("zombie", Vec3::new(3.0, 64.0, -7.0), 37.0, 1.0, &AnimInput {
                head_yaw_deg: 12.0,
                head_pitch_deg: -8.0,
                limb_swing: 3.5,
                limb_swing_amount: 0.9,
                attack_anim: 0.4,
                age_ticks: 42.0,
                aggressive: false,
            })
            .expect("zombie resolves");
        let mesh = models.get("zombie").expect("zombie mesh");
        let mut checked = 0;
        for (_, armour) in set.iter() {
            for (_, wearer_index) in armour.attach(&mesh.skeleton) {
                let m = instance.part_transforms[wearer_index];
                assert!(
                    m.determinant() > 0.0,
                    "armour part matrix determinant must be positive, was {}",
                    m.determinant()
                );
                // And the composed clip transform must then inherit the
                // camera's sign, which is what actually decides facing.
                assert_eq!((camera.view_projection() * m).determinant().signum(), view_proj_sign);
                checked += 1;
            }
        }
        assert!(checked >= 8, "only {checked} armour parts checked");
    }

    /// Layer resolution: two coplanar layers for leather (base + overlay), one
    /// for a plain material, none across slots, none for the head-slot items
    /// vanilla draws through some other layer.
    #[test]
    fn armour_layer_resolution_follows_the_item_and_its_slot() {
        assert_eq!(armour_layers(ArmourSlot::Chest, "leather_chestplate").len(), 2);
        assert_eq!(armour_layers(ArmourSlot::Legs, "leather_leggings").len(), 2);
        assert_eq!(armour_layers(ArmourSlot::Head, "diamond_helmet").len(), 1);
        assert_eq!(armour_layers(ArmourSlot::Head, "turtle_helmet").len(), 1);
        // A helmet forced into the boots slot draws nothing, as
        // `shouldRender`'s slot equality demands.
        assert!(armour_layers(ArmourSlot::Feet, "diamond_helmet").is_empty());
        // Not armour at all.
        assert!(armour_layers(ArmourSlot::Head, "carved_pumpkin").is_empty());
        assert!(armour_layers(ArmourSlot::Chest, "elytra").is_empty());
        assert!(armour_layers(ArmourSlot::Chest, "wolf_armor").is_empty());
        assert!(armour_layers(ArmourSlot::Head, "stone").is_empty());
    }

    /// Only leather's base layer is tinted, and it is tinted to vanilla's
    /// `color_when_undyed`. White for everything else — a tint of `[0,0,0]`
    /// would be black armour and a tint applied to the overlay would recolour
    /// the buckles.
    #[test]
    fn only_leathers_base_layer_carries_a_tint() {
        let leather = armour_layers(ArmourSlot::Chest, "leather_chestplate");
        assert_eq!(
            armour_layer_tint(&leather[0]),
            lodestone_assets::equipment::UNDYED_LEATHER_RGB
        );
        assert_eq!(armour_layer_tint(&leather[1]), [255, 255, 255]);
        let diamond = armour_layers(ArmourSlot::Head, "diamond_helmet");
        assert_eq!(armour_layer_tint(&diamond[0]), [255, 255, 255]);
    }

    /// The two vanilla anchor values, hand-derived from the decompiled curve
    /// rather than from this implementation, so agreement is evidence rather
    /// than a tautology:
    ///
    /// * noon (6000): `timeOfDay` = 0 → `cos(0)*2 + 0.2` = 2.2 → `1 - 2.2` =
    ///   −1.2 → clamps to 0 → `1 - 0` = 1 → `1*0.8 + 0.2` = 1.0 → lift = 1.0.
    /// * midnight (18000): `timeOfDay` = 0.5 → `cos(π)*2 + 0.2` = −1.8 →
    ///   `1 + 1.8` = 2.8 → clamps to 1 → `1 - 1` = 0 → `0*0.8 + 0.2` = 0.2 →
    ///   lift = `0.2*0.95 + 0.05` = 0.24.
    #[test]
    fn sky_darken_hits_vanillas_noon_and_midnight_anchors() {
        assert!((sky_darken_for_time_of_day(6_000) - 1.0).abs() < 1e-5);
        assert!((sky_darken_for_time_of_day(18_000) - 0.24).abs() < 1e-5);
    }

    /// A large world age must reduce into the day, not drift: `time_of_day`
    /// keeps counting past 24000 for the life of a world, and a curve that read
    /// it raw would eventually saturate at one end and stop darkening at all —
    /// a bug that only appears on a world that has been running for a while,
    /// i.e. never in a test and always for the player.
    #[test]
    fn sky_darken_reduces_a_large_world_age_into_the_day() {
        assert_eq!(
            sky_darken_for_time_of_day(18_000),
            sky_darken_for_time_of_day(18_000 + 24_000 * 500)
        );
        assert_eq!(
            sky_darken_for_time_of_day(6_000),
            sky_darken_for_time_of_day(6_000 - 24_000 * 500)
        );
    }

    /// The curve must stay inside vanilla's `[0.24, 1.0]` across a whole day and
    /// must actually *vary* — a constant 1.0 is the shipped bug, and a value
    /// that ever reaches 0.0 would collide with the shader's "not wired"
    /// sentinel and silently mean full daylight at exactly the darkest moment.
    #[test]
    fn sky_darken_stays_in_vanillas_range_and_is_not_constant() {
        let samples: Vec<f32> = (0..24_000)
            .step_by(50)
            .map(sky_darken_for_time_of_day)
            .collect();
        let lo = samples.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = samples.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(lo >= 0.24 - 1e-5, "dipped to {lo}, below vanilla's 0.24 floor");
        assert!(hi <= 1.0 + 1e-5, "rose to {hi}, above 1.0");
        assert!(lo > 0.0, "0.0 is the shader's 'unset' sentinel and must be unreachable");
        assert!(hi - lo > 0.5, "the curve barely moves ({lo}..{hi}) — that is the defect");
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

    /// The declared slot must **win**, and the fallback must still fire — the
    /// second half is the control. Without it a `ground_transform` that ignored
    /// its `display` argument entirely would pass on every vanilla item, because
    /// the constants happen to equal what vanilla declares.
    #[test]
    fn a_declared_ground_slot_beats_the_gui_light_fallback() {
        let odd = DisplayTransform {
            rotation: [0.0, 17.0, 0.0],
            translation: [0.0, 9.0, 0.0],
            scale: [0.125, 0.125, 0.125],
        };
        let declared = DisplayTransforms::NONE.with(DisplaySlot::Ground, odd);
        assert_eq!(
            ground_transform(&declared, GuiLight::Side),
            odd,
            "the model's own display.ground must be used, not the constant"
        );

        // Control: a chain that declares nothing falls back, and the two
        // fallbacks are still told apart by gui_light.
        assert_eq!(
            ground_transform(&DisplayTransforms::NONE, GuiLight::Side),
            BLOCK_ITEM_GROUND
        );
        assert_eq!(
            ground_transform(&DisplayTransforms::NONE, GuiLight::Front),
            GENERATED_ITEM_GROUND
        );

        // And an *explicitly declared* identity is honoured rather than being
        // mistaken for "absent" — the trap `DisplayTransforms::get` would fall
        // into here, since `get` cannot tell the two apart.
        let flat = DisplayTransforms::NONE.with(DisplaySlot::Ground, DisplayTransform::default());
        assert_eq!(
            ground_transform(&flat, GuiLight::Side),
            DisplayTransform::default()
        );
    }

    /// A left-hand slot with no left-hand data must mirror onto the right-hand
    /// one, as vanilla's `ItemTransforms.Deserializer` does. `block/block` and
    /// `item/generated` both declare `thirdperson_righthand` and no
    /// `thirdperson_lefthand`, so without this every block in an off hand would
    /// be posed with the identity.
    // ---- held items, and the first-person arm ---------------------------

    fn player_mesh() -> EntityMesh {
        EntityMesh::from_named_model(
            "player_wide",
            &lodestone_assets::entity::player_model(false),
        )
    }

    /// A plausible `thirdperson_righthand`: vanilla's `item/handheld` declares
    /// `rotation [0, -90, 55]`, `translation [0, 4, 0.5]`, `scale [0.85, …]`.
    fn handheld_third_person() -> DisplayTransform {
        DisplayTransform {
            rotation: [0.0, -90.0, 55.0],
            translation: [0.0, 4.0, 0.5],
            scale: [0.85, 0.85, 0.85],
        }
    }

    #[test]
    fn the_held_item_offsets_are_vanillas_two_triples() {
        // Guard against a transposed or halved transcription of
        // `ItemInHandLayer:45-48`, which is the whole content of this constant.
        assert_eq!(HELD_ITEM_OFFSET_TEXELS, [1.0, 2.0, -10.0]);
        assert_eq!(HELD_ITEM_BABY_OFFSET_TEXELS, [0.0, 1.0, -4.5]);
    }

    #[test]
    fn the_held_item_x_offset_mirrors_between_hands_and_nothing_else_does() {
        // The only asymmetry the layer itself introduces is `±offsetX`. Isolate
        // it by handing both arms the *same* identity display transform and the
        // same arm matrix, so any other difference would have to come from this
        // function.
        let flat = DisplayTransform::default();
        let right = held_item_matrix(Mat4::IDENTITY, Arm::Right, false, &flat);
        let left = held_item_matrix(Mat4::IDENTITY, Arm::Left, false, &flat);
        let r = right.transform_point3(Vec3::splat(0.5));
        let l = left.transform_point3(Vec3::splat(0.5));
        // Rx(-90) then Ry(180) sends the +x offset to -x, so the sign is flipped
        // once more than the naive reading — which is exactly why this is
        // measured rather than asserted from the constant.
        assert!((r.y - l.y).abs() < 1e-6, "y must not mirror: {r} vs {l}");
        assert!((r.z - l.z).abs() < 1e-6, "z must not mirror: {r} vs {l}");
        assert!(
            (r.x + l.x).abs() < 1e-6 && r.x.abs() > 1e-3,
            "x must mirror about zero and be non-zero: {r} vs {l}"
        );
    }

    #[test]
    fn a_baby_holds_its_item_closer_in() {
        // The baby triple is smaller on every axis, so the item sits nearer the
        // shoulder. A swapped adult/baby branch is the failure this catches.
        let t = handheld_third_person();
        let adult = held_item_matrix(Mat4::IDENTITY, Arm::Right, false, &t)
            .transform_point3(Vec3::splat(0.5));
        let baby =
            held_item_matrix(Mat4::IDENTITY, Arm::Right, true, &t).transform_point3(Vec3::splat(0.5));
        assert!(
            baby.length() < adult.length(),
            "the baby offset must be nearer the pivot: {baby} vs {adult}"
        );
    }

    #[test]
    fn the_held_item_pose_hangs_off_the_arm_matrix_it_is_given() {
        // The seam that makes this non-island: the caller passes
        // `part_transforms[arm]`, and translating that must translate the item
        // by exactly the same amount.
        let t = handheld_third_person();
        let base = held_item_matrix(Mat4::IDENTITY, Arm::Right, false, &t);
        let shift = Vec3::new(3.0, 64.0, -7.0);
        let moved = held_item_matrix(
            Mat4::from_translation(shift),
            Arm::Right,
            false,
            &t,
        );
        let a = base.transform_point3(Vec3::splat(0.5));
        let b = moved.transform_point3(Vec3::splat(0.5));
        assert!((b - a - shift).length() < 1e-4, "{a} -> {b}, expected +{shift}");
    }

    #[test]
    fn the_held_item_pose_preserves_winding_for_a_real_mob() {
        // Same discipline as `dropped_item_pose_preserves_winding`: the whole
        // chain is a *world* pose left-multiplied by `view_projection`, so its
        // determinant must be POSITIVE and the composition must inherit the
        // camera's negative sign. The GUI rule ("negative") applied here ships an
        // item you see the inside of, which still looks like a sword.
        let camera = crate::camera::Camera {
            position: Vec3::new(0.5, 1.0, 4.0),
            yaw: 180.0,
            pitch: 0.0,
            ..crate::camera::Camera::default()
        };
        let world = camera.view_projection();
        let front_sign = screen_area(world, cube_face(Direction::South)).signum();
        assert_eq!(
            screen_area(world, cube_face(Direction::North)).signum(),
            -front_sign,
            "the reference camera must disagree about the far face"
        );

        let mesh = player_mesh();
        let t = handheld_third_person();
        for yaw in [0.0f32, 37.0, 180.0, 271.0] {
            for (scale, baby) in [(1.0f32, false), (0.5, true)] {
                let inst = EntityInstance::new(
                    "player_wide",
                    &mesh,
                    Vec3::new(0.5, 0.0, 0.0),
                    yaw,
                    scale,
                    &AnimInput::REST,
                );
                for hand in [Arm::Right, Arm::Left] {
                    let arm_transform = inst
                        .hand_transform(hand)
                        .expect("player_wide has both arms");
                    let pose = held_item_matrix(arm_transform, hand, baby, &t);
                    assert!(
                        pose.determinant() > 0.0,
                        "a world-space held-item pose must not flip handedness; det = {} \
                         (yaw {yaw}, scale {scale}, {hand:?})",
                        pose.determinant()
                    );
                    assert_eq!(
                        (world * pose).determinant().signum(),
                        world.determinant().signum(),
                        "view_projection * pose must keep the camera's winding"
                    );
                }
            }
        }
    }

    /// The whole reason [`EntityInstance::hand_transform`] exists rather than
    /// reusing `part_transforms[skeleton.index_of(arm.part_name())]`: for a
    /// skeleton the two must actually differ (by the pivot shift), and the
    /// arm's *own* body-mesh transform (`part_transforms`) must stay exactly
    /// what it was — proof this crate's override never leaks into the
    /// whole-body draw it shares an index with.
    #[test]
    fn a_skeletons_hand_transform_differs_from_its_arms_body_transform() {
        let mesh = EntityMesh::from_named_model(
            "skeleton",
            &lodestone_assets::entity_models::skeleton_model(),
        );
        let inst = EntityInstance::new(
            "skeleton",
            &mesh,
            Vec3::new(0.5, 0.0, 0.0),
            0.0,
            1.0,
            &AnimInput::REST,
        );
        let arm_idx = mesh.skeleton.index_of("right_arm").unwrap();
        let body_mesh_transform = inst.part_transforms[arm_idx];
        let hand_transform = inst.hand_transform(Arm::Right).unwrap();
        assert!(
            (hand_transform.w_axis - body_mesh_transform.w_axis).length() > 1e-4,
            "the pivot shift did not reach the hand transform: {hand_transform:?} vs \
             {body_mesh_transform:?}"
        );
    }

    #[test]
    fn hand_transform_picks_the_slot_the_arm_and_person_name() {
        let third = handheld_third_person();
        let first = DisplayTransform {
            rotation: [0.0, -90.0, 25.0],
            translation: [1.13, 3.2, 1.13],
            scale: [0.68, 0.68, 0.68],
        };
        let d = DisplayTransforms::NONE
            .with(DisplaySlot::ThirdPersonRightHand, third)
            .with(DisplaySlot::FirstPersonRightHand, first);
        assert_eq!(hand_transform(&d, Arm::Right, false), third);
        assert_eq!(hand_transform(&d, Arm::Right, true), first);
        // Both left slots are undeclared, so both fall back to their right-hand
        // partner — vanilla's deserializer rule, and the reason a block in an off
        // hand is not identity-posed.
        assert_eq!(hand_transform(&d, Arm::Left, false), third);
        assert_eq!(hand_transform(&d, Arm::Left, true), first);
        // ...and a model that declares nothing gets NO_TRANSFORM, not a guess.
        assert_eq!(
            hand_transform(&DisplayTransforms::NONE, Arm::Right, false),
            DisplayTransform::default()
        );
    }

    #[test]
    fn the_player_arm_rest_pose_is_a_pure_translation() {
        // What makes `rest_pose()[arm] * Rz(±0.1)` *exact* rather than an
        // approximation of `arm.resetPose(); arm.zRot = ±0.1`: the authored rest
        // rotation is zero and the root above it is the identity, so replacing
        // zRot is the same as post-multiplying Rz. If a future corpus edit gave
        // the player arm a rest rotation, this fails instead of silently drifting.
        let mesh = player_mesh();
        for arm in [Arm::Right, Arm::Left] {
            let i = mesh.skeleton.index_of(arm.part_name()).expect("arm part");
            let rest = mesh.skeleton.rest_pose()[i];
            let expect = Mat4::from_translation(Vec3::new(
                arm.invert() * -5.0 / 16.0,
                2.0 / 16.0,
                0.0,
            ));
            assert!(
                (rest - expect).to_cols_array().iter().all(|v| v.abs() < 1e-6),
                "{arm:?} rest pose must be PartPose::offset(∓5, 2, 0) with no rotation; got {rest}"
            );
        }
    }

    #[test]
    fn the_sleeve_shares_the_arms_matrix_exactly() {
        // `right_sleeve` is `PartPose::ZERO` under `right_arm`, so one uploaded
        // matrix drives both parts. Drawing the sleeve with its own recomputed
        // matrix would be the same number; drawing it with the *body's* would
        // put a floating sleeve mid-screen.
        let mesh = player_mesh();
        let rest = mesh.skeleton.rest_pose();
        for arm in [Arm::Right, Arm::Left] {
            let a = mesh.skeleton.index_of(arm.part_name()).expect("arm");
            let s = mesh
                .skeleton
                .index_of(arm.sleeve_part_name())
                .expect("sleeve");
            assert!(
                (rest[a] - rest[s]).to_cols_array().iter().all(|v| v.abs() < 1e-6),
                "{arm:?} sleeve must share the arm's matrix"
            );
            let parts = first_person_arm_parts(&mesh, arm);
            assert_eq!(parts, vec![a, s], "both parts must be drawn, arm first");
        }
        // A rig with no sleeve yields just the arm, and a rig with no arm at all
        // yields nothing — the control that keeps the `Vec` honest.
        assert_eq!(
            first_person_arm_parts(&pig_mesh(), Arm::Right),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn the_first_person_arm_lands_in_the_bottom_right_of_frame() {
        // Hand-computed from `renderPlayerArm`'s chain with attack = 0 and
        // inverseArmHeight = 0, in camera space (x right, y up, -z forward):
        // the arm cube spans roughly x 0.33..0.91, y -0.99..-0.29, z -1.19..-0.44.
        // The load-bearing claims are the *signs*: right of centre, below the
        // eye, and in front of it. A missing rotation in the chain flips one.
        let mesh = player_mesh();
        let pose =
            first_person_arm_pose(&mesh, Arm::Right, 0.0).expect("player_wide has a right arm");
        // `player_wide`'s right arm cube: from [-3, -2, -2], size [4, 12, 4].
        let corners: Vec<Vec3> = (0..8u32)
            .map(|i| {
                let x = if i & 1 == 0 { -3.0f32 } else { 1.0 };
                let y = if i & 2 == 0 { -2.0f32 } else { 10.0 };
                let z = if i & 4 == 0 { -2.0f32 } else { 2.0 };
                pose.transform_point3(Vec3::new(x, y, z) / 16.0)
            })
            .collect();
        let lo = corners.iter().copied().reduce(Vec3::min).unwrap();
        let hi = corners.iter().copied().reduce(Vec3::max).unwrap();
        assert!(lo.x > 0.2 && hi.x < 1.1, "x span {}..{}", lo.x, hi.x);
        assert!(hi.y < -0.2 && lo.y > -1.2, "y span {}..{}", lo.y, hi.y);
        assert!(hi.z < -0.3 && lo.z > -1.4, "z span {}..{}", lo.z, hi.z);
        // Beyond the near plane, or the arm is clipped away entirely.
        assert!(hi.z < -HAND_NEAR, "the arm must be past the near plane");

        // The left arm is the mirror image about x, to within the zRot sign.
        let left = first_person_arm_pose(&mesh, Arm::Left, 0.0).expect("left arm");
        let lc = left.transform_point3(Vec3::ZERO);
        let rc = pose.transform_point3(Vec3::ZERO);
        assert!((lc.x + rc.x).abs() < 1e-4, "left/right must mirror: {lc} vs {rc}");
        assert!((lc.y - rc.y).abs() < 1e-4 && (lc.z - rc.z).abs() < 1e-4);
    }

    #[test]
    fn first_person_arm_pose_preserves_winding() {
        // The arm is drawn with the HUD projection alone, and a view matrix has
        // determinant +1, so `sign(det(hand_projection))` equals
        // `sign(det(view_projection))`. The arm pose must therefore be
        // orientation-*preserving*, like a world model matrix — not
        // orientation-reversing like the GUI item pose.
        let camera = crate::camera::Camera::default();
        let world = camera.view_projection();
        assert!(
            camera.view_matrix().determinant() > 0.0,
            "a view matrix must have determinant +1; that is why the two signs agree"
        );
        let proj = hand_projection(16.0 / 9.0);
        assert_eq!(
            proj.determinant().signum(),
            world.determinant().signum(),
            "hand_projection must share view_projection's handedness \
             (proj {}, world {})",
            proj.determinant(),
            world.determinant()
        );

        let mesh = player_mesh();
        for arm in [Arm::Right, Arm::Left] {
            // Every phase of the swing, not just rest: a rotation cannot change a
            // determinant's sign, but the chain is edited by hand and a stray
            // reflection (a negated scale, a mirrored translation folded into a
            // rotation) would only show up mid-swing.
            for step in 0..=8 {
                let attack = step as f32 / 8.0;
                let pose = first_person_arm_pose(&mesh, arm, attack).expect("arm");
                assert!(
                    pose.determinant() > 0.0,
                    "{arm:?} arm pose must not flip handedness at attack {attack}; det = {}",
                    pose.determinant()
                );
                assert_eq!(
                    (proj * pose).determinant().signum(),
                    world.determinant().signum(),
                    "hand_projection * arm pose must keep the world's winding at attack {attack}"
                );
            }
        }
    }

    /// The swing must be **additive**: `attack_anim == 0` has to reproduce the
    /// pre-swing chain exactly, or every existing framing assertion above (and the
    /// shell's headless arm gate) is silently measuring a different arm.
    ///
    /// The expected matrix is written out longhand rather than taken from
    /// `first_person_arm_chain` itself — comparing the function to itself would
    /// pass for any pair of symmetric mistakes.
    #[test]
    fn arm_chain_at_rest_matches_the_static_chain() {
        for arm in [Arm::Right, Arm::Left] {
            let i = arm.invert();
            let expected = Mat4::from_translation(Vec3::new(i * 0.640_000_05, -0.6, -0.719_999_97))
                * Mat4::from_rotation_y((i * 45.0).to_radians())
                * Mat4::from_translation(Vec3::new(i * -1.0, 3.6, 3.5))
                * Mat4::from_rotation_z((i * 120.0).to_radians())
                * Mat4::from_rotation_x(200.0f32.to_radians())
                * Mat4::from_rotation_y((i * -135.0).to_radians())
                * Mat4::from_translation(Vec3::new(i * 5.6, 0.0, 0.0));
            let actual = first_person_arm_chain(arm, 0.0);
            let delta = (expected - actual)
                .to_cols_array()
                .iter()
                .fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(delta < 1e-5, "{arm:?} rest chain drifted by {delta}");
        }
        // The control: something must actually change once the swing is running,
        // or "rest matches" is satisfied by a chain that ignores `attack_anim`.
        let moved = (first_person_arm_chain(Arm::Right, 0.0)
            - first_person_arm_chain(Arm::Right, 0.4))
        .to_cols_array()
        .iter()
        .fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(moved > 0.05, "the swing must move the chain, moved by {moved}");
    }

    /// The five swing scalars against hand-evaluated vanilla values.
    ///
    /// `a = 0.25` is chosen because `sqrt(0.25) = 0.5` **exactly**, so every
    /// expected number below is a closed form off the unit circle rather than
    /// something read back out of this code:
    ///
    /// ```text
    /// xs = -0.3 · sin(0.5π)    = -0.3 · 1          = -0.3
    /// ys =  0.4 · sin(1.0π)    =  0.4 · 0          =  0.0
    /// zs = -0.4 · sin(0.25π)   = -0.4 · √2/2       = -0.28284271
    /// yr =        sin(0.5π)    =  1                =  1.0
    /// zr =        sin(0.0625π) =  sin(11.25°)      =  0.19509032
    /// ```
    ///
    /// This is where the `sqrt` shaping is actually pinned. A linear ramp gives
    /// `xs = -0.3·sin(0.25π) = -0.212`, `yr = 0.707` instead of `1.0` — the arm
    /// still swings, just wrongly, which is exactly the failure the matrix-level
    /// and pixel-level gates cannot distinguish.
    ///
    /// `ys == 0` here is not a weak assertion, it is the `2π` term crossing zero
    /// a quarter of the way in; a `π` typo would give `0.4` and fail loudly.
    #[test]
    fn arm_swing_terms_match_hand_evaluated_vanilla() {
        let t = ArmSwingTerms::new(0.25);
        assert!((t.x_position - -0.3).abs() < 1e-6, "xs {}", t.x_position);
        assert!(t.y_position.abs() < 1e-6, "ys {}", t.y_position);
        assert!(
            (t.z_position - -0.282_842_71).abs() < 1e-6,
            "zs {}",
            t.z_position
        );
        assert!((t.y_rotation - 1.0).abs() < 1e-6, "yr {}", t.y_rotation);
        assert!(
            (t.z_rotation - 0.195_090_32).abs() < 1e-6,
            "zr {}",
            t.z_rotation
        );

        // At a = 1.0 the arm is back at rest in x and y (both `sin` arguments are
        // whole multiples of π) — the property that makes the wrapped
        // `attack_anim_lerp` in `lodestone_entity::pose` land the arm at rest
        // rather than mid-arc.
        let end = ArmSwingTerms::new(1.0);
        assert!(end.x_position.abs() < 1e-6, "xs at end {}", end.x_position);
        assert!(end.y_position.abs() < 1e-6, "ys at end {}", end.y_position);
        assert!(end.y_rotation.abs() < 1e-6, "yr at end {}", end.y_rotation);

        // Every term is zero at rest, which is what `arm_chain_at_rest_matches_
        // the_static_chain` depends on.
        let rest = ArmSwingTerms::new(0.0);
        for (name, v) in [
            ("xs", rest.x_position),
            ("ys", rest.y_position),
            ("zs", rest.z_position),
            ("yr", rest.y_rotation),
            ("zr", rest.z_rotation),
        ] {
            assert_eq!(v, 0.0, "{name} must be 0 at rest");
        }

        // Out of range clamps rather than extrapolating.
        assert_eq!(ArmSwingTerms::new(-1.0).y_rotation, rest.y_rotation);
        assert_eq!(ArmSwingTerms::new(4.0).y_rotation, end.y_rotation);
    }

    // ---- the deferred third-person body: `EntityInstance::part_transforms`,
    // not `first_person_arm_pose` -- see that function's doc comment for why
    // sharing a code path would silently give one of the two the other's pose.

    fn player_slim_mesh() -> EntityMesh {
        EntityMesh::from_named_model("player_slim", &lodestone_assets::entity::player_model(true))
    }

    #[test]
    fn player_model_name_selects_wide_or_slim() {
        assert_eq!(player_model_name(false), "player_wide");
        assert_eq!(player_model_name(true), "player_slim");
        // Both names must be real corpus entries in their own right (not just
        // `canonical_model_name`'s hidden alias target), since a caller with
        // real skin data passes this straight through as a `type_path`.
        assert_eq!(model_for_type("player_wide").unwrap().name, "player_wide");
        assert_eq!(model_for_type("player_slim").unwrap().name, "player_slim");
    }

    /// Vanilla draws two layers per limb: the base skin cube, and a slightly
    /// `grow`n overlay (`hat`/`jacket`/`right_sleeve`/`left_sleeve`/
    /// `right_pants`/`left_pants`) parented to it at `PartPose::ZERO`.
    /// Omitting the overlay looks like a missing-skin-layer bug, not a missing
    /// feature, so this pins that every overlay part is (a) present in the
    /// baked mesh and (b) posed *exactly* onto its base part by the animated
    /// third-person chain -- not just at rest, where a `ZERO`-pose child would
    /// trivially agree with its parent even if the composition were wrong.
    #[test]
    fn outer_layer_parts_follow_their_base_part_exactly() {
        for (name, mesh) in [("player_wide", player_mesh()), ("player_slim", player_slim_mesh())] {
            let anim = AnimInput {
                head_yaw_deg: 25.0,
                head_pitch_deg: -15.0,
                limb_swing: 3.7,
                limb_swing_amount: 1.0,
                attack_anim: 0.0,
                age_ticks: 40.0,
                aggressive: false,
            };
            let instance =
                EntityInstance::new(name, &mesh, Vec3::new(1.0, 0.0, 2.0), 37.0, 1.0, &anim);
            let pairs = [
                ("head", "hat"),
                ("body", "jacket"),
                ("right_arm", "right_sleeve"),
                ("left_arm", "left_sleeve"),
                ("right_leg", "right_pants"),
                ("left_leg", "left_pants"),
            ];
            for (base, overlay) in pairs {
                let bi = mesh.skeleton.index_of(base).unwrap_or_else(|| panic!("{name}.{base}"));
                let oi =
                    mesh.skeleton.index_of(overlay).unwrap_or_else(|| panic!("{name}.{overlay}"));
                let b = instance.part_transforms[bi].to_cols_array();
                let o = instance.part_transforms[oi].to_cols_array();
                for i in 0..16 {
                    assert!(
                        (b[i] - o[i]).abs() < 1e-5,
                        "{name}: {overlay} must be posed exactly onto {base} (a PartPose::ZERO \
                         child), element {i} differs: {} vs {}",
                        b[i],
                        o[i]
                    );
                }
            }
        }
    }

    /// The whole-body third-person chain is
    /// `entity_model_matrix(feet, yaw, scale) * Skeleton::pose(anim)[part]`
    /// (see [`EntityInstance::new`]) -- the *same* `scale(-1,-1,1)`-carrying
    /// placement matrix the module doc already proves has determinant `+1` for
    /// any rigid part chain, just exercised over every part of a real player
    /// mesh (including the outer-layer overlays) instead of asserted once in
    /// prose. A negative determinant here would mean a player rendered
    /// inside-out the moment a third-person camera exists to look at one.
    #[test]
    fn third_person_body_part_transforms_preserve_winding() {
        for (name, mesh) in [("player_wide", player_mesh()), ("player_slim", player_slim_mesh())] {
            for yaw in [0.0, 47.0, 90.0, 181.0, 300.0] {
                let anim = AnimInput {
                    limb_swing: yaw * 0.1,
                    limb_swing_amount: 1.0,
                    ..AnimInput::REST
                };
                let instance =
                    EntityInstance::new(name, &mesh, Vec3::new(3.0, 5.0, -2.0), yaw, 1.0, &anim);
                assert!(
                    !instance.part_transforms.is_empty(),
                    "{name}: expected a non-empty part chain"
                );
                for (i, part) in instance.part_transforms.iter().enumerate() {
                    assert!(
                        part.determinant() > 0.0,
                        "{name} part {i} at yaw {yaw}: determinant must be positive, was {}",
                        part.determinant()
                    );
                }
            }
        }
    }

    #[test]
    fn hand_projection_is_a_fixed_seventy_degrees_and_survives_a_degenerate_aspect() {
        // Vanilla's `calculateHudFov` is a constant 70, so the arm must NOT
        // follow the world FOV. Anything reading `Camera::fov_y_degrees` here
        // would make the arm balloon while sprinting.
        assert!((HAND_FOV_Y_DEGREES - 70.0).abs() < 1e-6);
        assert!((HAND_NEAR - 0.05).abs() < 1e-6);
        assert!((HAND_FAR - 100.0).abs() < 1e-6);
        assert!(hand_projection(0.0).to_cols_array().iter().all(|v| v.is_finite()));
        assert!(hand_projection(f32::NAN).to_cols_array().iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_missing_left_hand_slot_falls_back_to_the_right_hand_one() {
        let right = DisplayTransform {
            rotation: [75.0, 45.0, 0.0],
            translation: [0.0, 2.5, 0.0],
            scale: [0.375, 0.375, 0.375],
        };
        let d = DisplayTransforms::NONE.with(DisplaySlot::ThirdPersonRightHand, right);
        assert_eq!(d.get(DisplaySlot::ThirdPersonLeftHand), right);
        assert_eq!(
            d.declared(DisplaySlot::ThirdPersonLeftHand),
            None,
            "the fallback must not pretend the slot was declared"
        );
        // A slot with no fallback rule still reads as the identity.
        assert_eq!(
            d.get(DisplaySlot::Ground),
            DisplayTransform::default(),
            "an undeclared non-hand slot is vanilla's NO_TRANSFORM"
        );
    }
}
