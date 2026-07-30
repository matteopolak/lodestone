//! Non-secret account metadata: which Microsoft/Minecraft accounts are known
//! locally, and which one is selected — everything the account switcher
//! (issue #63) needs to draw its list **without unlocking the keychain**.
//! See [`crate::store`] for where the actual refresh tokens live, and
//! `docs/accounts.md` for why the two are deliberately split.
//!
//! Persisted at [`crate::paths::profiles_path`], beside `servers.json` and
//! `options.json`. `lodestone-shell/src/config.rs` establishes that home and
//! would be the natural place for this type too, but that module is held by
//! another agent in this session, so both the path helper ([`crate::paths`])
//! and this type live in `lodestone-auth` instead — see that module's docs
//! for the tradeoff.
//!
//! Parsing follows the same rule `lodestone-shell`'s `Options`/`Keybinds`
//! establish (`docs/keybindings.md`): a missing or corrupt file is silently
//! the empty default, and one malformed entry costs only itself, never the
//! rest of the file. See [`AccountsMetadata::from_json`] for the exact rules.

use std::path::Path;

use uuid::Uuid;

/// One locally-known account: enough to draw a row in an account switcher
/// without touching the keychain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountProfile {
    /// The Minecraft profile UUID — also the key used in the OS keychain
    /// (see [`crate::store::SecretStore`]).
    pub profile_id: Uuid,
    /// The player's username as of the last successful sign-in or refresh.
    pub username: String,
    /// A URL to the account's skin, if known. Always a pointer, never a
    /// local file path or embedded image data.
    pub skin_url: Option<String>,
    /// Unix timestamp (seconds) this account last completed a sign-in or
    /// token refresh. Lets the switcher sort most-recently-used first.
    pub last_used: u64,
}

/// The full contents of `profiles.json`: every known account, plus which one
/// is currently selected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountsMetadata {
    /// The profile the shell should use without asking, or `None` if nothing
    /// has ever been selected (e.g. a fresh install, or every account was
    /// removed).
    pub selected: Option<Uuid>,
    /// Every known account, in no particular persisted order.
    pub profiles: Vec<AccountProfile>,
}

impl AccountsMetadata {
    /// Loads from the real on-disk location ([`crate::paths::profiles_path`]).
    /// Missing or corrupt is the empty default, never an error or a panic.
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(&crate::paths::profiles_path())
    }

    /// As [`Self::load`], from an explicit path (for tests, so nothing
    /// touches a developer's real metadata file).
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path).map_or_else(|_| Self::default(), |t| Self::from_json(&t))
    }

    /// Parses `text`, degrading field-by-field and entry-by-entry instead of
    /// failing outright:
    ///
    /// * a top level that is not a JSON object yields the full default
    ///   (`selected: None`, `profiles: []`);
    /// * a missing or invalid `selected` is `None` — it does not affect
    ///   `profiles`;
    /// * a missing or non-array `profiles` is treated as empty;
    /// * an element of `profiles` that is not an object, or is missing (or
    ///   has an invalid-shaped) `profile_id` or `username`, is **skipped** —
    ///   only that one entry is lost; every other entry, before or after it,
    ///   still loads;
    /// * `skin_url`/`last_used` are independently optional per entry
    ///   (missing or wrong-typed defaults to `None`/`0`) and never invalidate
    ///   the rest of that entry.
    #[must_use]
    pub fn from_json(text: &str) -> Self {
        let Ok(serde_json::Value::Object(obj)) = serde_json::from_str(text) else {
            return Self::default();
        };
        let selected = obj
            .get("selected")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok());
        let profiles = obj
            .get("profiles")
            .and_then(serde_json::Value::as_array)
            .map(|arr| arr.iter().filter_map(profile_from_json).collect())
            .unwrap_or_default();
        Self { selected, profiles }
    }

    /// Adds `profile`, replacing any existing entry with the same
    /// [`AccountProfile::profile_id`] rather than duplicating it.
    pub fn upsert(&mut self, profile: AccountProfile) {
        if let Some(existing) = self
            .profiles
            .iter_mut()
            .find(|p| p.profile_id == profile.profile_id)
        {
            *existing = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    /// Removes the entry for `profile_id`, if present, clearing
    /// [`Self::selected`] too if it pointed at the removed entry.
    pub fn remove(&mut self, profile_id: Uuid) {
        self.profiles.retain(|p| p.profile_id != profile_id);
        if self.selected == Some(profile_id) {
            self.selected = None;
        }
    }

    /// The exact JSON value [`Self::save_to`] writes — exposed so tests (and
    /// any caller that wants the text without touching a file) don't have to
    /// duplicate the shape.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "selected".into(),
            self.selected
                .map_or(serde_json::Value::Null, |id| serde_json::Value::String(id.to_string())),
        );
        obj.insert(
            "profiles".into(),
            serde_json::Value::Array(self.profiles.iter().map(profile_to_json).collect()),
        );
        serde_json::Value::Object(obj)
    }

    /// Writes to the real on-disk location.
    ///
    /// # Errors
    /// Returns the underlying I/O error if the directory cannot be created or
    /// the file cannot be written.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&crate::paths::profiles_path())
    }

    /// As [`Self::save`], to an explicit path (for tests).
    ///
    /// # Errors
    /// Returns the underlying I/O error if the directory cannot be created or
    /// the file cannot be written.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text =
            serde_json::to_string_pretty(&self.to_json()).unwrap_or_else(|_| "{}".to_owned());
        std::fs::write(path, text)
    }
}

