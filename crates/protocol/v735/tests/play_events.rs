//! Hermetic dispatch tests for the protocol 754 (Minecraft 1.16.5) clientbound
//! play packets wired up beyond the original join-flow set.
//!
//! Every packet here previously decoded nowhere in `handle_play` — either its
//! codec already existed and was only ever round-tripped (`UpdateHealth`,
//! `Respawn`, `SpawnPosition`, `HeldItemSlot`, `CloseWindow`), or it is wired
//! here for the first time. Bytes are hand-assembled with `Writer` (not
//! decoded from our own `Encode` impl) for every hand-`Reader`-decoded,
//! action/mode-multiplexed packet, per this repo's own evidence rule that
//! `decode(encode(x)) == x` cannot prove a wire shape is right.

use lodestone_core::{Ctx, Encode, Reader, Writer};
use lodestone_model::{
    AdapterError, AnimationAction, BossAction, BossColor, BossOverlay, ChatKind, ClientAction,
    ClientEvent, CollisionRule, ConnectionState, Difficulty, Directive, DisplaySlot, GameMode,
    ObjectiveMode, ObjectiveRenderType, TeamAction, TeamColor, Vec3, VersionAdapter, Visibility,
};
use lodestone_testsupport::assert_emits_set;
use lodestone_v735::V735Adapter;
use lodestone_v735::packet_ids::play;
use lodestone_v735::packets::entity::SpawnEntityExperienceOrb;
use lodestone_v735::packets::game::{
    AttachEntity, Collect, DifficultyPacket, EntityEffect, JoinGame, OpenSignEntity,
    PlayerlistHeader, RemoveEntityEffect, Respawn, SetPassengers, SpawnPosition, UpdateHealth,
    UpdateTime,
};
use lodestone_v735::packets::window::{CloseWindow, HeldItemSlot};
use lodestone_world::World;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 754 };

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn dispatch(packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let adapter = V735Adapter::new();
    adapter.handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
}

fn join(adapter: &V735Adapter, world_name: &str) {
    let body = JoinGame {
        entity_id: 1,
        is_hardcore: false,
        game_mode: 0,
        previous_game_mode: 255,
        world_names: vec![world_name.to_owned()],
        dimension_codec: vec![0x00],
        dimension: vec![0x00],
        world_name: world_name.to_owned(),
        hashed_seed: 0,
        max_players: 20,
        #[allow(clippy::cast_sign_loss)]
        view_distance: 10,
        reduced_debug_info: false,
        enable_respawn_screen: true,
        is_debug: false,
        is_flat: false,
    };
    let payload = encode(&body);
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, play::clientbound::LOGIN, &payload)
        .expect("handle login");
}

// ---------------------------------------------------------------------------
// Codecs that already existed, round-trip-tested only (real islands)
// ---------------------------------------------------------------------------

#[test]
fn update_health_dispatches() {
    let payload = encode(&UpdateHealth {
        health: 13.5,
        food: 17,
        food_saturation: 2.5,
    });
    let directives = dispatch(play::clientbound::UPDATE_HEALTH, &payload).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::HealthChanged {
            health: 13.5,
            food: 17,
            saturation: 2.5,
        }],
    );
}

#[test]
fn respawn_dispatches_and_updates_dimension() {
    let payload = encode(&Respawn {
        dimension: vec![0x00],
        world_name: "minecraft:the_end".to_owned(),
        hashed_seed: 0,
        game_mode: 3,
        previous_game_mode: 0,
        is_debug: false,
        is_flat: false,
        copy_metadata: true,
    });
    let directives = dispatch(play::clientbound::RESPAWN, &payload).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Respawned { dimension, game_mode, .. })] => {
            assert_eq!(dimension.to_string(), "minecraft:the_end");
            assert_eq!(*game_mode, GameMode::Spectator);
        }
        other => panic!("expected Respawned, got {other:?}"),
    }
}

#[test]
fn spawn_position_uses_dimension_from_prior_login() {
    let adapter = V735Adapter::new();
    join(&adapter, "minecraft:the_nether");
    let payload = encode(&SpawnPosition {
        location: lodestone_v735::packets::position::Position::new(10, 64, -5),
    });
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SPAWN_POSITION,
            &payload,
        )
        .expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::SpawnPositionChanged { dimension, pos, .. })] => {
            assert_eq!(dimension.to_string(), "minecraft:the_nether");
            assert_eq!(pos.x, 10);
            assert_eq!(pos.y, 64);
            assert_eq!(pos.z, -5);
        }
        other => panic!("expected SpawnPositionChanged, got {other:?}"),
    }
}

