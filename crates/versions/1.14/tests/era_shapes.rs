//! The era's nine shape deltas, pinned on hand-built bytes.
//!
//! # What this is for
//!
//! The committed captures (`capture_join.rs`) are the authority for the
//! packets a join actually sends. They do not cover `respawn`, `chat`,
//! `use_entity`, the serverbound abilities packet or the recipe-book packet —
//! a flat-world join produces none of those — and those are five of the nine
//! deltas. This file covers them, and it covers the one property that matters
//! most about all of them: **how many bytes the packet occupies.**
//!
//! A wrong *value* in one packet is one wrong reading. A wrong *length*
//! desynchronises the stream: the next packet's header is read from the middle
//! of this one's body, and everything after it is garbage. So every case below
//! asserts the byte count as well as the fields, and asserts that the count
//! differs across the protocols where the wire says it should.
//!
//! # Where the expected values come from
//!
//! The byte strings are written out here by hand from each packet's documented
//! field list and the sizes of its primitives — not produced by this crate's
//! own encoder, which would make the test a round-trip and satisfy any pair of
//! symmetric misunderstandings. The lengths are then arithmetic over those
//! primitives, independent of both arms.

use lodestone_core::{Ctx, Decode, Encode, decode_body_exact, encode_body};
use lodestone_v1_14::packets::game::{
    ClientboundChat, CraftingBookData, JoinGameLegacy, RecipeBook, RespawnLegacy, UseEntity,
};
use lodestone_v1_14::packets::settings::PlayerAbilities;
use lodestone_v1_14::{PROTOCOL_1_14_4, PROTOCOL_1_15_2, PROTOCOL_1_16_5};

fn ctx(protocol: i32) -> Ctx {
    Ctx { version: protocol }
}

fn decode<T: Decode>(protocol: i32, bytes: &[u8]) -> T {
    decode_body_exact(bytes, ctx(protocol)).expect("body decodes exactly")
}

fn encode<T: Encode>(protocol: i32, value: &T) -> Vec<u8> {
    encode_body(value, ctx(protocol)).expect("body encodes")
}

/// 1.15 inserted a seed hash after the dimension and appended a
/// respawn-screen flag, so the same join packet is **nine bytes longer** at
/// 578 than at 498 — eight for the `i64`, one for the `bool`.
///
/// Both are fields appearing, so one struct with two `since` predicates
/// serves both; the test that this is *correct* rather than merely compiling
/// is that the 498 bytes below carry neither field and still decode to a
/// complete packet.
#[test]
fn join_gains_a_seed_hash_and_a_respawn_flag_at_578() {
    // entity id 7, game mode 1 (creative), dimension 0 (overworld),
    // max players 20, level type "flat", view distance 10, reduced debug false.
    let mut at_498: Vec<u8> = Vec::new();
    at_498.extend_from_slice(&7i32.to_be_bytes());
    at_498.push(1);
    at_498.extend_from_slice(&0i32.to_be_bytes());
    at_498.push(20);
    at_498.push(4); // VarInt length of "flat"
    at_498.extend_from_slice(b"flat");
    at_498.push(10);
    at_498.push(0);
    assert_eq!(at_498.len(), 4 + 1 + 4 + 1 + 5 + 1 + 1);

    // The same packet at 578: seed hash 0x0102030405060708 after the
    // dimension, respawn-screen flag `true` at the end.
    let mut at_578: Vec<u8> = Vec::new();
    at_578.extend_from_slice(&7i32.to_be_bytes());
    at_578.push(1);
    at_578.extend_from_slice(&0i32.to_be_bytes());
    at_578.extend_from_slice(&0x0102_0304_0506_0708i64.to_be_bytes());
    at_578.push(20);
    at_578.push(4);
    at_578.extend_from_slice(b"flat");
    at_578.push(10);
    at_578.push(0);
    at_578.push(1);
    assert_eq!(
        at_578.len() - at_498.len(),
        9,
        "an i64 and a bool, and nothing else, separate the two"
    );

    let old: JoinGameLegacy = decode(PROTOCOL_1_14_4, &at_498);
    assert_eq!(old.entity_id, 7);
    assert_eq!(old.game_mode, 1);
    assert_eq!(old.dimension, 0);
    assert_eq!(old.max_players, 20);
    assert_eq!(old.level_type, "flat");
    assert_eq!(old.view_distance, 10);
    assert_eq!(old.hashed_seed, 0, "498 sends no seed hash");
    assert!(!old.enable_respawn_screen, "498 sends no respawn flag");

    let new: JoinGameLegacy = decode(PROTOCOL_1_15_2, &at_578);
    assert_eq!(new.entity_id, 7);
    assert_eq!(new.hashed_seed, 0x0102_0304_0506_0708);
    assert!(new.enable_respawn_screen);
    assert_eq!(new.level_type, "flat", "the string is not shifted by the seed");

    // The control: 578's bytes read at 498 must not silently succeed. The
    // seed's leading byte becomes the max-players count and its next byte a
    // string length, so the decode either fails or leaves a tail — never a
    // clean, wrong packet.
    assert!(
        decode_body_exact::<JoinGameLegacy>(&at_578, ctx(PROTOCOL_1_14_4)).is_err(),
        "498 must not accept 578's join packet"
    );
    assert!(
        decode_body_exact::<JoinGameLegacy>(&at_498, ctx(PROTOCOL_1_15_2)).is_err(),
        "578 must not accept 498's join packet"
    );
}

