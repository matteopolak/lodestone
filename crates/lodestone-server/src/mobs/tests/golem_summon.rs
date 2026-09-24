use super::*;

/// A real floor near the origin — see `leash_tests::flat_world`'s own
/// doc comment for why a bare void `ChunkWorld` stopped being safe once
/// idle mobs fall. This module's `hurt_villager` calls at `x = ±50` are
/// deliberately left ungrounded: no assertion here reads either
/// villager's own position, only the resulting golem-summon count.
fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

fn hurt_villager(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
    let id = sim
        .spawn_species("minecraft:villager".parse().expect("valid key"), pos)
        .id();
    sim.get_mut(id)
        .expect("just spawned")
        .mob
        .note_hurt(Some(Vec3::new(pos.x + 1.0, pos.y, pos.z)));
    id
}

fn iron_golem_count(sim: &MobSim<'_>) -> usize {
    sim.mobs
        .iter()
        .filter(|m| m.entity_type.path() == "iron_golem")
        .count()
}

/// The headline case: three hurt villagers within the 10-block agreement
/// box must produce exactly one iron golem, and every villager in the box
/// (not only the triggering one) must be marked `golem_detected_until` so
/// a second pass this same 100-tick window does not summon a second one.
#[test]
fn three_hurt_villagers_close_together_summon_exactly_one_golem() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    hurt_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));
    hurt_villager(&mut sim, Vec3::new(2.0, 0.0, 0.0));
    hurt_villager(&mut sim, Vec3::new(-2.0, 0.0, 0.0));

    sim.tick();

    assert_eq!(
        iron_golem_count(&sim),
        1,
        "three hurt villagers within the agreement box must summon exactly one golem"
    );
    assert!(
        sim.mobs
            .iter()
            .filter(|m| m.entity_type.path() == "villager")
            .all(|m| m.golem_detected_until.is_some()),
        "every villager in the agreement box must be marked golem-detected after a spawn"
    );
}

/// Below `GOLEM_VILLAGERS_NEEDED` (3): two hurt villagers must summon
/// nothing — the discriminating floor, not merely "some villagers hurt
/// summons a golem eventually".
#[test]
fn two_hurt_villagers_do_not_summon_a_golem() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    hurt_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));
    hurt_villager(&mut sim, Vec3::new(2.0, 0.0, 0.0));

    for _ in 0..150 {
        sim.tick();
    }

    assert_eq!(iron_golem_count(&sim), 0, "two hurt villagers must never reach the agreement floor");
}

/// Three villagers far enough apart that they never share an agreement
/// box (each pairwise distance exceeds `GOLEM_AGREEMENT_RADIUS`) must not
/// summon a golem even though each is individually hurt.
#[test]
fn three_hurt_villagers_too_far_apart_do_not_summon_a_golem() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    hurt_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));
    hurt_villager(&mut sim, Vec3::new(50.0, 0.0, 0.0));
    hurt_villager(&mut sim, Vec3::new(-50.0, 0.0, 0.0));

    sim.tick();

    assert_eq!(iron_golem_count(&sim), 0, "villagers outside one shared agreement box must not summon a golem");
}

/// A villager that is neither hurt nor near a hostile is not a candidate
/// at all — three *unhurt* villagers standing together must never summon
/// a golem.
#[test]
fn unhurt_villagers_with_no_hostile_nearby_never_summon() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));
    sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0));
    sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(-2.0, 0.0, 0.0));

    for _ in 0..150 {
        sim.tick();
    }

    assert_eq!(iron_golem_count(&sim), 0, "villagers with nothing wrong must never summon a golem");
}

/// A hostile mob nearby is exactly as good as being hurt — three
/// *unhurt* villagers next to a zombie must still summon, proving the
/// `hurt || hostile_near` disjunction rather than only the hurt half.
#[test]
fn a_nearby_hostile_alone_is_enough_to_summon() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));
    sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0));
    sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(-2.0, 0.0, 0.0));
    sim.spawn_species("minecraft:zombie".parse().expect("valid key"), Vec3::new(1.0, 0.0, 1.0));

    sim.tick();

    assert_eq!(iron_golem_count(&sim), 1, "a nearby hostile alone must be enough to summon, with no villager hurt");
}
