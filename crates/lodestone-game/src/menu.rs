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
/// Menu index of the 2×2 crafting result on the player's own inventory screen
/// (vanilla `InventoryMenu.RESULT_SLOT`).
pub const PLAYER_RESULT_SLOT: usize = 0;
/// Native index of the off-hand slot within the player inventory.
pub const OFFHAND_NATIVE: usize = 40;
/// Sentinel slot index for a click outside any slot (drop).
pub const OUTSIDE_SLOT: i32 = -999;

/// The empty-slot sprites the player inventory declares, from
/// `InventoryMenu.java:29-33`.
///
/// **These are the 26.2 identifiers, and they are not what the pre-1.21.2 name
/// suggests.** `EMPTY_ARMOR_SLOT_HELMET` is the *Java constant's* name; its value
/// is the sprite path `container/slot/helmet`. There is no `empty_armor_slot_*`
/// texture anywhere in a 26.2 jar — interrogated, not assumed:
/// `unzip -l client.jar | grep -i empty` returns nothing under
/// `gui/sprites/**`. The Rust constant names below keep vanilla's spelling so the
/// mapping to the decompile is one grep, and the values are the real paths.
///
/// Relative to `gui/sprites/`, i.e.
/// `assets/minecraft/textures/gui/sprites/container/slot/helmet.png`, all 16x16.
/// See [`crate::container::Slot::no_item_icon`].
pub const EMPTY_ARMOR_SLOT_HELMET: &str = "container/slot/helmet";
/// See [`EMPTY_ARMOR_SLOT_HELMET`].
pub const EMPTY_ARMOR_SLOT_CHESTPLATE: &str = "container/slot/chestplate";
/// See [`EMPTY_ARMOR_SLOT_HELMET`].
pub const EMPTY_ARMOR_SLOT_LEGGINGS: &str = "container/slot/leggings";
/// See [`EMPTY_ARMOR_SLOT_HELMET`].
pub const EMPTY_ARMOR_SLOT_BOOTS: &str = "container/slot/boots";
/// See [`EMPTY_ARMOR_SLOT_HELMET`]. The off-hand slot, whose anonymous subclass
/// overrides `getNoItemIcon` (`InventoryMenu.java:68-72`).
pub const EMPTY_ARMOR_SLOT_SHIELD: &str = "container/slot/shield";

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
        // 45 offhand. Vanilla builds this as a plain `Slot` with an anonymous
        // subclass overriding `getNoItemIcon` (`InventoryMenu.java:64-73`); the
        // shield sprite is the whole of that override.
        slots.push(
            Slot::of(0, OFFHAND_NATIVE, SlotKind::Offhand)
                .with_no_item_icon(EMPTY_ARMOR_SLOT_SHIELD),
        );
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

    /// The **native** player-inventory index a menu slot addresses, or `None`
    /// when the slot belongs to one of this menu's own containers (a crafting
    /// grid cell, a result slot, a chest's own slots).
    ///
    /// This is the same `Slot`-level indirection [`slot_item`](Self::slot_item)
    /// already walks, exposed so a caller can re-address a *menu*-indexed
    /// server update as a *native* one. [`crate::menus::Menus`] needs exactly
    /// that: while a container is open the one player inventory is owned by the
    /// container's menu, so a window-0 `container_set_slot` has to be forwarded
    /// there, and forwarding needs the native index. Deriving it here rather
    /// than from a hand-written window-0 table is what stops the two
    /// numberings drifting (this module's header table is *documentation*; the
    /// `slots` vector is the truth).
    #[must_use]
    pub fn slot_native(&self, menu_index: usize) -> Option<usize> {
        let slot = self.slots.get(menu_index)?;
        (slot.container == self.player_container).then_some(slot.index)
    }

    /// The player-inventory container this menu's player-section slots read
    /// through.
    #[must_use]
    pub fn player_inventory(&self) -> &Container {
        &self.containers[self.player_container]
    }

    /// Moves the player-inventory container **out** of this menu, leaving an
    /// empty one of the same size behind.
    ///
    /// Vanilla has exactly one `Inventory` and every menu's player-section
    /// `Slot`s hold a *reference* into it, so a quick-move inside a chest
    /// mutates the same storage the HUD hotbar reads. Rust will not lend the
    /// same `Container` to two owned [`Menu`]s, so [`crate::menus::Menus`]
    /// models the aliasing as **single ownership that moves**: opening a
    /// container hands the inventory to the container's menu, closing it hands
    /// it back. The point is that at no instant do two copies exist, so there
    /// is nothing to keep in sync and nothing that can diverge — see issue
    /// #373, where the two copies were the whole bug.
    ///
    /// The menu left behind is a husk with respect to its player section: its
    /// slots still resolve, they just read an empty container. Do not read them
    /// — go through [`crate::menus::Menus::player`], which reinstalls the live
    /// inventory into the window-0 view it hands out.
    pub fn take_player_inventory(&mut self) -> Container {
        let index = self.player_container;
        let size = self.containers[index].size();
        std::mem::replace(&mut self.containers[index], Container::new(size))
    }

    /// Installs `inventory` as this menu's player-inventory container,
    /// returning the container it replaced. See
    /// [`take_player_inventory`](Self::take_player_inventory).
    pub fn install_player_inventory(&mut self, inventory: Container) -> Container {
        let index = self.player_container;
        std::mem::replace(&mut self.containers[index], inventory)
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

    /// Vanilla `Slot.onTake`, run after **every** successful removal from a
    /// slot. Only the crafting result slot has behaviour: `ResultSlot.onTake`
    /// removes exactly one item from every occupied grid cell.
    ///
    /// Without this, taking a result leaves the grid full — the ingredients are
    /// never consumed, so the very next prediction contradicts the server on
    /// every grid cell at once. It is the missing half of "slot 0 is take-only":
    /// [`Slot::may_place`] stops you *putting* something there, and this is what
    /// makes *taking* it cost something.
    ///
    /// The consumption is deliberately **recipe-free**: vanilla walks the
    /// positioned craft input and calls `removeItem(cell, 1)` on each non-empty
    /// cell, which needs no knowledge of which recipe matched.
    ///
    /// What *is* skipped is the **remainder** pass — the one that leaves an empty
    /// bucket behind after crafting a cake. Note that this is *not* skipped
    /// because it needs the recipe: `ResultSlot.getRemainingItems` only consults
    /// the recipe on a `ServerLevel`, and on the client falls through to
    /// `CraftingRecipe.defaultCraftingReminder`, which is a plain per-item lookup
    /// (`Item.getCraftingRemainder()`). It is skipped because **we have no
    /// crafting-remainder table** for 26.2's items yet, and inventing one would
    /// be a guess. Until there is one, a remainder-bearing ingredient mispredicts
    /// its cell for one round trip and the server corrects it with a
    /// `container_set_slot`; only ~10 items in the game have a remainder.
    ///
    /// The call sites mirror vanilla's exactly: `doClick`'s pickup and
    /// same-item-pull branches, `Slot.safeTake` (our throw), the swap take, and
    /// the tail of `quickMoveStack`. The both-occupied swap branch also calls it
    /// in vanilla, but is gated on `mayPlace`, which an output slot always
    /// fails, so it can never fire there.
    pub(crate) fn on_take(&mut self, menu_index: usize) {
        let Some(layout) = self.craft else {
            return;
        };
        if menu_index != layout.result_slot {
            return;
        }
        for i in 0..layout.cell_count() {
            let cell = layout.first_input + i;
            let Some(mut stack) = self.slot_item_cloned(cell) else {
                continue;
            };
            stack.shrink(1);
            self.set_slot_item(cell, crate::item::normalize(stack));
        }
    }

    /// Moves a stack into the `[start, end)` menu-slot range, merging into
    /// matching stacks first then filling empties, mirroring vanilla
    /// `AbstractContainerMenu.moveItemStackTo`
    /// (`AbstractContainerMenu.java:636-697`).
    ///
    /// `moving` is drained in place. Returns whether anything changed.
    ///
    /// Three details are transcribed deliberately and all three look like bugs:
    ///
    /// * **The merge pass does not consult `mayPlace`; only the empty-slot pass
    ///   does.** Compare `AbstractContainerMenu.java:647` (no check) with `:682`
    ///   (`target.isEmpty() && slot.mayPlace(itemStack)`). So a shift-click may
    ///   *top up* an existing stack in a slot that would refuse the same item
    ///   arriving into an empty cell. Adding the symmetric check "for
    ///   consistency" changes observable behaviour and desynchronises from the
    ///   server.
    /// * **The merge pass is gated on `moving.isStackable()`** (`:645`), not on
    ///   the per-slot cap. An unstackable item skips merging entirely and goes
    ///   straight to the first empty slot.
    /// * **The merge cap is measured against the stack already in the slot**
    ///   (`slot.getMaxStackSize(target)`, `:650`), while the empty-slot cap is
    ///   measured against the incoming stack (`slot.getMaxStackSize(itemStack)`,
    ///   `:683`). They agree whenever the two are the same item, which the merge
    ///   pass has already established, so this is only a difference in what the
    ///   code *says* — but it is what the source says.
    ///
    /// The empty-slot pass stops after **one** placement (`break` at `:687`),
    /// which is why a caller that must move more than one stack's worth loops.
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
    ///
    /// One vanilla tail is deliberately not modelled: `CraftingMenu` finishes a
    /// result-slot quick move with `player.drop(stack, false)`, throwing any
    /// remainder that would not fit into the inventory onto the floor rather
    /// than leaving it in the result slot. Reaching it needs a result stack
    /// larger than the free space in a 36-slot inventory, which one predicted
    /// craft cannot produce; the server's own loop can, and corrects slot 0 when
    /// it does.
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
        // Write back the (possibly reduced) source stack, then run the slot's
        // take hook — for the result slot that is what consumes the grid, and
        // it is the reason a shift-click can craft at all.
        self.set_slot_item(menu_index, crate::item::normalize(stack));
        self.on_take(menu_index);
        Some(template)
    }

    /// Quick-move for a plain container, mirroring vanilla `ChestMenu`
    /// (`ChestMenu.java:94-109`): container slots go out to the player
    /// inventory **backwards** (hotbar first), player slots come in forwards.
    ///
    /// This one order covers more of the game than its name suggests.
    /// `HopperMenu.java:36-58` and `DispenserMenu.java:45-70` are the same
    /// three lines with a different constant, and `ShulkerBoxMenu.java:40-62`
    /// likewise — so chests, barrels, ender chests, every `generic_9xN`,
    /// hoppers, dispensers, droppers and shulker boxes all share it.
    ///
    /// What it does **not** cover is the menus that route by *item kind* rather
    /// than by region: `AbstractFurnaceMenu.java:87-133` sends smeltables to
    /// slot 0 and fuel to slot 1 before falling back to the main↔hotbar hop,
    /// and `BrewingStandMenu.java:63-99` does the same for blaze powder,
    /// ingredients and potions. Neither is modelled: both need a data table we
    /// do not have (the fuel-value registry and the cooking-recipe input set),
    /// and inventing one would be a guess. A furnace therefore predicts a
    /// shift-click into container slot 0 where vanilla would have picked slot 1
    /// or done nothing, and the server corrects it one round trip later. See
    /// [`crate::menus::Menus`] for where the layout would be selected.
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

    /// Quick-move for a crafting-table menu, mirroring vanilla `CraftingMenu`
    /// (`CraftingMenu.java:107-152`):
    ///
    /// * result slot → player inventory, **filling from the back**;
    /// * grid cell → player inventory, forwards;
    /// * player inventory → the **grid** (`first_input..`), never the result;
    ///   and only if the grid is full does it fall back to the main↔hotbar hop.
    ///
    /// Note the third case: the destination is the grid range, not the whole
    /// container range. Routing it through [`quick_move_generic`] would aim at
    /// `0..container_size`, which includes the result slot — harmless only
    /// because `Slot::may_place` rejects an [`Output`](SlotKind::Output) slot,
    /// and silently wrong the moment that slot kind is lost.
    ///
    /// This is where a crafting table and the player's own 2×2 genuinely
    /// diverge, and the difference is not cosmetic: `CraftingMenu` tries the
    /// grid first (`CraftingMenu.java:123`), so shift-clicking planks in a
    /// crafting table *loads the grid*. `InventoryMenu` has no such branch —
    /// shift-clicking in the player screen never fills the 2×2 — so
    /// [`quick_move_player`](Self::quick_move_player) must not grow one.
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

    /// Quick-move for the player's own inventory screen, mirroring vanilla
    /// `InventoryMenu.quickMoveStack` (`InventoryMenu.java:100-152`).
    ///
    /// The branch **chain** is the specification, not the region list, and its
    /// order is the part that is easy to get wrong. Vanilla's chain is:
    ///
    /// | # | condition | destination |
    /// |---|-----------|-------------|
    /// | 1 | `slotIndex == 0` (result) | `9..45` **backwards** |
    /// | 2 | `1..5` (craft grid) | `9..45` forwards |
    /// | 3 | `5..9` (armour) | `9..45` forwards |
    /// | 4 | item is humanoid armour **and** its armour slot is empty | that one slot |
    /// | 5 | item is off-hand equipment **and** slot 45 is empty | slot 45 |
    /// | 6 | `9..36` (main storage) | `36..45` (hotbar) |
    /// | 7 | `36..45` (hotbar) | `9..36` (main storage) |
    /// | 8 | anything else (slot 45) | `9..45` forwards |
    ///
    /// Two orderings here are load-bearing:
    ///
    /// * **The auto-equip branches (4, 5) come *before* the main/hotbar hop.**
    ///   Shift-clicking a helmet out of main storage equips it; it does *not*
    ///   go to the hotbar. Putting the hop first is the plausible-looking
    ///   arrangement and is wrong.
    /// * **They are reached from *every* source slot at or after 9**, which
    ///   includes menu slot 45, the off-hand. A helmet sitting in the off-hand
    ///   slot shift-clicks up onto the head, not down into storage. The
    ///   previous shape of this function tested for an equip target only inside
    ///   the `9..36` and `36..45` arms, so slot 45 fell through to branch 8 —
    ///   a real divergence, and the reason this reads as vanilla's chain rather
    ///   than as a `match` over regions.
    ///
    /// Only slot 0's `player.drop(stack, false)` tail is not modelled; see
    /// [`quick_move`](Self::quick_move) for why.
    fn quick_move_player(&mut self, menu_index: usize, stack: &mut ItemStack) -> bool {
        // 1 — the result slot empties towards the hotbar first.
        if menu_index == PLAYER_RESULT_SLOT {
            return self.move_item_stack_to(stack, 9, 45, true);
        }
        // 2, 3 — crafting grid (1..5) and armour (5..9) fall out into storage.
        if menu_index < 9 {
            return self.move_item_stack_to(stack, 9, 45, false);
        }
        // 4, 5 — auto-equip, from any source at or after 9 including slot 45.
        if let Some(target) = self.empty_equip_target(stack) {
            return self.move_item_stack_to(stack, target, target + 1, false);
        }
        // 6, 7, 8 — the main-storage <-> hotbar hop, off-hand out to storage.
        match menu_index {
            9..36 => self.move_item_stack_to(stack, 36, 45, false),
            36..45 => self.move_item_stack_to(stack, 9, 36, false),
            _ => self.move_item_stack_to(stack, 9, 45, false),
        }
    }

    /// Returns the menu-slot index of the empty armour/off-hand slot a stack
    /// should auto-equip into, i.e. vanilla's branches 4 and 5 of
    /// `InventoryMenu.quickMoveStack` (`InventoryMenu.java:120-128`).
    ///
    /// Vanilla derives the position from `player.getEquipmentSlotForItem`, which
    /// is `itemStack.get(DataComponents.EQUIPPABLE).slot()`
    /// (`LivingEntity.java:3881-3884`), and maps it to a menu index as
    /// `8 - eqSlot.getIndex()` — head 3 → 5, chest 2 → 6, legs 1 → 7, feet 0 → 8
    /// — with the off-hand at 45. That is the mapping below.
    ///
    /// One caveat for whoever wires the census: vanilla gates branch 4 on
    /// `eqSlot.getType() == EquipmentSlot.Type.HUMANOID_ARMOR`
    /// (`InventoryMenu.java:120`), which excludes `BODY` — wolf and horse
    /// armour. [`crate::container::EquipmentSlot::from_name`] currently folds
    /// `"body"` into [`Chest`](EquipmentSlot::Chest), so once the component is
    /// populated a wolf-armour shift-click would try to equip a chestplate slot.
    /// `"body"` needs its own variant (or to map to `None`) before that happens.
    ///
    /// # This is currently unreachable in live play
    ///
    /// `minecraft:equippable` is a **prototype** component: like
    /// `minecraft:tool` (see [`lodestone_model::ToolPatch`]'s docs and
    /// `docs/tool-mining.md`), vanilla puts it in the item's built-in component
    /// map, so a clientbound stack — which carries only the *patch* — never
    /// mentions it. Nothing in the tree writes it, so
    /// [`crate::container::equippable_slot`] returns `None` for every stack that
    /// came off the wire and this function always returns `None`.
    ///
    /// The same absence disables [`Slot::may_place`] for an
    /// [`Armor`](SlotKind::Armor) slot outright, so no click of any kind can
    /// currently put armour on. The fix is an item→equippable census in the
    /// version crate, exactly like `generated/tools.rs`; it is not a change to
    /// the wire decoder, because the wire never carries the component. Until it
    /// lands, both this branch and armour placement are dead code and the
    /// tests below build the component by hand to exercise them.
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

    /// The slots this menu has accumulated from `ADD` packets during a drag —
    /// vanilla's `AbstractContainerMenu.quickcraftSlots`.
    ///
    /// Public so the **screen's** paint set can be checked against it: the two
    /// are grown independently (see [`can_drag_place_at`](Self::can_drag_place_at))
    /// and their sizes are the divisors for the previewed split and the real
    /// distribution respectively, so a drift between them is a wrong number on
    /// screen. Empty except between a `START` and its `END`.
    #[must_use]
    pub fn quick_craft_slots(&self) -> &[usize] {
        &self.quick_craft_slots
    }

    /// Records a slot painted by an in-progress drag, **de-duplicating**.
    ///
    /// Vanilla's accumulator is `Set<Slot> quickcraftSlots = Sets.newHashSet()`
    /// (`AbstractContainerMenu.java:62`) and the paint site is a bare
    /// `.add(slot)` (`:358`), so dragging back and forth across one slot records
    /// it once. That set's `size()` is then the divisor for an even split
    /// (`:386`), so a `Vec` that pushed duplicates would divide by too large a
    /// number and under-fill every slot — the classic off-by-N. The order is
    /// kept insertion-stable here where vanilla's is a hash order; that is safe
    /// because the per-slot amount is `count / size`, a constant, and the loop
    /// never mutates the cursor it reads (`:378`), so no ordering is observable.
    pub(crate) fn push_quick_craft_slot(&mut self, menu_index: usize) {
        if !self.quick_craft_slots.contains(&menu_index) {
            self.quick_craft_slots.push(menu_index);
        }
    }

    /// Vanilla `resetQuickCraft` (`AbstractContainerMenu.java:718-721`): clears
    /// the status and the painted set, but deliberately **not**
    /// `quick_craft_type`, which the single-slot degradation path reads back
    /// after the reset (`:364-365`).
    pub(crate) fn reset_quick_craft(&mut self) {
        self.quick_craft_status = 0;
        self.quick_craft_slots.clear();
    }
}

