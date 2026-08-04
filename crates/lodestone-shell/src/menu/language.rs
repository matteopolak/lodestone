//! The Language screen (issue #415) — vanilla's `LanguageSelectScreen`, the
//! first of the three settings sub-screens #392's plan always said would need
//! a *different* list widget than `OptionsList` (#396/#397's own
//! `ObjectSelectionList` note) or `KeyBindsList` (#15).
//!
//! ## Why this is the third list-widget kind, not a fold into an existing one
//!
//! `LanguageSelectScreen`'s list is vanilla's `ObjectSelectionList`
//! (`LanguageSelectScreen.java:104`, extending
//! `net.minecraft.client.gui.components.ObjectSelectionList`), and it is
//! shaped like neither list this tree already has:
//!
//! - Unlike [`super::options`]'s `OptionsList`, an entry here is not a
//!   caption-plus-widget pair — it is one centred line of text standing for
//!   the whole row, and the *row itself* is the click target
//!   (`Entry.mouseClicked` selects; `Entry.keyPressed`'s `isSelection()` case
//!   does too), not a button drawn inside it.
//! - Unlike [`super::key_binds`]'s `KeyBindsList`, there is exactly one
//!   control per row, not two right-anchored buttons plus a name label.
//!
//! ## The deliberate departure: rows draw as buttons
//!
//! Vanilla's `AbstractSelectionList` draws a selected/hovered entry with a
//! 1 px outline and a darker fill — no `widget/button*` nine-slice sprite at
//! all. Building that second selection-highlight primitive, in this pipeline,
//! for a list that (see below) has exactly **one** real entry, is geometry in
//! service of nothing — the same call [`super::create_world`] and
//! [`super::key_binds`] already made at a coarser grain (skipping a
//! sub-structure, or a whole tab system, rather than inventing chrome nothing
//! here can fill in). So each [`LanguageEntry`] row draws through the same
//! `widget/button*` path every other settings row already uses
//! ([`super::render::draw_widget`]), reusing 100% of the existing draw code
//! instead of adding a fourth one. If a later pass wants the exact vanilla
//! outline-and-fill look, [`Origin::Language`](super::render::Origin::Language)
//! is the one seam that would need a new draw arm — nothing else here assumes
//! button chrome.
//!
//! ## Why the list has exactly one entry, and why that is not a placeholder
//!
//! This client parses no `languages.json` — `resources.rs`'s `language` field
//! loads exactly one table, `assets/minecraft/lang/en_us.json`, and nothing
//! else. [`LANGUAGES`] therefore has one [`LanguageEntry`], always selected,
//! and selecting it (the only thing there is to select) changes nothing —
//! matching [`super::world_select`]'s own precedent: that screen's search box
//! is described in its own doc as "filters the list — of nothing, today",
//! and this one is the same honest shape, one screen over. A list mechanism
//! that already handles `N` entries correctly and is fed `N = 1` today is not
//! a stub; it is what #415 asked for "at minimum" — see the issue's own
//! scope note that Resource Packs' drag-between-lists and Telemetry's
//! prose-only layout "can follow separately since they build on top of this
//! shape rather than gating it".
//!
//! `en_us`'s display name (`"English (US)"`) is transcribed from vanilla's
//! well-known `languages.json` entry (`{"name":"English","region":"US"}`,
//! joined by `LanguageInfo.toComponent`) rather than read out of this repo's
//! own jar snapshot — that jar ships no `languages.json` at all, only
//! `en_us.json` (`unzip -l .cache/mc/26.2/client.jar | /usr/bin/grep -i
//! lang/`), so unlike every other citation in this module this one is public
//! vanilla knowledge, not jar-verified, and is flagged here rather than
//! presented as if it were.
//!
//! ## What is and is not wired
//!
//! - **Wired**: reaching the screen (the root grid's "Language..." button is
//!   now live) and back (Escape/Done → Root), a real search [`EditBox`]
//!   (typing filters [`LANGUAGES`] by name — see [`filtered`] — exactly the
//!   mechanism vanilla's `filterEntries` runs, just fed one entry), moving
//!   the selection cursor, and selecting the one real entry.
//! - **Decorative — the selection's effect.** Vanilla's `onDone` calls
//!   `languageManager.setSelected` and `minecraft.reloadResourcePacks()`
//!   (`LanguageSelectScreen.java:91-102`) when the selected code differs from
//!   the current one. It never can here: the one entry *is* the current
//!   language, so the guard vanilla itself has (`!selectedEntry.code.equals(
//!   this.languageManager.getSelected())`) is always false. Nothing is
//!   faked to look otherwise.
//! - **Present-and-inactive**: the footer's "Font Settings..." button
//!   (`options.font`), vanilla's own next hop to `FontOptionsScreen` — out of
//!   scope for this pass (see #415's own suggested split); it is the same
//!   `no_screen`-shaped placeholder every other unbuilt destination in
//!   [`super::options::SettingsPage`] uses, one screen closer than before
//!   rather than filed away again.
//!
//! ## Geometry, transcribed
//!
//! Every number is read out of `.cache/mc/26.2/client-src`, file and line
//! named — nothing here is measured off this crate's own output.
//!
//! - [`HEADER_HEIGHT`] = 36: `this.layout.setHeaderHeight((int)(12.0 + 9.0 +
//!   15.0))` (`LanguageSelectScreen.java:50`) — **not** the generic
//!   `OptionsSubScreen` 33 every other page uses, because this header also
//!   carries the search box.
//! - [`FOOTER_HEIGHT`] = 53: `this.layout.setFooterHeight(53)` (`:35`) — also
//!   taller than the generic 33, for the warning line above the button row.
//! - The list itself is constructed with a literal `y = 33` (`:106`), but
//!   `repositionElements` (`:84-89`) immediately calls
//!   `this.languageSelectionList.updateSize(this.width, this.layout)`, which
//!   is `updateSizeAndPosition(width, layout.getContentHeight(),
//!   layout.getHeaderHeight())` (`AbstractSelectionList.java:178-179`) — i.e.
//!   the constructor's `33` is overwritten with the real header height (36)
//!   before a frame is ever drawn. [`HEADER_HEIGHT`] is the value that
//!   survives, not the constructor literal — a vanilla quirk worth recording
//!   (measured, not assumed) rather than "corrected".
//! - [`ROW_H`] = 18: the same constructor's `itemHeight` parameter (`:106`).
//! - [`ROW_WIDTH`] = 270: `getRowWidth() = super.getRowWidth() + 50` (`:136-138`);
//!   `AbstractSelectionList.getRowWidth()`'s own default is `220` (`:389-391`).
//! - Row *y*: `AbstractSelectionList.getFirstEntryY() = getY() + 2` (`:104-106`)
//!   — the same "+2" as [`super::options::LIST_TOP_INSET`] — then one
//!   [`ROW_H`] per subsequent row.
//! - Row *x*: `Entry.extractContent`'s `centeredText(font, text, width / 2,
//!   …)` (`:151`) — the row's text is centred on the **screen's** half-width,
//!   not the row's own left edge, because the row already spans the full
//!   `getRowWidth()` band centred there.
//! - The header/footer widget columns (title, search box, warning label,
//!   button row) are a real [`super::layout::HeaderAndFooterLayout`] +
//!   [`super::layout::LinearLayout`] tree, arranged once per canvas by
//!   [`frame_widget_rects`] — asked, not restated, the same rule
//!   [`super::options::root_widget_rects`] follows for the root page.
//!
//! ## Dependencies
//!
//! - [`super::edit_box::EditBox`] — the search field, the same primitive
//!   [`super::world_select`]'s own search box and [`super::nav::EditForm`]
//!   already use.
//! - [`super::layout`] — [`super::layout::HeaderAndFooterLayout`],
//!   [`super::layout::LinearLayout`], [`super::layout::widget_rects`].
//! - [`super::options`] — [`super::options::LIST_TOP_INSET`],
//!   [`super::options::HEADER_LINE_HEIGHT`], [`super::options::WIDGET_H`],
//!   [`super::options::SMALL_BUTTON_WIDTH`].
//! - [`super::render`] — [`super::render::Origin`] (a
//!   [`super::render::Origin::Language`] arm is added there),
//!   [`super::render::MenuFrame`], [`super::render::MenuRow`],
//!   [`super::render::MenuLabel`], [`super::render::Slot`].
//! - The 26.2 jar's `assets/minecraft/lang/en_us.json` for every caption
//!   verbatim (`options.language.title`, `options.language`,
//!   `gui.language.search`, `options.languageAccuracyWarning`,
//!   `options.font`, `gui.done`).

