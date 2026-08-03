//! The first-person hand pass: the bare arm or the held item, drawn in its
//! own render pass with the depth buffer cleared (vanilla's
//! `GameRenderer.renderLevel` does the same before `renderItemInHand`). See
//! [`RenderState::prepare_first_person_hand`] for the vanilla parity notes
//! and `docs/arm-swing-animation.md`.
use lodestone_assets::ResourceLocation;
use lodestone_render::{
    Camera, CameraUniform, EntityCameraUniform, GpuEntityModel, GpuModelMesh,
    entity::{
        Arm, first_person_arm_parts, first_person_arm_pose_with_equip, first_person_item_mesh,
        hand_projection, hand_transform, model_for_type,
    },
    fog::FogUniform,
    update_model_shared_camera_buffer, upload_instances,
};

use super::{RenderState, RenderStats};

// ---------------------------------------------------------------------------
// The equip / swap animation (issue #366)
// ---------------------------------------------------------------------------

/// One server tick, in seconds — the rate [`HeldItemEquip`] steps at.
const TICK: f32 = lodestone_ecs::TICK_PERIOD as f32;

/// `ItemInHandRenderer.tick`'s per-tick ramp: `Mth.clamp(target - height, -0.4F,
/// 0.4F)`, so the height moves at most **0.4 per tick** in either direction.
///
/// A full `0 → 1` raise is therefore `1 / 0.4 = 2.5` ticks — **125 ms** — and a
/// complete swap (down then up) is twice that plus the tick the model changes on.
/// This is the one number the animation's *speed* is; the shape is a straight line
/// (see [`HeldItemEquip::inverse_arm_height`] on why the partial-tick lerp of a
/// clamped step is exactly a linear ramp).
const EQUIP_RATE_PER_TICK: f32 = 0.4;

/// `ItemInHandRenderer.tick`'s `if (this.mainHandHeight < 0.1F) this.mainHandItem =
/// nextMainHand;` — **the visible item changes at the bottom of the dip**, not when
/// the slot changes.
///
/// This is the constant that makes the animation read as a swap rather than a
/// twitch: without it the new item appears instantly and then dips, so you watch
/// the *new* pickaxe drop out of frame and come back. Vanilla lowers the **old**
/// item, exchanges it out of sight, and raises the new one.
const EQUIP_SWAP_BELOW: f32 = 0.1;

/// The rest target for the main hand's height.
///
/// Vanilla's is `player.getItemSwapScale(1.0F)³`, i.e. `clamp((itemSwapTicker + 1) /
/// getCurrentItemAttackStrengthDelay(), 0, 1)` cubed — the *attack-cooldown* dip,
/// a second animation that shares this field and lowers the hand briefly after a
/// swing. Neither `itemSwapTicker` nor the attack-strength delay is modelled on
/// this side of the wire, so this is the steady-state value that expression settles
/// at (`1³`). The consequence is precise and worth stating: the swap animation is
/// faithful, the post-attack dip is absent. Guessing a cooldown instead would dip
/// the hand on a schedule unrelated to the player's real attack speed, which is
/// wrong more often than a hand that never dips.
const EQUIP_REST_HEIGHT: f32 = 1.0;

