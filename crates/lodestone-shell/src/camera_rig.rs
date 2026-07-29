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

use lodestone_physics::PlayerState;
use lodestone_render::Camera;

/// Vertical field of view in degrees (vanilla default, "Normal" FOV).
pub const FOV_Y_DEGREES: f32 = 70.0;
/// Near plane distance in blocks.
pub const NEAR: f32 = 0.05;

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

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_physics::Vec3d;
    use lodestone_render::camera::PLAYER_EYE_HEIGHT;

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
}
