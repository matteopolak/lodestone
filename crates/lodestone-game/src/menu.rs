//! Menus: an ordered slot list layered over one or more containers.
//!
//! This is where the plan's "slot numbering differs between the player
//! inventory and an open container" lives. The **player inventory** is stored
//! once, in *native* Minecraft indexing (`0..=8` hotbar, `9..=35` main storage,
//! `36..=39` armour as feet/legs/chest/head, `40` off-hand). A [`Menu`] then
//! projects a *menu-slot* ordering over it — and the projection is different for
//! the player's own inventory screen versus an open chest. Because both the
//! menu slots and the number-key swap address the *same* backing container, a
//! swap moves the very stack the menu is displaying, exactly as vanilla's
//! shared `Slot`/`Container` aliasing does.
//!
//! Menu-slot layout of the **player inventory screen** (`InventoryMenu`):
//!
//! | menu slot | meaning        | native index |
//! |-----------|----------------|--------------|
//! | 0         | crafting result| result[0]    |
//! | 1..=4     | crafting grid  | grid[0..=3]  |
//! | 5..=8     | armour H/C/L/F | 39/38/37/36  |
//! | 9..=35    | main storage   | 9..=35       |
//! | 36..=44   | hotbar         | 0..=8        |
//! | 45        | off-hand       | 40           |
//!
//! Menu-slot layout of a **generic container** (`ChestMenu`, `n` container
//! slots): `0..n` container, then `n..n+27` main storage, then the last 9 are
//! the hotbar. The player's armour and off-hand are not shown but remain
//! swap-addressable through their native indices.

use crate::{
    container::{Container, EquipmentSlot, Slot, SlotKind},
    item::ItemStack,
    recipe::CraftingGrid,
};

/// Which menu layout a [`Menu`] uses, selecting the quick-move regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    /// The player's own inventory screen with 2×2 crafting and armour.
    Player,
    /// A generic container with `container_size` leading slots then the player
    /// main+hotbar.
    Generic {
        /// Number of container slots preceding the player inventory.
        container_size: usize,
    },
}

/// Where a menu's crafting grid and result live, in **menu-slot** indices.
///
/// Both of vanilla's grid menus put the result first and the grid immediately
/// after it (`InventoryMenu`: result 0, 2×2 grid 1..=4; `CraftingMenu`: result
/// 0, 3×3 grid 1..=9), so one descriptor covers both. It is carried on the
/// [`Menu`] rather than encoded in [`MenuKind`] deliberately: a crafting table's
/// *quick-move regions* are the generic-container ones, only its slot **kinds**
/// differ, and those already live on [`Slot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CraftLayout {
    /// Menu index of the take-only result slot.
    pub result_slot: usize,
    /// Menu index of the grid's top-left cell; the grid occupies
    /// `first_input..first_input + width * height` in row-major order.
    pub first_input: usize,
    /// Grid width in cells.
    pub width: usize,
    /// Grid height in cells.
    pub height: usize,
}

impl CraftLayout {
    /// Number of input cells.
    #[must_use]
    pub fn cell_count(&self) -> usize {
        self.width * self.height
    }

    /// Whether `menu_index` is one of the grid's input cells.
    #[must_use]
    pub fn is_input(&self, menu_index: usize) -> bool {
        menu_index >= self.first_input && menu_index < self.first_input + self.cell_count()
    }
}

/// Native size of the player inventory container (hotbar+main+armour+offhand).
pub const PLAYER_NATIVE_SIZE: usize = 41;
/// Native index of the off-hand slot within the player inventory.
pub const OFFHAND_NATIVE: usize = 40;
/// Sentinel slot index for a click outside any slot (drop).
pub const OUTSIDE_SLOT: i32 = -999;

/// An ordered slot list over backing containers, with a carried cursor stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    kind: MenuKind,
    containers: Vec<Container>,
    slots: Vec<Slot>,
    carried: Option<ItemStack>,
    /// Index into `containers` of the player inventory, for native swap
    /// addressing.
    player_container: usize,
    /// Where the crafting grid and result sit, for menus that have one.
    craft: Option<CraftLayout>,
    /// Server-synchronised state id; bumped on every predicted mutation.
    state_id: u32,
    /// Drag (quick-craft) accumulator state; see [`crate::click`].
    quick_craft_status: i32,
    quick_craft_type: i32,
    quick_craft_slots: Vec<usize>,
}

