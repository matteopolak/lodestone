//! A swimmer plans through water cells in three dimensions and moves at the
//! swim speed.

use lodestone_entity::ai::{GoalSelector, MobController, NavigatingMob};
use lodestone_entity::pathfinding::{Aabb, MobShape, PathType, PathWorld};
use lodestone_model::Vec3;

const WIDTH: i32 = 10;
const DEPTH: i32 = 5;
const HEIGHT: i32 = 6;
const WALL_X: i32 = 5;
const GAP_FROM_Z: i32 = 2;

/// A water tank with a divider that leaves a gap along the far wall.
struct Tank;

impl Tank {
    fn inside(x: i32, y: i32, z: i32) -> bool {
        (0..WIDTH).contains(&x) && (0..HEIGHT).contains(&y) && (0..DEPTH).contains(&z)
    }
    fn wall(x: i32, y: i32, z: i32) -> bool {
        Self::inside(x, y, z) && x == WALL_X && z < GAP_FROM_Z
    }
    fn water(x: i32, y: i32, z: i32) -> bool {
        Self::inside(x, y, z) && !Self::wall(x, y, z)
    }
}

impl PathWorld for Tank {
    fn min_y(&self) -> i32 {
        -4
    }
    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        if Self::water(x, y, z) {
            PathType::Water
        } else if Self::inside(x, y, z) {
            PathType::Blocked
        } else {
            // Beyond the tank: stone all round.
            PathType::Blocked
        }
    }
    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        if Self::water(x, y, z) { 0.0 } else { 1.0 }
    }
    fn collides(&self, aabb: Aabb) -> bool {
        let (x0, x1) = (aabb.min_x.floor() as i32, (aabb.max_x - 1e-7).floor() as i32);
        let (y0, y1) = (aabb.min_y.floor() as i32, (aabb.max_y - 1e-7).floor() as i32);
        let (z0, z1) = (aabb.min_z.floor() as i32, (aabb.max_z - 1e-7).floor() as i32);
        (x0..=x1).any(|x| (y0..=y1).any(|y| (z0..=z1).any(|z| !Self::water(x, y, z))))
    }
    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        Self::water(x, y, z)
    }
}

/// A cod (0.5 x 0.3, speed 0.7) swims from one half of the tank to the other.
/// It never leaves the water or touches the divider, crosses the divider's
/// line through the gap, and cruises at `0.01 * speed / (1 - 0.9)` =
/// 0.07 blocks per tick (thrust a hundredth of the speed, 0.9 retained). The
/// route is kept under the path timeout, three times the expected time to the
/// next waypoint.
#[test]
fn a_swimmer_goes_around_the_divider_at_the_swim_cruise() {
    let mut mob = NavigatingMob::new(&Tank, MobShape::swimmer(0.5, 0.3), Vec3::new(3.5, 3.0, 1.5), 0.7, 256, 3, 63);
    mob.set_follow_range(32.0);
    assert!(mob.move_to(Vec3::new(7.5, 3.0, 1.5), 0.7), "no swim path across the tank");
    let mut ai = GoalSelector::new();
    let mut crossed_at = None;
    let mut steps = Vec::new();
    let mut last = mob.position();
    for _ in 0..300 {
        mob.tick(&mut ai);
        let here = mob.position();
        assert!(
            Tank::water(here.x.floor() as i32, (here.y + 0.15).floor() as i32, here.z.floor() as i32),
            "left the water at {here:?}"
        );
        if crossed_at.is_none() && last.x < f64::from(WALL_X) && here.x >= f64::from(WALL_X) {
            crossed_at = Some(here.z);
        }
        steps.push((here.x - last.x).hypot(here.z - last.z));
        last = here;
        if mob.navigation_done() && (here.x - 7.5).abs() < 1.0 {
            break;
        }
    }
    let z = crossed_at.unwrap_or_else(|| panic!("never crossed; ended at {last:?}, done={}", mob.navigation_done()));
    assert!(z >= f64::from(GAP_FROM_Z), "crossed the divider at z={z}");
    assert!((last.x - 7.5).abs() < 1.5, "stopped at {last:?}");
    let fastest = steps.iter().copied().fold(0.0, f64::max);
    assert!(fastest <= 0.07 * 1.02, "swam {fastest} blocks per tick, past the 0.07 cruise");
    assert!(fastest > 0.9 * 0.07, "never approached the 0.07 cruise: {fastest}");
}
