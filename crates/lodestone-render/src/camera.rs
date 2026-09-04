//! Camera and frustum culling using Minecraft's conventions.
//!
//! # Reconciliation against the real client source
//!
//! These conventions were reconciled against the decompiled client's own
//! camera and game-renderer sources (26.2, behavioural reference only). Each assumption
//! and how it held:
//!
//! * **Handedness / axes:** right-handed, `+X` east, `+Y` up, `+Z` south —
//!   **held.** Vanilla's eye-space base forward is the constant `(0, 0, -1)`,
//!   rotated into world space by
//!   `rotationYXZ(π - yaw, -pitch, 0)`. Expanding that gives world forward
//!   `(-cos(pitch)·sin(yaw), -sin(pitch), cos(pitch)·cos(yaw))`.
//! * **Yaw/pitch/forward:** **held, exactly.** The expansion above equals our
//!   `(-sin(yaw)cos(pitch), -sin(pitch), cos(yaw)cos(pitch))`: yaw `0` faces
//!   `+Z` (south), yaw `90` faces `-X` (west), positive pitch looks down.
//! * **Up/right are *derived*, not supplied:** **this was wrong and is now
//!   corrected.** Vanilla rotates `UP` and `LEFT` by the same
//!   `rotationYXZ(π − yaw, −pitch, 0)` quaternion it rotates `FORWARDS` by, so
//!   `up = (−sin y·sin p, cos p, cos y·sin p)` and `left = (cos y, 0, sin y)`.
//!   [`Camera::view_matrix`] used to hand `Vec3::Y` to a look-at instead, which
//!   is **degenerate at pitch ±90** — the flipped-camera bug. See
//!   `Camera::basis` for the whole mechanism; there is no singularity in
//!   vanilla's construction, and no pitch clamp tighter than `±90` is needed.
//! * **FOV is vertical, default 70:** **held.** Vanilla passes
//!   `options.fov` (default 70) straight into JOML `Matrix4f.perspective`, whose
//!   first argument is the *vertical* FOV in radians. Sprint/sneak/speed/death
//!   and under-water/-lava scale this value *before* projection (a multiplier on
//!   the degrees, clamped `0.1..=1.5`); those are gameplay effects layered on the
//!   base FOV and are the caller's job — see [`Camera::fov_y_degrees`].
//! * **Near plane 0.05:** **held** (vanilla's own near-plane constant is `0.05F`).
//! * **Far plane:** **this was under-specified and is now corrected.** Vanilla's
//!   `depthFar = max(renderDistance_chunks * 16 * 4, cloudRange_chunks * 16)`,
//!   i.e. **four times the render distance in blocks**, not a fixed 512. At
//!   RD 32 that is `2048`. Use [`Camera::far_for_render_distance`]. The `[0,1]`
//!   depth choice matches vanilla's `isZZeroToOne()` device path on Metal, and
//!   like vanilla the range is **reversed** — near maps to `1`, far to `0`.
//! * **Eye height / camera offset:** the camera position is the *eye*, which sits
//!   `entity.y + eyeHeight` above the feet, with standing
//!   `DEFAULT_EYE_HEIGHT = 1.62` (`Avatar`'s own decompiled source). This offset is load-bearing for
//!   raycast/block-targeting parity, so it is exposed explicitly as
//!   [`PLAYER_EYE_HEIGHT`] and [`Camera::with_eye_from_feet`] rather than left
//!   for the caller to remember.
//!
//! Projection targets `wgpu`'s `[0, 1]` clip-space depth (the DirectX/Metal
//! convention) — *not* the OpenGL `[-1, 1]` variant, which would place
//! everything at the wrong depth — with the range **reversed**, near to `1` and
//! far to `0`, exactly as vanilla does. [`Camera::projection_matrix`] carries
//! the measurement of why and the list of what it makes true elsewhere; the
//! short form is that a forward `[0,1]` buffer's coplanar separation collapses
//! as the *square* of the viewing distance and a reversed one's as the distance,
//! and every ported depth comparison and polygon offset in the tree is written
//! against the reversed sense.

use glam::{Mat4, Vec3, Vec4};

/// Standing player eye height above the feet position, in blocks
/// (vanilla's own default-eye-height constant). The camera eye is `feet.y + this`.
///
/// Exposed because block-targeting/raycasts must originate from the same eye the
/// camera renders from; a mismatch here is a gameplay bug, not just a visual one.
pub const PLAYER_EYE_HEIGHT: f32 = 1.62;

/// A perspective camera positioned and oriented in Minecraft's world space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// Eye position in world space.
    pub position: Vec3,
    /// Yaw in degrees (about `+Y`; `0` faces `+Z`).
    pub yaw: f32,
    /// Pitch in degrees (positive looks down).
    pub pitch: f32,
    /// Vertical field of view in degrees.
    pub fov_y_degrees: f32,
    /// Viewport aspect ratio (width / height).
    pub aspect: f32,
    /// Near clip distance.
    pub near: f32,
    /// Far clip distance.
    pub far: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            position: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            fov_y_degrees: 70.0,
            aspect: 16.0 / 9.0,
            near: 0.05,
            far: 512.0,
        }
    }
}

