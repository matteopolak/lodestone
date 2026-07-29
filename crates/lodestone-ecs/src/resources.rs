//! Core ECS resources — state that has exactly one owner in the `World`,
//! never a component on some entity.

use bevy_ecs::resource::Resource;

/// The server's authoritative clock, folded from `ClientEvent::TimeChanged`.
///
/// Stage 0 of `docs/bevy-migration.md` moves this off
/// `lodestone_client::state::Inner` — where it was one of the two duplicate
/// state copies named in that doc's §1.1 — into a resource. The day/night
/// driver of sky and entity light
/// (`lodestone_render::entity::sky_darken_for_time_of_day`, wired in
/// `lodestone-shell`'s `app.rs`) reads it through
/// `lodestone_client::ClientHandle::world_time()`, which this is now the sole
/// backing store for: `Inner.world_age` / `Inner.time_of_day` no longer
/// exist, so there is nowhere left for a second copy to live.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorldTime {
    /// Total ticks since the world was created
    /// (`ClientEvent::TimeChanged::world_age`). Monotonically increasing,
    /// unlike `time_of_day`, which the server may freeze (`/gamerule
    /// doDaylightCycle false`) or set arbitrarily (`/time set`).
    pub age: i64,
    /// Ticks within the current day. The server does not wrap this to
    /// `0..24000` before sending it — `sky_darken_for_time_of_day` does that
    /// reduction itself — so treat it as an unbounded counter, not an angle.
    pub time_of_day: i64,
}
