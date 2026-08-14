//! Vanilla's `ConfirmScreen`: a question, a warning naming the thing at risk,
//! and two buttons — the screen an irreversible action has to pass through.
//!
//! ## What it is
//!
//! `client/gui/screens/ConfirmScreen.java`, as far as this shell's frame model
//! goes. That is what
//! made it exist, and the reason is worth stating before the mechanics, because
//! it is the whole design:
//!
//! **Deleting a world is irreversible, so the affirmative control must not be
//! the control the player just pressed.** Arming the world list's own Delete
//! button and treating a second press as confirmation is
//! *deletable-by-double-click* — a player who double-clicks, or whose mouse
//! chatters, loses a world. `crate::saves`'s module doc carried that argument for
//! a whole release with no screen to discharge it; this is the screen.
//!
//! What makes it safe is **geometry plus focus**, not a timer:
//!
//! - the affirmative button is a *different control on a different screen*, and
//!   its rect does not overlap the Delete button's — the two are 177 px apart at
//!   the reference canvas, because vanilla centres this block in the screen while
//!   `SelectWorldScreen` pins Delete to a footer band. So a second click where
//!   the first one landed hits **nothing**;
//! - **nothing is focused when this screen opens.** Vanilla's `ConfirmScreen.init`
//!   (`:45-56`) calls no `setInitialFocus`, unlike `SelectWorldScreen.java`,
//!   so Enter immediately after opening presses nothing. Reproducing that is both
//!   faithful *and* the safe direction: a held Enter cannot roll through the
//!   confirmation.
//!
//! `the_confirmation_cannot_be_fired_by_a_second_click_where_delete_was` and
//! `enter_immediately_after_opening_the_confirmation_does_nothing` are the gates
//! on those two facts.
//!
//! ## How it works
//!
//! [`confirm_block`] arranges vanilla's own tree — `LinearLayout.vertical()
//! .spacing(8)` holding a title `StringWidget`, the message, and a
//! `LinearLayout.horizontal().spacing(4)` of two `Button.DEFAULT_WIDTH` buttons
//! with `paddingTop(16)` (`ConfirmScreen.java`) — and
//! `FrameLayout.centerInRectangle`s it in the canvas (`:59-62`). Every leaf is
//! then read back as a [`ConfirmPlacement`], which [`Origin::Confirm`] resolves,
//! so the buttons' rects come out of the arranged tree rather than out of
//! restated numbers. That matters twice over here: the "cannot be
//! double-clicked" property is a statement about two rects, and a restated one
//! could be right while the drawn one was not.
//!
//! [`ConfirmNav`] is the input half — two [`Widget`]s, a [`FocusSet`], and the
//! [`ConfirmRequest`] saying what is being confirmed. It holds no filesystem
//! anything: [`super::nav::MenuNav`] owns the saves root, so it is what acts on a
//! [`ConfirmOutcome::Yes`], exactly as it is what acts on
//! `CreateWorldOutcome::Create`.
//!
//! ## Two deliberate deviations
//!
//! - **The message is one clipped line, not a `MultiLineTextWidget`.** Vanilla
//!   wraps it to `width - 50` over up to 15 rows (`:67-69`), which makes the
//!   block's *height* — and therefore the buttons' y — a function of the font.
//!   There is no font at arrange time here (the same reason every title cell in
//!   this tree is zero-width), so a wrap-dependent height would put the buttons
//!   somewhere the hit-test could not predict. Instead the block reserves one
//!   9 px line and [`ConfirmNav::new`] clips the *interpolated world name* until
//!   the whole sentence measures inside the block, using
//!   [`super::render::text_px`] — the same fixed advance the jar-less draw
//!   measures with. `the_confirmation_message_fits_its_own_block` is the gate.
//! - **No `setDelay`/`delayTicker`.** Vanilla's is not used by the world-delete
//!   path at all (`WorldSelectionList.deleteWorld` (`:619-637`) constructs a
//!   plain `ConfirmScreen`); it exists for the backup-and-experimental-world
//!   confirmations. It also could not work here — `frame_for` is a pure function
//!   of the UI state with no tick input — so porting the field would be a
//!   constant claiming a delay happened.
//!
//! ## Dependencies
//!
//! [`super::layout`] for the container tree, [`super::widget`] for the buttons and
//! [`super::focus`] for the traversal. No filesystem, no version family.

