//! The creative-inventory screen (issue #158): tab strip, item grid, scrollbar,
//! search.
//!
//! Vanilla's `CreativeModeInventoryScreen`, whose contents come from
//! [`super::creative_items::CREATIVE_TABS`] — the hand-transcribed
//! `CreativeModeTabs.java` table that landed ahead of this screen.
//!
//! # Why this is not a `MenuKind`
//!
//! Vanilla's creative screen is backed by `ItemPickerMenu`, a **client-only**
//! `AbstractContainerMenu` with no server container behind it, and
//! `lodestone-game`'s own `menu.rs` says plainly that `MenuKind` must not grow.
//! So this module owns its own layout, hit test and geometry rather than
//! extending [`super::layout::slot_layout`]. It reuses everything below that
//! seam: [`Builder`] for the four vertex streams, [`ContainerBackground`] for
//! the three loose `creative_inventory/*.png` sheets, and
//! [`ContainerGeometry`] as the geometry *type*, so
//! [`ContainerRenderer::render_geometry_scaled`](super::ContainerRenderer) draws
//! it through the exact passes the ordinary container screen already uses — no
//! new pipeline, no new bind group, no new shader.
//!
//! # What is and is not vanilla-exact
//!
//! Every constant here is transcribed from
//! `.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/inventory/CreativeModeInventoryScreen.java`.
//! Three deliberate departures, each because the machinery it would need does
//! not exist on this side yet:
//!
//! - **Clicking a grid cell gives the item into the selected hotbar slot**
//!   rather than picking it onto the cursor. The cursor stack lives on a real
//!   [`Menu`], and the creative grid has none; `ClientAction::SetCreativeModeSlot`
//!   is the one wire verb available, so the screen uses it directly. See
//!   `docs/creative-inventory-screen.md`.
//! - **The `hotbar` tab is empty.** Vanilla fills it from saved hotbars on disk
//!   (`HotbarManager`), which this client has no store for. It draws its
//!   background, tab strip and the player's live hotbar row like every other
//!   tab, and its grid is honestly blank rather than showing something invented.
//! - **No item tooltips carry the tab-membership lines** vanilla's
//!   `getTooltipFromContainerItem` appends; the hovered-slot tooltip itself is the
//!   ordinary container one.
//!
//! Note vanilla's `checkTabClicked` and `extractTabButton` disagree by 4 px
//! vertically (`getTabY` gives `-32`/`+imageHeight`, the blit uses
//! `-28`/`+imageHeight - 4`). That is vanilla's own behaviour, not a
//! transcription slip: the tab art tucks 4 px under the panel edge and that
//! overlapped strip is deliberately not clickable. [`creative_layout`] carries
//! both rects for that reason.

use lodestone_assets::ItemAtlas;
use lodestone_game::item::ItemStack;
use lodestone_game::menu::Menu;
use lodestone_model::Identifier;
use lodestone_render::BlockModels;

use crate::hud::item_icon::IconAssets;

use super::background::ContainerBackground;
use super::builder::Builder;
use super::creative_items::{CREATIVE_TABS, CreativeTab};
use super::geometry::ContainerGeometry;
use super::layout::Rect;
use super::{BG_FLOATS_PER_VERTEX, CELL, FLOATS_PER_VERTEX};

/// `super(..., 195, 136)` (`CreativeModeInventoryScreen.java:118`) — **not** the
/// `176 x 166` every other container screen defaults to.
pub const CREATIVE_PANEL_W: f32 = 195.0;
/// See [`CREATIVE_PANEL_W`].
pub const CREATIVE_PANEL_H: f32 = 136.0;

/// `NUM_ROWS` / `NUM_COLS` (`:96-97`).
pub const CREATIVE_ROWS: usize = 5;
/// See [`CREATIVE_ROWS`].
pub const CREATIVE_COLS: usize = 9;
/// The 45 cells one page of the grid shows.
pub const CREATIVE_PAGE: usize = CREATIVE_ROWS * CREATIVE_COLS;

/// `new CustomCreativeSlot(CONTAINER, y * 9 + x, 9 + x * 18, 18 + y * 18)`
/// (`:888`).
const GRID_X0: f32 = 9.0;
/// See [`GRID_X0`].
const GRID_Y0: f32 = 18.0;
/// The slot pitch every container screen in the game shares.
const SLOT: f32 = 18.0;

/// `addInventoryHotbarSlots(inventory, 9, 112)` (`:892`).
const HOTBAR_Y: f32 = 112.0;

/// `insideScrollbar` (`:650-658`): the track is `(175, 18)` to `(189, 130)`.
const SCROLL_X: f32 = 175.0;
/// See [`SCROLL_X`].
const SCROLL_Y: f32 = 18.0;
/// See [`SCROLL_X`].
const SCROLL_W: f32 = 14.0;
/// See [`SCROLL_X`].
const SCROLL_H: f32 = 112.0;
/// `blitSprite(..., xscr, yscr + (int)((yscr2 - yscr - 17) * scrollOffs), 12, 15)`
/// (`:753`). The thumb is 15 tall but travels `SCROLL_H - 17`, so it stops 2 px
/// short of the track's bottom edge — vanilla's own arithmetic, kept literal.
const THUMB_W: f32 = 12.0;
/// See [`THUMB_W`].
const THUMB_H: f32 = 15.0;
/// See [`THUMB_W`].
const THUMB_TRAVEL: f32 = SCROLL_H - 17.0;

