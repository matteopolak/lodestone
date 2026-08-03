//! The launcher's **asset-object store** — the half of the vanilla assets that
//! is not in `client.jar`.
//!
//! ## What it is
//!
//! A vanilla install has two asset sources, and `client.jar` is only one of them.
//! The other is a content-addressed object store: an `asset-index-<id>.json`
//! mapping a logical name (`minecraft/textures/…`, with **no** `assets/` prefix)
//! to `{hash, size}`, and a flat `objects/<hash[0..2]>/<hash>` tree holding the
//! bytes. This module resolves the first into the second.
//!
//! ## Why it exists (the bug that produced it)
//!
//! **`client.jar` ships deliberate stubs for files the object store overrides,
//! and reading the jar copy silently gives you the stub.** Measured on 26.2, with
//! `zipfile` against the jar rather than an extracted tree:
//!
//! | name | jar | object store |
//! |---|---|---|
//! | `textures/gui/title/background/panorama_0.png` | 69 B, **1×1 grey** | 547,239 B, 1024×1024 |
//! | `panorama_1.png` | 69 B | 294,940 B |
//! | `panorama_2.png` | 69 B | 425,769 B |
//! | `panorama_3.png` | 69 B | 461,522 B |
//! | `panorama_4.png` | 69 B | 738,917 B |
//! | `panorama_5.png` | 69 B | 118,484 B |
//! | `font/include/unifont.json` | 29 B | 3,993 B |
//! | `panorama_overlay.png` | 68 B | 86 B (both 1×1 transparent) |
//!
//! The panorama was ported against those stubs and concluded the title screen's
//! sky was "a flat grey placeholder Mojang shipped" — a confident, evidenced,
//! entirely wrong reading of the game. The jar was not stale and the extraction
//! was not stale; the jar genuinely ships stubs.
//!
//! **The scope is narrow, which is the useful part.** Of 5057 index objects only
//! **8** share a name with a jar entry, and all 8 are the table above — so the
//! panorama and `unifont.json` are the *only* jar entries in the game that a
//! store object overrides. There is no general asset-pipeline problem. Every
//! other index object is index-only: 4871 `.ogg`, 146 `.json`, 32 `.png` that
//! shadow nothing, 5 `.zip`, 2 `.icns`, 1 `.mcmeta`.
//!
//! The rule that falls out, and the one this module exists to make cheap: **for
//! any name present in both, prefer the object store.** Never the other way
//! round, and never the jar alone.
//!
//! ## How it works
//!
//! [`AssetObjectStore::open`] finds the single `asset-index-*.json` in a root
//! (refusing to guess between several — deterministic, never directory order),
//! parses it, and keeps `name -> {hash, size}`. [`AssetObjectStore::read`]
//! resolves a name to `objects/<hash[0..2]>/<hash>` and reads it, **checking the
//! length against the index** and treating a mismatch as absent rather than
//! returning half a PNG.
//!
//! Length, not SHA-1, on the read path: hashing every object at startup would be
//! 25 MB of SHA-1 for the panorama alone, and the download side already verifies
//! the full digest (`xtask fetch-assets`, `download_verified_file`). Length
//! catches the failure that actually happens here — an interrupted or truncated
//! fetch — for free. A silently *corrupt* object of the right length is caught by
//! the PNG decoder downstream.
//!
//! ## How to change it
//!
//! Adding a consumer is [`AssetObjectStore::read`] with the right index key; note
//! the key has no `assets/` prefix, which is the mistake that resolves nothing.
//! Populating the store is `cargo run -p xtask -- fetch-assets --version 26.2`,
//! which downloads exactly the shadowed set (see `fetch_shadowed_objects`) — not
//! all 5057 objects.
//!
//! **`crate::audio` has an older private copy of this logic** (`find_asset_index`
//! / `parse_asset_index` / `AssetObjectSource`), written for `sounds.json` and the
//! 4871 `.ogg` objects before this module existed. It is left alone deliberately:
//! `audio.rs` is `#![cfg(not(target_arch = "wasm32"))]` and depending on it from
//! `resources.rs` would make a cfg-gated module load-bearing for texture loading.
//! Collapsing the two is a named follow-up, not an accident. If you are here to
//! do that, note the two also disagree on which env var names the root:
//! `audio.rs` takes `LODESTONE_ASSET_ROOT`, while this module is handed whatever
//! [`crate::resources`] resolved (`LODESTONE_ASSETS`, or a `.cache/mc/*` scan).
//!
//! ## Dependencies
//!
//! `serde_json` for the index, nothing else. No network: this module is
//! read-only over a populated store, and a missing object is `None`, never a
//! blocking fetch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lodestone_assets::ResourceSource;

