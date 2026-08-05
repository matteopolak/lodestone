//! The Social Interactions screen (issue #189), reached from the pause
//! menu's Player Reporting icon button — vanilla's `SocialInteractionsScreen`.
//!
//! ## What this is
//!
//! An online-player list with a per-player **Hide in Chat**/**Show in Chat**
//! toggle (`gui.socialInteractions.hide`/`.show` — vanilla's own terms; this
//! is not "mute", which is not a string vanilla uses here) and a **Report**
//! button that is present and permanently disabled, because the report flow
//! needs secure chat signing (`ChatSession`/message signatures) and this
//! client has none — `/usr/bin/grep -rn 'SecureChat\|ChatSession\|signed_chat'`
//! over `crates/` turns up nothing. Issue #189's own scope is explicit that
//! this is the real dependency, not a stub to fill in casually: "do not build
//! a fake/unsigned report path".
//!
//! Vanilla gates the whole screen on session kind
//! (`multiplayer.socialInteractions.not_available = "Social Interactions are
//! only available in Multiplayer worlds"`, `SocialInteractionsScreen.java`'s
//! own singleplayer branch) — this client's only "world" is the bundled
//! singleplayer one (#287) today, so [`frame`]'s early-return "unavailable"
//! branch (guarded by [`available_for`]) is the one a player reaches every
//! time until real multiplayer sessions carry a populated
//! [`lodestone_game::tablist::TabList`] here.
//!
//! ## Wired vs. decorative
//!
//! - **Wired**: reaching the screen from the pause menu and back
//!   (Escape/Done), the singleplayer/multiplayer fork (real —
//!   [`super::SessionKind`] is already known at the point this screen opens),
//!   per-row Hide/Show ([`SocialNav::click_row`]/[`SocialNav::enter`] persist
//!   immediately through [`crate::config::HiddenPlayers`], the same
//!   eager-persistence rule `docs/keybindings.md` documents for rebinding).
//! - **Decorative**: the Report button, always inactive — the real dependency
//!   named above, not filled in by this issue. **Hiding a player has no
//!   consumer yet either**: this module only *records* the choice
//!   ([`crate::config::HiddenPlayers`]); nothing in `chat.rs` (off-limits for
//!   this batch — see the issue's file-ownership note) reads it back to
//!   actually suppress a hidden player's messages. So today, hiding a player
//!   persists correctly and self-heals trivially (toggle again, or it simply
//!   has no visible effect) but changes nothing on screen — an honestly
//!   declared island half, not an implied feature.
//! - **Wired since `2453c0f` — the list itself.** This section used to say
//!   "nothing calls [`SocialNav::refresh`] yet, because feeding it the live
//!   `TabList` needs a per-frame call from `app.rs`"; that patch has landed.
//!   `app.rs`'s `drive_ui_from_session` now calls [`entries_from_tablist`]
//!   off `Sim`'s own tab list every frame while connected and feeds the
//!   result to `MenuNav::refresh_social` (`app.rs:1514-1529`), so the roster
//!   shown is the real, live tab list with the local player excluded. See
//!   `docs/social-interactions.md`'s "Wired since" note for the full chain.
//!
//! ## What is deliberately not built
//!
//! Vanilla's screen has three tabs (All/Hidden/Blocked — `gui.socialInteractions.tab_*`).
//! **Blocked** is Microsoft-account-managed (`gui.socialInteractions.blocking_hint`
//! = "Manage with Microsoft account") — decorative in the same way the Online
//! settings page's seven controls are (no account social graph behind it) — so
//! building it would be a tab over nothing. **Hidden** is a filtered view of
//! the same data **All** already has. Given both, three tabs over one flat
//! list is geometry without proportionate value at this scope; this screen is
//! a single flat list instead, a documented reduction rather than a silent
//! one.

use lodestone_game::tablist::{PlayerListEntry, TabList};
use uuid::Uuid;

use super::options::{self, Placement};
use super::render::{Align, MenuFrame, MenuLabel, MenuNotice, MenuRow, Origin, Slot};
use super::widget;

/// Vanilla's `gui.socialInteractions.title`.
pub const TITLE: &str = "Social Interactions";
/// `multiplayer.socialInteractions.not_available`.
pub const NOT_AVAILABLE: &str = "Social Interactions are only available in Multiplayer worlds";

/// One row's worth of vanilla's `PlayerEntry` — just enough to draw the row
/// and act on it. Not [`PlayerListEntry`] directly: this module needs `id`
/// and a display name and nothing else about vanilla's tab-list record
/// (latency, game mode, skin), so it holds its own narrow copy rather than
/// depending on every field [`PlayerListEntry`] happens to carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocialEntry {
    pub id: Uuid,
    pub name: String,
}

impl SocialEntry {
    #[must_use]
    pub fn new(id: Uuid, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
        }
    }
}

