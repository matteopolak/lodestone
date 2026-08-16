//! The book-and-quill editing screen: vanilla's `BookEditScreen`/
//! `BookSignScreen` (`.cache/mc/26.2/client-src/net/minecraft/client/gui/
//! screens/inventory/{BookEditScreen,BookSignScreen}.java`) — issue #613's
//! `EditBook` remainder. See `docs/book-editing.md` for the server-side half
//! (`ServerboundEditBookPacket`'s slot addressing) that already existed
//! before this module; this is the client-side producer that packet had none
//! of.
//!
//! ## What it is
//!
//! One [`BookEditState`], covering both of vanilla's two screens with a
//! `signing` flag rather than two separate [`super::Screen`] variants —
//! `BookSignScreen` in vanilla is reached from exactly one place
//! (`BookEditScreen`'s Sign button) and returns to exactly one place (its own
//! Cancel button), so the pair is really one flow with two layouts, and
//! folding them into one state avoids duplicating the slot/pages/hand fields
//! every producer of [`lodestone_model::ClientAction::EditBook`] needs
//! either way.
//!
//! ## The page editor is [`super::text_area::TextArea`], the title is
//! [`super::edit_box::EditBox`]
//!
//! Vanilla's `BookEditScreen.page` is a `MultiLineEditBox` (word-wrapping,
//! multi-line); `BookSignScreen.titleBox` is a plain single-line `EditBox`.
//! This module reaches for the matching widget on each side rather than
//! inventing a book-specific one — see [`super::text_area`]'s own module doc
//! for why *it* had to be built (nothing multi-line existed anywhere in this
//! shell before it) and why it approximates word-wrap with a fixed
//! character-count width instead of `Font.Splitter`.
//!
//! ## What is deliberately simplified, named rather than hidden
//!
//! - **No per-pixel mouse caret placement inside the page.** Every other
//!   text screen in this shell that supports it (`ServerEdit`'s two fields,
//!   `SignEdit`'s four lines) does so through [`super::focus::FocusTarget`]
//!   and the generic click-to-`x`-coordinate machinery; wiring a *second*
//!   widget type into that generic system was judged not worth the surface
//!   area for a first pass. A click inside the page area is a no-op here
//!   (keyboard focus already always reaches the page while
//!   [`BookEditState::signing`] is `false` — there is nothing else on this
//!   screen to compete for it), and Left/Right/Home/End caret motion is not
//!   yet wired for the same reason `sign_edit`'s own module doc names: no
//!   [`super::focus::KeyEvent`] is produced for those GLFW codes from
//!   [`super::nav::MenuKey`] yet. Up/Down (line-to-line) and typing/
//!   Backspace/Delete/select-all/copy/cut/paste all work today.
//! - **No pseudo-3D book mesh.** Same simplification `sign_edit`'s own
//!   module doc names for signs: the page text draws as plain 2D labels, not
//!   `BookViewScreen`'s curved-page render.
//! - **Only `minecraft:writable_book` opens this screen.** A signed
//!   `minecraft:written_book` opens vanilla's **read-only** `BookViewScreen`
//!   instead, which sends nothing on the wire at all — out of scope for
//!   issue #613's `EditBook` producer, which is specifically about the
//!   *editable* book.
//!
//! ## `saveChanges`'s slot addressing
//!
//! `hand == MAIN_HAND ? owner.getInventory().getSelectedSlot() : 40` — the
//! **inventory** index (hotbar `0..=8`, not a container-native `36..=44`),
//! matching `docs/book-editing.md`'s own note that the server reads this
//! slot directly off `PlayerInventory`, not off a decoded `ItemStack`.
//! [`BookEditOpen::slot`] is already in that shape; this module never
//! recomputes it.
//!
//! ## Dependencies
//!
//! [`super::text_area::TextArea`] for the page; [`super::edit_box::EditBox`]
//! for the title; `lodestone_model::ClientAction` for the outbound packet.

use lodestone_model::ClientAction;

use super::edit_box::EditBox;
use super::focus::KeyEvent;
use super::text_area::TextArea;

