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
//! One thing is deliberately **not** here:
//!   * **Knockback impulse.** `impl-physics` builds the knockback velocity from
//!     the other side; this crate only decides *whether* a hit lands and *how
//!     much* it hurts. Coordinate the impulse through the project owner rather
//!     than growing a second model.
//!
//! # The damage-type table exists now (issue #263)
//!
//! This module's docs used to say that "which hits bypass armour, cooldown or
//! resistance is registry data; a caller passes the relevant [`DamageFlags`]
//! rather than this crate hardcoding a version's damage-type table" — and that
//! the table existed nowhere. **It does now**, in
//! [`lodestone_data::damage_types`], generated from vanilla 26.2's own datapack
//! JSON out of the server jar.
//!
//! The seam is unchanged in shape and the crate still hardcodes nothing: it
//! *reads* the table rather than embedding one. What changed is that callers no
//! longer hand-derive flags. Use [`DamageFlags::for_damage_type`] instead of
//! writing `bypasses_armor: true` next to a prose citation of
//! `bypasses_armor.json` — that hand-derivation was the exact pattern #263
//! existed to remove, and it had already appeared at four call sites.
//!
//! # Issue #261 status: the formula is live-verified, the *feed* is not
//!
//! `apply_reductions`/`damage_after_armor`/`damage_after_protection` are
//! cross-checked term-for-term against `CombatRules.getDamageAfterAbsorb`/
//! `getDamageAfterMagicAbsorb` (`.cache/mc/26.2/src/net/minecraft/world/
//! damagesource/CombatRules.java`, the whole file is 39 lines) and, beyond
//! that, **live-verified against a real running vanilla 26.2 server**: a pig
//! force-equipped with a full diamond armour set (`armor_formula_lands_on_
//! the_toughness_hypothesis_not_the_flat_one`'s doc comment has the exact
//! RCON transcript) took **3.0** damage from a raw 10.0 hit, matching this
//! module's formula and *not* a flat-percentage alternative. The pipeline is
//! also a real, non-island consumer: `lodestone-server`'s
//! `SimMob::apply_damage` (`crates/lodestone-server/src/mobs.rs:584`) calls
//! it for every landed melee hit and explosion.
//!
//! What #261 actually asked for beyond that, and does **not** exist anywhere
//! in this workspace yet (verified by a full-repo grep, not assumed):
//!   * **Feeding `Defenses` from an entity's real equipped items.**
//!     `crate::mobs::combat_defaults`-equivalent code only ever reads
//!     generic per-species base attributes (`default_attributes`), never an
//!     equipped helmet/chestplate/leggings/boots. There is no equipment/
//!     inventory model anywhere in `lodestone-server`, `lodestone-ecs`, or
//!     this crate that carries per-item armour/toughness/enchantment-level
//!     stats for combat purposes (the ECS `EntityEquipment` component that
//!     does exist is cosmetic-rendering-only). Building this needs, at
//!     minimum: per-material armour/toughness constants (`.cache/mc/26.2/
//!     src/net/minecraft/world/item/equipment/ArmorMaterials.java` has the
//!     real vanilla table) and a per-entity equipped-item slot model — a
//!     prerequisite feature, not a `damage.rs` change.
//!   * **`knockback_resistance` reducing an incoming melee push.** No melee
//!     knockback impulse is computed anywhere in this workspace at all (only
//!     `explosion::knockback_power` exists, for blasts) — `knockback_
//!     resistance`/`ARMOR`/`ARMOR_TOUGHNESS` from equipped items would have
//!     nothing to plug into yet.
//!   * **Attack-cooldown-scaled damage and critical-hit/sweep bonus damage**
//!     (also explicitly in #261's scope) — no attack-cooldown timer or hit
//!     classification exists server-side.
//!
//! None of this is started here — it is a materially larger prerequisite
//! (an equipment/inventory model, which several other in-flight issues also
//! depend on) than "wire an existing pipeline up," and inventing an
//! unconsumed per-material armour table now would itself be the kind of
//! island CLAUDE.md warns about. See issue #261 for the up-to-date status.

use lodestone_data::damage_types::{DamageType, DamageTypeTag};