use super::edit_box::EditBox;
use super::layout::{self, HeaderAndFooterLayout, LayoutSettings, LinearLayout};
use super::options;
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot};
use super::widget::{LayoutElement, Widget};

// -- the data ----------------------------------------------------------------

/// One selectable language: vanilla's `LanguageInfo`, reduced to what this
/// client can show (see the module docs on why there is no region/
/// `bidirectional` metadata to carry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanguageEntry {
    /// The language code, e.g. `"en_us"` (`LanguageManager`'s map key).
    pub code: &'static str,
    /// The display name, `LanguageInfo.toComponent()`'s `"{name} ({region})"`.
    pub name: &'static str,
}

/// The one language table `resources.rs` ever loads — see the module docs'
/// "why the list has exactly one entry" section.
pub const LANGUAGES: &[LanguageEntry] = &[LanguageEntry {
    code: "en_us",
    name: "English (US)",
}];

/// Vanilla's own filter predicate (`filterEntries`,
/// `LanguageSelectScreen.java:120-133`): a case-insensitive substring match
/// against the display name. Vanilla also matches the region separately, but
/// that is already folded into [`LanguageEntry::name`] here (there is no
/// separate region field), so one comparison covers both.
#[must_use]
pub fn filtered(query: &str) -> Vec<LanguageEntry> {
    if query.is_empty() {
        return LANGUAGES.to_vec();
    }
    let needle = query.to_lowercase();
    LANGUAGES
        .iter()
        .copied()
        .filter(|entry| entry.name.to_lowercase().contains(&needle))
        .collect()
}

