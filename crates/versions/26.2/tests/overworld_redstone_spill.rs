//! Regression for the Overworld feature source that writes across a chunk edge.

use lodestone_server::overworld_chunk_source;
use lodestone_worldgen_parity::lifecycle::{LifecycleMaterializer, FEATURES_SOURCE_RADIUS};

const SEED: i64 = 42;
const TARGET: (i32, i32) = (-8, -8);

#[test]
fn target_owned_features_write_only_from_the_target_origin() {
    let mut materializer = LifecycleMaterializer::new(overworld_chunk_source(SEED));
    for source_x in TARGET.0 - FEATURES_SOURCE_RADIUS..=TARGET.0 + FEATURES_SOURCE_RADIUS {
        for source_z in TARGET.1 - FEATURES_SOURCE_RADIUS..=TARGET.1 + FEATURES_SOURCE_RADIUS {
            materializer.admit((source_x, source_z));
        }
    }

    materializer.complete_target_features_observing(TARGET, 0, |_| {});
    materializer.finish_target(TARGET);

    let snapshot = materializer.snapshot_for_packet(TARGET);
    let actual = snapshot.block_state(14, -57, 15);
    assert_eq!(
        actual,
        "minecraft:deepslate[axis=y]",
        "target-owned FEATURES must run the target origin exactly once",
    );
}