#[test]
fn held_item_slot_dispatches() {
    let payload = encode(&HeldItemSlot { slot: 4 });
    let directives = dispatch(play::clientbound::HELD_ITEM_SLOT, &payload).expect("handle");
    assert_emits_set(&directives, &[ClientEvent::HeldSlotChanged { slot: 4 }]);
}

#[test]
fn close_window_dispatches() {
    let payload = encode(&CloseWindow { window_id: 3 });
    let directives = dispatch(play::clientbound::CLOSE_WINDOW, &payload).expect("handle");
    assert_emits_set(&directives, &[ClientEvent::ScreenClosed { window_id: 3 }]);
}

// ---------------------------------------------------------------------------
// New derived-struct decoders
// ---------------------------------------------------------------------------

#[test]
fn abilities_flags_decode_correctly() {
    let mut w = Writer::default();
    // invulnerable | instabuild set; flying and can_fly clear.
    w.i8(0x01 | 0x08);
    w.f32(1.5);
    w.f32(0.75);
    let directives = dispatch(play::clientbound::ABILITIES, &w.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::AbilitiesChanged {
            invulnerable: true,
            flying: false,
            can_fly: false,
            instabuild: true,
            flying_speed: 1.5,
            walking_speed: 0.75,
        }],
    );
}

#[test]
fn difficulty_carries_the_locked_bit() {
    let payload = encode(&DifficultyPacket {
        difficulty: 2,
        difficulty_locked: true,
    });
    let directives = dispatch(play::clientbound::DIFFICULTY, &payload).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::DifficultyChanged {
            difficulty: Difficulty::Normal,
            locked: true,
        }],
    );
}

#[test]
fn update_time_dispatches() {
    let payload = encode(&UpdateTime { age: 1000, time: 6000 });
    let directives = dispatch(play::clientbound::UPDATE_TIME, &payload).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::TimeChanged {
            world_age: 1000,
            time_of_day: 6000,
        }],
    );
}

#[test]
fn playerlist_header_extracts_json_text() {
    let payload = encode(&PlayerlistHeader {
        header: "{\"text\":\"Header\"}".to_owned(),
        footer: "{\"text\":\"Footer\"}".to_owned(),
    });
    let directives = dispatch(play::clientbound::PLAYERLIST_HEADER, &payload).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::TabListChanged { header, footer })] => {
            assert_eq!(header.to_plain_string(), "Header");
            assert_eq!(footer.to_plain_string(), "Footer");
        }
        other => panic!("expected TabListChanged, got {other:?}"),
    }
}

#[test]
fn attach_entity_zero_vehicle_id_means_no_holder() {
    let payload = encode(&AttachEntity {
        entity_id: 11,
        vehicle_id: 0,
    });
    let directives = dispatch(play::clientbound::ATTACH_ENTITY, &payload).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::EntityLeashed {
            entity_id: 11,
            holder_id: None,
        }],
    );
}

#[test]
fn attach_entity_nonzero_vehicle_id_is_the_holder() {
    let payload = encode(&AttachEntity {
        entity_id: 11,
        vehicle_id: 42,
    });
    let directives = dispatch(play::clientbound::ATTACH_ENTITY, &payload).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::EntityLeashed {
            entity_id: 11,
            holder_id: Some(42),
        }],
    );
}

#[test]
fn set_passengers_dispatches() {
    let payload = encode(&SetPassengers {
        entity_id: 5,
        passengers: vec![11, 1, 4],
    });
    let directives = dispatch(play::clientbound::SET_PASSENGERS, &payload).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::EntityPassengersChanged {
            vehicle_id: 5,
            passenger_ids: vec![11, 1, 4],
        }],
    );
}

#[test]
fn collect_dispatches_with_pairwise_distinct_fields() {
    let payload = encode(&Collect {
        collected_entity_id: 11,
        collector_entity_id: 1,
        pickup_item_count: 4,
    });
    let directives = dispatch(play::clientbound::COLLECT, &payload).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::ItemPickup {
            item_entity_id: 11,
            player_id: 1,
            amount: 4,
        }],
    );
}

