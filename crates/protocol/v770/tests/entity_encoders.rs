//! Hermetic tests for `V770ServerProtocol`'s entity spawn/update/remove
//! encoders (`add_entity`, `teleport_entity` + `rotate_head`,
//! `remove_entities`).
//!
//! Unlike a bare `decode(encode(x)) == x` round trip on freshly-written code
//! (weak: both directions can share the same misunderstanding), these tests
//! decode through [`V770Adapter::handle_packet`] — the exact function real
//! `lodestone-client` connections use, already exercised against live-server
//! bytes elsewhere in this crate (`tests/live_chunk.rs`, `tests/join_flow.rs`).
//! A wrong field order or scale in the new encoder therefore surfaces as a
//! wrong (or failing) decode through *old, independently-verified* code, not
//! just self-consistency.

use lodestone_core::Nbt;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, EntityMovement, ResourceKey, Rotation, Vec3,
    VersionAdapter,
};
use lodestone_server::{EntitySnapshot, ServerDirective, ServerProtocol};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_v770::V770ServerProtocol;
use lodestone_world::{BiomePatch, ChunkPos, ColumnPatch, LightPatch, LoadedChunk, WorldSink};
use uuid::Uuid;

/// A [`WorldSink`] that ignores every terrain call — these tests only decode
/// entity packets, which never touch the world.
#[derive(Default)]
struct NullSink;

impl WorldSink for NullSink {
    fn load(&mut self, _pos: ChunkPos, _chunk: LoadedChunk) {}
    fn merge(&mut self, _pos: ChunkPos, _patch: ColumnPatch) {}
    fn set_block(&mut self, _x: i32, _y: i32, _z: i32, _state: u32) {}
    fn set_blocks(
        &mut self,
        _section_x: i32,
        _section_y: i32,
        _section_z: i32,
        _blocks: &[(u8, u8, u8, u32)],
    ) {
    }
    fn merge_light(&mut self, _pos: ChunkPos, _patch: LightPatch) {}
    fn merge_biomes(&mut self, _pos: ChunkPos, _patch: BiomePatch) {}
    fn unload(&mut self, _pos: ChunkPos) {}
    fn set_block_entity(&mut self, _x: i32, _y: i32, _z: i32, _type_id: u32, _nbt: Nbt) {}
    fn sync_block_entity(
        &mut self,
        _x: i32,
        _y: i32,
        _z: i32,
        _block_entity_type: Option<u32>,
    ) -> lodestone_world::BlockEntitySync {
        lodestone_world::BlockEntitySync::ChunkAbsent
    }
}

/// Decodes one clientbound packet through the real adapter, returning its
/// emitted [`ClientEvent`]s (panics on anything else, since these packets
/// only ever emit events).
fn decode_events(packet_id: i32, payload: &[u8]) -> Vec<ClientEvent> {
    let adapter = V770Adapter::default();
    let mut sink = NullSink;
    let directives = adapter
        .handle_packet(&mut sink, ConnectionState::Play, packet_id, payload)
        .expect("decodes");
    directives
        .into_iter()
        .map(|d| match d {
            Directive::Emit(event) => event,
            other => panic!("expected only Emit directives, got {other:?}"),
        })
        .collect()
}

fn zombie_snapshot(id: i32, uuid: Uuid) -> EntitySnapshot {
    EntitySnapshot {
        id,
        uuid,
        entity_type: ResourceKey::new("minecraft", "zombie").unwrap(),
        position: Vec3::new(12.5, 64.0, -3.25),
        rotation: Rotation::new(-90.0, 5.0),
        head_yaw: -60.0,
        velocity: Vec3::new(0.1, 0.0, -0.05),
    }
}

#[test]
fn encode_add_entity_round_trips_through_the_real_adapter() {
    let proto = V770ServerProtocol;
    let uuid = Uuid::new_v4();
    let snapshot = zombie_snapshot(42, uuid);

    let ServerDirective::Send { packet_id, payload } = proto.encode_add_entity(&snapshot) else {
        panic!("expected a Send directive");
    };
    assert_eq!(packet_id, play::clientbound::ADD_ENTITY);

    let events = decode_events(packet_id, &payload);
    assert_eq!(events.len(), 2, "add_entity should emit spawn + head rotation");

    let ClientEvent::EntitySpawned {
        entity_id,
        uuid: decoded_uuid,
        entity_type,
        pos,
        rotation,
        velocity,
    } = &events[0]
    else {
        panic!("expected EntitySpawned, got {:?}", events[0]);
    };
    assert_eq!(*entity_id, 42);
    assert_eq!(*decoded_uuid, Some(uuid));
    assert_eq!(entity_type.to_string(), "minecraft:zombie");
    assert!((pos.x - 12.5).abs() < 1e-9);
    assert!((pos.y - 64.0).abs() < 1e-9);
    assert!((pos.z - (-3.25)).abs() < 1e-9);
    // Angle bytes are 256-steps-per-circle, so tolerance is one step (~1.4°).
    assert!((rotation.yaw - (-90.0)).abs() < 1.5, "yaw: {}", rotation.yaw);
    assert!((rotation.pitch - 5.0).abs() < 1.5, "pitch: {}", rotation.pitch);
    let v = velocity.expect("velocity present");
    assert!((v.x - 0.1).abs() < 1e-3, "vx: {}", v.x);
    assert!((v.z - (-0.05)).abs() < 1e-3, "vz: {}", v.z);

    let ClientEvent::EntityHeadRotation { entity_id, head_yaw } = &events[1] else {
        panic!("expected EntityHeadRotation, got {:?}", events[1]);
    };
    assert_eq!(*entity_id, 42);
    assert!((head_yaw - (-60.0)).abs() < 1.5, "head_yaw: {head_yaw}");
}

