//! Per-entity pose and animation render state.
//!
//! `impl-assets` settled that entity *meshes* are code-only (`CubeDef`/`PartPose`
//! → `bake_entity`) and that per-mob `EntityModelDef` **data** lives in the
//! version crate. The missing middle layer — the **pose** a renderer needs each
//! frame (walk cycle, head tracking, attack swing, idle age) — is behaviour, not
//! data, so it lives here, version-free. `impl-shell` / the renderer consumes
//! [`EntityPose`] every frame; the integrated server or the entity tracker feeds
//! it per server tick.
//!
//! # The seam shape
//!
//! Vanilla drives animation off two clocks, and so does the feeder contract a
//! renderer consumes:
//!   * a **per-server-tick** update ([`EntityPose::tick`]) — call once for every
//!     entity every time a movement/rotation update is applied (20 Hz), passing
//!     the entity's new `(x, z)`, body yaw and desired head yaw/pitch. It
//!     advances the walk phase, attack-swing timer and idle age and snapshots
//!     the previous tick's rotations, and
//!   * a **per-frame** read ([`EntityPose::render`]) — call once per rendered
//!     frame with a `partial_tick` in `0.0..=1.0`; it returns a [`RenderPose`]
//!     with every value already interpolated between the previous and current
//!     tick, so rendering is smooth at any framerate. Mesh **world position** is
//!     interpolated separately by
//!     [`Interpolated`](crate::interpolation::Interpolated) on the tracker — the
//!     same 20 Hz-to-render seam — and combined with `RenderPose::body_yaw` for
//!     the model transform.
//!
//! Everything is expressed in the exact units and constants vanilla uses (limb
//! swing target `min(distance*4, 1)`, smoothing factor `0.4`, baby leg-scale
//! `3.0`, head yaw clamp `75°`, head pitch clamp `40°`) so a renderer written
//! against this produces vanilla-identical limbs.

use crate::interpolation::wrap_degrees;

/// The walk-cycle smoothing factor vanilla feeds `WalkAnimationState.update`.
pub const LIMB_SWING_SMOOTHING: f32 = 0.4;
/// The leg-swing position scale applied to babies (their legs swing faster).
pub const BABY_LIMB_SCALE: f32 = 3.0;
/// The default position scale for adults.
pub const ADULT_LIMB_SCALE: f32 = 1.0;
/// Maximum head yaw offset from the body, in degrees (`Mob.getMaxHeadYRot`).
pub const MAX_HEAD_YAW: f32 = 75.0;
/// Maximum head pitch, in degrees (`Mob.getMaxHeadXRot`).
pub const MAX_HEAD_PITCH: f32 = 40.0;
/// Default arm-swing duration in ticks for an empty hand.
///
/// 26.2 moved this onto the held stack: vanilla's own swing-duration getter
/// reads the held stack's own swing-animation component, and
/// its own default swing animation is `(WHACK, 6)` — so `6` is the *component default*,
/// not a hard-coded constant, and an item shipping its own `swing_animation`
/// component swings for a different number of ticks. Nothing in this engine
/// decodes that component yet; see [`swing_duration`].
pub const DEFAULT_SWING_DURATION: i32 = 6;

