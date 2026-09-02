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
use std::sync::Arc;

use lodestone_assets::DisplaySlot;
use lodestone_assets::entity_models::sheep_wool_tint;
use lodestone_assets::equipment::ArmourSlot;
use lodestone_model::event::EquipmentSlot;
use lodestone_render::{
    Camera, CameraUniform, EntityCameraUniform, InstanceTint, ItemStateContext,
    entity::{Arm, armour_layer_tint_with_dye, armour_layers, ground_transform, hand_transform},
    plan_block_entities, plan_entities, stage_instances_tinted,
};

use crate::entities::EntityDraw;

use super::block_entities::{BannerLayerDrawBatch, BlockEntityDrawBatch};
use super::pack_trace::{should_trace_candidate, unit_quad_normal, unit_quad_plane};
use super::terrain::ModelRenderer;
use super::{
    ArmourAccum, ArmourDrawBatch, ArmourPartAccum, ArmourTextureKey, CapeDrawBatch, ElytraDrawBatch,
    EntityDrawBatch, EntitySpriteBatch, FlameBatch, OrbBatch, PaintingDrawBatch,
    PreparedEntityBatches, RenderState, RenderStats, ShadowBatch, WoolPartAccum,
    humanoid_armour_slot,
};

/// Select the texture for one third-person held special item.
///
/// A special rig normally owns a static sheet, but a player-head stack's
/// `minecraft:profile` replaces only that sheet. The producer emits this
/// channel only for the underlying `minecraft:player_head` item, matching
/// 26.2's `PlayerHeadSpecialRenderer.extractArgument`; its `item_model`
/// component may retarget the definition but does not change the profile's
/// owner. The URL remains slot-scoped because an entity can hold two distinct
/// custom heads.
fn held_special_texture(
    draw: &EntityDraw,
    slot: EquipmentSlot,
    fallback: lodestone_render::BlockEntityTexture,
) -> lodestone_render::BlockEntityTexture {
    if let Some((_, url)) = draw.equipment_skin.iter().find(|(candidate, _)| *candidate == slot)
    {
        lodestone_render::BlockEntityTexture::PlayerSkin(Arc::clone(url))
    } else {
        fallback
    }
}

/// Select a dropped special item's texture.
///
/// Only a player head has a dynamic sheet. Its profile crossed the
/// `DisplayItem` → [`EntityDraw`] boundary separately from the compact item
/// id, exactly as an equipped head does for [`held_special_texture`].
fn dropped_special_texture(
    draw: &EntityDraw,
    fallback: lodestone_render::BlockEntityTexture,
) -> lodestone_render::BlockEntityTexture {
    draw.item_skin
        .as_ref()
        .map_or(fallback, |url| lodestone_render::BlockEntityTexture::PlayerSkin(Arc::clone(url)))
}

/// Vanilla's `CustomHeadLayer` scale for a raw humanoid skull.
const WORN_PLAYER_HEAD_SCALE: f32 = 1.1875;

/// The only head-slot item rendered by the special-item layer in this client.
fn worn_player_head_item(draw: &EntityDraw) -> Option<&lodestone_assets::ResourceLocation> {
    draw.equipment.iter().find_map(|(slot, item)| {
        (*slot == EquipmentSlot::Head
            && item.namespace() == "minecraft"
            && item.path() == "player_head")
            .then_some(item)
    })
}

/// Apply `CustomHeadLayer`'s raw-skull scale after the animated head pose.
fn worn_player_head_placement(head_transform: glam::Mat4) -> glam::Mat4 {
    head_transform * glam::Mat4::from_scale(glam::Vec3::splat(WORN_PLAYER_HEAD_SCALE))
}

/// Record an entity type whose ordinary body dispatch had no baked model.
///
/// F3+B draws hitboxes independently of the model pass, so this is the useful
/// diagnostic for the otherwise confusing "hitbox but no entity" symptom. The
/// body pass deliberately skips types owned by specialised passes (items,
/// displays, frames, sprites, paintings and moving blocks); those are filtered
/// before this branch, while an unexpected mob type identifies a concrete
/// model/mapping gap for the next renderer addition.
fn ordinary_model_dispatch_is_unhandled(type_path: &str) -> bool {
    match type_path {
        // Dedicated item/display/block passes. These all intentionally decline
        // the ordinary entity-model resolver, so reporting them would turn a
        // healthy dispatch split into a false positive.
        "item"
        | "item_frame"
        | "glow_item_frame"
        | "text_display"
        | "item_display"
        | "block_display"
        | "experience_orb"
        | "painting"
        | "falling_block"
        | "tnt"
        | "firework_rocket"
        | "ominous_item_spawner"
        | "interaction"
        | "marker" => false,
        // Thrown-item billboards and the two camera-facing entity sprites have
        // their own geometry paths rather than a cuboid body model.
        path if lodestone_render::entity::thrown_item_for(path).is_some() => false,
        path if lodestone_render::entity_sprite::entity_sprite_index_for(path).is_some() => false,
        _ => true,
    }
}

fn note_missing_entity_model(type_path: &str, model_path: &str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut seen = match seen.lock() {
        Ok(seen) => seen,
        Err(poisoned) => poisoned.into_inner(),
    };
    if ordinary_model_dispatch_is_unhandled(type_path) && seen.insert(type_path.to_owned()) {
        tracing::debug!(
            target: "entity",
            entity_type = type_path,
            model = model_path,
            "entity has a hitbox but no ordinary body model; it may be handled by a specialised pass or need a model mapping"
        );
    }
}

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
/// on-fire entity per frame, so the scan cost was real.
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

/// `AbstractBoatRenderer.submit`'s hull roll — the rocking a punched boat does
/// — applied on top of whatever
/// [`lodestone_render::non_living_vehicle_matrix`] already placed. A no-op
/// while the hurt clock is not running, which is every boat in the world almost
/// all of the time.
///
/// # Why this is here and not in the placement
///
/// Every other input to the boat's placement is a fact about *where* the entity
/// is; this one is a fact about what just happened to it, and it arrives on the
/// animation record rather than on the draw record. Applying it here is the same
/// seam [`apply_swim_rotation`] uses for the player's prone rotation, one
/// placement over.
///
/// # Composed by conjugation, not by re-deriving the placement
///
/// `non_living_vehicle_matrix` documents its own product as
/// `T(feet) · T(0, bob, 0) · Ry(180 − yaw) · S(−s, −s, s) · Ry(extra)`, and
/// vanilla inserts the roll **between** the yaw term and the flip
/// (`AbstractBoatRenderer.submit` does `translate`, `mulPose(YP, 180 − yRot)`,
/// then the hurt `mulPose(XP, …)`, and only then `scale(-1, -1, 1)` and the
/// trailing `mulPose(YP, 90)`). So with `A = T(feet) · T(0, bob, 0) ·
/// Ry(180 − yaw)`, left-multiplying every baked matrix by `A · Rx(roll) · A⁻¹`
/// reproduces `A · Rx(roll) · S · Ry(extra)` exactly, without decomposing the
/// baked matrices back into their factors. `A` is rebuilt here from the same
/// `feet`/`yaw`/`bob` the resolver was called with, so it is bit-identical to
/// the `A` already folded into `instance`.
///
/// Inserting the roll *after* the flip instead would rotate about the model's
/// own X axis in the flipped frame — the same angle with the wrong sign, which
/// tips the hull the way the hit came *from*.
///
/// # What is not ported
///
/// The bubble-column tilt (`state.bubbleAngle`) that vanilla applies right after
/// this one. `AbstractBoat.DATA_ID_BUBBLE_TIME` is not streamed by this
/// workspace's server and not decoded by its client, so there is no value to
/// apply — an absence, not an approximation.
fn apply_boat_rock(
    instance: &mut lodestone_render::EntityInstance,
    feet: glam::Vec3,
    yaw_deg: f32,
    vertical_offset: f32,
    hurt: lodestone_render::entity_anim::BoatHurt,
) {
    let roll_deg = lodestone_render::entity_anim::boat_hurt_roll_degrees(hurt);
    if roll_deg == 0.0 {
        return;
    }
    let pivot = feet + glam::Vec3::new(0.0, vertical_offset, 0.0);
    let a = glam::Mat4::from_translation(pivot)
        * glam::Mat4::from_rotation_y((180.0 - yaw_deg).to_radians());
    let extra = a * glam::Mat4::from_rotation_x(roll_deg.to_radians()) * a.inverse();

    instance.transform = extra * instance.transform;
    for part in &mut instance.part_transforms {
        *part = extra * *part;
    }
    for hand in &mut instance.hand_transforms {
        if let Some(h) = hand {
            *h = extra * *h;
        }
    }
    // Conservative AABB widen, exactly as `apply_swim_rotation` does: the
    // rotation is about `pivot`, so a sphere of the old maximum corner distance
    // from it bounds the result. It can only widen the box, never wrongly cull.
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
    .map(|corner| (corner - pivot).length())
    .fold(0.0f32, f32::max);
    instance.aabb_min = pivot - glam::Vec3::splat(radius);
    instance.aabb_max = pivot + glam::Vec3::splat(radius);
}

