//! Language tables: the client-side translation strings that resolve a chat
//! component's `translate` keys into real words.
//!
//! A vanilla language file (`assets/<namespace>/lang/<code>.json`, e.g.
//! `assets/minecraft/lang/en_us.json`) is a flat JSON object mapping a
//! translation key to its format string:
//!
//! ```json
//! {
//!   "death.attack.mob": "%1$s was slain by %2$s",
//!   "entity.minecraft.spider": "Spider"
//! }
//! ```
//!
//! This module loads that file into a [`Language`] and exposes a single lookup,
//! [`Language::get`]. It intentionally holds no resolution logic of its own: a
//! [`lodestone_model::Text`] resolves itself against a `Fn(&str) -> Option<String>`
//! closure (see `Language::translator`), so the *table* lives here in the asset
//! layer while the *component walk* stays version-free above it. That split is
//! why a missing key is the loader's silence (`None`), never a substituted guess:
//! the caller decides the fallback, and vanilla's fallback is the key itself.
//!
//! The real vanilla table is ~500 KiB and ~7,000 keys, so it is never hand-typed
//! or embedded; it is read from the same `client.jar` the renderer already loads.

use std::collections::HashMap;

use crate::error::AssetError;
use crate::manager::ResourceManager;
use crate::source::ResourceSource;

/// A loaded translation table: translation key -> format string.
///
/// Construct one from raw JSON bytes with [`Language::from_json_bytes`] or read
/// it straight out of a resource pack with [`Language::from_source`]. Look keys
/// up with [`Language::get`], or hand the whole table to a component resolver as
/// a closure via [`Language::translator`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Language {
    entries: HashMap<String, String>,
}

impl Language {
    /// The in-jar path of a language file for `namespace`/`code`, e.g.
    /// `assets/minecraft/lang/en_us.json`.
    #[must_use]
    pub fn resource_path(namespace: &str, code: &str) -> String {
        format!("assets/{namespace}/lang/{code}.json")
    }

