//! Hermetic dispatch tests for protocol 754 entity movement, velocity,
//! teleport, spawn and destroy packets.
//!
//! Like the v47 equivalent these are *seam* tests through the real
//! [`VersionAdapter::handle_packet`], with anti-vacuous scaling assertions: the
//! 1.16 delta scale is `1/4096` (not 1.8's `1/32`), so a divisor mix-up is off
//! by 128× and cannot pass. Written independently of v47 — the two families are
//! deliberately not unified.

use lodestone_core::{Ctx, Encode, Writer};
use lodestone_model::{ClientEvent, ConnectionState, Directive, EntityMovement, VersionAdapter};
use lodestone_testsupport::EntitySpawnWireFixture;
use lodestone_v735::V735Adapter;
use lodestone_v735::packet_ids::play;
use lodestone_v735::packets::entity::{
    EntityLook, EntityMoveLook, EntityTeleport, EntityVelocityPacket, RelEntityMove,
};
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 754 };

/// 1.16.5's entity-spawn wire quirks. See [`EntitySpawnWireFixture`]'s doc for
/// why this table exists instead of a templated test body.
const FIXTURE: EntitySpawnWireFixture = EntitySpawnWireFixture {
    creeper_id: 12,
    arrow_id: 2,
    boat_id: 6,
    object_type_id_is_byte: false,
    metadata_terminator: None,
};

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn dispatch(packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    let adapter = V735Adapter::new();
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle_packet")
}

fn try_dispatch(
    packet_id: i32,
    payload: &[u8],
) -> Result<Vec<Directive>, lodestone_model::AdapterError> {
    let adapter = V735Adapter::new();
    adapter.handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
}

const EPS: f64 = 1e-9;

#[test]
fn rel_entity_move_dispatches_relative_movement_in_four_thousand_ninety_sixths() {
    // dx = 4096 units = exactly 1 block (1/4096 scale, the 1.9+ widening).
    let payload = encode(&RelEntityMove {
        entity_id: 7,
        dx: 4096,
        dy: -2048,
        dz: 1024,
        on_ground: true,
    });
    match dispatch(play::clientbound::REL_ENTITY_MOVE, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntityMoved {
                entity_id,
                movement: EntityMovement::Relative(delta),
                rotation,
                on_ground,
            }),
        ] => {
            assert_eq!(*entity_id, 7);
            assert!(
                (delta.x - 1.0).abs() < EPS,
                "dx should be 1.0 block, got {}",
                delta.x
            );
            assert!((delta.y - -0.5).abs() < EPS);
            assert!((delta.z - 0.25).abs() < EPS);
            assert!(rotation.is_none());
            assert!(*on_ground);
        }
        other => panic!("expected relative EntityMoved, got {other:?}"),
    }
}

#[test]
fn entity_look_dispatches_rotation_only() {
    let payload = encode(&EntityLook {
        entity_id: 3,
        yaw: 64,
        pitch: -64,
        on_ground: false,
    });
    match dispatch(play::clientbound::ENTITY_LOOK, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntityMoved {
                entity_id,
                movement: EntityMovement::Relative(delta),
                rotation: Some(rot),
                ..
            }),
        ] => {
            assert_eq!(*entity_id, 3);
            assert_eq!(*delta, lodestone_model::Vec3::new(0.0, 0.0, 0.0));
            assert!((f64::from(rot.yaw) - 90.0).abs() < 1e-4);
            assert!((f64::from(rot.pitch) - -90.0).abs() < 1e-4);
        }
        other => panic!("expected rotation EntityMoved, got {other:?}"),
    }
}

#[test]
fn entity_move_look_dispatches_delta_and_rotation() {
    let payload = encode(&EntityMoveLook {
        entity_id: 11,
        dx: -4096,
        dy: 0,
        dz: 8192,
        yaw: 0,
        pitch: 0,
        on_ground: true,
    });
    match dispatch(play::clientbound::ENTITY_MOVE_LOOK, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntityMoved {
                movement: EntityMovement::Relative(delta),
                rotation: Some(_),
                ..
            }),
        ] => {
            assert!((delta.x - -1.0).abs() < EPS);
            assert!((delta.z - 2.0).abs() < EPS);
        }
        other => panic!("expected move+look EntityMoved, got {other:?}"),
    }
}

