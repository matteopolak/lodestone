//! Server-authoritative player inventory: the server-side inventory model used by
//! container and item-use handlers.
//!
//! [`PlayerInventory`] has 41 native slots (hotbar + main + armour + off-hand):
//!
//! * `items` contains 36 entries — hotbar `0..=8`, main storage `9..=35`.
//! * The equipment-slot mapping adds feet `36`, legs
//!   `37`, chest `38`, head `39`, off-hand `40` (this module does not model
//!   `41`/body or `42`/saddle — those are mount equipment, not a player's own
//!   inventory, and have no menu slot on the player inventory screen at all).
//!
//! The numbering is intentionally independent of client code because this
//! crate is version- and client-free. Keeping both schemes identical lets a
//! `CONTAINER_CLICK` menu-slot index map onto this model — see
//! [`PlayerInventory::apply_menu_slot_change`].
//!
//! # Scope cut: no crafting grid, no armour/tool queries yet
//!
//! The player inventory screen's 2×2 crafting grid and result slot (menu
//! indices `0..=4`) are **not** part of this native inventory model — they
//! live in the menu's scratch crafting container, which this
//! server has no recipe model to resolve a result for. A `CONTAINER_CLICK`
//! that reports a change to one of those menu slots is dropped rather than
//! misapplied (see [`PlayerInventory::apply_menu_slot_change`]'s doc
//! comment) — the same "genuinely different, no data to model it" scope cut
//! `docs/container-cost-screens.md` already documents for the anvil/
//! enchanting-table costs.

use std::collections::{HashMap, HashSet};

use lodestone_entity::equipment::EquipmentSlot;
use lodestone_model::{BundleItemSlot, HotbarSlot, ItemStack, MenuSlot, RecipeBookType};

use crate::crafting::CraftingState;
use lodestone_game::recipe::RecipeBookSettings;

/// Native size of the player's own inventory: hotbar (`0..=8`) + main storage
/// (`9..=35`) + armour (`36..=39`) + off-hand (`40`). The
/// [`PlayerInventory::native`] and [`PlayerInventory::set_native`] methods use
/// this layout.
pub const PLAYER_NATIVE_SIZE: usize = 41;

/// Native index of the off-hand slot in the protocol's player-inventory layout.
/// [`PlayerInventory::native`] uses this index for the off-hand slot, and
/// [`PlayerInventory::set_native`] writes it.
pub const OFFHAND_NATIVE: usize = 40;

/// Number of hotbar slots (vanilla's own selection-size constant).
pub const HOTBAR_SIZE: u8 = HotbarSlot::COUNT;

/// Native index of the boots slot (`EQUIPMENT_SLOT_MAPPING`, see the module doc).
pub const FEET_NATIVE: usize = 36;
/// Native index of the leggings slot.
pub const LEGS_NATIVE: usize = 37;
/// Native index of the chestplate slot.
pub const CHEST_NATIVE: usize = 38;
/// Native index of the helmet slot.
pub const HEAD_NATIVE: usize = 39;

/// Why a host-requested native count change was refused before it could mutate
/// the authoritative inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryMutationError {
    /// The request did not name one of the player's native slots.
    InvalidNativeSlot { index: usize },
    /// The request named a valid but empty native slot.
    EmptyNativeSlot { index: usize },
    /// An occupied item stack cannot be rewritten to an absent count.
    ZeroCount { index: usize },
    /// The stack has components this build cannot preserve across a rewrite.
    UnmodeledComponents { index: usize },
}

