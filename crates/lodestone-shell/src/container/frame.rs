//! The per-frame input record ([`ContainerFrame`]) and vanilla's two label
//! anchors.
//!
//! Split out of `container.rs` verbatim.

use lodestone_game::menu::{Menu, MenuKind};
use lodestone_game::recipe::RecipeBook;
use lodestone_game::trades::TradeOffers;

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
    /// (`Inventory.java`), so resolving it locally *is* the vanilla
    /// behaviour. The default below is `en_us.json`'s value, which is what
    /// a jar-less run and every hermetic gate see; `app.rs` overrides it with
    /// the same key run through the live language table so a non-English client
    /// gets its own word.
    ///
    /// Drawn only when [`LabelLayout::inventory`] is `Some` — the player
    /// inventory screen omits it (`InventoryScreen.extractLabels`).
    pub inventory_label: &'a str,
    /// The player's active status effects, as
    /// `EffectsInInventory` would draw them beside the panel — already sorted,
    /// **translated** and duration-formatted by
    /// [`crate::effects::inventory_rows`].
    ///
    /// Empty (the default) draws no column, which is what every headless
    /// caller and every hermetic gate sees. Drawn only on
    /// [`MenuKind::Player`]: vanilla constructs an `EffectsInInventory` in
    /// `InventoryScreen` and `CreativeModeInventoryScreen` only, and every
    /// other screen's `Screen.showsActiveEffects()` returns `false`.
    ///
    /// The rows are resolved *outside* this crate's draw path because the
    /// language table lives on `Sim`; passing raw ids here and naming them at
    /// the draw site is how this widget came to render `speed` instead of
    /// "Speed" in the first place.
    pub effects: &'a [crate::effects::InventoryEffectRow],
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
    /// The live pose for the **inventory avatar** — see
    /// [`super::PlayerAvatar::pose`]. `AnimInput::REST` (the default from
    /// [`new`](Self::new)) draws a standing player, which is what keeps every
    /// existing caller (headless builds, the pixel gates,
    /// `tests/container_screen.rs`) unchanged. See
    /// [`with_avatar_pose`](Self::with_avatar_pose).
    pub avatar_pose: lodestone_render::AnimInput,
    /// The local player's own uuid, so the inventory avatar's *default* skin
    /// (when nothing local or fetched has overridden it) can be resolved the
    /// same way `entities.rs::default_remote_skin` resolves the world-side
    /// default for every other player: `lodestone_assets::skin::
    /// default_skin_for_uuid`, keyed on this one uuid. `None` (the default
    /// from [`new`](Self::new)) is what every caller without a live session
    /// keeps — the avatar then falls back to `PlayerPreview`'s own
    /// construction-time default, exactly as before this field existed.
    ///
    /// See `container/player_preview.rs`'s `PlayerPreview::
    /// maybe_default_from_uuid` for the consumer — issue #646's "both sites
    /// derive from one resolver, keyed on the same uuid" requirement.
    pub avatar_uuid: Option<uuid::Uuid>,
    /// `Player.hasInfiniteMaterials()` — `Abilities.instabuild`
    /// (`AnvilMenu.java`, `EnchantmentScreen.java`). Gates the
    /// anvil's "Too Expensive!" branch and the enchanting rows' afford
    /// check. `false` (the default) is the honest value for every existing
    /// caller and for a survival session.
    pub has_infinite_materials: bool,
    /// The local player's XP level, for the same afford checks
    /// (`AnvilMenu.mayPickup`, `EnchantmentScreen.java`). `0` (the
    /// default) matches every existing caller.
    pub xp_level: i32,
    /// Whether to draw the hovered slot's tooltip, and whether
    /// `advancedItemTooltips` (F3+H) is on — `Some(advanced)`.
    ///
    /// `None` (the default) draws no tooltip at all, which is what keeps every
    /// existing caller unchanged: the headless gates, `tests/container_screen.rs`
    /// and every pixel gate build their frames without it, and a tooltip appearing
    /// under a gate's cursor would change what those gates measure. See
    /// [`with_tooltips`](Self::with_tooltips) and `super::tooltip`.
    ///
    /// A `bool` inside the `Option` rather than two fields because "draw one" and
    /// "which flag" are always decided together: `advanced` alone could not
    /// express "no tooltip", and the pair could disagree.
    pub tooltips: Option<bool>,
    /// Whether the recipe book's panel is open, which **moves the container
    /// panel** — vanilla's `RecipeBookComponent.updateScreenPosition`. See
    /// [`super::layout::recipe_book_panel_shift`].
    ///
    /// `false` (the default) is the unshifted centring every existing caller and
    /// every pixel gate already measures. A caller that sets this **must** pass the
    /// same value to [`super::layout::hit_test_with_book`], or clicks land on the
    /// wrong slot while the screen looks right — this module's standing hazard.
    pub book_open: bool,
    /// Whether an **overlay above this screen owns the pointer** this frame, so
    /// no hovered slot resolves under it: no highlight sprites, no tooltip.
    ///
    /// `false` (the default) is every existing caller. The one real producer is
    /// `redraw`, which sets it from the same
    /// [`recipe_book_panel_hit_test_with_scale`](super::recipe_book_panel_hit_test_with_scale)
    /// predicate `container_input`'s **click** path already consults *before* the
    /// container's own hit test. The click path has consumed the pointer over the
    /// panel since the panel landed; the draw had no equivalent, so the highlight
    /// and the tooltip resolved to whatever slot happened to sit geometrically
    /// beneath the book.
    ///
    /// # Why this is a separate flag and not "withhold the cursor"
    ///
    /// [`cursor`](Self::cursor) also positions the **carried stack**, which must
    /// keep following the pointer over the panel — vanilla drags a held item
    /// across the recipe book perfectly happily. Clearing the cursor would
    /// suppress the hovered slot *and* park the carried stack, so the suppression
    /// has to be specific to hovered-slot resolution. That is this field.
    pub hover_blocked: bool,
    /// The open merchant's trade list (that fix's UI half) — `None` (the
    /// default) draws no trade rows, which is every existing caller (a
    /// merchant screen with no offers yet, or any non-merchant menu). See
    /// [`with_trades`](Self::with_trades) and `super::merchant`.
    pub trades: Option<&'a TradeOffers>,
    /// Which trade row is selected — vanilla's `MerchantScreen.shopItem`,
    /// which trade's out-of-stock overlay and progress bar (if any) show, and
    /// the index the next `SELECT_TRADE` send carries. `0` (the default)
    /// matches vanilla's own initial value.
    pub selected_trade: usize,
    /// Vanilla's `merchant.trades` second label, resolved through the
    /// language table exactly as [`inventory_label`](Self::inventory_label)
    /// is — `"Trades"` (the default) is `en_us.json`'s value, drawn on a
    /// jar-less run and by every existing caller. See
    /// [`with_trades`](Self::with_trades).
    pub trades_label: &'a str,
    /// The anvil's rename box **value** — vanilla's `EditBox::getValue()`
    /// (`AnvilScreen`'s `name` field). `None`/empty (the default) draws no
    /// text in the box, which is every existing caller and every non-anvil
    /// menu.
    ///
    /// **Keyboard-wired since issue #603.** This used to say there was no
    /// per-keystroke state anywhere in the crate, so the value could only
    /// ever be `slotChanged`'s own default (the slot-0 item's hover name),
    /// never anything the player had typed. `crate::container::AnvilRenameState`
    /// is that state now — held on `WindowApp`, synced from the input slot
    /// once per frame (vanilla's `slotChanged`) and edited per keystroke
    /// (`KeyOutcome::AnvilRename` in `app/lifecycle.rs`), which is also what
    /// produces `ClientAction::RenameItem`. This field is still just the
    /// **value** to draw — this struct owns no widget state itself, the same
    /// "per-frame input record" contract every other field here keeps. See
    /// [`with_anvil_name`](Self::with_anvil_name).
    pub anvil_name: Option<&'a str>,
    /// The beacon screen's currently pending primary power selection — see
    /// `crate::container::beacon::BeaconSelection`. `None` (the default)
    /// draws every power button unselected, which is what keeps every
    /// existing caller (headless builds, the pixel gates,
    /// `tests/container_screen.rs`) unchanged. See
    /// [`with_beacon_selection`](Self::with_beacon_selection).
    pub beacon_primary: Option<&'a lodestone_model::ResourceKey>,
    /// See [`Self::beacon_primary`].
    pub beacon_secondary: Option<&'a lodestone_model::ResourceKey>,
    /// The bundle slot currently tracking a scroll-driven selection highlight
    /// (`crate::container::bundle::BundleSelection`), already filtered to the
    /// menu this frame belongs to — the caller (`redraw`) is responsible for
    /// checking `window_id` before passing this in, since a `ContainerFrame`
    /// has no window id of its own to compare against. `None` (the default)
    /// draws the bundle tooltip's grid with nothing singled out, matching
    /// `BundleContents::NO_SELECTED_ITEM_INDEX`. See
    /// [`with_bundle_selection`](Self::with_bundle_selection) and
    /// `super::tooltip`'s bundle-image drawing.
    pub bundle_selection: Option<crate::container::bundle::BundleSelection>,
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
            avatar_pose: lodestone_render::AnimInput::REST,
            avatar_uuid: None,
            has_infinite_materials: false,
            xp_level: 0,
            tooltips: None,
            book_open: false,
            hover_blocked: false,
            trades: None,
            selected_trade: 0,
            trades_label: DEFAULT_TRADES_LABEL,
            anvil_name: None,
            beacon_primary: None,
            beacon_secondary: None,
            bundle_selection: None,
            effects: &[],
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
            avatar_pose: lodestone_render::AnimInput::REST,
            avatar_uuid: None,
            has_infinite_materials: false,
            xp_level: 0,
            tooltips: None,
            book_open: false,
            hover_blocked: false,
            trades: None,
            selected_trade: 0,
            trades_label: DEFAULT_TRADES_LABEL,
            anvil_name: None,
            beacon_primary: None,
            beacon_secondary: None,
            bundle_selection: None,
            effects: &[],
        }
    }

    /// Attach the active-effect column — see [`effects`](Self::effects).
    #[must_use]
    pub fn with_effects(mut self, effects: &'a [crate::effects::InventoryEffectRow]) -> Self {
        self.effects = effects;
        self
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

    /// Draw the hovered slot's **tooltip**, with vanilla's `advancedItemTooltips`
    /// (F3+H) either on or off — see [`Self::tooltips`].
    ///
    /// Needs [`with_cursor`](Self::with_cursor) as well: the hovered slot is
    /// resolved from the cursor, so a frame with tooltips enabled and no cursor
    /// draws none.
    #[must_use]
    pub fn with_tooltips(mut self, advanced: bool) -> Self {
        self.tooltips = Some(advanced);
        self
    }

    /// Shift the container panel right for an open recipe book — see
    /// [`Self::book_open`].
    #[must_use]
    pub fn with_book_open(mut self, open: bool) -> Self {
        self.book_open = open;
        self
    }

    /// Declare that an overlay above this screen owns the pointer this frame, so
    /// no hovered slot resolves under it — see [`Self::hover_blocked`].
    ///
    /// Does **not** affect the carried stack, which keeps tracking
    /// [`cursor`](Self::cursor) over the overlay.
    #[must_use]
    pub fn with_hover_blocked(mut self, blocked: bool) -> Self {
        self.hover_blocked = blocked;
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

    /// Attach the **live pose** for the inventory avatar, so a player who opens
    /// their inventory mid-swing sees the tail of that swing rather than a
    /// standing rig — vanilla poses the live render state
    /// (`InventoryScreen.extractEntityInInventoryFollowsMouse` is handed the real
    /// player entity). `AnimInput::REST` (the default) is what every caller
    /// without a `Sim` keeps.
    ///
    /// The head angles are **not** taken from here: `gui_entity_anim` overwrites
    /// them from the cursor, which is the whole point of it taking a base.
    #[must_use]
    pub fn with_avatar_pose(mut self, pose: lodestone_render::AnimInput) -> Self {
        self.avatar_pose = pose;
        self
    }

    /// Attach the local player's own uuid, so the inventory avatar's
    /// *default* skin (absent a local override or a fetched one) resolves
    /// from the same `default_skin_for_uuid` call the world-side default
    /// uses for every other player — see [`Self::avatar_uuid`]. `None` (the
    /// default) keeps every existing caller unchanged.
    #[must_use]
    pub fn with_avatar_uuid(mut self, uuid: Option<uuid::Uuid>) -> Self {
        self.avatar_uuid = uuid;
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
    /// preview** (part 2): in every painted cell, a 50%-white wash and
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

    /// Attach the open merchant's trade list and which row is selected — see
    /// [`Self::trades`]/[`Self::selected_trade`]. `None` (the default from
    /// [`new`](Self::new)) draws no trade rows.
    #[must_use]
    pub fn with_trades(mut self, trades: Option<&'a TradeOffers>, selected: usize) -> Self {
        self.trades = trades;
        self.selected_trade = selected;
        self
    }

    /// Override the merchant's "Trades" label with a translated one — see
    /// [`Self::trades_label`].
    #[must_use]
    pub fn with_trades_label(mut self, label: &'a str) -> Self {
        self.trades_label = label;
        self
    }

    /// Attach the anvil rename box's current value — see
    /// [`Self::anvil_name`]. `None` (the default) draws no text in the box.
    #[must_use]
    pub fn with_anvil_name(mut self, name: Option<&'a str>) -> Self {
        self.anvil_name = name;
        self
    }

    /// Attach the beacon screen's pending primary/secondary power selection
    /// — see [`Self::beacon_primary`]/[`Self::beacon_secondary`]. `None`
    /// (the default) draws every power button unselected.
    #[must_use]
    pub fn with_beacon_selection(
        mut self,
        primary: Option<&'a lodestone_model::ResourceKey>,
        secondary: Option<&'a lodestone_model::ResourceKey>,
    ) -> Self {
        self.beacon_primary = primary;
        self.beacon_secondary = secondary;
        self
    }

    /// Attach the bundle scroll-selection highlight — see
    /// [`Self::bundle_selection`]. The caller must already have filtered this
    /// to the currently open menu's own window id; this struct carries no
    /// window id to check it against.
    #[must_use]
    pub fn with_bundle_selection(
        mut self,
        selection: Option<crate::container::bundle::BundleSelection>,
    ) -> Self {
        self.bundle_selection = selection;
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
/// through to itself and **the raw key is what the panel draws**.
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

/// `en_us.json`'s value for `container.inventory` — the fallback
/// [`ContainerFrame::inventory_label`] carries when no caller supplies a
/// translated one.
const DEFAULT_INVENTORY_LABEL: &str = "Inventory";

/// `en_us.json`'s value for `merchant.trades` — the fallback
/// [`ContainerFrame::trades_label`] carries when no caller supplies a
/// translated one.
const DEFAULT_TRADES_LABEL: &str = "Trades";

/// Vanilla's `merchant.level.1`..`merchant.level.5` — `VillagerData`'s level
/// names, `en_us.json`. Index `0` is level `1` ("Novice").
const MERCHANT_LEVEL_WORDS: [&str; 5] = ["Novice", "Apprentice", "Journeyman", "Expert", "Master"];

/// Vanilla's `merchant.title` — the level-badge combined title
/// (`MerchantScreen.extractLabels`, `MerchantScreen.java`):
///
/// ```java
/// if (traderLevel > 0 && traderLevel <= 5 && this.menu.showProgressBar()) {
///    Component titleAndLevel = Component.translatable("merchant.title", this.title,
///       Component.translatable("merchant.level." + traderLevel));
///    ...
/// } else {
///    ... this.title ...
/// }
/// ```
///
/// `merchant.title` is `"%s - %s"` (`en_us.json`) — a genuinely nested
/// translation, the villager's own name as the first argument and a *second*
/// translated component (the level word) as the second, which is why this
/// goes through [`lodestone_game::text::resolve_to_string`] rather than a
/// local `format!`: a renamed villager's name can itself carry styling or a
/// further translate node (a custom-named villager from a command block, for
/// instance), and only the resolver preserves that.
///
/// `trader_level` outside `1..=5`, or `show_progress` false, draws the bare
/// name — vanilla's own `else` branch, not a missing case here.
#[must_use]
pub fn merchant_title(
    base_title: &lodestone_model::Text,
    trader_level: i32,
    show_progress: bool,
    translate: &dyn Fn(&str) -> Option<String>,
) -> String {
    if show_progress
        && let Ok(index) = usize::try_from(trader_level)
        && let Some(word) = index.checked_sub(1).and_then(|i| MERCHANT_LEVEL_WORDS.get(i))
    {
        // `fallback` (not a `with` argument): `merchant.level.N`'s pattern
        // takes no placeholder, it *is* the word. Carrying the known-good
        // English word here means a jar-less run or a stub table still shows
        // "Novice", not the raw key `merchant.level.1` — that fix's defect
        // class, avoidable here because the word is a fixed five-entry table
        // rather than server-authored prose.
        let level = lodestone_model::Text {
            content: lodestone_model::TextContent::Translate {
                key: format!("merchant.level.{trader_level}"),
                with: vec![],
                fallback: Some((*word).to_owned()),
            },
            ..lodestone_model::Text::default()
        };
        let composed =
            lodestone_model::Text::translate("merchant.title", vec![base_title.clone(), level]);
        menu_title(&composed, translate)
    } else {
        menu_title(base_title, translate)
    }
}

/// Vanilla's `merchant.trades` — the merchant screen's second label, "Trades"
/// (`MerchantScreen.java`), resolved the same way
/// [`player_inventory_label`] resolves `container.inventory`.
#[must_use]
pub fn merchant_trades_label(translate: &dyn Fn(&str) -> Option<String>) -> String {
    menu_title(
        &lodestone_model::Text::translate("merchant.trades", vec![]),
        translate,
    )
}

/// The player inventory screen's own title: **"Crafting"**, not "Inventory".
///
/// `InventoryScreen.java` passes `Component.translatable("container.crafting")`
/// to `super`, naming the 2×2 grid rather than the screen. This client used to
/// hardcode the string `"Inventory"` here (`app.rs`), which is wrong twice over:
/// wrong word, and — because it went in as the *title* — drawn at the title
/// anchor, which for this one screen is `x = 97` (`InventoryScreen.java`), not
/// `x = 8`.
///
/// Resolved through the language table for the same reason [`menu_title`] is: a
/// raw `container.crafting` on screen is that fix's defect class.
#[must_use]
pub fn player_inventory_title(translate: &dyn Fn(&str) -> Option<String>) -> String {
    menu_title(
        &lodestone_model::Text::translate("container.crafting", vec![]),
        translate,
    )
}

/// Vanilla's `playerInventoryTitle` — `container.inventory`, "Inventory".
///
/// A *client-side* constant in vanilla too (`Inventory.java`'s `DEFAULT_NAME`),
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
/// | generic container | `8` | yes | `AbstractContainerScreen.java` |
/// | crafting table | `29` | yes | `CraftingScreen.java` |
/// | player inventory | `97` | **no** | `InventoryScreen.java` |
///
/// The player inventory screen is the only one that omits the second label, and
/// it does so by *overriding `extractLabels`* to drop the second `graphics.text`
/// call entirely (`InventoryScreen.java`) — so the label is not wrong in
/// general, only there. Deleting it globally would trade one bug for another.
///
/// `inventory` is `[8, layout.height - 94]`: `inventoryLabelX = 8` and
/// `inventoryLabelY = imageHeight - 94` (`AbstractContainerScreen.java`,
/// restated by `ContainerScreen.java` for the row-count-dependent chest).
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
    // The merchant's `inventoryLabelX` is `107`, not `8`
    // (`MerchantScreen.java`'s constructor sets `this.inventoryLabelX =
    // 107`), the one screen in this whole family whose player-inventory
    // section is not left-aligned with the panel — see
    // `SpecialLayout::Merchant`'s doc comment. `title_x`/`title_y` here are
    // placeholders past what [`menu_type_title_anchor`]'s own `merchant`
    // branch always overrides (checked unconditionally at every real call
    // site, `container/geometry.rs`'s `build_inner`), so getting them wrong
    // here would never actually draw wrong — only a caller that skips that
    // second call would see it, which is why they still match the plain
    // generic default rather than being left at a nonsense value.
    if menu.special_layout() == Some(lodestone_game::menu::SpecialLayout::Merchant) {
        return LabelLayout {
            title_x: 8.0,
            title_y: 6.0,
            inventory: Some([107.0, layout.height - 94.0]),
        };
    }
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
/// | `furnace` / `blast_furnace` / `smoker` | `AbstractFurnaceScreen` subclasses | centred | `6` | `AbstractFurnaceScreen.java` |
/// | `brewing_stand` | `BrewingStandScreen` | centred | `6` | `BrewingStandScreen.java` |
/// | `generic_3x3` | `DispenserScreen` (dispenser **and** dropper) | centred | `6` | `DispenserScreen.java` |
/// | `crafter_3x3` | `CrafterScreen` | centred | `6` | `CrafterScreen.java` |
/// | `anvil` | `AnvilScreen` | `60` | `6` | `AnvilScreen.java` |
/// | `loom` | `LoomScreen` | `8` | `4` | `LoomScreen.java` (`titleLabelY -= 2`) |
/// | `stonecutter` | `StonecutterScreen` | `8` | `5` | `StonecutterScreen.java` (`titleLabelY--`) |
/// | `cartography_table` | `CartographyTableScreen` | `8` | `4` | `CartographyTableScreen.java` (`titleLabelY -= 2`) |
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
/// **`beacon` is excluded on purpose.** It draws at a different `imageWidth`
/// (`230`) with its own background art and has no case in [`background_kind`]
/// or [`slot_layout`], so an anchor alone would place correct text over a
/// still-wrong-shaped panel. It belongs with its own layout work.
///
/// **`merchant` has its own branch below, not a table row.** Two things set it
/// apart from the "centred" family: its centring formula has a `49` offset
/// vanilla's own default centring does not (`MerchantScreen.java`:
/// `49 + this.imageWidth / 2 - this.font.width(this.title) / 2`, vs. the
/// plain `(imageWidth - width) / 2` the furnace family etc. use), and its
/// *title text itself* is composed from the trader's level
/// ([`merchant_title`]) before it ever reaches this function — this function
/// only ever repositions [`ContainerFrame::title`], never rewrites it.
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
    if key.path() == "merchant" {
        // `MerchantScreen.java` — the same `49 +` offset in both the
        // level-badge and bare-name branches, which is why this is unaffected
        // by which form `title` (already composed by [`merchant_title`])
        // actually is.
        let text_width = font.map_or(0.0, |f| f.width(title, 1.0));
        return Some([(49.0 + layout.width / 2.0 - text_width / 2.0).floor(), 6.0]);
    }
    match key.path() {
        "anvil" => Some([60.0, 6.0]),
        "loom" | "cartography_table" => Some([8.0, 4.0]),
        "stonecutter" => Some([8.0, 5.0]),
        _ => None,
    }
}
