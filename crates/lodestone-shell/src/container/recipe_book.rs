//! The recipe-book panel: geometry, hit test and paged contents.
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
// Recipe-book panel
// ---------------------------------------------------------------------------
//
// The toggle button + browsable panel (search box, category tabs, a paged
// icon grid) for the crafting-table and furnace-family screens. Every
// constant below is transcribed from the decompiled 26.2 client's own
// recipe-book screen classes.
//
// **The screen shift is now real, and this note used to say it was not.** The
// old text read "this does *not* replicate `RecipeBookComponent.
// updateScreenPosition` ... Instead the book panel is clamped to a minimum left
// margin and may overlap the main panel's own left edge at narrow canvases".
// The owner reported the consequence: "when the screen isn't wide enough the
// four buttons on the side get squished into the menu". They were the four
// category tabs, all clamped to the same 4 px floor and stacked on the page.
//
// The clamp could never have worked, because the layout it was compensating for
// was inverted. Vanilla does not place the book relative to the container panel
// — the **book is screen-centred** (`getXOrigin()` is `(width - 147) / 2 -
// xOffset`) and the **panel is what moves** (`updateScreenPosition`). Both live
// in `super::layout` now: `recipe_book_panel_shift` and
// `recipe_book_width_too_narrow`, whose docs carry the arithmetic showing every
// constant meets at one pixel at `w == 379`.
//
// Threading the flag turned out not to need the 24 `panel_origin` call sites the
// old note feared: the shift is a *delta* a caller adds, so only the two places
// that know whether the book is open pass it (`ContainerFrame::with_book_open`
// for the draw, `layout::hit_test_with_book` for clicks).

/// `RecipeBookComponent.IMAGE_WIDTH`/`IMAGE_HEIGHT` (`RecipeBookComponent.
/// java:63-64`) — the panel's own background art size.
pub const RECIPE_PANEL_W: f32 = 147.0;
/// See [`RECIPE_PANEL_W`].
pub const RECIPE_PANEL_H: f32 = 166.0;
/// `RecipeBookComponent.BORDER_WIDTH`.
///
/// Kept because it is a real vanilla constant and is re-exported, but **no longer
/// used as a screen gap**: the book's x comes from `getXOrigin()` now, not from
/// the container panel minus a margin. See the module doc.
pub const RECIPE_PANEL_GAP: f32 = 8.0;

/// The screen-toggle button, in **local coordinates off the main container
/// panel's own origin** (not the book panel's). Derived, not guessed:
/// `CraftingScreen.getRecipeBookButtonPosition` returns `(leftPos + 5,
/// height/2 - 49)` and `topPos == (height -
/// imageHeight) / 2` for every `AbstractContainerScreen`
///, so subtracting the two —
/// `(leftPos+5) - leftPos = 5`, `(height/2-49) - (height/2-83) = 34` for
/// `imageHeight = 166` — cancels the screen height and leftPos out entirely,
/// leaving a width/height-independent local offset of `(5, 34)`, size `20x18`.
///
/// **This is the crafting *table*'s offset only.** See
/// [`recipe_toggle_local`] — `getRecipeBookButtonPosition` is `abstract`
/// (`AbstractRecipeBookScreen.java`, no default) and each of the three
/// screen families overrides it with a *different* answer. Using this one
/// everywhere is what the owner saw as "the book in my inventory is in the
/// wrong spot": the player inventory's real offset is 99 px further right and
/// 27 px further down, so the button landed on the armour column instead.
pub const RECIPE_TOGGLE_LOCAL: Rect = Rect { x: 5.0, y: 34.0, w: 20.0, h: 18.0 };

/// The **player inventory** screen's toggle offset —
/// `InventoryScreen.getRecipeBookButtonPosition` returns
/// `new ScreenPosition(this.leftPos + 104, this.height / 2 - 22)`
///. Same cancellation as
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
///, i.e. the crafting table's `y = 34` but
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
/// `EditBox(font, xo + 25, yo + 13, 81, 9 + 5, ...)`.
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

/// `RecipeBookTabButton.WIDTH`/`HEIGHT`.
pub const RECIPE_TAB_W: f32 = 35.0;
/// See [`RECIPE_TAB_W`].
pub const RECIPE_TAB_H: f32 = 27.0;
/// Local x for every tab — `xPosTab = (width-147)/2 - xOffset - 30`, i.e.
/// `xo - 30`.
pub const RECIPE_TAB_X: f32 = -30.0;
/// Local y of the **first** visible tab — `yPosTab = (height-166)/2 + 3`,
/// i.e. `yo + 3` (`:254`).
pub const RECIPE_TAB_Y0: f32 = 3.0;
/// Vertical spacing between consecutive visible tabs (`:255`, `:262`).
pub const RECIPE_TAB_SPACING: f32 = 27.0;

/// Local origin of the recipe-icon grid's first cell —
/// `setPosition(xo + 11 + 25*(i%5), yo + 31 + 25*(i/5))`.
pub const RECIPE_GRID_ORIGIN: (f32, f32) = (11.0, 31.0);
/// Grid step, both axes (`:65`).
pub const RECIPE_GRID_STEP: f32 = 25.0;
/// Grid columns (`i % 5`, `:65`).
pub const RECIPE_GRID_COLS: usize = 5;
/// Grid rows (`ITEMS_PER_PAGE / COLS`, `:65`, `RecipeBookPage.java`).
pub const RECIPE_GRID_ROWS: usize = 4;
/// `RecipeBookPage.ITEMS_PER_PAGE`.
pub const RECIPE_ITEMS_PER_PAGE: usize = RECIPE_GRID_COLS * RECIPE_GRID_ROWS;
/// `RecipeButton`'s own size (ctor `super(0, 0, 25, 25, ...)`,
/// `RecipeButton.java`).
pub const RECIPE_BUTTON_SIZE: f32 = 25.0;

