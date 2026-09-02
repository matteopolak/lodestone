//! The Spectator Menu — vanilla's own spectator menu/GUI classes,
//! opened by a hotbar-number key while in spectator mode. This is the
//! `TeleportToEntity` remainder: `ClientAction::SpectatorAction`
//! already had a real producer (`Sim::begin_attack_live`'s
//! left-click-while-spectating arm); this module is `TeleportToEntity`'s.
//!
//! ## The real trigger, corrected once already
//!
//! A prior pass on this issue first assumed the tab list's own click was the
//! trigger and found that wrong: `PlayerTabOverlay`/`ClientTabListScreen`
//! has **no click handling anywhere** in the decompile — it is a pure
//! readout. The sole real client-side path to
//! [`ClientAction::TeleportToEntity`](lodestone_model::ClientAction::TeleportToEntity)
//! is `PlayerMenuItem.selectItem`, reached only through the dedicated
//! Spectator Menu. Bolting a teleport action onto the existing
//! [`super::social`] screen (which *does* have real player rows and click
//! handling) was considered and rejected for the same reason a prior pass on
//! this issue already gave: it would fabricate a vanilla behaviour that does
//! not exist, not simplify a real one.
//!
//! ## What it is
//!
//! [`SpectatorMenuState`] holds this frame's roster (refreshed every frame
//! while connected, the same live-refresh shape [`super::social::SocialNav`]
//! uses for its own roster) and which team category, if any, is expanded.
//! [`spectator_menu_entries`] is the pure fold from a live [`TabList`] +
//! [`Scoreboard`] snapshot into the row list — unit-testable with no live
//! session, the same shape [`super::social::entries_from_tablist`] is.
//!
//! ## What is deliberately simplified, named rather than hidden
//!
//! - **A scrolling vertical list, not vanilla's paginated bottom row of
//!   icon slots.** `SpectatorMenu`'s real layout is nine icon cells with
//!   Next/Previous-page arrows (`SpectatorMenu.NUM_ROWS`); this reuses the
//!   list/row machinery every other overlay screen in this crate already has
//!   (`MenuRow`/[`super::render::origin::Slot`], the same click-hit-test
//!   system `book_edit`/`social` use) rather than a bespoke horizontal
//!   icon-slot layout. The entries and the wire behaviour are real; only the
//!   geometry differs.
//! - **A placeholder head icon, not a real per-player skin face.** Every row
//!   draws [`super::render::favicon::default_head_icon`] — the same
//!   fallback the account list uses for a skin that has not resolved yet.
//!   `favicon`'s own doc already names swapping this for a real
//!   `head_mosaic` slice of a fetched skin as the anticipated next step;
//!   this module does not yet resolve `crate::remote_skins` per row.
//! - **Team grouping without vanilla's exact `TeleportToTeam` construction.**
//!   Any team with **two or more** currently-listed members becomes one
//!   "Team Teleport" category row (selecting it expands to that team's
//!   members); every other listed player (no team, or a team of one) is a
//!   flat "Teleport to Player" row directly in the root list — the same two
//!   categories the issue's own research names, without reimplementing
//!   `SpectatorMenu`'s exact category-construction algorithm.
//! - **Opened by any of the 1-9 hotbar keys**, not vanilla's per-slot
//!   category binding (vanilla's own spectator-gui hotbar-selected handling reopens
//!   whichever category slot `slot` was last bound to) — this client has
//!   only one category tree to open, so every key opens the same root.
//! - **No scrolling.** A root or expanded list past
//!   [`MAX_VISIBLE_ROWS`] is truncated, with a message naming how many rows
//!   are hidden, rather than silently dropping them or drawing off-canvas,
//!   unclickable rows.

use uuid::Uuid;

use lodestone_game::scoreboard::Scoreboard;
use lodestone_game::tablist::{PlayerListEntry, TabList};

/// Cap on how many rows [`SpectatorMenuState::visible`] returns — see the
/// module doc's "No scrolling" note.
pub const MAX_VISIBLE_ROWS: usize = 8;

/// One player entry — just enough to draw a row and send the action. Not
/// [`PlayerListEntry`] directly, the same narrowing
/// [`super::social::SocialEntry`] already does for the identical reason:
/// this module needs `id` and a display name and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpectatorMenuPlayer {
    pub id: Uuid,
    pub name: String,
}

