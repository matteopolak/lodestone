//! Builds a [`lodestone_render::Camera`] from a physics [`PlayerState`].
//!
//! All camera conventions (RH, Y-up, yaw 0 = south, eye height 1.62, vertical
//! FOV 70, near 0.05, far = 4× render distance) are owned by the render crate;
//! this module only *reads* them so the shell never redefines them.
//!
//! # The eye height is a parameter, not a constant
//!
//! Vanilla's eye height is pose-dependent (`Avatar.java:22-36`: `0.4` swimming,
//! `1.27` crouching, `1.62` standing), so [`build_camera`] takes it explicitly.
//! It used to hardcode
//! [`PLAYER_EYE_HEIGHT`](lodestone_render::camera::PLAYER_EYE_HEIGHT), and the
//! swimming work therefore pre-biased the *feet* Y by the difference at the call
//! site. That was arithmetically identical and is why the swim-camera gate reads
//! the same number either way — but the value passed in was then not the feet
//! while any non-standing pose was active, which is the kind of comment-shaped
//! lie that costs someone an afternoon. Pass the eye height.
//!
//! # The eye height must be *smoothed*, not read raw (issue #59)
//!
//! A pose change (standing `1.62` ↔ swimming `0.4`) is a **snap** in
//! [`PlayerState::eye_height`] — it is set once, atomically, by
//! `crate::pose::update_player_pose` (`Player.java:343-357`). Passing that
//! straight into [`build_camera`] every frame is exactly what vanilla does
//! *not* do, and is the source of the entering/leaving-swim camera jerk: real
//! `Camera` (`.cache/mc/26.2/client-src/net/minecraft/client/Camera.java`)
//! keeps its **own** `eyeHeight`/`eyeHeightOld` pair, entirely separate from
//! the entity's, and eases toward the target by half the remaining distance
//! every tick:
//!
//! ```java
//! // Camera.tick(), :80-88
//! this.eyeHeightOld = this.eyeHeight;
//! this.eyeHeight = this.eyeHeight + (this.entity.getEyeHeight() - this.eyeHeight) * 0.5F;
//! ```
//!
//! and reads it with the same current/previous + partial-tick shape as the
//! player's own position (`Camera.alignWithEntity`, `:246-264`):
//!
//! ```java
//! Mth.lerp(partialTicks, this.entity.yo, this.entity.getY())
//!    + Mth.lerp(partialTicks, this.eyeHeightOld, this.eyeHeight)
//! ```
//!
//! [`EyeHeightSmoother`] is that pair. **It is not `swimAmount`.** `swimAmount`
//! (`LivingEntity.java:174,275-276,3478-3483`, modelled as
//! [`PlayerState::swim_amount`]/`swim_amount_o`) is a linear `0..1` ramp at
//! `0.09`/tick that blends the swimming **model**'s body-pitch animation —
//! grepping every `.cache/mc/26.2/client-src` hit for `swimAmount` turns up
//! only `HumanoidModel`, `HumanoidMobRenderer`, `DrownedRenderer`/`DrownedModel`
//! and the humanoid render state, never `Camera` or `GameRenderer`. The two
//! ramps happen to share the "current + previous twin, partial-tick lerp"
//! shape, but their update rules differ (exponential decay toward a target vs.
//! a linear clamped increment) and they smooth two unrelated things, so they
//! are kept as two separate types rather than forced into one.
//!
//! A working smoother needs state that outlives one frame — unlike everything
//! else in this module, [`EyeHeightSmoother`] cannot be a pure function of the
//! current [`PlayerState`]. The intended owner is the same place that already
//! owns the analogous per-tick smoothing state
//! (`lodestone_shell::sim::Sim::body_pose`, ticked once per physics tick in
//! `Sim::step`): a `Sim`-owned `EyeHeightSmoother`, ticked once per physics
//! tick from the *post-tick* pose's eye height, and read back through
//! [`EyeHeightSmoother::lerp`] with the frame's interpolation alpha wherever
//! `Sim::camera` currently reads `interp.eye_height` raw
//! (`lodestone-shell/src/sim.rs:3429-3437`). That call site is out of this
//! change's scope (`sim.rs` is held by another agent) — see `docs/swimming.md`
//! for the exact patch to apply there.
//!
//! # Riding needs nothing here, and that is a measured claim
//!
//! The obvious place to put "the camera sits on the vehicle" is this module, and
//! it would be wrong. 26.2's `Camera.alignWithEntity`
//! (`.cache/mc/26.2/client-src/net/minecraft/client/Camera.java:246-264`) has
//! **no `isPassenger()` branch** — it lerps `entity.xo/yo/zo` and adds the
//! smoothed eye height, mounted or not. The single exception is a fix-up for
//! *new-behaviour minecarts* (`:247-256`), which recomputes the attachment
//! against `behavior.getCartLerpPosition(partialTicks)` so the camera does not
//! stutter against a cart interpolating between server positions; that is a
//! smoothing correction, not a different camera.
//!
//! Riding also does not change the eye height: `Player.updatePlayerPose`
//! (`src/net/minecraft/world/entity/player/Player.java:343-357`) has no riding
//! case and there is no `SITTING` pose, so a mounted player keeps
//! `Avatar.DEFAULT_EYE_HEIGHT = 1.62` (`Avatar.java:16`).
//!
//! So the whole of camera-on-the-vehicle is `lodestone_ecs::player::
//! pin_passenger_to_vehicle` moving the player's **feet** onto the seat — which
//! this module then reads through [`PlayerState`] exactly as it reads a walking
//! player's. Adding a passenger branch here would double-apply the attachment.
//! The one thing genuinely missing is the minecart lerp fix-up above, which needs
//! per-vehicle interpolation state the ECS does not hold yet; its symptom is
//! camera stutter on a *moving* vehicle, not a wrong seat.

use glam::Vec3;
use lodestone_physics::{Aabb, CollisionView, PlayerState};
use lodestone_render::Camera;

/// Vertical field of view in degrees (vanilla default, "Normal" FOV).
pub const FOV_Y_DEGREES: f32 = 70.0;
/// Near plane distance in blocks.
pub const NEAR: f32 = 0.05;

/// Vanilla's third-person "back" camera distance, in blocks
/// (`Camera`'s zoom starts at `4.0` and is only ever pulled *in* from there by
/// collision, never pushed further out). This is the *desired* pullback before
/// [`collision_pullback`] clamps it against real geometry.
pub const THIRD_PERSON_DISTANCE: f32 = 4.0;

/// How far short of a real collision surface the pulled-back camera stops, in
/// blocks. Without this the eye would sit exactly on the wall it just clipped
/// against (and could poke a hair through it on the next frame's float
/// rounding); vanilla's own `Camera.getMaxZoom` shaves a small buffer the same
/// way (`partialTickTime` clip step 0.1).
pub const COLLISION_MARGIN: f32 = 0.1;

/// Builds the render camera for the current camera mode: the true first-person
/// eye when `third_person` is `false`, or that same eye pulled straight
/// backward along its own view direction — vanilla's actual "back" mode
/// algorithm, not an approximation of it — clamped so it never passes through
/// real collision geometry.
///
/// `view` is whichever [`CollisionView`] adapter the caller already has live
/// (`LiveCollision` on a server, `WorldCollision` on the offline fixture) —
/// this function is generic over it so it needs no dependency on
/// `crate::collision`'s concrete types.
#[must_use]
pub fn third_person_camera(eye: Camera, third_person: bool, view: &impl CollisionView) -> Camera {
    if !third_person {
        return eye;
    }
    let back = -eye.forward();
    let clamped = collision_pullback(eye.position, back, THIRD_PERSON_DISTANCE, view);
    // The margin only matters once something real was actually hit: in open
    // air `clamped` already equals the desired distance exactly, and shaving
    // a further 0.1 off *that* would make third person sit permanently 0.1
    // blocks closer than vanilla's own default zoom for no reason.
    let distance = if clamped < THIRD_PERSON_DISTANCE {
        (clamped - COLLISION_MARGIN).max(0.0)
    } else {
        clamped
    };
    let mut cam = eye;
    cam.position += back * distance;
    cam
}

