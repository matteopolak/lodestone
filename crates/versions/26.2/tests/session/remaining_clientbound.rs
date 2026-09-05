//! The decode gate for the eighteen clientbound packets that had no arm
//! at all, with every input byte hand-built from the record definition in
//! `.cache/mc/26.2/src`.
//!
//! # Why hand-built bytes rather than a round trip
//!
//! There is no *encoder* for any of these in this crate — they are clientbound
//! and `V770ServerProtocol` does not send them — so `decode(encode(x)) == x` is
//! not available even as weak evidence. Each `payload` below was written out from
//! the Java `write` method or `StreamCodec` composition, field by field.
//!
//! # The five shapes worth a named test
//!
//! | packet | trap |
//! |---|---|
//! | `debug_*_value` | `Optional` present-flag: absent means the server is **clearing** the key, not sending an empty payload |
//! | `debug_event` | has **no** `Optional` wrapper, unlike its three `*_value` siblings — reusing their reader eats the first payload byte |
//! | `server_links` | `either` writes `true` for **Left**, and Left is the *known id*, not the custom component |
//! | `waypoint` | the position is a four-way tagged union, not an optional — vanilla degrades to chunk and then to bearing with distance |
//! | `show_dialog` | `holder` is off-by-one: `0` means inline, `n` means registry id `n - 1` |
//!
//! # What this file does not cover
//!
//! That each event reaches a consumer. `route` claims all eighteen for `session`
//! and a decode test cannot see whether a fold exists — that is
//! `crates/lodestone-ecs/tests/remaining_clientbound_folds.rs`, which drives the
//! real `SessionPlugin`.

use lodestone_model::event::{
    ChatCompletionsAction, ClientEvent, DebugSampleKind, ServerLinkKind, WaypointId,
    WaypointOperation, WaypointPosition,
};
use lodestone_core::Nbt;
use lodestone_model::{BlockPos, ChunkPos as ModelChunkPos, ConnectionState, Directive, VersionAdapter};
use lodestone_world::{
    BiomePatch, BlockEntitySync, ChunkPos, ColumnPatch, LightPatch, LoadedChunk, WorldSink,
};
use lodestone_v26_2::packet_ids::play;

/// A [`WorldSink`] that ignores everything — none of these packets is terrain.
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
    ) -> BlockEntitySync {
        BlockEntitySync::ChunkAbsent
    }
}

/// Decodes one packet through the real adapter and returns the emitted events.
fn decode(packet_id: i32, payload: &[u8]) -> Vec<ClientEvent> {
    let adapter = lodestone_v26_2::adapter();
    let mut sink = NullSink;
    adapter
        .handle_packet(&mut sink, ConnectionState::Play, packet_id, payload)
        .expect("decode must succeed")
        .into_iter()
        .filter_map(|directive| match directive {
            Directive::Emit(event) => Some(event),
            _ => None,
        })
        .collect()
}

/// Decodes and asserts exactly one event came out, returning it.
fn one(packet_id: i32, payload: &[u8]) -> ClientEvent {
    let mut events = decode(packet_id, payload);
    assert_eq!(
        events.len(),
        1,
        "expected exactly one event from packet {packet_id}, got {events:?}"
    );
    events.remove(0)
}

fn key(name: &str) -> lodestone_model::Identifier {
    name.parse().expect("test key parses")
}

// ---- award_stats -----------------------------------------------------------

