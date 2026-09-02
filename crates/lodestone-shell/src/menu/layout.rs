//! Vanilla's **layout containers** — `net/minecraft/client/gui/layouts/` — ported
//! as arithmetic: [`GridLayout`], [`LinearLayout`], [`FrameLayout`],
//! [`HeaderAndFooterLayout`], [`SpacerElement`] and the [`LayoutSettings`] cell
//! model they all arrange against.
//!
//! ## What it is
//!
//! The second child of the menu-framework epic (#392/#394). [`super::widget`]
//! landed the leaf ([`Widget`]) and the [`LayoutElement`] seam; this is the part
//! that *places* leaves. Its consumer is [`super::render`]'s `title_slot` and
//! `pause_slot`, which no longer hold hand-typed offsets: the title column and
//! the pause grid are built here, arranged, and read back.
//!
//! ## The model, in three sentences
//!
//! A container owns children, each wrapped with a [`LayoutSettings`] (padding
//! plus a `0.0..=1.0` alignment per axis). One [`LayoutElement::arrange_elements`]
//! pass walks the tree bottom-up, sizes every container from its children, and
//! writes absolute positions into the leaves. A screen then walks the leaves with
//! [`LayoutElement::visit_widgets`] — which is the only way a leaf reaches a draw,
//! and is why [`SpacerElement`] (whose `visit_widgets` is a no-op, exactly as in
//! `SpacerElement.java`) occupies space without ever being drawn.
//!
//! ### The alignment model is padding-aware
//!
//! `AbstractLayout.AbstractChildWrapper::setX` (`AbstractLayout.java`) is
//! the whole of it:
//!
//! ```text
//! offset = lerp(xAlignment, paddingLeft, availableSpace - child.width - paddingRight)
//! ```
//!
//! **Not** `(available - width) / 2`. With `paddingLeft = 10`, `paddingRight = 0`,
//! a 20 px child in 100 px of space and `xAlignment = 0.5`, vanilla gives 45 and
//! the naive centre gives 40 — see `set_x_is_padding_aware_not_a_naive_centre`.
//!
//! `setY` (`:80-85`) is the same expression with **`Math.round` where `setX`
//! truncates**. That asymmetry is real, it is in the jar, and it is worth one
//! pixel: a child centred in a cell 5 px taller than itself lands at x = 2 and
//! y = 3 (`set_y_rounds_where_set_x_truncates`). It is reproduced, not tidied.
//!
//! ## Vanilla has two two-phase timings; this follows `PauseScreen`'s
//!
//! | screen | order |
//! |---|---|
//! | `PauseScreen.createPauseMenu` (`:180-182`) | build → `arrangeElements()` → `FrameLayout.alignInRectangle` → `visitWidgets` |
//! | `OptionsSubScreen.init` (`:28-34`) | build → `visitWidgets` → `repositionElements()` → `arrangeElements()` |
//!
//! The second exists so a **resize repositions existing widgets** instead of
//! rebuilding them, which matters once a widget holds state (focus, a scroll
//! offset, an `EditBox`'s text).
//!
//! This port follows the first, for three reasons: it is what the screen being
//! converted does; there is nothing to reposition, because
//! [`super::render`]'s `frame_for`/`pause_frame` rebuild every row — labels
//! included — every frame, so no widget survives a frame let alone a resize; and
//! the resize problem is already solved better by `Slot`. **Arranging is
//! canvas-independent**: only the final `alignInRectangle` depends on the screen
//! size, and that is exactly what `Origin` applies at draw time. So a tree is
//! arranged once per process and a resize costs neither a rebuild nor a
//! re-arrange. When #395 gives widgets persistent focus, `OptionsSubScreen`'s
//! order becomes the right one for screens that own state — which is the reason
//! this choice is written down rather than assumed.
//!
//! **#395 landed and that is what happened**, for the one screen that owns state:
//! [`super::edit_box::EditBox`] holds a caret, a selection and a scroll offset, so
//! `Screen::ServerEdit`'s fields cannot be rebuilt per frame and
//! [`super::render`]'s `draw_edit_box` *repositions* them instead. Nothing in this
//! file changed — the title and pause trees still have nothing to reposition — but
//! the sentence above is no longer hypothetical. See `docs/menu-focus.md`.
//!
//! ## How to change it
//!
//! - **`GridLayout`'s row and column counts are *derived*, not declared.** They
//!   are `max(lastOccupiedRow)` / `max(lastOccupiedColumn)` over the children
//!   (`GridLayout.java`), so a child at row 3 in an otherwise empty grid
//!   creates rows 0..3 with heights `[0, 0, 0, h]`. Nothing anywhere states a
//!   dimension.
//! - **A spanning cell splits its size with a [`Divisor`], not by division.**
//!   `Divisor` is Mojang's Bresenham-style integer splitter
//!   (`com/mojang/math/Divisor.java`): 7 over 3 is `2, 2, 3`, not `2.33` three
//!   times. A span only *grows* a row or column if its share exceeds what is
//!   already there (`Math.max`, `GridLayout.java`), which is why the pause
//!   screen's 212 px full-width cell produces two 106 px columns and its 100 px
//!   icon row (spanning the same two) changes nothing.
//! - **[`LinearLayout`] is not its own algorithm.** It *wraps* a one-row or
//!   one-column `GridLayout` and delegates `arrangeElements` entirely
//!   (`LinearLayout.java`); `spacing()` sets `columnSpacing` when
//!   horizontal and `rowSpacing` when vertical (`:103-111`). Do not give it
//!   arithmetic of its own — a bug fixed in `GridLayout` must fix it too.
//! - **`FrameLayout`'s children default to centred, `GridLayout`'s do not.**
//!   `FrameLayout.java` is `LayoutSettings.defaults().align(0.5F, 0.5F)`;
//!   `GridLayout.java` and `EqualSpacingLayout.java` are bare
//!   `LayoutSettings.defaults()`, i.e. top-left. Getting this backwards centres
//!   or corners a whole screen, and it looks deliberate either way.
//! - **[`GridLayout::default_cell_setting`] is the live baseline;
//!   [`GridLayout::new_cell_settings`] is a copy of it**
//!   (`GridLayout.java`). Mutating the former changes what
//!   every *subsequent* cell inherits. Ours are `Copy`, so a child snapshots its
//!   settings when added; vanilla can also *alias* the live object into a cell, so
//!   a late mutation there would move already-added children. That is the one
//!   deliberate deviation in this file, and it is unobservable across the whole
//!   client — grepped, not assumed, because the first version of this note claimed
//!   something false:
//!
//!   The aliasing is reachable through **exactly one** path: `RowHelper`'s short
//!   `addChild` forms, which pass `defaultCellSetting()` itself
//!   (`GridLayout.java`). Every other `addChild` — `GridLayout`'s,
//!   `LinearLayout`'s, `FrameLayout`'s, `EqualSpacingLayout`'s — passes a
//!   `copy()`. Of the ~75 `defaultCellSetting`/`defaultChildLayoutSetting` call
//!   sites in the client, the only one on a `RowHelper` is
//!   `RealmsResetWorldScreen.java`, and it runs before that helper's first
//!   add.
//!
//!   One screen *does* mutate a live baseline mid-build — `DisconnectedScreen`
//!   sets `padding(10)`, adds the title and reason, then sets `padding(2)` for the
//!   buttons that follow (`:44-47`) — and it is a `LinearLayout`, whose `addChild`
//!   copies, so the first two children keep padding 10 in vanilla exactly as they
//!   do here. "No screen mutates the baseline after an add" would have been the
//!   wrong claim; "no screen mutates a baseline that was aliased into a cell" is
//!   the true one.
//! - **`arrange_elements` lives on [`LayoutElement`] with a no-op default**,
//!   where vanilla puts it on `Layout` and tests `child instanceof Layout` in the
//!   default body (`Layout.java`). Behaviourally identical — a leaf's
//!   arrange is a no-op either way — and it avoids needing a downcast from
//!   `dyn LayoutElement`.
//! - **`addChild` returns nothing.** Vanilla hands the child back so the screen
//!   can keep a reference *and* the layout can own it; Rust cannot. Read results
//!   back with [`LayoutElement::visit_widgets`], whose order is insertion order in
//!   vanilla too.
//! - **Every offset here is an integer.** Vanilla's layouts are `int`-only, and
//!   the truncations are load-bearing (see the `setX`/`setY` asymmetry above), so
//!   the internals are `i32` and the `f32` [`LayoutElement`] seam is converted at
//!   the boundary by [`ipx`]. Nothing in the shell hands a layout a fractional
//!   size; if something ever does, it is rounded to the nearest pixel there.
//!
//! ## Not here, on purpose
//!
//! - **`EqualSpacingLayout`.** One user in the whole client tree
//!   (`CommonLayouts.java` aside, `screens/` references it once), so porting it
//!   now would be an island. Its only bearing on this file is that its default
//!   cell settings are top-left, unlike `FrameLayout`'s.
//! - **`CommonLayouts`.** Two static helpers over `LinearLayout`; they belong with
//!   whichever screen first needs them.
//! - **Focus and tab order.** `Screen`-level dispatch is #395. A layout knows
//!   nothing about it in vanilla either.
//!
//! ## Dependencies
//!
//! [`super::widget`] for [`Widget`] and the [`LayoutElement`] seam. Nothing else —
//! the module is pure arithmetic and allocates only its own child vectors.

use super::widget::{LayoutElement, Widget};

/// `Mth.lerp(alpha, p0, p1)` (`Mth.java`), argument order included.
#[must_use]
fn lerp(alpha: f32, p0: f32, p1: f32) -> f32 {
    p0 + alpha * (p1 - p0)
}

/// Java's `(int)` cast on a `float`: truncate toward zero.
#[must_use]
fn trunc_int(v: f32) -> i32 {
    v.trunc() as i32
}

/// Java's `Math.round(float)`: `floor(v + 0.5)`.
///
/// **Not** Rust's `f32::round`, which rounds half *away from zero* — the two
/// disagree at every negative half (`Math.round(-2.5) == -2`, `(-2.5f32).round()
/// == -3.0`). A layout only sees negatives when a child is wider than its cell,
/// which happens the moment a screen is narrower than its content.
#[must_use]
fn java_round(v: f32) -> i32 {
    (v + 0.5).floor() as i32
}

