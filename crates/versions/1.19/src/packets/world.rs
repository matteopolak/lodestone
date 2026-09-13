//! World-level packet definitions for protocol 762.

use lodestone_macros::{Decode, Encode, Packet};

use super::position::Position;

/// Clientbound `block_action` / block event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_action", state = Play, bound = Client)]
pub struct BlockAction {
    /// Packed block position.
    pub location: Position,
    /// First opaque event parameter.
    pub byte1: u8,
    /// Second opaque event parameter.
    pub byte2: u8,
    /// Protocol-762 `minecraft:block` registry id.
    #[mc(varint)]
    pub block_id: i32,
}
