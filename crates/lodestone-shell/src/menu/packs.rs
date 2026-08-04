//! The Resource Packs screen (issue #415) — vanilla's `PackSelectionScreen`.
//!
//! ## Why this is a reduced selection list, not the real thing
//!
//! Vanilla's screen is two `TransferableSelectionList`s (Available/Selected)
//! backed by a `PackRepository`: a filesystem watcher over the packs
//! directory, a zip/directory pack detector, drag-and-drop file import,
//! `pack.png` icon loading, and per-entry select/unselect/move-up/move-down
//! sprite buttons that transfer an entry between the two lists (or reorder
//! it within one). This client has **none of that model** —
//! `/usr/bin/grep -rn 'PackRepository\|resourcepacks\|pack\.mcmeta'
//! crates/lodestone-shell/ crates/lodestone-assets/` outside this module
//! finds nothing. `resources.rs` loads exactly one asset source (the
//! bundled jar) and has no concept of a packs *directory*, let alone a list
//! of discovered candidates.
//!
//! So this is deliberately the reduced shape #415 itself invited ("land a
//! simpler selection list and declare the divergence"), following
//! [`super::language`]'s own precedent (one real entry, decorative where the
//! effect does not exist) one step further:
//!
//! - **[`AVAILABLE_PACKS`] is always empty.** This client discovers no
//!   external packs — there is no directory scan to produce one.
//! - **[`SELECTED_PACKS`] always has exactly one entry**: this client's own
//!   built-in assets, labelled the way vanilla itself labels its own
//!   built-in pack (`pack.nameAndSource` = `"%s (%s)"` over
//!   `resourcePack.vanilla.name`/`pack.source.builtin` = `"Default
//!   (built-in)"`). It is not removable, matched by construction rather than
//!   a flag: there is no code path that could move it to `AVAILABLE_PACKS`.
//! - **No transfer controls (select/unselect/move up/down) are built.** With
//!   `AVAILABLE_PACKS` permanently empty and `SELECTED_PACKS`'s one entry
//!   never removable, there is nothing for a transfer control to *do* —
//!   building the sprite/icon mechanism now would be inactive chrome with
//!   no state it could ever change, the same "geometry in service of
//!   nothing" this tree already declines elsewhere (see
//!   `docs/language-screen.md`'s own "What is deliberately not built").
//!   Actual drag gestures are the same call, a layer further down: nothing
//!   to drag onto is still nothing to drag onto.
//! - **No search box**, unlike [`super::language`]'s. That screen kept one
//!   because filtering its one real entry is still a real (if trivial)
//!   predicate reachable by typing. Here the combined real content across
//!   *both* lists is the same one entry, and duplicating Language's
//!   `EditBox` + focus + `MenuKey::Char`/`Backspace` wiring to filter a
//!   single always-present row is disproportionate to what it would buy —
//!   declared here rather than built and left silently inert.
//! - **No drag-and-drop-file hint text** (`pack.dropInfo` = "Drag and drop
//!   files into this window to add packs"). Showing vanilla's own hint with
//!   no file-drop handling behind it would be exactly the "vanilla's labels
//!   without vanilla's function" trap — the hint is omitted rather than
//!   drawn as decoration implying a capability that does not exist.
//!
//! ## Geometry
//!
//! - Header: title only, in the generic 33 px `OptionsSubScreen` band every
//!   other reduced page in this tree uses (`options::SUB_HEADER_HEIGHT`),
//!   since dropping the search box and the drag hint leaves nothing else in
//!   it. Not vanilla's own header height (`4+9+4+9+4+15+4 = 49`, carrying the
//!   hint and search lines this screen does not draw) — restating that
//!   number here would claim a fidelity this screen no longer has.
//! - Footer: **is** vanilla's own shape — `Open Pack Folder` + `Done`,
//!   `LinearLayout.horizontal().spacing(8)` in the generic 33 px footer band
//!   (`PackSelectionScreen.java:115-119`) — so it reuses
//!   [`super::options::footer_rects`]/[`super::options::Placement::Footer`]
//!   directly, the same move [`super::telemetry`] and
//!   [`super::key_binds::footer_controls`] already made for their own
//!   two-button footers.
//! - The two lists: `width/2 - 15 - 200` (Available) and `width/2 + 15`
//!   (Selected), each 200 px wide, y at the header's real bottom
//!   (`PackSelectionScreen.java:164-170`) — transcribed directly, unaffected
//!   by the header reduction above. Row geometry:
//!   `TransferableSelectionList`'s own `getRowWidth() = width - 4`
//!   (`:44-46`), item height 36, and the underlined header entry's height
//!   `(int)(9.0F * 1.5F) = 13` (`:59-60`, Java's truncating cast).
//!
//! ## Wired vs. decorative
//!
//! - **Wired**: reaching the screen (the root grid's "Resource Packs..."
//!   button is now live) and back (Escape/Done → Root), viewing both lists'
//!   real (if minimal) content, cursor navigation.
//! - **Present-and-inactive**: **Open Pack Folder** — there is no packs
//!   directory to open (`resources.rs` has no such path).
//! - **Correctly absent, not decorative**: the search box, the transfer
//!   controls, the drag-and-drop hint, and any entry in `AVAILABLE_PACKS` —
//!   see above for why none of these is a gap.
//!
//! ## Dependencies
//!
//! - `super::options` — [`super::options::SUB_HEADER_HEIGHT`],
//!   [`super::options::FOOTER_HEIGHT`], [`super::options::footer_rects`],
//!   [`super::options::Placement::Footer`], [`super::options::title_y`]
//!   (page-independent for any non-`Root` argument — see its own doc).
//! - The 26.2 jar's `assets/minecraft/lang/en_us.json` for every caption
//!   verbatim (`resourcePack.title`, `pack.available.title`,
//!   `pack.selected.title`, `pack.openFolder`, `pack.nameAndSource`,
//!   `resourcePack.vanilla.name`, `pack.source.builtin`, `gui.done`).

