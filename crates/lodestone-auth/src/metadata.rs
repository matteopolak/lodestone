//! Non-secret account metadata: which Microsoft/Minecraft accounts are known
//! locally, and which one is selected — everything the account switcher
//! needs to draw its list **without unlocking the keychain**.
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

use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
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
    ///
    /// **wasm32**: `std::fs::read_to_string` always returns
    /// `Err(Unsupported)` on this target (measured; `CLAUDE.md`), which this
    /// function's own `map_or_else` was already built to treat as "missing
    /// file" rather than a hard error — so the browser build used to load the
    /// empty default on *every* launch, roster included, with no way to ever
    /// round-trip a saved account. The wasm32 arm below reaches
    /// `localStorage` instead, keyed by `path` exactly like
    /// [`Self::save_to`]'s wasm32 arm writes it, so the pair actually
    /// round-trips there.
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            let Some(storage) = local_storage() else {
                return Self::default();
            };
            let Ok(Some(text)) = storage.get_item(&storage_key(path)) else {
                return Self::default();
            };
            return Self::from_json(&text);
        }
        #[cfg(not(target_arch = "wasm32"))]
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
        let Ok(document) = serde_json::from_str::<AccountsMetadataDocument>(text) else {
            return Self::default();
        };
        document.into_metadata()
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
    /// the file cannot be written (native), or if `localStorage` cannot be
    /// reached at all (wasm32) — see [`Self::load_from`]'s doc.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let text =
            serde_json::to_string_pretty(&self.to_document()).unwrap_or_else(|_| "{}".to_owned());
        #[cfg(target_arch = "wasm32")]
        {
            let storage = local_storage()
                .ok_or_else(|| std::io::Error::other("no `localStorage` available in this browser"))?;
            return storage
                .set_item(&storage_key(path), &text)
                .map_err(|e| std::io::Error::other(format!("{e:?}")));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, text)
        }
    }

    fn to_document(&self) -> AccountsMetadataDocument {
        AccountsMetadataDocument {
            selected: self.selected,
            profiles: self
                .profiles
                .iter()
                .cloned()
                .map(AccountProfileDocument::from)
                .collect(),
        }
    }
}

/// The live `localStorage` handle, or `None` if this page has none — no
/// global `window` (a non-browser wasm host), or storage disabled/blocked by
/// the user's browser settings. wasm32-only; see [`AccountsMetadata::load_from`].
#[cfg(target_arch = "wasm32")]
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

/// The `localStorage` key [`AccountsMetadata::load_from`]/[`AccountsMetadata::save_to`]
/// use for `path` — the same [`Path`] the native arm would open, so a test
/// (or a caller) that points both at a distinct temp path on either target
/// cannot collide with a developer's real roster.
#[cfg(target_arch = "wasm32")]
fn storage_key(path: &Path) -> String {
    format!("lodestone:{}", path.to_string_lossy())
}

/// The typed on-disk roster document. Its optional fields are deliberate: the
/// reader uses them to distinguish an invalid entry (which is skipped) from a
/// valid entry whose optional fields should receive their defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AccountsMetadataDocument {
    #[serde(
        rename = "profiles",
        default,
        deserialize_with = "deserialize_profiles"
    )]
    profiles: Vec<AccountProfileDocument>,
    #[serde(
        rename = "selected",
        default,
        serialize_with = "serialize_optional_uuid",
        deserialize_with = "deserialize_optional_uuid"
    )]
    selected: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AccountProfileDocument {
    #[serde(
        rename = "last_used",
        default,
        deserialize_with = "deserialize_last_used"
    )]
    last_used: u64,
    #[serde(
        rename = "profile_id",
        default,
        serialize_with = "serialize_optional_uuid",
        deserialize_with = "deserialize_optional_uuid"
    )]
    profile_id: Option<Uuid>,
    #[serde(
        rename = "skin_url",
        default,
        deserialize_with = "deserialize_optional_string"
    )]
    skin_url: Option<String>,
    #[serde(
        rename = "username",
        default,
        deserialize_with = "deserialize_optional_string"
    )]
    username: Option<String>,
}

impl AccountsMetadataDocument {
    fn into_metadata(self) -> AccountsMetadata {
        AccountsMetadata {
            selected: self.selected,
            profiles: self
                .profiles
                .into_iter()
                .filter_map(AccountProfileDocument::into_profile)
                .collect(),
        }
    }
}

impl AccountProfileDocument {
    fn from(profile: AccountProfile) -> Self {
        Self {
            profile_id: Some(profile.profile_id),
            username: Some(profile.username),
            skin_url: profile.skin_url,
            last_used: profile.last_used,
        }
    }

    fn into_profile(self) -> Option<AccountProfile> {
        Some(AccountProfile {
            profile_id: self.profile_id?,
            username: self.username?,
            skin_url: self.skin_url,
            last_used: self.last_used,
        })
    }
}

