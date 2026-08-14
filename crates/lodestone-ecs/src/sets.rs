//! The public `SystemSet` labels a plugin orders against (`docs/bevy-migration.md`
//! §4.2, §6). These are the plugin ABI's ordering anchors — azalea's
//! precedent (§2.6) is that anchors should be sets, not system functions, so
//! internal systems can be renamed or split without breaking a plugin that
//! only names the set.

use bevy_ecs::schedule::SystemSet;

/// Bukkit-style cross-plugin event-priority tiers: a chain of
/// `SystemSet`s two plugins that have never heard of each other can order
/// against, mirroring `org.bukkit.event.EventPriority` almost exactly.
///
/// # Why this exists alongside `TickSet`/`IngestSet`/`FrameSet`/`ExtractSet`
///
/// Those four anchor a plugin against *our* systems, and that is enough for
/// one plugin composing with native code — `.after(TickSet::Intent)` never
/// needs to know what else is in the set. It does **not** help two
/// *third-party* plugins agree on order with each other, because neither
/// crate depends on the other's types to name in `.before()`/`.after()`.
/// `EventPriority` is published from `lodestone-ecs` specifically so both
/// sides can `.in_set(EventPriority::High)` without ever importing one
/// another — the actual Bukkit guarantee, and the only option that lets
/// two plugins that have never heard of
/// each other agree on order.
///
/// # `Monitor` is a distinct tier, not just the last one
///
/// Bukkit's `EventPriority.MONITOR` is documented as "read only, cannot
/// modify the event, guaranteed to run after every other priority including
/// cancellation" — logging/statistics/audit plugins rely on that guarantee to
/// see the *final* outcome, never an intermediate one a `High`-priority
/// handler might still veto. Ordering alone (being last in the chain) cannot
/// promise the "read only" half; nothing stops a plugin from registering a
/// mutating system in `Monitor` and defeating the guarantee for everyone else
/// reading state afterward. See `tests::monitor_tier_rejects_a_mutable_writer`
/// below for the structural check this crate adds on top of the chain: it
/// walks a *built* schedule and asks each system in this set, through
/// `bevy_ecs::system::System::initialize`'s public `FilteredAccessSet`,
/// whether it has any mutable World access at all — not a convention, a
/// runtime fact about the schedule.
///
/// # Anchored per schedule, not just declared
///
/// `CorePlugin` chains and configures this same six-tier order inside *every*
/// one of the four public schedules (`NetIngest`, `GameTick`, `Update`,
/// `Extract`), so a plugin's `GameEvent`-observing system gets a defined
/// cross-plugin order no matter which schedule it lives in — there is no
/// single canonical "the event schedule" here the way Bukkit has one thread.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventPriority {
    /// Runs first. Bukkit's `EventPriority.LOWEST`.
    Lowest,
    /// Bukkit's `EventPriority.LOW`.
    Low,
    /// The default a handler with no stated opinion should use. Bukkit's
    /// `EventPriority.NORMAL`.
    Normal,
    /// Bukkit's `EventPriority.HIGH`.
    High,
    /// Runs last among the mutating tiers. Bukkit's `EventPriority.HIGHEST`.
    Highest,
    /// Runs after every other tier, and must never mutate the `World` — see
    /// this enum's own doc section on why that is enforced rather than
    /// merely documented.
    Monitor,
}

