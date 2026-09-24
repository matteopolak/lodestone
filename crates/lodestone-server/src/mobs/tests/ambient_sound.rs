use super::*;
use crate::effects::WorldEffect;
use lodestone_model::SoundCategory;

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

/// **The wire is real, not merely the derivation.** A hermetic call to
/// `effects::mob_ambient_sound` proves the string derivation; it proves
/// nothing about whether anything in `MobSim::tick` ever calls it — the
/// exact island shape this repo's own evidence standards warn about
/// repeatedly. Ticking a real, freshly spawned cow through the real
/// production `tick()` loop and draining `take_ambient_sounds()` is what
/// proves the roll actually fires and actually reaches the queue
/// `crate::tick::run_tick_loop` drains.
///
/// `ambient_sound_time` starts at `0` and this cow's RNG stream is
/// seeded deterministically from its id, so how many ticks the first
/// firing takes is a fixed, measured number for this exact scenario, not
/// a guess: measured at **tick 36** for this world/spawn order. The
/// 400-tick loop bound is over 10x that measured value, not a round
/// number reached for on its own — it exists only so a regression that
/// pushed the firing tick out shows up as a clean failure rather than an
/// infinite loop.
#[test]
fn a_real_mob_ticked_through_the_production_loop_eventually_vocalises() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));

    let mut fired = Vec::new();
    for _ in 0..400 {
        sim.tick();
        fired.extend(sim.take_ambient_sounds());
        if !fired.is_empty() {
            break;
        }
    }

    assert_eq!(
        fired.len(),
        1,
        "exactly one ambient sound must have fired within the margin, got {fired:?}"
    );
    match &fired[0] {
        WorldEffect::Sound { sound, category, .. } => {
            assert_eq!(sound, "minecraft:entity.cow.ambient");
            assert_eq!(*category, SoundCategory::Neutral);
        }
        other => panic!("expected a Sound effect, got {other:?}"),
    }
}

/// Owner batching must not turn a globally serial entity pass into
/// owner-major publication. These effects deliberately interleave origin,
/// negative, then origin chunks; the negative source is the control for
/// truncating `-0.5 / 16` to the origin instead of using floor and Euclidean
/// division.
#[test]
fn ambient_effect_batches_keep_negative_owners_and_restore_serial_order() {
    let effect = |data| crate::effects::WorldEffect::LevelEvent {
        event: crate::effects::SOUND_ZOMBIE_CONVERTED,
        pos: BlockPos::new(data, 64, 0),
        data,
        global: false,
    };
    let pending = vec![
        PendingEntityTickEffect {
            owner: entity_tick_owner(Vec3::new(0.5, 64.0, 0.5)),
            source: Vec3::new(0.5, 64.0, 0.5),
            effect: effect(0),
        },
        PendingEntityTickEffect {
            owner: entity_tick_owner(Vec3::new(-0.5, 64.0, -0.5)),
            source: Vec3::new(-0.5, 64.0, -0.5),
            effect: effect(1),
        },
        PendingEntityTickEffect {
            owner: entity_tick_owner(Vec3::new(16.5, 64.0, 0.5)),
            source: Vec3::new(16.5, 64.0, 0.5),
            effect: effect(2),
        },
        PendingEntityTickEffect {
            owner: entity_tick_owner(Vec3::new(0.25, 64.0, 0.25)),
            source: Vec3::new(0.25, 64.0, 0.25),
            effect: effect(3),
        },
    ];

    let batches = batch_entity_tick_effects(pending);
    assert_eq!(
        batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
        vec![
            EntityTickOwner::Chunk { cx: 0, cz: 0 },
            EntityTickOwner::Chunk { cx: -1, cz: -1 },
            EntityTickOwner::Chunk { cx: 1, cz: 0 },
        ],
        "owners are deterministic by first serial appearance, including negative chunks"
    );
    assert_eq!(
        batches[0]
            .effects()
            .iter()
            .map(|effect| effect.sequence)
            .collect::<Vec<_>>(),
        vec![0, 3],
        "one owner receives its effects in the serial order it produced them"
    );

    let mut serial: Vec<_> = batches
        .into_iter()
        .flat_map(|batch| batch.effects)
        .collect();
    serial.sort_unstable_by_key(|effect| effect.sequence);
    assert_eq!(
        serial.iter().map(|effect| effect.sequence).collect::<Vec<_>>(),
        vec![0, 1, 2, 3],
        "the central hand-off restores the old cross-owner visit order"
    );
}

/// **Control: the roll is load-bearing, not vacuous.** A dead mob
/// (`health <= 0.0`) must never roll — vanilla's own guard is
/// `isAlive() && …` — so ticking a mob whose health is forced to zero
/// for the same window above must produce nothing at all. Without this,
/// the subject test's "it fired" is not distinguishable from "everything
/// always fires".
#[test]
fn a_dead_mob_never_rolls_an_ambient_sound() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.get_mut(id).expect("spawned").health = 0.0;

    let mut fired = Vec::new();
    for _ in 0..400 {
        sim.tick();
        fired.extend(sim.take_ambient_sounds());
    }
    assert!(
        fired.is_empty(),
        "a mob at zero health must never roll an ambient sound, got {fired:?}"
    );
}

/// A hostile species reports the `Hostile` sound category, matching
/// `mob_vocalisation`'s own hurt/death split — checked with the same
/// production-loop wiring as the subject test above, not just the
/// hermetic derivation.
#[test]
fn a_hostile_mob_vocalises_on_the_hostile_category() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.spawn_species("minecraft:zombie".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));

    let mut fired = Vec::new();
    for _ in 0..400 {
        sim.tick();
        fired.extend(sim.take_ambient_sounds());
        if !fired.is_empty() {
            break;
        }
    }
    assert_eq!(fired.len(), 1, "expected exactly one ambient sound, got {fired:?}");
    match &fired[0] {
        WorldEffect::Sound { sound, category, .. } => {
            assert_eq!(sound, "minecraft:entity.zombie.ambient");
            assert_eq!(*category, SoundCategory::Hostile);
        }
        other => panic!("expected a Sound effect, got {other:?}"),
    }
}
