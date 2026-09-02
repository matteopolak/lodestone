//! Acceptance gate: **our own server** telling a client what a
//! dropped item entity is, byte-checked against a packet captured off a real
//! vanilla 26.2 server.
//!
//! # Where the expected value comes from — not from us
//!
//! `tests/fixtures/item_entity_metadata_diamond.hex` was captured off the wire
//! of a real vanilla survival server by `tests/live_item_entity_metadata.rs` and
//! checked in verbatim, annotated byte by byte. It is `clientbound
//! set_entity_data` for a `{id:"minecraft:diamond",count:1}` drop:
//!
//! ```text
//! 9f e3 01   VarInt entity id (session-scoped; not compared)
//! 08         metadata index 8 = vanilla's own item entity's own item
//! 07         serializer 7 = ITEM_STACK
//! 01         VarInt stack count = 1
//! 9e 07      VarInt item registry id 926 = minecraft:diamond
//! 00 00      DataComponentPatch: 0 added, 0 removed
//! ff         end-of-list sentinel
//! ```
//!
//! So this file's central assertion — that `encode_set_entity_data`'s payload,
//! after its entity-id VarInt, is **byte-identical** to that capture's — has an
//! expected value that predates the encoder and was produced by Mojang's code,
//! not ours. That is what CLAUDE.md's evidence standard asks for and what
//! `decode(encode(x)) == x` cannot give: two symmetric misunderstandings of the
//! `ITEM_STACK` payload shape (a `u8` count, a plain-`i32` registry id, a
//! forgotten component patch) all round-trip perfectly through our own decoder
//! and every one of them fails the comparison below.
//!
//! The index itself has a **second**, independent outside source: the
//! `EntityDataIndexOracle` dump already in the tree
//! (`tests/support/entity_data_index_jvm.txt` — `8 vanilla's own item entity's own item 7
//! ITEM_STACK`), produced by booting the real 26.2 server headlessly. Index 8 is
//! the most contended index in that dump (nineteen claimants), and see
//! `server_protocol.rs`'s `METADATA_IDX_ITEM_ENTITY_ITEM` for why the separating
//! census column is neither `is_living` nor `is_mob` here.
//!
//! # Why this is the packet that decides whether a drop is *visible*
//!
//! A client draws nothing for an item entity whose stack it has not been told:
//! vanilla's `vanilla's own item entity renderer's own submit` returns early on
//! `state.item.isEmpty()`, and this project's own client does the same. Before
//! this was fixed, our server sent `EntitySnapshot::metadata: Vec::new()` for every drop,
//! so a broken block spawned a real item entity that fell, merged and could be
//! picked up — the pickup *visibly*, since the inventory slot updates — while
//! drawing zero pixels. Every link on that path read green.

use lodestone_model::adapter::{ConnectionState, Directive, VersionAdapter};
use lodestone_model::{ClientEvent, Reported, ResourceKey};
use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

const DIAMOND_FIXTURE: &str = include_str!("../fixtures/item_entity_metadata_diamond.hex");

/// Parses the reviewable hex-text fixture format: `#` comment lines carrying
/// the capture's provenance, then whitespace-separated hex bytes. Same reader
/// `tests/item_entity_metadata.rs` uses on the same file.
fn fixture_bytes(text: &str) -> Vec<u8> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).expect("fixture hex byte"))
        .collect()
}

/// Skips a payload's leading entity-id VarInt and returns the metadata list
/// that follows.
///
/// The entity id is the one field that legitimately differs between vanilla's
/// capture and ours — it is session-scoped, and the fixture's own annotation
/// says so. Everything after it is compared exactly.
fn metadata_list(payload: &[u8]) -> &[u8] {
    let mut i = 0;
    // VarInt: continuation bit in 0x80.
    while payload[i] & 0x80 != 0 {
        i += 1;
    }
    &payload[i + 1..]
}

fn rk(name: &str) -> ResourceKey {
    name.parse().expect("valid resource key")
}

fn send_payload(directive: ServerDirective) -> Vec<u8> {
    match directive {
        ServerDirective::Send { packet_id, payload } => {
            assert_eq!(
                packet_id,
                play::clientbound::SET_ENTITY_DATA,
                "the item field travels on set_entity_data, like every other metadata field"
            );
            payload
        }
        other => panic!("expected a Send directive, got {other:?}"),
    }
}

