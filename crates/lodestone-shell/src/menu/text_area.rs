//! A multi-line, word-wrapping text field — vanilla's `MultilineTextField`
//! (`client/gui/components/MultilineTextField.java`), the model
//! `MultiLineEditBox` wraps for `BookEditScreen`'s page editor.
//!
//! ## What it is
//!
//! Before this, every text-editing surface in this shell was
//! [`super::edit_box::EditBox`] — a **single** line. Issue #613's `EditBook`
//! remainder needs a book page: free-flowing text that wraps at a fixed
//! width, spans multiple visual lines, and still supports a caret, a
//! selection and Up/Down-by-visual-line the way a real text editor does.
//! [`TextArea`] is that widget, kept independent of [`super::menu::Screen`]
//! and [`super::render`] the same way `EditBox` is, so a sign editor or any
//! other text-heavy screen can reuse it later — see this module's own doc for
//! why the sign editor does not yet (four fixed lines, no wrapping, a
//! different shape entirely).
//!
//! ## Word-wrap is the fixed-advance approximation, not `Font.Splitter`
//!
//! Vanilla wraps by real glyph width (`Font.getSplitter().splitLines`). This
//! widget has no `Font` — the same "pure data, no renderer dependency" rule
//! [`super::edit_box::EditBox`]'s own module doc states and justifies at
//! length — so [`TextArea::wrap_chars`] is a **character count**, not a pixel
//! width, and wrapping is greedy word-wrap against that count. A `TextArea`
//! sized from [`super::edit_box::MENU_TEXT_ADVANCE`] wraps close to where a
//! real font would, and exactly the way an [`super::edit_box::EditBox`]'s own
//! horizontal scroll already approximates a proportional font — see that
//! module's doc for the shape of the same deviation and why it is accepted
//! here for the same reason: threading a `Font` through this layer would put
//! the renderer inside the input layer.
//!
//! ## Indices are `char`s, not UTF-16 code units
//!
//! Same convention as [`super::edit_box::EditBox`] — see that module's doc.
//! [`StringView`] positions are `char` indices into [`TextArea::value`].
//!
//! ## How to change it
//!
//! [`TextArea::handle_key`] is a direct transcription of
//! `MultilineTextField.keyPressed`'s `switch`, GLFW key code for GLFW key
//! code, so a diff against the jar stays legible. Enter (`257`) inserts a
//! literal `\n` rather than doing anything screen-level — a book page's Enter
//! key is a newline, unlike [`super::edit_box::EditBox`], which has no
//! concept of one. PageUp/PageDown (`266`/`267`, "jump to document
//! start/end") are the two vanilla cases this port omits: nothing in this
//! shell's [`super::focus::KeyEvent`] carries those GLFW codes yet (no
//! `KEY_PAGE_UP`/`KEY_PAGE_DOWN` constant exists), and no screen has asked for
//! them — add the constants to [`super::focus`] first if a caller ever needs
//! them, rather than inventing new key codes here.
//!
//! ## Dependencies
//!
//! [`super::focus::KeyEvent`] for the key-code constants and the shared
//! select-all/copy/cut/paste predicates; [`super::edit_box::clipboard_seam`]
//! is **not** reused directly (it is private to that module) — this widget
//! goes through the same production/test fork via its own thin wrapper below,
//! so a `cargo test` run here never touches the real OS clipboard either.

use super::focus::{KeyEvent, KEY_BACKSPACE, KEY_DELETE, KEY_DOWN, KEY_END, KEY_ENTER, KEY_HOME, KEY_LEFT, KEY_RIGHT, KEY_UP};

/// The GLFW code for the numpad Enter key (`KP_ENTER`), which
/// `MultilineTextField.keyPressed`'s `case 257: case 335:` treats identically
/// to the main Enter key.
pub const KEY_KP_ENTER: i32 = 335;

/// A half-open `[begin, end)` span of **`char` indices** into a [`TextArea`]'s
/// value — one visual (wrapped) line, or a selection/word span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StringView {
    /// First `char` index in the span.
    pub begin: usize,
    /// One past the last `char` index in the span.
    pub end: usize,
}

impl StringView {
    const EMPTY: Self = Self { begin: 0, end: 0 };

    /// The span's length in `char`s.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.end - self.begin
    }

    /// Whether the span is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.begin == self.end
    }
}

