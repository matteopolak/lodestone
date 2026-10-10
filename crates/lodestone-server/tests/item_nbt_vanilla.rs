//! The save-file item form reads and writes what the official server saves.
//!
//! The fixture is a chunk the official 26.3 server saved after filling a chest
//! with one stack per component (`capture_item_components.py --saved` in the
//! 26.3 protocol crate). Each saved stack must read into the stack we expect —
//! built here from the `give` argument, not from the reader — and write back
//! to the same compound, with compound keys compared as a set because the
//! reference server writes them in hash order.

use lodestone_core::{Nbt, Reader, read_named_nbt};
use lodestone_model::{
    ItemEnchantment, ItemInstrument, ItemStack, Text, TextColor, TextStyle, WrittenBookContent,
};
use lodestone_server::item_nbt::{stack_from_nbt, stack_to_nbt};

const CHUNK: &[u8] = include_bytes!("../../versions/26.3/tests/fixtures/item_components_26_3_chunk.nbt");
const SLOTS: &str = include_str!("../../versions/26.3/tests/fixtures/item_components_26_3_saved.json");

fn field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

/// The chest's `Items`, keyed by slot.
fn saved_items() -> Vec<(i8, Nbt)> {
    let (_, chunk) = read_named_nbt(&mut Reader::new(CHUNK)).expect("the saved chunk parses");
    let Some(Nbt::List { elements, .. }) = field(&chunk, "block_entities") else {
        panic!("the chunk has block entities");
    };
    let chest = elements
        .iter()
        .find(|entity| matches!(field(entity, "id"), Some(Nbt::String(id)) if id == "minecraft:chest"))
        .expect("the chest was saved");
    let Some(Nbt::List { elements, .. }) = field(chest, "Items") else { panic!("the chest has items") };
    elements
        .iter()
        .map(|item| match field(item, "Slot") {
            Some(Nbt::Byte(slot)) => (*slot, item.clone()),
            other => panic!("slot {other:?}"),
        })
        .collect()
}

fn slot_of(name: &str) -> i8 {
    let slots: serde_json::Value = serde_json::from_str(SLOTS).unwrap();
    slots["slots"][name]["slot"].as_i64().unwrap_or_else(|| panic!("no case {name}")) as i8
}

fn canonical(nbt: Nbt) -> Nbt {
    match nbt {
        Nbt::Compound(mut fields) => {
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            Nbt::Compound(fields.into_iter().map(|(k, v)| (k, canonical(v))).collect())
        }
        Nbt::List { element_type, elements } => {
            Nbt::List { element_type, elements: elements.into_iter().map(canonical).collect() }
        }
        other => other,
    }
}

fn stack(item: &str, count: u32, edit: impl FnOnce(&mut ItemStack)) -> ItemStack {
    let mut stack = ItemStack::new(item.parse().unwrap(), count);
    edit(&mut stack);
    stack
}

fn potion(name: &str) -> (i32, u32) {
    let id = lodestone_data::potion::PotionId::from_name(name).unwrap();
    (id.registry_id(), lodestone_data::potion::potion_color(Some(id), None, &[]))
}