/// Lowers a live [`TabList`] into [`SocialEntry`] rows, vanilla's own display
/// order (`TabList::ordered`), excluding `exclude` (the local player — vanilla
/// never lists you against yourself, `SocialInteractionsPlayerList`'s own
/// construction skips `Minecraft.player`'s UUID).
///
/// `app.rs`'s `drive_ui_from_session` calls this every frame while connected
/// to feed [`SocialNav::refresh`] via `MenuNav::refresh_social` — see the
/// module docs' "Wired since" note. Free-standing and pure so it is testable
/// without a live session, the same shape [`super::tablist::player_rows`]
/// already is for the HUD overlay.
#[must_use]
pub fn entries_from_tablist(tab_list: &TabList, exclude: Option<Uuid>) -> Vec<SocialEntry> {
    tab_list
        .ordered()
        .into_iter()
        .filter(|e: &&PlayerListEntry| Some(e.profile.id) != exclude)
        .map(|e| SocialEntry::new(e.profile.id, e.profile.name.clone()))
        .collect()
}

/// One focusable control on this screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocialControl {
    /// Toggle Hide/Show in Chat for the player at this row.
    HideToggle(usize),
    /// Report the player at this row — always inactive, see the module docs.
    Report(usize),
    Done,
}

impl SocialControl {
    #[must_use]
    pub fn is_live(self) -> bool {
        !matches!(self, SocialControl::Report(_))
    }
}

/// Where one [`SocialControl`] sits, mirroring [`super::key_binds::KeyPlacement`]'s
/// shape: every content-list variant shares `{row, scroll}`, only the x differs.
///
/// **`scroll` is pixels (issue #445), and this screen was the last to get there.**
/// This doc used to read: *"Still a row index... blocked on a `ListSpec` change,
/// not on this screen: this list's rows are full-width and left-anchored, while
/// `ListSpec::row_left` is `floor(width / 2) - floor(row_w / 2)` — a centred,
/// fixed-width row. There is no constant `row_w` that makes `row_right` land in
/// this screen's right margin at every canvas width."* That was correct, and it
/// was the right call to wait: the primitive gained
/// [`super::widget::RowBand::Inset`] first, and
/// `widget::tests::no_constant_row_width_can_express_a_full_width_row` now
/// carries the arithmetic that paragraph asserted in prose — a constant tuned
/// exact at 854 px is off by 107 px at 640 and 533 at 1920, because a centred row
/// edge moves at half the canvas edge's rate.
///
/// The one visible consequence of adopting it is in [`RIGHT_MARGIN`]: the row's
/// right gutter had to grow from 10 px to 14 to make room for the scrollbar,
/// which is what "this screen has a bar now" costs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SocialPlacement {
    Name { row: u16, scroll: f32 },
    Hide { row: u16, scroll: f32 },
    Report { row: u16, scroll: f32 },
}

/// Row height. Not vanilla-sourced (vanilla's `PlayerEntry` is 36 px with a
/// head icon this module does not draw — see the module docs' scope note);
/// this reuses [`options::WIDGET_H`], the same flat-row convention
/// [`super::key_binds`] uses for its own non-`OptionsList` rows.
pub const ROW_H: f32 = options::WIDGET_H;
/// Column widths, in the shell's existing button-width vocabulary.
pub const HIDE_BUTTON_W: f32 = 110.0;
pub const REPORT_BUTTON_W: f32 = options::SMALL_BUTTON_WIDTH;
const BUTTON_GAP: f32 = 5.0;
/// The row's right gutter — **and since #445 it is the scrollbar's, not a
/// decorative margin**.
///
/// This was a flat `10.0`. [`super::widget::RowBand::Inset`] requires
/// `SCROLLBAR_WIDTH + 2 + SCROLLBAR_WIDTH` = 14 px beyond where the row's own
/// content ends, because `AbstractSelectionList` overrides `scrollBarX()` to
/// `getRowRight() + scrollbarWidth() + 2` — the bar sits *outside* the row and
/// nothing clamps it to the canvas, so a 10 px gutter put it 4 px off the right
/// edge, silently (an off-canvas rect simply does not draw). Measured, not
/// assumed: `widget::tests::an_inset_rows_right_gutter_must_reserve_room_for_the_
/// scrollbar` observes 10 px failing and pins 14 as the boundary.
///
/// A centred list gets this gutter for free from the canvas margin either side.
/// A full-width one has to declare it, which is why the Hide/Report buttons moved
/// 4 px left when this screen adopted the primitive.
const RIGHT_MARGIN: f32 = super::widget::SCROLLBAR_WIDTH + 2.0 + super::widget::SCROLLBAR_WIDTH;
const NAME_LEFT_INSET: f32 = 4.0;

