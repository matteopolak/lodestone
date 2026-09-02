//! **The wire shape of the pickup animation packet**, and specifically that its
//! three VarInts are in the order the client reads them.
//!
//! # Where the expected value comes from
//!
//! `tests/entity_events.rs`'s `take_item_entity_emits_pickup` builds a
//! `TAKE_ITEM_ENTITY` payload **by hand** from the packet's spec —
//! `var_i32(11), var_i32(1), var_i32(4)` — and asserts the adapter decodes it into
//! `ClientEvent::ItemPickup { item_entity_id: 11, player_id: 1, amount: 4 }`. That
//! fixture predates the encoder and was written from the decode side, so comparing
//! the encoder's bytes against it is a check that **two independent transcriptions
//! agree**, not a self-round-trip.
//!
//! It is worth being precise about how strong that is: it is weaker than
//! `server_item_entity_metadata.rs`'s gate, whose expected bytes were captured off a
//! real vanilla server. Nothing here would catch both transcriptions being wrong in
//! the same way about the *packet id* or about VarInt framing. What it does catch is
//! the error this packet is actually prone to — **transposing the item entity and the
//! collector**, which are adjacent VarInts of the same type. Under a swap the client
//! would interpolate the *player* toward the item, and no round-trip through our own
//! symmetric code would notice.
//!
//! The `amount` semantics (vanilla's `orgCount`, not the amount banked) are a
//! *server* question, not a wire question, and are gated in `lodestone-server`'s
//! `serve_play.rs` — see
//! `a_partial_pickup_announces_the_original_stack_count_not_the_amount_banked`.

use lodestone_core::{Reader, Writer};
use lodestone_server::{ServerDirective, ServerProtocol};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::play;

fn var_i32(v: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(v);
    w.as_slice().to_vec()
}

/// The encoder emits `TAKE_ITEM_ENTITY` with the three VarInts in
/// item-entity/collector/amount order.
///
/// Asymmetric arguments on purpose: `11`, `1` and `4` are pairwise distinct, so a
/// transposition of any two shows up. Equal values would make the assertion pass
/// under every permutation — the same "an input where the hypotheses coincide is not
/// a test" trap that made `oak_leaves` the wrong choice for the item-collision gates.
#[test]
fn the_take_encodes_item_then_collector_then_amount() {
    let proto = V770ServerProtocol;

    let mut expected = var_i32(11);
    expected.extend_from_slice(&var_i32(1));
    expected.extend_from_slice(&var_i32(4));

    match proto.encode_take_item_entity(11, 1, 4) {
        ServerDirective::Send { packet_id, payload } => {
            assert_eq!(
                packet_id,
                play::clientbound::TAKE_ITEM_ENTITY,
                "the pickup animation must go out as take_item_entity"
            );
            assert_eq!(
                payload, expected,
                "payload must be item entity 11, collector 1, amount 4 in that order — \
                 the byte string `entity_events.rs` decodes into ItemPickup {{ 11, 1, 4 }}. \
                 A swap of the first two makes the client lerp the player toward the item"
            );

            // And read back field by field, so a failure says *which* field moved
            // rather than only that the bytes differ.
            let mut r = Reader::new(&payload);
            assert_eq!(r.var_i32().expect("item entity id"), 11);
            assert_eq!(r.var_i32().expect("collector id"), 1);
            assert_eq!(r.var_i32().expect("amount"), 4);
            assert!(
                r.ensure_empty().is_ok(),
                "no trailing bytes: the adapter's decode rejects them, so an extra \
                 field here would be a packet our own client refuses"
            );
        }
        other => panic!("expected a Send directive, got {other:?}"),
    }
}
