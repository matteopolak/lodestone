//! The `Display` entity family's shared geometry: the billboard orientation
//! and the `translation`/`left_rotation`/`scale`/`right_rotation`
//! transformation every `text_display`/`item_display`/`block_display` entity
//! carries (`world/entity/Display.java`, `26.2`).
//!
//! ## What it is
//!
//! Vanilla places a display entity in two composed steps
//! (`DisplayRenderer.submit`, `26.2`):
//!
//! ```text
//! pose = T(anchor) * orientation(billboard, entityYaw, entityPitch, cameraYaw, cameraPitch)
//!            * Transformation(translation, leftRotation, scale, rightRotation)
//! ```
//!
//! [`display_orientation`] is the first factor, [`DisplayTransformation::to_matrix`]
//! is the second, and [`display_placement_matrix`] composes both against a
//! world-space anchor. Everything here is pure geometry with no GPU
//! dependency, which is what makes the billboard-mode test axis the owner
//! named (`docs/`-adjacent brief: "test at least `fixed` against `center`")
//! checkable without a device at all — see this module's tests.
//!
//! ## How it works
//!
//! Four billboard modes (`Display.BillboardConstraints`) answer one question
//! differently: **which yaw and which pitch does the entity face with?**
//!
//! | mode | yaw source | pitch source |
//! |---|---|---|
//! | `Fixed` | the entity's own reported yaw | the entity's own reported pitch |
//! | `Horizontal` | the entity's own reported yaw | the **camera's** pitch |
//! | `Vertical` | the **camera's** yaw | the entity's own reported pitch |
//! | `Center` | the **camera's** yaw | the **camera's** pitch |
//!
//! `Fixed` therefore never rotates to face the viewer at all — it is a
//! billboard nailed to whatever orientation the entity itself carries (`0,0`
//! unless a summon command sets `Rotation`), while `Center` is a full
//! camera-facing sprite. `Horizontal`/`Vertical` each track one axis and hold
//! the other at the entity's own value.
//!
//! ## How to change it
//!
//! [`display_orientation`] is a direct transcription of
//! `DisplayRenderer.calculateOrientation` — do not "simplify" the per-mode
//! yaw/pitch source table above, since that table *is* the four modes' entire
//! behavioural difference. [`transform_camera_yaw`]/[`transform_camera_pitch`]
//! carry the `- 180`/negation vanilla applies to the raw camera angles before
//! they enter the same `rotationYXZ` vanilla uses for the entity's own
//! yaw/pitch — dropping either offset makes `Center` face 180° away from the
//! viewer, or invert its head-tilt tracking, while still looking plausible in
//! a screenshot taken from directly in front.
//!
//! ## Configuration
//!
//! None — every input is a per-frame value the caller already has (entity
//! rotation off the spawn/rotate packet, camera yaw/pitch, the synced
//! transformation fields).
//!
//! ## Dependencies
//!
//! `glam` only. No GPU device, no asset manager — see the module doc above
//! for why that is deliberate.

use glam::{Mat4, Quat, Vec3};

/// `Display.BillboardConstraints` (`world/entity/Display.java`, `26.2`): which
/// of the entity's own rotation and the camera's rotation this display faces
/// with, per axis. See the module doc's table for the exact source of each
/// axis in each mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BillboardMode {
    /// Faces its own reported rotation on both axes — never tracks the
    /// camera. Wire id `0`, the accessor's own default.
    #[default]
    Fixed,
    /// Yaw from the entity's own rotation, pitch from the camera. Wire id `1`.
    Vertical,
    /// Yaw from the camera, pitch from the entity's own rotation. Wire id `2`.
    Horizontal,
    /// Both axes from the camera — a full billboard. Wire id `3`.
    Center,
}

impl BillboardMode {
    /// The wire id `Display.BillboardConstraints.getId()` reports, `Byte`
    /// metadata index — `ByIdMap.continuous(..., OutOfBoundsStrategy.ZERO)`
    /// means an out-of-range byte resolves to `Fixed` (id `0`), which
    /// [`Self::from_wire`] reproduces via its own `unwrap_or(Fixed)` fallback
    /// rather than failing.
    #[must_use]
    pub fn wire_id(self) -> u8 {
        match self {
            BillboardMode::Fixed => 0,
            BillboardMode::Vertical => 1,
            BillboardMode::Horizontal => 2,
            BillboardMode::Center => 3,
        }
    }

