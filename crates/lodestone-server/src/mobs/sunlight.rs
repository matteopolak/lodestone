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
//! below `2 * (brightness - 0.4)`. Rain and block light are not modelled.
//!
//! # How to change it
//!
//! Add a species to [`burns_in_sunlight`]; wire weather by passing real rain
//! and thunder levels to the darkening function.

use lodestone_model::ResourceKey;

use crate::natural_spawn::sky_darkening_for;

/// Species that catch fire in daylight.
pub(super) fn burns_in_sunlight(entity_type: &ResourceKey) -> bool {
    matches!(
        entity_type.path(),
        "zombie" | "zombie_villager" | "skeleton" | "stray" | "bogged" | "phantom"
    )
}

/// The sky darkening at `day_time` under clear weather in the overworld.
pub(super) fn darkening(day_time: i32) -> u8 {
    sky_darkening_for(crate::dimension::Dimension::Overworld, i64::from(day_time), 0.0, 0.0)
}

/// Whether it is bright outside: the sky is dimmed by less than 4.
pub(super) fn bright_outside(day_time: i32) -> bool {
    darkening(day_time) < 4
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

    #[test]
    fn noon_is_bright_and_midnight_is_not() {
        assert!(bright_outside(6000));
        assert!(!bright_outside(18000));
        // Darkening is 5 at tick 12800, past the bright limit of 3.
        assert!(!bright_outside(12800));
    }
}
