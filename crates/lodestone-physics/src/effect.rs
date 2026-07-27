//! Canonical mapping from a mob-effect id to its **movement** consequence.
//!
//! The clientbound `update_mob_effect` / `remove_mob_effect` packets decode (in
//! the version adapters) into canonical effect ids — e.g. `minecraft:levitation`,
//! `minecraft:speed` — plus an amplifier. That data is only useful to physics if
//! something routes it to the right place, and vanilla splits the
//! movement-relevant effects across **two** destinations:
//!
//! 1. **Direct-read effects** — Levitation, Slow Falling, Dolphin's Grace and
//!    Jump Boost are read straight out of the effect map by the movement
//!    integrator (`travel`/`getJumpPower`/…), *not* through the attribute system.
//!    These fold into [`StatusEffects`] via [`StatusEffects::apply`] /
//!    [`StatusEffects::remove`] and are consumed by [`crate::player::tick`].
//! 2. **`MOVEMENT_SPEED` attribute modifiers** — Speed (`+0.2F`) and Slowness
//!    (`-0.15F`) are `ADD_MULTIPLIED_TOTAL` modifiers on the `MOVEMENT_SPEED`
//!    attribute (`MobEffects.SPEED`/`SLOWNESS`). They do **not** live in
//!    [`StatusEffects`]; the entity layer must add them to its
//!    `MOVEMENT_SPEED` `AttributeInstance`, whose folded value then reaches
//!    physics via [`PlayerState::movement_speed`]. [`movement_speed_modifier`]
//!    hands back the exact modifier amount so the entity layer never re-derives
//!    the constant, but physics deliberately does not perform the attribute fold
//!    (`calculateValue`) itself — that boundary is owned by `lodestone-entity`.
//!
//! This module is the single authoritative classifier so that a decoded packet
//! can never terminate one crate short of the integrator: [`classify`] answers
//! "does this effect change movement, and by which path?" and everything else is
//! derived from it.
//!
//! [`StatusEffects`]: crate::player::StatusEffects
//! [`PlayerState::movement_speed`]: crate::player::PlayerState::movement_speed

use crate::player::StatusEffects;

/// A direct-read movement effect and its 0-based amplifier (level I = `0`).
///
/// These are the effects the movement integrator reads out of the effect map by
/// hand, mirrored as fields on [`StatusEffects`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectEffect {
    /// `minecraft:levitation` — replaces gravity with a pull toward
    /// `0.05 * (amp + 1)` in `travelInAir`.
    Levitation(u32),
    /// `minecraft:slow_falling` — caps effective gravity at `0.01` while falling.
    SlowFalling,
    /// `minecraft:dolphins_grace` — pins the in-water horizontal slow-down to
    /// `0.96F`.
    DolphinsGrace,
    /// `minecraft:jump_boost` — adds `0.1F * (amp + 1)` to jump velocity.
    JumpBoost(u32),
}

/// How a mob effect influences player movement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MovementEffect {
    /// A direct-read effect; fold it into [`StatusEffects`].
    Direct(DirectEffect),
    /// A `MOVEMENT_SPEED` attribute modifier with operation
    /// `ADD_MULTIPLIED_TOTAL`. `amount` is `base * (amplifier + 1)` in `f64`,
    /// with `base` the widened `float` literal (`0.2F` for Speed, `-0.15F` for
    /// Slowness) — matching `AttributeModifier.create` in vanilla. The entity
    /// layer adds this to its `MOVEMENT_SPEED` attribute; physics does not fold
    /// it here.
    MovementSpeed { amount: f64 },
    /// The effect does not affect movement.
    None,
}

/// `MobEffects.SPEED`: `MOVEMENT_SPEED` `+0.2F`, `ADD_MULTIPLIED_TOTAL`.
///
/// Stored as a `float` literal in the source and widened to `double` in the
/// modifier's `amount` field, so the widening is reproduced with `f64::from`.
const SPEED_MOVEMENT_SPEED_BASE: f32 = 0.2;
/// `MobEffects.SLOWNESS`: `MOVEMENT_SPEED` `-0.15F`, `ADD_MULTIPLIED_TOTAL`.
const SLOWNESS_MOVEMENT_SPEED_BASE: f32 = -0.15;

/// Strips an optional `minecraft:` namespace, so both the canonical
/// `minecraft:speed` and a bare `speed` resolve identically.
fn effect_path(effect_id: &str) -> &str {
    effect_id.strip_prefix("minecraft:").unwrap_or(effect_id)
}