impl Menu {
    /// Builds the player inventory screen menu.
    #[must_use]
    pub fn player() -> Self {
        // container 0: player inventory (native indexing)
        // container 1: 2x2 crafting grid
        // container 2: crafting result
        let containers = vec![
            Container::new(PLAYER_NATIVE_SIZE),
            Container::new(4),
            Container::new(1),
        ];
        let mut slots = Vec::with_capacity(46);
        slots.push(Slot::of(2, 0, SlotKind::Output)); // 0 result
        for i in 0..4 {
            slots.push(Slot::of(1, i, SlotKind::CraftingInput)); // 1..=4 grid
        }
        // 5..=8 armour: head(39), chest(38), legs(37), feet(36)
        let armour = [
            (39usize, EquipmentSlot::Head),
            (38, EquipmentSlot::Chest),
            (37, EquipmentSlot::Legs),
            (36, EquipmentSlot::Feet),
        ];
        for (native, eq) in armour {
            slots.push(Slot::armor(0, native, eq));
        }
        for native in 9..36 {
            slots.push(Slot::normal(0, native)); // 9..=35 main
        }
        for native in 0..9 {
            slots.push(Slot::normal(0, native)); // 36..=44 hotbar
        }
        slots.push(Slot::of(0, OFFHAND_NATIVE, SlotKind::Offhand)); // 45 offhand
        Self {
            kind: MenuKind::Player,
            containers,
            slots,
            carried: None,
            player_container: 0,
            craft: Some(CraftLayout {
                result_slot: 0,
                first_input: 1,
                width: 2,
                height: 2,
            }),
            state_id: 0,
            quick_craft_status: 0,
            quick_craft_type: 0,
            quick_craft_slots: Vec::new(),
        }
    }

    /// Builds a crafting-table menu: a take-only result slot, a `width × height`
    /// input grid, then the player's main storage and hotbar.
    ///
    /// Vanilla's `CraftingMenu` is `0` result, `1..=9` grid, `10..=36` main,
    /// `37..=45` hotbar — **positionally identical** to
    /// [`generic`](Self::generic) with a container size of `1 + width * height`,
    /// which is why the [`MenuKind`] stays `Generic`: the size a
    /// `container_set_content` implies (`items.len() - 36`) and the quick-move
    /// regions are the same. What differs is the slot *kinds*, and getting those
    /// wrong is not cosmetic — with a plain `Normal` slot at index 0 a
    /// shift-click from the player inventory happily deposits into the **result
    /// slot**, and the server then contradicts every prediction that follows.
    #[must_use]
    pub fn crafting(width: usize, height: usize) -> Self {
        let cells = width * height;
        let container_size = cells + 1;
        // container 0: result; container 1: grid; container 2: player inventory.
        let containers = vec![
            Container::new(1),
            Container::new(cells),
            Container::new(PLAYER_NATIVE_SIZE),
        ];
        let mut slots = Vec::with_capacity(container_size + 36);
        slots.push(Slot::of(0, 0, SlotKind::Output));
        for i in 0..cells {
            slots.push(Slot::of(1, i, SlotKind::CraftingInput));
        }
        for native in 9..36 {
            slots.push(Slot::normal(2, native)); // main storage
        }
        for native in 0..9 {
            slots.push(Slot::normal(2, native)); // hotbar
        }
        Self {
            kind: MenuKind::Generic { container_size },
            containers,
            slots,
            carried: None,
            player_container: 2,
            craft: Some(CraftLayout {
                result_slot: 0,
                first_input: 1,
                width,
                height,
            }),
            state_id: 0,
            quick_craft_status: 0,
            quick_craft_type: 0,
            quick_craft_slots: Vec::new(),
        }
    }

    /// Builds a generic container menu with `container_size` leading slots.
    #[must_use]
    pub fn generic(container_size: usize) -> Self {
        // container 0: the opened container; container 1: player inventory.
        let containers = vec![
            Container::new(container_size),
            Container::new(PLAYER_NATIVE_SIZE),
        ];
        let mut slots = Vec::with_capacity(container_size + 36);
        for i in 0..container_size {
            slots.push(Slot::normal(0, i));
        }
        for native in 9..36 {
            slots.push(Slot::normal(1, native)); // main storage
        }
        for native in 0..9 {
            slots.push(Slot::normal(1, native)); // hotbar
        }
        Self {
            kind: MenuKind::Generic { container_size },
            containers,
            slots,
            carried: None,
            player_container: 1,
            craft: None,
            state_id: 0,
            quick_craft_status: 0,
            quick_craft_type: 0,
            quick_craft_slots: Vec::new(),
        }
    }

    /// Returns the menu kind.
    #[must_use]
    pub fn kind(&self) -> MenuKind {
        self.kind
    }

