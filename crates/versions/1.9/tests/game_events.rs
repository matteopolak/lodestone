//! Seam tests for protocol 340's entity-equipment, animation, sound,
//! scoreboard, team, and boss-bar packets.
//!
//! `ENTITY_EQUIPMENT`/`ANIMATION`/`NAMED_SOUND_EFFECT`/`SOUND_EFFECT`/
//! `SCOREBOARD_DISPLAY_OBJECTIVE` go through derived `Encode`/`Decode`
//! structs verified against minecraft-data's 1.12.2 `protocol.json`, so their
//! fixtures use the crate's own encoder like `tests/game.rs`'s
//! `BlockAction` tests do. `TEAMS`/`SCOREBOARD_OBJECTIVE`/`SCOREBOARD_SCORE`/
//! `BOSS_BAR` are mode/action-multiplexed and hand-decoded in the adapter, so
//! their fixtures are hand-assembled with a raw `Writer` instead — there is
//! no derived encoder for them to (mis)round-trip against.

use lodestone_core::{Ctx, Encode, Reader, Writer};
use lodestone_model::{
    AdapterError, AnimationAction, BossAction, BossColor, BossOverlay, ChatKind, ClientAction,
    ClientEvent, CollisionRule, ConnectionState, Directive, DisplaySlot, EntityEquipment,
    EquipmentSlot, ObjectiveMode, ObjectiveRenderType, SoundCategory, TeamAction, TeamColor,
    VersionAdapter, Visibility,
};
use lodestone_v1_9::V340Adapter;
use lodestone_v1_9::packet_ids::play;
use lodestone_v1_9::packets::game::{
    Animation, ClientboundEntityEquipment, NamedSoundEffect, ScoreboardDisplayObjective,
    SoundEffect,
};
use lodestone_v1_9::packets::slot::Slot;
use lodestone_world::World;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 340 };

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn dispatch(packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    let adapter = V340Adapter::new();
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle_packet")
}

fn dispatch_err(packet_id: i32, payload: &[u8]) -> AdapterError {
    let adapter = V340Adapter::new();
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect_err("expected a decode error")
}

// ---------------------------------------------------------------------------
// ENTITY_EQUIPMENT
// ---------------------------------------------------------------------------

#[test]
fn entity_equipment_resolves_slot_and_item() {
    let payload = encode(&ClientboundEntityEquipment {
        entity_id: 7,
        slot: 4, // Chest
        item: Slot::Item {
            id: 256, // iron_shovel
            count: 1,
            damage: 0,
            nbt: None,
        },
    });
    let directives = dispatch(play::clientbound::ENTITY_EQUIPMENT, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::EntityEquipmentUpdated {
            entity_id,
            equipment,
        })] => {
            assert_eq!(*entity_id, 7);
            assert_eq!(equipment.len(), 1);
            assert_eq!(
                equipment[0],
                EntityEquipment {
                    slot: EquipmentSlot::Chest,
                    item: Some(lodestone_model::ItemStack::new(
                        "minecraft:iron_shovel".parse().unwrap(),
                        1
                    )),
                }
            );
        }
        other => panic!("expected EntityEquipmentUpdated, got {other:?}"),
    }
}

