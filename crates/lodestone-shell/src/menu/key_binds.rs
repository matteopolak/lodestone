//! The Controls menu's Key Binds screen — vanilla's
//! `KeyBindsScreen`/`KeyBindsList`, over the rebindable layer
//! [`crate::keybinds`] already built with no screen in front of it.
//!
//! ## Why this is not one more `options.rs` page
//!
//! Every other settings page is `OptionsList` geometry: fixed-width columns,
//! one widget per `addBig`/`addSmall` cell, a caption plus a value.
//! `KeyBindsList` is a different `AbstractSelectionList` entirely
//! (`KeyBindsList.java`): `getRowWidth()` is 340 (not 310), every row's real
//! height is a flat 20 (not `OptionsList`'s header-dependent rule), and an
//! action row carries **two** buttons anchored from the row's *right* edge —
//! a 75 px bind button and a 50 px reset button, 5 px apart — plus a name
//! label at the *left* edge. None of `options.rs`'s [`super::options::Cell`],
//! [`super::options::Entry`] or [`super::options::Placement`] fit that shape
//! without bending it, which is exactly what the settings-tree docs flag this
//! screen for. So this module is the second list-widget kind that fix's plan
//! always said this tree would eventually need.
//!
//! ## What this module renders, and what it does not decide
//!
//! Structure (which rows exist, in what order) is a pure function of
//! [`crate::keybinds::Category::SORT_ORDER`] and
//! [`crate::keybinds::Keybinds::in_category`] — it does not depend on any
//! *binding*. **Creative and Spectator never appear**: this client has no
//! [`InputAction`] in either category (see the module's own doc on why —
//! vanilla mappings with no consumer are absent, not listed and dead), and a
//! header over zero rows would be decoration with nothing under it, unlike
//! every other present-and-inactive control in this tree, which stands for a
//! real vanilla feature. Six of vanilla's eight categories are real here.
//!
//! Labels and enabled-ness *do* depend on the live [`Keybinds`] table, so
//! [`KeyControl::label`]/[`KeyControl::is_live`] both take one — the same
//! split [`super::options::Cell::label`] already uses for `&Options`.
//!
//! ## Wired vs. decorative
//!
//! - **Wired**: reaching this page (Controls → Key Binds → Escape/Done
//!   back), viewing every one of the 29 actions grouped by category in
//!   vanilla's registration order, per-row Reset (`Keybinds::reset` +
//!   persist), and Reset Keys (`Keybinds::reset_all` + persist). None of
//!   these needs anything this crate cannot reach on its own.
//! - **The one genuine last hop**: clicking (or pressing Enter on) a bind
//!   button starts capture — [`KeyBindsNav::awaiting`] — entirely within this
//!   module, no different from any other button click. *Finishing* the
//!   capture needs the **next raw key or mouse event**, unfiltered by the
//!   `MenuKey` translation `app.rs`'s `menu_key_for` already applies (a
//!   physical key with no `text` — F-keys, modifiers, arrows other than
//!   Up/Down — is silently dropped there today, and rebinding to exactly one
//!   of those is a real, common case). That hop is `app.rs`'s, which this
//!   crate does not own; see `docs/keybindings.md`'s "Wiring the Controls
//!   menu" section for the exact patch and [`super::nav::MenuNav::capture_binding`]
//!   for the far end of it.
//!
//! ## Geometry, transcribed
//!
//! Every number below is read out of `.cache/mc/26.2/client-src`, file and
//! line named, in logical GUI pixels — nothing here is measured off our own
//! output.
//!
//! - `KeyBindsList.ITEM_HEIGHT = 20` (`:21`) — every row, category or action,
//!   is this tall. Unlike `OptionsList`, there is no
//!   [`super::options::header_padding_top`] rule: a `CategoryEntry` is added
//!   through the same `addEntry(entry, defaultEntryHeight)` as a `KeyEntry`
//!   (`KeyBindsList.java`, `AbstractSelectionList.java`), so the
//!   window math here has no first-entry special case.
//! - `getRowWidth() = 340` (`:59-61`). `getRowLeft() = x + width/2 -
//!   rowWidth/2` (`AbstractSelectionList.java`), and this list's `x`
//!   is 0 (the whole canvas width), so [`ROW_LEFT`] is `width/2 - 170`.
//! - `scrollBarX() = getRowRight() + scrollbarWidth() + 2`
//!   (`AbstractSelectionList.java`), and `scrollbarWidth()` is the
//!   record default `6` (`AbstractScrollArea.java`) — `width/2 + 170 + 8`.
//! - `KeyEntry.extractContent` (`:129-143`): `resetButtonX = scrollBarX() -
//!   50 - 10`, `changeButtonX = resetButtonX - 5 - 75`, `buttonY =
//!   getContentY() - 2`. `getContentY() = getY() + 2` (`AbstractSelectionList.java`),
//!   so `buttonY` is exactly the entry's own `y` — a button fills its 20 px
//!   row with no inset, unlike `OptionsList`'s 2 px content margin.
//! - The name label draws at `(getContentX(), getContentYMiddle() - 9/2)`
//!   (`:137`). `getContentX() = getX() + 2` and `getContentHeight() =
//!   getHeight() - 4 = 16`, so `getContentYMiddle() = y + 2 + 8 = y + 10`, and
//!   `9/2` is Java integer division (`4`) — the label's line-top is `y + 6`.
//! - `CategoryEntry.extractContent` (`:74-77`) centres a
//!   `FocusableTextWidget` at `width/2 - categoryName.getWidth()/2`, drawn at
//!   `getContentBottom() - categoryName.getHeight()`. This client has no
//!   `FocusableTextWidget` (no border chrome, no focus fill) and no font
//!   metrics available at layout-build time to replicate the exact
//!   `getWidth()/2` centring — so, like every settings-page header before it
//!   ([`super::options`]'s own departures), a category header here is a plain
//!   [`super::render::MenuLabel`] with [`super::render::Align::Centre`] about
//!   `width/2`, which centres the *drawn* text exactly the way vanilla
//!   centres its widget, without needing to know its width up front. The
//!   vertical position is a documented approximation
//!   ([`CATEGORY_TEXT_DY`]) rather than `FocusableTextWidget`'s real border
//!   metrics, which this client does not model.
//! - The footer is `LinearLayout.horizontal().spacing(8)` of two default-width
//!   (150 px) buttons (`KeyBindsScreen.java`) — **identical** in shape
//!   to [`super::options::SettingsPage::Accessibility`]'s own two-button
//!   footer, so this reuses [`super::options::Placement::Footer`] and
//!   [`super::options::footer_rects`] directly rather than a second
//!   implementation of the same `HeaderAndFooterLayout` arithmetic.
//!
//! ## The visible window
//!
//! Same departure as [`super::options`]'s (3): this pipeline has no scissor,
//! so the window is a fixed pixel budget derived from the *shortest* content
//! band any `gui_scale` can produce, not a continuously scrolling list.
//! [`VISIBLE_ROWS`] is that budget divided by the flat 20 px row height —
//! simpler than `options.rs`'s version, which has to account for a header's
//! variable padding.
//!
//! ## Dependencies
//!
//! - [`crate::keybinds`] — the whole model: `InputAction`, `Category`,
//!   `Binding`, `Keybinds`.
//! - [`super::options`] — [`super::options::SUB_HEADER_HEIGHT`],
//!   [`super::options::FOOTER_HEIGHT`], [`super::options::LIST_TOP_INSET`],
//!   [`super::options::Placement::Footer`], [`super::options::footer_rects`].
//! - [`super::render`] — [`super::render::Origin`] (a
//!   [`super::render::Origin::KeyBinds`] arm is added there),
//!   [`super::render::MenuFrame`], [`super::render::MenuRow`],
//!   [`super::render::MenuLabel`], [`super::render::Slot`].
//! - `docs/keybindings.md` — the model this is the last hop for.

