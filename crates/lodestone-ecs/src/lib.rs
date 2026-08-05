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
//! # Stage 4
//!
//! [`chunks`] holds [`ChunkWorld`] — the chunk store as a `Resource`, per
//! §4.1(d). Chunks are still not entities and the mesher is still a worker pool;
//! what changed is that there is now **one** `lodestone_world::World` in the
//! process instead of two (`lodestone_shell::sim::Sim`'s offline one and
//! `lodestone_client::state::SharedState`'s live one), so the shell no longer
//! branches per read site on which world it means. `Sim`'s `world` and
//! `demo_collision` fields are **deleted**.
//!
//! # Stage 5
//!
//! [`FrameClock`] (the *driver's* clock, as opposed to [`WorldTime`]'s server
//! clock), [`VersionData`] (§4.3) and [`session::SessionChat`]. The clock and the
//! chat log arrive together deliberately: Stage 3 deferred the log precisely
//! because every push and every read needs a monotonic client clock, so a
//! component here while the clock stayed a `Sim` field would have been a second
//! clock. `lodestone_shell::sim::Sim`'s `clock_secs`, `accumulator`,
//! `interp_alpha`, `tick_count`, `frame_count`, `chat_log`, `version_data`,
//! `target`, `particles`, `mining`, `placement`, `attacking` and `last_step` are
//! **deleted** — 28 fields down to 15.
//!
//! `Sim` itself is **not** deleted, and `docs/sim-dissolution.md` records why field
//! by field.
//!
//! # §4.1(c) — one `World`, one `GameTick`, one accumulator
//!
//! The three bevy `World`s are **one**. The driver builds it, hands the
//! [`EcsHandle`] to `NetClient::connect`, and `lodestone_client`'s `SharedState`
//! adopts that handle instead of minting its own; the entity interpolator's
//! `World` is gone entirely and its systems run in the same schedules as the
//! player's. Consequences, each of which was blocked on this and nothing else:
//!
//! - [`CorePlugin`] now inserts [`WorldTime`] **and** [`FrameClock`]. The guard
//!   that refused to existed only to stop two `World`s becoming two clocks.
//! - There is one 20 Hz accumulator ([`FrameClock::accumulator`]) on one
//!   catch-up policy ([`MAX_CATCH_UP_TICKS`] = vanilla's ten, not the shell's old
//!   five). `lodestone_shell::entities`'s `TickAccum` is deleted.
//! - A plugin adding a `GameTick` system no longer has to pick which `App` or
//!   which clock; there is one of each.
//! - Every guard taken through [`hold_read`] / [`hold_write`] folds its own
//!   duration into the [`LockHolds`] resource, so the "no guard spans a frame"
//!   bound the whole discipline rests on is **measured** rather than counted off
//!   the code.
//!
//! See `docs/world-unification.md` for the lock discipline this buys and costs.
//!
//! # The ingest seam — the local player, and the vitals
//!
//! §4.1(c) left two things at the `lodestone-client` ↔ `lodestone-ecs` seam, and
//! they turned out to be the same problem:
//!
//! - **The local player was not in [`entity::EntityIndex`] at all.** It is
//!   populated by `ClientEvent::EntitySpawned`, and vanilla never sends an
//!   `AddEntity` for yourself — only `Login`. So `update_attributes` for our *own*
//!   id was folded into nothing, and so would any future per-player component fed
//!   from entity ingest. [`ingest::apply_local_player_login`] closes it.
//! - **`PlayerSnapshot`'s vitals were still duplicated**, and the `World`
//!   unification was not the blocker — `SharedState::apply`'s *exclusive* routing
//!   was. [`session::ServerGameMode`], [`session::ServerDimension`] and
//!   [`session::ServerAlive`] complete the component set so the whole fold can
//!   move here and `PlayerSnapshot` can be **derived**, rather than weakening the
//!   routing so that one event has two folds. `docs/session-components.md` records
//!   the decision.
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

pub mod async_task;
pub mod chunks;
pub mod commands;
pub mod egress;
pub mod entity;
pub mod events;
mod handle;
pub mod ingest;
pub mod items;
pub mod permissions;
pub mod player;
mod plugin;
pub mod plugin_channel;
pub mod plugin_message;
pub mod recipes;
mod resources;
pub mod riding;
mod runner;
mod schedules;
pub mod scheduler;
pub mod session;
mod sets;
pub mod veto;