/// `new EditBox(font, leftPos + 82, topPos + 6, 80, 9, ...)` (`:327`).
const SEARCH_X: f32 = 82.0;
/// See [`SEARCH_X`].
const SEARCH_Y: f32 = 6.0;
/// See [`SEARCH_X`].
const SEARCH_W: f32 = 80.0;
/// See [`SEARCH_X`].
const SEARCH_H: f32 = 9.0;
/// `searchBox.setMaxLength(50)` (`:328`).
pub const CREATIVE_SEARCH_MAX_LEN: usize = 50;

/// `26 x 32` (`:796`, `:827`).
const TAB_W: f32 = 26.0;
/// See [`TAB_W`].
const TAB_H: f32 = 32.0;
/// `int spacing = 27` (`:772`).
const TAB_PITCH: f32 = 27.0;
/// The blit's own y offset: `topPos - (isTop ? 28 : -(imageHeight - 4))`
/// (`:816`).
const TAB_DRAW_TOP_DY: f32 = -28.0;
/// See [`TAB_DRAW_TOP_DY`].
const TAB_DRAW_BOTTOM_DY: f32 = CREATIVE_PANEL_H - 4.0;
/// `getTabY` (`:782-790`) — the *hit* rect, 4 px off the blit. See the module
/// doc.
const TAB_HIT_TOP_DY: f32 = -32.0;
/// See [`TAB_HIT_TOP_DY`].
const TAB_HIT_BOTTOM_DY: f32 = CREATIVE_PANEL_H;
/// `iconX = x + 13 - 8`, `iconY = y + 16 - 8 + (isTop ? 1 : -1)` (`:828-829`).
const TAB_ICON_DX: f32 = 5.0;
/// See [`TAB_ICON_DX`].
const TAB_ICON_DY: f32 = 8.0;

/// `extractLabels`: `graphics.text(font, selectedTab.getDisplayName(), 8, 6,
/// -12566464, false)` (`:483`) — `0xFF404040`, dark grey, unshadowed, and only
/// when the tab's `showTitle()` is set.
const TITLE_X: f32 = 8.0;
/// See [`TITLE_X`].
const TITLE_Y: f32 = 6.0;
/// See [`TITLE_X`].
const TITLE_COLOUR: [f32; 4] = [0.25, 0.25, 0.25, 1.0];

/// `new Slot(CONTAINER, 0, 173, 112)` (`:599`) — the inventory tab's trash slot.
const DESTROY_X: f32 = 173.0;
/// See [`DESTROY_X`].
const DESTROY_Y: f32 = 112.0;

/// `container/creative_inventory/scroller` (`:58`).
const SPRITE_SCROLLER: &str = "container/creative_inventory/scroller";
/// `container/creative_inventory/scroller_disabled` (`:59`).
const SPRITE_SCROLLER_DISABLED: &str = "container/creative_inventory/scroller_disabled";

/// Every tab-button sprite id, indexed `[top][selected][column]` — the four
/// arrays at `:60-95`. Vanilla indexes with `Mth.clamp(pos, 0, sprites.length)`,
/// i.e. the seven columns map one-to-one.
pub(super) const CREATIVE_TAB_SPRITES: [&str; 28] = [
    "container/creative_inventory/tab_top_unselected_1",
    "container/creative_inventory/tab_top_unselected_2",
    "container/creative_inventory/tab_top_unselected_3",
    "container/creative_inventory/tab_top_unselected_4",
    "container/creative_inventory/tab_top_unselected_5",
    "container/creative_inventory/tab_top_unselected_6",
    "container/creative_inventory/tab_top_unselected_7",
    "container/creative_inventory/tab_top_selected_1",
    "container/creative_inventory/tab_top_selected_2",
    "container/creative_inventory/tab_top_selected_3",
    "container/creative_inventory/tab_top_selected_4",
    "container/creative_inventory/tab_top_selected_5",
    "container/creative_inventory/tab_top_selected_6",
    "container/creative_inventory/tab_top_selected_7",
    "container/creative_inventory/tab_bottom_unselected_1",
    "container/creative_inventory/tab_bottom_unselected_2",
    "container/creative_inventory/tab_bottom_unselected_3",
    "container/creative_inventory/tab_bottom_unselected_4",
    "container/creative_inventory/tab_bottom_unselected_5",
    "container/creative_inventory/tab_bottom_unselected_6",
    "container/creative_inventory/tab_bottom_unselected_7",
    "container/creative_inventory/tab_bottom_selected_1",
    "container/creative_inventory/tab_bottom_selected_2",
    "container/creative_inventory/tab_bottom_selected_3",
    "container/creative_inventory/tab_bottom_selected_4",
    "container/creative_inventory/tab_bottom_selected_5",
    "container/creative_inventory/tab_bottom_selected_6",
    "container/creative_inventory/tab_bottom_selected_7",
];

/// The two scroller sprites, for [`super::GUI_SPRITES`].
pub(super) const CREATIVE_SCROLLER_SPRITES: [&str; 2] =
    [SPRITE_SCROLLER, SPRITE_SCROLLER_DISABLED];