    /// The inverse of [`Self::wire_id`], with vanilla's own out-of-range
    /// fallback to `Fixed` rather than a `None`/error — matching
    /// `ByIdMap.OutOfBoundsStrategy.ZERO`, since a byte off the wire is not a
    /// `Result` this renderer can refuse to draw.
    #[must_use]
    pub fn from_wire(id: u8) -> Self {
        match id {
            1 => BillboardMode::Vertical,
            2 => BillboardMode::Horizontal,
            3 => BillboardMode::Center,
            _ => BillboardMode::Fixed,
        }
    }
}

/// `DisplayRenderer.transformYRot`: `cameraYRot - 180`. Vanilla's camera yaw
/// and an entity's own yaw are zeroed at opposite headings (the camera looks
/// *along* its yaw; an entity's billboard needs to face *back toward* the
/// camera), so this rotates the raw camera yaw a half-turn before it can
/// stand in for an entity yaw in the same `rotationYXZ` call.
#[must_use]
pub fn transform_camera_yaw(camera_yaw_deg: f32) -> f32 {
    camera_yaw_deg - 180.0
}

/// `DisplayRenderer.transformXRot`: `-cameraXRot`. The camera's pitch and an
/// entity's own pitch increase in opposite senses (vanilla's camera pitch is
/// down-positive; the `rotationYXZ` this feeds expects the same sense an
/// entity's own reported pitch already has), so this negates it before reuse.
#[must_use]
pub fn transform_camera_pitch(camera_pitch_deg: f32) -> f32 {
    -camera_pitch_deg
}

/// `DisplayRenderer.calculateOrientation` (`26.2`): the rotation a display
/// entity's model faces, before its own [`DisplayTransformation`] is applied
/// on top. See the module doc's table for which of `entity_yaw_deg`/
/// `entity_pitch_deg`/`camera_yaw_deg`/`camera_pitch_deg` each `mode`
/// actually reads — the two it does not read are accepted but ignored, which
/// is itself the fixed-vs-tracking distinction this function exists to draw.
///
/// `rotationYXZ(y, x, z)` in vanilla (`Quaternionf`, JOML) builds the
/// quaternion for the intrinsic Y-then-X-then-Z Euler sequence — exactly
/// [`glam::EulerRot::YXZ`]'s own convention, so this is a direct call rather
/// than a hand-composed product of three axis quaternions.
#[must_use]
pub fn display_orientation(
    mode: BillboardMode,
    entity_yaw_deg: f32,
    entity_pitch_deg: f32,
    camera_yaw_deg: f32,
    camera_pitch_deg: f32,
) -> Quat {
    let (yaw_deg, pitch_deg) = match mode {
        BillboardMode::Fixed => (entity_yaw_deg, entity_pitch_deg),
        BillboardMode::Horizontal => (entity_yaw_deg, transform_camera_pitch(camera_pitch_deg)),
        BillboardMode::Vertical => (transform_camera_yaw(camera_yaw_deg), entity_pitch_deg),
        BillboardMode::Center => (
            transform_camera_yaw(camera_yaw_deg),
            transform_camera_pitch(camera_pitch_deg),
        ),
    };
    // Vanilla: `output.rotationYXZ(-rad(yRot), rad(xRot), 0)` — the yaw term
    // is negated, the pitch term is not.
    Quat::from_euler(
        glam::EulerRot::YXZ,
        -yaw_deg.to_radians(),
        pitch_deg.to_radians(),
        0.0,
    )
}

/// `com.mojang.math.Transformation`'s four synced fields
/// (`Display.DATA_TRANSLATION_ID`/`DATA_LEFT_ROTATION_ID`/`DATA_SCALE_ID`/
/// `DATA_RIGHT_ROTATION_ID`) — shared by **every** `Display` subtype
/// (`text_display`, `item_display`, `block_display`), which is exactly the
/// "field declared on a base record, inherited by every variant" shape this
/// codebase has been burned by before (a shield's `ItemModel.Unbaked`
/// transformation, ported once and read only on `special` nodes). Read it
/// off every display variant unconditionally, not just the ones that "look
/// like" they need scaling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayTransformation {
    /// `Display.DATA_TRANSLATION_ID`, in blocks.
    pub translation: Vec3,
    /// `Display.DATA_LEFT_ROTATION_ID` — applied **before** scale.
    pub left_rotation: Quat,
    /// `Display.DATA_SCALE_ID`, per-axis.
    pub scale: Vec3,
    /// `Display.DATA_RIGHT_ROTATION_ID` — applied **after** scale.
    pub right_rotation: Quat,
}

impl Default for DisplayTransformation {
    /// `Transformation.IDENTITY` / the entity data accessors' own defaults
    /// (`entityData.define(DATA_SCALE_ID, new Vector3f(1,1,1))`, the rest
    /// zero/identity) — what a `/summon` with no `transformation` NBT tag
    /// gets.
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            left_rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
            right_rotation: Quat::IDENTITY,
        }
    }
}

