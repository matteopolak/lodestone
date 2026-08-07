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

    /// Adds `stack` to this inventory, mirroring vanilla's
    /// `Inventory.add(-1, stack)` → `addResource` loop
    /// (`Inventory.java`'s `add`/`addResource`/`getSlotWithRemainingSpace`/
    /// `getFreeSlot`), and returns **every native index this call wrote** plus
    /// whatever could not be fitted.
    ///
    /// This is what item pickup credits into (issue #337). The returned index
    /// list is the point: the caller has to tell the client about each slot it
    /// touched with its own `container_set_slot`, and a pickup that overflows
    /// into a second slot writes *two*.
    ///
    /// # The destination order is not "first empty slot", and getting it wrong
    /// is invisible
    ///
    /// `getSlotWithRemainingSpace` searches for a **mergeable** slot in this
    /// exact order — and `hasRemainingSpaceForItem` requires the candidate to
    /// be non-empty, so this pass only ever tops up an existing stack:
    ///
    /// 1. the **selected hotbar slot** (`this.selected`),
    /// 2. the **off-hand** (native `40`, the literal `40` in vanilla's own
    ///    source),
    /// 3. natives `0..36` in ascending order (hotbar, then main storage).
    ///
    /// Only if that finds nothing does `getFreeSlot` place a fresh stack — and
    /// it scans **`items` alone, `0..36`**. So a fresh stack can never land in
    /// the off-hand or in an armour slot, while a *merge* into the off-hand is
    /// entirely normal. Both halves are needed: picking "first empty slot"
    /// unconditionally still produces an item in the inventory, so the naive
    /// version passes any test that only asks whether the pickup arrived.
    ///
    /// Returns `(written natives, leftover)`. `leftover` is `None` when the
    /// whole stack fitted; a full inventory returns the unplaced remainder so
    /// the caller can leave the item entity in the world rather than deleting
    /// it — vanilla's `playerTouch` only removes the entity when
    /// `getInventory().add(...)` consumed everything.
    pub fn add(&mut self, stack: ItemStack) -> (Vec<usize>, Option<ItemStack>) {
        let mut remaining = stack;
        let mut written = Vec::new();
        // `max_stack_size` is per-item in vanilla (`getMaxStackSize`); this
        // crate has no per-item census of it on the server side, so 64 stands
        // in. That is right for every block drop the bundled tables produce
        // (cobblestone/dirt/gravel/flint/coal/raw_iron are all 64-stackable)
        // and is the same constant `MobSim::spawn_item`'s callers already pass
        // to `ItemLifecycle::newly_dropped`. A tool or a bucket picked up this
        // way would over-stack; that wants a real `max_stack_size` census
        // rather than a guess here.
        let max = u32::from(lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE);

        loop {
            if remaining.count == 0 {
                return (written, None);
            }
            let Some(slot) = self.slot_with_remaining_space(&remaining, max).or_else(|| self.free_slot()) else {
                return (written, Some(remaining));
            };
            let in_slot = self.slots[slot].as_ref().map_or(0, |s| s.count);
            let to_add = remaining.count.min(max.saturating_sub(in_slot));
            if to_add == 0 {
                // `addResource` returns the untouched count in this case;
                // looping again would pick the same slot forever.
                return (written, Some(remaining));
            }
            match self.slots[slot].as_mut() {
                Some(existing) => existing.count += to_add,
                None => {
                    let mut fresh = remaining.clone();
                    fresh.count = to_add;
                    self.slots[slot] = Some(fresh);
                }
            }
            remaining.count -= to_add;
            if !written.contains(&slot) {
                written.push(slot);
            }
        }
    }

    /// `Inventory.getSlotWithRemainingSpace` — see [`add`](Self::add)'s doc
    /// comment for the order and why it matters.
    fn slot_with_remaining_space(&self, stack: &ItemStack, max: u32) -> Option<usize> {
        let mergeable = |index: usize| -> bool {
            self.slots
                .get(index)
                .and_then(Option::as_ref)
                .is_some_and(|slot| {
                    slot.item == stack.item
                        && slot.components == stack.components
                        && slot.count < max
                })
        };
        let selected = usize::from(self.selected_hotbar_slot);
        if mergeable(selected) {
            return Some(selected);
        }
        if mergeable(OFFHAND_NATIVE) {
            return Some(OFFHAND_NATIVE);
        }
        (0..ITEMS_SIZE).find(|&index| mergeable(index))
    }

    /// `Inventory.getFreeSlot` — the first empty slot in `items` (`0..36`)
    /// **only**, which is why a fresh stack never reaches armour or the
    /// off-hand.
    fn free_slot(&self) -> Option<usize> {
        (0..ITEMS_SIZE).find(|&index| self.slots[index].is_none())
    }
}

