//! Vanilla's `EditBox` (`client/gui/components/EditBox.java`) — a single-line
//! text field with a caret, a selection, horizontal scrolling and a length cap.
//!
//! ## What it is
//!
//! The first widget in this shell that genuinely needs #395's focus layer: it
//! only accepts input when [`super::focus::FocusSet`] says it is focused, and
//! the *reason* Left/Right move its caret instead of moving focus is the
//! `Screen.keyPressed` ordering [`super::focus`] ports, not a rule written here.
//!
//! Its consumer is [`super::Screen::ServerEdit`]'s name and address fields, via
//! [`super::nav::EditForm`] — converted, not added alongside. #396's world-select
//! search box is next.
//!
//! ## What vanilla does *not* have, which the written record got wrong
//!
//! - **There is no `setFilter`.** Input restriction is a fixed built-in:
//!   `insertText` runs `StringUtil.filterText` and `charTyped` gates on
//!   `CharacterEvent.isAllowedChatCharacter`, both of which are
//!   `StringUtil.isAllowedChatCharacter` — `ch != 167 && ch >= 32 && ch != 127`
//!   (`StringUtil.java:62-64`). A per-field predicate does not exist and cannot
//!   be added without editing the class. `addFormatter` **is** real but is
//!   display-only (`EditBox.java:476-485`, a `FormattedCharSequence`), so it
//!   cannot reject a character either. [`is_allowed_chat_character`] is the whole
//!   of the rule.
//!
//!   Note what that predicate *allows*: U+0080–U+009F, the C1 controls. Rust's
//!   `char::is_control` rejects them, so the pre-existing `EditForm::push` was
//!   marginally stricter than vanilla. This port follows the jar.
//!
//! - **There is no disabled sprite, but there *is* a `WidgetSprites`.**
//!   `EditBox.java:30-32` uses the **two**-argument constructor
//!   ([`super::widget::WidgetSprites::focusable`]), which collapses `disabled`
//!   onto `enabled`. "No `WidgetSprites` for `Checkbox`/`EditBox`/slider" is true
//!   of `Checkbox` and `AbstractSliderButton` and false of this one.
//!
//! - **Both `get` arguments differ from `AbstractButton`'s.**
//!   `EditBox.java:407` is `SPRITES.get(this.isActive(), this.isFocused())`
//!   where `AbstractButton.java:43-53` is
//!   `SPRITES.get(this.active, this.isHoveredOrFocused())`. So: `isActive()`
//!   (i.e. `visible && active`) not the raw field, and **`isFocused()` alone** —
//!   hovering a text field does *not* draw its highlighted sprite. #393's
//!   correction ("join them with `||`") is right about the button and wrong about
//!   this widget; [`EditBox::background_sprite`] is deliberately not
//!   [`super::widget::Widget::background_sprite`].
//!
//! - **The grey text colour is keyed on `isEditable`, not on `active`.**
//!   `EditBox.java:411` is `this.isEditable ? this.textColor :
//!   this.textColorUneditable`, and `isEditable` is a *separate* flag from
//!   `AbstractWidget.active`. A field can be active (clickable, focusable) and
//!   uneditable, which is how vanilla shows read-only text you can still select
//!   and copy. [`EditBox::text_colour`] keys on the right one.
//!
//! ## Indices are `char`s, not UTF-16 code units
//!
//! Every position in vanilla — `cursorPos`, `highlightPos`, `displayPos`,
//! `maxLength` — is a Java `String` index, i.e. a **UTF-16 code unit**. Ours are
//! `char` (Unicode scalar value) indices. Three consequences, all deliberate:
//!
//! 1. `Util.offsetByCodepoints` exists in vanilla only to step *over* a
//!    surrogate pair, so [`EditBox::move_cursor`] is a plain `±1` here.
//! 2. `insertText`'s `Character.isHighSurrogate(text.charAt(len - 1))` guard
//!    (`EditBox.java:139-141`), which backs the truncation point off by one to
//!    avoid splitting a pair, has nothing to split and is omitted.
//! 3. `maxLength` counts astral characters as **one**, not two. An emoji costs
//!    one of a 32-character budget here and two in vanilla. Nothing in this
//!    shell sends an `EditBox` value to a server as a length-capped protocol
//!    field, so the difference is unobservable — but it *would* matter for a
//!    sign-editing or book-editing screen, which is why it is written down.
//!
//! ## Measurement is a fixed advance, and that is a real deviation
//!
//! `displayPos` scrolling, the visible substring and the caret's x all depend on
//! `Font.plainSubstrByWidth`/`Font.width` — a **proportional** measure. This
//! widget has no `Font`: it is pure data, and threading a font through
//! [`super::focus::FocusTarget::key_pressed`] would put the renderer inside the
//! input layer.
//!
//! So it carries an [`EditBox::advance`] instead: one width for every character,
//! defaulting to [`MENU_TEXT_ADVANCE`]. `super::render` already makes exactly
//! this approximation for every other menu string (`render::text_px` and
//! `render::clip` are fixed-advance; only `clip_measured` consults the real
//! font), so this is not a new class of error — it means a long value can differ
//! from vanilla by a glyph at the right edge.
//!
//! **That fixed advance is now a *default*, not the only path.** A 2026-08-04
//! player report ("the gap between the end of the word and start of cursor
//! grows as I type") turned out to be exactly the deviation this section
//! already warned about, arriving in production: `render.rs`'s `draw_edit_box`
//! attaches a real [`super::hud::vanilla_font::VanillaFont`] whenever the jar
//! is present and draws `state.before`/`state.after` with **its** proportional
//! advances (`Quads::text` → `VanillaFont::draw`), while [`EditBox::draw_state`]
//! computed `cursor_x`/`highlight_x`/`before_x` from this box's fixed 6 px
//! [`MENU_TEXT_ADVANCE`] — two different per-glyph widths for the same string,
//! one used to draw, the other to place the caret after it. Vanilla's own
//! glyphs are mostly narrower than 6 px (`i` is 2, most lowercase letters are
//! 5), so every typed character added roughly `6 - real_advance` px of extra
//! gap, and it accumulated — the "grows as I type" symptom, precisely.
//!
//! The fix is the seam this paragraph used to say to add "if it ever
//! matters": [`EditBox::measure_with`], [`EditBox::text_x_with`] and
//! [`EditBox::draw_state_with`] take an `Option<&dyn Fn(&str) -> f32>` and
//! consult it instead of the fixed advance when given, falling back to the
//! exact old behaviour (`measure`/`text_x`/`draw_state`, unchanged, still
//! called by every existing test and by the command-suggestions popup in
//! `render.rs`) when `None`. `render.rs`'s `draw_edit_box` is the one caller
//! that now passes `Some(|s| b.text_width(s, EDIT_TEXT_SCALE))` —
//! `Quads::text_width`, the *exact* function `Quads::text` uses to pick a
//! font, so measurement and drawing can never disagree about which font is
//! in play. This box still carries no hard `Font` dependency: the closure is
//! threaded through a call, never stored on the struct, so `EditBox` stays
//! pure data and every hermetic test above still exercises the fixed-advance
//! path with no jar required.
//!
//! Note for whoever wires per-character bold/italic (`§`-formatting) into an
//! `EditBox` value: a plain `Fn(&str) -> f32` cannot see run-local style
//! state, so if a value ever carries legacy colour codes, the closure needs
//! to be built from something bold-aware (`VanillaFont::legacy_width`, not
//! `VanillaFont::width`) — this widget's `value` today is always plain
//! user-typed text (addresses, names, console commands), so that case does
//! not yet exist, but the moment it does, `width` silently under-measures a
//! bold run by `BOLD_OFFSET` per bold character.
//!
//! The advance must match the *scale the text is drawn at* (see
//! [`MENU_TEXT_ADVANCE`]) — which, for this widget specifically, **is**
//! vanilla's own scale-1 advance, unlike every other menu string in this
//! shell. `render.rs`'s `draw_edit_box` draws at `EDIT_TEXT_SCALE = 1.0`, not
//! the `TEXT_SCALE = 2.0` every other row uses, because an `EditBox` sits
//! beside vanilla-positioned buttons that already draw at scale 1 and a
//! mismatched scale there is what the input-box-text-too-big report turned
//! out to be.
//!
//! ## How to change it
//!
//! - **`textX`/`textY` are methods here, not cached fields.** Vanilla caches
//!   them and calls `updateTextPosition()` from `setX`, `setY`, `setValue`,
//!   `setBordered`, `setCentered` and `onValueChange` (`EditBox.java:487-493`) —
//!   six places, and a seventh that forgets is a field drawing at a stale
//!   offset. Computing them on demand deletes that bug class outright.
//! - **The caret does not blink.** `EditBox` blinks on
//!   `Util.getMillis() - focusedTime` with a 300 ms interval
//!   (`TextCursorUtils.java:8,20-22`), and no `super::render::MenuFrame` carries
//!   a clock. [`is_cursor_visible`] is the pure predicate, ready for the day one
//!   does; [`EditBox::show_cursor`] takes `None` to mean "always on", which is
//!   what the shell passes and what the pre-existing form caret already did.
//! - **Clipboard shortcuts are real, ported line-for-line from
//!   `EditBox.keyPressed`'s `default:` group.** `isCopy`/`isCut`/`isPaste` all
//!   return `true` in vanilla *and* touch
//!   `Minecraft.keyboardHandler.setClipboard`/`getClipboard`; [`clipboard_seam`]
//!   is that boundary here — the real OS clipboard in production, an in-memory
//!   `thread_local` under every `#[cfg(test)]` build so no test run ever reads
//!   or writes the developer's actual clipboard. This used to be a deliberate
//!   decline (`isCopy`/`isCut`/`isPaste` all returned `false`, on the reasoning
//!   that a cut into a clipboard nobody could write was a data-loss trap worse
//!   than doing nothing); that was right while the seam did not exist, and
//!   wrong to leave that way once it does — see the module's own issue history
//!   for the paste-inserts-`v` report this replaced.
//! - **A `false` from [`EditBox::handle_key`] is load-bearing.** It is what
//!   lets Up/Down out to `Screen`'s focus navigation
//!   (`EditBox.java:279-284` lists 260/264/265/266/267 in the `default:` group),
//!   and it is the entire reason this widget composes with a screen that
//!   navigates by arrow key. Consuming a key "to be safe" breaks Tab traversal
//!   in a way no unit test of this file can see.
//!
//! ## Dependencies
//!
//! [`super::widget`] for the [`super::widget::Widget`] it wraps, its
//! `WidgetSprites` record and `argb_to_rgba`; [`super::focus`] for
//! [`super::focus::FocusTarget`] and the GLFW key codes.