/// The structural half of the monitor-tier guarantee: panics if `system` has any mutable `World`
/// access, so a plugin author who registers a mutating handler under
/// [`EventPriority::Monitor`] gets a startup panic rather than a silently
/// broken "read only, guaranteed to run last" guarantee.
///
/// Call it once per candidate system, before `.in_set(EventPriority::Monitor)`
/// — `crates/plugins/lodestone-event-logger` calls this on its own logging
/// system for exactly that reason.
///
/// # Panics
///
/// If `system`'s own parameter types grant it write access to any component
/// or resource. The check runs `System::initialize` against a throwaway,
/// otherwise-unused `World` — the returned `FilteredAccessSet` is purely a
/// fact about the system's parameter *types*, not about any live data, so a
/// scratch `World` is exactly as informative as the real one and needs no
/// schedule at all.
///
/// # What this does not catch, named rather than silently accepted
///
/// `Commands` reserves no component/resource access up front — its
/// mutations are deferred and applied later via `System::apply_deferred`, out
/// of band from the access set `System::initialize` returns. A `Monitor`
/// system that takes `Commands` and queues a mutation passes this check
/// today and still breaks the guarantee. Tracked as a known gap rather than
/// worked around here; see `docs/plugin-api.md`.
///
/// # Why this checks the system directly rather than walking a built schedule
///
/// The obvious-looking alternative — add the candidate systems to a real
/// schedule, build it, then ask the *schedule* which systems in `Monitor`
/// have mutable access — was tried first and does not work with bevy 0.19's
/// public API, checked directly rather than assumed:
///
/// - `bevy_ecs::schedule::graph::node::SystemWithAccess` (the type that
///   pairs a boxed system with its computed `FilteredAccessSet`) keeps that
///   `access` field `pub(crate)`, so even a `&SystemWithAccess` reached from
///   outside `bevy_ecs` cannot read the cached set back out.
/// - `ScheduleGraph::systems` (`pub systems: Systems`) and
///   `Systems::get_mut` *are* public, and `SystemWithAccess` itself
///   `impl System for SystemWithAccess { fn initialize(...) { self.system
///   .initialize(world) } }` — delegating straight to the wrapped system —
///   so calling `.initialize()` again looked like a way to recompute the
///   identical set through 100% public API. It does not work: once
///   `Schedule::initialize` has built the schedule, the systems are moved
///   into the optimized `executable: SystemSchedule` representation for
///   execution, and `Systems::get_mut(key)` on the *graph*'s own node
///   storage returns `None` for every key — measured directly (a debug
///   assertion in an earlier draft of this function's test confirmed zero
///   iterations, not a logic bug in the loop). `Schedule::systems()` does
///   reach the executable's systems, but only as `&ScheduleSystem`
///   (immutable), which cannot call `initialize(&mut self, ..)`.
///
/// So: bevy exposes the access-set *computation* publicly (any caller can
/// run it on a system of their own), but not a way to read a **built
/// schedule's own cached copy** back out. This function uses the part that
/// is public — check before scheduling, not after.
pub fn assert_monitor_system_is_read_only<M>(system: impl bevy_ecs::system::IntoSystem<(), (), M>) {
    use bevy_ecs::system::{IntoSystem, System};
    use bevy_ecs::world::World;

    let mut world = World::new();
    let mut system = IntoSystem::into_system(system);
    let access = system.initialize(&mut world);
    assert!(
        !access.combined_access().has_any_write(),
        "system `{}` was registered for EventPriority::Monitor but has \
         mutable World access — Monitor must be read-only (issue #110)",
        system.name()
    );
}

/// Ordering within [`crate::NetIngest`].
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum IngestSet {
    /// Drain the net-thread's `ClientEvent` channel into a per-frame buffer.
    Drain,
    /// Fold each drained event into components/resources.
    Apply,
    /// Rebuild id/UUID → `Entity` indexes after this frame's spawns/despawns.
    Index,
}

/// Ordering within [`crate::GameTick`]. `Send` is last so a movement/input
/// packet reflects everything the tick did — azalea's
/// `game_tick_packet.after(PhysicsSystems).after(MiningSystems).after(send_position)`
/// (`azalea-client/src/plugins/tick_end.rs`) is the precedent.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum TickSet {
    /// Poll the platform's held keys / raw device state for this tick.
    /// Nothing lives here yet — it is reserved for whatever eventually reads
    /// a keyboard or gamepad as a system rather than a plain resource write —
    /// but it stays a distinct anchor from [`Self::Intent`] so a future
    /// raw-input system and an intent-writing system are never mistaken for
    /// the same ordering concern.
    Input,
    /// Write this tick's [`crate::player::MovementIntent`] (and
    /// [`crate::player::LookIntent`]).
    ///
    /// A dedicated anchor, split out from [`Self::Input`], because intent
    /// writers are exactly the systems a plugin adds a second one of — a
    /// navigator alongside human input — and `docs/bevy-migration.md`'s
    /// planned `ambiguity_detection: LogLevel::Error` turns two unordered
    /// writers of the same component into a schedule *build failure*, not a
    /// race. Anchoring intent on its own set gives a plugin author one place
    /// to order against (`.after(TickSet::Intent)` to override human input
    /// this tick, or `.in_set(TickSet::Intent).after(...)` to compose with
    /// it) without having to reason about whatever else might land in
    /// [`Self::Input`] later.
    ///
    /// **`compute_movement_intent` must stay ordered before
    /// `tick_sprint_window`** (`lodestone_controller::ecs`) — both are
    /// anchored here, chained. Swapping them moves the double-tap sprint
    /// window by one tick: see their doc comments.
    Intent,
    /// `lodestone-physics` integration. The math stays a plain library the
    /// system calls (`docs/bevy-migration.md` §8) — this set is only ever a
    /// caller, never the integrator itself.
    Physics,
    /// Client-side prediction reconciliation.
    Predict,
    /// Walk-cycle / item-physics animation advance.
    Animate,
    /// Emit whatever packets this tick's state changes require.
    Send,
}