#[test]
fn entity_effect_flags_include_the_1_13_show_icon_bit() {
    let payload = encode(&EntityEffect {
        entity_id: 9,
        effect_id: 1, // legacy id 1 = speed -> 0-based 0
        amplifier: 2,
        duration: 200,
        flags: 0x04, // show_icon only: not ambient, not visible
    });
    let directives = dispatch(play::clientbound::ENTITY_EFFECT, &payload).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::MobEffectApplied {
            entity_id,
            effect,
            amplifier,
            duration_ticks,
            ambient,
            visible,
            show_icon,
            blend,
        })] => {
            assert_eq!(*entity_id, 9);
            assert_eq!(effect.to_string(), "minecraft:speed");
            assert_eq!(*amplifier, 2);
            assert_eq!(*duration_ticks, 200);
            assert!(!ambient);
            assert!(!visible);
            assert!(show_icon);
            assert!(!blend);
        }
        other => panic!("expected MobEffectApplied, got {other:?}"),
    }
}

#[test]
fn remove_entity_effect_resolves_legacy_id() {
    let payload = encode(&RemoveEntityEffect {
        entity_id: 9,
        effect_id: 1,
    });
    let directives = dispatch(play::clientbound::REMOVE_ENTITY_EFFECT, &payload).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::MobEffectRemoved { entity_id, effect })] => {
            assert_eq!(*entity_id, 9);
            assert_eq!(effect.to_string(), "minecraft:speed");
        }
        other => panic!("expected MobEffectRemoved, got {other:?}"),
    }
}

#[test]
fn spawn_entity_experience_orb_dispatches() {
    let payload = encode(&SpawnEntityExperienceOrb {
        entity_id: 22,
        x: 1.0,
        y: 65.0,
        z: -2.0,
        count: 7,
    });
    let directives =
        dispatch(play::clientbound::SPAWN_ENTITY_EXPERIENCE_ORB, &payload).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::EntitySpawned {
            entity_id,
            entity_type,
            pos,
            ..
        })] => {
            assert_eq!(*entity_id, 22);
            assert_eq!(entity_type.to_string(), "minecraft:experience_orb");
            assert_eq!(*pos, Vec3::new(1.0, 65.0, -2.0));
        }
        other => panic!("expected EntitySpawned, got {other:?}"),
    }
}

#[test]
fn open_sign_entity_dispatches() {
    let payload = encode(&OpenSignEntity {
        location: lodestone_v735::packets::position::Position::new(3, 70, 9),
    });
    let directives = dispatch(play::clientbound::OPEN_SIGN_ENTITY, &payload).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::SignEditorOpened { pos, is_front_text })] => {
            assert_eq!(pos.x, 3);
            assert_eq!(pos.y, 70);
            assert_eq!(pos.z, 9);
            assert!(is_front_text);
        }
        other => panic!("expected SignEditorOpened, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Hand-Reader-decoded, non-multiplexed packets
// ---------------------------------------------------------------------------

#[test]
fn entity_status_dispatches() {
    let mut w = Writer::default();
    w.i32(77);
    w.u8(3);
    let directives = dispatch(play::clientbound::ENTITY_STATUS, &w.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::EntityStatus {
            entity_id: 77,
            status: 3,
        }],
    );
}

#[test]
fn entity_head_rotation_unpacks_degrees() {
    let mut w = Writer::default();
    w.var_i32(5);
    w.i8(-64); // -90 degrees (256 steps/circle)
    let directives =
        dispatch(play::clientbound::ENTITY_HEAD_ROTATION, &w.into_vec()).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::EntityHeadRotation { entity_id, head_yaw })] => {
            assert_eq!(*entity_id, 5);
            assert!((*head_yaw - (-90.0)).abs() < 0.01);
        }
        other => panic!("expected EntityHeadRotation, got {other:?}"),
    }
}

