//! World/block-level clientbound packets for protocol 47.

use lodestone_macros::{Decode, Encode, Packet};

use super::position::Position;

/// Clientbound `block_action` — a block-triggering "block event" (note block
/// play, piston start, chest lid animation).
///
/// Wire layout: packed [`Position`], two opaque bytes, then a varint legacy
/// block **type** id (no metadata component). Verified against
/// minecraft-data's 1.8 `packet_block_action` (identical shape at 1.12.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_action", state = Play, bound = Client)]
pub struct BlockAction {
    /// Block position.
    pub location: Position,
    /// First opaque event parameter.
    pub byte1: u8,
    /// Second opaque event parameter.
    pub byte2: u8,
    /// Legacy numeric block-type id (no metadata).
    #[mc(varint)]
    pub block_id: i32,
}

/// Clientbound `block_break_animation` — a block's break-progress overlay.
///
/// Wire layout: varint breaker entity id, packed [`Position`], signed byte
/// destroy stage. Verified against minecraft-data's 1.8
/// `packet_block_break_animation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_break_animation", state = Play, bound = Client)]
pub struct BlockBreakAnimation {
    /// Id of the entity breaking the block.
    #[mc(varint)]
    pub entity_id: i32,
    /// Block position.
    pub location: Position,
    /// Raw break-stage byte.
    pub destroy_stage: i8,
}

/// Clientbound `world_event` — a gameplay-level event code at a position
/// (door sound, wither spawn, block-fizz), distinct from `block_action`.
///
/// Wire layout: signed `i32` event code, packed [`Position`], signed `i32`
/// event data, bool global. Verified against minecraft-data's 1.8
/// `packet_world_event` (identical shape at 1.12.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:world_event", state = Play, bound = Client)]
pub struct WorldEvent {
    /// Gameplay event code.
    pub effect_id: i32,
    /// Event position.
    pub location: Position,
    /// Event-specific data.
    pub data: i32,
    /// Whether the event is global rather than distance-limited.
    pub global: bool,
}

/// Clientbound `named_sound_effect` — a positioned sound by name.
///
/// Wire layout: string sound name, three fixed-point `i32` coordinates
/// (real coordinate × 8), f32 volume, unsigned byte pitch. Verified against
/// minecraft-data's 1.8 `packet_named_sound_effect`. **1.8 carries no sound
/// category and packs pitch as a single byte** (`byte / 63.0` — the
/// long-stable external wire convention for this exact packet, unchanged
/// since its introduction); 1.9+ inserts a varint category and widens pitch
/// to a raw `f32`, so this shape must not be ported from a later family.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:named_sound_effect", state = Play, bound = Client)]
pub struct NamedSoundEffect {
    /// Sound event name (legacy dotted form, e.g. `random.pop`).
    #[mc(max = 256)]
    pub sound_name: String,
    /// Fixed-point X (real coordinate × 8).
    pub x: i32,
    /// Fixed-point Y (real coordinate × 8).
    pub y: i32,
    /// Fixed-point Z (real coordinate × 8).
    pub z: i32,
    /// Volume multiplier.
    pub volume: f32,
    /// Packed pitch byte (`byte / 63.0` is the real pitch multiplier).
    pub pitch: u8,
}

/// Clientbound `open_sign_entity` — the server opened a sign-editing UI.
///
/// Wire layout: a single packed [`Position`]. Verified against
/// minecraft-data's 1.8 `packet_open_sign_entity` (identical shape at
/// 1.12.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:open_sign_entity", state = Play, bound = Client)]
pub struct OpenSignEntity {
    /// Block position of the sign.
    pub location: Position,
}