impl Slot {
    fn armor(container: usize, index: usize, eq: EquipmentSlot) -> Self {
        let mut slot = Slot::of(container, index, SlotKind::Armor(eq));
        slot.max_stack_size = 1;
        // Vanilla's `InventoryMenu.TEXTURE_EMPTY_SLOTS` map (`:34-43`), passed to
        // `ArmorSlot`'s constructor and returned by its `getNoItemIcon`.
        slot.no_item_icon = Some(match eq {
            EquipmentSlot::Head => EMPTY_ARMOR_SLOT_HELMET,
            EquipmentSlot::Chest => EMPTY_ARMOR_SLOT_CHESTPLATE,
            EquipmentSlot::Legs => EMPTY_ARMOR_SLOT_LEGGINGS,
            EquipmentSlot::Feet => EMPTY_ARMOR_SLOT_BOOTS,
            // Not reachable — `Slot::armor` is only built for the four humanoid
            // positions — but the off-hand's own sprite is the honest answer if it
            // ever is.
            EquipmentSlot::Offhand => EMPTY_ARMOR_SLOT_SHIELD,
        });
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

/// Negative controls for the container click semantics.
///
/// The positive cases live in `tests/click_machine.rs`. What is here is the
/// other half that `CLAUDE.md`'s evidence standard demands: *"assertions of an
/// absence need a control proving the detector works"*. Every test below that
/// asserts nothing happened is paired with a **control** which differs by the
/// one thing the rule turns on and which must observably succeed — so a
/// regression that makes the mechanism fire always, or never, is caught either
/// way rather than being satisfied vacuously by a menu that simply refuses
/// everything.
///
/// Every expected value is hand-derived from the 26.2 decompile under
/// `.cache/mc/26.2/src/net/minecraft/world/inventory/`, cited per test. None is
/// derived by running our own implementation.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::click::{
        Click, ContainerInput, PlayerCtx, drag_header, drag_type, quick_craft_mask,
    };
    use crate::item::{ComponentValue, ItemComponents};
    use lodestone_model::{Identifier, Text};