    /// Returns the number of menu slots.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Where this menu's crafting grid and result slot live, if it has one.
    ///
    /// The player inventory screen reports a 2×2 grid at menu slot 1 with its
    /// result at 0; a crafting table reports 3×3. A chest reports `None`.
    #[must_use]
    pub fn craft_layout(&self) -> Option<CraftLayout> {
        self.craft
    }

    /// Snapshots the crafting grid's contents as a [`CraftingGrid`] ready to
    /// match against a [`RecipeBook`](crate::recipe::RecipeBook).
    ///
    /// Returns `None` for menus with no crafting grid. Item **components** are
    /// dropped: the matching model is id-based, matching vanilla's data-driven
    /// ingredients, which are also id/tag based.
    #[must_use]
    pub fn crafting_grid(&self) -> Option<CraftingGrid> {
        let layout = self.craft?;
        let cells = (0..layout.cell_count())
            .map(|i| {
                self.slot_item(layout.first_input + i)
                    .map(|s| s.item().clone())
            })
            .collect();
        Some(CraftingGrid::new(layout.width, layout.height, cells))
    }

    /// Returns the current state id.
    #[must_use]
    pub fn state_id(&self) -> u32 {
        self.state_id
    }

    /// Bumps the state id, mirroring the client incrementing its container
    /// state before sending a click.
    pub fn bump_state(&mut self) -> u32 {
        self.state_id = self.state_id.wrapping_add(1);
        self.state_id
    }

    /// Sets the state id directly (used to align with a server-sent id).
    pub fn set_state_id(&mut self, state_id: u32) {
        self.state_id = state_id;
    }

    /// Returns the [`Slot`] descriptor for a menu index.
    #[must_use]
    pub fn slot(&self, menu_index: usize) -> Option<&Slot> {
        self.slots.get(menu_index)
    }

    /// Returns the stack shown in a menu slot.
    #[must_use]
    pub fn slot_item(&self, menu_index: usize) -> Option<&ItemStack> {
        let slot = self.slots.get(menu_index)?;
        self.containers[slot.container].get(slot.index)
    }

    /// Sets the stack shown in a menu slot, returning the previous contents.
    ///
    /// An empty stack is normalised to `None`.
    pub fn set_slot_item(
        &mut self,
        menu_index: usize,
        stack: Option<ItemStack>,
    ) -> Option<ItemStack> {
        let slot = self.slots.get(menu_index).copied()?;
        self.containers[slot.container].set(slot.index, normalize_opt(stack))
    }

    /// Reads a slot for a click computation, cloning the stack.
    #[must_use]
    pub fn slot_item_cloned(&self, menu_index: usize) -> Option<ItemStack> {
        self.slot_item(menu_index).cloned()
    }

    /// Returns the carried (cursor) stack.
    #[must_use]
    pub fn carried(&self) -> Option<&ItemStack> {
        self.carried.as_ref()
    }

    /// Sets the carried (cursor) stack. An empty stack is normalised to `None`.
    pub fn set_carried(&mut self, stack: Option<ItemStack>) {
        self.carried = normalize_opt(stack);
    }

    /// Returns the player-inventory container index used for native swap
    /// addressing.
    #[must_use]
    pub fn player_container(&self) -> usize {
        self.player_container
    }

    /// Snapshots every menu slot's contents in menu-slot order.
    #[must_use]
    pub fn snapshot(&self) -> Vec<Option<ItemStack>> {
        (0..self.slot_count())
            .map(|i| self.slot_item_cloned(i))
            .collect()
    }

    /// Overwrites menu slots from a snapshot (server-authoritative resync).
    pub fn restore(&mut self, items: &[Option<ItemStack>]) {
        for (i, item) in items.iter().enumerate() {
            if i < self.slot_count() {
                self.set_slot_item(i, item.clone());
            }
        }
    }

    /// Reads the player inventory by *native* index (for number-key swaps).
    #[must_use]
    pub fn player_native(&self, native_index: usize) -> Option<&ItemStack> {
        self.containers[self.player_container].get(native_index)
    }

    /// Writes the player inventory by *native* index, returning the previous
    /// contents. An empty stack is normalised to `None`.
    pub fn set_player_native(
        &mut self,
        native_index: usize,
        stack: Option<ItemStack>,
    ) -> Option<ItemStack> {
        let container = self.player_container;
        self.containers[container].set(native_index, normalize_opt(stack))
    }

    /// Returns the underlying container by menu-container index.
    #[must_use]
    pub fn container(&self, index: usize) -> Option<&Container> {
        self.containers.get(index)
    }

