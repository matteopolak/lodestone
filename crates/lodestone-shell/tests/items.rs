//! Consolidated test binary for the **items** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "items/dropped_item_pixels.rs"]
mod dropped_item_pixels;
#[path = "items/firework_rocket_pixels.rs"]
mod firework_rocket_pixels;
#[path = "items/framed_map_pixels.rs"]
mod framed_map_pixels;
#[path = "items/item_frame_pixels.rs"]
mod item_frame_pixels;
#[path = "items/sheet_particle_atlas_pixels.rs"]
mod sheet_particle_atlas_pixels;
#[path = "items/special_item_world_pixels.rs"]
mod special_item_world_pixels;
#[path = "items/vault_item_cluster_pixels.rs"]
mod vault_item_cluster_pixels;