    fn id(name: &str) -> Identifier {
        name.parse().expect("valid identifier")
    }

    fn stack(name: &str, count: i32) -> ItemStack {
        ItemStack::new(id(name), count)
    }

    /// The same item carrying a `minecraft:custom_name`, i.e. same item,
    /// *different* components — the pair vanilla `isSameItemSameComponents` must
    /// refuse to merge. The payload shape matches what
    /// [`ItemStack::from`] produces for a wire stack that carried a custom name,
    /// so these are the components an adapter would really hand us.
    fn named(name: &str, count: i32, label: &str) -> ItemStack {
        let mut components = ItemComponents::new();
        components.insert(
            id("minecraft:custom_name"),
            ComponentValue::Text(Text::literal(label)),
        );
        ItemStack::with_components(id(name), count, components)
    }

    /// A stack whose `minecraft:equippable` names an armour position. Hand-built
    /// because nothing populates this component from the wire yet — see
    /// [`Menu::empty_equip_target`].
    fn equippable(name: &str, slot: &str) -> ItemStack {
        let mut components = ItemComponents::new();
        components.insert(
            id("minecraft:equippable"),
            ComponentValue::Str(slot.to_string()),
        );
        ItemStack::with_components(id(name), 1, components)
    }

    fn count_at(menu: &Menu, index: usize) -> Option<i32> {
        menu.slot_item(index).map(ItemStack::count)
    }