use super::focus::{
    ComponentPath, KeyEvent, ScreenRectangle, KEY_BACKSPACE, KEY_DELETE, KEY_END, KEY_HOME,
    KEY_LEFT, KEY_RIGHT,
};
use super::widget::{LayoutElement, Widget, WidgetSprites, argb_to_rgba};

/// The clipboard `handle_key`'s copy/cut/paste arms read and write —
/// `Minecraft.getInstance().keyboardHandler.getClipboard()`/`setClipboard`.
///
/// Forked at compile time exactly the way `accounts::open_in_browser` forks
/// its own OS side effect (`CLAUDE.md`'s "test that performs an OS-level side
/// effect" rule): production reads and writes the real OS clipboard through
/// [`crate::platform::clipboard`]; **every** `#[cfg(test)]` build — which
/// includes every other module's unit tests in this crate, not only this
/// file's — routes through an in-memory stand-in instead, so no `cargo test`
/// run ever touches, or depends on, whatever happens to be on the developer's
/// real clipboard. `EditBox` itself stays free of any `cfg` — only this
/// module forks.
///
/// This is deliberately *not* threaded through [`EditBox::handle_key`] as a
/// closure parameter the way [`EditBox::measure_with`]'s font is: `handle_key`
/// is also [`super::focus::FocusTarget::key_pressed`]'s body, and that trait
/// method's signature is shared with every other focusable widget, so adding
/// a parameter here would ripple into files this change does not own.
#[cfg(not(test))]
mod clipboard_seam {
    pub fn get() -> String {
        crate::platform::clipboard::get()
    }

    pub fn set(text: &str) {
        crate::platform::clipboard::set(text);
    }
}

/// The `#[cfg(test)]` half of [`clipboard_seam`] — a `thread_local` string, not
/// the OS clipboard. Exposed as `pub(crate)` (test-only, since the module only
/// exists under `cfg(test)`) so a test elsewhere in the crate can seed or
/// inspect it without cloning the OS-clipboard test hazard into every screen
/// that owns an [`EditBox`].
#[cfg(test)]
pub(crate) mod clipboard_seam {
    use std::cell::RefCell;

    thread_local! {
        static FAKE: RefCell<String> = const { RefCell::new(String::new()) };
    }

    pub fn get() -> String {
        FAKE.with(|c| c.borrow().clone())
    }

    pub fn set(text: &str) {
        FAKE.with(|c| *c.borrow_mut() = text.to_owned());
    }
}

/// `EditBox.SPRITES` (`EditBox.java:30-32`) — the **two**-argument
/// `WidgetSprites` collapse, so `disabled` is `enabled` and there is no disabled
/// art.
pub const SPRITES: WidgetSprites =
    WidgetSprites::focusable("widget/text_field", "widget/text_field_highlighted");

/// `EditBox.DEFAULT_TEXT_COLOR` (`EditBox.java:35`), as the signed ARGB integer
/// the jar writes.
pub const DEFAULT_TEXT_COLOR_ARGB: i32 = -2_039_584;

/// `EditBox.textColorUneditable`'s initialiser (`EditBox.java:51`). Keyed on
/// `isEditable`, **not** on `active` — see the module docs.
pub const TEXT_COLOR_UNEDITABLE_ARGB: i32 = -9_408_400;

/// The suggestion text's colour, spelled inline in the jar
/// (`EditBox.java:443`: `graphics.text(.., -8355712, ..)`).
pub const SUGGESTION_COLOR_ARGB: i32 = -8_355_712;

/// `EditBox.maxLength`'s initialiser (`EditBox.java:40`).
pub const DEFAULT_MAX_LENGTH: usize = 32;

/// `EditBox(Font, Component)`'s default size (`EditBox.java:61-63`): 150×20 —
/// which is `Button.DEFAULT_WIDTH` × `Button.DEFAULT_HEIGHT`.
pub const DEFAULT_WIDTH: f32 = 150.0;
/// See [`DEFAULT_WIDTH`].
pub const DEFAULT_HEIGHT: f32 = 20.0;

/// The horizontal inset a bordered box's text starts at (`EditBox.java:490`:
/// `this.bordered ? 4 : 0`), and half of what [`EditBox::inner_width`] subtracts.
pub const BORDER_INSET: f32 = 4.0;

/// `EditBox`'s hardcoded line height: `9` in `extractWidgetRenderState`'s
/// highlight rect and `9 + 1` in the insert cursor's
/// (`EditBox.java:452,459`).
pub const LINE_HEIGHT: f32 = 9.0;

/// `TextCursorUtils.CURSOR_BLINK_INTERVAL_MS`.
pub const CURSOR_BLINK_INTERVAL_MS: u64 = 300;

/// One character's width, in the units this shell's menu text is actually drawn
/// in.
///
/// **Matches vanilla's own advance, and that is deliberate.** `super::render`
/// draws every other menu string at `TEXT_SCALE = 2.0`, but an `EditBox`'s
/// *own* text draws at `EDIT_TEXT_SCALE = 1.0` — the same ratio
/// `draw_widget`'s vanilla-positioned siblings use, and the fix for the
/// input-box-text-too-big report (`render.rs`'s `2cd7c58`: a `14` px glyph in
/// vanilla's own `20` px box read as roughly double both vanilla's ratio and
/// this box's own neighbouring buttons'). At scale 1 the bitmap font's advance
/// is `(GLYPH_W + 1) * 1.0 = 6`, so this constant moved from `12.0` with it.
/// The number that matters to [`EditBox`] is still the one the *draw* uses,
/// because `displayPos` exists to keep the caret inside the drawn rect — a box
/// measuring at one value while drawing at another would scroll at the wrong
/// point.
pub const MENU_TEXT_ADVANCE: f32 = 6.0;

/// `StringUtil.isAllowedChatCharacter` (`StringUtil.java:62-64`): the *only*
/// input filter `EditBox` has.
///
/// `ch != 167 && ch >= 32 && ch != 127` — so the section sign (the legacy
/// formatting-code introducer), every C0 control, and DEL are refused, and
/// everything else including the C1 range is allowed.
#[must_use]
pub fn is_allowed_chat_character(ch: char) -> bool {
    let c = ch as u32;
    c != 167 && c >= 32 && c != 127
}

/// `StringUtil.filterText(input)` (`StringUtil.java:74-86`), single-line: drop
/// every character [`is_allowed_chat_character`] refuses.
#[must_use]
pub fn filter_text(input: &str) -> String {
    input.chars().filter(|&c| is_allowed_chat_character(c)).collect()
}

/// `TextCursorUtils.isCursorVisible(timeInMs)`: on for 300 ms, off for 300 ms.
#[must_use]
pub const fn is_cursor_visible(millis_since_focus: u64) -> bool {
    millis_since_focus / CURSOR_BLINK_INTERVAL_MS % 2 == 0
}

/// Everything one `EditBox` draw needs, derived once by
/// [`EditBox::draw_state`].
///
/// This exists so `extractWidgetRenderState`'s arithmetic
/// (`EditBox.java:404-473`) lives *here*, next to the state it reads, rather than
/// being re-derived inside `super::render`'s draw loop. #393 established the
/// discipline: a screen asks the widget, it does not restate the widget's rules.
#[derive(Debug, Clone, PartialEq)]
pub struct EditBoxDraw {
    /// The visible text left of the caret — or the whole visible slice when the
    /// caret has scrolled off.
    pub before: String,
    /// The visible text right of the caret. Empty when the caret is at the end
    /// of the visible slice.
    pub after: String,
    /// Where [`Self::before`] starts.
    pub before_x: f32,
    /// Where [`Self::after`] starts. Note this is **not** `before_x +
    /// width(before)`: vanilla adds a 1 px gap after the first half and then
    /// takes it back when the caret is in insert mode (`EditBox.java:422-432`).
    pub after_x: f32,
    /// The caret's left edge.
    pub cursor_x: f32,
    /// Whether the caret is the 1 px insert bar (`|`) rather than the appended
    /// underscore. `true` when the caret is not at the end of the value, or the
    /// value is already at `maxLength`.
    pub insert_cursor: bool,
    /// The selection rect as `(from_x, to_x)`, or `None` when nothing is
    /// selected. Both are clamped to the box's right edge, as vanilla clamps
    /// them.
    pub highlight: Option<(f32, f32)>,
    /// The text baseline row every part of this draw shares.
    pub text_y: f32,
    /// Whether the caret should be painted at all.
    pub show_cursor: bool,
}

/// `EditBox` (`client/gui/components/EditBox.java`).
///
/// Wraps a [`Widget`] rather than duplicating its bounds and state, so the
/// [`LayoutElement`] seam #394's containers arrange through, and the
/// `active`/`visible`/`focused` flags #395 dispatches on, have exactly one
/// definition.
#[derive(Debug, Clone, PartialEq)]
pub struct EditBox {
    /// The `AbstractWidget` half: bounds, `active`, `visible`, `focused`, and
    /// the narration message (`EditBox` never draws it — `createNarrationMessage`
    /// is its only reader, `EditBox.java:91-95`).
    pub widget: Widget,
    /// `EditBox.value`.
    value: String,
    /// `EditBox.maxLength`, in `char`s — see the module docs.
    max_length: usize,
    /// `EditBox.bordered`. `false` drops the sprite *and* the 4 px text inset.
    pub bordered: bool,
    /// `EditBox.canLoseFocus`. When `false`, `setFocused(false)` is ignored
    /// entirely (`EditBox.java:529-540`) — how the chat prompt keeps the
    /// keyboard.
    pub can_lose_focus: bool,
    /// `EditBox.isEditable`. Separate from `active`: this is what greys the text
    /// and blocks insertion, and it is what
    /// [`Self::text_colour`]/[`Self::can_consume_input`] key on.
    pub is_editable: bool,
    /// `EditBox.centered`.
    pub centered: bool,
    /// `EditBox.cursorPos`.
    cursor_pos: usize,
    /// `EditBox.highlightPos` — the *other* end of the selection. Equal to
    /// [`Self::cursor_pos`] when nothing is selected.
    highlight_pos: usize,
    /// `EditBox.displayPos`: the first visible character, i.e. the horizontal
    /// scroll offset in characters.
    display_pos: usize,
    /// `EditBox.suggestion` — ghost text after the caret, drawn only when the
    /// caret is in *append* mode.
    pub suggestion: Option<String>,
    /// `EditBox.hint` — ghost text shown when the box is empty and unfocused.
    pub hint: Option<String>,
    /// Width of one character, in the units the text is drawn in. See
    /// [`MENU_TEXT_ADVANCE`] and the module docs on why this is not a `Font`.
    pub advance: f32,
}