/// What the index says about one object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    /// Lowercase hex SHA-1, which is also the object's path.
    pub hash: String,
    /// Length in bytes, used as the read-path integrity check.
    pub size: u64,
}

/// A resolved asset-object store: an index plus the root it addresses.
#[derive(Clone)]
pub struct AssetObjectStore {
    root: PathBuf,
    index: HashMap<String, ObjectMeta>,
}

impl std::fmt::Debug for AssetObjectStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The index is 5057 entries; printing it is never what anyone wants.
        f.debug_struct("AssetObjectStore")
            .field("root", &self.root)
            .field("objects", &self.index.len())
            .finish()
    }
}

impl AssetObjectStore {
    /// Open the store rooted at `root` (the directory holding
    /// `asset-index-*.json` and `objects/`).
    ///
    /// # Errors
    ///
    /// Returns a message when the root has no `asset-index-*.json`, has several,
    /// or holds one that is not a parseable non-empty index.
    pub fn open(root: &Path) -> Result<Self, String> {
        let index_path = find_asset_index(root)?;
        let bytes = std::fs::read(&index_path)
            .map_err(|e| format!("reading {}: {e}", index_path.display()))?;
        let index = parse_asset_index(&bytes)?;
        Ok(Self {
            root: root.to_path_buf(),
            index,
        })
    }

    /// Number of objects the index declares (not how many are on disk).
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the index declares nothing. Always false for a store from
    /// [`Self::open`], which rejects an empty index.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// The index entry for `key`, if the index has one.
    #[must_use]
    pub fn meta(&self, key: &str) -> Option<&ObjectMeta> {
        self.index.get(key)
    }

    /// Where `key`'s bytes would live, whether or not they are there.
    #[must_use]
    pub fn object_path(&self, key: &str) -> Option<PathBuf> {
        self.meta(key).map(|m| object_relpath(&self.root, &m.hash))
    }

    /// Read `key`'s bytes, or `None` when the index does not list it, the object
    /// is not on disk, or its length disagrees with the index.
    ///
    /// `key` is a **logical asset name with no `assets/` prefix**
    /// (`minecraft/sounds.json`, `minecraft/textures/gui/…`). The
    /// [`ResourceSource`] impl below is the one that strips a pack-absolute path
    /// down to this; call that if you have `assets/…` in hand.
    ///
    /// A length mismatch logs a warning and reports absence: a truncated object
    /// must not reach a decoder as if it were the asset.
    #[must_use]
    pub fn object_bytes(&self, key: &str) -> Option<Vec<u8>> {
        let meta = self.meta(key)?;
        let path = object_relpath(&self.root, &meta.hash);
        let bytes = std::fs::read(&path).ok()?;
        if bytes.len() as u64 != meta.size {
            tracing::warn!(
                target: "assets",
                object = %path.display(),
                have = bytes.len(),
                want = meta.size,
                "asset object length disagrees with the index; treating as absent \
                 (re-run: cargo run -p xtask -- fetch-assets)"
            );
            return None;
        }
        Some(bytes)
    }

    /// Whether `key` is present on disk at its declared length.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.meta(key).is_some_and(|meta| {
            let path = object_relpath(&self.root, &meta.hash);
            std::fs::metadata(&path).is_ok_and(|m| m.len() == meta.size)
        })
    }
}

/// Lets the store stand in as a pack source for anything built on
/// [`lodestone_assets::ResourceManager`] — which is how `crate::audio` reads
/// `sounds.json` and every `.ogg`.
///
/// The **only** thing this adds over [`AssetObjectStore::object_bytes`] is
/// dropping the leading `assets/`: a `ResourceManager` consumer asks for
/// `assets/minecraft/sounds/…`, and an asset-index name has no such prefix. Get
/// that wrong and every lookup misses, silently, which for audio is the
/// "connected but plays nothing" failure and for the panorama was a flat grey sky.
impl ResourceSource for AssetObjectStore {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.object_bytes(path.strip_prefix("assets/").unwrap_or(path))
    }

    fn list(&self, _prefix: &str) -> Vec<String> {
        // A content-addressed store has no directory structure to walk, and every
        // consumer here resolves by explicit name. Listing would have to iterate
        // the whole 5057-entry index and is never what anyone wants.
        Vec::new()
    }
}

