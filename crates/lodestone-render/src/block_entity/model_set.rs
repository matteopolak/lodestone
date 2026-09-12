use glam::{Mat4, Quat, Vec3};
use lodestone_assets::ResourceLocation;
use lodestone_assets::block_entity_models::{BLOCK_ENTITY_MODELS, BlockEntityModelEntry};
use lodestone_assets::entity::PartPose;
use lodestone_model::{CampfireSlot, ShelfSlot};

use crate::banner_pattern::{DyeColor, StoredPatternLayer, banner_pattern_layers};
use crate::entity::ENTITY_FULLBRIGHT;

use super::batching::{
    BannerInstances, BannerLayerDraw, BlockEntityInstance, BlockEntityTexture, transformed_aabb,
};
use super::model_families::*;

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

/// The version-free description of one enchanting table's floating book this
/// frame — every field already interpolated by the caller.
///
/// **The only spawn in this module whose every animated field is client-simulated
/// with nothing on the wire.** Vanilla's own book-animation tick runs
/// on the client, driven by the nearest player's position, and the server sends
/// none of `time`/`open`/`flip`/`rot` — so a source that captured a stale copy of
/// this state freezes the book, and there is no packet whose absence would
/// explain it.
///
/// Interpolation belongs to the caller (vanilla's own render-state extraction
/// does it, not the submit step): `open` and `flip` are `lerp(partialTicks, o*, *)`,
/// `time` is `time + partialTicks`, and `y_rot` is the **shortest-arc** lerp of
/// `o_rot`→`rot`. That last one is not an ordinary lerp — see
/// [`Self::y_rot`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnchantingTableSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// The book's facing, in **radians**, already shortest-arc interpolated:
    /// `oRot + wrap(rot - oRot) * partialTicks`, where `wrap` brings the delta
    /// into `-PI..PI`.
    ///
    /// Skipping the wrap makes the book spin the long way round — a full
    /// backwards revolution in one tick — every time the angle crosses `±PI`,
    /// which happens whenever a player walks past the north-west corner. A plain
    /// `lerp` is wrong in exactly one place and looks right everywhere else.
    pub y_rot: f32,
    /// The block entity's own `time + partialTicks` — vanilla's raw tick
    /// counter, feeding both the hover and the openness breath.
    pub time: f32,
    /// `lerp(partialTicks, oOpen, open)`, `0..1`: how far the book has opened.
    /// `0` is fully shut, which is a **closed book** and not an absent one —
    /// [`enchanting_table_book_openness`] returns `0`, and [`book_part_poses`]
    /// at openness `0` puts `left_lid` at `PI` against `right_lid` at `0`, i.e.
    /// the covers folded together over six real posed parts.
    /// Vanilla's own submit step has no early return: vanilla draws a book
    /// for every enchanting table it renders, and the nearest-player test
    /// decides only whether it opens. A caller that skips a shut book therefore
    /// deletes every table nobody is standing at, which is exactly what this
    /// field's doc used to license.
    pub open: f32,
    /// `lerp(partialTicks, oFlip, flip)` — the page-flip accumulator, **not** a
    /// `0..1` phase. It is unbounded and drifts in either direction; the two
    /// phases come out of [`enchanting_table_page_flips`].
    pub flip: f32,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl EnchantingTableSpawn {
    /// A fully-open, resting, full-bright book over the table at `pos` — the
    /// minimum a hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        EnchantingTableSpawn {
            pos,
            y_rot: 0.0,
            time: 0.0,
            open: 1.0,
            flip: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one lectern's book to draw this frame.
///
/// Two fields and no animation state at all, which makes this the cheapest type
/// in the module: the lectern block's own HAS_BOOK property decides whether
/// there is a spawn to make in the first place (a bookless lectern draws
/// nothing here — its shelf is a real block model), and `FACING` gives the
/// yaw. There is no NBT read and
/// nothing on the wire.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LecternSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// The clockwise-rotated yaw of the lectern's own FACING property, in degrees
    /// — see [`horizontal_facing_clockwise_yaw`], which is the only correct way
    /// to produce this. Passing the facing's bare yaw puts the book sideways.
    pub facing_yaw_deg: f32,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl LecternSpawn {
    /// A north-facing, full-bright lectern book at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        LecternSpawn {
            pos,
            facing_yaw_deg: horizontal_facing_clockwise_yaw("north").unwrap_or(270.0),
            light: ENTITY_FULLBRIGHT,
        }
    }
}

impl Default for BlockEntityModelSet {
    fn default() -> Self {
        Self::load()
    }
}

/// The version-free description of one chest to draw this frame.
///
/// The caller owns every field: block state → `facing_yaw_deg`/`half`, block
/// path → `material`, block event viewer count → `openness`, world light →
/// `light`. Keeping this a plain struct is what stops the render crate depending
/// on a protocol version or a client.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChestSpawn {
    /// Block position (the block's minimum corner, in world coordinates).
    pub pos: [i32; 3],
    /// The facing direction's yaw, of the chest's `facing` property.
    pub facing_yaw_deg: f32,
    /// Which layer to draw.
    pub half: ChestHalf,
    /// Which sheet to draw with.
    pub material: ChestMaterial,
    /// **Raw** openness in `0..=1` — the eased value is computed here, so a
    /// caller that already eased would double-ease.
    pub openness: f32,
    /// Packed sky/block light (`sky << 4 | block`) at this block. Pass
    /// [`ENTITY_FULLBRIGHT`] only when there is genuinely no world to sample.
    pub light: u8,
}

