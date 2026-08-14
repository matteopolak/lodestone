//! The creative-inventory screen: tab strip, item grid, scrollbar,
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
//! The click semantics are a transcription of `slotClicked` — see
//! [`CreativeEffect`] and [`creative_item_list_click`], which is where the screen's
//! real behaviour lives and why it is not "a chest with a copy flag". Three deliberate
//! departures remain, each because the machinery it would need does not exist here yet:
//!
//! - **No quick-craft drag across the creative screen.** Vanilla, on a press with a
//!   loaded cursor, starts a drag and resolves it on release; a release over a single
//!   painted slot is then a plain `PICKUP` of that slot, so a click behaves identically
//!   and only a drag *across several* slots differs. This screen acts on press.
//! - **The `hotbar` tab is empty.** Vanilla fills it from saved hotbars on disk
//!   (`HotbarManager`), which this client has no store for. It draws its
//!   background, tab strip and the player's live hotbar row like every other
//!   tab, and its grid is honestly blank rather than showing something invented.
//! - **No item tooltips carry the tab-membership lines** vanilla's
//!   `getTooltipFromContainerItem` appends, nor the search tab's `#tag` lines; the
//!   hovered-slot tooltip itself is the ordinary container one, which is byte-for-byte
//!   what vanilla shows on a single-category tab.
//!
//! Note vanilla's `checkTabClicked` and `extractTabButton` disagree by 4 px
//! vertically (`getTabY` gives `-32`/`+imageHeight`, the blit uses
//! `-28`/`+imageHeight - 4`). That is vanilla's own behaviour, not a
//! transcription slip: the tab art tucks 4 px under the panel edge and that
//! overlapped strip is deliberately not clickable. [`creative_layout`] carries
//! both rects for that reason.