#[test]
fn animation_maps_known_ids_and_falls_back_to_other() {
    let mut w = Writer::default();
    w.var_i32(6);
    w.u8(4); // critical hit
    let directives = dispatch(play::clientbound::ANIMATION, &w.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::EntityAnimation {
            entity_id: 6,
            action: AnimationAction::CriticalHit,
        }],
    );

    let mut w2 = Writer::default();
    w2.var_i32(6);
    w2.u8(200); // unknown -> Other fallback
    let directives2 = dispatch(play::clientbound::ANIMATION, &w2.into_vec()).expect("handle");
    assert_emits_set(
        &directives2,
        &[ClientEvent::EntityAnimation {
            entity_id: 6,
            action: AnimationAction::Other(200),
        }],
    );
}

#[test]
fn block_change_writes_a_flattened_state_id_and_reports_relative_coords() {
    // pos (17, 70, -1) -> section (1, 4, -1), relative (1, 6, 15).
    let mut w = Writer::default();
    let packed = lodestone_v735::packets::position::pack_position(lodestone_model::BlockPos::new(
        17, 70, -1,
    ));
    w.i64(packed);
    w.var_i32(1); // some real 1.16.5 flat state id
    let directives = dispatch(play::clientbound::BLOCK_CHANGE, &w.into_vec()).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::SectionBlocksChanged { section, blocks })] => {
            assert_eq!(section.x, 1);
            assert_eq!(section.y, 4);
            assert_eq!(section.z, -1);
            assert_eq!(blocks.as_slice(), &[[1u8, 6u8, 15u8]]);
        }
        other => panic!("expected SectionBlocksChanged, got {other:?}"),
    }
}

#[test]
fn experience_dispatches() {
    let mut w = Writer::default();
    w.f32(0.25);
    w.var_i32(11);
    w.var_i32(315);
    let directives = dispatch(play::clientbound::EXPERIENCE, &w.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::ExperienceChanged {
            progress: 0.25,
            level: 11,
            total: 315,
        }],
    );
}

#[test]
fn vehicle_move_dispatches() {
    let mut w = Writer::default();
    w.f64(1.0);
    w.f64(2.0);
    w.f64(3.0);
    w.f32(45.0);
    w.f32(-10.0);
    let directives = dispatch(play::clientbound::VEHICLE_MOVE, &w.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::VehicleMoved {
            pos: Vec3::new(1.0, 2.0, 3.0),
            yaw: 45.0,
            pitch: -10.0,
        }],
    );
}

#[test]
fn select_advancement_tab_present_and_absent() {
    let mut w = Writer::default();
    w.bool(true);
    w.string("minecraft:story/root");
    let directives =
        dispatch(play::clientbound::SELECT_ADVANCEMENT_TAB, &w.into_vec()).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::AdvancementsTabSelected { tab: Some(id) })] => {
            assert_eq!(id.to_string(), "minecraft:story/root");
        }
        other => panic!("expected AdvancementsTabSelected(Some), got {other:?}"),
    }

    let mut w2 = Writer::default();
    w2.bool(false);
    let directives2 =
        dispatch(play::clientbound::SELECT_ADVANCEMENT_TAB, &w2.into_vec()).expect("handle");
    assert_emits_set(
        &directives2,
        &[ClientEvent::AdvancementsTabSelected { tab: None }],
    );
}

#[test]
fn camera_dispatches() {
    let mut w = Writer::default();
    w.var_i32(88);
    let directives = dispatch(play::clientbound::CAMERA, &w.into_vec()).expect("handle");
    assert_emits_set(&directives, &[ClientEvent::CameraSet { entity_id: 88 }]);
}

#[test]
fn update_view_position_dispatches() {
    let mut w = Writer::default();
    w.var_i32(11);
    w.var_i32(-4);
    let directives =
        dispatch(play::clientbound::UPDATE_VIEW_POSITION, &w.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::ChunkCacheCenterChanged { x: 11, z: -4 }],
    );
}

#[test]
fn update_view_distance_dispatches() {
    let mut w = Writer::default();
    w.var_i32(12);
    let directives =
        dispatch(play::clientbound::UPDATE_VIEW_DISTANCE, &w.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::ChunkCacheRadiusChanged { radius: 12 }],
    );
}

// ---------------------------------------------------------------------------
// Multiplexed packets
// ---------------------------------------------------------------------------