#[test]
fn entity_equipment_empty_slot_clears_item() {
    let payload = encode(&ClientboundEntityEquipment {
        entity_id: 3,
        slot: 0, // MainHand
        item: Slot::Empty,
    });
    let directives = dispatch(play::clientbound::ENTITY_EQUIPMENT, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::EntityEquipmentUpdated { equipment, .. })] => {
            assert_eq!(equipment[0].slot, EquipmentSlot::MainHand);
            assert_eq!(equipment[0].item, None);
        }
        other => panic!("expected EntityEquipmentUpdated, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// ANIMATION
// ---------------------------------------------------------------------------

#[test]
fn animation_maps_named_codes() {
    for (code, expected) in [
        (0u8, AnimationAction::SwingMainHand),
        (2, AnimationAction::WakeUp),
        (3, AnimationAction::SwingOffHand),
        (4, AnimationAction::CriticalHit),
        (5, AnimationAction::MagicCriticalHit),
    ] {
        let payload = encode(&Animation {
            entity_id: 11,
            animation: code,
        });
        let directives = dispatch(play::clientbound::ANIMATION, &payload);
        match directives.as_slice() {
            [Directive::Emit(ClientEvent::EntityAnimation { entity_id, action })] => {
                assert_eq!(*entity_id, 11);
                assert_eq!(*action, expected);
            }
            other => panic!("expected EntityAnimation, got {other:?}"),
        }
    }
}

#[test]
fn animation_unrecognised_code_travels_intact_as_other() {
    let payload = encode(&Animation {
        entity_id: 9,
        animation: 1,
    });
    let directives = dispatch(play::clientbound::ANIMATION, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::EntityAnimation { action, .. })] => {
            assert_eq!(*action, AnimationAction::Other(1));
        }
        other => panic!("expected EntityAnimation, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// NAMED_SOUND_EFFECT / SOUND_EFFECT
// ---------------------------------------------------------------------------

#[test]
fn named_sound_effect_resolves_category_and_fixed_point_position() {
    let payload = encode(&NamedSoundEffect {
        sound_name: "record.13".to_owned(),
        sound_category: 2, // Record
        x: 80,             // 10.0
        y: 128,            // 16.0
        z: -24,            // -3.0
        volume: 0.5,
        pitch: 2.0,
    });
    let directives = dispatch(play::clientbound::NAMED_SOUND_EFFECT, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Sound {
            sound,
            category,
            pos,
            volume,
            pitch,
            fixed_range,
            seed,
        })] => {
            assert_eq!(sound.to_string(), "minecraft:record.13");
            assert_eq!(*category, SoundCategory::Record);
            assert_eq!((pos.x, pos.y, pos.z), (10.0, 16.0, -3.0));
            assert_eq!(*volume, 0.5);
            assert_eq!(*pitch, 2.0);
            assert_eq!(*fixed_range, None);
            assert_eq!(*seed, 0);
        }
        other => panic!("expected Sound, got {other:?}"),
    }
}

#[test]
fn sound_effect_resolves_legacy_id_through_the_sound_table() {
    let payload = encode(&SoundEffect {
        sound_id: 1, // block.anvil.break
        sound_category: 4,
        x: 8,
        y: 16,
        z: 24,
        volume: 1.0,
        pitch: 1.0,
    });
    let directives = dispatch(play::clientbound::SOUND_EFFECT, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Sound {
            sound, category, ..
        })] => {
            assert_eq!(sound.to_string(), "minecraft:block.anvil.break");
            assert_eq!(*category, SoundCategory::Block);
        }
        other => panic!("expected Sound, got {other:?}"),
    }
}

#[test]
fn sound_effect_unknown_legacy_id_is_a_decode_error() {
    let payload = encode(&SoundEffect {
        sound_id: 999_999,
        sound_category: 0,
        x: 0,
        y: 0,
        z: 0,
        volume: 1.0,
        pitch: 1.0,
    });
    let err = dispatch_err(play::clientbound::SOUND_EFFECT, &payload);
    assert!(matches!(err, AdapterError::Decode(_)));
}

// ---------------------------------------------------------------------------
// SCOREBOARD_DISPLAY_OBJECTIVE
// ---------------------------------------------------------------------------

#[test]
fn scoreboard_display_objective_assigns_and_clears_slots() {
    let payload = encode(&ScoreboardDisplayObjective {
        position: 1,
        name: "obj1".to_owned(),
    });
    let directives = dispatch(play::clientbound::SCOREBOARD_DISPLAY_OBJECTIVE, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::DisplayObjective {
            slot: DisplaySlot::Sidebar,
            objective: Some("obj1".to_owned()),
        })]
    );

    let payload = encode(&ScoreboardDisplayObjective {
        position: 0,
        name: String::new(),
    });
    let directives = dispatch(play::clientbound::SCOREBOARD_DISPLAY_OBJECTIVE, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::DisplayObjective {
            slot: DisplaySlot::List,
            objective: None,
        })]
    );
}

// ---------------------------------------------------------------------------
// SCOREBOARD_OBJECTIVE (hand-assembled: mode-multiplexed, no derived codec)
// ---------------------------------------------------------------------------

fn encode_scoreboard_objective_add_or_change(
    name: &str,
    action: i8,
    display_text: &str,
    render_type: &str,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(name);
    w.i8(action);
    w.string(display_text);
    w.string(render_type);
    w.into_vec()
}

