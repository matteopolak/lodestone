//! Hermetic tests for protocol 776 item-stack **data-component** decoding.
//!
//! The wire shape of a non-empty stack (26.2 `ItemStack.OPTIONAL_STREAM_CODEC`)
//! is `count VarInt`, `item id VarInt`, then a `DataComponentPatch`:
//! `added VarInt`, `removed VarInt`, then the added components as
//! `(type id VarInt, payload)` pairs and the removed components as bare
//! `type id VarInt`s. The added components are **not** length-prefixed, so an
//! unmodeled component cannot be skipped in place — these tests pin both the
//! decode of the components we model and the graceful degradation when an
//! unmodeled component appears.
//!
//! Golden bytes are hand-built from the 26.2 spec so a symmetric encoder bug
//! cannot pass silently; wire-format correctness against a real server is proven
//! separately by the live gate.

use lodestone_core::{Nbt, Writer, write_network_nbt};
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, ItemEnchantment, Text, VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::data_component_types::component_type_name;
use lodestone_v770::items::item_id;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

/// Resolves a data-component-type id from its canonical name via the generated
/// table, so the test never hardcodes a numeric component id.
fn component_id(name: &str) -> i32 {
    (0..)
        .find(|&id| component_type_name(id) == Some(name))
        .expect("known component type")
}

fn handle(id: i32, payload: &[u8]) -> Vec<Directive> {
    V770Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Play, id, payload)
        .expect("handle packet")
}

/// Builds a `container_set_slot` payload (window 1, state 1, slot 36) wrapping a
/// single item stack whose raw component-patch bytes are `patch`.
fn set_slot_with_patch(item: &str, count: i32, patch: &[u8]) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(1); // window id
    w.var_i32(1); // state id
    w.i16(36); // slot
    w.var_i32(count); // stack count (> 0 -> present)
    w.var_i32(item_id(item).expect("known item"));
    w.bytes(patch);
    w.into_vec()
}

fn slot_item(directives: &[Directive]) -> lodestone_model::ItemStack {
    match directives {
        [Directive::Emit(ClientEvent::ContainerSlot { item, .. })] => {
            item.clone().expect("present item")
        }
        other => panic!("expected a single ContainerSlot emit, got {other:?}"),
    }
}

/// A diamond pickaxe with a custom name, durability damage, and one enchantment
/// decodes into the modeled component fields.
#[test]
fn decodes_modeled_components() {
    let mut patch = Writer::default();
    patch.var_i32(3); // three added components
    patch.var_i32(0); // none removed

    // custom_name: a network-NBT text component (here a bare string tag).
    patch.var_i32(component_id("minecraft:custom_name"));
    write_network_nbt(&mut patch, &Nbt::String("Digger".to_owned())).unwrap();

    // damage: a single VarInt.
    patch.var_i32(component_id("minecraft:damage"));
    patch.var_i32(137);

    // enchantments: a VarInt map of Holder<Enchantment> (id + 1) -> VarInt level.
    patch.var_i32(component_id("minecraft:enchantments"));
    patch.var_i32(1); // one entry
    patch.var_i32(12 + 1); // enchantment registry id 12, holder-encoded
    patch.var_i32(4); // level IV

    let payload = set_slot_with_patch("minecraft:diamond_pickaxe", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert_eq!(item.item.to_string(), "minecraft:diamond_pickaxe");
    assert_eq!(item.count, 1);
    assert_eq!(item.components.damage, Some(137));
    assert_eq!(
        item.components.custom_name.as_ref().map(Text::to_plain_string),
        Some("Digger".to_owned())
    );
    assert_eq!(
        item.components.enchantments,
        vec![ItemEnchantment { id: 12, level: 4 }]
    );
    assert!(!item.components.has_unmodeled);
}

/// A stack carrying a component this build does not model still decodes: the
/// session survives, the item/count are intact, and the stack is flagged as
/// carrying unmodeled components rather than raising a fatal decode error.
#[test]
fn tolerates_an_unmodeled_component() {
    // `minecraft:custom_data` (id 0) is an NBT blob this build does not model.
    let mut patch = Writer::default();
    patch.var_i32(1); // one added component
    patch.var_i32(0); // none removed
    patch.var_i32(component_id("minecraft:custom_data"));
    write_network_nbt(
        &mut patch,
        &Nbt::Compound(vec![("x".to_owned(), Nbt::Int(1))]),
    )
    .unwrap();

    let payload = set_slot_with_patch("minecraft:stone", 5, patch.as_slice());
    // Must not error out the whole packet handling.
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert_eq!(item.item.to_string(), "minecraft:stone");
    assert_eq!(item.count, 5);
    assert!(item.components.has_unmodeled);
}

/// Modeled components decoded *before* an unmodeled one are retained.
#[test]
fn retains_modeled_components_before_an_unmodeled_one() {
    let mut patch = Writer::default();
    patch.var_i32(2); // two added components
    patch.var_i32(0);
    // A modeled component first...
    patch.var_i32(component_id("minecraft:damage"));
    patch.var_i32(42);
    // ...then an unmodeled one.
    patch.var_i32(component_id("minecraft:custom_data"));
    write_network_nbt(&mut patch, &Nbt::Compound(Vec::new())).unwrap();

    let payload = set_slot_with_patch("minecraft:diamond_pickaxe", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert_eq!(item.components.damage, Some(42));
    assert!(item.components.has_unmodeled);
}