/// Page-forward arrow — `ImageButton(xo + 93, yo + 137, 12, 17, ...)`.
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
    /// Whether the All/Craftable cycle-button is in its **Craftable** state,
    /// which is what picks [`RECIPE_SPRITE_FILTER_ENABLED`] over
    /// [`RECIPE_SPRITE_FILTER`] in the geometry below.
    ///
    /// Carried on the *layout* rather than passed to the geometry functions
    /// purely so this stayed a one-field addition instead of a fourth
    /// argument threaded through six public entry points and their callers.
    /// [`recipe_book_panel_layout_with_scale`] leaves it `false`; the app
    /// layer's `recipe_panel_layout` overwrites it from the panel's own state,
    /// which is the single place that knows the user's filter choice.
    pub filtering: bool,
    /// One entry per visible tab, in [`Self::tabs`] order — the **item icon**
    /// each category tab draws.
    ///
    /// Carried on the layout for [`Self::filtering`]'s reason: the geometry layer
    /// is given neither the [`RecipeBookType`](lodestone_model::RecipeBookType)
    /// nor the browsed category list, and threading both through six public
    /// entry points for a two-sprite draw is the churn that field's doc already
    /// declined once. [`recipe_tab_icons`] is the mapping;
    /// [`recipe_book_panel_layout_with_scale`] leaves this empty and the app
    /// layer's `recipe_panel_layout` fills it.
    ///
    /// Empty means "draw no tab icons", which is what a jar-less run and every
    /// existing geometry gate get.
    pub tab_icons: Vec<RecipeTabIcons>,
    /// The search box's current text, and whether it has focus — vanilla's
    /// `EditBox` value and `isFocused()`.
    ///
    /// Same carrying argument as [`Self::tab_icons`]. Focus matters to the draw
    /// because vanilla shows the greyed hint *only* when the value is empty **and**
    /// the box is unfocused, and draws a cursor when focused.
    pub search: String,
    /// See [`Self::search`].
    pub search_focused: bool,
    /// The zero-based page shown and the total page count, for the `x / y`
    /// readout between the two arrows.
    ///
    /// Same carrying argument as [`Self::tab_icons`] — the geometry layer is given
    /// no [`RecipeBookPanelContents`]. `(0, 1)` is the default and draws nothing,
    /// because vanilla only shows the readout at all when `totalPages > 1`.
    pub page: usize,
    /// See [`Self::page`].
    pub total_pages: usize,
}

/// The item icon(s) one recipe-book category tab draws — vanilla's
/// `RecipeBookComponent.TabInfo` `(primaryIcon, secondaryIcon)`.
#[derive(Debug, Clone, PartialEq)]
pub struct RecipeTabIcons {
    /// Always drawn.
    pub primary: ItemStack,
    /// Drawn beside the primary when present, which also **moves** the primary:
    /// see [`RECIPE_TAB_ICON_SOLO_X`].
    pub secondary: Option<ItemStack>,
}

/// Local x of a tab's icon when it is the only one —
/// `graphics.fakeItem(primaryIcon, getX() + 9 + moveLeft, getY() + 5)`.
const RECIPE_TAB_ICON_SOLO_X: f32 = 9.0;
/// Local x of the **first** of two icons — `getX() + 3 + moveLeft` (`:76`).
const RECIPE_TAB_ICON_PAIR_X: f32 = 3.0;
/// Local x of the **second** of two icons — `getX() + 14 + moveLeft` (`:77`).
const RECIPE_TAB_ICON_PAIR2_X: f32 = 14.0;
/// Local y of every tab icon — `getY() + 5` (`:76-79`).
const RECIPE_TAB_ICON_Y: f32 = 5.0;

/// A `minecraft:`-namespaced [`ItemStack`] of one, for the icon tables below.
///
/// Panics-free: the paths are compile-time literals from the jar, so
/// `Identifier::new` cannot fail on them, and an `unwrap_or_else` fallback would
/// be dead code pretending otherwise. `expect` names the offender if someone
/// mistypes one.
fn icon(path: &str) -> ItemStack {
    ItemStack::new(
        lodestone_model::Identifier::new("minecraft", path).expect("a literal item path"),
        1,
    )
}

/// The tab icons for one book type's visible `categories`, in the same order —
/// feed the result straight into [`RecipeBookPanelLayout::tab_icons`].
///
/// # Where these come from
///
/// Each `TabInfo` list is declared per *screen*, not per category, so the same
/// [`RecipeCategory`](lodestone_game::recipe::RecipeCategory) has a different
/// icon in different books — `Blocks` is `stone` in a furnace and
/// `redstone_ore` in a blast furnace, `Misc` is `lava_bucket + apple` at a
/// crafting table and `lava_bucket + emerald` in a furnace. That is exactly why
/// this takes the book type and not just the category; keying on the category
/// alone would put a porkchop on the blast furnace.
///
/// | book | declared in | tabs |
/// |---|---|---|
/// | Crafting | `CraftingRecipeBookComponent.java` | `bricks`, `redstone`, `iron_axe + golden_sword`, `lava_bucket + apple` |
/// | Furnace | `FurnaceScreen.java` | `porkchop`, `stone`, `lava_bucket + emerald` |
/// | BlastFurnace | `BlastFurnaceScreen.java` | `redstone_ore`, `iron_shovel + golden_leggings` |
/// | Smoker | `SmokerScreen.java` | `porkchop` |
///
/// Vanilla's leading `TabInfo(SearchRecipeBookCategory)` — the `compass` "all"
/// tab — has no counterpart here: this client models "all categories" as
/// `tab == None` with no tab widget of its own (see
/// [`RecipeBookPanelContents`]), so the compass is deliberately absent rather
/// than missing.
#[must_use]
pub fn recipe_tab_icons(
    book_type: lodestone_model::RecipeBookType,
    categories: &[lodestone_game::recipe::RecipeCategory],
) -> Vec<RecipeTabIcons> {
    use lodestone_game::recipe::RecipeCategory as C;
    use lodestone_model::RecipeBookType as B;
    let one = |p: &str| RecipeTabIcons { primary: icon(p), secondary: None };
    let two = |a: &str, b: &str| RecipeTabIcons {
        primary: icon(a),
        secondary: Some(icon(b)),
    };
    categories
        .iter()
        .map(|&c| match (book_type, c) {
            (B::Crafting, C::Building) => one("bricks"),
            (B::Crafting, C::Redstone) => one("redstone"),
            (B::Crafting, C::Equipment) => two("iron_axe", "golden_sword"),
            (B::Crafting, _) => two("lava_bucket", "apple"),
            (B::Furnace, C::Food) => one("porkchop"),
            (B::Furnace, C::Blocks) => one("stone"),
            (B::Furnace, _) => two("lava_bucket", "emerald"),
            (B::BlastFurnace, C::Blocks) => one("redstone_ore"),
            (B::BlastFurnace, _) => two("iron_shovel", "golden_leggings"),
            (B::Smoker, _) => one("porkchop"),
        })
        .collect()
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
        false,
    )
}

