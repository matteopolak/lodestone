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
    /// Registry holder id of the world clock (`id + 1` on the wire; `0` selects
    /// an inline direct value, which carries no bytes for the empty
    /// `WorldClock` record).
    pub holder_id: i32,
    /// Total ticks elapsed on this clock. Modulo `24000` this is the clock's
    /// time of day.
    pub total_ticks: i64,
    /// Fractional progress into the current tick.
    pub partial_tick: f32,
    /// Rate at which the clock advances.
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
    /// Best-effort time of day: the first clock update's tick count, or the
    /// world age when no clock updates are present.
    ///
    /// A normal single-overworld session sends exactly one clock update (the
    /// day clock), so this is its `total_ticks`. It is documented as
    /// best-effort because the wire no longer names a single canonical day
    /// clock without resolving the `Holder` against the world-clock registry,
    /// which this phase does not load.
    #[must_use]
    pub fn day_time(&self) -> i64 {
        self.clocks
            .first()
            .map_or(self.game_time, |clock| clock.total_ticks)
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
