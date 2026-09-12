use super::*;

/// One resolved entity instance, including its world transform and culling box.
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
    /// The `entity → world` hand-translate matrix for `[Arm::Right,
    /// Arm::Left]`, honoring this model's [`HandPoseOverride`] — `None` for an
    /// arm the model doesn't have.
    ///
    /// **Prefer this over indexing `part_transforms` by
    /// `skeleton.index_of(arm.part_name())` when placing a held item.** For
    /// [`HandPoseOverride::Structural`] the two are numerically identical, but
    /// for the five corpus models with a real override they are not, and
    /// cannot be made to be: the override is scoped to the hand-translate step
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
        Self::new_animated(model, mesh, feet, yaw_deg, scale, anim, 0.0, 0.0)
    }

    /// [`new`](Self::new) with the two per-entity animation states that are neither
    /// placement nor skeletal pose: a creeper's **swell** fraction (vanilla's
    /// per-tick swelling progress) and a dying entity's **`death_time`**
    /// (`deathTime + partialTicks`, `0.0` while alive).
    ///
    /// A separate constructor rather than two more arguments on [`new`](Self::new),
    /// for [`new_projectile`](Self::new_projectile)'s reason inverted: here the
    /// variants really *are* one placement with options, and **both** extras have a
    /// documented exact identity at `0.0` —
    /// [`Skeleton::pose_swelling`](crate::entity_anim::Skeleton::pose_swelling)
    /// delegates `pose` to itself at zero swell, and
    /// [`dying_entity_model_matrix`] reduces to [`entity_model_matrix`] at zero
    /// roll. So the five call sites with nothing to pass keep working
    /// bit-identically instead of being widened for symmetry.
    ///
    /// The two land in different places, which is why they are not one value: the
    /// swell reaches the **pose** (a scale about the model-space feet plane composed
    /// above the root part, so a creeper grows upward out of the ground rather than
    /// moving), while the fall-over reaches the **placement**, between the body yaw
    /// and the Y-down flip.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "one argument per independent piece of a live entity's placement, \
                  pose and animation state; a bundle struct would move the same \
                  fields behind a name that adds nothing"
    )]
    pub fn new_animated(
        model: &'static str,
        mesh: &EntityMesh,
        feet: Vec3,
        yaw_deg: f32,
        scale: f32,
        anim: &AnimInput,
        swell: f32,
        death_time: f32,
    ) -> Self {
        Self::placed(
            model,
            mesh,
            dying_entity_model_matrix(
                feet,
                yaw_deg,
                scale,
                crate::entity_anim::death_fall_over_degrees(death_time),
            ),
            anim,
            swell,
        )
    }

    /// Build an instance for a **projectile** — a model
    /// [`projectile_pitch_offset_deg`] recognises — at `pos`/`yaw`/`pitch`/`scale`,
    /// placed by [`projectile_model_matrix`] instead of [`entity_model_matrix`].
    ///
    /// Separate constructor rather than a `pitch` argument on [`new`](Self::new)
    /// because the two placements share no ops at all: no flip, no
    /// [`MODEL_FEET_OFFSET`] lift, a different yaw term, and a rotation
    /// [`new`](Self::new) has no concept of. A single function with a "is it a
    /// projectile" branch inside would read as one placement with an option, when
    /// it is two placements from two different vanilla classes.
    ///
    /// `anim` is accepted (and forwarded) for uniformity, but every projectile rig
    /// classifies as [`AnimFamily::Static`](crate::entity_anim::AnimFamily::Static)
    /// — an arrow has no `head`, no legs and no arms — so the pose is its rest pose
    /// whatever is passed.
    #[must_use]
    pub fn new_projectile(
        model: &'static str,
        mesh: &EntityMesh,
        pos: Vec3,
        yaw_deg: f32,
        pitch_deg: f32,
        scale: f32,
        anim: &AnimInput,
    ) -> Self {
        Self::placed(
            model,
            mesh,
            projectile_model_matrix(pos, yaw_deg, pitch_deg, scale),
            anim,
            // No projectile is a creeper, and `0.0` is `pose_swelling`'s exact
            // identity case — see [`new_swelling`](Self::new_swelling).
            0.0,
        )
    }

    /// Build an instance for a **non-living vehicle** — a model
    /// [`non_living_vehicle_placement`] recognises — placed by
    /// [`non_living_vehicle_matrix`] instead of [`dying_entity_model_matrix`].
    ///
    /// Separate constructor for the same reason [`new_projectile`](Self::new_projectile)
    /// is: the placements share the yaw-rotate and the flip, but not the
    /// vertical term (a small bob in world-Y rather than the 1.501
    /// [`MODEL_FEET_OFFSET`] lift) or the trailing spin some of these rigs need.
    /// No vehicle is dying or swelling, so both extras `new_animated` carries are
    /// their documented identities here.
    #[must_use]
    pub fn new_non_living(
        model: &'static str,
        mesh: &EntityMesh,
        feet: Vec3,
        yaw_deg: f32,
        scale: f32,
        anim: &AnimInput,
        vertical_offset: f32,
        extra_yaw_deg: f32,
    ) -> Self {
        Self::placed(
            model,
            mesh,
            non_living_vehicle_matrix(feet, yaw_deg, scale, vertical_offset, extra_yaw_deg),
            anim,
            0.0,
        )
    }

    /// The half of instance construction that is placement-independent: pose the
    /// skeleton, hang the hands off it, and derive the world AABB — all from an
    /// already-built model→world `transform`.
    ///
    /// Shared by [`new`](Self::new) and [`new_projectile`](Self::new_projectile)
    /// rather than copied, so an arrow's culling box, part matrices and light
    /// default can never drift from a mob's. The *only* thing the two callers
    /// disagree about is the matrix.
    ///
    /// `swell` is a creeper's swell fraction and `0.0` for everything else, which
    /// [`Skeleton::pose_swelling`](crate::entity_anim::Skeleton::pose_swelling)
    /// documents as its exact identity case. The AABB is deliberately **not**
    /// recomputed from the swollen pose: `mesh.local_min`/`local_max` already
    /// contain a fully swollen creeper (see
    /// [`EntityMesh::from_named_model`](EntityMesh::from_named_model)), so a
    /// creeper's culling box is constant across its fuse rather than growing frame
    /// by frame — one box that always contains the drawn model, instead of a box
    /// that is exactly right and has to be rebuilt every frame.
    /// `pub(crate)` rather than private: [`EntityModelSet::resolve_at`] (a
    /// different module in this same crate) needs to build an instance from a
    /// caller-supplied transform rather than the ordinary feet/yaw placement
    /// every other constructor here derives one from.
    pub(crate) fn placed(
        model: &'static str,
        mesh: &EntityMesh,
        transform: Mat4,
        anim: &AnimInput,
        swell: f32,
    ) -> Self {
        let (aabb_min, aabb_max) = transformed_aabb(&transform, mesh.local_min, mesh.local_max);
        let part_transforms = mesh
            .skeleton
            .pose_swelling(anim, swell)
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

    /// The `entity → world` hand-translate matrix for `arm`, honoring this
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
pub(crate) fn transformed_aabb(m: &Mat4, local_min: Vec3, local_max: Vec3) -> (Vec3, Vec3) {
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
    pub(crate) models: Vec<(&'static str, EntityMesh)>,
    /// `name -> index into models`, built once in [`Self::load`] from the same
    /// vector so it cannot drift from it. Turns [`Self::get`] — called for
    /// every drawn entity in every one of the base/armour/flame/wool passes,
    /// every frame — from an O(90) linear scan into an O(1) lookup.
    index: std::collections::HashMap<&'static str, usize>,
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
        let models: Vec<(&'static str, EntityMesh)> = entity_models()
            .into_iter()
            .map(|entry| {
                (
                    entry.name,
                    EntityMesh::from_named_model(entry.name, &(entry.build)()),
                )
            })
            .collect();
        let index = models
            .iter()
            .enumerate()
            .map(|(i, (name, _))| (*name, i))
            .collect();
        Self { models, index }
    }

    /// The baked mesh for a model name, if present.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&EntityMesh> {
        self.index.get(name).map(|&i| &self.models[i].1)
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
    ///
    /// **A projectile resolved through here is drawn level.** This is
    /// [`resolve_posed`](Self::resolve_posed) with `pitch_deg = 0`, which is the
    /// right answer for every mob (a mob's pitch is head tracking, and that
    /// arrives through `anim`, not through the placement) and a flat one for an
    /// arrow. Callers that have a pitch — the live frame path — should use
    /// `resolve_posed`; the mesh-only tests and the offline demo that do not have
    /// nothing to pass and keep working unchanged.
    #[must_use]
    pub fn resolve(
        &self,
        type_path: &str,
        feet: Vec3,
        yaw_deg: f32,
        scale: f32,
        anim: &AnimInput,
    ) -> Option<EntityInstance> {
        self.resolve_posed(type_path, feet, yaw_deg, 0.0, scale, anim)
    }

    /// [`resolve`](Self::resolve) with the entity's **pitch**, which is what a
    /// projectile needs and a mob ignores.
    ///
    /// The pitch selects the placement, not just a rotation: a model
    /// [`projectile_pitch_offset_deg`] recognises is placed by
    /// [`projectile_model_matrix`] (no Y flip, no [`MODEL_FEET_OFFSET`] lift,
    /// `Ry(yaw − 90°) · Rz(pitch + offset)`), and everything else by
    /// [`entity_model_matrix`]. Sending an arrow down the mob path draws it 1.501
    /// blocks **high** and mirrored — see [`projectile_model_matrix`] for the
    /// direction of that offset, which is not the one it looks like.
    ///
    /// `yaw_deg`/`pitch_deg` are the entity's own reported rotation. For a
    /// projectile those are vanilla's velocity-derived yaw/pitch
    /// (recomputed from its own `atan2` on the projectile's own velocity every tick,
    /// and the server broadcasts the result), *not* a body yaw and a head
    /// pitch — the two use different conventions and
    /// [`projectile_model_matrix`] documents the one it expects.
    #[must_use]
    pub fn resolve_posed(
        &self,
        type_path: &str,
        feet: Vec3,
        yaw_deg: f32,
        pitch_deg: f32,
        scale: f32,
        anim: &AnimInput,
    ) -> Option<EntityInstance> {
        self.resolve_animated(type_path, feet, yaw_deg, pitch_deg, scale, anim, 0.0, 0.0)
    }

    /// [`resolve_posed`](Self::resolve_posed) with a creeper's **swell** fraction
    /// and a dying entity's **`death_time`** — see
    /// [`EntityInstance::new_animated`] for what each one reaches.
    ///
    /// This is the seam the live frame path wants, and the reason it exists is worth
    /// keeping, because both animations had the *same* defect. Every piece of the
    /// creeper swell — [`creeper_swell_scale`], the `swell` parameter on
    /// [`Skeleton::pose_swelling`](crate::entity_anim::Skeleton::pose_swelling),
    /// the `MAX_SWELL_SCALE` bounds pad, the white-flash blink
    /// ([`creeper_white_overlay_progress`]), its alpha byte
    /// ([`crate::entity_pipeline::creeper_overlay_alpha_from_progress`]), the
    /// instance lane that carries it and the shader that reads it — was built,
    /// individually tested and **reached zero pixels**, because the one hop from a
    /// decoded fuse to this call did not exist: the shell resolved every entity
    /// through `resolve_posed`, whose extras are a hard `0.0`. Both extras being an
    /// exact identity at `0.0` is what made that invisible, and is why a *live*
    /// call site passing them is the thing to check rather than either formula.
    ///
    /// [`creeper_swell_scale`]: crate::entity_anim::creeper_swell_scale
    /// [`creeper_white_overlay_progress`]: crate::entity_anim::creeper_white_overlay_progress
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "one argument per independent piece of a live entity's placement, pose \
                  and animation state; bundling them into a struct would move the same \
                  fields behind a name that adds nothing"
    )]
    pub fn resolve_animated(
        &self,
        type_path: &str,
        feet: Vec3,
        yaw_deg: f32,
        pitch_deg: f32,
        scale: f32,
        anim: &AnimInput,
        swell: f32,
        death_time: f32,
    ) -> Option<EntityInstance> {
        let name = canonical_model_name(type_path)?;
        let mesh = self.get(name)?;
        Some(if let Some(offset) = projectile_pitch_offset_deg(name) {
            EntityInstance::new_projectile(
                name,
                mesh,
                feet,
                yaw_deg,
                pitch_deg + offset,
                scale,
                anim,
            )
        } else if let Some((vertical_offset, extra_yaw_deg)) =
            non_living_vehicle_placement(name)
        {
            EntityInstance::new_non_living(
                name,
                mesh,
                feet,
                yaw_deg,
                scale,
                anim,
                vertical_offset,
                extra_yaw_deg,
            )
        } else {
            EntityInstance::new_animated(name, mesh, feet, yaw_deg, scale, anim, swell, death_time)
        })
    }

    /// Resolve a tracked entity's model at a caller-supplied `transform`,
    /// bypassing [`entity_model_matrix`]/[`dying_entity_model_matrix`]
    /// entirely — the seam a **nested** placement needs.
    ///
    /// Every other `resolve*` here derives its placement from `(feet, yaw,
    /// scale)` under vanilla's ordinary entity convention. That is the wrong
    /// shape for a mob drawn *inside* another transform chain — the mob
    /// spawner's miniature display entity, whose renderer builds vanilla's
    /// own pose stack (translate, spin, tilt, shrink) and then hands the
    /// entity's *own* renderer that already-transformed stack, rather than a
    /// `(feet, yaw)` pair. [`crate::spawner::spawner_display_outer_matrix`]
    /// builds that outer chain; the caller composes it with
    /// `entity_model_matrix(Vec3::ZERO, entity_yaw_deg, 1.0)` for the
    /// entity's own flip/lift, exactly the nesting vanilla's two render calls
    /// produce, and passes the product here.
    ///
    /// `None` for a `type_path` with no baked model, the same miss every
    /// other `resolve*` here has.
    #[must_use]
    pub fn resolve_at(
        &self,
        type_path: &str,
        transform: Mat4,
        anim: &AnimInput,
    ) -> Option<EntityInstance> {
        let name = canonical_model_name(type_path)?;
        let mesh = self.get(name)?;
        Some(EntityInstance::placed(name, mesh, transform, anim, 0.0))
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
// (vanilla's own zombie renderer builds an armour-model-set generic over the
// zombie model type), and its own submit step calls the wearer's own
// animation-setup on it with the wearer's render state. A zombie's chestplate
// therefore reaches out in front with the wearer's own zombie-arm animation,
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
// whose pivots differ from the plain humanoid model's gets its armour at
// *its* pivot, not at vanilla's:
//
// * `skeleton`/`stray`/`wither_skeleton` put their legs at `x = ±2.0` where
//   the humanoid model has `±1.9`, so skeleton leg armour sits 0.1 texel
//   (0.00625 blocks) further out than vanilla draws it.
// * `player_slim`'s arms pivot 0.5 texel lower than the wide rig's, and
//   vanilla bakes only *one* player armour set (its armour-mesh-set builder
//   takes no slim flag and adds only empty sleeve/pants/jacket nodes), so a
//   slim player's sleeves get armour 0.5 texel (0.03 blocks) low.
//
// Both are deliberate: following the visible limb is worth more than matching
// vanilla's pivot to a thirtieth of a block, and the alternative — posing a
// second skeleton — would reintroduce exactly the zombie-arm divergence vanilla
// avoids by construction.
