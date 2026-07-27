//! Scoreboard: objectives, scores, display slots, and teams.
//!
//! The scoreboard is version-free state that several HUD and rendering systems
//! read from. Two parts are more entangled than they look:
//!
//! * **Display slots.** Besides `list`, `sidebar`, and `below_name`, there are
//!   sixteen *per-colour* sidebar slots (`sidebar.team.red`, …). A player whose
//!   team has a colour reads its objective from the matching coloured slot in
//!   preference to the plain sidebar. Modelling only three slots silently drops
//!   this behaviour.
//! * **Teams.** Team membership is not an isolated subsystem: it rewrites a
//!   player's display name (prefix + colour + suffix), and drives name-tag
//!   visibility and entity collision. So [`Team`] lives here next to the scores
//!   it shares a packet family with, and [`Scoreboard`] keeps a reverse
//!   member→team index so a rename is O(1) to reflect.

use std::collections::HashMap;

use lodestone_model::event as m;
use lodestone_model::{ClientEvent, Text, TextColor};

/// How a score is rendered in a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderType {
    /// Plain integer.
    #[default]
    Integer,
    /// Rendered as hearts (the `health` criterion).
    Hearts,
}

/// Optional per-score number formatting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NumberFormat {
    /// Inherit the default (no override).
    #[default]
    Default,
    /// Render nothing in place of the number.
    Blank,
    /// Always render this fixed text instead of the number.
    Fixed(Box<Text>),
    /// Render the number using this colour/style.
    Styled(TextColor),
}

/// A scoreboard objective.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Objective {
    /// Internal name (the key by which scores reference it).
    pub name: String,
    /// Criterion id (e.g. `dummy`, `health`, `minecraft:custom:...`).
    pub criteria: String,
    /// Shown display name.
    pub display_name: Text,
    /// Render type.
    pub render_type: RenderType,
    /// Default number format for this objective's scores.
    pub number_format: NumberFormat,
}

impl Objective {
    /// Creates an objective with default render type and format.
    #[must_use]
    pub fn new(name: impl Into<String>, criteria: impl Into<String>, display_name: Text) -> Self {
        Self {
            name: name.into(),
            criteria: criteria.into(),
            display_name,
            render_type: RenderType::Integer,
            number_format: NumberFormat::Default,
        }
    }
}

/// A single holder's score under one objective.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScoreEntry {
    /// The numeric value.
    pub value: i32,
    /// Optional custom display name for the holder in this objective.
    pub display_name: Option<Text>,
    /// Optional per-score number format (overrides the objective default).
    pub number_format: NumberFormat,
}

/// A display slot: the three fixed slots plus the sixteen team-colour sidebars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplaySlot {
    /// The tab-list slot (`list`).
    List,
    /// The plain sidebar (`sidebar`).
    Sidebar,
    /// Below the player name tag (`below_name`).
    BelowName,
    /// A team-colour-specific sidebar (`sidebar.team.<colour>`).
    TeamSidebar(TeamColor),
}

impl DisplaySlot {
    /// The 16 team colours in slot order (`sidebar.team.black` = 3 … `white` = 18).
    const TEAM_COLORS: [TeamColor; 16] = [
        TeamColor::Black,
        TeamColor::DarkBlue,
        TeamColor::DarkGreen,
        TeamColor::DarkAqua,
        TeamColor::DarkRed,
        TeamColor::DarkPurple,
        TeamColor::Gold,
        TeamColor::Gray,
        TeamColor::DarkGray,
        TeamColor::Blue,
        TeamColor::Green,
        TeamColor::Aqua,
        TeamColor::Red,
        TeamColor::LightPurple,
        TeamColor::Yellow,
        TeamColor::White,
    ];