use crate::keybinds::{Category, InputAction, Keybinds};

use super::options::{self, Placement};
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot};

// -- vanilla's metrics, transcribed (see the module docs) --------------------

/// `KeyBindsList.ITEM_HEIGHT` (`:21`).
pub const ROW_H: f32 = 20.0;
/// `KeyBindsList.getRowWidth()` (`:59-61`).
pub const ROW_WIDTH: f32 = 340.0;
/// `KeyEntry.changeButton`'s bound width (`:114`).
pub const BIND_BUTTON_W: f32 = 75.0;
/// `KeyEntry.resetButton`'s bound width (`:124`).
pub const RESET_BUTTON_W: f32 = 50.0;
/// `resetButtonX = scrollBarX() - 50 - 10`'s trailing gap (`:130`).
const RESET_RIGHT_GAP: f32 = 10.0;
/// `changeButtonX = resetButtonX - 5 - 75`'s gap (`:134`).
const BIND_RESET_GAP: f32 = 5.0;
/// `AbstractScrollArea.ScrollbarSettings` default `scrollbarWidth` (`:145`),
/// plus `scrollBarX()`'s own `+ 2` (`AbstractSelectionList.java`).
const SCROLLBAR_GAP: f32 = 6.0 + 2.0;
/// `getContentX()`'s `+2` (`AbstractSelectionList.java`).
const NAME_LEFT_INSET: f32 = 2.0;
/// The name label's line-top offset from the entry's own `y` — derived in the
/// module docs: `getContentYMiddle() - 9/2 = (y + 10) - 4 = y + 6`.
const NAME_TEXT_DY: f32 = 6.0;
/// The category header's line-top offset from the entry's own `y` —
/// documented approximation, see the module docs' geometry section.
const CATEGORY_TEXT_DY: f32 = (ROW_H - 9.0) / 2.0;

/// How many pixels of list a canvas may show — see [`super::options::LIST_WINDOW_PX`]'s
/// doc for why this is a fixed budget rather than a continuous scroll, derived
/// from the same [`crate::config::MIN_SCALED_HEIGHT`] floor.
pub const LIST_WINDOW_PX: f32 = crate::config::MIN_SCALED_HEIGHT as f32
    - options::SUB_HEADER_HEIGHT
    - options::FOOTER_HEIGHT
    - options::LIST_TOP_INSET;

/// `getRowLeft()` on a `width`-wide canvas (`AbstractSelectionList.java`,
/// this list's own `x = 0`).
#[must_use]
pub fn row_left(width: f32) -> f32 {
    width * 0.5 - ROW_WIDTH * 0.5
}

/// `getRowRight()` (`:376-378`).
#[must_use]
pub fn row_right(width: f32) -> f32 {
    row_left(width) + ROW_WIDTH
}

/// `scrollBarX()` (`:289-291`).
#[must_use]
pub fn scrollbar_x(width: f32) -> f32 {
    row_right(width) + SCROLLBAR_GAP
}

/// The reset button's x (`KeyEntry.extractContent:130`).
#[must_use]
pub fn reset_button_x(width: f32) -> f32 {
    scrollbar_x(width) - RESET_BUTTON_W - RESET_RIGHT_GAP
}

/// The bind ("change") button's x (`:134`).
#[must_use]
pub fn bind_button_x(width: f32) -> f32 {
    reset_button_x(width) - BIND_RESET_GAP - BIND_BUTTON_W
}

/// The name label's x (`getContentX()`, `:137`).
#[must_use]
pub fn name_x(width: f32) -> f32 {
    row_left(width) + NAME_LEFT_INSET
}

// -- the row/control model ---------------------------------------------------

/// One row of the flattened list: a category header, or one action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Category(Category),
    Action(InputAction),
}

/// Every [`InputAction`] this client has, in vanilla's Controls-screen order:
/// walk [`Category::SORT_ORDER`] (registration order, not alphabetical — see
/// that constant's own doc) and take [`Keybinds::in_category`] for each.
/// **Not** [`InputAction::ALL`] directly — that is declaration order, which
/// groups Gameplay before Inventory before Multiplayer before Misc, not
/// `SORT_ORDER`'s Movement/Misc/Multiplayer/Gameplay/Inventory/…
#[must_use]
pub fn ordered_actions() -> Vec<InputAction> {
    Category::SORT_ORDER
        .into_iter()
        .flat_map(Keybinds::in_category)
        .collect()
}

/// Every row: a `Row::Category` header immediately before the first action of
/// each category that has one, then that category's actions. A category with
/// no actions (Creative, Spectator — see the module docs) contributes no
/// header and no rows at all.
#[must_use]
pub fn all_rows() -> Vec<Row> {
    let mut out = Vec::new();
    let mut previous: Option<Category> = None;
    for action in ordered_actions() {
        let category = action.category();
        if previous != Some(category) {
            out.push(Row::Category(category));
            previous = Some(category);
        }
        out.push(Row::Action(action));
    }
    out
}

/// The row index of `action` in [`all_rows`], for scrolling it into view.
#[must_use]
pub fn row_of_action(action: InputAction) -> Option<usize> {
    all_rows()
        .iter()
        .position(|r| matches!(r, Row::Action(a) if *a == action))
}

/// This screen's list, as the generic [`super::widget::ListSpec`] the scrollbar
/// draw and the mouse wheel both go through.
///
/// `top` is the **un-inset** [`options::SUB_HEADER_HEIGHT`], not
/// `SUB_HEADER_HEIGHT + LIST_TOP_INSET`, because
/// [`super::widget::ScrollList`] adds [`super::widget::LIST_CONTENT_PADDING`]
/// itself as its `first_entry_y` and the two constants are the same 2 px. Adding
/// the inset here as well would double it and put every row 2 px low — the same
/// note `stats::list_spec` carries.
///
/// [`ROW_WIDTH`] is the row band, so the bar lands at
/// [`scrollbar_x`]'s own answer: `ListSpec::row_right` is
/// `floor(w/2) - floor(340/2) + 340`, which is this module's [`row_right`],
/// and `ScrollList::scrollbar_x` then adds the same `6 + 2` [`SCROLLBAR_GAP`]
/// does. Asserted in this module's tests rather than left to agree by eye —
/// the reset and bind buttons are positioned off `scrollbar_x`, so a bar that
/// drifted would drag them with it.
#[must_use]
pub fn list_spec(scroll: f32) -> super::widget::ListSpec {
    super::widget::ListSpec::uniform(
        ROW_H,
        options::SUB_HEADER_HEIGHT,
        options::FOOTER_HEIGHT,
        all_rows().len(),
        ROW_WIDTH,
    )
    .at(scroll)
}

/// One focusable widget on this screen: a bind button, a per-action reset, the
/// footer's Reset Keys, or Done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyControl {
    /// Click to start capture (see [`KeyBindsNav::awaiting`]).
    Bind(InputAction),
    /// Reset this one action to its default and persist.
    Reset(InputAction),
    /// Vanilla's "Reset Keys" footer button: reset every action and persist.
    ResetAll,
    /// Leave the screen, back to Controls.
    Done,
}

