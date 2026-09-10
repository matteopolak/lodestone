//! Regression for the Overworld feature source that writes across a chunk edge.

use lodestone_server::overworld_chunk_source;
use lodestone_worldgen_parity::lifecycle::{
    LifecycleCompletion, LifecycleMaterializer, FEATURES_SOURCE_RADIUS,
};

const SEED: i64 = 42;
const TARGET: (i32, i32) = (-8, -8);

#[test]
fn north_source_writes_redstone_ore_into_target() {
    let mut materializer = LifecycleMaterializer::new(overworld_chunk_source(SEED));
    for source_x in TARGET.0 - FEATURES_SOURCE_RADIUS..=TARGET.0 + FEATURES_SOURCE_RADIUS {
        for source_z in TARGET.1 - FEATURES_SOURCE_RADIUS..=TARGET.1 + FEATURES_SOURCE_RADIUS {
            materializer.admit((source_x, source_z));
        }
    }

    let mut sequence = 0;
    for source_x in TARGET.0 - FEATURES_SOURCE_RADIUS..=TARGET.0 + FEATURES_SOURCE_RADIUS {
        for source_z in TARGET.1 - FEATURES_SOURCE_RADIUS..=TARGET.1 + FEATURES_SOURCE_RADIUS {
            materializer.complete_for_target(
                TARGET,
                (source_x, source_z),
                LifecycleCompletion::Features,
                sequence,
            );
            sequence += 1;
        }
    }

    assert_eq!(
        materializer.snapshot_for_packet(TARGET).block_state(14, -57, 15),
        "minecraft:deepslate_redstone_ore[lit=false]",
        "source (-8,-7) must spill ore into target (-8,-8) at local (14,-57,15)",
    );
}
