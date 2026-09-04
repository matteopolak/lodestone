//! The sign-editing screen and its four line editors.
//!
//! ## What it is
//!
//! A four-line text editor over a sign's front or back face. Unlike
//! [`super::command_block`], which the server never drives (it opens purely
//! from a client-side right-click), a sign edit is **server-authorised**:
//! the server decides whether the player may edit and sends
//! the sign-editor open packet (`pos`, `is_front_text`), decoded here as
//! [`lodestone_model::event::ClientEvent::SignEditorOpened`]. This module is
//! the screen that packet must reach; wiring the receive-direction hop
//! (`ClientEvent::SignEditorOpened` → `Sim` → [`SignEditOpen`]) is
//! `crate::sim`/`crate::net`'s job, not this one's — this module only owns
//! the screen's own state once it has been asked to open.
//!
//! ## Deliberate simplifications
//!
//! - **No sign-shaped background art.** The current renderer has no loader for
//!   loose GUI PNGs outside the sprite atlas, so the four lines draw as a plain
//!   stacked column of bordered [`super::edit_box::EditBox`] rows. The caret,
//!   selection and glyph positions are still measured by
//!   `super::render::draw::draw_edit_box`, while the missing board is purely a
//!   rendering-resource limitation.
//! - **The per-line pixel-width cap is real, but uses the plain sign's 90 px
//!   budget for every sign, including hanging ones.** The sign line-width rule
//!   accepts a line when its measured width is within the sign's maximum —
//!   `90` for a plain sign, `60` for a hanging one
//!   (`lodestone_render::SignKind::max_text_line_width`). This screen's
//!   [`SignEditOpen`] carries no [`lodestone_render::SignKind`] (the block
//!   state that would resolve it lives behind `crate::sim`/
//!   `crate::block_entities`, both outside this file's ownership), so
//!   [`SignEditState::new`] always applies the wider, plain budget via
//!   [`super::edit_box::EditBox::with_max_pixel_width`] — the editor accepts
//!   up to 1.5× as much input width as the 60 px hanging-sign budget. The
//!   renderer's separate, larger hanging-sign scale constant affects only
//!   visual size, not this editor's input cap. Threading
//!   `SignKind`
//!   through would need `SignEditOpen`/`PendingSignEdit`/`ClientEvent::
//!   SignEditorOpened` handling in `sim/net_apply.rs` to resolve the block
//!   state the way `crate::block_entities::sign_kind_for_state` already does.
//! - **No IME preedit overlay, no bidirectional shaping.** Neither exists
//!   anywhere else in this shell's text widgets either.
//!
//! ## Always sends on close
//!
//! Every close path submits the sign update action. Unlike
//! [`super::command_block::CommandBlockState`], which distinguishes Done from
//! Cancel/Escape, the sign editor has no cancel action: both Done and Escape
//! reach [`SignEditState::to_action`]. The invariant belongs to the navigation
//! paths in [`crate::menu::nav::MenuNav`], not to this state struct alone.
//!
//! ## Dependencies
//!
//! [`super::edit_box`] for the four line fields; `lodestone_model::{BlockPos,
//! ClientAction}` for the outbound packet shape.

use lodestone_model::{BlockPos, ClientAction};

use super::edit_box::EditBox;
use super::focus::KeyEvent;

/// Every sign face has exactly four lines.
pub const LINE_COUNT: usize = 4;