/// **The gate.** Our encoder's bytes for a diamond drop equal vanilla's own,
/// byte for byte, after the session-scoped entity id.
#[test]
fn our_server_encodes_a_dropped_diamond_exactly_as_vanilla_does() {
    let vanilla = fixture_bytes(DIAMOND_FIXTURE);
    let expected = metadata_list(&vanilla);
    // The fixture is evidence, so check it is the shape its own annotation
    // claims before trusting it as an expectation — a truncated or re-saved
    // fixture would otherwise make this gate agree with nothing in particular.
    assert_eq!(
        expected,
        &[0x08, 0x07, 0x01, 0x9e, 0x07, 0x00, 0x00, 0xff],
        "the captured metadata list is index 8 / serializer 7 / count 1 / \
         registry id 926 / empty patch / sentinel"
    );

    let ours = send_payload(V770ServerProtocol.encode_set_entity_data(
        58_271,
        &[MetadataField::Item {
            item: rk("minecraft:diamond"),
            count: 1,
        }],
    ));

    assert_eq!(
        metadata_list(&ours),
        expected,
        "our set_entity_data metadata list must be byte-identical to vanilla's \
         (ours = {:02x?}, vanilla = {expected:02x?})",
        metadata_list(&ours),
    );
}

/// The same bytes, back through the **real client adapter** — the decoder that
/// was already validated against live vanilla captures before this fix existed, so
/// it is not co-authored with the encoder under test.
///
/// This is the "reaches a consumer" half: the client's own `ClientEvent` must
/// carry the stack, because that event is what `EntityInterpolator::set_item_stack`
/// consumes and what the renderer's `isEmpty()` early-out tests.
#[test]
fn the_real_client_adapter_reads_our_bytes_back_as_the_right_stack() {
    let payload = send_payload(V770ServerProtocol.encode_set_entity_data(
        7,
        &[MetadataField::Item {
            item: rk("minecraft:cobblestone"),
            count: 3,
        }],
    ));

    let adapter = lodestone_v770::adapter();
    let mut world = World::new();
    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::SET_ENTITY_DATA,
            &payload,
        )
        .expect("our own set_entity_data must never be a fatal decode");
    let metadata = directives
        .into_iter()
        .find_map(|d| match d {
            Directive::Emit(ClientEvent::EntityMetadataUpdated { metadata, .. }) => Some(metadata),
            _ => None,
        })
        .expect("an item field must raise EntityMetadataUpdated");

    match metadata.item {
        Reported::Reported(Some(stack)) => {
            assert_eq!(stack.item.to_string(), "minecraft:cobblestone");
            assert_eq!(stack.count, 3, "the entity's stack size, not the entity count");
            assert!(
                !stack.components.has_unmodeled,
                "we write an empty component patch, so nothing is partial"
            );
        }
        other => panic!("the client must read a stack back, got {other:?}"),
    }
}

/// **The control for the two gates above, and it fails their assertion.**
///
/// The old behaviour was not "a wrong item field" — it was *no field at
/// all*: `MobSim::snapshots` set `metadata: Vec::new()` for every drop. So the
/// thing the gates must be able to detect is an empty field list, and this
/// asserts exactly what that produces: [`ServerDirective::None`], no packet,
/// nothing on the wire, and therefore an item entity a client renders as
/// nothing while still spawning, falling and being picked up.
///
/// Without this, "the payload matched" would be equally consistent with a gate
/// that never reached the encoder at all.
#[test]
fn an_empty_field_list_sends_no_packet_which_is_the_pre_537_behaviour() {
    let directive = V770ServerProtocol.encode_set_entity_data(7, &[]);
    assert!(
        matches!(directive, ServerDirective::None),
        "an empty metadata list must not spend a packet, got {directive:?}"
    );
}

/// A count-0 drop encodes as the *empty* stack (`vanilla's own item stack's own optional stream codec`
/// writes a bare VarInt `0` and no id), which is what vanilla sends for an item
/// entity whose stack was emptied — and what a client renders as nothing.
///
/// Predicted exactly rather than bracketed: index, serializer, the single `00`,
/// and the sentinel. A port that wrote the registry id anyway would put three
/// extra bytes here and the client would then misparse every following field.
#[test]
fn a_zero_count_stack_encodes_as_the_empty_stack_and_nothing_more() {
    let payload = send_payload(V770ServerProtocol.encode_set_entity_data(
        7,
        &[MetadataField::Item {
            item: rk("minecraft:diamond"),
            count: 0,
        }],
    ));
    assert_eq!(metadata_list(&payload), &[0x08, 0x07, 0x00, 0xff]);
}

/// A field list mixing an item with another species' field still encodes both,
/// in order, each with its own index — the property that makes
/// `encode_set_entity_data` a general encoder rather than one shaped around a
/// creeper. Predicted from the two constants, not observed.
#[test]
fn an_item_field_composes_with_the_other_fields_in_one_list() {
    let payload = send_payload(V770ServerProtocol.encode_set_entity_data(
        7,
        &[
            MetadataField::CreeperSwellDir(1),
            MetadataField::Item {
                item: rk("minecraft:diamond"),
                count: 1,
            },
        ],
    ));
    assert_eq!(
        metadata_list(&payload),
        &[
            0x10, 0x01, 0x01, // index 16 = vanilla's own creeper's own swell dir, INT (1), value 1
            0x08, 0x07, 0x01, 0x9e, 0x07, 0x00, 0x00, // index 8 = vanilla's own item entity's own item
            0xff,
        ],
    );
}
