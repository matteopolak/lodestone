//! Opt-in, nearby entity-placement diagnostics for resource-pack investigation.
//!
//! This module deliberately observes the final renderer boundary rather than
//! packet metadata. A frame, map, and display entity each have a different
//! pose chain, so packet-time logging cannot say which matrix or plane reached
//! the GPU. Set `RUST_LOG=pack_trace=debug` to log each nearby
//! `(render_surface, entity_id)` once for the process.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use glam::{Mat4, Vec3};

/// The local-player-eye radius used by the live placement trace.
pub(super) const CANDIDATE_RADIUS: f32 = 5.0;

/// Return whether a candidate is close enough to the local camera to inspect.
///
/// Kept separate from the once gate so the radius rule is unit-tested without
/// mutating process-global diagnostic state.
#[must_use]
pub(super) fn is_nearby_candidate(position: Vec3, camera_position: Vec3) -> bool {
    position.distance_squared(camera_position) <= CANDIDATE_RADIUS * CANDIDATE_RADIUS
}

/// Gate a nearby candidate to one opt-in diagnostic line per render surface.
///
/// The surface is part of the key because one item-frame entity legitimately
/// produces a body plus either a map, an ordinary item, or a special-item rig.
/// The log is otherwise quiet in a busy multiplayer area, while still leaving
/// every final producer observable.
#[must_use]
pub(super) fn should_trace_candidate(
    surface: &'static str,
    entity_id: i32,
    position: Vec3,
    camera_position: Vec3,
) -> bool {
    if !tracing::enabled!(target: "pack_trace", tracing::Level::DEBUG)
        || !is_nearby_candidate(position, camera_position)
    {
        return false;
    }
    static SEEN: OnceLock<Mutex<HashSet<(&'static str, i32)>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut seen = match seen.lock() {
        Ok(seen) => seen,
        Err(poisoned) => poisoned.into_inner(),
    };
    seen.insert((surface, entity_id))
}

/// Four world-space corners of a local `z = 0` unit quad after `transform`.
///
/// This is intentionally a geometric landmark rather than a decomposed
/// translation/rotation: it exposes an origin error, a flipped normal, and a
/// plane-depth conflict in one compact log field.
#[must_use]
pub(super) fn unit_quad_plane(transform: Mat4) -> [[f32; 3]; 4] {
    [
        transform.transform_point3(Vec3::new(0.0, 0.0, 0.0)).to_array(),
        transform.transform_point3(Vec3::new(1.0, 0.0, 0.0)).to_array(),
        transform.transform_point3(Vec3::new(1.0, 1.0, 0.0)).to_array(),
        transform.transform_point3(Vec3::new(0.0, 1.0, 0.0)).to_array(),
    ]
}

/// The outward normal of the diagnostic unit quad.
#[must_use]
pub(super) fn unit_quad_normal(transform: Mat4) -> [f32; 3] {
    transform.transform_vector3(Vec3::Z).normalize_or_zero().to_array()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_radius_is_five_blocks_from_the_local_camera() {
        let eye = Vec3::new(8.0, 65.62, -3.0);
        assert!(is_nearby_candidate(eye + Vec3::new(3.0, 0.0, 4.0), eye));
        assert!(!is_nearby_candidate(eye + Vec3::new(5.01, 0.0, 0.0), eye));
    }

    #[test]
    fn unit_quad_plane_keeps_all_four_landmarks_in_transform_order() {
        let transform = Mat4::from_translation(Vec3::new(2.0, 3.0, 4.0));
        assert_eq!(
            unit_quad_plane(transform),
            [[2.0, 3.0, 4.0], [3.0, 3.0, 4.0], [3.0, 4.0, 4.0], [2.0, 4.0, 4.0]]
        );
        assert_eq!(unit_quad_normal(transform), [0.0, 0.0, 1.0]);
    }
}