/// As [`recipe_book_panel_layout`], against an explicit `gui_scale` (`0` =
/// auto) — must be called with the same `gui_scale` the frame was last drawn
/// with, exactly like [`hit_test_with_scale`]'s own warning.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn recipe_book_panel_layout_with_scale(
    menu: &Menu,
    gui_scale: u32,
    width: u32,
    height: u32,
    tab_count: usize,
    has_prev_page: bool,
    has_next_page: bool,
    book_open: bool,
) -> RecipeBookPanelLayout {
    let main_layout = slot_layout(menu);
    let (mx, my) = panel_origin_with_scale(&main_layout, gui_scale, width, height);
    // The container panel's **shifted** x, which is what the toggle button hangs
    // off — vanilla's `updateScreenPosition`. Zero delta with the book closed, so
    // the toggle is where it always was until the panel actually moves.
    let (canvas_w, _) = crate::menu::render::logical_canvas(gui_scale, width, height);
    let mx = mx + super::layout::recipe_book_panel_shift(canvas_w, main_layout.width, book_open);
    // Vertically, vanilla's book and every screen this applies to share the
    // exact same `(canvas_h - 166) / 2` centring when both are 166 tall
    // (true for the crafting table and the whole furnace family — see
    // `slot_layout`'s own `height: 166.0` for both); the extra term below is
    // `0` for those two and keeps this correct if either ever isn't.
    let by = my + (main_layout.height - RECIPE_PANEL_H) * 0.5;
    // **Screen**-centred, then shifted left by `xOffset` — `getXOrigin()` is
    // `(this.width - 147) / 2 - this.xOffset`
    // and `xOffset` is `widthTooNarrow ? 0 : 86` (`:117`). It is *not* placed
    // relative to the container panel, which is what this used to do; see
    // `layout::recipe_book_panel_shift`'s own doc for why that could never fit.
    let x_offset = if super::layout::recipe_book_width_too_narrow(canvas_w) {
        0.0
    } else {
        super::layout::RECIPE_BOOK_X_OFFSET
    };
    let bx = ((canvas_w - RECIPE_PANEL_W) * 0.5).floor() - x_offset;

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
                // `xPosTab = xo - 30`, and
                // **no longer clamped**. The clamp existed because `bx` was
                // panel-relative and could be pushed to a 4 px floor, which put
                // every tab at `-26` and then stacked them all at `4` on top of
                // the page — the owner's "squished into the menu". With `bx`
                // screen-centred these fit by construction: at
                // `RECIPE_BOOK_MIN_WIDTH` the leftmost tab pixel is exactly `0`,
                // and below that width `x_offset` is `0`, which moves the tabs
                // *right* by 86. So the floor is unreachable, and keeping it
                // would only hide a future layout error.
                x: bx + RECIPE_TAB_X,
                y: by + RECIPE_TAB_Y0 + RECIPE_TAB_SPACING * i as f32,
                w: RECIPE_TAB_W,
                h: RECIPE_TAB_H,
            })
            .collect(),
        recipes: std::array::from_fn(|i| at(recipe_grid_cell_local(i))),
        page_forward: has_next_page.then(|| at(RECIPE_PAGE_FORWARD)),
        page_back: has_prev_page.then(|| at(RECIPE_PAGE_BACK)),
        // Geometry cannot know the filter state, the browsed categories or the
        // search text — see those fields' own docs. The app layer's
        // `recipe_panel_layout` fills all four.
        filtering: false,
        tab_icons: Vec::new(),
        search: String::new(),
        search_focused: false,
        page: 0,
        total_pages: 1,
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
    // `&|_| true` is "All", vanilla's un-filtered cycle state.
    recipe_book_panel_contents_filtered(book, book_type, tab, search, page, &|_| true)
}

/// [`recipe_book_panel_contents`] with vanilla's **Craftable** filter applied —
/// `RecipeBookComponent.java`'s `filtering` state, which hides every recipe the
/// player cannot currently make.
///
/// `craftable` is injected rather than computed here for the same reason the
/// contents query takes no viewport: this module has no [`Menu`] and no
/// inventory. The app layer builds the predicate out of
/// [`lodestone_game::menu::Menu::plan_recipe_auto_fill`] — the *same*
/// primitive the click path already uses to fill the grid, so the button can
/// never show a recipe that clicking it would then refuse to place.
///
/// The predicate runs over the whole browsed corpus, not just the visible
/// page, because pagination has to be computed from the filtered set — a
/// page count derived from the unfiltered corpus would leave empty pages the
/// user could still arrow into. Callers should pass `&|_| true` (or use
/// [`recipe_book_panel_contents`]) when not filtering, so the cost is only
/// paid in the state that asks for it.
#[must_use]
pub fn recipe_book_panel_contents_filtered<'a>(
    book: &'a RecipeBook,
    book_type: lodestone_model::RecipeBookType,
    tab: Option<usize>,
    search: &str,
    page: usize,
    craftable: &dyn Fn(&lodestone_model::Identifier) -> bool,
) -> RecipeBookPanelContents<'a> {
    let tabs = book.visible_tabs(book_type);
    let category = tab.and_then(|i| tabs.get(i).copied());
    let all_ids: Vec<&'a lodestone_model::Identifier> = book
        .browse(book_type, category, search)
        .into_iter()
        .filter(|id| craftable(id))
        .collect::<Vec<_>>();
    let total_pages = all_ids.len().div_ceil(RECIPE_ITEMS_PER_PAGE).max(1);
    let page = page.min(total_pages - 1);
    let start = page * RECIPE_ITEMS_PER_PAGE;
    let page_ids = all_ids.iter().skip(start).take(RECIPE_ITEMS_PER_PAGE).copied().collect();
    RecipeBookPanelContents { tabs, all_ids, page_ids, total_pages, page }
}

