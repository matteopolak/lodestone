//! The composed client [`App`] — the one place the client's plugin set is put
//! together, and the one registration point a consumer reaches.
//!
//! # What it is
//!
//! [`client_app`] returns a `bevy_app` [`App`] with the client's version-free,
//! renderer-free plugin set already installed and **nothing finalised**: the
//! caller may still `add_plugins` before anything consumes it. That is the whole
//! point of the crate. Before it existed, `lodestone_shell::sim::Sim::build`
//! called `App::new()` itself, added a fixed tuple, then did
//! `std::mem::take(app.world_mut())` and dropped the `App` — and since
//! `Plugin::build` needs `&mut App`, the plugin set was closed at compile time.
//! `Sim::ecs()` handing out `&mut World` did not help: there is no supported way
//! to merge one `App`'s `Schedules` into another `World`.
//!
//! # How it works
//!
//! ```no_run
//! # use lodestone_physics::{PlayerState, Vec3d};
//! let mut app = lodestone_app::client_app();
//! // ... app.add_plugins(MyPlugin);
//! let session = lodestone_app::spawn_session(
//!     &mut app,
//!     PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0),
//! );
//! // Headless: hand the `World` to `lodestone_client::ClientBuilder::ecs`.
//! // Rendered: hand the whole `App` to `lodestone_shell::sim::Sim::from_app`.
//! ```
//!
//! Two runners consume the result, and **neither of them is privileged**:
//!
//! * headless — `lodestone_client::ClientBuilder::ecs(handle, session)`, which
//!   is what a bot consumer has always been able to do; this crate only removes
//!   the need to hand-assemble the plugin tuple first, so a bot gets the *same*
//!   set the shipped client runs rather than an approximation of it;
//! * rendered — `lodestone_shell::sim::Sim::from_app`, which **adopts** a
//!   pre-built `App` instead of building one. `Sim::client_app()` is
//!   `client_app()` plus the shell's own three plugins, and `Sim::new` is that
//!   plus `from_app`. So the shell registers `CorePlugin` through the identical
//!   function a consumer calls; there is no private composition path left to
//!   drift out of sync (`docs/plugin-api.md`, and
//!   `docs/plans/runtime-plugin-loading.md` §"Milestone zero").
//!
//! # The plugin set, and why each one is here
//!
//! | plugin | owns |
//! |---|---|
//! | [`CorePlugin`](lodestone_ecs::CorePlugin) | the schedules (`GameTick`, `Update`, `Extract`) and their set chains, `FrameClock`, `WorldTime` |
//! | [`LocalPlayerPlugin`](lodestone_ecs::player::LocalPlayerPlugin) | `TickSet::Physics` — the bit-exact movement tick |
//! | [`ControllerPlugin`](lodestone_controller::ControllerPlugin) | `TickSet::Input` and `TickSet::Send` |
//! | [`SessionHudPlugin`](lodestone_ecs::session::SessionHudPlugin) | `TickSet::Animate` — ageing the title/action-bar/effect overlays at 20 Hz |
//! | [`IngestPlugin`](lodestone_ecs::ingest::IngestPlugin) | the net thread's per-entity fold |
//! | [`SessionPlugin`](lodestone_ecs::SessionPlugin) | the net thread's local-player-scalar fold |
//!
//! The last two are the folds `lodestone_client::state::SharedState` runs, and
//! they are installed here because there is one `World` and this composes it.
//! `SessionPlugin` guards the shared `drain_ingest_queue` with
//! `is_plugin_added`, because `add_systems` does not deduplicate and a second
//! copy blanks every batch the first one filled.
//!
//! # How to change it, and the gotcha that matters
//!
//! **Adding a dependency to this crate's manifest is the way to break M0.** The
//! acceptance gate is that a headless consumer depending on `lodestone-app` has
//! no `wgpu` and no `winit` anywhere in `cargo tree`; `tests/renderer_free_graph.rs`
//! asserts it from the manifest side and
//! `tests/headless_consumer_registers_a_plugin.rs` proves a real external plugin
//! reaches its goal through the seam. If you need something render-shaped, it
//! belongs in the shell, above this crate.
//!
//! **Three plugins deliberately did *not* move down here**, and the reason is a
//! finding rather than an omission —
//! `docs/plans/runtime-plugin-loading.md` predicted all three could, on the
//! evidence that none of their files names a `wgpu` type (still true). What
//! stops them is a *shell-internal* entanglement the plan did not check:
//!
//! | plugin | defined in | blocked by |
//! |---|---|---|
//! | `TerrainPlugin` | `lodestone-shell/src/mesher.rs` | `crate::blocks::{ShellClassifier, id}`, `crate::net::NetClient` |
//! | `InteractPlugin` | `lodestone-shell/src/interact.rs` | fourteen items imported from `crate::sim` — a cycle with the type that would adopt the `App` |
//! | `EntityInterpPlugin` | `lodestone-shell/src/entities.rs` | nothing in code (its `crate::sim` references are all prose) — it is movable, but on its own it moves 4,700 lines for no gate |
//!
//! So the split is: the six version-free plugins above compose here; the three
//! shell plugins are added by [`Sim::client_app`] on top of this crate's result,
//! through the same `add_plugins` call a consumer makes. A headless consumer
//! gets no terrain mesher, no pick target and no render-side interpolation —
//! which is correct, because all three exist to feed a renderer.
//!
//! # Dependencies
//!
//! [`lodestone_ecs`] (the `App`, five plugins, the spawn helpers),
//! [`lodestone_controller`] (the sixth), `lodestone-physics` ([`PlayerState`]),
//! `bevy_ecs` for [`Entity`] in [`spawn_session`]'s signature. Nothing else, on
//! purpose.

