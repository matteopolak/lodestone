//! [`ServerCorePlugin`] — the one plugin every server `World` installs, and
//! [`advance_server_tick`], the one system Phase 0 ships.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bevy_app::{App, Plugin};
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::{Res, ResMut};

use crate::ecs::schedules::{GameTick, IngestSet, NetIngest, ServerBoot, TickSet};

/// How many schedule runs have executed [`advance_server_tick`] in this
/// server's own `World`.
///
/// # Why a counter is the right Phase 0 payload
///
/// It is the cheapest piece of state that is *observably wrong when nothing
/// runs*. A resource that is merely present proves construction; a resource
/// that has been **incremented** proves a schedule ran a registered system
/// against the production-built `World`, which is the exact property
/// `WindowApp.ecs` (an inert scaffold nothing reads) lacks. Phase 1 keeps
/// incrementing it from `run_tick_loop` and gains a second, independent
/// witness: this count must then advance in lockstep with
/// [`crate::TickStats::tick_count`], and any divergence is the island
/// detector.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ServerTick {
    /// Monotonic; never reset. One increment per [`advance_server_tick`] run.
    pub count: u64,
}

/// A read-only mirror of [`ServerTick::count`] that a caller **outside** the
/// server's `World` can observe.
///
/// # Why this exists at all, given the `Resource` above already counts
///
/// The `World` is owned outright by the tick task and has **no lock** — per
/// `docs/server-ecs.md`, nothing outside that task may read it, and that is the
/// design working as intended rather than a gap. It is also exactly what makes
/// "did a system actually run in production?" unobservable from a caller
/// holding only an [`crate::IntegratedServer`], which is the one question Phase
/// 0 has to be able to answer (`WindowApp.ecs` is the client-side example of what
/// happens when nobody can).
///
/// So this is a deliberately one-way valve: an `Arc<AtomicU64>` the system
/// writes and a holder can only *read*, carrying a monotonic count and nothing
/// else. It is a **metric, not a back door** — no simulation state travels
/// through it, nothing branches on it, and it hands out no path into the
/// `World`. `crate::IntegratedServer::server_tick_count` is the production
/// consumer.
#[derive(Resource, Debug, Clone, Default)]
pub struct ServerTickWitness(Arc<AtomicU64>);

