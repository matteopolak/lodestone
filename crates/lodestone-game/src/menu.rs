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
use lodestone_model::Identifier;

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

/// A `menu_type`-specific slot **position** (and, in `lodestone-shell`,
/// background art) descriptor, for the handful of screens whose panel isn't
/// [`MenuKind::Generic`]'s plain left-to-right grid — the anvil, grindstone,
/// smithing table and enchanting table.
///
/// Carried on [`Menu`] rather than in [`MenuKind`], for the same reason
/// [`CraftLayout`] is: [`MenuKind`] is matched exhaustively in
/// `lodestone-shell`'s `slot_layout`, and all four of these menus are
/// mechanically a plain [`MenuKind::Generic`] (quick-move regions included —
/// see [`Menu::item_combiner`]'s doc comment) with only their **pixel layout**
/// different. Putting the discriminator here means `lodestone-shell`'s
/// `slot_layout(menu)` — the one function both drawing *and* click hit-testing
/// already call — can special-case it with no new parameter threaded through
/// `hit_test`/`hit_test_with_scale`'s callers. Getting that wrong (a
/// `menu_type` passed to the draw path but not the hit-test path) is exactly
/// this module's own documented failure mode: "clicks land one slot off... a
/// bug invisible in any screenshot."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialLayout {
    /// `AnvilMenu`: slots at `(27,47)`, `(76,47)`, `(134,47)`.
    Anvil,
    /// `GrindstoneMenu`: slots at `(49,19)`, `(49,40)`, `(129,34)`.
    Grindstone,
    /// `SmithingMenu`: slots at `(8,48)`, `(26,48)`, `(44,48)`, `(98,48)`.
    Smithing,
    /// `EnchantmentMenu`: slots at `(15,47)`, `(35,47)`.
    Enchanting,
    /// Vanilla's own furnace menu (wire `menu_type` `furnace`): ingredient `(56,17)`, fuel
    /// `(56,53)`, result `(116,35)`. A separate variant from
    /// [`BlastFurnace`](Self::BlastFurnace)/[`Smoker`](Self::Smoker) purely
    /// for the background sheet: all three share these exact slot
    /// coordinates (a shared base class provides the common constructor), but
    /// `furnace.png`/`blast_furnace.png`/`smoker.png` are three different
    /// textures with three differently-named progress sprites.
    Furnace,
    /// Vanilla's own blast-furnace menu (wire `menu_type` `blast_furnace`). Same three slot
    /// coordinates as [`Furnace`](Self::Furnace); see its doc comment for why
    /// this is still a separate variant.
    BlastFurnace,
    /// Vanilla's own smoker menu (wire `menu_type` `smoker`). Same three slot coordinates
    /// as [`Furnace`](Self::Furnace); see its doc comment for why this is
    /// still a separate variant.
    Smoker,
    /// Vanilla's own brewing-stand menu: potion slots `(56,51)`, `(79,58)`, `(102,51)`,
    /// ingredient `(79,17)`, fuel `(17,17)`.
    Brewing,
    /// Vanilla's own loom menu: banner `(13,26)`, dye `(33,26)`, pattern `(23,45)`, result
    /// `(143,57)`. **Stale, corrected**: this used to say
    /// the pattern-selection button grid was unmodelled, needing a banner
    /// pattern registry and a `ContainerButtonClick` producer this tree
    /// lacked. Both now exist — `lodestone-server`'s `loom` module computes
    /// real results and `lodestone-shell`'s `container::loom` is the click
    /// surface; see `docs/container-station-widgets.md`.
    Loom,
    /// Vanilla's own stonecutter menu: input `(20,33)`, result `(143,33)`.
    /// **Stale, corrected**: this used to say the
    /// recipe-selection scroll list was unmodelled, needing server-only
    /// recipe data this tree lacked. It now loads through the same
    /// jar-sourced `RecipeBook` the crafting recipe book uses, and
    /// `lodestone-shell`'s `container::stonecutter` is the click surface;
    /// see `docs/container-station-widgets.md`.
    Stonecutter,
    /// Vanilla's own cartography-table menu: map `(15,15)`, additional `(15,52)`, result
    /// `(145,39)`.
    Cartography,
    /// Vanilla's own dispenser menu (wire `menu_type` `generic_3x3`, shared by the
    /// dispenser **and** the dropper — vanilla ships no `dropper.png` or
    /// its own dropper screen; its own screen-registration table maps
    /// `GENERIC_3x3` to
    /// the dispenser screen alone): a 3×3 grid at `(62,17)`, step `18`.
    Dispenser,
    /// Vanilla's own hopper menu: five slots in a row at `(44,20)`, step `18`.
    /// Found while fixing a doc that (incorrectly) claimed
    /// this one had nowhere to go: vanilla's own hopper screen is a real, *shorter*
    /// screen — `imageHeight = 133`, not `166` — so a hopper drawing `generic_54`'s ordinary chest sheet was
    /// exactly the same class of defect: a plausible but wrong
    /// screen, not a missing one.
    Hopper,
    /// Vanilla's own merchant menu: payment slots `(136,37)`, `(162,37)`, take-only result
    /// `(220,37)`. The **only** special layout
    /// whose player-inventory section is not at `x = 8`:
    /// vanilla's own inventory-slot placement step
    /// starts it at `x = 108`, and the panel itself is `276` wide, not
    /// `176`. The trade **list** — seven scrollable
    /// rows of cost/result icons that are not menu slots at all, vanilla's own
    /// `ItemStack`s rendered as "fake items" — is not part of this layout;
    /// see `lodestone_shell::container::merchant`.
    Merchant,
    /// Vanilla's own beacon menu: one payment slot at `(136,110)`, panel `230×219`
    /// — its own inventory-slot placement step
    /// puts the player section at `x = 36` rather than the usual `8`,
    /// the second special layout (after [`Merchant`](Self::Merchant)) whose
    /// player section is not left-aligned. The primary/secondary power
    /// buttons and the confirm/cancel controls are not menu slots at all —
    /// vanilla drives them off `container_data` and its own screen-local
    /// selection state, not `AbstractContainerMenu` slots; see
    /// `lodestone_shell::container::beacon`.
    Beacon,
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
/// (vanilla's own player-inventory result-slot constant).
pub const PLAYER_RESULT_SLOT: usize = 0;
/// Native index of the off-hand slot within the player inventory.
pub const OFFHAND_NATIVE: usize = 40;
/// Sentinel slot index for a click outside any slot (drop).
pub const OUTSIDE_SLOT: i32 = -999;

