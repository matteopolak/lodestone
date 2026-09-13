//! Hermetic byte-exact tests for the recipe-book and bundle serverbound
//! backlog encoders: `select_bundle_item`, `container_slot_state_changed`,
//! `recipe_book_change_settings`, `recipe_book_seen_recipe`, and
//! `place_recipe`.
//!
//! Expected payloads are built from the wire specification with an
//! independent VarInt encoder (never the adapter's own codec), so a
//! symmetric bug cannot pass. Layouts are verified against 26.2's
//! `ServerboundSelectBundleItemPacket` (two VarInts: slot id, selected item
//! index), `ServerboundContainerSlotStateChangedPacket` (VarInt slot id,
//! VarInt container id, then a plain trailing boolean),
//! `ServerboundRecipeBookChangeSettingsPacket` (VarInt `RecipeBookType`
//! ordinal via `writeEnum`, then two plain booleans: open, filtering),
//! `ServerboundRecipeBookSeenRecipePacket` (single VarInt `RecipeDisplayId`
//! index), and `ServerboundPlaceRecipePacket` (VarInt container id, VarInt
//! `RecipeDisplayId` index, then a plain trailing boolean for "use max
//! items").
//!
//! All five actions are routine survival-gameplay interactions (recipe book
//! auto-craft, bundle item highlighting, crafter slot toggles) but none
//! currently have a live call site elsewhere in the workspace (no recipe
//! book UI, no bundle tooltip interaction, no crafter block screen). These
//! tests exist so a future caller has byte-exact protocol coverage before
//! it's wired to anything.

use lodestone_model::{ClientAction, ConnectionState, RecipeBookType, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;

fn varint(v: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut u = v as u32;
    loop {
        let byte = (u & 0x7F) as u8;
        u >>= 7;
        if u != 0 {
            out.push(byte | 0x80);
        } else {
            out.push(byte);
            break;
        }
    }
    out
}

#[test]
fn select_bundle_item_is_two_varints() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SelectBundleItem {
                slot_id: 36,
                selected_item_index: 2,
            },
        )
        .expect("encode select bundle item");
    let mut want = Vec::new();
    want.extend_from_slice(&varint(36));
    want.extend_from_slice(&varint(2));
    assert_eq!(
        encoded,
        Some((play::serverbound::BUNDLE_ITEM_SELECTED, want))
    );
}

#[test]
fn select_bundle_item_none_selected_is_negative_one() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SelectBundleItem {
                slot_id: 9,
                selected_item_index: -1,
            },
        )
        .expect("encode select bundle item");
    let mut want = Vec::new();
    want.extend_from_slice(&varint(9));
    want.extend_from_slice(&varint(-1));
    assert_eq!(
        encoded,
        Some((play::serverbound::BUNDLE_ITEM_SELECTED, want))
    );
}

#[test]
fn container_slot_state_changed_is_two_varints_then_bool() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SetContainerSlotState {
                slot_id: 4,
                container_id: 1,
                new_state: true,
            },
        )
        .expect("encode container slot state changed");
    let mut want = Vec::new();
    want.extend_from_slice(&varint(4));
    want.extend_from_slice(&varint(1));
    want.push(1);
    assert_eq!(
        encoded,
        Some((play::serverbound::CONTAINER_SLOT_STATE_CHANGED, want))
    );
}

#[test]
fn container_slot_state_changed_false_is_a_single_zero_byte_tail() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SetContainerSlotState {
                slot_id: 0,
                container_id: 0,
                new_state: false,
            },
        )
        .expect("encode container slot state changed");
    let (_, bytes) = encoded.expect("some");
    assert_eq!(bytes.last(), Some(&0u8));
}

#[test]
fn recipe_book_change_settings_orders_are_ordinal_then_open_then_filtering() {
    let adapter = V770Adapter::new();
    let cases = [
        (RecipeBookType::Crafting, 0i32),
        (RecipeBookType::Furnace, 1),
        (RecipeBookType::BlastFurnace, 2),
        (RecipeBookType::Smoker, 3),
    ];
    for (book_type, ordinal) in cases {
        let encoded = adapter
            .encode_action(
                ConnectionState::Play,
                &ClientAction::SetRecipeBookSettings {
                    book_type,
                    open: true,
                    filtering: false,
                },
            )
            .expect("encode recipe book change settings");
        let mut want = Vec::new();
        want.extend_from_slice(&varint(ordinal));
        want.push(1); // open
        want.push(0); // filtering
        assert_eq!(
            encoded,
            Some((play::serverbound::RECIPE_BOOK_CHANGE_SETTINGS, want)),
            "mismatch for {book_type:?}"
        );
    }
}

#[test]
fn recipe_book_seen_recipe_is_a_single_varint() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::RecipeBookSeenRecipe { recipe: 42 },
        )
        .expect("encode recipe book seen recipe");
    assert_eq!(
        encoded,
        Some((play::serverbound::RECIPE_BOOK_SEEN_RECIPE, varint(42)))
    );
}

#[test]
fn place_recipe_is_container_id_then_recipe_then_bool() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::PlaceRecipe {
                container_id: 1,
                recipe: 17,
                use_max_items: true,
            },
        )
        .expect("encode place recipe");
    let mut want = Vec::new();
    want.extend_from_slice(&varint(1));
    want.extend_from_slice(&varint(17));
    want.push(1);
    assert_eq!(encoded, Some((play::serverbound::PLACE_RECIPE, want)));
}

#[test]
fn place_recipe_use_max_items_false_is_a_single_zero_byte_tail() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::PlaceRecipe {
                container_id: 0,
                recipe: 0,
                use_max_items: false,
            },
        )
        .expect("encode place recipe");
    let (_, bytes) = encoded.expect("some");
    assert_eq!(bytes.last(), Some(&0u8));
}

#[test]
fn recipe_and_bundle_actions_are_not_encoded_outside_play() {
    let adapter = V770Adapter::new();
    let actions = [
        ClientAction::SelectBundleItem {
            slot_id: 0,
            selected_item_index: 0,
        },
        ClientAction::SetContainerSlotState {
            slot_id: 0,
            container_id: 0,
            new_state: false,
        },
        ClientAction::SetRecipeBookSettings {
            book_type: RecipeBookType::Crafting,
            open: false,
            filtering: false,
        },
        ClientAction::RecipeBookSeenRecipe { recipe: 0 },
        ClientAction::PlaceRecipe {
            container_id: 0,
            recipe: 0,
            use_max_items: false,
        },
    ];
    for action in actions {
        assert_eq!(
            adapter
                .encode_action(ConnectionState::Configuration, &action)
                .expect("encode outside play"),
            None,
            "{action:?} must not encode outside Play"
        );
    }
}
