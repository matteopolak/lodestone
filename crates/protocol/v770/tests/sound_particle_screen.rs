//! Hermetic tests for the protocol 776 `sound`, `sound_entity`,
//! `level_particles`, `open_screen`, and `stop_sound` packets.
//!
//! Golden byte vectors are hand-assembled from the wire specification
//! (behavioural reference only), so a symmetric encode/decode bug cannot pass
//! silently. Registry-referenced names are pinned against the generated tables,
//! and the server-rolled sound `seed` is asserted to survive decode untouched.

use lodestone_model::{
    ClientEvent, ConnectionState, Directive, ResourceKey, SoundCategory, Text, Vec3, Vec3f,
    VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

fn handle(adapter: &V770Adapter, packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle packet")
}

fn key(name: &str) -> ResourceKey {
    name.parse().expect("valid resource key")
}

// ---- sound ----------------------------------------------------------------

/// A registry-referenced sound: holder id `1` maps to registry index `0`.
fn sound_registry_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.push(0x01); // holder id 1 -> registry index 0
    bytes.push(0x00); // SoundSource ordinal 0 = Master
    bytes.extend_from_slice(&8i32.to_be_bytes()); // x = 8 -> 1.0
    bytes.extend_from_slice(&(-16i32).to_be_bytes()); // y = -16 -> -2.0
    bytes.extend_from_slice(&24i32.to_be_bytes()); // z = 24 -> 3.0
    bytes.extend_from_slice(&0.5f32.to_be_bytes()); // volume
    bytes.extend_from_slice(&1.0f32.to_be_bytes()); // pitch
    bytes.extend_from_slice(&0x0102_0304_0506_0708i64.to_be_bytes()); // seed
    bytes
}

#[test]
fn sound_registry_reference_decodes_to_named_event() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::SOUND, &sound_registry_bytes());
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::Sound {
            sound: key("minecraft:entity.allay.ambient_with_item"),
            category: SoundCategory::Master,
            pos: Vec3 {
                x: 1.0,
                y: -2.0,
                z: 3.0
            },
            volume: 0.5,
            pitch: 1.0,
            fixed_range: None,
            seed: 0x0102_0304_0506_0708,
        })]
    );
}

#[test]
fn sound_inline_definition_decodes_name_and_range() {
    let adapter = V770Adapter::new();
    let name = "minecraft:custom_sound";
    let mut bytes = Vec::new();
    bytes.push(0x00); // holder id 0 -> inline definition
    let name_bytes = name.as_bytes();
    bytes.push(u8::try_from(name_bytes.len()).unwrap()); // VarInt length (< 128)
    bytes.extend_from_slice(name_bytes);
    bytes.push(0x01); // fixed range present
    bytes.extend_from_slice(&16.0f32.to_be_bytes()); // fixed range value
    bytes.push(0x07); // SoundSource ordinal 7 = Player
    bytes.extend_from_slice(&0i32.to_be_bytes()); // x = 0
    bytes.extend_from_slice(&0i32.to_be_bytes()); // y = 0
    bytes.extend_from_slice(&0i32.to_be_bytes()); // z = 0
    bytes.extend_from_slice(&2.0f32.to_be_bytes()); // volume
    bytes.extend_from_slice(&0.5f32.to_be_bytes()); // pitch
    bytes.extend_from_slice(&42i64.to_be_bytes()); // seed

    let directives = handle(&adapter, play::clientbound::SOUND, &bytes);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::Sound {
            sound: key(name),
            category: SoundCategory::Player,
            pos: Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0
            },
            volume: 2.0,
            pitch: 0.5,
            fixed_range: Some(16.0),
            seed: 42,
        })]
    );
}

#[test]
fn sound_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = sound_registry_bytes();
    payload.push(0x00);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SOUND,
        &payload,
    );
    assert!(result.is_err(), "a misaligned sound must be rejected");
}

#[test]
fn sound_rejects_unknown_registry_id() {
    let adapter = V770Adapter::new();
    let mut bytes = Vec::new();
    // Holder id far beyond the registry: VarInt for 9_000_000.
    bytes.extend_from_slice(&[0xC0, 0xCF, 0xA5, 0x04]);
    bytes.push(0x00);
    bytes.extend_from_slice(&0i32.to_be_bytes());
    bytes.extend_from_slice(&0i32.to_be_bytes());
    bytes.extend_from_slice(&0i32.to_be_bytes());
    bytes.extend_from_slice(&1.0f32.to_be_bytes());
    bytes.extend_from_slice(&1.0f32.to_be_bytes());
    bytes.extend_from_slice(&0i64.to_be_bytes());
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SOUND,
        &bytes,
    );
    assert!(
        result.is_err(),
        "an unknown sound registry id must be rejected"
    );
}

// ---- sound_entity ---------------------------------------------------------

#[test]
fn sound_entity_decodes_with_entity_id() {
    let adapter = V770Adapter::new();
    let mut bytes = Vec::new();
    bytes.push(0x01); // holder id 1 -> registry index 0
    bytes.push(0x06); // SoundSource ordinal 6 = Neutral
    bytes.extend_from_slice(&[0xAC, 0x02]); // VarInt entity id 300
    bytes.extend_from_slice(&1.0f32.to_be_bytes()); // volume
    bytes.extend_from_slice(&1.5f32.to_be_bytes()); // pitch
    bytes.extend_from_slice(&123i64.to_be_bytes()); // seed

    let directives = handle(&adapter, play::clientbound::SOUND_ENTITY, &bytes);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::EntitySound {
            sound: key("minecraft:entity.allay.ambient_with_item"),
            category: SoundCategory::Neutral,
            entity_id: 300,
            volume: 1.0,
            pitch: 1.5,
            fixed_range: None,
            seed: 123,
        })]
    );
}

