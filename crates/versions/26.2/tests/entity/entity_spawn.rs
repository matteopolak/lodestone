//! Hermetic tests for protocol 776 `add_entity` dispatch.
//!
//! `add_entity`'s wire layout (`ClientboundAddEntityPacket`) is VarInt entity
//! id, UUID, VarInt entity-type registry id, position `f64`×3, a low-precision
//! velocity, three signed-byte angles (pitch, yaw, head yaw), and a VarInt data
//! field. Head yaw travels separately from body yaw and is surfaced through the
//! same `EntityHeadRotation` outlet `rotate_head` uses, so a spawn must emit
//! both a spawn event and a head-rotation event — losing either one strands the
//! renderer with a wrong-looking mob.

use lodestone_model::{ClientEvent, ConnectionState, Directive, Rotation, Vec3, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use lodestone_world::World;
use uuid::Uuid;

fn handle(adapter: &V770Adapter, packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle packet")
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

/// Independent angle unpacker (`vanilla's own mth's own unpack degrees`): a signed byte over a
/// 256-step circle.
fn unpack_degrees(packed: i8) -> f32 {
    f32::from(packed) * 360.0 / 256.0
}

/// Builds an `add_entity` payload. `head_yaw`/`yaw`/`pitch` are raw signed
/// angle bytes; velocity is the single-byte zero-vector encoding of `LpVec3`.
fn add_entity_bytes(
    entity_id: i32,
    uuid: Uuid,
    type_id: i32,
    pos: (f64, f64, f64),
    pitch: i8,
    yaw: i8,
    head_yaw: i8,
) -> Vec<u8> {
    let mut bytes = var_i32(entity_id);
    bytes.extend_from_slice(&uuid.as_u128().to_be_bytes());
    bytes.extend_from_slice(&var_i32(type_id));
    bytes.extend_from_slice(&pos.0.to_be_bytes());
    bytes.extend_from_slice(&pos.1.to_be_bytes());
    bytes.extend_from_slice(&pos.2.to_be_bytes());
    bytes.push(0x00); // LpVec3 zero-vector sentinel
    bytes.push(pitch as u8);
    bytes.push(yaw as u8);
    bytes.push(head_yaw as u8);
    bytes.extend_from_slice(&var_i32(0)); // data
    bytes
}

/// [`add_entity_bytes`] with a non-zero **Object Data** field — the trailing
/// VarInt whose meaning is per-type, and which a falling block carries its block
/// state id in.
fn add_entity_bytes_with_data(
    entity_id: i32,
    uuid: Uuid,
    type_id: i32,
    pos: (f64, f64, f64),
    data: i32,
) -> Vec<u8> {
    let mut bytes = add_entity_bytes(entity_id, uuid, type_id, pos, 0, 0, 0);
    // Strip the zero data byte `add_entity_bytes` appended and write ours, so the
    // two helpers cannot disagree about the rest of the layout.
    bytes.truncate(bytes.len() - 1);
    bytes.extend_from_slice(&var_i32(data));
    bytes
}

/// The registry id of `minecraft:falling_block` for protocol 776, resolved
/// through the adapter's own type table by scanning for the one id that names it.
///
/// Derived rather than hardcoded: a literal here would be a second copy of a
/// generated table, and the one thing this test must not do is assume the id.
fn falling_block_type_id() -> i32 {
    (0..2000)
        .find(|id| {
            lodestone_data::entity_types::entity_type_name(*id)
                == Some("minecraft:falling_block")
        })
        .expect("protocol 776 has a `minecraft:falling_block` entity type")
}

/// A falling block's `ADD_ENTITY` yields its imitated block state as a third
/// directive, after the spawn and the head rotation.
///
/// **This is the only channel the state ever travels on.**
/// `vanilla's own falling block entity's own define synched data` registers `DATA_START_POS` and nothing
/// else, so no `set_entity_data` ever carries it: an adapter that discards the
/// Object Data field leaves every falling block drawn as whatever state id `0`
/// resolves to, with nothing logged anywhere.
///
/// The ordering is asserted, not incidental: a consumer keyed on the entity id
/// needs `EntitySpawned` first, and `lodestone_ecs`'s
/// `apply_falling_block_state` resolves the entity through `EntityIndex`, which
/// `apply_entity_spawn` populates in the same batch.
#[test]
fn a_falling_blocks_add_entity_carries_its_block_state() {
    let adapter = V770Adapter::new();
    let uuid = Uuid::from_u128(9);
    // An arbitrary non-zero, non-small state id, so neither `0` nor a one-byte
    // VarInt could produce it by accident.
    let state_id = 1234;
    let payload = add_entity_bytes_with_data(
        11,
        uuid,
        falling_block_type_id(),
        (3.5, 70.0, -7.5),
        state_id,
    );
    let directives = handle(&adapter, play::clientbound::ADD_ENTITY, &payload);
    assert_eq!(
        directives.last(),
        Some(&Directive::Emit(ClientEvent::FallingBlockState {
            entity_id: 11,
            block_state_id: 1234,
        })),
        "the Object Data field must reach a consumer, and last so the entity exists"
    );
    assert!(
        matches!(
            directives.first(),
            Some(Directive::Emit(ClientEvent::EntitySpawned { .. }))
        ),
        "the spawn must still come first"
    );
}

/// Control: an ordinary mob's `ADD_ENTITY` emits **no** `FallingBlockState`, even
/// with a non-zero Object Data field.
///
/// Without this the gate above is satisfied by an adapter that emits the event
/// for every spawn — which would be wrong for every other type that reads the
/// field (a display block, an item-frame rotation) and would claim "this is a
/// block state" about values that are not.
#[test]
fn an_ordinary_mob_with_object_data_emits_no_falling_block_state() {
    let adapter = V770Adapter::new();
    let payload =
        add_entity_bytes_with_data(12, Uuid::from_u128(1), 100, (0.0, 0.0, 0.0), 1234);
    let directives = handle(&adapter, play::clientbound::ADD_ENTITY, &payload);
    assert!(
        !directives.iter().any(|d| matches!(
            d,
            Directive::Emit(ClientEvent::FallingBlockState { .. })
        )),
        "a pig is not a falling block: {directives:?}"
    );
}

/// The registry id of `minecraft:fishing_bobber` for protocol 776, resolved the
/// same way [`falling_block_type_id`] is and for the same reason.
fn fishing_bobber_type_id() -> i32 {
    (0..2000)
        .find(|id| {
            lodestone_data::entity_types::entity_type_name(*id)
                == Some("minecraft:fishing_bobber")
        })
        .expect("protocol 776 has a `minecraft:fishing_bobber` entity type")
}

/// A fishing bobber's `ADD_ENTITY` yields the **caster's** entity id out of the
/// same Object Data field the falling block reads a block state from.
///
/// `vanilla's own fishing hook's own get add entity packet` writes
/// `owner == null ? this.getId() : owner.getId()` there, and — exactly like the
/// falling block's state — nothing else carries it:
/// `vanilla's own fishing hook's own define synched data` registers only `DATA_HOOKED_ENTITY` and
/// `DATA_BITING`. An adapter that discards the field leaves the client with a
/// bobber it cannot anchor a line to.
///
/// The owner id is picked **distinct from the bobber's own id** on purpose: the
/// two are adjacent VarInts of the same type in this decode, and a fixture that
/// used one value for both could not tell a correct decode from one that echoed
/// the entity id back.
#[test]
fn a_fishing_bobbers_add_entity_carries_its_owner_id() {
    let adapter = V770Adapter::new();
    let payload = add_entity_bytes_with_data(
        21,
        Uuid::from_u128(13),
        fishing_bobber_type_id(),
        (4.5, 62.0, -1.5),
        44,
    );
    let directives = handle(&adapter, play::clientbound::ADD_ENTITY, &payload);
    assert_eq!(
        directives.last(),
        Some(&Directive::Emit(ClientEvent::ProjectileOwner {
            entity_id: 21,
            owner_id: 44,
        })),
        "the Object Data field must reach a consumer, and last so the entity exists"
    );
    assert!(
        matches!(
            directives.first(),
            Some(Directive::Emit(ClientEvent::EntitySpawned { .. }))
        ),
        "the spawn must still come first"
    );
    // And it must not also claim to be a block state — the two readings of one
    // field must stay disjoint or a hook would spawn a falling block too.
    assert!(
        !directives.iter().any(|d| matches!(
            d,
            Directive::Emit(ClientEvent::FallingBlockState { .. })
        )),
        "a fishing bobber is not a falling block: {directives:?}"
    );
}

/// Control: an ordinary mob's `ADD_ENTITY` emits **no** `ProjectileOwner`, even
/// with a non-zero Object Data field.
///
/// The mirror of `an_ordinary_mob_with_object_data_emits_no_falling_block_state`
/// and load-bearing for the same reason: without it the gate above is satisfied
/// by an adapter that emits the event for every spawn, which would claim "this
/// is an owner id" about a falling block's state.
#[test]
fn an_ordinary_mob_with_object_data_emits_no_projectile_owner() {
    let adapter = V770Adapter::new();
    let payload =
        add_entity_bytes_with_data(22, Uuid::from_u128(2), 100, (0.0, 0.0, 0.0), 44);
    let directives = handle(&adapter, play::clientbound::ADD_ENTITY, &payload);
    assert!(
        !directives.iter().any(|d| matches!(
            d,
            Directive::Emit(ClientEvent::ProjectileOwner { .. })
        )),
        "a pig has no fishing line: {directives:?}"
    );
}

#[test]
fn add_entity_emits_spawn_and_head_rotation() {
    let adapter = V770Adapter::new();
    let uuid = Uuid::from_u128(42);
    let payload = add_entity_bytes(7, uuid, 100, (1.0, 64.0, -2.0), 10, 20, 64);
    let directives = handle(&adapter, play::clientbound::ADD_ENTITY, &payload);
    assert_eq!(
        directives,
        vec![
            Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: 7,
                uuid: Some(uuid),
                entity_type: "minecraft:pig".parse().unwrap(),
                pos: Vec3::new(1.0, 64.0, -2.0),
                rotation: Rotation::new(unpack_degrees(20), unpack_degrees(10)),
                velocity: Some(Vec3::new(0.0, 0.0, 0.0)),
            }),
            Directive::Emit(ClientEvent::EntityHeadRotation {
                entity_id: 7,
                head_yaw: unpack_degrees(64), // 64 * 360/256 = 90 degrees
            }),
        ]
    );
}