/// The same 1.15 seed hash lands in `respawn`, again inserted rather than
/// appended — before the game mode, not after the level type.
///
/// Insertion is the case a length check alone cannot catch, so this asserts
/// the *game mode* too: reading 578's bytes at 498 would take the seed's
/// first byte as the mode.
#[test]
fn respawn_gains_the_seed_hash_at_578_before_the_game_mode() {
    let mut at_498: Vec<u8> = Vec::new();
    at_498.extend_from_slice(&(-1i32).to_be_bytes()); // the nether
    at_498.push(2); // adventure
    at_498.push(7);
    at_498.extend_from_slice(b"default");

    let mut at_578: Vec<u8> = Vec::new();
    at_578.extend_from_slice(&(-1i32).to_be_bytes());
    at_578.extend_from_slice(&0x1122_3344_5566_7788i64.to_be_bytes());
    at_578.push(2);
    at_578.push(7);
    at_578.extend_from_slice(b"default");
    assert_eq!(at_578.len() - at_498.len(), 8);

    let old: RespawnLegacy = decode(PROTOCOL_1_14_4, &at_498);
    assert_eq!((old.dimension, old.game_mode, old.hashed_seed), (-1, 2, 0));
    assert_eq!(old.level_type, "default");

    let new: RespawnLegacy = decode(PROTOCOL_1_15_2, &at_578);
    assert_eq!(
        (new.dimension, new.game_mode, new.hashed_seed),
        (-1, 2, 0x1122_3344_5566_7788),
        "the game mode must come from after the seed, not from inside it"
    );
    assert_eq!(new.level_type, "default");
}

/// 1.16 appended a 128-bit sender UUID to clientbound `chat`. Sixteen bytes,
/// at the end, so the two protocols' bodies differ in length by exactly that.
#[test]
fn chat_gains_a_sender_uuid_at_754() {
    let mut prefix: Vec<u8> = Vec::new();
    prefix.push(2); // VarInt length of the JSON below
    prefix.extend_from_slice(b"{}");
    prefix.push(1); // position: system

    let legacy = prefix.clone();
    let mut modern = prefix;
    modern.extend_from_slice(&[0xab; 16]);
    assert_eq!(modern.len() - legacy.len(), 16);

    for protocol in [PROTOCOL_1_14_4, PROTOCOL_1_15_2] {
        let body: ClientboundChat = decode(protocol, &legacy);
        assert_eq!(body.message, "{}");
        assert_eq!(body.position, 1);
        assert_eq!(
            body.sender,
            uuid::Uuid::nil(),
            "a field the wire does not carry decodes to its default"
        );
        assert_eq!(encode(protocol, &body).len(), legacy.len());
    }

    let body: ClientboundChat = decode(PROTOCOL_1_16_5, &modern);
    assert_eq!(body.sender, uuid::Uuid::from_bytes([0xab; 16]));
    assert_eq!(encode(PROTOCOL_1_16_5, &body).len(), modern.len());
}