/// The `f32` [`LayoutElement`] seam in this file's integer pixels.
///
/// Rounds rather than truncates: every size in the shell is an integral `f32`
/// constant, so this is exact for all of them, and rounding is the answer that
/// does not lose a pixel to `199.99999`.
#[must_use]
pub fn ipx(v: f32) -> i32 {
    v.round() as i32
}

/// Mojang's `Divisor` (`com/mojang/math/Divisor.java`): splits `numerator` into
/// `denominator` integer parts that sum back to it exactly, distributing the
/// remainder Bresenham-style rather than piling it on one part.
///
/// `Divisor::new(7, 3)` yields `2, 2, 3`. Used by [`GridLayout`] to share a
/// spanning cell's size across the rows and columns it occupies.
///
/// Deliberately **not** `Copy`: it is a stateful cursor, and a copied one would
/// silently restart mid-span.
#[derive(Debug, Clone)]
pub struct Divisor {
    denominator: i32,
    quotient: i32,
    modulo: i32,
    returned_parts: i32,
    remainder: i32,
}

impl Divisor {
    /// A splitter for `numerator` over `denominator` parts. A non-positive
    /// denominator yields nothing, as in the jar.
    #[must_use]
    pub fn new(numerator: i32, denominator: i32) -> Self {
        let (quotient, modulo) = if denominator > 0 {
            (numerator / denominator, numerator % denominator)
        } else {
            (0, 0)
        };
        Self {
            denominator,
            quotient,
            modulo,
            returned_parts: 0,
            remainder: 0,
        }
    }

    /// Whether another part is left.
    #[must_use]
    pub fn has_next(&self) -> bool {
        self.returned_parts < self.denominator
    }

    /// The next part, or `0` once exhausted.
    ///
    /// Vanilla throws `NoSuchElementException` here; returning zero is the same
    /// thing for every call site in this file, which asks for exactly
    /// `denominator` parts, and it keeps a layout bug from crashing a frame.
    pub fn next_int(&mut self) -> i32 {
        if !self.has_next() {
            return 0;
        }
        let mut next = self.quotient;
        self.remainder += self.modulo;
        if self.remainder >= self.denominator {
            self.remainder -= self.denominator;
            next += 1;
        }
        self.returned_parts += 1;
        next
    }
}

impl Iterator for Divisor {
    type Item = i32;

    fn next(&mut self) -> Option<i32> {
        if self.has_next() {
            Some(self.next_int())
        } else {
            None
        }
    }
}

/// Vanilla's `LayoutSettings` (`LayoutSettings.java`): one cell's padding and
/// alignment.
///
/// `Default` is vanilla's `LayoutSettingsImpl()` — zero padding, top-left
/// alignment — so `derive` is correct here, unlike [`Widget`]'s (whose vanilla
/// defaults are `active = true`, `visible = true`).
///
/// The builders take `self` by value and return `Self`, so a settings value is
/// copied rather than aliased; see the module docs on the one place vanilla
/// relies on aliasing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LayoutSettings {
    /// `paddingLeft`.
    pub padding_left: i32,
    /// `paddingTop`.
    pub padding_top: i32,
    /// `paddingRight`.
    pub padding_right: i32,
    /// `paddingBottom`.
    pub padding_bottom: i32,
    /// `xAlignment`: `0.0` left, `0.5` centre, `1.0` right.
    pub x_alignment: f32,
    /// `yAlignment`: `0.0` top, `0.5` middle, `1.0` bottom.
    pub y_alignment: f32,
}

impl LayoutSettings {
    /// `LayoutSettings.defaults()` (`LayoutSettings.java`).
    #[must_use]
    pub fn defaults() -> Self {
        Self::default()
    }

    /// `padding(int)` — the same value on all four sides.
    #[must_use]
    pub fn padding(self, padding: i32) -> Self {
        self.padding_hv(padding, padding)
    }

    /// `padding(int horizontal, int vertical)`.
    #[must_use]
    pub fn padding_hv(self, horizontal: i32, vertical: i32) -> Self {
        self.padding_horizontal(horizontal).padding_vertical(vertical)
    }

    /// `padding(int left, int top, int right, int bottom)` — note the order,
    /// which is *not* CSS's (`LayoutSettings.java`).
    #[must_use]
    pub fn padding_ltrb(self, left: i32, top: i32, right: i32, bottom: i32) -> Self {
        self.padding_left(left)
            .padding_right(right)
            .padding_top(top)
            .padding_bottom(bottom)
    }

    /// `paddingLeft(int)`.
    #[must_use]
    pub fn padding_left(mut self, padding: i32) -> Self {
        self.padding_left = padding;
        self
    }

    /// `paddingTop(int)`.
    #[must_use]
    pub fn padding_top(mut self, padding: i32) -> Self {
        self.padding_top = padding;
        self
    }

    /// `paddingRight(int)`.
    #[must_use]
    pub fn padding_right(mut self, padding: i32) -> Self {
        self.padding_right = padding;
        self
    }

    /// `paddingBottom(int)`.
    #[must_use]
    pub fn padding_bottom(mut self, padding: i32) -> Self {
        self.padding_bottom = padding;
        self
    }

    /// `paddingHorizontal(int)`.
    #[must_use]
    pub fn padding_horizontal(self, padding: i32) -> Self {
        self.padding_left(padding).padding_right(padding)
    }

    /// `paddingVertical(int)`.
    #[must_use]
    pub fn padding_vertical(self, padding: i32) -> Self {
        self.padding_top(padding).padding_bottom(padding)
    }

    /// `align(float, float)`.
    #[must_use]
    pub fn align(mut self, x_alignment: f32, y_alignment: f32) -> Self {
        self.x_alignment = x_alignment;
        self.y_alignment = y_alignment;
        self
    }

    /// `alignHorizontally(float)`.
    #[must_use]
    pub fn align_horizontally(mut self, x_alignment: f32) -> Self {
        self.x_alignment = x_alignment;
        self
    }

    /// `alignVertically(float)`.
    #[must_use]
    pub fn align_vertically(mut self, y_alignment: f32) -> Self {
        self.y_alignment = y_alignment;
        self
    }

    /// `alignHorizontallyLeft()`.
    #[must_use]
    pub fn align_horizontally_left(self) -> Self {
        self.align_horizontally(0.0)
    }

    /// `alignHorizontallyCenter()`.
    #[must_use]
    pub fn align_horizontally_center(self) -> Self {
        self.align_horizontally(0.5)
    }

    /// `alignHorizontallyRight()`.
    #[must_use]
    pub fn align_horizontally_right(self) -> Self {
        self.align_horizontally(1.0)
    }

    /// `alignVerticallyTop()`.
    #[must_use]
    pub fn align_vertically_top(self) -> Self {
        self.align_vertically(0.0)
    }

    /// `alignVerticallyMiddle()`.
    #[must_use]
    pub fn align_vertically_middle(self) -> Self {
        self.align_vertically(0.5)
    }

    /// `alignVerticallyBottom()`.
    #[must_use]
    pub fn align_vertically_bottom(self) -> Self {
        self.align_vertically(1.0)
    }
}

/// Vanilla's `Layout` (`Layout.java`): a [`LayoutElement`] that owns children.
///
/// `arrangeElements` and `visitWidgets` are on [`LayoutElement`] instead — see
/// the module docs — so what is left here is child traversal and teardown.
pub trait Layout: LayoutElement {
    /// `visitChildren`, read-only. Insertion order.
    fn visit_children(&self, visitor: &mut dyn FnMut(&dyn LayoutElement));

    /// `visitChildren`, for the arrange and translate passes.
    fn visit_children_mut(&mut self, visitor: &mut dyn FnMut(&mut dyn LayoutElement));

    /// `removeChildren`.
    fn remove_children(&mut self);
}

/// Every leaf a layout tree places, in insertion order — the rects a screen
/// draws.
///
/// This is [`LayoutElement::visit_widgets`] collected, which is the same walk
/// vanilla's `visitWidgets(this::addRenderableWidget)` performs; a
/// [`SpacerElement`] contributes nothing, so the indices are the *drawable*
/// children only.
#[must_use]
pub fn widget_rects(root: &dyn LayoutElement) -> Vec<(f32, f32, f32, f32)> {
    let mut out = Vec::new();
    root.visit_widgets(&mut |w| out.push(w.rect()));
    out
}

/// `AbstractLayout.AbstractChildWrapper` (`AbstractLayout.java`): one child
/// plus the cell settings it was added with.
#[derive(Debug)]
struct ChildWrapper {
    child: Box<dyn LayoutElement>,
    settings: LayoutSettings,
}

impl ChildWrapper {
    fn new(child: Box<dyn LayoutElement>, settings: LayoutSettings) -> Self {
        Self { child, settings }
    }

    /// `getWidth()`: the child plus its horizontal padding.
    fn width(&self) -> i32 {
        ipx(self.child.width()) + self.settings.padding_left + self.settings.padding_right
    }

    /// `getHeight()`: the child plus its vertical padding.
    fn height(&self) -> i32 {
        ipx(self.child.height()) + self.settings.padding_top + self.settings.padding_bottom
    }

    /// `setX(int x, int availableSpace)` (`AbstractLayout.java`) — the
    /// alignment model, truncating.
    fn set_x(&mut self, x: i32, available_space: i32) {
        let least = self.settings.padding_left as f32;
        let most = (available_space - ipx(self.child.width()) - self.settings.padding_right) as f32;
        let offset = trunc_int(lerp(self.settings.x_alignment, least, most));
        self.child.set_x((offset + x) as f32);
    }

    /// `setY(int y, int availableSpace)` (`AbstractLayout.java`) — the same
    /// expression, **rounded** rather than truncated.
    fn set_y(&mut self, y: i32, available_space: i32) {
        let least = self.settings.padding_top as f32;
        let most = (available_space - ipx(self.child.height()) - self.settings.padding_bottom) as f32;
        let offset = java_round(lerp(self.settings.y_alignment, least, most));
        self.child.set_y((offset + y) as f32);
    }
}

/// `GridLayout.ChildContainer` (`GridLayout.java`).
#[derive(Debug)]
struct GridChild {
    wrapper: ChildWrapper,
    row: usize,
    column: usize,
    occupied_rows: usize,
    occupied_columns: usize,
}

impl GridChild {
    fn last_occupied_row(&self) -> usize {
        self.row + self.occupied_rows - 1
    }

