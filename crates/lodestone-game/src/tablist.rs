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

use lodestone_model::event as m;
use lodestone_model::{ClientEvent, GameMode, Text};
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

/// A player's announced chat-signing session (vanilla's `RemoteChatSession`):
/// their session id and Mojang-issued public key, needed to verify a signed
/// message from them (`lodestone_auth::verify_signature`).
///
/// Kept as its own type rather than reusing
/// `lodestone_model::event::ChatSessionInfo` directly, matching this module's
/// own precedent for [`ProfileProperty`]: the game layer owns its own shapes
/// rather than speaking the wire model's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteChatSession {
    /// This player's chat-session UUID — half of the `SignedMessageLink`
    /// their signed messages are hashed against.
    pub session_id: Uuid,
    /// DER-encoded (X.509 `SubjectPublicKeyInfo`) RSA public key.
    pub public_key: Vec<u8>,
    /// Public-key expiry, epoch milliseconds.
    pub expires_at: i64,
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
    /// This player's announced chat-signing session, when the server has
    /// sent one (`INITIALIZE_CHAT`) — the receiving half of secure chat.
    /// `None` means either "never announced" or "not yet
    /// folded"; the two are indistinguishable here for the same reason
    /// [`Self::profile`]'s properties collapse an analogous pair — see
    /// `lodestone_model::event::PlayerListEntry::chat_session`'s doc.
    pub chat_session: Option<RemoteChatSession>,
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
            chat_session: None,
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

// --- ClientEvent fold -------------------------------------------------------
//
// Folds `PlayerListUpdate` / `PlayerListRemove` into the list. The model type is
// imported as `m`. Unlike a naive replace-by-uuid, this merges partial updates:
// each field of a model entry is `Some` only when the update carried it, so an
// existing entry keeps fields the update omitted. That makes the fold correct
// whether an adapter emits full snapshots or per-field deltas.
//
// The model gap this comment used to name is **closed**:
// `m::PlayerListEntry::properties` now carries the `ADD_PLAYER` profile-property
// multimap, and `fold_entry` seeds `GameProfile::properties` from it, so a folded
// profile has its `textures` blob.
//
// Note the merge rule for it, which differs from the scalars above: `None` means
// the update had no `ADD_PLAYER` action, so the existing properties are **kept**.
// `Some(vec![])` means it did and the profile genuinely has none — an offline-mode
// server. Collapsing those two would clear a skin on every latency ping.

