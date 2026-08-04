//! Server-authoritative player inventory (issue: server-side inventory
//! model, filed as the prerequisite `#266` itself named — see that issue's
//! investigation comment).
//!
//! Before this module, `lodestone-server` had **no inventory/container model
//! at all** — three separate doc comments (`server.rs`'s `apply_use_item_on`,
//! `protocol.rs`'s `UseItemOn`, `vitals.rs`) said so independently. This is
//! the minimum model that gives a real slot somewhere to write into:
//! [`PlayerInventory`] is 41 native slots (hotbar + main + armour + off-hand),
//! mirroring vanilla's `Inventory` class
//! (`.cache/mc/26.2/src/net/minecraft/world/entity/player/Inventory.java`)
//! exactly:
//!
//! * `items` is `NonNullList.withSize(36, ItemStack.EMPTY)` — hotbar `0..=8`,
//!   main storage `9..=35` (`Inventory.java:56`, `SELECTION_SIZE = 9`,
//!   `INVENTORY_SIZE = 36`).
//! * `EQUIPMENT_SLOT_MAPPING` (`Inventory.java:36-50`) adds feet `36`, legs
//!   `37`, chest `38`, head `39`, off-hand `40` (this module does not model
//!   `41`/body or `42`/saddle — those are mount equipment, not a player's own
//!   inventory, and have no menu slot on the player inventory screen at all).
//!
//! This is not a fresh numbering: it is a restatement of the **same** native
//! indexing `lodestone-game`'s client-side `Menu` already established and
//! documents at `crates/lodestone-game/src/menu.rs:5-27` (`PLAYER_NATIVE_SIZE
//! = 41`, `OFFHAND_NATIVE = 40`). Restated rather than shared because this
//! crate is version- and client-free and does not depend on
//! `lodestone-game`; keeping the two numbering schemes identical is what
//! lets a `CONTAINER_CLICK`'s menu-slot indices (the wire vocabulary, and the
//! same one the client's `Menu` speaks) map onto this model with the exact
//! table the client already uses to build its own menu — see
//! [`PlayerInventory::apply_menu_slot_change`].
//!
//! # Scope cut: no crafting grid, no armour/tool queries yet
//!
//! The player inventory screen's 2×2 crafting grid and result slot (menu
//! indices `0..=4`) are **not** part of vanilla's `Inventory` either — they
//! live in `InventoryMenu`'s own scratch `CraftSlots` container, which this
//! server has no recipe model to resolve a result for. A `CONTAINER_CLICK`
//! that reports a change to one of those menu slots is dropped rather than
//! misapplied (see [`PlayerInventory::apply_menu_slot_change`]'s doc
//! comment) — the same "genuinely different, no data to model it" scope cut
//! `docs/container-cost-screens.md` already documents for the anvil/
//! enchanting-table costs.

use lodestone_model::ItemStack;

/// Native size of the player's own inventory: hotbar (`0..=8`) + main storage
/// (`9..=35`) + armour (`36..=39`) + off-hand (`40`). See the module doc
/// comment for the vanilla citation; mirrors `lodestone-game`'s
/// `PLAYER_NATIVE_SIZE` (`crates/lodestone-game/src/menu.rs:113`).
pub const PLAYER_NATIVE_SIZE: usize = 41;

/// Native index of the off-hand slot (`Inventory.SLOT_OFFHAND`,
/// `Inventory.java:34`). Mirrors `lodestone-game`'s `OFFHAND_NATIVE`
/// (`crates/lodestone-game/src/menu.rs:118`).
pub const OFFHAND_NATIVE: usize = 40;

/// Number of hotbar slots (`Inventory.SELECTION_SIZE`, `Inventory.java:32`).
pub const HOTBAR_SIZE: u8 = 9;

/// A player's server-authoritative inventory: [`PLAYER_NATIVE_SIZE`] native
/// slots plus the selected hotbar index (vanilla's `Inventory.selected`,
/// `Inventory.java:59`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerInventory {
    slots: Vec<Option<ItemStack>>,
    selected_hotbar_slot: u8,
}

impl Default for PlayerInventory {
    fn default() -> Self {
        Self {
            slots: vec![None; PLAYER_NATIVE_SIZE],
            selected_hotbar_slot: 0,
        }
    }
}

