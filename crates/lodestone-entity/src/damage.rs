//! The damage-reduction pipeline and the invulnerability-frame gate.
//!
//! When something hurts a living entity, vanilla runs a fixed sequence and the
//! **order is load-bearing** — armour, then the Resistance effect, then
//! enchantment protection, then absorption hearts, then health. Reorder any two
//! and you get a number that is close enough to look right and wrong enough to
//! matter (a diamond-armour player surviving a hit they should not, or dying to
//! one they should tank). The formulas here are `CombatRules` and
//! `LivingEntity.actuallyHurt`, reproduced by behaviour and pinned with
//! known-value tests.
//!
//! Two things are deliberately **not** here:
//!   * **Knockback impulse.** `impl-physics` builds the knockback velocity from
//!     the other side; this crate only decides *whether* a hit lands and *how
//!     much* it hurts. Coordinate the impulse through the project owner rather
//!     than growing a second model.
//!   * **Damage *sources* and their tags.** Which hits bypass armour, cooldown
//!     or resistance is registry data; a caller passes the relevant [`DamageFlags`]
//!     rather than this crate hardcoding a version's damage-type table.

/// Per-hit flags a caller derives from the damage source's type tags. Each one
/// switches off a stage of the pipeline, matching the `DamageTypeTags` checks in
/// `LivingEntity`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DamageFlags {
    /// `BYPASSES_ARMOR`: skip the armour-absorb stage (e.g. fall, magic, void).
    pub bypasses_armor: bool,
    /// `BYPASSES_EFFECTS`: skip both Resistance and enchantment protection.
    pub bypasses_effects: bool,
    /// `BYPASSES_RESISTANCE`: skip only the Resistance effect stage.
    pub bypasses_resistance: bool,
    /// `BYPASSES_ENCHANTMENTS`: skip only the enchantment-protection stage.
    pub bypasses_enchantments: bool,
    /// `BYPASSES_COOLDOWN`: ignore the invulnerability-frame gate.
    pub bypasses_cooldown: bool,
}

/// Maximum armour points that count (`CombatRules.MAX_ARMOR`).
pub const MAX_ARMOR: f32 = 20.0;
/// Armour protection divider (`CombatRules.ARMOR_PROTECTION_DIVIDER`).
pub const ARMOR_PROTECTION_DIVIDER: f32 = 25.0;
/// The invulnerability window a full hit sets, in ticks.
pub const INVULNERABILITY_TICKS: i32 = 20;

/// Damage after armour absorption, `CombatRules.getDamageAfterAbsorb` — the
/// toughness-aware armour formula. `enchant_effectiveness` is the weapon's
/// armour-effectiveness multiplier in `0.0..=1.0` (1.0 = no piercing).
#[must_use]
pub fn damage_after_armor(
    damage: f32,
    total_armor: f32,
    armor_toughness: f32,
    enchant_effectiveness: f32,
) -> f32 {
    let toughness = 2.0 + armor_toughness / 4.0;
    let real_armor = (total_armor - damage / toughness).clamp(total_armor * 0.2, 20.0);
    let armor_fraction = real_armor / ARMOR_PROTECTION_DIVIDER;
    let modified = (armor_fraction * enchant_effectiveness).clamp(0.0, 1.0);
    damage * (1.0 - modified)
}

/// Damage after the Resistance mob effect. `amplifier` is the effect amplifier
/// (level − 1); each level cuts 20% (`5 * (amp + 1)` out of 25).
#[must_use]
pub fn damage_after_resistance(damage: f32, amplifier: i32) -> f32 {
    let absorb_value = (amplifier + 1) * 5;
    let absorb = 25 - absorb_value;
    (damage * absorb as f32 / 25.0).max(0.0)
}

/// Damage after enchantment protection, `CombatRules.getDamageAfterMagicAbsorb`.
/// `protection` is the summed enchantment protection value (EPF-derived), capped
/// at 20.
#[must_use]
pub fn damage_after_protection(damage: f32, protection: f32) -> f32 {
    let real = protection.clamp(0.0, 20.0);
    damage * (1.0 - real / ARMOR_PROTECTION_DIVIDER)
}