/// The **inverse** of [`player_menu_native_index`]: which window-`0` menu slot a
/// native index appears in, for a server-initiated `container_set_slot`.
///
/// Needed because a pickup writes wherever [`PlayerInventory::add`] decided, and
/// the client must be told in *menu* coordinates. Every existing
/// server-initiated slot write in this crate targets the selected hotbar slot and
/// so could hardcode `selected + 36`; a pickup can land in main storage or the
/// off-hand too, and inverting the table by hand at the call site is how the two
/// directions drift apart.
///
/// `None` for an index with no window-`0` menu slot, which today means only an
/// out-of-range one — all 41 native slots are reachable on the player's own
/// screen.
#[must_use]
pub fn window_zero_menu_slot(native: usize) -> Option<i32> {
    let slot = match native {
        0..=8 => i32::try_from(native).ok()? + 36, // hotbar -> 36..=44
        9..=35 => i32::try_from(native).ok()?,     // main storage, identity
        36 => 8,                                   // feet
        37 => 7,                                   // legs
        38 => 6,                                   // chest
        39 => 5,                                   // head
        OFFHAND_NATIVE => 45,
        _ => return None,
    };
    Some(slot)
}

/// Size of vanilla's `Inventory.items` list — hotbar plus main storage
/// (`Inventory.INVENTORY_SIZE = 36`). Distinct from [`PLAYER_NATIVE_SIZE`]:
/// the five slots past this (armour `36..=39`, off-hand `40`) live in separate
/// vanilla lists, and the difference is load-bearing for
/// [`PlayerInventory::add`] — `getFreeSlot` scans only this range.
const ITEMS_SIZE: usize = 36;

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

/// Where a `container_click`'s menu-slot index lands for a **non-player**
/// window (a furnace/hopper screen, `window_id != 0`) — either inside the
/// block entity's own container slots, or inside the player's standard
/// inventory rows every such menu appends after them.
///
/// Every vanilla non-player container menu builds its player-facing section
/// with `AbstractContainerMenu::addStandardInventorySlots` (e.g.
/// `AbstractFurnaceMenu.java:66`'s `addStandardInventorySlots(inventory, 8,
/// 84)`, `HopperMenu.java:27`'s `addStandardInventorySlots(inventory, 8,
/// 51)`): 27 main-storage slots (native `9..=35`) immediately followed by 9
/// hotbar slots (native `0..=8`) — **never** armour or off-hand, which only
/// `InventoryMenu` (window `0`) exposes. This is the same 36-slot tail
/// [`crate::block_entities::BlockEntity::container_slots`]'s callers append
/// when building a `container_set_content` payload, so the two sides agree
/// on where the boundary falls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerMenuSlot {
    /// An index into the block entity's own slots (`0..container_size`).
    Own(usize),
    /// A native [`PlayerInventory`] index (main storage `9..=35` or hotbar
    /// `0..=8` — never armour/off-hand, per this enum's own doc comment).
    Player(usize),
}

