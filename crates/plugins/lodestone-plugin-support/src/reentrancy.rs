//! A reusable reentrancy-deadlock test harness for third-party plugin authors —
//! issue #179.
//!
//! # What it is
//!
//! `lodestone_ecs::EcsHandle` (`Arc<parking_lot::RwLock<World>>`) is not
//! reentrant: a `write()` guard held while something reachable from inside it
//! takes a second guard on the *same* handle deadlocks silently, with no panic
//! and no log line. That shipped once in production —
//! `crates/lodestone-shell/tests/mining_deadlock.rs` pins the exact historical
//! shape — and `hold_read`/`hold_write` (`lodestone_ecs::handle`) now panic
//! instead of hanging **when both guards go through them**, but that backstop
//! cannot see a guard taken the raw way, `handle.read()`/`.write()` directly.
//! `docs/plugin-api.md`'s "Settled: `EcsHandle` reentrancy is unrepresentable
//! for the sanctioned plugin surface" section closed most of this by
//! *omission* — a plugin depending only on `lodestone-ecs` is never handed an
//! `EcsHandle` at all — but the escape hatch it names (a plugin depending
//! directly on `lodestone-shell`, e.g. to obtain a real `EcsHandle` from
//! `Sim::ecs()`) still needs the runtime check, and a third-party author has no
//! way to know any of this without reading `docs/world-unification.md` end to
//! end. This module is that check, extracted so a plugin author never has to
//! understand `parking_lot`'s guard semantics to run it.
//!
//! Two independent halves, matching the two ways the doctrine closes this gap:
//!
//! - [`assert_ecs_only_dependency_graph`] — the static check, for the common
//!   case: a plugin on the sanctioned `lodestone-ecs`-only surface has no route
//!   to an `EcsHandle`, so the deadlock is unrepresentable and a manifest scan
//!   is enough to certify it.
//! - [`assert_schedule_completes_under_write_guard`] /
//!   [`assert_plugin_is_reentrancy_safe`] — the runtime watchdog, for a plugin
//!   that has opted into the escape hatch (or that calls a host convenience
//!   function which has), generalising `mining_deadlock.rs`'s
//!   `within_budget(move || hold_write(&ecs, |world| world.run_schedule(...)))`
//!   away from that file's dig-specific harness.
//!
//! # How it works
//!
//! The runtime half wraps one tick of a caller-chosen schedule in
//! [`within_budget`] — a spawned thread joined through a bounded channel with a
//! timeout — the same shape `mining_deadlock.rs` uses and for the same reason:
//! a wedged thread is **leaked on purpose**, because joining it is the one
//! thing that would turn this harness into the hang it exists to detect.
//! [`ReentrancyFailure::Wedged`] and [`ReentrancyFailure::Panicked`] are kept
//! distinct for the same reason that file keeps `RecvTimeoutError::Timeout`
//! and `::Disconnected` apart: a harness setup mistake (a missing resource) and
//! a real deadlock look identical from "did not return `Ok`" and must not be
//! reported as the same failure.
//!
//! # How to change it
//!
//! If `lodestone_ecs`'s reentrancy ledger (`hold_read`/`hold_write`'s panic
//! path) is ever extended to intercept raw `handle.read()`/`.write()` calls too
//! (closing issue #20), [`ReentrancyFailure::Wedged`] becomes unreachable for
//! any caller going through this module — which would be worth a follow-up
//! doc note here rather than deleting the watchdog outright, since a plugin
//! could still capture a handle and call `.read()` from off any tracked
//! thread.
//!
//! # Configuration
//!
//! [`DEFAULT_WEDGE_TIMEOUT`] — three orders of magnitude above a real tick's
//! cost, matching `mining_deadlock.rs`'s own constant, so the *gate* half
//! cannot flake while the *control* half stays affordable to wait out.
//!
//! # Dependencies
//!
//! `lodestone-ecs` only (`EcsHandle`, `hold_write`, `bevy_app`/`bevy_ecs`
//! re-exports) — this module is itself on the sanctioned surface it partly
//! exists to verify other plugins are on.
//!
//! Not compiled for `wasm32`: a browser has no real threads, `std::thread::spawn`
//! traps there (`CLAUDE.md`'s wasm-hazard census), and this is a development-time
//! tool a plugin author runs on their own machine before shipping, never code
//! that ships inside a running client.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::Duration;