/// A player's server-authoritative inventory: [`PLAYER_NATIVE_SIZE`] native
/// slots plus the selected hotbar index (vanilla's own selected-slot field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerInventory {
    slots: Vec<Option<ItemStack>>,
    selected_hotbar_slot: HotbarSlot,
    /// The inventory screen's own 2×2 crafting grid. The menu keeps this in
    /// per-connection scratch space rather than in the native inventory, and
    /// this struct already reaches every caller that needs it.
    crafting: CraftingState,
    /// The cursor stack and in-progress drag the server's container-click state needs
    /// ([`crate::container_click`]). Same argument as `crafting` above: it is
    /// per-connection menu state, and this struct is the per-connection value
    /// every container call site already holds, so it costs no new parameter on
    /// `dispatch_play_packet` (which is at 28).
    click_state: crate::container_click::ClickState,
    /// The 3×3 grid of the crafting **table** this connection currently has open,
    /// if any. `None` when no table menu is open, which is
    /// what makes "is this window a crafting table" answerable without a second
    /// registry: the grid exists exactly while the menu does.
    table_crafting: Option<CraftingState>,
    /// The open workstation's input cells,
    /// if one is open — the same positionless scratch-space rule as
    /// `table_crafting` above: these cells belong to the open menu, not to a
    /// world block or the native inventory, and are discarded on close. Sized
    /// to the open station (`2` for the anvil/grindstone, `3` for smithing) by
    /// [`open_workstation`](Self::open_workstation).
    workstation: Option<Vec<Option<ItemStack>>>,
    /// An open rename field's typed-but-not-yet-taken text. `None` means
    /// "never touched this menu
    /// instance", which is distinct from a touched-but-blank field clearing an
    /// existing custom name — see [`crate::anvil::compute`]'s own `item_name`
    /// doc. Reset by [`open_workstation`](Self::open_workstation), the same "a
    /// new menu instance starts with no typed name" rule
    /// every newly opened menu starts without pending text.
    pending_rename: Option<String>,
    /// An open enchanting table's offer seed — the roll
    /// every offer this table shows is derived from, rerolled after every
    /// successful enchant. Reset to `0` by
    /// [`open_workstation`](Self::open_workstation) and set to a fresh draw by
    /// `crate::server::open_enchanting_screen`'s own caller.
    enchant_seed: i64,
    /// An open loom or stonecutter's `selectedBannerPatternIndex`/
    /// selected recipe index — which offer in the station's selectable list was
    /// most recently picked. `None` means "nothing chosen yet," the same shape
    /// `pending_rename` gives a fresh anvil menu. Reset by
    /// [`open_workstation`](Self::open_workstation), exactly like
    /// `pending_rename`/`enchant_seed`: a new menu instance starts with
    /// nothing selected.
    selected_recipe_index: Option<i32>,
    /// Per-player recipe-book tab settings received from the client.
    recipe_book_settings: RecipeBookSettings,
    /// Recipe-display ids whose join-time highlight this client has already
    /// acknowledged. The ids are session-local, like the corresponding
    /// recipe-book packet, so this belongs to the connection inventory rather
    /// than persistent player data.
    recipe_book_seen: HashSet<i32>,
    /// Menu-index → highlighted bundle-content index, from
    /// bundle-selection input (`crate::container_click`'s
    /// `SelectedBundleIndex`). This struct is the per-connection menu
    /// scratch state that already fills that role for `click_state` and
    /// `workstation`, so it lives here rather than adding a new field to
    /// `dispatch_play_packet`. A missing entry (never selected, or the last
    /// select cleared it with `-1`) reads as "nothing selected," matching
    /// a missing or out-of-range selection fallback.
    selected_bundle: HashMap<MenuSlot, BundleItemSlot>,
}

impl Default for PlayerInventory {
    fn default() -> Self {
        Self {
            slots: vec![None; PLAYER_NATIVE_SIZE],
            selected_hotbar_slot: HotbarSlot::new(0).expect("zero is a valid hotbar slot"),
            crafting: CraftingState::player(),
            click_state: crate::container_click::ClickState::default(),
            table_crafting: None,
            workstation: None,
            pending_rename: None,
            enchant_seed: 0,
            selected_recipe_index: None,
            recipe_book_settings: RecipeBookSettings::default(),
            recipe_book_seen: HashSet::new(),
            selected_bundle: HashMap::new(),
        }
    }
}

impl PlayerInventory {
    /// A fresh, empty inventory with hotbar slot `0` selected.
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