use super::options::{self, Placement};
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot};

/// One resource pack entry, reduced to what this client can show (no icon,
/// no compatibility state, no description — see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackEntry {
    pub id: &'static str,
    pub title: &'static str,
    pub source: &'static str,
}

impl PackEntry {
    /// `pack.nameAndSource` = `"%s (%s)"`.
    #[must_use]
    pub fn label(self) -> String {
        format!("{} ({})", self.title, self.source)
    }
}

/// This client discovers no external packs — see the module docs.
pub const AVAILABLE_PACKS: &[PackEntry] = &[];

/// This client's own built-in assets, always selected, never removable.
pub const SELECTED_PACKS: &[PackEntry] = &[PackEntry {
    id: "vanilla",
    title: "Default",   // resourcePack.vanilla.name
    source: "built-in", // pack.source.builtin
}];

// -- geometry, transcribed (see the module docs) -----------------------------

/// `TransferableSelectionList`'s per-list width (`PackSelectionScreen.java:113-114`).
pub const LIST_W: f32 = 200.0;
/// The gap between the two lists' inner edges and the screen's centre —
/// `this.width / 2 - 15 - 200` / `this.width / 2 + 15` (`:165,169`).
pub const LIST_GAP: f32 = 15.0;
/// `TransferableSelectionList.getRowWidth() = this.width - 4` (`:44-46`).
pub const ROW_W: f32 = LIST_W - 4.0;
/// The header ("Available"/"Selected") entry height: Java's truncating
/// `(int)(9.0F * 1.5F)` (`:59-60`).
pub const HEADER_ROW_H: f32 = 13.0;
/// `PackEntry`'s row height — `ObjectSelectionList` default `itemHeight`
/// passed to the constructor (`:38`).
pub const ROW_H: f32 = 36.0;

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

/// The list's own top — same repositioning quirk
/// [`super::language::first_entry_y`] documents: the real header height
/// wins over any constructor literal. Here the header is genuinely just 33
/// (see the module docs), so there is no discrepancy to record.
#[must_use]
pub fn list_top() -> f32 {
    options::SUB_HEADER_HEIGHT
}

/// Which of the two lists a [`PacksPlacement::Row`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackList {
    Available,
    Selected,
}