impl Camera {
    /// Build a camera whose eye sits at the player's standing eye height above
    /// the given feet position (`feet.y + `[`PLAYER_EYE_HEIGHT`]). Use this
    /// rather than setting `position` to the feet, so the render eye matches the
    /// raycast origin.
    #[must_use]
    pub fn with_eye_from_feet(mut self, feet: Vec3) -> Self {
        self.position = feet + Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0);
        self
    }

    /// Vanilla's far plane for a given render distance in chunks:
    /// `max(render_distance * 16 * 4, cloud_range * 16)`. Callers that don't
    /// model clouds can pass `cloud_range_chunks = 0`.
    ///
    /// At RD 32 this is `2048.0`.
    #[must_use]
    pub fn far_for_render_distance(render_distance_chunks: u32, cloud_range_chunks: u32) -> f32 {
        let rd_blocks = render_distance_chunks as f32 * 16.0;
        (rd_blocks * 4.0).max(cloud_range_chunks as f32 * 16.0)
    }

    /// The camera's orthonormal world-space basis, `(right, up, forward)`,
    /// derived from a **single YXZ Euler rotation** exactly the way vanilla's
    /// own rotation-setting function derives its own `forwards`/`up`/`left`
    /// (vanilla's decompiled camera source, 26.2):
    ///
    /// ```text
    /// this.rotation.rotationYXZ((float) Math.PI - yRot * (float) (Math.PI / 180.0),
    ///                           -xRot * (float) (Math.PI / 180.0), 0.0F);
    /// FORWARDS.rotate(this.rotation, this.forwards);   // FORWARDS = ( 0,  0, -1)
    /// UP.rotate(this.rotation, this.up);               // UP       = ( 0,  1,  0)
    /// LEFT.rotate(this.rotation, this.left);           // LEFT     = (-1,  0,  0)
    /// ```
    ///
    /// JOML's `Quaternionf.rotationYXZ(y, x, z)` is documented as
    /// `rotationY(y).rotateX(x).rotateZ(z)`, and JOML's `rotateX`/`rotateZ`
    /// right-multiply (local-frame), so the rotation matrix is `Ry · Rx · Rz`.
    /// With vanilla's arguments — `y = π − yaw`, `x = −pitch`, `z = 0` (**no
    /// roll**) — that is `R = Ry(π − yaw) · Rx(−pitch)`, whose expansion is:
    ///
    /// ```text
    /// forward = (−sin y · cos p,  −sin p,   cos y · cos p)
    /// up      = (−sin y · sin p,   cos p,   cos y · sin p)
    /// left    = ( cos y,           0,       sin y       )   → right = −left
    /// ```
    ///
    /// `cos(π − yaw) = −cos(yaw)` and `sin(π − yaw) = sin(yaw)` are what fold the
    /// `π` away, which is why no `π` survives below.
    ///
    /// # Why this is not a look-at
    ///
    /// This function is the fix for a long-standing bug: looking straight up or
    /// straight down flipped the camera. [`view_matrix`](Self::view_matrix) used
    /// to be `look_to_mat4(position, forward(), Vec3::Y)`, and a look-to derives
    /// `right = normalize(forward × up)` — which is **undefined** at pitch `±90`,
    /// where `forward` is `(0, ∓1, 0)`, exactly parallel to the hardcoded
    /// `Vec3::Y`. It failed in two modes, and the second is why it survived
    /// years of review:
    ///
    /// * with an *exactly* vertical forward, `forward × Vec3::Y` is the zero
    ///   vector and normalising it gives `NaN` — a blank frame;
    /// * with the forward f32 actually produces at pitch `90.0` it does **not**
    ///   go `NaN`, because `cos(90°)` rounds to `-4.371139e-8` rather than `0`.
    ///   The cross product is tiny but non-zero and normalises to **unit
    ///   length** — pointing the *opposite* way from the one it had at pitch
    ///   `89.95`. Measured (yaw 0): `right` goes `(-1, 0, 0) → (+1, 0, 0)` and
    ///   `up` goes `(0, 0.00087, 0.99999964) → (0, 4.4e-8, -1)` across that one
    ///   `0.05°` step. **Both** flip, so the result is a 180° roll about the
    ///   view axis, not a reflection: the basis stays finite, orthonormal,
    ///   right-handed and determinant `+1`, and the image simply turns upside
    ///   down. That is why every "is this matrix well-formed" check passes on
    ///   the broken code and **only a continuity sweep or a predicted basis
    ///   value can see it** — a gate sampling pitch `0`/`±45` sees nothing.
    ///
    /// Deriving `up` and `right` from the rotation removes the singularity rather
    /// than hiding it: there is nothing to normalise, so nothing to divide by
    /// zero. `right` has no pitch term at all (it is always horizontal), and at
    /// pitch `−90` `up` simply becomes horizontal too — precisely vanilla's
    /// behaviour, which renders looking perfectly straight down correctly. **Do
    /// not "fix" this by clamping pitch to `±89.9`**: that hides one symptom,
    /// diverges from vanilla, and leaves the `NaN` reachable by every other
    /// caller.
    ///
    /// Gated by `tests/camera_pitch_singularity.rs`, whose control runs the same
    /// assertions against the old construction and observes them fail.
    fn basis(&self) -> (Vec3, Vec3, Vec3) {
        let (sy, cy) = self.yaw.to_radians().sin_cos();
        let (sp, cp) = self.pitch.to_radians().sin_cos();
        let right = Vec3::new(-cy, 0.0, -sy);
        let up = Vec3::new(-sy * sp, cp, cy * sp);
        let forward = Vec3::new(-sy * cp, -sp, cy * cp);
        (right, up, forward)
    }

    /// The normalised view direction from yaw/pitch, per Minecraft's convention.
    ///
    /// Bit-identical to the closed form `(-sin y · cos p, -sin p, cos y · cos p)`
    /// it has always been — see `Camera::basis`, which is now the single
    /// source of that expression so the direction block-targeting raycasts from
    /// and the direction [`view_matrix`](Self::view_matrix) renders down cannot
    /// drift apart.
    #[must_use]
    pub fn forward(&self) -> Vec3 {
        self.basis().2
    }

    /// The right-handed view matrix (world → view).
    ///
    /// Built from `Camera::basis` rather than from a look-at, so it has no
    /// singularity at pitch `±90`; read that doc for the bug this shape exists to
    /// avoid. The layout is the standard right-handed one — the camera basis as
    /// the *rows* of the upper-left 3×3 block, in the order `right`, `up`,
    /// `-forward` (view space looks down `-Z`), with the translation column
    /// holding the basis-projected negated eye. That is element-for-element what
    /// `glam::camera::rh::view::look_to_mat4` produced for every non-singular
    /// pitch, which the gate pins directly.
    ///
    /// The determinant is `+1` (a rotation composed with a translation), so
    /// `sign(det(view_projection))` is decided entirely by the projection — this
    /// is the fact `entity.rs`'s `camera_orientation` (transpose instead of
    /// invert) and the GUI winding invariant in `CLAUDE.md` both rest on, and it
    /// is unchanged by this construction, at every pitch including `±90`.
    #[must_use]
    pub fn view_matrix(&self) -> Mat4 {
        let (right, up, forward) = self.basis();
        let eye = self.position;
        Mat4::from_cols(
            Vec4::new(right.x, up.x, -forward.x, 0.0),
            Vec4::new(right.y, up.y, -forward.y, 0.0),
            Vec4::new(right.z, up.z, -forward.z, 0.0),
            Vec4::new(-right.dot(eye), -up.dot(eye), forward.dot(eye), 1.0),
        )
    }

    /// The perspective projection matrix, targeting `wgpu`'s `[0,1]` depth with
    /// a **reversed** range: the near plane maps to `1.0` and the far plane to
    /// `0.0`.
    ///
    /// # Why reversed rather than glam's `directx::perspective`
    ///
    /// This is elementwise glam's `camera::rh::proj::directx::perspective` with
    /// `near` and `far` exchanged in the `z` row, and it is written out rather
    /// than called with swapped arguments so the exchange is visible.
    ///
    /// A forward `[0,1]` projection puts every window depth just under `1.0`,
    /// where `f32` spacing is a flat `2^-24` — so a fixed world-space clearance
    /// `c` at distance `d` buys `2^23 · near · c / d^2` representable values and
    /// **collapses as `d^2`**. Reversed, depth is `near · (far - d) / ((far -
    /// near) · d)`, which shrinks with distance and therefore rides down through
    /// the exponent: relative separation is `c · far / (d · (far - d))`, and a
    /// float carries a constant `2^23`-to-`2^24` window of ULPs per binade, so
    /// the separation degrades only as `1 / d` and is 100x-plus larger over
    /// everything terrain is drawn at. That is the whole reason vanilla's own
    /// depth constants are the size they are.
    ///
    /// # Consequences, all of which the tree spends
    ///
    /// * Nearer is **greater**, so vanilla's `GREATER_THAN_OR_EQUAL` is our
    ///   [`DEPTH_COMPARE_NEARER_OR_EQUAL`](crate::DEPTH_COMPARE_NEARER_OR_EQUAL)
    ///   with no sign flip at all, and a depth attachment clears to
    ///   [`DEPTH_CLEAR`](crate::DEPTH_CLEAR) = `0.0`.
    /// * A polygon offset that pulls toward the eye is **positive**.
    /// * The matrix determinant is positive, where the forward one's was
    ///   negative — mirroring the clip `z` axis flips the sign of a 4x4
    ///   determinant. Nothing about *screen* winding changes, because the
    ///   rasterizer decides facing from projected `x`/`y` alone; a gate that
    ///   asserts the 4x4 sign is asserting a polarity rather than measuring
    ///   winding.
    ///
    /// The far plane stays **finite**. An infinite-far reversed projection
    /// (`z_clip = near`, depth `near / d`) is the usual companion and would make
    /// the separation exactly `c / d` with no far term, but it also deletes the
    /// far clip plane, which [`Frustum`] extracts and section culling relies on.
    /// The finite form already carries 700-5900 ULP where the forward one
    /// carried 0-47, so the extra precision is not worth changing what gets
    /// culled.
    #[must_use]
    pub fn projection_matrix(&self) -> Mat4 {
        let (sin_fov, cos_fov) = (0.5 * self.fov_y_degrees.to_radians()).sin_cos();
        let h = cos_fov / sin_fov;
        let z_range_inv = 1.0 / (self.far - self.near);
        Mat4::from_cols(
            Vec4::new(h / self.aspect, 0.0, 0.0, 0.0),
            Vec4::new(0.0, h, 0.0, 0.0),
            Vec4::new(0.0, 0.0, self.near * z_range_inv, -1.0),
            Vec4::new(0.0, 0.0, self.near * self.far * z_range_inv, 0.0),
        )
    }

    /// The combined view-projection matrix (world → clip).
    #[must_use]
    pub fn view_projection(&self) -> Mat4 {
        self.projection_matrix() * self.view_matrix()
    }

    /// The view-projection matrix for a sky dome: rotation only, with the
    /// camera's translation stripped from the view matrix before combining
    /// with the projection.
    ///
    /// A sky dome (disc, sun/moon/star billboards, cloud plane) is built at a
    /// fixed radius around the origin and must appear infinitely distant —
    /// panning the camera must never visibly slide it, only turning may. Using
    /// [`view_projection`](Self::view_projection) unmodified would translate
    /// sky geometry by the camera's world position every frame, sliding the
    /// horizon out of alignment as soon as the player moves. Vanilla achieves
    /// the same thing by never touching its model-view translation when
    /// drawing the sky (`SkyRenderer`/`LevelRenderer` push only rotation onto
    /// the pose stack); this is the equivalent for a `view * projection`
    /// pipeline: zero the view matrix's translation column, keep its
    /// rotation/scale block, then project as usual.
    #[must_use]
    pub fn sky_view_projection(&self) -> Mat4 {
        let mut view = self.view_matrix();
        view.w_axis = Vec4::new(0.0, 0.0, 0.0, 1.0);
        self.projection_matrix() * view
    }

    /// The frustum for this camera, for culling.
    #[must_use]
    pub fn frustum(&self) -> Frustum {
        Frustum::from_view_projection(self.view_projection())
    }

    /// [`view_projection`](Self::view_projection), post-multiplied by
    /// [`nausea_portal_warp`] — the world-side half of the nausea and portal
    /// screen-space warp. See that function's doc for the transform itself
    /// and why it lives here rather than in `screen_effects.rs`: this is the
    /// **one** matrix every world-space uniform in `RenderState::render_inner`
    /// is rewritten from each frame (sections, the model shared camera
    /// buffer, the outline pass, debug lines — see that function's own `let
    /// view_proj = camera.view_projection()...` line), so injecting the warp
    /// here, at the single upstream source, reaches everything vanilla's own
    /// world-render entry point installs the projection matrix for (the
    /// whole world pass) and nothing it does not (the HUD/GUI and
    /// this crate's own screen-effect overlay pipeline read no camera matrix
    /// at all, so they are structurally unaffected) — without a second call
    /// site to keep in sync.
    #[must_use]
    pub fn view_projection_warped(&self, intensity: f32, angle_degrees: f32) -> Mat4 {
        self.view_projection_eye_space(nausea_portal_warp(intensity, angle_degrees))
    }

    /// [`view_projection`](Self::view_projection) with an arbitrary **eye-space**
    /// transform inserted between the projection and the view: `P · eye · V`.
    ///
    /// This is vanilla's projection-matrix multiply seam, and it is the *only*
    /// way a transform with three rotational degrees of freedom can reach this
    /// camera. [`Camera`] is parameterised by `position`/`yaw`/`pitch` — two
    /// angles — so anything folded into the fields themselves loses its roll
    /// component; that is not a fixable rounding error, it is the shape of the
    /// struct. Multiplying here costs nothing and loses nothing.
    ///
    /// # What "eye space" buys, and why no constant flips sign
    ///
    /// Our eye space matches vanilla's exactly (`+X` right, `+Y` up, forward
    /// `-Z`), so a transform transcribed from a transform-stack push in
    /// vanilla's world-render entry point composes here with **no** sign adjustment. The
    /// `[0,1]`-versus-reversed-Z depth difference lives entirely inside the
    /// projection matrix, which sits to the *left* of `eye` and therefore cannot
    /// reach it.
    ///
    /// # Two callers, composed in vanilla's own order
    ///
    /// Vanilla's world-render entry point multiplies in the view-bob transform
    /// **first** and applies the nausea/portal spin **after**, so the full product is
    /// `P · bob · warp · V` and a caller wanting both passes
    /// `bob * nausea_portal_warp(..)`. Reversing them puts the spin's skew on the
    /// unbobbed axis, which is a subtle wrongness rather than an obvious one.
    #[must_use]
    pub fn view_projection_eye_space(&self, eye: Mat4) -> Mat4 {
        (self.projection_matrix() * eye) * self.view_matrix()
    }
}