// ---------------------------------------------------------------------------
// The real 26.2 recipe-book art
// ---------------------------------------------------------------------------

/// Vanilla's own sprite ids for the recipe book, from the decompiled 26.2
/// client. Every one of these except [`RECIPE_SPRITE_PANEL`] is a real
/// `gui/sprites/recipe_book/**` entry and is therefore **already** in
/// [`GuiAtlas`](lodestone_render::GuiAtlas)'s enumeration — it stitches every
/// `assets/<ns>/textures/gui/sprites/**.png` in the pack, so wiring the book's
/// art needed no new atlas, no new pipeline and no new bind group. This module
/// only had to name the ids and say where they go.
///
/// The panel sheet is the exception: `RecipeBookComponent.java` declares it
/// as a raw texture path and `:305` blits a sub-rect of it, so it is registered
/// as a loose extra — see
/// [`crate::resources::RECIPE_BOOK_TEXTURES`].
pub const RECIPE_SPRITE_PANEL: &str = crate::resources::RECIPE_BOOK_PANEL_SPRITE;
/// The toggle button — `RecipeBookComponent.RECIPE_BUTTON_SPRITES`
///, 20×18.
pub const RECIPE_SPRITE_BUTTON: &str = "recipe_book/button";
/// An unselected category tab — `RecipeBookTabButton.SPRITES`
///, 35×27.
pub const RECIPE_SPRITE_TAB: &str = "recipe_book/tab";
/// A selected category tab. Note `RecipeBookTabButton.java` reads
/// `sprites.get(true, this.selected)` — the second argument is **selected**,
/// not hovered, so this is the selected art and hover has none of its own.
pub const RECIPE_SPRITE_TAB_SELECTED: &str = "recipe_book/tab_selected";
/// The filter cycle-button in its **not-filtering** state, 26×16 —
/// `CraftingRecipeBookComponent.java`. `filter_disabled` is the "All"
/// state (`getFilterButtonTextures().get(filtering, hovered)` with
/// `filtering == false`, `RecipeBookComponent.java`).
///
/// Both states are now real: [`RECIPE_SPRITE_FILTER_ENABLED`] is the other
/// half, picked by [`RecipeBookPanelLayout::filtering`]. This doc used to say
/// the disabled art was "the only state this client has" — true when written
/// and stale since the filter became modelled (that fix's
/// `SessionRecipeBookSettings` island).
pub const RECIPE_SPRITE_FILTER: &str = "recipe_book/filter_disabled";
/// The same cycle-button in its **Craftable** state —
/// `getFilterButtonTextures().get(true, false)`, `RecipeBookComponent.java`.
/// A distinct `gui/sprites/recipe_book/**` entry, so it is already stitched
/// into [`GuiAtlas`](lodestone_render::GuiAtlas) exactly like its sibling and
/// needed no new atlas entry.
pub const RECIPE_SPRITE_FILTER_ENABLED: &str = "recipe_book/filter_enabled";
/// The furnace family's own filter art — `FurnaceRecipeBookComponent.java`.
/// A genuinely different sheet, not a tint of the crafting one.
pub const RECIPE_SPRITE_FILTER_FURNACE: &str = "recipe_book/furnace_filter_disabled";
/// The page-forward arrow, 12×17 — `RecipeBookPage.java`.
pub const RECIPE_SPRITE_PAGE_FORWARD: &str = "recipe_book/page_forward";
/// The page-back arrow, 12×17 — `RecipeBookPage.java`. Note vanilla's
/// file is spelled `page_backward`, not `page_back`.
pub const RECIPE_SPRITE_PAGE_BACK: &str = "recipe_book/page_backward";
/// A populated recipe cell's frame, 25×25 — `RecipeButton.java`.
///
/// Vanilla picks between four of these from `StackedItemContents`
/// (craftable/uncraftable × single/many). Craftability is not modelled here, so
/// the *craftable* frame is used unconditionally: this panel browses the whole
/// corpus rather than only what the inventory can make, so drawing everything
/// greyed-out would be the more misleading of the two.
pub const RECIPE_SPRITE_SLOT: &str = "recipe_book/slot_craftable";

/// The `147×166` window `RecipeBookComponent.java` samples out of the
/// `256×256` panel sheet: `blit(..., xo, yo, 1.0F, 1.0F, 147, 166, 256, 256)`,
/// i.e. `u = v = 1`. The one-pixel inset is real — the sheet's opaque region is
/// exactly `x 1..147, y 1..166`, verified by decoding the PNG, so sampling from
/// `(0, 0)` would shift every pixel of the page by one and pull in the
/// transparent border.
pub const RECIPE_PANEL_SRC: [f32; 4] = [1.0, 1.0, RECIPE_PANEL_W, RECIPE_PANEL_H];

