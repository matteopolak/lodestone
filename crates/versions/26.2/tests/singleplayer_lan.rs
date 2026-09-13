//! Consolidated test binary for the **singleplayer_lan** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "singleplayer_lan/singleplayer_chat_sender_name.rs"]
mod singleplayer_chat_sender_name;
#[path = "singleplayer_lan/singleplayer_player_stream.rs"]
mod singleplayer_player_stream;
#[path = "singleplayer_lan/singleplayer_seam.rs"]
mod singleplayer_seam;
#[path = "singleplayer_lan/lan_player_stream.rs"]
mod lan_player_stream;
#[path = "singleplayer_lan/client_adapter_decorator_escape_hatch.rs"]
mod client_adapter_decorator_escape_hatch;
