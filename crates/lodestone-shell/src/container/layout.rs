//! Slot geometry: the rectangles a [`Menu`] projects onto, and the hit test
//! that resolves a viewport pixel back to a menu-slot index.
//!
//! Split out of `container.rs` verbatim; see that module's own doc comment for
//! why the layout dispatches the way it does.

use lodestone_game::menu::{CraftLayout, Menu, MenuKind, SpecialLayout};

use super::{CELL, SLOT};

/// A pixel-space rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge in pixels.
    pub x: f32,
    /// Top edge in pixels.
    pub y: f32,
    /// Width in pixels.
    pub w: f32,
    /// Height in pixels.
    pub h: f32,
}

/// One laid-out menu slot, in local widget coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotRect {
    /// Menu-slot index.
    pub menu_index: usize,
    /// Left edge in local widget pixels.
    pub x: f32,
    /// Top edge in local widget pixels.
    pub y: f32,
    /// Width in pixels.
    pub w: f32,
    /// Height in pixels.
    pub h: f32,
}

/// Complete local layout for a menu.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotLayout {
    /// Widget width in pixels.
    pub width: f32,
    /// Widget height in pixels.
    pub height: f32,
    /// Slot rectangles in menu-slot order.
    pub slots: Vec<SlotRect>,
}

/// Computes the slot layout in local widget coordinates.
///
/// [`Menu::special_layout`] is checked first: the anvil,
/// grindstone, smithing table and enchanting table are all mechanically a
/// plain [`MenuKind::Generic`] (see `lodestone_game::menu::Menu::item_combiner`'s
/// doc comment) whose real screen is *not* [`generic_layout`]'s left-to-right
/// grid — that grid would put three-in-a-row input slots at `y = 18` instead
/// of vanilla's spread-out, screen-specific placement. Checked before the
/// `MenuKind` match for the same reason [`Menu::craft_layout`] already is:
/// both are extra routing carried *on* [`Menu`] rather than grown into
/// `MenuKind`, which is matched exhaustively here and stays that way.
///
/// [`special_layout_positions`] has the per-screen coordinates, cited against
/// the decompile. Both this function and [`crate::hit_test`] (menu clicks)
/// call `slot_layout`, so a `special_layout` change is visible to drawing
/// *and* correctly hit-tested by construction — no second place to keep in
/// sync, which is exactly what put [`SpecialLayout`] on [`Menu`] instead of
/// requiring `menu_type` threaded into `hit_test`'s callers (this module's own
/// docs warn that mismatch reads as "clicks land one slot off" and is
/// invisible in any screenshot).
#[must_use]
pub fn slot_layout(menu: &Menu) -> SlotLayout {
    if let Some(layout) = special_layout_positions(menu) {
        return layout;
    }
    match menu.kind() {
        MenuKind::Player => player_layout(),
        MenuKind::Generic { container_size } => match menu.craft_layout() {
            Some(craft) => crafting_layout(craft, container_size),
            None => generic_layout(container_size),
        },
    }
}

