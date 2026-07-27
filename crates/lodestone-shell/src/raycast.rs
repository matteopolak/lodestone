//! Voxel ray casting for block targeting — the front half of the interaction
//! loop (look at a block, then break or place against it).
//!
//! This is the Amanatides–Woo grid-DDA traversal: from the eye, step voxel by
//! voxel along the view ray, stopping at the first solid cell within reach. It is
//! a pure function of an `is_solid` closure so it can be unit-tested against a
//! synthetic world with no GPU, no window, and no `lodestone-world` at all.
//!
//! The direction is taken straight from [`lodestone_render::Camera::forward`] so
//! the shell never re-derives the yaw/pitch → direction convention (the render
//! crate owns it, reconciled against vanilla). Vanilla's block-interaction reach
//! is 4.5 blocks from the eye; [`REACH`] matches.

/// Vanilla block-interaction range, in blocks, measured from the eye.
pub const REACH: f64 = 4.5;

/// A block the view ray struck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RayHit {
    /// World coordinates of the solid block that was hit.
    pub block: [i32; 3],
    /// Unit face normal of the struck face, pointing back toward the eye
    /// (e.g. `[0, 1, 0]` when the ray hit a block's top). This is exactly the
    /// offset to the empty cell a placed block would occupy.
    pub normal: [i32; 3],
}

impl RayHit {
    /// The cell adjacent to the hit face — where a placed block would go.
    #[must_use]
    pub fn place_position(&self) -> [i32; 3] {
        [
            self.block[0] + self.normal[0],
            self.block[1] + self.normal[1],
            self.block[2] + self.normal[2],
        ]
    }
}

/// Cast a ray from `origin` along `dir` (need not be normalised) up to `reach`
/// blocks, returning the first solid block hit.
///
/// `is_solid(x, y, z)` decides which cells are targetable. The origin cell is
/// skipped (the eye is assumed to be in air); traversal steps into neighbouring
/// cells and reports the face it entered through.
#[must_use]
pub fn raycast(
    origin: [f64; 3],
    dir: [f64; 3],
    reach: f64,
    is_solid: impl Fn(i32, i32, i32) -> bool,
) -> Option<RayHit> {
    let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    if !len.is_finite() || len < 1e-9 {
        return None;
    }
    let d = [dir[0] / len, dir[1] / len, dir[2] / len];

    let mut voxel = [
        origin[0].floor() as i32,
        origin[1].floor() as i32,
        origin[2].floor() as i32,
    ];
    let step = [sign(d[0]), sign(d[1]), sign(d[2])];

    // Distance (in ray-length units) to the first cell boundary on each axis,
    // and the per-cell increment thereafter.
    let mut t_max = [0.0f64; 3];
    let mut t_delta = [0.0f64; 3];
    for a in 0..3 {
        if d[a] == 0.0 {
            t_max[a] = f64::INFINITY;
            t_delta[a] = f64::INFINITY;
        } else {
            let next = if d[a] > 0.0 {
                f64::from(voxel[a]) + 1.0
            } else {
                f64::from(voxel[a])
            };
            t_max[a] = (next - origin[a]) / d[a];
            t_delta[a] = (1.0 / d[a]).abs();
        }
    }

    // A generous cap so a degenerate ray can never loop forever.
    for _ in 0..(reach.ceil() as i32 * 3 + 8) {
        // Advance across the nearest axis boundary.
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
        if t > reach {
            return None;
        }
        if is_solid(voxel[0], voxel[1], voxel[2]) {
            let mut normal = [0, 0, 0];
            normal[axis] = -step[axis];
            return Some(RayHit {
                block: voxel,
                normal,
            });
        }
    }
    None
}

fn sign(v: f64) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat solid floor at all `y < 10`.
    fn floor(_x: i32, y: i32, _z: i32) -> bool {
        y < 10
    }

    #[test]
    fn looking_down_hits_the_floor_from_above() {
        // Eye at y=12 looking straight down hits the top of the y=9 block.
        let hit = raycast([0.5, 12.0, 0.5], [0.0, -1.0, 0.0], REACH, floor)
            .expect("should hit the floor");
        assert_eq!(hit.block, [0, 9, 0]);
        assert_eq!(hit.normal, [0, 1, 0], "hit the top face");
        assert_eq!(hit.place_position(), [0, 10, 0], "place goes on top");
    }

    #[test]
    fn out_of_reach_misses() {
        // Floor top is at y=10; eye at y=20 looking down is >4.5 away.
        assert!(raycast([0.5, 20.0, 0.5], [0.0, -1.0, 0.0], REACH, floor).is_none());
    }

    #[test]
    fn looking_up_into_empty_sky_misses() {
        assert!(raycast([0.5, 12.0, 0.5], [0.0, 1.0, 0.0], REACH, floor).is_none());
    }

    #[test]
    fn side_face_normal_points_back_at_the_eye() {
        // A single wall block at x=3; ray travels +X into its −X face.
        let wall = |x: i32, _y: i32, _z: i32| x == 3;
        let hit = raycast([0.5, 0.5, 0.5], [1.0, 0.0, 0.0], 10.0, wall).expect("hits wall");
        assert_eq!(hit.block, [3, 0, 0]);
        assert_eq!(hit.normal, [-1, 0, 0]);
    }

    #[test]
    fn degenerate_direction_is_none() {
        assert!(raycast([0.0, 0.0, 0.0], [0.0, 0.0, 0.0], REACH, floor).is_none());
    }

    #[test]
    fn diagonal_ray_still_lands_on_a_solid_cell() {
        let hit = raycast([0.5, 12.0, 0.5], [0.3, -1.0, 0.2], REACH, floor)
            .expect("angled ray still reaches the floor");
        assert!(
            hit.block[1] == 9,
            "landed on the floor surface, got {:?}",
            hit.block
        );
    }
}
