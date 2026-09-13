//! Regression for source-owned Nether fungus placement across chunk admission.

use lodestone_server::nether_chunk_source;
use lodestone_worldgen_parity::lifecycle::{LifecycleCompletion, LifecycleMaterializer};

const SOURCE: (i32, i32) = (32, 47);
const TARGET: (i32, i32) = (32, 48);
const TARGET_LOCAL: (i32, i32, i32) = (2, 51, 1);
const SOURCE_LOCAL_CONTROL: (i32, i32, i32) = (2, 51, 15);

#[test]
fn crimson_fungus_keeps_caps_and_stems_in_source_chunk() {
    let mut materializer = LifecycleMaterializer::new(nether_chunk_source(42));
    for chunk_z in SOURCE.1 - 2..=SOURCE.1 + 2 {
        for chunk_x in SOURCE.0 - 2..=SOURCE.0 + 2 {
            materializer.admit((chunk_x, chunk_z));
        }
    }

    materializer.complete(SOURCE, LifecycleCompletion::Features, 0);

    let source = materializer
        .resident_column(SOURCE)
        .expect("source was admitted before completion");
    assert_eq!(
        source.block_state(
            SOURCE_LOCAL_CONTROL.0,
            SOURCE_LOCAL_CONTROL.1,
            SOURCE_LOCAL_CONTROL.2,
        ),
        "minecraft:crimson_stem[axis=y]",
        "the source control proves crimson-fungus placement still ran",
    );

    let target = materializer
        .resident_column(TARGET)
        .expect("target was admitted before source completion");
    assert_eq!(
        target.block_state(TARGET_LOCAL.0, TARGET_LOCAL.1, TARGET_LOCAL.2),
        "minecraft:air",
        "a source fungus must not write its cap into an admitted neighbour",
    );
}
