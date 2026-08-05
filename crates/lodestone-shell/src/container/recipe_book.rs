//! The recipe-book panel (issue #163): geometry, hit test and paged contents.
//!
//! Split out of `container.rs` verbatim.

use lodestone_assets::ItemAtlas;
use lodestone_game::item::ItemStack;
use lodestone_game::menu::Menu;
use lodestone_game::recipe::RecipeBook;
use lodestone_render::{BlockModels, ModelVertex};

use crate::hud::item_icon::{IconAssets, SpecialIconDraw};

use super::builder::Builder;
use super::layout::{Rect, panel_origin_with_scale, slot_layout};
use super::FLOATS_PER_VERTEX;

// ---------------------------------------------------------------------------
// Recipe-book panel (issue #163)
// ---------------------------------------------------------------------------
//
// The toggle button + browsable panel (search box, category tabs, a paged
// icon grid) for the crafting-table and furnace-family screens. Every
// constant below is transcribed from the decompiled 26.2 client, cited by
// `file:line`, under `.cache/mc/26.2/client-src/net/minecraft/client/gui/
// screens/recipebook/`.
//
// **One deliberate, documented simplification**: this does *not* replicate
// `RecipeBookComponent.updateScreenPosition` — vanilla shifts the *main*
// container screen rightward when the book opens so the two never overlap
// (`RecipeBookComponent.java:173-182`). Doing that here would mean threading
// an "is the book open" flag through `panel_origin`/`hit_test`/
// `ContainerGeometry::build_inner`, which *every* container screen already
// calls, for a change scoped to two of them. Instead the book panel is
// clamped to a minimum left margin ([`RECIPE_PANEL_MIN_X`]) and may overlap
// the main panel's own left edge at narrow canvases rather than displacing
// it — a bounded, visible cosmetic gap, not a crash or a hidden control.

/// `RecipeBookComponent.IMAGE_WIDTH`/`IMAGE_HEIGHT` (`RecipeBookComponent.
/// java:63-64`) — the panel's own background art size.
pub const RECIPE_PANEL_W: f32 = 147.0;
/// See [`RECIPE_PANEL_W`].
pub const RECIPE_PANEL_H: f32 = 166.0;
/// The gap this module keeps between the book panel and the (unshifted, see
/// the module doc above) main panel — vanilla's `BORDER_WIDTH`
/// (`RecipeBookComponent.java:66`), reused here for a different purpose than
/// vanilla's own (which is a widget-inset constant, not a screen gap) because
/// it is the nearest real vanilla constant for "how much breathing room
/// around this panel", and picking an unrelated number would be a guess.
pub const RECIPE_PANEL_GAP: f32 = 8.0;
/// Floor for the book panel's left edge in logical pixels — see the module
/// doc's "deliberate simplification".
const RECIPE_PANEL_MIN_X: f32 = 4.0;

/// The screen-toggle button, in **local coordinates off the main container
/// panel's own origin** (not the book panel's). Derived, not guessed:
/// `CraftingScreen.getRecipeBookButtonPosition` returns `(leftPos + 5,
/// height/2 - 49)` (`CraftingScreen.java:27`) and `topPos == (height -
/// imageHeight) / 2` for every `AbstractContainerScreen`
/// (`AbstractContainerScreen.java:78`), so subtracting the two —
/// `(leftPos+5) - leftPos = 5`, `(height/2-49) - (height/2-83) = 34` for
/// `imageHeight = 166` — cancels the screen height and leftPos out entirely,
/// leaving a width/height-independent local offset of `(5, 34)`, size `20x18`
/// (`AbstractRecipeBookScreen.java:40`).
///
/// **This is the crafting *table*'s offset only.** See
/// [`recipe_toggle_local`] — `getRecipeBookButtonPosition` is `abstract`
/// (`AbstractRecipeBookScreen.java:36`, no default) and each of the three
/// screen families overrides it with a *different* answer. Using this one
/// everywhere is what the owner saw as "the book in my inventory is in the
/// wrong spot": the player inventory's real offset is 99 px further right and
/// 27 px further down, so the button landed on the armour column instead.
pub const RECIPE_TOGGLE_LOCAL: Rect = Rect { x: 5.0, y: 34.0, w: 20.0, h: 18.0 };

