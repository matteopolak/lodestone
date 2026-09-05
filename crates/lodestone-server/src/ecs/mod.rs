//! The server's own `bevy_ecs::World` (Phase 0 of
//! `docs/plans/server-ecs-migration.md`).
//!
//! # What it is
//!
//! [`ServerApp`] builds the server's `World` out of [`ServerCorePlugin`] and
//! hands it to whoever will own it. `docs/server-ecs.md` is the decision
//! record; the two facts from it that shape this module are:
//!
//! * **Two `World`s, never one.** This is not the client's `World` and never
//!   becomes it. The client's is version-decoded client state driven by
//!   `FrameClock`; this one is version-free simulation state that must keep
//!   advancing with no render loop attached at all — which is literally the
//!   case for open-to-LAN.
//! * **No lock, at all.** This `World` is owned outright by the tick task. It
//!   is not `Arc<RwLock<_>>`, not `EcsHandle`, and nothing outside the tick task
//!   may read it. Every connection task's job is to enqueue proposals and read
//!   published snapshots. The client pays `docs/world-unification.md`'s entire
//!   lock-discipline complexity because it has no choice; the server gets to
//!   not pay it.
//!
//! # The tick task owns a `World`, not an `App` — and that is forced, not a taste call
//!
//! Measured against `bevy_app` 0.19 rather than assumed: **`App` is not
//! `Send`.** Its `runner` field is `Box<dyn FnOnce(App) -> AppExit>` with no
//! `Send` bound (`bevy_app-0.19.0/src/app.rs:1537`), so an `App` cannot be
//! moved into a `tokio::spawn`ed future, and cannot be held across an `.await`
//! inside one either. `World` *is* `Send`, and it carries the `Schedules`
//! resource with it, so a `World` moved out of a finished `App` can still run
//! every schedule the plugins installed.
//!
//! So [`ServerApp`] is a **builder**: it exists to accept `add_plugins`, and
//! then [`ServerApp::into_world`] hands the tick task the thing it actually
//! owns. This happens to be exactly the phrasing `docs/server-ecs.md` already
//! used ("the server's `World` is held directly by the tick task"); Phase 0
//! only supplies the mechanical reason it could not have been otherwise.
//! **Phase 1 should thread `&mut World` into `crate::tick::run_tick_loop`, not
//! `&mut App`** — the latter does not compile behind `crate::spawn::spawn`.
//!
//! # How it works
//!
//! [`ServerApp::bootstrap`] builds an [`bevy_app::App::empty`] (never
//! `App::new`, which installs `MainSchedulePlugin` and with it the
//! frame-shaped `Main`/`Update` schedules), adds [`ServerCorePlugin`], and runs
//! the [`ServerBoot`] schedule exactly once. `crate::IntegratedServer`'s
//! `open_in_memory_with_mobs` calls it in production, synchronously, before
//! spawning the tick task, and the resulting `World` is moved into that task.
//!
//! # How to change it, and the gotchas
//!
//! * **Phase 0 is deliberately shallow. It is not, however, an island.** The
//!   distinction matters and it is the one thing to preserve when extending
//!   this. `WindowApp.ecs` on the client is an `App` that is
//!   constructed and never has a schedule run against it — "an inert scaffold
//!   nothing reads", open to this day. The only structural difference here is
//!   [`ServerTickWitness`]: production runs [`ServerBoot`], one registered
//!   system increments it, `crate::IntegratedServer::server_tick_count` reads it
//!   back, and `gate.rs`'s
//!   `the_production_integrated_server_runs_a_registered_system` asserts the
//!   *production* constructor moved it. Do not delete that witness while the
//!   `World` is still shallow.
//! * **Assert against the production-built `World`, never a hand-built one.** A
//!   test that builds its own `App` passes whether or not production wires
//!   anything; a production-wiring check is therefore required. The gate below
//!   calls `IntegratedServer::open_in_memory_with_mobs`, not
//!   `ServerApp::bootstrap`.
//! * **Do not add `lodestone-ecs` to this crate without re-running
//!   `scripts/wasm-size.sh`.** See `schedules.rs`'s doc for why Phase 0 took the
//!   two bevy crates and nothing else.
//! * **Nothing here may hand out a reference into the `World`.** The no-lock
//!   invariant is enforced by there being no accessor, not by convention. If you
//!   need a fact out of the `World`, publish a snapshot from
//!   [`TickSet::Publish`](schedules::TickSet::Publish) the way `LiveMobSource`
//!   already does.
//!
//! # Configuration
//!
//! None. There is no server-side plugin-loading mechanism, feature flag or
//! manifest yet — a plugin today is a `Cargo.toml` dependency added with
//! `App::add_plugins`, and Phase 0 records the decision to build that surface,
//! not the surface itself.
//!
//! # Dependencies
//!
//! `bevy_app` and `bevy_ecs` 0.19, pinned through the same
//! `[workspace.dependencies]` entries `lodestone-ecs` builds against —
//! `default-features = false, features = ["std"]`, so no `bevy_reflect` and,
//! crucially, never `multi_threaded`: that omission is what keeps this
//! migration free of a second threading model, and it is also why bevy
//! dispatches these systems as direct calls.
//!

use bevy_app::App;
use bevy_ecs::world::World;

#[cfg(test)]
mod gate;
#[cfg(test)]
mod messages;
pub(crate) mod plugin;
pub mod proposals;
pub(crate) mod schedules;
mod scheduler;

