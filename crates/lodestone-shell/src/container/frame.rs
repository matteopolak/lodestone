//! The per-frame input record ([`ContainerFrame`]) and vanilla's two label
//! anchors.
//!
//! Split out of `container.rs` verbatim.

use lodestone_game::menu::{Menu, MenuKind};
use lodestone_game::recipe::RecipeBook;

use crate::hud::VanillaFont;

use super::layout::SlotLayout;

/// The container screen to draw for one frame.
#[derive(Debug, Clone, Copy)]
pub struct ContainerFrame<'a> {
    /// Menu contents to draw. `None` draws nothing.
    pub menu: Option<&'a Menu>,
    /// The screen's own title, already resolved to words — vanilla's
    /// `AbstractContainerScreen.title`. Drawn at
    /// [`LabelLayout::title_x`]/[`title_y`](LabelLayout::title_y), which is
    /// **not** always `(8, 6)`: see [`label_layout`].
    ///
    /// For a server-opened container this is the `Text` from `OPEN_SCREEN` run
    /// through the language table ([`menu_title`]), so a chest renamed in an
    /// anvil opens as its custom name. Nothing here consults a table keyed on
    /// menu type; the generic name is only the server's default.
    pub title: &'a str,
    /// Vanilla's *second* label — `AbstractContainerScreen.playerInventoryTitle`,
    /// the word "Inventory" over the player's own storage rows.
    ///
    /// Unlike [`title`](Self::title) this never comes from a packet: vanilla
    /// reads it from `Inventory.getDisplayName()`, whose default is the
    /// client-side constant `Component.translatable("container.inventory")`
    /// (`Inventory.java:55`), so resolving it locally *is* the vanilla
    /// behaviour. The default below is `en_us.json:3218`'s value, which is what
    /// a jar-less run and every hermetic gate see; `app.rs` overrides it with
    /// the same key run through the live language table so a non-English client
    /// gets its own word.
    ///
    /// Drawn only when [`LabelLayout::inventory`] is `Some` — the player
    /// inventory screen omits it (`InventoryScreen.extractLabels`).
    pub inventory_label: &'a str,
    /// Viewport-pixel position of the mouse cursor, the same coordinate space
    /// [`hit_test`] takes — **not** local widget coordinates. `None` (the
    /// default from [`new`](Self::new)) draws no carried stack even if
    /// [`Menu::carried`] holds one, which is what keeps every existing caller
    /// (headless builds, the pixel gates, `tests/container_screen.rs`)
    /// unchanged: nothing here reads this field unless a caller opts in
    /// through [`with_cursor`](Self::with_cursor).
    pub cursor: Option<[f32; 2]>,
    /// The local recipe corpus (see `crate::resources::load_recipe_book`), for
    /// a **ghost preview** of the crafting result: `None` (the default) draws
    /// nothing extra, which is what keeps every existing caller (headless
    /// builds, the pixel gates, `tests/container_screen.rs`) unchanged. See
    /// [`with_recipe_book`](Self::with_recipe_book).
    pub recipe_book: Option<&'a RecipeBook>,
    /// The in-progress paint-drag, as `(drag type, painted slots)` — exactly
    /// [`MenuInput::drag_paint`]'s output, which is vanilla's
    /// `(quickCraftingType, quickCraftSlots)` pair.
    ///
    /// `None` (the default) draws no preview, which is what keeps every existing
    /// caller unchanged. See [`with_drag`](Self::with_drag) for what it draws and
    /// why the counts cannot disagree with the release.
    pub drag: Option<(i32, &'a [usize])>,
    /// The wire `menu_type` from `OPEN_SCREEN` (`OpenMenuSnapshot::menu_type`),
    /// when this frame is a server-opened container.
    ///
    /// `None` on the player inventory screen — which involves no packet — and on
    /// every caller that predates this field; both keep [`label_layout`]'s own
    /// anchor. Read only by [`menu_type_title_anchor`], for the nine real screens
    /// whose title anchor vanilla overrides.
    pub menu_type: Option<&'a lodestone_model::ResourceKey>,
    /// `container_set_data` properties of the open menu
    /// (`OpenMenuSnapshot::data`), as `(property_id, value)` — the anvil's XP
    /// level cost (property `0`) and the enchanting table's three per-row
    /// level costs (properties `0..3`). `&[]` (the default) draws neither
    /// cost, which is what keeps every existing caller (headless builds, the
    /// pixel gates, `tests/container_screen.rs`) unchanged. See
    /// [`with_cost_context`](Self::with_cost_context) and
    /// `docs/container-cost-screens.md`.
    pub cost_data: &'a [(i32, i32)],
    /// `Player.hasInfiniteMaterials()` — `Abilities.instabuild`
    /// (`AnvilMenu.java:70-71`, `EnchantmentScreen.java:111`). Gates the
    /// anvil's "Too Expensive!" branch and the enchanting rows' afford
    /// check. `false` (the default) is the honest value for every existing
    /// caller and for a survival session.
    pub has_infinite_materials: bool,
    /// The local player's XP level, for the same afford checks
    /// (`AnvilMenu.mayPickup`, `EnchantmentScreen.java:111`). `0` (the
    /// default) matches every existing caller.
    pub xp_level: i32,
}

