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

    /// Builds a manager from sources ordered **highest priority first** — the
    /// order a resource-pack *UI* lists them in, top row winning.
    ///
    /// This exists because the two orders are opposites and getting it wrong
    /// fails silently: every pack still loads, nothing warns, and the pack the
    /// user put on top simply overrides nothing. Both directions are attested,
    /// from the record definitions rather than a summary of a call site:
    ///
    /// - Vanilla's own fallback resource-manager "get resource" accessor walks `for (int i =
    ///   fallbacks.size() - 1; i >= 0; i--)`,
    ///   and its own "push internal" step appends — so the **last** pack handed to
    ///   its own multi-pack resource manager wins. That is [`Self::new`]'s order, and
    ///   [`Self::read`]'s own `.iter().rev()` matches it exactly.
    /// - Vanilla's own pack-selection-model constructor does
    ///   `this.selected = Lists.newArrayList(repository.getSelectedPacks());
    ///   Collections.reverse(this.selected);`,
    ///   and its own commit step (via its own "update repo selected list" step)
    ///   reverses back — so the
    ///   **first** row of the Selected column is the last pack in the stack,
    ///   i.e. the winner.
    ///
    /// Vanilla's built-in pack is at a fixed bottom position,
    /// which its own position-insert step places at index 0 of the repository order —
    /// the bottom row of the UI column and the bottom of
    /// the stack, consistently.
    ///
    /// So: hand this the UI's own top-to-bottom order and the reversal is done
    /// for you, once, here.
    pub fn from_priority_order(mut sources: Vec<Box<dyn ResourceSource>>) -> Self {
        sources.reverse();
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

    /// Reads **every** source's own copy of `path`, lowest priority first —
    /// the shape a *merge* needs, as opposed to [`Self::read`]'s single
    /// winner-takes-all.
    ///
    /// Vanilla's own resource-manager "get resource stack" accessor returns exactly this
    /// (every pack layer that carries the path, not just the top one), and
    /// its own client-language "load from" step is why the distinction matters: a language
    /// file is not one pack's file replacing another's, it is every pack's
    /// file layered key-by-key, later (higher-priority) packs overriding
    /// individual keys. A pack that ships a `lang/en_us.json` defining only
    /// its own handful of custom keys must not blank out the ~7,000 vanilla
    /// keys underneath it — which is exactly what `read`'s single-winner
    /// return does, and is the shape of the bug that put the raw key
    /// `container.crafting` on screen instead of "Crafting" for a player
    /// running nothing but a small icon-font pack. See
    /// `Language::merged_from_stack`, the one caller.
    pub fn read_stack(&self, path: &str) -> Vec<Vec<u8>> {
        self.sources
            .iter()
            .filter_map(|source| source.read(path))
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;

    fn pack(name: &str, body: &str) -> Box<dyn ResourceSource> {
        let mut source = MemorySource::new(name);
        source.insert("marker.txt", body.as_bytes().to_vec());
        Box::new(source)
    }

    /// The two constructors disagree by design, and this is the assertion that
    /// says which is which: the **last** source `new` is given wins, and the
    /// **first** source `from_priority_order` is given wins.
    #[test]
    fn priority_order_is_the_reverse_of_stack_order() {
        let stack = ResourceManager::new(vec![pack("low", "low"), pack("high", "high")]);
        assert_eq!(stack.read("marker.txt").as_deref(), Some(&b"high"[..]));

        let ui = ResourceManager::from_priority_order(vec![pack("high", "high"), pack("low", "low")]);
        assert_eq!(
            ui.read("marker.txt").as_deref(),
            Some(&b"high"[..]),
            "the UI's top row must win; if this reads \"low\" the reversal is missing"
        );
    }
}