impl EditBox {
    /// A bordered, editable box at `(x, y)` with the given size. `message` is
    /// vanilla's narration `Component`, not drawn.
    #[must_use]
    pub fn new(x: f32, y: f32, width: f32, height: f32, message: impl Into<String>) -> Self {
        Self {
            // `Widget::new`, not `Widget::button`: an `EditBox`'s background is
            // its *own* two-state sprite set, and giving it `BUTTON_SPRITES`
            // would invent the disabled art the module docs say does not exist.
            widget: Widget::new(x, y, width, height, message),
            value: String::new(),
            max_length: DEFAULT_MAX_LENGTH,
            bordered: true,
            can_lose_focus: true,
            is_editable: true,
            centered: false,
            cursor_pos: 0,
            highlight_pos: 0,
            display_pos: 0,
            suggestion: None,
            hint: None,
            advance: MENU_TEXT_ADVANCE,
        }
    }

    /// `EditBox(font, narration)`'s 150×20 at the origin.
    #[must_use]
    pub fn default_sized(message: impl Into<String>) -> Self {
        Self::new(0.0, 0.0, DEFAULT_WIDTH, DEFAULT_HEIGHT, message)
    }

    /// `getValue()`.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// The value's length in `char`s — the unit every position here is in.
    #[must_use]
    pub fn len(&self) -> usize {
        self.value.chars().count()
    }

    /// Whether the value is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// `getCursorPosition()`.
    #[must_use]
    pub fn cursor_position(&self) -> usize {
        self.cursor_pos
    }

    /// `EditBox.highlightPos`.
    #[must_use]
    pub fn highlight_position(&self) -> usize {
        self.highlight_pos
    }

    /// `EditBox.displayPos`.
    #[must_use]
    pub fn display_position(&self) -> usize {
        self.display_pos
    }

    /// `EditBox.maxLength`.
    #[must_use]
    pub fn max_length(&self) -> usize {
        self.max_length
    }

    /// `setMaxLength(int)` (`EditBox.java:495-501`): truncates an over-long
    /// value in place, and notably does **not** move the cursor.
    pub fn set_max_length(&mut self, max_length: usize) {
        self.max_length = max_length;
        if self.len() > max_length {
            self.value = self.value.chars().take(max_length).collect();
            self.on_value_change();
        }
    }

    /// `setMaxLength` as a builder step, for a field declared inline.
    #[must_use]
    pub fn with_max_length(mut self, max_length: usize) -> Self {
        self.set_max_length(max_length);
        self
    }

    /// `setValue(String)` (`EditBox.java:97-107`): truncate to `maxLength`, then
    /// cursor **and** highlight to the end.
    pub fn set_value(&mut self, value: impl AsRef<str>) {
        let value = value.as_ref();
        self.value = if value.chars().count() > self.max_length {
            value.chars().take(self.max_length).collect()
        } else {
            value.to_owned()
        };
        self.move_cursor_to_end(false);
        self.set_highlight_pos(self.cursor_pos);
        self.on_value_change();
    }

    /// `setValue` as a builder step.
    #[must_use]
    pub fn with_value(mut self, value: impl AsRef<str>) -> Self {
        self.set_value(value);
        self
    }

    /// `getHighlighted()` — the selected substring, which is empty when
    /// `cursorPos == highlightPos`.
    #[must_use]
    pub fn highlighted(&self) -> String {
        let (start, end) = self.selection();
        self.value
            .chars()
            .skip(start)
            .take(end - start)
            .collect()
    }

    /// `(min(cursorPos, highlightPos), max(..))`.
    #[must_use]
    fn selection(&self) -> (usize, usize) {
        (
            self.cursor_pos.min(self.highlight_pos),
            self.cursor_pos.max(self.highlight_pos),
        )
    }

    /// Byte offset of `char` index `i`, or the string length past the end.
    fn byte_of(&self, i: usize) -> usize {
        self.value
            .char_indices()
            .nth(i)
            .map_or(self.value.len(), |(b, _)| b)
    }

    /// `Font.width(s)` under this box's fixed advance.
    #[must_use]
    pub fn measure(&self, s: &str) -> f32 {
        s.chars().count() as f32 * self.advance
    }

    /// [`measure`](Self::measure), but consulting `measure_fn` first when
    /// given — the seam the module docs describe. `render.rs` passes the
    /// exact proportional width function it is about to *draw* `s` with
    /// (`Quads::text_width`), so the caret advances by what was actually
    /// painted rather than by this box's own fixed approximation. `None`
    /// behaves identically to [`measure`](Self::measure).
    #[must_use]
    pub fn measure_with(&self, s: &str, measure_fn: Option<&dyn Fn(&str) -> f32>) -> f32 {
        match measure_fn {
            Some(f) => f(s),
            None => self.measure(s),
        }
    }

    /// `Font.plainSubstrByWidth(s, width)`'s *length*, under a fixed advance.
    fn fits(&self, width: f32) -> usize {
        if self.advance <= 0.0 {
            return 0;
        }
        (width / self.advance).floor().max(0.0) as usize
    }

    /// `getInnerWidth()` (`EditBox.java:571-573`): the width text may occupy,
    /// which for a bordered box is 8 px less — 4 on each side.
    #[must_use]
    pub fn inner_width(&self) -> f32 {
        if self.bordered {
            self.widget.width - 2.0 * BORDER_INSET
        } else {
            self.widget.width
        }
    }

    /// `updateTextPosition()`'s `textX` (`EditBox.java:487-493`), as a method —
    /// see the module docs on why it is not cached.
    #[must_use]
    pub fn text_x(&self) -> f32 {
        self.text_x_with(None)
    }

    /// [`text_x`](Self::text_x), but consulting `measure_fn` (see
    /// [`measure_with`](Self::measure_with)) instead of the fixed advance when
    /// `centered` is set. `None` behaves identically to
    /// [`text_x`](Self::text_x); no current caller sets `centered`, but a
    /// centred box measured with a real font would have this exact "gap grows"
    /// bug in its own centring arithmetic if this used the fixed path instead.
    #[must_use]
    pub fn text_x_with(&self, measure_fn: Option<&dyn Fn(&str) -> f32>) -> f32 {
        if self.centered {
            self.widget.x
                + ((self.widget.width - self.measure_with(self.displayed(), measure_fn)) / 2.0)
                    .floor()
        } else if self.bordered {
            self.widget.x + BORDER_INSET
        } else {
            self.widget.x
        }
    }

    /// `updateTextPosition()`'s `textY`. `(this.height - 8) / 2` is Java integer
    /// division, so it truncates — reproduced with `floor`.
    #[must_use]
    pub fn text_y(&self) -> f32 {
        if self.bordered {
            self.widget.y + ((self.widget.height - 8.0) / 2.0).floor()
        } else {
            self.widget.y
        }
    }

    /// The visible slice: `font.plainSubstrByWidth(value.substring(displayPos),
    /// getInnerWidth())`.
    #[must_use]
    pub fn displayed(&self) -> &str {
        let from = self.byte_of(self.display_pos);
        let tail = &self.value[from..];
        let n = self.fits(self.inner_width());
        match tail.char_indices().nth(n) {
            Some((b, _)) => &tail[..b],
            None => tail,
        }
    }

    /// `setCursorPosition(int)` (`EditBox.java:256-259`): clamp, then scroll so
    /// the new position is visible.
    pub fn set_cursor_position(&mut self, pos: usize) {
        self.cursor_pos = pos.min(self.len());
        self.scroll_to(self.cursor_pos);
    }

    /// `setHighlightPos(int)` (`EditBox.java:575-578`). Also scrolls — which is
    /// why dragging a selection off the right edge follows it.
    pub fn set_highlight_pos(&mut self, pos: usize) {
        self.highlight_pos = pos.min(self.len());
        self.scroll_to(self.highlight_pos);
    }

    /// `moveCursorTo(int, boolean)` (`EditBox.java:247-254`). `extend_selection`
    /// is Shift: it leaves `highlightPos` where it was.
    pub fn move_cursor_to(&mut self, pos: usize, extend_selection: bool) {
        self.set_cursor_position(pos);
        if !extend_selection {
            self.set_highlight_pos(self.cursor_pos);
        }
    }

    /// `moveCursor(int, boolean)`. `dir` is `EditBox.FORWARDS`/`BACKWARDS`,
    /// i.e. `±1`; vanilla's `Util.offsetByCodepoints` is a plain step here (see
    /// the module docs).
    pub fn move_cursor(&mut self, dir: i32, extend_selection: bool) {
        let target = self.offset_cursor(dir);
        self.move_cursor_to(target, extend_selection);
    }

    fn offset_cursor(&self, dir: i32) -> usize {
        if dir >= 0 {
            self.cursor_pos.saturating_add(dir.unsigned_abs() as usize).min(self.len())
        } else {
            self.cursor_pos.saturating_sub(dir.unsigned_abs() as usize)
        }
    }

    /// `moveCursorToStart(boolean)`.
    pub fn move_cursor_to_start(&mut self, extend_selection: bool) {
        self.move_cursor_to(0, extend_selection);
    }

    /// `moveCursorToEnd(boolean)`.
    pub fn move_cursor_to_end(&mut self, extend_selection: bool) {
        self.move_cursor_to(self.len(), extend_selection);
    }

