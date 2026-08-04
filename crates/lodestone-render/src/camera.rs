//! Camera and frustum culling using Minecraft's conventions.
//!
//! # Reconciliation against the real client source
//!
//! These conventions were reconciled against the decompiled client at
//! `.cache/mc/26.2/client-src/net/minecraft/client/Camera.java` and
//! `renderer/GameRenderer.java` (behavioural reference only). Each assumption
//! and how it held:
//!
//! * **Handedness / axes:** right-handed, `+X` east, `+Y` up, `+Z` south —
//!   **held.** Vanilla's eye-space base forward is `(0, 0, -1)`
//!   (`Camera.FORWARDS`), rotated into world space by
//!   `rotationYXZ(π - yaw, -pitch, 0)`. Expanding that gives world forward
//!   `(-cos(pitch)·sin(yaw), -sin(pitch), cos(pitch)·cos(yaw))`.
//! * **Yaw/pitch/forward:** **held, exactly.** The expansion above equals our
//!   `(-sin(yaw)cos(pitch), -sin(pitch), cos(yaw)cos(pitch))`: yaw `0` faces
//!   `+Z` (south), yaw `90` faces `-X` (west), positive pitch looks down.
//! * **FOV is vertical, default 70:** **held.** `GameRenderer`/`Camera` pass
//!   `options.fov` (default 70) straight into JOML `Matrix4f.perspective`, whose
//!   first argument is the *vertical* FOV in radians. Sprint/sneak/speed/death
//!   and under-water/-lava scale this value *before* projection (a multiplier on
//!   the degrees, clamped `0.1..=1.5`); those are gameplay effects layered on the
//!   base FOV and are the caller's job — see [`Camera::fov_y_degrees`].
//! * **Near plane 0.05:** **held** (`Camera.PROJECTION_Z_NEAR = 0.05F`).
//! * **Far plane:** **this was under-specified and is now corrected.** Vanilla's
//!   `depthFar = max(renderDistance_chunks * 16 * 4, cloudRange_chunks * 16)`,
//!   i.e. **four times the render distance in blocks**, not a fixed 512. At
//!   RD 32 that is `2048`. Use [`Camera::far_for_render_distance`]. The `[0,1]`
//!   depth choice matches vanilla's `isZZeroToOne()` device path on Metal.
//! * **Eye height / camera offset:** the camera position is the *eye*, which sits
//!   `entity.y + eyeHeight` above the feet, with standing
//!   `DEFAULT_EYE_HEIGHT = 1.62` (`Avatar.java`). This offset is load-bearing for
//!   raycast/block-targeting parity, so it is exposed explicitly as
//!   [`PLAYER_EYE_HEIGHT`] and [`Camera::with_eye_from_feet`] rather than left
//!   for the caller to remember.
//!
//! Projection targets `wgpu`'s `[0, 1]` clip-space depth (the DirectX/Metal
//! convention), via glam's `camera::rh::proj::directx::perspective` — *not* the
//! OpenGL `[-1, 1]` variant, which would place everything at the wrong depth.

use glam::{Mat4, Vec3, Vec4};