/// `WritableBookContent`'s own cap (`WritableBookContent.PAGE_EDIT_LENGTH`
/// is per-page; this is the *page count* cap, `BookEditScreen.
/// appendPageToBook`'s bare `100`).
pub const MAX_PAGES: usize = 100;
/// `MultilineTextField`'s character-limit argument in `BookEditScreen.init`
/// (`this.page.setCharacterLimit(1024)`).
pub const PAGE_CHAR_LIMIT: usize = 1024;
/// `BookEditScreen.init`'s `this.page.setLineLimit(126 / 9)` — vanilla's
/// `TEXT_HEIGHT / lineHeight`.
pub const PAGE_LINE_LIMIT: usize = 126 / 9;
/// The page text area's word-wrap width, in `char`s — see
/// [`super::text_area`]'s module doc on why this is a character count rather
/// than vanilla's real `TEXT_WIDTH = 114` px. Chosen as `114 /
/// `[`super::edit_box::MENU_TEXT_ADVANCE`]`` so a wrap at this shell's own
/// fixed advance lands close to where vanilla's real font would.
pub const PAGE_WRAP_CHARS: usize = 19;
/// `BookSignScreen.titleBox`'s `setMaxLength(15)`.
pub const TITLE_MAX_LENGTH: usize = 15;

/// What opening this screen needs: which inventory slot to save back to
/// (already in `saveChanges`'s shape — see the module doc), the book's
/// current draft pages (`WritableBookContent.getPages`, or a single empty
/// page for a freshly crafted book — `BookEditScreen`'s own
/// `if (this.pages.isEmpty()) { this.pages.add(""); }`), and the signing
/// player's plain-text name for the "by `<name>`" line signing shows.
#[derive(Debug, Clone, PartialEq)]
pub struct BookEditOpen {
    /// `ServerboundEditBookPacket.slot` — see the module doc.
    pub slot: i32,
    /// The book's current pages, in order. Never empty by the time this
    /// reaches [`BookEditState::new`] — see [`BookEditState::new`]'s own
    /// doc for where the single-empty-page fallback lives.
    pub pages: Vec<String>,
    /// The local player's plain-text display name, for [`BookEditState::
    /// author_line`].
    pub author: String,
}

/// The book-editing screen's live widget state. See the module doc for why
/// this covers both of vanilla's two screens.
#[derive(Debug, Clone, PartialEq)]
pub struct BookEditState {
    /// See [`BookEditOpen::slot`].
    pub slot: i32,
    /// Every page's text, kept in sync with [`Self::page`] by
    /// [`Self::sync_current_page`] before anything reads it.
    pages: Vec<String>,
    /// Index into [`Self::pages`] the player is currently viewing/editing.
    current_page: usize,
    /// The current page's live text-area widget — `BookEditScreen.page`.
    pub page: TextArea,
    /// `true` while [`Screen::BookSign`]'s layout (title entry) is showing
    /// instead of the page editor — see the module doc on why this is a flag
    /// rather than a second screen.
    pub signing: bool,
    /// `BookSignScreen.titleBox`. Built once, up front, rather than only on
    /// first entering signing mode: vanilla's own `BookSignScreen` instance
    /// (and therefore its `titleBox`'s contents) is constructed once in
    /// `BookEditScreen`'s constructor and persists across Sign/Cancel
    /// round-trips within one edit session, which this mirrors by simply
    /// never resetting the field.
    pub title: EditBox,
    /// `BookSignScreen`'s `Component.translatable("book.byAuthor", owner.
    /// getName())` — kept as the plain player name rather than the
    /// pre-formatted string, so [`Self::author_line`] can format it once
    /// where every consumer already expects a plain string (`MenuLabel`,
    /// matching every other screen in this shell — see `sign_edit`'s own
    /// `TITLE_TEXT` for the convention).
    author: String,
    /// The row currently hovered, for the generic row-highlight draw —
    /// `super::nav::MenuNav::hover`'s convention for every screen with no
    /// keyboard row cursor (`CommandBlockEdit`, `SignEdit`).
    pub hovered: Option<usize>,
}