// ---- level_particles ------------------------------------------------------

#[test]
fn level_particles_decodes_registry_particle() {
    let adapter = V770Adapter::new();
    let mut bytes = Vec::new();
    bytes.push(0x01); // override limiter (long distance) = true
    bytes.push(0x00); // always show = false
    bytes.extend_from_slice(&1.0f64.to_be_bytes()); // x
    bytes.extend_from_slice(&64.0f64.to_be_bytes()); // y
    bytes.extend_from_slice(&(-5.0f64).to_be_bytes()); // z
    bytes.extend_from_slice(&0.25f32.to_be_bytes()); // x dist
    bytes.extend_from_slice(&0.5f32.to_be_bytes()); // y dist
    bytes.extend_from_slice(&0.75f32.to_be_bytes()); // z dist
    bytes.extend_from_slice(&0.1f32.to_be_bytes()); // max speed
    bytes.extend_from_slice(&5i32.to_be_bytes()); // count
    bytes.push(0x00); // particle id 0 = angry_villager (no options)

    let directives = handle(&adapter, play::clientbound::LEVEL_PARTICLES, &bytes);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::Particles {
            particle: key("minecraft:angry_villager"),
            long_distance: true,
            pos: Vec3 {
                x: 1.0,
                y: 64.0,
                z: -5.0
            },
            offset: Vec3f {
                x: 0.25,
                y: 0.5,
                z: 0.75
            },
            max_speed: 0.1,
            count: 5,
        })]
    );
}

// ---- open_screen ----------------------------------------------------------

#[test]
fn open_screen_decodes_menu_and_title() {
    let adapter = V770Adapter::new();
    let mut bytes = Vec::new();
    bytes.push(0x01); // window id 1
    bytes.push(0x00); // menu id 0 = generic_9x1
    // NBT network form: String tag, no root name, modified-UTF8 "Hello".
    bytes.push(0x08); // TAG_String
    bytes.extend_from_slice(&5u16.to_be_bytes());
    bytes.extend_from_slice(b"Hello");

    let directives = handle(&adapter, play::clientbound::OPEN_SCREEN, &bytes);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ScreenOpened {
            window_id: 1,
            menu_type: key("minecraft:generic_9x1"),
            title: Text::literal("Hello".to_owned()),
        })]
    );
}

#[test]
fn open_screen_rejects_unknown_menu() {
    let adapter = V770Adapter::new();
    let mut bytes = Vec::new();
    bytes.push(0x01); // window id
    bytes.extend_from_slice(&[0xFF, 0x7F]); // VarInt menu id 16383 (out of range)
    bytes.push(0x08);
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(b"x");
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::OPEN_SCREEN,
        &bytes,
    );
    assert!(result.is_err(), "an unknown menu id must be rejected");
}

// ---- stop_sound -------------------------------------------------------

#[test]
fn stop_sound_decodes_neither_source_nor_name() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::STOP_SOUND, &[0x00]);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SoundStopped {
            sound: None,
            category: None,
        })]
    );
}

#[test]
fn stop_sound_decodes_source_only() {
    let adapter = V770Adapter::new();
    // flags 0x1 (source present), SoundSource ordinal 5 = Hostile.
    let directives = handle(&adapter, play::clientbound::STOP_SOUND, &[0x01, 0x05]);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SoundStopped {
            sound: None,
            category: Some(SoundCategory::Hostile),
        })]
    );
}

#[test]
fn stop_sound_decodes_name_only() {
    let adapter = V770Adapter::new();
    let mut bytes = vec![0x02]; // flags 0x2 (name present)
    let name = "minecraft:entity.pig.ambient";
    bytes.push(u8::try_from(name.len()).unwrap());
    bytes.extend_from_slice(name.as_bytes());
    let directives = handle(&adapter, play::clientbound::STOP_SOUND, &bytes);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SoundStopped {
            sound: Some(key(name)),
            category: None,
        })]
    );
}

#[test]
fn stop_sound_decodes_source_and_name() {
    let adapter = V770Adapter::new();
    let mut bytes = vec![0x03, 0x07]; // flags 0x3, SoundSource ordinal 7 = Player
    let name = "minecraft:entity.pig.ambient";
    bytes.push(u8::try_from(name.len()).unwrap());
    bytes.extend_from_slice(name.as_bytes());
    let directives = handle(&adapter, play::clientbound::STOP_SOUND, &bytes);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SoundStopped {
            sound: Some(key(name)),
            category: Some(SoundCategory::Player),
        })]
    );
}

#[test]
fn stop_sound_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::STOP_SOUND,
        &[0x00, 0xFF],
    );
    assert!(result.is_err(), "a trailing byte must be rejected");
}

#[test]
fn stop_sound_rejects_truncated_source() {
    let adapter = V770Adapter::new();
    // flags promises a source but the buffer ends before it arrives.
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::STOP_SOUND,
        &[0x01],
    );
    assert!(result.is_err(), "a truncated source must be rejected");
}