/// How far the camera may travel from `eye` along `dir` (assumed already
/// pointing away from the player, i.e. "backward") before it would pass
/// through real collision geometry, clamped to `desired` blocks.
///
/// Marches voxel by voxel along the ray — the same grid-DDA traversal
/// `crate::raycast::raycast` uses for block targeting — but tests each visited
/// cell against its **real** collision boxes
/// ([`CollisionView::collision_boxes`]) with an exact ray/AABB intersection,
/// rather than a full-cube occlusion predicate. `LiveCollision::is_solid`'s own
/// doc comment warns that method is the *occlusion* answer, not the collision
/// one (a slab collides only to half a block and does not occlude at all) —
/// using it here would pull the camera in a full block early on a slab, or
/// (worse, since occlusion and collision also disagree the other way for
/// blocks like barriers) let it clip through geometry that has no visual face
/// to trigger on. Exact per-box intersection gets both right.
#[must_use]
pub fn collision_pullback(eye: Vec3, dir: Vec3, desired: f32, view: &impl CollisionView) -> f32 {
    if !desired.is_finite() || desired <= 0.0 {
        return 0.0;
    }
    let len = dir.length();
    if !len.is_finite() || len < 1e-6 {
        return desired;
    }
    let d = dir / len;

    let mut voxel = [
        eye.x.floor() as i32,
        eye.y.floor() as i32,
        eye.z.floor() as i32,
    ];
    let step = [sign(d.x), sign(d.y), sign(d.z)];
    let o = [eye.x, eye.y, eye.z];
    let dd = [d.x, d.y, d.z];
    let mut t_max = [0.0f32; 3];
    let mut t_delta = [0.0f32; 3];
    for a in 0..3 {
        if dd[a] == 0.0 {
            t_max[a] = f32::INFINITY;
            t_delta[a] = f32::INFINITY;
        } else {
            let next = if dd[a] > 0.0 {
                voxel[a] as f32 + 1.0
            } else {
                voxel[a] as f32
            };
            t_max[a] = (next - o[a]) / dd[a];
            t_delta[a] = (1.0 / dd[a]).abs();
        }
    }

    let mut boxes = Vec::new();
    // The eye's own cell first — third-person pullback starts from a point
    // that is already inside air in normal play, but a crouch/step edge can
    // leave it overlapping geometry, and that must clip too.
    view.collision_boxes(voxel[0], voxel[1], voxel[2], &mut boxes);
    if let Some(t) = nearest_entry(eye, d, &boxes, desired) {
        return t;
    }

    let iterations = (desired.ceil() as i32 * 3 + 8).max(8);
    for _ in 0..iterations {
        let axis = if t_max[0] < t_max[1] && t_max[0] < t_max[2] {
            0
        } else if t_max[1] < t_max[2] {
            1
        } else {
            2
        };
        voxel[axis] += step[axis];
        let t = t_max[axis];
        t_max[axis] += t_delta[axis];
        if t > desired {
            return desired;
        }
        boxes.clear();
        view.collision_boxes(voxel[0], voxel[1], voxel[2], &mut boxes);
        if let Some(hit) = nearest_entry(eye, d, &boxes, desired) {
            return hit;
        }
    }
    desired
}

/// The nearest entry distance (`<= limit`) among `boxes`, or `None` if the ray
/// misses every one of them within range.
fn nearest_entry(origin: Vec3, dir: Vec3, boxes: &[Aabb], limit: f32) -> Option<f32> {
    boxes
        .iter()
        .filter_map(|b| ray_aabb_entry(origin, dir, b))
        .filter(|t| *t <= limit)
        .fold(None, |acc: Option<f32>, t| Some(acc.map_or(t, |a| a.min(t))))
}

/// Exact ray/AABB intersection (the slab method): the distance along the
/// already-normalised `dir` at which the ray first enters `aabb`, or `None` if
/// it misses, or if `aabb` lies entirely behind `origin`.
fn ray_aabb_entry(origin: Vec3, dir: Vec3, aabb: &Aabb) -> Option<f32> {
    let min = Vec3::new(aabb.min_x as f32, aabb.min_y as f32, aabb.min_z as f32);
    let max = Vec3::new(aabb.max_x as f32, aabb.max_y as f32, aabb.max_z as f32);
    let o = [origin.x, origin.y, origin.z];
    let d = [dir.x, dir.y, dir.z];
    let lo = [min.x, min.y, min.z];
    let hi = [max.x, max.y, max.z];

    let mut t_min = 0.0f32;
    let mut t_max = f32::INFINITY;
    for a in 0..3 {
        if d[a].abs() < 1e-9 {
            if o[a] < lo[a] || o[a] > hi[a] {
                return None;
            }
        } else {
            let inv = 1.0 / d[a];
            let (mut t0, mut t1) = ((lo[a] - o[a]) * inv, (hi[a] - o[a]) * inv);
            if t0 > t1 {
                std::mem::swap(&mut t0, &mut t1);
            }
            t_min = t_min.max(t0);
            t_max = t_max.min(t1);
            if t_min > t_max {
                return None;
            }
        }
    }
    if t_max < 0.0 { None } else { Some(t_min.max(0.0)) }
}

fn sign(v: f32) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

/// `Camera`'s own `eyeHeight`/`eyeHeightOld` pair (`Camera.java:59-60,80-88`) —
/// the fix for issue #59's entering/leaving-swim camera jerk.
///
/// This is deliberately **not** [`PlayerState::swim_amount`]; see the module
/// docs for why the two must stay separate. Own one of these per camera (per
/// [`PlayerState`], for split-screen or spectating another entity) and:
///
/// * call [`Self::tick`] exactly once per **physics** tick, with the target
///   pose's eye height (`entity.getEyeHeight()`, i.e. [`PlayerState::eye_height`]
///   *after* that tick's pose update) — never once per frame, or the `0.5`
///   decay rate would not match vanilla's fixed 20 Hz tick;
/// * read the camera's actual eye height every **frame** via [`Self::lerp`]
///   with the frame's partial-tick alpha, exactly like the player's own
///   position interpolation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyeHeightSmoother {
    /// `Camera.eyeHeightOld` — the value as of the *previous* tick.
    previous: f32,
    /// `Camera.eyeHeight` — the value as of the *current* tick.
    current: f32,
}

impl EyeHeightSmoother {
    /// A smoother with no jerk to ease out of — both the current and previous
    /// value start at `initial_eye_height`, matching a `Camera` freshly bound
    /// to an entity (its `eyeHeight`/`eyeHeightOld` fields default to `0.0F`,
    /// but the very next [`Self::tick`] call, given a real target, is what
    /// actually matters here; seeding both fields equal is what keeps that
    /// first tick from itself producing a spurious half-jump).
    #[must_use]
    pub fn new(initial_eye_height: f32) -> Self {
        Self {
            previous: initial_eye_height,
            current: initial_eye_height,
        }
    }

    /// `Camera.tick()` (`Camera.java:80-88`): ease halfway from the current
    /// smoothed value toward `target_eye_height` (the entity's real, possibly
    /// just-snapped, pose eye height). Call this once per physics tick.
    pub fn tick(&mut self, target_eye_height: f32) {
        self.previous = self.current;
        self.current += (target_eye_height - self.current) * 0.5;
    }

    /// `Mth.lerp(partialTicks, eyeHeightOld, eyeHeight)` — the frame's actual
    /// eye height, interpolated between the last two ticks' smoothed values by
    /// `alpha` (the same partial-tick fraction used for position
    /// interpolation elsewhere). `alpha` is not clamped, matching `Mth.lerp`.
    #[must_use]
    pub fn lerp(&self, alpha: f32) -> f32 {
        self.previous + (self.current - self.previous) * alpha
    }
}

// ---------------------------------------------------------------------------
// View bobbing (issue #58)
// ---------------------------------------------------------------------------

/// Vanilla's `hurtDuration`, set alongside `hurtTime` by both
/// `LivingEntity.animateHurt` (`LivingEntity.java:1873-1876`) and
/// `handleDamageEvent` (`:2044-2049`). Ten ticks — half a second.
pub const HURT_DURATION_TICKS: f32 = 10.0;

