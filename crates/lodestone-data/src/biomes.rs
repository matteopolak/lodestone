//! The generated built-in `minecraft:worldgen/biome` domain and its extension
//! boundary.
//!
//! The enum is generated from the checked-in worldgen asset registry by
//! `tests/biome_enum.rs`; it is not a hand-maintained list. `BiomeRef` keeps
//! dynamically registered entries explicit and compact instead of silently
//! treating arbitrary text as a built-in.

use crate::generated_biome_enum as table;

pub use self::table::BuiltinBiome;

/// Every built-in biome path, sorted for the existing command/data census
/// callers. The generated table is the source of truth.
pub use self::table::BIOME_NAMES;

/// A host-assigned index for a biome supplied by a plugin or data pack.
///
/// The index deliberately carries no string storage. The component that owns
/// the extension registry owns the name-to-id mapping and is responsible for
/// providing the display/serialization name at its boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtensionId(u32);

impl ExtensionId {
    /// Wraps a host-assigned extension index.
    #[must_use]
    pub const fn from_index(index: u32) -> Self {
        Self(index)
    }

    /// Returns the host-assigned extension index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// A biome identity that is either one of this build's generated built-ins or
/// an explicitly registered extension, packed into one `u32`.
///
/// Built-in ids occupy `0..BuiltinBiome::COUNT`; extension ids follow them.
/// This keeps the resident worldgen representation compact while retaining an
/// explicit split at the boundary where an extension can actually appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct BiomeRef(u32);

/// The resolved form of a [`BiomeRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BiomeKind {
    /// A generated built-in biome.
    Builtin(BuiltinBiome),
    /// A plugin/data-pack biome resolved by an external registry.
    Extension(ExtensionId),
}

/// A strict parse failure at a resource-loading boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownBiome<'a> {
    /// The resource name that was rejected.
    pub name: &'a str,
}

impl std::fmt::Display for UnknownBiome<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "unknown built-in biome {:?}", self.name)
    }
}

impl std::error::Error for UnknownBiome<'_> {}

impl BuiltinBiome {
    /// Number of built-in entries in this generated table.
    pub const COUNT: u8 = table::BUILTINS.len() as u8;

    /// Resolves a generated registry id without allocating.
    #[must_use]
    pub fn from_registry_id(id: u32) -> Option<Self> {
        table::BUILTINS.get(id as usize).copied()
    }

    /// The path without the `minecraft:` namespace.
    #[must_use]
    pub fn path(self) -> &'static str {
        table::BIOME_NAMES[self as usize]
    }

    /// Resolves a namespaced built-in name without allocating.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let path = name.strip_prefix("minecraft:")?;
        let index = table::BIOME_NAMES.binary_search(&path).ok()?;
        table::BUILTINS.get(index).copied()
    }

    /// Resolves a namespaced name or returns a strict parse error.
    pub fn parse(name: &str) -> Result<Self, UnknownBiome<'_>> {
        Self::from_name(name).ok_or(UnknownBiome { name })
    }

    /// Every built-in biome in canonical generated order.
    pub fn all() -> impl ExactSizeIterator<Item = Self> + Clone {
        table::BUILTINS.iter().copied()
    }
}

impl BiomeRef {
    /// Wraps a generated built-in.
    #[must_use]
    pub const fn builtin(biome: BuiltinBiome) -> Self {
        Self(biome as u8 as u32)
    }

    /// Wraps an extension id supplied by an external registry.
    #[must_use]
    pub const fn extension(id: ExtensionId) -> Self {
        match id.0.checked_add(BuiltinBiome::COUNT as u32) {
            Some(raw) => Self(raw),
            None => panic!("extension biome id overflows the BiomeRef encoding"),
        }
    }

    /// Splits this packed identity into its generated or extension arm.
    #[must_use]
    pub fn kind(self) -> BiomeKind {
        match BuiltinBiome::from_registry_id(self.0) {
            Some(biome) => BiomeKind::Builtin(biome),
            None => BiomeKind::Extension(ExtensionId(self.0 - BuiltinBiome::COUNT as u32)),
        }
    }

    /// The built-in value, if this is not an extension.
    #[must_use]
    pub fn builtin_or_none(self) -> Option<BuiltinBiome> {
        match self.kind() {
            BiomeKind::Builtin(biome) => Some(biome),
            BiomeKind::Extension(_) => None,
        }
    }

    /// The extension handle, or `None` for a generated built-in.
    #[must_use]
    pub fn extension_or_none(self) -> Option<ExtensionId> {
        match self.kind() {
            BiomeKind::Builtin(_) => None,
            BiomeKind::Extension(id) => Some(id),
        }
    }
}

/// Whether `namespace:path` (or a bare `path`, defaulting to `minecraft`) names
/// a real 26.2 biome — `/execute if biome`'s parse-time validation, the same
/// posture [`crate::block::Block::from_name`]/[`crate::entity_types::entity_type_id`]
/// take for their own registries.
#[must_use]
pub fn is_biome(qualified: &str) -> bool {
    BuiltinBiome::from_name(qualified).is_some()
}

/// Every biome id, namespace-qualified — [`lodestone_command_mc::BiomeArg`]'s
/// own suggestion list reaches through this rather than re-deriving one.
pub fn all() -> impl Iterator<Item = String> {
    BIOME_NAMES.iter().map(|name| format!("minecraft:{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_array_is_sorted_for_binary_search() {
        let mut sorted = BIOME_NAMES;
        sorted.sort_unstable();
        assert_eq!(BIOME_NAMES, sorted, "BIOME_NAMES must stay sorted for is_biome's binary_search");
    }

    #[test]
    fn a_real_biome_validates_with_and_without_the_namespace() {
        assert!(is_biome("minecraft:plains"));
        assert!(is_biome("minecraft:the_end"));
        assert!(!is_biome("plains"), "is_biome takes a qualified id, unlike the McArg layer above it");
    }

    #[test]
    fn an_unknown_biome_and_a_foreign_namespace_both_fail() {
        assert!(!is_biome("minecraft:not_a_real_biome"));
        assert!(!is_biome("modded:custom_biome"));
    }

    #[test]
    fn all_yields_every_entry_namespace_qualified() {
        let all: Vec<String> = all().collect();
        assert_eq!(all.len(), BIOME_NAMES.len());
        assert!(all.contains(&"minecraft:plains".to_string()));
    }

    /// Regenerate-or-assert against the real 26.2 data directory, the same
    /// shape as this crate's other `LODESTONE_REGEN=1` gates — guarded on the
    /// cache being present, since it is not committed to the repo.
    #[test]
    fn the_census_matches_the_generated_directory() {
        let dir = std::path::Path::new(
            "../../.cache/mc/26.2/client-src/data/minecraft/worldgen/biome",
        );
        if !dir.exists() {
            eprintln!("skipping: {} not present in this checkout", dir.display());
            return;
        }
        let mut found: Vec<String> = std::fs::read_dir(dir)
            .expect("read the biome directory")
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    path.file_stem().and_then(|s| s.to_str()).map(str::to_string)
                } else {
                    None
                }
            })
            .collect();
        found.sort();
        let expected: Vec<String> = BIOME_NAMES.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(
            found, expected,
            "BIOME_NAMES has drifted from data/minecraft/worldgen/biome — regenerate this module's array"
        );
    }
}