use lodestone_assets::ItemAtlas;
use lodestone_game::click::{Click, ContainerInput, PlayerCtx};
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
    /// The pointer in **physical** viewport pixels — the same space
    /// [`creative_hit_test`] takes. Drives the hovered-slot highlight, the carried
    /// stack (which follows the cursor), the tooltip, and the avatar's head.
    ///
    /// `None` for a caller with no pointer (every hermetic gate) draws none of those
    /// four, which is what keeps every pre-existing caller byte-identical.
    pub cursor: Option<[f32; 2]>,
    /// `Some(advanced)` draws the hovered item's tooltip, with the advanced (F3+H)
    /// lines when set. `None` draws none.
    pub tooltips: Option<bool>,
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
    // The hovered slot, resolved **once** and shared by the highlight pair, the
    // tooltip and (through it) the item under the pointer — the same one-resolution
    // rule `geometry.rs` follows so a highlight and a tooltip cannot disagree about
    // which slot the pointer is on.
    let hovered = view.cursor.and_then(|[cx, cy]| {
        creative_hit_test(&layout, gui_scale, width, height, cx, cy)
    });
    let hovered_rect = hovered.and_then(|hit| match hit {
        CreativeHit::Grid(cell) => layout.grid.get(cell).copied(),
        CreativeHit::Hotbar(i) => layout.hotbar.get(i).copied(),
        CreativeHit::Inventory(index) => layout
            .inventory
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, r)| *r),
        CreativeHit::Destroy => layout.destroy,
        _ => None,
    });

    // `extractSlot`'s `if (itemStack.isEmpty() && slot.isActive())` empty-slot
    // placeholders — the helmet/chestplate/leggings/boots/shield silhouettes. The
    // creative screen reaches these through `super.extractBackground`, and its
    // inventory tab is the one tab that shows the slots that declare them. The id
    // comes off `Slot::no_item_icon`, so this loop and `GUI_SPRITES` (which stitches
    // exactly those five) agree by construction rather than by two transcriptions.
    if let (Some(bg), Some(menu)) = (background, view.menu) {
        for (index, cell) in &layout.inventory {
            if menu.slot_item(*index).is_some() {
                continue;
            }
            let Some(id) = menu.slot(*index).and_then(|s| s.no_item_icon) else {
                continue;
            };
            if let Some(q) = bg.sprite_quad(id, cell.x, cell.y, CELL, CELL) {
                b.bg_sprite(q);
            }
        }
    }

    // The hover highlight's *back* half, under the item.
    if let (Some(bg), Some(r)) = (background, hovered_rect)
        && let Some(q) = bg.sprite_quad(
            super::SLOT_HIGHLIGHT_BACK,
            r.x - super::HIGHLIGHT_INSET,
            r.y - super::HIGHLIGHT_INSET,
            super::HIGHLIGHT,
            super::HIGHLIGHT,
        )
    {
        b.bg_sprite(q);
    }
    // Everything appended to the background stream past here draws **after** the
    // slot item passes — the front half of the highlight pair, and nothing else.
    let bg_slot_floats = b.bg_verts.len();
    if let (Some(bg), Some(r)) = (background, hovered_rect)
        && let Some(q) = bg.sprite_quad(
            super::SLOT_HIGHLIGHT_FRONT,
            r.x - super::HIGHLIGHT_INSET,
            r.y - super::HIGHLIGHT_INSET,
            super::HIGHLIGHT,
            super::HIGHLIGHT,
        )
    {
        b.bg_sprite(q);
    }

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

    let slot_floats = b.verts.len();
    let slot_item_floats = b.item_verts.len();
    let slot_glint_floats = b.glint_verts.len();
    let slot_model_verts = b.model_verts.len();
    let slot_special = b.special.len();

    // ---- the carried stratum ----
    //
    // The cursor stack, above every slot and below the tooltip. It is the shared
    // `player.inventoryMenu` cursor: vanilla's `ItemPickerMenu.getCarried` delegates
    // there, so this screen and the survival inventory screen show one cursor, not two.
    //
    // `view.cursor` is physical viewport space and this builder draws in the logical
    // canvas, so it is divided by the same effective scale `creative_hit_test` divides
    // its own input by — without that the stack drifts toward a corner as the GUI scale
    // grows.
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    let cursor_logical = view.cursor.map(|[cx, cy]| [cx / scale, cy / scale]);
    let carried = view.menu.and_then(Menu::carried);
    if let (Some([cx, cy]), Some(stack)) = (cursor_logical, carried) {
        b.draw_stack(&assets, stack, cx - CELL * 0.5, cy - CELL * 0.5);
    }

    // The hovered item's tooltip, last of everything — the tail of the stream is what
    // puts it on top. Suppressed while something is carried, vanilla's
    // `hoveredSlot.hasItem() && carried.isEmpty()`, which is also what makes the
    // layering above sound: the two can never both want the same pixels.
    //
    // Vanilla's `getTooltipFromContainerItem` override additionally prepends the
    // blue tab-membership lines on the search/hotbar/inventory tabs, and the
    // `#tag` lines on the search tab. Neither is modelled: the first needs a
    // resolved `itemGroup.*` display name per tab inside this module (it holds one
    // title string, not fourteen), and the second needs the item-tag corpus. The
    // *base* tooltip — the one Matthew reported missing entirely — is the ordinary
    // container one, and on a single-category tab that is byte-for-byte what
    // vanilla shows, because that branch returns `originalLines` untouched.
    if let (Some(advanced), None) = (view.tooltips, carried) {
        let stack = match hovered {
            Some(CreativeHit::Grid(cell)) if kind != CreativeTabKind::Inventory => {
                page.get(cell).copied().flatten().and_then(stack_of)
            }
            Some(CreativeHit::Hotbar(i)) => {
                view.menu.and_then(|m| m.slot_item(36 + i)).cloned()
            }
            Some(CreativeHit::Inventory(index)) => {
                view.menu.and_then(|m| m.slot_item(index)).cloned()
            }
            _ => None,
        };
        if let Some(stack) = stack {
            super::tooltip::emit_tooltip_for_stack(
                &mut b,
                &stack,
                view.cursor,
                advanced,
                gui_scale,
                width,
                height,
                (w, h),
            );
        }
    }

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
        // The avatar, on the inventory tab only.
        //
        // This field used to be `None` with a comment asserting that vanilla's
        // creative screen never calls `extractEntityInInventoryFollowsMouse`. It
        // does: `CreativeModeInventoryScreen.extractBackground` ends with exactly
        // that call, gated on `selectedTab.getType() == Type.INVENTORY`, into a
        // 32x43 recess at `(+73, +6)` at scale 20 — a different rect *and* a
        // different scale from `InventoryScreen`'s, which is why
        // `PlayerAvatar::creative` exists rather than reusing `new`.
        player_avatar: (kind == CreativeTabKind::Inventory).then(|| {
            super::PlayerAvatar::creative(layout.panel.x, layout.panel.y, cursor_logical)
        }),
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

