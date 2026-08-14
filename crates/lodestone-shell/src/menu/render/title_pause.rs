//! The title screen's button column and the pause screen's grid, plus the
//! [`Slot`] tables for the title, pause and death screens.
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use super::*;

/// Vanilla's title-screen stack, **re-expressed** as a [`layout::LinearLayout`]
/// column: three full-width rows, the three icon buttons as a nested centred
/// horizontal row, then the Options/Quit pair as another horizontal row.
///
/// **Vanilla's `TitleScreen` uses no layout class at all** — it hand-centres on
/// `this.width / 2 - 100` and steps `topPos` by 24 (`TitleScreen.java`),
/// and that fix's plan is explicit that a hand-arithmetic screen is legitimate
/// vanilla. What makes this re-expression faithful rather than invented is that
/// the two are *numerically identical*, which is not a coincidence:
///
/// - `spacing = 24` on 20 px buttons is a 4 px `rowSpacing`, so the rows land on
///   `0, 24, 48, 72, 96` either way.
/// - the column's width is `max(200, 68, 200) = 200`, so centring it on
///   `width / 2` is `width / 2 - 100`;
/// - `getHorizontalPosition(n, 3, 20)` is `width/2 - 34 + (n-1) * 24`
///   (`TitleScreen.java`), and a 68 px row centred in the 200 px column
///   is at `lerp(0.5, 0, 200 - 68) = 66`, i.e. `width/2 - 100 + 66` — the same
///   `width/2 - 34`. The 34 is `totalWidth / 2` and the 66 is `(200 - 68) / 2`;
///   they agree because `100 - 66 == 34`.
/// - `98 + 4 + 98 == 200`, so the half-width pair fills the column exactly and
///   its two children are at `+0` and `+102`.
///
/// `the_title_screen_rects_are_vanillas_own` asserts all eight rects against the
/// hand-derived table, so if the equality above ever stops holding, it fails.
fn title_menu_column() -> layout::LinearLayout {
    let button = |w: f32, h: f32| -> Box<dyn widget::LayoutElement> {
        Box::new(Widget::button(0.0, 0.0, w, h, ""))
    };
    // The gap `spacing = 24` leaves between two 20 px buttons.
    let row_spacing = (TITLE_PITCH - WIDGET_H) as i32;
    let mut column = layout::LinearLayout::vertical().spacing(row_spacing);
    for _ in 0..3 {
        column.add_child(button(WIDE_W, WIDGET_H));
    }
    // `getHorizontalPosition` centres the icon row in the stack's width.
    let mut icons = layout::LinearLayout::horizontal().spacing(row_spacing);
    for _ in 0..3 {
        icons.add_child(button(ICON_BTN, ICON_BTN));
    }
    column.add_child_settings(
        Box::new(icons),
        layout::LayoutSettings::defaults().align_horizontally_center(),
    );
    let mut pair = layout::LinearLayout::horizontal().spacing(row_spacing);
    for _ in 0..2 {
        pair.add_child(button(TITLE_HALF_W, WIDGET_H));
    }
    column.add_child(Box::new(pair));
    column.arrange_elements();
    column
}

/// Vanilla's `PauseScreen.createPauseMenu` (`PauseScreen.java`) as a real
/// [`layout::GridLayout`], arranged.
///
/// `menu_padding_top` is `MENU_PADDING_TOP` (50) in production; it is a parameter
/// only so `a_changed_cell_padding_moves_every_pause_rect` can run the negative
/// control that fix asks for — change one `LayoutSettings` padding value and watch the
/// rect assertions go red — against the real builder rather than a copy of it.
///
/// The Options row takes vanilla's **`hasSingleplayerServer()`** branch
/// (`:157-160`): two half-width buttons, Options and Open to LAN, rather than one
/// full-width Options. This client does host its own worlds, and
/// since that fix the second button has something to do — the previous version
/// of this comment said "this client has no integrated server" and had been stale
/// since singleplayer landed.
///
/// Vanilla takes the *other* branch on a remote server, hiding Open to LAN
/// entirely. This layout is static and always shows it; see
/// [`PauseButton::OpenToLan`](super::nav::PauseButton::OpenToLan).
pub(super) fn pause_menu_grid_with(menu_padding_top: i32) -> layout::GridLayout {
    let button = |w: f32, h: f32| -> Box<dyn widget::LayoutElement> {
        Box::new(Widget::button(0.0, 0.0, w, h, ""))
    };
    let mut grid = layout::GridLayout::new();
    {
        // `gridLayout.defaultCellSetting().padding(4, 4, 4, 0)` (`:93`) — the
        // *live* baseline, so every cell below inherits it.
        let baseline = grid.default_cell_setting();
        *baseline = baseline.padding_ltrb(
            PAUSE_BUTTON_PADDING,
            PAUSE_BUTTON_PADDING,
            PAUSE_BUTTON_PADDING,
            0,
        );
    }
    let mut helper = grid.create_row_helper(PAUSE_COLUMNS);
    // Back to Game: full width, and the one cell with the 50 px top padding.
    let first = helper.new_cell_settings().padding_top(menu_padding_top);
    helper.add_child_with(button(PAUSE_BUTTON_FULL_W, WIDGET_H), PAUSE_COLUMNS, first);
    // Advancements and Statistics share a row, one column each.
    helper.add_child(button(PAUSE_BUTTON_HALF_W, WIDGET_H));
    helper.add_child(button(PAUSE_BUTTON_HALF_W, WIDGET_H));
    // The four icon buttons are a nested horizontal row, spanning both columns
    // and horizontally centred in them (`:154`).
    let mut icons = layout::LinearLayout::horizontal().spacing(PAUSE_ICON_SPACING);
    for _ in 0..4 {
        icons.add_child(button(ICON_BTN, ICON_BTN));
    }
    let centred = helper.new_cell_settings().align_horizontally_center();
    helper.add_child_with(Box::new(icons), PAUSE_COLUMNS, centred);
    // Options and Open to LAN share a row, one column each — vanilla's
    // singleplayer branch. Then Disconnect, full width.
    helper.add_child(button(PAUSE_BUTTON_HALF_W, WIDGET_H));
    helper.add_child(button(PAUSE_BUTTON_HALF_W, WIDGET_H));
    helper.add_spanning(button(PAUSE_BUTTON_FULL_W, WIDGET_H), PAUSE_COLUMNS);
    drop(helper);
    grid.arrange_elements();
    grid
}

