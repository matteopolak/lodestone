//! The signed-book reading screen: vanilla's `BookViewScreen`
//! (`.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/inventory/
//! BookViewScreen.java`) — the read-only sibling of [`super::book_edit`]'s
//! `BookEditScreen`.
//!
//! ## What it is
//!
//! Right-clicking a `minecraft:written_book` opens this. It is entirely
//! client-local — `WrittenBookItem.use` calls `player.openItemGui(...)` and
//! nothing on this screen ever sends a packet, unlike the editable book's
//! `EditBook`. That is the same fork shape [`super::book_edit`] already has
//! in `WindowApp::try_use`, one branch further along.
//!
//! ## Why the wrapping and the page geometry are borrowed, not re-derived
//!
//! `BookViewScreen` and `BookEditScreen` share vanilla's own numbers —
//! `TEXT_WIDTH = 114`, `TEXT_HEIGHT = 128` against `BookEditScreen`'s
//! `126`, both `/ 9` lines — so this module reaches for
//! [`super::book_edit::PAGE_WRAP_CHARS`] and
//! [`super::book_edit::PAGE_LINE_LIMIT`] rather than restating them. The
//! wrapper itself is [`super::text_area::TextArea`], driven read-only: a
//! second word-wrap implementation beside the one the editor already uses
//! would be free to disagree with it about where a line breaks, and the two
//! screens are meant to show the same book identically.
//!
//! ## What is deliberately not here, named rather than hidden
//!
//! - **No title/author/generation header.** `BookViewScreen` draws exactly
//!   three things — the wrapped page text, the `book.pageIndicator` line and
//!   a Done button. The title, the `book.byAuthor` line and the
//!   `book.generation.<n>` line are `WrittenBookContent.addToTooltip`'s and
//!   `ItemStack.getCustomName()`'s, i.e. the *item tooltip*, which is where
//!   they now appear (`crate::container::tooltip`). Adding a header here
//!   would be this shell inventing a screen vanilla does not have.
//! - **No `textures/gui/book.png` background.** The same simplification
//!   [`super::book_edit`]'s own module doc names for the editor: the page
//!   draws as plain labels over the standard dimmed backdrop, not over the
//!   parchment sprite, because the menu overlay stream has no atlas.
//!   Consequently the page text is drawn in the menu's ordinary light
//!   colour rather than `BookViewScreen`'s `PAGE_TEXT_STYLE` black, which is
//!   only legible against that sprite.
//! - **No click/hover events on page text.** Vanilla's pages are full chat
//!   components and a `ClickEvent` on one runs a command or opens a link.
//!   Pages are flattened to plain text here ([`BookViewOpen::from_pages`]),
//!   so a book carrying such a component reads correctly and is inert. This
//!   is a real gap, not a decision that the data does not exist: the pages
//!   arrive as [`lodestone_model::Text`] and are flattened on the way in.
//!
//! ## Dependencies
//!
//! [`super::text_area::TextArea`] for wrapping, [`super::book_edit`] for the
//! shared page geometry. Nothing outbound — this screen has no wire traffic
//! at all.

use lodestone_model::Text;

use super::book_edit::{PAGE_LINE_LIMIT, PAGE_WRAP_CHARS};
use super::text_area::TextArea;

/// Row indices for [`super::render::book_view_frame`]'s `rows`, in the order
/// that function builds them. Mirrors [`super::book_edit::page_row`]'s own
/// convention so a click dispatch reads the same way on both book screens.
pub mod page_row {
    /// `backButton` — `<`.
    pub const PREVIOUS: usize = 0;
    /// `forwardButton` — `>`.
    pub const NEXT: usize = 1;
    /// `CommonComponents.GUI_DONE`.
    pub const DONE: usize = 2;
}