fn encode_scoreboard_objective_remove(name: &str) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(name);
    w.i8(1);
    w.into_vec()
}

#[test]
fn scoreboard_objective_add_carries_display_and_render_type() {
    let payload = encode_scoreboard_objective_add_or_change("obj1", 0, "My Objective", "integer");
    let directives = dispatch(play::clientbound::SCOREBOARD_OBJECTIVE, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::ObjectiveUpdate {
            name,
            mode,
            display_name,
            render_type,
            number_format,
        })] => {
            assert_eq!(name, "obj1");
            assert_eq!(*mode, ObjectiveMode::Add);
            assert_eq!(
                display_name.as_ref().unwrap().to_plain_string(),
                "My Objective"
            );
            assert_eq!(*render_type, Some(ObjectiveRenderType::Integer));
            assert_eq!(*number_format, None);
        }
        other => panic!("expected ObjectiveUpdate, got {other:?}"),
    }
}

#[test]
fn scoreboard_objective_change_uses_hearts_render_type() {
    let payload = encode_scoreboard_objective_add_or_change("obj2", 2, "Health", "hearts");
    let directives = dispatch(play::clientbound::SCOREBOARD_OBJECTIVE, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::ObjectiveUpdate {
            mode, render_type, ..
        })] => {
            assert_eq!(*mode, ObjectiveMode::Change);
            assert_eq!(*render_type, Some(ObjectiveRenderType::Hearts));
        }
        other => panic!("expected ObjectiveUpdate, got {other:?}"),
    }
}

#[test]
fn scoreboard_objective_remove_carries_no_display_fields() {
    let payload = encode_scoreboard_objective_remove("obj3");
    let directives = dispatch(play::clientbound::SCOREBOARD_OBJECTIVE, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ObjectiveUpdate {
            name: "obj3".to_owned(),
            mode: ObjectiveMode::Remove,
            display_name: None,
            render_type: None,
            number_format: None,
        })]
    );
}

#[test]
fn scoreboard_objective_unknown_action_is_a_decode_error() {
    let mut w = Writer::default();
    w.string("obj4");
    w.i8(9);
    let err = dispatch_err(play::clientbound::SCOREBOARD_OBJECTIVE, &w.into_vec());
    assert!(matches!(err, AdapterError::Decode(_)));
}

// ---------------------------------------------------------------------------
// SCOREBOARD_SCORE (hand-assembled: action-multiplexed, no derived codec)
// ---------------------------------------------------------------------------

#[test]
fn scoreboard_score_update_carries_the_value() {
    let mut w = Writer::default();
    w.string("Steve");
    w.var_i32(0);
    w.string("obj1");
    w.var_i32(42);
    let directives = dispatch(play::clientbound::SCOREBOARD_SCORE, &w.into_vec());
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ScoreUpdate {
            holder: "Steve".to_owned(),
            objective: "obj1".to_owned(),
            value: 42,
            display: None,
            number_format: None,
        })]
    );
}

#[test]
fn scoreboard_score_remove_names_the_objective() {
    let mut w = Writer::default();
    w.string("Alex");
    w.var_i32(1);
    w.string("obj2");
    let directives = dispatch(play::clientbound::SCOREBOARD_SCORE, &w.into_vec());
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ScoreReset {
            holder: "Alex".to_owned(),
            objective: Some("obj2".to_owned()),
        })]
    );
}

// ---------------------------------------------------------------------------
// TEAMS (hand-assembled: mode-multiplexed, no derived codec)
// ---------------------------------------------------------------------------

fn encode_team_create(
    team: &str,
    display: &str,
    prefix: &str,
    suffix: &str,
    friendly_flags: i8,
    visibility: &str,
    collision: &str,
    color: i8,
    members: &[&str],
) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(team);
    w.i8(0);
    w.string(display);
    w.string(prefix);
    w.string(suffix);
    w.i8(friendly_flags);
    w.string(visibility);
    w.string(collision);
    w.i8(color);
    w.var_i32(members.len() as i32);
    for member in members {
        w.string(member);
    }
    w.into_vec()
}

