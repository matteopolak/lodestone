//! The chunk store, as a `Resource`.
//!
//! Stage 4 of `docs/bevy-migration.md`, §4.1(d). Chunks are deliberately **not**
//! entities: a decoded column is orders of magnitude larger than a scalar event,
//! copy-on-write `Arc<ChunkSection>` snapshots exist precisely so a mesher can
//! grab and release the lock, and azalea reaches the same conclusion (§2.3 —
//! `World { chunks: ChunkStorage, … }` lives in a `Worlds` *resource*, with the
//! doc comment "this does not contain the entity data itself, that's in the
//! ECS").
//!
//! What this type is *for* is the other half of §4.1(d): before it there were
//! **two** `lodestone_world::World`s in the process — `lodestone_shell::sim::Sim`'s
//! offline one and `lodestone_client::state::SharedState`'s live one — and every
//! read site in the shell branched on which of the two it meant. That branch is
//! what this deletes. One store, named once, reachable from a system.

use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use bevy_ecs::resource::Resource;
use lodestone_world::{ChunkPos, ChunkSection, World};

/// The one chunk store, shared by handle.
///
/// Clones share the same `World`: this is a handle, not a copy, and that is the
/// whole point — `lodestone_client` hands its live store out through
/// `ClientHandle::chunk_world()` and the shell installs *that* handle as its own
/// resource, so a decoded column written by the net thread is visible to the
/// mesher with no copy and no second store to keep in step.
///
/// # The lock is `std::sync::RwLock`, not `parking_lot`
///
/// `docs/bevy-migration.md` §11 prescribes `parking_lot` for "the World lock",
/// and [`crate::EcsHandle`] uses it. That prescription is about the **bevy**
/// `World`. This lock is the one `SharedState` already owns and that the version
/// adapter writes decoded columns through as a `lodestone_world::WorldSink`;
/// converting it would churn every world access in `lodestone-client` for no
/// behavioural gain, and the two locks are never taken in a nested pair.
/// Poisoning is recovered rather than propagated (`into_inner`), matching what
/// `SharedState` already did at every one of its call sites.
#[derive(Resource, Clone, Debug)]
pub struct ChunkWorld(Arc<RwLock<World>>);

impl Default for ChunkWorld {
    fn default() -> Self {
        Self::new(World::new())
    }
}

impl ChunkWorld {
    /// Wrap a freshly built `World` in a new store.
    #[must_use]
    pub fn new(world: World) -> Self {
        Self(Arc::new(RwLock::new(world)))
    }

    /// Adopt an existing shared store — the route by which the shell comes to
    /// name the *same* `World` the net thread writes into.
    #[must_use]
    pub fn from_shared(world: Arc<RwLock<World>>) -> Self {
        Self(world)
    }

    /// The underlying handle, for a caller that has to hand the same store to
    /// something that cannot hold a `Resource` (a `'static` render-side closure,
    /// say).
    #[must_use]
    pub fn shared(&self) -> &Arc<RwLock<World>> {
        &self.0
    }

    /// Whether `self` and `other` are the *same* store rather than two stores
    /// that happen to hold equal data.
    ///
    /// This is the authority test for §4.1(d) expressed as a function: after
    /// adoption, the shell's resource and the client's field must be
    /// `Arc::ptr_eq`. Two stores with identical contents would pass any
    /// data-comparing assertion and still be the defect.
    #[must_use]
    pub fn is_same_store(&self, other: &ChunkWorld) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Take the read lock. Never held across an `await`, and never held while
    /// meshing — the mesher's contract is snapshot-then-release.
    #[must_use]
    pub fn read(&self) -> RwLockReadGuard<'_, World> {
        self.0.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Take the write lock.
    #[must_use]
    pub fn write(&self) -> RwLockWriteGuard<'_, World> {
        self.0.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Loaded column count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.read().len()
    }

    /// Whether no column is loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    /// Whether the column at `(cx, cz)` is loaded.
    #[must_use]
    pub fn contains_column(&self, cx: i32, cz: i32) -> bool {
        self.read().contains(ChunkPos::new(cx, cz))
    }

    /// The vertical shape of whatever dimension this store holds, or `None` when
    /// no column is loaded yet.
    ///
    /// Read off *any* loaded column: every column in a dimension shares one
    /// shape, because the adapter builds them all from the single dimension type
    /// the server sent at login. This is what lets one derivation serve both the
    /// live world (where it used to come from `ClientHandle::world_dimensions`)
    /// and the offline one (where the shell used to hard-code
    /// `worldgen::MIN_Y` and *no* section count at all).
    #[must_use]
    pub fn extent(&self) -> Option<WorldExtent> {
        let world = self.read();
        let column = &world.values().next()?.column;
        Some(WorldExtent {
            min_y: column.min_y(),
            section_count: column.section_count(),
        })
    }
}

/// The vertical shape of a dimension: where its lowest block section starts and
/// how many block sections a column holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldExtent {
    /// Lowest world-space `y` a column stores.
    pub min_y: i32,
    /// Number of 16-tall block sections per column.
    pub section_count: usize,
}

impl WorldExtent {
    /// Total column height in blocks.
    #[must_use]
    pub fn height(&self) -> usize {
        self.section_count * ChunkSection::EDGE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::world::World as EcsWorld;

    #[test]
    fn a_clone_is_the_same_store_not_a_copy() {
        let a = ChunkWorld::default();
        let b = a.clone();
        assert!(a.is_same_store(&b));
        assert!(
            !a.is_same_store(&ChunkWorld::default()),
            "two independently built stores must not read as one — otherwise \
             `is_same_store` could not detect the two-worlds defect it exists for"
        );
    }

    /// The resource round-trips: a write through one handle is visible through
    /// the resource the `World` holds, which is what makes a system's view of the
    /// chunk store the same view the net thread writes into.
    #[test]
    fn a_write_through_one_handle_is_visible_through_the_resource() {
        let store = ChunkWorld::default();
        let mut ecs = EcsWorld::new();
        ecs.insert_resource(store.clone());

        assert!(ecs.resource::<ChunkWorld>().is_empty());
        assert!(store.is_same_store(ecs.resource::<ChunkWorld>()));
    }

    #[test]
    fn extent_is_none_until_a_column_is_loaded() {
        assert_eq!(ChunkWorld::default().extent(), None);
    }
}
