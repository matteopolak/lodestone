use lodestone_server::nether_chunk_source;
use lodestone_worldgen_parity::lifecycle::{LifecycleCompletion, LifecycleMaterializer};

/// A Nether feature source can write across the target boundary before that
/// destination reaches FEATURES. The write is resident state, not a packet-
/// local speculative edit; retaining it is what lets the next adjacent ticket
/// observe the same block without replaying the source.
#[test]
fn cross_target_source_write_survives_target_boundary() {
    let first_target = (90, 90);
    let later_target = (91, 90);
    let mut materializer = LifecycleMaterializer::new(nether_chunk_source(42));
    for z in 88..=92 {
        for x in 88..=93 {
            materializer.admit((x, z));
        }
    }

    let mut sequence = 0;
    for x in 89..=91 {
        for z in 89..=91 {
            materializer.complete_for_target(
                first_target,
                (x, z),
                LifecycleCompletion::Features,
                sequence,
            );
            sequence += 1;
        }
    }
    materializer.finish_target(first_target);

    assert_eq!(
        materializer
            .resident_column(later_target)
            .expect("later target was admitted")
            .block_state(15, 6, 5),
        "minecraft:basalt[axis=y]",
        "the (90,89) source's basalt write must remain resident for (91,90)",
    );
}