// -- geometry, transcribed (see the module docs) -----------------------------

/// `LanguageSelectScreen.<init>`'s `setHeaderHeight` (`:50`) — taller than the
/// generic `OptionsSubScreen` 33 because this header also carries the search
/// box.
pub const HEADER_HEIGHT: f32 = 36.0;
/// `setFooterHeight(53)` (`:35`) — taller than the generic 33 for the warning
/// line above the button row.
pub const FOOTER_HEIGHT: f32 = 53.0;
/// `LanguageSelectionList`'s own `itemHeight` (`:106`).
pub const ROW_H: f32 = 18.0;
/// `getRowWidth() = super.getRowWidth() + 50` (`:136-138`), default `220`
/// (`AbstractSelectionList.java:389-391`).
pub const ROW_WIDTH: f32 = 270.0;
/// The search box's real size, `new EditBox(font, 0, 0, 200, 15, …)` (`:43`).
pub const SEARCH_W: f32 = 200.0;
pub const SEARCH_H: f32 = 15.0;

/// `getRowLeft() = getX() + width / 2 - getRowWidth() / 2` on a full-width
/// list (`getX() == 0`) (`AbstractSelectionList.java:372-374`).
#[must_use]
pub fn row_left(width: f32) -> f32 {
    width * 0.5 - ROW_WIDTH * 0.5
}

/// The list's own top, after `repositionElements` overwrites the
/// constructor's literal `33` with the real header height (see the module
/// docs).
#[must_use]
pub fn first_entry_y() -> f32 {
    HEADER_HEIGHT + options::LIST_TOP_INSET
}

/// Zero-width layout stand-in for a `StringWidget` of the given line height —
/// safe for **placement** only, the same trick
/// [`super::options::root_widget_rects`]'s own `string_widget()` uses and
/// documents: a column centred on `width / 2` puts a zero-width child at
/// exactly `width / 2` regardless of its siblings' widths, which is also
/// exactly what [`Align::Centre`] wants.
fn label_stand_in(height: f32) -> Box<dyn LayoutElement> {
    Box::new(Widget::new(0.0, 0.0, 0.0, height, ""))
}