/// Standing player eye height above the feet position, in blocks
/// (`Avatar.DEFAULT_EYE_HEIGHT`). The camera eye is `feet.y + this`.
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

    /// The normalised view direction from yaw/pitch, per Minecraft's convention.
    #[must_use]
    pub fn forward(&self) -> Vec3 {
        let yaw = self.yaw.to_radians();
        let pitch = self.pitch.to_radians();
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        Vec3::new(-sy * cp, -sp, cy * cp)
    }

    /// The right-handed view matrix (world → view).
    #[must_use]
    pub fn view_matrix(&self) -> Mat4 {
        glam::camera::rh::view::look_to_mat4(self.position, self.forward(), Vec3::Y)
    }

    /// The perspective projection matrix, targeting `wgpu`'s `[0,1]` depth.
    #[must_use]
    pub fn projection_matrix(&self) -> Mat4 {
        glam::camera::rh::proj::directx::perspective(
            self.fov_y_degrees.to_radians(),
            self.aspect,
            self.near,
            self.far,
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
    /// [`nausea_portal_warp`] — the world-side half of issues #144 (nausea)
    /// and #149 (portal). See that function's doc for the transform itself
    /// and why it lives here rather than in `screen_effects.rs`: this is the
    /// **one** matrix every world-space uniform in `RenderState::render_inner`
    /// is rewritten from each frame (sections, the model shared camera
    /// buffer, the outline pass, debug lines — see that function's own `let
    /// view_proj = camera.view_projection()...` line), so injecting the warp
    /// here, at the single upstream source, reaches everything vanilla's own
    /// `RenderSystem.setProjectionMatrix` call in `GameRenderer.renderLevel`
    /// does (the whole world pass) and nothing it does not (the HUD/GUI and
    /// this crate's own screen-effect overlay pipeline read no camera matrix
    /// at all, so they are structurally unaffected) — without a second call
    /// site to keep in sync.
    #[must_use]
    pub fn view_projection_warped(&self, intensity: f32, angle_degrees: f32) -> Mat4 {
        (self.projection_matrix() * nausea_portal_warp(intensity, angle_degrees)) * self.view_matrix()
    }
}

/// The nausea/portal "spinning" world-projection warp, issues #144/#149's
/// shared mechanism (`GameRenderer.renderLevel`,
/// `.cache/mc/26.2/client-src/net/minecraft/client/renderer/GameRenderer.java:543-552`):
///
/// ```text
/// if (spinningEffectIntensity > 0.0F) {
///     skew = 5.0F / (spinningEffectIntensity^2 + 5.0F) - spinningEffectIntensity * 0.04F;
///     skew *= skew;
///     axis = (0, sqrt(2)/2, sqrt(2)/2);
///     angle = (spinningEffectTime + worldPartialTicks * spinningEffectSpeed) * (pi/180);
///     projectionMatrix.rotate(angle, axis);
///     projectionMatrix.scale(1/skew, 1, 1);
///     projectionMatrix.rotate(-angle, axis);
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
/// from outside rather than accumulated internally), so unlike vanilla's
/// `GameRenderer.tick`, which integrates `spinningEffectTime +=
/// spinningEffectSpeed` every tick only while active, this takes the
/// *already-computed* `angle_degrees` as a pure argument — see
/// [`spinning_effect_angle_degrees`] for the (simplified) function this
/// codebase derives it with.
///
/// Returns the identity at `intensity <= 0.0`, matching vanilla's own `if
/// (spinningEffectIntensity > 0.0F)` guard, so a caller can multiply this in
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

/// The spyglass zoom's field-of-view multiplier (issue #154's *other* half —
/// see the module doc pointer in `crate::screen_effects` for the vignette/
/// letterbox half). `AbstractClientPlayer.getFieldOfViewModifier`
/// (`.cache/mc/26.2/client-src/net/minecraft/client/player/AbstractClientPlayer.java:92-114`):
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
/// Vanilla smooths this behind `Camera.fovModifier`, a `0.5`-per-frame lerp
/// toward the target (`Camera.java:172`) clamped to `0.1..=1.5`
/// (`Camera.java:173`) — this function returns the unsmoothed target only;
/// **not wired to a live `Camera.fov_y_degrees` anywhere in this codebase
/// yet**, since that value is assigned in `lodestone-shell/src/camera_rig.rs`
/// (`FOV_Y_DEGREES`/per-frame construction), a file outside this crate and
/// outside this change's ownership — see `docs/screen-overlays.md`'s #154
/// section for the exact composition point.
#[must_use]
pub fn spyglass_fov_modifier(scoping: bool) -> f32 {
    if scoping { 0.1 } else { 1.0 }
}

/// The warp's accumulated spin angle, in degrees, as a **pure function of the
/// current game tick** rather than an integral this crate stores anywhere —
/// see [`nausea_portal_warp`]'s doc for why. Vanilla's own per-tick speed
/// (`GameRenderer.tick`, lines 261-270) is
/// `(portalIntensity * 20 + nauseaIntensity * 7) / (portalIntensity +
/// nauseaIntensity)` while either is active, `0` otherwise; this treats that
/// blended speed as constant and multiplies it straight through by `tick`,
/// which is **not** the same number vanilla's own accumulator would hold
/// (vanilla only integrates while active and freezes, rather than resets,
/// the moment both intensities hit zero — this reaches the identical
/// steady-state rotation *rate* the instant either effect is active, just at
/// a different absolute phase, which is imperceptible for a continuously
/// looping spin with no fixed start reference). Returns `0.0` when neither
/// intensity is positive, matching vanilla's own `spinningEffectSpeed = 0`
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
                Plane::from_vec4(r2),      // near (z >= 0 for [0,1] depth)
                Plane::from_vec4(r3 - r2), // far  (z <= w)
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

    #[test]
    fn projection_maps_near_and_far_to_zero_and_one() {
        let c = cam_looking_south();
        let vp = c.view_projection();
        // A point on the near plane straight ahead.
        let near_pt = c.position + c.forward() * c.near;
        let clip = vp * near_pt.extend(1.0);
        let ndc_z = clip.z / clip.w;
        assert!(ndc_z.abs() < 1e-3, "near maps to ~0, got {ndc_z}");
        // A point on the far plane straight ahead.
        let far_pt = c.position + c.forward() * c.far;
        let clip = vp * far_pt.extend(1.0);
        let ndc_z = clip.z / clip.w;
        assert!((ndc_z - 1.0).abs() < 1e-3, "far maps to ~1, got {ndc_z}");
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

    // -- nausea/portal projection warp (#144/#149) -----------------------

    #[test]
    fn nausea_portal_warp_is_identity_at_zero_or_negative_intensity() {
        assert_eq!(nausea_portal_warp(0.0, 45.0), Mat4::IDENTITY);
        assert_eq!(nausea_portal_warp(-1.0, 45.0), Mat4::IDENTITY);
    }

    #[test]
    fn nausea_portal_warp_at_max_intensity_matches_hand_computed_skew() {
        // Hud.java / GameRenderer.java:544-552, intensity = 1.0:
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
        // Portal-only: speed is exactly 20 deg/tick (GameRenderer.java:268).
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

    // -- spyglass FOV modifier (#154) --------------------------------------

    #[test]
    fn spyglass_fov_modifier_is_a_tenth_while_scoping() {
        assert_eq!(spyglass_fov_modifier(true), 0.1);
    }

    #[test]
    fn spyglass_fov_modifier_is_unchanged_while_not_scoping() {
        assert_eq!(spyglass_fov_modifier(false), 1.0);
    }
}
