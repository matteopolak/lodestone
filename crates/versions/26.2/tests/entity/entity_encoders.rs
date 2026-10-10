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
    ClientEvent, ConnectionState, Directive, ResourceKey, Rotation, Vec3, VersionAdapter,
};
use lodestone_server::{EntitySnapshot, ServerDirective, ServerProtocol};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::V770ServerProtocol;
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
        on_ground: false,
        metadata: Vec::new(),
        object_data: 0,
        equipment: Vec::new(),
        leash_link: None,
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
fn encode_add_entity_falls_back_to_the_explicit_acacia_boat_type_for_an_unknown_key() {
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
    // The protocol's established recovery behavior is preserved, but the
    // writer reaches it through `EntityType::AcaciaBoat`, not a raw `0`.
    assert_eq!(entity_type.to_string(), "minecraft:acacia_boat");
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
    let ClientEvent::EntityTeleported {
        entity_id,
        pos,
        rotation,
        flags,
        velocity,
        on_ground,
        ..
    } = &events[0]
    else {
        panic!("expected EntityTeleported, got {:?}", events[0]);
    };
    assert_eq!(*entity_id, 7);
    assert!((pos.x - 12.5).abs() < 1e-9);
    assert!((pos.y - 64.0).abs() < 1e-9);
    assert!((pos.z - (-3.25)).abs() < 1e-9);
    assert!((rotation.yaw - (-90.0)).abs() < 1e-4, "yaw: {}", rotation.yaw);
    assert!((rotation.pitch - 5.0).abs() < 1e-4, "pitch: {}", rotation.pitch);
    assert!(!flags.relative_x);
    assert!(!flags.relative_y);
    assert!(!flags.relative_z);
    assert!(!flags.relative_yaw);
    assert!(!flags.relative_pitch);
    assert_eq!(velocity.delta, snapshot.velocity);
    assert!(!velocity.relative_x);
    assert!(!velocity.relative_y);
    assert!(!velocity.relative_z);
    assert!(!velocity.rotate_delta);
    assert_eq!(*on_ground, snapshot.on_ground);

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
fn entity_corrections_preserve_velocity_and_ground_on_the_wire() {
    let proto = V770ServerProtocol;
    let mut snapshot = zombie_snapshot(7, Uuid::nil());
    snapshot.velocity = Vec3::new(0.125, -0.25, 0.375);
    for grounded in [false, true] {
        snapshot.on_ground = grounded;
        let directives = proto.encode_entity_update(None, &snapshot);
        let ServerDirective::Send { packet_id, payload } = &directives[0] else {
            panic!("expected entity correction");
        };
        assert_eq!(*packet_id, play::clientbound::TELEPORT_ENTITY);
        assert_eq!(payload.len(), 62);
        assert_eq!(&payload[25..33], &0.125_f64.to_be_bytes());
        assert_eq!(&payload[33..41], &(-0.25_f64).to_be_bytes());
        assert_eq!(&payload[41..49], &0.375_f64.to_be_bytes());
        assert_eq!(payload[61], u8::from(grounded));
        let events = decode_events(*packet_id, payload);
        let ClientEvent::EntityTeleported { velocity, on_ground, .. } = &events[0] else {
            panic!("expected decoded entity correction");
        };
        assert_eq!(velocity.delta, snapshot.velocity);
        assert_eq!(*on_ground, grounded);
    }
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

/// `encode_set_entity_link` round-tripped through the real adapter's own
/// `SET_ENTITY_LINK` decode arm — the server-encode half meeting
/// the client-decode half this crate already had. Pairwise-distinct ids
/// (11 and 4, not 1 and 1 or 1 and 4): a transposition of `source_id` and
/// `target_id` inside the encoder would otherwise round-trip byte-perfectly
/// and only show up as the client leashing the wrong entity.
#[test]
fn encode_set_entity_link_round_trips_an_attach_through_the_real_adapter() {
    let proto = V770ServerProtocol;
    let ServerDirective::Send { packet_id, payload } = proto.encode_set_entity_link(11, Some(4))
    else {
        panic!("expected a Send directive");
    };
    assert_eq!(packet_id, play::clientbound::SET_ENTITY_LINK);

    let events = decode_events(packet_id, &payload);
    assert_eq!(events.len(), 1);
    let ClientEvent::EntityLeashed {
        entity_id,
        holder_id,
    } = &events[0]
    else {
        panic!("expected EntityLeashed, got {:?}", events[0]);
    };
    assert_eq!(*entity_id, 11);
    assert_eq!(*holder_id, Some(4));
}

/// The detach shape: `None` must encode to vanilla's own `destId == 0`
/// sentinel and decode back to `None`, not to `Some(0)`.
#[test]
fn encode_set_entity_link_round_trips_a_detach_through_the_real_adapter() {
    let proto = V770ServerProtocol;
    let ServerDirective::Send { packet_id, payload } = proto.encode_set_entity_link(11, None)
    else {
        panic!("expected a Send directive");
    };

    let events = decode_events(packet_id, &payload);
    assert_eq!(events.len(), 1);
    let ClientEvent::EntityLeashed {
        entity_id,
        holder_id,
    } = &events[0]
    else {
        panic!("expected EntityLeashed, got {:?}", events[0]);
    };
    assert_eq!(*entity_id, 11);
    assert_eq!(*holder_id, None);
}

/// `SET_EQUIPMENT` round-tripped through the real adapter: two slots with a
/// distinct item each (so a swapped slot ordinal shows), an explicit cleared
/// slot, and the ominous banner's eight pattern layers in order.
#[test]
fn encode_set_equipment_round_trips_slots_items_and_banner_layers_through_the_real_adapter() {
    use lodestone_model::item::BannerPatternLayer;
    use lodestone_model::{EntityEquipment, EquipmentSlot, ItemStack};
    let key = |s: &str| s.parse::<ResourceKey>().expect("valid key");
    let layer = |pattern: &str, color: &str| BannerPatternLayer {
        pattern_asset_id: pattern.to_owned(),
        color: color.to_owned(),
    };
    let mut banner = ItemStack::new(key("minecraft:white_banner"), 1);
    banner.components.banner_patterns = vec![
        layer("rhombus", "cyan"),
        layer("stripe_bottom", "light_gray"),
        layer("stripe_center", "gray"),
        layer("border", "light_gray"),
        layer("stripe_middle", "black"),
        layer("half_horizontal", "light_gray"),
        layer("circle", "light_gray"),
        layer("border", "black"),
    ];
    let sent = vec![
        EntityEquipment { slot: EquipmentSlot::MainHand, item: Some(ItemStack::new(key("minecraft:iron_sword"), 1)) },
        EntityEquipment { slot: EquipmentSlot::OffHand, item: None },
        EntityEquipment { slot: EquipmentSlot::Head, item: Some(banner.clone()) },
    ];
    let ServerDirective::Send { packet_id, payload } = V770ServerProtocol.encode_set_equipment(77, &sent) else {
        panic!("expected a Send directive");
    };
    assert_eq!(packet_id, play::clientbound::SET_EQUIPMENT);
    let events = decode_events(packet_id, &payload);
    let [ClientEvent::EntityEquipmentUpdated { entity_id, equipment }] = events.as_slice() else {
        panic!("expected one EntityEquipmentUpdated, got {events:?}");
    };
    assert_eq!(*entity_id, 77);
    assert_eq!(equipment.len(), 3);
    assert_eq!(equipment[0].slot, EquipmentSlot::MainHand);
    assert_eq!(equipment[0].item.as_ref().map(|i| i.item.clone()), Some(key("minecraft:iron_sword")));
    assert_eq!(equipment[1].slot, EquipmentSlot::OffHand);
    assert_eq!(equipment[1].item, None);
    assert_eq!(equipment[2].slot, EquipmentSlot::Head);
    let head = equipment[2].item.as_ref().expect("banner on the head");
    assert_eq!(head.item, key("minecraft:white_banner"));
    assert_eq!(head.components.banner_patterns, banner.components.banner_patterns);
}
