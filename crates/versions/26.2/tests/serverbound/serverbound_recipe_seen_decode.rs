//! Independent raw-wire fixtures for the recipe-book seen acknowledgement.
//!
//! `RecipeDisplayId` is a plain VarInt. These bytes are written directly from
//! that shape rather than through the client encoder, so the decoder's length
//! guard cannot pass because both halves share a mistake.

use lodestone_core::State;
use lodestone_server::{ServerBound, ServerProtocol};
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::V770ServerProtocol;

#[test]
fn recipe_book_seen_decodes_a_multibyte_display_id() {
    let decoded = V770ServerProtocol.decode(
        State::Play,
        play::serverbound::RECIPE_BOOK_SEEN_RECIPE,
        &[0xac, 0x02], // VarInt 300
    );

    assert!(
        matches!(decoded, ServerBound::RecipeBookRecipeSeen { recipe_index: 300 }),
        "expected the raw display id to reach the server vocabulary, got {decoded:?}"
    );
}

#[test]
fn recipe_book_seen_rejects_truncated_and_trailing_bodies() {
    for body in [&[0xac][..], &[0xac, 0x02, 0x00][..]] {
        let decoded = V770ServerProtocol.decode(
            State::Play,
            play::serverbound::RECIPE_BOOK_SEEN_RECIPE,
            body,
        );
        assert!(
            matches!(decoded, ServerBound::Ignored),
            "a non-exact recipe-book seen body must be ignored, got {decoded:?}"
        );
    }
}