fn sized(w: f32, h: f32) -> Box<dyn LayoutElement> {
    Box::new(Widget::new(0.0, 0.0, w, h, ""))
}

/// Index into [`frame_widget_rects`]'s output for each header/footer widget,
/// in `visitWidgets` order (`addTitle` then `addFooter`,
/// `LanguageSelectScreen.java:39-51,72-81`; the content list is not part of
/// this tree — see the module docs, it is positioned by [`first_entry_y`]
/// directly).
const TITLE_RECT: usize = 0;
const SEARCH_RECT: usize = 1;
const WARNING_RECT: usize = 2;
/// The footer button pair starts here; `+ 1` is Done (see
/// [`LanguagePlacement::Footer`]'s `index`).
const FONT_BUTTON_RECT: usize = 3;

/// The header/footer widget columns, arranged for one canvas — asked of a
/// real [`HeaderAndFooterLayout`] rather than restated, the same rule
/// [`super::options::root_widget_rects`] follows.
#[must_use]
pub fn frame_widget_rects(width: f32, height: f32) -> Vec<(f32, f32, f32, f32)> {
    let mut root = HeaderAndFooterLayout::with_heights(width, height, HEADER_HEIGHT, FOOTER_HEIGHT);

    // `LinearLayout header = layout.addToHeader(LinearLayout.vertical().spacing(4))`
    // `header.defaultCellSetting().alignHorizontallyCenter()` (`:40-41`).
    let mut header = LinearLayout::vertical().spacing(4);
    *header.default_cell_setting() = LayoutSettings::defaults().align_horizontally_center();
    header.add_child(label_stand_in(options::HEADER_LINE_HEIGHT)); // title
    header.add_child(sized(SEARCH_W, SEARCH_H)); // search box
    root.add_to_header(Box::new(header));

    // `LinearLayout footer = layout.addToFooter(LinearLayout.vertical()).spacing(8)`
    // `footer.defaultCellSetting().alignHorizontallyCenter()` (`:73-74`).
    let mut footer = LinearLayout::vertical().spacing(8);
    *footer.default_cell_setting() = LayoutSettings::defaults().align_horizontally_center();
    footer.add_child(label_stand_in(options::HEADER_LINE_HEIGHT)); // warning line
    let mut buttons = LinearLayout::horizontal().spacing(8);
    buttons.add_child(sized(options::SMALL_BUTTON_WIDTH, options::WIDGET_H)); // Font Settings...
    buttons.add_child(sized(options::SMALL_BUTTON_WIDTH, options::WIDGET_H)); // Done
    footer.add_child(Box::new(buttons));
    root.add_to_footer(Box::new(footer));

    root.arrange_elements();
    layout::widget_rects(&root)
}

// -- the row/control model ----------------------------------------------------

/// One focusable widget on this screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageControl {
    /// Select this entry (an index into whatever [`LanguageNav::visible`]
    /// currently shows, i.e. post-filter).
    Select(usize),
    /// The present-and-inactive "Font Settings..." button — see the module
    /// docs.
    FontSettings,
    /// Leave the screen, back to the root.
    Done,
}

impl LanguageControl {
    #[must_use]
    pub fn is_live(self) -> bool {
        matches!(self, LanguageControl::Select(_) | LanguageControl::Done)
    }
}

/// Where one widget sits — [`Origin::Language`]'s whole body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguagePlacement {
    /// A row of the (possibly filtered) language list, absolute index `row`
    /// with `first` scrolled to the window's top.
    Row { row: u16, first: u16 },
    /// The header's title line ("Language").
    Title,
    /// The header's search box.
    Search,
    /// The footer's warning line (`options.languageAccuracyWarning`).
    Warning,
    /// The footer's "Font Settings..."/"Done" pair, `index` 0 or 1.
    Footer { index: u8 },
}