/// `MultilineTextField.Whence` — how [`TextArea::seek_cursor`] interprets its
/// offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whence {
    /// An absolute `char` index, clamped to the value's length.
    Absolute,
    /// An offset from the current cursor.
    Relative,
    /// An offset from the end of the value.
    End,
}

/// The clipboard seam, forked at compile time exactly the way
/// [`super::edit_box`]'s own (private) `clipboard_seam` is — see that
/// module's doc for why: production reads/writes the real OS clipboard,
/// every `#[cfg(test)]` build routes through an in-memory stand-in so no test
/// run anywhere in this crate touches the developer's real clipboard.
#[cfg(not(test))]
mod clipboard_seam {
    pub fn get() -> String {
        crate::platform::clipboard::get()
    }

    pub fn set(text: &str) {
        crate::platform::clipboard::set(text);
    }
}

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

/// A multi-line, word-wrapping text field. See the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct TextArea {
    value: String,
    cursor: usize,
    select_cursor: usize,
    character_limit: Option<usize>,
    line_limit: Option<usize>,
    /// Wrap width, in `char`s — see the module doc on why this is a count
    /// rather than a pixel width.
    wrap_chars: usize,
    display_lines: Vec<StringView>,
}

impl TextArea {
    /// A field wrapping at `wrap_chars` characters per visual line, with no
    /// character or line limit.
    #[must_use]
    pub fn new(wrap_chars: usize) -> Self {
        let mut field = Self {
            value: String::new(),
            cursor: 0,
            select_cursor: 0,
            character_limit: None,
            line_limit: None,
            wrap_chars: wrap_chars.max(1),
            display_lines: Vec::new(),
        };
        field.reflow();
        field
    }

    /// `setCharacterLimit`.
    pub fn set_character_limit(&mut self, limit: Option<usize>) {
        self.character_limit = limit;
    }

    /// [`set_character_limit`](Self::set_character_limit) as a builder step.
    #[must_use]
    pub fn with_character_limit(mut self, limit: usize) -> Self {
        self.set_character_limit(Some(limit));
        self
    }

    /// `setLineLimit`.
    pub fn set_line_limit(&mut self, limit: Option<usize>) {
        self.line_limit = limit;
    }

    /// [`set_line_limit`](Self::set_line_limit) as a builder step.
    #[must_use]
    pub fn with_line_limit(mut self, limit: usize) -> Self {
        self.set_line_limit(Some(limit));
        self
    }

    /// `value()`.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// The value's length in `char`s.
    #[must_use]
    pub fn len(&self) -> usize {
        self.value.chars().count()
    }

    /// Whether the value is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// `cursor()`.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// `setValue(value, allowOverflowLineLimit)`: truncates to the character
    /// limit, then applies unless it would overflow the line limit (unless
    /// `allow_overflow_line_limit`). Returns whether the value was applied —
    /// vanilla's own method returns `void` and simply no-ops, but a caller
    /// here (the page-turn button) needs to know whether to advance.
    pub fn set_value(&mut self, value: impl AsRef<str>, allow_overflow_line_limit: bool) -> bool {
        let truncated = self.truncate_full_text(value.as_ref());
        if allow_overflow_line_limit || !self.overflows_line_limit(&truncated) {
            self.value = truncated;
            self.cursor = self.len();
            self.select_cursor = self.cursor;
            self.reflow();
            true
        } else {
            false
        }
    }

    fn byte_of(&self, i: usize) -> usize {
        self.value
            .char_indices()
            .nth(i)
            .map_or(self.value.len(), |(b, _)| b)
    }

    fn truncate_full_text(&self, input: &str) -> String {
        match self.character_limit {
            Some(limit) => input.chars().take(limit).collect(),
            None => input.to_owned(),
        }
    }

    fn truncate_insertion_text(&self, input: &str) -> String {
        match self.character_limit {
            Some(limit) => {
                let remaining = limit.saturating_sub(self.len());
                input.chars().take(remaining).collect()
            }
            None => input.to_owned(),
        }
    }

    fn overflows_line_limit(&self, new_value: &str) -> bool {
        match self.line_limit {
            Some(limit) => wrapped_line_count(new_value, self.wrap_chars) > limit,
            None => false,
        }
    }