/// The **player inventory** screen's toggle offset —
/// `InventoryScreen.getRecipeBookButtonPosition` returns
/// `new ScreenPosition(this.leftPos + 104, this.height / 2 - 22)`
/// (`InventoryScreen.java:64`). Same cancellation as
/// [`RECIPE_TOGGLE_LOCAL`]'s: `x = 104`, and
/// `y = (height/2 - 22) - (height/2 - 83) = 61`.
///
/// Geometrically this is not an arbitrary difference — the survival
/// inventory's 2×2 grid sits in the panel's *upper right* (the player model
/// occupies the left), so vanilla puts the button beside that grid, whereas
/// the crafting table's 3×3 grid is centred and its button goes to the far
/// left.
pub const RECIPE_TOGGLE_LOCAL_INVENTORY: Rect = Rect { x: 104.0, y: 61.0, w: 20.0, h: 18.0 };

/// The **furnace family**'s toggle offset (furnace, blast furnace, smoker,
/// which all inherit it) — `AbstractFurnaceScreen.getRecipeBookButtonPosition`
/// returns `new ScreenPosition(this.leftPos + 20, this.height / 2 - 49)`
/// (`AbstractFurnaceScreen.java:44`), i.e. the crafting table's `y = 34` but
/// `x = 20` rather than `5`. `FurnaceScreen`, `BlastFurnaceScreen` and
/// `SmokerScreen` declare no override of their own.
pub const RECIPE_TOGGLE_LOCAL_FURNACE: Rect = Rect { x: 20.0, y: 34.0, w: 20.0, h: 18.0 };

/// Which of the three jar-derived toggle offsets `menu`'s screen uses.
///
/// Dispatched through [`background_kind`](super::background::background_kind)
/// rather than a second hand-written `match` on
/// [`Menu::special_layout`]/[`Menu::kind`], for the same reason that function
/// exists: it already encodes "which vanilla screen class is this menu",
/// **including** the trap that a `special_layout` menu is mechanically a
/// [`MenuKind::Generic`](lodestone_game::menu::MenuKind::Generic) and would
/// otherwise fall through to the crafting-table case. Two independent
/// dispatches on the same question is how they drift apart.
///
/// Screens with no recipe book at all (a chest, an anvil, …) never reach a
/// draw of the toggle — `app`'s `recipe_book_type_for` returns `None` and no
/// geometry is built — so their arm here is unreachable in practice rather
/// than a claim about vanilla. It falls back to the crafting-table offset
/// because that is the shape a future book-bearing screen is most likely to
/// share, not because vanilla says so.
#[must_use]
pub fn recipe_toggle_local(menu: &Menu) -> Rect {
    use super::background::BackgroundKind;
    match super::background::background_kind(menu) {
        BackgroundKind::Inventory => RECIPE_TOGGLE_LOCAL_INVENTORY,
        BackgroundKind::Furnace | BackgroundKind::BlastFurnace | BackgroundKind::Smoker => {
            RECIPE_TOGGLE_LOCAL_FURNACE
        }
        _ => RECIPE_TOGGLE_LOCAL,
    }
}

/// The search box, local to the book panel's own origin —
/// `EditBox(font, xo + 25, yo + 13, 81, 9 + 5, ...)`
/// (`RecipeBookComponent.java:124`).
pub const RECIPE_SEARCH_BOX: Rect = Rect { x: 25.0, y: 13.0, w: 81.0, h: 14.0 };
/// The magnifier-icon hit region beside the search box —
/// `ScreenRectangle.of(HORIZONTAL, xo + 8, searchBox.y, searchBox.x - xo,
/// searchBox.height)` (`:130-132`); its width works out to the search box's
/// own local `x` (`25`), so the two regions deliberately overlap by design,
/// not by a transcription error.
pub const RECIPE_MAGNIFIER: Rect = Rect { x: 8.0, y: 13.0, w: 25.0, h: 14.0 };
/// The "All"/"Craftable" filter cycle-button — `.create(xo + 110, yo + 12,
/// 26, 16, ...)` (`:138`).
pub const RECIPE_FILTER_BUTTON: Rect = Rect { x: 110.0, y: 12.0, w: 26.0, h: 16.0 };