    /// `insertText(String)` (`EditBox.java:131-152`): replace the selection with
    /// `input`, filtered and truncated to what `maxLength` still allows.
    ///
    /// Read vanilla's budget again — it is written oddly and it is right:
    /// `maxLength - value.length() - (start - end)`, where `start <= end`, so the
    /// third term *adds* the selection length back. Inserting into a selection
    /// can therefore always proceed even when the value is already full.
    pub fn insert_text(&mut self, input: &str) {
        let (start, end) = self.selection();
        let len = self.len();
        let budget = self.max_length as isize - len as isize + (end as isize - start as isize);
        if budget <= 0 {
            return;
        }
        let mut text = filter_text(input);
        let budget = budget as usize;
        if text.chars().count() > budget {
            text = text.chars().take(budget).collect();
        }
        let insertion_len = text.chars().count();
        let (sb, eb) = (self.byte_of(start), self.byte_of(end));
        self.value.replace_range(sb..eb, &text);
        self.set_cursor_position(start + insertion_len);
        self.set_highlight_pos(self.cursor_pos);
        self.on_value_change();
    }

    /// `deleteText(dir, wholeWord)` — the switch `keyPressed` calls for
    /// Backspace and Delete.
    pub fn delete_text(&mut self, dir: i32, whole_word: bool) {
        if whole_word {
            self.delete_words(dir);
        } else {
            self.delete_chars(dir);
        }
    }

    /// `deleteWords(int)` (`EditBox.java:170-178`). A live selection wins: it is
    /// deleted instead of a word, which is why Ctrl+Backspace over a selection
    /// does not eat the word before it as well.
    pub fn delete_words(&mut self, dir: i32) {
        if self.value.is_empty() {
            return;
        }
        if self.highlight_pos != self.cursor_pos {
            self.insert_text("");
        } else {
            let pos = self.word_position(dir);
            self.delete_chars_to_pos(pos);
        }
    }

    /// `deleteChars(int)`.
    pub fn delete_chars(&mut self, dir: i32) {
        let pos = self.offset_cursor(dir);
        self.delete_chars_to_pos(pos);
    }

    /// `deleteCharsToPos(int)` (`EditBox.java:184-199`).
    pub fn delete_chars_to_pos(&mut self, pos: usize) {
        if self.value.is_empty() {
            return;
        }
        if self.highlight_pos != self.cursor_pos {
            self.insert_text("");
            return;
        }
        let start = pos.min(self.cursor_pos);
        let end = pos.max(self.cursor_pos);
        if start == end {
            return;
        }
        let (sb, eb) = (self.byte_of(start), self.byte_of(end));
        self.value.replace_range(sb..eb, "");
        self.set_cursor_position(start);
        self.on_value_change();
        // Vanilla calls `moveCursorTo(start, false)` *after* `setCursorPosition`
        // and `onValueChange` (`:193-195`). The extra call is what collapses the
        // selection; the redundant re-set is reproduced rather than tidied so a
        // diff against the jar is clean.
        self.move_cursor_to(start, false);
    }

    /// `getWordPosition(dir)` from the current cursor.
    #[must_use]
    pub fn word_position(&self, dir: i32) -> usize {
        self.word_position_from(dir, self.cursor_pos, true)
    }

    /// `getWordPosition(dir, from, stripSpaces)` (`EditBox.java:209-237`),
    /// transcribed over `char` indices.
    #[must_use]
    pub fn word_position_from(&self, dir: i32, from: usize, strip_spaces: bool) -> usize {
        let chars: Vec<char> = self.value.chars().collect();
        let mut result = from.min(chars.len());
        let reverse = dir < 0;
        for _ in 0..dir.unsigned_abs() {
            if reverse {
                while strip_spaces && result > 0 && chars[result - 1] == ' ' {
                    result -= 1;
                }
                while result > 0 && chars[result - 1] != ' ' {
                    result -= 1;
                }
            } else {
                let length = chars.len();
                result = match chars[result.min(length)..].iter().position(|&c| c == ' ') {
                    Some(offset) => {
                        let mut r = result + offset;
                        while strip_spaces && r < length && chars[r] == ' ' {
                            r += 1;
                        }
                        r
                    }
                    // `indexOf` returning -1 becomes the end of the value.
                    None => length,
                };
            }
        }
        result
    }

    /// `scrollTo(int)` (`EditBox.java:580-598`): nudge `displayPos` until `pos`
    /// is inside the visible window.
    ///
    /// The order is deliberate and slightly strange: `lastPos` is computed from
    /// the **old** `displayPos`, then the `pos == displayPos` branch may change
    /// `displayPos`, and the two comparisons that follow still test against the
    /// stale `lastPos`. Reproduced, not corrected.
    fn scroll_to(&mut self, pos: usize) {
        let len = self.len();
        self.display_pos = self.display_pos.min(len);
        let inner = self.inner_width();
        let visible = self.fits(inner).min(len.saturating_sub(self.display_pos));
        let last_pos = visible + self.display_pos;
        if pos == self.display_pos {
            // `displayPos -= plainSubstrByWidth(value, innerWidth, true).length()`:
            // a whole window's worth, backwards, capped by the value's length.
            self.display_pos = self.display_pos.saturating_sub(self.fits(inner).min(len));
        }
        if pos > last_pos {
            self.display_pos += pos - last_pos;
        } else if pos <= self.display_pos {
            // `displayPos - (displayPos - pos)`, i.e. `pos`.
            self.display_pos = pos;
        }
        self.display_pos = self.display_pos.min(len);
    }

    /// `onValueChange` minus the responder callback: vanilla's `Consumer<String>`
    /// exists so a screen can react to typing, and every consumer in this shell
    /// reads the value back from the widget instead. The other half —
    /// `updateTextPosition()` — is not needed because `textX`/`textY` are
    /// computed on demand.
    fn on_value_change(&mut self) {
        self.display_pos = self.display_pos.min(self.len());
        self.cursor_pos = self.cursor_pos.min(self.len());
        self.highlight_pos = self.highlight_pos.min(self.len());
    }

    /// `canConsumeInput()` (`EditBox.java:344-346`):
    /// `isActive() && isFocused() && isEditable()`.
    #[must_use]
    pub fn can_consume_input(&self) -> bool {
        self.widget.is_active() && self.widget.focused && self.is_editable
    }