/// The top-left of the widget a [`LanguagePlacement`] names, on a
/// `width`×`height` canvas.
#[must_use]
pub fn placement_anchor(placement: LanguagePlacement, width: f32, height: f32) -> (f32, f32) {
    match placement {
        LanguagePlacement::Row { row, first } => {
            // Off-canvas rather than underflowing, the same anti-island
            // sentinel `super::key_binds::placement_anchor` uses.
            let Some(index) = row.checked_sub(first) else {
                return (-1000.0, -1000.0);
            };
            let y = first_entry_y() + f32::from(index) * ROW_H;
            (width * 0.5, y)
        }
        LanguagePlacement::Title => rect_xy(&frame_widget_rects(width, height), TITLE_RECT),
        LanguagePlacement::Search => rect_xy(&frame_widget_rects(width, height), SEARCH_RECT),
        LanguagePlacement::Warning => rect_xy(&frame_widget_rects(width, height), WARNING_RECT),
        LanguagePlacement::Footer { index } => rect_xy(
            &frame_widget_rects(width, height),
            FONT_BUTTON_RECT + usize::from(index),
        ),
    }
}

fn rect_xy(rects: &[(f32, f32, f32, f32)], index: usize) -> (f32, f32) {
    let (x, y, _, _) = rects.get(index).copied().unwrap_or((-1000.0, -1000.0, 0.0, 0.0));
    (x, y)
}

/// How many rows a canvas may show — same fixed-pixel-budget departure as
/// [`super::options::LIST_WINDOW_PX`] and [`super::key_binds::LIST_WINDOW_PX`]:
/// this pipeline has no scissor, so the window is derived from the shortest
/// content band any `gui_scale` can produce.
#[must_use]
pub fn visible_rows_len() -> usize {
    let window = crate::config::MIN_SCALED_HEIGHT as f32 - HEADER_HEIGHT - FOOTER_HEIGHT;
    (window / ROW_H).floor().max(1.0) as usize
}

// -- navigation ---------------------------------------------------------------

/// What [`LanguageNav`] asks its caller ([`super::options::SettingsNav`]) to
/// do after a keypress or a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageOutcome {
    /// Handled internally.
    None,
    /// Leave this page, back to the root — Done, or Escape.
    Back,
}

/// This screen's own cursor: the search text, which (filtered) entry is
/// selected, and how far the list is scrolled.
#[derive(Debug, Clone, PartialEq)]
pub struct LanguageNav {
    search: EditBox,
    /// Index into [`Self::visible`] (post-filter).
    cursor: usize,
    first: usize,
}

impl Default for LanguageNav {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageNav {
    /// A fresh screen: empty search, the one real entry selected — mirrors
    /// vanilla's own constructor, which calls `setSelected` on the entry
    /// matching `languageManager.getSelected()` (`:107-114`), which for this
    /// client is always the sole entry.
    #[must_use]
    pub fn new() -> Self {
        let mut search = EditBox::new(0.0, 0.0, SEARCH_W, SEARCH_H, "Search languages".to_string());
        search.hint = Some("Search...".to_string()); // gui.language.search
        // This screen has exactly one focusable text field, unlike
        // `world_select`'s/`create_world`'s multi-field forms — there is no
        // Tab order to arbitrate, so the search box is simply always the
        // keyboard's target while this page is open, matching vanilla's own
        // `setInitialFocus(this.search)` (`:54-60`) with nothing else able to
        // steal it away.
        search.widget.focused = true;
        Self {
            search,
            cursor: 0,
            first: 0,
        }
    }

    /// Called whenever the page is entered — see
    /// [`super::options::SettingsNav::activate`]'s `SettingsPage::Language`
    /// arm — so re-opening it never resumes mid-filter or scrolled down,
    /// matching vanilla building a new screen (and a new search box) each
    /// time.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    #[must_use]
    pub fn search(&self) -> &EditBox {
        &self.search
    }

    /// The entries the current search text keeps, in table order.
    #[must_use]
    pub fn entries(&self) -> Vec<LanguageEntry> {
        filtered(self.search.value())
    }

    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub fn first(&self) -> usize {
        self.first
    }