impl<'a> ContainerFrame<'a> {
    /// A frame for an optional menu, with no cursor position — the carried
    /// stack (if any) will not draw. Chain [`with_cursor`](Self::with_cursor)
    /// to supply one.
    #[must_use]
    pub fn new(menu: Option<&'a Menu>, title: &'a str) -> Self {
        Self {
            menu,
            title,
            inventory_label: DEFAULT_INVENTORY_LABEL,
            cursor: None,
            recipe_book: None,
            drag: None,
            menu_type: None,
            cost_data: &[],
            has_infinite_materials: false,
            xp_level: 0,
        }
    }

    /// A frame that deliberately draws nothing.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            menu: None,
            title: "",
            inventory_label: DEFAULT_INVENTORY_LABEL,
            cursor: None,
            recipe_book: None,
            drag: None,
            menu_type: None,
            cost_data: &[],
            has_infinite_materials: false,
            xp_level: 0,
        }
    }

    /// Override the player-inventory label with a translated one — see
    /// [`inventory_label`](Self::inventory_label) and
    /// [`player_inventory_label`].
    #[must_use]
    pub fn with_inventory_label(mut self, label: &'a str) -> Self {
        self.inventory_label = label;
        self
    }

    /// Attach the mouse position, in viewport pixels, so a loaded cursor
    /// (`menu.carried().is_some()`) draws the carried stack centred on it.
    #[must_use]
    pub fn with_cursor(mut self, cursor: Option<[f32; 2]>) -> Self {
        self.cursor = cursor;
        self
    }

    /// Attach the server's own `menu_type`, so [`menu_type_title_anchor`] can
    /// correct the title anchor for the screens [`label_layout`] does not model.
    /// `None` (the default) keeps the existing anchor.
    #[must_use]
    pub fn with_menu_type(mut self, menu_type: Option<&'a lodestone_model::ResourceKey>) -> Self {
        self.menu_type = menu_type;
        self
    }

    /// Attach the anvil/enchanting-table cost context: the raw
    /// `container_set_data` properties (`OpenMenuSnapshot::data`), whether
    /// the local player has infinite materials, and their XP level. `&[]`
    /// (the default from [`new`](Self::new)) draws neither cost, which is
    /// what keeps every existing caller unchanged. See [`cost_data`](Self::cost_data).
    #[must_use]
    pub fn with_cost_context(
        mut self,
        data: &'a [(i32, i32)],
        has_infinite_materials: bool,
        xp_level: i32,
    ) -> Self {
        self.cost_data = data;
        self.has_infinite_materials = has_infinite_materials;
        self.xp_level = xp_level;
        self
    }

    /// Attach a recipe book so an **empty** crafting result slot draws a
    /// dimmed ghost preview of what the grid would produce — never the real
    /// (undimmed) icon, and never written into `menu` itself. The server's own
    /// `container_set_slot` remains the only thing that ever fills the result
    /// slot for real; see `docs/crafting.md`'s "who computes the result slot".
    #[must_use]
    pub fn with_recipe_book(mut self, book: Option<&'a RecipeBook>) -> Self {
        self.recipe_book = book;
        self
    }

    /// Attach the in-progress paint-drag so the screen draws vanilla's **live
    /// preview** (issue #378 part 2): in every painted cell, a 50%-white wash and
    /// the provisional stack the release would leave there, with a clamped count
    /// in yellow; and on the cursor, the count it would be left holding.
    ///
    /// Pass [`MenuInput::drag_paint`]'s output directly. `None` (the default)
    /// draws nothing extra.
    ///
    /// # The counts come from the release path, not from here
    ///
    /// Every number this draws is [`Menu::quick_craft_plan`]'s, the same function
    /// `finish_quick_craft` distributes with. A preview that disagreed with the
    /// outcome would be worse than no preview, so the arithmetic is shared rather
    /// than mirrored — see that method's own doc comment, and
    /// `docs/container-screen.md`.
    #[must_use]
    pub fn with_drag(mut self, drag: Option<(i32, &'a [usize])>) -> Self {
        self.drag = drag;
        self
    }
}