    /// `SPRITES.get(isActive(), isFocused())` (`EditBox.java:406-409`), or `None`
    /// when `bordered` is false — an unbordered box draws no background at all.
    ///
    /// **Both arguments differ from `AbstractButton`'s.** See the module docs:
    /// `isActive()` rather than the raw `active` field, and `isFocused()` rather
    /// than `isHoveredOrFocused()`.
    #[must_use]
    pub fn background_sprite(&self) -> Option<&'static str> {
        self.bordered
            .then(|| SPRITES.get(self.widget.is_active(), self.widget.focused))
    }

    /// `isEditable ? textColor : textColorUneditable` (`EditBox.java:411`).
    ///
    /// Note the flag: **`isEditable`, not `active`**. A widget can be active and
    /// uneditable.
    #[must_use]
    pub fn text_colour(&self) -> [f32; 4] {
        argb_to_rgba(if self.is_editable {
            DEFAULT_TEXT_COLOR_ARGB
        } else {
            TEXT_COLOR_UNEDITABLE_ARGB
        })
    }

    /// Whether the caret is painted this frame.
    ///
    /// `millis_since_focus` is `Util.getMillis() - focusedTime`; `None` means
    /// "no clock available, always visible", which is what `super::render`
    /// passes — see the module docs.
    #[must_use]
    pub fn show_cursor(&self, millis_since_focus: Option<u64>) -> bool {
        self.widget.focused && millis_since_focus.is_none_or(is_cursor_visible)
    }

    /// `extractWidgetRenderState`'s geometry (`EditBox.java:404-473`), gathered
    /// so `super::render` reads rather than re-derives it.
    #[must_use]
    pub fn draw_state(&self, millis_since_focus: Option<u64>) -> EditBoxDraw {
        self.draw_state_with(millis_since_focus, None)
    }

    /// [`draw_state`](Self::draw_state), but every measurement
    /// (`text_x`, the width of `before`, the highlight's x) goes through
    /// `measure_fn` when given, instead of this box's fixed
    /// [`MENU_TEXT_ADVANCE`] approximation. `None` behaves identically to
    /// [`draw_state`](Self::draw_state).
    ///
    /// This is the fix for the "cursor gap grows while typing" report: the
    /// caller must pass the *same* width function the glyphs are about to be
    /// drawn with (`render.rs`'s `draw_edit_box` passes `Quads::text_width`),
    /// or the caret and the text it follows are measured by two different
    /// rulers again. See the module docs.
    #[must_use]
    pub fn draw_state_with(
        &self,
        millis_since_focus: Option<u64>,
        measure_fn: Option<&dyn Fn(&str) -> f32>,
    ) -> EditBoxDraw {
        let text_x = self.text_x_with(measure_fn);
        let text_y = self.text_y();
        let displayed = self.displayed();
        let displayed_len = displayed.chars().count();
        // `relCursorPos` can be negative in Java; here the two cases are
        // "before the window" and "after it", and `cursor_on_screen` is the
        // conjunction vanilla writes as `>= 0 && <= displayed.length()`.
        let rel_cursor = self.cursor_pos.saturating_sub(self.display_pos);
        let cursor_on_screen = self.cursor_pos >= self.display_pos && rel_cursor <= displayed_len;
        let rel_highlight = self
            .highlight_pos
            .saturating_sub(self.display_pos)
            .min(displayed_len);

        let split = if cursor_on_screen { rel_cursor } else { displayed_len };
        let before: String = displayed.chars().take(split).collect();
        let after: String = if cursor_on_screen {
            displayed.chars().skip(rel_cursor).collect()
        } else {
            String::new()
        };

        let mut draw_x = text_x;
        if !displayed.is_empty() {
            draw_x += self.measure_with(&before, measure_fn) + 1.0;
        }
        // `insert` is what picks the bar over the underscore, and it is also what
        // suppresses the suggestion.
        let insert_cursor = self.cursor_pos < self.len() || self.len() >= self.max_length;
        let mut cursor_x = draw_x;
        if !cursor_on_screen {
            cursor_x = if rel_cursor > 0 {
                text_x + self.widget.width
            } else {
                text_x
            };
        } else if insert_cursor {
            cursor_x -= 1.0;
            draw_x -= 1.0;
        }

        let right_edge = self.widget.x + self.widget.width;
        let highlight = (rel_highlight != rel_cursor).then(|| {
            let highlight_x = text_x
                + self.measure_with(
                    &displayed.chars().take(rel_highlight).collect::<String>(),
                    measure_fn,
                );
            (
                cursor_x.min(right_edge),
                (highlight_x - 1.0).min(right_edge),
            )
        });

        EditBoxDraw {
            before,
            after,
            before_x: text_x,
            after_x: draw_x,
            cursor_x,
            insert_cursor,
            highlight,
            text_y,
            show_cursor: self.show_cursor(millis_since_focus) && cursor_on_screen,
        }
    }

    /// `EditBox.keyPressed(KeyEvent)` (`EditBox.java:270-342`).
    ///
    /// **The `false` returns are the interesting part**, not the `true` ones —
    /// they are what let Up/Down (and anything unhandled) reach
    /// `Screen`'s focus navigation. See the module docs.
    pub fn handle_key(&mut self, event: KeyEvent) -> bool {
        if !(self.widget.is_active() && self.widget.focused) {
            return false;
        }
        match event.key {
            KEY_BACKSPACE => {
                if self.is_editable {
                    self.delete_text(-1, event.has_control_down_with_quirk());
                }
                true
            }
            KEY_DELETE => {
                if self.is_editable {
                    self.delete_text(1, event.has_control_down_with_quirk());
                }
                true
            }
            KEY_RIGHT => {
                if event.has_control_down_with_quirk() {
                    let pos = self.word_position(1);
                    self.move_cursor_to(pos, event.has_shift_down());
                } else {
                    self.move_cursor(1, event.has_shift_down());
                }
                true
            }
            KEY_LEFT => {
                if event.has_control_down_with_quirk() {
                    let pos = self.word_position(-1);
                    self.move_cursor_to(pos, event.has_shift_down());
                } else {
                    self.move_cursor(-1, event.has_shift_down());
                }
                true
            }
            KEY_HOME => {
                self.move_cursor_to_start(event.has_shift_down());
                true
            }
            KEY_END => {
                self.move_cursor_to_end(event.has_shift_down());
                true
            }
            // Vanilla's `default:` group, which the Insert / Up / Down / PageUp /
            // PageDown cases deliberately fall *into* (`EditBox.java:279-284`) —
            // so those five keys reach the shortcut tests and then return false,
            // which is exactly how vertical arrows escape to focus navigation.
            _ => {
                if event.is_select_all() {
                    self.move_cursor_to_end(false);
                    self.set_highlight_pos(0);
                    true
                } else if event.is_copy() {
                    // `keyboardHandler.setClipboard(this.getHighlighted())` —
                    // unconditional, even on a non-editable (read-only,
                    // still-selectable) field.
                    clipboard_seam::set(&self.highlighted());
                    true
                } else if event.is_paste() {
                    // `insertText(getClipboard())`, gated on `isEditable()` —
                    // the read happens either way in vanilla, but there is
                    // nothing to do with it on a read-only field.
                    if self.is_editable {
                        let text = clipboard_seam::get();
                        self.insert_text(&text);
                    }
                    true
                } else if event.is_cut() {
                    // `setClipboard(getHighlighted())` first, then delete the
                    // selection — same order as vanilla, so a cut on a
                    // read-only field still copies (it just cannot delete).
                    clipboard_seam::set(&self.highlighted());
                    if self.is_editable {
                        self.insert_text("");
                    }
                    true
                } else {
                    false
                }
            }
        }
    }

    /// `EditBox.charTyped(CharacterEvent)` (`EditBox.java:348-363`).
    pub fn handle_char(&mut self, ch: char) -> bool {
        if !self.can_consume_input() {
            return false;
        }
        if is_allowed_chat_character(ch) {
            if self.is_editable {
                self.insert_text(&ch.to_string());
            }
            true
        } else {
            false
        }
    }

    /// `onClick` (`EditBox.java:385-392`): put the caret at the clicked
    /// character, extending the selection if Shift is held.
    ///
    /// `findClickedPositionInText` (`:371-375`) clamps the click's offset to
    /// `getInnerWidth()` first, so a click past the right edge lands at the end
    /// of the *visible* text rather than the end of the value.
    pub fn click_at(&mut self, mouse_x: f32, extend_selection: bool) {
        let pos = self.clicked_position(mouse_x);
        self.move_cursor_to(pos, extend_selection);
    }

    /// `findClickedPositionInText(event)`.
    #[must_use]
    pub fn clicked_position(&self, mouse_x: f32) -> usize {
        let offset = (mouse_x.floor() - self.text_x()).min(self.inner_width());
        let visible = self.displayed().chars().count();
        self.display_pos + self.fits(offset.max(0.0)).min(visible)
    }
}

impl super::focus::FocusTarget for EditBox {
    fn rectangle(&self) -> ScreenRectangle {
        ScreenRectangle::from_rect(self.widget.rect())
    }

    fn is_active(&self) -> bool {
        self.widget.is_active()
    }

    fn is_focused(&self) -> bool {
        self.widget.focused
    }

    /// `EditBox.setFocused(boolean)` (`EditBox.java:528-540`): the
    /// `canLoseFocus || focused` guard means a `canLoseFocus == false` box
    /// **ignores** being unfocused.
    fn set_focused(&mut self, focused: bool) {
        if self.can_lose_focus || focused {
            self.widget.focused = focused;
        }
    }

    fn takes_focus(&self) -> bool {
        self.widget.takes_focus()
    }

    fn is_mouse_over(&self, x: f32, y: f32) -> bool {
        self.widget.is_mouse_over(x, y)
    }

    /// `AbstractWidget.mouseClicked` -> `onClick` (`AbstractWidget.java:109-125`,
    /// `EditBox.java:385-392`): the caret moves to the click *and* the click is
    /// reported as consumed, which is what makes `ContainerEventHandler` focus
    /// the box.
    fn mouse_clicked(&mut self, x: f32, y: f32) -> bool {
        if !self.widget.is_mouse_over(x, y) {
            return false;
        }
        self.click_at(x, false);
        true
    }

    fn key_pressed(&mut self, event: KeyEvent) -> bool {
        self.handle_key(event)
    }

    fn char_typed(&mut self, ch: char) -> bool {
        self.handle_char(ch)
    }

    fn current_focus_path(&self, id: usize) -> ComponentPath {
        ComponentPath::Leaf(id)
    }
}

impl LayoutElement for EditBox {
    fn x(&self) -> f32 {
        self.widget.x
    }

    fn y(&self) -> f32 {
        self.widget.y
    }

    fn width(&self) -> f32 {
        self.widget.width
    }

    fn height(&self) -> f32 {
        self.widget.height
    }

    fn set_x(&mut self, x: f32) {
        self.widget.x = x;
    }

    fn set_y(&mut self, y: f32) {
        self.widget.y = y;
    }

    fn visit_widgets(&self, visitor: &mut dyn FnMut(&Widget)) {
        visitor(&self.widget);
    }
}

#[cfg(test)]
mod tests {
    use super::super::focus::{
        FocusTarget, KeyEvent, EDIT_SHORTCUT_MODIFIER, KEY_A, KEY_BACKSPACE, KEY_C, KEY_DELETE,
        KEY_DOWN, KEY_END, KEY_HOME, KEY_LEFT, KEY_RIGHT, KEY_TAB, KEY_UP, KEY_V, KEY_X,
        MOD_SHIFT,
    };
    use super::*;

    /// A field wide enough that nothing scrolls, so cursor tests are not also
    /// `displayPos` tests. `inner_width` = 400 - 8 = 392, /6 = 65 visible.
    fn field() -> EditBox {
        let mut b = EditBox::new(0.0, 0.0, 400.0, 20.0, "Address");
        b.widget.focused = true;
        b
    }

    fn typed(b: &mut EditBox, s: &str) {
        for ch in s.chars() {
            assert!(b.handle_char(ch), "`{ch}` must be accepted");
        }
    }

    #[test]
    fn vanillas_own_constants_are_lifted_not_invented() {
        // The values come from the jar; the *channels* are derived from them, so
        // neither can agree with itself.
        assert_eq!(DEFAULT_MAX_LENGTH, 32, "`EditBox.java:40`");
        assert_eq!(DEFAULT_TEXT_COLOR_ARGB, -2_039_584, "`EditBox.java:35`");
        assert_eq!(TEXT_COLOR_UNEDITABLE_ARGB, -9_408_400, "`EditBox.java:51`");
        assert_eq!(DEFAULT_TEXT_COLOR_ARGB as u32, 0xFF_E0_E0_E0);
        // The two colours must actually differ, or `text_colour`'s branch is
        // unobservable and the `isEditable` keying could not be wrong.
        let mut b = field();
        let editable = b.text_colour();
        b.is_editable = false;
        let uneditable = b.text_colour();
        assert_ne!(editable, uneditable);
        assert_eq!(editable, argb_to_rgba(DEFAULT_TEXT_COLOR_ARGB));
        assert_eq!(uneditable, argb_to_rgba(TEXT_COLOR_UNEDITABLE_ARGB));
        // And it keys on `isEditable`, *not* on `active`: disabling the widget
        // must leave the text colour alone.
        b.is_editable = true;
        b.widget.active = false;
        assert_eq!(
            b.text_colour(),
            editable,
            "`EditBox.java:411` reads `isEditable`; keying on `active` here would \
             grey the text of every disabled-but-editable field"
        );
    }