/// Per-hit flags derived from the damage source's type tags. Each one switches
/// off a stage of the pipeline, matching the `DamageTypeTags` checks in
/// `LivingEntity`.
///
/// Build these with [`DamageFlags::for_damage_type`] rather than by hand.
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

impl DamageFlags {
    /// Derives the per-hit flags from a real `minecraft:damage_type` and its
    /// resolved tag memberships — the seam this struct was shaped for
    /// (issue #263).
    ///
    /// Each field is one `DamageTypeTags` query, matching the vanilla checks
    /// one-for-one:
    ///
    /// | field | vanilla check |
    /// |---|---|
    /// | `bypasses_armor` | `LivingEntity.java:1903` |
    /// | `bypasses_effects` | `LivingEntity.java:1912` |
    /// | `bypasses_resistance` | `LivingEntity.java:1916` |
    /// | `bypasses_enchantments` | `LivingEntity.java:1936` |
    /// | `bypasses_cooldown` | `LivingEntity.java:1217` |
    ///
    /// Note `bypasses_cooldown` is **empty** in vanilla 26.2 — the tag is
    /// declared at `DamageTypeTags.java:12` and gates the i-frame window, but no
    /// damage type opts into it. So this always yields `bypasses_cooldown: false`
    /// for a vanilla type, which is correct rather than unimplemented. A caller
    /// that needs to force a hit past the i-frame gate (fall damage in
    /// `lodestone-server` deliberately does) must set it explicitly and say why.
    #[must_use]
    pub fn for_damage_type(ty: DamageType) -> Self {
        Self {
            bypasses_armor: ty.is_in(DamageTypeTag::BypassesArmor),
            bypasses_effects: ty.is_in(DamageTypeTag::BypassesEffects),
            bypasses_resistance: ty.is_in(DamageTypeTag::BypassesResistance),
            bypasses_enchantments: ty.is_in(DamageTypeTag::BypassesEnchantments),
            bypasses_cooldown: ty.is_in(DamageTypeTag::BypassesCooldown),
        }
    }

    /// [`for_damage_type`](Self::for_damage_type) by registry name, with or
    /// without the `minecraft:` namespace.
    ///
    /// `None` for an unknown name, so a datapack-added or future-version type
    /// surfaces as an explicit miss rather than silently reducing like a
    /// default-flagged hit.
    #[must_use]
    pub fn for_damage_type_name(name: &str) -> Option<Self> {
        DamageType::from_name(name).map(Self::for_damage_type)
    }
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
    use lodestone_data::damage_types::DamageScaling;

    #[test]
    fn armor_formula_matches_known_values() {
        // 20 armour, 0 toughness, unpierced, 10 damage:
        // toughness=2, realArmor=clamp(20-10/2, 4, 20)=15, frac=15/25=0.6,
        // result = 10 * (1-0.6) = 4.0.
        let out = damage_after_armor(10.0, 20.0, 0.0, 1.0);
        assert!((out - 4.0).abs() < 1e-4, "got {out}");
    }