/// Vanilla's `ItemInHandRenderer` swap state for the **main hand**: which item is
/// *visible* (as opposed to selected), and how far raised it is.
///
/// # Why this lives in the renderer and not in `Sim`
///
/// Because that is where vanilla puts it. `mainHandItem`, `mainHandHeight` and
/// `oMainHandHeight` are fields of `ItemInHandRenderer`, not of `LocalPlayer`: the
/// *player* owns the selected slot, and the renderer owns the lag between that and
/// what is drawn. Keeping it here also means the whole feature needed no new
/// installation call — [`RenderState::set_main_hand_source`] is already called once
/// per in-world frame with the currently selected item, and it is the `&mut self`
/// boundary this state is advanced on.
///
/// # The fields are vanilla's, renamed once
///
/// 26.2 calls them `mainHandItem` / `mainHandHeight` / `oMainHandHeight`. Older
/// versions (and issue #366's own description) call the pair
/// `equippedProgress` / `oldEquippedProgress`; there is no field by either of those
/// names in this jar, so grepping for them finds nothing and reads as "the
/// mechanism is absent".
#[derive(Debug)]
pub(super) struct HeldItemEquip {
    /// Vanilla's `mainHandItem` — the item currently **drawn**, which lags the
    /// selected one across a swap. `None` is an empty hand, which draws the bare
    /// arm, so this field is also what decides the arm/item fork mid-swap: putting
    /// away a pickaxe lowers the pickaxe and *then* raises an arm.
    visible: Option<ResourceLocation>,
    /// Vanilla's `mainHandHeight`, `0.0` (fully lowered) to `1.0` (fully raised).
    height: f32,
    /// Vanilla's `oMainHandHeight` — last tick's value, for the partial-tick lerp.
    previous: f32,
    /// Seconds accumulated toward the next 20 Hz step. Doubles as the partial tick.
    accumulator: f32,
    /// `None` until the first [`Self::advance`] call, which **seeds at rest**
    /// rather than stepping.
    ///
    /// Vanilla starts `mainHandItem = EMPTY, mainHandHeight = 0`, so its very first
    /// tick in a world adopts the held item at height 0 and raises it — the item
    /// rises into view on join. That is deliberately *not* reproduced: this state is
    /// advanced per frame from the render thread and a single-frame caller (every
    /// GPU gate in `tests/`, and the first frame after any pass rebuild) would then
    /// render a permanently dipped hand, which is a worse failure than a missing
    /// join flourish. First observation ⇒ fully equipped.
    last: Option<std::time::Instant>,
}

/// **Not `#[derive(Default)]`** — and the difference is a whole broken feature.
///
/// A derived default zeroes `height`/`previous`, and `inverse_arm_height` is
/// `1 - height`, so a `RenderState` on which nobody ever calls
/// [`RenderState::set_main_hand_source`] would draw its bare arm **permanently
/// lowered by 0.6 blocks** — mostly off the bottom of frame. That is every headless
/// and GPU test that renders a hand without opting into a held item, and it looks
/// exactly like "the first-person arm stopped rendering".
impl Default for HeldItemEquip {
    fn default() -> Self {
        Self {
            visible: None,
            height: EQUIP_REST_HEIGHT,
            previous: EQUIP_REST_HEIGHT,
            accumulator: 0.0,
            last: None,
        }
    }
}

impl HeldItemEquip {
    /// Fold this frame's *selected* main-hand item, stepping the 20 Hz swap clock
    /// by however much wall time has passed.
    ///
    /// The wall clock is the honest source here: this is advanced from
    /// [`RenderState::set_main_hand_source`], which the shell calls once per
    /// rendered frame, and there is no game tick on that path. Whole ticks are
    /// consumed from an accumulator (never a fraction of the `0.4` step), so the
    /// animation takes the same wall time at 30 fps as at 240 — the
    /// frame-rate-dependence trap `Sim::step`'s note on `chest_lids.tick()`
    /// records, avoided the same way.
    pub(super) fn advance(&mut self, expected: Option<&ResourceLocation>) {
        let now = std::time::Instant::now();
        let Some(last) = self.last.replace(now) else {
            // First observation: adopt, fully equipped. See `last`'s doc.
            self.visible = expected.cloned();
            self.height = EQUIP_REST_HEIGHT;
            self.previous = EQUIP_REST_HEIGHT;
            self.accumulator = 0.0;
            return;
        };
        self.advance_by(now.saturating_duration_since(last).as_secs_f32(), expected);
    }

    /// [`Self::advance`] with the elapsed time supplied rather than read from the
    /// clock.
    ///
    /// Split out purely so the ramp is testable: a state machine whose only input is
    /// `Instant::now()` can be asserted for *direction* and never for *magnitude*,
    /// and magnitude is the whole question here (a gate that accepts any nonzero
    /// rate is satisfied by a rate that is wrong by 2×, which is how a 70%-vs-30%
    /// shader bug shipped in this repo).
    fn advance_by(&mut self, dt: f32, expected: Option<&ResourceLocation>) {
        self.accumulator += dt;
        // A bounded catch-up. A tab-out, a breakpoint, a menu the shell returns from
        // or a slow first frame after a resource load can hand us an arbitrarily
        // large gap; a full swap is 6 ticks, so 20 is generously past "the animation
        // has finished either way" and the loop cannot become a hang.
        let mut steps = 0;
        while self.accumulator >= TICK && steps < 20 {
            self.accumulator -= TICK;
            self.step(expected);
            steps += 1;
        }
        if self.accumulator >= TICK {
            self.accumulator = 0.0;
        }
    }