use bevy_ecs::schedule::ScheduleLabel;
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::{EcsHandle, hold_write};

/// Three orders of magnitude above a real tick's cost — see this module's
/// "Configuration" doc.
pub const DEFAULT_WEDGE_TIMEOUT: Duration = Duration::from_secs(3);

/// Why a schedule tick did not complete successfully under
/// [`assert_schedule_completes_under_write_guard`].
#[derive(Debug)]
pub enum ReentrancyFailure {
    /// The tick did not return within the budget: something reachable from
    /// inside it took a second guard on the same [`EcsHandle`] — the
    /// mining-deadlock class of bug.
    Wedged,
    /// The tick panicked instead of completing or wedging. Almost always a
    /// resource or component the schedule needs that the caller's harness
    /// never inserted — **not** the deadlock. Kept distinct from [`Self::Wedged`]
    /// so a setup mistake does not read as "still reentrant" (the same
    /// reasoning `mining_deadlock.rs` gives for keeping
    /// `RecvTimeoutError::Disconnected` apart from `::Timeout`).
    Panicked,
}

impl std::fmt::Display for ReentrancyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wedged => f.write_str("wedged (timed out)"),
            Self::Panicked => f.write_str("panicked"),
        }
    }
}

/// Run `f` on a fresh thread and wait at most `timeout` for it to return.
///
/// A wedged thread is **leaked on purpose**: joining it is the one thing that
/// would turn this function into the hang it exists to detect. See
/// `crates/lodestone-shell/tests/mining_deadlock.rs`'s `within_budget`, which
/// this is a direct, engine-independent generalisation of.
pub fn within_budget<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, RecvTimeoutError> {
    let (tx, rx) = sync_channel(1);
    std::thread::spawn(move || {
        let value = f();
        // A full channel cannot happen (capacity 1, one send); a disconnected
        // one means the caller already gave up, which is not this thread's
        // problem.
        let _ = tx.send(value);
    });
    rx.recv_timeout(timeout)
}

/// Wrap `app`'s `World` as an [`EcsHandle`] the way a real driver would hold
/// it — taking the `World` by value out of `app` (via `World`'s `Default`,
/// leaving `app` with an empty one behind) rather than merely borrowing it,
/// because the class of bug this module exists to catch is specifically about
/// what happens when *the handle itself* — not a bare `&mut World` — is held
/// across a call.
///
/// `app` is left usable but empty after this call; build everything onto it
/// first.
///
/// Goes through [`lodestone_ecs::new_handle`] plus one short [`hold_write`]
/// swap rather than constructing `Arc<RwLock<_>>` directly, so this crate does
/// not need `parking_lot` as a dependency in its own right — `EcsHandle`'s
/// backing lock type is an implementation detail this module has no other
/// reason to name.
#[must_use]
pub fn handle_from_app(app: &mut App) -> EcsHandle {
    let handle = lodestone_ecs::new_handle();
    hold_write(&handle, |world| {
        *world = std::mem::take(app.world_mut());
    });
    handle
}