    /// `hasSelection()`.
    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.select_cursor != self.cursor
    }

    /// `getSelected()`.
    #[must_use]
    pub fn selected(&self) -> StringView {
        StringView {
            begin: self.select_cursor.min(self.cursor),
            end: self.select_cursor.max(self.cursor),
        }
    }

    /// `getSelectedText()`.
    #[must_use]
    pub fn selected_text(&self) -> String {
        let sel = self.selected();
        self.value.chars().skip(sel.begin).take(sel.len()).collect()
    }

    /// `insertText(String)`: replace the selection (or insert at the caret)
    /// with `input`, filtered through
    /// [`super::edit_box::is_allowed_chat_character`] and truncated to
    /// whatever the character limit still allows. Vanilla's own filter
    /// (`StringUtil.filterText(input, true)`) passes `allowNewlines = true`,
    /// unlike [`super::edit_box::filter_text`]'s single-line callers — a
    /// pasted multi-line block keeps its newlines here.
    pub fn insert_text(&mut self, input: &str) {
        if input.is_empty() && !self.has_selection() {
            return;
        }
        let filtered: String = input
            .chars()
            .filter(|&c| c == '\n' || super::edit_box::is_allowed_chat_character(c))
            .collect();
        let text = self.truncate_insertion_text(&filtered);
        let sel = self.selected();
        let (sb, eb) = (self.byte_of(sel.begin), self.byte_of(sel.end));
        let mut new_value = self.value.clone();
        new_value.replace_range(sb..eb, &text);
        if !self.overflows_line_limit(&new_value) {
            self.value = new_value;
            self.cursor = sel.begin + text.chars().count();
            self.select_cursor = self.cursor;
            self.reflow();
        }
    }

    /// `deleteText(int)`.
    pub fn delete_text(&mut self, dir: i32) {
        if !self.has_selection() {
            self.select_cursor = self.offset_cursor(self.cursor, dir);
        }
        self.insert_text("");
    }

    fn offset_cursor(&self, from: usize, dir: i32) -> usize {
        if dir >= 0 {
            from.saturating_add(dir.unsigned_abs() as usize).min(self.len())
        } else {
            from.saturating_sub(dir.unsigned_abs() as usize)
        }
    }

    /// `seekCursor(Whence, int)`.
    pub fn seek_cursor(&mut self, whence: Whence, offset: i32, extend_selection: bool) {
        let target = match whence {
            Whence::Absolute => offset.max(0) as usize,
            Whence::Relative => self.offset_cursor(self.cursor, offset),
            Whence::End => self.offset_cursor(self.len(), offset),
        };
        self.cursor = target.min(self.len());
        if !extend_selection {
            self.select_cursor = self.cursor;
        }
    }

    /// `getLineCount()`.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.display_lines.len()
    }

    /// `iterateLines()`.
    #[must_use]
    pub fn lines(&self) -> &[StringView] {
        &self.display_lines
    }

    /// `getLineAtCursor()`. `-1` (no containing line, which should not
    /// happen after a `reflow`) reads as the last line, matching vanilla's
    /// own logged-error fallback (`getCursorLineView`).
    #[must_use]
    pub fn line_at_cursor(&self) -> usize {
        self.display_lines
            .iter()
            .position(|v| self.cursor >= v.begin && self.cursor <= v.end)
            .unwrap_or_else(|| self.display_lines.len().saturating_sub(1))
    }

    /// `getLineView(int)`, clamped.
    #[must_use]
    pub fn line_view(&self, index: usize) -> StringView {
        let clamped = index.min(self.display_lines.len().saturating_sub(1));
        self.display_lines.get(clamped).copied().unwrap_or(StringView::EMPTY)
    }

    /// `seekCursorLine(int)`: move the caret up/down one *visual* line,
    /// preserving its column as best the fixed-advance approximation allows
    /// (a straight column index rather than vanilla's pixel-position
    /// re-measure, since there is no [`super::edit_box::EditBox::measure`]
    /// font behind this widget either).
    pub fn seek_cursor_line(&mut self, line_offset: i32, extend_selection: bool) {
        if line_offset == 0 {
            return;
        }
        let current = self.line_at_cursor();
        let cur_view = self.line_view(current);
        let column = self.cursor.saturating_sub(cur_view.begin);
        let target_index = (current as i64 + i64::from(line_offset))
            .clamp(0, self.display_lines.len().saturating_sub(1) as i64) as usize;
        let target_view = self.line_view(target_index);
        let new_cursor = (target_view.begin + column).min(target_view.end);
        self.seek_cursor(Whence::Absolute, new_cursor as i32, extend_selection);
    }

    fn is_word_char(c: char) -> bool {
        !c.is_whitespace()
    }

    /// `getPreviousWord()`.
    #[must_use]
    pub fn previous_word(&self) -> StringView {
        if self.value.is_empty() {
            return StringView::EMPTY;
        }
        let chars: Vec<char> = self.value.chars().collect();
        let mut start = self.cursor.min(chars.len().saturating_sub(1));
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && Self::is_word_char(chars[start - 1]) {
            start -= 1;
        }
        StringView { begin: start, end: word_end(&chars, start) }
    }

    /// `getNextWord()`.
    #[must_use]
    pub fn next_word(&self) -> StringView {
        if self.value.is_empty() {
            return StringView::EMPTY;
        }
        let chars: Vec<char> = self.value.chars().collect();
        let mut start = self.cursor.min(chars.len().saturating_sub(1));
        while start < chars.len() && Self::is_word_char(chars[start]) {
            start += 1;
        }
        while start < chars.len() && chars[start].is_whitespace() {
            start += 1;
        }
        StringView { begin: start, end: word_end(&chars, start) }
    }

    fn reflow(&mut self) {
        self.display_lines.clear();
        if self.value.is_empty() {
            self.display_lines.push(StringView::EMPTY);
            return;
        }
        let chars: Vec<char> = self.value.chars().collect();
        let mut para_start = 0usize;
        for (i, &c) in chars.iter().enumerate() {
            if c == '\n' {
                wrap_paragraph(&chars, para_start, i, self.wrap_chars, &mut self.display_lines);
                para_start = i + 1;
            }
        }
        wrap_paragraph(&chars, para_start, chars.len(), self.wrap_chars, &mut self.display_lines);
        if chars.last() == Some(&'\n') {
            self.display_lines.push(StringView { begin: chars.len(), end: chars.len() });
        }
    }

    /// `MultilineTextField.keyPressed`. `extend_selection` is Shift, threaded
    /// in rather than read off `event` a second time, matching
    /// [`super::edit_box::EditBox::handle_key`]'s own convention.
    pub fn handle_key(&mut self, event: KeyEvent) -> bool {
        let extend = event.has_shift_down();
        if event.is_select_all() {
            self.cursor = self.len();
            self.select_cursor = 0;
            return true;
        }
        if event.is_copy() {
            clipboard_seam::set(&self.selected_text());
            return true;
        }
        if event.is_paste() {
            let text = clipboard_seam::get();
            self.insert_text(&text);
            return true;
        }
        if event.is_cut() {
            clipboard_seam::set(&self.selected_text());
            self.insert_text("");
            return true;
        }
        match event.key {
            KEY_ENTER | KEY_KP_ENTER => {
                self.insert_text("\n");
                true
            }
            KEY_BACKSPACE => {
                if event.has_control_down_with_quirk() {
                    let word = self.previous_word();
                    let dir = i32::try_from(word.begin).unwrap_or(0) - i32::try_from(self.cursor).unwrap_or(0);
                    self.delete_text(dir);
                } else {
                    self.delete_text(-1);
                }
                true
            }
            KEY_DELETE => {
                if event.has_control_down_with_quirk() {
                    let word = self.next_word();
                    let dir = i32::try_from(word.begin).unwrap_or(0) - i32::try_from(self.cursor).unwrap_or(0);
                    self.delete_text(dir);
                } else {
                    self.delete_text(1);
                }
                true
            }
            KEY_RIGHT => {
                if event.has_control_down_with_quirk() {
                    let word = self.next_word();
                    self.seek_cursor(Whence::Absolute, word.begin as i32, extend);
                } else {
                    self.seek_cursor(Whence::Relative, 1, extend);
                }
                true
            }
            KEY_LEFT => {
                if event.has_control_down_with_quirk() {
                    let word = self.previous_word();
                    self.seek_cursor(Whence::Absolute, word.begin as i32, extend);
                } else {
                    self.seek_cursor(Whence::Relative, -1, extend);
                }
                true
            }
            KEY_DOWN => {
                if !event.has_control_down_with_quirk() {
                    self.seek_cursor_line(1, extend);
                }
                true
            }
            KEY_UP => {
                if !event.has_control_down_with_quirk() {
                    self.seek_cursor_line(-1, extend);
                }
                true
            }
            KEY_HOME => {
                if event.has_control_down_with_quirk() {
                    self.seek_cursor(Whence::Absolute, 0, extend);
                } else {
                    let begin = self.line_view(self.line_at_cursor()).begin;
                    self.seek_cursor(Whence::Absolute, begin as i32, extend);
                }
                true
            }
            KEY_END => {
                if event.has_control_down_with_quirk() {
                    self.seek_cursor(Whence::End, 0, extend);
                } else {
                    let end = self.line_view(self.line_at_cursor()).end;
                    self.seek_cursor(Whence::Absolute, end as i32, extend);
                }
                true
            }
            _ => false,
        }
    }

    /// `charTyped`, matching [`super::edit_box::EditBox::handle_char`]'s
    /// shape.
    pub fn handle_char(&mut self, ch: char) -> bool {
        if super::edit_box::is_allowed_chat_character(ch) {
            self.insert_text(&ch.to_string());
            true
        } else {
            false
        }
    }
}

