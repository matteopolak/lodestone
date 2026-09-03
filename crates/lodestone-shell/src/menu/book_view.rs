//! The signed-book reading screen: vanilla's own book-view screen —
//! the read-only sibling of [`super::book_edit`]'s
//! `BookEditScreen`.
//!
//! ## What it is
//!
//! Right-clicking a `minecraft:written_book` opens this. A hand-held book is
//! client-local — `WrittenBookItem.use` calls `player.openItemGui(...)` —
//! while the lectern form of this screen sends server container actions for
//! page turns and close. That is the same fork shape [`super::book_edit`]
//! already has in `WindowApp::try_use`, one branch further along.
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
//!   vanilla's own get-custom-name accessor's, i.e. the *item tooltip*, which is where
//!   they now appear (`crate::container::tooltip`). Adding a header here
//!   would be this shell inventing a screen vanilla does not have.
//! - **No `textures/gui/book.png` background.** The same simplification
//!   [`super::book_edit`]'s own module doc names for the editor: the page
//!   draws as plain labels over the standard dimmed backdrop, not over the
//!   parchment sprite, because the menu overlay stream has no atlas.
//!   Consequently the page text is drawn in the menu's ordinary light
//!   colour rather than `BookViewScreen`'s `PAGE_TEXT_STYLE` black, which is
//!   only legible against that sprite.
//! ## Page text is interactive, and where that lives
//!
//! A page is a full chat component, so a run on it can carry a click or a
//! hover exactly as a chat line can — `change_page` in particular exists
//! almost solely for books. [`BookViewState::page_runs`] is the one place
//! that says where each authored run draws: [`super::render::
//! book_view_frame`] builds its labels from it and
//! [`BookViewState::run_under_cursor`] hit-tests against it, so a click
//! cannot land on a run the player sees somewhere else. The rects are
//! [`Slot`]s rather than bare numbers for the same reason — [`Slot::resolve`]
//! is then the single definition of where a slot is on a given canvas.
//!
//! The cursor reaches the state rather than the frame builder
//! ([`BookViewState::set_page_cursor`], the shape
//! [`super::nav::MenuNav::set_menu_cursor`] already uses for the row cursor),
//! because `book_view_frame` takes only state and the hover tooltip has to be
//! resolvable from it alone.
//!
//! Still absent, and named rather than hidden: the tooltip a hovered run
//! paints goes through the menu overlay's own tooltip painter, which draws
//! `§`-coded strings — so a hover payload's sixteen legacy colours survive
//! and a hex colour does not. The chat HUD's tooltip carries real spans; this
//! one is a plain-string surface.
//!
//! ## Dependencies
//!
//! [`super::text_area::TextArea`] for wrapping, [`super::book_edit`] for the
//! shared page geometry. Hand-held books have no wire traffic; the lectern
//! form reports `ContainerButtonClick` and `ContainerClose` through the menu
//! action boundary.

use lodestone_model::{ResolvedText, Text, text::InteractiveTextSpan, text::TextSpan};

use super::book_edit::{PAGE_LINE_LIMIT, PAGE_WRAP_CHARS};
use super::render::{Origin, Slot};
use super::text_area::TextArea;

/// The page text block's left edge, as an offset from [`Origin::ScreenTop`] —
/// the same anchor and offset [`super::book_edit`]'s non-signing layout uses,
/// so a draft and the signed book it becomes put their text in the same place.
pub const PAGE_DX: f32 = -60.0;
/// The first wrapped line's top edge.
pub const PAGE_TOP_Y: f32 = 32.0;
/// The pitch between wrapped lines, and each line's own clickable height —
/// one font line.
pub const PAGE_LINE_H: f32 = 9.0;
/// Per-glyph horizontal advance for page text.
///
/// A fixed advance rather than a measured one, matching what the menu overlay
/// actually draws with: authored page text carries no legacy `§` pairs (the
/// component model expands those before a span is produced), so the fixed and
/// the `§`-aware measurement agree run for run.
pub const PAGE_GLYPH_W: f32 = 6.0;

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
/// metadata and its full authored text pages.
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
    /// The pages, in order, with authored style retained for the reader.
    pub pages: Vec<ResolvedText>,
}

