//! The Resource Packs screen (issue #415) — vanilla's `PackSelectionScreen`.
//!
//! ## What it is
//!
//! Two transferable lists over a real pack repository: **Available** on the
//! left, **Selected** on the right. Clicking a row moves it between them;
//! Selected rows carry two right-anchored buttons that move a pack up or down
//! its priority order. Leaving the screen writes the order to
//! [`crate::config::SelectedPacks`] and installs it into
//! [`crate::resources`]'s pack stack, so the next atlas/model build sees it.
//!
//! This replaces the deliberately-reduced shape this module shipped as first
//! (Available permanently empty, Selected permanently one non-removable entry,
//! no transfer controls), which was honest at the time: nothing in the shell
//! knew what a packs *directory* was. `crates/lodestone-assets` always did the
//! hard half — a [`lodestone_assets::ResourceSource`] is a directory tree *or* a
//! zip, and [`lodestone_assets::ResourceManager`] is an ordered stack with
//! vanilla's override semantics — it was simply only ever handed one source.
//!
//! ## The one trap: the UI list and the manager stack are reversed
//!
//! [`lodestone_assets::ResourceManager`] stores sources **lowest priority
//! first**. This screen, like vanilla's, shows **highest priority at the top**.
//! Get it backwards and nothing errors: every pack loads, nothing warns, and the
//! pack on top overrides nothing. Both directions are attested from the record
//! definitions in
//! [`ResourceManager::from_priority_order`](lodestone_assets::ResourceManager::from_priority_order)'s
//! own doc (`FallbackResourceManager.java:55,65` and
//! `PackSelectionModel.java:36-37,52`) — that is the single place the reversal
//! happens, and [`crate::resources`] is its only caller.
//!
//! ## The built-in pack
//!
//! Always selected, always at the **bottom** of the Selected column, never
//! removable — matched by construction rather than by a flag, exactly as
//! vanilla's own fixed-position `Pack.Position.BOTTOM` built-in pack is
//! (`Pack.java:145-157`): [`PackRow::builtin`] rows are appended by
//! [`PacksNav::rebuild`] after the user's, never enumerated as transfer
//! targets, and [`crate::config::SelectedPacks`] does not persist it at all, so
//! there is no state that could deselect it. Labelled the way vanilla labels
//! its own (`pack.nameAndSource` = `"%s (%s)"` over
//! `resourcePack.vanilla.name`/`pack.source.builtin` = `"Default (built-in)"`).
//!
//! ## The row itself
//!
//! A pack row is a **selection-list entry**, not a button:
//! [`super::render::MenuRow::pack`] routes it to `draw_pack_entry`, which draws
//! the 32×32 `pack.png` thumbnail (or vanilla's own `unknown_pack` fallback), the
//! name, up to two grey description lines and the `transferable_list/select`
//! overlay — `TransferableSelectionList.PackEntry.extractContent`'s shape, with
//! that function's doc naming the three departures.
//!
//! It was a button for one release, and worth recording *why* nothing caught it:
//! this module's tests all assert on the **frame data**, which carried the icon
//! and the description correctly throughout. The fault was one branch further on,
//! in which draw the row dispatched to — so a green suite and a screen showing a
//! big centred label were entirely consistent.
//!
//! ## What is deliberately not built
//!
//! - **Pack-format validation.** Vanilla checks `pack_format` against the host's
//!   and shows an "incompatible" warning, a red content box and a confirmation
//!   prompt (`TransferableSelectionList.java:137-144`, `pack.incompatible.*`).
//!   [`lodestone_assets::PackMeta::accepts`] already exists to answer it and
//!   [`crate::resources::DiscoveredPack::pack_format`] already carries the
//!   number, but **nothing in this client declares a host `pack_format`** to
//!   compare against, and the scan drops `pack.mcmeta`'s `supported_formats`
//!   range — so a guessed host number would paint a warning over packs that are
//!   in fact fine. Painting nothing is the honest reduction; a wrong warning is
//!   not. Worth knowing this client is *more* permissive than vanilla here: an
//!   old pack loads and its stale paths silently resolve to nothing.
//! - **A search box** and the **drag-and-drop-file hint** (`pack.dropInfo`).
//!   The hint would advertise a file-drop handler that does not exist; the
//!   header stays the generic 33 px `OptionsSubScreen` band rather than
//!   vanilla's taller one because neither line is drawn.
//! - **A scrollbar.** Both lists share one vertical band, and
//!   [`list_spec`] declares it so the rows get clipped to it
//!   ([`Origin::is_scrolling_list_row`]) and the wheel reaches the focused list
//!   — but the thumb reflects whichever list the cursor is in, not both. A
//!   canvas at [`crate::config::MIN_SCALED_HEIGHT`] shows four rows per column;
//!   beyond that the cursor auto-scrolls and the wheel works, which is what
//!   makes every row reachable.
//! - **Filesystem watching.** Vanilla rescans when the folder changes; here the
//!   scan happens on entering the screen ([`PacksNav::reset`]).
//!
//! ## Geometry
//!
//! - Header: the generic 33 px band ([`options::SUB_HEADER_HEIGHT`]) — see
//!   above. Footer: vanilla's own shape, `Open Pack Folder` + `Done`
//!   (`LinearLayout.horizontal().spacing(8)`, `PackSelectionScreen.java:115-119`)
//!   through [`options::footer_rects`].
//! - The two lists: `width/2 - 15 - 200` and `width/2 + 15`, each 200 px wide,
//!   at the header's bottom (`PackSelectionScreen.java:164-170`). Row geometry:
//!   `TransferableSelectionList::getRowWidth() = width - 4` (`:44-46`), item
//!   height 36, and the underlined header entry's `(int)(9.0F * 1.5F) = 13`
//!   (`:59-60`, Java's truncating cast).
//! - The per-row move buttons are **this client's shape, not vanilla's**:
//!   vanilla draws hover-revealed 32 px sprite zones over the pack icon's two
//!   right quadrants (`TransferableSelectionList.PackEntry.extractContent`,
//!   `:187-209`). Two right-anchored square buttons per row is
//!   [`super::key_binds`]'s existing row shape, which this pipeline already draws
//!   and hit-tests, and is recorded here rather than presented as transcribed.
//!   What they *carry* is vanilla's: a triangle
//!   ([`super::render::MenuRow::arrow`]) rather than the letters `"U"`/`"D"` they
//!   shipped with, drawn as geometry because the fallback bitmap font is
//!   upper-case 5×7 and has no arrow glyph.
//!
//! ## Dependencies
//!
//! - [`crate::resources`] — the directory scan
//!   ([`crate::resources::scan_resource_packs`]) and the live stack
//!   ([`crate::resources::set_selected_packs`]).
//! - [`crate::config::SelectedPacks`] — persistence.
//! - [`super::options`] — [`options::SUB_HEADER_HEIGHT`],
//!   [`options::FOOTER_HEIGHT`], [`options::footer_rects`],
//!   [`options::Placement::Footer`], [`options::SMALL_BUTTON_WIDTH`],
//!   [`options::WIDGET_H`], [`options::title_y`].
//! - [`super::render::FaviconMosaic`] — the `pack.png` thumbnail, through the
//!   same box-filtered mosaic a server favicon and an account head already use.
//! - The 26.2 jar's `assets/minecraft/lang/en_us.json` for every caption
//!   verbatim (`resourcePack.title`, `pack.available.title`,
//!   `pack.selected.title`, `pack.openFolder`, `pack.nameAndSource`,
//!   `resourcePack.vanilla.name`, `pack.source.builtin`, `gui.done`).

