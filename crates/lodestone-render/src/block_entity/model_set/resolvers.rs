use glam::{Mat4, Quat, Vec3};
use lodestone_assets::block_entity_models::{BLOCK_ENTITY_MODELS, BlockEntityModelEntry};
use lodestone_assets::entity::PartPose;

use crate::banner_pattern::banner_pattern_layers;

use super::item_rigs::*;
use super::spawns::*;
use super::super::batching::{
    BannerInstances, BannerLayerDraw, BlockEntityInstance, BlockEntityTexture, transformed_aabb,
};
use super::super::model_families::*;

/// The baked block-entity corpus: one [`BlockEntityMesh`] per entry in
/// [`BLOCK_ENTITY_MODELS`], baked on the CPU with no GPU involvement.
#[derive(Debug, Clone)]
pub struct BlockEntityModelSet {
    models: Vec<(&'static str, BlockEntityMesh)>,
}

impl BlockEntityModelSet {
    /// Bakes every ported block-entity model.
    #[must_use]
    pub fn load() -> Self {
        BlockEntityModelSet {
            models: BLOCK_ENTITY_MODELS
                .iter()
                .map(|entry: &BlockEntityModelEntry| {
                    (entry.name, BlockEntityMesh::from_model(&(entry.build)()))
                })
                .collect(),
        }
    }

    /// Iterates `(model name, mesh)`.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &BlockEntityMesh)> {
        self.models.iter().map(|(n, m)| (*n, m))
    }

