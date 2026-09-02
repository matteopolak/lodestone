//! Hermetic byte-exact tests for the remaining play-state ping/spectator
//! backlog encoders: `ping_request`, `spectator_action`, and
//! `teleport_to_entity`.
//!
//! Expected payloads are built independently of the adapter's own codec, so
//! a symmetric bug cannot pass. Layouts are verified against 26.2's
//! `net.minecraft.network.protocol.ping.ServerboundPingRequestPacket` (a
//! single big-endian 64-bit long, shared with the status state but also
//! sent during play by vanilla's `PingDebugMonitor` for the F3 network
//! graph — independent of the server-initiated `ping`/`pong` challenge),
//! `ServerboundSpectatorActionPacket` (a single VarInt using
//! `ByteBufCodecs.OPTIONAL_VAR_INT`'s **offset** encoding — `0` for "none",
//! `id + 1` when present — which is *not* the common bool-prefixed optional
//! shape used elsewhere in this protocol), and
//! `ServerboundTeleportToEntityPacket` (a single raw 16-byte UUID).
//!
//! None of these three actions currently have a live call site elsewhere in
//! the workspace (no debug-overlay ping timer, no spectator UI). These
//! tests exist so a future caller has byte-exact protocol coverage before
//! it's wired to anything.

use lodestone_model::{ClientAction, ConnectionState, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use uuid::Uuid;

fn varint(v: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut u = v as u32;
    loop {
        let byte = (u & 0x7F) as u8;
        u >>= 7;
        if u != 0 {
            out.push(byte | 0x80);
        } else {
            out.push(byte);
            break;
        }
    }
    out
}

#[test]
fn ping_request_is_a_big_endian_i64() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::PingRequest {
                time: 0x0102_0304_0506_0708,
            },
        )
        .expect("encode ping request");
    assert_eq!(
        encoded,
        Some((
            play::serverbound::PING_REQUEST,
            0x0102_0304_0506_0708_i64.to_be_bytes().to_vec()
        ))
    );
}

#[test]
fn ping_request_is_not_encoded_outside_play() {
    let adapter = V770Adapter::new();
    assert_eq!(
        adapter
            .encode_action(
                ConnectionState::Status,
                &ClientAction::PingRequest { time: 1 }
            )
            .expect("encode outside play"),
        None
    );
}

#[test]
fn spectator_action_none_is_a_single_zero_byte() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SpectatorAction {
                target_entity_id: None,
            },
        )
        .expect("encode spectator action");
    assert_eq!(
        encoded,
        Some((play::serverbound::SPECTATOR_ACTION, vec![0]))
    );
}

#[test]
fn spectator_action_present_id_is_offset_by_one_not_bool_prefixed() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SpectatorAction {
                target_entity_id: Some(41),
            },
        )
        .expect("encode spectator action");
    // 41 + 1 = 42, a single-byte VarInt: not `[1, 41]` (bool-prefixed) and
    // not `[41]` (raw id) — the offset encoding is the whole point of this
    // test.
    assert_eq!(
        encoded,
        Some((play::serverbound::SPECTATOR_ACTION, varint(42)))
    );
}

#[test]
fn spectator_action_id_zero_is_offset_to_one_not_confused_with_none() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SpectatorAction {
                target_entity_id: Some(0),
            },
        )
        .expect("encode spectator action");
    assert_eq!(
        encoded,
        Some((play::serverbound::SPECTATOR_ACTION, varint(1)))
    );
}

#[test]
fn teleport_to_entity_is_a_raw_sixteen_byte_uuid() {
    let adapter = V770Adapter::new();
    let uuid = Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff);
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::TeleportToEntity { target: uuid },
        )
        .expect("encode teleport to entity");
    assert_eq!(
        encoded,
        Some((
            play::serverbound::TELEPORT_TO_ENTITY,
            uuid.as_u128().to_be_bytes().to_vec()
        ))
    );
}

#[test]
fn teleport_to_entity_is_not_encoded_outside_play() {
    let adapter = V770Adapter::new();
    assert_eq!(
        adapter
            .encode_action(
                ConnectionState::Configuration,
                &ClientAction::TeleportToEntity {
                    target: Uuid::nil(),
                }
            )
            .expect("encode outside play"),
        None
    );
}