    /// One 20 Hz step — `ItemInHandRenderer.tick()`, main hand only, in vanilla's
    /// own order.
    ///
    /// The order is load-bearing at both ends. The *pre*-step value is saved first
    /// (that is what `oMainHandHeight` is for), and the visible-item exchange is
    /// checked **after** the ramp, so the item swaps on the tick the height reaches
    /// the bottom rather than the tick after.
    fn step(&mut self, expected: Option<&ResourceLocation>) {
        self.previous = self.height;
        // `shouldInstantlyReplaceVisibleItem`: vanilla's `matchesIgnoringComponents`
        // plus the item model's `handAnimationOnSwap` opt-out.
        //
        // **Reduced to an item-id comparison here, and that loses two triggers.**
        // Vanilla compares whole `ItemStack`s, so `getCount()` and the component map
        // both participate: eating one bread out of a stack, or a pickaxe taking a
        // point of damage, re-triggers the dip. The shell's main-hand source is
        // narrowed to a bare `ResourceLocation` well upstream of here (`app.rs`
        // builds it from `HotbarSlot::item`), so a same-item change is invisible to
        // this function and only a genuine item swap animates. That is the
        // conservative direction: over-triggering would dip the hand on every
        // durability tick while mining.
        //
        // The `handAnimationOnSwap` opt-out (`ItemModelResolver::shouldPlaySwapAnimation`,
        // default `true`, overridden per item-model definition) is likewise not
        // reachable from an item id alone, so every item animates.
        if self.visible.as_ref() == expected {
            let target = EQUIP_REST_HEIGHT;
            self.height += (target - self.height).clamp(-EQUIP_RATE_PER_TICK, EQUIP_RATE_PER_TICK);
        } else {
            // `mainHandItem != nextMainHand` ⇒ target 0: lower what is on screen.
            self.height += (0.0 - self.height).clamp(-EQUIP_RATE_PER_TICK, EQUIP_RATE_PER_TICK);
            if self.height < EQUIP_SWAP_BELOW {
                self.visible = expected.cloned();
            }
        }
    }

    /// Vanilla's `inverseArmHeight` for this frame:
    /// `swapAnimationScale(item) · (1 - Mth.lerp(frameInterp, oHeight, height))`.
    ///
    /// `swapAnimationScale` is the item model definition's `swap_animation_scale`,
    /// **defaulting to `1.0`** (`ItemModelResolver.swapAnimationScale` returns `1.0F`
    /// for a stack with no `minecraft:item_model` component). The item pipeline does
    /// not read item-model definitions, so `1.0` is used for every item — the
    /// per-item override is the only thing missing, not the animation.
    ///
    /// # The lerp of a clamped step is a straight line, and that is why this is right
    ///
    /// `height` moves by at most `±0.4` per tick, so `lerp(p, previous, height)` is
    /// `previous ± 0.4p` — a continuous ramp of slope `0.4` per tick, i.e. **8.0 per
    /// second**, with no discontinuity at a tick boundary. Predicting the value is
    /// therefore arithmetic rather than a simulation. From rest, `t` seconds into a
    /// swap (`t < 0.125`) this returns exactly `8.0 · t`: a quarter of a tick in,
    /// `0.1`, which puts the item `0.1 · -0.6 = -0.06` blocks below its resting
    /// `-0.52`; a full tick in, `0.4`, i.e. `-0.24` blocks.
    ///
    /// Note the value at a tick *boundary* is last tick's, not this tick's — `p == 0`
    /// selects `previous`. That is vanilla's own phasing (`tick()` runs, then frames
    /// interpolate forward across the following tick) and it is the thing to check
    /// first if the dip looks one tick early or late. Halving the rate, dropping the
    /// lerp, reversing it, or advancing per frame instead of per tick each land on a
    /// different number at the same instant — a gate that only asserts "it moved"
    /// cannot tell any of them apart.
    fn inverse_arm_height(&self) -> f32 {
        let partial = (self.accumulator / TICK).clamp(0.0, 1.0);
        1.0 - (self.previous + (self.height - self.previous) * partial)
    }