impl ServerTickWitness {
    /// The count as of now. Monotonic, so a caller may compare two readings.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// The one system Phase 0 registers: bump [`ServerTick`] inside the `World`,
/// and mirror it onto [`ServerTickWitness`] so someone outside can tell that it
/// happened.
///
/// `Res`, not `ResMut`, for the witness: it is an interior-mutable counter, and
/// taking it immutably is what keeps a second registered system from being an
/// ambiguity against this one on that resource. `ServerTick` is the `ResMut`,
/// and it is the resource
/// `plugin::tests::a_second_unordered_server_tick_writer_fails_the_ambiguity_check`
/// uses to prove strict ambiguity detection is switched on.
pub fn advance_server_tick(mut tick: ResMut<ServerTick>, witness: Res<ServerTickWitness>) {
    tick.count = tick.count.saturating_add(1);
    witness.0.fetch_add(1, Ordering::Relaxed);
}

/// Installs the server `World`'s schedules, their set chains, and
/// [`ServerTick`]. An ordinary `bevy_app` plugin — `docs/server-ecs.md`'s
/// standing rule is that core game systems are themselves plugins, so this is
/// deliberately nothing a third party could not have written.
///
/// # Never install `lodestone_ecs::CorePlugin` on a server `App`
///
/// It inserts three resources, and all three are wrong here for different
/// reasons:
///
/// | resource | verdict |
/// |---|---|
/// | `WorldTime` | reusable in principle — but it arrives welded to the other two |
/// | `FrameClock` | a lie: there is no frame, and open-to-LAN has no render loop at all |
/// | `LockHolds` | **worse than a lie** — it is the meter for a lock the server does not have, so a reading of zero would look like a measurement |
///
/// `CorePlugin` also chains `FrameSet::{Input, Interpolate, Camera, Terrain}`
/// into `Update`, and configures an `Extract` schedule. `Update` does not exist
/// on an [`App::empty`]-built server `App`, and `configure_sets` would
/// helpfully *create* it — so installing `CorePlugin` would not fail loudly, it
/// would quietly grow a frame-shaped schedule inside the server. That is the
/// failure mode this plugin exists to make impossible.
#[derive(Debug, Default, Clone, Copy)]
pub struct ServerCorePlugin;

impl Plugin for ServerCorePlugin {
    fn build(&self, app: &mut App) {
        // `init_resource`, not `insert_resource`: re-running `build`, or an
        // owner that seeded a counter before adding the plugin, must not zero a
        // live one. Same reasoning `lodestone_ecs::CorePlugin` records for its
        // own clocks.
        app.init_resource::<ServerTick>();
        app.init_resource::<ServerTickWitness>();
        app.init_resource::<super::ServerTaskScheduler>();

        app.init_schedule(ServerBoot);
        app.add_systems(ServerBoot, advance_server_tick);

        app.init_schedule(NetIngest);
        app.configure_sets(
            NetIngest,
            (IngestSet::Drain, IngestSet::Apply, IngestSet::Index).chain(),
        );

        app.init_schedule(GameTick);
        app.configure_sets(
            GameTick,
            (
                TickSet::Drain,
                TickSet::Adjudicate,
                TickSet::Apply,
                TickSet::Simulate,
                TickSet::Publish,
            )
                .chain(),
        );
        // Registered in `Simulate` rather than `Publish` so Phase 1's
        // lockstep assertion (`ServerTick::count` versus
        // `TickStats::tick_count`) is counting the same thing the rest of the
        // tick body will live in.
        app.add_systems(GameTick, advance_server_tick.in_set(TickSet::Simulate));
        app.add_systems(GameTick, super::run_server_tasks.in_set(TickSet::Drain));
        // The server never runs Bevy's frame schedules. Age every registered
        // plugin message before any gameplay reader or scheduled callback runs.
        app.add_systems(
            GameTick,
            bevy_ecs::message::message_update_system.before(TickSet::Drain),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ecs::ServerApp;

    /// The server `App` must not carry a frame-shaped schedule. This is the
    /// `contains_resource::<FrameClock>() == false` gate
    /// `docs/plans/server-ecs-migration.md` Phase 0 asks for, expressed against
    /// what Phase 0 actually links: `FrameClock` is a `lodestone-ecs` type and
    /// this crate deliberately does not depend on that crate (see
    /// `schedules.rs`'s own doc for why), so the checkable property is the one
    /// that would *carry* a frame clock — `Update`, which only exists if
    /// something installed `MainSchedulePlugin` or configured sets into it.
    #[test]
    fn the_server_app_has_no_frame_shaped_schedule() {
        let server = ServerApp::bootstrap();
        assert!(
            server.app().get_schedule(bevy_app::Update).is_none(),
            "the server App must have no `Update` schedule — see ServerCorePlugin's doc"
        );
        assert!(
            server.app().get_schedule(bevy_app::Main).is_none(),
            "the server App must have no `Main` schedule; App::empty() is the constructor"
        );
    }

    /// Negative control for the test above: both halves of that gate must be
    /// reachable, or it proves nothing about `ServerCorePlugin` and only
    /// something about `App::empty()`.
    ///
    /// # A false premise, caught by running the control
    ///
    /// This test first asserted that installing `MainSchedulePlugin` creates
    /// `Update`. **It does not** — measured, not assumed:
    /// `bevy_app`'s `main_schedule.rs` (in `MainSchedulePlugin`'s plugin build)
    /// adds `Main`, `FixedMain` and
    /// `RunFixedMainLoop` and never touches `Update`, which `App::default()`
    /// gets only because its own `add_systems`/`configure_sets` calls create it
    /// on demand. So the control failed with "MainSchedulePlugin did not create
    /// `Update`" — the CLAUDE.md hazard where a control's premise is false
    /// before the subject ever existed, and the reason the rule is to *run* a
    /// control rather than describe it.
    ///
    /// Both halves are therefore driven by what actually creates each schedule:
    /// `MainSchedulePlugin` for `Main`, and a `configure_sets(Update, …)` call
    /// for `Update` — which is precisely the shape `lodestone_ecs::CorePlugin`
    /// uses, and precisely why installing it on a server `App` would grow a
    /// frame-shaped schedule instead of failing loudly.
    #[test]
    fn both_halves_of_the_frame_shape_gate_are_reachable() {
        let mut main_app = App::empty();
        main_app.add_plugins(bevy_app::MainSchedulePlugin);
        main_app.add_plugins(ServerCorePlugin);
        main_app.finish();
        main_app.cleanup();
        assert!(
            main_app.get_schedule(bevy_app::Main).is_some(),
            "control failed: MainSchedulePlugin did not create `Main`, so the `Main` half of \
             the gate proves nothing"
        );

        let mut frame_app = App::empty();
        frame_app.add_plugins(ServerCorePlugin);
        // The `CorePlugin` shape, verbatim in structure: configuring sets into
        // `Update` *creates* `Update` rather than erroring on its absence.
        frame_app.configure_sets(
            bevy_app::Update,
            (IngestSet::Drain, IngestSet::Apply, IngestSet::Index).chain(),
        );
        frame_app.finish();
        frame_app.cleanup();
        assert!(
            frame_app.get_schedule(bevy_app::Update).is_some(),
            "control failed: configure_sets(Update, ..) did not create `Update`, so the \
             `Update` half of the gate proves nothing"
        );
    }

    /// The three schedules exist and their set chains build. Promoting
    /// `ambiguity_detection` to `Error` is what makes this more than a
    /// smoke test: an unordered conflicting pair inside a set becomes a
    /// schedule *build* error rather than a nondeterministic runtime order.
    ///
    /// The recorded gotcha from
    /// `lodestone_controller::ecs::exactly_one_system_writes_movement_intent`,
    /// copied verbatim because it is easy to reintroduce: do **not** run the
    /// schedule first. An already-built schedule is not rebuilt, so
    /// `initialize` returns `Ok` without ever consulting the new settings —
    /// which is exactly how this assertion would go vacuous.
    #[test]
    fn every_server_schedule_initializes_under_strict_ambiguity_detection() {
        use bevy_ecs::schedule::{LogLevel, ScheduleBuildSettings, ScheduleLabel};

        // Built here rather than through `ServerApp::bootstrap`, which runs
        // `ServerBoot` — see the gotcha above.
        let mut app = App::empty();
        app.add_plugins(ServerCorePlugin);
        app.finish();
        app.cleanup();

        for label in [
            ServerBoot.intern(),
            NetIngest.intern(),
            GameTick.intern(),
        ] {
            // `.err()` rather than the whole `Result`: the `Ok` half carries a
            // `ScheduleBuildMetadata`, which has no `Debug`, so formatting the
            // `Result` does not compile. The error alone is what a failure
            // needs to name anyway.
            let failure = app
                .world_mut()
                .schedule_scope(label, |world, schedule| {
                    schedule.set_build_settings(ScheduleBuildSettings {
                        ambiguity_detection: LogLevel::Error,
                        ..ScheduleBuildSettings::default()
                    });
                    schedule.initialize(world)
                })
                .err();
            assert!(
                failure.is_none(),
                "schedule {label:?} failed to build under strict ambiguity detection: {failure:?}"
            );
        }
    }

    /// Negative control for the test above: a second, unordered writer of
    /// `ServerTick` in the same set must be reported, proving the detector is
    /// switched on rather than the assertion passing against a no-op checker.
    #[test]
    fn a_second_unordered_server_tick_writer_fails_the_ambiguity_check() {
        use bevy_ecs::schedule::{LogLevel, ScheduleBuildSettings};

        fn rogue(mut tick: ResMut<ServerTick>) {
            tick.count = tick.count.wrapping_add(1);
        }

        let mut app = App::empty();
        app.add_plugins(ServerCorePlugin);
        app.add_systems(GameTick, rogue.in_set(TickSet::Simulate));
        app.finish();
        app.cleanup();

        let result = app.world_mut().schedule_scope(GameTick, |world, schedule| {
            schedule.set_build_settings(ScheduleBuildSettings {
                ambiguity_detection: LogLevel::Error,
                ..ScheduleBuildSettings::default()
            });
            schedule.initialize(world)
        });
        assert!(
            result.is_err(),
            "control failed: a second unordered ServerTick writer was not reported"
        );
    }

    /// `bootstrap` runs `ServerBoot` exactly once, so the counter is 1 — not 0
    /// (nothing ran) and not 2 (the schedule ran twice). A predicted value, not
    /// a direction.
    #[test]
    fn bootstrap_runs_the_boot_schedule_exactly_once() {
        let server = ServerApp::bootstrap();
        assert_eq!(
            server.tick_count(),
            1,
            "ServerBoot must run exactly once during bootstrap"
        );
    }

    /// [`ServerTickWitness`] tracks [`ServerTick`] exactly, which is what makes
    /// it usable as the production gate's evidence: boot (1) plus two
    /// `GameTick` runs (3), predicted rather than observed-then-recorded.
    ///
    /// The witness is per-`World`, not per-process, so this is an absolute
    /// assertion and not a delta — there is no other test it can race with.
    /// That is the whole reason it is an `Arc<AtomicU64>` handed out per
    /// bootstrap rather than a `static`.
    #[test]
    fn the_witness_tracks_the_in_world_counter_exactly() {
        let mut server = ServerApp::bootstrap();
        assert_eq!(server.witness().count(), 1, "boot must be witnessed");
        server.run_game_tick();
        server.run_game_tick();
        assert_eq!(server.tick_count(), 3, "in-World counter after boot + 2 ticks");
        assert_eq!(
            server.witness().count(),
            3,
            "the witness must not drift from the in-World counter"
        );
    }

    /// Negative control for the test above: a `World` built **without**
    /// [`ServerCorePlugin`] has no registered system, so running the same
    /// schedule label must leave the witness at zero. Proves the witness
    /// reports a system *executing*, not merely a schedule being run.
    #[test]
    fn a_world_without_the_plugin_leaves_the_witness_at_zero() {
        let mut app = App::empty();
        app.init_resource::<ServerTickWitness>();
        app.init_schedule(ServerBoot);
        app.init_schedule(GameTick);
        app.finish();
        app.cleanup();
        let witness = app.world().resource::<ServerTickWitness>().clone();
        app.world_mut().run_schedule(ServerBoot);
        app.world_mut().run_schedule(GameTick);
        assert_eq!(
            witness.count(),
            0,
            "control failed: the witness moved with no system registered, so it is not \
             evidence that anything ran"
        );
    }
}
