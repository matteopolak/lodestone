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
    zombies_alight_in(day_time, roof, 0.0)
}

fn zombies_alight_in(day_time: i32, roof: bool, rain_level: f32) -> usize {
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
    sim.set_environment(crate::dimension::Dimension::Overworld, rain_level, 0.0);
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

/// Rain wets a mob under open sky: of 400 zombies at noon none catches fire in
/// a downpour (the same 400 average 16 in clear weather), and one already alight
/// is put out within a tick.
#[test]
fn rain_stops_the_sun_igniting_zombies_and_puts_out_a_burning_one() {
    assert_eq!(zombies_alight_in(6000, false, 1.0), 0);
    assert!(zombies_alight_in(6000, false, 0.0) >= 6);

    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..32 {
        for x in 0..32 {
            world.set_block(x, 0, z, "minecraft:stone");
        }
    }
    let alight_after_two_ticks = |rain_level: f32| {
        let mut sim = MobSim::new(&world);
        sim.set_day_time(18000);
        sim.set_environment(crate::dimension::Dimension::Overworld, rain_level, 0.0);
        let id = sim.spawn_species("minecraft:skeleton".parse().expect("valid key"), Vec3::new(8.5, 1.0, 8.5)).id();
        sim.get_mut(id).expect("alive").ignite_for_seconds(8.0);
        for _ in 0..2 {
            sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        }
        sim.get(id).expect("alive").is_on_fire()
    };
    assert!(alight_after_two_ticks(0.0));
    assert!(!alight_after_two_ticks(1.0));
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
    for _ in 0..400 {
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

/// A bee flies paths through open air under no gravity: with a floor 30 blocks
/// below it never settles, it wanders away from where it started, and thrust of
/// 0.02 scaled by the 0.6 flying speed limits a level flight to
/// `0.012 / (1 - 0.91)` blocks per tick.
#[test]
fn a_bee_flies_and_wanders() {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..64 {
        for x in 0..64 {
            world.set_block(x, 0, z, "minecraft:stone");
        }
    }
    let mut sim = MobSim::new(&world);
    sim.set_day_time(6000);
    let start = Vec3::new(32.5, 30.0, 32.5);
    let id = sim.spawn_species("minecraft:bee".parse().expect("valid key"), start).id();
    let (mut lowest, mut farthest, mut fastest) = (f64::MAX, 0.0_f64, 0.0_f64);
    let mut last = start;
    for _ in 0..800 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        let p = sim.get(id).expect("alive").position();
        lowest = lowest.min(p.y);
        farthest = farthest.max(((p.x - start.x).powi(2) + (p.z - start.z).powi(2)).sqrt());
        fastest = fastest.max(((p.x - last.x).powi(2) + (p.y - last.y).powi(2) + (p.z - last.z).powi(2)).sqrt());
        last = p;
    }
    assert!(lowest > 10.0, "the bee sank to y {lowest}");
    assert!(farthest >= 4.0, "only {farthest} blocks from its start");
    assert!(fastest > 0.05 && fastest < 0.2, "fastest step {fastest}");
}

/// A bat world: a stone floor at y=0, and optionally a stone ceiling at y=11.
fn bat_world(ceiling: bool) -> ChunkWorld {
    bat_world_with(ceiling.then_some("minecraft:stone"))
}

fn bat_world_with(ceiling: Option<&str>) -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..64 {
        for x in 0..64 {
            world.set_block(x, 0, z, "minecraft:stone");
            if let Some(block) = ceiling {
                world.set_block(x, 11, z, block);
            }
        }
    }
    world
}

/// A bat under a full-cube ceiling hangs motionless with its head against the
/// block: `floor(10.0) + 1 - 0.9` is 10.1.
#[test]
fn a_bat_hangs_from_a_ceiling_until_a_player_comes_within_four_blocks() {
    let world = bat_world(true);
    let mut sim = MobSim::new(&world);
    let start = Vec3::new(32.5, 10.0, 32.5);
    let id = sim.spawn_species("minecraft:bat".parse().expect("valid key"), start).id();
    for _ in 0..200 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
    }
    let hung = sim.get(id).expect("alive").position();
    assert!((hung.y - 10.1).abs() < 1e-6 && (hung.x - start.x).abs() < 1e-9, "{hung:?}");
    assert!(sim.get(id).expect("alive").is_resting());
    assert!(sim.get(id).expect("alive").snapshot().metadata.contains(&MetadataField::BatResting(true)));

    sim.set_players(vec![PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(35.5, 10.0, 32.5),
            held_item: None,
            view_direction: Vec3::new(-1.0, 0.0, 0.0),
        },
    }]);
    let mut woke = false;
    for _ in 0..10 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        woke |= !sim.get(id).expect("alive").is_resting();
    }
    assert!(woke, "a player three blocks away wakes it");
}