/// Vanilla's `LivingEntity.getCurrentSwingDuration`, as a pure function of the
/// held item's base duration and the two mining effects.
///
/// ```text
/// haste (DIG_SPEED / Conduit Power) present -> base - (1 + amplifier)
/// mining fatigue present                    -> base + (1 + amplifier) * 2
/// neither                                   -> base
/// ```
///
/// Haste wins outright when both are present — this is `if/else if`, not two
/// independent adjustments. `amplifier` is vanilla's raw 0-based amplifier, so
/// `Some(0)` is Haste I / Mining Fatigue I.
///
/// The result is clamped to at least `1`: Haste is uncapped through
/// `/effect give`, and vanilla itself will happily compute a zero or negative
/// duration here, which divides by zero in `updateSwingTime`. Clamping is a
/// deliberate divergence from a division-by-zero, not a modelling choice.
///
/// # Nothing feeds the effect arguments today
///
/// Both are `None` at every call site in this workspace, because **no local
/// mob-effect state is reachable**: the v770 adapter decodes
/// `update_mob_effect` and `lodestone-shell` forwards it as
/// `NetUpdate::EffectApplied`, but nothing folds it into a per-entity effect set
/// that a swing (or a dig) can query. `lodestone_game::mining::BreakInputs` has
/// the identical hole for the identical reason — its `haste_amplifier` /
/// `mining_fatigue` fields are also always `None` (see
/// `tool_inputs_stay_at_bare_hand_defaults` in `lodestone-shell`'s `sim.rs`).
/// This function exists so that closing that hole is a change of *arguments* at
/// one call site rather than a rewrite of the swing clock.
#[must_use]
pub fn swing_duration(base: i32, haste_amplifier: Option<u32>, mining_fatigue: Option<u32>) -> i32 {
    let duration = if let Some(amp) = haste_amplifier {
        base - (1 + i32::try_from(amp).unwrap_or(i32::MAX - 1))
    } else if let Some(amp) = mining_fatigue {
        base.saturating_add((1 + i32::try_from(amp).unwrap_or(i32::MAX / 4)).saturating_mul(2))
    } else {
        base
    };
    duration.max(1)
}

/// The target limb-swing amplitude for a given horizontal distance moved this
/// tick: `min(distance * 4, 1)`, exactly as `updateWalkAnimation`.
#[must_use]
pub fn walk_target_speed(distance: f32) -> f32 {
    (distance * 4.0).min(1.0)
}

/// Vanilla's `WalkAnimationState`: the leg/arm swing phase and amplitude, with a
/// one-tick history so the renderer can interpolate.
///
/// `speed` is the swing **amplitude** (how far limbs swing), `position` is the
/// accumulated **phase** (where in the cycle they are). Both are advanced once
/// per tick; the renderer reads interpolated values with a partial tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct WalkAnimation {
    speed_old: f32,
    speed: f32,
    position: f32,
    position_scale: f32,
}

impl WalkAnimation {
    /// A fresh, stopped walk animation with an adult position scale.
    #[must_use]
    pub fn new() -> Self {
        Self {
            speed_old: 0.0,
            speed: 0.0,
            position: 0.0,
            position_scale: ADULT_LIMB_SCALE,
        }
    }

    /// Advances one tick toward `target_speed` (`walk_target_speed`) with the
    /// given `factor` (vanilla `0.4`) and `position_scale` (`3.0` for babies).
    pub fn update(&mut self, target_speed: f32, factor: f32, position_scale: f32) {
        self.speed_old = self.speed;
        self.speed += (target_speed - self.speed) * factor;
        self.position += self.speed;
        self.position_scale = position_scale;
    }

    /// Resets the animation (vanilla `stop`, used for dead/riding entities).
    pub fn stop(&mut self) {
        self.speed_old = 0.0;
        self.speed = 0.0;
        self.position = 0.0;
    }

    /// The current amplitude (no interpolation).
    #[must_use]
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// The interpolated amplitude for a partial tick, capped at 1 like vanilla.
    #[must_use]
    pub fn speed_lerp(&self, partial_tick: f32) -> f32 {
        (self.speed_old + (self.speed - self.speed_old) * partial_tick).min(1.0)
    }

    /// The current scaled phase (no interpolation).
    #[must_use]
    pub fn position(&self) -> f32 {
        self.position * self.position_scale
    }

    /// The interpolated, scaled phase for a partial tick, matching
    /// vanilla's own walk-animation-state position step.
    #[must_use]
    pub fn position_lerp(&self, partial_tick: f32) -> f32 {
        (self.position - self.speed * (1.0 - partial_tick)) * self.position_scale
    }

