//! Weather state (issue #324, `docs/plans/world-state.md` W1) — the server
//! half of vanilla's rain/thunder cycle.
//!
//! Vanilla keeps two independent booleans — `raining` and `thundering` — in
//! the world-global `WeatherData` SavedData (`minecraft:weather`,
//! `world/level/saveddata/WeatherData.java`), each driven by its own
//! countdown timer, plus two *interpolated* intensity levels (`rainLevel` /
//! `thunderLevel`) on `ServerLevel` itself that the client renders. A weather
//! cycle is a long clear spell, a rain spell, and — at an independent,
//! longer cadence — a thunder spell that can overlap either. All of it is
//! server-global, not per-dimension or per-connection; the client's existing
//! `WeatherCell` (`net.rs:1868` fold) is what these events reach, and the
//! particles/overlay rendering that cell feeds is a Tier-1 backlog client
//! item, explicitly not this module.
//!
//! This is the exact `advanceWeatherCycle` algorithm
//! (`ServerLevel.java:708-793`):
//!
//! * the two timers count **down** one per tick, and the boolean flips only
//!   when its timer reaches zero — at which point a *fresh* duration is
//!   sampled from vanilla's four `UniformInt` ranges (`ServerLevel.java:188-191`)
//!   so the next spell is long by construction;
//! * `clear_weather_time` (the `/weather clear <duration>` spell) short-circuits
//!   the whole timer block — it forces clear while it counts down;
//! * the levels interpolate ±0.01F per tick toward 1.0 (raining/thundering)
//!   or 0.0 (not), clamped to `[0, 1]` (`ServerLevel.java:753-768`);
//! * the caller is told, as a [`WeatherEvent`] list, exactly what changed —
//!   which the tick loop publishes onto a [`WeatherFeed`] so a connection can
//!   encode real `GAME_EVENT` packets (ids 1/2/7/8), the same snapshot-feed
//!   idiom as [`crate::tick::BlockTickFeed`]/[`ExplosionFeed`].
//!
//! # What is deliberately not here
//!
//! * **Persistence** — the four `WeatherData` scalars load/save with #437
//!   (`world_clocks`-shaped SavedData). Until then every world opens with the
//!   all-zero fresh state below, exactly as `WeatherData` starts, and the
//!   `prepareWeather` level-snap (`ServerLevel.java:699-706`) that a saved
//!   *raining* world does on load is moot. When #437 lands, load calls that
//!   snap and this struct gains fields from it; the cycle itself is
//!   unchanged.
//! * **The `advance_weather` game rule** is read as
//!   [`crate::tick::advance_weather()`], a function returning vanilla's
//!   default `true` — the same disclosed gap as
//!   [`crate::tick::mob_griefing()`]: this crate has no world-level
//!   `GameRules` registry yet (R1 of the world-state plan), and the
//!   per-connection `WorldAdminState::game_rules` is the wrong side of the
//!   world for a tick loop that runs with no connection at all.
//! * **The dimension gate** (`canHaveWeather`, `ServerLevel.java:708`) is
//!   unmodelled: this crate has only the overworld, which can have weather.
//! * **The seed** is a fixed literal (see [`WEATHER_SEED`]): this crate has
//!   no per-world seed store to draw a "real" one from yet, the same reason
//!   `tick.rs`'s `RANDOM_TICK_*_SEED` literals exist. Picking a different
//!   literal changes the cycle's *timing* but nothing structural.

use std::sync::{Arc, Mutex};

use lodestone_worldgen::rng::{LegacyRandomSource, RandomSource};

/// Seed for [`WeatherState`]'s `java.util.Random`-exact generator — see the
/// module doc's "deliberately not here" list. A fixed literal rather than
/// derived from a world seed, because no per-world seed store exists yet
/// (same reasoning as `crate::tick`'s `RANDOM_TICK_POSITION_SEED`/
/// `RANDOM_TICK_BEHAVIOR_SEED`).
const WEATHER_SEED: u64 = 0x5EED_9ABC;

/// Vanilla's per-tick intensity step, `ServerLevel.java:753-768` — each tick
/// moves `rainLevel`/`thunderLevel` this far toward the target implied by the
/// boolean, then clamps to `[0, 1]`.
pub(crate) const LEVEL_STEP: f32 = 0.01;

