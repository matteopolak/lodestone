//! Hermetic dispatch tests for the protocol 47 (Minecraft 1.8.9) packets wired
//! in this pass: equipment, animation, attach/mount, item pickup, block/world
//! events, sound, potion effects, weather/orb/painting spawns, scoreboard, and
//! the misc single-field state packets (spawn position, difficulty, camera,
//! tab list, experience).
//!
//! Every case drives the packet through the real
//! [`VersionAdapter::handle_packet`] — never the decoder in isolation — so a
//! correct-but-undispatched decoder (this crate's own recurring "island"
//! defect) fails here exactly as it would in production. Multiplexed packets
//! with no derived struct (`world_border`, `combat_event`, the three
//! scoreboard packets) use hand-assembled bytes via [`Writer`]'s primitives,
//! never this crate's own `Encode`, so a symmetric encode/decode bug cannot
//! pass silently. Fixture fields that are wire-adjacent and same-typed are
//! kept pairwise-distinct so a transposition cannot survive.

use lodestone_core::{Ctx, Encode, Reader, Writer};
use lodestone_model::{
    ClientAction, ClientEvent, CollisionRule, ConnectionState, Difficulty, DisplaySlot,
    EquipmentSlot, ObjectiveMode, ObjectiveRenderType, SoundCategory, TeamAction, TeamColor,
    VersionAdapter, Visibility,
};
use lodestone_v1_8::V47Adapter;
use lodestone_v1_8::packet_ids::play;
use lodestone_v1_8::packets::entity::{
    Animation, AttachEntity, ClientboundEntityEquipment, Collect, EntityEffect,
    RemoveEntityEffect, SpawnEntityExperienceOrb, SpawnEntityPainting, SpawnEntityWeather,
};
use lodestone_v1_8::packets::game::{
    CameraPacket, DifficultyPacket, Experience, PlayerlistHeader, SpawnPosition,
};
use lodestone_v1_8::packets::slot::Slot;
use lodestone_v1_8::packets::world::{
    BlockAction, BlockBreakAnimation, NamedSoundEffect, OpenSignEntity, WorldEvent,
};
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 47 };
const EPS: f32 = 1e-6;

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn dispatch(packet_id: i32, payload: &[u8]) -> Vec<lodestone_model::Directive> {
    let adapter = V47Adapter::new();
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle_packet")
}

fn dispatch_with(
    adapter: &V47Adapter,
    packet_id: i32,
    payload: &[u8],
) -> Vec<lodestone_model::Directive> {
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle_packet")
}

fn only_event(directives: Vec<lodestone_model::Directive>) -> ClientEvent {
    let mut events: Vec<ClientEvent> = directives
        .into_iter()
        .filter_map(|d| match d {
            lodestone_model::Directive::Emit(event) => Some(event),
            _ => None,
        })
        .collect();
    assert_eq!(events.len(), 1, "expected exactly one emitted event");
    events.remove(0)
}

// ---------------------------------------------------------------------------
// spawn_position / difficulty / camera / playerlist_header / experience
// ---------------------------------------------------------------------------