/// `RecipeBookTabButton.WIDTH`/`HEIGHT` (`RecipeBookTabButton.java:18-19`).
pub const RECIPE_TAB_W: f32 = 35.0;
/// See [`RECIPE_TAB_W`].
pub const RECIPE_TAB_H: f32 = 27.0;
/// Local x for every tab — `xPosTab = (width-147)/2 - xOffset - 30`, i.e.
/// `xo - 30` (`RecipeBookComponent.java:253`).
pub const RECIPE_TAB_X: f32 = -30.0;
/// Local y of the **first** visible tab — `yPosTab = (height-166)/2 + 3`,
/// i.e. `yo + 3` (`:254`).
pub const RECIPE_TAB_Y0: f32 = 3.0;
/// Vertical spacing between consecutive visible tabs (`:255`, `:262`).
pub const RECIPE_TAB_SPACING: f32 = 27.0;

/// Local origin of the recipe-icon grid's first cell —
/// `setPosition(xo + 11 + 25*(i%5), yo + 31 + 25*(i/5))`
/// (`RecipeBookPage.java:65`).
pub const RECIPE_GRID_ORIGIN: (f32, f32) = (11.0, 31.0);
/// Grid step, both axes (`:65`).
pub const RECIPE_GRID_STEP: f32 = 25.0;
/// Grid columns (`i % 5`, `:65`).
pub const RECIPE_GRID_COLS: usize = 5;
/// Grid rows (`ITEMS_PER_PAGE / COLS`, `:65`, `RecipeBookPage.java:25`).
pub const RECIPE_GRID_ROWS: usize = 4;
/// `RecipeBookPage.ITEMS_PER_PAGE` (`RecipeBookPage.java:25`).
pub const RECIPE_ITEMS_PER_PAGE: usize = RECIPE_GRID_COLS * RECIPE_GRID_ROWS;
/// `RecipeButton`'s own size (ctor `super(0, 0, 25, 25, ...)`,
/// `RecipeButton.java:37`).
pub const RECIPE_BUTTON_SIZE: f32 = 25.0;

/// Page-forward arrow — `ImageButton(xo + 93, yo + 137, 12, 17, ...)`
/// (`RecipeBookPage.java:68`).
pub const RECIPE_PAGE_FORWARD: Rect = Rect { x: 93.0, y: 137.0, w: 12.0, h: 17.0 };
/// Page-back arrow — `(xo + 38, yo + 137, 12, 17, ...)` (`:70`).
pub const RECIPE_PAGE_BACK: Rect = Rect { x: 38.0, y: 137.0, w: 12.0, h: 17.0 };

/// One recipe-icon grid cell's local rect, 0-indexed row-major (matches
/// [`RecipeBookPage`]'s own `buttons` order, and therefore
/// [`RecipeBookPanelContents::page_ids`]'s order).
#[must_use]
fn recipe_grid_cell_local(i: usize) -> Rect {
    let (col, row) = (i % RECIPE_GRID_COLS, i / RECIPE_GRID_COLS);
    Rect {
        x: RECIPE_GRID_ORIGIN.0 + RECIPE_GRID_STEP * col as f32,
        y: RECIPE_GRID_ORIGIN.1 + RECIPE_GRID_STEP * row as f32,
        w: RECIPE_BUTTON_SIZE,
        h: RECIPE_BUTTON_SIZE,
    }
}