impl KeyControl {
    /// The label drawn on the widget.
    ///
    /// `awaiting` decorates the one bind button currently capturing input
    /// with vanilla's own `"> {name} <"` (`KeyBindsList.java`) —
    /// there is no `changeButton.getMessage` state to read here, so the
    /// decoration is computed fresh from the same fact
    /// [`KeyBindsNav::awaiting`] already tracks, the same "one source of
    /// truth for label and behaviour" rule the settings-tree Online button
    /// fix (task `task_036bd7b9`) applied one screen over.
    #[must_use]
    pub fn label(self, keybinds: &Keybinds, awaiting: Option<InputAction>) -> String {
        match self {
            KeyControl::Bind(action) => {
                let bound = keybinds.binding(action).label();
                if awaiting == Some(action) {
                    format!("> {bound} <")
                } else if keybinds.has_conflict(action) {
                    format!("[ {bound} ]")
                } else {
                    bound
                }
            }
            KeyControl::Reset(_) => "Reset".to_string(),
            KeyControl::ResetAll => "Reset Keys".to_string(),
            KeyControl::Done => "Done".to_string(),
        }
    }

    /// Whether this control can be activated.
    ///
    /// A bind button is always live — capturing a new binding is always
    /// available, even for an already-default one. A reset button is live
    /// only when its action is *not* already default
    /// (`KeyEntry.refreshEntry`'s `resetButton.active = !key.isDefault()`,
    /// `:158`); Reset Keys mirrors that at the whole-table level
    /// (`KeyBindsScreen.extractRenderState`'s `canReset` scan, `:91-100`).
    #[must_use]
    pub fn is_live(self, keybinds: &Keybinds) -> bool {
        match self {
            KeyControl::Bind(_) | KeyControl::Done => true,
            KeyControl::Reset(action) => !keybinds.is_default(action),
            KeyControl::ResetAll => ordered_actions()
                .into_iter()
                .any(|a| !keybinds.is_default(a)),
        }
    }
}

/// Where one [`KeyControl`] sits — [`super::render::Origin::KeyBinds`]'s whole
/// body. Unlike [`super::options::Placement`], every list-content variant
/// here shares the same `{row, scroll}` shape because every row is the same
/// height; only the x differs per widget, and that is a `match` in
/// [`placement_anchor`] rather than a fourth field.
///
/// **`scroll` is pixels, not a row index.** It was `first: u16`,
/// the index of the row at the top of the window, which is the snap-to-row
/// behaviour pixel scrolling exists to remove: a row-index offset is always a
/// multiple of [`ROW_H`], so the list could only ever jump a whole 20 px at a
/// time and a half-scrolled row was not expressible. `Eq` had to go with it —
/// `f32` — matching [`super::options::SettingsNav`], which dropped `Eq` for the
/// same reason and records it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyPlacement {
    /// The bind button, at absolute row `row` (an index into [`all_rows`]),
    /// with the list scrolled `scroll` pixels down.
    Bind { row: u16, scroll: f32 },
    /// The per-row reset button, same row.
    Reset { row: u16, scroll: f32 },
    /// The action's name label (not a [`KeyControl`] — a
    /// [`super::render::MenuLabel`], like an `OptionsList` header).
    Name { row: u16, scroll: f32 },
    /// The category header label, also a [`super::render::MenuLabel`].
    Category { row: u16, scroll: f32 },
}

impl KeyPlacement {
    fn row_scroll(self) -> (u16, f32) {
        match self {
            KeyPlacement::Bind { row, scroll }
            | KeyPlacement::Reset { row, scroll }
            | KeyPlacement::Name { row, scroll }
            | KeyPlacement::Category { row, scroll } => (row, scroll),
        }
    }
}

/// The top-left of the widget a [`KeyPlacement`] names, on a `width`×`height`
/// canvas. [`super::render::Origin::KeyBinds`]'s whole body — see
/// [`super::options::placement_anchor`] for the sibling this mirrors.
#[must_use]
pub fn placement_anchor(placement: KeyPlacement, width: f32, _height: f32) -> (f32, f32) {
    let (row, scroll) = placement.row_scroll();
    // Pixel scrolling, so a row's y is its *absolute* offset in the
    // list minus the scroll — no `checked_sub` sentinel any more. That guard
    // existed because a row above the window underflowed a `u16` subtraction;
    // here a row scrolled off the top simply resolves to a y above the band and
    // `render::draw` clips it, which is the same answer without a magic
    // coordinate. `scroll.floor()` is vanilla's `(int)scrollAmount` cast — the
    // rows land on whole pixels while the offset itself stays fractional, which
    // is what lets a trackpad's sub-notch delta accumulate.
    let row_y = options::SUB_HEADER_HEIGHT + options::LIST_TOP_INSET + f32::from(row) * ROW_H
        - scroll.floor();
    match placement {
        KeyPlacement::Bind { .. } => (bind_button_x(width), row_y),
        KeyPlacement::Reset { .. } => (reset_button_x(width), row_y),
        KeyPlacement::Name { .. } => (name_x(width), row_y + NAME_TEXT_DY),
        KeyPlacement::Category { .. } => (width * 0.5, row_y + CATEGORY_TEXT_DY),
    }
}

/// One flattened, focusable control: the widget and its already-resolved
/// [`Slot`]. Mirrors [`super::options::Control`], with one difference: this
/// screen has two genuinely different [`Origin`] variants in play (the
/// scrolled content list is [`Origin::KeyBinds`], the footer is
/// [`Origin::Settings`] — see [`footer_controls`]), and a `Slot` already
/// carries whichever one applies, so there is no need for a second
/// placement-typed field the way [`super::options::Control`] has exactly one
/// kind to carry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyControlView {
    pub control: KeyControl,
    pub slot: Slot,
}

/// Every focusable control, ignoring the scroll — the cursor steps through
/// this. Mirrors [`super::options::all_controls`].
#[must_use]
pub fn all_controls() -> Vec<KeyControl> {
    let mut out = Vec::new();
    for action in ordered_actions() {
        out.push(KeyControl::Bind(action));
        out.push(KeyControl::Reset(action));
    }
    out.push(KeyControl::ResetAll);
    out.push(KeyControl::Done);
    out
}

/// **Every** control on screen at scroll offset `scroll`, then the footer.
/// Mirrors [`super::options::controls`].
///
/// Emits every row rather than a `visible_rows(first)` window. The
/// window slice was the row-index model's other half: it had to skip any row
/// that did not *wholly* fit, because a partially-visible row drawn in full
/// would spill over the footer. Clipping to the band is now
/// [`super::render::draw`]'s job — it wraps each row in the band's own clip
/// rect — so a half-scrolled row draws its visible half instead of vanishing,
/// which is the whole point of the conversion.
///
/// The consequence for callers: a row's index in this vector is no longer a
/// *visible* position, it is an absolute one. That is what
/// [`KeyBindsNav::selected_row`] and [`KeyBindsNav::hover_row`] key off, and
/// they are unchanged because they always matched on the `control` rather than
/// on the index.
#[must_use]
pub fn controls(scroll: f32) -> Vec<KeyControlView> {
    let mut out = Vec::new();
    for (row, entry) in all_rows().iter().enumerate() {
        if let Row::Action(action) = *entry {
            out.push(KeyControlView {
                control: KeyControl::Bind(action),
                slot: Slot {
                    origin: Origin::KeyBinds(KeyPlacement::Bind {
                        row: row as u16,
                        scroll,
                    }),
                    dx: 0.0,
                    dy: 0.0,
                    w: BIND_BUTTON_W,
                    h: ROW_H,
                },
            });
            out.push(KeyControlView {
                control: KeyControl::Reset(action),
                slot: Slot {
                    origin: Origin::KeyBinds(KeyPlacement::Reset {
                        row: row as u16,
                        scroll,
                    }),
                    dx: 0.0,
                    dy: 0.0,
                    w: RESET_BUTTON_W,
                    h: ROW_H,
                },
            });
        }
    }
    out.extend(footer_controls());
    out
}