    /// Every focusable control: each visible (post-filter, post-scroll) row,
    /// then Font Settings and Done.
    #[must_use]
    pub fn visible(&self) -> Vec<LanguageControl> {
        let entries = self.entries();
        let len = entries.len();
        let end = (self.first + visible_rows_len()).min(len);
        let mut out: Vec<LanguageControl> = (self.first.min(len)..end)
            .map(LanguageControl::Select)
            .collect();
        out.push(LanguageControl::FontSettings);
        out.push(LanguageControl::Done);
        out
    }

    /// The cursor's position within [`Self::visible`], for the highlight.
    #[must_use]
    pub fn selected_row(&self) -> Option<usize> {
        let len = self.entries().len();
        if self.cursor >= len {
            // Past Font Settings/Done, or off the (possibly just-filtered)
            // list — resolved the same way as a row index, below.
            return self
                .visible()
                .iter()
                .position(|c| matches!(c, LanguageControl::FontSettings | LanguageControl::Done)
                    && self.cursor == len + control_footer_offset(*c));
        }
        self.visible()
            .iter()
            .position(|c| matches!(c, LanguageControl::Select(i) if *i == self.cursor))
    }

    /// Moves the cursor by one control, wrapping — steps over nothing, the
    /// same departure as [`super::options`]'s (4) and [`super::key_binds`]'s
    /// own copy of it.
    pub fn step(&mut self, forward: bool) {
        let len = self.entries().len() + 2; // + Font Settings + Done
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
        let len = self.entries().len();
        if self.cursor >= len {
            return; // the footer is always visible; nothing to scroll for it.
        }
        if self.cursor < self.first {
            self.first = self.cursor;
            return;
        }
        while self.cursor >= self.first + visible_rows_len() {
            if self.first + 1 >= len {
                break;
            }
            self.first += 1;
        }
    }

    /// Puts the cursor on the control at visible row `row` — the mouse's
    /// half.
    pub fn hover_row(&mut self, row: usize) {
        let visible = self.visible();
        let Some(control) = visible.get(row).copied() else {
            return;
        };
        let len = self.entries().len();
        self.cursor = match control {
            LanguageControl::Select(i) => i,
            LanguageControl::FontSettings => len,
            LanguageControl::Done => len + 1,
        };
    }

    /// Activates the control at visible row `row` — a click, resolved
    /// directly to the row it hit rather than through Enter (issue #391's
    /// rule, inherited by construction the same way every sibling screen
    /// added since has been).
    pub fn click_row(&mut self, row: usize) -> LanguageOutcome {
        let visible = self.visible();
        let Some(control) = visible.get(row).copied() else {
            return LanguageOutcome::None;
        };
        self.hover_row(row);
        self.activate(control)
    }

    /// Activates whatever the cursor is on — Enter's half.
    pub fn enter(&mut self) -> LanguageOutcome {
        let len = self.entries().len();
        let control = if self.cursor < len {
            LanguageControl::Select(self.cursor)
        } else if self.cursor == len {
            LanguageControl::FontSettings
        } else {
            LanguageControl::Done
        };
        self.activate(control)
    }

    fn activate(&mut self, control: LanguageControl) -> LanguageOutcome {
        if !control.is_live() {
            return LanguageOutcome::None;
        }
        match control {
            // Selecting the one real entry changes nothing observable — see
            // the module docs. Vanilla's own double-click/Enter-on-selected
            // shortcut (`onDone`) is not reproduced here because it would be
            // indistinguishable from doing nothing, which single-click
            // selection already achieves honestly.
            LanguageControl::Select(_) | LanguageControl::FontSettings => LanguageOutcome::None,
            LanguageControl::Done => LanguageOutcome::Back,
        }
    }

    /// Escape: leave the page — `Screen.shouldCloseOnEsc` plus
    /// `OptionsSubScreen.onClose` (`:69-75`), same as every other settings
    /// sub-screen.
    pub fn escape(&mut self) -> LanguageOutcome {
        LanguageOutcome::Back
    }

    /// Routes a typed character into the search box and re-derives the
    /// selection/scroll for the new (possibly empty) filtered list — vanilla's
    /// `EditBox.setResponder` callback (`:45-49`).
    pub fn type_char(&mut self, ch: char) {
        self.search.handle_char(ch);
        self.after_filter_changed();
    }