pub use plugin::{ServerCorePlugin, ServerTick, ServerTickWitness, advance_server_tick};
pub use proposals::{
    ProposalVerdict, ServerProposal, ServerProposalAction, ServerProposalDecisions,
    ServerProposalHandle, ServerProposalPlugin, SpawnProposalRefusal,
};
pub use schedules::{GameTick, IngestSet, NetIngest, ServerBoot, TickSet};
pub use scheduler::{ServerTaskId, ServerTaskScheduler, run_server_tasks};

/// Builder for the server's `World`: install plugins here, then call
/// [`into_world`](Self::into_world) to hand the tick task what it owns.
///
/// See this module's doc for why the split exists (`App` is `!Send`).
#[derive(Debug)]
pub struct ServerApp {
    app: App,
}

impl ServerApp {
    /// Builds the server `World` from [`ServerCorePlugin`] and runs
    /// [`ServerBoot`] once.
    ///
    /// `App::empty()` deliberately, not `App::new()`: the latter installs
    /// `MainSchedulePlugin`, which creates the `Main` and `Update` schedules —
    /// frame-shaped things a server must not have. `ServerCorePlugin`'s doc
    /// tables the three resources `lodestone_ecs::CorePlugin` would smuggle in
    /// for the same reason.
    #[must_use]
    pub fn bootstrap() -> Self {
        Self::bootstrap_with(|_| {})
    }

    /// Builds the server `World` with caller-supplied application
    /// configuration before plugin finalization and [`ServerBoot`].
    ///
    /// [`ServerCorePlugin`] is installed before `configure` runs, so native
    /// server plugins can add systems to the public server schedules and
    /// ordering sets. The completed [`App`] remains on the constructing
    /// thread; [`into_world`](Self::into_world) is the boundary used by the
    /// async tick task because `App` itself is not `Send`.
    #[must_use]
    pub fn bootstrap_with(configure: impl FnOnce(&mut App)) -> Self {
        let mut app = App::empty();
        app.add_plugins(ServerCorePlugin);
        configure(&mut app);
        app.finish();
        app.cleanup();
        app.world_mut().run_schedule(ServerBoot);
        Self { app }
    }

    /// The `App` under construction, for asserting on what a plugin installed.
    #[must_use]
    pub fn app(&self) -> &App {
        &self.app
    }

    /// Runs [`GameTick`] once. Phase 1 moves this call into
    /// `crate::tick::run_tick_loop`, against the `World` rather than this
    /// builder.
    pub fn run_game_tick(&mut self) {
        self.app.world_mut().run_schedule(GameTick);
    }

    /// A clone of this `World`'s [`ServerTickWitness`] — the only thing a
    /// holder keeps after [`into_world`](Self::into_world) has handed the
    /// `World` away. Read-only by construction; see that type's own doc for why
    /// it is a valve rather than a lock.
    ///
    /// # Panics
    ///
    /// If [`ServerCorePlugin`] was not installed. `bootstrap` always installs
    /// it, so this can only fire for a `ServerApp` assembled some other way.
    #[must_use]
    pub fn witness(&self) -> ServerTickWitness {
        self.app.world().resource::<ServerTickWitness>().clone()
    }

    /// A cloneable ingress for actions that must be adjudicated by the tick
    /// task before they mutate server state.
    ///
    /// This is deliberately a sender, never a `World` accessor: callers can
    /// wait for an answer without borrowing or locking the tick-owned world.
    #[must_use]
    pub fn proposal_handle(&self) -> ServerProposalHandle {
        self.app.world().resource::<ServerProposalHandle>().clone()
    }

    /// [`ServerTick::count`] — how many schedule runs have executed
    /// [`advance_server_tick`] in *this* `World`.
    #[must_use]
    pub fn tick_count(&self) -> u64 {
        self.app
            .world()
            .get_resource::<ServerTick>()
            .map_or(0, |tick| tick.count)
    }

    /// Consumes the builder and yields the `World` the tick task owns.
    ///
    /// The `Schedules` resource travels with the `World`, so every schedule
    /// [`ServerCorePlugin`] installed is still runnable through
    /// `World::run_schedule`. What is left behind is the `App` wrapper, whose
    /// only remaining job was `add_plugins` — and whose non-`Send` `runner`
    /// field is the reason this method exists.
    #[must_use]
    pub fn into_world(mut self) -> World {
        std::mem::take(self.app.world_mut())
    }
}

impl Default for ServerApp {
    fn default() -> Self {
        Self::bootstrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `into_world` must carry the schedules across, or Phase 1 has nothing to
    /// drive: the `World` the tick task owns has to be able to run `GameTick`
    /// on its own, with no `App` left anywhere.
    #[test]
    fn the_extracted_world_still_runs_its_schedules() {
        let mut world = ServerApp::bootstrap().into_world();
        assert_eq!(
            world.get_resource::<ServerTick>().map(|t| t.count),
            Some(1),
            "the extracted World must carry ServerBoot's result"
        );
        world.run_schedule(GameTick);
        world.run_schedule(GameTick);
        assert_eq!(
            world.get_resource::<ServerTick>().map(|t| t.count),
            Some(3),
            "two GameTick runs on the extracted World must advance the counter twice"
        );
    }

    /// The `World` must be `Send` — the whole reason [`ServerApp::into_world`]
    /// exists is that `App` is not, and a Phase 1 that discovers this at the
    /// `tokio::spawn` call site would have to redesign. Compile-time assertion:
    /// it fails to build, not at runtime.
    #[test]
    fn the_extracted_world_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<World>();
    }
}
