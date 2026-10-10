//! The first-person hand pass: both hands — the main hand's held item or bare
//! arm, and the off hand's held item — drawn in one render pass with the depth
//! buffer cleared first, as the reference client does before drawing hands.
//! The per-hand state (which stack each hand shows, how far each is lowered)
//! is advanced per game tick by `Sim` and arrives here as a finished
//! [`FirstPersonHandsFrame`]; see `docs/held-items.md`.
use lodestone_assets::ResourceLocation;
use lodestone_data::entity_type::EntityType;
use lodestone_render::{
    Camera, CameraUniform, EntityCameraUniform, GpuEntityModel, GpuModelMesh, ItemStateContext,
    entity::{
        Arm, FirstPersonItemUse, first_person_arm_parts, first_person_arm_pose_with_equip,
        first_person_item_mesh_with_use, hand_projection, hand_transform, model_for_type,
    },
    fog::FogUniform,
    update_model_shared_camera_buffer, upload_instances, upload_instances_tinted,
};

use crate::camera_rig::{BobFrame, ViewLagFrame};

use super::{FirstPersonHandsFrame, MainHandItem, RenderState, RenderStats};

// ---------------------------------------------------------------------------
// The walk/hurt bob reaches the hand (follow-up)
// ---------------------------------------------------------------------------

/// A `damage_tilt_strength` of zero, for the gates below that isolate a single
/// walk-bob term and need the hurt tilt provably inert.
///
/// **This used to be `HAND_HURT_TILT_STRENGTH`, a *production* constant holding
/// the hurt tilt off, and its stated blocker was already stale when it was read.**
/// The blocker was that `Sim::bob_frame` returned `BobFrame::default()` whole-cloth
/// when View Bobbing was off, zeroing `hurt`/`hurt_dir_degrees` along with the walk
/// terms — so a nonzero strength would have muted the damage tilt for anyone who
/// turned View Bobbing off, which the reference client does not do (it applies
/// the hurt tilt outside the View Bobbing check). That was true when written and
/// had since been fixed: `bob_frame` now zeroes **only** `walk_phase`/`bob` and passes the
/// hurt half through untouched. The hand therefore draws the real strength, and
/// this constant survives only as the gates' zero anchor.
#[cfg(test)]
const NO_DAMAGE_TILT: f32 = 0.0;

/// Where this frame's walk/hurt bob comes from, for the first-person hand pass
/// — polled once per frame like [`super::HandSwingSource`].
///
/// # Why the hand needs its *own* source rather than reading `camera`
///
/// `camera: &Camera`, passed into [`RenderState::prepare_first_person_hands`],
/// is already [`crate::sim::camera`]'s **folded** render camera —
/// `Sim::render_camera` bakes
/// [`BobFrame::eye_transform`] into the camera's position/yaw/pitch via
/// [`crate::camera_rig::bobbed_camera`], mirroring vanilla's own
/// level-render step's post-multiply of the bob stack onto the projection —
/// the bob folded into the
/// **world's** projection matrix.
///
/// Vanilla's own hand-render path
/// does not read that folded value at all. It builds a **second, independent**
/// pose stack, seeds it with the inverse of the camera's *unbobbed* view
/// rotation — and then applies the hurt/death roll and walk bob to *that*
/// stack a second time. The GPU's own model-view is pushed as the
/// very same unbobbed view matrix, so at draw time the
/// inverse cancels it exactly and the hand's net pose is **just the bob
/// matrix**, with no trace of the world's position and none of
/// [`crate::camera_rig::bobbed_camera`]'s lossy roll-dropping decomposition —
/// that fold's own doc names roll as the one term a folded `Camera` cannot
/// carry, and the hand must not inherit that loss. [`hand_view_proj`] is where
/// that decomposition is sidestepped entirely: the raw matrix is multiplied
/// straight into the projection, never folded through a `Camera`.
///
/// So: a fresh, independent copy of the same [`BobFrame`], not a value
/// inherited from `camera`. That is *why* a source is needed at all — the
/// value has to reach here from `Sim::bob_frame()`,
/// which nothing below the GPU boundary can read.
pub(super) struct HandBobSource(pub(super) Option<Box<dyn Fn() -> BobFrame + Send + Sync>>);

impl HandBobSource {
    /// This frame's bob, or [`BobFrame::default`] — the identity: no dip, no
    /// sway, no tilt — until a source is installed. That default reproduces
    /// exactly the arm's pre-existing (unbobbed) behaviour, the same guarantee
    /// `HandSwingSource`'s unset state gives the swing.
    #[must_use]
    pub(super) fn value(&self) -> BobFrame {
        self.0.as_ref().map_or_else(BobFrame::default, |f| f())
    }
}

impl Default for HandBobSource {
    fn default() -> Self {
        Self(None)
    }
}

impl std::fmt::Debug for HandBobSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("HandBobSource")
            .field(&if self.0.is_some() { "set" } else { "rest" })
            .finish()
    }
}

/// Per-frame view-lag source shared by the first-person hand and the local
/// third-person held-item attachment. The value is sampled by the frame bridge,
/// so both consumers see the same partial-tick residual.
pub(super) struct ViewLagSource(pub(super) Option<Box<dyn Fn() -> ViewLagFrame + Send + Sync>>);

impl ViewLagSource {
    #[must_use]
    pub(super) fn value(&self) -> ViewLagFrame {
        self.0.as_ref().map_or_else(ViewLagFrame::default, |f| f())
    }
}

impl Default for ViewLagSource {
    fn default() -> Self {
        Self(None)
    }
}

impl std::fmt::Debug for ViewLagSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ViewLagSource")
            .field(&if self.0.is_some() { "set" } else { "rest" })
            .finish()
    }
}

/// The hand pass's whole camera-space transform: [`hand_projection`] composed
/// with a fresh copy of the walk/hurt bob — vanilla's own second application
/// of it, described in full on
/// [`HandBobSource`]'s doc.
///
/// **Post-multiplied, matching vanilla's own projection-times-bob-stack
/// multiply** — the bob lands between the projection
/// and the already-camera-space arm/item pose, exactly where vanilla's own
/// `projection · model-view · pose` puts it once the view rotation cancels
/// (see [`hand_projection`]'s own doc for that cancellation). Pre-multiplying
/// instead would apply the bob in *clip* space and scale its magnitude by
/// whatever the projection does to depth — a different, wrong transform that
/// happens to also move the arm, which is exactly the kind of bug a gate that
/// only asserts "it moved" cannot catch.
///
/// A pure function of its inputs (no GPU handle), so it is unit-testable
/// against hand-derived vanilla numbers with no adapter — see the `tests`
/// module below.
#[must_use]
fn hand_view_proj(
    aspect: f32,
    bob: BobFrame,
    view_lag: ViewLagFrame,
    damage_tilt_strength: f32,
) -> glam::Mat4 {
    hand_projection(aspect)
        * view_lag.hand_transform()
        * bob.eye_transform(damage_tilt_strength)
}

/// What one first-person hand draws this frame: the held item's model, or (main
/// hand only) the bare arm. **Never both** — see
/// [`RenderState::prepare_first_person_hands`]: a hand holding a stack draws
/// that stack and no arm.
pub(super) enum FirstPersonHand<'a> {
    /// The held item, meshed camera-space and drawn through the *model* pipeline
    /// with the model pass's own `hand_cam_bind_group`. The `bool` is the
    /// enchantment-foil flag: when `true`, [`RenderState::draw_first_person_hands`]
    /// re-rasterises the same mesh through the glint pipeline in the same pass.
    Item(GpuModelMesh, bool),
    /// A held **filled map**: one quad drawn through the same model
    /// pipeline as [`Self::Item`], with group 1 swapped from the block atlas to the
    /// map's own 128×128 texture, then its decoration quads with group 1 swapped
    /// to the decoration sheet. Each bind group travels with its mesh because the
    /// two are meaningless apart — see `super::maps`.
    Map(super::maps::HeldMap),
    /// A held **block-entity rig** — a chest, a shulker box, a skull/head: an item
    /// whose definition resolves to a `minecraft:special` node and whose geometry is
    /// therefore a block-entity renderer rather than any baked model.
    ///
    /// # Why this cannot be [`Self::Item`]
    ///
    /// Not because of the geometry — a `BlockEntityMesh`'s part transforms take an
    /// arbitrary placement matrix, so
    /// [`first_person_item_matrix`](lodestone_render::entity::first_person_item_matrix)
    /// slots in exactly where the GUI's `gui_item_pose` and the world's
    /// `block_entity_placement_matrix` go. It is the **texture**: a chest's UVs are
    /// `[0,1]` against a standalone 64×64 `entity/chest/*.png`, while the model
    /// pipeline [`Self::Item`] draws through binds the stitched *block* atlas, which
    /// contains nothing under `textures/entity/`. Routing a chest through it samples
    /// arbitrary block texels.
    ///
    /// And the model pipeline cannot simply bind a second texture: it spends all
    /// four bind groups (camera / atlas / palette / anim), which is wgpu's portable
    /// `max_bind_groups` floor. So this draws through the **block-entity pass's**
    /// `EntityPipeline`, which spends two — the same reasoning
    /// `hud/item_icon.rs`'s `SpecialIcons` records for the inventory slot, reaching
    /// the same conclusion from the other end of the frame.
    ///
    /// No foil flag: the glint pipeline rasterises a `GpuModelMesh` through the
    /// model shader's vertex layout, and this is instanced entity geometry. An
    /// enchanted held chest therefore draws unglinted — the same shortfall the
    /// inventory slot has for the same reason.
    ///
    /// A `Vec` rather than a single draw because the banner rig is two meshes
    /// (pole/bar and flag, both drawn untinted) sharing one placement — see
    /// [`RenderState::prepare_special_hand`] and
    /// [`lodestone_render::banner_item_rig`]'s doc. Every other kind still
    /// pushes exactly one.
    ///
    /// The second element is the banner's own translucent pattern-layer
    /// draws (base mask plus every loom pattern, in order) — empty for every
    /// non-banner kind. Drawn in a second pass, over the same flag geometry,
    /// through [`super::block_entities::BlockEntityRenderer::banner_layer_pipeline`]
    /// — see [`RenderState::draw_first_person_hands`]'s `Special` arm.
    Special(Vec<SpecialHandDraw<'a>>, Vec<HandBannerLayerDraw>),
    /// The bare arm, drawn through the *entity* pipeline.
    Arm(FirstPersonArm<'a>),
}

/// A held block-entity rig's draw for one frame: the uploaded mesh and sheet
/// (borrowed — both are uploaded once at startup) and one single-instance buffer
/// per part.
///
/// One buffer per part rather than one for the whole rig, because a
/// `BlockEntityMesh` is *part*-instanced: every part carries its own world matrix
/// so an animation can move one of them. Nothing animates here (a held chest's lid
/// is shut — a held chest's rig takes no lid angle), but the mesh's draw shape is
/// the same either way and diverging from it would mean a second draw loop.
pub(super) struct SpecialHandDraw<'a> {
    model: &'a GpuEntityModel,
    texture: &'a wgpu::BindGroup,
    /// Already baked with a per-instance tint — `[255, 255, 255]` (a no-op)
    /// for every kind, banner included: colour rides the separate translucent
    /// pattern-layer draws now, not this mesh's own instance tint. See
    /// [`RenderState::build_special_hand_draw`].
    parts: Vec<(lodestone_render::entity::PartRange, wgpu::Buffer)>,
}

