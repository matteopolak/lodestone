//! Consolidated test binary for the **live** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "live/live_camera_follows_server_spawn.rs"]
mod live_camera_follows_server_spawn;
#[path = "live/live_chat.rs"]
mod live_chat;
#[path = "live/live_container_render.rs"]
mod live_container_render;
#[path = "live/live_death_respawn.rs"]
mod live_death_respawn;
#[path = "live/live_dig_place.rs"]
mod live_dig_place;
#[path = "live/live_dropped_item.rs"]
mod live_dropped_item;
#[path = "live/live_edge_back_off_rubber_band.rs"]
mod live_edge_back_off_rubber_band;
#[path = "live/live_entity_light_time_of_day.rs"]
mod live_entity_light_time_of_day;
#[path = "live/live_entity_render.rs"]
mod live_entity_render;
#[path = "live/live_framed_item_wire.rs"]
mod live_framed_item_wire;
#[path = "live/live_particles.rs"]
mod live_particles;
#[path = "live/live_respawn_ground_trace.rs"]
mod live_respawn_ground_trace;
#[path = "live/live_section_read.rs"]
mod live_section_read;
#[path = "live/live_sign_text_pixels.rs"]
mod live_sign_text_pixels;
#[path = "live/live_sign_text_wire.rs"]
mod live_sign_text_wire;
#[path = "live/live_stands_on_server_ground.rs"]
mod live_stands_on_server_ground;
#[path = "live/live_tab_scoreboard_pixels.rs"]
mod live_tab_scoreboard_pixels;
#[path = "live/live_time_of_day.rs"]
mod live_time_of_day;
#[path = "live/live_use_item_release.rs"]
mod live_use_item_release;
#[path = "live/live_walk_on_server_ground.rs"]
mod live_walk_on_server_ground;
#[path = "live/live_world_mesh.rs"]
mod live_world_mesh;