    /// Backspace in the search box — `EditBox.keyPressed`'s `deleteText(-1,
    /// ctrl)` arm (`EditBox.java:819`), without the whole-word modifier.
    pub fn backspace(&mut self) {
        self.search.delete_chars(-1);
        self.after_filter_changed();
    }

    fn after_filter_changed(&mut self) {
        self.cursor = 0;
        self.first = 0;
    }
}

fn control_footer_offset(control: LanguageControl) -> usize {
    match control {
        LanguageControl::FontSettings => 0,
        LanguageControl::Done => 1,
        LanguageControl::Select(_) => 0,
    }
}

// -- the frame ----------------------------------------------------------------

/// Builds the whole Language frame. Called from
/// [`super::options::settings_frame`]'s `SettingsPage::Language` branch, the
/// same shape [`super::key_binds::frame`] already established.
#[must_use]
pub fn frame(nav: &LanguageNav) -> MenuFrame<'static> {
    let entries = nav.entries();
    let selected = nav.selected_row();

    // Row 0 is the search box — not part of `LanguageNav::visible`'s cursor
    // space at all, the same split `super::world_select::SEARCH_FIELD` makes:
    // this screen's Up/Down cursor and the search field's keyboard focus are
    // two independent things, and `draw_edit_box` reads the box's own
    // `widget.focused` rather than `MenuFrame::selected` for its caret/border
    // (see [`super::render::draw_edit_box`]), so the field draws focused
    // unconditionally without needing a slot in the cursor's index space.
    let mut rows: Vec<MenuRow> = vec![MenuRow {
        label: nav.search().value().to_string(),
        enabled: true,
        field: true,
        edit: Some(nav.search().clone()),
        slot: Some(Slot {
            origin: Origin::Language(LanguagePlacement::Search),
            dx: 0.0,
            dy: 0.0,
            w: SEARCH_W,
            h: SEARCH_H,
        }),
        ..Default::default()
    }];
    // Every row pushed after this point is one past `selected_row`'s index
    // space because of the search row above.
    let end = (nav.first() + visible_rows_len()).min(entries.len());
    for (row, entry) in entries.iter().enumerate().take(end).skip(nav.first()) {
        rows.push(MenuRow {
            label: entry.name.to_string(),
            enabled: true,
            slot: Some(Slot {
                origin: Origin::Language(LanguagePlacement::Row {
                    row: row as u16,
                    first: nav.first() as u16,
                }),
                dx: -ROW_WIDTH * 0.5,
                dy: 0.0,
                w: ROW_WIDTH,
                h: ROW_H,
            }),
            ..Default::default()
        });
    }
    rows.push(MenuRow {
        label: "Font Settings...".to_string(), // options.font
        enabled: false,
        slot: Some(Slot {
            origin: Origin::Language(LanguagePlacement::Footer { index: 0 }),
            dx: 0.0,
            dy: 0.0,
            w: options::SMALL_BUTTON_WIDTH,
            h: options::WIDGET_H,
        }),
        ..Default::default()
    });
    rows.push(MenuRow {
        label: "Done".to_string(), // gui.done
        enabled: true,
        slot: Some(Slot {
            origin: Origin::Language(LanguagePlacement::Footer { index: 1 }),
            dx: 0.0,
            dy: 0.0,
            w: options::SMALL_BUTTON_WIDTH,
            h: options::WIDGET_H,
        }),
        ..Default::default()
    });

    let labels = vec![
        MenuLabel {
            text: "Language".to_string(), // options.language.title
            origin: Origin::Language(LanguagePlacement::Title),
            dx: 0.0,
            dy: 0.0,
            align: Align::Centre,
            colour: super::widget::ACTIVE_LABEL,
            scale: 1.0,
        },
        MenuLabel {
            // options.languageAccuracyWarning, colour -4539718 (ARGB
            // 255,186,186,186 — see the module docs for the decode).
            text: "(Language translations may not be 100% accurate)".to_string(),
            origin: Origin::Language(LanguagePlacement::Warning),
            dx: 0.0,
            dy: 0.0,
            align: Align::Centre,
            colour: WARNING_COLOUR,
            scale: 1.0,
        },
    ];