fn word_end(chars: &[char], from: usize) -> usize {
    let mut end = from;
    while end < chars.len() && !chars[end].is_whitespace() {
        end += 1;
    }
    end
}

/// Greedy word-wrap of `chars[start..end]` (one explicit `\n`-delimited
/// paragraph) into `out`, breaking at the last space at or before `wrap`
/// characters, or hard-breaking a single word wider than `wrap`.
fn wrap_paragraph(chars: &[char], start: usize, end: usize, wrap: usize, out: &mut Vec<StringView>) {
    if start == end {
        out.push(StringView { begin: start, end });
        return;
    }
    let wrap = wrap.max(1);
    let mut line_start = start;
    let mut i = start;
    let mut last_space: Option<usize> = None;
    loop {
        if i >= end {
            out.push(StringView { begin: line_start, end });
            return;
        }
        if chars[i] == ' ' {
            last_space = Some(i);
        }
        if i - line_start + 1 > wrap {
            match last_space {
                Some(sp) if sp >= line_start => {
                    out.push(StringView { begin: line_start, end: sp });
                    line_start = sp + 1;
                    last_space = None;
                    i = line_start;
                }
                _ => {
                    out.push(StringView { begin: line_start, end: i });
                    line_start = i;
                    last_space = None;
                }
            }
            continue;
        }
        i += 1;
    }
}