/// Complete recipe-book panel geometry for one frame, in **absolute logical
/// canvas pixels** (the same post-gui-scale space [`panel_origin`] and
/// [`slot_layout`]'s [`SlotRect`]s already use), so it composes directly
/// with the main panel's own geometry without a caller re-deriving any
/// offset.
#[derive(Debug, Clone, PartialEq)]
pub struct RecipeBookPanelLayout {
    /// The panel's own background rect.
    pub panel: Rect,
    /// The always-present screen toggle (drawn/hit-tested even when the
    /// panel itself is closed — vanilla's own toggle button lives on the
    /// screen, not inside `RecipeBookComponent`).
    pub toggle: Rect,
    /// The search text box.
    pub search_box: Rect,
    /// The magnifier-icon click region beside it.
    pub magnifier: Rect,
    /// The "All"/"Craftable" filter cycle-button.
    pub filter_button: Rect,
    /// One rect per **visible** category tab (`tabs_for`/`visible_tabs`
    /// filtered), in vanilla's declaration order.
    pub tabs: Vec<Rect>,
    /// All 20 grid cells, always present regardless of how many are actually
    /// populated this page — matching vanilla's own fixed 20-button pool
    /// (`RecipeBookPage`'s `buttons`), which hides unused entries rather
    /// than not creating them.
    pub recipes: [Rect; RECIPE_ITEMS_PER_PAGE],
    /// The page-forward arrow, only present when there is a next page.
    pub page_forward: Option<Rect>,
    /// The page-back arrow, only present when there is a previous page.
    pub page_back: Option<Rect>,
}

/// Builds [`RecipeBookPanelLayout`] for `menu`'s own screen geometry — see
/// [`recipe_book_panel_layout_with_scale`] for the explicit-scale form
/// `app.rs`'s real hit-test path needs (mirroring [`panel_origin`]/
/// [`hit_test`]'s own pairing).
#[must_use]
pub fn recipe_book_panel_layout(
    menu: &Menu,
    width: u32,
    height: u32,
    tab_count: usize,
    has_prev_page: bool,
    has_next_page: bool,
) -> RecipeBookPanelLayout {
    recipe_book_panel_layout_with_scale(
        menu,
        crate::config::AUTO_GUI_SCALE,
        width,
        height,
        tab_count,
        has_prev_page,
        has_next_page,
    )
}

/// As [`recipe_book_panel_layout`], against an explicit `gui_scale` (`0` =
/// auto) — must be called with the same `gui_scale` the frame was last drawn
/// with, exactly like [`hit_test_with_scale`]'s own warning.
#[must_use]
pub fn recipe_book_panel_layout_with_scale(
    menu: &Menu,
    gui_scale: u32,
    width: u32,
    height: u32,
    tab_count: usize,
    has_prev_page: bool,
    has_next_page: bool,
) -> RecipeBookPanelLayout {
    let main_layout = slot_layout(menu);
    let (mx, my) = panel_origin_with_scale(&main_layout, gui_scale, width, height);
    // Vertically, vanilla's book and every screen this applies to share the
    // exact same `(canvas_h - 166) / 2` centring when both are 166 tall
    // (true for the crafting table and the whole furnace family — see
    // `slot_layout`'s own `height: 166.0` for both); the extra term below is
    // `0` for those two and keeps this correct if either ever isn't.
    let by = my + (main_layout.height - RECIPE_PANEL_H) * 0.5;
    let bx = (mx - RECIPE_PANEL_W - RECIPE_PANEL_GAP).max(RECIPE_PANEL_MIN_X);

    let at = |r: Rect| Rect { x: bx + r.x, y: by + r.y, w: r.w, h: r.h };
    let main_at = |r: Rect| Rect { x: mx + r.x, y: my + r.y, w: r.w, h: r.h };

    RecipeBookPanelLayout {
        panel: Rect { x: bx, y: by, w: RECIPE_PANEL_W, h: RECIPE_PANEL_H },
        toggle: main_at(recipe_toggle_local(menu)),
        search_box: at(RECIPE_SEARCH_BOX),
        magnifier: at(RECIPE_MAGNIFIER),
        filter_button: at(RECIPE_FILTER_BUTTON),
        tabs: (0..tab_count)
            .map(|i| Rect {
                // Tabs sit left of the panel at `bx + RECIPE_TAB_X` (`-30`),
                // so once `bx` itself has hit the `RECIPE_PANEL_MIN_X` floor
                // above, an *unclamped* tab x would land at `4.0 - 30.0 ==
                // -26.0` — off-canvas and unclickable, not merely
                // overlapping the panel as the module doc's "clamped, may
                // overlap" simplification describes. Clamping the tab's own
                // x to the same floor keeps every tab visible and hit-
                // testable (overlapping the panel body instead), which is
                // the "dead control is worse than a missing one" rule: a
                // tab nobody can click is worse than one drawn atop the
                // panel it belongs to.
                x: (bx + RECIPE_TAB_X).max(RECIPE_PANEL_MIN_X),
                y: by + RECIPE_TAB_Y0 + RECIPE_TAB_SPACING * i as f32,
                w: RECIPE_TAB_W,
                h: RECIPE_TAB_H,
            })
            .collect(),
        recipes: std::array::from_fn(|i| at(recipe_grid_cell_local(i))),
        page_forward: has_next_page.then(|| at(RECIPE_PAGE_FORWARD)),
        page_back: has_prev_page.then(|| at(RECIPE_PAGE_BACK)),
    }
}