/// Classifies a mob effect's movement consequence from its canonical id and
/// 0-based amplifier.
///
/// The id may be namespaced (`minecraft:speed`) or bare (`speed`). Every effect
/// that does not change movement — including purely cosmetic or combat effects —
/// maps to [`MovementEffect::None`].
#[must_use]
pub fn classify(effect_id: &str, amplifier: u32) -> MovementEffect {
    match effect_path(effect_id) {
        "levitation" => MovementEffect::Direct(DirectEffect::Levitation(amplifier)),
        "slow_falling" => MovementEffect::Direct(DirectEffect::SlowFalling),
        "dolphins_grace" => MovementEffect::Direct(DirectEffect::DolphinsGrace),
        "jump_boost" => MovementEffect::Direct(DirectEffect::JumpBoost(amplifier)),
        "speed" => MovementEffect::MovementSpeed {
            amount: f64::from(SPEED_MOVEMENT_SPEED_BASE) * f64::from(amplifier.saturating_add(1)),
        },
        "slowness" => MovementEffect::MovementSpeed {
            amount: f64::from(SLOWNESS_MOVEMENT_SPEED_BASE)
                * f64::from(amplifier.saturating_add(1)),
        },
        _ => MovementEffect::None,
    }
}

/// The `MOVEMENT_SPEED` attribute modifier amount a mob effect contributes, if
/// any (operation is always `ADD_MULTIPLIED_TOTAL`).
///
/// `amount` is `base * (amplifier + 1)` in `f64`, with `base` the widened
/// `float` literal, matching vanilla's `AttributeModifier.create`. Returns
/// `None` for effects that do not modify `MOVEMENT_SPEED` (including the
/// direct-read effects). The entity layer adds the returned amount to its
/// `MOVEMENT_SPEED` `AttributeInstance`; physics never folds it.
#[must_use]
pub fn movement_speed_modifier(effect_id: &str, amplifier: u32) -> Option<f64> {
    match classify(effect_id, amplifier) {
        MovementEffect::MovementSpeed { amount } => Some(amount),
        _ => None,
    }
}

impl StatusEffects {
    /// Folds a mob-effect application (`update_mob_effect`) into these effects,
    /// keyed by canonical id (namespaced or bare) and 0-based amplifier.
    ///
    /// Only the four direct-read movement effects (Levitation, Slow Falling,
    /// Dolphin's Grace, Jump Boost) are recognised. Everything else — including
    /// Speed / Slowness, which are `MOVEMENT_SPEED` attribute modifiers handled
    /// by [`movement_speed_modifier`] — leaves `self` unchanged. Returns `true`
    /// iff a direct-read field changed as a result.
    pub fn apply(&mut self, effect_id: &str, amplifier: u32) -> bool {
        match classify(effect_id, amplifier) {
            MovementEffect::Direct(DirectEffect::Levitation(amp)) => {
                self.levitation = Some(amp);
                true
            }
            MovementEffect::Direct(DirectEffect::SlowFalling) => {
                self.slow_falling = true;
                true
            }
            MovementEffect::Direct(DirectEffect::DolphinsGrace) => {
                self.dolphins_grace = true;
                true
            }
            MovementEffect::Direct(DirectEffect::JumpBoost(amp)) => {
                self.jump_boost = Some(amp);
                true
            }
            MovementEffect::MovementSpeed { .. } | MovementEffect::None => false,
        }
    }