/// Which mesh and which mask map [`HandBannerLayerDraw::family`] resolves
/// against — a banner's layers redraw only the `"flag"` part of
/// `"banner_flag"`, a shield's redraw the *whole* `"shield"` mesh (both
/// `plate` and `handle` — see [`lodestone_assets::block_entity_models::
/// shield_model`]'s doc for why a shield has no separate flag-shaped part to
/// single out).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HandPatternFamily {
    Banner,
    Shield,
}

/// One pattern-mask layer to draw over a held banner's flag or a held
/// shield's plate, in the hand pass's own camera space — the hand-rig
/// sibling of `super::block_entities::BannerLayerDrawBatch`, which does the
/// identical job for a *placed* banner in world space. See that type's doc
/// for why this is a flat, strictly-ordered list rather than a batch: each
/// layer is one draw of the *same* geometry with its own mask and its own
/// colour. A single [`Self::family`] carried per-layer rather than once per
/// list keeps the field on the record it actually describes, but every
/// layer in one call's returned `Vec` is the same family — a stack is a
/// banner or a shield, never both — so a draw-site reads it once off the
/// first entry.
pub(super) struct HandBannerLayerDraw {
    /// Which mesh/mask-map this layer belongs to.
    family: HandPatternFamily,
    /// Bare pattern asset id, keying
    /// [`super::block_entities::BlockEntityRenderer::banner_patterns`] or
    /// `::shield_patterns`, per [`Self::family`].
    pattern: String,
    /// A one-instance buffer carrying the mesh's own world matrix (shared
    /// with [`SpecialHandDraw`]'s corresponding entry), this layer's
    /// gamma-space colour as the instance tint, and the hand's light.
    instances: wgpu::Buffer,
}

/// The first-person arm's draw for one frame: the uploaded `player_wide` mesh and
/// texture (borrowed — they are uploaded once at startup), plus one
/// single-instance buffer per drawn part.
///
/// Only the arm and its sleeve are listed. Both carry the *same* matrix, so this
/// is two draw calls over one pose and not a pose per part.
pub(super) struct FirstPersonArm<'a> {
    model: &'a GpuEntityModel,
    texture: &'a wgpu::BindGroup,
    parts: Vec<(lodestone_render::entity::PartRange, wgpu::Buffer)>,
}

/// Both first-person hands' draws for one frame. Either may be `None`: an
/// empty off hand draws nothing, a hand hidden by a drawn bow draws nothing,
/// and a main hand whose rig failed to load draws nothing.
pub(super) struct FirstPersonHands<'a> {
    pub(super) main: Option<FirstPersonHand<'a>>,
    pub(super) off: Option<FirstPersonHand<'a>>,
}

impl FirstPersonHands<'_> {
    /// Whether either hand has anything to draw.
    pub(super) fn is_empty(&self) -> bool {
        self.main.is_none() && self.off.is_none()
    }
}

/// Which hand a [`HandPose`] belongs to and how it is posed this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HandPose {
    /// `true` for the main hand. Only the main hand can draw the bare arm, a
    /// map, or an item-use pose, and only it swings (this client never swings
    /// the off hand).
    pub(crate) is_main: bool,
    /// The arm the hand is drawn on: the main arm, or its opposite.
    pub(crate) arm: Arm,
    /// Swing progress for this hand, `0.0` for a hand that is not swinging.
    pub(crate) attack_anim: f32,
    /// How far below rest the hand is drawn.
    pub(crate) inverse_arm_height: f32,
}

/// The two hand poses a [`FirstPersonHandsFrame`] asks for this frame, main
/// hand first, skipping a hand the frame does not draw. A pure function so
/// the path from the installed frame to the pose matrices is testable with no
/// GPU: `prepare_first_person_hands` draws exactly these.
#[must_use]
pub(crate) fn hand_poses(frame: &FirstPersonHandsFrame, swing: f32) -> Vec<(HandPose, Option<&MainHandItem>)> {
    let mut poses = Vec::with_capacity(2);
    if frame.main.drawn {
        poses.push((
            HandPose {
                is_main: true,
                arm: frame.main_arm,
                attack_anim: swing,
                inverse_arm_height: frame.main.inverse_arm_height,
            },
            frame.main.item.as_ref(),
        ));
    }
    if frame.off.drawn {
        poses.push((
            HandPose {
                is_main: false,
                arm: frame.off_arm(),
                attack_anim: 0.0,
                inverse_arm_height: frame.off.inverse_arm_height,
            },
            frame.off.item.as_ref(),
        ));
    }
    poses
}