    /// The wire id (`0`=list … `18`=`sidebar.team.white`).
    #[must_use]
    pub fn id(self) -> u8 {
        match self {
            DisplaySlot::List => 0,
            DisplaySlot::Sidebar => 1,
            DisplaySlot::BelowName => 2,
            DisplaySlot::TeamSidebar(c) => {
                3 + Self::TEAM_COLORS.iter().position(|&x| x == c).unwrap() as u8
            }
        }
    }

    /// Builds a slot from its wire id, if valid (`0..=18`).
    #[must_use]
    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(DisplaySlot::List),
            1 => Some(DisplaySlot::Sidebar),
            2 => Some(DisplaySlot::BelowName),
            3..=18 => Some(DisplaySlot::TeamSidebar(
                Self::TEAM_COLORS[(id - 3) as usize],
            )),
            _ => None,
        }
    }
}

/// The sixteen named team colours (a subset of [`TextColor`] with no RGB).
///
/// A team's colour selects both its display formatting and which coloured
/// sidebar slot its members read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum TeamColor {
    Black,
    DarkBlue,
    DarkGreen,
    DarkAqua,
    DarkRed,
    DarkPurple,
    Gold,
    Gray,
    DarkGray,
    Blue,
    Green,
    Aqua,
    Red,
    LightPurple,
    Yellow,
    White,
}

impl TeamColor {
    /// The equivalent [`TextColor`].
    #[must_use]
    pub fn as_text_color(self) -> TextColor {
        match self {
            TeamColor::Black => TextColor::Black,
            TeamColor::DarkBlue => TextColor::DarkBlue,
            TeamColor::DarkGreen => TextColor::DarkGreen,
            TeamColor::DarkAqua => TextColor::DarkAqua,
            TeamColor::DarkRed => TextColor::DarkRed,
            TeamColor::DarkPurple => TextColor::DarkPurple,
            TeamColor::Gold => TextColor::Gold,
            TeamColor::Gray => TextColor::Gray,
            TeamColor::DarkGray => TextColor::DarkGray,
            TeamColor::Blue => TextColor::Blue,
            TeamColor::Green => TextColor::Green,
            TeamColor::Aqua => TextColor::Aqua,
            TeamColor::Red => TextColor::Red,
            TeamColor::LightPurple => TextColor::LightPurple,
            TeamColor::Yellow => TextColor::Yellow,
            TeamColor::White => TextColor::White,
        }
    }
}

/// Name-tag / death-message visibility rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Always visible.
    #[default]
    Always,
    /// Never visible.
    Never,
    /// Hidden from players on other teams.
    HideForOtherTeams,
    /// Hidden from players on the same team.
    HideForOwnTeam,
}

/// Entity collision rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CollisionRule {
    /// Always collide.
    #[default]
    Always,
    /// Never collide.
    Never,
    /// Push only members of other teams.
    PushOtherTeams,
    /// Push only members of the same team.
    PushOwnTeam,
}

/// A team with its formatting and behavioural flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Team {
    /// Internal name.
    pub name: String,
    /// Shown display name.
    pub display_name: Text,
    /// Prefix prepended to member names.
    pub prefix: Text,
    /// Suffix appended to member names.
    pub suffix: Text,
    /// Team colour (also selects the coloured sidebar slot). `None` = no colour.
    pub color: Option<TeamColor>,
    /// Whether members can damage each other.
    pub friendly_fire: bool,
    /// Whether members can see invisible teammates.
    pub see_friendly_invisibles: bool,
    /// Name-tag visibility.
    pub name_tag_visibility: Visibility,
    /// Death-message visibility.
    pub death_message_visibility: Visibility,
    /// Collision rule.
    pub collision_rule: CollisionRule,
    /// Member holder names (players or entity UUID strings).
    pub members: Vec<String>,
}