impl BookEditState {
    /// Builds fresh state for [`super::nav::MenuNav::open_book_edit`].
    /// `open.pages` is used verbatim if non-empty; an empty list (a freshly
    /// crafted, never-edited `minecraft:writable_book`, which carries no
    /// `writable_book_content` component at all) becomes a single empty
    /// page — `BookEditScreen`'s own fallback, quoted in the module doc.
    #[must_use]
    pub fn new(open: BookEditOpen) -> Self {
        let pages = if open.pages.is_empty() {
            vec![String::new()]
        } else {
            open.pages
        };
        let mut page = TextArea::new(PAGE_WRAP_CHARS)
            .with_character_limit(PAGE_CHAR_LIMIT)
            .with_line_limit(PAGE_LINE_LIMIT);
        page.set_value(&pages[0], true);
        // `setInitialFocus(this.titleBox)` (`BookSignScreen.java`): the title
        // box is the screen's only field, so it holds focus for its whole
        // life — see [`Self::title`]'s own doc on why this state does not
        // route focus through the generic multi-widget system.
        let mut title = EditBox::new(0.0, 0.0, 114.0, 20.0, "Book title").with_max_length(TITLE_MAX_LENGTH);
        title.widget.focused = true;
        title.centered = true;
        Self {
            slot: open.slot,
            pages,
            current_page: 0,
            page,
            signing: false,
            title,
            author: open.author,
            hovered: None,
        }
    }

    /// The 1-based page indicator vanilla's `book.pageIndicator` shows —
    /// `(current, total)`.
    #[must_use]
    pub fn page_indicator(&self) -> (usize, usize) {
        (self.current_page + 1, self.pages.len())
    }

    /// `BookSignScreen`'s `"book.byAuthor"` line, pre-formatted.
    #[must_use]
    pub fn author_line(&self) -> String {
        format!("by {}", self.author)
    }

    /// Writes [`Self::page`]'s live value back into [`Self::pages`] at
    /// [`Self::current_page`] — `MultilineTextField`'s
    /// `setValueListener(value -> this.pages.set(this.currentPage, value))`,
    /// called explicitly here (this shell has no value-changed callback
    /// hook) rather than on every keystroke.
    fn sync_current_page(&mut self) {
        if let Some(slot) = self.pages.get_mut(self.current_page) {
            *slot = self.page.value().to_owned();
        }
    }

    fn reload_page(&mut self) {
        let text = self.pages[self.current_page].clone();
        self.page.set_value(&text, true);
    }

    /// `pageBack()`.
    pub fn page_back(&mut self) {
        if self.current_page > 0 {
            self.sync_current_page();
            self.current_page -= 1;
            self.reload_page();
        }
    }

    /// `pageForward()`: advance, appending a fresh page (up to
    /// [`MAX_PAGES`]) when already on the last one.
    pub fn page_forward(&mut self) {
        self.sync_current_page();
        if self.current_page + 1 >= self.pages.len() {
            if self.pages.len() < MAX_PAGES {
                self.pages.push(String::new());
            } else {
                return;
            }
        }
        self.current_page += 1;
        self.reload_page();
    }

    /// The Sign button: `this.minecraft.gui.setScreen(this.signScreen)`.
    /// Syncs the current page first, the same way [`Self::to_save_action`]
    /// does, so a page mid-edit is not lost switching layouts.
    pub fn begin_sign(&mut self) {
        self.sync_current_page();
        self.signing = true;
    }

    /// The sign screen's Cancel button: back to the page editor, discarding
    /// nothing (`titleValue` persists — see [`Self::title`]'s own doc).
    pub fn cancel_sign(&mut self) {
        self.signing = false;
    }

    /// Whether the Finalize button is active — `titleBox.setResponder(value
    /// -> finalizeButton.active = !StringUtil.isBlank(value))`.
    #[must_use]
    pub fn can_finalize(&self) -> bool {
        !self.title.value().trim().is_empty()
    }

