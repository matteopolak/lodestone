//! Consolidated test binary for the **live** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "live/live_block_light.rs"]
mod live_block_light;
#[path = "live/live_chunk.rs"]
mod live_chunk;
#[path = "live/live_chunk_flow.rs"]
mod live_chunk_flow;
#[path = "live/live_command_tree.rs"]
mod live_command_tree;
#[path = "live/live_creeper_explosion.rs"]
mod live_creeper_explosion;
#[path = "live/live_destroy_block_event.rs"]
mod live_destroy_block_event;
#[path = "live/live_item_components.rs"]
mod live_item_components;
#[path = "live/live_item_entity_metadata.rs"]
mod live_item_entity_metadata;
#[path = "live/live_mob_sim.rs"]
mod live_mob_sim;
#[path = "live/live_physics.rs"]
mod live_physics;
#[path = "live/live_plugin_server_join.rs"]
mod live_plugin_server_join;
#[path = "live/live_registry_data.rs"]
mod live_registry_data;
#[path = "live/live_registry_data_full_set.rs"]
mod live_registry_data_full_set;
#[path = "live/live_respawn.rs"]
mod live_respawn;
#[path = "live/live_terrain_light.rs"]
mod live_terrain_light;
#[path = "live/live_tool_component.rs"]
mod live_tool_component;
#[path = "live/live_view_distance.rs"]
mod live_view_distance;
#[path = "live/live_world_state.rs"]
mod live_world_state;