    fn last_occupied_column(&self) -> usize {
        self.column + self.occupied_columns - 1
    }
}

/// Vanilla's `GridLayout` (`GridLayout.java`): children at explicit `(row,
/// column)` cells, optionally spanning, with the grid's dimensions **derived**
/// from the highest occupied index.
///
/// This is the primitive everything else is built on: [`LinearLayout`] is a grid
/// with one row or one column, and the pause menu is a grid directly.
#[derive(Debug, Default)]
pub struct GridLayout {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    children: Vec<GridChild>,
    default_cell_settings: LayoutSettings,
    row_spacing: i32,
    column_spacing: i32,
}

impl GridLayout {
    /// `new GridLayout()` — at the origin, zero-sized until arranged.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `new GridLayout(int x, int y)`.
    #[must_use]
    pub fn at(x: f32, y: f32) -> Self {
        Self {
            x: ipx(x),
            y: ipx(y),
            ..Self::default()
        }
    }

    /// `columnSpacing(int)`.
    #[must_use]
    pub fn column_spacing(mut self, spacing: i32) -> Self {
        self.column_spacing = spacing;
        self
    }

    /// `rowSpacing(int)`.
    #[must_use]
    pub fn row_spacing(mut self, spacing: i32) -> Self {
        self.row_spacing = spacing;
        self
    }

    /// `spacing(int)` — both axes.
    #[must_use]
    pub fn spacing(self, spacing: i32) -> Self {
        self.column_spacing(spacing).row_spacing(spacing)
    }

    /// `defaultCellSetting()` (`GridLayout.java`): the **live** baseline
    /// every later cell is copied from. Mutating it changes what subsequent
    /// `add_child` calls inherit — and nothing else, unlike vanilla, which also
    /// aliases it into already-added cells (see the module docs).
    pub fn default_cell_setting(&mut self) -> &mut LayoutSettings {
        &mut self.default_cell_settings
    }

    /// `newCellSettings()` (`GridLayout.java`): a **copy** of the
    /// baseline, to be adjusted for one cell.
    #[must_use]
    pub fn new_cell_settings(&self) -> LayoutSettings {
        self.default_cell_settings
    }

    /// `addChild(child, row, column)` with the baseline cell settings.
    pub fn add_child(&mut self, child: Box<dyn LayoutElement>, row: usize, column: usize) {
        let settings = self.new_cell_settings();
        self.add_child_with(child, row, column, 1, 1, settings);
    }

    /// `addChild(child, row, column, cellSettings)`.
    pub fn add_child_settings(
        &mut self,
        child: Box<dyn LayoutElement>,
        row: usize,
        column: usize,
        settings: LayoutSettings,
    ) {
        self.add_child_with(child, row, column, 1, 1, settings);
    }

    /// `addChild(child, row, column, rows, columns, cellSettings)` — the one all
    /// the others funnel into (`GridLayout.java`).
    ///
    /// # Panics
    ///
    /// If `occupied_rows` or `occupied_columns` is zero, as vanilla throws
    /// `IllegalArgumentException` for the same input. Both are structural
    /// mistakes in a tree literal, not runtime conditions.
    pub fn add_child_with(
        &mut self,
        child: Box<dyn LayoutElement>,
        row: usize,
        column: usize,
        occupied_rows: usize,
        occupied_columns: usize,
        settings: LayoutSettings,
    ) {
        assert!(occupied_rows >= 1, "Occupied rows must be at least 1");
        assert!(occupied_columns >= 1, "Occupied columns must be at least 1");
        self.children.push(GridChild {
            wrapper: ChildWrapper::new(child, settings),
            row,
            column,
            occupied_rows,
            occupied_columns,
        });
    }

    /// `createRowHelper(int columns)` (`GridLayout.java`): fills cells
    /// left to right, wrapping to the next row.
    pub fn create_row_helper(&mut self, columns: usize) -> RowHelper<'_> {
        RowHelper {
            grid: self,
            columns,
            index: 0,
        }
    }
}

impl LayoutElement for GridLayout {
    fn x(&self) -> f32 {
        self.x as f32
    }

    fn y(&self) -> f32 {
        self.y as f32
    }

    fn width(&self) -> f32 {
        self.width as f32
    }

    fn height(&self) -> f32 {
        self.height as f32
    }

    /// `AbstractLayout.setX` (`AbstractLayout.java`): moving a layout moves
    /// every child by the same delta. This is what places a nested
    /// [`LinearLayout`]'s children after its parent has arranged it.
    fn set_x(&mut self, x: f32) {
        let new_x = ipx(x);
        let delta = (new_x - self.x) as f32;
        for child in &mut self.children {
            let cx = child.wrapper.child.x();
            child.wrapper.child.set_x(cx + delta);
        }
        self.x = new_x;
    }

    /// `AbstractLayout.setY` (`AbstractLayout.java`).
    fn set_y(&mut self, y: f32) {
        let new_y = ipx(y);
        let delta = (new_y - self.y) as f32;
        for child in &mut self.children {
            let cy = child.wrapper.child.y();
            child.wrapper.child.set_y(cy + delta);
        }
        self.y = new_y;
    }

    /// `GridLayout.arrangeElements` (`GridLayout.java`), transliterated.
    fn arrange_elements(&mut self) {
        // `super.arrangeElements()` — `Layout.arrangeElements`'s default body
        // (`Layout.java`): nested layouts size themselves *before* this grid
        // measures them, or every nested container would measure as 0×0. Written
        // as a direct walk of the child list, which is what `visitChildren` is.
        for child in &mut self.children {
            child.wrapper.child.arrange_elements();
        }

        let (row_spacing, column_spacing) = (self.row_spacing, self.column_spacing);
        let mut max_row = 0usize;
        let mut max_column = 0usize;
        for child in &self.children {
            max_row = max_row.max(child.last_occupied_row());
            max_column = max_column.max(child.last_occupied_column());
        }

        let mut max_column_widths = vec![0i32; max_column + 1];
        let mut max_row_heights = vec![0i32; max_row + 1];
        for child in &self.children {
            let child_height =
                child.wrapper.height() - (child.occupied_rows as i32 - 1) * row_spacing;
            let mut heights = Divisor::new(child_height, child.occupied_rows as i32);
            for row in child.row..=child.last_occupied_row() {
                max_row_heights[row] = max_row_heights[row].max(heights.next_int());
            }
            let child_width =
                child.wrapper.width() - (child.occupied_columns as i32 - 1) * column_spacing;
            let mut widths = Divisor::new(child_width, child.occupied_columns as i32);
            for column in child.column..=child.last_occupied_column() {
                max_column_widths[column] = max_column_widths[column].max(widths.next_int());
            }
        }

        let mut column_x_offsets = vec![0i32; max_column + 1];
        let mut row_y_offsets = vec![0i32; max_row + 1];
        for column in 1..=max_column {
            column_x_offsets[column] =
                column_x_offsets[column - 1] + max_column_widths[column - 1] + column_spacing;
        }
        for row in 1..=max_row {
            row_y_offsets[row] = row_y_offsets[row - 1] + max_row_heights[row - 1] + row_spacing;
        }

        let (grid_x, grid_y) = (self.x, self.y);
        for child in &mut self.children {
            let (row, column) = (child.row, child.column);
            let last_column = column + child.occupied_columns - 1;
            let last_row = row + child.occupied_rows - 1;
            let width_span: i32 = max_column_widths[column..=last_column].iter().sum();
            let available_width =
                width_span + column_spacing * (child.occupied_columns as i32 - 1);
            let height_span: i32 = max_row_heights[row..=last_row].iter().sum();
            let available_height = height_span + row_spacing * (child.occupied_rows as i32 - 1);
            let (cell_x, cell_y) = (grid_x + column_x_offsets[column], grid_y + row_y_offsets[row]);
            child.wrapper.set_x(cell_x, available_width);
            child.wrapper.set_y(cell_y, available_height);
        }

        self.width = column_x_offsets[max_column] + max_column_widths[max_column];
        self.height = row_y_offsets[max_row] + max_row_heights[max_row];
    }

    /// `Layout.visitWidgets`'s default (`Layout.java`): forward to children,
    /// which is what makes a whole tree reachable from one call.
    fn visit_widgets(&self, visitor: &mut dyn FnMut(&Widget)) {
        for child in &self.children {
            child.wrapper.child.visit_widgets(&mut *visitor);
        }
    }
}

impl Layout for GridLayout {
    fn visit_children(&self, visitor: &mut dyn FnMut(&dyn LayoutElement)) {
        for child in &self.children {
            visitor(child.wrapper.child.as_ref());
        }
    }

    fn visit_children_mut(&mut self, visitor: &mut dyn FnMut(&mut dyn LayoutElement)) {
        for child in &mut self.children {
            visitor(child.wrapper.child.as_mut());
        }
    }

    fn remove_children(&mut self) {
        self.children.clear();
    }
}

/// `GridLayout.RowHelper` (`GridLayout.java`): adds children left to
/// right, wrapping to the next row when the next span would not fit.
///
/// Every child it adds spans exactly **one row** — the helper only ever varies
/// the column span (`:219`).
#[derive(Debug)]
pub struct RowHelper<'a> {
    grid: &'a mut GridLayout,
    columns: usize,
    index: usize,
}

impl RowHelper<'_> {
    /// `addChild(widget)`: one column, baseline cell settings.
    pub fn add_child(&mut self, child: Box<dyn LayoutElement>) {
        let settings = self.grid.new_cell_settings();
        self.add_child_with(child, 1, settings);
    }

    /// `addChild(widget, int columnWidth)`: spans `column_width` columns,
    /// baseline cell settings.
    pub fn add_spanning(&mut self, child: Box<dyn LayoutElement>, column_width: usize) {
        let settings = self.grid.new_cell_settings();
        self.add_child_with(child, column_width, settings);
    }

    /// `addChild(widget, int columnWidth, LayoutSettings)` (`:209-220`).
    ///
    /// The wrap is the interesting part: when the span would overflow the row,
    /// the *rest of the current row is abandoned* and the index jumps to the next
    /// row boundary with `Mth.roundToward`, so a later 1-wide child lands on the
    /// row after the spanning one rather than beside it.
    pub fn add_child_with(
        &mut self,
        child: Box<dyn LayoutElement>,
        column_width: usize,
        settings: LayoutSettings,
    ) {
        let mut row = self.index / self.columns;
        let mut column_begin = self.index % self.columns;
        if column_begin + column_width > self.columns {
            row += 1;
            column_begin = 0;
            // `Mth.roundToward(index, columns)` == `positiveCeilDiv * multiple`.
            self.index = self.index.div_ceil(self.columns) * self.columns;
        }
        self.index += column_width;
        self.grid
            .add_child_with(child, row, column_begin, 1, column_width, settings);
    }

    /// `newCellSettings()` — a copy of the grid's baseline.
    #[must_use]
    pub fn new_cell_settings(&self) -> LayoutSettings {
        self.grid.new_cell_settings()
    }

    /// `defaultCellSetting()` — the grid's live baseline.
    pub fn default_cell_setting(&mut self) -> &mut LayoutSettings {
        self.grid.default_cell_setting()
    }
}