/// A stack of one **carrying the item's prototype components** — vanilla's
/// `new ItemStack(item)`, whose max stack size, max damage and equippable slot come
/// from the item's built-in component map rather than from any patch.
///
/// `None` on a malformed id, which the transcribed table makes impossible for its own
/// contents; the fallible form exists so a hostile id degrades to a blank cell rather
/// than a panic.
///
/// # Why the prototype is not optional here
///
/// A clientbound stack never carries these three on the wire — they are the item's
/// defaults, and `lodestone_data::item_prototypes` is the census of them. Building the
/// list entry without them has three visible consequences, all of which read as
/// unrelated bugs:
///
/// * every stack caps at [`crate::hud`]'s default of 64, so a bucket, an ender pearl
///   or a shulker box picks up 64 from the item list and the server corrects it;
/// * `equippable` is absent, so `Slot::may_place` on an armour slot compares
///   `None == Some(_)` and **no creative armour can be put into an armour slot at
///   all**;
/// * `max_damage` is absent, so two identical swords stack.
///
/// Routed through `lodestone_model::ItemStack` and the existing
/// `From<&lodestone_model::ItemStack>` conversion rather than inserting the three
/// components by hand, so the component *names* have exactly one transcription in the
/// tree — the one the wire decoder already uses.
fn stack_of(id: &str) -> Option<ItemStack> {
    let item = id.parse::<Identifier>().ok()?;
    let mut model = lodestone_model::ItemStack::new(item, 1);
    if let Some(proto) = lodestone_data::item_prototypes::model_prototype(id) {
        model.components.max_stack_size = Some(proto.max_stack_size);
        model.components.max_damage = proto.max_damage;
        model.components.equippable = proto.equip_slot;
    }
    Some(ItemStack::from(&model))
}

/// One consequence of a creative-screen click, in the order it must be applied.
///
/// The creative screen is not a chest with a copy flag: vanilla gives it its own
/// `slotClicked` override that intercepts **before** the ordinary container path, and
/// the item list behind it is an infinite source with no server container. So a click
/// there cannot become a `container_click` — the server's cursor is empty and would
/// reject an item the client minted. Every mutation is instead applied locally and
/// reported per slot with `SET_CREATIVE_MODE_SLOT`, which is exactly what vanilla's
/// `handleCreativeModeItemAdd` / `handleCreativeModeItemDrop` do.
#[derive(Debug, Clone, PartialEq)]
pub enum CreativeEffect {
    /// Replace the cursor stack. Client-only — there is no wire verb for the cursor,
    /// and vanilla's `ItemPickerMenu.setCarried` delegates straight to
    /// `player.inventoryMenu`, so the creative screen and the survival inventory
    /// screen share one cursor rather than each having their own.
    SetCarried(Option<ItemStack>),
    /// Write one **window-0 menu slot** and report it with `SET_CREATIVE_MODE_SLOT`
    /// (`handleCreativeModeItemAdd`). `menu_index` is the same numbering
    /// `container.rs`'s slot layout uses, which is also the numbering that packet's
    /// `slot` field is defined in.
    SetSlot {
        /// Window-0 menu slot index.
        menu_index: usize,
        /// New contents.
        item: Option<ItemStack>,
    },
    /// Throw a stack into the world — `handleCreativeModeItemDrop`, i.e.
    /// `SET_CREATIVE_MODE_SLOT` with vanilla's `-1` "drop" slot.
    Drop(ItemStack),
    /// Empty every player-inventory slot. Vanilla's shift-click on the inventory
    /// tab's trash slot, which loops `inventoryMenu.getItems()` setting each to empty.
    ClearInventory,
}