impl Team {
    /// Creates a team with vanilla defaults (friendly fire on, invisibles
    /// visible, everything always-visible, always-collide).
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            display_name: Text::literal(name.clone()),
            name,
            prefix: Text::literal(""),
            suffix: Text::literal(""),
            color: None,
            friendly_fire: true,
            see_friendly_invisibles: true,
            name_tag_visibility: Visibility::Always,
            death_message_visibility: Visibility::Always,
            collision_rule: CollisionRule::Always,
            members: Vec::new(),
        }
    }

    /// Builds the formatted display name for a member: `prefix + name + suffix`,
    /// assembled as a root node whose children inherit the team style.
    #[must_use]
    pub fn decorate(&self, member_name: &str) -> Text {
        let mut root = Text::literal("");
        if let Some(color) = self.color {
            root.style.color = Some(color.as_text_color());
        }
        root.extra = vec![
            self.prefix.clone(),
            Text::literal(member_name),
            self.suffix.clone(),
        ];
        root
    }
}

/// The full scoreboard state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scoreboard {
    objectives: HashMap<String, Objective>,
    /// objective name -> (holder -> score).
    scores: HashMap<String, HashMap<String, ScoreEntry>>,
    display: HashMap<DisplaySlot, String>,
    teams: HashMap<String, Team>,
    /// Reverse index: member holder -> team name.
    member_team: HashMap<String, String>,
}

impl Scoreboard {
    /// A new empty scoreboard.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // --- objectives ---

    /// Adds or replaces an objective.
    pub fn add_objective(&mut self, objective: Objective) {
        self.scores.entry(objective.name.clone()).or_default();
        self.objectives.insert(objective.name.clone(), objective);
    }

    /// Removes an objective and its scores, clearing any display slot showing it.
    pub fn remove_objective(&mut self, name: &str) {
        self.objectives.remove(name);
        self.scores.remove(name);
        self.display.retain(|_, obj| obj != name);
    }

    /// Looks up an objective.
    #[must_use]
    pub fn objective(&self, name: &str) -> Option<&Objective> {
        self.objectives.get(name)
    }

    /// Number of objectives.
    #[must_use]
    pub fn objective_count(&self) -> usize {
        self.objectives.len()
    }

    // --- scores ---

    /// Sets a holder's score under an objective, creating the entry if needed.
    /// Ignored if the objective does not exist.
    pub fn set_score(&mut self, objective: &str, holder: impl Into<String>, value: i32) {
        if let Some(map) = self.scores.get_mut(objective) {
            map.entry(holder.into()).or_default().value = value;
        }
    }

    /// Sets the full score entry (value + display name + format).
    pub fn set_score_entry(
        &mut self,
        objective: &str,
        holder: impl Into<String>,
        entry: ScoreEntry,
    ) {
        if let Some(map) = self.scores.get_mut(objective) {
            map.insert(holder.into(), entry);
        }
    }

    /// Reads a holder's score entry.
    #[must_use]
    pub fn score(&self, objective: &str, holder: &str) -> Option<&ScoreEntry> {
        self.scores.get(objective)?.get(holder)
    }

    /// Removes a holder's score under one objective.
    pub fn reset_score(&mut self, objective: &str, holder: &str) {
        if let Some(map) = self.scores.get_mut(objective) {
            map.remove(holder);
        }
    }

    /// All scores for an objective, sorted by descending value then holder name
    /// (the order the sidebar renders).
    #[must_use]
    pub fn sorted_scores(&self, objective: &str) -> Vec<(&str, &ScoreEntry)> {
        let Some(map) = self.scores.get(objective) else {
            return Vec::new();
        };
        let mut v: Vec<(&str, &ScoreEntry)> = map.iter().map(|(k, e)| (k.as_str(), e)).collect();
        v.sort_by(|a, b| b.1.value.cmp(&a.1.value).then_with(|| a.0.cmp(b.0)));
        v
    }

    // --- display slots ---

    /// Assigns an objective to a display slot (or clears it with `None`).
    pub fn set_display(&mut self, slot: DisplaySlot, objective: Option<&str>) {
        match objective {
            Some(name) => {
                self.display.insert(slot, name.to_string());
            }
            None => {
                self.display.remove(&slot);
            }
        }
    }

