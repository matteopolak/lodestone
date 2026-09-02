//! The pitch-±90 camera basis gate: looking straight up or straight down must
//! not flip the view.
//!
//! # The defect this pins
//!
//! [`Camera::view_matrix`] used to be built with a *look-to* construction:
//!
//! ```text
//! glam::camera::rh::view::look_to_mat4(position, forward(), Vec3::Y)
//! ```
//!
//! A look-to derives its right vector as `right = normalize(forward × up)`, and
//! [`Camera::forward`] at pitch `±90` is `(0, ∓1, 0)` — **parallel to the
//! `Vec3::Y`** that construction hardcodes. The cross product of two parallel
//! vectors is the zero vector, and normalising it is undefined: with an exactly
//! vertical forward the whole matrix is `NaN`
//! ([`legacy_look_to_is_nan_for_an_exactly_vertical_forward`]), and with the
//! *nearly* vertical forward f32 actually produces (`cos(90°)` rounds to
//! `-4.371139e-8`, not `0`) the right vector survives with **unit length and the
//! wrong sign** — the instant pitch reaches the clamp bound in `lodestone-ecs`'s
//! `player.rs` (`pitch.clamp(-90.0, 90.0)`).
//!
//! Measured at yaw 0 across the single `0.05°` step from `89.95` to `90.0`:
//!
//! | vector | pitch 89.95 | pitch 90.0 |
//! |---|---|---|
//! | `right` | `(-1, 0, 0)` | `(+1, 0, 0)` |
//! | `up` | `(0, 0.00087, 0.99999964)` | `(0, 4.4e-8, -1)` |
//! | `forward` | `(0, -0.99999964, 0.00087)` | `(0, -1, -4.4e-8)` |
//!
//! **Both** `right` and `up` flip while `forward` stays put, so the result is a
//! 180° **roll about the view axis** — the image turns upside down — and *not* a
//! reflection. That is the whole reason the bug survived: the basis remains
//! finite, unit length, orthogonal, right-handed and determinant `+1` at the
//! singularity, so every well-formedness check passes
//! ([`legacy_basis_at_the_singularity_is_rolled_180_degrees`] asserts exactly
//! that). **Only a continuity sweep or a predicted basis value can see it**,
//! which is what this gate does, and a gate sampling only pitch `0`/`±45` cannot
//! see it at all.
//!
//! # The fix this pins
//!
//! Vanilla does not use a look-at. `Camera.setRotation`
//! (`.cache/mc/26.2/client-src/net/minecraft/client/Camera.java:336-344`) builds
//! a YXZ Euler quaternion and **derives** all three basis vectors from it:
//!
//! ```text
//! this.rotation.rotationYXZ((float) Math.PI - yRot * (float) (Math.PI / 180.0),
//!                           -xRot * (float) (Math.PI / 180.0), 0.0F);
//! FORWARDS.rotate(this.rotation, this.forwards);   // FORWARDS = ( 0,  0, -1)
//! UP.rotate(this.rotation, this.up);               // UP       = ( 0,  1,  0)
//! LEFT.rotate(this.rotation, this.left);           // LEFT     = (-1,  0,  0)
//! ```
//!
//! There is no singularity in that: at pitch `-90` vanilla's `up` simply becomes
//! horizontal. [`basis_matches_the_vanilla_yxz_rotation`] re-derives the same
//! three vectors from glam's own `Mat3::from_rotation_y` / `from_rotation_x`
//! primitives — a *different construction path* for the same vanilla expression,
//! so the expected values do not originate in the closed form under test.

use glam::{Mat3, Mat4, Vec3, Vec4};
use lodestone_render::Camera;

/// Yaws to sample. Includes the axis-aligned ones (where a sign error hides in a
/// zero component) and off-axis ones (where it does not).
const YAWS: [f32; 8] = [0.0, 45.0, 90.0, 135.0, 180.0, 235.0, 270.0, -37.5];

