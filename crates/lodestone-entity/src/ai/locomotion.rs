//! Horizontal mob locomotion: how a speed becomes blocks per tick.
//!
//! A mob's move control sets its forward input equal to its speed, and the
//! travel step scales that input by the speed again, so the thrust added to the
//! velocity each tick is the *square* of `modifier * movement_speed`. Velocity
//! then decays by the medium's drag, so the cruising rate is
//! `thrust / (1 - drag)`: `(modifier * movement_speed)^2 / (1 - 0.6 * 0.91)` on
//! stone. This module is the only place those constants live.

/// Friction at or below this leaves ground thrust unscaled; above it the thrust
/// is divided by the cube of the friction so slippery floors do not outrun the
/// 0.6 baseline.
const SLIPPERY_THRESHOLD: f32 = 0.6;
const SLIPPERY_NUMERATOR: f32 = 0.216_000_02;
const AIR_DRAG: f32 = 0.91;
const WATER_DRAG: f32 = 0.8;
const LAVA_DRAG: f64 = 0.5;
/// Thrust scale for a body that is airborne or in a fluid.
const UNGROUNDED_THRUST: f64 = 0.02;

/// What a body is moving through this tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Medium {
    /// Standing on a block of the given friction.
    Ground {
        /// The block's friction.
        friction: f32,
    },
    /// Neither supported nor submerged.
    Air,
    /// Feet in water.
    Water,
    /// Feet in lava.
    Lava,
}

/// Horizontal thrust added to the velocity this tick when the move control is
/// driving at `speed` (`modifier * movement_speed`).
#[must_use]
pub fn thrust(speed: f64, medium: Medium) -> f64 {
    // Input magnitude saturates at one; speeds above it are not normal mobs.
    let input = speed.min(1.0);
    let scale = match medium {
        Medium::Ground { friction } => {
            let speed = speed as f32;
            f64::from(if friction > SLIPPERY_THRESHOLD {
                speed * (SLIPPERY_NUMERATOR / (friction * friction * friction))
            } else {
                speed
            })
        }
        Medium::Air | Medium::Water | Medium::Lava => UNGROUNDED_THRUST,
    };
    scale * input
}

/// Per-tick horizontal velocity retention in `medium`, before the block's speed
/// factor.
#[must_use]
pub fn drag(medium: Medium) -> f64 {
    match medium {
        Medium::Ground { friction } => f64::from(friction * AIR_DRAG),
        Medium::Air => f64::from(AIR_DRAG),
        Medium::Water => f64::from(WATER_DRAG),
        Medium::Lava => LAVA_DRAG,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the per-tick update `v' = (v + thrust) * factor * drag` to
    /// convergence and returns the distance moved in the last tick.
    fn cruise(speed: f64, medium: Medium, speed_factor: f64) -> f64 {
        let mut v = 0.0;
        let mut step = 0.0;
        for _ in 0..400 {
            step = v + thrust(speed, medium);
            v = step * speed_factor * drag(medium);
        }
        step
    }

    #[test]
    fn stone_cruise_matches_the_closed_form_and_squares_the_modifier() {
        let stone = Medium::Ground { friction: 0.6 };
        // A zombie at modifier 1.0 and a panicking cow at modifier 2.0.
        for (attribute, modifier) in [(0.23, 1.0), (0.2, 2.0), (0.2, 1.0), (0.2, 0.5)] {
            let s: f64 = attribute * modifier;
            let expected = s * s / (1.0 - 0.6 * 0.91);
            let got = cruise(s, stone, 1.0);
            assert!((got - expected).abs() < 1e-6, "{attribute} x {modifier}: {got} vs {expected}");
        }
        // Doubling the modifier quadruples the pace; halving quarters it.
        let base = cruise(0.2, stone, 1.0);
        assert!((cruise(0.4, stone, 1.0) / base - 4.0).abs() < 1e-6);
        assert!((cruise(0.1, stone, 1.0) / base - 0.25).abs() < 1e-6);
    }

    #[test]
    fn ice_thrust_is_scaled_by_the_cube_of_friction_but_drag_is_not() {
        let ice = Medium::Ground { friction: 0.98 };
        let s: f64 = 0.2;
        let expected = s * (0.216 / 0.98_f64.powi(3)) * s / (1.0 - 0.98 * 0.91);
        assert!((cruise(s, ice, 1.0) - expected).abs() < 1e-5);
    }

    #[test]
    fn soul_sand_factor_slows_the_cruise() {
        let stone = Medium::Ground { friction: 0.6 };
        let s: f64 = 0.2;
        let expected = s * s / (1.0 - 0.4 * 0.6 * 0.91);
        assert!((cruise(s, stone, 0.4) - expected).abs() < 1e-6);
    }

    #[test]
    fn fluids_and_air_use_the_fixed_thrust_and_their_own_drag() {
        let s: f64 = 0.2;
        assert!((cruise(s, Medium::Water, 1.0) - 0.02 * s / (1.0 - 0.8)).abs() < 1e-6);
        assert!((cruise(s, Medium::Lava, 1.0) - 0.02 * s / (1.0 - 0.5)).abs() < 1e-6);
        assert!((cruise(s, Medium::Air, 1.0) - 0.02 * s / (1.0 - 0.91)).abs() < 1e-5);
    }
}