    #[test]
    fn the_sprite_is_the_two_argument_collapse_and_keys_on_focus_alone() {
        // `EditBox.java:30-32` + `:407`. Both differences from `AbstractButton`
        // are asserted, because both are easy to "fix" into the button's rule.
        assert_eq!(SPRITES.enabled, "widget/text_field");
        assert_eq!(
            SPRITES.disabled, SPRITES.enabled,
            "the 2-argument constructor collapses disabled onto enabled — there \
             is no `text_field_disabled` in the pack"
        );
        assert_eq!(SPRITES.enabled_focused, "widget/text_field_highlighted");
        assert_eq!(SPRITES.disabled_focused, SPRITES.enabled_focused);

        let mut b = EditBox::new(0.0, 0.0, 150.0, 20.0, "Name");
        assert_eq!(b.background_sprite(), Some("widget/text_field"));
        b.widget.focused = true;
        assert_eq!(
            b.background_sprite(),
            Some("widget/text_field_highlighted")
        );
        // Hover must NOT highlight it: `EditBox` passes `isFocused()` where
        // `AbstractButton` passes `isHoveredOrFocused()`.
        b.widget.focused = false;
        b.widget.hovered = true;
        assert_eq!(
            b.background_sprite(),
            Some("widget/text_field"),
            "hovering a text field draws the plain sprite; the `||` belongs to \
             `AbstractButton`, not here"
        );
        // The control: the same flags on a *button* do highlight, so this is a
        // real difference between the two widgets and not a broken predicate.
        let mut button = Widget::button(0.0, 0.0, 150.0, 20.0, "Save");
        button.hovered = true;
        assert_eq!(button.background_sprite(), Some("widget/button_highlighted"));

        // `isActive()` not `active`: an invisible box is not drawn at all, but
        // the first argument still changes — and since disabled == enabled here,
        // it is unobservable, which is exactly why there is no disabled art.
        b.widget.hovered = false;
        b.widget.visible = false;
        assert_eq!(b.background_sprite(), Some("widget/text_field"));
        // And `bordered = false` draws no background whatsoever.
        b.bordered = false;
        assert_eq!(b.background_sprite(), None);
    }

    #[test]
    fn typing_moves_the_caret_and_the_max_length_stops_it() {
        let mut b = field();
        assert_eq!((b.cursor_position(), b.len()), (0, 0));
        typed(&mut b, "mc.example.com");
        assert_eq!(b.value(), "mc.example.com");
        assert_eq!(b.cursor_position(), 14, "the caret follows insertion");
        // The cap is in characters and it is vanilla's default until set.
        assert_eq!(b.max_length(), DEFAULT_MAX_LENGTH);
        for _ in 0..40 {
            b.handle_char('x');
        }
        assert_eq!(b.len(), DEFAULT_MAX_LENGTH);
        assert_eq!(b.cursor_position(), DEFAULT_MAX_LENGTH);
        // A refused character is refused *and* reported as unconsumed, so the
        // screen can still act on it — that is `charTyped`'s contract.
        let mut b = field();
        assert!(!b.handle_char('\u{a7}'), "the section sign is filtered out");
        assert!(!b.handle_char('\u{7f}'), "and DEL");
        assert!(b.is_empty());
        // But the C1 range *is* allowed, unlike Rust's `char::is_control`.
        assert!(is_allowed_chat_character('\u{80}'));
        assert!(!is_allowed_chat_character('\u{1f}'));
    }

    #[test]
    fn horizontal_arrows_move_the_caret_and_vertical_ones_are_declined() {
        // This is the ordering #395 exists for, at the widget end: `EditBox`
        // consumes 262/263 and refuses 264/265, and that refusal is what lets a
        // screen move focus with Up/Down while the field keeps Left/Right.
        let mut b = field();
        typed(&mut b, "abcd");
        assert_eq!(b.cursor_position(), 4);
        assert!(b.handle_key(KeyEvent::new(KEY_LEFT)));
        assert_eq!(b.cursor_position(), 3);
        assert!(b.handle_key(KeyEvent::new(KEY_LEFT)));
        assert_eq!(b.cursor_position(), 2);
        assert!(b.handle_key(KeyEvent::new(KEY_RIGHT)));
        assert_eq!(b.cursor_position(), 3);
        assert!(b.handle_key(KeyEvent::new(KEY_HOME)));
        assert_eq!(b.cursor_position(), 0);
        assert!(b.handle_key(KeyEvent::new(KEY_END)));
        assert_eq!(b.cursor_position(), 4);

        // The declines. Each one must leave the caret alone as well as return
        // false, or a screen would move focus *and* the caret.
        for key in [KEY_UP, KEY_DOWN, KEY_TAB] {
            assert!(
                !b.handle_key(KeyEvent::new(key)),
                "key {key} must fall through to the screen"
            );
            assert_eq!(b.cursor_position(), 4);
        }
        // Clamped at both ends rather than wrapping.
        b.handle_key(KeyEvent::new(KEY_HOME));
        assert!(b.handle_key(KeyEvent::new(KEY_LEFT)));
        assert_eq!(b.cursor_position(), 0);
        b.handle_key(KeyEvent::new(KEY_END));
        assert!(b.handle_key(KeyEvent::new(KEY_RIGHT)));
        assert_eq!(b.cursor_position(), 4);
    }

    #[test]
    fn an_unfocused_or_inactive_box_consumes_nothing() {
        // `keyPressed` is gated on `isActive() && isFocused()`
        // (`EditBox.java:271`) and `charTyped` on `canConsumeInput()`, which adds
        // `isEditable()`. Three different gates, and they are not the same.
        let mut b = field();
        b.widget.focused = false;
        assert!(!b.handle_key(KeyEvent::new(KEY_LEFT)));
        assert!(!b.handle_char('a'));
        b.widget.focused = true;
        b.widget.active = false;
        assert!(!b.handle_key(KeyEvent::new(KEY_LEFT)));
        assert!(!b.handle_char('a'));
        b.widget.active = true;
        b.is_editable = false;
        assert!(
            b.handle_key(KeyEvent::new(KEY_LEFT)),
            "an uneditable box still moves its caret — only insertion is blocked"
        );
        assert!(!b.handle_char('a'), "but typing does nothing");
        assert!(b.is_empty());
        // Backspace on an uneditable box is *consumed* and does nothing
        // (`EditBox.java:273-278`: the `isEditable` test is inside the case, and
        // `return true` is outside it).
        b.is_editable = true;
        typed(&mut b, "ab");
        b.is_editable = false;
        assert!(b.handle_key(KeyEvent::new(KEY_BACKSPACE)));
        assert_eq!(b.value(), "ab", "consumed, but nothing deleted");
    }

    #[test]
    fn backspace_and_delete_work_from_the_caret_not_the_end() {
        let mut b = field();
        typed(&mut b, "abcd");
        b.move_cursor_to(2, false);
        assert!(b.handle_key(KeyEvent::new(KEY_BACKSPACE)));
        assert_eq!((b.value(), b.cursor_position()), ("acd", 1));
        assert!(b.handle_key(KeyEvent::new(KEY_DELETE)));
        assert_eq!(
            (b.value(), b.cursor_position()),
            ("ad", 1),
            "Delete removes forward and leaves the caret where it was"
        );
        // Backspace at the start and Delete at the end are no-ops, still
        // consumed.
        b.move_cursor_to_start(false);
        assert!(b.handle_key(KeyEvent::new(KEY_BACKSPACE)));
        assert_eq!(b.value(), "ad");
        b.move_cursor_to_end(false);
        assert!(b.handle_key(KeyEvent::new(KEY_DELETE)));
        assert_eq!(b.value(), "ad");
    }

    #[test]
    fn a_selection_is_replaced_by_whatever_comes_next() {
        let mut b = field();
        typed(&mut b, "hello world");
        // Shift+Left thrice selects "rld" backwards from the end.
        for _ in 0..3 {
            b.handle_key(KeyEvent::with_modifiers(KEY_LEFT, MOD_SHIFT));
        }
        assert_eq!(b.highlighted(), "rld");
        assert_eq!((b.cursor_position(), b.highlight_position()), (8, 11));
        // Typing replaces it.
        typed(&mut b, "X");
        assert_eq!(b.value(), "hello woX");
        assert_eq!(b.highlighted(), "", "and collapses the selection");
        // Select-all then Backspace clears the field.
        let all = KeyEvent::with_modifiers(KEY_A, EDIT_SHORTCUT_MODIFIER);
        assert!(all.is_select_all(), "premise: the modifier is the quirked one");
        assert!(b.handle_key(all));
        assert_eq!(b.highlighted(), "hello woX");
        assert!(b.handle_key(KeyEvent::new(KEY_BACKSPACE)));
        assert!(b.is_empty());
    }

    /// Copy writes the *selection*, not the whole value, to
    /// [`clipboard_seam`]'s test-mode fake — `getHighlighted()` in the jar,
    /// not `getValue()`.
    #[test]
    fn copy_writes_only_the_selection_to_the_clipboard() {
        clipboard_seam::set(""); // isolate from any earlier test on this thread
        let mut b = field();
        typed(&mut b, "hello world");
        for _ in 0..3 {
            b.handle_key(KeyEvent::with_modifiers(KEY_LEFT, MOD_SHIFT));
        }
        assert_eq!(b.highlighted(), "rld");
        let copy = KeyEvent::with_modifiers(KEY_C, EDIT_SHORTCUT_MODIFIER);
        assert!(copy.is_copy());
        assert!(b.handle_key(copy));
        assert_eq!(clipboard_seam::get(), "rld");
        // Copy does not delete.
        assert_eq!(b.value(), "hello world");
    }

    /// Cut writes the selection to the clipboard *and* deletes it — same
    /// order as vanilla's `setClipboard` then `insertText("")`.
    #[test]
    fn cut_writes_the_selection_and_deletes_it() {
        clipboard_seam::set("");
        let mut b = field();
        typed(&mut b, "hello world");
        for _ in 0..3 {
            b.handle_key(KeyEvent::with_modifiers(KEY_LEFT, MOD_SHIFT));
        }
        let cut = KeyEvent::with_modifiers(KEY_X, EDIT_SHORTCUT_MODIFIER);
        assert!(cut.is_cut());
        assert!(b.handle_key(cut));
        assert_eq!(clipboard_seam::get(), "rld", "the cut text reached the clipboard");
        assert_eq!(b.value(), "hello wo", "and was removed from the field");
    }

    /// A cut on a non-editable field still copies but does not delete —
    /// vanilla's own `isEditable()` gate is on `insertText`, not on
    /// `setClipboard`.
    #[test]
    fn cut_on_a_read_only_field_copies_but_does_not_delete() {
        clipboard_seam::set("");
        let mut b = field();
        typed(&mut b, "hello world");
        b.is_editable = false;
        for _ in 0..3 {
            b.handle_key(KeyEvent::with_modifiers(KEY_LEFT, MOD_SHIFT));
        }
        assert!(b.handle_key(KeyEvent::with_modifiers(KEY_X, EDIT_SHORTCUT_MODIFIER)));
        assert_eq!(clipboard_seam::get(), "rld");
        assert_eq!(b.value(), "hello world", "nothing was deleted");
    }

