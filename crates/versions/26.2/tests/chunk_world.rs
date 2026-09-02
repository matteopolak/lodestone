//! Consolidated test binary for the **chunk_world** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "chunk_world/chunk_batch.rs"]
mod chunk_batch;
#[path = "chunk_world/chunk_batch_ack.rs"]
mod chunk_batch_ack;
#[path = "chunk_world/chunk_decode.rs"]
mod chunk_decode;
#[path = "chunk_world/chunk_encode_cycles.rs"]
mod chunk_encode_cycles;
#[path = "chunk_world/chunk_events.rs"]
mod chunk_events;
#[path = "chunk_world/chunks_biomes.rs"]
mod chunks_biomes;
#[path = "chunk_world/light_update.rs"]
mod light_update;
#[path = "chunk_world/block_hardness_seam.rs"]
mod block_hardness_seam;
#[path = "chunk_world/bubble_column_seam.rs"]
mod bubble_column_seam;
#[path = "chunk_world/block_edit.rs"]
mod block_edit;
#[path = "chunk_world/block_updates.rs"]
mod block_updates;
#[path = "chunk_world/world_border.rs"]
mod world_border;
#[path = "chunk_world/world_events.rs"]
mod world_events;
#[path = "chunk_world/world_state.rs"]
mod world_state;
#[path = "chunk_world/prototype_shape_seams.rs"]
mod prototype_shape_seams;
#[path = "chunk_world/undecodable_packet_resync.rs"]
mod undecodable_packet_resync;
