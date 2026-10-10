//! Doors on a mob's path: raiders and villagers open them, zombies and
//! vindicators break them under the right difficulty.

use lodestone_data::block::Block;
use lodestone_data::block_properties::{BuiltinPropertyValue as V, PropertyKey as K};
use lodestone_entity::ai::door;
use lodestone_entity::ai::{BlockExpect, MobController};
use lodestone_model::{BlockPos, Difficulty};

use super::*;

fn key(species: &str) -> ResourceKey {
    format!("minecraft:{species}").parse().expect("key")
}

fn door_state(half: V, open: bool) -> lodestone_data::block_states::StateId {
    crate::redstone::configured_state(
        Block::OakDoor,
        &[(K::Half, half), (K::Open, if open { V::True } else { V::False }), (K::Facing, V::East)],
    )
}

/// A floor with a stone wall across x = 5 and a closed door at z = 0.
fn walled() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -10..20 {
        for z in -12..12 {
            world.set_block(x, 0, z, "minecraft:stone");
        }
    }
    for z in -12..12 {
        for y in 1..=3 {
            world.set_block(5, y, z, "minecraft:stone");
        }
    }
    world.set_block_id(5, 1, 0, door_state(V::Lower, false));
    world.set_block_id(5, 2, 0, door_state(V::Upper, false));
    world
}

fn run(sim: &mut MobSim<'_>, world: &mut ChunkWorld, ticks: u32) {
    for _ in 0..ticks {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        for edit in sim.take_block_edits() {
            let (x, y, z) = edit.cell;
            let old = world.block_state_id(x, y, z);
            let holds = match edit.expect {
                BlockExpect::Air => lodestone_entity::ai::turtle_egg::is_air(old),
                BlockExpect::State(s) => old == s,
                BlockExpect::Block(b) => old.block() == b,
            };
            if holds {
                world.set_block_id(x, y, z, edit.set.unwrap_or_else(crate::chunk::air_state));
            }
        }
    }
}

fn lower_open(world: &ChunkWorld) -> Option<bool> {
    door::is_open(world.block_state_id(5, 1, 0))
}

/// A night sim with `species` on the near side of the wall.
fn sim_with<'w>(base: &'w ChunkWorld, species: &str, difficulty: Difficulty) -> (MobSim<'w>, i32) {
    let mut sim = MobSim::new(base);
    sim.set_day_time(18000);
    sim.set_difficulty(difficulty);
    // A multiplier of 10 makes the zombie family's spawn-time door roll certain.
    sim.set_spawn_difficulty(10.0, difficulty == Difficulty::Hard);
    // A player beyond the wall keeps the AI running without being a reachable target.
    sim.set_players(vec![PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(9.5, 1.0, 10.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }]);
    let id = sim.spawn_species(key(species), Vec3::new(1.5, 1.0, 0.5)).id();
    (sim, id)
}

/// Sends the mob toward the far side of the wall. Re-issued whenever the
/// route ends, since a roaming goal's own stop cancels it.
fn head_for_far_side(sim: &mut MobSim<'_>, id: i32) {
    let mob = &mut sim.get_mut(id).expect("alive").mob;
    if mob.navigation_done() {
        mob.move_to(Vec3::new(9.5, 1.0, 0.5), 0.5);
    }
}

/// A raider in an active raid opens the door in its way and walks through; the
/// same raider with no raid leaves it shut (control).
#[test]
fn a_raiding_vindicator_opens_a_door_and_a_vindicator_with_no_raid_does_not() {
    let outcome = |raiding: bool| {
        let base = walled();
        let mut world = walled();
        let (mut sim, id) = sim_with(&base, "vindicator", Difficulty::Easy);
        if raiding {
            assert!(sim.bell_claims.try_claim(BlockPos::new(0, 1, 0)));
            let raid = sim.start_raid(Vec3::new(0.5, 1.0, 0.5), Difficulty::Easy, 1).expect("raid");
            let state = sim.raids.get_mut(&raid).expect("raid");
            state.groups_spawned = state.total_waves;
            state.raiders.push(id);
        }
        let mut opened = false;
        for _ in 0..400 {
            head_for_far_side(&mut sim, id);
            run(&mut sim, &mut world, 1);
            opened |= lower_open(&world) == Some(true);
        }
        (opened, sim.get(id).expect("alive").position().x)
    };
    let (opened, x) = outcome(true);
    assert!(opened, "the door must swing open for a raider");
    assert!(x > 5.5, "the raider must end up past the wall, not at x = {x}");
    let (opened, x) = outcome(false);
    assert!(!opened, "outside a raid the door stays shut");
    assert!(x < 5.0, "and the vindicator is held behind it, at x = {x}");
}

/// A zombie that rolled the door ability breaks the door after 240 ticks on
/// Hard and not on Normal (control).
#[test]
fn a_zombie_breaks_a_door_on_hard_but_not_on_normal() {
    let broken = |difficulty: Difficulty| {
        let base = walled();
        let mut world = walled();
        let (mut sim, id) = sim_with(&base, "zombie", difficulty);
        for _ in 0..400 {
            head_for_far_side(&mut sim, id);
            run(&mut sim, &mut world, 1);
        }
        (
            door::is_wooden_door(world.block_state_id(5, 1, 0)),
            door::is_wooden_door(world.block_state_id(5, 2, 0)),
        )
    };
    assert_eq!(broken(Difficulty::Hard), (false, false), "both halves are gone");
    assert_eq!(broken(Difficulty::Normal), (true, true), "the door stands on Normal");
}

/// A villager heading home to a bed behind a door opens it (control: with no
/// bed to go to, it leaves the door shut).
#[test]
fn a_villager_walking_to_its_bed_opens_the_door_on_the_way() {
    let opened = |bed: bool| {
        let base = {
            let mut w = walled();
            if bed {
                w.set_block(9, 1, 0, "minecraft:red_bed");
            }
            w
        };
        let mut world = base.clone();
        let (mut sim, id) = sim_with(&base, "villager", Difficulty::Normal);
        let mut opened = false;
        let _ = id;
        for _ in 0..1500 {
            run(&mut sim, &mut world, 1);
            opened |= lower_open(&world) == Some(true);
        }
        opened
    };
    assert!(opened(true), "the villager must open the door on its way to bed");
    assert!(!opened(false), "with no bed behind it the door stays shut");
}