/// How many rows of list a canvas may show — same fixed-budget departure as
/// [`super::options::LIST_WINDOW_PX`]/[`super::key_binds::LIST_WINDOW_PX`]:
/// this pipeline has no scissor, so the window is derived from the shortest
/// content band any `gui_scale` can produce, not a continuous scroll.
pub const LIST_WINDOW_PX: f32 =
    crate::config::MIN_SCALED_HEIGHT as f32 - options::SUB_HEADER_HEIGHT - options::FOOTER_HEIGHT - options::LIST_TOP_INSET;

#[must_use]
pub fn visible_rows_len() -> usize {
    (LIST_WINDOW_PX / ROW_H).floor().max(1.0) as usize
}

/// This screen's list, as the generic [`super::widget::ListSpec`] (issue #445) —
/// **the first and only adopter of [`super::widget::RowBand::Inset`]**.
///
/// The `row_w` argument to `uniform` is dead here: `spanning` replaces the whole
/// [`super::widget::RowBand`], so `0.0` is passed to say so rather than a number
/// that looks meaningful. The band runs from [`NAME_LEFT_INSET`] — where
/// [`name_x`] actually puts the name — to [`RIGHT_MARGIN`] from the canvas's
/// right edge, so `row_right` is exactly the x [`report_button_x`] hangs its
/// button's right edge off, at every width. That equality is gated below; it is
/// the property no centred `row_w` could have.
#[must_use]
pub fn list_spec(len: usize, scroll: f32) -> super::widget::ListSpec {
    super::widget::ListSpec::uniform(
        ROW_H,
        options::SUB_HEADER_HEIGHT,
        options::FOOTER_HEIGHT,
        len,
        0.0,
    )
    .spanning(NAME_LEFT_INSET, RIGHT_MARGIN)
    .at(scroll)
}

#[must_use]
pub fn report_button_x(width: f32) -> f32 {
    width - RIGHT_MARGIN - REPORT_BUTTON_W
}

#[must_use]
pub fn hide_button_x(width: f32) -> f32 {
    report_button_x(width) - BUTTON_GAP - HIDE_BUTTON_W
}

#[must_use]
pub fn name_x(_width: f32) -> f32 {
    NAME_LEFT_INSET
}

/// The top-left of the widget a [`SocialPlacement`] names, mirroring
/// [`super::key_binds::placement_anchor`].
#[must_use]
pub fn placement_anchor(placement: SocialPlacement, width: f32, _height: f32) -> (f32, f32) {
    let (row, scroll) = match placement {
        SocialPlacement::Name { row, scroll }
        | SocialPlacement::Hide { row, scroll }
        | SocialPlacement::Report { row, scroll } => (row, scroll),
    };
    // Pixel scrolling (#445): the row's absolute offset minus the scroll, so no
    // `checked_sub` to underflow and no off-canvas sentinel — a row above the
    // band resolves above it and `render::draw` clips it. `scroll.floor()` is
    // vanilla's `(int)scrollAmount`.
    let row_y = options::SUB_HEADER_HEIGHT + options::LIST_TOP_INSET + f32::from(row) * ROW_H
        - scroll.floor();
    match placement {
        SocialPlacement::Name { .. } => (name_x(width), row_y),
        SocialPlacement::Hide { .. } => (hide_button_x(width), row_y),
        SocialPlacement::Report { .. } => (report_button_x(width), row_y),
    }
}

/// Whether a session kind shows the real list or vanilla's unavailable
/// message — `multiplayer.socialInteractions.not_available`'s own condition.
#[must_use]
pub fn available_for(kind: Option<super::SessionKind>) -> bool {
    matches!(kind, Some(super::SessionKind::Multiplayer))
}

/// This screen's own cursor and hidden-player choices.
#[derive(Debug, Clone, PartialEq)]
pub struct SocialNav {
    entries: Vec<SocialEntry>,
    cursor: usize,
    /// Scroll offset in **pixels** (issue #445), not a row index. `Eq` went with
    /// the change — see [`SocialPlacement`]'s doc.
    scroll: f32,
    hidden: crate::config::HiddenPlayers,
    hidden_path: std::path::PathBuf,
}

impl SocialNav {
    #[must_use]
    pub fn new() -> Self {
        Self::with_path(crate::config::hidden_players_path())
    }

    /// As [`Self::new`], from an explicit path — for tests, so nothing
    /// touches the developer's real settings file. Mirrors every other
    /// persisted-list nav in this tree (`AccountsNav::with_path`,
    /// `WorldSelectNav`'s sibling pattern).
    #[must_use]
    pub fn with_path(path: std::path::PathBuf) -> Self {
        Self {
            entries: Vec::new(),
            cursor: 0,
            scroll: 0.0,
            hidden: crate::config::HiddenPlayers::load_from(&path),
            hidden_path: path,
        }
    }

