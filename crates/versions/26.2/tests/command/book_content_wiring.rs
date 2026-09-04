//! Hermetic wiring tests for `EDIT_BOOK` packet decoding and book-content
//! delivery: the local `ServerBound::EditBook` variant, plus the two
//! clientbound item components (`minecraft:writable_book_content` /
//! `minecraft:written_book_content`) that let a signed or drafted book actually
//! reach a client's screen.
//!
//! Serverbound bytes are hand-built from the wire schema: slot, page count,
//! length-prefixed UTF-8 pages, and an optional title presence byte. They are
//! never round-tripped through this crate's own encoder — there is no
//! serverbound `EditBook` encoder in this client-side crate, so this is the
//! only available check. Clientbound bytes go through the actual
//! [`V770Adapter::handle_packet`] path rather than a bespoke reader (the
//! same "two independent implementations of one spec" shape
//! `container_encoders.rs` already documents), *and* the component-patch
//! tail is additionally asserted byte-for-byte against a hand-computed
//! expectation — a pure round trip alone cannot catch a transposition that
//! is symmetric in both directions, which is exactly the class CLAUDE.md's
//! evidence section warns a round-trip-only check is blind to.

use lodestone_core::State;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, ItemStack, ResourceKey, VersionAdapter,
    WrittenBookContent,
};
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::play;

fn key(s: &str) -> ResourceKey {
    ResourceKey::new("minecraft", s).expect("static key is valid")
}

// ---------------------------------------------------------------------
// Serverbound: `EDIT_BOOK`
// ---------------------------------------------------------------------

/// Draft save: slot 3 (a hotbar slot), one page, no title.
///
/// Wire: VarInt slot (3), VarInt page count (1), one length-prefixed UTF8
/// page ("hi" -> len 2 then the two bytes), then the `Option<String>` title
/// as a bare presence bool (`false`, so no bytes follow).
#[test]
fn decode_edit_book_draft_save_carries_slot_and_pages_with_no_title() {
    let proto = V770ServerProtocol;
    let body = vec![0x03, 0x01, 0x02, b'h', b'i', 0x00];
    assert_eq!(
        proto.decode(State::Play, play::serverbound::EDIT_BOOK, &body),
        ServerBound::EditBook {
            slot: 3,
            pages: vec!["hi".to_owned()],
            title: None,
        },
    );
}

/// Signing submission: slot 40 (off-hand), two pages, a title present.
///
/// The title's presence bool is the field that most needs its own case —
/// swapping `true`/`false` here is exactly the "two adjacent bools" trap
/// CLAUDE.md's evidence section names, so this case's title is `Some` where
/// the draft-save case above is `None`, deliberately different rather than
/// coincidentally so.
#[test]
fn decode_edit_book_signing_carries_slot_pages_and_title() {
    let proto = V770ServerProtocol;
    let mut body = vec![0x28]; // slot 40 (off-hand)
    body.push(0x02); // 2 pages
    body.push(0x01);
    body.push(b'A');
    body.push(0x02);
    body.extend_from_slice(b"BC");
    body.push(0x01); // title present
    body.push(0x03);
    body.extend_from_slice(b"Bee");
    assert_eq!(
        proto.decode(State::Play, play::serverbound::EDIT_BOOK, &body),
        ServerBound::EditBook {
            slot: 40,
            pages: vec!["A".to_owned(), "BC".to_owned()],
            title: Some("Bee".to_owned()),
        },
    );
}

/// **Control** for both cases above: a short/malformed frame must not
/// construct a variant with truncated or zeroed fields — without this, an
/// implementation that always returned `EditBook { slot: 0, pages: vec![],
/// title: None }` regardless of the payload would also pass.
#[test]
fn decode_edit_book_rejects_a_short_frame() {
    let proto = V770ServerProtocol;
    let short = vec![0x03, 0x01];
    assert_eq!(
        proto.decode(State::Play, play::serverbound::EDIT_BOOK, &short),
        ServerBound::Ignored,
    );
}

// ---------------------------------------------------------------------
// Clientbound: the two book components, via `container_set_slot`
// ---------------------------------------------------------------------

/// A [`WorldSink`] that ignores every terrain call — these tests only decode
/// container packets. Mirrors `container_encoders.rs`'s own `NullSink`
/// exactly (duplicated rather than shared: integration test binaries in this
/// crate do not share a support module today).
#[derive(Default)]
struct NullSink;

