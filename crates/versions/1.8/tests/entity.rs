//! Hermetic dispatch tests for protocol 47 entity movement, velocity, teleport,
//! spawn and destroy packets.
//!
//! These are *seam* tests, not decoder tests: every case drives the packet
//! through the real [`VersionAdapter::handle_packet`] and asserts on the
//! resulting [`ClientEvent`], so a decoder that is correct but never dispatched
//! (the "correct but never called" trap) fails here. The scaling assertions are
//! deliberately anti-vacuous — a wrong fixed-point divisor (e.g. the 1.9
//! `1/4096` delta scale applied to 1.8's `1/32` bytes) is off by ~128×, far
//! outside any float tolerance, so it cannot slip through.

use lodestone_core::{Ctx, Encode, Reader, Writer};
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, EntityMovement, EntityVariant, VersionAdapter,
};
use lodestone_v1_8::V47Adapter;
use lodestone_v1_8::packet_ids::play;
use lodestone_v1_8::packets::entity::{
    EntityLook, EntityMetadataPacket, EntityMoveLook, EntityTeleport, EntityVelocityPacket,
    RelEntityMove, SpawnEntityLiving,
};
use lodestone_v1_8::packets::metadata::{EntityMetadata, MetadataEntry, MetadataValue};
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 47 };

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn dispatch(packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    let adapter = V47Adapter::new();
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle_packet")
}

fn try_dispatch(
    packet_id: i32,
    payload: &[u8],
) -> Result<Vec<Directive>, lodestone_model::AdapterError> {
    let adapter = V47Adapter::new();
    adapter.handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
}

fn empty_metadata() -> EntityMetadata {
    // The 1.8 metadata list is terminated by a single 0x7F byte.
    let mut reader = Reader::new(&[0x7Fu8]);
    <EntityMetadata as lodestone_core::Decode>::decode(&mut reader, CTX).expect("empty metadata")
}

/// Dispatches through an adapter the caller supplies, rather than a fresh
/// one — needed to exercise `spawn_entity_living` -> `entity_metadata`
/// sequencing, where the second packet's interpretation depends on state the
/// first one recorded.
fn dispatch_with(adapter: &V47Adapter, packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle_packet")
}

const EPS: f64 = 1e-9;

// ---------------------------------------------------------------------------
// Relative move / look / move+look
// ---------------------------------------------------------------------------

#[test]
fn rel_entity_move_dispatches_relative_movement_in_thirty_seconds_of_a_block() {
    // dx = 32 signed-byte units = exactly 1 block (1/32 scale).
    let payload = encode(&RelEntityMove {
        entity_id: 7,
        dx: 32,
        dy: -16,
        dz: 8,
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
    // yaw byte 64 = 90°, pitch byte -64 = -90°.
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
                on_ground,
            }),
        ] => {
            assert_eq!(*entity_id, 3);
            assert_eq!(*delta, lodestone_model::Vec3::new(0.0, 0.0, 0.0));
            assert!((f64::from(rot.yaw) - 90.0).abs() < 1e-4);
            assert!((f64::from(rot.pitch) - -90.0).abs() < 1e-4);
            assert!(!*on_ground);
        }
        other => panic!("expected rotation EntityMoved, got {other:?}"),
    }
}

#[test]
fn entity_move_look_dispatches_delta_and_rotation() {
    let payload = encode(&EntityMoveLook {
        entity_id: 11,
        dx: -32,
        dy: 0,
        dz: 64,
        yaw: 0,
        pitch: 0,
        on_ground: true,
    });
    match dispatch(play::clientbound::ENTITY_MOVE_LOOK, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntityMoved {
                entity_id,
                movement: EntityMovement::Relative(delta),
                rotation: Some(_),
                on_ground,
            }),
        ] => {
            assert_eq!(*entity_id, 11);
            assert!((delta.x - -1.0).abs() < EPS);
            assert!((delta.z - 2.0).abs() < EPS);
            assert!(*on_ground);
        }
        other => panic!("expected move+look EntityMoved, got {other:?}"),
    }
}

