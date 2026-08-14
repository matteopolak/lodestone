//! Container and inventory screen rendering.
//!
//! Slot state is folded by `lodestone-client`/`lodestone-game`; this module only
//! projects a [`Menu`](lodestone_game::menu::Menu) into rectangles, coloured
//! quads and **item icons**. The generic-container hotbar starts at `n + 27`,
//! not absolute slot 36.
//!
//! # Layout
//!
//! [`slot_layout`] dispatches on [`MenuKind`] and then, additively, on
//! [`Menu::craft_layout`]: a menu that reports a crafting grid gets the vanilla
//! crafting-table arrangement (grid + result to its right, player inventory
//! below) rather than the flat 9-wide run a plain container gets. That branch is
//! deliberately *not* a new `MenuKind` — a crafting table's quick-move regions
//! and content size are a generic container's, only its slot kinds and its
//! screen differ, and `MenuKind` is matched exhaustively across this crate.
//!
//! Every `SlotRect` carries the real `menu_index`, so there is no constant
//! offset anywhere: window 0 is `0` result / `1..=4` craft / `5..=8` armour /
//! `9..=35` main / `36..=44` hotbar / `45` offhand, while a `Generic { n }` has
//! neither armour nor offhand and its hotbar is at `n + 27`.
//!
//! # Icons
//!
//! Slot contents draw through [`crate::hud::item_icon`] — the same flat-sprite
//! and 3-D block-item pass the hotbar uses, with the same atlases, tint palette
//! and animation slots. Without [`ContainerRenderer::attach_items`] the screen
//! falls back to the hash-derived colour swatch and letter it drew before there
//! was an atlas to draw from, so a jar-less run still shows *something* in an
//! occupied slot.

mod background;
/// The shared GUI vertex builder — colour, item-sprite, glint, block-model and
/// background streams. `pub(crate)` because it now has three consumers: the
/// container screen, the creative screen (#158) and the Advancements screen
/// (#167), all of which draw through `ContainerRenderer`'s passes.
pub(crate) mod builder;
mod creative;
/// Vanilla's creative-inventory tab contents (issue #158) — a hand-transcription
/// of the decompiled `CreativeModeTabs.java`, cross-checked against
/// `lodestone-data`'s real item registry. Read by [`creative`], the screen that
/// draws it.
mod creative_items;
mod frame;
mod geometry;
mod input;
mod layout;
/// The merchant/trading screen's own trade-list layout, prices and click
/// hit-test (issue #245's UI half) — see `docs/merchant-screen.md`.
pub mod merchant;
/// The inventory avatar (the player standing in the panel, head tracking the
/// cursor) — see `docs/inventory-player-preview.md`. The pose arithmetic lives in
/// `lodestone_render::gui_entity`; this is the GPU half.
mod player_preview;
mod recipe_book;
mod renderer;
mod tooltip;

