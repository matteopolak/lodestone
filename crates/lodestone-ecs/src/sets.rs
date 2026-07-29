//! The public `SystemSet` labels a plugin orders against (`docs/bevy-migration.md`
//! §4.2, §6). These are the plugin ABI's ordering anchors — azalea's
//! precedent (§2.6) is that anchors should be sets, not system functions, so
//! internal systems can be renamed or split without breaking a plugin that
//! only names the set.

use bevy_ecs::schedule::SystemSet;

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
/// (`azalea-client/src/plugins/tick_end.rs:18-26`) is the precedent.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum TickSet {
    /// Turn buffered input into this tick's movement intent.
    Input,
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
    /// HUD/overlay extraction.
    Hud,
}