impl PackList {
    #[must_use]
    pub fn entries(self) -> &'static [PackEntry] {
        match self {
            PackList::Available => AVAILABLE_PACKS,
            PackList::Selected => SELECTED_PACKS,
        }
    }

    #[must_use]
    pub fn header_label(self) -> &'static str {
        match self {
            PackList::Available => "Available", // pack.available.title
            PackList::Selected => "Selected",    // pack.selected.title
        }
    }

    fn x(self, width: f32) -> f32 {
        match self {
            PackList::Available => available_x(width),
            PackList::Selected => selected_x(width),
        }
    }
}

/// Where one widget sits — [`Origin::Packs`]'s whole body. The footer
/// (Open Pack Folder, Done) reuses [`Origin::Settings`]`(`[`Placement::Footer`]`)`
/// directly instead of a variant here — same move [`super::telemetry`] made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacksPlacement {
    Title,
    /// A list's own header label ("Available"/"Selected").
    ListHeader(PackList),
    /// A `PackEntry` row, absolute index `row` within `list.entries()`
    /// (never scrolled — both lists are far shorter than the visible
    /// window at every canvas size this client supports).
    Row { list: PackList, row: u16 },
}

#[must_use]
pub fn placement_anchor(placement: PacksPlacement, width: f32, height: f32) -> (f32, f32) {
    let _ = height;
    match placement {
        PacksPlacement::Title => (width * 0.5, options::title_y(super::options::SettingsPage::Controls)),
        PacksPlacement::ListHeader(list) => (list.x(width), list_top()),
        PacksPlacement::Row { list, row } => {
            let y = list_top() + HEADER_ROW_H + f32::from(row) * ROW_H;
            (list.x(width), y)
        }
    }
}

// -- the row/control model ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacksControl {
    /// One of `SELECTED_PACKS`' rows — inert to click (see the module docs:
    /// there is nowhere for it to move to).
    Entry { list: PackList, row: u16 },
    /// Present-and-inactive.
    OpenPackFolder,
    Done,
}

impl PacksControl {
    /// An `Entry` is live (selectable, matching vanilla's own
    /// `ObjectSelectionList` rows) even though selecting it has no visible
    /// effect — the same "live but a no-op" shape
    /// [`super::language::LanguageControl::Select`] already has for its own
    /// one real entry. `OpenPackFolder` is the one genuinely inactive
    /// control — see the module docs.
    #[must_use]
    pub fn is_live(self) -> bool {
        !matches!(self, PacksControl::OpenPackFolder)
    }
}

/// Every focusable control, in list order (Available's entries, then
/// Selected's, then the footer) — mirrors every sibling page's
/// `all_controls`. Neither list is ever scrolled (see [`PacksPlacement::Row`]'s
/// doc), so this is also the visible set.
#[must_use]
pub fn all_controls() -> Vec<PacksControl> {
    let mut out = Vec::new();
    for (i, _) in AVAILABLE_PACKS.iter().enumerate() {
        out.push(PacksControl::Entry {
            list: PackList::Available,
            row: i as u16,
        });
    }
    for (i, _) in SELECTED_PACKS.iter().enumerate() {
        out.push(PacksControl::Entry {
            list: PackList::Selected,
            row: i as u16,
        });
    }
    out.push(PacksControl::OpenPackFolder);
    out.push(PacksControl::Done);
    out
}

