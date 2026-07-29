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

/// An [`EcsHandle`] onto a `World` that carries [`crate::CorePlugin`]'s
/// schedules, [`crate::ingest::IngestPlugin`]'s entity-ingest systems and
/// [`crate::SessionPlugin`]'s session folds — i.e. a `World` that is
/// *authoritative* over the network read-model and can be handed
/// `ClientEvent`s.
///
/// The caller still has to [`crate::spawn_session`] the entity the session
/// components hang off: a plugin registers *behaviour*, and which entity is
/// "this client" is the owner's decision, exactly as
/// [`crate::spawn_local_player`] is not done by `LocalPlayerPlugin`.
///
/// The `App` is built only to run the plugins' `build` (which is the only way
/// to register schedules and systems) and is then discarded, keeping the
/// `World` it produced. That is azalea's own shape: it takes the `World` out of
/// the `App` and puts it behind an `Arc<RwLock<_>>`
/// (`azalea-client/src/client.rs:143`) because the driver is a hand-written
/// loop, not `App::run`. Nothing here calls `App::update`, so the discarded
/// `App`'s own `Main` schedule ordering is irrelevant; the caller runs named
/// schedules on the `World` directly (`world.run_schedule(NetIngest)`).
///
/// Carries no [`crate::WorldTime`] — see [`new_handle`] and
/// [`crate::CorePlugin`] on why inserting that is left to whoever is making a
/// particular `World` authoritative over the clock.
#[must_use]
pub fn new_ingest_handle() -> EcsHandle {
    let mut app = bevy_app::App::new();
    app.add_plugins((crate::ingest::IngestPlugin, crate::SessionPlugin));
    Arc::new(RwLock::new(std::mem::take(app.world_mut())))
}
