//! One module per shared packet (or tightly-coupled packet family), so
//! concurrent edits to different packets never touch the same file.

pub mod chat;
pub mod client_settings;
pub mod entity;
pub mod keep_alive;
pub mod login;
pub mod movement;
pub mod player_info;
pub mod position;
pub mod slot;
pub mod window;
pub mod status;