/// Ordering within bevy's own [`crate::Update`] schedule.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum FrameSet {
    /// Poll the platform (winit/headless) for input events.
    Input,
    /// Advance interpolation clocks toward the next `GameTick` sample.
    Interpolate,
    /// Recompute the camera from the interpolated state.
    Camera,
    /// Terrain mesh scheduling: enqueue snapshots for whatever went stale and
    /// collect what the worker pool finished (Stage 4).
    ///
    /// Last in the frame because it depends on nothing else in it and because
    /// that is where the pre-ECS driver ran it — after the net poll, so a column
    /// that arrived this frame is meshed this frame. **It only enqueues and
    /// drains**: the meshing itself stays on the worker pool
    /// (`lodestone_shell::mesher::MeshScheduler`), so nothing here can make
    /// presentation gate simulation (`docs/frame-pacing.md`).
    Terrain,
}

/// Ordering within [`crate::Extract`].
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExtractSet {
    /// Terrain mesh upload/removal bookkeeping.
    Terrain,
    /// Entity draw-instance extraction.
    Entities,
    /// World-space debug geometry a plugin wants drawn this frame — a
    /// pathfinder's planned route, a bot's reachability probe, anything that
    /// is otherwise invisible and therefore undebuggable (`CLAUDE.md`'s
    /// island rule). A plugin system that pushes into
    /// [`crate::player::DebugLines`] belongs in this set. Grouped with the
    /// other world-space extracts (after [`Self::Entities`]) and before
    /// [`Self::Hud`], which is screen-space.
    Debug,
    /// HUD/overlay extraction.
    Hud,
}

#[cfg(test)]
mod tests {
    //! [`assert_monitor_system_is_read_only`]'s enforcement, plus
    //! proof that it composes with a real schedule the way a plugin would use
    //! it — `.in_set(EventPriority::Monitor)` inside a real `GameTick`, not
    //! only a standalone function call.
    use bevy_app::App;
    use bevy_ecs::resource::Resource;
    use bevy_ecs::schedule::IntoScheduleConfigs;
    use bevy_ecs::system::{Local, ResMut};

    use super::{EventPriority, assert_monitor_system_is_read_only};
    use crate::plugin::CorePlugin;
    use crate::schedules::GameTick;

    #[derive(Resource, Default)]
    struct Probe(u32);

    /// A genuinely read-only observer: `Local` is system-private state, never
    /// backed by the `World`, so incrementing it touches no component or
    /// resource access at all — the shape a real MONITOR-tier logger
    /// (`crates/plugins/lodestone-event-logger`) uses.
    fn read_only_observer(mut seen: Local<u32>) {
        *seen += 1;
    }

    /// A system with genuine mutable `World` access (`ResMut`) — the shape
    /// MONITOR must reject.
    fn mutable_writer(mut probe: ResMut<Probe>) {
        probe.0 += 1;
    }

    /// The positive case: a real read-only observer passes the check with no
    /// panic.
    #[test]
    fn a_read_only_observer_passes_the_monitor_check() {
        assert_monitor_system_is_read_only(read_only_observer);
    }

    /// **The control.** An absence-of-panic assertion is worth only as much
    /// as the evidence the detector would have fired on the shape it exists
    /// to catch — this feeds the identical checker a `ResMut` writer and
    /// requires it to panic. Without this, the test above could be passing
    /// against a checker that never panics at all.
    #[test]
    #[should_panic(expected = "must be read-only")]
    fn a_mutable_writer_fails_the_monitor_check() {
        assert_monitor_system_is_read_only(mutable_writer);
    }

    /// End-to-end proof that a system cleared by
    /// [`assert_monitor_system_is_read_only`] is exactly the shape a plugin
    /// registers `.in_set(EventPriority::Monitor)` on a real schedule — the
    /// checker is not exercising a signature the ABI never actually accepts.
    #[test]
    fn a_checked_read_only_system_builds_cleanly_in_monitor() {
        assert_monitor_system_is_read_only(read_only_observer);

        let mut app = App::new();
        app.add_plugins(CorePlugin);
        app.add_systems(GameTick, read_only_observer.in_set(EventPriority::Monitor));
        app.world_mut().schedule_scope(GameTick, |world, schedule| {
            schedule
                .initialize(world)
                .expect("a read-only Monitor system must build cleanly");
        });
    }
}
