use lodestone_server::nether_chunk_source;
use lodestone_worldgen_parity::lifecycle::{LifecycleCompletion, LifecycleMaterializer};

#[test]
fn target_scoped_replay_retains_basalt_then_replaces_quartz() {
    // These are the first three target tickets from the bounded stream witness.
    // The two cells are independently observed packet states: (91,90)'s
    // basalt at local (15,6,5), and (92,90)'s quartz at local (0,18,11).
    let targets = [(90, 90), (91, 90), (92, 90)];
    let mut materializer = LifecycleMaterializer::new(nether_chunk_source(42));
    for z in 88..=92 {
        for x in 88..=94 {
            materializer.admit((x, z));
        }
    }

    let mut sequence = 0_u64;
    for target in targets {
        for source_x in target.0 - 1..=target.0 + 1 {
            for source_z in target.1 - 1..=target.1 + 1 {
                materializer.complete_for_target(
                    target,
                    (source_x, source_z),
                    LifecycleCompletion::Features,
                    sequence,
                );
                sequence += 1;
            }
        }
        materializer.finish_target(target);
    }

    assert_eq!(
        materializer
            .snapshot_for_packet((91, 90))
            .block_state(15, 6, 5),
        "minecraft:basalt[axis=y]",
        "the 91,90 packet must retain its basalt witness",
    );
    assert_eq!(
        materializer
            .snapshot_for_packet((92, 90))
            .block_state(0, 18, 11),
        "minecraft:nether_quartz_ore",
        "replaying the 92,90 source must not leave the prior target's basalt",
    );
}

#[test]
fn adjacent_target_packets_retain_the_external_neighbour_boundary() {
    // The captured 26.2 stream emits adjacent targets (380,380) and
    // (381,380).  Each target FEATURES body owns its target column; the
    // later east source (382,380) completes before the second packet and
    // supplies its crimson-root spill at local (15,77,5).
    let targets = [(380, 380), (381, 380)];
    let mut materializer = LifecycleMaterializer::new(nether_chunk_source(42));
    for z in 378..=382 {
        for x in 378..=383 {
            materializer.admit((x, z));
        }
    }

    for (sequence, target) in targets.into_iter().enumerate() {
        materializer.complete_for_target(
            target,
            target,
            LifecycleCompletion::Features,
            sequence as u64,
        );
        materializer.finish_target(target);
    }

    let first = materializer.snapshot_for_packet((380, 380));
    assert_eq!(
        first.block_state(8, 50, 1),
        "minecraft:crimson_roots",
        "the first captured packet keeps its target FEATURES witness",
    );
    assert_eq!(
        materializer
            .resident_column((381, 380))
            .expect("adjacent target was admitted")
            .block_state(15, 77, 5),
        "minecraft:air",
        "the east-source witness is absent before its completion",
    );

    materializer.complete((382, 380), LifecycleCompletion::Features, 2);
    let second = materializer.snapshot_for_packet((381, 380));
    assert_eq!(
        second.block_state(3, 56, 6),
        "minecraft:crimson_roots",
        "the second captured packet keeps its target FEATURES witness",
    );
    assert_eq!(
        second.block_state(15, 77, 5),
        "minecraft:crimson_roots",
        "the second packet includes the completed east-neighbour spill",
    );
}