fn profile_from_json(value: &serde_json::Value) -> Option<AccountProfile> {
    let obj = value.as_object()?;
    let profile_id = obj
        .get("profile_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())?;
    let username = obj.get("username").and_then(serde_json::Value::as_str)?.to_owned();
    let skin_url = obj
        .get("skin_url")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let last_used = obj
        .get("last_used")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some(AccountProfile {
        profile_id,
        username,
        skin_url,
        last_used,
    })
}

fn profile_to_json(profile: &AccountProfile) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "profile_id".into(),
        serde_json::Value::String(profile.profile_id.to_string()),
    );
    obj.insert(
        "username".into(),
        serde_json::Value::String(profile.username.clone()),
    );
    obj.insert(
        "skin_url".into(),
        profile
            .skin_url
            .clone()
            .map_or(serde_json::Value::Null, serde_json::Value::String),
    );
    obj.insert("last_used".into(), serde_json::Value::from(profile.last_used));
    serde_json::Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AccountsMetadata {
        let id = Uuid::parse_str("069a79f4-44e9-4726-a5be-fca90e38aaf5").unwrap();
        AccountsMetadata {
            selected: Some(id),
            profiles: vec![AccountProfile {
                profile_id: id,
                username: "Notch".to_owned(),
                skin_url: Some("https://textures.minecraft.net/texture/abc123".to_owned()),
                last_used: 1_700_000_000,
            }],
        }
    }

    // -- literal-JSON evidence, both directions --------------------------
    //
    // `load(save(x)) == x` alone would be satisfied by two symmetric
    // misunderstandings of the shape, so both directions are checked against
    // a JSON string that was not produced by this module's own code.
    //
    // `serde_json::Map` is `BTreeMap`-backed in this workspace (the
    // `preserve_order` feature is not enabled anywhere in the dependency
    // graph — see `Cargo.lock`), so object keys always serialise in
    // alphabetical order regardless of insertion order: `profiles` before
    // `selected` at the top level, `last_used`/`profile_id`/`skin_url`/
    // `username` within each entry. That is a property of the library, not a
    // deliberate design choice, and is spelled out in `docs/accounts.md`.

    const EXPECTED_JSON: &str = r#"{
  "profiles": [
    {
      "last_used": 1700000000,
      "profile_id": "069a79f4-44e9-4726-a5be-fca90e38aaf5",
      "skin_url": "https://textures.minecraft.net/texture/abc123",
      "username": "Notch"
    }
  ],
  "selected": "069a79f4-44e9-4726-a5be-fca90e38aaf5"
}"#;

    #[test]
    fn saving_produces_the_exact_hand_written_json_shape() {
        let text = serde_json::to_string_pretty(&sample().to_json()).unwrap();
        assert_eq!(text, EXPECTED_JSON);
    }

    #[test]
    fn loading_the_hand_written_json_produces_the_expected_value() {
        assert_eq!(AccountsMetadata::from_json(EXPECTED_JSON), sample());
    }

    // -- tolerant parsing -------------------------------------------------

    #[test]
    fn a_missing_or_non_object_top_level_is_the_empty_default() {
        for text in ["", "not json", "[1,2,3]", "null", "42", "\"str\""] {
            assert_eq!(
                AccountsMetadata::from_json(text),
                AccountsMetadata::default(),
                "input: {text:?}"
            );
        }
    }

    #[test]
    fn a_missing_selected_or_profiles_key_is_independently_defaulted() {
        let meta = AccountsMetadata::from_json(r#"{"profiles":[]}"#);
        assert_eq!(meta.selected, None);
        assert_eq!(meta.profiles, vec![]);

        let id = Uuid::new_v4();
        let meta = AccountsMetadata::from_json(&format!(r#"{{"selected":"{id}"}}"#));
        assert_eq!(meta.selected, Some(id));
        assert_eq!(meta.profiles, vec![]);
    }

    #[test]
    fn a_non_array_profiles_value_degrades_to_empty_without_losing_selected() {
        let id = Uuid::new_v4();
        for bad in ["\"nope\"", "{}", "null", "17"] {
            let meta =
                AccountsMetadata::from_json(&format!(r#"{{"selected":"{id}","profiles":{bad}}}"#));
            assert_eq!(meta.profiles, vec![], "profiles: {bad}");
            assert_eq!(meta.selected, Some(id), "selected must survive profiles: {bad}");
        }
    }

    #[test]
    fn an_invalid_selected_value_is_none_without_costing_profiles() {
        let good = sample();
        let json_text = serde_json::to_string(&good.to_json()).unwrap();
        // Corrupt just the `selected` field by re-parsing and mutating.
        let mut value: serde_json::Value = serde_json::from_str(&json_text).unwrap();
        value["selected"] = serde_json::Value::String("not-a-uuid".to_owned());
        let corrupted = serde_json::to_string(&value).unwrap();

        let meta = AccountsMetadata::from_json(&corrupted);
        assert_eq!(meta.selected, None);
        assert_eq!(meta.profiles, good.profiles);
    }

    #[test]
    fn one_malformed_profile_entry_costs_only_itself() {
        let good_id = Uuid::new_v4();
        let text = format!(
            r#"{{
                "selected": null,
                "profiles": [
                    {{"profile_id": "{good_id}", "username": "Alice"}},
                    {{"profile_id": "not-a-uuid", "username": "Bob"}},
                    {{"username": "NoId"}},
                    {{"profile_id": "{good_id2}"}},
                    "not-an-object",
                    42,
                    {{"profile_id": "{good_id3}", "username": "Carol", "skin_url": 5, "last_used": "oops"}}
                ]
            }}"#,
            good_id2 = Uuid::new_v4(),
            good_id3 = Uuid::new_v4(),
        );
        let meta = AccountsMetadata::from_json(&text);
        // Alice (valid) and Carol (valid id/username, but garbage-typed
        // optional fields that must default rather than reject the entry)
        // survive; the four broken entries in between are silently skipped,
        // not fatal to the rest of the array.
        assert_eq!(meta.profiles.len(), 2, "{:#?}", meta.profiles);
        assert_eq!(meta.profiles[0].username, "Alice");
        assert_eq!(meta.profiles[0].profile_id, good_id);
        assert_eq!(meta.profiles[1].username, "Carol");
        assert_eq!(meta.profiles[1].skin_url, None, "bad-typed skin_url must default to None");
        assert_eq!(meta.profiles[1].last_used, 0, "bad-typed last_used must default to 0");
    }

    // -- upsert / remove ----------------------------------------------------

    #[test]
    fn upsert_replaces_by_profile_id_rather_than_duplicating() {
        let mut meta = AccountsMetadata::default();
        let id = Uuid::new_v4();
        meta.upsert(AccountProfile {
            profile_id: id,
            username: "Old".to_owned(),
            skin_url: None,
            last_used: 1,
        });
        meta.upsert(AccountProfile {
            profile_id: id,
            username: "New".to_owned(),
            skin_url: None,
            last_used: 2,
        });
        assert_eq!(meta.profiles.len(), 1);
        assert_eq!(meta.profiles[0].username, "New");
    }

    #[test]
    fn remove_clears_selected_only_when_it_pointed_at_the_removed_profile() {
        let mut meta = sample();
        let id = meta.profiles[0].profile_id;
        let other = Uuid::new_v4();
        meta.upsert(AccountProfile {
            profile_id: other,
            username: "Other".to_owned(),
            skin_url: None,
            last_used: 0,
        });
        meta.selected = Some(other);
        meta.remove(id);
        assert_eq!(meta.profiles.len(), 1);
        assert_eq!(meta.selected, Some(other), "unrelated selection must survive");

        meta.remove(other);
        assert_eq!(meta.profiles.len(), 0);
        assert_eq!(meta.selected, None, "selection pointing at the removed profile must clear");
    }

    // -- real file round trip -----------------------------------------------

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lodestone-auth-metadata-test-{}-{tag}/profiles.json",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        path
    }

    #[test]
    fn round_trips_through_a_real_file() {
        let path = temp_path("roundtrip");
        let meta = sample();
        meta.save_to(&path).expect("save should create parent dirs");
        assert_eq!(AccountsMetadata::load_from(&path), meta);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_or_corrupt_file_is_the_default_not_an_error() {
        assert_eq!(
            AccountsMetadata::load_from(Path::new("/nonexistent/profiles.json")),
            AccountsMetadata::default()
        );
        let path = temp_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "}{ not json").unwrap();
        assert_eq!(AccountsMetadata::load_from(&path), AccountsMetadata::default());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_unknown_future_version_key_is_ignored_rather_than_rejected() {
        // A hypothetical future writer adds a top-level "version" key; this
        // reader must still parse everything it understands rather than
        // bailing out because of one unrecognised key.
        let id = Uuid::new_v4();
        let text = format!(
            r#"{{"version": 2, "selected": "{id}", "profiles": [], "extra": {{"nested": true}}}}"#
        );
        let meta = AccountsMetadata::from_json(&text);
        assert_eq!(meta.selected, Some(id));
        assert_eq!(meta.profiles, vec![]);
    }
}