fn cam(yaw: f32, pitch: f32) -> Camera {
    Camera {
        position: Vec3::new(12.5, 71.62, -304.25),
        yaw,
        pitch,
        fov_y_degrees: 70.0,
        aspect: 16.0 / 9.0,
        near: 0.05,
        far: 2048.0,
    }
}

/// The construction this gate exists to reject, kept verbatim so the control
/// tests below measure the real thing rather than a description of it.
fn legacy_view_matrix(c: &Camera) -> Mat4 {
    glam::camera::rh::view::look_to_mat4(c.position, c.forward(), Vec3::Y)
}

/// `(right, up, forward)` in world space, read out of a view matrix.
///
/// A view matrix's upper-left 3×3 block holds the camera basis **as rows**
/// (`right`, `up`, `-forward`); in glam's column-major `Mat4` that is one
/// component from each of the first three columns. This is the same extraction
/// `lodestone-shell`'s particle billboards do (`particles.rs`'s
/// `right`/`up` uniform fields), so the gate measures what production consumes
/// rather than a parallel accessor path.
fn basis_from_view(m: Mat4) -> (Vec3, Vec3, Vec3) {
    let right = Vec3::new(m.x_axis.x, m.y_axis.x, m.z_axis.x);
    let up = Vec3::new(m.x_axis.y, m.y_axis.y, m.z_axis.y);
    let back = Vec3::new(m.x_axis.z, m.y_axis.z, m.z_axis.z);
    (right, up, -back)
}

/// Every way a basis can be degenerate, as a `Result` so the control tests can
/// assert the detector *fires* rather than describing what it would do.
fn basis_health(m: Mat4) -> Result<(), String> {
    let (right, up, forward) = basis_from_view(m);
    for (name, v) in [("right", right), ("up", up), ("forward", forward)] {
        if !v.is_finite() {
            return Err(format!("{name} is not finite: {v:?}"));
        }
        let len = v.length();
        if (len - 1.0).abs() > 1e-4 {
            return Err(format!("{name} is not unit length: |{v:?}| = {len}"));
        }
    }
    for (a_name, a, b_name, b) in [
        ("right", right, "up", up),
        ("right", right, "forward", forward),
        ("up", up, "forward", forward),
    ] {
        let d = a.dot(b);
        if d.abs() > 1e-4 {
            return Err(format!("{a_name} · {b_name} = {d} (must be 0)"));
        }
    }
    // Right-handed: right × up must equal `back` (= -forward), not its negation.
    let handed = right.cross(up).dot(-forward);
    if handed < 0.999 {
        return Err(format!(
            "basis is left-handed or skewed: (right × up) · back = {handed} (must be +1)"
        ));
    }
    // A view matrix is a rotation composed with a translation, so its
    // determinant is exactly +1 — the property `entity.rs`'s winding tests and
    // `camera_orientation`'s transpose-instead-of-invert both depend on.
    let det = m.determinant();
    if (det - 1.0).abs() > 1e-3 {
        return Err(format!("det(view) = {det} (must be +1)"));
    }
    Ok(())
}

// -- non-degeneracy at the singular inputs -----------------------------------

#[test]
fn basis_is_healthy_at_the_pitch_singularity() {
    for pitch in [90.0_f32, -90.0] {
        for yaw in YAWS {
            let c = cam(yaw, pitch);
            if let Err(why) = basis_health(c.view_matrix()) {
                panic!("yaw {yaw}, pitch {pitch}: {why}");
            }
        }
    }
}

/// The control for the *finiteness* half specifically: the look-to construction
/// really is undefined for a forward parallel to its up vector. This is the mode
/// that produces `NaN` — a blank frame — and it is reachable by any caller that
/// hands the construction an exactly vertical direction.
#[test]
fn legacy_look_to_is_nan_for_an_exactly_vertical_forward() {
    let m = glam::camera::rh::view::look_to_mat4(Vec3::ZERO, Vec3::NEG_Y, Vec3::Y);
    assert!(
        !m.is_finite(),
        "control failed: look_to_mat4 was expected to be degenerate for a \
         forward parallel to its up vector, but it produced a finite matrix {m:?}"
    );
    assert!(
        basis_health(m).is_err(),
        "control failed: basis_health must reject a NaN matrix"
    );
}

