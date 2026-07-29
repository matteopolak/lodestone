//! The handle non-driver code uses to reach a live `World`.

use std::sync::Arc;

use bevy_ecs::world::World;
use parking_lot::RwLock;

/// A cheap, cloneable handle onto a live [`World`], for readers that are not
/// the schedule driver itself — async bot code, `ClientHandle`, anything off
/// the driver's own thread.
///
/// `docs/bevy-migration.md` §4.1(c), as shipped: since the unification the
/// driver holds this too, because there is exactly one `World` and the net
/// thread's ingest writes into it. azalea's `Client` wraps the same shape
/// (`azalea-client/src/client.rs:143`) so bot code can read and write from async
/// context (`azalea/src/bot.rs:85`). `parking_lot` rather than
/// `std::sync::RwLock` per §11, matching azalea's choice for the same lock.
///
/// # Lock discipline — three rules, and the reasons are not style
///
/// 1. **Never hold a guard across a call that might take the same lock.** In
///    particular the driver must not hold one while calling into
///    `NetClient`/`ClientHandle`: every read on those locks this same `World`,
///    and `parking_lot::RwLock` is neither reentrant nor upgradable, so
///    `write()` → `…read()` on one thread is an instant deadlock and
///    `read()` → `read()` deadlocks too whenever a writer is already queued
///    (that is what `read_recursive` exists for, and we do not rely on it).
///    Take the lock for one statement, or one `run_schedule`, and let it go.
/// 2. **Never hold a guard across an `.await`.** `lodestone_client`'s driver
///    already promised this for the scalar read-model
///    (`state.rs`); it now matters for this lock too, because a task parked with
///    the `World` write-locked would stall the frame.
/// 3. **`World` before chunks, never the reverse.** The driver takes this lock
///    and *then* (inside a system) the `ChunkWorld` lock; the net thread takes
///    the chunk lock for `handle_packet` and releases it **before** folding
///    events, so it only ever takes this one afterward. Both orders are
///    `World → chunks`. Reversing either side is an ABBA deadlock, and nothing
///    in the type system stops it.
///
/// # What contention this actually creates
///
/// The net thread takes a short `write()` per folded `ClientEvent`; the driver
/// takes one per `run_schedule` and one per accessor. They now genuinely contend
/// where they did not before, and **§4.1(a)'s promise does not cover this lock**:
/// it says a slow frame "delays *application*, never *receipt*", which is true of
/// the socket→`ClientEvent` channel but false here, because
/// `lodestone_client::state::SharedState::apply` runs *inline in the driver task*
/// before `events.send(event).await`. Blocking on this lock therefore blocks the
/// task that reads the socket.
///
/// What bounds it is rule 1: **no guard spans a frame.** The driver takes many
/// short guards (one per `run_schedule`, one per accessor) rather than one long
/// one, so the worst a packet waits is one guard hold — sub-millisecond against
/// keep-alive timeouts measured in seconds. That bound is structural, not measured;
/// see `docs/world-unification.md` for the longest known hold and what would
/// measure it.
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
/// # Not the shell's path since §4.1(c)
///
/// `lodestone_shell::sim::Sim` builds the one `World` itself (with these plugins
/// among many others) and hands the handle *down* through `NetClient::connect` →
/// `ClientBuilder::ecs`. This constructor is for a client with **no driver** —
/// `lodestone_client::state::SharedState::default`, i.e. a bot or a test — which
/// legitimately owns its own `World`. Do not use it to make a second `World` in a
/// process that already has a driver: a component folded here would be invisible to
/// every one of that driver's systems, which is the defect §4.1(c) deleted.
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
