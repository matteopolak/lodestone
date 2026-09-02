//! A plugin-facing registry of custom dimensions, built on
//! [`lodestone_worldgen::generator::ChunkGenerator`]'s seam. See
//! `docs/plugin-worldgen-api.md` for the full design and, in particular, its
//! "What this does not do" section — the honest boundary of what registering
//! a dimension here actually gets a plugin.
//!
//! # Why this is not a new [`crate::dimension::Dimension`] variant
//!
//! [`crate::dimension::Dimension`] documents itself as **deliberately not an
//! open-ended registry**: every variant needs a generator, a chunk store, a
//! wire `dimension_type` holder id and a travel rule, and — critically — the
//! holder id is published from a **fixed, compile-time NBT table**
//! (`DIMENSION_TYPE_REGISTRY` in the v770 protocol family), not derived from
//! any server-side registry a plugin could append to. Making `Dimension`
//! open-ended would mean either wiring a new wire `dimension_type` entry
//! through the protocol family (a version-crate change, outside
//! `lodestone-worldgen`/`lodestone-server`'s own seam and outside this
//! issue's scope) or silently mis-describing a plugin dimension's real
//! properties to a joining client — worse than not offering it.
//!
//! So [`DimensionRegistry`] is deliberately a **separate, additive**
//! mechanism: a plugin registers a generator plus the properties a *server*
//! decides on its own (portal/respawn/bed rules, coordinate scale, vertical
//! bounds) under a key, and gets back a real [`ChunkSource`] it can open a
//! **primary** integrated-server world with —
//! `IntegratedServer::open_in_memory_with_entities`/
//! `open_persistent_with_mobs` are already generic over `S: ChunkSource`, so
//! this closes the "no per-world generator selection mechanism exists" gap
//! with zero changes to `crate::integrated`. What it does **not** yet do is make a
//! registered dimension reachable as a **second**, portal-travel dimension
//! alongside a running Overworld — that is the wire-registry work described
//! above, tracked as future scope rather than silently half-built here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use lodestone_worldgen::generator::ChunkGenerator;

use crate::chunk::ChunkSource;
use crate::plugin_worldgen::PluginChunkSource;

/// The subset of the real per-dimension-type record a plugin decides for its
/// own custom dimension — everything a **server** can honour on its own,
/// without a client-visible `dimension_type` registry entry (ambient light,
/// sky/ceiling rendering and the exact fog curve are wire concerns; see the
/// module doc's boundary note).
#[derive(Debug, Clone)]
pub struct DimensionProperties {
    /// The lowest world `y` a column in this dimension covers.
    pub min_y: i32,
    /// The number of `y` levels a chunk covers.
    pub height: i32,
    /// The highest `y` anything may be placed at.
    pub logical_height: i32,
    /// The real Nether/Overworld 8:1 coordinate-scale ratio,
    /// generalised. `1.0` for a dimension with no special scaling.
    pub coordinate_scale: f64,
    /// Whether a compass spins erratically and a bed/respawn-anchor
    /// explosion rule based on "is this the Overworld" applies the real way.
    pub natural: bool,
    /// Whether a bed can be used to sleep/set spawn here.
    pub bed_works: bool,
    /// Whether a respawn anchor can be used to set spawn here.
    pub respawn_anchor_works: bool,
    /// Whether piglins are immune to zombification here.
    pub piglin_safe: bool,
    /// Affects water evaporation and lava spread
    /// speed rules a plugin dimension may want to opt into.
    pub ultrawarm: bool,
    /// Kept here for a plugin's own server-side
    /// logic (e.g. deciding whether to run a day/night mob-spawning rule for
    /// this dimension); **not** what a real joined client's sky rendering
    /// reads, since that comes from the wire `dimension_type` registry entry
    /// (see the module doc).
    pub has_skylight: bool,
    /// Same caveat as `has_skylight`.
    pub has_ceiling: bool,
}