// -- predicted basis values at pitch ±90 -------------------------------------

/// The vanilla construction expanded by hand, stated as *values* rather than as
/// a sign of change:
///
/// `R = Ry(π − yaw) · Rx(−pitch)` gives
/// `forward = (−sin y·cos p, −sin p,  cos y·cos p)`,
/// `up      = (−sin y·sin p,  cos p,  cos y·sin p)`,
/// `left    = ( cos y,        0,      sin y      )`, so `right = −left`.
///
/// At yaw `0`, pitch `+90` (facing south, looking straight down) that is
/// `forward = (0, −1, 0)`, `up = (0, 0, 1)` (screen-up is south, the way you
/// were facing) and `right = (−1, 0, 0)` (west, which is your right hand while
/// facing south). At pitch `−90` the `up` flips to `(0, 0, −1)` and `right` is
/// unchanged, because `right` has no pitch term at all.
///
/// The unfixed look-to gets `right` **backwards** here, which is the whole bug.
#[test]
fn basis_at_the_singularity_matches_the_hand_expanded_vanilla_values() {
    let cases: [(f32, f32, Vec3, Vec3, Vec3); 4] = [
        // yaw, pitch, right, up, forward
        (0.0, 90.0, Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, -1.0, 0.0)),
        (0.0, -90.0, Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, 1.0, 0.0)),
        (90.0, 90.0, Vec3::new(0.0, 0.0, -1.0), Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, -1.0, 0.0)),
        (180.0, -90.0, Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 1.0, 0.0)),
    ];
    for (yaw, pitch, want_right, want_up, want_forward) in cases {
        let (right, up, forward) = basis_from_view(cam(yaw, pitch).view_matrix());
        for (name, got, want) in [
            ("right", right, want_right),
            ("up", up, want_up),
            ("forward", forward, want_forward),
        ] {
            assert!(
                (got - want).length() < 1e-6,
                "yaw {yaw}, pitch {pitch}: {name} is {got:?}, vanilla's YXZ rotation predicts {want:?}"
            );
        }
    }
}

/// The control: the same predictions against the unfixed construction. The
/// failure is *not* a NaN and *not* a length — it is a **180° roll about the view
/// axis**, `right` and `up` both negated with `forward` untouched, which is why
/// every finiteness/orthonormality/handedness check passed on the broken code.
#[test]
fn legacy_basis_at_the_singularity_is_rolled_180_degrees() {
    let c = cam(0.0, 90.0);
    let legacy = legacy_view_matrix(&c);
    let (right, up, forward) = basis_from_view(legacy);
    let (want_right, want_up, want_forward) = basis_from_view(c.view_matrix());

    // Both flip: +X (east) where vanilla predicts -X (west), and up inverted.
    assert!(
        (right + want_right).length() < 1e-4,
        "control failed: the legacy look-to was expected to negate `right` at pitch 90 \
         ({want_right:?} → {right:?})"
    );
    assert!(
        (up + want_up).length() < 1e-4,
        "control failed: the legacy look-to was expected to negate `up` at pitch 90 \
         ({want_up:?} → {up:?})"
    );
    // ...but `forward` is untouched, so the difference is a pure roll, which no
    // amount of pitch clamping to ±89.9 would have removed from the construction.
    assert!(
        (forward - want_forward).length() < 1e-6,
        "control failed: `forward` must be identical under both constructions \
         ({want_forward:?} vs {forward:?}); a roll is the only difference"
    );
    assert!(
        basis_health(legacy).is_ok(),
        "control failed: the legacy basis at pitch 90 is expected to be finite, \
         orthonormal, right-handed and determinant +1 — that is precisely why a \
         health check alone cannot see this bug"
    );
}