/// A living entity's defensive state for one incoming hit. Enchantment
/// protection and Resistance amplifier are supplied per-call because they are
/// registry/effect-derived.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Defenses {
    /// Total armour points.
    pub armor: f32,
    /// Armour toughness attribute value.
    pub armor_toughness: f32,
    /// Resistance amplifier, or `None` if the effect is absent.
    pub resistance_amplifier: Option<i32>,
    /// Summed enchantment protection value.
    pub enchant_protection: f32,
    /// Weapon armour-effectiveness multiplier (`1.0` = attacker has no piercing).
    pub enchant_effectiveness: f32,
    /// Current absorption (yellow) hearts.
    pub absorption: f32,
}

impl Default for Defenses {
    fn default() -> Self {
        Self {
            armor: 0.0,
            armor_toughness: 0.0,
            resistance_amplifier: None,
            enchant_protection: 0.0,
            enchant_effectiveness: 1.0,
            absorption: 0.0,
        }
    }
}

/// The result of running the reduction pipeline for one hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DamageOutcome {
    /// Damage that reaches health after every reduction and absorption.
    pub to_health: f32,
    /// Absorption hearts consumed by this hit.
    pub absorbed: f32,
    /// Absorption hearts remaining afterward.
    pub remaining_absorption: f32,
}

/// Runs the full reduction pipeline in vanilla order: armour → Resistance →
/// enchantment protection → absorption. Returns how much reaches health and how
/// the absorption pool changed (`LivingEntity.actuallyHurt`).
#[must_use]
pub fn apply_reductions(damage: f32, defenses: &Defenses, flags: DamageFlags) -> DamageOutcome {
    let mut dmg = damage;

    if !flags.bypasses_armor {
        dmg = damage_after_armor(
            dmg,
            defenses.armor,
            defenses.armor_toughness,
            defenses.enchant_effectiveness,
        );
    }

    if !flags.bypasses_effects {
        if let Some(amp) = defenses.resistance_amplifier
            && !flags.bypasses_resistance
        {
            dmg = damage_after_resistance(dmg, amp);
        }
        if dmg > 0.0 && !flags.bypasses_enchantments && defenses.enchant_protection > 0.0 {
            dmg = damage_after_protection(dmg, defenses.enchant_protection);
        }
    }

    // Absorption hearts soak the remainder before health.
    let after_absorb = (dmg - defenses.absorption).max(0.0);
    let absorbed = dmg - after_absorb;
    DamageOutcome {
        to_health: after_absorb,
        absorbed,
        remaining_absorption: defenses.absorption - absorbed,
    }
}

/// The per-entity invulnerability-frame state that gates rapid re-hits
/// (`invulnerableTime` / `lastHurt`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HurtCooldown {
    /// Ticks of invulnerability remaining.
    pub invulnerable_time: i32,
    /// The damage of the hit that opened the current window.
    pub last_hurt: f32,
}

/// What the i-frame gate decided for an incoming hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HurtDecision {
    /// The hit is fully ignored (still inside i-frames and no stronger).
    Ignored,
    /// A full hit: apply `amount`, opening a fresh 20-tick window.
    Full { amount: f32 },
    /// A hit that beats the current window's `last_hurt`: only the *difference*
    /// `amount` is applied and no new window opens.
    Topup { amount: f32 },
}

impl HurtCooldown {
    /// Decrements the window one tick (call from the entity's base tick).
    pub fn tick(&mut self) {
        if self.invulnerable_time > 0 {
            self.invulnerable_time -= 1;
        }
    }