/// A profile entry is independently optional so one scalar or non-object
/// element cannot reject the rest of the array.
#[derive(Debug, Clone)]
struct LenientAccountProfile(Option<AccountProfileDocument>);

impl<'de> Deserialize<'de> for LenientAccountProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self(AccountProfileDocument::deserialize(deserializer).ok()))
    }
}

fn deserialize_optional_uuid<'de, D>(deserializer: D) -> Result<Option<Uuid>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(deserialize_optional_string(deserializer)?.and_then(|raw| Uuid::parse_str(&raw).ok()))
}

fn serialize_optional_uuid<S>(value: &Option<Uuid>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    value.map(|id| id.to_string()).serialize(serializer)
}

fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(LenientStringVisitor)
}

fn deserialize_last_used<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(LenientU64Visitor)
}

struct LenientStringVisitor;

impl<'de> Visitor<'de> for LenientStringVisitor {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an optional string")
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(None)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(None)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Some(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Some(value))
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(None)
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(None)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(None)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(None)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}

struct LenientU64Visitor;

impl<'de> Visitor<'de> for LenientU64Visitor {
    type Value = u64;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an unsigned integer")
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(value)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(0)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(0)
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(0)
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(0)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(0)
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(0)
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(0)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(0)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(0)
    }
}

fn deserialize_profiles<'de, D>(deserializer: D) -> Result<Vec<AccountProfileDocument>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(ProfilesVisitor)
}

struct ProfilesVisitor;

impl<'de> Visitor<'de> for ProfilesVisitor {
    type Value = Vec<AccountProfileDocument>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an array of account profiles")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut profiles = Vec::new();
        while let Some(entry) = sequence.next_element::<LenientAccountProfile>()? {
            if let Some(profile) = entry.0 {
                profiles.push(profile);
            }
        }
        Ok(profiles)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Vec::new())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_bytes<E>(self, _value: &[u8]) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_byte_buf<E>(self, _value: Vec<u8>) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Vec::new())
    }
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
        let text = serde_json::to_string_pretty(&sample().to_document()).unwrap();
        assert_eq!(text, EXPECTED_JSON);
    }

    #[test]
    fn loading_the_hand_written_json_produces_the_expected_value() {
        assert_eq!(AccountsMetadata::from_json(EXPECTED_JSON), sample());
    }

    #[test]
    fn the_legacy_profiles_document_migrates_into_typed_metadata() {
        // This is the on-disk shape written before the typed document existed;
        // parsing it is the read-side migration boundary.
        let legacy = r#"{
          "selected":"069a79f4-44e9-4726-a5be-fca90e38aaf5",
          "profiles":[
            {"profile_id":"069a79f4-44e9-4726-a5be-fca90e38aaf5","username":"Notch",
             "skin_url":"https://textures.minecraft.net/texture/abc123","last_used":1700000000}
          ]
        }"#;
        assert_eq!(AccountsMetadata::from_json(legacy), sample());
    }

    #[test]
    fn serialization_keeps_optional_metadata_fields_in_the_legacy_shape() {
        let id = Uuid::parse_str("069a79f4-44e9-4726-a5be-fca90e38aaf5").unwrap();
        let metadata = AccountsMetadata {
            selected: None,
            profiles: vec![AccountProfile {
                profile_id: id,
                username: "NoSkin".to_owned(),
                skin_url: None,
                last_used: 0,
            }],
        };
        let text = serde_json::to_string_pretty(&metadata.to_document()).unwrap();
        assert!(text.contains("\"skin_url\": null"));
        assert!(text.contains("\"last_used\": 0"));
        assert_eq!(AccountsMetadata::from_json(&text), metadata);
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
        let corrupted = format!(
            r#"{{"selected":"not-a-uuid","profiles":[{{"profile_id":"{}","username":"Notch","skin_url":"https://textures.minecraft.net/texture/abc123","last_used":1700000000}}]}}"#,
            good.profiles[0].profile_id
        );

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

    #[test]
    fn wrong_json_types_for_each_profile_field_are_tolerated_independently() {
        let id = Uuid::new_v4();
        let text = format!(
            r#"{{"profiles":[
                {{"profile_id":{{}},"username":"bad id"}},
                {{"profile_id":"{id}","username":{{}}}},
                {{"profile_id":"{id}","username":"kept","skin_url":{{}},"last_used":[]}}
            ]}}"#
        );
        let metadata = AccountsMetadata::from_json(&text);
        assert_eq!(metadata.profiles.len(), 1);
        assert_eq!(metadata.profiles[0].profile_id, id);
        assert_eq!(metadata.profiles[0].username, "kept");
        assert_eq!(metadata.profiles[0].skin_url, None);
        assert_eq!(metadata.profiles[0].last_used, 0);
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