// The four `UniformInt` ranges (`ServerLevel.java:188-191`), inclusive both
// ends. Rain spells are shorter than clear spells (a world is rainy ~15.8% of
// the time) and thunder spells shorter still (~9.1%).
const RAIN_DELAY_MIN: i32 = 12_000;
const RAIN_DELAY_MAX: i32 = 180_000;
const RAIN_DURATION_MIN: i32 = 12_000;
const RAIN_DURATION_MAX: i32 = 24_000;
const THUNDER_DELAY_MIN: i32 = 12_000;
const THUNDER_DELAY_MAX: i32 = 180_000;
const THUNDER_DURATION_MIN: i32 = 3_600;
const THUNDER_DURATION_MAX: i32 = 15_600;

/// A weather transition a connection must learn about — one element of the
/// [`WeatherEvent::wire`] table below is exactly one
/// `ClientboundGameEventPacket` broadcast, in the order
/// `advanceWeatherCycle` sends them (`ServerLevel.java:771-793`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WeatherEvent {
    /// Rain just turned on (`ClientboundGameEventPacket.START_RAINING = 1`).
    StartRaining,
    /// Rain just turned off (`ClientboundGameEventPacket.STOP_RAINING = 2`).
    StopRaining,
    /// `rainLevel` moved this tick (`RAIN_LEVEL_CHANGE = 7`, value the new
    /// level).
    RainLevelChanged(f32),
    /// `thunderLevel` moved this tick (`THUNDER_LEVEL_CHANGE = 8`, value the
    /// new level).
    ThunderLevelChanged(f32),
}

impl WeatherEvent {
    /// The `ClientboundGameEventPacket` event code and `param` this event
    /// encodes as — `(event, param)` written as `writeByte(event)` +
    /// `writeFloat(param)` (`ClientboundGameEventPacket.java:14`). Start/stop
    /// raining carry `0.0F` exactly as vanilla's own broadcasts do.
    pub fn wire(self) -> (u8, f32) {
        match self {
            WeatherEvent::StartRaining => (1, 0.0),
            WeatherEvent::StopRaining => (2, 0.0),
            WeatherEvent::RainLevelChanged(level) => (7, level),
            WeatherEvent::ThunderLevelChanged(level) => (8, level),
        }
    }
}

/// A shared feed of weather transitions the world tick loop wants every
/// connection to learn about — the exact same idiom
/// [`crate::tick::BlockTickFeed`]/[`crate::tick::ExplosionFeed`] establish
/// for block changes and detonations, applied to the
/// [`WeatherState::tick`] drain instead.
///
/// Same single-consumer caveat as both of those, and the same resolution:
/// singleplayer (`crate::IntegratedServer::open_in_memory_with_mobs`) spawns
/// exactly one connection task per feed instance, and `bind` (LAN, issue
/// #439) gives each connection its own instance behind a relay arm.
#[derive(Debug, Clone, Default)]
pub struct WeatherFeed(Arc<Mutex<Vec<WeatherEvent>>>);

impl WeatherFeed {
    /// Records one weather transition for every consumer to learn about on
    /// their next [`drain_all`](Self::drain_all).
    pub fn publish(&self, event: WeatherEvent) {
        self.0
            .lock()
            .expect("weather feed lock poisoned")
            .push(event);
    }

    /// Drains and returns every transition published since the last call —
    /// see the struct doc comment for why this is safe only for exactly one
    /// consumer.
    pub fn drain_all(&self) -> Vec<WeatherEvent> {
        std::mem::take(&mut *self.0.lock().expect("weather feed lock poisoned"))
    }
}