/// Find the single `asset-index-*.json` under `root`.
///
/// Refuses to pick between several rather than taking directory order — the
/// same discipline `crate::audio` applies, and for the same reason: several
/// client versions coexist under `.cache/mc` and "first match wins over a shared
/// directory" is a known cross-agent landmine.
///
/// # Errors
///
/// Returns a message when zero or more than one candidate exists.
pub fn find_asset_index(root: &Path) -> Result<PathBuf, String> {
    let mut matches: Vec<PathBuf> = Vec::new();
    let entries =
        std::fs::read_dir(root).map_err(|e| format!("reading dir {}: {e}", root.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("asset-index-") && name.ends_with(".json") {
            matches.push(entry.path());
        }
    }
    matches.sort();
    match matches.len() {
        0 => Err(format!(
            "no asset-index-*.json in {} — run: cargo run -p xtask -- fetch-assets",
            root.display()
        )),
        1 => Ok(matches.pop().expect("len checked")),
        n => Err(format!(
            "{n} asset-index-*.json files in {}; refusing to guess (found e.g. {})",
            root.display(),
            matches[0].display()
        )),
    }
}

/// Parse an `asset-index-*.json` into `name -> {hash, size}`.
///
/// Shape: `{"objects": {"<name>": {"hash": "<sha1>", "size": <n>}, …}}`.
///
/// # Errors
///
/// Returns a message when the bytes are not JSON, carry no `objects` map, or
/// yield an empty one — an index that parses to nothing would make every
/// consumer come up "working" and resolve nothing, which is the silent
/// half-state this must fail on instead.
pub fn parse_asset_index(bytes: &[u8]) -> Result<HashMap<String, ObjectMeta>, String> {
    let json: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("asset index is not valid JSON: {e}"))?;
    let objects = json
        .get("objects")
        .and_then(|o| o.as_object())
        .ok_or("asset index has no \"objects\" map")?;
    let mut index = HashMap::with_capacity(objects.len());
    for (name, meta) in objects {
        let Some(hash) = meta.get("hash").and_then(|h| h.as_str()) else {
            continue;
        };
        // A hash that is not a plausible SHA-1 would build a nonsense path; skip
        // it rather than manufacturing `objects/xx/garbage`.
        if hash.len() < 2 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let size = meta.get("size").and_then(serde_json::Value::as_u64);
        index.insert(
            name.clone(),
            ObjectMeta {
                hash: hash.to_string(),
                size: size.unwrap_or(0),
            },
        );
    }
    if index.is_empty() {
        return Err("asset index \"objects\" map is empty".to_string());
    }
    Ok(index)
}

