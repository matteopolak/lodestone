//! The four Stage-0 schedule labels from `docs/bevy-migration.md` §4.2.
//!
//! One thread, fixed order, once per driver iteration (§4.1(b)) —
//! `lodestone-physics` is bit-exact against a JVM oracle with golden traces,
//! so which thread an input lands on must never become a scheduling
//! artefact.

use bevy_ecs::schedule::ScheduleLabel;

/// Drains the net-thread's `ClientEvent` channel and applies it to the ECS.
/// Runs once per driver iteration, before `GameTick` — see `IngestSet` for
/// its internal ordering.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NetIngest;

/// The 20 Hz Minecraft simulation tick. Catch-up capped at ten ticks per
/// driver iteration (`docs/frame-pacing.md`, matching vanilla's own
/// `MAX_TICKS_PER_UPDATE` and azalea's `run_schedule_loop`,
/// `azalea-client/src/client.rs:199-206`) — see [`crate::Runner`].
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GameTick;

/// Per-frame, last: reads components and writes the plain POD buffers
/// `lodestone-render` consumes. Never depended on by `lodestone-render`
/// itself (§4.4) — the extract systems live upstream of it, in whichever
/// crate owns the `App`.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Extract;

/// Bevy's own per-frame schedule, reused rather than redefined (§4.2: "Update
/// (bevy's)") — `FrameSet` orders within it. Re-exported here so a plugin
/// author never needs a second `bevy_app` dependency just to name it.
pub use bevy_app::Update;