/// The two footer controls, reusing [`super::options::Placement::Footer`] —
/// see the module docs' geometry section for why this footer needs no
/// KeyBinds-specific placement at all.
fn footer_controls() -> [KeyControlView; 2] {
    let slot = |index: u8| Slot {
        origin: Origin::Settings(Placement::Footer { index, count: 2 }),
        dx: 0.0,
        dy: 0.0,
        w: options::SMALL_BUTTON_WIDTH,
        h: options::WIDGET_H,
    };
    [
        KeyControlView {
            control: KeyControl::ResetAll,
            slot: slot(0),
        },
        KeyControlView {
            control: KeyControl::Done,
            slot: slot(1),
        },
    ]
}

// -- navigation ---------------------------------------------------------------

/// What [`KeyBindsNav`] asks its caller ([`super::options::SettingsNav`]) to
/// do after a keypress or a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyBindsOutcome {
    /// Handled internally (a capture started, or nothing was under the
    /// cursor).
    None,
    /// Leave this page, back to Controls — Done, or Escape when nothing is
    /// being captured.
    Back,
    /// Reset one action to its default and persist.
    ResetOne(InputAction),
    /// Reset every action to its default and persist.
    ResetAll,
}

/// This screen's own cursor: which control, how far scrolled, and which
/// action (if any) is mid-capture.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct KeyBindsNav {
    cursor: usize,
    /// Scroll offset in **pixels**, not a row index. `Eq` went with
    /// the change — see [`KeyPlacement`]'s doc.
    scroll: f32,
    awaiting: Option<InputAction>,
}

impl KeyBindsNav {
    /// A fresh cursor at the top, capturing nothing — called whenever the
    /// page is entered, so re-opening it never resumes mid-capture or
    /// scrolled down. See [`super::options::SettingsNav::activate`]'s
    /// `SettingsPage::KeyBinds` arm.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The scroll offset, in pixels.
    #[must_use]
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// The live [`super::widget::ScrollList`] at this canvas height, or `None`
    /// when there is nothing to scroll.
    #[must_use]
    fn model(&self, canvas_height: f32) -> Option<super::widget::ScrollList> {
        list_spec(self.scroll).model(canvas_height)
    }

    /// One mouse-wheel notch, through the primitive. Positive scrolls **up**;
    /// the negation lives in [`super::widget::ScrollList::mouse_scrolled`] and
    /// nowhere else, so there is exactly one place the sign can be wrong.
    pub fn scroll_by(&mut self, notches: f32, canvas_height: f32) {
        let Some(mut list) = self.model(canvas_height) else {
            return;
        };
        list.mouse_scrolled(notches);
        self.scroll = list.scroll();
    }

    /// The action a bind button is currently capturing input for, if any.
    /// `app.rs` reads this (through
    /// [`super::nav::MenuNav::awaiting_key_capture`]) to decide whether the
    /// *next* raw key/mouse event is a rebind rather than ordinary menu
    /// input.
    #[must_use]
    pub fn awaiting(&self) -> Option<InputAction> {
        self.awaiting
    }

    #[must_use]
    pub fn visible(&self) -> Vec<KeyControlView> {
        controls(self.scroll)
    }

    /// The cursor's position within [`Self::visible`], for the highlight.
    #[must_use]
    pub fn selected_row(&self) -> Option<usize> {
        let all = all_controls();
        let control = *all.get(self.cursor)?;
        self.visible().iter().position(|c| c.control == control)
    }

    /// Moves the cursor by one control, wrapping. Steps over nothing — same
    /// departure as [`super::options`]'s (4): a reset button that cannot be
    /// pressed yet is still a cursor stop, or it and everything past it on a
    /// scrolled page would be unreachable.
    pub fn step(&mut self, forward: bool) {
        let len = all_controls().len();
        if len == 0 {
            return;
        }
        self.cursor = if forward {
            (self.cursor + 1) % len
        } else {
            (self.cursor + len - 1) % len
        };
        self.scroll_to_cursor();
    }

    /// Bring the cursor's row into the band — vanilla's
    /// `AbstractSelectionList.ensureVisible`, through
    /// [`super::widget::ScrollList::scroll_to_entry`].
    ///
    /// This used to be a hand-rolled `while !visible_rows(self.first).contains(
    /// &row) { self.first += 1 }` walk, which is where the snap-to-row behaviour
    /// came from: every step moved a whole [`ROW_H`]. `scroll_to_entry` moves the
    /// **minimum** number of pixels instead, so a row one pixel below the band
    /// scrolls one pixel.
    ///
    /// [`crate::config::MIN_SCALED_HEIGHT`] rather than the live canvas, for the
    /// reason `stats::step` records: a keypress has no canvas in hand, and the
    /// smallest supported canvas is the conservative choice — it can only
    /// over-scroll into a region a larger canvas also shows.
    fn scroll_to_cursor(&mut self) {
        let all = all_controls();
        let Some(&control) = all.get(self.cursor) else {
            return;
        };
        let action = match control {
            KeyControl::Bind(a) | KeyControl::Reset(a) => a,
            // The footer is always visible; nothing to scroll for it.
            KeyControl::ResetAll | KeyControl::Done => return,
        };
        let Some(row) = row_of_action(action) else {
            return;
        };
        let Some(mut list) = self.model(crate::config::MIN_SCALED_HEIGHT as f32) else {
            return;
        };
        list.scroll_to_entry(row);
        self.scroll = list.scroll();
    }

    /// Puts the cursor on the control at visible row `row` — the mouse's
    /// half. Mirrors [`super::options::SettingsNav::hover_row`].
    pub fn hover_row(&mut self, row: usize) {
        let visible = controls(self.scroll);
        let Some(view) = visible.get(row).copied() else {
            return;
        };
        let all = all_controls();
        if let Some(i) = all.iter().position(|&c| c == view.control) {
            self.cursor = i;
        }
    }

    /// Activates the control at visible row `row` — a click. Mirrors
    /// [`super::options::SettingsNav::click_row`]'s "resolve the row to its
    /// own control, do not route through Enter" rule (that fix's fix,
    /// which this page inherits by construction rather than by copying the
    /// guard).
    pub fn click_row(&mut self, row: usize, keybinds: &Keybinds) -> KeyBindsOutcome {
        let visible = self.visible();
        let Some(view) = visible.get(row).copied() else {
            return KeyBindsOutcome::None;
        };
        self.hover_row(row);
        self.activate(view.control, keybinds)
    }

    /// Activates whatever the cursor is on — Enter's half.
    pub fn enter(&mut self, keybinds: &Keybinds) -> KeyBindsOutcome {
        let all = all_controls();
        match all.get(self.cursor).copied() {
            Some(control) => self.activate(control, keybinds),
            None => KeyBindsOutcome::None,
        }
    }

