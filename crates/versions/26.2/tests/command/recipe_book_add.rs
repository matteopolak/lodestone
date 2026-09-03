//! Bit-exact wire gate for `ClientboundRecipeBookAddPacket`.
//!
//! A recipe book is a packet a *real vanilla client* parses, and the failure mode
//! of a wrong layout is not a visibly wrong recipe — it is a desync partway
//! through a 1,000-entry list, which surfaces as a disconnect with a stack trace
//! about some unrelated later field. So the layout is asserted byte for byte
//! against the stream codecs read as record definitions:
//!
//! * `vanilla's own recipe display entry's own stream codec` — id, display, `OPTIONAL_VAR_INT` group,
//!   `recipe_book_category` registry id, optional ingredient list.
//! * `vanilla's own slot display's own stream codec` — `vanilla's own byte buf codecs's own registry(SLOT_DISPLAY)` dispatch
//!   then the variant body, with `vanilla's own slot displays's own bootstrap`'s registration order as
//!   the id assignment.
//! * `vanilla's own item stack template's own stream codec` — item **then** count, the opposite field
//!   order from `vanilla's own item stack's own optional stream codec`.
//! * `vanilla's own byte buf codecs's own holder set` — `0` means "a tag reference follows", `n + 1`
//!   means "`n` direct entries follow". Every ingredient list here takes the
//!   direct form, so every count is one more than its length.
//!
//! Item registry ids come from `lodestone_data::items`, the jar-derived census —
//! outside the encoder, which is the point.

use lodestone_data::items::item_id;
use lodestone_server::crafting::{RecipeBookEntry, RecipeDisplay, SlotDisplay};
use lodestone_server::{ServerDirective, ServerProtocol};
use lodestone_v26_2::V770ServerProtocol;