    fn carried_count(menu: &Menu) -> Option<i32> {
        menu.carried().map(ItemStack::count)
    }

    fn drag(slot: i32, header: i32, kind: i32) -> Click {
        Click {
            slot,
            button: quick_craft_mask(header, kind),
            input: ContainerInput::QuickCraft,
        }
    }

    /// Total item count across every menu slot plus the cursor. A drag must
    /// conserve it exactly; an off-by-one in the even split shows up here even
    /// when every individual slot assertion is written to match the bug.
    fn total_items(menu: &Menu) -> i32 {
        (0..menu.slot_count())
            .filter_map(|i| menu.slot_item(i))
            .map(ItemStack::count)
            .sum::<i32>()
            + menu.carried().map_or(0, ItemStack::count)
    }

    // --- QUICK_CRAFT: drags that must reset and commit nothing ---

    /// `AbstractContainerMenu.java:337-339`. The header sequence is checked
    /// against the *previous* status: `(expected != 1 || header != 2) &&
    /// expected != header` resets. A bare `END` arrives with `expected == 0` and
    /// `header == 2`, so `(true || false) && (0 != 2)` holds and the drag is
    /// reset — nothing is placed and the cursor is untouched.
    #[test]
    fn bare_drag_end_without_start_commits_nothing() {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", 9)));

        // No START, no ADD: just the commit packet.
        drag(OUTSIDE_SLOT, drag_header::END, drag_type::EVEN)
            .apply(&mut menu, PlayerCtx::survival());

        assert_eq!(count_at(&menu, 0), None, "no slot may be written");
        assert_eq!(
            carried_count(&menu),
            Some(9),
            "the cursor must be returned whole"
        );
    }

