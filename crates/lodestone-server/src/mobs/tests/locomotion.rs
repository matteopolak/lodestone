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
    sim.set_day_time(18000);
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

const WALL_X: i32 = 10;
const WALL_OPENS_AT_Z: i32 = 14;

fn fence_state() -> lodestone_data::block_states::StateId {
    lodestone_data::block_states::StateId::from_state_str(
        "minecraft:oak_fence[north=true,south=true,east=false,west=false,waterlogged=false]",
    )
    .expect("fence state")
}

/// A zombie behind a stone wall does not see the player; once the live terrain
/// swaps the stone for a fence it sees over it, acquires, and walks around the
/// fence's end at the squared-speed cruise. The base world never has the wall:
/// only the live terrain closure does, so a route that honours it proves the
/// tick reads live terrain.
#[test]
fn a_zombie_ignores_a_walled_player_then_paths_around_a_fence_it_sees_over() {
    let mut base = ChunkWorld::new(-64, 384);
    for z in 0..32 {
        for x in 0..60 {
            base.set_block(x, 0, z, "minecraft:stone");
        }
    }
    let stone = base.block_state_id(0, 0, 0);
    let fence = fence_state();
    let wall = std::sync::Mutex::new(Some(stone));
    let terrain = |x: i32, y: i32, z: i32| {
        // Stone is three blocks tall; the fence is one block, whose 1.5-high
        // shape tops out below the eyes.
        let in_wall = x == WALL_X && (0..WALL_OPENS_AT_Z).contains(&z);
        match *wall.lock().unwrap() {
            Some(state) if in_wall && state == stone && (1..=3).contains(&y) => Some(state),
            Some(state) if in_wall && state == fence && y == 1 => Some(state),
            _ => Some(base.block_state_id(x, y, z)),
        }
    };

    let mut sim = MobSim::new(&base);
    sim.set_day_time(18000);
    let id = sim
        .spawn_species("minecraft:zombie".parse().expect("valid key"), Vec3::new(2.5, 1.0, 8.5))
        .id();
    sim.set_players(vec![PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(30.5, 1.0, 8.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, -1.0),
        },
    }]);

    for _ in 0..80 {
        sim.tick_with_terrain(&terrain);
        assert!(
            sim.get(id).expect("alive").attack_target().is_none(),
            "a stone wall blocks the line of sight"
        );
    }

    *wall.lock().unwrap() = Some(fence);
    let mut crossing_z = None;
    let mut steps = Vec::new();
    let mut last = sim.get(id).expect("alive").position();
    for _ in 0..600 {
        sim.tick_with_terrain(&terrain);
        let here = sim.get(id).expect("alive").position();
        steps.push((here.x - last.x).hypot(here.z - last.z));
        if crossing_z.is_none() && last.x < f64::from(WALL_X) && here.x >= f64::from(WALL_X) {
            crossing_z = Some(here.z);
        }
        last = here;
    }
    let crossing = crossing_z.expect("the zombie never got past the fence");
    assert!(
        crossing >= f64::from(WALL_OPENS_AT_Z),
        "the zombie crossed the wall line at z={crossing}, through the fence"
    );
    let cruise = 0.23_f64 * 0.23 / (1.0 - 0.6 * 0.91);
    let fastest = steps.iter().copied().fold(0.0, f64::max);
    assert!(fastest <= cruise * 1.02, "{fastest} exceeds the cruise {cruise}");
    assert!(fastest > 0.9 * cruise, "{fastest} never approached the cruise {cruise}");
}

/// Control: with no wall the same zombie acquires at once.
#[test]
fn the_same_zombie_with_a_clear_view_acquires() {
    let mut base = ChunkWorld::new(-64, 384);
    for z in 0..32 {
        for x in 0..60 {
            base.set_block(x, 0, z, "minecraft:stone");
        }
    }
    let mut sim = MobSim::new(&base);
    let id = sim
        .spawn_species("minecraft:zombie".parse().expect("valid key"), Vec3::new(2.5, 1.0, 8.5))
        .id();
    sim.set_players(vec![PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(30.5, 1.0, 8.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, -1.0),
        },
    }]);
    let mut acquired = false;
    for _ in 0..80 {
        sim.tick_with_terrain(&|x, y, z| Some(base.block_state_id(x, y, z)));
        acquired |= sim.get(id).expect("alive").attack_target().is_some();
    }
    assert!(acquired);
}

