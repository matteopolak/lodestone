//! The hosted 26.3 server writes item component patches the way the official
//! server does.
//!
//! The expected bytes are `fixtures/item_components_26_3.json`, captured by
//! `capture_item_components.py` from the official 26.3 server after a `give`
//! of each stack: one component per case, so no case depends on map order.
//! Our encoding of the same stack must equal those bytes, except that a
//! compound's keys are compared as a set: the reference server writes them in
//! hash order, which carries no meaning for a reader.

use lodestone_core::{Nbt, Reader, read_network_nbt};
use lodestone_model::{
    ItemEnchantment, ItemInstrument, ItemStack, Text, TextColor, TextStyle,
};
use lodestone_server::{ServerDirective, ServerProtocol};

fn hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn captured(name: &str) -> Vec<u8> {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/item_components_26_3.json")).unwrap();
    hex(fixtures["cases"][name]["stack_hex"].as_str().unwrap_or_else(|| panic!("no case {name}")))
}

/// The stack bytes our 26.3 server writes into a slot update.
fn encoded(stack: &ItemStack) -> Vec<u8> {
    let ServerDirective::Send { payload, .. } =
        lodestone_v26_3::server_protocol().encode_container_slot(0, 0, 36, Some(stack))
    else {
        panic!("a slot update is one packet");
    };
    // Container id 0, state id 0 and the big-endian slot 36 precede the stack.
    assert_eq!(payload[..4], [0, 0, 0, 36]);
    payload[4..].to_vec()
}

fn stack(item: &str, edit: impl FnOnce(&mut ItemStack)) -> ItemStack {
    let mut stack = ItemStack::new(item.parse().unwrap(), 1);
    edit(&mut stack);
    stack
}

fn assert_exact(name: &str, stack: &ItemStack) {
    assert_eq!(hex_of(&encoded(stack)), hex_of(&captured(name)), "case {name}");
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Sorts every compound's fields so two trees compare as unordered maps.
fn canonical(nbt: Nbt) -> Nbt {
    match nbt {
        Nbt::Compound(mut fields) => {
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            Nbt::Compound(fields.into_iter().map(|(k, v)| (k, canonical(v))).collect())
        }
        Nbt::List { element_type, elements } => Nbt::List {
            element_type,
            elements: elements.into_iter().map(canonical).collect(),
        },
        other => other,
    }
}

/// Splits a single-component stack into its header (count, item, the two
/// patch counts and the component id) and its one NBT payload.
fn header_and_nbt(bytes: &[u8]) -> (Vec<u8>, Nbt) {
    let mut reader = Reader::new(bytes);
    for _ in 0..5 {
        reader.var_i32().unwrap();
    }
    let header = bytes[..bytes.len() - reader.remaining()].to_vec();
    let nbt = read_network_nbt(&mut reader).unwrap();
    assert!(reader.ensure_empty().is_ok(), "one NBT payload follows the header");
    (header, canonical(nbt))
}

#[test]
fn a_plain_stack_carries_an_empty_patch() {
    assert_exact("plain", &stack("minecraft:diamond", |_| {}));
}

#[test]
fn damage_is_a_varint() {
    assert_exact("damage", &stack("minecraft:diamond_pickaxe", |s| s.components.damage = Some(250)));
}

#[test]
fn repair_cost_is_a_varint() {
    assert_exact("repair_cost", &stack("minecraft:diamond_pickaxe", |s| s.components.repair_cost = 7));
}

#[test]
fn enchantments_are_holder_level_pairs() {
    // Efficiency is entry 8 of the alphabetical enchantment registry.
    assert_exact("enchantments", &stack("minecraft:diamond_pickaxe", |s| {
        s.components.enchantments = vec![ItemEnchantment { id: 8, level: 4 }];
    }));
}

#[test]
fn a_book_stores_its_enchantments() {
    // Mending is entry 23.
    assert_exact("stored_enchantments", &stack("minecraft:enchanted_book", |s| {
        s.components.enchantments = vec![ItemEnchantment { id: 23, level: 1 }];
    }));
}

#[test]
fn an_unstyled_name_is_a_bare_string() {
    assert_exact("custom_name_plain", &stack("minecraft:diamond_pickaxe", |s| {
        s.components.custom_name = Some(Text::literal("Lodey"));
    }));
}

#[test]
fn a_styled_name_is_a_compound() {
    let ours = stack("minecraft:diamond_sword", |s| {
        let mut name = Text::literal("Red");
        name.style = TextStyle { color: Some(TextColor::Red), italic: Some(false), ..TextStyle::default() };
        s.components.custom_name = Some(name);
    });
    assert_eq!(header_and_nbt(&encoded(&ours)), header_and_nbt(&captured("custom_name_styled")));
}

#[test]
fn lore_is_a_list_of_components() {
    assert_exact("lore", &stack("minecraft:stick", |s| {
        s.components.lore = vec![Text::literal("line one"), Text::literal("line two")];
    }));
}

#[test]
fn dyed_color_is_a_fixed_width_int() {
    assert_exact("dyed_color", &stack("minecraft:leather_helmet", |s| s.components.dyed_color = Some(1_193_046)));
}

#[test]
fn potion_contents_carry_the_potion_holder() {
    let poison = lodestone_data::potion::PotionId::from_name("minecraft:poison").unwrap();
    assert_exact("potion", &stack("minecraft:potion", |s| s.components.potion = Some(poison.registry_id())));
    let strength = lodestone_data::potion::PotionId::from_name("minecraft:strength").unwrap();
    assert_exact("potion_named", &stack("minecraft:potion", |s| {
        s.components.potion = Some(strength.registry_id());
        s.components.potion_custom_name = Some("lodey".to_owned());
    }));
}

#[test]
fn an_instrument_is_a_registry_reference() {
    assert_exact("instrument", &stack("minecraft:goat_horn", |s| {
        s.components.instrument = Some(ItemInstrument::Reference("minecraft:sing_goat_horn".parse().unwrap()));
    }));
}