use super::focus::{FocusChildren, FocusSet, FocusTarget, KeyEvent, KeyOutcome};
use super::layout::{self, LayoutSettings, LinearLayout};
use super::nav::MenuKey;
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot};
use super::widget::{self, LayoutElement, Widget};

/// `selectWorld.deleteQuestion` (`en_us.json`) — the title line.
pub const DELETE_QUESTION: &str = "Are you sure you want to delete this world?";
/// `selectWorld.deleteWarning` (`en_us.json`), whose `%s` is the world's
/// **display** name — `LevelSummary.getLevelName()`, not the folder
/// (`WorldSelectionList.java`).
pub const DELETE_WARNING: &str = "'%s' will be lost forever! (A long time!)";
/// `selectWorld.deleteButton` — vanilla's affirmative label here is **"Delete"**,
/// not `gui.yes` (`WorldSelectionList.java`). The wording is part of the
/// safety: a button saying `Yes` answers a question the player may not have read.
pub const DELETE_BUTTON: &str = "Delete";
/// `CommonComponents.GUI_CANCEL` = `gui.cancel` (`:635`).
pub const CANCEL_BUTTON: &str = "Cancel";

/// `LinearLayout.vertical().spacing(8)` (`ConfirmScreen.java`), and the same 8
/// on the message-to-buttons gap.
const BLOCK_SPACING: i32 = 8;
/// `LinearLayout.horizontal().spacing(4)` for the button row (`:51`).
const BUTTON_SPACING: i32 = 4;
/// `buttonLayout.defaultCellSetting().paddingTop(16)` (`:52`).
const BUTTON_PADDING_TOP: i32 = 16;
/// A `StringWidget`'s height (`StringWidget.java`) — the title, and the one
/// message line this port reserves (see the module doc's first deviation).
const LINE_H: f32 = 9.0;

/// The affirmative button's row index, and its [`FocusSet`] id.
///
/// `addButtons` adds yes **then** no (`ConfirmScreen.java`), so this is
/// vanilla's own order — which is also the tab order, since nothing overrides
/// `getTabOrderGroup`.
pub const YES_ROW: usize = 0;
/// The cancel button's row index. See [`YES_ROW`].
pub const NO_ROW: usize = 1;
/// How many rows this screen has.
pub const ROW_COUNT: usize = 2;

/// The canvas the widgets' bounds are seeded at — same role as
/// [`super::world_select`]'s: the widgets outlive a frame, so arrow navigation
/// needs real bounds before any frame exists. The block is centred, so the pair
/// is only ever used as a *relative* position and the choice of canvas cannot
/// change which button is left of which.
const SEED_CANVAS: (f32, f32) = (854.0, 480.0);

/// Which leaf of the arranged block a [`Slot`] means.
///
/// A data-carrying [`Origin`] for the same reason [`Origin::Social`] is one: the
/// position comes out of an arranged tree rather than from an expression, and the
/// tree is centred in a canvas that is only known at draw time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmPlacement {
    /// The title `StringWidget` — zero-width, so its rect *is* the text's centre.
    Title,
    /// The message line. Zero-width for the same reason.
    Message,
    /// Button `index`: 0 is [`YES_ROW`], 1 is [`NO_ROW`].
    Button(u8),
}

