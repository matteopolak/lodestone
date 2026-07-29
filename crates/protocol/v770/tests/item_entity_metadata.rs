//! Hermetic replay of **server-authored** `set_entity_data` bytes for dropped
//! items.
//!
//! # Where the expected values come from
//!
//! Not from us. Every packet replayed here was captured off the wire of a real
//! vanilla 26.2 survival server by the `live-item-entity` gate
//! (`tests/live_item_entity_metadata.rs`) and checked in verbatim under
//! `tests/fixtures/`. That gate re-captures and diffs them, so if vanilla ever
//! changes the shape it fails there rather than rotting here.
//!
//! `HANDOFF.md` records why this matters: a decoder validated against bytes our
//! own encoder produced closes perfectly over a shared misunderstanding. The
//! hermetic chunk fixtures passed that way and the live gate then produced 49
//! "unexpected end of input". So these fixtures are evidence, not construction.
//!
//! # What is asserted
//!
//! 1. A plain drop's stack decodes to the right item, count, and empty patch.
//! 2. A drop whose stack carries a component this build does not model still
//!    yields the item's identity, flagged partial — the fail-open path.
//! 3. That partial decode **abandons** the rest of the metadata list rather than
//!    resuming into it, because resuming would produce plausible garbage.

use lodestone_core::{Reader, Writer};
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, EntityMetadataUpdate, ItemStack, Reported,
    VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_v770::packets::metadata::read_entity_metadata;
use lodestone_world::World;

const DIAMOND_FIXTURE: &str = include_str!("fixtures/item_entity_metadata_diamond.hex");
const UNMODELED_FIXTURE: &str =
    include_str!("fixtures/item_entity_metadata_unmodeled_component.hex");

/// Parses the reviewable hex-text fixture format: `#` comment lines carrying the
/// capture's provenance and byte-by-byte annotation, then whitespace-separated
/// hex bytes.
fn fixture_bytes(text: &str) -> Vec<u8> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).expect("fixture hex byte"))
        .collect()
}

/// Splits a captured `set_entity_data` payload into its entity id and the raw
/// metadata list that follows.
fn split_payload(payload: &[u8]) -> (i32, &[u8]) {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().expect("entity id VarInt");
    (entity_id, &payload[reader.position()..])
}

/// Feeds a captured payload through the public adapter seam — the same call the
/// live client makes — and returns the metadata event it raised.
fn replay(payload: &[u8]) -> EntityMetadataUpdate {
    let adapter = V770Adapter::new();
    let mut world = World::new();
    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::SET_ENTITY_DATA,
            payload,
        )
        .expect("a real set_entity_data must never be a fatal decode");
    directives
        .into_iter()
        .find_map(|d| match d {
            Directive::Emit(ClientEvent::EntityMetadataUpdated { metadata, .. }) => Some(metadata),
            _ => None,
        })
        .expect("an item entity's metadata packet must raise EntityMetadataUpdated")
}

fn stack(metadata: &EntityMetadataUpdate) -> ItemStack {
    match metadata.item.clone() {
        Reported::Unreported => panic!("the packet carried the item field"),
        Reported::Reported(None) => panic!("a summoned drop is never the empty stack"),
        Reported::Reported(Some(stack)) => stack,
    }
}

/// The bytes the server sent for a plain diamond drop decode to that item —
/// through the full adapter seam, ending in the event the client consumes.
#[test]
fn server_bytes_for_a_diamond_drop_carry_the_item() {
    let payload = fixture_bytes(DIAMOND_FIXTURE);
    let metadata = replay(&payload);
    let stack = stack(&metadata);

    assert_eq!(stack.item.to_string(), "minecraft:diamond");
    assert_eq!(stack.count, 1);
    assert!(
        !stack.components.has_unmodeled,
        "the captured patch is empty (0 added, 0 removed), so nothing is partial"
    );
    // Nothing else rides an item entity's metadata, and nothing must be invented.
    assert_eq!(metadata.health, None);
    assert_eq!(metadata.custom_name, Reported::Unreported);
    assert_eq!(metadata.variant, None);
}

/// The item stack is the only field an item entity emits, and it is a *complete*
/// decode: the list terminator is reached and the payload fully consumed. This
/// is the alignment assertion — the one the module header calls the misparse
/// detector — run over real bytes.
#[test]
fn a_plain_drop_consumes_its_whole_payload() {
    let payload = fixture_bytes(DIAMOND_FIXTURE);
    let (_, list) = split_payload(&payload);
    let mut reader = Reader::new(list);
    let decoded = read_entity_metadata(&mut reader, None).expect("decode");

    assert!(
        decoded.complete,
        "a stack with no unmodeled component is a complete decode"
    );
    reader
        .ensure_empty()
        .expect("a complete decode leaves zero trailing bytes");
}

