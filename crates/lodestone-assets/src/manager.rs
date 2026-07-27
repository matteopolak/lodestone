//! The [`ResourceManager`] pack stack.

use crate::error::AssetError;
use crate::location::ResourceLocation;
use crate::meta::PackMeta;
use crate::source::ResourceSource;
use std::collections::HashSet;

/// The in-pack path of a pack's metadata file.
pub const PACK_META_PATH: &str = "pack.mcmeta";

/// The in-pack path of vanilla's version metadata file.
pub const VERSION_META_PATH: &str = "version.json";

/// An ordered stack of resource packs implementing vanilla override semantics.
///
/// Sources are stored **lowest priority first** (vanilla at the bottom, user
/// packs above). A lookup is served by the highest-priority pack that contains
/// the resource.
#[derive(Debug)]
pub struct ResourceManager {
    /// Lowest priority first; the last element has the highest priority.
    sources: Vec<Box<dyn ResourceSource>>,
}

impl ResourceManager {
    /// Builds a manager from sources ordered lowest priority first.
    pub fn new(sources: Vec<Box<dyn ResourceSource>>) -> Self {
        Self { sources }
    }

    /// Adds a source at the highest priority (top of the stack).
    pub fn push(&mut self, source: Box<dyn ResourceSource>) {
        self.sources.push(source);
    }

    /// The number of packs in the stack.
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Whether the stack is empty.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Reads a full in-pack path, letting the highest-priority pack win.
    pub fn read(&self, path: &str) -> Option<Vec<u8>> {
        for source in self.sources.iter().rev() {
            if let Some(bytes) = source.read(path) {
                return Some(bytes);
            }
        }
        None
    }

    /// Builds the in-pack path for a namespaced asset:
    /// `assets/<namespace>/<kind>/<path>.<ext>`.
    pub fn asset_path(location: &ResourceLocation, kind: &str, ext: &str) -> String {
        format!(
            "assets/{}/{}/{}.{}",
            location.namespace(),
            kind,
            location.path(),
            ext
        )
    }

    /// Reads a namespaced asset, resolving it via [`Self::asset_path`].
    pub fn read_asset(
        &self,
        location: &ResourceLocation,
        kind: &str,
        ext: &str,
    ) -> Option<Vec<u8>> {
        self.read(&Self::asset_path(location, kind, ext))
    }

    /// Lists full in-pack paths under `prefix` across the whole stack,
    /// deduplicated so a path present in several packs appears once.
    pub fn list(&self, prefix: &str) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        // Highest priority first so the surviving entry reflects the winning pack.
        for source in self.sources.iter().rev() {
            for path in source.list(prefix) {
                if seen.insert(path.clone()) {
                    out.push(path);
                }
            }
        }
        out
    }

    /// Reads and parses the pack metadata served by the winning pack.
    ///
    /// Prefers a `pack.mcmeta` (user packs always ship one). Vanilla's
    /// `client.jar` has **no** root `pack.mcmeta`, so this falls back to
    /// deriving the metadata from `version.json`. Returns
    /// [`AssetError::MetaMissing`] only when neither is present anywhere in the
    /// stack.
    pub fn read_pack_meta(&self) -> Result<PackMeta, AssetError> {
        if let Some(bytes) = self.read(PACK_META_PATH) {
            return PackMeta::parse(&bytes);
        }
        if let Some(bytes) = self.read(VERSION_META_PATH) {
            return PackMeta::from_version_json(&bytes);
        }
        Err(AssetError::MetaMissing)
    }

    /// The sources in the stack, lowest priority first.
    pub fn sources(&self) -> &[Box<dyn ResourceSource>] {
        &self.sources
    }
}
