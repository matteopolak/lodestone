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

use glam::Vec3;
use lodestone_physics::{Aabb, CollisionView, PlayerState};
use lodestone_render::Camera;

/// Vertical field of view in degrees (vanilla default, "Normal" FOV).
pub const FOV_Y_DEGREES: f32 = 70.0;
/// Near plane distance in blocks.
pub const NEAR: f32 = 0.05;

/// Vanilla's third-person "back" camera distance, in blocks
/// (`Camera`'s zoom starts at `4.0` and is only ever pulled *in* from there by
/// collision, never pushed further out). This is the *desired* pullback before
/// [`collision_pullback`] clamps it against real geometry.
pub const THIRD_PERSON_DISTANCE: f32 = 4.0;

/// How far short of a real collision surface the pulled-back camera stops, in
/// blocks. Without this the eye would sit exactly on the wall it just clipped
/// against (and could poke a hair through it on the next frame's float
/// rounding); vanilla's own `Camera.getMaxZoom` shaves a small buffer the same
/// way (`partialTickTime` clip step 0.1).
pub const COLLISION_MARGIN: f32 = 0.1;

/// Builds the render camera for the current camera mode: the true first-person
/// eye when `third_person` is `false`, or that same eye pulled straight
/// backward along its own view direction — vanilla's actual "back" mode
/// algorithm, not an approximation of it — clamped so it never passes through
/// real collision geometry.
///
/// `view` is whichever [`CollisionView`] adapter the caller already has live
/// (`LiveCollision` on a server, `WorldCollision` on the offline fixture) —
/// this function is generic over it so it needs no dependency on
/// `crate::collision`'s concrete types.
#[must_use]
pub fn third_person_camera(eye: Camera, third_person: bool, view: &impl CollisionView) -> Camera {
    if !third_person {
        return eye;
    }
    let back = -eye.forward();
    let clamped = collision_pullback(eye.position, back, THIRD_PERSON_DISTANCE, view);
    // The margin only matters once something real was actually hit: in open
    // air `clamped` already equals the desired distance exactly, and shaving
    // a further 0.1 off *that* would make third person sit permanently 0.1
    // blocks closer than vanilla's own default zoom for no reason.
    let distance = if clamped < THIRD_PERSON_DISTANCE {
        (clamped - COLLISION_MARGIN).max(0.0)
    } else {
        clamped
    };
    let mut cam = eye;
    cam.position += back * distance;
    cam
}

/// How far the camera may travel from `eye` along `dir` (assumed already
/// pointing away from the player, i.e. "backward") before it would pass
/// through real collision geometry, clamped to `desired` blocks.
///
/// Marches voxel by voxel along the ray — the same grid-DDA traversal
/// `crate::raycast::raycast` uses for block targeting — but tests each visited
/// cell against its **real** collision boxes
/// ([`CollisionView::collision_boxes`]) with an exact ray/AABB intersection,
/// rather than a full-cube occlusion predicate. `LiveCollision::is_solid`'s own
/// doc comment warns that method is the *occlusion* answer, not the collision
/// one (a slab collides only to half a block and does not occlude at all) —
/// using it here would pull the camera in a full block early on a slab, or
/// (worse, since occlusion and collision also disagree the other way for
/// blocks like barriers) let it clip through geometry that has no visual face
/// to trigger on. Exact per-box intersection gets both right.
#[must_use]
pub fn collision_pullback(eye: Vec3, dir: Vec3, desired: f32, view: &impl CollisionView) -> f32 {
    if !desired.is_finite() || desired <= 0.0 {
        return 0.0;
    }
    let len = dir.length();
    if !len.is_finite() || len < 1e-6 {
        return desired;
    }
    let d = dir / len;

    let mut voxel = [
        eye.x.floor() as i32,
        eye.y.floor() as i32,
        eye.z.floor() as i32,
    ];
    let step = [sign(d.x), sign(d.y), sign(d.z)];
    let o = [eye.x, eye.y, eye.z];
    let dd = [d.x, d.y, d.z];
    let mut t_max = [0.0f32; 3];
    let mut t_delta = [0.0f32; 3];
    for a in 0..3 {
        if dd[a] == 0.0 {
            t_max[a] = f32::INFINITY;
            t_delta[a] = f32::INFINITY;
        } else {
            let next = if dd[a] > 0.0 {
                voxel[a] as f32 + 1.0
            } else {
                voxel[a] as f32
            };
            t_max[a] = (next - o[a]) / dd[a];
            t_delta[a] = (1.0 / dd[a]).abs();
        }
    }

    let mut boxes = Vec::new();
    // The eye's own cell first — third-person pullback starts from a point
    // that is already inside air in normal play, but a crouch/step edge can
    // leave it overlapping geometry, and that must clip too.
    view.collision_boxes(voxel[0], voxel[1], voxel[2], &mut boxes);
    if let Some(t) = nearest_entry(eye, d, &boxes, desired) {
        return t;
    }

    let iterations = (desired.ceil() as i32 * 3 + 8).max(8);
    for _ in 0..iterations {
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
        if t > desired {
            return desired;
        }
        boxes.clear();
        view.collision_boxes(voxel[0], voxel[1], voxel[2], &mut boxes);
        if let Some(hit) = nearest_entry(eye, d, &boxes, desired) {
            return hit;
        }
    }
    desired
}

