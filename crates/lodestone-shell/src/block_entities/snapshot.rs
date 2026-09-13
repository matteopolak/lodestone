//! Shared camera-scoped input for state-driven block-entity renderers.
//!
//! The snapshot owns the one world read that can feed several render sources in
//! the same frame. It intentionally contains only validated block-state ids
//! and packed light; NBT-dependent renderers retain their typed candidate
//! gathers in [`super::scanner`].

use glam::Vec3;
use lodestone_data::block_states::StateId;
use lodestone_render::SkyDefault;
use lodestone_world::ChunkPos;

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
    let chunks = client.loaded_chunks();
    let store = client.chunk_world();
    let world = store.read();
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for model_pos in chunks {
        let pos = ChunkPos { x: model_pos.x, z: model_pos.z };
        let Some(chunk) = world.get(pos) else { continue };
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
    Some(BlockEntityFrameSnapshot { candidates })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_entities::{bell_spawns_from_snapshot, chest_spawns_from_snapshot, BellShakes, ChestLids};

    fn first_state(name: &str) -> StateId {
        (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
            .and_then(StateId::new)
            .unwrap_or_else(|| panic!("missing state for {name}"))
    }

    #[test]
    fn one_snapshot_feeds_multiple_renderers_without_a_handle() {
        let snapshot = BlockEntityFrameSnapshot {
            candidates: vec![
                BlockEntityFrameCandidate { pos: [1, 64, 2], state_id: first_state("minecraft:chest"), light: 0xab },
                BlockEntityFrameCandidate { pos: [3, 64, 4], state_id: first_state("minecraft:bell"), light: 0xcd },
            ],
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