/// Away from any ceiling a bat flutters: it wakes at once, never touches the
/// floor, and wanders. Its velocity eases toward half a block per tick, so no
/// step is longer than that plus the 0.01 forward thrust.
#[test]
fn a_bat_in_the_open_flutters_and_wanders() {
    let world = bat_world(false);
    let mut sim = MobSim::new(&world);
    let start = Vec3::new(32.5, 20.0, 32.5);
    let id = sim.spawn_species("minecraft:bat".parse().expect("valid key"), start).id();
    let (mut lowest, mut farthest, mut fastest) = (f64::MAX, 0.0_f64, 0.0_f64);
    let mut last = start;
    for _ in 0..400 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        let p = sim.get(id).expect("alive").position();
        lowest = lowest.min(p.y);
        farthest = farthest.max(((p.x - start.x).powi(2) + (p.z - start.z).powi(2)).sqrt());
        fastest = fastest.max(((p.x - last.x).powi(2) + (p.y - last.y).powi(2) + (p.z - last.z).powi(2)).sqrt());
        last = p;
    }
    assert!(!sim.get(id).expect("alive").is_resting());
    assert!(lowest > 1.0, "the bat sank to y {lowest}");
    assert!(farthest >= 3.0, "only {farthest} blocks from its start");
    assert!(fastest > 0.05 && fastest < 0.9, "fastest step {fastest}");
}

/// A phantom with a player below dives and reaches the player's body, climbs
/// away to circle, and dives again: each dive ends on contact, which drops the
/// target, and the scan finds the player again within a few seconds.
#[test]
fn a_phantom_swoops_down_on_a_player_again_and_again() {
    let world = bat_world(false);
    let mut sim = MobSim::new(&world);
    sim.set_day_time(18000);
    let id = sim.spawn_species("minecraft:phantom".parse().expect("valid key"), Vec3::new(32.5, 26.0, 32.5)).id();
    sim.set_players(vec![PerceivedPlayer {
        identity: Some(PlayerIdentity { uuid: uuid::Uuid::from_u128(7), entity_id: 42 }),
        perception: PlayerPerception {
            position: Vec3::new(32.5, 1.0, 32.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, -1.0),
        },
    }]);
    let (mut dives, mut near, mut hits) = (0, false, Vec::new());
    for _ in 0..2400 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        hits.extend(sim.take_player_hits());
        let p = sim.get(id).expect("alive").position();
        let distance = ((p.x - 32.5).powi(2) + (p.y - 1.9).powi(2) + (p.z - 32.5).powi(2)).sqrt();
        if distance < 2.0 && !near {
            dives += 1;
            near = true;
        } else if distance > 10.0 {
            near = false;
        }
    }
    assert!(dives >= 2, "only {dives} dives");
    assert!(hits.len() >= 2, "{} hits", hits.len());
    assert!(hits.iter().all(|h| (h.raw_damage - 6.0).abs() < 1e-4), "{:?}", hits.iter().map(|h| h.raw_damage).collect::<Vec<_>>());
}