#[test]
fn encode_add_entity_falls_back_to_id_zero_for_an_unknown_type() {
    let proto = V770ServerProtocol;
    let mut snapshot = zombie_snapshot(1, Uuid::new_v4());
    snapshot.entity_type = ResourceKey::new("minecraft", "definitely_not_a_real_entity").unwrap();

    let ServerDirective::Send { packet_id, payload } = proto.encode_add_entity(&snapshot) else {
        panic!("expected a Send directive");
    };
    // Must still decode cleanly (a valid, if wrong, entity type id) rather
    // than corrupt the stream.
    let events = decode_events(packet_id, &payload);
    let ClientEvent::EntitySpawned { entity_type, .. } = &events[0] else {
        panic!("expected EntitySpawned");
    };
    // Network id 0 in the generated table (not asserting the exact name here,
    // only that *some* valid entry decoded rather than an error).
    assert!(!entity_type.to_string().is_empty());
}

#[test]
fn encode_entity_update_round_trips_an_absolute_teleport_and_head_rotation() {
    let proto = V770ServerProtocol;
    let snapshot = zombie_snapshot(7, Uuid::new_v4());

    let directives = proto.encode_entity_update(None, &snapshot);
    assert_eq!(directives.len(), 2, "expected teleport + rotate_head");

    let ServerDirective::Send { packet_id, payload } = &directives[0] else {
        panic!("expected a Send directive");
    };
    assert_eq!(*packet_id, play::clientbound::TELEPORT_ENTITY);
    let events = decode_events(*packet_id, payload);
    let ClientEvent::EntityMoved {
        entity_id,
        movement,
        rotation,
        ..
    } = &events[0]
    else {
        panic!("expected EntityMoved, got {:?}", events[0]);
    };
    assert_eq!(*entity_id, 7);
    let EntityMovement::Absolute(pos) = movement else {
        panic!("expected an absolute movement, got {movement:?}");
    };
    assert!((pos.x - 12.5).abs() < 1e-9);
    assert!((pos.z - (-3.25)).abs() < 1e-9);
    // teleport_entity's yaw/pitch are full-precision f32, not signed bytes.
    let rotation = rotation.expect("rotation present");
    assert!((rotation.yaw - (-90.0)).abs() < 1e-4, "yaw: {}", rotation.yaw);
    assert!((rotation.pitch - 5.0).abs() < 1e-4, "pitch: {}", rotation.pitch);

    let ServerDirective::Send { packet_id, payload } = &directives[1] else {
        panic!("expected a Send directive");
    };
    assert_eq!(*packet_id, play::clientbound::ROTATE_HEAD);
    let events = decode_events(*packet_id, payload);
    let ClientEvent::EntityHeadRotation { entity_id, head_yaw } = &events[0] else {
        panic!("expected EntityHeadRotation, got {:?}", events[0]);
    };
    assert_eq!(*entity_id, 7);
    assert!((head_yaw - (-60.0)).abs() < 1.5, "head_yaw: {head_yaw}");
}

#[test]
fn encode_remove_entity_batches_every_id_into_one_packet() {
    let proto = V770ServerProtocol;
    let ServerDirective::Send { packet_id, payload } = proto.encode_remove_entity(&[3, 17, 256]) else {
        panic!("expected a Send directive");
    };
    assert_eq!(packet_id, play::clientbound::REMOVE_ENTITIES);

    let events = decode_events(packet_id, &payload);
    assert_eq!(events.len(), 1);
    let ClientEvent::EntityRemoved { entity_ids } = &events[0] else {
        panic!("expected EntityRemoved, got {:?}", events[0]);
    };
    assert_eq!(entity_ids, &vec![3, 17, 256]);
}