impl Default for DimensionProperties {
    /// The real `minecraft:overworld` entry — the safest default for a
    /// plugin dimension that wants ordinary player rules (beds, respawn
    /// anchors that don't explode, full vertical build range) and differs
    /// from the real overworld only in its terrain.
    fn default() -> Self {
        Self {
            min_y: -64,
            height: 384,
            logical_height: 384,
            coordinate_scale: 1.0,
            natural: true,
            bed_works: true,
            respawn_anchor_works: false,
            piglin_safe: false,
            ultrawarm: false,
            has_skylight: true,
            has_ceiling: false,
        }
    }
}

/// One plugin-registered dimension: a key, its properties, and the generator
/// backing it.
pub struct PluginDimension {
    /// A namespaced id (`"myplugin:void"`), the plugin's own key — never a
    /// vanilla `minecraft:` key, so a registration can never shadow
    /// [`crate::dimension::Dimension`]'s three hosted levels.
    pub key: String,
    pub properties: DimensionProperties,
    pub generator: Arc<dyn ChunkGenerator>,
}

impl std::fmt::Debug for PluginDimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginDimension")
            .field("key", &self.key)
            .field("properties", &self.properties)
            .finish_non_exhaustive()
    }
}

/// The registry a plugin populates at startup and a world-creation path
/// consults by key.
///
/// Cheap to hold: registering a dimension stores only its properties and
/// generator — nothing is generated until [`Self::chunk_source`] is asked
/// for one, and the built [`ChunkSource`] is cached (one per key, for the
/// life of the registry) so repeated lookups (a reconnecting player, a
/// second connection) share the same edit state rather than each opening an
/// independent, edit-blind copy of the world.
#[derive(Default)]
pub struct DimensionRegistry {
    entries: RwLock<HashMap<String, Arc<PluginDimension>>>,
    sources: Mutex<HashMap<String, Arc<dyn ChunkSource>>>,
}

