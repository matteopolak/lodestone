//! Focused controls for Nether local-modification basalt pillars.

use lodestone_server::nether_chunk_source;

#[test]
fn local_modification_pillars_reach_both_reported_columns() {
    let source = nether_chunk_source(42);
    for &(chunk_x, chunk_z, local_x, y, local_z) in &[
        (70, -64, 10, 62, 7),
        (3, 64, 13, 31, 0),
    ] {
        let column = source.generator().column(chunk_x, chunk_z);
        let state = column.block_state(local_x, y, local_z);
        assert!(
            state.starts_with("minecraft:basalt"),
            "step-2 pillar missing at chunk ({chunk_x},{chunk_z}) local ({local_x},{y},{local_z}): {state}"
        );
    }
}
