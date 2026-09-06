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

/// Clientbound `explosion`.
///
/// The affected-block list is counted with a fixed-width `i32`, so this packet
/// cannot use the derive's length annotations. The trailing player-motion
/// vector is always present on this wire, including when all three components
/// are zero.
#[derive(Debug, Clone, PartialEq)]
pub struct Explosion {
    /// Explosion centre.
    pub x: f32,
    /// Explosion centre.
    pub y: f32,
    /// Explosion centre.
    pub z: f32,
    /// Blast radius.
    pub radius: f32,
    /// Removed-block offsets relative to the floored explosion position.
    pub affected_block_offsets: Vec<[i8; 3]>,
    /// Local player's additive X velocity impulse.
    pub player_motion_x: f32,
    /// Local player's additive Y velocity impulse.
    pub player_motion_y: f32,
    /// Local player's additive Z velocity impulse.
    pub player_motion_z: f32,
}

/// Generous bound for an explosion's removed-block list.
const MAX_EXPLOSION_BLOCKS: i32 = 1_000_000;

impl lodestone_core::Decode for Explosion {
    fn decode(
        reader: &mut lodestone_core::Reader<'_>,
        _ctx: lodestone_core::Ctx,
    ) -> lodestone_core::Result<Self> {
        let x = reader.f32()?;
        let y = reader.f32()?;
        let z = reader.f32()?;
        let radius = reader.f32()?;
        let count = reader.i32()?;
        if count < 0 {
            return Err(lodestone_core::Error::NegativeLength(count));
        }
        if count > MAX_EXPLOSION_BLOCKS {
            return Err(lodestone_core::Error::LimitExceeded {
                limit: MAX_EXPLOSION_BLOCKS as usize,
                actual: count as usize,
            });
        }

        let mut affected_block_offsets = Vec::with_capacity(count as usize);
        for _ in 0..count {
            affected_block_offsets.push([reader.i8()?, reader.i8()?, reader.i8()?]);
        }

        Ok(Self {
            x,
            y,
            z,
            radius,
            affected_block_offsets,
            player_motion_x: reader.f32()?,
            player_motion_y: reader.f32()?,
            player_motion_z: reader.f32()?,
        })
    }
}

impl lodestone_core::Packet for Explosion {
    const NAME: &'static str = "minecraft:explosion";
    const STATE: lodestone_core::State = lodestone_core::State::Play;
    const BOUND: lodestone_core::Bound = lodestone_core::Bound::Client;
    const PROTOCOLS: lodestone_core::ProtocolRange = lodestone_core::ProtocolRange::new(47, 47);
}