/// The weather machine, owned by [`crate::tick::run_tick_loop`] (the
/// tick-thread world state, per `docs/plans/world-state.md` W1) and ticked
/// once per world tick. A plain struct, deliberately: when the world-state
/// migration (shape A) lands, this becomes a `Resource` unchanged.
#[derive(Debug, Clone)]
pub struct WeatherState {
    /// Spells the server forces clear while this counts down (`/weather clear
    /// <duration>`); during a clear spell the timers are pinned so no spell
    /// can start.
    pub(crate) clear_weather_time: i32,
    /// Ticks until the rain boolean next flips. 0 means "a flip is due *next*
    /// tick" only in the sense that the else-if branch samples a fresh spell.
    pub(crate) rain_time: i32,
    /// Ticks until the thunder boolean next flips.
    pub(crate) thunder_time: i32,
    /// Whether a thunder spell is active (`WeatherData.isThundering`).
    pub(crate) thundering: bool,
    /// Whether a rain spell is active (`WeatherData.isRaining`).
    pub(crate) raining: bool,
    /// Interpolated rain intensity, `[0, 1]`, the value the client renders.
    pub(crate) rain_level: f32,
    /// Interpolated thunder intensity, `[0, 1]`.
    pub(crate) thunder_level: f32,
    /// The level's `java.util.Random`-exact generator (`LegacyRandomSource`),
    /// used for every spell-duration draw in the same order vanilla samples
    /// them (thunder before rain, `ServerLevel.java:719-742`).
    rng: LegacyRandomSource,
}

impl Default for WeatherState {
    fn default() -> Self {
        Self::new(WEATHER_SEED)
    }
}

impl WeatherState {
    /// A fresh world's weather — the all-zero initial state `WeatherData`
    /// itself starts from (`WeatherData.java`): clear, levels at rest, and
    /// both timers at 0 so the *first* cycle samples a fresh delay. A new
    /// world therefore stays clear for roughly `RAIN_DELAY`'s 12k-180k ticks
    /// before rain can even begin.
    pub fn new(seed: u64) -> Self {
        Self {
            clear_weather_time: 0,
            rain_time: 0,
            thunder_time: 0,
            thundering: false,
            raining: false,
            rain_level: 0.0,
            thunder_level: 0.0,
            rng: LegacyRandomSource::new(seed as i64),
        }
    }

    /// Advances the cycle one tick — `ServerLevel.advanceWeatherCycle`
    /// (`ServerLevel.java:708-793`), translated exactly — and returns the
    /// transitions a client must hear about, in the order vanilla broadcasts
    /// them.
    ///
    /// `advance_weather` is the `GameRules.ADVANCE_WEATHER` gate
    /// (`ServerLevel.java:713`); it gates only the *timers* (the boolean
    /// flips). The level interpolation runs regardless, exactly as vanilla's
    /// does (`ServerLevel.java:753-768` is outside the rule check) — so with
    /// the rule off, levels still converge to whatever state the world is in,
    /// and a mid-ramp level still broadcasts.
    pub fn tick(&mut self, advance_weather: bool) -> Vec<WeatherEvent> {
        let mut events = Vec::new();
        let was_raining = self.raining;

        if advance_weather {
            if self.clear_weather_time > 0 {
                self.clear_weather_time -= 1;
                self.thunder_time = if self.thundering { 0 } else { 1 };
                self.rain_time = if self.raining { 0 } else { 1 };
                self.thundering = false;
                self.raining = false;
            } else {
                if self.thunder_time > 0 {
                    self.thunder_time -= 1;
                    if self.thunder_time == 0 {
                        self.thundering = !self.thundering;
                    }
                } else if self.thundering {
                    self.thunder_time = sample_inclusive(
                        &mut self.rng,
                        THUNDER_DURATION_MIN,
                        THUNDER_DURATION_MAX,
                    );
                } else {
                    self.thunder_time = sample_inclusive(
                        &mut self.rng,
                        THUNDER_DELAY_MIN,
                        THUNDER_DELAY_MAX,
                    );
                }

                if self.rain_time > 0 {
                    self.rain_time -= 1;
                    if self.rain_time == 0 {
                        self.raining = !self.raining;
                    }
                } else if self.raining {
                    self.rain_time = sample_inclusive(
                        &mut self.rng,
                        RAIN_DURATION_MIN,
                        RAIN_DURATION_MAX,
                    );
                } else {
                    self.rain_time =
                        sample_inclusive(&mut self.rng, RAIN_DELAY_MIN, RAIN_DELAY_MAX);
                }
            }
        }

        let old_thunder_level = self.thunder_level;
        if self.thundering {
            self.thunder_level += LEVEL_STEP;
        } else {
            self.thunder_level -= LEVEL_STEP;
        }
        self.thunder_level = self.thunder_level.clamp(0.0, 1.0);

        let old_rain_level = self.rain_level;
        if self.raining {
            self.rain_level += LEVEL_STEP;
        } else {
            self.rain_level -= LEVEL_STEP;
        }
        self.rain_level = self.rain_level.clamp(0.0, 1.0);

        // Broadcast order, `ServerLevel.java:771-793`: the two level ramps
        // first, then — only on a rain flip — the start/stop event followed
        // by a re-sent pair of level changes (vanilla re-broadcasts them
        // there unconditionally).
        if old_rain_level != self.rain_level {
            events.push(WeatherEvent::RainLevelChanged(self.rain_level));
        }
        if old_thunder_level != self.thunder_level {
            events.push(WeatherEvent::ThunderLevelChanged(self.thunder_level));
        }
        if was_raining != self.raining {
            events.push(if was_raining {
                WeatherEvent::StopRaining
            } else {
                WeatherEvent::StartRaining
            });
            events.push(WeatherEvent::RainLevelChanged(self.rain_level));
            events.push(WeatherEvent::ThunderLevelChanged(self.thunder_level));
        }

        events
    }
}