/// Vanilla's `Inventory.setItem(buttonNum, …)` target for a `SWAP` click, as a
/// **window-0 menu index**.
///
/// Vanilla's `buttonNum` here is a *native inventory* index — `0..=8` is the hotbar
/// and `40` is the off-hand, which is what `AbstractContainerScreen`'s hotbar-key and
/// off-hand-key handlers pass. Window 0 puts the hotbar at menu `36..=44` and the
/// off-hand at `45`, so the two spaces differ by 36 for the hotbar and are unrelated
/// for the off-hand. Getting this mapping wrong swaps into a *main inventory* slot and
/// looks like the key doing nothing.
#[must_use]
fn swap_target_menu_index(button: i32) -> Option<usize> {
    match button {
        0..=8 => Some(36 + button as usize),
        40 => Some(45),
        _ => None,
    }
}

/// `stack` at its own maximum count — vanilla's `copyWithCount(getMaxStackSize())`.
///
/// **Not 64.** The limit is the item's own, which [`stack_of`] has already attached
/// from the prototype census: a bucket is 1, an ender pearl 16, a snowball 16.
fn full_stack(stack: &ItemStack) -> ItemStack {
    let mut out = stack.clone();
    out.set_count(stack.max_stack_size());
    out
}

fn with_count(stack: &ItemStack, count: i32) -> ItemStack {
    let mut out = stack.clone();
    out.set_count(count);
    out
}

/// A click on one of the 45 **item-list** cells — a transcription of
/// `CreativeModeInventoryScreen.slotClicked`'s `slot.container == CONTAINER` branch.
///
/// `clicked` is the list entry under the pointer (`None` on a cell past the end of the
/// list) and `carried` is the shared cursor. The list is an infinite source, so nothing
/// here ever writes back to it — that is why no branch produces a `SetSlot` for the
/// grid.
///
/// # The counts are the part worth reading twice
///
/// A plain left-click yields `clicked.getCount()`, and the list holds stacks of **one**
/// (`CreativeModeTab`'s output builds `new ItemStack(item)`), so **one left-click gives
/// one item** — clicking the same entry again `grow(1)`s the cursor. It is
/// `QUICK_MOVE` (shift), `CLONE` (middle / pick-item) and `SWAP` (the hotbar number
/// keys) that yield `getMaxStackSize()`. "Click gives you a full stack" is the
/// plausible wrong reading; the record is explicit and the two disagree on the very
/// first click.
#[must_use]
pub fn creative_item_list_click(
    input: ContainerInput,
    button: i32,
    clicked: Option<&ItemStack>,
    carried: Option<&ItemStack>,
) -> Vec<CreativeEffect> {
    let quick = input == ContainerInput::QuickMove;
    match input {
        // `inventory.setItem(buttonNum, clicked.copyWithCount(clicked.getMaxStackSize()))`
        // then `return` — the cursor is untouched and no exchange happens, unlike a
        // swap against a real container slot.
        ContainerInput::Swap => match (clicked, swap_target_menu_index(button)) {
            (Some(clicked), Some(menu_index)) => vec![CreativeEffect::SetSlot {
                menu_index,
                item: Some(full_stack(clicked)),
            }],
            _ => Vec::new(),
        },
        // `if (carried.isEmpty() && slot.hasItem()) setCarried(copyWithCount(max))`.
        // A loaded cursor makes this a no-op, which is why middle-clicking with
        // something in hand does nothing rather than swapping.
        ContainerInput::Clone => match (carried, clicked) {
            (None, Some(clicked)) => vec![CreativeEffect::SetCarried(Some(full_stack(clicked)))],
            _ => Vec::new(),
        },
        // `copyWithCount(buttonNum == 0 ? 1 : maxStackSize)`, dropped into the world.
        // The cursor is not involved at all.
        ContainerInput::Throw => match clicked {
            Some(clicked) => {
                let count = if button == 0 { 1 } else { clicked.max_stack_size() };
                vec![CreativeEffect::Drop(with_count(clicked, count))]
            }
            None => Vec::new(),
        },
        // `PICKUP`, `QUICK_MOVE`, and anything else that reaches the branch.
        // `QUICK_CRAFT` never does: `ItemPickerMenu.canDragTo` refuses every
        // `CONTAINER` slot, so a drag across the item list distributes nothing.
        _ => match (carried, clicked) {
            (Some(carried), Some(clicked))
                if ItemStack::is_same_item_same_components(carried, clicked) =>
            {
                let mut next = carried.clone();
                if button == 0 {
                    if quick {
                        next.set_count(next.max_stack_size());
                    } else if next.count() < next.max_stack_size() {
                        next.grow(1);
                    }
                } else {
                    next.shrink(1);
                }
                vec![CreativeEffect::SetCarried(lodestone_game::item::normalize(next))]
            }
            (None, Some(clicked)) => {
                let count = if quick {
                    clicked.max_stack_size()
                } else {
                    clicked.count()
                };
                vec![CreativeEffect::SetCarried(Some(with_count(clicked, count)))]
            }
            // Everything else, including a **loaded cursor over an empty cell or a
            // different item**: left-click destroys the whole cursor stack and
            // right-click takes one off it. This is real vanilla behaviour and not a
            // case to guard against — the item list doubles as the screen's bin.
            (carried, _) => {
                if button == 0 {
                    vec![CreativeEffect::SetCarried(None)]
                } else if let Some(carried) = carried {
                    let mut next = carried.clone();
                    next.shrink(1);
                    vec![CreativeEffect::SetCarried(lodestone_game::item::normalize(next))]
                } else {
                    Vec::new()
                }
            }
        },
    }
}