/// Vanilla's `ConfirmScreen.init` as a real [`LinearLayout`] tree, arranged and
/// then centred in a `width`×`height` canvas — `repositionElements`' own two
/// steps (`ConfirmScreen.java`).
///
/// Returns the four leaf rects in `visitWidgets` order: title, message, yes, no.
/// Built per call rather than cached, because `centerInRectangle` reads the
/// canvas — the same reason `options::root_widget_rects` is not cached. It is four
/// small boxes.
#[must_use]
fn confirm_rects(width: f32, height: f32) -> Vec<(f32, f32, f32, f32)> {
    let string_widget = || -> Box<dyn LayoutElement> {
        Box::new(Widget::new(0.0, 0.0, 0.0, LINE_H, ""))
    };
    let mut root = LinearLayout::vertical().spacing(BLOCK_SPACING);
    {
        let baseline = root.default_cell_setting();
        *baseline = baseline.align_horizontally_center();
    }
    // `this.layout.addChild(new StringWidget(this.title, this.font))` (`:48`).
    root.add_child(string_widget());
    // `this.addMessage()` (`:49`) — one reserved line, see the module doc.
    root.add_child(string_widget());
    // `LinearLayout buttonLayout = this.layout.addChild(LinearLayout.horizontal()
    // .spacing(4)); buttonLayout.defaultCellSetting().paddingTop(16);` (`:51-52`)
    let mut buttons = LinearLayout::horizontal().spacing(BUTTON_SPACING);
    for _ in 0..ROW_COUNT {
        buttons.add_child_settings(
            Box::new(Widget::button(
                0.0,
                0.0,
                widget::DEFAULT_WIDTH,
                widget::DEFAULT_HEIGHT,
                "",
            )),
            LayoutSettings::defaults().padding_top(BUTTON_PADDING_TOP),
        );
    }
    root.add_child(Box::new(buttons));
    root.arrange_elements();
    // `FrameLayout.centerInRectangle(this.layout, this.getRectangle())` (`:61`).
    layout::align_in_rectangle(&mut root, 0.0, 0.0, width, height, 0.5, 0.5);
    layout::widget_rects(&root)
}

/// The arranged block's own width, for a caller that has to clip text to it.
///
/// Derived from the arranged tree rather than stated as `2 * 150 + 4`, so the
/// clip budget and the drawn block cannot disagree — the button row is what sets
/// the block's width, and a change to `Button.DEFAULT_WIDTH` must move both.
#[must_use]
pub fn block_width() -> f32 {
    let rects = confirm_rects(SEED_CANVAS.0, SEED_CANVAS.1);
    let left = rects.iter().map(|r| r.0).fold(f32::INFINITY, f32::min);
    let right = rects
        .iter()
        .map(|r| r.0 + r.2)
        .fold(f32::NEG_INFINITY, f32::max);
    (right - left).max(0.0)
}

/// The anchor for one leaf of the block, on a `width`×`height` canvas — the body
/// of [`Origin::Confirm`].
#[must_use]
pub fn placement_anchor(placement: ConfirmPlacement, width: f32, height: f32) -> (f32, f32) {
    let rects = confirm_rects(width, height);
    let index = match placement {
        ConfirmPlacement::Title => 0,
        ConfirmPlacement::Message => 1,
        ConfirmPlacement::Button(i) => 2 + usize::from(i),
    };
    // Off-canvas rather than a panic in a draw path, for `placement_anchor`'s own
    // reason in `options.rs`: a table that no longer describes the screen must
    // fail in a gate, not in the renderer.
    let (x, y, ..) = rects.get(index).copied().unwrap_or((-1000.0, -1000.0, 0.0, 0.0));
    (x, y)
}

/// The [`Slot`] for row `row` — the rect the draw, the seeded widget and
/// `app.rs`'s hit-test all read, so they cannot drift apart.
#[must_use]
pub fn row_slot(row: usize) -> Slot {
    let index = u8::try_from(row.min(NO_ROW)).unwrap_or(0);
    Slot {
        origin: Origin::Confirm(ConfirmPlacement::Button(index)),
        dx: 0.0,
        dy: 0.0,
        w: widget::DEFAULT_WIDTH,
        h: widget::DEFAULT_HEIGHT,
    }
}

