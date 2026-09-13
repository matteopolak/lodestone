//! Regression for source-border fungus reads feeding the following vegetation pass.

use lodestone_server::nether_chunk_source;
use lodestone_worldgen_parity::lifecycle::{LifecycleCompletion, LifecycleMaterializer};

#[test]
fn crimson_forest_roots_keep_the_source_border_context() {
    let source = nether_chunk_source(42);
    let column = source.generator().column(32, 48);

    assert_eq!(
        column.block_state(14, 43, 14),
        "minecraft:crimson_roots",
        "fungus border writes must remain visible while the source's later vegetation is placed",
    );
}

#[test]
fn lifecycle_wavefront_keeps_roots_and_clears_prior_fungus_border() {
    let mut materializer = LifecycleMaterializer::new(nether_chunk_source(42));
    for z in 46..=50 {
        for x in 30..=34 {
            materializer.admit((x, z));
        }
    }
    for (sequence, source) in (31..=33)
        .flat_map(|x| (47..=49).map(move |z| (x, z)))
        .enumerate()
    {
        materializer.complete(source, LifecycleCompletion::Features, sequence as u64);
    }
    let target = materializer.snapshot_for_packet((32, 48));
    assert_eq!(target.block_state(14, 43, 14), "minecraft:crimson_roots");
    assert_eq!(target.block_state(2, 51, 1), "minecraft:air");
    assert_eq!(target.block_state(1, 45, 5), "minecraft:nether_wart_block");
}
