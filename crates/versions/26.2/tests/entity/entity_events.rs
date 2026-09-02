//! Hermetic tests for the protocol 776 entity/simulation packets
//! `entity_event`, `rotate_head`, `set_passengers`, `set_entity_link`,
//! `take_item_entity`, `damage_event`, `hurt_animation`, `animate`,
//! `update_mob_effect`, `remove_mob_effect`, and `move_vehicle`.
//!
//! Clientbound golden byte vectors are hand-built from the wire specification
//! (behavioural reference only), so a symmetric encode/decode bug cannot pass
//! silently.

use lodestone_model::{
    AnimationAction, ClientEvent, ConnectionState, Directive, Vec3, VersionAdapter,
};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use lodestone_world::World;

fn handle(adapter: &V770Adapter, packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle packet")
}

fn expect_err(adapter: &V770Adapter, packet_id: i32, payload: &[u8]) {
    let result =
        adapter.handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload);
    assert!(
        result.is_err(),
        "expected packet {packet_id} to be rejected"
    );
}

/// Independent VarInt encoder (not the codec under test).
fn var_i32(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value as u32;
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out
}

// ---- entity_event -----------------------------------------------------

#[test]
fn entity_event_emits_id_and_status() {
    let adapter = V770Adapter::new();
    let mut payload = 42i32.to_be_bytes().to_vec(); // raw int entity id
    payload.push(3); // status byte
    let directives = handle(&adapter, play::clientbound::ENTITY_EVENT, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::EntityStatus {
            entity_id: 42,
            status: 3,
        })]
    );
}

#[test]
fn entity_event_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = 1i32.to_be_bytes().to_vec();
    payload.push(0);
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::ENTITY_EVENT, &payload);
}

#[test]
fn entity_event_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let payload = 1i32.to_be_bytes().to_vec(); // missing status byte
    expect_err(&adapter, play::clientbound::ENTITY_EVENT, &payload);
}

// ---- rotate_head --------------------------------------------------------

#[test]
fn rotate_head_unpacks_quarter_turn() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(9);
    payload.push(64i8 as u8); // 64 * 360/256 = 90 degrees
    let directives = handle(&adapter, play::clientbound::ROTATE_HEAD, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::EntityHeadRotation {
            entity_id: 9,
            head_yaw: 90.0,
        })]
    );
}

#[test]
fn rotate_head_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.push(0);
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::ROTATE_HEAD, &payload);
}

// ---- set_passengers ------------------------------------------------------

#[test]
fn set_passengers_decodes_varint_array() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(100); // vehicle id
    payload.extend_from_slice(&var_i32(2)); // count
    payload.extend_from_slice(&var_i32(7));
    payload.extend_from_slice(&var_i32(8));
    let directives = handle(&adapter, play::clientbound::SET_PASSENGERS, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::EntityPassengersChanged {
            vehicle_id: 100,
            passenger_ids: vec![7, 8],
        })]
    );
}

#[test]
fn set_passengers_handles_empty_list() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.extend_from_slice(&var_i32(0));
    let directives = handle(&adapter, play::clientbound::SET_PASSENGERS, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::EntityPassengersChanged {
            vehicle_id: 1,
            passenger_ids: vec![],
        })]
    );
}

#[test]
fn set_passengers_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.extend_from_slice(&var_i32(0));
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::SET_PASSENGERS, &payload);
}

#[test]
fn set_passengers_rejects_truncated_array() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.extend_from_slice(&var_i32(3)); // claims 3 entries but supplies none
    expect_err(&adapter, play::clientbound::SET_PASSENGERS, &payload);
}

// ---- set_entity_link -----------------------------------------------------

#[test]
fn set_entity_link_emits_holder() {
    let adapter = V770Adapter::new();
    let mut payload = 5i32.to_be_bytes().to_vec();
    payload.extend_from_slice(&9i32.to_be_bytes());
    let directives = handle(&adapter, play::clientbound::SET_ENTITY_LINK, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::EntityLeashed {
            entity_id: 5,
            holder_id: Some(9),
        })]
    );
}

#[test]
fn set_entity_link_zero_holder_means_unleashed() {
    let adapter = V770Adapter::new();
    let mut payload = 5i32.to_be_bytes().to_vec();
    payload.extend_from_slice(&0i32.to_be_bytes());
    let directives = handle(&adapter, play::clientbound::SET_ENTITY_LINK, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::EntityLeashed {
            entity_id: 5,
            holder_id: None,
        })]
    );
}

#[test]
fn set_entity_link_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = 1i32.to_be_bytes().to_vec();
    payload.extend_from_slice(&0i32.to_be_bytes());
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::SET_ENTITY_LINK, &payload);
}

