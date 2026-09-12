//! Regression coverage for vegetation feature iteration order.

use lodestone_server::overworld_chunk_source;
use lodestone_worldgen_parity::lifecycle::{LifecycleCompletion, LifecycleMaterializer};

const TARGET: (i32, i32) = (-9, -9);
const WITNESS: (i32, i32, i32) = (15, -31, 2);

#[test]
fn vegetation_patch_stream_keeps_seed_42_target_witness() {
    let mut materializer = LifecycleMaterializer::new(overworld_chunk_source(42));
    for x in TARGET.0 - 2..=TARGET.0 + 2 {
        for z in TARGET.1 - 2..=TARGET.1 + 2 {
            materializer.admit((x, z));
        }
    }

    let mut sequence = 0;
    for x in TARGET.0 - 1..=TARGET.0 + 1 {
        for z in TARGET.1 - 1..=TARGET.1 + 1 {
            materializer.complete_for_target(
                TARGET,
                (x, z),
                LifecycleCompletion::Features,
                sequence,
            );
            sequence += 1;
        }
    }
    materializer.finish_target(TARGET);

    assert_eq!(
        materializer
            .snapshot_for_packet(TARGET)
            .block_state(WITNESS.0, WITNESS.1, WITNESS.2),
        "minecraft:azalea",
        "vegetation patch iteration must retain insertion order for its random stream",
    );
}