/// A click on one of the **player-inventory** slots the creative screen shows — the
/// hotbar row on every category tab, and all 41 slots on the inventory tab.
///
/// This is the "like a chest" half: vanilla routes these straight into
/// `player.inventoryMenu.clicked(target.index, …)`, i.e. the ordinary click matrix,
/// which is why left/right/shift/double/number-key all keep working there. Rather than
/// restate that matrix, the click is applied to a **clone** of the menu through the
/// same [`Click::apply`] the container screen uses, and the clone is then diffed
/// against the original to produce the per-slot writes `SET_CREATIVE_MODE_SLOT` has to
/// carry. A second copy of `doClick` in the shell would be free to drift from the one
/// `container.rs` already tests.
///
/// `PlayerCtx::creative()` and not `survival()`: this screen only exists for a player
/// with `instabuild`, and the context decides which drag types are legal and whether a
/// clone click resolves at all.
#[must_use]
pub fn creative_inventory_click(menu: &Menu, click: Click) -> Vec<CreativeEffect> {
    let before = menu.snapshot();
    let before_carried = menu.carried().cloned();
    let mut after = menu.clone();
    let outcome = click.apply(&mut after, PlayerCtx::creative());

    let mut out = Vec::new();
    for (menu_index, item) in after.snapshot().into_iter().enumerate() {
        if before.get(menu_index) != Some(&item) {
            out.push(CreativeEffect::SetSlot { menu_index, item });
        }
    }
    let carried = after.carried().cloned();
    if carried != before_carried {
        out.push(CreativeEffect::SetCarried(carried));
    }
    out.extend(outcome.dropped.into_iter().map(CreativeEffect::Drop));
    out
}