#[test]
fn add_entity_head_yaw_diverges_from_body_yaw() {
    // A mob looking sideways while walking forward: body yaw 0, head yaw 90 —
    // the two must not collapse into a single rotation.
    let adapter = V770Adapter::new();
    let uuid = Uuid::from_u128(1);
    let payload = add_entity_bytes(1, uuid, 100, (0.0, 0.0, 0.0), 0, 0, 64);
    let directives = handle(&adapter, play::clientbound::ADD_ENTITY, &payload);
    match directives.as_slice() {
        [
            Directive::Emit(ClientEvent::EntitySpawned { rotation, .. }),
            Directive::Emit(ClientEvent::EntityHeadRotation { head_yaw, .. }),
        ] => {
            assert_eq!(rotation.yaw, 0.0, "body yaw unaffected by head yaw");
            assert_eq!(*head_yaw, 90.0);
        }
        other => panic!("expected [EntitySpawned, EntityHeadRotation], got {other:?}"),
    }
}

#[test]
fn add_entity_rejects_unknown_entity_type() {
    let adapter = V770Adapter::new();
    let uuid = Uuid::from_u128(1);
    let payload = add_entity_bytes(1, uuid, 1_000_000, (0.0, 0.0, 0.0), 0, 0, 0);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::ADD_ENTITY,
        &payload,
    );
    assert!(
        result.is_err(),
        "an unknown entity-type id must be rejected"
    );
}

#[test]
fn add_entity_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let uuid = Uuid::from_u128(1);
    let mut payload = add_entity_bytes(1, uuid, 100, (0.0, 0.0, 0.0), 0, 0, 0);
    payload.push(0xFF);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::ADD_ENTITY,
        &payload,
    );
    assert!(result.is_err(), "a misaligned add_entity must be rejected");
}

#[test]
fn add_entity_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let uuid = Uuid::from_u128(1);
    let mut payload = add_entity_bytes(1, uuid, 100, (0.0, 0.0, 0.0), 0, 0, 0);
    payload.truncate(payload.len() - 1); // drop the data varint
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::ADD_ENTITY,
        &payload,
    );
    assert!(
        result.is_err(),
        "a truncated add_entity must be rejected, not panic"
    );
}