use super::options::{self, Placement};
use super::render::{
    Align, Arrow, FaviconMosaic, MenuFrame, MenuLabel, MenuRow, Origin, PackEntryView, Slot,
};

/// One pack row, in either column.
#[derive(Debug, Clone, PartialEq)]
pub struct PackRow {
    /// Vanilla's pack id — `"file/<filename>"` for a discovered pack,
    /// [`BUILTIN_ID`] for the built-in one.
    pub id: String,
    /// The display title (the filename, or `"Default"`).
    pub title: String,
    /// The `pack.mcmeta` description, flattened to plain text.
    pub description: String,
    /// The `pack.png` thumbnail, reduced to a mosaic.
    pub icon: Option<FaviconMosaic>,
    /// Whether this is the built-in pack — see the module docs.
    pub builtin: bool,
}

/// The built-in pack's id. Never persisted and never a transfer target.
pub const BUILTIN_ID: &str = "vanilla";

impl PackRow {
    /// The built-in pack's row: `pack.nameAndSource` over
    /// `resourcePack.vanilla.name` / `pack.source.builtin`.
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            id: BUILTIN_ID.to_string(),
            title: "Default (built-in)".to_string(),
            description: "The default look and feel of Minecraft".to_string(),
            icon: None,
            builtin: true,
        }
    }

    /// The label the row draws — the title, verbatim.
    #[must_use]
    pub fn label(&self) -> String {
        self.title.clone()
    }
}

// -- geometry, transcribed (see the module docs) -----------------------------

/// `TransferableSelectionList`'s per-list width (`PackSelectionScreen.java:113-114`).
pub const LIST_W: f32 = 200.0;
/// The gap between each list's inner edge and the screen's centre —
/// `this.width / 2 - 15 - 200` / `this.width / 2 + 15` (`:165,169`).
pub const LIST_GAP: f32 = 15.0;
/// `TransferableSelectionList::getRowWidth() = this.width - 4` (`:44-46`).
pub const ROW_W: f32 = LIST_W - 4.0;
/// The header ("Available"/"Selected") entry height: Java's truncating
/// `(int)(9.0F * 1.5F)` (`:59-60`).
pub const HEADER_ROW_H: f32 = 13.0;
/// A pack row's height — `ObjectSelectionList`'s `itemHeight` (`:38`).
pub const ROW_H: f32 = 36.0;
/// Side of a per-row move button — see the module docs on why this shape is
/// this client's rather than vanilla's.
pub const MOVE_BTN: f32 = 16.0;
/// Combined width of the two lists plus both gaps, for the scroll band's
/// [`super::widget::RowBand::Centred`] width: `2 * (15 + 200)`.
pub const BAND_W: f32 = 2.0 * (LIST_GAP + LIST_W);

/// Left edge of the Available list.
#[must_use]
pub fn available_x(width: f32) -> f32 {
    width * 0.5 - LIST_GAP - LIST_W
}

/// Left edge of the Selected list.
#[must_use]
pub fn selected_x(width: f32) -> f32 {
    width * 0.5 + LIST_GAP
}

/// Both lists' shared top.
#[must_use]
pub fn list_top() -> f32 {
    options::SUB_HEADER_HEIGHT
}

/// The first pack row's top, below the underlined column header.
#[must_use]
pub fn first_entry_y() -> f32 {
    list_top() + HEADER_ROW_H
}