// ---- take_item_entity -----------------------------------------------------

#[test]
fn take_item_entity_emits_pickup() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(11);
    payload.extend_from_slice(&var_i32(1));
    payload.extend_from_slice(&var_i32(4));
    let directives = handle(&adapter, play::clientbound::TAKE_ITEM_ENTITY, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ItemPickup {
            item_entity_id: 11,
            player_id: 1,
            amount: 4,
        })]
    );
}

#[test]
fn take_item_entity_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.extend_from_slice(&var_i32(1));
    payload.extend_from_slice(&var_i32(1));
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::TAKE_ITEM_ENTITY, &payload);
}

// ---- damage_event ----------------------------------------------------------

#[test]
fn damage_event_with_cause_and_direct_no_pos() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(3); // entity_id
    payload.extend_from_slice(&var_i32(5)); // damage_type_id
    payload.extend_from_slice(&var_i32(2)); // cause_id + 1 = 2 -> cause_id 1
    payload.extend_from_slice(&var_i32(3)); // direct_id + 1 = 3 -> direct_id 2
    payload.push(0); // has_pos = false
    let directives = handle(&adapter, play::clientbound::DAMAGE_EVENT, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::EntityDamaged {
            entity_id: 3,
            damage_type_id: 5,
            cause_id: Some(1),
            direct_id: Some(2),
            source_pos: None,
        })]
    );
}

#[test]
fn damage_event_with_no_cause_but_source_pos() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(3);
    payload.extend_from_slice(&var_i32(9));
    payload.extend_from_slice(&var_i32(0)); // cause_id + 1 = 0 -> None
    payload.extend_from_slice(&var_i32(0)); // direct_id + 1 = 0 -> None
    payload.push(1); // has_pos = true
    payload.extend_from_slice(&1.0f64.to_be_bytes());
    payload.extend_from_slice(&2.5f64.to_be_bytes());
    payload.extend_from_slice(&(-3.0f64).to_be_bytes());
    let directives = handle(&adapter, play::clientbound::DAMAGE_EVENT, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::EntityDamaged {
            entity_id: 3,
            damage_type_id: 9,
            cause_id: None,
            direct_id: None,
            source_pos: Some(Vec3 {
                x: 1.0,
                y: 2.5,
                z: -3.0
            }),
        })]
    );
}

#[test]
fn damage_event_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.extend_from_slice(&var_i32(1));
    payload.extend_from_slice(&var_i32(0));
    payload.extend_from_slice(&var_i32(0));
    payload.push(0);
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::DAMAGE_EVENT, &payload);
}

#[test]
fn damage_event_rejects_truncated_source_pos() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.extend_from_slice(&var_i32(1));
    payload.extend_from_slice(&var_i32(0));
    payload.extend_from_slice(&var_i32(0));
    payload.push(1); // claims a source pos follows
    payload.extend_from_slice(&1.0f64.to_be_bytes()); // but only x is present
    expect_err(&adapter, play::clientbound::DAMAGE_EVENT, &payload);
}

// ---- hurt_animation --------------------------------------------------------

#[test]
fn hurt_animation_emits_yaw() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(6);
    payload.extend_from_slice(&180.0f32.to_be_bytes());
    let directives = handle(&adapter, play::clientbound::HURT_ANIMATION, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::EntityHurtAnimation {
            entity_id: 6,
            yaw: 180.0,
        })]
    );
}

#[test]
fn hurt_animation_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.extend_from_slice(&0.0f32.to_be_bytes());
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::HURT_ANIMATION, &payload);
}

// ---- animate ----------------------------------------------------------

#[test]
fn animate_decodes_named_actions() {
    let adapter = V770Adapter::new();
    let cases = [
        (0u8, AnimationAction::SwingMainHand),
        (2, AnimationAction::WakeUp),
        (3, AnimationAction::SwingOffHand),
        (4, AnimationAction::CriticalHit),
        (5, AnimationAction::MagicCriticalHit),
    ];
    for (action_byte, expected) in cases {
        let mut payload = var_i32(1);
        payload.push(action_byte);
        let directives = handle(&adapter, play::clientbound::ANIMATE, &payload);
        assert_eq!(
            directives,
            vec![Directive::Emit(ClientEvent::EntityAnimation {
                entity_id: 1,
                action: expected,
            })]
        );
    }
}

#[test]
fn animate_falls_back_to_other_for_unnamed_action() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.push(1); // reserved/unused byte
    let directives = handle(&adapter, play::clientbound::ANIMATE, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::EntityAnimation {
            entity_id: 1,
            action: AnimationAction::Other(1),
        })]
    );
}