    /// Whether the entity is meaningfully moving (`speed > 1e-5`).
    #[must_use]
    pub fn is_moving(&self) -> bool {
        self.speed > 1.0e-5
    }
}

/// Clamps a head yaw to within `limit` degrees of the body yaw, mirroring
/// `Mob.clampHeadRotationToBody`. Returns the clamped absolute head yaw.
#[must_use]
pub fn clamp_head_to_body(body_yaw: f32, head_yaw: f32, limit: f32) -> f32 {
    let delta = wrap_degrees(body_yaw - head_yaw);
    let target = delta.clamp(-limit, limit);
    head_yaw + delta - target
}

/// Interpolates between two angles along the shortest arc, like vanilla's
/// `Mth.rotLerp`. `partial` is in `0.0..=1.0`; the result is `from` when
/// `partial == 0` and `to` when `partial == 1`, wrapping across ±180°.
#[must_use]
pub fn rot_lerp(partial: f32, from: f32, to: f32) -> f32 {
    from + partial * wrap_degrees(to - from)
}

/// The interpolated pose a renderer reads once per frame to place and animate a
/// mob mesh. Every field is already interpolated for the frame's `partial_tick`
/// and expressed in the units a baked model expects, so a renderer needs no
/// further clock or clamp logic. Produced by [`EntityPose::render`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderPose {
    /// Body facing in degrees (interpolated `yBodyRotO`→`yBodyRot`).
    pub body_yaw: f32,
    /// Head yaw **relative to the body**, already clamped to [`MAX_HEAD_YAW`],
    /// in degrees — the value a head part is posed by (`netHeadYaw`).
    pub head_yaw: f32,
    /// Head pitch in degrees (`headPitch`).
    pub head_pitch: f32,
    /// Walk-cycle phase (`limbSwing`): the accumulated leg/arm position.
    pub limb_swing: f32,
    /// Walk-cycle amplitude (`limbSwingAmount`) in `0.0..=1.0`.
    pub limb_swing_amount: f32,
    /// Attack-swing progress in `0.0..=1.0` (`attackTime`).
    pub attack_anim: f32,
    /// Continuous idle age in ticks (`ageInTicks = tickCount + partialTick`),
    /// driving head bob and other idle motion.
    pub age: f32,
}

/// The full per-entity render pose. Fed once per server tick, read (interpolated)
/// once per frame.
#[derive(Debug, Clone)]
pub struct EntityPose {
    /// Leg/arm walk cycle.
    pub walk: WalkAnimation,
    /// Body facing (degrees). Set from the entity's `yBodyRot`.
    pub body_yaw: f32,
    /// Previous-tick body facing, for per-frame interpolation.
    pub o_body_yaw: f32,
    /// Head facing (degrees, absolute), already clamped to the body.
    pub head_yaw: f32,
    /// Previous-tick absolute head facing, for per-frame interpolation.
    pub o_head_yaw: f32,
    /// Head pitch (degrees), clamped to [`MAX_HEAD_PITCH`].
    pub head_pitch: f32,
    /// Previous-tick head pitch, for per-frame interpolation.
    pub o_head_pitch: f32,
    /// Current attack-swing progress in `0.0..1.0`.
    pub attack_anim: f32,
    /// Previous-tick attack-swing progress, for interpolation.
    pub o_attack_anim: f32,
    /// Idle age in ticks (drives head bob and other idle motion).
    pub age: u32,
    swing_time: i32,
    swinging: bool,
    swing_duration: i32,
    is_baby: bool,
    prev_x: f64,
    prev_z: f64,
}