/// What this confirmation is *for*.
///
/// An enum with one variant rather than a bare `dir_name`, because the whole
/// value of a generic confirm screen is that the next irreversible action
/// (`EditWorldScreen`'s reset, a re-create that overwrites) adds a variant here
/// instead of a screen — and because [`super::nav::MenuNav::apply_confirm`]'s
/// `match` then makes "opened a confirmation and forgot to act on it" a compile
/// error rather than a silently harmless Yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmRequest {
    /// Delete the world in `dir_name`. Carries the **folder** name,
    /// which is what [`crate::saves::delete_world_in`] resolves through
    /// [`crate::saves::world_dir_in`], and the display name only so the warning
    /// can quote it.
    DeleteWorld {
        /// The folder under the saves root.
        dir_name: String,
        /// `LevelSummary.getLevelName()`, for the message.
        display_name: String,
    },
}

/// What one key or click did to the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmOutcome {
    /// A widget or the focus layer dealt with it; nothing for the caller to do.
    Handled,
    /// The negative answer — Cancel, Escape, or a click on Cancel. Vanilla's
    /// `callback.accept(false)`.
    No,
    /// The affirmative answer. Vanilla's `callback.accept(true)`; the caller
    /// reads [`ConfirmNav::request`] to find out what it agreed to.
    Yes,
}

/// The two buttons, in one struct so [`FocusSet`] can borrow them while
/// [`ConfirmNav`] borrows the set — the split
/// [`super::world_select::WorldSelectWidgets`] documents.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmWidgets {
    /// Yes then No, in [`YES_ROW`]/[`NO_ROW`] order.
    pub buttons: [Widget; ROW_COUNT],
}

impl FocusChildren for ConfirmWidgets {
    fn get(&self, id: usize) -> Option<&dyn FocusTarget> {
        self.buttons.get(id).map(|w| w as &dyn FocusTarget)
    }

    fn get_mut(&mut self, id: usize) -> Option<&mut dyn FocusTarget> {
        self.buttons.get_mut(id).map(|w| w as &mut dyn FocusTarget)
    }
}

/// One live confirmation screen.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmNav {
    /// The two buttons. Public so [`super::render`] can read their labels.
    pub widgets: ConfirmWidgets,
    focus: FocusSet,
    hovered: Option<usize>,
    title: String,
    message: String,
    request: ConfirmRequest,
}

impl Default for ConfirmNav {
    fn default() -> Self {
        Self::delete_world("", "")
    }
}

impl ConfirmNav {
    /// The world-delete confirmation for the world in `dir_name` displayed as
    /// `display_name` — `WorldSelectionList.WorldListEntry.deleteWorld`'s own
    /// `ConfirmScreen` (`:619-637`).
    ///
    /// **Nothing is focused.** See the module doc: vanilla's `ConfirmScreen.init`
    /// sets no initial focus, and here that is also what stops a held Enter
    /// carrying through from the world list into a deletion.
    #[must_use]
    pub fn delete_world(dir_name: &str, display_name: &str) -> Self {
        let button = |row: usize, label: &str| {
            let (x, y, w, h) = row_slot(row).resolve(SEED_CANVAS.0, SEED_CANVAS.1);
            Widget::button(x, y, w, h, label)
        };
        let widgets = ConfirmWidgets {
            buttons: [
                button(YES_ROW, DELETE_BUTTON),
                button(NO_ROW, CANCEL_BUTTON),
            ],
        };
        let mut focus = FocusSet::new();
        for row in 0..ROW_COUNT {
            focus.add_renderable_widget(row);
        }
        Self {
            widgets,
            focus,
            hovered: None,
            title: DELETE_QUESTION.to_string(),
            message: warning_for(display_name),
            request: ConfirmRequest::DeleteWorld {
                dir_name: dir_name.to_string(),
                display_name: display_name.to_string(),
            },
        }
    }

    /// The title line — `selectWorld.deleteQuestion`.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The warning line, with the world's name already interpolated and clipped
    /// to the block. See the module doc's first deviation.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// What a [`ConfirmOutcome::Yes`] agrees to.
    #[must_use]
    pub fn request(&self) -> &ConfirmRequest {
        &self.request
    }

