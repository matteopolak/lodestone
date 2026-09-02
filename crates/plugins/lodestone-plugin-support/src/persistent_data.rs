//! The persistent data container's non-persistent half: an in-memory, namespaced key-value store,
//! attachable to an entity or a chunk — the familiar plugin per-object metadata
//! convention, minus the "survives a restart" guarantee, which
//! the issue itself scopes out until world persistence (Anvil, Tier 4 of
//! `docs/backlog.md`) exists at all.
//!
//! # Why `serde_json::Value`, not a decoded struct
//!
//! `CLAUDE.md` names a specific, paid-for hazard: a round-trip that decides
//! which fields to carry through by consulting a **static name list** silently
//! drops data it failed to decode (the `Age`/`Health` NBT-collision incident).
//! That hazard is about excluding a field *because* a schema didn't name it.
//! This store never decodes into named fields at all — every value is an
//! opaque [`serde_json::Value`] keyed by whatever string the plugin chose, so
//! there is no schema to fall out of sync with and nothing gets silently
//! excluded. **This matters again the moment persistence is added**: whoever
//! builds the Tier 2 half of this issue must carry each entry through
//! wholesale (e.g. as one opaque NBT compound blob per key) rather than
//! decoding into a fixed set of named fields and excluding whatever a schema
//! does not list — the exact shape that hazard needs to recur.
//!
//! # Why two stores, not one keyed by an enum
//!
//! Entities and chunks have unrelated key types
//! ([`lodestone_ecs::entity::MinecraftEntityId`] vs. [`lodestone_world::ChunkPos`])
//! and unrelated lifetimes (an entity can despawn and its id be reused; a
//! chunk unloads and reloads with the same [`ChunkPos`] identity). Folding both
//! into one resource behind an enum key would only hide that they are
//! different problems with different eviction rules.
//!
//! # The id-reuse hazard, named rather than solved here
//!
//! [`docs/entity-components.md`](../../../../docs/entity-components.md) records
//! that a despawned entity's id can be reused, and that `apply_entity_removal`
//! specifically guards against taking out the local player's identity for
//! exactly that reason. This store has **no automatic eviction** on despawn —
//! wiring that would mean reading `lodestone-ecs`'s own ingest fold, which is
//! outside this crate's dependency direction (plugins depend on the engine,
//! never the reverse) and outside this crate's ownership for this pass. A
//! plugin that cares must call [`EntityDataStore::remove_entity`] itself when
//! it observes a despawn (e.g. via `lodestone_ecs::GameEvent`), or accept that
//! a reused id could read back a previous occupant's data. This is stated
//! plainly rather than papered over: it is the real cost of building the
//! entity half of this store without also owning the despawn fold.

use std::collections::HashMap;

use bevy_app::{App, Plugin};
use bevy_ecs::resource::Resource;
use lodestone_ecs::entity::MinecraftEntityId;
use lodestone_world::ChunkPos;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Builds the `"<plugin>:<key>"` form every store here uses, the familiar
/// namespaced-key convention — two plugins that both choose
/// `"balance"` do not collide.
#[must_use]
pub fn namespaced_key(plugin: &str, key: &str) -> String {
    format!("{plugin}:{key}")
}

/// A generic namespaced-key value bag, shared by [`EntityDataStore`] and
/// [`ChunkDataStore`] so the two stay behaviourally identical and only their
/// key type differs.
#[derive(Debug, Default, Clone)]
struct DataBag(HashMap<String, Value>);

impl DataBag {
    fn set<T: Serialize>(&mut self, key: &str, value: &T) -> serde_json::Result<()> {
        self.0.insert(key.to_string(), serde_json::to_value(value)?);
        Ok(())
    }

    fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.0
            .get(key)
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
    }

    fn remove(&mut self, key: &str) -> Option<Value> {
        self.0.remove(key)
    }

    fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }
}

/// Per-entity plugin key-value storage, keyed by
/// [`MinecraftEntityId`] — the id every `ClientEvent`/component names an
/// entity by, so a plugin already has this key from any entity query.
#[derive(Resource, Debug, Default)]
pub struct EntityDataStore(HashMap<i32, DataBag>);

impl EntityDataStore {
    /// Stores `value` under `namespaced_key` for `entity`. `namespaced_key`
    /// should come from [`namespaced_key`] to avoid cross-plugin collisions.
    ///
    /// # Errors
    ///
    /// Only if `T`'s `Serialize` impl itself fails (e.g. a `NaN` float, or a
    /// non-string map key) — the same conditions `serde_json::to_value`
    /// documents, never a storage-layer failure.
    pub fn set<T: Serialize>(
        &mut self,
        entity: MinecraftEntityId,
        namespaced_key: &str,
        value: &T,
    ) -> serde_json::Result<()> {
        self.0.entry(entity.0).or_default().set(namespaced_key, value)
    }