/// The band both columns scroll in, as the generic [`super::widget::ListSpec`]
/// the clip rect and the mouse wheel both go through.
///
/// `top` is [`first_entry_y`] minus the primitive's own
/// [`super::widget::LIST_CONTENT_PADDING`] so the padding is counted exactly
/// once — the trap `super::language`'s own gate recorded.
///
/// **The one spec here with no band chrome.** Vanilla's two 200 px lists carry a
/// tinted background and a separator pair *each*; one canvas-wide set would paint
/// the gutter this screen deliberately leaves clear between the columns. See
/// [`super::widget::ListChrome::None`].
#[must_use]
pub fn list_spec(len: usize, scroll: f32) -> super::widget::ListSpec {
    super::widget::ListSpec::uniform(
        ROW_H,
        first_entry_y() - super::widget::LIST_CONTENT_PADDING,
        options::FOOTER_HEIGHT,
        len,
        BAND_W,
    )
    .without_chrome()
    .at(scroll)
}

/// Which of the two lists a [`PacksPlacement`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackList {
    /// The left column: packs in the folder that are not selected.
    Available,
    /// The right column: the priority order, highest first.
    Selected,
}

impl PackList {
    /// `pack.available.title` / `pack.selected.title`.
    #[must_use]
    pub fn header_label(self) -> &'static str {
        match self {
            PackList::Available => "Available",
            PackList::Selected => "Selected",
        }
    }

    fn x(self, width: f32) -> f32 {
        match self {
            PackList::Available => available_x(width),
            PackList::Selected => selected_x(width),
        }
    }
}

/// Where one widget sits — [`Origin::Packs`]'s whole body. The footer reuses
/// [`Origin::Settings`]`(`[`Placement::Footer`]`)` directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PacksPlacement {
    /// The screen title.
    Title,
    /// A column's own header label.
    ListHeader(PackList),
    /// A pack row, absolute index `row` in its column, with that column
    /// scrolled `scroll` **pixels** down.
    Row {
        /// Which column.
        list: PackList,
        /// Absolute row index within the column.
        row: u16,
        /// The column's scroll offset, in pixels.
        scroll: f32,
    },
    /// A Selected row's move button. `up` picks which of the stacked pair.
    MoveButton {
        /// Absolute row index within the Selected column.
        row: u16,
        /// The upper (raise priority) button when `true`.
        up: bool,
        /// The column's scroll offset, in pixels.
        scroll: f32,
    },
}

/// The top-left of the widget a [`PacksPlacement`] names.
#[must_use]
pub fn placement_anchor(placement: PacksPlacement, width: f32, height: f32) -> (f32, f32) {
    let _ = height;
    match placement {
        PacksPlacement::Title => (
            width * 0.5,
            options::title_y(options::SettingsPage::Controls),
        ),
        PacksPlacement::ListHeader(list) => (list.x(width), list_top()),
        PacksPlacement::Row { list, row, scroll } => (list.x(width), row_y(row, scroll)),
        PacksPlacement::MoveButton { row, up, scroll } => {
            let x = selected_x(width) + ROW_W - 2.0 - MOVE_BTN;
            let y = row_y(row, scroll) + if up { 2.0 } else { 2.0 + MOVE_BTN };
            (x, y)
        }
    }
}

/// A row's top: its absolute offset minus the column's pixel scroll. Pixel
/// scrolling (#445) — `scroll.floor()` is vanilla's `(int)scrollAmount`, and a
/// row above the band simply resolves above it and is clipped by the draw.
fn row_y(row: u16, scroll: f32) -> f32 {
    first_entry_y() + f32::from(row) * ROW_H - scroll.floor()
}

// -- the control model --------------------------------------------------------

/// One focusable control on this screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacksControl {
    /// A pack row. Activating it **transfers** the pack to the other column —
    /// select from Available, unselect from Selected — except the built-in
    /// row, which is selectable and inert.
    Entry {
        /// Which column the row is in.
        list: PackList,
        /// Absolute row index within the column.
        row: u16,
    },
    /// Move a Selected pack one place up (`up`) or down its priority order.
    Move {
        /// Absolute row index within the Selected column.
        row: u16,
        /// Raise priority when `true`.
        up: bool,
    },
    /// Open the `resourcepacks/` folder in the platform file manager.
    OpenPackFolder,
    /// Commit the order and leave.
    Done,
}

// -- navigation ---------------------------------------------------------------

/// What [`PacksNav`] asks its caller ([`super::options::SettingsNav`]) to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacksOutcome {
    /// Handled internally.
    None,
    /// Leave the page — Done, or Escape. The caller must call [`commit`]
    /// first; see [`super::nav::MenuNav`]'s `apply_packs`.
    Back,
}

/// This screen's state: the repository, the order, the cursor, and each
/// column's scroll.
#[derive(Debug, Clone, PartialEq)]
pub struct PacksNav {
    available: Vec<PackRow>,
    /// Highest priority first; the built-in row is always last.
    selected: Vec<PackRow>,
    cursor: usize,
    scroll_available: f32,
    scroll_selected: f32,
}

impl Default for PacksNav {
    /// The built-in pack and nothing else, **without touching the
    /// filesystem** — this is what `SettingsNav::new` constructs, on every
    /// launch and in every test. The scan happens in [`Self::reset`], which is
    /// called when the page is actually entered.
    fn default() -> Self {
        Self {
            available: Vec::new(),
            selected: vec![PackRow::builtin()],
            cursor: 0,
            scroll_available: 0.0,
            scroll_selected: 0.0,
        }
    }
}

