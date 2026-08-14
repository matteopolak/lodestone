//! The per-entity layer passes: mob bodies, humanoid armour, sheep wool, the
//! mob-fire billboard, and the block-entity rigs.
//!
//! # One resolver, one pose
//!
//! Every layer here resolves through the *same* `EntityModelSet` and the same
//! [`lodestone_render::AnimInput`] that [`RenderState::prepare_entities`] puts
//! on screen, and reads `instance.part_transforms` **without writing anything
//! back**. That is the whole discipline of this file: a helmet can never be
//! posed off a head the body pass did not draw, and a future "optimisation"
//! that posed a layer by mutating the wearer's transforms would break the mob
//! rather than the layer.
//!
//! # Block entities are the one input that is not a mob
//!
//! A chest, skull or bell is a *block*, gathered from the world's decoded
//! block-entity records by an installed source rather than taken from the
//! `entities` slice. Everything downstream — per-part instance buffers, the
//! group-0 camera+fog write, the frustum cull — is deliberately identical,
//! because a chest that fogged or lit differently from the mobs next to it
//! would be the more visible bug.
//!
//! Each function returns uploaded per-part instance buffers for
//! [`super::frame`] to submit; see that module on why they all run before the
//! render pass opens.
use std::collections::HashMap;

use lodestone_assets::DisplaySlot;
use lodestone_assets::entity_models::sheep_wool_tint;
use lodestone_assets::equipment::ArmourSlot;
use lodestone_model::event::EquipmentSlot;
use lodestone_render::{
    Camera, CameraUniform, EntityCameraUniform, InstanceTint, ItemStateContext,
    entity::{Arm, armour_layer_tint_with_dye, armour_layers, ground_transform, hand_transform},
    plan_block_entities, plan_entities, upload_instances_tinted,
};

use crate::entities::EntityDraw;

use super::block_entities::{BannerLayerDrawBatch, BlockEntityDrawBatch};
use super::terrain::ModelRenderer;
use super::{
    ArmourAccum, ArmourDrawBatch, ArmourPartAccum, ArmourTextureKey, EntityDrawBatch, FlameBatch,
    OrbBatch, RenderState, RenderStats, WoolPartAccum, humanoid_armour_slot,
};

/// One entity's own hitbox width in blocks — its type's base width times its age
/// scale — or `None` for a type with no `entity_dimensions` entry.
///
/// This is vanilla's `EntityRenderState.boundingBoxWidth`, and it is the only
/// input to the flame's size. `age_scale` is [`EntityDraw::scale`], which is
/// `0.5` for a `Baby` and `1.0` otherwise — vanilla's
/// `getDimensions().scale(getAgeScale())`, which scales **both** axes, so it
/// changes the flame's `s` and cannot change its layer count.
///
/// Kept as a free function rather than a method on `EntityDraw` because it reads
/// `lodestone_data`, which the draw type deliberately does not.
///
/// Resolves `type_path` through [`lodestone_data::entity_type::EntityType::from_name`]
/// (binary search over the generated registry) rather than
/// `entity_type_id_parts`'s linear `strip_prefix` scan — called once per
/// on-fire entity per frame, so the scan cost was real (issue #523).
/// Issue #573: `AvatarRenderer.setupRotations`'s swim branch — vanilla's own
/// player-only body-pitch rotation toward horizontal as `swim_amount` ramps
/// `0..1`, applied as an extra whole-body rotation on top of whatever
/// [`lodestone_render::dying_entity_model_matrix`] already placed. A no-op
/// when `swim_amount <= 0.0`, which is every non-swimming entity, every
/// frame.
///
/// # Player only, not every `LivingEntity`
///
/// Vanilla's base `LivingEntityRenderer.setupRotations` has **no** swim
/// branch at all; only `AvatarRenderer` (the player) and `DrownedRenderer`
/// override it, and they use two different formulas — a plain rotation about
/// the origin for the player, a `rotateAround` the vertical centre for a
/// drowned zombie. Porting one formula to every `LivingEntity` would be
/// wrong for the entities that report `Pose.SWIMMING` and are not a player.
/// `EntityDraw::swim_amount` is populated for every entity kind (see
/// `crate::entities::SwimRamp`), so the type gate lives at the call site in
/// [`RenderState::prepare_entities`], not in here.
///
/// # Composed by conjugation, not by re-deriving the placement
///
/// `instance.transform`/`part_transforms`/`hand_transforms` already equal
/// `A * flip_scale * lift` for `A = T(feet) · Ry(180 − yaw) · Rz(fall_over)`
/// — `dying_entity_model_matrix`'s own documented decomposition, bit for
/// bit. Vanilla inserts the swim rotation exactly between the yaw/fall-over
/// term and the Y-down flip (`AvatarRenderer.setupRotations` calls
/// `super.setupRotations` — which applies the `Ry`/`Rz` terms — and *then*
/// `mulPose(Axis.XP.rotationDegrees(xAngle))`, before `render`'s
/// `poseStack.scale(-1, -1, 1)`), so left-multiplying every already-baked
/// matrix by `A · Rx(xAngle) · A⁻¹` reproduces `A · Rx(xAngle) · flip_scale
/// · lift` exactly, without decomposing the baked matrices back into their
/// factors. `A` is rebuilt here from the same `feet`/`yaw`/`death_time`
/// inputs the resolver was called with, so it is bit-identical to the `A`
/// already folded into `instance`.
///
/// # Two vanilla pieces not ported, both because the input is not available
/// at this call site
///
/// * `AvatarRenderer.setupRotations`'s `targetXRot` is `isInWater ? -90 −
///   xRot : -90`; this always takes the water branch. `PlayerState::swimming`
///   (the producer behind the ramp) requires `FluidState::in_water`/
///   `under_water` to ever become true, so for the overwhelming majority of
///   a nonzero `swim_amount` the player genuinely is submerged — the gap is
///   only the tail of the ramp decaying back to `0.0` after leaving the
///   water, which `RenderState::prepare_entities` has no fluid query to
///   detect.
/// * `isVisuallySwimming`'s extra `translate(0, -1, 0.3)` (the crawling
///   nudge) is not ported — same reason, no fluid/on-ground state reaches
///   this call site today.
fn apply_swim_rotation(
    instance: &mut lodestone_render::EntityInstance,
    feet: glam::Vec3,
    yaw_deg: f32,
    death_time: f32,
    pitch_deg: f32,
    swim_amount: f32,
) {
    if swim_amount <= 0.0 {
        return;
    }
    let fall_over_deg = lodestone_render::entity_anim::death_fall_over_degrees(death_time);
    let a = glam::Mat4::from_translation(feet)
        * glam::Mat4::from_rotation_y((180.0 - yaw_deg).to_radians())
        * glam::Mat4::from_rotation_z(fall_over_deg.to_radians());
    // `AvatarRenderer.setupRotations`: `Mth.lerp(swimAmount, 0.0F, -90.0F - xRot)`.
    let x_angle_deg = swim_amount * (-90.0 - pitch_deg);
    let rx = glam::Mat4::from_rotation_x(x_angle_deg.to_radians());
    let extra = a * rx * a.inverse();

    instance.transform = extra * instance.transform;
    for part in &mut instance.part_transforms {
        *part = extra * *part;
    }
    for hand in &mut instance.hand_transforms {
        if let Some(h) = hand {
            *h = extra * *h;
        }
    }
    // Conservative AABB recompute: the rotation is about `feet`, so the new
    // extent is bounded by a sphere of the old maximum corner distance from
    // `feet` — cheaper than re-deriving true per-corner bounds, and it can
    // only widen the box, never wrongly cull something a tighter box would
    // have kept on screen.
    let radius = [
        glam::Vec3::new(instance.aabb_min.x, instance.aabb_min.y, instance.aabb_min.z),
        glam::Vec3::new(instance.aabb_min.x, instance.aabb_min.y, instance.aabb_max.z),
        glam::Vec3::new(instance.aabb_min.x, instance.aabb_max.y, instance.aabb_min.z),
        glam::Vec3::new(instance.aabb_min.x, instance.aabb_max.y, instance.aabb_max.z),
        glam::Vec3::new(instance.aabb_max.x, instance.aabb_min.y, instance.aabb_min.z),
        glam::Vec3::new(instance.aabb_max.x, instance.aabb_min.y, instance.aabb_max.z),
        glam::Vec3::new(instance.aabb_max.x, instance.aabb_max.y, instance.aabb_min.z),
        instance.aabb_max,
    ]
    .into_iter()
    .map(|corner| (corner - feet).length())
    .fold(0.0f32, f32::max);
    instance.aabb_min = feet - glam::Vec3::splat(radius);
    instance.aabb_max = feet + glam::Vec3::splat(radius);
}

fn flame_hitbox_width(type_path: &str, age_scale: f32) -> Option<f32> {
    let entity_type = lodestone_data::entity_type::EntityType::from_name(type_path)?;
    let dims = lodestone_data::entity_dimensions::base_dimensions_for(entity_type);
    let width = dims.width * age_scale;
    (width > 0.0).then_some(width)
}