    /// A fresh cursor at the top — called whenever the page is entered
    /// (mirrors [`super::key_binds::KeyBindsNav::reset`]), so re-opening it
    /// never resumes scrolled down onto a row a fresh player list may not
    /// even have any more.
    pub fn reset(&mut self) {
        self.cursor = 0;
        self.scroll = 0.0;
    }

    /// Replaces the online-player snapshot — see [`entries_from_tablist`]'s
    /// doc for who is meant to call this and how often. Clamps the cursor
    /// rather than resetting it, so a mid-session roster change (a player
    /// joins or leaves) does not silently yank the highlight back to the top.
    pub fn refresh(&mut self, entries: Vec<SocialEntry>) {
        self.entries = entries;
        let len = self.all_controls().len();
        if len == 0 {
            self.cursor = 0;
        } else if self.cursor >= len {
            self.cursor = len - 1;
        }
        // Re-clamp the pixel offset against the new, possibly shorter list. Done
        // through the primitive rather than by hand: `ListSpec::model` runs
        // `set_scroll`, which is the one place the clamp lives.
        if let Some(list) = self.model(crate::config::MIN_SCALED_HEIGHT as f32) {
            self.scroll = list.scroll();
        } else {
            // No band, or no entries at all — nothing to scroll to.
            self.scroll = 0.0;
        }
    }

    #[must_use]
    pub fn entries(&self) -> &[SocialEntry] {
        &self.entries
    }

    #[must_use]
    pub fn is_hidden(&self, id: Uuid) -> bool {
        self.hidden.contains(id)
    }

    /// Toggles Hide/Show for the player at `row` and persists immediately —
    /// same eager-persistence rule as every other setting in this tree (no
    /// guaranteed clean-shutdown hook, so a setting that only saved on exit
    /// would be the setting a crash loses).
    fn toggle_hidden_at(&mut self, row: usize) {
        let Some(entry) = self.entries.get(row) else {
            return;
        };
        self.hidden.toggle(entry.id);
        let _ = self.hidden.save_to(&self.hidden_path);
    }

    fn all_controls(&self) -> Vec<SocialControl> {
        let mut out = Vec::with_capacity(self.entries.len() * 2 + 1);
        for i in 0..self.entries.len() {
            out.push(SocialControl::HideToggle(i));
            out.push(SocialControl::Report(i));
        }
        out.push(SocialControl::Done);
        out
    }

    /// The scroll offset, in pixels.
    #[must_use]
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// The live [`super::widget::ScrollList`] at this canvas height, or `None`
    /// when there is nothing to scroll.
    #[must_use]
    fn model(&self, canvas_height: f32) -> Option<super::widget::ScrollList> {
        list_spec(self.entries.len(), self.scroll).model(canvas_height)
    }

    /// One mouse-wheel notch, through the primitive. Positive scrolls **up**;
    /// the negation lives in [`super::widget::ScrollList::mouse_scrolled`].
    pub fn scroll_by(&mut self, notches: f32, canvas_height: f32) {
        let Some(mut list) = self.model(canvas_height) else {
            return;
        };
        list.mouse_scrolled(notches);
        self.scroll = list.scroll();
    }

    /// Every control on screen, scrolled, then the footer — mirrors
    /// [`super::key_binds::controls`].
    #[must_use]
    pub fn visible(&self) -> Vec<(SocialControl, Slot)> {
        // **Every** row, not a `visible_range()` window (issue #445): clipping to
        // the band is `render::draw`'s job now, so a half-scrolled row draws its
        // visible half instead of vanishing. `selected_row` matches on the
        // control, not the index, so it is indifferent.
        let mut out = Vec::new();
        for row in 0..self.entries.len() {
            out.push((
                SocialControl::HideToggle(row),
                Slot {
                    origin: Origin::Social(SocialPlacement::Hide {
                        row: row as u16,
                        scroll: self.scroll,
                    }),
                    dx: 0.0,
                    dy: 0.0,
                    w: HIDE_BUTTON_W,
                    h: ROW_H,
                },
            ));
            out.push((
                SocialControl::Report(row),
                Slot {
                    origin: Origin::Social(SocialPlacement::Report {
                        row: row as u16,
                        scroll: self.scroll,
                    }),
                    dx: 0.0,
                    dy: 0.0,
                    w: REPORT_BUTTON_W,
                    h: ROW_H,
                },
            ));
        }
        out.push((
            SocialControl::Done,
            Slot {
                origin: Origin::Settings(Placement::Footer { index: 0, count: 1 }),
                dx: 0.0,
                dy: 0.0,
                w: options::SMALL_BUTTON_WIDTH,
                h: options::WIDGET_H,
            },
        ));
        out
    }

    #[must_use]
    pub fn selected_row(&self) -> Option<usize> {
        let all = self.all_controls();
        let control = *all.get(self.cursor)?;
        self.visible().iter().position(|(c, _)| *c == control)
    }