pub use background::ContainerBackground;
pub use creative::{
    CREATIVE_COLS, CREATIVE_DEFAULT_TAB, CREATIVE_PAGE, CREATIVE_PANEL_H, CREATIVE_PANEL_W,
    CREATIVE_ROWS, CREATIVE_SEARCH_MAX_LEN, CreativeBackground, CreativeEffect, CreativeHit,
    CreativeLayout, CreativeState, CreativeTabKind, CreativeView, can_scroll, creative_click,
    creative_geometry, creative_hit_test, creative_inventory_click, creative_item_list_click,
    creative_items_for, creative_layout, creative_page_items, creative_tab_count,
    creative_tab_kind, creative_tab_title_key, row_count, row_for_scroll,
};
pub use frame::{
    ContainerFrame, LabelLayout, label_layout, menu_title, menu_type_title_anchor,
    merchant_title, merchant_trades_label, player_inventory_label, player_inventory_title,
};
pub use geometry::ContainerGeometry;
pub use input::{MenuButton, MenuContext, MenuInput, MenuKey};
pub use layout::{
    MenuHit, RECIPE_BOOK_MIN_WIDTH, RECIPE_BOOK_X_OFFSET, Rect, SlotLayout, SlotRect, hit_test,
    hit_test_with_book, hit_test_with_scale, panel_origin, panel_origin_with_scale,
    recipe_book_panel_shift, recipe_book_width_too_narrow, slot_layout,
};
pub use player_preview::PlayerAvatar;
pub use recipe_book::{
    RECIPE_BUTTON_SIZE, RECIPE_FILTER_BUTTON, RECIPE_GRID_COLS, RECIPE_GRID_ORIGIN,
    RECIPE_GRID_ROWS, RECIPE_GRID_STEP, RECIPE_ITEMS_PER_PAGE, RECIPE_MAGNIFIER, RECIPE_PAGE_BACK,
    RECIPE_PAGE_FORWARD, RECIPE_PANEL_GAP, RECIPE_PANEL_H, RECIPE_PANEL_W, RECIPE_SEARCH_BOX,
    RECIPE_TAB_H, RECIPE_TAB_SPACING, RECIPE_TAB_W, RECIPE_TAB_X, RECIPE_TAB_Y0,
    RECIPE_PANEL_SRC, RECIPE_SPRITE_BUTTON, RECIPE_SPRITE_FILTER, RECIPE_SPRITE_FILTER_ENABLED,
    RECIPE_SPRITE_FILTER_FURNACE, RECIPE_SPRITE_PAGE_BACK, RECIPE_SPRITE_PAGE_FORWARD, RECIPE_SPRITE_PANEL, RECIPE_SPRITE_SLOT,
    RECIPE_SPRITE_TAB, RECIPE_SPRITE_TAB_SELECTED, RECIPE_TOGGLE_LOCAL,
    RECIPE_TOGGLE_LOCAL_FURNACE, RECIPE_TOGGLE_LOCAL_INVENTORY, RecipeBookPanelContents,
    RecipeBookPanelGeometry, RecipeBookPanelHit, RecipeBookPanelLayout, RecipeBookSprite,
    RecipeTabIcons, RecipeTooltipContext, recipe_book_panel_contents,
    recipe_book_panel_contents_filtered,
    recipe_book_panel_geometry, recipe_book_panel_geometry_with_icons, recipe_book_panel_hit_test,
    recipe_book_panel_hit_test_with_scale, recipe_book_panel_layout,
    recipe_book_panel_layout_with_scale, recipe_tab_icons, recipe_toggle_local,
};
pub use renderer::ContainerRenderer;

const FLOATS_PER_VERTEX: usize = 6;
/// Floats per vertex on the background/GUI-sprite stream: `[x, y, u, v, r, g, b, a]`.
/// Distinct from [`FLOATS_PER_VERTEX`], which is the untextured colour stream.
const BG_FLOATS_PER_VERTEX: usize = 8;
const SLOT: f32 = 18.0;
const CELL: f32 = 16.0;

/// The hover highlight's two sprite ids, from
/// `AbstractContainerScreen.java:29-30`. Blitted at
/// `(slot.x - 4, slot.y - 4, 24, 24)` (`:155`, `:161`) — one *behind* the slot's
/// item and one *in front of* it, which is the whole reason there are two.
pub const SLOT_HIGHLIGHT_BACK: &str = "container/slot_highlight_back";
/// See [`SLOT_HIGHLIGHT_BACK`].
pub const SLOT_HIGHLIGHT_FRONT: &str = "container/slot_highlight_front";

/// Vanilla's `24` for the highlight blit — the sprite's own native size, so this
/// is a 1:1 blit and the `nine_slice` scaling in its `.png.mcmeta` (border 4)
/// never actually stretches anything. Worth stating: implementing the nine-slice
/// path for this sprite is work with no observable effect.
const HIGHLIGHT: f32 = 24.0;
/// Vanilla's `-4` inset (`:155`), which is `(HIGHLIGHT - SLOT) / 2` off the
/// 18×18 well but is written as the literal vanilla uses, against the 16×16
/// cell origin.
const HIGHLIGHT_INSET: f32 = 4.0;

/// The furnace family's two progress sprites (issue #28), one pair per
/// texture — `AbstractFurnaceScreen.java:17-18` takes them as constructor
/// parameters, and `FurnaceScreen`/`BlastFurnaceScreen`/`SmokerScreen` each
/// supply their own rather than sharing one id.
const FURNACE_LIT_PROGRESS: &str = "container/furnace/lit_progress";
const FURNACE_BURN_PROGRESS: &str = "container/furnace/burn_progress";
const BLAST_FURNACE_LIT_PROGRESS: &str = "container/blast_furnace/lit_progress";
const BLAST_FURNACE_BURN_PROGRESS: &str = "container/blast_furnace/burn_progress";
const SMOKER_LIT_PROGRESS: &str = "container/smoker/lit_progress";
const SMOKER_BURN_PROGRESS: &str = "container/smoker/burn_progress";

