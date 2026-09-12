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
fn source_centred_global_replay_retains_cross_target_order() {
    let targets = [(90, 90), (91, 90), (92, 90)];
    let mut materializer = LifecycleMaterializer::new(nether_chunk_source(42));
    for z in 88..=92 {
        for x in 88..=94 {
            materializer.admit((x, z));
        }
    }

    let mut completed = std::collections::BTreeSet::new();
    let mut sequence = 0_u64;
    for target in targets {
        for source_x in target.0 - 1..=target.0 + 1 {
            for source_z in target.1 - 1..=target.1 + 1 {
                let source = (source_x, source_z);
                if completed.insert(source) {
                    materializer.complete_for_target(
                        target,
                        source,
                        LifecycleCompletion::Features,
                        sequence,
                    );
                    sequence += 1;
                }
            }
        }
        materializer.finish_target(target);
        if target == (91, 90) {
            assert_eq!(
                materializer
                    .snapshot_for_packet(target)
                    .block_state(15, 6, 5),
                "minecraft:basalt[axis=y]",
                "the 91,90 packet must retain its cross-target basalt witness",
            );
        }
        if target == (92, 90) {
            assert_eq!(
                materializer
                    .snapshot_for_packet(target)
                    .block_state(0, 18, 11),
                "minecraft:nether_quartz_ore",
                "the 92,90 packet must retain its later quartz witness",
            );
        }
    }
}
