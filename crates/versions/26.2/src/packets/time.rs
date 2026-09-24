//! Clientbound `set_time` packet for protocol 776.
//!
//! In 26.2 `set_time` changed shape: it is no longer a pair of longs
//! (`gameTime`, `dayTime`). It now carries the world's `gameTime` followed by a
//! **map** of per-world-clock updates — `Map<Holder<WorldClock>,
//! ClockNetworkState>`. Each clock update contributes its own running tick
//! count, so "the" time of day is no longer a single wire field.
//!
//! The map cannot be expressed with the derive macros: there is no generic
//! `Vec<T>`/map codec in `lodestone-core` (only `Vec<u8>`), and the key is a
//! registry `Holder` (a VarInt id, `id + 1`, with `0` meaning an inline direct
//! value). The decoder is therefore hand-written against the wire format of
//! `ClientboundSetTimePacket` (behavioural reference only) and reported as the
//! signal for a future generic-collection codec.

use lodestone_core::{Ctx, Decode, Reader, Result};

/// A single world-clock update from a `set_time` packet.
///
/// Wire layout (`ClockNetworkState`): a VarLong running tick count, then two
/// big-endian `f32`s — the partial tick and the clock rate.
#[derive(Debug, Clone, PartialEq)]
pub struct ClockUpdate {
    /// Registry holder id of the world clock — a **plain** VarInt registry id.
    ///
    /// The key codec is `vanilla's own byte buf codecs's own holder registry(vanilla's own registries's own world clock)`,
    /// which is `registry(key, Registry::asHolderIdMap)`: a bare
    /// `vanilla's own var int's own write(id)` with **no `+1` offset and no inline-direct path**. The
    /// `id + 1` / `0 = inline` convention belongs to the *other* codec,
    /// `vanilla's own byte buf codecs's own holder(key, directCodec)`, which `set_time` does not use.
    /// (This comment previously said otherwise; the decode was always right, the
    /// record was not.)
    ///
    /// 26.2 registers two clocks, in this order: `minecraft:overworld` = `0`,
    /// `minecraft:the_end` = `1` (`WorldClocks::bootstrap`). The overworld clock
    /// is the day/night one; see [`SetTime::day_clock`].
    pub holder_id: i32,
    /// Total ticks elapsed on this clock. Modulo `24000` this is the clock's
    /// time of day.
    pub total_ticks: i64,
    /// Fractional progress into the current tick.
    pub partial_tick: f32,
    /// Rate at which the clock advances. **`0.0` when the clock is paused** —
    /// `vanilla's own server clock manager's own clock instance's own pack network state` sends
    /// `paused || !advance_time ? 0.0 : rate`, so `/gamerule advanceTime false`
    /// arrives as a rate of zero rather than as a flag.
    pub rate: f32,
}

/// Clientbound `set_time` packet body.
///
/// [`game_time`](SetTime::game_time) is the monotonic world age. The day/night
/// time of day lives inside the [`clocks`](SetTime::clocks) map — see
/// [`day_time`](SetTime::day_time).
#[derive(Debug, Clone, PartialEq)]
pub struct SetTime {
    /// Monotonic world age in ticks.
    pub game_time: i64,
    /// Per-world-clock updates, in wire order.
    pub clocks: Vec<ClockUpdate>,
}