/// Vanilla's real slot layout for the menus that have a
/// [`Menu::special_layout`]. Positions are vanilla's slot constructor
/// arguments, re-read from the decompile rather than taken from a summary:
///
/// | [`SpecialLayout`] | slots (menu index @ x,y) | source |
/// |---|---|---|
/// | `Anvil` | `0@27,47` `1@76,47` `2@134,47` | `AnvilMenu.java:42-45,58-60` |
/// | `Grindstone` | `0@49,19` `1@49,40` `2@129,34` | `GrindstoneMenu.java:48-60` |
/// | `Smithing` | `0@8,48` `1@26,48` `2@44,48` `3@98,48` | `SmithingMenu.java:25-29,58-61` |
/// | `Enchanting` | `0@15,47` `1@35,47` | `EnchantmentMenu.java:55-61` |
/// | `Furnace`/`BlastFurnace`/`Smoker` | `0@56,17` `1@56,53` `2@116,35` | `AbstractFurnaceMenu.java:63-65` |
/// | `Brewing` | `0@56,51` `1@79,58` `2@102,51` `3@79,17` `4@17,17` | `BrewingStandMenu.java:48-52` |
/// | `Loom` | `0@13,26` `1@33,26` `2@23,45` `3@143,57` | `LoomMenu.java:64-82` |
/// | `Stonecutter` | `0@20,33` `1@143,33` | `StonecutterMenu.java:54-55` |
/// | `Cartography` | `0@15,15` `1@15,52` `2@145,39` | `CartographyTableMenu.java:49-61` |
/// | `Dispenser` | `0..9` a 3×3 grid from `62,17`, step `18` | `DispenserMenu.java:26,30-37` |
/// | `Hopper` | `0..5` a row from `44,20`, step `18` | `HopperMenu.java:24` |
///
/// Every one of these calls vanilla's standard `addStandardInventorySlots`
/// for the player section (`ItemCombinerMenu.java:48`,
/// `EnchantmentMenu.java:72`, and — re-read individually rather than assumed
/// to match — `AbstractFurnaceMenu.java:66`, `BrewingStandMenu.java:54`,
/// `LoomMenu.java:106`, `StonecutterMenu.java:84`,
/// `CartographyTableMenu.java:89`, `DispenserMenu.java:27`) with a **fixed**
/// `main_y`, not derived from the top section the way
/// [`generic_layout`]/[`crafting_layout`] compute it from their own row
/// count, because none of these screens' top sections stack rows the way a
/// chest or crafting grid does. `main_y` is `84` for every one of them
/// **except** the hopper, whose own call is `addStandardInventorySlots(inventory,
/// 8, 51)` (`HopperMenu.java:27`) — its panel is genuinely shorter
/// (`imageHeight = 133`, not `166`), so restating `84` here would silently
/// overlap the hopper's own five slots with the top of the main inventory.
///
/// `None` for a menu with no [`Menu::special_layout`], or (defensively) if the
/// menu's `container_size` does not match what the real screen has — the same
/// guard [`lodestone_game::menus::build_menu`] itself takes before ever
/// setting a `special_layout`, so this should be unreachable in practice, but
/// a mismatch here is safer falling back to the plain grid than drawing a
/// panel sized for the wrong content.
#[must_use]
fn special_layout_positions(menu: &Menu) -> Option<SlotLayout> {
    let Some(special) = menu.special_layout() else {
        return None;
    };
    let MenuKind::Generic { container_size } = menu.kind() else {
        return None;
    };
    let mut slots = Vec::new();
    match (special, container_size) {
        (SpecialLayout::Anvil, 3) => {
            slots.push(slot(0, 27.0, 47.0));
            slots.push(slot(1, 76.0, 47.0));
            slots.push(slot(2, 134.0, 47.0));
        }
        (SpecialLayout::Grindstone, 3) => {
            slots.push(slot(0, 49.0, 19.0));
            slots.push(slot(1, 49.0, 40.0));
            slots.push(slot(2, 129.0, 34.0));
        }
        (SpecialLayout::Smithing, 4) => {
            slots.push(slot(0, 8.0, 48.0));
            slots.push(slot(1, 26.0, 48.0));
            slots.push(slot(2, 44.0, 48.0));
            slots.push(slot(3, 98.0, 48.0));
        }
        (SpecialLayout::Enchanting, 2) => {
            slots.push(slot(0, 15.0, 47.0));
            slots.push(slot(1, 35.0, 47.0));
        }
        // All three furnace-family menus share these coordinates
        // (`AbstractFurnaceMenu` is the common constructor); only the
        // background art differs, which `background_kind` selects on.
        (SpecialLayout::Furnace | SpecialLayout::BlastFurnace | SpecialLayout::Smoker, 3) => {
            slots.push(slot(0, 56.0, 17.0));
            slots.push(slot(1, 56.0, 53.0));
            slots.push(slot(2, 116.0, 35.0));
        }
        (SpecialLayout::Brewing, 5) => {
            slots.push(slot(0, 56.0, 51.0));
            slots.push(slot(1, 79.0, 58.0));
            slots.push(slot(2, 102.0, 51.0));
            slots.push(slot(3, 79.0, 17.0));
            slots.push(slot(4, 17.0, 17.0));
        }
        (SpecialLayout::Loom, 4) => {
            slots.push(slot(0, 13.0, 26.0));
            slots.push(slot(1, 33.0, 26.0));
            slots.push(slot(2, 23.0, 45.0));
            slots.push(slot(3, 143.0, 57.0));
        }
        (SpecialLayout::Stonecutter, 2) => {
            slots.push(slot(0, 20.0, 33.0));
            slots.push(slot(1, 143.0, 33.0));
        }
        (SpecialLayout::Cartography, 3) => {
            slots.push(slot(0, 15.0, 15.0));
            slots.push(slot(1, 15.0, 52.0));
            slots.push(slot(2, 145.0, 39.0));
        }
        // A 3×3 square, not `generic_layout`'s flat 9-wide row — the one
        // thing that actually distinguishes a dispenser/dropper's screen
        // from a plain 9-slot chest.
        (SpecialLayout::Dispenser, 9) => {
            for i in 0..9 {
                slots.push(slot(
                    i,
                    62.0 + (i % 3) as f32 * SLOT,
                    17.0 + (i / 3) as f32 * SLOT,
                ));
            }
        }
        (SpecialLayout::Hopper, 5) => {
            for i in 0..5 {
                slots.push(slot(i, 44.0 + i as f32 * SLOT, 20.0));
            }
        }
        // `MerchantMenu.java:42-44` — two payment slots then a take-only
        // result. The trade **list** to their left is not a slot at all; see
        // `super::merchant`.
        (SpecialLayout::Merchant, 3) => {
            slots.push(slot(0, 136.0, 37.0));
            slots.push(slot(1, 162.0, 37.0));
            slots.push(slot(2, 220.0, 37.0));
        }
        _ => return None,
    }
    // Every one of these calls `addStandardInventorySlots(inventory, x,
    // main_y)` with a **fixed** `main_y` — `84.0` for every screen except the
    // hopper, whose real panel is *shorter* (`imageHeight = 133`, not `166`)
    // and whose own constructor passes `51` (`HopperMenu.java:27`), not `84`.
    // Getting this one wrong is exactly the "plausible but transposed"
    // failure mode this whole function warns about: `84.0` would still
    // produce a valid-looking layout, just one that overlaps the hopper's
    // own five slots.
    let main_y = if special == SpecialLayout::Hopper {
        51.0
    } else {
        84.0
    };
    // `x = 8` for every screen but the merchant, whose player section starts
    // at `x = 108` (`MerchantMenu.java:45`) — see `append_main_inventory_at`.
    let main_x = if special == SpecialLayout::Merchant {
        108.0
    } else {
        8.0
    };
    // `176` for every screen but the merchant, whose panel is `276` wide
    // (`MerchantScreen.java:57`'s `super(menu, inventory, title, 276, 166)`).
    let width = if special == SpecialLayout::Merchant {
        276.0
    } else {
        176.0
    };
    let hotbar_y = append_main_inventory_at(&mut slots, container_size, main_x, main_y);
    Some(SlotLayout {
        width,
        height: hotbar_y + 24.0,
        slots,
    })
}

