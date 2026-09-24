//! Production controls for the Overworld mixed FEATURES replay.

use lodestone_server::overworld_chunk_source;

#[test]
fn production_features_replay_retains_a_neighbour_dungeon_spill() {
    let source = overworld_chunk_source(42);
    let column = source.generator().column(-8, -8);

    assert_eq!(
        column.block_state_id(15, -33, 3).block(),
        lodestone_data::block::Block::Chest,
        "the target must retain the dungeon chest spilled from its east source",
    );
}

#[test]
fn uncached_features_replay_is_byte_identical_to_production() {
    let source = overworld_chunk_source(42);
    let production = source.generator().column(-8, -8);
    let (uncached, _) = source.generator().column_timed(-8, -8);

    assert_eq!(
        production.into_raw(),
        uncached.into_raw(),
        "the cache-cold mixed replay must dispatch the same nine source bodies",
    );
}