/// The walk-bob state vanilla spreads across `ClientAvatarState` and
/// `LocalPlayer`, gathered into one per-camera value (issue #58).
///
/// # What this is, and the four fields that are really two pairs
///
/// Everything here is a `current`/`previous` twin read back with the frame's
/// partial-tick alpha, exactly like [`EyeHeightSmoother`] above and for the same
/// reason: the update rule runs at the fixed 20 Hz tick, and a frame between two
/// ticks must interpolate rather than re-run it.
///
/// * `walk_dist`/`walk_dist_o` — `ClientAvatarState.walkDist`/`walkDistO`. The
///   **phase** of the bob: total horizontal distance walked, scaled by `0.6`
///   (`LocalPlayer.move`, `LocalPlayer.java:989`:
///   `addWalkedDistance(Mth.length(deltaX, deltaZ) * 0.6F)` — note it is the
///   distance actually *moved*, post-collision, not the intended delta).
/// * `bob`/`bob_o` — `ClientAvatarState.bob`/`bobO`. The **amplitude**, an
///   exponential ease toward `min(0.1, horizontal speed)`
///   (`AbstractClientPlayer.updateBob`, `ClientAvatarState.updateBob`):
///   `bob += (target - bob) * 0.4`, and the target is a flat `0.0` unless the
///   player is on the ground and neither dead nor swimming. That gate is why the
///   bob fades out in mid-air instead of snapping.
///
/// The `0.1` ceiling is the reason the bob does not grow without bound while
/// sprinting: vanilla's sprint speed exceeds it, so amplitude saturates and only
/// the phase speeds up.
///
/// # `hurt_time`/`hurt_dir` are not interpolated the same way
///
/// They have no `previous` twin because vanilla does not keep one: `Camera.setup`
/// (`Camera.java:135-137`) reads `hurtTime - partialTicks` and `hurtDir` raw. The
/// subtraction is what smooths the flash out, and it is also why
/// [`BobFrame::hurt_roll_degrees`] must tolerate a *negative* `hurt` — vanilla's
/// `bobHurt` returns early on `hurt < 0.0F`, which is the ordinary case in the
/// frames just after the countdown reaches zero.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ViewBob {
    walk_dist: f32,
    walk_dist_o: f32,
    bob: f32,
    bob_o: f32,
    hurt_time: u32,
    hurt_dir_degrees: f32,
}

impl ViewBob {
    /// A camera that has not moved yet — no phase, no amplitude, no hurt.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One fixed physics tick.
    ///
    /// `moved_horizontal` is the distance the feet actually travelled this tick
    /// in the XZ plane (post-collision — a player walking into a wall does not
    /// bob). `speed_horizontal` is the horizontal speed vanilla's `updateBob`
    /// reads off `getDeltaMovement()`.
    ///
    /// # Order matters and is vanilla's, not the readable one
    ///
    /// `AbstractClientPlayer.tick` saves `walkDistO = walkDist` **before**
    /// `super.tick()` runs the movement, and `aiStep` calls `updateBob()`
    /// **before** `super.aiStep()` — so within one tick the amplitude is eased
    /// first and the phase advanced second. Doing it the other way round shifts
    /// the bob by one tick against the stride, which is a phase error rather
    /// than an amplitude one and therefore does not show up as "too much bob".
    pub fn tick(
        &mut self,
        moved_horizontal: f32,
        speed_horizontal: f32,
        on_ground: bool,
        dead: bool,
        swimming: bool,
    ) {
        // 1. `ClientAvatarState.tick`: the phase's previous value, saved before
        //    this tick's movement is added to it.
        self.walk_dist_o = self.walk_dist;
        // 2. `AbstractClientPlayer.updateBob` -> `ClientAvatarState.updateBob`.
        let target = if on_ground && !dead && !swimming {
            speed_horizontal.min(0.1)
        } else {
            0.0
        };
        self.bob_o = self.bob;
        self.bob += (target - self.bob) * 0.4;
        // 3. `LocalPlayer.move` -> `addWalkedDistance`.
        self.walk_dist += moved_horizontal * 0.6;
        // 4. `LivingEntity.tick`'s countdown, saturating at zero — the same
        //    rule `lodestone_ecs::ingest::tick_hurt_time` applies to remote
        //    entities.
        self.hurt_time = self.hurt_time.saturating_sub(1);
    }

    /// A damage report: `LivingEntity.animateHurt(yaw)` (`:1873-1876`), which
    /// resets the countdown to [`HURT_DURATION_TICKS`] and records the direction
    /// the hit came from. `yaw_degrees` is the wire value from
    /// `ClientboundHurtAnimationPacket`.
    pub fn hurt(&mut self, yaw_degrees: f32) {
        self.hurt_time = HURT_DURATION_TICKS as u32;
        self.hurt_dir_degrees = yaw_degrees;
    }

    /// The frame's interpolated bob, for the partial-tick fraction `alpha`.
    #[must_use]
    pub fn frame(&self, alpha: f32) -> BobFrame {
        // `ClientAvatarState.getBackwardsInterpolatedWalkDistance`:
        //     float wda = walkDist - walkDistO;
        //     return -(walkDist + wda * partialTicks);
        // Note this is **not** `Mth.lerp` — it extrapolates *forward* from the
        // current value and then negates, so it runs slightly ahead of the
        // interpolated position rather than between the two ticks. The sibling
        // `getInterpolatedWalkDistance` is the lerp, and nothing in `bobView`
        // uses it. Reading the lerp here instead would be a plausible-looking
        // half-tick phase error.
        let wda = self.walk_dist - self.walk_dist_o;
        BobFrame {
            walk_phase: -(self.walk_dist + wda * alpha),
            bob: self.bob_o + (self.bob - self.bob_o) * alpha,
            // `Camera.setup`: `hurtTime - cameraEntityPartialTicks`.
            hurt: self.hurt_time as f32 - alpha,
            hurt_dir_degrees: self.hurt_dir_degrees,
        }
    }
}

/// One frame's worth of interpolated bob input — what vanilla puts on
/// `CameraRenderState.entityRenderState` for `GameRenderer.bobView`/`bobHurt` to
/// read (`Camera.java:135-152`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BobFrame {
    /// `backwardsInterpolatedWalkDistance`. **Already negated** by
    /// [`ViewBob::frame`], matching vanilla, so every use below is a bare
    /// `walk_phase * PI` with no sign of its own.
    pub walk_phase: f32,
    /// The interpolated amplitude, `0.0..=0.1`.
    pub bob: f32,
    /// `hurtTime - partialTicks`. Negative means no flash — see
    /// [`Self::hurt_roll_degrees`].
    pub hurt: f32,
    /// `hurtDir`, in degrees.
    pub hurt_dir_degrees: f32,
}

impl BobFrame {
    /// `GameRenderer.bobView`'s eye-space translation
    /// (`GameRenderer.java:323-327`), as `(x, y, z)`:
    ///
    /// ```java
    /// poseStack.translate(
    ///    Mth.sin(bd * (float) Math.PI) * bob * 0.5F,
    ///    -Math.abs(Mth.cos(bd * (float) Math.PI) * bob),
    ///    0.0F
    /// );
    /// ```
    ///
    /// The `abs` on Y is the whole character of the walk bob: the sway is a full
    /// sine (left, right, left) but the dip is **rectified**, so the eye drops
    /// twice per stride cycle — once per footfall — instead of rising above its
    /// resting height on alternate steps. Dropping the `abs` halves the apparent
    /// cadence and is not visible as an error in a still frame.
    #[must_use]
    pub fn view_translation(&self) -> Vec3 {
        let phase = self.walk_phase * std::f32::consts::PI;
        Vec3::new(
            phase.sin() * self.bob * 0.5,
            -(phase.cos() * self.bob).abs(),
            0.0,
        )
    }

    /// `bobView`'s Z rotation in degrees (`GameRenderer.java:328`):
    /// `sin(bd * PI) * bob * 3.0`. A **roll**, in phase with the sway.
    #[must_use]
    pub fn view_roll_degrees(&self) -> f32 {
        (self.walk_phase * std::f32::consts::PI).sin() * self.bob * 3.0
    }

    /// `bobView`'s X rotation in degrees (`GameRenderer.java:329`):
    /// `abs(cos(bd * PI - 0.2) * bob) * 5.0`.
    ///
    /// **The `- 0.2` is inside the cosine and is in radians, not a multiple of
    /// PI** — `Mth.cos(backwardsInterpolatedWalkDistance * (float) Math.PI - 0.2F)`.
    /// It is a small phase lead that makes the nod peak just before the dip
    /// bottoms out. Folding it in as `(bd - 0.2) * PI` would be a 36° phase
    /// error and would still look like a nod.
    ///
    /// Rectified like the dip, so it is always a nod in one direction.
    #[must_use]
    pub fn view_nod_degrees(&self) -> f32 {
        let phase = self.walk_phase * std::f32::consts::PI - 0.2;
        (phase.cos() * self.bob).abs() * 5.0
    }

    /// `GameRenderer.bobHurt`'s tilt magnitude in degrees
    /// (`GameRenderer.java:305-315`), before it is swung onto the damage
    /// direction:
    ///
    /// ```java
    /// if (hurt < 0.0F) { return; }
    /// hurt /= hurtDuration;
    /// hurt = Mth.sin(hurt * hurt * hurt * hurt * (float) Math.PI);
    /// ...
    /// float tiltAmount = (float)(-hurt * 14.0 * damageTiltStrength);
    /// ```
    ///
    /// The quartic inside the sine is doing real work: `sin(t⁴ · π)` stays near
    /// zero for most of the window and then spikes, so the tilt is a sharp jolt
    /// at the moment of the hit rather than a slow lean. `sin(t · π)` would be a
    /// smooth arc over the whole half-second and would read as nausea.
    ///
    /// `damage_tilt_strength` is vanilla's accessibility option
    /// (`Options.java:876-883`, default `1.0`); pass `1.0` for the default.
    #[must_use]
    pub fn hurt_roll_degrees(&self, damage_tilt_strength: f32) -> f32 {
        if self.hurt < 0.0 {
            return 0.0;
        }
        let t = self.hurt / HURT_DURATION_TICKS;
        let shaped = (t * t * t * t * std::f32::consts::PI).sin();
        -shaped * 14.0 * damage_tilt_strength
    }

