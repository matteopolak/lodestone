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
/// shape: every content-list variant shares `{row, first}`, only the x differs.
///
/// **Still a row index, and issue #445 records why.** Converting it to a pixel
/// offset is blocked on a `ListSpec` change, not on this screen: this list's
/// rows are **full-width and left-anchored** (`name_x` is a flat
/// `NAME_LEFT_INSET`, the buttons hang off `width - RIGHT_MARGIN`), while
/// `ListSpec::row_left` is `floor(width / 2) - floor(row_w / 2)` — a *centred,
/// fixed-width* row. There is no constant `row_w` that makes `row_right` land
/// in this screen's right margin at every canvas width, so the scrollbar the
/// spec exists to place cannot be positioned from it. The primitive needs a
/// canvas-relative row edge before this screen, `key_binds` or `language` can
/// adopt it honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocialPlacement {
    Name { row: u16, first: u16 },
    Hide { row: u16, first: u16 },
    Report { row: u16, first: u16 },
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
const RIGHT_MARGIN: f32 = 10.0;
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
    let (row, first) = match placement {
        SocialPlacement::Name { row, first }
        | SocialPlacement::Hide { row, first }
        | SocialPlacement::Report { row, first } => (row, first),
    };
    let Some(index) = row.checked_sub(first) else {
        // Off-window: the anti-island sentinel every other placement in this
        // tree uses (see `super::options::placement_anchor`,
        // `super::key_binds::placement_anchor`).
        return (-1000.0, -1000.0);
    };
    let row_y = options::SUB_HEADER_HEIGHT + options::LIST_TOP_INSET + f32::from(index) * ROW_H;
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocialNav {
    entries: Vec<SocialEntry>,
    cursor: usize,
    first: usize,
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
            first: 0,
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
        self.first = 0;
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
        self.first = self.first.min(self.entries.len());
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

    fn visible_range(&self) -> std::ops::Range<usize> {
        let len = self.entries.len();
        let end = (self.first + visible_rows_len()).min(len);
        self.first.min(len)..end
    }

    /// Every control on screen, scrolled, then the footer — mirrors
    /// [`super::key_binds::controls`].
    #[must_use]
    pub fn visible(&self) -> Vec<(SocialControl, Slot)> {
        let mut out = Vec::new();
        for row in self.visible_range() {
            out.push((
                SocialControl::HideToggle(row),
                Slot {
                    origin: Origin::Social(SocialPlacement::Hide {
                        row: row as u16,
                        first: self.first as u16,
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
                        first: self.first as u16,
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
        if row < self.first {
            self.first = row;
            return;
        }
        while !self.visible_range().contains(&row) {
            if self.first + 1 >= self.entries.len() {
                break;
            }
            self.first += 1;
        }
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

    for row in nav.visible_range() {
        if let Some(entry) = nav.entries().get(row) {
            labels.push(MenuLabel {
                text: entry.name.clone(),
                origin: Origin::Social(SocialPlacement::Name {
                    row: row as u16,
                    first: nav.first as u16,
                }),
                dx: 0.0,
                dy: 0.0,
                align: Align::Left,
                colour: widget::ACTIVE_LABEL,
                scale: 1.0,
            });
        }
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

    #[test]
    fn placement_off_the_window_is_the_anti_island_sentinel() {
        let (x, y) = placement_anchor(SocialPlacement::Hide { row: 0, first: 5 }, 480.0, 320.0);
        assert!(x < 0.0 && y < 0.0, "off-canvas sentinel, not a wrapped u16");
    }
}