    /// Applies the vanilla i-frame gate for an incoming `damage` (this is the
    /// *raw* damage compared before reductions, exactly as `LivingEntity.hurt`).
    /// Mutates the cooldown and returns what should happen.
    pub fn on_hurt(&mut self, damage: f32, flags: DamageFlags) -> HurtDecision {
        if self.invulnerable_time > 10 && !flags.bypasses_cooldown {
            if damage <= self.last_hurt {
                return HurtDecision::Ignored;
            }
            let delta = damage - self.last_hurt;
            self.last_hurt = damage;
            HurtDecision::Topup { amount: delta }
        } else {
            self.last_hurt = damage;
            self.invulnerable_time = INVULNERABILITY_TICKS;
            HurtDecision::Full { amount: damage }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn armor_formula_matches_known_values() {
        // 20 armour, 0 toughness, unpierced, 10 damage:
        // toughness=2, realArmor=clamp(20-10/2, 4, 20)=15, frac=15/25=0.6,
        // result = 10 * (1-0.6) = 4.0.
        let out = damage_after_armor(10.0, 20.0, 0.0, 1.0);
        assert!((out - 4.0).abs() < 1e-4, "got {out}");
    }

    #[test]
    fn armor_has_a_minimum_effectiveness_floor() {
        // Huge single hit vs modest armour: realArmor floored at armor*0.2.
        // 8 armour, 0 toughness, 100 dmg: realArmor=clamp(8-50,1.6,20)=1.6,
        // frac=0.064, result=100*(1-0.064)=93.6.
        let out = damage_after_armor(100.0, 8.0, 0.0, 1.0);
        assert!((out - 93.6).abs() < 1e-3, "got {out}");
    }

    #[test]
    fn resistance_scales_per_level() {
        // amp 0 (Resistance I): 20% off.
        assert!((damage_after_resistance(10.0, 0) - 8.0).abs() < 1e-4);
        // amp 3 (Resistance IV): 80% off.
        assert!((damage_after_resistance(10.0, 3) - 2.0).abs() < 1e-4);
        // amp 4 (Resistance V): immune (clamped to 0).
        assert!((damage_after_resistance(10.0, 4) - 0.0).abs() < 1e-4);
    }

    #[test]
    fn protection_caps_at_twenty() {
        // Protection value 25 clamps to 20: 10*(1-20/25)=2.0.
        assert!((damage_after_protection(10.0, 25.0) - 2.0).abs() < 1e-4);
    }

    #[test]
    fn pipeline_runs_stages_in_order() {
        // armour first (4.0), then Resistance I (×0.8 → 3.2), then protection
        // value 5 (×(1-5/25)=0.8 → 2.56), then 2.0 absorption soaks first.
        let d = Defenses {
            armor: 20.0,
            armor_toughness: 0.0,
            resistance_amplifier: Some(0),
            enchant_protection: 5.0,
            enchant_effectiveness: 1.0,
            absorption: 2.0,
        };
        let out = apply_reductions(10.0, &d, DamageFlags::default());
        assert!((out.absorbed - 2.0).abs() < 1e-4);
        assert!(
            (out.to_health - 0.56).abs() < 1e-3,
            "to_health {}",
            out.to_health
        );
        assert!((out.remaining_absorption - 0.0).abs() < 1e-4);
    }

    #[test]
    fn bypass_armor_skips_only_armor_stage() {
        let d = Defenses {
            armor: 20.0,
            ..Default::default()
        };
        let flags = DamageFlags {
            bypasses_armor: true,
            ..Default::default()
        };
        let out = apply_reductions(10.0, &d, flags);
        assert!(
            (out.to_health - 10.0).abs() < 1e-4,
            "armour should be skipped"
        );
    }

    #[test]
    fn iframe_ignores_weaker_followup_within_window() {
        let mut c = HurtCooldown::default();
        // First hit: full 8, opens 20-tick window.
        assert_eq!(
            c.on_hurt(8.0, DamageFlags::default()),
            HurtDecision::Full { amount: 8.0 }
        );
        assert_eq!(c.invulnerable_time, 20);
        // Same tick-ish, weaker hit (5 <= 8) is ignored.
        assert_eq!(
            c.on_hurt(5.0, DamageFlags::default()),
            HurtDecision::Ignored
        );
    }

    #[test]
    fn iframe_tops_up_for_a_stronger_followup() {
        let mut c = HurtCooldown {
            invulnerable_time: 20,
            last_hurt: 6.0,
        };
        // Stronger hit (10 > 6) applies only the 4.0 difference, no new window.
        assert_eq!(
            c.on_hurt(10.0, DamageFlags::default()),
            HurtDecision::Topup { amount: 4.0 }
        );
        assert_eq!(c.last_hurt, 10.0);
        assert_eq!(c.invulnerable_time, 20, "top-up must not re-arm the window");
    }

    #[test]
    fn iframe_window_expires_below_ten() {
        let mut c = HurtCooldown {
            invulnerable_time: 11,
            last_hurt: 8.0,
        };
        c.tick(); // 10 -> not > 10 anymore
        // Now a fresh full hit lands even though it is weaker.
        assert_eq!(
            c.on_hurt(3.0, DamageFlags::default()),
            HurtDecision::Full { amount: 3.0 }
        );
        assert_eq!(c.invulnerable_time, 20);
    }

    #[test]
    fn bypasses_cooldown_always_lands_full() {
        let mut c = HurtCooldown {
            invulnerable_time: 20,
            last_hurt: 100.0,
        };
        let flags = DamageFlags {
            bypasses_cooldown: true,
            ..Default::default()
        };
        assert_eq!(c.on_hurt(1.0, flags), HurtDecision::Full { amount: 1.0 });
    }
}
