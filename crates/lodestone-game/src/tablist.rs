//! Tab list / player info: the client's view of who is on the server.
//!
//! Each entry pairs a [`GameProfile`] (name, id, and signed properties — the
//! skin/cape texture blob lives here) with per-connection state: game mode,
//! latency, whether the player is *listed* (shown in the tab overlay at all),
//! an optional rich display name, a list-order key, and a hat-visibility flag.
//! The list also carries a header and footer.
//!
//! The canonical state shape is version-free. Modern servers send a **bitmask
//! of actions** in a single `player_info_update` packet (add, update latency,
//! update game mode, …) where legacy servers sent discrete packets; that
//! packing is a protocol-adapter concern. What lives here is the resulting
//! per-entry state and the operations that mutate it.

use std::collections::HashMap;

use lodestone_model::{GameMode, Text};
use uuid::Uuid;

/// A signed profile property (e.g. `textures`, whose value is the base64 skin
/// blob and whose signature is Mojang's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileProperty {
    /// Property name (`textures`, …).
    pub name: String,
    /// Property value (base64 for `textures`).
    pub value: String,
    /// Optional Yggdrasil signature.
    pub signature: Option<String>,
}

/// A Mojang game profile: identity plus signed properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameProfile {
    /// The player UUID.
    pub id: Uuid,
    /// The player name.
    pub name: String,
    /// Signed properties (textures, etc.).
    pub properties: Vec<ProfileProperty>,
}

impl GameProfile {
    /// Creates a profile with no properties.
    #[must_use]
    pub fn new(id: Uuid, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            properties: Vec::new(),
        }
    }

    /// The `textures` property value (the base64 skin/cape blob), if present.
    #[must_use]
    pub fn skin_texture(&self) -> Option<&str> {
        self.property("textures")
    }

    /// Looks up a property value by name.
    #[must_use]
    pub fn property(&self, name: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.value.as_str())
    }
}

/// One tab-list entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerListEntry {
    /// Identity and properties.
    pub profile: GameProfile,
    /// Current game mode.
    pub game_mode: GameMode,
    /// Ping in milliseconds (`-1` = unknown; negative renders as "no
    /// connection" bars).
    pub latency: i32,
    /// Whether the player is shown in the tab overlay.
    pub listed: bool,
    /// Optional rich display name overriding the plain profile name.
    pub display_name: Option<Text>,
    /// Sort key; higher orders sort first.
    pub list_order: i32,
    /// Whether the player's hat (second skin layer) renders in the tab list.
    pub show_hat: bool,
}

impl PlayerListEntry {
    /// Creates an entry from a profile with vanilla defaults (survival, unknown
    /// latency, listed, hat shown).
    #[must_use]
    pub fn new(profile: GameProfile) -> Self {
        Self {
            profile,
            game_mode: GameMode::Survival,
            latency: -1,
            listed: true,
            display_name: None,
            list_order: 0,
            show_hat: true,
        }
    }

    /// The name to render: the display name if set, else the plain profile name.
    #[must_use]
    pub fn effective_name(&self) -> Text {
        self.display_name
            .clone()
            .unwrap_or_else(|| Text::literal(self.profile.name.clone()))
    }
}

/// The full tab list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabList {
    entries: HashMap<Uuid, PlayerListEntry>,
    /// Header shown above the list.
    pub header: Option<Text>,
    /// Footer shown below the list.
    pub footer: Option<Text>,
}

impl TabList {
    /// A new empty tab list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces an entry (the `add_player` action).
    pub fn insert(&mut self, entry: PlayerListEntry) {
        self.entries.insert(entry.profile.id, entry);
    }

    /// Removes an entry by id (the `player_info_remove` packet).
    pub fn remove(&mut self, id: &Uuid) {
        self.entries.remove(id);
    }

    /// Looks up an entry.
    #[must_use]
    pub fn get(&self, id: &Uuid) -> Option<&PlayerListEntry> {
        self.entries.get(id)
    }

    /// Mutable access to an entry, for applying a partial update action.
    pub fn get_mut(&mut self, id: &Uuid) -> Option<&mut PlayerListEntry> {
        self.entries.get_mut(id)
    }

    /// Number of entries (listed and unlisted).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries, unordered.
    pub fn iter(&self) -> impl Iterator<Item = &PlayerListEntry> {
        self.entries.values()
    }

    /// The listed entries in render order, matching vanilla's comparator:
    /// higher `list_order` first, then spectators last, then profile name
    /// case-insensitively.
    ///
    /// Vanilla inserts a team-name tie-break between spectator and name; since
    /// team membership is owned by the scoreboard, callers that need the exact
    /// ordering supply it via [`ordered_by`](Self::ordered_by). This default
    /// omits the team key.
    #[must_use]
    pub fn ordered(&self) -> Vec<&PlayerListEntry> {
        self.ordered_by(|_| "")
    }

    /// Like [`ordered`](Self::ordered) but with a caller-supplied team-name key
    /// (empty string when a player has no team) for the vanilla tie-break.
    #[must_use]
    pub fn ordered_by<'a, F>(&'a self, team_of: F) -> Vec<&'a PlayerListEntry>
    where
        F: Fn(&Uuid) -> &'a str,
    {
        let mut v: Vec<&PlayerListEntry> = self.entries.values().filter(|e| e.listed).collect();
        v.sort_by(|a, b| {
            b.list_order
                .cmp(&a.list_order)
                .then_with(|| spectator_rank(a).cmp(&spectator_rank(b)))
                .then_with(|| team_of(&a.profile.id).cmp(team_of(&b.profile.id)))
                .then_with(|| {
                    a.profile
                        .name
                        .to_lowercase()
                        .cmp(&b.profile.name.to_lowercase())
                })
        });
        v
    }
}

fn spectator_rank(e: &PlayerListEntry) -> u8 {
    u8::from(e.game_mode == GameMode::Spectator)
}