#[test]
fn entity_teleport_dispatches_absolute_position() {
    // 1.8 teleport is fixed-point i32 (block × 32): 64 blocks = 2048.
    let payload = encode(&EntityTeleport {
        entity_id: 99,
        x: 2048,
        y: 32 * 70,
        z: -64,
        yaw: 0,
        pitch: 0,
        on_ground: false,
    });
    match dispatch(play::clientbound::ENTITY_TELEPORT, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntityMoved {
                entity_id,
                movement: EntityMovement::Absolute(pos),
                ..
            }),
        ] => {
            assert_eq!(*entity_id, 99);
            assert!(
                (pos.x - 64.0).abs() < EPS,
                "x should be 64.0, got {}",
                pos.x
            );
            assert!((pos.y - 70.0).abs() < EPS);
            assert!((pos.z - -2.0).abs() < EPS);
        }
        other => panic!("expected absolute EntityMoved, got {other:?}"),
    }
}

#[test]
fn entity_velocity_dispatches_in_eight_thousandths() {
    // 8000 units = exactly 1 block/tick.
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
            assert!(
                (velocity.x - 1.0).abs() < EPS,
                "vx should be 1.0, got {}",
                velocity.x
            );
            assert!((velocity.y - -0.5).abs() < EPS);
            assert!((velocity.z).abs() < EPS);
        }
        other => panic!("expected EntityVelocity, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Spawns
// ---------------------------------------------------------------------------

#[test]
fn spawn_entity_living_resolves_mob_type_and_scales_coords() {
    let payload = encode(&SpawnEntityLiving {
        entity_id: 42,
        kind: 90, // pig
        x: 32 * 10,
        y: 32 * 64,
        z: -32 * 3,
        yaw: 0,
        pitch: 0,
        head_pitch: 0,
        velocity_x: 0,
        velocity_y: 0,
        velocity_z: 0,
        metadata: empty_metadata(),
    });
    match dispatch(play::clientbound::SPAWN_ENTITY_LIVING, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntitySpawned {
                entity_id,
                uuid,
                entity_type,
                pos,
                ..
            }),
        ] => {
            assert_eq!(*entity_id, 42);
            assert!(uuid.is_none(), "1.8 mobs carry no UUID");
            assert_eq!(entity_type.to_string(), "minecraft:pig");
            assert!((pos.x - 10.0).abs() < EPS);
            assert!((pos.y - 64.0).abs() < EPS);
            assert!((pos.z - -3.0).abs() < EPS);
        }
        other => panic!("expected EntitySpawned mob, got {other:?}"),
    }
}

#[test]
fn spawn_entity_living_unknown_type_is_a_clean_error() {
    // id 200 is absent from the 1.8 mob table.
    let payload = encode(&SpawnEntityLiving {
        entity_id: 1,
        kind: 200,
        x: 0,
        y: 0,
        z: 0,
        yaw: 0,
        pitch: 0,
        head_pitch: 0,
        velocity_x: 0,
        velocity_y: 0,
        velocity_z: 0,
        metadata: empty_metadata(),
    });
    let err = try_dispatch(play::clientbound::SPAWN_ENTITY_LIVING, &payload)
        .expect_err("unknown mob type must error, not silently drop");
    assert!(
        format!("{err:?}").contains("200"),
        "error should name the bad id: {err:?}"
    );
}

#[test]
fn spawn_object_with_data_carries_velocity() {
    // Manually frame a 1.8 spawn_entity (object) with non-zero object_data so
    // the switched velocity is present. type 60 = arrow.
    let mut w = Writer::default();
    w.var_i32(1000);
    w.i8(60);
    w.i32(32 * 5); // x = 5
    w.i32(32 * 65); // y = 65
    w.i32(32 * -8); // z = -8
    w.i8(0);
    w.i8(0);
    w.i32(1); // object_data != 0 → velocity present
    w.i16(8000); // vx = 1.0
    w.i16(0);
    w.i16(0);
    let payload = w.into_vec();
    match dispatch(play::clientbound::SPAWN_ENTITY, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntitySpawned {
                entity_id,
                entity_type,
                pos,
                velocity: Some(vel),
                ..
            }),
        ] => {
            assert_eq!(*entity_id, 1000);
            assert_eq!(entity_type.to_string(), "minecraft:arrow");
            assert!((pos.x - 5.0).abs() < EPS);
            assert!((vel.x - 1.0).abs() < EPS);
        }
        other => panic!("expected EntitySpawned object with velocity, got {other:?}"),
    }
}

#[test]
fn spawn_object_without_data_omits_velocity() {
    let mut w = Writer::default();
    w.var_i32(1001);
    w.i8(1); // boat
    w.i32(0);
    w.i32(0);
    w.i32(0);
    w.i8(0);
    w.i8(0);
    w.i32(0); // object_data == 0 → no velocity, no trailing bytes
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
            assert!(velocity.is_none(), "no velocity when object_data == 0");
        }
        other => panic!("expected EntitySpawned object without velocity, got {other:?}"),
    }
}