/// The declared (16x-baseline) sheet size [`RECIPE_PANEL_SRC`]'s coordinates
/// are authored against — the trailing `256, 256` of `RecipeBookComponent`'s
/// `blit(..., 147, 166, 256, 256)`. Not yet consumed: the panel is currently
/// drawn through
/// [`GuiAtlas::subregion_quad`](lodestone_render::GuiAtlas::subregion_quad),
/// which treats [`RECIPE_PANEL_SRC`] as real sprite pixels rather than
/// rescaling it against this declared size, so a resource pack whose panel
/// sheet exceeds 256×256 real pixels currently draws the recipe book panel
/// magnified — issue #582. The fix is
/// [`GuiAtlas::subregion_quad_declared`](lodestone_render::GuiAtlas::subregion_quad_declared)`(id, RECIPE_PANEL_DECLARED, src, dst)`
/// at the draw site in `crate::hud`.
pub const RECIPE_PANEL_DECLARED: (f32, f32) = (256.0, 256.0);

/// Vanilla's 2 px leftward nudge on the **selected** tab's blit —
/// `RecipeBookTabButton.java`. It shifts only the drawn art; the widget's
/// own rect (and so its hit region) does not move, which is why
/// [`RecipeBookPanelLayout::tabs`] is unaffected and this offset lives here
/// rather than in the layout.
const RECIPE_TAB_SELECTED_NUDGE: f32 = 2.0;

/// One textured quad request: a sprite id, where it goes, and optionally which
/// sub-rect of it to sample.
///
/// **Ids and rects only — deliberately no UVs and no atlas.** The producer
/// ([`recipe_book_panel_geometry`]) runs with no GPU and no
/// [`GuiAtlas`](lodestone_render::GuiAtlas) in scope, and resolving UVs at build
/// time would have meant threading an atlas through every caller. The renderer
/// resolves each of these against whatever atlas it has bound, and skips any id
/// the pack does not carry, so a resource pack missing one sprite loses that
/// sprite rather than the panel.
///
/// The order of the list **is** the draw order, for the same reason the colour
/// stream's split matters: this GUI path has no depth compare. The panel body
/// must come first because it is opaque and would otherwise erase the widgets
/// on top of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecipeBookSprite {
    /// The [`GuiAtlas`](lodestone_render::GuiAtlas) sprite id.
    pub id: &'static str,
    /// Destination `[x, y, w, h]` in logical GUI pixels.
    pub dst: [f32; 4],
    /// Sub-rect `[x, y, w, h]` in the sprite's own native pixels, or `None` to
    /// draw the whole sprite at its native size. Only the panel sheet needs
    /// one — see [`RECIPE_PANEL_SRC`].
    pub src: Option<[f32; 4]>,
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
    /// How many leading vertices of [`verts`](Self::verts) are the panel's own
    /// **chrome** — the panel body, search box, filter button, tabs, the 20
    /// slot wells and the page arrows. Everything from here on belongs to a
    /// recipe result's *icon*: its stack-count digits, its durability bar and
    /// (on a jar-less run) its fallback swatch.
    ///
    /// A caller **must** draw the two ranges in separate passes with the
    /// sprite and model passes sandwiched between them, exactly as
    /// [`ContainerGeometry::chrome_vertex_count`] requires. This is not
    /// bookkeeping for its own sake — it is the whole fix for an owner-reported
    /// bug. The stream used to be unsplit, so a recipe result's count digits
    /// (emitted into this same colour stream by
    /// [`Builder::draw_stack`](super::builder::Builder::draw_stack)) were
    /// submitted **before** the 3-D block models and flat sprites and
    /// disappeared underneath them.
    ///
    /// The GUI path here has no meaningful depth compare, so **submission
    /// order alone decides z** and there is nothing else that could fix this:
    /// a caller that draws all of `verts` in one pass reproduces the bug
    /// exactly, whichever end it draws it from.
    pub chrome_vertex_count: usize,
    /// Vanilla's **real** recipe-book art for this frame, in draw order — see
    /// [`RecipeBookSprite`].
    ///
    /// The renderer draws these *after* the `verts[..chrome]` flat fills and
    /// *before* the item passes, so on a run with the GUI atlas attached the
    /// real art covers the flat fills entirely (the panel sheet is opaque) and
    /// on a jar-less run the flat fills are all there is. That is why the
    /// fallback palette below is unchanged rather than deleted: it is still the
    /// whole headless picture, and every existing geometry gate still measures
    /// it.
    ///
    /// This list is what made the panel stop being "completely incorrectly
    /// textured": it had none of this art and drew flat dark rectangles, while
    /// vanilla's page is an opaque **white** sheet — near-inverted, which is why
    /// the report said *completely* rather than *slightly off*.
    pub sprites: Vec<RecipeBookSprite>,
}

impl RecipeBookPanelGeometry {
    /// Number of coloured vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.verts.len() / FLOATS_PER_VERTEX
    }
}

/// The page readout's own anchor — `xo - pWidth / 2 + 73`, `yo + 141`
///. The `73` is measured from the book page's local
/// origin and the `- pWidth / 2` centres the string on it, which puts it midway
/// between the back arrow (local x `38`) and the forward arrow (local x `93`).
const PAGE_TEXT_CENTRE_X: f32 = 73.0;
/// See [`PAGE_TEXT_CENTRE_X`].
const PAGE_TEXT_Y: f32 = 141.0;
/// The readout's colour — `graphics.text(..., -1)`,
/// i.e. opaque white.
const PAGE_TEXT_COLOUR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// `EditBox`'s bordered text inset — `textX = getX() + 4`.
const SEARCH_TEXT_INSET: f32 = 4.0;
/// The glyph height `EditBox` centres its text against — the literal `8` in
/// `textY = getY() + (height - 8) / 2`. Note this is `8`,
/// not the `9` line *pitch* the caret height uses, and the two are different
/// numbers in the same expression pair in vanilla too.
const SEARCH_GLYPH_H: f32 = 8.0;
/// `EditBox.setTextColor(-1)` — opaque white.
const SEARCH_TEXT_COLOUR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// `EditBox.SEARCH_HINT_STYLE`'s `ChatFormatting.GRAY`,
/// which is `0xAAAAAA`.
const SEARCH_HINT_COLOUR: [f32; 4] = [0.666_666_7, 0.666_666_7, 0.666_666_7, 1.0];
/// `RecipeBookComponent.SEARCH_HINT`'s `gui.recipebook.search_hint`, whose
/// `en_us` value is `"Search..."`.
const SEARCH_HINT: &str = "Search...";