/// Resolve a whole creative-screen click: which region was hit, then which of the two
/// matrices above applies.
///
/// `page` is [`creative_page_items`]'s output for the current scroll — the authority on
/// which of the 45 cells is populated — and `menu` is the player's own inventory menu,
/// which owns the shared cursor.
#[must_use]
pub fn creative_click(
    hit: CreativeHit,
    input: ContainerInput,
    button: i32,
    tab: CreativeTabKind,
    page: &[Option<&str>],
    menu: &Menu,
) -> Vec<CreativeEffect> {
    match hit {
        CreativeHit::Grid(cell) if tab != CreativeTabKind::Inventory => {
            let clicked = page.get(cell).copied().flatten().and_then(stack_of);
            creative_item_list_click(input, button, clicked.as_ref(), menu.carried())
        }
        // The hotbar row is window 0's `36 + i`, on every tab that draws it.
        CreativeHit::Hotbar(i) => {
            creative_inventory_click(
                menu,
                Click { slot: (36 + i) as i32, button, input },
            )
        }
        // The inventory tab's slots already carry their own menu index.
        CreativeHit::Inventory(menu_index) => {
            creative_inventory_click(
                menu,
                Click { slot: menu_index as i32, button, input },
            )
        }
        // `slot == this.destroyItemSlot`: shift-click empties the whole inventory,
        // any other click just discards the cursor.
        CreativeHit::Destroy => {
            if input == ContainerInput::QuickMove {
                vec![CreativeEffect::ClearInventory]
            } else {
                vec![CreativeEffect::SetCarried(None)]
            }
        }
        CreativeHit::Grid(_)
        | CreativeHit::Tab(_)
        | CreativeHit::Scrollbar
        | CreativeHit::SearchBox
        | CreativeHit::Panel => Vec::new(),
    }
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

    /// The three stack limits every count assertion below is measured against, read off
    /// `Items.java`'s own `Item.Properties` rather than assumed.
    ///
    /// **`minecraft:bucket` stacks to 16 in 26.2**, not 1 — `registerItem(ItemIds.BUCKET,
    /// … new Item.Properties().stacksTo(16))`. It is `water_bucket`/`lava_bucket`/
    /// `milk_bucket` that are `stacksTo(1)`. Reaching for "a bucket is unstackable"
    /// would have been the plausible-round-number failure this repo's own rules warn
    /// about, in the direction that reads as a code bug.
    const STONE_MAX: i32 = 64;
    /// `SNOWBALL = registerItem(ItemIds.SNOWBALL, SnowballItem::new, …stacksTo(16))`.
    const SNOWBALL_MAX: i32 = 16;
    /// `WATER_BUCKET = registerItem(… .craftRemainder(BUCKET).stacksTo(1))`.
    const WATER_BUCKET_MAX: i32 = 1;

    fn list_entry(id: &str) -> ItemStack {
        stack_of(id).expect("a real item id")
    }

    /// Every count in the item-list matrix is the item's *own* limit. `64` is only the
    /// answer for an item whose limit happens to be 64, and a gate that used stone alone
    /// would pass under a hardcoded `64` — so each assertion is made at two different
    /// limits.
    #[test]
    fn the_item_lists_prototype_supplies_the_real_stack_limit() {
        assert_eq!(list_entry("minecraft:stone").max_stack_size(), STONE_MAX);
        assert_eq!(
            list_entry("minecraft:snowball").max_stack_size(),
            SNOWBALL_MAX
        );
        assert_eq!(
            list_entry("minecraft:water_bucket").max_stack_size(),
            WATER_BUCKET_MAX
        );
        // The equippable component has to survive too, or no creative armour can enter
        // an armour slot — `ArmorSlot.mayPlace` is `isEquippableInSlot`.
        assert!(
            list_entry("minecraft:diamond_helmet")
                .components()
                .get_str(lodestone_game::item::EQUIPPABLE_COMPONENT)
                .is_some(),
            "an armour item's list entry must carry minecraft:equippable"
        );
    }

    /// The owner's report: "if i hotkey it, it should make a stack of 64 in that slot,
    /// not 1". The record is `inventory.setItem(buttonNum, clicked.copyWithCount(
    /// clicked.getMaxStackSize()))`, so the rule is the *limit*, not 64.
    ///
    /// Two things are pinned here that fail in different ways: the count, and the slot.
    /// Vanilla's `buttonNum` is a **native** inventory index, and window 0 puts hotbar
    /// slot `n` at menu `36 + n` — passing it through unmapped writes into the main
    /// inventory and looks like the key doing nothing.
    #[test]
    fn a_hotbar_key_over_the_item_list_fills_that_slot_to_the_items_own_limit() {
        for (id, limit) in [
            ("minecraft:stone", STONE_MAX),
            ("minecraft:snowball", SNOWBALL_MAX),
            ("minecraft:water_bucket", WATER_BUCKET_MAX),
        ] {
            let entry = list_entry(id);
            let effects =
                creative_item_list_click(ContainerInput::Swap, 3, Some(&entry), None);
            let CreativeEffect::SetSlot { menu_index, item } = &effects[0] else {
                panic!("a swap must write a slot, got {effects:?}");
            };
            assert_eq!(effects.len(), 1, "a swap touches the slot and nothing else");
            assert_eq!(*menu_index, 39, "hotbar slot 3 is window-0 menu slot 39");
            assert_eq!(item.as_ref().map(ItemStack::count), Some(limit));
        }
        // The off-hand key is vanilla's button 40, and window 0 puts the off-hand at
        // menu slot 45 — not at 40, and not at 76.
        let entry = list_entry("minecraft:stone");
        let effects = creative_item_list_click(ContainerInput::Swap, 40, Some(&entry), None);
        assert!(matches!(
            effects.as_slice(),
            [CreativeEffect::SetSlot { menu_index: 45, .. }]
        ));
    }

    /// A plain left-click yields `clicked.getCount()`, and the list holds stacks of one,
    /// so **one** item lands on the cursor and a second click grows it to two. Shift
    /// yields `getMaxStackSize()`. The two readings differ on the very first click,
    /// which is why this asserts the count rather than "something was picked up".
    #[test]
    fn left_click_takes_one_and_shift_click_takes_a_full_stack() {
        let entry = list_entry("minecraft:snowball");

        let first = creative_item_list_click(ContainerInput::Pickup, 0, Some(&entry), None);
        assert_eq!(
            first,
            vec![CreativeEffect::SetCarried(Some(with_count(&entry, 1)))],
            "a plain left-click on the item list takes one item, not a stack"
        );

        // Clicking the same entry again with one already in hand: `carried.grow(1)`.
        let one = with_count(&entry, 1);
        let second =
            creative_item_list_click(ContainerInput::Pickup, 0, Some(&entry), Some(&one));
        assert_eq!(
            second,
            vec![CreativeEffect::SetCarried(Some(with_count(&entry, 2)))]
        );

        // Shift over an empty cursor: the item's own limit, 16 here and not 64.
        let shifted =
            creative_item_list_click(ContainerInput::QuickMove, 0, Some(&entry), None);
        assert_eq!(
            shifted,
            vec![CreativeEffect::SetCarried(Some(with_count(
                &entry,
                SNOWBALL_MAX
            )))]
        );

        // Right-click with a loaded cursor takes one *off* it, and an empty cursor
        // normalizes to `None` rather than to a count-0 stack.
        let two = with_count(&entry, 2);
        assert_eq!(
            creative_item_list_click(ContainerInput::Pickup, 1, Some(&entry), Some(&two)),
            vec![CreativeEffect::SetCarried(Some(with_count(&entry, 1)))]
        );
        assert_eq!(
            creative_item_list_click(ContainerInput::Pickup, 1, Some(&entry), Some(&one)),
            vec![CreativeEffect::SetCarried(None)]
        );
    }

    /// Middle-click (`CLONE`) always takes a full stack, and does nothing at all with a
    /// loaded cursor — `if (carried.isEmpty() && slot.hasItem())`, then `return`.
    #[test]
    fn middle_click_clones_a_full_stack_only_into_an_empty_cursor() {
        let entry = list_entry("minecraft:water_bucket");
        assert_eq!(
            creative_item_list_click(ContainerInput::Clone, 0, Some(&entry), None),
            vec![CreativeEffect::SetCarried(Some(with_count(
                &entry,
                WATER_BUCKET_MAX
            )))]
        );
        let held = list_entry("minecraft:stone");
        assert_eq!(
            creative_item_list_click(ContainerInput::Clone, 0, Some(&entry), Some(&held)),
            Vec::new(),
            "a loaded cursor makes a clone click a no-op"
        );
    }

    /// Dropping a cursor stack into the item-list area **destroys** it. Real vanilla
    /// behaviour — the `else if (buttonNum == 0) setCarried(EMPTY)` fall-through — and
    /// not a case to guard against: the item list doubles as the screen's bin.
    #[test]
    fn the_item_list_is_the_bin_for_a_loaded_cursor() {
        let held = with_count(&list_entry("minecraft:stone"), 10);
        // Over an empty cell.
        assert_eq!(
            creative_item_list_click(ContainerInput::Pickup, 0, None, Some(&held)),
            vec![CreativeEffect::SetCarried(None)]
        );
        // Over a *different* item, which is the same fall-through: the cursor goes,
        // and the list entry is untouched because the list is a source and never a sink.
        let other = list_entry("minecraft:snowball");
        assert_eq!(
            creative_item_list_click(ContainerInput::Pickup, 0, Some(&other), Some(&held)),
            vec![CreativeEffect::SetCarried(None)]
        );
        // Right-click over an empty cell shaves one off instead.
        assert_eq!(
            creative_item_list_click(ContainerInput::Pickup, 1, None, Some(&held)),
            vec![CreativeEffect::SetCarried(Some(with_count(
                &list_entry("minecraft:stone"),
                9
            )))]
        );
    }

    /// `THROW` from the item list: one item on button 0 and the item's own maximum on
    /// button 1 (ctrl), with the cursor untouched either way.
    #[test]
    fn throwing_from_the_item_list_never_touches_the_cursor() {
        let entry = list_entry("minecraft:snowball");
        assert_eq!(
            creative_item_list_click(ContainerInput::Throw, 0, Some(&entry), None),
            vec![CreativeEffect::Drop(with_count(&entry, 1))]
        );
        assert_eq!(
            creative_item_list_click(ContainerInput::Throw, 1, Some(&entry), None),
            vec![CreativeEffect::Drop(with_count(&entry, SNOWBALL_MAX))]
        );
    }

    /// The "like a chest" half: a click on a player-inventory slot runs the ordinary
    /// matrix and comes back as per-slot writes, because `SET_CREATIVE_MODE_SLOT` is
    /// what carries them.
    #[test]
    fn an_inventory_slot_click_yields_the_slots_it_actually_changed() {
        let mut menu = Menu::player();
        let stone = with_count(&list_entry("minecraft:stone"), 12);
        menu.set_slot_item(36, Some(stone.clone()));

        // Left-click the hotbar slot with an empty cursor: the slot empties onto the
        // cursor. Two effects, and the counts are exact rather than "it moved".
        let effects = creative_inventory_click(&menu, Click::left(36));
        assert_eq!(
            effects,
            vec![
                CreativeEffect::SetSlot {
                    menu_index: 36,
                    item: None
                },
                CreativeEffect::SetCarried(Some(stone.clone())),
            ]
        );

        // Right-click splits: vanilla takes the *larger* half, so 12 leaves 6 and takes
        // 6 — but an odd count is the discriminating input, because "half rounded down"
        // and "half rounded up" agree on every even one.
        let mut odd = Menu::player();
        let thirteen = with_count(&list_entry("minecraft:stone"), 13);
        odd.set_slot_item(36, Some(thirteen));
        let effects = creative_inventory_click(&odd, Click::right(36));
        let carried = effects.iter().find_map(|e| match e {
            CreativeEffect::SetCarried(item) => item.as_ref().map(ItemStack::count),
            _ => None,
        });
        assert_eq!(carried, Some(7), "vanilla's split takes the larger half");
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
