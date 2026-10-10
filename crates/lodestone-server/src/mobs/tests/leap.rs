use super::*;

fn floor() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -20..40 {
        for z in -20..40 {
            world.set_block(x, 0, z, "minecraft:stone");
        }
    }
    world
}

/// The highest the mob rises above the floor (y = 1) while closing on a target
/// 6 blocks away.
fn peak_height(species: &str) -> f64 {
    let world = floor();
    let mut sim = MobSim::new(&world);
    sim.set_day_time(18000);
    let m = sim.spawn_species(format!("minecraft:{species}").parse().expect("key"), Vec3::new(0.5, 1.0, 0.5));
    m.set_attack_target(Some(Vec3::new(6.5, 1.0, 0.5)));
    let id = m.id();
    sim.set_players(vec![PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(6.5, 1.0, 0.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, -1.0),
        },
    }]);
    let mut peak = 0.0_f64;
    for _ in 0..120 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        if let Some(m) = sim.get(id) {
            peak = peak.max(m.position().y - 1.0);
        }
    }
    peak
}

/// A spider pounces from 2 to 4 blocks out with a vertical velocity of 0.4
/// blocks per tick, which carries it more than 0.3 above the floor (a bare step
/// is 0); a zombie, which has no pounce, never leaves the floor.
#[test]
fn a_spider_leaps_at_its_target_and_a_zombie_does_not() {
    assert!(peak_height("spider") > 0.3, "{}", peak_height("spider"));
    assert!(peak_height("zombie") < 0.01, "{}", peak_height("zombie"));
}