/// Re-exported so plugin authors never need to match `bevy_app`'s version by
/// hand (azalea does the same at `azalea/src/lib.rs:63-64`).
pub use bevy_app as app;
/// Re-exported so plugin authors never need to match `bevy_ecs`'s version by
/// hand.
pub use bevy_ecs as ecs;
/// Re-exported because [`EcsHandle`] is a `parking_lot::RwLock` and a driver that
/// wants to *name* a guard type (rather than only use one as a temporary) must
/// spell it with the same `parking_lot` this crate locked with. Matching the
/// version by hand in every consumer's manifest is how you end up with two
/// `RwLock`s that look identical and are not the same lock.
pub use parking_lot;

pub use async_task::{
    AsyncTaskPool, AsyncTaskPoolPlugin, PendingTask, PoolStats, drain_completed_tasks,
    in_async_worker,
};
pub use chunks::{ChunkWorld, ChunkWorldWrite, WorldExtent};
pub use commands::{
    CommandDispatchError, CommandHandler, CommandInvocation, CommandOutcome, CommandRegisterError,
    CommandRegistry, CommandSource, PlayerDirectory, PluginCommand, PluginCommandsPlugin,
    RegisteredCommand, choice_argument, command_tree_for, dispatch, player_argument, suggest,
};
pub use egress::{EgressFilterPlugin, EgressFilters, EgressStats, Verdict};
pub use events::{GameEvent, GameEventBus, GameEventBusPlugin};
pub use handle::{
    EcsHandle, HoldStats, LockHolds, hold_read, hold_write, new_handle, new_ingest_handle,
};
pub use items::{CustomItems, CustomItemsExt, CustomItemsPlugin};
pub use permissions::{
    DEFAULT_PERMISSION, Grant, GrantSet, Group, LevelBasedPermissionSet, Permission,
    PermissionDefault, PermissionLevel, PermissionQuery, PermissionRegistry, PermissionResolver,
    PermissionStore, PermissionSubject, Permissions, SubjectPermissions, normalize_node,
};
pub use player::{
    ActionQueue, AttackStrengthTicker, CollisionSource, Dead, DebugLine, DebugLines, Egress,
    Flying, JumpTriggerTime, LastFlyingSent, LastPlayerInput, LastSprintingSent, LocalPlayer,
    LocalPlayerPlugin, LookIntent, MovementIntent, PhysicsState, PlayerCollision, PrevPosition,
    Profile, SelectedSlot, SprintKeyHeld, Submersion, WasJumping, apply_creative_flight_input,
    apply_look_intent, cancel_flight_on_landing, clear_debug_lines, pin_passenger_to_vehicle,
    reset_local_player, spawn_local_player, tick_attack_strength,
};
pub use plugin::CorePlugin;
pub use plugin_channel::{
    OutboundPluginChannel, OutboundPluginChannelPlugin, OutboundPluginChannelState,
    PluginChannel, PluginChannelAppExt, PluginChannelPlugin, PluginChannelState,
    dispatch_plugin_channel, dispatch_plugin_channel_outbound,
};
pub use plugin_message::{PluginMessageAppExt, PluginMessagePlugin};
pub use recipes::{RecipeRegistry, RecipeRegistryExt, RecipeRegistryPlugin};
pub use resources::{
    FrameClock, MAX_CATCH_UP_SECS, MAX_CATCH_UP_TICKS, TICK_PERIOD, VersionData, WorldTime,
};
pub use runner::Runner;
pub use scheduler::{SchedulerPlugin, TaskId, TaskScheduler, run_due_tasks};
pub use schedules::{Extract, GameTick, NetIngest, Update};
pub use session::{
    Abilities, ActionBarOverlay, HudEffects, Phase, RespawnCount, Riding, ServerAlive,
    ServerDifficulty, ServerDimension, ServerEntityId, ServerGameMode, SessionBlockDestruction,
    SessionBossBars, SessionChat, SessionHudPlugin, SessionMenus, SessionPhase, SessionPlugin,
    SessionScoreboard, SessionSet, SessionTabList, TitleOverlay, Vitals, Xp,
    insert_hud_components, insert_session_components, spawn_session,
};
pub use veto::{ActionVetoPlugin, ActionVetoes, Verb, VerbContext, VetoStats};
pub use sets::{
    EventPriority, ExtractSet, FrameSet, IngestSet, TickSet, assert_monitor_system_is_read_only,
};

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
