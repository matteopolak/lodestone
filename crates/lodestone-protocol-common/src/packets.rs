//! One module per shared packet (or tightly-coupled packet family), so
//! concurrent edits to different packets never touch the same file.

pub mod login;
pub mod player_info;
pub mod position;
pub mod status;