/// A beach: stone floor at y=0 that climbs one block per column from x=27 to a
/// plateau of height 3 from x=29 on, with water filling every column below it
/// up to y=3 (so the shore is x<=28, land standing at y=4).
fn beach_world() -> ChunkWorld {
    beach_world_at(0)
}

/// The beach with its floor at `base`, so its water surface sits at
/// `base + 3`.
fn beach_world_at(base: i32) -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..64 {
        for x in 0..64 {
            let top = base + (x - 26).clamp(0, 3);
            for y in base..=top {
                world.set_block(x, y, z, "minecraft:stone");
            }
            if x <= 28 {
                for y in (top + 1)..=(base + 3) {
                    world.set_block(x, y, z, "minecraft:water");
                }
            }
        }
    }
    world
}

fn in_beach_water(p: Vec3) -> bool {
    p.x < 29.0 && p.y < 3.9
}

/// A turtle on the plateau finds water two blocks under its feet seven blocks
/// away (spiral ring 7), walks down the steps and swims; one placed beyond the
/// 24-block search never sees water and stays on land.
#[test]
fn a_turtle_ashore_walks_to_the_water_and_one_out_of_range_does_not() {
    let world = beach_world();
    let reaches = |x: f64| {
        let mut sim = MobSim::new(&world);
        sim.set_day_time(6000);
        let id = sim.spawn_species("minecraft:turtle".parse().expect("valid key"), Vec3::new(x, 4.0, 32.5)).id();
        let mut entered = false;
        for _ in 0..600 {
            sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
            entered |= in_beach_water(sim.get(id).expect("alive").position());
        }
        entered
    };
    assert!(reaches(34.5));
    assert!(!reaches(60.5));
}

/// A drowned out of water in daylight picks a water cell within ten blocks
/// and walks to it; at night the same drowned is not drawn to water.
#[test]
fn a_drowned_in_daylight_walks_to_the_water_and_at_night_does_not() {
    let world = beach_world();
    let enters = |time: i32| {
        let mut sim = MobSim::new(&world);
        sim.set_day_time(time);
        let id = sim.spawn_species("minecraft:drowned".parse().expect("valid key"), Vec3::new(34.5, 4.0, 32.5)).id();
        let mut entered = false;
        for _ in 0..300 {
            sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
            entered |= in_beach_water(sim.get(id).expect("alive").position());
        }
        entered
    };
    assert!(enters(6000));
    assert!(!enters(18000));
}

/// A walled pond, 23 by 23 blocks and 4 deep, with open air above its rim.
fn pond_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 20..=44 {
        for x in 20..=44 {
            let wall = x == 20 || x == 44 || z == 20 || z == 44;
            for y in 0..=5 {
                if y == 0 || wall {
                    world.set_block(x, y, z, "minecraft:stone");
                } else if y <= 4 {
                    world.set_block(x, y, z, "minecraft:water");
                }
            }
        }
    }
    world
}

/// An axolotl in a pond swims under no gravity without leaving the water or
/// sinking to the floor, wandering away from where it began. Its idle swim
/// speed of 0.5 against an attribute of 1.0 makes the move speed 0.05 and the
/// input 0.5, so thrust is 0.025 a tick and a step is bounded by
/// `0.025 / (1 - 0.9)`.
#[test]
fn an_axolotl_swims_and_wanders_in_a_pond() {
    let world = pond_world();
    let mut sim = MobSim::new(&world);
    let start = Vec3::new(32.5, 2.5, 32.5);
    let id = sim.spawn_species("minecraft:axolotl".parse().expect("valid key"), start).id();
    let (mut lowest, mut highest, mut farthest, mut fastest) = (f64::MAX, 0.0_f64, 0.0_f64, 0.0_f64);
    let mut last = start;
    for _ in 0..800 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        let p = sim.get(id).expect("alive").position();
        lowest = lowest.min(p.y);
        highest = highest.max(p.y);
        farthest = farthest.max(((p.x - start.x).powi(2) + (p.z - start.z).powi(2)).sqrt());
        fastest = fastest.max(((p.x - last.x).powi(2) + (p.y - last.y).powi(2) + (p.z - last.z).powi(2)).sqrt());
        last = p;
    }
    assert!(lowest >= 1.0 && highest <= 5.05, "left the water: {lowest}..{highest}");
    assert!(farthest >= 3.0, "only {farthest} blocks from its start");
    assert!(fastest > 0.1 && fastest <= 0.251, "fastest step {fastest}");
}