impl EntityPose {
    /// A fresh pose for an entity at `(x, z)`, facing `yaw`.
    #[must_use]
    pub fn new(x: f64, z: f64, yaw: f32, is_baby: bool) -> Self {
        Self {
            walk: WalkAnimation::new(),
            body_yaw: yaw,
            o_body_yaw: yaw,
            head_yaw: yaw,
            o_head_yaw: yaw,
            head_pitch: 0.0,
            o_head_pitch: 0.0,
            attack_anim: 0.0,
            o_attack_anim: 0.0,
            age: 0,
            swing_time: 0,
            swinging: false,
            swing_duration: DEFAULT_SWING_DURATION,
            is_baby,
            prev_x: x,
            prev_z: z,
        }
    }

    /// Begins an arm swing if one is not already past its half-way point, like
    /// `LivingEntity.swing`. `duration` is the swing length in ticks — build it
    /// with [`swing_duration`].
    ///
    /// The "not already past its half-way point" test is what makes a held
    /// mine look like continuous swinging rather than a stutter: vanilla's
    /// `continueAttack` calls `swing` **every tick**, and all but every third
    /// call is swallowed here.
    ///
    /// # One deliberate divergence: the duration is latched
    ///
    /// Vanilla re-reads `getCurrentSwingDuration()` inside `updateSwingTime`
    /// every tick, so gaining Haste mid-swing changes the denominator under a
    /// running animation. This latches the duration at the start of the swing
    /// instead. The difference is unobservable today — see [`swing_duration`]
    /// for why both effect inputs are always `None` — and re-reading it would
    /// mean handing this type an effect source it has no other use for.
    pub fn start_swing(&mut self, duration: i32) {
        if !self.swinging || self.swing_time >= duration / 2 || self.swing_time < 0 {
            self.swing_time = -1;
            self.swinging = true;
            self.swing_duration = duration.max(1);
        }
    }

    /// Advances the pose one server tick from the entity's new motion and
    /// orientation. `head_yaw`/`head_pitch` are the *desired* absolute head
    /// angles; they are clamped to the body here.
    pub fn tick(&mut self, x: f64, z: f64, body_yaw: f32, head_yaw: f32, head_pitch: f32) {
        self.age = self.age.saturating_add(1);

        // Attack swing (LivingEntity.updateSwingTime).
        self.o_attack_anim = self.attack_anim;
        if self.swinging {
            self.swing_time += 1;
            if self.swing_time >= self.swing_duration {
                self.swing_time = 0;
                self.swinging = false;
            }
        } else {
            self.swing_time = 0;
        }
        self.attack_anim = self.swing_time.max(0) as f32 / self.swing_duration as f32;

        // Walk cycle (calculateEntityAnimation, useY = false for ground mobs).
        let dx = x - self.prev_x;
        let dz = z - self.prev_z;
        let distance = ((dx * dx + dz * dz) as f32).sqrt();
        let scale = if self.is_baby {
            BABY_LIMB_SCALE
        } else {
            ADULT_LIMB_SCALE
        };
        self.walk
            .update(walk_target_speed(distance), LIMB_SWING_SMOOTHING, scale);
        self.prev_x = x;
        self.prev_z = z;

        // Orientation, head clamped to the body.
        self.o_body_yaw = self.body_yaw;
        self.o_head_yaw = self.head_yaw;
        self.o_head_pitch = self.head_pitch;
        self.body_yaw = body_yaw;
        self.head_yaw = clamp_head_to_body(body_yaw, head_yaw, MAX_HEAD_YAW);
        self.head_pitch = head_pitch.clamp(-MAX_HEAD_PITCH, MAX_HEAD_PITCH);
    }

    /// Interpolated body yaw for a partial tick, along the shortest arc.
    #[must_use]
    pub fn body_yaw_lerp(&self, partial_tick: f32) -> f32 {
        rot_lerp(partial_tick, self.o_body_yaw, self.body_yaw)
    }

    /// Interpolated absolute head yaw for a partial tick.
    #[must_use]
    pub fn head_yaw_lerp(&self, partial_tick: f32) -> f32 {
        rot_lerp(partial_tick, self.o_head_yaw, self.head_yaw)
    }