/// What a viewport pixel is over, in the recipe-book panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipeBookPanelHit {
    /// The always-present screen toggle.
    Toggle,
    /// The search box (or its magnifier-icon click region).
    SearchBox,
    /// The "All"/"Craftable" filter button.
    FilterButton,
    /// A category tab, by its index into [`RecipeBookPanelLayout::tabs`]
    /// (which is already [`RecipeBook::visible_tabs`]'s own order).
    Tab(usize),
    /// A recipe-grid cell, by its index into [`RecipeBookPanelLayout::recipes`]
    /// (`0..20`) — a caller must still check the cell is actually populated
    /// this page before treating it as a click on a real recipe.
    Recipe(usize),
    /// The page-forward arrow.
    PageForward,
    /// The page-back arrow.
    PageBack,
    /// Inside the panel but not over any widget.
    Panel,
}

fn rect_hit(r: Rect, x: f32, y: f32) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

/// Resolves a **logical-canvas** pixel to a [`RecipeBookPanelHit`]. The
/// toggle is tested unconditionally (vanilla's toggle button lives on the
/// screen itself, not inside the closeable component); everything else only
/// while `open`.
#[must_use]
pub fn recipe_book_panel_hit_test(
    layout: &RecipeBookPanelLayout,
    open: bool,
    x: f32,
    y: f32,
) -> Option<RecipeBookPanelHit> {
    if rect_hit(layout.toggle, x, y) {
        return Some(RecipeBookPanelHit::Toggle);
    }
    if !open {
        return None;
    }
    if rect_hit(layout.search_box, x, y) || rect_hit(layout.magnifier, x, y) {
        return Some(RecipeBookPanelHit::SearchBox);
    }
    if rect_hit(layout.filter_button, x, y) {
        return Some(RecipeBookPanelHit::FilterButton);
    }
    for (i, r) in layout.tabs.iter().enumerate() {
        if rect_hit(*r, x, y) {
            return Some(RecipeBookPanelHit::Tab(i));
        }
    }
    for (i, r) in layout.recipes.iter().enumerate() {
        if rect_hit(*r, x, y) {
            return Some(RecipeBookPanelHit::Recipe(i));
        }
    }
    if let Some(r) = layout.page_forward
        && rect_hit(r, x, y)
    {
        return Some(RecipeBookPanelHit::PageForward);
    }
    if let Some(r) = layout.page_back
        && rect_hit(r, x, y)
    {
        return Some(RecipeBookPanelHit::PageBack);
    }
    if rect_hit(layout.panel, x, y) {
        return Some(RecipeBookPanelHit::Panel);
    }
    None
}

/// As [`recipe_book_panel_hit_test`], taking a **physical** viewport cursor
/// position and the same `gui_scale`/`width`/`height` the layout was built
/// with — mirrors [`hit_test`]/[`hit_test_with_scale`]'s own physical-to-
/// logical division exactly, so the two never disagree about scale.
#[must_use]
pub fn recipe_book_panel_hit_test_with_scale(
    layout: &RecipeBookPanelLayout,
    open: bool,
    gui_scale: u32,
    width: u32,
    height: u32,
    x: f32,
    y: f32,
) -> Option<RecipeBookPanelHit> {
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    recipe_book_panel_hit_test(layout, open, x / scale, y / scale)
}