/// Resolve an open menu's server-authored title into the plain string
/// [`ContainerFrame::title`] draws.
///
/// A server does not send the words "Crafting"; it sends
/// `translate("container.crafting")` in `ClientboundOpenScreen`. Flattening that
/// with [`lodestone_model::Text::to_plain_string`] consults the model's tiny
/// built-in stub table (fourteen chat/death keys — `text.rs`'s
/// `default_translation`), which has no `container.*` entry, so the key falls
/// through to itself and **the raw key is what the panel draws** (issue #52).
///
/// This is the same read-boundary resolution the chat feed, the tab list and the
/// scoreboard sidebar already do; the container screen was the one HUD surface
/// that skipped it. `translate` is the language table — an
/// `lodestone_assets::Language` becomes one via `Language::translator`, and
/// `Sim::translator` hands out exactly that closure.
///
/// A missing key still falls back to the component's own `fallback`, then to the
/// key: losing a translation must never cost the title.
#[must_use]
pub fn menu_title(
    title: &lodestone_model::Text,
    translate: &dyn Fn(&str) -> Option<String>,
) -> String {
    lodestone_game::text::resolve_to_string(title, translate)
}

/// `en_us.json:3218`'s value for `container.inventory` — the fallback
/// [`ContainerFrame::inventory_label`] carries when no caller supplies a
/// translated one.
const DEFAULT_INVENTORY_LABEL: &str = "Inventory";

/// The player inventory screen's own title: **"Crafting"**, not "Inventory".
///
/// `InventoryScreen.java:28` passes `Component.translatable("container.crafting")`
/// to `super`, naming the 2×2 grid rather than the screen. This client used to
/// hardcode the string `"Inventory"` here (`app.rs`), which is wrong twice over:
/// wrong word, and — because it went in as the *title* — drawn at the title
/// anchor, which for this one screen is `x = 97` (`InventoryScreen.java:29`), not
/// `x = 8`.
///
/// Resolved through the language table for the same reason [`menu_title`] is: a
/// raw `container.crafting` on screen is issue #52's defect class.
#[must_use]
pub fn player_inventory_title(translate: &dyn Fn(&str) -> Option<String>) -> String {
    menu_title(
        &lodestone_model::Text::translate("container.crafting", vec![]),
        translate,
    )
}

/// Vanilla's `playerInventoryTitle` — `container.inventory`, "Inventory".
///
/// A *client-side* constant in vanilla too (`Inventory.java:55`'s `DEFAULT_NAME`),
/// so unlike a container's title this is legitimately resolved locally rather
/// than read off a packet. See [`ContainerFrame::inventory_label`].
#[must_use]
pub fn player_inventory_label(translate: &dyn Fn(&str) -> Option<String>) -> String {
    menu_title(
        &lodestone_model::Text::translate("container.inventory", vec![]),
        translate,
    )
}