    /// The full eye-space bob transform: `bobHurt` then `bobView`, pushed onto
    /// one pose stack in that order (`GameRenderer.java:534-536`), which is
    /// `Hurt * View` as a matrix product.
    ///
    /// # Why this is a *projection* post-multiply in vanilla, and what that means here
    ///
    /// `GameRenderer.renderLevel` does `projectionMatrix.mul(bobStack.last().pose())`
    /// — the bob lands **between** the projection and the view, so it acts on
    /// **eye-space** coordinates. Our eye space is the same as vanilla's (`+X`
    /// right, `+Y` up, forward `-Z`: `Camera.FORWARDS` is `(0, 0, -1)` and
    /// `glam::camera::rh::view::look_to_mat4` produces the same basis), so every
    /// constant above transcribes with **no sign flip**. The `[0,1]`-vs-reversed-Z
    /// depth difference `CLAUDE.md` warns about lives entirely inside the
    /// projection matrix, which sits to the *left* of this one in `P · B · V` and
    /// therefore cannot affect it.
    ///
    /// Nothing in the shell multiplies this into a projection today —
    /// [`bobbed_camera`] folds it into the [`Camera`] instead, because
    /// `Camera::view_projection` is what the GPU layer reads and it is built from
    /// fields. This method exists as the **reference** that fold is tested
    /// against; see [`bobbed_camera`]'s docs for what that costs.
    #[must_use]
    pub fn eye_transform(&self, damage_tilt_strength: f32) -> glam::Mat4 {
        use glam::Mat4;
        // bobHurt: Ry(-d) * Rz(tilt) * Ry(d) — a rotation by `tilt` about the
        // eye-space +Z axis swung `d` degrees about +Y, so a hit from straight
        // ahead (`d == 0`) is pure roll and a hit from the side is pure nod.
        let d = self.hurt_dir_degrees.to_radians();
        let tilt = self.hurt_roll_degrees(damage_tilt_strength).to_radians();
        let hurt = Mat4::from_rotation_y(-d) * Mat4::from_rotation_z(tilt) * Mat4::from_rotation_y(d);
        // bobView: T * Rz * Rx.
        let view = Mat4::from_translation(self.view_translation())
            * Mat4::from_rotation_z(self.view_roll_degrees().to_radians())
            * Mat4::from_rotation_x(self.view_nod_degrees().to_radians());
        hurt * view
    }
}

/// Fold a [`BobFrame`] into a [`Camera`], so the bob reaches pixels through the
/// *existing* `Camera::view_projection` rather than needing a second matrix seam.
///
/// # What this models exactly, and the one thing it drops
///
/// A bob matrix `B` applied in eye space gives a combined view matrix `B · V`,
/// and *any* such matrix is still a camera — it has a position and an
/// orientation. This function recovers them mechanically: it builds `B · V`,
/// inverts it, and reads the camera origin and forward direction straight back
/// out. Nothing here picks a sign by hand, which is deliberate — `CLAUDE.md`
/// records shipping an inside-out block because a polarity was asserted rather
/// than derived, and a camera transform is exactly that shape of hazard.
///
/// What it cannot carry is **roll**. [`Camera`] has `position`, `yaw` and
/// `pitch` — two angles — so a decomposed orientation has two degrees of freedom
/// where `B · V` has three.
///
/// **The reason is the parameterisation, not the up vector.** This comment used
/// to say `view_matrix` hardcodes `Vec3::Y` as up, which was true until
/// `d17c731c` and is now false: the basis is derived from vanilla's
/// `Ry(π − yaw) · Rx(−pitch)`, so `up` rotates with the camera. That change
/// fixed a 180° roll at pitch ±90 and did **not** add a roll degree of freedom —
/// the rotation still has a zero `Rz` term. Corrected rather than deleted
/// because §12.114's finding was that a stale comment is precisely why nobody
/// looks again. Concretely:
///
/// | bob term | magnitude | carried? |
/// |---|---|---|
/// | `bobView` translate (sway + dip) | ≤ `0.05` blocks | yes, exactly |
/// | `bobView` nod (`Axis.XP`) | ≤ `0.5°` | yes, exactly |
/// | `bobView` roll (`Axis.ZP`) | ≤ `0.3°` | **no** |
/// | `bobHurt` tilt (`Axis.ZP`, swung by `hurtDir`) | ≤ `14°` | only the component that lands on the nod axis |
///
/// **This is a divergence, recorded rather than hidden.** Carrying roll needs one
/// more field on [`Camera`] (or a `Mat4` hook on `view_projection`), and every
/// full `Camera { .. }` struct literal in the workspace — 48 of them across ~40
/// files, six inside `lodestone-shell/src/gpu.rs` and one in
/// `lodestone-render/src/entity.rs` — would have to change with it. See
/// `docs/view-bobbing.md`. The two roll terms are very different in size, which
/// is why the walk bob is worth landing without it (`0.3°` is below noticing) and
/// the damage tilt is not (`14°` is the whole effect).
///
/// `damage_tilt_strength` is vanilla's accessibility option; pass `1.0` for the
/// default and `0.0` to disable the hurt tilt entirely.
#[must_use]
pub fn bobbed_camera(cam: Camera, frame: BobFrame, damage_tilt_strength: f32) -> Camera {
    // An inert frame returns the camera **bit-identically**, not merely close.
    // Without this the matrix round-trip below perturbs the position by ~1e-5
    // even with an identity bob, which is enough to make "bobbing off is a
    // no-op" and "standing still does not bob" un-assertable — and those are
    // exactly the preconditions every gate downstream leans on. It is also the
    // common case: standing still, and every frame with the option off.
    if frame.bob == 0.0 && frame.hurt_roll_degrees(damage_tilt_strength) == 0.0 {
        return cam;
    }
    let bobbed_view = frame.eye_transform(damage_tilt_strength) * cam.view_matrix();
    let Some(inv) = invertible(bobbed_view) else {
        // A degenerate view is not something a bob can produce, but returning
        // the unbobbed camera is the only safe answer if one ever appears —
        // never a NaN camera, which would blank the frame rather than mis-bob it.
        return cam;
    };
    // The camera origin is whatever world point maps to the eye-space origin,
    // and the camera forward is whatever world direction maps to eye-space
    // `-Z` (`Camera::forward`'s own definition of forward).
    let position = inv.project_point3(Vec3::ZERO);
    let forward = inv.transform_vector3(Vec3::new(0.0, 0.0, -1.0)).normalize();
    let (yaw, pitch) = yaw_pitch_from_forward(forward);
    Camera {
        position,
        yaw,
        pitch,
        ..cam
    }
}

fn invertible(m: glam::Mat4) -> Option<glam::Mat4> {
    if m.determinant().abs() < 1e-12 {
        return None;
    }
    let inv = m.inverse();
    if inv.to_cols_array().iter().all(|v| v.is_finite()) {
        Some(inv)
    } else {
        None
    }
}

/// The inverse of [`Camera::forward`]: given a unit world direction, the
/// `(yaw, pitch)` in degrees that [`Camera`] would need to look along it.
///
/// `Camera::forward` is `(-sin(yaw)cos(pitch), -sin(pitch), cos(yaw)cos(pitch))`,
/// so this is `pitch = asin(-y)` and `yaw = atan2(-x, z)`. Derived from that one
/// expression rather than from a convention, and round-tripped in the tests
/// against `Camera::forward` itself so the two cannot drift.
#[must_use]
pub fn yaw_pitch_from_forward(forward: Vec3) -> (f32, f32) {
    let pitch = (-forward.y).clamp(-1.0, 1.0).asin().to_degrees();
    let yaw = (-forward.x).atan2(forward.z).to_degrees();
    (yaw, pitch)
}

