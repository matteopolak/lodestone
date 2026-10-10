use super::*;
use crate::beehive::{Occupant, Released};
use lodestone_data::block_states::StateId;
use crate::entity_record::SavedEntity;
use crate::mobs::bees::HiveEntry;

const HIVE: (i32, i32, i32) = (24, 1, 10);

/// Stone floor under y=1, with the given blocks on top.
fn field(blocks: &[(i32, i32, i32, &str)]) -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -30..70 {
        for z in -30..60 {
            world.set_block(x, 0, z, "minecraft:stone");
        }
    }
    for &(x, y, z, name) in blocks {
        world.set_block(x, y, z, name);
    }
    world
}

/// A meadow of poppies, so a wandering bee is never far from a bloom.
fn meadow() -> ChunkWorld {
    let mut blocks = Vec::new();
    for x in 0..18 {
        for z in 0..18 {
            blocks.push((x, 1, z, "minecraft:poppy"));
        }
    }
    field(&blocks)
}

fn bee_sim<'w>(world: &'w ChunkWorld, day_time: i32, rain: f32) -> (MobSim<'w>, i32) {
    let mut sim = MobSim::new(world);
    sim.set_day_time(day_time);
    sim.set_environment(crate::dimension::Dimension::Overworld, rain, 0.0);
    let id = sim.spawn_species("minecraft:bee".parse().expect("key"), Vec3::new(8.5, 2.0, 8.5)).id();
    sim.set_players(vec![PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(10.5, 1.0, 10.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, -1.0),
        },
    }]);
    (sim, id)
}

fn tick(sim: &mut MobSim<'_>, world: &ChunkWorld) {
    sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
}

/// The first tick at which the bee carries nectar, within `limit`.
fn first_nectar(sim: &mut MobSim<'_>, world: &ChunkWorld, id: i32, limit: usize) -> Option<usize> {
    (1..=limit).find(|_| {
        tick(sim, world);
        sim.get(id).is_some_and(|m| m.mob.bee_state().has_nectar)
    })
}

/// A bee finds the poppy, hovers over it and carries nectar. Pollination lasts
/// at least 400 ticks after the 20-tick minimum first-search delay, so nectar
/// cannot appear before tick 420; the controls (no bloom, and rain) never get
/// any in the same time.
#[test]
fn a_bee_pollinates_a_poppy_and_gains_nectar() {
    let world = meadow();
    let (mut sim, id) = bee_sim(&world, 6000, 0.0);
    let at = first_nectar(&mut sim, &world, id, 2000).expect("the bee gains nectar");
    assert!(at >= 420, "pollination needs 400 ticks, took {at}");
    let bee = sim.get(id).expect("alive").mob.bee_state();
    assert!(bee.flower.is_some_and(|(x, y, z)| (0..18).contains(&x) && y == 1 && (0..18).contains(&z)));
}

#[test]
fn a_bee_with_no_bloom_gains_no_nectar() {
    let world = field(&[]);
    let (mut sim, id) = bee_sim(&world, 6000, 0.0);
    assert_eq!(first_nectar(&mut sim, &world, id, 2000), None);
}

#[test]
fn rain_stops_pollination() {
    let world = meadow();
    let (mut sim, id) = bee_sim(&world, 6000, 1.0);
    assert_eq!(first_nectar(&mut sim, &world, id, 2000), None);
}

fn hive_world() -> ChunkWorld {
    field(&[(HIVE.0, HIVE.1, HIVE.2, "minecraft:bee_nest")])
}

fn homing_bee<'w>(world: &'w ChunkWorld, day_time: i32, nectar: bool, occupants: u8) -> (MobSim<'w>, i32) {
    let (mut sim, id) = bee_sim(world, day_time, 0.0);
    sim.set_hives([(HIVE, occupants)].into());
    let bee = sim.get_mut(id).expect("alive").mob.bee_state_mut();
    bee.hive = Some(HIVE);
    bee.set_has_nectar(nectar);
    (sim, id)
}

fn entries_within(sim: &mut MobSim<'_>, world: &ChunkWorld, limit: usize) -> Vec<HiveEntry> {
    for _ in 0..limit {
        tick(sim, world);
        let entries = sim.take_hive_entries();
        if !entries.is_empty() {
            return entries;
        }
    }
    Vec::new()
}

/// A bee carrying nectar flies home and enters; it leaves the simulation and
/// arrives as a saved occupant that remembers the nectar.
#[test]
fn a_bee_with_nectar_enters_its_hive() {
    let world = hive_world();
    let (mut sim, id) = homing_bee(&world, 6000, true, 0);
    let entries = entries_within(&mut sim, &world, 1500);
    assert_eq!(entries.len(), 1, "the bee enters");
    assert_eq!(entries[0].pos, BlockPos::new(HIVE.0, HIVE.1, HIVE.2));
    assert!(entries[0].occupant.has_nectar());
    assert_eq!(entries[0].occupant.min_ticks_in_hive, 2400);
    assert!(sim.get(id).is_none(), "the bee left the simulation");
}

/// Controls: a full hive refuses, a bee without nectar by day stays out, and
/// the same bee at night goes in.
#[test]
fn a_full_hive_refuses_a_bee() {
    let world = hive_world();
    let (mut sim, id) = homing_bee(&world, 6000, true, 3);
    assert!(entries_within(&mut sim, &world, 1500).is_empty());
    assert!(sim.get(id).is_some());
}

#[test]
fn a_bee_without_nectar_stays_out_by_day() {
    let world = hive_world();
    let (mut sim, _) = homing_bee(&world, 6000, false, 0);
    assert!(entries_within(&mut sim, &world, 1500).is_empty());
}

