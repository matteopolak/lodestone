//! Hermetic tests for the protocol 776 `sound`, `sound_entity`,
//! `level_particles`, `open_screen`, and `stop_sound` packets.
//!
//! Golden byte vectors are hand-assembled from the wire specification
//! (behavioural reference only), so a symmetric encode/decode bug cannot pass
//! silently. Registry-referenced names are pinned against the generated tables,
//! and the server-rolled sound `seed` is asserted to survive decode untouched.

use lodestone_model::{
    ClientEvent, ConnectionState, Directive, ParticleOptions, ResourceKey, SoundCategory, Text,
    Vec3, Vec3f, VersionAdapter,
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
            options: ParticleOptions::None,
        })]
    );
}

/// The gap this pass closed: before it, `LEVEL_PARTICLES`'s trailing option
/// bytes were captured (`#[mc(remaining)]`) and then thrown away entirely, so
/// `minecraft:dust` decoded a particle event with no colour at all. Registry
/// id 21 (`PARTICLE_TYPE_NAMES[21]`, generated from the real 26.2 registry
/// report) is `minecraft:dust`, whose payload is a packed RGB24 `i32` plus an
/// `f32` scale (`DustParticleOptions::STREAM_CODEC`). Pairwise-distinct R/G/B
/// bytes (`0x11`, `0x22`, `0x33`) so a channel transposition in the decode
/// could not survive this test unnoticed, matching `ARGB.red/green/blue`'s
/// `>> 16`/`>> 8`/plain `& 0xFF` order.
#[test]
fn level_particles_decodes_a_dust_payload() {
    let adapter = V770Adapter::new();
    let mut bytes = Vec::new();
    bytes.push(0x00); // override limiter = false
    bytes.push(0x00); // always show = false
    bytes.extend_from_slice(&2.0f64.to_be_bytes()); // x
    bytes.extend_from_slice(&65.0f64.to_be_bytes()); // y
    bytes.extend_from_slice(&3.0f64.to_be_bytes()); // z
    bytes.extend_from_slice(&0.0f32.to_be_bytes()); // x dist
    bytes.extend_from_slice(&0.0f32.to_be_bytes()); // y dist
    bytes.extend_from_slice(&0.0f32.to_be_bytes()); // z dist
    bytes.extend_from_slice(&0.0f32.to_be_bytes()); // max speed
    bytes.extend_from_slice(&1i32.to_be_bytes()); // count
    bytes.push(21); // particle id 21 = dust
    bytes.extend_from_slice(&0x0011_2233i32.to_be_bytes()); // packed RGB24
    bytes.extend_from_slice(&1.5f32.to_be_bytes()); // scale

    let directives = handle(&adapter, play::clientbound::LEVEL_PARTICLES, &bytes);
    let Directive::Emit(ClientEvent::Particles { particle, options, .. }) = &directives[0] else {
        panic!("expected a Particles directive, got {directives:?}");
    };
    assert_eq!(*particle, key("minecraft:dust"));
    assert_eq!(
        *options,
        ParticleOptions::Dust {
            color: [0x11 as f32 / 255.0, 0x22 as f32 / 255.0, 0x33 as f32 / 255.0],
            scale: 1.5,
        },
        "the RGB24 payload must unpack in ARGB's red/green/blue order, not transposed"
    );
}


/// A `LEVEL_PARTICLES` payload with a fixed prefix, `particle_id` as its
/// registry id, and `options` as the trailing type-specific bytes.
///
/// The prefix values are arbitrary but pairwise distinct so a field
/// transposition inside the fixed part cannot hide; the tests using this
/// helper assert on `options`, and lean on
/// `level_particles_decodes_position_and_count` above to pin the prefix.
fn level_particles_bytes(particle_id: u8, options: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.push(0x00); // override limiter = false
    bytes.push(0x00); // always show = false
    bytes.extend_from_slice(&2.0f64.to_be_bytes()); // x
    bytes.extend_from_slice(&65.0f64.to_be_bytes()); // y
    bytes.extend_from_slice(&3.0f64.to_be_bytes()); // z
    bytes.extend_from_slice(&0.0f32.to_be_bytes()); // x dist
    bytes.extend_from_slice(&0.0f32.to_be_bytes()); // y dist
    bytes.extend_from_slice(&0.0f32.to_be_bytes()); // z dist
    bytes.extend_from_slice(&0.0f32.to_be_bytes()); // max speed
    bytes.extend_from_slice(&1i32.to_be_bytes()); // count
    bytes.push(particle_id); // every id used here is < 0x80, so one VarInt byte
    bytes.extend_from_slice(options);
    bytes
}