    /// Returns whether a menu slot may accept `stack`.
    #[must_use]
    pub fn may_place(&self, menu_index: usize, stack: &ItemStack) -> bool {
        self.slots
            .get(menu_index)
            .is_some_and(|slot| slot.may_place(stack))
    }

    /// Returns whether a menu slot's item may be taken.
    #[must_use]
    pub fn may_pickup(&self, menu_index: usize) -> bool {
        self.slots.get(menu_index).is_some_and(Slot::may_pickup)
    }

    /// Returns the effective per-slot cap for a stack.
    #[must_use]
    pub fn effective_max(&self, menu_index: usize, stack: &ItemStack) -> i32 {
        self.slots
            .get(menu_index)
            .map_or(0, |slot| slot.effective_max(stack))
    }

    /// Moves a stack into the `[start, end)` menu-slot range, merging into
    /// matching stacks first then filling empties, mirroring vanilla
    /// `AbstractContainerMenu.moveItemStackTo`.
    ///
    /// `moving` is drained in place. Returns whether anything changed.
    pub fn move_item_stack_to(
        &mut self,
        moving: &mut ItemStack,
        start: usize,
        end: usize,
        backwards: bool,
    ) -> bool {
        let mut changed = false;

        if moving.is_stackable() {
            let indices = order(start, end, backwards);
            for i in indices {
                if moving.is_empty() {
                    break;
                }
                let Some(target) = self.slot_item_cloned(i) else {
                    continue;
                };
                if !ItemStack::is_same_item_same_components(moving, &target) {
                    continue;
                }
                let cap = self.effective_max(i, &target);
                let total = target.count() + moving.count();
                if total <= cap {
                    moving.set_count(0);
                    let mut merged = target;
                    merged.set_count(total);
                    self.set_slot_item(i, Some(merged));
                    changed = true;
                } else if target.count() < cap {
                    moving.shrink(cap - target.count());
                    let mut merged = target;
                    merged.set_count(cap);
                    self.set_slot_item(i, Some(merged));
                    changed = true;
                }
            }
        }

        if !moving.is_empty() {
            let indices = order(start, end, backwards);
            for i in indices {
                if self.slot_item(i).is_none() && self.may_place(i, moving) {
                    let cap = self.effective_max(i, moving);
                    let place = moving.count().min(cap);
                    let mut placed = moving.clone();
                    placed.set_count(place);
                    self.set_slot_item(i, Some(placed));
                    moving.shrink(place);
                    changed = true;
                    break;
                }
            }
        }

        changed
    }

    /// Shift-click quick-move of the stack in `menu_index`.
    ///
    /// Returns a *template* copy of the pre-move stack (as vanilla does, so the
    /// caller's repeat loop can detect a re-filling output slot). Returns `None`
    /// when nothing could be moved.
    pub fn quick_move(&mut self, menu_index: usize) -> Option<ItemStack> {
        let original = self.slot_item_cloned(menu_index)?;
        let template = original.clone();
        let mut stack = original;
        let moved = match (self.kind, self.craft) {
            (MenuKind::Player, _) => self.quick_move_player(menu_index, &mut stack),
            (MenuKind::Generic { container_size }, Some(layout)) => {
                self.quick_move_crafting(menu_index, container_size, layout, &mut stack)
            }
            (MenuKind::Generic { container_size }, None) => {
                self.quick_move_generic(menu_index, container_size, &mut stack)
            }
        };
        if !moved {
            return None;
        }
        if stack.count() == template.count() {
            // Nothing actually transferred.
            return None;
        }
        // Write back the (possibly reduced) source stack.
        self.set_slot_item(menu_index, crate::item::normalize(stack));
        Some(template)
    }

    fn quick_move_generic(
        &mut self,
        menu_index: usize,
        container_size: usize,
        stack: &mut ItemStack,
    ) -> bool {
        let total = self.slot_count();
        if menu_index < container_size {
            // container -> player inventory, filling from the back
            self.move_item_stack_to(stack, container_size, total, true)
        } else {
            // player inventory -> container
            self.move_item_stack_to(stack, 0, container_size, false)
        }
    }

