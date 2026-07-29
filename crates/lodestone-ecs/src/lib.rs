//! Lodestone's `bevy_ecs`-backed world/entity/session state — the crate
//! `docs/bevy-migration.md` (§7, Stage 0) introduces so third-party
//! extensions become native Rust plugins with the same power as built-in
//! code, per `DESIGN.md:520-521`.
//!
//! # Stage 0
//!
//! This is the App/schedule scaffold plus one real slice
//! ([`WorldTime`]) migrated authoritatively off
//! `lodestone_client::state::Inner`. Later stages move state here one at a
//! time, each one *deleting* the old owner rather than adding a second reader
//! (the "authority test", §1).
//!
//! # Stage 1
//!
//! [`entity`] holds the entity component set, and [`ingest`] the `NetIngest`
//! systems that fold `ClientEvent`s into it. `Inner`'s
//! `HashMap<i32, EntityView>` and its `apply_metadata` helper are **deleted**;
//! `EntityView` survives only as a value type derived on demand for
//! `ClientHandle::entities()`, which is the plan's one sanctioned intermediate
//! (components authoritative, struct derived — never the reverse).
//!
//! # Stage 2
//!
//! [`player`] holds the local player's component set plus the
//! [`TickSet::Physics`] system that advances it. `lodestone_shell::sim::Sim`'s
//! `player`, `prev_position`, `fluid_state`, `fly`, `input`, `profile`,
//! `selected_slot`, `last_player_input`, `last_sprinting_sent` and `dead`
//! fields are **deleted**; `Sim` reads and writes components through
//! accessors. The input and egress halves of the same tick live in
//! `lodestone_controller::ecs`, which cannot be a dependency of this crate
//! (`lodestone-controller` → `lodestone-client` → here would be a cycle).
//!
//! # Stage 3
//!
//! [`session`] holds the session/HUD component set and the fold that produces
//! it. Its reason for existing is the *double fold*: two different types named
//! `Scoreboard` (and two player-list folds, and an entirely unwired third
//! boss-bar fold) all consuming the same `ClientEvent` stream.
//! `lodestone_client::scoreboard` is **deleted**, `Inner`'s `players` /
//! `scoreboard` / `boss_bars` / `menus` are **deleted**, and
//! `lodestone_shell::sim::Sim`'s `phase`, `chat_log`, `tab_list`, `scoreboard`,
//! `hud_effects`, `title`, `action_bar`, `health`, `food`, `experience`,
//! `respawn_count` and `local_entity_id` are **deleted**. `lodestone-game`'s
//! aggregates are the storage; one system per fold calls them.
//!
//! Still where it was: the chunk world, `Sim` itself.
//!
//! # What this crate depends on
//!
//! `bevy_app` + `bevy_ecs`, `default-features = false, features = ["std"]`
//! (§3): no `multi_threaded` (does not even compile on wasm32 with no
//! threads, §3.1), no `bevy_reflect` (default-on upstream, but truly
//! optional here — verified with `cargo tree -e features`, not assumed; see
//! the Stage 0 report). `parking_lot` for [`EcsHandle`]'s lock, matching
//! azalea's choice for the same purpose. Deliberately **no** dependency on
//! any version crate, ever (§5) — `xtask check-isolation` is expected to
//! enforce that once wired up.
//!
//! Deliberately **not** added to `cargo xtask check-connected`'s allowlist:
//! per the plan, that tool going red for this crate is either "the island
//! detector working" (a stage that left it disconnected) or a green light
//! once a shipped binary root actually depends on it — never something to
//! suppress.

pub mod entity;
mod handle;
pub mod ingest;
pub mod player;
mod plugin;
mod resources;
mod runner;
mod schedules;
pub mod session;
mod sets;

/// Re-exported so plugin authors never need to match `bevy_app`'s version by
/// hand (azalea does the same at `azalea/src/lib.rs:63-64`).
pub use bevy_app as app;
/// Re-exported so plugin authors never need to match `bevy_ecs`'s version by
/// hand.
pub use bevy_ecs as ecs;