    /// The objective shown in a slot, if any.
    #[must_use]
    pub fn displayed(&self, slot: DisplaySlot) -> Option<&str> {
        self.display.get(&slot).map(String::as_str)
    }

    /// The sidebar objective a given team colour should show: the coloured
    /// sidebar slot if set, otherwise the plain sidebar. This is the lookup a
    /// client performs to pick a player's sidebar.
    #[must_use]
    pub fn sidebar_for_color(&self, color: Option<TeamColor>) -> Option<&str> {
        color
            .and_then(|c| self.displayed(DisplaySlot::TeamSidebar(c)))
            .or_else(|| self.displayed(DisplaySlot::Sidebar))
    }

    // --- teams ---

    /// Adds or replaces a team, updating the reverse member index.
    pub fn add_team(&mut self, team: Team) {
        // Drop stale reverse entries for any previous incarnation.
        self.member_team.retain(|_, t| t != &team.name);
        for m in &team.members {
            self.member_team.insert(m.clone(), team.name.clone());
        }
        self.teams.insert(team.name.clone(), team);
    }

    /// Removes a team and clears its members from the reverse index.
    pub fn remove_team(&mut self, name: &str) {
        if let Some(team) = self.teams.remove(name) {
            for m in &team.members {
                self.member_team.remove(m);
            }
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

    /// Adds a holder to a team, moving it off any previous team. Returns whether
    /// the team existed.
    pub fn add_member(&mut self, team: &str, holder: impl Into<String>) -> bool {
        let holder = holder.into();
        if !self.teams.contains_key(team) {
            return false;
        }
        // Remove from previous team.
        if let Some(prev) = self.member_team.get(&holder).cloned()
            && let Some(prev_team) = self.teams.get_mut(&prev)
        {
            prev_team.members.retain(|m| m != &holder);
        }
        let t = self.teams.get_mut(team).unwrap();
        if !t.members.contains(&holder) {
            t.members.push(holder.clone());
        }
        self.member_team.insert(holder, team.to_string());
        true
    }

    /// Removes a holder from its team, if any.
    pub fn remove_member(&mut self, holder: &str) {
        if let Some(team) = self.member_team.remove(holder)
            && let Some(t) = self.teams.get_mut(&team)
        {
            t.members.retain(|m| m != holder);
        }
    }

    /// The display name a holder should render with: decorated by its team if it
    /// has one, else the plain name.
    #[must_use]
    pub fn display_name_of(&self, holder: &str) -> Text {
        match self.team_of(holder) {
            Some(team) => team.decorate(holder),
            None => Text::literal(holder),
        }
    }
}

// --- ClientEvent fold -------------------------------------------------------
//
// These conversions and `apply` methods are the seam that lets a driver fold a
// version-free `ClientEvent` stream (as produced by `lodestone-client`) into
// this state without the driver re-implementing the fold. They translate the
// model's parallel enums (which the wire adapters target) into this crate's
// richer types. The model type is imported as `m` so its enums do not collide
// with the same-named types defined above.

impl From<m::ObjectiveRenderType> for RenderType {
    fn from(rt: m::ObjectiveRenderType) -> Self {
        match rt {
            m::ObjectiveRenderType::Integer => RenderType::Integer,
            m::ObjectiveRenderType::Hearts => RenderType::Hearts,
        }
    }
}

impl From<&m::NumberFormat> for NumberFormat {
    fn from(nf: &m::NumberFormat) -> Self {
        match nf {
            m::NumberFormat::Default => NumberFormat::Default,
            m::NumberFormat::Blank => NumberFormat::Blank,
            m::NumberFormat::Fixed(t) => NumberFormat::Fixed(t.clone()),
            m::NumberFormat::Styled(c) => NumberFormat::Styled(*c),
        }
    }
}

impl From<m::TeamColor> for TeamColor {
    fn from(c: m::TeamColor) -> Self {
        match c {
            m::TeamColor::Black => TeamColor::Black,
            m::TeamColor::DarkBlue => TeamColor::DarkBlue,
            m::TeamColor::DarkGreen => TeamColor::DarkGreen,
            m::TeamColor::DarkAqua => TeamColor::DarkAqua,
            m::TeamColor::DarkRed => TeamColor::DarkRed,
            m::TeamColor::DarkPurple => TeamColor::DarkPurple,
            m::TeamColor::Gold => TeamColor::Gold,
            m::TeamColor::Gray => TeamColor::Gray,
            m::TeamColor::DarkGray => TeamColor::DarkGray,
            m::TeamColor::Blue => TeamColor::Blue,
            m::TeamColor::Green => TeamColor::Green,
            m::TeamColor::Aqua => TeamColor::Aqua,
            m::TeamColor::Red => TeamColor::Red,
            m::TeamColor::LightPurple => TeamColor::LightPurple,
            m::TeamColor::Yellow => TeamColor::Yellow,
            m::TeamColor::White => TeamColor::White,
        }
    }
}

impl From<m::DisplaySlot> for DisplaySlot {
    fn from(s: m::DisplaySlot) -> Self {
        match s {
            m::DisplaySlot::List => DisplaySlot::List,
            m::DisplaySlot::Sidebar => DisplaySlot::Sidebar,
            m::DisplaySlot::BelowName => DisplaySlot::BelowName,
            m::DisplaySlot::TeamSidebar(c) => DisplaySlot::TeamSidebar(c.into()),
        }
    }
}

impl From<m::Visibility> for Visibility {
    fn from(v: m::Visibility) -> Self {
        match v {
            m::Visibility::Always => Visibility::Always,
            m::Visibility::Never => Visibility::Never,
            m::Visibility::HideForOtherTeams => Visibility::HideForOtherTeams,
            m::Visibility::HideForOwnTeam => Visibility::HideForOwnTeam,
        }
    }
}

impl From<m::CollisionRule> for CollisionRule {
    fn from(r: m::CollisionRule) -> Self {
        match r {
            m::CollisionRule::Always => CollisionRule::Always,
            m::CollisionRule::Never => CollisionRule::Never,
            m::CollisionRule::PushOtherTeams => CollisionRule::PushOtherTeams,
            m::CollisionRule::PushOwnTeam => CollisionRule::PushOwnTeam,
        }
    }
}

/// Builds a [`Team`] from the model's team parameters. `death_message_visibility`
/// has no counterpart in the modern `set_player_team` packet (that flag was
/// removed), so it keeps its vanilla default; every other field is carried.
fn team_from_params(name: &str, p: &m::TeamParameters) -> Team {
    Team {
        name: name.to_string(),
        display_name: p.display_name.clone(),
        prefix: p.prefix.clone(),
        suffix: p.suffix.clone(),
        color: p.color.map(Into::into),
        friendly_fire: p.friendly_fire,
        see_friendly_invisibles: p.see_friendly_invisibles,
        name_tag_visibility: p.name_tag_visibility.into(),
        death_message_visibility: Visibility::Always,
        collision_rule: p.collision_rule.into(),
        members: Vec::new(),
    }
}

impl Scoreboard {
    /// Folds a scoreboard-family [`ClientEvent`] into this state, returning
    /// whether the event was one this aggregate owns. Non-scoreboard events
    /// return `false` unchanged, so a driver can fan a single event out to
    /// several aggregates and let each claim its own.
    ///
    /// The clientbound `set_objective` packet carries no criterion (criteria is
    /// serverbound only), so a folded [`Objective`] has an empty `criteria`; its
    /// render type comes from the packet instead. A [`Change`](m::ObjectiveMode::Change)
    /// on an unknown objective upserts it — the server treats a late change as
    /// authoritative and dropping it would silently lose the objective.
    ///
    /// A [`ScoreUpdate`](ClientEvent::ScoreUpdate) for an objective this board
    /// has never seen is dropped, matching this crate's objective-gated
    /// [`set_score_entry`](Self::set_score_entry): vanilla always sends the
    /// objective before its scores, so the drop is unreachable in practice and
    /// preferred over silently inventing an objective-less score bucket. This is
    /// a deliberate divergence from `lodestone-client`'s create-on-demand fold,
    /// noted so a future reconciliation of the two does not "fix" it blindly.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::ObjectiveUpdate {
                name,
                mode,
                display_name,
                render_type,
                number_format,
            } => {
                match mode {
                    m::ObjectiveMode::Add | m::ObjectiveMode::Change => {
                        let mut obj = Objective::new(
                            name.clone(),
                            "",
                            display_name
                                .clone()
                                .unwrap_or_else(|| Text::literal(name.clone())),
                        );
                        if let Some(rt) = render_type {
                            obj.render_type = (*rt).into();
                        }
                        if let Some(nf) = number_format {
                            obj.number_format = nf.into();
                        }
                        self.add_objective(obj);
                    }
                    m::ObjectiveMode::Remove => self.remove_objective(name),
                }
                true
            }
            ClientEvent::DisplayObjective { slot, objective } => {
                self.set_display((*slot).into(), objective.as_deref());
                true
            }
            ClientEvent::ScoreUpdate {
                holder,
                objective,
                value,
                display,
                number_format,
            } => {
                self.set_score_entry(
                    objective,
                    holder.clone(),
                    ScoreEntry {
                        value: *value,
                        display_name: display.clone(),
                        number_format: number_format.as_ref().map(Into::into).unwrap_or_default(),
                    },
                );
                true
            }
            ClientEvent::ScoreReset { holder, objective } => {
                match objective {
                    Some(name) => self.reset_score(name, holder),
                    // Reset the holder from every objective. Touches private
                    // state directly because the objective-scoped public
                    // `reset_score` cannot express "all objectives".
                    None => {
                        for map in self.scores.values_mut() {
                            map.remove(holder);
                        }
                    }
                }
                true
            }
            ClientEvent::TeamUpdate { name, action } => {
                self.fold_team(name, action);
                true
            }
            _ => false,
        }
    }

    /// Folds a [`TeamUpdate`](ClientEvent::TeamUpdate) action. `Update` rebuilds
    /// the team from new parameters while preserving its current membership,
    /// since the update action carries no member list.
    fn fold_team(&mut self, name: &str, action: &m::TeamAction) {
        match action {
            m::TeamAction::Create { params, members } => {
                let mut team = team_from_params(name, params);
                team.members = members.clone();
                self.add_team(team);
            }
            m::TeamAction::Remove => self.remove_team(name),
            m::TeamAction::Update { params } => {
                if let Some(existing) = self.teams.get(name) {
                    let members = existing.members.clone();
                    let mut team = team_from_params(name, params);
                    team.members = members;
                    self.add_team(team);
                }
            }
            m::TeamAction::AddMembers(members) => {
                for holder in members {
                    self.add_member(name, holder.clone());
                }
            }
            m::TeamAction::RemoveMembers(members) => {
                for holder in members {
                    self.remove_member(holder);
                }
            }
        }
    }
}