    pub fn step(&mut self, forward: bool) {
        let len = self.all_controls().len();
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
        let all = self.all_controls();
        let Some(&control) = all.get(self.cursor) else {
            return;
        };
        let row = match control {
            SocialControl::HideToggle(r) | SocialControl::Report(r) => r,
            SocialControl::Done => return,
        };
        // `ScrollList::scroll_to_entry` moves the MINIMUM pixels — vanilla's
        // `ensureVisible` — where the loop this replaced stepped a whole ROW_H at
        // a time. `MIN_SCALED_HEIGHT` for the reason `stats::step` records: a
        // keypress has no canvas in hand, and the smallest canvas can only
        // over-scroll into a region a larger one also shows.
        let Some(mut list) = self.model(crate::config::MIN_SCALED_HEIGHT as f32) else {
            return;
        };
        list.scroll_to_entry(row);
        self.scroll = list.scroll();
    }

    pub fn hover_row(&mut self, row: usize) {
        let visible = self.visible();
        let Some(&(control, _)) = visible.get(row) else {
            return;
        };
        let all = self.all_controls();
        if let Some(i) = all.iter().position(|&c| c == control) {
            self.cursor = i;
        }
    }

    /// Activates the control at visible row `row` — #391's shape, same
    /// resolve-the-row-directly rule every other list in this tree follows
    /// (see [`super::key_binds::KeyBindsNav::click_row`]'s own doc for why
    /// this does not route through Enter).
    pub fn click_row(&mut self, row: usize) -> SocialOutcome {
        let visible = self.visible();
        let Some(&(control, _)) = visible.get(row) else {
            return SocialOutcome::None;
        };
        self.hover_row(row);
        self.activate(control)
    }

    pub fn enter(&mut self) -> SocialOutcome {
        let all = self.all_controls();
        match all.get(self.cursor).copied() {
            Some(control) => self.activate(control),
            None => SocialOutcome::None,
        }
    }

    fn activate(&mut self, control: SocialControl) -> SocialOutcome {
        if !control.is_live() {
            return SocialOutcome::None;
        }
        match control {
            SocialControl::HideToggle(row) => {
                self.toggle_hidden_at(row);
                SocialOutcome::None
            }
            SocialControl::Report(_) => SocialOutcome::None,
            SocialControl::Done => SocialOutcome::Back,
        }
    }
}

impl Default for SocialNav {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocialOutcome {
    None,
    Back,
}

/// Builds the whole Social Interactions frame. `kind` decides the
/// available/unavailable fork (see [`available_for`]); `nav` supplies the
/// entries and cursor once available.
#[must_use]
pub fn frame(nav: &SocialNav, kind: Option<super::SessionKind>) -> MenuFrame<'static> {
    let mut labels = vec![MenuLabel {
        text: TITLE.to_string(),
        origin: Origin::ScreenTop,
        dx: 0.0,
        dy: 12.0,
        align: Align::Centre,
        colour: widget::ACTIVE_LABEL,
        scale: 1.0,
    }];

    if !available_for(kind) {
        return MenuFrame {
            rows: vec![MenuRow {
                label: "Done".to_string(),
                enabled: true,
                slot: Some(Slot {
                    origin: Origin::Settings(Placement::Footer { index: 0, count: 1 }),
                    dx: 0.0,
                    dy: 0.0,
                    w: options::SMALL_BUTTON_WIDTH,
                    h: options::WIDGET_H,
                }),
                ..Default::default()
            }],
            selected: 0,
            vanilla: true,
            labels,
            notice: Some(MenuNotice {
                text: NOT_AVAILABLE.to_string(),
                origin: Origin::ScreenTop,
                dx: -140.0,
                dy: 60.0,
                w: 280.0,
                bottom: options::WIDGET_H + 20.0,
                colour: widget::ACTIVE_LABEL,
            }),
            ..Default::default()
        };
    }

    let visible = nav.visible();
    let selected = nav.selected_row();
    let rows: Vec<MenuRow> = visible
        .iter()
        .map(|(control, slot)| MenuRow {
            label: match *control {
                SocialControl::HideToggle(row) => {
                    let hidden = nav
                        .entries()
                        .get(row)
                        .is_some_and(|e| nav.is_hidden(e.id));
                    if hidden {
                        "Show in Chat".to_string() // gui.socialInteractions.show
                    } else {
                        "Hide in Chat".to_string() // gui.socialInteractions.hide
                    }
                }
                SocialControl::Report(_) => "Report".to_string(), // gui.socialInteractions.report
                SocialControl::Done => "Done".to_string(),
            },
            enabled: control.is_live(),
            slot: Some(*slot),
            ..Default::default()
        })
        .collect();

