//! The handle non-driver code uses to reach a live `World`.

use std::sync::Arc;

use bevy_ecs::world::World;
use parking_lot::RwLock;

/// A cheap, cloneable handle onto a live [`World`], for readers that are not
/// the schedule driver itself — async bot code, `ClientHandle`, anything off
/// the driver's own thread.
///
/// `docs/bevy-migration.md` §4.1(c): the driver that owns the `World`
/// outright never needs this (it just holds `World` or `App` by value); this
/// type exists for the *outsiders*, exactly as azalea's `Client` wraps
/// `Arc<parking_lot::RwLock<World>>` (`azalea-client/src/client.rs:143`) so
/// bot code can read/write from async context (`azalea/src/bot.rs:85`).
/// `parking_lot` rather than `std::sync::RwLock` per §11, matching azalea's
/// choice for the same lock.
pub type EcsHandle = Arc<RwLock<World>>;

/// Builds a fresh, empty [`World`] wrapped as an [`EcsHandle`].
///
/// Carries no resources of its own — see the note on [`crate::CorePlugin`]
/// about why inserting [`crate::WorldTime`] is left to whoever is making this
/// particular `World` authoritative, rather than done unconditionally here.
#[must_use]
pub fn new_handle() -> EcsHandle {
    Arc::new(RwLock::new(World::new()))
}
