//! Shared coordinate-ordering arithmetic for join and view streaming.
//!
//! This module owns the pure distance/frustum calculations. The parent
//! scheduler keeps the queue state and worker admission, while both queue
//! ordering and movement-triggered view batches use these same keys.

use super::FRUSTUM_HALF_ANGLE_DEGREES;

#[must_use]
pub(super) fn ring_distance(centre: (i32, i32), coord: (i32, i32)) -> i32 {
    (coord.0 - centre.0).abs().max((coord.1 - centre.1).abs())
}

#[must_use]
pub(super) fn in_frustum(centre: (i32, i32), yaw_degrees: f32, coord: (i32, i32)) -> bool {
    if ring_distance(centre, coord) <= 1 {
        return true;
    }
    let yaw = yaw_degrees.to_radians();
    // The game's yaw convention points 0 degrees towards +Z and 90 towards -X.
    let (fx, fz) = (-yaw.sin(), yaw.cos());
    let (dx, dz) = (
        (coord.0 - centre.0) as f32,
        (coord.1 - centre.1) as f32,
    );
    let len = (dx * dx + dz * dz).sqrt();
    if len == 0.0 {
        return true;
    }
    let cosine = (fx * dx + fz * dz) / len;
    cosine >= FRUSTUM_HALF_ANGLE_DEGREES.to_radians().cos()
}

#[must_use]
pub(super) fn priority_key(
    centre: (i32, i32),
    facing: Option<f32>,
    coord: (i32, i32),
    given_index: u32,
) -> (i32, u8, u32) {
    let (ring, penalty) = distance_and_penalty(centre, facing, coord);
    (ring, penalty, given_index)
}

#[must_use]
pub(super) fn distance_and_penalty(
    centre: (i32, i32),
    facing: Option<f32>,
    coord: (i32, i32),
) -> (i32, u8) {
    let penalty = match facing {
        Some(yaw) if in_frustum(centre, yaw, coord) => 0,
        Some(_) => 1,
        None => 0,
    };
    (ring_distance(centre, coord), penalty)
}

#[must_use]
pub(super) fn view_order_key(
    centre: (i32, i32),
    facing: Option<f32>,
    coord: (i32, i32),
) -> (i32, u8, i32, i32) {
    let (ring, penalty) = distance_and_penalty(centre, facing, coord);
    (ring, penalty, coord.0, coord.1)
}