/// The recipe ids the panel shows for one tab/search/page combination —
/// [`RecipeBook::browse`] plus pagination, kept as a separate query from
/// [`RecipeBookPanelLayout`] so geometry never needs a [`RecipeBook`] and a
/// data query never needs a viewport size.
#[derive(Debug, Clone, PartialEq)]
pub struct RecipeBookPanelContents<'a> {
    /// Visible category tabs for this book type, in vanilla's declaration
    /// order — feed `tabs.len()` to [`recipe_book_panel_layout`].
    pub tabs: Vec<lodestone_game::recipe::RecipeCategory>,
    /// Every matching recipe id, across all pages, in corpus id order.
    pub all_ids: Vec<&'a lodestone_model::Identifier>,
    /// This page's slice of `all_ids` (at most
    /// [`RECIPE_ITEMS_PER_PAGE`]) — index `i` corresponds to
    /// [`RecipeBookPanelLayout::recipes`]`[i]`.
    pub page_ids: Vec<&'a lodestone_model::Identifier>,
    /// Total page count (at least `1`, even for zero results, matching
    /// `RecipeBookPage.totalPages`'s own `ceil` — an empty result set is
    /// page `0` of `1`, not page `0` of `0`).
    pub total_pages: usize,
    /// The page actually shown, clamped into `0..total_pages`.
    pub page: usize,
}

/// Builds [`RecipeBookPanelContents`] for one `(book_type, tab, search,
/// page)` combination. `tab` indexes into [`RecipeBook::visible_tabs`]'s own
/// list (`None` is vanilla's "search"/all-categories tab, always tab index
/// `0` in this client's own tab ordering — see `docs/crafting.md`).
#[must_use]
pub fn recipe_book_panel_contents<'a>(
    book: &'a RecipeBook,
    book_type: lodestone_model::RecipeBookType,
    tab: Option<usize>,
    search: &str,
    page: usize,
) -> RecipeBookPanelContents<'a> {
    let tabs = book.visible_tabs(book_type);
    let category = tab.and_then(|i| tabs.get(i).copied());
    let all_ids = book.browse(book_type, category, search);
    let total_pages = all_ids.len().div_ceil(RECIPE_ITEMS_PER_PAGE).max(1);
    let page = page.min(total_pages - 1);
    let start = page * RECIPE_ITEMS_PER_PAGE;
    let page_ids = all_ids.iter().skip(start).take(RECIPE_ITEMS_PER_PAGE).copied().collect();
    RecipeBookPanelContents { tabs, all_ids, page_ids, total_pages, page }
}

/// Standalone geometry for one frame of the recipe-book panel, in the same
/// three streams [`ContainerGeometry`] uses (colour, flat item sprite, 3-D
/// block model) but kept as its **own** buffer rather than folded into
/// `ContainerGeometry::build_inner`. That function's vertex-range bookkeeping
/// ([`ContainerGeometry::chrome_vertex_count`] etc.) is already delicate and
/// covered by pixel gates that assume the main panel's own layout; a second,
/// independently-composited draw call is the additive, zero-risk way to reach
/// pixels without touching it. `app.rs` draws this in its own pass, the same
/// way it already composites the HUD and the container panel as separate
/// passes.
#[derive(Debug, Clone, Default)]
pub struct RecipeBookPanelGeometry {
    /// Flat `[x, y, r, g, b, a]` per vertex — panel, tabs, buttons, the
    /// atlas-less fallback swatch for an unrendered icon.
    pub verts: Vec<f32>,
    /// Flat `[x, y, u, v, r, g, b, a]` per flat-sprite item-icon vertex.
    pub item_verts: Vec<f32>,
    /// 3-D block-item icons, already posed into GUI pixel space.
    pub model_verts: Vec<ModelVertex>,
    /// Special-renderer (block-entity) icons — e.g. a chest recipe's own
    /// baked mesh, the same way a chest slot icon needs one elsewhere in
    /// this module. Never populated yet: no recipe result in the current
    /// corpus resolves to a special-renderer icon through `draw_stack`'s own
    /// dispatch, so this is honestly empty rather than dead — kept for
    /// parity with [`ContainerGeometry::special`] so a future caller
    /// threading a chest recipe through needs no struct change here.
    #[allow(dead_code)]
    pub(crate) special: Vec<SpecialIconDraw>,
}

