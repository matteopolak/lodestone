//! Scoreboard, team, boss-bar, and sound payloads.

use crate::*;

/// A Minecraft sound source category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoundCategory {
    /// Master volume category.
    Master,
    /// Music category.
    Music,
    /// Record / jukebox category.
    Record,
    /// Weather category.
    Weather,
    /// Block sound category.
    Block,
    /// Hostile entity category.
    Hostile,
    /// Neutral entity category.
    Neutral,
    /// Player sound category.
    Player,
    /// Ambient sound category.
    Ambient,
    /// Voice category.
    Voice,
    /// User-interface sound category.
    Ui,
}

impl SoundCategory {
    /// Categories in the wire's own ordinal order.
    ///
    /// Protocol adapters that decode raw enum ordinals should index through this
    /// table rather than duplicating the order.
    pub const ALL: [Self; 11] = [
        Self::Master,
        Self::Music,
        Self::Record,
        Self::Weather,
        Self::Block,
        Self::Hostile,
        Self::Neutral,
        Self::Player,
        Self::Ambient,
        Self::Voice,
        Self::Ui,
    ];

    /// Returns the category for its wire ordinal.
    #[must_use]
    pub const fn from_ordinal(ordinal: u8) -> Option<Self> {
        match ordinal {
            0 => Some(Self::Master),
            1 => Some(Self::Music),
            2 => Some(Self::Record),
            3 => Some(Self::Weather),
            4 => Some(Self::Block),
            5 => Some(Self::Hostile),
            6 => Some(Self::Neutral),
            7 => Some(Self::Player),
            8 => Some(Self::Ambient),
            9 => Some(Self::Voice),
            10 => Some(Self::Ui),
            _ => None,
        }
    }

    /// Returns this category's wire ordinal.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Master => 0,
            Self::Music => 1,
            Self::Record => 2,
            Self::Weather => 3,
            Self::Block => 4,
            Self::Hostile => 5,
            Self::Neutral => 6,
            Self::Player => 7,
            Self::Ambient => 8,
            Self::Voice => 9,
            Self::Ui => 10,
        }
    }
}

/// Objective update mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectiveMode {
    /// Add a new objective.
    Add,
    /// Remove an existing objective.
    Remove,
    /// Change an existing objective.
    Change,
}

/// How objective scores should render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectiveRenderType {
    /// Render as a plain integer.
    Integer,
    /// Render as hearts.
    Hearts,
}

/// Optional scoreboard number formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumberFormat {
    /// Use the objective or client default.
    Default,
    /// Render no number.
    Blank,
    /// Render this fixed text instead of the number.
    Fixed(Box<Text>),
    /// Render the number styled with this colour.
    Styled(TextColor),
}

/// The sixteen named team colours.
///
/// These are the named text colours that can be used as team colours and as
/// coloured sidebar display-slot selectors. RGB text colours are intentionally
/// excluded because teams can only use the named set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TeamColor {
    /// Black.
    Black,
    /// Dark blue.
    DarkBlue,
    /// Dark green.
    DarkGreen,
    /// Dark aqua.
    DarkAqua,
    /// Dark red.
    DarkRed,
    /// Dark purple.
    DarkPurple,
    /// Gold.
    Gold,
    /// Gray.
    Gray,
    /// Dark gray.
    DarkGray,
    /// Blue.
    Blue,
    /// Green.
    Green,
    /// Aqua.
    Aqua,
    /// Red.
    Red,
    /// Light purple.
    LightPurple,
    /// Yellow.
    Yellow,
    /// White.
    White,
}

impl TeamColor {
    /// Converts this team colour to the matching text colour.
    #[must_use]
    pub const fn as_text_color(self) -> TextColor {
        match self {
            Self::Black => TextColor::Black,
            Self::DarkBlue => TextColor::DarkBlue,
            Self::DarkGreen => TextColor::DarkGreen,
            Self::DarkAqua => TextColor::DarkAqua,
            Self::DarkRed => TextColor::DarkRed,
            Self::DarkPurple => TextColor::DarkPurple,
            Self::Gold => TextColor::Gold,
            Self::Gray => TextColor::Gray,
            Self::DarkGray => TextColor::DarkGray,
            Self::Blue => TextColor::Blue,
            Self::Green => TextColor::Green,
            Self::Aqua => TextColor::Aqua,
            Self::Red => TextColor::Red,
            Self::LightPurple => TextColor::LightPurple,
            Self::Yellow => TextColor::Yellow,
            Self::White => TextColor::White,
        }
    }
}

