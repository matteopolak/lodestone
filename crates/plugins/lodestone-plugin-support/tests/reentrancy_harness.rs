//! The control for `src/reentrancy.rs`'s runtime watchdog: without this file,
//! [`assert_schedule_completes_under_write_guard`] passing for a well-behaved
//! plugin proves nothing, per `CLAUDE.md`'s "assertions of an absence need a
//! control proving the detector works" — a harness that always returns `Ok`
//! would look identical from the outside.
//!
//! So this builds a genuinely reentrant toy plugin — a system that captures an
//! [`EcsHandle`] as a resource and takes a **raw** `handle.read()` on it, the
//! exact bypass class `src/reentrancy.rs`'s doc names as the gap
//! `hold_read`/`hold_write`'s own panic-based ledger cannot see — and asserts
//! the harness reports [`ReentrancyFailure::Wedged`] for it. A benign sibling
//! plugin, using only `Query`/`Res`/`ResMut` the ordinary way, is the paired
//! gate proving the harness does not false-positive on ordinary systems.
//!
//! Note this reentrant shape is exactly what
//! `docs/plugin-api.md`'s "Settled: EcsHandle reentrancy is unrepresentable"
//! section says a plugin on the sanctioned `lodestone-ecs`-only surface has no
//! route to construct in the first place — nothing stops a *test* from wiring
//! it up deliberately, which is the whole point here.

use std::time::Duration;

use bevy_ecs::resource::Resource;
use bevy_ecs::system::{Query, Res, ResMut};
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::entity::MinecraftEntityId;
use lodestone_ecs::{EcsHandle, GameTick};
use lodestone_plugin_support::reentrancy::{
    ReentrancyFailure, assert_plugin_is_reentrancy_safe, assert_schedule_completes_under_write_guard,
    handle_from_app,
};

/// Short: the control is *expected* to wedge, and a test suite should not pay
/// the default 3s for a timeout it knows is coming.
const CONTROL_TIMEOUT: Duration = Duration::from_millis(300);

/// Carries the very handle its own system is running under — the shape a host
/// convenience function (or a plugin that opted into the `lodestone-shell`
/// escape hatch) could hand a system.
#[derive(Resource, Clone)]
struct SelfHandle(EcsHandle);

/// **THE MISBEHAVING FIXTURE.** Takes a raw read guard on the handle its own
/// system is executing under — bypassing `hold_read` entirely, so the
/// panic-based ledger never sees it. `parking_lot::RwLock` is not reentrant,
/// so this blocks forever: the write guard `assert_schedule_completes_under_write_guard`
/// is holding for the whole tick will never be dropped, because the thread
/// blocked here is the one thread that could drop it.
fn reentrant_system(handle: Res<SelfHandle>) {
    let _guard = handle.0.read();
}

#[derive(Debug, Default)]
struct ReentrantPlugin;

impl Plugin for ReentrantPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(GameTick, reentrant_system);
    }
}

/// **THE GATE'S FIXTURE.** An ordinary system: no handle capture, just the
/// ECS surface every sanctioned plugin actually gets.
#[derive(Resource, Default)]
struct TickCount(u32);

fn count_entities(query: Query<&MinecraftEntityId>, mut count: ResMut<TickCount>) {
    count.0 += query.iter().count() as u32;
}

#[derive(Debug, Default)]
struct BenignPlugin;

impl Plugin for BenignPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TickCount>();
        app.add_systems(GameTick, count_entities);
    }
}

/// Build an `App` with `ReentrantPlugin` plus the self-referential
/// [`SelfHandle`] resource, wrapped as an [`EcsHandle`] with that same handle
/// visible to its own systems.
fn reentrant_handle() -> EcsHandle {
    let mut app = App::new();
    app.add_plugins((lodestone_ecs::CorePlugin, ReentrantPlugin));
    let handle = handle_from_app(&mut app);
    // Insert the handle into the World it wraps — the self-reference a host
    // accessor (e.g. `ClientHandle`) makes for real, and the shape this test
    // exists to reproduce hermetically. A short, ordinary write here; nothing
    // reentrant about *this* line, since it runs before the watchdog's guard.
    lodestone_ecs::hold_write(&handle, |world| {
        world.insert_resource(SelfHandle(handle.clone()));
    });
    handle
}

#[test]
fn a_raw_guard_taken_from_inside_a_held_write_guard_is_reported_as_wedged() {
    let handle = reentrant_handle();
    let outcome =
        assert_schedule_completes_under_write_guard(&handle, GameTick, CONTROL_TIMEOUT);
    assert!(
        matches!(outcome, Err(ReentrancyFailure::Wedged)),
        "a system that takes a raw `handle.read()` on the same EcsHandle its write guard is \
         held under must be reported as Wedged, not {outcome:?} — if this is Ok, the watchdog \
         itself is not detecting the hazard it exists to catch"
    );
}

/// **THE GATE.** The paired benign plugin must pass — proving the control
/// above is not simply "the harness always reports failure".
#[test]
fn a_benign_plugin_completes_its_schedule_normally() {
    let mut app = App::new();
    app.add_plugins((lodestone_ecs::CorePlugin, BenignPlugin));
    app.world_mut().spawn(MinecraftEntityId(1));
    app.world_mut().spawn(MinecraftEntityId(2));
    let handle = handle_from_app(&mut app);

    let outcome =
        assert_schedule_completes_under_write_guard(&handle, GameTick, CONTROL_TIMEOUT);
    assert!(outcome.is_ok(), "a benign plugin's tick must complete: {outcome:?}");

    lodestone_ecs::hold_write(&handle, |world| {
        assert_eq!(
            world.resource::<TickCount>().0,
            2,
            "the tick must actually have run the system, not merely returned without \
             executing it"
        );
    });
}

/// The one-call convenience must not panic for a well-behaved plugin.
#[test]
fn assert_plugin_is_reentrancy_safe_passes_a_benign_plugin() {
    assert_plugin_is_reentrancy_safe(BenignPlugin, GameTick);
}

/// **Not exercisable through the one-call wrapper, and that absence is itself
/// worth pinning.** [`ReentrantPlugin`] only deadlocks because
/// [`reentrant_handle`] inserts `SelfHandle` *after* the `App`/handle both
/// exist — a self-reference `assert_plugin_is_reentrancy_safe` structurally
/// cannot make on a plugin's behalf, because `Plugin::build` runs before
/// [`handle_from_app`] ever produces a handle to hand back. So driving
/// `ReentrantPlugin` through the wrapper does not wedge; `SelfHandle` is
/// simply missing, which is the ordinary "this plugin needs a resource
/// `CorePlugin` does not provide" case the [`ReentrancyFailure::Panicked`]
/// message is written for. This test pins that message rather than asserting
/// something the wrapper cannot actually produce for this fixture; the
/// reentrancy control itself lives in
/// [`a_raw_guard_taken_from_inside_a_held_write_guard_is_reported_as_wedged`],
/// using [`assert_schedule_completes_under_write_guard`] directly with the
/// resource inserted by hand, which is what a plugin author needing this
/// shape should also do.
#[test]
#[should_panic(expected = "not the deadlock")]
fn assert_plugin_is_reentrancy_safe_reports_a_missing_resource_as_panicked_not_wedged() {
    assert_plugin_is_reentrancy_safe(ReentrantPlugin, GameTick);
}
