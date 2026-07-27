//! End-to-end pathfinding tests against a synthetic world.
//!
//! These exercise the whole `PathFinder` + `WalkNodeEvaluator` stack through the
//! public API, using the project's preferred detector: **known node positions in
//! a known world**. A transposed axis or an off-by-one floor calculation
//! survives a "did it find *a* path" check but fails an exact-coordinate check
//! instantly. Per-mob traversability (width/height changing what is passable),
//! step-up and drop-down each get a dedicated world.

use lodestone_entity::pathfinding::{
    Aabb, MobShape, PathFinder, PathNode, PathParams, PathStart, PathType, PathWorld,
};
use lodestone_model::BlockPos;
use std::collections::{HashMap, HashSet};

/// A block world defined by a per-column ground height plus explicit extra
/// solid blocks and water. A block `(x, y, z)` is solid if it is an explicit
/// solid or `y <= ground_top(x, z)`.
struct GridWorld {
    ground_top: i32,
    columns: HashMap<(i32, i32), i32>,
    solids: HashSet<(i32, i32, i32)>,
    water: HashSet<(i32, i32, i32)>,
}

impl GridWorld {
    fn flat(ground_top: i32) -> Self {
        Self {
            ground_top,
            columns: HashMap::new(),
            solids: HashSet::new(),
            water: HashSet::new(),
        }
    }

    fn column_top(&self, x: i32, z: i32) -> i32 {
        *self.columns.get(&(x, z)).unwrap_or(&self.ground_top)
    }

    fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        if self.solids.contains(&(x, y, z)) {
            return true;
        }
        y <= self.column_top(x, z)
    }
}

impl PathWorld for GridWorld {
    fn min_y(&self) -> i32 {
        0
    }

    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        if self.water.contains(&(x, y, z)) {
            return PathType::Water;
        }
        if self.is_solid(x, y, z) {
            PathType::Blocked
        } else {
            PathType::Open
        }
    }

    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        if self.is_solid(x, y, z) { 1.0 } else { 0.0 }
    }

    fn collides(&self, aabb: Aabb) -> bool {
        let x0 = aabb.min_x.floor() as i32;
        let x1 = (aabb.max_x - 1e-7).floor() as i32;
        let y0 = aabb.min_y.floor() as i32;
        let y1 = (aabb.max_y - 1e-7).floor() as i32;
        let z0 = aabb.min_z.floor() as i32;
        let z1 = (aabb.max_z - 1e-7).floor() as i32;
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    if self.is_solid(x, y, z) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        self.water.contains(&(x, y, z))
    }
}

fn finder() -> PathFinder {
    // Generous budget so synthetic searches never starve.
    PathFinder::new(4000)
}

fn params() -> PathParams {
    PathParams {
        max_path_length: 200.0,
        reach_range: 1,
        visited_multiplier: 1.0,
    }
}

fn contains_node(nodes: &[PathNode], x: i32, y: i32, z: i32) -> bool {
    nodes.iter().any(|n| n.x == x && n.y == y && n.z == z)
}

#[test]
fn straight_path_on_flat_ground_has_known_endpoints() {
    let world = GridWorld::flat(63); // air at y >= 64
    let mob = MobShape::land(0.9, 0.9); // pig-sized
    let start = PathStart::grounded(0.5, 64.0, 0.5);
    let path = finder()
        .find_path(&world, &mob, start, &[BlockPos::new(5, 64, 0)], params())
        .expect("a path should be found on flat ground");

    assert!(path.reached(), "target on open flat ground must be reached");
    // Known values at known positions: the walk starts on the mob's own cell
    // and ends adjacent to the requested target, all at feet level y=64.
    let first = path.node(0).unwrap();
    assert_eq!((first.x, first.y, first.z), (0, 64, 0));
    let last = path.end_node().unwrap();
    assert_eq!(last.y, 64, "flat ground must not change height");
    assert!(
        (last.x - 5).abs() + last.z.abs() <= 1,
        "last waypoint {:?} should be within reach of the target",
        (last.x, last.y, last.z)
    );
    // Every waypoint stays at feet level on flat ground.
    assert!(path.nodes().iter().all(|n| n.y == 64));
}