impl ChestSpawn {
    /// A closed, full-bright, south-facing single chest at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        ChestSpawn {
            pos,
            facing_yaw_deg: 0.0,
            half: ChestHalf::Single,
            material: ChestMaterial::Regular,
            openness: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one skull/head to draw this frame.
///
/// The caller owns every field, the same contract as [`ChestSpawn`]: block
/// state → `orientation`/`skull_type`, world light → `light`.
#[derive(Debug, Clone, PartialEq)]
pub struct SkullSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// Floor or wall placement.
    pub orientation: SkullOrientation,
    /// Which mob's model and sheet.
    pub skull_type: SkullType,
    /// Static skull sheet or the URL of a placed player head's remote skin.
    pub texture: BlockEntityTexture,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl SkullSpawn {
    /// A floor-placed, `rotation_segment = 0`, full-bright skeleton skull at
    /// `pos` — the minimum a hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        SkullSpawn {
            pos,
            orientation: SkullOrientation::Floor { rotation_segment: 0 },
            skull_type: SkullType::Skeleton,
            texture: BlockEntityTexture::Static(skull_texture_stem(SkullType::Skeleton)),
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one bell to draw this frame.
///
/// Unlike [`ChestSpawn`]/[`SkullSpawn`], placement needs no facing at all:
/// Vanilla's own bell renderer applies no rotation of its own before
/// submitting the model (contrast the chest renderer's explicit
/// rotate-around-pivot step), so every `FACING`/`ATTACHMENT` combination poses the
/// body identically — only the block's own attachment-frame *model* (drawn
/// by the ordinary block mesher, not this pass) differs per attachment.
/// [`BlockEntityModelSet::resolve_bell`] therefore calls
/// [`block_entity_placement_matrix`] with a fixed `facing_yaw_deg` of `0.0`,
/// reusing the chest's placement function unchanged rather than adding a
/// bell-specific one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BellSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// The in-progress shake (direction plus vanilla's raw tick counter,
    /// `0..50`), or `None` at rest.
    ///
    /// **`None` is the only value this pass can produce today.** The
    /// block-event trigger that starts a shake (`b0 == 1`, direction packed
    /// in `b1` — vanilla's own bell trigger-event handler) is not wired from any
    /// gather in this crate; see `docs/block-entity-renderers.md`'s Bell
    /// section for exactly what is missing and why (the install call site is
    /// outside this crate's file ownership for the session that ported the
    /// geometry). A bell always draws — closing the "hole" the doc's chest
    /// section describes for a model-less block entity — it just never
    /// shakes yet.
    pub shake: Option<(BellShakeDirection, f32)>,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl BellSpawn {
    /// A resting, full-bright bell at `pos` — the minimum a hermetic gate
    /// needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        BellSpawn {
            pos,
            shake: None,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one shulker box to draw this frame.
///
/// Three fields and no animation state, which is why this type was the cheapest
/// one to add after bell: the box's facing and its dye colour both come straight
/// off the block state (`FACING`, and the block id for the colour), and a closed
/// box needs no part override — so a shulker box slots into
/// [`plan_block_entities`]' existing `(model, texture)` batch key untouched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShulkerSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// The shulker box block's own FACING property, defaulting to
    /// [`ShulkerFacing::Up`] the way vanilla's own render-state extraction
    /// does.
    pub facing: ShulkerFacing,
    /// The dye colour name (`"red"`, …) or `None` for the undyed box.
    pub colour: Option<&'static str>,
    /// Vanilla's own open/close progress accessor — `0.0` closed, `1.0`
    /// fully open.
    ///
    /// **`0.0` is the only value this pass can produce today.** Progress comes
    /// from the block entity's own open/close counter, which the server drives
    /// through the same block-event path a chest lid uses — and unlike a chest,
    /// nothing in this workspace folds a shulker box's event yet. A closed box is
    /// what a shulker box looks like whenever nobody has it open, so this is the
    /// honest state rather than a placeholder; see
    /// `docs/block-entity-renderers.md`.
    pub progress: f32,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl ShulkerSpawn {
    /// A closed, upward-facing, undyed, full-bright box at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        ShulkerSpawn {
            pos,
            facing: ShulkerFacing::Up,
            colour: None,
            progress: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one banner — standing **or** wall — to draw
/// this frame.
///
/// The caller owns every field, the same contract as [`ChestSpawn`]: the
/// `ROTATION` property or `FACING`, whichever the block has → `attachment`; the
/// banner **block's own** colour (vanilla's own per-block-registration colour
/// — one banner block per dye colour, there is no `type`-style state
/// property, so this is
/// not read off block state the way [`ChestSpawn::material`] is) →
/// `base_color`; the block entity's own NBT `"patterns"` key
/// (`docs/banner-shield-patterns.md`'s "Prerequisite 1 does not block the
/// block-entity consumer" section — this is *not* an item component) →
/// `patterns`; the world clock → `phase` (see [`banner_phase`]); world light
/// → `light`.
///
/// Everything past `attachment` is shared by both forms, including the sway and
/// the whole pattern-layer stack — vanilla's own banner renderer picks two
/// meshes and an angle off the attachment type and then runs one banner
/// submit step for either.
#[derive(Debug, Clone, PartialEq)]
pub struct BannerSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// Standing or wall, carrying that form's own angle — see
    /// [`BannerAttachment`], and [`banner_ground_placement_matrix`] for why a
    /// rotation segment is not [`horizontal_facing_yaw`]'s convention.
    pub attachment: BannerAttachment,
    /// The banner block's own dye colour.
    pub base_color: DyeColor,
    /// The block entity's stored pattern layers, in stack order.
    pub patterns: Vec<StoredPatternLayer>,
    /// This frame's cloth-sway phase, `0.0..1.0` — see [`banner_phase`].
    pub phase: f32,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl BannerSpawn {
    /// A resting (`phase = 0`), full-bright, segment-`0` **standing**,
    /// pattern-less white banner at `pos` — the minimum a hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        BannerSpawn {
            pos,
            attachment: BannerAttachment::Ground { rotation_segment: 0 },
            base_color: DyeColor::White,
            patterns: Vec::new(),
            phase: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }

    /// The wall sibling of [`Self::at`]: a resting, full-bright, pattern-less
    /// white banner on a wall facing `facing_yaw_deg`.
    #[must_use]
    pub fn on_wall(pos: [i32; 3], facing_yaw_deg: f32) -> Self {
        BannerSpawn {
            attachment: BannerAttachment::Wall { facing_yaw_deg },
            ..BannerSpawn::at(pos)
        }
    }
}

/// One item cooking in one campfire slot.
///
/// **The only `*Spawn` here that [`BlockEntityModelSet`] does not resolve**, and
/// deliberately so: a campfire's renderer draws item *models*, not a cuboid part
/// rig, so this feeds the model pipeline through
/// [`crate::entity::campfire_item_mesh`] the way a dropped item does — see
/// [`campfire_item_matrix`]'s doc for why there is no mesh and no sheet on this
/// path at all. Sending it through `resolve_*` would need a texture stem that
/// does not exist.
///
/// One per **occupied** slot, so a campfire holding two steaks yields two of
/// these and an empty campfire yields none — matching vanilla's own submit
/// step's per-slot non-empty guard.
#[derive(Debug, Clone, PartialEq)]
pub struct CampfireItemSpawn {
    /// Block position of the campfire.
    pub pos: [i32; 3],
    /// The campfire block's `facing`, in [`horizontal_facing_yaw`]'s convention.
    pub facing_yaw_deg: f32,
    /// Which of the four cooking slots (`0..CAMPFIRE_SLOTS`) this item is in.
    /// Vanilla offsets it by the facing, so this is *not* a world corner —
    /// see [`campfire_item_matrix`].
    pub slot: CampfireSlot,
    /// The item id whose baked geometry to draw, from the block entity's NBT
    /// `Items` list.
    pub item: ResourceLocation,
    /// Packed sky/block light at the campfire.
    pub light: u8,
}

/// One suspicious sand/gravel block's revealed item, for this frame —
/// vanilla's own brushable-block renderer.
///
/// **A second `*Spawn` here [`BlockEntityModelSet`] does not resolve**, for the
/// same reason [`CampfireItemSpawn`] is the first: vanilla's own brushable-block
/// renderer draws an *item model*, not a cuboid part rig — the sand/gravel a player
/// sees is the ordinary **block** model, real geometry the terrain mesher
/// already draws (`suspicious_sand`/`suspicious_gravel` are not a hole in the
/// world), so this feeds the model pipeline through
/// [`crate::entity::brushable_item_mesh`] the way a dropped item does.
///
/// Present only once **both** vanilla's own hit-direction accessor is
/// non-null (a player has brushed at least once) and `item` is non-empty (a
/// loot table has actually rolled a reward) and `dust_progress > 0` —
/// vanilla's own three-part guard in its own submit step. A brand
/// new, never-brushed block therefore contributes no spawn at all.
#[derive(Debug, Clone, PartialEq)]
pub struct BrushableItemSpawn {
    /// Block position of the suspicious sand/gravel.
    pub pos: [i32; 3],
    /// The face a player last brushed, from `hit_direction` NBT
    /// (vanilla's own legacy direction-id codec) — feeds [`brushable_item_matrix`].
    pub hit_direction: lodestone_assets::Direction,
    /// The block state's own `dusted` property, `0..=3` —
    /// vanilla's own completion-state range, read off the
    /// state rather than re-derived from the block entity's own brush counter
    /// (which is not on the wire; only the property is).
    pub dust_progress: u8,
    /// The revealed item's id, from the block entity's `item` NBT
    /// (vanilla's own item-stack codec).
    pub item: ResourceLocation,
    /// Packed sky/block light at the block.
    pub light: u8,
}

/// One item on a shelf's `slot`, for this frame — vanilla's own shelf renderer.
///
/// **A third `*Spawn` here [`BlockEntityModelSet`] does not resolve**, for the
/// same reason [`CampfireItemSpawn`]/[`BrushableItemSpawn`] are the first
/// two: vanilla's own shelf renderer draws item models, not a cuboid part rig — a shelf's
/// own board/back/sides are all real block-model geometry the terrain
/// mesher already draws — so this feeds the model pipeline through
/// [`crate::entity::shelf_item_mesh`] the way a dropped item does.
///
/// One per **occupied** slot, matching vanilla's own submit step's per-slot
/// non-null render-state guard — an empty shelf yields none.
#[derive(Debug, Clone, PartialEq)]
pub struct ShelfItemSpawn {
    /// Block position of the shelf.
    pub pos: [i32; 3],
    /// The shelf block's `facing`, in [`horizontal_facing_yaw`]'s convention.
    pub facing_yaw_deg: f32,
    /// Which of the three slots (`0..SHELF_SLOTS`) this item is in.
    pub slot: ShelfSlot,
    /// Vanilla's own align-items-to-bottom accessor's own NBT flag.
    pub align_to_bottom: bool,
    /// The item id whose baked geometry to draw, from the block entity's
    /// `Items` NBT list.
    pub item: ResourceLocation,
    /// Packed sky/block light at the shelf.
    pub light: u8,
}

/// One vault's floating display-item cluster, for this frame — vanilla's
/// own vault renderer.
///
/// **A third `*Spawn` here [`BlockEntityModelSet`] does not resolve**, for the
/// same reason [`CampfireItemSpawn`] is the first: vanilla's own vault-submit
/// step draws multiple item-entity-style instances at a fixed pose, not a cuboid
/// part rig — the vault's own cage, base and door are all real *block* model
/// geometry the ordinary terrain mesher already draws (`blockstates/vault.json`
/// is a plain `variants` map over `facing`/`ominous`/`vault_state`, the same
/// shape the mob-spawner cage and trial-spawner's per-state textures already
/// proved), so this feeds the model pipeline through
/// [`crate::entity::vault_display_item_mesh`] the way a dropped item does.
///
/// Present only when vanilla's own client-side active-effects check
/// (its own has-display-item test) is true — an empty `shared_data.display_item`
/// yields **no** spawn for that vault, matching vanilla's own
/// non-empty guard in its own render-state extraction. A
/// vault the server has not yet rolled a reward for (state `INACTIVE`) is
/// therefore silent, not a partially-drawn cluster.
#[derive(Debug, Clone, PartialEq)]
pub struct VaultSpawn {
    /// Block position of the vault.
    pub pos: [i32; 3],
    /// The display item's id, from `shared_data.display_item.id`.
    pub item: ResourceLocation,
    /// The display item's stack count, from `shared_data.display_item.count`
    /// (vanilla's codec defaults this to `1` when absent) — feeds
    /// [`crate::entity::rendered_amount`] the same way a dropped stack's count
    /// does.
    pub count: u32,
    /// This frame's spin, in degrees — [`crate::entity::vault_spin_degrees`]
    /// evaluated at the gather's `(game_time, partial_tick)`, already resolved
    /// so the draw site needs no clock of its own.
    pub spin_deg: f32,
    /// Packed sky/block light at the vault.
    pub light: u8,
}

/// One `moving_piston` block entity for this frame — vanilla's own
/// piston-head renderer.
///
/// **The second `*Spawn` here [`BlockEntityModelSet`] does not resolve**, and for
/// the same reason [`CampfireItemSpawn`] is the first: vanilla's own
/// piston-head renderer's constructor bakes no model layer, so it owns no
/// cuboid rig. What it draws is
/// whole *block models* posed somewhere other than their own cell, which is the
/// moving-block seam (`gpu/moving_blocks.rs` in the shell) rather than either the
/// entity or the item pipeline.
///
/// # Everything here is semantic, not a matrix
///
/// The offset is deliberately **not** precomputed into a `Mat4` by the gather.
/// Vanilla's own extended-progress calculation is the one piece of arithmetic
/// in this renderer that a plausible reading gets backwards — it is
/// `progress - 1.0` while extending and `1.0 - progress` while retracting, and
/// the two agree at `progress == 0.5` — so
/// it lives next to its sibling `falling_block_pose` where its wrong hypothesis is
/// evaluated against it. Carrying `direction`/`progress`/`extending` keeps that
/// possible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovingPistonSpawn {
    /// The cell the `moving_piston` block entity itself occupies. Geometry draws
    /// at this cell **plus** the offset derived from the three fields below; the
    /// cell itself has no block model (`moving_piston` renders as
    /// invisible), which is why an unset source leaves a hole.
    pub pos: [i32; 3],
    /// The global block-state id to draw offset — already resolved by the gather,
    /// because two of vanilla's own render-state extraction's three branches
    /// *synthesise* a state rather than using the stored one (a `piston_head`
    /// with `short` set from the
    /// progress, in particular).
    pub state_id: u32,
    /// The retracting **source** piston's own base, drawn at [`Self::pos`] with
    /// no offset at all — vanilla's own submit step pops the translated pose
    /// before submitting it.
    /// `None` for every other case, which is the common one.
    pub base_state_id: Option<u32>,
    /// The block entity's own facing direction's unit step.
    ///
    /// **Not the movement direction.** Vanilla's own movement-direction
    /// accessor is `extending ? direction : direction.opposite()`, but the
    /// per-axis direction-step accessors multiply the *raw* `direction` step
    /// by a signed progress that carries the
    /// retraction's sign itself. Using the movement direction here and a positive
    /// progress would double-negate the retracting case.
    pub direction: [i32; 3],
    /// Vanilla's own progress accessor — `lerp(a, progress_o, progress)`, in `0..=1`.
    pub progress: f32,
    /// The block entity's own extending flag.
    pub extending: bool,
    /// Packed sky/block light for the offset geometry. Vanilla samples it one cell
    /// **back** along the movement direction
    /// (the block's own position, offset by the opposite of the movement
    /// direction), not at the block entity's own cell — the cell being moved
    /// *into* is the one full of
    /// `moving_piston`.
    pub light: u8,
    /// Packed sky/block light for [`Self::base_state_id`], sampled at
    /// [`Self::pos`] itself. Ignored when there is no base.
    pub base_light: u8,
}

/// Which block-entity rig and sheet draw one `minecraft:special` **item** form —
/// vanilla's own special-model-renderer family, the ex-`builtin/entity` items.
///
/// `kind` is the special-renderer id an item definition names
/// (`lodestone_assets::IconPart::Special::kind`); `item_path` is the item's own
/// registry path with the namespace stripped. Returns `(model name, texture stem)`
/// — the same two keys a placed block entity is batched on, because it is the same
/// rig and the same sheet.
///
/// # One resolver, every surface
///
/// This exists so a chest's rig is chosen in exactly **one** place for all five
/// surfaces vanilla draws these items on: the inventory slot, the first-person
/// hand, another entity's hand, a dropped stack, and an item frame. Two copies of
/// the mapping is how a chest ends up correct in the GUI and oak-coloured in the
/// hand — the copies differ in whichever `kind` was added later.
///
/// # Keyed by `kind` first and item path second, never by item path alone
///
/// The family is ten `kind`s over 91 item definitions. A `match` on the item id
/// alone would need 91 arms and would leave any datapack item invisible; the
/// `kind` says *what rig*, and the path only picks *which sheet* within it —
/// exactly vanilla's split, where vanilla's own unbaked chest special
/// renderer carries the `texture` field the item definition names.
///
/// `ChestHalf::Single` is not a simplification: vanilla's own unbaked chest
/// special renderer's `chest_type` defaults to SINGLE and no 26.2 item
/// definition overrides it, so an item chest is never one of the two double
/// halves.
///
/// # Which kinds resolve today
///
/// `chest` (13 item definitions), `shulker_box` (17), `head` (6),
/// `player_head` (1), `shield` (2, undyed/pattern-less only — see below),
/// `conduit` (1) and `copper_golem_statue` (8) — every `kind` whose rig is one
/// mesh and one sheet. The rest need more than a single pair and so have their
/// own entry points rather than an arm here:
///
/// | kind | items | why not a `(model, sheet)` pair |
/// |---|---|---|
/// | `banner` | 16 | the ordered translucent pattern-mask pass — [`banner_item_rig`] plus `crate::banner_pattern::banner_pattern_layers` |
/// | `decorated_pot` | 1 | five independently textured parts per instance — [`decorated_pot_item_rig`] |
/// | `trident` | 2 | its mesh is in the **entity** model set, not [`BLOCK_ENTITY_MODELS`] — [`trident_item_rig`] |
///
/// # The conduit item is one layer, not four
///
/// The obvious reading of vanilla's own conduit renderer is that a conduit
/// needs four (a model layer is baked for shell, cage, wind and eye), and an
/// earlier version of this doc said exactly that. That is the **block
/// entity**. Vanilla's own conduit-item special renderer bakes the shell
/// model layer alone and its own submit step issues one part-submit call
/// against its own inactive-shell texture — no cage, no wind, no eye,
/// because an item conduit is never *active*. So it is a plain pair, and it
/// always was.
///
/// # A copper golem statue item is always the **standing** pose
///
/// `copper_golem_statue.json` is a `select` on `minecraft:block_state`'s
/// `copper_golem_pose` property with a `standing` fallback, and an ordinary
/// stack carries no such property — so standing is what vanilla itself draws
/// for one in a hand or a slot. The eight item paths differ only in oxidation,
/// which is exactly what [`copper_golem_statue_oxidation_from_item_path`]
/// reads, so the two-level key lands the same way it does for a chest: the
/// `kind` picks the rig, the path picks the sheet. A stack that really does
/// carry a `minecraft:block_state` component naming another pose is the same
/// bounded shortfall `shield` records below — this signature has no room for
/// per-stack state.
///
/// **`shield` also resolves here now, but only ever as the undyed,
/// pattern-less rig.** The first-person hand and the GUI icon both bypass
/// this function and call [`shield_item_rig`] directly, because *they* carry
/// real per-stack state (`minecraft:base_color`, `minecraft:banner_patterns`)
/// this function's `(kind, item_path)` signature has no room for — see that
/// pair's own call sites (`lodestone_shell::gpu::first_person`'s
/// `prepare_special_hand`, `lodestone_shell::hud::item_icon`'s GUI-icon
/// pass). But this resolver's three callers (a dropped stack, another
/// entity's hand, an item frame) had **no** shield arm at all until this one
/// landed — `_ => None` swallowed every one of them, so a dropped or framed
/// shield drew nothing, full stop, not merely undyed. Resolving it here to
/// the no-pattern sheet unconditionally is the same *bounded* shortfall this
/// module already accepts elsewhere on these three surfaces (a dropped
/// stack's own doc: "no stack multiplication"; a framed item's own doc: "the
/// in-frame rotation is undecoded") — a real shield reaching real pixels,
/// just not its dye or loom pattern, because neither surface threads that
/// state through `EntityDraw` today.
///
/// `None` is also the right answer for an item path a `kind` does not recognise (a
/// datapack item declaring `minecraft:chest` over something that is not a chest):
/// drawing nothing beats drawing a plain oak chest for it. **All seven head paths
/// resolve**, including `dragon_head`/`piglin_head`, which reach their own
/// multi-part rigs rather than a skull layer; vanilla scales the dragon head's
/// icon down through its base model's own `gui` display transform
/// (`item/dragon_head.json`, `scale 0.6`), which is the caller's own display
/// transform and not this function's business.
#[must_use]
pub fn special_item_rig(kind: &str, item_path: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "minecraft:chest" => {
            let material = ChestMaterial::from_block_path(item_path)?;
            Some((
                CHEST_SINGLE,
                chest_texture_stem(material, ChestHalf::Single),
            ))
        }
        "minecraft:shulker_box" => {
            // `shulker_box` (the undyed one) has no colour prefix and takes the
            // default sheet; every other path is `<colour>_shulker_box`. Passing
            // the whole path through would silently take the default arm for all
            // seventeen, which is the plausible wrong version: it draws, and it
            // draws purple.
            let colour = item_path.strip_suffix("_shulker_box").filter(|c| {
                // Only a real dye colour — a datapack `foo_shulker_box` should
                // not quietly become the default sheet.
                SHULKER_COLOURS.contains(c)
            });
            if colour.is_none() && item_path != "shulker_box" {
                return None;
            }
            Some((SHULKER_BOX, shulker_texture_stem(colour)))
        }
        // Two `kind`s, one rig family: vanilla splits `player_head` out because
        // its renderer resolves a profile texture. This function has no stack in
        // hand, so it answers for a *plain* head and returns the default Steve
        // stem; a custom head's own sheet is substituted by the caller, which
        // does — the shell's GUI icon pass and its placed-head pass both replace
        // this stem with a `BlockEntityTexture::PlayerSkin`.
        "minecraft:head" | "minecraft:player_head" => {
            let ty = SkullType::from_block_path(item_path)?;
            Some((ty.model(), skull_texture_stem(ty)))
        }
        // Always the no-pattern sheet — see this function's own doc for why
        // a dyed or patterned shield still only reaches pixels through the
        // hand/GUI call sites that bypass this resolver entirely.
        "minecraft:shield" => Some((SHIELD, SHIELD_BASE_NO_PATTERN_TEXTURE_STEM)),
        // One layer, not four — vanilla's own conduit-item special renderer
        // takes the shell model alone. See this function's own doc for why the
        // four-layer reading is about the block entity instead.
        "minecraft:conduit" if item_path == "conduit" => {
            Some((CONDUIT_SHELL, CONDUIT_SHELL_TEXTURE_STEM))
        }
        // Always the standing pose: an item stack has no `copper_golem_pose`
        // block-state property, so vanilla's own `select` takes its fallback.
        "minecraft:copper_golem_statue" => {
            let oxidation = copper_golem_statue_oxidation_from_item_path(item_path)?;
            Some((
                CopperGolemPose::Standing.model_name(),
                copper_golem_statue_texture_stem(oxidation),
            ))
        }
        _ => None,
    }
}

/// A copper golem statue **item**'s oxidation level, from its own registry
/// path — the item-side twin of the block-state-keyed resolver the shell's
/// placed-statue pass uses, and the reason [`special_item_rig`] can pick a
/// sheet for all eight paths without a block state.
///
/// `waxed_` is stripped first: waxing halts further weathering but does not
/// change which of the four sheets a statue draws, and vanilla's own
/// oxidation-level table has no fifth, waxed-specific entry. That makes
/// the eight item paths four sheets, which is exactly what the eight item
/// definitions in the 26.2 jar name — `waxed_oxidized_copper_golem_statue.json`
/// and `oxidized_copper_golem_statue.json` both carry
/// `entity/copper_golem/copper_golem_oxidized`.
///
/// `None` for any other path, so a datapack item declaring
/// `minecraft:copper_golem_statue` over something that is not one draws nothing
/// rather than an unaffected-copper statue.
#[must_use]
pub fn copper_golem_statue_oxidation_from_item_path(
    item_path: &str,
) -> Option<CopperGolemOxidation> {
    let path = item_path.strip_prefix("waxed_").unwrap_or(item_path);
    Some(match path {
        "copper_golem_statue" => CopperGolemOxidation::Unaffected,
        "exposed_copper_golem_statue" => CopperGolemOxidation::Exposed,
        "weathered_copper_golem_statue" => CopperGolemOxidation::Weathered,
        "oxidized_copper_golem_statue" => CopperGolemOxidation::Oxidized,
        _ => return None,
    })
}

/// [`decorated_pot_item_rig`]'s result: five opaque draws sharing one
/// placement, in vanilla's own decorated-pot renderer's own submission order.
///
/// Five rather than two (a banner) or one (a shield) because the thing that
/// varies here is the **diffuse sheet per quad**, not a tint over one mesh —
/// which is a shape the ordinary batcher was already built for. See
/// [`BlockEntityModelSet::resolve_decorated_pot`], the placed-pot twin, whose
/// own doc carries the derivation; this is the same decomposition at the item
/// surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoratedPotItemRig {
    /// `(model, texture)` for the neck/top/bottom body — always the base sheet.
    pub base: (&'static str, &'static str),
    /// `(model, texture)` for the front face.
    pub front: (&'static str, &'static str),
    /// `(model, texture)` for the back face.
    pub back: (&'static str, &'static str),
    /// `(model, texture)` for the left face.
    pub left: (&'static str, &'static str),
    /// `(model, texture)` for the right face.
    pub right: (&'static str, &'static str),
}

