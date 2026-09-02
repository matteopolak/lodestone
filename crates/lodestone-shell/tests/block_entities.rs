//! Consolidated test binary for the **block_entities** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "block_entities/bell_block_entity_pixels.rs"]
mod bell_block_entity_pixels;
#[path = "block_entities/brushable_block_pixels.rs"]
mod brushable_block_pixels;
#[path = "block_entities/chest_block_entity_pixels.rs"]
mod chest_block_entity_pixels;
#[path = "block_entities/conduit_block_entity_pixels.rs"]
mod conduit_block_entity_pixels;
#[path = "block_entities/decorated_pot_block_entity_pixels.rs"]
mod decorated_pot_block_entity_pixels;
#[path = "block_entities/placed_chest_block_entity_pixels.rs"]
mod placed_chest_block_entity_pixels;
#[path = "block_entities/shelf_pixels.rs"]
mod shelf_pixels;
#[path = "block_entities/skull_block_entity_pixels.rs"]
mod skull_block_entity_pixels;