/// Appends the 27-slot main inventory (9-wide rows starting at `base`) and the
/// 9-slot hotbar 58px below the last main row — the standard vanilla
/// arrangement shared by every screen that shows the player's own inventory
/// below its container-specific slots. Returns the hotbar's y so callers can
/// size their panel around it.
///
/// `main_x` is the left edge of the grid — `8.0` for every screen but the
/// merchant, whose `addStandardInventorySlots(inventory, 108, 84)` starts its
/// player section at `x = 108` (`MerchantMenu.java:45`); see
/// [`append_main_inventory`] for the `x = 8` convenience every other caller
/// still uses.
fn append_main_inventory_at(slots: &mut Vec<SlotRect>, base: usize, main_x: f32, main_y: f32) -> f32 {
    for i in 0..27 {
        slots.push(slot(
            base + i,
            main_x + (i % 9) as f32 * SLOT,
            main_y + (i / 9) as f32 * SLOT,
        ));
    }
    let hotbar_y = main_y + 58.0;
    for i in 0..9 {
        slots.push(slot(base + 27 + i, main_x + i as f32 * SLOT, hotbar_y));
    }
    hotbar_y
}

/// [`append_main_inventory_at`] at vanilla's usual `x = 8` — every screen
/// except the merchant.
fn append_main_inventory(slots: &mut Vec<SlotRect>, base: usize, main_y: f32) -> f32 {
    append_main_inventory_at(slots, base, 8.0, main_y)
}