    /// The mesh for a model name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&BlockEntityMesh> {
        self.models
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, m)| m)
    }

    /// Number of baked models.
    #[must_use]
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Whether the corpus is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Resolves a **`minecraft:special` item** into a drawable instance at an
    /// arbitrary placement — the world surfaces of the seam
    /// [`special_item_rig`] owns.
    ///
    /// `kind` is the special-renderer id from the item's own definition
    /// (`SpecialItemForm::kind`), `item_path` the item's registry path with the
    /// namespace stripped, and `placement` the surface's already-composed
    /// block→world matrix: a dropped stack's bob-and-spin chain, another entity's
    /// hand chain, or an item frame's wall pose. `transformation` is the item
    /// definition's whole root-to-`special` `"transformation"` chain, outermost
    /// first (`SpecialItemForm::transformation`) — folded *underneath*
    /// `placement`, same as every other consumer; see
    /// [`crate::compose_special_item_transform`]'s doc for the derivation.
    /// Returns `None` when the `kind` has no ported rig (six of the ten do not)
    /// or the rig is not in the corpus.
    ///
    /// # Why this exists rather than each surface building the instance itself
    ///
    /// Three callers, each with its own placement and nothing else different.
    /// Everything after the placement — the rig lookup, the sheet, the rest-pose
    /// part transforms, the cull AABB — is identical, and a second copy of it is
    /// how one surface ends up drawing a chest the batcher groups under the wrong
    /// sheet. It is the same shape [`Self::resolve_chest`] and its six siblings
    /// have, differing only in taking the placement instead of deriving one from a
    /// block position.
    ///
    /// # No pose overrides, deliberately
    ///
    /// `part_transforms(placement, &[])`, so a chest's lid is **shut** and a
    /// shulker box's is **closed**: vanilla's own item special renderers for
    /// these carry a fixed `openness` and no animation at all. An item is not the block,
    /// and passing a lid angle here would open every chest lying on every floor.
    #[must_use]
    pub fn resolve_special_item(
        &self,
        kind: &str,
        item_path: &str,
        placement: Mat4,
        transformation: &[lodestone_assets::ItemNodeTransform],
        light: u8,
    ) -> Option<BlockEntityInstance> {
        let (model, texture) = special_item_rig(kind, item_path)?;
        let mesh = self.get(model)?;
        let placement = crate::compose_special_item_transform(placement, kind, transformation);
        let part_transforms = mesh.part_transforms(placement, &[]);
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model,
            texture: BlockEntityTexture::Static(texture),
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one chest into a drawable instance, or `None` if its model is
    /// not in the corpus.
    #[must_use]
    pub fn resolve_chest(&self, spawn: &ChestSpawn) -> Option<BlockEntityInstance> {
        let model = spawn.half.model();
        let mesh = self.get(model)?;
        let placement = block_entity_placement_matrix(spawn.pos, spawn.facing_yaw_deg);

        // The lid and the lock rotate together about the *same* pivot
        // (`lock.rot_x = lid.rot_x`), which is why the asset corpus makes them
        // siblings sharing `offset(0, 9, 1)` rather than nesting the lock.
        let x_rot = chest_lid_x_rot(chest_lid_openness(spawn.openness));
        let mut overrides = Vec::with_capacity(2);
        for name in ["lid", "lock"] {
            if let Some(index) = mesh.index_of(name) {
                let mut pose = mesh.part_rest[index];
                pose.x_rot = x_rot;
                overrides.push((index, pose));
            }
        }
        let part_transforms = mesh.part_transforms(placement, &overrides);

        // The cull AABB is taken from the *rest* bounds through the placement
        // matrix, deliberately ignoring the lid angle: an open lid only ever
        // grows the box backwards by a few texels, and recomputing per frame
        // would make a chest pop in and out of view as somebody opens it.
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model,
            texture: BlockEntityTexture::Static(chest_texture_stem(spawn.material, spawn.half)),
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one skull/head into a drawable instance, or `None` if its
    /// model is not in the corpus.
    ///
    /// # The head part is never posed; two child parts are
    ///
    /// No type poses its **head**: vanilla's own render-state yaw/pitch fields
    /// are only ever set for the *item-frame*/GUI skull paths, never by
    /// vanilla's own render-state extraction for a placed block, so every
    /// animation update's `head.rot_y = state.rot_y * PI/180` line is a
    /// multiply by zero here.
    ///
    /// The dragon's jaw and the piglin's two ears are the exception, and they
    /// are an exception in the direction that bites: their animation update
    /// **assigns** rather than adds, and the assigned value at rest is not the
    /// authored rest pose (`0.2` against `0` for the jaw, `±0.7` against
    /// `±PI/6` for the ears). Drawing either from the mesh's own rest pose
    /// therefore looks plausible and is wrong — a dragon head with its mouth
    /// clamped shut, ears a few degrees off. See
    /// [`SKULL_RESTING_ANIMATION_POS`] for why the position is a constant here
    /// rather than a `SkullSpawn` field.
    #[must_use]
    pub fn resolve_skull(&self, spawn: &SkullSpawn) -> Option<BlockEntityInstance> {
        let model = spawn.skull_type.model();
        let mesh = self.get(model)?;
        let placement = match spawn.orientation {
            SkullOrientation::Floor { rotation_segment } => {
                skull_ground_placement_matrix(spawn.pos, rotation_segment)
            }
            SkullOrientation::Wall { facing_yaw_deg } => {
                skull_wall_placement_matrix(spawn.pos, facing_yaw_deg)
            }
        };
        let mut overrides = Vec::new();
        let mut pose_part = |name: &str, apply: &dyn Fn(&mut PartPose)| {
            if let Some(index) = mesh.index_of(name) {
                let mut pose = mesh.part_rest[index];
                apply(&mut pose);
                overrides.push((index, pose));
            }
        };
        match spawn.skull_type {
            SkullType::Dragon => {
                let x_rot = dragon_head_jaw_x_rot(SKULL_RESTING_ANIMATION_POS);
                pose_part(DRAGON_HEAD_JAW_PART, &|pose| pose.x_rot = x_rot);
            }
            SkullType::Piglin => {
                let (left, right) = piglin_head_ear_z_rots(SKULL_RESTING_ANIMATION_POS);
                pose_part(PIGLIN_HEAD_EAR_PARTS[0], &|pose| pose.z_rot = left);
                pose_part(PIGLIN_HEAD_EAR_PARTS[1], &|pose| pose.z_rot = right);
            }
            SkullType::Skeleton
            | SkullType::WitherSkeleton
            | SkullType::Zombie
            | SkullType::Creeper
            | SkullType::Player => {}
        }
        let part_transforms = mesh.part_transforms(placement, &overrides);
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model,
            texture: spawn.texture.clone(),
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one copper golem statue into a drawable instance, or `None`
    /// if that pose's model is not in the corpus.
    #[must_use]
    pub fn resolve_copper_golem_statue(
        &self,
        spawn: &CopperGolemStatueSpawn,
    ) -> Option<BlockEntityInstance> {
        let model = spawn.pose.model_name();
        let mesh = self.get(model)?;
        let placement = copper_golem_statue_placement_matrix(spawn.pos, spawn.facing_yaw_deg);
        let part_transforms = mesh.part_transforms(placement, &[]);
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model,
            texture: BlockEntityTexture::Static(copper_golem_statue_texture_stem(spawn.oxidation)),
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one bell into a drawable instance, or `None` if the model is
    /// not in the corpus.
    ///
    /// `bell_body` is the only part overridden — `bell_base` is its *child*
    /// in the baked mesh (see `lodestone_assets::block_entity_models::bell_model`'s
    /// doc), so rotating the body carries the rim with it through
    /// [`BlockEntityMesh::part_transforms`]'s own chain, exactly as vanilla's
    /// own model-part parent/child composition does in the jar. There is no
    /// second override for `bell_base`, unlike chest's `lid`/`lock` pair,
    /// because vanilla itself poses only the bell body's `rot_x`/`rot_z`.
    #[must_use]
    pub fn resolve_bell(&self, spawn: &BellSpawn) -> Option<BlockEntityInstance> {
        let mesh = self.get(BELL)?;
        let placement = block_entity_placement_matrix(spawn.pos, 0.0);

        let (x_rot, z_rot) = match spawn.shake {
            Some((direction, ticks)) => bell_shake_angle(Some(direction), ticks),
            None => (0.0, 0.0),
        };
        let mut overrides = Vec::with_capacity(1);
        if let Some(index) = mesh.index_of("bell_body") {
            let mut pose = mesh.part_rest[index];
            pose.x_rot = x_rot;
            pose.z_rot = z_rot;
            overrides.push((index, pose));
        }
        let part_transforms = mesh.part_transforms(placement, &overrides);

        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model: BELL,
            texture: BlockEntityTexture::Static(BELL_TEXTURE_STEM),
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one shulker box into a drawable instance, or `None` if the model
    /// is not in the corpus.
    ///
    /// Only `lid` is ever overridden, and only when the box is actually open:
    /// `progress == 0.0` leaves the rest pose alone, so the common case produces
    /// an instance whose `part_transforms` depend on nothing but `pos` and
    /// `facing`.
    #[must_use]
    pub fn resolve_shulker(&self, spawn: &ShulkerSpawn) -> Option<BlockEntityInstance> {
        let mesh = self.get(SHULKER_BOX)?;
        let placement = shulker_placement_matrix(spawn.pos, spawn.facing);

        let mut overrides = Vec::new();
        if spawn.progress > 0.0
            && let Some(index) = mesh.index_of("lid")
        {
            let (y, y_rot) = shulker_lid_pose(spawn.progress);
            let mut pose = mesh.part_rest[index];
            pose.y = y;
            pose.y_rot = y_rot;
            overrides.push((index, pose));
        }
        let part_transforms = mesh.part_transforms(placement, &overrides);
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model: SHULKER_BOX,
            texture: BlockEntityTexture::Static(shulker_texture_stem(spawn.colour)),
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one decorated pot into a base instance plus up to four
    /// independently textured side instances — the base plus per-side
    /// decomposition [`docs/block-entity-renderers.md`] names as the way
    /// around the `(model, texture)` single-texture-per-instance batch key.
    /// `None` only if the base model itself is missing from the corpus (the
    /// four side models are checked the same way, per side, and a missing
    /// one simply omits that side rather than failing the whole pot).
    ///
    /// # Five instances, five ordinary batch slots — no new mechanism
    ///
    /// Unlike [`Self::resolve_banner`], nothing here needs a second,
    /// unbatched draw pass. A banner layers N *tinted masks* over **one**
    /// mesh, which the plain `(model, texture)` key cannot express and which
    /// is why that pass exists at all. A decorated pot instead needs four
    /// **distinct diffuse textures on four distinct quads** — a shape the
    /// ordinary batcher was already built for, the same way two chests with
    /// different materials batch separately today. Each returned instance
    /// carries its own `model/texture` pair and rejoins
    /// [`plan_block_entities`]'s normal instanced draw exactly like a chest
    /// or a skull; two pots sharing a sherd on the same side coalesce into
    /// one GPU instance of one batch, the same as two oak chests would.
    ///
    /// # Every side always draws
    ///
    /// Vanilla's own decorated-pot renderer submits each side part for
    /// `front`/`back`/`left`/`right` unconditionally, falling back to its own
    /// default side sheet per side rather than skipping an
    /// undecorated one — so this returns all four side instances always, not
    /// only the decorated ones. Skipping a blank side would draw a pot with
    /// invisible faces on three sides of the corpus's test cases and silently
    /// autocorrect on real ones the moment a player added their first sherd.
    #[must_use]
    pub fn resolve_decorated_pot(&self, spawn: &DecoratedPotSpawn) -> Option<[BlockEntityInstance; 5]> {
        let placement = decorated_pot_placement_matrix(spawn.pos, spawn.facing_yaw_deg);

        let instance = |model: &'static str, texture: &'static str| -> Option<BlockEntityInstance> {
            let mesh = self.get(model)?;
            let part_transforms = mesh.part_transforms(placement, &[]);
            let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
            Some(BlockEntityInstance {
                model,
                texture: BlockEntityTexture::Static(texture),
                transform: placement,
                part_transforms,
                aabb_min,
                aabb_max,
                light: spawn.light,
                tint: [255, 255, 255],
            })
        };

        let side_texture = |sherd: &Option<String>| -> &'static str {
            sherd
                .as_deref()
                .and_then(decorated_pot_pattern_texture_stem)
                .unwrap_or(DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM)
        };

        let base = instance(DECORATED_POT_BASE, DECORATED_POT_BASE_TEXTURE_STEM)?;
        let front = instance(DECORATED_POT_SIDE_FRONT, side_texture(&spawn.front))?;
        let back = instance(DECORATED_POT_SIDE_BACK, side_texture(&spawn.back))?;
        let left = instance(DECORATED_POT_SIDE_LEFT, side_texture(&spawn.left))?;
        let right = instance(DECORATED_POT_SIDE_RIGHT, side_texture(&spawn.right))?;
        Some([base, front, back, left, right])
    }

    /// Resolves one conduit into its drawable instances — vanilla's own submit
    /// step, branch by branch. Returns one instance (the slowly-spinning inactive
    /// shell) or four (the tumbling cage, both wind planes, and the
    /// camera-facing eye) — never a mix, because vanilla's own submit step is a
    /// single active/inactive branch, not two independently
    /// gated draws. A missing mesh drops that one instance rather than the
    /// whole conduit, the same fail-open [`Self::resolve_decorated_pot`]'s four
    /// sides use.
    ///
    /// `camera_orientation` is [`crate::entity::camera_orientation`] applied to
    /// the frame's view matrix — only the eye instance reads it
    /// (folded into the pose stack, active branch only), but it is
    /// taken unconditionally so building it is the caller's problem once per
    /// frame, exactly as [`crate::entity::experience_orb_matrix`] does.
    ///
    /// See [`conduit_frame_scan`]/[`conduit_advance`] for how [`ConduitSpawn`]'s
    /// fields are meant to be produced — this method only consumes an
    /// already-resolved spawn, the same split [`Self::resolve_bell`] makes
    /// between [`crate::block_entity::BellSpawn`] and its `BellShakes` tracker.
    #[must_use]
    pub fn resolve_conduit(
        &self,
        spawn: &ConduitSpawn,
        camera_orientation: Mat4,
    ) -> Vec<BlockEntityInstance> {
        let origin = Vec3::new(
            spawn.pos[0] as f32,
            spawn.pos[1] as f32,
            spawn.pos[2] as f32,
        );
        let make = |model: &'static str, texture: &'static str, placement: Mat4| {
            let mesh = self.get(model)?;
            let part_transforms = mesh.part_transforms(placement, &[]);
            let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
            Some(BlockEntityInstance {
                model,
                texture: BlockEntityTexture::Static(texture),
                transform: placement,
                part_transforms,
                aabb_min,
                aabb_max,
                light: spawn.light,
                tint: [255, 255, 255],
            })
        };

        if !spawn.active {
            // Inactive: `translate(0.5,0.5,0.5)` then
            // `rotationY(state * (PI/180))` — the "degrees"
            // reading, see [`conduit_inactive_y_rot_radians`]'s doc — then the
            // 6×6×6 shell model against its own inactive-shell texture (`entity/conduit/base`).
            let placement = Mat4::from_translation(origin + Vec3::new(0.5, 0.5, 0.5))
                * Mat4::from_rotation_y(conduit_inactive_y_rot_radians(
                    spawn.active_rotation_value,
                ));
            return make(CONDUIT_SHELL, CONDUIT_SHELL_TEXTURE_STEM, placement)
                .into_iter()
                .collect();
        }

        let hh = conduit_bob(spawn.anim_time);
        // `translate(0.5F, 0.3F + hh * 0.2F, 0.5F)` — the cage's and the eye's
        // own bob; the two wind planes stay fixed at `translate(0.5,0.5,0.5)`
        // with **no** `hh` term. All four calls look alike at a glance; only two
        // of them carry this.
        let bob = 0.3 + hh * 0.2;
        let mut out = Vec::with_capacity(4);

        // Cage: tumbles about the fixed diagonal `Vector3f(0.5,1,0.5).normalize()`,
        // using the "radians" reading of the active-rotation state — see
        // [`conduit_active_axis_rotation_radians`]'s doc for why that is not a
        // second unit conversion.
        let axis = Vec3::new(0.5, 1.0, 0.5).normalize();
        let cage_placement = Mat4::from_translation(origin + Vec3::new(0.5, bob, 0.5))
            * Mat4::from_axis_angle(
                axis,
                conduit_active_axis_rotation_radians(spawn.active_rotation_value),
            );
        out.extend(make(CONDUIT_CAGE, CONDUIT_CAGE_TEXTURE_STEM, cage_placement));

        // The two wind planes share **one** texture choice — vanilla computes
        // its own wind sprite id/render type/sprite once and passes the same
        // three to both submit calls.
        let wind_texture = if spawn.animation_phase == 1 {
            CONDUIT_WIND_VERTICAL_TEXTURE_STEM
        } else {
            CONDUIT_WIND_TEXTURE_STEM
        };
        // First wind plane: `translate(0.5,0.5,0.5)` then, only by the
        // animation phase, a quarter turn about X (phase 1) or Z (phase 2) —
        // phase 0 submits with no extra rotation at all.
        let wind1_rot = match spawn.animation_phase {
            1 => Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2),
            2 => Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2),
            _ => Mat4::IDENTITY,
        };
        let wind1_placement =
            Mat4::from_translation(origin + Vec3::new(0.5, 0.5, 0.5)) * wind1_rot;
        out.extend(make(CONDUIT_WIND, wind_texture, wind1_placement));

        // Second wind plane: `translate(0.5,0.5,0.5)`, `scale(0.875)`, then
        // `rotationXYZ(PI, 0, PI)` — JOML's `rotationXYZ(x,y,z)` composes as
        // `rotateX(x).rotateY(y).rotateZ(z)` (each a post-multiply), so with
        // `y == 0` this is `qX(PI) * qZ(PI)`: rotate 180° about Z first, then
        // 180° about X. Written as two `glam::Quat`s multiplied in that same
        // left-to-right order, not collapsed to a single axis by hand.
        let wind2_rot =
            Quat::from_rotation_x(std::f32::consts::PI) * Quat::from_rotation_z(std::f32::consts::PI);
        let wind2_placement = Mat4::from_translation(origin + Vec3::new(0.5, 0.5, 0.5))
            * Mat4::from_scale(Vec3::splat(0.875))
            * Mat4::from_quat(wind2_rot);
        out.extend(make(CONDUIT_WIND, wind_texture, wind2_placement));

        // Eye: `translate(0.5, 0.3+hh*0.2, 0.5)`, `scale(0.5)`,
        // `mulPose(camera.orientation)`, `mulPose(rotationZ(PI).rotateY(PI))`,
        // `scale(1.3333334)` — net linear scale `0.5 * 1.3333334 ≈ 0.6667`, not
        // a single `scale(1.3333334)`; the two calls are not redundant, they
        // straddle the billboard rotation. `rotationZ(PI).rotateY(PI)` is
        // `qZ(PI) * qY(PI)` by the same JOML post-multiply reading as the
        // second wind plane above.
        let eye_rot =
            Quat::from_rotation_z(std::f32::consts::PI) * Quat::from_rotation_y(std::f32::consts::PI);
        let eye_placement = Mat4::from_translation(origin + Vec3::new(0.5, bob, 0.5))
            * Mat4::from_scale(Vec3::splat(0.5))
            * camera_orientation
            * Mat4::from_quat(eye_rot)
            * Mat4::from_scale(Vec3::splat(1.333_333_4));
        let eye_texture = if spawn.hunting {
            CONDUIT_OPEN_EYE_TEXTURE_STEM
        } else {
            CONDUIT_CLOSED_EYE_TEXTURE_STEM
        };
        out.extend(make(CONDUIT_EYE, eye_texture, eye_placement));

        out
    }

    /// Resolves one ground/standing banner into its opaque body+flag
    /// instances plus its ordered, translucent pattern-layer draw list, or
    /// `None` if either model is not in the corpus.
    ///
    /// # Two meshes, three draws — see the module's banner section
    ///
    /// Vanilla's own banner submit step draws the pole+bar opaque, the flag
    /// opaque (both with its own banner-base sheet), then its own
    /// pattern-submit step: the base mask tinted by `base_color` plus every
    /// stored pattern layer, all through the banner-pattern render type
    /// (`EntityPipeline::banner_layer_pipeline`). The first two ride the
    /// ordinary [`plan_block_entities`] batcher via
    /// [`BannerInstances::body`]/[`BannerInstances::flag`]; the third is
    /// [`BannerInstances::layers`], a flat ordered list a caller draws
    /// directly, one draw per entry, in order — never re-batched by texture,
    /// since these draws are translucent and depth-write-off and so must
    /// submit in the item's own stored order (two banners reusing the same
    /// two sprites in opposite orders could not both be right).
    ///
    /// # The flag's own transform, reused by every layer
    ///
    /// Every pattern mask paints over the *posed* flag (the same sway
    /// [`banner_flag_x_rot`] applies to the opaque flag draw), never the
    /// pole/bar — vanilla's own pattern-submit step is called with the same
    /// flag model its own banner-submit step already posed.
    /// [`BannerLayerDraw::transform`] is therefore the flag part's own world
    /// matrix, computed once and shared by all
    /// `1 + patterns.len().min(MAX_PATTERN_LAYERS)` layers.
    #[must_use]
    pub fn resolve_banner(&self, spawn: &BannerSpawn) -> Option<BannerInstances> {
        // Both the mesh pair and the placement angle come from the attachment, in
        // one match, so a wall banner can never be drawn on the standing rig (a
        // 42-texel pole hanging in mid-air) or at the wrong angle.
        let (body_model, flag_model, placement) = match spawn.attachment {
            BannerAttachment::Ground { rotation_segment } => (
                BANNER_BODY,
                BANNER_FLAG,
                banner_ground_placement_matrix(spawn.pos, rotation_segment),
            ),
            BannerAttachment::Wall { facing_yaw_deg } => (
                BANNER_WALL_BODY,
                BANNER_WALL_FLAG,
                banner_wall_placement_matrix(spawn.pos, facing_yaw_deg),
            ),
        };
        let body_mesh = self.get(body_model)?;
        let flag_mesh = self.get(flag_model)?;

        let body_transforms = body_mesh.part_transforms(placement, &[]);
        let (body_min, body_max) =
            transformed_aabb(&placement, body_mesh.local_min, body_mesh.local_max);
        let body = BlockEntityInstance {
            model: body_model,
            texture: BlockEntityTexture::Static(BANNER_BASE_TEXTURE_STEM),
            transform: placement,
            part_transforms: body_transforms,
            aabb_min: body_min,
            aabb_max: body_max,
            light: spawn.light,
            tint: [255, 255, 255],
        };

        // The one override: the flag's own sway, the same mechanism the
        // chest lid and the bell body already use.
        let x_rot = banner_flag_x_rot(spawn.phase);
        let flag_index = flag_mesh.index_of("flag")?;
        let mut pose = flag_mesh.part_rest[flag_index];
        pose.x_rot = x_rot;
        let flag_transforms = flag_mesh.part_transforms(placement, &[(flag_index, pose)]);
        let (flag_min, flag_max) =
            transformed_aabb(&placement, flag_mesh.local_min, flag_mesh.local_max);
        let flag_world = flag_transforms[flag_index];
        let flag = BlockEntityInstance {
            model: flag_model,
            texture: BlockEntityTexture::Static(BANNER_BASE_TEXTURE_STEM),
            transform: placement,
            part_transforms: flag_transforms,
            aabb_min: flag_min,
            aabb_max: flag_max,
            light: spawn.light,
            tint: [255, 255, 255],
        };

        let layers = banner_pattern_layers(spawn.base_color, &spawn.patterns)
            .into_iter()
            .map(|layer| BannerLayerDraw {
                transform: flag_world,
                sprite: layer.sprite,
                color: layer.color,
                light: spawn.light,
            })
            .collect();

        Some(BannerInstances { body, flag, layers })
    }

    /// Resolves one lectern's open book into a drawable instance, or `None` if
    /// the model is not in the corpus.
    ///
    /// Six overrides, one per posed part, from [`book_part_poses`] — the widest
    /// override list in this module, and the reason
    /// [`BlockEntityMesh::part_transforms`]' `(index, pose)` mechanism was
    /// written to take a slice rather than one part.
    ///
    /// Every one of the six is a **flat child of the root**, so there is no
    /// parent/child composition to get right the way [`Self::resolve_bell`] has.
    /// What there *is* instead is `x`: four of the six move their pivot as well
    /// as rotating, which no other type here does.
    ///
    /// Nothing about the result varies per frame — [`LECTERN_BOOK_OPENNESS`] is
    /// constant — so a caller may cache it against `(pos, facing)` if it ever
    /// matters.
    #[must_use]
    pub fn resolve_lectern(&self, spawn: &LecternSpawn) -> Option<BlockEntityInstance> {
        let mesh = self.get(BOOK)?;
        let placement = lectern_book_placement_matrix(spawn.pos, spawn.facing_yaw_deg);

        let mut overrides = Vec::with_capacity(6);
        for (name, y_rot, x) in book_part_poses(LECTERN_BOOK_OPENNESS, LECTERN_BOOK_PAGE_FLIP) {
            let Some(index) = mesh.index_of(name) else {
                continue;
            };
            let mut pose = mesh.part_rest[index];
            pose.y_rot = y_rot;
            if let Some(x) = x {
                pose.x = x;
            }
            overrides.push((index, pose));
        }
        let part_transforms = mesh.part_transforms(placement, &overrides);

        // The rest AABB through the placement matrix, like every other type
        // here. The posed book opens *wider* than its rest bounds (the lids
        // swing out past `openness` radians), so this is deliberately
        // generous-in-the-wrong-direction rather than exact — but a book is a
        // few texels across and the placement is a block off the floor, so the
        // block's own AABB dominates either way.
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model: BOOK,
            texture: BlockEntityTexture::Static(BOOK_TEXTURE_STEM),
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one enchanting table's floating book into a drawable instance, or
    /// `None` if the model is not in the corpus.
    ///
    /// The same mesh, the same six overrides and the same
    /// [`book_part_poses`] as [`Self::resolve_lectern`] — and **everything else
    /// differs**. The placement is
    /// [`enchanting_table_book_placement_matrix`] (an `80°` tilt, not `67.5`, and
    /// a live hover), the openness is
    /// [`enchanting_table_book_openness`] rather than the lectern's frozen
    /// constant, and the page flips come from
    /// [`enchanting_table_page_flips`] rather than a pair of literals.
    ///
    /// So this is emphatically **not** `resolve_lectern` with a different matrix,
    /// and folding the two into one function parameterised by a matrix would
    /// silently give an enchanting table the lectern's dead `1.5` openness — a
    /// book that never opens, on a rig that draws perfectly.
    #[must_use]
    pub fn resolve_enchanting_table(
        &self,
        spawn: &EnchantingTableSpawn,
    ) -> Option<BlockEntityInstance> {
        let mesh = self.get(BOOK)?;
        let placement =
            enchanting_table_book_placement_matrix(spawn.pos, spawn.y_rot, spawn.time);
        let openness = enchanting_table_book_openness(spawn.time, spawn.open);
        let page_flip = enchanting_table_page_flips(spawn.flip);

        let mut overrides = Vec::with_capacity(6);
        for (name, y_rot, x) in book_part_poses(openness, page_flip) {
            let Some(index) = mesh.index_of(name) else {
                continue;
            };
            let mut pose = mesh.part_rest[index];
            pose.y_rot = y_rot;
            if let Some(x) = x {
                pose.x = x;
            }
            overrides.push((index, pose));
        }
        let part_transforms = mesh.part_transforms(placement, &overrides);

        // The rest AABB through the placement matrix, as `resolve_lectern` — and
        // generous in the same wrong direction, but by more: this book's lids
        // swing all the way open rather than sitting at a fixed `1.5`. Still
        // dominated by the block's own AABB, and the hover is ±0.01 blocks.
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model: BOOK,
            texture: BlockEntityTexture::Static(BOOK_TEXTURE_STEM),
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }
}