use bevy_ecs::entity::Entity;
use lodestone_physics::PlayerState;

pub use lodestone_ecs::EcsHandle;
pub use lodestone_ecs::app::App;

/// The client's plugin set, composed into a fresh [`App`] that is **not**
/// finalised — add your own plugins to the result.
///
/// Version-free and renderer-free: nothing here names a protocol family or a
/// GPU type. See this module's docs for the six plugins and for the three that
/// stay in the shell.
///
/// This installs plugins only. Session-scoped *resources* (the chunk store, the
/// mesh worker pool, the particle sprite table, the version adapter) are not
/// here, because each has to be built against something the composer does not
/// know — the block-id space of the world this session will hold, or the
/// configured protocol. Resources need no `Plugin::build`, so a runner inserts
/// them after adoption with only the `World` in hand.
#[must_use]
pub fn client_app() -> App {
    let mut app = App::new();
    app.add_plugins((
        lodestone_ecs::CorePlugin,
        lodestone_ecs::player::LocalPlayerPlugin,
        lodestone_controller::ControllerPlugin,
        lodestone_ecs::session::SessionHudPlugin,
        lodestone_ecs::ingest::IngestPlugin,
        lodestone_ecs::SessionPlugin,
    ));
    app
}

/// Spawn the one entity the whole client hangs off — local player, HUD and
/// session component sets — and return it.
///
/// This is the `session` argument `lodestone_client::ClientBuilder::ecs` wants,
/// and it is three separate inserts rather than one because the component sets
/// belong to three different plugins: a harness that leaves a plugin out must
/// not be left holding components no system looks at.
///
/// Call it *after* every `add_plugins`, for the same reason `Sim` does: a plugin
/// may install a resource a spawn hook reads.
pub fn spawn_session(app: &mut App, player: PlayerState) -> Entity {
    let world = app.world_mut();
    let entity = lodestone_ecs::player::spawn_local_player(world, player);
    lodestone_ecs::session::insert_hud_components(world, entity);
    lodestone_ecs::session::insert_session_components(world, entity);
    entity
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plugin set is actually installed — a `GameTick` run on a fresh
    /// `client_app()` must not panic on a missing schedule or resource, which is
    /// the failure shape a half-composed `App` produces.
    #[test]
    fn a_fresh_client_app_ticks() {
        let mut app = client_app();
        let entity = spawn_session(
            &mut app,
            PlayerState::at(lodestone_physics::Vec3d::new(0.5, 1.0, 0.5), 0.0),
        );
        for _ in 0..5 {
            app.world_mut().run_schedule(lodestone_ecs::GameTick);
        }
        assert!(
            app.world()
                .get::<lodestone_ecs::player::PhysicsState>(entity)
                .is_some(),
            "the session entity must carry the local player's physics state"
        );
    }

    /// The seam itself: a plugin added *after* `client_app` returns is really
    /// built. `is_plugin_added` is the interrogation a `Vec<Box<dyn Plugin>>`
    /// constructor argument could not offer, which is why this crate hands out
    /// the `App` instead.
    #[test]
    fn a_plugin_added_after_composition_is_registered() {
        struct Marker;
        #[derive(bevy_ecs::resource::Resource)]
        struct Built;
        impl lodestone_ecs::app::Plugin for Marker {
            fn build(&self, app: &mut App) {
                app.insert_resource(Built);
            }
        }

        let mut app = client_app();
        assert!(
            !app.is_plugin_added::<Marker>(),
            "control: the marker plugin must not already be present"
        );
        app.add_plugins(Marker);
        assert!(app.is_plugin_added::<Marker>());
        assert!(
            app.world().get_resource::<Built>().is_some(),
            "`Plugin::build` must have run against the composed `App`"
        );
    }
}
