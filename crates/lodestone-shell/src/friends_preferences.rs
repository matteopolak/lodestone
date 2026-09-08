//! Account-scoped Friends presentation preferences.
//!
//! The Friends service owns whether the account participates and accepts
//! requests. This module owns the two client-only choices that never belong on
//! the service wire: whether relationship changes may produce an in-world
//! notification and how much local activity is published as presence.
//!
//! The store is deliberately separate from [`crate::config::Options`]. Options
//! are one global document, while these values belong to a selected online
//! profile and must not follow the next account when the player switches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lodestone_auth::friends::PresenceStatus;
use uuid::Uuid;

const STORE_VERSION: u64 = 1;

/// The amount of local activity an account publishes to Friends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FriendsPresenceVisibility {
    /// Publish the concrete activity, such as a world or remote server.
    Full,
    /// Publish only whether the client is online; do not identify the activity.
    Limited,
    /// Publish an offline presence regardless of local activity.
    Hidden,
}

impl Default for FriendsPresenceVisibility {
    fn default() -> Self {
        Self::Full
    }
}

impl FriendsPresenceVisibility {
    /// The value shown in the Friends settings row.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::Limited => "Limited",
            Self::Hidden => "Hidden",
        }
    }

    /// The next value in the settings cycle.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Full => Self::Limited,
            Self::Limited => Self::Hidden,
            Self::Hidden => Self::Full,
        }
    }

    /// Applies this local privacy choice to the activity the runtime observed.
    ///
    /// Limited keeps only the binary online/offline signal. Hidden intentionally
    /// maps every status to Offline, including a transient or unknown status, so
    /// it cannot disclose activity through a fallback branch.
    #[must_use]
    pub const fn apply(self, activity: PresenceStatus) -> PresenceStatus {
        match self {
            Self::Full => activity,
            Self::Limited => match activity {
                PresenceStatus::Offline => PresenceStatus::Offline,
                _ => PresenceStatus::Online,
            },
            Self::Hidden => PresenceStatus::Offline,
        }
    }

    fn from_json(value: Option<&serde_json::Value>) -> Self {
        match value.and_then(serde_json::Value::as_str) {
            Some("limited") => Self::Limited,
            Some("hidden") => Self::Hidden,
            _ => Self::Full,
        }
    }

    fn as_json(self) -> serde_json::Value {
        self.label().to_ascii_lowercase().into()
    }
}

/// Client-only Friends choices for one online account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FriendsLocalPreferences {
    /// Whether relationship changes can reach the HUD while a world is open.
    pub in_game_notifications: bool,
    /// The account's presence publication level.
    pub presence_visibility: FriendsPresenceVisibility,
}

impl Default for FriendsLocalPreferences {
    fn default() -> Self {
        Self {
            in_game_notifications: false,
            presence_visibility: FriendsPresenceVisibility::Full,
        }
    }
}

/// Versioned, profile-keyed persistence for [`FriendsLocalPreferences`].
#[derive(Debug, Clone)]
pub struct FriendsPreferencesStore {
    path: PathBuf,
    accounts: BTreeMap<Uuid, FriendsLocalPreferences>,
}

impl FriendsPreferencesStore {
    /// Load a store. Missing, malformed, or unsupported documents degrade to an
    /// empty store so account selection is never blocked by local preferences.
    #[must_use]
    pub fn load_from(path: PathBuf) -> Self {
        let accounts = crate::platform::store::read_text(&path)
            .ok()
            .and_then(|text| Self::parse(&text))
            .unwrap_or_default();
        Self { path, accounts }
    }