impl DecoratedPotItemRig {
    /// The five draws in submission order, for a caller that wants to iterate
    /// rather than name each face — the shape every consumer actually uses.
    #[must_use]
    pub const fn parts(&self) -> [(&'static str, &'static str); 5] {
        [self.base, self.front, self.back, self.left, self.right]
    }
}

/// The `minecraft:decorated_pot` item rig — vanilla's own decorated-pot-item
/// special renderer's submit step, which forwards straight to the same
/// decorated-pot renderer submit step a placed pot uses, substituting an
/// empty decorations value when the stack carries none.
///
/// The four arguments are the sherd **item paths** off the stack's own
/// `minecraft:pot_decorations` component, namespace stripped, in vanilla's
/// record order. `None` is a plain brick face rather than "unknown" —
/// Vanilla's own pot-decorations accessor maps the brick item to "absent" on
/// the way in, so a blank face and a brick face are the same state by
/// construction.
///
/// # Every side always draws
///
/// An undecorated side takes [`DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM`] rather
/// than being skipped, because vanilla's own submit step submits a model
/// part for all four faces unconditionally. Skipping blank sides would draw a pot with
/// three invisible faces for the overwhelmingly common undecorated stack, and
/// then **silently autocorrect** the moment a player added their first sherd —
/// which is the failure that looks like a component-decode bug rather than a
/// draw one.
///
/// # This returns the whole rig even when a sherd is unrecognised
///
/// A datapack sherd path that [`decorated_pot_pattern_texture_stem`] declines
/// falls back to the default side sprite for **that face only**. The pot still
/// draws. That is deliberately unlike [`special_item_rig`]'s "decline the whole
/// item rather than guess", and the asymmetry is the point: there, an
/// unrecognised path means we do not know what rig the item wants; here we know
/// exactly what rig it wants and only one of its four sheets is unknown, and
/// vanilla's own side-sprite lookup takes precisely this fallback.
///
/// Unlike a banner's, no argument here is optional-by-shortfall — the sherds are
/// a real decoded component (`lodestone_model::PotDecorations`), so a caller
/// that has the stack can pass the truth.
#[must_use]
pub fn decorated_pot_item_rig(
    back: Option<&str>,
    left: Option<&str>,
    right: Option<&str>,
    front: Option<&str>,
) -> DecoratedPotItemRig {
    let side = |sherd: Option<&str>| -> &'static str {
        sherd
            .and_then(decorated_pot_pattern_texture_stem)
            .unwrap_or(DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM)
    };
    DecoratedPotItemRig {
        base: (DECORATED_POT_BASE, DECORATED_POT_BASE_TEXTURE_STEM),
        front: (DECORATED_POT_SIDE_FRONT, side(front)),
        back: (DECORATED_POT_SIDE_BACK, side(back)),
        left: (DECORATED_POT_SIDE_LEFT, side(left)),
        right: (DECORATED_POT_SIDE_RIGHT, side(right)),
    }
}

