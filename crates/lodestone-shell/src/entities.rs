//! Client-side entity interpolation: turning the 20 Hz stream of
//! [`EntityView`](lodestone_client::EntityView) snapshots into smooth per-frame
//! render transforms.
//!
//! The server reports entity positions at tick rate (20 Hz); the shell draws at
//! 50–120 fps. Snapping each mob to its latest reported position would stutter
//! visibly, so — exactly as vanilla does — we render *behind* the latest
//! snapshot and ease from the previous position to the current one over a fixed
//! window. When a fresh position arrives we start the new interpolation from
//! where the mob is *currently being drawn*, not from the stale target, so
//! motion is C0-continuous and never jumps.
//!
//! # Why the window is three ticks, not one
//!
//! Vanilla eases entity movement over **three** ticks, not one. Its
//! `InterpolationHandler` (26.2 client) sets `DEFAULT_INTERPOLATION_STEPS = 3`
//! and `interpolateTo` resets the step counter to 3 on every position packet,
//! then `interpolate()` consumes `1/steps` of the remaining gap each of the next
//! three client ticks. The consequence is load-bearing: the server only sends a
//! movement packet when a mob's position *changes*, so packets routinely arrive
//! less often than once per tick. If the ease completes in a single tick (50 ms)
//! the mob reaches its target and then **sits frozen** until the next packet —
//! move, freeze, move, freeze — which reads as "not interpolated" even though
//! interpolation is running. A three-tick (150 ms) window keeps the mob gliding
//! across the gap between sparse packets, matching vanilla's feel. A continuous
//! linear ease over three ticks is the faithful continuous form of vanilla's
//! discrete `alpha = 1/steps` schedule (its per-tick positions land on 1/3, 2/3,
//! 1 — linearly spaced).
//!
//! # Why the walk cycle is measured off the *drawn* position
//!
//! Vanilla's `updateWalkAnimation` feeds `min(distance * 4, 1)` where `distance`
//! is how far the entity moved **this tick**. The tempting local quantity is the
//! gap a fresh snapshot opens up — "the mob was here, the server says it is now
//! there" — and it is wrong by exactly [`INTERP_STEPS`].
//!
//! Steady state, with a mob walking `v` blocks per tick and a packet every tick:
//! each tick the drawn position closes `1/3` of the outstanding gap `g`, while
//! the target runs on by `v`, so `g' = (2/3)g + v` and `g` settles at `3v`. Feed
//! `3v` to `walk_target_speed` and the amplitude saturates at three times the
//! speed it should, and since `WalkAnimation::position` accumulates `speed` per
//! tick, the *phase* advances up to 3× too fast as well — legs that swing both
//! too far and far too quickly, which is precisely how it was reported.
//!
//! Sampling `render_pos` once per 20 Hz tick measures `v` instead, because that
//! is what vanilla is measuring: on the client the entity's own position has
//! already been advanced by `InterpolationHandler`, so `getX() - xo` is the
//! *interpolated* step, not the packet delta. The two agree under dense packets
//! and under sparse ones, which the gap measure never does.
//!
//! This module is deliberately GPU-free and depends only on `glam`, so the
//! interpolation is unit-testable without a device or a server: the sim converts
//! each [`EntityView`] into an [`EntitySnapshot`] (version-free, glam-only) and
//! feeds those in. The output is a flat list of [`EntityDraw`]s — type path,
//! feet position, body yaw and scale — that the renderer resolves into instanced
//! draws.

use std::collections::HashMap;

use glam::Vec3;
use lodestone_entity::pose::{
    ADULT_LIMB_SCALE, BABY_LIMB_SCALE, LIMB_SWING_SMOOTHING, MAX_HEAD_YAW, WalkAnimation,
    clamp_head_to_body, walk_target_speed,
};
use lodestone_render::AnimInput;

/// One physics tick, in seconds.
const TICK: f32 = 0.05;

