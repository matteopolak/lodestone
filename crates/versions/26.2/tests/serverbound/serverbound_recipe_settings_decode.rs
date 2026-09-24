//! Independent wire fixtures for the inbound recipe-book settings packet.
//!
//! The body is a recipe-book ordinal followed by the open and filtering
//! booleans. The expected bytes are assembled directly from that wire shape,
//! rather than from the adapter's encoder, so a symmetric codec mistake cannot
//! make the decode test pass.

use lodestone_core::State;
use lodestone_model::RecipeBookType;
use lodestone_server::{ServerBound, ServerProtocol};
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::V770ServerProtocol;

#[test]
fn recipe_book_settings_decodes_the_book_and_both_flags() {
    let decoded = V770ServerProtocol.decode(
        State::Play,
        play::serverbound::RECIPE_BOOK_CHANGE_SETTINGS,
        &[2, 1, 0],
    );

    assert!(
        matches!(
            decoded,
            ServerBound::RecipeBookSettingsChanged {
                book_type: RecipeBookType::BlastFurnace,
                open: true,
                filtering: false,
            }
        ),
        "expected a blast-furnace settings update, got {decoded:?}"
    );
}

#[test]
fn recipe_book_settings_rejects_unknown_ordinals_and_trailing_bytes() {
    let unknown = V770ServerProtocol.decode(
        State::Play,
        play::serverbound::RECIPE_BOOK_CHANGE_SETTINGS,
        &[4, 1, 0],
    );
    assert!(
        matches!(unknown, ServerBound::Ignored),
        "an unknown book ordinal must be ignored, got {unknown:?}"
    );

    let trailing = V770ServerProtocol.decode(
        State::Play,
        play::serverbound::RECIPE_BOOK_CHANGE_SETTINGS,
        &[0, 1, 0, 0xff],
    );
    assert!(
        matches!(trailing, ServerBound::Ignored),
        "trailing bytes must be rejected, got {trailing:?}"
    );
}
