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
