//! Regression coverage for the fixed End platform at the lifecycle source seam.

use lodestone_server::end_chunk_source;
use lodestone_worldgen_parity::lifecycle::{LifecycleCompletion, LifecycleMaterializer};

const SEED: i64 = 42;
const PLATFORM_SOURCE: (i32, i32) = (6, 0);
const NON_PLATFORM_SOURCE: (i32, i32) = (5, 0);
const TARGET: (i32, i32) = (6, 0);
const PLATFORM_CELL: (i32, i32, i32) = (98, 48, 0);

#[test]
fn fixed_platform_is_emitted_by_its_source_and_reaches_lifecycle_heightmaps() {
    let source = end_chunk_source(SEED);
    let direct = source.generator().parity_source_decoration_with_overrides(
        PLATFORM_SOURCE.0,
        PLATFORM_SOURCE.1,
        &[],
    );
    assert!(
        direct
            .spills
            .iter()
            .any(|spill| spill.position == PLATFORM_CELL && spill.state == "minecraft:obsidian"),
        "the source containing the fixed placement origin must emit its platform spill",
    );

    let negative = source.generator().parity_source_decoration_with_overrides(
        NON_PLATFORM_SOURCE.0,
        NON_PLATFORM_SOURCE.1,
        &[],
    );
    assert!(
        !negative.spills.iter().any(|spill| spill.position == PLATFORM_CELL),
        "a neighboring source must not claim the fixed placement",
    );

    let mut materializer = LifecycleMaterializer::new(end_chunk_source(SEED));
    materializer.admit(TARGET);
    materializer.complete(TARGET, LifecycleCompletion::Features, 0);
    let column = materializer
        .resident_column(TARGET)
        .expect("the target was admitted before its feature completion");
    assert_eq!(column.block_state(2, 48, 0), "minecraft:obsidian");
    assert_eq!(
        column
            .client_heightmaps()
            .expect("FEATURES entry primes client heightmaps")
            .get(1)
            .expect("WORLD_SURFACE heightmap")
            .get(2, 0),
        49,
        "the platform spill must update the retained world-surface heightmap",
    );
}

