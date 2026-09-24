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

fn spawn_villager(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
    sim.spawn_species("minecraft:villager".parse().expect("valid key"), pos).id()
}

/// The headline case: a villager standing near an unclaimed bed claims it
/// over a real tick, and the claim is visible through the same live query
/// the raid trigger needs (`occupied_homes_in_range`).
#[test]
fn a_villager_claims_a_nearby_bed_through_a_real_tick_and_it_becomes_findable() {
    let mut world = flat_world();
    world.set_block(3, 0, 0, "minecraft:red_bed[facing=north,occupied=false,part=foot]");
    let mut sim = MobSim::new(&world);
    let id = spawn_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));

    sim.tick();

    let claimed = sim.get(id).expect("just spawned").bed();
    assert_eq!(
        claimed,
        Some(BlockPos::new(3, 0, 0)),
        "the villager must claim the only nearby bed"
    );
    let found = sim.occupied_homes_in_range(BlockPos::new(0, 0, 0), 64);
    assert_eq!(
        found,
        vec![BlockPos::new(3, 0, 0)],
        "the raid trigger's live query must see the claim the same tick it happens"
    );
}

/// Two villagers, one bed — the discriminating shape every claim gate in
/// this file uses: a single-villager test would pass under an
/// implementation with no occupancy at all.
#[test]
fn a_second_villager_cannot_claim_an_already_claimed_bed_through_the_sim() {
    let mut world = flat_world();
    world.set_block(3, 0, 0, "minecraft:red_bed[facing=north,occupied=false,part=foot]");
    let mut sim = MobSim::new(&world);
    let first = spawn_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));
    let second = spawn_villager(&mut sim, Vec3::new(1.0, 0.0, 0.0));

    sim.tick();

    let first_bed = sim.get(first).expect("spawned").bed();
    let second_bed = sim.get(second).expect("spawned").bed();
    assert_eq!(first_bed, Some(BlockPos::new(3, 0, 0)));
    assert_eq!(
        second_bed, None,
        "the bed has one ticket and the closer villager already holds it"
    );
}

/// A non-villager species standing right next to a bed must never claim
/// it — the species filter is load-bearing, the same control
/// `cat_block_search_tests` already runs for its own search.
#[test]
fn a_non_villager_species_never_claims_a_bed() {
    let mut world = flat_world();
    world.set_block(1, 0, 0, "minecraft:red_bed[facing=north,occupied=false,part=foot]");
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();

    sim.tick();

    assert_eq!(sim.get(id).expect("spawned").bed(), None);
    assert!(sim.occupied_homes_in_range(BlockPos::new(0, 0, 0), 64).is_empty());
}