/// Vanilla's `InterpolationHandler::DEFAULT_INTERPOLATION_STEPS`: entity moves
/// ease over three ticks, not one. See the module docs for why a one-tick window
/// reads as "not interpolated" against the server's sparse move packets.
const INTERP_STEPS: f32 = 3.0;

/// The interpolation window in seconds: `TICK * INTERP_STEPS` (150 ms). A fresh
/// snapshot is reached this long after it arrives, re-anchored from the current
/// render pose so motion stays continuous.
const INTERP_WINDOW: f32 = TICK * INTERP_STEPS;

/// Position change (blocks) below which a snapshot is treated as "no movement",
/// so idle mobs don't restart their interpolation clock every frame.
/// Seconds per server tick, the cadence the walk animation is advanced at.
const TICK_SECONDS: f32 = TICK;
/// Server ticks per second, for the continuous `ageInTicks` clock.
const TICKS_PER_SECOND: f32 = 20.0;

const POS_EPS: f32 = 1.0e-4;

/// Yaw change (degrees) below which a snapshot is treated as "no turn". Applies
/// to body yaw, head yaw and pitch alike.
const YAW_EPS: f32 = 1.0e-2;

/// A version-free entity snapshot as reported by the client for one tick. Built
/// by the sim from an [`EntityView`](lodestone_client::EntityView); carries only
/// what the renderer needs, in glam types, so this module needs no client or
/// model dependency.
#[derive(Debug, Clone, PartialEq)]
pub struct EntitySnapshot {
    /// The server-assigned entity id (interpolation key).
    pub id: i32,
    /// The entity type's canonical path (e.g. `"pig"`), for model resolution.
    pub type_path: String,
    /// Feet position in world space.
    pub feet: Vec3,
    /// Body yaw in degrees.
    pub yaw: f32,
    /// Head yaw in degrees (absolute). Tracked separately from the body: a
    /// walking mob keeps its body facing its movement while its head turns to
    /// track a target, so this is never derived from `yaw`.
    pub head_yaw: f32,
    /// Head pitch in degrees (look up/down).
    pub pitch: f32,
    /// Uniform render scale (baby mobs are drawn smaller).
    pub scale: f32,
}

/// A single entity ready to draw this frame: its model type and interpolated
/// transform inputs. The renderer turns this into an
/// [`EntityInstance`](lodestone_render::EntityInstance).
#[derive(Debug, Clone, PartialEq)]
pub struct EntityDraw {
    /// The entity type's canonical path (e.g. `"pig"`).
    pub type_path: String,
    /// Interpolated feet position in world space.
    pub feet: Vec3,
    /// Interpolated body yaw in degrees.
    pub yaw: f32,
    /// Interpolated head yaw in degrees (absolute), for head tracking.
    pub head_yaw: f32,
    /// Interpolated head pitch in degrees.
    pub pitch: f32,
    /// Uniform render scale.
    pub scale: f32,
    /// Per-part animation drive (head tracking, walk cycle, idle age), already
    /// interpolated for this frame and in the units
    /// [`Skeleton::pose`](lodestone_render::Skeleton::pose) expects — note
    /// `head_yaw_deg` is **relative to the body**, matching vanilla's
    /// `netHeadYaw`.
    pub anim: AnimInput,
}

/// Per-entity interpolation track: the position/yaw we are easing *from*, the
/// latest reported target we are easing *to*, and how far through the tick we
/// are.
#[derive(Debug, Clone)]
struct Track {
    type_path: String,
    scale: f32,
    prev: Vec3,
    curr: Vec3,
    prev_yaw: f32,
    curr_yaw: f32,
    prev_head_yaw: f32,
    curr_head_yaw: f32,
    prev_pitch: f32,
    curr_pitch: f32,
    /// Seconds since the ease was last re-anchored, capped at [`INTERP_WINDOW`].
    t: f32,
    /// Vanilla's `WalkAnimationState`, ticked at 20 Hz.
    walk: WalkAnimation,
    /// The drawn position at the previous 20 Hz walk tick. The distance between
    /// this and the current drawn position *is* the per-tick travel
    /// [`walk_target_speed`] wants — see the module note on why the eased gap is
    /// not.
    walk_pos: Vec3,
    /// Continuous age in ticks (`ageInTicks`), driving idle bob.
    age: f32,
}

