//! Daylight burning for undead: when a mob catches fire in the sun.
//!
//! # What it is
//!
//! The chance each tick that a sun-sensitive, bareheaded mob under open sky
//! in bright daylight is set alight for 8 seconds, plus the sky-brightness
//! arithmetic behind it.
//!
//! # How it works
//!
//! Sky light under open sky is 15 less the sky darkening
//! ([`crate::natural_spawn::sky_darkening_for`]). The brightness curve is
//! `v / (4 - 3v)` with `v = light / 15`; the sun only bites when that exceeds
//! 0.5 (light 13 or more), and then on a tick whose uniform roll times 30 is
//! below `2 * (brightness - 0.4)`. Rain darkens the sky, and a mob under open
//! sky in rain is wet, which puts out and prevents the fire. Block light and
//! the biome's precipitation type are not modelled.
//!
//! # How to change it
//!
//! Add a species to [`burns_in_sunlight`]; the host feeds the dimension and
//! weather through `MobSim::set_environment`.

use lodestone_model::ResourceKey;

use crate::dimension::Dimension;
use crate::natural_spawn::sky_darkening_for;

/// Species that catch fire in daylight.
pub(super) fn burns_in_sunlight(entity_type: &ResourceKey) -> bool {
    matches!(
        entity_type.path(),
        "zombie" | "zombie_villager" | "skeleton" | "stray" | "bogged" | "phantom"
    )
}

/// The dimension and weather the sky is read in.
#[derive(Debug, Clone, Copy)]
pub(super) struct Sky {
    pub(super) dimension: Dimension,
    pub(super) rain_level: f32,
    pub(super) thunder_level: f32,
}

impl Sky {
    /// Clear overworld weather, until the host says otherwise.
    pub(super) const fn clear() -> Self {
        Self { dimension: Dimension::Overworld, rain_level: 0.0, thunder_level: 0.0 }
    }

    /// The sky darkening at `day_time`.
    pub(super) fn darkening(self, day_time: i32) -> u8 {
        sky_darkening_for(self.dimension, i64::from(day_time), self.rain_level, self.thunder_level)
    }

    /// Whether it is bright outside: the sky is dimmed by less than 4.
    pub(super) fn bright_outside(self, day_time: i32) -> bool {
        self.darkening(day_time) < 4
    }

    /// Whether rain is falling hard enough to wet a mob under open sky.
    pub(super) fn raining(self) -> bool {
        self.rain_level > 0.2
    }

    /// Whether bees stay in their hives: any rain at all, or the overworld
    /// night from tick 12542 up to 23460.
    pub(super) fn bees_stay_in_hive(self, day_time: i32) -> bool {
        let night = self.dimension == Dimension::Overworld && (12542..23460).contains(&day_time.rem_euclid(24_000));
        night || self.rain_level > 0.0 || self.thunder_level > 0.0
    }
}

/// The per-tick chance of catching fire under open sky at this sky darkening.
pub(super) fn ignite_chance(darkening: u8) -> f32 {
    let v = f32::from(15u8.saturating_sub(darkening)) / 15.0;
    let brightness = v / (4.0 - 3.0 * v);
    if brightness > 0.5 { (brightness - 0.4) * 2.0 / 30.0 } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand arithmetic: darkening 0 gives v 1 and brightness 1, so 1.2/30; darkening 2
    /// gives v 13/15, brightness 0.619048, so 0.438095/30; darkening 3 gives
    /// brightness 0.5 up to single-precision rounding (0.5000001 in the
    /// reference's float arithmetic, so a 0.0067 chance), and darkening 4 gives
    /// 0.4074, below the 0.5 floor.
    #[test]
    fn the_ignite_chance_follows_the_brightness_curve() {
        assert!((ignite_chance(0) - 0.04).abs() < 1e-6);
        assert!((ignite_chance(2) - 0.014_603).abs() < 1e-5);
        assert!((ignite_chance(3) - 0.006_667).abs() < 1e-5);
        assert_eq!(ignite_chance(4), 0.0);
        assert_eq!(ignite_chance(11), 0.0);
    }

    /// The attribute turns on at 12542 and off at 23460 (a day is 24000 ticks), so
    /// the tick before each edge is outside and the edge itself is inside the
    /// night, and any rain keeps bees in at noon.
    #[test]
    fn bees_stay_in_from_tick_12542_to_23459_and_whenever_it_rains() {
        let clear = Sky::clear();
        for (tick, stays) in [(0, false), (12541, false), (12542, true), (18000, true), (23459, true), (23460, false), (24000 + 12542, true)] {
            assert_eq!(clear.bees_stay_in_hive(tick), stays, "tick {tick}");
        }
        let drizzle = Sky { rain_level: 0.05, ..Sky::clear() };
        assert!(drizzle.bees_stay_in_hive(6000));
        let nether = Sky { dimension: Dimension::Nether, ..Sky::clear() };
        assert!(!nether.bees_stay_in_hive(18000), "only the overworld has the night");
    }

    #[test]
    fn noon_is_bright_and_midnight_is_not() {
        let sky = Sky::clear();
        assert!(sky.bright_outside(6000));
        assert!(!sky.bright_outside(18000));
        // Darkening is 5 at tick 12800, past the bright limit of 3.
        assert!(!sky.bright_outside(12800));
    }

    /// At noon the light is 15. Full rain alone pulls it by `0.3125 * (4 - 15)`
    /// to 11.5625, a darkening of 3 (still bright); a full thunderstorm then
    /// pulls it by `0.52734375 * (4 - 15)` to 9.2, a darkening of 5.
    #[test]
    fn rain_alone_keeps_noon_bright_and_a_thunderstorm_does_not() {
        let rain = Sky { rain_level: 1.0, ..Sky::clear() };
        assert_eq!(rain.darkening(6000), 3);
        assert!(rain.bright_outside(6000));
        let storm = Sky { rain_level: 1.0, thunder_level: 1.0, ..Sky::clear() };
        assert_eq!(storm.darkening(6000), 5);
        assert!(!storm.bright_outside(6000));
    }
}