impl PlayerInventory {
    /// A fresh, empty inventory with hotbar slot `0` selected — vanilla's
    /// `Inventory`'s own field defaults (`private int selected;` starts at
    /// `0`, `Inventory.java:59`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads the item in native slot `index`, if any. Returns `None` for
    /// both an empty slot and an out-of-range index — a malformed packet's
    /// index should read as "nothing there," not panic, matching this
    /// crate's established convention elsewhere (e.g.
    /// `V770ServerProtocol::face_from_ordinal`'s malformed-input fallback).
    #[must_use]
    pub fn native(&self, index: usize) -> Option<&ItemStack> {
        self.slots.get(index).and_then(Option::as_ref)
    }

    /// Writes `item` into native slot `index`. A silent no-op when `index`
    /// is out of range (see [`native`](Self::native)'s doc comment for why).
    pub fn set_native(&mut self, index: usize, item: Option<ItemStack>) {
        if let Some(slot) = self.slots.get_mut(index) {
            *slot = item;
        }
    }

    /// The currently selected hotbar slot, `0..HOTBAR_SIZE`.
    #[must_use]
    pub fn selected_hotbar_slot(&self) -> u8 {
        self.selected_hotbar_slot
    }

    /// Sets the selected hotbar slot from a `SET_CARRIED_ITEM` packet.
    /// Returns `false` (no-op) for anything outside `0..HOTBAR_SIZE`,
    /// mirroring vanilla's `Inventory.setSelectedSlot` guard
    /// (`Inventory.java:70-76`: `isHotbarSlot` throws server-side; here it
    /// degrades to a rejected update instead of a panic/disconnect, matching
    /// this crate's "malformed packet drops the effect, not the connection"
    /// convention — e.g. `WorldAdminState`'s difficulty/game-rule decode).
    pub fn set_selected_hotbar_slot(&mut self, slot: u8) -> bool {
        if slot < HOTBAR_SIZE {
            self.selected_hotbar_slot = slot;
            true
        } else {
            false
        }
    }

    /// The item in the currently selected hotbar slot (vanilla's
    /// `Inventory.getSelectedItem`, `Inventory.java:78-80`).
    #[must_use]
    pub fn selected_item(&self) -> Option<&ItemStack> {
        self.native(usize::from(self.selected_hotbar_slot))
    }

    /// Applies one `(menu slot, new stack)` entry from a `CONTAINER_CLICK`
    /// targeting window `0` (the player's own inventory), via the
    /// menu-index → native-index table `lodestone-game`'s `Menu::player`
    /// already establishes and documents
    /// (`crates/lodestone-game/src/menu.rs:13-22`):
    ///
    /// | menu slot | native index |
    /// |---|---|
    /// | `5..=8` (armour head/chest/legs/feet) | `39`/`38`/`37`/`36` |
    /// | `9..=35` (main storage) | `9..=35` (identity) |
    /// | `36..=44` (hotbar) | `0..=8` |
    /// | `45` (off-hand) | `40` |
    ///
    /// Menu slots `0..=4` (the 2×2 crafting result/grid) have no native
    /// index at all — see the module doc comment's scope note — so those
    /// entries are dropped rather than misapplied. Returns whether the slot
    /// was recognised, so a caller can log a dropped entry rather than
    /// silently discarding it.
    pub fn apply_menu_slot_change(&mut self, menu_slot: i32, item: Option<ItemStack>) -> bool {
        match player_menu_native_index(menu_slot) {
            Some(native) => {
                self.set_native(native, item);
                true
            }
            None => false,
        }
    }
}

