//! Acceptance gate for the natural spawn cycle (issues #221, #222).
//!
//! The engine halves (cap arithmetic, despawn gates) are already gated by
//! `mob_spawn.rs`. What was missing — and what this covers — is that something
//! *drives* them against real terrain, real biome spawn lists and real light:
//!
//! 1. **A lit grass plain populates with the plains creature list**, and only with
//!    species that list actually names. This is the whole feature: before it, a
//!    world held exactly the mobs `seed_demo_mobs` placed and nothing else, ever.
//! 2. **A sealed dark room populates with monsters**, and the same room lit by a
//!    glowstone floor does not. The light half is where a spawn table goes
//!    silently wrong, and the direction is the assertion: darkness spawns
//!    monsters, light does not.
//!
//! ## Why the world is hand-built rather than generated
//!
//! `ChunkWorld` is fed by hand so the test is deterministic and fast, but the
//! *biome data* is the real bundled table (`bundled_biome_spawners`) and the light
//! is the real engine over the real per-state census — so what is stubbed here is
//! only the surface worldgen would have produced, which has its own oracle. Naming
//! `minecraft:plains` means this really is asking "what does the plains spawn list
//! say", not "what does a fixture say".

use std::str::FromStr;

use lodestone_model::{Difficulty, ResourceKey, Vec3};
use lodestone_server::natural_spawn::NaturalSpawner;
use lodestone_server::{
    ChunkColumn, ChunkWorld, MobCategory, MobSim, PlayerPerception, SpawnCandidateSource,
};

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
/// Surface height. Must be inside the plains-relevant band and above sea level so
/// the water lists do not compete.
const FLOOR: i32 = 70;
const RADIUS: i32 = 3;

/// Chunk coordinates the cycle runs over — a 7×7 area, so the caps are non-zero
/// (`70 × 49 / 289 = 11` monsters, `10 × 49 / 289 = 1` creature).
fn chunks() -> Vec<(i32, i32)> {
    (-RADIUS..=RADIUS)
        .flat_map(|cz| (-RADIUS..=RADIUS).map(move |cx| (cx, cz)))
        .collect()
}

/// A `ChunkWorld` of `minecraft:plains` columns: stone up to `FLOOR - 1`, a grass
/// surface at `FLOOR`, air above. `roof` seals the sky at `FLOOR + 4`, which is
/// what turns the surface dark.
fn plains_world(roof: Option<&str>) -> ChunkWorld {
    let columns = chunks().into_iter().map(|(cx, cz)| {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for qy in 0..column.biome_y_quarts() {
            for qz in 0..4 {
                for qx in 0..4 {
                    column.set_biome_cell(qx, qy, qz, "minecraft:plains");
                }
            }
        }
        for z in 0..16 {
            for x in 0..16 {
                for y in MIN_Y..FLOOR {
                    column.set_block(x, y, z, "minecraft:stone");
                }
                column.set_block(x, FLOOR, z, "minecraft:grass_block[snowy=false]");
                if let Some(roof) = roof {
                    column.set_block(x, FLOOR + 4, z, roof);
                }
            }
        }
        ((cx, cz), column)
    });
    ChunkWorld::from_columns(columns)
}

/// A player standing at the centre of the area. Every candidate must be more than
/// 24 blocks away and inside the category's despawn radius, so a 7×7 area around
/// one central player leaves a real annulus to spawn in.
fn player() -> PlayerPerception {
    PlayerPerception {
        position: Vec3::new(0.5, f64::from(FLOOR) + 1.0, 0.5),
        held_item: None,
        view_direction: Vec3::new(0.0, 0.0, 1.0),
    }
}

/// Runs `cycles` spawn cycles against `world`, returning the species census of
/// what ended up alive.
fn populate(world: ChunkWorld, cycles: usize) -> Vec<(String, MobCategory)> {
    populate_at(world, cycles, Difficulty::Normal)
}