/// Inclusive uniform draw over `[min, max]` — `UniformInt.sample`'s
/// `Mth.randomBetweenInclusive` (`net/minecraft/util/random/UniformInt.java`),
/// which samples the inclusive range via `RandomSource.nextIntInclusive`.
/// `LegacyRandomSource` is `java.util.Random`-exact, so a seeded draw is
/// reproducible by hand (that is what the transition-tick test does).
fn sample_inclusive(rng: &mut LegacyRandomSource, min: i32, max: i32) -> i32 {
    min + rng.next_int_bounded(max - min + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hand-derived expected draws for [`WEATHER_SEED`], computed from
    /// the JDK's own `java.util.Random` spec (an independent Python
    /// implementation of `next(bits)`/`nextInt(bound)`) — deliberately NOT
    /// from `LegacyRandomSource`:
    ///
    /// ```text
    /// seed = 0x5EED9ABC
    /// next(31) #1 = 1468338872      next(31) #2 = 1165619973
    /// THUNDER_DURATION.sample = 3600 + (1468338872 % 12001) = 8121
    /// RAIN_DURATION.sample    = 12000 + (1165619973 % 12001) = 22847
    /// ```
    ///
    /// `nextInt(12001)` accepts on its first draw (the rejection test
    /// `u - r + (bound-1) < 0` is never true for these bounds), so each
    /// sample is exactly one 31-bit LCG draw and the two above are
    /// consecutive.
    const EXPECTED_THUNDER_DURATION_DRAW: i32 = 8121;
    const EXPECTED_RAIN_DURATION_DRAW: i32 = 22847;

    /// Pins the transition behaviour tick by tick for a known seed: the
    /// decrement, the flip exactly when a timer reaches zero, the
    /// sample-order (thunder **before** rain — swap them and the draws swap),
    /// and the ±0.01 level interpolation.
    ///
    /// Set-up is a world whose timers are at 1, so tick 1 flips *both* booleans
    /// with no draw, tick 2 samples both fresh durations, and the pinned
    /// numbers after that are the hand-derived draws above — this is the gate
    /// the plan's "expected values derived by hand from the UniformInt
    /// sampling" (a) names.
    #[test]
    fn transition_ticks_pin_the_seeded_cycle() {
        let mut s = WeatherState::new(WEATHER_SEED);
        s.thunder_time = 1;
        s.rain_time = 1;

        let tick1 = s.tick(true);
        assert!(s.raining && s.thundering, "both booleans must flip on tick 1");
        assert_eq!(s.rain_time, 0, "rain_time must reach 0 the tick it flips");
        assert_eq!(s.thunder_time, 0, "thunder_time must reach 0 the tick it flips");
        assert_eq!(s.rain_level, 0.01);
        assert_eq!(s.thunder_level, 0.01);
        assert_eq!(
            tick1,
            vec![
                WeatherEvent::RainLevelChanged(0.01),
                WeatherEvent::ThunderLevelChanged(0.01),
                WeatherEvent::StartRaining,
                WeatherEvent::RainLevelChanged(0.01),
                WeatherEvent::ThunderLevelChanged(0.01),
            ],
            "tick 1 must emit the ramp pair, the flip, and the flip's re-sent pair"
        );

        let tick2 = s.tick(true);
        assert_eq!(s.thunder_time, EXPECTED_THUNDER_DURATION_DRAW);
        assert_eq!(s.rain_time, EXPECTED_RAIN_DURATION_DRAW);
        assert_eq!(s.rain_level, 0.02);
        assert_eq!(s.thunder_level, 0.02);
        assert_eq!(
            tick2,
            vec![
                WeatherEvent::RainLevelChanged(0.02),
                WeatherEvent::ThunderLevelChanged(0.02),
            ],
            "tick 2 must only emit the ramp pair"
        );

        let tick3 = s.tick(true);
        assert_eq!(s.thunder_time, EXPECTED_THUNDER_DURATION_DRAW - 1);
        assert_eq!(s.rain_time, EXPECTED_RAIN_DURATION_DRAW - 1);
        assert_eq!(s.rain_level, 0.03);
        assert_eq!(s.thunder_level, 0.03);
        assert_eq!(
            tick3,
            vec![
                WeatherEvent::RainLevelChanged(0.03),
                WeatherEvent::ThunderLevelChanged(0.03),
            ]
        );
    }

    /// The `advance_weather` gate's negative control: with the rule **off**,
    /// the cycle must not move — timers stay put, booleans never flip, and
    /// the levels (already at rest) emit nothing. With the rule **on** from
    /// the identical starting point, that same "unchanged" claim must fail:
    /// the same one-tick assert that holds below has to break above.
    #[test]
    fn advance_weather_off_freezes_the_cycle() {
        let mut off = WeatherState::new(WEATHER_SEED);
        off.rain_time = 1;
        off.thunder_time = 1;
        let before = off.clone();
        let mut events = 0;
        for _ in 0..3 {
            events += off.tick(false).len();
        }
        assert_eq!(off.clear_weather_time, before.clear_weather_time);
        assert_eq!(off.rain_time, before.rain_time);
        assert_eq!(off.thunder_time, before.thunder_time);
        assert_eq!(off.thundering, before.thundering);
        assert_eq!(off.raining, before.raining);
        assert_eq!(off.rain_level, before.rain_level);
        assert_eq!(off.thunder_level, before.thunder_level);
        assert_eq!(events, 0, "levels are at rest and the rule is off: no broadcasts");

        // The control. Identical start, rule on: the timers move and the
        // booleans flip, so every one of the "unchanged" asserts above would
        // fail. One suffices.
        let mut on = WeatherState::new(WEATHER_SEED);
        on.rain_time = 1;
        on.thunder_time = 1;
        on.tick(true);
        assert!(on.raining, "with the rule on the same tick must flip rain");
        assert_ne!(on.rain_time, 1, "the timer must have advanced");
    }

    /// Duty-cycle band over ~1M simulated ticks: rain is on
    /// `RAIN_DURATION`-midpoint / (`RAIN_DELAY`-midpoint + `RAIN_DURATION`-midpoint)
    /// ≈ 18000 / 114000 ≈ 15.8% of ticks and thunder
    /// ≈ 9600 / 105600 ≈ 9.1% — derived from the range *midpoints*, not from
    /// the code under test.
    ///
    /// The ±4% band is wide for a reason, and the reason is *measured*, not
    /// guessed: a 1M-tick run draws only ~9 rain spells and ~13 thunder spells
    /// (each spell is one delay draw plus one duration draw, from ranges up to
    /// 168,000 ticks wide), so the sampled delay mean — and hence the duty —
    /// carries real variance. Simulated at increasing N (a diagnostic run
    /// outside the crate), this seed's thunder duty walks 11.99% → 10.28% →
    /// 10.02% → 9.23% as N goes 1M → 5M → 10M → 20M, converging on the 9.09%
    /// midpoint while the sampled THUNDER_DELAY mean (77,306 at N=1M, ~1.6 SE
    /// low) closes on the 96,000 expectation. The ±4% band is thus ~2.3 SE at
    /// N=1M: a gross-error detector — halving either range moves the duty to
    /// ~5% or ~17%, outside it — while the exact draws are pinned separately
    /// by [`transition_ticks_pin_the_seeded_cycle`] and the exact ramp by
    /// [`forced_rain_ramps_level_exactly_level_step_per_tick`].
    #[test]
    fn duty_cycle_sits_in_the_uniform_int_ranges() {
        let mut s = WeatherState::new(WEATHER_SEED);
        const N: u64 = 1_000_000;
        let mut rain_ticks: u64 = 0;
        let mut thunder_ticks: u64 = 0;
        for _ in 0..N {
            s.tick(true);
            rain_ticks += u64::from(s.raining);
            thunder_ticks += u64::from(s.thundering);
        }
        let rain_duty = rain_ticks as f64 / N as f64;
        let thunder_duty = thunder_ticks as f64 / N as f64;
        assert!(
            (rain_duty - 0.1579).abs() < 0.04,
            "rain duty {rain_duty} must sit near 15.8%"
        );
        assert!(
            (thunder_duty - 0.0909).abs() < 0.04,
            "thunder duty {thunder_duty} must sit near 9.1%"
        );
    }

    /// The wire gate (plan (c)): force rain on the way `/weather rain` does
    /// (set `raining`, leave the level to ramp), and assert the ramp is
    /// exactly `LEVEL_STEP` per tick from 0 up to the top — every event a
    /// `RAIN_LEVEL_CHANGE` (code 7) — then that the clamp stops the ramp and
    /// the emissions. `k * 0.01` in f32 drifts from the accumulated value by
    /// the accumulation's own rounding (0.9999994 at tick 100, exactly
    /// vanilla's own result — `Mth.clamp` only fires once the sum crosses
    /// 1.0), so the magnitude assert is a tight tolerance rather than an
    /// exact-equality, and the clamp-reaching assert is exact.
    #[test]
    fn forced_rain_ramps_level_exactly_level_step_per_tick() {
        let mut s = WeatherState::new(WEATHER_SEED);
        // `/weather rain <duration>`: `setRaining(true)`, `setRainTime(duration)`,
        // `setThundering(false)` — a duration long enough that no flip can
        // interrupt the 100-tick ramp.
        s.raining = true;
        s.thundering = false;
        s.rain_time = i32::MAX;
        s.thunder_time = i32::MAX;

        let mut ramps = Vec::new();
        for _ in 0..110 {
            for event in s.tick(true) {
                if let WeatherEvent::RainLevelChanged(level) = event {
                    ramps.push(level);
                }
            }
        }

        // 100 ticks of exactly one event each (the clamp engages at tick 101).
        assert!(ramps.len() >= 100, "ramp must span at least 100 ticks");
        for (i, &level) in ramps.iter().take(100).enumerate() {
            let tick = i as f32 + 1.0;
            assert_eq!(
                WeatherEvent::RainLevelChanged(level).wire(),
                (7, level),
                "every ramp step must be a RAIN_LEVEL_CHANGE carrying its level"
            );
            assert!(
                (level - tick * LEVEL_STEP).abs() < 1e-5,
                "tick {tick}: level {level} must be ~{tick} x 0.01"
            );
        }
        // Monotonic up to the clamp.
        assert!(ramps.windows(2).take(100).all(|w| w[0] <= w[1]));
        // The clamp engages once the f32 sum crosses 1.0 (tick 101), and the
        // level then holds exactly 1.0 with no further broadcasts.
        assert_eq!(s.rain_level, 1.0, "rain must have clamped at exactly 1.0");
        assert!(
            ramps.iter().all(|&level| level <= 1.0),
            "no ramp value may exceed the clamp"
        );
    }

    /// `WeatherFeed` round-trips publish → drain in FIFO order.
    #[test]
    fn weather_feed_drains_in_order() {
        let feed = WeatherFeed::default();
        assert!(feed.drain_all().is_empty());
        feed.publish(WeatherEvent::StartRaining);
        feed.publish(WeatherEvent::RainLevelChanged(0.5));
        assert_eq!(
            feed.drain_all(),
            vec![WeatherEvent::StartRaining, WeatherEvent::RainLevelChanged(0.5)]
        );
        assert!(feed.drain_all().is_empty());
    }
}
