//! The spectator HUD selector: live player/team targets, nine paginated slots,
//! two-step activation, and an idle fade. It never owns a screen or cursor.

use uuid::Uuid;

use lodestone_game::scoreboard::Scoreboard;
use lodestone_game::tablist::{PlayerListEntry, TabList};

/// The identity and display name needed for a teleport target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpectatorMenuPlayer {
    pub id: Uuid,
    pub name: String,
}

/// A group of targets folded from the live player list and scoreboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpectatorMenuEntry {
    /// A team with at least one eligible connected player.
    Team {
        /// Stable category key across roster refreshes.
        name: String,
        /// Plain-text category label.
        label: String,
        members: Vec<SpectatorMenuPlayer>,
    },
    /// An eligible connected player with no team.
    Player(SpectatorMenuPlayer),
}

/// Live eligible targets, grouped by scoreboard team. Spectators, the local
/// player, and profiles without a wire UUID cannot be teleport destinations.
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
        let Some(id) = e.profile.id else {
            continue;
        };
        if Some(id) == exclude || e.game_mode == lodestone_model::GameMode::Spectator {
            continue;
        }
        let player = SpectatorMenuPlayer {
            id,
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
        out.push(SpectatorMenuEntry::Team {
            name,
            label,
            members,
        });
    }
    out.extend(unteamed.into_iter().map(SpectatorMenuEntry::Player));
    out
}