    /// The control for [`bare_drag_end_without_start_commits_nothing`]: the same
    /// three slots, the same cursor, but a well-formed START/ADD…/END sequence
    /// must place. Without this, a `Menu` that had lost the ability to commit a
    /// drag at all would pass the negative test.
    #[test]
    fn control_well_formed_drag_does_commit() {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", 9)));
        menu.perform_drag(drag_type::EVEN, &[0, 1, 2], PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), Some(3));
        assert_eq!(count_at(&menu, 1), Some(3));
        assert_eq!(count_at(&menu, 2), Some(3));
        assert_eq!(carried_count(&menu), None);
    }

    /// `AbstractContainerMenu.java:400-401`: *any* non-`QUICK_CRAFT` click while
    /// a drag is armed takes the `else if (this.quickcraftStatus != 0)` branch,
    /// which resets and falls out of `doClick` entirely. So the interrupting
    /// click is **also** swallowed — it does not pick anything up — and the
    /// subsequent `END` finds an empty painted set.
    #[test]
    fn ordinary_click_mid_drag_resets_and_is_itself_swallowed() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(5, Some(stack("minecraft:dirt", 8)));
        menu.set_carried(Some(stack("minecraft:stone", 9)));
        let ctx = PlayerCtx::survival();

        drag(OUTSIDE_SLOT, drag_header::START, drag_type::EVEN).apply(&mut menu, ctx);
        drag(0, drag_header::ADD, drag_type::EVEN).apply(&mut menu, ctx);
        drag(1, drag_header::ADD, drag_type::EVEN).apply(&mut menu, ctx);

        // The interrupt. A left-click on an occupied slot would normally swap.
        Click::left(5).apply(&mut menu, ctx);
        assert_eq!(
            count_at(&menu, 5),
            Some(8),
            "the interrupting click must be swallowed, not applied"
        );
        assert_eq!(carried_count(&menu), Some(9));

        // And the commit that follows has nothing left to commit.
        drag(OUTSIDE_SLOT, drag_header::END, drag_type::EVEN).apply(&mut menu, ctx);
        assert_eq!(count_at(&menu, 0), None);
        assert_eq!(count_at(&menu, 1), None);
        assert_eq!(carried_count(&menu), Some(9));
    }

    /// The control for the interrupt: with the drag *not* armed, the identical
    /// left-click on slot 5 does swap cursor and slot. This is what proves the
    /// assertion above is observing the reset rather than a menu where clicking
    /// never worked.
    #[test]
    fn control_same_click_applies_when_no_drag_is_armed() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(5, Some(stack("minecraft:dirt", 8)));
        menu.set_carried(Some(stack("minecraft:stone", 9)));
        Click::left(5).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(
            menu.slot_item(5).map(|s| s.item().path().to_string()),
            Some("stone".into())
        );
        assert_eq!(carried_count(&menu), Some(8));
    }

    /// `AbstractContainerMenu.java:341-342`: an empty cursor at any stage resets
    /// the drag. The paint stage therefore cannot record slots against nothing,
    /// and the commit cannot invent items.
    #[test]
    fn drag_with_empty_cursor_commits_nothing() {
        let mut menu = Menu::generic(27);
        // Cursor deliberately empty.
        menu.perform_drag(drag_type::EVEN, &[0, 1, 2], PlayerCtx::survival());
        assert_eq!(total_items(&menu), 0);
    }

    /// `AbstractContainerMenu.java:356` (paint) and `:382` (commit). The paint
    /// guard is `carried.getCount() > quickcraftSlots.size()` — strictly greater
    /// — so a cursor of 2 can only ever paint 2 slots: the third `ADD` sees
    /// `2 > 2` and is dropped. The even split is then over 2, not 3.
    ///
    /// This is the off-by-one the parent task called the classic bug, stated as
    /// an assertion: a naive implementation paints all three and divides 2 by 3,
    /// placing zero everywhere.
    #[test]
    fn paint_stops_when_the_cursor_runs_out_of_items() {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", 2)));
        menu.perform_drag(drag_type::EVEN, &[0, 1, 2], PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), Some(1));
        assert_eq!(count_at(&menu, 1), Some(1));
        assert_eq!(count_at(&menu, 2), None, "the third slot is never painted");
        assert_eq!(carried_count(&menu), None);
        assert_eq!(total_items(&menu), 2, "a drag conserves items exactly");
    }

    /// `AbstractContainerMenu.java:386`. The per-slot amount is clamped by
    /// `min(source.getMaxStackSize(), slot.getMaxStackSize(source))` **after**
    /// adding what the slot already holds, and the shortfall stays on the
    /// cursor. Slot 0 starts at 62 of a 64 cap, so it can only take 2 of its
    /// nominal 5; the other 3 must come back.
    #[test]
    fn even_split_clamps_at_the_slot_cap_and_returns_the_remainder() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(stack("minecraft:stone", 62)));
        menu.set_carried(Some(stack("minecraft:stone", 10)));
        menu.perform_drag(drag_type::EVEN, &[0, 1], PlayerCtx::survival());
        // place count = 10 / 2 = 5. Slot 0: min(5 + 62, 64) = 64, so +2.
        assert_eq!(count_at(&menu, 0), Some(64));
        // Slot 1: min(5 + 0, 64) = 5.
        assert_eq!(count_at(&menu, 1), Some(5));
        // 10 - 2 - 5 = 3 back on the cursor.
        assert_eq!(carried_count(&menu), Some(3));
        assert_eq!(total_items(&menu), 72);
    }

    /// `AbstractContainerMenu.java:62` — the painted accumulator is a
    /// `HashSet`, so a slot dragged over twice counts once. With `[0, 1, 0, 1]`
    /// the divisor must be 2, not 4: 8 items becomes 4 each, not 2 each.
    #[test]
    fn repainting_a_slot_does_not_inflate_the_divisor() {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", 8)));
        menu.perform_drag(drag_type::EVEN, &[0, 1, 0, 1], PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), Some(4));
        assert_eq!(count_at(&menu, 1), Some(4));
        assert_eq!(carried_count(&menu), None);
    }

    /// `AbstractContainerMenu.java:345` → `isValidQuickcraftType`
    /// (`:715-716`): type 2 requires `player.hasInfiniteMaterials()`, so a
    /// middle-drag in survival resets at the START stage and commits nothing.
    #[test]
    fn clone_drag_resets_in_survival() {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", 64)));
        menu.perform_drag(drag_type::CLONE, &[0, 1], PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), None);
        assert_eq!(count_at(&menu, 1), None);
        assert_eq!(carried_count(&menu), Some(64));
    }

    /// The control: the identical sequence with infinite materials places a full
    /// stack per slot (`getQuickCraftPlaceCount` case 2, `:733`).
    #[test]
    fn control_clone_drag_commits_in_creative() {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", 64)));
        menu.perform_drag(drag_type::CLONE, &[0, 1], PlayerCtx::creative());
        assert_eq!(count_at(&menu, 0), Some(64));
        assert_eq!(count_at(&menu, 1), Some(64));
    }

    /// `canItemQuickReplace` (`AbstractContainerMenu.java:722-727`) is applied at
    /// both the paint and commit stages, and it refuses an occupied slot holding
    /// a different item. Slot 1 holds dirt, so it is never painted and the split
    /// is over the two remaining slots.
    #[test]
    fn drag_skips_a_slot_holding_a_different_item() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(1, Some(stack("minecraft:dirt", 1)));
        menu.set_carried(Some(stack("minecraft:stone", 9)));
        menu.perform_drag(drag_type::EVEN, &[0, 1, 2], PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), Some(4));
        assert_eq!(
            menu.slot_item(1).map(|s| s.item().path().to_string()),
            Some("dirt".into()),
            "the foreign stack must be untouched"
        );
        assert_eq!(count_at(&menu, 2), Some(4));
        assert_eq!(carried_count(&menu), Some(1));
    }

    /// The result slot rejects placement (`ResultSlot.mayPlace` returns `false`,
    /// `ResultSlot.java:24-27`), and both drag stages test `slot.mayPlace`
    /// (`:355`, `:381`). A drag across a crafting grid that clips the result
    /// slot must skip it and divide over the grid cells only.
    #[test]
    fn drag_never_paints_the_result_slot() {
        let mut menu = Menu::crafting(3, 3);
        menu.set_carried(Some(stack("minecraft:stone", 4)));
        // Slot 0 is the result; 1 and 2 are grid cells.
        menu.perform_drag(drag_type::EVEN, &[0, 1, 2], PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), None, "the result slot is take-only");
        assert_eq!(count_at(&menu, 1), Some(2));
        assert_eq!(count_at(&menu, 2), Some(2));
        assert_eq!(carried_count(&menu), None);
    }

    // --- Merging: refused for differing components ---

    /// `AbstractContainerMenu.java:452` gates the deposit on
    /// `ItemStack.isSameItemSameComponents(clicked, carried)`. Two stacks of the
    /// same item with different components must **swap**, not merge — the
    /// `:455` branch — so neither count changes and the identities exchange.
    #[test]
    fn pickup_refuses_to_merge_stacks_with_differing_components() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(named("minecraft:diamond_sword", 1, "Excalibur")));
        menu.set_carried(Some(stack("minecraft:diamond_sword", 1)));

        Click::left(0).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(count_at(&menu, 0), Some(1), "no merge to 2");
        assert_eq!(carried_count(&menu), Some(1), "no merge to 0");
        assert!(
            menu.slot_item(0).unwrap().components().is_empty(),
            "the plain stack is now in the slot: this was a swap"
        );
        assert!(
            !menu.carried().unwrap().components().is_empty(),
            "the named stack is now on the cursor"
        );
    }

    /// The control: identical components *do* merge, on the `:452-454` branch.
    /// Without it the test above passes for a `Menu` that never merges at all.
    #[test]
    fn control_pickup_merges_stacks_with_identical_components() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(named("minecraft:stone", 1, "Rock")));
        menu.set_carried(Some(named("minecraft:stone", 1, "Rock")));
        Click::left(0).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), Some(2));
        assert_eq!(carried_count(&menu), None);
    }

    /// `moveItemStackTo`'s merge pass tests the same predicate
    /// (`AbstractContainerMenu.java:648`), so a shift-click must not stack a
    /// named item onto a plain one either. It falls through to the empty-slot
    /// pass and lands in the first free cell instead.
    #[test]
    fn quick_move_refuses_to_merge_differing_components() {
        let mut menu = Menu::generic(27);
        // Container slot 0 holds the named stack to be shift-moved out.
        menu.set_slot_item(0, Some(named("minecraft:stone", 4, "Rock")));
        // A plain stack of the same item sits in the *last* player slot, which is
        // where a backwards merge pass would reach first.
        let last = menu.slot_count() - 1;
        menu.set_slot_item(last, Some(stack("minecraft:stone", 10)));

        Click::shift(0).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(
            count_at(&menu, last),
            Some(10),
            "the plain stack must not absorb the named one"
        );
        assert_eq!(
            count_at(&menu, last - 1),
            Some(4),
            "the named stack takes the next empty slot instead"
        );
    }

    /// The control: make the destination stack's components match and the same
    /// shift-click merges into it rather than taking a fresh slot.
    #[test]
    fn control_quick_move_merges_identical_components() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(named("minecraft:stone", 4, "Rock")));
        let last = menu.slot_count() - 1;
        menu.set_slot_item(last, Some(named("minecraft:stone", 10, "Rock")));

        Click::shift(0).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(count_at(&menu, last), Some(14));
        assert_eq!(count_at(&menu, last - 1), None);
    }

    // --- PICKUP_ALL: the maxed-slot skip ---

    /// `AbstractContainerMenu.java:541-548`. The gather runs **two** passes over
    /// the slot list, and pass 0 skips any slot whose stack is already at its
    /// own max (`itemStack.getCount() != itemStack.getMaxStackSize()`). So a
    /// full stack is only drawn from once every partial one has been consumed.
    ///
    /// Cursor 4 + partials 30 and 20 = 54; the remaining 10 then comes off the
    /// full 64, leaving 54 behind. An implementation with a single pass would
    /// hit the full stack first and leave the partials untouched.
    #[test]
    fn pickup_all_defers_a_maxed_slot_to_the_second_pass() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(stack("minecraft:stone", 64))); // maxed, first in order
        menu.set_slot_item(1, Some(stack("minecraft:stone", 30)));
        menu.set_slot_item(2, Some(stack("minecraft:stone", 20)));
        menu.set_carried(Some(stack("minecraft:stone", 4)));

        // Gather is triggered on an empty slot (slot 3), as the real double-click
        // does once the first click has lifted the stack onto the cursor.
        Click::double(3).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(carried_count(&menu), Some(64));
        assert_eq!(count_at(&menu, 1), None, "partials are drained first");
        assert_eq!(count_at(&menu, 2), None);
        assert_eq!(
            count_at(&menu, 0),
            Some(54),
            "the maxed slot is only tapped in pass 1, for the shortfall"
        );
    }

    /// The control for the skip: one item short of max, the *same* slot is drawn
    /// from in pass 0. This is what makes the assertion above about ordering
    /// meaningful rather than a statement that slot 0 is never touched.
    #[test]
    fn control_pickup_all_takes_a_near_max_slot_in_the_first_pass() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(stack("minecraft:stone", 63))); // one short of max
        menu.set_slot_item(1, Some(stack("minecraft:stone", 30)));
        menu.set_carried(Some(stack("minecraft:stone", 4)));

        Click::double(3).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(carried_count(&menu), Some(64));
        assert_eq!(
            count_at(&menu, 0),
            Some(3),
            "pass 0 drained 60 of the 63 before reaching slot 1"
        );
        assert_eq!(count_at(&menu, 1), Some(30), "slot 1 was never needed");
    }

    /// `AbstractContainerMenu.java:544` requires
    /// `this.canTakeItemForPickAll(carried, target)`, which every result-bearing
    /// menu overrides to exclude its own result container — `CraftingMenu.java:156`,
    /// `InventoryMenu.java:164`, `SmithingMenu.java:129`, `StonecutterMenu.java:175`
    /// and `CartographyTableMenu.java:144` all carry the identical
    /// `target.container != this.resultSlots` line.
    ///
    /// Vacuuming the result slot would craft an item the player never asked for
    /// *and* silently charge the grid for it, because taking from the result runs
    /// `ResultSlot.onTake` (`ResultSlot.java:87`).
    #[test]
    fn pickup_all_never_drains_the_crafting_result() {
        let mut menu = Menu::crafting(3, 3);
        // A result the server has pushed, and a matching stack in the inventory.
        menu.set_slot_item(0, Some(stack("minecraft:stone", 8)));
        let last = menu.slot_count() - 1;
        menu.set_slot_item(last, Some(stack("minecraft:stone", 5)));
        // Grid cells that on_take would decrement if the result were taken.
        menu.set_slot_item(1, Some(stack("minecraft:cobblestone", 3)));
        menu.set_carried(Some(stack("minecraft:stone", 1)));

        Click::double(last - 1).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(
            count_at(&menu, 0),
            Some(8),
            "the result slot must not be a gather source"
        );
        assert_eq!(
            count_at(&menu, 1),
            Some(3),
            "and so the grid must not be charged"
        );
        assert_eq!(
            carried_count(&menu),
            Some(6),
            "only the ordinary inventory stack was gathered"
        );
    }

    // --- QUICK_MOVE ordering, per menu ---

    /// `ChestMenu.java:99` moves container contents out with `backwards = true`,
    /// so a chest empties into the **hotbar** (the tail of the menu slot list)
    /// before the main storage rows. Getting the flag wrong is invisible in an
    /// empty inventory and obvious to a player.
    #[test]
    fn chest_to_player_fills_the_hotbar_first() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(stack("minecraft:stone", 10)));
        Click::shift(0).apply(&mut menu, PlayerCtx::survival());
        let last = menu.slot_count() - 1;
        assert_eq!(count_at(&menu, last), Some(10));
        assert_eq!(count_at(&menu, 27), None, "main storage is untouched");
    }

    /// `CraftingMenu.java:123`: a shift-click from the player rows of a crafting
    /// table tries the **grid** (`1..10`) first, and only falls back to the
    /// main↔hotbar hop if the grid takes nothing.
    #[test]
    fn crafting_table_shift_click_loads_the_grid_first() {
        let mut menu = Menu::crafting(3, 3);
        let hotbar = menu.slot_count() - 9;
        menu.set_slot_item(hotbar, Some(stack("minecraft:oak_planks", 1)));
        Click::shift(hotbar).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 1), Some(1), "into the first grid cell");
        assert_eq!(count_at(&menu, hotbar), None);
    }

    /// `InventoryMenu` has **no** such branch: its chain
    /// (`InventoryMenu.java:100-152`) never targets the 2×2 grid, so the same
    /// gesture on the player's own screen does the main↔hotbar hop instead.
    /// This is the negative control for the test above — the two menus must not
    /// share one implementation.
    #[test]
    fn player_screen_shift_click_never_loads_the_two_by_two_grid() {
        let mut menu = Menu::player();
        menu.set_slot_item(36, Some(stack("minecraft:oak_planks", 1))); // hotbar[0]
        Click::shift(36).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 1), None, "grid cell 1 must stay empty");
        assert_eq!(count_at(&menu, 9), Some(1), "it goes to main storage");
    }

    /// Branches 4 and 5 of `InventoryMenu.quickMoveStack`
    /// (`InventoryMenu.java:120-128`) precede the main↔hotbar hop, so a helmet
    /// in main storage equips rather than moving to the hotbar.
    #[test]
    fn shift_click_equips_armour_before_trying_the_hotbar() {
        let mut menu = Menu::player();
        menu.set_slot_item(9, Some(equippable("minecraft:diamond_helmet", "head")));
        Click::shift(9).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 5), Some(1), "menu slot 5 is the head slot");
        assert_eq!(count_at(&menu, 9), None);
        assert_eq!(count_at(&menu, 36), None, "not the hotbar");
    }

    /// The regression this change fixes. Vanilla reaches the auto-equip branches
    /// from *every* source slot at or after 9, which includes menu slot 45, the
    /// off-hand: a helmet stashed in the off-hand shift-clicks up onto the head.
    /// Testing for an equip target only inside the `9..36` / `36..45` arms let
    /// slot 45 fall through to branch 8 and dump it into storage.
    #[test]
    fn shift_click_equips_armour_out_of_the_offhand_slot() {
        let mut menu = Menu::player();
        menu.set_slot_item(45, Some(equippable("minecraft:diamond_helmet", "head")));
        Click::shift(45).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 5), Some(1), "equipped onto the head");
        assert_eq!(count_at(&menu, 45), None);
        assert_eq!(count_at(&menu, 9), None, "not dumped into main storage");
    }

    /// The control for the branch order: with the head slot already **occupied**,
    /// branch 4's `!slots.get(8 - index).hasItem()` fails and the item takes the
    /// ordinary path out of the off-hand into storage (branch 8, `9..45`
    /// forwards). Without this, the test above would pass for an implementation
    /// that equips unconditionally and overwrites worn armour.
    #[test]
    fn control_shift_click_falls_through_when_the_armour_slot_is_taken() {
        let mut menu = Menu::player();
        menu.set_slot_item(5, Some(equippable("minecraft:iron_helmet", "head")));
        menu.set_slot_item(45, Some(equippable("minecraft:diamond_helmet", "head")));
        Click::shift(45).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(
            menu.slot_item(5).map(|s| s.item().path().to_string()),
            Some("iron_helmet".into()),
            "worn armour must not be displaced"
        );
        assert_eq!(
            count_at(&menu, 9),
            Some(1),
            "the diamond helmet goes to storage"
        );
        assert_eq!(count_at(&menu, 45), None);
    }

    /// `Slot.mayPlace` for an armour slot is
    /// `owner.isEquippableInSlot(stack, slot)` (`ArmorSlot.java:44-47`), which is
    /// `slot == equippable.slot()` (`LivingEntity.java:3886-3891`). A chestplate
    /// must not enter the head slot, by any route — here the direct place.
    #[test]
    fn armour_slot_refuses_the_wrong_equipment_position() {
        let mut menu = Menu::player();
        menu.set_carried(Some(equippable("minecraft:diamond_chestplate", "chest")));
        Click::left(5).apply(&mut menu, PlayerCtx::survival()); // 5 = head
        assert_eq!(count_at(&menu, 5), None);
        assert_eq!(carried_count(&menu), Some(1), "the cursor keeps it");
    }

    /// The control: the matching position accepts. Together with the test above
    /// this pins `may_place` to the *position*, not to "armour slots reject
    /// everything" — which is, today, exactly what happens for any stack that
    /// came off the wire, because nothing populates `minecraft:equippable`. See
    /// [`Menu::empty_equip_target`].
    #[test]
    fn control_armour_slot_accepts_the_matching_position() {
        let mut menu = Menu::player();
        menu.set_carried(Some(equippable("minecraft:diamond_helmet", "head")));
        Click::left(5).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 5), Some(1));
        assert_eq!(carried_count(&menu), None);
    }

    /// The other half of the canary above: once the effective fields *are*
    /// populated, the conversion must carry them and armour must go on.
    ///
    /// This exercises `From<&lodestone_model::ItemStack>`, which is the only
    /// path a wire stack takes into the menu model. The values here stand in for
    /// what the v770 prototype census folds in during decode — this crate cannot
    /// depend on a version crate to decode for real, so the fields are set
    /// directly and the *conversion* is what is under test.
    #[test]
    fn populated_prototype_components_survive_the_conversion_and_equip() {
        let helmet = ItemStack::from(&lodestone_model::ItemStack {
            item: id("minecraft:diamond_helmet"),
            count: 1,
            components: lodestone_model::ItemComponents {
                equippable: Some(lodestone_model::event::EquipmentSlot::Head),
                max_stack_size: Some(1),
                max_damage: Some(363),
                ..lodestone_model::ItemComponents::default()
            },
        });

        assert_eq!(
            crate::container::equippable_slot(&helmet),
            Some(EquipmentSlot::Head),
            "the equippable slot must survive the wire->menu conversion"
        );
        assert_eq!(
            helmet.max_stack_size(),
            1,
            "a real per-item cap must not fall back to 64"
        );
        assert!(
            !helmet.is_stackable(),
            "carrying max_damage must make a damageable item unstackable"
        );

        let mut menu = Menu::player();
        menu.set_carried(Some(helmet));
        Click::left(5).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(
            count_at(&menu, 5),
            Some(1),
            "a diamond helmet must now actually go into the head slot"
        );
    }

    /// The control the old suite could not express, and the one that would have
    /// caught `"chest" | "body"`.
    ///
    /// `wolf_armor` is genuinely `body`, and vanilla's humanoid-armour gate
    /// (`EquipmentSlot.Type.HUMANOID_ARMOR`) excludes `BODY`. If `body` is ever
    /// folded into `Chest` again, this fails while every positive test above
    /// keeps passing.
    #[test]
    fn animal_body_armour_is_refused_by_the_player_chestplate_slot() {
        let wolf = ItemStack::from(&lodestone_model::ItemStack {
            item: id("minecraft:wolf_armor"),
            count: 1,
            components: lodestone_model::ItemComponents {
                equippable: Some(lodestone_model::event::EquipmentSlot::Body),
                max_stack_size: Some(1),
                ..lodestone_model::ItemComponents::default()
            },
        });

        assert_eq!(
            crate::container::equippable_slot(&wolf),
            None,
            "`body` must not resolve to a humanoid armour slot"
        );

        let mut menu = Menu::player();
        menu.set_carried(Some(wolf));
        // Slot 6 is the chestplate position on the player screen.
        Click::left(6).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(
            count_at(&menu, 6),
            None,
            "wolf armour must not be wearable as a chestplate"
        );
    }
    /// `AbstractContainerMenu.java:493-506`: number-key swapping a bigger stack
    /// onto a slot whose cap is smaller than the incoming count splits the
    /// overflow into the slot and pushes the slot's *previous* contents back
    /// into the inventory via `inventory.add` (`:498`).
    ///
    /// The subtlety is aliasing: vanilla's `source` is the *same object* as
    /// `inventory.getItem(buttonNum)` (`Inventory.getItem`,
    /// `Inventory.java:437-440`, returns the live list element, not a copy), and
    /// `ItemStack.split` (`ItemStack.java:327-332`) mutates that object in place
    /// via `shrink`. So by the time `inventory.add` runs, the hotbar slot the
    /// swap came from *already* shows its reduced remainder — and a same-item
    /// displaced stack merges back into it rather than taking a fresh slot.
    /// Egg's stack cap is overridden to 16 here purely to make the overflow
    /// branch reachable without exceeding the real 64-cap game items use.
    #[test]
    fn hotbar_swap_overflow_merges_into_the_remainder_it_left_behind() {
        let mut menu = Menu::player();
        // hotbar key 0 -> native 0 -> menu slot 36.
        menu.set_slot_item(
            36,
            Some(stack("minecraft:egg", 20).with_max_stack_size(16)),
        );
        // Target: main storage slot 9 holds 5 eggs.
        menu.set_slot_item(9, Some(stack("minecraft:egg", 5).with_max_stack_size(16)));
        Click::hotbar_swap(9, 0).apply(&mut menu, PlayerCtx::survival());

        // cap = min(64, 16) = 16; source(20) > cap, so 16 eggs land in slot 9 and
        // 4 remain in the hotbar slot the swap came from.
        assert_eq!(count_at(&menu, 9), Some(16), "the overflow split fills the slot to its cap");
        assert_eq!(
            count_at(&menu, 36),
            Some(9),
            "the displaced 5 eggs merge back into the 4 left in the hotbar slot"
        );
        // No new slot should have been used for the overflow.
        for i in 37..45 {
            assert_eq!(count_at(&menu, i), None, "slot {i} must stay empty");
        }
    }

    /// The control: with room to spare (cap not exceeded), the ordinary
    /// no-overflow swap path is unaffected by the reordering above — source and
    /// target simply trade places (`AbstractContainerMenu.java:501-505`).
    #[test]
    fn control_hotbar_swap_without_overflow_is_a_plain_exchange() {
        let mut menu = Menu::player();
        menu.set_slot_item(36, Some(stack("minecraft:egg", 10)));
        menu.set_slot_item(9, Some(stack("minecraft:egg", 5)));
        Click::hotbar_swap(9, 0).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 9), Some(10));
        assert_eq!(count_at(&menu, 36), Some(5));
    }
}