/// Every 26.2 entity type whose registration names an explicit eye height,
/// `(type path, eye height in blocks above the feet)`, **sorted by key** for
/// [`eye_probe_offset`]'s binary search.
///
/// # Where these come from
///
/// `EntityType.Builder.eyeHeight` calls in `EntityTypes`' registration block —
/// 102 of the 158 registered types. The other 56 name none and take
/// `EntityDimensions.defaultEyeHeight`, which is `height * 0.85F`; that is the
/// fallback [`eye_probe_offset`] computes rather than a row here, so a type
/// only appears below when vanilla actually disagrees with the default.
///
/// **`height * 0.85` alone is not a usable approximation and it was tempting.**
/// A cow is `1.4` tall with an eye at `1.3`, not `1.19`; a player is `1.8` tall
/// with an eye at `1.62`, not `1.53`. Most of those tweaks happen to floor into
/// the same block cell as the default would for an entity standing on integer
/// `y`, which is exactly why a wrong table here would look right in a
/// screenshot — but three do not (`elder_guardian` `0.99875` vs `1.69788`, and
/// `ghast`/`happy_ghast` `2.6` vs `3.4`), and any entity at a non-integer `y`
/// moves the boundary under all the rest.
///
/// # What this table cannot express
///
/// Vanilla resolves the eye height of the entity's **current pose**
/// (`Entity.getDimensions(pose).eyeHeight()`), and two things it varies by are
/// not on this side of the wire as far as [`EntityDraw`] is concerned:
///
/// * **Pose.** A crouching player's eye is `1.27` and a swimming one's `0.4`,
///   from `Avatar`'s `POSES` map. `EntityDraw` carries no pose, so a sneaking
///   remote player is probed standing.
/// * **A baby's own dimensions.** Vanilla gives most babies a hand-written
///   `BABY_DIMENSIONS` with its own eye height rather than a scaled adult one —
///   a baby zombie's is `0.775`, where the adult's `1.74` halved is `0.87`.
///   `age_scale` below is the scaled-adult approximation; both land in the same
///   block cell for every baby checked, so the probe cell is right and the
///   number is not.
const EYE_HEIGHTS: &[(&str, f32)] = &[
    ("acacia_boat", 0.5625),
    ("acacia_chest_boat", 0.5625),
    ("allay", 0.36),
    ("armadillo", 0.26),
    ("armor_stand", 1.7775),
    ("arrow", 0.13),
    ("axolotl", 0.2751),
    ("bamboo_chest_raft", 0.5625),
    ("bamboo_raft", 0.5625),
    ("bat", 0.45),
    ("bee", 0.3),
    ("birch_boat", 0.5625),
    ("birch_chest_boat", 0.5625),
    ("bogged", 1.74),
    ("breeze", 1.3452),
    ("breeze_wind_charge", 0.0),
    ("camel", 2.275),
    ("camel_husk", 2.275),
    ("cat", 0.35),
    ("cave_spider", 0.45),
    ("cherry_boat", 0.5625),
    ("cherry_chest_boat", 0.5625),
    ("chicken", 0.644),
    ("cod", 0.195),
    ("copper_golem", 0.8125),
    ("cow", 1.3),
    ("creaking", 2.3),
    ("dark_oak_boat", 0.5625),
    ("dark_oak_chest_boat", 0.5625),
    ("dolphin", 0.3),
    ("donkey", 1.425),
    ("drowned", 1.74),
    ("elder_guardian", 0.99875),
    ("enderman", 2.55),
    ("endermite", 0.13),
    ("fox", 0.4),
    ("ghast", 2.6),
    ("giant", 10.44),
    ("glow_item_frame", 0.0),
    ("glow_squid", 0.4),
    ("guardian", 0.425),
    ("happy_ghast", 2.6),
    ("horse", 1.52),
    ("husk", 1.74),
    ("item", 0.2125),
    ("item_frame", 0.0),
    ("jungle_boat", 0.5625),
    ("jungle_chest_boat", 0.5625),
    ("leash_knot", 0.0625),
    ("llama", 1.7765),
    ("magma_cube", 0.325),
    ("mangrove_boat", 0.5625),
    ("mangrove_chest_boat", 0.5625),
    ("mannequin", 1.62),
    ("mooshroom", 1.3),
    ("mule", 1.52),
    ("nautilus", 0.2751),
    ("oak_boat", 0.5625),
    ("oak_chest_boat", 0.5625),
    ("pale_oak_boat", 0.5625),
    ("pale_oak_chest_boat", 0.5625),
    ("parched", 1.74),
    ("parrot", 0.54),
    ("phantom", 0.175),
    ("piglin", 1.79),
    ("piglin_brute", 1.79),
    ("player", 1.62),
    ("pufferfish", 0.455),
    ("rabbit", 0.59),
    ("salmon", 0.26),
    ("sheep", 1.235),
    ("shulker", 0.5),
    ("silverfish", 0.13),
    ("skeleton", 1.74),
    ("skeleton_horse", 1.52),
    ("slime", 0.325),
    ("sniffer", 1.05),
    ("snow_golem", 1.7),
    ("spectral_arrow", 0.13),
    ("spider", 0.65),
    ("spruce_boat", 0.5625),
    ("spruce_chest_boat", 0.5625),
    ("squid", 0.4),
    ("stray", 1.74),
    ("sulfur_cube", 0.175),
    ("tadpole", 0.195_000_01),
    ("tnt", 0.15),
    ("trader_llama", 1.7765),
    ("trident", 0.13),
    ("tropical_fish", 0.26),
    ("vex", 0.51875),
    ("villager", 1.62),
    ("wandering_trader", 1.62),
    ("wind_charge", 0.0),
    ("witch", 1.62),
    ("wither_skeleton", 2.1),
    ("wolf", 0.68),
    ("zombie", 1.74),
    ("zombie_horse", 1.52),
    ("zombie_nautilus", 0.2751),
    ("zombie_villager", 1.74),
    ("zombified_piglin", 1.79),
];

/// How far above its feet this entity type's **light probe** sits, in blocks.
///
/// Vanilla's `Entity.getLightProbePosition` is `getEyePosition`, so the eye
/// height *is* the probe offset. `age_scale` is [`EntityDraw::scale`] — vanilla
/// scales the whole `EntityDimensions`, eye height included
/// (`EntityDimensions.scale`), so a baby's probe is half an adult's.
///
/// An unknown type path returns `0.0` rather than a guess: a modded or
/// future-version entity is then probed at its feet, which is where it was
/// probed before this existed, instead of somewhere invented.
fn eye_probe_offset(type_path: &str, age_scale: f32) -> f32 {
    if let Ok(i) = EYE_HEIGHTS.binary_search_by(|(name, _)| (*name).cmp(type_path)) {
        return EYE_HEIGHTS[i].1 * age_scale;
    }
    // `EntityDimensions.defaultEyeHeight`: `height * 0.85F`, off the same base
    // dimensions table `flame_hitbox_width` reads. Same binary-search resolve
    // as that function, for the same reason (issue #523).
    lodestone_data::entity_type::EntityType::from_name(type_path)
        .map(lodestone_data::entity_dimensions::base_dimensions_for)
        .map_or(0.0, |dims| dims.height * 0.85 * age_scale)
}

/// The packed `sky << 4 | block` light one entity draws with — the whole of
/// vanilla's `EntityRenderer.getPackedLightCoords`, and the single place any
/// pass in this module or [`super::world_items`] should get an entity's light.
///
/// Two rules, both of which this file used to miss:
///
/// * **The probe is the entity's eye, not its feet.**
///   `getPackedLightCoords` is `BlockPos.containing(entity.getLightProbePosition(t))`
///   and `getLightProbePosition` returns `getEyePosition`, so a tall mob in a
///   dark cell with a lit head is lit *by its head*. Every call site here passed
///   `feet` before, and a comment on `EntityLightSource` claimed that was
///   vanilla; it never was.
/// * **Fire forces the block half to 15, and only the block half.**
///   `EntityRenderer.getBlockLightLevel` is
///   `entity.isOnFire() ? 15 : level.getBrightness(BLOCK, pos)`, while
///   `getSkyLightLevel` has no such branch — so a burning mob in a pitch-dark
///   cave lights itself without also acquiring a daytime sky, which is what
///   forcing the whole byte would do. `LightCoordsUtil.withBlock` is the vanilla
///   spelling; here the block half is the low nibble, so it is `| 0x0F`.
///
/// `BlockPos.containing` floors, and so does the sampler on the other side of
/// [`EntityLightSource`] — the offset is added in world space and the truncation
/// happens once, in the world lookup.
pub(super) fn entity_light(source: &super::EntityLightSource, draw: &EntityDraw) -> u8 {
    // `type_path`, not `model_type_path`: the eye height is a property of the
    // entity *type*, and a slim-rigged player is still a `player`.
    let offset = eye_probe_offset(&draw.type_path, draw.scale);
    let packed = source.sample(draw.feet + glam::Vec3::new(0.0, offset, 0.0));
    if draw.on_fire { packed | 0x0F } else { packed }
}

/// Fold one layer's instances into `accum`, finding-or-creating the
/// `(slot, texture)` group and, within it, the per-part row.
///
/// Shared by the armour-sheet and trim arms of [`RenderState::prepare_armour`]
/// **so the two cannot diverge**: they must produce byte-identical transforms for
/// the same wearer or the trim would sit a fraction off the piece it decorates.
/// Insertion order is preserved, which is what keeps a slot's trim after that
/// slot's layers — see [`ArmourDrawBatch`].
fn push_armour_instances(
    accum: &mut Vec<ArmourAccum>,
    slot: ArmourSlot,
    texture: ArmourTextureKey,
    attached: &[(lodestone_render::PartRange, usize)],
    instance: &lodestone_render::entity::EntityInstance,
    light: u32,
    tint: InstanceTint,
) {
    let group = match accum
        .iter_mut()
        .position(|a| a.slot == slot && a.texture == texture)
    {
        Some(i) => &mut accum[i],
        None => {
            accum.push(ArmourAccum {
                slot,
                texture,
                parts: Vec::new(),
            });
            accum.last_mut().expect("just pushed")
        }
    };
    for (range, wearer_index) in attached {
        let Some(transform) = instance.part_transforms.get(*wearer_index) else {
            continue;
        };
        let part = match group.parts.iter_mut().position(|p| p.range == *range) {
            Some(i) => &mut group.parts[i],
            None => {
                group.parts.push(ArmourPartAccum {
                    range: *range,
                    transforms: Vec::new(),
                    lights: Vec::new(),
                    tints: Vec::new(),
                });
                group.parts.last_mut().expect("just pushed")
            }
        };
        part.transforms.push(*transform);
        part.lights.push(light);
        part.tints.push(tint);
    }
}

impl RenderState {

    /// The trim sprite this wearer's `slot` should draw over its armour, or `None`
    /// for an untrimmed piece, an unknown pattern/material, or no pack.
    ///
    /// The `wearer_asset_id` argument to `trim_sprite_id` is the **armour's own**
    /// material id, not the trim's, and it is load-bearing:
    /// `TrimMaterial::suffix_for` overrides the suffix when the two coincide, so
    /// diamond trim on diamond armour resolves `diamond_darker` and is visible
    /// instead of vanishing into the piece. `armour_layers` cannot supply it — it
    /// discards the `ArmourAsset` — which is why this goes back to
    /// `equipment::armour_item`.
    fn trim_sprite_for(
        &self,
        draw: &EntityDraw,
        slot: ArmourSlot,
        item_path: &str,
    ) -> Option<lodestone_assets::ResourceLocation> {
        use lodestone_assets::trim::{trim_material, trim_pattern, trim_sprite_id};

        if self.entities.trim_textures.is_empty() {
            return None;
        }
        let (_, trim) = draw
            .equipment_trim
            .iter()
            .find(|(s, _)| humanoid_armour_slot(*s) == Some(slot))?;
        let pattern = trim_pattern(&trim.pattern)?;
        let material = trim_material(&trim.material)?;
        let (_, asset) = lodestone_assets::equipment::armour_item(item_path)?;
        let id = trim_sprite_id(pattern, material, slot.layer_type(), asset.id).ok()?;
        self.entities.trim_textures.contains_key(&id).then_some(id)
    }