/// The empty-slot sprites the player inventory declares.
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
/// overrides vanilla's own no-item-icon getter.
pub const EMPTY_ARMOR_SLOT_SHIELD: &str = "container/slot/shield";
/// The enchanting table's lapis slot empty-icon, vanilla's own
/// `EMPTY_SLOT_LAPIS_LAZULI`. See [`EMPTY_ARMOR_SLOT_HELMET`] for why this is
/// a constant rather than inferred from the slot index.
pub const EMPTY_SLOT_LAPIS_LAZULI: &str = "container/slot/lapis_lazuli";

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
    state_id: u32,
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
            state_id: 0,
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
            state_id: 0,
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
            state_id: 0,
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
/// Every expected value is hand-derived from the 26.2 decompile, cited per
/// test. None is
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

    /// Vanilla's own quick-craft header-check step. The header sequence is checked
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

    /// Vanilla's own click-dispatch step: *any* non-`QUICK_CRAFT` click while
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

        drag(OUTSIDE_SLOT, drag_header::START, drag_type::EVEN).apply(&mut menu, ctx.clone());
        drag(0, drag_header::ADD, drag_type::EVEN).apply(&mut menu, ctx.clone());
        drag(1, drag_header::ADD, drag_type::EVEN).apply(&mut menu, ctx.clone());

        // The interrupt. A left-click on an occupied slot would normally swap.
        Click::left(5).apply(&mut menu, ctx.clone());
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

    /// Vanilla's own drag-start step: an empty cursor at any stage resets
    /// the drag. The paint stage therefore cannot record slots against nothing,
    /// and the commit cannot invent items.
    #[test]
    fn drag_with_empty_cursor_commits_nothing() {
        let mut menu = Menu::generic(27);
        // Cursor deliberately empty.
        menu.perform_drag(drag_type::EVEN, &[0, 1, 2], PlayerCtx::survival());
        assert_eq!(total_items(&menu), 0);
    }

    /// Vanilla's own drag paint and commit steps. The paint
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

    /// Vanilla's own drag commit step. The per-slot amount is clamped by
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

    /// Vanilla's own painted-accumulator field — a
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

    /// Vanilla's own quick-craft-type validity check: type 2 requires
    /// `player.hasInfiniteMaterials()`, so a
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

    /// Vanilla's own quick-replace eligibility check is applied at
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

    /// The result slot rejects placement (vanilla's own result-slot placement
    /// check always returns `false`), and both drag stages test `slot.mayPlace`.
    /// A drag across a crafting grid that clips the result
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

    /// Vanilla's own click-deposit step gates the deposit on the two
    /// stacks being the same item with the same components. Two stacks of the
    /// same item with different components must **swap**, not merge —
    /// so neither count changes and the identities exchange.
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

    /// Vanilla's own stack-move-to merge pass tests the same predicate,
    /// so a shift-click must not stack a
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

    /// Vanilla's own pick-all gather step. It runs **two** passes over
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

    /// Vanilla's own pick-all eligibility check requires
    /// `this.canTakeItemForPickAll(carried, target)`, which every result-bearing
    /// menu overrides to exclude its own result container — every one of
    /// them carries the identical
    /// `target.container != this.resultSlots` line.
    ///
    /// Vacuuming the result slot would craft an item the player never asked for
    /// *and* silently charge the grid for it, because taking from the result runs
    /// vanilla's own result-slot take hook.
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

    /// Vanilla's own chest quick-move step moves container contents out with
    /// `backwards = true`,
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

    /// Vanilla's own crafting-table quick-move step: a shift-click from the player rows of a crafting
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

    /// Vanilla's own player-inventory quick-move step has **no** such branch: its chain
    /// never targets the 2×2 grid, so the same
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

    fn assert_declared_furnace_input_does_not_spill_into_fuel(layout: SpecialLayout) {
        let mut menu = Menu::furnace(layout);
        let hotbar = menu.slot_count() - 9;
        menu.set_slot_item(0, Some(stack("minecraft:stone", 64)));
        menu.set_slot_item(hotbar, Some(stack("minecraft:raw_iron", 1)));

        Click::shift(hotbar).apply(
            &mut menu,
            PlayerCtx::survival().with_furnace_input_items(vec![id("minecraft:raw_iron")]),
        );

        assert_eq!(count_at(&menu, 0), Some(64), "the full ingredient slot stays intact");
        assert_eq!(
            count_at(&menu, 1),
            None,
            "a declared input must not spill into the fuel slot"
        );
        assert_eq!(count_at(&menu, hotbar), Some(1), "the input remains with the player");
    }

    fn assert_declared_furnace_input_moves_to_ingredient_slot(layout: SpecialLayout) {
        let mut menu = Menu::furnace(layout);
        let hotbar = menu.slot_count() - 9;
        menu.set_slot_item(hotbar, Some(stack("minecraft:raw_iron", 1)));

        Click::shift(hotbar).apply(
            &mut menu,
            PlayerCtx::survival().with_furnace_input_items(vec![id("minecraft:raw_iron")]),
        );

        assert_eq!(count_at(&menu, 0), Some(1), "the declared input fills slot 0");
        assert_eq!(count_at(&menu, 1), None, "the fuel slot stays empty");
        assert_eq!(count_at(&menu, hotbar), None, "the input leaves the player inventory");
    }

    #[test]
    fn furnace_declared_input_moves_to_ingredient_slot() {
        assert_declared_furnace_input_moves_to_ingredient_slot(SpecialLayout::Furnace);
    }

    #[test]
    fn blast_furnace_declared_input_moves_to_ingredient_slot() {
        assert_declared_furnace_input_moves_to_ingredient_slot(SpecialLayout::BlastFurnace);
    }

    #[test]
    fn smoker_declared_input_moves_to_ingredient_slot() {
        assert_declared_furnace_input_moves_to_ingredient_slot(SpecialLayout::Smoker);
    }

    #[test]
    fn furnace_declared_input_does_not_spill_into_fuel() {
        assert_declared_furnace_input_does_not_spill_into_fuel(SpecialLayout::Furnace);
    }

    #[test]
    fn blast_furnace_declared_input_does_not_spill_into_fuel() {
        assert_declared_furnace_input_does_not_spill_into_fuel(SpecialLayout::BlastFurnace);
    }

    #[test]
    fn smoker_declared_input_does_not_spill_into_fuel() {
        assert_declared_furnace_input_does_not_spill_into_fuel(SpecialLayout::Smoker);
    }

    #[test]
    fn furnace_non_input_preserves_generic_fuel_slot_fallback() {
        let mut menu = Menu::furnace(SpecialLayout::Furnace);
        let hotbar = menu.slot_count() - 9;
        menu.set_slot_item(0, Some(stack("minecraft:stone", 64)));
        menu.set_slot_item(hotbar, Some(stack("minecraft:raw_iron", 1)));

        Click::shift(hotbar).apply(
            &mut menu,
            PlayerCtx::survival().with_furnace_input_items(vec![id("minecraft:raw_copper")]),
        );

        assert_eq!(count_at(&menu, 1), Some(1), "non-inputs retain generic ordering");
        assert_eq!(count_at(&menu, hotbar), None);
    }

    #[test]
    fn furnace_without_property_set_preserves_generic_fuel_slot_fallback() {
        let mut menu = Menu::furnace(SpecialLayout::Furnace);
        let hotbar = menu.slot_count() - 9;
        menu.set_slot_item(0, Some(stack("minecraft:stone", 64)));
        menu.set_slot_item(hotbar, Some(stack("minecraft:raw_iron", 1)));

        Click::shift(hotbar).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(count_at(&menu, 1), Some(1), "missing data retains generic ordering");
        assert_eq!(count_at(&menu, hotbar), None);
    }

    /// Branches 4 and 5 of vanilla's own player-inventory quick-move step
    /// precede the main↔hotbar hop, so a helmet
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

    /// The regression this test guards against. Vanilla reaches the auto-equip branches
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

    /// Vanilla's own armour-slot placement check is
    /// `slot == equippable.slot()`. A chestplate
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
    /// what the v26-2 prototype census folds in during decode — this crate cannot
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
    /// `wolf_armor` is genuinely `body`, and vanilla's own humanoid-armour gate
    /// excludes `BODY`. If `body` is ever
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
    /// Vanilla's own number-key swap step: swapping a bigger stack
    /// onto a slot whose cap is smaller than the incoming count splits the
    /// overflow into the slot and pushes the slot's *previous* contents back
    /// into the inventory via `inventory.add`.
    ///
    /// The subtlety is aliasing: vanilla's `source` is the *same object* as
    /// its own live inventory-item lookup (returns the live list element, not a
    /// copy), and vanilla's own stack-split step mutates that object in place
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
    /// target simply trade places.
    #[test]
    fn control_hotbar_swap_without_overflow_is_a_plain_exchange() {
        let mut menu = Menu::player();
        menu.set_slot_item(36, Some(stack("minecraft:egg", 10)));
        menu.set_slot_item(9, Some(stack("minecraft:egg", 5)));
        Click::hotbar_swap(9, 0).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 9), Some(10));
        assert_eq!(count_at(&menu, 36), Some(5));
    }

    /// `give_to_player`'s overflow-displacement scan used to be a
    /// plain `0..36` merge-then-fill pass, ignoring vanilla's real priority —
    /// the *selected* hotbar slot first, then the off-hand, only then a linear
    /// scan (vanilla's own slot-with-remaining-space search).
    ///
    /// A torch sits in *both* native 0 (menu slot 36, room for 4) and native 4
    /// (menu slot 40, the *selected* slot, room for 63) when a second torch
    /// stack is displaced from a container slot by an unrelated egg swap.
    /// Vanilla drains the selected slot first and never touches native 0's
    /// torch; the pre-fix scan drained native 0 first because it always
    /// started its linear pass at index 0, regardless of what was selected.
    /// Selected slot is 4, not 0, precisely so a scan that merely "starts from
    /// 0" cannot pass this test by accident (`CLAUDE.md`'s *world*-species
    /// vacuous-test trap). Watched failing pre-fix: native 0 landed at 64 and
    /// native 4 at 2, the old in-order-scan result.
    #[test]
    fn swap_overflow_gives_to_the_selected_hotbar_slot_before_a_lower_index() {
        let mut menu = Menu::player();
        // hotbar key 8 -> native 8 -> menu slot 44: the oversized source.
        menu.set_slot_item(
            44,
            Some(stack("minecraft:egg", 20).with_max_stack_size(16)),
        );
        // Target: main storage slot 9 holds a torch, unrelated to the egg
        // swap, so its displacement exercises `give_to_player`'s ordinary scan
        // rather than the same-item remainder-merge finding 1 already fixed.
        menu.set_slot_item(9, Some(stack("minecraft:torch", 5)));
        // A torch at native 0, almost full: an in-order 0..36 scan drains into
        // this one first.
        menu.set_slot_item(36, Some(stack("minecraft:torch", 60)));
        // A torch at native 4, the *selected* hotbar slot, with plenty of room.
        menu.set_slot_item(40, Some(stack("minecraft:torch", 1)));

        let ctx = PlayerCtx {
            infinite_materials: false,
            can_drop: true,
            selected_hotbar_slot: 4,
            furnace_input_items: None,
        };
        Click::hotbar_swap(9, 8).apply(&mut menu, ctx);

        assert_eq!(
            count_at(&menu, 36),
            Some(60),
            "native 0's torch must be untouched — the selected slot is tried first"
        );
        assert_eq!(
            count_at(&menu, 40),
            Some(6),
            "the displaced 5 torches land in the selected hotbar slot"
        );
    }

    /// The control for the test above: with no *selected* slot preference in
    /// play (slot 0 selected, matching every other test's default), the
    /// linear-scan behaviour is unchanged — the lowest-index mergeable slot
    /// still wins, same as before this fix.
    #[test]
    fn control_swap_overflow_without_a_selected_slot_still_scans_from_zero() {
        let mut menu = Menu::player();
        menu.set_slot_item(
            44,
            Some(stack("minecraft:egg", 20).with_max_stack_size(16)),
        );
        menu.set_slot_item(9, Some(stack("minecraft:torch", 5)));
        menu.set_slot_item(36, Some(stack("minecraft:torch", 60)));
        menu.set_slot_item(40, Some(stack("minecraft:torch", 1)));

        Click::hotbar_swap(9, 8).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(count_at(&menu, 36), Some(64), "native 0 fills first when it is not the selected slot");
        assert_eq!(count_at(&menu, 40), Some(2), "only the 1 remaining torch reaches native 4");
    }

    // --- item-combiner menus: anvil / grindstone / smithing / enchanting ---

    /// [`Menu::item_combiner`]'s result slot is take-only, matching
    /// vanilla's own item-combiner result-slot placement override
    /// — the anvil/grindstone shape
    /// (`container_size = 3, result_slot = 2`).
    #[test]
    fn item_combiner_result_slot_rejects_placement() {
        let menu = Menu::item_combiner(3, 2, SpecialLayout::Anvil);
        assert!(
            !menu.may_place(2, &stack("minecraft:diamond_pickaxe", 1)),
            "the result slot must reject a placed item, matching ItemCombinerMenu"
        );
        // The two input slots are untouched — anvil's own placement predicate
        // accepts anything, so this only proves the
        // result slot is the *one* that changed.
        assert!(menu.may_place(0, &stack("minecraft:diamond_pickaxe", 1)));
        assert!(menu.may_place(1, &stack("minecraft:diamond_pickaxe", 1)));
    }

    /// The smithing table shape: `container_size = 4, result_slot = 3`.
    #[test]
    fn item_combiner_covers_the_smithing_table_shape() {
        let menu = Menu::item_combiner(4, 3, SpecialLayout::Smithing);
        assert!(!menu.may_place(3, &stack("minecraft:netherite_upgrade_smithing_template", 1)));
        assert!(menu.may_place(0, &stack("minecraft:netherite_upgrade_smithing_template", 1)));
        assert!(menu.may_place(1, &stack("minecraft:diamond_pickaxe", 1)));
        assert!(menu.may_place(2, &stack("minecraft:netherite_ingot", 1)));
    }

    /// [`Menu::enchanting_table`]'s slot 1 accepts only lapis lazuli
    /// (vanilla's own enchanting-table lapis-slot restriction); slot 0 (the item to enchant) accepts
    /// anything, matching the plain `Slot` vanilla gives it.
    #[test]
    fn enchanting_table_lapis_slot_rejects_non_lapis() {
        let menu = Menu::enchanting_table();
        assert!(
            !menu.may_place(1, &stack("minecraft:diamond", 1)),
            "the lapis slot must reject a non-lapis item"
        );
        assert!(
            menu.may_place(1, &stack("minecraft:lapis_lazuli", 1)),
            "the lapis slot must accept lapis lazuli"
        );
        assert!(menu.may_place(0, &stack("minecraft:diamond_sword", 1)));
    }

    /// Control for the test above: an *ordinary* generic container never
    /// applies the lapis restriction, so this can only pass because
    /// `enchanting_table` specifically marked slot 1 — not because `may_place`
    /// rejects diamonds everywhere.
    #[test]
    fn control_generic_container_has_no_lapis_restriction() {
        let menu = Menu::generic(2);
        assert!(menu.may_place(1, &stack("minecraft:diamond", 1)));
    }

    /// The anvil and grindstone are mechanically identical (`container_size =
    /// 3, result_slot = 2`) but must carry *different* [`SpecialLayout`]s —
    /// `lodestone-shell`'s `slot_layout` places their three slots at
    /// completely different pixel positions, and `Menu` has no other field that could
    /// tell them apart.
    #[test]
    fn special_layout_distinguishes_menus_with_identical_mechanics() {
        let anvil = Menu::item_combiner(3, 2, SpecialLayout::Anvil);
        let grindstone = Menu::item_combiner(3, 2, SpecialLayout::Grindstone);
        assert_eq!(anvil.special_layout(), Some(SpecialLayout::Anvil));
        assert_eq!(grindstone.special_layout(), Some(SpecialLayout::Grindstone));
        assert_eq!(Menu::enchanting_table().special_layout(), Some(SpecialLayout::Enchanting));
        assert_eq!(
            Menu::generic(3).special_layout(),
            None,
            "an ordinary generic container has no special layout"
        );
    }

    // -- plan_recipe_auto_fill ------------------------------

    /// A crafting table's 3×3: coal at menu slot 12, a stick at menu slot 20
    /// (both inside the `10..=36` main-storage range this menu reports —
    /// see the module's own slot-order table), recipe wants coal above stick
    /// in a **1-wide** pattern placed into the real **3-wide** grid.
    /// `Recipe::placement` lays that pattern out row-major against the
    /// grid's own width, so coal (row 0, col 0) is grid cell `0` and stick
    /// (row 1, col 0) is grid cell `3` — **not** cell `1`, which is what a
    /// hand-count that forgot the grid is 3 cells per row, not 1, would
    /// predict. `craft.first_input == 1` then offsets both: menu slots `1`
    /// and `4`.
    #[test]
    fn plan_recipe_auto_fill_offsets_crafting_table_cells_by_first_input() {
        let mut menu = Menu::crafting(3, 3);
        menu.set_slot_item(12, Some(stack("minecraft:coal", 5)));
        menu.set_slot_item(20, Some(stack("minecraft:stick", 3)));
        let torch = crate::recipe::Recipe::Shaped(crate::recipe::ShapedRecipe::new(
            1,
            2,
            vec![
                Some(crate::recipe::Ingredient::Item(id("minecraft:coal"))),
                Some(crate::recipe::Ingredient::Item(id("minecraft:stick"))),
            ],
            stack("minecraft:torch", 4),
        ));
        let tags = crate::recipe::TagResolver::new();
        let plan = menu
            .plan_recipe_auto_fill(&torch, &tags)
            .expect("both ingredients present in main storage");
        assert_eq!(
            plan,
            vec![
                crate::recipe::PlacementStep { cell: 1, source_slot: 12 },
                crate::recipe::PlacementStep { cell: 4, source_slot: 20 },
            ]
        );
    }

    /// A furnace-family menu has no `craft_layout` at all — its single
    /// ingredient slot is menu index `0`, reached through
    /// `special_layout` instead. Predicts `cell: 0` unmodified (no offset to
    /// apply, unlike the crafting-table case above).
    #[test]
    fn plan_recipe_auto_fill_targets_furnace_ingredient_slot_zero() {
        let mut menu = Menu::furnace(SpecialLayout::Furnace);
        // Main storage starts at `container_size == 3`.
        menu.set_slot_item(15, Some(stack("minecraft:porkchop", 8)));
        let smelting = crate::recipe::Recipe::Cooking(crate::recipe::CookingRecipe {
            kind: crate::recipe::CookingKind::Smelting,
            ingredient: crate::recipe::Ingredient::Item(id("minecraft:porkchop")),
            result: stack("minecraft:cooked_porkchop", 1),
            experience: 0.35,
            cooking_time: 200,
            category: crate::recipe::RecipeCategory::Food,
        });
        let tags = crate::recipe::TagResolver::new();
        let plan = menu.plan_recipe_auto_fill(&smelting, &tags).expect("porkchop is in main storage");
        assert_eq!(plan, vec![crate::recipe::PlacementStep { cell: 0, source_slot: 15 }]);
    }

    /// A blast-furnace recipe never matches a plain furnace's ingredient
    /// slot: `CookingRecipe::placement` only returns `Some` for `(1, 1)`, and
    /// `Recipe::book_type` distinguishes furnace/blast-furnace/smoker, but
    /// `plan_recipe_auto_fill` does not itself check the kind matches the
    /// menu — this pins that a *smoking*-only ingredient (raw chicken, not
    /// modelled as smeltable here) with no matching inventory item still
    /// correctly returns `None` via `plan_auto_fill`'s own all-or-nothing
    /// rule, rather than silently placing the wrong thing.
    #[test]
    fn plan_recipe_auto_fill_returns_none_when_ingredient_is_absent() {
        let menu = Menu::furnace(SpecialLayout::Furnace);
        let smelting = crate::recipe::Recipe::Cooking(crate::recipe::CookingRecipe {
            kind: crate::recipe::CookingKind::Smelting,
            ingredient: crate::recipe::Ingredient::Item(id("minecraft:iron_ore")),
            result: stack("minecraft:iron_ingot", 1),
            experience: 0.7,
            cooking_time: 200,
            category: crate::recipe::RecipeCategory::Blocks,
        });
        let tags = crate::recipe::TagResolver::new();
        assert_eq!(menu.plan_recipe_auto_fill(&smelting, &tags), None);
    }

    /// A menu with neither a crafting grid nor a furnace-family
    /// `special_layout` (a plain chest) has nothing to auto-fill at all.
    #[test]
    fn plan_recipe_auto_fill_none_for_a_menu_with_no_grid() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(30, Some(stack("minecraft:coal", 5)));
        let torch = crate::recipe::Recipe::Shaped(crate::recipe::ShapedRecipe::new(
            1,
            1,
            vec![Some(crate::recipe::Ingredient::Item(id("minecraft:coal")))],
            stack("minecraft:torch", 4),
        ));
        let tags = crate::recipe::TagResolver::new();
        assert_eq!(menu.plan_recipe_auto_fill(&torch, &tags), None);
    }

    /// Auto-fill never draws from armour or off-hand, even when they hold a
    /// matching item — vanilla's own placement helper only ever walks
    /// the player's main+hotbar list. The player-inventory screen's 2×2
    /// puts armour at menu slots `5..=8`; a coal "helmet" placed there must
    /// be invisible to the planner.
    #[test]
    fn plan_recipe_auto_fill_never_draws_from_armour_or_offhand() {
        let mut menu = Menu::player();
        menu.set_slot_item(5, Some(stack("minecraft:coal", 1))); // armour range
        menu.set_slot_item(45, Some(stack("minecraft:coal", 1))); // off-hand
        let torch = crate::recipe::Recipe::Shapeless(crate::recipe::ShapelessRecipe::new(
            vec![crate::recipe::Ingredient::Item(id("minecraft:coal"))],
            stack("minecraft:torch", 4),
        ));
        let tags = crate::recipe::TagResolver::new();
        assert_eq!(menu.plan_recipe_auto_fill(&torch, &tags), None);
    }
}
