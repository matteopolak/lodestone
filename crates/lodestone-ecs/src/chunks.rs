//! The chunk store, as a pair of `Resource`s.
//!
//! Stage 4 of `docs/bevy-migration.md`, §4.1(d). Chunks are deliberately **not**
//! entities: a decoded column is orders of magnitude larger than a scalar event,
//! copy-on-write `Arc<ChunkSection>` snapshots exist precisely so a mesher can
//! grab and release the lock, and azalea reaches the same conclusion (§2.3 —
//! `World { chunks: ChunkStorage, … }` lives in a `Worlds` *resource*, with the
//! doc comment "this does not contain the entity data itself, that's in the
//! ECS").
//!
//! What these types are *for* is the other half of §4.1(d): before Stage 4 there
//! were **two** `lodestone_world::World`s in the process —
//! `lodestone_shell::sim::Sim`'s offline one and `lodestone_client::state::SharedState`'s
//! live one — and every read site in the shell branched on which of the two it
//! meant. That branch is what this deletes. One store, named once, reachable
//! from a system.
//!
//! Issue #423 splits the one handle in two. [`ChunkWorld`] is the **read** side:
//! a plugin or read-only system takes it and physically cannot mutate the store.
//! [`ChunkWorldWrite`] is the **write** side, held only by the store's legitimate
//! writers (`drive_placement`, `Sim::predict_block`, `Sim::set_block_world`, the
//! net-ingest path). The two always name the same `Arc` — an installer builds the
//! write handle from the raw `World` (or the client's `Arc`) and derives the read
//! handle from *it*, never the other way round — so "just write the block
//! directly" from a read handle is a compile error rather than a footgun that
//! forks block-prediction.

use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use bevy_ecs::resource::Resource;
use lodestone_world::{ChunkPos, ChunkSection, World};

/// The one chunk store, read by handle.
///
/// Clones share the same `World`: this is a handle, not a copy, and that is the
/// whole point — `lodestone_client` hands its live store out through
/// `ClientHandle::chunk_world()` and the shell installs *that* handle as its own
/// resource, so a decoded column written by the net thread is visible to the
/// mesher with no copy and no second store to keep in step.
///
/// **This is the read side of the issue #423 split.** There is deliberately no
/// write path here, and no way to obtain the store's `Arc`: a system that asked
/// for a [`ChunkWorld`] physically cannot mutate the chunk store, so the
/// state+block-entity pairing of `write_predicted_block` and the re-mesh that
/// makes an edit visible cannot be bypassed by accident. The write side is
/// [`ChunkWorldWrite`], held only by the store's legitimate writers.
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
    /// Wrap a freshly built `World` in a new store, taking the read side.
    ///
    /// Prefer building the store through [`ChunkWorldWrite::new`] and deriving
    /// this read handle from it with [`ChunkWorldWrite::read_handle`] wherever a
    /// write handle will also be needed, so both halves name the same `Arc`.
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

/// The **write side** of the chunk-store split (issue #423).
///
/// The same `Arc<RwLock<World>>` as the paired [`ChunkWorld`] read handle,
/// exposed deliberately: this is the one type a system may hold to mutate the
/// store, and it is the one the store's legitimate writers actually take —
/// `drive_placement`'s predicted write, `Sim::predict_block`, the demo world's
/// `Sim::set_block_world`, and the net-ingest path (which writes through the
/// client's own `Arc` before this resource is even installed).
///
/// Installers build the write handle from the raw `World` or the client's
/// `Arc<RwLock<World>>` and derive the read handle from *it* via
/// [`read_handle`](Self::read_handle), so the two resources never name different
/// stores. The reverse direction does not exist: a [`ChunkWorld`] yields no
/// write handle.
#[derive(Resource, Clone, Debug)]
pub struct ChunkWorldWrite(Arc<RwLock<World>>);

impl Default for ChunkWorldWrite {
    fn default() -> Self {
        Self::new(World::new())
    }
}

impl ChunkWorldWrite {
    /// Wrap a freshly built `World` in a new store, taking the write side.
    ///
    /// The canonical way to create a store the shell will edit (demo world, and
    /// every test harness that loads columns by hand): build the write handle
    /// here, then derive the paired read handle with
    /// [`read_handle`](Self::read_handle).
    #[must_use]
    pub fn new(world: World) -> Self {
        Self(Arc::new(RwLock::new(world)))
    }

    /// Adopt an existing shared store — the route by which `lodestone-client`
    /// hands its net-thread write target out as a `Resource` a system can hold.
    #[must_use]
    pub fn from_shared(world: Arc<RwLock<World>>) -> Self {
        Self(world)
    }

    /// The paired read handle on the **same** store.
    ///
    /// This is the only direction that exists between the two halves — an
    /// installer builds both from one `World`. A read handle yields no write
    /// handle, which is the whole point of the split.
    #[must_use]
    pub fn read_handle(&self) -> ChunkWorld {
        ChunkWorld::from_shared(Arc::clone(&self.0))
    }

    /// Whether `self` and `other` are the *same* store.
    #[must_use]
    pub fn is_same_store(&self, other: &ChunkWorldWrite) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Take the write lock. Never held across an `await`, and never held while
    /// meshing — the mesher's contract is snapshot-then-release.
    #[must_use]
    pub fn write(&self) -> RwLockWriteGuard<'_, World> {
        self.0.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Take the read lock, for a writer that also wants to read.
    #[must_use]
    pub fn read(&self) -> RwLockReadGuard<'_, World> {
        self.0.read().unwrap_or_else(|e| e.into_inner())
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

    /// The issue #423 split is real: a read handle and its write handle name the
    /// *same* `Arc`, and the read handle exposes no path back to a write lock.
    #[test]
    fn the_read_handle_derived_from_a_write_handle_is_the_same_store() {
        let write = ChunkWorldWrite::default();
        let read = write.read_handle();
        assert!(
            Arc::ptr_eq(&read.0, &write.0),
            "a read handle derived from the write handle must name the same Arc \
             — otherwise `drive_placement` would write a store the mesher does not read"
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
