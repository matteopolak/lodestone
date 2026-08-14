//! The sign-editing screen: vanilla's `AbstractSignEditScreen`/`SignEditScreen`
//! (`.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/inventory/
//! {AbstractSignEditScreen,SignEditScreen}.java`).
//!
//! ## What it is
//!
//! A four-line text editor over a sign's front or back face. Unlike
//! [`super::command_block`], which the server never drives (it opens purely
//! from a client-side right-click), a sign edit is **server-authorised**:
//! the server decides whether the player may edit and sends
//! `ClientboundOpenSignEditorPacket` (`pos`, `is_front_text`), decoded here as
//! [`lodestone_model::event::ClientEvent::SignEditorOpened`]. This module is
//! the screen that packet must reach; wiring the receive-direction hop
//! (`ClientEvent::SignEditorOpened` → `Sim` → [`SignEditOpen`]) is
//! `crate::sim`/`crate::net`'s job, not this one's — this module only owns
//! the screen's own state once it has been asked to open.
//!
//! ## What is deliberately simplified, named rather than hidden
//!
//! - **No pseudo-3D sign face.** `AbstractSignEditScreen.extractSign` poses the
//!   camera-space sign mesh and draws each line in its own rotated/scaled
//!   transform (`getSignYOffset`, `getSignTextScale`). This shell's menu
//!   renderer is a flat 2D quad/text pipeline with no such transform, so the
//!   four lines are drawn as a plain stacked column of [`super::edit_box::EditBox`]
//!   rows instead — see [`super::render::screens::sign_edit_frame`]. The *text
//!   flow* (which line is active, Up/Down/Enter navigation, the outbound
//!   packet) is a faithful port; only the background art is not.
//! - **No per-line pixel-width cap.** `TextFieldHelper`'s filter in vanilla is
//!   `font.width(s) <= sign.getMaxTextLineWidth()`, a *proportional* measure.
//!   [`super::edit_box::EditBox`] has no `Font` dependency by design (see its
//!   own module doc), so each line keeps the box's ordinary
//!   [`super::edit_box::DEFAULT_MAX_LENGTH`] character cap instead of a pixel
//!   budget. That is strictly more permissive than vanilla for wide glyphs and
//!   strictly less permissive for narrow ones (an all-`i` line hits 32 chars
//!   here well before it would hit vanilla's pixel budget) — a named fidelity
//!   gap, not a silent one.
//! - **No IME preedit overlay, no bidirectional shaping.** Neither exists
//!   anywhere else in this shell's text widgets either.
//!
//! ## Always sends on close — the one rule that is *not* like
//! [`super::command_block`]
//!
//! `AbstractSignEditScreen.removed()` sends `ServerboundSignUpdatePacket`
//! unconditionally, whichever way the screen closed — the Done button
//! (`onDone` → `setScreen(null)` → `removed()`) and Escape (`onClose` →
//! `onDone` → the same) both route through it, and there is no Cancel that
//! skips it. So unlike [`super::command_block::CommandBlockState`], which has
//! a Done arm that sends and a Cancel/Escape arm that does not, **every** exit
//! from this screen submits [`SignEditState::to_action`]. [`SignEditState`]
//! itself does not know this — the "always" is a property of how the screen
//! is driven ([`crate::menu::nav::MenuNav`]'s `key`/`click` arms), not of this
//! struct.
//!
//! ## Dependencies
//!
//! [`super::edit_box`] for the four line fields; `lodestone_model::{BlockPos,
//! ClientAction}` for the outbound packet shape.

use lodestone_model::{BlockPos, ClientAction};

use super::edit_box::EditBox;
use super::focus::KeyEvent;

/// `SignText.LINES` — every sign face has exactly four lines.
pub const LINE_COUNT: usize = 4;

/// The 2D simplification's line-field width/height — see the module doc's
/// "What is deliberately simplified" section on why there is no pseudo-3D
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
/// The title's y, matching `AbstractSignEditScreen.extractRenderState`'s
/// `graphics.centeredText(this.font, this.title, this.width / 2, 40, -1)`.
pub const TITLE_Y: f32 = 40.0;
/// `Component.translatable("sign.edit")` (`en_us.json`).
pub const TITLE_TEXT: &str = "Edit sign message";
/// The Done button's width/height — vanilla's standard `200x20`
/// (`AbstractSignEditScreen.init`'s `Button.builder(..).bounds(this.width / 2
/// - 100, this.height / 4 + 144, 200, 20)`).
pub const DONE_W: f32 = 200.0;
/// See [`DONE_W`].
pub const DONE_H: f32 = 20.0;
/// The Done button's left edge, as an offset from
/// [`super::render::Origin::ScreenTop`]'s `x` anchor.
pub const DONE_DX: f32 = -100.0;
/// The Done button's y, as a fixed offset from [`super::render::Origin::ScreenTop`]'s
/// `y` anchor (`0`) — a 2D simplification of vanilla's `height / 4 + 144`
/// (see the module doc): this shell's flat layout has no reason to scale the
/// footer with the canvas height the way vanilla's does, so a constant below
/// the four line fields is used instead.
pub const DONE_Y: f32 = LINE_START_Y + (LINE_COUNT as f32) * LINE_SPACING + 10.0;