/// Construct the render camera for the given player state, **eye height above
/// the feet**, viewport aspect, and render distance (in chunks).
///
/// `state.position` is the feet — always, in every pose. `eye_height` is what
/// varies; see the module docs.
#[must_use]
pub fn build_camera(
    state: &PlayerState,
    eye_height: f32,
    aspect: f32,
    render_distance: u32,
) -> Camera {
    let feet = glam::Vec3::new(
        state.position.x as f32,
        state.position.y as f32,
        state.position.z as f32,
    );
    Camera {
        position: feet + glam::Vec3::new(0.0, eye_height, 0.0),
        yaw: state.yaw,
        pitch: state.pitch,
        fov_y_degrees: FOV_Y_DEGREES,
        aspect: if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.0
        },
        near: NEAR,
        far: Camera::far_for_render_distance(render_distance, 0),
    }
}

/// Applies the spyglass FOV-zoom modifier (issue #154) to an already-built
/// camera — `AbstractClientPlayer.getFieldOfViewModifier`
/// (`AbstractClientPlayer.java:92-114`) returns `0.1F` outright (overriding
/// every other FOV modifier) when `firstPerson && isScoping()`, and
/// [`lodestone_render::spyglass_fov_modifier`] is the tested pure function
/// for that.
///
/// Deliberately **not** folded into [`build_camera`] itself: `build_camera`'s
/// only production call site is `Sim::camera` in `sim.rs`, which is outside
/// this file's ownership (`CLAUDE.md`'s file-ownership section) — adding a
/// required `scoping` parameter there is a patch for whoever owns that file
/// to apply, not something to force through here. This function is the
/// composable half: `apply_spyglass_fov(build_camera(...), scoping)`, or
/// equivalently `camera.fov_y_degrees *= spyglass_fov_modifier(scoping)`
/// inline, composed with (never overwriting) whatever else already produces
/// `fov_y_degrees`. See `docs/screen-overlays.md`'s "Spyglass" section for
/// the full citation.
#[must_use]
pub fn apply_spyglass_fov(mut camera: Camera, scoping: bool) -> Camera {
    camera.fov_y_degrees *= lodestone_render::spyglass_fov_modifier(scoping);
    camera
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_physics::Vec3d;
    use lodestone_render::camera::PLAYER_EYE_HEIGHT;

    // --- EyeHeightSmoother: Camera.tick()'s eyeHeight/eyeHeightOld pair. ---

    #[test]
    fn a_fresh_smoother_reports_the_seeded_height_before_any_tick() {
        let s = EyeHeightSmoother::new(1.62);
        assert_eq!(s.lerp(0.0), 1.62);
        assert_eq!(s.lerp(1.0), 1.62);
        assert_eq!(s.lerp(0.5), 1.62);
    }

    #[test]
    fn one_tick_covers_exactly_half_the_remaining_distance() {
        // Standing (1.62) diving to swimming (0.4): a real pose snap.
        let mut s = EyeHeightSmoother::new(1.62);
        s.tick(0.4);
        // previous = 1.62 (pre-tick), current = 1.62 + (0.4 - 1.62) * 0.5 = 1.01.
        assert!((s.lerp(0.0) - 1.62).abs() < 1e-6, "partial 0 reads the old value");
        assert!((s.lerp(1.0) - 1.01).abs() < 1e-6, "partial 1 reads the new value");
        assert!(
            (s.lerp(0.5) - 1.315).abs() < 1e-6,
            "partial 0.5 is the midpoint of old/new, not of target"
        );
    }

    #[test]
    fn repeated_ticks_converge_toward_the_target_without_ever_snapping() {
        let mut s = EyeHeightSmoother::new(1.62);
        let mut prev_gap = f32::INFINITY;
        for _ in 0..20 {
            s.tick(0.4);
            let gap = (s.lerp(1.0) - 0.4).abs();
            assert!(gap <= prev_gap, "distance to target must not increase");
            prev_gap = gap;
        }
        assert!(prev_gap < 1e-4, "converges close to the target: {prev_gap}");
        assert_ne!(prev_gap, 0.0, "exponential decay never exactly reaches it");
    }

    #[test]
    fn a_pose_reverting_before_the_ramp_settles_reverses_direction_smoothly() {
        // Dive then immediately surface: the smoother must not have "memorised"
        // a direction — it eases toward whatever the current target is.
        let mut s = EyeHeightSmoother::new(1.62);
        s.tick(0.4);
        let mid = s.lerp(1.0);
        assert!(mid < 1.62, "eased toward swimming");
        s.tick(1.62);
        assert!(s.lerp(1.0) > mid, "eases back up, not stuck low");
    }

    // --- ViewBob: the walk bob's phase, amplitude and hurt tilt. ------------

    /// Vanilla's own numbers, computed by hand from the source rather than from
    /// this implementation: a player walking at `0.13` blocks/tick (a little
    /// over the `0.1` ceiling) on the ground.
    #[test]
    fn the_amplitude_eases_by_four_tenths_toward_a_ceiling_of_a_tenth() {
        let mut b = ViewBob::new();
        // `updateBob`: target = min(0.1, 0.13) = 0.1, so bob += (0.1 - 0) * 0.4.
        b.tick(0.13, 0.13, true, false, false);
        assert!(
            (b.frame(1.0).bob - 0.04).abs() < 1e-6,
            "one tick reaches 0.04, got {}",
            b.frame(1.0).bob
        );
        assert!(
            (b.frame(0.0).bob - 0.0).abs() < 1e-6,
            "partial 0 still reads the pre-tick amplitude"
        );
        // Twenty ticks: converged, and *never* past the ceiling however fast the
        // player runs. This is the reason sprinting speeds the bob up rather than
        // making it wilder.
        for _ in 0..20 {
            b.tick(0.5, 0.5, true, false, false);
        }
        let settled = b.frame(1.0).bob;
        assert!(
            (settled - 0.1).abs() < 1e-4,
            "converges to the 0.1 ceiling, got {settled}"
        );
        assert!(settled <= 0.1 + 1e-6, "must never exceed it: {settled}");
    }

    #[test]
    fn leaving_the_ground_fades_the_bob_out_rather_than_cutting_it() {
        let mut b = ViewBob::new();
        for _ in 0..20 {
            b.tick(0.13, 0.13, true, false, false);
        }
        let walking = b.frame(1.0).bob;
        assert!(walking > 0.09);
        // Airborne: the target is a flat 0.0, so the same 0.4 ease runs the other
        // way. A *cut* would read 0.0 on the very first airborne tick.
        b.tick(0.13, 0.13, false, false, false);
        let first_air = b.frame(1.0).bob;
        assert!(
            first_air < walking && first_air > 0.0,
            "eases toward zero rather than snapping: {walking} -> {first_air}"
        );
        // Swimming and death use the same flat-zero target.
        let mut swim = ViewBob::new();
        for _ in 0..20 {
            swim.tick(0.13, 0.13, true, false, false);
        }
        swim.tick(0.13, 0.13, true, false, true);
        assert!(swim.frame(1.0).bob < walking, "swimming suppresses it too");
        let mut dead = ViewBob::new();
        for _ in 0..20 {
            dead.tick(0.13, 0.13, true, false, false);
        }
        dead.tick(0.13, 0.13, true, true, false);
        assert!(dead.frame(1.0).bob < walking, "and so does being dead");
    }

    #[test]
    fn the_phase_accumulates_six_tenths_of_the_distance_actually_moved() {
        let mut b = ViewBob::new();
        b.tick(1.0, 0.13, true, false, false);
        // `addWalkedDistance(length * 0.6)`, then negated by
        // `getBackwardsInterpolatedWalkDistance`.
        assert!(
            (b.frame(1.0).walk_phase - -(0.6 + 0.6)).abs() < 1e-6,
            "partial 1 extrapolates a further (walkDist - walkDistO): got {}",
            b.frame(1.0).walk_phase
        );
        assert!(
            (b.frame(0.0).walk_phase - -0.6).abs() < 1e-6,
            "partial 0 is the bare current value"
        );
        // Walking into a wall moves nothing, so the phase must not advance —
        // it is the distance *moved*, not the distance asked for.
        let before = b.frame(0.0).walk_phase;
        b.tick(0.0, 0.13, true, false, false);
        assert!(
            (b.frame(0.0).walk_phase - before).abs() < 1e-6,
            "a blocked step must not advance the stride"
        );
    }

    #[test]
    fn the_dip_is_rectified_but_the_sway_is_not() {
        // Two half-strides apart: the sway must reverse sign, the dip must not.
        // This is the `Math.abs` on the Y term, and the defect it guards against
        // (dropping the abs) halves the apparent cadence rather than changing the
        // amplitude — invisible in a still frame.
        let a = BobFrame {
            walk_phase: -0.5,
            bob: 0.1,
            ..BobFrame::default()
        };
        let b = BobFrame {
            walk_phase: -1.5,
            bob: 0.1,
            ..BobFrame::default()
        };
        assert!(a.view_translation().x * b.view_translation().x < 0.0, "sway alternates");
        assert!(a.view_translation().y <= 0.0 && b.view_translation().y <= 0.0);
        // And the dip's peak is a full `bob`, at whole-number phase.
        let peak = BobFrame {
            walk_phase: 0.0,
            bob: 0.1,
            ..BobFrame::default()
        };
        assert!((peak.view_translation().y - -0.1).abs() < 1e-6);
        assert!(peak.view_translation().x.abs() < 1e-6, "no sway at the dip's bottom");
    }

    #[test]
    fn the_nods_phase_offset_is_zero_point_two_radians_not_zero_point_two_pi() {
        // `Mth.cos(bd * (float) Math.PI - 0.2F)`. Getting this wrong is a 36°
        // phase error that still looks like a nod, so it is pinned against the
        // hand-evaluated value rather than against a restatement of the code.
        let f = BobFrame {
            walk_phase: 0.0,
            bob: 0.1,
            ..BobFrame::default()
        };
        // cos(-0.2) = 0.980067; * 0.1 * 5.0 = 0.4900335 degrees.
        assert!(
            (f.view_nod_degrees() - 0.490_033_5).abs() < 1e-5,
            "got {}",
            f.view_nod_degrees()
        );
        // The wrong reading, `(bd - 0.2) * PI`, would give cos(-0.2 * PI) =
        // 0.809017 -> 0.4045 degrees. Materially different, and this asserts we
        // are not there.
        assert!(
            (f.view_nod_degrees() - 0.404_508_5).abs() > 1e-3,
            "control: the (bd - 0.2) * PI misreading must not satisfy the above"
        );
    }

    #[test]
    fn the_hurt_tilt_is_a_quartic_spike_and_is_silent_once_the_countdown_lapses() {
        let mut b = ViewBob::new();
        b.hurt(0.0);
        // At the instant of the hit `hurt == 10 - 0 == 10`, so t == 1 and
        // sin(1 * PI) == 0: vanilla's tilt *starts* at zero, spikes, and returns.
        let at_hit = b.frame(0.0).hurt_roll_degrees(1.0);
        assert!(at_hit.abs() < 1e-4, "starts at zero, got {at_hit}");
        // Walk the countdown down and find the peak.
        let mut peak: f32 = 0.0;
        let mut peak_tick = 0;
        for tick in 0..=10 {
            let m = b.frame(0.0).hurt_roll_degrees(1.0).abs();
            if m > peak {
                peak = m;
                peak_tick = tick;
            }
            b.tick(0.0, 0.0, true, false, false);
        }
        assert!(
            (peak - 14.0).abs() < 1.5,
            "the peak is near the full 14 degrees, got {peak}"
        );
        // A quartic spike peaks *late* in the countdown (i.e. soon after the
        // hit), not at the midpoint a plain sine would give. t = 0.7 -> t^4 =
        // 0.24, so the peak sits around hurtTime 8.
        assert!(
            peak_tick <= 3,
            "sin(t^4 PI) spikes early in the window; a plain sin(t PI) would peak \
             at tick 5. got tick {peak_tick}"
        );
        // Lapsed: `hurt` goes negative and `bobHurt` returns before touching the
        // pose at all.
        for _ in 0..5 {
            b.tick(0.0, 0.0, true, false, false);
        }
        assert!(b.frame(0.5).hurt < 0.0, "the countdown has gone negative");
        assert_eq!(b.frame(0.5).hurt_roll_degrees(1.0), 0.0);
        // The accessibility option scales it, and zero disables it.
        let mut c = ViewBob::new();
        c.hurt(0.0);
        c.tick(0.0, 0.0, true, false, false);
        c.tick(0.0, 0.0, true, false, false);
        let full = c.frame(0.0).hurt_roll_degrees(1.0);
        assert!(full.abs() > 1.0, "precondition: there is a tilt to scale");
        assert!((c.frame(0.0).hurt_roll_degrees(0.5) - full * 0.5).abs() < 1e-5);
        assert_eq!(c.frame(0.0).hurt_roll_degrees(0.0), 0.0);
    }

    // --- The fold into Camera, and what it drops. --------------------------

    fn walking_camera() -> Camera {
        Camera {
            position: Vec3::new(8.5, 65.62, -12.25),
            yaw: 37.0,
            pitch: -8.0,
            ..Camera::default()
        }
    }

    #[test]
    fn yaw_pitch_round_trips_through_cameras_own_forward() {
        // `yaw_pitch_from_forward` must be the exact inverse of
        // `Camera::forward`, because that is the only thing making the
        // decomposition in `bobbed_camera` derivation-free. Checked against
        // `Camera::forward` itself, never against a restated convention.
        for yaw in [-179.0f32, -90.0, -1.0, 0.0, 37.0, 90.0, 178.0] {
            for pitch in [-89.0f32, -45.0, -8.0, 0.0, 8.0, 45.0, 89.0] {
                let cam = Camera { yaw, pitch, ..Camera::default() };
                let (y, p) = yaw_pitch_from_forward(cam.forward());
                assert!((p - pitch).abs() < 1e-3, "pitch {pitch} -> {p}");
                assert!((y - yaw).abs() < 1e-3, "yaw {yaw} -> {y}");
            }
        }
    }

    /// The load-bearing one: the folded camera must agree with vanilla's literal
    /// `P · B · V` for every term `Camera` can express.
    ///
    /// This is what stops a sign from being *asserted*. `BobFrame::eye_transform`
    /// is a direct transcription of `GameRenderer.bobView`, and the fold is a
    /// mechanical decomposition of it — so if either the transcription or the
    /// decomposition has a polarity backwards, the two disagree here rather than
    /// both looking plausible in a screenshot.
    #[test]
    fn the_folded_camera_reproduces_vanillas_own_bob_matrix() {
        let cam = walking_camera();
        // Roll set to zero by construction (`walk_phase` at the dip's bottom and
        // no hurt), which is the regime the fold is exact in. The non-zero-roll
        // case is measured by the next test rather than glossed.
        let frame = BobFrame {
            walk_phase: 0.0,
            bob: 0.1,
            hurt: -1.0,
            hurt_dir_degrees: 0.0,
        };
        assert!(
            frame.view_roll_degrees().abs() < 1e-6,
            "precondition: this frame has no roll to lose"
        );
        assert!(
            frame.view_nod_degrees() > 0.1,
            "precondition: but it does have a nod, or this proves nothing"
        );
        assert!(
            frame.view_translation().length() > 0.05,
            "precondition: and a translation"
        );

        let reference = cam.projection_matrix() * frame.eye_transform(1.0) * cam.view_matrix();
        let folded = bobbed_camera(cam, frame, 1.0).view_projection();

        // Compared by where world points *land in clip space*, not element by
        // element: two matrices differing by a harmless scale would pass an
        // element comparison in one direction and fail it in the other, and it
        // is the projected position that reaches a pixel.
        for p in [
            Vec3::new(8.5, 65.0, 0.0),
            Vec3::new(20.0, 70.0, 30.0),
            Vec3::new(-4.0, 60.0, -40.0),
            Vec3::new(8.5, 100.0, -12.0),
        ] {
            let a = reference * p.extend(1.0);
            let b = folded * p.extend(1.0);
            let (a, b) = (a.truncate() / a.w, b.truncate() / b.w);
            assert!(
                (a - b).length() < 1e-4,
                "world {p} lands at {a} under vanilla's matrix and {b} under the \
                 folded camera"
            );
        }

        // -- negative control --------------------------------------------
        // The comparison is sensitive: the *unbobbed* camera does not satisfy it.
        let unbobbed = cam.view_projection();
        let p = Vec3::new(20.0, 70.0, 30.0);
        let a = reference * p.extend(1.0);
        let c = unbobbed * p.extend(1.0);
        let (a, c) = (a.truncate() / a.w, c.truncate() / c.w);
        assert!(
            (a - c).length() > 1e-3,
            "control failed: the bob is too small for this comparison to see, so \
             agreement above would prove nothing. delta {}",
            (a - c).length()
        );
    }

    /// The divergence, measured rather than described.
    ///
    /// `Camera` cannot carry roll, so a frame with roll in it *must* disagree
    /// with vanilla — and the point of this test is to pin **how much**, so the
    /// cost of the missing field is a number in the record rather than a shrug.
    #[test]
    fn the_dropped_roll_is_the_only_disagreement_and_it_is_small_for_the_walk_bob() {
        let cam = walking_camera();
        // Peak sway: `sin(phase * PI) == 1`, so the roll is at its maximum
        // `bob * 3.0` degrees = 0.3 degrees at the 0.1 amplitude ceiling.
        let frame = BobFrame {
            walk_phase: -0.5,
            bob: 0.1,
            hurt: -1.0,
            hurt_dir_degrees: 0.0,
        };
        assert!(
            (frame.view_roll_degrees().abs() - 0.3).abs() < 1e-4,
            "precondition: this is the worst case, {} degrees",
            frame.view_roll_degrees()
        );

        let reference = cam.projection_matrix() * frame.eye_transform(1.0) * cam.view_matrix();
        let folded = bobbed_camera(cam, frame, 1.0).view_projection();

        // Screen-space residual on a 1920x1080 view, in pixels — the honest
        // unit, since a roll's error grows with distance from the screen centre
        // and an NDC number hides that.
        //
        // **Only samples that actually land on screen count.** The first version
        // of this test used four hand-picked world points and reported 27.9 px,
        // which looked like a real cost and was not: a 0.3 degree roll can only
        // displace a point by `radius * tan(0.3 deg)`, so 27.9 px implies a
        // radius of ~5300 px — i.e. the sample was far outside the frustum,
        // where "pixels" is a meaningless unit. Points are generated along the
        // camera's own view direction here and filtered by `|ndc| <= 1`, so what
        // is measured is what a player could see.
        let mut worst_px = 0.0f32;
        let mut on_screen = 0;
        let f = cam.forward();
        let right = f.cross(Vec3::Y).normalize();
        let up = right.cross(f).normalize();
        for dist in [4.0f32, 12.0, 40.0] {
            for (sx, sy) in [(0.0f32, 0.0f32), (0.5, 0.3), (-0.6, -0.4), (0.9, 0.9)] {
                // A spread proportional to distance keeps the sample inside the
                // frustum at every depth rather than only the nearest.
                let p = cam.position + f * dist + right * (sx * dist * 0.6) + up * (sy * dist * 0.35);
                let a = reference * p.extend(1.0);
                let b = folded * p.extend(1.0);
                if a.w <= 0.0 {
                    continue;
                }
                let (a, b) = (a.truncate() / a.w, b.truncate() / b.w);
                if a.x.abs() > 1.0 || a.y.abs() > 1.0 {
                    continue;
                }
                on_screen += 1;
                let px = ((a.x - b.x) * 960.0).hypot((a.y - b.y) * 540.0);
                worst_px = worst_px.max(px);
            }
        }
        assert!(
            on_screen >= 6,
            "precondition: this measures nothing if the samples are off screen \
             ({on_screen} landed on screen)"
        );
        println!(
            "dropped-roll residual at 1920x1080: {worst_px:.2} px worst of \
             {on_screen} on-screen samples"
        );
        // `radius * tan(0.3 deg)` at the far corner of a 1920x1080 frame is
        // `hypot(960, 540) * 0.00524 = 5.8 px`, so anything under ~6 is the
        // geometry working out and anything much over it means the fold is
        // losing more than the roll.
        assert!(
            worst_px < 6.5,
            "a 0.3 degree roll cannot displace an on-screen point by more than \
             ~5.8 px; {worst_px:.2} means the fold is dropping something else too"
        );
        // And it is genuinely non-zero — this test would be vacuous if the roll
        // were somehow being carried after all, in which case the divergence
        // note in `bobbed_camera`'s docs would be the stale thing.
        assert!(
            worst_px > 0.05,
            "control failed: no residual at all, so `Camera` is carrying roll and \
             the recorded divergence is wrong"
        );
    }

    #[test]
    fn a_still_player_gets_no_bob_at_all() {
        // The precondition every bob gate depends on, and the one that makes
        // "the frame changed" a meaningful assertion elsewhere: standing still
        // must be bit-identical to no bob.
        let cam = walking_camera();
        let mut b = ViewBob::new();
        for _ in 0..40 {
            b.tick(0.0, 0.0, true, false, false);
        }
        let frame = b.frame(0.5);
        assert_eq!(frame.bob, 0.0, "amplitude decayed to exactly zero");
        assert_eq!(frame.walk_phase, 0.0, "and the phase never advanced");
        assert_eq!(bobbed_camera(cam, frame, 1.0).position, cam.position);
        assert_eq!(bobbed_camera(cam, frame, 1.0).pitch, cam.pitch);
    }

    /// The magnitude check, at the exact camera and world point
    /// `tests/view_bob_pixels.rs` renders — and the one assertion that can tell a
    /// **present** nod from a missing one.
    ///
    /// This exists because the pixel gate's metric could not. That gate measures
    /// the chest's pixel *bounding box*, whose centre carries a systematic bias
    /// against the projected centroid: under a camera pitch change the near and
    /// far faces of a 3-D box move by different amounts, so the silhouette's
    /// extremes do not shift like its centre. Measured, it reports `+8.50 px`
    /// where the centroid moves `+6.54`, which is close enough to the
    /// `+8.31` a **nod-free** bob would give that the pixel tolerance cannot
    /// separate them — the *magnitude* species `CLAUDE.md` names, found by
    /// predicting the number and then looking at it rather than by reading the
    /// test.
    ///
    /// So the discriminating assertion lives here, on the projected point, where
    /// it can be exact.
    #[test]
    fn the_nod_reaches_the_projection_and_is_worth_one_point_eight_pixels() {
        let cam = Camera {
            position: Vec3::new(0.5, 0.45, 2.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_y_degrees: 60.0,
            aspect: 320.0 / 240.0,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };
        // The dip's bottom at the 0.1 amplitude ceiling: translate `(0, -0.1, 0)`,
        // no roll, nod `0.4900335` degrees.
        let dip = BobFrame {
            walk_phase: 0.0,
            bob: 0.1,
            hurt: -1.0,
            hurt_dir_degrees: 0.0,
        };
        // The chest's centre in `view_bob_pixels.rs`: block [0,0,4], a chest model
        // 14/16 tall, so `(0.5, 0.4375, 4.5)` — 2.5 blocks in front of the eye.
        let p = Vec3::new(0.5, 0.4375, 4.5);
        // Screen y on a 240-tall viewport, growing downward.
        let screen_y = |m: glam::Mat4| {
            let c = m * p.extend(1.0);
            (0.5 - c.y / c.w * 0.5) * 240.0
        };

        let plain = screen_y(cam.view_projection());
        let vanilla = screen_y(cam.projection_matrix() * dip.eye_transform(1.0) * cam.view_matrix());
        let folded = screen_y(bobbed_camera(cam, dip, 0.0).view_projection());
        let translate_only = screen_y(
            cam.projection_matrix()
                * glam::Mat4::from_translation(dip.view_translation())
                * cam.view_matrix(),
        );

        // Hand-computed, from vanilla's constants and the pinhole geometry, with
        // the arithmetic written out so the expected value does not originate in
        // the code under test:
        //
        //   focal length in pixels  f = (240/2) / tan(30 deg) = 207.85
        //   translate term          f * (0.1 / 2.5)           = +8.31 px  (down)
        //   nod term                f * tan(0.4900335 deg)    = -1.78 px  (up)
        //   net                                               = +6.53 px
        assert!(
            (translate_only - plain - 8.31).abs() < 0.05,
            "the translate alone must move it +8.31 px, got {:+.3}",
            translate_only - plain
        );
        assert!(
            (vanilla - plain - 6.53).abs() < 0.05,
            "vanilla's own matrix must net +6.53 px, got {:+.3}",
            vanilla - plain
        );
        // The load-bearing one: the fold agrees with vanilla to well inside the
        // 1.78 px the nod is worth, so a dropped or inverted nod cannot pass.
        assert!(
            (folded - vanilla).abs() < 0.01,
            "the folded camera must reproduce vanilla exactly here: vanilla \
             {:+.3}, folded {:+.3}",
            vanilla - plain,
            folded - plain
        );
        // -- the two controls this discriminates ---------------------------
        assert!(
            (folded - translate_only).abs() > 1.5,
            "control failed: the nod is worth less than 1.5 px here, so agreement \
             above cannot distinguish a missing nod from a present one"
        );
        let inverted = plain + 8.31 + 1.78;
        assert!(
            (folded - inverted).abs() > 1.5,
            "control failed: an inverted nod would land at {inverted:.3} and the \
             fold is at {folded:.3}, which is not far enough apart to matter"
        );
        // And the fold really does carry it as pitch, in the direction that moves
        // the scene *up*. Read, not asserted as a polarity: the sign is whatever
        // agreement with vanilla above requires.
        let bobbed = bobbed_camera(cam, dip, 0.0);
        println!(
            "folded camera: pitch {:+.5} deg (was {:+.1}), position {:?}",
            bobbed.pitch, cam.pitch, bobbed.position
        );
        assert!(
            (bobbed.pitch - dip.view_nod_degrees()).abs() < 1e-3,
            "the nod lands on pitch as +{:.5} deg",
            dip.view_nod_degrees()
        );
    }

    #[test]
    fn eye_is_above_feet() {
        let state = PlayerState::at(Vec3d::new(1.0, 64.0, 2.0), 0.0);
        let cam = build_camera(&state, PLAYER_EYE_HEIGHT, 16.0 / 9.0, 8);
        assert!((cam.position.y - (64.0 + PLAYER_EYE_HEIGHT)).abs() < 1e-6);
        assert!((cam.position.x - 1.0).abs() < 1e-6);
    }

    #[test]
    fn far_scales_with_render_distance() {
        let state = PlayerState::at(Vec3d::ZERO, 0.0);
        let near = build_camera(&state, PLAYER_EYE_HEIGHT, 1.0, 4);
        let far = build_camera(&state, PLAYER_EYE_HEIGHT, 1.0, 16);
        assert!(
            far.far > near.far,
            "more render distance ⇒ farther far plane"
        );
    }

    #[test]
    fn degenerate_aspect_is_sanitised() {
        let state = PlayerState::at(Vec3d::ZERO, 0.0);
        let cam = build_camera(&state, PLAYER_EYE_HEIGHT, 0.0, 8);
        assert_eq!(cam.aspect, 1.0);
    }

    // --- apply_spyglass_fov: issue #154's FOV-zoom half. ---

    #[test]
    fn apply_spyglass_fov_is_a_tenth_while_scoping() {
        let state = PlayerState::at(Vec3d::ZERO, 0.0);
        let cam = build_camera(&state, PLAYER_EYE_HEIGHT, 16.0 / 9.0, 8);
        let zoomed = apply_spyglass_fov(cam, true);
        assert!(
            (zoomed.fov_y_degrees - cam.fov_y_degrees * 0.1).abs() < 1e-6,
            "AbstractClientPlayer.getFieldOfViewModifier returns 0.1F outright while scoping: \
             expected {}, got {}",
            cam.fov_y_degrees * 0.1,
            zoomed.fov_y_degrees
        );
    }

    #[test]
    fn apply_spyglass_fov_is_unchanged_while_not_scoping() {
        let state = PlayerState::at(Vec3d::ZERO, 0.0);
        let cam = build_camera(&state, PLAYER_EYE_HEIGHT, 16.0 / 9.0, 8);
        let unzoomed = apply_spyglass_fov(cam, false);
        assert_eq!(unzoomed.fov_y_degrees, cam.fov_y_degrees);
    }

    #[test]
    fn apply_spyglass_fov_composes_rather_than_overwrites() {
        // A camera whose fov_y_degrees already differs from the module
        // default (e.g. a future sprint-FOV or settings-driven value) must
        // still be scaled relative to *its own* value, not reset to some
        // absolute spyglass constant — this is the "composed with, not
        // overwriting" property `docs/screen-overlays.md` calls out.
        let state = PlayerState::at(Vec3d::ZERO, 0.0);
        let mut cam = build_camera(&state, PLAYER_EYE_HEIGHT, 16.0 / 9.0, 8);
        cam.fov_y_degrees = 90.0;
        let zoomed = apply_spyglass_fov(cam, true);
        assert!((zoomed.fov_y_degrees - 9.0).abs() < 1e-6);
    }

    /// A fixed set of full-block collision boxes, keyed by block position —
    /// enough surface to test the pullback marcher without any real world.
    struct FakeWorld(std::collections::HashSet<(i32, i32, i32)>);

    impl CollisionView for FakeWorld {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if self.0.contains(&(x, y, z)) {
                out.push(Aabb::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(z),
                    f64::from(x) + 1.0,
                    f64::from(y) + 1.0,
                    f64::from(z) + 1.0,
                ));
            }
        }
    }

    fn empty_world() -> FakeWorld {
        FakeWorld(std::collections::HashSet::new())
    }

    fn wall_at_x(x: i32) -> FakeWorld {
        let mut blocks = std::collections::HashSet::new();
        for y in -5..5 {
            for z in -5..5 {
                blocks.insert((x, y, z));
            }
        }
        FakeWorld(blocks)
    }

    #[test]
    fn pullback_reaches_the_desired_distance_with_nothing_in_the_way() {
        let world = empty_world();
        let d = collision_pullback(Vec3::new(0.5, 0.5, 0.5), Vec3::new(-1.0, 0.0, 0.0), 4.0, &world);
        assert_eq!(d, 4.0);
    }

    #[test]
    fn pullback_stops_at_a_real_wall() {
        // A full-block wall occupying x∈[-2,-1]: the ray from x=0.5 travelling
        // in -X enters its near face (x=-1) at t = 0.5 - (-1) = 1.5.
        let world = wall_at_x(-2);
        let d = collision_pullback(Vec3::new(0.5, 0.5, 0.5), Vec3::new(-1.0, 0.0, 0.0), 4.0, &world);
        assert!((d - 1.5).abs() < 1e-4, "expected ~1.5, got {d}");
    }

    #[test]
    fn pullback_never_exceeds_desired_even_past_a_distant_wall() {
        // The wall is farther than the desired pullback, so the desired
        // distance wins outright rather than clamping to the (irrelevant) hit.
        let world = wall_at_x(-20);
        let d = collision_pullback(Vec3::new(0.5, 0.5, 0.5), Vec3::new(-1.0, 0.0, 0.0), 4.0, &world);
        assert_eq!(d, 4.0);
    }

    #[test]
    fn a_partial_shape_only_clips_where_it_actually_is() {
        // A "slab" occupying only the lower half of the block two behind the
        // eye: a ray at the slab's own height clips on it, one at head height
        // sails over it to the full desired distance. This is exactly the
        // occlusion-vs-collision distinction `is_solid` gets wrong for a real
        // slab; it is not, on its own, evidence that `is_solid` was used here
        // (nothing in this module calls it) but it is the regression this
        // exact-AABB approach exists to keep correct.
        struct Slab;
        impl CollisionView for Slab {
            fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
                if (x, y, z) == (-2, 0, 0) {
                    out.push(Aabb::new(-2.0, 0.0, 0.0, -1.0, 0.5, 1.0));
                }
            }
        }
        let slab = Slab;
        let low = collision_pullback(Vec3::new(0.5, 0.25, 0.5), Vec3::new(-1.0, 0.0, 0.0), 4.0, &slab);
        assert!((low - 1.5).abs() < 1e-4, "low ray should clip the slab: {low}");
        let high = collision_pullback(Vec3::new(0.5, 0.75, 0.5), Vec3::new(-1.0, 0.0, 0.0), 4.0, &slab);
        assert_eq!(high, 4.0, "high ray passes over the slab entirely");
    }

    #[test]
    fn third_person_camera_is_the_identity_in_first_person() {
        let eye = Camera {
            position: Vec3::new(0.0, 0.0, 0.0),
            yaw: 0.0,
            ..Camera::default()
        };
        let world = empty_world();
        let cam = third_person_camera(eye, false, &world);
        assert_eq!(cam.position, eye.position);
    }

    #[test]
    fn third_person_camera_pulls_back_along_view_direction() {
        let eye = Camera {
            position: Vec3::new(0.0, 0.0, 0.0),
            yaw: 0.0, // forward = +Z
            pitch: 0.0,
            ..Camera::default()
        };
        let world = empty_world();
        let cam = third_person_camera(eye, true, &world);
        // Pulled *backward*, i.e. -Z, by the full desired distance with
        // nothing to collide against.
        assert!((cam.position.z - (-THIRD_PERSON_DISTANCE)).abs() < 1e-4);
        assert!(cam.position.x.abs() < 1e-6);
        assert_eq!(cam.yaw, eye.yaw, "orientation is unchanged by pullback");
    }

    #[test]
    fn third_person_camera_stops_short_of_a_wall_by_the_margin() {
        let eye = Camera {
            position: Vec3::new(0.0, 0.0, 0.0),
            yaw: 0.0, // forward = +Z, so "back" is -Z
            pitch: 0.0,
            ..Camera::default()
        };
        // A full-block wall occupying z∈[-2,-1]: its near face (z=-1) sits
        // exactly 1 block behind the eye at z=0, so the raw hit distance is
        // 1.0 before the margin is subtracted.
        let mut blocks = std::collections::HashSet::new();
        for x in -5..5 {
            for y in -5..5 {
                blocks.insert((x, y, -2));
            }
        }
        let world = FakeWorld(blocks);
        let cam = third_person_camera(eye, true, &world);
        assert!(
            (cam.position.z - -(1.0 - COLLISION_MARGIN)).abs() < 1e-4,
            "got {}",
            cam.position.z
        );
    }
}