/// The potion-effect family's payloads, which `decode_particle_options` had no
/// arm for until this pass: the colour rode the wire, was captured into
/// `#[mc(remaining)]`, and was then dropped, so every `effect`,
/// `instant_effect` and `entity_effect` particle reached the shell as
/// `ParticleOptions::None` and drew white.
///
/// Three registry types, three *different* option classes, and they cannot
/// share one arm — which is exactly the thing this asserts. Registry ids come
/// from `PARTICLE_TYPE_NAMES` (generated from the real 26.2 registry report):
/// 23 `effect`, 53 `instant_effect`, 28 `entity_effect`.
///
/// * `SpellParticleOption::streamCodec` is
///   `StreamCodec.composite(ByteBufCodecs.INT, colour, ByteBufCodecs.FLOAT,
///   power)` — eight bytes, and its accessors read only the three low bytes of
///   the word, so the top byte is **not** an alpha here.
/// * `ColorParticleOption::streamCodec` is `ByteBufCodecs.INT` alone — four
///   bytes, ARGB, and `SpellParticle.MobEffectProvider` really does call
///   `setAlpha(options.getAlpha())` with the top byte.
///
/// Every byte in every colour word below is pairwise distinct, so neither a
/// channel transposition nor an ARGB/RGB24 mix-up can survive: `0x44` as the
/// `entity_effect` alpha would show up as a red channel under the wrong
/// unpacking.
#[test]
fn level_particles_decodes_the_potion_effect_payloads() {
    let adapter = V770Adapter::new();

    // `effect`: RGB24 0x00112233 then power 2.5.
    let mut spell_payload = Vec::new();
    spell_payload.extend_from_slice(&0x0011_2233i32.to_be_bytes());
    spell_payload.extend_from_slice(&2.5f32.to_be_bytes());
    let directives = handle(
        &adapter,
        play::clientbound::LEVEL_PARTICLES,
        &level_particles_bytes(23, &spell_payload),
    );
    let Directive::Emit(ClientEvent::Particles { particle, options, .. }) = &directives[0] else {
        panic!("expected a Particles directive, got {directives:?}");
    };
    assert_eq!(*particle, key("minecraft:effect"));
    assert_eq!(
        *options,
        ParticleOptions::Spell {
            color: [0x11 as f32 / 255.0, 0x22 as f32 / 255.0, 0x33 as f32 / 255.0],
            power: 2.5,
        },
        "effect must decode a SpellParticleOption: RGB24 then an f32 power"
    );

    // `instant_effect` reads the same option class, over a different sheet.
    let directives = handle(
        &adapter,
        play::clientbound::LEVEL_PARTICLES,
        &level_particles_bytes(53, &spell_payload),
    );
    let Directive::Emit(ClientEvent::Particles { particle, options, .. }) = &directives[0] else {
        panic!("expected a Particles directive, got {directives:?}");
    };
    assert_eq!(*particle, key("minecraft:instant_effect"));
    assert_eq!(
        *options,
        ParticleOptions::Spell {
            color: [0x11 as f32 / 255.0, 0x22 as f32 / 255.0, 0x33 as f32 / 255.0],
            power: 2.5,
        },
    );

    // `entity_effect`: one ARGB word, alpha 0x44 in the top byte.
    let directives = handle(
        &adapter,
        play::clientbound::LEVEL_PARTICLES,
        &level_particles_bytes(28, &0x4411_2233u32.to_be_bytes()),
    );
    let Directive::Emit(ClientEvent::Particles { particle, options, .. }) = &directives[0] else {
        panic!("expected a Particles directive, got {directives:?}");
    };
    assert_eq!(*particle, key("minecraft:entity_effect"));
    assert_eq!(
        *options,
        ParticleOptions::Color {
            color: [
                0x11 as f32 / 255.0,
                0x22 as f32 / 255.0,
                0x33 as f32 / 255.0,
                0x44 as f32 / 255.0,
            ],
        },
        "entity_effect must decode a ColorParticleOption, alpha included"
    );
}