/// The nausea/portal "spinning" world-projection warp, the shared mechanism
/// behind both effects (vanilla's decompiled game-renderer source, 26.2):
///
/// ```text
/// if (intensity > 0.0F) {
///     skew = 5.0F / (intensity^2 + 5.0F) - intensity * 0.04F;
///     skew *= skew;
///     axis = (0, sqrt(2)/2, sqrt(2)/2);
///     angle = (spin_time + partial_ticks * spin_speed) * (pi/180);
///     proj.rotate(angle, axis);
///     proj.scale(1/skew, 1, 1);
///     proj.rotate(-angle, axis);
/// }
/// ```
///
/// Transcribed as `R(angle, axis) * S(1/skew, 1, 1) * R(-angle, axis)` — JOML's
/// `Matrix4f#rotate`/`#scale` right-multiply the receiver (`this = this *
/// arg`), the same convention glam's `*` uses, so the three chained calls
/// compose in the order written, matching this function's return expression
/// read left to right.
///
/// This crate has no persistent per-frame state (`RenderState::render_inner`
/// takes `&self`, and every "how far has this animation progressed" input
/// elsewhere in this pass — e.g. the fire overlay's `tick` — is threaded in
/// from outside rather than accumulated internally), so unlike vanilla's own
/// per-tick update, which integrates the accumulated spin time by the spin
/// speed every tick only while active, this takes the
/// *already-computed* `angle_degrees` as a pure argument — see
/// [`spinning_effect_angle_degrees`] for the (simplified) function this
/// codebase derives it with.
///
/// Returns the identity at `intensity <= 0.0`, matching vanilla's own
/// positive-intensity guard, so a caller can multiply this in
/// unconditionally rather than branching — the same "always safe to call"
/// shape [`Camera::view_projection_warped`] relies on.
#[must_use]
pub fn nausea_portal_warp(intensity: f32, angle_degrees: f32) -> Mat4 {
    if intensity <= 0.0 {
        return Mat4::IDENTITY;
    }
    let mut skew = 5.0 / (intensity * intensity + 5.0) - intensity * 0.04;
    skew *= skew;
    let axis = Vec3::new(0.0, std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2);
    let angle = angle_degrees.to_radians();
    Mat4::from_axis_angle(axis, angle)
        * Mat4::from_scale(Vec3::new(1.0 / skew, 1.0, 1.0))
        * Mat4::from_axis_angle(axis, -angle)
}