/// A frog is buoyant: each tick in water adds 0.005 upward, which 0.9 drag
/// settles at `0.005 / (1 - 0.9)` = 0.05 blocks a tick, so a frog released at
/// the bottom of a pond (surface at y 5) spends its time near the top while
/// it wanders; a body without the push averages mid-depth.
#[test]
fn a_frog_in_a_pond_floats_near_the_surface() {
    let world = pond_world();
    let mut sim = MobSim::new(&world);
    let id = sim.spawn_species("minecraft:frog".parse().expect("valid key"), Vec3::new(32.5, 1.5, 32.5)).id();
    let mut sum = 0.0;
    for i in 0..500 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        if i >= 200 {
            sum += sim.get(id).expect("alive").position().y;
        }
    }
    let mean = sum / 300.0;
    assert!(mean >= 4.4, "mean height {mean}");
}

/// Two sims built the same way must trace bit-identical paths: a mob's
/// randomness comes from its id, never from its uuid, a hasher or a clock.
#[test]
fn a_mob_trajectory_is_a_pure_function_of_its_spawn() {
    let trace = || {
        let world = pond_world();
        let mut sim = MobSim::new(&world);
        let ids: Vec<i32> = ["minecraft:frog", "minecraft:axolotl", "minecraft:turtle"]
            .iter()
            .map(|k| sim.spawn_species(k.parse().expect("valid key"), Vec3::new(32.5, 1.5, 32.5)).id())
            .collect();
        let mut out = Vec::new();
        for _ in 0..400 {
            sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
            for &id in &ids {
                let p = sim.get(id).expect("alive").position();
                out.push((p.x.to_bits(), p.y.to_bits(), p.z.to_bits()));
            }
        }
        out
    };
    assert!(trace() == trace(), "identical spawns diverged");
}

fn viewed(x: f64, y: f64, z: f64, entity_id: i32) -> PerceivedPlayer {
    PerceivedPlayer {
        identity: Some(PlayerIdentity { uuid: uuid::Uuid::from_u128(entity_id as u128), entity_id }),
        perception: PlayerPerception {
            position: Vec3::new(x, y, z),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, -1.0),
        },
    }
}

/// The first target a phantom at (8.5, 26, 32.5) picks at night among `players`.
fn phantom_first_target(players: Vec<PerceivedPlayer>) -> Option<Vec3> {
    let world = bat_world(false);
    let mut sim = MobSim::new(&world);
    sim.set_day_time(18000);
    let id = sim.spawn_species("minecraft:phantom".parse().expect("valid key"), Vec3::new(8.5, 26.0, 32.5)).id();
    sim.set_players(players);
    for _ in 0..400 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        if let Some(target) = sim.get(id).expect("alive").attack_target() {
            return Some(target);
        }
    }
    None
}

/// Of two players in reach a phantom takes the higher even when the lower is
/// nearer: the one at y 12 is 14 blocks away, the one at y 40 is 17.2.
#[test]
fn a_phantom_targets_the_highest_player_not_the_nearest() {
    let target = phantom_first_target(vec![viewed(8.5, 12.0, 32.5, 1), viewed(18.5, 40.0, 32.5, 2)]);
    assert_eq!(target.map(|t| t.y), Some(40.0));
}