impl RecipeBookPanelGeometry {
    /// Number of coloured vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.verts.len() / FLOATS_PER_VERTEX
    }
}

/// Vanilla's own icon inset within a `RecipeButton` — `offset = 4`
/// (`RecipeButton.java:104`, the non-multi-recipe branch — the "stack two
/// icons" `offset` dance for a multi-recipe button is not modelled, see the
/// module doc's other documented simplifications).
const RECIPE_ICON_INSET: f32 = 4.0;

/// Builds [`RecipeBookPanelGeometry`] for one frame, with **no item atlas**:
/// occupied icon cells fall back to the same hash-derived colour swatch every
/// other slot in this module falls back to. This is the jar-less/headless
/// path and the pixel gate's negative control — see
/// [`recipe_book_panel_geometry_with_icons`] for real icons.
///
/// `page_results` is the current page's recipe **result stacks**, in the same
/// order as [`RecipeBookPanelContents::page_ids`] (and therefore
/// [`RecipeBookPanelLayout::recipes`]): index `i` draws into `layout.recipes[i]`.
/// Fewer entries than populated cells draws only what is given — a caller
/// passing a short slice degrades to empty cells, not a panic or a wrap.
///
/// `gui_scale`/`width`/`height` must be the **same triple** `layout` was
/// built from (see [`recipe_book_panel_layout_with_scale`]) — they exist so
/// this function can derive the logical-canvas size through the exact same
/// [`crate::menu::render::logical_canvas`] call [`ContainerGeometry::build_inner`]
/// uses, rather than the caller (or this function) restating a `w`/`h`
/// constant that could silently drift from the one the layout was measured
/// against. Passing a mismatched triple reproduces this function's own
/// original bug: every pixel-space coordinate is divided by the *wrong*
/// canvas size, and the panel draws off-screen even though `layout` itself is
/// correct — see `recipe_panel_geometry_open_draws_inside_the_logical_screen_rect`'s
/// negative control below for exactly that failure mode.
#[must_use]
pub fn recipe_book_panel_geometry(
    layout: &RecipeBookPanelLayout,
    open: bool,
    selected_tab: Option<usize>,
    page_results: &[&ItemStack],
    gui_scale: u32,
    width: u32,
    height: u32,
) -> RecipeBookPanelGeometry {
    recipe_book_panel_geometry_inner(
        layout,
        open,
        selected_tab,
        page_results,
        gui_scale,
        width,
        height,
        &IconAssets { items: None, models: None },
    )
}

/// As [`recipe_book_panel_geometry`], drawing **real item icons** from the
/// atlases — the recipe-grid analogue of
/// [`ContainerGeometry::build_with_icons`].
#[must_use]
pub fn recipe_book_panel_geometry_with_icons(
    layout: &RecipeBookPanelLayout,
    open: bool,
    selected_tab: Option<usize>,
    page_results: &[&ItemStack],
    gui_scale: u32,
    width: u32,
    height: u32,
    items: &ItemAtlas,
    models: Option<&BlockModels>,
) -> RecipeBookPanelGeometry {
    recipe_book_panel_geometry_inner(
        layout,
        open,
        selected_tab,
        page_results,
        gui_scale,
        width,
        height,
        &IconAssets { items: Some(items), models },
    )
}