/// Where a screen's two labels go, in **local widget pixels** (add
/// [`panel_origin`] to reach the canvas).
///
/// The reason this is a computed record and not four constants: `inventoryLabelY`
/// is `imageHeight - 94`, and `imageHeight` moves with the row count. Restating
/// it as a number is the exact failure `CLAUDE.md` documents for the HUD's
/// `cluster_top` — a gate measured 20 logical pixels above a row that was drawing
/// perfectly and reported zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabelLayout {
    /// `titleLabelX`. `8` on a generic container, `29` on a crafting table,
    /// `97` on the player inventory screen.
    pub title_x: f32,
    /// `titleLabelY` — `6` everywhere in vanilla.
    pub title_y: f32,
    /// `(inventoryLabelX, inventoryLabelY)`, or `None` on the one screen that
    /// draws no such label.
    pub inventory: Option<[f32; 2]>,
}

/// Vanilla's label anchors for `menu`'s screen, derived from `layout` rather than
/// restated.
///
/// Read out of `.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/inventory/`:
///
/// | screen | `titleLabelX` | second label | source |
/// |---|---|---|---|
/// | generic container | `8` | yes | `AbstractContainerScreen.java:68-71` |
/// | crafting table | `29` | yes | `CraftingScreen.java:22` |
/// | player inventory | `97` | **no** | `InventoryScreen.java:29,73-75` |
///
/// The player inventory screen is the only one that omits the second label, and
/// it does so by *overriding `extractLabels`* to drop the second `graphics.text`
/// call entirely (`InventoryScreen.java:73-75`) — so the label is not wrong in
/// general, only there. Deleting it globally would trade one bug for another.
///
/// `inventory` is `[8, layout.height - 94]`: `inventoryLabelX = 8` and
/// `inventoryLabelY = imageHeight - 94` (`AbstractContainerScreen.java:70-71`,
/// restated by `ContainerScreen.java:17` for the row-count-dependent chest).
/// [`SlotLayout::height`] *is* `imageHeight` — 166 for the player and crafting
/// panels, `114 + rows * 18` for a chest, both matching vanilla's own
/// constructors — so this is the same expression the panel art is blitted with,
/// not a parallel one that can drift.
///
/// The nine screens whose anchors vanilla overrides away from these two are
/// **not** handled here, and deliberately so: they need the wire `menu_type` and
/// (for the centred ones) the measured title width, neither of which this
/// function takes. [`menu_type_title_anchor`] carries them and overrides this
/// function's result at the one call site in `build_inner`.
///
/// This paragraph used to say the centred furnace title was simply "not
/// modelled", waiting on a furnace `MenuKind`. That framing was wrong in a way
/// worth recording: a furnace does not need a `MenuKind` at all — the anchor
/// keys off `menu_type`, which the server already sends and
/// `OpenMenuSnapshot::menu_type` already carries. Growing `MenuKind` for it
/// would have been the expensive way round, and is constrained against anyway
/// (`lodestone-game/src/menus.rs`: `MenuKind` is matched exhaustively in
/// [`slot_layout`]).
#[must_use]
pub fn label_layout(menu: &Menu, layout: &SlotLayout) -> LabelLayout {
    match menu.kind() {
        MenuKind::Player => LabelLayout {
            title_x: 97.0,
            title_y: 6.0,
            inventory: None,
        },
        MenuKind::Generic { .. } => LabelLayout {
            title_x: if menu.craft_layout().is_some() { 29.0 } else { 8.0 },
            title_y: 6.0,
            inventory: Some([8.0, layout.height - 94.0]),
        },
    }
}

