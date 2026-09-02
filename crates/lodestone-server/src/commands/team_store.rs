//! The team store — `/team`'s own state, behind [`TeamHandle`].
//!
//! # What it is
//!
//! The real team record carries a display name, colour, prefix/suffix,
//! friendly-fire and see-friendly-invisibles flags, three visibility-style
//! enums (nametag visibility, death-message visibility, collision rule), and
//! a membership set. This models all of it — unlike
//! [`crate::commands::scoreboard_store`], which deliberately drops display
//! slots because nothing renders one, every field here has a real command
//! reader: `/team list` echoes them back, and `team=`
//! (`lodestone_command_mc::SelectorPredicate::Team`) reads membership.
//!
//! **Not modelled**: anything the wire would need to actually *render* a
//! prefix/suffix/colour on a nametag or in chat (this crate has no JSON text
//! component parser — the same honest omission
//! `crate::commands::scoreboard`'s module doc names for `/scoreboard
//! objectives add`'s `displayName` — so prefix/suffix here are plain text,
//! not a component), and collision/friendly-fire/see-invisible are stored
//! and readable but not yet *enforced* anywhere in the mob/combat
//! simulation — the same "stored and broadcast is not enforced" shape
//! `crate::world_state`'s own module doc names for difficulty before its
//! first real consumer landed.
//!
//! # How it works
//!
//! [`TeamHandle`] is `Arc<Mutex<TeamState>>`, shaped exactly like
//! [`crate::commands::scoreboard_store::ScoreboardHandle`]: cheap to clone,
//! every clone shares the store, and it rides *inside*
//! [`crate::world_state::WorldStateHandle`] as a sibling field for the
//! identical reason the scoreboard does — every command entry point already
//! holds that one handle, so a second field on it is reached by all of them
//! for free. See that handle's own module doc for why a second,
//! independently-constructed store would be the island this crate has
//! already paid twice to learn about.
//!
//! A holder is on **at most one team**, matching the real join-team rule
//! (which silently removes the holder from whatever team it was on first) —
//! [`TeamHandle::join`] does the same.
//!
//! # How to change it
//!
//! Read/write access is via `WorldStateHandle::team` (`crate::world_state`),
//! never a second constructor — a `TeamHandle::default()` built anywhere
//! outside `WorldStateHandle`'s own `Default` is a fresh, disconnected
//! store.
//!
//! # Configuration
//!
//! None.
//!
//! # Dependencies
//!
//! `lodestone_model::text::TextColor` for the sixteen named colours (`None`
//! stands for `reset`, the real formatting reset code — there is no
//! seventeenth [`TextColor`](lodestone_model::text::TextColor) variant for
//! it).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_model::text::TextColor;

/// `nametagVisibility` and `deathMessageVisibility` share this one enum in
/// the real record, and so do the two fields here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Always,
    Never,
    HideForOtherTeams,
    HideForOwnTeam,
}

impl Visibility {
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Never => "never",
            Self::HideForOtherTeams => "hideForOtherTeams",
            Self::HideForOwnTeam => "hideForOwnTeam",
        }
    }
}

/// A team's collision-rule setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CollisionRule {
    #[default]
    Always,
    Never,
    PushOwnTeam,
    PushOtherTeams,
}

impl CollisionRule {
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Never => "never",
            Self::PushOwnTeam => "pushOwnTeam",
            Self::PushOtherTeams => "pushOtherTeams",
        }
    }
}

/// One team, as `/team list <team>` and friends read it back.
#[derive(Debug, Clone, PartialEq)]
pub struct Team {
    pub name: String,
    pub display_name: String,
    pub color: Option<TextColor>,
    pub prefix: String,
    pub suffix: String,
    pub friendly_fire: bool,
    pub see_friendly_invisibles: bool,
    pub nametag_visibility: Visibility,
    pub death_message_visibility: Visibility,
    pub collision_rule: CollisionRule,
    /// Insertion order, matching the real membership set's own ordering.
    pub members: Vec<String>,
}