    /// Parses a vanilla language file (a flat JSON object of string values).
    ///
    /// Non-string values (a language file should have none) are skipped rather
    /// than rejected, so a slightly non-conforming pack still loads every usable
    /// key instead of failing wholesale.
    ///
    /// # Errors
    /// Returns [`AssetError::LangMalformed`] if the bytes are not a JSON object.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, AssetError> {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|err| AssetError::LangMalformed(err.to_string()))?;
        let serde_json::Value::Object(map) = value else {
            return Err(AssetError::LangMalformed(
                "language file is not a JSON object".to_string(),
            ));
        };
        let entries = map
            .into_iter()
            .filter_map(|(key, value)| match value {
                serde_json::Value::String(text) => Some((key, text)),
                _ => None,
            })
            .collect();
        Ok(Self { entries })
    }

    /// Reads and parses `assets/<namespace>/lang/<code>.json` from a resource
    /// pack. Returns `None` when the pack has no such file; propagates a parse
    /// error when the file is present but malformed.
    ///
    /// # Errors
    /// Returns [`AssetError::LangMalformed`] if the file exists but is not a
    /// valid JSON object.
    pub fn from_source(
        source: &dyn ResourceSource,
        namespace: &str,
        code: &str,
    ) -> Result<Option<Self>, AssetError> {
        match source.read(&Self::resource_path(namespace, code)) {
            Some(bytes) => Self::from_json_bytes(&bytes).map(Some),
            None => Ok(None),
        }
    }

    /// Reads `assets/minecraft/lang/en_us.json` — the default client language —
    /// from a resource pack.
    ///
    /// # Errors
    /// Returns [`AssetError::LangMalformed`] if the file exists but is malformed.
    pub fn en_us_from_source(source: &dyn ResourceSource) -> Result<Option<Self>, AssetError> {
        Self::from_source(source, "minecraft", "en_us")
    }

    /// Reads and **merges** every pack's own copy of
    /// `assets/<namespace>/lang/<code>.json` across the whole `manager`
    /// stack, lowest priority first, so a higher-priority pack's key
    /// overrides individually rather than its file replacing the base one
    /// wholesale.
    ///
    /// This is [`Self::from_source`]'s intended replacement wherever the
    /// caller wants vanilla's own language-loading behaviour
    /// (its own client-language "load from" step, which walks its own
    /// resource-manager "get resource stack" accessor
    /// and folds every layer's entries into one map): `from_source`/`read`
    /// answer "what does the winning pack's file say", which is correct for a
    /// texture or a model but wrong for a language file, where vanilla treats
    /// every active pack's file as a partial patch rather than a full
    /// replacement. A pack that ships only its own handful of custom keys
    /// must still leave the ~7,000 vanilla keys underneath it resolvable.
    ///
    /// A layer that fails to parse is skipped (with the table still built
    /// from whatever else is on the stack) rather than failing the whole
    /// merge — one malformed pack must not blank every key from every other
    /// layer. Returns `None` only when **no** layer has the file at all,
    /// matching [`Self::from_source`]'s `Ok(None)` for "not shipped here".
    #[must_use]
    pub fn merged_from_stack(manager: &ResourceManager, namespace: &str, code: &str) -> Option<Self> {
        let path = Self::resource_path(namespace, code);
        let mut entries = HashMap::new();
        let mut any = false;
        for bytes in manager.read_stack(&path) {
            match Self::from_json_bytes(&bytes) {
                Ok(layer) => {
                    any = true;
                    entries.extend(layer.entries);
                }
                Err(_) => continue,
            }
        }
        any.then_some(Self { entries })
    }

    /// Looks up the format string for a translation `key`, or `None` if the
    /// table has no such key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    /// The number of translation keys in the table.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Borrows the table as a `Fn(&str) -> Option<String>` closure, the shape a
    /// [`lodestone_model::Text`] resolver consumes. The returned closure borrows
    /// `self`, so it costs one `String` clone per resolved key and no table copy.
    pub fn translator(&self) -> impl Fn(&str) -> Option<String> + '_ {
        move |key: &str| self.get(key).map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;

    #[test]
    fn parses_flat_object_and_looks_up() {
        let lang = Language::from_json_bytes(
            br#"{ "death.attack.mob": "%1$s was slain by %2$s", "entity.minecraft.spider": "Spider" }"#,
        )
        .expect("valid language json");
        assert_eq!(lang.len(), 2);
        assert_eq!(lang.get("death.attack.mob"), Some("%1$s was slain by %2$s"));
        assert_eq!(lang.get("entity.minecraft.spider"), Some("Spider"));
        assert_eq!(lang.get("no.such.key"), None);
    }

    #[test]
    fn non_string_values_are_skipped_not_fatal() {
        let lang = Language::from_json_bytes(br#"{ "a": "x", "b": 5, "c": true, "d": "y" }"#)
            .expect("object still parses");
        assert_eq!(lang.get("a"), Some("x"));
        assert_eq!(lang.get("d"), Some("y"));
        assert_eq!(lang.get("b"), None);
    }

    #[test]
    fn non_object_json_is_an_error() {
        assert!(Language::from_json_bytes(br#"["not", "an", "object"]"#).is_err());
        assert!(Language::from_json_bytes(b"not json at all").is_err());
    }

    #[test]
    fn reads_from_a_resource_pack_at_the_vanilla_path() {
        let mut pack = MemorySource::new("test-pack");
        pack.insert(
            "assets/minecraft/lang/en_us.json",
            br#"{ "menu.singleplayer": "Singleplayer" }"#.to_vec(),
        );
        let lang = Language::en_us_from_source(&pack)
            .expect("no parse error")
            .expect("file present");
        assert_eq!(lang.get("menu.singleplayer"), Some("Singleplayer"));

        // A pack without the file yields Ok(None), not an error.
        let empty = MemorySource::new("empty");
        assert!(
            Language::en_us_from_source(&empty)
                .expect("no parse error")
                .is_none()
        );
    }

    #[test]
    fn translator_closure_resolves_and_reports_misses() {
        let lang =
            Language::from_json_bytes(br#"{ "k": "value" }"#).expect("valid language json");
        let tr = lang.translator();
        assert_eq!(tr("k"), Some("value".to_string()));
        assert_eq!(tr("missing"), None);
    }
}
