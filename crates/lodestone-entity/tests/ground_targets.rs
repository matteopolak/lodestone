//! Where a ground mob goes: wander targets and path destinations land on ground it can stand on, and a melee chase re-paths on its timer.

use lodestone_entity::ai::{MobController, NavigatingMob};
use lodestone_entity::pathfinding::{Aabb, MobShape, PathType, PathWorld};
use lodestone_model::Vec3;

/// A floor at `y <= -1` for `x >= -3` (nothing beyond), with a plateau of
/// solid blocks up to `y = 2` for `x >= 3`.
struct Plateau;

impl Plateau {
    fn solid(x: i32, y: i32, _z: i32) -> bool {
        (x >= -3 && y <= -1) || (x >= 3 && y <= 2)
    }
}

impl PathWorld for Plateau {
    fn min_y(&self) -> i32 {
        -8
    }
    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        if Self::solid(x, y, z) { PathType::Blocked } else { PathType::Open }
    }
    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        if Self::solid(x, y, z) { 1.0 } else { 0.0 }
    }
    fn collides(&self, aabb: Aabb) -> bool {
        let (x0, x1) = (aabb.min_x.floor() as i32, (aabb.max_x - 1e-7).floor() as i32);
        let (y0, y1) = (aabb.min_y.floor() as i32, (aabb.max_y - 1e-7).floor() as i32);
        let (z0, z1) = (aabb.min_z.floor() as i32, (aabb.max_z - 1e-7).floor() as i32);
        (x0..=x1).any(|x| (y0..=y1).any(|y| (z0..=z1).any(|z| Self::solid(x, y, z))))
    }
    fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

/// Every chosen target is a bottom-centre cell that is clear and has ground
/// beneath it: candidates inside the plateau are lifted onto its top (y = 3),
/// and candidates over the missing floor are refused.
#[test]
fn stroll_targets_stand_on_ground_and_never_inside_it() {
    let mut mob = NavigatingMob::new(&Plateau, MobShape::land(0.6, 1.95), Vec3::new(0.5, 0.0, 0.5), 0.2, 64, 7, 63);
    let mut lifted = 0;
    let mut chosen = 0;
    for _ in 0..400 {
        let Some(t) = mob.random_stroll_target() else { continue };
        chosen += 1;
        let (x, y, z) = (t.x.floor() as i32, t.y.floor() as i32, t.z.floor() as i32);
        assert!(!Plateau::solid(x, y, z), "target inside a block: {t:?}");
        assert!(Plateau::solid(x, y - 1, z), "target with nothing under it: {t:?}");
        assert_eq!((t.x.rem_euclid(1.0), t.z.rem_euclid(1.0)), (0.5, 0.5), "not bottom-centred: {t:?}");
        if y == 3 {
            lifted += 1;
        }
    }
    assert!(chosen > 300, "ten attempts almost always find ground: {chosen}");
    assert!(lifted > 0, "no target was lifted out of the plateau");
}

/// A destination in the air drops to the ground under it and one inside the
/// ground rises out of it, so a ground path always ends on a standable cell.
#[test]
fn ground_destinations_snap_to_the_surface() {
    for (label, y) in [("in the air", 9.0), ("inside the floor", -3.0)] {
        let mut mob = NavigatingMob::new(&Plateau, MobShape::land(0.6, 1.95), Vec3::new(0.5, 0.0, 0.5), 0.2, 64, 7, 63);
        assert!(mob.move_to(Vec3::new(1.5, y, 0.5), 1.0), "no path to a target {label}");
        assert!(mob.path_reaches_target(), "a target {label} was searched for as written, not snapped");
        let end = mob.path_end().expect("a path was started");
        // Reach 1: the path may stop one block short of the snapped cell.
        assert!(
            end.y == 0 && (end.x - 1).abs() + end.z.abs() <= 1,
            "a target {label} must snap to the floor top, got {end:?}"
        );
    }
}

/// A melee chaser re-paths no more often than its 4-to-10 tick countdown even
/// when the target moves two blocks every tick. Over 120 ticks that bounds the
/// searches to 120 / 4 = 30 at most and, since the target always moves, to
/// 120 / 10 = 12 at least (each countdown is at most 4 + 6 = 10 near the
/// target). Re-pathing whenever the target block changed would search ~120.
#[test]
fn a_melee_chase_repaths_on_its_countdown_not_every_tick() {
    use lodestone_entity::ai::GoalSelector;
    use lodestone_entity::ai::goals::MeleeStrikeGoal;

    // Slow enough not to arrive within the run.
    let mut mob = NavigatingMob::new(&Plateau, MobShape::land(0.6, 1.95), Vec3::new(-2.5, 0.0, 0.5), 0.05, 64, 7, 63);
    let mut ai = GoalSelector::new();
    ai.add(1, Box::new(MeleeStrikeGoal::new(0.05, 1.0)));
    for tick in 0..120 {
        let wobble = if tick % 2 == 0 { 0.0 } else { 2.0 };
        mob.set_attack_target(Some(Vec3::new(-0.5 + wobble, 0.0, 0.5)));
        mob.tick(&mut ai);
    }
    let searches = mob.path_searches();
    assert!((12..=30).contains(&searches), "{searches} searches in 120 ticks");
}