/// Vanilla's own icon inset within a `RecipeButton` — `offset = 4`
/// (`RecipeButton.java`, the non-multi-recipe branch — the "stack two
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
        None,
        // No font on this path, so no tooltip could be drawn even if a cursor
        // were supplied — see `RecipeTooltipContext`.
        RecipeTooltipContext::default(),
    )
}

/// What the panel needs in order to draw a **hover tooltip** over the recipe
/// button under the pointer.
///
/// Vanilla really does show one: `RecipeBookComponent.extractTooltip` forwards to
/// `RecipeBookPage.extractTooltip`, which — while a screen is up and the
/// ghost-recipe overlay is not visible — sets a component tooltip built by
/// `RecipeButton.getTooltipText` for the hovered button. That method is
/// `Screen.getTooltipFromItem(displayStack)`, i.e. exactly the lines an inventory
/// slot holding the same stack would show, which is why this reuses
/// [`super::tooltip::emit_tooltip_for_stack`] rather than growing a second
/// tooltip builder.
///
/// `cursor` is **physical viewport pixels** — the same space
/// [`recipe_book_panel_hit_test_with_scale`] and the container's own hit test
/// take, and the space `emit_tooltip_for_stack` divides down internally. `None`
/// is "no pointer this frame", which draws no tooltip.
///
/// # What vanilla adds and this deliberately does not
///
/// `getTooltipText` appends `gui.recipebook.moreRecipes` ("Right Click for More")
/// **only when `hasMultipleRecipes()`**, which is `selectedEntries.size() > 1` on
/// the button's `RecipeCollection`. This client has no collection grouping:
/// [`lodestone_game::recipe::RecipeBook::browse`] hands back one recipe id per
/// button, so every button we draw carries exactly one recipe and vanilla's
/// predicate is false for all of them. Emitting the line anyway would be
/// fabricating a right-click affordance that does not exist here. Whoever lands
/// grouping-by-result-display is the right person to add it, and this is the
/// place.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RecipeTooltipContext {
    /// Pointer position in physical viewport pixels, or `None` for no pointer.
    pub cursor: Option<[f32; 2]>,
    /// Vanilla's persisted `advancedItemTooltips` (F3+H), passed straight
    /// through to the shared line builder.
    pub advanced: bool,
}

/// As [`recipe_book_panel_geometry`], drawing **real item icons** from the
/// atlases — the recipe-grid analogue of
/// [`ContainerGeometry::build`](super::geometry::ContainerGeometry::build).
///
/// `font` is the only thing that can draw the search box's *text*; with `None`
/// the box is chrome and nothing else, which is the jar-less picture (there is no
/// vanilla font to draw with there either). It also gates the hover tooltip, for
/// the same reason — see [`RecipeTooltipContext`].
#[must_use]
#[allow(clippy::too_many_arguments)]
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
    font: Option<&crate::hud::VanillaFont>,
    tooltip: RecipeTooltipContext,
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
        font,
        tooltip,
    )
}