/// Vanilla's `FrameLayout` (`FrameLayout.java`): every child is aligned
/// independently inside one box, whose size is `max(minWidth/minHeight, the
/// largest padded child)`.
///
/// Its children default to **centred** (`align(0.5, 0.5)`,
/// `FrameLayout.java`), unlike [`GridLayout`]'s top-left. That default is the
/// whole reason `HeaderAndFooterLayout` centres a header title without saying so.
#[derive(Debug)]
pub struct FrameLayout {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    min_width: i32,
    min_height: i32,
    children: Vec<ChildWrapper>,
    default_child_settings: LayoutSettings,
}

impl Default for FrameLayout {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            min_width: 0,
            min_height: 0,
            children: Vec::new(),
            // `FrameLayout.java` — centred, not top-left.
            default_child_settings: LayoutSettings::defaults().align(0.5, 0.5),
        }
    }
}

impl FrameLayout {
    /// `new FrameLayout()`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `new FrameLayout(int minWidth, int minHeight)`.
    #[must_use]
    pub fn with_min_size(min_width: f32, min_height: f32) -> Self {
        Self {
            width: ipx(min_width),
            height: ipx(min_height),
            min_width: ipx(min_width),
            min_height: ipx(min_height),
            ..Self::default()
        }
    }

    /// `setMinWidth(int)`.
    pub fn set_min_width(&mut self, min_width: f32) {
        self.min_width = ipx(min_width);
    }

    /// `setMinHeight(int)`.
    pub fn set_min_height(&mut self, min_height: f32) {
        self.min_height = ipx(min_height);
    }

    /// `setMinDimensions(int, int)`.
    pub fn set_min_dimensions(&mut self, min_width: f32, min_height: f32) {
        self.set_min_width(min_width);
        self.set_min_height(min_height);
    }

    /// `defaultChildLayoutSetting()` — the live baseline, centred by default.
    pub fn default_child_layout_setting(&mut self) -> &mut LayoutSettings {
        &mut self.default_child_settings
    }

    /// `newChildLayoutSettings()` — a copy of it.
    #[must_use]
    pub fn new_child_layout_settings(&self) -> LayoutSettings {
        self.default_child_settings
    }

    /// `addChild(child)`.
    pub fn add_child(&mut self, child: Box<dyn LayoutElement>) {
        let settings = self.new_child_layout_settings();
        self.add_child_settings(child, settings);
    }

    /// `addChild(child, childLayoutSettings)`.
    pub fn add_child_settings(&mut self, child: Box<dyn LayoutElement>, settings: LayoutSettings) {
        self.children.push(ChildWrapper::new(child, settings));
    }
}

/// `FrameLayout.alignInDimension` (`FrameLayout.java`): the one-axis
/// half of aligning a whole block inside a rectangle. Returns the new position.
///
/// Note the truncation happens on the *offset*, before `pos` is added, and that
/// this is the expression a screen uses to place an already-arranged tree — it is
/// not the same code path as a cell's own alignment (which is padding-aware; see
/// the module docs).
#[must_use]
pub fn align_in_dimension(pos: f32, length: f32, widget_length: f32, align: f32) -> f32 {
    pos + trunc_int(lerp(align, 0.0, length - widget_length)) as f32
}

/// `FrameLayout.alignInRectangle` (`FrameLayout.java`): position an
/// element inside `(x, y, width, height)` at the given alignment.
///
/// This is what `PauseScreen.createPauseMenu` calls on the whole grid, with
/// `(0.5, 0.25)` — centred horizontally, a quarter of the way down
/// (`PauseScreen.java`).
pub fn align_in_rectangle(
    element: &mut dyn LayoutElement,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    align_x: f32,
    align_y: f32,
) {
    let new_x = align_in_dimension(x, width, element.width(), align_x);
    let new_y = align_in_dimension(y, height, element.height(), align_y);
    element.set_x(new_x);
    element.set_y(new_y);
}

/// `FrameLayout.centerInRectangle` (`FrameLayout.java`).
pub fn center_in_rectangle(
    element: &mut dyn LayoutElement,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) {
    align_in_rectangle(element, x, y, width, height, 0.5, 0.5);
}

impl LayoutElement for FrameLayout {
    fn x(&self) -> f32 {
        self.x as f32
    }

    fn y(&self) -> f32 {
        self.y as f32
    }

    fn width(&self) -> f32 {
        self.width as f32
    }

    fn height(&self) -> f32 {
        self.height as f32
    }

    fn set_x(&mut self, x: f32) {
        let new_x = ipx(x);
        let delta = (new_x - self.x) as f32;
        for child in &mut self.children {
            let cx = child.child.x();
            child.child.set_x(cx + delta);
        }
        self.x = new_x;
    }

    fn set_y(&mut self, y: f32) {
        let new_y = ipx(y);
        let delta = (new_y - self.y) as f32;
        for child in &mut self.children {
            let cy = child.child.y();
            child.child.set_y(cy + delta);
        }
        self.y = new_y;
    }

    /// `FrameLayout.arrangeElements` (`FrameLayout.java`): size is the
    /// largest padded child (floored at the minimum), and every child is then
    /// aligned in the *whole* box independently.
    fn arrange_elements(&mut self) {
        // `super.arrangeElements()` — see `GridLayout`'s.
        for child in &mut self.children {
            child.child.arrange_elements();
        }

        let mut result_width = self.min_width;
        let mut result_height = self.min_height;
        for child in &self.children {
            result_width = result_width.max(child.width());
            result_height = result_height.max(child.height());
        }
        let (x, y) = (self.x, self.y);
        for child in &mut self.children {
            child.set_x(x, result_width);
            child.set_y(y, result_height);
        }
        self.width = result_width;
        self.height = result_height;
    }

    fn visit_widgets(&self, visitor: &mut dyn FnMut(&Widget)) {
        for child in &self.children {
            child.child.visit_widgets(&mut *visitor);
        }
    }
}

impl Layout for FrameLayout {
    fn visit_children(&self, visitor: &mut dyn FnMut(&dyn LayoutElement)) {
        for child in &self.children {
            visitor(child.child.as_ref());
        }
    }

    fn visit_children_mut(&mut self, visitor: &mut dyn FnMut(&mut dyn LayoutElement)) {
        for child in &mut self.children {
            visitor(child.child.as_mut());
        }
    }

    fn remove_children(&mut self) {
        self.children.clear();
    }
}

/// `LinearLayout.Orientation` (`LinearLayout.java`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// One row: children advance in `column`, and `spacing` is `columnSpacing`.
    Horizontal,
    /// One column: children advance in `row`, and `spacing` is `rowSpacing`.
    Vertical,
}

/// Vanilla's `LinearLayout` (`LinearLayout.java`) — the container 46 of the 57
/// layout-using screens reach for, and **not its own algorithm**: it wraps a
/// one-row or one-column [`GridLayout`] and delegates every arrangement decision
/// to it.
#[derive(Debug)]
pub struct LinearLayout {
    wrapped: GridLayout,
    orientation: Orientation,
    next_child_index: usize,
}

impl LinearLayout {
    /// `LinearLayout.vertical()`.
    #[must_use]
    pub fn vertical() -> Self {
        Self::at(0.0, 0.0, Orientation::Vertical)
    }

    /// `LinearLayout.horizontal()`.
    #[must_use]
    pub fn horizontal() -> Self {
        Self::at(0.0, 0.0, Orientation::Horizontal)
    }

    /// `new LinearLayout(int x, int y, Orientation)`.
    #[must_use]
    pub fn at(x: f32, y: f32, orientation: Orientation) -> Self {
        Self {
            wrapped: GridLayout::at(x, y),
            orientation,
            next_child_index: 0,
        }
    }

    /// `spacing(int)`: `columnSpacing` when horizontal, `rowSpacing` when
    /// vertical (`LinearLayout.java`). Setting the wrong one is a no-op
    /// on a single-row grid, which is a silently unspaced row.
    #[must_use]
    pub fn spacing(mut self, spacing: i32) -> Self {
        // Written as a field assignment rather than `self.wrapped =
        // self.wrapped.column_spacing(..)` so the wrapped grid is never moved out
        // of `self`; the two are the same arithmetic.
        match self.orientation {
            Orientation::Horizontal => self.wrapped.column_spacing = spacing,
            Orientation::Vertical => self.wrapped.row_spacing = spacing,
        }
        self
    }

    /// `newCellSettings()`.
    #[must_use]
    pub fn new_cell_settings(&self) -> LayoutSettings {
        self.wrapped.new_cell_settings()
    }

    /// `defaultCellSetting()`.
    pub fn default_cell_setting(&mut self) -> &mut LayoutSettings {
        self.wrapped.default_cell_setting()
    }

    /// `addChild(child)`.
    pub fn add_child(&mut self, child: Box<dyn LayoutElement>) {
        let settings = self.new_cell_settings();
        self.add_child_settings(child, settings);
    }

    /// `addChild(child, cellSettings)`.
    pub fn add_child_settings(&mut self, child: Box<dyn LayoutElement>, settings: LayoutSettings) {
        let index = self.next_child_index;
        self.next_child_index += 1;
        match self.orientation {
            Orientation::Horizontal => self.wrapped.add_child_settings(child, 0, index, settings),
            Orientation::Vertical => self.wrapped.add_child_settings(child, index, 0, settings),
        }
    }
}

impl LayoutElement for LinearLayout {
    fn x(&self) -> f32 {
        self.wrapped.x()
    }

    fn y(&self) -> f32 {
        self.wrapped.y()
    }

    fn width(&self) -> f32 {
        self.wrapped.width()
    }