/// LEB128, matching the `Writer`'s own varint.
fn varint(v: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut u = v as u32;
    loop {
        let byte = (u & 0x7F) as u8;
        u >>= 7;
        if u == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out
}

fn ident(s: &str) -> lodestone_model::Identifier {
    s.parse().expect("static id is valid")
}

fn item(name: &str) -> i32 {
    item_id(name).unwrap_or_else(|| panic!("{name} must be in the 26.2 item census"))
}

fn payload(directive: ServerDirective) -> (i32, Vec<u8>) {
    match directive {
        ServerDirective::Send { packet_id, payload } => (packet_id, payload),
        other => panic!("expected a Send, got {other:?}"),
    }
}

/// Every `SlotDisplay` variant a crafting recipe can produce, in one shaped entry,
/// asserted byte for byte.
#[test]
fn shaped_entry_matches_the_stream_codecs_byte_for_byte() {
    let entry = RecipeBookEntry {
        id: 7,
        display: RecipeDisplay::Shaped {
            width: 2,
            height: 2,
            ingredients: vec![
                SlotDisplay::Item(ident("minecraft:stick")),
                SlotDisplay::Tag(ident("minecraft:planks")),
                SlotDisplay::Empty,
                SlotDisplay::Composite(vec![
                    SlotDisplay::Item(ident("minecraft:oak_log")),
                    SlotDisplay::Item(ident("minecraft:birch_log")),
                ]),
            ],
            result: SlotDisplay::Stack {
                item: ident("minecraft:crafting_table"),
                count: 3,
            },
        },
        group: Some(11),
        category: "crafting_redstone",
        crafting_requirements: vec![vec![ident("minecraft:stick")], vec![ident("minecraft:stone")]],
    };

    let (packet_id, body) = payload(V770ServerProtocol.encode_recipe_book_add(&[entry], true));
    assert_eq!(packet_id, 74, "play/clientbound recipe_book_add");

    let mut want: Vec<u8> = Vec::new();
    want.extend(varint(1)); // one entry
    want.extend(varint(7)); // vanilla's own recipe display id's own index
    want.extend(varint(1)); // recipe_display: crafting_shaped
    want.extend(varint(2)); // width
    want.extend(varint(2)); // height
    want.extend(varint(4)); // ingredients.len()
    want.extend(varint(4)); // slot_display: item
    want.extend(varint(item("minecraft:stick")));
    want.extend(varint(6)); // slot_display: tag
    want.extend(varint("minecraft:planks".len() as i32));
    want.extend(b"minecraft:planks");
    want.extend(varint(0)); // slot_display: empty
    want.extend(varint(10)); // slot_display: composite
    want.extend(varint(2)); // contents.len()
    want.extend(varint(4));
    want.extend(varint(item("minecraft:oak_log")));
    want.extend(varint(4));
    want.extend(varint(item("minecraft:birch_log")));
    // result: item_stack — item, then count, then an empty component patch.
    want.extend(varint(5));
    want.extend(varint(item("minecraft:crafting_table")));
    want.extend(varint(3));
    want.extend(varint(0));
    want.extend(varint(0));
    // craftingStation: item(crafting_table), hardcoded by every crafting display.
    want.extend(varint(4));
    want.extend(varint(item("minecraft:crafting_table")));
    // group: an offset VarInt, present — the value written one higher, never a
    // bool-prefixed optional. Derived from the shipped codec's own mapping
    // (`0` empty, `value + 1` present), not from this crate's encoder.
    want.extend(varint(12));
    // recipe_book_category: crafting_redstone is registration index 1.
    want.extend(varint(1));
    // craftingRequirements: present, two ingredients, each a direct HolderSet.
    want.push(1);
    want.extend(varint(2));
    want.extend(varint(2)); // 1 entry + 1
    want.extend(varint(item("minecraft:stick")));
    want.extend(varint(2));
    want.extend(varint(item("minecraft:stone")));
    want.push(0); // flags
    want.push(1); // replace

    assert_eq!(body, want, "recipe_book_add body");
}

/// The shapeless dispatch id and the absent-optional encodings, which the shaped
/// case cannot cover.
#[test]
fn shapeless_entry_and_absent_optionals() {
    let entry = RecipeBookEntry {
        id: 0,
        display: RecipeDisplay::Shapeless {
            ingredients: vec![SlotDisplay::Item(ident("minecraft:oak_log"))],
            result: SlotDisplay::Stack {
                item: ident("minecraft:oak_planks"),
                count: 4,
            },
        },
        group: None,
        category: "crafting_building_blocks",
        crafting_requirements: Vec::new(),
    };

    let (_, body) = payload(V770ServerProtocol.encode_recipe_book_add(&[entry], false));

    let mut want: Vec<u8> = Vec::new();
    want.extend(varint(1));
    want.extend(varint(0));
    want.extend(varint(0)); // recipe_display: crafting_shapeless
    want.extend(varint(1)); // ingredients.len()
    want.extend(varint(4));
    want.extend(varint(item("minecraft:oak_log")));
    want.extend(varint(5)); // result: item_stack
    want.extend(varint(item("minecraft:oak_planks")));
    want.extend(varint(4));
    want.extend(varint(0));
    want.extend(varint(0));
    want.extend(varint(4)); // craftingStation: item
    want.extend(varint(item("minecraft:crafting_table")));
    // group: an offset VarInt, absent — a zero VarInt. Note this byte is the
    // one case a bool-prefixed encoding coincides with the real one, so the
    // present case above is the load-bearing half of this pair.
    want.extend(varint(0));
    want.extend(varint(0)); // crafting_building_blocks is index 0
    want.push(0); // craftingRequirements: absent
    want.push(0); // flags
    want.push(0); // replace

    assert_eq!(body, want, "shapeless recipe_book_add body");
}

/// **The index space is one space.** A `PLACE_RECIPE` carries a position in the
/// list this packet sent, and `crafting::recipe_at_index` is what resolves it
/// server-side. If the two ever walk the corpus differently, every recipe click
/// places a *different* recipe — silently, and plausibly.
#[test]
fn every_entry_id_resolves_to_the_same_recipe() {
    let entries = lodestone_server::crafting::recipe_book_entries();
    assert!(
        entries.len() > 1000,
        "the bundled corpus should yield the whole grid-recipe set; got {}",
        entries.len()
    );
    for entry in entries {
        let index = usize::try_from(entry.id).expect("non-negative id");
        let (_, recipe) = lodestone_server::crafting::recipe_at_index(index)
            .unwrap_or_else(|| panic!("id {index} resolves to no recipe"));
        // The result the entry advertises must be the result the resolved recipe
        // produces — the one field that would visibly disagree under a drift.
        let SlotDisplay::Stack { item, count } = entry.display_result() else {
            panic!("a crafting result is always an item_stack display");
        };
        let expected = recipe.result_stack().expect("a grid recipe has a result");
        assert_eq!(item, expected.item(), "recipe {index} result item");
        assert_eq!(*count, expected.count(), "recipe {index} result count");
    }
}
