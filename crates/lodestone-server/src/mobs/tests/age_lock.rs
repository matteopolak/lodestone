use super::*;

fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

fn alice() -> PlayerIdentity {
    PlayerIdentity { uuid: Uuid::from_u128(0xA11CE), entity_id: 4242 }
}

fn dandelion() -> ResourceKey {
    "minecraft:golden_dandelion".parse().expect("valid key")
}

fn baby(sim: &mut MobSim<'_>, species: &str, x: f64) -> i32 {
    let id = sim
        .spawn_species(format!("minecraft:{species}").parse().expect("valid key"), Vec3::new(x, 0.0, 0.0))
        .id();
    sim.get_mut(id).expect("spawned").set_age(BABY_START_AGE + 100);
    id
}

/// A golden dandelion on a baby cow freezes its growth and puts it back at the
/// start of babyhood; a second use inside the 40-tick cooldown is refused, and
/// one after it unlocks. Ticking while locked leaves the age untouched.
#[test]
fn a_golden_dandelion_locks_a_baby_and_the_cooldown_gates_the_next_use() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let cow = baby(&mut sim, "cow", 0.0);

    let first = sim.interact(cow, alice(), Some(&dandelion()));
    assert_eq!(first, InteractOutcome::AgeLockToggled { locked: true });
    assert!(first.consumes_item());
    assert_eq!(sim.get(cow).expect("alive").age(), BABY_START_AGE);

    assert_eq!(
        sim.interact(cow, alice(), Some(&dandelion())),
        InteractOutcome::Pass,
        "the cooldown must refuse an immediate second use"
    );

    for _ in 0..60 {
        sim.tick();
    }
    assert_eq!(sim.get(cow).expect("alive").age(), BABY_START_AGE, "a locked baby must not grow");

    assert_eq!(
        sim.interact(cow, alice(), Some(&dandelion())),
        InteractOutcome::AgeLockToggled { locked: false }
    );
}

/// The controls: an adult cow and a baby villager (the tag lists villagers)
/// both refuse the item.
#[test]
fn a_golden_dandelion_is_refused_by_adults_and_by_tagged_species() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let adult = sim.spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
    let villager = baby(&mut sim, "villager", 4.0);
    assert_eq!(sim.interact(adult, alice(), Some(&dandelion())), InteractOutcome::Pass);
    assert_eq!(sim.interact(villager, alice(), Some(&dandelion())), InteractOutcome::Pass);
}
