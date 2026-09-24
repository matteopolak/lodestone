//! Interpolation between 20 Hz server updates and the render clock.
//!
//! The server confirms an entity's position roughly every tick (50 ms), but a
//! renderer runs far faster. Snapping to each new server position produces
//! visible stutter, so this module makes the seam explicit: an
//! [`Interpolated`] value keeps the **previous** confirmed sample alongside the
//! **current** one, and [`Interpolated::sample`] blends them by a render alpha
//! in `[0, 1]`.
//!
//! This mirrors vanilla's client behaviour, where an entity renders at
//! `lerp(prevPos, pos, partialTick)`. We deliberately keep the *policy* (how
//! alpha is derived from wall-clock time) out of this crate — that belongs to
//! the render loop — and provide only the mechanism.

use lodestone_model::{Rotation, Vec3};

/// Linearly interpolates two `f64`s.
#[must_use]
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Interpolates two positions.
#[must_use]
pub fn lerp_vec3(a: Vec3, b: Vec3, t: f64) -> Vec3 {
    Vec3::new(lerp(a.x, b.x, t), lerp(a.y, b.y, t), lerp(a.z, b.z, t))
}

/// Interpolates two rotations along the shortest arc, matching
/// `Mth.rotLerp`: the delta is wrapped into `[-180, 180)` before blending so a
/// yaw crossing the `±180` seam does not spin the long way around.
#[must_use]
pub fn lerp_rotation(a: Rotation, b: Rotation, t: f32) -> Rotation {
    Rotation::new(
        a.yaw + wrap_degrees(b.yaw - a.yaw) * t,
        a.pitch + wrap_degrees(b.pitch - a.pitch) * t,
    )
}

/// Wraps an angle in degrees into `[-180, 180)`, matching `Mth.wrapDegrees`.
#[must_use]
pub fn wrap_degrees(mut degrees: f32) -> f32 {
    degrees %= 360.0;
    if degrees >= 180.0 {
        degrees -= 360.0;
    }
    if degrees < -180.0 {
        degrees += 360.0;
    }
    degrees
}

/// A value with a remembered previous sample, so it can be rendered smoothly
/// between the discrete updates that set it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interpolated<T> {
    /// The previous confirmed sample.
    pub prev: T,
    /// The most recent confirmed sample.
    pub current: T,
}

impl<T: Copy> Interpolated<T> {
    /// Creates an interpolator whose `prev` and `current` both start at `value`
    /// (so it renders as a fixed point until the first update).
    #[must_use]
    pub fn new(value: T) -> Self {
        Self {
            prev: value,
            current: value,
        }
    }

    /// Pushes `current` into `prev` and stores `value` as the new `current`.
    /// Call this once per server update.
    pub fn push(&mut self, value: T) {
        self.prev = self.current;
        self.current = value;
    }

    /// Overwrites both samples, collapsing any pending interpolation. Used for
    /// hard teleports where blending would drag the entity across the map.
    pub fn snap(&mut self, value: T) {
        self.prev = value;
        self.current = value;
    }
}

impl Interpolated<Vec3> {
    /// The rendered position at render alpha `t` in `[0, 1]`.
    #[must_use]
    pub fn sample(&self, t: f64) -> Vec3 {
        lerp_vec3(self.prev, self.current, t)
    }
}

impl Interpolated<Rotation> {
    /// The rendered rotation at render alpha `t` in `[0, 1]`.
    #[must_use]
    pub fn sample(&self, t: f32) -> Rotation {
        lerp_rotation(self.prev, self.current, t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_blends_halfway() {
        let mut p = Interpolated::new(Vec3::new(0.0, 0.0, 0.0));
        p.push(Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(p.sample(0.0), Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(p.sample(0.5), Vec3::new(5.0, 0.0, 0.0));
        assert_eq!(p.sample(1.0), Vec3::new(10.0, 0.0, 0.0));
    }

    #[test]
    fn push_shifts_previous() {
        let mut p = Interpolated::new(Vec3::new(0.0, 0.0, 0.0));
        p.push(Vec3::new(1.0, 0.0, 0.0));
        p.push(Vec3::new(2.0, 0.0, 0.0));
        assert_eq!(p.prev, Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(p.sample(0.5), Vec3::new(1.5, 0.0, 0.0));
    }

    #[test]
    fn snap_collapses_interpolation() {
        let mut p = Interpolated::new(Vec3::new(0.0, 0.0, 0.0));
        p.push(Vec3::new(100.0, 0.0, 0.0));
        p.snap(Vec3::new(500.0, 64.0, -20.0));
        assert_eq!(p.sample(0.0), Vec3::new(500.0, 64.0, -20.0));
        assert_eq!(p.sample(1.0), Vec3::new(500.0, 64.0, -20.0));
    }

    #[test]
    fn yaw_takes_short_arc_across_seam() {
        // from 170 to -170 is a +20 turn, not -340.
        let a = Rotation::new(170.0, 0.0);
        let b = Rotation::new(-170.0, 0.0);
        let mid = lerp_rotation(a, b, 0.5);
        assert!((mid.yaw - 180.0).abs() < 1e-4 || (mid.yaw + 180.0).abs() < 1e-4);
    }

    #[test]
    fn wrap_degrees_range() {
        assert_eq!(wrap_degrees(0.0), 0.0);
        assert_eq!(wrap_degrees(190.0), -170.0);
        assert_eq!(wrap_degrees(-190.0), 170.0);
        assert_eq!(wrap_degrees(360.0), 0.0);
    }
}