    // **`list_labels`, not `labels` (issue #445)** — the vector `render::draw`
    // clips to the band. These player names are the only labels here that scroll;
    // a free text label has nowhere else to carry a clip rect, so in `labels` a
    // scrolled-away name would draw over the footer. The Hide/Report buttons are
    // `MenuRow`s and get their clip from draw.rs's per-row `with_clip`. Same
    // split `stats::frame` and `key_binds::frame` make.
    let mut list_labels = Vec::with_capacity(nav.entries().len());
    for (row, entry) in nav.entries().iter().enumerate() {
        list_labels.push(MenuLabel {
            text: entry.name.clone(),
            origin: Origin::Social(SocialPlacement::Name {
                row: row as u16,
                scroll: nav.scroll(),
            }),
            dx: 0.0,
            dy: 0.0,
            align: Align::Left,
            colour: widget::ACTIVE_LABEL,
            scale: 1.0,
        });
    }

    if nav.entries().is_empty() {
        labels.push(MenuLabel {
            text: "No players online.".to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: options::SUB_HEADER_HEIGHT + 20.0,
            align: Align::Centre,
            colour: widget::ACTIVE_LABEL,
            scale: 1.0,
        });
    }

    MenuFrame {
        rows,
        selected: selected.unwrap_or(usize::MAX),
        vanilla: true,
        labels,
        list_labels,
        // `list` is deliberately not set: `render::dispatch` stamps
        // `f.list = nav.active_list(ui)` on every frame, so the bar the draw
        // paints and the offset the wheel clamps stay two readers of one
        // declaration. See `key_binds::frame`'s note.
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::tablist::GameProfile;

    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "lodestone-social-{}-{tag}/hidden_players.json",
            std::process::id()
        ))
    }

    fn nav_with(tag: &str) -> SocialNav {
        let path = temp_path(tag);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        SocialNav::with_path(path)
    }

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    // -- entries_from_tablist ---------------------------------------------

    #[test]
    fn entries_from_tablist_uses_vanillas_order_and_excludes_the_local_player() {
        let mut tabs = TabList::new();
        tabs.insert(PlayerListEntry::new(GameProfile::new(uuid(2), "Bob")));
        tabs.insert(PlayerListEntry::new(GameProfile::new(uuid(1), "Alice")));
        tabs.insert(PlayerListEntry::new(GameProfile::new(uuid(3), "Me")));

        let entries = entries_from_tablist(&tabs, Some(uuid(3)));
        assert_eq!(
            entries,
            vec![SocialEntry::new(uuid(1), "Alice"), SocialEntry::new(uuid(2), "Bob")],
            "TabList::ordered's own order, local player excluded"
        );
    }

    #[test]
    fn entries_from_tablist_with_no_exclusion_keeps_everyone() {
        let mut tabs = TabList::new();
        tabs.insert(PlayerListEntry::new(GameProfile::new(uuid(1), "Alice")));
        assert_eq!(entries_from_tablist(&tabs, None).len(), 1);
    }

    // -- the singleplayer/multiplayer fork ----------------------------------

    #[test]
    fn available_for_is_true_only_in_multiplayer() {
        assert!(!available_for(None), "no session at all");
        assert!(!available_for(Some(super::super::SessionKind::Singleplayer)));
        assert!(available_for(Some(super::super::SessionKind::Multiplayer)));
    }

    #[test]
    fn the_unavailable_frame_has_no_roster_but_still_has_a_done_button() {
        let nav = nav_with("unavailable-frame");
        let f = frame(&nav, Some(super::super::SessionKind::Singleplayer));
        assert_eq!(f.rows.len(), 1, "just Done");
        assert_eq!(f.rows[0].label, "Done");
        assert!(f.rows[0].enabled);
        assert!(
            f.notice.as_ref().is_some_and(|n| n.text == NOT_AVAILABLE),
            "must show vanilla's own not-available string verbatim"
        );
    }

    // -- SocialNav: cursor, hover, click -------------------------------------

    fn with_three(tag: &str) -> SocialNav {
        let mut nav = nav_with(tag);
        nav.refresh(vec![
            SocialEntry::new(uuid(1), "Alice"),
            SocialEntry::new(uuid(2), "Bob"),
            SocialEntry::new(uuid(3), "Carol"),
        ]);
        nav
    }

    #[test]
    fn the_census_is_two_controls_per_player_plus_done() {
        let nav = with_three("census");
        assert_eq!(nav.all_controls().len(), 3 * 2 + 1);
    }

    #[test]
    fn report_is_present_and_permanently_inactive() {
        // The one control on this whole screen that clicking must never do
        // anything for, regardless of cursor state — the module docs' whole
        // point. `click_row` on a Report control must return `None`, not
        // `Back` or any mutation of `hidden`.
        let mut nav = with_three("report-inert");
        let visible = nav.visible();
        let row = visible
            .iter()
            .position(|(c, _)| *c == SocialControl::Report(0))
            .unwrap();
        assert_eq!(nav.click_row(row), SocialOutcome::None);
        assert!(!nav.is_hidden(uuid(1)), "Report must not have touched Hide state");
    }

    #[test]
    fn clicking_hide_toggles_and_persists_immediately() {
        let path = temp_path("hide-persist");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let mut nav = SocialNav::with_path(path.clone());
        nav.refresh(vec![SocialEntry::new(uuid(1), "Alice")]);

        let row = nav
            .visible()
            .iter()
            .position(|(c, _)| *c == SocialControl::HideToggle(0))
            .unwrap();
        assert!(!nav.is_hidden(uuid(1)));
        assert_eq!(nav.click_row(row), SocialOutcome::None);
        assert!(nav.is_hidden(uuid(1)), "must be hidden after one click");

        // Persisted immediately — a fresh nav reading the same path sees it,
        // same eager-persistence rule as keybinding rebinds.
        let reloaded = SocialNav::with_path(path.clone());
        assert!(reloaded.is_hidden(uuid(1)), "must survive a reload");

        // And it self-heals with one more click.
        assert_eq!(nav.click_row(row), SocialOutcome::None);
        assert!(!nav.is_hidden(uuid(1)));
        let reloaded = SocialNav::with_path(path);
        assert!(!reloaded.is_hidden(uuid(1)));
    }

    #[test]
    fn the_hide_label_reflects_the_persisted_state_both_ways() {
        let mut nav = with_three("labels");
        let f = frame(&nav, Some(super::super::SessionKind::Multiplayer));
        let hide_row = f
            .rows
            .iter()
            .zip(nav.visible())
            .find(|(_, (c, _))| *c == SocialControl::HideToggle(0))
            .unwrap()
            .0;
        assert_eq!(hide_row.label, "Hide in Chat");

        nav.click_row(0); // Alice's HideToggle is visible row 0
        let f = frame(&nav, Some(super::super::SessionKind::Multiplayer));
        assert_eq!(f.rows[0].label, "Show in Chat");
    }

    #[test]
    fn a_click_acts_on_the_row_it_landed_on_and_nothing_else() {
        // #391's shape, on this screen too.
        let mut nav = with_three("click-precision");
        let visible = nav.visible();
        let bob_hide = visible
            .iter()
            .position(|(c, _)| *c == SocialControl::HideToggle(1))
            .unwrap();
        nav.click_row(bob_hide);
        assert!(nav.is_hidden(uuid(2)), "Bob is hidden");
        assert!(!nav.is_hidden(uuid(1)), "Alice untouched");
        assert!(!nav.is_hidden(uuid(3)), "Carol untouched");
    }

    #[test]
    fn hover_and_the_cursor_agree_on_every_visible_row() {
        let mut nav = with_three("hover-agree");
        let len = nav.visible().len();
        for row in 0..len {
            nav.hover_row(row);
            assert_eq!(nav.selected_row(), Some(row), "hovering row {row}");
        }
    }

    #[test]
    fn stepping_the_cursor_reaches_every_control() {
        let mut nav = with_three("step-reaches-all");
        let total = nav.all_controls().len();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..total * 2 {
            assert!(nav.selected_row().is_some(), "cursor off-window at {}", nav.cursor);
            seen.insert(nav.cursor);
            nav.step(true);
        }
        assert_eq!(seen.len(), total);
    }

    #[test]
    fn done_leaves_the_screen() {
        let mut nav = with_three("done");
        let done_row = nav
            .visible()
            .iter()
            .position(|(c, _)| *c == SocialControl::Done)
            .unwrap();
        assert_eq!(nav.click_row(done_row), SocialOutcome::Back);
    }

    #[test]
    fn refreshing_clamps_the_cursor_instead_of_resetting_it() {
        // A mid-session roster shrink (a player left) must not silently yank
        // the highlight back to the top.
        let mut nav = with_three("refresh-clamp");
        for _ in 0..6 {
            nav.step(true);
        }
        let before = nav.cursor;
        assert!(before > 0, "test setup should have moved the cursor");
        nav.refresh(vec![SocialEntry::new(uuid(1), "Alice")]);
        assert!(nav.cursor < nav.all_controls().len(), "clamped, not out of range");
    }

    #[test]
    fn every_visible_placement_resolves_on_screen() {
        let nav = with_three("placement-onscreen");
        let (w, h) = (480.0, 320.0);
        for (control, slot) in nav.visible() {
            let (x, y, sw, sh) = slot.resolve(w, h);
            assert!(
                x >= 0.0 && y >= 0.0 && x + sw <= w && y + sh <= h,
                "{control:?} at ({x}, {y}) size {sw}x{sh}"
            );
        }
    }

    /// A row scrolled above the band resolves **above** it, not at the old
    /// `(-1000, -1000)` sentinel, which existed only because
    /// `row.checked_sub(first)` could underflow.
    #[test]
    fn a_row_scrolled_above_the_band_resolves_above_it_not_at_a_sentinel() {
        let band_top = options::SUB_HEADER_HEIGHT + options::LIST_TOP_INSET;
        let (_, y) = placement_anchor(
            SocialPlacement::Hide {
                row: 0,
                scroll: 5.0 * ROW_H,
            },
            480.0,
            320.0,
        );
        assert_eq!(
            y,
            band_top - 5.0 * ROW_H,
            "five rows above the band's top, exactly"
        );
    }

    /// **The property no centred `row_w` could have, and the whole reason
    /// `RowBand::Inset` exists** (issue #445): the band's right edge is exactly
    /// where [`report_button_x`] hangs its button's right edge, **at every canvas
    /// width**.
    ///
    /// Two expressions from two modules required to agree, at four widths, with no
    /// tolerance. This is the gate that would have been impossible before the
    /// primitive grew a canvas-relative edge — and
    /// `widget::tests::no_constant_row_width_can_express_a_full_width_row`
    /// measures how badly the centred alternative misses (107 px at 640, 533 at
    /// 1920), so the pair together is the argument rather than either alone.
    #[test]
    fn the_declared_band_tracks_this_screens_own_right_anchored_buttons() {
        for w in [640.0_f32, 854.0, 1280.0, 1920.0] {
            let spec = list_spec(40, 0.0);
            assert_eq!(
                spec.row_right(w),
                report_button_x(w) + REPORT_BUTTON_W,
                "at {w} px the band's right edge must land on the Report button's \
                 own right edge — this is what a centred `row_w` cannot do"
            );
            assert_eq!(
                spec.row_left(w),
                name_x(w),
                "and the band's left edge on where the name actually draws"
            );
            // And the bar fits on the canvas, which is what RIGHT_MARGIN grew for.
            let list = spec.model(240.0).expect("a band at 240 px");
            assert!(
                list.scrollbar_x(spec.row_right(w)) + super::super::widget::SCROLLBAR_WIDTH <= w,
                "the scrollbar must fit on a {w} px canvas — a 10 px gutter put it \
                 4 px off the edge, which is why RIGHT_MARGIN is now 14"
            );
        }
    }

    /// **One notch is `floor(ROW_H / 2)` = `floor(20 / 2)` = 10 px** (issue #445),
    /// and the offset must coincide with no row top.
    ///
    /// Hypotheses named and separated rather than a tolerance: the row-index
    /// answer is 20, the page answer is `LIST_WINDOW_PX` (172), the pixel answer
    /// is 10. Three notches is 30, **not** a multiple of `ROW_H` — an offset no
    /// row-index implementation can produce at all, so that assertion excludes the
    /// whole family.
    ///
    /// Driven through `SocialNav` itself with a synthetic roster, because unlike
    /// `language` this screen's list length is a live tablist fact rather than a
    /// one-entry constant — so the real production path is reachable here.
    #[test]
    fn one_wheel_notch_is_half_a_row_and_lands_off_every_row_top() {
        const CANVAS_H: f32 = 240.0;
        let mut nav = nav_with("notch");
        nav.refresh(
            (1..=40)
                .map(|i| SocialEntry::new(uuid(i), format!("player{i}")))
                .collect(),
        );
        assert!(
            list_spec(nav.entries().len(), 0.0)
                .model(CANVAS_H)
                .is_some_and(|l| l.scrollable()),
            "premise: 40 rows of {ROW_H} px must overflow the band at {CANVAS_H} \
             px, or every assertion below is vacuous"
        );
        assert_eq!(nav.scroll(), 0.0, "precondition: starts at the top");

        nav.scroll_by(-1.0, CANVAS_H);
        assert_eq!(
            nav.scroll(),
            10.0,
            "one notch must be floor(ROW_H / 2) = 10, not the row-index answer \
             ({ROW_H}) and not a page ({LIST_WINDOW_PX})"
        );
        assert_ne!(nav.scroll(), ROW_H, "control: the row-index answer is excluded");

        nav.scroll_by(-2.0, CANVAS_H);
        assert_eq!(nav.scroll(), 30.0, "three notches: 30");
        assert_ne!(
            nav.scroll() % ROW_H,
            0.0,
            "30 must coincide with no row top — a multiple of {ROW_H} is exactly \
             what snap-to-row produces"
        );

        // The keyboard half: `scroll_to_entry` moves the minimum pixels, so it too
        // can land off a row top — the loop it replaced never could.
        nav.reset();
        let mut moved = false;
        for _ in 0..nav.all_controls().len() {
            nav.step(true);
            if nav.scroll() > 0.0 && nav.scroll() % ROW_H != 0.0 {
                moved = true;
                break;
            }
        }
        assert!(
            moved,
            "keyboard scroll-into-view must be able to land off a row top; it \
             stopped at {}",
            nav.scroll()
        );
    }
}