#[test]
fn teams_create_carries_full_parameters_and_members() {
    let payload = encode_team_create(
        "red",
        "Red Team",
        "[R]",
        "!",
        0x03, // friendly fire + see friendly invisibles
        "hideForOtherTeams",
        "pushOwnTeam",
        9, // Blue
        &["Alice", "Bob"],
    );
    let directives = dispatch(play::clientbound::TEAMS, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::TeamUpdate { name, action })] => {
            assert_eq!(name, "red");
            match action {
                TeamAction::Create { params, members } => {
                    assert_eq!(params.display_name.to_plain_string(), "Red Team");
                    assert_eq!(params.prefix.to_plain_string(), "[R]");
                    assert_eq!(params.suffix.to_plain_string(), "!");
                    assert!(params.friendly_fire);
                    assert!(params.see_friendly_invisibles);
                    assert_eq!(
                        params.name_tag_visibility,
                        Visibility::HideForOtherTeams
                    );
                    assert_eq!(params.collision_rule, CollisionRule::PushOwnTeam);
                    assert_eq!(params.color, Some(TeamColor::Blue));
                    assert_eq!(members, &["Alice".to_owned(), "Bob".to_owned()]);
                }
                other => panic!("expected Create, got {other:?}"),
            }
        }
        other => panic!("expected TeamUpdate, got {other:?}"),
    }
}

#[test]
fn teams_remove_carries_no_parameters() {
    let mut w = Writer::default();
    w.string("blue");
    w.i8(1);
    let directives = dispatch(play::clientbound::TEAMS, &w.into_vec());
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::TeamUpdate {
            name: "blue".to_owned(),
            action: TeamAction::Remove,
        })]
    );
}

#[test]
fn teams_add_and_remove_members() {
    let mut w = Writer::default();
    w.string("green");
    w.i8(3);
    w.var_i32(2);
    w.string("Carol");
    w.string("Dave");
    let directives = dispatch(play::clientbound::TEAMS, &w.into_vec());
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::TeamUpdate {
            name: "green".to_owned(),
            action: TeamAction::AddMembers(vec!["Carol".to_owned(), "Dave".to_owned()]),
        })]
    );

    let mut w = Writer::default();
    w.string("green");
    w.i8(4);
    w.var_i32(1);
    w.string("Carol");
    let directives = dispatch(play::clientbound::TEAMS, &w.into_vec());
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::TeamUpdate {
            name: "green".to_owned(),
            action: TeamAction::RemoveMembers(vec!["Carol".to_owned()]),
        })]
    );
}

#[test]
fn teams_no_color_byte_resolves_to_none() {
    let payload = encode_team_create(
        "grey", "Grey", "", "", 0, "always", "always", -1, &[],
    );
    let directives = dispatch(play::clientbound::TEAMS, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::TeamUpdate { action, .. })] => match action {
            TeamAction::Create { params, .. } => assert_eq!(params.color, None),
            other => panic!("expected Create, got {other:?}"),
        },
        other => panic!("expected TeamUpdate, got {other:?}"),
    }
}

#[test]
fn teams_unknown_mode_is_a_decode_error() {
    let mut w = Writer::default();
    w.string("x");
    w.i8(9);
    let err = dispatch_err(play::clientbound::TEAMS, &w.into_vec());
    assert!(matches!(err, AdapterError::Decode(_)));
}

// ---------------------------------------------------------------------------
// BOSS_BAR (hand-assembled: action-multiplexed, no derived codec)
// ---------------------------------------------------------------------------

const BAR_UUID: Uuid = Uuid::from_u128(0x0123_4567_89ab_cdef_0011_2233_4455_6677);

#[test]
fn boss_bar_add_carries_full_parameters() {
    let mut w = Writer::default();
    w.uuid(BAR_UUID);
    w.var_i32(0);
    w.string("{\"text\":\"Dragon\"}");
    w.f32(0.75);
    w.var_i32(2); // Red
    w.var_i32(1); // Notched6
    w.u8(0x05); // darken + fog, no music
    let directives = dispatch(play::clientbound::BOSS_BAR, &w.into_vec());
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::BossBarUpdate { id, action })] => {
            assert_eq!(*id, BAR_UUID);
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
                    assert_eq!(title.to_plain_string(), "Dragon");
                    assert_eq!(*progress, 0.75);
                    assert_eq!(*color, BossColor::Red);
                    assert_eq!(*overlay, BossOverlay::Notched6);
                    assert!(*darken);
                    assert!(!*music);
                    assert!(*fog);
                }
                other => panic!("expected Add, got {other:?}"),
            }
        }
        other => panic!("expected BossBarUpdate, got {other:?}"),
    }
}