    /// Interpolated head pitch for a partial tick.
    #[must_use]
    pub fn head_pitch_lerp(&self, partial_tick: f32) -> f32 {
        rot_lerp(partial_tick, self.o_head_pitch, self.head_pitch)
    }

    /// Interpolated head yaw **relative to the interpolated body**, in
    /// `[-75, 75]` degrees — the value a head part is posed by each frame.
    #[must_use]
    pub fn relative_head_yaw_lerp(&self, partial_tick: f32) -> f32 {
        wrap_degrees(self.head_yaw_lerp(partial_tick) - self.body_yaw_lerp(partial_tick))
    }

    /// The single per-frame bundle a renderer reads to pose this mob's mesh:
    /// every value interpolated for `partial_tick` and ready to feed a baked
    /// model. This is the whole feeder contract — call [`EntityPose::tick`] once
    /// per server tick, then [`render`](Self::render) once per frame.
    #[must_use]
    pub fn render(&self, partial_tick: f32) -> RenderPose {
        RenderPose {
            body_yaw: self.body_yaw_lerp(partial_tick),
            head_yaw: self.relative_head_yaw_lerp(partial_tick),
            head_pitch: self.head_pitch_lerp(partial_tick),
            limb_swing: self.walk.position_lerp(partial_tick),
            limb_swing_amount: self.walk.speed_lerp(partial_tick),
            attack_anim: self.attack_anim_lerp(partial_tick),
            age: self.age as f32 + partial_tick,
        }
    }

    /// The interpolated attack-swing progress for a partial tick — vanilla's
    /// `LivingEntity.getAttackAnim`.
    ///
    /// # This is not a plain lerp, and the difference is visible
    ///
    /// Vanilla wraps a *negative* delta forward by one whole swing:
    ///
    /// ```text
    /// diff = attackAnim - oAttackAnim;  if (diff < 0) diff++;
    /// return oAttackAnim + diff * partialTick;
    /// ```
    ///
    /// `attack_anim` is a sawtooth — it climbs to `(duration-1)/duration` and
    /// then drops to `0` in one tick, either because the swing ended or because
    /// [`start_swing`](Self::start_swing) restarted it past its half-way point.
    /// A plain lerp across that drop runs the arm **backwards** through the whole
    /// arc inside one 50 ms tick; the wrap instead carries it forward to `1.0`,
    /// which is where `sin(sqrt(t) · π)` returns to zero, so the arm arrives at
    /// rest instead of rewinding.
    ///
    /// This is load-bearing for hold-to-mine specifically, which re-swings every
    /// few ticks and therefore hits the drop repeatedly rather than once.
    #[must_use]
    pub fn attack_anim_lerp(&self, partial_tick: f32) -> f32 {
        let mut diff = self.attack_anim - self.o_attack_anim;
        if diff < 0.0 {
            diff += 1.0;
        }
        self.o_attack_anim + diff * partial_tick
    }

