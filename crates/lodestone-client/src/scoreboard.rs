//! Client-side scoreboard, team and boss-bar read-model.
//!
//! The server never sends a whole scoreboard; it sends a stream of *deltas*
//! ([`ClientEvent::ObjectiveUpdate`], [`ClientEvent::ScoreUpdate`],
//! [`ClientEvent::TeamUpdate`], [`ClientEvent::BossBarUpdate`], ...). This
//! module folds those deltas into queryable aggregates so a bot can ask "what
//! is on the sidebar" or "which team is this holder on" without replaying the
//! event stream by hand.
//!
//! The aggregates store the model's own delta primitives ([`Text`],
//! [`TeamParameters`], [`BossColor`], ...) verbatim — this crate is
//! version-free and never re-interprets them. The fold semantics deliberately
//! mirror `lodestone-game`'s scoreboard so the bot read-model and the game HUD
//! agree on the same server data (removing an objective purges its scores and
//! any display slot pointing at it; resetting a score with no objective clears
//! the holder everywhere; moving a holder onto a team removes it from its old
//! one; boss bars keep insertion order for rendering).

use std::collections::HashMap;

use lodestone_model::{
    BossAction, BossColor, BossOverlay, DisplaySlot, NumberFormat, ObjectiveMode,
    ObjectiveRenderType, TeamAction, TeamParameters, Text,
};
use uuid::Uuid;

/// A scoreboard objective: a named counter with display metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct Objective {
    /// Objective name (the wire key).
    pub name: String,
    /// Display name, when the server sent one.
    pub display_name: Option<Text>,
    /// How scores render (integer vs hearts).
    pub render_type: Option<ObjectiveRenderType>,
    /// Objective-wide default number format.
    pub number_format: Option<NumberFormat>,
}

/// A single score line: one holder's value under one objective.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreEntry {
    /// Score holder name (a player name or arbitrary entity/fake name).
    pub holder: String,
    /// The objective this score belongs to.
    pub objective: String,
    /// The score value.
    pub value: i32,
    /// Per-holder display override, when the server sent one.
    pub display: Option<Text>,
    /// Per-score number format override, when the server sent one.
    pub number_format: Option<NumberFormat>,
}

/// A team and its current membership.
#[derive(Debug, Clone, PartialEq)]
pub struct Team {
    /// Team name (the wire key).
    pub name: String,
    /// Team parameters (colour, prefix/suffix, collision and visibility rules).
    pub params: TeamParameters,
    /// Member holder names, in the order the server last set them.
    pub members: Vec<String>,
}

/// A boss bar.
#[derive(Debug, Clone, PartialEq)]
pub struct BossBar {
    /// Stable boss-bar id.
    pub id: Uuid,
    /// Displayed title.
    pub title: Text,
    /// Progress, normally `0.0..=1.0`.
    pub progress: f32,
    /// Bar colour.
    pub color: BossColor,
    /// Bar overlay/division style.
    pub overlay: BossOverlay,
    /// Whether the sky should darken.
    pub darken: bool,
    /// Whether boss music should play.
    pub music: bool,
    /// Whether world fog should appear.
    pub fog: bool,
}

/// The folded scoreboard: objectives, per-objective scores, display-slot
/// assignments and teams.
///
/// Cloned out whole by [`crate::ClientHandle::scoreboard`]; query it with the
/// accessor methods below.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scoreboard {
    objectives: HashMap<String, Objective>,
    /// objective name -> (holder -> entry)
    scores: HashMap<String, HashMap<String, ScoreEntry>>,
    /// display slot -> objective name
    display: HashMap<DisplaySlot, String>,
    teams: HashMap<String, Team>,
    /// holder -> team name (reverse index for `team_of`)
    member_team: HashMap<String, String>,
}

impl Scoreboard {
    /// Looks up an objective by name.
    #[must_use]
    pub fn objective(&self, name: &str) -> Option<&Objective> {
        self.objectives.get(name)
    }

    /// All objectives, in arbitrary order.
    #[must_use]
    pub fn objectives(&self) -> Vec<&Objective> {
        self.objectives.values().collect()
    }