#[test]
fn boss_bar_remove_and_progress_and_name() {
    let mut w = Writer::default();
    w.uuid(BAR_UUID);
    w.var_i32(1);
    let directives = dispatch(play::clientbound::BOSS_BAR, &w.into_vec());
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::BossBarUpdate {
            id: BAR_UUID,
            action: BossAction::Remove,
        })]
    );

    let mut w = Writer::default();
    w.uuid(BAR_UUID);
    w.var_i32(2);
    w.f32(0.3);
    let directives = dispatch(play::clientbound::BOSS_BAR, &w.into_vec());
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::BossBarUpdate { action, .. })] => {
            assert_eq!(*action, BossAction::UpdateProgress(0.3));
        }
        other => panic!("expected BossBarUpdate, got {other:?}"),
    }

    let mut w = Writer::default();
    w.uuid(BAR_UUID);
    w.var_i32(3);
    w.string("{\"text\":\"New Name\"}");
    let directives = dispatch(play::clientbound::BOSS_BAR, &w.into_vec());
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::BossBarUpdate { action, .. })] => match action {
            BossAction::UpdateName(title) => assert_eq!(title.to_plain_string(), "New Name"),
            other => panic!("expected UpdateName, got {other:?}"),
        },
        other => panic!("expected BossBarUpdate, got {other:?}"),
    }
}

#[test]
fn boss_bar_update_style_and_flags() {
    let mut w = Writer::default();
    w.uuid(BAR_UUID);
    w.var_i32(4);
    w.var_i32(5); // Purple
    w.var_i32(3); // Notched12
    let directives = dispatch(play::clientbound::BOSS_BAR, &w.into_vec());
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::BossBarUpdate {
            id: BAR_UUID,
            action: BossAction::UpdateStyle {
                color: BossColor::Purple,
                overlay: BossOverlay::Notched12,
            },
        })]
    );

    let mut w = Writer::default();
    w.uuid(BAR_UUID);
    w.var_i32(5);
    w.u8(0x02); // music only
    let directives = dispatch(play::clientbound::BOSS_BAR, &w.into_vec());
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::BossBarUpdate {
            id: BAR_UUID,
            action: BossAction::UpdateFlags {
                darken: false,
                music: true,
                fog: false,
            },
        })]
    );
}

#[test]
fn boss_bar_unknown_action_is_a_decode_error() {
    let mut w = Writer::default();
    w.uuid(BAR_UUID);
    w.var_i32(9);
    let err = dispatch_err(play::clientbound::BOSS_BAR, &w.into_vec());
    assert!(matches!(err, AdapterError::Decode(_)));
}

// ---------------------------------------------------------------------------
// title — hand-assembled bytes, action-multiplexed. 1.12.2 adds an
// action-bar text case at `2` that 1.8 does not have, which pushes the
// times/clear/reset actions up by one from v1-8's numbering.
// ---------------------------------------------------------------------------

#[test]
fn title_text_and_subtitle_actions_are_distinguishable() {
    let mut title = Writer::default();
    title.var_i32(0);
    title.string("\"Title\"");
    match dispatch(play::clientbound::TITLE, title.as_slice()).as_slice() {
        [Directive::Emit(ClientEvent::TitleText { text })] => {
            assert_eq!(text.resolve(&|_| None).to_legacy_string(), "Title");
        }
        other => panic!("unexpected directives: {other:?}"),
    }

    let mut subtitle = Writer::default();
    subtitle.var_i32(1);
    subtitle.string("\"Subtitle\"");
    match dispatch(play::clientbound::TITLE, subtitle.as_slice()).as_slice() {
        [Directive::Emit(ClientEvent::SubtitleText { text })] => {
            assert_eq!(text.resolve(&|_| None).to_legacy_string(), "Subtitle");
        }
        other => panic!("unexpected directives: {other:?}"),
    }
}