// -- continuity through the singularity --------------------------------------

/// Sweep pitch across `±90` in `0.05°` steps. A `0.05°` step can rotate a basis
/// vector by at most `0.05°`, so `dot(previous, current) ≥ cos(0.05°) =
/// 0.99999962`; the threshold below leaves ~26× headroom while a sign flip
/// (`dot ≈ -1`) is rejected by a mile.
fn assert_pitch_sweep_is_continuous(view: impl Fn(&Camera) -> Mat4, label: &str) {
    for yaw in YAWS {
        for centre in [90.0_f32, -90.0] {
            let mut previous: Option<(f32, Vec3, Vec3, Vec3)> = None;
            for step in 0..=200 {
                let pitch = centre + (step as f32 - 100.0) * 0.05;
                let c = cam(yaw, pitch);
                let m = view(&c);
                if let Err(why) = basis_health(m) {
                    panic!("{label}: yaw {yaw}, pitch {pitch}: {why}");
                }
                let (right, up, forward) = basis_from_view(m);
                if let Some((prev_pitch, pr, pu, pf)) = previous {
                    for (name, prev, cur) in
                        [("right", pr, right), ("up", pu, up), ("forward", pf, forward)]
                    {
                        let d = prev.dot(cur);
                        assert!(
                            d > 0.9999,
                            "{label}: {name} is discontinuous between pitch {prev_pitch} and \
                             {pitch} at yaw {yaw}: {prev:?} → {cur:?}, dot = {d} \
                             (a 0.05 deg step must give dot >= 0.99999962; dot ~ -1 is a sign flip)"
                        );
                    }
                }
                previous = Some((pitch, right, up, forward));
            }
        }
    }
}

#[test]
fn basis_is_continuous_through_the_pitch_singularity() {
    assert_pitch_sweep_is_continuous(|c| c.view_matrix(), "view_matrix");
}

/// The control: the same sweep against the unfixed construction must fail. Run
/// as a `should_panic` so a green run *proves* the detector fires rather than
/// describing that it would.
#[test]
#[should_panic(expected = "is discontinuous between pitch")]
fn legacy_basis_flips_through_the_pitch_singularity() {
    assert_pitch_sweep_is_continuous(|c| legacy_view_matrix(c), "legacy look_to_mat4");
}

// -- agreement with the vanilla rotation, and with the old matrix elsewhere ---

/// The vanilla expression, rebuilt from glam's own rotation primitives rather
/// than from the closed form in `camera.rs`. `Mat3::from_rotation_y(a)` and
/// `from_rotation_x(a)` are the standard right-handed rotations, and JOML's
/// `Quaternionf.rotationYXZ(y, x, z)` is documented as
/// `rotationY(y).rotateX(x).rotateZ(z)` — a *local*-frame chain, i.e. the matrix
/// product `Ry · Rx · Rz`. With `z = 0` that is `Ry(π − yaw) · Rx(−pitch)`.
#[test]
fn basis_matches_the_vanilla_yxz_rotation() {
    for yaw in YAWS {
        for pitch in [-90.0_f32, -89.9, -45.0, -0.5, 0.0, 12.25, 45.0, 89.9, 90.0] {
            let rotation = Mat3::from_rotation_y(std::f32::consts::PI - yaw.to_radians())
                * Mat3::from_rotation_x(-pitch.to_radians());
            let want_forward = rotation * Vec3::new(0.0, 0.0, -1.0);
            let want_up = rotation * Vec3::new(0.0, 1.0, 0.0);
            let want_right = -(rotation * Vec3::new(-1.0, 0.0, 0.0));

            let (right, up, forward) = basis_from_view(cam(yaw, pitch).view_matrix());
            for (name, got, want) in [
                ("right", right, want_right),
                ("up", up, want_up),
                ("forward", forward, want_forward),
            ] {
                assert!(
                    (got - want).length() < 1e-5,
                    "yaw {yaw}, pitch {pitch}: {name} is {got:?}, vanilla's \
                     Ry(pi - yaw) * Rx(-pitch) gives {want:?}"
                );
            }
        }
    }
}