    /// The objective name assigned to a display slot, if any.
    #[must_use]
    pub fn displayed(&self, slot: DisplaySlot) -> Option<&str> {
        self.display.get(&slot).map(String::as_str)
    }

    /// A single holder's score under an objective.
    #[must_use]
    pub fn score(&self, objective: &str, holder: &str) -> Option<&ScoreEntry> {
        self.scores.get(objective)?.get(holder)
    }

    /// All scores for an objective, sorted by value descending then holder name
    /// ascending — the order a vanilla sidebar renders them in.
    #[must_use]
    pub fn scores(&self, objective: &str) -> Vec<ScoreEntry> {
        let Some(map) = self.scores.get(objective) else {
            return Vec::new();
        };
        let mut entries: Vec<ScoreEntry> = map.values().cloned().collect();
        entries.sort_by(|a, b| b.value.cmp(&a.value).then_with(|| a.holder.cmp(&b.holder)));
        entries
    }

    /// The scores for whichever objective currently occupies a display slot,
    /// sorted for rendering. Empty if the slot is unassigned.
    #[must_use]
    pub fn scores_in_slot(&self, slot: DisplaySlot) -> Vec<ScoreEntry> {
        match self.displayed(slot) {
            Some(objective) => self.scores(objective),
            None => Vec::new(),
        }
    }

    /// Looks up a team by name.
    #[must_use]
    pub fn team(&self, name: &str) -> Option<&Team> {
        self.teams.get(name)
    }

    /// The team a holder belongs to, if any.
    #[must_use]
    pub fn team_of(&self, holder: &str) -> Option<&Team> {
        let name = self.member_team.get(holder)?;
        self.teams.get(name)
    }

    /// Folds an `ObjectiveUpdate`. Add/Change upsert the objective (Change on an
    /// unknown objective creates it — the server treats a late Change as
    /// authoritative state, and dropping it would silently lose data). Remove
    /// deletes the objective together with its scores and any display slot
    /// pointing at it, matching `lodestone-game`.
    pub(crate) fn apply_objective(
        &mut self,
        name: &str,
        mode: ObjectiveMode,
        display_name: Option<Text>,
        render_type: Option<ObjectiveRenderType>,
        number_format: Option<NumberFormat>,
    ) {
        match mode {
            ObjectiveMode::Add | ObjectiveMode::Change => {
                self.scores.entry(name.to_string()).or_default();
                self.objectives.insert(
                    name.to_string(),
                    Objective {
                        name: name.to_string(),
                        display_name,
                        render_type,
                        number_format,
                    },
                );
            }
            ObjectiveMode::Remove => {
                self.objectives.remove(name);
                self.scores.remove(name);
                self.display.retain(|_, obj| obj != name);
            }
        }
    }

    /// Folds a `DisplayObjective`: assign an objective to a slot, or clear it.
    pub(crate) fn apply_display(&mut self, slot: DisplaySlot, objective: Option<&str>) {
        match objective {
            Some(name) => {
                self.display.insert(slot, name.to_string());
            }
            None => {
                self.display.remove(&slot);
            }
        }
    }

    /// Folds a `ScoreUpdate`. The score is stored under its objective; the
    /// objective's score bucket is created on demand so a score is never
    /// dropped even if it races ahead of its objective definition.
    pub(crate) fn apply_score(
        &mut self,
        holder: &str,
        objective: &str,
        value: i32,
        display: Option<Text>,
        number_format: Option<NumberFormat>,
    ) {
        self.scores
            .entry(objective.to_string())
            .or_default()
            .insert(
                holder.to_string(),
                ScoreEntry {
                    holder: holder.to_string(),
                    objective: objective.to_string(),
                    value,
                    display,
                    number_format,
                },
            );
    }

    /// Folds a `ScoreReset`: clear a holder from one objective, or (when
    /// `objective` is `None`) from every objective.
    pub(crate) fn apply_score_reset(&mut self, holder: &str, objective: Option<&str>) {
        match objective {
            Some(name) => {
                if let Some(map) = self.scores.get_mut(name) {
                    map.remove(holder);
                }
            }
            None => {
                for map in self.scores.values_mut() {
                    map.remove(holder);
                }
            }
        }
    }

