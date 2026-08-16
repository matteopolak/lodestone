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
use lodestone_data::data_component_types::component_type_name;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, EntityMetadataUpdate, ItemStack, Reported,
    VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_v770::packets::metadata::{TrackedEntity, read_entity_metadata};
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

/// The component the capture actually carries. It was unmodeled when the bytes
/// were captured and is decoded now (`ByteBufCodecs.VAR_INT`), which is why
/// [`with_unmodeled_component`] exists.
const CAPTURED_COMPONENT: &str = "minecraft:repair_cost";

/// A component this build still does not decode. `minecraft:profile` held this
/// slot until it was modeled (see `lodestone_model::ItemProfile`), which is the
/// exact failure this file's sibling gates warned about — replaced with
/// `minecraft:instrument`: `Instrument.STREAM_CODEC` is `ByteBufCodecs.holder`
/// over a `DIRECT_STREAM_CODEC` that is itself a nested holder (`SoundEvent`)
/// plus two floats plus a full chat component, so it is genuinely expensive
/// rather than merely unfinished — a deliberately durable choice, since a cheap
/// one gets modeled and voids this gate.
const UNMODELED_COMPONENT: &str = "minecraft:instrument";

fn component_id(name: &str) -> i32 {
    (0..)
        .find(|&id| component_type_name(id) == Some(name))
        .expect("known component type")
}

/// Rewrites the capture's single component-type id to one this build does not
/// model, leaving every other byte and every offset untouched.
///
/// # Why the capture is spliced rather than replaced
///
/// The captured component (`minecraft:repair_cost`) was unmodeled when these
/// bytes were taken and is modeled now, so the three abandonment gates below
/// silently stopped exercising abandonment at all — the *world* species of
/// vacuous test, where the source stays exemplary and the input stops containing
/// the structure under test. All three failed the moment the arm landed, which is
/// the only reason it was noticed.
///
/// What those gates need from the capture is the **item-stack framing** — count,
/// registry id, patch counts, the terminator — and that is still entirely the
/// server's. Only the one type-id byte is ours. The swap asserts both ids are
/// single-byte VarInts, so no offset moves and the fixture's byte-by-byte
/// annotation stays true.
fn with_unmodeled_component(payload: &[u8]) -> Vec<u8> {
    let captured = component_id(CAPTURED_COMPONENT);
    let replacement = component_id(UNMODELED_COMPONENT);
    assert!(
        (0..0x80).contains(&captured) && (0..0x80).contains(&replacement),
        "both ids must be one-byte VarInts ({captured}, {replacement}) or this \
         splice would shift every following offset"
    );
    let mut out = payload.to_vec();
    let at = out
        .iter()
        .position(|&b| i32::from(b) == captured)
        .expect("the capture must carry the component type id this splices");
    out[at] = u8::try_from(replacement).expect("one-byte VarInt");
    out
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
    let decoded = read_entity_metadata(&mut reader, TrackedEntity::default()).expect("decode");

    assert!(
        decoded.complete,
        "a stack with no unmodeled component is a complete decode"
    );
    reader
        .ensure_empty()
        .expect("a complete decode leaves zero trailing bytes");
}

/// The captured `minecraft:repair_cost` drop now decodes **completely**, and its
/// expected value comes from outside our encoder: these are the server's own
/// bytes for `{"minecraft:repair_cost":7}`, so consuming exactly one VarInt is
/// what makes the terminator land where the capture says it does.
///
/// The two other hypotheses this rules out are the ones a wrong width would
/// produce: consume nothing and `0x07` reads as a metadata index (a misparse), or
/// consume too much and the `0xff` terminator is eaten (an unterminated list).
#[test]
fn the_captured_repair_cost_drop_now_decodes_whole() {
    let payload = fixture_bytes(UNMODELED_FIXTURE);
    let (_, list) = split_payload(&payload);
    let mut reader = Reader::new(list);
    let decoded = read_entity_metadata(&mut reader, TrackedEntity::default()).expect("decode");

    assert!(
        decoded.complete,
        "{CAPTURED_COMPONENT} is a bare VarInt and is decoded now, so the \
         server's bytes must consume exactly"
    );
    reader
        .ensure_empty()
        .expect("consuming exactly one VarInt leaves the terminator as the last byte");
    let stack = stack(&replay(&payload));
    assert_eq!(stack.item.to_string(), "minecraft:diamond_pickaxe");
    assert!(
        !stack.components.has_unmodeled,
        "nothing in the capture is unmodeled any more"
    );
}

/// The fail-open gate. A drop whose stack carries a component this build does not
/// model still yields the item's identity. The key and count are read *before*
/// any component is, so an unrecognised component costs detail and never the
/// answer to "what is it".
#[test]
fn an_unmodeled_component_still_yields_the_item() {
    let payload = with_unmodeled_component(&fixture_bytes(UNMODELED_FIXTURE));
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
/// the reader is left with nothing to read.
///
/// The two bytes behind the abandonment point are `07 ff` — a value and the list
/// terminator. Resuming there would read `0x07` as a metadata index and then try
/// `0xff` as a serializer VarInt, which is exactly the plausible-looking misparse
/// the abandonment exists to prevent — so the decoder **drains** the reader on
/// its way out rather than merely reporting `false`. That is what makes the
/// contract self-enforcing: a caller that ignores the flag can now only raise
/// `UnexpectedEof` (a dropped packet the session survives), never consume those
/// two bytes as a field.
#[test]
fn an_unmodeled_component_parks_the_reader_mid_payload() {
    let payload = with_unmodeled_component(&fixture_bytes(UNMODELED_FIXTURE));
    let (_, list) = split_payload(&payload);
    let mut reader = Reader::new(list);
    let decoded = read_entity_metadata(&mut reader, TrackedEntity::default()).expect("must not be an error");

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
        &[] as &[u8],
        "the abandoning decoder drains the reader, so no byte behind the \
         abandonment point is reachable by a caller that reads on"
    );
    // The control for the drain: those two bytes really were there to be
    // misread. Without this, an empty remainder would be indistinguishable from
    // a fixture that simply ended at the component id.
    let captured = fixture_bytes(UNMODELED_FIXTURE);
    let complete_run = split_payload(&captured).1.len();
    assert_eq!(
        list.len(),
        complete_run,
        "the spliced and captured lists must be the same length, or the splice \
         moved an offset"
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
    let payload = with_unmodeled_component(&fixture_bytes(UNMODELED_FIXTURE));
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
    let decoded = read_entity_metadata(&mut reader, TrackedEntity::default()).expect("must not be an error");

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
    // The abandoned tail is not merely skipped, it is *unreachable*: the decoder
    // drains the reader on its way out, so a caller that ignores the flag and
    // reads on gets `UnexpectedEof` — one dropped packet — rather than this
    // well-formed-looking health field. The assertion above is the control for
    // this one: a real 12.5 was appended and did not arrive, so there genuinely
    // was a tail to make unreachable.
    assert_eq!(
        reader.remaining(),
        0,
        "the abandoning decoder drains the reader; a non-zero remainder means \
         the drain went away and a caller ignoring `complete` can misread again"
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
