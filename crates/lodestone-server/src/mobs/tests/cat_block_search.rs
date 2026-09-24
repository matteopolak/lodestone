use super::*;

/// A real floor — see `leash_tests::flat_world`'s own doc comment for
/// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
/// Set at `y = -1` so it never collides with a search target block a
/// test places at `y = 0`.
fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

fn spawn_cat(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
    sim.spawn_species("minecraft:cat".parse().expect("valid key"), pos).id()
}

/// The headline case: a chest three blocks away, with clear headroom,
/// must be found and fed to the sit goal's seam — and the bed seam must
/// stay empty, since nothing bed-shaped exists in this world.
#[test]
fn a_cat_finds_a_nearby_chest_as_its_sit_target() {
    let mut world = flat_world();
    world.set_block(3, 0, 0, "minecraft:chest");
    let mut sim = MobSim::new(&world);
    let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

    sim.tick();

    let cat = sim.get(id).expect("just spawned");
    let target = cat.mob.cat_sit_target();
    assert_eq!(
        target,
        Some(Vec3::new(3.5, 1.0, 0.5)),
        "the sit target must be the chest's stand-on point, got {target:?}"
    );
    assert_eq!(cat.mob.cat_bed_target(), None, "no bed exists in this world");
}

/// A bed's *foot* part must feed both seams: the sit goal accepts a bed
/// foot (vanilla's own valid-target check's own third clause) and the
/// lie goal accepts any bed part.
#[test]
fn a_cat_finds_a_nearby_bed_foot_for_both_seams() {
    let mut world = flat_world();
    world.set_block(0, 0, 2, "minecraft:red_bed[facing=north,part=foot]");
    let mut sim = MobSim::new(&world);
    let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

    sim.tick();

    let cat = sim.get(id).expect("just spawned");
    assert!(cat.mob.cat_sit_target().is_some(), "a bed foot is a valid sit target too");
    assert!(cat.mob.cat_bed_target().is_some(), "a bed foot is a valid lie target");
}

/// A bed's *head* part must be excluded from the sit seam
/// (vanilla's own valid-target check's "not the head part" clause) but
/// still accepted by the lie seam, which makes no part distinction at
/// all (vanilla's own lie-goal valid-target check).
#[test]
fn a_beds_head_part_is_excluded_from_sitting_but_not_from_lying() {
    let mut world = flat_world();
    world.set_block(0, 0, 2, "minecraft:red_bed[facing=north,part=head]");
    let mut sim = MobSim::new(&world);
    let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

    sim.tick();

    let cat = sim.get(id).expect("just spawned");
    assert_eq!(cat.mob.cat_sit_target(), None, "a bed head must not be a sit target");
    assert!(cat.mob.cat_bed_target().is_some(), "a bed head is still a valid lie target");
}

/// An unlit furnace is not a valid sit target — only the lit furnace
/// state qualifies (vanilla's own valid-target check's second clause).
#[test]
fn an_unlit_furnace_is_not_a_sit_target_but_a_lit_one_is() {
    let mut world = flat_world();
    world.set_block(0, 0, 2, "minecraft:furnace[facing=north,lit=false]");
    let mut sim = MobSim::new(&world);
    let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));
    sim.tick();
    assert_eq!(
        sim.get(id).expect("spawned").mob.cat_sit_target(),
        None,
        "an unlit furnace must not be a sit target"
    );

    let mut world2 = flat_world();
    world2.set_block(0, 0, 2, "minecraft:furnace[facing=north,lit=true]");
    let mut sim2 = MobSim::new(&world2);
    let id2 = spawn_cat(&mut sim2, Vec3::new(0.0, 0.0, 0.0));
    sim2.tick();
    assert!(
        sim2.get(id2).expect("spawned").mob.cat_sit_target().is_some(),
        "a lit furnace must be a sit target"
    );
}

/// A chest with no headroom (a solid block directly above it) must be
/// rejected — vanilla's own valid-target check's
/// "is empty block one above" clause.
#[test]
fn a_chest_with_no_headroom_is_rejected() {
    let mut world = flat_world();
    world.set_block(0, 0, 2, "minecraft:chest");
    world.set_block(0, 1, 2, "minecraft:stone");
    let mut sim = MobSim::new(&world);
    let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

    sim.tick();

    assert_eq!(
        sim.get(id).expect("spawned").mob.cat_sit_target(),
        None,
        "a chest with a solid block on top must not be a sit target"
    );
}

/// An empty world (no chest, furnace or bed anywhere in range) must
/// leave both seams empty — the negative control.
#[test]
fn an_empty_world_finds_neither_target() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

    sim.tick();

    let cat = sim.get(id).expect("just spawned");
    assert_eq!(cat.mob.cat_sit_target(), None);
    assert_eq!(cat.mob.cat_bed_target(), None);
}

/// A non-cat species (a villager, which also walks around chests every
/// day) must never receive a cat block search feed — the species filter
/// is load-bearing, not merely a cost optimisation.
#[test]
fn a_non_cat_species_never_receives_a_cat_block_search_feed() {
    let mut world = flat_world();
    world.set_block(1, 0, 0, "minecraft:chest");
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();

    sim.tick();

    assert_eq!(sim.get(id).expect("spawned").mob.cat_sit_target(), None);
}