/// The fail-open gate. The server's own bytes for a drop carrying
/// `minecraft:repair_cost` — a component this build does not model — still yield
/// the item's identity. The key and count are read *before* any component is, so
/// an unrecognised component costs detail and never the answer to "what is it".
#[test]
fn an_unmodeled_component_still_yields_the_item() {
    let payload = fixture_bytes(UNMODELED_FIXTURE);
    let metadata = replay(&payload);
    let stack = stack(&metadata);

    assert_eq!(stack.item.to_string(), "minecraft:diamond_pickaxe");
    assert_eq!(stack.count, 1);
    assert!(
        stack.components.has_unmodeled,
        "an unmodeled component must flag the stack as partial, not vanish"
    );
}

/// The same bytes, one level down: the decode reports `complete == false` and
/// leaves the reader parked inside the unmodeled component's payload.
///
/// The two unread bytes are `07 ff` — the `repair_cost` value and the list
/// terminator. Resuming there would read `0x07` as a metadata index and then try
/// `0xff` as a serializer VarInt, which is exactly the plausible-looking
/// misparse the abandonment exists to prevent.
#[test]
fn an_unmodeled_component_parks_the_reader_mid_payload() {
    let payload = fixture_bytes(UNMODELED_FIXTURE);
    let (_, list) = split_payload(&payload);
    let mut reader = Reader::new(list);
    let decoded = read_entity_metadata(&mut reader, None).expect("must not be an error");

    assert!(
        !decoded.complete,
        "an unmodeled component cannot be skipped, so the decode is incomplete"
    );
    assert!(
        decoded.metadata.item.is_reported(),
        "the item decoded before the unmodeled component must still be carried"
    );
    assert_eq!(
        reader.remaining_bytes(),
        &[0x07, 0xff],
        "the reader must stop exactly at the unmodeled component's payload"
    );
}

/// Abandonment is *contained*: a field that follows a partially-consumed stack
/// is dropped, never decoded as garbage.
///
/// A real item entity emits nothing after index 8, so the trailing field here is
/// appended to the server's captured stack bytes. That synthesis is deliberately
/// confined to the property under test — the assertion is that the appended
/// health does **not** appear — so it cannot launder any misunderstanding of the
/// item-stack wire, which is supplied entirely by the fixture.
#[test]
fn fields_after_a_partial_stack_are_abandoned_not_misread() {
    let payload = fixture_bytes(UNMODELED_FIXTURE);
    let (_, list) = split_payload(&payload);

    // Everything up to (not including) the unmodeled component's payload and the
    // terminator, then a well-formed health field, then a fresh terminator.
    let mut spliced = list[..list.len() - 2].to_vec();
    spliced.push(0x07); // the repair_cost payload the real packet carried
    spliced.push(9); // index 9 = health
    let mut w = Writer::default();
    w.var_i32(3); // serializer 3 = FLOAT
    spliced.extend(w.into_vec());
    spliced.extend(12.5f32.to_be_bytes());
    spliced.push(0xff);

    let mut reader = Reader::new(&spliced);
    let decoded = read_entity_metadata(&mut reader, None).expect("must not be an error");

    assert!(!decoded.complete);
    assert_eq!(
        decoded
            .metadata
            .item
            .clone()
            .into_value()
            .map(|s| s.item.to_string()),
        Some("minecraft:diamond_pickaxe".to_owned()),
        "the item ahead of the abandonment point is kept"
    );
    assert_eq!(
        decoded.metadata.health, None,
        "a field behind a partially-consumed stack must be abandoned — decoding \
         on would misalign and raise a plausible-looking wrong value"
    );
    assert!(
        reader.remaining() > 0,
        "the abandoned tail is left unread, which is why the caller must skip \
         its trailing-bytes assertion"
    );
}

/// The whole point, stated as the event the client actually sees: a real drop's
/// packet raises `EntityMetadataUpdated` carrying the item, in both the clean
/// and the degraded case. Neither is swallowed.
#[test]
fn both_captures_raise_an_event_rather_than_being_dropped() {
    for (name, text) in [
        ("diamond", DIAMOND_FIXTURE),
        ("unmodeled component", UNMODELED_FIXTURE),
    ] {
        let payload = fixture_bytes(text);
        let (entity_id, _) = split_payload(&payload);
        assert!(entity_id > 0, "{name}: captured a real entity id");
        let metadata = replay(&payload);
        assert!(
            metadata.item.is_reported(),
            "{name}: the drop's identity must reach the event"
        );
    }
}