    /// Changes the count of one occupied native stack without discarding any
    /// modeled component data.
    ///
    /// A component-aware serialization boundary must refuse a stack marked by
    /// [`lodestone_model::ItemComponents::has_unmodeled`] before it changes
    /// anything. Reconstructing an item from its key and count would otherwise
    /// silently erase data this process cannot reproduce.
    pub fn set_native_count(
        &mut self,
        index: usize,
        count: u32,
    ) -> Result<(), InventoryMutationError> {
        if count == 0 {
            return Err(InventoryMutationError::ZeroCount { index });
        }
        let Some(slot) = self.slots.get_mut(index) else {
            return Err(InventoryMutationError::InvalidNativeSlot { index });
        };
        let Some(stack) = slot.as_mut() else {
            return Err(InventoryMutationError::EmptyNativeSlot { index });
        };
        if stack.components.has_unmodeled {
            return Err(InventoryMutationError::UnmodeledComponents { index });
        }
        stack.count = count;
        Ok(())
    }

    /// The currently selected hotbar slot, `0..HOTBAR_SIZE`.
    #[must_use]
    pub fn selected_hotbar_slot(&self) -> HotbarSlot {
        self.selected_hotbar_slot
    }

    /// Sets the selected hotbar slot from a held-slot packet.
    /// Returns `false` (no-op) for anything outside `0..HOTBAR_SIZE`,
    /// mirroring vanilla's own selected-slot setter guard
    /// (its own hotbar-slot check throws server-side; here it
    /// degrades to a rejected update instead of a panic/disconnect, matching
    /// this crate's "malformed packet drops the effect, not the connection"
    /// convention — e.g. `WorldAdminState`'s difficulty/game-rule decode).
    pub fn set_selected_hotbar_slot(&mut self, slot: u8) -> bool {
        let Some(slot) = HotbarSlot::new(slot) else {
            return false;
        };
        self.select_hotbar_slot(slot);
        true
    }

    /// Stores a hotbar position already validated at a caller's boundary.
    pub fn select_hotbar_slot(&mut self, slot: HotbarSlot) {
        self.selected_hotbar_slot = slot;
    }

    /// The item in the currently selected hotbar slot.
    #[must_use]
    pub fn selected_item(&self) -> Option<&ItemStack> {
        self.native(self.selected_hotbar_slot.index())
    }

    /// Every combat-relevant equipment slot and the item in it, ready to feed
    /// [`lodestone_entity::equipment::player_combat_stats`].
    ///
    /// This joins worn equipment and the selected hand with the damage pipeline,
    /// so armour and held-item modifiers affect combat statistics.
    ///
    /// The **selected** hotbar slot is what goes in the main hand, not native
    /// slot `0` — a player holding a sword in slot 3 must not punch for `1.0`.
    /// Empty slots are skipped rather than yielded as an empty id, so an
    /// unarmoured player produces an empty iterator and every stat falls back to
    /// the wearer's own base.
    #[must_use]
    pub fn combat_equipment(&self) -> Vec<(EquipmentSlot, &str)> {
        // Native indices per this module's own doc comment: feet 36, legs 37,
        // chest 38, head 39, off-hand 40, main hand = the *selected* hotbar slot.
        let pairs = [
            (EquipmentSlot::MainHand, self.selected_hotbar_slot.index()),
            (EquipmentSlot::OffHand, OFFHAND_NATIVE),
            (EquipmentSlot::Head, HEAD_NATIVE),
            (EquipmentSlot::Chest, CHEST_NATIVE),
            (EquipmentSlot::Legs, LEGS_NATIVE),
            (EquipmentSlot::Feet, FEET_NATIVE),
        ];
        pairs
            .into_iter()
            .filter_map(|(slot, native)| {
                self.native(native).map(|stack| (slot, stack.item.path()))
            })
            .collect()
    }

