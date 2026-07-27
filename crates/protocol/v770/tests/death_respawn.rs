//! Hermetic tests for protocol 776 death/respawn handling.
//!
//! Golden serverbound/clientbound byte vectors are hand-built from the wire
//! specification so a symmetric encode/decode bug cannot pass silently.

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_model::{ClientAction, ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_v770::packets::game::{ClientCommand, SetHealth};
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 776 };

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    T::decode(&mut reader, CTX).expect("decode")
}

#[test]
fn set_health_round_trips_against_golden_bytes() {
    // health 20.0 (0x41A00000), food 20 (varint 0x14), saturation 5.0 (0x40A00000).
    let golden = [0x41, 0xA0, 0x00, 0x00, 0x14, 0x40, 0xA0, 0x00, 0x00];
    let body = SetHealth {
        health: 20.0,
        food: 20,
        saturation: 5.0,
    };
    assert_eq!(encode(&body), golden);
    let decoded: SetHealth = decode(&golden);
    assert_eq!(decoded, body);
}

#[test]
fn client_command_perform_respawn_is_single_zero_byte() {
    // Action enum ordinal: PERFORM_RESPAWN = 0, encoded as a VarInt.
    let body = ClientCommand { action: 0 };
    assert_eq!(encode(&body), [0x00]);
    let decoded: ClientCommand = decode(&[0x00]);
    assert_eq!(decoded, body);
}

#[test]
fn handle_play_set_health_emits_health_changed() {
    let adapter = V770Adapter::new();
    let payload = [0x41, 0xA0, 0x00, 0x00, 0x14, 0x40, 0xA0, 0x00, 0x00];
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SET_HEALTH,
            &payload,
        )
        .expect("handle set_health");
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::HealthChanged {
            health: 20.0,
            food: 20,
            saturation: 5.0,
        })]
    );
}

#[test]
fn handle_play_combat_kill_emits_death_with_message() {
    let adapter = V770Adapter::new();
    // VarInt player id 1, then a network-NBT bare-string component "You died".
    let mut payload = vec![0x01u8, 0x08, 0x00, 0x08];
    payload.extend_from_slice(b"You died");
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::PLAYER_COMBAT_KILL,
            &payload,
        )
        .expect("handle combat_kill");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Death { message })] => {
            assert_eq!(message.to_plain_string(), "You died");
        }
        other => panic!("expected a single Death event, got {other:?}"),
    }
}

#[test]
fn encode_action_respawn_targets_client_command() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(ConnectionState::Play, &ClientAction::Respawn)
        .expect("encode respawn");
    assert_eq!(
        encoded,
        Some((play::serverbound::CLIENT_COMMAND, vec![0x00]))
    );
}