impl DisplayTransformation {
    /// `Transformation.compose`'s private `compose(...)` (`26.2`):
    /// `T(translation) * R(leftRotation) * S(scale) * R(rightRotation)`, in
    /// that order — translate, then the left rotation, then scale, then the
    /// right rotation, composed left-to-right exactly as vanilla's own
    /// `Matrix4f` calls chain (`result.translation(...); result.rotate(left);
    /// result.scale(...); result.rotate(right);`).
    #[must_use]
    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_translation(self.translation)
            * Mat4::from_quat(self.left_rotation)
            * Mat4::from_scale(self.scale)
            * Mat4::from_quat(self.right_rotation)
    }
}

/// The full per-frame placement for a display entity: `T(anchor) *
/// orientation * transformation`, vanilla's `DisplayRenderer.submit`
/// (`poseStack.pushPose(); mulPose(orientation); mulPose(transformation);`)
/// with the entity's world position folded in as the outermost translation —
/// `submit` itself receives that translation already applied by the caller's
/// own `PoseStack`, one level up (`EntityRenderDispatcher.render`).
#[must_use]
pub fn display_placement_matrix(anchor: Vec3, orientation: Quat, transform: &DisplayTransformation) -> Mat4 {
    Mat4::from_translation(anchor) * Mat4::from_quat(orientation) * transform.to_matrix()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    /// A pure Y-axis rotation should round-trip through `display_orientation`
    /// with `Fixed` exactly as `Quat::from_rotation_y` would build it —
    /// pinning the sign/axis convention before the mode table is trusted for
    /// anything else. Vanilla's yaw term is negated relative to the raw
    /// input, so feeding `yaw = -90` (not `90`) is what should land on `+90`
    /// about Y.
    #[test]
    fn fixed_mode_reproduces_the_entitys_own_yaw_with_negated_sign() {
        let q = display_orientation(BillboardMode::Fixed, -90.0, 0.0, 12345.0, -6789.0);
        let expected = Quat::from_rotation_y(FRAC_PI_2);
        assert!(
            q.abs_diff_eq(expected, 1e-4) || q.abs_diff_eq(-expected, 1e-4),
            "fixed mode should face the entity's own (negated) yaw regardless \
             of the camera angles it was also given: got {q:?}, expected {expected:?}"
        );
    }

    /// **The owner's own discriminating pair.** `Fixed` must not rotate with
    /// the camera at all; `Center` must fully track it. Feeding the two modes
    /// the *same* entity rotation and the *same* camera rotation, with the
    /// two genuinely different, is what makes this an actual test rather
    /// than "the code runs": if `Fixed` secretly tracked the camera (or
    /// `Center` secretly ignored it), the two branches below would produce
    /// the same quaternion, and this assertion is exactly the one that
    /// would then fail.
    #[test]
    fn fixed_does_not_track_the_camera_and_center_fully_does() {
        let entity_yaw = 20.0;
        let entity_pitch = 5.0;
        let camera_yaw = 200.0;
        let camera_pitch = -35.0;

        let fixed = display_orientation(
            BillboardMode::Fixed,
            entity_yaw,
            entity_pitch,
            camera_yaw,
            camera_pitch,
        );
        let center = display_orientation(
            BillboardMode::Center,
            entity_yaw,
            entity_pitch,
            camera_yaw,
            camera_pitch,
        );

        // Fixed depends only on the entity's own rotation: swapping in a
        // *different* camera rotation must not move it at all.
        let fixed_other_camera = display_orientation(BillboardMode::Fixed, entity_yaw, entity_pitch, 0.0, 0.0);
        assert!(
            fixed.abs_diff_eq(fixed_other_camera, 1e-5),
            "Fixed rotated when only the camera angle changed — it has started \
             tracking the camera, which is exactly what Fixed must never do"
        );

        // Center depends only on the camera: swapping in a *different* entity
        // rotation must not move it at all.
        let center_other_entity = display_orientation(BillboardMode::Center, 999.0, -40.0, camera_yaw, camera_pitch);
        assert!(
            center.abs_diff_eq(center_other_entity, 1e-5),
            "Center rotated when only the entity's own rotation changed — it \
             is not fully tracking the camera"
        );

        // And the two must actually differ from each other for this input,
        // since entity and camera rotation are chosen to disagree — this is
        // the "an input where both hypotheses coincide is not a test" check.
        assert!(
            !fixed.abs_diff_eq(center, 1e-3),
            "Fixed and Center produced the same orientation for inputs chosen \
             to disagree — the fixture cannot discriminate the two modes"
        );
    }

    /// `Vertical`/`Horizontal` each track exactly one axis — checked the same
    /// "hold the other input fixed, vary the one that should not matter" way
    /// as the Fixed/Center pair above, one axis at a time.
    #[test]
    fn vertical_tracks_camera_yaw_only_and_horizontal_tracks_camera_pitch_only() {
        let entity_yaw = 15.0;
        let entity_pitch = -8.0;

        let vertical_a = display_orientation(BillboardMode::Vertical, entity_yaw, entity_pitch, 40.0, 999.0);
        let vertical_b = display_orientation(BillboardMode::Vertical, entity_yaw, entity_pitch, 40.0, -999.0);
        assert!(
            vertical_a.abs_diff_eq(vertical_b, 1e-5),
            "Vertical moved when only the camera *pitch* changed — it should \
             track camera yaw only, holding its own pitch"
        );
        let vertical_c = display_orientation(BillboardMode::Vertical, entity_yaw, entity_pitch, 41.0, 999.0);
        assert!(
            !vertical_a.abs_diff_eq(vertical_c, 1e-5),
            "Vertical did not move when the camera yaw changed"
        );

        let horizontal_a = display_orientation(BillboardMode::Horizontal, entity_yaw, entity_pitch, 999.0, 12.0);
        let horizontal_b = display_orientation(BillboardMode::Horizontal, entity_yaw, entity_pitch, -999.0, 12.0);
        assert!(
            horizontal_a.abs_diff_eq(horizontal_b, 1e-5),
            "Horizontal moved when only the camera *yaw* changed — it should \
             track camera pitch only, holding its own yaw"
        );
        let horizontal_c = display_orientation(BillboardMode::Horizontal, entity_yaw, entity_pitch, 999.0, 13.0);
        assert!(
            !horizontal_a.abs_diff_eq(horizontal_c, 1e-5),
            "Horizontal did not move when the camera pitch changed"
        );
    }

    /// `BillboardMode::wire_id`/`from_wire` round-trip for every real id, and
    /// an out-of-range byte falls back to `Fixed` rather than panicking —
    /// `ByIdMap.OutOfBoundsStrategy.ZERO`.
    #[test]
    fn wire_id_round_trips_and_out_of_range_falls_back_to_fixed() {
        for mode in [
            BillboardMode::Fixed,
            BillboardMode::Vertical,
            BillboardMode::Horizontal,
            BillboardMode::Center,
        ] {
            assert_eq!(BillboardMode::from_wire(mode.wire_id()), mode);
        }
        assert_eq!(BillboardMode::from_wire(200), BillboardMode::Fixed);
    }

    /// `Transformation.compose`'s order — translate, left-rotate, scale,
    /// right-rotate — checked with a case where getting the order wrong
    /// (e.g. scaling before rotating) changes the result: a left rotation of
    /// 90 degrees about Z composed with a non-uniform scale.
    #[test]
    fn transformation_matrix_applies_translate_then_left_rotate_then_scale_then_right_rotate() {
        let t = DisplayTransformation {
            translation: Vec3::new(1.0, 0.0, 0.0),
            left_rotation: Quat::from_rotation_z(FRAC_PI_2),
            scale: Vec3::new(2.0, 1.0, 1.0),
            right_rotation: Quat::IDENTITY,
        };
        // A point at the local origin, scaled and rotated, then translated:
        // (0,0,0) -> scale -> (0,0,0) -> rotate -> (0,0,0) -> translate -> (1,0,0).
        let origin = t.to_matrix().transform_point3(Vec3::ZERO);
        assert!(
            origin.abs_diff_eq(Vec3::new(1.0, 0.0, 0.0), 1e-5),
            "origin should land at the pure translation: got {origin:?}"
        );
        // A unit +X point: scaled to (2,0,0) *before* the left rotation, then
        // rotated 90 degrees about Z (+X -> +Y), then translated.
        let point = t.to_matrix().transform_point3(Vec3::X);
        assert!(
            point.abs_diff_eq(Vec3::new(1.0, 2.0, 0.0), 1e-4),
            "scale must apply before the left rotation in local space, and the \
             left rotation before the outer translation: got {point:?}, \
             expected (1, 2, 0)"
        );
    }

    /// Identity transformation composed with identity orientation at a
    /// nonzero anchor should place a local point at exactly `anchor + point`.
    #[test]
    fn identity_placement_is_a_pure_translation_to_the_anchor() {
        let anchor = Vec3::new(3.0, 4.0, 5.0);
        let placement = display_placement_matrix(anchor, Quat::IDENTITY, &DisplayTransformation::default());
        let world = placement.transform_point3(Vec3::new(0.5, 0.5, 0.5));
        assert!(world.abs_diff_eq(anchor + Vec3::splat(0.5), 1e-5));
    }
}
