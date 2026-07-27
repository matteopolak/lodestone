//! Builds a [`lodestone_render::Camera`] from a physics [`PlayerState`].
//!
//! All camera conventions (RH, Y-up, yaw 0 = south, eye height 1.62, vertical
//! FOV 70, near 0.05, far = 4× render distance) are owned by the render crate;
//! this module only *reads* them so the shell never redefines them. The eye sits
//! [`PLAYER_EYE_HEIGHT`](lodestone_render::camera::PLAYER_EYE_HEIGHT) above the
//! feet, exactly like vanilla's standing eye.

use lodestone_physics::PlayerState;
use lodestone_render::{Camera, camera::PLAYER_EYE_HEIGHT};

/// Vertical field of view in degrees (vanilla default, "Normal" FOV).
pub const FOV_Y_DEGREES: f32 = 70.0;
/// Near plane distance in blocks.
pub const NEAR: f32 = 0.05;

/// Construct the render camera for the given player state, viewport aspect, and
/// render distance (in chunks).
#[must_use]
pub fn build_camera(state: &PlayerState, aspect: f32, render_distance: u32) -> Camera {
    let feet = glam::Vec3::new(
        state.position.x as f32,
        state.position.y as f32,
        state.position.z as f32,
    );
    Camera {
        position: feet + glam::Vec3::new(0.0, PLAYER_EYE_HEIGHT, 0.0),
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

    #[test]
    fn eye_is_above_feet() {
        let state = PlayerState::at(Vec3d::new(1.0, 64.0, 2.0), 0.0);
        let cam = build_camera(&state, 16.0 / 9.0, 8);
        assert!((cam.position.y - (64.0 + PLAYER_EYE_HEIGHT)).abs() < 1e-6);
        assert!((cam.position.x - 1.0).abs() < 1e-6);
    }

    #[test]
    fn far_scales_with_render_distance() {
        let state = PlayerState::at(Vec3d::ZERO, 0.0);
        let near = build_camera(&state, 1.0, 4);
        let far = build_camera(&state, 1.0, 16);
        assert!(
            far.far > near.far,
            "more render distance ⇒ farther far plane"
        );
    }

    #[test]
    fn degenerate_aspect_is_sanitised() {
        let state = PlayerState::at(Vec3d::ZERO, 0.0);
        let cam = build_camera(&state, 0.0, 8);
        assert_eq!(cam.aspect, 1.0);
    }
}
