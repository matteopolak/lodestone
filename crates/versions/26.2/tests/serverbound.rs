//! Consolidated test binary for the **serverbound** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "serverbound/serverbound_actions.rs"]
mod serverbound_actions;
#[path = "serverbound/serverbound_accept_teleportation.rs"]
mod serverbound_accept_teleportation;
#[path = "serverbound/serverbound_block_entity_query.rs"]
mod serverbound_block_entity_query;
#[path = "serverbound/serverbound_backlog.rs"]
mod serverbound_backlog;
#[path = "serverbound/serverbound_change_game_mode.rs"]
mod serverbound_change_game_mode;
#[path = "serverbound/serverbound_interaction_tier2.rs"]
mod serverbound_interaction_tier2;
#[path = "serverbound/serverbound_ping_spectator.rs"]
mod serverbound_ping_spectator;
#[path = "serverbound/serverbound_player_loaded_decode.rs"]
mod serverbound_player_loaded_decode;
#[path = "serverbound/serverbound_client_tick_end_decode.rs"]
mod serverbound_client_tick_end_decode;
#[path = "serverbound/serverbound_protocol_hygiene.rs"]
mod serverbound_protocol_hygiene;
#[path = "serverbound/serverbound_recipe_bundle.rs"]
mod serverbound_recipe_bundle;
#[path = "serverbound/serverbound_recipe_seen_decode.rs"]
mod serverbound_recipe_seen_decode;
#[path = "serverbound/serverbound_recipe_settings_decode.rs"]
mod serverbound_recipe_settings_decode;
#[path = "serverbound/serverbound_resource_pack_decode.rs"]
mod serverbound_resource_pack_decode;
#[path = "serverbound/serverbound_spectator_action_decode.rs"]
mod serverbound_spectator_action_decode;
#[path = "serverbound/serverbound_swing_decode.rs"]
mod serverbound_swing_decode;
#[path = "serverbound/serverbound_teleport_to_entity_decode.rs"]
mod serverbound_teleport_to_entity_decode;
#[path = "serverbound/serverbound_wiring.rs"]
mod serverbound_wiring;
#[path = "serverbound/interaction_actions.rs"]
mod interaction_actions;