/// 1.16 appended a sneaking flag to serverbound `use_entity`, one byte.
///
/// Asserted through *encode* rather than decode, because that is the
/// direction this crate actually uses: sending a 754-shaped attack to a
/// 1.14.4 server would leave a stray byte in the server's read buffer.
#[test]
fn use_entity_gains_a_sneaking_flag_at_754() {
    let attack = UseEntity {
        target: 42,
        mouse: 1,
        sneaking: true,
    };
    // VarInt 42, VarInt 1, and — 754 only — one bool.
    assert_eq!(encode(PROTOCOL_1_14_4, &attack).len(), 2);
    assert_eq!(encode(PROTOCOL_1_15_2, &attack).len(), 2);
    assert_eq!(encode(PROTOCOL_1_16_5, &attack).len(), 3);
    assert_eq!(encode(PROTOCOL_1_16_5, &attack)[2], 1);
}

/// 1.16 dropped the two trailing `f32` speeds from serverbound `abilities`,
/// so the pre-1.16 body is nine bytes and 754's is one.
///
/// This is the era's only `until` predicate, and the only delta where the
/// *older* protocol carries more.
#[test]
fn abilities_drops_its_two_speeds_at_754() {
    let flying = PlayerAbilities {
        flags: 0x02,
        flying_speed: 0.05,
        walking_speed: 0.1,
    };
    for protocol in [PROTOCOL_1_14_4, PROTOCOL_1_15_2] {
        let bytes = encode(protocol, &flying);
        assert_eq!(bytes.len(), 1 + 4 + 4);
        assert_eq!(bytes[0], 0x02);
        assert_eq!(&bytes[1..5], &0.05f32.to_be_bytes());
        assert_eq!(&bytes[5..9], &0.1f32.to_be_bytes());
    }
    let bytes = encode(PROTOCOL_1_16_5, &flying);
    assert_eq!(bytes, vec![0x02]);
}

/// 1.16 split `crafting_book_data` into `recipe_book` and `displayed_recipe`.
///
/// Not a field delta at all: the older packet leads with an action selector
/// and re-states **all four** recipe books, while 754's names one. The two
/// structs are wired to the same `ClientAction`, so this checks the bodies
/// are what each protocol's own wire says rather than that they compile.
#[test]
fn the_recipe_book_packet_is_two_different_packets_across_the_era() {
    let modern = RecipeBook {
        book_id: 2, // blast furnace
        book_open: true,
        filter_active: false,
    };
    assert_eq!(encode(PROTOCOL_1_16_5, &modern), vec![2, 1, 0]);

    let legacy = CraftingBookData {
        action: 1,
        crafting_open: true,
        crafting_filter: false,
        smelting_open: false,
        smelting_filter: false,
        blasting_open: true,
        blasting_filter: false,
        smoking_open: false,
        smoking_filter: false,
    };
    for protocol in [PROTOCOL_1_14_4, PROTOCOL_1_15_2] {
        assert_eq!(
            encode(protocol, &legacy),
            vec![1, 1, 0, 0, 0, 1, 0, 0, 0],
            "the action selector plus four open/filter pairs, in book order"
        );
    }
}

/// Each of the two protocol-ranged packets refuses the protocols outside its
/// declared range, rather than silently producing bytes for a wire that has
/// no such packet.
///
/// This is the container-level `#[mc(protocols)]` precondition doing its job,
/// and it is what makes "second struct where a field changes type" a
/// checkable claim rather than a naming convention.
#[test]
fn a_ranged_packet_refuses_the_protocols_it_does_not_serve() {
    let legacy = CraftingBookData {
        action: 1,
        crafting_open: false,
        crafting_filter: false,
        smelting_open: false,
        smelting_filter: false,
        blasting_open: false,
        blasting_filter: false,
        smoking_open: false,
        smoking_filter: false,
    };
    assert!(
        encode_body(&legacy, ctx(PROTOCOL_1_16_5)).is_err(),
        "crafting_book_data does not exist at 754"
    );

    let modern = RecipeBook {
        book_id: 0,
        book_open: false,
        filter_active: false,
    };
    for protocol in [PROTOCOL_1_14_4, PROTOCOL_1_15_2] {
        assert!(
            encode_body(&modern, ctx(protocol)).is_err(),
            "recipe_book does not exist at {protocol}"
        );
    }

    // Control: each still encodes for a protocol inside its own range, so the
    // two assertions above are about the range and not about the packet being
    // broken.
    assert!(encode_body(&legacy, ctx(PROTOCOL_1_14_4)).is_ok());
    assert!(encode_body(&modern, ctx(PROTOCOL_1_16_5)).is_ok());
}