/// The entity-corpus model name a held `minecraft:trident` draws —
/// vanilla's own trident-item special renderer bakes its trident model from
/// the trident model layer.
///
/// # Why this is not an arm in [`special_item_rig`]
///
/// Not because the rig is unported — it has been in the tree all along. The
/// trident's mesh is `lodestone_assets::entity_models`' `"trident"` entry, the
/// same one vanilla's own thrown-trident renderer draws a thrown trident with, and
/// [`special_item_rig`]'s contract is that its `&'static str` is a key into
/// [`BLOCK_ENTITY_MODELS`]. Returning an entity-corpus name from there would
/// type-check, resolve to nothing in every one of that function's callers, and
/// draw exactly the blank hand this exists to fix — so the *corpus* is part of
/// the return type's meaning, and a separate entry point is how that is said.
///
/// This mirrors vanilla more closely than folding it in would: every one of
/// these renderers bakes out of `context.entityModelSet()`, and which corpus a
/// rig happens to live in on our side is our own storage decision, not a
/// statement about the item.
///
/// # The sheet is the corpus entry's own
///
/// Unlike a chest, there is no second key to return: the entity corpus binds a
/// texture per entry (`EntityTexture::Fixed("entity/trident/trident")`, which is
/// vanilla's own trident-model texture), so a caller looks the sheet up by
/// this same name
/// rather than by a stem. That is why this returns one string and not a pair.
///
/// # The GUI is deliberately not a caller
///
/// `trident.json` is a `select` on `minecraft:display_context` whose
/// `gui`/`ground`/`fixed`/`on_shelf` case is a plain `minecraft:model`
/// (`item/trident`, the flat sprite) — only the *fallback* reaches a
/// `minecraft:trident` special node. So an inventory trident is a sprite in
/// vanilla too, and a rig in the slot would be the regression, not the fix.
///
/// `None` for any path that is not the trident itself, so a datapack item
/// naming this `kind` over something else draws nothing rather than a trident.
#[must_use]
pub fn trident_item_rig(item_path: &str) -> Option<&'static str> {
    (item_path == "trident").then_some(TRIDENT_ENTITY_MODEL)
}