fn tab_sprite(top: bool, selected: bool, column: u8) -> &'static str {
    let base = usize::from(!top) * 14 + usize::from(selected) * 7;
    CREATIVE_TAB_SPRITES[base + (column as usize).min(6)]
}

/// Which of vanilla's four `CreativeModeTab.Type`s a tab is — the thing that
/// decides its background sheet, whether it scrolls, and whether the search box
/// is live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreativeTabKind {
    /// The eleven ordinary item tabs.
    Category,
    /// `Type.SEARCH` — a live filter over every other tab's items.
    Search,
    /// `Type.HOTBAR`. Vanilla's saved hotbars; empty here (module doc).
    Hotbar,
    /// `Type.INVENTORY` — the survival inventory, at its own slot positions.
    Inventory,
}

impl CreativeTabKind {
    /// `tab.canScroll()` — only `Type.INVENTORY` calls `noScrollBar()`
    /// (`CreativeModeTabs.java:1637`).
    #[must_use]
    pub fn scrolls(self) -> bool {
        self != Self::Inventory
    }

    /// `tab.showTitle()` — only the inventory tab calls `hideTitle()`
    /// (`CreativeModeTabs.java:1630`).
    #[must_use]
    pub fn shows_title(self) -> bool {
        self != Self::Inventory
    }

    /// The loose `textures/gui/container/creative_inventory/*.png` sheet this
    /// tab's background blits from — `tab.getBackgroundTexture()`.
    #[must_use]
    pub fn background(self) -> CreativeBackground {
        match self {
            Self::Search => CreativeBackground::ItemSearch,
            Self::Inventory => CreativeBackground::Inventory,
            Self::Category | Self::Hotbar => CreativeBackground::Items,
        }
    }
}

/// One of the three creative background sheets. Loose art under
/// `textures/gui/container/`, so it rides [`ContainerBackground`]'s atlas
/// alongside `container/inventory` and friends rather than the GUI sprite atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreativeBackground {
    /// `tab_items.png` — every category tab, and the hotbar tab.
    Items,
    /// `tab_item_search.png` — the search tab (it has the box well baked in).
    ItemSearch,
    /// `tab_inventory.png` — the inventory tab.
    Inventory,
}

/// The tab kind of `CREATIVE_TABS[index]`, by its registry id.
///
/// Keyed on the id rather than on a new field in
/// [`CreativeTab`](super::creative_items::CreativeTab) so the transcribed table
/// needs no edit: the four special ids are fixed in 26.2 and named literally in
/// `CreativeModeTabs.bootstrap`'s own `.type(...)` calls.
#[must_use]
pub fn creative_tab_kind(index: usize) -> CreativeTabKind {
    match CREATIVE_TABS.get(index).map(|t| t.id) {
        Some("minecraft:search") => CreativeTabKind::Search,
        Some("minecraft:hotbar") => CreativeTabKind::Hotbar,
        Some("minecraft:inventory") => CreativeTabKind::Inventory,
        _ => CreativeTabKind::Category,
    }
}

/// How many tabs the strip has — 14 in 26.2.
#[must_use]
pub fn creative_tab_count() -> usize {
    CREATIVE_TABS.len()
}

/// The `itemGroup.*` translation key for a tab's display name, for a caller that
/// holds the language table (this module does not).
#[must_use]
pub fn creative_tab_title_key(index: usize) -> Option<&'static str> {
    CREATIVE_TABS.get(index).map(|t| t.title_key)
}

/// `tab.isAlignedRight()`.
///
/// Derived from the column rather than stored: in 26.2 `alignedRight()` is
/// called on exactly the four tabs in columns 5 and 6 (`CreativeModeTabs.java`
/// `:1000`, `:1022`, `:1584`, `:1631`), both rows. Re-check this against a newer
/// `CreativeModeTabs.java` if the strip ever grows an eighth column.
fn aligned_right(tab: &CreativeTab) -> bool {
    tab.column >= 5
}

/// The default tab a freshly opened screen shows — vanilla's
/// `selectedTab = BUILDING_BLOCKS` (`CreativeModeTabs.getDefaultTab`).
pub const CREATIVE_DEFAULT_TAB: usize = 0;

/// Persisted creative-screen UI state.
#[derive(Debug, Clone)]
pub struct CreativeState {
    /// Index into [`CREATIVE_TABS`].
    pub tab: usize,
    /// `scrollOffs`, `0.0..=1.0`.
    pub scroll: f32,
    /// The search tab's filter text.
    pub search: String,
    /// Whether typing edits [`search`](Self::search). Vanilla focuses the box
    /// unconditionally while the search tab is up (`:609-612`), which is what
    /// this mirrors — it is set by [`Self::select_tab`], not only by a click.
    pub search_focused: bool,
    /// Whether the scrollbar thumb is being dragged.
    pub scrolling: bool,
}

impl Default for CreativeState {
    fn default() -> Self {
        Self {
            tab: CREATIVE_DEFAULT_TAB,
            scroll: 0.0,
            search: String::new(),
            search_focused: false,
            scrolling: false,
        }
    }
}