    /// **Magnitude check** (CLAUDE.md's vacuous-test species): a flat
    /// `armor / ARMOR_PROTECTION_DIVIDER` reduction with no toughness term is
    /// a plausible-looking wrong formula that still shows "armour reduces
    /// damage" — it would predict `10 * (1 - 20/25) = 2.0` for full diamond
    /// armour (20 armour, 8 toughness) against a 10.0 hit. The real
    /// toughness-aware formula predicts `3.0` (`toughness=2+8/4=4,
    /// realArmor=clamp(20-10/4,4,20)=17.5, frac=17.5/25=0.7, 10*0.3=3.0`).
    /// Live-verified against a real vanilla 26.2 server (not just this
    /// hermetic assertion): a pig force-equipped with a full diamond armour
    /// set (`equipment:{head:diamond_helmet,...}`, confirmed via
    /// `/attribute get` to resolve to armor=20.0/toughness=8.0) took exactly
    /// **3.0** damage from a raw 10.0 `minecraft:mob_attack` hit (`/damage
    /// <pig> 10 minecraft:mob_attack`, health 20.0 -> 17.0) — landing on the
    /// correct hypothesis, not the flat-percentage one.
    #[test]
    fn armor_formula_lands_on_the_toughness_hypothesis_not_the_flat_one() {
        let correct = damage_after_armor(10.0, 20.0, 8.0, 1.0);
        let flat_wrong = 10.0 * (1.0 - 20.0 / ARMOR_PROTECTION_DIVIDER);
        assert!((correct - 3.0).abs() < 1e-4, "got {correct}");
        assert!((flat_wrong - 2.0).abs() < 1e-4, "sanity-check on the wrong hypothesis itself");
        assert!(
            (correct - flat_wrong).abs() > 0.5,
            "the two hypotheses must actually differ for this to be a real check"
        );
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

    // -----------------------------------------------------------------------
    // Issue #263: DamageFlags derived from the real damage-type table
    // -----------------------------------------------------------------------

    fn ty(name: &str) -> DamageType {
        DamageType::from_name(name).unwrap_or_else(|| panic!("{name} is a real damage type"))
    }

    /// Full diamond armour, the same set the live-verified armour test uses.
    fn diamond() -> Defenses {
        Defenses {
            armor: 20.0,
            armor_toughness: 8.0,
            ..Default::default()
        }
    }

    /// The **prediction** gate, and the one that would catch a broken tag
    /// lookup. Two real damage types, one `Defenses`, one raw amount — the only
    /// difference is the tag data.
    ///
    /// `minecraft:generic` **is** `bypasses_armor`-tagged (the trap that cost a
    /// mid-oracle debugging session: a fully-armoured subject takes full damage
    /// and the armour maths looks broken). `minecraft:mob_attack` is not.
    ///
    /// So against 20 armour / 8 toughness and a raw 10.0:
    ///   * `mob_attack` → **3.0** (`toughness=2+8/4=4`,
    ///     `realArmor=clamp(20-10/4,4,20)=17.5`, `frac=0.7`, `10*0.3`)
    ///   * `generic` → **10.0**, untouched
    ///
    /// Those differ by 7.0 of 10.0, so this is a magnitude check, not a
    /// direction check. If the tag lookup broke such that `bypasses_armor` were
    /// always `false`, `generic` would measure 3.0 — and the assertion below
    /// fails by 7.0 rather than passing on a technicality. That is exactly the
    /// mutation `a_broken_bypasses_armor_lookup_would_be_caught` performs.
    #[test]
    fn armour_reduction_lands_on_the_real_tag_data_for_both_types() {
        let d = diamond();

        let reducible = apply_reductions(10.0, &d, DamageFlags::for_damage_type(ty("mob_attack")));
        let bypassing = apply_reductions(10.0, &d, DamageFlags::for_damage_type(ty("generic")));

        assert!(
            (reducible.to_health - 3.0).abs() < 1e-4,
            "mob_attack is reducible: expected 3.0, got {}",
            reducible.to_health
        );
        assert!(
            (bypassing.to_health - 10.0).abs() < 1e-4,
            "generic is bypasses_armor-tagged: expected the full 10.0, got {}",
            bypassing.to_health
        );

        // The absence claim ("armour does not reduce generic") is only as good
        // as evidence that armour *would* have reduced it. Same Defenses, same
        // amount, 7.0 points of difference — the detector demonstrably fires.
        assert!(
            bypassing.to_health - reducible.to_health > 6.9,
            "the two types must differ by the full armour reduction, or this proves nothing \
             (bypassing {}, reducible {})",
            bypassing.to_health,
            reducible.to_health
        );
    }

    /// The **negative control**, run rather than described: mutate the tag
    /// lookup so `bypasses_armor` is always `false`, and the assertion above
    /// must fail.
    #[test]
    fn a_broken_bypasses_armor_lookup_would_be_caught() {
        let d = diamond();

        // The mutation: what `for_damage_type` would produce if the
        // `BypassesArmor` tag query always returned false.
        let broken = DamageFlags {
            bypasses_armor: false,
            ..DamageFlags::for_damage_type(ty("generic"))
        };
        let real = DamageFlags::for_damage_type(ty("generic"));
        assert_ne!(
            broken, real,
            "the control is vacuous: the mutation changed nothing, so generic is not actually \
             carrying bypasses_armor from the table"
        );

        let mutated = apply_reductions(10.0, &d, broken).to_health;
        assert!(
            (mutated - 3.0).abs() < 1e-4,
            "a broken lookup should reduce generic like an ordinary hit, got {mutated}"
        );
        // ...and that is 7.0 away from the value the real gate asserts, so the
        // real gate fails under this mutation instead of tolerating it.
        assert!(
            (mutated - 10.0).abs() > 6.9,
            "the mutation must move the measurement far outside the real gate's tolerance"
        );
    }

    /// Every flag comes from a tag query, checked against memberships read out of
    /// the datapack by hand (`bypasses_armor.json`, `bypasses_effects.json`,
    /// `bypasses_resistance.json`, `bypasses_enchantments.json`).
    #[test]
    fn each_flag_tracks_its_own_tag() {
        // fall: bypasses_armor only.
        let fall = DamageFlags::for_damage_type(ty("fall"));
        assert!(fall.bypasses_armor);
        assert!(!fall.bypasses_effects);
        assert!(!fall.bypasses_resistance);
        assert!(!fall.bypasses_enchantments);

        // starve is the sole bypasses_effects member, and is also bypasses_armor.
        let starve = DamageFlags::for_damage_type(ty("starve"));
        assert!(starve.bypasses_effects);
        assert!(starve.bypasses_armor);

        // sonic_boom is the sole bypasses_enchantments member.
        let sonic = DamageFlags::for_damage_type(ty("sonic_boom"));
        assert!(sonic.bypasses_enchantments);
        assert!(!sonic.bypasses_resistance);

        // out_of_world / generic_kill are the two bypasses_resistance members.
        for name in ["out_of_world", "generic_kill"] {
            let f = DamageFlags::for_damage_type(ty(name));
            assert!(f.bypasses_resistance, "{name} is bypasses_resistance");
            assert!(f.bypasses_armor, "{name} is bypasses_armor");
        }

        // A plain melee hit switches nothing off.
        assert_eq!(
            DamageFlags::for_damage_type(ty("player_attack")),
            DamageFlags::default(),
            "player_attack runs every reduction stage"
        );
    }

    /// `bypasses_cooldown` is empty in vanilla 26.2, so the derived flag is
    /// always false — asserted, with a control showing the derivation *can*
    /// produce a true flag, so this is not measuring a dead code path.
    #[test]
    fn no_vanilla_type_bypasses_the_iframe_cooldown() {
        let mut any_cooldown = false;
        let mut any_flag_at_all = false;
        for t in DamageType::ALL {
            let f = DamageFlags::for_damage_type(t);
            any_cooldown |= f.bypasses_cooldown;
            any_flag_at_all |= f.bypasses_armor;
        }
        assert!(
            !any_cooldown,
            "bypasses_cooldown has no data file in 26.2, so no type can set this flag"
        );
        assert!(
            any_flag_at_all,
            "control: the derivation must be capable of setting a flag, or the assertion \
             above measures nothing"
        );
    }

    /// The by-name entry point, and that an unknown name is a miss rather than a
    /// silently default-flagged hit.
    #[test]
    fn lookup_by_name_accepts_both_forms_and_rejects_unknowns() {
        assert_eq!(
            DamageFlags::for_damage_type_name("minecraft:fall"),
            DamageFlags::for_damage_type_name("fall")
        );
        assert!(
            DamageFlags::for_damage_type_name("fall")
                .expect("fall resolves")
                .bypasses_armor
        );
        assert!(DamageFlags::for_damage_type_name("minecraft:nonsense").is_none());
    }

    /// Guards the citation in this module's docs: the table is reachable from
    /// here and carries the non-flag fields the loot/death-message consumers
    /// (#272) will read, so those do not need a second table.
    #[test]
    fn the_table_carries_more_than_flags() {
        assert_eq!(ty("mob_attack").message_id(), "mob");
        assert_eq!(ty("mob_attack").exhaustion(), 0.1);
        assert_eq!(ty("fall").exhaustion(), 0.0);
        assert_eq!(ty("explosion").scaling(), DamageScaling::Always);
        assert!(ty("lava").is_in(DamageTypeTag::IsFire));
        assert!(ty("fall").is_in(DamageTypeTag::IsFall));
        assert!(ty("arrow").is_in(DamageTypeTag::IsProjectile));
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