#[test]
fn named_entity_spawn_resolves_player_and_uuid() {
    let uuid = uuid::Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff);
    let mut w = Writer::default();
    w.var_i32(2000);
    w.uuid(uuid);
    w.i32(32); // x = 1
    w.i32(32 * 64); // y = 64
    w.i32(32 * 2); // z = 2
    w.i8(0);
    w.i8(0);
    w.i16(0); // current_item
    w.bytes(&[0x7F]); // trailing (unread) metadata terminator
    let payload = w.into_vec();
    match dispatch(play::clientbound::NAMED_ENTITY_SPAWN, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntitySpawned {
                entity_id,
                uuid: Some(got_uuid),
                entity_type,
                pos,
                ..
            }),
        ] => {
            assert_eq!(*entity_id, 2000);
            assert_eq!(*got_uuid, uuid);
            assert_eq!(entity_type.to_string(), "minecraft:player");
            assert!((pos.x - 1.0).abs() < EPS);
            assert!((pos.y - 64.0).abs() < EPS);
        }
        other => panic!("expected EntitySpawned player, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Entity metadata — the DataWatcher table, wired end to end.
// ---------------------------------------------------------------------------

fn sheep_metadata(color: u8, sheared: bool) -> EntityMetadata {
    let mut byte = color & 0x0F;
    if sheared {
        byte |= 0x10;
    }
    EntityMetadata(vec![MetadataEntry {
        key: 16,
        value: MetadataValue::Byte(byte as i8),
    }])
}

#[test]
fn spawn_entity_living_dispatches_metadata_alongside_the_spawn_event() {
    // Pairwise-distinct sheep colour/sheared so a transposition of the two
    // bit ranges cannot survive: colour 11 (not 0/1), sheared true.
    let payload = encode(&SpawnEntityLiving {
        entity_id: 7,
        kind: 91, // sheep
        x: 0,
        y: 0,
        z: 0,
        yaw: 0,
        pitch: 0,
        head_pitch: 0,
        velocity_x: 0,
        velocity_y: 0,
        velocity_z: 0,
        metadata: sheep_metadata(11, true),
    });
    match dispatch(play::clientbound::SPAWN_ENTITY_LIVING, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntitySpawned { entity_id, .. }),
            Directive::Emit(ClientEvent::EntityMetadataUpdated {
                entity_id: meta_id,
                metadata,
            }),
        ] => {
            assert_eq!(*entity_id, 7);
            assert_eq!(*meta_id, 7);
            assert_eq!(
                metadata.variant,
                Some(EntityVariant::Dyed {
                    color: 11,
                    sheared: true
                })
            );
        }
        other => panic!("expected EntitySpawned then EntityMetadataUpdated, got {other:?}"),
    }
}

#[test]
fn standalone_entity_metadata_uses_the_type_recorded_at_spawn() {
    let adapter = V47Adapter::new();

    let spawn_payload = encode(&SpawnEntityLiving {
        entity_id: 55,
        kind: 91, // sheep
        x: 0,
        y: 0,
        z: 0,
        yaw: 0,
        pitch: 0,
        head_pitch: 0,
        velocity_x: 0,
        velocity_y: 0,
        velocity_z: 0,
        metadata: sheep_metadata(3, false),
    });
    dispatch_with(&adapter, play::clientbound::SPAWN_ENTITY_LIVING, &spawn_payload);

    // A later incremental update re-dyes and shears the same sheep. If the
    // adapter forgot (or never recorded) that entity 55 is a sheep, this
    // would fall back to the universal-base-only fold and `variant` would
    // stay `None` — the exact index-collision trap the metadata module's
    // docs describe.
    let update_payload = encode(&EntityMetadataPacket {
        entity_id: 55,
        metadata: sheep_metadata(9, true),
    });
    match dispatch_with(&adapter, play::clientbound::ENTITY_METADATA, &update_payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntityMetadataUpdated { entity_id, metadata }),
        ] => {
            assert_eq!(*entity_id, 55);
            assert_eq!(
                metadata.variant,
                Some(EntityVariant::Dyed {
                    color: 9,
                    sheared: true
                })
            );
        }
        other => panic!("expected EntityMetadataUpdated with sheep variant, got {other:?}"),
    }
}