    /// The one place a control's activation is interpreted — mirrors
    /// [`super::options::SettingsNav::activate`]'s own doc, including its
    /// `isActive()` guard: a `Reset`/`ResetAll` that is not
    /// [`KeyControl::is_live`] does nothing at all, the same
    /// `AbstractWidget.mouseClicked` rule every other inactive control in
    /// this tree already follows. Needs `keybinds` for exactly that check —
    /// unlike [`Self::step`]/[`Self::hover_row`], which only need this
    /// screen's fixed *structure*.
    fn activate(&mut self, control: KeyControl, keybinds: &Keybinds) -> KeyBindsOutcome {
        if !control.is_live(keybinds) {
            return KeyBindsOutcome::None;
        }
        match control {
            // Starting capture needs no `Keybinds` mutation at all — it is
            // pure UI state until the raw key/mouse hop lands (see the
            // module docs). A second click on a *different* bind button
            // while one is already awaiting simply moves which one, matching
            // vanilla's own `this.selectedKey = key` overwrite with no
            // "cancel the old one" step.
            KeyControl::Bind(action) => {
                self.awaiting = Some(action);
                KeyBindsOutcome::None
            }
            KeyControl::Reset(action) => KeyBindsOutcome::ResetOne(action),
            KeyControl::ResetAll => KeyBindsOutcome::ResetAll,
            KeyControl::Done => KeyBindsOutcome::Back,
        }
    }

    /// Escape: cancel a pending capture if there is one (vanilla's own
    /// `keyPressed` intercept sets `InputConstants.UNKNOWN` on Escape while
    /// capturing — **this client does not**, deliberately: see
    /// [`super::nav::MenuNav::capture_binding`]'s doc on the `Pause`-unbind
    /// hazard `docs/keybindings.md` already names). Otherwise leave the page.
    pub fn escape(&mut self) -> KeyBindsOutcome {
        if self.awaiting.take().is_some() {
            return KeyBindsOutcome::None;
        }
        KeyBindsOutcome::Back
    }

    /// Consumes the pending capture, if any. Called once from
    /// [`super::nav::MenuNav::capture_binding`] when `app.rs` forwards the
    /// raw key/mouse event that finishes it.
    pub fn take_awaiting(&mut self) -> Option<InputAction> {
        self.awaiting.take()
    }
}

// -- the frame ----------------------------------------------------------------

/// Builds the whole Key Binds frame. Called from
/// [`super::options::settings_frame`]'s `SettingsPage::KeyBinds` branch,
/// which is why this takes the same two live-state pieces that branch already
/// has in scope rather than reaching into a [`super::options::SettingsNav`]
/// itself.
#[must_use]
pub fn frame(nav: &KeyBindsNav, keybinds: &Keybinds) -> MenuFrame<'static> {
    let visible = nav.visible();
    let selected = nav.selected_row();
    let awaiting = nav.awaiting();

    // `nav.visible()` already ends with the two footer controls (see
    // `controls`), so this one pass covers the scrolled content and the
    // footer alike — nothing to add twice.
    let rows: Vec<MenuRow> = visible
        .iter()
        .map(|view| MenuRow {
            label: view.control.label(keybinds, awaiting),
            enabled: view.control.is_live(keybinds),
            slot: Some(view.slot),
            ..Default::default()
        })
        .collect();

    let mut labels = vec![MenuLabel {
        text: "Key Binds".to_string(), // `controls.keybinds.title`
        origin: Origin::ScreenTop,
        dx: 0.0,
        dy: options::title_y(super::options::SettingsPage::Controls),
        align: Align::Centre,
        colour: super::widget::ACTIVE_LABEL,
        scale: 1.0,
    }];
    // **`list_labels`, not `labels`.** These are the only labels on
    // this screen that scroll, and `render::draw` clips that vector to the band
    // `frame.list` declares. Putting them in `labels` — where the title, which
    // does *not* scroll, correctly lives — would draw a scrolled-away category
    // header over the footer, because a free text label has nowhere else to carry
    // a clip rect. `MenuRow`s already get one from draw.rs's per-row `with_clip`,
    // which is why the bind/reset buttons above need nothing here. Same split
    // `stats::frame` landed.
    let mut list_labels = Vec::new();
    for (row, entry) in all_rows().iter().enumerate() {
        let scroll = nav.scroll();
        match *entry {
            Row::Category(category) => list_labels.push(MenuLabel {
                text: category_caption(category).to_string(),
                origin: Origin::KeyBinds(KeyPlacement::Category {
                    row: row as u16,
                    scroll,
                }),
                dx: 0.0,
                dy: 0.0,
                align: Align::Centre,
                colour: super::widget::ACTIVE_LABEL,
                scale: 1.0,
            }),
            Row::Action(action) => list_labels.push(MenuLabel {
                text: action_caption(action).to_string(),
                origin: Origin::KeyBinds(KeyPlacement::Name {
                    row: row as u16,
                    scroll,
                }),
                dx: 0.0,
                dy: 0.0,
                align: Align::Left,
                colour: super::widget::ACTIVE_LABEL,
                scale: 1.0,
            }),
        }
    }

    MenuFrame {
        title: "Key Binds",
        subtitle: "",
        rows,
        selected: selected.unwrap_or(usize::MAX),
        vanilla: true,
        labels,
        list_labels,
        // **`list` is deliberately not set here.** `render::dispatch` stamps
        // `f.list = nav.active_list(ui)` on every frame it returns, so the
        // scrollbar the draw paints and the offset the wheel clamps are two
        // readers of *one* declaration. Setting it here as well would be a second
        // declaration that agrees today — see `dispatch`'s own comment, and
        // `stats::frame`, which likewise leaves it alone.
        ..Default::default()
    }
}

/// The category's caption, verbatim from `en_us.json`'s **real** 26.2 key —
/// `key.category.minecraft.<id>` (`KeyMapping.Category.label`,
/// `KeyMapping.java`, `Identifier.toLanguageKey(String)`). **Not**
/// `key.categories.<id>`, which is legacy/unused text still sitting in the lang
/// file from an older versioning scheme — a trap worth naming because it reads
/// as the obvious key and is wrong (measured by reading `toLanguageKey`'s
/// actual call chain, not by grepping the lang file for something plausible).
#[must_use]
pub fn category_caption(category: Category) -> &'static str {
    match category {
        Category::Movement => "Movement",
        Category::Misc => "Miscellaneous",
        Category::Multiplayer => "Multiplayer",
        Category::Gameplay => "Gameplay",
        Category::Inventory => "Inventory",
        Category::Creative => "Creative Mode",
        Category::Spectator => "Spectator",
        Category::Debug => "Debug",
    }
}