    /// Quick-move for a crafting-table menu, mirroring vanilla `CraftingMenu`:
    ///
    /// * result slot → player inventory, **filling from the back**;
    /// * grid cell → player inventory, forwards;
    /// * player inventory → the **grid** (`first_input..`), never the result.
    ///
    /// Note the last case: the destination is the grid range, not the whole
    /// container range. Routing it through [`quick_move_generic`] would aim at
    /// `0..container_size`, which includes the result slot — harmless only
    /// because `Slot::may_place` rejects an [`Output`](SlotKind::Output) slot,
    /// and silently wrong the moment that slot kind is lost.
    fn quick_move_crafting(
        &mut self,
        menu_index: usize,
        container_size: usize,
        layout: CraftLayout,
        stack: &mut ItemStack,
    ) -> bool {
        let total = self.slot_count();
        if menu_index == layout.result_slot {
            self.move_item_stack_to(stack, container_size, total, true)
        } else if layout.is_input(menu_index) {
            self.move_item_stack_to(stack, container_size, total, false)
        } else {
            let grid_end = layout.first_input + layout.cell_count();
            if self.move_item_stack_to(stack, layout.first_input, grid_end, false) {
                return true;
            }
            // Grid full: fall back to the main-storage ↔ hotbar hop vanilla does.
            let hotbar_start = total.saturating_sub(9);
            if menu_index < hotbar_start {
                self.move_item_stack_to(stack, hotbar_start, total, false)
            } else {
                self.move_item_stack_to(stack, container_size, hotbar_start, false)
            }
        }
    }

    fn quick_move_player(&mut self, menu_index: usize, stack: &mut ItemStack) -> bool {
        // Regions: 9..=35 main (menu 9..36 hotbar? no) — the *menu* layout is
        // result0, craft1-4, armour5-8, main9-35, hotbar36-44, offhand45.
        match menu_index {
            0 => self.move_item_stack_to(stack, 9, 45, true), // result -> inv, reversed
            1..=8 => self.move_item_stack_to(stack, 9, 45, false), // craft/armour -> inv
            9..=35 => {
                // main storage: try armour/offhand auto-equip first, else hotbar
                if let Some(target_menu) = self.empty_equip_target(stack) {
                    self.move_item_stack_to(stack, target_menu, target_menu + 1, false)
                } else {
                    self.move_item_stack_to(stack, 36, 45, false)
                }
            }
            36..=44 => {
                if let Some(target_menu) = self.empty_equip_target(stack) {
                    self.move_item_stack_to(stack, target_menu, target_menu + 1, false)
                } else {
                    self.move_item_stack_to(stack, 9, 36, false)
                }
            }
            _ => self.move_item_stack_to(stack, 9, 45, false),
        }
    }

    /// Returns the menu-slot index of the empty armour/off-hand slot a stack
    /// should auto-equip into, based on its `minecraft:equippable` component.
    /// Returns the menu-slot index of the empty armour/off-hand slot a stack
    /// should auto-equip into, based on its `minecraft:equippable` component.
    fn empty_equip_target(&self, stack: &ItemStack) -> Option<usize> {
        let eq = crate::container::equippable_slot(stack)?;
        let menu_index = match eq {
            EquipmentSlot::Head => 5,
            EquipmentSlot::Chest => 6,
            EquipmentSlot::Legs => 7,
            EquipmentSlot::Feet => 8,
            EquipmentSlot::Offhand => 45,
        };
        if self.slot_item(menu_index).is_none() {
            Some(menu_index)
        } else {
            None
        }
    }

    // --- Drag (quick-craft) state, driven by `crate::click`. ---

    pub(crate) fn quick_craft_status(&self) -> i32 {
        self.quick_craft_status
    }

    pub(crate) fn set_quick_craft_status(&mut self, status: i32) {
        self.quick_craft_status = status;
    }

    pub(crate) fn quick_craft_type(&self) -> i32 {
        self.quick_craft_type
    }

    pub(crate) fn set_quick_craft_type(&mut self, kind: i32) {
        self.quick_craft_type = kind;
    }

    pub(crate) fn quick_craft_slots(&self) -> &[usize] {
        &self.quick_craft_slots
    }

    pub(crate) fn push_quick_craft_slot(&mut self, menu_index: usize) {
        if !self.quick_craft_slots.contains(&menu_index) {
            self.quick_craft_slots.push(menu_index);
        }
    }

    pub(crate) fn reset_quick_craft(&mut self) {
        self.quick_craft_status = 0;
        self.quick_craft_slots.clear();
    }
}

impl Slot {
    fn armor(container: usize, index: usize, eq: EquipmentSlot) -> Self {
        let mut slot = Slot::of(container, index, SlotKind::Armor(eq));
        slot.max_stack_size = 1;
        slot
    }
}

fn order(start: usize, end: usize, backwards: bool) -> Vec<usize> {
    if backwards {
        (start..end).rev().collect()
    } else {
        (start..end).collect()
    }
}

fn normalize_opt(stack: Option<ItemStack>) -> Option<ItemStack> {
    stack.and_then(crate::item::normalize)
}
