//! The empty-slot sprites a menu declares (issue #376, the data half).
//!
//! Vanilla draws a placeholder in an empty armour or off-hand slot via
//! `Slot.getNoItemIcon`, which every screen reads *off the slot* rather than
//! deriving from the slot's position. This file asserts the declarations, and —
//! more usefully — asserts the two things a positional shortcut would get wrong.
//!
//! Expected values hand-read from
//! `.cache/mc/26.2/src/net/minecraft/world/inventory/InventoryMenu.java:20-73`,
//! not from our own output.

use lodestone_game::menu::{
    EMPTY_ARMOR_SLOT_BOOTS, EMPTY_ARMOR_SLOT_CHESTPLATE, EMPTY_ARMOR_SLOT_HELMET,
    EMPTY_ARMOR_SLOT_LEGGINGS, EMPTY_ARMOR_SLOT_SHIELD, Menu,
};

fn icon(menu: &Menu, index: usize) -> Option<&'static str> {
    menu.slot(index).and_then(|s| s.no_item_icon)
}

/// `InventoryMenu`'s constructor walks `SLOT_IDS = {HEAD, CHEST, LEGS, FEET}`
/// (`:44`) placing menu slots 5..=8 in that order, and pushes the off-hand at 45
/// with the shield sprite. Head is the *top* slot, so 5 is the helmet.
#[test]
fn the_player_menu_declares_vanillas_five_empty_slot_sprites() {
    let menu = Menu::player();
    assert_eq!(icon(&menu, 5), Some(EMPTY_ARMOR_SLOT_HELMET));
    assert_eq!(icon(&menu, 6), Some(EMPTY_ARMOR_SLOT_CHESTPLATE));
    assert_eq!(icon(&menu, 7), Some(EMPTY_ARMOR_SLOT_LEGGINGS));
    assert_eq!(icon(&menu, 8), Some(EMPTY_ARMOR_SLOT_BOOTS));
    assert_eq!(icon(&menu, 45), Some(EMPTY_ARMOR_SLOT_SHIELD));
}

/// The values are the **26.2 sprite paths**, not the pre-1.21.2 texture names the
/// Java constants are still called after.
///
/// This is a spelling assertion and it exists because the issue that asked for
/// this feature named `empty_armor_slot_helmet` — the Java *constant*, not its
/// value. There is no such texture in a 26.2 jar. Getting it wrong costs nothing
/// at compile time and produces five silently missing sprites at runtime, since
/// `GuiAtlas::geometry` returns an empty quad list for an unknown id.
#[test]
fn the_sprite_ids_are_the_26_2_paths_not_the_java_constant_names() {
    for id in [
        EMPTY_ARMOR_SLOT_HELMET,
        EMPTY_ARMOR_SLOT_CHESTPLATE,
        EMPTY_ARMOR_SLOT_LEGGINGS,
        EMPTY_ARMOR_SLOT_BOOTS,
        EMPTY_ARMOR_SLOT_SHIELD,
    ] {
        assert!(
            id.starts_with("container/slot/"),
            "{id} must be a `gui/sprites/container/slot/*` path"
        );
        assert!(
            !id.contains("empty_armor_slot"),
            "{id} looks like the Java constant name; 26.2 ships no such texture"
        );
    }
}

/// **The reason this is a per-slot field.** Nothing else in the player menu
/// declares an icon — and in particular the crafting grid and result, which
/// occupy menu slots 0..=4, must not, or an empty 2x2 would draw four
/// placeholders. A positional rule of the shape "slots 5..=8 are armour" happens
/// to work here; it is the *absences* that show the rule has to come from the
/// slot.
#[test]
fn every_other_slot_declares_nothing() {
    let menu = Menu::player();
    let declared: Vec<usize> = (0..menu.slot_count())
        .filter(|&i| icon(&menu, i).is_some())
        .collect();
    assert_eq!(
        declared,
        vec![5, 6, 7, 8, 45],
        "exactly the four armour slots and the off-hand; the crafting result (0), \
         the 2x2 grid (1..=4), main storage and the hotbar all declare none"
    );
}

/// A chest and a crafting table declare none at all — so a container screen that
/// blits `no_item_icon` unconditionally draws nothing extra there. Without this,
/// the feature could be keyed on "menu slot 5..=8" and pass everything above
/// while painting a helmet into the sixth slot of every chest.
#[test]
fn control_a_generic_container_and_a_crafting_table_declare_none() {
    for menu in [Menu::generic(27), Menu::crafting(3, 3)] {
        assert!(
            (0..menu.slot_count()).all(|i| icon(&menu, i).is_none()),
            "no non-player menu this client models has an empty-slot sprite yet"
        );
    }
}