/// The nearest entry distance (`<= limit`) among `boxes`, or `None` if the ray
/// misses every one of them within range.
fn nearest_entry(origin: Vec3, dir: Vec3, boxes: &[Aabb], limit: f32) -> Option<f32> {
    boxes
        .iter()
        .filter_map(|b| ray_aabb_entry(origin, dir, b))
        .filter(|t| *t <= limit)
        .fold(None, |acc: Option<f32>, t| Some(acc.map_or(t, |a| a.min(t))))
}

/// Exact ray/AABB intersection (the slab method): the distance along the
/// already-normalised `dir` at which the ray first enters `aabb`, or `None` if
/// it misses, or if `aabb` lies entirely behind `origin`.
fn ray_aabb_entry(origin: Vec3, dir: Vec3, aabb: &Aabb) -> Option<f32> {
    let min = Vec3::new(aabb.min_x as f32, aabb.min_y as f32, aabb.min_z as f32);
    let max = Vec3::new(aabb.max_x as f32, aabb.max_y as f32, aabb.max_z as f32);
    let o = [origin.x, origin.y, origin.z];
    let d = [dir.x, dir.y, dir.z];
    let lo = [min.x, min.y, min.z];
    let hi = [max.x, max.y, max.z];

    let mut t_min = 0.0f32;
    let mut t_max = f32::INFINITY;
    for a in 0..3 {
        if d[a].abs() < 1e-9 {
            if o[a] < lo[a] || o[a] > hi[a] {
                return None;
            }
        } else {
            let inv = 1.0 / d[a];
            let (mut t0, mut t1) = ((lo[a] - o[a]) * inv, (hi[a] - o[a]) * inv);
            if t0 > t1 {
                std::mem::swap(&mut t0, &mut t1);
            }
            t_min = t_min.max(t0);
            t_max = t_max.min(t1);
            if t_min > t_max {
                return None;
            }
        }
    }
    if t_max < 0.0 { None } else { Some(t_min.max(0.0)) }
}