impl PacksNav {
    /// Rescans the packs folder and re-derives both columns from the persisted
    /// order — called from [`super::options::SettingsNav::activate`] whenever
    /// the page is entered, which is also vanilla's cadence (a new screen over
    /// a freshly reloaded `PackRepository` each time).
    pub fn reset(&mut self) {
        let discovered = discover();
        let order = crate::resources::selected_packs();
        *self = Self::rebuild(discovered, &order);
    }

    /// Builds both columns from a scan result and an id order (highest priority
    /// first). The pure half of [`Self::reset`], and what tests drive.
    #[must_use]
    pub fn rebuild(discovered: Vec<crate::resources::DiscoveredPack>, order: &[String]) -> Self {
        let rows: Vec<PackRow> = discovered
            .into_iter()
            .map(|p| PackRow {
                icon: p
                    .icon
                    .as_ref()
                    .and_then(|img| {
                        super::render::head_mosaic(
                            &img.rgba,
                            img.width as usize,
                            img.height as usize,
                        )
                    }),
                id: p.id,
                title: p.title,
                description: p.description,
                builtin: false,
            })
            .collect();

        // Selected takes the persisted order, skipping ids that are no longer
        // in the folder; everything else falls to Available in scan order.
        let mut selected = Vec::new();
        for id in order {
            if let Some(row) = rows.iter().find(|r| &r.id == id) {
                selected.push(row.clone());
            }
        }
        let available: Vec<PackRow> = rows
            .into_iter()
            .filter(|r| !selected.iter().any(|s| s.id == r.id))
            .collect();
        // The built-in pack is appended, never selected — see the module docs.
        selected.push(PackRow::builtin());

        Self {
            available,
            selected,
            cursor: 0,
            scroll_available: 0.0,
            scroll_selected: 0.0,
        }
    }

    /// The Available column, top to bottom.
    #[must_use]
    pub fn available(&self) -> &[PackRow] {
        &self.available
    }

    /// The Selected column, **highest priority first**, built-in last.
    #[must_use]
    pub fn selected(&self) -> &[PackRow] {
        &self.selected
    }

    /// The user-selected ids in priority order, built-in excluded — exactly
    /// what [`crate::config::SelectedPacks`] persists and what
    /// [`crate::resources::set_selected_packs`] wants.
    #[must_use]
    pub fn selected_ids(&self) -> Vec<String> {
        self.selected
            .iter()
            .filter(|r| !r.builtin)
            .map(|r| r.id.clone())
            .collect()
    }

    /// The cursor's index into [`Self::controls`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The scroll offset of the column the cursor is in — the one the wheel and
    /// the scrollbar act on.
    #[must_use]
    pub fn scroll(&self) -> f32 {
        match self.focused_list() {
            PackList::Available => self.scroll_available,
            PackList::Selected => self.scroll_selected,
        }
    }

    /// Which column the cursor is in. The footer counts as Selected, so leaving
    /// the lists does not reset the bar to the left column.
    #[must_use]
    pub fn focused_list(&self) -> PackList {
        match self.controls().get(self.cursor) {
            Some(PacksControl::Entry {
                list: PackList::Available,
                ..
            }) => PackList::Available,
            _ => PackList::Selected,
        }
    }

    /// The number of rows in the focused column, for [`list_spec`].
    #[must_use]
    pub fn focused_len(&self) -> usize {
        match self.focused_list() {
            PackList::Available => self.available.len(),
            PackList::Selected => self.selected.len(),
        }
    }

    /// Every focusable control, in cursor order: Available's rows, then each
    /// Selected row followed by its two move buttons, then the footer.
    ///
    /// The built-in row contributes an `Entry` (it is selectable, matching
    /// vanilla's own list) but **no** move buttons — it cannot leave the
    /// bottom, so there is nothing for them to do.
    #[must_use]
    pub fn controls(&self) -> Vec<PacksControl> {
        let mut out = Vec::new();
        for row in 0..self.available.len() {
            out.push(PacksControl::Entry {
                list: PackList::Available,
                row: row as u16,
            });
        }
        for (row, entry) in self.selected.iter().enumerate() {
            let row = row as u16;
            out.push(PacksControl::Entry {
                list: PackList::Selected,
                row,
            });
            if !entry.builtin {
                out.push(PacksControl::Move { row, up: true });
                out.push(PacksControl::Move { row, up: false });
            }
        }
        out.push(PacksControl::OpenPackFolder);
        out.push(PacksControl::Done);
        out
    }

    /// Whether a control can be activated. A move button at an order boundary
    /// is inactive — vanilla's own guard on its move arrows
    /// (`TransferableSelectionList.Entry`'s `canMoveUp`/`canMoveDown`).
    #[must_use]
    pub fn is_live(&self, control: PacksControl) -> bool {
        match control {
            PacksControl::Entry { list, row } => match list {
                PackList::Available => usize::from(row) < self.available.len(),
                // The built-in row is selectable but inert; see `activate`.
                PackList::Selected => usize::from(row) < self.selected.len(),
            },
            PacksControl::Move { row, up } => {
                let row = usize::from(row);
                if self.selected.get(row).is_none_or(|r| r.builtin) {
                    return false;
                }
                if up {
                    row > 0
                } else {
                    // The row below must exist and not be the built-in one.
                    self.selected.get(row + 1).is_some_and(|r| !r.builtin)
                }
            }
            // Now genuinely wired — see `open_pack_folder`.
            PacksControl::OpenPackFolder | PacksControl::Done => true,
        }
    }