#[cfg(test)]
mod fold_tests {
    use super::*;

    fn objective_add(name: &str, render: m::ObjectiveRenderType) -> ClientEvent {
        ClientEvent::ObjectiveUpdate {
            name: name.to_string(),
            mode: m::ObjectiveMode::Add,
            display_name: Some(Text::literal("Kills")),
            render_type: Some(render),
            number_format: None,
        }
    }

    #[test]
    fn objective_add_is_readable_through_public_api() {
        let mut sb = Scoreboard::new();
        assert!(sb.apply(&objective_add("kills", m::ObjectiveRenderType::Hearts)));
        let obj = sb.objective("kills").expect("objective present after add");
        assert_eq!(obj.name, "kills");
        assert_eq!(obj.render_type, RenderType::Hearts);
        assert_eq!(obj.display_name, Text::literal("Kills"));
        // The clientbound packet carries no criterion.
        assert_eq!(obj.criteria, "");
    }

    #[test]
    fn change_on_unknown_objective_upserts() {
        let mut sb = Scoreboard::new();
        let ev = ClientEvent::ObjectiveUpdate {
            name: "late".to_string(),
            mode: m::ObjectiveMode::Change,
            display_name: Some(Text::literal("Late")),
            render_type: Some(m::ObjectiveRenderType::Integer),
            number_format: None,
        };
        assert!(sb.apply(&ev));
        assert!(sb.objective("late").is_some());
    }

