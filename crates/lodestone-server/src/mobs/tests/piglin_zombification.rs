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

fn species_after(sim: &MobSim<'_>) -> Vec<String> {
    let mut names: Vec<String> = sim.snapshots().iter().map(|s| s.entity_type.to_string()).collect();
    names.sort();
    names
}

fn run(piglin_safe: bool, ticks: u32) -> Vec<String> {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.set_piglin_safe(piglin_safe);
    for name in ["minecraft:piglin", "minecraft:hoglin"] {
        sim.spawn_species(name.parse().expect("key"), Vec3::new(0.5, 0.0, 0.5));
    }
    for _ in 0..ticks {
        sim.tick();
    }
    species_after(&sim)
}

#[test]
fn piglins_and_hoglins_zombify_after_300_ticks_outside_a_piglin_safe_dimension() {
    let before = run(false, 300);
    assert_eq!(before, ["minecraft:hoglin", "minecraft:piglin"], "no conversion before the 300-tick threshold");
    let after = run(false, 302);
    assert_eq!(after, ["minecraft:zoglin", "minecraft:zombified_piglin"]);
}

#[test]
fn a_piglin_safe_dimension_never_zombifies_them() {
    let after = run(true, 400);
    assert_eq!(after, ["minecraft:hoglin", "minecraft:piglin"], "control: the safe dimension keeps both");
}