/// The line count [`wrap_paragraph`] would produce for `value`, used by
/// [`TextArea::overflows_line_limit`] without mutating `self`.
fn wrapped_line_count(value: &str, wrap: usize) -> usize {
    let chars: Vec<char> = value.chars().collect();
    let mut lines = Vec::new();
    let mut para_start = 0usize;
    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            wrap_paragraph(&chars, para_start, i, wrap, &mut lines);
            para_start = i + 1;
        }
    }
    wrap_paragraph(&chars, para_start, chars.len(), wrap, &mut lines);
    if chars.last() == Some(&'\n') {
        lines.push(StringView::EMPTY);
    }
    lines.len()
}

#[cfg(test)]
mod tests {
    use super::super::focus::{
        KeyEvent, EDIT_SHORTCUT_MODIFIER, KEY_A, KEY_BACKSPACE, KEY_C, KEY_DELETE, KEY_DOWN,
        KEY_ENTER, KEY_RIGHT, KEY_UP, KEY_V, MOD_SHIFT,
    };
    use super::*;

    fn typed(t: &mut TextArea, s: &str) {
        for ch in s.chars() {
            assert!(t.handle_char(ch), "`{ch}` must be accepted");
        }
    }

    #[test]
    fn short_text_does_not_wrap() {
        let mut t = TextArea::new(40);
        typed(&mut t, "hello world");
        assert_eq!(t.line_count(), 1);
        assert_eq!(t.value(), "hello world");
    }

    #[test]
    fn long_text_wraps_at_a_word_boundary() {
        let mut t = TextArea::new(10);
        typed(&mut t, "the quick brown fox jumps");
        assert!(t.line_count() > 1, "must wrap into multiple visual lines");
        // No visual line should exceed the wrap width.
        for line in t.lines() {
            assert!(
                line.len() <= 10 || !t.value()[..].contains(' '),
                "a line with a space in it must not exceed the wrap width: {:?}",
                line
            );
        }
        // Reassembling every line (skipping the break, adding it back as a
        // space where one was consumed) must recover words in order —
        // spot-checked by first/last word rather than exact byte layout.
        assert!(t.value().starts_with("the"));
        assert!(t.value().ends_with("jumps"));
    }

