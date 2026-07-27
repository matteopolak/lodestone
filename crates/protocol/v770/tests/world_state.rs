//! Hermetic tests for the protocol 776 world-state packets `respawn` and
//! `set_time`.
//!
//! Clientbound golden byte vectors are hand-built from the wire specification
//! (`ClientboundRespawnPacket` / `ClientboundSetTimePacket`, behavioural
//! reference only), so a symmetric encode/decode bug cannot pass silently.

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_v770::packets::game::{GlobalPos, Respawn};
use lodestone_v770::packets::time::SetTime;
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 776 };

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    let value = T::decode(&mut reader, CTX).expect("decode");
    reader.ensure_empty().expect("no trailing bytes");
    value
}

/// A `respawn` body: dimension-type holder id `0`, dimension
/// `minecraft:the_nether`, zero seed, survival game type, previous game type
/// `-1`, not debug, not flat, no last death location, zero portal cooldown, sea
/// level `63`, and a `data_to_keep` mask of `0`.
fn respawn_golden() -> Vec<u8> {
    let mut bytes = vec![0x00]; // dimension_type varint 0
    let dim = b"minecraft:the_nether";
    bytes.push(dim.len() as u8); // string length varint (20)
    bytes.extend_from_slice(dim);
    bytes.extend_from_slice(&[0x00; 8]); // seed i64 = 0
    bytes.push(0x00); // game_type survival
    bytes.push(0xFF); // previous_game_type -1
    bytes.push(0x00); // is_debug false
    bytes.push(0x00); // is_flat false
    bytes.push(0x00); // last_death_location None
    bytes.push(0x00); // portal_cooldown varint 0
    bytes.push(0x3F); // sea_level varint 63
    bytes.push(0x00); // data_to_keep 0
    bytes
}

#[test]
fn respawn_decodes_from_golden_bytes() {
    let body: Respawn = decode(&respawn_golden());
    assert_eq!(body.dimension_type, 0);
    assert_eq!(body.dimension, "minecraft:the_nether");
    assert_eq!(body.seed, 0);
    assert_eq!(body.game_type, 0);
    assert_eq!(body.previous_game_type, -1);
    assert!(!body.is_debug);
    assert!(!body.is_flat);
    assert_eq!(body.last_death_location, None);
    assert_eq!(body.portal_cooldown, 0);
    assert_eq!(body.sea_level, 63);
    assert_eq!(body.data_to_keep, 0);
}

#[test]
fn respawn_re_encodes_to_the_same_bytes() {
    // Symmetric check against the hand-built vector, so the decoder and encoder
    // are pinned to the wire layout rather than to each other.
    let body: Respawn = decode(&respawn_golden());
    assert_eq!(encode(&body), respawn_golden());
}

#[test]
fn respawn_decodes_present_last_death_location() {
    let mut bytes = vec![0x00];
    let dim = b"minecraft:overworld";
    bytes.push(dim.len() as u8);
    bytes.extend_from_slice(dim);
    bytes.extend_from_slice(&[0x00; 8]); // seed
    bytes.push(0x01); // creative
    bytes.push(0x00); // previous survival
    bytes.push(0x00);
    bytes.push(0x01); // is_flat true
    bytes.push(0x01); // last_death_location Some
    let death_dim = b"minecraft:overworld";
    bytes.push(death_dim.len() as u8);
    bytes.extend_from_slice(death_dim);
    bytes.extend_from_slice(&123_i64.to_be_bytes()); // packed BlockPos
    bytes.push(0x00); // portal_cooldown
    bytes.push(0x3F); // sea_level 63
    bytes.push(0x02); // data_to_keep
    let body: Respawn = decode(&bytes);
    assert_eq!(
        body.last_death_location,
        Some(GlobalPos {
            dimension: "minecraft:overworld".to_owned(),
            position: 123,
        })
    );
    assert!(body.is_flat);
    assert_eq!(body.data_to_keep, 2);
}

#[test]
fn handle_play_respawn_emits_respawned_event() {
    // Previously respawn only updated internal chunk shape and emitted
    // nothing — a decode-and-discard gap. It now also surfaces a
    // `ClientEvent::Respawned` so a consumer (HUD gamemode, dimension change,
    // last-death compass) actually receives it.
    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::RESPAWN,
            &respawn_golden(),
        )
        .expect("handle respawn");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Respawned {
            dimension,
            game_mode,
            previous_game_mode,
            last_death_location,
        })] => {
            assert_eq!(dimension.to_string(), "minecraft:the_nether");
            assert_eq!(*game_mode, lodestone_model::GameMode::Survival);
            assert_eq!(*previous_game_mode, None);
            assert_eq!(*last_death_location, None);
        }
        other => panic!("expected a single Respawned event, got {other:?}"),
    }
}

#[test]
fn handle_play_respawn_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = respawn_golden();
    payload.push(0xAB); // one byte too many
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::RESPAWN,
        &payload,
    );
    assert!(result.is_err(), "a misaligned respawn must be rejected");
}

#[test]
fn handle_play_respawn_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let mut payload = respawn_golden();
    payload.truncate(payload.len() - 1); // drop data_to_keep
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::RESPAWN,
        &payload,
    );
    assert!(result.is_err(), "a truncated respawn must error, not panic");
}

/// A `set_time` body: world age `1000`, one clock update — holder id `1`, total
/// ticks `6000`, partial tick `0.0`, rate `1.0`.
fn set_time_golden() -> Vec<u8> {
    let mut bytes = 1000_i64.to_be_bytes().to_vec(); // game_time
    bytes.push(0x01); // clock count varint 1
    bytes.push(0x01); // holder_id varint 1
    bytes.extend_from_slice(&[0xF0, 0x2E]); // total_ticks varlong 6000
    bytes.extend_from_slice(&0.0_f32.to_be_bytes()); // partial_tick
    bytes.extend_from_slice(&1.0_f32.to_be_bytes()); // rate
    bytes
}

#[test]
fn set_time_decodes_from_golden_bytes() {
    let body: SetTime = decode(&set_time_golden());
    assert_eq!(body.game_time, 1000);
    assert_eq!(body.clocks.len(), 1);
    let clock = &body.clocks[0];
    assert_eq!(clock.holder_id, 1);
    assert_eq!(clock.total_ticks, 6000);
    assert_eq!(clock.partial_tick, 0.0);
    assert_eq!(clock.rate, 1.0);
    assert_eq!(body.day_time(), 6000);
}

#[test]
fn set_time_day_time_falls_back_to_world_age_when_no_clocks() {
    let mut bytes = 42_i64.to_be_bytes().to_vec();
    bytes.push(0x00); // zero clock updates
    let body: SetTime = decode(&bytes);
    assert!(body.clocks.is_empty());
    assert_eq!(body.day_time(), 42);
}

#[test]
fn handle_play_set_time_emits_time_changed() {
    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SET_TIME,
            &set_time_golden(),
        )
        .expect("handle set_time");
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::TimeChanged {
            world_age: 1000,
            time_of_day: 6000,
        })]
    );
}

#[test]
fn handle_play_set_time_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = set_time_golden();
    payload.push(0x00);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_TIME,
        &payload,
    );
    assert!(result.is_err(), "a misaligned set_time must be rejected");
}