/// The menu-index → native-index mapping for the player's own inventory
/// screen (window `0`) — see [`PlayerInventory::apply_menu_slot_change`]'s
/// doc comment for the table this implements.
fn player_menu_native_index(menu_slot: i32) -> Option<usize> {
    match menu_slot {
        5 => Some(39), // head
        6 => Some(38), // chest
        7 => Some(37), // legs
        8 => Some(36), // feet
        9..=35 => usize::try_from(menu_slot).ok(),
        36..=44 => usize::try_from(menu_slot - 36).ok(),
        45 => Some(OFFHAND_NATIVE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(name: &str, count: u32) -> ItemStack {
        ItemStack::new(name.parse().expect("valid resource key"), count)
    }

    #[test]
    fn fresh_inventory_is_all_empty_with_hotbar_zero_selected() {
        let inv = PlayerInventory::new();
        assert_eq!(inv.selected_hotbar_slot(), 0);
        for i in 0..PLAYER_NATIVE_SIZE {
            assert!(inv.native(i).is_none(), "native {i} should start empty");
        }
    }

    #[test]
    fn set_and_read_native_round_trips() {
        let mut inv = PlayerInventory::new();
        inv.set_native(9, Some(stack("minecraft:stone", 64)));
        assert_eq!(inv.native(9), Some(&stack("minecraft:stone", 64)));
        assert!(inv.native(10).is_none());
    }

    /// An out-of-range native write is a documented no-op, not a panic — the
    /// control that the guard is real (proof it isn't merely a `Vec` that
    /// happens not to have been exercised out of range yet).
    #[test]
    fn out_of_range_native_write_is_a_silent_no_op() {
        let mut inv = PlayerInventory::new();
        inv.set_native(PLAYER_NATIVE_SIZE, Some(stack("minecraft:stone", 1)));
        inv.set_native(9999, Some(stack("minecraft:stone", 1)));
        assert!(inv.native(PLAYER_NATIVE_SIZE).is_none());
    }

    #[test]
    fn selected_hotbar_slot_rejects_out_of_range() {
        let mut inv = PlayerInventory::new();
        assert!(inv.set_selected_hotbar_slot(8));
        assert_eq!(inv.selected_hotbar_slot(), 8);
        assert!(!inv.set_selected_hotbar_slot(9));
        // Rejected update must not have clobbered the last-good value —
        // this is the control: without the guard, 9 would have "succeeded"
        // and this assertion would go red.
        assert_eq!(inv.selected_hotbar_slot(), 8);
    }

    #[test]
    fn selected_item_reads_the_selected_hotbar_native_slot() {
        let mut inv = PlayerInventory::new();
        inv.set_native(3, Some(stack("minecraft:diamond", 1)));
        assert!(inv.set_selected_hotbar_slot(3));
        assert_eq!(inv.selected_item(), Some(&stack("minecraft:diamond", 1)));
    }

    /// Pins every entry of the menu→native table against vanilla's
    /// `InventoryMenu` layout (mirrored from `lodestone-game`'s own client
    /// model, `menu.rs:13-22`) — the exact mapping a real `CONTAINER_CLICK`
    /// against window 0 must agree with for the server model to land in the
    /// same native slot the client already predicted into.
    #[test]
    fn menu_slot_change_maps_every_documented_entry() {
        let cases: &[(i32, usize)] = &[
            (5, 39),  // head
            (6, 38),  // chest
            (7, 37),  // legs
            (8, 36),  // feet
            (9, 9),   // main storage start
            (35, 35), // main storage end
            (36, 0),  // hotbar start
            (44, 8),  // hotbar end
            (45, 40), // off-hand
        ];
        for &(menu_slot, native) in cases {
            let mut inv = PlayerInventory::new();
            let item = stack("minecraft:emerald", 1);
            assert!(
                inv.apply_menu_slot_change(menu_slot, Some(item.clone())),
                "menu slot {menu_slot} should be recognised"
            );
            assert_eq!(
                inv.native(native),
                Some(&item),
                "menu slot {menu_slot} should land at native {native}"
            );
        }
    }

    /// The crafting grid/result (menu `0..=4`) has no native slot — a
    /// `CONTAINER_CLICK` reporting a change there must be dropped, not
    /// misapplied to some other native index. The control:
    /// [`menu_slot_change_maps_every_documented_entry`] proves recognised
    /// slots really do get written, so this asserting `false` here is a
    /// meaningful negative, not a vacuous one.
    #[test]
    fn crafting_grid_and_result_menu_slots_are_dropped() {
        for menu_slot in 0..=4 {
            let mut inv = PlayerInventory::new();
            let before = inv.clone_slots_for_test();
            assert!(
                !inv.apply_menu_slot_change(menu_slot, Some(stack("minecraft:stone", 1))),
                "menu slot {menu_slot} should not be recognised"
            );
            assert_eq!(
                inv.clone_slots_for_test(),
                before,
                "dropped entry must not mutate any native slot"
            );
        }
    }

    /// A change writing `None` clears a native slot that previously held an
    /// item — the "the item was removed" case a click's own diff also has to
    /// carry (e.g. picking a whole stack off a slot onto the cursor).
    #[test]
    fn menu_slot_change_can_clear_a_native_slot() {
        let mut inv = PlayerInventory::new();
        assert!(inv.apply_menu_slot_change(9, Some(stack("minecraft:stone", 1))));
        assert!(inv.native(9).is_some());
        assert!(inv.apply_menu_slot_change(9, None));
        assert!(inv.native(9).is_none());
    }

    impl PlayerInventory {
        /// Test-only accessor so the drop test above can assert "nothing
        /// changed" without exposing a general clone of internal state on
        /// the public API.
        fn clone_slots_for_test(&self) -> Vec<Option<ItemStack>> {
            self.slots.clone()
        }
    }
}