fn player_layout() -> SlotLayout {
    let mut slots = Vec::with_capacity(46);
    slots.push(slot(0, 154.0, 28.0));
    for i in 0..4 {
        slots.push(slot(
            1 + i,
            98.0 + (i % 2) as f32 * SLOT,
            18.0 + (i / 2) as f32 * SLOT,
        ));
    }
    for i in 0..4 {
        slots.push(slot(5 + i, 8.0, 8.0 + i as f32 * SLOT));
    }
    append_main_inventory(&mut slots, 9, 84.0);
    slots.push(slot(45, 77.0, 62.0));
    SlotLayout {
        width: 176.0,
        height: 166.0,
        slots,
    }
}

fn generic_layout(container_size: usize) -> SlotLayout {
    let cols = 9usize;
    let rows = container_size.div_ceil(cols).max(1);
    let mut slots = Vec::with_capacity(container_size + 36);
    for i in 0..container_size {
        slots.push(slot(
            i,
            8.0 + (i % cols) as f32 * SLOT,
            18.0 + (i / cols) as f32 * SLOT,
        ));
    }
    let main_y = 18.0 + rows as f32 * SLOT + 14.0;
    let hotbar_y = append_main_inventory(&mut slots, container_size, main_y);
    SlotLayout {
        width: 176.0,
        height: hotbar_y + 24.0,
        slots,
    }
}

/// The crafting-table arrangement: the input grid top-left, the take-only
/// result slot to its right, then the player's main storage and hotbar below.
///
/// The constants are vanilla's `crafting_table.png` slot origins for the 3×3
/// case — grid at `(30, 17)`, result at `(124, 35)`, main at `(8, 84)`, hotbar at
/// `(8, 142)`, panel `176x166` — expressed in terms of the grid's real
/// dimensions so a differently sized grid (none ships in vanilla) still lands
/// somewhere sane rather than on top of the inventory.
///
/// The result slot is drawn but never *filled* here: a vanilla server computes
/// the crafting result itself and pushes it as a `container_set_slot` for slot
/// 0, which `Menus::apply` reconciles into the menu. Reading `slot_item` is
/// therefore reading server truth; matching a recipe locally to fill this slot
/// would overwrite it with a guess.
fn crafting_layout(craft: CraftLayout, container_size: usize) -> SlotLayout {
    let grid_x = 30.0;
    let grid_y = 17.0;
    let cols = craft.width.max(1);
    let rows = craft.height.max(1);
    let mut slots = Vec::with_capacity(container_size + 36);

    slots.push(slot(
        craft.result_slot,
        grid_x + cols as f32 * SLOT + 40.0,
        grid_y + (rows as f32 - 1.0) * SLOT * 0.5,
    ));
    for i in 0..craft.cell_count() {
        slots.push(slot(
            craft.first_input + i,
            grid_x + (i % cols) as f32 * SLOT,
            grid_y + (i / cols) as f32 * SLOT,
        ));
    }

    let main_y = (grid_y + rows as f32 * SLOT + 13.0).max(84.0);
    let hotbar_y = append_main_inventory(&mut slots, container_size, main_y);
    SlotLayout {
        width: 176.0,
        height: hotbar_y + 24.0,
        slots,
    }
}

/// Where the panel's top-left corner lands, in the **logical** GUI canvas —
/// `width`/`height` in physical framebuffer pixels, divided by the effective
/// GUI scale exactly as [`crate::menu::render::logical_canvas`] does for the
/// menu screens (reused here rather than a second scale computation). [`hit_test`]
/// converts an incoming physical cursor position down to this same logical
/// space before comparing against it, which is what keeps the two agreeing.
///
/// The single source of the centring offset. [`ContainerGeometry::build_inner`]
/// and [`hit_test`] must agree to the pixel or the screen and the mouse disagree
/// about which slot is which — a bug that reads as "clicks land one slot off"
/// and is invisible in any screenshot.
///
/// Always lays out against [`crate::config::AUTO_GUI_SCALE`]. Use
/// [`panel_origin_with_scale`] to lay out against a specific (e.g. persisted
/// manual) `gui_scale` instead — this is a thin wrapper over it, kept so every
/// existing caller of this exact signature (the pixel gates,
/// `tests/container_screen.rs`) is unaffected.
#[must_use]
pub fn panel_origin(layout: &SlotLayout, width: u32, height: u32) -> (f32, f32) {
    panel_origin_with_scale(layout, crate::config::AUTO_GUI_SCALE, width, height)
}