    /// The focused row, or `None` — which is the state this screen **opens** in.
    #[must_use]
    pub fn focused_row(&self) -> Option<usize> {
        self.focus.focused()
    }

    /// The row the cursor is over. Separate from focus for
    /// [`super::world_select::WorldSelectNav::hovered`]'s reason, and joined only
    /// where the sprite is picked (`isHoveredOrFocused()`).
    #[must_use]
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// The mouse moved onto row `row`. Records hover only, never focus.
    pub fn hover(&mut self, row: usize) {
        if row < ROW_COUNT {
            self.hovered = Some(row);
        }
    }

    /// A click on row `row`.
    ///
    /// Its own arm rather than "hover then Enter", for that fix's reason — and here
    /// the translation would be actively dangerous: a hover that moved focus onto
    /// the affirmative button would make the *next* Enter delete.
    pub fn click_row(&mut self, row: usize) -> ConfirmOutcome {
        if row >= ROW_COUNT {
            return ConfirmOutcome::Handled;
        }
        self.focus.set_focused(&mut self.widgets, Some(row));
        Self::answer(row)
    }

    /// One key, in vanilla's `Screen.keyPressed` order.
    ///
    /// Escape is `callback.accept(false)` (`ConfirmScreen.java`) — note
    /// `shouldCloseOnEsc()` is `false` on this screen precisely so that the
    /// *callback* runs rather than a bare `onClose`, which is why this is a `No`
    /// and not a silent dismissal.
    pub fn handle_key(&mut self, key: MenuKey) -> ConfirmOutcome {
        if key == MenuKey::Escape {
            return ConfirmOutcome::No;
        }
        if let MenuKey::Char(ch) = key {
            self.focus.char_typed(&mut self.widgets, ch);
            return ConfirmOutcome::Handled;
        }
        let Some(event) = KeyEvent::from_menu_key(key) else {
            return ConfirmOutcome::Handled;
        };
        match self.focus.screen_key_pressed(&mut self.widgets, event) {
            KeyOutcome::Close => ConfirmOutcome::No,
            KeyOutcome::Consumed | KeyOutcome::FocusMoved => ConfirmOutcome::Handled,
            // `AbstractButton.keyPressed` presses a focused, active button on
            // Enter. With **no** focus — the state this screen opens in — there is
            // nothing to press, which is the point.
            KeyOutcome::Declined if key == MenuKey::Enter => match self.focus.focused() {
                Some(row) => Self::answer(row),
                None => ConfirmOutcome::Handled,
            },
            KeyOutcome::Declined => ConfirmOutcome::Handled,
        }
    }

    /// Which answer row `row` is. Exhaustive rather than `row == YES_ROW`, so a
    /// third button could not silently inherit Yes's meaning.
    fn answer(row: usize) -> ConfirmOutcome {
        match row {
            YES_ROW => ConfirmOutcome::Yes,
            NO_ROW => ConfirmOutcome::No,
            _ => ConfirmOutcome::Handled,
        }
    }
}

/// [`DELETE_WARNING`] with `name` interpolated, the name clipped so the whole
/// sentence measures inside [`block_width`].
///
/// The clip is on the **name**, not on the finished sentence, so the
/// `will be lost forever!` half — the part that says the action is irreversible —
/// can never be the thing that gets cut. A clipped name ends in `...`, matching
/// [`super::render`]'s own `clip_measured`.
#[must_use]
fn warning_for(name: &str) -> String {
    let budget = block_width();
    let full = DELETE_WARNING.replace("%s", name);
    if super::render::text_px(&full, 1.0) <= budget {
        return full;
    }
    let mut kept: Vec<char> = name.chars().collect();
    while !kept.is_empty() {
        kept.pop();
        let candidate: String = kept.iter().collect();
        let line = DELETE_WARNING.replace("%s", &format!("{candidate}..."));
        if super::render::text_px(&line, 1.0) <= budget {
            return line;
        }
    }
    // Even the empty name does not fit, which needs a narrower sentence rather
    // than a narrower name. Return the fixed half alone rather than something
    // that overhangs: the player still learns the action is irreversible.
    DELETE_WARNING.replace("'%s' ", "")
}