#[test]
fn spawn_position_reports_the_current_dimension() {
    let adapter = V47Adapter::new();
    // Join into the nether so `SpawnPositionChanged::dimension` must reflect
    // it rather than defaulting to the overworld.
    let login = lodestone_v1_8::packets::game::JoinGame {
        entity_id: 1,
        game_mode: 0,
        dimension: -1,
        difficulty: 0,
        max_players: 8,
        level_type: "default".to_owned(),
        reduced_debug_info: false,
    };
    dispatch_with(&adapter, play::clientbound::LOGIN, &encode(&login));

    let payload = encode(&SpawnPosition {
        location: lodestone_v1_8::packets::position::Position(lodestone_model::BlockPos::new(
            11, 4, 100,
        )),
    });
    match only_event(dispatch_with(&adapter, play::clientbound::SPAWN_POSITION, &payload)) {
        ClientEvent::SpawnPositionChanged { dimension, pos, .. } => {
            assert_eq!(dimension.to_string(), "minecraft:the_nether");
            assert_eq!((pos.x, pos.y, pos.z), (11, 4, 100));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn difficulty_maps_every_ordinal() {
    for (byte, expected) in [
        (0u8, Difficulty::Peaceful),
        (1, Difficulty::Easy),
        (2, Difficulty::Normal),
        (3, Difficulty::Hard),
    ] {
        let payload = encode(&DifficultyPacket { difficulty: byte });
        match only_event(dispatch(play::clientbound::DIFFICULTY, &payload)) {
            ClientEvent::DifficultyChanged { difficulty, locked } => {
                assert_eq!(difficulty, expected);
                assert!(!locked, "1.8 has no locked bit");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

#[test]
fn camera_dispatches_camera_set() {
    let payload = encode(&CameraPacket { camera_id: 42 });
    match only_event(dispatch(play::clientbound::CAMERA, &payload)) {
        ClientEvent::CameraSet { entity_id } => assert_eq!(entity_id, 42),
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn playerlist_header_parses_both_as_json() {
    let payload = encode(&PlayerlistHeader {
        header: "{\"text\":\"Header\"}".to_owned(),
        footer: "{\"text\":\"Footer\"}".to_owned(),
    });
    match only_event(dispatch(play::clientbound::PLAYERLIST_HEADER, &payload)) {
        ClientEvent::TabListChanged { header, footer } => {
            assert_eq!(header.to_plain_string(), "Header");
            assert_eq!(footer.to_plain_string(), "Footer");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn experience_carries_pairwise_distinct_fields_without_transposition() {
    let payload = encode(&Experience {
        bar: 0.25,
        level: 11,
        total: 1400,
    });
    match only_event(dispatch(play::clientbound::EXPERIENCE, &payload)) {
        ClientEvent::ExperienceChanged { progress, level, total } => {
            assert!((progress - 0.25).abs() < EPS);
            assert_eq!(level, 11);
            assert_eq!(total, 1400);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// entity_equipment — the 1.8-only ordinal table
// ---------------------------------------------------------------------------

#[test]
fn entity_equipment_ordinal_zero_is_main_hand_not_a_five_slot_table() {
    // Ordinal 1 is the case that actually distinguishes the 1.8 table
    // (`MainHand, Feet, Legs, Chest, Head`) from the modern
    // `EquipmentSlot::from_ordinal` table (`MainHand, OffHand, Feet, Legs,
    // Chest, Head`): ordinal 1 means boots in 1.8 and off-hand in the modern
    // table. Using the wrong table would report `OffHand` here.
    let payload = encode(&ClientboundEntityEquipment {
        entity_id: 9,
        slot: 1,
        item: Slot::Empty,
    });
    match only_event(dispatch(play::clientbound::ENTITY_EQUIPMENT, &payload)) {
        ClientEvent::EntityEquipmentUpdated { entity_id, equipment } => {
            assert_eq!(entity_id, 9);
            assert_eq!(equipment.len(), 1);
            assert_eq!(equipment[0].slot, EquipmentSlot::Feet);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn entity_equipment_every_1_8_ordinal_maps_correctly() {
    let cases = [
        (0i16, EquipmentSlot::MainHand),
        (1, EquipmentSlot::Feet),
        (2, EquipmentSlot::Legs),
        (3, EquipmentSlot::Chest),
        (4, EquipmentSlot::Head),
    ];
    for (ordinal, expected) in cases {
        let payload = encode(&ClientboundEntityEquipment {
            entity_id: 3,
            slot: ordinal,
            item: Slot::Empty,
        });
        match only_event(dispatch(play::clientbound::ENTITY_EQUIPMENT, &payload)) {
            ClientEvent::EntityEquipmentUpdated { equipment, .. } => {
                assert_eq!(equipment[0].slot, expected, "ordinal {ordinal}");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// animation
// ---------------------------------------------------------------------------

#[test]
fn animation_swing_main_hand() {
    let payload = encode(&Animation {
        entity_id: 5,
        animation: 0,
    });
    match only_event(dispatch(play::clientbound::ANIMATION, &payload)) {
        ClientEvent::EntityAnimation { entity_id, action } => {
            assert_eq!(entity_id, 5);
            assert_eq!(action, lodestone_model::AnimationAction::SwingMainHand);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// attach_entity — leash and mount/dismount folding
// ---------------------------------------------------------------------------

#[test]
fn attach_entity_leash_true_sets_and_clears_holder() {
    let leashed = encode(&AttachEntity {
        entity_id: 20,
        vehicle_id: 30,
        leash: true,
    });
    match only_event(dispatch(play::clientbound::ATTACH_ENTITY, &leashed)) {
        ClientEvent::EntityLeashed { entity_id, holder_id } => {
            assert_eq!(entity_id, 20);
            assert_eq!(holder_id, Some(30));
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let unleashed = encode(&AttachEntity {
        entity_id: 20,
        vehicle_id: -1,
        leash: true,
    });
    match only_event(dispatch(play::clientbound::ATTACH_ENTITY, &unleashed)) {
        ClientEvent::EntityLeashed { entity_id, holder_id } => {
            assert_eq!(entity_id, 20);
            assert_eq!(holder_id, None);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn attach_entity_mount_then_dismount_folds_into_full_passenger_lists() {
    let adapter = V47Adapter::new();

    // Two different passengers mount the same vehicle via separate packets;
    // the adapter must fold them into one growing list.
    let mount_a = encode(&AttachEntity {
        entity_id: 101,
        vehicle_id: 500,
        leash: false,
    });
    match only_event(dispatch_with(&adapter, play::clientbound::ATTACH_ENTITY, &mount_a)) {
        ClientEvent::EntityPassengersChanged { vehicle_id, passenger_ids } => {
            assert_eq!(vehicle_id, 500);
            assert_eq!(passenger_ids, vec![101]);
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let mount_b = encode(&AttachEntity {
        entity_id: 102,
        vehicle_id: 500,
        leash: false,
    });
    match only_event(dispatch_with(&adapter, play::clientbound::ATTACH_ENTITY, &mount_b)) {
        ClientEvent::EntityPassengersChanged { vehicle_id, passenger_ids } => {
            assert_eq!(vehicle_id, 500);
            assert_eq!(passenger_ids, vec![101, 102]);
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // Passenger 101 dismounts; the vehicle's remaining list must drop only
    // that id, not clear entirely (a bug that would go unnoticed with only
    // one tracked passenger).
    let dismount = encode(&AttachEntity {
        entity_id: 101,
        vehicle_id: -1,
        leash: false,
    });
    match only_event(dispatch_with(&adapter, play::clientbound::ATTACH_ENTITY, &dismount)) {
        ClientEvent::EntityPassengersChanged { vehicle_id, passenger_ids } => {
            assert_eq!(vehicle_id, 500);
            assert_eq!(passenger_ids, vec![102]);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// collect
// ---------------------------------------------------------------------------

#[test]
fn collect_dispatches_item_pickup() {
    let payload = encode(&Collect {
        collected_entity_id: 11,
        collector_entity_id: 4,
    });
    match only_event(dispatch(play::clientbound::COLLECT, &payload)) {
        ClientEvent::ItemPickup { item_entity_id, player_id, .. } => {
            assert_eq!(item_entity_id, 11);
            assert_eq!(player_id, 4);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// block_action — the chest meta 0/1 hole
// ---------------------------------------------------------------------------

#[test]
fn block_action_chest_resolves_despite_no_meta_zero_table_entry() {
    // Legacy block id 54 is a chest, whose flattening table has no entry at
    // meta 0 or 1 (only 2..=5, the real facings) — see the adapter's
    // `legacy_block_type_key` doc. A naive `meta = 0` lookup would resolve to
    // air; the scan-every-meta approach must not.
    let payload = encode(&BlockAction {
        location: lodestone_v1_8::packets::position::Position(lodestone_model::BlockPos::new(
            1, 2, 3,
        )),
        byte1: 1,
        byte2: 0,
        block_id: 54,
    });
    match only_event(dispatch(play::clientbound::BLOCK_ACTION, &payload)) {
        ClientEvent::BlockEvent { pos, b0, b1, block } => {
            assert_eq!((pos.x, pos.y, pos.z), (1, 2, 3));
            assert_eq!(b0, 1);
            assert_eq!(b1, 0);
            assert!(
                block.to_string().contains("chest"),
                "expected a chest-family key, got {block}"
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// block_break_animation / world_event / open_sign_entity
// ---------------------------------------------------------------------------

#[test]
fn block_break_animation_dispatches_block_destruction() {
    let payload = encode(&BlockBreakAnimation {
        entity_id: 6,
        location: lodestone_v1_8::packets::position::Position(lodestone_model::BlockPos::new(
            9, 8, 7,
        )),
        destroy_stage: 5,
    });
    match only_event(dispatch(play::clientbound::BLOCK_BREAK_ANIMATION, &payload)) {
        ClientEvent::BlockDestruction { entity_id, pos, progress } => {
            assert_eq!(entity_id, 6);
            assert_eq!((pos.x, pos.y, pos.z), (9, 8, 7));
            assert_eq!(progress, 5);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn world_event_dispatches_level_event_with_distinct_fields() {
    let payload = encode(&WorldEvent {
        effect_id: 1003,
        location: lodestone_v1_8::packets::position::Position(lodestone_model::BlockPos::new(
            4, 5, 6,
        )),
        data: 17,
        global: true,
    });
    match only_event(dispatch(play::clientbound::WORLD_EVENT, &payload)) {
        ClientEvent::LevelEvent { event, pos, data, global } => {
            assert_eq!(event, 1003);
            assert_eq!((pos.x, pos.y, pos.z), (4, 5, 6));
            assert_eq!(data, 17);
            assert!(global);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn open_sign_entity_always_edits_front_text() {
    let payload = encode(&OpenSignEntity {
        location: lodestone_v1_8::packets::position::Position(lodestone_model::BlockPos::new(
            0, 64, 0,
        )),
    });
    match only_event(dispatch(play::clientbound::OPEN_SIGN_ENTITY, &payload)) {
        ClientEvent::SignEditorOpened { pos, is_front_text } => {
            assert_eq!((pos.x, pos.y, pos.z), (0, 64, 0));
            assert!(is_front_text);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// named_sound_effect — pitch byte conversion
// ---------------------------------------------------------------------------

#[test]
fn named_sound_effect_converts_fixed_point_position_and_pitch_byte() {
    let payload = encode(&NamedSoundEffect {
        sound_name: "random.pop".to_owned(),
        x: 80,  // 10.0 blocks (x / 8)
        y: 160, // 20.0 blocks
        z: 240, // 30.0 blocks
        volume: 0.8,
        pitch: 63, // ~1.0 (63 / 63.0)
    });
    match only_event(dispatch(play::clientbound::NAMED_SOUND_EFFECT, &payload)) {
        ClientEvent::Sound { sound, category, pos, volume, pitch, .. } => {
            assert_eq!(sound.to_string(), "minecraft:random.pop");
            assert_eq!(category, SoundCategory::Master);
            assert!((pos.x - 10.0).abs() < 1e-9);
            assert!((pos.y - 20.0).abs() < 1e-9);
            assert!((pos.z - 30.0).abs() < 1e-9);
            assert!((volume - 0.8).abs() < EPS);
            assert!((pitch - 1.0).abs() < 0.02);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// entity_effect / remove_entity_effect
// ---------------------------------------------------------------------------

#[test]
fn entity_effect_hide_particles_true_means_not_visible() {
    // Legacy effect id 1 is `minecraft:speed` (1-based).
    let payload = encode(&EntityEffect {
        entity_id: 8,
        effect_id: 1,
        amplifier: 2,
        duration: 600,
        hide_particles: true,
    });
    match only_event(dispatch(play::clientbound::ENTITY_EFFECT, &payload)) {
        ClientEvent::MobEffectApplied {
            entity_id,
            amplifier,
            duration_ticks,
            ambient,
            visible,
            show_icon,
            blend,
            ..
        } => {
            assert_eq!(entity_id, 8);
            assert_eq!(amplifier, 2);
            assert_eq!(duration_ticks, 600);
            assert!(!ambient, "1.8 sends no ambient bit on this packet");
            assert!(!visible, "hide_particles=true must clear visible");
            assert!(show_icon);
            assert!(!blend);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn remove_entity_effect_resolves_the_same_legacy_id_space() {
    let payload = encode(&RemoveEntityEffect {
        entity_id: 8,
        effect_id: 1,
    });
    match only_event(dispatch(play::clientbound::REMOVE_ENTITY_EFFECT, &payload)) {
        ClientEvent::MobEffectRemoved { entity_id, effect } => {
            assert_eq!(entity_id, 8);
            assert_eq!(effect.to_string(), "minecraft:speed");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// spawn_entity_weather / spawn_entity_experience_orb / spawn_entity_painting
// ---------------------------------------------------------------------------

#[test]
fn spawn_entity_weather_spawns_a_lightning_bolt() {
    let payload = encode(&SpawnEntityWeather {
        entity_id: 55,
        kind: 1,
        x: 32 * 10,
        y: 32 * 70,
        z: 32 * 20,
    });
    match only_event(dispatch(play::clientbound::SPAWN_ENTITY_WEATHER, &payload)) {
        ClientEvent::EntitySpawned { entity_id, uuid, entity_type, pos, .. } => {
            assert_eq!(entity_id, 55);
            assert_eq!(uuid, None);
            assert_eq!(entity_type.to_string(), "minecraft:lightning_bolt");
            assert!((pos.x - 10.0).abs() < 1e-9);
            assert!((pos.y - 70.0).abs() < 1e-9);
            assert!((pos.z - 20.0).abs() < 1e-9);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn spawn_entity_experience_orb_uses_fixed_point_coordinates() {
    let payload = encode(&SpawnEntityExperienceOrb {
        entity_id: 77,
        x: 32 * 3,
        y: 32 * 4,
        z: 32 * 5,
        count: 10,
    });
    match only_event(dispatch(play::clientbound::SPAWN_ENTITY_EXPERIENCE_ORB, &payload)) {
        ClientEvent::EntitySpawned { entity_id, entity_type, pos, .. } => {
            assert_eq!(entity_id, 77);
            assert_eq!(entity_type.to_string(), "minecraft:experience_orb");
            assert!((pos.x - 3.0).abs() < 1e-9);
            assert!((pos.y - 4.0).abs() < 1e-9);
            assert!((pos.z - 5.0).abs() < 1e-9);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn spawn_entity_painting_carries_no_uuid() {
    let payload = encode(&SpawnEntityPainting {
        entity_id: 88,
        title: "Aztec".to_owned(),
        location: lodestone_v1_8::packets::position::Position(lodestone_model::BlockPos::new(
            5, 6, 7,
        )),
        direction: 2,
    });
    match only_event(dispatch(play::clientbound::SPAWN_ENTITY_PAINTING, &payload)) {
        ClientEvent::EntitySpawned { entity_id, uuid, entity_type, pos, .. } => {
            assert_eq!(entity_id, 88);
            assert_eq!(uuid, None, "1.8 spawn_entity_painting carries no UUID field");
            assert_eq!(entity_type.to_string(), "minecraft:painting");
            assert_eq!((pos.x, pos.y, pos.z), (5.0, 6.0, 7.0));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// world_border — hand-assembled bytes, action-multiplexed
// ---------------------------------------------------------------------------

#[test]
fn world_border_set_size() {
    let mut w = Writer::default();
    w.var_i32(0); // action: SET_SIZE
    w.f64(123.5);
    match only_event(dispatch(play::clientbound::WORLD_BORDER, w.as_slice())) {
        ClientEvent::WorldBorderSizeChanged { size } => assert!((size - 123.5).abs() < 1e-9),
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn world_border_initialize_reads_every_field_in_order() {
    let mut w = Writer::default();
    w.var_i32(3); // action: INITIALIZE
    w.f64(11.0); // x
    w.f64(22.0); // z
    w.f64(33.0); // old_radius
    w.f64(44.0); // new_radius
    w.var_i64(5000); // speed (lerp time ms)
    w.var_i32(60_000_000); // portal boundary / absolute max size
    w.var_i32(15); // warning_time
    w.var_i32(5); // warning_blocks
    match only_event(dispatch(play::clientbound::WORLD_BORDER, w.as_slice())) {
        ClientEvent::WorldBorderInitialized {
            x,
            z,
            old_size,
            new_size,
            lerp_time_ms,
            absolute_max_size,
            warning_blocks,
            warning_time,
        } => {
            assert!((x - 11.0).abs() < 1e-9);
            assert!((z - 22.0).abs() < 1e-9);
            assert!((old_size - 33.0).abs() < 1e-9);
            assert!((new_size - 44.0).abs() < 1e-9);
            assert_eq!(lerp_time_ms, 5000);
            assert_eq!(absolute_max_size, 60_000_000);
            assert_eq!(warning_blocks, 5);
            assert_eq!(warning_time, 15);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// combat_event — hand-assembled bytes, action-multiplexed
// ---------------------------------------------------------------------------

#[test]
fn combat_event_enter_combat_has_no_payload() {
    let mut w = Writer::default();
    w.var_i32(0);
    match only_event(dispatch(play::clientbound::COMBAT_EVENT, w.as_slice())) {
        ClientEvent::PlayerCombatEntered => {}
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn combat_event_end_combat_reads_duration_then_discards_entity_id() {
    let mut w = Writer::default();
    w.var_i32(1);
    w.var_i32(240); // duration_ticks
    w.i32(99); // entity id, unused downstream
    match only_event(dispatch(play::clientbound::COMBAT_EVENT, w.as_slice())) {
        ClientEvent::PlayerCombatEnded { duration_ticks } => assert_eq!(duration_ticks, 240),
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn combat_event_entity_dead_surfaces_the_death_message() {
    let mut w = Writer::default();
    w.var_i32(2);
    w.var_i32(1); // player id, unused downstream
    w.i32(2); // killer entity id, unused downstream
    w.string("{\"text\":\"Steve was slain\"}");
    match only_event(dispatch(play::clientbound::COMBAT_EVENT, w.as_slice())) {
        ClientEvent::Death { message } => {
            assert_eq!(message.to_plain_string(), "Steve was slain");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// scoreboard_objective / scoreboard_score / scoreboard_display_objective
// ---------------------------------------------------------------------------

#[test]
fn scoreboard_objective_add_uses_plain_legacy_text() {
    let mut w = Writer::default();
    w.string("obj1");
    w.i8(0); // action: create
    w.string("\u{00A7}6Kills"); // legacy formatted, not JSON
    w.string("integer");
    match only_event(dispatch(play::clientbound::SCOREBOARD_OBJECTIVE, w.as_slice())) {
        ClientEvent::ObjectiveUpdate { name, mode, display_name, render_type, .. } => {
            assert_eq!(name, "obj1");
            assert_eq!(mode, ObjectiveMode::Add);
            assert_eq!(display_name.unwrap().to_plain_string(), "Kills");
            assert_eq!(render_type, Some(ObjectiveRenderType::Integer));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn scoreboard_score_holder_and_objective_are_not_transposed() {
    // `itemName` (holder) and `scoreName` (objective) are easy to swap since
    // minecraft-data's own field names are misleading; use distinct strings
    // so a transposition fails loudly.
    let mut w = Writer::default();
    w.string("Notch"); // holder
    w.var_i32(0); // action: update
    w.string("obj1"); // objective
    w.var_i32(42); // value
    match only_event(dispatch(play::clientbound::SCOREBOARD_SCORE, w.as_slice())) {
        ClientEvent::ScoreUpdate { holder, objective, value, .. } => {
            assert_eq!(holder, "Notch");
            assert_eq!(objective, "obj1");
            assert_eq!(value, 42);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn scoreboard_display_objective_maps_the_three_legacy_slots() {
    for (slot_byte, expected) in [
        (0u8, DisplaySlot::List),
        (1, DisplaySlot::Sidebar),
        (2, DisplaySlot::BelowName),
    ] {
        let mut w = Writer::default();
        w.i8(i8::try_from(slot_byte).unwrap());
        w.string("obj1");
        match only_event(dispatch(play::clientbound::SCOREBOARD_DISPLAY_OBJECTIVE, w.as_slice())) {
            ClientEvent::DisplayObjective { slot, objective } => {
                assert_eq!(slot, expected);
                assert_eq!(objective, Some("obj1".to_owned()));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// scoreboard_team — hand-assembled bytes, mode-multiplexed
// ---------------------------------------------------------------------------

#[test]
fn scoreboard_team_create_reads_every_field_in_order_with_no_collision_rule_on_wire() {
    let mut w = Writer::default();
    w.string("red"); // team
    w.i8(0); // mode: create
    w.string("Red Team"); // display_name
    w.string("[R] "); // prefix
    w.string(" [R]"); // suffix
    w.i8(0x03); // friendly_fire=true, see_friendly_invisibles=true
    w.string("hideForOtherTeams"); // name_tag_visibility
    w.i8(12); // color: Red (ordinal 12)
    w.var_i32(2); // player count
    w.string("Alice");
    w.string("Bob");
    match only_event(dispatch(play::clientbound::SCOREBOARD_TEAM, w.as_slice())) {
        ClientEvent::TeamUpdate { name, action } => {
            assert_eq!(name, "red");
            match action {
                TeamAction::Create { params, members } => {
                    assert_eq!(params.display_name.to_plain_string(), "Red Team");
                    assert_eq!(params.prefix.to_plain_string(), "[R] ");
                    assert_eq!(params.suffix.to_plain_string(), " [R]");
                    assert!(params.friendly_fire);
                    assert!(params.see_friendly_invisibles);
                    assert_eq!(params.name_tag_visibility, Visibility::HideForOtherTeams);
                    assert_eq!(params.collision_rule, CollisionRule::Always);
                    assert_eq!(params.color, Some(TeamColor::Red));
                    assert_eq!(members, vec!["Alice".to_owned(), "Bob".to_owned()]);
                }
                other => panic!("unexpected team action: {other:?}"),
            }
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn scoreboard_team_remove_and_membership_modes() {
    let mut remove = Writer::default();
    remove.string("blue");
    remove.i8(1);
    match only_event(dispatch(play::clientbound::SCOREBOARD_TEAM, remove.as_slice())) {
        ClientEvent::TeamUpdate { name, action } => {
            assert_eq!(name, "blue");
            assert_eq!(action, TeamAction::Remove);
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let mut add_members = Writer::default();
    add_members.string("blue");
    add_members.i8(3);
    add_members.var_i32(1);
    add_members.string("Carol");
    match only_event(dispatch(play::clientbound::SCOREBOARD_TEAM, add_members.as_slice())) {
        ClientEvent::TeamUpdate { action, .. } => {
            assert_eq!(action, TeamAction::AddMembers(vec!["Carol".to_owned()]));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// title — hand-assembled bytes, action-multiplexed. 1.8 has no action-bar
// case (added in 1.11), so only title/subtitle/times/clear/reset are tested.
// ---------------------------------------------------------------------------

#[test]
fn title_text_and_subtitle_actions_are_distinguishable() {
    let mut title = Writer::default();
    title.var_i32(0);
    title.string("\"Title\"");
    match only_event(dispatch(play::clientbound::TITLE, title.as_slice())) {
        ClientEvent::TitleText { text } => assert_eq!(text.to_legacy_string(), "Title"),
        other => panic!("unexpected event: {other:?}"),
    }

    let mut subtitle = Writer::default();
    subtitle.var_i32(1);
    subtitle.string("\"Subtitle\"");
    match only_event(dispatch(play::clientbound::TITLE, subtitle.as_slice())) {
        ClientEvent::SubtitleText { text } => assert_eq!(text.to_legacy_string(), "Subtitle"),
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn title_times_action_reads_pairwise_distinct_fields_in_order() {
    let mut w = Writer::default();
    w.var_i32(2); // action: TIMES
    w.i32(11); // fade_in
    w.i32(1); // stay
    w.i32(4); // fade_out
    match only_event(dispatch(play::clientbound::TITLE, w.as_slice())) {
        ClientEvent::TitlesAnimation { fade_in, stay, fade_out } => {
            assert_eq!(fade_in, 11);
            assert_eq!(stay, 1);
            assert_eq!(fade_out, 4);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn title_clear_and_reset_actions_set_reset_times_distinctly() {
    let mut clear = Writer::default();
    clear.var_i32(3);
    match only_event(dispatch(play::clientbound::TITLE, clear.as_slice())) {
        ClientEvent::TitlesCleared { reset_times } => assert!(!reset_times),
        other => panic!("unexpected event: {other:?}"),
    }

    let mut reset = Writer::default();
    reset.var_i32(4);
    match only_event(dispatch(play::clientbound::TITLE, reset.as_slice())) {
        ClientEvent::TitlesCleared { reset_times } => assert!(reset_times),
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// craft_progress_bar
// ---------------------------------------------------------------------------

#[test]
fn craft_progress_bar_dispatches_container_data_with_distinct_fields() {
    let mut w = Writer::default();
    w.u8(3); // window_id
    w.i16(7); // property
    w.i16(21); // value
    match only_event(dispatch(play::clientbound::CRAFT_PROGRESS_BAR, w.as_slice())) {
        ClientEvent::ContainerData { window_id, property, value } => {
            assert_eq!(window_id, 3);
            assert_eq!(property, 7);
            assert_eq!(value, 21);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// tab_complete — round trip through the same adapter instance, since 1.8's
// reply carries neither a transaction id nor a replacement range and must be
// reconstructed from the outgoing request `pending_tab_complete` remembered.
// ---------------------------------------------------------------------------

#[test]
fn tab_complete_request_encodes_text_with_no_looked_at_block() {
    let adapter = V47Adapter::new();
    let (packet_id, payload) = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::CommandSuggestion {
                id: 7,
                command: "/gib Ste".to_owned(),
            },
        )
        .expect("encode_action")
        .expect("protocol 47 can encode CommandSuggestion");
    assert_eq!(packet_id, play::serverbound::TAB_COMPLETE);
    let mut reader = Reader::new(&payload);
    let text = reader.string(32_767).expect("text");
    let has_block = reader.bool().expect("has_block");
    assert_eq!(text, "/gib Ste");
    assert!(!has_block);
}

#[test]
fn tab_complete_reply_reconstructs_id_and_range_from_the_request_it_answers() {
    let adapter = V47Adapter::new();
    adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::CommandSuggestion {
                id: 7,
                command: "/gib Ste".to_owned(),
            },
        )
        .expect("encode_action")
        .expect("protocol 47 can encode CommandSuggestion");

    let mut w = Writer::default();
    w.var_i32(2);
    w.string("Steve");
    w.string("Stella");
    match only_event(dispatch_with(&adapter, play::clientbound::TAB_COMPLETE, w.as_slice())) {
        ClientEvent::CommandSuggestionsReceived { id, start, length, suggestions } => {
            // Echoes the id the request used, not the wire (which has none).
            assert_eq!(id, 7);
            // "/gib Ste" is 8 bytes; the last word ("Ste") starts at byte 5.
            assert_eq!(start, 5);
            assert_eq!(length, 3);
            assert_eq!(
                suggestions.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
                vec!["Steve", "Stella"]
            );
            assert!(suggestions.iter().all(|s| s.tooltip.is_none()));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn tab_complete_reply_with_no_pending_request_falls_back_to_zeroed_range() {
    // A reply with no matching outgoing request (e.g. a stray/duplicate
    // packet) must not panic or desync — id/start/length fall back to 0
    // rather than reading uninitialized state.
    let adapter = V47Adapter::new();
    let mut w = Writer::default();
    w.var_i32(1);
    w.string("Steve");
    match only_event(dispatch_with(&adapter, play::clientbound::TAB_COMPLETE, w.as_slice())) {
        ClientEvent::CommandSuggestionsReceived { id, start, length, .. } => {
            assert_eq!(id, 0);
            assert_eq!(start, 0);
            assert_eq!(length, 0);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}