/// As [`panel_origin`], but against an explicit `gui_scale` (`0` = auto) rather
/// than always auto. `app.rs`'s real windowed render/hit-test path uses this
/// with the persisted `Options.gui_scale` so a manual scale setting moves the
/// drawn panel and the click hit-rects together — see [`hit_test_with_scale`].
#[must_use]
pub fn panel_origin_with_scale(
    layout: &SlotLayout,
    gui_scale: u32,
    width: u32,
    height: u32,
) -> (f32, f32) {
    let (w, h) = crate::menu::render::logical_canvas(gui_scale, width, height);
    (
        ((w - layout.width) * 0.5).max(8.0),
        ((h - layout.height) * 0.5).max(8.0),
    )
}

/// `AbstractRecipeBookScreen.widthTooNarrow`'s threshold —
/// `this.widthTooNarrow = this.width < 379`
/// (`AbstractRecipeBookScreen.java:30`).
///
/// **379 is not a round number, it is the exact fit**, and reading it that way is
/// what makes the rest of this arithmetic checkable. At `w == 379` the book's own
/// origin is `(379 - 147) / 2 - 86 = 30`, its category tabs sit 30 px further
/// left at exactly `0`, its right edge is `30 + 147 = 177`, and the shifted
/// container panel starts at `177 + (379 - 176 - 200) / 2 = 178`. Every one of
/// `147`, `86`, `30` and `177` locks into that single pixel. Below it, nothing
/// fits and vanilla stops offsetting at all (`xOffset = 0`) and accepts the
/// overlap.
pub const RECIPE_BOOK_MIN_WIDTH: f32 = 379.0;

/// `RecipeBookComponent.xOffset`'s wide-screen value, `86`
/// (`RecipeBookComponent.java:117`) — how far left of screen-centre the book's
/// own 147-wide page is drawn.
pub const RECIPE_BOOK_X_OFFSET: f32 = 86.0;

/// The `177` and `200` in `RecipeBookComponent.updateScreenPosition`
/// (`:173-180`).
const RECIPE_BOOK_SCREEN_LEFT: f32 = 177.0;
/// See [`RECIPE_BOOK_SCREEN_LEFT`].
const RECIPE_BOOK_SCREEN_SPAN: f32 = 200.0;

/// Whether the canvas is too narrow for the book to sit beside the container
/// panel — see [`RECIPE_BOOK_MIN_WIDTH`].
#[must_use]
pub fn recipe_book_width_too_narrow(canvas_w: f32) -> bool {
    canvas_w < RECIPE_BOOK_MIN_WIDTH
}

/// How far **right** the container panel moves when the recipe book is open —
/// `RecipeBookComponent.updateScreenPosition` (`:173-180`):
///
/// ```java
/// if (this.isVisible() && !this.widthTooNarrow) {
///    leftPos = 177 + (width - imageWidth - 200) / 2;
/// } else {
///    leftPos = (width - imageWidth) / 2;
/// }
/// ```
///
/// Returned as a **delta** from [`panel_origin_with_scale`]'s own centring rather
/// than as an absolute `leftPos`, so every caller that does not know about the
/// book keeps the origin it already had and the ones that do add one number. That
/// is the whole reason this is a separate function instead of two more parameters
/// on `panel_origin_with_scale`, which has 24 call sites across 14 files.
///
/// Zero when the book is closed **or** the canvas is too narrow, which is exactly
/// vanilla's `else` branch — the narrow case accepts the overlap rather than
/// shifting, and that is a decision vanilla makes, not a gap here.
///
/// # Why the book was overlapping before
///
/// `container::recipe_book` used to place the book *relative to the container
/// panel* (`mx - 147 - 8`, floored at 4) and clamp the tabs to the same floor. So
/// on a narrow canvas the four category tabs stacked on top of the page and the
/// page slid under the panel — the owner's "the four buttons on the side get
/// squished into the menu". Vanilla never places the book relative to the panel
/// at all: the book is **screen**-centred (`(width - 147) / 2 - xOffset`) and the
/// *panel* is what moves. Getting that backwards is why no amount of clamping
/// could make it fit.
#[must_use]
pub fn recipe_book_panel_shift(canvas_w: f32, panel_w: f32, book_open: bool) -> f32 {
    if !book_open || recipe_book_width_too_narrow(canvas_w) {
        return 0.0;
    }
    // Both halves use Java integer division on the same expression, so both are
    // floored before the subtraction — computing the delta in floats and flooring
    // once would be off by a pixel on odd canvases.
    let shifted = RECIPE_BOOK_SCREEN_LEFT
        + ((canvas_w - panel_w - RECIPE_BOOK_SCREEN_SPAN) * 0.5).floor();
    let centred = ((canvas_w - panel_w) * 0.5).floor();
    shifted - centred
}

