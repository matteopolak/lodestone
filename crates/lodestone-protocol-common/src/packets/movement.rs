//! Serverbound player-movement packets, byte-identical across every protocol
//! these three crates cover (47, 340, 754) -- these carry raw `f64`/`f32`
//! coordinates directly, unlike `BlockDig`/`SpawnPosition`, so there is no
//! embedded [`Position`](super::position::Position) to inherit a narrower
//! range from. Measured against v735's own definitions field for field; no
//! `#[mc(protocols = ...)]` is declared.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `position` packet.
///
/// Wire layout: f64 x/y/z, boolean on-ground.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:position", state = Play, bound = Server)]
pub struct ServerboundPosition {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate (feet position).
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// Whether the player is on the ground.
    pub on_ground: bool,
}

/// Serverbound `look` packet.
///
/// Wire layout: f32 yaw/pitch, boolean on-ground.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:look", state = Play, bound = Server)]
pub struct ServerboundLook {
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the player is on the ground.
    pub on_ground: bool,
}

/// Serverbound `position_look` packet.
///
/// Wire layout: f64 x/y/z, f32 yaw/pitch, boolean on-ground.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:position_look", state = Play, bound = Server)]
pub struct ServerboundPositionLook {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate (feet position).
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the player is on the ground.
    pub on_ground: bool,
}

/// Serverbound `flying` (player-on-ground) packet.
///
/// Wire layout: a single boolean on-ground flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:flying", state = Play, bound = Server)]
pub struct ServerboundFlying {
    /// Whether the player is on the ground.
    pub on_ground: bool,
}
