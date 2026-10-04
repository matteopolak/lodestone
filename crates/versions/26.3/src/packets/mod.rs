//! Protocol 777 body codecs, independent of packet identifier translation.

use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};

pub use lodestone_v26_2::packets::release_layout::{
    AddTransientBlock, PostEffects, SwingAnimation, SwingKind, WireHand, WireStateId,
    AcceptTeleportation, Punch, OpenSignEditor, SignTextSlot, SignUpdate,
    DeltaPath, DeltaStep, EntityPositionSync, MoveEntityPos, MoveEntityPosRot, MoveEntityRot,
    PositionPath, PositionStep, ParticleDistribution, ParticleSpawn,
};

const CTX: Ctx = Ctx { version: crate::PROTOCOL };

/// Decode a complete packet body and reject trailing bytes.
pub fn decode_body<T: Decode>(payload: &[u8]) -> Result<T> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX)?;
    reader.ensure_empty()?;
    Ok(value)
}

/// Encode a protocol 777 packet body without its identifier or frame length.
pub fn encode_body<T: Encode>(value: &T) -> Result<Vec<u8>> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX)?;
    Ok(writer.into_vec())
}
