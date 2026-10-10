//! The charge profile of a spear: how long it winds up, and how fast the wielder
//! and target must be moving for a stab to hurt, knock back or unseat.

/// A time limit and the speeds a stab needs inside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Condition {
    /// Ticks after the wind-up during which the condition can hold.
    pub max_ticks: i32,
    /// Wielder speed along the stab direction, in blocks per second.
    pub min_speed: f32,
    /// Wielder speed relative to the target, in blocks per second.
    pub min_relative_speed: f32,
}

impl Condition {
    /// Whether a stab `ticks_used` ticks after wind-up meets the condition;
    /// `factor` scales both minimum speeds (0.2 for a mob).
    #[must_use]
    pub fn test(&self, ticks_used: i32, speed: f64, relative_speed: f64, factor: f64) -> bool {
        ticks_used <= self.max_ticks
            && speed >= f64::from(self.min_speed) * factor
            && relative_speed >= f64::from(self.min_relative_speed) * factor
    }
}

/// A spear's charge profile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KineticSpear {
    /// Ticks of wind-up before a stab can land.
    pub delay_ticks: i32,
    /// Extra damage per block per second of relative speed.
    pub damage_multiplier: f32,
    /// When a stab hurts.
    pub damage: Condition,
    /// When a stab knocks the target back.
    pub knockback: Condition,
}

impl KineticSpear {
    /// Ticks of use from the start of the charge until the damage window closes.
    #[must_use]
    pub fn use_duration(&self) -> i32 {
        self.delay_ticks + self.damage.max_ticks
    }
}

fn spear(
    multiplier: f32,
    delay: f32,
    knockback: (f32, f32),
    damage: (f32, f32),
) -> KineticSpear {
    KineticSpear {
        delay_ticks: (delay * 20.0) as i32,
        damage_multiplier: multiplier,
        damage: Condition { max_ticks: (damage.0 * 20.0) as i32, min_speed: 0.0, min_relative_speed: damage.1 },
        knockback: Condition { max_ticks: (knockback.0 * 20.0) as i32, min_speed: knockback.1, min_relative_speed: 0.0 },
    }
}

/// The charge profile of a spear item, or `None` for any other item. The
/// numbers are the spear definitions in the reference item registry.
#[must_use]
pub fn kinetic_spear(item: &str) -> Option<KineticSpear> {
    let name = item.strip_prefix("minecraft:").unwrap_or(item);
    Some(match name {
        "wooden_spear" => spear(0.7, 0.75, (10.0, 5.1), (15.0, 4.6)),
        "stone_spear" => spear(0.82, 0.7, (9.0, 5.1), (13.75, 4.6)),
        "copper_spear" => spear(0.82, 0.65, (8.25, 5.1), (12.5, 4.6)),
        "iron_spear" => spear(0.95, 0.6, (6.75, 5.1), (11.25, 4.6)),
        "golden_spear" => spear(0.7, 0.7, (8.5, 5.1), (13.75, 4.6)),
        "diamond_spear" => spear(1.075, 0.5, (6.5, 5.1), (10.0, 4.6)),
        "netherite_spear" => spear(1.2, 0.4, (5.5, 5.1), (8.75, 4.6)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iron_spear_numbers() {
        let s = kinetic_spear("minecraft:iron_spear").expect("iron spear");
        assert_eq!(s.delay_ticks, 12);
        assert_eq!(s.damage.max_ticks, 225);
        assert_eq!(s.use_duration(), 237);
        assert!(kinetic_spear("iron_sword").is_none());
    }

    #[test]
    fn a_mob_needs_a_fifth_of_the_relative_speed() {
        let s = kinetic_spear("iron_spear").expect("iron spear");
        assert!(s.damage.test(10, 0.0, 4.6 * 0.2 + 0.01, 0.2));
        assert!(!s.damage.test(10, 0.0, 4.6 * 0.2 - 0.01, 0.2));
        assert!(!s.damage.test(300, 0.0, 10.0, 0.2), "too late");
    }
}