impl TabList {
    /// Folds a tab-list [`ClientEvent`] into this state, returning whether the
    /// event was one this aggregate owns.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::PlayerListUpdate { entries } => {
                for entry in entries {
                    self.fold_entry(entry);
                }
                true
            }
            ClientEvent::PlayerListRemove { profile_ids } => {
                for id in profile_ids {
                    self.remove(id);
                }
                true
            }
            // The header/footer text, from `ClientboundTabListPacket`. This was
            // a genuine island, and it stayed one for longer than this comment
            // used to admit. The old wording — "read downstream by `hud.rs`'s
            // snapshot" — was *literally* true and *practically* wrong, which is
            // why it survived review: the reader it named is this crate's own
            // `hud.rs` (`HudSnapshot::assemble`'s `tab_header`/`tab_footer`),
            // and **`HudSnapshot` has no consumer in `lodestone-shell` at all**.
            // The shell builds its own `HudFrame`, so both fields terminated in
            // a read model only tests exercise. Two hops, one of them dead.
            //
            // Closed on the shell side by `lodestone_shell::sim::Sim::tab_banner`
            // → `HudFrame::tab_header`/`tab_footer` → `hud.rs`'s tab-overlay
            // block, which draws them centred above and below the player rows.
            // `HudSnapshot` itself is still unwired to any renderer.
            //
            // The lesson, not the fact: naming a reader by *file* across a
            // workspace with two `hud.rs` files is how a true sentence hides a
            // dead wire. Name the crate.
            ClientEvent::TabListChanged { header, footer } => {
                self.header = Some(header.clone());
                self.footer = Some(footer.clone());
                true
            }
            _ => false,
        }
    }

    /// Merges one model entry: updates the present (`Some`) fields of an existing
    /// entry, or creates a new one keyed by uuid. A brand-new entry with no name
    /// in the update falls back to an empty profile name.
    fn fold_entry(&mut self, e: &m::PlayerListEntry) {
        match self.get_mut(&e.uuid) {
            Some(existing) => {
                if let Some(gm) = e.game_mode {
                    existing.game_mode = gm;
                }
                if let Some(latency) = e.latency {
                    existing.latency = latency;
                }
                if let Some(display_name) = &e.display_name {
                    existing.display_name = Some(display_name.clone());
                }
                if let Some(listed) = e.listed {
                    existing.listed = listed;
                }
                // Only when the update actually carried `ADD_PLAYER`; see the
                // note above this impl on why `None` and `Some(vec![])` differ.
                if let Some(properties) = &e.properties {
                    existing.profile.properties = properties
                        .iter()
                        .map(|property| ProfileProperty {
                            name: property.name.clone(),
                            value: property.value.clone(),
                            signature: property.signature.clone(),
                        })
                        .collect();
                }
                // Same merge rule as `properties` just above: `None` means
                // this delta carried no `INITIALIZE_CHAT`, so the existing
                // session (if any) survives a latency-only update untouched.
                if let Some(session) = &e.chat_session {
                    existing.chat_session = Some(RemoteChatSession {
                        session_id: session.session_id,
                        public_key: session.public_key.clone(),
                        expires_at: session.expires_at,
                    });
                }
                if let Some(list_order) = e.list_order {
                    existing.list_order = list_order;
                }
                if let Some(hat_visible) = e.hat_visible {
                    existing.show_hat = hat_visible;
                }
            }
            None => {
                let name = e.name.clone().unwrap_or_default();
                let mut entry = PlayerListEntry::new(GameProfile::new(e.uuid, name));
                if let Some(gm) = e.game_mode {
                    entry.game_mode = gm;
                }
                if let Some(latency) = e.latency {
                    entry.latency = latency;
                }
                entry.display_name = e.display_name.clone();
                if let Some(listed) = e.listed {
                    entry.listed = listed;
                }
                if let Some(properties) = &e.properties {
                    entry.profile.properties = properties
                        .iter()
                        .map(|property| ProfileProperty {
                            name: property.name.clone(),
                            value: property.value.clone(),
                            signature: property.signature.clone(),
                        })
                        .collect();
                }
                if let Some(session) = &e.chat_session {
                    entry.chat_session = Some(RemoteChatSession {
                        session_id: session.session_id,
                        public_key: session.public_key.clone(),
                        expires_at: session.expires_at,
                    });
                }
                if let Some(list_order) = e.list_order {
                    entry.list_order = list_order;
                }
                if let Some(hat_visible) = e.hat_visible {
                    entry.show_hat = hat_visible;
                }
                self.insert(entry);
            }
        }
    }
}

#[cfg(test)]
mod fold_tests {
    use super::*;

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn add_entry(id: Uuid, name: &str, mode: GameMode, latency: i32) -> m::PlayerListEntry {
        m::PlayerListEntry {
            uuid: id,
            name: Some(name.to_string()),
            game_mode: Some(mode),
            latency: Some(latency),
            display_name: None,
            listed: Some(true),
            properties: None,
            chat_session: None,
            list_order: None,
            hat_visible: None,
        }
    }