impl SetTime {
    /// The overworld **day** clock's update, if this packet carries one.
    ///
    /// # Why this is an `Option`, and why the old fallback was a bug
    ///
    /// This used to be `day_time() -> i64`, falling back to
    /// [`game_time`](Self::game_time) when `clocks` was empty. Measured against
    /// a live 26.2 server, that fallback **is** the value the client ends up
    /// using, essentially always:
    ///
    /// * `MinecraftServer::forceGameTimeSynchronization` broadcasts
    ///   `SetTime(gameTime, an empty/literal map())` — an **empty** clock map — roughly once a
    ///   second, forever.
    /// * `ServerClockManager::modifyClock` sends a *one-entry* map only when a
    ///   clock actually changes (`/time set`, rate, pause), and
    ///   `createFullSyncPacket` sends the full map once, at join.
    ///
    /// So the empty-map packet arrived every second and overwrote the day time
    /// with the monotonic world age. Measured on the survival oracle: `/time set`
    /// `noon`/`midnight`/`day`/`night` in turn, and the client's reported
    /// `time_of_day` never left `age` (`639197`, `639257`, `639317`, `639377`) —
    /// so `sky_darken_for_time_of_day` returned a **session constant**
    /// (`0.24` there), and terrain and mobs were lit at one fixed hour for the
    /// whole session. On a world whose `age % 24000` lands in daylight the
    /// constant is `1.0` instead: permanent noon, i.e. the reported
    /// "the world is fullbright" and "the mobs look like they're in the daytime".
    ///
    /// An absent clock update therefore means **"nothing changed, keep what you
    /// had"**, never "the day time equals the world age". The caller holds the
    /// last update and extrapolates from `game_time` — see `V770Adapter`'s
    /// `DayClock`.
    ///
    /// # Which clock
    ///
    /// 26.2 has two (`WorldClocks::bootstrap`): `minecraft:overworld` (id `0`)
    /// and `minecraft:the_end` (id `1`). The map is a Java `HashMap`, so **wire
    /// order is not registry order** and `clocks.first()` cannot be trusted on
    /// the full-sync packet. This selects the lowest holder id present, which is
    /// the overworld clock for vanilla's registration order.
    ///
    /// # This is the fallback now, not the answer
    ///
    /// The crate now *does* ingest the `minecraft:world_clock`
    /// registry (see [`crate::packets::registry`]), so the caller can name the
    /// clock the current dimension actually follows and pass its holder id to
    /// [`clock_for`](Self::clock_for). Two things were wrong with the heuristic,
    /// and only one of them was hypothetical:
    ///
    /// * **The End, today, on vanilla.** `the_end`'s dimension type declares
    ///   `default_clock: minecraft:the_end`, holder id `1`. The lowest-id pick
    ///   returns holder `0` — the overworld's clock — so the End's sky followed
    ///   overworld time. Not a data-pack edge case.
    /// * A data pack reordering the registry, which was the only case the old
    ///   comment anticipated.
    ///
    /// Reach for this only when the registry did not resolve.
    #[must_use]
    pub fn day_clock(&self) -> Option<&ClockUpdate> {
        self.clocks.iter().min_by_key(|c| c.holder_id)
    }

    /// The update for world-clock holder `holder_id`, falling back to
    /// [`day_clock`](Self::day_clock) when the caller has no resolved holder.
    ///
    /// A `Some(id)` that this packet does not carry yields `None` — "this packet
    /// says nothing about my clock", which the caller must treat as "keep what
    /// you had". It must **not** fall through to another dimension's clock: a
    /// one-entry `modifyClock` broadcast for the overworld would otherwise
    /// re-anchor an End session to overworld time, which is the original bug in
    /// a new disguise.
    #[must_use]
    pub fn clock_for(&self, holder_id: Option<i32>) -> Option<&ClockUpdate> {
        match holder_id {
            Some(id) => self.clocks.iter().find(|c| c.holder_id == id),
            None => self.day_clock(),
        }
    }
}

impl Decode for SetTime {
    fn decode(r: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        let game_time = r.i64()?;
        let count = r.var_i32()?;
        let count =
            usize::try_from(count).map_err(|_| lodestone_core::Error::NegativeLength(count))?;
        let mut clocks = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let holder_id = r.var_i32()?;
            let total_ticks = r.var_i64()?;
            let partial_tick = r.f32()?;
            let rate = r.f32()?;
            clocks.push(ClockUpdate {
                holder_id,
                total_ticks,
                partial_tick,
                rate,
            });
        }
        let _ = ctx;
        Ok(Self { game_time, clocks })
    }
}