    fn height(&self) -> f32 {
        self.wrapped.height()
    }

    fn set_x(&mut self, x: f32) {
        self.wrapped.set_x(x);
    }

    fn set_y(&mut self, y: f32) {
        self.wrapped.set_y(y);
    }

    fn arrange_elements(&mut self) {
        self.wrapped.arrange_elements();
    }

    fn visit_widgets(&self, visitor: &mut dyn FnMut(&Widget)) {
        self.wrapped.visit_widgets(visitor);
    }
}

impl Layout for LinearLayout {
    fn visit_children(&self, visitor: &mut dyn FnMut(&dyn LayoutElement)) {
        self.wrapped.visit_children(visitor);
    }

    fn visit_children_mut(&mut self, visitor: &mut dyn FnMut(&mut dyn LayoutElement)) {
        self.wrapped.visit_children_mut(visitor);
    }

    fn remove_children(&mut self) {
        self.wrapped.remove_children();
        self.next_child_index = 0;
    }
}

/// `HeaderAndFooterLayout.MAGIC_PADDING` (`HeaderAndFooterLayout.java`).
pub const MAGIC_PADDING: f32 = 13.0;
/// `HeaderAndFooterLayout.DEFAULT_HEADER_AND_FOOTER_HEIGHT` (`:11`).
pub const DEFAULT_HEADER_AND_FOOTER_HEIGHT: f32 = 33.0;
/// `HeaderAndFooterLayout.CONTENT_MARGIN_TOP` (`:12`): the gap the content
/// *prefers* below the header, before clamping.
pub const CONTENT_MARGIN_TOP: f32 = 30.0;

/// Vanilla's `HeaderAndFooterLayout` (`HeaderAndFooterLayout.java`): three
/// [`FrameLayout`]s — header pinned at the top, footer pinned at the bottom,
/// content between them.
///
/// It is the base of `OptionsSubScreen` (`OptionsSubScreen.java`), so **every**
/// settings sub-screen inherits it, which is why it is ported now even though the
/// screen that consumes it is #55/#396. Nothing in this commit builds one outside
/// its tests — that is a deliberate exception to this repo's island rule, taken
/// because the arithmetic is exactly the part that is expensive to rediscover and
/// cheap to get subtly wrong.
///
/// The trap named in #394 is real: **the content rect depends on the header and
/// footer heights, which screens set after construction**
/// (`setHeaderHeight`/`setFooterHeight`). Nothing is computed until
/// `arrange_elements`, so reading a content rect earlier gives a plausible,
/// stable, wrong answer. Vanilla dodges it by reading `this.screen.width/height`
/// live; we take the screen size as constructor arguments plus
/// [`Self::set_screen_size`], because this shell has no `Screen` object to hold —
/// and because the canvas is only known at draw time (`render::logical_canvas`).
#[derive(Debug)]
pub struct HeaderAndFooterLayout {
    screen_width: i32,
    screen_height: i32,
    header_height: i32,
    footer_height: i32,
    header: FrameLayout,
    contents: FrameLayout,
    footer: FrameLayout,
}

impl HeaderAndFooterLayout {
    /// `new HeaderAndFooterLayout(screen)`: 33 px header and footer.
    #[must_use]
    pub fn new(screen_width: f32, screen_height: f32) -> Self {
        Self::with_heights(
            screen_width,
            screen_height,
            DEFAULT_HEADER_AND_FOOTER_HEIGHT,
            DEFAULT_HEADER_AND_FOOTER_HEIGHT,
        )
    }

    /// `new HeaderAndFooterLayout(screen, headerHeight, footerHeight)`.
    ///
    /// Vanilla's constructor also calls `align(0.5, 0.5)` on the header's and
    /// footer's default child settings (`:32-33`) — which is already
    /// [`FrameLayout`]'s own default, so it is a no-op there and omitted here.
    /// The consequence is worth knowing: the *contents* frame centres its
    /// children too, because nobody ever changes it.
    #[must_use]
    pub fn with_heights(
        screen_width: f32,
        screen_height: f32,
        header_height: f32,
        footer_height: f32,
    ) -> Self {
        Self {
            screen_width: ipx(screen_width),
            screen_height: ipx(screen_height),
            header_height: ipx(header_height),
            footer_height: ipx(footer_height),
            header: FrameLayout::new(),
            contents: FrameLayout::new(),
            footer: FrameLayout::new(),
        }
    }

    /// The canvas this layout arranges against. Vanilla reads `screen.width` and
    /// `screen.height` live; this is the seam that replaces it, and it must be
    /// called before `arrange_elements` on a resize.
    pub fn set_screen_size(&mut self, width: f32, height: f32) {
        self.screen_width = ipx(width);
        self.screen_height = ipx(height);
    }

    /// `getHeaderHeight()`.
    #[must_use]
    pub fn header_height(&self) -> f32 {
        self.header_height as f32
    }

    /// `setHeaderHeight(int)`.
    pub fn set_header_height(&mut self, height: f32) {
        self.header_height = ipx(height);
    }

    /// `getFooterHeight()`.
    #[must_use]
    pub fn footer_height(&self) -> f32 {
        self.footer_height as f32
    }

    /// `setFooterHeight(int)`.
    pub fn set_footer_height(&mut self, height: f32) {
        self.footer_height = ipx(height);
    }

    /// `getContentHeight()`: what is left of the screen between the two bands.
    #[must_use]
    pub fn content_height(&self) -> f32 {
        (self.screen_height - self.header_height - self.footer_height) as f32
    }

    /// `addToHeader(child)`.
    pub fn add_to_header(&mut self, child: Box<dyn LayoutElement>) {
        self.header.add_child(child);
    }

    /// `addToContents(child)`.
    pub fn add_to_contents(&mut self, child: Box<dyn LayoutElement>) {
        self.contents.add_child(child);
    }

    /// `addToFooter(child)`.
    pub fn add_to_footer(&mut self, child: Box<dyn LayoutElement>) {
        self.footer.add_child(child);
    }

    /// The header band, for reading its arranged rect.
    #[must_use]
    pub fn header(&self) -> &FrameLayout {
        &self.header
    }

    /// The content band.
    #[must_use]
    pub fn contents(&self) -> &FrameLayout {
        &self.contents
    }

    /// The footer band.
    #[must_use]
    pub fn footer(&self) -> &FrameLayout {
        &self.footer
    }
}

impl LayoutElement for HeaderAndFooterLayout {
    /// `getX()` is a hardcoded `0` in vanilla (`:44-47`) — this layout *is* the
    /// screen.
    fn x(&self) -> f32 {
        0.0
    }

    /// `getY()` is a hardcoded `0` (`:49-52`).
    fn y(&self) -> f32 {
        0.0
    }

    /// `getWidth()` is the screen's width (`:54-57`), not a measured size.
    fn width(&self) -> f32 {
        self.screen_width as f32
    }

    /// `getHeight()` is the screen's height (`:59-62`).
    fn height(&self) -> f32 {
        self.screen_height as f32
    }

    /// `setX` is an **empty body** in vanilla (`:36-38`): this layout cannot be
    /// moved, because it is pinned to the screen.
    fn set_x(&mut self, _x: f32) {}

    /// `setY` is likewise empty (`:40-42`).
    fn set_y(&mut self, _y: f32) {}

    /// `HeaderAndFooterLayout.arrangeElements` (`:98-115`).
    ///
    /// The order matters twice: the header is *positioned* before it is arranged
    /// (children are placed relative to the frame's own origin), and the footer is
    /// arranged before it is moved (`setY` translates children by a delta, so it
    /// needs them placed first).
    fn arrange_elements(&mut self) {
        let header_height = self.header_height;
        let footer_height = self.footer_height;
        let screen_width = self.screen_width as f32;
        let screen_height = self.screen_height;

        self.header.set_min_width(screen_width);
        self.header.set_min_height(header_height as f32);
        self.header.set_position(0.0, 0.0);
        self.header.arrange_elements();

        self.footer.set_min_width(screen_width);
        self.footer.set_min_height(footer_height as f32);
        self.footer.arrange_elements();
        self.footer.set_y((screen_height - footer_height) as f32);

        self.contents.set_min_width(screen_width);
        self.contents.arrange_elements();
        // The content *prefers* a 30 px gap under the header and is clamped
        // upward so it can never overlap the footer — `Math.min`, which reads
        // like a maximum until you notice y grows downward.
        let preferred_content_y = header_height + ipx(CONTENT_MARGIN_TOP);
        let max_content_y = screen_height - footer_height - ipx(self.contents.height());
        self.contents
            .set_position(0.0, preferred_content_y.min(max_content_y) as f32);
    }

    /// Header, then contents, then footer (`:84-89`) — the order a screen
    /// registers its widgets in, and therefore tab order in vanilla.
    fn visit_widgets(&self, visitor: &mut dyn FnMut(&Widget)) {
        self.header.visit_widgets(&mut *visitor);
        self.contents.visit_widgets(&mut *visitor);
        self.footer.visit_widgets(&mut *visitor);
    }
}

impl Layout for HeaderAndFooterLayout {
    fn visit_children(&self, visitor: &mut dyn FnMut(&dyn LayoutElement)) {
        visitor(&self.header as &dyn LayoutElement);
        visitor(&self.contents as &dyn LayoutElement);
        visitor(&self.footer as &dyn LayoutElement);
    }

    fn visit_children_mut(&mut self, visitor: &mut dyn FnMut(&mut dyn LayoutElement)) {
        visitor(&mut self.header as &mut dyn LayoutElement);
        visitor(&mut self.contents as &mut dyn LayoutElement);
        visitor(&mut self.footer as &mut dyn LayoutElement);
    }

    fn remove_children(&mut self) {
        self.header.remove_children();
        self.contents.remove_children();
        self.footer.remove_children();
    }
}