    /// The reported bug's other half: paste inserts the clipboard's contents
    /// in place of the selection, exactly as typing over a selection does —
    /// see `insert_text`'s doc for why a single `insertText` call and this
    /// replace-then-continue behaviour are the same operation.
    #[test]
    fn paste_replaces_the_selection_with_the_clipboard() {
        clipboard_seam::set("goodbye");
        let mut b = field();
        typed(&mut b, "hello world");
        for _ in 0..3 {
            b.handle_key(KeyEvent::with_modifiers(KEY_LEFT, MOD_SHIFT));
        }
        assert_eq!(b.highlighted(), "rld");
        let paste = KeyEvent::with_modifiers(KEY_V, EDIT_SHORTCUT_MODIFIER);
        assert!(paste.is_paste());
        assert!(b.handle_key(paste));
        assert_eq!(b.value(), "hello wogoodbye");
    }

    /// Pasting with no selection inserts at the caret rather than replacing
    /// anything, and the field's `maxLength` still caps the result.
    #[test]
    fn paste_at_the_caret_respects_max_length() {
        clipboard_seam::set("xyz");
        let mut b = field().with_max_length(4);
        typed(&mut b, "ab");
        assert!(b.handle_key(KeyEvent::with_modifiers(KEY_V, EDIT_SHORTCUT_MODIFIER)));
        assert_eq!(b.value(), "abxy", "only two more chars fit the 4-char budget");
    }

    /// Paste is declined on a read-only field — nothing to insert into.
    #[test]
    fn paste_on_a_read_only_field_does_nothing() {
        clipboard_seam::set("xyz");
        let mut b = field();
        typed(&mut b, "ab");
        b.is_editable = false;
        assert!(b.handle_key(KeyEvent::with_modifiers(KEY_V, EDIT_SHORTCUT_MODIFIER)));
        assert_eq!(b.value(), "ab", "the keystroke is still consumed (`true`)");
    }

    #[test]
    fn inserting_into_a_full_selection_is_allowed_because_the_budget_adds_it_back() {
        // `maxLength - value.length() - (start - end)` with `start <= end`, so
        // the third term is *negative* and adds the selection length. A port that
        // reads it as a subtraction refuses to overtype a full field.
        let mut b = field().with_max_length(4);
        typed(&mut b, "abcd");
        assert_eq!(b.len(), 4, "premise: the field is full");
        assert!(!b.handle_char('z') || b.value() == "abcd");
        assert_eq!(b.value(), "abcd", "no room with nothing selected");
        // Select all four, then overtype.
        b.move_cursor_to(0, false);
        b.set_highlight_pos(4);
        b.insert_text("wxyz");
        assert_eq!(b.value(), "wxyz");
        // And a longer replacement is truncated to the freed budget.
        b.move_cursor_to(0, false);
        b.set_highlight_pos(4);
        b.insert_text("123456");
        assert_eq!(b.value(), "1234");
    }

    #[test]
    fn word_wise_motion_and_delete_follow_vanillas_space_walk() {
        let mut b = field();
        typed(&mut b, "one two  three");
        assert_eq!(b.cursor_position(), 14);
        // Backwards skips trailing spaces then the word.
        assert_eq!(b.word_position(-1), 9, "start of `three`");
        b.move_cursor_to(9, false);
        assert_eq!(b.word_position(-1), 4, "start of `two`");
        // Forwards lands *past* the run of spaces after the word.
        b.move_cursor_to(0, false);
        assert_eq!(b.word_position(1), 4, "past the space after `one`");
        b.move_cursor_to(4, false);
        assert_eq!(b.word_position(1), 9, "past *both* spaces after `two`");
        b.move_cursor_to(9, false);
        assert_eq!(b.word_position(1), 14, "no space left, so the end");
        // Ctrl/Cmd+Backspace deletes a whole word.
        b.move_cursor_to_end(false);
        let ctrl_back = KeyEvent::with_modifiers(KEY_BACKSPACE, EDIT_SHORTCUT_MODIFIER);
        assert!(ctrl_back.has_control_down_with_quirk());
        assert!(b.handle_key(ctrl_back));
        assert_eq!(b.value(), "one two  ");
        // With a selection live, the selection wins over the word
        // (`EditBox.java:172-176`).
        b.move_cursor_to(0, false);
        b.set_highlight_pos(3);
        assert!(b.handle_key(ctrl_back));
        assert_eq!(b.value(), " two  ", "the selection went, not a word");
    }

    #[test]
    fn display_pos_scrolls_to_keep_the_caret_inside_the_field() {
        // A narrow box: inner width 6*4 = 24 -> exactly 4 characters visible.
        let mut b = EditBox::new(0.0, 0.0, 24.0 + 2.0 * BORDER_INSET, 20.0, "narrow");
        b.widget.focused = true;
        b.set_max_length(64);
        assert_eq!(b.inner_width(), 24.0);
        typed(&mut b, "abcd");
        assert_eq!(b.display_position(), 0, "still fits");
        assert_eq!(b.displayed(), "abcd");
        typed(&mut b, "ef");
        assert_eq!(
            b.display_position(),
            2,
            "the window slid right to keep the caret visible"
        );
        assert_eq!(b.displayed(), "cdef");
        // Walking back to the start pulls it home again.
        for _ in 0..6 {
            b.handle_key(KeyEvent::new(KEY_LEFT));
        }
        assert_eq!(b.cursor_position(), 0);
        assert_eq!(b.display_position(), 0);
        assert_eq!(b.displayed(), "abcd");
        // Home/End are the same mechanism.
        b.handle_key(KeyEvent::new(KEY_END));
        assert_eq!(b.cursor_position(), 6);
        assert!(
            b.display_position() > 0,
            "End must scroll, or the caret is drawn outside the field"
        );
        assert!(
            b.displayed().chars().count() <= 4,
            "and never more than the window holds"
        );
    }

    #[test]
    fn the_draw_state_puts_the_caret_where_the_text_ends() {
        // The pixel claim, at the level this file can make it: the caret's x is
        // derived from the *measured* text before it, and it moves when the
        // caret moves. `super::render`'s gate asserts the glyphs land in the
        // widget's own rect.
        let mut b = field();
        let empty = b.draw_state(None);
        assert_eq!(empty.before_x, BORDER_INSET, "bordered text starts at +4");
        assert!(empty.show_cursor, "a focused empty field shows its caret");
        assert!(
            !empty.insert_cursor,
            "an empty field appends, so the caret is the underscore"
        );
        typed(&mut b, "abc");
        let at_end = b.draw_state(None);
        assert_eq!(at_end.before, "abc");
        assert_eq!(at_end.after, "");
        // 4 (inset) + 3 chars * 6 + 1 (vanilla's gap after the first half).
        assert_eq!(at_end.cursor_x, 4.0 + 18.0 + 1.0);
        assert!(at_end.highlight.is_none());

        b.move_cursor_to(1, false);
        let mid = b.draw_state(None);
        assert_eq!((mid.before.as_str(), mid.after.as_str()), ("a", "bc"));
        assert!(
            mid.insert_cursor,
            "a caret before the end is the 1 px bar, not the underscore"
        );
        // 4 + 1*6 + 1, then `cursorX--` for insert mode.
        assert_eq!(mid.cursor_x, 4.0 + 6.0 + 1.0 - 1.0);
        assert!(
            mid.cursor_x < at_end.cursor_x,
            "the caret moved left between two draws — the two positions #395 \
             asks for"
        );

        // A selection produces a rect, and it spans the selected glyphs.
        b.set_highlight_pos(3);
        let selected = b.draw_state(None);
        let (from, to) = selected.highlight.expect("a selection must draw");
        assert!(from < to, "got ({from}, {to})");
        assert!(
            (to - from - 2.0 * 6.0).abs() <= 2.0,
            "two selected characters is about two advances wide, got {}",
            to - from
        );
        // An unfocused field draws no caret at all.
        b.widget.focused = false;
        assert!(!b.draw_state(None).show_cursor);
    }

    #[test]
    fn text_y_truncates_like_javas_integer_division() {
        // `this.getY() + (this.height - 8) / 2` with int arithmetic
        // (`EditBox.java:491`). A 20-high box gives 6; a 19-high one gives 5,
        // not 5.5.
        assert_eq!(EditBox::new(0.0, 0.0, 100.0, 20.0, "a").text_y(), 6.0);
        assert_eq!(EditBox::new(0.0, 100.0, 100.0, 19.0, "a").text_y(), 105.0);
        // Unbordered puts the text at the top edge and reclaims the 8 px.
        let mut b = EditBox::new(0.0, 100.0, 100.0, 20.0, "a");
        b.bordered = false;
        assert_eq!(b.text_y(), 100.0);
        assert_eq!(b.text_x(), 0.0);
        assert_eq!(b.inner_width(), 100.0);
        b.bordered = true;
        assert_eq!(b.inner_width(), 92.0);
        assert_eq!(b.text_x(), BORDER_INSET);
    }

    #[test]
    fn a_click_lands_the_caret_on_the_clicked_character() {
        let mut b = field();
        typed(&mut b, "abcdef");
        // text_x is 4; each character is 6 wide. A click 2.5 characters in
        // lands after the second.
        b.click_at(4.0 + 15.0, false);
        assert_eq!(b.cursor_position(), 2);
        // Past the right edge clamps to the end of the *visible* text, not past
        // it (`findClickedPositionInText`'s `Math.min(.., getInnerWidth())`).
        b.click_at(10_000.0, false);
        assert_eq!(b.cursor_position(), 6);
        // Left of the text clamps to 0.
        b.click_at(0.0, false);
        assert_eq!(b.cursor_position(), 0);
        // And through the focus trait, a click both moves the caret and reports
        // itself consumed, which is what makes the container focus the box.
        assert!(FocusTarget::mouse_clicked(&mut b, 4.0 + 15.0, 10.0));
        assert_eq!(b.cursor_position(), 2);
        assert!(
            !FocusTarget::mouse_clicked(&mut b, 4.0, 500.0),
            "a click outside the rect is not consumed"
        );
    }

    #[test]
    fn can_lose_focus_false_ignores_being_unfocused() {
        let mut b = field();
        FocusTarget::set_focused(&mut b, false);
        assert!(!b.widget.focused);
        b.can_lose_focus = false;
        FocusTarget::set_focused(&mut b, true);
        assert!(b.widget.focused);
        FocusTarget::set_focused(&mut b, false);
        assert!(
            b.widget.focused,
            "`canLoseFocus == false` drops the unfocus entirely \
             (`EditBox.java:530`)"
        );
    }