/// Builds the whole confirmation frame: two `widget/button*` rows at the
/// arranged block's own rects, and the question and warning as centred labels.
///
/// `selected` is `usize::MAX` when nothing is focused — **which is the state this
/// screen opens in** — rather than `0`, which would light the affirmative button
/// up and, worse, would make [`super::render::draw_widget`]'s
/// `isHoveredOrFocused()` draw it as the one the keyboard is on.
#[must_use]
pub fn frame(nav: &ConfirmNav) -> MenuFrame<'static> {
    let rows: Vec<MenuRow> = (0..ROW_COUNT)
        .map(|row| MenuRow {
            label: nav.widgets.buttons[row].message.clone(),
            enabled: nav.widgets.buttons[row].active,
            slot: Some(row_slot(row)),
            ..Default::default()
        })
        .collect();
    let line = |text: String, placement: ConfirmPlacement| MenuLabel {
        text,
        origin: Origin::Confirm(placement),
        dx: 0.0,
        dy: 0.0,
        // `Align::Centre` because the cell is zero-width and therefore *is* the
        // text's centre — the same argument `world_select_title_label` makes.
        align: Align::Centre,
        colour: super::widget::ACTIVE_LABEL,
        scale: 1.0,
    };
    MenuFrame {
        rows,
        selected: nav.focused_row().unwrap_or(usize::MAX),
        hovered: nav.hovered(),
        vanilla: true,
        labels: vec![
            line(nav.title().to_string(), ConfirmPlacement::Title),
            line(nav.message().to_string(), ConfirmPlacement::Message),
        ],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference canvas every rect assertion below is taken at.
    const V: (f32, f32) = (854.0, 480.0);

    /// The block is vanilla's own tree, and the arithmetic is hand-derived from
    /// the Java rather than read back out of the layout.
    ///
    /// `LinearLayout.vertical().spacing(8)` over a 9 px title, a 9 px message and
    /// a button row of two 150 px buttons 4 px apart with `paddingTop(16)`:
    ///
    /// - width  = `2 * 150 + 4` = **304** (the title and message cells are
    ///   zero-width, so the button row is what sets it);
    /// - height = `9 + 8 + 9 + 8 + (16 + 20)` = **70**;
    /// - centred in 854×480 → `(int)((854 - 304) / 2)` = 275,
    ///   `(int)((480 - 70) / 2)` = 205;
    /// - the buttons' own y = `205 + 9 + 8 + 9 + 8 + 16` = **255**.
    #[test]
    fn the_confirm_block_is_vanillas_own_arrangement() {
        let rects = confirm_rects(V.0, V.1);
        assert_eq!(rects.len(), 4, "title, message and two buttons");
        assert_eq!(block_width(), 304.0, "2 * 150 + 4");

        let (tx, ty, tw, th) = rects[0];
        assert_eq!((tw, th), (0.0, 9.0), "the title cell is zero-width");
        assert_eq!(ty, 205.0, "the block's own top");
        // A zero-width cell centred in a 304 px column sits on its centre.
        assert_eq!(tx, 275.0 + 152.0);

        let (_, my, ..) = rects[1];
        assert_eq!(my, 205.0 + 9.0 + 8.0, "one spacing below the title");

        let yes = rects[2];
        let no = rects[3];
        assert_eq!(yes, (275.0, 255.0, 150.0, 20.0));
        assert_eq!(no, (275.0 + 150.0 + 4.0, 255.0, 150.0, 20.0));
        // And the slots the draw and the hit-test read resolve to the same rects.
        assert_eq!(row_slot(YES_ROW).resolve(V.0, V.1), yes);
        assert_eq!(row_slot(NO_ROW).resolve(V.0, V.1), no);
    }

    /// **The double-click property, as a rect assertion.**
    ///
    /// The world list's Delete button and this screen's affirmative button must
    /// not overlap, or a second click at the place the player just clicked would
    /// press it. Both rects are resolved through the *same* slot machinery the
    /// draw and `app.rs`'s hit-test use — a restated constant could be right
    /// while the drawn rect was not.
    ///
    /// The control is the third assertion: the two rects are resolved at the same
    /// canvas and are both on screen, so "they do not overlap" is not satisfied by
    /// one of them being the `(-1000, -1000)` sentinel.
    #[test]
    fn the_confirmation_cannot_be_fired_by_a_second_click_where_delete_was() {
        use super::super::world_select::WorldSelectButton;
        let (dx, dy, dw, dh) = super::super::render::world_select_slot(WorldSelectButton::Delete)
            .resolve(V.0, V.1);
        let (yx, yy, yw, yh) = row_slot(YES_ROW).resolve(V.0, V.1);

        let overlaps = dx < yx + yw && yx < dx + dw && dy < yy + yh && yy < dy + dh;
        assert!(
            !overlaps,
            "the Delete button {:?} overlaps the confirmation's affirmative button \
             {:?}, so a double-click on Delete would confirm the deletion",
            (dx, dy, dw, dh),
            (yx, yy, yw, yh)
        );
        // The vertical gap, predicted rather than merely asserted non-zero: the
        // footer's Delete sits at y 452 and the centred block's buttons at 255, so
        // the gap is 452 - 275 = 177 px.
        assert_eq!(dy - (yy + yh), 177.0, "Delete is 177 px below the Yes button");

        // -- control ---------------------------------------------------------
        // Both rects must be real and on screen, or "no overlap" is vacuous.
        for (what, r) in [("Delete", (dx, dy, dw, dh)), ("Yes", (yx, yy, yw, yh))] {
            assert!(
                r.0 >= 0.0 && r.1 >= 0.0 && r.0 + r.2 <= V.0 && r.1 + r.3 <= V.1,
                "{what} resolved off-canvas at {r:?}, so the no-overlap assertion \
                 measures nothing"
            );
        }
        // And the detector itself fires: a rect compared with itself overlaps.
        let self_overlap = dx < dx + dw && dx < dx + dw && dy < dy + dh && dy < dy + dh;
        assert!(
            self_overlap,
            "the overlap test cannot detect an overlap, so the assertion above \
             proves nothing"
        );
    }

    /// Nothing is focused on open, so Enter presses nothing.
    ///
    /// The control is the second half: after one Tab, Enter *does* answer — so
    /// the assertion is about the initial focus state and not about a screen
    /// whose Enter is dead.
    #[test]
    fn enter_immediately_after_opening_the_confirmation_does_nothing() {
        let mut nav = ConfirmNav::delete_world("alpha", "Alpha World");
        assert_eq!(nav.focused_row(), None, "vanilla sets no initial focus here");
        assert_eq!(nav.handle_key(MenuKey::Enter), ConfirmOutcome::Handled);
        assert_eq!(nav.handle_key(MenuKey::Enter), ConfirmOutcome::Handled);

        // -- control ---------------------------------------------------------
        nav.handle_key(MenuKey::Tab);
        assert_eq!(nav.focused_row(), Some(YES_ROW), "Tab reaches the first button");
        assert_eq!(
            nav.handle_key(MenuKey::Enter),
            ConfirmOutcome::Yes,
            "Enter on a focused affirmative button must answer, or the assertion \
             above passes for a screen whose Enter never works"
        );
    }

    /// Escape and Cancel are both the negative answer; only the affirmative
    /// control is affirmative.
    #[test]
    fn only_the_affirmative_control_answers_yes() {
        let mut nav = ConfirmNav::delete_world("alpha", "Alpha World");
        assert_eq!(nav.handle_key(MenuKey::Escape), ConfirmOutcome::No);

        let mut nav = ConfirmNav::delete_world("alpha", "Alpha World");
        assert_eq!(nav.click_row(NO_ROW), ConfirmOutcome::No);

        let mut nav = ConfirmNav::delete_world("alpha", "Alpha World");
        assert_eq!(nav.click_row(YES_ROW), ConfirmOutcome::Yes);

        // A row this screen does not have does nothing at all.
        let mut nav = ConfirmNav::delete_world("alpha", "Alpha World");
        assert_eq!(nav.click_row(ROW_COUNT), ConfirmOutcome::Handled);
        assert_eq!(nav.focused_row(), None, "and does not take focus");
    }

    /// Hover is not focus here either — and on this screen the consequence is
    /// sharper than on the world list: a hover that moved focus onto the
    /// affirmative button would arm Enter.
    #[test]
    fn hovering_the_affirmative_button_does_not_arm_enter() {
        let mut nav = ConfirmNav::delete_world("alpha", "Alpha World");
        nav.hover(YES_ROW);
        assert_eq!(nav.hovered(), Some(YES_ROW));
        assert_eq!(nav.focused_row(), None, "hover must not focus");
        assert_eq!(
            nav.handle_key(MenuKey::Enter),
            ConfirmOutcome::Handled,
            "Enter after a hover over Yes must not delete"
        );
        // A row that does not exist is ignored rather than recorded.
        nav.hover(ROW_COUNT + 5);
        assert_eq!(nav.hovered(), Some(YES_ROW));
    }

    /// The message names the world, and fits the block it is drawn in.
    ///
    /// Both halves matter: a message that did not name the world would let a
    /// player confirm the deletion of a different one, and a message wider than
    /// the block would overhang it (there is no clip on a `MenuLabel`).
    #[test]
    fn the_confirmation_message_fits_its_own_block() {
        let budget = block_width();
        let nav = ConfirmNav::delete_world("alpha", "Alpha World");
        assert!(
            nav.message().contains("Alpha World"),
            "the warning must name the world: {:?}",
            nav.message()
        );
        assert!(
            nav.message().contains("lost forever"),
            "and must say the action is irreversible: {:?}",
            nav.message()
        );
        assert!(
            super::super::render::text_px(nav.message(), 1.0) <= budget,
            "{:?} measures {} px in a {budget} px block",
            nav.message(),
            super::super::render::text_px(nav.message(), 1.0)
        );

        // A 255-character name — `saves::MAX_FILE_NAME`'s own ceiling, so this is
        // the longest name that can reach here — is clipped rather than allowed to
        // overhang, and the irreversibility half survives the clip.
        let long: String = std::iter::repeat_n('W', 255).collect();
        let nav = ConfirmNav::delete_world("w", &long);
        assert!(
            super::super::render::text_px(nav.message(), 1.0) <= budget,
            "a 255-character name overhung the block: {:?}",
            nav.message()
        );
        assert!(
            nav.message().contains("lost forever"),
            "the clip must fall on the name, not on the warning: {:?}",
            nav.message()
        );
        assert!(nav.message().contains("..."), "and say it clipped");

        // -- control ---------------------------------------------------------
        // The un-clipped sentence really is too wide, or the clip above did
        // nothing and this test would pass for a `warning_for` that never clips.
        let unclipped = DELETE_WARNING.replace("%s", &long);
        assert!(
            super::super::render::text_px(&unclipped, 1.0) > budget,
            "the long name is not actually too wide, so nothing was clipped"
        );
    }

    /// The request travels with the screen, so the caller acts on the world the
    /// player was looking at rather than on the current selection.
    #[test]
    fn the_request_carries_the_folder_the_player_confirmed() {
        let nav = ConfirmNav::delete_world("alpha (1)", "Alpha World");
        assert_eq!(
            nav.request(),
            &ConfirmRequest::DeleteWorld {
                dir_name: "alpha (1)".to_string(),
                display_name: "Alpha World".to_string(),
            }
        );
    }
}