    /// Folds a `TeamUpdate` against the current teams and the reverse
    /// membership index.
    pub(crate) fn apply_team(&mut self, name: &str, action: &TeamAction) {
        match action {
            TeamAction::Create { params, members } => {
                // Drop any stale reverse entries for a previous incarnation of
                // this team before re-seeding membership.
                self.member_team.retain(|_, t| t != name);
                for m in members {
                    self.member_team.insert(m.clone(), name.to_string());
                }
                self.teams.insert(
                    name.to_string(),
                    Team {
                        name: name.to_string(),
                        params: (**params).clone(),
                        members: members.clone(),
                    },
                );
            }
            TeamAction::Remove => {
                if let Some(team) = self.teams.remove(name) {
                    for m in &team.members {
                        self.member_team.remove(m);
                    }
                }
            }
            TeamAction::Update { params } => {
                if let Some(team) = self.teams.get_mut(name) {
                    team.params = (**params).clone();
                }
            }
            TeamAction::AddMembers(members) => {
                for m in members {
                    self.add_member(name, m);
                }
            }
            TeamAction::RemoveMembers(members) => {
                for m in members {
                    self.remove_member(m);
                }
            }
        }
    }

    /// Adds `holder` to `team`, moving it off any previous team. No-op if the
    /// team does not exist.
    fn add_member(&mut self, team: &str, holder: &str) {
        if !self.teams.contains_key(team) {
            return;
        }
        if let Some(prev) = self.member_team.get(holder).cloned()
            && prev != team
            && let Some(prev_team) = self.teams.get_mut(&prev)
        {
            prev_team.members.retain(|m| m != holder);
        }
        let t = self
            .teams
            .get_mut(team)
            .expect("team presence checked above");
        if !t.members.iter().any(|m| m == holder) {
            t.members.push(holder.to_string());
        }
        self.member_team
            .insert(holder.to_string(), team.to_string());
    }

    /// Removes `holder` from whatever team it is on, if any.
    fn remove_member(&mut self, holder: &str) {
        if let Some(team) = self.member_team.remove(holder)
            && let Some(t) = self.teams.get_mut(&team)
        {
            t.members.retain(|m| m != holder);
        }
    }
}

/// Folds a [`BossBarUpdate`](lodestone_model::ClientEvent::BossBarUpdate) into
/// the insertion-ordered boss-bar list. `Add` appends a new bar (or replaces
/// one with the same id in place, preserving its position); every other action
/// mutates the matching bar and is a no-op if no bar with that id is present.
pub(crate) fn apply_boss_bar(bars: &mut Vec<BossBar>, id: Uuid, action: &BossAction) {
    match action {
        BossAction::Add {
            title,
            progress,
            color,
            overlay,
            darken,
            music,
            fog,
        } => {
            let bar = BossBar {
                id,
                title: (**title).clone(),
                progress: *progress,
                color: *color,
                overlay: *overlay,
                darken: *darken,
                music: *music,
                fog: *fog,
            };
            match bars.iter_mut().find(|b| b.id == id) {
                Some(existing) => *existing = bar,
                None => bars.push(bar),
            }
        }
        BossAction::Remove => {
            bars.retain(|b| b.id != id);
        }
        BossAction::UpdateProgress(progress) => {
            if let Some(bar) = bars.iter_mut().find(|b| b.id == id) {
                bar.progress = *progress;
            }
        }
        BossAction::UpdateName(title) => {
            if let Some(bar) = bars.iter_mut().find(|b| b.id == id) {
                bar.title = (**title).clone();
            }
        }
        BossAction::UpdateStyle { color, overlay } => {
            if let Some(bar) = bars.iter_mut().find(|b| b.id == id) {
                bar.color = *color;
                bar.overlay = *overlay;
            }
        }
        BossAction::UpdateFlags { darken, music, fog } => {
            if let Some(bar) = bars.iter_mut().find(|b| b.id == id) {
                bar.darken = *darken;
                bar.music = *music;
                bar.fog = *fog;
            }
        }
    }
}