    #[test]
    fn remove_objective_clears_scores_and_display() {
        let mut sb = Scoreboard::new();
        sb.apply(&objective_add("kills", m::ObjectiveRenderType::Integer));
        sb.apply(&ClientEvent::ScoreUpdate {
            holder: "Alice".into(),
            objective: "kills".into(),
            value: 7,
            display: None,
            number_format: None,
        });
        sb.apply(&ClientEvent::DisplayObjective {
            slot: m::DisplaySlot::Sidebar,
            objective: Some("kills".into()),
        });
        assert_eq!(sb.score("kills", "Alice").map(|s| s.value), Some(7));
        assert_eq!(sb.displayed(DisplaySlot::Sidebar), Some("kills"));

        sb.apply(&ClientEvent::ObjectiveUpdate {
            name: "kills".into(),
            mode: m::ObjectiveMode::Remove,
            display_name: None,
            render_type: None,
            number_format: None,
        });
        assert!(sb.objective("kills").is_none());
        assert!(sb.score("kills", "Alice").is_none());
        assert_eq!(sb.displayed(DisplaySlot::Sidebar), None);
    }

    #[test]
    fn display_slot_team_colour_maps_and_clears() {
        let mut sb = Scoreboard::new();
        sb.apply(&objective_add("k", m::ObjectiveRenderType::Integer));
        sb.apply(&ClientEvent::DisplayObjective {
            slot: m::DisplaySlot::TeamSidebar(m::TeamColor::Red),
            objective: Some("k".into()),
        });
        assert_eq!(
            sb.displayed(DisplaySlot::TeamSidebar(TeamColor::Red)),
            Some("k")
        );
        assert_eq!(sb.sidebar_for_color(Some(TeamColor::Red)), Some("k"));
        // Clear it.
        sb.apply(&ClientEvent::DisplayObjective {
            slot: m::DisplaySlot::TeamSidebar(m::TeamColor::Red),
            objective: None,
        });
        assert_eq!(sb.displayed(DisplaySlot::TeamSidebar(TeamColor::Red)), None);
    }