    /// The item to **draw** this frame — vanilla's `mainHandItem`, not the selected
    /// one. `None` draws the bare arm.
    fn visible(&self) -> Option<&ResourceLocation> {
        self.visible.as_ref()
    }
}

/// What the first-person hand pass draws this frame: the held item's model, or
/// the bare arm. **Never both** — see
/// [`RenderState::prepare_first_person_hand`], which is vanilla's own
/// `isEmpty()` branch.
pub(super) enum FirstPersonHand<'a> {
    /// The held item, meshed camera-space and drawn through the *model* pipeline
    /// with the model pass's own `hand_cam_bind_group`.
    Item(GpuModelMesh),
    /// The bare arm, drawn through the *entity* pipeline.
    Arm(FirstPersonArm<'a>),
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

impl RenderState {
    /// Build this frame's first-person hand draw — **the held item, or the bare
    /// arm**, never both.
    ///
    /// # Which one, and why it is exclusive
    ///
    /// Vanilla's `ItemInHandRenderer.submitArmWithItem` branches on
    /// `itemStack.isEmpty()`: the empty hand gets `renderPlayerArm`, and a
    /// non-empty one gets the *item* through `applyItemArmTransform` **with no arm
    /// drawn at all**. So this returns a [`FirstPersonHand`] and the caller draws
    /// exactly one of its two variants. Drawing both — the tempting "add the item
    /// on top of the arm" reading — puts an item model inside the wrist.
    ///
    /// [`MainHandSource`] decides. Unset yields `None` and the bare-arm branch,
    /// which is what this shell did before the item path existed. An item that is
    /// held but has no baked geometry (a `IconPart::Special` chest or shield) also
    /// falls back to the arm rather than to nothing: vanilla would draw the special
    /// renderer, and a bare arm is closer to that than an empty screen.
    ///
    /// Also rewrites the arm pass's group-0 uniform. That uniform's `view_proj`
    /// is [`hand_projection`] — **the projection alone** — because
    /// `GameRenderer.renderItemInHand` multiplies the pose stack by
    /// `modelViewMatrix.invert()` while pushing `modelViewStack.mul(modelViewMatrix)`,
    /// and the shader evaluates `Proj · ModelViewStack · PoseStack`: the view
    /// rotation cancels exactly, leaving a camera-space pose. Feeding
    /// `Camera::view_projection` here instead would leave the arm parked at the
    /// world origin, visible only when the player stands on it.
    ///
    /// # Unconditional, and why that is right rather than lazy
    ///
    /// This is not gated on anything. `RenderState::render` is only reached
    /// in-world (`app.rs` returns early for every menu screen) and the shell has
    /// no third-person camera, so "first person, in a world" is exactly when this
    /// function runs. Making it opt-in would have needed a setter on `&mut self`
    /// and therefore an `app.rs` call — i.e. it would have shipped as another
    /// zero-pixel island.
    ///
    /// # The swing
    ///
    /// The pose is driven by [`HandSwingSource`] — vanilla's `attackValue`, a
    /// tick-advanced clock read with this frame's partial tick. It is polled here
    /// rather than passed in for the same reason the light and sky-darken samplers
    /// are: `render` takes only `&[EntityDraw]`, and the local player is not in it.
    ///
    /// **With no source installed this is `0.0` and the arm is rested**, which is
    /// the state to suspect first if a swing does not appear — the pass runs and
    /// `first_person_arm_drawn` is `true` either way, so a missing
    /// `set_hand_swing_source` looks exactly like a working rested arm. See
    /// `docs/arm-swing-animation.md`.
    ///
    /// # The equip/swap dip (issue #366)
    ///
    /// [`HeldItemEquip`] is vanilla's `ItemInHandRenderer` swap state, advanced in
    /// [`RenderState::set_main_hand_source`] and read here for **both** branches.
    /// Two things it changes about this function: the arm/item fork is decided by
    /// the *visible* item rather than the selected one, and both poses take an
    /// `inverseArmHeight` instead of a hardcoded `0.0`.
    ///
    /// # The remaining fidelity gap, missing *shell state*, not code
    ///
    /// * **`bobView` / `bobHurt` and the `xBob`/`yBob` view lag are absent.** All
    ///   need per-tick player state the shell does not track (walk distance, hurt
    ///   time, the two smoothed view angles). All are the identity standing still.
    ///
    /// The rig is `player_wide` unconditionally — the shell has no skin-model
    /// signal, and `canonical_model_name` already maps `"player"` to it.
    pub(super) fn prepare_first_person_hand<'a>(
        &'a self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
    ) -> Option<FirstPersonHand<'a>> {
        const ARM: Arm = Arm::Right;

        // Group 0 for *both* branches: `hand_projection` alone. Written before
        // either branch can return, so the arm's uniform is never left holding a
        // stale projection from a frame that drew an item (and vice versa).
        self.write_hand_camera(queue, camera);

        // `inverseArmHeight` for both branches (issue #366) — vanilla's own single
        // scalar, read once so the arm and the item cannot disagree about how far
        // the hand is lowered on the frame a swap crosses between them.
        let inverse_arm_height = self.equip.inverse_arm_height();

        // The item branch first: it needs no entity rig at all, so a missing
        // `player_wide` mesh must not silently suppress a held item too.
        //
        // **`equip.visible()`, not `main_hand.value()`** — vanilla branches on
        // `this.mainHandItem`, the item currently *drawn*, which lags the selected
        // one until the dip bottoms out. Branching on the selected item instead is
        // what makes a swap look like the new item dropping out of frame and coming
        // back, and it is the natural mistake: `main_hand` is right there and reads
        // like the answer.
        if let Some(item) = self.equip.visible()
            && let Some(model) = self.model.as_ref()
            && let Some(geometry) = model.items.get(item)
        {
            // `true`: the *first-person* hand slot. `false` here reads
            // `thirdperson_righthand`, a different rotation and scale, and puts
            // the item at a plausible-but-wrong angle rather than off screen.
            let transform = hand_transform(&geometry.display, ARM, true);
            let mesh = first_person_item_mesh(
                &geometry.quads,
                geometry.gui_light,
                ARM,
                self.hand_swing.value(),
                inverse_arm_height,
                &transform,
                u8::try_from(self.hand_light(camera)).unwrap_or(u8::MAX),
            );
            if let Some(gpu) = GpuModelMesh::upload(device, &mesh) {
                return Some(FirstPersonHand::Item(gpu));
            }
        }

        let entry = model_for_type("player")?;
        let mesh = self.entities.models.get(entry.name)?;
        let gpu = self.entities.gpu_models.get(entry.name)?;
        let texture = self.entities.textures.get(entry.name)?;
        // The bare arm takes the *same* dip: `renderPlayerArm` is called with the
        // very `inverseArmHeight` `submitArmWithItem` computed for the item branch
        // (`ItemInHandRenderer.java:446`), so swapping an item away for an empty slot
        // lowers the item and raises the arm as one continuous motion.
        let pose =
            first_person_arm_pose_with_equip(mesh, ARM, self.hand_swing.value(), inverse_arm_height)?;

        let light = self.hand_light(camera);

        let parts: Vec<(lodestone_render::entity::PartRange, wgpu::Buffer)> =
            first_person_arm_parts(mesh, ARM)
                .into_iter()
                .filter_map(|index| {
                    let range = *gpu.parts.get(index)?;
                    if range.index_count == 0 {
                        return None;
                    }
                    // One instance, and the *same* matrix for arm and sleeve —
                    // `right_sleeve` is a `PartPose::ZERO` child of `right_arm`,
                    // so they share it exactly.
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

    /// The packed light byte the first-person hand is lit with, for both branches.
    ///
    /// Exactly `renderItemInHand`'s `getPackedLightCoords(minecraft.player, …)`,
    /// sampled at the **eye** rather than the feet: it is what the player is
    /// looking through, and the two only differ standing in a doorway.
    #[must_use]
    fn hand_light(&self, camera: &Camera) -> u32 {
        u32::from(self.entity_light.sample(camera.position))
    }

    /// Rewrite both hand passes' group-0 uniforms with [`hand_projection`].
    ///
    /// **The projection alone, with no view matrix**, because
    /// `GameRenderer.renderItemInHand` multiplies the pose stack by
    /// `modelViewMatrix.invert()` while pushing `modelViewStack.mul(modelViewMatrix)`
    /// and the shader evaluates `Proj · ModelViewStack · PoseStack`: the view
    /// rotation cancels exactly, leaving a camera-space pose. Feeding
    /// `Camera::view_projection` here instead parks the hand at the world origin,
    /// visible only when the player stands on it.
    ///
    /// Two buffers, one value: the entity pipeline (bare arm) and the model
    /// pipeline (held item) declare different group-0 layouts, so each needs its
    /// own. Written together here so they cannot drift.
    fn write_hand_camera(&self, queue: &wgpu::Queue, camera: &Camera) {
        let camera_uniform = CameraUniform {
            view_proj: hand_projection(camera.aspect).to_cols_array_2d(),
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
        // which leaves the spare lane at its `0.0`/"unwired" sentinel, and the
        // model shader's `sky_darken()` reads that sentinel as permanent
        // noon. That was issue #74's actual bug — not a missing light sample
        // (`hand_light` already samples real per-position world light for
        // both branches; see its own doc), but the held item's sky component
        // never darkening: at night, in the open, the item stayed lit as if
        // it were noon while the arm right next to it correctly dimmed.
        let mut hand_fog = FogUniform::disabled();
        hand_fog.end_enabled[2] = self.sky_darken.value();
        queue.write_buffer(
            &self.entities.hand_cam_buffer,
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
    }

    /// Record the first-person arm/held-item pass: its own render pass, with
    /// the depth buffer cleared.
    ///
    /// Vanilla does exactly this, and it is not an optimisation detail:
    /// `GameRenderer.renderLevel` calls
    /// `clearDepthTexture(mainRenderTarget.getDepthTexture(), 0.0)`
    /// immediately before `renderItemInHand`. Vanilla's depth is reversed-Z,
    /// so its `0.0` is *far*; ours is `[0,1]` DirectX-style, so the equivalent
    /// clear value is `1.0`. (This is the sign flip `CLAUDE.md` warns about,
    /// applied to a clear rather than a comparison.)
    ///
    /// Without the clear the arm would be occluded by any block within ~0.75
    /// blocks of the eye — standing in a doorway, or facing the block you are
    /// mining — because the arm genuinely *is* inside that geometry. The
    /// colour attachment loads rather than clears, so the world stays.
    pub(super) fn draw_first_person_hand(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        hand: &FirstPersonHand<'_>,
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
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        match hand {
            // The held item is item-model geometry, so it draws through the
            // *model* pipeline with that pipeline's four bind groups — the
            // same atlas, palette and animation slots the terrain and the
            // hotbar icons use. Only group 0 differs: the hand projection.
            FirstPersonHand::Item(mesh) => {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str) -> ResourceLocation {
        ResourceLocation::new("minecraft", path).unwrap()
    }

    /// The state a `RenderState` that nobody ever gave a main-hand source must be
    /// in: **no dip at all**.
    ///
    /// This is the regression a derived `Default` produces (`height == 0.0` ⇒
    /// `inverse_arm_height == 1.0` ⇒ the bare arm sits 0.6 blocks below frame), and
    /// it would show up nowhere except as "the first-person arm disappeared" in the
    /// `#[ignore]`d GPU gates. Asserted rather than commented for that reason.
    #[test]
    fn an_uninstalled_equip_state_is_fully_equipped() {
        let equip = HeldItemEquip::default();
        assert_eq!(equip.inverse_arm_height(), 0.0);
        assert_eq!(equip.visible(), None);
    }

    /// The first observation seeds at rest instead of animating up from zero — see
    /// `HeldItemEquip::last`. A single-frame caller must see a rested hand holding
    /// the item it asked for, not a hand mid-raise.
    #[test]
    fn the_first_observation_seeds_at_rest() {
        let pickaxe = item("diamond_pickaxe");
        let mut equip = HeldItemEquip::default();
        equip.advance(Some(&pickaxe));
        assert_eq!(equip.visible(), Some(&pickaxe));
        assert_eq!(equip.inverse_arm_height(), 0.0);
    }

    /// **The magnitude gate on the ramp itself.** The per-tick height sequence must
    /// be the one vanilla's `Mth.clamp(target - height, -0.4F, 0.4F)` produces from
    /// rest, not merely a decreasing one.
    ///
    /// Truth, from `1.0` toward `0.0`: `0.6, 0.2, 0.0`. Two wrong readings of the
    /// same source line, both of which any "it went down" assertion accepts:
    ///
    /// * **`0.4` of the *remaining distance* per tick** rather than an absolute
    ///   `0.4` step — a plausible misreading of `clamp`, and it gives
    ///   `0.6, 0.36, 0.216`: the same first value and never reaching the bottom at
    ///   all, so the item would never be exchanged. The second tick separates them
    ///   (`0.2` against `0.36`).
    /// * **half the rate** (`0.2`, reading the clamp bound as the full swing rather
    ///   than the per-tick step) gives `0.8, 0.6, 0.4`.
    #[test]
    fn the_swap_ramp_steps_by_exactly_the_vanilla_rate() {
        let pickaxe = item("diamond_pickaxe");
        let sword = item("diamond_sword");
        let mut equip = HeldItemEquip::default();
        equip.advance(Some(&pickaxe));

        equip.advance_by(TICK, Some(&sword));
        assert!(
            (equip.height - 0.6).abs() < 1e-6,
            "one tick in, height must be 0.6; got {}",
            equip.height
        );
        // Still the *old* item on screen: the exchange happens at the bottom.
        assert_eq!(equip.visible(), Some(&pickaxe));

        equip.advance_by(TICK, Some(&sword));
        assert!(
            (equip.height - 0.2).abs() < 1e-6,
            "two ticks in, height must be 0.2 — not the 0.36 a proportional ramp \
             gives, nor the 0.6 a half-rate one does; got {}",
            equip.height
        );
        assert_eq!(equip.visible(), Some(&pickaxe));
    }

    /// The visible item is exchanged **at the bottom of the dip**, and the hand
    /// comes back up afterwards.
    ///
    /// Vanilla's height sequence from rest is `0.6, 0.2, 0.0` (the last step is
    /// short because the change is clamped to the remaining distance) with
    /// `mainHandItem = next` on the tick `height < 0.1` — so three ticks down, then
    /// `0.4, 0.8, 1.0` back up. The full swap is six ticks: **300 ms**.
    #[test]
    fn the_item_is_exchanged_at_the_bottom_and_the_hand_rises_again() {
        let pickaxe = item("diamond_pickaxe");
        let sword = item("diamond_sword");
        let mut equip = HeldItemEquip::default();
        equip.advance(Some(&pickaxe));

        let mut heights = Vec::new();
        let mut swap_tick = None;
        for tick in 0..8 {
            equip.advance_by(TICK, Some(&sword));
            heights.push(equip.height);
            if swap_tick.is_none() && equip.visible() == Some(&sword) {
                swap_tick = Some(tick);
            }
        }
        assert_eq!(
            swap_tick,
            Some(2),
            "the exchange must land on the third tick, where height first goes \
             below 0.1; heights were {heights:?}"
        );
        // 0.6, 0.2, 0.0 down; 0.4, 0.8, 1.0 up; then rest.
        let expected = [0.6, 0.2, 0.0, 0.4, 0.8, 1.0, 1.0, 1.0];
        for (got, want) in heights.iter().zip(expected) {
            assert!(
                (got - want).abs() < 1e-6,
                "height sequence {heights:?} does not match vanilla's {expected:?}"
            );
        }
        assert_eq!(equip.inverse_arm_height(), 0.0, "the swap must finish rested");
    }

    /// Re-installing the *same* item every frame must not animate anything.
    ///
    /// `shouldInstantlyReplaceVisibleItem` is the reason: an unchanged selection
    /// matches and is adopted instantly, so the target stays at rest. Without that
    /// branch the hand would dip continuously, because `app.rs` re-installs the
    /// source on every single frame.
    #[test]
    fn holding_the_same_item_never_dips() {
        let pickaxe = item("diamond_pickaxe");
        let mut equip = HeldItemEquip::default();
        equip.advance(Some(&pickaxe));
        for _ in 0..40 {
            equip.advance_by(TICK, Some(&pickaxe));
            assert_eq!(equip.inverse_arm_height(), 0.0);
        }
    }

    /// **The magnitude gate on what actually reaches the pose matrix.**
    /// `inverseArmHeight` is `1 - Mth.lerp(frameInterp, oHeight, height)` — the
    /// partial-tick lerp runs from *last* tick's value to this one, so at
    /// `frameInterp == 0` the drawn hand is still where it was a tick ago and the dip
    /// arrives continuously across the tick rather than in a step.
    ///
    /// Measured a **quarter** tick past the first step, where `oHeight == 1.0` and
    /// `height == 0.6`: the drawn height is `1.0 - 0.4·0.25 == 0.9` and
    /// `inverse_arm_height` is `0.1`.
    ///
    /// The quarter (rather than a half) is deliberate — it separates three
    /// hypotheses a half cannot:
    ///
    /// * **lerping backwards** (`lerp(p, height, oHeight)`) gives `0.7` drawn,
    ///   `0.3` inverse. At a half tick both readings give `0.8`/`0.2` and the test
    ///   passes on a reversed lerp.
    /// * **no partial-tick lerp at all** (drawing `height` directly) gives `0.4`
    ///   inverse — a hand that jumps 0.24 blocks once per tick instead of gliding.
    /// * **`height` passed through unsubtracted** gives `0.9`, a hand that rises out
    ///   of frame on a swap.
    #[test]
    fn the_partial_tick_lerp_lands_on_the_predicted_value() {
        let pickaxe = item("diamond_pickaxe");
        let sword = item("diamond_sword");
        let mut equip = HeldItemEquip::default();
        equip.advance(Some(&pickaxe));

        equip.advance_by(TICK, Some(&sword));
        assert!(
            equip.inverse_arm_height().abs() < 1e-6,
            "at the tick boundary the drawn hand is still at last tick's rest, so \
             the dip must be 0.0; got {}",
            equip.inverse_arm_height()
        );
        equip.advance_by(TICK * 0.25, Some(&sword));
        assert!(
            (equip.inverse_arm_height() - 0.1).abs() < 1e-6,
            "a quarter tick into the first step the dip must be 0.1 (drawn height \
             0.9); got {}",
            equip.inverse_arm_height()
        );
    }

    /// Swapping to an **empty** slot lowers the item and then draws the bare arm —
    /// the arm/item fork follows the *visible* item, so the transition is one
    /// continuous motion rather than an item vanishing.
    #[test]
    fn putting_an_item_away_lowers_it_before_the_arm_appears() {
        let pickaxe = item("diamond_pickaxe");
        let mut equip = HeldItemEquip::default();
        equip.advance(Some(&pickaxe));

        equip.advance_by(TICK, None);
        assert_eq!(
            equip.visible(),
            Some(&pickaxe),
            "the item must still be drawn while it lowers"
        );
        equip.advance_by(TICK * 2.0, None);
        assert_eq!(equip.visible(), None, "the hand must be empty at the bottom");
        assert!(
            equip.inverse_arm_height() > 0.5,
            "the arm must appear still lowered, not at rest; got {}",
            equip.inverse_arm_height()
        );
    }

    /// A frame gap longer than the whole animation must land on the finished state,
    /// not somewhere arbitrary in the middle — the catch-up cap must not truncate a
    /// swap that a tab-out spanned.
    #[test]
    fn a_long_frame_gap_completes_the_swap() {
        let pickaxe = item("diamond_pickaxe");
        let sword = item("diamond_sword");
        let mut equip = HeldItemEquip::default();
        equip.advance(Some(&pickaxe));
        equip.advance_by(5.0, Some(&sword));
        assert_eq!(equip.visible(), Some(&sword));
        assert_eq!(equip.inverse_arm_height(), 0.0);
    }
}
