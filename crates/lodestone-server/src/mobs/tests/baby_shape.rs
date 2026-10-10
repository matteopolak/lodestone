use super::*;

fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for z in 0..16 {
        for x in 0..16 {
            world.set_block(x, 0, z, "minecraft:stone");
        }
    }
    world
}

fn above_floor() -> Vec3 {
    Vec3::new(8.0, 1.0, 8.0)
}

/// **A baby zombie is the real `0.49×0.98` literal, not a halved adult.**
///
/// `0.6×1.95` halved is `0.3×0.975` — close enough to the true value that
/// an assertion only checking "shrank" would pass under either
/// hypothesis. Predicting the exact literal is what separates a real
/// `BABY_DIMENSIONS` port from the generic `getAgeScale()` fallback.
#[test]
fn a_baby_zombie_is_the_exact_vanilla_literal_not_a_halved_adult() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:zombie".parse().expect("valid key"), above_floor())
        .id();
    {
        let adult = sim.get(id).expect("spawned");
        assert_eq!(adult.shape().width, 0.6, "adult zombie width");
        assert_eq!(adult.shape().height, 1.95, "adult zombie height");
    }

    let mob = sim.get_mut(id).expect("spawned");
    mob.set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
    let baby = sim.get(id).expect("still spawned");
    assert_eq!(
        baby.shape().width,
        0.49,
        "baby zombie width is the literal BABY_DIMENSIONS, not 0.6 * 0.5 = 0.3"
    );
    assert_eq!(
        baby.shape().height,
        0.98,
        "baby zombie height is the literal BABY_DIMENSIONS, not 1.95 * 0.5 = 0.975"
    );
}

/// Growing back up re-derives the adult shape — the boundary crossing
/// runs in both directions, not just baby-ward.
#[test]
fn growing_up_restores_the_adult_shape() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), above_floor())
        .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE)
        .id();
    assert_eq!(sim.get(id).expect("spawned").shape().width, 0.45, "baby cow width");

    sim.get_mut(id).expect("spawned").set_age(0);
    let grown = sim.get(id).expect("still spawned");
    assert!(!grown.is_baby(), "age 0 is the cooldown-free adult reading");
    assert_eq!(grown.shape().width, 0.9, "adult cow width restored");
    assert_eq!(grown.shape().height, 1.4, "adult cow height restored");
}

/// **Control: a species with no `baby_dimensions` entry uses the real
/// baby fallback (half size), not a made-up constant.**
///
/// Skeletons never naturally have babies, but `is_baby()` only reads the
/// age counter — nothing species-gates it — so this is the discriminating
/// input for the fallback arm specifically: a skeleton's adult box is
/// `0.6×1.99`, and the *wrong* hypothesis (no fallback at all, i.e. the
/// shape not changing) would leave it at `0.6×1.99` where the fallback
/// predicts `0.3×0.995`.
#[test]
fn control_a_species_with_no_baby_table_entry_uses_the_generic_age_scale() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:skeleton".parse().expect("valid key"), above_floor())
        .id();
    let adult_width = sim.get(id).expect("spawned").shape().width;
    let adult_height = sim.get(id).expect("spawned").shape().height;
    assert_eq!(adult_width, 0.6, "adult skeleton width");
    assert_eq!(adult_height, 1.99, "adult skeleton height");

    sim.get_mut(id)
        .expect("spawned")
        .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
    let baby = sim.get(id).expect("still spawned");
    assert_eq!(
        baby.shape().width,
        adult_width * 0.5,
        "no BABY_DIMENSIONS entry falls back to LivingEntity's own 0.5 age scale"
    );
    assert_eq!(
        baby.shape().height,
        adult_height * 0.5,
        "no BABY_DIMENSIONS entry falls back to LivingEntity's own 0.5 age scale"
    );
}

/// The zombie family's baby speed bonus is a +50% multiplicative attribute
/// modifier (`0.23 -> 0.345`); a cow has none. Expected values are the species
/// attributes and the bonus, written out here, not read back from the
/// implementation.
#[test]
fn baby_zombie_speeds_up_and_baby_cow_does_not() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);

    let zombie_id = sim
        .spawn_species("minecraft:zombie".parse().expect("valid key"), above_floor())
        .id();
    assert!((sim.get(zombie_id).expect("spawned").movement_speed() - 0.23).abs() < 1e-12);
    sim.get_mut(zombie_id)
        .expect("spawned")
        .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
    assert!((sim.get(zombie_id).expect("still spawned").movement_speed() - 0.345).abs() < 1e-12);
    sim.get_mut(zombie_id).expect("spawned").set_age(0);
    assert!((sim.get(zombie_id).expect("grown").movement_speed() - 0.23).abs() < 1e-12);

    let cow_id = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), above_floor())
        .id();
    assert!((sim.get(cow_id).expect("spawned").movement_speed() - 0.2).abs() < 1e-12);
    sim.get_mut(cow_id)
        .expect("spawned")
        .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
    assert!((sim.get(cow_id).expect("still spawned").movement_speed() - 0.2).abs() < 1e-12);
}

/// A bred child inherits the correct baby shape through
/// `resolve_breeding`'s existing `child.set_age(BABY_START_AGE)` call —
/// no separate wiring needed, because [`SimMob::set_age`] itself now
/// re-derives the shape. This is the island check: a shape fold that only
/// ran for a hand-called `set_age` in a test, and never for the
/// production breeding path, would still look finished from the unit
/// tests above alone.
#[test]
fn a_bred_child_spawns_with_the_baby_shape_already_applied() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let a = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(7.0, 1.0, 8.0))
        .id();
    let _b = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(9.0, 1.0, 8.0))
        .id();

    // Exercises `resolve_breeding`'s own partner search and
    // `child.set_age(BABY_START_AGE)` call directly — the real
    // production path a breeding goal completing feeds through
    // `MobSim::tick`, without re-driving sixty ticks of love-mode timing
    // just to reach it.
    sim.resolve_breeding(vec![(
        a,
        Vec3::new(8.0, 1.0, 8.0),
        "minecraft:cow".parse().expect("valid key"),
    )]);

    let child = sim
        .mobs
        .iter()
        .find(|m| m.is_baby())
        .expect("a child was spawned and is a baby");
    assert_eq!(
        child.shape().width,
        0.45,
        "the bred calf's shape must already be the baby literal, not the adult default"
    );
}