/// `sculk_charge` (registry id 45) carries a single `ByteBufCodecs.FLOAT`
/// roll — `SculkChargeParticleOptions::STREAM_CODEC`. The value is what makes
/// a spreading charge's motes lie along the direction it is travelling; with
/// the payload dropped they all shared one orientation.
///
/// A deliberately non-round angle: a roll that divides evenly into anything
/// (0, π, π/2) could agree with a zero default or a mis-scaled value by
/// coincidence.
#[test]
fn level_particles_decodes_a_sculk_charge_roll() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::LEVEL_PARTICLES,
        &level_particles_bytes(45, &1.234_5f32.to_be_bytes()),
    );
    let Directive::Emit(ClientEvent::Particles { particle, options, .. }) = &directives[0] else {
        panic!("expected a Particles directive, got {directives:?}");
    };
    assert_eq!(*particle, key("minecraft:sculk_charge"));
    assert_eq!(*options, ParticleOptions::SculkCharge { roll: 1.234_5 });
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

// ---- explode (issue: live player report, "creeper has no explosion sound") --
//
// `ClientboundExplodePacket`'s wire order (`ClientboundExplodePacket.java`'s
// `STREAM_CODEC.composite(...)` list): `center: Vec3` (three raw `f64`s, *not*
// the sound packet's fixed-point ints — see `Vec3.java`'s own `STREAM_CODEC`),
// `radius: f32`, `blockCount: i32` (plain 4-byte, `ByteBufCodecs.INT`),
// `playerKnockback: Optional<Vec3>`, `explosionParticle: ParticleOptions`,
// `explosionSound: Holder<SoundEvent>`, `blockParticles: WeightedList<...>`
// (not decoded — see `decode_explode`'s doc). Golden bytes are hand-assembled
// from that spec, not from our own encoder, per `CLAUDE.md`'s evidence
// standard.

/// A minimal, byte-accurate `explode` payload: centred at `(1.0, 2.0, 3.0)`,
/// radius `3.0`, `blockCount` `0`, no player knockback, the
/// `explosion_emitter` particle (registry id 29, a single-byte VarInt), and a
/// registry-referenced `minecraft:entity.generic.explode` sound (holder id
/// `700` = registry index `699`, a two-byte VarInt: low 7 bits `0x3C` with the
/// continuation bit set is `0xBC`, then the remaining `5` is `0x05`).
fn explode_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1.0f64.to_be_bytes()); // center.x
    bytes.extend_from_slice(&2.0f64.to_be_bytes()); // center.y
    bytes.extend_from_slice(&3.0f64.to_be_bytes()); // center.z
    bytes.extend_from_slice(&3.0f32.to_be_bytes()); // radius
    bytes.extend_from_slice(&0i32.to_be_bytes()); // blockCount
    bytes.push(0x00); // playerKnockback absent
    bytes.push(29); // explosionParticle: explosion_emitter, single-byte VarInt
    bytes.push(0xBC); // explosionSound holder id 700, byte 1
    bytes.push(0x05); // explosionSound holder id 700, byte 2
    // `blockParticles` deliberately omitted: `decode_explode` never reads it.
    bytes
}

#[test]
fn explode_decodes_the_explosion_sound_at_its_centre() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::EXPLODE, &explode_bytes());
    // A leading `Particles` directive (the shockwave/smoke
    // visual) now precedes the `Sound` directive this test was already
    // pinning — see `decode_explode`'s doc comment.
    assert_eq!(
        directives.len(),
        2,
        "one Particles directive, then one Sound directive"
    );
    let Directive::Emit(ClientEvent::Particles { particle, .. }) = &directives[0] else {
        panic!("expected a Particles directive first, got {:?}", directives[0]);
    };
    assert_eq!(*particle, key("minecraft:explosion_emitter"));
    let Directive::Emit(ClientEvent::Sound {
        sound,
        category,
        pos,
        volume,
        pitch,
        seed: _,
        fixed_range: _,
    }) = &directives[1]
    else {
        panic!("expected a Sound directive second, got {:?}", directives[1]);
    };
    assert_eq!(*sound, key("minecraft:entity.generic.explode"));
    assert_eq!(*category, SoundCategory::Block, "SoundSource.BLOCKS");
    assert_eq!(
        *pos,
        Vec3 {
            x: 1.0,
            y: 2.0,
            z: 3.0
        }
    );
    // `volume` (4.0) and `pitch`'s formula are client constants, never on the
    // wire (`ClientPacketListener.handleExplosion`); pitch is rolled fresh
    // each decode, so only its documented bound is checked, not an exact
    // value — `(1.0 ± 0.2) * 0.7` bounds to `[0.56, 0.84]`.
    assert_eq!(*volume, 4.0);
    assert!(
        (0.56..=0.84).contains(pitch),
        "pitch {pitch} outside vanilla's (1.0 +/- 0.2) * 0.7 band"
    );
}