impl CreativeState {
    /// `selectTab` (`:558-625`): reset the scroll, and take or drop search
    /// focus with the tab.
    pub fn select_tab(&mut self, index: usize) {
        if self.tab != index {
            self.search.clear();
        }
        self.tab = index;
        self.scroll = 0.0;
        self.scrolling = false;
        self.search_focused = creative_tab_kind(index) == CreativeTabKind::Search;
    }

    /// `subtractInputFromScroll` (`:911-913`) — one wheel notch is one row, so
    /// the step depends on how many rows the current tab has.
    pub fn scroll_by(&mut self, notches: f32, item_count: usize) {
        let rows = row_count(item_count);
        if rows == 0 {
            self.scroll = 0.0;
            return;
        }
        self.scroll = (self.scroll - notches / rows as f32).clamp(0.0, 1.0);
    }

    /// `mouseDragged` (`:661-671`): the thumb centre follows the pointer over
    /// the track's usable travel.
    ///
    /// `y` is in **logical canvas pixels**, and `track_y` is the track's own
    /// absolute top — both the same space [`creative_layout`] produces.
    pub fn drag_scroll(&mut self, y: f32, track_y: f32) {
        self.scroll = ((y - track_y - THUMB_H * 0.5) / THUMB_TRAVEL).clamp(0.0, 1.0);
    }
}

/// `calculateRowCount` (`:900-902`) — the number of rows the grid can scroll
/// *past*, which is why a tab with 45 or fewer items reports zero.
#[must_use]
pub fn row_count(item_count: usize) -> usize {
    item_count.div_ceil(CREATIVE_COLS).saturating_sub(CREATIVE_ROWS)
}

/// `canScroll` (`:930-932`).
#[must_use]
pub fn can_scroll(item_count: usize) -> bool {
    item_count > CREATIVE_PAGE
}

/// `getRowIndexForScroll` (`:904-906`).
#[must_use]
pub fn row_for_scroll(scroll: f32, item_count: usize) -> usize {
    let rows = row_count(item_count);
    ((scroll * rows as f32 + 0.5) as i32).max(0) as usize
}

/// Every item id the tab at `index` shows, in vanilla's own registration order.
///
/// The search tab is the union of every other tab's items in tab order, deduped
/// (vanilla's `ItemStackLinkedSet` does the same), filtered by `search` as a
/// case-insensitive substring of the id's path. Vanilla matches on the item's
/// *display name*; the path is what this client can resolve without the language
/// table, and `search.contains('#')`-style tag queries are not modelled.
#[must_use]
pub fn creative_items_for(index: usize, search: &str) -> Vec<&'static str> {
    match creative_tab_kind(index) {
        CreativeTabKind::Search => {
            let needle = search.trim().to_ascii_lowercase();
            let mut seen = Vec::new();
            for (i, tab) in CREATIVE_TABS.iter().enumerate() {
                if creative_tab_kind(i) == CreativeTabKind::Search {
                    continue;
                }
                for id in tab.items {
                    if !needle.is_empty() && !id.to_ascii_lowercase().contains(&needle) {
                        continue;
                    }
                    if !seen.contains(id) {
                        seen.push(*id);
                    }
                }
            }
            seen
        }
        _ => CREATIVE_TABS
            .get(index)
            .map_or_else(Vec::new, |tab| tab.items.to_vec()),
    }
}