/// The scan box is 16 blocks wide each way horizontally: a player 20 away is
/// never taken, one 10 away is.
#[test]
fn a_phantom_ignores_players_beyond_sixteen_blocks_horizontally() {
    assert_eq!(phantom_first_target(vec![viewed(60.5, 20.0, 32.5, 1)]), None);
    assert!(phantom_first_target(vec![viewed(18.5, 20.0, 32.5, 1)]).is_some());
}

/// Hits a phantom lands on a player over 2400 night ticks with `cats` cats
/// standing beside the player.
fn phantom_hits(cats: usize) -> usize {
    let world = bat_world(false);
    let mut sim = MobSim::new(&world);
    sim.set_day_time(18000);
    sim.spawn_species("minecraft:phantom".parse().expect("valid key"), Vec3::new(32.5, 26.0, 32.5));
    for i in 0..cats {
        sim.spawn_species("minecraft:cat".parse().expect("valid key"), Vec3::new(33.5 + i as f64, 1.0, 32.5));
    }
    sim.set_players(vec![viewed(32.5, 1.0, 32.5, 42)]);
    let mut hits = 0;
    for _ in 0..2400 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        hits += sim.take_player_hits().len();
    }
    hits
}

/// A cat within 16 blocks ends a swoop whenever the tick count is a multiple of
/// twenty, so a phantom with cats about lands fewer dives.
#[test]
fn a_cat_near_the_player_scares_a_phantom_off_its_dives() {
    let without = phantom_hits(0);
    let with = phantom_hits(1);
    assert!(without >= 3, "{without} hits without a cat");
    assert!(with < without, "{with} hits with a cat, {without} without");
}

/// A ghast faces a target within 64 blocks: its yaw is the bearing to the
/// target, `-atan2(dx, dz)` (south is 0, east -90), not its drift heading.
#[test]
fn a_ghast_turns_to_face_its_target() {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..64 {
        for x in 0..64 {
            world.set_block(x, 0, z, "minecraft:stone");
        }
    }
    let mut sim = MobSim::new(&world);
    let id = sim.spawn_species("minecraft:ghast".parse().expect("valid key"), Vec3::new(8.5, 4.0, 32.5)).id();
    sim.set_players(vec![viewed(40.5, 4.0, 32.5, 1)]);
    for _ in 0..200 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
    }
    let mob = sim.get(id).expect("alive");
    assert!(mob.attack_target().is_some(), "no target acquired");
    let (here, target) = (mob.position(), mob.attack_target().expect("target"));
    let expected = -(target.x - here.x).atan2(target.z - here.z).to_degrees();
    let yaw = f64::from(mob.rotation().yaw);
    assert!((yaw - expected).abs() < 1.0, "yaw {yaw}, facing the target is {expected}");
}

/// A skeleton under a roof 4 blocks wide, in daylight, never leaves it by
/// wandering: paths stop short of the first sunlit waypoint. At night the same
/// skeleton does leave.
#[test]
fn a_skeleton_in_daylight_wanders_only_under_cover() {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..32 {
        for x in 0..32 {
            world.set_block(x, 0, z, "minecraft:stone");
            if (12..20).contains(&x) && (4..28).contains(&z) {
                world.set_block(x, 4, z, "minecraft:stone");
            }
        }
    }
    // Beyond a skeleton's 16-block follow range, inside the 32 that keep it awake.
    let leavers = |day_time: i32| {
        let mut sim = MobSim::new(&world);
        sim.set_day_time(day_time);
        sim.set_players(vec![viewed(15.5, 1.0, 40.5, 9)]);
        let ids: Vec<i32> = (0..8)
            .map(|i| {
                let at = Vec3::new(15.5, 1.0, 6.5 + f64::from(i) * 2.5);
                sim.spawn_species("minecraft:skeleton".parse().expect("valid key"), at).id()
            })
            .collect();
        let mut left = [false; 8];
        for _ in 0..3000 {
            sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
            for (flag, &id) in left.iter_mut().zip(&ids) {
                if let Some(m) = sim.get(id) {
                    *flag |= !(11.5..20.5).contains(&m.position().x);
                }
            }
        }
        left.iter().filter(|&&l| l).count()
    };
    assert_eq!(leavers(6000), 0, "skeletons walked out into the sun");
    assert!(leavers(18000) >= 3, "only {} of 8 left the roof at night", leavers(18000));
}