/// The `lodestone_assets::entity_models` corpus entry a trident is registered
/// under — shared by [`trident_item_rig`] and vanilla's own thrown-trident
/// renderer's own projectile path so the held and thrown tridents cannot
/// drift onto two
/// meshes.
pub const TRIDENT_ENTITY_MODEL: &str = "trident";

/// The `minecraft:banner` item rig — [`special_item_rig`]'s own doc table lists
/// this `kind` as one of the six that resolve to `None` ("needs the ordered
/// translucent pattern-mask pass, not one rig"), which was the honest state the
/// day that table was written and is also the reason a banner drew nothing at
/// all in a hotbar slot, an inventory slot or the first-person hand — not a
/// missing draw call, a `kind` this dispatcher never recognised.
///
/// # Two landings, and this is the second
///
/// The first landing multiplied the base colour directly into the opaque flag
/// texture — an approximation, disclosed at the time: vanilla's own opaque
/// body+flag pass (its own plain wood/cloth sheet
/// [`BANNER_BASE_TEXTURE_STEM`] names) is drawn **untinted**, and everything
/// the player perceives as colour is a *second*, translucent
/// pattern-submit draw layered over it — first the base mask tinted by the
/// base colour, then up to 16 loom pattern masks, each its own draw
/// (vanilla's own banner-pattern submit step). A single flat tint cannot show a
/// pattern at all.
///
/// Now that `minecraft:banner_patterns` decodes to a real, typed value (see
/// [`lodestone_model::ItemComponents::banner_patterns`]) and a caller can
/// derive the same ordered mask list a placed banner uses
/// (`crate::banner_pattern::banner_pattern_layers`, fed by
/// [`banner_item_base_color`] plus the decoded patterns), this rig's own two
/// meshes draw **untinted**, exactly like
/// [`BlockEntityModelSet::resolve_banner`]'s own `body`/`flag` — matching
/// vanilla's opaque pass exactly, with every bit of colour riding the
/// caller's own translucent layer draws instead. Reusing the field name
/// `flag_tint` for "no tint" would have been the same defaulting trap
/// `DESIGN.md` §12 records for a trait method with a `true` default: the
/// safer shape is a rig with no colour field to forget to multiply in twice.
///
/// Returns `None` for an item path that is not `<dye>_banner` — a shield, or a
/// datapack item naming this `kind` over something else.
#[must_use]
pub fn banner_item_rig(item_path: &str) -> Option<BannerItemRig> {
    banner_item_base_color(item_path)?;
    Some(BannerItemRig {
        body: (BANNER_BODY, BANNER_BASE_TEXTURE_STEM),
        flag: (BANNER_FLAG, BANNER_BASE_TEXTURE_STEM),
    })
}