/// The brewing stand's three progress sprites (issue #28),
/// `BrewingStandScreen.java:12-14`.
const BREWING_FUEL_LENGTH: &str = "container/brewing_stand/fuel_length";
const BREWING_BREW_PROGRESS: &str = "container/brewing_stand/brew_progress";
const BREWING_BUBBLES: &str = "container/brewing_stand/bubbles";

/// The merchant screen's own sprites (issue #245's UI half),
/// `MerchantScreen.java:21-29`. The experience-bar trio and the scroller pair
/// are not drawn yet — see `container::merchant`'s module doc for what is
/// modelled and what is a named gap.
const MERCHANT_OUT_OF_STOCK: &str = "container/villager/out_of_stock";
const MERCHANT_TRADE_ARROW: &str = "container/villager/trade_arrow";
const MERCHANT_TRADE_ARROW_OUT_OF_STOCK: &str = "container/villager/trade_arrow_out_of_stock";
const MERCHANT_DISCOUNT_STRIKETHROUGH: &str = "container/villager/discount_strikethrough";

/// Every GUI sprite [`ContainerBackground`] stitches alongside the three panel
/// sheets: the hover-highlight pair and the five empty-slot placeholders the
/// player inventory declares.
///
/// The placeholders are addressed by the id `Slot::no_item_icon` already
/// carries, so this list and the draw agree by construction rather than by two
/// transcriptions of the same table.
const GUI_SPRITES: &[&str] = &[
    SLOT_HIGHLIGHT_BACK,
    SLOT_HIGHLIGHT_FRONT,
    lodestone_game::menu::EMPTY_ARMOR_SLOT_HELMET,
    lodestone_game::menu::EMPTY_ARMOR_SLOT_CHESTPLATE,
    lodestone_game::menu::EMPTY_ARMOR_SLOT_LEGGINGS,
    lodestone_game::menu::EMPTY_ARMOR_SLOT_BOOTS,
    lodestone_game::menu::EMPTY_ARMOR_SLOT_SHIELD,
    // Furnace family + brewing stand progress widgets (issue #28) — real
    // container_set_data-driven bars, not slot placeholders, but they ride
    // the same atlas for the same reason the highlight pair does.
    FURNACE_LIT_PROGRESS,
    FURNACE_BURN_PROGRESS,
    BLAST_FURNACE_LIT_PROGRESS,
    BLAST_FURNACE_BURN_PROGRESS,
    SMOKER_LIT_PROGRESS,
    SMOKER_BURN_PROGRESS,
    BREWING_FUEL_LENGTH,
    BREWING_BREW_PROGRESS,
    BREWING_BUBBLES,
    MERCHANT_OUT_OF_STOCK,
    MERCHANT_TRADE_ARROW,
    MERCHANT_TRADE_ARROW_OUT_OF_STOCK,
    MERCHANT_DISCOUNT_STRIKETHROUGH,
];

/// [`GUI_SPRITES`] plus the creative screen's 28 tab buttons and 2 scroller
/// sprites (issue #158) — all `gui/sprites/container/creative_inventory/**`, so
/// they belong in this atlas rather than needing one of their own. Kept as a
/// separate concatenation only because `GUI_SPRITES` is indexed by nothing and
/// this keeps the creative ids next to the module that names them.
fn all_gui_sprites() -> impl Iterator<Item = &'static str> {
    GUI_SPRITES
        .iter()
        .copied()
        .chain(creative::CREATIVE_TAB_SPRITES)
        .chain(creative::CREATIVE_SCROLLER_SPRITES)
        // The Advancements screen's 13 sprites (issue #167) — `advancements/**`,
        // also `gui/sprites/**`, so they ride this atlas too.
        .chain(crate::menu::advancements::ADVANCEMENT_SPRITES)
}

const CONTAINER_WGSL: &str = include_str!("shaders/container.wgsl");

/// A plain textured quad shader for [`ContainerBackground`]'s atlas — the same
/// shape as `menu/render.rs`'s `MENU_SPRITE_WGSL`, restated here rather than
/// shared because that one is `menu`'s own module-private constant.
const CONTAINER_BG_WGSL: &str = include_str!("shaders/container_bg.wgsl");

#[cfg(test)]
mod tests;