/// One selectable row at the menu's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpectatorMenuEntry {
    /// A "Team Teleport" category — a team with two or more currently-listed
    /// members. Selecting it expands to [`SpectatorMenuState::expanded`].
    Team {
        /// The team's internal name (`Team::name`) — the stable key used to
        /// keep a category expanded across a roster refresh.
        name: String,
        /// The team's display text, already flattened to plain text
        /// (`Team::display_name.to_plain_string()`).
        label: String,
        members: Vec<SpectatorMenuPlayer>,
    },
    /// A "Teleport to Player" flat entry — a player with no team, or on a
    /// team of exactly one. Selecting it sends
    /// [`ClientAction::TeleportToEntity`](lodestone_model::ClientAction::TeleportToEntity)
    /// and closes the menu.
    Player(SpectatorMenuPlayer),
}

/// Builds the root entry list from a snapshot of the tab list and
/// scoreboard — pure and unit-testable without a live session, the same
/// shape [`super::social::entries_from_tablist`] uses. `exclude` is the
/// local player's own uuid (vanilla never lists you against yourself,
/// matching [`entries_from_tablist`](super::social::entries_from_tablist)).
///
/// A team with fewer than two currently-listed members does not become a
/// category — its lone member (if any) folds into the flat player list
/// instead, matching the module doc's "What is deliberately simplified"
/// note.
#[must_use]
pub fn spectator_menu_entries(
    tab_list: &TabList,
    scoreboard: &Scoreboard,
    exclude: Option<Uuid>,
) -> Vec<SpectatorMenuEntry> {
    use std::collections::BTreeMap;

    let mut by_team: BTreeMap<String, (String, Vec<SpectatorMenuPlayer>)> = BTreeMap::new();
    let mut unteamed = Vec::new();

    for entry in tab_list.ordered() {
        let e: &PlayerListEntry = entry;
        if Some(e.profile.id) == exclude {
            continue;
        }
        let player = SpectatorMenuPlayer {
            id: e.profile.id,
            name: e.profile.name.clone(),
        };
        match scoreboard.team_of(&e.profile.name) {
            Some(team) => {
                by_team
                    .entry(team.name.clone())
                    .or_insert_with(|| (team.display_name.to_plain_string(), Vec::new()))
                    .1
                    .push(player);
            }
            None => unteamed.push(player),
        }
    }

    let mut out = Vec::new();
    for (name, (label, members)) in by_team {
        if members.len() >= 2 {
            out.push(SpectatorMenuEntry::Team {
                name,
                label,
                members,
            });
        } else {
            unteamed.extend(members);
        }
    }
    out.extend(unteamed.into_iter().map(SpectatorMenuEntry::Player));
    out
}

/// What activating a row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpectatorMenuOutcome {
    /// Nothing to send — expanding/collapsing a category, or a click outside
    /// any real row.
    None,
    /// Send `ClientAction::TeleportToEntity { target }` and close the menu.
    Teleport(Uuid),
}

/// One row as the renderer sees it — [`SpectatorMenuState::visible`]'s
/// output type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpectatorMenuRow<'a> {
    /// Row 0 of an expanded category: return to the root list.
    Back,
    /// A root-level team category, with its member count.
    Team { label: &'a str, count: usize },
    /// A player — root-level (no/singleton team) or a member of the
    /// currently-expanded category.
    Player(&'a SpectatorMenuPlayer),
}

/// Live state for one open (or closed-but-refreshed) spectator menu. Kept
/// live-refreshed every frame while connected (mirroring
/// [`super::social::SocialNav`]'s own roster field) rather than constructed
/// only at open time, so the list the player sees the instant they open the
/// menu is never one frame stale.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpectatorMenuState {
    /// This frame's root categories/players.
    root: Vec<SpectatorMenuEntry>,
    /// Index into [`Self::root`] of the currently-expanded team category, or
    /// `None` at the root view.
    expanded: Option<usize>,
    /// The row index the cursor is over, in the *currently visible* list
    /// ([`Self::visible`]) — the same "mouse highlight only, no keyboard row
    /// cursor" shape [`super::book_edit::BookEditState::hovered`] documents.
    pub hovered: Option<usize>,
}