    #[test]
    fn set_value_truncates_and_parks_the_caret_at_the_end() {
        let mut b = field().with_max_length(5);
        b.set_value("abcdefgh");
        assert_eq!(b.value(), "abcde");
        assert_eq!(b.cursor_position(), 5);
        assert_eq!(b.highlight_position(), 5, "and clears any selection");
        // `set_max_length` truncates in place without moving the caret, which is
        // vanilla's asymmetry (`:495-501` has no cursor call).
        b.set_max_length(2);
        assert_eq!(b.value(), "ab");
        assert!(
            b.cursor_position() <= b.len(),
            "but the caret must still be inside the value"
        );
    }

    #[test]
    fn the_layout_seam_and_the_focus_seam_agree_about_where_the_box_is() {
        let mut b = EditBox::new(0.0, 0.0, 200.0, 20.0, "Address");
        b.set_position(40.0, 128.0);
        // `rectangle()` is ambiguous on purpose: `LayoutElement`'s is the `f32`
        // tuple #394's containers arrange with, `FocusTarget`'s is the integer
        // `ScreenRectangle` navigation compares. Both must describe the same box,
        // and a call site has to say which it wants.
        assert_eq!(
            LayoutElement::rectangle(&b),
            (40.0, 128.0, 200.0, 20.0)
        );
        assert_eq!(
            FocusTarget::rectangle(&b),
            super::super::focus::ScreenRectangle::new(40, 128, 200, 20)
        );
        // Moving it moves the text with it — the reason `textX`/`textY` are
        // methods and not fields.
        assert_eq!(b.text_x(), 44.0);
        assert_eq!(b.text_y(), 134.0);
        // And `visit_widgets` yields the wrapped widget, so #394's containers
        // can arrange a field exactly like a button.
        let mut seen = 0;
        b.visit_widgets(&mut |w| {
            seen += 1;
            assert_eq!(w.rect(), (40.0, 128.0, 200.0, 20.0));
        });
        assert_eq!(seen, 1);
    }

    /// A synthetic proportional width function for the gates below: `i`/`l`
    /// are narrow, `W`/`M` are wide, everything else is a middling width —
    /// deliberately nothing like [`MENU_TEXT_ADVANCE`]'s flat `6.0`, so a
    /// test that happened to pass by picking an average-width string could
    /// not happen here. These numbers are what "correct" is computed against
    /// below; they are not claimed to be vanilla's real advances — those are
    /// covered separately by `hud::vanilla_font`'s own jar-gated pixel tests.
    fn synth_advance(ch: char) -> f32 {
        match ch {
            'i' | 'l' => 2.0,
            'W' | 'M' => 9.0,
            _ => 5.0,
        }
    }

    fn synth_measure(s: &str) -> f32 {
        s.chars().map(synth_advance).sum()
    }

    /// The exact cursor x `draw_state_with` must produce for a wide,
    /// never-scrolled [`field`], derived independently from the same
    /// `EditBox.java:404-473` arithmetic `draw_state_with` implements —
    /// `text_x + width(before) + 1.0`, minus 1 more when the caret is the
    /// insert bar rather than the append underscore — but evaluated here with
    /// a hand-computed width, not by calling the code under test.
    ///
    /// The one place this differs from `draw_state`'s own arithmetic: when
    /// the *value* is empty, vanilla adds no gap at all (`displayed.isEmpty()`
    /// short-circuits both the `+1` and the insert `-1`), so `before` being
    /// empty is not by itself enough to predict the "no gap" case — an empty
    /// value is.
    fn predicted_cursor_x(value: &str, prefix: usize, measure: impl Fn(&str) -> f32) -> f32 {
        if value.is_empty() {
            return BORDER_INSET;
        }
        let before: String = value.chars().take(prefix).collect();
        let insert_cursor = prefix < value.chars().count();
        BORDER_INSET + measure(&before) + 1.0 - if insert_cursor { 1.0 } else { 0.0 }
    }

    /// **The gate for the "cursor gap grows while typing" report.** Predicts
    /// the exact `cursor_x` from the correct (proportional) hypothesis and
    /// requires `draw_state_with` to land on it — not merely "closer than
    /// before" — at *every* prefix length of several adversarial strings, so
    /// an error that is zero only at the string's end (or only at its start)
    /// cannot pass.
    #[test]
    fn draw_state_with_tracks_a_proportional_font_not_the_fixed_advance() {
        let measure_fn: &dyn Fn(&str) -> f32 = &synth_measure;

        for s in ["iiiil", "WWWWM", "iWiWi", "", "i", "W"] {
            let mut b = field();
            typed(&mut b, s);
            let n = s.chars().count();
            for prefix in 0..=n {
                b.move_cursor_to(prefix, false);
                let state = b.draw_state_with(None, Some(measure_fn));
                let before: String = s.chars().take(prefix).collect();
                assert_eq!(
                    state.before, before,
                    "premise: `before` must be exactly the prefix at cursor {prefix}"
                );

                let correct = predicted_cursor_x(s, prefix, synth_measure);
                assert!(
                    (state.cursor_x - correct).abs() < 1e-4,
                    "string {s:?} prefix {prefix}: cursor_x {} != predicted proportional \
                     {correct}",
                    state.cursor_x
                );

                // The two hypotheses must actually differ here, or this
                // assertion could not have told a correct implementation from
                // a broken one — see the sibling negative-control test for
                // the case that must fail.
                let wrong_fixed = predicted_cursor_x(s, prefix, |t: &str| {
                    t.chars().count() as f32 * MENU_TEXT_ADVANCE
                });
                if prefix > 0 && !s.is_empty() {
                    assert!(
                        (correct - wrong_fixed).abs() > 1e-6,
                        "string {s:?} prefix {prefix}: the proportional and fixed-advance \
                         hypotheses coincide ({correct}), so this assertion is vacuous here"
                    );
                }
            }
        }
    }

    /// **Negative control, run rather than described.** `EditBox::draw_state`
    /// (no measurement seam — the box's own fixed [`MENU_TEXT_ADVANCE`]) is
    /// exactly what production called before this fix, for every render where
    /// a real proportional font was attached. This test proves that path
    /// really does diverge from the correct, proportional cursor position,
    /// and that the divergence **accumulates with every character** rather
    /// than being a one-off rounding difference — the "gap ... grows as I
    /// type" the player reported, reproduced numerically.
    #[test]
    fn the_fixed_advance_path_is_the_bug_and_the_error_grows_with_length() {
        let s = "iiiil"; // every character narrower than MENU_TEXT_ADVANCE (6.0)
        let mut b = field();
        typed(&mut b, s);
        let n = s.chars().count();

        let mut errors = Vec::new();
        for prefix in 0..=n {
            b.move_cursor_to(prefix, false);
            // The pre-fix call shape: no seam, so `draw_state` falls back to
            // this box's own fixed advance — reproducing the bug exactly.
            let naive = b.draw_state(None);
            let correct = predicted_cursor_x(s, prefix, synth_measure);
            errors.push(naive.cursor_x - correct);
        }

        assert_eq!(errors[0], 0.0, "premise: an empty prefix has nothing to diverge on");
        for (prefix, &err) in errors.iter().enumerate().skip(1) {
            assert!(
                err > 0.0,
                "the fixed-advance path must overshoot the correct cursor position by \
                 prefix {prefix} (every character in {s:?} is narrower than \
                 MENU_TEXT_ADVANCE); measured error {err}"
            );
        }
        // The accumulating property, asserted directly: each additional
        // character must widen the gap by exactly one character's worth of
        // (fixed advance - real advance) = 6.0 - 2.0 = 4.0, not merely "some
        // more" — a constant per-character error, growing linearly, is
        // precisely what turns "off by a pixel" into "unusable after a
        // sentence".
        for prefix in 1..=n {
            assert!(
                (errors[prefix] - errors[prefix - 1] - 4.0).abs() < 1e-4,
                "expected the per-character error to grow by exactly 4.0 px from prefix \
                 {} to {prefix}; got {} -> {} (delta {})",
                prefix - 1,
                errors[prefix - 1],
                errors[prefix],
                errors[prefix] - errors[prefix - 1]
            );
        }

        // And the seam this file adds is the fix: the same string, same
        // cursor positions, through `draw_state_with`, has zero error at
        // every prefix length — not just at the end.
        let measure_fn: &dyn Fn(&str) -> f32 = &synth_measure;
        for prefix in 0..=n {
            b.move_cursor_to(prefix, false);
            let fixed = b.draw_state_with(None, Some(measure_fn));
            let correct = predicted_cursor_x(s, prefix, synth_measure);
            assert!(
                (fixed.cursor_x - correct).abs() < 1e-4,
                "prefix {prefix}: draw_state_with must land exactly on {correct}, got {}",
                fixed.cursor_x
            );
        }
    }

    /// The highlight rect's far edge is measured the same way `cursor_x` is
    /// (`EditBox.java:404-473`'s `highlightX`), so a selection under a
    /// proportional font must widen by the *selected characters'* real
    /// widths, not by `count * MENU_TEXT_ADVANCE`.
    #[test]
    fn a_selection_under_a_proportional_font_spans_the_real_glyph_widths() {
        let measure_fn: &dyn Fn(&str) -> f32 = &synth_measure;
        let mut b = field();
        typed(&mut b, "iiWWi"); // 2 narrow, 2 wide, 1 narrow
        b.move_cursor_to(0, false);
        b.set_highlight_pos(4); // selects "iiWW": 2*2.0 + 2*9.0 = 22.0
        let state = b.draw_state_with(None, Some(measure_fn));
        let (from, to) = state.highlight.expect("a live selection must draw a rect");
        assert!((from - BORDER_INSET).abs() < 1e-4, "selection starts at the caret, here 0");
        let expected_to = BORDER_INSET + synth_measure("iiWW") - 1.0;
        assert!(
            (to - expected_to).abs() < 1e-4,
            "selection's far edge {to} != predicted {expected_to} (would be {} under the \
             fixed advance)",
            BORDER_INSET + 4.0 * MENU_TEXT_ADVANCE - 1.0
        );
    }
}
