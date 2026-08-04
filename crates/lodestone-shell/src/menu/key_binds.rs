//! The Controls menu's Key Binds screen (issue #15) — vanilla's
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
//! screen for. So this module is the second list-widget kind #392's plan
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
//!   (`KeyBindsList.java:36,45`, `AbstractSelectionList.java:119`), so the
//!   window math here has no first-entry special case.
//! - `getRowWidth() = 340` (`:59-61`). `getRowLeft() = x + width/2 -
//!   rowWidth/2` (`AbstractSelectionList.java:372-374`), and this list's `x`
//!   is 0 (the whole canvas width), so [`ROW_LEFT`] is `width/2 - 170`.
//! - `scrollBarX() = getRowRight() + scrollbarWidth() + 2`
//!   (`AbstractSelectionList.java:289-291`), and `scrollbarWidth()` is the
//!   record default `6` (`AbstractScrollArea.java:145`) — `width/2 + 170 + 8`.
//! - `KeyEntry.extractContent` (`:129-143`): `resetButtonX = scrollBarX() -
//!   50 - 10`, `changeButtonX = resetButtonX - 5 - 75`, `buttonY =
//!   getContentY() - 2`. `getContentY() = getY() + 2` (`AbstractSelectionList.java:481-483`),
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
//!   (150 px) buttons (`KeyBindsScreen.java:47-49`) — **identical** in shape
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
/// plus `scrollBarX()`'s own `+ 2` (`AbstractSelectionList.java:289-291`).
const SCROLLBAR_GAP: f32 = 6.0 + 2.0;
/// `getContentX()`'s `+2` (`AbstractSelectionList.java:477-479`).
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

/// Rows per window. Simpler than `options.rs`'s version: every row here is the
/// same height, so there is no header-padding case to walk one entry at a
/// time — a floor division is exact.
#[must_use]
pub fn visible_rows_len() -> usize {
    (LIST_WINDOW_PX / ROW_H).floor().max(1.0) as usize
}

/// `getRowLeft()` on a `width`-wide canvas (`AbstractSelectionList.java:372-374`,
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

/// The rows visible with `first` at the top of the window.
#[must_use]
pub fn visible_rows(first: usize) -> std::ops::Range<usize> {
    let len = all_rows().len();
    let end = (first + visible_rows_len()).min(len);
    first.min(len)..end
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
    /// with vanilla's own `"> {name} <"` (`KeyBindsList.java:187-195`) —
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
/// here shares the same `{row, first}` shape because every row is the same
/// height; only the x differs per widget, and that is a `match` in
/// [`placement_anchor`] rather than a fourth field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPlacement {
    /// The bind button, at absolute row `row` (an index into [`all_rows`]),
    /// with `first` scrolled to the top of the window.
    Bind { row: u16, first: u16 },
    /// The per-row reset button, same row.
    Reset { row: u16, first: u16 },
    /// The action's name label (not a [`KeyControl`] — a
    /// [`super::render::MenuLabel`], like an `OptionsList` header).
    Name { row: u16, first: u16 },
    /// The category header label, also a [`super::render::MenuLabel`].
    Category { row: u16, first: u16 },
}

impl KeyPlacement {
    fn row_first(self) -> (u16, u16) {
        match self {
            KeyPlacement::Bind { row, first }
            | KeyPlacement::Reset { row, first }
            | KeyPlacement::Name { row, first }
            | KeyPlacement::Category { row, first } => (row, first),
        }
    }
}