#[test]
fn entity_teleport_dispatches_absolute_f64_position() {
    // 1.16 sends absolute f64 directly — no fixed-point conversion.
    let payload = encode(&EntityTeleport {
        entity_id: 99,
        x: 64.5,
        y: 70.0,
        z: -2.25,
        yaw: 0,
        pitch: 0,
        on_ground: false,
    });
    match dispatch(play::clientbound::ENTITY_TELEPORT, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntityMoved {
                movement: EntityMovement::Absolute(pos),
                ..
            }),
        ] => {
            assert!(
                (pos.x - 64.5).abs() < EPS,
                "x should be 64.5, got {}",
                pos.x
            );
            assert!((pos.y - 70.0).abs() < EPS);
            assert!((pos.z - -2.25).abs() < EPS);
        }
        other => panic!("expected absolute EntityMoved, got {other:?}"),
    }
}

#[test]
fn entity_velocity_dispatches_in_eight_thousandths() {
    let payload = encode(&EntityVelocityPacket {
        entity_id: 5,
        velocity_x: 8000,
        velocity_y: -4000,
        velocity_z: 0,
    });
    match dispatch(play::clientbound::ENTITY_VELOCITY, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntityVelocity {
                entity_id,
                velocity,
            }),
        ] => {
            assert_eq!(*entity_id, 5);
            assert!((velocity.x - 1.0).abs() < EPS);
            assert!((velocity.y - -0.5).abs() < EPS);
        }
        other => panic!("expected EntityVelocity, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Spawns (1.16 carries UUIDs and f64 coordinates)
// ---------------------------------------------------------------------------

#[test]
fn spawn_entity_living_resolves_mob_type_uuid_and_f64_coords() {
    let uuid = uuid::Uuid::from_u128(0x1122_3344);
    let mut w = Writer::default();
    w.var_i32(42);
    w.uuid(uuid);
    FIXTURE.write_mob_type_id(&mut w, FIXTURE.creeper_id);
    w.f64(10.5);
    w.f64(64.0);
    w.f64(-3.0);
    w.i8(0);
    w.i8(0);
    w.i8(0);
    w.i16(0);
    w.i16(0);
    w.i16(0);
    let payload = w.into_vec();
    match dispatch(play::clientbound::SPAWN_ENTITY_LIVING, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntitySpawned {
                entity_id,
                uuid: Some(got),
                entity_type,
                pos,
                ..
            }),
        ] => {
            assert_eq!(*entity_id, 42);
            assert_eq!(*got, uuid);
            assert_eq!(entity_type.to_string(), "minecraft:creeper");
            assert!((pos.x - 10.5).abs() < EPS);
            assert!((pos.z - -3.0).abs() < EPS);
        }
        other => panic!("expected EntitySpawned mob, got {other:?}"),
    }
}

#[test]
fn spawn_object_resolves_type_and_carries_uuid() {
    let uuid = uuid::Uuid::from_u128(0xABCD);
    let mut w = Writer::default();
    w.var_i32(1000);
    w.uuid(uuid);
    FIXTURE.write_object_type_id(&mut w, FIXTURE.arrow_id);
    w.f64(5.0);
    w.f64(65.0);
    w.f64(-8.0);
    w.i8(0);
    w.i8(0);
    w.i32(1); // object_data
    w.i16(8000); // vx = 1.0
    w.i16(0);
    w.i16(0);
    let payload = w.into_vec();
    match dispatch(play::clientbound::SPAWN_ENTITY, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntitySpawned {
                entity_id,
                uuid: Some(got),
                entity_type,
                velocity: Some(vel),
                ..
            }),
        ] => {
            assert_eq!(*entity_id, 1000);
            assert_eq!(*got, uuid);
            assert_eq!(entity_type.to_string(), "minecraft:arrow");
            assert!((vel.x - 1.0).abs() < EPS);
        }
        other => panic!("expected EntitySpawned object, got {other:?}"),
    }
}

