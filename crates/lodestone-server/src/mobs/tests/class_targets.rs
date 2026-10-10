//! Mob-versus-mob targeting: classes of entity the perception feed answers for
//! non-player targets, the guardian look-at, home restriction and universal
//! anger.

use super::*;

fn floor() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -20..60 {
        for z in -20..20 {
            world.set_block(x, 0, z, "minecraft:stone");
        }
    }
    world
}

fn key(species: &str) -> ResourceKey {
    format!("minecraft:{species}").parse().expect("key")
}

fn far_player() -> PerceivedPlayer {
    player_at(None, Vec3::new(55.5, 1.0, 0.5))
}

fn player_at(identity: Option<PlayerIdentity>, position: Vec3) -> PerceivedPlayer {
    PerceivedPlayer {
        identity,
        perception: PlayerPerception {
            position,
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }
}

fn run(sim: &mut MobSim<'_>, world: &ChunkWorld, ticks: u32) {
    for _ in 0..ticks {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
    }
}

/// The lowest health `victim` reaches while `hunter` stands 8 blocks from it
/// and no player is in range of either.
fn victim_health_after(hunter: &str, victim: &str, ticks: u32, setup: impl Fn(&mut SimMob<'_>)) -> f32 {
    let world = floor();
    let mut sim = MobSim::new(&world);
    sim.set_day_time(18000);
    sim.set_players(vec![far_player()]);
    let h = sim.spawn_species(key(hunter), Vec3::new(0.5, 1.0, 0.5));
    setup(h);
    let v = sim.spawn_species(key(victim), Vec3::new(8.5, 1.0, 0.5)).id();
    let mut lowest = f32::MAX;
    for _ in 0..ticks {
        run(&mut sim, &world, 1);
        match sim.get(v) {
            Some(m) => lowest = lowest.min(m.health()),
            None => return 0.0,
        }
    }
    lowest
}

/// A zombie walks to a villager 8 blocks away and hurts it; a pig, which is
/// not in the zombie's target classes, is left alone.
#[test]
fn a_zombie_attacks_a_villager_and_ignores_a_pig() {
    let villager = victim_health_after("zombie", "villager", 400, |_| {});
    let pig = victim_health_after("zombie", "pig", 400, |_| {});
    assert!(villager < 20.0, "the villager was never hurt: {villager}");
    assert!((pig - 10.0).abs() < f32::EPSILON, "the pig was hurt: {pig}");
}

/// An untamed wolf hunts a sheep; a tame one does not.
#[test]
fn an_untamed_wolf_hunts_sheep_and_a_tame_one_does_not() {
    let wild = victim_health_after("wolf", "sheep", 600, |_| {});
    let tame = victim_health_after("wolf", "sheep", 600, |wolf| {
        wolf.tame = true;
        wolf.mob.set_tame(true);
    });
    assert!(wild < 8.0, "the sheep was never hurt: {wild}");
    assert!((tame - 8.0).abs() < f32::EPSILON, "a tame wolf hurt the sheep: {tame}");
}

/// A guardian watches another guardian within 12 blocks; with a pig in the
/// other's place it never looks at that spot.
#[test]
fn a_guardian_watches_another_guardian() {
    let looked_at = |other: &str| {
        let world = floor();
        let mut sim = MobSim::new(&world);
        sim.set_players(vec![far_player()]);
        let id = sim.spawn_species(key("guardian"), Vec3::new(0.5, 1.0, 0.5)).id();
        let spot = Vec3::new(9.5, 1.0, 0.5);
        sim.spawn_species(key(other), spot);
        let mut seen = false;
        for _ in 0..1500 {
            run(&mut sim, &world, 1);
            seen |= sim
                .get(id)
                .and_then(|m| m.facing())
                .is_some_and(|look| (look.x - spot.x).abs() < 1.0 && (look.z - spot.z).abs() < 1.0);
        }
        seen
    };
    assert!(looked_at("guardian"), "the guardian never looked at its neighbour");
    assert!(!looked_at("pig"), "the control pig drew a look");
}

/// An elder guardian carried 19 blocks from where it first ticked swims back
/// inside its 16-block home; the same trip leaves a guardian, which has no home,
/// where it is.
#[test]
fn an_elder_guardian_swims_back_inside_its_home() {
    let mut world = floor();
    for x in -20..60 {
        for z in -20..20 {
            for y in 1..6 {
                world.set_block(x, y, z, "minecraft:water");
            }
        }
    }
    let distance_after = |species: &str| {
        let mut sim = MobSim::new(&world);
        sim.set_players(vec![far_player()]);
        let id = sim.spawn_species(key(species), Vec3::new(0.5, 3.0, 0.5)).id();
        run(&mut sim, &world, 2);
        sim.get_mut(id).expect("alive").teleport_to(Vec3::new(19.5, 3.0, 0.5));
        run(&mut sim, &world, 300);
        sim.get(id).expect("alive").position().x
    };
    let elder = distance_after("elder_guardian");
    let plain = distance_after("guardian");
    assert!(elder < 16.0, "the elder stayed outside its home: x = {elder}");
    assert!(plain > 16.0, "the control guardian drifted home on its own: x = {plain}");
}

/// Under `universal_anger` a player's hit leaves the wolf angry at every
/// player, so it goes for the nearest one; without the rule it keeps to the
/// attacker.
#[test]
fn universal_anger_turns_a_hurt_wolf_on_the_nearest_player() {
    let attacker = PlayerIdentity { uuid: Uuid::from_u128(0xA11CE), entity_id: 1 };
    let bystander = PlayerIdentity { uuid: Uuid::from_u128(0xB0B), entity_id: 2 };
    let target_x = |rule: bool| {
        let world = floor();
        let mut sim = MobSim::new(&world);
        sim.set_universal_anger(rule);
        let far = Vec3::new(12.5, 1.0, 0.5);
        sim.set_players(vec![
            player_at(Some(attacker), far),
            player_at(Some(bystander), Vec3::new(5.5, 1.0, 3.5)),
        ]);
        let wolf = sim.spawn_species(key("wolf"), Vec3::new(0.5, 1.0, 0.5)).id();
        sim.attack_from_player(wolf, Some(attacker), far, 1.0, DamageFlags::default(), 0.0)
            .expect("the wolf exists");
        run(&mut sim, &world, 10);
        sim.get(wolf).and_then(|m| m.attack_target()).map(|t| t.x)
    };
    assert_eq!(target_x(true), Some(5.5), "universal anger targets the bystander");
    assert_eq!(target_x(false), Some(12.5), "the plain grudge targets the attacker");
}

/// A restriction is saved as `home_pos` and `home_radius` and read back.
#[test]
fn a_home_restriction_survives_a_reload() {
    let world = floor();
    let mut sim = MobSim::new(&world);
    sim.set_players(vec![far_player()]);
    sim.spawn_species(key("elder_guardian"), Vec3::new(4.5, 1.0, 4.5));
    run(&mut sim, &world, 2);
    let records: Vec<crate::entity_record::SavedEntity> = sim
        .saved_entities()
        .iter()
        .map(|saved| crate::entity_record::SavedEntity::from_nbt(&saved.to_nbt()).expect("record decodes"))
        .collect();
    let mut restored = MobSim::new(&world);
    restored.restore_saved(&records);
    let home = restored.mobs.iter().find_map(|m| m.mob.restriction());
    assert_eq!(home, Some((BlockPos::new(4, 1, 4), 16)));
}