impl RenderState {
    /// Build this frame's first-person hand draws — the main hand and the off
    /// hand, each from [`hand_poses`].
    ///
    /// # What each hand draws
    ///
    /// A hand showing a stack draws that stack and **no arm**; drawing both
    /// puts the item inside the wrist. An empty main hand draws the bare arm;
    /// an empty off hand draws nothing at all. That asymmetry is the reference
    /// client's, and it is why the off hand only ever appears when it holds
    /// something.
    ///
    /// The off hand is the main hand's pose mirrored: the same chain with the
    /// arm sign flipped and the stack's own `firstperson_lefthand` display
    /// transform (or the mirrored right-hand one, via the left-hand fallback).
    /// It does not swing — this client only ever swings the main hand — and
    /// it takes no item-use pose, because only the main hand is ever used.
    ///
    /// # Shared by both hands
    ///
    /// One group-0 uniform: [`hand_projection`] composed with the hand bob,
    /// **no view matrix**. The pose is already camera-space (the reference
    /// client cancels the view rotation exactly before posing hands), so the
    /// ordinary view-projection would park the hands at the world origin. One
    /// light sample, at the eye. One pass, with depth cleared once before
    /// either hand (see [`Self::draw_first_person_hands`]).
    ///
    /// # Not gated, and why that is right
    ///
    /// `RenderState::render` only runs in-world, and the caller already skips
    /// this whenever a third-person body or spectator view draws instead, so
    /// "first person, in a world" is exactly when this runs.
    ///
    /// The bare arm wears our own skin on our own rig — see the `local_skin`
    /// resolve in [`Self::prepare_hand`].
    pub(super) fn prepare_first_person_hands<'a>(
        &'a self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
    ) -> FirstPersonHands<'a> {
        // Written before either hand can return, so no uniform is left holding
        // a stale projection. The returned matrix is the very `view_proj` the
        // base item draw uses, which the glint draw must reuse verbatim
        // (depth-`EQUAL`).
        let view_proj = self.write_hand_camera(queue, camera);
        let mut hands = FirstPersonHands { main: None, off: None };
        for (pose, item) in hand_poses(&self.hands, self.hand_swing.value()) {
            let draw = self.prepare_hand(device, queue, camera, view_proj, pose, item);
            if pose.is_main {
                hands.main = draw;
            } else {
                hands.off = draw;
            }
        }
        hands
    }

    /// One hand's draw: a held map (main hand only), the held item's baked
    /// model, a held block-entity rig, or — for an empty main hand, or a main
    /// hand whose item has no drawable form — the bare arm.
    fn prepare_hand<'a>(
        &'a self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
        view_proj: [[f32; 4]; 4],
        pose: HandPose,
        item: Option<&MainHandItem>,
    ) -> Option<FirstPersonHand<'a>> {
        let HandPose {
            is_main,
            arm,
            attack_anim,
            inverse_arm_height,
        } = pose;

        // A held filled map draws as a textured quad, not as the item's flat
        // sprite (which has no terrain on it). Centred in two hands when it is
        // the main hand's and the off hand is empty, else off to the side of
        // whichever hand holds it.
        if let Some(held) = item {
            let stance = if is_main && self.hands.off.item.is_none() {
                super::maps::HeldMapStance::TwoHanded { pitch_degrees: camera.pitch }
            } else {
                super::maps::HeldMapStance::OneHanded { right_arm: arm == Arm::Right }
            };
            if let Some(map) =
                self.prepare_held_map(device, queue, held, is_main, stance, attack_anim, inverse_arm_height)
            {
                return Some(FirstPersonHand::Map(map));
            }
        }

        // The use state belongs to the main hand alone: it is the only hand
        // this client ever uses.
        let use_state = if is_main {
            self.item_use.sample()
        } else {
            super::ItemUseState::default()
        };
        // `arm.display_slot(true)` — the same slot `hand_transform` below reads
        // the pose from, so the resolved variant and its transform cannot
        // disagree about which hand this is. Resolving with the live use state
        // is what selects a bow's pulling models.
        let hand_ctx = ItemStateContext::new(arm.display_slot(true))
            .with_use(use_state.using, use_state.ticks)
            .with_custom_model_data(item.and_then(|i| i.custom_model_data).unwrap_or(0) as f32);
        if let Some(held) = item
            && let Some(model) = self.model.as_ref()
            && let Some(forms) = model.items.get(&held.item)
        {
            log_diamond_sword_model_resolution(&held.item, &hand_ctx, forms);
            if let Some(geometry) = forms.resolve(&hand_ctx) {
                // The first-person slot. The third-person one is a different
                // rotation and scale and puts the item at a plausible but
                // wrong angle rather than off screen.
                let transform = hand_transform(&geometry.display, arm, true);
                // A use animation replaces the whole resting pose and takes no
                // swing. A drawn bow has its own aimed pose; a consumable its
                // own dip toward the mouth.
                let item_use = if use_state.using
                    && held.item.namespace() == "minecraft"
                    && held.item.path() == "bow"
                {
                    Some(FirstPersonItemUse::Bow {
                        held_ticks: use_state.ticks as f32,
                    })
                } else {
                    use_state.eat.map(|(curr_usage_time, use_duration)| {
                        FirstPersonItemUse::Eat {
                            curr_usage_time,
                            use_duration,
                        }
                    })
                };
                let mut mesh = first_person_item_mesh_with_use(
                    &geometry.quads,
                    geometry.gui_light,
                    arm,
                    attack_anim,
                    inverse_arm_height,
                    &transform,
                    u8::try_from(self.hand_light(camera)).unwrap_or(u8::MAX),
                    item_use,
                );
                // The stack's real dye/potion tint — a no-op unless it is a
                // dyed leather piece or a mixed potion.
                let live_components = lodestone_model::item::ItemComponents {
                    dyed_color: held.dyed_color,
                    potion_color: held.potion_color,
                    ..Default::default()
                };
                lodestone_render::stamp_live_item_tint(
                    &mut mesh,
                    &geometry.quads,
                    &geometry.live_tints,
                    &live_components,
                );
                if let Some(gpu) = GpuModelMesh::upload(device, &mesh) {
                    // An enchanted item gets the glint second draw. Both hands
                    // write the same matrix, so a second write is harmless.
                    if held.foil {
                        self.write_glint_uniform(queue, view_proj);
                    }
                    return Some(FirstPersonHand::Item(gpu, held.foil));
                }
            }
        }

        // A held block-entity rig — a chest, a shulker box, a skull. After the
        // baked-model branch (a definition reaching both draws the model
        // first) and before the bare arm (which is the *empty* hand: falling
        // through to it with a chest in hand draws an empty arm).
        if let Some(held) = item
            && let Some(model) = self.model.as_ref()
            && let Some(form) = model.items.get(&held.item).and_then(|v| v.resolve_special(&hand_ctx))
            && let Some((draws, layers)) = self.prepare_special_hand(device, held, form, pose, camera)
        {
            return Some(FirstPersonHand::Special(draws, layers));
        }

        // Only the main hand ever draws the bare arm. An off hand with nothing
        // drawable draws nothing.
        if !is_main {
            return None;
        }

        // The bare arm wears **our own** skin, resolved by
        // `Sim::local_player_skin` and published through
        // `remote_skins::local()`. It cannot come through
        // `ThirdPersonBodyState`: this pass runs precisely on the frames that
        // state is `None`, so the two are mutually exclusive by construction.
        let local_skin = crate::remote_skins::local();
        // Rig first, and it must agree with the sheet: a slim-authored sheet on
        // the wide rig puts the arm UVs a texel out.
        let rig = local_skin.as_ref().map_or_else(
            || model_for_type(EntityType::Player).map(|entry| entry.name),
            |skin| Some(lodestone_render::entity::player_model_name(skin.model.is_slim())),
        )?;
        let mesh = self.entities.models.get(rig)?;
        let gpu = self.entities.gpu_models.get(rig)?;
        // The fetched sheet, then the uuid-hash built-in identity, then the
        // model's own sheet — the ladder the world entity pass uses for every
        // other player. A miss on the first two is normal.
        let texture = local_skin
            .as_ref()
            .and_then(|skin| self.entities.player_skins.get(&skin.url))
            .or_else(|| {
                local_skin
                    .as_ref()
                    .and_then(|skin| self.entities.variant_textures.get(skin.default_sheet))
            })
            .or_else(|| self.entities.textures.get(rig))?;
        // The arm takes the same lowering as an item would, so putting an item
        // away lowers the item and then raises the arm as one motion.
        let pose = first_person_arm_pose_with_equip(mesh, arm, attack_anim, inverse_arm_height)?;

        let light = self.hand_light(camera);

        let parts: Vec<(lodestone_render::entity::PartRange, wgpu::Buffer)> =
            first_person_arm_parts(mesh, arm)
                .into_iter()
                .filter_map(|index| {
                    let range = *gpu.parts.get(index)?;
                    if range.index_count == 0 {
                        return None;
                    }
                    // One instance, and the *same* matrix for arm and sleeve —
                    // the sleeve is a zero-pose child of the arm.
                    let buffer = upload_instances(device, &[pose], &[light])?;
                    Some((range, buffer))
                })
                .collect();
        if parts.is_empty() {
            return None;
        }

        Some(FirstPersonHand::Arm(FirstPersonArm {
            model: gpu,
            texture,
            parts,
        }))
    }

    /// Build the held block-entity rig's draw, or `None` when this item's `kind` has
    /// no ported rig (six of the ten do not) or the pack shipped no sheet for it.
    ///
    /// # It is the same rig the world chest uses, resolved by the same function
    ///
    /// `special_item_rig` is the single owner of `kind` + item path → (model, sheet),
    /// shared with the inventory pass — so a held chest, a placed chest and a chest
    /// in a hotbar slot cannot disagree about which mesh or which sheet they are.
    /// Anything keyed on the item id alone would need 91 arms; `kind` says *what
    /// rig*, the path says *which sheet within it*.
    ///
    /// # The pose is the ordinary held-item pose, not a special one
    ///
    /// `first_person_item_matrix(arm, swing, dip, transform)` — byte-for-byte the
    /// matrix the `Item` branch feeds `first_person_item_mesh`, and the `transform`
    /// comes from the `base` model's own `display` map. That is what makes the two
    /// branches agree about where the hand is: a chest and a pickaxe swing on the
    /// same arc, dip on the same ramp, and sit at whatever offset the template's
    /// first-person display slot asks for — in either hand.
    ///
    /// **The display slot is resolved by the caller and passed in via `form`**,
    /// so the variant that was chosen and the transform that poses it cannot
    /// come from two different slots — the bug `item/spyglass_in_hand`
    /// recorded for the baked path.
    fn prepare_special_hand<'a>(
        &'a self,
        device: &wgpu::Device,
        held: &MainHandItem,
        form: &lodestone_render::SpecialItemForm,
        pose: HandPose,
        camera: &Camera,
    ) -> Option<(Vec<SpecialHandDraw<'a>>, Vec<HandBannerLayerDraw>)> {
        let item = &held.item;
        let banner_patterns = held.banner_patterns.as_slice();
        let base_color = held.base_color.as_deref();
        let skin = held.skin.as_ref();
        // The caller resolved `form` with this same arm's display slot, so the
        // variant chosen and the transform that poses it name the same hand.
        let transform = hand_transform(&form.display, pose.arm, true);
        let placement = lodestone_render::entity::first_person_item_matrix(
            pose.arm,
            pose.attack_anim,
            pose.inverse_arm_height,
            &transform,
        );
        // The item definition's whole root-to-`special` `"transformation"` chain
        // composes *underneath* the display-context pose just built, exactly as
        // `bake` threads it down — see `compose_special_item_transform`'s doc
        // for the derivation and for why an ancestor node's entry counts (a
        // shield's `scale [1, -1, -1]` is one).
        let placement = lodestone_render::compose_special_item_transform(
            placement,
            &form.kind,
            &form.transformation,
        );
        let light = self.hand_light(camera);

        // The banner rig is two meshes sharing this same placement — see
        // `lodestone_render::banner_item_rig`'s doc. Both draw **untinted**;
        // colour rides the translucent pattern-layer draws built below.
        if form.kind == "minecraft:banner"
            && let Some(rig) = lodestone_render::banner_item_rig(item.path())
            && let Some(base) = lodestone_render::banner_item_base_color(item.path())
        {
            let body = self.build_special_hand_draw(
                device,
                (rig.body.0, lodestone_render::BlockEntityTexture::Static(rig.body.1)),
                [255, 255, 255],
                placement,
                light,
            )?;
            let flag = self.build_special_hand_draw(
                device,
                (rig.flag.0, lodestone_render::BlockEntityTexture::Static(rig.flag.1)),
                [255, 255, 255],
                placement,
                light,
            )?;
            // The flag part's own world matrix — `build_special_hand_draw`
            // resolves it internally via `mesh.part_transforms`, but the layer
            // draws need it again to pose the mask over the very same flag, so
            // it is recomputed here from the same `BANNER_FLAG` mesh rather than
            // threaded out of the private helper. `index_of("flag")`, not the
            // first transform: mirrors `BlockEntityModelSet::resolve_banner`,
            // which does not assume the flag is part 0 either.
            let flag_world = self
                .block_entities
                .models
                .get(rig.flag.0)
                .and_then(|mesh| {
                    let index = mesh.index_of("flag")?;
                    mesh.part_transforms(placement, &[]).into_iter().nth(index)
                })
                .unwrap_or(placement);
            let stored: Vec<lodestone_render::StoredPatternLayer> = banner_patterns
                .iter()
                .filter_map(|layer| {
                    Some(lodestone_render::StoredPatternLayer {
                        pattern_asset_id: layer.pattern_asset_id.clone(),
                        color: lodestone_render::DyeColor::from_name(&layer.color)?,
                    })
                })
                .collect();
            let mut layers = Vec::new();
            for layer in lodestone_render::banner_pattern_layers(base, &stored) {
                let Some(pattern) = layer.sprite.path().rsplit('/').next() else {
                    continue;
                };
                if !self.block_entities.banner_patterns.contains_key(pattern) {
                    continue;
                }
                let rgb = lodestone_render::gamma_rgb_to_bytes(layer.color);
                let Some(buffer) = upload_instances_tinted(
                    device,
                    &[flag_world],
                    &[light],
                    &[lodestone_render::InstanceTint::rgb(rgb)],
                ) else {
                    continue;
                };
                layers.push(HandBannerLayerDraw {
                    family: HandPatternFamily::Banner,
                    pattern: pattern.to_string(),
                    instances: buffer,
                });
            }
            return Some((vec![body, flag], layers));
        }

        // The shield rig is one mesh (plate+handle together, see
        // `lodestone_render::block_entity::SHIELD`'s doc) drawn opaque once,
        // then re-submitted per pattern layer through the same translucent
        // pass a banner's flag uses — vanilla's own shield special-renderer
        // submit routine ported.
        // Unlike a banner, there is no separate "flag" quad the layers paint
        // over: `base` re-tints the *whole* shield mesh each time.
        if form.kind == "minecraft:shield" {
            let has_patterns =
                lodestone_render::shield_has_patterns(base_color, banner_patterns.len());
            let rig = lodestone_render::shield_item_rig(has_patterns);
            let base_draw = self.build_special_hand_draw(
                device,
                (rig.0, lodestone_render::BlockEntityTexture::Static(rig.1)),
                [255, 255, 255],
                placement,
                light,
            )?;

            let mut layers = Vec::new();
            if has_patterns {
                let stored: Vec<lodestone_render::StoredPatternLayer> = banner_patterns
                    .iter()
                    .filter_map(|layer| {
                        Some(lodestone_render::StoredPatternLayer {
                            pattern_asset_id: layer.pattern_asset_id.clone(),
                            color: lodestone_render::DyeColor::from_name(&layer.color)?,
                        })
                    })
                    .collect();
                // Vanilla's own shield special-renderer submit routine's
                // default (base colour or white) when a shield
                // has stored patterns but no `minecraft:base_color` at all.
                let base_dye = base_color
                    .and_then(lodestone_render::DyeColor::from_name)
                    .unwrap_or(lodestone_render::DyeColor::White);
                for layer in lodestone_render::shield_pattern_layers(base_dye, &stored) {
                    let Some(pattern) = layer.sprite.path().rsplit('/').next() else {
                        continue;
                    };
                    if !self.block_entities.shield_patterns.contains_key(pattern) {
                        continue;
                    }
                    let rgb = lodestone_render::gamma_rgb_to_bytes(layer.color);
                    let Some(buffer) = upload_instances_tinted(
                        device,
                        &[placement],
                        &[light],
                        &[lodestone_render::InstanceTint::rgb(rgb)],
                    ) else {
                        continue;
                    };
                    layers.push(HandBannerLayerDraw {
                        family: HandPatternFamily::Shield,
                        pattern: pattern.to_string(),
                        instances: buffer,
                    });
                }
            }
            return Some((vec![base_draw], layers));
        }

        // A decorated pot is five draws sharing one placement — a base and four
        // independently sheeted faces, which is vanilla's own decorated-pot
        // renderer's submit routine's
        // own decomposition and the reason it cannot be a `special_item_rig`
        // pair. Unlike a banner's layers these are ordinary opaque draws, so
        // there is no translucent list to return alongside them.
        //
        // **The four sherds are vanilla's own empty-decorations sentinel here,
        // not the stack's own.** That is exactly what vanilla does for a
        // stack carrying no
        // `minecraft:pot_decorations`, so
        // an undecorated pot — the overwhelmingly common one — is *correct*
        // rather than approximate. A pot that really does carry sherds draws
        // with the four default side sprites, and the missing link is named
        // rather than left to be rediscovered: `lodestone_model` already
        // decodes the component into a real `PotDecorations`, but the shell's
        // hotbar record does not carry it, so it never reaches `MainHandItem`
        // and cannot reach here. Threading it is the same one-field-per-layer
        // walk `banner_patterns` and `base_color` already made; do that rather
        // than reading this as an unported rig.
        if form.kind == "minecraft:decorated_pot" && item.path() == "decorated_pot" {
            let rig = lodestone_render::decorated_pot_item_rig(None, None, None, None);
            let draws: Vec<SpecialHandDraw<'a>> = rig
                .parts()
                .into_iter()
                .filter_map(|part| {
                    self.build_special_hand_draw(
                        device,
                        (part.0, lodestone_render::BlockEntityTexture::Static(part.1)),
                        [255, 255, 255],
                        placement,
                        light,
                    )
                })
                .collect();
            if draws.is_empty() {
                return None;
            }
            return Some((draws, Vec::new()));
        }

        // The trident is the one special rig whose mesh lives in the **entity**
        // corpus rather than in `BLOCK_ENTITY_MODELS` — it is the same
        // `"trident"` entry vanilla's own thrown-trident renderer draws a thrown one with, and
        // vanilla bakes it the same way (its own trident special-renderer bake
        // routine takes the trident model layer out of the entity model set). So it cannot
        // go through `special_item_rig`, whose `&'static str` is a
        // `BLOCK_ENTITY_MODELS` key; see `lodestone_render::trident_item_rig`.
        //
        // The `scale [1, -1, -1]` flip that stands a held trident the right way
        // up is **not** applied here. It rides `form.transformation`, because
        // `trident.json` declares it on the enclosing `minecraft:condition`
        // node rather than on either `special` node underneath — the same
        // inherited-`transformation` shape the shield's own flip has, already
        // composed by `compose_special_item_transform` above. Re-applying it
        // here would square the flip and land the trident upright but mirrored.
        if form.kind == "minecraft:trident"
            && let Some(entry) = lodestone_render::trident_item_rig(item.path())
        {
            let draw = self.build_entity_rig_hand_draw(device, entry, placement, light)?;
            return Some((vec![draw], Vec::new()));
        }

        let (model_name, texture_stem) = lodestone_render::special_item_rig(&form.kind, item.path())?;
        // A custom head's own sheet, replacing the default skull stem
        // `special_item_rig` resolves — the same substitution the placed-head
        // pass makes on `SkullSpawn::texture` and the GUI icon pass makes in
        // `push_special_icon`. `skin` is `None` for every other kind and for a
        // plain head, which leaves the resolved stem untouched.
        let texture = match skin {
            Some(url) => lodestone_render::BlockEntityTexture::PlayerSkin(std::sync::Arc::clone(url)),
            None => lodestone_render::BlockEntityTexture::Static(texture_stem),
        };
        let draw = self.build_special_hand_draw(
            device,
            (model_name, texture),
            [255, 255, 255],
            placement,
            light,
        )?;
        Some((vec![draw], Vec::new()))
    }

    /// Build one [`SpecialHandDraw`] for a rig held in the **entity** corpus
    /// (`self.entities`) rather than the block-entity one — today that is the
    /// trident and only the trident.
    ///
    /// It is a sibling of [`Self::build_special_hand_draw`] rather than a
    /// parameter on it because the two corpora key their textures differently,
    /// and that difference is the whole reason this exists: a block-entity rig
    /// names a *sheet stem* that `block_entities.textures` is keyed on, while an
    /// entity rig's texture is bound to the corpus **entry name**
    /// (`EntityTexture::Fixed`). Threading a bool through one function would put
    /// two meanings on one `&str` argument, which is exactly how a rig ends up
    /// looked up in the corpus that does not hold it and silently draws nothing.
    ///
    /// The upload shape is otherwise identical, deliberately: same
    /// `part_transforms(placement, &[])` with no pose overrides, same untinted
    /// instance, same "a missing part range is skipped, an empty result is
    /// `None`" fail-open. A trident has no animated part — vanilla's own
    /// trident model is a
    /// static rig and its own trident special-renderer submit routine calls
    /// no pose-setup routine — so
    /// the empty override list is the rest pose *and* the right pose, not a
    /// simplification.
    fn build_entity_rig_hand_draw<'a>(
        &'a self,
        device: &wgpu::Device,
        entry: &str,
        placement: glam::Mat4,
        light: u32,
    ) -> Option<SpecialHandDraw<'a>> {
        let mesh = self.entities.models.get(entry)?;
        let gpu = self.entities.gpu_models.get(entry)?;
        let texture = self.entities.textures.get(entry)?;

        // The entity corpus keeps vertices **part-local**, so each part's own
        // rest matrix has to be composed back in — `Skeleton::rest_pose` is the
        // exact inverse of that split (`part_bake_recomposes_to_the_whole_model_
        // bake` asserts it over the whole corpus). The block-entity corpus'
        // `part_transforms` does the same job on its side; skipping it here and
        // uploading `placement` for every part collapses the whole trident onto
        // the root and draws a heap at the origin rather than nothing, which is
        // the failure that would look like a pose bug instead of a lookup one.
        let transforms = mesh.skeleton.rest_pose();
        let instance_tint = lodestone_render::InstanceTint::rgb([255, 255, 255]);
        let parts: Vec<(lodestone_render::entity::PartRange, wgpu::Buffer)> = transforms
            .iter()
            .enumerate()
            .filter_map(|(index, local)| {
                let matrix = placement * *local;
                let range = *gpu.parts.get(index)?;
                if range.index_count == 0 {
                    return None;
                }
                let buffer =
                    upload_instances_tinted(device, &[matrix], &[light], &[instance_tint])?;
                Some((range, buffer))
            })
            .collect();
        if parts.is_empty() {
            return None;
        }
        Some(SpecialHandDraw {
            model: gpu,
            texture,
            parts,
        })
    }

    /// Build one [`SpecialHandDraw`] for `(model_name, texture_stem)`, tinted by
    /// `tint`. Shared by every `minecraft:special` kind
    /// [`Self::prepare_special_hand`] resolves, so a chest, a shulker, a skull
    /// and now a banner's two meshes all go through the same upload shape.
    fn build_special_hand_draw<'a>(
        &'a self,
        device: &wgpu::Device,
        (model_name, texture_id): (&'static str, lodestone_render::BlockEntityTexture),
        tint: [u8; 3],
        placement: glam::Mat4,
        light: u32,
    ) -> Option<SpecialHandDraw<'a>> {
        let mesh = self.block_entities.models.get(model_name)?;
        let gpu = self.block_entities.gpu_models.get(model_name)?;
        // A missing sheet draws **nothing** rather than an untextured box — the same
        // fail-open the world block-entity pass uses, and for the same reason: a
        // flat-magenta chest-shaped box in the hand reads as a renderer bug.
        //
        // A **fetched** custom-head skin resolves through the world entity pass's
        // own url-keyed cache, exactly as `gpu/frame.rs`'s placed-head loop does —
        // one fetch, one decode, one bind group, whether that head is in the world,
        // in a slot or in your hand. A miss there is normal while the fetch is in
        // flight, so it falls back to the default Steve sheet rather than drawing
        // nothing, which is the one case where the two fail-opens differ.
        let texture = match &texture_id {
            lodestone_render::BlockEntityTexture::Static(stem) => {
                self.block_entities.textures.get(stem)
            }
            lodestone_render::BlockEntityTexture::PlayerSkin(url) => self
                .entities
                .player_skins
                .get(url.as_ref())
                .or_else(|| {
                    self.block_entities.textures.get(lodestone_render::skull_texture_stem(
                        lodestone_render::SkullType::Player,
                    ))
                }),
        }?;

        // `&[]` — no pose overrides. A held chest's lid is shut:
        // the held-chest rig takes no lid angle at all, so the rest pose *is*
        // the pose. Passing a lid angle here would open every chest in every hand.
        let transforms = mesh.part_transforms(placement, &[]);
        let instance_tint = lodestone_render::InstanceTint::rgb(tint);

        let parts: Vec<(lodestone_render::entity::PartRange, wgpu::Buffer)> = transforms
            .iter()
            .enumerate()
            .filter_map(|(index, matrix)| {
                let range = *gpu.parts.get(index)?;
                if range.index_count == 0 {
                    return None;
                }
                let buffer = upload_instances_tinted(
                    device,
                    &[*matrix],
                    &[light],
                    &[instance_tint],
                )?;
                Some((range, buffer))
            })
            .collect();
        if parts.is_empty() {
            return None;
        }
        Some(SpecialHandDraw {
            model: gpu,
            texture,
            parts,
        })
    }

    /// The packed light byte the first-person hand is lit with, for both branches.
    ///
    /// Vanilla's own held-item render routine's packed-light-coords lookup,
    /// evaluated for the local player at the frame's partial tick.
    ///
    /// # The eye is not a deviation, and the byte is not one channel
    ///
    /// This doc used to read "sampled at the **eye** rather than the feet", framed
    /// as a departure we chose. It is not — it is what vanilla does. Following the
    /// call through: vanilla's own entity-renderer light lookup resolves the
    /// sample position via a per-entity "light probe position" hook, which the
    /// base entity implementation simply returns as the interpolated eye
    /// position; it then reads the block-light and sky-light levels at that
    /// position's containing block and packs them into one value.
    ///
    /// And the `u32::from(u8)` is a widen, not a truncation to a single channel:
    /// [`EntityLightSource::sample`](super::sources::EntityLightSource) returns
    /// vanilla's **packed** pair — sky in the high nibble, block in the low (see
    /// [`lodestone_render::ENTITY_FULLBRIGHT`], which is `15 << 4`) — and
    /// `entity.wgsl`'s `vs_main` unpacks both:
    ///
    /// ```wgsl
    /// let sky = f32((light >> 4u) & 15u) / 15.0;
    /// let block = f32(light & 15u) / 15.0;
    /// ```
    ///
    /// So the hand is lit by exactly the same two-channel value every mob is, and
    /// this is the same call `entity_passes.rs` makes for them. The clock term
    /// rides the uniform rather than the byte — see `write_hand_camera`'s note on
    /// That fix, which was the last real defect here.
    ///
    /// The one measurable difference left from vanilla is that `camera.position`
    /// has the view bob folded into it (`camera_rig::bobbed_camera`), so the probe
    /// wanders by up to `0.05` blocks while walking where vanilla's
    /// unbobbed eye position does not. That can flip the sampled block across a
    /// boundary, and it is shared with every other `entity_light.sample` call in
    /// this file's siblings rather than specific to the hand. Recorded, not fixed:
    /// unbobbing it means passing a second camera down from
    /// [`super::frame`], which is another agent's file.
    #[must_use]
    fn hand_light(&self, camera: &Camera) -> u32 {
        u32::from(self.entity_light.sample(camera.position))
    }

    /// Install this frame's walk/hurt bob for the first-person hand pass — see
    /// [`HandBobSource`]'s doc for why the hand needs its own copy of the same
    /// [`BobFrame`] the world's camera already folded, and
    /// [`crate::sim::camera::Sim::bob_frame`] for the value to pass.
    ///
    /// **Install every frame**, like
    /// [`Self::set_hand_swing_source`](RenderState::set_hand_swing_source) and
    /// [`Self::set_main_hand_source`](RenderState::set_main_hand_source): the
    /// value is a partial-tick interpolation of `Sim`'s walk distance, so a
    /// one-shot install would freeze the bob at whatever it looked like the
    /// instant the source was wired in.
    ///
    /// Unset — the default, the offline demo, every headless test — reads as
    /// [`BobFrame::default`] via [`HandBobSource::value`], which reproduces
    /// exactly the pre-existing (unbobbed) hand.
    pub fn set_hand_bob_source(&mut self, f: impl Fn() -> BobFrame + Send + Sync + 'static) {
        self.hand_bob = HandBobSource(Some(Box::new(f)));
    }

    /// Install this frame's decaying view-lag sample for both hand attachment
    /// consumers. Re-installed every frame because the lagged pair is
    /// partial-tick interpolated and decays after a turn.
    pub fn set_view_lag_source(
        &mut self,
        f: impl Fn() -> ViewLagFrame + Send + Sync + 'static,
    ) {
        self.view_lag = ViewLagSource(Some(Box::new(f)));
    }

    /// Rewrite both hand passes' group-0 uniforms with [`hand_view_proj`].
    ///
    /// **No view matrix, but now a bob matrix**, because
    /// vanilla's own held-item render routine multiplies the pose stack by
    /// the inverse model-view matrix while pushing the model-view stack
    /// times the model-view matrix
    /// and the shader evaluates `projection · model-view · pose`: the view
    /// rotation cancels exactly, leaving a camera-space pose. Feeding
    /// `Camera::view_projection` here instead parks the hand at the world origin,
    /// visible only when the player stands on it. [`hand_view_proj`]'s own doc
    /// (and [`HandBobSource`]'s) has the rest: vanilla applies the hurt tilt and walk bob
    /// to this same pass a **second** time, independent of the world's copy.
    ///
    /// Two buffers, one value: the entity pipeline (bare arm) and the model
    /// pipeline (held item) declare different group-0 layouts, so each needs its
    /// own. Written together here so they cannot drift.
    ///
    /// Returns the column-major `view_proj` it wrote. The caller hands it to
    /// [`RenderState::write_glint_uniform`] for an enchanted held item: the glint
    /// pass runs under depth-`EQUAL`, which only passes if it rasterises the
    /// *same* clip positions as this base pass — so the glint uniform must carry
    /// exactly this matrix, not a second copy of it.
    fn write_hand_camera(&self, queue: &wgpu::Queue, camera: &Camera) -> [[f32; 4]; 4] {
        // Vanilla applies the hurt tilt to this pass a **second** time, independently
        // of the world's copy, and it reaches the hand without any lossy fold:
        // `hand_view_proj` multiplies the raw bob matrix straight into the hand's
        // projection, so roll survives here where it cannot survive
        // `bobbed_camera`.
        let view_proj = hand_view_proj(
            camera.aspect,
            self.hand_bob.value(),
            self.view_lag.value(),
            self.damage_tilt_strength,
        );
        let camera_uniform = CameraUniform {
            view_proj: view_proj.to_cols_array_2d(),
            section_origin: [0.0, 0.0, 0.0, 0.0],
        };
        // No distance fog on the hand for either branch (vanilla does not fog
        // it either, and at ~0.7 blocks it could contribute nothing), but the
        // sky-darken lane still rides along — the same lane `fog_with_clock`
        // sets for terrain and mobs, so the hand cannot disagree with the
        // world about what time it is.
        //
        // **Both branches must read this from the same place.** Before this,
        // the arm's uniform carried it (via `EntityCameraUniform::
        // with_sky_darken`) and the item's did not: `update_model_shared_
        // camera_buffer` was called with a bare `FogUniform::disabled()`,
        // which leaves the spare lane at its negative/"unwired" sentinel, and the
        // model shader's `sky_darken()` reads that sentinel as permanent
        // noon. That was that fix's actual bug — not a missing light sample
        // (`hand_light` already samples real per-position world light for
        // both branches; see its own doc), but the held item's sky component
        // never darkening: at night, in the open, the item stayed lit as if
        // it were noon while the arm right next to it correctly dimmed.
        //
        // The dimension's ambient floor rides along too, for the same reason:
        // `FogUniform::new`/`disabled` default it to the overworld's own
        // grey, so without this the held item would stay overworld-lit while
        // standing in the Nether even after `fog_with_clock` fixed terrain
        // and the arm.
        let mut hand_fog = FogUniform::disabled();
        hand_fog.end_enabled[2] = self.sky_darken.value();
        let ambient = self.effective_ambient_light();
        hand_fog.ambient_light = [ambient[0], ambient[1], ambient[2], 0.0];
        queue.write_buffer(
            &self.entities.hand_cam_buffer,
            0,
            bytemuck::bytes_of(&EntityCameraUniform {
                camera: camera_uniform,
                fog: hand_fog,
            }),
        );
        // And the block-entity pass's own copy, for a held chest/shulker box/skull
        // ([`FirstPersonHand::Special`]). Written unconditionally rather than only
        // when that branch wins: it is 128 bytes, and a conditional write is how a
        // rarely-taken branch ends up reading last frame's matrix — visible as a
        // held chest that lags the camera by one frame while the arm does not.
        queue.write_buffer(
            &self.block_entities.hand_cam_buffer,
            0,
            bytemuck::bytes_of(&EntityCameraUniform {
                camera: camera_uniform,
                fog: hand_fog,
            }),
        );
        if let Some(model) = self.model.as_ref() {
            // The origin binding is untouched here: it always points at the
            // shared arena's reserved zero slot (see the draw site), so only
            // the shared view_proj/fog half needs rewriting.
            update_model_shared_camera_buffer(
                queue,
                &model.hand_cam_buffer,
                camera_uniform.view_proj,
                hand_fog,
            );
        }
        view_proj.to_cols_array_2d()
    }

    /// Record the first-person hands pass: its own render pass, with the depth
    /// buffer cleared, both hands drawn into it.
    ///
    /// The reference client clears depth immediately before drawing hands. Its
    /// depth is reversed-Z, so its clear is *far* at `0.0`; ours is `[0,1]`, so
    /// the equivalent is `1.0` (the sign flip `CLAUDE.md` warns about, on a
    /// clear rather than a comparison).
    ///
    /// Without the clear a hand would be occluded by any block within ~0.75
    /// blocks of the eye — standing in a doorway, or facing the block you are
    /// mining — because the hand genuinely *is* inside that geometry. The
    /// colour attachment loads rather than clears, so the world stays. The two
    /// hands share the cleared buffer and depth-test against each other, which
    /// is what keeps an off-hand shield and a main-hand sword correctly ordered
    /// where they overlap.
    pub(super) fn draw_first_person_hands(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        hands: &FirstPersonHands<'_>,
        stats: &mut RenderStats,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("first-person hand pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(lodestone_render::DEPTH_CLEAR),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            // GPU frame profiling (`gpu::gpu_timing`) — see the identical
            // pattern's comment at the "block pass" descriptor in `frame.rs`
            // for why this temporary borrow cannot collide with the
            // `resolve`/`after_submit` calls `render_inner` makes later.
            timestamp_writes: self.gpu_timer.borrow().as_ref().and_then(|t| t.writes("first_person")),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for hand in [hands.main.as_ref(), hands.off.as_ref()].into_iter().flatten() {
            self.record_hand(&mut pass, hand, stats);
        }
    }

    /// Record one hand's draws into the shared hands pass.
    fn record_hand<'p>(
        &'p self,
        pass: &mut wgpu::RenderPass<'p>,
        hand: &'p FirstPersonHand<'_>,
        stats: &mut RenderStats,
    ) {
        match hand {
            // The held item is item-model geometry, so it draws through the
            // *model* pipeline with that pipeline's four bind groups — the
            // same atlas, palette and animation slots the terrain and the
            // hotbar icons use. Only group 0 differs: the hand projection.
            FirstPersonHand::Item(mesh, foil) => {
                if let Some(model) = self.model.as_ref() {
                    pass.set_pipeline(&model.pipeline.pipeline);
                    // The held item's pose is already camera-space (see
                    // `write_hand_camera`'s doc), so like the dropped-item
                    // pass it has no origin of its own: the shared arena's
                    // reserved zero slot.
                    pass.set_bind_group(
                        0,
                        &model.hand_cam_bind_group,
                        &[model.origin_arena.zero_offset()],
                    );
                    pass.set_bind_group(1, &model.atlas_bind_group, &[]);
                    pass.set_bind_group(2, &model.palette_bind_group, &[]);
                    pass.set_bind_group(3, &model.anim_bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    stats.draw_calls += 1;

                    // The glint second pass, in this **same** render pass: the
                    // glint pipeline's depth compare is `EQUAL`, which only
                    // matches where the base draw above just wrote depth — a
                    // later pass would find the depth buffer and EQUAL nothing.
                    // The uniform was written by `prepare_first_person_hands`
                    // with this frame's hand view_proj (the one `write_hand_camera`
                    // computed), so both passes rasterise identical clip
                    // positions and the shimmer lands exactly on the item.
                    if *foil
                        && let Some(glint) = self.glint.as_ref()
                    {
                        pass.set_pipeline(&glint.pipeline.pipeline);
                        pass.set_bind_group(0, &glint.uniform_bind_group, &[]);
                        pass.set_bind_group(1, &glint.texture_bind_group, &[]);
                        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                        pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                        stats.draw_calls += 1;
                    }
                }
            }
            // A filled map: the same four bind groups as the item branch with
            // **group 1 swapped** to the map's own texture. No glint second pass —
            // vanilla's own map-render path draws no foil, and a map is not enchantable.
            FirstPersonHand::Map(map) => {
                if let Some(model) = self.model.as_ref() {
                    pass.set_pipeline(&model.pipeline.pipeline);
                    pass.set_bind_group(
                        0,
                        &model.hand_cam_bind_group,
                        &[model.origin_arena.zero_offset()],
                    );
                    pass.set_bind_group(2, &model.palette_bind_group, &[]);
                    pass.set_bind_group(3, &model.anim_bind_group, &[]);
                    let (mesh, texture) = map.picture();
                    pass.set_bind_group(1, texture, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    stats.draw_calls += 1;
                    stats.filled_maps_drawn += 1;
                    if let Some((mesh, sheet)) = map.decorations() {
                        pass.set_bind_group(1, sheet, &[]);
                        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                        pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                        stats.draw_calls += 1;
                    }
                }
            }
            // A held block-entity rig: the **block-entity pass's** pipeline and its
            // own hand camera, so the sheet is `entity/chest/normal` and not the
            // stitched block atlas. Two bind groups, not four — see
            // `FirstPersonHand::Special`.
            FirstPersonHand::Special(draws, layers) => {
                pass.set_pipeline(&self.block_entities.pipeline.pipeline);
                pass.set_bind_group(0, &self.block_entities.hand_cam_bind_group, &[]);
                // Usually one draw; the banner rig is two (pole/bar, flag) sharing
                // this pipeline and camera bind group — see `FirstPersonHand::Special`.
                for draw in draws {
                    pass.set_bind_group(1, draw.texture, &[]);
                    pass.set_vertex_buffer(0, draw.model.vertices.slice(..));
                    pass.set_index_buffer(draw.model.indices.slice(..), wgpu::IndexFormat::Uint32);
                    for (range, buffer) in &draw.parts {
                        pass.set_vertex_buffer(1, buffer.slice(..));
                        let end = range.index_start + range.index_count;
                        pass.draw_indexed(range.index_start..end, 0, 0..1);
                        stats.draw_calls += 1;
                    }
                }

                // The held banner's own translucent pattern-layer draws, over the
                // same flag geometry the opaque loop above just drew — mirrors
                // `gpu/frame.rs`'s world-space banner-layer pass exactly, one
                // camera space over. Empty for every non-banner kind.
                //
                // **`index_of("flag")`, not `.parts.first()`.** `banner_flag_model`'s
                // root part carries no cube of its own (`PartDef::new(PartPose::ZERO)`
                // with no `with_cube`, only a `"flag"` child) — its own part range is
                // `index_count == 0`, so `.first()` silently draws zero indices,
                // reaching every guard here (`layers` non-empty, the model found) and
                // still painting nothing. Measured live: `parts.first()` was
                // `PartRange { index_count: 0, .. }`, and switching to the real
                // "flag" part's index is what made this pass draw at all.
                //
                // **A shield's layers redraw the *whole* mesh, not one named
                // part.** Vanilla's own shield special-renderer submit routine
                // re-submits its whole model
                // (both `plate` and `handle`) unchanged per pattern layer —
                // there is no shield analogue of a banner's separate `"flag"`
                // quad (see `lodestone_render::block_entity::SHIELD`'s doc) — so
                // the shield arm below draws every part with `index_count > 0`
                // rather than singling one out by name.
                if let Some(family) = layers.first().map(|l| l.family) {
                    let (mesh_name, single_part): (&str, Option<&str>) = match family {
                        HandPatternFamily::Banner => ("banner_flag", Some("flag")),
                        HandPatternFamily::Shield => ("shield", None),
                    };
                    let mask_map = match family {
                        HandPatternFamily::Banner => &self.block_entities.banner_patterns,
                        HandPatternFamily::Shield => &self.block_entities.shield_patterns,
                    };
                    if let Some(gpu) = self.block_entities.gpu_models.get(mesh_name) {
                        let ranges: Vec<lodestone_render::entity::PartRange> = match single_part {
                            Some(part_name) => self
                                .block_entities
                                .models
                                .get(mesh_name)
                                .and_then(|mesh| mesh.index_of(part_name))
                                .and_then(|i| gpu.parts.get(i))
                                .filter(|r| r.index_count > 0)
                                .copied()
                                .into_iter()
                                .collect(),
                            None => gpu
                                .parts
                                .iter()
                                .filter(|r| r.index_count > 0)
                                .copied()
                                .collect(),
                        };
                        if !ranges.is_empty() {
                            pass.set_pipeline(&self.block_entities.banner_layer_pipeline);
                            pass.set_bind_group(0, &self.block_entities.hand_cam_bind_group, &[]);
                            pass.set_vertex_buffer(0, gpu.vertices.slice(..));
                            pass.set_index_buffer(gpu.indices.slice(..), wgpu::IndexFormat::Uint32);
                            for layer in layers {
                                let Some(mask) = mask_map.get(&layer.pattern) else {
                                    continue;
                                };
                                pass.set_bind_group(1, mask, &[]);
                                pass.set_vertex_buffer(1, layer.instances.slice(..));
                                for range in &ranges {
                                    let end = range.index_start + range.index_count;
                                    pass.draw_indexed(range.index_start..end, 0, 0..1);
                                    stats.draw_calls += 1;
                                }
                            }
                        }
                    }
                }
            }
            FirstPersonHand::Arm(arm) => {
                pass.set_pipeline(&self.entities.pipeline.pipeline);
                // The *hand* camera uniform: `hand_projection` alone, because
                // the arm pose is already camera-space. Binding the world one
                // here would leave the arm sitting at the world origin.
                pass.set_bind_group(0, &self.entities.hand_cam_bind_group, &[]);
                pass.set_bind_group(1, arm.texture, &[]);
                pass.set_vertex_buffer(0, arm.model.vertices.slice(..));
                pass.set_index_buffer(arm.model.indices.slice(..), wgpu::IndexFormat::Uint32);
                for (range, buffer) in &arm.parts {
                    pass.set_vertex_buffer(1, buffer.slice(..));
                    let end = range.index_start + range.index_count;
                    pass.draw_indexed(range.index_start..end, 0, 0..1);
                    stats.draw_calls += 1;
                }
            }
        }
    }
}

/// Reports the first-person selector result once for each pack generation and
/// custom-model-data value. This is a diagnostic-only seam: it records the
/// decoded component, the live context, the selected output and whether a
/// missing selected model forced `ItemVariants` back to its GUI form.
fn log_diamond_sword_model_resolution(
    item: &ResourceLocation,
    context: &ItemStateContext,
    forms: &lodestone_render::ItemVariants,
) {
    if item.namespace() != "minecraft"
        || item.path() != "diamond_sword"
        || !tracing::enabled!(target: "pack_trace", tracing::Level::DEBUG)
    {
        return;
    }
    static LAST: std::sync::OnceLock<std::sync::Mutex<Option<(u64, i32)>>> = std::sync::OnceLock::new();
    let generation = crate::resources::pack_generation();
    let data = context.custom_model_data as i32;
    let Ok(mut last) = LAST.get_or_init(|| std::sync::Mutex::new(None)).lock() else {
        return;
    };
    if *last == Some((generation, data)) {
        return;
    }
    *last = Some((generation, data));

    let outputs = forms.definition().resolve(context);
    let chosen_model_is_baked = outputs.iter().any(|output| {
        matches!(output, lodestone_assets::ItemModelOutput::Model { model, .. } if forms.variant(model).is_some())
    });
    tracing::debug!(
        target: "pack_trace",
        surface = "first_person",
        item = %item,
        custom_model_data = context.custom_model_data,
        ?context,
        ?outputs,
        chosen_model_is_baked,
        resolved_geometry = forms.resolve(context).is_some(),
        "diamond-sword item-model selector evaluated"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(path: &str) -> MainHandItem {
        MainHandItem {
            map_id: None,
            item: ResourceLocation::new("minecraft", path).unwrap(),
            foil: false,
            custom_model_data: None,
            dyed_color: None,
            potion_color: None,
            banner_patterns: Vec::new(),
            base_color: None,
            skin: None,
        }
    }

    /// The off hand is the opposite arm of the main one, takes no swing, and
    /// carries its own lowering — in both handednesses. A left-handed frame
    /// puts the main hand on the left and the off hand on the right.
    #[test]
    fn hand_poses_mirror_the_off_hand_and_never_swing_it() {
        let mut frame = FirstPersonHandsFrame::holding(Some(held("iron_sword")));
        frame.off = super::super::HandFrame {
            item: Some(held("shield")),
            inverse_arm_height: 0.25,
            drawn: true,
        };
        let poses = hand_poses(&frame, 0.5);
        assert_eq!(poses.len(), 2);
        assert_eq!((poses[0].0.arm, poses[0].0.attack_anim), (Arm::Right, 0.5));
        assert_eq!((poses[1].0.arm, poses[1].0.attack_anim), (Arm::Left, 0.0));
        assert_eq!(poses[1].0.inverse_arm_height, 0.25);
        assert_eq!(poses[1].1.map(|h| h.item.path()), Some("shield"));

        frame.main_arm = Arm::Left;
        let poses = hand_poses(&frame, 0.5);
        assert_eq!(poses[0].0.arm, Arm::Left);
        assert_eq!(poses[1].0.arm, Arm::Right);

        // A hand the frame does not draw (a drawn bow hides the other) is
        // skipped entirely.
        frame.off.drawn = false;
        assert_eq!(hand_poses(&frame, 0.0).len(), 1);
    }

    // -----------------------------------------------------------------------
    // `hand_view_proj`: the bob reaching the hand's own projection. No GPU
    // needed — every number below is hand-derived from vanilla's constants,
    // never from `eye_transform`/`hand_projection` themselves, the same
    // standard `camera_rig.rs`'s own bob tests and `view_bob_pixels.rs`'s
    // module doc hold to.
    // -----------------------------------------------------------------------

    /// A synthetic eye-space point, `0.6` blocks straight ahead — a plausible
    /// hand-mesh depth (`write_hand_camera`'s own doc: "at ~0.7 blocks"). It is
    /// not read from any real mesh; it exists only so the matrix can be
    /// checked against numbers computed independently of the code under test.
    const HAND_TEST_POINT: glam::Vec3 = glam::Vec3::new(0.0, 0.0, -0.6);
    const HAND_TEST_ASPECT: f32 = 320.0 / 240.0;
    const HAND_TEST_W: f32 = 320.0;
    const HAND_TEST_H: f32 = 240.0;

    /// `clip.xy / clip.w` for `p` under `m`.
    fn ndc(m: glam::Mat4, p: glam::Vec3) -> (f32, f32) {
        let clip = m * p.extend(1.0);
        (clip.x / clip.w, clip.y / clip.w)
    }

    /// Bit-identical, not merely close, to the bare projection — the "view
    /// bobbing off" and "no source installed" cases both land here via
    /// [`BobFrame::default`], and CLAUDE.md's evidence standards ask for exact
    /// equality on an inert input, not a small-diff tolerance.
    #[test]
    fn a_zero_frame_is_bit_identical_to_the_bare_hand_projection() {
        let bobbed = hand_view_proj(
            HAND_TEST_ASPECT,
            BobFrame::default(),
            ViewLagFrame::default(),
            NO_DAMAGE_TILT,
        );
        let bare = hand_projection(HAND_TEST_ASPECT);
        assert_eq!(
            bobbed.to_cols_array(),
            bare.to_cols_array(),
            "an identity bob must leave hand_projection completely untouched, not \
             merely close to it"
        );
        // And an unset source reads the same way, through `HandBobSource`.
        let source = HandBobSource::default();
        assert_eq!(source.value(), BobFrame::default());
    }

    /// The production hand-camera matrix must move a real screen-space sample by
    /// the residual's measured amount. The expected pixels come from the
    /// projection equation and the two rotation angles, not from another call
    /// into the matrix under test; the zero-residual control must remain centred.
    #[test]
    fn a_rapid_turn_reaches_the_predicted_hand_pixels() {
        let frame = ViewLagFrame {
            pitch: 15.0,
            yaw: 45.0,
            view_pitch: 30.0,
            view_yaw: 90.0,
        };
        let m = hand_view_proj(
            HAND_TEST_ASPECT,
            BobFrame::default(),
            frame,
            NO_DAMAGE_TILT,
        );
        let (ndc_x, ndc_y) = ndc(m, HAND_TEST_POINT);
        let pixel_x = ndc_x * (HAND_TEST_W / 2.0);
        let pixel_y = -ndc_y * (HAND_TEST_H / 2.0);
        assert!(
            (pixel_x - -13.4923).abs() < 0.02,
            "the yaw residual must reach the hand at the predicted location; got {pixel_x:+.4} px"
        );
        assert!(
            (pixel_y - -4.4877).abs() < 0.02,
            "the pitch residual must reach the hand at the predicted location; got {pixel_y:+.4} px"
        );

        let (control_x, control_y) = ndc(
            hand_view_proj(
                HAND_TEST_ASPECT,
                BobFrame::default(),
                ViewLagFrame::default(),
                NO_DAMAGE_TILT,
            ),
            HAND_TEST_POINT,
        );
        assert_eq!((control_x, control_y), (0.0, 0.0));
    }

    /// **The dip, at the amplitude ceiling** (`walk_phase = 0`, `bob = 0.1`).
    ///
    /// Hand-derived, not from `eye_transform`: the nod is
    /// `|cos(-0.2)*0.1|*5 = 0.4900335°` (pinned independently against vanilla's
    /// source in `camera_rig::tests::the_nods_phase_offset_is_...`), rotating
    /// [`HAND_TEST_POINT`] about `+X` gives `(0, 0.0051318, -0.5999780)`; adding
    /// the dip's translate `(0, -0.1, 0)` gives `(0, -0.0948684, -0.5999780)`.
    /// Projected through `hand_projection`'s `70°` FOV (`tan(35°) = 0.700208`):
    /// `NDC.y` goes from `0` to `-0.2258186`, i.e. **`+27.10 px` down**
    /// (`dpixel_y = -dNDC_y * (H/2)`, `H = 240`).
    ///
    /// That is far larger than the chest's `+8.50 px` in `view_bob_pixels.rs`
    /// for the *same* `0.1`-amplitude dip — because the hand sits `0.6` blocks
    /// from the eye against the chest's `2.5`, and the same physical
    /// displacement subtends a bigger angle the closer the surface is. This is
    /// also why vanilla's own hand visibly swings more than the scenery while
    /// walking; a smaller number here would be the sign of a wrong depth
    /// assumption, not a mistake in the transform itself.
    ///
    /// Two rejected hypotheses, computed the same way: dropping the nod
    /// entirely gives `+28.56 px` (`1.46 px` off — a small but real gap, not
    /// hidden by rounding), and inverting the nod's sign gives `+30.03 px`
    /// (`2.93 px` off). Both are closer to the true sign than to the true
    /// magnitude, which is exactly the "it moved" trap CLAUDE.md's *magnitude*
    /// species names — a gate that only checked direction would accept either.
    #[test]
    fn the_dip_moves_the_test_point_by_the_hand_derived_pixel_offset() {
        let bare = hand_projection(HAND_TEST_ASPECT);
        let (x0, y0) = ndc(bare, HAND_TEST_POINT);
        assert_eq!((x0, y0), (0.0, 0.0), "precondition: the test point starts dead centre");

        let dip = BobFrame {
            walk_phase: 0.0,
            bob: 0.1,
            hurt: -1.0,
            hurt_dir_degrees: 0.0,
            death_time: 0.0,
        };
        let m = hand_view_proj(HAND_TEST_ASPECT, dip, ViewLagFrame::default(), NO_DAMAGE_TILT);
        let (x1, y1) = ndc(m, HAND_TEST_POINT);
        let dpixel_y = -(y1 - y0) * (HAND_TEST_H / 2.0);
        let dpixel_x = (x1 - x0) * (HAND_TEST_W / 2.0);

        assert!(
            (dpixel_y - 27.098).abs() < 0.05,
            "predicted +27.10 px down; got {dpixel_y:+.3}"
        );
        assert!(
            dpixel_x.abs() < 0.01,
            "no sway at the dip's bottom (`sin(0) == 0`); got {dpixel_x:+.3}"
        );

        // -- the two rejected hypotheses, each individually distinguishable --
        let no_nod = glam::Mat4::from_translation(dip.view_translation());
        let (nx, ny) = ndc(bare * no_nod, HAND_TEST_POINT);
        let no_nod_dy = -(ny - y0) * (HAND_TEST_H / 2.0);
        assert!(
            (no_nod_dy - dpixel_y).abs() > 1.0,
            "dropping the nod must move the prediction by more than a pixel \
             (predicted +28.56 vs the real +27.10); got {no_nod_dy:+.3} vs \
             {dpixel_y:+.3} — the control would not separate them"
        );
        let _ = nx;

        let inverted_nod = glam::Mat4::from_translation(dip.view_translation())
            * glam::Mat4::from_rotation_x(-dip.view_nod_degrees().to_radians());
        // Note the rotation is applied to the *point*, matching `apply_bob`'s
        // T*Rz*Rx composition order (Rz is identity at the dip's bottom):
        // `v' = T(Rx(v))`, i.e. `T * Rx` as a matrix product.
        let inverted = ndc(bare * inverted_nod, HAND_TEST_POINT);
        let inverted_dy = -(inverted.1 - y0) * (HAND_TEST_H / 2.0);
        assert!(
            (inverted_dy - dpixel_y).abs() > 2.0,
            "an inverted nod must move the prediction by more than two pixels \
             (predicted +30.03 vs the real +27.10); got {inverted_dy:+.3} vs \
             {dpixel_y:+.3}"
        );
    }

    /// **The sway, a quarter-stride later** (`walk_phase = -0.5`, `bob = 0.1`) —
    /// the roles swap, same as `view_bob_pixels.rs`'s world-side gate.
    ///
    /// Hand-derived: translate `(-0.05, 0, 0)`, roll `-0.3°`, nod `0.0993°`.
    /// Rotating [`HAND_TEST_POINT`] by the nod then the roll and adding the
    /// translate gives `(-0.0499946, 0.0010397, -0.5999991)`; projected, `NDC.x`
    /// moves from `0` to `-0.0892567` — **`-14.28 px`, leftward** — and `NDC.y`
    /// moves by under a pixel (`-0.30 px`), the residual nod.
    #[test]
    fn the_sway_moves_the_test_point_by_the_hand_derived_pixel_offset() {
        let bare = hand_projection(HAND_TEST_ASPECT);
        let (x0, y0) = ndc(bare, HAND_TEST_POINT);

        let sway = BobFrame {
            walk_phase: -0.5,
            bob: 0.1,
            hurt: -1.0,
            hurt_dir_degrees: 0.0,
            death_time: 0.0,
        };
        let m = hand_view_proj(HAND_TEST_ASPECT, sway, ViewLagFrame::default(), NO_DAMAGE_TILT);
        let (x1, y1) = ndc(m, HAND_TEST_POINT);
        let dpixel_x = (x1 - x0) * (HAND_TEST_W / 2.0);
        let dpixel_y = -(y1 - y0) * (HAND_TEST_H / 2.0);

        assert!(
            (dpixel_x - -14.280).abs() < 0.05,
            "predicted -14.28 px left; got {dpixel_x:+.3}"
        );
        assert!(
            (dpixel_y - -0.297).abs() < 0.05,
            "predicted a residual -0.30 px; got {dpixel_y:+.3}"
        );
        assert!(
            dpixel_x < 0.0,
            "must move LEFT; a sign flip would land near +14.28. got {dpixel_x:+.3}"
        );
    }

    /// The hand's hurt tilt is **live** in production now, and this gate is what
    /// the tilt's presence and its absence both look like.
    ///
    /// It used to assert the opposite — that the production constant was `0.0` —
    /// with a doc naming a blocker in `sim/camera.rs` that had already been fixed.
    /// See [`NO_DAMAGE_TILT`] for that record.
    #[test]
    fn hand_view_proj_carries_the_hurt_tilt_at_a_nonzero_strength() {
        let hurt = BobFrame {
            walk_phase: 0.0,
            bob: 0.0,
            hurt: 5.0,
            hurt_dir_degrees: 0.0,
            death_time: 0.0,
        };
        assert!(
            hurt.hurt_roll_degrees(1.0).abs() > 1.0,
            "precondition: this frame has a real tilt to lose"
        );

        // At the accessibility option's `0.0`, inert — matching the bare
        // projection exactly, not just closely. This is the contract that makes
        // the option a real off switch rather than a shrink.
        let off = hand_view_proj(HAND_TEST_ASPECT, hurt, ViewLagFrame::default(), NO_DAMAGE_TILT);
        assert_eq!(
            off.to_cols_array(),
            hand_projection(HAND_TEST_ASPECT).to_cols_array(),
            "a zero damage-tilt strength must be completely inert"
        );

        // At vanilla's own accessibility default, live.
        let on = hand_view_proj(HAND_TEST_ASPECT, hurt, ViewLagFrame::default(), 1.0);
        assert_ne!(
            on.to_cols_array(),
            hand_projection(HAND_TEST_ASPECT).to_cols_array(),
            "at the default strength the hurt tilt must reach the matrix"
        );
    }
}
