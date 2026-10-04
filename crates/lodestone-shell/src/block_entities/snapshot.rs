//! Shared camera-scoped input for state-driven block-entity renderers.
//!
//! The snapshot owns the one world read that can feed several render sources in
//! the same frame. It intentionally contains only validated block-state ids
//! and packed light; NBT-dependent renderers retain their typed candidate
//! gathers in [`super::scanner`].

use glam::Vec3;
use lodestone_data::block_states::StateId;
use lodestone_render::SkyDefault;
use lodestone_world::World;

use crate::net::SharedHandle;

use super::VIEW_DISTANCE;

/// One state-driven block entity captured for a single rendered frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BlockEntityFrameCandidate {
    pub(super) pos: [i32; 3],
    pub(super) state_id: StateId,
    pub(super) light: u8,
}

/// Camera-scoped immutable input shared by state-driven block-entity renderers.
#[derive(Debug, Default)]
pub(crate) struct BlockEntityFrameSnapshot {
    pub(super) candidates: Vec<BlockEntityFrameCandidate>,
    pub(crate) scan_counts: BlockEntityScanCounts,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockEntityScanCounts {
    pub(crate) loaded_chunks: usize,
    pub(crate) candidate_chunks: usize,
    pub(crate) records_visited: usize,
}

fn packed_light_in_chunk(
    chunk: &lodestone_world::LoadedChunk,
    block: [i32; 3],
    dimensions: Option<lodestone_client::WorldDimensions>,
    sky_default: SkyDefault,
) -> u8 {
    let Some(dimensions) = dimensions else {
        return lodestone_render::ENTITY_FULLBRIGHT;
    };
    let section = (block[1] - dimensions.min_y).div_euclid(16);
    if section < 0 || section >= dimensions.section_count() as i32 {
        return lodestone_render::ENTITY_FULLBRIGHT;
    }
    let section = section as usize;
    let x = block[0].rem_euclid(16) as usize;
    let y = (block[1] - dimensions.min_y).rem_euclid(16) as usize;
    let z = block[2].rem_euclid(16) as usize;
    let sky = chunk
        .light
        .section_sky_light(section, x, y, z)
        .unwrap_or(match sky_default {
            SkyDefault::Full => 15,
            SkyDefault::None => 0,
        });
    let block = chunk.light.section_block_light(section, x, y, z).unwrap_or(0);
    (sky << 4) | block
}

/// Capture validated state and light once for this camera position.
#[must_use]
pub(crate) fn block_entity_frame_snapshot(
    handle: &SharedHandle,
    eye: Vec3,
) -> Option<BlockEntityFrameSnapshot> {
    let client = handle.get()?;
    let dimensions = client.world_dimensions();
    let player = client.player();
    let sky_default = crate::mesher::sky_default_for_dimension(
        player.dimension.as_ref(),
        player.dimension_type.as_ref(),
    );
    let store = client.chunk_world();
    let world = store.read();
    Some(block_entity_frame_snapshot_from_world(&world, eye, dimensions, sky_default))
}

fn block_entity_frame_snapshot_from_world(
    world: &World,
    eye: Vec3,
    dimensions: Option<lodestone_client::WorldDimensions>,
    sky_default: SkyDefault,
) -> BlockEntityFrameSnapshot {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    let mut scan_counts = BlockEntityScanCounts {
        loaded_chunks: world.len(),
        ..Default::default()
    };
    for (pos, chunk) in world.block_entity_chunks() {
        scan_counts.candidate_chunks += 1;
        scan_counts.records_visited += chunk.block_entities.len();
        for entity in &chunk.block_entities {
            let block = [
                pos.x * 16 + i32::from(entity.rel_x),
                i32::from(entity.y),
                pos.z * 16 + i32::from(entity.rel_z),
            ];
            let centre = Vec3::new(
                block[0] as f32 + 0.5,
                block[1] as f32 + 0.5,
                block[2] as f32 + 0.5,
            );
            if centre.distance_squared(eye) > cutoff { continue }
            let raw_state_id = chunk.column.get_block(
                usize::from(entity.rel_x),
                block[1],
                usize::from(entity.rel_z),
            );
            let Some(state_id) = StateId::new(raw_state_id) else { continue };
            candidates.push(BlockEntityFrameCandidate {
                pos: block,
                state_id,
                light: packed_light_in_chunk(chunk, block, dimensions, sky_default),
            });
        }
    }
    BlockEntityFrameSnapshot { candidates, scan_counts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::Nbt;
    use lodestone_world::{
        BlockEntity, ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LightData,
        LoadedChunk, PaletteKind,
    };
    use crate::block_entities::{bell_spawns_from_snapshot, chest_spawns_from_snapshot, BellShakes, ChestLids};

    fn first_state(name: &str) -> StateId {
        (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
            .and_then(StateId::new)
            .unwrap_or_else(|| panic!("missing state for {name}"))
    }

    fn fixture_chunk(state: StateId, with_record: bool) -> LoadedChunk {
        let mut column = ChunkColumn::new(
            0, 1, PaletteKind::block_states(), PaletteKind::biomes(), 0, 0,
        );
        column.set_block(3, 7, 5, state.raw());
        let mut light = ColumnLight::new(1);
        *light.sky_mut(1) = LightData::Uniform(9);
        *light.block_mut(1) = LightData::Uniform(4);
        let records = if with_record {
            vec![BlockEntity { rel_x: 3, rel_z: 5, y: 7, type_id: 0, nbt: Nbt::End }]
        } else {
            Vec::new()
        };
        LoadedChunk::new(column, light, Heightmaps::new(), records)
    }

    #[test]
    fn sparse_snapshot_counts_work_and_keeps_state_light_and_distance_live() {
        let chest = first_state("minecraft:chest");
        let bell = first_state("minecraft:bell");
        let mut world = World::new();
        for x in 100..164 {
            world.load(ChunkPos::new(x, 0), fixture_chunk(chest, false));
        }
        let dimensions = Some(lodestone_client::WorldDimensions { min_y: 0, height: 16 });
        let eye = Vec3::new(3.5, 7.5, 5.5);
        let empty = block_entity_frame_snapshot_from_world(&world, eye, dimensions, SkyDefault::Full);
        assert!(empty.candidates.is_empty());
        assert_eq!(empty.scan_counts, BlockEntityScanCounts {
            loaded_chunks: 64, candidate_chunks: 0, records_visited: 0,
        });

        for x in [0, 2, 8] {
            world.load(ChunkPos::new(x, 0), fixture_chunk(chest, true));
        }
        let frame = block_entity_frame_snapshot_from_world(&world, eye, dimensions, SkyDefault::Full);
        assert_eq!(frame.scan_counts, BlockEntityScanCounts {
            loaded_chunks: 67, candidate_chunks: 3, records_visited: 3,
        });
        let mut positions = frame.candidates.iter().map(|candidate| candidate.pos).collect::<Vec<_>>();
        positions.sort_unstable();
        assert_eq!(positions, [[3, 7, 5], [35, 7, 5]]);
        assert!(frame.candidates.iter().all(|candidate| candidate.light == 0x94));

        world.set_block(3, 7, 5, bell.raw());
        *world.get_mut(ChunkPos::new(0, 0)).unwrap().light.block_mut(1) = LightData::Uniform(11);
        let changed = block_entity_frame_snapshot_from_world(&world, eye, dimensions, SkyDefault::Full);
        let candidate = changed.candidates.iter().find(|candidate| candidate.pos == [3, 7, 5]).unwrap();
        assert_eq!(candidate.state_id, bell);
        assert_eq!(candidate.light, 0x9b);
        assert_eq!(frame.candidates.iter().find(|candidate| candidate.pos == [3, 7, 5]).unwrap().state_id, chest);
        world.get_mut(ChunkPos::new(0, 0)).unwrap().block_entities.clear();
        let removed = block_entity_frame_snapshot_from_world(&world, eye, dimensions, SkyDefault::Full);
        assert_eq!(removed.scan_counts.candidate_chunks, 2);
        assert!(removed.candidates.iter().all(|candidate| candidate.pos != [3, 7, 5]));
    }

    #[test]
    fn one_snapshot_feeds_multiple_renderers_without_a_handle() {
        let snapshot = BlockEntityFrameSnapshot {
            candidates: vec![
                BlockEntityFrameCandidate { pos: [1, 64, 2], state_id: first_state("minecraft:chest"), light: 0xab },
                BlockEntityFrameCandidate { pos: [3, 64, 4], state_id: first_state("minecraft:bell"), light: 0xcd },
            ],
            ..Default::default()
        };
        let chests = chest_spawns_from_snapshot(&snapshot, &ChestLids::new(), 0.0);
        let bells = bell_spawns_from_snapshot(&snapshot, &BellShakes::new(), 0.0);
        assert_eq!(chests.len(), 1);
        assert_eq!(chests[0].pos, [1, 64, 2]);
        assert_eq!(chests[0].light, 0xab);
        assert_eq!(bells.len(), 1);
        assert_eq!(bells[0].pos, [3, 64, 4]);
        assert_eq!(bells[0].light, 0xcd);
    }
}