    /// Moves the cursor by one control, wrapping.
    pub fn step(&mut self, forward: bool) {
        let len = self.controls().len();
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

    /// Puts the cursor on the control at index `row`.
    pub fn hover_row(&mut self, row: usize) {
        if row < self.controls().len() {
            self.cursor = row;
        }
    }

    /// Activates the control at index `row` — a click, resolved directly to the
    /// row it hit rather than through Enter (#391's rule).
    pub fn click_row(&mut self, row: usize) -> PacksOutcome {
        let Some(&control) = self.controls().get(row) else {
            return PacksOutcome::None;
        };
        self.cursor = row;
        self.activate(control)
    }

    /// Activates whatever the cursor is on — Enter's half.
    pub fn enter(&mut self) -> PacksOutcome {
        let Some(&control) = self.controls().get(self.cursor) else {
            return PacksOutcome::None;
        };
        self.activate(control)
    }

    fn activate(&mut self, control: PacksControl) -> PacksOutcome {
        if !self.is_live(control) {
            return PacksOutcome::None;
        }
        match control {
            PacksControl::Entry { list, row } => {
                self.transfer(list, usize::from(row));
                PacksOutcome::None
            }
            PacksControl::Move { row, up } => {
                self.move_selected(usize::from(row), up);
                PacksOutcome::None
            }
            PacksControl::OpenPackFolder => {
                open_pack_folder();
                PacksOutcome::None
            }
            PacksControl::Done => PacksOutcome::Back,
        }
    }

    /// Escape: leave the page — `Screen.shouldCloseOnEsc` plus
    /// `OptionsSubScreen.onClose`, the same as every settings sub-screen. The
    /// order is committed either way (vanilla's `onClose` reloads too), so
    /// Escape is not a cancel.
    pub fn escape(&mut self) -> PacksOutcome {
        PacksOutcome::Back
    }

    /// Moves the pack at `row` of `from` into the other column.
    ///
    /// A newly selected pack goes to the **top** of Selected — vanilla's
    /// `FolderRepositorySource.DISCOVERED_PACK_SELECTION_CONFIG`, whose default
    /// position is `Pack.Position.TOP` — i.e. it wins over everything already
    /// there, which is what a player who just enabled a pack expects to see.
    fn transfer(&mut self, from: PackList, row: usize) {
        match from {
            PackList::Available => {
                if row >= self.available.len() {
                    return;
                }
                let pack = self.available.remove(row);
                self.selected.insert(0, pack);
            }
            PackList::Selected => {
                // The built-in row is the one Selected entry that cannot move.
                if self.selected.get(row).is_none_or(|r| r.builtin) {
                    return;
                }
                let pack = self.selected.remove(row);
                self.available.push(pack);
                self.available.sort_by(|a, b| a.id.cmp(&b.id));
            }
        }
        self.clamp_cursor();
    }

    /// Swaps the Selected pack at `row` with its neighbour. Guarded by
    /// [`Self::is_live`], so this never reaches the built-in row's slot.
    fn move_selected(&mut self, row: usize, up: bool) {
        let other = if up {
            row.checked_sub(1)
        } else {
            Some(row + 1)
        };
        let Some(other) = other else { return };
        if other >= self.selected.len() || self.selected[other].builtin {
            return;
        }
        self.selected.swap(row, other);
        // Follow the pack, not the slot: the cursor lands on the row the pack
        // just moved to, so holding the button walks it up the list.
        let target = PacksControl::Move { row: other as u16, up };
        if let Some(i) = self.controls().iter().position(|c| *c == target) {
            self.cursor = i;
        }
        self.scroll_to_cursor();
    }

    fn clamp_cursor(&mut self) {
        let len = self.controls().len();
        if len == 0 {
            self.cursor = 0;
        } else if self.cursor >= len {
            self.cursor = len - 1;
        }
        self.scroll_to_cursor();
    }

    /// One mouse-wheel notch on the focused column, through the primitive.
    pub fn scroll_by(&mut self, notches: f32, canvas_height: f32) {
        let Some(mut list) = list_spec(self.focused_len(), self.scroll()).model(canvas_height)
        else {
            return;
        };
        list.mouse_scrolled(notches);
        self.set_scroll(list.scroll());
    }

    fn set_scroll(&mut self, value: f32) {
        match self.focused_list() {
            PackList::Available => self.scroll_available = value,
            PackList::Selected => self.scroll_selected = value,
        }
    }

    /// Brings the cursor's row into the band — vanilla's `ensureVisible`,
    /// through [`super::widget::ScrollList::scroll_to_entry`].
    /// `MIN_SCALED_HEIGHT` for the reason `stats::step` records: a keypress has
    /// no canvas in hand.
    fn scroll_to_cursor(&mut self) {
        let controls = self.controls();
        let Some(&control) = controls.get(self.cursor) else {
            return;
        };
        let row = match control {
            PacksControl::Entry { row, .. } | PacksControl::Move { row, .. } => usize::from(row),
            // The footer is always visible; nothing to scroll for it.
            PacksControl::OpenPackFolder | PacksControl::Done => return,
        };
        let Some(mut list) = list_spec(self.focused_len(), self.scroll())
            .model(crate::config::MIN_SCALED_HEIGHT as f32)
        else {
            return;
        };
        list.scroll_to_entry(row);
        self.set_scroll(list.scroll());
    }
}

/// Installs this screen's order into the live pack stack and persists it.
///
/// Called from [`super::nav::MenuNav`]'s `apply_packs` on the way out, which is
/// vanilla's cadence: `PackSelectionScreen.onClose` commits the model and calls
/// `minecraft.reloadResourcePacks()`. This client has no live reload — see
/// [`crate::resources::selected_packs`]' own doc for which consumer rebuilds
/// when — so the visible effect lands on the next world join.
pub fn commit(nav: &PacksNav) {
    let ids = nav.selected_ids();
    crate::resources::set_selected_packs(ids.clone());
    persist(ids);
}

/// The scan, forked so a unit test never reads the developer's real packs
/// folder. A `#[cfg(test)]` fork rather than an early return on `cfg!(test)`,
/// so the interception is a property of the build rather than a silent skip
/// (`CLAUDE.md` §12.44).
#[cfg(not(test))]
fn discover() -> Vec<crate::resources::DiscoveredPack> {
    crate::resources::scan_resource_packs()
}

#[cfg(test)]
fn discover() -> Vec<crate::resources::DiscoveredPack> {
    // A test drives `PacksNav::rebuild` with its own fixture instead.
    Vec::new()
}

/// The write, forked for [`discover`]'s reason: a unit test must not rewrite
/// the developer's `resource_packs.json`.
#[cfg(not(test))]
fn persist(ids: Vec<String>) {
    if let Err(e) = crate::config::SelectedPacks::from_ids(ids).save() {
        tracing::warn!(target: "menu", "save resource pack selection: {e}");
    }
}

#[cfg(test)]
fn persist(_ids: Vec<String>) {}

/// Opens the `resourcepacks/` folder in the platform file manager — vanilla's
/// `pack.openFolder` button, which calls `Util.getPlatform().openPath(...)`.
///
/// Creates the folder first, because the most likely reason a player pressed
/// this is that it does not exist yet. Forked for [`discover`]'s reason: a unit
/// test must not spawn a file manager (`CLAUDE.md` §12.44's own incident was
/// exactly this shape, one screen over).
#[cfg(not(test))]
fn open_pack_folder() {
    let dir = crate::resources::resource_packs_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(target: "menu", "create {}: {e}", dir.display());
        return;
    }
    super::accounts::open_in_browser(&format!("file://{}", dir.display()));
}