#[test]
fn player_info_add_then_remove() {
    let uuid = Uuid::from_u128(1);
    let mut w = Writer::default();
    w.var_i32(0); // add_player
    w.var_i32(1);
    w.uuid(uuid);
    w.string("Steve");
    w.var_i32(0); // no properties
    w.var_i32(1); // creative
    w.var_i32(42); // ping
    w.bool(false); // no display name
    let directives = dispatch(play::clientbound::PLAYER_INFO, &w.into_vec()).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::PlayerListUpdate { entries })] => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name.as_deref(), Some("Steve"));
            assert_eq!(entries[0].game_mode, Some(GameMode::Creative));
            assert_eq!(entries[0].latency, Some(42));
        }
        other => panic!("expected PlayerListUpdate, got {other:?}"),
    }

    let mut w2 = Writer::default();
    w2.var_i32(4); // remove_player
    w2.var_i32(1);
    w2.uuid(uuid);
    let directives2 = dispatch(play::clientbound::PLAYER_INFO, &w2.into_vec()).expect("handle");
    match directives2.as_slice() {
        [Directive::Emit(ClientEvent::PlayerListRemove { profile_ids })] => {
            assert_eq!(profile_ids.as_slice(), &[uuid]);
        }
        other => panic!("expected PlayerListRemove, got {other:?}"),
    }
}

#[test]
fn boss_bar_add_action() {
    let id = Uuid::from_u128(2);
    let mut w = Writer::default();
    w.uuid(id);
    w.var_i32(0); // add
    w.string("{\"text\":\"Boss\"}");
    w.f32(0.5);
    w.var_i32(2); // red
    w.var_i32(1); // notched_6
    w.u8(0x01 | 0x04); // darken sky + create fog
    let directives = dispatch(play::clientbound::BOSS_BAR, &w.into_vec()).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::BossBarUpdate { id: got_id, action })] => {
            assert_eq!(*got_id, id);
            match action {
                BossAction::Add {
                    title,
                    progress,
                    color,
                    overlay,
                    darken,
                    music,
                    fog,
                } => {
                    assert_eq!(title.to_plain_string(), "Boss");
                    assert!((*progress - 0.5).abs() < 0.001);
                    assert_eq!(*color, BossColor::Red);
                    assert_eq!(*overlay, BossOverlay::Notched6);
                    assert!(darken);
                    assert!(!music);
                    assert!(fog);
                }
                other => panic!("expected Add, got {other:?}"),
            }
        }
        other => panic!("expected BossBarUpdate, got {other:?}"),
    }
}

#[test]
fn combat_event_death_carries_the_message() {
    let mut w = Writer::default();
    w.var_i32(2); // entity died
    w.var_i32(1); // player id, discarded
    w.i32(99); // killer entity id, discarded
    w.string("{\"text\":\"blew up\"}");
    let directives = dispatch(play::clientbound::COMBAT_EVENT, &w.into_vec()).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Death { message })] => {
            assert_eq!(message.to_plain_string(), "blew up");
        }
        other => panic!("expected Death, got {other:?}"),
    }
}

#[test]
fn world_border_initialize_reads_every_field_in_order() {
    let mut w = Writer::default();
    w.var_i32(3); // initialize
    w.f64(100.0); // x
    w.f64(-50.0); // z
    w.f64(200.0); // old_radius
    w.f64(300.0); // new_radius
    w.var_i64(5000); // speed
    w.var_i32(29_999_984); // portal boundary
    w.var_i32(15); // warning_time
    w.var_i32(5); // warning_blocks
    let directives = dispatch(play::clientbound::WORLD_BORDER, &w.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::WorldBorderInitialized {
            x: 100.0,
            z: -50.0,
            old_size: 200.0,
            new_size: 300.0,
            lerp_time_ms: 5000,
            absolute_max_size: 29_999_984,
            warning_blocks: 5,
            warning_time: 15,
        }],
    );
}