impl Track {
    /// The fraction `[0, 1]` through the current interpolation window.
    fn alpha(&self) -> f32 {
        (self.t / INTERP_WINDOW).clamp(0.0, 1.0)
    }

    /// The currently-drawn position: `prev` eased toward `curr` by `alpha`.
    fn render_pos(&self) -> Vec3 {
        self.prev.lerp(self.curr, self.alpha())
    }

    /// The currently-drawn body yaw, taking the shortest arc so a wrap across
    /// 360° (e.g. 350°→10°) turns +20° rather than −340°.
    fn render_yaw(&self) -> f32 {
        lerp_angle(self.prev_yaw, self.curr_yaw, self.alpha())
    }

    /// The currently-drawn head yaw, shortest-arc like the body yaw.
    fn render_head_yaw(&self) -> f32 {
        lerp_angle(self.prev_head_yaw, self.curr_head_yaw, self.alpha())
    }

    /// The currently-drawn head pitch. Pitch is bounded to ±90° and never wraps,
    /// so a plain linear ease is correct.
    fn render_pitch(&self) -> f32 {
        self.prev_pitch + (self.curr_pitch - self.prev_pitch) * self.alpha()
    }

    /// The animation drive for this frame.
    ///
    /// `partial_tick` is the fraction through the current 50 ms tick, used for
    /// the walk cycle exactly as vanilla's `WalkAnimationState` interpolation.
    /// The head yaw is clamped to the body (`Mob.clampHeadRotationToBody`) and
    /// then expressed *relative* to it, because that is what
    /// `LivingEntityRenderer` feeds `setupAnim` — passing the absolute value
    /// would spin every mob's head with its body.
    fn render_anim(&self, partial_tick: f32) -> AnimInput {
        let body = self.render_yaw();
        let head = clamp_head_to_body(body, self.render_head_yaw(), MAX_HEAD_YAW);
        AnimInput {
            head_yaw_deg: wrap_degrees(head - body),
            head_pitch_deg: self.render_pitch(),
            limb_swing: self.walk.position_lerp(partial_tick),
            limb_swing_amount: self.walk.speed_lerp(partial_tick),
            attack_anim: 0.0,
            age_ticks: self.age,
            // `Mob.isAggressive` rides a shared-flags bit nothing decodes yet.
            aggressive: false,
        }
    }
}

/// Wraps degrees into `(-180, 180]`, like `Mth.wrapDegrees`.
fn wrap_degrees(deg: f32) -> f32 {
    angle_diff(deg, 0.0)
}

/// Tracks and interpolates every visible entity between server ticks.
#[derive(Debug, Default)]
pub struct EntityInterpolator {
    tracks: HashMap<i32, Track>,
    /// Seconds accumulated toward the next 20 Hz animation tick.
    tick_accum: f32,
}