#[cfg(test)]
fn open_pack_folder() {}

// -- the frame ----------------------------------------------------------------

/// Builds the whole Resource Packs frame. Called from
/// [`super::options::settings_frame`]'s `SettingsPage::ResourcePacks` branch.
#[must_use]
pub fn frame(nav: &PacksNav) -> MenuFrame<'static> {
    let mut rows: Vec<MenuRow> = Vec::new();
    for &control in &nav.controls() {
        let (label, detail, icon, pack, arrow) = match control {
            PacksControl::Entry { list, row } => {
                let column = match list {
                    PackList::Available => nav.available(),
                    PackList::Selected => nav.selected(),
                };
                match column.get(usize::from(row)) {
                    // `pack` is what routes this row to `draw_pack_entry` — the
                    // icon, name and description below are drawn as a
                    // selection-list entry rather than as a button's centred
                    // label. Without it they are computed and discarded, which
                    // is exactly the bug this field was added to fix.
                    Some(entry) => (
                        entry.label(),
                        entry.description.clone(),
                        entry.icon.clone(),
                        Some(PackEntryView {
                            // `canSelect()`/`canUnselect()`: an Available row can
                            // be selected, a Selected row unselected — except the
                            // built-in one, which is neither and therefore draws
                            // no hover overlay at all, as vanilla's does not.
                            can_select: list == PackList::Available,
                            can_unselect: list == PackList::Selected && !entry.builtin,
                        }),
                        None,
                    ),
                    None => continue,
                }
            }
            // Vanilla's reorder affordance is a pair of 32 px sprite arrows in the
            // icon's right quadrants; these two are separate square buttons (see
            // the module docs on why), but they are *arrows* now rather than the
            // letters `"U"`/`"D"` they used to be — drawn as geometry, because the
            // fallback bitmap font is upper-case 5×7 and has no arrow glyph. The
            // label is kept as the control's real name; nothing draws it.
            PacksControl::Move { up: true, .. } => (
                "Move Up".to_string(),
                String::new(),
                None,
                None,
                Some(Arrow::Up),
            ),
            PacksControl::Move { up: false, .. } => (
                "Move Down".to_string(),
                String::new(),
                None,
                None,
                Some(Arrow::Down),
            ),
            PacksControl::OpenPackFolder => (
                "Open Pack Folder".to_string(),
                String::new(),
                None,
                None,
                None,
            ),
            PacksControl::Done => ("Done".to_string(), String::new(), None, None, None),
        };
        rows.push(MenuRow {
            label,
            detail,
            favicon: icon,
            pack,
            arrow,
            enabled: nav.is_live(control),
            slot: Some(slot_for(control, nav)),
            ..Default::default()
        });
    }

    let mut labels = vec![MenuLabel {
        text: "Select Resource Packs".to_string(), // resourcePack.title
        origin: Origin::Packs(PacksPlacement::Title),
        dx: 0.0,
        dy: 0.0,
        align: Align::Centre,
        colour: super::widget::ACTIVE_LABEL,
        scale: 1.0,
    }];
    for list in [PackList::Available, PackList::Selected] {
        labels.push(MenuLabel {
            text: list.header_label().to_string(),
            origin: Origin::Packs(PacksPlacement::ListHeader(list)),
            dx: 0.0,
            dy: 0.0,
            align: Align::Left,
            colour: super::widget::ACTIVE_LABEL,
            scale: 1.0,
        });
    }

    MenuFrame {
        rows,
        labels,
        selected: nav.cursor(),
        ..Default::default()
    }
}