/// What opening this screen needs: a signed book's already-resolved
/// metadata and its pages as plain text.
///
/// Built by `Sim::written_book_in_hand` from the stack's own
/// `minecraft:written_book_content`, so a book that carries no such
/// component never produces one of these and never opens the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookViewOpen {
    /// `WrittenBookContent.title` — carried for the screen's narration and
    /// for anything that wants to name the open book; the screen itself does
    /// not draw it (see the module doc).
    pub title: String,
    /// `WrittenBookContent.author`.
    pub author: String,
    /// `WrittenBookContent.generation`, `0..=3`.
    pub generation: u8,
    /// The pages, in order, flattened to plain text.
    pub pages: Vec<String>,
}

impl BookViewOpen {
    /// Flattens a decoded book's [`Text`] pages to the plain strings this
    /// screen draws — see the module doc on the click/hover gap this loses.
    ///
    /// An empty page list becomes one empty page, matching
    /// `BookViewScreen`'s own `Math.max(this.getNumPages(), 1)` in the page
    /// indicator: a book with no pages still shows "Page 1 of 1" rather than
    /// "Page 1 of 0".
    #[must_use]
    pub fn from_pages(title: String, author: String, generation: u8, pages: &[Text]) -> Self {
        let mut pages: Vec<String> = pages.iter().map(Text::to_plain_string).collect();
        if pages.is_empty() {
            pages.push(String::new());
        }
        Self {
            title,
            author,
            generation,
            pages,
        }
    }
}

/// The reading screen's live state — vanilla's `BookViewScreen`'s
/// `currentPage` plus the wrapped text of the page it names.
#[derive(Debug, Clone, PartialEq)]
pub struct BookViewState {
    /// See [`BookViewOpen::title`].
    pub title: String,
    /// See [`BookViewOpen::author`].
    pub author: String,
    /// See [`BookViewOpen::generation`].
    pub generation: u8,
    /// Every page's plain text, in order. Never empty — see
    /// [`BookViewOpen::from_pages`].
    pages: Vec<String>,
    /// `BookViewScreen.currentPage`, always a valid index into
    /// [`Self::pages`].
    current_page: usize,
    /// The current page, wrapped. Read-only: nothing on this screen calls
    /// [`TextArea::handle_key`] or [`TextArea::handle_char`], and the widget
    /// is here purely so the wrap matches the editor's exactly (module doc).
    page: TextArea,
    /// The row currently hovered, for the generic row-highlight draw — the
    /// same "no keyboard row cursor" shape
    /// [`super::book_edit::BookEditState::hovered`] documents.
    pub hovered: Option<usize>,
}

impl BookViewState {
    /// Builds fresh state for [`super::nav::MenuNav::open_book_view`].
    #[must_use]
    pub fn new(open: BookViewOpen) -> Self {
        let mut pages = open.pages;
        if pages.is_empty() {
            pages.push(String::new());
        }
        // `with_line_limit` is deliberately **not** set: the limit exists to
        // stop an *editor* growing a page past what the parchment can show,
        // and applying it here would silently refuse to load an
        // over-long page rather than showing its first
        // `PAGE_LINE_LIMIT` lines. The truncation to what fits is
        // `visible_lines`'s, at the draw side, exactly as vanilla's
        // `Math.min(128 / 9, cachedPageComponents.size())` is.
        let mut page = TextArea::new(PAGE_WRAP_CHARS);
        page.set_value(&pages[0], true);
        Self {
            title: open.title,
            author: open.author,
            generation: open.generation,
            pages,
            current_page: 0,
            page,
            hovered: None,
        }
    }

    /// The 1-based page indicator vanilla's `book.pageIndicator` shows —
    /// `(current, total)`, matching [`super::book_edit::BookEditState::
    /// page_indicator`]'s shape.
    #[must_use]
    pub fn page_indicator(&self) -> (usize, usize) {
        (self.current_page + 1, self.pages.len())
    }

    /// Whether `backButton` is visible — `currentPage > 0`.
    #[must_use]
    pub fn can_page_back(&self) -> bool {
        self.current_page > 0
    }