#[test]
fn standalone_entity_metadata_for_an_untracked_id_still_folds_the_universal_base() {
    // No prior spawn recorded entity 999's type. The sheep-shaped wool byte
    // at index 16 must NOT be read as a variant (that would require knowing
    // this is a sheep) — but the universal on-fire flag at index 0 is always
    // safe to interpret regardless of class.
    let adapter = V47Adapter::new();
    let mut md = sheep_metadata(11, true);
    md.0.push(MetadataEntry {
        key: 0,
        value: MetadataValue::Byte(0x01), // on fire
    });
    let payload = encode(&EntityMetadataPacket {
        entity_id: 999,
        metadata: md,
    });
    match dispatch_with(&adapter, play::clientbound::ENTITY_METADATA, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::EntityMetadataUpdated { entity_id, metadata }),
        ] => {
            assert_eq!(*entity_id, 999);
            assert_eq!(metadata.flags, Some(0x01));
            assert_eq!(
                metadata.variant, None,
                "an untracked id must not have its wool byte read as a variant"
            );
        }
        other => panic!("expected EntityMetadataUpdated with only base fields, got {other:?}"),
    }
}

#[test]
fn entity_destroy_forgets_the_recorded_type_so_a_stale_id_cannot_leak_state() {
    let adapter = V47Adapter::new();

    let spawn_payload = encode(&SpawnEntityLiving {
        entity_id: 3,
        kind: 91, // sheep
        x: 0,
        y: 0,
        z: 0,
        yaw: 0,
        pitch: 0,
        head_pitch: 0,
        velocity_x: 0,
        velocity_y: 0,
        velocity_z: 0,
        metadata: sheep_metadata(0, false),
    });
    dispatch_with(&adapter, play::clientbound::SPAWN_ENTITY_LIVING, &spawn_payload);

    let mut w = Writer::default();
    w.var_i32(1); // count
    w.var_i32(3); // the sheep's id
    dispatch_with(&adapter, play::clientbound::ENTITY_DESTROY, &w.into_vec());

    // A later `entity_metadata` for the same numeric id (e.g. reused by the
    // server for an unrelated entity) must not still be folded as a sheep.
    let update_payload = encode(&EntityMetadataPacket {
        entity_id: 3,
        metadata: sheep_metadata(5, true),
    });
    match dispatch_with(&adapter, play::clientbound::ENTITY_METADATA, &update_payload).as_slice() {
        [] => {}
        [
            Directive::Emit(ClientEvent::EntityMetadataUpdated { metadata, .. }),
        ] => {
            assert_eq!(
                metadata.variant, None,
                "id 3 was destroyed; its old sheep type must not be reused"
            );
        }
        other => panic!("unexpected directives after destroy: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Destroy (the inline varint-list workaround)
// ---------------------------------------------------------------------------

#[test]
fn entity_destroy_dispatches_removal_of_all_ids() {
    let mut w = Writer::default();
    w.var_i32(3); // count
    w.var_i32(1);
    w.var_i32(2);
    w.var_i32(300); // multi-byte varint id
    let payload = w.into_vec();
    match dispatch(play::clientbound::ENTITY_DESTROY, &payload).as_slice() {
        [Directive::Emit(ClientEvent::EntityRemoved { entity_ids })] => {
            assert_eq!(entity_ids, &vec![1, 2, 300]);
        }
        other => panic!("expected EntityRemoved, got {other:?}"),
    }
}

#[test]
fn entity_destroy_truncated_count_is_a_clean_error() {
    // Count claims 3 ids but only 2 follow.
    let mut w = Writer::default();
    w.var_i32(3);
    w.var_i32(1);
    w.var_i32(2);
    let payload = w.into_vec();
    assert!(
        try_dispatch(play::clientbound::ENTITY_DESTROY, &payload).is_err(),
        "a truncated id list must error rather than under-report"
    );
}

#[test]
fn entity_destroy_trailing_bytes_is_a_clean_error() {
    // The `ensure_empty` guard makes "0 trailing bytes" a real claim: prove it
    // fires when an extra byte is appended.
    let mut w = Writer::default();
    w.var_i32(1);
    w.var_i32(7);
    w.i8(0); // stray trailing byte
    let payload = w.into_vec();
    assert!(
        try_dispatch(play::clientbound::ENTITY_DESTROY, &payload).is_err(),
        "trailing bytes after the id list must be rejected"
    );
}

// ---------------------------------------------------------------------------
// Truncation across the movement family
// ---------------------------------------------------------------------------

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
