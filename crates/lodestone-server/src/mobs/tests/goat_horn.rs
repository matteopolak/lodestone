use super::*;

/// A real floor — see `leash_tests::flat_world`'s own doc comment for
/// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

/// **Real arithmetic, not merely "sometimes true"**: over a large sample,
/// the fraction of goats spawned missing a horn must land near
/// vanilla's own goat spawn-finalization's own `0.1` roll — bounded generously (5%–15%
/// over 2,000 trials) since this crate's `SpawnRng` is not a
/// bit-identical port of `java.util.Random` (a disclosed approximation
/// already established elsewhere in this crate, e.g. `raid::bonus_spawns`).
#[test]
fn about_one_in_ten_goats_spawn_missing_a_horn() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let trials = 2000;
    let mut missing = 0;
    for _ in 0..trials {
        let id = sim.spawn_species("minecraft:goat".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
        let m = sim.get(id).expect("just spawned");
        if !(m.has_left_horn() && m.has_right_horn()) {
            missing += 1;
        }
    }
    let fraction = f64::from(missing) / f64::from(trials);
    assert!(
        (0.05..=0.15).contains(&fraction),
        "expected roughly 10% of {trials} goats missing a horn, got {missing} ({:.1}%)",
        fraction * 100.0
    );
}

/// The discriminating control: a goat that *does* lose a horn loses
/// exactly one, never both — proves the roll picks a single horn rather
/// than clearing the pair.
#[test]
fn a_goat_that_loses_a_horn_loses_exactly_one() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let mut saw_a_miss = false;
    for _ in 0..500 {
        let id = sim.spawn_species("minecraft:goat".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
        let m = sim.get(id).expect("just spawned");
        let horn_count = i32::from(m.has_left_horn()) + i32::from(m.has_right_horn());
        assert!((1..=2).contains(&horn_count), "a goat must never lose both horns at spawn");
        if horn_count == 1 {
            saw_a_miss = true;
        }
    }
    assert!(saw_a_miss, "500 trials at a 10% roll must produce at least one miss");
}

/// A non-goat species is never touched by the roll — both accessors stay
/// their default `true`, matching every other species' "meaningless"
/// reading.
#[test]
fn a_non_goat_species_always_reports_both_horns() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    for _ in 0..20 {
        let id = sim.spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
        let m = sim.get(id).expect("just spawned");
        assert!(m.has_left_horn() && m.has_right_horn());
    }
}

/// The wiring proof: `SimMob::snapshot` pushes `MetadataField::GoatHorns`
/// for a goat, carrying whatever [`SimMob::has_left_horn`]/
/// [`SimMob::has_right_horn`] actually are — and pushes nothing for a
/// species this field does not apply to, which is the control that rules
/// out "always pushed regardless of species".
#[test]
fn snapshot_carries_goat_horns_for_a_goat_and_nothing_for_a_pig() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let goat = sim.spawn_species("minecraft:goat".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
    let pig = sim.spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(1.0, 0.0, 0.0)).id();

    let goat_mob = sim.get_mut(goat).expect("spawned");
    goat_mob.has_left_horn = false;
    goat_mob.has_right_horn = true;
    let goat_snapshot = sim.get(goat).expect("spawned").snapshot();
    assert_eq!(
        goat_snapshot.metadata,
        vec![crate::protocol::MetadataField::GoatHorns { has_left: false, has_right: true }],
        "the snapshot must carry the mob's own current horn state, not the spawn default"
    );

    let pig_snapshot = sim.get(pig).expect("spawned").snapshot();
    assert!(
        !pig_snapshot.metadata.iter().any(|f| matches!(f, crate::protocol::MetadataField::GoatHorns { .. })),
        "a pig must never carry a GoatHorns field"
    );
}