impl EntityInterpolator {
    /// A fresh interpolator with no tracked entities.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance every track by `dt` seconds, then fold in this frame's snapshots.
    ///
    /// Entities absent from `snapshots` are dropped (despawned/out of range).
    /// A snapshot whose position or yaw differs from the current target starts a
    /// new interpolation *from the current render pose*, so the mob never jumps.
    /// A snapshot that matches the current target only lets the existing ease
    /// run to completion.
    pub fn update(&mut self, snapshots: &[EntitySnapshot], dt: f32) {
        // Advance existing clocks first, so a snapshot that resets `t` to 0 this
        // frame starts the new window from exactly its previous render pose.
        for track in self.tracks.values_mut() {
            track.t = (track.t + dt).min(INTERP_WINDOW);
            track.age += dt * TICKS_PER_SECOND;
        }

        // Advance the walk cycle on a fixed 20 Hz clock rather than per frame:
        // vanilla's `WalkAnimationState` is a tick-rate state machine, and
        // driving it per frame would make the swing speed depend on frame rate.
        self.tick_accum += dt;
        while self.tick_accum >= TICK_SECONDS {
            self.tick_accum -= TICK_SECONDS;
            for track in self.tracks.values_mut() {
                // Vanilla's `updateWalkAnimation` measures `this.getX() - this.xo`
                // — how far the entity moved *this tick*. On the client that
                // entity has already been advanced by `InterpolationHandler`, so
                // the quantity is the per-tick step of the **interpolated**
                // position, which is what `render_pos` is here. Sampling it once
                // per 20 Hz tick reproduces vanilla under both dense and sparse
                // move packets, and needs no separate "has it stopped?" rule: a
                // mob that stops stops moving `render_pos`, so the distance goes
                // to zero and the amplitude decays on its own.
                let now = track.render_pos();
                let distance = (now - track.walk_pos).with_y(0.0).length();
                track.walk_pos = now;
                let limb_scale = if track.scale < 1.0 {
                    BABY_LIMB_SCALE
                } else {
                    ADULT_LIMB_SCALE
                };
                track
                    .walk
                    .update(walk_target_speed(distance), LIMB_SWING_SMOOTHING, limb_scale);
            }
        }

        for snap in snapshots {
            match self.tracks.get_mut(&snap.id) {
                None => {
                    // A newly seen entity is drawn at rest at its reported pose.
                    self.tracks.insert(
                        snap.id,
                        Track {
                            type_path: snap.type_path.clone(),
                            scale: snap.scale,
                            prev: snap.feet,
                            curr: snap.feet,
                            prev_yaw: snap.yaw,
                            curr_yaw: snap.yaw,
                            prev_head_yaw: snap.head_yaw,
                            curr_head_yaw: snap.head_yaw,
                            prev_pitch: snap.pitch,
                            curr_pitch: snap.pitch,
                            t: INTERP_WINDOW,
                            walk: WalkAnimation::new(),
                            walk_pos: snap.feet,
                            age: 0.0,
                        },
                    );
                }
                Some(track) => {
                    track.type_path.clone_from(&snap.type_path);
                    track.scale = snap.scale;
                    let moved = (snap.feet - track.curr).length() > POS_EPS;
                    let turned = angle_diff(snap.yaw, track.curr_yaw).abs() > YAW_EPS;
                    let head_turned =
                        angle_diff(snap.head_yaw, track.curr_head_yaw).abs() > YAW_EPS;
                    let pitched = (snap.pitch - track.curr_pitch).abs() > YAW_EPS;
                    if moved || turned || head_turned || pitched {
                        // Re-anchor the ease at where the mob is drawn right now.
                        track.prev = track.render_pos();
                        track.prev_yaw = track.render_yaw();
                        track.prev_head_yaw = track.render_head_yaw();
                        track.prev_pitch = track.render_pitch();
                        track.curr = snap.feet;
                        track.curr_yaw = snap.yaw;
                        track.curr_head_yaw = snap.head_yaw;
                        track.curr_pitch = snap.pitch;
                        track.t = 0.0;
                    }
                }
            }
        }

        // Drop tracks for entities no longer reported.
        let seen: std::collections::HashSet<i32> = snapshots.iter().map(|s| s.id).collect();
        self.tracks.retain(|id, _| seen.contains(id));
    }

    /// The interpolated draw list for this frame. Order is unspecified (grouped
    /// by model downstream), so no ordering guarantees are made here.
    #[must_use]
    pub fn draws(&self) -> Vec<EntityDraw> {
        self.tracks
            .values()
            .map(|t| EntityDraw {
                type_path: t.type_path.clone(),
                feet: t.render_pos(),
                yaw: t.render_yaw(),
                head_yaw: t.render_head_yaw(),
                pitch: t.render_pitch(),
                scale: t.scale,
                anim: t.render_anim((self.tick_accum / TICK_SECONDS).clamp(0.0, 1.0)),
            })
            .collect()
    }