/// Vanilla's `SpacerElement` (`SpacerElement.java`): a fixed-size hole in a
/// layout.
///
/// It implements only `LayoutElement`, and **its `visitWidgets` is a no-op**
/// (`:61-63`), so a spacer takes part in every measurement and reaches a screen's
/// renderable list never. That is the mechanism behind vanilla's empty grid
/// cells; it is not a widget with no art.
///
/// The constructors are `of_width`/`of_height` rather than vanilla's
/// `width`/`height` because those names collide with
/// [`LayoutElement::width`]/[`height`](LayoutElement::height) on the same type.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SpacerElement {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl SpacerElement {
    /// `new SpacerElement(width, height)`.
    #[must_use]
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            x: 0,
            y: 0,
            width: ipx(width),
            height: ipx(height),
        }
    }

    /// `SpacerElement.width(int)`: horizontal only.
    #[must_use]
    pub fn of_width(width: f32) -> Self {
        Self::new(width, 0.0)
    }

    /// `SpacerElement.height(int)`: vertical only.
    #[must_use]
    pub fn of_height(height: f32) -> Self {
        Self::new(0.0, height)
    }
}

impl LayoutElement for SpacerElement {
    fn x(&self) -> f32 {
        self.x as f32
    }

    fn y(&self) -> f32 {
        self.y as f32
    }

    fn width(&self) -> f32 {
        self.width as f32
    }

    fn height(&self) -> f32 {
        self.height as f32
    }

    fn set_x(&mut self, x: f32) {
        self.x = ipx(x);
    }

    fn set_y(&mut self, y: f32) {
        self.y = ipx(y);
    }

    /// The no-op that is the whole point of the type.
    fn visit_widgets(&self, _visitor: &mut dyn FnMut(&Widget)) {}
}

// -- the tab widget's row geometry ------------------------------
//
// `MenuTabBar.arrangeElements` (`MenuTabBar.java`) — not a
// `LayoutElement`, unlike everything above: a `MenuTabBar` positions its own
// children directly rather than through `LinearLayout`/`GridLayout`
// arithmetic, so this is transcribed as a free function instead of a type.

/// `MenuTabBar.HEIGHT` (`MenuTabBar.java`).
pub const TAB_BAR_HEIGHT: f32 = 24.0;
/// `MenuTabBar.MAX_WIDTH` (`:18`).
const TAB_BAR_MAX_WIDTH: f32 = 400.0;

/// `Mth.roundToward(int, int)` (`Mth.java`): round `value` **up** to
/// the nearest multiple of `multiple`, via `positiveCeilDiv`.
///
/// Transcribed for the `f32` geometry this crate uses everywhere else rather
/// than reproducing vanilla's `int` type. `positiveCeilDiv` exists to get the
/// sign right for a *negative* `value` (`-Math.floorDiv(-input, divisor)`);
/// every call site in [`tab_bar_geometry`] passes a non-negative `value` (a
/// tab's share of a canvas width, or the canvas's own centring remainder,
/// which [`tab_bar_geometry`]'s own doc shows is always positive), so the
/// plain `ceil` division below is the same function on the domain this crate
/// actually evaluates it on.
#[must_use]
pub fn round_toward(value: f32, multiple: f32) -> f32 {
    (value / multiple).ceil() * multiple
}

/// `MenuTabBar.arrangeElements(width)` (`MenuTabBar.java`): the tab
/// row's own left edge and each tab's (equal) width, for `tab_count` tabs
/// spread across a canvas `width` px wide.
///
/// Returns `(start_x, tab_width)`; tab `i`'s rect is
/// `(start_x + tab_width * i, 0.0, tab_width, `[`TAB_BAR_HEIGHT`]`)`.
///
/// Vanilla's `tabsWidth` is `min(400, width) - 28` (`MARGIN = 14` either
/// side), each tab gets an equal, 2 px-rounded share of it, and the row is
/// then centred (also 2 px-rounded) rather than flush left — `roundToward`
/// always rounds *up*, so the centring is asymmetric by construction on an
/// odd remainder, exactly as vanilla's is. `tabs_width < width` always holds
/// (`min(400, width) - 28 < width` for any `width > -28`), so
/// `width - tabs_width` — [`round_toward`]'s input here — is always positive.
#[must_use]
pub fn tab_bar_geometry(width: f32, tab_count: usize) -> (f32, f32) {
    let tabs_width = (width.min(TAB_BAR_MAX_WIDTH) - 28.0).max(0.0);
    let count = (tab_count.max(1)) as f32;
    let tab_width = round_toward(tabs_width / count, 2.0);
    let start_x = round_toward((width - tabs_width) / 2.0, 2.0);
    (start_x, tab_width)
}