/// A mob never enters a column the terrain read reports absent. The control
/// reads the same floor everywhere and the zombie does cross the line.
#[test]
fn a_chasing_zombie_stops_at_the_edge_of_loaded_terrain() {
    let mut base = ChunkWorld::new(-64, 384);
    for z in 0..16 {
        for x in 0..60 {
            base.set_block(x, 0, z, "minecraft:stone");
        }
    }
    let furthest = |loaded_to: i32| {
        let mut sim = MobSim::new(&base);
        let m = sim.spawn(Vec3::new(10.5, 1.0, 8.5), MobShape::land(0.6, 1.95), 0.25, 400);
        m.add_goal(1, Box::new(lodestone_entity::ai::goals::MeleeAttackGoal::new(0.25, 2.0)));
        m.set_attack_target(Some(Vec3::new(40.5, 1.0, 8.5)));
        let id = m.id();
        let mut furthest = 0.0_f64;
        for _ in 0..300 {
            sim.tick_with_terrain(&|x, y, z| (x < loaded_to).then(|| base.block_state_id(x, y, z)));
            furthest = furthest.max(sim.get(id).expect("alive").position().x);
        }
        furthest
    };
    assert!(furthest(16) < 16.0, "walked into an absent column");
    assert!(furthest(60) > 16.0, "control: the same chase crosses x=16 on loaded ground");
}

/// A hunting mob spends health on a drop: 3 plus `(health - 33% of max)` less
/// 4 per difficulty step below Hard. A full-health 20/20 mob is (20 - 6.6) = 13
/// spare, so Peaceful 13 - 12 = 1 -> 4 blocks, Normal 13 - 4 = 9 -> 12 blocks.
/// The 6-block drop is crossed on Normal and refused on Peaceful.
#[test]
fn a_hunting_zombie_descends_a_drop_its_health_budget_allows() {
    let furthest_down = |difficulty| {
        let mut base = ChunkWorld::new(-64, 384);
        for z in 0..16 {
            for x in 0..10 {
                base.set_block(x, 0, z, "minecraft:stone");
            }
            for x in 10..30 {
                base.set_block(x, -6, z, "minecraft:stone");
            }
        }
        let mut sim = MobSim::new(&base);
        sim.set_difficulty(difficulty);
        let m = sim.spawn(Vec3::new(5.5, 1.0, 8.5), MobShape::land(0.6, 1.95), 0.25, 400);
        m.add_goal(1, Box::new(lodestone_entity::ai::goals::MeleeAttackGoal::new(0.25, 2.0)));
        m.set_attack_target(Some(Vec3::new(14.5, -5.0, 8.5)));
        let id = m.id();
        let mut lowest = f64::MAX;
        for _ in 0..300 {
            sim.tick_with_terrain(&|x, y, z| Some(base.block_state_id(x, y, z)));
            lowest = lowest.min(sim.get(id).expect("alive").position().y);
        }
        lowest
    };
    assert!(furthest_down(lodestone_model::Difficulty::Normal) < -4.0, "a 12-block budget crosses a 6-block drop");
    assert!(furthest_down(lodestone_model::Difficulty::Peaceful) > 0.0, "a 4-block budget refuses it");
}

/// A mob in a column that is not ticking does not idle: its no-action counter
/// holds, where a mob on loaded ground counts one per tick.
#[test]
fn only_a_ticking_mob_accumulates_idle_time() {
    let mut base = ChunkWorld::new(-64, 384);
    for z in 0..16 {
        for x in 0..16 {
            base.set_block(x, 0, z, "minecraft:stone");
        }
    }
    let idle_after = |loaded: bool| {
        let mut sim = MobSim::new(&base);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(8.5, 1.0, 8.5))
            .id();
        for _ in 0..30 {
            sim.tick_with_terrain(&|x, y, z| loaded.then(|| base.block_state_id(x, y, z)));
        }
        sim.get(id).expect("alive").no_action_time
    };
    assert_eq!(idle_after(false), 0);
    assert_eq!(idle_after(true), 30);
}