#[test]
fn teams_create_uses_1_16_field_order_not_1_12s() {
    // Field order for modes 0/2 in 1.16.2's own protocol.json: name,
    // friendlyFire, nameTagVisibility, collisionRule, formatting, prefix,
    // suffix — genuinely reordered from 1.12.2 (name, prefix, suffix,
    // friendlyFire, nameTagVisibility, collisionRule, color). Getting this
    // wrong reads plausible garbage instead of erroring, so this test pins
    // the exact order.
    let mut w = Writer::default();
    w.string("red_team");
    w.i8(0); // mode = create
    w.string("{\"text\":\"Red Team\"}"); // display_name
    w.i8(0x01 | 0x02); // friendly fire + see invisibles
    w.string("always"); // nameTagVisibility
    w.string("never"); // collisionRule
    w.var_i32(12); // formatting = red
    w.string("{\"text\":\"[R] \"}"); // prefix
    w.string("{\"text\":\" [R]\"}"); // suffix
    w.var_i32(1); // one member
    w.string("Steve");
    let directives = dispatch(play::clientbound::TEAMS, &w.into_vec()).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::TeamUpdate { name, action })] => {
            assert_eq!(name, "red_team");
            match action {
                TeamAction::Create { params, members } => {
                    assert_eq!(params.display_name.to_plain_string(), "Red Team");
                    assert_eq!(params.prefix.to_plain_string(), "[R] ");
                    assert_eq!(params.suffix.to_plain_string(), " [R]");
                    assert_eq!(params.name_tag_visibility, Visibility::Always);
                    assert_eq!(params.collision_rule, CollisionRule::Never);
                    assert_eq!(params.color, Some(TeamColor::Red));
                    assert!(params.friendly_fire);
                    assert!(params.see_friendly_invisibles);
                    assert_eq!(members.as_slice(), &["Steve".to_owned()]);
                }
                other => panic!("expected Create, got {other:?}"),
            }
        }
        other => panic!("expected TeamUpdate, got {other:?}"),
    }
}

#[test]
fn scoreboard_objective_render_type_is_a_varint_not_a_string() {
    // 1.16.2's `type` field is a VarInt ordinal (0 = integer, 1 = hearts),
    // unlike 1.12.2's plain "integer"/"hearts" string.
    let mut w = Writer::default();
    w.string("health_obj");
    w.i8(0); // add
    w.string("{\"text\":\"Health\"}");
    w.var_i32(1); // hearts
    let directives =
        dispatch(play::clientbound::SCOREBOARD_OBJECTIVE, &w.into_vec()).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::ObjectiveUpdate {
            name,
            mode,
            display_name,
            render_type,
            ..
        })] => {
            assert_eq!(name, "health_obj");
            assert_eq!(*mode, ObjectiveMode::Add);
            assert_eq!(
                display_name.as_ref().map(lodestone_model::Text::to_plain_string),
                Some("Health".to_owned())
            );
            assert_eq!(*render_type, Some(ObjectiveRenderType::Hearts));
        }
        other => panic!("expected ObjectiveUpdate, got {other:?}"),
    }
}

#[test]
fn scoreboard_display_objective_dispatches() {
    let mut w = Writer::default();
    w.i8(1); // sidebar
    w.string("health_obj");
    let directives =
        dispatch(play::clientbound::SCOREBOARD_DISPLAY_OBJECTIVE, &w.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::DisplayObjective {
            slot: DisplaySlot::Sidebar,
            objective: Some("health_obj".to_owned()),
        }],
    );
}

#[test]
fn scoreboard_score_holder_and_objective_are_not_transposed() {
    // `itemName` (wire field name) is the *holder*, `scoreName` is the
    // *objective* — the mcdata field names are misleading, not the wire
    // order. Pairwise-distinct strings so a transposition is visible.
    let mut w = Writer::default();
    w.string("Steve"); // holder
    w.var_i32(0); // action = update
    w.string("health_obj"); // objective
    w.var_i32(17); // value
    let directives = dispatch(play::clientbound::SCOREBOARD_SCORE, &w.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::ScoreUpdate {
            holder: "Steve".to_owned(),
            objective: "health_obj".to_owned(),
            value: 17,
            display: None,
            number_format: None,
        }],
    );
}

// ---------------------------------------------------------------------------
// title — hand-assembled bytes, action-multiplexed. Same shape as 1.12.2's:
// an action-bar text case at `2`, times shifted to `3`, clear/reset at `4`/`5`.
// ---------------------------------------------------------------------------