impl Team {
    fn new(name: &str, display_name: &str) -> Self {
        Self {
            name: name.to_string(),
            display_name: display_name.to_string(),
            color: None,
            prefix: String::new(),
            suffix: String::new(),
            // The real team record's own constructor defaults.
            friendly_fire: true,
            see_friendly_invisibles: true,
            nametag_visibility: Visibility::Always,
            death_message_visibility: Visibility::Always,
            collision_rule: CollisionRule::Always,
            members: Vec::new(),
        }
    }
}

#[derive(Debug, Default)]
struct TeamState {
    /// Insertion order, matching `/scoreboard objectives list`'s own
    /// convention in `scoreboard_store.rs`.
    teams: Vec<Team>,
}

impl TeamState {
    fn team(&self, name: &str) -> Option<&Team> {
        self.teams.iter().find(|t| t.name == name)
    }

    fn team_mut(&mut self, name: &str) -> Option<&mut Team> {
        self.teams.iter_mut().find(|t| t.name == name)
    }
}

/// A cheap, cloneable handle to one world's teams. See the module doc for why
/// this is reached through [`crate::world_state::WorldStateHandle::team`]
/// rather than constructed directly.
#[derive(Debug, Clone, Default)]
pub struct TeamHandle(Arc<Mutex<TeamState>>);

/// Why a team operation could not run — every variant is a player-facing
/// message, matching the real command's own error text closely enough to be
/// recognisable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamError {
    AlreadyExists(String),
    Unknown(String),
}

impl std::fmt::Display for TeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists(name) => write!(f, "A team already exists by the name '{name}'"),
            Self::Unknown(name) => write!(f, "Unknown team '{name}'"),
        }
    }
}

impl TeamHandle {
    fn with<R>(&self, f: impl FnOnce(&mut TeamState) -> R) -> R {
        f(&mut self.0.lock().expect("team lock poisoned"))
    }

    /// The real create-team rule — refuses a duplicate name, matching
    /// [`crate::commands::scoreboard_store::ScoreboardHandle::add_objective`]'s
    /// own refusal shape.
    ///
    /// # Errors
    ///
    /// [`TeamError::AlreadyExists`].
    pub fn add_team(&self, name: &str, display_name: &str) -> Result<(), TeamError> {
        self.with(|state| {
            if state.team(name).is_some() {
                return Err(TeamError::AlreadyExists(name.to_string()));
            }
            state.teams.push(Team::new(name, display_name));
            Ok(())
        })
    }

    /// The real delete-team rule — also clears every membership row it held,
    /// which happens for free here since a member lives only inside its
    /// team's own `members` list.
    ///
    /// # Errors
    ///
    /// [`TeamError::Unknown`].
    pub fn remove_team(&self, name: &str) -> Result<(), TeamError> {
        self.with(|state| {
            let before = state.teams.len();
            state.teams.retain(|t| t.name != name);
            if state.teams.len() == before {
                return Err(TeamError::Unknown(name.to_string()));
            }
            Ok(())
        })
    }

    /// Every team, insertion order — `/team list` with no argument.
    #[must_use]
    pub fn teams(&self) -> Vec<Team> {
        self.with(|state| state.teams.clone())
    }

    /// One team — `/team list <team>`.
    #[must_use]
    pub fn team(&self, name: &str) -> Option<Team> {
        self.with(|state| state.team(name).cloned())
    }

    /// The real join-team rule — a holder is on at most one team, so
    /// this removes it from whatever team it was already on (a no-op if
    /// none) before adding it here, and does nothing if it is already a
    /// member of `name` itself.
    ///
    /// # Errors
    ///
    /// [`TeamError::Unknown`] if `name` is not a registered team.
    pub fn join(&self, name: &str, holder: &str) -> Result<(), TeamError> {
        self.with(|state| {
            if state.team(name).is_none() {
                return Err(TeamError::Unknown(name.to_string()));
            }
            for team in &mut state.teams {
                team.members.retain(|m| m != holder);
            }
            let team = state.team_mut(name).expect("checked present above");
            team.members.push(holder.to_string());
            Ok(())
        })
    }

    /// The real leave-team rule with no team named — removes
    /// `holder` from whichever team it is on, across every team (there can be
    /// at most one). Returns whether it was actually on one.
    pub fn leave(&self, holder: &str) -> bool {
        self.with(|state| {
            let mut removed = false;
            for team in &mut state.teams {
                let before = team.members.len();
                team.members.retain(|m| m != holder);
                removed |= team.members.len() != before;
            }
            removed
        })
    }