/// The 45 grid cells for `scroll` over `items` — `scrollTo` (`:916-928`), which
/// leaves a cell empty past the end of the list rather than shortening the grid.
#[must_use]
pub fn creative_page_items(items: &[&'static str], scroll: f32) -> Vec<Option<&'static str>> {
    let row = row_for_scroll(scroll, items.len());
    (0..CREATIVE_PAGE)
        .map(|i| {
            let index = i % CREATIVE_COLS + (i / CREATIVE_COLS + row) * CREATIVE_COLS;
            items.get(index).copied()
        })
        .collect()
}

/// Complete creative-screen geometry for one frame, in **absolute logical canvas
/// pixels** — the same space [`super::panel_origin`] and [`super::SlotRect`]
/// already use, so a caller composes it with nothing to re-derive.
#[derive(Debug, Clone)]
pub struct CreativeLayout {
    /// The `195 x 136` panel.
    pub panel: Rect,
    /// The 45 grid cells, row-major, at `CELL` size (the 16×16 icon well, not
    /// the 18 px pitch).
    pub grid: Vec<Rect>,
    /// The player's own hotbar row, slots `36..45`.
    pub hotbar: Vec<Rect>,
    /// The inventory tab's `(menu_index, rect)` pairs — armour, offhand, main
    /// and hotbar, at `selectTab`'s own positions (`:568-597`). Empty on every
    /// other tab.
    pub inventory: Vec<(usize, Rect)>,
    /// The inventory tab's trash slot.
    pub destroy: Option<Rect>,
    /// The 14 tab buttons' **blit** rects, in [`CREATIVE_TABS`] order.
    pub tabs: Vec<Rect>,
    /// The 14 tab buttons' **hit** rects. See the module doc for why these are
    /// not [`tabs`](Self::tabs).
    pub tab_hits: Vec<Rect>,
    /// The scrollbar track, on a tab that has one.
    pub scroll_track: Option<Rect>,
    /// The thumb, positioned for the current scroll.
    pub scroll_thumb: Option<Rect>,
    /// The search box, on the search tab only.
    pub search_box: Option<Rect>,
    /// Whether the current tab's list is long enough to scroll — drives the
    /// enabled/disabled thumb sprite.
    pub can_scroll: bool,
}

/// Builds [`CreativeLayout`] against an explicit `gui_scale` (`0` = auto) — it
/// **must** be the same scale the frame was drawn with, exactly as
/// [`super::hit_test_with_scale`] warns.
#[must_use]
pub fn creative_layout(
    state: &CreativeState,
    item_count: usize,
    gui_scale: u32,
    width: u32,
    height: u32,
) -> CreativeLayout {
    let (cw, ch) = crate::menu::render::logical_canvas(gui_scale, width, height);
    // The same centring `panel_origin_with_scale` performs, at this screen's own
    // panel size — restated rather than routed through that function because it
    // takes a `SlotLayout` this screen has none of.
    let x = ((cw - CREATIVE_PANEL_W) * 0.5).max(8.0);
    let y = ((ch - CREATIVE_PANEL_H) * 0.5).max(8.0);
    let kind = creative_tab_kind(state.tab);

    let grid = (0..CREATIVE_PAGE)
        .map(|i| Rect {
            x: x + GRID_X0 + (i % CREATIVE_COLS) as f32 * SLOT,
            y: y + GRID_Y0 + (i / CREATIVE_COLS) as f32 * SLOT,
            w: CELL,
            h: CELL,
        })
        .collect();
    let hotbar = (0..CREATIVE_COLS)
        .map(|i| Rect {
            x: x + GRID_X0 + i as f32 * SLOT,
            y: y + HOTBAR_Y,
            w: CELL,
            h: CELL,
        })
        .collect();

    let mut tabs = Vec::with_capacity(CREATIVE_TABS.len());
    let mut tab_hits = Vec::with_capacity(CREATIVE_TABS.len());
    for tab in CREATIVE_TABS {
        let local_x = if aligned_right(tab) {
            CREATIVE_PANEL_W - TAB_PITCH * (7.0 - f32::from(tab.column)) + 1.0
        } else {
            TAB_PITCH * f32::from(tab.column)
        };
        let draw_dy = if tab.top_row { TAB_DRAW_TOP_DY } else { TAB_DRAW_BOTTOM_DY };
        let hit_dy = if tab.top_row { TAB_HIT_TOP_DY } else { TAB_HIT_BOTTOM_DY };
        tabs.push(Rect { x: x + local_x, y: y + draw_dy, w: TAB_W, h: TAB_H });
        tab_hits.push(Rect { x: x + local_x, y: y + hit_dy, w: TAB_W, h: TAB_H });
    }

    let scroll_track = kind.scrolls().then(|| Rect {
        x: x + SCROLL_X,
        y: y + SCROLL_Y,
        w: SCROLL_W,
        h: SCROLL_H,
    });
    let scroll_thumb = scroll_track.map(|track| Rect {
        x: track.x,
        y: track.y + (THUMB_TRAVEL * state.scroll).floor(),
        w: THUMB_W,
        h: THUMB_H,
    });
    let search_box = (kind == CreativeTabKind::Search).then(|| Rect {
        x: x + SEARCH_X,
        y: y + SEARCH_Y,
        w: SEARCH_W,
        h: SEARCH_H,
    });

    let (inventory, destroy) = if kind == CreativeTabKind::Inventory {
        (inventory_tab_slots(x, y), Some(Rect { x: x + DESTROY_X, y: y + DESTROY_Y, w: CELL, h: CELL }))
    } else {
        (Vec::new(), None)
    };

    CreativeLayout {
        panel: Rect { x, y, w: CREATIVE_PANEL_W, h: CREATIVE_PANEL_H },
        grid,
        hotbar,
        inventory,
        destroy,
        tabs,
        tab_hits,
        scroll_track,
        scroll_thumb,
        search_box,
        can_scroll: kind.scrolls() && can_scroll(item_count),
    }
}

/// The inventory tab's own slot table — `selectTab`'s `SlotWrapper` loop
/// (`:568-597`), which re-places the *player inventory* menu's slots into this
/// narrower panel.
///
/// Slots `0..5` (the 2×2 crafting grid and its result) are moved to `-2000` and
/// therefore simply omitted here; the four armour slots go into two columns of
/// two, and the offhand lands beside them.
fn inventory_tab_slots(x: f32, y: f32) -> Vec<(usize, Rect)> {
    let mut out = Vec::with_capacity(41);
    let cell = |lx: f32, ly: f32| Rect { x: x + lx, y: y + ly, w: CELL, h: CELL };
    for i in 5..9 {
        let pos = i - 5;
        out.push((i, cell(54.0 + (pos / 2) as f32 * 54.0, 6.0 + (pos % 2) as f32 * 27.0)));
    }
    out.push((45, cell(35.0, 20.0)));
    for i in 9..45 {
        let pos = i - 9;
        let lx = 9.0 + (pos % 9) as f32 * SLOT;
        let ly = if i >= 36 { HOTBAR_Y } else { 54.0 + (pos / 9) as f32 * SLOT };
        out.push((i, cell(lx, ly)));
    }
    out
}

/// What a logical-canvas pixel is over, on the creative screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreativeHit {
    /// A tab button, by [`CREATIVE_TABS`] index.
    Tab(usize),
    /// A grid cell, `0..45`. May be empty — the page contents are the authority.
    Grid(usize),
    /// The player's hotbar, `0..9`.
    Hotbar(usize),
    /// A player-inventory slot on the inventory tab, by menu index.
    Inventory(usize),
    /// The inventory tab's trash slot.
    Destroy,
    /// The scrollbar track (a press here begins a drag).
    Scrollbar,
    /// The search box.
    SearchBox,
    /// The panel body — consumed, so it does not fall through to the world.
    Panel,
}

fn inside(r: Rect, px: f32, py: f32) -> bool {
    px >= r.x && py >= r.y && px < r.x + r.w && py < r.y + r.h
}

/// Resolves a **physical** viewport cursor position against `layout`, using the
/// same `gui_scale`/`width`/`height` triple the layout was built from.
///
/// Tabs are tested first: the strip sits outside the panel, and a bottom-row tab
/// overlaps nothing, so order only matters for the 4 px the top row tucks under
/// the panel edge — which vanilla's own `mouseClicked` also gives to the tab
/// (`:488-497`, before `super.mouseClicked`).
#[must_use]
pub fn creative_hit_test(
    layout: &CreativeLayout,
    gui_scale: u32,
    width: u32,
    height: u32,
    cursor_x: f32,
    cursor_y: f32,
) -> Option<CreativeHit> {
    // The same division `hit_test_with_scale` performs — one expression, so a
    // click and a draw cannot disagree about where a widget is.
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    let (px, py) = (cursor_x / scale, cursor_y / scale);

    for (i, r) in layout.tab_hits.iter().enumerate() {
        if inside(*r, px, py) {
            return Some(CreativeHit::Tab(i));
        }
    }
    if let Some(r) = layout.search_box
        && inside(r, px, py)
    {
        return Some(CreativeHit::SearchBox);
    }
    if let Some(r) = layout.scroll_track
        && inside(r, px, py)
    {
        return Some(CreativeHit::Scrollbar);
    }
    for (i, r) in layout.grid.iter().enumerate() {
        if inside(*r, px, py) {
            return Some(CreativeHit::Grid(i));
        }
    }
    if let Some(r) = layout.destroy
        && inside(r, px, py)
    {
        return Some(CreativeHit::Destroy);
    }
    for (index, r) in &layout.inventory {
        if inside(*r, px, py) {
            return Some(CreativeHit::Inventory(*index));
        }
    }
    // Only where the inventory tab has not already claimed the row: its own
    // slots `36..45` sit at exactly these rects, and reporting `Hotbar` there
    // would give two names to one pixel.
    if layout.inventory.is_empty() {
        for (i, r) in layout.hotbar.iter().enumerate() {
            if inside(*r, px, py) {
                return Some(CreativeHit::Hotbar(i));
            }
        }
    }
    inside(layout.panel, px, py).then_some(CreativeHit::Panel)
}

/// Everything the draw needs that this module cannot derive itself.
#[derive(Debug, Clone, Copy)]
pub struct CreativeView<'a> {
    /// The player's own inventory menu, for the hotbar row and the inventory
    /// tab. `None` draws empty wells.
    pub menu: Option<&'a Menu>,
    /// The selected tab's display name, already resolved through the language
    /// table (`itemGroup.*`). Empty draws no title.
    pub title: &'a str,
}

