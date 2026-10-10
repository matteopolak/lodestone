//! Horse rearing and tempting, and parrots following other species.

use super::*;

fn grass() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -40..40 {
        for z in -40..40 {
            world.set_block(x, 0, z, "minecraft:grass_block");
        }
    }
    world
}

fn key(species: &str) -> ResourceKey {
    format!("minecraft:{species}").parse().expect("key")
}

fn player_at(x: f64, held: Option<&str>) -> PerceivedPlayer {
    PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(x, 1.0, 0.5),
            held_item: held.map(key),
            view_direction: Vec3::new(-1.0, 0.0, 0.0),
        },
    }
}

/// Whether the species ever rears in `ticks` ticks.
fn ever_rears(species: &str, ticks: u32) -> bool {
    let world = grass();
    let mut sim = MobSim::new(&world);
    sim.set_players(vec![player_at(60.5, None)]);
    let id = sim.spawn_species(key(species), Vec3::new(0.5, 1.0, 0.5)).id();
    (0..ticks).any(|_| {
        sim.tick();
        sim.get(id).is_some_and(|m| m.mob.is_rearing())
    })
}

#[test]
fn a_horse_rears_at_random_and_a_pig_never_does() {
    assert!(ever_rears("horse", 6000), "a horse must rear within 6000 ticks");
    assert!(!ever_rears("pig", 6000), "control: only horses rear");
}

/// Distance the mob closes toward a player holding `held` at x = 8.
fn closes_on_player(species: &str, held: &str) -> f64 {
    let world = grass();
    let mut sim = MobSim::new(&world);
    sim.set_players(vec![player_at(8.5, Some(held))]);
    let id = sim.spawn_species(key(species), Vec3::new(0.5, 1.0, 0.5)).id();
    for _ in 0..120 {
        sim.tick();
    }
    sim.get(id).expect("alive").position().x - 0.5
}

#[test]
fn a_horse_follows_a_golden_carrot_and_ignores_a_stick() {
    let toward = closes_on_player("horse", "golden_carrot");
    let control = closes_on_player("horse", "stick");
    assert!(toward > 4.0, "horse closed {toward} blocks on a golden carrot");
    assert!(control < toward - 2.0, "control: a stick drew it {control} blocks");
}

/// A parrot with another species nearby heads for it; alone it stays near home.
#[test]
fn a_parrot_follows_a_nearby_mob_of_another_species() {
    let run = |with_pig: bool| {
        let world = grass();
        let mut sim = MobSim::new(&world);
        sim.set_players(vec![player_at(60.5, None)]);
        let id = sim.spawn_species(key("parrot"), Vec3::new(0.5, 1.0, 0.5)).id();
        if with_pig {
            sim.spawn_species(key("cow"), Vec3::new(5.5, 1.0, 0.5));
        }
        let mut best = f64::MAX;
        for _ in 0..200 {
            sim.tick();
            let p = sim.get(id).expect("alive").position();
            best = best.min(((p.x - 5.5).powi(2) + (p.z - 0.5).powi(2)).sqrt());
        }
        best
    };
    let with = run(true);
    let without = run(false);
    assert!(with < 3.0, "parrot came within {with} of the cow");
    assert!(without > with + 2.0, "control: with no cow it got {without} from the spot");
}

/// Furthest an untamed or tamed wolf gets from a llama 8 blocks east of it.
fn wolf_flight(tamed: bool) -> f64 {
    let world = grass();
    let mut sim = MobSim::new(&world);
    sim.set_players(vec![player_at(60.5, None)]);
    let id = sim.spawn_species(key("wolf"), Vec3::new(0.5, 1.0, 0.5)).id();
    if tamed {
        sim.get_mut(id).expect("wolf").tame(MobOwner::Player(Uuid::from_u128(9)));
    }
    sim.spawn_species(key("llama"), Vec3::new(8.5, 1.0, 0.5));
    let mut west = 0.0_f64;
    for _ in 0..160 {
        sim.tick();
        west = west.max(0.5 - sim.get(id).expect("alive").position().x);
    }
    west
}

#[test]
fn an_untamed_wolf_flees_a_llama_and_a_tamed_one_does_not() {
    let wild = wolf_flight(false);
    let tame = wolf_flight(true);
    assert!(wild > 5.0, "the wild wolf only got {wild} blocks away");
    assert!(tame < wild - 3.0, "control: the tamed wolf got {tame} blocks away");
}

#[test]
fn a_wolf_begs_at_a_player_holding_a_bone_and_not_a_stick() {
    let begs = |held: &str| {
        let world = grass();
        let mut sim = MobSim::new(&world);
        sim.set_players(vec![player_at(5.5, Some(held))]);
        let id = sim.spawn_species(key("wolf"), Vec3::new(0.5, 1.0, 0.5)).id();
        (0..100).any(|_| {
            sim.tick();
            sim.get(id).expect("alive").mob.is_interested()
        })
    };
    assert!(begs("bone"));
    assert!(!begs("stick"), "control: a stick draws no begging");
}