/// Every case: its name in the fixture and the stack the `give` describes.
/// Enchantment ids are positions in the alphabetical enchantment registry.
fn expected() -> Vec<(&'static str, ItemStack)> {
    let mut red = Text::literal("Red");
    red.style = TextStyle { color: Some(TextColor::Red), italic: Some(false), ..TextStyle::default() };
    let custom_data = {
        let mut w = lodestone_core::Writer::default();
        lodestone_core::write_network_nbt(
            &mut w,
            &Nbt::Compound(vec![("lodestone".into(), Nbt::Byte(1)), ("name".into(), Nbt::String("x".into()))]),
        )
        .unwrap();
        w.into_vec()
    };
    vec![
        ("damage", stack("minecraft:diamond_pickaxe", 1, |s| s.components.damage = Some(250))),
        ("enchantments", stack("minecraft:diamond_pickaxe", 1, |s| {
            s.components.enchantments = vec![ItemEnchantment { id: 8, level: 4 }];
        })),
        ("stored_enchantments", stack("minecraft:enchanted_book", 1, |s| {
            s.components.enchantments = vec![ItemEnchantment { id: 23, level: 1 }];
        })),
        ("custom_name_plain", stack("minecraft:diamond_pickaxe", 1, |s| s.components.custom_name = Some(Text::literal("Lodey")))),
        ("custom_name_styled", stack("minecraft:diamond_sword", 1, |s| s.components.custom_name = Some(red))),
        ("lore", stack("minecraft:stick", 1, |s| s.components.lore = vec![Text::literal("line one"), Text::literal("line two")])),
        ("dyed_color", stack("minecraft:leather_helmet", 1, |s| s.components.dyed_color = Some(1_193_046))),
        ("repair_cost", stack("minecraft:diamond_pickaxe", 1, |s| s.components.repair_cost = 7)),
        ("potion", stack("minecraft:potion", 1, |s| {
            let (id, color) = potion("minecraft:poison");
            s.components.potion = Some(id);
            s.components.potion_color = Some(color);
        })),
        ("potion_named", stack("minecraft:potion", 1, |s| {
            let (id, color) = potion("minecraft:strength");
            s.components.potion = Some(id);
            s.components.potion_color = Some(color);
            s.components.potion_custom_name = Some("lodey".into());
        })),
        ("instrument", stack("minecraft:goat_horn", 1, |s| {
            s.components.instrument = Some(ItemInstrument::Reference("minecraft:sing_goat_horn".parse().unwrap()));
        })),
        ("plain", stack("minecraft:diamond", 1, |_| {})),
        ("custom_data", stack("minecraft:stick", 1, |s| s.components.custom_data = Some(custom_data))),
        ("writable_book", stack("minecraft:writable_book", 1, |s| {
            s.components.writable_book_content = Some(vec!["first".into(), "second".into()]);
        })),
        ("written_book", stack("minecraft:written_book", 1, |s| {
            s.components.written_book_content = Some(WrittenBookContent {
                title: "T".into(),
                author: "A".into(),
                generation: 0,
                pages: vec![Text::literal("page one")],
                resolved: true,
            });
        })),
        ("stack_of_many", stack("minecraft:diamond", 37, |_| {})),
    ]
}

#[test]
fn every_saved_stack_reads_as_the_stack_it_was_given() {
    let saved = saved_items();
    for (name, want) in expected() {
        let slot = slot_of(name);
        let (_, nbt) = saved.iter().find(|(s, _)| *s == slot).unwrap_or_else(|| panic!("slot {slot} ({name}) saved"));
        let read = stack_from_nbt(nbt).unwrap_or_else(|| panic!("{name} reads"));
        assert!(!read.components.has_unmodeled, "{name}: every component is read");
        assert_eq!(read, want, "{name}");
    }
    assert_eq!(saved.len(), expected().len(), "every saved stack is a case");
}

#[test]
fn every_stack_writes_back_to_what_the_server_saved() {
    let saved = saved_items();
    for (name, want) in expected() {
        let slot = slot_of(name);
        let (_, nbt) = saved.iter().find(|(s, _)| *s == slot).unwrap();
        let persisted = stack_to_nbt(&want);
        assert!(persisted.complete, "{name}: every component has a saved form");
        let mut fields = vec![("Slot".to_owned(), Nbt::Byte(slot))];
        fields.extend(persisted.fields);
        assert_eq!(canonical(Nbt::Compound(fields)), canonical(nbt.clone()), "{name}");
    }
}

/// The control for `complete`: a stack carrying a component this form does
/// not cover says so instead of passing as whole.
#[test]
fn a_component_without_a_saved_form_is_reported() {
    let decorated = stack("minecraft:decorated_pot", 1, |s| {
        s.components.pot_decorations = Some(lodestone_model::item::PotDecorations::default());
    });
    assert!(!stack_to_nbt(&decorated).complete);
    let mapped = stack("minecraft:filled_map", 1, |s| s.components.map_id = Some(4));
    assert!(stack_to_nbt(&mapped).complete, "a map id is saved");
    let back = stack_from_nbt(&Nbt::Compound(stack_to_nbt(&mapped).fields));
    assert_eq!(back.and_then(|stack| stack.components.map_id), Some(4), "and read back");
    let unread = stack("minecraft:stick", 1, |s| s.components.has_unmodeled = true);
    assert!(!stack_to_nbt(&unread).complete);
}