fn slot_for(control: PacksControl) -> Slot {
    match control {
        PacksControl::Entry { list, row } => Slot {
            origin: Origin::Packs(PacksPlacement::Row { list, row }),
            dx: 0.0,
            dy: 0.0,
            w: ROW_W,
            h: ROW_H,
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

// -- navigation ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacksOutcome {
    None,
    Back,
}

/// This screen's own cursor. No scroll — see [`PacksPlacement::Row`]'s doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PacksNav {
    cursor: usize,
}

impl PacksNav {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn step(&mut self, forward: bool) {
        let len = all_controls().len();
        if len == 0 {
            return;
        }
        self.cursor = if forward {
            (self.cursor + 1) % len
        } else {
            (self.cursor + len - 1) % len
        };
    }

    pub fn hover_row(&mut self, row: usize) {
        if row < all_controls().len() {
            self.cursor = row;
        }
    }

    pub fn click_row(&mut self, row: usize) -> PacksOutcome {
        let all = all_controls();
        let Some(&control) = all.get(row) else {
            return PacksOutcome::None;
        };
        self.cursor = row;
        self.activate(control)
    }

    pub fn enter(&mut self) -> PacksOutcome {
        let all = all_controls();
        let Some(&control) = all.get(self.cursor) else {
            return PacksOutcome::None;
        };
        self.activate(control)
    }

    fn activate(&mut self, control: PacksControl) -> PacksOutcome {
        if !control.is_live() {
            return PacksOutcome::None;
        }
        match control {
            PacksControl::Entry { .. } | PacksControl::OpenPackFolder => PacksOutcome::None,
            PacksControl::Done => PacksOutcome::Back,
        }
    }

    pub fn escape(&mut self) -> PacksOutcome {
        PacksOutcome::Back
    }
}

// -- the frame ----------------------------------------------------------------

#[must_use]
pub fn frame(nav: &PacksNav) -> MenuFrame<'static> {
    let mut rows: Vec<MenuRow> = Vec::new();
    for &control in &all_controls() {
        let label = match control {
            PacksControl::Entry { list, row } => list.entries()[usize::from(row)].label(),
            PacksControl::OpenPackFolder => "Open Pack Folder".to_string(), // pack.openFolder
            PacksControl::Done => "Done".to_string(),                      // gui.done
        };
        rows.push(MenuRow {
            label,
            enabled: control.is_live(),
            slot: Some(slot_for(control)),
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

    let _ = &mut labels;
    MenuFrame {
        rows,
        labels,
        selected: nav.cursor(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_is_always_empty_and_selected_has_exactly_the_built_in_pack() {
        assert!(AVAILABLE_PACKS.is_empty());
        assert_eq!(SELECTED_PACKS.len(), 1);
        assert_eq!(SELECTED_PACKS[0].label(), "Default (built-in)");
    }

    #[test]
    fn all_controls_is_the_one_entry_plus_the_two_footer_buttons() {
        let all = all_controls();
        assert_eq!(all.len(), 3, "{all:?}");
        assert_eq!(
            all[0],
            PacksControl::Entry { list: PackList::Selected, row: 0 }
        );
        assert_eq!(all[1], PacksControl::OpenPackFolder);
        assert_eq!(all[2], PacksControl::Done);
    }

    #[test]
    fn open_pack_folder_is_the_one_inactive_control() {
        for control in all_controls() {
            assert_eq!(
                control.is_live(),
                control != PacksControl::OpenPackFolder,
                "{control:?}"
            );
        }
    }

    #[test]
    fn clicking_the_selected_entry_does_nothing_there_is_nowhere_for_it_to_go() {
        let mut nav = PacksNav::default();
        assert_eq!(nav.click_row(0), PacksOutcome::None);
    }

    #[test]
    fn done_is_reachable_by_stepping_and_leaves_the_page() {
        let mut nav = PacksNav::default();
        nav.step(false); // wrap back to the last control: Done
        assert_eq!(nav.cursor(), 2);
        assert_eq!(nav.enter(), PacksOutcome::Back);
    }

    #[test]
    fn escape_leaves_the_page() {
        assert_eq!(PacksNav::default().escape(), PacksOutcome::Back);
    }

    #[test]
    fn the_two_lists_sit_on_either_side_of_centre_with_vanillas_own_gap() {
        assert_eq!(available_x(480.0), 480.0 * 0.5 - 15.0 - 200.0);
        assert_eq!(selected_x(480.0), 480.0 * 0.5 + 15.0);
    }

    #[test]
    fn a_selected_row_sits_below_its_own_list_header() {
        let (_, header_y) = placement_anchor(
            PacksPlacement::ListHeader(PackList::Selected),
            480.0,
            270.0,
        );
        let (_, row_y) = placement_anchor(
            PacksPlacement::Row { list: PackList::Selected, row: 0 },
            480.0,
            270.0,
        );
        assert_eq!(row_y, header_y + HEADER_ROW_H);
    }
}
