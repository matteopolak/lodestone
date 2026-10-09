use super::*;

/// A stone-topped strip 80 blocks long whose floor blocks are `floor`.
fn strip(floor: &str) -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..16 {
        for x in 0..80 {
            world.set_block(x, 0, z, floor);
        }
    }
    world
}

fn run(sim: &mut MobSim<'_>, world: &ChunkWorld, id: i32, ticks: usize) -> Vec<f64> {
    let mut steps = Vec::new();
    let mut last = sim.get(id).expect("spawned").position();
    for _ in 0..ticks {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        let here = sim.get(id).expect("alive").position();
        steps.push((here.x - last.x).hypot(here.z - last.z));
        last = here;
    }
    steps
}

fn chase(floor: &str, y: f64, species: &str, ticks: usize) -> Vec<f64> {
    let world = strip(floor);
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(species.parse().expect("valid key"), Vec3::new(2.5, y, 8.5))
        .id();
    sim.set_players(vec![PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(30.5, y, 8.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, -1.0),
        },
    }]);
    run(&mut sim, &world, id, ticks)
}

/// A chasing zombie (`movement_speed` 0.23, melee modifier 1.0) cruises at
/// `0.23^2 / (1 - 0.6 * 0.91)` blocks per tick; a live zombie was measured at
/// about 0.118 against this 0.1165.
#[test]
fn a_chasing_zombie_cruises_at_the_squared_speed() {
    let steps = chase("minecraft:stone", 1.0, "minecraft:zombie", 50);
    let expected = 0.23_f64 * 0.23 / (1.0 - 0.6 * 0.91);
    let cruise = steps[40];
    assert!((cruise - expected).abs() < 0.001, "step {cruise} vs {expected}");
}

/// Soul sand scales the velocity retained each tick by 0.4, so the same chase
/// cruises at `0.23^2 / (1 - 0.4 * 0.6 * 0.91)`.
#[test]
fn soul_sand_slows_a_chase_by_its_speed_factor() {
    let steps = chase("minecraft:soul_sand", 0.875, "minecraft:zombie", 50);
    let expected = 0.23_f64 * 0.23 / (1.0 - 0.4 * 0.6 * 0.91);
    let cruise = steps[40];
    assert!((cruise - expected).abs() < 0.001, "step {cruise} vs {expected}");
}

/// Ice (friction 0.98) cube-scales the thrust down and keeps more velocity:
/// `0.23^2 * (0.216 / 0.98^3) / (1 - 0.98 * 0.91)`.
#[test]
fn ice_cruises_at_the_slippery_closed_form() {
    let steps = chase("minecraft:ice", 1.0, "minecraft:zombie", 160);
    let thrust = 0.23_f64 * 0.23 * (0.216 / 0.98_f64.powi(3));
    let expected = thrust / (1.0 - 0.98 * 0.91);
    let cruise = steps[150];
    assert!((cruise - expected).abs() < 0.002, "step {cruise} vs {expected}");
}

/// A hurt cow flees at modifier 2.0, so its push is `(2.0 * 0.2)^2`, four times
/// the walking push, and no tick ever exceeds the `0.16 / 0.454` cruise.
#[test]
fn a_panicking_cow_moves_at_the_doubled_speed_squared() {
    let world = strip("minecraft:stone");
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(40.5, 1.0, 8.5))
        .id();
    sim.get_mut(id).expect("spawned").mob.note_hurt(None);
    let steps = run(&mut sim, &world, id, 30);
    let cruise = (0.4_f64 * 0.4) / (1.0 - 0.6 * 0.91);
    let fastest = steps.iter().copied().fold(0.0, f64::max);
    assert!(fastest <= cruise + 1e-6, "{fastest} exceeds the cruise {cruise}");
    assert!(fastest > 0.8 * cruise, "{fastest} never approached the cruise {cruise}");
}