impl lodestone_world::WorldSink for NullSink {
    fn load(&mut self, _pos: lodestone_world::ChunkPos, _chunk: lodestone_world::LoadedChunk) {}
    fn merge(&mut self, _pos: lodestone_world::ChunkPos, _patch: lodestone_world::ColumnPatch) {}
    fn set_block(&mut self, _x: i32, _y: i32, _z: i32, _state: u32) {}
    fn set_blocks(
        &mut self,
        _section_x: i32,
        _section_y: i32,
        _section_z: i32,
        _blocks: &[(u8, u8, u8, u32)],
    ) {
    }
    fn merge_light(&mut self, _pos: lodestone_world::ChunkPos, _patch: lodestone_world::LightPatch) {}
    fn merge_biomes(&mut self, _pos: lodestone_world::ChunkPos, _patch: lodestone_world::BiomePatch) {}
    fn unload(&mut self, _pos: lodestone_world::ChunkPos) {}
    fn set_block_entity(&mut self, _x: i32, _y: i32, _z: i32, _type_id: u32, _nbt: lodestone_core::Nbt) {}
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

fn decode_container_slot(packet_id: i32, payload: &[u8]) -> ClientEvent {
    let adapter = V770Adapter::default();
    let mut sink = NullSink;
    let directives = adapter
        .handle_packet(&mut sink, ConnectionState::Play, packet_id, payload)
        .expect("decodes");
    assert_eq!(directives.len(), 1, "expected exactly one directive, got {directives:?}");
    let Directive::Emit(event) = directives.into_iter().next().unwrap() else {
        panic!("expected an Emit directive");
    };
    event
}

/// `minecraft:writable_book_content` (component id 54 — the 55th entry of
/// `DATA_COMPONENT_TYPE_NAMES`, verified against the committed generated
/// table) round trips through the real client decoder, and the encoded tail
/// matches a hand-computed expectation.
#[test]
fn writable_book_content_reaches_a_client_container_set_slot() {
    let proto = V770ServerProtocol;
    let mut stack = ItemStack::new(key("writable_book"), 1);
    stack.components.writable_book_content = Some(vec!["A".to_owned(), "BC".to_owned()]);

    let ServerDirective::Send { packet_id, payload } = proto.encode_container_slot(0, 0, 36, Some(&stack))
    else {
        panic!("expected a Send directive");
    };

    // window 0, state 0, slot 36 (big-endian i16), count 1, item id, then the
    // component patch. `writable_book`'s registry id is looked up rather than
    // hardcoded, since only the *component* bytes below are this test's
    // subject.
    let item_id = lodestone_data::items::item_id("minecraft:writable_book")
        .expect("writable_book is a real item");
    let mut expected_prefix = vec![0x00, 0x00, 0x00, 0x24, 0x01];
    expected_prefix.extend(write_varint(item_id));
    assert_eq!(&payload[..expected_prefix.len()], expected_prefix.as_slice());

    let tail = &payload[expected_prefix.len()..];
    let expected_tail: Vec<u8> = vec![
        0x01, // 1 added component
        0x00, // 0 removed components (both counts precede every entry —
        // added entries are encoded before removed entries; not
        // added-count/entries/removed-count)
        0x36, // component id 54 = minecraft:writable_book_content
        0x02, // 2 pages
        0x01, b'A', 0x00, // page "A", no filtered alternate
        0x02, b'B', b'C', 0x00, // page "BC", no filtered alternate
    ];
    assert_eq!(tail, expected_tail.as_slice());

    let ClientEvent::ContainerSlot { item: decoded, .. } = decode_container_slot(packet_id, &payload) else {
        panic!("expected ContainerSlot");
    };
    let decoded = decoded.expect("stack must decode, not vanish");
    assert_eq!(
        decoded.components.writable_book_content,
        Some(vec!["A".to_owned(), "BC".to_owned()]),
    );
}

/// `minecraft:written_book_content` (component id 55) round trips with every
/// field distinct from its neighbours (generation `2`, not `0`; `resolved:
/// false`, intentionally different from the neighbouring boolean value, so a
/// hardcoded `true` in either direction would be caught) — the
/// "pairwise-distinct fixture" discipline
/// CLAUDE.md's evidence section requires for adjacent same-typed fields.
#[test]
fn written_book_content_reaches_a_client_container_set_slot() {
    let proto = V770ServerProtocol;
    let mut stack = ItemStack::new(key("written_book"), 1);
    stack.components.written_book_content = Some(WrittenBookContent {
        title: "My Book".to_owned(),
        author: "Steve".to_owned(),
        generation: 2,
        pages: vec![
            lodestone_model::Text::literal("Once upon a time"),
            lodestone_model::Text::literal("The End"),
        ],
        resolved: false,
    });

    let ServerDirective::Send { packet_id, payload } = proto.encode_container_slot(0, 0, 45, Some(&stack))
    else {
        panic!("expected a Send directive");
    };

    let ClientEvent::ContainerSlot {
        window_id,
        state_id,
        slot,
        item: decoded,
    } = decode_container_slot(packet_id, &payload)
    else {
        panic!("expected ContainerSlot");
    };
    assert_eq!(window_id, 0);
    assert_eq!(state_id, 0);
    assert_eq!(slot, 45);
    let decoded = decoded.expect("stack must decode, not vanish");
    let content = decoded
        .components
        .written_book_content
        .expect("written_book_content must survive the round trip");
    assert_eq!(content.title, "My Book");
    assert_eq!(content.author, "Steve");
    assert_eq!(content.generation, 2);
    assert!(!content.resolved);
    assert_eq!(
        content.pages.iter().map(lodestone_model::Text::to_plain_string).collect::<Vec<_>>(),
        vec!["Once upon a time".to_owned(), "The End".to_owned()],
    );
}

/// **Control** for the component-id lookup both encoders share
/// (`component_type_id`): the two book components must not collide with
/// each other on the wire, which a copy-paste of one id into the other
/// entry's writer would otherwise pass silently (both would decode as *a*
/// book component, just the wrong one, and only a direct id comparison
/// catches that).
#[test]
fn writable_and_written_book_content_use_distinct_component_ids() {
    let proto = V770ServerProtocol;
    let mut writable = ItemStack::new(key("writable_book"), 1);
    writable.components.writable_book_content = Some(vec!["x".to_owned()]);
    let mut written = ItemStack::new(key("written_book"), 1);
    written.components.written_book_content = Some(WrittenBookContent {
        title: "T".to_owned(),
        author: "A".to_owned(),
        generation: 0,
        pages: vec![lodestone_model::Text::literal("p")],
        resolved: true,
    });

    let ServerDirective::Send { payload: writable_payload, .. } =
        proto.encode_container_slot(0, 0, 0, Some(&writable))
    else {
        panic!("expected a Send directive");
    };
    let ServerDirective::Send { payload: written_payload, .. } =
        proto.encode_container_slot(0, 0, 0, Some(&written))
    else {
        panic!("expected a Send directive");
    };

    // Both payloads share the same header shape up to the component id:
    // window(1) + state(1) + slot(2, big-endian i16) + count(1) + item
    // id(N) + added(1) + removed(1) -> the component id follows immediately.
    // Both item ids are looked up (not assumed one byte) so this holds
    // regardless of registry-id width.
    let writable_item_id = lodestone_data::items::item_id("minecraft:writable_book")
        .expect("writable_book is a real item");
    let written_item_id = lodestone_data::items::item_id("minecraft:written_book")
        .expect("written_book is a real item");
    let writable_id_width = write_varint(writable_item_id).len();
    let written_id_width = write_varint(written_item_id).len();
    let writable_component_id = writable_payload[5 + writable_id_width + 2];
    let written_component_id = written_payload[5 + written_id_width + 2];
    assert_ne!(writable_component_id, written_component_id);
    assert_eq!(writable_component_id, 0x36);
    assert_eq!(written_component_id, 0x37);
}

/// Hand-written VarInt encoder (7 payload bits per byte, MSB a continuation
/// flag) — used only to build expected-byte-prefixes in these tests, never
/// as the thing under test.
fn write_varint(mut value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (value as u32 & 0x7F) as u8;
        value = ((value as u32) >> 7) as i32;
        if value != 0 {
            byte |= 0x80;
            out.push(byte);
        } else {
            out.push(byte);
            break;
        }
    }
    out
}
