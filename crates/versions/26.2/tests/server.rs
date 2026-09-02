//! Consolidated test binary for the **server** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "server/server_block_placement.rs"]
mod server_block_placement;
#[path = "server/server_chat_broadcast.rs"]
mod server_chat_broadcast;
#[path = "server/server_command_block.rs"]
mod server_command_block;
#[path = "server/server_creeper_metadata_and_explode.rs"]
mod server_creeper_metadata_and_explode;
#[path = "server/server_death_screen.rs"]
mod server_death_screen;
#[path = "server/server_disconnect.rs"]
mod server_disconnect;
#[path = "server/server_fall_cancellation.rs"]
mod server_fall_cancellation;
#[path = "server/server_hand_use.rs"]
mod server_hand_use;
#[path = "server/server_hurt_and_death_animations.rs"]
mod server_hurt_and_death_animations;
#[path = "server/server_integration.rs"]
mod server_integration;
#[path = "server/server_inventory_live.rs"]
mod server_inventory_live;
#[path = "server/server_item_entity_metadata.rs"]
mod server_item_entity_metadata;
#[path = "server/server_join_experience.rs"]
mod server_join_experience;
#[path = "server/server_join_inventory.rs"]
mod server_join_inventory;
#[path = "server/server_light.rs"]
mod server_light;
#[path = "server/server_liveness.rs"]
mod server_liveness;
#[path = "server/server_no_demo_mobs.rs"]
mod server_no_demo_mobs;
#[path = "server/server_player_entity_stream.rs"]
mod server_player_entity_stream;
#[path = "server/server_player_rotation_stream.rs"]
mod server_player_rotation_stream;
#[path = "server/server_redstone_placement.rs"]
mod server_redstone_placement;
#[path = "server/server_status.rs"]
mod server_status;
#[path = "server/server_take_item_entity.rs"]
mod server_take_item_entity;
#[path = "server/block_entities_live.rs"]
mod block_entities_live;
#[path = "server/combat_live.rs"]
mod combat_live;
#[path = "server/drowning.rs"]
mod drowning;
#[path = "server/entity_streaming_live.rs"]
mod entity_streaming_live;
