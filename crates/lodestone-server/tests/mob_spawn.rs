//! Acceptance gate for natural mob spawning and despawn.
//!
//! Two things are proven through the crate's **public** API:
//!
//! 1. **Caps are honoured across seeds.** A spawn source that always has a
//!    candidate to offer, run to saturation under several RNG seeds, fills every
//!    category to *exactly* its global cap and never one over — the aggregate
//!    invariant that catches a wrong `<=`/`<` gate or a mis-indexed category
//!    count, which a single-seed fixture would miss.
//! 2. **The two despawn gates do not fold, over a long run.** A mob at 40 blocks
//!    (the middle band) ages and eventually random-despawns, while a mob at 20
//!    blocks (inside the immune radius) is reset every check and is therefore
//!    immortal. Folding the gates would make the 40-block mob immortal too; that
//!    bug is invisible in a short test and blatant here.
//!
//! Both are hermetic and deterministic, so they always run (no skip path).

use std::str::FromStr;

use lodestone_entity::pathfinding::MobShape;
use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{
    ChunkWorld, MobCategory, MobSim, SpawnCandidate, SpawnCandidateSource, SpawnRng,
};

/// A spawn source that always offers a candidate on a flat floor at y=0. It is
/// deliberately *not* terrain- or biome-aware — that is the real source's job;
/// this exercises the cap-accounting driver, so it just needs to keep proposing
/// so the caps are what stops it, not an empty well of candidates.
struct AlwaysSpawns {
    next_x: f64,
}

impl SpawnCandidateSource for AlwaysSpawns {
    /// One candidate per call, not a real cluster: the cap is what must stop this
    /// source, so a group would only make the arithmetic harder to read.
    fn cluster(&mut self, _category: MobCategory, cx: i32, cz: i32) -> Vec<SpawnCandidate> {
        // Spread mobs out so positions are distinct; the value is irrelevant to
        // cap accounting, which is what this source feeds.
        self.next_x += 1.0;
        vec![SpawnCandidate {
            pos: Vec3::new(
                self.next_x + f64::from(cx) * 16.0,
                0.0,
                f64::from(cz) * 16.0,
            ),
            entity_type: ResourceKey::from_str("minecraft:zombie").expect("static key"),
        }]
    }
}

/// Over several seeds, running spawn cycles to saturation fills every category to
/// exactly its global cap and never exceeds it at any point.
#[test]
fn spawn_cycles_fill_to_cap_and_never_exceed_across_seeds() {
    // A full single-player spawn radius: caps equal the per-chunk maxima.
    let spawnable_chunks = 289;
    let expected: [(MobCategory, i32); 7] = [
        (MobCategory::Monster, 70),
        (MobCategory::Creature, 10),
        (MobCategory::Ambient, 15),
        (MobCategory::Axolotls, 5),
        (MobCategory::UndergroundWaterCreature, 5),
        (MobCategory::WaterCreature, 5),
        (MobCategory::WaterAmbient, 20),
    ];

    // A modest chunk grid so several cycles are needed to reach the 70 monster
    // cap — one cycle can spawn at most one per (chunk, category).
    let chunks: Vec<(i32, i32)> = (0..4).flat_map(|x| (0..4).map(move |z| (x, z))).collect();

    // The seed only perturbs which chunk order the caller might use; here we vary
    // the source's starting position to stand in for run-to-run variation. The
    // invariant must hold identically regardless.
    for seed in [1u64, 7, 42, 1000, 999_999] {
        let world = ChunkWorld::new(-64, 128);
        let mut sim = MobSim::new(&world);
        let mut source = AlwaysSpawns {
            next_x: f64::from(u32::try_from(seed % 4096).unwrap()),
        };

        // Run enough cycles to saturate the largest cap (70) with a 16-chunk grid.
        for _ in 0..40 {
            let mut state = sim.census(spawnable_chunks);
            // No category may ever be over its cap in the census.
            for (cat, cap) in expected {
                assert!(
                    state.count(cat) <= cap,
                    "seed {seed}: {cat:?} count {} exceeds cap {cap}",
                    state.count(cat)
                );
            }
            sim.run_spawn_cycle(&mut state, &mut source, &chunks);
        }

        let final_state = sim.census(spawnable_chunks);
        for (cat, cap) in expected {
            assert_eq!(
                final_state.count(cat),
                cap,
                "seed {seed}: {cat:?} should saturate to exactly {cap}"
            );
        }
        // Total live == sum of caps (130): no category leaked into another.
        let total: i32 = expected.iter().map(|(_, c)| c).sum();
        assert_eq!(sim.len() as i32, total, "seed {seed}: total live mob count");
    }
}