    /// Reads back a value stored by [`set`](Self::set), or `None` if nothing
    /// is stored under that key for that entity, or if it does not deserialize
    /// as `T`.
    #[must_use]
    pub fn get<T: DeserializeOwned>(&self, entity: MinecraftEntityId, namespaced_key: &str) -> Option<T> {
        self.0.get(&entity.0)?.get(namespaced_key)
    }

    /// Removes a single key for one entity, returning the raw value that was
    /// there.
    pub fn remove(&mut self, entity: MinecraftEntityId, namespaced_key: &str) -> Option<Value> {
        self.0.get_mut(&entity.0)?.remove(namespaced_key)
    }

    /// Whether `entity` has a value stored under `namespaced_key`.
    #[must_use]
    pub fn has(&self, entity: MinecraftEntityId, namespaced_key: &str) -> bool {
        self.0.get(&entity.0).is_some_and(|bag| bag.has(namespaced_key))
    }

    /// Drops every key stored for `entity`, returning whether anything was
    /// there. The manual eviction hook this module's doc names — call this
    /// when a plugin observes the entity despawn.
    pub fn remove_entity(&mut self, entity: MinecraftEntityId) -> bool {
        self.0.remove(&entity.0).is_some()
    }

    /// How many distinct entities currently have at least one stored key —
    /// a plain count, useful for a plugin's own leak-detection self-check
    /// (`CLAUDE.md`'s "a count, not a duration").
    #[must_use]
    pub fn tracked_entity_count(&self) -> usize {
        self.0.len()
    }
}

/// Per-chunk plugin key-value storage, keyed by [`ChunkPos`].
///
/// Unlike [`EntityDataStore`], a [`ChunkPos`] is stable across unload/reload —
/// the identity is the grid coordinate, not an allocated id — so there is no
/// analogous id-reuse hazard here. Nothing evicts an unloaded chunk's data
/// automatically either, since that is a policy choice (a plugin re-visiting a
/// chunk later may want its data back exactly as it left it) rather than a
/// safety concern the way stale entity data is.
#[derive(Resource, Debug, Default)]
pub struct ChunkDataStore(HashMap<ChunkPos, DataBag>);

impl ChunkDataStore {
    /// Stores `value` under `namespaced_key` for the chunk at `pos`. See
    /// [`EntityDataStore::set`] for the error condition.
    pub fn set<T: Serialize>(
        &mut self,
        pos: ChunkPos,
        namespaced_key: &str,
        value: &T,
    ) -> serde_json::Result<()> {
        self.0.entry(pos).or_default().set(namespaced_key, value)
    }

    /// Reads back a value stored by [`set`](Self::set).
    #[must_use]
    pub fn get<T: DeserializeOwned>(&self, pos: ChunkPos, namespaced_key: &str) -> Option<T> {
        self.0.get(&pos)?.get(namespaced_key)
    }

    /// Removes a single key for one chunk, returning the raw value that was
    /// there.
    pub fn remove(&mut self, pos: ChunkPos, namespaced_key: &str) -> Option<Value> {
        self.0.get_mut(&pos)?.remove(namespaced_key)
    }

    /// Whether the chunk at `pos` has a value stored under `namespaced_key`.
    #[must_use]
    pub fn has(&self, pos: ChunkPos, namespaced_key: &str) -> bool {
        self.0.get(&pos).is_some_and(|bag| bag.has(namespaced_key))
    }

    /// Drops every key stored for the chunk at `pos`.
    pub fn remove_chunk(&mut self, pos: ChunkPos) -> bool {
        self.0.remove(&pos).is_some()
    }

    /// How many distinct chunks currently have at least one stored key.
    #[must_use]
    pub fn tracked_chunk_count(&self) -> usize {
        self.0.len()
    }
}

/// Installs both [`EntityDataStore`] and [`ChunkDataStore`] as resources.
///
/// Adds no systems of its own — this is pure storage a plugin's own systems
/// read and write directly through `Res`/`ResMut`, the same shape
/// [`lodestone_ecs::ChunkWorldWrite`] uses. `init_resource`, so adding it
/// twice (two plugins that both want the store) is a no-op, not a panic.
#[derive(Debug, Default)]
pub struct PersistentDataPlugin;