#[test]
fn title_action_bar_case_maps_to_chat_game_info() {
    let mut w = Writer::default();
    w.var_i32(2); // action: ACTIONBAR — 1.12.2 only, absent from 1.8
    w.string("\"Action bar\"");
    match dispatch(play::clientbound::TITLE, w.as_slice()).as_slice() {
        [Directive::Emit(ClientEvent::Chat { text, kind, sender, ack })] => {
            assert_eq!(text.resolve(&|_| None).to_legacy_string(), "Action bar");
            assert_eq!(*kind, ChatKind::GameInfo);
            assert!(sender.is_none());
            assert!(ack.is_none());
        }
        other => panic!("unexpected directives: {other:?}"),
    }
}

#[test]
fn title_times_action_is_shifted_by_the_action_bar_case() {
    let mut w = Writer::default();
    w.var_i32(3); // action: TIMES (4 in 1.8's numbering, shifted by ACTIONBAR)
    w.i32(11); // fade_in
    w.i32(1); // stay
    w.i32(4); // fade_out
    match dispatch(play::clientbound::TITLE, w.as_slice()).as_slice() {
        [Directive::Emit(ClientEvent::TitlesAnimation { fade_in, stay, fade_out })] => {
            assert_eq!(*fade_in, 11);
            assert_eq!(*stay, 1);
            assert_eq!(*fade_out, 4);
        }
        other => panic!("unexpected directives: {other:?}"),
    }
}

#[test]
fn title_clear_and_reset_actions_are_shifted_and_distinct() {
    let mut clear = Writer::default();
    clear.var_i32(4);
    match dispatch(play::clientbound::TITLE, clear.as_slice()).as_slice() {
        [Directive::Emit(ClientEvent::TitlesCleared { reset_times })] => assert!(!reset_times),
        other => panic!("unexpected directives: {other:?}"),
    }

    let mut reset = Writer::default();
    reset.var_i32(5);
    match dispatch(play::clientbound::TITLE, reset.as_slice()).as_slice() {
        [Directive::Emit(ClientEvent::TitlesCleared { reset_times })] => assert!(reset_times),
        other => panic!("unexpected directives: {other:?}"),
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
    match dispatch(play::clientbound::CRAFT_PROGRESS_BAR, w.as_slice()).as_slice() {
        [Directive::Emit(ClientEvent::ContainerData { window_id, property, value })] => {
            assert_eq!(*window_id, 3);
            assert_eq!(*property, 7);
            assert_eq!(*value, 21);
        }
        other => panic!("unexpected directives: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// tab_complete — round trip through the same adapter instance, since
// 1.12.2's reply carries neither a transaction id nor a replacement range
// (both added in 1.13) and must be reconstructed from the outgoing request
// `pending_tab_complete` remembered.
// ---------------------------------------------------------------------------

#[test]
fn tab_complete_request_encodes_text_with_no_assume_command_or_looked_at_block() {
    let adapter = V340Adapter::new();
    let (packet_id, payload) = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::CommandSuggestion {
                id: 7,
                command: "/gib Ste".to_owned(),
            },
        )
        .expect("encode_action")
        .expect("protocol 340 can encode CommandSuggestion");
    assert_eq!(packet_id, play::serverbound::TAB_COMPLETE);
    let mut reader = Reader::new(&payload);
    let text = reader.string(32_767).expect("text");
    let assume_command = reader.bool().expect("assume_command");
    let has_block = reader.bool().expect("has_block");
    assert_eq!(text, "/gib Ste");
    assert!(!assume_command);
    assert!(!has_block);
}

#[test]
fn tab_complete_reply_reconstructs_id_and_range_from_the_request_it_answers() {
    let adapter = V340Adapter::new();
    adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::CommandSuggestion {
                id: 7,
                command: "/gib Ste".to_owned(),
            },
        )
        .expect("encode_action")
        .expect("protocol 340 can encode CommandSuggestion");

    let mut w = Writer::default();
    w.var_i32(2);
    w.string("Steve");
    w.string("Stella");
    match adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::TAB_COMPLETE,
            w.as_slice(),
        )
        .expect("handle_packet")
        .as_slice()
    {
        [Directive::Emit(ClientEvent::CommandSuggestionsReceived { id, start, length, suggestions })] => {
            assert_eq!(*id, 7);
            // "/gib Ste" is 8 bytes; the last word ("Ste") starts at byte 5.
            assert_eq!(*start, 5);
            assert_eq!(*length, 3);
            assert_eq!(
                suggestions.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
                vec!["Steve", "Stella"]
            );
        }
        other => panic!("unexpected directives: {other:?}"),
    }
}