/// A cod in a closed 16x16x6 tank wanders the water: it travels well past a
/// stroll's first step, and every tick its body stays inside the water volume
/// (feet cell between the floor and the surface, column inside the walls).
#[test]
fn a_cod_swims_around_inside_its_tank() {
    // Water fills y 1..=6, so the surface is at 7. A fish carries its vertical
    // velocity through its last step, so it may breach the surface briefly before
    // gravity returns it.
    const SURFACE: f64 = 7.0;
    const COAST: f64 = 0.5;
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..16 {
        for x in 0..16 {
            world.set_block(x, 0, z, "minecraft:stone");
            for y in 1..=6 {
                world.set_block(x, y, z, "minecraft:water");
            }
        }
    }
    let mut sim = MobSim::new(&world);
    let start = Vec3::new(8.5, 1.5, 8.5);
    let id = sim.spawn_species("minecraft:cod".parse().expect("valid key"), start).id();
    // A player beyond the 8-block flee radius keeps the mob out of the idle
    // throttle, which silences strolling once no player has been near for 100 ticks.
    sim.set_players(vec![PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(8.5, 3.5, 30.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, -1.0),
        },
    }]);
    let mut farthest = 0.0_f64;
    let (mut low, mut high) = (f64::MAX, f64::MIN);
    for t in 0..1200 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        let p = sim.get(id).expect("alive").position();
        assert!(
            (0..16).contains(&(p.x.floor() as i32))
                && (0..16).contains(&(p.z.floor() as i32))
                && (1.0..SURFACE + COAST).contains(&p.y),
            "tick {t}: left the water at {p:?}"
        );
        farthest = farthest.max((p.x - start.x).hypot(p.z - start.z));
        (low, high) = (low.min(p.y), high.max(p.y));
    }
    assert!(farthest >= 3.0, "the cod only got {farthest} blocks from its start");
    // Destinations spread +-7 vertically, so a swimmer leaves the floor it
    // started near; a body that sinks and walks does not.
    assert!(high >= start.y + 2.0, "vertical range {low}..{high}");
}

/// A cod with a player 3 blocks away flees: within 3 seconds it is farther
/// from the player than the 3 blocks it started at (the reference fish base
/// flees players inside 8 blocks).
#[test]
fn a_cod_flees_a_nearby_player() {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..16 {
        for x in 0..16 {
            world.set_block(x, 0, z, "minecraft:stone");
            for y in 1..=6 {
                world.set_block(x, y, z, "minecraft:water");
            }
        }
    }
    let mut sim = MobSim::new(&world);
    let player = Vec3::new(8.5, 3.5, 5.5);
    let id = sim.spawn_species("minecraft:cod".parse().expect("valid key"), Vec3::new(8.5, 3.5, 8.5)).id();
    sim.set_players(vec![PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: player,
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }]);
    for _ in 0..60 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
    }
    let p = sim.get(id).expect("alive").position();
    let away = (p.x - player.x).hypot(p.z - player.z);
    assert!(away > 4.5, "the cod is {away} blocks from the player");
}

/// A squid in a tank pulses along a random heading. Each pulse pushes the full
/// 0.2 horizontal vector, so the fastest tick is exactly 0.2 blocks and none
/// is faster; across the run it travels well beyond its start.
#[test]
fn a_squid_pulses_through_its_tank_at_the_vector_speed() {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..32 {
        for x in 0..32 {
            world.set_block(x, 0, z, "minecraft:stone");
            for y in 1..=10 {
                world.set_block(x, y, z, "minecraft:water");
            }
        }
    }
    let mut sim = MobSim::new(&world);
    let start = Vec3::new(16.5, 5.0, 16.5);
    let id = sim.spawn_species("minecraft:squid".parse().expect("valid key"), start).id();
    sim.set_players(vec![PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(16.5, 5.0, 40.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, -1.0),
        },
    }]);
    let (mut fastest, mut farthest) = (0.0_f64, 0.0_f64);
    let mut last = start;
    for _ in 0..600 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        let p = sim.get(id).expect("alive").position();
        fastest = fastest.max((p.x - last.x).hypot(p.z - last.z));
        farthest = farthest.max((p.x - start.x).hypot(p.z - start.z));
        last = p;
    }
    assert!((fastest - 0.2).abs() < 1e-6, "fastest horizontal step {fastest}");
    assert!(farthest >= 3.0, "only {farthest} blocks from its start");
}

/// A spider chasing a player standing on a 4-high plateau climbs the face. A
/// climb rises 0.2 blocks per tick, so the fastest vertical step is exactly
/// 0.2, and it gets within biting reach of the player (feet above y 4) before
/// the attack goal stops the climb.
#[test]
fn a_spider_climbs_a_wall_to_reach_a_player() {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..16 {
        for x in 0..32 {
            world.set_block(x, 0, z, "minecraft:stone");
            if x >= 10 {
                for y in 1..=4 {
                    world.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
    }
    let mut sim = MobSim::new(&world);
    sim.set_day_time(18000);
    let id = sim.spawn_species("minecraft:spider".parse().expect("valid key"), Vec3::new(5.5, 1.0, 8.5)).id();
    sim.set_players(vec![PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(10.5, 5.0, 8.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, -1.0),
        },
    }]);
    let (mut fastest_rise, mut highest) = (0.0_f64, 0.0_f64);
    let mut last_y = 1.0;
    for _ in 0..400 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        let y = sim.get(id).expect("alive").position().y;
        fastest_rise = fastest_rise.max(y - last_y);
        highest = highest.max(y);
        last_y = y;
    }
    assert!((fastest_rise - 0.2).abs() < 1e-6, "fastest rise {fastest_rise}");
    assert!(highest >= 4.0, "the spider only reached y {highest}");
}