impl DimensionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `dimension` under its own key, replacing any earlier
    /// registration for the same key (a plugin reloading its own config, for
    /// one). Returns the previous registration, if any, so a caller can
    /// notice a name clash rather than silently losing it.
    pub fn register(&self, dimension: PluginDimension) -> Option<Arc<PluginDimension>> {
        let key = dimension.key.clone();
        // A stale cached source under this key must not survive a
        // re-registration — otherwise a plugin reloading its generator would
        // keep serving the *old* one from the cache below, silently.
        self.sources.lock().expect("dimension source cache lock poisoned").remove(&key);
        self.entries
            .write()
            .expect("dimension registry lock poisoned")
            .insert(key, Arc::new(dimension))
    }

    /// The registered entry for `key`, if any.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Arc<PluginDimension>> {
        self.entries
            .read()
            .expect("dimension registry lock poisoned")
            .get(key)
            .cloned()
    }

    /// Every registered key, for a caller listing available custom worlds
    /// (a world-creation menu, a `/world list` command).
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        self.entries
            .read()
            .expect("dimension registry lock poisoned")
            .keys()
            .cloned()
            .collect()
    }

    /// A real [`ChunkSource`] for `key`'s generator, built (and cached) on
    /// first request. `None` if nothing is registered under `key`.
    ///
    /// This is the seam a world-open path consumes: pass the result as the
    /// `S: ChunkSource` argument to
    /// `IntegratedServer::open_in_memory_with_entities`/
    /// `open_persistent_with_mobs` in place of `overworld_chunk_source(seed)`.
    #[must_use]
    pub fn chunk_source(&self, key: &str) -> Option<Arc<dyn ChunkSource>> {
        if let Some(cached) = self
            .sources
            .lock()
            .expect("dimension source cache lock poisoned")
            .get(key)
        {
            return Some(Arc::clone(cached));
        }
        let entry = self.get(key)?;
        let source: Arc<dyn ChunkSource> =
            Arc::new(PluginChunkSource::new(Arc::clone(&entry.generator)));
        self.sources
            .lock()
            .expect("dimension source cache lock poisoned")
            .insert(key.to_string(), Arc::clone(&source));
        Some(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_worldgen::dense_grid::DenseBlockGrid;

    struct AllStone {
        min_y: i32,
        height: i32,
    }

    impl ChunkGenerator for AllStone {
        fn min_y(&self) -> i32 {
            self.min_y
        }
        fn height(&self) -> i32 {
            self.height
        }
        fn generate(&self, cx: i32, cz: i32) -> DenseBlockGrid {
            let mut grid =
                DenseBlockGrid::new(cx * 16, self.min_y, cz * 16, 16, self.height, 16, "minecraft:air");
            for lx in 0..16 {
                for lz in 0..16 {
                    grid.set(cx * 16 + lx, self.min_y, cz * 16 + lz, "minecraft:stone");
                }
            }
            grid
        }
        fn biome(&self) -> &str {
            "minecraft:the_void"
        }
    }

    #[test]
    fn unregistered_key_returns_nothing() {
        let registry = DimensionRegistry::new();
        assert!(registry.get("nope:nothing").is_none());
        assert!(registry.chunk_source("nope:nothing").is_none());
    }

    #[test]
    fn registered_dimension_is_reachable_by_key_and_generates_real_terrain() {
        let registry = DimensionRegistry::new();
        registry.register(PluginDimension {
            key: "voidworld:test".to_string(),
            properties: DimensionProperties {
                min_y: 0,
                height: 16,
                logical_height: 16,
                ..Default::default()
            },
            generator: Arc::new(AllStone { min_y: 0, height: 16 }),
        });

        assert_eq!(registry.keys(), vec!["voidworld:test".to_string()]);
        let entry = registry.get("voidworld:test").expect("just registered");
        assert_eq!(entry.properties.height, 16);

        let source = registry.chunk_source("voidworld:test").expect("just registered");
        assert_eq!(source.block_state(0, 0, 0), "minecraft:stone");
        assert_eq!(source.block_state(0, 1, 0), "minecraft:air");
    }

    #[test]
    fn chunk_source_is_cached_so_edits_persist_across_lookups() {
        let registry = DimensionRegistry::new();
        registry.register(PluginDimension {
            key: "voidworld:test".to_string(),
            properties: DimensionProperties::default(),
            generator: Arc::new(AllStone { min_y: 0, height: 16 }),
        });

        let first = registry.chunk_source("voidworld:test").unwrap();
        first.set_block(0, 1, 0, "minecraft:diamond_block");

        let second = registry.chunk_source("voidworld:test").unwrap();
        assert_eq!(
            second.block_state(0, 1, 0),
            "minecraft:diamond_block",
            "a second lookup of the same key must see the first's edit — proof the source is \
             cached, not rebuilt (and edit-blind) on every call"
        );
    }

    #[test]
    fn re_registering_a_key_drops_the_stale_cached_source() {
        let registry = DimensionRegistry::new();
        registry.register(PluginDimension {
            key: "voidworld:test".to_string(),
            properties: DimensionProperties::default(),
            generator: Arc::new(AllStone { min_y: 0, height: 16 }),
        });
        let _ = registry.chunk_source("voidworld:test").unwrap();

        registry.register(PluginDimension {
            key: "voidworld:test".to_string(),
            properties: DimensionProperties::default(),
            generator: Arc::new(AllStone { min_y: 5, height: 16 }),
        });
        let source = registry.chunk_source("voidworld:test").unwrap();
        assert_eq!(
            source.block_state(0, 5, 0),
            "minecraft:stone",
            "the re-registered generator (stone at y=5) must be the one actually served, not a \
             stale cached source built from the first registration (stone at y=0)"
        );
        assert_eq!(source.block_state(0, 0, 0), "minecraft:air");
    }
}