    /// [`lodestone_entity::equipment::player_combat_stats`] for whatever this
    /// inventory currently has equipped — the one call a damage or attack site
    /// should make, so armour and weapon cannot be fed independently (and one of
    /// them forgotten, which is what kept a flat `1.0` alive next to a verified
    /// armour formula).
    #[must_use]
    pub fn combat_stats(&self) -> lodestone_entity::equipment::PlayerCombatStats {
        lodestone_entity::equipment::player_combat_stats(self.combat_equipment())
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
    /// Menu slots `1..=4` are the 2×2 crafting grid, which has no *native*
    /// index — it is menu scratch space, not part of the native inventory — so
    /// those land in [`crafting`](Self::crafting) instead, and
    /// re-derive the result as they go.
    ///
    /// **Menu slot `0`, the crafting result, is never written from here and
    /// returns `false`**. It is derived by the server from the grid,
    /// and accepting a client's value for it is exactly the hole that let a
    /// container diff mint any item. The caller's job on slot `0` is to push
    /// back the server's own value, not to store the claim.
    ///
    /// Returns whether the slot was recognised, so a caller can log a dropped
    /// entry rather than silently discarding it.
    pub fn apply_menu_slot_change(&mut self, menu_slot: i32, item: Option<ItemStack>) -> bool {
        let Some(menu_slot) = MenuSlot::from_raw(menu_slot) else {
            return false;
        };
        self.apply_menu_slot(menu_slot, item)
    }

    /// Applies a change to an already validated menu slot.
    pub fn apply_menu_slot(&mut self, menu_slot: MenuSlot, item: Option<ItemStack>) -> bool {
        if menu_slot == PLAYER_CRAFT_RESULT_MENU_SLOT {
            return false;
        }
        if let Some(cell) = player_craft_grid_cell(menu_slot) {
            return self.crafting.set_input(cell, item);
        }
        match player_menu_native_index(menu_slot) {
            Some(native) => {
                self.set_native(native, item);
                true
            }
            None => false,
        }
    }

    /// This inventory screen's 2×2 crafting grid and the result the *server*
    /// derived for it.
    #[must_use]
    pub fn crafting(&self) -> &CraftingState {
        &self.crafting
    }

    /// Mutable access to the 2×2 grid — for the recipe-book fill and for
    /// consuming a craft.
    pub fn crafting_mut(&mut self) -> &mut CraftingState {
        &mut self.crafting
    }

    /// This connection's cursor and in-progress drag.
    #[must_use]
    pub fn click_state(&self) -> &crate::container_click::ClickState {
        &self.click_state
    }

    /// Mutable access to the cursor and drag — for
    /// [`crate::container_click::do_click`] and for a menu close, which returns
    /// the cursor to the world.
    pub fn click_state_mut(&mut self) -> &mut crate::container_click::ClickState {
        &mut self.click_state
    }

    /// Records or clears the selected bundle-content index for menu slot `slot`.
    /// `selected < 0` clears it, using `-1` as the no-selection value,
    /// matching [`Self::selected_bundle_item`]'s read side.
    pub fn set_selected_bundle_item(&mut self, slot: i32, selected: i32) {
        let Some(slot) = MenuSlot::from_raw(slot) else {
            return;
        };
        let selected = BundleItemSlot::from_wire(selected);
        self.set_bundle_item_selection(slot, selected);
    }

    /// Stores a bundle selection whose menu slot and non-sentinel item index
    /// were validated at the packet boundary.
    pub fn set_bundle_item_selection(&mut self, slot: MenuSlot, selected: Option<BundleItemSlot>) {
        if let Some(selected) = selected {
            self.selected_bundle.insert(slot, selected);
        } else {
            self.selected_bundle.remove(&slot);
        }
    }

    /// The last selected bundle-content index for menu slot `slot`, if any —
    /// [`crate::container_click::SelectedBundleIndex`]'s read side.
    #[must_use]
    pub fn selected_bundle_item(&self, slot: MenuSlot) -> Option<BundleItemSlot> {
        self.selected_bundle.get(&slot).copied()
    }

    /// Clears every tracked bundle selection — a menu close, mirroring
    /// menu-close scratch-state teardown
    /// ([`Self::click_state_mut`]'s `reset`, [`Self::take_table_crafting`]
    /// and [`Self::take_workstation`] are the same shape).
    pub fn clear_selected_bundle_items(&mut self) {
        self.selected_bundle.clear();
    }

    /// The open crafting **table**'s 3×3 grid, if a table menu is open.
    #[must_use]
    pub fn table_crafting(&self) -> Option<&CraftingState> {
        self.table_crafting.as_ref()
    }

    /// Mutable access to the open table's grid.
    pub fn table_crafting_mut(&mut self) -> Option<&mut CraftingState> {
        self.table_crafting.as_mut()
    }

    /// Opens a fresh 3×3 table grid — called when a crafting-table menu opens.
    pub fn open_table_crafting(&mut self) {
        self.table_crafting = Some(CraftingState::table());
    }

    /// Closes the table grid and returns whatever was in it, so the caller can
    /// give it back to the player. A grid silently discarded on close deletes
    /// items.
    pub fn take_table_crafting(&mut self) -> Vec<ItemStack> {
        self.table_crafting
            .take()
            .map(|grid| grid.inputs().iter().flatten().cloned().collect())
            .unwrap_or_default()
    }

    /// The open anvil/grindstone/smithing-table's input cells, if one is open.
    #[must_use]
    pub fn workstation(&self) -> Option<&[Option<ItemStack>]> {
        self.workstation.as_deref()
    }

    /// Mutable access to the open station's input cells.
    pub fn workstation_mut(&mut self) -> Option<&mut Vec<Option<ItemStack>>> {
        self.workstation.as_mut()
    }

    /// Opens a fresh, empty workstation with `inputs` cells when a workstation
    /// menu opens. Also resets
    /// [`pending_rename`](Self::pending_rename) and
    /// [`enchant_seed`](Self::enchant_seed): a new menu instance starts with
    /// neither a typed name nor a rolled seed.
    pub fn open_workstation(&mut self, inputs: usize) {
        self.workstation = Some(vec![None; inputs]);
        self.pending_rename = None;
        self.enchant_seed = 0;
        self.selected_recipe_index = None;
    }

    /// The open anvil's typed-but-not-yet-taken rename text, if any. See this
    /// struct's own `pending_rename` field doc for what `None` means.
    #[must_use]
    pub fn pending_rename(&self) -> Option<&str> {
        self.pending_rename.as_deref()
    }

    /// Sets (or clears) the open anvil's pending rename text; see
    /// `crate::server`'s consumer for
    /// the validation performed by that consumer.
    pub fn set_pending_rename(&mut self, name: Option<String>) {
        self.pending_rename = name;
    }

    /// The open enchanting table's current offer seed.
    #[must_use]
    pub fn enchant_seed(&self) -> i64 {
        self.enchant_seed
    }

    /// Sets the open enchanting table's offer seed — called once with a fresh
    /// roll when the screen opens, and again after every successful enchant.
    pub fn set_enchant_seed(&mut self, seed: i64) {
        self.enchant_seed = seed;
    }

    /// The open loom/stonecutter's currently selected offer index, if any —
    /// see this struct's own `selected_recipe_index` field doc.
    #[must_use]
    pub fn selected_recipe_index(&self) -> Option<i32> {
        self.selected_recipe_index
    }

    /// Sets the open loom/stonecutter's selected offer index —
    /// menu-button write; see `crate::server`'s consumer for the
    /// validation performed by that consumer.
    pub fn set_selected_recipe_index(&mut self, index: Option<i32>) {
        self.selected_recipe_index = index;
    }

    /// Returns the recipe-book tab settings this connection most recently
    /// supplied, including whether the client has reported any settings yet.
    #[must_use]
    pub fn recipe_book_settings(&self) -> RecipeBookSettings {
        self.recipe_book_settings
    }

    /// Folds one inbound recipe-book settings update into this inventory's
    /// per-player state. The protocol layer validates the wire ordinal before
    /// calling this method, so the canonical enum is exhaustive here.
    pub fn set_recipe_book_settings(
        &mut self,
        book_type: RecipeBookType,
        open: bool,
        filtering: bool,
    ) {
        let settings = lodestone_model::RecipeBookTypeSettings { open, filtering };
        match book_type {
            RecipeBookType::Crafting => self.recipe_book_settings.crafting = settings,
            RecipeBookType::Furnace => self.recipe_book_settings.furnace = settings,
            RecipeBookType::BlastFurnace => self.recipe_book_settings.blast_furnace = settings,
            RecipeBookType::Smoker => self.recipe_book_settings.smoker = settings,
        }
        self.recipe_book_settings.reported = true;
    }

    /// Whether the server should mark this recipe-book entry as new in this
    /// connection's next book snapshot. Callers validate `recipe_index` against
    /// the book they are about to encode; this state only answers whether a
    /// valid entry remains unacknowledged.
    #[must_use]
    pub fn recipe_book_entry_is_highlighted(&self, recipe_index: i32) -> bool {
        !self.recipe_book_seen.contains(&recipe_index)
    }

    /// Folds a validated recipe-book seen acknowledgement into this connection.
    /// Repeated packets are idempotent because the client can populate the same
    /// button more than once while its recipe panel remains open.
    pub fn mark_recipe_book_entry_seen(&mut self, recipe_index: i32) {
        self.recipe_book_seen.insert(recipe_index);
    }

    /// Closes the open workstation and returns whatever was in it, so the
    /// caller can give it back to the player — same "do not silently delete
    /// items on close" story as [`take_table_crafting`](Self::take_table_crafting).
    pub fn take_workstation(&mut self) -> Vec<ItemStack> {
        self.workstation
            .take()
            .map(|cells| cells.into_iter().flatten().collect())
            .unwrap_or_default()
    }

    /// Adds `stack` to this inventory using merge-before-empty-slot order and
    /// returns **every native index this call wrote** plus
    /// whatever could not be fitted.
    ///
    /// This is what item pickup credits into. The returned index
    /// list is the point: the caller has to tell the client about each slot it
    /// touched with its own `container_set_slot`, and a pickup that overflows
    /// into a second slot writes *two*.
    ///
    /// # The destination order is not "first empty slot", and getting it wrong
    /// is invisible
    ///
    /// The merge pass searches for a **mergeable** slot in this exact order and
    /// requires the candidate to be non-empty, so this pass only ever tops up
    /// an existing stack:
    ///
    /// 1. the **selected hotbar slot** (`this.selected`),
    /// 2. the **off-hand** (native `40`),
    /// 3. natives `0..36` in ascending order (hotbar, then main storage).
    ///
    /// Only if that finds nothing does the empty-slot pass place a fresh stack —
    /// it scans **`items` alone, `0..36`**. So a fresh stack can never land in
    /// the off-hand or in an armour slot, while a *merge* into the off-hand is
    /// entirely normal. Both halves are needed: picking "first empty slot"
    /// unconditionally still produces an item in the inventory, so the naive
    /// version passes any test that only asks whether the pickup arrived.
    ///
    /// Returns `(written natives, leftover)`. `leftover` is `None` when the
    /// whole stack fitted; a full inventory returns the unplaced remainder so
    /// the caller can leave the item entity in the world rather than deleting
    /// it — the item entity is removed only when this call consumes everything.
    pub fn add(&mut self, stack: ItemStack) -> (Vec<usize>, Option<ItemStack>) {
        let mut remaining = stack;
        let mut written = Vec::new();
        // `max_stack_size` is per-item; this
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

    /// Removes **one** item matching `predicate` from `items` (`0..36`) and returns
    /// it as a single-count stack — the take side of a recipe-book fill
    /// by scanning native slots and splitting one item from the first match.
    ///
    /// Scans hotbar then main storage in native order, which is vanilla's own
    /// `items` order. Armour and the off-hand are excluded, exactly as
    /// [`add`](Self::add)'s empty-slot pass: a recipe never consumes the
    /// boots you are wearing.
    pub fn take_matching(&mut self, predicate: impl Fn(&ItemStack) -> bool) -> Option<ItemStack> {
        let index = (0..ITEMS_SIZE).find(|&index| {
            self.slots[index]
                .as_ref()
                .is_some_and(|stack| stack.count > 0 && predicate(stack))
        })?;
        let stack = self.slots[index].as_mut()?;
        let mut one = stack.clone();
        one.count = 1;
        if stack.count <= 1 {
            self.slots[index] = None;
        } else {
            stack.count -= 1;
        }
        Some(one)
    }

    /// How many of `item` the player holds across `items` (`0..36`, hotbar +
    /// main storage — armour and the off-hand are excluded, matching
    /// [`add`](Self::add)/[`take_matching`](Self::take_matching)'s own
    /// range), by resource key string.
    #[must_use]
    pub fn count_of(&self, item: &str) -> u32 {
        self.slots[..ITEMS_SIZE]
            .iter()
            .flatten()
            .filter(|stack| stack.item.to_string() == item)
            .map(|stack| stack.count)
            .sum()
    }

    /// Removes exactly `count` of `item` from across `items` (`0..36`),
    /// refusing (and changing nothing) if the player does not hold enough —
    /// the villager-trade cost check
    /// ([`crate::server::attempt_villager_trade`]'s own doc explains why this
    /// scans the whole inventory rather than two fixed payment slots).
    /// Scans in native order, draining a stack fully before moving to the
    /// next, so a cost spread across several partial stacks is satisfied
    /// correctly.
    pub fn consume(&mut self, item: &str, count: u32) -> Option<()> {
        if self.count_of(item) < count {
            return None;
        }
        let mut remaining = count;
        for slot in &mut self.slots[..ITEMS_SIZE] {
            if remaining == 0 {
                break;
            }
            if let Some(stack) = slot
                && stack.item.to_string() == item
            {
                let take = remaining.min(stack.count);
                stack.count -= take;
                remaining -= take;
                if stack.count == 0 {
                    *slot = None;
                }
            }
        }
        Some(())
    }

    /// Finds a mergeable slot; see [`add`](Self::add)'s doc comment for the
    /// order and why it matters.
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

    /// Finds the first empty slot in the main `0..36` range **only**, which is why
    /// a fresh stack never reaches armour or the
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

/// Size of the hotbar plus main-storage range (`36`). Distinct from
/// [`PLAYER_NATIVE_SIZE`]:
/// the five slots past this (armour `36..=39`, off-hand `40`) live in separate
/// vanilla lists, and the difference is load-bearing for
/// [`PlayerInventory::add`] — the empty-slot pass scans only this range.
const ITEMS_SIZE: usize = 36;

/// The menu-index → native-index mapping for the player's own inventory
/// screen (window `0`) — see [`PlayerInventory::apply_menu_slot_change`]'s
/// doc comment for the table this implements.
/// Menu slot of the player inventory screen's crafting **result**
/// (`0`). Server-derived; never client-writable.
pub const PLAYER_CRAFT_RESULT_MENU_SLOT: MenuSlot = MenuSlot::from_index(0).expect("zero fits a menu slot");

/// The 2×2 grid cell a player-inventory menu slot addresses, if any.
///
/// The player-inventory menu lays the grid out as menu slots `1..=4` in row-major
/// order immediately after the result, so cell index is `menu_slot - 1`.
#[must_use]
pub fn player_craft_grid_cell(menu_slot: MenuSlot) -> Option<usize> {
    match menu_slot.index() {
        1..=4 => Some(menu_slot.index() - 1),
        _ => None,
    }
}

fn player_menu_native_index(menu_slot: MenuSlot) -> Option<usize> {
    match menu_slot.index() {
        5 => Some(39), // head
        6 => Some(38), // chest
        7 => Some(37), // legs
        8 => Some(36), // feet
        9..=35 => Some(menu_slot.index()),
        36..=44 => Some(menu_slot.index() - 36),
        45 => Some(OFFHAND_NATIVE),
        _ => None,
    }
}

/// Non-player menu layout lives in [`crate::container_click::MenuLayout::container`],
/// which describes the 27-main + 9-hotbar tail and answers
/// `may_place`/`max_stack_size` for it.
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
    fn recipe_book_settings_fold_into_the_player_inventory() {
        let mut inv = PlayerInventory::new();
        assert!(!inv.recipe_book_settings().reported);

        inv.set_recipe_book_settings(RecipeBookType::BlastFurnace, true, false);
        let settings = inv.recipe_book_settings();
        assert!(settings.reported);
        assert!(settings.blast_furnace.open);
        assert!(!settings.blast_furnace.filtering);
        assert!(!settings.crafting.open);

        inv.set_recipe_book_settings(RecipeBookType::BlastFurnace, false, true);
        let settings = inv.recipe_book_settings();
        assert!(!settings.blast_furnace.open);
        assert!(settings.blast_furnace.filtering);
    }

    #[test]
    fn recipe_book_seen_fold_clears_only_that_entries_highlight() {
        let mut inv = PlayerInventory::new();
        assert!(inv.recipe_book_entry_is_highlighted(12));
        assert!(inv.recipe_book_entry_is_highlighted(13));

        inv.mark_recipe_book_entry_seen(12);
        assert!(!inv.recipe_book_entry_is_highlighted(12));
        assert!(inv.recipe_book_entry_is_highlighted(13));

        inv.mark_recipe_book_entry_seen(12);
        assert!(
            !inv.recipe_book_entry_is_highlighted(12),
            "a repeated seen packet must not restore a highlight"
        );
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

    #[test]
    fn count_mutation_preserves_modeled_components_and_refuses_unmodeled_ones() {
        let mut inv = PlayerInventory::new();
        let mut modeled = stack("minecraft:diamond_pickaxe", 1);
        modeled.components.damage = Some(17);
        inv.set_native(4, Some(modeled));

        assert_eq!(inv.set_native_count(4, 3), Ok(()));
        let updated = inv.native(4).expect("modeled stack remains present");
        assert_eq!(updated.count, 3);
        assert_eq!(updated.components.damage, Some(17));

        let mut partial = stack("minecraft:written_book", 1);
        partial.components.has_unmodeled = true;
        inv.set_native(5, Some(partial));
        assert_eq!(
            inv.set_native_count(5, 2),
            Err(InventoryMutationError::UnmodeledComponents { index: 5 })
        );
        assert_eq!(inv.native(5).expect("partial stack remains").count, 1);
        assert_eq!(
            inv.set_native_count(PLAYER_NATIVE_SIZE, 2),
            Err(InventoryMutationError::InvalidNativeSlot {
                index: PLAYER_NATIVE_SIZE
            })
        );
    }

    /// Pins every entry of the menu→native table against the documented player
    /// layout (mirrored from `lodestone-game`'s own client
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

    /// Menu slots `1..=4` reach the crafting grid, never a native slot, and menu
    /// slot `0` (the result) is refused outright — a client's claimed result is
    /// the mint-anything hole, so the only value that slot ever
    /// holds is the one the server derived.
    #[test]
    fn crafting_grid_menu_slots_reach_the_grid_and_the_result_is_refused() {
        let mut inv = PlayerInventory::new();
        let before = inv.clone_slots_for_test();
        assert!(
            !inv.apply_menu_slot_change(0, Some(stack("minecraft:diamond_block", 1))),
            "the result slot is not client-writable"
        );
        assert!(inv.crafting().result().is_none());

        for menu_slot in 1..=4 {
            assert!(
                inv.apply_menu_slot_change(menu_slot, Some(stack("minecraft:oak_planks", 1))),
                "menu slot {menu_slot} is a grid cell"
            );
        }
        assert_eq!(
            inv.clone_slots_for_test(),
            before,
            "grid cells must not mutate any native slot"
        );
        // And the server derived the result itself, from the grid it now holds.
        assert_eq!(
            inv.crafting().result().map(|r| r.item.to_string()),
            Some("minecraft:crafting_table".to_string())
        );
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
                player_menu_native_index(MenuSlot::from_raw(menu).expect("menu mapping is non-negative")),
                Some(native),
                "native {native} -> menu {menu} must map back to itself"
            );
        }
        assert_eq!(window_zero_menu_slot(PLAYER_NATIVE_SIZE), None);
    }

    /// The merge-before-empty destination order, asserted by **destination**
    /// rather than by "the item arrived".
    ///
    /// Both halves are load-bearing and a naive "first empty slot"
    /// implementation gets both wrong while still putting the item somewhere:
    ///
    /// * a mergeable **selected** slot wins over an earlier mergeable one,
    /// * a mergeable **off-hand** wins over any slot in `0..36`,
    /// * but a *fresh* stack never reaches the off-hand, because the empty-slot pass
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

        // …but never for a *fresh* stack: the empty-slot pass scans main storage only.
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
