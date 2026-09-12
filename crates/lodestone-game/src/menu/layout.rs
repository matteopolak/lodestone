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