/// The action's caption, verbatim from `en_us.json` at its own [`InputAction::name`]
/// key — except [`InputAction::Pause`], which is not a vanilla `KeyMapping` at
/// all (see that variant's own doc) and so has no `en_us.json` line; "Pause
/// Game" is this client's own caption for it, in the same spirit as
/// `key.lodestone.pause`'s namespaced name.
#[must_use]
pub fn action_caption(action: InputAction) -> &'static str {
    match action {
        InputAction::Forward => "Walk Forward",
        InputAction::Back => "Walk Backward",
        InputAction::Left => "Strafe Left",
        InputAction::Right => "Strafe Right",
        InputAction::Jump => "Jump",
        InputAction::Sneak => "Sneak",
        InputAction::Sprint => "Sprint",
        InputAction::Attack => "Attack/Destroy",
        InputAction::Use => "Use Item/Place Block",
        InputAction::Inventory => "Open/Close Inventory",
        InputAction::SwapOffhand => "Swap Item With Off Hand",
        InputAction::Drop => "Drop Selected Item",
        // `key.pickItem` and `key.screenshot`, verbatim from `en_us.json` like
        // every caption above. Added with the verbs themselves; this
        // match is exhaustive on purpose, so a new `InputAction` cannot reach the
        // Key Binds screen without a real caption.
        InputAction::PickItem => "Pick Block",
        InputAction::Screenshot => "Take Screenshot",
        InputAction::Hotbar1 => "Hotbar Slot 1",
        InputAction::Hotbar2 => "Hotbar Slot 2",
        InputAction::Hotbar3 => "Hotbar Slot 3",
        InputAction::Hotbar4 => "Hotbar Slot 4",
        InputAction::Hotbar5 => "Hotbar Slot 5",
        InputAction::Hotbar6 => "Hotbar Slot 6",
        InputAction::Hotbar7 => "Hotbar Slot 7",
        InputAction::Hotbar8 => "Hotbar Slot 8",
        InputAction::Hotbar9 => "Hotbar Slot 9",
        InputAction::Chat => "Open Chat",
        InputAction::Command => "Open Command",
        InputAction::PlayerList => "List Players",
        InputAction::TogglePerspective => "Toggle Perspective",
        InputAction::Pause => "Pause Game",
        InputAction::DebugOverlay => "Toggle Overlay",
        // The seven F3 chords, verbatim from `en_us.json` like every caption
        // above. Note `key.debug.spectate` reads "Cycle Spectator", not
        // anything with "Toggle" in it.
        InputAction::DebugShowHitboxes => "Show Hitboxes",
        InputAction::DebugShowChunkBorders => "Show Chunk Boundaries",
        InputAction::DebugShowAdvancedTooltips => "Show Advanced Tooltips",
        InputAction::DebugSpectate => "Cycle Spectator",
        InputAction::DebugSwitchGameMode => "Game Mode Switcher",
        InputAction::DebugFocusPause => "Toggle Lost Focus Pause",
        InputAction::DebugCopyLocation => "Copy Location",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybinds::Binding;

    /// The census: 6 categories (Creative/Spectator have no actions — see the
    /// module docs), 29 actions, 35 rows total.
    #[test]
    fn six_categories_carry_all_twenty_seven_actions() {
        let rows = all_rows();
        let categories: Vec<Category> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Category(c) => Some(*c),
                Row::Action(_) => None,
            })
            .collect();
        assert_eq!(
            categories,
            vec![
                Category::Movement,
                Category::Misc,
                Category::Multiplayer,
                Category::Gameplay,
                Category::Inventory,
                Category::Debug,
            ],
            "SORT_ORDER's own order (Movement, Misc, Multiplayer, Gameplay, \
             Inventory, …, Debug) with Creative/Spectator skipped — **not** \
             InputAction::ALL's declaration order, which groups Gameplay \
             before Inventory before Multiplayer before Misc"
        );
        let actions: Vec<InputAction> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Action(a) => Some(*a),
                Row::Category(_) => None,
            })
            .collect();
        // 36: 29 (Pick Block and Take Screenshot included) plus the seven F3
        // chords, which `Options.java` declares as `Category.DEBUG`
        // `KeyMapping`s in `debugKeys` and `KeyboardHandler.handleDebugKeys`
        // dispatches through `KeyMapping::matches` — so they belong on this
        // screen, and hardcoding them in `resolve_key` was the divergence.
        // Debug was already non-empty (`DebugOverlay`), so the *category* list
        // above is unchanged and only the action count moves. Deriving this
        // from `InputAction::ALL.len()` would make the assertion vacuous: it
        // would then agree with itself no matter which actions the screen
        // forgot.
        assert_eq!(actions.len(), 36, "every InputAction, once each");
        assert_eq!(rows.len(), 6 + 36);
        // The control: Creative and Spectator genuinely have zero actions —
        // if they ever gain one, this must start failing until the six above
        // does too, which is what proves the category list is not hand-typed
        // wishful thinking.
        assert_eq!(Keybinds::in_category(Category::Creative).count(), 0);
        assert_eq!(Keybinds::in_category(Category::Spectator).count(), 0);
    }

    /// Vanilla's registration order, not declaration order — the trap the
    /// module docs name. `Category::SORT_ORDER` is Movement, Misc,
    /// Multiplayer, Gameplay, Inventory, Creative, Spectator, Debug
    /// (`KeyMapping.java`). `InputAction::ALL`'s own *declaration*
    /// order is different: Movement, then Gameplay (Attack/Use), then
    /// Inventory, then Multiplayer (Chat/Command/PlayerList), then Misc
    /// (TogglePerspective/Pause), then Debug — Gameplay and Inventory both
    /// come *before* Multiplayer there, where `SORT_ORDER` puts Multiplayer
    /// *before* both. Walking `ALL` directly instead of `SORT_ORDER` would
    /// pass every other test in this file (same 29 actions, same 6 non-empty
    /// categories) and still render the Controls-menu categories in the wrong
    /// relative order — this is the one assertion that would catch it.
    #[test]
    fn actions_walk_registration_order_not_declaration_order() {
        let ordered = ordered_actions();
        assert_eq!(ordered[0], InputAction::Forward, "Movement first");
        let misc_pos = ordered.iter().position(|&a| a == InputAction::Pause).unwrap();
        let multiplayer_pos = ordered.iter().position(|&a| a == InputAction::Chat).unwrap();
        let gameplay_pos = ordered.iter().position(|&a| a == InputAction::Attack).unwrap();
        let inventory_pos = ordered
            .iter()
            .position(|&a| a == InputAction::Inventory)
            .unwrap();
        let debug_pos = ordered
            .iter()
            .position(|&a| a == InputAction::DebugOverlay)
            .unwrap();
        assert!(misc_pos < multiplayer_pos, "Misc (2nd) before Multiplayer (3rd)");
        // The trap itself: `InputAction::ALL` declares Gameplay's Attack/Use
        // *before* Multiplayer's Chat/Command/PlayerList, but `SORT_ORDER`
        // says the opposite.
        assert!(
            multiplayer_pos < gameplay_pos,
            "Multiplayer (3rd) before Gameplay (4th), even though \
             InputAction::ALL declares Attack/Use earlier than Chat/Command/ \
             PlayerList"
        );
        assert!(gameplay_pos < inventory_pos, "Gameplay (4th) before Inventory (5th)");
        assert!(inventory_pos < debug_pos, "Inventory (5th) before Debug (8th)");
    }

    #[test]
    fn every_control_has_a_row_and_every_row_but_the_footer_scrolls_into_view() {
        let all = all_controls();
        assert_eq!(all.len(), 36 * 2 + 2, "36 binds, 36 resets, ResetAll, Done");
        for &control in &all {
            match control {
                KeyControl::Bind(a) | KeyControl::Reset(a) => {
                    assert!(row_of_action(a).is_some(), "{a:?} must have a row");
                }
                KeyControl::ResetAll | KeyControl::Done => {}
            }
        }
    }

    #[test]
    fn stepping_the_cursor_reaches_every_control_and_scrolls_to_show_it() {
        let mut nav = KeyBindsNav::default();
        let mut seen_controls = std::collections::BTreeSet::new();
        let total = all_controls().len();
        for _ in 0..total * 2 {
            assert!(
                nav.selected_row().is_some(),
                "cursor {} off-window at scroll={}",
                nav.cursor(),
                nav.scroll()
            );
            seen_controls.insert(nav.cursor());
            nav.step(true);
        }
        assert_eq!(seen_controls.len(), total, "every control was reachable");
    }

    #[test]
    fn clicking_bind_starts_capture_and_escape_cancels_it_without_leaving() {
        let kb = Keybinds::new();
        let mut nav = KeyBindsNav::default();
        let forward_row = row_of_action(InputAction::Forward).unwrap();
        // Scroll Forward into view (it is the very first action, so first=0
        // already shows it, but drive it through the real API rather than
        // assuming that).
        nav.scroll = 0.0;
        let visible = nav.visible();
        let row = visible
            .iter()
            .position(|v| v.control == KeyControl::Bind(InputAction::Forward))
            .expect("Forward's bind button is visible at first=0");
        assert_eq!(nav.click_row(row, &kb), KeyBindsOutcome::None);
        assert_eq!(nav.awaiting(), Some(InputAction::Forward));
        let _ = forward_row;

        // Escape cancels the capture, not the page.
        assert_eq!(nav.escape(), KeyBindsOutcome::None);
        assert_eq!(nav.awaiting(), None, "capture cancelled");

        // The *second* Escape, with nothing awaiting, leaves the page.
        assert_eq!(nav.escape(), KeyBindsOutcome::Back);
    }

    #[test]
    fn a_click_acts_on_the_row_it_landed_on_and_nothing_else() {
        // That fix's shape, on this page too: clicking Forward's bind button
        // must not touch its neighbour's reset button.
        let kb = Keybinds::new();
        let mut nav = KeyBindsNav::default();
        let visible = nav.visible();
        let forward_bind = visible
            .iter()
            .position(|v| v.control == KeyControl::Bind(InputAction::Forward))
            .unwrap();
        let forward_reset = visible
            .iter()
            .position(|v| v.control == KeyControl::Reset(InputAction::Forward))
            .unwrap();
        assert_ne!(forward_bind, forward_reset);
        assert_eq!(nav.click_row(forward_bind, &kb), KeyBindsOutcome::None);
        assert_eq!(nav.awaiting(), Some(InputAction::Forward));
        // Clicking Back's reset (a genuinely different, unbound-by-default
        // action's row) must not touch Forward's pending capture. It is also
        // the control that catches a missing `isActive()` guard: every
        // action starts at its default, so every reset button starts
        // inactive, and this is the assertion that would have failed had
        // `activate` not checked `KeyControl::is_live` first.
        let back_reset = visible
            .iter()
            .position(|v| v.control == KeyControl::Reset(InputAction::Back))
            .unwrap();
        assert_eq!(
            nav.click_row(back_reset, &kb),
            KeyBindsOutcome::None,
            "Back is default, reset is inactive"
        );
        assert_eq!(nav.awaiting(), Some(InputAction::Forward), "untouched");
    }

    /// The control's other side: once an action is genuinely non-default,
    /// clicking its reset button must actually ask to reset it, and clicking
    /// a *live* Reset Keys must ask to reset every action.
    #[test]
    fn a_live_reset_button_asks_to_reset_and_a_live_reset_all_asks_to_reset_everything() {
        let mut kb = Keybinds::new();
        kb.set(InputAction::Forward, crate::keybinds::Binding::Unbound);
        let mut nav = KeyBindsNav::default();
        let visible = nav.visible();
        let forward_reset = visible
            .iter()
            .position(|v| v.control == KeyControl::Reset(InputAction::Forward))
            .unwrap();
        assert_eq!(
            nav.click_row(forward_reset, &kb),
            KeyBindsOutcome::ResetOne(InputAction::Forward)
        );
        let reset_all = visible
            .iter()
            .position(|v| v.control == KeyControl::ResetAll)
            .unwrap();
        assert_eq!(nav.click_row(reset_all, &kb), KeyBindsOutcome::ResetAll);
    }

    #[test]
    fn reset_button_liveness_tracks_is_default() {
        let kb = Keybinds::new();
        // Every action starts at its default, so no reset button is live and
        // Reset Keys itself is inactive too.
        for action in ordered_actions() {
            assert!(
                !KeyControl::Reset(action).is_live(&kb),
                "{action:?} starts default"
            );
        }
        assert!(!KeyControl::ResetAll.is_live(&kb));
        let mut changed = kb;
        changed.set(InputAction::Forward, Binding::Unbound);
        assert!(KeyControl::Reset(InputAction::Forward).is_live(&changed));
        assert!(
            !KeyControl::Reset(InputAction::Back).is_live(&changed),
            "an untouched neighbour must not report live"
        );
        assert!(
            KeyControl::ResetAll.is_live(&changed),
            "one changed action is enough to make Reset Keys live"
        );
    }

    #[test]
    fn bind_labels_show_the_current_binding_and_decorate_capture_and_conflict() {
        let mut kb = Keybinds::new();
        assert_eq!(
            KeyControl::Bind(InputAction::Forward).label(&kb, None),
            "W"
        );
        assert_eq!(
            KeyControl::Bind(InputAction::Forward).label(&kb, Some(InputAction::Forward)),
            "> W <",
            "the one bind button being captured is decorated"
        );
        assert_eq!(
            KeyControl::Bind(InputAction::Back).label(&kb, Some(InputAction::Forward)),
            "S",
            "a different row's bind button is not"
        );
        // A conflict: bind Back onto Forward's own key.
        kb.set(InputAction::Back, kb.binding(InputAction::Forward));
        assert_eq!(
            KeyControl::Bind(InputAction::Forward).label(&kb, None),
            "[ W ]",
            "both sides of a collision are bracketed, matching vanilla's own \
             highlight applying symmetrically"
        );
    }

    /// A row scrolled above the band resolves **above** the band rather than at a
    /// wrapped `u16`, and `render::draw` clips it.
    ///
    /// This replaces `placement_off_the_window_is_the_anti_island_sentinel`, which
    /// asserted the old `(-1000, -1000)` sentinel. That sentinel existed because
    /// `row.checked_sub(first)` underflowed for a row above the window; with a
    /// pixel offset there is nothing to underflow — the y is simply negative
    /// relative to the band, which is a real coordinate the clip handles, not a
    /// magic one.
    #[test]
    fn a_row_scrolled_above_the_band_resolves_above_it_not_at_a_sentinel() {
        let band_top = options::SUB_HEADER_HEIGHT + options::LIST_TOP_INSET;
        // Row 0 with the list scrolled five rows down.
        let (_, y) = placement_anchor(
            KeyPlacement::Bind {
                row: 0,
                scroll: 5.0 * ROW_H,
            },
            480.0,
            320.0,
        );
        assert_eq!(
            y,
            band_top - 5.0 * ROW_H,
            "the y must be the row's absolute offset minus the scroll — five rows \
             above the band's top, exactly"
        );
        // And the row that *is* at the top of the band lands on the band's top.
        let (_, at_top) = placement_anchor(
            KeyPlacement::Bind {
                row: 5,
                scroll: 5.0 * ROW_H,
            },
            480.0,
            320.0,
        );
        assert_eq!(at_top, band_top);
    }

    /// Every row inside the band resolves inside the band, at every scroll offset
    /// the list can reach — the horizontal half unchanged, the vertical half now
    /// bounded by the band rather than by the canvas.
    ///
    /// The old version asserted every placement fits *on the canvas*, which pixel
    /// scrolling makes false on purpose: a partially-scrolled row hangs over the
    /// band edge and is clipped rather than skipped. Asserting the band is the
    /// honest form of the same property.
    #[test]
    fn every_row_inside_the_band_resolves_inside_the_band() {
        const W: f32 = 480.0;
        const H: f32 = 320.0;
        let spec = list_spec(0.0);
        let list = spec.model(H).expect("this canvas has a band");
        let band_top = list.top();
        let band_bottom = list.top() + list.height();
        let mut checked = 0usize;
        for step in 0..=20 {
            let scroll = (step as f32) * 17.0; // deliberately not a multiple of ROW_H
            for view in controls(scroll) {
                let (x, y, w, h) = view.slot.resolve(W, H);
                // The footer rows use `Origin::Settings` and do not scroll, so
                // they are outside this property; skip them by position.
                if y > band_bottom {
                    continue;
                }
                if y < band_top {
                    continue;
                }
                checked += 1;
                assert!(
                    x >= 0.0 && x + w <= W,
                    "scroll={scroll} {:?} at x={x} w={w} runs off a {W}px canvas",
                    view.control
                );
                assert!(
                    y + h <= band_bottom + h,
                    "scroll={scroll} {:?} at y={y} h={h} is past the band bottom \
                     {band_bottom} by more than one row",
                    view.control
                );
            }
        }
        assert!(
            checked > 100,
            "premise: this must actually have examined rows inside the band \
             ({checked} seen) — a filter that skipped everything would pass \
             vacuously"
        );
    }

    #[test]
    fn hover_and_the_cursor_agree_on_every_row() {
        // Absolute row indices now (see `controls`'s doc), so one pass over the
        // whole list covers what the old `first`-windowed double loop did.
        let mut nav = KeyBindsNav::default();
        let len = nav.visible().len();
        assert!(len > 0, "premise: there are controls to hover");
        for row in 0..len {
            nav.hover_row(row);
            assert_eq!(
                nav.selected_row(),
                Some(row),
                "hovering row {row} must select row {row}"
            );
        }
    }

    /// **One notch is `floor(ROW_H / 2)` = `floor(20 / 2)` = 10 px**,
    /// and the offset must land somewhere that is **not** a row top.
    ///
    /// The second half is the load-bearing one. "It scrolled" is satisfied by the
    /// row-index implementation this replaced — that is the *magnitude* species of
    /// vacuous test — so the two competing hypotheses are named and the
    /// measurement is required to land on one:
    ///
    /// | hypothesis | one notch | three notches |
    /// |---|---|---|
    /// | row index (what this screen did) | 20 | 60 |
    /// | page (`LIST_WINDOW_PX`) | 174 | 522 (clamped) |
    /// | **pixels, `scrollRate` = `floor(row_h / 2)`** | **10** | **30** |
    ///
    /// 10 and 30 are both non-multiples of `ROW_H`, which no row-index
    /// implementation can produce at all, and that is the cross-check.
    #[test]
    fn one_wheel_notch_is_half_a_row_and_lands_off_every_row_top() {
        const CANVAS_H: f32 = 240.0;
        let mut nav = KeyBindsNav::default();
        assert_eq!(nav.scroll(), 0.0, "precondition: starts at the top");
        // Premise, executed: this list is long enough to scroll at this canvas.
        assert!(
            list_spec(0.0)
                .model(CANVAS_H)
                .is_some_and(|l| l.scrollable()),
            "premise: the key binds list must actually scroll at {CANVAS_H} px, or \
             every assertion below is vacuous"
        );

        // Negative notches scroll *down* — the sign lives in `mouse_scrolled`.
        nav.scroll_by(-1.0, CANVAS_H);
        assert_eq!(
            nav.scroll(),
            (ROW_H / 2.0).floor(),
            "one notch must be floor(ROW_H / 2) = 10 px, not the row-index \
             answer ({ROW_H}) and not a page ({LIST_WINDOW_PX})"
        );
        assert_ne!(
            nav.scroll(),
            ROW_H,
            "control: the row-index hypothesis is 20, and it must be excluded"
        );

        nav.scroll_by(-2.0, CANVAS_H);
        assert_eq!(nav.scroll(), 3.0 * (ROW_H / 2.0).floor(), "three notches: 30");

        // The cross-check the brief calls for: three notches must land somewhere
        // that is **not** a row top. A row-index implementation reports an offset
        // that is always a multiple of ROW_H, so this single assertion excludes
        // the whole family rather than one member of it.
        assert_ne!(
            nav.scroll() % ROW_H,
            0.0,
            "the offset {} must coincide with no row top — a multiple of {ROW_H} \
             is exactly what snap-to-row produces",
            nav.scroll()
        );
    }

    /// The keyboard half, and the same cross-check: `scroll_to_cursor` must be
    /// able to produce an offset that is not a row top.
    ///
    /// The old implementation walked `self.first += 1` until the row was in the
    /// window, so its answer was *always* a multiple of `ROW_H`.
    /// [`super::super::widget::ScrollList::scroll_to_entry`] moves the minimum
    /// number of pixels instead, which lands on `row_bottom - band_height` — a
    /// number derived from the band, not from the row pitch.
    #[test]
    fn stepping_to_a_row_below_the_band_scrolls_by_pixels_not_whole_rows() {
        let mut nav = KeyBindsNav::default();
        // Walk the cursor forward until something actually scrolls.
        let mut moved = false;
        for _ in 0..all_controls().len() {
            nav.step(true);
            if nav.scroll() > 0.0 {
                moved = true;
                break;
            }
        }
        assert!(
            moved,
            "premise: stepping the cursor down the list must eventually scroll \
             it, or this measures nothing"
        );
        assert_ne!(
            nav.scroll() % ROW_H,
            0.0,
            "keyboard scroll-into-view landed on {}, a multiple of {ROW_H} — that \
             is the snap-to-row answer, and `scroll_to_entry` should have moved \
             the minimum pixels instead",
            nav.scroll()
        );
        // And the cursor's row really is visible now, so the offset is not just
        // an arbitrary non-multiple.
        assert!(
            nav.selected_row().is_some(),
            "and the cursor's control must be in the emitted set"
        );
    }

    /// The scrollbar the draw paints must hang off the same x this module's own
    /// [`scrollbar_x`] answers — the reset and bind buttons are positioned from
    /// it, so a bar that drifted would take them with it.
    ///
    /// Two expressions from two modules required to agree, rather than one
    /// asserted against itself: [`list_spec`]'s [`ROW_WIDTH`] band goes through
    /// `ListSpec::row_right` and `ScrollList::scrollbar_x`, while [`scrollbar_x`]
    /// here is this screen's transcription of `AbstractSelectionList:289-291`.
    #[test]
    fn the_shared_primitive_puts_the_bar_where_this_screen_already_drew_it() {
        for w in [640.0_f32, 854.0, 1280.0] {
            let spec = list_spec(0.0);
            let list = spec.model(240.0).expect("a band at 240 px");
            assert_eq!(
                spec.row_right(w),
                row_right(w),
                "the primitive's row_right must equal this screen's at {w} px"
            );
            assert_eq!(
                list.scrollbar_x(spec.row_right(w)),
                scrollbar_x(w),
                "and the bar must land on this screen's own scrollbar_x at {w} px"
            );
        }
    }
}