    /// Number of entities currently tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Whether no entities are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}

/// The signed shortest difference `a − b` mapped into `(−180, 180]` degrees.
fn angle_diff(a: f32, b: f32) -> f32 {
    let mut d = (a - b) % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d <= -180.0 {
        d += 360.0;
    }
    d
}

/// Interpolate between two angles along the shortest arc.
fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    a + angle_diff(b, a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: i32, feet: Vec3, yaw: f32) -> EntitySnapshot {
        EntitySnapshot {
            id,
            type_path: "pig".into(),
            feet,
            yaw,
            head_yaw: yaw,
            pitch: 0.0,
            scale: 1.0,
        }
    }

    #[test]
    fn a_new_entity_is_drawn_at_its_reported_pose() {
        let mut interp = EntityInterpolator::new();
        interp.update(&[snap(1, Vec3::new(3.0, 64.0, -2.0), 90.0)], 0.016);
        let draws = interp.draws();
        assert_eq!(draws.len(), 1);
        assert_eq!(draws[0].feet, Vec3::new(3.0, 64.0, -2.0));
        assert_eq!(draws[0].yaw, 90.0);
    }

    #[test]
    fn movement_interpolates_rather_than_snapping() {
        let mut interp = EntityInterpolator::new();
        // Establish the entity at the origin, its ease already complete.
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], INTERP_WINDOW);
        // A new position arrives 4 blocks along +X.
        let target = Vec3::new(4.0, 0.0, 0.0);
        interp.update(&[snap(1, target, 0.0)], 0.0);

        // At t≈0 the mob must still be at (near) the old pose — NOT snapped to
        // the target. This is the anti-vacuity guard: a renderer that ignored
        // interpolation and drew the latest snapshot would already be at x=4.
        let x0 = interp.draws()[0].feet.x;
        assert!(
            x0 < 0.5,
            "on a fresh snapshot the mob must start from its old pose, was x={x0}"
        );

        // Half the window later it must be strictly between old and new — a snap
        // (jump straight to 4) or a freeze (stuck at 0) both fail this.
        interp.update(&[snap(1, target, 0.0)], INTERP_WINDOW / 2.0);
        let xm = interp.draws()[0].feet.x;
        assert!(
            xm > 0.5 && xm < 3.5,
            "half the window in, the mob should be mid-way, was x={xm}"
        );

        // A full window after the snapshot it reaches the target.
        interp.update(&[snap(1, target, 0.0)], INTERP_WINDOW);
        let xf = interp.draws()[0].feet.x;
        assert!(
            (xf - 4.0).abs() < 1.0e-3,
            "after the window it arrives, was x={xf}"
        );
    }

    #[test]
    fn a_single_tick_move_keeps_gliding_between_sparse_packets() {
        // The bug this module was fixed for: with a one-tick ease window, a mob
        // whose move packets arrive less often than every tick reaches its target
        // in 50 ms and then freezes until the next packet — a visible stutter.
        // With vanilla's three-tick window it must still be advancing a full tick
        // after the last packet. This is the regression guard on INTERP_STEPS.
        let mut interp = EntityInterpolator::new();
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], INTERP_WINDOW);
        // One packet: the mob steps one block. No further packets arrive.
        interp.update(&[snap(1, Vec3::new(1.0, 0.0, 0.0), 0.0)], 0.0);

        // Sample the drawn x each render frame for the next three ticks at 60 fps
        // and require it to keep increasing well past the first tick — a one-tick
        // window would have plateaued at x=1 by 50 ms.
        let frame = 1.0 / 60.0;
        let mut last = interp.draws()[0].feet.x;
        let mut advanced_after_one_tick = false;
        let mut elapsed = 0.0;
        while elapsed < INTERP_WINDOW - 1.0e-4 {
            interp.update(&[snap(1, Vec3::new(1.0, 0.0, 0.0), 0.0)], frame);
            elapsed += frame;
            let x = interp.draws()[0].feet.x;
            assert!(
                x + 1.0e-4 >= last,
                "drawn x must never step backwards, {last} -> {x}"
            );
            if elapsed > TICK + frame && x > last + 1.0e-5 {
                advanced_after_one_tick = true;
            }
            last = x;
        }
        assert!(
            advanced_after_one_tick,
            "the mob must still be moving after the first tick (was it, x plateaued at {last}?)"
        );
        assert!(
            (last - 1.0).abs() < 1.0e-3,
            "after the full window the mob should have reached the target, was {last}"
        );
    }

    #[test]
    fn a_despawned_entity_stops_being_drawn() {
        let mut interp = EntityInterpolator::new();
        interp.update(&[snap(1, Vec3::ZERO, 0.0), snap(2, Vec3::X, 0.0)], 0.016);
        assert_eq!(interp.len(), 2);
        // Entity 2 vanishes from the report.
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], 0.016);
        let draws = interp.draws();
        assert_eq!(draws.len(), 1, "the despawned entity must be gone");
    }

    #[test]
    fn yaw_interpolates_along_the_shortest_arc_across_the_wrap() {
        let mut interp = EntityInterpolator::new();
        interp.update(&[snap(1, Vec3::ZERO, 350.0)], INTERP_WINDOW);
        // Turn to 10°: the short way is +20° through 360/0, not −340° through 180.
        interp.update(&[snap(1, Vec3::ZERO, 10.0)], 0.0);
        interp.update(&[snap(1, Vec3::ZERO, 10.0)], INTERP_WINDOW / 2.0);
        let y = interp.draws()[0].yaw;
        // Halfway along the +20° arc from 350° is 360° ≡ 0°. Reject the long-way
        // answer (~180°), which is what naive linear lerp would give.
        let near_zero = y.rem_euclid(360.0);
        let dist = near_zero.min(360.0 - near_zero);
        assert!(dist < 5.0, "yaw should pass through ~0°, was {y}");
    }

    #[test]
    fn head_yaw_interpolates_independently_of_the_body() {
        // A mob can turn its head without turning its body; the interpolator must
        // ease head yaw separately and along the shortest arc. A snapshot that
        // changes only head yaw (body and position unchanged) must still animate.
        let mut interp = EntityInterpolator::new();
        let mut s = snap(1, Vec3::ZERO, 0.0);
        s.head_yaw = 350.0;
        interp.update(std::slice::from_ref(&s), INTERP_WINDOW);
        // Head turns to 10° (short arc +20° through 0), body stays at 0.
        s.head_yaw = 10.0;
        interp.update(std::slice::from_ref(&s), 0.0);
        interp.update(std::slice::from_ref(&s), INTERP_WINDOW / 2.0);
        let d = &interp.draws()[0];
        assert!(
            d.yaw.abs() < 1.0e-3,
            "body yaw must stay put while only the head turns, was {}",
            d.yaw
        );
        let near_zero = d.head_yaw.rem_euclid(360.0);
        let dist = near_zero.min(360.0 - near_zero);
        assert!(dist < 5.0, "head yaw should pass through ~0°, was {}", d.head_yaw);
    }

    /// Drive a mob at a steady `v` blocks/tick for `ticks` server ticks, one
    /// packet per tick and one render frame per tick, and report the walk
    /// amplitude and the phase advanced over the last ten ticks.
    fn walk_at(v: f32, ticks: usize) -> (f32, f32) {
        let mut interp = EntityInterpolator::new();
        let mut pos = Vec3::ZERO;
        interp.update(&[snap(1, pos, 0.0)], INTERP_WINDOW);
        let mut phase_at_mark = 0.0;
        let mark = ticks.saturating_sub(10);
        for i in 0..ticks {
            pos.x += v;
            interp.update(&[snap(1, pos, 0.0)], TICK);
            if i == mark {
                phase_at_mark = interp.draws()[0].anim.limb_swing;
            }
        }
        let d = &interp.draws()[0].anim;
        (d.limb_swing_amount, d.limb_swing - phase_at_mark)
    }

    /// The reported defect: legs swing far too fast.
    ///
    /// Vanilla's amplitude is `min(distance * 4, 1)` on the **per-tick** travel.
    /// This walks a mob at a fixed speed and checks the amplitude the animator
    /// actually receives against that closed form. The measure this replaced
    /// used the interpolation *gap*, which settles at `3 * v` (see the module
    /// docs) — at `v = 0.05` that is `min(0.6, 1) = 0.6` instead of `0.2`, and
    /// because `WalkAnimation::position` accumulates the amplitude every tick,
    /// the phase ran 3× fast as well. Both halves are asserted, since fixing the
    /// amplitude without the phase would leave the legs still visibly quick.
    #[test]
    fn limb_swing_tracks_per_tick_travel_not_the_interpolation_gap() {
        for v in [0.02f32, 0.05, 0.1] {
            let (amount, phase_10) = walk_at(v, 120);
            let want = walk_target_speed(v);
            assert!(
                (amount - want).abs() < 0.05,
                "at {v} blocks/tick the amplitude should settle near vanilla's {want}, got \
                 {amount}. The old gap-based measure gives {} — a factor of {INTERP_STEPS}",
                walk_target_speed(v * INTERP_STEPS)
            );
            // Phase advances by `speed` per tick, so ten ticks is ~10 * amount.
            let want_phase = want * 10.0;
            assert!(
                (phase_10 - want_phase).abs() < want_phase * 0.25 + 0.05,
                "at {v} blocks/tick the phase advanced {phase_10} over ten ticks, expected \
                 ~{want_phase} — the leg cycle frequency is wrong, not just its amplitude"
            );
        }
    }

    /// The control the assertion above needs: at a walking speed *below*
    /// vanilla's saturation point the amplitude must be strictly less than 1.
    /// The old measure saturated at a third of the travel, so every mob that
    /// moved at all swung its legs at full throw — which is why the test above
    /// cannot be satisfied by simply clamping.
    #[test]
    fn a_slow_walk_does_not_saturate_the_limb_swing() {
        let (slow, _) = walk_at(0.05, 120);
        let (fast, _) = walk_at(0.30, 120);
        assert!(
            slow < 0.5,
            "a 0.05 blocks/tick amble swung at amplitude {slow}; vanilla gives 0.2"
        );
        assert!(
            fast > 0.95,
            "a 0.30 blocks/tick sprint should still saturate, got {fast}"
        );
        assert!(slow < fast);
    }

    #[test]
    fn a_mob_that_stops_walking_decays_to_standing() {
        let mut interp = EntityInterpolator::new();
        let mut pos = Vec3::ZERO;
        interp.update(&[snap(1, pos, 0.0)], INTERP_WINDOW);
        for _ in 0..40 {
            pos.x += 0.1;
            interp.update(&[snap(1, pos, 0.0)], TICK);
        }
        assert!(interp.draws()[0].anim.limb_swing_amount > 0.2, "was walking");
        // The mob stops: same position reported for two seconds.
        for _ in 0..40 {
            interp.update(&[snap(1, pos, 0.0)], TICK);
        }
        let amount = interp.draws()[0].anim.limb_swing_amount;
        assert!(
            amount < 0.01,
            "a standing mob still swings at {amount} — it will moonwalk on the spot"
        );
    }

    #[test]
    fn pitch_interpolates_linearly() {
        let mut interp = EntityInterpolator::new();
        let mut s = snap(1, Vec3::ZERO, 0.0);
        s.pitch = -30.0;
        interp.update(std::slice::from_ref(&s), INTERP_WINDOW);
        s.pitch = 30.0;
        interp.update(std::slice::from_ref(&s), 0.0);
        interp.update(std::slice::from_ref(&s), INTERP_WINDOW / 2.0);
        let p = interp.draws()[0].pitch;
        assert!(p.abs() < 1.0, "half the window from -30 to 30 is ~0, was {p}");
    }
}