/// `<root>/objects/<hash[0..2]>/<hash>`.
///
/// # Panics
///
/// Panics if `hash` is shorter than two characters; [`parse_asset_index`] rejects
/// those, so every hash reaching here has been screened.
#[must_use]
pub fn object_relpath(root: &Path, hash: &str) -> PathBuf {
    root.join("objects").join(&hash[0..2]).join(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &[u8] = br#"{
        "objects": {
            "minecraft/textures/gui/title/background/panorama_0.png": {
                "hash": "f5e0a5cfbc2e7b1e89094d7882dde106b030ec26", "size": 547239 },
            "minecraft/sounds.json": {
                "hash": "abcdef0123456789abcdef0123456789abcdef01", "size": 10 },
            "minecraft/broken": { "hash": "nothex!!", "size": 1 },
            "minecraft/hashless": { "size": 1 }
        }
    }"#;

    #[test]
    fn the_index_parses_name_to_hash_and_size() {
        let index = parse_asset_index(INDEX).expect("parses");
        // The two well-formed entries only: the non-hex hash and the entry with
        // no hash at all are both dropped.
        assert_eq!(index.len(), 2);
        let pano = index
            .get("minecraft/textures/gui/title/background/panorama_0.png")
            .expect("panorama_0 is in the index");
        assert_eq!(pano.hash, "f5e0a5cfbc2e7b1e89094d7882dde106b030ec26");
        // The real 26.2 size, which is also the evidence the jar's 69-byte entry
        // is a stub rather than the asset.
        assert_eq!(pano.size, 547_239);
        assert!(!index.contains_key("minecraft/broken"));
        assert!(!index.contains_key("minecraft/hashless"));
    }

    #[test]
    fn an_empty_or_malformed_index_is_an_error_not_a_silent_empty() {
        // An index that parses to zero objects would make every consumer come up
        // "working" and resolve nothing.
        assert!(parse_asset_index(b"{\"objects\":{}}").is_err());
        assert!(parse_asset_index(b"not json").is_err());
        assert!(parse_asset_index(b"{\"nope\":1}").is_err());
        // And one whose every entry is unusable is empty too.
        assert!(parse_asset_index(br#"{"objects":{"a":{"hash":"zz"}}}"#).is_err());
    }

    #[test]
    fn the_object_path_is_the_two_character_fanout() {
        let p = object_relpath(
            Path::new("/root"),
            "f5e0a5cfbc2e7b1e89094d7882dde106b030ec26",
        );
        assert_eq!(
            p,
            Path::new("/root/objects/f5/f5e0a5cfbc2e7b1e89094d7882dde106b030ec26")
        );
    }

    #[test]
    fn a_short_read_is_absence_not_a_truncated_asset() {
        // The store points at a temp root holding an object of the wrong length.
        let root = std::env::temp_dir().join(format!(
            "lodestone-asset-objects-{}-{}",
            std::process::id(),
            "shortread"
        ));
        let hash = "abcdef0123456789abcdef0123456789abcdef01";
        let dir = root.join("objects").join("ab");
        std::fs::create_dir_all(&dir).expect("temp dirs");
        std::fs::write(dir.join(hash), b"short").expect("write object");

        let mut index = HashMap::new();
        index.insert(
            "minecraft/thing".to_string(),
            ObjectMeta {
                hash: hash.to_string(),
                size: 999,
            },
        );
        let store = AssetObjectStore {
            root: root.clone(),
            index,
        };
        assert_eq!(
            store.object_bytes("minecraft/thing"),
            None,
            "a 5-byte object for a 999-byte index entry must read as absent"
        );
        assert!(!store.has("minecraft/thing"));

        // Control: the same store with the index agreeing on the length reads it,
        // which proves the rejection above was the length check and not a broken
        // path or a missing file.
        let mut ok_index = HashMap::new();
        ok_index.insert(
            "minecraft/thing".to_string(),
            ObjectMeta {
                hash: hash.to_string(),
                size: 5,
            },
        );
        let ok = AssetObjectStore {
            root: root.clone(),
            index: ok_index,
        };
        assert_eq!(
            ok.object_bytes("minecraft/thing").as_deref(),
            Some(&b"short"[..])
        );
        assert!(ok.has("minecraft/thing"));

        // And the `ResourceSource` path reaches the same object *through* the
        // prefix strip, which is the whole reason that impl exists.
        assert_eq!(
            ResourceSource::read(&ok, "assets/minecraft/thing").as_deref(),
            Some(&b"short"[..]),
            "the ResourceSource impl must drop `assets/` to form the index name"
        );
        // Control: without stripping, the key would not be in the index at all —
        // so this must miss, which is what proves the line above did the strip
        // rather than matching by luck.
        assert_eq!(
            ok.object_bytes("assets/minecraft/thing"),
            None,
            "an index name never carries the assets/ prefix"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_key_the_index_does_not_list_reads_as_none_without_touching_disk() {
        let index = parse_asset_index(INDEX).expect("parses");
        let store = AssetObjectStore {
            root: PathBuf::from("/nonexistent-root"),
            index,
        };
        assert_eq!(store.object_bytes("minecraft/not/in/the/index"), None);
        assert!(store.object_path("minecraft/not/in/the/index").is_none());
        // A listed key yields a path even with nothing on disk, which is what
        // lets a caller report *where* it looked.
        assert!(
            store
                .object_path("minecraft/textures/gui/title/background/panorama_0.png")
                .is_some()
        );
    }

    #[test]
    fn several_indexes_in_one_root_is_an_error_rather_than_directory_order() {
        let root = std::env::temp_dir().join(format!(
            "lodestone-asset-objects-{}-{}",
            std::process::id(),
            "twoindexes"
        ));
        std::fs::create_dir_all(&root).expect("temp dir");
        std::fs::write(root.join("asset-index-32.json"), b"{}").expect("write");
        std::fs::write(root.join("asset-index-17.json"), b"{}").expect("write");
        let err = find_asset_index(&root).expect_err("two indexes must not be guessed between");
        assert!(err.contains("refusing to guess"), "unexpected: {err}");

        // Control: with one removed it resolves, so the error above is the count
        // and not something else about the directory.
        std::fs::remove_file(root.join("asset-index-17.json")).expect("rm");
        assert_eq!(
            find_asset_index(&root).expect("one index resolves"),
            root.join("asset-index-32.json")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