/// Run `schedule` once against `handle`'s `World`, inside a real
/// [`hold_write`] guard — the same guard shape the client and server drivers
/// hold for a whole tick — under a [`within_budget`] watchdog.
///
/// This is `mining_deadlock.rs`'s
/// `within_budget(move || hold_write(&ecs, |world| world.run_schedule(GameTick)))`,
/// generalised away from that file's dig-specific harness: any plugin's
/// systems, on any [`ScheduleLabel`], exercised the identical way production
/// runs them.
///
/// # What this catches, and what it does not
///
/// `hold_read`/`hold_write` already panic if a second guard is requested
/// *through them* on the same thread while the first is held — a fast, precise
/// diagnosis when it fires, and this function will observe it as
/// [`ReentrancyFailure::Panicked`]. It does **not** see a guard taken the raw
/// way, `handle.read()`/`.write()` directly — and a plugin author reaching for
/// a lower-level API, or calling a host convenience function that does, gets
/// exactly the silent hang `docs/world-unification.md` documents. This
/// function's [`ReentrancyFailure::Wedged`] is the backstop for that gap: a
/// bypassed reentrant call still wedges the watchdog thread, which turns "the
/// test process never returns" into a failed assertion instead of a hung CI
/// job.
///
/// # Errors
///
/// See [`ReentrancyFailure`].
pub fn assert_schedule_completes_under_write_guard<L>(
    handle: &EcsHandle,
    schedule: L,
    timeout: Duration,
) -> Result<(), ReentrancyFailure>
where
    L: ScheduleLabel + Clone,
{
    let handle = Arc::clone(handle);
    match within_budget(timeout, move || {
        hold_write(&handle, |world| {
            world.run_schedule(schedule);
        });
    }) {
        Ok(()) => Ok(()),
        Err(RecvTimeoutError::Timeout) => Err(ReentrancyFailure::Wedged),
        Err(RecvTimeoutError::Disconnected) => Err(ReentrancyFailure::Panicked),
    }
}

/// The near-zero-authoring-cost entry point: build a fresh [`App`] with
/// [`lodestone_ecs::CorePlugin`] (so `schedule` is a registered, runnable label
/// even if `plugin` adds no systems to it) plus `plugin`, and assert one tick
/// of `schedule` completes within [`DEFAULT_WEDGE_TIMEOUT`].
///
/// A plugin author who wants "does my plugin deadlock the way the mining bug
/// did" gets it in one call, with no `EcsHandle`/`hold_write` knowledge
/// required. For anything needing extra setup (spawned entities, seeded
/// resources) beyond what `plugin` itself inserts, build the `App` by hand and
/// call [`assert_schedule_completes_under_write_guard`] directly instead — this
/// wrapper only carries the two plugins mentioned above.
///
/// **Cannot exercise a plugin whose reentrancy hazard depends on a
/// self-referencing `EcsHandle` resource** (a system that reads back the very
/// handle its own tick is running under, the exact shape
/// `docs/plugin-api.md`'s "Settled" section calls out): `Plugin::build` runs
/// before [`handle_from_app`] produces a handle to hand back, so there is no
/// point in this call's sequence to insert one. Build the `App`, obtain the
/// handle, insert the self-reference, *then* call
/// [`assert_schedule_completes_under_write_guard`] directly for that shape —
/// `crates/plugins/lodestone-plugin-support/tests/reentrancy_harness.rs`'s
/// `reentrant_handle` is the worked example.
///
/// # Panics
///
/// If the tick wedges or panics — see [`assert_schedule_completes_under_write_guard`]'s
/// two [`ReentrancyFailure`] variants, reported with different messages so the
/// two causes are not conflated.
pub fn assert_plugin_is_reentrancy_safe<P, L>(plugin: P, schedule: L)
where
    P: Plugin,
    L: ScheduleLabel + Clone,
{
    let mut app = App::new();
    app.add_plugins((lodestone_ecs::CorePlugin, plugin));
    let handle = handle_from_app(&mut app);
    if let Err(failure) =
        assert_schedule_completes_under_write_guard(&handle, schedule, DEFAULT_WEDGE_TIMEOUT)
    {
        match failure {
            ReentrancyFailure::Wedged => panic!(
                "a schedule tick did not return within {DEFAULT_WEDGE_TIMEOUT:?}: a system in \
                 this plugin (or a host function it called) took a second guard on the same \
                 EcsHandle while a write guard shaped like the real driver's was already held. \
                 This is the mining-deadlock class of bug — see \
                 docs/plugin-api.md's \"Settled: EcsHandle reentrancy is unrepresentable\" \
                 section and crates/lodestone-shell/tests/mining_deadlock.rs for the original \
                 incident. If this plugin does not knowingly hold an EcsHandle, check any host \
                 convenience function it calls for one."
            ),
            ReentrancyFailure::Panicked => panic!(
                "a schedule tick panicked instead of completing or wedging — this is very \
                 likely a resource or component this plugin's systems need that a bare \
                 lodestone_ecs::CorePlugin plus this plugin does not provide (a LocalPlayer \
                 entity, world content, or a resource normally installed by the shell or \
                 server). See the panic output above for what was missing; this is not the \
                 deadlock. If it names EcsHandle's own reentrancy ledger, that IS the \
                 deadlock, reported as a panic rather than a hang because both guards went \
                 through hold_read/hold_write."
            ),
        }
    }
}

