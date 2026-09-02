//! Consolidated test binary for the **command** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "command/command_tree.rs"]
mod command_tree;
#[path = "command/command_tree_encode.rs"]
mod command_tree_encode;
#[path = "command/command_wire_path.rs"]
mod command_wire_path;
#[path = "command/builtin_command_parity.rs"]
mod builtin_command_parity;
#[path = "command/builtin_commands_wire_path.rs"]
mod builtin_commands_wire_path;
#[path = "command/chat_dispatch.rs"]
mod chat_dispatch;
#[path = "command/plugin_channels_round_trip.rs"]
mod plugin_channels_round_trip;
#[path = "command/gamemaster_packet_permission_gate.rs"]
mod gamemaster_packet_permission_gate;
#[path = "command/operator_encoders.rs"]
mod operator_encoders;
#[path = "command/recipe_book_add.rs"]
mod recipe_book_add;
#[path = "command/beacon_wiring.rs"]
mod beacon_wiring;
#[path = "command/book_content_wiring.rs"]
mod book_content_wiring;