/// One arranged menu block: its own size, plus each leaf's rect in the order
/// `visit_widgets` yields them (which is insertion order, in vanilla too).
#[derive(Debug)]
struct MenuBlock {
    size: (f32, f32),
    cells: Vec<(f32, f32, f32, f32)>,
}

impl MenuBlock {
    /// Collect an **already-arranged** `root`'s leaves. `expected` is the number
    /// of drawable leaves the caller's button table needs; a mismatch is a tree
    /// that no longer describes the screen, and it must fail loudly rather than
    /// silently shift every rect by one.
    fn of(root: &dyn widget::LayoutElement, expected: usize) -> Self {
        let cells = layout::widget_rects(root);
        assert_eq!(
            cells.len(),
            expected,
            "the arranged tree has {} drawable leaves, the screen has {expected}",
            cells.len()
        );
        Self {
            size: (root.width(), root.height()),
            cells,
        }
    }
}

/// The title-screen column, arranged once.
///
/// Arranging is canvas-*independent* — only the final
/// `FrameLayout.alignInRectangle` step depends on the screen size, and that is
/// what [`Origin`] applies at draw time — so the tree is built once per process
/// rather than per frame. [`super::layout`]'s module docs say which of vanilla's
/// two two-phase timings this is, and why.
fn title_block() -> &'static MenuBlock {
    static BLOCK: std::sync::OnceLock<MenuBlock> = std::sync::OnceLock::new();
    // Vanilla's own eight. `MAIN_BUTTONS`' ninth, `Accounts`, is ours and is a
    // corner widget outside the column entirely.
    BLOCK.get_or_init(|| MenuBlock::of(&title_menu_column(), 8))
}

/// The pause-screen grid, arranged once. See [`title_block`].
fn pause_block() -> &'static MenuBlock {
    static BLOCK: std::sync::OnceLock<MenuBlock> = std::sync::OnceLock::new();
    // Every one of `PAUSE_BUTTONS`, four of them inside the nested icon row.
    // Derived from the table rather than restated: the count moved from 9 to 10
    // when Open to LAN landed, and a literal here is a second place
    // to forget.
    BLOCK.get_or_init(|| {
        MenuBlock::of(
            &pause_menu_grid_with(PAUSE_MENU_PADDING_TOP),
            super::nav::PAUSE_BUTTONS.len(),
        )
    })
}

/// The arranged pause grid's own `(width, height)` — what
/// [`Origin::PauseGrid`] aligns in the screen rect.
///
/// Public so a gate can check it against the hand-derived
/// [`PAUSE_GRID_W`]×[`PAUSE_GRID_H`] rather than restating either.
#[must_use]
pub fn pause_grid_size() -> (f32, f32) {
    pause_block().size
}