/// The 2D simplification's line-field width/height — see the module doc's
/// "Deliberate simplifications" section on why there is no pseudo-3D
/// sign face to measure these against instead.
pub const LINE_W: f32 = 300.0;
/// See [`LINE_W`].
pub const LINE_H: f32 = 20.0;
/// Left edge of a line field, as an offset from [`super::render::Origin::ScreenTop`]'s
/// `x` anchor (`width / 2`) — centring a 300 px-wide field the same way
/// [`super::command_block::COMMAND_DX`] centres the command block's field.
pub const LINE_DX: f32 = -150.0;
/// Vertical position of the first line field, as an offset from
/// [`super::render::Origin::ScreenTop`]'s `y` anchor (`0`). Chosen to sit
/// below the title (`y = 40`, see [`TITLE_Y`]) with room for four rows above
/// [`DONE_Y`].
pub const LINE_START_Y: f32 = 60.0;
/// Vertical spacing between consecutive line fields.
pub const LINE_SPACING: f32 = 24.0;
/// The title's y coordinate in the flat screen layout.
pub const TITLE_Y: f32 = 40.0;
/// The localized title shown by the screen.
pub const TITLE_TEXT: &str = "Edit sign message";
/// The Done button's standard width and height.
pub const DONE_W: f32 = 200.0;
/// See [`DONE_W`].
pub const DONE_H: f32 = 20.0;
/// The Done button's left edge, as an offset from
/// [`super::render::Origin::ScreenTop`]'s `x` anchor.
pub const DONE_DX: f32 = -100.0;
/// The Done button's y, as a fixed offset from [`super::render::Origin::ScreenTop`]'s
/// `y` anchor (`0`). The flat layout keeps the footer below the four line
/// fields instead of scaling it with canvas height.
pub const DONE_Y: f32 = LINE_START_Y + (LINE_COUNT as f32) * LINE_SPACING + 10.0;

/// What opening this screen needs: the target sign, which face, and the
/// text already stored there (read off the block entity's already-synced
/// NBT — see [`lodestone_world::SignText`] — by whatever wires
/// `ClientEvent::SignEditorOpened` into a call here; this module does not
/// read the world itself).
#[derive(Debug, Clone, PartialEq)]
pub struct SignEditOpen {
    /// The sign block's world position carried by the open and update actions.
    pub pos: BlockPos,
    /// Whether the front (vs. back) face is being edited.
    pub is_front_text: bool,
    /// The four lines currently stored on that face, in top-to-bottom order.
    pub lines: [String; 4],
}

impl Default for SignEditOpen {
    /// A freshly placed, blank sign.
    fn default() -> Self {
        Self {
            pos: BlockPos::new(0, 0, 0),
            is_front_text: true,
            lines: Default::default(),
        }
    }
}

/// The screen's live state: the target, the four line fields, and which one
/// currently has the (single, screen-wide) keyboard focus.
///
/// Unlike [`super::command_block::CommandBlockState`], which has exactly one
/// focus target, this screen has four. Separate [`EditBox`] values let each
/// line retain its cursor, selection and horizontal scroll position when focus
/// moves; switching lines also places the cursor at the destination line's end.
#[derive(Debug, Clone, PartialEq)]
pub struct SignEditState {
    /// The target sign's position — carried so [`Self::to_action`] can name it
    /// without a second "which sign" parameter threaded through every caller.
    pub pos: BlockPos,
    /// Whether the front (vs. back) face is being edited.
    pub is_front_text: bool,
    /// The four line fields, top to bottom.
    pub lines: [EditBox; LINE_COUNT],
    /// Which line currently has focus.
    active_line: usize,
    /// Whether the mouse is over the Done row, for hover highlighting —
    /// matching [`super::command_block::CommandBlockState::hovered`]'s shape,
    /// just narrower: this screen has exactly one clickable row.
    pub done_hovered: bool,
}

impl SignEditState {
    /// Builds the screen's state from the open action plus the sign's already-synced
    /// block-entity data: see
    /// [`SignEditOpen`].
    #[must_use]
    pub fn new(open: SignEditOpen) -> Self {
        let mut lines: [EditBox; LINE_COUNT] = std::array::from_fn(|_| {
            // Sign lines have no character-count cap; only the pixel-width
            // budget applies. See the module doc for the `SignKind` caveat.
            EditBox::new(0.0, 0.0, LINE_W, LINE_H, "Sign line")
                .with_max_length(super::edit_box::UNBOUNDED_LENGTH)
                .with_max_pixel_width(lodestone_render::SignKind::Plain.max_text_line_width())
        });
        for (line, value) in lines.iter_mut().zip(open.lines.iter()) {
            line.set_value(value);
        }
        lines[0].widget.focused = true;
        Self {
            pos: open.pos,
            is_front_text: open.is_front_text,
            lines,
            active_line: 0,
            done_hovered: false,
        }
    }

    /// Which line currently has focus (`0..4`).
    #[must_use]
    pub fn active_line(&self) -> usize {
        self.active_line
    }