/// Hides an armour stand's arms and/or base plate, per its
/// `ArmorStand.DATA_CLIENT_FLAGS` byte — remaining half, once
/// the byte itself reaches `EntityDraw::armor_stand`.
///
/// Vanilla toggles `ModelPart.visible` directly
/// (`ArmorStandModel.setupAnim`: `leftArm.visible = state.showArms`,
/// `basePlate.visible = state.showBasePlate`). This renderer has no
/// per-part visibility flag — [`lodestone_render::EntityInstance`] carries
/// one *matrix* per part, not a flag — so "invisible" is expressed as "this
/// part's own matrix collapses every one of its vertices to a single point",
/// which draws zero-area geometry instead.
///
/// **Scale-to-zero, not zero the matrix outright.** Multiplying by a matrix
/// with a zero *scale* keeps the result a valid affine transform: every
/// vertex of the part maps to that part's own origin. A bare all-zero
/// matrix would zero the homogeneous `w` too, which is a divide-by-zero in
/// the perspective divide rather than a degenerate point.
///
/// Only `no_base_plate` and `show_arms` are handled: `small` is folded into
/// [`crate::entities`]'s scale resolution instead (see `EntityFacts`'s doc),
/// and `marker` has no renderer equivalent — see `EntityDraw::armor_stand`'s
/// doc for why.
fn hide_armor_stand_parts(
    instance: &mut lodestone_render::EntityInstance,
    wearer: &lodestone_render::EntityMesh,
    flags: lodestone_ecs::entity::ArmorStandFlags,
) {
    let mut hide = |part_name: &str| {
        if let Some(index) = wearer.skeleton.index_of(part_name)
            && let Some(part) = instance.part_transforms.get_mut(index)
        {
            *part *= glam::Mat4::from_scale(glam::Vec3::ZERO);
        }
    };
    if flags.no_base_plate {
        hide("base_plate");
    }
    if !flags.show_arms {
        hide("left_arm");
        hide("right_arm");
    }
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
/// (its own per-pose dimensions' eye-height accessor), and two things it varies by are
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

/// Every entity type's own `shadowRadius`, in blocks — vanilla's per-species
/// value, generated by `scripts/dump-entity-shadows.py` from the decompiled
/// client rather than transcribed. Read it with [`shadow_radius`].
///
/// **This used to be one flat `0.5` for every entity**, disclosed as a
/// simplification whose worst case was "a chicken casts a slightly oversized
/// shadow and a cow a slightly undersized one, never a missing or a wildly
/// wrong one". Both halves of that were wrong, which is why the owner saw
/// shadows that were simply too big:
///
/// * `EntityRenderer.shadowRadius`'s own field default is **`0.0F`** — no
///   shadow at all — and **35 of the 157 registered types take it**: every
///   arrow and thrown item, item frames, paintings, armour stands, shulkers,
///   `interaction`/`marker`, the whole projectile family. A flat `0.5` drew a
///   player-sized disc under all of them.
/// * The spread among the types that *do* cast one is 21×, not a nudge:
///   `0.14` (tadpole) to `3.0` (giant), with `0.15` for a dropped item or an
///   experience orb — the two things you see most often — against `1.5` for a
///   ghast and `2.0` for a happy ghast.
///
/// `0.5` was never a "common case" either. It is the modal value only because
/// the humanoids cluster there (34 of 157); the boats are `0.8` (21) and the
/// quadrupeds and minecarts `0.7` (25).
///
/// # How to change it
///
/// Re-run the script — do not edit a row by hand, and do not add one for a
/// modded or synthetic type. Its header carries the two traps that made the
/// first two generations of this table wrong (a generic `extends` bound read
/// as a superclass; a literal passed at the *registration* site rather than
/// in the renderer). A `Display` entity's radius is genuinely **synced
/// per-entity** rather than a renderer constant, so the three `*_display`
/// rows carry the accessor default and [`EntityDraw`] would need to carry the
/// real one before they can be right.
const SHADOW_RADII: &[(&str, f32)] = &[
    ("acacia_boat", 0.8),
    ("acacia_chest_boat", 0.8),
    ("allay", 0.4),
    ("area_effect_cloud", 0.0),
    ("armadillo", 0.4),
    ("armor_stand", 0.0),
    ("arrow", 0.0),
    ("axolotl", 0.5),
    ("bamboo_chest_raft", 0.8),
    ("bamboo_raft", 0.8),
    ("bat", 0.25),
    ("bee", 0.4),
    ("birch_boat", 0.8),
    ("birch_chest_boat", 0.8),
    ("blaze", 0.5),
    ("block_display", 0.0),
    ("bogged", 0.5),
    ("breeze", 0.5),
    ("breeze_wind_charge", 0.0),
    ("camel", 0.7),
    ("camel_husk", 0.7),
    ("cat", 0.4),
    ("cave_spider", 0.56),
    ("cherry_boat", 0.8),
    ("cherry_chest_boat", 0.8),
    ("chest_minecart", 0.7),
    ("chicken", 0.3),
    ("cod", 0.3),
    ("command_block_minecart", 0.7),
    ("copper_golem", 0.5),
    ("cow", 0.7),
    ("creaking", 0.6),
    ("creeper", 0.5),
    ("dark_oak_boat", 0.8),
    ("dark_oak_chest_boat", 0.8),
    ("dolphin", 0.7),
    ("donkey", 0.75),
    ("dragon_fireball", 0.0),
    ("drowned", 0.5),
    ("egg", 0.0),
    ("elder_guardian", 1.2),
    ("end_crystal", 0.5),
    ("ender_dragon", 0.5),
    ("ender_pearl", 0.0),
    ("enderman", 0.5),
    ("endermite", 0.3),
    ("evoker", 0.5),
    ("evoker_fangs", 0.0),
    ("experience_bottle", 0.0),
    ("experience_orb", 0.15),
    ("eye_of_ender", 0.0),
    ("falling_block", 0.5),
    ("fireball", 0.0),
    ("firework_rocket", 0.0),
    ("fishing_bobber", 0.0),
    ("fox", 0.4),
    ("frog", 0.3),
    ("furnace_minecart", 0.7),
    ("ghast", 1.5),
    ("giant", 3.0),
    ("glow_item_frame", 0.0),
    ("glow_squid", 0.7),
    ("goat", 0.7),
    ("guardian", 0.5),
    ("happy_ghast", 2.0),
    ("hoglin", 0.7),
    ("hopper_minecart", 0.7),
    ("horse", 0.75),
    ("husk", 0.5),
    ("illusioner", 0.5),
    ("interaction", 0.0),
    ("iron_golem", 0.7),
    ("item", 0.15),
    ("item_display", 0.0),
    ("item_frame", 0.0),
    ("jungle_boat", 0.8),
    ("jungle_chest_boat", 0.8),
    ("leash_knot", 0.0),
    ("lightning_bolt", 0.0),
    ("lingering_potion", 0.0),
    ("llama", 0.7),
    ("llama_spit", 0.0),
    ("magma_cube", 0.25),
    ("mangrove_boat", 0.8),
    ("mangrove_chest_boat", 0.8),
    ("marker", 0.0),
    ("minecart", 0.7),
    ("mooshroom", 0.7),
    ("mule", 0.75),
    ("nautilus", 0.7),
    ("oak_boat", 0.8),
    ("oak_chest_boat", 0.8),
    ("ocelot", 0.4),
    ("ominous_item_spawner", 0.0),
    ("painting", 0.0),
    ("pale_oak_boat", 0.8),
    ("pale_oak_chest_boat", 0.8),
    ("panda", 0.9),
    ("parched", 0.5),
    ("parrot", 0.3),
    ("phantom", 0.75),
    ("pig", 0.7),
    ("piglin", 0.5),
    ("piglin_brute", 0.5),
    ("pillager", 0.5),
    ("player", 0.5),
    ("polar_bear", 0.9),
    ("pufferfish", 0.2),
    ("rabbit", 0.3),
    ("ravager", 1.1),
    ("salmon", 0.4),
    ("sheep", 0.7),
    ("shulker", 0.0),
    ("shulker_bullet", 0.0),
    ("silverfish", 0.3),
    ("skeleton", 0.5),
    ("skeleton_horse", 0.75),
    ("slime", 0.25),
    ("small_fireball", 0.0),
    ("sniffer", 1.1),
    ("snow_golem", 0.5),
    ("snowball", 0.0),
    ("spawner_minecart", 0.7),
    ("spectral_arrow", 0.0),
    ("spider", 0.8),
    ("splash_potion", 0.0),
    ("spruce_boat", 0.8),
    ("spruce_chest_boat", 0.8),
    ("squid", 0.7),
    ("stray", 0.5),
    ("strider", 0.5),
    ("sulfur_cube", 0.25),
    ("tadpole", 0.14),
    ("text_display", 0.0),
    ("tnt", 0.5),
    ("tnt_minecart", 0.7),
    ("trader_llama", 0.7),
    ("trident", 0.0),
    ("tropical_fish", 0.15),
    ("turtle", 0.7),
    ("vex", 0.3),
    ("villager", 0.5),
    ("vindicator", 0.5),
    ("wandering_trader", 0.5),
    ("warden", 0.9),
    ("wind_charge", 0.0),
    ("witch", 0.5),
    ("wither", 1.0),
    ("wither_skeleton", 0.5),
    ("wither_skull", 0.0),
    ("wolf", 0.5),
    ("zoglin", 0.7),
    ("zombie", 0.5),
    ("zombie_horse", 0.75),
    ("zombie_nautilus", 0.7),
    ("zombie_villager", 0.5),
    ("zombified_piglin", 0.5),
];

/// The handful of types overriding `EntityRenderer.shadowStrength`'s `1.0F`
/// default — generated alongside [`SHADOW_RADII`] by the same script, and
/// exactly two entries in 26.2. Read it with [`shadow_strength`]; anything
/// absent takes `1.0`.
const SHADOW_STRENGTHS: &[(&str, f32)] = &[
    ("experience_orb", 0.75),
    ("item", 0.75),
];

/// Vanilla's `shadowRadius` for `type_path`, or [`SHADOW_RADIUS_FALLBACK`]
/// when the type is not in [`SHADOW_RADII`].
///
/// The fallback is **not** `EntityRenderer`'s `0.0F` default on purpose. A
/// miss here means *this table* is stale — a type added by a newer protocol
/// family, or a name this tree spells differently — not that vanilla gives
/// that entity no shadow, and silently dropping the shadow for everything new
/// is the harder failure to notice. `0.5` keeps the pre-table behaviour for
/// exactly the rows nobody has generated yet.
#[must_use]
fn shadow_radius(type_path: &str) -> f32 {
    SHADOW_RADII
        .binary_search_by_key(&type_path, |&(name, _)| name)
        .map_or(SHADOW_RADIUS_FALLBACK, |i| SHADOW_RADII[i].1)
}

/// Vanilla's `shadowStrength` for `type_path` — `1.0` unless the type is one
/// of [`SHADOW_STRENGTHS`]' overrides.
#[must_use]
fn shadow_strength(type_path: &str) -> f32 {
    SHADOW_STRENGTHS
        .binary_search_by_key(&type_path, |&(name, _)| name)
        .map_or(1.0, |i| SHADOW_STRENGTHS[i].1)
}

/// What [`shadow_radius`] returns for a type [`SHADOW_RADII`] does not list —
/// see its doc for why this is the old flat value rather than vanilla's own
/// `0.0F` field default.
const SHADOW_RADIUS_FALLBACK: f32 = 0.5;


/// Whether block-state `id`'s collision shape fills the entire cell —
/// vanilla's `Block.isShapeFullBlock`/`BlockState.isCollisionShapeFullBlock`,
/// the gate `EntityRenderer.extractShadowPiece` puts on the block a shadow
/// piece sits on.
///
/// Approximated as "at least one of the state's collision boxes spans the
/// full unit cube on every axis" rather than vanilla's exact "the shape
/// occludes every face" predicate — the two agree for the overwhelming
/// majority of real ground (stone, dirt, planks, wool, glass…) and both
/// reject every partial shape (slabs, stairs, fences, carpets, pressure
/// plates), which is the property [`RenderState::prepare_shadows`]'s own doc
/// depends on. `None` (an id outside the collision table) reads as "not
/// ground", never as a guess.
#[must_use]
fn is_full_solid_ground(id: u32) -> bool {
    const EPS: f32 = 1e-4;
    lodestone_data::collision_shapes::collision_boxes(id).is_some_and(|boxes| {
        boxes.iter().any(|b| {
            b.min[0] <= EPS
                && b.max[0] >= 1.0 - EPS
                && b.min[1] <= EPS
                && b.max[1] >= 1.0 - EPS
                && b.min[2] <= EPS
                && b.max[2] >= 1.0 - EPS
        })
    })
}

/// Push one shadow piece — a flat, `y`-level quad from `(x0, z0)` to `(x1,
/// z1)`, its four corners' UVs interpolated from `(u0, v0)`..`(u1, v1)`, all
/// four vertices carrying the same per-piece `alpha` — as two triangles (six
/// [`lodestone_render::entity_pipeline::ShadowVertex`]).
///
/// Winding is not load-bearing here: [`EntityPipeline::shadow_pipeline`]
/// draws with `cull_mode: None`, the same double-sided choice this crate's
/// entity meshes already make while per-model winding parity is unverified —
/// see that pipeline's own module docs.
///
/// [`EntityPipeline::shadow_pipeline`]: lodestone_render::entity_pipeline::EntityPipeline::shadow_pipeline
#[allow(clippy::too_many_arguments)]
fn push_shadow_quad(
    out: &mut Vec<lodestone_render::entity_pipeline::ShadowVertex>,
    y: f32,
    x0: f32,
    x1: f32,
    z0: f32,
    z1: f32,
    u0: f32,
    u1: f32,
    v0: f32,
    v1: f32,
    alpha: f32,
) {
    use lodestone_render::entity_pipeline::ShadowVertex;
    let a = ShadowVertex {
        position: [x0, y, z0],
        uv: [u0, v0],
        alpha,
    };
    let b = ShadowVertex {
        position: [x0, y, z1],
        uv: [u0, v1],
        alpha,
    };
    let c = ShadowVertex {
        position: [x1, y, z1],
        uv: [u1, v1],
        alpha,
    };
    let d = ShadowVertex {
        position: [x1, y, z0],
        uv: [u1, v0],
        alpha,
    };
    out.extend_from_slice(&[a, b, c, a, c, d]);
}

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
    // as that function, for the same reason.
    lodestone_data::entity_type::EntityType::from_name(type_path)
        .map(lodestone_data::entity_dimensions::base_dimensions_for)
        .map_or(0.0, |dims| dims.height * 0.85 * age_scale)
}

