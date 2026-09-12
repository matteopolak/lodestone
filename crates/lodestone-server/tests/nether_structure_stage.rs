//! External control for Nether structure sidecar stage ownership.
//!
//! A shaped admission is a partial column. Its terrain may be sent as a
//! dependency, but structure placement and generated block entities belong to
//! the FULL completion only.

use lodestone_server::{nether_chunk_source, ChunkGenerationStage, ChunkSource};
use lodestone_model::BlockPos;

const SEED: i64 = 42;
const CHEST: BlockPos = BlockPos::new(9, 53, 172);
const SECOND_CHEST: BlockPos = BlockPos::new(7, 53, 189);

#[test]
fn shaped_nether_admission_does_not_attach_fortress_chest_sidecar() {
    let source = nether_chunk_source(SEED);
    let shaped = source.column_at(0, 10, ChunkGenerationStage::Shaped);
    assert!(
        shaped.block_entities().iter().all(|(position, _)| *position != CHEST),
        "partial Nether admission must not attach a FULL fortress chest sidecar"
    );

    let full = source.column_at(0, 10, ChunkGenerationStage::Full);
    assert_eq!(
        full.block_state(CHEST.x, CHEST.y, CHEST.z.rem_euclid(16)),
        "minecraft:chest[facing=south,type=single,waterlogged=false]"
    );
    assert!(
        full.block_entities().iter().any(|(position, entity)| {
            *position == CHEST && entity.kind() == lodestone_server::BlockEntityKind::Chest
        }),
        "FULL Nether completion must attach the fortress chest sidecar"
    );

    let second = source.column_at(0, 11, ChunkGenerationStage::Full);
    assert_eq!(
        second
            .block_state(
                SECOND_CHEST.x,
                SECOND_CHEST.y,
                SECOND_CHEST.z.rem_euclid(16)
            )
            .split('[')
            .next(),
        Some("minecraft:chest"),
        "the second receiving chunk must retain the fortress chest block"
    );
    let second_entity = second
        .block_entities()
        .iter()
        .find(|(position, _)| *position == SECOND_CHEST)
        .map(|(_, entity)| entity);
    assert_eq!(
        second_entity.map(lodestone_server::BlockEntity::kind),
        Some(lodestone_server::BlockEntityKind::Chest),
        "a generated fortress chest must carry its rolled container sidecar, not an empty inferred record"
    );
}