    MenuFrame {
        rows,
        labels,
        // Offset by one for the search row prepended above; `usize::MAX` is
        // the shared "nothing highlighted" sentinel every other frame builder
        // here uses (`options::settings_frame`, `key_binds::frame`).
        selected: selected.map_or(usize::MAX, |i| i + 1),
        ..Default::default()
    }
}

/// `-4539718` decoded: ARGB(255, 186, 186, 186).
const WARNING_COLOUR: [f32; 4] = [186.0 / 255.0, 186.0 / 255.0, 186.0 / 255.0, 1.0];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_exactly_one_real_language_and_it_is_english_us() {
        assert_eq!(LANGUAGES.len(), 1);
        assert_eq!(LANGUAGES[0].code, "en_us");
    }

    #[test]
    fn filtering_by_a_matching_substring_keeps_the_one_entry() {
        assert_eq!(filtered("english").len(), 1);
        assert_eq!(filtered("ENGLISH").len(), 1, "vanilla's filter is case-insensitive");
    }

    #[test]
    fn filtering_by_a_non_matching_substring_empties_the_list() {
        assert!(filtered("francais").is_empty());
    }

    #[test]
    fn a_fresh_nav_has_the_one_entry_selected_and_no_search_text() {
        let nav = LanguageNav::new();
        assert_eq!(nav.cursor(), 0);
        assert!(nav.search().value().is_empty());
        assert_eq!(nav.entries().len(), 1);
    }

    #[test]
    fn stepping_reaches_font_settings_then_done_then_wraps() {
        let mut nav = LanguageNav::new();
        assert_eq!(nav.visible()[0], LanguageControl::Select(0));
        nav.step(true);
        assert_eq!(nav.visible()[nav.cursor().min(2)], LanguageControl::FontSettings);
        nav.step(true);
        assert_eq!(
            nav.enter(),
            LanguageOutcome::Back,
            "Done must be reachable by stepping"
        );
    }

    #[test]
    fn selecting_the_one_entry_does_nothing_observable() {
        let mut nav = LanguageNav::new();
        assert_eq!(nav.click_row(0), LanguageOutcome::None);
    }

    #[test]
    fn done_is_reachable_by_click_and_leaves_the_page() {
        let mut nav = LanguageNav::new();
        let visible = nav.visible();
        let done_row = visible
            .iter()
            .position(|c| *c == LanguageControl::Done)
            .expect("Done is always present");
        assert_eq!(nav.click_row(done_row), LanguageOutcome::Back);
    }

    #[test]
    fn typing_a_non_matching_filter_leaves_no_selectable_row_but_keeps_the_footer() {
        let mut nav = LanguageNav::new();
        for ch in "zz".chars() {
            nav.type_char(ch);
        }
        let visible = nav.visible();
        assert!(!visible.iter().any(|c| matches!(c, LanguageControl::Select(_))));
        assert!(visible.contains(&LanguageControl::Done));
    }

    #[test]
    fn escape_leaves_the_page() {
        let mut nav = LanguageNav::new();
        assert_eq!(nav.escape(), LanguageOutcome::Back);
    }

    #[test]
    fn row_left_matches_the_hand_derived_formula() {
        assert_eq!(row_left(480.0), 480.0 * 0.5 - 270.0 * 0.5);
    }

    #[test]
    fn first_entry_y_is_header_height_plus_the_shared_list_top_inset() {
        assert_eq!(first_entry_y(), HEADER_HEIGHT + options::LIST_TOP_INSET);
    }

    #[test]
    fn a_row_placement_scrolled_above_the_window_is_off_canvas() {
        let (x, y) = placement_anchor(
            LanguagePlacement::Row { row: 0, first: 5 },
            480.0,
            270.0,
        );
        assert_eq!((x, y), (-1000.0, -1000.0));
    }

    #[test]
    fn the_footer_buttons_are_two_of_the_five_widget_rects() {
        let rects = frame_widget_rects(480.0, 270.0);
        assert_eq!(rects.len(), 5);
    }
}