/// A scoreboard display slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplaySlot {
    /// Tab-list player slot.
    List,
    /// Plain sidebar.
    Sidebar,
    /// Below-name slot.
    BelowName,
    /// Sidebar shown to members of a team with the given colour.
    TeamSidebar(TeamColor),
}

/// Name-tag visibility rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Visibility {
    /// Always visible.
    Always,
    /// Never visible.
    Never,
    /// Hidden from players on other teams.
    HideForOtherTeams,
    /// Hidden from players on the same team.
    HideForOwnTeam,
}

/// Team collision rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CollisionRule {
    /// Always collide.
    Always,
    /// Never collide.
    Never,
    /// Push only members of other teams.
    PushOtherTeams,
    /// Push only members of the same team.
    PushOwnTeam,
}

/// Shared parameters for team create/update actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamParameters {
    /// Shown team display name.
    pub display_name: Text,
    /// Prefix prepended to member names.
    pub prefix: Text,
    /// Suffix appended to member names.
    pub suffix: Text,
    /// Name-tag visibility rule.
    pub name_tag_visibility: Visibility,
    /// Collision rule.
    pub collision_rule: CollisionRule,
    /// Optional team colour.
    pub color: Option<TeamColor>,
    /// Whether members can damage each other.
    pub friendly_fire: bool,
    /// Whether members can see invisible teammates.
    pub see_friendly_invisibles: bool,
}

/// Team update action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamAction {
    /// Create a team with parameters and initial members.
    Create {
        /// Team parameters.
        params: Box<TeamParameters>,
        /// Initial member holder names.
        members: Vec<String>,
    },
    /// Remove a team.
    Remove,
    /// Update team parameters.
    Update {
        /// New team parameters.
        params: Box<TeamParameters>,
    },
    /// Add members to the team.
    AddMembers(Vec<String>),
    /// Remove members from the team.
    RemoveMembers(Vec<String>),
}

/// Boss bar colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BossColor {
    /// Pink.
    Pink,
    /// Blue.
    Blue,
    /// Red.
    Red,
    /// Green.
    Green,
    /// Yellow.
    Yellow,
    /// Purple.
    Purple,
    /// White.
    White,
}

/// Boss bar overlay/division style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BossOverlay {
    /// Continuous progress bar.
    Progress,
    /// Six notches.
    Notched6,
    /// Ten notches.
    Notched10,
    /// Twelve notches.
    Notched12,
    /// Twenty notches.
    Notched20,
}

/// Boss bar update action.
#[derive(Debug, Clone, PartialEq)]
pub enum BossAction {
    /// Add a boss bar.
    Add {
        /// Displayed title.
        title: Box<Text>,
        /// Current progress, normally `0.0..=1.0`.
        progress: f32,
        /// Bar colour.
        color: BossColor,
        /// Bar overlay/division style.
        overlay: BossOverlay,
        /// Whether the sky should darken.
        darken: bool,
        /// Whether boss music should play.
        music: bool,
        /// Whether world fog should appear.
        fog: bool,
    },
    /// Remove the boss bar.
    Remove,
    /// Update progress.
    UpdateProgress(f32),
    /// Update title.
    UpdateName(Box<Text>),
    /// Update colour and overlay.
    UpdateStyle {
        /// New bar colour.
        color: BossColor,
        /// New overlay/division style.
        overlay: BossOverlay,
    },
    /// Update visual/audio flags.
    UpdateFlags {
        /// Whether the sky should darken.
        darken: bool,
        /// Whether boss music should play.
        music: bool,
        /// Whether world fog should appear.
        fog: bool,
    },
}