/// Resolves one `container_click` menu-slot index against a `container_size`-
/// slot non-player menu — see [`ContainerMenuSlot`]'s doc comment for the
/// layout this implements. `None` for an index past the end of the standard
/// 36-slot player tail (a malformed or future-layout packet).
#[must_use]
pub fn container_menu_slot(container_size: usize, menu_slot: i32) -> Option<ContainerMenuSlot> {
    let menu_slot = usize::try_from(menu_slot).ok()?;
    if menu_slot < container_size {
        return Some(ContainerMenuSlot::Own(menu_slot));
    }
    match menu_slot - container_size {
        main @ 0..=26 => Some(ContainerMenuSlot::Player(main + 9)),
        hotbar @ 27..=35 => Some(ContainerMenuSlot::Player(hotbar - 27)),
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

    /// Pins [`container_menu_slot`]'s boundary for a 3-slot furnace menu
    /// (`AbstractFurnaceMenu.SLOT_COUNT = 3`): the container's own slots
    /// (`0..3`), then main storage (`3..30` menu -> native `9..35`), then
    /// hotbar (`30..39` menu -> native `0..8`) — the exact table
    /// `AbstractFurnaceMenu.java:63-66` builds.
    #[test]
    fn container_menu_slot_maps_a_furnace_sized_menu() {
        assert_eq!(container_menu_slot(3, 0), Some(ContainerMenuSlot::Own(0)));
        assert_eq!(container_menu_slot(3, 2), Some(ContainerMenuSlot::Own(2)));
        assert_eq!(container_menu_slot(3, 3), Some(ContainerMenuSlot::Player(9)));
        assert_eq!(container_menu_slot(3, 29), Some(ContainerMenuSlot::Player(35)));
        assert_eq!(container_menu_slot(3, 30), Some(ContainerMenuSlot::Player(0)));
        assert_eq!(container_menu_slot(3, 38), Some(ContainerMenuSlot::Player(8)));
        assert_eq!(container_menu_slot(3, 39), None);
    }

    /// Same shape, a 5-slot hopper menu (`HopperMenu.CONTAINER_SIZE = 5`) —
    /// the control that the boundary genuinely moves with `container_size`
    /// rather than being hardcoded to the furnace's `3`.
    #[test]
    fn container_menu_slot_maps_a_hopper_sized_menu() {
        assert_eq!(container_menu_slot(5, 4), Some(ContainerMenuSlot::Own(4)));
        assert_eq!(container_menu_slot(5, 5), Some(ContainerMenuSlot::Player(9)));
        assert_eq!(container_menu_slot(5, 31), Some(ContainerMenuSlot::Player(35)));
        assert_eq!(container_menu_slot(5, 32), Some(ContainerMenuSlot::Player(0)));
        assert_eq!(container_menu_slot(5, 40), Some(ContainerMenuSlot::Player(8)));
        assert_eq!(container_menu_slot(5, 41), None);
    }

    /// [`window_zero_menu_slot`] must be the exact inverse of
    /// [`player_menu_native_index`] over every native slot — the property that
    /// keeps a pickup's `container_set_slot` landing where the pickup actually
    /// wrote.
    ///
    /// A round-trip against the *other* direction rather than a restatement of
    /// the table: restating it is how the two copies drift, and a hand-written
    /// expectation list would be checking this file against itself.
    #[test]
    fn window_zero_menu_slot_inverts_the_menu_to_native_table() {
        for native in 0..PLAYER_NATIVE_SIZE {
            let menu = window_zero_menu_slot(native)
                .unwrap_or_else(|| panic!("native {native} has no window-0 menu slot"));
            assert_eq!(
                player_menu_native_index(menu),
                Some(native),
                "native {native} -> menu {menu} must map back to itself"
            );
        }
        assert_eq!(window_zero_menu_slot(PLAYER_NATIVE_SIZE), None);
    }

    /// Vanilla's `getSlotWithRemainingSpace` order, asserted by **destination**
    /// rather than by "the item arrived".
    ///
    /// Both halves are load-bearing and a naive "first empty slot"
    /// implementation gets both wrong while still putting the item somewhere:
    ///
    /// * a mergeable **selected** slot wins over an earlier mergeable one,
    /// * a mergeable **off-hand** wins over any slot in `0..36`,
    /// * but a *fresh* stack never reaches the off-hand, because `getFreeSlot`
    ///   scans `items` alone.
    #[test]
    fn add_follows_vanillas_selected_then_offhand_then_scan_order() {
        let cobble = |count: u32| ItemStack::new("minecraft:cobblestone".parse().unwrap(), count);

        // Selected beats an earlier mergeable slot.
        let mut inv = PlayerInventory::new();
        inv.set_native(2, Some(cobble(1)));
        inv.set_native(5, Some(cobble(1)));
        assert!(inv.set_selected_hotbar_slot(5));
        let (written, leftover) = inv.add(cobble(1));
        assert_eq!(written, vec![5], "the selected slot is checked first");
        assert!(leftover.is_none());
        assert_eq!(inv.native(5).unwrap().count, 2);
        assert_eq!(inv.native(2).unwrap().count, 1, "slot 2 is untouched");

        // Off-hand beats the 0..36 scan for a *merge*.
        let mut inv = PlayerInventory::new();
        inv.set_native(3, Some(cobble(1)));
        inv.set_native(OFFHAND_NATIVE, Some(cobble(1)));
        let (written, _) = inv.add(cobble(1));
        assert_eq!(
            written,
            vec![OFFHAND_NATIVE],
            "the off-hand is checked before the 0..36 scan"
        );

        // …but never for a *fresh* stack: `getFreeSlot` scans `items` only.
        let mut inv = PlayerInventory::new();
        for native in 0..36 {
            inv.set_native(native, Some(ItemStack::new("minecraft:stone".parse().unwrap(), 64)));
        }
        let (written, leftover) = inv.add(cobble(1));
        assert!(
            written.is_empty(),
            "a full items list must not spill into the off-hand or armour, got {written:?}"
        );
        assert_eq!(
            leftover.map(|s| s.count),
            Some(1),
            "the unplaced remainder is reported so the caller leaves the entity in the world"
        );
        assert!(inv.native(OFFHAND_NATIVE).is_none());
    }

    /// A stack larger than one slot's space overflows into a second slot, and
    /// **both** indices are reported — the property a caller needs to send two
    /// `container_set_slot`s rather than one.
    #[test]
    fn add_reports_every_slot_it_wrote_when_a_stack_overflows() {
        let mut inv = PlayerInventory::new();
        let cobble = |count: u32| ItemStack::new("minecraft:cobblestone".parse().unwrap(), count);
        inv.set_native(0, Some(cobble(60)));
        // 60 in the selected slot takes 4, the remaining 6 start a fresh stack.
        let (written, leftover) = inv.add(cobble(10));
        assert!(leftover.is_none());
        assert_eq!(written, vec![0, 1], "topped up slot 0, then opened slot 1");
        assert_eq!(inv.native(0).unwrap().count, 64);
        assert_eq!(inv.native(1).unwrap().count, 6);
    }
}