/// The top-left of the widget a [`KeyPlacement`] names, on a `width`×`height`
/// canvas. [`super::render::Origin::KeyBinds`]'s whole body — see
/// [`super::options::placement_anchor`] for the sibling this mirrors.
#[must_use]
pub fn placement_anchor(placement: KeyPlacement, width: f32, _height: f32) -> (f32, f32) {
    let (row, first) = placement.row_first();
    // A row scrolled above the window (or a stale placement from a page that
    // no longer has this many rows) resolves off-canvas rather than
    // underflowing — the same anti-island sentinel
    // `super::options::placement_anchor` uses for `Placement::Root`.
    let Some(index) = row.checked_sub(first) else {
        return (-1000.0, -1000.0);
    };
    let row_y = options::SUB_HEADER_HEIGHT + options::LIST_TOP_INSET + f32::from(index) * ROW_H;
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

/// Every control on screen, scrolled so row `first` is at the top of the
/// window, then the footer. Mirrors [`super::options::controls`].
#[must_use]
pub fn controls(first: usize) -> Vec<KeyControlView> {
    let mut out = Vec::new();
    let rows = all_rows();
    for row in visible_rows(first) {
        if let Row::Action(action) = rows[row] {
            out.push(KeyControlView {
                control: KeyControl::Bind(action),
                slot: Slot {
                    origin: Origin::KeyBinds(KeyPlacement::Bind {
                        row: row as u16,
                        first: first as u16,
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
                        first: first as u16,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyBindsNav {
    cursor: usize,
    first: usize,
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

    #[must_use]
    pub fn first(&self) -> usize {
        self.first
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
        controls(self.first)
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
        let rows = all_rows();
        if row < self.first {
            self.first = row;
            return;
        }
        while !visible_rows(self.first).contains(&row) {
            if self.first + 1 >= rows.len() {
                break;
            }
            self.first += 1;
        }
    }

    /// Puts the cursor on the control at visible row `row` — the mouse's
    /// half. Mirrors [`super::options::SettingsNav::hover_row`].
    pub fn hover_row(&mut self, row: usize) {
        let visible = controls(self.first);
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
    /// own control, do not route through Enter" rule (issue #391's fix,
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
    let all_rows_list = all_rows();
    for row in visible_rows(nav.first()) {
        match all_rows_list[row] {
            Row::Category(category) => labels.push(MenuLabel {
                text: category_caption(category).to_string(),
                origin: Origin::KeyBinds(KeyPlacement::Category {
                    row: row as u16,
                    first: nav.first() as u16,
                }),
                dx: 0.0,
                dy: 0.0,
                align: Align::Centre,
                colour: super::widget::ACTIVE_LABEL,
                scale: 1.0,
            }),
            Row::Action(action) => labels.push(MenuLabel {
                text: action_caption(action).to_string(),
                origin: Origin::KeyBinds(KeyPlacement::Name {
                    row: row as u16,
                    first: nav.first() as u16,
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
        ..Default::default()
    }
}

/// The category's caption, verbatim from `en_us.json`'s **real** 26.2 key —
/// `key.category.minecraft.<id>` (`KeyMapping.Category.label`,
/// `KeyMapping.java:227-229`, `Identifier.toLanguageKey(String)`). **Not**
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
        // every caption above. Added with the verbs themselves (issue #16); this
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
        // 29 since issue #16 added Pick Block and Take Screenshot. Vanilla puts
        // them in Gameplay and Misc respectively (`Options.java:669,675`), both
        // categories that were already non-empty — so the *category* list above
        // is unchanged and only the action count moves. Deriving this from
        // `InputAction::ALL.len()` would make the assertion vacuous: it would
        // then agree with itself no matter which actions the screen forgot.
        assert_eq!(actions.len(), 29, "every InputAction, once each");
        assert_eq!(rows.len(), 6 + 29);
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
    /// (`KeyMapping.java:204-211`). `InputAction::ALL`'s own *declaration*
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
        assert_eq!(all.len(), 29 * 2 + 2, "29 binds, 29 resets, ResetAll, Done");
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
                "cursor {} off-window at first={}",
                nav.cursor(),
                nav.first()
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
        nav.first = 0;
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
        // #391's shape, on this page too: clicking Forward's bind button
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

    #[test]
    fn placement_off_the_window_is_the_anti_island_sentinel() {
        // A row scrolled above `first` must not underflow into a huge y —
        // the same guard `super::options::placement_anchor` has for
        // `Placement::Root`.
        let (x, y) = placement_anchor(KeyPlacement::Bind { row: 0, first: 5 }, 480.0, 320.0);
        assert!(x < 0.0 && y < 0.0, "off-canvas sentinel, not a wrapped u16");
    }

    #[test]
    fn every_visible_placement_resolves_on_screen() {
        // `Slot::resolve` dispatches on whichever `Origin` the row actually
        // carries — `Origin::KeyBinds` for the scrolled content,
        // `Origin::Settings` for the footer (see `footer_controls`) — so one
        // call covers both without the test needing to know which is which.
        for first in 0..all_rows().len() {
            for view in controls(first) {
                let (x, y, w, h) = view.slot.resolve(480.0, 320.0);
                assert!(
                    x >= 0.0 && y >= 0.0 && x + w <= 480.0 && y + h <= 320.0,
                    "first={first} {:?} at ({x}, {y}) size {w}x{h}",
                    view.control
                );
            }
        }
    }

    #[test]
    fn hover_and_the_cursor_agree_on_every_visible_row() {
        for first in 0..all_rows().len() {
            let mut nav = KeyBindsNav {
                first,
                ..KeyBindsNav::default()
            };
            let len = nav.visible().len();
            for row in 0..len {
                nav.first = first;
                nav.hover_row(row);
                assert_eq!(
                    nav.selected_row(),
                    Some(row),
                    "first={first}: hovering row {row} must select row {row}"
                );
            }
        }
    }
}
