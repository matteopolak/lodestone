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
    click::QuickCraftType,
    container::{Container, EquipmentSlot, Slot, SlotKind},
    item::ItemStack,
    recipe::CraftingGrid,
};
use lodestone_model::{ContainerStateId, Identifier};

mod layout;
pub use layout::{
    CraftLayout, MenuKind, SpecialLayout, EMPTY_ARMOR_SLOT_BOOTS, EMPTY_ARMOR_SLOT_CHESTPLATE,
    EMPTY_ARMOR_SLOT_HELMET, EMPTY_ARMOR_SLOT_LEGGINGS, EMPTY_ARMOR_SLOT_SHIELD,
    EMPTY_SLOT_LAPIS_LAZULI, OFFHAND_NATIVE, OUTSIDE_SLOT, PLAYER_NATIVE_SIZE, PLAYER_RESULT_SLOT,
};


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
    /// The screen-specific pixel layout, for the few menus that have one.
    /// See [`SpecialLayout`].
    special_layout: Option<SpecialLayout>,
    /// Server-synchronised state id; bumped on every predicted mutation.
    state_id: ContainerStateId,
    /// Drag (quick-craft) accumulator state; see [`crate::click`].
    quick_craft_status: i32,
    quick_craft_type: QuickCraftType,
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
        // subclass overriding its own no-item-icon getter; the
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
            special_layout: None,
            state_id: ContainerStateId::INITIAL,
            quick_craft_status: 0,
            quick_craft_type: QuickCraftType::Even,
            quick_craft_slots: Vec::new(),
        }
    }

    /// Builds a crafting-table menu: a take-only result slot, a `width × height`
    /// input grid, then the player's main storage and hotbar.
    ///
    /// Vanilla's own crafting-table menu is `0` result, `1..=9` grid, `10..=36` main,
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
            special_layout: None,
            state_id: ContainerStateId::INITIAL,
            quick_craft_status: 0,
            quick_craft_type: QuickCraftType::Even,
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
            special_layout: None,
            state_id: ContainerStateId::INITIAL,
            quick_craft_status: 0,
            quick_craft_type: QuickCraftType::Even,
            quick_craft_slots: Vec::new(),
        }
    }

    /// Builds an "item combiner" menu: `container_size` leading slots, all
    /// accepting any item except `result_slot` (take-only), then the player's
    /// main storage and hotbar — vanilla's `ItemCombinerMenu` shape shared by
    /// the anvil (`AnvilMenu`, `container_size = 3, result_slot = 2`), the
    /// grindstone (`GrindstoneMenu`, `3, 2`) and the smithing table
    /// (`SmithingMenu`, `4, 3`). All three put `getInventorySlotStart()` /
    /// `INV_SLOT_START` at exactly `result_slot + 1`, which is why
    /// [`generic`](Self::generic)'s own numbering — `0..container_size`
    /// container, then main, then hotbar — already matches their quick-move
    /// ranges with no further change; only the result slot's *kind* differs.
    ///
    /// The input-slot `mayPlace` predicates these three menus actually declare
    /// (smithing's per-slot `RecipePropertySet` tests, the grindstone's
    /// damageable-or-enchanted check) are server data this tree does not have —
    /// the same "genuinely different, left on generic order" call
    /// [`crate::menus::build_menu`]'s doc comment already makes for the furnace
    /// and brewing stand. Accepting anything client-side and letting the
    /// server's own `container_set_slot` correct a wrong placement is the same
    /// bounded, self-correcting cost that comment describes: a visible flicker,
    /// not a desync. What **is** modelled, because it needs no such data, is
    /// the result slot itself: take-only ([`SlotKind::Output`]), which is what
    /// stops a shift-click from depositing into it.
    ///
    /// `layout` is stored as [`Menu::special_layout`], purely a pixel-position
    /// discriminator for `lodestone-shell`'s `slot_layout` — it changes no
    /// mechanics here (the anvil and grindstone are mechanically identical:
    /// same `container_size`, same `result_slot`).
    #[must_use]
    pub fn item_combiner(container_size: usize, result_slot: usize, layout: SpecialLayout) -> Self {
        let mut menu = Self::generic(container_size);
        if let Some(slot) = menu.slots.get_mut(result_slot) {
            slot.kind = SlotKind::Output;
        }
        menu.special_layout = Some(layout);
        menu
    }

    /// Builds the enchanting table menu: an item slot, a lapis-only currency
    /// slot, then the player's main storage and hotbar.
    /// Positionally identical to
    /// [`generic`](Self::generic) with a container size of 2 — confirmed
    /// against vanilla's own quick-move step for this menu — so [`MenuKind`] stays `Generic` here too; there is no
    /// take-only result slot to mark, only a placement restriction on slot 1.
    ///
    /// The three enchantment **costs**, the level-requirement clues and the
    /// enchantment seed (`EnchantmentMenu`'s ten `DataSlot`s) are not part of
    /// the slot layout; they arrive as `container_set_data` and are read back
    /// through [`crate::menus::Menus::container_data`].
    #[must_use]
    pub fn enchanting_table() -> Self {
        let mut menu = Self::generic(2);
        if let Some(slot) = menu.slots.get_mut(1) {
            slot.kind = SlotKind::LapisOnly;
            slot.no_item_icon = Some(EMPTY_SLOT_LAPIS_LAZULI);
        }
        menu.special_layout = Some(SpecialLayout::Enchanting);
        menu
    }

    /// Builds a furnace-family menu (`layout` selects which of
    /// [`SpecialLayout::Furnace`]/[`SpecialLayout::BlastFurnace`]/
    /// [`SpecialLayout::Smoker`] — all three are the same three slots, only
    /// the background art differs): ingredient, fuel, then a take-only
    /// result slot at menu index 2,
    /// followed by the player's main storage and hotbar.
    ///
    /// A server-declared cooking-input property set can route a known input to
    /// slot 0 during prediction. Fuel routing remains deliberately unmodelled:
    /// the client has no fuel data and must not invent it.
    /// What *is* modelled, because it needs no recipe/fuel data, is that the
    /// result slot only ever yields, never accepts.
    ///
    /// # Panics
    ///
    /// Never in practice: `layout` is expected to be one of the three
    /// furnace-family variants. Any other [`SpecialLayout`] still builds a
    /// mechanically correct 3-slot menu (`lodestone-shell`'s
    /// `special_layout_positions` simply will not recognise it and falls
    /// back to a plain generic row), so this is not a hard precondition.
    #[must_use]
    pub fn furnace(layout: SpecialLayout) -> Self {
        let mut menu = Self::generic(3);
        if let Some(slot) = menu.slots.get_mut(2) {
            slot.kind = SlotKind::Output;
        }
        menu.special_layout = Some(layout);
        menu
    }

    /// Builds the brewing stand menu: three potion slots (`0..3`), an
    /// ingredient slot (`3`), a fuel slot (`4`), then the player's main
    /// storage and hotbar.
    ///
    /// Vanilla's own brewing-stand quick-move step routes by item kind (blaze
    /// powder/ingredient/potion), which is the same "genuinely different,
    /// left on generic order" gap [`furnace`](Self::furnace) and
    /// [`crate::menus::build_menu`]'s doc comment both name — it needs the
    /// potion-brewing predicate tables this tree does not have. No slot kind
    /// changes are made here for the same reason: unlike the furnace's
    /// result slot, none of the five brewing-stand slots are unconditionally
    /// take-only (a potion slot yields *and* accepts a fresh bottle).
    #[must_use]
    pub fn brewing_stand() -> Self {
        let mut menu = Self::generic(5);
        menu.special_layout = Some(SpecialLayout::Brewing);
        menu
    }

    /// Builds the loom menu: banner (`0`), dye (`1`), pattern (`2`), then a
    /// take-only result slot (`3`), then the player's main storage and
    /// hotbar.
    ///
    /// **Stale, corrected**: this used to say the banner-pattern selection
    /// grid was not modelled here at all. That was true when written and is
    /// not any more — `lodestone-shell`'s `container::loom` module is the
    /// grid's click surface (see `docs/container-station-widgets.md`); this
    /// constructor still needs no pattern data of its own, since a
    /// banner/dye/pattern item is accepted or refused by its own item kind, a
    /// placement predicate this menu leaves on the generic "accept anything,
    /// let the server correct it" order, and the result slot is still
    /// correctly take-only.
    #[must_use]
    pub fn loom() -> Self {
        let mut menu = Self::generic(4);
        if let Some(slot) = menu.slots.get_mut(3) {
            slot.kind = SlotKind::Output;
        }
        menu.special_layout = Some(SpecialLayout::Loom);
        menu
    }

    /// Builds the stonecutter menu: an input slot (`0`) and a take-only
    /// result slot (`1`), then the player's main storage and hotbar.
    ///
    /// The recipe-selection scroll list (vanilla's own menu-button click handler)
    /// is not modelled — see [`SpecialLayout::Stonecutter`]'s doc comment.
    #[must_use]
    pub fn stonecutter() -> Self {
        let mut menu = Self::generic(2);
        if let Some(slot) = menu.slots.get_mut(1) {
            slot.kind = SlotKind::Output;
        }
        menu.special_layout = Some(SpecialLayout::Stonecutter);
        menu
    }

    /// Builds the cartography table menu: a map slot (`0`), an additional
    /// (paper/map/glass-pane) slot (`1`), then a take-only result slot
    /// (`2`), then the player's main storage and hotbar.
    #[must_use]
    pub fn cartography_table() -> Self {
        let mut menu = Self::generic(3);
        if let Some(slot) = menu.slots.get_mut(2) {
            slot.kind = SlotKind::Output;
        }
        menu.special_layout = Some(SpecialLayout::Cartography);
        menu
    }

    /// Builds a dispenser/dropper menu: a 3×3 grid (`0..9`), then the
    /// player's main storage and hotbar.
    /// Mechanically identical to [`generic`](Self::generic) — no slot kind
    /// changes, vanilla's own dispenser quick-move step is the same "container then
    /// player" shape [`crate::menus::build_menu`]'s doc comment already
    /// attributes to `quick_move_generic` — this exists purely to attach
    /// [`SpecialLayout::Dispenser`] so the 3×3 grid draws as a square
    /// instead of `generic`'s flat 9-wide row.
    #[must_use]
    pub fn dispenser() -> Self {
        let mut menu = Self::generic(9);
        menu.special_layout = Some(SpecialLayout::Dispenser);
        menu
    }

    /// Builds a hopper menu: five slots in a row (`0..5`), then the player's
    /// main storage and hotbar. Mechanically
    /// identical to [`generic`](Self::generic) — vanilla's own hopper quick-move step
    /// is the same container-then-player shape
    /// [`crate::menus::build_menu`]'s doc comment already attributes to
    /// `quick_move_generic` — this exists purely to attach
    /// [`SpecialLayout::Hopper`] so the screen draws at vanilla's real,
    /// *shorter* `176×133` panel instead of the plain chest sheet's `166`.
    #[must_use]
    pub fn hopper() -> Self {
        let mut menu = Self::generic(5);
        menu.special_layout = Some(SpecialLayout::Hopper);
        menu
    }

    /// Builds the merchant/trading menu: two payment slots (`0`, `1`), then a
    /// take-only result slot (`2`), then the player's main storage and hotbar.
    ///
    /// Vanilla's own merchant quick-move step is genuinely different from
    /// [`quick_move_generic`](Self::quick_move_generic) — the result slot
    /// (`slotIndex == 2`) empties into the player inventory the same way, but
    /// the two payment slots (`0`, `1`) move to the player inventory
    /// **forwards**, not backwards, and vanilla's own trade-item-move step
    /// (auto-filling the payment slots from the player's own
    /// inventory when a trade row is selected) is not modelled at all — it
    /// needs the offer list, which lives on [`crate::trades::TradeOffers`], not
    /// on this menu. Left on the generic "container then player" order for the
    /// same reason the furnace and brewing stand are (see
    /// [`crate::menus::build_menu`]'s doc comment): the cost is bounded and
    /// self-correcting, a visible flicker rather than a desync, and no server
    /// half of trading exists yet to correct it against.
    #[must_use]
    pub fn merchant() -> Self {
        let mut menu = Self::generic(3);
        if let Some(slot) = menu.slots.get_mut(2) {
            slot.kind = SlotKind::Output;
        }
        menu.special_layout = Some(SpecialLayout::Merchant);
        menu
    }

    /// Builds the beacon menu: a single payment slot (`0`), then the
    /// player's main storage and hotbar.
    ///
    /// Vanilla's own beacon payment-slot placement restriction (the beacon-payment
    /// item tag) is
    /// not modelled — the same "accept anything, let the server's own
    /// `container_set_slot` correct a wrong guess" convention
    /// [`Self::item_combiner`]'s doc comment already applies to the anvil,
    /// grindstone and smithing table. The slot is not marked
    /// [`SlotKind::Output`] either: unlike those three, the payment slot both
    /// accepts an item (a placement) and later loses it (consumed by a
    /// successful `SET_BEACON`, vanilla's own effect-update step's own
    /// payment-slot removal) — never a take-only result.
    #[must_use]
    pub fn beacon() -> Self {
        let mut menu = Self::generic(1);
        menu.special_layout = Some(SpecialLayout::Beacon);
        menu
    }

    /// Builds the read-only lectern menu. A lectern content packet contains
    /// only its displayed book; the player's inventory is not part of that
    /// packet because the screen is the book reader overlay.
    #[must_use]
    pub fn lectern() -> Self {
        let mut menu = Self::generic(1);
        menu.slots[0].kind = SlotKind::ReadOnly;
        menu.special_layout = Some(SpecialLayout::Lectern);
        menu
    }

    /// Returns the menu kind.
    #[must_use]
    pub fn kind(&self) -> MenuKind {
        self.kind
    }

    /// The screen-specific pixel layout, if this menu has one. See
    /// [`SpecialLayout`].
    #[must_use]
    pub fn special_layout(&self) -> Option<SpecialLayout> {
        self.special_layout
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

    /// The menu-slot range holding the player's **main storage and hotbar**
    /// only — never armour or off-hand, matching vanilla's own recipe-book
    /// placement (its own place-recipe helper walks
    /// the 36 main+hotbar slots, never the armour or off-hand slots).
    /// `None` for a [`MenuKind`] this crate
    /// does not (yet) know an inventory range for.
    fn inventory_slot_range(&self) -> Option<std::ops::Range<usize>> {
        match self.kind {
            // `0` result, `1..=4` crafting, `5..=8` armour, `9..=35` main,
            // `36..=44` hotbar, `45` off-hand (module doc table).
            MenuKind::Player => Some(9..45),
            // `0..n` container, `n..n+27` main, `n+27..n+36` hotbar.
            MenuKind::Generic { container_size } => {
                Some(container_size..container_size + 36)
            }
        }
    }

    /// Computes an auto-fill plan ("click recipe to auto-fill")
    /// for `recipe` against this menu's crafting grid — a crafting table's
    /// grid via [`craft_layout`](Self::craft_layout), or a furnace-family
    /// menu's single ingredient slot (menu index `0`) via
    /// [`special_layout`](Self::special_layout) — reusing
    /// [`crate::recipe::plan_auto_fill`] against a snapshot of the player's
    /// main storage and hotbar.
    ///
    /// Every [`recipe::PlacementStep::cell`] in the returned plan is already
    /// translated to an **absolute menu-slot index** (crafting's
    /// `craft_layout().first_input` offset applied, furnace's own `0`), so a
    /// caller can feed each step directly to the same slot-click machinery
    /// [`crate::click::Click`] already provides — no separate offset step
    /// needed downstream.
    ///
    /// Returns `None` when this menu has no crafting grid *and* no
    /// furnace-family [`SpecialLayout`], when the recipe's own
    /// [`Recipe::book_type`](crate::recipe::Recipe::book_type) does not
    /// match this menu's grid shape at all, or when
    /// [`plan_auto_fill`](crate::recipe::plan_auto_fill) itself returns
    /// `None` (missing ingredient — see its own doc comment).
    #[must_use]
    pub fn plan_recipe_auto_fill(
        &self,
        recipe: &crate::recipe::Recipe,
        tags: &crate::recipe::TagResolver,
    ) -> Option<Vec<crate::recipe::PlacementStep>> {
        let inventory_range = self.inventory_slot_range()?;
        let inventory: Vec<(usize, &ItemStack)> = inventory_range
            .filter_map(|i| self.slot_item(i).map(|s| (i, s)))
            .collect();

        if let Some(craft) = self.craft {
            let steps = crate::recipe::plan_auto_fill(recipe, craft.width, craft.height, &inventory, tags)?;
            return Some(
                steps
                    .into_iter()
                    .map(|s| crate::recipe::PlacementStep {
                        cell: craft.first_input + s.cell,
                        source_slot: s.source_slot,
                    })
                    .collect(),
            );
        }

        match self.special_layout {
            Some(SpecialLayout::Furnace | SpecialLayout::BlastFurnace | SpecialLayout::Smoker) => {
                // The ingredient slot is menu index 0 (`Menu::furnace`'s own
                // layout), so no cell offset is needed beyond what
                // `plan_auto_fill` already reports (cell 0).
                crate::recipe::plan_auto_fill(recipe, 1, 1, &inventory, tags)
            }
            _ => None,
        }
    }

    /// Returns the current state id.
    #[must_use]
    pub fn state_id(&self) -> ContainerStateId {
        self.state_id
    }

    /// Bumps the state id, mirroring the client incrementing its container
    /// state before sending a click.
    pub fn bump_state(&mut self) -> ContainerStateId {
        self.state_id = self.state_id.next();
        self.state_id
    }

    /// Sets the state id directly (used to align with a server-sent id).
    pub fn set_state_id(&mut self, state_id: ContainerStateId) {
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

    /// Removes items from the hotbar slot `selected` (a **native** index, `0..9`)
    /// and returns what was removed, or `None` if nothing was.
    ///
    /// `all == false` takes **one** item (plain `Q`); `all == true` takes the
    /// whole stack (`Ctrl`+`Q`). A slot whose count reaches zero becomes `None`
    /// rather than a zero-count stack, so no draw path can render an item with a
    /// blank or `0` number.
    ///
    /// # Why this exists, and why it is a *prediction*
    ///
    /// Port of vanilla's own selected-item removal step: if the selected
    /// stack is empty, do nothing; otherwise remove either the whole stack
    /// (`all == true`) or a single item from it, returning what was removed.
    /// That step lowers through vanilla's own inventory-remove and
    /// container-helper remove steps to a plain stack split — hence the
    /// `split` below rather than a hand-rolled
    /// decrement. Note vanilla's own container-helper remove step guards `count > 0`, which is
    /// why `all == true` on an already-empty slot cannot produce a phantom
    /// removal: the empty check above it returns first.
    ///
    /// **The dropped-item entity is not ours to make.** Vanilla's client calls
    /// this from its own local-player drop step,
    /// which names the result `prediction` and then sends only a bare
    /// drop-item server-bound action packet. The server
    /// handles that action and
    /// **sends no slot update back**, so this local mutation is the *only* thing
    /// that will ever change the count the hotbar draws. Without it the count is
    /// stale forever, not merely late — which is the bug this closes.
    ///
    /// The return value exists because vanilla's does, and it is used for exactly
    /// one thing there: vanilla's own local-player drop step returns whether the
    /// prediction was non-empty and
    /// its own render loop swings the arm only when it is `true`. Nothing
    /// downstream needs the stack itself — the item entity is spawned by the
    /// server and arrives as an ordinary entity-spawn packet.
    pub fn remove_from_selected(&mut self, selected: usize, all: bool) -> Option<ItemStack> {
        let container = self.player_container;
        // An out-of-range index reads as empty
        // too, matching vanilla's own container-helper remove step's bounds guard.
        let Some(stack) = self.containers[container].get(selected) else {
            return None;
        };
        if stack.is_empty() {
            return None;
        }
        let count = if all { stack.count() } else { 1 };
        let mut stack = stack.clone();
        let removed = stack.split(count);
        // `set` normalises a zero-count remainder to `None`: vanilla leaves a
        // `count == 0` `ItemStack` in the list and relies on its own
        // emptiness check everywhere downstream, which `Option` models directly.
        self.containers[container].set(selected, normalize_opt(Some(stack)));
        crate::item::normalize(removed)
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
    /// is nothing to keep in sync and nothing that can diverge — the two
    /// copies desyncing was once the whole bug here.
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

    /// Vanilla's own post-take slot hook, run after **every** successful removal from a
    /// slot. Only the crafting result slot has behaviour: vanilla's own
    /// result-slot take hook
    /// removes exactly one item from every occupied grid cell.
    ///
    /// Without this, taking a result leaves the grid full — the ingredients are
    /// never consumed, so the very next prediction contradicts the server on
    /// every grid cell at once. It is the missing half of "slot 0 is take-only":
    /// [`Slot::may_place`] stops you *putting* something there, and this is what
    /// makes *taking* it cost something.
    ///
    /// The consumption is deliberately **recipe-free**: vanilla walks the
    /// positioned craft input and removes one item from each non-empty
    /// cell, which needs no knowledge of which recipe matched.
    ///
    /// What *is* skipped is the **remainder** pass — the one that leaves an empty
    /// bucket behind after crafting a cake. Note that this is *not* skipped
    /// because it needs the recipe: vanilla's own remaining-items step only consults
    /// the recipe on the server, and on the client falls through to
    /// a plain per-item crafting-remainder lookup. It is skipped because **we have no
    /// crafting-remainder table** for 26.2's items yet, and inventing one would
    /// be a guess. Until there is one, a remainder-bearing ingredient mispredicts
    /// its cell for one round trip and the server corrects it with a
    /// `container_set_slot`; only ~10 items in the game have a remainder.
    ///
    /// The call sites mirror vanilla's exactly: the click handler's pickup and
    /// same-item-pull branches, the safe-take throw path, the swap take, and
    /// the tail of the quick-move step. The both-occupied swap branch also calls it
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
    /// matching stacks first then filling empties, mirroring vanilla's
    /// own stack-move-to step.
    ///
    /// `moving` is drained in place. Returns whether anything changed.
    ///
    /// Three details are transcribed deliberately and all three look like bugs:
    ///
    /// * **The merge pass does not consult `mayPlace`; only the empty-slot pass
    ///   does.** So a shift-click may
    ///   *top up* an existing stack in a slot that would refuse the same item
    ///   arriving into an empty cell. Adding the symmetric check "for
    ///   consistency" changes observable behaviour and desynchronises from the
    ///   server.
    /// * **The merge pass is gated on `moving.isStackable()`**, not on
    ///   the per-slot cap. An unstackable item skips merging entirely and goes
    ///   straight to the first empty slot.
    /// * **The merge cap is measured against the stack already in the slot**,
    ///   while the empty-slot cap is
    ///   measured against the incoming stack. They agree whenever the two are the same item, which the merge
    ///   pass has already established, so this is only a difference in what the
    ///   code *says* — but it is what the source says.
    ///
    /// The empty-slot pass stops after **one** placement,
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
        self.quick_move_with_furnace_input_items(menu_index, None)
    }

    /// Quick-move with the server-declared furnace-family input set available
    /// to the predictor. The public [`quick_move`](Self::quick_move) entry
    /// retains generic behavior for callers without live recipe-book sync.
    pub(crate) fn quick_move_with_furnace_input_items(
        &mut self,
        menu_index: usize,
        furnace_input_items: Option<&[Identifier]>,
    ) -> Option<ItemStack> {
        let original = self.slot_item_cloned(menu_index)?;
        let template = original.clone();
        let mut stack = original;
        let moved = match (self.kind, self.craft) {
            (MenuKind::Player, _) => self.quick_move_player(menu_index, &mut stack),
            (MenuKind::Generic { container_size }, Some(layout)) => {
                self.quick_move_crafting(menu_index, container_size, layout, &mut stack)
            }
            (MenuKind::Generic { container_size }, None) => {
                self.quick_move_generic(
                    menu_index,
                    container_size,
                    &mut stack,
                    furnace_input_items,
                )
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

    /// Quick-move for a plain container, mirroring vanilla's own chest
    /// quick-move step: container slots go out to the player
    /// inventory **backwards** (hotbar first), player slots come in forwards.
    ///
    /// This one order covers more of the game than its name suggests.
    /// The hopper's and dispenser's own quick-move steps are the same
    /// three lines with a different constant, and the shulker box's own step
    /// likewise — so chests, barrels, ender chests, every `generic_9xN`,
    /// hoppers, dispensers, droppers and shulker boxes all share it.
    ///
    /// A furnace-family menu is the narrow exception: when the server supplied
    /// its cooking-input property set and the player stack belongs to it, the
    /// move targets only slot 0. The fuel branch remains absent because no
    /// fuel data is available. Non-input stacks, and every click before that
    /// property set arrives, keep the generic order below.
    fn quick_move_generic(
        &mut self,
        menu_index: usize,
        container_size: usize,
        stack: &mut ItemStack,
        furnace_input_items: Option<&[Identifier]>,
    ) -> bool {
        let total = self.slot_count();
        if menu_index < container_size {
            // container -> player inventory, filling from the back
            self.move_item_stack_to(stack, container_size, total, true)
        } else if matches!(
            self.special_layout,
            Some(SpecialLayout::Furnace | SpecialLayout::BlastFurnace | SpecialLayout::Smoker)
        ) && furnace_input_items.is_some_and(|items| items.contains(stack.item()))
        {
            // A cooking input never takes the fuel slot. If input slot 0 cannot
            // accept it, the server will reconcile rather than this predictor
            // guessing a fuel classification or a fallback destination.
            self.move_item_stack_to(stack, 0, 1, false)
        } else {
            // player inventory -> container
            self.move_item_stack_to(stack, 0, container_size, false)
        }
    }

    /// Quick-move for a crafting-table menu, mirroring vanilla's own
    /// crafting-table quick-move step:
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
    /// diverge, and the difference is not cosmetic: vanilla's own crafting-table
    /// quick-move step tries the
    /// grid first, so shift-clicking planks in a
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

    /// Quick-move for the player's own inventory screen, mirroring vanilla's
    /// own player-inventory quick-move step.
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
    /// its own player-inventory quick-move step.
    ///
    /// Vanilla derives the position from its own item-to-equipment-slot
    /// resolver, which reads the item's own equippable component's slot,
    /// and maps it to a menu index as
    /// `8 - eqSlot.getIndex()` — head 3 → 5, chest 2 → 6, legs 1 → 7, feet 0 → 8
    /// — with the off-hand at 45. That is the mapping below.
    ///
    /// One thing this deliberately does **not** do: vanilla gates branch 4 on
    /// the equipment slot's type being humanoid armour, which excludes `BODY` — wolf and horse
    /// armour. [`crate::container::EquipmentSlot::from_name`] deliberately
    /// leaves `"body"` unmatched (falling through to `None`) rather than
    /// folding it into [`Chest`](EquipmentSlot::Chest), so a wolf/horse-armour
    /// item never resolves an `eq` here and this function correctly declines
    /// to auto-equip it into a player's chestplate slot.
    ///
    /// # Reachable in live play
    ///
    /// `minecraft:equippable` is a **prototype** component: like
    /// `minecraft:tool` (see [`lodestone_model::ToolPatch`]'s docs and
    /// `docs/tool-mining.md`), vanilla puts it in the item's built-in
    /// component map, so a clientbound stack — which carries only the
    /// *patch* — never mentions it on its own. `crates/versions/26.2/src/
    /// adapter/inventory.rs`'s `read_component_patch` seeds
    /// [`lodestone_model::ItemComponents::equippable`] from
    /// [`lodestone_data::item_prototypes::prototype`] before the patch is
    /// read, the same way it seeds `max_stack_size`/`max_damage`, so a real
    /// stack off the wire does carry it and
    /// [`crate::container::equippable_slot`] returns `Some` for every one of
    /// the census's 84 equippable items. [`Slot::may_place`] for an
    /// [`Armor`](SlotKind::Armor) slot and this function are both live.
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

    pub(crate) fn quick_craft_type(&self) -> QuickCraftType {
        self.quick_craft_type
    }

    pub(crate) fn set_quick_craft_type(&mut self, kind: QuickCraftType) {
        self.quick_craft_type = kind;
    }

    /// The slots this menu has accumulated from `ADD` packets during a drag —
    /// vanilla's own quick-craft accumulator field.
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
    /// Vanilla's accumulator is a hash set and the paint site is a bare
    /// insert, so dragging back and forth across one slot records
    /// it once. That set's size is then the divisor for an even split,
    /// so a `Vec` that pushed duplicates would divide by too large a
    /// number and under-fill every slot — the classic off-by-N. The order is
    /// kept insertion-stable here where vanilla's is a hash order; that is safe
    /// because the per-slot amount is `count / size`, a constant, and the loop
    /// never mutates the cursor it reads, so no ordering is observable.
    pub(crate) fn push_quick_craft_slot(&mut self, menu_index: usize) {
        if !self.quick_craft_slots.contains(&menu_index) {
            self.quick_craft_slots.push(menu_index);
        }
    }

    /// Vanilla's own quick-craft reset step: clears
    /// the status and the painted set, but deliberately **not**
    /// `quick_craft_type`, which the single-slot degradation path reads back
    /// after the reset.
    pub(crate) fn reset_quick_craft(&mut self) {
        self.quick_craft_status = 0;
        self.quick_craft_slots.clear();
    }
}

impl Slot {
    fn armor(container: usize, index: usize, eq: EquipmentSlot) -> Self {
        let mut slot = Slot::of(container, index, SlotKind::Armor(eq));
        slot.max_stack_size = 1;
        // Vanilla's own empty-slot-texture map, passed to
        // its own armour-slot constructor and returned by its no-item-icon getter.
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

#[cfg(test)]
mod tests;