/// The spyglass zoom's field-of-view multiplier — see the module doc pointer
/// in `crate::screen_effects` for the vignette/letterbox half. Vanilla's own
/// field-of-view-modifier accessor
/// (vanilla's decompiled abstract-client-player source, 26.2):
///
/// ```text
/// } else if (firstPerson && this.isScoping()) {
///     return 0.1F;
/// }
/// ```
///
/// — an early `return`, so scoping **overrides** every other modifier in that
/// method (flying's `1.1`, the walk-speed ratio, the bow-draw ease-in), not a
/// factor composed with them. This function models only the scoping case
/// (flying/sprint/bow FOV are a different, unstarted issue) — `1.0` (no
/// change) when not scoping, vanilla's `0.1` (a 10x zoom-in: `getFov`
/// multiplies `options.fov` by this) when scoping.
///
/// Vanilla smooths this behind its own per-frame FOV-modifier field, a
/// `0.5`-per-frame lerp toward the target (`Camera`'s own decompiled source)
/// clamped to `0.1..=1.5`
/// (`Camera`'s own decompiled source) — this function returns the unsmoothed target only;
/// **not wired to a live `Camera.fov_y_degrees` anywhere in this codebase
/// yet**, since that value is assigned in `lodestone-shell/src/camera_rig.rs`
/// (`FOV_Y_DEGREES`/per-frame construction), a file outside this crate's
/// ownership — see `docs/screen-overlays.md`'s spyglass-FOV
/// section for the exact composition point.
#[must_use]
pub fn spyglass_fov_modifier(scoping: bool) -> f32 {
    if scoping { 0.1 } else { 1.0 }
}