/// A 3x3 tank of water two blocks deep on a stone floor, walled in.
fn tank_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..64 {
        for x in 0..64 {
            world.set_block(x, 0, z, "minecraft:stone");
            let inside = (31..=33).contains(&x) && (31..=33).contains(&z);
            let wall = (30..=34).contains(&x) && (30..=34).contains(&z) && !inside;
            for y in 1..=3 {
                if inside {
                    world.set_block(x, y, z, "minecraft:water");
                } else if wall {
                    world.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
    }
    world
}

fn puff_after(ticks: usize, scare: Option<&str>) -> (i32, Vec<MetadataField>) {
    let world = tank_world();
    let mut sim = MobSim::new(&world);
    let id = sim.spawn_species("minecraft:pufferfish".parse().expect("valid key"), Vec3::new(32.5, 1.5, 32.5)).id();
    if let Some(species) = scare {
        sim.spawn_species(format!("minecraft:{species}").parse().expect("valid key"), Vec3::new(32.5, 2.2, 32.5));
    }
    for _ in 0..ticks {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
    }
    let metadata = sim.get(id).expect("alive").snapshot().metadata;
    let state = metadata.iter().find_map(|m| if let MetadataField::PuffState(s) = m { Some(*s) } else { None });
    (state.expect("a pufferfish always reports its puff"), metadata)
}

/// A pufferfish puffs up when a mob it fears is within 2 blocks, and stays
/// small beside a mob on the jar's ignore list or alone. A zombie in the tank
/// is 0.7 blocks away; a cod is equally close.
#[test]
fn a_pufferfish_puffs_for_a_scary_mob_but_not_for_a_cod() {
    assert_eq!(puff_after(100, Some("cod")).0, 0, "a cod does not scare it");
    assert_eq!(puff_after(100, None).0, 0, "alone it stays small");
    let (state, metadata) = puff_after(100, Some("zombie"));
    assert_eq!(state, 2, "41 inflating ticks reach full");
    assert!(metadata.contains(&MetadataField::PuffState(2)));
}

/// A puffed fish stings a player touching it for `1 + state` damage and
/// `60 * state` ticks of poison; a small one does not sting.
#[test]
fn a_puffed_pufferfish_stings_a_player_touching_it() {
    let world = tank_world();
    let mut sim = MobSim::new(&world);
    sim.spawn_species("minecraft:pufferfish".parse().expect("valid key"), Vec3::new(32.5, 1.5, 32.5));
    sim.set_players(vec![viewed(32.5, 1.0, 32.5, 77)]);
    let mut hits = Vec::new();
    for _ in 0..3 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        hits.extend(sim.take_player_hits());
    }
    let first = hits.first().expect("a sting");
    assert_eq!((first.raw_damage, first.poison_ticks), (2.0, 60), "mid puff");
}

/// Six cod released in one corner of a pond organise into a school within the
/// first minutes: some follow a leader, every school stays within the cap of 8
/// and a leader's size equals itself plus the fish naming it. Six pufferfish in
/// the same spot (which do not school) form no links.
#[test]
fn cod_in_a_pond_form_a_school_and_pufferfish_do_not() {
    let census = |species: &str| {
        let world = pond_world();
        let mut sim = MobSim::new(&world);
        let ids: Vec<i32> = (0..6)
            .map(|i| {
                let at = Vec3::new(30.5 + f64::from(i % 3) * 1.5, 2.5, 30.5 + f64::from(i / 3) * 1.5);
                sim.spawn_species(format!("minecraft:{species}").parse().expect("valid key"), at).id()
            })
            .collect();
        sim.set_players(vec![viewed(32.5, 1.0, 36.5, 5)]);
        for _ in 0..1500 {
            sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        }
        let followers: Vec<(i32, i32)> =
            ids.iter().filter_map(|&id| sim.get(id).and_then(|m| m.school_leader()).map(|l| (id, l))).collect();
        for &(_, leader) in &followers {
            let named = followers.iter().filter(|&&(_, l)| l == leader).count() as i32;
            let size = sim.get(leader).expect("leader alive").school_size();
            assert_eq!(size, named + 1, "leader {leader}'s size counts itself and its followers");
            assert!(size <= 8);
        }
        for &(id, leader) in &followers {
            let (a, b) = (sim.get(id).expect("alive").position(), sim.get(leader).expect("alive").position());
            let d = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt();
            assert!(d < 13.0, "follower {id} is {d} blocks from its leader");
        }
        followers.len()
    };
    assert!(census("cod") >= 3, "only {} cod follow a leader", census("cod"));
    assert_eq!(census("pufferfish"), 0);
}

/// A drowned floating at the surface of a sea (y 63) comes ashore at night to a
/// standable block with two empty cells above it; in daylight it stays in the
/// water. Ashore means standing on the plateau (x >= 29) at feet y 64.
#[test]
fn a_drowned_at_the_surface_comes_ashore_at_night_but_not_by_day() {
    let world = beach_world_at(60);
    let ashore = |time: i32| {
        let mut sim = MobSim::new(&world);
        sim.set_day_time(time);
        let id = sim.spawn_species("minecraft:drowned".parse().expect("valid key"), Vec3::new(26.5, 62.0, 32.5)).id();
        let mut landed = false;
        for _ in 0..900 {
            sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
            let p = sim.get(id).expect("alive").position();
            landed |= p.x >= 29.0 && p.y >= 63.9;
        }
        landed
    };
    assert!(ashore(18000));
    assert!(!ashore(6000));
}

/// A turtle carried 80 blocks from the beach it was born on walks back until
/// within 7 blocks of it (the 1/700 draw per goal tick fires within a few
/// hundred ticks); the test requires it to get within 20 blocks.
#[test]
fn a_turtle_far_from_its_nest_walks_home() {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..40 {
        for x in 0..200 {
            world.set_block(x, 0, z, "minecraft:stone");
        }
    }
    let mut sim = MobSim::new(&world);
    sim.set_day_time(6000);
    let id = sim.spawn_species("minecraft:turtle".parse().expect("valid key"), Vec3::new(20.5, 1.0, 20.5)).id();
    sim.get_mut(id).expect("alive").teleport_to(Vec3::new(100.5, 1.0, 20.5));
    sim.set_players(vec![viewed(100.5, 1.0, 40.5, 3)]);
    let mut closest = f64::MAX;
    for _ in 0..8000 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        closest = closest.min(sim.get(id).expect("alive").position().x - 20.5);
    }
    assert!(closest < 20.0, "closest approach to the nest {closest}");
}

/// A bat hangs from a ceiling that conducts redstone and not from one that is
/// a full cube yet does not (glass), nor one that is not a full cube (a slab).
#[test]
fn a_bat_roosts_only_under_a_block_that_conducts_redstone() {
    let rests = |ceiling: &str| {
        let world = bat_world_with(Some(ceiling));
        let mut sim = MobSim::new(&world);
        let id = sim.spawn_species("minecraft:bat".parse().expect("valid key"), Vec3::new(32.5, 10.0, 32.5)).id();
        for _ in 0..200 {
            sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        }
        sim.get(id).expect("alive").is_resting()
    };
    assert!(rests("minecraft:stone"));
    assert!(!rests("minecraft:glass"), "glass is a full cube that does not conduct");
    assert!(!rests("minecraft:oak_slab[type=bottom,waterlogged=false]"));
}