/// [`populate`] at a chosen world difficulty — the input `SpawnPlacements`'
/// peaceful guard turns on.
fn populate_at(
    world: ChunkWorld,
    cycles: usize,
    difficulty: Difficulty,
) -> Vec<(String, MobCategory)> {
    // Two handles onto the same terrain, because the two consumers now want
    // different ownership. `MobSim` still borrows for `'static` (the `Box::leak`
    // `MobHandle::new` documents), while `NaturalSpawner` refcounts — the
    // production spawner's view is rebuilt as the tick area follows the player, so
    // leaking one per rebuild would leak for the life of the process. An identical
    // clone rather than a shared allocation keeps this fixture honest about which
    // consumer reads which.
    let spawn_world = std::sync::Arc::new(world.clone());
    let world: &'static ChunkWorld = Box::leak(Box::new(world));
    let mut sim = MobSim::new(world);
    sim.set_players(vec![player()]);
    let mut spawner = NaturalSpawner::new(
        lodestone_server::bundled_biome_spawners().clone(),
        0xB0_0B_1E5,
    );
    spawner.set_difficulty(difficulty);
    let all = chunks();
    for cycle in 0..cycles {
        spawner.begin_cycle(
            std::sync::Arc::clone(&spawn_world),
            cycle as u64,
            vec![player().position],
        );
        let mut state = sim.census(i32::try_from(all.len()).unwrap());
        sim.run_spawn_cycle(&mut state, &mut spawner, &all);
    }
    sim.iter()
        .map(|m| (m.entity_type().to_string(), m.category()))
        .collect()
}

/// The plains creature list, straight out of the bundled biome document — the
/// expected value comes from the data, not from this file's opinion of it.
fn plains_creatures() -> Vec<String> {
    let spawner = NaturalSpawner::new(lodestone_server::bundled_biome_spawners().clone(), 0);
    let listed: Vec<String> = spawner
        .species_for("minecraft:plains", MobCategory::Creature)
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert!(
        !listed.is_empty(),
        "the bundled plains document must name creature spawners"
    );
    listed
}

/// **A fresh, lit grass plain populates**, and only with species the plains
/// creature list names.
#[test]
fn a_lit_plain_populates_with_the_plains_creature_list() {
    let alive = populate(plains_world(None), 400);
    let creatures: Vec<&(String, MobCategory)> = alive
        .iter()
        .filter(|(_, c)| *c == MobCategory::Creature)
        .collect();
    assert!(
        !creatures.is_empty(),
        "a lit grass plain must spawn animals; got {alive:?}"
    );

    let listed = plains_creatures();
    for (species, _) in &creatures {
        assert!(
            listed.contains(species),
            "{species} is not in the plains creature list {listed:?}"
        );
    }
    // No monster may spawn on a fully sky-lit surface: `isDarkEnoughToSpawn`'s
    // block-light ceiling is 0 and its brightness test is against `nextInt(8)`,
    // and full daylight is 15.
    assert!(
        !alive.iter().any(|(_, c)| *c == MobCategory::Monster),
        "daylight must not spawn monsters; got {alive:?}"
    );
}

/// **A sealed dark plain spawns monsters**, and the same room floored with
/// glowstone does not. Direction is the claim: this is the light half of the
/// table, and it is the half that goes silently wrong.
#[test]
fn darkness_spawns_monsters_and_light_suppresses_them() {
    let dark = populate(plains_world(Some("minecraft:stone")), 400);
    let monsters = dark
        .iter()
        .filter(|(_, c)| *c == MobCategory::Monster)
        .count();
    assert!(
        monsters > 0,
        "a sealed dark room must spawn monsters; got {dark:?}"
    );

    // The same geometry, but the surface itself emits 15. `isDarkEnoughToSpawn`
    // rejects any block light above 0, so this must be zero monsters — not
    // "fewer".
    let mut lit = plains_world(Some("minecraft:stone"));
    for (cx, cz) in chunks() {
        for z in 0..16 {
            for x in 0..16 {
                lit.set_block(cx * 16 + x, FLOOR, cz * 16 + z, "minecraft:glowstone");
            }
        }
    }
    let lit = populate(lit, 400);
    assert_eq!(
        lit.iter()
            .filter(|(_, c)| *c == MobCategory::Monster)
            .count(),
        0,
        "a glowstone floor must suppress every monster spawn; got {lit:?}"
    );
}