/// What a viewport pixel is over, in an open menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuHit {
    /// A menu slot, by its real menu-slot index — feed it straight to
    /// [`Click`](lodestone_game::click::Click).
    Slot(usize),
    /// Inside the panel but not over a slot. Vanilla does **nothing** here; it
    /// is deliberately not a drop.
    Panel,
    /// Outside the panel. Vanilla treats a click here as the outside-slot
    /// sentinel (`-999`), i.e. throw the cursor stack into the world.
    Outside,
}

/// Resolves a viewport pixel to a menu slot, mirroring vanilla
/// `AbstractContainerScreen.findSlot` / `hasClickedOutside`.
///
/// The hit rect is vanilla's `isHovering(x, y, 16, 16, …)`: the 16×16 cell grown
/// by one pixel on every side, which is exactly the 18×18 well this module
/// draws — so the clickable area and the visible area are the same rectangle by
/// construction rather than by coincidence.
///
/// `x`/`y` are raw **physical** viewport pixels — the same space `width`/
/// `height` and the cursor position `app.rs` tracks are already in. This module
/// *does* apply a GUI scale of its own (the same effective scale
/// [`crate::config::calculate_gui_scale`] picks for [`panel_origin`] and the
/// drawn geometry): the incoming physical cursor is divided down to the same
/// logical space the widget was laid out in before anything is compared, so a
/// caller does not need to pre-scale the cursor itself.
#[must_use]
pub fn hit_test(menu: &Menu, width: u32, height: u32, x: f32, y: f32) -> MenuHit {
    hit_test_with_scale(menu, crate::config::AUTO_GUI_SCALE, width, height, x, y)
}

/// As [`hit_test`], but against an explicit `gui_scale` (`0` = auto). Must be
/// called with the **same** `gui_scale` the frame was last drawn with — see
/// [`panel_origin_with_scale`] — or clicks land on the wrong slot while the
/// screen still looks correct, exactly the class of bug this module's own
/// docs warn about.
#[must_use]
pub fn hit_test_with_scale(
    menu: &Menu,
    gui_scale: u32,
    width: u32,
    height: u32,
    x: f32,
    y: f32,
) -> MenuHit {
    hit_test_with_book(menu, gui_scale, width, height, x, y, false)
}

/// As [`hit_test_with_scale`], with the recipe book's own panel shift applied —
/// see [`recipe_book_panel_shift`].
///
/// The split keeps every existing caller on the unshifted answer (which is the
/// correct one with the book closed, and what every pixel gate measures) while the
/// driver's real click path passes the live flag. **`book_open` here must be the
/// same bool `ContainerFrame::with_book_open` was given for the frame that was
/// drawn**, for exactly the reason this module's `gui_scale` warning gives: a
/// hit-test shifted differently from the draw sends every click to the wrong slot
/// while the screen still looks right.
#[must_use]
pub fn hit_test_with_book(
    menu: &Menu,
    gui_scale: u32,
    width: u32,
    height: u32,
    x: f32,
    y: f32,
    book_open: bool,
) -> MenuHit {
    let layout = slot_layout(menu);
    let (px, py) = panel_origin_with_scale(&layout, gui_scale, width, height);
    let (cw, _) = crate::menu::render::logical_canvas(gui_scale, width, height);
    let px = px + recipe_book_panel_shift(cw, layout.width, book_open);
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    let local_x = x / scale - px;
    let local_y = y / scale - py;
    if local_x < 0.0 || local_y < 0.0 || local_x >= layout.width || local_y >= layout.height {
        return MenuHit::Outside;
    }
    for rect in &layout.slots {
        if local_x >= rect.x - 1.0
            && local_x < rect.x + rect.w + 1.0
            && local_y >= rect.y - 1.0
            && local_y < rect.y + rect.h + 1.0
        {
            return MenuHit::Slot(rect.menu_index);
        }
    }
    MenuHit::Panel
}

fn slot(menu_index: usize, x: f32, y: f32) -> SlotRect {
    SlotRect {
        menu_index,
        x,
        y,
        w: CELL,
        h: CELL,
    }
}
