//! Independent wire fixtures for resource-pack responses.
//!
//! The expected payload is assembled from the packet's documented wire shape:
//! a raw UUID followed by a VarInt response ordinal. This keeps the decoder
//! check independent of the client adapter's encoder.

use lodestone_core::State;
use lodestone_model::ResourcePackResponseKind;
use lodestone_server::{ServerBound, ServerProtocol};
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::V770ServerProtocol;

const PACK_ID: [u8; 16] = [
    0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc,
    0xde, 0xf0,
];

#[test]
fn resource_pack_response_decodes_id_and_action() {
    let mut payload = PACK_ID.to_vec();
    payload.push(3);

    let decoded = V770ServerProtocol.decode(
        State::Play,
        play::serverbound::RESOURCE_PACK,
        &payload,
    );

    assert!(
        matches!(
            decoded,
            ServerBound::ResourcePackResponse {
                id,
                response: ResourcePackResponseKind::Accepted,
            } if id.as_bytes() == &PACK_ID
        ),
        "expected an accepted response, got {decoded:?}"
    );
}

#[test]
fn resource_pack_response_rejects_unknown_actions_and_trailing_bytes() {
    let mut unknown = PACK_ID.to_vec();
    unknown.push(8);
    assert!(matches!(
        V770ServerProtocol.decode(State::Play, play::serverbound::RESOURCE_PACK, &unknown),
        ServerBound::Ignored
    ));

    let mut trailing = PACK_ID.to_vec();
    trailing.extend_from_slice(&[0, 0xff]);
    assert!(matches!(
        V770ServerProtocol.decode(State::Play, play::serverbound::RESOURCE_PACK, &trailing),
        ServerBound::Ignored
    ));
}