    /// Moves focus to `line & 3`, wrapping modulo four rather than clamping.
    ///
    /// The destination line's cursor is placed at its end.
    pub fn set_active_line(&mut self, line: usize) {
        let line = line & (LINE_COUNT - 1);
        self.active_line = line;
        for (i, field) in self.lines.iter_mut().enumerate() {
            field.widget.focused = i == line;
        }
        self.lines[line].move_cursor_to_end(false);
    }

    /// Selects the previous line, wrapping from zero to the final line.
    pub fn previous_line(&mut self) {
        // Use wrapping subtraction before masking so zero wraps to the final
        // line without a signed intermediate.
        self.set_active_line(self.active_line.wrapping_sub(1));
    }

    /// Selects the next line, wrapping from the final line to zero. Enter also advances
    /// the line here, it does **not** activate Done (see the module doc: only
    /// the screen's own Done row, or Escape, closes it).
    pub fn next_line(&mut self) {
        self.set_active_line(self.active_line + 1);
    }

    /// One printable character into the active line.
    pub fn handle_char(&mut self, ch: char) -> bool {
        self.lines[self.active_line].handle_char(ch)
    }

    /// One non-printable key into the active line — Backspace/Delete,
    /// Left/Right/Home/End caret motion (plain, word-wise under the platform's
    /// edit modifier, extending the selection under Shift), select-all and the
    /// clipboard. See [`EditBox::handle_key`], which implements all of it.
    ///
    /// The caret keys reach here as [`crate::menu::nav::MenuKey::Edit`],
    /// which carries the whole [`KeyEvent`] rather than abstracting it — for
    /// these four the modifiers are the meaning. They act on the **active**
    /// line only, which is what a gate has to check: `active_line` is this
    /// screen's own focus notion and nothing in `EditBox` knows about it.
    pub fn handle_key(&mut self, event: KeyEvent) -> bool {
        self.lines[self.active_line].handle_key(event)
    }

    /// Builds the outbound sign-update action. See the module doc: the caller must
    /// call this on **every** exit, not only a Done click.
    #[must_use]
    pub fn to_action(&self) -> ClientAction {
        self.to_submit().into_action()
    }

    /// [`Self::to_action`]'s `Eq`-able intermediate — see
    /// [`SignEditSubmit`]'s own doc for why `nav::MenuAction` carries this
    /// rather than a [`ClientAction`] directly (the same reason
    /// [`super::command_block::CommandBlockSubmit`] exists).
    #[must_use]
    pub fn to_submit(&self) -> SignEditSubmit {
        SignEditSubmit {
            pos: self.pos,
            is_front_text: self.is_front_text,
            lines: std::array::from_fn(|i| self.lines[i].value().to_string()),
        }
    }
}

/// The `Eq`-able subset of [`SignEditState`] that reaches
/// `super::nav::MenuAction::SignUpdate`. [`ClientAction`] itself cannot derive
/// `Eq` (a sibling variant carries a float), and `MenuAction` derives `Eq` for
/// every one of its other variants — see
/// [`super::command_block::CommandBlockSubmit`]'s own doc, which this mirrors
/// exactly. Every field here — a [`BlockPos`], a `bool` and four `String`s —
/// is already `Eq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignEditSubmit {
    /// `ClientAction::SignUpdate`'s `pos` field.
    pub pos: BlockPos,
    /// `ClientAction::SignUpdate`'s `is_front_text` field.
    pub is_front_text: bool,
    /// `ClientAction::SignUpdate`'s `lines` field.
    pub lines: [String; 4],
}