/// Builds one frame of creative-screen geometry.
///
/// Returns a [`ContainerGeometry`] so
/// [`ContainerRenderer::render_geometry_scaled`](super::ContainerRenderer)
/// draws it through the ordinary container passes. The stream splits carry the
/// same meaning they do there, and the same warning applies: the four passes are
/// not a nicety, they are what keeps a stack count from being drawn under its own
/// icon.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn creative_geometry(
    state: &CreativeState,
    view: CreativeView<'_>,
    gui_scale: u32,
    width: u32,
    height: u32,
    items: Option<&ItemAtlas>,
    models: Option<&BlockModels>,
    font: Option<&crate::hud::VanillaFont>,
    background: Option<&ContainerBackground>,
) -> ContainerGeometry {
    let assets = IconAssets { items, models };
    let tab_items = creative_items_for(state.tab, &state.search);
    let layout = creative_layout(state, tab_items.len(), gui_scale, width, height);
    let kind = creative_tab_kind(state.tab);
    let (w, h) = crate::menu::render::logical_canvas(gui_scale, width, height);
    let mut b = Builder::new(w, h, font);

    // The same full-canvas dim every container screen draws, in its own leading
    // pass — see `ContainerGeometry::dim_vertex_count`.
    b.gradient_rect_px(
        0.0,
        0.0,
        w,
        h,
        [16.0 / 255.0, 16.0 / 255.0, 16.0 / 255.0, 192.0 / 255.0],
        [16.0 / 255.0, 16.0 / 255.0, 16.0 / 255.0, 208.0 / 255.0],
    );
    let dim_floats = b.verts.len();

    // The tab strip goes down **before** the panel sheet, exactly as
    // `extractBackground` does it (`:737-741` draws every unselected tab, then
    // the panel, then the selected tab last at `:756`): an unselected tab's art
    // is supposed to be partly covered by the panel edge, and the selected one is
    // not.
    for (i, tab) in CREATIVE_TABS.iter().enumerate() {
        if i == state.tab {
            continue;
        }
        push_tab(&mut b, background, &layout, i, tab, false);
    }
    if let Some(bg) = background
        && let Some(q) = bg.creative_quad(kind.background(), layout.panel.x, layout.panel.y)
    {
        b.bg_sprite(q);
    } else {
        // The jar-less picture, and the same fallback the ordinary container
        // screen keeps: a flat panel rather than nothing at all.
        b.rect_px(
            layout.panel.x,
            layout.panel.y,
            layout.panel.w,
            layout.panel.h,
            [0.08, 0.075, 0.065, 0.88],
        );
    }
    if let Some(tab) = CREATIVE_TABS.get(state.tab) {
        push_tab(&mut b, background, &layout, state.tab, tab, true);
    }
    if let (Some(bg), Some(thumb)) = (background, layout.scroll_thumb) {
        let id = if layout.can_scroll { SPRITE_SCROLLER } else { SPRITE_SCROLLER_DISABLED };
        if let Some(q) = bg.sprite_quad(id, thumb.x, thumb.y, thumb.w, thumb.h) {
            b.bg_sprite(q);
        }
    }
    // No hovered-slot highlight on this screen, so every background vertex is
    // under the items.
    let bg_slot_floats = b.bg_verts.len();

    // The search box's well: `tab_item_search.png` bakes it in, so this fill is
    // the jar-less picture only.
    if let Some(r) = layout.search_box.filter(|_| background.is_none()) {
        b.rect_px(r.x, r.y, r.w, r.h, [0.03, 0.03, 0.03, 1.0]);
    }
    if !view.title.is_empty() && kind.shows_title() {
        b.label(view.title, layout.panel.x + TITLE_X, layout.panel.y + TITLE_Y, 1.0, TITLE_COLOUR);
    }
    let chrome_floats = b.verts.len();

    // ---- the chrome/icon split ----

    let page = creative_page_items(&tab_items, state.scroll);
    if kind != CreativeTabKind::Inventory {
        for (cell, id) in layout.grid.iter().zip(page.iter()) {
            let Some(id) = id else { continue };
            let Some(stack) = stack_of(id) else { continue };
            b.draw_stack(&assets, &stack, cell.x, cell.y);
        }
        for (i, cell) in layout.hotbar.iter().enumerate() {
            let Some(stack) = view.menu.and_then(|m| m.slot_item(36 + i)) else {
                continue;
            };
            b.draw_stack(&assets, stack, cell.x, cell.y);
        }
    } else {
        for (index, cell) in &layout.inventory {
            let Some(stack) = view.menu.and_then(|m| m.slot_item(*index)) else {
                continue;
            };
            b.draw_stack(&assets, stack, cell.x, cell.y);
        }
    }

    // The tab icons, after the tab art they sit on — `extractTabButton` calls
    // `graphics.item` after its own `blitSprite` (`:827-830`), and the sprite
    // stream is drawn between this module's two colour ranges.
    for (i, tab) in CREATIVE_TABS.iter().enumerate() {
        let Some(rect) = layout.tabs.get(i) else { continue };
        let Some(stack) = stack_of(tab.icon) else { continue };
        let dy = if tab.top_row { TAB_ICON_DY + 1.0 } else { TAB_ICON_DY - 1.0 };
        b.draw_stack(&assets, &stack, rect.x + TAB_ICON_DX, rect.y + dy);
    }

    if font.is_some()
        && let Some(r) = layout.search_box
    {
        let ty = r.y + ((r.h - 8.0) * 0.5).floor();
        b.shadowed_label(&state.search, r.x, ty, 1.0, [1.0, 1.0, 1.0, 1.0]);
        if state.search_focused {
            let cx = r.x + font.map_or(0.0, |f| f.width(&state.search, 1.0));
            b.rect_px(cx, ty - 1.0, 1.0, 11.0, [1.0, 1.0, 1.0, 1.0]);
        }
    }

    // Nothing on this screen draws a carried stack (the creative grid has no
    // cursor — see the module doc), so the slot stratum runs to the end of every
    // stream and the renderer's carried passes are empty by construction.
    let slot_floats = b.verts.len();
    let slot_item_floats = b.item_verts.len();
    let slot_glint_floats = b.glint_verts.len();
    let slot_model_verts = b.model_verts.len();
    let slot_special = b.special.len();

    ContainerGeometry {
        bg_slot_vertex_count: bg_slot_floats / BG_FLOATS_PER_VERTEX,
        dim_vertex_count: dim_floats / FLOATS_PER_VERTEX,
        chrome_vertex_count: chrome_floats / FLOATS_PER_VERTEX,
        slot_vertex_count: slot_floats / FLOATS_PER_VERTEX,
        slot_item_vertex_count: slot_item_floats / crate::hud::SPRITE_FLOATS_PER_VERTEX,
        slot_glint_vertex_count: slot_glint_floats / crate::hud::SPRITE_FLOATS_PER_VERTEX,
        slot_model_vertex_count: slot_model_verts,
        slot_special_count: slot_special,
        verts: b.verts,
        item_verts: b.item_verts,
        glint_verts: b.glint_verts,
        model_verts: b.model_verts,
        special: b.special,
        bg_verts: b.bg_verts,
        widget_rect: Some(layout.panel),
    }
}