impl BookViewOpen {
    /// An empty page list becomes one empty page, matching
    /// `BookViewScreen`'s own `Math.max(this.getNumPages(), 1)` in the page
    /// indicator: a book with no pages still shows "Page 1 of 1" rather than
    /// "Page 1 of 0".
    #[must_use]
    pub fn from_pages(
        title: String,
        author: String,
        generation: u8,
        pages: &[Text],
        translate: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let mut pages: Vec<ResolvedText> =
            pages.iter().map(|page| page.resolve(translate)).collect();
        if pages.is_empty() {
            pages.push(ResolvedText::literal(""));
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
    /// Every authored page, in order. Never empty — see
    /// [`BookViewOpen::from_pages`].
    pages: Vec<ResolvedText>,
    /// `BookViewScreen.currentPage`, always a valid index into
    /// [`Self::pages`].
    current_page: usize,
    /// The current page, wrapped. Read-only: nothing on this screen calls
    /// [`TextArea::handle_key`] or [`TextArea::handle_char`], and the widget
    /// is here purely so the wrap matches the editor's exactly (module doc).
    page: TextArea,
    /// The open server container when this is a lectern reader. Hand-held
    /// books are purely local, but lectern page changes are menu button
    /// actions owned by the server.
    lectern_window_id: Option<i32>,
    /// The row currently hovered, for the generic row-highlight draw — the
    /// same "no keyboard row cursor" shape
    /// [`super::book_edit::BookEditState::hovered`] documents.
    pub hovered: Option<usize>,
    /// The pointer's last known logical position and the canvas it was
    /// measured against — `(x, y, canvas_width, canvas_height)`, the same
    /// tuple [`super::nav::MenuNav::set_menu_cursor`] records for rows.
    ///
    /// Held on the state rather than passed to the frame builder because
    /// [`super::render::book_view_frame`] takes only state, and a hovered
    /// run's tooltip has to be resolvable from that alone. The canvas rides
    /// along because a rect is only meaningful against one.
    page_cursor: Option<(f32, f32, f32, f32)>,
    /// The tooltip the hovered run asks for, resolved by
    /// [`Self::set_page_cursor`] — see that method for why it is stored
    /// rather than derived at draw time.
    page_tooltip: Option<Vec<String>>,
}

/// One authored run on the current page: the run itself, and the rect it
/// draws at.
///
/// The draw and the hit-test both come from [`BookViewState::page_runs`], so
/// the geometry has exactly one definition — see this module's own doc.
#[derive(Debug, Clone, PartialEq)]
pub struct PageRun {
    /// The run's text, fully-inherited style, and whichever click, hover or
    /// insertion the page's component tree put on it.
    pub span: InteractiveTextSpan,
    /// Where it draws, resolvable against any canvas through
    /// [`Slot::resolve`].
    pub slot: Slot,
}

impl BookViewState {
    /// Builds fresh state for [`super::nav::MenuNav::open_book_view`].
    #[must_use]
    pub fn new(open: BookViewOpen) -> Self {
        let mut pages = open.pages;
        if pages.is_empty() {
            pages.push(ResolvedText::literal(""));
        }
        // `with_line_limit` is deliberately **not** set: the limit exists to
        // stop an *editor* growing a page past what the parchment can show,
        // and applying it here would silently refuse to load an
        // over-long page rather than showing its first
        // `PAGE_LINE_LIMIT` lines. The truncation to what fits is
        // `visible_lines`'s, at the draw side, exactly as vanilla's
        // `Math.min(128 / 9, cachedPageComponents.size())` is.
        let mut page = TextArea::new(PAGE_WRAP_CHARS);
        page.set_value(&pages[0].to_plain_string(), true);
        Self {
            title: open.title,
            author: open.author,
            generation: open.generation,
            pages,
            current_page: 0,
            page,
            lectern_window_id: None,
            hovered: None,
            page_cursor: None,
            page_tooltip: None,
        }
    }

    /// Builds the lectern form of the reader. `LecternMenu` stores the page as
    /// a zero-based container-data value, so clamp malformed/out-of-range
    /// values to the book's actual page range before drawing it.
    #[must_use]
    pub fn lectern(open: BookViewOpen, window_id: i32, page: i32) -> Self {
        let mut state = Self::new(open);
        state.lectern_window_id = Some(window_id);
        state.current_page = usize::try_from(page)
            .unwrap_or(0)
            .min(state.pages.len().saturating_sub(1));
        state.reload_page();
        state
    }

    /// The server window that owns page changes, if this reader came from a
    /// lectern rather than an item in either hand.
    #[must_use]
    pub fn lectern_window_id(&self) -> Option<i32> {
        self.lectern_window_id
    }

    /// The packet payload for the current page after a successful page turn.
    /// Vanilla's `LecternScreen.sendPageToServer` sends this new zero-based
    /// index as `ServerboundContainerButtonClickPacket.buttonId`.
    #[must_use]
    pub fn lectern_page_action(&self) -> Option<(i32, i32)> {
        let window_id = self.lectern_window_id?;
        Some((window_id, i32::try_from(self.current_page).unwrap_or(i32::MAX)))
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

    /// Turns to the page a `change_page` click event names, clamped to the
    /// book's real range. Returns whether the page actually changed.
    ///
    /// **The argument is 1-based**, matching the wire (its codec accepts only
    /// a positive integer) and the `book.pageIndicator` line a player reads
    /// off the screen, while [`Self::current_page`] is an index. A book with
    /// three pages therefore honours `1`..=`3`; anything outside clamps
    /// rather than being refused, the same as the reading screen's own
    /// set-page path.
    ///
    /// [`Self::current_page`]: Self::page_indicator
    pub fn force_page(&mut self, one_based: i32) -> bool {
        let index = usize::try_from(one_based.saturating_sub(1))
            .unwrap_or(0)
            .min(self.pages.len() - 1);
        if index == self.current_page {
            return false;
        }
        self.current_page = index;
        self.reload_page();
        true
    }

    /// `pageForward()`.
    pub fn page_forward(&mut self) {
        if self.can_page_forward() {
            self.current_page += 1;
            self.reload_page();
        }
    }

    fn reload_page(&mut self) {
        self.page
            .set_value(&self.pages[self.current_page].to_plain_string(), true);
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

    /// The current page split into the same visible wrapped lines as
    /// [`Self::visible_lines`], while retaining each fully inherited authored
    /// [`TextSpan`] style. The `TextArea` owns wrapping; this method only
    /// intersects its character ranges with the model's already-resolved
    /// spans, so presentation cannot silently change line breaks.
    ///
    /// A projection of [`Self::visible_interactive_lines`] rather than its own
    /// intersection pass: two intersections would be free to disagree about
    /// where a run starts, and a run's *style* boundary and its *click*
    /// boundary have to be the same boundary.
    #[must_use]
    pub fn visible_styled_lines(&self) -> Vec<Vec<TextSpan>> {
        self.visible_interactive_lines()
            .into_iter()
            .map(|line| {
                line.into_iter()
                    .map(|span| TextSpan { text: span.text, style: span.style })
                    .collect()
            })
            .collect()
    }

    /// [`Self::visible_styled_lines`]'s interactive form: the same wrapped
    /// lines, each run still carrying whichever click, hover or insertion the
    /// page's component tree put on it.
    #[must_use]
    pub fn visible_interactive_lines(&self) -> Vec<Vec<InteractiveTextSpan>> {
        let value = self.page.value();
        let spans = self.pages[self.current_page].to_interactive_spans();
        self.page
            .lines()
            .iter()
            .take(PAGE_LINE_LIMIT)
            .map(|line| interactive_range(&spans, line.begin, line.begin + line.len()))
            .filter(|line| !line.is_empty() || !value.is_empty())
            .collect()
    }

    /// Every authored run on the current page with the rect it draws at — the
    /// one definition of the page's text geometry, read by both the draw and
    /// [`Self::run_under_cursor`].
    #[must_use]
    pub fn page_runs(&self) -> Vec<PageRun> {
        let mut runs = Vec::new();
        for (row, line) in self.visible_interactive_lines().into_iter().enumerate() {
            let mut dx = PAGE_DX;
            for span in line {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a page line is at most PAGE_WRAP_CHARS glyphs, exactly representable"
                )]
                let w = span.text.chars().count() as f32 * PAGE_GLYPH_W;
                runs.push(PageRun {
                    span,
                    slot: Slot {
                        origin: Origin::ScreenTop,
                        dx,
                        dy: PAGE_TOP_Y + row as f32 * PAGE_LINE_H,
                        w,
                        h: PAGE_LINE_H,
                    },
                });
                dx += w;
            }
        }
        runs
    }

    /// Records the pointer, in logical canvas pixels, along with the canvas it
    /// was measured against, and resolves the tooltip whichever run now sits
    /// under it asks for.
    ///
    /// **The tooltip is resolved here, not read at draw time**, because it
    /// needs the language table and
    /// [`super::render::book_view_frame`] has no access to one. A page is
    /// already a [`ResolvedText`], but a hover payload inside it is not: the
    /// resolve step carries interactivity through *untouched* by design, so a
    /// payload written as a `translate` still holds its key. Resolving on
    /// cursor motion costs nothing extra — motion is the only thing that can
    /// change which run is hovered.
    pub fn set_page_cursor(
        &mut self,
        x: f32,
        y: f32,
        canvas_width: f32,
        canvas_height: f32,
        translate: &dyn Fn(&str) -> Option<String>,
    ) {
        self.page_cursor = Some((x, y, canvas_width, canvas_height));
        self.page_tooltip = self.hover_tooltip_under_cursor(translate);
    }

    /// The pointer's last known logical position, for
    /// [`super::render::MenuFrame::cursor`].
    #[must_use]
    pub fn page_cursor(&self) -> Option<(f32, f32)> {
        self.page_cursor.map(|(x, y, _, _)| (x, y))
    }

    /// The authored run the pointer is over, or `None` when it is over no page
    /// text (or no pointer has been seen yet).
    ///
    /// A zero canvas is refused rather than divided by: a rect resolved
    /// against one would put every run at the same place.
    #[must_use]
    pub fn run_under_cursor(&self) -> Option<PageRun> {
        let (x, y, canvas_w, canvas_h) = self.page_cursor?;
        if canvas_w <= 0.0 || canvas_h <= 0.0 {
            return None;
        }
        self.page_runs().into_iter().find(|run| {
            let (rx, ry, rw, rh) = run.slot.resolve(canvas_w, canvas_h);
            x >= rx && x <= rx + rw && y >= ry && y <= ry + rh
        })
    }

    /// The click action on the run under the pointer, if that run has one.
    #[must_use]
    pub fn click_under_cursor(&self) -> Option<lodestone_model::text::ClickEvent> {
        self.run_under_cursor()?.span.click
    }

    /// The tooltip lines the run under the pointer asks for, as resolved by
    /// the last [`Self::set_page_cursor`] — for
    /// [`super::render::MenuFrame::tooltip`].
    #[must_use]
    pub fn hover_tooltip(&self) -> Option<Vec<String>> {
        self.page_tooltip.clone()
    }

    /// [`Self::hover_tooltip`]'s producer: the hovered run's hover payload,
    /// resolved and flattened.
    ///
    /// Flattened to `§`-coded strings because that is what the menu overlay's
    /// tooltip painter draws — the loss this module's own doc names. Split on
    /// literal newlines the way the chat tooltip's layout does, so a
    /// multi-line hover payload is multi-line here too. An item or entity
    /// payload has no component to flatten and so shows nothing here; the
    /// chat HUD's own tooltip is the surface that composes those.
    fn hover_tooltip_under_cursor(
        &self,
        translate: &dyn Fn(&str) -> Option<String>,
    ) -> Option<Vec<String>> {
        let hover = self.run_under_cursor()?.span.hover?;
        let text = hover.text_payload()?.resolve(translate).to_legacy_string();
        Some(text.split('\n').map(str::to_owned).collect())
    }
}

/// Intersects an authored span sequence with a `[begin, end)` character range
/// from [`TextArea`]. The text model and `TextArea` both index Unicode scalar
/// values through `.chars()`, so this deliberately does not use byte offsets.
///
/// Every field but `text` applies uniformly to the run, so a slice of a run
/// keeps its style and its interaction untouched.
fn interactive_range(
    spans: &[InteractiveTextSpan],
    begin: usize,
    end: usize,
) -> Vec<InteractiveTextSpan> {
    let mut offset = 0;
    let mut out = Vec::new();
    for span in spans {
        let len = span.text.chars().count();
        let span_end = offset + len;
        let start = begin.max(offset);
        let stop = end.min(span_end);
        if start < stop {
            out.push(InteractiveTextSpan {
                text: span
                    .text
                    .chars()
                    .skip(start - offset)
                    .take(stop - start)
                    .collect(),
                style: span.style,
                click: span.click.clone(),
                hover: span.hover.clone(),
                insertion: span.insertion.clone(),
            });
        }
        offset = span_end;
        if offset >= end {
            break;
        }
    }
    out
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
            pages: vec![ResolvedText::literal("one"), ResolvedText::literal("two"), ResolvedText::literal("three")],
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

    /// A page whose second word carries a `change_page` click and a
    /// `show_text` hover — the shape a book of contents actually has.
    fn linked_page() -> BookViewState {
        use lodestone_model::text::{ClickAction, ClickEvent, HoverEvent};

        let mut link = Text::literal("there");
        link.click = Some(ClickEvent {
            action: ClickAction::ChangePage,
            value: "3".to_owned(),
        });
        link.hover = Some(HoverEvent::ShowText(Box::new(Text::literal("go to page 3"))));
        let page = Text {
            extra: vec![Text::literal("go "), link],
            ..Text::literal("")
        };
        BookViewState::new(BookViewOpen {
            title: "Contents".to_owned(),
            author: "Steve".to_owned(),
            generation: 0,
            pages: vec![
                page.resolve(&|_| None),
                ResolvedText::literal("two"),
                ResolvedText::literal("three"),
            ],
        })
    }

    /// The draw and the hit-test read the same geometry: every label
    /// `render::book_view_frame` emits for page text sits at the `dx`/`dy` of
    /// a [`PageRun`], with the same text, in the same order.
    ///
    /// This is the property that makes a click land where a run was drawn.
    /// Asserted against the frame builder rather than against restated
    /// constants — restating them is exactly how the two would drift.
    #[test]
    fn the_frames_page_labels_are_the_page_runs() {
        let state = linked_page();
        let frame = crate::menu::render::book_view_frame(&state);
        let runs = state.page_runs();
        assert!(!runs.is_empty(), "the fixture page has text");
        for (run, label) in runs.iter().zip(&frame.labels) {
            assert_eq!(label.text, run.span.text);
            assert_eq!(label.origin, run.slot.origin);
            assert!((label.dx - run.slot.dx).abs() < f32::EPSILON, "{label:?} vs {run:?}");
            assert!((label.dy - run.slot.dy).abs() < f32::EPSILON, "{label:?} vs {run:?}");
        }
    }

    /// A click at the centre of the linked run's own rect finds its click
    /// event; a click on the plain run beside it, and one a line below the
    /// text, find nothing.
    ///
    /// The two negatives are what make the positive mean something: a
    /// hit-test that returned the first interactive run regardless of
    /// position would pass the first assertion alone.
    #[test]
    fn a_click_on_a_linked_run_resolves_and_its_neighbours_do_not() {
        let (canvas_w, canvas_h) = (400.0, 240.0);
        let mut state = linked_page();
        let runs = state.page_runs();
        let linked = runs
            .iter()
            .find(|r| r.span.text == "there")
            .expect("the fixture's second run is the link");
        let plain = runs
            .iter()
            .find(|r| r.span.text == "go ")
            .expect("the fixture's first run is plain");

        let centre = |run: &PageRun| {
            let (x, y, w, h) = run.slot.resolve(canvas_w, canvas_h);
            (x + w / 2.0, y + h / 2.0)
        };

        let (lx, ly) = centre(linked);
        state.set_page_cursor(lx, ly, canvas_w, canvas_h, &|_| None);
        assert_eq!(
            state.click_under_cursor().map(|c| c.value),
            Some("3".to_owned())
        );

        let (px, py) = centre(plain);
        state.set_page_cursor(px, py, canvas_w, canvas_h, &|_| None);
        assert_eq!(
            state.click_under_cursor(),
            None,
            "the plain run beside the link must carry no click"
        );

        state.set_page_cursor(lx, ly + PAGE_LINE_H * 2.0, canvas_w, canvas_h, &|_| None);
        assert_eq!(
            state.run_under_cursor(),
            None,
            "two lines below a one-line page there is no run at all"
        );
    }

    /// The hovered run's `show_text` payload becomes the frame's tooltip, and
    /// moving off it clears it. Without the clear a tooltip would hang over
    /// the page for the rest of the session.
    #[test]
    fn a_hovered_runs_tooltip_reaches_the_frame_and_clears_on_leaving() {
        let (canvas_w, canvas_h) = (400.0, 240.0);
        let mut state = linked_page();
        let linked = state
            .page_runs()
            .into_iter()
            .find(|r| r.span.text == "there")
            .expect("the fixture's second run is the link");
        let (x, y, w, h) = linked.slot.resolve(canvas_w, canvas_h);

        state.set_page_cursor(x + w / 2.0, y + h / 2.0, canvas_w, canvas_h, &|_| None);
        assert_eq!(
            crate::menu::render::book_view_frame(&state).tooltip,
            Some(vec!["go to page 3".to_owned()])
        );

        state.set_page_cursor(x + w / 2.0, y + h + PAGE_LINE_H * 3.0, canvas_w, canvas_h, &|_| None);
        assert_eq!(
            crate::menu::render::book_view_frame(&state).tooltip,
            None,
            "moving off the run must clear the tooltip"
        );
    }

    /// `change_page`'s argument is 1-based and clamps at both ends — the page
    /// a player reads off the indicator, not an index.
    #[test]
    fn force_page_is_one_based_and_clamps() {
        let mut state = three_pages();
        assert!(state.force_page(3));
        assert_eq!(state.page_indicator(), (3, 3));
        assert_eq!(state.visible_lines(), vec!["three".to_owned()]);

        assert!(!state.force_page(3), "turning to the current page changes nothing");
        assert!(
            !state.force_page(99),
            "past the end clamps to the last page, which is already current"
        );
        assert_eq!(state.page_indicator(), (3, 3));

        assert!(state.force_page(1));
        assert_eq!(state.page_indicator(), (1, 3));
        assert!(!state.force_page(0), "a zero page clamps to the first, already current");
        assert_eq!(state.page_indicator(), (1, 3));

        // From the first page, a past-the-end request really does move.
        assert!(state.force_page(99));
        assert_eq!(state.page_indicator(), (3, 3));
        assert_eq!(state.visible_lines(), vec!["three".to_owned()]);
    }

    /// The reader must retain the authored component style instead of
    /// flattening a coloured page into an unstyled `String` before rendering.
    #[test]
    fn visible_page_runs_keep_the_authored_text_style() {
        let mut page = Text::literal("ruby");
        page.style.color = Some(lodestone_model::TextColor::Red);
        let state = BookViewState::new(BookViewOpen::from_pages(
            "T".to_owned(),
            "A".to_owned(),
            0,
            &[page],
                                &|_| None,
));

        let runs = state.visible_styled_lines();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0][0].text, "ruby");
        assert_eq!(runs[0][0].style.color, Some(lodestone_model::TextColor::Red));
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
            pages: vec![ResolvedText::literal(long)],
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
        let open = BookViewOpen::from_pages("T".to_owned(), "A".to_owned(), 0, &[], &|_| None);
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
                pages: vec![ResolvedText::literal("one"), ResolvedText::literal("two")],
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
                pages: vec![ResolvedText::literal("p")],
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

    /// A lectern reuses the reader's visible pages, but the server owns its
    /// selected page. Turning one must send `container_button_click` with the
    /// new zero-based page, while Done closes the open lectern container.
    #[test]
    fn lectern_page_turns_report_the_new_page_to_its_open_menu() {
        let mut ui = UiState::new();
        let mut nav = MenuNav::new();
        ui.enter_dev_world();
        nav.open_lectern_book_view(
            &mut ui,
            12,
            BookViewOpen::from_pages(
                "Library".to_owned(),
                "Librarian".to_owned(),
                0,
                &[Text::literal("first"), Text::literal("second")],
                                    &|_| None,
),
            0,
        );

        assert_eq!(
            nav.click(&mut ui, page_row::NEXT),
            crate::menu::nav::MenuAction::ContainerButtonClick {
                window_id: 12,
                button_id: 1,
            }
        );
        assert_eq!(nav.book_view().unwrap().page_indicator(), (2, 2));
        assert_eq!(
            nav.click(&mut ui, page_row::DONE),
            crate::menu::nav::MenuAction::CloseContainer { window_id: 12 }
        );
        assert_eq!(ui.screen(), Screen::Playing);
        assert!(nav.book_view().is_none());
    }
}