impl SpectatorMenuState {
    /// Replace this frame's roster. Keeps an expanded category open across
    /// the refresh where the same team (by internal name) is still present
    /// with two or more members; collapses back to the root otherwise —
    /// e.g. the last other member of an expanded team left, or the roster
    /// reordered such that the team no longer resolves.
    pub fn refresh(&mut self, root: Vec<SpectatorMenuEntry>) {
        if let Some(i) = self.expanded {
            let name = match self.root.get(i) {
                Some(SpectatorMenuEntry::Team { name, .. }) => Some(name.clone()),
                _ => None,
            };
            self.expanded = name.and_then(|name| {
                root.iter()
                    .position(|e| matches!(e, SpectatorMenuEntry::Team { name: n, .. } if *n == name))
            });
        }
        self.root = root;
    }

    /// Reset to the root view with nothing hovered — called on open, so a
    /// menu closed mid-category-browse does not reopen already-expanded.
    pub fn reset_view(&mut self) {
        self.expanded = None;
        self.hovered = None;
    }

    #[must_use]
    pub fn root(&self) -> &[SpectatorMenuEntry] {
        &self.root
    }

    /// The currently-expanded team's `(label, members)`, or `None` at the
    /// root view.
    #[must_use]
    pub fn expanded_team(&self) -> Option<(&str, &[SpectatorMenuPlayer])> {
        match self.expanded.and_then(|i| self.root.get(i)) {
            Some(SpectatorMenuEntry::Team { label, members, .. }) => {
                Some((label.as_str(), members.as_slice()))
            }
            _ => None,
        }
    }