/// A selection sends a teleport only after the same slot is activated twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpectatorMenuOutcome {
    None,
    Teleport(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Category {
    Root,
    Players,
    Teams,
    Team(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ItemAction {
    Category(Category),
    Teleport(Uuid),
    Previous,
    Next,
    Close,
}

/// One cell of the spectator HUD bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpectatorSlot {
    pub name: String,
    pub sprite: &'static str,
    pub enabled: bool,
    action: ItemAction,
}

/// Owned draw projection; roster refreshes cannot invalidate its selected labels.
#[derive(Debug, Clone, PartialEq)]
pub struct SpectatorHotbarView {
    pub slots: [Option<SpectatorSlot>; 9],
    pub selected: Option<usize>,
    pub prompt: String,
    pub alpha: f32,
}

/// Live roster plus the transient HUD selection. Opening it never changes screens.
#[derive(Debug, Clone, PartialEq)]
pub struct SpectatorMenuState {
    root: Vec<SpectatorMenuEntry>,
    category: Category,
    page: usize,
    selected: Option<usize>,
    last_input: Option<f64>,
}

impl Default for SpectatorMenuState {
    fn default() -> Self {
        Self {
            root: Vec::new(),
            category: Category::Root,
            page: 0,
            selected: None,
            last_input: None,
        }
    }
}

impl SpectatorMenuState {
    pub fn refresh(&mut self, root: Vec<SpectatorMenuEntry>) {
        self.root = root;
        if let Category::Team(name) = &self.category
            && !self.root.iter().any(|entry| {
                matches!(entry, SpectatorMenuEntry::Team { name: candidate, .. } if candidate == name)
            })
        {
            self.category = Category::Teams;
            self.page = 0;
            self.selected = None;
        }
        if self.page > 0 && self.page * 6 >= self.items().len() {
            self.page = 0;
            self.selected = None;
        }
    }

    pub fn close(&mut self) {
        self.last_input = None;
        self.category = Category::Root;
        self.page = 0;
        self.selected = None;
    }

    #[must_use]
    pub fn active(&self, now: f64) -> bool {
        self.last_input.is_some_and(|last| now - last < 5.0)
    }

    /// A number key supplies a slot; the pick-item key activates the selection.
    /// The first press opens the root with no selection.
    pub fn select(&mut self, slot: Option<usize>, now: f64) -> SpectatorMenuOutcome {
        if !self.active(now) {
            self.close();
            self.last_input = Some(now);
            return SpectatorMenuOutcome::None;
        }
        self.last_input = Some(now);
        let Some(slot) = slot.or(self.selected).filter(|slot| *slot < 9) else {
            return SpectatorMenuOutcome::None;
        };
        let Some(item) = self.slot(slot) else {
            return SpectatorMenuOutcome::None;
        };
        if self.selected != Some(slot) || !item.enabled {
            self.selected = Some(slot);
            return SpectatorMenuOutcome::None;
        }
        match item.action {
            ItemAction::Category(category) => {
                self.category = category;
                self.page = 0;
                self.selected = None;
            }
            ItemAction::Teleport(target) => return SpectatorMenuOutcome::Teleport(target),
            ItemAction::Previous => self.page = self.page.saturating_sub(1),
            ItemAction::Next => self.page += 1,
            ItemAction::Close => self.close(),
        }
        SpectatorMenuOutcome::None
    }

    /// Wheel navigation skips disabled and empty cells without activating a target.
    pub fn scroll(&mut self, direction: i32, now: f64) {
        if !self.active(now) || direction == 0 {
            return;
        }
        let direction = direction.signum();
        let items = self.items();
        let mut slot = self.selected.map_or(-1, |slot| slot as i32) + direction;
        while (0..9).contains(&slot) {
            if self.slot_from(&items, slot as usize).is_some_and(|item| item.enabled) {
                self.selected = Some(slot as usize);
                self.last_input = Some(now);
                return;
            }
            slot += direction;
        }
    }

    #[must_use]
    pub fn view(&self, now: f64) -> Option<SpectatorHotbarView> {
        if !self.active(now) {
            return None;
        }
        let alpha = ((self.last_input? + 5.0 - now) / 2.0).clamp(0.0, 1.0) as f32;
        let items = self.items();
        let slots = std::array::from_fn(|slot| self.slot_from(&items, slot));
        let prompt = self.selected.and_then(|slot| slots.get(slot)?.as_ref())
            .map(|item| item.name.clone())
            .unwrap_or_else(|| match &self.category {
                Category::Root => "Select a category",
                Category::Teams => "Select a team to teleport to",
                Category::Players | Category::Team(_) => "Select a player to teleport to",
            }.to_string());
        Some(SpectatorHotbarView {
            slots,
            selected: self.selected,
            prompt,
            alpha,
        })
    }

    fn slot(&self, slot: usize) -> Option<SpectatorSlot> {
        self.slot_from(&self.items(), slot)
    }

    fn slot_from(&self, items: &[SpectatorSlot], slot: usize) -> Option<SpectatorSlot> {
        match slot {
            0 if self.page > 0 => Some(Self::item(
                "Previous Page", "spectator/scroll_left", true, ItemAction::Previous,
            )),
            7 => Some(Self::item(
                "Next Page", "spectator/scroll_right", self.page * 6 + 7 < items.len(), ItemAction::Next,
            )),
            8 => Some(Self::item(
                "Close Menu", "spectator/close", true, ItemAction::Close,
            )),
            0..=6 => items.get(self.page * 6 + slot).cloned(),
            _ => None,
        }
    }

    fn item(name: &str, sprite: &'static str, enabled: bool, action: ItemAction) -> SpectatorSlot {
        SpectatorSlot { name: name.to_string(), sprite, enabled, action }
    }

    fn players(&self) -> Vec<&SpectatorMenuPlayer> {
        let mut players = Vec::new();
        for entry in &self.root {
            match entry {
                SpectatorMenuEntry::Player(player) => players.push(player),
                SpectatorMenuEntry::Team { members, .. } => players.extend(members),
            }
        }
        players.sort_by_key(|player| player.id);
        players
    }

    fn items(&self) -> Vec<SpectatorSlot> {
        let player_item = |player: &SpectatorMenuPlayer| {
            Self::item(
                &player.name, "spectator/teleport_to_player", true, ItemAction::Teleport(player.id),
            )
        };
        match &self.category {
            Category::Root => {
                let has_players = self.root.iter().any(|entry| match entry {
                    SpectatorMenuEntry::Player(_) => true,
                    SpectatorMenuEntry::Team { members, .. } => !members.is_empty(),
                });
                let has_teams = self.root.iter().any(|entry| {
                    matches!(entry, SpectatorMenuEntry::Team { .. })
                });
                vec![
                    Self::item(
                        "Teleport to Player", "spectator/teleport_to_player", has_players,
                        ItemAction::Category(Category::Players),
                    ),
                    Self::item(
                        "Teleport to Team", "spectator/teleport_to_team", has_teams,
                        ItemAction::Category(Category::Teams),
                    ),
                ]
            }
            Category::Players => self.players().into_iter().map(player_item).collect(),
            Category::Teams => self.root.iter().filter_map(|entry| match entry {
                SpectatorMenuEntry::Team { name, label, .. } => Some(Self::item(
                    label, "spectator/teleport_to_team", true,
                    ItemAction::Category(Category::Team(name.clone())),
                )),
                SpectatorMenuEntry::Player(_) => None,
            }).collect(),
            Category::Team(name) => self.root.iter().find_map(|entry| match entry {
                SpectatorMenuEntry::Team { name: candidate, members, .. } if candidate == name => {
                    let mut players: Vec<_> = members.iter().collect();
                    players.sort_by_key(|player| player.id);
                    Some(players.into_iter().map(player_item).collect())
                }
                _ => None,
            }).unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::scoreboard::Team;
    use lodestone_game::tablist::GameProfile;

    fn populated(count: usize) -> SpectatorMenuState {
        let mut state = SpectatorMenuState::default();
        state.refresh((1..=count).map(|i| SpectatorMenuEntry::Player(SpectatorMenuPlayer {
            id: Uuid::from_u128(i as u128), name: format!("Player {i}"),
        })).collect());
        state
    }

    #[test]
    fn pick_and_number_keys_open_select_then_activate_without_a_screen() {
        let mut state = populated(2);
        assert_eq!(state.select(None, 0.0), SpectatorMenuOutcome::None);
        assert_eq!(state.view(0.0).unwrap().selected, None);
        state.select(Some(0), 0.1);
        assert_eq!(state.view(0.1).unwrap().prompt, "Teleport to Player");
        state.select(None, 0.2);
        assert_eq!(state.view(0.2).unwrap().selected, None);
        assert_eq!(state.select(Some(1), 0.3), SpectatorMenuOutcome::None);
        assert_eq!(state.select(None, 0.4), SpectatorMenuOutcome::Teleport(Uuid::from_u128(2)));
        assert!(state.active(0.4), "teleport leaves the HUD selection open");
    }

    #[test]
    fn pages_preserve_the_seven_first_and_six_later_target_cells() {
        let mut state = populated(15);
        state.select(Some(0), 0.0);
        state.select(Some(0), 0.1);
        state.select(Some(0), 0.2);
        let first = state.view(0.2).unwrap();
        assert_eq!(first.slots[6].as_ref().unwrap().name, "Player 7");
        assert!(first.slots[7].as_ref().unwrap().enabled);
        state.select(Some(7), 0.3);
        state.select(Some(7), 0.4);
        let second = state.view(0.4).unwrap();
        assert_eq!(second.slots[0].as_ref().unwrap().name, "Previous Page");
        assert_eq!(second.slots[1].as_ref().unwrap().name, "Player 8");
        assert_eq!(second.slots[6].as_ref().unwrap().name, "Player 13");
        state.select(Some(7), 0.5);
        let third = state.view(0.5).unwrap();
        assert_eq!(third.slots[1].as_ref().unwrap().name, "Player 14");
        assert!(!third.slots[7].as_ref().unwrap().enabled);
        state.select(Some(0), 0.6);
        state.select(Some(0), 0.7);
        assert_eq!(state.view(0.7).unwrap().slots[1].as_ref().unwrap().name, "Player 8");
    }

    #[test]
    fn disabled_categories_wheel_navigation_and_timeout_are_real() {
        let mut state = populated(0);
        state.select(None, 10.0);
        assert!(!state.view(10.0).unwrap().slots[0].as_ref().unwrap().enabled);
        state.scroll(1, 10.1);
        assert_eq!(state.view(10.1).unwrap().selected, Some(8));
        assert_eq!(state.view(13.1).unwrap().alpha, 1.0);
        assert!((state.view(14.1).unwrap().alpha - 0.5).abs() < 1e-6);
        assert!(state.view(15.1).is_none());
        state.select(Some(0), 16.0);
        assert_eq!(state.view(16.0).unwrap().selected, None);
        state.select(Some(8), 16.1);
        state.select(None, 16.2);
        assert!(state.view(16.2).is_none());
    }

    #[test]
    fn roster_omits_self_spectators_and_uuidless_rows_and_keeps_single_member_teams() {
        let mut tab = TabList::default();
        let self_id = Uuid::from_u128(1);
        tab.insert(PlayerListEntry::new(GameProfile::new(self_id, "Self")));
        let mut spectator = PlayerListEntry::new(GameProfile::new(Uuid::from_u128(2), "Ghost"));
        spectator.game_mode = lodestone_model::GameMode::Spectator;
        tab.insert(spectator);
        tab.insert(PlayerListEntry::new(GameProfile::new(None, "Legacy")));
        tab.insert(PlayerListEntry::new(GameProfile::new(Uuid::from_u128(3), "Team Member")));
        let mut board = Scoreboard::new();
        let mut team = Team::new("solo");
        team.members.push("Team Member".to_string());
        board.add_team(team);
        let entries = spectator_menu_entries(&tab, &board, Some(self_id));
        assert_eq!(entries.len(), 1);
        assert!(matches!(&entries[0], SpectatorMenuEntry::Team { members, .. } if members.len() == 1));
    }
}