/// The pixel rect of tab `index` of `count` at canvas `width` — [`tab_bar_geometry`]
/// plus the per-tab offset, in one place so a **second** screen's tab bar cannot
/// drift from the first's arithmetic.
///
/// This is what makes the tab widget actually shared rather than merely
/// duplicated: before this existed, [`super::render::row_rect`]'s `MenuRow::tab`
/// arm called `super::stats::tab_row_rect` directly, so a second consumer of the
/// same [`super::render::TabEntryView`] (Create New World, issue #567) had no
/// generic geometry to resolve against — only Statistics's own screen-specific
/// wrapper. Both screens' own `tab_row_rect` helpers now call this.
#[must_use]
pub fn tab_bar_row_rect(index: usize, count: usize, width: f32) -> (f32, f32, f32, f32) {
    let (start_x, tab_width) = tab_bar_geometry(width, count);
    (start_x + tab_width * index as f32, 0.0, tab_width, TAB_BAR_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w`×`h` button, boxed for a layout. Labels do not affect geometry —
    /// every vanilla menu `Button` is built with an explicit `.width(n)` — so
    /// the trees below carry no text.
    fn cell(w: f32, h: f32) -> Box<dyn LayoutElement> {
        Box::new(Widget::button(0.0, 0.0, w, h, ""))
    }

    #[test]
    fn set_x_is_padding_aware_not_a_naive_centre() {
        // `AbstractLayout.AbstractChildWrapper::setX` (`AbstractLayout.java`)
        // interpolates between `paddingLeft` and
        // `availableSpace - child.width - paddingRight`, so padding *biases* the
        // alignment rather than shrinking a box that is then centred.
        //
        // A `FrameLayout` is the shortest way to give a child a known amount of
        // available space: its box is `max(minWidth, largest padded child)`.
        let mut frame = FrameLayout::with_min_size(100.0, 40.0);
        frame.add_child_settings(
            cell(20.0, 20.0),
            LayoutSettings::defaults()
                .padding_ltrb(10, 0, 0, 0)
                .align(0.5, 0.0),
        );
        frame.arrange_elements();
        let placed = widget_rects(&frame);
        // lerp(0.5, 10, 100 - 20 - 0) = 45.
        assert_eq!(placed[0].0, 45.0, "padding must bias the alignment");
        // The naive reading — centre the child in what is left — is 40, and the
        // *other* naive reading, centring in the full box, is also 40. Both are
        // wrong and both are what a from-memory port writes.
        assert_ne!(placed[0].0, 40.0);
    }

    #[test]
    fn set_y_rounds_where_set_x_truncates() {
        // The asymmetry is in the jar: `setX` casts (`(int)Mth.lerp(..)`,
        // `AbstractLayout.java`) and `setY` rounds (`Math.round(Mth.lerp(..))`,
        // `:83`). A 20 px child centred in a 25 px box has a half-pixel offset,
        // so the two axes disagree by one.
        let mut frame = FrameLayout::with_min_size(25.0, 25.0);
        frame.add_child(cell(20.0, 20.0)); // FrameLayout centres by default.
        frame.arrange_elements();
        let (x, y, ..) = widget_rects(&frame)[0];
        assert_eq!((x, y), (2.0, 3.0), "x truncates 2.5, y rounds it");
    }

    #[test]
    fn frame_children_default_to_centred_and_grid_children_to_top_left() {
        // `FrameLayout.java` vs `GridLayout.java`. Getting this backwards
        // moves an entire screen and looks intentional in a screenshot.
        let mut frame = FrameLayout::with_min_size(100.0, 100.0);
        frame.add_child(cell(20.0, 20.0));
        frame.arrange_elements();
        assert_eq!(widget_rects(&frame)[0], (40.0, 40.0, 20.0, 20.0));

        let mut grid = GridLayout::new();
        grid.add_child(cell(20.0, 20.0), 0, 0);
        grid.add_child(cell(80.0, 40.0), 0, 1);
        grid.arrange_elements();
        // The tall/wide neighbour gives cell (0,0) 80 px of column and 40 px of
        // row to sit in; a top-left default leaves it at the origin.
        assert_eq!(widget_rects(&grid)[0], (0.0, 0.0, 20.0, 20.0));
    }

    #[test]
    fn frame_size_is_the_largest_padded_child_or_the_minimum() {
        let mut frame = FrameLayout::new();
        frame.add_child_settings(cell(20.0, 20.0), LayoutSettings::defaults().padding(5));
        frame.add_child(cell(40.0, 10.0));
        frame.arrange_elements();
        // max(20 + 5 + 5, 40) = 40 wide; max(20 + 5 + 5, 10) = 30 tall.
        assert_eq!((frame.width(), frame.height()), (40.0, 30.0));

        let mut floored = FrameLayout::with_min_size(200.0, 200.0);
        floored.add_child(cell(20.0, 20.0));
        floored.arrange_elements();
        assert_eq!((floored.width(), floored.height()), (200.0, 200.0));
    }

    #[test]
    fn grid_dimensions_are_derived_from_the_highest_occupied_index() {
        // `GridLayout.java`: nothing declares a row or column count, and
        // the cells in between exist at size zero.
        let mut grid = GridLayout::new();
        grid.add_child(cell(30.0, 10.0), 3, 2);
        grid.arrange_elements();
        // Rows 0..3 and columns 0..2 exist; only the last of each has size.
        assert_eq!((grid.width(), grid.height()), (30.0, 10.0));
        assert_eq!(widget_rects(&grid)[0], (0.0, 0.0, 30.0, 10.0));

        // And a child in an earlier row is not pushed down by the empty ones.
        let mut sparse = GridLayout::new();
        sparse.add_child(cell(10.0, 10.0), 0, 0);
        sparse.add_child(cell(10.0, 10.0), 2, 0);
        sparse.arrange_elements();
        let rects = widget_rects(&sparse);
        assert_eq!(rects[0].1, 0.0);
        assert_eq!(rects[1].1, 10.0, "row 1 exists but is 0 px tall");
        assert_eq!(sparse.height(), 20.0, "two 10 px rows and one empty one");
    }

    #[test]
    fn divisor_matches_mojangs_bresenham_sequence() {
        // `com/mojang/math/Divisor.java`. The remainder goes to the *later*
        // parts, and the parts always sum back to the numerator — which is what
        // stops a spanning cell from being a pixel wider or narrower than its
        // columns.
        assert_eq!(Divisor::new(7, 3).collect::<Vec<_>>(), vec![2, 2, 3]);
        assert_eq!(Divisor::new(212, 2).collect::<Vec<_>>(), vec![106, 106]);
        assert_eq!(Divisor::new(7, 2).collect::<Vec<_>>(), vec![3, 4]);
        assert_eq!(Divisor::new(5, 1).collect::<Vec<_>>(), vec![5]);
        // A naive `numerator / denominator` would give [2, 2, 2] for the first
        // case and lose a pixel.
        for (n, d) in [(7, 3), (212, 2), (7, 2), (100, 7), (1, 4)] {
            assert_eq!(Divisor::new(n, d).sum::<i32>(), n, "{n}/{d} lost a part");
        }
        // A non-positive denominator yields nothing rather than dividing by zero.
        assert_eq!(Divisor::new(9, 0).collect::<Vec<_>>(), Vec::<i32>::new());
    }

    #[test]
    fn a_spanning_cell_splits_its_size_and_only_grows_a_column_that_is_smaller() {
        // `GridLayout.java`: the span's share is `Math.max`ed into each
        // column, so a wide span sets the columns only when nothing else is
        // wider.
        let mut grid = GridLayout::new();
        grid.add_child_with(cell(7.0, 10.0), 0, 0, 1, 2, LayoutSettings::defaults());
        grid.add_child(cell(10.0, 10.0), 1, 0);
        grid.arrange_elements();
        // Column 0 wants max(3, 10) = 10; column 1 gets the span's 4.
        assert_eq!(grid.width(), 14.0);
        let rects = widget_rects(&grid);
        assert_eq!(rects[0].0, 0.0);
        assert_eq!(rects[1].0, 0.0);

        // With nothing else in the grid the span alone sets both columns.
        let mut alone = GridLayout::new();
        alone.add_child_with(cell(7.0, 10.0), 0, 0, 1, 2, LayoutSettings::defaults());
        alone.arrange_elements();
        assert_eq!(alone.width(), 7.0, "3 + 4, not 4 + 4");
    }

    #[test]
    fn linear_layout_is_a_one_row_or_one_column_grid() {
        // `LinearLayout.java`, and `spacing` maps to the axis's own
        // spacing (`:103-111`). Both directions, because a `columnSpacing` set on
        // a vertical layout is a silent no-op.
        let mut column = LinearLayout::vertical().spacing(4);
        for _ in 0..3 {
            column.add_child(cell(200.0, 20.0));
        }
        column.arrange_elements();
        assert_eq!((column.width(), column.height()), (200.0, 68.0));
        let ys: Vec<f32> = widget_rects(&column).iter().map(|r| r.1).collect();
        assert_eq!(ys, vec![0.0, 24.0, 48.0]);
        assert!(widget_rects(&column).iter().all(|r| r.0 == 0.0));

        let mut row = LinearLayout::horizontal().spacing(4);
        for _ in 0..3 {
            row.add_child(cell(20.0, 20.0));
        }
        row.arrange_elements();
        assert_eq!((row.width(), row.height()), (68.0, 20.0));
        let xs: Vec<f32> = widget_rects(&row).iter().map(|r| r.0).collect();
        assert_eq!(xs, vec![0.0, 24.0, 48.0]);
        assert!(widget_rects(&row).iter().all(|r| r.1 == 0.0));

        // The control: without spacing the children abut, so the 4s above are
        // measuring the spacing and not the widths.
        let mut tight = LinearLayout::horizontal();
        for _ in 0..3 {
            tight.add_child(cell(20.0, 20.0));
        }
        tight.arrange_elements();
        assert_eq!(tight.width(), 60.0);
    }

    #[test]
    fn row_helper_wraps_and_abandons_the_rest_of_the_row() {
        // `GridLayout.RowHelper.addChild` (`GridLayout.java`). Two
        // columns: a 1-wide child, then a 2-wide one that cannot fit beside it,
        // then another 1-wide. The 2-wide starts a new row, and `roundToward`
        // pushes the *third* child to the row after that rather than beside the
        // spanning one.
        let mut grid = GridLayout::new();
        {
            let mut helper = grid.create_row_helper(2);
            helper.add_child(cell(10.0, 10.0));
            helper.add_spanning(cell(30.0, 10.0), 2);
            helper.add_child(cell(10.0, 10.0));
        }
        grid.arrange_elements();
        let ys: Vec<f32> = widget_rects(&grid).iter().map(|r| r.1).collect();
        assert_eq!(
            ys,
            vec![0.0, 10.0, 20.0],
            "the spanning child gets its own row and the next child a fresh one"
        );
        // Every helper child spans exactly one row (`:219`), so three children
        // over three rows is three 10 px rows and nothing taller.
        assert_eq!(grid.height(), 30.0);
    }

    #[test]
    fn nested_layouts_are_arranged_before_the_parent_measures_them() {
        // `Layout.arrangeElements`'s default body (`Layout.java`) recurses
        // first. Without it a nested container measures 0×0 and every column it
        // is in collapses — which looks like a missing widget, not a missing
        // recursion.
        let mut inner = LinearLayout::horizontal().spacing(4);
        for _ in 0..3 {
            inner.add_child(cell(20.0, 20.0));
        }
        assert_eq!(inner.width(), 0.0, "unarranged, it has no size yet");

        let mut outer = GridLayout::new();
        outer.add_child(Box::new(inner), 0, 0);
        outer.arrange_elements();
        assert_eq!(outer.width(), 68.0, "the parent must arrange it first");
        // And the leaves are reachable through the nesting, in insertion order.
        let xs: Vec<f32> = widget_rects(&outer).iter().map(|r| r.0).collect();
        assert_eq!(xs, vec![0.0, 24.0, 48.0]);
    }

    #[test]
    fn visit_children_yields_the_children_where_visit_widgets_yields_the_leaves() {
        // `Layout.visitChildren` yields the immediate children — a nested layout
        // counts as *one* — while `visitWidgets` recurses to the leaves
        // (`Layout.java`). Both readings are load-bearing: the pause
        // screen's icon row is one grid cell and four drawable buttons.
        let mut inner = LinearLayout::horizontal().spacing(4);
        for _ in 0..3 {
            inner.add_child(cell(20.0, 20.0));
        }
        let mut outer = GridLayout::new();
        outer.add_child(cell(200.0, 20.0), 0, 0);
        outer.add_child(Box::new(inner), 1, 0);
        outer.arrange_elements();

        let mut children = 0usize;
        outer.visit_children(&mut |_| children += 1);
        assert_eq!(children, 2, "the nested row is one child");
        assert_eq!(widget_rects(&outer).len(), 4, "and four leaves");

        // The mutable walk is what `AbstractLayout.setX` uses, so moving every
        // child through it has to move the nested row's leaves too.
        outer.visit_children_mut(&mut |child| {
            let x = child.x();
            child.set_x(x + 5.0);
        });
        let xs: Vec<f32> = widget_rects(&outer).iter().map(|r| r.0).collect();
        assert_eq!(xs, vec![5.0, 5.0, 29.0, 53.0]);
    }

    #[test]
    fn moving_a_layout_moves_its_children_by_the_same_delta() {
        // `AbstractLayout.setX`/`setY` (`AbstractLayout.java`). This is the
        // mechanism that positions an already-arranged nested layout, and the one
        // `FrameLayout.alignInRectangle` uses on a whole screen's tree.
        let mut row = LinearLayout::horizontal().spacing(4);
        for _ in 0..2 {
            row.add_child(cell(20.0, 20.0));
        }
        row.arrange_elements();
        assert_eq!(widget_rects(&row)[1].0, 24.0);
        row.set_position(100.0, 50.0);
        let rects = widget_rects(&row);
        assert_eq!(rects[0], (100.0, 50.0, 20.0, 20.0));
        assert_eq!(rects[1], (124.0, 50.0, 20.0, 20.0));
        // Idempotent: setting the same position again must not double the shift.
        row.set_position(100.0, 50.0);
        assert_eq!(widget_rects(&row)[1].0, 124.0);
    }

    #[test]
    fn align_in_rectangle_truncates_the_offset_before_adding_the_origin() {
        // `FrameLayout.alignInDimension` (`FrameLayout.java`). This is
        // the *screen*-level align, distinct from a cell's padding-aware one.
        let mut block = LinearLayout::vertical();
        block.add_child(cell(200.0, 20.0));
        block.arrange_elements();
        align_in_rectangle(&mut block, 0.0, 0.0, 854.0, 480.0, 0.5, 0.25);
        // (854 - 200) / 2 = 327; (480 - 20) * 0.25 = 115.
        assert_eq!(widget_rects(&block)[0], (327.0, 115.0, 200.0, 20.0));

        // A half-pixel offset truncates rather than rounding — the odd-width
        // case, which is the only place this differs from a floor.
        assert_eq!(align_in_dimension(0.0, 855.0, 200.0, 0.5), 327.0);
        assert_eq!(align_in_dimension(0.0, 200.0, 200.0, 0.5), 0.0);
        // A block wider than its rectangle overhangs to the left, negatively.
        assert_eq!(align_in_dimension(0.0, 100.0, 200.0, 0.5), -50.0);
    }

    #[test]
    fn header_is_pinned_top_footer_bottom_and_content_prefers_a_thirty_pixel_gap() {
        // `HeaderAndFooterLayout.arrangeElements` (`:98-115`), both branches of
        // the clamp. Screen 400×200, default 33 px bands.
        let mut layout = HeaderAndFooterLayout::new(400.0, 200.0);
        layout.add_to_header(cell(100.0, 9.0));
        layout.add_to_contents(cell(200.0, 60.0));
        layout.add_to_footer(cell(200.0, 20.0));
        layout.arrange_elements();

        assert_eq!(
            (layout.header().x(), layout.header().y()),
            (0.0, 0.0),
            "the header is pinned at the origin"
        );
        assert_eq!(
            layout.header().width(),
            400.0,
            "and stretched to the screen's width"
        );
        assert_eq!(layout.header().height(), 33.0);
        assert_eq!(
            layout.footer().y(),
            200.0 - 33.0,
            "the footer is pinned to the bottom band"
        );
        // 33 + 30 = 63 preferred; the clamp is 200 - 33 - 60 = 107, so the
        // preference wins.
        assert_eq!(layout.contents().y(), 63.0);
        assert_eq!(layout.content_height(), 200.0 - 66.0);

        // The other branch: content tall enough that a 30 px gap would push it
        // into the footer, so it is clamped *upward*.
        let mut tall = HeaderAndFooterLayout::new(400.0, 200.0);
        tall.add_to_contents(cell(200.0, 150.0));
        tall.arrange_elements();
        assert_eq!(
            tall.contents().y(),
            200.0 - 33.0 - 150.0,
            "content must never overlap the footer"
        );
        assert!(
            tall.contents().y() < 63.0,
            "the clamp has to be able to beat the preference, or it is untested"
        );
    }

    #[test]
    fn header_and_footer_children_are_centred_in_their_band() {
        // The consequence of `FrameLayout`'s centred default (which vanilla's
        // constructor restates for the header and footer, `:32-33`): a title or a
        // Done button needs no alignment call to land in the middle.
        let mut layout = HeaderAndFooterLayout::new(400.0, 200.0);
        layout.add_to_header(cell(100.0, 10.0));
        layout.add_to_footer(cell(200.0, 20.0));
        layout.arrange_elements();
        let rects = widget_rects(&layout);
        // Header first, then contents, then footer (`:84-89`).
        assert_eq!(rects.len(), 2);
        // x = lerp(0.5, 0, 400 - 100) = 150; y = round(lerp(0.5, 0, 33 - 10)) =
        // round(11.5) = 12 — the `setY` rounding, which a truncating port would
        // put at 11.
        assert_eq!(rects[0], (150.0, 12.0, 100.0, 10.0), "header child centred");
        assert_eq!(
            rects[1],
            (100.0, 174.0, 200.0, 20.0),
            "footer child centred in the bottom band"
        );
    }

    #[test]
    fn header_and_footer_heights_set_after_construction_change_the_content_rect() {
        // #394's named trap: the content rect depends on band heights a screen
        // sets *later*, so nothing may be computed before `arrange_elements`.
        let mut layout = HeaderAndFooterLayout::new(400.0, 200.0);
        layout.add_to_contents(cell(200.0, 60.0));
        layout.set_header_height(50.0);
        layout.set_footer_height(10.0);
        layout.arrange_elements();
        assert_eq!(layout.contents().y(), 80.0, "50 + 30");
        assert_eq!(layout.footer().y(), 190.0);
        assert_eq!(layout.content_height(), 140.0);
    }

    #[test]
    fn a_spacer_occupies_space_and_reaches_no_widget_list() {
        // `SpacerElement.visitWidgets` is empty (`SpacerElement.java`), so
        // a spacer is measured and never drawn. This is the mechanism, not an
        // accident of having no art.
        let mut column = LinearLayout::vertical();
        column.add_child(Box::new(SpacerElement::of_height(30.0)));
        column.add_child(cell(200.0, 20.0));
        column.arrange_elements();
        let rects = widget_rects(&column);
        assert_eq!(rects.len(), 1, "the spacer must not reach the widget list");
        assert_eq!(
            rects[0].1, 30.0,
            "but it must still push the widget down by its height"
        );
        // The control: the same tree without the spacer puts the widget at 0.
        let mut bare = LinearLayout::vertical();
        bare.add_child(cell(200.0, 20.0));
        bare.arrange_elements();
        assert_eq!(widget_rects(&bare)[0].1, 0.0);
    }

    #[test]
    fn the_default_cell_setting_is_a_live_baseline_and_new_cell_settings_is_a_copy() {
        // `GridLayout.java`. The baseline is what every subsequent cell
        // inherits; a copy is what one cell adjusts.
        let mut grid = GridLayout::new();
        {
            let baseline = grid.default_cell_setting();
            *baseline = baseline.padding_ltrb(4, 4, 4, 0);
        }
        // A copy, adjusted for one cell only.
        let one_off = grid.new_cell_settings().padding_top(50);
        assert_eq!(one_off.padding_left, 4, "the copy keeps the baseline");
        assert_eq!(one_off.padding_top, 50);
        assert_eq!(
            grid.new_cell_settings().padding_top,
            4,
            "and adjusting the copy must not have touched the baseline"
        );

        // Both reach the arrangement: the baseline child gets 4 px of top
        // padding, the adjusted one 50.
        grid.add_child(cell(100.0, 20.0), 0, 0);
        grid.add_child_settings(cell(100.0, 20.0), 1, 0, one_off);
        grid.arrange_elements();
        let rects = widget_rects(&grid);
        assert_eq!(rects[0], (4.0, 4.0, 100.0, 20.0));
        assert_eq!(rects[1], (4.0, 74.0, 100.0, 20.0), "row 0 is 24 tall, row 1 starts at 24 + 50");
        assert_eq!(grid.width(), 108.0, "100 + 4 + 4");
    }

    #[test]
    fn remove_children_empties_a_layout_and_resets_a_linears_index() {
        // `LinearLayout.removeChildren` also resets `nextChildIndex` (`:50-54`);
        // without that, refilling a layout would leave the first rows empty.
        let mut column = LinearLayout::vertical().spacing(4);
        column.add_child(cell(200.0, 20.0));
        column.add_child(cell(200.0, 20.0));
        column.remove_children();
        column.add_child(cell(200.0, 20.0));
        column.arrange_elements();
        let rects = widget_rects(&column);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].1, 0.0, "the refilled child starts at row 0 again");
    }

    #[test]
    fn an_empty_layout_arranges_to_nothing_rather_than_panicking() {
        // The grid's dimension arrays are sized from the highest occupied index,
        // which is 0 for no children at all — an off-by-one here is an index
        // panic on the first frame of a screen that has not been filled yet.
        let mut grid = GridLayout::new();
        grid.arrange_elements();
        assert_eq!((grid.width(), grid.height()), (0.0, 0.0));
        assert!(widget_rects(&grid).is_empty());

        let mut frame = FrameLayout::with_min_size(100.0, 20.0);
        frame.arrange_elements();
        assert_eq!((frame.width(), frame.height()), (100.0, 20.0));

        let mut hf = HeaderAndFooterLayout::new(400.0, 200.0);
        hf.arrange_elements();
        assert_eq!(hf.contents().y(), 63.0);
    }

    // -- the tab widget's geometry ------------------------------

    #[test]
    fn round_toward_rounds_up_to_the_multiple_never_down() {
        assert_eq!(round_toward(0.0, 2.0), 0.0);
        assert_eq!(round_toward(1.0, 2.0), 2.0, "not 0 -- always rounds up");
        assert_eq!(round_toward(2.0, 2.0), 2.0, "already a multiple: unchanged");
        assert_eq!(round_toward(3.0, 2.0), 4.0);
        assert_eq!(round_toward(124.0, 2.0), 124.0);
        assert_eq!(round_toward(125.0, 2.0), 126.0);
    }

    /// Three tabs at 854 px — the width every other geometry test in this
    /// tree measures against. Hand-derived from `MenuTabBar.java`
    /// rather than round-tripped through [`tab_bar_geometry`] itself:
    /// `tabsWidth = min(400, 854) - 28 = 372`, `tabWidth =
    /// roundToward(372 / 3, 2) = roundToward(124, 2) = 124`, `startX =
    /// roundToward((854 - 372) / 2, 2) = roundToward(241, 2) = 242`.
    #[test]
    fn tab_bar_geometry_matches_vanillas_own_arithmetic_at_854_wide() {
        let (start_x, tab_width) = tab_bar_geometry(854.0, 3);
        assert_eq!(tab_width, 124.0);
        assert_eq!(start_x, 242.0);
        // The three tabs' rects, laid out left to right, must not overlap and
        // must not overrun the canvas.
        for i in 0..3u32 {
            let x = start_x + tab_width * i as f32;
            assert!(x >= 0.0 && x + tab_width <= 854.0, "tab {i} at x={x}");
        }
    }

    /// `min(400, width)` clamps at a wide canvas — the row does not keep
    /// growing past vanilla's own 400 px ceiling.
    #[test]
    fn tab_bar_width_is_capped_on_a_wide_canvas() {
        let (start_x_wide, tab_width_wide) = tab_bar_geometry(1280.0, 3);
        let (start_x_max, tab_width_max) = tab_bar_geometry(428.0, 3);
        // tabsWidth = min(400, 428) - 28 = 372, identical to the 400-ceiling
        // case at 1280 px: tabsWidth = min(400, 1280) - 28 = 372.
        assert_eq!(tab_width_wide, tab_width_max);
        // The row centres differently on the two canvases even though its own
        // width is the same, because centring depends on the *canvas* width.
        assert_ne!(start_x_wide, start_x_max);
    }

    #[test]
    fn a_single_tab_gets_the_whole_row_and_the_count_is_never_treated_as_zero() {
        // `tab_count.max(1)` guards the division; a caller that (incorrectly)
        // passes 0 must not divide by it.
        let (_, width_one) = tab_bar_geometry(854.0, 1);
        let (_, width_zero_guarded) = tab_bar_geometry(854.0, 0);
        assert_eq!(width_one, width_zero_guarded);
        assert_eq!(width_one, round_toward(372.0, 2.0));
    }

    /// [`tab_bar_row_rect`] must agree with [`tab_bar_geometry`] plus the
    /// per-tab offset — the shared function two screens' own `tab_row_rect`
    /// wrappers (`stats::tab_row_rect`, `create_world::tab_row_rect`) now
    /// resolve through, at a tab count neither of those screens uses (5), so
    /// this is not merely re-deriving `3` twice.
    #[test]
    fn tab_bar_row_rect_matches_geometry_plus_the_per_tab_offset() {
        let width = 854.0;
        let count = 5;
        let (start_x, tab_width) = tab_bar_geometry(width, count);
        for i in 0..count {
            let rect = tab_bar_row_rect(i, count, width);
            assert_eq!(
                rect,
                (start_x + tab_width * i as f32, 0.0, tab_width, TAB_BAR_HEIGHT)
            );
        }
        // The discriminating control: two different tab counts at the same
        // width must not produce the same row rect for the same index, or
        // this function is silently ignoring `count`.
        let (start_x_3, tab_width_3) = tab_bar_geometry(width, 3);
        assert_ne!(
            tab_bar_row_rect(1, count, width),
            (start_x_3 + tab_width_3, 0.0, tab_width_3, TAB_BAR_HEIGHT)
        );
    }
}