#[test]
fn narrow_mob_fits_gap_that_wide_mob_cannot() {
    // A wall one block above the floor at x=2 spanning a wide z range, with a
    // single one-block gap at z=0.
    let mut world = GridWorld::flat(63);
    for z in -10..=10 {
        if z != 0 {
            world.solids.insert((2, 64, z));
        }
    }
    let start = PathStart::grounded(0.5, 64.0, 0.5);
    let target = [BlockPos::new(4, 64, 0)];

    let narrow = MobShape::land(0.6, 1.8); // cell_width 1: fits the 1-wide gap
    let narrow_path = finder()
        .find_path(&world, &narrow, start, &target, params())
        .expect("narrow mob should find a path");
    assert!(narrow_path.reached());
    assert!(
        contains_node(narrow_path.nodes(), 2, 64, 0),
        "narrow mob should walk straight through the gap at (2,64,0)"
    );

    let wide = MobShape::land(1.4, 1.8); // cell_width 2: cannot fit a 1-wide gap
    let wide_path = finder().find_path(&world, &wide, start, &target, params());
    // Either it fails to reach, or it detours — but it must never squeeze
    // through the one-wide gap, because its body occupies two cells in z.
    let squeezed = wide_path
        .as_ref()
        .is_some_and(|p| contains_node(p.nodes(), 2, 64, 0) && p.reached());
    assert!(
        !squeezed,
        "wide mob must not fit through a gap narrower than its body"
    );
}

#[test]
fn mob_steps_up_one_block() {
    // Ground rises by one block for x >= 3 (top 64, so its surface is y=65).
    let mut world = GridWorld::flat(63);
    for x in 3..=6 {
        for z in -2..=2 {
            world.columns.insert((x, z), 64);
        }
    }
    let mob = MobShape::land(0.9, 0.9);
    let start = PathStart::grounded(0.5, 64.0, 0.5);
    let path = finder()
        .find_path(&world, &mob, start, &[BlockPos::new(5, 65, 0)], params())
        .expect("mob should path up a one-block step");
    assert!(path.reached());
    assert!(
        path.nodes().iter().any(|n| n.y == 65),
        "path must climb onto the raised ground (y=65)"
    );
    let last = path.end_node().unwrap();
    assert_eq!(last.y, 65);
}

#[test]
fn mob_drops_down_within_fall_limit() {
    // Ground drops by three blocks for x >= 3 (top 60, surface y=61); a
    // default land mob tolerates a three-block fall.
    let mut world = GridWorld::flat(63);
    for x in 3..=6 {
        for z in -2..=2 {
            world.columns.insert((x, z), 60);
        }
    }
    let mob = MobShape::land(0.9, 0.9);
    let start = PathStart::grounded(0.5, 64.0, 0.5);
    let path = finder()
        .find_path(&world, &mob, start, &[BlockPos::new(5, 61, 0)], params())
        .expect("mob should path down a three-block drop");
    assert!(path.reached());
    assert!(
        path.nodes().iter().any(|n| n.y == 61),
        "path must drop onto the lower ground (y=61)"
    );
}

#[test]
fn unreachable_target_returns_best_effort_not_reached() {
    // Wall the target off with a solid ring so it can never be reached.
    let mut world = GridWorld::flat(63);
    for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
        for y in 64..=66 {
            world.solids.insert((5 + dx, y, dz));
        }
    }
    let mob = MobShape::land(0.9, 0.9);
    let start = PathStart::grounded(0.5, 64.0, 0.5);
    let path = finder().find_path(&world, &mob, start, &[BlockPos::new(5, 64, 0)], params());
    if let Some(p) = path {
        assert!(
            !p.reached(),
            "a walled-off target must not report as reached"
        );
    }
}
