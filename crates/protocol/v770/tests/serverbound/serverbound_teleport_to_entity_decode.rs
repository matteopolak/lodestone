//! Hermetic byte-exact test for `V770ServerProtocol::decode`'s
//! `TELEPORT_TO_ENTITY` arm — the decode-side twin of
//! `serverbound_ping_spectator.rs`'s `teleport_to_entity_is_a_raw_sixteen_byte_uuid`,
//! which only proves the encode side.
//!
//! Layout verified independently against 26.2's own serverbound
//! teleport-to-entity packet (confirmed against the decompiled 26.2 source): a single
//! raw 16-byte UUID, no VarInt length prefix and no wrapping optional — the
//! same layout `serverbound_ping_spectator.rs` already hand-verified for the
//! encoder, reused here from the wire side rather than copied from this
//! crate's own `decode_full::<TeleportToEntity>` helper.

use lodestone_server::{ServerBound, ServerProtocol};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::play;
use uuid::Uuid;

#[test]
fn teleport_to_entity_decodes_the_raw_sixteen_byte_uuid() {
    let proto = V770ServerProtocol;
    // Pairwise-distinct nibbles throughout, so a byte-order or field
    // transposition inside the 16-byte body cannot survive unnoticed.
    let uuid = Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff);
    let payload = uuid.as_u128().to_be_bytes();

    let decoded = proto.decode(
        lodestone_core::State::Play,
        play::serverbound::TELEPORT_TO_ENTITY,
        &payload,
    );

    assert!(
        matches!(decoded, ServerBound::TeleportToEntity { uuid: u } if u == uuid),
        "expected TeleportToEntity{{ uuid: {uuid} }}, got {decoded:?}"
    );
}

/// The control: a truncated frame (15 of the 16 bytes) must not decode to a
/// well-formed variant carrying a zero-padded or truncated uuid — proving
/// the arm actually validates frame length rather than reading whatever
/// bytes happen to be present.
#[test]
fn teleport_to_entity_rejects_a_short_frame() {
    let proto = V770ServerProtocol;
    let uuid = Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff);
    let short_payload = &uuid.as_u128().to_be_bytes()[..15];

    let decoded = proto.decode(
        lodestone_core::State::Play,
        play::serverbound::TELEPORT_TO_ENTITY,
        short_payload,
    );

    assert!(
        matches!(decoded, ServerBound::Ignored),
        "a truncated frame must decode to Ignored, got {decoded:?}"
    );
}