impl SignEditSubmit {
    /// Rebuilds the [`ClientAction`] `app.rs` actually sends — the one step
    /// [`nav::MenuAction`](super::nav::MenuAction)'s `Eq` derive cannot cross
    /// itself; see this struct's own doc.
    #[must_use]
    pub fn into_action(self) -> ClientAction {
        ClientAction::SignUpdate {
            pos: self.pos,
            is_front_text: self.is_front_text,
            lines: self.lines,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> SignEditOpen {
        SignEditOpen::default()
    }

    #[test]
    fn starts_on_line_zero_focused_and_seeded_with_the_supplied_text() {
        let state = SignEditState::new(SignEditOpen {
            pos: BlockPos::new(1, 2, 3),
            is_front_text: true,
            lines: [
                "one".to_string(),
                "two".to_string(),
                "three".to_string(),
                "four".to_string(),
            ],
        });
        assert_eq!(state.active_line(), 0);
        assert!(state.lines[0].widget.focused);
        for i in 1..LINE_COUNT {
            assert!(!state.lines[i].widget.focused, "line {i} must not start focused");
        }
        assert_eq!(state.lines[0].value(), "one");
        assert_eq!(state.lines[1].value(), "two");
        assert_eq!(state.lines[2].value(), "three");
        assert_eq!(state.lines[3].value(), "four");
    }

    #[test]
    fn next_and_previous_line_wrap_with_javas_bitwise_rule() {
        let mut state = SignEditState::new(open());
        assert_eq!(state.active_line(), 0);
        state.previous_line();
        assert_eq!(state.active_line(), 3, "0 - 1 & 3 == 3, not a clamp to 0");
        state.next_line();
        assert_eq!(state.active_line(), 0, "3 + 1 & 3 == 0, wraps back");
        state.next_line();
        state.next_line();
        state.next_line();
        state.next_line();
        assert_eq!(state.active_line(), 0, "four forward steps is a full lap");
    }

    #[test]
    fn switching_lines_moves_focus_and_parks_the_cursor_at_the_end() {
        let mut state = SignEditState::new(SignEditOpen {
            lines: [
                "hello".to_string(),
                String::new(),
                String::new(),
                String::new(),
            ],
            ..open()
        });
        state.next_line();
        assert_eq!(state.active_line(), 1);
        assert!(state.lines[1].widget.focused);
        assert!(!state.lines[0].widget.focused, "focus must move, not merely add");
        state.previous_line();
        assert_eq!(state.active_line(), 0);
        assert_eq!(
            state.lines[0].cursor_position(),
            5,
            "re-entering a line parks the cursor at its end, matching \
             `signField.setCursorToEnd()`"
        );
    }

    #[test]
    fn typing_only_reaches_the_active_line() {
        let mut state = SignEditState::new(open());
        state.handle_char('A');
        state.next_line();
        state.handle_char('B');
        assert_eq!(state.lines[0].value(), "A");
        assert_eq!(state.lines[1].value(), "B");
        assert_eq!(state.lines[2].value(), "");
        assert_eq!(state.lines[3].value(), "");
    }

    /// The discriminating gate: **pairwise-distinct**
    /// lines, so a transposition of two lines (or of `pos`/`is_front_text`)
    /// cannot survive a round trip unnoticed. This drives the screen — typing
    /// into each [`EditBox`] — not the `ClientAction` constructor directly.
    #[test]
    fn to_action_carries_every_field_the_wire_needs_with_pairwise_distinct_lines() {
        let mut state = SignEditState::new(SignEditOpen {
            pos: BlockPos::new(11, 1, 4),
            is_front_text: false,
            lines: Default::default(),
        });
        for (i, text) in ["alpha", "bravo", "charlie", "delta"].iter().enumerate() {
            state.set_active_line(i);
            for ch in text.chars() {
                state.handle_char(ch);
            }
        }
        assert_eq!(
            state.to_action(),
            ClientAction::SignUpdate {
                pos: BlockPos::new(11, 1, 4),
                is_front_text: false,
                lines: [
                    "alpha".to_string(),
                    "bravo".to_string(),
                    "charlie".to_string(),
                    "delta".to_string(),
                ],
            }
        );
    }

    #[test]
    fn is_front_text_is_carried_through_untouched() {
        let front = SignEditState::new(SignEditOpen {
            is_front_text: true,
            ..open()
        });
        let back = SignEditState::new(SignEditOpen {
            is_front_text: false,
            ..open()
        });
        assert_eq!(
            front.to_action(),
            ClientAction::SignUpdate {
                pos: BlockPos::new(0, 0, 0),
                is_front_text: true,
                lines: Default::default(),
            }
        );
        assert_eq!(
            back.to_action(),
            ClientAction::SignUpdate {
                pos: BlockPos::new(0, 0, 0),
                is_front_text: false,
                lines: Default::default(),
            }
        );
    }

}
