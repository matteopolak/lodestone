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

fn baby_field(metadata: &[MetadataField]) -> Option<bool> {
    metadata.iter().find_map(|f| match f {
        MetadataField::Baby(b) => Some(*b),
        _ => None,
    })
}

/// **Positive arm**: every species this sim scopes ageing to
/// (vanilla's own ageable-mob breedable-animal set plus the zombie family — see
/// `SimMob::snapshot`'s own comment for the mechanical derivation off
/// `.cache/mc/26.2/src/`) must push `MetadataField::Baby(false)` as a
/// freshly-spawned adult.
///
/// **Negative control**: index 16's other real claimants — `creeper`
/// (vanilla's own swell-direction metadata field, an `INT`, already the producer for a
/// different variant at this same index), `ghast`
/// (vanilla's own "is charging" metadata field) and `phantom` (vanilla's
/// own size metadata field) — must
/// push no `Baby` field at all. These are exactly the entities a shared
/// "is baby" encoder would corrupt: a ghast told `Baby(false)` reads to
/// a real client as "not charging", and a phantom's size becomes `0`.
///
/// Collected rather than asserted per-iteration so one run reports every
/// wrong species, not just the first.
#[test]
fn eligible_species_emit_baby_and_only_those_do() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let mut wrong: Vec<String> = Vec::new();

    for species in [
        "cow",
        "mooshroom",
        "sheep",
        "pig",
        "chicken",
        "rabbit",
        "wolf",
        "zombie",
        "husk",
        "zombie_villager",
        "drowned",
        "zombified_piglin",
    ] {
        let key = format!("minecraft:{species}").parse().expect("valid key");
        let id = sim.spawn_species(key, above_floor()).id();
        let metadata = sim.get(id).expect("spawned").snapshot().metadata;
        if baby_field(&metadata) != Some(false) {
            wrong.push(format!(
                "{species}: expected Baby(false), metadata was {metadata:?}"
            ));
        }
    }

    for species in ["creeper", "ghast", "phantom"] {
        let key = format!("minecraft:{species}").parse().expect("valid key");
        let id = sim.spawn_species(key, above_floor()).id();
        let metadata = sim.get(id).expect("spawned").snapshot().metadata;
        if baby_field(&metadata).is_some() {
            wrong.push(format!(
                "{species}: must emit no Baby field at all, metadata was {metadata:?}"
            ));
        }
    }

    assert!(wrong.is_empty(), "{wrong:?}");
}

/// **The grown-up transition.** A baby that matures must produce a
/// snapshot whose `Baby` is `Some(false)`, not absent — an absent field
/// leaves the client holding whatever `Baby(true)` it was sent on
/// arrival, so the mob would stay a baby on screen forever. See
/// `SimMob::snapshot`'s own doc comment for why this variant is pushed
/// unconditionally rather than only while `is_baby()` is true.
#[test]
fn a_grown_up_baby_reports_baby_false_not_absent() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:zombie".parse().expect("valid key"), above_floor())
        .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE)
        .id();
    let baby_metadata = sim.get(id).expect("spawned").snapshot().metadata;
    assert_eq!(
        baby_field(&baby_metadata),
        Some(true),
        "a freshly spawned baby zombie must report Baby(true), got {baby_metadata:?}"
    );

    sim.get_mut(id).expect("spawned").set_age(0);
    let adult_metadata = sim.get(id).expect("still spawned").snapshot().metadata;
    assert!(
        adult_metadata.iter().any(|f| matches!(f, MetadataField::Baby(_))),
        "the grown-up snapshot must still carry a Baby field, not omit it: {adult_metadata:?}"
    );
    assert_eq!(
        baby_field(&adult_metadata),
        Some(false),
        "after growing up the field must flip to Baby(false), got {adult_metadata:?}"
    );
}