    /// `eraseEmptyTrailingPages()`: pop every empty page off the end.
    /// Vanilla's own `ListIterator`-based loop can empty the list to zero
    /// pages if every page is blank; this is reproduced rather than
    /// guarded, since the server is authoritative over the result either way.
    fn erase_empty_trailing_pages(&mut self) {
        while self.pages.last().is_some_and(String::is_empty) {
            self.pages.pop();
        }
    }

    /// `saveChanges()` with no title — the page editor's Done button.
    /// Syncs the current page and trims trailing empty pages first, matching
    /// `saveChanges`'s own order (`eraseEmptyTrailingPages()` before
    /// building the packet).
    ///
    /// Returns a [`BookEditSubmit`] rather than a
    /// [`lodestone_model::ClientAction`] directly, for the identical `Eq`-derive
    /// reason [`super::nav::MenuAction::SetCommandBlock`]'s own doc gives —
    /// `super::nav::MenuNav`'s click dispatch wraps this in
    /// `MenuAction::EditBook`, and `app.rs`'s arm calls
    /// [`BookEditSubmit::into_action`] to cross back.
    #[must_use]
    pub fn to_save_action(&mut self) -> BookEditSubmit {
        self.sync_current_page();
        self.erase_empty_trailing_pages();
        BookEditSubmit {
            slot: self.slot,
            pages: self.pages.clone(),
            title: None,
        }
    }

    /// `BookSignScreen.saveChanges()` — Finalize, with the trimmed title.
    #[must_use]
    pub fn to_sign_action(&mut self) -> BookEditSubmit {
        self.erase_empty_trailing_pages();
        BookEditSubmit {
            slot: self.slot,
            pages: self.pages.clone(),
            title: Some(self.title.value().trim().to_owned()),
        }
    }

    /// Routes a key event to whichever field currently holds keyboard focus
    /// — the title while [`Self::signing`], the page otherwise. Neither
    /// screen has more than one focusable field, so there is no navigation
    /// to arbitrate — see the module doc's "What is deliberately
    /// simplified".
    pub fn handle_key(&mut self, event: KeyEvent) -> bool {
        if self.signing {
            self.title.handle_key(event)
        } else {
            self.page.handle_key(event)
        }
    }

    /// [`Self::handle_key`]'s `charTyped` counterpart.
    pub fn handle_char(&mut self, ch: char) -> bool {
        if self.signing {
            self.title.handle_char(ch)
        } else {
            self.page.handle_char(ch)
        }
    }
}

/// The payload [`BookEditState::to_save_action`]/[`to_sign_action`
/// ](BookEditState::to_sign_action) build, crossing into
/// `super::nav::MenuAction::EditBook` and back out through
/// [`Self::into_action`] — see [`BookEditState::to_save_action`]'s own doc
/// for why this exists rather than a bare [`lodestone_model::ClientAction`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookEditSubmit {
    /// See [`BookEditOpen::slot`].
    pub slot: i32,
    /// The pages to save, in order, with trailing empty pages already
    /// trimmed.
    pub pages: Vec<String>,
    /// `Some(title)` when signing (Finalize); `None` for a draft save
    /// (Done).
    pub title: Option<String>,
}

impl BookEditSubmit {
    /// Rebuilds the [`lodestone_model::ClientAction::EditBook`] this payload
    /// stands in for.
    #[must_use]
    pub fn into_action(self) -> ClientAction {
        ClientAction::EditBook {
            slot: self.slot,
            pages: self.pages,
            title: self.title,
        }
    }
}

/// Row indices for [`BookEditState`] while [`BookEditState::signing`] is
/// `false` — the page editor. Matches [`super::nav::sign_edit_row`]'s own
/// convention of a small `pub mod` of named constants next to the state they
/// index.
pub mod page_row {
    /// The Back page-turn button.
    pub const BACK: usize = 0;
    /// The Forward page-turn button.
    pub const FORWARD: usize = 1;
    /// The Sign button — switches to [`super::sign_row`].
    pub const SIGN: usize = 2;
    /// The Done button — saves the draft and closes.
    pub const DONE: usize = 3;
}