impl Plugin for PersistentDataPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EntityDataStore>();
        app.init_resource::<ChunkDataStore>();
    }

    /// Multiple plugins each want the store and none of them knows whether
    /// another already added it — the same reasoning
    /// `lodestone_shop_api::ShopApiPlugin` documents. Without this, bevy's
    /// default duplicate-plugin check panics on the second `add_plugins` call.
    fn is_unique(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_set_on_one_entity_does_not_leak_to_another() {
        let mut store = EntityDataStore::default();
        let alice = MinecraftEntityId(1);
        let bob = MinecraftEntityId(2);
        let key = namespaced_key("economy", "balance");

        store.set(alice, &key, &100u32).unwrap();
        assert_eq!(store.get::<u32>(alice, &key), Some(100));
        assert_eq!(
            store.get::<u32>(bob, &key), None,
            "a different entity id must read back nothing, not alice's value"
        );
    }

    #[test]
    fn two_plugins_choosing_the_same_bare_key_do_not_collide() {
        let mut store = EntityDataStore::default();
        let e = MinecraftEntityId(7);
        let economy_key = namespaced_key("economy", "level");
        let quest_key = namespaced_key("quests", "level");

        store.set(e, &economy_key, &3u32).unwrap();
        store.set(e, &quest_key, &99u32).unwrap();

        assert_eq!(store.get::<u32>(e, &economy_key), Some(3));
        assert_eq!(
            store.get::<u32>(e, &quest_key),
            Some(99),
            "namespacing must keep the two plugins' identically-named bare \
             keys from overwriting one another"
        );
    }

    #[test]
    fn remove_entity_actually_evicts_every_key_not_just_stops_returning_them() {
        let mut store = EntityDataStore::default();
        let e = MinecraftEntityId(1);
        store.set(e, "a", &1u32).unwrap();
        store.set(e, "b", &2u32).unwrap();
        assert_eq!(store.tracked_entity_count(), 1);

        assert!(store.remove_entity(e));
        assert_eq!(
            store.tracked_entity_count(),
            0,
            "eviction must drop the entity's whole bag, not merely mask reads"
        );
        assert_eq!(store.get::<u32>(e, "a"), None);
        assert!(
            !store.remove_entity(e),
            "removing an entity with nothing stored must report false"
        );
    }

    #[test]
    fn arbitrary_struct_values_round_trip_through_the_opaque_value_store() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct ClaimBoundary {
            min: [i32; 2],
            max: [i32; 2],
            owner: String,
        }

        let mut store = ChunkDataStore::default();
        let pos = ChunkPos::new(4, -2);
        let boundary = ClaimBoundary {
            min: [0, 0],
            max: [16, 16],
            owner: "matteopolak".to_string(),
        };
        let key = namespaced_key("protection", "claim");

        store.set(pos, &key, &boundary).unwrap();
        assert_eq!(store.get::<ClaimBoundary>(pos, &key), Some(boundary));

        let other_pos = ChunkPos::new(4, -1);
        assert_eq!(
            store.get::<ClaimBoundary>(other_pos, &key),
            None,
            "a neighbouring chunk must not read another chunk's claim"
        );
    }

    #[test]
    fn getting_a_key_with_the_wrong_shape_reports_none_not_a_panic() {
        let mut store = EntityDataStore::default();
        let e = MinecraftEntityId(1);
        store.set(e, "k", &"a string value".to_string()).unwrap();

        // Asking for it back as a struct it cannot deserialize into must not
        // panic — it is a caller bug, not a storage-layer one, and the
        // contract is "None", matching every other lookup miss here.
        #[derive(serde::Deserialize)]
        struct NotAString {
            #[allow(dead_code)]
            field: u32,
        }
        assert!(store.get::<NotAString>(e, "k").is_none());
    }

    #[test]
    fn the_plugin_installs_both_stores_and_is_idempotent() {
        use lodestone_ecs::app::App;

        let mut app = App::new();
        app.add_plugins(PersistentDataPlugin);
        // A second plugin wanting the store too must not panic — `init_resource`
        // is idempotent, unlike a bare `insert_resource` twice would silently
        // clobber the first, or `add_plugins` of a *unique* plugin twice would
        // panic outright.
        app.add_plugins(PersistentDataPlugin);

        assert_eq!(
            app.world().resource::<EntityDataStore>().tracked_entity_count(),
            0
        );
        assert_eq!(
            app.world().resource::<ChunkDataStore>().tracked_chunk_count(),
            0
        );
    }
}
