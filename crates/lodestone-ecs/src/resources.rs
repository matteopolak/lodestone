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

/// The **driver's** clock: real elapsed time, the fixed-timestep residual, and
/// the tick/frame counters derived from them.
///
/// Not the server's clock — that is [`WorldTime`], which the server sets and can
/// freeze. This one only ever moves forward with wall time and exists because
/// several consumers need "how long ago did this happen" in the *client's* own
/// frame of reference: the chat fade-out
/// (`lodestone_game::chat::ChatLog::recent_ages`), the render-side interpolation
/// factor, and the fixed-timestep health readout (`frames / ticks`).
///
/// # Why this is a resource and not four fields on the driver
///
/// `docs/bevy-migration.md` Stage 5. Stage 3 deferred `chat_log` explicitly
/// *because* of this: every chat push needs the clock and every read needs it
/// again to age the line, so moving the log to a component while the clock stayed
/// a `Sim` field would have put a second clock in the process. They move
/// together, and this is the half that had nowhere to live.
///
/// # `secs` is monotonic, `accumulator` is not
///
/// `secs` is the sum of every `dt` ever handed to the driver — it never
/// decreases and is never reset by a session teardown, which is what makes a
/// chat timestamp from before a reconnect still age correctly. `accumulator` is
/// the sub-tick residual and cycles within `[0, tick_period)`.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Default)]
pub struct FrameClock {
    /// Monotonic wall-clock seconds since the driver started, accumulated from
    /// the real per-frame `dt`.
    pub secs: f64,
    /// Seconds banked toward the next fixed tick. Always less than one tick
    /// period once the driver's tick loop has drained it.
    pub accumulator: f64,
    /// Fractional progress `[0,1)` from the last tick toward the next — the
    /// render-side interpolation factor.
    pub interp_alpha: f32,
    /// Total fixed ticks run since the driver started.
    pub ticks: u64,
    /// Total driver iterations (frames) since the driver started.
    pub frames: u64,
}

impl FrameClock {
    /// Frames per fixed tick since start — the fixed-timestep health number the
    /// debug overlay draws. `0.0` before the first tick, rather than a division
    /// by zero.
    #[must_use]
    pub fn frames_per_tick(&self) -> f32 {
        if self.ticks == 0 {
            0.0
        } else {
            self.frames as f32 / self.ticks as f32
        }
    }
}

/// The version adapter for the configured protocol, as a resource — §4.3 of
/// `docs/bevy-migration.md`.
///
/// Kept a trait object deliberately: a generic parameter would monomorphise the
/// whole `App` per protocol family and force whoever builds it to *name* a
/// version, which is the thing `lodestone-shell` has never done (its only route
/// to version data is `lodestone_registry::adapter_for_protocol`). `VersionAdapter`
/// is already declared `Send + Sync + Debug`, so this needs no signature change
/// anywhere.
///
/// `None` is a real, expected state and not an error: it means **no version
/// family is compiled in** for that protocol, which is every build without
/// `--features live`. Consumers must degrade honestly rather than substituting a
/// default — the mining predictor, for instance, refuses to dig rather than
/// guessing a hardness, because guessing one is precisely how block breaking got
/// too fast the first time.
#[derive(Resource, Debug, Default)]
pub struct VersionData(pub Option<Box<dyn lodestone_model::VersionAdapter>>);

impl VersionData {
    /// The version's break-time census for a block-state id, or `None` when
    /// there is no adapter or the id is outside its census. The two causes are
    /// deliberately not distinguished: the correct response to both is the same,
    /// refuse to dig.
    #[must_use]
    pub fn block_hardness(&self, state_id: u32) -> Option<lodestone_model::BlockHardness> {
        self.0.as_ref()?.block_hardness(state_id)
    }

    /// The held item's mining contribution for a block-state id, or `None` when
    /// there is no adapter or nothing in the census.
    #[must_use]
    pub fn tool_mining(
        &self,
        held: Option<&lodestone_model::ItemStack>,
        state_id: u32,
    ) -> Option<lodestone_model::ToolMining> {
        self.0.as_ref()?.tool_mining(held, state_id)
    }
}