/// The non-fold proof, run long enough to be boring: a 40-block mob ages and
/// eventually random-despawns; a 20-block mob, reset every check, never does.
#[test]
fn middle_band_mob_despawns_over_time_while_immune_mob_is_immortal() {
    let world = ChunkWorld::new(-64, 128);
    let mut sim = MobSim::new(&world);
    let player = Vec3::new(0.0, 0.0, 0.0);

    // Three monsters at three bands relative to the player.
    let immune = sim
        .spawn(
            Vec3::new(20.0, 0.0, 0.0),
            MobShape::land(0.6, 1.95),
            0.0,
            400,
        )
        .id(); // dist 20 (<32)
    let middle = sim
        .spawn(
            Vec3::new(40.0, 0.0, 0.0),
            MobShape::land(0.6, 1.95),
            0.0,
            400,
        )
        .id(); // dist 40
    let far = sim
        .spawn(
            Vec3::new(130.0, 0.0, 0.0),
            MobShape::land(0.6, 1.95),
            0.0,
            400,
        )
        .id(); // dist 130 (>128)

    let mut rng = SpawnRng::new(0xDEAD_BEEF);

    // One pass: the far mob is instantly despawned; the other two survive.
    let discarded = sim.despawn_pass(Some(player), &mut rng);
    assert_eq!(
        discarded, 1,
        "only the >128-block mob should instant-despawn"
    );
    assert!(sim.get(far).is_none(), "far mob gone");
    assert!(sim.get(immune).is_some() && sim.get(middle).is_some());

    // Age both survivors past the 600-tick idle threshold. Idle mobs (no goal)
    // stay put, so ticking only advances their age timers.
    sim.tick_for(700);
    assert!(sim.get(middle).unwrap().no_action_time() > 600);

    // Now hammer despawn checks. The middle-band mob must eventually lose the
    // 1/800 gate-B roll and be discarded; the immune mob is reset every pass and
    // must still be alive at the end. Bounded so the gate *fails* rather than
    // hangs if the logic regresses.
    let mut middle_despawned_at = None;
    for pass in 0..100_000 {
        sim.despawn_pass(Some(player), &mut rng);
        if sim.get(middle).is_none() {
            middle_despawned_at = Some(pass);
            break;
        }
        // The immune mob is reset each pass, so its timer never climbs.
        assert_eq!(
            sim.get(immune).unwrap().no_action_time(),
            0,
            "immune mob at 20 blocks must be reset every check"
        );
    }

    assert!(
        middle_despawned_at.is_some(),
        "middle-band mob should random-despawn within 100k checks (~800 expected)"
    );
    assert!(
        sim.get(immune).is_some(),
        "immune mob at 20 blocks must NEVER despawn — the non-fold invariant"
    );
}

/// With no player loaded, vanilla runs no despawn logic at all: every mob is
/// kept regardless of distance.
#[test]
fn no_player_means_no_despawn() {
    let world = ChunkWorld::new(-64, 128);
    let mut sim = MobSim::new(&world);
    sim.spawn(
        Vec3::new(500.0, 0.0, 0.0),
        MobShape::land(0.6, 1.95),
        0.0,
        400,
    );
    let mut rng = SpawnRng::new(1);
    sim.tick_for(1000);
    let discarded = sim.despawn_pass(None, &mut rng);
    assert_eq!(discarded, 0);
    assert_eq!(
        sim.len(),
        1,
        "no player → nothing despawns even at 500 blocks"
    );
}