#[test]
fn animate_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.push(0);
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::ANIMATE, &payload);
}

// ---- update_mob_effect / remove_mob_effect --------------------------------

#[test]
fn update_mob_effect_decodes_speed_with_flags() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(2); // entity id
    payload.extend_from_slice(&var_i32(0)); // effect id 0 = minecraft:speed
    payload.extend_from_slice(&var_i32(1)); // amplifier
    payload.extend_from_slice(&var_i32(200)); // duration ticks
    payload.push(0b0000_1111); // ambient | visible | show_icon | blend
    let directives = handle(&adapter, play::clientbound::UPDATE_MOB_EFFECT, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::MobEffectApplied {
            entity_id: 2,
            effect: "minecraft:speed".parse().unwrap(),
            amplifier: 1,
            duration_ticks: 200,
            ambient: true,
            visible: true,
            show_icon: true,
            blend: true,
        })]
    );
}

#[test]
fn update_mob_effect_no_flags_set() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(2);
    payload.extend_from_slice(&var_i32(1)); // minecraft:slowness
    payload.extend_from_slice(&var_i32(0));
    payload.extend_from_slice(&var_i32(100));
    payload.push(0);
    let directives = handle(&adapter, play::clientbound::UPDATE_MOB_EFFECT, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::MobEffectApplied {
            entity_id: 2,
            effect: "minecraft:slowness".parse().unwrap(),
            amplifier: 0,
            duration_ticks: 100,
            ambient: false,
            visible: false,
            show_icon: false,
            blend: false,
        })]
    );
}

#[test]
fn update_mob_effect_rejects_unknown_effect_id() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(2);
    payload.extend_from_slice(&var_i32(9999));
    payload.extend_from_slice(&var_i32(0));
    payload.extend_from_slice(&var_i32(0));
    payload.push(0);
    expect_err(&adapter, play::clientbound::UPDATE_MOB_EFFECT, &payload);
}

#[test]
fn update_mob_effect_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(2);
    payload.extend_from_slice(&var_i32(0));
    payload.extend_from_slice(&var_i32(0));
    payload.extend_from_slice(&var_i32(0));
    payload.push(0);
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::UPDATE_MOB_EFFECT, &payload);
}

#[test]
fn remove_mob_effect_emits_entity_and_effect() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(4);
    payload.extend_from_slice(&var_i32(1)); // minecraft:slowness
    let directives = handle(&adapter, play::clientbound::REMOVE_MOB_EFFECT, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::MobEffectRemoved {
            entity_id: 4,
            effect: "minecraft:slowness".parse().unwrap(),
        })]
    );
}

#[test]
fn remove_mob_effect_rejects_unknown_effect_id() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(4);
    payload.extend_from_slice(&var_i32(9999));
    expect_err(&adapter, play::clientbound::REMOVE_MOB_EFFECT, &payload);
}

#[test]
fn remove_mob_effect_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(4);
    payload.extend_from_slice(&var_i32(0));
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::REMOVE_MOB_EFFECT, &payload);
}

// ---- move_vehicle ----------------------------------------------------------

#[test]
fn move_vehicle_emits_pos_and_rotation() {
    let adapter = V770Adapter::new();
    let mut payload = 10.0f64.to_be_bytes().to_vec();
    payload.extend_from_slice(&65.0f64.to_be_bytes());
    payload.extend_from_slice(&(-3.0f64).to_be_bytes());
    payload.extend_from_slice(&90.0f32.to_be_bytes());
    payload.extend_from_slice(&15.0f32.to_be_bytes());
    let directives = handle(&adapter, play::clientbound::MOVE_VEHICLE, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::VehicleMoved {
            pos: Vec3 {
                x: 10.0,
                y: 65.0,
                z: -3.0
            },
            yaw: 90.0,
            pitch: 15.0,
        })]
    );
}

#[test]
fn move_vehicle_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = 0.0f64.to_be_bytes().to_vec();
    payload.extend_from_slice(&0.0f64.to_be_bytes());
    payload.extend_from_slice(&0.0f64.to_be_bytes());
    payload.extend_from_slice(&0.0f32.to_be_bytes());
    payload.extend_from_slice(&0.0f32.to_be_bytes());
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::MOVE_VEHICLE, &payload);
}

#[test]
fn move_vehicle_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let mut payload = 0.0f64.to_be_bytes().to_vec();
    payload.extend_from_slice(&0.0f64.to_be_bytes());
    // missing z, yaw, pitch
    expect_err(&adapter, play::clientbound::MOVE_VEHICLE, &payload);
}