/// Carrot age after a rabbit that starts on a ripe crop has had 200 ticks, with
/// the edits it asks for applied.
fn carrot_age_after_raid(griefing: bool) -> String {
    let mut world = grass();
    world.set_block(5, 0, 5, "minecraft:farmland");
    world.set_block(5, 1, 5, "minecraft:carrots[age=7]");
    let base = world.clone();
    let mut sim = MobSim::new(&base);
    sim.set_players(vec![player_at(60.5, None)]);
    sim.set_mob_griefing(griefing);
    sim.spawn_species(key("rabbit"), Vec3::new(5.5, 1.0, 5.5));
    for _ in 0..200 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        for edit in sim.take_block_edits() {
            let (x, y, z) = edit.cell;
            world.set_block_id(x, y, z, edit.set.unwrap_or_else(crate::chunk::air_state));
        }
    }
    format!("{:?}", world.block_state_id(5, 1, 5).properties())
}

#[test]
fn a_rabbit_on_a_ripe_carrot_eats_a_growth_stage_only_when_mobs_may_grief() {
    assert!(carrot_age_after_raid(true).contains("\"6\""), "{}", carrot_age_after_raid(true));
    assert!(carrot_age_after_raid(false).contains("\"7\""), "control: griefing off leaves the crop");
}

/// How far east a pillager gets in 300 ticks when a raid centred 100 blocks east
/// of it is ongoing (or, for the control, absent).
fn pillager_progress(in_raid: bool) -> f64 {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -40..160 {
        for z in -40..40 {
            world.set_block(x, 0, z, "minecraft:grass_block");
        }
    }
    let mut sim = MobSim::new(&world);
    assert!(sim.bell_claims.try_claim(lodestone_model::BlockPos::new(100, 1, 0)));
    let id = sim.spawn_species(key("pillager"), Vec3::new(0.5, 1.0, 0.5)).id();
    if in_raid {
        let raid = sim
            .start_raid(Vec3::new(100.5, 1.0, 0.5), lodestone_model::Difficulty::Easy, 1)
            .expect("Easy is not Peaceful");
        let state = sim.raids.get_mut(&raid).expect("just started");
        state.groups_spawned = state.total_waves;
        state.raiders.push(id);
    }
    for _ in 0..300 {
        sim.tick();
    }
    sim.get(id).expect("alive").position().x - 0.5
}

#[test]
fn a_raider_walks_toward_the_raid_centre_and_a_stray_pillager_does_not() {
    let raider = pillager_progress(true);
    let stray = pillager_progress(false);
    assert!(raider > 20.0, "the raider only advanced {raider} blocks");
    assert!(stray < raider - 10.0, "control: the stray pillager advanced {stray} blocks");
}