    /// Folds a mob-effect removal (`remove_mob_effect`) into these effects.
    ///
    /// Clears the matching direct-read field. Returns `true` iff a field that
    /// was set is now cleared (so a no-op removal of an inactive effect reports
    /// `false`).
    pub fn remove(&mut self, effect_id: &str) -> bool {
        match effect_path(effect_id) {
            "levitation" => self.levitation.take().is_some(),
            "slow_falling" => core::mem::replace(&mut self.slow_falling, false),
            "dolphins_grace" => core::mem::replace(&mut self.dolphins_grace, false),
            "jump_boost" => self.jump_boost.take().is_some(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collision::CollisionView;
    use crate::geometry::{Aabb, Vec3d};
    use crate::player::{MovementInput, PlayerState, tick};
    use crate::profile::PhysicsProfile;

    #[test]
    fn classify_direct_effects_namespaced_and_bare() {
        assert_eq!(
            classify("minecraft:levitation", 2),
            MovementEffect::Direct(DirectEffect::Levitation(2))
        );
        assert_eq!(
            classify("levitation", 2),
            MovementEffect::Direct(DirectEffect::Levitation(2))
        );
        assert_eq!(
            classify("minecraft:slow_falling", 0),
            MovementEffect::Direct(DirectEffect::SlowFalling)
        );
        assert_eq!(
            classify("minecraft:dolphins_grace", 0),
            MovementEffect::Direct(DirectEffect::DolphinsGrace)
        );
        assert_eq!(
            classify("minecraft:jump_boost", 1),
            MovementEffect::Direct(DirectEffect::JumpBoost(1))
        );
    }

    #[test]
    fn unknown_and_non_movement_effects_are_none() {
        // A real effect that does not touch movement, plus a nonsense id.
        assert_eq!(classify("minecraft:night_vision", 0), MovementEffect::None);
        assert_eq!(classify("minecraft:regeneration", 3), MovementEffect::None);
        assert_eq!(classify("not_an_effect", 0), MovementEffect::None);
        assert_eq!(movement_speed_modifier("minecraft:levitation", 0), None);
    }

    #[test]
    fn speed_modifier_is_widened_float_times_level() {
        // Vanilla: AttributeModifier.create → amount * (amplifier + 1), with
        // amount the double-widened float literal 0.2F. The widening is
        // observable: 0.2f64 != f64::from(0.2f32).
        let amp0 = movement_speed_modifier("minecraft:speed", 0).unwrap();
        assert_eq!(amp0.to_bits(), f64::from(0.2f32).to_bits());
        assert_ne!(amp0.to_bits(), 0.2f64.to_bits());

        let amp1 = movement_speed_modifier("minecraft:speed", 1).unwrap();
        assert_eq!(amp1.to_bits(), (f64::from(0.2f32) * 2.0).to_bits());
    }

    #[test]
    fn slowness_modifier_is_negative_widened_float_times_level() {
        let amp0 = movement_speed_modifier("minecraft:slowness", 0).unwrap();
        assert_eq!(amp0.to_bits(), f64::from(-0.15f32).to_bits());
        assert!(amp0 < 0.0);
        let amp2 = movement_speed_modifier("slowness", 2).unwrap();
        assert_eq!(amp2.to_bits(), (f64::from(-0.15f32) * 3.0).to_bits());
    }

    #[test]
    fn apply_and_remove_round_trip_on_status_effects() {
        let mut e = StatusEffects::default();
        assert!(e.apply("minecraft:levitation", 3));
        assert_eq!(e.levitation, Some(3));
        assert!(e.apply("minecraft:jump_boost", 1));
        assert_eq!(e.jump_boost, Some(1));
        assert!(e.apply("slow_falling", 0));
        assert!(!e.dolphins_grace);
        assert!(e.apply("dolphins_grace", 0));
        assert!(e.dolphins_grace);

        // Speed/Slowness are attribute-path effects: apply() must not touch
        // StatusEffects and must report "not a direct-read change".
        let before = e;
        assert!(!e.apply("minecraft:speed", 4));
        assert!(!e.apply("minecraft:slowness", 0));
        assert_eq!(e, before);

        assert!(e.remove("minecraft:levitation"));
        assert_eq!(e.levitation, None);
        // Removing an already-inactive effect is a no-op that reports false.
        assert!(!e.remove("minecraft:levitation"));
        assert!(!e.remove("minecraft:speed"));
    }

    /// End-to-end seam proof: routing a decoded effect *id* through
    /// [`StatusEffects::apply`] reaches the integrator and changes motion — so
    /// the mapping is not "a struct nobody reads". Levitation's rise is already
    /// covered by `player::tests::levitation_makes_player_rise`; here the effect
    /// arrives as the wire id rather than a hand-set field.
    #[test]
    fn applying_levitation_by_id_makes_player_rise() {
        struct Air;
        impl CollisionView for Air {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        }
        let p = PhysicsProfile::mc_1_21();
        let mut effects = StatusEffects::default();
        assert!(effects.apply("minecraft:levitation", 0));
        let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0).with_effects(effects);
        for _ in 0..40 {
            tick(&mut s, MovementInput::NONE, &Air, &p);
        }
        assert!(s.position.y > 100.5, "levitation-by-id y = {}", s.position.y);
    }

    /// End-to-end seam proof for the attribute path: folding a Speed modifier
    /// into `MOVEMENT_SPEED` (as the entity layer would, via a single
    /// `ADD_MULTIPLIED_TOTAL` step: `base * (1 + amount)`) and handing the result
    /// to [`PlayerState::with_movement_speed`] yields more horizontal travel than
    /// the unmodified base — demonstrating the modifier reaches the integrator.
    #[test]
    fn speed_modifier_folded_into_movement_speed_walks_faster() {
        struct Floor;
        impl CollisionView for Floor {
            fn collision_boxes(&self, _x: i32, y: i32, _z: i32, out: &mut Vec<Aabb>) {
                if y == 0 {
                    out.push(Aabb::new(-64.0, 0.0, -64.0, 64.0, 1.0, 64.0));
                }
            }
        }
        let p = PhysicsProfile::mc_1_21();
        let base_speed = f64::from(p.base_movement_speed); // 0.1F widened

        let walk = |movement_speed: f64| -> f64 {
            let mut s = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0)
                .with_movement_speed(movement_speed);
            s.on_ground = true;
            let input = MovementInput {
                forward: 1.0,
                ..MovementInput::NONE
            };
            for _ in 0..20 {
                tick(&mut s, input, &Floor, &p);
            }
            s.position.z - 0.5
        };

        let amount = movement_speed_modifier("minecraft:speed", 0).unwrap();
        let boosted = base_speed * (1.0 + amount); // ADD_MULTIPLIED_TOTAL fold
        let plain_dist = walk(base_speed);
        let boosted_dist = walk(boosted);
        assert!(
            boosted_dist > plain_dist,
            "speed should walk further: boosted={boosted_dist} plain={plain_dist}"
        );
    }
}