    #[test]
    fn add_is_readable_through_public_api() {
        let mut tabs = TabList::new();
        let id = uid(1);
        assert!(tabs.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![add_entry(id, "Alice", GameMode::Creative, 42)],
        }));
        let entry = tabs.get(&id).expect("entry present");
        assert_eq!(entry.profile.name, "Alice");
        assert_eq!(entry.game_mode, GameMode::Creative);
        assert_eq!(entry.latency, 42);
        assert!(entry.listed);
    }

    /// `UPDATE_LIST_ORDER`/`UPDATE_HAT` used to be decoded and discarded in
    /// the protocol crate, so `PlayerListEntry::list_order`/`show_hat` here
    /// never left their constructor defaults (`0`/`true`) no matter what a
    /// server sent. Distinct, non-default values on both so a coincidental
    /// pass against the defaults cannot hide a missed wire.
    #[test]
    fn list_order_and_hat_visibility_are_folded_in() {
        let mut tabs = TabList::new();
        let id = uid(30);
        tabs.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![m::PlayerListEntry {
                uuid: id,
                name: Some("Frank".into()),
                game_mode: Some(GameMode::Survival),
                latency: Some(1),
                display_name: None,
                listed: Some(true),
                properties: None,
                chat_session: None,
                list_order: Some(9),
                hat_visible: Some(false),
            }],
        });
        let entry = tabs.get(&id).expect("entry present");
        assert_eq!(entry.list_order, 9);
        assert!(!entry.show_hat);

        // A delta that omits both actions must keep them, not reset to the
        // constructor defaults -- same merge rule as every other field here.
        tabs.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![m::PlayerListEntry {
                uuid: id,
                name: None,
                game_mode: None,
                latency: Some(2),
                display_name: None,
                listed: None,
                properties: None,
                chat_session: None,
                list_order: None,
                hat_visible: None,
            }],
        });
        let entry = tabs.get(&id).expect("entry present");
        assert_eq!(entry.list_order, 9, "must survive a delta without UPDATE_LIST_ORDER");
        assert!(!entry.show_hat, "must survive a delta without UPDATE_HAT");
    }

    #[test]
    fn partial_update_merges_and_keeps_untouched_fields() {
        let mut tabs = TabList::new();
        let id = uid(2);
        tabs.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![add_entry(id, "Bob", GameMode::Survival, 10)],
        });
        // A latency-only delta must not wipe the name or game mode.
        tabs.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![m::PlayerListEntry {
                uuid: id,
                name: None,
                game_mode: None,
                latency: Some(250),
                display_name: None,
                listed: None,
                properties: None,
                chat_session: None,
                list_order: None,
                hat_visible: None,
            }],
        });
        let entry = tabs.get(&id).expect("entry present");
        assert_eq!(entry.profile.name, "Bob");
        assert_eq!(entry.game_mode, GameMode::Survival);
        assert_eq!(entry.latency, 250);
        assert!(entry.listed);
    }

    /// Issue #62's merge rule, and the one that has a user-visible failure mode:
    /// a latency-only delta carries `properties: None` and **must not clear** the
    /// skin. `Some(vec![])` is the different case — an offline-mode server saying
    /// the profile genuinely has none.
    #[test]
    fn a_delta_without_add_player_keeps_the_existing_profile_properties() {
        let mut tabs = TabList::new();
        let id = uid(9);
        tabs.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![m::PlayerListEntry {
                uuid: id,
                name: Some("Dana".into()),
                game_mode: Some(GameMode::Survival),
                latency: Some(1),
                display_name: None,
                listed: Some(true),
                properties: Some(vec![m::ProfileProperty {
                    name: "textures".into(),
                    value: "eyJ0ZXh0dXJlcyI6e319".into(),
                    signature: Some("sig".into()),
                }]),
                chat_session: None,
                list_order: None,
                hat_visible: None,
            }],
        });
        assert_eq!(
            tabs.get(&id).expect("entry present").profile.properties.len(),
            1,
            "the textures property must survive the fold at all"
        );

        // A latency ping: `properties` absent.
        tabs.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![m::PlayerListEntry {
                uuid: id,
                name: None,
                game_mode: None,
                latency: Some(99),
                display_name: None,
                listed: None,
                properties: None,
                chat_session: None,
                list_order: None,
                hat_visible: None,
            }],
        });
        let entry = tabs.get(&id).expect("entry present");
        assert_eq!(entry.latency, 99);
        assert_eq!(
            entry.profile.properties.len(),
            1,
            "a delta with no ADD_PLAYER must keep the skin -- clearing it here              would drop every remote skin on the next latency ping"
        );
        assert_eq!(entry.profile.properties[0].name, "textures");

        // The control for the distinction: an explicit empty set *does* clear.
        tabs.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![m::PlayerListEntry {
                uuid: id,
                name: None,
                game_mode: None,
                latency: None,
                display_name: None,
                listed: None,
                properties: Some(Vec::new()),
                chat_session: None,
                list_order: None,
                hat_visible: None,
            }],
        });
        assert!(
            tabs.get(&id)
                .expect("entry present")
                .profile
                .properties
                .is_empty(),
            "Some(vec![]) means the profile really has none, unlike None"
        );
    }

    /// The real gap here, closed: `INITIALIZE_CHAT`'s session used to be
    /// decoded and have nowhere to go once it reached this layer. Same shape
    /// as `a_delta_without_add_player_keeps_the_existing_profile_properties`:
    /// a session survives a delta that did not carry `INITIALIZE_CHAT`.
    #[test]
    fn a_chat_session_survives_a_delta_without_initialize_chat() {
        let mut tabs = TabList::new();
        let id = uid(20);
        let session_id = Uuid::from_u128(999);
        let public_key = vec![1, 2, 3, 4];
        tabs.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![m::PlayerListEntry {
                uuid: id,
                name: Some("Eve".into()),
                game_mode: Some(GameMode::Survival),
                latency: Some(1),
                display_name: None,
                listed: Some(true),
                properties: None,
                chat_session: Some(m::ChatSessionInfo {
                    session_id,
                    public_key: public_key.clone(),
                    expires_at: 1_700_000_000_000,
                }),
                list_order: None,
                hat_visible: None,
            }],
        });
        let entry = tabs.get(&id).expect("entry present");
        let session = entry.chat_session.as_ref().expect("session folded in");
        assert_eq!(session.session_id, session_id);
        assert_eq!(session.public_key, public_key);
        assert_eq!(session.expires_at, 1_700_000_000_000);

        // A latency-only delta: `chat_session` absent, must not clear it.
        tabs.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![m::PlayerListEntry {
                uuid: id,
                name: None,
                game_mode: None,
                latency: Some(50),
                display_name: None,
                listed: None,
                properties: None,
                chat_session: None,
                list_order: None,
                hat_visible: None,
            }],
        });
        let entry = tabs.get(&id).expect("entry present");
        assert_eq!(entry.latency, 50);
        assert!(
            entry.chat_session.is_some(),
            "a delta with no INITIALIZE_CHAT must keep the announced session"
        );
    }

    #[test]
    fn display_name_is_applied() {
        let mut tabs = TabList::new();
        let id = uid(3);
        tabs.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![m::PlayerListEntry {
                uuid: id,
                name: Some("Cara".into()),
                game_mode: Some(GameMode::Adventure),
                latency: Some(5),
                display_name: Some(Text::literal("[VIP] Cara")),
                listed: Some(true),
                properties: None,
                chat_session: None,
                list_order: None,
                hat_visible: None,
            }],
        });
        let entry = tabs.get(&id).expect("entry present");
        assert_eq!(entry.effective_name(), Text::literal("[VIP] Cara"));
    }

    #[test]
    fn remove_drops_entries() {
        let mut tabs = TabList::new();
        let a = uid(10);
        let b = uid(11);
        tabs.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![
                add_entry(a, "A", GameMode::Survival, 1),
                add_entry(b, "B", GameMode::Survival, 1),
            ],
        });
        assert_eq!(tabs.len(), 2);
        assert!(tabs.apply(&ClientEvent::PlayerListRemove {
            profile_ids: vec![a],
        }));
        assert_eq!(tabs.len(), 1);
        assert!(tabs.get(&a).is_none());
        assert!(tabs.get(&b).is_some());
    }

    #[test]
    fn non_tablist_event_is_not_claimed() {
        let mut tabs = TabList::new();
        assert!(!tabs.apply(&ClientEvent::BossBarUpdate {
            id: uid(1),
            action: m::BossAction::Remove,
        }));
    }

    /// `TabListChanged` sets header/footer through the fold — previously this
    /// was reachable only by a test poking the fields directly (see
    /// `hud_snapshot.rs`/`tests/tablist.rs`), because nothing fed the event
    /// through `apply` at all.
    #[test]
    fn tab_list_changed_sets_header_and_footer() {
        let mut tabs = TabList::new();
        assert!(tabs.apply(&ClientEvent::TabListChanged {
            header: Text::literal("Welcome"),
            footer: Text::literal("Bye"),
        }));
        assert_eq!(tabs.header, Some(Text::literal("Welcome")));
        assert_eq!(tabs.footer, Some(Text::literal("Bye")));
    }

    /// A later `TabListChanged` replaces, not merges — vanilla's
    /// `ClientboundTabListPacket` always carries both fields, never a partial
    /// update (unlike `PlayerListUpdate`'s per-field `Option`s).
    #[test]
    fn tab_list_changed_replaces_previous_header_and_footer() {
        let mut tabs = TabList::new();
        tabs.apply(&ClientEvent::TabListChanged {
            header: Text::literal("First"),
            footer: Text::literal("First footer"),
        });
        tabs.apply(&ClientEvent::TabListChanged {
            header: Text::literal("Second"),
            footer: Text::literal("Second footer"),
        });
        assert_eq!(tabs.header, Some(Text::literal("Second")));
        assert_eq!(tabs.footer, Some(Text::literal("Second footer")));
    }
}