/// Rides an untamed (or, for the control, tame) horse for 600 ticks; returns
/// whether the rider was thrown, whether the horse ended tame, and its temper.
fn ride(tame: bool, temper: i32) -> (bool, bool, i32) {
    let world = grass();
    let mut sim = MobSim::new(&world);
    let rider = PerceivedPlayer {
        identity: Some(PlayerIdentity { uuid: Uuid::from_u128(5), entity_id: 77 }),
        perception: PlayerPerception {
            position: Vec3::new(0.5, 1.0, 0.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    };
    sim.set_players(vec![rider]);
    let id = sim.spawn_species(key("horse"), Vec3::new(0.5, 1.0, 0.5)).id();
    {
        let horse = sim.get_mut(id).expect("horse");
        horse.set_temper(temper);
        if tame {
            horse.tame(MobOwner::Player(Uuid::from_u128(5)));
        }
    }
    assert!(sim.mount_mob(id, 77), "an untamed horse accepts a rider");
    let mut thrown = false;
    for _ in 0..600 {
        sim.tick();
        thrown |= sim.take_ejections_of(77).contains(&id);
    }
    let horse = sim.get(id).expect("horse");
    (thrown, horse.is_tame(), horse.temper())
}

#[test]
fn an_untamed_horse_throws_a_low_temper_rider_and_yields_to_a_high_temper_one() {
    let (thrown, tame, temper) = ride(false, 0);
    assert!(thrown, "temper 0 never tames, so the rider must be thrown");
    assert!(!tame);
    assert!(temper >= 5, "a failed roll raises temper by five, got {temper}");
    let (thrown, tame, _) = ride(false, 100);
    assert!(tame && !thrown, "temper at the maximum always tames");
    let (thrown, tame, _) = ride(true, 0);
    assert!(tame && !thrown, "control: a tame horse never throws its rider");
}

/// A zombie holding `item` hunts a player 12 blocks away for 400 ticks. Returns
/// whether it ever raised the item and the damage of each hit it landed.
fn spear_fight(item: &str) -> (bool, Vec<f32>) {
    let world = grass();
    let mut sim = MobSim::new(&world);
    let victim = PerceivedPlayer {
        identity: Some(PlayerIdentity { uuid: Uuid::from_u128(6), entity_id: 78 }),
        perception: PlayerPerception {
            position: Vec3::new(12.5, 1.0, 0.5),
            held_item: None,
            view_direction: Vec3::new(-1.0, 0.0, 0.0),
        },
    };
    sim.set_players(vec![victim]);
    let id = sim.spawn_species(key("zombie"), Vec3::new(0.5, 1.0, 0.5)).id();
    sim.get_mut(id).expect("zombie").mob.set_main_hand_item(Some(item.to_owned()));
    let mut raised = false;
    let mut damages = Vec::new();
    for _ in 0..400 {
        sim.tick();
        raised |= sim.get(id).is_some_and(|m| m.mob.is_using_item());
        damages.extend(sim.take_player_hits().into_iter().map(|h| h.raw_damage));
    }
    (raised, damages)
}

#[test]
fn a_zombie_with_a_spear_levels_it_and_stabs_for_more_than_its_base_damage() {
    let (raised, damages) = spear_fight("minecraft:iron_spear");
    assert!(raised, "the spear was never levelled");
    assert!(!damages.is_empty(), "the charge never landed");
    assert!(damages.iter().any(|d| *d > 3.0), "a stab adds speed damage to the base 3, got {damages:?}");
    let (raised, damages) = spear_fight("minecraft:iron_sword");
    assert!(!raised, "control: a sword is not levelled");
    assert!(!damages.is_empty(), "control: the zombie still fights with a sword");
    assert!(damages.iter().all(|d| (*d - 3.0).abs() < 0.01), "control: plain hits do base damage, got {damages:?}");
}

/// Where a zombie ends up after 100 ticks (before its idle throttle shuts
/// wandering off) when a village point of interest, a claimed bell when
/// `village`, is 12 blocks east.
fn zombie_x_near_a_bell(village: bool, day_time: i32) -> f64 {
    let world = grass();
    let mut sim = MobSim::new(&world);
    sim.set_day_time(day_time);
    if village {
        assert!(sim.bell_claims.try_claim(lodestone_model::BlockPos::new(12, 1, 0)));
    }
    let id = sim.spawn_species(key("zombie"), Vec3::new(0.5, 1.0, 0.5)).id();
    for _ in 0..100 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
    }
    sim.get(id).expect("alive").position().x - 0.5
}

#[test]
fn a_night_zombie_walks_to_a_village_point_of_interest_and_only_at_night() {
    let village = zombie_x_near_a_bell(true, 18000);
    let control = zombie_x_near_a_bell(false, 18000);
    assert!(village > 5.0, "the zombie only advanced {village}");
    assert!(control.abs() < 5.0, "control: with no village it moved {control}");
    let by_day = zombie_x_near_a_bell(true, 6000);
    assert!(by_day < village - 5.0, "control: by day it does not patrol, moved {by_day} against {village}");
}

/// The height a `species` dropped onto a powder snow field ends at after 60 ticks.
fn height_over_powder_snow(species: &str) -> f64 {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -20..20 {
        for z in -20..20 {
            world.set_block(x, 0, z, "minecraft:stone");
            world.set_block(x, 1, z, "minecraft:powder_snow");
        }
    }
    let mut sim = MobSim::new(&world);
    let id = sim.spawn_species(key(species), Vec3::new(0.5, 2.0, 0.5)).id();
    for _ in 0..60 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
    }
    sim.get(id).expect("alive").position().y
}

#[test]
fn a_rabbit_stands_on_powder_snow_and_a_pig_sinks_through_it() {
    let rabbit = height_over_powder_snow("rabbit");
    let pig = height_over_powder_snow("pig");
    assert!((rabbit - 2.0).abs() < 0.3, "the rabbit ended at y={rabbit}");
    assert!(pig < 1.3, "control: the pig ended at y={pig}");
}

/// The height a `species` sunk to the floor of a two-block powder snow bed ends at.
fn height_climbing_out(species: &str) -> f64 {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -20..20 {
        for z in -20..20 {
            world.set_block(x, 0, z, "minecraft:stone");
            world.set_block(x, 1, z, "minecraft:powder_snow");
            world.set_block(x, 2, z, "minecraft:powder_snow");
        }
    }
    let mut sim = MobSim::new(&world);
    let id = sim.spawn_species(key(species), Vec3::new(0.5, 1.0, 0.5)).id();
    for _ in 0..200 {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
    }
    sim.get(id).expect("alive").position().y
}

#[test]
fn a_rabbit_sunk_in_powder_snow_jumps_out_and_a_pig_stays_buried() {
    let rabbit = height_climbing_out("rabbit");
    let pig = height_climbing_out("pig");
    assert!(rabbit > 2.8, "the rabbit ended at y={rabbit}");
    assert!(pig < 1.3, "control: the pig ended at y={pig}");
}