    #[test]
    fn score_for_unknown_objective_is_dropped() {
        // Deliberate objective-gated divergence from lodestone-client.
        let mut sb = Scoreboard::new();
        assert!(sb.apply(&ClientEvent::ScoreUpdate {
            holder: "Bob".into(),
            objective: "ghost".into(),
            value: 3,
            display: None,
            number_format: None,
        }));
        assert!(sb.score("ghost", "Bob").is_none());
    }

    #[test]
    fn score_reset_scopes_to_one_or_all_objectives() {
        let mut sb = Scoreboard::new();
        sb.apply(&objective_add("a", m::ObjectiveRenderType::Integer));
        sb.apply(&objective_add("b", m::ObjectiveRenderType::Integer));
        for obj in ["a", "b"] {
            sb.apply(&ClientEvent::ScoreUpdate {
                holder: "Zoe".into(),
                objective: obj.into(),
                value: 1,
                display: None,
                number_format: None,
            });
        }
        // Scoped reset touches only "a".
        sb.apply(&ClientEvent::ScoreReset {
            holder: "Zoe".into(),
            objective: Some("a".into()),
        });
        assert!(sb.score("a", "Zoe").is_none());
        assert!(sb.score("b", "Zoe").is_some());
        // All-objective reset clears the rest.
        sb.apply(&ClientEvent::ScoreReset {
            holder: "Zoe".into(),
            objective: None,
        });
        assert!(sb.score("b", "Zoe").is_none());
    }