#[test]
fn night_sends_a_bee_without_nectar_home() {
    let world = hive_world();
    let (mut sim, _) = homing_bee(&world, 18000, false, 0);
    let entries = entries_within(&mut sim, &world, 1500);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].occupant.min_ticks_in_hive, 600);
}

/// Hand arithmetic for a 0.55 by 0.5 bee (the release's dimension table)
/// leaving a south-facing hive at (24, 1, 10): x = 24.5;
/// z = 10.5 + 0.55 + 0.275 = 11.325; y = 1.5 - 0.25 = 1.25.
/// A blocked front puts it at the centre column instead (z = 10.5).
#[test]
fn a_released_bee_appears_in_front_of_the_hive_and_delivers_its_nectar() {
    let world = hive_world();
    let (mut sim, id) = homing_bee(&world, 6000, true, 0);
    let entry = entries_within(&mut sim, &world, 1500).remove(0);
    let uuid = {
        let saved = SavedEntity::from_nbt(&entry.occupant.entity_data).expect("record");
        saved.uuid
    };
    let hive = BlockPos::new(HIVE.0, HIVE.1, HIVE.2);
    let flower = BlockPos::new(30, 1, 5);
    let mut occupant = entry.occupant.clone();
    occupant.ticks_in_hive = 2500;
    let released = Released { occupant, honey_delivered: true };
    assert!(sim.release_bee(hive, (0, 1), false, &released, Some(flower), true));
    let bee = sim.mobs.iter().find(|m| m.uuid == uuid).expect("released");
    assert_ne!(bee.id, id, "a released bee is a new entity");
    let at = bee.position();
    assert!((at.x - 24.5).abs() < 1e-6 && (at.z - 11.325).abs() < 1e-6 && (at.y - 1.25).abs() < 1e-6, "{at:?}");
    let state = bee.mob.bee_state();
    assert!(!state.has_nectar, "nectar was delivered");
    assert_eq!(state.hive, Some(HIVE));
    assert_eq!(state.flower, Some((30, 1, 5)));

    let mut sim = MobSim::new(&world);
    assert!(sim.release_bee(hive, (0, 1), true, &released, None, false));
    let at = sim.mobs[0].position();
    assert!((at.z - 10.5).abs() < 1e-6, "a blocked front releases at the hive centre: {at:?}");
}

/// A bee carrying nectar tends a wheat crop beneath it, each step one age
/// (2 to 3) and no more than ten per pollination; without nectar it leaves
/// the crop alone.
#[test]
fn a_bee_with_nectar_grows_wheat() {
    let mut blocks = Vec::new();
    for x in 4..14 {
        for z in 4..14 {
            blocks.push((x, 1, z, "minecraft:wheat[age=2]"));
        }
    }
    let world = field(&blocks);
    let grown = |nectar: bool| {
        let (mut sim, id) = bee_sim(&world, 6000, 0.0);
        sim.set_hives([(HIVE, 0)].into());
        let bee = sim.get_mut(id).expect("alive").mob.bee_state_mut();
        bee.set_has_nectar(nectar);
        bee.flower_cooldown = 100_000;
        // A valid hive is a precondition of tending crops; the stay-out timer
        // keeps the bee from going home while it works.
        bee.hive = Some(HIVE);
        bee.stay_out_ticks = 100_000;
        let mut writes = Vec::new();
        for _ in 0..1500 {
            tick(&mut sim, &world);
            writes.extend(sim.take_crop_growths());
        }
        writes
    };
    let with = grown(true);
    assert!(!with.is_empty(), "a nectar bee grows crops");
    let expected = StateId::from_state_str("minecraft:wheat[age=3]").expect("state");
    assert!(with.iter().all(|&(_, state)| state == expected), "{with:?}");
    assert!(grown(false).is_empty());
}

/// Nectar, stings, the hive, the bloom and the clocks survive a save through
/// the NBT encoding.
#[test]
fn a_bee_keeps_its_nectar_hive_and_bloom_across_a_reload() {
    let world = field(&[]);
    let (mut sim, id) = bee_sim(&world, 6000, 0.0);
    {
        let bee = sim.get_mut(id).expect("alive").mob.bee_state_mut();
        bee.set_has_nectar(true);
        bee.hive = Some((3, 4, 5));
        bee.flower = Some((6, 7, 8));
        bee.stay_out_ticks = 123;
        bee.crops_grown = 4;
    }
    let records: Vec<SavedEntity> = sim
        .saved_entities()
        .iter()
        .map(|saved| SavedEntity::from_nbt(&saved.to_nbt()).expect("record decodes"))
        .collect();
    let mut restored = MobSim::new(&world);
    assert_eq!(restored.restore_saved(&records), 1);
    let bee = restored.mobs[0].mob.bee_state();
    assert!(bee.has_nectar);
    assert_eq!((bee.hive, bee.flower), (Some((3, 4, 5)), Some((6, 7, 8))));
    assert_eq!((bee.stay_out_ticks, bee.crops_grown), (123, 4));
    assert!(bee.active);
}

#[test]
fn an_occupant_built_from_a_saved_bee_round_trips_through_the_hive_record() {
    let world = field(&[]);
    let (mut sim, id) = bee_sim(&world, 6000, 0.0);
    sim.get_mut(id).expect("alive").mob.bee_state_mut().set_has_nectar(true);
    let data = sim.saved_mob(sim.get(id).expect("alive")).to_nbt();
    let occupant = Occupant::entering(data);
    assert!(occupant.has_nectar());
}