    /// Whether `forwardButton` is visible — `currentPage < numPages - 1`.
    /// Unlike the editor's `>`, this **cannot** append a page: a signed book
    /// is immutable.
    #[must_use]
    pub fn can_page_forward(&self) -> bool {
        self.current_page + 1 < self.pages.len()
    }

    /// `pageBack()`.
    pub fn page_back(&mut self) {
        if self.can_page_back() {
            self.current_page -= 1;
            self.reload_page();
        }
    }

    /// `pageForward()`.
    pub fn page_forward(&mut self) {
        if self.can_page_forward() {
            self.current_page += 1;
            self.reload_page();
        }
    }

    fn reload_page(&mut self) {
        let text = self.pages[self.current_page].clone();
        self.page.set_value(&text, true);
    }

    /// The current page's wrapped lines, capped at what the page can show —
    /// `Math.min(TEXT_HEIGHT / 9, cachedPageComponents.size())`.
    #[must_use]
    pub fn visible_lines(&self) -> Vec<String> {
        let value = self.page.value();
        self.page
            .lines()
            .iter()
            .take(PAGE_LINE_LIMIT)
            .map(|line| value.chars().skip(line.begin).take(line.len()).collect())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::{Screen, UiState, nav::MenuNav};

    fn three_pages() -> BookViewState {
        BookViewState::new(BookViewOpen {
            title: "Wandering Notes".to_owned(),
            author: "Steve".to_owned(),
            generation: 2,
            pages: vec!["one".to_owned(), "two".to_owned(), "three".to_owned()],
        })
    }

    /// Paging is clamped at both ends, and — unlike the editor's `>` — the
    /// forward arrow **never appends**. A signed book is immutable, so a
    /// forward press on the last page must leave the count alone; an editor
    /// copied one line too far would silently grow a read-only book.
    #[test]
    fn paging_clamps_at_both_ends_and_never_appends() {
        let mut state = three_pages();
        assert_eq!(state.page_indicator(), (1, 3));
        assert!(!state.can_page_back(), "page 1 has nothing behind it");

        state.page_back();
        assert_eq!(state.page_indicator(), (1, 3), "a back press on page 1 is inert");

        state.page_forward();
        state.page_forward();
        assert_eq!(state.page_indicator(), (3, 3));
        assert!(!state.can_page_forward(), "page 3 of 3 has nothing ahead of it");

        state.page_forward();
        assert_eq!(
            state.page_indicator(),
            (3, 3),
            "a forward press on the last page must not append a fourth -- that is \
             `BookEditScreen.pageForward`'s behaviour, and this book is signed"
        );
    }

    /// The page the indicator names is the page the screen shows. A paging
    /// bug that moved the counter without reloading the wrapped text would
    /// pass every assertion above and show page one forever.
    #[test]
    fn the_visible_text_follows_the_page_counter() {
        let mut state = three_pages();
        assert_eq!(state.visible_lines(), vec!["one".to_owned()]);
        state.page_forward();
        assert_eq!(state.visible_lines(), vec!["two".to_owned()]);
        state.page_back();
        assert_eq!(state.visible_lines(), vec!["one".to_owned()]);
    }

    /// An over-long page shows its first `PAGE_LINE_LIMIT` wrapped lines and
    /// is not refused — vanilla's `Math.min(TEXT_HEIGHT / 9, size())`.
    ///
    /// The fixture is built from a word count that must wrap past the limit
    /// under *this* shell's wrap width rather than from a guessed string
    /// length, so it cannot quietly stop being a discriminating input if
    /// [`PAGE_WRAP_CHARS`] changes.
    #[test]
    fn an_over_long_page_is_truncated_to_what_the_page_can_show() {
        let word = "x".repeat(PAGE_WRAP_CHARS);
        let long = vec![word; PAGE_LINE_LIMIT + 5].join(" ");
        let state = BookViewState::new(BookViewOpen {
            title: String::new(),
            author: String::new(),
            generation: 0,
            pages: vec![long],
        });
        assert_eq!(
            state.visible_lines().len(),
            PAGE_LINE_LIMIT,
            "an over-long page shows exactly what fits, and is not dropped"
        );
    }

    /// A book with no pages at all still reads as "Page 1 of 1", not "of 0" —
    /// `BookViewScreen`'s own `Math.max(getNumPages(), 1)`.
    #[test]
    fn a_pageless_book_still_reads_as_one_page() {
        let open = BookViewOpen::from_pages("T".to_owned(), "A".to_owned(), 0, &[]);
        assert_eq!(BookViewState::new(open).page_indicator(), (1, 1));
    }

    /// **The screen actually reaches a frame.** `menu::render::frame_for` has
    /// no arm for an overlay screen, so `Screen::BookView` opening without a
    /// draw call would be a screen that hit-tests correctly and renders
    /// nothing — the island class `CLAUDE.md`'s first rule names.
    ///
    /// Asserted through `nav::book_edit_overlay_frame`, which is the
    /// function `app/redraw.rs`'s overlay block calls by name, rather than
    /// through `render::book_view_frame` directly: a gate on the frame
    /// builder alone proves the builder and says nothing about whether the
    /// production draw path reaches it.
    #[test]
    fn opening_the_screen_produces_the_frame_the_redraw_path_asks_for() {
        let mut ui = UiState::new();
        let mut nav = MenuNav::new();
        ui.enter_dev_world();
        assert!(
            crate::menu::nav::book_edit_overlay_frame(&ui, &nav).is_none(),
            "control: no book screen is open yet, so the overlay must be absent"
        );

        nav.open_book_view(
            &mut ui,
            BookViewOpen {
                title: "Wandering Notes".to_owned(),
                author: "Steve".to_owned(),
                generation: 2,
                pages: vec!["one".to_owned(), "two".to_owned()],
            },
        );
        assert_eq!(ui.screen(), Screen::BookView);

        let frame = crate::menu::nav::book_edit_overlay_frame(&ui, &nav)
            .expect("the reading screen must produce an overlay frame to draw");
        assert_eq!(frame.rows.len(), page_row::DONE + 1);
        assert!(
            !frame.rows[page_row::PREVIOUS].enabled,
            "`<` must be inert on page 1"
        );
        assert!(frame.rows[page_row::NEXT].enabled, "`>` must be live with a page ahead");
        assert!(
            frame.labels.iter().any(|label| label.text == "one"),
            "the current page's text must reach the frame, got {:?}",
            frame.labels.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        assert!(
            frame.labels.iter().any(|label| label.text == "Page 1 of 2"),
            "the page indicator must reach the frame"
        );
    }

    /// Done closes the screen and sends nothing — the whole of this screen's
    /// exit behaviour (`BookViewScreen` has no wire traffic at all), and the
    /// discriminator against a copy of the editor's Done, which returns a
    /// `MenuAction::EditBook`.
    #[test]
    fn done_closes_the_screen_and_sends_nothing() {
        let mut ui = UiState::new();
        let mut nav = MenuNav::new();
        ui.enter_dev_world();
        nav.open_book_view(
            &mut ui,
            BookViewOpen {
                title: "T".to_owned(),
                author: "A".to_owned(),
                generation: 0,
                pages: vec!["p".to_owned()],
            },
        );

        let action = nav.click(&mut ui, page_row::DONE);
        assert!(
            matches!(action, crate::menu::nav::MenuAction::None),
            "reading a book must produce no outbound action, got {action:?}"
        );
        assert_eq!(ui.screen(), Screen::Playing);
        assert!(
            nav.book_view().is_none(),
            "the closed screen must drop its state, or the next open is stale"
        );
    }
}
