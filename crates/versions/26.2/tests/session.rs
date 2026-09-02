//! Consolidated test binary for the **session** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "session/join_flow.rs"]
mod join_flow;
#[path = "session/login_compression.rs"]
mod login_compression;
#[path = "session/online_mode.rs"]
mod online_mode;
#[path = "session/start_configuration.rs"]
mod start_configuration;
#[path = "session/resource_pack_push.rs"]
mod resource_pack_push;
#[path = "session/registry_data.rs"]
mod registry_data;
#[path = "session/remaining_clientbound.rs"]
mod remaining_clientbound;
#[path = "session/clientbound_backlog.rs"]
mod clientbound_backlog;
#[path = "session/clientbound_ping.rs"]
mod clientbound_ping;
#[path = "session/death_respawn.rs"]
mod death_respawn;
#[path = "session/player_list.rs"]
mod player_list;
#[path = "session/player_view.rs"]
mod player_view;
#[path = "session/action_bar.rs"]
mod action_bar;
#[path = "session/titles.rs"]
mod titles;
#[path = "session/sound_particle_screen.rs"]
mod sound_particle_screen;
#[path = "session/maps_and_advancements.rs"]
mod maps_and_advancements;
