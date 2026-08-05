//! The core plugin every `App` in the tree installs.

use bevy_app::{App, Plugin, Update};
use bevy_ecs::schedule::IntoScheduleConfigs;

use crate::schedules::{Extract, GameTick, NetIngest};
use crate::sets::{EventPriority, ExtractSet, FrameSet, IngestSet, TickSet};

/// [`EventPriority`]'s chain, in Bukkit's own `LOWEST..MONITOR` order,
/// configured into all four public schedules below by
/// [`CorePlugin::build`] — one call site so the six variants cannot drift
/// into four different orderings of themselves.
macro_rules! event_priority_chain {
    () => {
        (
            EventPriority::Lowest,
            EventPriority::Low,
            EventPriority::Normal,
            EventPriority::High,
            EventPriority::Highest,
            EventPriority::Monitor,
        )
            .chain()
    };
}

/// Registers the Stage-0 schedule/set scaffolding
/// (`docs/bevy-migration.md` §4.2) on an `App`: the three schedules this
/// crate owns (`NetIngest`, `GameTick`, `Extract`), plus the internal
/// ordering of all four schedules' public sets, including bevy's own
/// `Update`.
///
/// # The two clocks it now owns, and why it refused to before
///
/// Until §4.1(c) this plugin deliberately inserted **no** state at all — not
/// even [`crate::WorldTime`] — because there were three bevy `World`s in the
/// process (the net thread's, the entity interpolator's and the driver's) and a
/// plugin that inserted a clock would have given each of them its own silently
/// diverging copy. That guard was doing real work: the *other* clock, the 20 Hz
/// accumulator, escaped it (each `World` had one because
/// `World::run_schedule(GameTick)` runs that `World`'s schedule) and diverged by
/// five ticks per stall, unbounded.
///
/// There is now one `World`, so the guard has nothing left to protect and is
/// retired: [`crate::WorldTime`] (the *server's* clock) and
/// [`crate::FrameClock`] (the *driver's*) are inserted here, once, by the one
/// plugin every `App` in the tree installs. `init_resource`, not
/// `insert_resource`, so re-running `build` — or an owner that seeded a clock
/// before adding the plugin — cannot zero a live clock.
#[derive(Debug, Default)]
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<crate::WorldTime>();
        app.init_resource::<crate::FrameClock>();
        // The guard-hold meter. Inserted here rather than by the driver because
        // `EcsHandle`'s duration bound is a property of *every* holder of the
        // handle, not of one of them — and because a `World` without it is
        // silently unmeasured (`hold_read`/`hold_write` tolerate its absence),
        // which is precisely how a counter stops being evidence.
        app.init_resource::<crate::LockHolds>();

        app.init_schedule(NetIngest);
        app.configure_sets(
            NetIngest,
            (IngestSet::Drain, IngestSet::Apply, IngestSet::Index).chain(),
        );
        // Issue #105: the same `EventPriority` chain, anchored here too, so a
        // plugin's `GameEvent` observer ordered inside `NetIngest` still gets
        // a cross-plugin order against other plugins' observers in the same
        // schedule. See `EventPriority`'s own doc for why this is repeated
        // in all four schedules rather than declared once.
        app.configure_sets(NetIngest, event_priority_chain!());

        app.init_schedule(GameTick);
        app.configure_sets(
            GameTick,
            (
                TickSet::Input,
                TickSet::Intent,
                TickSet::Physics,
                TickSet::Predict,
                TickSet::Animate,
                TickSet::Send,
            )
                .chain(),
        );
        app.configure_sets(GameTick, event_priority_chain!());

        // `Update` already exists (installed by `MainSchedulePlugin` as part
        // of `App::new()`/`App::default()`); `configure_sets` creates it if
        // it does not, so this is safe even against `App::empty()`.
        app.configure_sets(
            Update,
            (
                FrameSet::Input,
                FrameSet::Interpolate,
                FrameSet::Camera,
                FrameSet::Terrain,
            )
                .chain(),
        );
        app.configure_sets(Update, event_priority_chain!());

        app.init_schedule(Extract);
        app.configure_sets(
            Extract,
            (
                ExtractSet::Terrain,
                ExtractSet::Entities,
                ExtractSet::Debug,
                ExtractSet::Hud,
            )
                .chain(),
        );
        app.configure_sets(Extract, event_priority_chain!());
    }
}