    /// Whether an arm swing is currently in progress.
    #[must_use]
    pub fn is_swinging(&self) -> bool {
        self.swinging
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rot_lerp_takes_shortest_arc() {
        // Straight interpolation within range.
        assert!((rot_lerp(0.5, 0.0, 90.0) - 45.0).abs() < 1e-4);
        // Across the ±180 seam: 170 -> -170 is +20, so half is 180 (== -180).
        let mid = rot_lerp(0.5, 170.0, -170.0);
        assert!((wrap_degrees(mid - 180.0)).abs() < 1e-3, "got {mid}");
        // Endpoints are exact.
        assert!((rot_lerp(0.0, 30.0, 200.0) - 30.0).abs() < 1e-4);
        assert!((wrap_degrees(rot_lerp(1.0, 30.0, 200.0) - 200.0)).abs() < 1e-3);
    }

    #[test]
    fn render_pose_interpolates_rotations_between_ticks() {
        let mut pose = EntityPose::new(0.0, 0.0, 0.0, false);
        // First tick establishes a previous rotation.
        pose.tick(0.0, 0.0, 0.0, 0.0, 0.0);
        // Second tick rotates the body to 40 and pitches down to 20.
        pose.tick(0.0, 0.0, 40.0, 40.0, 20.0);
        // Mid-frame body yaw is halfway between the two ticks (0 -> 40).
        let r = pose.render(0.5);
        assert!((r.body_yaw - 20.0).abs() < 1e-3, "body {}", r.body_yaw);
        assert!((r.head_pitch - 10.0).abs() < 1e-3, "pitch {}", r.head_pitch);
        // Head is within the body, so its relative yaw is ~0.
        assert!(r.head_yaw.abs() < 1e-3, "head {}", r.head_yaw);
        // A full partial matches the current tick exactly.
        let end = pose.render(1.0);
        assert!((end.body_yaw - 40.0).abs() < 1e-3);
        // age is continuous: two ticks in, +0.5 partial.
        assert!((r.age - 2.5).abs() < 1e-6, "age {}", r.age);
    }

    #[test]
    fn render_head_yaw_is_clamped_and_relative() {
        let mut pose = EntityPose::new(0.0, 0.0, 0.0, false);
        pose.tick(0.0, 0.0, 0.0, 0.0, 0.0);
        // Body stays at 0, head wants +120 -> clamps to +75 relative.
        pose.tick(0.0, 0.0, 0.0, 120.0, 0.0);
        let r = pose.render(1.0);
        assert!((r.head_yaw - 75.0).abs() < 1e-3, "head {}", r.head_yaw);
    }

    #[test]
    fn walk_target_is_capped() {
        assert!((walk_target_speed(0.1) - 0.4).abs() < 1e-6);
        assert!((walk_target_speed(0.5) - 1.0).abs() < 1e-6); // 0.5*4 = 2 -> capped
        assert_eq!(walk_target_speed(0.0), 0.0);
    }

    #[test]
    fn walk_animation_smooths_toward_target() {
        let mut w = WalkAnimation::new();
        // First tick toward target 1.0 with factor 0.4: speed = 0 + 0.4 = 0.4.
        w.update(1.0, 0.4, 1.0);
        assert!((w.speed() - 0.4).abs() < 1e-6);
        assert!((w.position() - 0.4).abs() < 1e-6);
        // Second tick: speed = 0.4 + 0.6*0.4 = 0.64; position = 0.4 + 0.64.
        w.update(1.0, 0.4, 1.0);
        assert!((w.speed() - 0.64).abs() < 1e-6);
        assert!((w.position() - 1.04).abs() < 1e-6);
    }

    #[test]
    fn walk_position_interpolates() {
        let mut w = WalkAnimation::new();
        w.update(1.0, 0.4, 1.0); // speed 0.4, position 0.4
        // Half a tick back from position 0.4 at speed 0.4 = 0.4 - 0.4*0.5 = 0.2.
        assert!((w.position_lerp(0.5) - 0.2).abs() < 1e-6);
        // Full partial returns the current position.
        assert!((w.position_lerp(1.0) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn baby_scale_triples_position() {
        let mut w = WalkAnimation::new();
        w.update(1.0, 0.4, BABY_LIMB_SCALE);
        assert!((w.position() - 0.4 * 3.0).abs() < 1e-6);
    }

    #[test]
    fn head_clamps_to_body() {
        // Head wants +120 from a body at 0: clamps to +75.
        assert!((clamp_head_to_body(0.0, 120.0, 75.0) - 75.0).abs() < 1e-4);
        // Within range: unchanged.
        assert!((clamp_head_to_body(0.0, 30.0, 75.0) - 30.0).abs() < 1e-4);
        // Wrap-around: body 170, head -170 (really +20 apart) stays put.
        let clamped = clamp_head_to_body(170.0, -170.0, 75.0);
        assert!((wrap_degrees(clamped - (-170.0))).abs() < 1e-3);
    }

    #[test]
    fn pose_tick_advances_walk_and_head() {
        let mut pose = EntityPose::new(0.0, 0.0, 0.0, false);
        // Move 0.25 blocks in x: distance 0.25 -> target min(1.0,1.0)=1.0.
        pose.tick(0.25, 0.0, 10.0, 90.0, 50.0);
        assert!(pose.walk.is_moving());
        assert_eq!(pose.body_yaw, 10.0);
        // Head wanted 90 but body is 10, clamp 75 -> 85.
        assert!((pose.head_yaw - 85.0).abs() < 1e-3);
        // Pitch clamped to 40.
        assert!((pose.head_pitch - 40.0).abs() < 1e-6);
        assert_eq!(pose.age, 1);
    }

    #[test]
    fn swing_runs_for_its_duration() {
        let mut pose = EntityPose::new(0.0, 0.0, 0.0, false);
        pose.start_swing(6);
        assert!(pose.is_swinging());
        let mut progressed = false;
        for _ in 0..=6 {
            pose.tick(0.0, 0.0, 0.0, 0.0, 0.0);
            if pose.attack_anim > 0.0 {
                progressed = true;
            }
        }
        assert!(progressed, "attack_anim should rise during the swing");
        // The swing starts at -1, so it ends after duration+1 ticks and resets.
        assert!(!pose.is_swinging());
        assert_eq!(pose.attack_anim, 0.0);
    }

    #[test]
    fn attack_anim_interpolates() {
        let mut pose = EntityPose::new(0.0, 0.0, 0.0, false);
        pose.start_swing(6);
        pose.tick(0.0, 0.0, 0.0, 0.0, 0.0); // swing_time 0 -> anim 0
        pose.tick(0.0, 0.0, 0.0, 0.0, 0.0); // swing_time 1 -> anim 1/6
        let mid = pose.attack_anim_lerp(0.5);
        assert!(mid > pose.o_attack_anim - 1e-6 && mid <= pose.attack_anim + 1e-6);
    }

    #[test]
    fn attack_anim_wraps_forward_across_the_sawtooth_drop() {
        // Hand-built rather than driven, so the drop is placed exactly: the last
        // tick of a 6-tick swing reports 5/6 and the next reports 0.
        let mut pose = EntityPose::new(0.0, 0.0, 0.0, false);
        pose.o_attack_anim = 5.0 / 6.0;
        pose.attack_anim = 0.0;

        // A plain lerp would run *backwards* through the whole arc here; vanilla
        // carries it forward, so every sample is >= where it started and the
        // final one lands on 1.0 (== 0.0 in the sawtooth, but 1.0 in the shaping
        // function, which is where the arm is back at rest).
        assert!(
            (pose.attack_anim_lerp(1.0) - 1.0).abs() < 1e-6,
            "end of the wrap should be 1.0, got {}",
            pose.attack_anim_lerp(1.0)
        );
        let mut previous = pose.o_attack_anim - 1e-6;
        for step in 0..=10 {
            let value = pose.attack_anim_lerp(step as f32 / 10.0);
            assert!(
                value >= previous,
                "the wrapped arc must be monotonically forward, but {value} < {previous}"
            );
            previous = value;
        }
    }

    /// The counterpart control: an *ordinary* rising tick must not be wrapped.
    /// Without this, "the wrap fires" is also satisfied by a function that adds
    /// 1.0 unconditionally.
    #[test]
    fn attack_anim_does_not_wrap_a_rising_delta() {
        let mut pose = EntityPose::new(0.0, 0.0, 0.0, false);
        pose.o_attack_anim = 1.0 / 6.0;
        pose.attack_anim = 2.0 / 6.0;
        assert!((pose.attack_anim_lerp(0.5) - 0.25).abs() < 1e-6);
        assert!((pose.attack_anim_lerp(1.0) - 2.0 / 6.0).abs() < 1e-6);
    }

    /// The swing clock is a **tick** state machine, and reading it per frame must
    /// not advance it. This is the same defect class `entities.rs`'s
    /// `limb_swing_tracks_per_tick_travel_not_the_interpolation_gap` documents
    /// for the walk cycle, where driving the phase per frame made it run up to 3x
    /// too fast and made the speed frame-rate dependent.
    #[test]
    fn swing_progress_advances_per_tick_not_per_render_read() {
        let mut pose = EntityPose::new(0.0, 0.0, 0.0, false);
        pose.start_swing(6);
        pose.tick(0.0, 0.0, 0.0, 0.0, 0.0);
        pose.tick(0.0, 0.0, 0.0, 0.0, 0.0);
        let before = (pose.o_attack_anim, pose.attack_anim);

        // 300 render reads — 5 seconds of frames inside one 50 ms tick.
        let mut samples = Vec::new();
        for i in 0..300 {
            samples.push(pose.render(i as f32 / 300.0).attack_anim);
        }
        assert_eq!(
            before,
            (pose.o_attack_anim, pose.attack_anim),
            "render() must be a pure read; it moved the clock"
        );
        // Every sample stays inside the one tick's window, so no number of frames
        // can carry the animation past where the tick put it.
        let ceiling = pose.attack_anim + 1e-6;
        assert!(
            samples.iter().all(|&s| s <= ceiling),
            "a render read escaped the current tick's window (ceiling {ceiling}): {:?}",
            samples.iter().copied().fold(f32::MIN, f32::max)
        );
    }

    /// Vanilla's `getCurrentSwingDuration`, including the fact that Haste wins
    /// outright when both effects are present (`if`/`else if`, not additive).
    #[test]
    fn swing_duration_models_haste_and_mining_fatigue() {
        assert_eq!(swing_duration(6, None, None), 6);
        // Haste I: 6 - (1 + 0) = 5. Haste II: 6 - (1 + 1) = 4.
        assert_eq!(swing_duration(6, Some(0), None), 5);
        assert_eq!(swing_duration(6, Some(1), None), 4);
        // Mining Fatigue I: 6 + (1 + 0) * 2 = 8. II: 6 + (1 + 1) * 2 = 10.
        assert_eq!(swing_duration(6, None, Some(0)), 8);
        assert_eq!(swing_duration(6, None, Some(1)), 10);
        // Both: haste branch only.
        assert_eq!(swing_duration(6, Some(0), Some(3)), 5);
        // Uncapped Haste cannot produce a zero denominator.
        assert_eq!(swing_duration(6, Some(50), None), 1);
    }

    #[test]
    fn a_second_swing_is_swallowed_until_half_way() {
        let mut pose = EntityPose::new(0.0, 0.0, 0.0, false);
        pose.start_swing(6);
        pose.tick(0.0, 0.0, 0.0, 0.0, 0.0); // swing_time 0
        pose.tick(0.0, 0.0, 0.0, 0.0, 0.0); // swing_time 1
        // Below duration/2 == 3, so this call must not restart the swing.
        pose.start_swing(6);
        pose.tick(0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            pose.attack_anim > 1.0 / 6.0,
            "the swing was restarted; attack_anim fell back to {}",
            pose.attack_anim
        );
        // Now at/past half way, a re-swing does restart it.
        pose.tick(0.0, 0.0, 0.0, 0.0, 0.0); // swing_time 3
        pose.start_swing(6);
        pose.tick(0.0, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(pose.attack_anim, 0.0, "a re-swing past half way restarts");
        assert!(pose.is_swinging(), "and it is still swinging");
    }

    #[test]
    fn stationary_entity_stops_swinging_limbs() {
        let mut pose = EntityPose::new(5.0, 5.0, 0.0, false);
        for _ in 0..10 {
            pose.tick(5.0, 5.0, 0.0, 0.0, 0.0);
        }
        assert!(!pose.walk.is_moving());
    }
}