/// How many of 400 zombies are on fire after one tick on a stone floor.
fn zombies_alight(day_time: i32, roof: bool) -> usize {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..32 {
        for x in 0..32 {
            world.set_block(x, 0, z, "minecraft:stone");
            if roof {
                world.set_block(x, 4, z, "minecraft:stone");
            }
        }
    }
    let mut sim = MobSim::new(&world);
    sim.set_day_time(day_time);
    let ids: Vec<i32> = (0..400)
        .map(|i| {
            let at = Vec3::new(f64::from(i % 20) * 1.5 + 1.5, 1.0, f64::from(i / 20) * 1.5 + 1.5);
            sim.spawn_species("minecraft:zombie".parse().expect("valid key"), at).id()
        })
        .collect();
    sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
    ids.iter().filter(|&&id| sim.get(id).is_some_and(|m| m.is_on_fire())).count()
}

/// At noon under open sky each zombie ignites with probability
/// `(1 - 0.4) * 2 / 30 = 0.04` per tick, so 400 of them average 16 alight
/// (standard deviation 3.9). Night, a roof and a helmet each remove it.
#[test]
fn zombies_catch_fire_only_in_open_daylight() {
    let open = zombies_alight(6000, false);
    assert!((6..=28).contains(&open), "{open} of 400 alight at noon");
    assert_eq!(zombies_alight(18000, false), 0, "none should burn at night");
    assert_eq!(zombies_alight(6000, true), 0, "none should burn under a roof");
}

/// A burning skeleton in daylight with no target heads for the first spot the
/// sky does not light: here a 4-wide roofed patch within 10 blocks. Standing
/// still or wandering would leave it in the open; it must reach the roof.
#[test]
fn a_burning_skeleton_runs_for_shade() {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..32 {
        for x in 0..32 {
            world.set_block(x, 0, z, "minecraft:stone");
            if (16..20).contains(&x) && (4..28).contains(&z) {
                world.set_block(x, 4, z, "minecraft:stone");
            }
        }
    }
    let mut sim = MobSim::new(&world);
    sim.set_day_time(6000);
    let id = sim.spawn_species("minecraft:skeleton".parse().expect("valid key"), Vec3::new(12.5, 1.0, 14.5)).id();
    sim.get_mut(id).expect("alive").ignite_for_seconds(8.0);
    let mut sheltered = false;
    for _ in 0..120 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        sheltered |= (16.0..20.0).contains(&sim.get(id).expect("alive").position().x);
    }
    assert!(sheltered, "the skeleton never reached the roof");
}

/// A ghast floats: with a floor 30 blocks below it never settles onto it, and its
/// random wandering carries it away from where it started. A push is at most
/// `0.06 * 5 / 3 = 0.1` blocks per tick of velocity, so no tick moves it more
/// than a few tenths of a block.
#[test]
fn a_ghast_floats_and_wanders() {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..64 {
        for x in 0..64 {
            world.set_block(x, 0, z, "minecraft:stone");
        }
    }
    let mut sim = MobSim::new(&world);
    let start = Vec3::new(32.5, 30.0, 32.5);
    let id = sim.spawn_species("minecraft:ghast".parse().expect("valid key"), start).id();
    let (mut lowest, mut farthest, mut fastest) = (f64::MAX, 0.0_f64, 0.0_f64);
    let mut last = start;
    for _ in 0..600 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        let p = sim.get(id).expect("alive").position();
        lowest = lowest.min(p.y);
        farthest = farthest.max(((p.x - start.x).powi(2) + (p.z - start.z).powi(2)).sqrt());
        fastest = fastest.max(((p.x - last.x).powi(2) + (p.y - last.y).powi(2) + (p.z - last.z).powi(2)).sqrt());
        last = p;
    }
    assert!(lowest > 2.0, "the ghast sank to y {lowest}");
    assert!(farthest >= 4.0, "only {farthest} blocks from its start");
    assert!(fastest > 0.0 && fastest < 0.5, "fastest step {fastest}");
}