/// **Peaceful proposes no forbidden species at all**, and the discriminating pair
/// is the same sealed dark room on Normal — which the test above already proves
/// fills with monsters. A gate on Peaceful alone cannot tell "the guard works" from
/// "this room never spawned anything".
///
/// The claim is about *proposal*, not eviction: `MobSim::remove_monsters` is not
/// called here at all, so a monster reaching this census is one
/// `SpawnPlacements.checkSpawnRules` let through. That distinction is the whole
/// point — with only the eviction half, a monster still existed for one tick, long
/// enough for the tick loop to publish it and a client to be sent `ADD_ENTITY`
/// followed by `REMOVE_ENTITIES`, so monsters blinked on Peaceful.
///
/// Passive spawns are deliberately **not** asserted to be zero: Peaceful forbids
/// only `notInPeaceful` species, and a sealed room's floor is still plains, so a
/// sheep here is correct. Asserting an empty census would be asserting the wrong
/// rule.
#[test]
fn peaceful_proposes_no_forbidden_species_and_normal_does() {
    let normal = populate_at(plains_world(Some("minecraft:stone")), 400, Difficulty::Normal);
    let normal_monsters: Vec<&String> = normal
        .iter()
        .filter(|(species, _)| !lodestone_server::allowed_in_peaceful(strip_namespace(species)))
        .map(|(species, _)| species)
        .collect();
    assert!(
        !normal_monsters.is_empty(),
        "the control failed: this sealed dark room must spawn forbidden species on \
         Normal, or the Peaceful assertion below is about nothing; got {normal:?}"
    );

    let peaceful = populate_at(
        plains_world(Some("minecraft:stone")),
        400,
        Difficulty::Peaceful,
    );
    let leaked: Vec<&String> = peaceful
        .iter()
        .filter(|(species, _)| !lodestone_server::allowed_in_peaceful(strip_namespace(species)))
        .map(|(species, _)| species)
        .collect();
    assert!(
        leaked.is_empty(),
        "Peaceful must propose no notInPeaceful species; {} leaked through \
         (Normal proposed {normal_monsters:?} in the same room): {leaked:?}",
        leaked.len()
    );
}

/// `minecraft:zombie` → `zombie`, the key
/// [`lodestone_server::allowed_in_peaceful`] takes.
fn strip_namespace(key: &str) -> &str {
    key.split_once(':').map_or(key, |(_, path)| path)
}

/// A species with no `SpawnPlacements` registration is never proposed, so a
/// candidate always names something the spawner can actually place. The guard
/// against a table that silently falls back to "no restrictions".
#[test]
fn every_candidate_names_a_registered_species() {
    let world = std::sync::Arc::new(plains_world(None));
    let mut spawner = NaturalSpawner::new(lodestone_server::bundled_biome_spawners().clone(), 7);
    let mut seen = 0;
    for cycle in 0..200 {
        spawner.begin_cycle(std::sync::Arc::clone(&world), cycle, vec![player().position]);
        for (cx, cz) in chunks() {
            for category in MobCategory::SPAWNING {
                for candidate in spawner.cluster(category, cx, cz) {
                    seen += 1;
                    assert!(
                        lodestone_server::natural_spawn::spawn_rule(candidate.entity_type.path())
                            .is_some(),
                        "{} was proposed with no SpawnPlacements row",
                        candidate.entity_type
                    );
                }
            }
        }
    }
    assert!(seen > 0, "the spawner proposed nothing at all");
    let _ = ResourceKey::from_str("minecraft:pig").unwrap();
}