/// The warp's accumulated spin angle, in degrees, as a **pure function of the
/// current game tick** rather than an integral this crate stores anywhere —
/// see [`nausea_portal_warp`]'s doc for why. Vanilla's own per-tick speed
/// calculation is
/// `(portal_intensity * 20 + nausea_intensity * 7) / (portal_intensity +
/// nausea_intensity)` while either is active, `0` otherwise; this treats that
/// blended speed as constant and multiplies it straight through by `tick`,
/// which is **not** the same number vanilla's own accumulator would hold
/// (vanilla only integrates while active and freezes, rather than resets,
/// the moment both intensities hit zero — this reaches the identical
/// steady-state rotation *rate* the instant either effect is active, just at
/// a different absolute phase, which is imperceptible for a continuously
/// looping spin with no fixed start reference). Returns `0.0` when neither
/// intensity is positive, matching vanilla's own zero-speed
/// branch (though the caller does not need this: [`nausea_portal_warp`]
/// already no-ops below `intensity <= 0.0` regardless of the angle passed).
#[must_use]
pub fn spinning_effect_angle_degrees(tick: u64, portal_intensity: f32, nausea_intensity: f32) -> f32 {
    let (p, n) = (portal_intensity.max(0.0), nausea_intensity.max(0.0));
    if p <= 0.0 && n <= 0.0 {
        return 0.0;
    }
    let speed = (p * 20.0 + n * 7.0) / (p + n);
    (tick as f32 * speed).rem_euclid(360.0)
}

/// A plane `normal · p + d = 0`, with `normal` unit length and the positive
/// half-space (`> 0`) being inside the frustum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    /// Unit normal pointing into the frustum.
    pub normal: Vec3,
    /// Plane offset.
    pub d: f32,
}

impl Plane {
    fn from_vec4(v: Vec4) -> Self {
        let normal = Vec3::new(v.x, v.y, v.z);
        let len = normal.length();
        // Guard against a degenerate row (shouldn't happen for a valid VP).
        if len > 0.0 {
            Plane {
                normal: normal / len,
                d: v.w / len,
            }
        } else {
            Plane {
                normal: Vec3::Z,
                d: v.w,
            }
        }
    }

    /// Signed distance from the plane to `p`; positive is inside.
    #[must_use]
    pub fn signed_distance(&self, p: Vec3) -> f32 {
        self.normal.dot(p) + self.d
    }
}

/// Result of testing an AABB against a frustum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intersection {
    /// The box is entirely inside the frustum.
    Inside,
    /// The box straddles at least one frustum plane.
    Intersecting,
    /// The box is entirely outside the frustum.
    Outside,
}

/// A view frustum as six inward-facing planes, extracted from a view-projection
/// matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frustum {
    /// Planes in order: left, right, bottom, top, near, far.
    pub planes: [Plane; 6],
}

impl Frustum {
    /// Extract the six frustum planes from a `[0,1]`-depth view-projection matrix
    /// (Gribb–Hartmann, DirectX/Metal depth convention).
    ///
    /// The clip-space region is `0 <= z <= w` under **either** depth direction,
    /// so the two half-space rows below are the same expressions a forward
    /// projection needs; what reversed-Z changes is which physical plane each
    /// one *is*. `z >= 0` is the plane depth `0.0` sits on, and under reversed-Z
    /// that is the **far** plane; `z <= w` is depth `1.0`, the **near** plane.
    /// The set of six planes is therefore identical and no culling decision
    /// moves — only the labels swap, and they are swapped here so the field's
    /// documented `[left, right, bottom, top, near, far]` order stays true.
    #[must_use]
    pub fn from_view_projection(m: Mat4) -> Self {
        // Rows of the column-major matrix.
        let r0 = Vec4::new(m.x_axis.x, m.y_axis.x, m.z_axis.x, m.w_axis.x);
        let r1 = Vec4::new(m.x_axis.y, m.y_axis.y, m.z_axis.y, m.w_axis.y);
        let r2 = Vec4::new(m.x_axis.z, m.y_axis.z, m.z_axis.z, m.w_axis.z);
        let r3 = Vec4::new(m.x_axis.w, m.y_axis.w, m.z_axis.w, m.w_axis.w);
        Frustum {
            planes: [
                Plane::from_vec4(r3 + r0), // left
                Plane::from_vec4(r3 - r0), // right
                Plane::from_vec4(r3 + r1), // bottom
                Plane::from_vec4(r3 - r1), // top
                Plane::from_vec4(r3 - r2), // near (z <= w, i.e. depth <= 1)
                Plane::from_vec4(r2),      // far  (z >= 0, i.e. depth >= 0)
            ],
        }
    }

    /// Classify an axis-aligned box `[min, max]` against the frustum.
    ///
    /// Uses the positive/negative-vertex test: for each plane the box corner
    /// furthest along the normal decides "fully outside", and the corner
    /// furthest against it decides "straddling". This is exact for the
    /// fully-outside case (no false "outside") and conservative for straddling,
    /// which is the correct bias for culling.
    #[must_use]
    pub fn test_aabb(&self, min: Vec3, max: Vec3) -> Intersection {
        let mut intersecting = false;
        for plane in &self.planes {
            let n = plane.normal;
            let p_vertex = Vec3::new(
                if n.x >= 0.0 { max.x } else { min.x },
                if n.y >= 0.0 { max.y } else { min.y },
                if n.z >= 0.0 { max.z } else { min.z },
            );
            let n_vertex = Vec3::new(
                if n.x >= 0.0 { min.x } else { max.x },
                if n.y >= 0.0 { min.y } else { max.y },
                if n.z >= 0.0 { min.z } else { max.z },
            );
            if plane.signed_distance(p_vertex) < 0.0 {
                return Intersection::Outside;
            }
            if plane.signed_distance(n_vertex) < 0.0 {
                intersecting = true;
            }
        }
        if intersecting {
            Intersection::Intersecting
        } else {
            Intersection::Inside
        }
    }