fn sign(v: f32) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

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

    /// A fixed set of full-block collision boxes, keyed by block position —
    /// enough surface to test the pullback marcher without any real world.
    struct FakeWorld(std::collections::HashSet<(i32, i32, i32)>);

    impl CollisionView for FakeWorld {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if self.0.contains(&(x, y, z)) {
                out.push(Aabb::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(z),
                    f64::from(x) + 1.0,
                    f64::from(y) + 1.0,
                    f64::from(z) + 1.0,
                ));
            }
        }
    }

    fn empty_world() -> FakeWorld {
        FakeWorld(std::collections::HashSet::new())
    }

    fn wall_at_x(x: i32) -> FakeWorld {
        let mut blocks = std::collections::HashSet::new();
        for y in -5..5 {
            for z in -5..5 {
                blocks.insert((x, y, z));
            }
        }
        FakeWorld(blocks)
    }

    #[test]
    fn pullback_reaches_the_desired_distance_with_nothing_in_the_way() {
        let world = empty_world();
        let d = collision_pullback(Vec3::new(0.5, 0.5, 0.5), Vec3::new(-1.0, 0.0, 0.0), 4.0, &world);
        assert_eq!(d, 4.0);
    }

    #[test]
    fn pullback_stops_at_a_real_wall() {
        // A full-block wall occupying x∈[-2,-1]: the ray from x=0.5 travelling
        // in -X enters its near face (x=-1) at t = 0.5 - (-1) = 1.5.
        let world = wall_at_x(-2);
        let d = collision_pullback(Vec3::new(0.5, 0.5, 0.5), Vec3::new(-1.0, 0.0, 0.0), 4.0, &world);
        assert!((d - 1.5).abs() < 1e-4, "expected ~1.5, got {d}");
    }

    #[test]
    fn pullback_never_exceeds_desired_even_past_a_distant_wall() {
        // The wall is farther than the desired pullback, so the desired
        // distance wins outright rather than clamping to the (irrelevant) hit.
        let world = wall_at_x(-20);
        let d = collision_pullback(Vec3::new(0.5, 0.5, 0.5), Vec3::new(-1.0, 0.0, 0.0), 4.0, &world);
        assert_eq!(d, 4.0);
    }

    #[test]
    fn a_partial_shape_only_clips_where_it_actually_is() {
        // A "slab" occupying only the lower half of the block two behind the
        // eye: a ray at the slab's own height clips on it, one at head height
        // sails over it to the full desired distance. This is exactly the
        // occlusion-vs-collision distinction `is_solid` gets wrong for a real
        // slab; it is not, on its own, evidence that `is_solid` was used here
        // (nothing in this module calls it) but it is the regression this
        // exact-AABB approach exists to keep correct.
        struct Slab;
        impl CollisionView for Slab {
            fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
                if (x, y, z) == (-2, 0, 0) {
                    out.push(Aabb::new(-2.0, 0.0, 0.0, -1.0, 0.5, 1.0));
                }
            }
        }
        let slab = Slab;
        let low = collision_pullback(Vec3::new(0.5, 0.25, 0.5), Vec3::new(-1.0, 0.0, 0.0), 4.0, &slab);
        assert!((low - 1.5).abs() < 1e-4, "low ray should clip the slab: {low}");
        let high = collision_pullback(Vec3::new(0.5, 0.75, 0.5), Vec3::new(-1.0, 0.0, 0.0), 4.0, &slab);
        assert_eq!(high, 4.0, "high ray passes over the slab entirely");
    }

    #[test]
    fn third_person_camera_is_the_identity_in_first_person() {
        let eye = Camera {
            position: Vec3::new(0.0, 0.0, 0.0),
            yaw: 0.0,
            ..Camera::default()
        };
        let world = empty_world();
        let cam = third_person_camera(eye, false, &world);
        assert_eq!(cam.position, eye.position);
    }

    #[test]
    fn third_person_camera_pulls_back_along_view_direction() {
        let eye = Camera {
            position: Vec3::new(0.0, 0.0, 0.0),
            yaw: 0.0, // forward = +Z
            pitch: 0.0,
            ..Camera::default()
        };
        let world = empty_world();
        let cam = third_person_camera(eye, true, &world);
        // Pulled *backward*, i.e. -Z, by the full desired distance with
        // nothing to collide against.
        assert!((cam.position.z - (-THIRD_PERSON_DISTANCE)).abs() < 1e-4);
        assert!(cam.position.x.abs() < 1e-6);
        assert_eq!(cam.yaw, eye.yaw, "orientation is unchanged by pullback");
    }

    #[test]
    fn third_person_camera_stops_short_of_a_wall_by_the_margin() {
        let eye = Camera {
            position: Vec3::new(0.0, 0.0, 0.0),
            yaw: 0.0, // forward = +Z, so "back" is -Z
            pitch: 0.0,
            ..Camera::default()
        };
        // A full-block wall occupying z∈[-2,-1]: its near face (z=-1) sits
        // exactly 1 block behind the eye at z=0, so the raw hit distance is
        // 1.0 before the margin is subtracted.
        let mut blocks = std::collections::HashSet::new();
        for x in -5..5 {
            for y in -5..5 {
                blocks.insert((x, y, -2));
            }
        }
        let world = FakeWorld(blocks);
        let cam = third_person_camera(eye, true, &world);
        assert!(
            (cam.position.z - -(1.0 - COLLISION_MARGIN)).abs() < 1e-4,
            "got {}",
            cam.position.z
        );
    }
}