#[test]
fn title_text_and_subtitle_actions_are_distinguishable() {
    let mut title = Writer::default();
    title.var_i32(0);
    title.string("\"Title\"");
    let directives = dispatch(play::clientbound::TITLE, &title.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::TitleText { text: lodestone_model::Text::literal("Title") }],
    );

    let mut subtitle = Writer::default();
    subtitle.var_i32(1);
    subtitle.string("\"Subtitle\"");
    let directives = dispatch(play::clientbound::TITLE, &subtitle.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::SubtitleText { text: lodestone_model::Text::literal("Subtitle") }],
    );
}

#[test]
fn title_action_bar_case_maps_to_chat_game_info() {
    let mut w = Writer::default();
    w.var_i32(2);
    w.string("\"Action bar\"");
    let directives = dispatch(play::clientbound::TITLE, &w.into_vec()).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Chat { text, kind, sender, ack })] => {
            assert_eq!(text.to_legacy_string(), "Action bar");
            assert_eq!(*kind, ChatKind::GameInfo);
            assert!(sender.is_none());
            assert!(ack.is_none());
        }
        other => panic!("unexpected directives: {other:?}"),
    }
}

#[test]
fn title_times_action_reads_pairwise_distinct_fields_in_order() {
    let mut w = Writer::default();
    w.var_i32(3);
    w.i32(11); // fade_in
    w.i32(1); // stay
    w.i32(4); // fade_out
    let directives = dispatch(play::clientbound::TITLE, &w.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::TitlesAnimation { fade_in: 11, stay: 1, fade_out: 4 }],
    );
}

#[test]
fn title_clear_and_reset_actions_are_distinct() {
    let mut clear = Writer::default();
    clear.var_i32(4);
    let directives = dispatch(play::clientbound::TITLE, &clear.into_vec()).expect("handle");
    assert_emits_set(&directives, &[ClientEvent::TitlesCleared { reset_times: false }]);

    let mut reset = Writer::default();
    reset.var_i32(5);
    let directives = dispatch(play::clientbound::TITLE, &reset.into_vec()).expect("handle");
    assert_emits_set(&directives, &[ClientEvent::TitlesCleared { reset_times: true }]);
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
    let directives =
        dispatch(play::clientbound::CRAFT_PROGRESS_BAR, &w.into_vec()).expect("handle");
    assert_emits_set(
        &directives,
        &[ClientEvent::ContainerData { window_id: 3, property: 7, value: 21 }],
    );
}

// ---------------------------------------------------------------------------
// tab_complete — full parity with 26.2's shape (transaction id, start,
// length, matches with optional tooltips), so unlike v47/v340 no
// client-tracked state is needed on either side of the round trip.
// ---------------------------------------------------------------------------

#[test]
fn tab_complete_request_encodes_transaction_id_and_text() {
    let adapter = V735Adapter::new();
    let (packet_id, payload) = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::CommandSuggestion { id: 7, command: "/gib Ste".to_owned() },
        )
        .expect("encode_action")
        .expect("protocol 754 can encode CommandSuggestion");
    assert_eq!(packet_id, play::serverbound::TAB_COMPLETE);
    let mut reader = Reader::new(&payload);
    let id = reader.var_i32().expect("id");
    let text = reader.string(32_767).expect("text");
    assert_eq!(id, 7);
    assert_eq!(text, "/gib Ste");
}

#[test]
fn tab_complete_reply_reads_id_range_and_tooltips_straight_off_the_wire() {
    let mut w = Writer::default();
    w.var_i32(7); // transaction id
    w.var_i32(5); // start
    w.var_i32(3); // length
    w.var_i32(2); // count
    w.string("Steve");
    w.bool(false); // no tooltip
    w.string("Stella");
    w.bool(true);
    w.string("\"a player\""); // tooltip, JSON text component

    let directives = dispatch(play::clientbound::TAB_COMPLETE, &w.into_vec()).expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::CommandSuggestionsReceived { id, start, length, suggestions })] => {
            assert_eq!(*id, 7);
            assert_eq!(*start, 5);
            assert_eq!(*length, 3);
            assert_eq!(suggestions.len(), 2);
            assert_eq!(suggestions[0].text, "Steve");
            assert_eq!(suggestions[0].tooltip, None);
            assert_eq!(suggestions[1].text, "Stella");
            assert_eq!(suggestions[1].tooltip.as_deref(), Some("a player"));
        }
        other => panic!("unexpected directives: {other:?}"),
    }
}