/// The base dye colour parsed from a banner item's own path (`<dye>_banner`,
/// via [`crate::banner_pattern::DyeColor::from_name`]) — factored out of
/// [`banner_item_rig`] so a caller building the *translucent* pattern-layer
/// draws (base mask plus every loom pattern, via
/// [`crate::banner_pattern::banner_pattern_layers`]) derives the same base
/// colour [`banner_item_rig`] validated, rather than re-parsing the item path
/// a second, potentially diverging way. `None` for the same inputs
/// [`banner_item_rig`] rejects.
#[must_use]
pub fn banner_item_base_color(item_path: &str) -> Option<crate::banner_pattern::DyeColor> {
    let name = item_path.strip_suffix("_banner")?;
    crate::banner_pattern::DyeColor::from_name(name)
}

/// [`banner_item_rig`]'s result: two opaque draws sharing one placement, both
/// **untinted** — see that function's doc for why colour no longer lives
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BannerItemRig {
    /// `(model, texture)` for the pole/bar.
    pub body: (&'static str, &'static str),
    /// `(model, texture)` for the flag — the same mesh a caller's translucent
    /// pattern-layer draws paint over.
    pub flag: (&'static str, &'static str),
}

// A skull special renderer submits the raw Y-down skull model cube
// (built from an `addBox(-4, -8, -4, 8, 8, 8)`-shaped box, matching this crate's
// `SKULL_HUMANOID`/`SKULL_MOB` AABB). It does not supply a pose itself: 26.2's
// `items/player_head.json` and every ordinary `items/*_head.json` instead put
// `T(0.5, 0, 0.5) * Rx(180°)` on the `minecraft:special` model node. The parser
// retains that whole root-to-node chain and every consumer folds it through
// `compose_special_item_transform`. If a server pack retargets player heads to
// a bare `minecraft:head` special with an empty chain, that shared compositor
// restores this exact canonical wrapper once; parsed chains remain untouched.