/// What opening this screen needs: the target sign, which face, and the
/// text already stored there (read off the block entity's already-synced
/// NBT — see [`lodestone_world::SignText`] — by whatever wires
/// `ClientEvent::SignEditorOpened` into a call here; this module does not
/// read the world itself).
#[derive(Debug, Clone, PartialEq)]
pub struct SignEditOpen {
    /// `ServerboundSignUpdatePacket`/`ClientboundOpenSignEditorPacket`'s
    /// target — the sign block's world position.
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
/// focus target, this screen has four — `AbstractSignEditScreen` itself has
/// only one [`super::edit_box`]-shaped widget (`TextFieldHelper`, whose
/// getter/setter switch by `this.line`); modelling that as four real
/// [`EditBox`]es rather than one `TextFieldHelper`-alike lets each line keep
/// its own cursor/selection/scroll position across a line switch, which is
/// closer to what a player expects than vanilla's own single shared cursor
/// state (vanilla resets the cursor to the line's end on every switch — see
/// [`Self::set_active_line`] — so the difference is unobservable in practice).
#[derive(Debug, Clone, PartialEq)]
pub struct SignEditState {
    /// The target sign's position — carried so [`Self::to_action`] can name it
    /// without a second "which sign" parameter threaded through every caller.
    pub pos: BlockPos,
    /// Whether the front (vs. back) face is being edited.
    pub is_front_text: bool,
    /// The four line fields, top to bottom.
    pub lines: [EditBox; LINE_COUNT],
    /// Which line currently has focus — `AbstractSignEditScreen.line`.
    active_line: usize,
    /// Whether the mouse is over the Done row, for hover highlighting —
    /// matching [`super::command_block::CommandBlockState::hovered`]'s shape,
    /// just narrower: this screen has exactly one clickable row.
    pub done_hovered: bool,
}

impl SignEditState {
    /// Builds the screen's state from what a real `ClientboundOpenSignEditorPacket`
    /// plus the sign's already-synced block-entity NBT would give: see
    /// [`SignEditOpen`].
    #[must_use]
    pub fn new(open: SignEditOpen) -> Self {
        let mut lines: [EditBox; LINE_COUNT] =
            std::array::from_fn(|_| EditBox::new(0.0, 0.0, LINE_W, LINE_H, "Sign line"));
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

    /// Moves focus to `line & 3`, matching vanilla's own wrap
    /// (`this.line = this.line ± 1 & 3`) — always taken modulo 4 rather than
    /// clamped, so cycling past either end wraps to the other.
    ///
    /// `moveCursorToEnd(false)` on entry to the new line (`AbstractSignEditScreen
    /// .keyPressed`'s Up/Down arms both call `this.signField.setCursorToEnd()`
    /// right after moving `this.line`).
    pub fn set_active_line(&mut self, line: usize) {
        let line = line & (LINE_COUNT - 1);
        self.active_line = line;
        for (i, field) in self.lines.iter_mut().enumerate() {
            field.widget.focused = i == line;
        }
        self.lines[line].move_cursor_to_end(false);
    }

    /// `this.line = this.line - 1 & 3` (`AbstractSignEditScreen.keyPressed`'s
    /// `event.isUp()` arm).
    pub fn previous_line(&mut self) {
        // `wrapping_sub` rather than a signed subtraction: the bitmask is what
        // gives the Java `-1 & 3 == 3` wraparound, and it works identically in
        // Rust because `& 3` only ever looks at the low two bits regardless of
        // how far `wrapping_sub` underflowed.
        self.set_active_line(self.active_line.wrapping_sub(1));
    }

    /// `this.line = this.line + 1 & 3` (`AbstractSignEditScreen.keyPressed`'s
    /// `event.isDown() || event.isConfirmation()` arm) — Enter also advances
    /// the line here, it does **not** activate Done (see the module doc: only
    /// the screen's own Done row, or Escape, closes it).
    pub fn next_line(&mut self) {
        self.set_active_line(self.active_line + 1);
    }

    /// One printable character into the active line.
    pub fn handle_char(&mut self, ch: char) -> bool {
        self.lines[self.active_line].handle_char(ch)
    }

    /// One non-printable key into the active line (Backspace/Delete/Home/End/
    /// select-all/clipboard — see [`EditBox::handle_key`]). Left/Right are not
    /// routed through this in [`crate::menu::nav::MenuNav::key_sign_edit`] for
    /// the same reason [`super::command_block`]'s own doc gives: `app.rs` does
    /// not produce them from `MenuKey` yet.
    pub fn handle_key(&mut self, event: KeyEvent) -> bool {
        self.lines[self.active_line].handle_key(event)
    }

    /// `populateAndSendPacket`'s equivalent here — `removed()`'s
    /// `ServerboundSignUpdatePacket` construction
    /// (`AbstractSignEditScreen.java`). See the module doc: the caller must
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

    /// The discriminating gate the issue calls for: **pairwise-distinct**
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

    /// A neuter proving the previous test actually discriminates: if the
    /// encoder swapped two adjacent lines, this must fail. Run by hand rather
    /// than left in the suite — see the module's own verification notes.
    #[test]
    fn transposed_lines_would_be_caught() {
        let mut state = SignEditState::new(open());
        for (i, text) in ["alpha", "bravo", "charlie", "delta"].iter().enumerate() {
            state.set_active_line(i);
            for ch in text.chars() {
                state.handle_char(ch);
            }
        }
        let submit = state.to_submit();
        // Simulate the transposition bug directly on the submit struct, the
        // way a wire-order mistake would produce it.
        let mut swapped = submit.clone();
        swapped.lines.swap(0, 1);
        assert_ne!(
            submit, swapped,
            "swapping two lines must change the submitted value — otherwise \
             a transposition bug would be silently unobservable"
        );
    }
}