/// The static half: certify that `manifest_path` (a plugin crate's
/// `Cargo.toml`) names no crate that can hand a system a real [`EcsHandle`].
///
/// `docs/plugin-api.md`'s "Settled" section identifies exactly two:
/// `lodestone-shell` (`Sim::ecs()`) and `lodestone-client` (`ClientHandle`'s
/// backing handle). A plugin depending on neither has, by construction, no
/// route to an `EcsHandle` at all — the deadlock class this module exists to
/// catch cannot be *expressed*, which is a strictly stronger guarantee than
/// the runtime watchdog above (nothing to run, nothing that can flake) and is
/// the doc's own suggested check for the common case: "a grep of the plugin's
/// own manifest".
///
/// This is a text scan, not a `Cargo.toml` parse, deliberately: reading a
/// plugin author's manifest should not need this crate to carry a TOML parser
/// dependency, and a crate *name* appearing anywhere in a `[dependencies]`-shaped
/// file is exactly what determines whether `cargo` lets the plugin name the
/// type — independent of inline-table, `dependencies.foo`, or workspace-alias
/// syntax variations a real parse would still have to special-case.
///
/// # Panics
///
/// If `manifest_path` cannot be read, or if it names one of the two forbidden
/// crates (as either the hyphenated crate name or its Rust identifier form).
pub fn assert_ecs_only_dependency_graph(manifest_path: &Path) {
    let text = std::fs::read_to_string(manifest_path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", manifest_path.display()));
    for forbidden in [
        "lodestone-shell",
        "lodestone_shell",
        "lodestone-client",
        "lodestone_client",
    ] {
        assert!(
            !text.contains(forbidden),
            "{} names `{forbidden}` — a plugin on the sanctioned `lodestone-ecs`-only surface \
             must not depend on it, because it is one of the two routes to a real EcsHandle \
             (see docs/plugin-api.md's \"Settled: EcsHandle reentrancy is unrepresentable\" \
             section). If this dependency is deliberate — the explicit, version-locking escape \
             hatch that section names — this plugin is no longer on the surface this check \
             certifies, and every system it registers should instead be exercised with \
             assert_schedule_completes_under_write_guard.",
            manifest_path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The control for [`assert_ecs_only_dependency_graph`] itself: a manifest
    /// that *does* name the escape-hatch crate must be refused, proving the
    /// scan is not vacuously permissive.
    #[test]
    #[should_panic(expected = "lodestone-shell")]
    fn a_manifest_naming_the_escape_hatch_crate_is_refused() {
        let dir = std::env::temp_dir().join(format!(
            "lodestone-reentrancy-manifest-check-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let manifest = dir.join("Cargo.toml");
        std::fs::write(
            &manifest,
            "[package]\nname = \"toy\"\n\n[dependencies]\nlodestone-ecs = { workspace = true }\nlodestone-shell = { workspace = true }\n",
        )
        .expect("write scratch manifest");
        assert_ecs_only_dependency_graph(&manifest);
    }

    /// The gate: a manifest naming only the sanctioned crate passes.
    #[test]
    fn a_manifest_naming_only_lodestone_ecs_passes() {
        let dir = std::env::temp_dir().join(format!(
            "lodestone-reentrancy-manifest-check-ok-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let manifest = dir.join("Cargo.toml");
        std::fs::write(
            &manifest,
            "[package]\nname = \"toy\"\n\n[dependencies]\nlodestone-ecs = { workspace = true }\n",
        )
        .expect("write scratch manifest");
        assert_ecs_only_dependency_graph(&manifest);
    }
}