/// How far above its feet the **fishing line's** owner keeps its eye, in blocks.
///
/// Vanilla's `getPlayerHandPos` builds its offset from
/// `owner.getEyePosition(partialTicks)`, and `getEyePosition` reads the eye
/// height of the entity's **current pose** — so a crouching caster's line leaves
/// 0.35 blocks lower than a standing one's, on top of the separate `-0.1875`
/// crouch term the offset itself carries.
///
/// [`eye_probe_offset`] alone cannot answer that: its table is the *standing*
/// eye height, which is the right answer for a light probe (vanilla's own
/// `getLightProbePosition` is called on entities whose pose this client mostly
/// does not track) and the wrong one here for the one entity type whose crouch
/// this client does know about. So this forks on the player's crouch and falls
/// through to the shared probe offset for everything else, rather than
/// introducing a second eye-height table.
fn fishing_owner_eye_offset(owner: &EntityDraw) -> f32 {
    if owner.anim.crouching {
        return lodestone_physics::pose::Pose::Crouching.eye_height() * owner.scale;
    }
    eye_probe_offset(&owner.type_path, owner.scale)
}

/// The packed `sky << 4 | block` light one entity draws with — the whole of
/// vanilla's own get-packed-light-coords entity renderer accessor, and the single place any
/// pass in this module or [`super::world_items`] should get an entity's light.
///
/// Two rules, both of which this file used to miss:
///
/// * **The probe is the entity's eye, not its feet.**
///   Vanilla's own get-packed-light-coords accessor floors the entity's own
///   light-probe position at a given partial tick into a block position
///   and its own get-light-probe-position accessor returns `getEyePosition`, so a tall mob in a
///   dark cell with a lit head is lit *by its head*. Every call site here passed
///   `feet` before, and a comment on `EntityLightSource` claimed that was
///   vanilla; it never was.
/// * **Fire forces the block half to 15, and only the block half.**
///   Vanilla's own get-block-light-level entity renderer accessor is
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

/// The packed light an item **frame** — its body, and by default its contents —
/// is drawn at.
///
/// Vanilla's own item-frame renderer's get-block-light-level override overrides the base renderer's to
/// `Math.max(5, super…)` for a `glow_item_frame`, so a glowing frame is never
/// darker than block light 5 however dark the wall behind it. That floor is on
/// the **block** nibble only; the sky nibble passes through, which is what keeps
/// a glow frame in daylight looking like every other frame.
///
/// The probe is the entity's own position and not an eye-height offset: an item
/// frame's `EntityDimensions` eye height is `0.0` (see `EYE_HEIGHTS`), and the
/// entity sits inside the air cell it hangs in rather than in the wall — that is
/// what `ItemFrame.createBoundingBox`'s `-0.46875` leaves it 1/32 short of.
#[must_use]
pub(super) fn item_frame_light(
    source: &super::EntityLightSource,
    draw: &EntityDraw,
    glow: bool,
) -> u8 {
    /// `ItemFrameRenderer.GLOW_FRAME_BRIGHTNESS`.
    const GLOW_FRAME_BRIGHTNESS: u8 = 5;
    let packed = source.sample(draw.feet);
    if glow {
        (packed & 0xF0) | (packed & 0x0F).max(GLOW_FRAME_BRIGHTNESS)
    } else {
        packed
    }
}