    /// Whether an AABB is at least partly visible.
    #[must_use]
    pub fn intersects_aabb(&self, min: Vec3, max: Vec3) -> bool {
        self.test_aabb(min, max) != Intersection::Outside
    }

    /// Convenience: whether the 16³ section at grid `coord` is visible.
    #[must_use]
    pub fn section_visible(&self, coord: (i32, i32, i32)) -> bool {
        let size = crate::section::SECTION_SIZE as f32;
        let min = Vec3::new(coord.0 as f32, coord.1 as f32, coord.2 as f32) * size;
        let max = min + Vec3::splat(size);
        self.intersects_aabb(min, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam_looking_south() -> Camera {
        Camera {
            position: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            aspect: 1.0,
            fov_y_degrees: 90.0,
            near: 0.1,
            far: 100.0,
        }
    }

    #[test]
    fn forward_matches_minecraft_convention() {
        let mut c = Camera {
            yaw: 0.0,
            pitch: 0.0,
            ..Default::default()
        };
        // Yaw 0 → +Z (south).
        assert!((c.forward() - Vec3::Z).length() < 1e-6);
        // Yaw 90 → -X (west).
        c.yaw = 90.0;
        assert!((c.forward() - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-6);
        // Pitch 90 → straight down.
        c.yaw = 0.0;
        c.pitch = 90.0;
        assert!((c.forward() - Vec3::new(0.0, -1.0, 0.0)).length() < 1e-6);
    }

    /// Reversed-Z: the near plane is depth `1`, the far plane depth `0`, and
    /// depth **decreases** monotonically with distance in between.
    ///
    /// The monotonicity arm is not decoration. Near maps to 1 and far to 0 under
    /// any projection that merely negates the forward one's `z` row, including
    /// several that are not a valid perspective transform at all, so the two
    /// endpoint checks alone do not distinguish reversed-Z from a sign slip.
    #[test]
    fn projection_maps_near_to_one_and_far_to_zero() {
        let c = cam_looking_south();
        let vp = c.view_projection();
        let depth_at = |distance: f32| {
            let clip = vp * (c.position + c.forward() * distance).extend(1.0);
            clip.z / clip.w
        };
        let near_z = depth_at(c.near);
        assert!((near_z - 1.0).abs() < 1e-3, "near maps to ~1, got {near_z}");
        let far_z = depth_at(c.far);
        assert!(far_z.abs() < 1e-3, "far maps to ~0, got {far_z}");
        // Strictly decreasing across the whole range, and inside `[0, 1]`.
        let mut previous = f32::INFINITY;
        for step in 0..64 {
            let d = c.near + (c.far - c.near) * (step as f32 / 63.0);
            let z = depth_at(d);
            assert!(
                (0.0..=1.0).contains(&z),
                "depth at {d} left [0, 1]: {z}"
            );
            assert!(
                z < previous,
                "depth must decrease with distance: {z} at {d} is not below {previous}"
            );
            previous = z;
        }
    }

    /// The projection is glam's forward DirectX RH perspective with `near` and
    /// `far` exchanged in the `z` row, and nothing else.
    ///
    /// Sourced outside [`Camera::projection_matrix`]: glam builds the reference,
    /// so a slip in the hand-written `x`/`y` terms (the aspect divide, the
    /// half-angle cotangent) fails here rather than showing up as a subtly wrong
    /// field of view nobody measures.
    #[test]
    fn the_projection_is_glams_forward_one_with_near_and_far_exchanged() {
        let c = Camera {
            fov_y_degrees: 70.0,
            aspect: 16.0 / 9.0,
            near: 0.05,
            far: 768.0,
            ..Camera::default()
        };
        let reference = glam::camera::rh::proj::directx::perspective(
            c.fov_y_degrees.to_radians(),
            c.aspect,
            c.far,
            c.near,
        );
        let ours = c.projection_matrix();
        for (i, (a, b)) in ours
            .to_cols_array()
            .iter()
            .zip(reference.to_cols_array().iter())
            .enumerate()
        {
            assert!(
                (a - b).abs() <= 1e-6 * b.abs().max(1.0),
                "element {i}: ours {a}, glam-with-exchanged-planes {b}"
            );
        }
        // And it is genuinely *not* the un-exchanged one, so the comparison
        // above is not satisfied by both orders at once.
        let forward = glam::camera::rh::proj::directx::perspective(
            c.fov_y_degrees.to_radians(),
            c.aspect,
            c.near,
            c.far,
        );
        assert_ne!(ours.to_cols_array(), forward.to_cols_array());
    }

    #[test]
    fn box_in_front_is_inside() {
        let f = cam_looking_south().frustum();
        // Small box 10 units ahead (+Z).
        assert_eq!(
            f.test_aabb(Vec3::new(-1.0, -1.0, 9.0), Vec3::new(1.0, 1.0, 11.0)),
            Intersection::Inside
        );
    }

    #[test]
    fn box_behind_is_outside() {
        let f = cam_looking_south().frustum();
        // Box behind the camera (−Z).
        assert_eq!(
            f.test_aabb(Vec3::new(-1.0, -1.0, -20.0), Vec3::new(1.0, 1.0, -10.0)),
            Intersection::Outside
        );
    }

    #[test]
    fn box_far_to_the_side_is_outside() {
        let f = cam_looking_south().frustum();
        // Way off to +X at moderate depth: outside the right plane.
        assert_eq!(
            f.test_aabb(Vec3::new(100.0, -1.0, 10.0), Vec3::new(120.0, 1.0, 11.0)),
            Intersection::Outside
        );
    }

    #[test]
    fn box_straddling_near_plane_intersects() {
        let f = cam_looking_south().frustum();
        // Spans from just behind to in front of the camera along Z.
        assert_eq!(
            f.test_aabb(Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, 5.0)),
            Intersection::Intersecting
        );
    }

    #[test]
    fn huge_box_enclosing_the_frustum_is_not_outside() {
        // The classic false-negative guard: a box far larger than the frustum,
        // centred on it, must never be reported Outside.
        let f = cam_looking_south().frustum();
        let r = 10_000.0;
        assert_ne!(
            f.test_aabb(Vec3::splat(-r), Vec3::splat(r)),
            Intersection::Outside
        );
    }

    #[test]
    fn section_visible_helper_agrees_with_aabb() {
        let f = cam_looking_south().frustum();
        // Section straight ahead at z∈[16,32] should be visible.
        assert!(f.section_visible((0, 0, 1)));
        // Section far behind should not.
        assert!(!f.section_visible((0, 0, -5)));
    }

    #[test]
    fn eye_from_feet_applies_player_eye_height() {
        let c = Camera::default().with_eye_from_feet(Vec3::new(8.0, 64.0, 8.0));
        assert!((c.position.y - (64.0 + PLAYER_EYE_HEIGHT)).abs() < 1e-6);
        assert_eq!(c.position.x, 8.0);
        assert_eq!(c.position.z, 8.0);
    }

    /// A point fixed relative to the *sky* (i.e. a fixed offset from the
    /// camera's own position, the way sky geometry is actually built each
    /// frame) must land at the same clip-space position regardless of where
    /// the camera has moved to in the world — that is the entire point of
    /// stripping translation. `view_projection` on the same relative point
    /// does *not* have this property, which is the defect this guards
    /// against.
    #[test]
    fn sky_view_projection_is_translation_invariant() {
        let base = Camera {
            position: Vec3::new(10.0, 70.0, -30.0),
            yaw: 35.0,
            pitch: -12.0,
            ..cam_looking_south()
        };
        let moved = Camera {
            position: Vec3::new(-400.0, 5.0, 900.0),
            ..base
        };
        // A "sky point" 100 blocks in front of the eye, exactly like a
        // billboard placed relative to the camera every frame.
        let offset = base.forward() * 100.0;
        let clip_base = base.sky_view_projection() * offset.extend(1.0);
        let clip_moved = moved.sky_view_projection() * offset.extend(1.0);
        assert!(
            (clip_base - clip_moved).length() < 1e-3,
            "sky_view_projection must not move sky geometry when the camera translates: \
             {clip_base:?} vs {clip_moved:?}"
        );

        // Negative control: the detector actually distinguishes something —
        // plain `view_projection` on the same relative point *does* move when
        // the camera translates, proving the assertion above is not vacuous.
        let world_point = base.position + offset;
        let ordinary_base = base.view_projection() * world_point.extend(1.0);
        let ordinary_moved = moved.view_projection() * world_point.extend(1.0);
        assert!(
            (ordinary_base - ordinary_moved).length() > 1.0,
            "control failed: view_projection should be translation-*sensitive*"
        );
    }

    #[test]
    fn far_plane_matches_vanilla_render_distance_formula() {
        // Vanilla: max(rd*16*4, cloud*16). RD32, no clouds → 2048.
        assert_eq!(Camera::far_for_render_distance(32, 0), 2048.0);
        // Cloud range can dominate at tiny render distances.
        assert_eq!(Camera::far_for_render_distance(2, 192), 3072.0);
    }

    // -- nausea/portal projection warp -----------------------

    #[test]
    fn nausea_portal_warp_is_identity_at_zero_or_negative_intensity() {
        assert_eq!(nausea_portal_warp(0.0, 45.0), Mat4::IDENTITY);
        assert_eq!(nausea_portal_warp(-1.0, 45.0), Mat4::IDENTITY);
    }

    #[test]
    fn nausea_portal_warp_at_max_intensity_matches_hand_computed_skew() {
        // vanilla's decompiled hud source / vanilla's decompiled game-renderer source, intensity = 1.0:
        // skew = 5/(1+5) - 0.04 = 0.793333...; skew *= skew = 0.629378...
        let intensity = 1.0_f32;
        let mut skew = 5.0 / (intensity * intensity + 5.0) - intensity * 0.04;
        skew *= skew;
        assert!((skew - 0.629_377_78).abs() < 1e-6, "hypothesis drifted: {skew}");

        // At angle 0, R(0)=I on both sides, so the warp reduces to a pure
        // scale by 1/skew on the rotated-axis-perpendicular component. Probe
        // it algebraically instead: the warp matrix's determinant must equal
        // the scale matrix's determinant (1/skew), since rotations have
        // determinant 1 and do not change it.
        let warp = nausea_portal_warp(intensity, 0.0);
        let expected_det = 1.0 / skew;
        assert!(
            (warp.determinant() - expected_det).abs() < 1e-4,
            "warp determinant {} must equal the scale-only determinant {expected_det} \
             (rotations are determinant-1 and must not change it)",
            warp.determinant()
        );
    }

    #[test]
    fn nausea_portal_warp_angle_zero_is_a_pure_x_axis_scale() {
        // At angle 0 both rotations are identity, so R(0)*S*R(0) = S exactly:
        // a plain scale of (1/skew, 1, 1) with no rotation component at all.
        let intensity = 0.5_f32;
        let mut skew = 5.0 / (intensity * intensity + 5.0) - intensity * 0.04;
        skew *= skew;
        let warp = nausea_portal_warp(intensity, 0.0);
        let expected = Mat4::from_scale(Vec3::new(1.0 / skew, 1.0, 1.0));
        assert!(
            (warp.to_cols_array().iter().zip(expected.to_cols_array()).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max)) < 1e-5,
            "warp at angle 0 must equal a pure scale(1/skew,1,1): {warp:?} vs {expected:?}"
        );
    }

    #[test]
    fn nausea_portal_warp_preserves_the_rotation_axis() {
        // R(angle,axis) * S(...) * R(-angle,axis) fixes any point exactly on
        // `axis` up to the scale term's effect on that same direction — since
        // axis = (0, sqrt2/2, sqrt2/2) is orthogonal to the scaled x-axis, a
        // vector along `axis` must be completely unaffected by the whole warp
        // (scale(1/skew,1,1) leaves y/z untouched, and axis has no x
        // component).
        let intensity = 0.7_f32;
        let axis = Vec3::new(0.0, std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2);
        let warp = nausea_portal_warp(intensity, 33.0);
        let transformed = warp.transform_vector3(axis);
        assert!(
            (transformed - axis).length() < 1e-4,
            "a vector along the warp's own rotation axis must be fixed: {transformed:?} vs {axis:?}"
        );
    }

    /// An identity eye transform is **bit-identical** to `view_projection`, not
    /// merely close.
    ///
    /// Load-bearing rather than pedantic: almost every frame has no damage tilt, so
    /// if this were only approximately equal then every gate downstream that
    /// asserts "nothing moved" would have acquired a tolerance it did not have
    /// before this seam existed.
    #[test]
    fn an_identity_eye_transform_leaves_view_projection_bit_identical() {
        let c = cam_looking_south();
        assert_eq!(
            c.view_projection().to_cols_array(),
            c.view_projection_eye_space(Mat4::IDENTITY).to_cols_array()
        );
    }

    /// A roll about eye-space `+Z` reaches clip space **with the right sign** — the
    /// thing a `Camera` built from `yaw`/`pitch` structurally cannot express.
    ///
    /// Both hypotheses computed. A rotation by `a` about `+Z` maps eye-space up to
    /// `(-sin a, cos a, 0)`, so at `a = -14°` (the sign `bobHurt`'s
    /// `-hurt * 14 * strength` produces) a point directly **above** the eye must
    /// project to positive clip `x`, and at `a = +14°` to negative. A gate that only
    /// checked "the matrix changed" would pass with the sign inverted, and an
    /// inverted damage tilt leans the camera *into* the hit instead of away from it.
    #[test]
    fn an_eye_space_roll_reaches_clip_space_with_the_signed_lean() {
        let c = cam_looking_south();
        // Straight up from the eye and pushed **along the direction this camera
        // actually looks**, which is world `+Z` (`yaw == 0` faces `+Z`), not eye
        // `-Z`. This test first used `(0, 1, -4)`, i.e. a point *behind* the eye:
        // the perspective divide by a negative `w` flips the clip `x` sign, so the
        // gate failed on a correct implementation and reported `-0.0605` where it
        // predicted a positive value. A sign gate must be fed a probe that is in
        // front of the camera or it measures the divide instead of the roll.
        let above = Vec3::new(0.0, 1.0, 4.0);
        let right_way = c
            .view_projection_eye_space(Mat4::from_rotation_z((-14.0_f32).to_radians()))
            .project_point3(above);
        let wrong_way = c
            .view_projection_eye_space(Mat4::from_rotation_z(14.0_f32.to_radians()))
            .project_point3(above);
        let unrolled = c.view_projection().project_point3(above);
        assert!(
            unrolled.x.abs() < 1e-6,
            "precondition: the probe is centred before the roll, got {}",
            unrolled.x
        );
        assert!(
            right_way.x > 0.05,
            "a negative roll must swing the point above the eye to positive clip x, \
             got {}",
            right_way.x
        );
        assert!(
            wrong_way.x < -0.05,
            "and the opposite sign must land on the other side, got {}",
            wrong_way.x
        );
    }

    #[test]
    fn view_projection_warped_matches_plain_view_projection_when_inactive() {
        let c = cam_looking_south();
        let plain = c.view_projection();
        let warped = c.view_projection_warped(0.0, 999.0);
        assert!(
            (plain.to_cols_array().iter().zip(warped.to_cols_array()).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max)) < 1e-6,
            "zero intensity must leave view_projection completely unmodified"
        );
    }