/// Row indices while [`BookEditState::signing`] is `true` — the title entry
/// layout. A disjoint numbering from [`page_row`] is safe because
/// `super::nav::MenuNav`'s click/hover dispatch always checks
/// `BookEditState::signing` before choosing which table a row index means,
/// the same way `Screen::Settings`'s per-page row tables already coexist.
pub mod sign_row {
    /// The title field — a click here is caret placement, a no-op today
    /// (see the module doc).
    pub const TITLE: usize = 0;
    /// The Finalize button — inactive while [`BookEditState::can_finalize`]
    /// is `false`.
    pub const FINALIZE: usize = 1;
    /// The Cancel button — back to the page editor.
    pub const CANCEL: usize = 2;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(pages: Vec<&str>) -> BookEditOpen {
        BookEditOpen {
            slot: 3,
            pages: pages.into_iter().map(str::to_owned).collect(),
            author: "Steve".to_string(),
        }
    }

    #[test]
    fn a_freshly_crafted_book_seeds_one_empty_page() {
        let state = BookEditState::new(open(vec![]));
        assert_eq!(state.page_indicator(), (1, 1));
        assert_eq!(state.page.value(), "");
    }

    #[test]
    fn page_forward_appends_past_the_last_page() {
        let mut state = BookEditState::new(open(vec!["one"]));
        state.page_forward();
        assert_eq!(state.page_indicator(), (2, 2));
        assert_eq!(state.page.value(), "");
    }

    #[test]
    fn page_forward_never_exceeds_the_page_cap() {
        let mut state = BookEditState::new(open(vec!["only"]));
        for _ in 0..(MAX_PAGES + 5) {
            state.page_forward();
        }
        assert_eq!(state.page_indicator().1, MAX_PAGES);
    }

    #[test]
    fn editing_a_page_and_paging_back_and_forth_keeps_the_edit() {
        let mut state = BookEditState::new(open(vec!["", ""]));
        for ch in "hello".chars() {
            state.handle_char(ch);
        }
        state.page_forward();
        assert_eq!(state.page.value(), "");
        state.page_back();
        assert_eq!(state.page.value(), "hello");
    }

    #[test]
    fn done_syncs_the_current_page_and_trims_trailing_empties() {
        let mut state = BookEditState::new(open(vec!["kept", ""]));
        let action = state.to_save_action();
        assert_eq!(action.slot, 3);
        assert_eq!(action.pages, vec!["kept".to_string()]);
        assert_eq!(action.title, None);
    }

    #[test]
    fn signing_requires_a_non_blank_title() {
        let mut state = BookEditState::new(open(vec!["a page"]));
        state.begin_sign();
        assert!(!state.can_finalize());
        for ch in "  ".chars() {
            state.handle_char(ch);
        }
        assert!(!state.can_finalize(), "whitespace-only must not count as a title");
        for ch in "My Book".chars() {
            state.handle_char(ch);
        }
        assert!(state.can_finalize());
        let action = state.to_sign_action();
        assert_eq!(action.pages, vec!["a page".to_string()]);
        assert_eq!(action.title, Some("My Book".to_string()));
    }

    #[test]
    fn cancel_sign_returns_to_the_page_editor_keeping_the_title() {
        let mut state = BookEditState::new(open(vec!["p"]));
        state.begin_sign();
        for ch in "Draft".chars() {
            state.handle_char(ch);
        }
        state.cancel_sign();
        assert!(!state.signing);
        state.begin_sign();
        assert_eq!(state.title.value(), "Draft");
    }

    #[test]
    fn key_and_char_route_to_the_page_or_the_title_by_signing_flag() {
        let mut state = BookEditState::new(open(vec![""]));
        assert!(state.handle_char('x'));
        assert_eq!(state.page.value(), "x");
        assert_eq!(state.title.value(), "");
        state.begin_sign();
        assert!(state.handle_char('y'));
        assert_eq!(state.title.value(), "y");
        // The page's own value must be untouched by title typing.
        assert_eq!(state.page.value(), "x");
    }

    #[test]
    fn all_pages_erased_reaches_zero_rather_than_panicking() {
        let mut state = BookEditState::new(open(vec!["", ""]));
        let action = state.to_save_action();
        assert!(action.pages.is_empty());
    }
}