/// Vanilla's `titleLabelX`/`titleLabelY` for the menu types whose real screen
/// overrides them away from [`label_layout`]'s two anchors — the screens that
/// function's doc comment names as unmodelled.
///
/// Read from `.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/inventory/`,
/// and each line below was re-read from the decompile rather than taken from a
/// summary:
///
/// | wire `menu_type` | screen | `titleLabelX` | `titleLabelY` | source |
/// |---|---|---|---|---|
/// | `furnace` / `blast_furnace` / `smoker` | `AbstractFurnaceScreen` subclasses | centred | `6` | `AbstractFurnaceScreen.java:39` |
/// | `brewing_stand` | `BrewingStandScreen` | centred | `6` | `BrewingStandScreen.java:25` |
/// | `generic_3x3` | `DispenserScreen` (dispenser **and** dropper) | centred | `6` | `DispenserScreen.java:20` |
/// | `crafter_3x3` | `CrafterScreen` | centred | `6` | `CrafterScreen.java:33` |
/// | `anvil` | `AnvilScreen` | `60` | `6` | `AnvilScreen.java:30` |
/// | `loom` | `LoomScreen` | `8` | `4` | `LoomScreen.java:68` (`titleLabelY -= 2`) |
/// | `stonecutter` | `StonecutterScreen` | `8` | `5` | `StonecutterScreen.java:45` (`titleLabelY--`) |
/// | `cartography_table` | `CartographyTableScreen` | `8` | `4` | `CartographyTableScreen.java:29` (`titleLabelY -= 2`) |
///
/// Note the last three are expressed in vanilla as *decrements of the inherited
/// `titleLabelY`*, not as absolute values. They are resolved to absolutes here
/// because the inherited value is `6` in all three cases; if `label_layout`'s
/// `title_y` ever stops being `6` for a `Generic`, these three become wrong and
/// nothing would say so — which is why it is written down rather than left to be
/// inferred from the table.
///
/// "Centred" is vanilla's `(imageWidth - font.width(title)) / 2`, **integer**
/// division in Java (`titleLabelX` is an `int`), so it truncates toward zero;
/// matched with `.floor()`, which agrees for every real title because they are
/// all narrower than the panel and the numerator is therefore non-negative.
/// `layout.width` is used rather than a literal `176.0` because it already *is*
/// vanilla's `imageWidth` for every type in the table.
///
/// A `None` return means "no override" and the caller keeps [`label_layout`]'s
/// anchor. Every other server-openable type genuinely matches `(8, 6)` in
/// vanilla too — `grindstone`, `hopper`, `shulker_box`, `enchantment` and every
/// `generic_9x*` have no override at all — so they are absent from this table
/// rather than listed as no-ops, and `crafting` is absent because
/// `label_layout`'s own `craft_layout()` branch already places it at `29`.
///
/// **`beacon` and `merchant` are excluded on purpose.** Both draw at a different
/// `imageWidth` (230 and 276) with their own background art, and
/// `MerchantScreen.extractLabels` composes trade-level text into the title
/// rather than merely moving the anchor (`MerchantScreen.java:85-98`). Neither
/// has a case in [`background_kind`] or [`slot_layout`], so an anchor alone would
/// place correct text over a still-wrong-shaped panel. They belong with their
/// own layout work.
#[must_use]
pub fn menu_type_title_anchor(
    menu_type: Option<&lodestone_model::ResourceKey>,
    layout: &SlotLayout,
    title: &str,
    font: Option<&VanillaFont>,
) -> Option<[f32; 2]> {
    let key = menu_type?;
    if key.namespace() != "minecraft" {
        return None;
    }
    if matches!(
        key.path(),
        "furnace" | "blast_furnace" | "smoker" | "brewing_stand" | "generic_3x3" | "crafter_3x3"
    ) {
        // A jar-less run has no font to measure with. Falling back to a width of
        // zero centres the *anchor* rather than the text, which is visibly wrong
        // — but it is the same degradation the labels already take on that path
        // (they draw in the fixed-advance debug font), so it is consistent rather
        // than a new divergence.
        let text_width = font.map_or(0.0, |f| f.width(title, 1.0));
        return Some([((layout.width - text_width) / 2.0).floor(), 6.0]);
    }
    match key.path() {
        "anvil" => Some([60.0, 6.0]),
        "loom" | "cartography_table" => Some([8.0, 4.0]),
        "stonecutter" => Some([8.0, 5.0]),
        _ => None,
    }
}