fn push_tab(
    b: &mut Builder<'_>,
    background: Option<&ContainerBackground>,
    layout: &CreativeLayout,
    index: usize,
    tab: &CreativeTab,
    selected: bool,
) {
    let Some(rect) = layout.tabs.get(index) else { return };
    match background.and_then(|bg| {
        bg.sprite_quad(tab_sprite(tab.top_row, selected, tab.column), rect.x, rect.y, rect.w, rect.h)
    }) {
        Some(q) => b.bg_sprite(q),
        None => b.rect_px(
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            if selected { [0.52, 0.45, 0.33, 1.0] } else { [0.24, 0.21, 0.17, 1.0] },
        ),
    }
}

/// A stack of one, for an icon. `None` on a malformed id, which the transcribed
/// table makes impossible for its own contents — the fallible form exists so a
/// hostile id degrades to a blank cell rather than a panic.
fn stack_of(id: &str) -> Option<ItemStack> {
    id.parse::<Identifier>().ok().map(|id| ItemStack::new(id, 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tab_strip_position_is_unique() {
        // Vanilla's own `CreativeModeTabs.validate()` invariant, applied to the
        // rects this screen actually draws rather than to the table's fields.
        let layout = creative_layout(&CreativeState::default(), 0, 2, 1280, 720);
        for (i, a) in layout.tabs.iter().enumerate() {
            for b in &layout.tabs[i + 1..] {
                assert!(
                    (a.x - b.x).abs() > 0.5 || (a.y - b.y).abs() > 0.5,
                    "two tabs share the strip position ({}, {})",
                    a.x,
                    a.y
                );
            }
        }
    }

    #[test]
    fn the_grid_scrolls_by_whole_rows() {
        let items = creative_items_for(0, "");
        assert!(items.len() > CREATIVE_PAGE, "building blocks must be scrollable");
        let top = creative_page_items(&items, 0.0);
        assert_eq!(top[0], Some(items[0]));
        let bottom = creative_page_items(&items, 1.0);
        // The last page's first cell is the first item of the last visible row.
        let last_row = row_count(items.len());
        assert_eq!(bottom[0], Some(items[last_row * CREATIVE_COLS]));
        // The last *populated* cell is the list's own end, and the cells past it
        // are empty rather than wrapping — vanilla's `scrollTo` leaves a short
        // final page short instead of shrinking the grid.
        let last_filled = bottom.iter().rposition(Option::is_some).expect("a populated page");
        assert_eq!(bottom[last_filled], items.last().copied());
        assert!(bottom[last_filled + 1..].iter().all(Option::is_none));
    }

    #[test]
    fn a_click_on_each_tab_resolves_to_that_tab() {
        let state = CreativeState::default();
        let layout = creative_layout(&state, 0, 1, 1280, 720);
        for (i, r) in layout.tab_hits.iter().enumerate() {
            let hit = creative_hit_test(
                &layout,
                1,
                1280,
                720,
                r.x + r.w * 0.5,
                r.y + r.h * 0.5,
            );
            assert_eq!(hit, Some(CreativeHit::Tab(i)), "tab {i} is unclickable");
        }
    }

    #[test]
    fn a_click_on_a_grid_cell_resolves_to_that_cell() {
        let state = CreativeState::default();
        let layout = creative_layout(&state, 500, 1, 1280, 720);
        for (i, r) in layout.grid.iter().enumerate() {
            let hit =
                creative_hit_test(&layout, 1, 1280, 720, r.x + 1.0, r.y + 1.0);
            assert_eq!(hit, Some(CreativeHit::Grid(i)));
        }
    }

    #[test]
    fn the_search_tab_filters_the_union_of_every_other_tab() {
        let search = CREATIVE_TABS
            .iter()
            .position(|t| t.id == "minecraft:search")
            .expect("the search tab is in the table");
        let all = creative_items_for(search, "");
        assert!(all.len() > 1000, "the search tab should see most of the game: {}", all.len());
        let filtered = creative_items_for(search, "diamond");
        assert!(!filtered.is_empty());
        assert!(filtered.iter().all(|id| id.contains("diamond")));
        assert!(filtered.len() < all.len());
    }

    #[test]
    fn the_inventory_tab_has_no_scrollbar_and_no_title() {
        let inventory = CREATIVE_TABS
            .iter()
            .position(|t| t.id == "minecraft:inventory")
            .expect("the inventory tab is in the table");
        let kind = creative_tab_kind(inventory);
        assert!(!kind.scrolls());
        assert!(!kind.shows_title());
        let mut state = CreativeState::default();
        state.select_tab(inventory);
        let layout = creative_layout(&state, 0, 1, 1280, 720);
        assert!(layout.scroll_track.is_none());
        // 4 armour + offhand + 27 main + 9 hotbar = 41; the 2x2 crafting grid and
        // its result are moved off-screen by vanilla and omitted here.
        assert_eq!(layout.inventory.len(), 41);
        assert!(layout.destroy.is_some());
    }
}