/// Away from the singularity the new construction must reproduce the old matrix
/// element for element — the fix is not licence to move the camera everywhere
/// else. `look_to_mat4` normalises `forward × Vec3::Y`, so agreement here also
/// re-confirms the derived `up`/`right` against an independent third path.
#[test]
fn view_matrix_agrees_with_the_legacy_construction_away_from_the_singularity() {
    for yaw in YAWS {
        for pitch in [-89.0_f32, -60.0, -30.0, -1.0, 0.0, 7.5, 30.0, 60.0, 89.0] {
            let c = cam(yaw, pitch);
            let new = c.view_matrix();
            let old = legacy_view_matrix(&c);
            let max_diff = new
                .to_cols_array()
                .iter()
                .zip(old.to_cols_array())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f32, f32::max);
            assert!(
                max_diff < 1e-4,
                "yaw {yaw}, pitch {pitch}: view_matrix must be unchanged away from the \
                 singularity, max element diff {max_diff}\n new {new:?}\n old {old:?}"
            );
        }
    }
}

/// `forward()` is pinned to Minecraft's convention by `camera.rs`'s own
/// `forward_matches_minecraft_convention`; this adds the bit-identity claim the
/// refactor needs — the closed form was moved behind a shared basis helper, and
/// nothing about the returned bits may change.
#[test]
fn forward_is_bit_identical_to_the_original_closed_form() {
    for yaw in [-180.0_f32, -37.5, 0.0, 12.25, 45.0, 90.0, 179.0, 360.0] {
        for pitch in [-90.0_f32, -89.9, -45.0, -0.5, 0.0, 12.25, 45.0, 89.9, 90.0] {
            let (sy, cy) = yaw.to_radians().sin_cos();
            let (sp, cp) = pitch.to_radians().sin_cos();
            let original = Vec3::new(-sy * cp, -sp, cy * cp);
            let got = cam(yaw, pitch).forward();
            assert_eq!(
                got.to_array(),
                original.to_array(),
                "forward() must stay bit-identical at yaw {yaw}, pitch {pitch}"
            );
        }
    }
}

/// The third row of the view matrix must be `-forward` exactly, so the matrix
/// and the accessor cannot drift apart — the failure mode where block targeting
/// (which uses `forward()`) and rendering (which uses `view_matrix()`) disagree.
#[test]
fn view_matrix_third_row_is_the_negated_forward() {
    for yaw in YAWS {
        for pitch in [-90.0_f32, -45.0, 0.0, 45.0, 90.0] {
            let c = cam(yaw, pitch);
            let (_, _, forward) = basis_from_view(c.view_matrix());
            assert_eq!(
                forward.to_array(),
                c.forward().to_array(),
                "yaw {yaw}, pitch {pitch}: the view matrix's forward row must equal forward()"
            );
        }
    }
}

// -- the winding invariant, at the singularity too ---------------------------