    /// The rows to draw and hit-test this frame, capped at
    /// [`MAX_VISIBLE_ROWS`] — see the module doc's "No scrolling" note.
    #[must_use]
    pub fn visible(&self) -> Vec<SpectatorMenuRow<'_>> {
        let mut rows = if let Some((_, members)) = self.expanded_team() {
            let mut rows = vec![SpectatorMenuRow::Back];
            rows.extend(members.iter().map(SpectatorMenuRow::Player));
            rows
        } else {
            self.root
                .iter()
                .map(|e| match e {
                    SpectatorMenuEntry::Team { label, members, .. } => SpectatorMenuRow::Team {
                        label: label.as_str(),
                        count: members.len(),
                    },
                    SpectatorMenuEntry::Player(p) => SpectatorMenuRow::Player(p),
                })
                .collect()
        };
        rows.truncate(MAX_VISIBLE_ROWS);
        rows
    }

    /// How many rows [`Self::visible`] had to drop off the end this frame —
    /// what a "N more not shown" message reads from.
    #[must_use]
    pub fn hidden_row_count(&self) -> usize {
        let total = if let Some((_, members)) = self.expanded_team() {
            members.len() + 1
        } else {
            self.root.len()
        };
        total.saturating_sub(MAX_VISIBLE_ROWS)
    }

    /// What clicking row `row` (in [`Self::visible`]'s index space) does.
    /// Note this does **not** clamp to [`MAX_VISIBLE_ROWS`] itself — a row
    /// index past the visible cap simply resolves to nothing, the same as
    /// any other out-of-range row.
    pub fn activate(&mut self, row: usize) -> SpectatorMenuOutcome {
        if let Some(i) = self.expanded {
            let members_len = match self.root.get(i) {
                Some(SpectatorMenuEntry::Team { members, .. }) => members.len(),
                _ => {
                    // The expanded team vanished from under us (a `refresh`
                    // should already have caught this, but resolve safely
                    // regardless of ordering).
                    self.expanded = None;
                    return SpectatorMenuOutcome::None;
                }
            };
            if row == 0 {
                self.expanded = None;
                return SpectatorMenuOutcome::None;
            }
            let member_idx = row - 1;
            if member_idx >= members_len || member_idx >= MAX_VISIBLE_ROWS.saturating_sub(1) {
                return SpectatorMenuOutcome::None;
            }
            let Some(SpectatorMenuEntry::Team { members, .. }) = self.root.get(i) else {
                return SpectatorMenuOutcome::None;
            };
            return SpectatorMenuOutcome::Teleport(members[member_idx].id);
        }
        if row >= MAX_VISIBLE_ROWS {
            return SpectatorMenuOutcome::None;
        }
        match self.root.get(row) {
            Some(SpectatorMenuEntry::Team { .. }) => {
                self.expanded = Some(row);
                SpectatorMenuOutcome::None
            }
            Some(SpectatorMenuEntry::Player(p)) => SpectatorMenuOutcome::Teleport(p.id),
            None => SpectatorMenuOutcome::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::scoreboard::Team;
    use lodestone_game::tablist::GameProfile;

    fn entry(id: Uuid, name: &str) -> PlayerListEntry {
        PlayerListEntry::new(GameProfile::new(id, name))
    }

    fn team(name: &str, members: &[&str]) -> Team {
        Team {
            members: members.iter().map(|s| s.to_string()).collect(),
            ..Team::new(name)
        }
    }

    /// A team with two or more listed members becomes a `Team` category; a
    /// team of one folds into the flat player list, matching the module
    /// doc's own simplification note.
    #[test]
    fn a_two_member_team_becomes_a_category_and_a_solo_team_does_not() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let mut tab_list = TabList::default();
        tab_list.insert(entry(a, "Alice"));
        tab_list.insert(entry(b, "Bob"));
        tab_list.insert(entry(c, "Solo"));

        let mut board = Scoreboard::new();
        board.add_team(team("red", &["Alice", "Bob"]));
        board.add_team(team("blue", &["Solo"]));

        let entries = spectator_menu_entries(&tab_list, &board, None);

        let team_entries: Vec<_> = entries
            .iter()
            .filter(|e| matches!(e, SpectatorMenuEntry::Team { .. }))
            .collect();
        assert_eq!(team_entries.len(), 1, "exactly the two-member team: {entries:?}");
        let SpectatorMenuEntry::Team { members, .. } = team_entries[0] else {
            unreachable!()
        };
        assert_eq!(members.len(), 2);

        let solo_is_flat = entries
            .iter()
            .any(|e| matches!(e, SpectatorMenuEntry::Player(p) if p.name == "Solo"));
        assert!(solo_is_flat, "a one-member team must fold into the flat list: {entries:?}");
    }

    /// The local player is never listed against themselves.
    #[test]
    fn the_excluded_uuid_is_never_listed() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let mut tab_list = TabList::default();
        tab_list.insert(entry(a, "Alice"));
        tab_list.insert(entry(b, "Bob"));
        let board = Scoreboard::new();

        let entries = spectator_menu_entries(&tab_list, &board, Some(a));
        assert_eq!(entries.len(), 1);
        assert!(matches!(&entries[0], SpectatorMenuEntry::Player(p) if p.id == b));
    }

    /// A player with no team goes straight to the flat list.
    #[test]
    fn an_unteamed_player_is_a_flat_entry() {
        let a = Uuid::from_u128(1);
        let mut tab_list = TabList::default();
        tab_list.insert(entry(a, "Alice"));
        let board = Scoreboard::new();

        let entries = spectator_menu_entries(&tab_list, &board, None);
        assert_eq!(entries, vec![SpectatorMenuEntry::Player(SpectatorMenuPlayer {
            id: a,
            name: "Alice".to_string(),
        })]);
    }

    /// Selecting a team category expands it without sending anything;
    /// selecting a player inside it sends a teleport and does not disturb
    /// `root`. Selecting row 0 (Back) collapses back to the root.
    #[test]
    fn expanding_a_team_then_selecting_a_member_teleports_and_back_collapses() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let mut state = SpectatorMenuState::default();
        state.refresh(vec![SpectatorMenuEntry::Team {
            name: "red".to_string(),
            label: "Red Team".to_string(),
            members: vec![
                SpectatorMenuPlayer { id: a, name: "Alice".to_string() },
                SpectatorMenuPlayer { id: b, name: "Bob".to_string() },
            ],
        }]);

        assert_eq!(state.activate(0), SpectatorMenuOutcome::None, "expands, sends nothing");
        assert!(state.expanded_team().is_some());

        // Row 0 is now Back, row 1 is Alice, row 2 is Bob.
        assert_eq!(state.activate(1), SpectatorMenuOutcome::Teleport(a));
        // Teleporting does not itself collapse the category (the caller
        // closes the whole menu on a real teleport — see `nav.rs`).
        assert!(state.expanded_team().is_some());

        assert_eq!(state.activate(0), SpectatorMenuOutcome::None, "Back collapses");
        assert!(state.expanded_team().is_none());
    }

    /// Selecting a root-level player entry teleports directly, with no
    /// expand step.
    #[test]
    fn a_root_level_player_teleports_directly() {
        let a = Uuid::from_u128(1);
        let mut state = SpectatorMenuState::default();
        state.refresh(vec![SpectatorMenuEntry::Player(SpectatorMenuPlayer {
            id: a,
            name: "Alice".to_string(),
        })]);
        assert_eq!(state.activate(0), SpectatorMenuOutcome::Teleport(a));
    }

    /// An expanded team that loses its second member across a refresh
    /// collapses back to the root rather than pointing at a stale or
    /// wrong-shaped entry.
    #[test]
    fn refresh_collapses_an_expanded_team_that_no_longer_qualifies() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let mut state = SpectatorMenuState::default();
        state.refresh(vec![SpectatorMenuEntry::Team {
            name: "red".to_string(),
            label: "Red Team".to_string(),
            members: vec![
                SpectatorMenuPlayer { id: a, name: "Alice".to_string() },
                SpectatorMenuPlayer { id: b, name: "Bob".to_string() },
            ],
        }]);
        state.activate(0);
        assert!(state.expanded_team().is_some());

        // Bob left; "red" no longer qualifies as a category (see
        // `spectator_menu_entries`) and the refreshed root has no `Team`
        // entry named "red" at all.
        state.refresh(vec![SpectatorMenuEntry::Player(SpectatorMenuPlayer {
            id: a,
            name: "Alice".to_string(),
        })]);
        assert!(
            state.expanded_team().is_none(),
            "a refresh must collapse an expanded category that no longer exists"
        );
    }

    /// The same team, same name, surviving a refresh (e.g. a third member
    /// joined) stays expanded rather than bouncing the player back to the
    /// root on every roster tick.
    #[test]
    fn refresh_keeps_the_same_team_expanded_across_a_refresh() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let mut state = SpectatorMenuState::default();
        state.refresh(vec![SpectatorMenuEntry::Team {
            name: "red".to_string(),
            label: "Red Team".to_string(),
            members: vec![
                SpectatorMenuPlayer { id: a, name: "Alice".to_string() },
                SpectatorMenuPlayer { id: b, name: "Bob".to_string() },
            ],
        }]);
        state.activate(0);

        state.refresh(vec![SpectatorMenuEntry::Team {
            name: "red".to_string(),
            label: "Red Team".to_string(),
            members: vec![
                SpectatorMenuPlayer { id: a, name: "Alice".to_string() },
                SpectatorMenuPlayer { id: b, name: "Bob".to_string() },
                SpectatorMenuPlayer { id: c, name: "Cara".to_string() },
            ],
        }]);
        let (label, members) = state.expanded_team().expect("still expanded");
        assert_eq!(label, "Red Team");
        assert_eq!(members.len(), 3);
    }

    /// A row past [`MAX_VISIBLE_ROWS`] resolves to nothing — the "no
    /// scrolling" simplification must not let a click past the cap reach an
    /// entry the player never saw.
    #[test]
    fn a_row_past_the_visible_cap_does_nothing() {
        let mut state = SpectatorMenuState::default();
        let root: Vec<_> = (0..MAX_VISIBLE_ROWS + 3)
            .map(|i| {
                SpectatorMenuEntry::Player(SpectatorMenuPlayer {
                    id: Uuid::from_u128(i as u128),
                    name: format!("p{i}"),
                })
            })
            .collect();
        state.refresh(root);
        assert_eq!(state.visible().len(), MAX_VISIBLE_ROWS);
        assert_eq!(state.hidden_row_count(), 3);
        assert_eq!(state.activate(MAX_VISIBLE_ROWS), SpectatorMenuOutcome::None);
    }
}
