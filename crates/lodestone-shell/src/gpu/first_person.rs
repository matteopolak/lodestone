//! The first-person hand pass: the bare arm or the held item, drawn in its
//! own render pass with the depth buffer cleared (vanilla's
//! `GameRenderer.renderLevel` does the same before `renderItemInHand`). See
//! [`RenderState::prepare_first_person_hand`] for the vanilla parity notes
//! and `docs/arm-swing-animation.md`.
use lodestone_assets::ResourceLocation;
use lodestone_render::{
    Camera, CameraUniform, EntityCameraUniform, GpuEntityModel, GpuModelMesh, ItemStateContext,
    entity::{
        Arm, first_person_arm_parts, first_person_arm_pose_with_equip, first_person_item_mesh,
        hand_projection, hand_transform, model_for_type,
    },
    fog::FogUniform,
    update_model_shared_camera_buffer, upload_instances,
};

use crate::camera_rig::BobFrame;

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
    ///
    /// The pair's `bool` is the enchantment-foil flag (issue #452), carried
    /// because the glint second pass is gated on it and the flag must follow the
    /// *drawn* item — a swap that raises an enchanted sword glints the sword the
    /// moment it appears, not the stack the player selected two ticks ago.
    visible: Option<(ResourceLocation, bool)>,
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
    last: Option<crate::platform::Instant>,
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
    pub(super) fn advance(&mut self, expected: Option<&(ResourceLocation, bool)>) {
        let now = crate::platform::Instant::now();
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
    fn advance_by(&mut self, dt: f32, expected: Option<&(ResourceLocation, bool)>) {
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
    fn step(&mut self, expected: Option<&(ResourceLocation, bool)>) {
        self.previous = self.height;
        // `shouldInstantlyReplaceVisibleItem`: vanilla's `matchesIgnoringComponents`
        // plus the item model's `handAnimationOnSwap` opt-out.
        //
        // **Reduced to an (id, foil) comparison here, and that loses two triggers.**
        // Vanilla compares whole `ItemStack`s, so `getCount()` and the rest of the
        // component map both participate: eating one bread out of a stack, or a
        // pickaxe taking a point of damage, re-triggers the dip. The shell's
        // main-hand source is narrowed to the id plus the enchantment-foil flag
        // (`app.rs` builds it from `HotbarSlot::{item, enchanted}`), so a same-item
        // change is invisible to this function and only a genuine item swap — or a
        // swap of the stack's foil state — animates. That is the conservative
        // direction: over-triggering would dip the hand on every durability tick
        // while mining.
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
    /// one — plus its enchantment-foil flag (the glint gate, issue #452). `None`
    /// draws the bare arm.
    pub(super) fn visible(&self) -> Option<&(ResourceLocation, bool)> {
        self.visible.as_ref()
    }
}

// ---------------------------------------------------------------------------
// The walk/hurt bob reaches the hand (issue #58 follow-up)
// ---------------------------------------------------------------------------

/// A `damage_tilt_strength` of zero, for the gates below that isolate a single
/// `bobView` term and need the hurt half provably inert.
///
/// **This used to be `HAND_HURT_TILT_STRENGTH`, a *production* constant holding
/// `bobHurt` off, and its stated blocker was already stale when it was read.** The
/// blocker was that `Sim::bob_frame` returned `BobFrame::default()` whole-cloth
/// when View Bobbing was off, zeroing `hurt`/`hurt_dir_degrees` along with the walk
/// terms — so a nonzero strength would have muted the damage tilt for anyone who
/// turned View Bobbing off, which vanilla does not do (`renderLevel` calls
/// `bobHurt` outside the `bobView` check). That was true when written and had since
/// been fixed: `bob_frame` now zeroes **only** `walk_phase`/`bob` and passes the
/// hurt half through untouched. The hand therefore draws the real strength, and
/// this constant survives only as the gates' zero anchor.
#[cfg(test)]
const NO_DAMAGE_TILT: f32 = 0.0;

/// Where this frame's walk/hurt bob comes from, for the first-person hand pass
/// — polled once per frame like [`super::HandSwingSource`]/[`super::MainHandSource`].
///
/// # Why the hand needs its *own* source rather than reading `camera`
///
/// `camera: &Camera`, passed into [`RenderState::prepare_first_person_hand`],
/// is already [`crate::sim::camera`]'s **folded** render camera —
/// `Sim::render_camera` bakes
/// [`BobFrame::eye_transform`] into the camera's position/yaw/pitch via
/// [`crate::camera_rig::bobbed_camera`], mirroring vanilla's own
/// `GameRenderer.renderLevel`'s
/// `projectionMatrix.mul(bobStack.last().pose())` — the bob folded into the
/// **world's** projection matrix.
///
/// Vanilla's hand path (`GameRenderer.renderItemInHand`, same file, `:333-362`)
/// does not read that folded value at all. It builds a **second, independent**
/// `PoseStack`, seeds it with `modelViewMatrix.invert()` — the camera's
/// *unbobbed* view rotation — and then applies `bobHurt`/`bobView` to *that*
/// stack a second time (`:344-347`). The GPU's own model-view is pushed as the
/// very same unbobbed `modelViewMatrix` (`:342-343`), so at draw time the
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
/// value has to reach here from `Sim::bob_frame()`
/// (`sim/camera.rs:320-325`), which nothing below the GPU boundary can read.
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

/// The hand pass's whole camera-space transform: [`hand_projection`] composed
/// with a fresh copy of the walk/hurt bob — vanilla's own second application
/// of it (`GameRenderer.java:344-347`), described in full on
/// [`HandBobSource`]'s doc.
///
/// **Post-multiplied, matching vanilla's own `projectionMatrix.mul(bobStack.
/// pose())` (`GameRenderer.java:539`)** — the bob lands between the projection
/// and the already-camera-space arm/item pose, exactly where vanilla's own
/// `Proj · ModelViewStack · PoseStack` puts it once the view rotation cancels
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
fn hand_view_proj(aspect: f32, bob: BobFrame, damage_tilt_strength: f32) -> glam::Mat4 {
    hand_projection(aspect) * bob.eye_transform(damage_tilt_strength)
}

/// What the first-person hand pass draws this frame: the held item's model, or
/// the bare arm. **Never both** — see
/// [`RenderState::prepare_first_person_hand`], which is vanilla's own
/// `isEmpty()` branch.
pub(super) enum FirstPersonHand<'a> {
    /// The held item, meshed camera-space and drawn through the *model* pipeline
    /// with the model pass's own `hand_cam_bind_group`. The `bool` is the
    /// enchantment-foil flag: when `true`, [`RenderState::draw_first_person_hand`]
    /// re-rasterises the same mesh through the glint pipeline in the same pass
    /// (issue #452).
    Item(GpuModelMesh, bool),
    /// A held **filled map** (issue #184): one quad drawn through the same model
    /// pipeline as [`Self::Item`], with group 1 swapped from the block atlas to the
    /// map's own 128×128 texture. The bind group travels with the mesh because the
    /// two are meaningless apart — see `super::maps`.
    Map(GpuModelMesh, wgpu::BindGroup),
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
    /// * **`bobView` now reaches the hand (issue #58 follow-up, the player
    ///   report that "the arm should bob too").** See [`HandBobSource`]'s doc
    ///   for the derivation — it is vanilla's own **second, independent**
    ///   application of the identical [`BobFrame`] the world's camera already
    ///   folds, not something inherited from `camera`.
    /// * **`bobHurt` reaches the hand *mechanically* but is held at `0.0`
    ///   (`HAND_HURT_TILT_STRENGTH`).** The transform is proven — see that
    ///   constant's own doc — but landing it needs a small fix to
    ///   `Sim::bob_frame` first, or it would silently drop the tilt whenever a
    ///   player has View Bobbing off, which vanilla does not do.
    /// * **The `xBob`/`yBob` view lag is still absent** — a *different* feature
    ///   (the hand trailing behind camera rotation, not the walk bob), needing
    ///   the two smoothed view angles, which the shell does not track. Tracked
    ///   as its own issue rather than folded in here; see `docs/view-bobbing.md`.
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
        // stale projection from a frame that drew an item (and vice versa). The
        // returned matrix is the very `view_proj` the base item pass draws with,
        // which the glint pass must reuse verbatim (depth-`EQUAL`); see
        // `write_hand_camera`'s return value.
        let view_proj = self.write_hand_camera(queue, camera);

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
        // `firstperson_righthand`, resolved rather than assumed. This is the pass
        // the `display_context` branch exists for: 26 of 26.2's items name a
        // *different model* in the hand than in the inventory slot, and baking one
        // form per item drew `item/spyglass`'s flat sprite here instead of
        // `item/spyglass_in_hand`'s 3-D tube — and then posed it with
        // `item/generated`'s `firstperson_righthand` rather than the in-hand
        // model's, because `ItemIcon::display` is the first drawable part's map.
        //
        // `using` is `false`: the local player's using-item state has no fold on
        // this side yet (see `docs/item-variants.md` — it needs a `Vitals`-shaped
        // session component and a `PlayerSnapshot` line in `sim.rs`), so *our own*
        // bow still draws slack while a remote player's and a mob's do not.
        // `ARM.display_slot(true)` — the same expression `hand_transform` below
        // reads the pose from, so the resolved variant and its transform cannot
        // disagree about which slot this pass is.
        // A filled map first (issue #184): vanilla forks *before* the ordinary
        // item pose too — `renderArmWithItem` tests `MapItem.isFilledMap` and
        // calls `renderMap`, which is a textured quad and not the item's baked
        // model. Falling through would draw `item/filled_map`'s flat blank sprite,
        // which looks like a working map until you notice it has no terrain on it.
        if let Some((mesh, texture)) = self.prepare_held_map(device, queue, inverse_arm_height) {
            return Some(FirstPersonHand::Map(mesh, texture));
        }

        let hand_ctx = ItemStateContext::new(ARM.display_slot(true));
        if let Some((item, foil)) = self.equip.visible()
            && let Some(model) = self.model.as_ref()
            && let Some(geometry) = model.items.get(item).and_then(|v| v.resolve(&hand_ctx))
        {
            // `true`: the *first-person* hand slot. `false` here reads
            // `thirdperson_righthand`, a different rotation and scale, and puts
            // the item at a plausible-but-wrong angle rather than off screen.
            //
            // `geometry.display` is now the **resolved variant's** map, so this
            // reads `item/spyglass_in_hand`'s own transforms — which declare no
            // `firstperson_righthand` at all, i.e. vanilla poses it with the
            // identity, not with `item/generated`'s `[0, -90, 25]` / 0.68.
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
                // An enchanted held item gets the glint second pass. The uniform
                // is written now (this is the `&self` + queue point in the frame)
                // and consumed by the draw later in the same frame: one buffer,
                // rewritten per glint draw. No pass installed (jar-less) is a
                // no-op and the item still draws unglinted.
                if *foil {
                    self.write_glint_uniform(queue, view_proj);
                }
                return Some(FirstPersonHand::Item(gpu, *foil));
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
    /// `renderItemInHand`'s `getPackedLightCoords(minecraft.player, partialTick)`.
    ///
    /// # The eye is not a deviation, and the byte is not one channel
    ///
    /// This doc used to read "sampled at the **eye** rather than the feet", framed
    /// as a departure we chose. It is not — it is what vanilla does. Following the
    /// call through:
    ///
    /// ```java
    /// // EntityRenderer.java:48-50
    /// BlockPos blockPos = BlockPos.containing(entity.getLightProbePosition(partialTickTime));
    /// return LightCoordsUtil.pack(this.getBlockLightLevel(entity, blockPos),
    ///                             this.getSkyLightLevel(entity, blockPos));
    /// // Entity.java:2001-2003
    /// public Vec3 getLightProbePosition(final float partialTickTime) {
    ///    return this.getEyePosition(partialTickTime);
    /// }
    /// ```
    ///
    /// And the `u32::from(u8)` is a widen, not a truncation to a single channel:
    /// [`EntityLightSource::sample`](super::sources::EntityLightSource) returns
    /// vanilla's **packed** pair — sky in the high nibble, block in the low (see
    /// [`lodestone_render::ENTITY_FULLBRIGHT`], which is `15 << 4`) — and
    /// `entity.wgsl:180-181` unpacks both:
    ///
    /// ```wgsl
    /// let sky = f32((light >> 4u) & 15u) / 15.0;
    /// let block = f32(light & 15u) / 15.0;
    /// ```
    ///
    /// So the hand is lit by exactly the same two-channel value every mob is, and
    /// this is the same call `entity_passes.rs` makes for them. The clock term
    /// rides the uniform rather than the byte — see `write_hand_camera`'s note on
    /// issue #74, which was the last real defect here.
    ///
    /// The one measurable difference left from vanilla is that `camera.position`
    /// has the view bob folded into it (`camera_rig::bobbed_camera`), so the probe
    /// wanders by up to `0.05` blocks while walking where vanilla's
    /// `getEyePosition` does not. That can flip the sampled block across a
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

    /// Rewrite both hand passes' group-0 uniforms with [`hand_view_proj`].
    ///
    /// **No view matrix, but now a bob matrix**, because
    /// `GameRenderer.renderItemInHand` multiplies the pose stack by
    /// `modelViewMatrix.invert()` while pushing `modelViewStack.mul(modelViewMatrix)`
    /// and the shader evaluates `Proj · ModelViewStack · PoseStack`: the view
    /// rotation cancels exactly, leaving a camera-space pose. Feeding
    /// `Camera::view_projection` here instead parks the hand at the world origin,
    /// visible only when the player stands on it. [`hand_view_proj`]'s own doc
    /// (and [`HandBobSource`]'s) has the rest: vanilla applies `bobHurt`/`bobView`
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
        // Vanilla applies `bobHurt` to this pass a **second** time, independently
        // of the world's copy, and it reaches the hand without any lossy fold:
        // `hand_view_proj` multiplies the raw bob matrix straight into the hand's
        // projection, so roll survives here where it cannot survive
        // `bobbed_camera`.
        let view_proj = hand_view_proj(
            camera.aspect,
            self.hand_bob.value(),
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
        view_proj.to_cols_array_2d()
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
                    // The uniform was written by `prepare_first_person_hand`
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
            // vanilla's `renderMap` draws no foil, and a map is not enchantable.
            FirstPersonHand::Map(mesh, texture) => {
                if let Some(model) = self.model.as_ref() {
                    pass.set_pipeline(&model.pipeline.pipeline);
                    pass.set_bind_group(
                        0,
                        &model.hand_cam_bind_group,
                        &[model.origin_arena.zero_offset()],
                    );
                    pass.set_bind_group(1, texture, &[]);
                    pass.set_bind_group(2, &model.palette_bind_group, &[]);
                    pass.set_bind_group(3, &model.anim_bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    stats.draw_calls += 1;
                    stats.filled_maps_drawn += 1;
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
        let pickaxe = (item("diamond_pickaxe"), false);
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
        let pickaxe = (item("diamond_pickaxe"), false);
        let sword = (item("diamond_sword"), false);
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
        let pickaxe = (item("diamond_pickaxe"), false);
        let sword = (item("diamond_sword"), false);
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
        let pickaxe = (item("diamond_pickaxe"), false);
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
        let pickaxe = (item("diamond_pickaxe"), false);
        let sword = (item("diamond_sword"), false);
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
        let pickaxe = (item("diamond_pickaxe"), false);
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
        let pickaxe = (item("diamond_pickaxe"), false);
        let sword = (item("diamond_sword"), false);
        let mut equip = HeldItemEquip::default();
        equip.advance(Some(&pickaxe));
        equip.advance_by(5.0, Some(&sword));
        assert_eq!(equip.visible(), Some(&sword));
        assert_eq!(equip.inverse_arm_height(), 0.0);
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
        let bobbed = hand_view_proj(HAND_TEST_ASPECT, BobFrame::default(), NO_DAMAGE_TILT);
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
        let m = hand_view_proj(HAND_TEST_ASPECT, dip, NO_DAMAGE_TILT);
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
        let m = hand_view_proj(HAND_TEST_ASPECT, sway, NO_DAMAGE_TILT);
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

    /// The hand's `bobHurt` is **live** in production now, and this gate is what
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
        let off = hand_view_proj(HAND_TEST_ASPECT, hurt, NO_DAMAGE_TILT);
        assert_eq!(
            off.to_cols_array(),
            hand_projection(HAND_TEST_ASPECT).to_cols_array(),
            "a zero damage-tilt strength must be completely inert"
        );

        // At vanilla's own accessibility default, live.
        let on = hand_view_proj(HAND_TEST_ASPECT, hurt, 1.0);
        assert_ne!(
            on.to_cols_array(),
            hand_projection(HAND_TEST_ASPECT).to_cols_array(),
            "at the default strength the hurt tilt must reach the matrix"
        );
    }
}