pub use handle::{EcsHandle, new_handle, new_ingest_handle};
pub use player::{
    ActionQueue, CollisionSource, Dead, Egress, Flying, LastPlayerInput, LastSprintingSent,
    LocalPlayer, LocalPlayerPlugin, MovementIntent, PhysicsState, PlayerCollision, PrevPosition,
    Profile, SelectedSlot, SprintKeyHeld, Submersion, reset_local_player, spawn_local_player,
};
pub use plugin::CorePlugin;
pub use resources::WorldTime;
pub use session::{
    ActionBarOverlay, HudEffects, Phase, RespawnCount, ServerEntityId, SessionBossBars,
    SessionHudPlugin, SessionMenus, SessionPhase, SessionPlugin, SessionScoreboard, SessionSet,
    SessionTabList, TitleOverlay, Vitals, Xp, insert_hud_components, spawn_session,
};
pub use runner::Runner;
pub use schedules::{Extract, GameTick, NetIngest, Update};
pub use sets::{ExtractSet, FrameSet, IngestSet, TickSet};

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::world::World;

    /// `CorePlugin` must not panic building on a bare `App`, and each of the
    /// three schedules it owns must exist afterward (`init_schedule` is
    /// otherwise silent, so "did it actually run" needs a positive check, not
    /// just "the build didn't panic").
    #[test]
    fn core_plugin_registers_all_three_owned_schedules() {
        use bevy_ecs::schedule::Schedules;

        let mut app = app::App::new();
        app.add_plugins(CorePlugin);

        let schedules = app.world().resource::<Schedules>();
        assert!(schedules.contains(NetIngest));
        assert!(schedules.contains(GameTick));
        assert!(schedules.contains(Extract));
    }

    /// The negative control for the above: a `World` nobody ran `CorePlugin`
    /// on has no `Schedules` resource at all, so the assertion above is
    /// actually discriminating rather than trivially true of any `World`.
    #[test]
    fn bare_world_has_no_schedules_resource() {
        use bevy_ecs::schedule::Schedules;

        let world = World::new();
        assert!(world.get_resource::<Schedules>().is_none());
    }

    /// [`WorldTime`] is a plain resource: `CorePlugin` does not insert it
    /// (see its doc comment), so a consumer inserts and updates it directly.
    #[test]
    fn world_time_is_a_plain_insertable_resource() {
        let mut world = World::new();
        world.insert_resource(WorldTime::default());
        assert_eq!(world.resource::<WorldTime>().time_of_day, 0);
        world.resource_mut::<WorldTime>().time_of_day = 13_000;
        assert_eq!(world.resource::<WorldTime>().time_of_day, 13_000);
    }

    /// [`EcsHandle`] readers see writes made through another clone of the
    /// same handle — the whole point of it being an `Arc<RwLock<_>>` rather
    /// than an owned `World`.
    #[test]
    fn ecs_handle_clones_share_state() {
        let handle = new_handle();
        handle.write().insert_resource(WorldTime {
            age: 42,
            time_of_day: 6_000,
        });

        let reader = handle.clone();
        assert_eq!(reader.read().resource::<WorldTime>().age, 42);
    }

    /// The headless [`Runner`] arm actually runs `GameTick` — a fixed-tick
    /// accumulator is easy to write wrong in a way where it silently runs
    /// zero ticks, so this counts them rather than only checking the loop
    /// exits.
    #[test]
    fn headless_runner_runs_game_tick_at_least_once() {
        use bevy_ecs::resource::Resource;

        #[derive(Resource, Default)]
        struct TickCount(u32);

        let mut app = app::App::new();
        app.add_plugins(CorePlugin);
        app.init_resource::<TickCount>();
        app.world_mut()
            .schedule_scope(GameTick, |_world, schedule| {
                schedule.add_systems(|mut count: bevy_ecs::system::ResMut<TickCount>| {
                    count.0 += 1;
                });
            });

        let runner = Runner::Headless {
            tick_hz: 1000.0, // fast, so the test does not sleep meaningfully
            max_catch_up_ticks: 10,
        };
        let mut iterations = 0;
        runner.run_headless(&mut app, || {
            iterations += 1;
            iterations >= 3
        });

        assert!(app.world().resource::<TickCount>().0 >= 1);
    }
}