/// The `explosion` particle (registry id 30) is the other simple particle type
/// `Level.explode`'s call sites can select; it must decode exactly like
/// `explosion_emitter` above.
#[test]
fn explode_accepts_the_plain_explosion_particle_too() {
    let adapter = V770Adapter::new();
    let mut bytes = explode_bytes();
    // Replace the single `explosionParticle` byte (29) with 30. Its position is
    // fixed: 8 (center) + 8 (center) + 8 (center) + 4 (radius) + 4 (blockCount)
    // + 1 (knockback absent) = 33.
    assert_eq!(bytes[33], 29);
    bytes[33] = 30;
    let directives = handle(&adapter, play::clientbound::EXPLODE, &bytes);
    assert_eq!(directives.len(), 2, "one Particles directive, then one Sound directive");
}

/// A **parameterised** `explosionParticle` must fail loudly rather than
/// silently misparse the rest of the packet: its stream codec reads trailing
/// arguments this decoder cannot skip byte-accurately, so continuing would
/// read `explosionSound` out of the middle of the particle's payload. Same
/// reject-rather-than-guess convention `metadata.rs`'s `SER_PARTICLE` uses.
///
/// **This test used to say "anything but 29/30" and pass id `5`.** The guard
/// it describes was later widened, correctly, from the two ids we happen to
/// draw to every `SimpleParticleType` — and `5` is `noxious_gas`, which is
/// simple, so the premise expired and the gate went red without anything
/// being wrong. The discriminating input is not "an id we do not draw", it is
/// **an id that carries arguments**: `1` is `minecraft:block`, whose codec
/// reads a block-state VarInt after the id.
#[test]
fn explode_rejects_a_parameterised_explosion_particle() {
    let adapter = V770Adapter::new();
    let mut bytes = explode_bytes();
    // `minecraft:block` — `lodestone_data::particle_types` classifies it
    // `false`, i.e. not skippable. Asserted here rather than assumed, so this
    // gate reports its own premise expiring instead of silently passing.
    assert!(
        !lodestone_data::particle_types::is_simple_particle_type(1),
        "particle id 1 must be parameterised or this gate proves nothing",
    );
    bytes[33] = 1;
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::EXPLODE,
        &bytes,
    );
    assert!(
        result.is_err(),
        "a particle whose trailing arguments cannot be skipped must not be guessed at",
    );
}

/// Player knockback, when present, must still leave the reader aligned to
/// reach `explosionSound` correctly — even though nothing consumes the
/// knockback value itself yet.
#[test]
fn explode_stays_aligned_past_a_present_player_knockback() {
    let adapter = V770Adapter::new();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0.0f64.to_be_bytes());
    bytes.extend_from_slice(&64.0f64.to_be_bytes());
    bytes.extend_from_slice(&0.0f64.to_be_bytes());
    bytes.extend_from_slice(&3.0f32.to_be_bytes());
    bytes.extend_from_slice(&12i32.to_be_bytes());
    bytes.push(0x01); // playerKnockback present
    bytes.extend_from_slice(&0.1f64.to_be_bytes());
    bytes.extend_from_slice(&0.2f64.to_be_bytes());
    bytes.extend_from_slice(&0.3f64.to_be_bytes());
    bytes.push(29); // explosion_emitter
    bytes.push(0xBC);
    bytes.push(0x05);
    let directives = handle(&adapter, play::clientbound::EXPLODE, &bytes);
    // `directives[0]` is now the leading `Particles` directive.
    let Directive::Emit(ClientEvent::Sound { sound, pos, .. }) = &directives[1] else {
        panic!("expected a Sound directive");
    };
    assert_eq!(*sound, key("minecraft:entity.generic.explode"));
    assert_eq!(
        *pos,
        Vec3 {
            x: 0.0,
            y: 64.0,
            z: 0.0
        }
    );
}