/// `CLAUDE.md`: `sign(det(gui_ortho * gui_item_pose))` must **equal**
/// `sign(det(Camera::view_projection()))`. The GUI half is pinned by
/// `item_render.rs`'s `winding_matches_the_world_camera` and
/// `sprite_drop_pixels.rs`; what this adds is that the *camera* half is
/// unchanged by the new construction — and that it stays unchanged at pitch
/// `±90`, where the old matrix was `NaN`/mirrored and `det` therefore said
/// nothing at all.
///
/// The sign is **derived** from a real camera (the pitch-0 one, which is
/// unchanged element-for-element by
/// [`view_matrix_agrees_with_the_legacy_construction_away_from_the_singularity`])
/// rather than asserted as a polarity. What that polarity *is* follows from the
/// projection's depth direction — negative under a forward `[0, 1]` one,
/// positive under reversed-Z, because mirroring the clip `z` axis flips a 4x4
/// determinant — and the GUI half tracks it by construction, since `gui_ortho`
/// carries the same depth direction. Only the agreement is asserted.
#[test]
fn view_projection_winding_sign_is_unchanged_at_every_pitch() {
    let reference = cam(0.0, 0.0).view_projection().determinant();
    assert!(
        reference.is_finite() && reference != 0.0,
        "premise check: the pitch-0 reference camera is degenerate ({reference}), \
         so there is no sign for the other pitches to agree with"
    );
    for yaw in YAWS {
        for pitch in [-90.0_f32, -89.9, -45.0, 0.0, 45.0, 89.9, 90.0] {
            let c = cam(yaw, pitch);
            let det = c.view_projection().determinant();
            assert!(
                det.is_finite() && det != 0.0,
                "yaw {yaw}, pitch {pitch}: det(view_projection) = {det} is degenerate"
            );
            assert_eq!(
                det.signum(),
                reference.signum(),
                "yaw {yaw}, pitch {pitch}: det(view_projection) = {det} flipped sign \
                 against the pitch-0 reference {reference} — held items and GUI blocks \
                 would render inside-out"
            );
            // And the view half specifically is +1, which is why the two signs agree.
            let view_det = c.view_matrix().determinant();
            assert!(
                (view_det - 1.0).abs() < 1e-3,
                "yaw {yaw}, pitch {pitch}: det(view_matrix) = {view_det}, must be +1"
            );
        }
    }
}

/// `sky_view_projection` zeroes the translation column of the view matrix, so it
/// inherits the basis wholesale — a NaN or mirrored basis at pitch `±90` reaches
/// the sky dome too. Cheap to pin, and it fails on the unfixed code for free.
#[test]
fn sky_view_projection_is_finite_at_the_singularity() {
    for yaw in YAWS {
        for pitch in [90.0_f32, -90.0] {
            let m = cam(yaw, pitch).sky_view_projection();
            assert!(
                m.is_finite(),
                "yaw {yaw}, pitch {pitch}: sky_view_projection is not finite: {m:?}"
            );
        }
    }
}

/// The frustum extracted at the singularity must still classify a box the camera
/// is unambiguously pointing at as visible. A degenerate basis makes every plane
/// normal `NaN`, and `Plane::from_vec4`'s `len > 0.0` guard silently substitutes
/// `Vec3::Z` — so the culler does not crash, it just culls the wrong things.
#[test]
fn frustum_still_sees_what_it_points_at_when_looking_straight_down() {
    let c = cam(0.0, 90.0);
    let f = c.frustum();
    let target = c.position + c.forward() * 20.0;
    let min = target - Vec3::splat(2.0);
    let max = target + Vec3::splat(2.0);
    assert!(
        f.intersects_aabb(min, max),
        "a box 20 blocks straight down the view direction must be visible; \
         planes were {:?}",
        f.planes.map(|p| (p.normal, p.d))
    );
    // Negative control on the detector itself: a box 20 blocks the *other* way
    // must be culled, so the assertion above is not satisfied by a frustum that
    // accepts everything.
    let behind = c.position - c.forward() * 20.0;
    assert!(
        !f.intersects_aabb(behind - Vec3::splat(2.0), behind + Vec3::splat(2.0)),
        "control failed: a box directly behind the camera must be culled"
    );
}

/// A `Vec4` import guard: `basis_from_view` reads columns, and a future edit that
/// switches the view matrix to a row-major layout would break every assertion in
/// this file in the same direction. Pin the one element whose position is
/// unambiguous — the homogeneous `w` row of a view matrix is `(0, 0, 0, 1)`.
#[test]
fn view_matrix_bottom_row_is_the_homogeneous_identity() {
    let m = cam(33.0, 90.0).view_matrix();
    let bottom = Vec4::new(m.x_axis.w, m.y_axis.w, m.z_axis.w, m.w_axis.w);
    assert_eq!(bottom, Vec4::W, "a view matrix's bottom row must be (0,0,0,1)");
}
