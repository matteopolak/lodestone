//! The turtle egg cycle on the mob side: laying, trampling, zombies breaking
//! eggs, hatching and breeding.

use lodestone_entity::ai::{BlockEdit, BlockExpect, turtle_egg};

use super::*;

fn beach() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -20..40 {
        for z in -20..20 {
            world.set_block(x, 0, z, "minecraft:sand");
        }
    }
    world
}

fn key(species: &str) -> ResourceKey {
    format!("minecraft:{species}").parse().expect("key")
}

fn far_player() -> PerceivedPlayer {
    PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(60.5, 1.0, 0.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }
}

/// Runs `ticks` ticks, applying every block edit the mobs ask for to `world`
/// the way the tick loop does, and returns the edits seen.
fn run(sim: &mut MobSim<'_>, world: &mut ChunkWorld, ticks: u32) -> Vec<BlockEdit> {
    let mut seen = Vec::new();
    for _ in 0..ticks {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        for edit in sim.take_block_edits() {
            let (x, y, z) = edit.cell;
            let old = world.block_state_id(x, y, z);
            let holds = match edit.expect {
                BlockExpect::Air => turtle_egg::is_air(old),
                BlockExpect::State(s) => old == s,
                BlockExpect::Block(b) => old.block() == b,
            };
            if holds {
                world.set_block_id(x, y, z, edit.set.unwrap_or_else(crate::chunk::air_state));
                seen.push(edit);
            }
        }
    }
    seen
}

fn sim_for<'w>(world: &'w ChunkWorld) -> MobSim<'w> {
    let mut sim = MobSim::new(world);
    sim.set_day_time(18000);
    sim.set_players(vec![far_player()]);
    sim
}

/// A turtle carrying an egg, born on this beach, walks to sand near its nest,
/// digs, and leaves an egg block; one without an egg lays nothing.
#[test]
fn a_turtle_with_an_egg_lays_it_on_the_sand_near_its_nest() {
    let laid = |carrying: bool| {
        let base = beach();
        let mut world = beach();
        let mut sim = sim_for(&base);
        let turtle = sim.spawn_species(key("turtle"), Vec3::new(0.5, 1.0, 0.5));
        turtle.mob.restore_has_egg(carrying);
        let id = turtle.id();
        let edits = run(&mut sim, &mut world, 800);
        let placed = edits
            .iter()
            .filter(|e| e.set.is_some_and(|s| turtle_egg::counts(s).is_some()))
            .count();
        (placed, sim.get(id).expect("turtle").mob.carries_egg())
    };
    assert_eq!(laid(true), (1, false), "the egg is laid and no longer carried");
    assert_eq!(laid(false), (0, false), "control: no egg, nothing laid");
}

/// A zombie walks to a turtle egg and breaks it; with mob griefing off it
/// leaves it alone.
#[test]
fn a_zombie_breaks_a_turtle_egg_only_when_mobs_may_change_blocks() {
    let broken = |griefing: bool| {
        let base = beach();
        let mut world = beach();
        world.set_block_id(8, 1, 0, turtle_egg::state(2, 0).expect("egg"));
        let mut sim = sim_for(&base);
        sim.set_mob_griefing(griefing);
        sim.spawn_species(key("zombie"), Vec3::new(0.5, 1.0, 0.5));
        run(&mut sim, &mut world, 600);
        turtle_egg::counts(world.block_state_id(8, 1, 0)).is_none()
    };
    assert!(broken(true), "the egg is gone");
    assert!(!broken(false), "control: mob_griefing off");
}

/// A mob standing on an egg crushes one egg in about a hundred ticks; a turtle
/// standing on it never does.
#[test]
fn a_mob_standing_on_an_egg_crushes_it_but_a_turtle_does_not() {
    let eggs_left = |species: &str| {
        let base = beach();
        let mut world = beach();
        world.set_block_id(0, 1, 0, turtle_egg::state(4, 0).expect("egg"));
        let mut sim = sim_for(&base);
        let mob = sim.spawn_species(key(species), Vec3::new(0.5, 1.5, 0.5));
        let id = mob.id();
        // Keep it on the egg: a baby too young to wander far still drifts, so
        // reset its position each tick.
        let mut crushed = 0;
        for _ in 0..1500 {
            sim.get_mut(id).expect("mob").teleport_to(Vec3::new(0.5, 1.4375, 0.5));
            crushed += run(&mut sim, &mut world, 1).len();
        }
        let _ = crushed;
        turtle_egg::counts(world.block_state_id(0, 1, 0)).map_or(0, |(eggs, _)| eggs)
    };
    assert!(eggs_left("pig") < 4, "the pig crushed at least one egg");
    assert_eq!(eggs_left("turtle"), 4, "control: turtles spare eggs");
}

/// Hatching spawns one baby turtle per egg, each nested at the egg's cell.
#[test]
fn a_hatching_egg_spawns_baby_turtles_nested_at_the_egg() {
    let world = beach();
    let mut sim = sim_for(&world);
    sim.hatch_turtles((5, 1, 5), 3);
    let turtles: Vec<_> = sim.mobs.iter().filter(|m| m.entity_type().path() == "turtle").collect();
    assert_eq!(turtles.len(), 3);
    for t in turtles {
        assert!(t.is_baby());
        assert_eq!(t.mob.nest_position(), Some(Vec3::new(5.5, 1.0, 5.5)));
    }
}

/// Two turtles in love produce an egg for one of them to lay instead of a
/// child.
#[test]
fn breeding_turtles_make_an_egg_not_a_baby() {
    let base = beach();
    let mut world = beach();
    let mut sim = sim_for(&base);
    for x in [0.5, 1.5] {
        let t = sim.spawn_species(key("turtle"), Vec3::new(x, 1.0, 0.5));
        t.set_in_love();
    }
    let mut carried = false;
    for _ in 0..400 {
        run(&mut sim, &mut world, 1);
        carried |= sim.mobs.iter().any(|m| m.mob.carries_egg());
    }
    assert!(carried, "one turtle carried the egg");
    let turtles: Vec<_> = sim.mobs.iter().filter(|m| m.entity_type().path() == "turtle").collect();
    assert_eq!(turtles.len(), 2, "no child was born");
}

/// A carried egg and the nest survive a save and load.
#[test]
fn a_carried_egg_and_nest_persist() {
    let world = beach();
    let mut sim = sim_for(&world);
    let turtle = sim.spawn_species(key("turtle"), Vec3::new(3.5, 1.0, 3.5));
    turtle.mob.restore_has_egg(true);
    turtle.mob.set_nest(Some(Vec3::new(9.5, 1.0, 9.5)));
    let id = turtle.id();
    let saved = sim.saved_mob(sim.get(id).expect("turtle"));
    let mut other = sim_for(&world);
    other.restore_saved(&[saved]);
    let restored = other.mobs.iter().find(|m| m.entity_type().path() == "turtle").expect("restored");
    assert!(restored.mob.carries_egg());
    assert_eq!(restored.mob.nest_position(), Some(Vec3::new(9.5, 1.0, 9.5)));
}