/// The packed light the **contents** of an item frame are drawn at, given the
/// frame's own [`item_frame_light`].
///
/// A glow frame lights what it holds *fully*, not merely to its own floor:
/// `getLightCoords(state.isGlowFrame, 15728880, state.lightCoords)` substitutes
/// `15728880` — sky 15, block 15 — for the sampled value in the item branch. So
/// the body of a glow frame in a dark room is dim-but-visible and the item in it
/// is at full brightness, which is two different numbers from one sample and the
/// reason this is a second function rather than a flag on the first.
#[must_use]
pub(super) fn framed_content_light(frame_light: u8, glow: bool) -> u8 {
    if glow {
        lodestone_render::ENTITY_FULLBRIGHT | 0x0F
    } else {
        frame_light
    }
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
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> PreparedEntityBatches {
        if entities.is_empty() {
            return PreparedEntityBatches::default();
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
        let mut water_mask_instances = Vec::new();
        for e in entities {
            // `LivingEntityRenderer.submit`'s `isBodyVisible` gate on its own
            // `submitModel` call: an invisible entity draws no body/rig at
            // all, full stop, for *this* pass. Armour (`prepare_armour`) and
            // held items (`merge_held_items`/`special_item_instances`) are
            // separate passes that re-resolve the entity's pose from scratch
            // rather than reusing the instance built below, so they are
            // unaffected — matching vanilla's own `shouldRenderLayers`
            // running unconditionally regardless of body visibility. So is
            // the nametag pass, which reads this same `entities` slice
            // independently: an invisible, named entity still shows its tag.
            // See `EntityDraw::invisible`.
            if e.invisible {
                continue;
            }
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
                note_missing_entity_model(e.type_path.as_ref(), e.model_type_path());
                continue;
            };
            // Issue #573: the swim body-pitch rotation. Gated on `type_path`,
            // not on `swim_amount > 0.0` alone — see `apply_swim_rotation`'s
            // own doc for why only the player is ported.
            if e.type_path.as_ref() == "player" {
                apply_swim_rotation(&mut instance, e.feet, e.yaw, e.death_time, e.pitch, e.swim_amount);
            }
            // `AbstractBoatRenderer.submit`'s hull roll — and
            // `AbstractMinecartRenderer.submit`'s, which is the identical
            // formula about the identical axis at the identical point in the
            // pose stack. Gated on the *placement* rather than on the type
            // path, because that is what decides the matrix this conjugates
            // against; every other rig routed through that placement (the
            // leash knot, the wither skull, the two projectiles) carries
            // `BoatHurt::REST` and takes the exact-zero early return.
            if let Some((vertical_offset, _)) =
                lodestone_render::non_living_vehicle_placement(instance.model)
            {
                apply_boat_rock(&mut instance, e.feet, e.yaw, vertical_offset, e.anim.boat_hurt);
            }
            // `ArmorStandModel.setupAnim`'s `leftArm.visible = state.showArms`
            // and `basePlate.visible = state.showBasePlate`. See
            // `hide_armor_stand_parts`'s own doc for why a matrix, not a flag,
            // is how this renderer expresses "invisible part".
            if let Some(flags) = e.armor_stand
                && let Some(wearer) = self.entities.models.get(instance.model)
            {
                hide_armor_stand_parts(&mut instance, wearer, flags);
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

            // The water-clip mask: a **second**, separately-pipelined instance
            // for this same boat, at the same feet/yaw/pitch/scale but resolved
            // to an entirely different rig (`"boat_water_patch"`) — see
            // `EntityPipeline::water_mask_pipeline`'s doc for why this closes
            // the owner report "placing down a boat still shows water through
            // the bottom". `ends_with("_boat")` also matches `_chest_boat`
            // (`"oak_chest_boat".ends_with("_boat")` is `true`), which is
            // exactly right: vanilla's `BoatRenderer` submits this mask for
            // both. `_raft`/`_chest_raft` never match, matching
            // `RaftRenderer`'s own empty `submitTypeAdditions` — see
            // `lodestone_assets::entity_models::boat_water_patch_model`'s doc
            // for why rafts get none of this. The patch is accumulated in a
            // separate phase, never in `groups`: even pushing it after this
            // boat is insufficient because a later material/skin group can
            // contain the rider. A depth-only patch between those batches
            // erased 798 rider pixels in the dry GPU gate. `gpu/frame.rs`
            // submits the entire phase only after every visible opaque/cutout
            // draw and immediately before translucent water.
            let patch = if e.type_path.as_ref().ends_with("_boat") {
                self.entities
                    .models
                    .resolve_animated(
                        "boat_water_patch",
                        e.feet,
                        e.yaw,
                        e.pitch,
                        e.scale,
                        &e.anim,
                        0.0,
                        0.0,
                    )
                    .map(|mut patch| {
                        // The mask has to rock with the hull it masks: vanilla
                        // submits it from inside the *same* `pushPose` block, after
                        // the hurt rotation, so a patch left level would slide out
                        // from under a tipped boat and let water back through
                        // exactly while the boat is moving most.
                        // The bob is read back off the patch's *own* placement
                        // rather than restated, so the two can never drift apart.
                        if let Some((vertical_offset, _)) =
                            lodestone_render::non_living_vehicle_placement(patch.model)
                        {
                            apply_boat_rock(
                                &mut patch,
                                e.feet,
                                e.yaw,
                                vertical_offset,
                                e.anim.boat_hurt,
                            );
                        }
                        patch.with_light(entity_light(&self.entity_light, e))
                    })
            } else {
                None
            };

            match groups.iter_mut().position(|(hurt, flash, url, s, _)| {
                *hurt == e.hurt && *flash == white && *url == skin && *s == sheet
            }) {
                Some(i) => groups[i].4.push(instance),
                None => groups.push((e.hurt, white, skin.clone(), sheet, vec![instance])),
            }

            if let Some(patch) = patch {
                water_mask_instances.push(patch);
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
        let visible = plans
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
                    .map(|p| {
                        stage_instances_tinted(
                            &self.instance_arena,
                            p,
                            &batch.lights,
                            &tints,
                        )
                    })
                    .collect();
                EntityDrawBatch {
                    model: batch.model,
                    count,
                    parts,
                    skin: skin.cloned(),
                    variant_sheet: sheet,
                }
            })
            .collect();

        let water_mask_frame = plan_entities(&water_mask_instances, &frustum);
        let water_masks = water_mask_frame
            .batches
            .iter()
            .map(|batch| {
                let count = u32::try_from(batch.transforms.len()).unwrap_or(u32::MAX);
                let tints = vec![InstanceTint::NONE; batch.transforms.len()];
                let parts = batch
                    .parts
                    .iter()
                    .map(|part| {
                        stage_instances_tinted(
                            &self.instance_arena,
                            part,
                            &batch.lights,
                            &tints,
                        )
                    })
                    .collect();
                EntityDrawBatch {
                    model: batch.model,
                    count,
                    parts,
                    skin: None,
                    variant_sheet: None,
                }
            })
            .collect();

        PreparedEntityBatches {
            visible,
            water_masks,
        }
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
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
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
                        stage_instances_tinted(
                            &self.instance_arena,
                            &p.transforms,
                            &p.lights,
                            &p.tints,
                        )
                            .map(|instances| (p.range, instances, count))
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
            if !self.entities.flame_gpu_models.contains_key(draw.type_path.as_ref()) {
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
            accum.entry(draw.type_path.to_string()).or_default().push(transform);
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

    /// Resolve this frame's entity ground shadows (owner report: "entity
    /// shadows are missing") into one vertex buffer — vanilla's own
    /// entity-renderer shadow-extract routines, transcribed as a formula rather than a
    /// per-species table; see `SHADOW_RADIUS`/`SHADOW_STRENGTH` for the
    /// disclosed simplification.
    ///
    /// # The ground scan, and what it does about slabs/stairs/edges
    ///
    /// For each candidate cell `pos` from `floor(feet - depth)` up to
    /// `floor(feet)` in Y (vanilla's own range — `depth` shrinks as the
    /// entity's own light-derived `pow` shrinks, capped at the shadow
    /// radius), this asks the installed `ShadowGroundSource` for the block
    /// **below** `pos` and treats it as shadow-catching ground only when
    /// `is_full_solid_ground` says its collision shape fills the whole cell.
    ///
    /// That is where this port departs from vanilla on purpose. Vanilla asks
    /// for `belowState.getShape(..)`'s real `VoxelShape` and paints a piece
    /// shaped exactly like it — so a shadow on a slab is a half-height piece
    /// and a shadow at a stair's edge follows the step. This scan instead
    /// gates on "is the block below a full cube" and draws nothing at all for
    /// a non-full block (a slab, a stair, a fence, a carpet…) — the loop then
    /// simply continues scanning **downward** through the rest of `y0..=y1`,
    /// so a mob standing on a slab or a stair gets its shadow one cell lower,
    /// on the next full block underneath (or no shadow at all if none is
    /// within `depth`), rather than a shadow shaped like the slab or stair
    /// itself. An edge — the entity standing half over open air — behaves the
    /// same way per column: the columns with ground under them draw a piece,
    /// the columns without draw nothing, exactly matching vanilla's per-column
    /// independence; only the *shape* of an individual non-full piece is
    /// approximated away.
    pub(super) fn prepare_shadows(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Option<ShadowBatch> {
        if !self.entity_shadows_enabled {
            return None;
        }
        let Some(_shadow_texture) = &self.entities.shadow_texture else {
            return None;
        };
        let frustum = camera.frustum();
        let mut vertices: Vec<lodestone_render::entity_pipeline::ShadowVertex> = Vec::new();

        for draw in entities {
            // Vanilla's own gate — `minecraft.options.entityShadows().get() &&
            // !state.isInvisible`; the option half is checked once, above.
            if draw.invisible {
                continue;
            }
            let radius = (shadow_radius(&draw.type_path) * draw.scale).min(32.0);
            if radius <= 0.0 {
                continue;
            }
            let feet = draw.feet;
            if !frustum.intersects_aabb(
                feet - glam::Vec3::new(radius, 0.0, radius),
                feet + glam::Vec3::new(radius, 1.0, radius),
            ) {
                continue;
            }
            let dist_sq = camera.position.distance_squared(feet);
            let pow = (1.0 - dist_sq / 256.0) * shadow_strength(&draw.type_path);
            if pow <= 0.0 {
                continue;
            }
            let depth = (pow / 0.5 - 1.0).min(radius);
            let x0 = (feet.x - radius).floor() as i32;
            let x1 = (feet.x + radius).floor() as i32;
            let z0 = (feet.z - radius).floor() as i32;
            let z1 = (feet.z + radius).floor() as i32;
            let y0 = (feet.y - depth).floor() as i32;
            let y1 = feet.y.floor() as i32;

            for z in z0..=z1 {
                for x in x0..=x1 {
                    for y in y0..=y1 {
                        let Some(below_id) = self.shadow_ground.sample([x, y - 1, z]) else {
                            continue;
                        };
                        if !is_full_solid_ground(below_id) {
                            continue;
                        }
                        // Vanilla samples brightness at `pos` itself (the open
                        // cell above the ground), not at the ground below it —
                        // `EntityRenderer.extractShadowPiece`'s own `pos`.
                        let probe = glam::Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                        let packed = self.entity_light.sample(probe);
                        let raw = ((packed >> 4) & 0x0F).max(packed & 0x0F);
                        // `level.getMaxLocalRawBrightness(pos) > 3` — vanilla's own
                        // floor before a piece is added at all.
                        if raw <= 3 {
                            continue;
                        }
                        let power_at_depth = pow - (feet.y - y as f32) * 0.5;
                        let curve = lodestone_render::light::brightness(f32::from(raw) / 15.0);
                        let alpha = (power_at_depth * 0.5 * curve).clamp(0.0, 1.0);
                        if alpha <= 0.0 {
                            continue;
                        }
                        let rel_x = x as f32 - feet.x;
                        let rel_z = z as f32 - feet.z;
                        // `ShadowFeatureRenderer.prepare`'s own UV formula —
                        // `-x / 2.0 / radius + 0.5`, and the `z` sibling.
                        let u0 = -rel_x / (2.0 * radius) + 0.5;
                        let u1 = -(rel_x + 1.0) / (2.0 * radius) + 0.5;
                        let v0 = -rel_z / (2.0 * radius) + 0.5;
                        let v1 = -(rel_z + 1.0) / (2.0 * radius) + 0.5;
                        push_shadow_quad(
                            &mut vertices,
                            y as f32,
                            feet.x + rel_x,
                            feet.x + rel_x + 1.0,
                            feet.z + rel_z,
                            feet.z + rel_z + 1.0,
                            u0,
                            u1,
                            v0,
                            v1,
                            alpha,
                        );
                        stats.shadow_pieces_drawn += 1;
                    }
                }
            }
        }

        let count = u32::try_from(vertices.len()).unwrap_or(u32::MAX);
        lodestone_render::entity_pipeline::upload_shadow_vertices(device, &vertices)
            .map(|buffer| ShadowBatch { buffer, count })
    }

    /// Resolve this frame's experience orbs into per-sprite-cell instance
    /// buffers — `ExperienceOrbRenderer`, which is one camera-facing quad each.
    ///
    /// No pack, no sheet, nothing to draw, and no synthetic fallback — the same
    /// asymmetry `EntityRenderer::flame_texture`/`wool_texture` document.
    ///
    /// # Batched by sprite cell, because the cell is geometry
    ///
    /// Vanilla's own experience-orb icon lookup buckets an orb's value into one of eleven cells,
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
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
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
                stage_instances_tinted(
                    &self.instance_arena,
                    &transforms,
                    &lights,
                    &tints,
                )
                .map(|instances| {
                    OrbBatch {
                        icon: u32::try_from(icon).unwrap_or(0),
                        instances,
                        count,
                    }
                })
            })
            .collect()
    }

    /// The camera-facing entity sprites — a dragon fireball and a fishing
    /// bobber, the two 26.2 entity renderers that build a textured quad vertex
    /// by vertex instead of posing a rig.
    ///
    /// Mirrors [`prepare_orbs`](Self::prepare_orbs) exactly, with one
    /// difference: the orb's eleven cells share one sheet so its batch key
    /// selects only geometry, while these two sprites are two *separate*
    /// sheets, so the batch key selects the group-1 bind group as well. Both
    /// come out of the same index — see `EntityRenderer::sprite_gpu_model`.
    ///
    /// The billboard rotation is per *frame*, not per entity, for
    /// `prepare_orbs`' reason: it depends only on the camera.
    ///
    /// # What this pass does **not** draw
    ///
    /// The fishing line. It is line geometry through a screen-space ribbon
    /// pipeline, not a textured quad — see
    /// [`fishing_line_vertices`](Self::fishing_line_vertices).
    pub(super) fn prepare_entity_sprites(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<EntitySpriteBatch> {
        let Some(model) = &self.entities.sprite_gpu_model else {
            return Vec::new();
        };
        let orientation = lodestone_render::entity::camera_orientation(camera.view_matrix());
        let frustum = camera.frustum();
        // One accumulator per table row, indexed by the row — a `Vec` rather
        // than a map for `prepare_orbs`' reason: the key is a small dense
        // integer.
        let mut accum: Vec<(Vec<glam::Mat4>, Vec<u32>)> =
            lodestone_render::entity_sprite::ENTITY_SPRITES
                .iter()
                .map(|_| (Vec::new(), Vec::new()))
                .collect();

        for draw in entities {
            // The **index**, not a reference: it selects the baked geometry and
            // the texture bind group as well as the row, and recovering it from
            // a `&'static EntitySprite` by address cannot work — `ENTITY_SPRITES`
            // is a `const`, so it is inlined per use site. That is not
            // hypothetical: this pass shipped that way for one run and drew
            // zero pixels with every table value correct.
            let Some(index) =
                lodestone_render::entity_sprite::entity_sprite_index_for(&draw.type_path)
            else {
                continue;
            };
            let Some(sprite) = lodestone_render::entity_sprite::entity_sprite_at(index) else {
                continue;
            };
            // No sheet, no draw — a sprite whose texture the pack does not
            // carry contributes nothing rather than an untextured quad, the
            // same asymmetry `prepare_orbs` and the flame pass document. Tested
            // **here**, before the counter below, so `entity_sprites_drawn`
            // never reports a sprite that no draw call will reach: a counter
            // that leads its own draw is the shape `CLAUDE.md` records for
            // `vram_bytes`, one layer down.
            if self
                .entities
                .sprite_textures
                .get(index)
                .and_then(Option::as_ref)
                .is_none()
            {
                continue;
            }
            // A sprite is at most `scale` blocks across after the billboard
            // rotation, so a box of that half-extent around the feet covers it
            // however the camera is turned. No `EntityModelSet::resolve` to take
            // an AABB from: these types have no rig, which is why this pass
            // exists.
            let extent = glam::Vec3::splat(sprite.scale.max(0.5));
            if !frustum.intersects_aabb(draw.feet - extent, draw.feet + extent) {
                continue;
            }
            let Some((transforms, lights)) = accum.get_mut(index) else {
                continue;
            };
            transforms.push(lodestone_render::entity_sprite::entity_sprite_matrix(
                draw.feet,
                orientation,
                sprite.scale,
            ));
            // `entity_light` is the shared eye probe every other entity layer
            // here uses. `full_bright` forces the **block** nibble only, which
            // is where vanilla's `getBlockLightLevel` override sits — forcing
            // the whole byte would give a fireball in a dark cave a daytime sky
            // as well, the same asymmetry `experience_orb_light` records.
            let packed = entity_light(&self.entity_light, draw);
            lights.push(u32::from(if sprite.full_bright {
                packed | 0x0F
            } else {
                packed
            }));
            stats.entity_sprites_drawn += 1;
        }

        accum
            .into_iter()
            .enumerate()
            .filter(|(_, (transforms, _))| !transforms.is_empty())
            .filter_map(|(sprite, (transforms, lights))| {
                let count = u32::try_from(transforms.len()).unwrap_or(u32::MAX);
                // Both vanilla renderers pass `setColor(-1)`, i.e. plain white,
                // so there is no per-instance tint to carry — an empty slice
                // leaves every instance at `EntityInstanceRaw`'s untinted
                // default.
                stage_instances_tinted(
                    &self.instance_arena,
                    &transforms,
                    &lights,
                    &[],
                )
                .map(|instances| {
                    EntitySpriteBatch {
                        sprite,
                        instances,
                        count,
                    }
                })
            })
            .filter(|batch| {
                model
                    .parts
                    .get(batch.sprite)
                    .is_some_and(|range| range.index_count > 0)
            })
            .collect()
    }

    /// This frame's fishing lines, as the flat vertex-pair wire shape
    /// [`super::debug_lines::DebugLineRenderer::prepare`] expands into
    /// screen-space ribbons.
    ///
    /// One line per `fishing_bobber` whose spawn packet carried an owner id
    /// ([`EntityDraw::projectile_owner`]), sixteen segments each
    /// ([`lodestone_render::entity_sprite::fishing_line_points`]).
    ///
    /// # Resolving the anchor, which is the whole of the interesting part
    ///
    /// Vanilla forks on `getCameraType().isFirstPerson() && owner ==
    /// (vanilla's own client-instance-accessor's player)`: our own rod seen from our own eyes gets
    /// a near-plane projection, everything else gets an offset off the owner
    /// entity's body. This reproduces that fork **without** knowing our own
    /// entity id, because two facts already encode it:
    ///
    /// * `entities::extract_entity_draws` deliberately excludes the local
    ///   player, so a lookup by the wire's owner id missing means "the owner is
    ///   us";
    /// * `ThirdPersonBodyState::into_draw` pushes a synthetic draw under
    ///   [`super::sources::LOCAL_PLAYER_DRAW_ID`] **iff** the camera is
    ///   detached.
    ///
    /// So: found by real id → third-person branch on that entity; not found but
    /// the synthetic body is present → third-person branch on our own body;
    /// neither → first person, and the camera is the anchor. Each of the three
    /// is exactly the branch vanilla would take.
    ///
    /// The one case this gets wrong is a bobber whose owner is a *remote* player
    /// outside tracking range: vanilla draws nothing at all (`shouldRender`
    /// requires a non-null player owner) and this anchors the line at our own
    /// hand. A bobber is always within a few blocks of its caster, so a visible
    /// bobber whose owner is untracked is close to unreachable — but it is a
    /// real difference and not a rounding one.
    pub(super) fn fishing_line_vertices(
        &self,
        camera: &Camera,
        entities: &[EntityDraw],
    ) -> Vec<super::debug_lines::DebugLineVertex> {
        use lodestone_render::entity_sprite as sprite;

        let frustum = camera.frustum();
        let mut out = Vec::new();
        for draw in entities {
            if draw.type_path.as_ref() != sprite::FISHING_BOBBER_TYPE_PATH {
                continue;
            }
            let Some(owner_id) = draw.projectile_owner else {
                continue;
            };
            // The line can be long, so the cull box is the *pair* of endpoints
            // rather than the bobber alone — a bobber just off screen still has
            // a line crossing it.
            let owner = entities
                .iter()
                .find(|d| d.id == owner_id)
                .or_else(|| entities.iter().find(|d| d.id == super::sources::LOCAL_PLAYER_DRAW_ID));
            let hand = match owner {
                Some(owner) => {
                    // `getHoldingArm`: the rod's own hand, or the opposite when
                    // the main hand is holding something else.
                    let rod_in_main_hand = owner.equipment.iter().any(|(slot, item)| {
                        *slot == EquipmentSlot::MainHand && item.path() == "fishing_rod"
                    });
                    let arm = sprite::fishing_holding_arm_sign(
                        owner.main_arm_left,
                        rod_in_main_hand,
                    );
                    sprite::fishing_hand_anchor_third_person(
                        owner.feet
                            + glam::Vec3::new(0.0, fishing_owner_eye_offset(owner), 0.0),
                        owner.yaw,
                        owner.scale,
                        arm,
                        owner.anim.crouching,
                    )
                }
                None => {
                    // First person, and the owner is us. `camera_orientation`'s
                    // columns are the camera basis in world space — the same
                    // matrix every billboard here shares — so `right`/`up`/
                    // `forward` come out of it rather than from a second,
                    // independently-derived Euler expansion.
                    let orientation =
                        lodestone_render::entity::camera_orientation(camera.view_matrix());
                    let right = orientation.x_axis.truncate();
                    let up = orientation.y_axis.truncate();
                    let forward = -orientation.z_axis.truncate();
                    // The local player has no draw record, so the two owner
                    // facts vanilla reads off the entity come from elsewhere.
                    //
                    // The swing is real: `HandSwingSource` is the same
                    // `Sim::hand_swing_progress` the first-person arm pass
                    // polls, i.e. exactly the `getAttackAnim(partialTicks)`
                    // vanilla passes here — so the rod tip carries the cast
                    // rather than hanging at rest. Feeding this a constant is
                    // the defect shape `CLAUDE.md` records for
                    // `creeper_swelling`, and the source already existed.
                    //
                    // The **arm** is not: vanilla's own main-arm accessor is a synced
                    // client option this build does not decode for anyone,
                    // local player included, so right-handed is vanilla's own
                    // default rather than a guess — the same gap
                    // `ThirdPersonBodyState::into_draw`'s `main_arm_left`
                    // states for our own body. It is `1.0` here and not a read
                    // of `EntityDraw::main_arm_left` because there is no draw
                    // record to read it from.
                    sprite::fishing_hand_anchor_first_person(
                        camera.position,
                        right,
                        up,
                        forward,
                        camera.near,
                        camera.fov_y_degrees,
                        camera.aspect,
                        sprite::fishing_swing_shaping(self.hand_swing.value()),
                        1.0,
                    )
                }
            };
            let lo = draw.feet.min(hand) - glam::Vec3::splat(0.5);
            let hi = draw.feet.max(hand) + glam::Vec3::splat(0.5);
            if !frustum.intersects_aabb(lo, hi) {
                continue;
            }
            let points = sprite::fishing_line_points(draw.feet, hand);
            for pair in points.windows(2) {
                out.push(super::debug_lines::DebugLineVertex {
                    position: pair[0].to_array(),
                    color: sprite::FISHING_LINE_COLOR,
                });
                out.push(super::debug_lines::DebugLineVertex {
                    position: pair[1].to_array(),
                    color: sprite::FISHING_LINE_COLOR,
                });
            }
        }
        out
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
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<(lodestone_render::PartRange, std::ops::Range<u64>, u32)> {
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
                stage_instances_tinted(
                    &self.instance_arena,
                    &p.transforms,
                    &p.lights,
                    &p.tints,
                )
                    .map(|instances| (p.range, instances, count))
            })
            .collect()
    }

    /// Resolve this frame's **player capes** into per-URL instance buffers.
    ///
    /// `CapeLayer.submit` (`26.2`), transcribed: `!invisible && showCape &&
    /// skin.cape() != null && !hasLayer(chestEquipment, WINGS)`. The last
    /// clause — an elytra in the chest slot suppresses the cape — is
    /// approximated as "the chest slot's item path is literally `elytra`"
    /// rather than the real `EquipmentClientInfo`/`EquipmentAssetManager`
    /// lookup vanilla's `hasLayer` does (which resolves a `minecraft:elytra`
    /// asset id to a set of layer types and asks whether `WINGS` is one of
    /// them): every 26.2 elytra item *is* that asset with a `WINGS` layer, so
    /// the two agree for the vanilla item and would only diverge for a
    /// resource-pack-only custom chestplate asset that also declares a wings
    /// layer, which this build has no path to represent anyway.
    ///
    /// `showCape` — the *subject* player's own `modelPart.cape` toggle,
    /// broadcast to observers on `Player`'s `DATA_PLAYER_MODE_CUSTOMISATION`
    /// metadata byte — is not decoded on this side of the wire (no
    /// clientbound arm for that byte exists in `crates/protocol/v770` today),
    /// so every remote player draws as if `showCape` were `true`, matching
    /// vanilla's own default when the byte has never been reported.
    pub(super) fn prepare_cape(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<CapeDrawBatch> {
        if self.entities.cape_gpu.is_none() {
            return Vec::new();
        }
        let frustum = camera.frustum();
        let mut groups: Vec<(String, Vec<glam::Mat4>, Vec<u32>, Vec<InstanceTint>)> = Vec::new();

        for draw in entities {
            if draw.invisible || draw.type_path.as_ref() != "player" {
                continue;
            }
            let Some(skin) = draw.player_skin.as_ref() else {
                continue;
            };
            let Some(cape_url) = skin.cape.as_ref().filter(|u| !u.is_empty()) else {
                continue;
            };
            // Only a bind group actually installed in `player_skins` can be
            // drawn — a fetch still in flight (or failed) draws nothing for
            // this player this frame, exactly as an unresolved body skin
            // falls back to the default sheet rather than blocking the mob.
            if !self.entities.player_skins.contains_key(cape_url) {
                continue;
            }
            let wearing_elytra = draw.equipment.iter().any(|(slot, id)| {
                *slot == EquipmentSlot::Chest && id.namespace() == "minecraft" && id.path() == "elytra"
            });
            if wearing_elytra {
                continue;
            }
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
            let Some((_, body_index)) = self.entities.cape_model.attach(&wearer.skeleton).next()
            else {
                continue;
            };
            let Some(body_transform) = instance.part_transforms.get(body_index) else {
                continue;
            };
            let (lean, lean2, flap) = draw.cape_sway;
            let transform = *body_transform * lodestone_render::cape_local_rotation(lean, lean2, flap);
            let light = u32::from(entity_light(&self.entity_light, draw));
            let tint = InstanceTint::rgb([255, 255, 255]).with_hurt(draw.hurt);
            let group = match groups.iter_mut().position(|(url, ..)| url == cape_url) {
                Some(i) => &mut groups[i],
                None => {
                    groups.push((cape_url.clone(), Vec::new(), Vec::new(), Vec::new()));
                    groups.last_mut().expect("just pushed")
                }
            };
            group.1.push(transform);
            group.2.push(light);
            group.3.push(tint);
            stats.cape_layers_drawn += 1;
        }

        groups
            .into_iter()
            .filter_map(|(url, transforms, lights, tints)| {
                let count = u32::try_from(transforms.len()).unwrap_or(u32::MAX);
                stage_instances_tinted(
                    &self.instance_arena,
                    &transforms,
                    &lights,
                    &tints,
                )
                .map(|instances| CapeDrawBatch { url, instances, count })
            })
            .collect()
    }

    /// Resolve this frame's **elytra wings** into per-`(texture, wing)`
    /// instance buffers — the layer that replaces the cape
    /// [`Self::prepare_cape`] suppresses for an elytra wearer.
    ///
    /// `WingsLayer.submit` (`26.2`), transcribed: draw when the chest item
    /// carries an `Equippable` with a non-empty `assetId`, then ask that asset
    /// for its `WINGS` layers — which for every vanilla item means the elytra
    /// and nothing else, since a chestplate's asset declares `HUMANOID` layers
    /// and `renderLayers` then emits nothing. The gate here is therefore the
    /// same "the chest slot's item path is literally `elytra`" approximation
    /// [`Self::prepare_cape`] uses to suppress the cape, and the two must stay
    /// the same predicate or a player will be able to lose their cape and get
    /// no wings.
    ///
    /// Unlike the cape this is **two** instances per wearer, one per
    /// [`lodestone_render::ElytraMesh::attach`] entry, each with its own
    /// [`lodestone_render::elytra_wing_transform`] composed onto the wearer's
    /// `"body"` matrix. `attach` is also what gates on a humanoid rig, so a
    /// pig handed an elytra by a plugin grows no wings.
    ///
    /// # The pose is the resting one, always — a deliberate first cut
    ///
    /// `elytra_wing_transform`'s three angles want
    /// `ElytraAnimationState`'s lerped state, which does not exist on this
    /// side yet: the pure half is `lodestone_render::elytra_target_rotations`,
    /// and the impure half (two triples advanced once per game tick by
    /// `ELYTRA_ROTATION_LERP` and read back interpolated by partial ticks)
    /// belongs beside `crate::entities::cape_sway`'s lagged cloak position,
    /// which is where the equivalent cape state already lives. Until it does,
    /// this passes `lodestone_render::elytra_rest_rotations()` and `false` for
    /// `crouching` straight through.
    ///
    /// That is **correct for a wearer who is standing, walking or running**
    /// (the rest triple *is* the not-flying-not-crouching branch's target) and
    /// **wrong during a glide or a crouch**, where the wings will stay spread
    /// instead of folding back. The check a reader can run: `EntityDraw`
    /// carries no fall-flying flag and no crouch flag, so there is no input
    /// here that could select either of the other two branches — closing this
    /// means adding that state, not editing this function's arithmetic.
    ///
    /// # The texture
    ///
    /// `getPlayerElytraTexture` prefers `skin.elytra()`, then `skin.cape()`
    /// when the cape is shown, then the jar sheet. The first preference is
    /// unreachable here — `crate::remote_skins::RemoteSkin` carries no
    /// `elytra` field, so `lodestone_assets::skin::ProfileTextures::elytra` is
    /// dropped at the decode — so this implements the second and third. As in
    /// [`Self::prepare_cape`], `showCape` is not decoded on this side of the
    /// wire and is treated as `true`.
    pub(super) fn prepare_elytra(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<ElytraDrawBatch> {
        if self.entities.elytra_gpu.is_none() {
            return Vec::new();
        }
        let frustum = camera.frustum();
        type Group = (
            Option<String>,
            lodestone_render::PartRange,
            Vec<glam::Mat4>,
            Vec<u32>,
            Vec<InstanceTint>,
        );
        let mut groups: Vec<Group> = Vec::new();

        for draw in entities {
            if draw.invisible {
                continue;
            }
            let wearing_elytra = draw.equipment.iter().any(|(slot, id)| {
                *slot == EquipmentSlot::Chest && id.namespace() == "minecraft" && id.path() == "elytra"
            });
            if !wearing_elytra {
                continue;
            }
            // The wearer's own cape sheet when one is installed, else the jar
            // sheet — and if neither exists there is nothing to bind, so the
            // wings draw nothing rather than drawing untextured.
            let cape_url = draw
                .player_skin
                .as_ref()
                .and_then(|skin| skin.cape.as_ref())
                .filter(|u| !u.is_empty() && self.entities.player_skins.contains_key(u.as_str()));
            let texture = match cape_url {
                Some(url) => Some(url.clone()),
                None if self.entities.elytra_texture.is_some() => None,
                None => continue,
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
            let Some(wearer) = self.entities.models.get(instance.model) else {
                continue;
            };
            let light = u32::from(entity_light(&self.entity_light, draw));
            let tint = InstanceTint::rgb([255, 255, 255]).with_hurt(draw.hurt);
            let (x_rot, y_rot, z_rot) = lodestone_render::elytra_rest_rotations();
            for (wing, range, body_index) in self.entities.elytra_model.attach(&wearer.skeleton) {
                let Some(body_transform) = instance.part_transforms.get(body_index) else {
                    continue;
                };
                let transform = *body_transform
                    * lodestone_render::elytra_wing_transform(wing, x_rot, y_rot, z_rot, false);
                let group = match groups
                    .iter_mut()
                    .position(|(t, r, ..)| *t == texture && *r == range)
                {
                    Some(i) => &mut groups[i],
                    None => {
                        groups.push((texture.clone(), range, Vec::new(), Vec::new(), Vec::new()));
                        groups.last_mut().expect("just pushed")
                    }
                };
                group.2.push(transform);
                group.3.push(light);
                group.4.push(tint);
                stats.elytra_wings_drawn += 1;
            }
        }

        groups
            .into_iter()
            .filter_map(|(texture, range, transforms, lights, tints)| {
                let count = u32::try_from(transforms.len()).unwrap_or(u32::MAX);
                stage_instances_tinted(
                    &self.instance_arena,
                    &transforms,
                    &lights,
                    &tints,
                )
                .map(|instances| {
                    ElytraDrawBatch {
                        texture,
                        range,
                        instances,
                        count,
                    }
                })
            })
            .collect()
    }

    /// Resolve this frame's **paintings** into per-`(shape, face)` instance
    /// buffers.
    ///
    /// `PaintingRenderer.submit` (26.2), which is unusually short: rotate by
    /// `180 - direction.get2DDataValue() * 90` and emit a `width x height` grid
    /// of cells. Both halves are in
    /// [`lodestone_render::painting`]; this function is the batching and the
    /// culling.
    ///
    /// # A painting with no variant draws nothing, and that is the design
    ///
    /// `EntityDraw::painting` is `None` for a variant this build has no table
    /// entry for, and there is no fallback shape to draw instead — a painting's
    /// size in blocks *is* a property of its variant. The same applies one step
    /// later to a missing sprite: [`Self::painting_texture`] is keyed by the
    /// same static name, so a variant in the table whose PNG is not in the pack
    /// is skipped rather than bound to something else.
    ///
    /// # The facing needs no field
    ///
    /// `HangingEntity.setDirection` writes the direction into the entity's
    /// ordinary yaw (`setYRot(direction.get2DDataValue() * 90)`), so
    /// `draw.yaw` already carries it and nothing had to be decoded out of the
    /// spawn packet's Object Data. The four legal yaws survive the wire's
    /// byte-angle quantisation exactly.
    ///
    /// # Light is per painting, not per cell
    ///
    /// Vanilla samples the wall **once per 1x1 cell** — that is the entire
    /// reason its geometry is a grid — and lights each cell separately, so a
    /// 4x4 painting half in torchlight is visibly graded. This engine carries
    /// light per *instance*, so all cells share the painting's own entity
    /// probe. The grid geometry is built anyway (see
    /// [`lodestone_render::painting::painting_mesh`]), so closing this is a
    /// change to how the light lane is fed, not a re-bake.
    pub(super) fn prepare_paintings(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<PaintingDrawBatch> {
        if self.entities.painting_models.is_empty() {
            return Vec::new();
        }
        let frustum = camera.frustum();
        type Group = (usize, usize, Option<&'static str>, Vec<glam::Mat4>, Vec<u32>, Vec<InstanceTint>);
        let mut groups: Vec<Group> = Vec::new();

        for draw in entities {
            if draw.invisible {
                continue;
            }
            let Some(variant) = draw.painting else {
                continue;
            };
            let Some(size) = lodestone_render::painting::painting_size(variant) else {
                continue;
            };
            // Skipped rather than drawn untextured: a white rectangle where a
            // painting belongs reads as a rendering bug, not as a missing pack.
            if !self.entities.painting_textures.contains_key(variant)
                || self.entities.painting_back_texture.is_none()
            {
                continue;
            }
            let Some(model) = self
                .entities
                .painting_models
                .iter()
                .position(|(candidate, _)| *candidate == size)
            else {
                continue;
            };
            // The entity's wire position is the slab's **centre** (a painting is
            // placed by `Painting.calculateBoundingBox`, not stood on the
            // ground), so the cull box is centred on it too. Half the diagonal
            // in every axis covers the slab whichever of the four ways it
            // faces, which is cheaper and safer than rotating a tight box.
            let reach = 0.5 * (size.width.max(size.height) as f32) + 0.5;
            if !frustum.intersects_aabb(
                draw.feet - glam::Vec3::splat(reach),
                draw.feet + glam::Vec3::splat(reach),
            ) {
                continue;
            }
            let transform = lodestone_render::painting::painting_matrix(draw.feet, draw.yaw);
            let light = u32::from(entity_light(&self.entity_light, draw));
            let tint = InstanceTint::rgb([255, 255, 255]).with_hurt(draw.hurt);
            for (part, texture) in [(0usize, Some(variant)), (1usize, None)] {
                let group = match groups
                    .iter_mut()
                    .position(|(m, p, t, ..)| *m == model && *p == part && *t == texture)
                {
                    Some(i) => &mut groups[i],
                    None => {
                        groups.push((model, part, texture, Vec::new(), Vec::new(), Vec::new()));
                        groups.last_mut().expect("just pushed")
                    }
                };
                group.3.push(transform);
                group.4.push(light);
                group.5.push(tint);
            }
            stats.paintings_drawn += 1;
        }

        groups
            .into_iter()
            .filter_map(|(model, part, variant, transforms, lights, tints)| {
                let count = u32::try_from(transforms.len()).unwrap_or(u32::MAX);
                stage_instances_tinted(
                    &self.instance_arena,
                    &transforms,
                    &lights,
                    &tints,
                )
                .map(|instances| {
                    PaintingDrawBatch {
                        model,
                        part,
                        variant,
                        instances,
                        count,
                    }
                })
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
    /// # Item frames: this is one of three producers, not all of them
    ///
    /// Only the *special* items are here, and that is the split rather than a
    /// shortfall. A framed `filled_map` draws through `prepare_framed_maps`, every
    /// **ordinary** framed item through `world_items.rs`'s `merge_framed_items`,
    /// and the frame's own body through `moving_blocks.rs`'s `merge_item_frames`.
    /// All four share [`lodestone_render::entity::item_frame_space`], so a chest
    /// and a sword in adjacent frames cannot hang at two different depths.
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
            let is_item_frame = super::maps::ITEM_FRAME_TYPES
                .contains(&draw.type_path.as_ref());
            let visible = if is_item_frame {
                let has_map = draw.item.as_ref().is_some_and(|item| {
                    item.path() == super::maps::FILLED_MAP_ITEM
                });
                let (min, max) = lodestone_render::entity::item_frame_culling_aabb(
                    draw.feet, draw.yaw, draw.pitch, has_map,
                );
                frustum.intersects_aabb(min, max)
            } else {
                // Covers a tall holder plus the item's own reach, exactly as
                // `merge_held_items` does for the baked case.
                frustum.intersects_aabb(
                    draw.feet - glam::Vec3::new(1.0, 0.5, 1.0),
                    draw.feet + glam::Vec3::new(1.0, 2.5, 1.0),
                )
            };
            if !visible {
                continue;
            }
            let light = entity_light(&self.entity_light, draw);

            if draw.type_path.as_ref() == crate::entities::ITEM_ENTITY_TYPE_PATH {
                if let Some(instance) = self.dropped_special_item(model, draw, light) {
                    out.push(instance);
                    stats.special_item_drops_drawn += 1;
                }
                // A dropped item carries no equipment; skip the hand scan.
                continue;
            }
            if is_item_frame {
                // Not `light`: a frame's own probe is its position rather than an
                // eye height, and a glow frame lights what it holds *fully* —
                // `getLightCoords(state.isGlowFrame, 15728880, ..)`. See
                // `item_frame_light`/`framed_content_light`.
                let glow = draw.type_path.as_ref() == super::maps::GLOW_ITEM_FRAME_TYPE_PATH;
                let framed = framed_content_light(
                    item_frame_light(&self.entity_light, draw, glow),
                    glow,
                );
                if let Some(instance) = self.framed_special_item(model, draw, framed, camera) {
                    out.push(instance);
                    stats.special_item_frames_drawn += 1;
                }
                continue;
            }
            if let Some(instance) = self.worn_player_head_special_item(draw, light) {
                out.push(instance);
            }
            for (slot, id) in &draw.equipment {
                // `Mob.getMainArm()` is `RIGHT` for every mob **except a
                // left-handed one** (`draw.main_arm_left`, `Mob.isLeftHanded()`):
                // main hand is the right arm and off hand the left, unless that
                // flag is set, in which case both sides flip — the same mapping
                // `merge_held_items` applies, and the only two slots that hold an
                // *item model*. Armour goes through `prepare_armour`.
                let arm = match (slot, draw.main_arm_left) {
                    (EquipmentSlot::MainHand, false) | (EquipmentSlot::OffHand, true) => Arm::Right,
                    (EquipmentSlot::MainHand, true) | (EquipmentSlot::OffHand, false) => Arm::Left,
                    _ => continue,
                };
                if let Some(instance) = self.held_special_item(model, draw, *slot, arm, id, light) {
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
        let mut special = self.block_entities
            .models
            .resolve_special_item(&form.kind, item.path(), placement, &form.transformation, light)?;
        special.texture = dropped_special_texture(draw, special.texture);
        Some(special)
    }

    /// Player-head equipment in the `Head` slot, following 26.2's
    /// `CustomHeadLayer` rather than the hand item's display transform.
    ///
    /// The vanilla layer routes a worn skull through `wornHeadType`, poses it
    /// from `HeadedModel.translateToHead`, then scales the raw skull model by
    /// `1.1875`. A player head's profile selects only the render sheet; it does
    /// not alter the skull model or placement. Other head-slot items are left to
    /// their existing renderers outside this narrowly scoped player-head path.
    fn worn_player_head_special_item(
        &self,
        draw: &EntityDraw,
        light: u8,
    ) -> Option<lodestone_render::BlockEntityInstance> {
        let item = worn_player_head_item(draw)?;
        let wearer = self.entities.models.resolve(
            draw.model_type_path(),
            draw.feet,
            draw.yaw,
            draw.scale,
            &draw.anim,
        )?;
        let mesh = self.entities.models.get(wearer.model)?;
        let head = mesh.skeleton.index_of("head")?;
        let head_transform = *wearer.part_transforms.get(head)?;
        let placement = worn_player_head_placement(head_transform);
        let mut special = self.block_entities.models.resolve_special_item(
            "minecraft:player_head",
            item.path(),
            placement,
            &[],
            light,
        )?;
        special.texture = held_special_texture(draw, EquipmentSlot::Head, special.texture);
        Some(special)
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
        slot: EquipmentSlot,
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
        let mut special = self
            .block_entities
            .models
            .resolve_special_item(&form.kind, item.path(), placement, &form.transformation, light)?;
        special.texture = held_special_texture(draw, slot, special.texture);
        Some(special)
    }

    /// A `minecraft:special` item hanging in an item frame.
    ///
    /// `DisplaySlot::Fixed`, which is `ItemFrameRenderer.extractRenderState`'s
    /// `ItemDisplayContext.FIXED` — the same context the campfire path uses and the
    /// single easiest thing to get wrong here, because every *other* world item
    /// surface is `Ground`. Reusing `Ground` poses a framed chest on its edge.
    ///
    /// `light` is the frame's own — [`item_frame_light`] through
    /// [`framed_content_light`], not the generic eye-height probe — so a chest in
    /// a glow frame is lit the way vanilla lights it rather than as dark as the
    /// wall behind it.
    fn framed_special_item(
        &self,
        model: &ModelRenderer,
        draw: &EntityDraw,
        light: u8,
        camera: &Camera,
    ) -> Option<lodestone_render::BlockEntityInstance> {
        let item = draw.item.as_ref()?;
        let ctx = ItemStateContext::new(DisplaySlot::Fixed);
        let form = model.items.get(item)?.resolve_special(&ctx)?;
        let placement = lodestone_render::framed_item_matrix(
            draw.feet,
            draw.yaw,
            draw.pitch,
            draw.item_frame_rotation,
            draw.invisible,
            &form.display.get(DisplaySlot::Fixed),
        );
        if should_trace_candidate("framed_special_item", draw.id, draw.feet, camera.position) {
            let facing = lodestone_render::entity::item_frame_facing(draw.yaw, draw.pitch)
                .transform_vector3(glam::Vec3::NEG_Z)
                .to_array();
            tracing::debug!(
                target: "pack_trace",
                surface = "framed_special_item",
                entity_id = draw.id,
                protocol_type = %draw.type_path,
                world_pos = ?draw.feet.to_array(),
                yaw = draw.yaw,
                pitch = draw.pitch,
                invisible = draw.invisible,
                attachment_facing = ?facing,
                frame_rotation = draw.item_frame_rotation,
                held_item = %item,
                item_model = ?draw.item_model.as_ref().map(ToString::to_string),
                special_kind = %form.kind,
                selected_display_transform = ?form.display.get(DisplaySlot::Fixed),
                final_transform = ?placement.to_cols_array_2d(),
                rig_origin_plane = ?unit_quad_plane(placement),
                rig_origin_normal = ?unit_quad_normal(placement),
                "nearby render candidate reached framed-special-item draw"
            );
        }
        self.block_entities
            .models
            .resolve_special_item(&form.kind, item.path(), placement, &form.transformation, light)
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
        _device: &wgpu::Device,
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
        crate::block_entities::request_player_head_skins(&skulls);
        let copper_golem_statues = self
            .copper_golem_statue_source
            .copper_golem_statues(eye);
        let bells = self.bell_source.bells(eye);
        let shulkers = self.shulker_source.shulkers(eye);
        let banners = self.banner_source.banners(eye);
        let lecterns = self.lectern_source.lecterns(eye);
        let enchanting_tables = self.enchanting_table_source.enchanting_tables(eye);
        let decorated_pots = self.decorated_pot_source.decorated_pots(eye);
        let conduits = self.conduit_source.conduits(eye);
        // All nine, not any subset: an early return on only `chests`/`skulls`
        // would make a bell in an otherwise chestless, skull-less room draw
        // nothing, which is exactly how this pass would have grown a third
        // island — a shulker box in an empty end-city room is the fourth
        // instance of the same shape, a banner in a village the fifth, a
        // lectern in an otherwise bare village library the sixth, an
        // enchanting table alone in a room the seventh, a decorated pot
        // alone the eighth, and a conduit alone the ninth. Every source
        // added here has to join this condition.
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
            && copper_golem_statues.is_empty()
            && bells.is_empty()
            && shulkers.is_empty()
            && banners.is_empty()
            && lecterns.is_empty()
            && enchanting_tables.is_empty()
            && decorated_pots.is_empty()
            && conduits.is_empty()
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
        // Copper golem statues. A real cuboid rig like chest/skull above —
        // `copper_golem_statue.json` is a total-absence hole — so this joins
        // the same batcher rather than `prepare_item_geometry`.
        instances.extend(copper_golem_statues.iter().filter_map(|spawn| {
            self.block_entities.models.resolve_copper_golem_statue(spawn)
        }));
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

        // Conduits. `resolve_conduit` returns one instance (inactive shell) or
        // four (cage, both wind planes, the camera-facing eye) per conduit —
        // `Vec::extend` folds either shape in exactly like `decorated_pots`'
        // `.flatten()` above. The billboard orientation is computed once per
        // frame and shared by every conduit's eye, the same way
        // `prepare_orbs` shares one `orientation` across every orb.
        if !conduits.is_empty() {
            let orientation = lodestone_render::entity::camera_orientation(camera.view_matrix());
            for spawn in &conduits {
                instances.extend(self.block_entities.models.resolve_conduit(spawn, orientation));
            }
        }

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
                let rgb = lodestone_render::gamma_rgb_to_bytes(layer.color);
                let Some(instances) = stage_instances_tinted(
                    &self.instance_arena,
                    &[layer.transform],
                    &[u32::from(layer.light)],
                    &[InstanceTint::rgb(rgb)],
                ) else {
                    continue;
                };
                banner_layers.push(BannerLayerDrawBatch {
                    pattern: pattern.to_string(),
                    instances,
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
                texture: batch.texture.clone(),
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
                    .map(|p| {
                        stage_instances_tinted(
                            &self.instance_arena,
                            p,
                            &batch.lights,
                            &batch.tints,
                        )
                    })
                    .collect(),
            })
            .collect();
        (opaque, banner_layers)
    }
}

#[cfg(test)]
mod tests {

    /// **The tables are binary-searched, so their sortedness is load-bearing
    /// and completely invisible** — an out-of-order row does not fail to
    /// compile, it silently reports [`SHADOW_RADIUS_FALLBACK`] for a type
    /// that has a real value, which looks exactly like the flat-`0.5`
    /// behaviour this table replaced.
    #[test]
    fn the_shadow_tables_are_sorted_and_carry_no_duplicate_types() {
        for (label, table) in [("SHADOW_RADII", SHADOW_RADII), ("SHADOW_STRENGTHS", SHADOW_STRENGTHS)] {
            let out_of_order: Vec<_> = table
                .windows(2)
                .filter(|w| w[0].0 >= w[1].0)
                .map(|w| (w[0].0, w[1].0))
                .collect();
            assert!(
                out_of_order.is_empty(),
                "{label} must be sorted by type path with no duplicates — \
                 `shadow_radius`/`shadow_strength` binary-search it, so these \
                 rows are unreachable: {out_of_order:?}"
            );
        }
    }

    #[test]
    fn specialised_entity_types_do_not_pollute_missing_body_diagnostics() {
        for specialised in [
            "item",
            "item_frame",
            "glow_item_frame",
            "text_display",
            "item_display",
            "block_display",
            "experience_orb",
            "painting",
            "falling_block",
            "tnt",
            "snowball",
            "dragon_fireball",
            "fishing_bobber",
            "firework_rocket",
            "ominous_item_spawner",
        ] {
            assert!(
                !ordinary_model_dispatch_is_unhandled(specialised),
                "{specialised} has a dedicated renderer and must not be reported"
            );
        }
        assert!(ordinary_model_dispatch_is_unhandled("zombie"));
        assert!(ordinary_model_dispatch_is_unhandled("unknown_server_entity"));
    }

    /// **The discriminating rows, not a smoke test.** Each pair below was
    /// chosen because the *old* flat `0.5` and the real value differ, and in
    /// both directions — a table that had silently fallen back to the
    /// constant would fail every one of them.
    #[test]
    fn shadow_radius_reports_vanillas_own_per_species_value() {
        // `EntityRenderer.shadowRadius`'s field default: vanilla draws no
        // shadow at all for these, where the flat constant drew a
        // player-sized disc.
        for none in ["arrow", "item_frame", "painting", "armor_stand", "shulker", "marker"] {
            assert_eq!(shadow_radius(none), 0.0, "{none} casts no shadow in vanilla");
        }
        // Smaller than the old constant…
        assert_eq!(shadow_radius("item"), 0.15);
        assert_eq!(shadow_radius("experience_orb"), 0.15);
        assert_eq!(shadow_radius("chicken"), 0.3);
        assert_eq!(shadow_radius("tadpole"), 0.14);
        // …and larger.
        assert_eq!(shadow_radius("cow"), 0.7);
        assert_eq!(shadow_radius("oak_boat"), 0.8);
        assert_eq!(shadow_radius("spider"), 0.8);
        assert_eq!(shadow_radius("ghast"), 1.5);
        assert_eq!(shadow_radius("giant"), 3.0);
        // The rows that survive at the old value, so the gate is not merely
        // asserting "everything changed".
        assert_eq!(shadow_radius("player"), 0.5);
        assert_eq!(shadow_radius("zombie"), 0.5);
        // An unlisted type falls back rather than vanishing — see
        // `SHADOW_RADIUS_FALLBACK`'s doc for why that is not vanilla's `0.0`.
        assert_eq!(shadow_radius("not_a_real_entity"), SHADOW_RADIUS_FALLBACK);

        // `shadowStrength` has exactly two overrides in 26.2.
        assert_eq!(shadow_strength("item"), 0.75);
        assert_eq!(shadow_strength("experience_orb"), 0.75);
        assert_eq!(shadow_strength("player"), 1.0);
        assert_eq!(shadow_strength("not_a_real_entity"), 1.0);
    }
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
            type_path: Arc::from(type_path),
            item: None,
            item_model: None,
            item_skin: None,
            main_arm_left: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_skin: Vec::new(),
            equipment_trim: Vec::new(),
            wool: None,
            block_state: None,
            item_frame_rotation: 0,
            count: 1,
            foil: false,
            item_dyed_color: None,
            item_potion_color: None,
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
            invisible: false,
            armor_stand: None,
            player_skin: None,
            variant_sheet: None,
            // A flame subject, not an orb.
            experience_orb_value: None,
            cape_sway: (0.0, 0.0, 0.0),
            painting: None,
            firework: None,
            projectile_owner: None,
        }
    }

    #[test]
    fn held_player_head_special_draw_uses_its_slots_profile_skin() {
        let url: Arc<str> = Arc::from("https://example.invalid/custom-head.png");
        let mut draw = subject("player_wide", 64.0, 1.0, false);
        draw.equipment_skin.push((EquipmentSlot::MainHand, Arc::clone(&url)));

        let texture = held_special_texture(
            &draw,
            EquipmentSlot::MainHand,
            lodestone_render::BlockEntityTexture::Static("entity/player/wide/steve"),
        );
        assert_eq!(
            texture,
            lodestone_render::BlockEntityTexture::PlayerSkin(url),
            "the held special-item boundary must replace a player head's static Steve sheet"
        );
    }

    #[test]
    fn worn_player_head_is_head_slot_only_and_uses_custom_head_layer_scale() {
        let mut draw = subject("player_wide", 64.0, 1.0, false);
        let player_head = "minecraft:player_head"
            .parse()
            .expect("valid player-head item id");
        draw.equipment.push((EquipmentSlot::Head, player_head));

        assert!(
            worn_player_head_item(&draw).is_some(),
            "a player head in the head slot must enter the worn-head special pass"
        );
        let pose = glam::Mat4::from_translation(glam::Vec3::new(3.0, 5.0, 7.0));
        assert_eq!(
            worn_player_head_placement(pose),
            pose * glam::Mat4::from_scale(glam::Vec3::splat(1.1875)),
            "CustomHeadLayer scales the raw skull after translateToHead"
        );

        let mut hand_only = draw.clone();
        hand_only.equipment[0].0 = EquipmentSlot::MainHand;
        assert!(
            worn_player_head_item(&hand_only).is_none(),
            "a hand head belongs to held_special_item, not the worn-head layer"
        );
    }

    #[test]
    fn dropped_player_head_special_draw_uses_the_stacks_profile_skin() {
        let url: Arc<str> = Arc::from("https://example.invalid/dropped-custom-head.png");
        let mut draw = subject("item", 64.0, 1.0, false);
        draw.item_skin = Some(Arc::clone(&url));

        let texture = dropped_special_texture(
            &draw,
            lodestone_render::BlockEntityTexture::Static("entity/player/wide/steve"),
        );
        assert_eq!(
            texture,
            lodestone_render::BlockEntityTexture::PlayerSkin(url),
            "the dropped special-item boundary must replace a player head's static Steve sheet"
        );
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

    /// `hide_armor_stand_parts` collapses exactly the named parts — and no
    /// others — to a degenerate, zero-scale matrix.
    ///
    /// Pure geometry, no GPU: this inspects `EntityInstance::part_transforms`
    /// directly rather than reading back pixels. "Did this specific named
    /// part's own matrix degenerate, and no other part's" is not a claim a
    /// full-frame pixel readback can localise this precisely — the visible
    /// *consequence* (a hologram's base plate and arms contributing no
    /// pixels while its head/body/legs still do) is covered separately by
    /// `tests/armor_stand_hologram_pixels.rs`'s rasterized gate. Two
    /// different instruments for two different claims.
    ///
    /// Three configurations, not two: `ArmorStandFlags::default()` already
    /// has `show_arms: false` (vanilla's own default — an armour stand's
    /// arms are hidden unless explicitly shown), so "all flags false" is
    /// *not* the all-visible case here, unlike almost every other bool flag
    /// in this codebase. Using it as the sole baseline would have made the
    /// arms half of this test pass by accident even if `show_arms` were
    /// read backwards. `all_visible` (`show_arms: true`) is the real
    /// nothing-hidden baseline instead.
    #[test]
    fn hide_armor_stand_parts_collapses_only_the_named_parts() {
        let models = lodestone_render::EntityModelSet::load();
        let mesh = models
            .get("armor_stand")
            .expect("armor_stand must be in the corpus");
        let base_plate = mesh
            .skeleton
            .index_of("base_plate")
            .expect("armor_stand must have a base_plate part");
        let left_arm = mesh
            .skeleton
            .index_of("left_arm")
            .expect("armor_stand must have a left_arm part");
        let right_arm = mesh
            .skeleton
            .index_of("right_arm")
            .expect("armor_stand must have a right_arm part");
        let head = mesh
            .skeleton
            .index_of("head")
            .expect("armor_stand must have a head part");

        // A part's own linear (3x3) determinant collapses to exactly `0.0`
        // under `M * diag(0, 0, 0, 1)` — the affine matrix's 4x4 determinant
        // reduces to the linear part's, since the bottom row stays
        // `(0, 0, 0, 1)` — while an ordinary rotate+translate bone keeps a
        // determinant with magnitude close to `1.0`. `1e-6` separates the two
        // by many orders of magnitude; there is no near-miss case here.
        let degenerate = |m: glam::Mat4| m.determinant().abs() < 1e-6;

        let resolve = || {
            models
                .resolve(
                    "armor_stand",
                    glam::Vec3::ZERO,
                    0.0,
                    1.0,
                    &lodestone_render::AnimInput::REST,
                )
                .expect("armor_stand must resolve")
        };

        let mut mismatches: Vec<String> = Vec::new();
        let mut check = |label: &str, flags: lodestone_ecs::entity::ArmorStandFlags,
                          expect_degenerate: &[(&str, usize, bool)]| {
            let mut instance = resolve();
            hide_armor_stand_parts(&mut instance, mesh, flags);
            for (part_name, index, want_degenerate) in expect_degenerate {
                let got = degenerate(instance.part_transforms[*index]);
                if got != *want_degenerate {
                    mismatches.push(format!(
                        "{label}: {part_name} degenerate={got}, expected {want_degenerate}"
                    ));
                }
            }
        };

        // Nothing hidden: proves the function does not degenerate parts it
        // was not told to.
        check(
            "all_visible",
            lodestone_ecs::entity::ArmorStandFlags {
                small: false,
                show_arms: true,
                no_base_plate: false,
                marker: false,
            },
            &[
                ("base_plate", base_plate, false),
                ("left_arm", left_arm, false),
                ("right_arm", right_arm, false),
                ("head", head, false),
            ],
        );

        // `no_base_plate` alone: only the base plate degenerates, arms and
        // head (an unrelated part with no flag of its own) do not.
        check(
            "no_base_plate_only",
            lodestone_ecs::entity::ArmorStandFlags {
                small: false,
                show_arms: true,
                no_base_plate: true,
                marker: false,
            },
            &[
                ("base_plate", base_plate, true),
                ("left_arm", left_arm, false),
                ("right_arm", right_arm, false),
                ("head", head, false),
            ],
        );

        // The real vanilla default (`ArmorStandFlags::default()`): arms
        // hidden, base plate and head untouched — the discriminating case
        // this test exists for, since it is neither "all hidden" nor
        // "nothing hidden".
        check(
            "default_flags",
            lodestone_ecs::entity::ArmorStandFlags::default(),
            &[
                ("base_plate", base_plate, false),
                ("left_arm", left_arm, true),
                ("right_arm", right_arm, true),
                ("head", head, false),
            ],
        );

        assert!(
            mismatches.is_empty(),
            "hide_armor_stand_parts mismatches:\n{}",
            mismatches.join("\n")
        );
    }
}