#[test]
fn spawn_object_stationary_omits_velocity() {
    let mut w = Writer::default();
    w.var_i32(1001);
    w.uuid(uuid::Uuid::nil());
    FIXTURE.write_object_type_id(&mut w, FIXTURE.boat_id);
    w.f64(0.0);
    w.f64(0.0);
    w.f64(0.0);
    w.i8(0);
    w.i8(0);
    w.i32(0);
    w.i16(0);
    w.i16(0);
    w.i16(0);
    let payload = w.into_vec();
    match dispatch(play::clientbound::SPAWN_ENTITY, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntitySpawned {
                entity_type,
                velocity,
                ..
            }),
        ] => {
            assert_eq!(entity_type.to_string(), "minecraft:boat");
            assert!(velocity.is_none(), "all-zero velocity forwards as None");
        }
        other => panic!("expected EntitySpawned object, got {other:?}"),
    }
}

#[test]
fn named_entity_spawn_resolves_player_and_uuid() {
    let uuid = uuid::Uuid::from_u128(0xDEAD_BEEF);
    let mut w = Writer::default();
    w.var_i32(2000);
    w.uuid(uuid);
    w.f64(1.0);
    w.f64(64.0);
    w.f64(2.0);
    w.i8(0);
    w.i8(0);
    let payload = w.into_vec();
    match dispatch(play::clientbound::NAMED_ENTITY_SPAWN, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntitySpawned {
                entity_id,
                uuid: Some(got),
                entity_type,
                pos,
                ..
            }),
        ] => {
            assert_eq!(*entity_id, 2000);
            assert_eq!(*got, uuid);
            assert_eq!(entity_type.to_string(), "minecraft:player");
            assert!((pos.y - 64.0).abs() < EPS);
        }
        other => panic!("expected EntitySpawned player, got {other:?}"),
    }
}

#[test]
fn spawn_object_unknown_type_is_a_clean_error() {
    let mut w = Writer::default();
    w.var_i32(1);
    w.uuid(uuid::Uuid::nil());
    FIXTURE.write_object_type_id(&mut w, 120); // absent from the 1.16 entity table (max id 107)
    w.f64(0.0);
    w.f64(0.0);
    w.f64(0.0);
    w.i8(0);
    w.i8(0);
    w.i32(0);
    w.i16(0);
    w.i16(0);
    w.i16(0);
    let payload = w.into_vec();
    let err = try_dispatch(play::clientbound::SPAWN_ENTITY, &payload)
        .expect_err("unknown object type must error");
    assert!(
        format!("{err:?}").contains("120"),
        "error should name the id: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Destroy + truncation
// ---------------------------------------------------------------------------

#[test]
fn entity_destroy_dispatches_removal_of_all_ids() {
    let mut w = Writer::default();
    w.var_i32(3);
    w.var_i32(1);
    w.var_i32(2);
    w.var_i32(300);
    let payload = w.into_vec();
    match dispatch(play::clientbound::ENTITY_DESTROY, &payload).as_slice() {
        [Directive::Emit(ClientEvent::EntityRemoved { entity_ids })] => {
            assert_eq!(entity_ids, &vec![1, 2, 300]);
        }
        other => panic!("expected EntityRemoved, got {other:?}"),
    }
}

#[test]
fn entity_destroy_truncated_and_trailing_are_clean_errors() {
    let mut truncated = Writer::default();
    truncated.var_i32(3);
    truncated.var_i32(1);
    truncated.var_i32(2);
    assert!(try_dispatch(play::clientbound::ENTITY_DESTROY, &truncated.into_vec()).is_err());

    let mut trailing = Writer::default();
    trailing.var_i32(1);
    trailing.var_i32(7);
    trailing.i8(0);
    assert!(try_dispatch(play::clientbound::ENTITY_DESTROY, &trailing.into_vec()).is_err());
}

#[test]
fn truncated_movement_packets_error_not_panic() {
    for id in [
        play::clientbound::REL_ENTITY_MOVE,
        play::clientbound::ENTITY_LOOK,
        play::clientbound::ENTITY_MOVE_LOOK,
        play::clientbound::ENTITY_TELEPORT,
        play::clientbound::ENTITY_VELOCITY,
        play::clientbound::SPAWN_ENTITY,
        play::clientbound::SPAWN_ENTITY_LIVING,
        play::clientbound::NAMED_ENTITY_SPAWN,
    ] {
        assert!(
            try_dispatch(id, &[0x01]).is_err(),
            "packet id {id} must reject a 1-byte truncated payload"
        );
    }
}