/// The one body both public entry points share.
///
/// `pub(super)` so `super::tests` can drive it with **no atlas but a real font** —
/// the only combination that exercises the hover tooltip (which needs a font to
/// measure against) without needing a stitched [`ItemAtlas`]. That is the same
/// arrangement `ContainerGeometry::build_inner`'s own tooltip gate uses.
#[allow(clippy::too_many_arguments)]
pub(super) fn recipe_book_panel_geometry_inner(
    layout: &RecipeBookPanelLayout,
    open: bool,
    selected_tab: Option<usize>,
    page_results: &[&ItemStack],
    gui_scale: u32,
    width: u32,
    height: u32,
    assets: &IconAssets<'_>,
    font: Option<&crate::hud::VanillaFont>,
    tooltip: RecipeTooltipContext,
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
    let mut b = Builder::new(w, h, font);
    b.rect_px(layout.toggle.x, layout.toggle.y, layout.toggle.w, layout.toggle.h, TOGGLE_COLOUR);
    // The real art, in draw order. Built alongside the flat fills rather than
    // instead of them: the fills stay as the jar-less picture and the renderer
    // draws these over the top when an atlas is bound (the panel sheet is fully
    // opaque, so it hides them completely).
    let mut sprites: Vec<RecipeBookSprite> = Vec::new();
    let whole = |id: &'static str, r: Rect| RecipeBookSprite {
        id,
        dst: [r.x, r.y, r.w, r.h],
        src: None,
    };
    if !open {
        // A closed panel is chrome and nothing else, so the split point is the
        // end of the stream — the caller's second colour range is empty and its
        // icon passes draw nothing.
        sprites.push(whole(RECIPE_SPRITE_BUTTON, layout.toggle));
        let chrome_vertex_count = b.verts.len() / FLOATS_PER_VERTEX;
        return RecipeBookPanelGeometry {
            verts: b.verts,
            item_verts: b.item_verts,
            model_verts: b.model_verts,
            special: b.special,
            chrome_vertex_count,
            sprites,
        };
    }

    // The panel page first, and opaque: anything emitted before it would be
    // erased, and anything that must sit on the page has to come after.
    sprites.push(RecipeBookSprite {
        id: RECIPE_SPRITE_PANEL,
        dst: [layout.panel.x, layout.panel.y, layout.panel.w, layout.panel.h],
        src: Some(RECIPE_PANEL_SRC),
    });
    // The search box has **no sprite of its own** — vanilla's is a plain
    // `EditBox` over the well that is already
    // painted into the panel sheet, and the magnifier glyph beside it is baked
    // into the sheet too. So there is deliberately nothing to emit here; the
    // flat `SEARCH_BOX_COLOUR` rect underneath is jar-less-only.
    //
    // The filter button always uses the **crafting** art, never
    // [`RECIPE_SPRITE_FILTER_FURNACE`]: this function is not given the `Menu`,
    // and threading one in means changing a caller that is not this module's to
    // change. A bounded, documented deviation — the two sheets differ only in
    // the glyph inside the button.
    //
    // The All/Craftable state, however, *is* modelled now — vanilla's
    // `getFilterButtonTextures().get(filtering, hovered)`
    //, with `hovered` still unmodelled.
    sprites.push(whole(
        if layout.filtering { RECIPE_SPRITE_FILTER_ENABLED } else { RECIPE_SPRITE_FILTER },
        layout.filter_button,
    ));

    for (i, r) in layout.tabs.iter().enumerate() {
        let selected = selected_tab == Some(i);
        let id = if selected { RECIPE_SPRITE_TAB_SELECTED } else { RECIPE_SPRITE_TAB };
        // `RecipeBookTabButton.java` nudges the *blit* of a selected tab
        // 2 px left while leaving the widget's rect alone, so the drawn art and
        // the hit region legitimately disagree by 2 px — vanilla's own
        // behaviour, not a transcription slip.
        let x = if selected { r.x - RECIPE_TAB_SELECTED_NUDGE } else { r.x };
        sprites.push(RecipeBookSprite { id, dst: [x, r.y, r.w, r.h], src: None });
    }

    if let Some(r) = layout.page_forward {
        sprites.push(whole(RECIPE_SPRITE_PAGE_FORWARD, r));
    }
    if let Some(r) = layout.page_back {
        sprites.push(whole(RECIPE_SPRITE_PAGE_BACK, r));
    }

    // A slot frame for **populated cells only** — vanilla hides an unused
    // `RecipeButton` outright (`RecipeBookPage.java`'s own visibility pass), and
    // an empty cell therefore shows the bare page. Verified by decoding
    // `recipe_book.png`: the whole grid region of the sheet is uniform opaque
    // white with no slot frames baked in, so emitting all 20 would draw a grid
    // vanilla does not have.
    for (i, r) in layout.recipes.iter().enumerate() {
        if page_results.get(i).is_some() {
            sprites.push(whole(RECIPE_SPRITE_SLOT, *r));
        }
    }

    // The toggle **last**: it is a screen widget rather than part of the book
    // component (see `RecipeBookPanelHit::Toggle`), it lives on the main
    // container panel's chrome, and at narrow canvases the book panel is
    // clamped so that it may overlap the main panel's left edge. Emitting it
    // before the page would let the page bury a live control — the "a dead
    // control is worse than a missing one" rule this module already applies to
    // the tab-x clamp.
    sprites.push(whole(RECIPE_SPRITE_BUTTON, layout.toggle));

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

    // Every slot **well**, before any icon. This loop used to be one pass that
    // interleaved `rect_px` and `draw_stack` per cell, which is what made the
    // stream unsplittable and put every count digit under its own icon — see
    // `RecipeBookPanelGeometry::chrome_vertex_count`. Wells are chrome; the
    // icons that sit on them are drawn in the second loop below.
    for r in &layout.recipes {
        b.rect_px(r.x, r.y, r.w, r.h, RECIPE_SLOT_COLOUR);
    }

    // Still chrome, and deliberately still *before* the split: the page arrows
    // are widgets, not icon overlays, so they must not end up in the range the
    // caller draws over the item passes.
    if let Some(r) = layout.page_forward {
        b.rect_px(r.x, r.y, r.w, r.h, BUTTON_COLOUR);
    }
    if let Some(r) = layout.page_back {
        b.rect_px(r.x, r.y, r.w, r.h, BUTTON_COLOUR);
    }

    // ---- the chrome/icon split point ----
    let chrome_vertex_count = b.verts.len() / FLOATS_PER_VERTEX;

    // Now the icons. Each `draw_stack` appends to the sprite/model streams and
    // to the *tail* of the colour stream (count digits, durability bar, or the
    // whole fallback swatch on a jar-less run), which the caller draws in a
    // pass after both item passes.
    for (i, r) in layout.recipes.iter().enumerate() {
        if let Some(stack) = page_results.get(i) {
            b.draw_stack(assets, stack, r.x + RECIPE_ICON_INSET, r.y + RECIPE_ICON_INSET);
        }
    }

    // The category tabs' own item icons — `RecipeBookTabButton.extractIcon`
    //, which the panel had none of: the tabs
    // drew their sprite and nothing on it, so every category slot was blank.
    //
    // In the icon half of the stream on purpose. `extractIcon` is called *after*
    // the tab's own `blitSprite` (`:60-61`), and this path's tab sprite is in
    // `sprites` — which the caller draws between the two colour ranges — so an
    // icon emitted before the split would be buried by its own tab.
    //
    // `moveLeft` is vanilla's: the selected tab's art shifts 2 px left
    // (`RECIPE_TAB_SELECTED_NUDGE`) and its icon goes with it, while the widget
    // rect does not move.
    for (i, r) in layout.tabs.iter().enumerate() {
        let Some(icons) = layout.tab_icons.get(i) else {
            continue;
        };
        let move_left = if selected_tab == Some(i) { -RECIPE_TAB_SELECTED_NUDGE } else { 0.0 };
        let y = r.y + RECIPE_TAB_ICON_Y;
        match &icons.secondary {
            Some(second) => {
                b.draw_stack(assets, &icons.primary, r.x + RECIPE_TAB_ICON_PAIR_X + move_left, y);
                b.draw_stack(assets, second, r.x + RECIPE_TAB_ICON_PAIR2_X + move_left, y);
            }
            None => {
                b.draw_stack(assets, &icons.primary, r.x + RECIPE_TAB_ICON_SOLO_X + move_left, y);
            }
        }
    }

    // The search box's text. Vanilla's is a plain `EditBox`, so there is no
    // sprite for it (the well is baked into the panel sheet) and the *text* was
    // the whole widget — which is why the box read as "completely missing" even
    // though the state behind it (`RecipePanelState::search`) was already live
    // and already edited by typing.
    //
    // `EditBox.renderWidget`: `textX = getX() + 4`
    // (bordered), `textY = getY() + (height - 8) / 2`. With this box's declared
    // `9 + 5` height that is `y + 3`.
    //
    // The hint is drawn **only** when the value is empty *and* the box is
    // unfocused (`:438`), in `SEARCH_HINT_STYLE`'s grey; a focused empty box
    // shows the cursor instead, which is how a player can tell typing will land
    // here. Italic is not modelled — this font has no italic variant, and a
    // fabricated slant would be worse than upright grey.
    // The `x / y` readout between the two arrows — `RecipeBookPage.java`,
    // which the panel had nothing for at all ("the page numbers are missing in
    // between the arrows"). Gated on `total_pages > 1` exactly as vanilla is, so a
    // single-page result shows bare page rather than a pointless "1 / 1".
    //
    // In the icon half of the stream for the same reason the tab icons are: the
    // page sheet is a *sprite*, drawn between the caller's two colour ranges, so
    // anything in the chrome half would be painted over by the page it sits on.
    if let Some(f) = font.filter(|_| layout.total_pages > 1) {
        // `gui.recipebook.page` is `"%s / %s"` in `en_us.json`, and `currentPage`
        // is zero-based on the wire but one-based on screen (`currentPage + 1`).
        let text = format!("{} / {}", layout.page + 1, layout.total_pages);
        let tw = f.width(&text, 1.0);
        b.shadowed_label(
            &text,
            layout.panel.x + PAGE_TEXT_CENTRE_X - (tw * 0.5).floor(),
            layout.panel.y + PAGE_TEXT_Y,
            1.0,
            PAGE_TEXT_COLOUR,
        );
    }

    if font.is_some() {
        let tx = layout.search_box.x + SEARCH_TEXT_INSET;
        let ty = layout.search_box.y + ((layout.search_box.h - SEARCH_GLYPH_H) * 0.5).floor();
        if layout.search.is_empty() && !layout.search_focused {
            b.shadowed_label(SEARCH_HINT, tx, ty, 1.0, SEARCH_HINT_COLOUR);
        } else {
            b.shadowed_label(&layout.search, tx, ty, 1.0, SEARCH_TEXT_COLOUR);
            if layout.search_focused {
                // `TextCursorUtils.extractInsertCursor(graphics, cursorX, textY,
                // color, 9 + 1)` — a 1 px caret one glyph
                // line tall, at the end of the value because this client has no
                // cursor position within it.
                let cx = tx + b.font.map_or(0.0, |f| f.width(&layout.search, 1.0));
                b.rect_px(cx, ty - 1.0, 1.0, SEARCH_GLYPH_H + 2.0, SEARCH_TEXT_COLOUR);
            }
        }
    }

    // The hovered recipe button's tooltip — `RecipeBookPage.extractTooltip`.
    //
    // **Last of all, deliberately.** This appends to the tail of the colour
    // stream, which the caller draws after both item passes, so the tooltip sits
    // over the icons rather than under them. It is the same argument
    // `chrome_vertex_count` exists for, and the same one `super::tooltip`'s own
    // module doc makes about the container.
    //
    // The hovered button is resolved through `recipe_book_panel_hit_test_with_scale`
    // rather than a second walk of `layout.recipes`: the *click* path already
    // goes through that function, so a tooltip that appeared over a cell the
    // click would not resolve to is impossible by construction.
    if let Some(cursor) = tooltip.cursor
        && let Some(RecipeBookPanelHit::Recipe(i)) = recipe_book_panel_hit_test_with_scale(
            layout, open, gui_scale, width, height, cursor[0], cursor[1],
        )
        && let Some(stack) = page_results.get(i)
    {
        super::tooltip::emit_tooltip_for_stack(
            &mut b,
            assets,
            stack,
            Some(cursor),
            tooltip.advanced,
            gui_scale,
            width,
            height,
            (w, h),
            // No scroll-selection tracking on this screen — a bundle in the
            // recipe book preview always draws its grid with nothing
            // highlighted, matching `BundleContents::NO_SELECTED_ITEM_INDEX`.
            None,
        );
    }

    RecipeBookPanelGeometry {
        verts: b.verts,
        item_verts: b.item_verts,
        model_verts: b.model_verts,
        special: b.special,
        chrome_vertex_count,
        sprites,
    }
}

/// Flat-fill colours for the panel's chrome, in the same muted palette family
/// the rest of this module's atlas-less fallback already uses (see
/// `build_inner`'s own `[0.08, 0.075, 0.065, 0.88]` panel fill).
///
/// **These are the jar-less fallback only.** This comment used to say the module
/// did not load vanilla's real `recipe_book.png`/`recipe_book/*` art — it now
/// does, via [`RecipeBookPanelGeometry::sprites`], and the renderer draws that
/// art over these fills. Keep them anyway: they are the whole picture on a
/// headless/jar-less run and every existing geometry gate in this module
/// measures them. Note the real page is opaque **white** where these are
/// near-black, so with an atlas bound none of this is visible.
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
/// See [`PANEL_COLOUR`]. `pub(super)` so the ordering gate can identify a slot
/// **well** vertex by the same constant the draw fills it with, rather than
/// restating the literal — a gate that restates it stops measuring the draw the
/// moment someone retunes the palette.
pub(super) const RECIPE_SLOT_COLOUR: [f32; 4] = [0.16, 0.14, 0.12, 1.0];