    #[test]
    fn view_projection_warped_differs_when_active() {
        let c = cam_looking_south();
        let plain = c.view_projection();
        let warped = c.view_projection_warped(0.8, 25.0);
        let max_diff = plain
            .to_cols_array()
            .iter()
            .zip(warped.to_cols_array())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_diff > 1e-3, "an active warp must actually change the matrix, max diff was {max_diff}");
    }

    #[test]
    fn spinning_effect_angle_is_zero_when_both_intensities_are_zero() {
        assert_eq!(spinning_effect_angle_degrees(1000, 0.0, 0.0), 0.0);
    }

    #[test]
    fn spinning_effect_angle_uses_vanillas_blended_speed() {
        // Portal-only: speed is exactly 20 deg/tick (vanilla's decompiled game-renderer source).
        assert!((spinning_effect_angle_degrees(1, 1.0, 0.0) - 20.0).abs() < 1e-4);
        // Nausea-only: speed is exactly 7 deg/tick.
        assert!((spinning_effect_angle_degrees(1, 0.0, 1.0) - 7.0).abs() < 1e-4);
        // Both at equal intensity: the average of the two speeds, 13.5.
        assert!((spinning_effect_angle_degrees(1, 1.0, 1.0) - 13.5).abs() < 1e-4);
    }

    #[test]
    fn spinning_effect_angle_wraps_into_0_360() {
        let angle = spinning_effect_angle_degrees(10_000, 1.0, 0.0);
        assert!((0.0..360.0).contains(&angle), "angle {angle} must be wrapped into [0, 360)");
    }

    // -- spyglass FOV modifier --------------------------------------

    #[test]
    fn spyglass_fov_modifier_is_a_tenth_while_scoping() {
        assert_eq!(spyglass_fov_modifier(true), 0.1);
    }

    #[test]
    fn spyglass_fov_modifier_is_unchanged_while_not_scoping() {
        assert_eq!(spyglass_fov_modifier(false), 1.0);
    }
}