#[allow(clippy::too_many_arguments)]
fn recipe_book_panel_geometry_inner(
    layout: &RecipeBookPanelLayout,
    open: bool,
    selected_tab: Option<usize>,
    page_results: &[&ItemStack],
    gui_scale: u32,
    width: u32,
    height: u32,
    assets: &IconAssets<'_>,
) -> RecipeBookPanelGeometry {
    // The same logical-canvas expression `recipe_book_panel_layout_with_scale`
    // (via `panel_origin_with_scale`) and `ContainerGeometry::build_inner`
    // both already use — never a restated `w`/`h` constant. This is the
    // fix for the bug the negative control below pins: passing `(1.0, 1.0)`
    // here made every emitted vertex land far outside the `[-1, 1]` NDC clip
    // range, so the panel *had* geometry and drew exactly nothing.
    let (w, h) = crate::menu::render::logical_canvas(gui_scale, width, height);
    // The toggle button is the one widget that draws even when the panel
    // itself is closed — vanilla's toggle lives on the screen, not inside
    // `RecipeBookComponent` (see `RecipeBookPanelHit::Toggle`'s own doc).
    let mut b = Builder::new(w, h, None);
    b.rect_px(layout.toggle.x, layout.toggle.y, layout.toggle.w, layout.toggle.h, TOGGLE_COLOUR);
    if !open {
        return RecipeBookPanelGeometry {
            verts: b.verts,
            item_verts: b.item_verts,
            model_verts: b.model_verts,
            special: b.special,
        };
    }

    b.rect_px(layout.panel.x, layout.panel.y, layout.panel.w, layout.panel.h, PANEL_COLOUR);
    b.rect_px(
        layout.search_box.x,
        layout.search_box.y,
        layout.search_box.w,
        layout.search_box.h,
        SEARCH_BOX_COLOUR,
    );
    b.rect_px(
        layout.filter_button.x,
        layout.filter_button.y,
        layout.filter_button.w,
        layout.filter_button.h,
        BUTTON_COLOUR,
    );

    for (i, r) in layout.tabs.iter().enumerate() {
        let colour = if selected_tab == Some(i) { TAB_SELECTED_COLOUR } else { TAB_COLOUR };
        b.rect_px(r.x, r.y, r.w, r.h, colour);
    }

    for (i, r) in layout.recipes.iter().enumerate() {
        b.rect_px(r.x, r.y, r.w, r.h, RECIPE_SLOT_COLOUR);
        if let Some(stack) = page_results.get(i) {
            b.draw_stack(assets, stack, r.x + RECIPE_ICON_INSET, r.y + RECIPE_ICON_INSET);
        }
    }

    if let Some(r) = layout.page_forward {
        b.rect_px(r.x, r.y, r.w, r.h, BUTTON_COLOUR);
    }
    if let Some(r) = layout.page_back {
        b.rect_px(r.x, r.y, r.w, r.h, BUTTON_COLOUR);
    }

    RecipeBookPanelGeometry {
        verts: b.verts,
        item_verts: b.item_verts,
        model_verts: b.model_verts,
        special: b.special,
    }
}

/// Flat-fill colours for the panel's chrome, in the same muted palette
/// family the rest of this module's atlas-less fallback already uses (see
/// `build_inner`'s own `[0.08, 0.075, 0.065, 0.88]` panel fill) — this is not
/// vanilla's real `recipe_book.png`/`recipe_book/*` sprites, which this
/// module does not load; see the module doc's "deliberate simplification".
const PANEL_COLOUR: [f32; 4] = [0.09, 0.08, 0.07, 0.94];
/// See [`PANEL_COLOUR`].
const TOGGLE_COLOUR: [f32; 4] = [0.30, 0.26, 0.20, 1.0];
/// See [`PANEL_COLOUR`].
const SEARCH_BOX_COLOUR: [f32; 4] = [0.03, 0.03, 0.03, 1.0];
/// See [`PANEL_COLOUR`].
const BUTTON_COLOUR: [f32; 4] = [0.32, 0.32, 0.32, 1.0];
/// See [`PANEL_COLOUR`].
const TAB_COLOUR: [f32; 4] = [0.24, 0.21, 0.17, 1.0];
/// See [`PANEL_COLOUR`]. Brighter than [`TAB_COLOUR`] — the selected tab's
/// only visual distinction in this flat-fill fallback.
const TAB_SELECTED_COLOUR: [f32; 4] = [0.52, 0.45, 0.33, 1.0];
/// See [`PANEL_COLOUR`].
const RECIPE_SLOT_COLOUR: [f32; 4] = [0.16, 0.14, 0.12, 1.0];