    #[test]
    fn a_single_word_wider_than_the_wrap_hard_breaks() {
        let mut t = TextArea::new(4);
        typed(&mut t, "supercalifragilistic");
        assert!(t.line_count() >= 5, "a 21-char word at wrap=4 must hard-break");
    }

    #[test]
    fn newline_starts_a_new_paragraph() {
        let mut t = TextArea::new(40);
        typed(&mut t, "line one");
        assert!(t.handle_key(KeyEvent::new(KEY_ENTER)));
        typed(&mut t, "line two");
        assert_eq!(t.value(), "line one\nline two");
        assert_eq!(t.line_count(), 2);
    }

    #[test]
    fn character_limit_truncates_insertion() {
        let mut t = TextArea::new(100).with_character_limit(5);
        typed(&mut t, "hello world");
        assert_eq!(t.value(), "hello");
    }

    #[test]
    fn line_limit_rejects_an_overflowing_insert() {
        // wrap=5, line_limit=1: "hello world" would wrap onto two lines, so
        // the character that overflows must be refused, not silently
        // truncated — matching `overflowsLineLimit`'s all-or-nothing gate.
        let mut t = TextArea::new(5).with_line_limit(1);
        typed(&mut t, "hello");
        assert_eq!(t.value(), "hello");
        // `handle_char` returns whether the character passed the *filter*,
        // matching vanilla's own `EditBox.charTyped` — a length-rejected
        // insert is a silent no-op there too (`insertText`'s budget check).
        // The observable rejection is the value staying put.
        t.handle_char(' ');
        assert_eq!(t.value(), "hello", "an insert that would overflow the line limit must be refused");
    }

    #[test]
    fn backspace_deletes_one_char_and_delete_removes_the_next() {
        let mut t = TextArea::new(40);
        typed(&mut t, "abc");
        assert!(t.handle_key(KeyEvent::new(KEY_BACKSPACE)));
        assert_eq!(t.value(), "ab");
        t.seek_cursor(Whence::Absolute, 0, false);
        assert!(t.handle_key(KeyEvent::new(KEY_DELETE)));
        assert_eq!(t.value(), "b");
    }

    #[test]
    fn up_and_down_move_between_visual_lines_by_column() {
        let mut t = TextArea::new(5);
        typed(&mut t, "abcde fghij");
        // Wraps to ["abcde", "fghij"]; put the cursor at column 2 of line 2.
        t.seek_cursor(Whence::Absolute, 8, false);
        assert_eq!(t.line_at_cursor(), 1);
        assert!(t.handle_key(KeyEvent::new(KEY_UP)));
        assert_eq!(t.line_at_cursor(), 0);
        assert_eq!(t.cursor(), 2);
        assert!(t.handle_key(KeyEvent::new(KEY_DOWN)));
        assert_eq!(t.line_at_cursor(), 1);
        assert_eq!(t.cursor(), 8);
    }

    #[test]
    fn shift_extends_a_selection_rather_than_moving_the_caret_alone() {
        let mut t = TextArea::new(40);
        typed(&mut t, "abcdef");
        t.seek_cursor(Whence::Absolute, 0, false);
        // Move right three times with Shift held, extending the selection.
        for _ in 0..3 {
            t.handle_key(KeyEvent::with_modifiers(KEY_RIGHT, MOD_SHIFT));
        }
        assert!(t.has_selection());
        assert_eq!(t.selected_text(), "abc");
    }

    #[test]
    fn set_value_resets_cursor_to_the_end() {
        let mut t = TextArea::new(40);
        t.set_value("hello", false);
        assert_eq!(t.cursor(), 5);
        assert_eq!(t.value(), "hello");
    }

    #[test]
    fn clipboard_round_trips_through_the_test_seam_only() {
        let mut t = TextArea::new(40);
        typed(&mut t, "secret");
        t.seek_cursor(Whence::Absolute, 0, true);
        // Select the whole value, copy it, clear, then paste it back.
        assert!(t.handle_key(KeyEvent::with_modifiers(KEY_A, EDIT_SHORTCUT_MODIFIER)));
        assert!(t.handle_key(KeyEvent::with_modifiers(KEY_C, EDIT_SHORTCUT_MODIFIER)));
        assert_eq!(clipboard_seam::get(), "secret");
        t.insert_text("");
        assert_eq!(t.value(), "");
        assert!(t.handle_key(KeyEvent::with_modifiers(KEY_V, EDIT_SHORTCUT_MODIFIER)));
        assert_eq!(t.value(), "secret");
    }
}