    /// Load the production store next to the other shell documents.
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(friends_preferences_path())
    }

    /// Read one account's choices, using the safe defaults when it has no row.
    #[must_use]
    pub fn get(&self, profile_id: Uuid) -> FriendsLocalPreferences {
        self.accounts.get(&profile_id).copied().unwrap_or_default()
    }

    /// Replace one account's choices and persist the document immediately.
    ///
    /// The in-memory value changes before the write, matching the other menu
    /// stores: a failed write is reported by the caller, while the active frame
    /// still reflects exactly what the player selected.
    pub fn set(
        &mut self,
        profile_id: Uuid,
        preferences: FriendsLocalPreferences,
    ) -> std::io::Result<()> {
        self.accounts.insert(profile_id, preferences);
        self.save()
    }

    /// Persist this store to its configured path.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&self.path)
    }

    /// Persist to an explicit path. Primarily useful for hermetic tests.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        crate::platform::store::write_text(path, &self.to_json())
    }

    fn parse(text: &str) -> Option<BTreeMap<Uuid, FriendsLocalPreferences>> {
        let serde_json::Value::Object(root) = serde_json::from_str(text).ok()? else {
            return None;
        };
        if root.get("version").and_then(serde_json::Value::as_u64) != Some(STORE_VERSION) {
            return None;
        }
        let serde_json::Value::Object(accounts) = root.get("accounts")? else {
            return None;
        };
        Some(
            accounts
                .iter()
                .filter_map(|(id, value)| {
                    let profile_id = Uuid::parse_str(id).ok()?;
                    let serde_json::Value::Object(preferences) = value else {
                        return None;
                    };
                    Some((
                        profile_id,
                        FriendsLocalPreferences {
                            in_game_notifications: preferences
                                .get("in_game_notifications")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false),
                            presence_visibility: FriendsPresenceVisibility::from_json(
                                preferences.get("presence_visibility"),
                            ),
                        },
                    ))
                })
                .collect(),
        )
    }

    fn to_json(&self) -> String {
        let accounts = self
            .accounts
            .iter()
            .map(|(profile_id, preferences)| {
                let mut value = serde_json::Map::new();
                value.insert(
                    "in_game_notifications".into(),
                    preferences.in_game_notifications.into(),
                );
                value.insert(
                    "presence_visibility".into(),
                    preferences.presence_visibility.as_json(),
                );
                (profile_id.to_string(), serde_json::Value::Object(value))
            })
            .collect::<serde_json::Map<_, _>>();
        let mut root = serde_json::Map::new();
        root.insert("version".into(), STORE_VERSION.into());
        root.insert("accounts".into(), serde_json::Value::Object(accounts));
        serde_json::to_string_pretty(&serde_json::Value::Object(root))
            .expect("Friends preferences JSON is composed only of finite values")
    }
}

/// The production path for the profile-keyed Friends document.
#[must_use]
pub fn friends_preferences_path() -> PathBuf {
    lodestone_auth::paths::data_dir().join("friends.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lodestone-friends-preferences-{name}-{}",
            Uuid::new_v4()
        ))
    }

    #[test]
    fn missing_accounts_use_private_defaults() {
        let store = FriendsPreferencesStore::load_from(path("missing"));
        assert_eq!(store.get(Uuid::from_u128(1)), FriendsLocalPreferences::default());
    }

    #[test]
    fn profile_values_round_trip_without_tokens_or_service_fields() {
        let file = path("round-trip");
        let profile_id = Uuid::from_u128(7);
        let other_profile_id = Uuid::from_u128(8);
        let mut store = FriendsPreferencesStore::load_from(file.clone());
        let preferences = FriendsLocalPreferences {
            in_game_notifications: true,
            presence_visibility: FriendsPresenceVisibility::Limited,
        };
        store.set(profile_id, preferences).expect("temp preferences file");
        store
            .set(
                other_profile_id,
                FriendsLocalPreferences {
                    presence_visibility: FriendsPresenceVisibility::Hidden,
                    ..FriendsLocalPreferences::default()
                },
            )
            .expect("other profile preferences file");

        let text = crate::platform::store::read_text(&file).expect("preferences JSON");
        assert!(!text.contains("token"));
        assert!(!text.contains("friendsPreferences"));
        let loaded = FriendsPreferencesStore::load_from(file);
        assert_eq!(loaded.get(profile_id), preferences);
        assert_eq!(
            loaded.get(other_profile_id).presence_visibility,
            FriendsPresenceVisibility::Hidden
        );
    }

    #[test]
    fn malformed_or_future_documents_degrade_to_defaults() {
        let malformed = path("malformed");
        crate::platform::store::write_text(&malformed, "not json").expect("malformed fixture");
        assert_eq!(FriendsPreferencesStore::load_from(malformed).accounts.len(), 0);

        let future = path("future");
        crate::platform::store::write_text(&future, r#"{"version":2,"accounts":{}}"#)
            .expect("future fixture");
        assert_eq!(FriendsPreferencesStore::load_from(future).accounts.len(), 0);
    }

    #[test]
    fn visibility_cycle_and_projection_keep_the_privacy_contract() {
        assert_eq!(FriendsPresenceVisibility::Full.next(), FriendsPresenceVisibility::Limited);
        assert_eq!(FriendsPresenceVisibility::Limited.next(), FriendsPresenceVisibility::Hidden);
        assert_eq!(FriendsPresenceVisibility::Hidden.next(), FriendsPresenceVisibility::Full);
        assert_eq!(
            FriendsPresenceVisibility::Limited.apply(PresenceStatus::Server),
            PresenceStatus::Online
        );
        assert_eq!(
            FriendsPresenceVisibility::Limited.apply(PresenceStatus::Unknown),
            PresenceStatus::Online
        );
        assert_eq!(
            FriendsPresenceVisibility::Hidden.apply(PresenceStatus::Server),
            PresenceStatus::Offline
        );
        assert_eq!(
            FriendsPresenceVisibility::Full.apply(PresenceStatus::LanWorld),
            PresenceStatus::LanWorld
        );
    }
}