/// Vanilla's rect for one title-screen widget, from
/// `TitleScreen.init`/`createNormalMenuOptions`
/// (`TitleScreen.java`) — **read out of the arranged
/// `title_menu_column`**, not written down.
///
/// The offsets are relative to [`Origin::TitleTop`], whose x is `width / 2`,
/// so a cell's `dx` is its position in the column minus half the column's width.
#[must_use]
pub fn title_slot(button: MainButton) -> Slot {
    // The insertion order of `title_menu_column`, which is also `MAIN_BUTTONS`'
    // order for vanilla's own eight. Written as an exhaustive match rather than
    // `button as usize` so adding a variant fails to compile instead of silently
    // indexing the wrong cell.
    let index = match button {
        MainButton::Singleplayer => 0,
        MainButton::Multiplayer => 1,
        MainButton::Realms => 2,
        MainButton::Friends => 3,
        MainButton::Language => 4,
        MainButton::Accessibility => 5,
        MainButton::Options => 6,
        MainButton::Quit => 7,
        // Not vanilla — see `MainButton::Accounts`'s docs and
        // `Origin::TopRight`'s. A corner widget, not one more stack row:
        // vanilla's own eight already reach to within 16 px of the bottom of
        // a 320×240 canvas, so nothing fits below them there. The gap above
        // the logo (`y < LOGO_Y`) is free at every canvas size instead. It is
        // outside the arranged column entirely, which is why it returns early.
        MainButton::Accounts => {
            return Slot {
                origin: Origin::TopRight,
                dx: -(ACCOUNTS_ENTRY_W + ACCOUNTS_ENTRY_MARGIN),
                dy: ACCOUNTS_ENTRY_MARGIN,
                w: ACCOUNTS_ENTRY_W,
                h: WIDGET_H,
            };
        }
    };
    let block = title_block();
    let (x, y, w, h) = block.cells[index];
    Slot {
        origin: Origin::TitleTop,
        dx: x - block.size.0 * 0.5,
        dy: y,
        w,
        h,
    }
}

/// Width of the non-vanilla `Accounts` corner button — see
/// [`Origin::TopRight`]'s docs for why it lives there rather than in
/// vanilla's own vertical stack.
const ACCOUNTS_ENTRY_W: f32 = 90.0;
/// Distance from the top-right corner to the `Accounts` button, both axes.
const ACCOUNTS_ENTRY_MARGIN: f32 = 4.0;

/// Vanilla's rect for one pause-screen widget, from
/// `PauseScreen.createPauseMenu` (`PauseScreen.java`) — **read out of the
/// arranged grid** (`pause_menu_grid_with`) rather than resolved by hand.
///
/// It used to be a table of nine hand-derived offsets, and the derivation is
/// worth keeping because it is what the port has to reproduce: column widths are
/// `[106, 106]` (the 204+8 full-width cell split over two columns by `Divisor`);
/// row heights are `[70, 24, 24, 24, 24]`, so row y offsets are
/// `[0, 70, 94, 118, 142]`. Each child's own offset inside its cell is its
/// `paddingLeft`/`paddingTop` because the default `xAlignment` is 0 — and with
/// `padding(4, 4, 4, 0)` a full-width button's `mostOffset` is also 4, so
/// alignment could not move it anyway. The icon row is the one centred cell
/// (`alignHorizontallyCenter`, `PauseScreen.java`):
/// `lerp(0.5, 4, 212 - 92 - 4) = 60`, and its own `LinearLayout` spaces four
/// 20 px children 4 px apart from there — 60, 84, 108, 132.
///
/// That table now lives in `the_pause_screen_rects_are_vanillas_own`, where it is
/// the *expectation* instead of the implementation — an expected value has to come
/// from outside the code under test.
#[must_use]
pub fn pause_slot(button: PauseButton) -> Slot {
    // `pause_menu_grid_with`'s insertion order, which is `PAUSE_BUTTONS`' order.
    // Exhaustive rather than `button as usize` so a new variant is a compile
    // error and not a silent off-by-one across every rect.
    let index = match button {
        PauseButton::BackToGame => 0,
        PauseButton::Advancements => 1,
        PauseButton::Statistics => 2,
        PauseButton::ReportBugs => 3,
        PauseButton::Feedback => 4,
        PauseButton::Friends => 5,
        PauseButton::PlayerReporting => 6,
        PauseButton::Options => 7,
        PauseButton::OpenToLan => 8,
        PauseButton::QuitToTitle => 9,
    };
    let (dx, dy, w, h) = pause_block().cells[index];
    Slot {
        origin: Origin::PauseGrid,
        dx,
        dy,
        w,
        h,
    }
}

/// Vanilla's rect for one death-screen button:
/// `this.width / 2 - 100, this.height / 4 + 72 | 96, 200, 20`
/// (`DeathScreen.java`). Both buttons share `x`; only `y` differs.
///
/// `height / 4 + 72` and `+ 96` are `Origin::TitleTop`'s own anchor
/// (`height / 4 + 48`, `TitleScreen.java`) plus `24`/`48` — the death
/// screen and the title screen both lay their stacks out from
/// `this.height / 4`, so reusing that origin here rather than adding a
/// second one is deliberate, not a coincidence to "clean up".
#[must_use]
pub fn death_slot(button: super::nav::DeathButton) -> Slot {
    use super::nav::DeathButton;
    let dy = match button {
        DeathButton::Respawn => 24.0,
        DeathButton::TitleScreen => 48.0,
    };
    Slot {
        origin: Origin::TitleTop,
        dx: -100.0,
        dy,
        w: WIDE_W,
        h: WIDGET_H,
    }
}