    /// Resolve each interpolated entity into a renderable instance, frustum-cull
    /// and group them by model, upload one instance buffer per surviving model,
    /// and record draw/cull counts. Runs before the render pass so every GPU
    /// buffer it creates outlives the pass that reads it.
    ///
    /// # Why this plans twice (that fix's hurt overlay)
    ///
    /// `plan_entities` groups by model and drops the input order, so a
    /// per-entity flag cannot be zipped back onto a batch afterwards — and
    /// `EntityInstance` (in `lodestone-render`'s `entity.rs`) carries only the
    /// light byte, not the overlay. The instances are therefore split by
    /// [`EntityDraw::hurt`] *before* planning, and each half's flag stays
    /// attached to the plan it produced as a `(bool, EntityFrame)` pair. That
    /// pairing is the point: a `Vec<bool>` parallel to the batches would be an
    /// invariant nothing enforces, which is precisely how this class of bug
    /// comes back. Grouping by `(model, hurt)` instead of `model` is also what
    /// a hurt mob costs in vanilla — one extra batch while its 10 ticks run,
    /// and nothing at all the rest of the time (the hurt half is empty, and
    /// `plan_entities` on an empty slice returns no batches).
    pub(super) fn prepare_entities(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<EntityDrawBatch> {
        if entities.is_empty() {
            return Vec::new();
        }

        // Rewrite the entity group-0 uniform: view-projection (world position
        // lives per-instance, so the section origin stays zero), **this frame's
        // fog** from the same `self.fog` the terrain sections get, and **this
        // frame's sky darkening**. Both passes therefore fade on one curve; a mob
        // under water or at the render edge dissolves with the blocks around it
        // instead of punching through.
        //
        // Sky darkening rides the fog block's one spare lane, and is rewritten
        // every frame rather than at install time, because the world clock moves:
        // a value captured once would freeze the mob at whatever time of day it
        // happened to spawn.
        let eye = camera.position;
        queue.write_buffer(
            &self.entities.cam_buffer,
            0,
            bytemuck::bytes_of(
                &EntityCameraUniform {
                    camera: CameraUniform {
                        view_proj: self.world_view_projection(camera).to_cols_array_2d(),
                        section_origin: [0.0, 0.0, 0.0, 0.0],
                    },
                    fog: self.fog_with_clock(eye),
                }
                .with_sky_darken(self.sky_darken.value()),
            ),
        );

        // Split by `(hurt, creeper white-flash alpha, skin url, variant sheet)` here,
        // at the one point that still knows which `EntityDraw` each instance came
        // from — `plan_entities` groups by model and drops the input order, so none
        // of the four can be zipped back on.
        //
        // The skin half is a `Vec` of groups rather than a `HashMap` because it is
        // *ordered by first appearance* and tiny: one group for everything with no
        // skin (every mob in the world) plus one per distinct skin actually in
        // view, which is bounded by the number of players on screen. The white-flash
        // byte joins the key for the same reason `hurt` is in it — the tint is one
        // repeated value per batch — and costs nothing off a creeper's fuse, where
        // every entity in view shares the byte `0`. The variant sheet joins it for
        // the *skin's* reason rather than the tint's: it selects a bind group, so a
        // shared group would draw one breed's sheet on every breed.
        let mut groups: Vec<(bool, u8, Option<String>, Option<&'static str>, Vec<_>)> = Vec::new();
        for e in entities {
            // `resolve_posed`, not `resolve`, and this is the *only* call site that
            // needs it: the pitch selects the **placement**, and a
            // projectile placed by the mob matrix draws 1.501 blocks high and
            // mirrored. For every mob the extra argument changes nothing — a mob's
            // pitch is head tracking and arrives through `e.anim`, not through the
            // placement — so the other five `resolve` call sites are deliberately
            // left alone rather than widened for symmetry.
            // `model_type_path`, not `type_path`: a player whose skin declares the
            // slim rig resolves `player_slim` here and nowhere else. The rig and
            // the sheet have to change together, and this is the rig half — the
            // sheet half is the group key below.
            // `resolve_animated`, not `resolve_posed`: the creeper swell is a
            // whole-model scale composed above the root part so it has to reach the
            // *pose*, and the death fall-over is a Z rotation between the body yaw
            // and the Y-down flip so it has to reach the *placement*.
            // `resolve_posed` passes a hard `0.0` for both. Those two missing
            // arguments are what made the entire creeper swell (the scale, the ±1%
            // wobble, the bounds pad, the white blink) and the entire death
            // fall-over reach zero pixels while every formula behind them was built
            // and unit-tested; `0.0` is an exact identity for each, so nothing
            // looked wrong anywhere.
            let Some(mut instance) = self.entities.models.resolve_animated(
                e.model_type_path(),
                e.feet,
                e.yaw,
                e.pitch,
                e.scale,
                &e.anim,
                e.creeper_swelling,
                e.death_time,
            ) else {
                continue;
            };
            // Issue #573: the swim body-pitch rotation. Gated on `type_path`,
            // not on `swim_amount > 0.0` alone — see `apply_swim_rotation`'s
            // own doc for why only the player is ported.
            if e.type_path == "player" {
                apply_swim_rotation(&mut instance, e.feet, e.yaw, e.death_time, e.pitch, e.swim_amount);
            }
            let instance = instance.with_light(entity_light(&self.entity_light, e));
            // `CreeperRenderer.getWhiteOverlayProgress` through
            // `OverlayTexture`'s 16-column quantise. Suppressed while `hurt` is on
            // because vanilla's overlay texture puts red and white on **mutually
            // exclusive rows** (the red row ignores the white column entirely), so
            // red always wins — a creeper hurt mid-fuse flashes red, never pink.
            let white = if e.hurt {
                0
            } else {
                lodestone_render::entity_pipeline::creeper_overlay_alpha_from_progress(
                    lodestone_render::entity_anim::creeper_white_overlay_progress(
                        e.creeper_swelling,
                    ),
                )
            };
            let skin = e.player_skin.as_ref().map(|s| s.url.clone());
            // The variant sheet joins the key for `skin`'s reason: texture identity
            // is not the model, so without it every wolf breed shares one bind group
            // and all nine draw pale. See `EntityDrawBatch::variant_sheet`.
            let sheet = e.variant_sheet;
            match groups.iter_mut().position(|(hurt, flash, url, s, _)| {
                *hurt == e.hurt && *flash == white && *url == skin && *s == sheet
            }) {
                Some(i) => groups[i].4.push(instance),
                None => groups.push((e.hurt, white, skin, sheet, vec![instance])),
            }
        }

        let frustum = camera.frustum();
        // The two flags, the sheet and the plan they describe travel as one value
        // from here on. A `Vec<bool>`/`Vec<Option<String>>` parallel to the batches
        // would be an invariant nothing enforces, which is precisely how this
        // class of bug comes back.
        let plans: Vec<_> = groups
            .into_iter()
            .map(|(hurt, white, skin, sheet, instances)| {
                (hurt, white, skin, sheet, plan_entities(&instances, &frustum))
            })
            .collect();
        stats.entities_drawn = plans.iter().map(|(.., f)| f.stats.drawn).sum();
        stats.entities_culled = plans.iter().map(|(.., f)| f.stats.culled_frustum).sum();

        // One instance buffer per *part*, not per entity: the mesh's vertices are
        // part-local, so a limb only moves if its own matrices are uploaded
        // separately. A mob is ~10–35 parts but hundreds of quads, so this moves
        // roughly 1% of the data a per-entity vertex re-bake would.
        plans
            .iter()
            .flat_map(|(hurt, white, skin, sheet, frame)| {
                frame
                    .batches
                    .iter()
                    .map(move |batch| (*hurt, *white, skin.as_ref(), *sheet, batch))
            })
            .map(|(hurt, white, skin, sheet, batch)| {
                let count = u32::try_from(batch.transforms.len()).unwrap_or(u32::MAX);
                // Every instance in this batch shares one overlay state — both
                // halves of it — by construction of the split above, so one repeated
                // value rather than a per-instance vector, and no way for the two to
                // disagree.
                let tints = vec![
                    InstanceTint::NONE
                        .with_hurt(hurt)
                        .with_creeper_white_overlay(white);
                    batch.transforms.len()
                ];
                // Every part uploads the *same* light and tint slices: a mob's
                // lightmap sample and its overlay state are per entity, so its
                // head and its leg share both values.
                let parts = batch
                    .parts
                    .iter()
                    .map(|p| upload_instances_tinted(device, p, &batch.lights, &tints))
                    .collect();
                EntityDrawBatch {
                    model: batch.model,
                    count,
                    parts,
                    skin: skin.cloned(),
                    variant_sheet: sheet,
                }
            })
            .collect()
    }

    /// Resolve this frame's **humanoid armour layers** into per-`(slot, texture)`
    /// instance buffers, ready to draw over the mobs wearing them.
    ///
    /// # Every piece is posed off the wearer's own part matrix
    ///
    /// Vanilla's armour model is an instance of the wearer's model *class* and is
    /// animated by the wearer's render state, so a zombie's chestplate reaches
    /// out in front with `animateZombieArms`. The equivalent here is to run no
    /// second pose at all: `ArmourMesh::attach` pairs each armour part with the
    /// wearer's index for the same name, and this reads
    /// `instance.part_transforms[i]` — the matrix the mob is *already* being
    /// drawn with.
    ///
    /// **Nothing is written back.** That is the same discipline
    /// `EntityInstance::hand_transforms` exists to enforce for held items: there,
    /// folding the item's pivot shift into `part_transforms` would have dragged
    /// the visible arm along with the sword. Armour needs the wearer's matrix
    /// *unmodified*, so there is nothing to fold in — but the rule is the same
    /// one, and a future "optimisation" that poses armour by mutating the
    /// wearer's transforms would break the mob, not the armour.
    ///
    /// # What is deliberately not handled
    ///
    /// * **Trims are handled here now**, and the note this bullet used
    ///   to carry was stale in all three of its claims: `minecraft:trim` *is*
    ///   decoded (`read_component_patch`'s own arm), `net::entity_snapshot` no
    ///   longer exists (`entities::resolve_entity_facts` lifts it beside the dye),
    ///   and `TrimAtlas` needs **no** stitching — it hands back one full-size
    ///   palette-swapped sheet per `(pattern, suffix, layer type)`. Nor is a third
    ///   depth mode involved: all eighteen of 26.2's patterns are `decal: false`,
    ///   so a trim draws through `armour_pipeline` like any other layer, and
    ///   `EntityPipeline::trim_decal_pipeline` stays selectable and unused.
    /// * **The local player's own trim, in third person.** `ThirdPersonBodyState`
    ///   reads the inventory through `lodestone_game`'s `ComponentMap`, which drops
    ///   `trim` at its `From<&lodestone_model::ItemStack>` boundary — the same
    ///   boundary that drops the local player's dye. One shared fix, in a crate
    ///   this pass does not own.
    /// * **Baby rigs.** Vanilla swaps in a whole second mesh set
    ///   (`createBabyArmorMesh`, `humanoid_baby` sheets, its own deformations);
    ///   a baby zombie wears adult armour scaled by the mob's 0.5 uniform scale
    ///   instead. Visibly close, not vanilla.
    /// * **Enchantment glint.** `hasFoil` is not on this side of the wire.
    pub(super) fn prepare_armour(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<ArmourDrawBatch> {
        // No pack, no sheets, nothing to draw — and no synthetic fallback, on
        // purpose (see `EntityRenderer::armour_textures`).
        if self.entities.armour_textures.is_empty() {
            return Vec::new();
        }
        let frustum = camera.frustum();
        let mut accum: Vec<ArmourAccum> = Vec::new();

        for draw in entities {
            if draw.equipment.is_empty() {
                continue;
            }
            // Cheap reject before any pose work: most equipment is a held item.
            if !draw
                .equipment
                .iter()
                .any(|(slot, _)| humanoid_armour_slot(*slot).is_some())
            {
                continue;
            }
            // Same resolver, same `AnimInput` **and same rig selection** as
            // `prepare_entities`, so a piece of armour can never be posed off a
            // different pose — or a different model — than the body it is drawn
            // over. `model_type_path` is the rig half: a slim player's chestplate
            // has to be posed off the *slim* body's part matrices.
            let Some(instance) = self.entities.models.resolve(
                draw.model_type_path(),
                draw.feet,
                draw.yaw,
                draw.scale,
                &draw.anim,
            ) else {
                continue;
            };
            if !frustum.intersects_aabb(instance.aabb_min, instance.aabb_max) {
                continue;
            }
            let Some(wearer) = self.entities.models.get(instance.model) else {
                continue;
            };
            // The wearer's own light, eye-probed and fire-forced — armour is one
            // of the wearer's model layers in vanilla, drawn from the *same*
            // `state.lightCoords` its body is, so the two can never disagree.
            let light = u32::from(entity_light(&self.entity_light, draw));

            // Walk the *slots* rather than the equipment list, so the draw order
            // is `HumanoidArmorLayer.submit`'s (chest, legs, feet, head)
            // regardless of what order the server happened to send.
            for slot in ArmourSlot::ALL {
                let Some((_, id)) = draw
                    .equipment
                    .iter()
                    .find(|(s, _)| humanoid_armour_slot(*s) == Some(slot))
                else {
                    continue;
                };
                // A modded namespace has no entry in the 26.2 asset table, and
                // guessing one would draw the wrong material.
                if id.namespace() != "minecraft" {
                    continue;
                }
                let layers = armour_layers(slot, id.path());
                if layers.is_empty() {
                    continue;
                }
                let Some(mesh) = self.entities.armour_models.get(slot) else {
                    continue;
                };
                // The humanoid gate lives inside `attach`: a pig handed a
                // chestplate resolves `body` by name and still wears nothing.
                let attached: Vec<_> = mesh.attach(&wearer.skeleton).collect();
                if attached.is_empty() {
                    continue;
                }
                for layer in layers {
                    let sheet = (layer.texture, slot.layer_type());
                    if !self.entities.armour_textures.contains_key(&sheet) {
                        continue;
                    }
                    let texture = ArmourTextureKey::Sheet(sheet);
                    // Vanilla's overlay is sampled by every layer of a
                    // `LivingEntityRenderer`'s model, armour included — a hurt
                    // mob whose breastplate stayed its own colour would read as
                    // a rendering fault, not as damage.
                    //
                    // `dye` is looked up per-slot, not per-layer: a slot's dye
                    // applies to every layer drawn for it, and
                    // `armour_layer_tint_with_dye` itself is what ignores the
                    // dye for a non-dyeable layer (diamond, iron, …) — see
                    // that function's own doc and `docs/armour-rendering.md`.
                    let dye = draw
                        .equipment_dye
                        .iter()
                        .find(|(s, _)| humanoid_armour_slot(*s) == Some(slot))
                        .map(|(_, dye)| *dye);
                    let tint =
                        InstanceTint::rgb(armour_layer_tint_with_dye(layer, dye)).with_hurt(draw.hurt);
                    push_armour_instances(
                        &mut accum,
                        slot,
                        texture,
                        &attached,
                        &instance,
                        light,
                        tint,
                    );
                    stats.armour_layers_drawn += 1;
                }

                // This slot's trim, **after** its own layers so the coplanar
                // `LessEqual` depth test lets it win. Once per slot rather than
                // once per layer: vanilla's `HumanoidArmorLayer` draws the trim as
                // a single pass over the slot, so a leather piece (two layers) still
                // gets one trim.
                //
                // Untinted white, and that is not an oversight: the sprite is
                // *already* the material's colour (`TrimAtlas` palette-swaps it per
                // material), so multiplying by a dye would tint gold trim green on
                // dyed leather.
                if let Some(sprite) = self.trim_sprite_for(draw, slot, id.path()) {
                    push_armour_instances(
                        &mut accum,
                        slot,
                        ArmourTextureKey::Trim(sprite),
                        &attached,
                        &instance,
                        light,
                        InstanceTint::rgb([255, 255, 255]).with_hurt(draw.hurt),
                    );
                    stats.armour_trims_drawn += 1;
                }
            }
        }

        accum
            .into_iter()
            .map(|group| ArmourDrawBatch {
                slot: group.slot,
                texture: group.texture,
                parts: group
                    .parts
                    .into_iter()
                    .filter_map(|p| {
                        let count = u32::try_from(p.transforms.len()).unwrap_or(u32::MAX);
                        upload_instances_tinted(device, &p.transforms, &p.lights, &p.tints)
                            .map(|buffer| (p.range, buffer, count))
                    })
                    .collect(),
            })
            .collect()
    }

    /// Resolve this frame's on-fire entities into per-model-type flame
    /// instance buffers (— player report: "mobs dont show flames
    /// yet"). One [`FlameBatch`] per distinct `EntityDraw::type_path` that has
    /// at least one on-fire, frustum-visible instance this frame.
    ///
    /// No pack, no texture, nothing to draw — and no synthetic fallback, on
    /// purpose (see `EntityRenderer::flame_texture`'s doc, the same asymmetry
    /// `wool_texture`/`armour_textures` already document).
    ///
    /// # The transform, and the two things that used to be wrong about it
    ///
    /// Both the billboard rotation and the uniform scale come from
    /// `lodestone_render::entity_pipeline::flame_instance_matrix`, which is the
    /// single place either is decided. Read its doc before changing the sign.
    ///
    /// * **The rotation is `Ry(PI - yaw)`**, not `Ry(yaw)`. The earlier
    ///   `Ry(yaw)` was defended on the grounds that `cull_mode: None` makes a
    ///   flat billboard sign-symmetric; the flame is a *stack* with depth and
    ///   lateral inset, so it is not, and the wrong sign made it counter-rotate
    ///   as the camera orbited — right from one side, displaced from the other.
    /// * **The scale is per instance and reads the entity's own hitbox width**,
    ///   `base width × EntityDraw::scale`, so a baby's flame is half an adult's.
    ///   It also restores vanilla's `pose.scale(s, s, s)`, which was missing
    ///   outright: every flame was `1/s` too large, worst on a wide mob.
    ///
    /// The mesh stays keyed by entity type, and correctly so — see
    /// `flame_instance_matrix`'s doc for why an age scale cannot change the layer
    /// count.
    pub(super) fn prepare_flame(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<FlameBatch> {
        let Some(_flame_texture) = &self.entities.flame_texture else {
            return Vec::new();
        };
        // The current animation frame. Both `fire_0`/`fire_1` have exactly 32
        // frames, held one *render* frame each rather than the real 20 Hz
        // game tick — see `docs/entity-rendering.md`'s "Mob fire" section for
        // why: avoiding a new parameter threaded through `render`/
        // `render_with_crack`/`render_with_effects`'s call sites.
        let tick = self.flame_frame_counter.get();
        self.flame_frame_counter.set(tick.wrapping_add(1));
        let frame = (tick % 32) as u32;

        let frustum = camera.frustum();
        let mut accum: HashMap<String, Vec<glam::Mat4>> = HashMap::new();

        for draw in entities {
            if !draw.on_fire {
                continue;
            }
            if !self.entities.flame_gpu_models.contains_key(&draw.type_path) {
                continue;
            }
            // The entity's **own** hitbox width, which is vanilla's
            // `EntityRenderState.boundingBoxWidth` — its type's base width times
            // its age scale. `EntityDraw::scale` is exactly that age scale
            // (`0.5` for a `Baby`, `1.0` otherwise), and it scales the box
            // uniformly, so it does not disturb the mesh's layer count.
            //
            // Declining when the type has no dimensions entry is unreachable in
            // practice — `flame_gpu_models` is built from the same table and was
            // just checked above — but it must not silently become a width of
            // zero, which would collapse the flame to a point.
            let Some(bb_width) = flame_hitbox_width(&draw.type_path, draw.scale) else {
                continue;
            };
            let Some(instance) = self.entities.models.resolve(
                draw.model_type_path(),
                draw.feet,
                draw.yaw,
                draw.scale,
                &draw.anim,
            ) else {
                continue;
            };
            if !frustum.intersects_aabb(instance.aabb_min, instance.aabb_max) {
                continue;
            }
            let transform = lodestone_render::entity_pipeline::flame_instance_matrix(
                draw.feet,
                camera.yaw,
                bb_width,
            );
            accum.entry(draw.type_path.clone()).or_default().push(transform);
            stats.flame_billboards_drawn += 1;
        }

        accum
            .into_iter()
            .filter_map(|(model, transforms)| {
                let count = u32::try_from(transforms.len()).unwrap_or(u32::MAX);
                lodestone_render::entity_pipeline::upload_flame_instances(device, &transforms, frame)
                    .map(|buffer| FlameBatch { model, buffer, count })
            })
            .collect()
    }

    /// Resolve this frame's experience orbs into per-sprite-cell instance
    /// buffers — `ExperienceOrbRenderer`, which is one camera-facing quad each.
    ///
    /// No pack, no sheet, nothing to draw, and no synthetic fallback — the same
    /// asymmetry `EntityRenderer::flame_texture`/`wool_texture` document.
    ///
    /// # Batched by sprite cell, because the cell is geometry
    ///
    /// `ExperienceOrb.getIcon()` buckets an orb's value into one of eleven cells,
    /// and the cell rides the quad's **UVs**, so two orbs in different buckets
    /// cannot share an instanced draw. One batch per cell actually on screen; a
    /// world full of 1-XP orbs is therefore still a single draw call.
    ///
    /// # Everything else is per instance
    ///
    /// The pulsing green (`experience_orb_tint`) is derived from each orb's **own**
    /// age, so two orbs that spawned a tick apart are at different points of the
    /// cycle — it cannot be hoisted to a per-frame uniform. The light is each orb's
    /// own eye sample with vanilla's `+7` block-nibble boost
    /// (`experience_orb_light`), which is what keeps an orb readable on a cave
    /// floor.
    ///
    /// The billboard rotation *is* per frame: `camera_orientation` depends only on
    /// the camera, so it is computed once here and shared, exactly as
    /// `prepare_item_geometry` does for thrown projectiles.
    pub(super) fn prepare_orbs(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<OrbBatch> {
        let (Some(_orb_texture), Some(_orb_model)) =
            (&self.entities.orb_texture, &self.entities.orb_gpu_model)
        else {
            return Vec::new();
        };
        let orientation = lodestone_render::entity::camera_orientation(camera.view_matrix());
        let frustum = camera.frustum();
        // One accumulator per sprite cell, indexed by the cell itself — an orb's
        // icon is a small dense integer, so this is a `Vec` rather than a map.
        let mut accum: Vec<(Vec<glam::Mat4>, Vec<u32>, Vec<InstanceTint>)> =
            (0..lodestone_render::EXPERIENCE_ORB_ICON_COUNT)
                .map(|_| (Vec::new(), Vec::new(), Vec::new()))
                .collect();

        for draw in entities {
            // `Some` for orbs and only orbs — see `EntityDraw::experience_orb_value`.
            let Some(value) = draw.experience_orb_value else {
                continue;
            };
            // The sprite is 0.3 blocks across and lifted 0.1, so a half-block box
            // around the orb's feet covers it however the camera is turned. No
            // `EntityModelSet::resolve` to take an AABB from: an orb has no rig,
            // which is the whole reason this pass exists.
            if !frustum.intersects_aabb(
                draw.feet - glam::Vec3::splat(0.5),
                draw.feet + glam::Vec3::splat(0.5),
            ) {
                continue;
            }
            let icon = lodestone_render::experience_orb_icon(value);
            let Some((transforms, lights, tints)) = accum.get_mut(icon as usize) else {
                continue;
            };
            transforms.push(lodestone_render::experience_orb_matrix(
                draw.feet,
                orientation,
            ));
            // `entity_light` is the shared eye-probe (and fire-force) every other
            // entity layer here uses; the `+7` boost is applied on top of its
            // result, which is exactly where vanilla's override sits — it wraps
            // `super.getBlockLightLevel`, it does not replace the probe.
            lights.push(u32::from(lodestone_render::experience_orb_light(
                entity_light(&self.entity_light, draw),
            )));
            tints.push(InstanceTint::rgb(lodestone_render::experience_orb_tint(
                draw.anim.age_ticks,
            )));
            stats.experience_orbs_drawn += 1;
        }

        accum
            .into_iter()
            .enumerate()
            .filter(|(_, (transforms, ..))| !transforms.is_empty())
            .filter_map(|(icon, (transforms, lights, tints))| {
                let count = u32::try_from(transforms.len()).unwrap_or(u32::MAX);
                upload_instances_tinted(device, &transforms, &lights, &tints).map(|buffer| {
                    OrbBatch {
                        icon: u32::try_from(icon).unwrap_or(0),
                        buffer,
                        count,
                    }
                })
            })
            .collect()
    }

    /// Sheep wool layers, over the same instances `prepare_entities`
    /// resolved — same resolver, same `AnimInput`, so wool can never be posed
    /// off a different pose than the body it grows out of. Mirrors
    /// [`prepare_armour`](Self::prepare_armour) exactly, minus the per-slot/
    /// per-texture grouping armour needs: wool has one mesh and one sheet, so
    /// every attached part accumulates into a single set of per-part buffers.
    ///
    /// # What is deliberately not handled
    ///
    /// * **Sheared sheep.** `draw.wool.sheared` is checked here, not filtered
    ///   upstream — [`EntityDraw::wool`]'s own doc explains why the data stays
    ///   honest about what the server reported. This is vanilla's own
    ///   `if (!state.isSheared)` gate (`SheepWoolLayer.submit`), applied at
    ///   exactly the point that draws the mesh.
    /// * **The pig/cow trap.** [`WoolMesh::attach`]'s `wearer_model` argument
    ///   is `instance.model` — the *resolved* model name — never
    ///   `wearer.family()`. `AnimFamily::Quadruped` is shared by `pig`, `cow`
    ///   and `wolf`; gating on family alone would grow wool on a pig the way
    ///   an ungated armour attach once drew a breastplate on one. In practice
    ///   `EntityDraw::wool` is already `None` for every non-sheep type
    ///   ([`crate::entities::sheep_wool`]'s own gate), so this is a second,
    ///   independent gate rather than the only one — belt and braces, the same
    ///   discipline `docs/entity-rendering.md` asks for.
    /// * **Baby sheep, the `jeb_` rainbow name, and the undercoat overlay.**
    ///   Not built — see `docs/entity-rendering.md`'s "deliberately out of
    ///   scope" list, unchanged by this pass.
    pub(super) fn prepare_wool(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<(lodestone_render::PartRange, wgpu::Buffer, u32)> {
        // No pack, no sheet, nothing to draw — and no synthetic fallback, on
        // purpose (see `EntityRenderer::wool_texture`).
        let (Some(wool_texture), Some(_wool_gpu)) =
            (&self.entities.wool_texture, &self.entities.wool_gpu)
        else {
            return Vec::new();
        };
        let _ = wool_texture; // presence check only; the bind group is read at draw time.
        let frustum = camera.frustum();
        let mut accum: Vec<WoolPartAccum> = Vec::new();

        for draw in entities {
            let Some(wool) = draw.wool else { continue };
            // Vanilla's own gate: a sheared sheep grows no wool mesh at all.
            if wool.sheared {
                continue;
            }
            let Some(instance) = self.entities.models.resolve(
                &draw.type_path,
                draw.feet,
                draw.yaw,
                draw.scale,
                &draw.anim,
            ) else {
                continue;
            };
            if !frustum.intersects_aabb(instance.aabb_min, instance.aabb_max) {
                continue;
            }
            let Some(wearer) = self.entities.models.get(instance.model) else {
                continue;
            };
            // The pig/cow-trap gate lives inside `attach`, keyed on the
            // resolved model name — see this method's docs.
            let attached: Vec<_> = self
                .entities
                .wool_models
                .mesh()
                .attach(&wearer.skeleton, instance.model)
                .collect();
            if attached.is_empty() {
                continue;
            }
            // Same source as the body: the wool is one of the sheep's own model
            // layers, so it takes the sheep's eye-probed, fire-forced light.
            let light = u32::from(entity_light(&self.entity_light, draw));
            // Same reason armour carries it: the wool is one of the sheep's
            // model layers, so it reddens with the body.
            let tint = InstanceTint::rgb(sheep_wool_tint(wool.color)).with_hurt(draw.hurt);
            for (range, wearer_index) in &attached {
                let Some(transform) = instance.part_transforms.get(*wearer_index) else {
                    continue;
                };
                let part = match accum.iter_mut().position(|p| p.range == *range) {
                    Some(i) => &mut accum[i],
                    None => {
                        accum.push(WoolPartAccum {
                            range: *range,
                            transforms: Vec::new(),
                            lights: Vec::new(),
                            tints: Vec::new(),
                        });
                        accum.last_mut().expect("just pushed")
                    }
                };
                part.transforms.push(*transform);
                part.lights.push(light);
                part.tints.push(tint);
            }
            stats.wool_layers_drawn += 1;
        }

        accum
            .into_iter()
            .filter_map(|p| {
                let count = u32::try_from(p.transforms.len()).unwrap_or(u32::MAX);
                upload_instances_tinted(device, &p.transforms, &p.lights, &p.tints)
                    .map(|buffer| (p.range, buffer, count))
            })
            .collect()
    }

    /// Resolve this frame's block entities (chests, that fix) into per-part
    /// instance buffers, frustum-culled and batched by `(model, sheet)`.
    ///
    /// # The one thing that is *not* like `prepare_entities`
    ///
    /// A chest's input does not come from the `entities` slice — it is a block,
    /// gathered from the world's decoded block-entity records by the source the
    /// shell installs. Everything downstream (per-part instance buffers, the
    /// group-0 camera+fog write, the `Frustum` cull) is deliberately identical,
    /// because a chest that fogged or lit differently from the mobs standing next
    /// to it would be the more visible bug.
    ///
    /// Light arrives already sampled on each [`lodestone_render::ChestSpawn`]
    /// rather than being read through [`Self::entity_light`] here: the gather
    /// already holds the world open to find the chest at all, and sampling there
    /// costs one lock instead of one per chest.
    /// Every `minecraft:special` item on a **3-D world surface** this frame, as
    /// block-entity instances ready to join `prepare_block_entities`' batch list.
    ///
    /// Three surfaces, all consumers of a finished seam rather than new machinery:
    ///
    /// | surface | item comes from | display context | pose |
    /// |---|---|---|---|
    /// | dropped stack | `EntityDraw::item` | `Ground` | `dropped_item_matrix` — the same bob and spin a pickaxe gets |
    /// | another entity's hand | `EntityDraw::equipment` | `thirdperson_{left,right}hand` | `held_item_matrix` off the holder's own arm |
    /// | item frame | `EntityDraw::item` | `Fixed` | `framed_item_matrix` |
    ///
    /// # One resolver, not three
    ///
    /// `resolve_special` finds the form, `special_item_rig` maps `kind` + item path
    /// to `(rig, sheet)`, and `BlockEntityModelSet::resolve_special_item` builds the
    /// instance. Nothing here knows that a chest is `chest_single` or that a
    /// trapped chest differs only by sheet — that is deliberately the one place
    /// that knows, shared with the first-person hand and the inventory slot. If you
    /// find yourself matching on an item id in this function, that is the defect.
    ///
    /// # The baked path is tried first, by construction
    ///
    /// This runs *alongside* `prepare_item_geometry`, not instead of it, and the two
    /// are disjoint because `ItemVariants::resolve` and `resolve_special` are: an
    /// item with bakeable geometry resolves in the first and answers `None` in the
    /// second. No 26.2 item has both, so nothing is double-drawn; a resource pack
    /// that made one would draw it twice rather than not at all, which is the
    /// failure that is at least visible.
    ///
    /// # Item frames are the surface with a real shortfall
    ///
    /// Only the *special* items are covered. An ordinary item in an item frame still
    /// draws nothing (only a filled map does, through `prepare_framed_maps`), and the
    /// in-frame `rotation` is undecoded so every framed item hangs upright — see
    /// `framed_item_matrix`. So a chest in a frame now draws and a sword in a frame
    /// still does not, which is stated rather than hidden.
    fn special_item_instances(
        &self,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<lodestone_render::BlockEntityInstance> {
        // No baked item pipeline means no item definitions to resolve a special
        // form out of, so there is nothing to draw — the same fail-open every
        // sheet-less path here takes.
        let Some(model) = self.model.as_ref() else {
            return Vec::new();
        };
        let frustum = camera.frustum();
        let mut out = Vec::new();

        for draw in entities {
            // Cull on the entity before any pose work, with two blocks of slack —
            // covers a tall holder plus the item's own reach, exactly as
            // `merge_held_items` does for the baked case.
            if !frustum.intersects_aabb(
                draw.feet - glam::Vec3::new(1.0, 0.5, 1.0),
                draw.feet + glam::Vec3::new(1.0, 2.5, 1.0),
            ) {
                continue;
            }
            let light = entity_light(&self.entity_light, draw);

            if draw.type_path == crate::entities::ITEM_ENTITY_TYPE_PATH {
                if let Some(instance) = self.dropped_special_item(model, draw, light) {
                    out.push(instance);
                    stats.special_item_drops_drawn += 1;
                }
                // A dropped item carries no equipment; skip the hand scan.
                continue;
            }
            if super::maps::ITEM_FRAME_TYPES.contains(&draw.type_path.as_str()) {
                if let Some(instance) = self.framed_special_item(model, draw, light) {
                    out.push(instance);
                    stats.special_item_frames_drawn += 1;
                }
                continue;
            }
            for (slot, id) in &draw.equipment {
                // `Mob.getMainArm()` is `RIGHT` for every mob, so main hand is the
                // right arm and off hand the left — the same mapping
                // `merge_held_items` applies, and the only two slots that hold an
                // *item model*. Armour goes through `prepare_armour`.
                let arm = match slot {
                    EquipmentSlot::MainHand => Arm::Right,
                    EquipmentSlot::OffHand => Arm::Left,
                    _ => continue,
                };
                if let Some(instance) = self.held_special_item(model, draw, arm, id, light) {
                    out.push(instance);
                    stats.special_item_hands_drawn += 1;
                }
            }
        }
        out
    }

    /// A dropped `minecraft:special` stack, posed by the **same** bob-and-spin
    /// chain the baked path uses.
    ///
    /// `dropped_item_matrix`, not a lookalike: a dropped chest and a dropped
    /// pickaxe have to rise and turn together or the difference reads as a bug in
    /// whichever one you are not looking at. The one input a rig cannot supply the
    /// way a quad list can is the hover lift, which comes from the rig's own AABB
    /// through `special_item_hover_lift`.
    ///
    /// **No stack multiplication.** Vanilla's `submitMultipleFromCount` draws up to
    /// five jittered copies of a stack, and the baked path does; a stack of chests
    /// draws as one chest here. That is a bounded shortfall rather than an
    /// oversight: the copies need the posed model's own `z` depth to pick between
    /// the jitter and the fan, which is the quad-list measurement this path does not
    /// have — and a wrong branch there would fan a chest along `z` like a sprite.
    fn dropped_special_item(
        &self,
        model: &ModelRenderer,
        draw: &EntityDraw,
        light: u8,
    ) -> Option<lodestone_render::BlockEntityInstance> {
        let item = draw.item.as_ref()?;
        // `DisplaySlot::Ground` — `ItemEntityRenderer.extractRenderState` resolves a
        // drop in `ItemDisplayContext.GROUND`, and the transform below is read from
        // the same slot so the variant and the pose cannot disagree.
        let ctx = ItemStateContext::new(DisplaySlot::Ground);
        let form = model.items.get(item)?.resolve_special(&ctx)?;
        let (rig, _) = lodestone_render::special_item_rig(&form.kind, item.path())?;
        let mesh = self.block_entities.models.get(rig)?;
        // `ground_transform` prefers the model's **declared** `ground`, and all four
        // ported `kind`s declare one (`item/template_chest`,
        // `item/template_shulker_box` and `item/template_skull` all carry a `ground`
        // slot), so the `GuiLight` fallback is unreachable for anything that resolves
        // today. `Side` is still the right value to pass: these are block-shaped
        // rigs, and the fallback for the block family is `BLOCK_ITEM_GROUND`'s
        // quarter scale — `Front`'s half scale would draw a datapack chest that
        // omitted the slot at twice the size.
        let ground = ground_transform(&form.display, lodestone_assets::GuiLight::Side);
        let lift = lodestone_render::special_item_hover_lift(mesh.local_min, mesh.local_max, &ground);
        let placement = lodestone_render::entity::dropped_item_matrix(
            draw.feet,
            draw.anim.age_ticks,
            lodestone_render::entity::item_bob_offset(draw.id),
            &ground,
            lift,
        );
        self.block_entities
            .models
            .resolve_special_item(&form.kind, item.path(), placement, light)
    }

    /// A `minecraft:special` item in another entity's hand, posed off that
    /// entity's own arm.
    ///
    /// The arm matrix comes from the *same* `EntityModelSet::resolve` and the same
    /// `AnimInput` `prepare_entities` draws the holder with, so a held chest can
    /// never hang off a pose the player is not seeing. `hand_transform` first, with
    /// the structural `part_transforms` fallback, is the pair `merge_held_items`
    /// already established — five corpus models shift the item's pivot relative to
    /// the arm and that shift must not move the arm's visible mesh.
    ///
    /// A rig with no arm (a creeper handed a chest by a plugin) resolves nothing and
    /// draws nothing, which is vanilla's behaviour too: `ItemInHandLayer` is only
    /// attached to renderers whose model implements `ArmedModel`.
    fn held_special_item(
        &self,
        model: &ModelRenderer,
        draw: &EntityDraw,
        arm: Arm,
        item: &lodestone_assets::ResourceLocation,
        light: u8,
    ) -> Option<lodestone_render::BlockEntityInstance> {
        // `arm.display_slot(false)` — the third-person hand slot, and the *same*
        // expression `hand_transform` below reads its transform from.
        let ctx = ItemStateContext::new(arm.display_slot(false));
        let form = model.items.get(item)?.resolve_special(&ctx)?;
        let instance = self.entities.models.resolve(
            draw.model_type_path(),
            draw.feet,
            draw.yaw,
            draw.scale,
            &draw.anim,
        )?;
        let wearer = self.entities.models.get(instance.model)?;
        let arm_transform = instance.hand_transform(arm).or_else(|| {
            let part = wearer.skeleton.index_of(arm.part_name())?;
            instance.part_transforms.get(part).copied()
        })?;
        // `net::entity_snapshot` maps `baby` onto a 0.5 uniform scale, the only baby
        // signal that reaches this layer — the same test `merge_held_items` uses.
        let baby = draw.scale < 1.0;
        let transform = hand_transform(&form.display, arm, false);
        let placement =
            lodestone_render::entity::held_item_matrix(arm_transform, arm, baby, &transform);
        self.block_entities
            .models
            .resolve_special_item(&form.kind, item.path(), placement, light)
    }

    /// A `minecraft:special` item hanging in an item frame.
    ///
    /// `DisplaySlot::Fixed`, which is `ItemFrameRenderer.extractRenderState`'s
    /// `ItemDisplayContext.FIXED` — the same context the campfire path uses and the
    /// single easiest thing to get wrong here, because every *other* world item
    /// surface is `Ground`. Reusing `Ground` poses a framed chest on its edge.
    ///
    /// A **glow** item frame's `getBlockLightLevel` floor of 5 is not applied: the
    /// wire distinguishes the two entity types, but the light source here is the
    /// shared eye probe and giving one type a floor belongs with the frame's own
    /// renderer rather than in this one branch. A framed chest in a glow frame is
    /// therefore as dark as the wall behind it.
    fn framed_special_item(
        &self,
        model: &ModelRenderer,
        draw: &EntityDraw,
        light: u8,
    ) -> Option<lodestone_render::BlockEntityInstance> {
        let item = draw.item.as_ref()?;
        let ctx = ItemStateContext::new(DisplaySlot::Fixed);
        let form = model.items.get(item)?.resolve_special(&ctx)?;
        let placement = lodestone_render::framed_item_matrix(
            draw.feet,
            draw.yaw,
            draw.pitch,
            &form.display.get(DisplaySlot::Fixed),
        );
        self.block_entities
            .models
            .resolve_special_item(&form.kind, item.path(), placement, light)
    }

    /// # The `entities` slice is here for the `minecraft:special` items
    ///
    /// Three of the five surfaces vanilla draws a chest/shulker/skull *item* on
    /// are ordinary entities — a dropped stack, an item in another entity's hand,
    /// and an item frame — and all three draw through the block-entity rig, so
    /// they join this pass rather than `prepare_item_geometry`'s baked-quad one.
    /// See [`Self::special_item_instances`].
    pub(super) fn prepare_block_entities(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> (Vec<BlockEntityDrawBatch>, Vec<BannerLayerDrawBatch>) {
        // Always reported, even on an empty frame: this is what separates "no
        // chests in view" from "no pack, so nothing can ever draw" — a chest with
        // no sheet draws nothing rather than a placeholder.
        stats.block_entity_sheets_loaded = self.block_entities.sheet_count();

        let eye = camera.position;
        let chests = self.block_entity_source.chests(eye);
        let skulls = self.skull_source.skulls(eye);
        let bells = self.bell_source.bells(eye);
        let shulkers = self.shulker_source.shulkers(eye);
        let banners = self.banner_source.banners(eye);
        let lecterns = self.lectern_source.lecterns(eye);
        let enchanting_tables = self.enchanting_table_source.enchanting_tables(eye);
        let decorated_pots = self.decorated_pot_source.decorated_pots(eye);
        // All eight, not any subset: an early return on only `chests`/`skulls`
        // would make a bell in an otherwise chestless, skull-less room draw
        // nothing, which is exactly how this pass would have grown a third
        // island — a shulker box in an empty end-city room is the fourth
        // instance of the same shape, a banner in a village the fifth, a
        // lectern in an otherwise bare village library the sixth, an
        // enchanting table alone in a room the seventh, and a decorated pot
        // alone the eighth. Every source added here has to join this
        // condition.
        //
        // **`CampfireSource` is the one exception, and it is not a subset
        // oversight**: a campfire's renderer contributes no cuboid instance at
        // all (it draws item models through `prepare_item_geometry`), so adding
        // it here would make this condition read as satisfied while this pass
        // still had nothing to draw.
        //
        // The special-item instances **must** join this condition too, and for
        // exactly the reason the comment above gives: a chest lying on the floor of
        // an otherwise empty room is the eighth instance of the same shape. They
        // are resolved before the early return rather than after it, because a
        // `Vec` this pass has not built yet cannot be tested for emptiness.
        let specials = self.special_item_instances(camera, entities, stats);
        if chests.is_empty()
            && skulls.is_empty()
            && bells.is_empty()
            && shulkers.is_empty()
            && banners.is_empty()
            && lecterns.is_empty()
            && enchanting_tables.is_empty()
            && decorated_pots.is_empty()
            && specials.is_empty()
        {
            return (Vec::new(), Vec::new());
        }

        // Same group-0 contents as the entity pass, written to this pass's own
        // buffer: view-projection (world position is per-instance, so the section
        // origin stays zero), this frame's fog, and this frame's sky darkening.
        queue.write_buffer(
            &self.block_entities.cam_buffer,
            0,
            bytemuck::bytes_of(
                &EntityCameraUniform {
                    camera: CameraUniform {
                        view_proj: self.world_view_projection(camera).to_cols_array_2d(),
                        section_origin: [0.0, 0.0, 0.0, 0.0],
                    },
                    fog: self.fog_with_clock(eye),
                }
                .with_sky_darken(self.sky_darken.value()),
            ),
        );

        // The `minecraft:special` items first, so the list is never empty when only
        // they are on screen. They batch by `(model, sheet)` alongside the placed
        // blocks and coalesce with them for free: a held chest and a placed chest
        // are the same mesh and the same sheet, so one batch draws both.
        let mut instances: Vec<_> = specials;
        instances.extend(
            chests
                .iter()
                .filter_map(|spawn| self.block_entities.models.resolve_chest(spawn)),
        );
        // Appended into the same list rather than planned separately: a chest and
        // a skull batch independently inside one `plan_block_entities` call, so
        // frustum culling and the batch split are shared for free.
        instances.extend(
            skulls
                .iter()
                .filter_map(|spawn| self.block_entities.models.resolve_skull(spawn)),
        );
        instances.extend(
            bells
                .iter()
                .filter_map(|spawn| self.block_entities.models.resolve_bell(spawn)),
        );
        // Shulker boxes batch by `(model, texture)` like everything else here, and
        // the seventeen dye sheets mean up to seventeen batches — one per colour
        // actually in view, which is what the batcher is for.
        instances.extend(
            shulkers
                .iter()
                .filter_map(|spawn| self.block_entities.models.resolve_shulker(spawn)),
        );
        // One model and one sheet for every lectern in the world, so all of them
        // coalesce into a single batch regardless of facing — the facing rides
        // the per-instance placement matrix.
        instances.extend(
            lecterns
                .iter()
                .filter_map(|spawn| self.block_entities.models.resolve_lectern(spawn)),
        );
        // Enchanting-table books share the lectern's mesh *and* its sheet, so they
        // coalesce into the **same** batch as the lecterns above rather than a
        // seventh one — the only pair here that does. Everything that differs
        // between the two (the 80-degree tilt, the hover, the live openness and the
        // page flips) rides the per-instance matrices, which is exactly why
        // `resolve_enchanting_table` had to stay a separate function: sharing a
        // batch is not sharing a pose.
        instances.extend(
            enchanting_tables
                .iter()
                .filter_map(|spawn| self.block_entities.models.resolve_enchanting_table(spawn)),
        );

        // Decorated pots. `resolve_decorated_pot` returns five ordinary opaque
        // instances at once (base + four sides) rather than one — unlike
        // banner's `layers`, none of the five need a second, unbatched draw
        // pass: each carries its own `(model, texture)` pair and rejoins this
        // same batcher, so `.flatten()` is all that is needed to fold them in.
        instances.extend(
            decorated_pots
                .iter()
                .filter_map(|spawn| self.block_entities.models.resolve_decorated_pot(spawn))
                .flatten(),
        );

        // Banners. `resolve_banner` returns three things at once: the
        // pole/bar `body` and the swaying `flag` are ordinary opaque instances that
        // join the batch above, while `layers` is an **ordered** list of masks that
        // cannot be batched at all — see [`BannerLayerDrawBatch`].
        let mut banner_layers = Vec::new();
        for resolved in banners
            .iter()
            .filter_map(|spawn| self.block_entities.models.resolve_banner(spawn))
        {
            for layer in &resolved.layers {
                // The mask's bare asset id — `PatternLayer::sprite` is
                // `minecraft:entity/banner/<id>`, and `banner_patterns` keys on
                // `<id>` alone. Passing the full location through resolves nothing
                // and the layer silently disappears.
                let Some(pattern) = layer.sprite.path().rsplit('/').next() else {
                    continue;
                };
                if !self.block_entities.banner_patterns.contains_key(pattern) {
                    continue;
                }
                // Gamma-space `0.0..=1.0` from the compositor, quantised to the
                // gamma-space bytes `InstanceTint` carries. Doing this in linear
                // would wash every dye toward white — vanilla is not
                // colour-managed, and the shader multiplies in gamma too.
                let rgb = layer.color.map(|c| {
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "clamped into 0..=255 first"
                    )]
                    {
                        (c.clamp(0.0, 1.0) * 255.0).round() as u8
                    }
                });
                let Some(buffer) = upload_instances_tinted(
                    device,
                    &[layer.transform],
                    &[u32::from(layer.light)],
                    &[InstanceTint::rgb(rgb)],
                ) else {
                    continue;
                };
                banner_layers.push(BannerLayerDrawBatch {
                    pattern: pattern.to_string(),
                    instances: buffer,
                });
            }
            instances.push(resolved.body);
            instances.push(resolved.flag);
        }
        stats.banner_layers_drawn = banner_layers.len();

        let frame = plan_block_entities(&instances, &camera.frustum());
        stats.block_entities_drawn = frame.stats.drawn;
        stats.block_entities_culled = frame.stats.culled_frustum;

        let opaque = frame
            .batches
            .iter()
            .map(|batch| BlockEntityDrawBatch {
                model: batch.model,
                texture: batch.texture,
                count: batch.count(),
                // One buffer per part, for the reason `prepare_entities` gives:
                // vertices are part-local, so the lid only moves if its own
                // matrices are uploaded separately from the bottom's.
                //
                // `_tinted`, not the plain `upload_instances`: block entities
                // carry a per-instance `InstanceTint`
                // (`lodestone_render::block_entity::BlockEntityBatch::tints`),
                // the same plumbing sheep wool/dyed armour/the hurt overlay
                // already use.
                //
                // **Every resolver here still passes white, banner included, and
                // that is correct rather than pending.** A banner's base colour is
                // not a tint on its cloth: vanilla draws `banner_base` untinted and
                // puts the dye on *layer 0 of the mask list*, which is why it rides
                // `BannerLayerDrawBatch` and not this field. The tint hook remains
                // unused, and the next genuinely tinted block-entity type is the
                // one that will use it.
                parts: batch
                    .parts
                    .iter()
                    .map(|p| upload_instances_tinted(device, p, &batch.lights, &batch.tints))
                    .collect(),
            })
            .collect();
        (opaque, banner_layers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The caller was the bug, so the caller gets the assertion.**
    ///
    /// `flame_quads` was always faithful; `EntityRenderer::new` fed it
    /// model-*type* constants and baked one mesh per type, so a baby zombie drew
    /// an adult's flame. The fix is per-instance, and this is the function that
    /// makes it per instance: it must multiply the type's base width by the
    /// entity's own age scale.
    ///
    /// Predicted from the generated dimensions table (index 151, zombie
    /// `(0.6, 1.95)`) rather than asserted as "smaller", and the neutral case is
    /// asserted alongside so a version that scaled *everything* by `0.5` cannot
    /// pass either.
    #[test]
    fn a_babys_flame_reads_its_own_halved_hitbox_and_an_adults_is_untouched() {
        let adult = flame_hitbox_width("zombie", 1.0).expect("zombie has dimensions");
        let baby = flame_hitbox_width("zombie", 0.5).expect("zombie has dimensions");
        assert!((adult - 0.6).abs() < 1e-6, "adult zombie width, got {adult}");
        assert!((baby - 0.3).abs() < 1e-6, "baby zombie width, got {baby}");
        assert!(
            (baby - adult * 0.5).abs() < 1e-6,
            "the age scale must reach the width"
        );

        // A different aspect ratio, so this is not pinned to one hitbox.
        let spider = flame_hitbox_width("spider", 1.0).expect("spider has dimensions");
        assert!((spider - 1.4).abs() < 1e-6, "spider width, got {spider}");

        // Degenerate inputs: never a zero width, which would collapse the flame
        // to a point rather than declining to draw one.
        assert!(flame_hitbox_width("zombie", 0.0).is_none());
        assert!(flame_hitbox_width("not_a_real_entity_type", 1.0).is_none());
    }

    /// The test column's packed light, `sky << 4 | block`, from two rules stated
    /// once and evaluated by the assertions rather than restated as constants.
    ///
    /// * **Block light** floods cell `y = 60` at 15 from a lava pool and falls
    ///   off 1 per block as you rise: `15 - (y - 60)`, clamped at 0.
    /// * **Sky light** climbs 2 per block from a shaft mouth at `y = 62`:
    ///   `2 * (y - 62)`, clamped into `0..=15`.
    ///
    /// The two gradients have different slopes *and* different origins on
    /// purpose: a single shared gradient would make several wrong probe cells
    /// produce the same byte, and a same-slope pair could not tell a swapped
    /// nibble from a shifted cell.
    fn column_light(y: i32) -> u8 {
        let block = (15 - (y - 60)).clamp(0, 15);
        let sky = (2 * (y - 62)).clamp(0, 15);
        u8::try_from((sky << 4) | block).expect("both nibbles are 0..=15")
    }

    fn gradient_source() -> super::super::EntityLightSource {
        super::super::EntityLightSource(Some(Box::new(|p: glam::Vec3| {
            // `BlockPos.containing` floors; `f32::floor` then `as i32` is that.
            #[expect(
                clippy::cast_possible_truncation,
                reason = "test positions are small integers plus an eye height"
            )]
            Some(column_light(p.y.floor() as i32))
        })))
    }

    fn subject(type_path: &str, feet_y: f32, scale: f32, on_fire: bool) -> EntityDraw {
        EntityDraw {
            id: 1,
            type_path: type_path.to_owned(),
            item: None,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_trim: Vec::new(),
            wool: None,
            block_state: None,
            count: 1,
            foil: false,
            feet: glam::Vec3::new(0.5, feet_y, 0.5),
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale,
            anim: lodestone_render::AnimInput::REST,
            name_tag: None,
            hurt: false,
            item_use: None,
            creeper_swelling: 0.0,
            swim_amount: 0.0,
            death_time: 0.0,
            on_fire,
            player_skin: None,
            variant_sheet: None,
            // A flame subject, not an orb.
            experience_orb_value: None,
        }
    }

    /// The eye heights are the jar's, not a formula, and the table is searchable.
    ///
    /// Every value comes from an `EntityType.Builder.eyeHeight` call in 26.2's
    /// `EntityTypes` — an outside record definition, read as a record — and the
    /// spot checks below are the ones that would move if the table were
    /// regenerated with the wrong column or shifted by a row.
    #[test]
    fn eye_heights_are_the_jars_own_and_the_default_is_the_only_fallback() {
        // Sorted and unique, or `binary_search_by` silently misses rows.
        for pair in EYE_HEIGHTS.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "EYE_HEIGHTS must be strictly sorted by key: {:?} then {:?}",
                pair[0].0,
                pair[1].0
            );
        }
        // Every key is a real 26.2 entity type, per the generated registry — a
        // typo'd row would otherwise sit in the table forever, unreachable and
        // silently defaulting.
        for (name, _) in EYE_HEIGHTS {
            assert!(
                lodestone_data::entity_types::entity_type_id_parts("minecraft", name).is_some(),
                "{name} is not a registered entity type"
            );
        }

        // Overridden: the value is the jar's, and it is *not* `height * 0.85`.
        assert!((eye_probe_offset("zombie", 1.0) - 1.74).abs() < 1e-6);
        assert!((eye_probe_offset("player", 1.0) - 1.62).abs() < 1e-6);
        assert!((eye_probe_offset("ghast", 1.0) - 2.6).abs() < 1e-6);
        // A zombie is 1.95 tall and a ghast 4.0, so the default formula would
        // give 1.6575 and 3.4. Assert the distance so a regression to the
        // formula cannot pass by rounding.
        assert!((eye_probe_offset("ghast", 1.0) - 4.0 * 0.85).abs() > 0.7);

        // Not overridden: `EntityDimensions.defaultEyeHeight`, off the generated
        // dimensions table. A creeper is 1.7 tall.
        let creeper = eye_probe_offset("creeper", 1.0);
        assert!(
            (creeper - 1.7 * 0.85).abs() < 1e-6,
            "creeper takes the 0.85 default, got {creeper}"
        );

        // Age scale reaches the offset, as `EntityDimensions.scale` does.
        assert!((eye_probe_offset("zombie", 0.5) - 0.87).abs() < 1e-6);

        // An unknown type probes at the feet rather than at an invented height.
        assert_eq!(eye_probe_offset("not_a_real_entity_type", 1.0), 0.0);
    }

    /// **The probe is the eye cell, and fire forces only the block nibble.**
    ///
    /// Both halves of `EntityRenderer.getPackedLightCoords` at once, on inputs
    /// chosen so the right answer and the two wrong ones are three different
    /// bytes. Before this, every pass in this module and in
    /// [`super::super::world_items`] sampled at `draw.feet`.
    ///
    /// The subject is **tall** deliberately. On flat, uniformly-lit ground — or
    /// for a short entity whose eye shares its feet's cell — feet-probing and
    /// eye-probing return the same byte, so such an input measures only that the
    /// code runs. The last case below is exactly that coincidence, asserted as a
    /// coincidence.
    #[test]
    fn an_entity_probes_its_eye_cell_and_fire_forces_only_the_block_nibble() {
        let source = gradient_source();

        // The column, so the assertions below can be read against it:
        //   y=64 -> block 11, sky 4  -> 75
        //   y=65 -> block 10, sky 6  -> 106
        //   y=66 -> block  9, sky 8  -> 137
        //   y=67 -> block  8, sky 10 -> 168
        assert_eq!(
            [
                column_light(64),
                column_light(65),
                column_light(66),
                column_light(67)
            ],
            [75, 106, 137, 168],
            "the gradient itself moved; every prediction below is derived from it"
        );

        // A zombie standing on the block top at y=64. Eye 1.74 -> 65.74 -> cell
        // 65. The feet hypothesis reads cell 64.
        let zombie = entity_light(&source, &subject("zombie", 64.0, 1.0, false));
        assert_eq!(
            zombie,
            column_light(65),
            "a zombie at y=64 must read its eye cell 65 ({}), not its feet cell 64 ({})",
            column_light(65),
            column_light(64)
        );

        // A ghast, which separates all *three* hypotheses: real eye 2.6 -> cell
        // 66, the `height * 0.85` default 3.4 -> cell 67, the feet -> cell 64.
        let ghast = entity_light(&source, &subject("ghast", 64.0, 1.0, false));
        assert_eq!(
            ghast,
            column_light(66),
            "a ghast at y=64 must read cell 66 ({}); the 0.85 default would read \
             cell 67 ({}) and feet-probing cell 64 ({})",
            column_light(66),
            column_light(67),
            column_light(64)
        );

        // Fire: the block nibble becomes 15, the sky nibble does not move. Cell
        // 65 is block 10, sky 6 — so 15 is a *change* here, which is what makes
        // this input discriminating at all.
        let burning = entity_light(&source, &subject("zombie", 64.0, 1.0, true));
        assert_eq!(
            burning & 0x0F,
            15,
            "fire forces block light to 15 (sampled cell has {})",
            column_light(65) & 0x0F
        );
        assert_eq!(
            burning >> 4,
            column_light(65) >> 4,
            "fire must not touch the sky nibble — `getSkyLightLevel` has no \
             `isOnFire` branch"
        );
        // Spelled out as one byte too, so neither of the two nibble assertions
        // can drift apart from the value the pass actually uploads. 111 is
        // sky 6 << 4 | 15; forcing the whole byte would be 255, forcing sky
        // alone 250, and fire-plus-feet-probing 79.
        assert_eq!(burning, 111);

        // **Not a discriminating input, and that is the point.** A baby zombie's
        // eye is 0.87 and a dropped item's 0.2125, so both share their feet's
        // cell: for these, feet-probing and eye-probing agree, and a gate built
        // on either would have passed against the bug.
        assert_eq!(
            entity_light(&source, &subject("zombie", 64.0, 0.5, false)),
            column_light(64)
        );
        assert_eq!(
            entity_light(&source, &subject("item", 64.0, 1.0, false)),
            column_light(64)
        );

        // A non-integer feet height moves the boundary, which is the case the
        // three-cells-coincide reasoning in `EYE_HEIGHTS`' doc does not cover: a
        // zombie on a slab at y=64.5 has its eye at 66.24, one cell higher.
        assert_eq!(
            entity_light(&source, &subject("zombie", 64.5, 1.0, false)),
            column_light(66)
        );
    }

    /// Issue #573's own discriminating assertion: `swim_amount = 0.0` must be
    /// the exact untouched orientation, `1.0` must rotate the body
    /// substantially, and `0.5` must sit **strictly between** the two —
    /// which a boolean/threshold implementation (snapping at some cutoff)
    /// cannot do, since it can only ever agree with one endpoint or the
    /// other.
    #[test]
    fn swim_rotation_interpolates_and_does_not_snap_at_a_threshold() {
        let feet = glam::Vec3::new(4.0, 70.0, -2.0);
        let yaw = 0.0;
        let pitch = 0.0;
        let base = lodestone_render::dying_entity_model_matrix(feet, yaw, 1.0, 0.0);

        let orientation_at = |swim_amount: f32| {
            let mut instance = lodestone_render::EntityInstance {
                model: "player_wide",
                transform: base,
                part_transforms: vec![base],
                hand_transforms: [None, None],
                aabb_min: feet,
                aabb_max: feet + glam::Vec3::ONE,
                light: 0,
            };
            apply_swim_rotation(&mut instance, feet, yaw, 0.0, pitch, swim_amount);
            (
                instance.transform.transform_vector3(glam::Vec3::Y),
                instance.part_transforms[0],
            )
        };

        let (at0, part0) = orientation_at(0.0);
        let (at_half, _) = orientation_at(0.5);
        let (at1, part1) = orientation_at(1.0);

        // `swim_amount <= 0.0` must be a hard no-op — not "rotate by zero
        // degrees", which a floating-point conjugation could round away from
        // bit-identical.
        assert_eq!(
            at0,
            base.transform_vector3(glam::Vec3::Y),
            "swim_amount=0.0 must leave the transform untouched"
        );

        let full_swing = (at1 - at0).length();
        assert!(
            full_swing > 0.5,
            "swim_amount=1.0 did not rotate the body toward horizontal at all: \
             at0={at0}, at1={at1} (delta {full_swing})"
        );

        // The discriminating input: a boolean implementation puts `at_half`
        // exactly on one of the two endpoints (`d0` or `d1` would be ~0); a
        // real linear ramp puts it strictly between both.
        let d0 = (at_half - at0).length();
        let d1 = (at_half - at1).length();
        assert!(
            d0 > 0.1 && d1 > 0.1,
            "swim_amount=0.5 must sit strictly between the two endpoints, not \
             snap to either (d0={d0}, d1={d1}, full_swing={full_swing})"
        );

        // Every part transform must move with the body, not just the root —
        // a fix that only rotated `instance.transform` and left
        // `part_transforms` alone would still make the mob upright and this
        // catches it.
        assert_ne!(
            part0, part1,
            "part_transforms[0] did not rotate with the body"
        );
    }
}
