//! Proves the fix for the "white sheep render with no wool" report: vanilla's
//! `SynchedEntityData` only ever puts a field on the wire when it differs from
//! the accessor's own default (`vanilla's own data item's own is set to default`,
//! `vanilla's own synched entity data's own get non default values` — the only source `ServerEntity`
//! ever draws a spawn's initial `set_entity_data` from), and
//! `vanilla's own sheep's own define synched data` defines `DATA_WOOL_ID` with default byte `0`
//! (`vanilla's own dye color's own by id(0) == WHITE`, sheared bit unset). A naturally white,
//! unsheared sheep therefore never puts index 18 on the wire, at spawn or
//! ever — not a decode bug, a wire *absence* — so `read_entity_metadata`
//! alone can never recover it (see
//! `the_raw_decoder_never_invents_a_default_only_spawn_does` below, which
//! pins that boundary).
//!
//! The fix lives in `crate::adapter::handle_add_entity`: it now emits an
//! extra `ClientEvent::EntityMetadataUpdated` synthesizing the vanilla
//! default, through the exact same channel a real `set_entity_data` uses, so
//! every downstream consumer needs no special case for "unreported".
//!
//! # Why the fixture must not carry an explicit wool byte
//!
//! `CLAUDE.md`'s "world" species of vacuous test: a fixture that carries the
//! wool byte with value `0` exercises the ordinary decode path, not the
//! absence this bug is about. This module's tests decode only an
//! `add_entity` packet — no `set_entity_data` payload is built or read at
//! all — so the wool field is genuinely and structurally never on the wire,
//! the same way a fully-default live sheep's spawn behaves.

use lodestone_core::Reader;
use lodestone_data::entity_types::entity_type_id;
use lodestone_model::{ClientEvent, ConnectionState, Directive, EntityVariant, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::packets::metadata::{MetadataClass, TrackedEntity, read_entity_metadata};
use lodestone_world::World;
use uuid::Uuid;

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

/// Builds a minimal `add_entity` payload. Angles and velocity are zeroed —
/// nothing in these tests reads them.
fn add_entity_bytes(entity_id: i32, uuid: Uuid, type_id: i32) -> Vec<u8> {
    let mut bytes = var_i32(entity_id);
    bytes.extend_from_slice(&uuid.as_u128().to_be_bytes());
    bytes.extend_from_slice(&var_i32(type_id));
    bytes.extend_from_slice(&0.0f64.to_be_bytes()); // x
    bytes.extend_from_slice(&64.0f64.to_be_bytes()); // y
    bytes.extend_from_slice(&0.0f64.to_be_bytes()); // z
    bytes.push(0x00); // LpVec3 zero-vector sentinel
    bytes.push(0); // pitch
    bytes.push(0); // yaw
    bytes.push(0); // head yaw
    bytes.extend_from_slice(&var_i32(0)); // data
    bytes
}

fn handle(adapter: &V770Adapter, payload: &[u8]) -> Vec<Directive> {
    adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::ADD_ENTITY,
            payload,
        )
        .expect("handle add_entity")
}

/// The positive case: a sheep `add_entity` with **no accompanying
/// `set_entity_data` at all** (exactly what vanilla sends for a fresh,
/// undyed, unsheared sheep) must still yield a wool variant — the vanilla
/// default — through the ordinary `EntityMetadataUpdated` channel.
#[test]
fn sheep_spawn_with_no_metadata_packet_still_reports_default_wool() {
    let adapter = V770Adapter::new();
    let sheep_type = entity_type_id("minecraft:sheep").expect("sheep is a real entity type");
    let payload = add_entity_bytes(9, Uuid::from_u128(9), sheep_type);
    let directives = handle(&adapter, &payload);

    let variant = directives.iter().find_map(|d| match d {
        Directive::Emit(ClientEvent::EntityMetadataUpdated { entity_id, metadata })
            if *entity_id == 9 =>
        {
            metadata.variant.clone()
        }
        _ => None,
    });

    assert_eq!(
        variant,
        Some(EntityVariant::Dyed {
            color: 0,
            sheared: false,
        }),
        "a sheep spawn with no metadata packet at all must still report the vanilla default \
         (white, unsheared) — the reported directives were: {directives:?}"
    );
}

/// The gating control: a **non**-sheep spawn (a pig, which the wool index
/// has no meaning for) must synthesize nothing. Without this, the fix could
/// be "always emit a Dyed variant on every spawn", which would misdraw every
/// other mob type as woolly.
#[test]
fn non_sheep_spawn_synthesizes_no_wool_variant() {
    let adapter = V770Adapter::new();
    let pig_type = entity_type_id("minecraft:pig").expect("pig is a real entity type");
    let payload = add_entity_bytes(11, Uuid::from_u128(11), pig_type);
    let directives = handle(&adapter, &payload);

    let has_metadata_event = directives
        .iter()
        .any(|d| matches!(d, Directive::Emit(ClientEvent::EntityMetadataUpdated { .. })));
    assert!(
        !has_metadata_event,
        "a pig spawn must synthesize no metadata event at all — got {directives:?}"
    );
}

/// Architectural boundary this fix relies on, pinned directly: the **raw**
/// metadata decoder must never invent a default on its own. If it did, an
/// incremental `set_entity_data` that updates an unrelated field (health,
/// pose, …) on an already-dyed sheep would silently reset the wool to white
/// on every such packet, because the decoder has no way to tell "the
/// spawn's first packet" apart from "the fortieth unrelated update" —
/// `EntityMetadataUpdate` is documented as cumulative, `None` meaning "this
/// packet did not mention it", not "reset to default". Only
/// `handle_add_entity`, which runs exactly once per spawn, may synthesize
/// the default.
///
/// An empty metadata list (a single `0xFF` terminator — the same bytes a
/// `set_entity_data` carries when nothing changed) decoded for a
/// `Sheep`-classed entity must report no variant at all.
#[test]
fn the_raw_decoder_never_invents_a_default_only_spawn_does() {
    let payload = [0xFFu8];
    let mut reader = Reader::new(&payload);
    let tracked = TrackedEntity {
        class: Some(MetadataClass::Sheep),
        living: false,
        mob: false,
    };
    let decoded = read_entity_metadata(&mut reader, tracked).expect("decodes the empty list");
    assert_eq!(
        decoded.metadata.variant, None,
        "the raw decoder must not synthesize a default — only handle_add_entity may, exactly \
         once per spawn; a decoder-level default would corrupt every later incremental update"
    );
}