fn slot_for(control: PacksControl, nav: &PacksNav) -> Slot {
    match control {
        PacksControl::Entry { list, row } => {
            let scroll = match list {
                PackList::Available => nav.scroll_available,
                PackList::Selected => nav.scroll_selected,
            };
            Slot {
                origin: Origin::Packs(PacksPlacement::Row { list, row, scroll }),
                dx: 0.0,
                dy: 0.0,
                w: ROW_W,
                h: ROW_H,
            }
        }
        PacksControl::Move { row, up } => Slot {
            origin: Origin::Packs(PacksPlacement::MoveButton {
                row,
                up,
                scroll: nav.scroll_selected,
            }),
            dx: 0.0,
            dy: 0.0,
            w: MOVE_BTN,
            h: MOVE_BTN,
        },
        PacksControl::OpenPackFolder => Slot {
            origin: Origin::Settings(Placement::Footer { index: 0, count: 2 }),
            dx: 0.0,
            dy: 0.0,
            w: options::SMALL_BUTTON_WIDTH,
            h: options::WIDGET_H,
        },
        PacksControl::Done => Slot {
            origin: Origin::Settings(Placement::Footer { index: 1, count: 2 }),
            dx: 0.0,
            dy: 0.0,
            w: options::SMALL_BUTTON_WIDTH,
            h: options::WIDGET_H,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{DiscoveredPack, PackKind};

    fn pack(name: &str, description: &str) -> DiscoveredPack {
        DiscoveredPack {
            id: format!("file/{name}"),
            title: name.to_string(),
            description: description.to_string(),
            pack_format: 64,
            icon: None,
            path: std::path::PathBuf::from("/nonexistent").join(name),
            kind: if name.ends_with(".zip") {
                PackKind::Zip
            } else {
                PackKind::Directory
            },
        }
    }

    /// In the order a real scan hands them over: [`crate::resources::
    /// scan_resource_packs_in`] sorts by id, and `rebuild` deliberately does
    /// **not** re-sort, so a fixture in any other order would be testing an
    /// input the production path cannot produce.
    fn two_packs() -> Vec<DiscoveredPack> {
        vec![pack("alpha", "a folder pack"), pack("bravo.zip", "a zip pack")]
    }

    #[test]
    fn a_fresh_screen_with_no_packs_shows_only_the_built_in_one() {
        let nav = PacksNav::default();
        assert!(nav.available().is_empty());
        assert_eq!(nav.selected().len(), 1);
        assert!(nav.selected()[0].builtin);
        assert_eq!(nav.selected()[0].label(), "Default (built-in)");
        assert!(nav.selected_ids().is_empty(), "the built-in pack is never persisted");
    }

    #[test]
    fn both_a_folder_and_a_zip_are_listed_as_available_with_their_descriptions() {
        let nav = PacksNav::rebuild(two_packs(), &[]);
        assert_eq!(nav.available().len(), 2);
        assert_eq!(nav.available()[0].id, "file/alpha", "scan order is the caller's");
        assert_eq!(nav.available()[1].description, "a zip pack");
        assert_eq!(nav.selected().len(), 1, "only the built-in pack starts selected");
    }

    #[test]
    fn selecting_a_pack_puts_it_at_the_top_and_unselecting_returns_it() {
        let mut nav = PacksNav::rebuild(two_packs(), &[]);
        // Click Available row 1 (`bravo.zip`).
        let row = nav
            .controls()
            .iter()
            .position(|c| *c == PacksControl::Entry { list: PackList::Available, row: 1 })
            .expect("two available rows");
        assert_eq!(nav.click_row(row), PacksOutcome::None);
        assert_eq!(nav.selected_ids(), vec!["file/bravo.zip".to_string()]);
        assert_eq!(nav.available().len(), 1);
        assert!(nav.selected().last().is_some_and(|r| r.builtin), "built-in stays at the bottom");

        // And back: the Selected column's row 0 is the pack we just moved.
        let row = nav
            .controls()
            .iter()
            .position(|c| *c == PacksControl::Entry { list: PackList::Selected, row: 0 })
            .expect("one user row plus the built-in");
        assert_eq!(nav.click_row(row), PacksOutcome::None);
        assert!(nav.selected_ids().is_empty());
        assert_eq!(nav.available().len(), 2);
    }

    #[test]
    fn the_built_in_row_cannot_be_unselected_and_has_no_move_buttons() {
        let mut nav = PacksNav::rebuild(two_packs(), &[]);
        let builtin_row = (nav.selected().len() - 1) as u16;
        assert!(
            !nav.controls()
                .iter()
                .any(|c| matches!(c, PacksControl::Move { row, .. } if *row == builtin_row)),
            "the built-in row must offer no reorder controls"
        );
        let row = nav
            .controls()
            .iter()
            .position(
                |c| *c == PacksControl::Entry { list: PackList::Selected, row: builtin_row },
            )
            .expect("the built-in row is selectable");
        nav.click_row(row);
        assert_eq!(nav.selected().len(), 1, "it is still there");
        assert!(nav.selected()[0].builtin);
    }

    /// The list order **is** the priority order, top first — the property
    /// `ResourceManager::from_priority_order` reverses on the way to the stack.
    #[test]
    fn moving_a_pack_up_raises_its_priority_and_the_bottom_one_cannot_go_lower() {
        let order = vec!["file/alpha".to_string(), "file/bravo.zip".to_string()];
        let mut nav = PacksNav::rebuild(two_packs(), &order);
        assert_eq!(nav.selected_ids(), order, "the persisted order is honoured verbatim");

        // `bravo.zip` is second; move it up.
        let up = nav
            .controls()
            .iter()
            .position(|c| *c == PacksControl::Move { row: 1, up: true })
            .expect("row 1 has a move-up button");
        assert!(nav.is_live(PacksControl::Move { row: 1, up: true }));
        nav.click_row(up);
        assert_eq!(
            nav.selected_ids(),
            vec!["file/bravo.zip".to_string(), "file/alpha".to_string()]
        );

        // Row 0 cannot go up, and the last user row cannot go down past the
        // built-in one.
        assert!(!nav.is_live(PacksControl::Move { row: 0, up: true }));
        assert!(!nav.is_live(PacksControl::Move { row: 1, up: false }));
    }

    #[test]
    fn a_selected_id_that_is_no_longer_in_the_folder_is_dropped_not_kept() {
        let nav = PacksNav::rebuild(two_packs(), &["file/deleted".to_string()]);
        assert!(nav.selected_ids().is_empty());
        assert_eq!(nav.available().len(), 2);
    }

    #[test]
    fn done_and_escape_both_leave_the_page() {
        let mut nav = PacksNav::rebuild(two_packs(), &[]);
        let done = nav
            .controls()
            .iter()
            .position(|c| *c == PacksControl::Done)
            .expect("Done is always present");
        assert_eq!(nav.click_row(done), PacksOutcome::Back);
        assert_eq!(nav.escape(), PacksOutcome::Back);
    }

    #[test]
    fn the_two_lists_sit_on_either_side_of_centre_with_vanillas_own_gap() {
        assert_eq!(available_x(480.0), 480.0 * 0.5 - 15.0 - 200.0);
        assert_eq!(selected_x(480.0), 480.0 * 0.5 + 15.0);
        assert_eq!(first_entry_y(), list_top() + HEADER_ROW_H);
    }

    /// The move buttons must sit **inside** their row's right edge, or a click
    /// on one lands on nothing.
    #[test]
    fn a_move_button_sits_inside_its_own_rows_right_edge() {
        let nav = PacksNav::rebuild(two_packs(), &["file/alpha".to_string()]);
        let (bx, by) = placement_anchor(
            PacksPlacement::MoveButton { row: 0, up: true, scroll: 0.0 },
            480.0,
            270.0,
        );
        let (rx, ry) = placement_anchor(
            PacksPlacement::Row { list: PackList::Selected, row: 0, scroll: 0.0 },
            480.0,
            270.0,
        );
        assert!(bx >= rx && bx + MOVE_BTN <= rx + ROW_W, "{bx} .. {}", rx + ROW_W);
        assert!(by >= ry && by + MOVE_BTN <= ry + ROW_H);
        // And the pair does not overlap.
        let (_, by2) = placement_anchor(
            PacksPlacement::MoveButton { row: 0, up: false, scroll: 0.0 },
            480.0,
            270.0,
        );
        assert_eq!(by2, by + MOVE_BTN);
        assert!(by2 + MOVE_BTN <= ry + ROW_H);
        let _ = nav;
    }

    /// The band `list_spec` declares must put its first row where this screen
    /// draws it — the 2 px content padding counted exactly once, which is the
    /// mistake `language.rs`'s own gate recorded making first.
    #[test]
    fn the_declared_band_puts_its_first_row_where_this_screen_draws_it() {
        let list = list_spec(50, 0.0).model(240.0).expect("a band at 240 px");
        assert_eq!(list.first_entry_y(), first_entry_y());
        assert!(list.scrollable(), "50 rows of 36 px must overflow a 240 px canvas");
    }

    /// The **last** row is reachable: wheeling to the clamp puts its bottom edge
    /// inside the band, not under the footer.
    ///
    /// Asked because a player reported settings-screen scrolling "doesn't reach the
    /// end", and this screen shares that band arithmetic
    /// ([`super::widget::ListSpec`]). The row's position comes from [`row_y`] — the
    /// same expression [`placement_anchor`] and therefore the draw use — rather than
    /// from the list model, so this compares the *draw's* answer against the band,
    /// which is the only pairing that could show the reported symptom.
    #[test]
    fn wheeling_to_the_clamp_brings_the_last_row_fully_inside_the_band() {
        for canvas in [crate::config::MIN_SCALED_HEIGHT as f32, 480.0, 763.0] {
            let len = 40;
            let mut list = list_spec(len, 0.0).model(canvas).expect("a band");
            assert!(list.scrollable(), "40 rows of 36 px overflow {canvas} px");
            // Bounded, and inside one loop: negative notches scroll toward the end.
            for _ in 0..500 {
                list.mouse_scrolled(-1.0);
            }
            let scroll = list.scroll();
            assert_eq!(scroll, list.max_scroll(), "the wheel reaches the clamp");
            let last_top = row_y(len as u16 - 1, scroll);
            let band_bottom = list.top() + list.height();
            assert!(
                last_top + ROW_H <= band_bottom,
                "at {canvas} px the last row bottoms out at {} against a band ending at \
                 {band_bottom}",
                last_top + ROW_H
            );
            // And it is not scrolled clean past the top either — the failure mode in
            // the other direction, which "reaches the end" alone cannot see.
            assert!(
                last_top >= list.top(),
                "the last row's top {last_top} is above the band's {}",
                list.top()
            );
        }
    }
}