/// The **second** id's registry depends on the first: this asserts three
/// different `stat_type`s in one packet resolve through three different tables.
/// A decoder using one fixed table would mislabel two of the three while still
/// producing a plausible-looking event.
#[test]
fn award_stats_resolves_each_value_through_its_own_stat_types_registry() {
    // count 3, then (stat_type id, value id, count) triples:
    //   minecraft:custom (8) / leave_game (0)  = 5
    //   minecraft:mined  (0) / block registry 1 (minecraft:stone) = 7
    //   minecraft:killed (6) / entity type 0 = 9
    let payload = [3u8, 8, 0, 5, 0, 1, 7, 6, 0, 9];
    let ClientEvent::StatisticsAwarded { stats } = one(play::clientbound::AWARD_STATS, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(stats.len(), 3);

    assert_eq!(stats[0].stat_type, key("minecraft:custom"));
    assert_eq!(stats[0].value, Some(key("minecraft:leave_game")));
    assert_eq!(stats[0].count, 5);

    assert_eq!(stats[1].stat_type, key("minecraft:mined"));
    assert_eq!(
        stats[1].value,
        Some(key("minecraft:stone")),
        "a mined value is a block *registry* id, not a palette state id"
    );
    assert_eq!(stats[1].count, 7);

    assert_eq!(stats[2].stat_type, key("minecraft:killed"));
    assert!(
        stats[2].value.is_some(),
        "a killed value is an entity type id and must resolve"
    );
    assert_ne!(
        stats[2].value, stats[1].value,
        "two different registries must not resolve id 0/1 to the same name"
    );
}

/// The control for the "resolves through the right table" claim above: an
/// unknown `stat_type` id must be a hard error, because the bytes after it cannot
/// be attributed.
#[test]
fn award_stats_rejects_an_unknown_stat_type() {
    let adapter = lodestone_v26_2::adapter();
    let mut sink = NullSink;
    let result = adapter.handle_packet(
        &mut sink,
        ConnectionState::Play,
        play::clientbound::AWARD_STATS,
        &[1u8, 99, 0, 1],
    );
    assert!(result.is_err(), "stat_type 99 does not exist in 26.2");
}

// ---- chat completions ------------------------------------------------------

#[test]
fn custom_chat_completions_carries_the_action_and_the_entries() {
    // action 2 (SET), then count 1, then "bob".
    let payload = [2u8, 1, 3, b'b', b'o', b'b'];
    let ClientEvent::ChatCompletionsChanged { action, entries } =
        one(play::clientbound::CUSTOM_CHAT_COMPLETIONS, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(action, ChatCompletionsAction::Set);
    assert_eq!(entries, vec!["bob".to_owned()]);
}

// ---- the debug feeds -------------------------------------------------------

/// **Trap.** The `Optional` present-flag distinguishes "here is a value" from
/// "clear this key". Both must decode, and to *different* events.
#[test]
fn a_debug_block_value_distinguishes_a_payload_from_a_clear() {
    let mut present = 0i64.to_be_bytes().to_vec();
    present.extend_from_slice(&[5u8, 0x01, 0xAB, 0xCD]); // entity_paths, present, 2 bytes
    let ClientEvent::DebugBlockValue {
        pos,
        subscription,
        value,
    } = one(play::clientbound::DEBUG_BLOCK_VALUE, &present)
    else {
        panic!("wrong event");
    };
    assert_eq!(pos, BlockPos { x: 0, y: 0, z: 0 });
    assert_eq!(subscription, key("minecraft:entity_paths"));
    assert_eq!(value, Some(vec![0xAB, 0xCD]));

    let mut absent = 0i64.to_be_bytes().to_vec();
    absent.extend_from_slice(&[5u8, 0x00]);
    let ClientEvent::DebugBlockValue { value, .. } =
        one(play::clientbound::DEBUG_BLOCK_VALUE, &absent)
    else {
        panic!("wrong event");
    };
    assert_eq!(
        value, None,
        "an absent value must decode to None, not to Some(vec![]) -- the store \
         uses that distinction to clear the key"
    );
}

/// `ChunkPos` is one packed long, low word x and high word z. Two VarInts would
/// decode to garbage that still parses.
#[test]
fn a_debug_chunk_value_unpacks_the_chunk_pos_from_one_long() {
    let packed = ((-3i64) << 32) | (7i64 & 0xFFFF_FFFF);
    let mut payload = packed.to_be_bytes().to_vec();
    payload.extend_from_slice(&[1u8, 0x00]); // bees, absent
    let ClientEvent::DebugChunkValue { chunk, .. } =
        one(play::clientbound::DEBUG_CHUNK_VALUE, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(chunk, ModelChunkPos { x: 7, z: -3 });
}

#[test]
fn a_debug_entity_value_keys_on_the_network_id() {
    let payload = [0xACu8, 0x02, 2, 0x01, 0x11]; // entity 300, brains, present
    let ClientEvent::DebugEntityValue {
        entity_id,
        subscription,
        value,
    } = one(play::clientbound::DEBUG_ENTITY_VALUE, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(entity_id, 300);
    assert_eq!(subscription, key("minecraft:brains"));
    assert_eq!(value, Some(vec![0x11]));
}

/// **Trap.** `debug_event` has no `Optional` wrapper. If it were decoded with the
/// `*_value` reader, the first payload byte would be eaten as a present-flag —
/// so this asserts the payload survives whole, and the `0x00` first byte is
/// chosen precisely because the wrong reader would read it as "absent".
#[test]
fn a_debug_event_has_no_optional_wrapper() {
    let payload = [15u8, 0x00, 0x99]; // game_events, then a two-byte payload
    let ClientEvent::DebugEvent {
        subscription,
        value,
    } = one(play::clientbound::DEBUG_EVENT, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(subscription, key("minecraft:game_events"));
    assert_eq!(
        value,
        vec![0x00, 0x99],
        "the leading 0x00 is payload, not an absent-flag"
    );
}

#[test]
fn debug_sample_is_a_counted_list_of_longs_then_a_kind() {
    let mut payload = vec![2u8];
    payload.extend_from_slice(&1_000i64.to_be_bytes());
    payload.extend_from_slice(&2_000i64.to_be_bytes());
    payload.push(0); // TICK_TIME
    let ClientEvent::DebugSample { sample, kind } = one(play::clientbound::DEBUG_SAMPLE, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(sample, vec![1_000, 2_000]);
    assert_eq!(kind, DebugSampleKind::TickTime);
}

#[test]
fn game_test_highlight_pos_is_two_packed_positions() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0i64.to_be_bytes());
    payload.extend_from_slice(&0i64.to_be_bytes());
    let ClientEvent::GameTestHighlightPos { absolute, relative } =
        one(play::clientbound::GAME_TEST_HIGHLIGHT_POS, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(absolute, BlockPos { x: 0, y: 0, z: 0 });
    assert_eq!(relative, BlockPos { x: 0, y: 0, z: 0 });
}

// ---- the zero-byte packets -------------------------------------------------

/// Both of these are `vanilla's own stream codec's own unit`, and a non-empty body means the id table
/// is wrong rather than that the packet grew a field — so the emptiness check is
/// load-bearing and gets a control.
#[test]
fn the_two_zero_byte_packets_decode_from_nothing_and_reject_a_body() {
    assert!(matches!(
        one(play::clientbound::LOW_DISK_SPACE_WARNING, &[]),
        ClientEvent::LowDiskSpaceWarning
    ));
    assert!(matches!(
        one(play::clientbound::CLEAR_DIALOG, &[]),
        ClientEvent::DialogCleared
    ));

    let adapter = lodestone_v26_2::adapter();
    let mut sink = NullSink;
    assert!(
        adapter
            .handle_packet(
                &mut sink,
                ConnectionState::Play,
                play::clientbound::LOW_DISK_SPACE_WARNING,
                &[0x01],
            )
            .is_err(),
        "a body on a unit packet must be an error, or the emptiness check is decorative"
    );
}

// ---- server metadata -------------------------------------------------------

#[test]
fn custom_report_details_is_capped_at_thirty_two_entries() {
    let payload = [1u8, 1, b'a', 1, b'b'];
    let ClientEvent::CustomReportDetails { details } =
        one(play::clientbound::CUSTOM_REPORT_DETAILS, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(details, vec![("a".to_owned(), "b".to_owned())]);

    // The control: 33 entries must be refused, so the cap is real.
    let adapter = lodestone_v26_2::adapter();
    let mut sink = NullSink;
    assert!(
        adapter
            .handle_packet(
                &mut sink,
                ConnectionState::Play,
                play::clientbound::CUSTOM_REPORT_DETAILS,
                &[33u8],
            )
            .is_err()
    );
}

/// **Trap.** `either` writes `true` for Left, and Left is the **known id**.
/// A decoder with the polarity reversed would try to read an NBT blob starting
/// at the VarInt id — which can succeed by accident — so this asserts both arms
/// in one packet.
#[test]
fn server_links_reads_true_as_the_known_id_arm() {
    let mut bytes = vec![2u8];
    // entry 1: true (Left = known), id 3, then a valid URL
    bytes.extend_from_slice(&[0x01, 0x03]);
    let known_url = b"https://known.invalid/";
    bytes.push(known_url.len() as u8);
    bytes.extend_from_slice(known_url);
    // entry 2: false (Right = custom component), a TAG_String NBT component, then a valid URL
    // Network NBT for a bare string: tag id 8, then a UTF length-prefixed body.
    bytes.extend_from_slice(&[0x00, 0x08, 0x00, 0x02, b'h', b'i']);
    let custom_url = b"https://custom.invalid/";
    bytes.push(custom_url.len() as u8);
    bytes.extend_from_slice(custom_url);

    let ClientEvent::ServerLinksReceived { links } = one(play::clientbound::SERVER_LINKS, &bytes)
    else {
        panic!("wrong event");
    };
    assert_eq!(links.len(), 2);
    assert_eq!(
        links[0].kind,
        ServerLinkKind::Known(3),
        "true must select the known-id arm"
    );
    assert_eq!(links[0].url.as_str(), "https://known.invalid/");
    assert!(
        matches!(links[1].kind, ServerLinkKind::Custom(_)),
        "false must select the custom-component arm"
    );
    assert_eq!(links[1].url.as_str(), "https://custom.invalid/");
}

#[test]
fn server_links_rejects_a_malformed_url_at_ingress() {
    let bytes = [0x01, 0x01, 0x03, 0x09, b'n', b'o', b't', b' ', b'a', b' ', b'U', b'R', b'L'];
    let adapter = lodestone_v26_2::adapter();
    let mut sink = NullSink;
    assert!(
        adapter
            .handle_packet(
                &mut sink,
                ConnectionState::Play,
                play::clientbound::SERVER_LINKS,
                &bytes,
            )
            .is_err()
    );
}

#[test]
fn ticking_state_and_step_are_separate_packets() {
    let mut state = 10.0f32.to_be_bytes().to_vec();
    state.push(0x01);
    let ClientEvent::TickingStateChanged { tick_rate, frozen } =
        one(play::clientbound::TICKING_STATE, &state)
    else {
        panic!("wrong event");
    };
    assert_eq!(tick_rate, 10.0);
    assert!(frozen);

    let ClientEvent::TickingStepped { tick_steps } =
        one(play::clientbound::TICKING_STEP, &[0x04])
    else {
        panic!("wrong event");
    };
    assert_eq!(tick_steps, 4);
}

// ---- tag query -------------------------------------------------------------

/// A `null` tag is a bare `TAG_End` byte, and that is a *reply with nothing*
/// rather than a malformed packet — the store keeps the two apart.
#[test]
fn a_tag_query_reply_distinguishes_null_from_a_compound() {
    let ClientEvent::TagQueryResponse {
        transaction_id,
        tag,
    } = one(play::clientbound::TAG_QUERY, &[0x07, 0x00])
    else {
        panic!("wrong event");
    };
    assert_eq!(transaction_id, 7);
    assert_eq!(tag, None, "a bare TAG_End is null");

    let ClientEvent::TagQueryResponse { tag, .. } =
        one(play::clientbound::TAG_QUERY, &[0x07, 0x0A, 0x00])
    else {
        panic!("wrong event");
    };
    assert!(tag.is_some(), "a TAG_Compound is a present tag");
}

// ---- waypoints -------------------------------------------------------------

/// **Trap.** Four position kinds, and all four must decode. A decoder that
/// treated anything but `VEC3I` as "no position" would blank vanilla's locator
/// bar at exactly the distance it matters.
#[test]
fn a_waypoint_decodes_all_four_position_precisions() {
    // operation TRACK(0), then id = named "w", the style key, no colour, and
    // finally the four-way position. "minecraft:test" is 14 chars = 0x0E.
    let base = |position: &[u8]| -> Vec<u8> {
        [
            &[0x00u8][..],
            &[0x00, 0x01, b'w'],
            &[0x0E],
            b"minecraft:test",
            &[0x00],
            position,
        ]
        .concat()
    };

    let ClientEvent::WaypointUpdated {
        operation,
        waypoint,
    } = one(play::clientbound::WAYPOINT, &base(&[0x00]))
    else {
        panic!("wrong event");
    };
    assert_eq!(operation, WaypointOperation::Track);
    assert_eq!(waypoint.id, WaypointId::Named("w".to_owned()));
    assert_eq!(waypoint.style, key("minecraft:test"));
    assert_eq!(waypoint.color, None);
    assert_eq!(waypoint.position, WaypointPosition::Empty);

    let ClientEvent::WaypointUpdated { waypoint, .. } =
        one(play::clientbound::WAYPOINT, &base(&[0x01, 0x01, 0x02, 0x03]))
    else {
        panic!("wrong event");
    };
    assert_eq!(
        waypoint.position,
        WaypointPosition::Exact(BlockPos { x: 1, y: 2, z: 3 }),
        "vanilla's own vec3i's own stream codec is three plain vanilla's own byte buf codecs's own var in ts -- *not* zigzag, \
         which an earlier draft of this test assumed and the decoder correctly did not"
    );

    let ClientEvent::WaypointUpdated { waypoint, .. } =
        one(play::clientbound::WAYPOINT, &base(&[0x02, 0x02, 0x03]))
    else {
        panic!("wrong event");
    };
    assert_eq!(waypoint.position, WaypointPosition::Chunk(ModelChunkPos { x: 2, z: 3 }));

    let mut azimuth = vec![0x03u8];
    azimuth.extend_from_slice(&1.5f32.to_be_bytes());
    let ClientEvent::WaypointUpdated { waypoint, .. } =
        one(play::clientbound::WAYPOINT, &base(&azimuth))
    else {
        panic!("wrong event");
    };
    assert_eq!(waypoint.position, WaypointPosition::Azimuth(1.5));
}

#[test]
fn a_waypoint_can_be_identified_by_uuid() {
    let uuid = uuid::Uuid::from_u128(0x0123_4567_89AB_CDEF_0123_4567_89AB_CDEF);
    let payload: Vec<u8> = [
        &[0x02u8][..], // UPDATE
        &[0x01],       // is-uuid
        &uuid.as_u128().to_be_bytes()[..],
        &[0x0E],
        b"minecraft:test",
        &[0x00], // no colour
        &[0x00], // EMPTY position
    ]
    .concat();
    let ClientEvent::WaypointUpdated {
        operation,
        waypoint,
    } = one(play::clientbound::WAYPOINT, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(operation, WaypointOperation::Update);
    assert_eq!(waypoint.id, WaypointId::Entity(uuid));
}

// ---- dialogs ---------------------------------------------------------------

/// **Trap.** `vanilla's own byte buf codecs's own holder` is off by one: `0` means an inline value
/// follows, `n > 0` means registry id `n - 1`. Reading the raw VarInt as the id
/// would reference the wrong dialog every time — and, worse, would read `0` as
/// "dialog 0" and then leave the inline blob as trailing bytes.
#[test]
fn show_dialog_decodes_the_holder_prefix_with_its_off_by_one() {
    let ClientEvent::DialogShown {
        registry_id,
        inline,
    } = one(play::clientbound::SHOW_DIALOG, &[0x05])
    else {
        panic!("wrong event");
    };
    assert_eq!(
        registry_id,
        Some(4),
        "holder n means registry id n - 1, not n"
    );
    assert_eq!(inline, None);

    let ClientEvent::DialogShown {
        registry_id,
        inline,
    } = one(play::clientbound::SHOW_DIALOG, &[0x00, 0x0A, 0x00])
    else {
        panic!("wrong event");
    };
    assert_eq!(registry_id, None, "holder 0 is the inline form");
    assert_eq!(inline, Some(vec![0x0A, 0x00]));
}

// ---- test instance status --------------------------------------------------

#[test]
fn test_instance_block_status_has_an_optional_size() {
    // A bare TAG_String component "s", then no size.
    let payload = [0x08u8, 0x00, 0x01, b's', 0x00];
    let ClientEvent::TestInstanceBlockStatus { size, .. } =
        one(play::clientbound::TEST_INSTANCE_BLOCK_STATUS, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(size, None);

    let with_size = [0x08u8, 0x00, 0x01, b's', 0x01, 0x01, 0x02, 0x03];
    let ClientEvent::TestInstanceBlockStatus { size, .. } =
        one(play::clientbound::TEST_INSTANCE_BLOCK_STATUS, &with_size)
    else {
        panic!("wrong event");
    };
    assert_eq!(size, Some((1, 2, 3)));
}

// ---- the recipe/trade tranche ----------------------------------------------
//
// All five of these needed `read_slot_display`: `SlotDisplay` is a *recursive*
// registry-dispatched union of eleven variants with no length prefix anywhere, so
// there is no way to skip one without decoding it. That is why none of them could
// land before the walker existed.
//
// `SlotDisplay` ids used below (`vanilla's own slot displays's own java` registration order):
//   0 empty, 1 any_fuel, 4 item, 6 tag, 9 with_remainder, 10 composite.
// `RecipeDisplay` ids (`vanilla's own recipe displays's own java`): 0 shapeless, 1 shaped, 3 stonecutter.

/// A `SlotDisplay` of kind `item` holding item registry id `id`.
fn item_display(id: u8) -> Vec<u8> {
    vec![0x04, id]
}

/// An `empty` `SlotDisplay` — zero payload after its id.
const EMPTY_DISPLAY: [u8; 1] = [0x00];

#[test]
fn recipe_book_remove_is_a_counted_list_of_display_ids() {
    let ClientEvent::RecipeBookRemoved { display_ids } =
        one(play::clientbound::RECIPE_BOOK_REMOVE, &[0x02, 0x07, 0x09])
    else {
        panic!("wrong event");
    };
    assert_eq!(display_ids, vec![7, 9]);
}

/// **The trap.** `replace` sits *after* the entry list, so the list cannot be
/// carried as opaque trailing bytes — you must walk every display to reach it.
/// This test's `replace` is `true`, and a decoder that grabbed the tail as opaque
/// would either lose it or read a display byte as the flag.
#[test]
fn recipe_book_add_reaches_the_replace_flag_past_the_entry_list() {
    // count 1; entry = display_id 4, then a shapeless RecipeDisplay
    // (0 ingredients, result = item 12, station = empty), then group
    // (OPTIONAL_VAR_INT 0 = absent), category 3, no crafting requirements,
    // flags 0b11; then replace = true.
    let payload: Vec<u8> = [
        &[0x01u8][..],       // entry count
        &[0x04],             // display_id
        &[0x00],             // RecipeDisplay kind 0 = crafting_shapeless
        &[0x00],             // ingredient list count 0
        &item_display(12),   // result
        &EMPTY_DISPLAY,      // craftingStation
        &[0x00],             // group: OPTIONAL_VAR_INT absent
        &[0x03],             // category registry id
        &[0x00],             // craftingRequirements absent
        &[0x03],             // flags: notification | highlight
        &[0x01],             // replace = true
    ]
    .concat();
    let ClientEvent::RecipeBookAdded { entries, replace } =
        one(play::clientbound::RECIPE_BOOK_ADD, &payload)
    else {
        panic!("wrong event");
    };
    assert!(replace, "the trailing flag must be reached, not lost");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].display_id, 4);
    assert_eq!(
        entries[0].result_items,
        vec![12],
        "the *result* slot's item, not an ingredient's -- shapeless puts the \
         result after the ingredient list"
    );
    assert!(entries[0].notification);
    assert!(entries[0].highlight);
}

/// The control for "it really walks the ingredients": the same packet with two
/// ingredients before the result must still pick the result, and must still land
/// on the trailing flag. A decoder that took the *first* display as the result
/// would report item 1 here and item 12 above, passing the test above alone.
#[test]
fn a_shapeless_displays_result_is_after_its_ingredients() {
    let payload: Vec<u8> = [
        &[0x01u8][..],
        &[0x04],
        &[0x00],           // crafting_shapeless
        &[0x02],           // two ingredients
        &item_display(1),
        &item_display(2),
        &item_display(12), // result
        &EMPTY_DISPLAY,    // station
        &[0x00],
        &[0x03],
        &[0x00],
        &[0x00], // flags: neither bit
        &[0x00], // replace = false
    ]
    .concat();
    let ClientEvent::RecipeBookAdded { entries, replace } =
        one(play::clientbound::RECIPE_BOOK_ADD, &payload)
    else {
        panic!("wrong event");
    };
    assert!(!replace);
    assert_eq!(
        entries[0].result_items,
        vec![12],
        "picked an ingredient instead of the result"
    );
    assert!(!entries[0].notification);
    assert!(!entries[0].highlight);
}

/// A `shaped` display carries width and height *before* its ingredient list.
/// Skipping them shifts the whole walk by two bytes.
#[test]
fn a_shaped_display_consumes_its_width_and_height() {
    let payload: Vec<u8> = [
        &[0x01u8][..],
        &[0x09],           // display_id
        &[0x01],           // crafting_shaped
        &[0x02],           // width
        &[0x02],           // height
        &[0x01],           // one ingredient
        &item_display(5),
        &item_display(21), // result
        &EMPTY_DISPLAY,    // station
        &[0x00],
        &[0x00],
        &[0x00],
        &[0x00],
        &[0x01], // replace
    ]
    .concat();
    let ClientEvent::RecipeBookAdded { entries, replace } =
        one(play::clientbound::RECIPE_BOOK_ADD, &payload)
    else {
        panic!("wrong event");
    };
    assert!(replace);
    assert_eq!(entries[0].display_id, 9);
    assert_eq!(entries[0].result_items, vec![21]);
}

/// A `composite` result collects every nested display's item, and nesting is what
/// makes the walk recursive rather than a fixed field list.
#[test]
fn a_composite_result_collects_every_nested_item() {
    let payload: Vec<u8> = [
        &[0x01u8][..],
        &[0x01],
        &[0x03], // stonecutter: input, result, station
        &item_display(1),
        &[0x0A, 0x02][..], // result = composite of 2
        &item_display(7),
        &item_display(8),
        &EMPTY_DISPLAY, // station
        &[0x00],
        &[0x00],
        &[0x00],
        &[0x00],
        &[0x00],
    ]
    .concat();
    let ClientEvent::RecipeBookAdded { entries, .. } =
        one(play::clientbound::RECIPE_BOOK_ADD, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(entries[0].result_items, vec![7, 8]);
}

/// An unmodeled `SlotDisplay` id must abandon the packet, not emit a half-read
/// event: the reader's position is untrustworthy from that point, so anything
/// after it would be misattributed bytes.
#[test]
fn an_unknown_slot_display_drops_the_packet_rather_than_guessing() {
    let payload: Vec<u8> = [
        &[0x01u8][..],
        &[0x01],
        &[0x03],
        &[0x7Fu8][..], // slot display id 127: not in the built-in table
        &[0x00],
    ]
    .concat();
    assert!(
        decode(play::clientbound::RECIPE_BOOK_ADD, &payload).is_empty(),
        "an unmodeled nested display must emit nothing"
    );
    // The control: the same framing with a *known* display id does emit, so the
    // assertion above is about the unknown id and not about the framing.
    let ok: Vec<u8> = [
        &[0x01u8][..],
        &[0x01],
        &[0x03],
        &item_display(1),
        &item_display(2),
        &EMPTY_DISPLAY,
        &[0x00],
        &[0x00],
        &[0x00],
        &[0x00],
        &[0x00],
    ]
    .concat();
    assert_eq!(decode(play::clientbound::RECIPE_BOOK_ADD, &ok).len(), 1);
}

#[test]
fn place_ghost_recipe_carries_the_window_and_the_result() {
    let payload: Vec<u8> = [
        &[0x03u8][..], // window id
        &[0x03],       // stonecutter display
        &item_display(1),
        &item_display(44), // result
        &EMPTY_DISPLAY,
    ]
    .concat();
    let ClientEvent::GhostRecipeShown {
        window_id,
        result_items,
    } = one(play::clientbound::PLACE_GHOST_RECIPE, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(window_id, 3);
    assert_eq!(result_items, vec![44]);
}

/// `update_recipes`' first field is cleanly decodable; the second needs a
/// `SlotDisplay` walk. The `tag` display is included because it consumes an
/// `Identifier` string and contributes no item — a walker that assumed every
/// display yields an item would mis-frame the entry after it.
#[test]
fn update_recipes_decodes_property_sets_then_the_stonecutter_list() {
    let payload: Vec<u8> = [
        &[0x01u8][..], // one property set
        &[0x17],       // key length: "minecraft:furnace_input" = 23
        b"minecraft:furnace_input",
        &[0x02, 0x05, 0x06], // two item ids
        &[0x02],             // two stonecutter entries
        // entry 1: ingredient = holder set of one explicit id, result = item 9
        &[0x02, 0x03][..],
        &item_display(9),
        // entry 2: ingredient = tag form, result = a `tag` display (no item)
        &[0x00, 0x0E][..],
        b"minecraft:logs",
        &[0x06, 0x0E][..],
        b"minecraft:logs",
    ]
    .concat();
    let ClientEvent::RecipePropertySetsUpdated {
        item_sets,
        stonecutter_results,
    } = one(play::clientbound::UPDATE_RECIPES, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(item_sets.len(), 1);
    assert_eq!(item_sets[0].0, key("minecraft:furnace_input"));
    assert_eq!(item_sets[0].1, vec![5, 6]);
    assert_eq!(stonecutter_results.len(), 2);
    assert_eq!(stonecutter_results[0].0, vec![3], "the explicit-id ingredient");
    assert_eq!(stonecutter_results[0].1, vec![9]);
    assert!(
        stonecutter_results[1].0.is_empty(),
        "a tag-form ingredient yields no explicit item id"
    );
    assert!(
        stonecutter_results[1].1.is_empty(),
        "a `tag` display consumes an Identifier and yields no item id"
    );
}

/// **Two traps in one packet.** Five `MerchantOffer` fields are big-endian `i32`s
/// rather than VarInts, and the trailing scalars sit *after* the offer list — so
/// they are unreachable unless every offer parsed exactly. The values below are
/// chosen so a VarInt misread cannot coincide with the right answer.
#[test]
fn merchant_offers_reads_five_plain_i32s_and_reaches_the_trailing_scalars() {
    let payload: Vec<u8> = [
        &[0x05u8][..], // window id
        &[0x01],       // one offer
        // cost_a: item 3, count 2, empty component predicate
        &[0x03, 0x02, 0x00][..],
        // result: an absent item stack (count 0)
        &[0x00][..],
        // cost_b absent
        &[0x00][..],
        // out_of_stock
        &[0x00][..],
        &4i32.to_be_bytes()[..],   // uses
        &12i32.to_be_bytes()[..],  // max_uses
        &2i32.to_be_bytes()[..],   // xp
        &(-1i32).to_be_bytes()[..], // special_price_diff -- negative, so a VarInt
                                    // reader cannot land on the same value
        &0.05f32.to_be_bytes()[..], // price_multiplier
        &7i32.to_be_bytes()[..],    // demand
        // trailing scalars, past the list
        &[0x03][..], // villager_level
        &[0x46][..], // villager_xp = 70
        &[0x01][..], // show_progress
        &[0x01][..], // can_restock
    ]
    .concat();
    let ClientEvent::MerchantOffersReceived {
        window_id,
        offers,
        villager_level,
        villager_xp,
        show_progress,
        can_restock,
    } = one(play::clientbound::MERCHANT_OFFERS, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(window_id, 5);
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].cost_a, (3, 2));
    assert_eq!(offers[0].cost_b, None);
    assert_eq!(offers[0].uses, 4);
    assert_eq!(offers[0].max_uses, 12);
    assert_eq!(offers[0].xp, 2);
    assert_eq!(
        offers[0].special_price_diff, -1,
        "a negative writeInt field -- a VarInt reader would produce a huge \
         positive number here and desynchronise everything after it"
    );
    assert_eq!(offers[0].price_multiplier, 0.05);
    assert_eq!(offers[0].demand, 7);
    // These four are the proof the offer list parsed exactly: they sit past it.
    assert_eq!(villager_level, 3);
    assert_eq!(villager_xp, 70);
    assert!(show_progress);
    assert!(can_restock);
}

/// A non-empty `DataComponentExactPredicate` is unmodeled and must abandon the
/// packet. Every vanilla trade sends `EMPTY`, so this is the datapack case.
#[test]
fn a_merchant_offer_with_a_component_predicate_drops_the_packet() {
    let payload: Vec<u8> = [
        &[0x05u8][..],
        &[0x01],
        &[0x03, 0x02, 0x01][..], // predicate count 1: unmodeled
    ]
    .concat();
    assert!(decode(play::clientbound::MERCHANT_OFFERS, &payload).is_empty());
}

/// The three per-entry fields between the display walk and the flags byte:
/// `group`, `category`, and the optional list of reveal-gate ingredient sets.
///
/// **`group` is an offset VarInt, not a bool-prefixed optional.** The two
/// encodings coincide only on the absent case (`0x00` either way), which every
/// other gate in this file happens to use — so the present case here is what
/// distinguishes them, and a decoder reading a presence bool would take the
/// group's own value as the category and desync from there.
///
/// The two ingredient sets deliberately use *different* arms: an explicit id
/// list and a tag name. A decoder that kept only ids would report the second as
/// gating on nothing.
#[test]
fn a_recipe_book_entry_keeps_its_group_category_and_reveal_gate() {
    let payload: Vec<u8> = [
        &[0x01u8][..],       // entry count
        &[0x06],             // display_id
        &[0x00],             // crafting_shapeless
        &[0x00],             // no ingredients
        &item_display(12),   // result
        &EMPTY_DISPLAY,      // craftingStation
        // group: OPTIONAL_VAR_INT, present. The value is written one higher, so
        // 0x0C decodes to 11 — a wrong reader lands on 12 or on a presence bool.
        &[0x0C],
        &[0x02],             // category registry id
        &[0x01],             // craftingRequirements: present
        &[0x02],             // two ingredient sets
        &[0x02, 0x1F][..],   // set 1: direct, one entry (1 + 1), item id 31
        &[0x00][..],         // set 2: tag arm
        &[0x10][..],         // tag name length 16
        b"minecraft:planks",
        &[0x00],             // flags: neither bit
        &[0x00],             // replace = false
    ]
    .concat();
    let ClientEvent::RecipeBookAdded { entries, replace } =
        one(play::clientbound::RECIPE_BOOK_ADD, &payload)
    else {
        panic!("wrong event");
    };
    assert!(!replace, "the trailing flag must still be reached");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].display_id, 6);
    assert_eq!(
        entries[0].group,
        Some(11),
        "the offset comes off at decode, so the wire's 12 is group 11"
    );
    assert_eq!(entries[0].category, 2);
    assert_eq!(
        entries[0].crafting_requirements,
        Some(vec![
            lodestone_model::RegistrySet::Ids(vec![31]),
            lodestone_model::RegistrySet::Tag("minecraft:planks".to_owned()),
        ]),
    );
}