    fn red_params() -> Box<m::TeamParameters> {
        Box::new(m::TeamParameters {
            display_name: Text::literal("Red Team"),
            prefix: Text::literal("["),
            suffix: Text::literal("]"),
            name_tag_visibility: m::Visibility::HideForOtherTeams,
            collision_rule: m::CollisionRule::PushOwnTeam,
            color: Some(m::TeamColor::Red),
            friendly_fire: false,
            see_friendly_invisibles: true,
        })
    }

    #[test]
    fn team_create_maps_params_and_seeds_membership() {
        let mut sb = Scoreboard::new();
        assert!(sb.apply(&ClientEvent::TeamUpdate {
            name: "red".into(),
            action: m::TeamAction::Create {
                params: red_params(),
                members: vec!["Alice".into()],
            },
        }));
        let team = sb.team("red").expect("team present");
        assert_eq!(team.prefix, Text::literal("["));
        assert_eq!(team.suffix, Text::literal("]"));
        assert_eq!(team.color, Some(TeamColor::Red));
        assert!(!team.friendly_fire);
        assert_eq!(team.name_tag_visibility, Visibility::HideForOtherTeams);
        assert_eq!(team.collision_rule, CollisionRule::PushOwnTeam);
        // Reverse index resolves membership.
        assert_eq!(sb.team_of("Alice").map(|t| t.name.as_str()), Some("red"));
    }

    #[test]
    fn team_update_preserves_membership() {
        let mut sb = Scoreboard::new();
        sb.apply(&ClientEvent::TeamUpdate {
            name: "red".into(),
            action: m::TeamAction::Create {
                params: red_params(),
                members: vec!["Alice".into()],
            },
        });
        let mut updated = red_params();
        updated.prefix = Text::literal("<<");
        sb.apply(&ClientEvent::TeamUpdate {
            name: "red".into(),
            action: m::TeamAction::Update { params: updated },
        });
        let team = sb.team("red").expect("team present");
        assert_eq!(team.prefix, Text::literal("<<"));
        assert_eq!(team.members, vec!["Alice".to_string()]);
        assert_eq!(sb.team_of("Alice").map(|t| t.name.as_str()), Some("red"));
    }

    #[test]
    fn team_member_add_and_remove() {
        let mut sb = Scoreboard::new();
        sb.apply(&ClientEvent::TeamUpdate {
            name: "red".into(),
            action: m::TeamAction::Create {
                params: red_params(),
                members: vec![],
            },
        });
        sb.apply(&ClientEvent::TeamUpdate {
            name: "red".into(),
            action: m::TeamAction::AddMembers(vec!["Bob".into(), "Cara".into()]),
        });
        assert_eq!(sb.team_of("Bob").map(|t| t.name.as_str()), Some("red"));
        assert_eq!(sb.team_of("Cara").map(|t| t.name.as_str()), Some("red"));
        sb.apply(&ClientEvent::TeamUpdate {
            name: "red".into(),
            action: m::TeamAction::RemoveMembers(vec!["Bob".into()]),
        });
        assert!(sb.team_of("Bob").is_none());
        assert!(sb.team_of("Cara").is_some());
    }

    #[test]
    fn non_scoreboard_event_is_not_claimed() {
        let mut sb = Scoreboard::new();
        let ev = ClientEvent::PlayerListRemove {
            profile_ids: vec![],
        };
        assert!(!sb.apply(&ev));
    }
}
