//! Regression coverage for vegetation feature iteration order.

use lodestone_server::overworld_chunk_source;
use lodestone_worldgen_parity::lifecycle::{LifecycleCompletion, LifecycleMaterializer};

const TARGET: (i32, i32) = (-9, -9);
const WITNESS: (i32, i32, i32) = (15, -31, 2);

#[test]
fn vegetation_patch_stream_uses_seed_42_reference_hash_order() {
    let mut materializer = LifecycleMaterializer::new(overworld_chunk_source(42));
    for x in TARGET.0 - 2..=TARGET.0 + 2 {
        for z in TARGET.1 - 2..=TARGET.1 + 2 {
            materializer.admit((x, z));
        }
    }

    materializer.complete_for_target(TARGET, TARGET, LifecycleCompletion::Features, 0);
    materializer.finish_target(TARGET);

    assert_eq!(
        materializer
            .snapshot_for_packet(TARGET)
            .block_state(WITNESS.0, WITNESS.1, WITNESS.2),
        "minecraft:short_grass",
        "vegetation patch iteration must retain reference hash-table order for its random stream",
    );
}
