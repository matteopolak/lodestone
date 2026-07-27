//! Hermetic byte-exact tests for the "protocol hygiene" serverbound encoders:
//! `client_information`, `custom_payload` (`minecraft:brand`), `pong`,
//! `resource_pack` response, and `client_tick_end`.
//!
//! Expected payloads are built from the wire specification with an
//! independent VarInt encoder (never the adapter's own codec), so a symmetric
//! bug cannot pass. Layouts are verified against 26.2's
//! `ServerboundClientInformationPacket`, `ServerboundCustomPayloadPacket` /
//! `BrandPayload`, `ServerboundPongPacket`, `ServerboundResourcePackPacket`,
//! and `ServerboundClientTickEndPacket`. These four actions are valid in both
//! the configuration and play states, except `client_tick_end`, which is
//! play-only.

use lodestone_model::{
    ChatMode, ClientAction, ClientSettings, ConnectionState, DisplayedSkinParts, MainHand,
    ParticleStatus, ResourcePackResponseKind, VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::{configuration, play};
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

fn settings() -> ClientSettings {
    ClientSettings {
        locale: "en_us".to_owned(),
        view_distance: 10,
        chat_mode: ChatMode::Full,
        chat_colors: true,
        skin_parts: DisplayedSkinParts {
            cape: true,
            jacket: false,
            left_sleeve: true,
            right_sleeve: false,
            left_pants_leg: true,
            right_pants_leg: false,
            hat: true,
        },
        main_hand: MainHand::Right,
        text_filtering: false,
        allow_server_listing: true,
        particle_status: ParticleStatus::Decreased,
    }
}

fn settings_golden() -> Vec<u8> {
    let mut want = Vec::new();
    want.extend_from_slice(&varint(5)); // "en_us" length
    want.extend_from_slice(b"en_us");
    want.push(10); // view distance (signed byte)
    want.extend_from_slice(&varint(0)); // chat visibility: FULL
    want.push(1); // chat colors: true
    // skin parts bitmask: cape(0x01) | left_sleeve(0x04) | left_pants_leg(0x10) | hat(0x40)
    want.push(0x01 | 0x04 | 0x10 | 0x40);
    want.extend_from_slice(&varint(1)); // main hand: RIGHT
    want.push(0); // text filtering: false
    want.push(1); // allows listing: true
    want.extend_from_slice(&varint(1)); // particle status: DECREASED
    want
}

#[test]
fn client_information_is_byte_exact_in_play() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SetClientSettings(settings()),
        )
        .expect("encode client settings");
    assert_eq!(
        encoded,
        Some((play::serverbound::CLIENT_INFORMATION, settings_golden()))
    );
}

#[test]
fn client_information_is_byte_exact_in_configuration() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Configuration,
            &ClientAction::SetClientSettings(settings()),
        )
        .expect("encode client settings");
    assert_eq!(
        encoded,
        Some((
            configuration::serverbound::CLIENT_INFORMATION,
            settings_golden()
        ))
    );
}

#[test]
fn send_brand_is_byte_exact() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SendBrand {
                brand: "vanilla".to_owned(),
            },
        )
        .expect("encode brand");
    let mut want = Vec::new();
    want.extend_from_slice(&varint(15)); // "minecraft:brand" length
    want.extend_from_slice(b"minecraft:brand");
    want.extend_from_slice(&varint(7)); // "vanilla" length
    want.extend_from_slice(b"vanilla");
    assert_eq!(encoded, Some((play::serverbound::CUSTOM_PAYLOAD, want)));
}

#[test]
fn pong_response_is_big_endian_i32() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(ConnectionState::Play, &ClientAction::PongResponse { id: 42 })
        .expect("encode pong");
    assert_eq!(
        encoded,
        Some((play::serverbound::PONG, 42i32.to_be_bytes().to_vec()))
    );
}

#[test]
fn resource_pack_response_is_byte_exact() {
    let adapter = V770Adapter::new();
    let id = Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10);
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::ResourcePackResponse {
                id,
                response: ResourcePackResponseKind::Accepted,
            },
        )
        .expect("encode resource pack response");
    let mut want = Vec::new();
    want.extend_from_slice(id.as_bytes());
    want.extend_from_slice(&varint(3)); // Action.ACCEPTED
    assert_eq!(encoded, Some((play::serverbound::RESOURCE_PACK, want)));
}

#[test]
fn end_client_tick_is_an_empty_body() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(ConnectionState::Play, &ClientAction::EndClientTick)
        .expect("encode client tick end");
    assert_eq!(
        encoded,
        Some((play::serverbound::CLIENT_TICK_END, Vec::new()))
    );
}

#[test]
fn end_client_tick_is_not_encoded_outside_play() {
    let adapter = V770Adapter::new();
    assert_eq!(
        adapter
            .encode_action(ConnectionState::Configuration, &ClientAction::EndClientTick)
            .expect("encode"),
        None,
        "client_tick_end is a play-state action only"
    );
}