    /// `/team empty <team>` — clears every member and returns how many there
    /// were.
    ///
    /// # Errors
    ///
    /// [`TeamError::Unknown`].
    pub fn empty(&self, name: &str) -> Result<usize, TeamError> {
        self.with(|state| {
            let team = state.team_mut(name).ok_or_else(|| TeamError::Unknown(name.to_string()))?;
            let count = team.members.len();
            team.members.clear();
            Ok(count)
        })
    }

    /// The team a holder is currently on, or `""` for none — the exact shape
    /// `lodestone_command_mc::SelectorPredicate::Team` compares against, so
    /// `crate::commands::registrar::Ctx::resolve` can hand this straight to
    /// `lodestone_command_mc`'s resolver as a closure.
    #[must_use]
    pub fn team_of(&self, holder: &str) -> String {
        self.with(|state| {
            state
                .teams
                .iter()
                .find(|t| t.members.iter().any(|m| m == holder))
                .map(|t| t.name.clone())
                .unwrap_or_default()
        })
    }

    /// Every field `/team modify` can change, applied through one shared
    /// closure over `&mut Team` — kept as a single generic setter rather than
    /// nine near-identical methods, since every one of them is "look the team
    /// up, mutate one field, refuse if unknown".
    ///
    /// # Errors
    ///
    /// [`TeamError::Unknown`].
    pub fn modify(&self, name: &str, f: impl FnOnce(&mut Team)) -> Result<(), TeamError> {
        self.with(|state| {
            let team = state.team_mut(name).ok_or_else(|| TeamError::Unknown(name.to_string()))?;
            f(team);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_holder_moves_teams_rather_than_joining_both() {
        let teams = TeamHandle::default();
        teams.add_team("red", "Red").unwrap();
        teams.add_team("blue", "Blue").unwrap();

        teams.join("red", "alice").unwrap();
        assert_eq!(teams.team_of("alice"), "red");

        teams.join("blue", "alice").unwrap();
        assert_eq!(teams.team_of("alice"), "blue", "alice must leave red when she joins blue");
        assert!(!teams.team("red").unwrap().members.iter().any(|m| m == "alice"));
    }

    #[test]
    fn leave_and_empty_both_remove_membership() {
        let teams = TeamHandle::default();
        teams.add_team("red", "Red").unwrap();
        teams.join("red", "alice").unwrap();
        teams.join("red", "bob").unwrap();

        assert!(teams.leave("alice"));
        assert!(!teams.leave("alice"), "alice is already gone; leave reports false the second time");
        assert_eq!(teams.team_of("alice"), "");

        let removed = teams.empty("red").unwrap();
        assert_eq!(removed, 1, "only bob remained");
        assert_eq!(teams.team("red").unwrap().members, Vec::<String>::new());
    }

    #[test]
    fn an_unregistered_team_refuses_every_mutation() {
        let teams = TeamHandle::default();
        assert_eq!(teams.join("ghost", "alice"), Err(TeamError::Unknown("ghost".to_string())));
        assert_eq!(teams.empty("ghost"), Err(TeamError::Unknown("ghost".to_string())));
        assert_eq!(
            teams.modify("ghost", |_| {}),
            Err(TeamError::Unknown("ghost".to_string()))
        );
    }

    #[test]
    fn a_duplicate_team_name_is_refused() {
        let teams = TeamHandle::default();
        teams.add_team("red", "Red").unwrap();
        assert_eq!(teams.add_team("red", "Red Again"), Err(TeamError::AlreadyExists("red".to_string())));
    }

    #[test]
    fn defaults_match_the_real_constructor() {
        let teams = TeamHandle::default();
        teams.add_team("red", "Red").unwrap();
        let team = teams.team("red").unwrap();
        assert!(team.friendly_fire);
        assert!(team.see_friendly_invisibles);
        assert_eq!(team.nametag_visibility, Visibility::Always);
        assert_eq!(team.death_message_visibility, Visibility::Always);
        assert_eq!(team.collision_rule, CollisionRule::Always);
        assert_eq!(team.color, None);
        assert_eq!(team.prefix, "");
        assert_eq!(team.suffix, "");
    }
}
